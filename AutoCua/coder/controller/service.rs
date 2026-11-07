//! The coder's controller: it routes each tool call to the code that runs it.
//!
//! The port of the coder's path through mac/controller/view.py (the
//! ControllerView class): route_action's flat path for the coder's tools
//! (shell, view, grep, glob, write, replace, web, plan, todo_list,
//! update_todo, wait, scratchpad, subagent, exit), the subagent spawn (the
//! mac minion) and the end-of-batch wait that turns each subagent's report
//! into its call's result. The result dicts, their key order and texts, the
//! early returns and the whole-batch error are the mac ones. Left out: the
//! GUI services and tools, the old nested action format, the web animation
//! and the UI events.
//!
//! Changes from Python, on purpose:
//! - a subagent that ends without a valid result file is reported with its
//!   exit code and last stderr lines, where Python waited for it forever;
//! - a subagent in its own terminal window (external_terminal) is watched
//!   through two files its window writes beside its result, its pid and its
//!   exit code, so a crash, a closed window or a window that never starts
//!   it (no pid within a minute of opening) is reported too, where Python
//!   waited forever; on Windows it keeps its process handle; the mac window
//!   runs the command from a script file, since a typed line is cut at the
//!   tty's 1024 bytes;
//! - `wait` and `update_todo` with a value that is not a number take the
//!   first number in it ("2s", "#3"), else that one call gets an error
//!   result, where Python failed the whole batch;
//! - a tool nobody routes gets an "Unknown action type" error result, where
//!   Python added nothing;
//! - the subagent's API key goes in an environment variable, not on argv;
//! - the subagents still running are stopped on Ctrl+C and when the
//!   controller goes, where Python left them running;
//! - a subagent call carries a `mode` ("workflow" or "critic") that its
//!   report repeats; a call with no valid mode or an empty brief starts
//!   nothing and gets an error result.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pyo3::exceptions::{PyException, PyOSError};
use pyo3::prelude::*;
use regex::Regex;
use serde_json::{json, Value};

use super::cli::service::CLIService;
use super::scratchpad::service::ScratchpadService;
use super::task_tracker::service::TaskTrackerService;
use super::tool::plan::service::PlanService;
use super::tool::web::service::WebService;
use crate::sandbox::service::{py_os_error, py_repr_str};
use crate::tool_registry::service::NOTE_TOOLS;
use crate::{py_err_str, py_str_of, truthy};

/// The end-of-batch wait looks at the subagents this often, in seconds.
const AWAIT_POLL: f64 = 0.5;
/// Waits are cut into slices this long, so Ctrl+C is seen at once.
const SLICE: Duration = Duration::from_millis(50);
/// How many of a subagent's last stderr lines its crash report shows.
const STDERR_TAIL: usize = 20;
/// How long a crashed subagent's stderr may take to reach its end.
const STDERR_SETTLE: Duration = Duration::from_millis(500);
/// How long a window's shell may take to write the exit code once the
/// subagent in it is gone.
const MARK_SETTLE: Duration = Duration::from_millis(50);
/// How long a window's shell may take to start the subagent (the pid it
/// writes) once the window is open.
const START_LIMIT: Duration = Duration::from_secs(60);
/// How long a stopped subagent gets to end on its own before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// On Ctrl+C, how long the subagents may take to end by themselves before
/// they are asked to stop.
const CTRL_C_SETTLE: Duration = Duration::from_secs(1);
/// The subagent child's module, run with `python -m`.
const SUBAGENT_MODULE: &str = "AutoCua.coder.subagent";
/// The environment variable that carries the API key to the child.
const API_KEY_VAR: &str = "AutoCua_CODER_API_KEY";
/// The output of a subagent that gave no report (the mac minion's text).
const NO_RESULT: &str = "Subagent did not return a result (subprocess crashed or was terminated).";
/// The start of the text WebService gives for a failed search.
const WEB_FAILED: &str = "{\ntool: web,\nstatus: error,";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows: runs the subagent in a console window of its own
/// (CREATE_NEW_CONSOLE, 0x10). Python leaves the new console's handles to the
/// child, where Command would hand it this process's (STARTF_USESTDHANDLES)
/// and the window would stay empty. This one-liner is the child the coder
/// holds: its exit code is the subagent's (a Windows status code at or above
/// 2^31 folded into the signed range, since sys.exit cannot pass a number
/// past a C long), taskkill /T ends both.
#[cfg(windows)]
const WIN_CONSOLE_TRAMPOLINE: &str = "import subprocess, sys; c = subprocess.call(sys.argv[1:], creationflags=16); \
                                      sys.exit(c - (1 << 32) if c >= (1 << 31) else c)";
#[cfg(windows)]
const PATH_LIST_SEP: &str = ";";
#[cfg(not(windows))]
const PATH_LIST_SEP: &str = ":";

/// Python's logging.lastResort: the bare message on stderr, never a panic.
fn log(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// Why a batch stopped: a message that becomes the whole batch's error
/// result (Python's `except Exception`), or a Python error that must fly
/// (KeyboardInterrupt).
enum RouteErr {
    Msg(String),
    Py(PyErr),
}

impl From<PyErr> for RouteErr {
    fn from(e: PyErr) -> Self {
        RouteErr::Py(e)
    }
}

/// One result of a batch, in call order: done, or a subagent of this batch
/// whose report is filled in at the end (Python's `_pending_minion`
/// placeholder).
enum Slot {
    Done(Value),
    Pending { query: Value, mode: String, session_id: String },
}

/// The subagent's modes: "workflow" maps the code, "critic" judges an edit.
const SUBAGENT_MODES: [&str; 2] = ["workflow", "critic"];

/// A subagent at work (a row of Python's `_minion_tasks`; its query rides on
/// the batch's placeholder).
struct SubagentTask {
    /// Matches the report to its call's placeholder; never given to the child.
    session_id: String,
    /// Where the child writes `{"task", "summary", "status"}`.
    result_file: PathBuf,
    /// None when it could not be started (start_error says why), or when
    /// it runs in a window.
    child: Option<Child>,
    start_error: String,
    /// The child runs in its own terminal window (mac and Linux), with no
    /// handle here: it writes `<result>.pid` as it starts and its window's
    /// shell writes `<result>.exit` when it ends.
    window: bool,
    /// What opened the window (`open`, or the terminal emulator), until
    /// it exits: one that fails says the window never opened. Never killed,
    /// the window is the user's.
    launcher: Option<Child>,
    /// When the window was opened (the opener's end; its start before that):
    /// the child's pid is due within START_LIMIT of it.
    opened: Instant,
    /// The child's last stderr lines, kept by its reader thread.
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    stderr_reader: Option<JoinHandle<()>>,
    /// The result file is there but does not read: said once.
    read_error_logged: bool,
}

/// A finished subagent (a row of Python's `_minion_completed`): the summary
/// and status of its result file, or the text that says it gave none.
struct Completion {
    session_id: String,
    report: Result<(Value, Value), String>,
}

/// A window child that reported while still running (its interpreter
/// ending): the pid it wrote and its result's path, whose marks go once it
/// is gone.
struct DeliveredWindow {
    pid: i32,
    result_file: PathBuf,
}

/// The central router for the coder's tool calls (and a subagent's).
pub struct ControllerView {
    /// The file tools and the shell, on the session's sandbox folder.
    pub cli_service: CLIService,
    /// The todo list: cli_todo/<session>/todo.md.
    pub task_tracker: TaskTrackerService,
    /// The plan: cli_plan/<session>/plan.md and plan_no.txt.
    pub plan_service: PlanService,
    /// The notes: cli_milestone/<session>/milestone.md, or
    /// cli_subagent/<session>/milestone.md for a subagent.
    pub scratchpad_service: ScratchpadService,
    /// LLM provider, model and runtime key, as given: for the web search and
    /// the subagents' command line.
    provider: String,
    model: String,
    api_key: Option<String>,
    /// AutoCua/coder/scratchpad: subagent_result/ goes in it.
    scratch_root: PathBuf,
    /// Each subagent in a terminal window of its own (main.py's default)
    /// instead of piped into this process (the app's runs).
    external_terminal: bool,
    /// The coder saves its conversation, so its subagents save theirs too
    /// (cli_subagent_conversation/<their session> in the cwd).
    save_conversation: bool,
    /// Subagents at work, oldest first. The ones of a batch that returned
    /// early stay here, and the next batch waits for them at its end.
    subagent_tasks: Vec<SubagentTask>,
    /// Children whose report came in while they were still running: reaped
    /// at the next look, or stopped with the rest.
    delivered: Vec<Child>,
    /// The same for window children (by pid): their marks go at the next
    /// look once they are gone.
    delivered_windows: Vec<DeliveredWindow>,
}

impl ControllerView {
    /// The coder's controller (`subagent_mode` false) or a subagent's (true:
    /// its notes go to their own folder). Makes the session's folders under
    /// AutoCua/coder/scratchpad and its sandbox ~/Desktop/sandbox_workspace/
    /// <session_id>. `external_terminal`: its subagents each get a terminal
    /// window. `save_conversation`: they save their conversations, as the
    /// coder saves its own. Err when the sandbox folder cannot be made, as
    /// Python raised.
    pub fn new(
        py: Python<'_>,
        provider: &str,
        model: &str,
        session_id: &str,
        api_key: Option<&str>,
        subagent_mode: bool,
        external_terminal: bool,
        save_conversation: bool,
    ) -> PyResult<ControllerView> {
        let scratch_root = crate::scratch_root(py)?;
        let task_tracker = TaskTrackerService::new(&scratch_root, session_id);
        let plan_service = PlanService::new(&scratch_root, session_id);
        let scratchpad_service = ScratchpadService::new(&scratch_root, session_id, subagent_mode);
        let cli_service = CLIService::new(Some(session_id)).map_err(|e| PyOSError::new_err(e.to_string()))?;
        Ok(ControllerView {
            cli_service,
            task_tracker,
            plan_service,
            scratchpad_service,
            provider: provider.to_string(),
            model: model.to_string(),
            api_key: api_key.map(str::to_string),
            scratch_root,
            external_terminal,
            save_conversation,
            subagent_tasks: Vec::new(),
            delivered: Vec::new(),
            delivered_windows: Vec::new(),
        })
    }

    /// One batch of actions, each a JSON object with "type" and its
    /// arguments (what tool_calls_to_steps gives), run in order. Returns the
    /// mac result: the one result, `{"status": "success", "action":
    /// "multiple", "results": [...]}` for several, an early return's own
    /// result (exit, a failed plan or scratchpad, a refused shell command),
    /// or `{"status": "error", "message": ...}` when the batch fails as a
    /// whole. The subagents at work are waited for at the batch's end.
    /// Err only for KeyboardInterrupt (or another non-Exception error),
    /// after every subagent child is stopped.
    pub fn route_action(&mut self, py: Python<'_>, actions: &[Value]) -> PyResult<Value> {
        let error = match self.route(py, actions) {
            Ok(result) => return Ok(result),
            Err(RouteErr::Msg(message)) => message,
            Err(RouteErr::Py(e)) if e.is_instance_of::<PyException>(py) => py_err_str(py, &e),
            Err(RouteErr::Py(e)) => {
                // Ctrl+C: the subagents go too, then it flies. A terminal's
                // Ctrl+C reached them as well (same process group): they
                // get a moment to end on their own first.
                py.detach(|| self.stop_subagents(CTRL_C_SETTLE));
                return Err(e);
            }
        };
        log(&format!("Error routing action: {error}"));
        Ok(json!({"status": "error", "message": error}))
    }

    /// Stop every subagent child still running. Each is asked to stop first
    /// (SIGINT on unix, so it ends its own command and writes its partial
    /// result; on Windows its tree is ended), gets up to 3 s, and is killed
    /// after that. Their result files are deleted, since nobody reads them
    /// now. Also runs on Drop.
    pub fn kill_subagents(&mut self) {
        self.stop_subagents(Duration::ZERO);
    }

    /// kill_subagents, after `settle`: the children that end on their own
    /// within it are not signalled again (a second Ctrl+C could cut off the
    /// partial result the first one is writing).
    fn stop_subagents(&mut self, settle: Duration) {
        let mut children: Vec<Child> = Vec::new();
        // The children in windows (mac and Linux), by the pid each wrote,
        // read again after each wait (one whose pid comes during it is
        // stopped too); one that wrote none has nothing of ours to stop.
        let mut pids: Vec<i32> = Vec::new();
        let mut pid_files: Vec<PathBuf> = Vec::new();
        let mut windows = false;
        let mut result_files: Vec<PathBuf> = Vec::new();
        for mut task in std::mem::take(&mut self.subagent_tasks) {
            children.extend(task.child.take());
            if let Some(launcher) = task.launcher.take() {
                reap(launcher);
            }
            if task.window {
                windows = true;
                pid_files.push(mark(&task.result_file, "pid"));
            }
            result_files.push(task.result_file);
        }
        for delivered in std::mem::take(&mut self.delivered_windows) {
            windows = true;
            pids.push(delivered.pid);
            result_files.push(delivered.result_file);
        }
        children.append(&mut self.delivered);
        children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        pids.retain(|&pid| alive(pid));
        // Each pid once: one dropped as gone is never picked up again (its
        // number could be another process's by then).
        let mut seen: Vec<i32> = Vec::new();
        let mut late_pids = |pids: &mut Vec<i32>| {
            for file in &pid_files {
                if let Some(pid) = read_pid(file).filter(|pid| !seen.contains(pid)) {
                    seen.push(pid);
                    if alive(pid) {
                        pids.push(pid);
                    }
                }
            }
        };
        late_pids(&mut pids);
        wait_for_exit(&mut children, &mut pids, settle);
        late_pids(&mut pids);
        for child in &children {
            ask_to_stop(child);
        }
        for &pid in &pids {
            ask_pid_to_stop(pid);
        }
        wait_for_exit(&mut children, &mut pids, STOP_GRACE);
        late_pids(&mut pids);
        for child in children {
            stop_now(child);
        }
        for pid in pids {
            kill_pid(pid);
        }
        if windows {
            // A window's shell writes the exit code a moment after its child
            // is gone: not left behind.
            thread::sleep(MARK_SETTLE);
        }
        for file in result_files {
            // With the temp file of a write that was cut off (subagent/service.rs
            // writes <result>.tmp, then renames it) and a window's two marks.
            let _ = std::fs::remove_file(&file);
            let _ = std::fs::remove_file(mark(&file, "tmp"));
            remove_marks(&file);
        }
    }

    fn route(&mut self, py: Python<'_>, actions: &[Value]) -> Result<Value, RouteErr> {
        self.reap_delivered(py);
        let mut results: Vec<Slot> = Vec::new();

        for item in actions {
            let action_type = match item.get("type") {
                Some(kind) if truthy(kind) => py_str_of(kind),
                Some(_) => {
                    log(&format!("Action item missing 'type' field: {}", py_repr(item)));
                    continue;
                }
                // The old nested format ({"shell": {...}}), which nothing
                // builds any more, is not ported: such an item adds nothing,
                // as on the mac for a key it does not know.
                None => continue,
            };

            match action_type.as_str() {
                "wait" => {
                    let one = json!("1");
                    let raw = item.get("value").unwrap_or(&one);
                    let row = match py_float(py, raw)? {
                        Ok(wait_time) => {
                            let mut elapsed = 0.0;
                            while elapsed < wait_time {
                                sleep_checked(py, (wait_time - elapsed).min(0.5))?;
                                elapsed += 0.5;
                            }
                            json!({"status": "success", "action": "tool", "tool": "wait", "duration": wait_time})
                        }
                        Err(message) => json!({"status": "error", "action": "tool", "tool": "wait", "message": message}),
                    };
                    results.push(Slot::Done(row));
                }

                "web" => {
                    let query = item.get("value").cloned().unwrap_or(Value::Null);
                    // A new service for each search, as on the mac.
                    let web_service = WebService::new(py, &self.provider, &self.model, self.api_key.clone(), None)?;
                    let web_result = web_service.search(py, &py_str_of(&query))?;
                    // WebService reports a failed search inside its text
                    let failed = web_result.starts_with(WEB_FAILED);
                    results.push(Slot::Done(json!({
                        "status": if failed { "error" } else { "success" },
                        "action": "tool",
                        "tool": "web",
                        "query": query,
                        "result": web_result,
                    })));
                }

                "subagent" => {
                    // Several in one batch run in parallel; the end of the
                    // batch waits for every one of them. A call with no valid
                    // mode, or with an empty brief, starts nothing and gets
                    // an error result.
                    let mode = arg_str(item, "mode", "");
                    let error = if !SUBAGENT_MODES.contains(&mode.as_str()) {
                        Some("subagent mode must be \"workflow\" or \"critic\".")
                    } else if crate::py_strip(&arg_str(item, "value", "")).is_empty() {
                        Some("subagent value must be a brief, not empty.")
                    } else {
                        None
                    };
                    if let Some(message) = error {
                        results.push(Slot::Done(json!({
                            "status": "error",
                            "action": "subagent",
                            "message": message,
                        })));
                    } else {
                        let query = item.get("value").cloned().unwrap_or_else(|| json!(""));
                        let session_id = self.start_subagent(py, &query, &mode)?;
                        results.push(Slot::Pending { query, mode, session_id });
                    }
                }

                "plan" => {
                    let op = item.get("op").cloned().unwrap_or_else(|| json!("set"));
                    let (success, detail) = self.plan_service.apply_op(
                        &py_str_of(&op),
                        &arg_str(item, "value", ""),
                        arg_int(item, "from", 0),
                        arg_int(item, "to", 0),
                    );
                    if !success {
                        return Ok(json!({"status": "error", "action": "plan_failed", "op": op, "message": detail}));
                    }
                    results.push(Slot::Done(json!({"status": "success", "action": "plan_updated", "op": op})));
                }

                "todo_list" => {
                    let todo_value = item.get("value").cloned().unwrap_or(Value::Null);
                    // Not checked, as on the mac: the list is always "created".
                    self.task_tracker.save_todo(&py_str_of(&todo_value));
                    results.push(Slot::Done(json!({"status": "success", "action": "todo_created"})));
                }

                "update_todo" => {
                    let zero = json!("0");
                    let raw = item.get("value").unwrap_or(&zero);
                    let row = match py_int(py, raw)? {
                        Ok((task_number, shown)) => {
                            if self.task_tracker.update_task(task_number) {
                                json!({"status": "success", "action": "todo_updated", "task": task_number})
                            } else {
                                // Reported on its own call; the rest of the batch still runs
                                json!({
                                    "status": "error",
                                    "action": "todo_update_failed",
                                    "message": format!("Task {shown} is not in the todo list"),
                                })
                            }
                        }
                        Err(message) => json!({"status": "error", "action": "todo_update_failed", "message": message}),
                    };
                    results.push(Slot::Done(row));
                }

                "scratchpad" => {
                    let value = item.get("value").cloned().unwrap_or(Value::Null);
                    if !self.scratchpad_service.append_scratchpad(&py_str_of(&value)) {
                        return Ok(json!({
                            "status": "error",
                            "action": "scratchpad_failed",
                            "message": "Could not add scratchpad entry",
                        }));
                    }
                    results.push(Slot::Done(json!({"status": "success", "action": "scratchpad_added", "scratchpad": value})));
                }

                "shell" => {
                    let command = arg_str(item, "command", "");
                    // input "" (the registry's default) keeps the sandbox's
                    // silence checks off; a missing input is Python's None,
                    // which turns them on.
                    let input_text = match item.get("input") {
                        None | Some(Value::Null) => None,
                        Some(input) => Some(py_str_of(input)),
                    };
                    let result = self.cli_service.shell(py, &command, input_text.as_deref())?;
                    // A refused or failed start ends the batch
                    if result.get("status").and_then(Value::as_str) == Some("error")
                        && result.get("error").is_some_and(truthy)
                    {
                        return Ok(result);
                    }
                    results.push(Slot::Done(result));
                }

                // The file tools run without the GIL: a big grep or glob
                // never holds up Python's own threads (the compression).
                "view" => {
                    let path = arg_str(item, "path", "");
                    let start = arg_int(item, "start", 0);
                    let end = arg_int(item, "end", 0);
                    let cli = &self.cli_service;
                    results.push(Slot::Done(py.detach(|| cli.view(&path, start, end))));
                }

                "grep" => {
                    let pattern = arg_str(item, "pattern", "");
                    let path = arg_str(item, "path", "");
                    let glob_filter = arg_str(item, "glob", "");
                    let output_mode = arg_str(item, "output_mode", "content");
                    let case_insensitive = arg_bool(item, "case_insensitive", false);
                    let head_limit = arg_int(item, "head_limit", 50);
                    let context = arg_int(item, "context", 0);
                    let cli = &self.cli_service;
                    results.push(Slot::Done(py.detach(|| {
                        cli.grep(&pattern, &path, &glob_filter, &output_mode, case_insensitive, head_limit, context)
                    })));
                }

                "glob" => {
                    let pattern = arg_str(item, "pattern", "");
                    let path = arg_str(item, "path", "");
                    let head_limit = arg_int(item, "head_limit", 100);
                    let cli = &self.cli_service;
                    results.push(Slot::Done(py.detach(|| cli.glob(&pattern, &path, head_limit))));
                }

                "write" => {
                    let path = arg_str(item, "path", "");
                    let line = arg_int(item, "line", 1);
                    let content = arg_str(item, "content", "");
                    let cli = &self.cli_service;
                    results.push(Slot::Done(py.detach(|| cli.write(&path, line, &content))));
                }

                "replace" => {
                    let path = arg_str(item, "path", "");
                    let line = arg_int(item, "line", 0);
                    let old_block = arg_str(item, "old_block", "");
                    let new_block = arg_str(item, "new_block", "");
                    let cli = &self.cli_service;
                    results.push(Slot::Done(py.detach(|| cli.replace(&path, line, &old_block, &new_block))));
                }

                "exit" => {
                    let summary = item.get("value").cloned().unwrap_or(Value::Null);
                    return Ok(json!({"status": "success", "action": "exit", "summary": summary}));
                }

                // The notes tools are answered by the loop itself; the mac
                // has no branch for them, so they add no result here either.
                name if NOTE_TOOLS.contains(&name) => {}

                _ => {
                    log(&format!("Unknown action type: {action_type}"));
                    results.push(Slot::Done(json!({
                        "status": "error",
                        "action": action_type,
                        "message": format!("Unknown action type: {action_type}"),
                    })));
                }
            }

            py.check_signals()?;
        }

        // The implicit await, only when subagents are at work, as on the mac:
        // every one of them, this batch's and any carried over.
        let completed = if self.subagent_tasks.is_empty() { Vec::new() } else { self.await_subagents(py)? };
        let mut results: Vec<Value> = results
            .into_iter()
            .map(|slot| match slot {
                Slot::Done(result) => result,
                Slot::Pending { query, mode, session_id } => {
                    subagent_report(query, &mode, completed.iter().find(|c| c.session_id == session_id))
                }
            })
            .collect();
        // Every report collected here goes now, a carried-over one too.

        if results.is_empty() {
            return Ok(json!({"status": "error", "message": "No valid action found"}));
        }
        if results.len() == 1 {
            if let Some(result) = results.pop() {
                return Ok(result);
            }
        }
        Ok(json!({"status": "success", "action": "multiple", "results": results}))
    }

    /// Start one subagent child for `query` in `mode` and register it (the
    /// mac's minion branch): piped, or in a terminal window of its own with
    /// external_terminal. Gives the placeholder's session id. A child that
    /// cannot be started is registered too: the end of the batch reports it.
    fn start_subagent(&mut self, py: Python<'_>, query: &Value, mode: &str) -> Result<String, RouteErr> {
        let result_dir = self.scratch_root.join("subagent_result");
        std::fs::create_dir_all(&result_dir).map_err(|e| RouteErr::Msg(py_os_error(&e, &result_dir)))?;
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let task_id = format!("result_{millis}_{}", uuid_hex(py, 6)?);
        let result_file = result_dir.join(format!("{task_id}.json"));
        let session_id = uuid_hex(py, 8)?;
        let executable: Option<String> = py.import("sys")?.getattr("executable")?.extract()?;
        let repo_root = repo_root(py)?;

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let task_text = py_str_of(query);
        let launch = Launch {
            executable: executable.as_deref().unwrap_or(""),
            repo_root: &repo_root,
            task: &task_text,
            provider: &self.provider,
            model: &self.model,
            mode,
            api_key: self.api_key.as_deref(),
            result_file: &result_file,
            save_conversation: self.save_conversation,
        };
        let started = if self.external_terminal {
            open_window(&launch, Arc::clone(&stderr_tail))
        } else {
            spawn_child(&launch, Arc::clone(&stderr_tail), false).map(|(child, reader)| (child, reader, false))
        };
        let (child, launcher, window, stderr_reader, start_error) = match started {
            Ok((launcher, reader, true)) => (None, Some(launcher), true, Some(reader), String::new()),
            Ok((child, reader, false)) => (Some(child), None, false, Some(reader), String::new()),
            Err(e) => (None, None, false, None, e),
        };
        self.subagent_tasks.push(SubagentTask {
            session_id: session_id.clone(),
            result_file,
            child,
            start_error,
            window,
            launcher,
            opened: Instant::now(),
            stderr_tail,
            stderr_reader,
            read_error_logged: false,
        });
        Ok(session_id)
    }

    /// Python's implicit await: hold until every subagent at work has
    /// reported or ended, this batch's and any left from a batch that
    /// returned early. Looks at each child and its result file every 0.5 s;
    /// Ctrl+C flies (route_action then stops the children).
    fn await_subagents(&mut self, py: Python<'_>) -> PyResult<Vec<Completion>> {
        let mut completed = Vec::new();
        loop {
            self.collect(py, &mut completed);
            if self.subagent_tasks.is_empty() {
                return Ok(completed);
            }
            sleep_checked(py, AWAIT_POLL)?;
        }
    }

    /// One look at every subagent at work: the finished ones move to
    /// `completed` (Python's watcher and _minion_complete_callback).
    fn collect(&mut self, py: Python<'_>, completed: &mut Vec<Completion>) {
        self.reap_delivered(py);
        for mut task in std::mem::take(&mut self.subagent_tasks) {
            match check_task(py, &mut task) {
                None => self.subagent_tasks.push(task),
                Some(report) => {
                    if let Some(child) = task.child.take() {
                        self.delivered.push(child);
                    }
                    if let Some(launcher) = task.launcher.take() {
                        reap(launcher);
                    }
                    let lingering = if task.window {
                        read_pid(&mark(&task.result_file, "pid")).filter(|&pid| alive(pid))
                    } else {
                        None
                    };
                    match lingering {
                        // Reported, still ending in its window: its marks go
                        // once it is gone.
                        Some(pid) => {
                            self.delivered_windows.push(DeliveredWindow { pid, result_file: task.result_file.clone() });
                        }
                        None => remove_marks(&task.result_file),
                    }
                    completed.push(Completion { session_id: task.session_id, report });
                }
            }
        }
    }

    /// Reap the children that delivered their report and have exited since;
    /// the marks of the window children among them go once they are gone.
    fn reap_delivered(&mut self, py: Python<'_>) {
        self.delivered.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        let (live, gone): (Vec<DeliveredWindow>, Vec<DeliveredWindow>) =
            std::mem::take(&mut self.delivered_windows).into_iter().partition(|delivered| alive(delivered.pid));
        self.delivered_windows = live;
        if !gone.is_empty() {
            // A window's shell writes the exit code a moment after its child
            // is gone: not left behind.
            py.detach(|| thread::sleep(MARK_SETTLE));
            for delivered in gone {
                remove_marks(&delivered.result_file);
            }
        }
    }
}

impl Drop for ControllerView {
    fn drop(&mut self) {
        self.kill_subagents();
    }
}

// ---------------------------------------------------------------------------
// The subagent children
// ---------------------------------------------------------------------------

/// What a subagent child is started with.
struct Launch<'a> {
    executable: &'a str,
    repo_root: &'a Path,
    task: &'a str,
    provider: &'a str,
    model: &'a str,
    /// "workflow" or "critic": the child reads its mode's prompt block.
    mode: &'a str,
    api_key: Option<&'a str>,
    result_file: &'a Path,
    /// The child saves its conversation (--save-conversation).
    save_conversation: bool,
}

/// Start the child, `python -m AutoCua.coder.subagent --task Q --provider P
/// --model M --mode MODE --result PATH`, in this process's folder, with no stdin and its
/// stdout and stderr drained on threads (the last stderr lines kept in
/// `tail`). On unix it stays in this process group, so a terminal Ctrl+C
/// reaches it too. `console`: on Windows, the subagent gets a console window
/// of its own (behind WIN_CONSOLE_TRAMPOLINE, the child held here). Err: why
/// it did not start.
fn spawn_child(
    launch: &Launch<'_>,
    tail: Arc<Mutex<VecDeque<String>>>,
    console: bool,
) -> Result<(Child, JoinHandle<()>), String> {
    let mut cmd = Command::new(launch.executable);
    #[cfg(windows)]
    if console {
        cmd.args(["-c", WIN_CONSOLE_TRAMPOLINE, launch.executable]);
    }
    #[cfg(not(windows))]
    let _ = console;
    cmd.args(child_args(launch));
    cmd.env("PYTHONUNBUFFERED", "1");
    cmd.env("PYTHONPATH", pythonpath(launch.repo_root));
    // Never on the command line, where any process list shows it.
    if let Some(key) = launch.api_key.filter(|key| !key.is_empty()) {
        cmd.env(API_KEY_VAR, key);
    } else {
        cmd.env_remove(API_KEY_VAR);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| py_os_error(&e, Path::new(launch.executable)))?;

    // Read to the end, so a chatty child never blocks on a full pipe.
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        stop_now(child);
        return Err("its output pipes could not be opened".to_string());
    };
    if thread::Builder::new().name("subagent-stdout".into()).spawn(move || drain(stdout)).is_err() {
        stop_now(child);
        return Err("can't start new thread".to_string());
    }
    match thread::Builder::new().name("subagent-stderr".into()).spawn(move || keep_tail(stderr, &tail)) {
        Ok(reader) => Ok((child, reader)),
        Err(_) => {
            stop_now(child);
            Err("can't start new thread".to_string())
        }
    }
}

/// The child's arguments: `-m AutoCua.coder.subagent --task Q --provider P
/// --model M --mode MODE --result PATH`.
fn child_args(launch: &Launch<'_>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-m",
        SUBAGENT_MODULE,
        "--task",
        launch.task,
        "--provider",
        launch.provider,
        "--model",
        launch.model,
        "--mode",
        launch.mode,
        "--result",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    args.push(launch.result_file.as_os_str().to_os_string());
    if launch.save_conversation {
        args.push(OsString::from("--save-conversation"));
    }
    args
}

/// The child's PYTHONPATH: the repo first, so it imports this AutoCua from
/// any folder, then this process's.
fn pythonpath(repo_root: &Path) -> OsString {
    let mut pythonpath = repo_root.as_os_str().to_os_string();
    if let Some(old) = std::env::var_os("PYTHONPATH").filter(|old| !old.is_empty()) {
        pythonpath.push(PATH_LIST_SEP);
        pythonpath.push(old);
    }
    pythonpath
}

// The terminal window of a subagent (external_terminal). What comes back:
// the process held, its stderr reader, and whether that process only opened
// a window (true: the mac and Linux opener, watched through the files the
// window writes) or is the subagent itself (false: a handle, watched as a
// piped child).

/// macOS: a new Terminal.app window, titled with the mode and the brief,
/// runs the shell line. The line sits in `<result>.command`, a file Terminal
/// runs in a new window when `open -a Terminal` hands it over, so nothing is
/// typed into the window: the mac coder typed its line in with AppleScript's
/// `do script`, which the tty cuts at 1024 bytes and Terminal garbles when
/// the window's shell is not ready yet. The window stays open after the
/// subagent ends, as Terminal keeps a finished .command window by default.
#[cfg(target_os = "macos")]
fn open_window(launch: &Launch<'_>, tail: Arc<Mutex<VecDeque<String>>>) -> Result<(Child, JoinHandle<()>, bool), String> {
    let script_file = mark(launch.result_file, "command");
    {
        // Runnable and readable by this user alone: the line may carry the
        // API key. The first line sets the window's title (Terminal's title
        // escape), the second is the shell line.
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&script_file)
            .map_err(|e| py_os_error(&e, &script_file))?;
        writeln!(
            file,
            "#!/bin/sh\nprintf '\\033]0;%s\\007' {}\n{}",
            sh_quote(&window_title(launch)),
            shell_line(launch, "")
        )
        .map_err(|e| py_os_error(&e, &script_file))?;
    }
    let mut cmd = Command::new("open");
    cmd.arg("-a").arg("Terminal").arg(&script_file);
    spawn_opener(cmd, tail).map(|(opener, reader)| (opener, reader, true))
}

/// Linux (and the other unixes): the first terminal emulator found on PATH,
/// the Linux coder's list in its order, runs the shell line with a shell
/// kept after it, so the window stays readable. None found: the piped
/// child, said once.
#[cfg(all(unix, not(target_os = "macos")))]
fn open_window(launch: &Launch<'_>, tail: Arc<Mutex<VecDeque<String>>>) -> Result<(Child, JoinHandle<()>, bool), String> {
    let line = shell_line(launch, "; exec bash");
    let title = window_title(launch);
    let launchers: [(&str, Vec<String>); 6] = [
        ("gnome-terminal", vec![format!("--title={title}"), "--".into(), "bash".into(), "-c".into(), line.clone()]),
        ("ptyxis", vec!["--new-window".into(), "-T".into(), title.clone(), "--".into(), "bash".into(), "-c".into(), line.clone()]),
        ("konsole", vec!["-p".into(), format!("tabtitle={title}"), "-e".into(), "bash".into(), "-c".into(), line.clone()]),
        ("xfce4-terminal", vec![format!("--title={title}"), "-e".into(), format!("bash -c {}", sh_quote(&line))]),
        ("x-terminal-emulator", vec!["-e".into(), "bash".into(), "-c".into(), line.clone()]),
        ("xterm", vec!["-T".into(), title.clone(), "-e".into(), "bash".into(), "-c".into(), line.clone()]),
    ];
    for (binary, args) in launchers {
        if !on_path(binary) {
            continue;
        }
        let mut cmd = Command::new(binary);
        cmd.args(&args);
        match spawn_opener(cmd, Arc::clone(&tail)) {
            Ok((opener, reader)) => return Ok((opener, reader, true)),
            Err(e) => log(&format!("Failed to start {binary} for the subagent: {e}")),
        }
    }
    log("No terminal emulator found to show the subagent: it runs piped");
    spawn_child(launch, tail, false).map(|(child, reader)| (child, reader, false))
}

/// Windows: the subagent itself, in a console window of its own; its handle
/// stays, so it is watched as a piped child.
#[cfg(windows)]
fn open_window(launch: &Launch<'_>, tail: Arc<Mutex<VecDeque<String>>>) -> Result<(Child, JoinHandle<()>, bool), String> {
    spawn_child(launch, tail, true).map(|(child, reader)| (child, reader, false))
}

#[cfg(not(any(unix, windows)))]
fn open_window(launch: &Launch<'_>, tail: Arc<Mutex<VecDeque<String>>>) -> Result<(Child, JoinHandle<()>, bool), String> {
    spawn_child(launch, tail, false).map(|(child, reader)| (child, reader, false))
}

/// Start what opens the window, in a process group of its own (a terminal
/// Ctrl+C here never cuts the opening short), with no stdin, its stdout
/// dropped and its last stderr lines kept for a failure report.
#[cfg(unix)]
fn spawn_opener(mut cmd: Command, tail: Arc<Mutex<VecDeque<String>>>) -> Result<(Child, JoinHandle<()>), String> {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| py_os_error(&e, Path::new(cmd.get_program())))?;
    let Some(stderr) = child.stderr.take() else {
        stop_now(child);
        return Err("its stderr pipe could not be opened".to_string());
    };
    match thread::Builder::new().name("subagent-opener".into()).spawn(move || keep_tail(stderr, &tail)) {
        Ok(reader) => Ok((child, reader)),
        Err(_) => {
            stop_now(child);
            Err("can't start new thread".to_string())
        }
    }
}

/// The line the window's shell runs: into this process's folder, the
/// child's environment as assignments (Terminal.app does not inherit it),
/// the piped child's command line, then its exit code into `<result>.exit`;
/// `after` closes the line (Linux keeps a shell for the window).
#[cfg(unix)]
fn shell_line(launch: &Launch<'_>, after: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| launch.repo_root.to_path_buf());
    let mut words = vec![format!("cd {} &&", sh_quote(&cwd.to_string_lossy()))];
    if let Some(key) = launch.api_key.filter(|key| !key.is_empty()) {
        words.push(format!("{API_KEY_VAR}={}", sh_quote(key)));
    }
    words.push("PYTHONUNBUFFERED=1".to_string());
    words.push(format!("PYTHONPATH={}", sh_quote(&pythonpath(launch.repo_root).to_string_lossy())));
    words.push(sh_quote(launch.executable));
    words.extend(child_args(launch).iter().map(|arg| sh_quote(&arg.to_string_lossy())));
    let exit_file = mark(launch.result_file, "exit");
    format!("{} ; echo $? > {}{after}", words.join(" "), sh_quote(&exit_file.to_string_lossy()))
}

/// "Subagent (<mode>): <the brief's first 60 characters>", quotes made
/// single and line breaks spaces, as the mac coder titled its windows.
#[cfg(unix)]
fn window_title(launch: &Launch<'_>) -> String {
    let brief: String = launch
        .task
        .chars()
        .take(60)
        .map(|c| match c {
            '"' => '\'',
            '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    format!("Subagent ({}): {brief}", launch.mode)
}

/// Python's shlex.quote: as is when every character is a safe ASCII one,
/// else in single quotes.
#[cfg(unix)]
fn sh_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Python's shutil.which: the binary is an executable file in a PATH folder.
#[cfg(all(unix, not(target_os = "macos")))]
fn on_path(binary: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(binary)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Read a pipe to its end and drop what comes (Python passed it on to the
/// app's UI, which a terminal run does not have).
fn drain(mut pipe: impl Read) {
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Read stderr to its end, line by line (UTF-8, bad bytes as U+FFFD), and
/// keep its last non-empty lines for a crash report.
fn keep_tail(pipe: impl Read, tail: &Mutex<VecDeque<String>>) {
    let mut reader = BufReader::new(pipe);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&raw);
                let line = text.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    continue;
                }
                let mut kept = tail.lock().unwrap_or_else(PoisonError::into_inner);
                if kept.len() >= STDERR_TAIL {
                    kept.pop_front();
                }
                kept.push_back(line.to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// One look at a subagent: its report, the text saying it gave none, or
/// None while it is still at work.
fn check_task(py: Python<'_>, task: &mut SubagentTask) -> Option<Result<(Value, Value), String>> {
    if task.window {
        return check_window(py, task);
    }
    if let Some(report) = take_result(task) {
        return Some(Ok(report));
    }
    let Some(child) = task.child.as_mut() else {
        return Some(Err(format!("{NO_RESULT} It failed to start: {}", task.start_error)));
    };
    let status = match child.try_wait() {
        Ok(None) => return None,
        Ok(Some(status)) => status,
        Err(e) => return Some(Err(format!("{NO_RESULT} It could not be watched: {e}"))),
    };
    // The child writes its result before it exits: one last look.
    if let Some(report) = take_result(task) {
        return Some(Ok(report));
    }
    Some(Err(crash_text(py, task, status)))
}

/// One look at a subagent in its own window, which left no handle: its
/// report once it is there (as for a piped child, before its process ends),
/// the text saying it gave none once its process has ended (its window's
/// shell wrote the exit code to `<result>.exit`, or the pid in
/// `<result>.pid` is gone, the window closed), or None while it is at work.
/// Before the pid is there, an opener that failed, or a window that shows
/// no sign of it in time, is the start failure.
fn check_window(py: Python<'_>, task: &mut SubagentTask) -> Option<Result<(Value, Value), String>> {
    if let Some(report) = take_result(task) {
        return Some(Ok(report));
    }
    let exit_file = mark(&task.result_file, "exit");
    let mut code = read_number(&exit_file);
    if code.is_none() {
        match read_pid(&mark(&task.result_file, "pid")) {
            Some(pid) if alive(pid) => return None,
            // Gone: its shell writes the exit code a moment after.
            Some(_) => {
                py.detach(|| thread::sleep(MARK_SETTLE));
                code = read_number(&exit_file);
            }
            None => return not_started(py, task),
        }
    }
    // The child writes its result before it exits: one last look.
    if let Some(report) = take_result(task) {
        return Some(Ok(report));
    }
    Some(Err(match code {
        Some(code) => format!("{NO_RESULT} It exited with code {code}."),
        None => format!("{NO_RESULT} Its process is gone without an exit code (its window closed?)."),
    }))
}

/// Before the window's child has written its pid: None while the opener
/// runs, or ended well and the child is due; the start failure when the
/// opener ended badly (the window never opened: `open` refused, the
/// emulator could not start), or when no pid came START_LIMIT after it
/// ended well (the window was closed before its shell ran the command, or
/// the shell's start-up blocks).
fn not_started(py: Python<'_>, task: &mut SubagentTask) -> Option<Result<(Value, Value), String>> {
    if let Some(launcher) = task.launcher.as_mut() {
        let status = match launcher.try_wait() {
            Ok(None) => return None,
            Ok(Some(status)) => status,
            Err(e) => return Some(Err(format!("{NO_RESULT} Its terminal window could not be watched: {e}"))),
        };
        task.launcher = None;
        if !status.success() {
            let mut text = format!(
                "{NO_RESULT} It failed to start: its terminal window did not open (exit code {}).",
                exit_code(status)
            );
            text.push_str(&stderr_lines(py, task));
            return Some(Err(text));
        }
        task.opened = Instant::now();
    }
    if task.opened.elapsed() < START_LIMIT {
        return None;
    }
    Some(Err(format!(
        "{NO_RESULT} It failed to start: no sign of it in its window after {} s.",
        START_LIMIT.as_secs()
    )))
}

/// `<result>.<ext>`: the files beside the result: the ones a window writes
/// (pid, exit), the mac window's command script (command) and the result's
/// own temp file.
fn mark(result_file: &Path, ext: &str) -> PathBuf {
    let mut path = result_file.as_os_str().to_os_string();
    path.push(".");
    path.push(ext);
    PathBuf::from(path)
}

/// Delete a result's pid and exit marks and its command script, once nobody
/// reads them.
fn remove_marks(result_file: &Path) {
    let _ = std::fs::remove_file(mark(result_file, "pid"));
    let _ = std::fs::remove_file(mark(result_file, "exit"));
    let _ = std::fs::remove_file(mark(result_file, "command"));
}

/// The whole number a mark holds; None while it is not there or not
/// written yet.
fn read_number(path: &Path) -> Option<i64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// The pid a mark holds. Only a positive one: a signal to 0 or below would
/// go to a whole group.
fn read_pid(path: &Path) -> Option<i32> {
    i32::try_from(read_number(path)?).ok().filter(|&pid| pid > 0)
}

/// The report in the result file, once it is there; the file is deleted
/// then, as Python did. A file that does not read is said once.
fn take_result(task: &mut SubagentTask) -> Option<(Value, Value)> {
    if !task.result_file.exists() {
        return None;
    }
    match read_result(&task.result_file) {
        Ok(report) => {
            let _ = std::fs::remove_file(&task.result_file);
            Some(report)
        }
        Err(e) => {
            if !task.read_error_logged {
                log(&format!("Error reading subagent result: {e}"));
                task.read_error_logged = true;
            }
            None
        }
    }
}

/// The result file's summary and status (Python's `.get("summary", "")` and
/// `.get("status", "complete")`), or why it cannot be read.
fn read_result(path: &Path) -> Result<(Value, Value), String> {
    let text = crate::read_text(path).map_err(|e| py_os_error(&e, path))?;
    match serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())? {
        Value::Object(map) => Ok((
            map.get("summary").cloned().unwrap_or_else(|| json!("")),
            map.get("status").cloned().unwrap_or_else(|| json!("complete")),
        )),
        other => Err(format!("'{}' object has no attribute 'get'", py_type_name(&other))),
    }
}

/// Python's type name of a JSON value.
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// The report of a child that exited without a result: the mac text, its
/// exit code and its last stderr lines.
fn crash_text(py: Python<'_>, task: &mut SubagentTask, status: ExitStatus) -> String {
    let mut text = format!("{NO_RESULT} It exited with code {}.", exit_code(status));
    text.push_str(&stderr_lines(py, task));
    text
}

/// "\nLast stderr lines:\n..." from the task's reader, or "" with none.
fn stderr_lines(py: Python<'_>, task: &mut SubagentTask) -> String {
    // Its stderr ends with it: give the reader a moment for the last lines.
    if let Some(reader) = task.stderr_reader.take() {
        let deadline = Instant::now().checked_add(STDERR_SETTLE);
        py.detach(|| {
            while !reader.is_finished() && deadline.is_some_and(|d| Instant::now() < d) {
                thread::sleep(Duration::from_millis(10));
            }
        });
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
    let tail: Vec<String> =
        task.stderr_tail.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect();
    let mut text = String::new();
    if !tail.is_empty() {
        text.push_str("\nLast stderr lines:\n");
        text.push_str(&tail.join("\n"));
    }
    text
}

/// A subagent's call result once it is done (Python's placeholder patch);
/// a "partial" report is an error, as on the mac. `mode` says which kind of
/// subagent answered.
fn subagent_report(query: Value, mode: &str, completion: Option<&Completion>) -> Value {
    match completion.map(|c| &c.report) {
        Some(Ok((summary, status))) => json!({
            "status": if status.as_str() == Some("complete") { "success" } else { "error" },
            "action": "subagent_completed",
            "mode": mode,
            "query": query,
            "output": summary,
        }),
        Some(Err(text)) => json!({
            "status": "error",
            "action": "subagent_completed",
            "mode": mode,
            "query": query,
            "output": text,
        }),
        None => json!({
            "status": "error",
            "action": "subagent_completed",
            "mode": mode,
            "query": query,
            "output": NO_RESULT,
        }),
    }
}

/// Popen's returncode: the exit status, or minus the signal that ended it.
#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i64 {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(code) => i64::from(code),
        None => status.signal().map_or(-1, |signal| -i64::from(signal)),
    }
}

/// Popen's returncode on Windows: the exit code as an unsigned number.
#[cfg(windows)]
fn exit_code(status: ExitStatus) -> i64 {
    status.code().map_or(-1, |code| i64::from(code as u32))
}

#[cfg(not(any(unix, windows)))]
fn exit_code(status: ExitStatus) -> i64 {
    status.code().map_or(-1, i64::from)
}

/// Ask a running child to stop. On unix that is SIGINT, the Ctrl+C a
/// terminal sends: the child's KeyboardInterrupt kills its running command,
/// writes its partial result and exits. A Windows child without a console
/// gets no such signal, so its tree is ended there.
#[cfg(unix)]
fn ask_to_stop(child: &Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill only sends a signal. The caller just saw the child
        // still running and it is not reaped, so the pid is still its own.
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
    }
}

#[cfg(windows)]
fn ask_to_stop(child: &Child) {
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

#[cfg(not(any(unix, windows)))]
fn ask_to_stop(_child: &Child) {}

// A child in a window (mac and Linux) is reached by the pid it wrote, the
// way its handle would be: asked to stop with SIGINT, killed after the grace,
// looked at with signal 0. On Windows it keeps a handle, and these are never
// reached.

/// Whether the process is still there (or a zombie not yet reaped).
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; it only asks whether pid exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(unix)]
fn ask_pid_to_stop(pid: i32) {
    // SAFETY: kill only sends a signal, and read_pid gave a positive pid.
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
}

#[cfg(unix)]
fn kill_pid(pid: i32) {
    // SAFETY: as above.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn alive(_pid: i32) -> bool {
    false
}

#[cfg(not(unix))]
fn ask_pid_to_stop(_pid: i32) {}

#[cfg(not(unix))]
fn kill_pid(_pid: i32) {}

/// Wait up to `limit` for the children to exit; the ones that did are
/// reaped and dropped from the list, the pids gone from theirs.
fn wait_for_exit(children: &mut Vec<Child>, pids: &mut Vec<i32>, limit: Duration) {
    let deadline = Instant::now().checked_add(limit);
    while !(children.is_empty() && pids.is_empty()) && deadline.is_some_and(|d| Instant::now() < d) {
        thread::sleep(Duration::from_millis(50));
        children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        pids.retain(|&pid| alive(pid));
    }
}

/// Kill a child at once and reap it in the background.
fn stop_now(mut child: Child) {
    let _ = child.kill();
    reap(child);
}

/// Wait for a child on a thread of its own, so no zombie is left.
fn reap(mut child: Child) {
    let _ = thread::Builder::new().name("subagent-reaper".into()).spawn(move || {
        let _ = child.wait();
    });
}

/// The first `n` characters of Python's uuid.uuid4().hex.
fn uuid_hex(py: Python<'_>, n: usize) -> PyResult<String> {
    let hex: String = py.import("uuid")?.call_method0("uuid4")?.getattr("hex")?.extract()?;
    Ok(hex.chars().take(n).collect())
}

/// The folder that holds the AutoCua package (web_agent.py's _REPO_ROOT).
fn repo_root(py: Python<'_>) -> PyResult<PathBuf> {
    let file = py.import("AutoCua")?.getattr("__file__")?;
    py.import("pathlib")?
        .getattr("Path")?
        .call1((file,))?
        .call_method0("resolve")?
        .getattr("parent")?
        .getattr("parent")?
        .extract()
}

// ---------------------------------------------------------------------------
// Arguments and Python's number rules
// ---------------------------------------------------------------------------

/// `action_item.get(key, default)` for a text argument (any other value as
/// Python's str() of it).
fn arg_str(item: &Value, key: &str, default: &str) -> String {
    match item.get(key) {
        None => default.to_string(),
        Some(value) => py_str_of(value),
    }
}

/// `action_item.get(key, default)` for a whole-number argument. The registry
/// always sends an int here; anything else reads as a number when it is
/// one, else as the default.
fn arg_int(item: &Value, key: &str, default: i64) -> i64 {
    match item.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).unwrap_or(default),
        Some(Value::Bool(b)) => i64::from(*b),
        Some(Value::String(s)) => crate::py_strip(s).parse().unwrap_or(default),
        _ => default,
    }
}

/// `action_item.get(key, default)` for a flag, by Python truthiness.
fn arg_bool(item: &Value, key: &str, default: bool) -> bool {
    item.get(key).map_or(default, truthy)
}

/// Python's repr() of a JSON value, as the mac's log line prints an item.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let pairs: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v))).collect();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}

/// Python's `float(raw)`; on a value that is not a number, the first number
/// in it ("2s" waits 2 s). Err: Python's error text, when there is none.
fn py_float(py: Python<'_>, raw: &Value) -> PyResult<Result<f64, String>> {
    let failure = match py_builtin(py, "float", raw) {
        Ok(number) => return Ok(Ok(number.extract()?)),
        Err(e) if e.is_instance_of::<PyException>(py) => e,
        Err(e) => return Err(e),
    };
    if let Some(found) = raw.as_str().and_then(first_number) {
        match py_builtin(py, "float", &json!(found)) {
            Ok(number) => return Ok(Ok(number.extract()?)),
            Err(e) if !e.is_instance_of::<PyException>(py) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(Err(py_err_str(py, &failure)))
}

/// Python's `int(raw)`, as the number and its text; on a value that is not
/// a whole number, the first digits in it ("#3" and "task number #3" give
/// 3). A number past 64 bits is held at the nearest end (its text stays
/// exact). Err: Python's error text, when there are no digits.
fn py_int(py: Python<'_>, raw: &Value) -> PyResult<Result<(i64, String), String>> {
    let failure = match py_builtin(py, "int", raw) {
        Ok(number) => return Ok(Ok(int_parts(&number)?)),
        Err(e) if e.is_instance_of::<PyException>(py) => e,
        Err(e) => return Err(e),
    };
    if let Some(found) = raw.as_str().and_then(first_digits) {
        match py_builtin(py, "int", &json!(found)) {
            Ok(number) => return Ok(Ok(int_parts(&number)?)),
            Err(e) if !e.is_instance_of::<PyException>(py) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(Err(py_err_str(py, &failure)))
}

/// A Python int as an i64 (held at i64::MAX or MIN past 64 bits) and its str().
fn int_parts(number: &Bound<'_, PyAny>) -> PyResult<(i64, String)> {
    let text = number.str()?.to_string_lossy().into_owned();
    let value = match number.extract::<i64>() {
        Ok(value) => value,
        Err(_) if number.gt(0)? => i64::MAX,
        Err(_) => i64::MIN,
    };
    Ok((value, text))
}

/// A Python builtin (int or float) on a JSON value, for its exact rules
/// (spaces, "_", "+", Unicode digits, "inf") and its error text.
fn py_builtin<'py>(py: Python<'py>, name: &str, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    let arg = pythonize::pythonize(py, value)?;
    py.import("builtins")?.getattr(name)?.call1((arg,))
}

/// The first run of digits in a text.
fn first_digits(text: &str) -> Option<&str> {
    static DIGITS: OnceLock<Option<Regex>> = OnceLock::new();
    let re = DIGITS.get_or_init(|| Regex::new(r"\d+").ok()).as_ref()?;
    re.find(text).map(|m| m.as_str())
}

/// The first number in a text: digits with an optional decimal part.
fn first_number(text: &str) -> Option<&str> {
    static NUMBER: OnceLock<Option<Regex>> = OnceLock::new();
    let re = NUMBER.get_or_init(|| Regex::new(r"\d+(?:\.\d+)?|\.\d+").ok()).as_ref()?;
    re.find(text).map(|m| m.as_str())
}

/// Sleep without the GIL, in short slices, with Ctrl+C seen at each one.
fn sleep_checked(py: Python<'_>, seconds: f64) -> PyResult<()> {
    let Ok(total) = Duration::try_from_secs_f64(seconds) else {
        return Ok(());
    };
    let Some(deadline) = Instant::now().checked_add(total) else {
        return Ok(());
    };
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        py.detach(|| thread::sleep(left.min(SLICE)));
        py.check_signals()?;
    }
}
