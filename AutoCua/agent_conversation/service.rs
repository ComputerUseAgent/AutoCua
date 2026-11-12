//! ConversationService: permanent, resumable chat memory for the UI path.
//!
//! The port of agent_conversation/service.py. This is the ONE place that
//! knows how a chat is persisted, optimized and replayed. app.py is thin glue:
//!
//! ```text
//!     sid, prior = conversation.start_or_resume(session_id, task)   # load
//!     agent = AgentService(..., prior_history=prior)                # trigger
//!     agent.process_request(task)
//!     conversation.save_run(sid, agent.assistant_messages,          # save
//!                           agent.tool_responses, status, message, task)
//! ```
//!
//! On-disk layout. The data lives OUTSIDE the install folder, so uninstalling
//! the app can never destroy the user's chats (the Windows installer
//! uninstalls with `[UninstallDelete] Type: filesandordirs; Name: "{app}"`,
//! which deletes the WHOLE install directory, and chats used to be written
//! inside it):
//!
//! ```text
//!     <home>/AutoCua_data/agent_conversation/     (packaged build)
//!     <repo>/AutoCua_data/agent_conversation/     (python app.py)
//!         index.json                      <- { id: {title, created_at,
//!                                               updated_at, last_done_message} }
//!         settings.json                   <- last-used provider/model (frontend)
//!         <session_id>/conversation.json  <- one folder per session: the
//!                                            optimized, resumable history snapshot
//! ```
//!
//! There is exactly ONE of these for the whole app: windows, mac and ios all
//! route their memory through this single service, so this one root is every
//! platform's chat store.
//!
//! The saved history is the agent's FULL per-step memory: each assistant step
//! with its thinking / eval / decision / memory / next_goal / action intact,
//! paired 1:1 with the agent's already-compacted tool results, and capped with
//! a single "terminal note" step carrying a clean done-message, so a resumed
//! run always knows how the previous run ended, on EVERY ending (done, user
//! stop, error, provider kill, step limit, parse fails). Storing the FULL steps
//! (not a trimmed copy) is deliberate: the debug download (render_readable) is
//! then the TRUE agent memory, readable like main.py's conversation_N.txt.
//! Resume token-efficiency is unaffected: the agent's OWN message builder trims
//! older steps to decision/memory/next_goal when it sends them to the LLM.
//!
//! It contains NO system prompt and NO screenshot bytes, so a resumed run never
//! double-loads the system prompt: the agent's message builder always prepends
//! exactly one system message and seeds these two lists underneath it. (The
//! debug log adds the system prompt for readability only, supplied by app.py
//! at export.)
//!
//! How the port keeps the Python's behaviour:
//! - Every value a caller hands in, and every value read from a chat file,
//!   stays a Python object, and what the Python did with it (str(), int(),
//!   format(), truthiness, `x or default`, .get, ==, len, iteration, the
//!   sort) is the real Python operation, called through PyO3. A caller gets
//!   back the same types as before, and odd input fails the way it did.
//! - JSON is read and written here, in Rust, with Python's rules: json_loads
//!   reads what json.loads reads (NaN, Infinity, ints of any size, lone
//!   surrogate escapes; a repeated key keeps its first place and its last
//!   value), and JsonOut writes what json.dump and json.dumps write (indent=2
//!   or compact, ensure_ascii, Python's float repr, the same TypeError and
//!   ValueError, and for a file, the text written before the error).
//! - Paths come from Python (data_root, install_dir and _ensure, bound from the
//!   AutoCua package as the Python module imported them), so the
//!   AutoCua_DATA_DIR override and the compiled-build rules stay in one place.
//!   So does the clock: time.time() and time.strftime() (local time) are read
//!   from Python's time module when they are needed, as the Python read them.
//! - Logging goes to the "agent_conversation" logger. What the Python logged
//!   with logger.exception is an ERROR record whose exc_info carries the error.
//! - Files are written as Python's text mode writes them ("\n" is "\r\n" on
//!   Windows), except the merged legacy index, which the Python wrote as bytes.
//! - Not kept, on purpose: a Python string holding a lone surrogate (half of a
//!   UTF-16 pair, such as a broken emoji pasted into the chat box) cannot be a
//!   Rust string, so text built here from it (the done message, the memory
//!   log, a rendered tool call, an exchanges.json row) has U+FFFD in its
//!   place. Python failed on that text instead: json.dump left exchanges.json
//!   cut off at the string and memory_log.txt empty. Values that only pass
//!   through to an ensure_ascii file (the task and title in index.json and
//!   conversation.json) keep the exact \udXXX escape.
//! - Nesting limits are Python's from a shallow call (json.dump 994 levels,
//!   json.loads and json.dumps 9998), raised as the same RecursionError;
//!   CPython's own limit shrinks a little as the caller's stack grows.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pyo3::exceptions::{
    PyException, PyOSError, PyOverflowError, PyRecursionError, PyTypeError, PyUnicodeDecodeError,
    PyValueError,
};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyBytes, PyDict, PyFloat, PyInt, PyList, PySet, PyString, PyTuple};
use regex::Regex;
use serde_json::json;

use crate::coder::{dumps, dumps_pretty, py_rstrip, py_splitlines, py_strip, read_text};

/// A Coder chat's bridge note is one `insight` call whose id starts with
/// this. The Rust coder looks for the same prefix (coder/main/service.rs
/// BRIDGE_ID_PREFIX): keep the two identical.
pub const SHELL_BRIDGE_ID_PREFIX: &str = "bridge_";

/// The logger every record of this module goes to.
const LOGGER_NAME: &str = "agent_conversation";

/// The pattern a chat id must match (ConversationService._SAFE_ID), as Python
/// wrote it.
const SAFE_ID_PATTERN: &str = r"^[A-Za-z0-9_-]{1,64}$";

/// The legacy entries the migration may touch (ConversationService._SAFE_ENTRY).
const SAFE_ENTRY_PATTERN: &str = r"^[A-Za-z0-9._-]+$";

/// The only files the migration moves (ConversationService._LEGACY_FILES).
const LEGACY_FILES: [&str; 2] = ["index.json", "settings.json"];

/// The only folders the migration moves (ConversationService._LEGACY_DIR_PREFIX).
const LEGACY_DIR_PREFIX: &str = "chat_";

/// The main agent's bridge note: its next_goal.
const MAIN_BRIDGE_GOAL: &str =
    "Previous run concluded. Awaiting the user's next request; resume from this point.";

/// The first line of memory_log.txt.
const MEMORY_LOG_TITLE: &str =
    "=== AGENT MEMORY \u{2014} exact payload sent to the model (true snapshot) ===";

// ---------------------------------------------------------------------------
// The module, its logger and its paths
// ---------------------------------------------------------------------------

/// This part of the extension, kept by register() so a method reads the
/// module's attributes (logger, data_root, install_dir, _ensure) when it runs,
/// the way the Python read its module globals.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// A module attribute, read at call time.
fn module_attr<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    match MODULE.get(py) {
        Some(m) => m.bind(py).getattr(name),
        None if name == "logger" => {
            py.import("logging")?.call_method1("getLogger", (LOGGER_NAME,))
        }
        None => py.import("AutoCua")?.getattr(name),
    }
}

/// One record to the module's logger: logger.<level>(msg, *args), with
/// exc_info carrying `err` when there is one (what logger.exception records).
/// A logging failure is not the caller's problem, as in Python.
fn log<'py>(
    py: Python<'py>,
    level: &str,
    msg: &str,
    args: Vec<Bound<'py, PyAny>>,
    err: Option<&PyErr>,
) {
    let record = || -> PyResult<()> {
        let logger = module_attr(py, "logger")?;
        let mut all: Vec<Bound<'py, PyAny>> = Vec::with_capacity(args.len() + 1);
        all.push(PyString::new(py, msg).into_any());
        all.extend(args);
        let call_args = PyTuple::new(py, all)?;
        let kwargs = match err {
            Some(e) => {
                let kw = PyDict::new(py);
                let tb = match e.traceback(py) {
                    Some(t) => t.into_any(),
                    None => py.None().into_bound(py),
                };
                kw.set_item("exc_info", (e.get_type(py), e.value(py), tb))?;
                Some(kw)
            }
            None => None,
        };
        logger.call_method(level, call_args, kwargs.as_ref())?;
        Ok(())
    };
    let _ = record();
}

/// logger.exception(msg, *args) inside an `except`, for `err`.
fn log_exception<'py>(py: Python<'py>, msg: &str, args: Vec<Bound<'py, PyAny>>, err: &PyErr) {
    log(py, "error", msg, args, Some(err));
}

/// Python's `except Exception`: an Exception comes back to be logged, anything
/// else (KeyboardInterrupt, SystemExit) goes on up.
fn caught(py: Python<'_>, e: PyErr) -> PyResult<PyErr> {
    if e.is_instance_of::<PyException>(py) {
        Ok(e)
    } else {
        Err(e)
    }
}

/// os.fspath(p) as a Rust path (a non-UTF-8 name by its bytes).
fn fspath(p: &Bound<'_, PyAny>) -> PyResult<PathBuf> {
    let py = p.py();
    let os = py.import("os")?;
    let raw = os.call_method1("fspath", (p,))?;
    if let Ok(s) = raw.cast::<PyString>() {
        if let Ok(text) = s.to_str() {
            return Ok(PathBuf::from(text));
        }
    }
    let encoded = os.call_method1("fsencode", (&raw,))?;
    let bytes = encoded.cast::<PyBytes>()?.as_bytes().to_vec();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// pathlib.Path for a Rust path, for log arguments and return values.
fn py_path<'py>(py: Python<'py>, p: &Path) -> PyResult<Bound<'py, PyAny>> {
    let pathlib = py.import("pathlib")?;
    match p.to_str() {
        Some(text) => pathlib.getattr("Path")?.call1((text,)),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                let raw = PyBytes::new(py, p.as_os_str().as_bytes());
                let text = py.import("os")?.call_method1("fsdecode", (raw,))?;
                pathlib.getattr("Path")?.call1((text,))
            }
            #[cfg(not(unix))]
            {
                pathlib.getattr("Path")?.call1((p.to_string_lossy().into_owned(),))
            }
        }
    }
}

/// `p.with_name(p.name + ".part")`: the staging name beside `p`.
fn part_path(p: &Path) -> PathBuf {
    let mut name = p.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    p.with_file_name(name)
}

// ---------------------------------------------------------------------------
// Python's errors for Rust's I/O
// ---------------------------------------------------------------------------

/// The OSError Python raises for `e` on `path`: the same errno, strerror and
/// filename, so str(error) reads as it did (FileNotFoundError and friends
/// come from OSError's own errno mapping).
fn os_error(py: Python<'_>, e: &io::Error, path: &Path) -> PyErr {
    os_error_of(py, e, path, None)
}

/// The OSError of a two-path call (os.replace): "...: 'src' -> 'dst'".
fn os_error2(py: Python<'_>, e: &io::Error, path: &Path, path2: &Path) -> PyErr {
    os_error_of(py, e, path, Some(path2))
}

fn os_error_of(py: Python<'_>, e: &io::Error, path: &Path, path2: Option<&Path>) -> PyErr {
    let Some(code) = e.raw_os_error() else {
        return PyOSError::new_err(e.to_string());
    };
    let made = || -> PyResult<PyErr> {
        let filename = path.to_string_lossy().into_owned();
        let filename2 = path2.map(|p| p.to_string_lossy().into_owned());
        // Rust's code is an errno on Unix and a Win32 error on Windows, which
        // OSError takes as `winerror` (it maps the errno and the subclass
        // itself and reads "[WinError 5] Access is denied: ...").
        #[cfg(windows)]
        let (errno, strerror, winerror) = {
            let text = e.to_string();
            let message = text.split(" (os error").next().unwrap_or("");
            let message = message.trim_end_matches(|c: char| c <= ' ' || c == '.').to_string();
            (0, PyString::new(py, &message).into_any(), Some(code))
        };
        #[cfg(not(windows))]
        let (errno, strerror, winerror) = (code, py.import("os")?.call_method1("strerror", (code,))?, None::<i32>);
        let exc = py.get_type::<PyOSError>().call1((errno, strerror, filename, winerror, filename2))?;
        Ok(PyErr::from_value(exc))
    };
    made().unwrap_or_else(|err| err)
}

/// The UnicodeDecodeError bytes.decode("utf-8") raises for `bytes`.
fn decode_failure(py: Python<'_>, bytes: &[u8], err: std::str::Utf8Error) -> PyErr {
    let start = err.valid_up_to();
    let (end, reason) = match err.error_len() {
        None => (bytes.len(), "unexpected end of data"),
        Some(n) => {
            let first = bytes.get(start).copied().unwrap_or(0);
            let reason = if (0x80..0xC2).contains(&first) || first >= 0xF5 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (start + n, reason)
        }
    };
    let made = py
        .get_type::<PyUnicodeDecodeError>()
        .call1(("utf-8", PyBytes::new(py, bytes), start, end, reason));
    match made {
        Ok(exc) => PyErr::from_value(exc),
        Err(e) => e,
    }
}

// ---------------------------------------------------------------------------
// Files, the way Python's open() reads and writes them
// ---------------------------------------------------------------------------

/// open(path, "r", encoding="utf-8").read(): strict UTF-8, universal newlines.
fn read_text_file(py: Python<'_>, path: &Path) -> PyResult<String> {
    let owned = path.to_path_buf();
    match py.detach(move || read_text(&owned)) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            // Not UTF-8: Python's UnicodeDecodeError, from the bytes themselves.
            let owned = path.to_path_buf();
            let bytes = py.detach(move || fs::read(&owned)).map_err(|err| os_error(py, &err, path))?;
            match std::str::from_utf8(&bytes) {
                Err(err) => Err(decode_failure(py, &bytes, err)),
                Ok(_) => Err(os_error(py, &e, path)),
            }
        }
        Err(e) => Err(os_error(py, &e, path)),
    }
}

/// What Python's text mode writes for `text`: "\n" as os.linesep.
fn text_mode(text: &str) -> Cow<'_, str> {
    #[cfg(windows)]
    {
        Cow::Owned(text.replace('\n', "\r\n"))
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(text)
    }
}

/// path.parent.mkdir(parents=True, exist_ok=True)
fn mkdir_parent(py: Python<'_>, path: &Path) -> PyResult<()> {
    let Some(parent) = path.parent() else { return Ok(()) };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let owned = parent.to_path_buf();
    py.detach(move || fs::create_dir_all(&owned)).map_err(|e| os_error(py, &e, parent))
}

/// open(path, "w", encoding="utf-8"): created or emptied.
fn create_file(py: Python<'_>, path: &Path) -> PyResult<File> {
    let owned = path.to_path_buf();
    py.detach(move || File::create(&owned)).map_err(|e| os_error(py, &e, path))
}

/// fh.write(text) on a text-mode file.
fn write_all(py: Python<'_>, file: &mut File, text: &str, path: &Path) -> PyResult<()> {
    let data = text_mode(text);
    py.detach(|| file.write_all(data.as_bytes())).map_err(|e| os_error(py, &e, path))
}

/// with open(path, "w", encoding="utf-8") as fh: fh.write(text)
fn write_text_file(py: Python<'_>, path: &Path, text: &str) -> PyResult<()> {
    let mut file = create_file(py, path)?;
    write_all(py, &mut file, text, path)
}

/// path.parent.mkdir(parents=True, exist_ok=True), then
/// `with open(path, "w", encoding="utf-8") as fh: json.dump(obj, fh, indent=2,
/// ensure_ascii=...)`. A value json cannot write raises after the text before
/// it is in the file, as json.dump's streaming leaves it.
fn dump_json_file(
    py: Python<'_>,
    path: &Path,
    obj: &Bound<'_, PyAny>,
    ensure_ascii: bool,
) -> PyResult<()> {
    mkdir_parent(py, path)?;
    let mut file = create_file(py, path)?;
    let mut out = JsonOut::new(py, true, ensure_ascii, true);
    let failed = out.write(obj).err();
    write_all(py, &mut file, &out.text, path)?;
    match failed {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Path.read_bytes(), logged and None on failure (ConversationService._read_bytes).
fn read_bytes_logged<'py>(
    py: Python<'py>,
    path: &Path,
    shown: impl FnOnce() -> PyResult<Bound<'py, PyAny>>,
) -> PyResult<Option<Vec<u8>>> {
    let owned = path.to_path_buf();
    match py.detach(move || fs::read(&owned)) {
        Ok(data) => Ok(Some(data)),
        Err(e) => {
            let err = os_error(py, &e, path);
            log_exception(py, "read %s", vec![shown()?], &err);
            Ok(None)
        }
    }
}

/// shutil.rmtree(path, ignore_errors=True): best effort, and nothing at all
/// when `path` is a symlink or not a folder (rmtree refuses those).
fn rmtree(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else { return };
    if !meta.is_dir() {
        return;
    }
    clear_dir(path);
    let _ = fs::remove_dir(path);
}

/// Everything inside `dir`, deepest first; symlinks are unlinked, never followed.
fn clear_dir(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => {
                clear_dir(&p);
                let _ = fs::remove_dir(&p);
            }
            _ => {
                let _ = fs::remove_file(&p);
            }
        }
    }
}

/// shutil.copystat: access and modification times, then the mode. Windows
/// may refuse a folder's times, which copytree ignores there.
fn copy_stat(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = fs::metadata(src)?;
    let times = fs::FileTimes::new()
        .set_accessed(meta.accessed()?)
        .set_modified(meta.modified()?);
    #[cfg(unix)]
    {
        File::open(dst)?.set_times(times)?;
    }
    #[cfg(not(unix))]
    {
        if let Ok(f) = fs::OpenOptions::new().write(true).open(dst) {
            let _ = f.set_times(times);
        }
    }
    fs::set_permissions(dst, meta.permissions())
}

/// One failure copytree collected and went on past: shutil's (srcname,
/// dstname, why) with the OSError behind `why` (its errno and the path it
/// names), or the text of a SpecialFileError.
struct CopyFailure {
    src: PathBuf,
    dst: PathBuf,
    why: Result<(io::Error, PathBuf), String>,
}

/// How copytree failed: at once with an OSError (src unreadable, dst already
/// there), or after trying everything, with the failures shutil.Error lists.
enum TreeError {
    Os(io::Error, PathBuf),
    Many(Vec<CopyFailure>),
}

/// One file as shutil.copy2 copies it: a named pipe refused (it would block),
/// the data, then copystat.
fn copy_file(from: &Path, to: &Path) -> Result<(), Result<(io::Error, PathBuf), String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if fs::metadata(from).is_ok_and(|m| m.file_type().is_fifo()) {
            return Err(Err(format!("`{}` is a named pipe", from.display())));
        }
    }
    File::open(from).map_err(|e| Ok((e, from.to_path_buf())))?;
    fs::copy(from, to).map_err(|e| Ok((e, to.to_path_buf())))?;
    copy_stat(from, to).map_err(|e| Ok((e, to.to_path_buf())))
}

/// shutil.copytree(src, dst) with its defaults: symlinks followed, dst must
/// not exist yet, every file copied with copy2 (data, then times and mode),
/// each folder's own times set last. A failure inside is collected and the
/// rest still copied, then all of them are reported together, as shutil does.
fn copytree(src: &Path, dst: &Path) -> Result<(), TreeError> {
    let entries: Vec<fs::DirEntry> = fs::read_dir(src)
        .and_then(|it| it.collect())
        .map_err(|e| TreeError::Os(e, src.to_path_buf()))?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| TreeError::Os(e, parent.to_path_buf()))?;
    }
    fs::create_dir(dst).map_err(|e| TreeError::Os(e, dst.to_path_buf()))?;
    let mut failures: Vec<CopyFailure> = Vec::new();
    for entry in entries {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let is_dir = fs::metadata(&from).is_ok_and(|m| m.is_dir());
        if is_dir {
            match copytree(&from, &to) {
                Ok(()) => {}
                Err(TreeError::Many(inner)) => failures.extend(inner),
                Err(TreeError::Os(e, at)) => {
                    failures.push(CopyFailure { src: from, dst: to, why: Ok((e, at)) })
                }
            }
        } else if let Err(why) = copy_file(&from, &to) {
            failures.push(CopyFailure { src: from, dst: to, why });
        }
    }
    if let Err(e) = copy_stat(src, dst) {
        // Copying file access times may fail on Windows: shutil ignores it there.
        if cfg!(unix) {
            failures.push(CopyFailure {
                src: src.to_path_buf(),
                dst: dst.to_path_buf(),
                why: Ok((e, dst.to_path_buf())),
            });
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(TreeError::Many(failures))
    }
}

/// How a legacy move failed: inside copytree, at os.replace (two paths), or at
/// another single-path call.
enum MoveError {
    Tree(TreeError),
    Replace(io::Error, PathBuf, PathBuf),
    Os(io::Error, PathBuf),
}

/// The Python error for a failed copytree: the OSError, or shutil.Error with
/// its list of (srcname, dstname, str(why)).
fn tree_error(py: Python<'_>, e: TreeError) -> PyErr {
    match e {
        TreeError::Os(err, at) => os_error(py, &err, &at),
        TreeError::Many(failures) => {
            let made = || -> PyResult<PyErr> {
                let list = PyList::empty(py);
                for f in failures {
                    let why = match f.why {
                        Ok((err, at)) => {
                            let exc = os_error(py, &err, &at);
                            text_of(&exc.value(py).str()?)
                        }
                        Err(text) => text,
                    };
                    let src = f.src.to_string_lossy().into_owned();
                    let dst = f.dst.to_string_lossy().into_owned();
                    list.append((src, dst, why))?;
                }
                let exc = py.import("shutil")?.getattr("Error")?.call1((list,))?;
                Ok(PyErr::from_value(exc))
            };
            made().unwrap_or_else(|err| err)
        }
    }
}

/// The names in a folder in the order Python's sorted(Path.iterdir()) gives
/// them (case-blind on Windows). A name that is not UTF-8 can never be a
/// legacy entry, so it is left out.
fn sorted_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    if cfg!(windows) {
        names.sort_by_key(|n| n.to_lowercase());
    } else {
        names.sort();
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// Python's text and object rules
// ---------------------------------------------------------------------------

/// A regex compiled once (None only if the pattern were invalid).
fn cached_regex(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// _SAFE_ID.match(text): Python's `$` also matches before one final "\n".
fn safe_id(text: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached_regex(&RE, r"^[A-Za-z0-9_-]{1,64}\n?$").is_some_and(|re| re.is_match(text))
}

/// _SAFE_ENTRY.match(name), with the same `$`.
fn safe_entry(text: &str) -> bool {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached_regex(&RE, r"^[A-Za-z0-9._-]+\n?$").is_some_and(|re| re.is_match(text))
}

/// The run of characters export_to_downloads turns into one "-".
fn slug_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    cached_regex(&RE, r"[^A-Za-z0-9._-]+")
}

/// A Python str's text, one U+FFFD for each lone surrogate (half of a UTF-16
/// pair, which a Rust string cannot hold).
fn text_of(s: &Bound<'_, PyString>) -> String {
    if let Ok(text) = s.to_str() {
        return text.to_owned();
    }
    let units = s
        .call_method1("encode", ("utf-16-le", "surrogatepass"))
        .and_then(|raw| raw.cast_into::<PyBytes>().map_err(PyErr::from))
        .map(|raw| {
            raw.as_bytes()
                .chunks_exact(2)
                .filter_map(|pair| match pair {
                    [lo, hi] => Some(u16::from_le_bytes([*lo, *hi])),
                    _ => None,
                })
                .collect::<Vec<u16>>()
        });
    match units {
        Ok(units) => String::from_utf16_lossy(&units),
        Err(_) => s.to_string_lossy().into_owned(),
    }
}

/// str(x) as text.
fn str_text(x: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = x.cast_exact::<PyString>() {
        return Ok(text_of(s));
    }
    Ok(text_of(&x.str()?))
}

/// f"{x}", which is format(x, "").
fn fmt_text(x: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(s) = x.cast_exact::<PyString>() {
        return Ok(text_of(s));
    }
    let formatted = x.py().import("builtins")?.getattr("format")?.call1((x,))?;
    Ok(text_of(formatted.cast::<PyString>()?))
}

/// int(x)
fn py_int<'py>(x: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    x.py().get_type::<PyInt>().call1((x,))
}

/// `x or default`
fn or_else<'py>(x: Bound<'py, PyAny>, default: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    Ok(if x.is_truthy()? { x } else { default })
}

/// `x or ""`
fn or_str<'py>(x: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let empty = PyString::new(x.py(), "").into_any();
    or_else(x, empty)
}

/// `x or []`
fn or_list<'py>(x: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let empty = PyList::empty(x.py()).into_any();
    or_else(x, empty)
}

/// `x or {}`
fn or_dict<'py>(x: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let empty = PyDict::new(x.py()).into_any();
    or_else(x, empty)
}

/// x.get(key), the method call (AttributeError for a non-mapping, as in Python).
fn get<'py>(x: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    x.call_method1("get", (key,))
}

/// x.get(key, default)
fn get_or<'py, D>(x: &Bound<'py, PyAny>, key: &str, default: D) -> PyResult<Bound<'py, PyAny>>
where
    D: IntoPyObject<'py>,
{
    x.call_method1("get", (key, default))
}

/// dict(x)
fn dict_of<'py>(x: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    x.py().get_type::<PyDict>().call1((x,))
}

/// The Python name of x's class, as error messages give it.
fn class_name(x: &Bound<'_, PyAny>) -> PyResult<String> {
    str_text(&x.getattr("__class__")?.getattr("__name__")?)
}

/// str.upper(), Python's own for anything beyond ASCII.
fn upper(s: &Bound<'_, PyString>) -> PyResult<String> {
    let text = text_of(s);
    if text.is_ascii() {
        return Ok(text.to_ascii_uppercase());
    }
    str_text(&s.call_method0("upper")?)
}

/// time.time(), read from Python's time module when it is called, as the
/// Python read it, so a test that freezes the clock freezes this one too.
fn py_time(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py.import("time")?.call_method0("time")
}

/// int(time.time())
fn now_secs(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    py_int(&py_time(py)?)
}

/// The lines of a log being built, joined with "\n" at the end. A piece that
/// is not a str (a task or system prompt of another type) makes the join fail
/// with Python's own TypeError, as "\n".join(out) did.
struct Lines<'py> {
    pieces: Vec<Piece<'py>>,
}

enum Piece<'py> {
    Text(String),
    Obj(Bound<'py, PyAny>),
}

impl<'py> Lines<'py> {
    fn new() -> Self {
        Lines { pieces: Vec::new() }
    }

    fn text(&mut self, s: impl Into<String>) {
        self.pieces.push(Piece::Text(s.into()));
    }

    fn obj(&mut self, o: Bound<'py, PyAny>) {
        match o.cast::<PyString>() {
            Ok(s) => self.pieces.push(Piece::Text(text_of(s))),
            Err(_) => self.pieces.push(Piece::Obj(o)),
        }
    }

    fn join(self, py: Python<'py>) -> PyResult<String> {
        if self.pieces.iter().all(|p| matches!(p, Piece::Text(_))) {
            let texts: Vec<String> = self
                .pieces
                .into_iter()
                .filter_map(|p| match p {
                    Piece::Text(t) => Some(t),
                    Piece::Obj(_) => None,
                })
                .collect();
            return Ok(texts.join("\n"));
        }
        let list = PyList::empty(py);
        for piece in self.pieces {
            match piece {
                Piece::Text(t) => list.append(t)?,
                Piece::Obj(o) => list.append(o)?,
            }
        }
        let joined = PyString::new(py, "\n").call_method1("join", (list,))?;
        str_text(&joined)
    }
}

// ---------------------------------------------------------------------------
// JSON in: json.loads, read into the same Python objects
// ---------------------------------------------------------------------------

/// How deep json.loads nests before Python's C recursion limit stops it.
const LOADS_DEPTH: usize = 9998;

/// JSON whitespace, as json's scanner skips it.
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// json.JSONDecodeError(msg, doc, pos), pos counted in characters as Python
/// counts it.
fn decode_error(py: Python<'_>, msg: &str, s: &str, byte_pos: usize) -> PyErr {
    let pos = s.get(..byte_pos).map(|head| head.chars().count()).unwrap_or(0);
    let made = py
        .import("json")
        .and_then(|json| json.getattr("JSONDecodeError"))
        .and_then(|cls| cls.call1((msg, s, pos)));
    match made {
        Ok(exc) => PyErr::from_value(exc),
        Err(e) => e,
    }
}

/// A container being read: a list, or a dict with the key of the value that
/// comes next.
enum Open<'py> {
    List(Bound<'py, PyList>),
    Dict(Bound<'py, PyDict>, Bound<'py, PyAny>),
}

/// json.loads(s): the same objects, errors and messages as Python's scanner.
fn json_loads<'py>(py: Python<'py>, s: &str) -> PyResult<Bound<'py, PyAny>> {
    if s.starts_with('\u{feff}') {
        return Err(decode_error(py, "Unexpected UTF-8 BOM (decode using utf-8-sig)", s, 0));
    }
    let b = s.as_bytes();
    let (value, end) = scan(py, s, skip_ws(b, 0))?;
    let end = skip_ws(b, end);
    if end != b.len() {
        return Err(decode_error(py, "Extra data", s, end));
    }
    Ok(value)
}

/// One JSON value from `idx`, with the index after it. A loop with its own
/// stack rather than recursion, so a deep document can never run the native
/// stack out; Python's depth limit is kept as a RecursionError.
fn scan<'py>(py: Python<'py>, s: &str, mut idx: usize) -> PyResult<(Bound<'py, PyAny>, usize)> {
    let b = s.as_bytes();
    let mut stack: Vec<Open<'py>> = Vec::new();
    loop {
        let rest = b.get(idx..).unwrap_or(&[]);
        let (mut value, mut end): (Bound<'py, PyAny>, usize) = match rest.first() {
            None => return Err(decode_error(py, "Expecting value", s, idx)),
            Some(b'"') => scan_string(py, s, idx + 1)?,
            Some(&c @ (b'{' | b'[')) => {
                if stack.len() >= LOADS_DEPTH {
                    let what = if c == b'{' { "object" } else { "array" };
                    return Err(PyRecursionError::new_err(format!(
                        "maximum recursion depth exceeded while decoding a JSON {what} from a unicode string"
                    )));
                }
                let i = skip_ws(b, idx + 1);
                if c == b'[' {
                    if b.get(i) == Some(&b']') {
                        (PyList::empty(py).into_any(), i + 1)
                    } else {
                        stack.push(Open::List(PyList::empty(py)));
                        idx = i;
                        continue;
                    }
                } else if b.get(i) == Some(&b'}') {
                    (PyDict::new(py).into_any(), i + 1)
                } else {
                    let (key, at) = scan_key(py, s, i)?;
                    stack.push(Open::Dict(PyDict::new(py), key));
                    idx = at;
                    continue;
                }
            }
            Some(b'n') if rest.starts_with(b"null") => (py.None().into_bound(py), idx + 4),
            Some(b't') if rest.starts_with(b"true") => (PyBool::new(py, true).to_owned().into_any(), idx + 4),
            Some(b'f') if rest.starts_with(b"false") => {
                (PyBool::new(py, false).to_owned().into_any(), idx + 5)
            }
            Some(b'N') if rest.starts_with(b"NaN") => (PyFloat::new(py, f64::NAN).into_any(), idx + 3),
            Some(b'I') if rest.starts_with(b"Infinity") => {
                (PyFloat::new(py, f64::INFINITY).into_any(), idx + 8)
            }
            Some(b'-') if rest.starts_with(b"-Infinity") => {
                (PyFloat::new(py, f64::NEG_INFINITY).into_any(), idx + 9)
            }
            Some(_) => match scan_number(py, s, idx)? {
                Some(found) => found,
                None => return Err(decode_error(py, "Expecting value", s, idx)),
            },
        };
        // Hand the value to the containers it completes.
        loop {
            let (is_list, close) = match stack.last() {
                None => return Ok((value, end)),
                Some(Open::List(list)) => {
                    list.append(&value)?;
                    (true, b']')
                }
                Some(Open::Dict(dict, key)) => {
                    dict.set_item(key, &value)?;
                    (false, b'}')
                }
            };
            let i = skip_ws(b, end);
            match b.get(i) {
                Some(&c) if c == close => {
                    value = match stack.pop() {
                        Some(Open::List(list)) => list.into_any(),
                        Some(Open::Dict(dict, _)) => dict.into_any(),
                        None => return Ok((value, end)),
                    };
                    end = i + 1;
                }
                Some(b',') => {
                    let j = skip_ws(b, i + 1);
                    if b.get(j) == Some(&close) {
                        let what = if is_list { "array" } else { "object" };
                        let msg = format!("Illegal trailing comma before end of {what}");
                        return Err(decode_error(py, &msg, s, i));
                    }
                    if is_list {
                        idx = j;
                    } else {
                        let (key, at) = scan_key(py, s, j)?;
                        if let Some(Open::Dict(_, slot)) = stack.last_mut() {
                            *slot = key;
                        }
                        idx = at;
                    }
                    break;
                }
                _ => return Err(decode_error(py, "Expecting ',' delimiter", s, i)),
            }
        }
    }
}

/// A dict key at `i` and its ':' delimiter; the index of the value after it.
fn scan_key<'py>(py: Python<'py>, s: &str, i: usize) -> PyResult<(Bound<'py, PyAny>, usize)> {
    let b = s.as_bytes();
    if b.get(i) != Some(&b'"') {
        return Err(decode_error(py, "Expecting property name enclosed in double quotes", s, i));
    }
    let (key, after) = scan_string(py, s, i + 1)?;
    let colon = skip_ws(b, after);
    if b.get(colon) != Some(&b':') {
        return Err(decode_error(py, "Expecting ':' delimiter", s, colon));
    }
    Ok((key, skip_ws(b, colon + 1)))
}

/// Four hex digits.
fn hex4(digits: &[u8]) -> Option<u32> {
    if digits.len() != 4 {
        return None;
    }
    digits.iter().try_fold(0u32, |acc, d| Some(acc * 16 + char::from(*d).to_digit(16)?))
}

/// A string's text as it is read: UTF-8 until a lone surrogate turns up,
/// UTF-16 units from then on (Python strs may hold lone surrogates).
enum StrBuf {
    Utf8(String),
    Wide(Vec<u16>),
}

impl StrBuf {
    fn push_str(&mut self, t: &str) {
        match self {
            StrBuf::Utf8(s) => s.push_str(t),
            StrBuf::Wide(w) => w.extend(t.encode_utf16()),
        }
    }

    fn push_code(&mut self, cp: u32) {
        if let StrBuf::Utf8(s) = self {
            if let Some(ch) = char::from_u32(cp) {
                s.push(ch);
                return;
            }
            let units: Vec<u16> = s.encode_utf16().collect();
            *self = StrBuf::Wide(units);
        }
        if let StrBuf::Wide(w) = self {
            match char::from_u32(cp) {
                Some(ch) => {
                    let mut buf = [0u16; 2];
                    w.extend_from_slice(ch.encode_utf16(&mut buf));
                }
                // A lone surrogate: one unit of its own.
                None => w.push(u16::try_from(cp).unwrap_or(0xFFFD)),
            }
        }
    }

    fn into_py<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            StrBuf::Utf8(s) => Ok(PyString::new(py, &s).into_any()),
            StrBuf::Wide(w) => {
                let bytes: Vec<u8> = w.iter().flat_map(|u| u.to_le_bytes()).collect();
                PyBytes::new(py, &bytes).call_method1("decode", ("utf-16-le", "surrogatepass"))
            }
        }
    }
}

/// The string whose text starts at `start` (just after its opening quote);
/// the index after its closing quote.
fn scan_string<'py>(py: Python<'py>, s: &str, start: usize) -> PyResult<(Bound<'py, PyAny>, usize)> {
    let b = s.as_bytes();
    let begin = start.saturating_sub(1);
    let mut out = StrBuf::Utf8(String::new());
    let mut i = start;
    loop {
        let chunk = i;
        while let Some(&c) = b.get(i) {
            if c == b'"' || c == b'\\' || c < 0x20 {
                break;
            }
            i += 1;
        }
        if let Some(text) = s.get(chunk..i) {
            out.push_str(text);
        }
        match b.get(i) {
            None => return Err(decode_error(py, "Unterminated string starting at", s, begin)),
            Some(b'"') => return Ok((out.into_py(py)?, i + 1)),
            Some(b'\\') => {}
            Some(_) => return Err(decode_error(py, "Invalid control character at", s, i)),
        }
        // An escape: i is at the backslash.
        let Some(&e) = b.get(i + 1) else {
            return Err(decode_error(py, "Unterminated string starting at", s, begin));
        };
        if e != b'u' {
            let ch = match e {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => return Err(decode_error(py, "Invalid \\escape", s, i)),
            };
            out.push_code(u32::from(ch));
            i += 2;
            continue;
        }
        let u_at = i + 1;
        let end = u_at + 5;
        if end >= b.len() {
            return Err(decode_error(py, "Invalid \\uXXXX escape", s, u_at));
        }
        let Some(c) = b.get(u_at + 1..end).and_then(hex4) else {
            return Err(decode_error(py, "Invalid \\uXXXX escape", s, u_at));
        };
        let mut code = c;
        let mut next = end;
        if (0xD800..0xDC00).contains(&c)
            && end + 6 < b.len()
            && b.get(end) == Some(&b'\\')
            && b.get(end + 1) == Some(&b'u')
        {
            let Some(c2) = b.get(end + 2..end + 6).and_then(hex4) else {
                return Err(decode_error(py, "Invalid \\uXXXX escape", s, end + 1));
            };
            if (0xDC00..0xE000).contains(&c2) {
                code = 0x10000 + ((c - 0xD800) << 10) + (c2 - 0xDC00);
                next = end + 6;
            }
        }
        out.push_code(code);
        i = next;
    }
}

/// A number at `start`, read as json's _match_number does: an int (any size)
/// or a float; None when there is no number there.
fn scan_number<'py>(
    py: Python<'py>,
    s: &str,
    start: usize,
) -> PyResult<Option<(Bound<'py, PyAny>, usize)>> {
    let b = s.as_bytes();
    let n = b.len();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let mut i = start;
    if b.get(i) == Some(&b'-') {
        i += 1;
        if i >= n {
            return Ok(None);
        }
    }
    match b.get(i) {
        Some(b'1'..=b'9') => {
            i += 1;
            while digit(i) {
                i += 1;
            }
        }
        Some(b'0') => i += 1,
        _ => return Ok(None),
    }
    let mut is_float = false;
    if i + 1 < n && b.get(i) == Some(&b'.') && digit(i + 1) {
        is_float = true;
        i += 2;
        while digit(i) {
            i += 1;
        }
    }
    if i + 1 < n && matches!(b.get(i), Some(b'e' | b'E')) {
        let e_start = i;
        i += 1;
        if i + 1 < n && matches!(b.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        while digit(i) {
            i += 1;
        }
        if digit(i - 1) {
            is_float = true;
        } else {
            i = e_start;
        }
    }
    let text = s.get(start..i).unwrap_or("");
    let value = if is_float {
        match text.parse::<f64>() {
            Ok(f) => PyFloat::new(py, f).into_any(),
            Err(_) => py.get_type::<PyFloat>().call1((text,))?,
        }
    } else {
        match text.parse::<i64>() {
            Ok(v) => PyInt::new(py, v).into_any(),
            // Beyond 64 bits: Python's int(), digit limit included.
            Err(_) => py.get_type::<PyInt>().call1((text,))?,
        }
    };
    Ok(Some((value, i)))
}

// ---------------------------------------------------------------------------
// JSON out: json.dump / json.dumps of Python objects
// ---------------------------------------------------------------------------

/// How deep json.dump (the pure-Python encoder a file write uses) nests before
/// Python's recursion limit stops it, counted from a shallow call.
const DUMP_DEPTH: usize = 994;
/// The same for json.dumps (the C encoder): Python's C recursion limit.
const DUMPS_DEPTH: usize = 9998;

/// json's floatstr: NaN, Infinity and -Infinity by name, float.__repr__
/// otherwise. The repr is Python's own: Rust's shortest float text breaks an
/// exact tie between two shortest candidates the other way (776067594006333.25
/// is "...333.2" in Python, "...333.3" in Rust).
fn json_float(x: &Bound<'_, PyAny>) -> PyResult<String> {
    let f = x.extract::<f64>()?;
    Ok(if f.is_nan() {
        "NaN".into()
    } else if f == f64::INFINITY {
        "Infinity".into()
    } else if f == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        str_text(&x.py().get_type::<PyFloat>().call_method1("__repr__", (x,))?)?
    })
}

/// int.__repr__(x) of a Python int.
fn json_int(x: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(v) = x.extract::<i64>() {
        return Ok(v.to_string());
    }
    str_text(&x.py().get_type::<PyInt>().call_method1("__repr__", (x,))?)
}

/// What is left to write of a container.
enum Frame<'py> {
    Seq { items: Vec<Bound<'py, PyAny>>, next: usize, id: usize },
    Map { items: Vec<(Bound<'py, PyAny>, Bound<'py, PyAny>)>, next: usize, id: usize },
}

/// The next thing a frame wants written.
enum Step<'py> {
    Item(Bound<'py, PyAny>, bool),
    Pair(Bound<'py, PyAny>, Bound<'py, PyAny>, bool),
    Close(char),
}

/// A JSON writer for Python objects with json's encoder rules: indent=2 (",",
/// ": ") or compact (", ", ": "), ensure_ascii, the circular-reference check,
/// and json's TypeError for what it cannot write. On an error, `text` holds
/// what the streaming encoder had written to a file by then.
struct JsonOut<'py> {
    indent: bool,
    ensure_ascii: bool,
    file: bool,
    text: String,
    /// The objects written live as long as this GIL hold.
    gil: std::marker::PhantomData<Python<'py>>,
}

impl<'py> JsonOut<'py> {
    fn new(_py: Python<'py>, indent: bool, ensure_ascii: bool, file: bool) -> Self {
        JsonOut { indent, ensure_ascii, file, text: String::new(), gil: std::marker::PhantomData }
    }

    /// One character of a string literal.
    fn push_char(&mut self, c: u32) {
        match c {
            0x22 => self.text.push_str("\\\""),
            0x5C => self.text.push_str("\\\\"),
            0x0A => self.text.push_str("\\n"),
            0x0D => self.text.push_str("\\r"),
            0x09 => self.text.push_str("\\t"),
            0x08 => self.text.push_str("\\b"),
            0x0C => self.text.push_str("\\f"),
            c if c < 0x20 => self.text.push_str(&format!("\\u{c:04x}")),
            c if !self.ensure_ascii || c < 0x7F => match char::from_u32(c) {
                Some(ch) => self.text.push(ch),
                None => self.text.push('\u{FFFD}'),
            },
            c if c <= 0xFFFF => self.text.push_str(&format!("\\u{c:04x}")),
            c => {
                let v = c - 0x10000;
                self.text.push_str(&format!("\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)));
            }
        }
    }

    fn push_text(&mut self, t: &str) {
        self.text.push('"');
        for ch in t.chars() {
            self.push_char(u32::from(ch));
        }
        self.text.push('"');
    }

    /// A Python str as a JSON string. With ensure_ascii, a lone surrogate is
    /// escaped like any other unit, as Python does.
    fn push_str_obj(&mut self, s: &Bound<'py, PyString>) -> PyResult<()> {
        if let Ok(t) = s.to_str() {
            self.push_text(t);
            return Ok(());
        }
        if !self.ensure_ascii {
            let t = text_of(s);
            self.push_text(&t);
            return Ok(());
        }
        let raw = s.call_method1("encode", ("utf-16-le", "surrogatepass"))?;
        let bytes = raw.cast::<PyBytes>()?.as_bytes().to_vec();
        self.text.push('"');
        for pair in bytes.chunks_exact(2) {
            if let [lo, hi] = pair {
                self.push_char(u32::from(u16::from_le_bytes([*lo, *hi])));
            }
        }
        self.text.push('"');
        Ok(())
    }

    fn newline(&mut self, level: usize) {
        self.text.push('\n');
        self.text.push_str(&"  ".repeat(level));
    }

    /// The separator before an item: the first one's newline, or ", " /
    /// ",\n<indent>".
    fn separator(&mut self, first: bool, level: usize) {
        if first {
            if self.indent {
                self.newline(level);
            }
        } else if self.indent {
            self.text.push(',');
            self.newline(level);
        } else {
            self.text.push_str(", ");
        }
    }

    fn too_deep(&self) -> PyErr {
        PyRecursionError::new_err(if self.file {
            "maximum recursion depth exceeded"
        } else {
            "maximum recursion depth exceeded while encoding a JSON object"
        })
    }

    /// A dict key as json coerces it: str, float, True, False, None or int;
    /// anything else is a TypeError.
    fn key(&mut self, k: &Bound<'py, PyAny>) -> PyResult<KeyText<'py>> {
        if let Ok(s) = k.cast::<PyString>() {
            return Ok(KeyText::Str(s.clone()));
        }
        if k.is_instance_of::<PyFloat>() {
            return Ok(KeyText::Plain(json_float(k)?));
        }
        if k.is_instance_of::<PyBool>() {
            return Ok(KeyText::Plain(if k.is_truthy()? { "true" } else { "false" }.into()));
        }
        if k.is_none() {
            return Ok(KeyText::Plain("null".into()));
        }
        if k.is_instance_of::<PyInt>() {
            return Ok(KeyText::Plain(json_int(k)?));
        }
        Err(PyTypeError::new_err(format!(
            "keys must be str, int, float, bool or None, not {}",
            class_name(k)?
        )))
    }

    /// One value: a scalar is written; a container opens a frame (written as
    /// [] or {} when empty).
    fn value(
        &mut self,
        o: &Bound<'py, PyAny>,
        depth: usize,
        markers: &mut HashSet<usize>,
    ) -> PyResult<Option<Frame<'py>>> {
        if let Ok(s) = o.cast::<PyString>() {
            self.push_str_obj(s)?;
            return Ok(None);
        }
        if o.is_none() {
            self.text.push_str("null");
            return Ok(None);
        }
        if o.is_instance_of::<PyBool>() {
            self.text.push_str(if o.is_truthy()? { "true" } else { "false" });
            return Ok(None);
        }
        if o.is_instance_of::<PyInt>() {
            let t = json_int(o)?;
            self.text.push_str(&t);
            return Ok(None);
        }
        if o.is_instance_of::<PyFloat>() {
            let t = json_float(o)?;
            self.text.push_str(&t);
            return Ok(None);
        }
        let id = o.as_ptr() as usize;
        let is_seq = o.is_instance_of::<PyList>() || o.is_instance_of::<PyTuple>();
        let is_map = !is_seq && o.is_instance_of::<PyDict>();
        if !is_seq && !is_map {
            if markers.contains(&id) {
                return Err(PyValueError::new_err("Circular reference detected"));
            }
            return Err(PyTypeError::new_err(format!(
                "Object of type {} is not JSON serializable",
                class_name(o)?
            )));
        }
        let limit = if self.file { DUMP_DEPTH } else { DUMPS_DEPTH };
        if depth + 1 > limit {
            return Err(self.too_deep());
        }
        if !o.is_truthy()? {
            self.text.push_str(if is_seq { "[]" } else { "{}" });
            return Ok(None);
        }
        if markers.contains(&id) {
            return Err(PyValueError::new_err("Circular reference detected"));
        }
        if is_seq {
            let items = o.try_iter()?.collect::<PyResult<Vec<_>>>()?;
            markers.insert(id);
            self.text.push('[');
            Ok(Some(Frame::Seq { items, next: 0, id }))
        } else {
            let items = if o.is_exact_instance_of::<PyDict>() {
                o.cast::<PyDict>()?.iter().collect::<Vec<_>>()
            } else {
                let mut pairs = Vec::new();
                for pair in o.call_method0("items")?.try_iter()? {
                    let pair = pair?;
                    pairs.push((pair.get_item(0)?, pair.get_item(1)?));
                }
                pairs
            };
            markers.insert(id);
            self.text.push('{');
            Ok(Some(Frame::Map { items, next: 0, id }))
        }
    }

    /// Write `top`. On an error, `self.text` keeps what came before it.
    fn write(&mut self, top: &Bound<'py, PyAny>) -> PyResult<()> {
        let mut markers: HashSet<usize> = HashSet::new();
        let mut stack: Vec<Frame<'py>> = Vec::new();
        if let Some(frame) = self.value(top, 0, &mut markers)? {
            stack.push(frame);
        }
        loop {
            let level = stack.len();
            let step = match stack.last_mut() {
                None => return Ok(()),
                Some(Frame::Seq { items, next, .. }) => match items.get(*next) {
                    Some(item) => {
                        let first = *next == 0;
                        *next += 1;
                        Step::Item(item.clone(), first)
                    }
                    None => Step::Close(']'),
                },
                Some(Frame::Map { items, next, .. }) => match items.get(*next) {
                    Some((k, v)) => {
                        let first = *next == 0;
                        *next += 1;
                        Step::Pair(k.clone(), v.clone(), first)
                    }
                    None => Step::Close('}'),
                },
            };
            match step {
                Step::Item(item, first) => {
                    self.separator(first, level);
                    if let Some(frame) = self.value(&item, level, &mut markers)? {
                        stack.push(frame);
                    }
                }
                Step::Pair(k, v, first) => {
                    let key = self.key(&k)?;
                    self.separator(first, level);
                    match key {
                        KeyText::Str(s) => self.push_str_obj(&s)?,
                        KeyText::Plain(t) => self.push_text(&t),
                    }
                    self.text.push_str(": ");
                    if let Some(frame) = self.value(&v, level, &mut markers)? {
                        stack.push(frame);
                    }
                }
                Step::Close(c) => {
                    if let Some(Frame::Seq { id, .. } | Frame::Map { id, .. }) = stack.pop() {
                        markers.remove(&id);
                    }
                    if self.indent {
                        self.newline(level.saturating_sub(1));
                    }
                    self.text.push(c);
                }
            }
        }
    }
}

/// A dict key once coerced.
enum KeyText<'py> {
    Str(Bound<'py, PyString>),
    Plain(String),
}

/// json.dumps(obj, indent=2 if indent else None, ensure_ascii=...).
fn json_dumps(py: Python<'_>, obj: &Bound<'_, PyAny>, indent: bool, ensure_ascii: bool) -> PyResult<String> {
    let mut out = JsonOut::new(py, indent, ensure_ascii, false);
    out.write(obj)?;
    Ok(out.text)
}

// ---------------------------------------------------------------------------
// The renderers (staticmethods in Python)
// ---------------------------------------------------------------------------

/// A tool call's arguments: a JSON string is parsed ({"raw_arguments": text}
/// when it is not JSON), anything else is returned as it is.
fn parse_arguments<'py>(args: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = args.py();
    let Ok(text) = args.cast::<PyString>() else { return Ok(args) };
    match json_loads(py, &text_of(text)) {
        Ok(parsed) => Ok(parsed),
        Err(e) if e.is_instance_of::<PyValueError>(py) || e.is_instance_of::<PyTypeError>(py) => {
            let raw = PyDict::new(py);
            raw.set_item("raw_arguments", &args)?;
            Ok(raw.into_any())
        }
        Err(e) => Err(e),
    }
}

/// _call_name_args: (name, arguments dict) of one native tool call.
fn call_name_args<'py>(tc: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = tc.py();
    let function = or_dict(get(&or_dict(tc.clone())?, "function")?)?;
    let args = parse_arguments(get(&function, "arguments")?)?;
    let name = or_str(get(&function, "name")?)?;
    let args = if args.is_instance_of::<PyDict>() { dict_of(&args)? } else { PyDict::new(py).into_any() };
    Ok((name, args))
}

/// {"type": name, **args}
fn action_of<'py>(name: &Bound<'py, PyAny>, args: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let action = PyDict::new(name.py());
    action.set_item("type", name)?;
    action.call_method1("update", (args,))?;
    Ok(action.into_any())
}

/// `str(x or "").strip()` as text.
fn stripped(x: Bound<'_, PyAny>) -> PyResult<String> {
    Ok(py_strip(&str_text(&or_str(x)?)?).to_string())
}

/// _render_tool_calls: a native assistant turn's tool_calls, readable.
fn render_tool_calls(calls: &Bound<'_, PyAny>, coder: bool) -> PyResult<String> {
    if coder {
        return render_coder_calls(calls);
    }
    let py = calls.py();
    let (mut thinking, mut memory, mut next_goal) = (String::new(), String::new(), String::new());
    let actions = PyList::empty(py);
    for tc in or_list(calls.clone())?.try_iter()? {
        let tc = tc?;
        let function = or_dict(get(&or_dict(tc)?, "function")?)?;
        let mut args = parse_arguments(get(&function, "arguments")?)?;
        if !args.is_instance_of::<PyDict>() {
            args = PyDict::new(py).into_any();
        }
        let args = dict_of(&args)?;
        let step_thinking = stripped(args.call_method1("pop", ("thinking", ""))?)?;
        let step_memory = stripped(args.call_method1("pop", ("memory", ""))?)?;
        let step_next_goal = stripped(args.call_method1("pop", ("next_goal", ""))?)?;
        if thinking.is_empty() {
            thinking = step_thinking;
        }
        if memory.is_empty() {
            memory = step_memory;
        }
        if next_goal.is_empty() {
            next_goal = step_next_goal;
        }
        let name = or_str(get(&function, "name")?)?;
        actions.append(action_of(&name, &args)?)?;
    }
    let rendered = PyDict::new(py);
    rendered.set_item("thinking", if thinking.is_empty() { "skipped" } else { thinking.as_str() })?;
    if !memory.is_empty() {
        rendered.set_item("memory", memory)?;
    }
    rendered.set_item("next_goal", next_goal)?;
    rendered.set_item("action", actions)?;
    json_dumps(py, rendered.as_any(), true, false)
}

/// _render_coder_calls: a Rust coder turn, its insight ({kind, content}), its
/// next_goal and its other calls as `action`, each only when the turn has it.
fn render_coder_calls(calls: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = calls.py();
    let mut insight: Option<Bound<'_, PyDict>> = None;
    let mut next_goal = String::new();
    let actions = PyList::empty(py);
    for tc in or_list(calls.clone())?.try_iter()? {
        let (name, args) = call_name_args(&tc?)?;
        if name.eq("insight")? && insight.is_none() {
            let note = PyDict::new(py);
            note.set_item("kind", get_or(&args, "kind", "")?)?;
            note.set_item("content", get_or(&args, "content", "")?)?;
            insight = Some(note);
        } else if name.eq("next_goal")? && next_goal.is_empty() {
            next_goal = stripped(get_or(&args, "value", "")?)?;
        } else {
            actions.append(action_of(&name, &args)?)?;
        }
    }
    let rendered = PyDict::new(py);
    if let Some(note) = insight {
        rendered.set_item("insight", note)?;
    }
    if !next_goal.is_empty() {
        rendered.set_item("next_goal", next_goal)?;
    }
    if !actions.is_empty() {
        rendered.set_item("action", actions)?;
    }
    json_dumps(py, rendered.as_any(), true, false)
}

/// _render_persisted_step: a saved assistant entry, readable.
fn render_persisted_step(entry: &Bound<'_, PyAny>, coder: bool) -> PyResult<String> {
    let py = entry.py();
    let parsed = entry.str().and_then(|text| json_loads(py, &text_of(&text)));
    let d = match parsed {
        Ok(d) => d,
        Err(e) => {
            caught(py, e)?;
            return str_text(&or_str(entry.clone())?);
        }
    };
    if d.is_instance_of::<PyDict>() {
        let calls = get(&d, "tool_calls")?;
        if calls.is_instance_of::<PyList>() && (d.contains("content")? || calls.is_truthy()?) {
            let content = str_text(&or_str(get(&d, "content")?)?)?;
            let tool_calls = d.get_item("tool_calls")?;
            let mut block = render_tool_calls(&tool_calls, coder)?;
            if coder && !tool_calls.is_truthy()? {
                block.clear();
            }
            return Ok(if !content.is_empty() && !block.is_empty() {
                format!("{content}\n{block}")
            } else if !content.is_empty() {
                content
            } else {
                block
            });
        }
    }
    str_text(&or_str(entry.clone())?)
}

/// _render_persisted_results: saved tool results, readable.
fn render_persisted_results(raw: &Bound<'_, PyAny>) -> PyResult<String> {
    let py = raw.py();
    let parsed = raw.str().and_then(|text| json_loads(py, &text_of(&text)));
    let data = match parsed {
        Ok(d) => d,
        Err(e) => {
            caught(py, e)?;
            return str_text(raw);
        }
    };
    if data.is_instance_of::<PyList>() {
        let mut every = true;
        for d in data.try_iter()? {
            let d = d?;
            let ok = d.is_instance_of::<PyDict>()
                && (d.contains("tool_call_id")? || get(&d, "role")?.eq("user")?);
            if !ok {
                every = false;
                break;
            }
        }
        if every {
            let mut parts = Vec::new();
            for d in data.try_iter()? {
                parts.push(str_text(&or_str(get(&d?, "content")?)?)?);
            }
            return Ok(parts.join("\n"));
        }
    }
    str_text(raw)
}

/// _content_text: a message's content as a str; a list of blocks joins its
/// dict blocks' text.
fn content_text<'py>(content: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = content.py();
    if content.is_instance_of::<PyList>() {
        let parts = PyList::empty(py);
        for block in content.try_iter()? {
            let block = block?;
            if block.is_instance_of::<PyDict>() {
                parts.append(get_or(&block, "text", "")?)?;
            }
        }
        return PyString::new(py, "\n").call_method1("join", (parts,));
    }
    if content.is_instance_of::<PyString>() {
        return Ok(content.clone());
    }
    Ok(content.str()?.into_any())
}

/// _shell_bridge_id: the id of a Coder bridge's insight call, when `d` (a
/// parsed step) is one.
fn shell_bridge_id<'py>(d: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = d.py();
    if !d.is_instance_of::<PyDict>() {
        return Ok(None);
    }
    let keys = py.get_type::<PySet>().call1((d.call_method0("keys")?,))?;
    if keys.ne(PySet::new(py, ["content", "tool_calls"])?)? {
        return Ok(None);
    }
    let calls = get(d, "tool_calls")?;
    if !calls.is_instance_of::<PyList>() || calls.len()? != 1 {
        return Ok(None);
    }
    let call = calls.get_item(0)?;
    if !call.is_instance_of::<PyDict>() {
        return Ok(None);
    }
    let call_id = call.call_method1("get", ("id",))?;
    let call_id = or_str(call_id)?.str()?;
    let name = get(&or_dict(get(&call, "function")?)?, "name")?;
    if name.eq("insight")? && text_of(&call_id).starts_with(SHELL_BRIDGE_ID_PREFIX) {
        return Ok(Some(call_id.into_any()));
    }
    Ok(None)
}

/// _is_terminal_note: True only for OUR synthetic terminal note.
fn is_terminal_note(entry: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = entry.py();
    let parsed = entry.str().and_then(|text| json_loads(py, &text_of(&text)));
    let d = match parsed {
        Ok(d) => d,
        Err(e) => {
            caught(py, e)?;
            return Ok(false);
        }
    };
    if !d.is_instance_of::<PyDict>() {
        return Ok(false);
    }
    if shell_bridge_id(&d)?.is_some() {
        return Ok(true);
    }
    let mut keys: Vec<String> = Vec::new();
    for key in d.cast::<PyDict>()?.keys() {
        keys.push(str_text(&key)?);
    }
    keys.sort();
    let goal = str_text(&get_or(&d, "next_goal", "")?)?;
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    // Main-agent bridge (both generations): pre-thinking {memory, next_goal}
    // and {thinking, memory, next_goal} (thinking = "not required"), also the
    // shape coder chats saved before the coder's native-tools bridge.
    if keys == ["memory", "next_goal"] || keys == ["memory", "next_goal", "thinking"] {
        return Ok(goal.starts_with("Previous run concluded."));
    }
    // Coder bridge of the old Python coder: memory folded into
    // next_goal ("memory: ... next_goal: ..."), no standalone memory key.
    if keys == ["next_goal", "thinking"] {
        return Ok(goal.starts_with("memory: ") && goal.contains("next_goal: Previous run concluded."));
    }
    Ok(false)
}

/// _shell_bridge as JSON: one `insight` call saying how request N ended.
fn shell_bridge_value(
    status: &Bound<'_, PyAny>,
    message: &Bound<'_, PyAny>,
    request_no: &Bound<'_, PyAny>,
) -> PyResult<serde_json::Value> {
    let n = py_int(&or_else(request_no.clone(), PyInt::new(request_no.py(), 0).into_any())?)?;
    let n_text = fmt_text(&n)?;
    let what = if n.is_truthy()? { format!("request {n_text}") } else { "the request".to_string() };
    let mut chars = what.chars();
    let what_cap = match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    };
    let (kind, text) = if status.eq("success")? {
        ("result", format!("{what_cap} is done. Continue from here."))
    } else if status.eq("error")? {
        let reason = if message.is_truthy()? { format!(": {}", fmt_text(message)?) } else { String::new() };
        ("failure", format!("{what_cap} ended with an error{reason}. Continue from here."))
    } else {
        ("result", format!("The agent stopped {what} before it finished. Continue from here."))
    };
    let arguments = dumps(&json!({"kind": kind, "content": text}));
    Ok(json!({
        "content": "",
        "tool_calls": [{
            "id": format!("{SHELL_BRIDGE_ID_PREFIX}{n_text}"),
            "type": "function",
            "function": {"name": "insight", "arguments": arguments},
        }],
    }))
}

/// _bridge_slot: what fills the bridge's empty slot once the next request is
/// here.
fn bridge_slot<'py>(
    bridge_entry: &Bound<'py, PyAny>,
    request: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = request.py();
    let found = bridge_entry
        .str()
        .and_then(|text| json_loads(py, &text_of(&text)))
        .and_then(|d| shell_bridge_id(&d));
    let call_id = match found {
        Ok(id) => id,
        Err(e) => {
            caught(py, e)?;
            None
        }
    };
    let Some(call_id) = call_id else { return Ok(request.clone()) };
    if !call_id.is_truthy()? {
        return Ok(request.clone());
    }
    let ok = PyDict::new(py);
    ok.set_item("tool_call_id", call_id)?;
    ok.set_item("content", "ok")?;
    let user = PyDict::new(py);
    user.set_item("role", "user")?;
    user.set_item("content", request)?;
    let slot = PyList::new(py, [ok, user])?;
    Ok(PyString::new(py, &json_dumps(py, slot.as_any(), false, false)?).into_any())
}

/// _render_messages: an exact messages payload as the human-readable log.
fn render_messages(
    messages: &Bound<'_, PyAny>,
    header_lines: Option<&Bound<'_, PyAny>>,
    final_response: Option<&Bound<'_, PyAny>>,
    coder: bool,
) -> PyResult<String> {
    let py = messages.py();
    let bar = "=".repeat(60);
    let mut out = Lines::new();
    let header = match header_lines {
        Some(h) if h.is_truthy()? => Some(h),
        _ => None,
    };
    if let Some(h) = header {
        for line in h.try_iter()? {
            out.obj(line?);
        }
        out.text(bar.as_str());
        out.text("");
    }
    let mut note_ids = PySet::empty(py)?;
    for m in or_list(messages.clone())?.try_iter()? {
        let m = m?;
        let role = upper(&get_or(&m, "role", "?")?.str()?)?;
        let mut content = str_text(&content_text(&get_or(&m, "content", "")?)?)?;
        if role == "SYSTEM" {
            out.text("=== SYSTEM PROMPT ===");
            out.text(content);
            out.text("");
            out.text(bar.as_str());
            out.text("");
            continue;
        }
        let calls = get(&m, "tool_calls")?;
        if coder && role == "ASSISTANT" {
            let ids = PySet::empty(py)?;
            let kinds = PyTuple::new(py, ["insight", "next_goal"])?;
            for tc in or_list(calls.clone())?.try_iter()? {
                let tc = tc?;
                let (name, _) = call_name_args(&tc)?;
                if kinds.contains(name)? {
                    ids.add(get(&or_dict(tc)?, "id")?)?;
                }
            }
            note_ids = ids;
        }
        if coder
            && role == "TOOL"
            && note_ids.contains(get(&m, "tool_call_id")?)?
            && py_strip(&content) == "ok"
        {
            continue;
        }
        if calls.is_truthy()? {
            let block = render_tool_calls(&calls, coder)?;
            content = if content.is_empty() { block } else { format!("{content}\n{block}") };
        }
        out.text(format!("--- {role} ---"));
        out.text(content);
        out.text("");
    }
    if let Some(f) = final_response {
        if f.is_truthy()? {
            out.text("--- ASSISTANT (final response) ---");
            out.text(render_persisted_step(f, coder)?);
            out.text("");
        }
    }
    Ok(out.join(py)? + "\n")
}

/// _title_from_task: the first non-empty line of the task, trimmed to a short
/// sidebar label.
fn title_from_task<'py>(task: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = task.py();
    let task = or_str(task.clone())?;
    if let Ok(s) = task.cast_exact::<PyString>() {
        if let Ok(full) = s.to_str() {
            let text = py_strip(full);
            if text.is_empty() {
                return Ok(PyString::new(py, "New chat").into_any());
            }
            let first = py_strip(py_splitlines(text).first().copied().unwrap_or(""));
            if first.chars().count() <= 60 {
                return Ok(PyString::new(py, first).into_any());
            }
            let head: String = first.chars().take(57).collect();
            return Ok(PyString::new(py, &format!("{}...", py_rstrip(&head))).into_any());
        }
    }
    // Anything but a plain str: Python's own methods, so it fails as it did.
    let text = task.call_method0("strip")?;
    if !text.is_truthy()? {
        return Ok(PyString::new(py, "New chat").into_any());
    }
    let first = text.call_method0("splitlines")?.get_item(0)?.call_method0("strip")?;
    if first.len()? <= 60 {
        return Ok(first);
    }
    let cut = py.import("builtins")?.getattr("slice")?.call1((py.None(), 57))?;
    first.get_item(cut)?.call_method0("rstrip")?.add("...")
}

/// _terminal_message: the clean "done message" for ANY ending.
fn terminal_message(status: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>) -> PyResult<String> {
    let message = fmt_text(&or_str(message.clone())?)?;
    let head = if status.eq("success")? {
        "Task completed: "
    } else if status.eq("error")? {
        "Agent terminated (error / provider): "
    } else {
        "Agent stopped before completing: "
    };
    let full = format!("{head}{message}");
    Ok(py_strip(&full).trim_end_matches(':').to_string())
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// Owns all conversation-memory persistence and optimization for the UI.
#[pyclass(frozen, module = "AutoCua.agent_conversation.service")]
pub struct ConversationService;

impl ConversationService {
    /// A class attribute (the migration's lock and its set of done
    /// destinations), shared by every instance as in Python.
    fn class_attr<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        py.get_type::<ConversationService>().getattr(name)
    }

    /// _move_tree on Rust paths.
    fn move_tree_at(py: Python<'_>, src: &Path, dst: &Path) -> PyResult<i64> {
        if dst.exists() {
            return Ok(0); // newer data wins; never clobber
        }
        let part = part_path(dst);
        let (s, d, p) = (src.to_path_buf(), dst.to_path_buf(), part.clone());
        let moved = py.detach(move || -> Result<(), MoveError> {
            if p.exists() {
                rmtree(&p); // stale attempt from a crash
            }
            copytree(&s, &p).map_err(MoveError::Tree)?;
            fs::rename(&p, &d).map_err(|e| MoveError::Replace(e, p.clone(), d.clone()))?;
            rmtree(&s);
            Ok(())
        });
        match moved {
            Ok(()) => Ok(1),
            Err(e) => {
                let err = match e {
                    MoveError::Tree(t) => tree_error(py, t),
                    MoveError::Replace(err, from, to) => os_error2(py, &err, &from, &to),
                    MoveError::Os(err, at) => os_error(py, &err, &at),
                };
                log_exception(py, "migrate chat folder %s", vec![py_path(py, src)?], &err);
                py.detach(move || rmtree(&part));
                Ok(0)
            }
        }
    }

    /// _move_file on Rust paths.
    fn move_file_at(py: Python<'_>, src: &Path, dst: &Path) -> PyResult<i64> {
        if dst.exists() {
            return Ok(0);
        }
        let Some(data) = read_bytes_logged(py, src, || py_path(py, src))? else {
            return Ok(0);
        };
        let part = part_path(dst);
        let (s, d, p) = (src.to_path_buf(), dst.to_path_buf(), part.clone());
        let moved = py.detach(move || -> Result<(), MoveError> {
            fs::write(&p, &data).map_err(|e| MoveError::Os(e, p.clone()))?;
            fs::rename(&p, &d).map_err(|e| MoveError::Replace(e, p.clone(), d.clone()))?;
            fs::remove_file(&s).map_err(|e| MoveError::Os(e, s.clone()))?;
            Ok(())
        });
        match moved {
            Ok(()) => Ok(1),
            Err(e) => {
                let err = match e {
                    MoveError::Tree(t) => tree_error(py, t),
                    MoveError::Replace(err, from, to) => os_error2(py, &err, &from, &to),
                    MoveError::Os(err, at) => os_error(py, &err, &at),
                };
                log_exception(py, "migrate %s", vec![py_path(py, src)?], &err);
                let _ = fs::remove_file(&part);
                Ok(0)
            }
        }
    }

    /// _read_json_dict on a Rust path.
    fn read_json_dict_at<'py>(py: Python<'py>, path: &Path) -> PyResult<Bound<'py, PyDict>> {
        let Some(raw) = read_bytes_logged(py, path, || py_path(py, path))? else {
            return Ok(PyDict::new(py));
        };
        if raw.is_empty() {
            return Ok(PyDict::new(py));
        }
        let parsed = match std::str::from_utf8(&raw) {
            Ok(text) => json_loads(py, text),
            Err(err) => Err(decode_failure(py, &raw, err)),
        };
        match parsed {
            Ok(data) => Ok(data.cast_into::<PyDict>().unwrap_or_else(|_| PyDict::new(py))),
            Err(e) => {
                let e = caught(py, e)?;
                log_exception(py, "parse %s", vec![py_path(py, path)?], &e);
                Ok(PyDict::new(py))
            }
        }
    }

    /// _merge_legacy_index on Rust paths.
    fn merge_legacy_index_at(py: Python<'_>, legacy: &Path, dest: &Path) -> PyResult<()> {
        let merged = Self::read_json_dict_at(py, dest)?;
        let mut added: i64 = 0;
        let parent = dest.parent().unwrap_or_else(|| Path::new(""));
        for (cid, entry) in Self::read_json_dict_at(py, legacy)?.iter() {
            if merged.contains(&cid)? || !parent.join(str_text(&cid)?).is_dir() {
                continue;
            }
            merged.set_item(&cid, &entry)?;
            added += 1;
        }
        if added > 0 {
            let part = part_path(dest);
            let written = (|| -> PyResult<()> {
                let text = json_dumps(py, merged.as_any(), true, true)?;
                fs::write(&part, text.as_bytes()).map_err(|e| os_error(py, &e, &part))?;
                // atomic: never a truncated index
                fs::rename(&part, dest).map_err(|e| os_error2(py, &e, &part, dest))?;
                Ok(())
            })();
            match written {
                Ok(()) => log(
                    py,
                    "info",
                    "merged %d legacy chat(s) into %s",
                    vec![PyInt::new(py, added).into_any(), py_path(py, dest)?],
                    None,
                ),
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "merge index.json", Vec::new(), &e);
                    return Ok(());
                }
            }
        }
        let _ = fs::remove_file(legacy);
        Ok(())
    }

    /// The body of _migrate_legacy's try block.
    fn migrate_body(&self, py: Python<'_>, dest: &Bound<'_, PyAny>) -> PyResult<()> {
        let legacy = self.legacy_root(py)?;
        let legacy_at = fspath(&legacy)?;
        if !legacy_at.is_dir() || legacy.call_method0("resolve")?.eq(dest.call_method0("resolve")?)? {
            return Ok(());
        }
        let dest_at = fspath(dest)?;
        let names = {
            let at = legacy_at.clone();
            py.detach(move || sorted_names(&at)).map_err(|e| os_error(py, &e, &legacy_at))?
        };
        let mut moved: i64 = 0;
        for name in names {
            if !safe_entry(&name) {
                continue;
            }
            let item = legacy_at.join(&name);
            if item.is_dir() && name.starts_with(LEGACY_DIR_PREFIX) {
                moved += Self::move_tree_at(py, &item, &dest_at.join(&name))?;
            } else if item.is_file() && LEGACY_FILES.contains(&name.as_str()) {
                let target = dest_at.join(&name);
                if name == "index.json" && target.exists() {
                    Self::merge_legacy_index_at(py, &item, &target)?;
                } else {
                    moved += Self::move_file_at(py, &item, &target)?;
                }
            }
        }
        // NEVER rmtree the legacy folder: in dev it IS this package's source
        // dir. rmdir succeeds only when it is genuinely empty, i.e. the
        // packaged case, where the folder held data and nothing else.
        let _ = fs::remove_dir(&legacy_at);
        if moved > 0 {
            log(
                py,
                "info",
                "migrated %d chat item(s): %s -> %s",
                vec![PyInt::new(py, moved).into_any(), legacy, dest.clone()],
                None,
            );
        }
        Ok(())
    }

    /// _touch_index with Rust options.
    #[allow(clippy::too_many_arguments)]
    fn touch<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        title: Option<&Bound<'py, PyAny>>,
        last_done_message: Option<&Bound<'py, PyAny>>,
        context_tokens: Option<&Bound<'py, PyAny>>,
        context_cap: Option<&Bound<'py, PyAny>>,
        run_pkg: Option<&Bound<'py, PyAny>>,
        agent_mode: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let index = self.read_index(py)?;
        let now = now_secs(py)?;
        let existing = index.get_item(session_id.str()?)?;
        let entry = match existing {
            Some(e) if e.is_truthy()? => e,
            _ => {
                let fresh = PyDict::new(py);
                fresh.set_item("title", py.None())?;
                fresh.set_item("created_at", &now)?;
                fresh.set_item("updated_at", &now)?;
                fresh.set_item("last_done_message", "")?;
                fresh.set_item("context_tokens", 0)?;
                fresh.set_item("context_cap", 0)?;
                fresh.into_any()
            }
        };
        if let Some(title) = title {
            if title.is_truthy()? && !get(&entry, "title")?.is_truthy()? {
                entry.set_item("title", title)?;
            }
        }
        if let Some(message) = last_done_message {
            entry.set_item("last_done_message", message)?;
        }
        let zero = PyInt::new(py, 0).into_any();
        if let Some(tokens) = context_tokens {
            entry.set_item("context_tokens", py_int(&or_else(tokens.clone(), zero.clone())?)?)?;
        }
        if let Some(cap) = context_cap {
            entry.set_item("context_cap", py_int(&or_else(cap.clone(), zero.clone())?)?)?;
        }
        if let Some(pkg) = run_pkg {
            if pkg.is_truthy()? {
                // Which agent package produced this chat's memory (mac /
                // windows / ios): lets a resume detect a mode switch.
                entry.set_item("run_pkg", pkg)?;
            }
        }
        if let Some(mode) = agent_mode {
            if mode.is_truthy()? {
                // Which AGENT MODE owns this chat ("computer" / "mobile" /
                // "coder"). Orthogonal to run_pkg: coder runs share the desktop
                // PLATFORM_PKG, so the pkg alone can't tell a coder chat from a
                // computer one; this field is what the per-chat mode lock
                // reads. Stamped at MINT time (start_or_resume) so even a chat
                // whose first run never finished is correctly tagged, then
                // re-stamped on every save.
                entry.set_item("agent_mode", mode)?;
            }
        }
        entry.set_item("updated_at", now)?;
        index.set_item(session_id.str()?, &entry)?;
        self.write_index(py, index.as_any())?;
        Ok(entry)
    }

    /// _build_history with Rust options.
    #[allow(clippy::too_many_arguments)]
    fn build<'py>(
        &self,
        py: Python<'py>,
        task: &Bound<'py, PyAny>,
        assistant_messages: &Bound<'py, PyAny>,
        tool_responses: &Bound<'py, PyAny>,
        status: &Bound<'py, PyAny>,
        message: &Bound<'py, PyAny>,
        prior_task: Option<&Bound<'py, PyAny>>,
        agent_mode: Option<&Bound<'py, PyAny>>,
        request_no: Option<&Bound<'py, PyAny>>,
        sub_agent_last_id: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        let list = py.get_type::<PyList>();
        let full_assistant = list.call1((or_list(assistant_messages.clone())?,))?.cast_into::<PyList>()?;
        let mut tools = list.call1((or_list(tool_responses.clone())?,))?.cast_into::<PyList>()?;
        // Force exact 1:1 alignment defensively.
        let steps = full_assistant.len();
        if tools.len() < steps {
            while tools.len() < steps {
                tools.append(py.None())?;
            }
        } else {
            tools = tools.get_slice(0, steps);
        }
        let done_message = terminal_message(status, message)?;
        let coder = match agent_mode {
            Some(mode) => mode.eq("coder")?,
            None => false,
        };
        // A zero-step run (stopped/failed before its first step) saves its seed
        // back unchanged: the tail is then ALREADY a terminal note with an
        // empty tool slot. Coder chats RECORD that run instead of erasing it:
        // fill the old bridge's slot with this request's numbered marker (the
        // same string a normal resume writes), then let the new terminal note
        // below cap it, so the memory keeps "request N arrived, stopped/failed"
        // in chronological order and the previous conclusion survives.
        // Elsewhere (main agent, or no request number to write) stacking would
        // accrete dangling notes with nothing between them: REPLACE the tail so
        // it stays singular and carries the NEWEST ending. The check is
        // STRUCTURAL (exact key sets + the canonical sentence), never a
        // substring: a real model step that merely echoes the bridge phrase it
        // saw in context must never be eaten.
        if steps > 0 {
            let last = steps - 1;
            if tools.get_item(last)?.is_none() && is_terminal_note(&full_assistant.get_item(last)?)? {
                let numbered = match request_no {
                    Some(n) if coder && n.is_truthy()? => {
                        let task_text = str_text(&or_str(task.clone())?)?;
                        if py_strip(&task_text).is_empty() {
                            None
                        } else {
                            Some(n)
                        }
                    }
                    _ => None,
                };
                if let Some(n) = numbered {
                    let opening = fmt_text(&py_int(n)?)?;
                    let body = fmt_text(task)?;
                    let closing = fmt_text(&py_int(n)?)?;
                    let request = format!("<user_request={opening}>\n{body}\n</user_request={closing}>");
                    let request = PyString::new(py, &request).into_any();
                    let slot = bridge_slot(&full_assistant.get_item(last)?, &request)?;
                    tools.set_item(last, slot)?;
                } else {
                    full_assistant.del_item(last)?;
                    tools.del_item(last)?;
                }
            }
        }
        // Cap with a terminal note so a resumed run (and the reader) always sees
        // how the previous run ended, on EVERY ending (done / stop / error). On
        // resume it becomes the most-recent step, so the agent's builder replays
        // every real step's tool result beneath it.
        let note = if coder {
            // Coder bridge: one insight, how request N ended; the next
            // request fills its empty slot after the insight's `ok`.
            let none = py.None().into_bound(py);
            shell_bridge_value(status, message, request_no.unwrap_or(&none))?
        } else {
            json!({
                "thinking": "not required", // matches the step schema's skip convention
                "memory": done_message,
                "next_goal": MAIN_BRIDGE_GOAL,
            })
        };
        full_assistant.append(dumps_pretty(&note))?;
        tools.append(py.None())?;
        let payload = PyDict::new(py);
        payload.set_item("version", 1)?;
        let task_value = match prior_task {
            Some(p) if p.is_truthy()? => p.clone(),
            _ => task.clone(),
        };
        payload.set_item("task", task_value)?;
        payload.set_item("assistant_messages", full_assistant)?;
        payload.set_item("tool_responses", tools)?;
        payload.set_item("final_status", status)?;
        payload.set_item("final_message", message)?;
        payload.set_item("done_message", done_message)?;
        // Desktop main agents only: the last sub-agent id handed out, so the ids
        // in <sub_agents> keep counting on the next request of this chat.
        if let Some(last_id) = sub_agent_last_id {
            payload.set_item("sub_agent_last_id", py_int(last_id)?)?;
        }
        Ok(payload)
    }

    /// The body of save_run's try block.
    #[allow(clippy::too_many_arguments)]
    fn save_run_body<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        assistant_messages: &Bound<'py, PyAny>,
        tool_responses: &Bound<'py, PyAny>,
        status: &Bound<'py, PyAny>,
        message: &Bound<'py, PyAny>,
        task: &Bound<'py, PyAny>,
        last_messages: Option<&Bound<'py, PyAny>>,
        context_tokens: Option<&Bound<'py, PyAny>>,
        context_cap: Option<&Bound<'py, PyAny>>,
        run_pkg: Option<&Bound<'py, PyAny>>,
        agent_mode: Option<&Bound<'py, PyAny>>,
        request_no: Option<&Bound<'py, PyAny>>,
        sub_agent_last_id: Option<&Bound<'py, PyAny>>,
        recording: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let existing = self.read_session(py, session_id)?;
        let prior_task = match &existing {
            Some(d) => Some(get(d, "task")?),
            None => None,
        };
        // A run that never built its agent reports no counter: keep the saved one.
        let mut last_id = sub_agent_last_id.cloned();
        if last_id.is_none() {
            if let Some(d) = &existing {
                let saved = get(d, "sub_agent_last_id")?;
                if !saved.is_none() {
                    last_id = Some(saved);
                }
            }
        }
        let last_id = match last_id {
            None => None,
            Some(v) => match py_int(&v) {
                Ok(n) => Some(n),
                // a damaged value must never cost the chat its save
                Err(e)
                    if e.is_instance_of::<PyTypeError>(py)
                        || e.is_instance_of::<PyValueError>(py)
                        || e.is_instance_of::<PyOverflowError>(py) =>
                {
                    None
                }
                Err(e) => return Err(e),
            },
        };
        let payload = self.build(
            py,
            task,
            assistant_messages,
            tool_responses,
            status,
            message,
            prior_task.as_ref(),
            agent_mode,
            request_no,
            last_id.as_ref(),
        )?;
        self.write_session(py, session_id, payload.as_any())?;
        let done_message = payload.as_any().get_item("done_message")?;
        // Reopen-view transcript: this run's request + how it ended (+ its video).
        self.append_exchange(py, session_id, task, &done_message, status, recording)?;

        // TRUE memory log (debug): render the exact payload the model received.
        if let Some(messages) = last_messages {
            if messages.is_truthy()? {
                let header = PyList::new(
                    py,
                    [
                        MEMORY_LOG_TITLE.to_string(),
                        format!("Session: {}", fmt_text(session_id)?),
                        format!("Task: {}", fmt_text(&get_or(payload.as_any(), "task", "")?)?),
                        format!("Final status: {}", fmt_text(status)?),
                        format!("Done: {}", fmt_text(&done_message)?),
                    ],
                )?;
                // The latest step's response is generated AFTER last_messages
                // was built, so append it as the final assistant turn (like
                // main.py).
                let final_response = if assistant_messages.is_truthy()? {
                    assistant_messages.get_item(-1)?
                } else {
                    py.None().into_bound(py)
                };
                let coder = match agent_mode {
                    Some(mode) => mode.eq("coder")?,
                    None => false,
                };
                let text = render_messages(messages, Some(header.as_any()), Some(&final_response), coder)?;
                let log_file = self.memory_log_file(py, session_id)?;
                self.write_text_at(py, &log_file, &text)?;
            }
        }

        let title_source = match &prior_task {
            Some(p) if p.is_truthy()? => p.clone(),
            _ => task.clone(),
        };
        let title = title_from_task(&title_source)?; // no-op if already set
        self.touch(
            py,
            session_id,
            Some(&title),
            Some(&done_message),
            context_tokens, // latest context size for the memory bar
            context_cap,    // fixed 300k memory budget (MEMORY_CAP)
            run_pkg,        // which agent produced this memory
            agent_mode,     // which MODE owns the chat (per-chat lock)
        )?;
        Ok(done_message)
    }

    /// _write_text with the text already Rust's.
    fn write_text_at(&self, py: Python<'_>, path: &Bound<'_, PyAny>, text: &str) -> PyResult<()> {
        // path.parent.mkdir(...): a path that is not a Path fails right there.
        let written = path.getattr("parent").and_then(|_| fspath(path)).and_then(|at| {
            mkdir_parent(py, &at)?;
            write_text_file(py, &at, text)
        });
        if let Err(e) = written {
            let e = caught(py, e)?;
            log_exception(py, "write text %s", vec![path.clone()], &e);
        }
        Ok(())
    }

    /// The body of delete_session's try block.
    fn delete_body(&self, py: Python<'_>, session_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        let dir = self.session_dir(py, session_id)?;
        let at = fspath(&dir)?;
        if at.exists() {
            py.detach(move || rmtree(&at));
        }
        let index = self.read_index(py)?;
        if index.contains(session_id.str()?)? {
            index.del_item(session_id.str()?)?;
            self.write_index(py, index.as_any())?;
        }
        Ok(true)
    }
}

#[pymethods]
impl ConversationService {
    #[new]
    fn new() -> Self {
        ConversationService
    }

    /// Chat ids are minted as chat_<hex ms>, but they arrive from the CLIENT
    /// (start_or_resume's request body, DELETE /api/chats/<id>) and are joined
    /// into a path that delete_session() removes. Now that root() sits in the
    /// user's own data folder rather than inside the package, a traversal
    /// would be far more destructive, so ids are validated before the path is
    /// built.
    #[classattr]
    #[pyo3(name = "_SAFE_ID")]
    fn safe_id_attr(py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(py.import("re")?.call_method1("compile", (SAFE_ID_PATTERN,))?.unbind())
    }

    /// The legacy files the migration moves.
    #[classattr]
    #[pyo3(name = "_LEGACY_FILES")]
    fn legacy_files_attr() -> (&'static str, &'static str) {
        (LEGACY_FILES[0], LEGACY_FILES[1])
    }

    /// The prefix of the legacy chat folders the migration moves.
    #[classattr]
    #[pyo3(name = "_LEGACY_DIR_PREFIX")]
    fn legacy_dir_prefix_attr() -> &'static str {
        LEGACY_DIR_PREFIX
    }

    /// The names the migration may touch at all.
    #[classattr]
    #[pyo3(name = "_SAFE_ENTRY")]
    fn safe_entry_attr(py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(py.import("re")?.call_method1("compile", (SAFE_ENTRY_PATTERN,))?.unbind())
    }

    /// One migration at a time in this process.
    #[classattr]
    #[pyo3(name = "_migrate_lock")]
    fn migrate_lock_attr(py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(py.import("threading")?.call_method0("Lock")?.unbind())
    }

    /// The destinations already migrated in this process, keyed by
    /// destination so tests can repoint the env var.
    #[classattr]
    #[pyo3(name = "_migrated_dests")]
    fn migrated_dests_attr(py: Python<'_>) -> PyResult<Py<PyAny>> {
        Ok(PySet::empty(py)?.into_any().unbind())
    }

    // ── storage paths ──────────────────────────────────────────────────────

    /// Root folder holding index.json + one sub-folder per session.
    ///
    /// Lives in AutoCua_data/, OUTSIDE the install folder, so uninstalling the
    /// app no longer deletes the user's chats. The first call also migrates any
    /// chats left behind in this package's own directory by an older build.
    fn root<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let base = module_attr(py, "data_root")?.call0()?;
        let d = module_attr(py, "_ensure")?.call1((base.div("agent_conversation")?,))?;
        self.migrate_legacy(py, &d)?;
        Ok(d)
    }

    #[pyo3(name = "_index_file")]
    fn index_file<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.root(py)?.div("index.json")
    }

    #[pyo3(name = "_is_safe_id")]
    fn is_safe_id(&self, session_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(safe_id(&str_text(session_id)?))
    }

    #[pyo3(name = "_session_dir")]
    fn session_dir<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let sid = session_id.str()?;
        if !safe_id(&text_of(&sid)) {
            return Err(PyValueError::new_err(format!("unsafe session id: {}", text_of(&sid.repr()?))));
        }
        self.root(py)?.div(sid)
    }

    #[pyo3(name = "_session_file")]
    fn session_file<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.session_dir(py, session_id)?.div("conversation.json")
    }

    /// The TRUE, human-readable memory log (exact payload sent to the model).
    #[pyo3(name = "_memory_log_file")]
    fn memory_log_file<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.session_dir(py, session_id)?.div("memory_log.txt")
    }

    /// The reopen-view transcript: one {task, done_message, status} per run
    /// ending, appended by save_run, separate from conversation.json (which is
    /// REWRITTEN each run) so the full request/outcome history survives. A
    /// recorded run's row also names its video (`recording`).
    #[pyo3(name = "_exchanges_file")]
    fn exchanges_file<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.session_dir(py, session_id)?.div("exchanges.json")
    }

    // ── one-time migration: chats out of the install folder ────────────────
    // Old layout, from this service's previous root():
    //     packaged : <exe dir>/AutoCua/agent_conversation/
    //     dev      : <repo>/AutoCua/agent_conversation/   <- THIS package's dir
    // In dev the package's own source sits in that very folder, so the
    // migration is an ALLOWLIST, never a denylist: only index.json,
    // settings.json and chat_* directories are ever touched. No file added to
    // the package later can be moved by accident.

    /// Where root() used to point before the move to AutoCua_data/.
    #[pyo3(name = "_legacy_root")]
    fn legacy_root<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        module_attr(py, "install_dir")?.call0()?.div("AutoCua")?.div("agent_conversation")
    }

    /// Path.read_bytes, which (unlike the compiled build's patched builtin
    /// open()) always reads this user's file. None, logged, on failure.
    #[staticmethod]
    #[pyo3(name = "_read_bytes")]
    fn read_bytes<'py>(py: Python<'py>, p: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let at = match p.getattr("read_bytes").and_then(|_| fspath(p)) {
            Ok(at) => at,
            Err(e) => {
                let e = caught(py, e)?;
                log_exception(py, "read %s", vec![p.clone()], &e);
                return Ok(None);
            }
        };
        Ok(read_bytes_logged(py, &at, || Ok(p.clone()))?.map(|data| PyBytes::new(py, &data)))
    }

    /// Move one chat folder, crash- AND volume-safe.
    ///
    /// shutil.move degrades to copy+delete across volumes (the install drive is
    /// often not the home drive), so a crash mid-copy would leave a
    /// HALF-WRITTEN dst that the next run would accept as 'already migrated'.
    /// Stage into '<dst>.part' and rename into place: the rename is atomic
    /// within the destination folder, so dst either does not exist or is
    /// complete. The source is removed only after the rename succeeds.
    #[pyo3(name = "_move_tree")]
    fn move_tree(&self, py: Python<'_>, src: &Bound<'_, PyAny>, dst: &Bound<'_, PyAny>) -> PyResult<i64> {
        // dst is a Path (dst.exists(), dst.with_name()); src goes in as str(src).
        dst.getattr("exists")?;
        dst.getattr("with_name")?;
        Self::move_tree_at(py, &PathBuf::from(str_text(src)?), &fspath(dst)?)
    }

    #[pyo3(name = "_move_file")]
    fn move_file(&self, py: Python<'_>, src: &Bound<'_, PyAny>, dst: &Bound<'_, PyAny>) -> PyResult<i64> {
        dst.getattr("exists")?;
        let dst_at = fspath(dst)?;
        if dst_at.exists() {
            return Ok(0);
        }
        if src.getattr("read_bytes").is_err() {
            // _read_bytes logs the AttributeError and gives None.
            Self::read_bytes(py, src)?;
            return Ok(0);
        }
        dst.getattr("with_name")?;
        Self::move_file_at(py, &fspath(src)?, &dst_at)
    }

    #[pyo3(name = "_read_json_dict")]
    fn read_json_dict<'py>(&self, py: Python<'py>, p: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
        if p.getattr("read_bytes").is_err() {
            // _read_bytes logs the AttributeError and gives None.
            Self::read_bytes(py, p)?;
            return Ok(PyDict::new(py));
        }
        Self::read_json_dict_at(py, &fspath(p)?)
    }

    /// Both index.json exist: reachable after an interrupted migration, or when
    /// a user ran an older build again after migrating. The NEW file always
    /// wins per chat id; a legacy row is adopted only when its chat folder is
    /// actually present now, so the sidebar never shows a ghost row pointing at
    /// a chat that never made it across.
    #[pyo3(name = "_merge_legacy_index")]
    fn merge_legacy_index(&self, py: Python<'_>, legacy: &Bound<'_, PyAny>, dest: &Bound<'_, PyAny>) -> PyResult<()> {
        let paths = dest.getattr("read_bytes").is_ok()
            && dest.getattr("parent").is_ok()
            && legacy.getattr("read_bytes").is_ok();
        if paths {
            return Self::merge_legacy_index_at(py, &fspath(legacy)?, &fspath(dest)?);
        }
        // Not Paths: what the Python does with them, step by step. A str has
        // no read_bytes (logged, {}), and no .parent to test a row against.
        let merged = self.read_json_dict(py, dest)?;
        for (cid, _) in self.read_json_dict(py, legacy)?.iter() {
            if !merged.contains(&cid)? {
                dest.getattr("parent")?;
            }
        }
        if legacy.getattr("unlink").is_ok() {
            let _ = fs::remove_file(fspath(legacy)?);
        }
        Ok(())
    }

    /// Move pre-AutoCua_data chats into `dest` exactly once.
    ///
    /// WHERE IT RUNS: lazily, on the first root() call in each process. That
    /// covers EVERY entry point (the GUI, the agents' child processes, a bare
    /// import of this module) with no ordering requirement on app.py, and it
    /// only runs when chat data is actually touched.
    ///
    /// IDEMPOTENT + CRASH-SAFE BY CONSTRUCTION, with NO on-disk 'done' marker:
    /// every item moves only when the destination does not exist, so a re-run
    /// is a no-op, a crash resumes on the next run, and a user who restores an
    /// old folder months later still gets it picked up. A marker would
    /// introduce state that can disagree with the filesystem; the in-process
    /// set is only a fast path, never a correctness gate.
    ///
    /// CONCURRENCY: the lock covers threads (Flask serves the chat routes from
    /// worker threads). Across processes, skip-if-exists plus the broad
    /// excepts make a double run harmless.
    #[pyo3(name = "_migrate_legacy")]
    fn migrate_legacy(&self, py: Python<'_>, dest: &Bound<'_, PyAny>) -> PyResult<()> {
        let key = dest.str()?;
        let done = Self::class_attr(py, "_migrated_dests")?;
        if done.contains(&key)? {
            return Ok(());
        }
        // threading.Lock.acquire waits without the GIL, so a thread holding the
        // lock while it calls into Python can never deadlock with a waiter.
        let lock = Self::class_attr(py, "_migrate_lock")?;
        lock.call_method0("acquire")?;
        let outcome = (|| -> PyResult<()> {
            if done.contains(&key)? {
                return Ok(());
            }
            let body = match self.migrate_body(py, dest) {
                Ok(()) => Ok(()),
                Err(e) => caught(py, e).map(|e| {
                    log_exception(py, "migrate legacy conversation data", Vec::new(), &e);
                }),
            };
            // Set even on failure so we don't re-scan on every root() call.
            // The next app start retries from scratch.
            done.call_method1("add", (&key,))?;
            body
        })();
        let released = lock.call_method0("release");
        outcome?;
        released?;
        Ok(())
    }

    // ── index.json (session -> display meta) ───────────────────────────────

    #[pyo3(name = "_read_index")]
    fn read_index<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let f = self.index_file(py)?;
        let at = fspath(&f)?;
        if at.exists() {
            match read_text_file(py, &at).and_then(|text| json_loads(py, &text)) {
                Ok(data) => {
                    if let Ok(dict) = data.cast_into::<PyDict>() {
                        return Ok(dict);
                    }
                }
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "read index.json", Vec::new(), &e);
                }
            }
        }
        Ok(PyDict::new(py))
    }

    #[pyo3(name = "_write_index")]
    fn write_index(&self, py: Python<'_>, index: &Bound<'_, PyAny>) -> PyResult<()> {
        let f = self.index_file(py)?;
        let written = fspath(&f).and_then(|at| dump_json_file(py, &at, index, true));
        if let Err(e) = written {
            let e = caught(py, e)?;
            log_exception(py, "write index.json", Vec::new(), &e);
        }
        Ok(())
    }

    /// Create-or-update one index entry. created_at + title are set ONCE
    /// (title never overwritten, so the original objective stays the label);
    /// updated_at + last_done_message + context_tokens/context_cap refresh
    /// whenever provided.
    ///
    /// context_tokens is the memory bar's LATEST context size (not a sum) and
    /// context_cap is the fixed memory budget the bar fills toward
    /// (MemoryTracker MEMORY_CAP = 300k, NOT the model context window):
    /// together they let a reopened chat restore the bar to where memory was.
    #[pyo3(
        name = "_touch_index",
        signature = (session_id, *, title=None, last_done_message=None, context_tokens=None,
                     context_cap=None, run_pkg=None, agent_mode=None)
    )]
    #[allow(clippy::too_many_arguments)]
    fn touch_index<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        title: Option<Bound<'py, PyAny>>,
        last_done_message: Option<Bound<'py, PyAny>>,
        context_tokens: Option<Bound<'py, PyAny>>,
        context_cap: Option<Bound<'py, PyAny>>,
        run_pkg: Option<Bound<'py, PyAny>>,
        agent_mode: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.touch(
            py,
            session_id,
            title.as_ref(),
            last_done_message.as_ref(),
            context_tokens.as_ref(),
            context_cap.as_ref(),
            run_pkg.as_ref(),
            agent_mode.as_ref(),
        )
    }

    // ── per-session conversation.json ──────────────────────────────────────

    /// The stored optimized history dict, or None on missing/corrupt (so the
    /// caller can resume the SAME id with a fresh agent context).
    #[pyo3(name = "_read_session")]
    fn read_session<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let f = self.session_file(py, session_id)?;
        let at = fspath(&f)?;
        if at.exists() {
            match read_text_file(py, &at).and_then(|text| json_loads(py, &text)) {
                Ok(data) => {
                    if data.is_instance_of::<PyDict>() {
                        let history = get(&data, "history")?;
                        if history.is_instance_of::<PyDict>() {
                            return Ok(Some(history));
                        }
                    }
                }
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "read session %s", vec![session_id.clone()], &e);
                }
            }
        }
        Ok(None)
    }

    #[pyo3(name = "_write_session")]
    fn write_session(
        &self,
        py: Python<'_>,
        session_id: &Bound<'_, PyAny>,
        history: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let f = self.session_file(py, session_id)?;
        let written = (|| -> PyResult<()> {
            let at = fspath(&f)?;
            let payload = PyDict::new(py);
            payload.set_item("session_id", session_id.str()?)?;
            payload.set_item("history", history)?;
            dump_json_file(py, &at, payload.as_any(), true)
        })();
        if let Err(e) = written {
            let e = caught(py, e)?;
            log_exception(py, "write session %s", vec![session_id.clone()], &e);
        }
        Ok(())
    }

    // ── per-session exchanges.json (reopen-view transcript) ────────────────

    #[pyo3(name = "_read_exchanges")]
    fn read_exchanges<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let f = self.exchanges_file(py, session_id)?;
        let at = fspath(&f)?;
        if at.exists() {
            match read_text_file(py, &at).and_then(|text| json_loads(py, &text)) {
                Ok(data) => {
                    if data.is_instance_of::<PyList>() {
                        return Ok(data);
                    }
                }
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "read exchanges %s", vec![session_id.clone()], &e);
                }
            }
        }
        Ok(PyList::empty(py).into_any())
    }

    /// Append this run's (user request, terminal message) pair. Every run
    /// ending records one (completion, user stop, error alike) so a reopened
    /// chat can replay '1. request / outcome' for its whole life. `recording`
    /// is the run's video folder in AutoCua_data/recordings, if it was
    /// recorded: the reopened chat shows that video again.
    #[pyo3(name = "_append_exchange", signature = (session_id, task, done_message, status, recording=None))]
    fn append_exchange(
        &self,
        py: Python<'_>,
        session_id: &Bound<'_, PyAny>,
        task: &Bound<'_, PyAny>,
        done_message: &Bound<'_, PyAny>,
        status: &Bound<'_, PyAny>,
        recording: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let items = self.read_exchanges(py, session_id)?;
        let item = PyDict::new(py);
        item.set_item("task", or_str(task.clone())?.str()?)?;
        item.set_item("done_message", or_str(done_message.clone())?.str()?)?;
        item.set_item("status", or_str(status.clone())?.str()?)?;
        item.set_item("at", now_secs(py)?)?;
        if let Some(rec) = recording {
            if rec.is_truthy()? {
                item.set_item("recording", rec.str()?)?;
            }
        }
        items.call_method1("append", (item,))?;
        let f = self.exchanges_file(py, session_id)?;
        let written = fspath(&f).and_then(|at| dump_json_file(py, &at, &items, false));
        if let Err(e) = written {
            let e = caught(py, e)?;
            log_exception(py, "write exchanges %s", vec![session_id.clone()], &e);
        }
        Ok(())
    }

    // ── helpers ────────────────────────────────────────────────────────────

    /// Filesystem-safe persistent chat id (distinct from app.py's per-run
    /// guard token): chat_<hex ms>.
    #[pyo3(name = "_new_id")]
    fn new_id(&self, py: Python<'_>) -> PyResult<String> {
        let millis = py_int(&py_time(py)?.mul(1000)?)?;
        let hex = py.import("builtins")?.getattr("format")?.call1((millis, "x"))?;
        Ok(format!("chat_{}", str_text(&hex)?))
    }

    /// First non-empty line of the task, trimmed to a short sidebar label.
    #[pyo3(name = "_title_from_task")]
    fn title_from_task_py<'py>(&self, task: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        title_from_task(task)
    }

    /// Clean 'done message' for ANY ending, so an interrupted / failed /
    /// provider-killed run still records a sensible conclusion in memory.
    #[pyo3(name = "_terminal_message")]
    fn terminal_message_py(&self, status: &Bound<'_, PyAny>, message: &Bound<'_, PyAny>) -> PyResult<String> {
        terminal_message(status, message)
    }

    /// True only for OUR synthetic terminal note (the resume bridge), any
    /// generation. Structural (exact key sets + the canonical sentence), so a
    /// real step (native {content, tool_calls} or 4-key main-agent JSON) that
    /// merely echoes the bridge phrase it saw in context can never match.
    #[staticmethod]
    #[pyo3(name = "_is_terminal_note")]
    fn is_terminal_note_py(entry: &Bound<'_, PyAny>) -> PyResult<bool> {
        is_terminal_note(entry)
    }

    /// The id of a Coder bridge's insight call, when `d` (a parsed step)
    /// is one: a coder step with exactly one `insight` call whose id starts
    /// with SHELL_BRIDGE_ID_PREFIX.
    #[staticmethod]
    #[pyo3(name = "_shell_bridge_id")]
    fn shell_bridge_id_py<'py>(d: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
        shell_bridge_id(d)
    }

    /// A Coder chat's bridge: one `insight` call (the coder's own kind of
    /// step) saying how request N ended, so the next request continues from
    /// it. No thinking: the coder has none.
    #[staticmethod]
    #[pyo3(name = "_shell_bridge")]
    fn shell_bridge_py<'py>(
        py: Python<'py>,
        status: &Bound<'py, PyAny>,
        message: &Bound<'py, PyAny>,
        request_no: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value = shell_bridge_value(status, message, request_no)?;
        pythonize::pythonize(py, &value).map_err(Into::into)
    }

    /// The bridge's slot once the next request is here: a Coder bridge is
    /// an insight call, so its `ok` comes first and the request follows as a
    /// user turn; an older bridge is plain text, so the request alone.
    #[staticmethod]
    #[pyo3(name = "_bridge_slot")]
    fn bridge_slot_py<'py>(
        bridge_entry: &Bound<'py, PyAny>,
        request: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        bridge_slot(bridge_entry, request)
    }

    /// Build the saved conversation = the agent's FULL per-step memory
    /// (thinking / eval / decision / memory / next_goal / action all preserved,
    /// tool results as-is), capped with a terminal note recording how the run
    /// ended.
    ///
    /// The FULL steps are stored, NOT a trimmed copy, so the debug download is
    /// the true, human-readable agent memory. Resume token-efficiency is
    /// unaffected: the agent's OWN message builder trims older steps to
    /// decision/memory/next_goal when it sends them to the LLM, so storing full
    /// here costs only a little disk and loses nothing for debugging.
    /// prior_task preserves the ORIGINAL objective across resumes.
    #[pyo3(
        name = "_build_history",
        signature = (task, assistant_messages, tool_responses, status, message, prior_task=None,
                     agent_mode=None, request_no=None, sub_agent_last_id=None)
    )]
    #[allow(clippy::too_many_arguments)]
    fn build_history<'py>(
        &self,
        py: Python<'py>,
        task: &Bound<'py, PyAny>,
        assistant_messages: &Bound<'py, PyAny>,
        tool_responses: &Bound<'py, PyAny>,
        status: &Bound<'py, PyAny>,
        message: &Bound<'py, PyAny>,
        prior_task: Option<Bound<'py, PyAny>>,
        agent_mode: Option<Bound<'py, PyAny>>,
        request_no: Option<Bound<'py, PyAny>>,
        sub_agent_last_id: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.build(
            py,
            task,
            assistant_messages,
            tool_responses,
            status,
            message,
            prior_task.as_ref(),
            agent_mode.as_ref(),
            request_no.as_ref(),
            sub_agent_last_id.as_ref(),
        )
    }

    // ── public API used by app.py ──────────────────────────────────────────

    /// Resolve the chat session for an incoming run.
    ///
    /// Returns (session_id, prior_history):
    ///   - existing session (folder present): CONTINUATION, load its saved
    ///     optimized history (None if missing/corrupt, so a fresh agent
    ///     context on the SAME id).
    ///   - otherwise: fresh start, mint a new id and seed its index entry so
    ///     the sidebar shows it immediately. prior_history is None.
    ///
    /// agent_mode ("computer"/"mobile"/"coder") stamps a freshly MINTED chat's
    /// index row so the per-chat mode lock is correct from second one, even if
    /// the run later dies before save_run ever re-stamps it.
    #[pyo3(signature = (req_session_id, task, agent_mode=None))]
    fn start_or_resume<'py>(
        &self,
        py: Python<'py>,
        req_session_id: &Bound<'py, PyAny>,
        task: &Bound<'py, PyAny>,
        agent_mode: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyTuple>> {
        // A malformed id (stale/corrupt client state) is treated as "no such
        // session" and falls through to a fresh start, exactly as it did before
        // _session_dir began rejecting unsafe ids: starting a run must never
        // fail just because the client sent a junk id.
        if req_session_id.is_truthy()?
            && req_session_id.ne("new")?
            && self.is_safe_id(req_session_id)?
            && fspath(&self.session_dir(py, req_session_id)?)?.exists()
        {
            let sid = req_session_id.str()?.into_any();
            let prior = match self.read_session(py, &sid)? {
                Some(history) => history,
                None => py.None().into_bound(py),
            };
            return PyTuple::new(py, [sid, prior]);
        }
        let sid = PyString::new(py, &self.new_id(py)?).into_any();
        let title = title_from_task(task)?;
        self.touch(py, &sid, Some(&title), None, None, None, None, agent_mode.as_ref())?;
        PyTuple::new(py, [sid, py.None().into_bound(py)])
    }

    /// Persist a finished run + refresh index meta. Writes TWO things:
    ///   - conversation.json: the lean resume seed (assistant/tool turns).
    ///   - memory_log.txt: the TRUE debug memory, the exact final payload sent
    ///     to the model (system + interleaved history + the live user message
    ///     that re-injects user_request/todo/scratchpad/element_tree).
    ///
    /// Called for EVERY ending (completion, user stop, error, provider kill).
    /// Returns the done message, or None (logged) when anything failed.
    #[pyo3(signature = (session_id, assistant_messages, tool_responses, status, message, task,
                        last_messages=None, context_tokens=None, context_cap=None, run_pkg=None,
                        agent_mode=None, request_no=None, sub_agent_last_id=None, recording=None))]
    #[allow(clippy::too_many_arguments)]
    fn save_run<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        assistant_messages: &Bound<'py, PyAny>,
        tool_responses: &Bound<'py, PyAny>,
        status: &Bound<'py, PyAny>,
        message: &Bound<'py, PyAny>,
        task: &Bound<'py, PyAny>,
        last_messages: Option<Bound<'py, PyAny>>,
        context_tokens: Option<Bound<'py, PyAny>>,
        context_cap: Option<Bound<'py, PyAny>>,
        run_pkg: Option<Bound<'py, PyAny>>,
        agent_mode: Option<Bound<'py, PyAny>>,
        request_no: Option<Bound<'py, PyAny>>,
        sub_agent_last_id: Option<Bound<'py, PyAny>>,
        recording: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let saved = self.save_run_body(
            py,
            session_id,
            assistant_messages,
            tool_responses,
            status,
            message,
            task,
            last_messages.as_ref(),
            context_tokens.as_ref(),
            context_cap.as_ref(),
            run_pkg.as_ref(),
            agent_mode.as_ref(),
            request_no.as_ref(),
            sub_agent_last_id.as_ref(),
            recording.as_ref(),
        );
        match saved {
            Ok(done) => Ok(Some(done)),
            Err(e) => {
                let e = caught(py, e)?;
                log_exception(py, "save_run %s", vec![session_id.clone()], &e);
                Ok(None)
            }
        }
    }

    #[pyo3(name = "_write_text")]
    fn write_text(&self, py: Python<'_>, path: &Bound<'_, PyAny>, text: &Bound<'_, PyAny>) -> PyResult<()> {
        if let Ok(s) = text.cast::<PyString>() {
            return self.write_text_at(py, path, &text_of(s));
        }
        // fh.write() of a non-str: the file is opened (emptied), then TypeError.
        let written: PyResult<()> = path.getattr("parent").and_then(|_| fspath(path)).and_then(|at| {
            mkdir_parent(py, &at)?;
            create_file(py, &at)?;
            Err(PyTypeError::new_err(format!("write() argument must be str, not {}", class_name(text)?)))
        });
        if let Err(e) = written {
            let e = caught(py, e)?;
            log_exception(py, "write text %s", vec![path.clone()], &e);
        }
        Ok(())
    }

    /// Flatten a message's content to text. Prompt-cached turns carry content
    /// as a list of {type, text, ...} blocks rather than a plain string.
    #[staticmethod]
    #[pyo3(name = "_content_text")]
    fn content_text_py<'py>(content: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        content_text(content)
    }

    /// Readable view of a native assistant turn's tool_calls: the tracking
    /// params pulled OUT of the call arguments into their own fields, then one
    /// action entry per call, the same shape the agents' snapshot views use.
    /// Without this the whole step renders as an empty assistant turn (all its
    /// substance rides in the calls).
    ///
    /// `memory` is a MAIN-DRIVER tracking param, so it is rendered only when
    /// present. coder=True (a Coder chat): the Rust coder's turn as its own
    /// conversation log shows it, `insight` and `next_goal` being calls of
    /// their own, and only what the turn has: {insight, next_goal, action}.
    #[staticmethod]
    #[pyo3(name = "_render_tool_calls", signature = (calls, coder=None), text_signature = "(calls, coder=False)")]
    fn render_tool_calls_py(calls: &Bound<'_, PyAny>, coder: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        render_tool_calls(calls, truthy(coder)?)
    }

    /// (name, arguments dict) of one native tool call.
    #[staticmethod]
    #[pyo3(name = "_call_name_args")]
    fn call_name_args_py<'py>(py: Python<'py>, tc: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
        let (name, args) = call_name_args(tc)?;
        PyTuple::new(py, [name, args])
    }

    /// A Rust coder turn: its insight ({kind, content}), its next_goal and its
    /// other calls as `action`, each only when the turn has it.
    #[staticmethod]
    #[pyo3(name = "_render_coder_calls")]
    fn render_coder_calls_py(calls: &Bound<'_, PyAny>) -> PyResult<String> {
        render_coder_calls(calls)
    }

    /// A persisted assistant entry, readable: a native {content, tool_calls}
    /// step renders like the live turns (provider_meta dropped: it is opaque
    /// provider blobs, unreadable by design); anything else (old-format step
    /// JSON, bridge note, plain prose) is already readable and passes through.
    #[staticmethod]
    #[pyo3(name = "_render_persisted_step", signature = (entry, coder=None), text_signature = "(entry, coder=False)")]
    fn render_persisted_step_py(entry: &Bound<'_, PyAny>, coder: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
        render_persisted_step(entry, truthy(coder)?)
    }

    /// Persisted tool results, readable: the native [{tool_call_id, content}]
    /// list joins to its content bodies (already <Tool_response>-wrapped);
    /// plain strings (request markers, legacy results) pass through.
    #[staticmethod]
    #[pyo3(name = "_render_persisted_results")]
    fn render_persisted_results_py(raw: &Bound<'_, PyAny>) -> PyResult<String> {
        render_persisted_results(raw)
    }

    /// Render an exact messages payload to the human-readable conversation log
    /// (same shape as main.py's conversation_N.txt): SYSTEM PROMPT block, then
    /// each ASSISTANT / USER / TOOL turn in the order sent. Native (coder)
    /// assistant turns get their tool_calls rendered readably; main-agent turns
    /// carry no tool_calls and render exactly as before. coder=True (a Coder
    /// chat): the Rust coder's turns as its own log shows them, without the
    /// `ok` that answers each insight / next_goal call.
    #[pyo3(
        name = "_render_messages",
        signature = (messages, header_lines=None, final_response=None, coder=None),
        text_signature = "($self, messages, header_lines=None, final_response=None, coder=False)"
    )]
    fn render_messages_py(
        &self,
        messages: &Bound<'_, PyAny>,
        header_lines: Option<&Bound<'_, PyAny>>,
        final_response: Option<&Bound<'_, PyAny>>,
        coder: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<String> {
        render_messages(messages, header_lines, final_response, truthy(coder)?)
    }

    /// All sessions, newest-first, as {id, name, updated_at, last_done_message,
    /// agent_mode, run_pkg}.
    fn list_sessions<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let index = self.read_index(py)?;
        let items = PyList::empty(py);
        let new_chat = PyString::new(py, "New chat").into_any();
        let empty = PyString::new(py, "").into_any();
        for (sid, meta) in index.iter() {
            let item = PyDict::new(py);
            item.set_item("id", sid)?;
            item.set_item("name", or_else(get(&meta, "title")?, new_chat.clone())?)?;
            item.set_item("updated_at", get_or(&meta, "updated_at", 0)?)?;
            item.set_item("last_done_message", get_or(&meta, "last_done_message", "")?)?;
            item.set_item("agent_mode", get_or(&meta, "agent_mode", "")?)?;
            // legacy rows: lets the sidebar infer the mode icon
            item.set_item("run_pkg", or_else(get(&meta, "run_pkg")?, empty.clone())?)?;
            items.append(item)?;
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("key", py.import("operator")?.call_method1("itemgetter", ("updated_at",))?)?;
        kwargs.set_item("reverse", true)?;
        items.call_method("sort", (), Some(&kwargs))?;
        Ok(items)
    }

    /// {id, name, last_done_message, exchanges, context_tokens, context_cap}
    /// for the reopen view, or None if unknown. `exchanges` is the full
    /// request/outcome transcript ({task, done_message, status} per run) shown
    /// as the numbered Agent Notes on reopen; last_done_message stays for
    /// legacy sessions saved before exchanges.json existed. Legacy rows that
    /// only carried the old cumulative `tokens_used` are ignored
    /// (context_tokens defaults to 0, so the bar starts empty and the next run
    /// corrects it).
    fn get_session<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let meta = self.read_index(py)?.get_item(session_id.str()?)?;
        let Some(meta) = meta else { return Ok(None) };
        if !meta.is_truthy()? {
            return Ok(None);
        }
        let zero = PyInt::new(py, 0).into_any();
        let d = PyDict::new(py);
        d.set_item("id", session_id.str()?)?;
        d.set_item("name", or_else(get(&meta, "title")?, PyString::new(py, "New chat").into_any())?)?;
        d.set_item("context_tokens", py_int(&or_else(get_or(&meta, "context_tokens", 0)?, zero.clone())?)?)?;
        d.set_item("context_cap", py_int(&or_else(get_or(&meta, "context_cap", 0)?, zero.clone())?)?)?;
        d.set_item("last_done_message", get_or(&meta, "last_done_message", "")?)?;
        d.set_item("run_pkg", or_str(get(&meta, "run_pkg")?)?)?;
        d.set_item("agent_mode", get_or(&meta, "agent_mode", "")?)?;
        d.set_item("exchanges", self.read_exchanges(py, session_id)?)?;
        Ok(Some(d))
    }

    /// Remove a session's folder and its index entry (idempotent).
    fn delete_session(&self, py: Python<'_>, session_id: &Bound<'_, PyAny>) -> PyResult<bool> {
        // Nothing with a malformed id can exist on disk, so refuse quietly
        // rather than letting _session_dir raise into the handler below and log
        // a traceback for what is simply a bad request.
        if !self.is_safe_id(session_id)? {
            log(py, "warning", "delete_session: ignoring unsafe id %r", vec![session_id.clone()], None);
            return Ok(false);
        }
        match self.delete_body(py, session_id) {
            Ok(done) => Ok(done),
            Err(e) => {
                let e = caught(py, e)?;
                log_exception(py, "delete_session %s", vec![session_id.clone()], &e);
                Ok(false)
            }
        }
    }

    /// Full saved conversation.json payload ({session_id, history}) exactly as
    /// on disk, or None if missing/corrupt. This is the agent's real optimized
    /// memory, used by the debug export.
    fn read_raw<'py>(&self, py: Python<'py>, session_id: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let f = self.session_file(py, session_id)?;
        let at = fspath(&f)?;
        if at.exists() {
            match read_text_file(py, &at).and_then(|text| json_loads(py, &text)) {
                Ok(data) => {
                    if data.is_instance_of::<PyDict>() {
                        return Ok(Some(data));
                    }
                }
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "read_raw %s", vec![session_id.clone()], &e);
                }
            }
        }
        Ok(None)
    }

    /// Render a session's saved memory as a HUMAN-READABLE conversation log,
    /// the same shape main.py's save_conversation writes (conversation_N.txt):
    /// a SYSTEM PROMPT block, then the real interleaved ASSISTANT (per-step
    /// JSON, full thinking/eval/decision/memory/next_goal/action) / USER
    /// (<tool_response>) turns. This is the true agent memory, formatted to
    /// read and critique, not raw JSON. Returns the text, or None if nothing
    /// was saved.
    #[pyo3(signature = (session_id, system_prompt=None))]
    fn render_readable<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        system_prompt: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Option<String>> {
        let Some(data) = self.read_raw(py, session_id)? else { return Ok(None) };
        let hist = or_dict(get(&data, "history")?)?;
        let meta = match self.get_session(py, session_id)? {
            Some(d) => or_dict(d.into_any())?,
            None => PyDict::new(py).into_any(),
        };
        let asst = or_list(get(&hist, "assistant_messages")?)?;
        let tools = or_list(get(&hist, "tool_responses")?)?;
        let task = or_str(get_or(&hist, "task", "")?)?;
        let bar = "=".repeat(60);
        let mut out = Lines::new();
        out.text("=== AGENT MEMORY (conversation log) ===");
        out.text(format!("Chat: {}", fmt_text(&get_or(&meta, "name", "")?)?));
        out.text(format!("Session: {}", fmt_text(session_id)?));
        out.text(format!("Steps: {}", asst.len()?));
        out.text(format!("Final status: {}", fmt_text(&get_or(&hist, "final_status", "")?)?));
        out.text(format!("Done: {}", fmt_text(&get_or(&hist, "done_message", "")?)?));
        out.text(format!("Task: {}", fmt_text(&task)?));
        out.text(bar.as_str());
        if let Some(prompt) = system_prompt {
            if prompt.is_truthy()? {
                out.text("");
                out.text("=== SYSTEM PROMPT ===");
                let trimmed = match prompt.cast_exact::<PyString>() {
                    Ok(s) => PyString::new(py, py_strip(&text_of(s))).into_any(),
                    Err(_) => prompt.call_method0("strip")?,
                };
                out.obj(trimmed);
                out.text(bar.as_str());
            }
        }
        for (i, step) in asst.try_iter()?.enumerate() {
            let step = step?;
            out.text("");
            out.text("--- ASSISTANT ---");
            if i == 0 && task.is_truthy()? {
                out.text("<User_Task>");
                out.obj(task.clone());
                out.text("</User_Task>");
                out.text("");
            }
            let coder = get(&meta, "agent_mode")?.eq("coder")?;
            out.text(render_persisted_step(&step, coder)?);
            let tr = if i < tools.len()? { Some(tools.get_item(i)?) } else { None };
            if let Some(tr) = tr {
                if tr.is_truthy()? {
                    out.text("");
                    out.text("--- USER ---");
                    out.text("<tool_response>");
                    out.text(render_persisted_results(&tr)?);
                    out.text("</tool_response>");
                }
            }
        }
        Ok(Some(out.join(py)? + "\n"))
    }

    /// Debug aid: write a session's TRUE memory log (.txt) to the user's
    /// Downloads folder. Prefers memory_log.txt (the exact payload sent to the
    /// model); falls back to a reconstruction for chats saved before memory_log
    /// existed. Returns the destination path, or None if nothing's saved.
    #[pyo3(signature = (session_id, system_prompt=None))]
    fn export_to_downloads<'py>(
        &self,
        py: Python<'py>,
        session_id: &Bound<'py, PyAny>,
        system_prompt: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Option<String>> {
        let log_file = self.memory_log_file(py, session_id)?;
        let at = fspath(&log_file)?;
        let mut text: Option<String> = None;
        if at.exists() {
            match read_text_file(py, &at) {
                Ok(t) => text = Some(t),
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "read memory_log %s", vec![session_id.clone()], &e);
                }
            }
        }
        if text.is_none() {
            text = self.render_readable(py, session_id, system_prompt)?; // back-compat fallback
        }
        let Some(text) = text else { return Ok(None) };
        let meta = match self.get_session(py, session_id)? {
            Some(d) => d.into_any(),
            None => PyDict::new(py).into_any(),
        };
        let name = or_else(get(&meta, "name")?, session_id.str()?.into_any())?;
        let Ok(name) = name.cast::<PyString>() else {
            return Err(PyTypeError::new_err(format!(
                "expected string or bytes-like object, got '{}'",
                class_name(&name)?
            )));
        };
        let name = text_of(name);
        let slug_full = match slug_regex() {
            Some(re) => re.replace_all(&name, "-").into_owned(),
            None => name.clone(),
        };
        let slug: String = slug_full.trim_matches('-').chars().take(40).collect();
        let stem = if slug.is_empty() { fmt_text(session_id)? } else { slug };
        // System date-time in a clear, readable form so each download is easy
        // to tell apart.
        let stamp = str_text(&py.import("time")?.call_method1("strftime", ("%Y-%m-%d_%H-%M-%S",))?)?;
        let fname = format!("AutoCua-{stem}-{stamp}.txt");
        let home = py.import("pathlib")?.getattr("Path")?.call_method0("home")?;
        let folders = [home.div("Downloads")?, home.div("Desktop")?, home.clone()];
        for folder in folders {
            let attempt = (|| -> PyResult<Option<String>> {
                if !fspath(&folder)?.exists() {
                    return Ok(None);
                }
                let dest = folder.div(fname.as_str())?;
                write_text_file(py, &fspath(&dest)?, &text)?;
                Ok(Some(str_text(&dest)?))
            })();
            match attempt {
                Ok(Some(path)) => return Ok(Some(path)),
                Ok(None) => {}
                Err(e) => {
                    let e = caught(py, e)?;
                    log_exception(py, "export_to_downloads -> %s", vec![folder.clone()], &e);
                }
            }
        }
        Ok(None)
    }
}

/// Truthiness of an optional flag (None and a missing one are false).
fn truthy(flag: Option<&Bound<'_, PyAny>>) -> PyResult<bool> {
    match flag {
        Some(f) => f.is_truthy(),
        None => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills AutoCua.agent_native.agent_conversation: the class, the one
/// ConversationService the app shares (`conversation`, which app.py imports
/// directly), SHELL_BRIDGE_ID_PREFIX, the logger, and the path helpers bound
/// from the AutoCua package as the Python module imported them.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let _ = MODULE.set(py, m.clone().unbind());
    m.add_class::<ConversationService>()?;
    m.add("SHELL_BRIDGE_ID_PREFIX", SHELL_BRIDGE_ID_PREFIX)?;
    m.add("logger", py.import("logging")?.call_method1("getLogger", (LOGGER_NAME,))?)?;
    // Where the user's data lives: one definition for the whole app, in the
    // package root, so chats / api keys / everything else can never disagree
    // about it.
    if let Ok(package) = py.import("AutoCua") {
        for name in ["data_root", "install_dir", "_ensure"] {
            if let Ok(f) = package.getattr(name) {
                m.add(name, f)?;
            }
        }
    }
    // Module-level singleton: app.py imports this directly.
    m.add("conversation", Py::new(py, ConversationService)?)?;
    Ok(())
}
