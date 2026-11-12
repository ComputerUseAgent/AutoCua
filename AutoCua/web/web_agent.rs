//! web_agent.py: the browser-agent fallback for the `web` tool. Providers
//! with no native web-search API (together, cerebras, requesty, aws, ollama)
//! still have to honour the `web` action, so the query goes to the browser
//! agent (AutoCua/browser, the Rust/CDP Chrome driver) running on the SAME
//! provider and model; its `done` summary is the web result. The task text
//! below is what turns that summary into a proper report rather than a
//! one-line activity log.
//!
//! The browser agent runs as a child process rather than in this one: the
//! browser agent's AgentService deletes its working folder's conversation/,
//! debug/ and raw_reasoning/, so the child gets its own folder; it owns a
//! headless Chrome on a dedicated port, never a visible one a `web use` run
//! may have left open on the default port; and a stop or a timeout only has
//! to end a process. It is started through Python's subprocess.Popen as on
//! the Mac, so stopping it sends the same signal on every OS and its exit
//! code reads the same. The paths are Python's pathlib paths and the clock
//! is Python's time module, as on the Mac. Ctrl+C while it runs ends the
//! child first, then flies on.

use pyo3::exceptions::{PyException, PyKeyboardInterrupt, PyOSError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyList, PySlice, PyString};
use serde_json::json;

use super::service::{fill, kwargs, print_line, str_of};

/// How often the browser agent is checked on, in seconds.
const POLL_SEC: f64 = 0.5;

/// A `done` value shorter than this is almost certainly an activity log, not
/// the answer: it is topped up with what the agent recorded along the way.
const SHORT_REPORT_CHARS: usize = 200;

/// The runtime (frontend) key reaches the browser agent through its env: the
/// browser agent's __main__ has no --api_key flag, and its .env loader never
/// overrides a var that is already set.
const PROVIDER_ENV_KEY: [(&str, &str); 5] = [
    ("together", "TOGETHER_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("requesty", "REQUESTY_API_KEY"),
    ("aws", "AWS_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
];

/// The browser agent's task: str.format fills in `{query}`.
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
/// for the tool result's Note: line; the message carries the detail. Both
/// are Python str.
pub struct WebAgentSearchError {
    pub note: Py<PyAny>,
    detail: Py<PyAny>,
}

impl WebAgentSearchError {
    fn new(note: Bound<'_, PyAny>, detail: Bound<'_, PyAny>) -> Self {
        WebAgentSearchError { note: note.unbind(), detail: detail.unbind() }
    }

    /// One of the fixed reasons, with no detail.
    fn plain(py: Python<'_>, note: &str) -> Self {
        Self::new(PyString::new(py, note).into_any(), PyString::new(py, "").into_any())
    }

    /// str(e): the detail, or the note when there is none.
    pub fn message<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let detail = self.detail.bind(py);
        Ok(if detail.is_truthy()? { detail.clone() } else { self.note.bind(py).clone() })
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

/// web_agent.py's _REPO_ROOT, a pathlib path: the folder holding the AutoCua
/// package.
fn repo_root<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    let file = py.import("AutoCua")?.getattr("__file__")?;
    py.import("pathlib")?
        .getattr("Path")?
        .call1((file,))?
        .call_method0("resolve")?
        .getattr("parent")?
        .getattr("parent")
}

/// Run the browser agent on `query` and return its report (Markdown, a
/// Python str), or why there is none: stopped by the user, timed out, the
/// child crashed, or it finished without `done`.
pub fn web_search<'py>(
    py: Python<'py>,
    query: &Bound<'py, PyAny>,
    provider: &Bound<'py, PyAny>,
    model: &Bound<'py, PyAny>,
    api_key: &Bound<'py, PyAny>,
    stop_event: &Bound<'py, PyAny>,
) -> Result<Bound<'py, PyAny>, Failure> {
    let autocua = py.import("AutoCua")?;
    if autocua.getattr("IS_COMPILED")?.is_truthy()? {
        return Err(Failure::Search(WebAgentSearchError::plain(
            py,
            "browser-agent web search is unavailable in the packaged build",
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
    let int = py.get_type::<PyInt>();
    let browser_port = int.call1((environ.call_method1("get", ("AutoCua_WEB_FALLBACK_PORT", "9333"))?,))?;
    let browser_profile = environ.call_method1("get", ("AutoCua_WEB_FALLBACK_PROFILE", "web_fallback"))?;
    let timeout_sec = int.call1((environ.call_method1("get", ("AutoCua_WEB_FALLBACK_TIMEOUT", "900"))?,))?;
    let repo_root = repo_root(py)?;
    let web_scratchpad = repo_root.div("AutoCua")?.div("browser")?.div("scratchpad")?;

    let time = py.import("time")?;
    let secs = int.call1((time.call_method0("time")?,))?;
    let hex = py
        .import("uuid")?
        .call_method0("uuid4")?
        .getattr("hex")?
        .get_item(PySlice::new(py, 0, 6, 1))?;
    let sid = fill(py, "web_{}_{}", (secs, hex))?;
    let run_dir = autocua.call_method0("data_root")?.div("web_fallback")?.div(&sid)?;
    run_dir.call_method("mkdir", (), Some(&kwargs(py, json!({"parents": true, "exist_ok": true}))?))?;
    let result_file = run_dir.div("result.json")?;
    let log_file = run_dir.div("agent.log")?;

    let task = PyString::new(py, TASK_TEMPLATE).call_method("format", (), Some(&{
        let fields = PyDict::new(py);
        fields.set_item("query", query)?;
        fields
    }))?;
    let s = |text: &str| PyString::new(py, text).into_any();
    let cmd = PyList::new(
        py,
        [
            py.import("sys")?.getattr("executable")?,
            s("-m"),
            s("AutoCua.browser.agent"),
            s("--task"),
            task,
            s("--provider"),
            provider.clone(),
            s("--model"),
            model.clone(),
            s("--result"),
            str_of(&result_file)?,
            s("--headless"),
            s("--session-id"),
            sid.clone(),
            s("--browser-port"),
            str_of(&browser_port)?,
            s("--browser-profile"),
            browser_profile,
        ],
    )?;
    let env = environ.call_method0("copy")?;
    env.set_item("PYTHONUNBUFFERED", "1")?;
    // The child's cwd is outside the repo, so make the package importable.
    let pythonpath = str_of(&repo_root)?
        .add(os.getattr("pathsep")?)?
        .add(env.call_method1("get", ("PYTHONPATH", ""))?)?;
    env.set_item("PYTHONPATH", pythonpath)?;
    if api_key.is_truthy()? {
        let named = provider.cast::<PyString>().ok().and_then(|p| p.to_str().ok());
        if let Some((_, var)) = PROVIDER_ENV_KEY.iter().find(|(name, _)| Some(*name) == named) {
            env.set_item(*var, api_key)?;
        }
    }

    print_line(
        py,
        &fill(
            py,
            "\n🌐 web: {} has no native search — delegating to the headless browser agent ({}); log: {}",
            (provider, model, &log_file),
        )?,
    )?;

    let outcome = run_web_agent(py, &cmd, &run_dir, &env, &log_file, &result_file, &web_scratchpad, &sid, stop_event, &timeout_sec);
    // finally: the run's own scratchpad is spent; result.json + agent.log
    // stay in run_dir for inspection.
    py.import("shutil")?.call_method(
        "rmtree",
        (web_scratchpad.div(&sid)?,),
        Some(&kwargs(py, json!({"ignore_errors": true}))?),
    )?;
    outcome
}

/// Start the browser agent, wait for it, and read its result.json.
#[allow(clippy::too_many_arguments)]
fn run_web_agent<'py>(
    py: Python<'py>,
    cmd: &Bound<'py, PyList>,
    run_dir: &Bound<'py, PyAny>,
    env: &Bound<'py, PyAny>,
    log_file: &Bound<'py, PyAny>,
    result_file: &Bound<'py, PyAny>,
    web_scratchpad: &Bound<'py, PyAny>,
    sid: &Bound<'py, PyAny>,
    stop_event: &Bound<'py, PyAny>,
    timeout_sec: &Bound<'py, PyAny>,
) -> Result<Bound<'py, PyAny>, Failure> {
    // stdout/stderr go to a file: the browser agent prints a lot, and a pipe
    // nobody drains would deadlock it. Deliberately no new session: the
    // child stays in this process group, so a Stop of the parent takes it
    // down too. `with open(log_file, "ab") as log:` closes the file however
    // the wait ends.
    let log = py.import("builtins")?.getattr("open")?.call1((log_file, "ab"))?;
    let waited = wait_web_agent(py, cmd, run_dir, env, &log, web_scratchpad, sid, stop_event, timeout_sec);
    let closed = log.call_method0("close");
    let proc = match (waited, closed) {
        (_, Err(e)) => return Err(e.into()),
        (waited, Ok(_)) => waited?,
    };

    let read = (|| -> PyResult<Bound<'py, PyAny>> {
        let text = result_file.call_method("read_text", (), Some(&kwargs(py, json!({"encoding": "utf-8"}))?))?;
        py.import("json")?.call_method1("loads", (text,))
    })();
    let result = match read {
        Ok(result) => result,
        Err(e) if e.is_instance_of::<PyOSError>(py) || e.is_instance_of::<PyValueError>(py) => {
            let note = fill(py, "browser agent exited with code {} without a result", (proc.getattr("returncode")?,))?;
            return Err(Failure::Search(WebAgentSearchError::new(note, tail(py, log_file)?)));
        }
        Err(e) => return Err(e.into()),
    };

    // str(result.get("status") or "error"), str(result.get("message") or "").strip()
    let status = result.call_method1("get", ("status",))?;
    let status = if status.is_truthy()? { str_of(&status)? } else { PyString::new(py, "error").into_any() };
    let message = result.call_method1("get", ("message",))?;
    let message = if message.is_truthy()? { message } else { PyString::new(py, "").into_any() };
    let mut message = str_of(&message)?.call_method0("strip")?;
    if status.eq("success")? && message.is_truthy()? {
        if message.len()? < SHORT_REPORT_CHARS {
            let notes = milestone_notes(py, web_scratchpad, sid)?;
            if notes.is_truthy()? {
                message = message.add(PyString::new(py, "\n\nFindings recorded during the run:\n").add(&notes)?)?;
            }
        }
        return Ok(message);
    }

    let notes = milestone_notes(py, web_scratchpad, sid)?;
    let shown = if message.is_truthy()? { message.clone() } else { PyString::new(py, "no message").into_any() };
    let mut detail = shown.clone();
    if notes.is_truthy()? {
        detail = detail.add(PyString::new(py, "\n\nFindings recorded before it stopped:\n").add(&notes)?)?;
    }
    let note = fill(py, "browser agent did not finish ({}): {}", (status, shown))?;
    Err(Failure::Search(WebAgentSearchError::new(note, detail)))
}

/// Popen the browser agent and poll it until it exits, the user stops the
/// task, or the timeout runs out (both of which end it). The process, once
/// it has exited. Ctrl+C ends it too, then flies on.
#[allow(clippy::too_many_arguments)]
fn wait_web_agent<'py>(
    py: Python<'py>,
    cmd: &Bound<'py, PyList>,
    run_dir: &Bound<'py, PyAny>,
    env: &Bound<'py, PyAny>,
    log: &Bound<'py, PyAny>,
    web_scratchpad: &Bound<'py, PyAny>,
    sid: &Bound<'py, PyAny>,
    stop_event: &Bound<'py, PyAny>,
    timeout_sec: &Bound<'py, PyAny>,
) -> Result<Bound<'py, PyAny>, Failure> {
    let subprocess = py.import("subprocess")?;
    let popen = PyDict::new(py);
    popen.set_item("cwd", str_of(run_dir)?)?;
    popen.set_item("env", env)?;
    popen.set_item("stdin", subprocess.getattr("DEVNULL")?)?;
    popen.set_item("stdout", log)?;
    popen.set_item("stderr", subprocess.getattr("STDOUT")?)?;
    let proc = subprocess.getattr("Popen")?.call((cmd,), Some(&popen))?;

    let waited = (|| -> Result<(), Failure> {
        let time = py.import("time")?;
        let deadline = time.call_method0("monotonic")?.add(timeout_sec)?;
        while proc.call_method0("poll")?.is_none() {
            if !stop_event.is_none() && stop_event.call_method0("is_set")?.is_truthy()? {
                terminate(py, &proc)?;
                return Err(Failure::Search(WebAgentSearchError::plain(py, "Stopped by user")));
            }
            if time.call_method0("monotonic")?.gt(&deadline)? {
                terminate(py, &proc)?;
                let note = fill(py, "browser agent timed out after {} min", (timeout_sec.floor_div(60)?,))?;
                let notes = milestone_notes(py, web_scratchpad, sid)?;
                let detail = if notes.is_truthy()? {
                    notes
                } else {
                    PyString::new(py, "no findings were recorded before the timeout").into_any()
                };
                return Err(Failure::Search(WebAgentSearchError::new(note, detail)));
            }
            time.call_method1("sleep", (POLL_SEC,))?;
        }
        Ok(())
    })();
    match waited {
        Ok(()) => Ok(proc),
        Err(Failure::Py(interrupt)) if interrupt.is_instance_of::<PyKeyboardInterrupt>(py) => {
            // Ctrl+C: end the browser agent too, then let it fly.
            let _ = terminate(py, &proc);
            Err(Failure::Py(interrupt))
        }
        Err(other) => Err(other),
    }
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
fn tail<'py>(py: Python<'py>, path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let lines = (|| -> PyResult<Bound<'py, PyAny>> {
        let text = path.call_method(
            "read_text",
            (),
            Some(&kwargs(py, json!({"encoding": "utf-8", "errors": "replace"}))?),
        )?;
        let last = text.call_method0("splitlines")?.get_item(PySlice::new(py, -30, isize::MAX, 1))?;
        PyString::new(py, "\n").call_method1("join", (last,))
    })();
    match lines {
        Err(e) if e.is_instance_of::<PyOSError>(py) => Ok(PyString::new(py, "").into_any()),
        other => other,
    }
}

/// What the browser agent wrote to its scratchpad during the run: the
/// salvage when the `done` value is thin or the run never reached `done`.
fn milestone_notes<'py>(
    py: Python<'py>,
    web_scratchpad: &Bound<'py, PyAny>,
    sid: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let p = web_scratchpad.div(sid)?.div("milestone")?.div("milestone.md")?;
    let notes = (|| -> PyResult<Bound<'py, PyAny>> {
        if !p.call_method0("exists")?.is_truthy()? {
            return Ok(PyString::new(py, "").into_any());
        }
        p.call_method("read_text", (), Some(&kwargs(py, json!({"encoding": "utf-8"}))?))?
            .call_method0("strip")
    })();
    match notes {
        Err(e) if e.is_instance_of::<PyOSError>(py) => Ok(PyString::new(py, "").into_any()),
        other => other,
    }
}
