//! web_agent.py: the browser-agent fallback for the `web` tool. Providers
//! with no native web-search API (together, cerebras, aws) still have to
//! honour the `web` action, so the query goes to the browser agent
//! (AutoCua/web, the Rust/CDP Chrome driver) running on the SAME provider
//! and model; its `done` summary is the web result. The task text below is
//! what turns that summary into a proper report rather than a one-line
//! activity log.
//!
//! The browser agent runs as a child process rather than in this one: the
//! web AgentService deletes its working folder's conversation/, debug/ and
//! raw_reasoning/, so the child gets its own folder; it owns a headless
//! Chrome on a dedicated port, never a visible one a `web use` run may have
//! left open on the default port; and a stop or a timeout only has to end a
//! process. It is started through Python's subprocess.Popen as on iOS, so
//! stopping it sends the same signal on every OS and its exit code reads the
//! same.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pyo3::exceptions::{PyException, PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PySlice, PyString};
use serde_json::json;

use super::service::{kwargs, path_str, text_of};

/// How often the browser agent is checked on.
const POLL: Duration = Duration::from_millis(500);

/// A `done` value shorter than this is almost certainly an activity log, not
/// the answer: it is topped up with what the agent recorded along the way.
const SHORT_REPORT_CHARS: usize = 200;

/// The runtime (frontend) key reaches the browser agent through its env: the
/// web agent's __main__ has no --api_key flag, and its .env loader never
/// overrides a var that is already set.
const PROVIDER_ENV_KEY: [(&str, &str); 3] = [
    ("together", "TOGETHER_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("aws", "AWS_API_KEY"),
];

/// The browser agent's task, `{query}` filled in.
const TASK_TEMPLATE: &str = r##"Research the following on the web and deliver the answer as a written report.

<research_request>
{query}
</research_request>

How to work:
- Open a search engine via `update_tab` (https://www.google.com — if it shows a consent/captcha wall use https://duckduckgo.com or https://www.bing.com) and visit several credible sources: official sites, primary documents, reputable news/reference sites. Cross-check key facts across at least two sources when possible.
- Record every key finding (numbers, dates, names, exact URLs) in the scratchpad as you go so nothing is lost.
- Read only: never log in, sign up, buy, download, or change anything.

How to finish — THIS IS THE DELIVERABLE:
- Your final `done` call's value MUST BE THE ANSWER ITSELF as a detailed, well-formatted Markdown report — NOT a summary of the actions you took.
- Structure: a title line, `##` headings and `###` subheadings per sub-topic, `-` bullets, and the key numbers/facts/quotes stated explicitly. Go in depth where the request needs it; keep it brief where it doesn't.
- End with a `## Sources` section listing the URLs you actually used.
- If something could not be found, say so explicitly instead of guessing.
"##;

/// The browser agent did not produce a report. `note` is the short reason
/// for the tool result's Note: line; the message carries the detail.
pub struct WebAgentSearchError {
    pub note: String,
    detail: String,
}

impl WebAgentSearchError {
    fn new(note: impl Into<String>, detail: impl Into<String>) -> Self {
        WebAgentSearchError { note: note.into(), detail: detail.into() }
    }

    /// str(e): the detail, or the note when there is none.
    pub fn message(&self) -> &str {
        if self.detail.is_empty() { &self.note } else { &self.detail }
    }
}

/// Why web_search gave no report: the browser agent's own reason, or a
/// Python error (anything else that went wrong, reported as a failed start).
pub enum Failure {
    Search(WebAgentSearchError),
    Py(PyErr),
}

impl From<PyErr> for Failure {
    fn from(e: PyErr) -> Self {
        Failure::Py(e)
    }
}

/// web_agent.py's _REPO_ROOT: the folder holding the AutoCua package.
fn repo_root(py: Python<'_>) -> PyResult<PathBuf> {
    let file = py.import("AutoCua")?.getattr("__file__")?;
    let root = py
        .import("pathlib")?
        .getattr("Path")?
        .call1((file,))?
        .call_method0("resolve")?
        .getattr("parent")?
        .getattr("parent")?;
    Ok(PathBuf::from(text_of(&root)?))
}

/// Run the browser agent on `query` and return its report (Markdown), or
/// why there is none: stopped by the user, timed out, the child crashed, or
/// it finished without `done`.
pub fn web_search(
    py: Python<'_>,
    query: &str,
    provider: &str,
    model: &str,
    api_key: Option<&str>,
    stop_event: Option<&Py<PyAny>>,
) -> Result<String, Failure> {
    if py.import("AutoCua")?.getattr("IS_COMPILED")?.is_truthy()? {
        return Err(Failure::Search(WebAgentSearchError::new(
            "browser-agent web search is unavailable in the packaged build",
            "",
        )));
    }

    // A dedicated Chrome port: launching ATTACHES to whatever already runs
    // on a port and then ignores --headless, and Chrome stays up after a
    // `web use` run, so the default 9222 could be a visible window. A
    // dedicated profile too: Chrome given a profile that a running instance
    // already holds hands the launch to that instance. The child has its
    // own 100-step cap; the timeout bounds the wall clock.
    let os = py.import("os")?;
    let environ = os.getattr("environ")?;
    let int = py.import("builtins")?.getattr("int")?;
    let browser_port = int.call1((environ.call_method1("get", ("AutoCua_WEB_FALLBACK_PORT", "9333"))?,))?;
    let browser_profile = environ.call_method1("get", ("AutoCua_WEB_FALLBACK_PROFILE", "web_fallback"))?;
    let timeout_sec: i64 = int
        .call1((environ.call_method1("get", ("AutoCua_WEB_FALLBACK_TIMEOUT", "900"))?,))?
        .extract()?;
    let repo_root = repo_root(py)?;
    let web_scratchpad = repo_root.join("AutoCua").join("web").join("scratchpad");

    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let hex: String = py.import("uuid")?.call_method0("uuid4")?.getattr("hex")?.extract()?;
    let sid = format!("web_{secs}_{}", &hex[..6]);
    let run_dir = crate::data_root(py)?.join("web_fallback").join(&sid);
    // Through pathlib, as on iOS: a folder that cannot be made fails with
    // Python's error, which names it.
    py.import("pathlib")?
        .getattr("Path")?
        .call1((path_str(&run_dir),))?
        .call_method("mkdir", (), Some(&kwargs(py, json!({"parents": true, "exist_ok": true}))?))?;
    let result_file = run_dir.join("result.json");
    let log_file = run_dir.join("agent.log");

    let s = |text: &str| PyString::new(py, text).into_any();
    let cmd = PyList::new(
        py,
        [
            py.import("sys")?.getattr("executable")?,
            s("-m"),
            s("AutoCua.web.agent"),
            s("--task"),
            s(&TASK_TEMPLATE.replace("{query}", query)),
            s("--provider"),
            s(provider),
            s("--model"),
            s(model),
            s("--result"),
            s(&path_str(&result_file)),
            s("--headless"),
            s("--session-id"),
            s(&sid),
            s("--browser-port"),
            browser_port.str()?.into_any(),
            s("--browser-profile"),
            browser_profile,
        ],
    )?;
    let env = environ.call_method0("copy")?;
    env.set_item("PYTHONUNBUFFERED", "1")?;
    // The child's cwd is outside the repo, so make the package importable.
    let pythonpath = format!(
        "{}{}{}",
        path_str(&repo_root),
        text_of(&os.getattr("pathsep")?)?,
        text_of(&env.call_method1("get", ("PYTHONPATH", ""))?)?
    );
    env.set_item("PYTHONPATH", pythonpath)?;
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        if let Some((_, var)) = PROVIDER_ENV_KEY.iter().find(|(name, _)| *name == provider) {
            env.set_item(*var, key)?;
        }
    }

    println!(
        "\n🌐 web: {provider} has no native search — delegating to the headless browser agent ({model}); log: {}",
        log_file.display()
    );

    let outcome = run_web_agent(py, &cmd, &run_dir, &env, &log_file, &result_file, &web_scratchpad, &sid, stop_event, timeout_sec);
    // The run's own scratchpad is spent; result.json + agent.log stay in
    // run_dir for inspection.
    let _ = std::fs::remove_dir_all(web_scratchpad.join(&sid));
    outcome
}

/// Start the browser agent, wait for it, and read its result.json.
#[allow(clippy::too_many_arguments)]
fn run_web_agent(
    py: Python<'_>,
    cmd: &Bound<'_, PyList>,
    run_dir: &Path,
    env: &Bound<'_, PyAny>,
    log_file: &Path,
    result_file: &Path,
    web_scratchpad: &Path,
    sid: &str,
    stop_event: Option<&Py<PyAny>>,
    timeout_sec: i64,
) -> Result<String, Failure> {
    // stdout/stderr go to a file: the browser agent prints a lot, and a pipe
    // nobody drains would deadlock it. Deliberately no new session: the
    // child stays in this process group, so a Stop of the parent takes it
    // down too.
    let log = py.import("builtins")?.call_method1("open", (path_str(log_file), "ab"))?;
    let waited = wait_web_agent(py, cmd, run_dir, env, &log, web_scratchpad, sid, stop_event, timeout_sec);
    let _ = log.call_method0("close");
    let proc = waited?;

    let read = (|| -> PyResult<Bound<'_, PyAny>> {
        let utf8 = PyDict::new(py);
        utf8.set_item("encoding", "utf-8")?;
        let text = py
            .import("pathlib")?
            .getattr("Path")?
            .call1((path_str(result_file),))?
            .call_method("read_text", (), Some(&utf8))?;
        py.import("json")?.call_method1("loads", (text,))
    })();
    let result = match read {
        Ok(result) => result,
        Err(e) if e.is_instance_of::<PyOSError>(py) || e.is_instance_of::<PyValueError>(py) => {
            return Err(Failure::Search(WebAgentSearchError::new(
                format!("browser agent exited with code {} without a result", text_of(&proc.getattr("returncode")?)?),
                tail(py, log_file)?,
            )));
        }
        Err(e) => return Err(e.into()),
    };

    let status = result.call_method1("get", ("status",))?;
    let status = if status.is_truthy()? { text_of(&status)? } else { "error".to_string() };
    let message = result.call_method1("get", ("message",))?;
    let message = if message.is_truthy()? { message.str()? } else { PyString::new(py, "") };
    let mut message = text_of(&message.call_method0("strip")?)?;
    if status == "success" && !message.is_empty() {
        if message.chars().count() < SHORT_REPORT_CHARS {
            let notes = milestone_notes(py, web_scratchpad, sid)?;
            if !notes.is_empty() {
                message.push_str("\n\nFindings recorded during the run:\n");
                message.push_str(&notes);
            }
        }
        return Ok(message);
    }

    let notes = milestone_notes(py, web_scratchpad, sid)?;
    let shown = if message.is_empty() { "no message" } else { message.as_str() };
    let mut detail = shown.to_string();
    if !notes.is_empty() {
        detail.push_str("\n\nFindings recorded before it stopped:\n");
        detail.push_str(&notes);
    }
    Err(Failure::Search(WebAgentSearchError::new(format!("browser agent did not finish ({status}): {shown}"), detail)))
}

/// Popen the browser agent and poll it until it exits, the user stops the
/// task, or the timeout runs out (both of which end it). The process, once
/// it has exited.
#[allow(clippy::too_many_arguments)]
fn wait_web_agent<'py>(
    py: Python<'py>,
    cmd: &Bound<'py, PyList>,
    run_dir: &Path,
    env: &Bound<'py, PyAny>,
    log: &Bound<'py, PyAny>,
    web_scratchpad: &Path,
    sid: &str,
    stop_event: Option<&Py<PyAny>>,
    timeout_sec: i64,
) -> Result<Bound<'py, PyAny>, Failure> {
    let subprocess = py.import("subprocess")?;
    let popen = PyDict::new(py);
    popen.set_item("cwd", path_str(run_dir))?;
    popen.set_item("env", env)?;
    popen.set_item("stdin", subprocess.getattr("DEVNULL")?)?;
    popen.set_item("stdout", log)?;
    popen.set_item("stderr", subprocess.getattr("STDOUT")?)?;
    let proc = subprocess.getattr("Popen")?.call((cmd,), Some(&popen))?;

    // A timeout too large for the clock never runs out, as on iOS.
    let deadline = Instant::now().checked_add(Duration::from_secs(timeout_sec.max(0) as u64));
    while proc.call_method0("poll")?.is_none() {
        if let Some(event) = stop_event {
            if event.bind(py).call_method0("is_set")?.is_truthy()? {
                terminate(py, &proc)?;
                return Err(Failure::Search(WebAgentSearchError::new("Stopped by user", "")));
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() > deadline) {
            terminate(py, &proc)?;
            let notes = milestone_notes(py, web_scratchpad, sid)?;
            let detail = if notes.is_empty() { "no findings were recorded before the timeout".to_string() } else { notes };
            return Err(Failure::Search(WebAgentSearchError::new(
                format!("browser agent timed out after {} min", timeout_sec.div_euclid(60)),
                detail,
            )));
        }
        py.detach(|| std::thread::sleep(POLL));
        py.check_signals()?;
    }
    Ok(proc)
}

/// End the browser agent: terminate() (SIGTERM; TerminateProcess on
/// Windows), five seconds to go, then kill(). Other failures are let go.
fn terminate(py: Python<'_>, proc: &Bound<'_, PyAny>) -> PyResult<()> {
    if !proc.call_method0("poll")?.is_none() {
        return Ok(());
    }
    let ended = (|| -> PyResult<()> {
        proc.call_method0("terminate")?;
        let five = PyDict::new(py);
        five.set_item("timeout", 5)?;
        proc.call_method("wait", (), Some(&five))?;
        Ok(())
    })();
    match ended {
        Ok(()) => Ok(()),
        Err(e) if e.is_instance(py, &py.import("subprocess")?.getattr("TimeoutExpired")?) => {
            proc.call_method0("kill")?;
            proc.call_method0("wait")?;
            Ok(())
        }
        Err(e) if e.is_instance_of::<PyException>(py) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The last 30 lines of the browser agent's log, "" when it can't be read.
fn tail(py: Python<'_>, path: &Path) -> PyResult<String> {
    let lines = (|| -> PyResult<Bound<'_, PyAny>> {
        let read = PyDict::new(py);
        read.set_item("encoding", "utf-8")?;
        read.set_item("errors", "replace")?;
        let text = py
            .import("pathlib")?
            .getattr("Path")?
            .call1((path_str(path),))?
            .call_method("read_text", (), Some(&read))?;
        let last = text.call_method0("splitlines")?.get_item(PySlice::new(py, -30, isize::MAX, 1))?;
        PyString::new(py, "\n").call_method1("join", (last,))
    })();
    match lines {
        Ok(joined) => text_of(&joined),
        Err(e) if e.is_instance_of::<PyOSError>(py) => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// What the browser agent wrote to its scratchpad during the run: the
/// salvage when the `done` value is thin or the run never reached `done`.
fn milestone_notes(py: Python<'_>, web_scratchpad: &Path, sid: &str) -> PyResult<String> {
    let p = web_scratchpad.join(sid).join("milestone").join("milestone.md");
    let notes = (|| -> PyResult<String> {
        let path = py.import("pathlib")?.getattr("Path")?.call1((path_str(&p),))?;
        if !path.call_method0("exists")?.is_truthy()? {
            return Ok(String::new());
        }
        let utf8 = PyDict::new(py);
        utf8.set_item("encoding", "utf-8")?;
        text_of(&path.call_method("read_text", (), Some(&utf8))?.call_method0("strip")?)
    })();
    match notes {
        Err(e) if e.is_instance_of::<PyOSError>(py) => Ok(String::new()),
        other => other,
    }
}
