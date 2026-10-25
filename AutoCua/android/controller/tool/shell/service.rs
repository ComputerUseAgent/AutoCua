//! The host shell tool: the copy of ios/controller/tool/shell.py for the
//! Android agent. One `shell` call runs a command on the computer the agent
//! runs on (not the phone), in the sandbox workspace on the Desktop the
//! agent makes at start, through the host's own shell: sh on macOS and
//! Linux, cmd on Windows. The result is the iOS ShellService's: status,
//! action, agent_location, shell, exit_code, output (verbatim stdout +
//! stderr, "(no output)" when there is none), error.
//!
//! Kept simple, as asked: no permission-popup watcher (macOS only) and no
//! "waiting for input" detection; a command that runs past TIMEOUT is
//! stopped and reported as a timeout.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use serde_json::{json, Value};

/// How long a command may run.
const TIMEOUT: Duration = Duration::from_secs(120);

pub struct ShellService {
    /// The sandbox workspace: ~/Desktop/sandbox_workspace, the agent's cwd
    /// for commands.
    cwd: PathBuf,
}

impl ShellService {
    pub fn new(py: Python<'_>) -> Self {
        let home: String = py
            .import("pathlib")
            .and_then(|m| m.getattr("Path"))
            .and_then(|p| p.call_method0("home"))
            .and_then(|h| h.str().map(|s| s.to_string()))
            .unwrap_or_else(|_| ".".to_string());
        let cwd = PathBuf::from(home).join("Desktop").join("sandbox_workspace");
        let _ = std::fs::create_dir_all(&cwd);
        ShellService { cwd }
    }

    pub fn get_cwd(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    /// Execute a shell command and return the formatted result.
    pub fn run(&self, command: &str) -> Value {
        let agent_location = self.get_cwd();
        match self.execute(command) {
            Err(e) => {
                eprintln!("ShellService error: {e}");
                json!({
                    "status": "failed",
                    "action": "shell",
                    "agent_location": agent_location,
                    "shell": command,
                    "error": e,
                })
            }
            Ok(Run { timed_out: true, stdout, stderr, .. }) => {
                let mut response = json!({
                    "status": "timeout",
                    "action": "shell",
                    "agent_location": agent_location,
                    "shell": command,
                    "message": format!("Command timed out after {} seconds", TIMEOUT.as_secs()),
                });
                let output = format!("{stdout}{stderr}");
                if !output.trim().is_empty() {
                    response["output"] = json!(output.trim());
                }
                response
            }
            Ok(Run { stdout, stderr, code, .. }) => {
                let succeeded = code == Some(0);
                let mut response = json!({
                    "status": if succeeded { "success" } else { "failed" },
                    "action": "shell",
                    "agent_location": agent_location,
                    "shell": command,
                });
                // Always surface the exit code so the agent can tell a real
                // error from an empty result (e.g. grep exits 1 on no match).
                if let Some(code) = code {
                    response["exit_code"] = json!(code);
                }
                // Show exactly what the command returned (verbatim stdout +
                // stderr). When the terminal returns absolutely nothing, show
                // a clear marker instead of a blank string.
                let output = format!("{stdout}{stderr}");
                let clean = output.trim();
                response["output"] = json!(if clean.is_empty() { "(no output)" } else { clean });
                response
            }
        }
    }

    fn execute(&self, command: &str) -> Result<Run, String> {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(command);
            c
        };
        let mut child = cmd
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        // Drain both pipes on their own threads: a command that writes more
        // than the pipe holds would otherwise block before it can exit.
        let mut out_pipe = child.stdout.take().ok_or("no stdout")?;
        let mut err_pipe = child.stderr.take().ok_or("no stderr")?;
        let out_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out_pipe.read_to_end(&mut buf);
            buf
        });
        let err_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err_pipe.read_to_end(&mut buf);
            buf
        });
        let started = Instant::now();
        let mut timed_out = false;
        let code = loop {
            match child.try_wait().map_err(|e| e.to_string())? {
                Some(status) => break status.code(),
                None if started.elapsed() >= TIMEOUT => {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
        Ok(Run { stdout, stderr, code, timed_out })
    }
}

struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    timed_out: bool,
}
