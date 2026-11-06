//! Runs the coder's shell commands: zsh, PowerShell or bash.
//!
//! The port of mac/sandbox/service.py, the Sandbox class: one folder on the
//! Desktop (~/Desktop/sandbox_workspace/<session id>) and a fresh shell for
//! every command, started in that folder and watched until it ends, goes
//! silent or runs out of time, with the mac result dicts (same keys, order
//! and texts). The shell, the folder blocklist, the prompt patterns and the
//! timeout text follow the OS, as the mac, linux and windows Python copies
//! do. Left out: the macOS permission-popup watcher, so there is no
//! "permission_dialog" result (the linux and windows copies have none).
//!
//! Changes from Python, on purpose: a command runs in its own process group
//! and every kill (timeout, idle, Ctrl+C) takes the whole group, where Python
//! killed the shell only; output is read as lossy UTF-8, where Python's
//! reader thread died on a bad byte; the input is written on its own thread,
//! where Python could block on a full pipe.
//!
//! The Python path and error-text rules the file tools share live here too:
//! Path.home(), Path.resolve(), str(OSError) and repr(str).

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};

// Timeouts, as in Python.
/// Silence before the prompt-pattern check.
const IDLE_TIMEOUT_PROMPT: Duration = Duration::from_secs(15);
/// Silence before "it must be waiting for input".
const IDLE_TIMEOUT_FALLBACK: Duration = Duration::from_secs(60);
/// The hard cap per command (the message says "10 minutes" as plain text).
const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the command is checked.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Python's thread.join(timeout=0.5) per stream once the shell has exited.
const JOIN_TIMEOUT: Duration = Duration::from_millis(500);
/// How much of the output end the prompt check looks at (characters).
const CHUNK_SIZE: usize = 200;
/// One read from a pipe.
const READ_SIZE: usize = 8192;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "macos")]
const SHELL: &str = "/bin/zsh";
#[cfg(all(unix, not(target_os = "macos")))]
const SHELL: &str = "/bin/bash";

// Folders a command may not name (a lowercase substring test on the command).
#[cfg(target_os = "macos")]
const BLOCKED_PATHS: &[&str] = &["/system", "/usr/sbin", "/private/var"];
#[cfg(all(unix, not(target_os = "macos")))]
const BLOCKED_PATHS: &[&str] = &["/boot", "/usr/sbin", "/sbin", "/var/lib"];
#[cfg(windows)]
const BLOCKED_PATHS: &[&str] = &["c:\\windows", "c:/windows"];

#[cfg(not(windows))]
const TIMEOUT_MESSAGE: &str =
    "Command timed out after 10 minutes (may need input - use input parameter)";
#[cfg(windows)]
const TIMEOUT_MESSAGE: &str = "Command timed out after 10 minutes (may need input - use input parameter if program requires user input)";

/// Output ends that look like a program waiting for input. The text is
/// stripped first, so the five patterns that need a trailing space never
/// match (as in Python).
#[cfg(not(windows))]
const PROMPT_PATTERNS: &[&str] = &[
    r": $",
    r"> $",
    r"\? $",
    r":: $",
    r"\] $",
    r"\(y/n\)\s*$",
    r"\(yes/no\)\s*$",
    r"press enter",
    r"waiting for",
    r"enter .*:",
    r"input\s*:",
    r"password\s*:",
    r">>>\s*$",
    r"\.\.\.\s*$",
    r"\$\s*$",
    r"#\s*$",
    r"interactive",
    r"commands?\s*:",
    r"options?\s*:",
    r"menu",
    r"choice",
    r"select",
    r"exit.*quit",
    r"type .* to",
];
/// The Windows list: the same plus two menu rules after the root prompt.
#[cfg(windows)]
const PROMPT_PATTERNS: &[&str] = &[
    r": $",
    r"> $",
    r"\? $",
    r":: $",
    r"\] $",
    r"\(y/n\)\s*$",
    r"\(yes/no\)\s*$",
    r"press enter",
    r"waiting for",
    r"enter .*:",
    r"input\s*:",
    r"password\s*:",
    r">>>\s*$",
    r"\.\.\.\s*$",
    r"\$\s*$",
    r"#\s*$",
    r"={3,}\s*$",
    r"-{3,}\s*$",
    r"interactive",
    r"commands?\s*:",
    r"options?\s*:",
    r"menu",
    r"choice",
    r"select",
    r"exit.*quit",
    r"type .* to",
];

fn prompt_regexes() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        PROMPT_PATTERNS
            .iter()
            .filter_map(|p| RegexBuilder::new(p).case_insensitive(true).build().ok())
            .collect()
    })
}

/// The coder's shell runner on its own Desktop folder.
pub struct Sandbox {
    pub desktop_path: PathBuf,
    pub sandbox_root: PathBuf,
    /// Where every command starts; only cd() changes it.
    pub working_dir: String,
}

impl Sandbox {
    /// `~/Desktop/<workspace_name>[/<session_id>]`, created with its parents
    /// (mac default name: "sandbox_workspace"). An empty session id means
    /// none, as Python's `if session_id:`.
    pub fn new(workspace_name: &str, session_id: Option<&str>) -> io::Result<Sandbox> {
        let home = py_home_dir()
            .ok_or_else(|| io::Error::other("Could not determine home directory."))?;
        let desktop_path = pathlib_normalize(&home.join("Desktop"));
        let sandbox_root = match session_id {
            Some(sid) if !sid.is_empty() => desktop_path.join(workspace_name).join(sid),
            _ => desktop_path.join(workspace_name),
        };
        let sandbox_root = pathlib_normalize(&sandbox_root);
        fs::create_dir_all(&sandbox_root)?;
        let working_dir = sandbox_root.to_string_lossy().into_owned();
        Ok(Sandbox { desktop_path, sandbox_root, working_dir })
    }

    /// Python's _validate_path: (is_valid, resolved path or the BLOCKED text).
    fn validate_path(&self, path: &str) -> (bool, String) {
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            Path::new(&self.working_dir).join(path)
        };
        match py_resolve(&full) {
            Ok(resolved) => {
                let text = resolved.to_string_lossy().into_owned();
                let lower = text.to_lowercase();
                if BLOCKED_PATHS.iter().any(|b| lower.starts_with(b)) {
                    return (false, "BLOCKED: Access to system folder not allowed".to_string());
                }
                (true, text)
            }
            Err(_) => (false, "BLOCKED: Invalid path".to_string()),
        }
    }

    /// Python's _is_command_safe: (is_safe, the BLOCKED text).
    fn is_command_safe(&self, command: &str) -> (bool, String) {
        let cmd_lower = command.to_lowercase();
        for blocked in BLOCKED_PATHS {
            if cmd_lower.contains(blocked) {
                return (false, format!("BLOCKED: Access to system folder not allowed - {blocked}"));
            }
        }
        (true, String::new())
    }

    /// Run one command in the working folder and return the mac result dict:
    /// finished `{success, stdout, stderr, returncode}`, blocked or failed to
    /// start `{success: false, error}`, timed out `{success: false, timeout,
    /// message, last_output, stdout, stderr}`, or waiting for input
    /// `{success: false, error: "input_required", message, last_output,
    /// stdout, stderr}`.
    ///
    /// `input_text`: non-empty text is written to a new stdin pipe, which is
    /// then closed; None and "" both leave the child this process's stdin,
    /// and only None turns on the 15 s / 60 s silence checks (Python's
    /// `if input_text` and `is None`).
    ///
    /// Err only for KeyboardInterrupt (or another exception from a signal
    /// handler), after the command's whole process tree is killed.
    pub fn run(
        &mut self,
        py: Python<'_>,
        command: &str,
        input_text: Option<&str>,
        trusted: bool,
    ) -> PyResult<Value> {
        if !trusted {
            let (is_safe, error_msg) = self.is_command_safe(command);
            if !is_safe {
                return Ok(json!({"success": false, "error": error_msg}));
            }
        }

        // Popen refuses a NUL in its arguments or its cwd.
        if command.contains('\0') || self.working_dir.contains('\0') {
            return Ok(json!({"success": false, "error": "embedded null byte"}));
        }
        let feed = input_text.filter(|t| !t.is_empty());
        let mut cmd = shell_command(command);
        cmd.current_dir(&self.working_dir)
            .stdin(if feed.is_some() { Stdio::piped() } else { Stdio::inherit() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Ok(json!({"success": false, "error": spawn_error_text(&e, &self.working_dir)}))
            }
        };

        if let (Some(text), Some(stdin)) = (feed, child.stdin.take()) {
            write_input(stdin, text);
        }

        let readers = match (child.stdout.take(), child.stderr.take()) {
            (Some(out), Some(err)) => match (start_reader(out), start_reader(err)) {
                (Ok(out_rx), Ok(err_rx)) => Some((out_rx, err_rx)),
                _ => None,
            },
            _ => None,
        };
        let Some((out_rx, err_rx)) = readers else {
            py.detach(move || kill_tree(child));
            return Ok(json!({"success": false, "error": "can't start new thread"}));
        };

        let mut out_buf: Vec<u8> = Vec::new();
        let mut err_buf: Vec<u8> = Vec::new();
        // The last 200 characters, kept until new output comes.
        let mut chunk_cache: Option<String> = None;
        let start = Instant::now();
        let mut last_output = Instant::now();

        loop {
            if start.elapsed() > TOTAL_TIMEOUT {
                py.detach(move || kill_tree(child));
                return Ok(json!({
                    "success": false,
                    "timeout": true,
                    "message": TIMEOUT_MESSAGE,
                    "last_output": last_chunk_of(&out_buf, &err_buf),
                    "stdout": decode(&out_buf),
                    "stderr": decode(&err_buf),
                }));
            }

            let poll = match child.try_wait() {
                Ok(poll) => poll,
                Err(e) => return Ok(json!({"success": false, "error": os_error_text(&e)})),
            };

            let got_out = drain(&out_rx, &mut out_buf);
            let got_err = drain(&err_rx, &mut err_buf);
            if got_out || got_err {
                last_output = Instant::now();
                chunk_cache = None;
            }

            if let Some(status) = poll {
                let returncode = exit_code(status);
                // A background job can keep the pipes open: wait at most
                // 0.5 s per stream, then return what has come so far.
                let (out_rx, got) = py.detach(move || join_reader(out_rx, JOIN_TIMEOUT));
                out_buf.extend_from_slice(&got);
                let (err_rx, got) = py.detach(move || join_reader(err_rx, JOIN_TIMEOUT));
                err_buf.extend_from_slice(&got);
                drain(&out_rx, &mut out_buf);
                drain(&err_rx, &mut err_buf);
                // The shell has exited and been reaped: nothing is killed here.
                py.check_signals()?;
                return Ok(json!({
                    "success": returncode == 0,
                    "stdout": decode(&out_buf),
                    "stderr": decode(&err_buf),
                    "returncode": returncode,
                }));
            }

            if input_text.is_none() {
                let idle = last_output.elapsed();
                if idle > IDLE_TIMEOUT_PROMPT || idle > IDLE_TIMEOUT_FALLBACK {
                    let last_chunk = chunk_cache
                        .get_or_insert_with(|| last_chunk_of(&out_buf, &err_buf))
                        .clone();
                    if idle > IDLE_TIMEOUT_PROMPT && is_input_prompt(&last_chunk) {
                        py.detach(move || kill_tree(child));
                        return Ok(json!({
                            "success": false,
                            "error": "input_required",
                            "message": "Process waiting for input",
                            "last_output": last_chunk,
                            "stdout": decode(&out_buf),
                            "stderr": decode(&err_buf),
                        }));
                    }
                    if idle > IDLE_TIMEOUT_FALLBACK {
                        py.detach(move || kill_tree(child));
                        return Ok(json!({
                            "success": false,
                            "error": "input_required",
                            "message": "Process idle for 60s, likely waiting for input",
                            "last_output": last_chunk,
                            "stdout": decode(&out_buf),
                            "stderr": decode(&err_buf),
                        }));
                    }
                }
            }

            py.detach(|| thread::sleep(POLL_INTERVAL));
            if let Err(e) = py.check_signals() {
                py.detach(move || kill_tree(child));
                return Err(e);
            }
        }
    }

    /// Change the working folder (the app's hand-typed terminal used it; the
    /// coder never does): `{success: true, cwd}` or `{success: false, error}`.
    pub fn cd(&mut self, path: &str) -> Value {
        let (is_valid, result) = self.validate_path(path);
        if !is_valid {
            return json!({"success": false, "error": result});
        }
        if Path::new(&result).is_dir() {
            self.working_dir = result;
            return json!({"success": true, "cwd": self.working_dir});
        }
        json!({"success": false, "error": "Directory not found"})
    }

    pub fn get_cwd(&self) -> String {
        self.working_dir.clone()
    }
}

/// Python's _is_input_prompt.
fn is_input_prompt(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let text_lower = crate::py_strip(&lower);
    prompt_regexes().iter().any(|re| re.is_match(text_lower))
}

/// Python's _get_last_output_chunk: the last `chunk_size` characters, stripped.
fn last_output_chunk(buffer: &str, chunk_size: usize) -> String {
    if buffer.is_empty() {
        return String::new();
    }
    // buffer[-chunk_size:], in characters (Python's [-0:] is the whole text).
    let tail = match chunk_size.checked_sub(1) {
        Some(back) => match buffer.char_indices().rev().nth(back) {
            Some((i, _)) => buffer.get(i..).unwrap_or(buffer),
            None => buffer,
        },
        None => buffer,
    };
    crate::py_strip(tail).to_string()
}

/// The end of stdout + stderr, as Python's last_chunk.
fn last_chunk_of(out: &[u8], err: &[u8]) -> String {
    let mut text = decode(out);
    text.push_str(&decode(err));
    last_output_chunk(&text, CHUNK_SIZE)
}

/// Bytes from a pipe as Python's text-mode reader gives them: UTF-8 (bad
/// bytes become U+FFFD) with universal newlines ("\r\n" and "\r" read "\n").
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n").replace('\r', "\n")
}

/// The shell for this OS around one command.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", command]);
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(SHELL);
        cmd.arg("-c").arg(command);
        // Its own process group, so a kill reaches everything it started.
        cmd.process_group(0);
        cmd
    }
}

/// Write the input and close stdin, on a thread of its own. Errors (the
/// child already gone) are ignored, as in Python.
fn write_input(mut stdin: ChildStdin, text: &str) {
    // Python's text mode writes "\n" as os.linesep.
    #[cfg(windows)]
    let data = text.replace('\n', "\r\n");
    #[cfg(not(windows))]
    let data = text.to_string();
    let _ = thread::Builder::new().name("sandbox-input".into()).spawn(move || {
        let _ = stdin.write_all(data.as_bytes());
        let _ = stdin.flush();
    });
}

/// Read a pipe in chunks on a thread. Once nobody listens any more (run()
/// returned), it keeps draining until EOF, so a background job that still
/// writes never blocks on a full pipe (Python's reader thread did the same).
fn start_reader<R: Read + Send + 'static>(mut pipe: R) -> io::Result<Receiver<Vec<u8>>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    thread::Builder::new().name("sandbox-reader".into()).spawn(move || {
        let mut buf = vec![0u8; READ_SIZE];
        let mut listening = true;
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if listening {
                        if let Some(chunk) = buf.get(..n) {
                            if tx.send(chunk.to_vec()).is_err() {
                                listening = false;
                            }
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })?;
    Ok(rx)
}

/// Take what a reader has sent so far. True if anything came.
fn drain(rx: &Receiver<Vec<u8>>, buf: &mut Vec<u8>) -> bool {
    let mut got = false;
    while let Ok(chunk) = rx.try_recv() {
        if !chunk.is_empty() {
            got = true;
        }
        buf.extend_from_slice(&chunk);
    }
    got
}

/// Python's thread.join(timeout): collect until the reader ends (EOF) or the
/// time is up. Runs without the GIL; hands the receiver back.
fn join_reader(rx: Receiver<Vec<u8>>, timeout: Duration) -> (Receiver<Vec<u8>>, Vec<u8>) {
    let deadline = Instant::now() + timeout;
    let mut got = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(chunk) => got.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    (rx, got)
}

/// Kill the command with everything it started, then reap it in the
/// background so no zombie is left.
fn kill_tree(mut child: Child) {
    #[cfg(unix)]
    {
        if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
            // SAFETY: killpg only sends a signal. The child was started with
            // process_group(0) and is not reaped yet, so its pid is still its
            // own group's id.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", pid.as_str()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = thread::Builder::new().name("sandbox-reaper".into()).spawn(move || {
        let _ = child.wait();
    });
}

/// Popen's returncode: the exit status, or -signal when a signal ended it.
#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    status.signal().map(|s| -i64::from(s)).unwrap_or(-1)
}

/// Popen's returncode on Windows: the exit code as an unsigned number.
#[cfg(windows)]
fn exit_code(status: ExitStatus) -> i64 {
    status.code().map(|c| i64::from(c as u32)).unwrap_or(-1)
}

/// str(e) of a failed Popen, as Python words it: the cwd is named when it is
/// the missing part, else the shell (Windows names neither).
fn spawn_error_text(e: &io::Error, cwd: &str) -> String {
    #[cfg(windows)]
    {
        let _ = cwd;
        match e.raw_os_error() {
            Some(code) => format!("[WinError {code}] {}", os_error_message(e)),
            None => e.to_string(),
        }
    }
    #[cfg(not(windows))]
    {
        let name = if Path::new(cwd).is_dir() { SHELL } else { cwd };
        py_os_error(e, Path::new(name))
    }
}

// ---------------------------------------------------------------------------
// Python's path and error-text rules, shared with the file tools
// ---------------------------------------------------------------------------

/// True if the path holds a NUL (Python raises ValueError for it).
pub(crate) fn has_nul(p: &Path) -> bool {
    p.as_os_str().as_encoded_bytes().contains(&0)
}

/// The OS message of an error, without Rust's " (os error N)" ending.
fn os_error_message(e: &io::Error) -> String {
    let full = e.to_string();
    match e.raw_os_error() {
        Some(code) => {
            let tail = format!(" (os error {code})");
            full.strip_suffix(&tail).unwrap_or(&full).to_string()
        }
        None => full,
    }
}

/// str(e) of an OSError without a file name: "[Errno 2] No such file or directory".
fn os_error_text(e: &io::Error) -> String {
    match e.raw_os_error() {
        #[cfg(windows)]
        Some(code) => format!("[WinError {code}] {}", os_error_message(e)),
        #[cfg(not(windows))]
        Some(code) => format!("[Errno {code}] {}", os_error_message(e)),
        None => e.to_string(),
    }
}

/// str(e) of an OSError about `filename`, as Python prints it:
/// "[Errno 21] Is a directory: '/x'". A NUL in the name is Python's
/// ValueError text "embedded null byte".
pub(crate) fn py_os_error(e: &io::Error, filename: &Path) -> String {
    let name = py_repr_str(&filename.to_string_lossy());
    match e.raw_os_error() {
        #[cfg(windows)]
        Some(code) => format!("[WinError {code}] {}: {name}", os_error_message(e)),
        #[cfg(not(windows))]
        Some(code) => format!("[Errno {code}] {}: {name}", os_error_message(e)),
        None if e.kind() == io::ErrorKind::InvalidInput && has_nul(filename) => {
            "embedded null byte".to_string()
        }
        None => e.to_string(),
    }
}

/// Python's repr() of a str: single quotes (double when the text has a `'`
/// and no `"`), backslash escapes for \\, \t, \n, \r, the quote and the
/// characters Python does not print.
pub(crate) fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_isprintable(c) => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// str.isprintable() for one character, close enough without Unicode
/// tables: control characters, separators other than the space, format
/// characters and private use are not printable.
fn py_isprintable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false;
    }
    let cp = c as u32;
    let hidden = matches!(cp,
        0xA0 | 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x8E2 | 0x1680 | 0x180E
        | 0x2000..=0x200F | 0x2028..=0x202F | 0x205F..=0x2064 | 0x2066..=0x206F
        | 0x3000 | 0xFEFF | 0xFFF9..=0xFFFB | 0xE000..=0xF8FF
        | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
        | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD);
    !hidden
}

/// posixpath.expanduser's rule for a home folder: trailing "/" dropped, and
/// an empty result is "/".
#[cfg(not(windows))]
pub(crate) fn py_home_rstrip(raw: &std::ffi::OsStr) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = raw.as_bytes();
    while let Some((b'/', head)) = bytes.split_last() {
        bytes = head;
    }
    if bytes.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
    }
}

/// Python's Path.home(): $HOME when set (else the password entry) on
/// macOS/Linux, %USERPROFILE% (else %HOMEDRIVE%%HOMEPATH%) on Windows.
pub(crate) fn py_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Some(PathBuf::from(profile));
        }
        if let Some(home_path) = std::env::var_os("HOMEPATH") {
            let mut s = std::env::var_os("HOMEDRIVE").unwrap_or_default();
            s.push(home_path);
            return Some(PathBuf::from(s));
        }
        None
    }
    #[cfg(not(windows))]
    {
        match std::env::var_os("HOME") {
            Some(home) => Some(py_home_rstrip(&home)),
            None => std::env::home_dir().map(|h| py_home_rstrip(h.as_os_str())),
        }
    }
}

/// str(Path(p)): repeated separators, "." parts and a trailing separator go.
fn pathlib_normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

/// Python's Path.resolve() (os.path.realpath, strict=False): absolute, with
/// every symlink followed and ".." applied, missing parts kept as text, and
/// the case as typed. Err only for a NUL in a part (Python's ValueError text).
pub(crate) fn py_resolve(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        resolve_windows(path)
    }
    #[cfg(not(windows))]
    {
        resolve_posix(path)
    }
}

/// One entry of realpath's stack: a part still to walk, or the mark that
/// the link pushed before it is now resolved.
#[cfg(not(windows))]
enum Step {
    Part(std::ffi::OsString),
    Resolved(PathBuf),
}

#[cfg(not(windows))]
fn push_parts(rest: &mut Vec<Step>, count: &mut usize, p: &Path) {
    let parts: Vec<std::ffi::OsString> = p
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_os_string()),
            Component::ParentDir => Some(std::ffi::OsString::from("..")),
            _ => None,
        })
        .collect();
    *count += parts.len();
    rest.extend(parts.into_iter().rev().map(Step::Part));
}

/// The port of Python 3.13's posixpath.realpath(strict=False).
#[cfg(not(windows))]
fn resolve_posix(filename: &Path) -> Result<PathBuf, String> {
    let mut rest: Vec<Step> = Vec::new();
    let mut part_count = 0usize;
    push_parts(&mut rest, &mut part_count, filename);
    let mut path = if filename.has_root() {
        PathBuf::from("/")
    } else {
        std::env::current_dir().map_err(|e| os_error_text(&e))?
    };
    // Link path -> its resolved target, or None while it is being resolved.
    let mut seen: std::collections::HashMap<PathBuf, Option<PathBuf>> =
        std::collections::HashMap::new();

    while part_count > 0 {
        let Some(step) = rest.pop() else { break };
        let name = match step {
            Step::Resolved(link) => {
                seen.insert(link, Some(path.clone()));
                continue;
            }
            Step::Part(name) => name,
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if let Some(parent) = path.parent() {
                path = parent.to_path_buf();
            }
            continue;
        }
        if name.as_encoded_bytes().contains(&0) {
            return Err("lstat: embedded null character in path".to_string());
        }
        let newpath = path.join(&name);
        let is_link = match fs::symlink_metadata(&newpath) {
            Ok(md) => md.file_type().is_symlink(),
            Err(_) => false,
        };
        if !is_link {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            // A cached target, or a loop (still None): keep the link as text.
            path = cached.clone().unwrap_or(newpath);
            continue;
        }
        let target = match fs::read_link(&newpath) {
            Ok(target) => target,
            Err(_) => {
                path = newpath;
                continue;
            }
        };
        if target.has_root() {
            path = PathBuf::from("/");
        }
        seen.insert(newpath.clone(), None);
        rest.push(Step::Resolved(newpath));
        push_parts(&mut rest, &mut part_count, &target);
    }
    Ok(path)
}

/// ntpath.realpath, close enough: absolute and normalized, then the final
/// path of the longest existing part (no \\?\ prefix) plus the rest.
#[cfg(windows)]
fn resolve_windows(path: &Path) -> Result<PathBuf, String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| os_error_text(&e))?.join(path)
    };
    let mut norm = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if norm.file_name().is_some() {
                    norm.pop();
                }
            }
            other => norm.push(other.as_os_str()),
        }
    }
    if has_nul(&norm) {
        return Ok(norm);
    }
    let comps: Vec<Component> = norm.components().collect();
    for cut in (1..=comps.len()).rev() {
        let Some(head) = comps.get(..cut) else { continue };
        let prefix: PathBuf = head.iter().collect();
        if let Ok(real) = fs::canonicalize(&prefix) {
            let text = real.to_string_lossy().into_owned();
            let mut out = if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
                PathBuf::from(format!("\\\\{unc}"))
            } else if let Some(local) = text.strip_prefix("\\\\?\\") {
                PathBuf::from(local)
            } else {
                real
            };
            for c in comps.iter().skip(cut) {
                out.push(c.as_os_str());
            }
            return Ok(out);
        }
    }
    Ok(norm)
}
