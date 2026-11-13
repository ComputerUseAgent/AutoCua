//! service: computer use's browser_agent sub-agent tool (macOS, Windows,
//! Linux). `sub_agent {"agent_type": "browser_agent"}` starts a browser agent
//! on a task in a child process of its own (browser_agent_child/service.py,
//! run by path), and the controller polls `outcome` until the run is over;
//! `stop` and `stop_all` end runs from outside, and with 2 or more at work
//! the extension's cover page is in front.
//!
//! The Rust copy of universal_tool/browser_agent/service.py. Python imports
//! it under its old dotted name, AutoCua.universal_tool.browser_agent.service
//! (browser_agent/__init__.py points that name here), and finds the same
//! names in the same order, with the same values, return values, errors and
//! calls. Python's own rules decide every result, so they are called, not
//! imitated: Path, subprocess, json, shutil, the builtins, the str, dict and
//! list methods, the operators and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module its docstring and _build. Then the loader
//! binds what service.py's import lines bound (atexit, ctypes, json, os,
//! shutil, subprocess, sys, threading, time, uuid, Path, data_root,
//! IS_COMPILED) and calls _build(), which runs the rest of service.py's lines
//! in their order: the paths, the constants, the class BrowserAgentError,
//! `_runs`, `_cover` and `_cover_lock`, each function where its def was, and
//! atexit.register(stop_all) between stop_all and _milestone_notes. Loading
//! the extension imports none of them.
//!
//! The paths service.py took from its own __file__ (_REPO_ROOT, _CHILD) are
//! made from the extension's folder (AutoCua) and
//! universal_tool/browser_agent/service.py, through the same Path calls, so
//! they come out the same.
//!
//! Every global a line reads (_runs, _cover, time, subprocess, sum, ...) is
//! read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces one is obeyed. An error is raised the way
//! Python's raise statement raises it, so the error being handled at that
//! moment becomes its __context__.

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple};

use crate::computer::Handling;
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{self, caught, dict, import_from, now, unbound_local};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// service.py's docstring.
const MODULE_DOC: &str = "The browser agent as a sub-agent of the main agent.\n\n\
`sub_agent {\"agent_type\": \"browser_agent\"}` lands here. It starts the browser\n\
agent (AutoCua/browser, the Rust Chrome driver) on a task and returns at once;\n\
the controller then polls `outcome` until the run is over.\n\n\
Not the `web` tool's fallback (AutoCua/universal_tool/web/web_agent.rs),\n\
which is a blocking, headless, read-only research call. This one ACTS: it\n\
navigates, clicks and fills forms, in the person's own Chrome, through the\n\
AutoCua extension.\n\n\
It runs the way parallel `web use` does (see browser_agent_child/service.py):\n\
each agent works in a background tab of its own and never in another's, so up to\n\
MAX_RUNS run at once. Chrome comes up as it does there (in front on macOS; on\n\
Windows and Linux the system places the new window, and nothing here pulls it\n\
forward). Nothing here starts the browser; the child's own constructor starts\n\
it or attaches to it. So the first one opens Chrome, and start() holds the next\n\
until it has (_wait_for_chrome): started together, each would open Chrome over\n\
the others. With 2 or more at work, the cover parallel web use shows is in\n\
front, through a line of our own (_cover_send).\n\n\
One file serves macOS, Windows and Linux (AutoCua/universal_tool).\n\n\
Each run is a subprocess because the web AgentService constructor deletes\n\
CWD-relative conversation/, debug/ and raw_reasoning/ (the main agent's LIVE\n\
folders), so the child gets a working directory of its own.\n";

/// BrowserAgentError's docstring.
const CLASS_DOC: &str = "The browser agent could not be started.";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 83;

/// _TASK_TEMPLATE: the task the child gets, with how to finish.
const TASK_TEMPLATE: &str = "{task}\n\n\
How to finish:\n\
- Your final `done` call's value is the report the main agent receives, and it is all it receives. State what you did, the result with the exact values, names and URLs that matter, and anything you could not do.\n";

/// _PROVIDER_ENV_KEY, in its order: the env var each provider's key goes in.
const PROVIDER_ENV_KEY: [(&str, &str); 11] = [
    ("openrouter", "OPENROUTER_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("google", "GOOGLE_API_KEY"),
    ("perplexity", "PERPLEXITY_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("requesty", "REQUESTY_API_KEY"),
    ("aws", "AWS_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import(format!("{}.browser_agent.service", crate::universal_tool::NATIVE)).map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// Python's statements (the shared ones in lib.rs's ios::controller, given
// this module)
// ---------------------------------------------------------------------------

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `raise BrowserAgentError(<message>)`: the global read first, then the
/// message made, then the error made and raised, in Python's order.
fn agent_error<'py>(py: Python<'py>, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyErr {
    match module(py) {
        Ok(m) => controller::throw(&m, "BrowserAgentError", message),
        Err(e) => e,
    }
}

/// One except clause of a try statement: `e` is the error being handled
/// while the clause's classes are read (`kinds`, globals read now) and while
/// `body` runs, so an error either raises gets `e` as its __context__. What
/// does not match passes on untouched.
fn except<'py, T>(
    py: Python<'py>,
    e: PyErr,
    kinds: impl FnOnce() -> PyResult<Obj<'py>>,
    body: impl FnOnce(&PyErr) -> PyResult<T>,
) -> PyResult<T> {
    let _handling = Handling::new(py, &e);
    let kinds = kinds().map_err(|raised| now(py, raised))?;
    if !caught(py, &e, &kinds)? {
        return Err(e);
    }
    body(&e).map_err(|raised| now(py, raised))
}

/// `except Exception: pass`.
fn pass_exception(py: Python<'_>, e: PyErr) -> PyResult<()> {
    except(py, e, || g(py, "Exception"), |_| Ok(()))
}

/// A str constant.
fn lit<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

fn int(py: Python<'_>, value: i64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

/// `(a, b)`, a tuple display.
fn pair<'py>(a: Obj<'py>, b: Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(PyTuple::new(a.py(), [a, b])?.into_any())
}

/// `value[start:stop]`, as Python's slice syntax takes it: a slice of the
/// two (None for one left out), then value[that slice].
fn slice<'py>(value: &Obj<'py>, start: Option<&Obj<'py>>, stop: Option<&Obj<'py>>) -> PyResult<Obj<'py>> {
    let py = value.py();
    let nothing = none(py);
    let start = start.unwrap_or(&nothing);
    let stop = stop.unwrap_or(&nothing);
    // SAFETY: live objects; a new reference, or NULL with the error set.
    let range = unsafe {
        Bound::from_owned_ptr_or_err(py, ffi::PySlice_New(start.as_ptr(), stop.as_ptr(), std::ptr::null_mut()))
    }?;
    value.get_item(range)
}

/// `value += other`: Python's in-place add.
fn add_in_place<'py>(value: &Obj<'py>, other: &Obj<'py>) -> PyResult<Obj<'py>> {
    // SAFETY: live objects; a new reference, or NULL with the error set.
    unsafe { Bound::from_owned_ptr_or_err(value.py(), ffi::PyNumber_InPlaceAdd(value.as_ptr(), other.as_ptr())) }
}

/// An f-string, built as Python builds one: each value through
/// format(value, "") at its place, then the pieces joined.
struct Fmt<'py> {
    py: Python<'py>,
    parts: Vec<Obj<'py>>,
}

impl<'py> Fmt<'py> {
    fn new(py: Python<'py>) -> Self {
        Fmt { py, parts: Vec::new() }
    }

    fn s(mut self, literal: &str) -> Self {
        self.parts.push(lit(self.py, literal));
        self
    }

    fn v(mut self, value: &Obj<'py>) -> PyResult<Self> {
        self.parts.push(format_value(value, "")?.into_any());
        Ok(self)
    }

    fn done(self) -> PyResult<Obj<'py>> {
        Ok(join_str(self.py, &self.parts.iter().collect::<Vec<_>>())?.into_any())
    }
}

/// An argument as the call gave it, or Missing when the call left it out and
/// the Python default applies. A plain Option cannot tell a None that was
/// passed from an argument that was not.
enum Given<'py> {
    Value(Bound<'py, PyAny>),
    Missing,
}

impl<'py> FromPyObject<'_, 'py> for Given<'py> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, 'py, PyAny>) -> PyResult<Self> {
        Ok(Given::Value(obj.to_owned()))
    }
}

impl<'py> Given<'py> {
    fn or(self, default: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
        match self {
            Given::Value(value) => Ok(value),
            Given::Missing => default(),
        }
    }
}

/// What one of service.py's generator expressions works out for each run.
#[derive(Clone, Copy)]
enum Each {
    /// `r["subprocess"].poll() is None`: the run is still at work.
    AtWork,
    /// `(r["result_file"].parent / "events.jsonl").exists()`: the run has
    /// Chrome (its run_start event is written).
    HasChrome,
}

/// A generator expression of service.py, run as Python runs one: the
/// iterator is taken when it is made, and each item is worked out only when
/// the caller (sum, any) asks for the next one, so `any` stops at the first
/// true one.
#[pyclass(frozen, name = "generator")]
struct Generator {
    items: Py<PyIterator>,
    each: Each,
}

#[pymethods]
impl Generator {
    fn __iter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __next__<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Obj<'py>>> {
        let py = slf.py();
        let this = slf.get();
        let mut items = this.items.bind(py).clone();
        let Some(r) = items.next() else {
            return Ok(None);
        };
        let r = r?;
        match this.each {
            Each::AtWork => {
                let polled = r.get_item("subprocess")?.call_method0("poll")?;
                Ok(Some(boolean(py, polled.is_none())))
            }
            Each::HasChrome => {
                let events = r.get_item("result_file")?.getattr("parent")?.div("events.jsonl")?;
                Ok(Some(events.call_method0("exists")?))
            }
        }
    }
}

/// `(<each> for r in <items>)`: iter() of `items` now, the rest lazily.
fn generator<'py>(items: &Obj<'py>, each: Each) -> PyResult<Obj<'py>> {
    let py = items.py();
    let items = items.try_iter()?.unbind();
    Ok(Bound::new(py, Generator { items, each })?.into_any())
}

/// `r["subprocess"].poll() is None` for every run of `_runs` that is still
/// at work, the list comprehension of _wait_for_chrome.
fn live_runs<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
    let live = PyList::empty(py);
    for r in g(py, "_runs")?.try_iter()? {
        let r = r?;
        if r.get_item("subprocess")?.call_method0("poll")?.is_none() {
            live.append(&r)?;
        }
    }
    Ok(live)
}

/// `if run in _runs: _runs.remove(run)`.
fn forget(py: Python<'_>, run: &Obj<'_>) -> PyResult<()> {
    if g(py, "_runs")?.contains(run)? {
        g(py, "_runs")?.call_method1("remove", (run,))?;
    }
    Ok(())
}

/// `shutil.rmtree(_WEB_SCRATCHPAD / <sid>, ignore_errors=True)`.
fn clear_notes<'py>(py: Python<'py>, sid: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<()> {
    let rmtree = g(py, "shutil")?.getattr("rmtree")?;
    let folder = g(py, "_WEB_SCRATCHPAD")?.div(sid()?)?;
    rmtree.call((folder,), Some(&dict(py, &[("ignore_errors", &boolean(py, true))])?))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// service.py's functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_cover_send", signature = (timeout = Given::Missing), text_signature = "(timeout=10.0)")]
#[doc = "Put up the cover that fits the browser agents at work now, or take it down."]
fn cover_send<'py>(py: Python<'py>, timeout: Given<'py>) -> PyResult<()> {
    let timeout = timeout.or(|| Ok(PyFloat::new(py, 10.0).into_any()))?;
    let lock = g(py, "_cover_lock")?;
    with(&lock, |_| -> PyResult<()> {
        let sum = g(py, "sum")?;
        let runs = g(py, "list")?.call1((g(py, "_runs")?,))?;
        let n = sum.call1((generator(&runs, Each::AtWork)?,))?;
        let want = if n.ge(2)? { n } else { int(py, 0)? };
        let shown = g(py, "_cover")?.get_item("shown")?;
        if want.ne(&shown)? {
            let sent = (|| -> PyResult<()> {
                if g(py, "_cover")?.get_item("bridge")?.is_none() {
                    let found = import_from(&module(py)?, "AutoCua.browser.agent", &["Bridge"])?;
                    let bridge = found[0].call0()?;
                    g(py, "_cover")?.set_item("bridge", bridge)?;
                }
                let request = g(py, "_cover")?.get_item("bridge")?.getattr("request")?;
                let message = dict(py, &[("type", &lit(py, "cover")), ("count", &want)])?;
                request.call1((message, &timeout))?;
                g(py, "_cover")?.set_item("shown", &want)?;
                Ok(())
            })();
            if let Err(e) = sent {
                pass_exception(py, e)?; // a run never fails over its cover page
            }
        }
        if g(py, "_cover")?.get_item("shown")?.eq(0)? {
            g(py, "_cover")?.set_item("bridge", py.None())?; // no cover up: its line goes
        }
        Ok(())
    })?;
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_cover_update", text_signature = "()")]
#[doc = "_cover_send in a thread of its own: opening a line to the extension takes a\n\
second, and the main agent's step does not wait for it."]
fn cover_update(py: Python<'_>) -> PyResult<()> {
    let thread = g(py, "threading")?.getattr("Thread")?;
    let target = g(py, "_cover_send")?;
    let kwargs = dict(py, &[("target", &target), ("daemon", &boolean(py, true))])?;
    thread.call((), Some(&kwargs))?.call_method0("start")?;
    Ok(())
}

#[pyfunction]
#[pyo3(name = "stop_all", text_signature = "()")]
#[doc = "Quitting the app mid-run must not leave a browser agent working: it acts\n\
on websites and calls the model, with nobody left to report to. The watcher\n\
threads are daemons and the run's own cleanup only fires if the run unwinds,\n\
so nothing else covers a quit.\n\n\
Runs at exit, and from the app window's closing event too, because not\n\
every quit runs atexit: Cmd+Q on macOS leaves through NSApplication\n\
terminate:, which calls C exit()."]
fn stop_all(py: Python<'_>) -> PyResult<()> {
    let runs = g(py, "list")?.call1((g(py, "_runs")?,))?;
    for run in runs.try_iter()? {
        let run = run?;
        let stopped = (|| -> PyResult<()> {
            g(py, "stop")?.call1((&run,))?;
            Ok(())
        })();
        if let Err(e) = stopped {
            pass_exception(py, e)?;
        }
    }
    // The cover goes too, here and now: at exit no thread is left to take it down
    let sent = (|| -> PyResult<()> {
        let send = g(py, "_cover_send")?;
        send.call((), Some(&dict(py, &[("timeout", &PyFloat::new(py, 2.0).into_any())])?))?;
        Ok(())
    })();
    if let Err(e) = sent {
        pass_exception(py, e)?;
    }
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_milestone_notes", text_signature = "(sid)")]
#[doc = "What the browser agent wrote to its scratchpad during the run: the\n\
salvage when the `done` value is thin or the run never reached `done`."]
fn milestone_notes<'py>(py: Python<'py>, sid: Obj<'py>) -> PyResult<Obj<'py>> {
    let p = g(py, "_WEB_SCRATCHPAD")?.div(&sid)?.div("milestone")?.div("milestone.md")?;
    let read = (|| -> PyResult<Obj<'py>> {
        if p.call_method0("exists")?.is_truthy()? {
            let kwargs = dict(py, &[("encoding", &lit(py, "utf-8"))])?;
            p.call_method("read_text", (), Some(&kwargs))?.call_method0("strip")
        } else {
            Ok(lit(py, ""))
        }
    })();
    match read {
        Ok(notes) => Ok(notes),
        Err(e) => except(py, e, || g(py, "OSError"), |_| Ok(lit(py, ""))),
    }
}

#[pyfunction]
#[pyo3(name = "_tail", signature = (path, lines = Given::Missing), text_signature = "(path, lines=30)")]
fn tail<'py>(py: Python<'py>, path: Obj<'py>, lines: Given<'py>) -> PyResult<Obj<'py>> {
    let lines = lines.or(|| int(py, 30))?;
    let joined = (|| -> PyResult<Obj<'py>> {
        let join = lit(py, "\n").getattr("join")?;
        let kwargs = dict(py, &[("encoding", &lit(py, "utf-8")), ("errors", &lit(py, "replace"))])?;
        let read = path.call_method("read_text", (), Some(&kwargs))?;
        let last = slice(&read.call_method0("splitlines")?, Some(&lines.neg()?), None)?;
        join.call1((last,))
    })();
    match joined {
        Ok(text) => Ok(text),
        Err(e) => except(py, e, || g(py, "OSError"), |_| Ok(lit(py, ""))),
    }
}

#[pyfunction]
#[pyo3(name = "_wait_for_chrome", signature = (stop_event = None), text_signature = "(stop_event=None)")]
#[doc = "Hold while a browser agent is still opening Chrome and none has it yet:\n\
a child has Chrome once its run has begun (its run_start event in\n\
events.jsonl, written right after its constructor took Chrome). Returns at\n\
once when no browser agent is at work, or one has Chrome; else when the one\n\
opening it gets there, exits, START_WAIT_SEC passes, or Stop is pressed."]
fn wait_for_chrome<'py>(py: Python<'py>, stop_event: Option<Obj<'py>>) -> PyResult<()> {
    let stop_event = stop_event.unwrap_or_else(|| none(py));
    let deadline = g(py, "time")?.getattr("monotonic")?.call0()?.add(g(py, "START_WAIT_SEC")?)?;
    loop {
        // while time.monotonic() < deadline and not (stop_event is not None and stop_event.is_set()):
        if !g(py, "time")?.getattr("monotonic")?.call0()?.lt(&deadline)? {
            return Ok(());
        }
        if !stop_event.is_none() && stop_event.call_method0("is_set")?.is_truthy()? {
            return Ok(());
        }
        let live = live_runs(py)?;
        if live.is_empty() {
            return Ok(());
        }
        let any = g(py, "any")?;
        if any.call1((generator(live.as_any(), Each::HasChrome)?,))?.is_truthy()? {
            return Ok(());
        }
        g(py, "time")?.getattr("sleep")?.call1((0.1,))?;
    }
}

#[pyfunction]
#[pyo3(
    name = "start",
    signature = (task, provider, model, api_key = None, speed = None, stop_event = None),
    text_signature = "(task, provider, model, api_key=None, speed=None, stop_event=None)"
)]
#[doc = "Start one browser agent on `task` and return its run record, the dict\n\
`outcome` and `stop` take. Raises BrowserAgentError when it cannot start."]
fn start<'py>(
    py: Python<'py>,
    task: Obj<'py>,
    provider: Obj<'py>,
    model: Obj<'py>,
    api_key: Option<Obj<'py>>,
    speed: Option<Obj<'py>>,
    stop_event: Option<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    let api_key = api_key.unwrap_or_else(|| none(py));
    let speed = speed.unwrap_or_else(|| none(py));
    let stop_event = stop_event.unwrap_or_else(|| none(py));
    if g(py, "IS_COMPILED")?.is_truthy()? {
        return Err(agent_error(py, || Ok(lit(py, "the browser agent is unavailable in the packaged build"))));
    }
    // Google Chrome is never downloaded (utils/chrome.py): without it no
    // browser agent starts, and the main agent is told why
    let found = import_from(&module(py)?, "AutoCua.utils.chrome", &["MISSING", "find_chrome"])?;
    let (chrome_missing, find_chrome) = (&found[0], &found[1]);
    if find_chrome.call0()?.is_none() {
        return Err(agent_error(py, || Ok(chrome_missing.clone())));
    }
    // Each one works in a tab of its own (browser_agent_child/service.py), up to MAX_RUNS
    let sum = g(py, "sum")?;
    let at_work = sum.call1((generator(&g(py, "_runs")?, Each::AtWork)?,))?;
    if at_work.ge(g(py, "MAX_RUNS")?)? {
        return Err(agent_error(py, || {
            Fmt::new(py)
                .s("all ")
                .v(&g(py, "MAX_RUNS")?)?
                .s(" browser agents are at work: wait for one of them to finish (agent_wait), then start this one")
                .done()
        }));
    }
    // The first one opens Chrome; this one starts once it has, and only attaches
    g(py, "_wait_for_chrome")?.call1((&stop_event,))?;
    if !stop_event.is_none() && stop_event.call_method0("is_set")?.is_truthy()? {
        return Err(agent_error(py, || Ok(lit(py, "stopped before it started"))));
    }

    // sid = f"browser_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    let int_ = g(py, "int")?;
    let seconds = int_.call1((g(py, "time")?.getattr("time")?.call0()?,))?;
    let sid = Fmt::new(py).s("browser_").v(&seconds)?.s("_");
    let hex = g(py, "uuid")?.getattr("uuid4")?.call0()?.getattr("hex")?;
    let sid = sid.v(&slice(&hex, None, Some(&int(py, 6)?))?)?.done()?;
    let run_dir = g(py, "data_root")?.call0()?.div("browser_agent")?.div(&sid)?;
    let made = dict(py, &[("parents", &boolean(py, true)), ("exist_ok", &boolean(py, true))])?;
    run_dir.call_method("mkdir", (), Some(&made))?;
    let result_file = run_dir.div("result.json")?;
    let log_file = run_dir.div("agent.log")?;

    let executable = g(py, "sys")?.getattr("executable")?;
    let child = g(py, "str")?.call1((g(py, "_CHILD")?,))?;
    let task_text = g(py, "_TASK_TEMPLATE")?.call_method("format", (), Some(&dict(py, &[("task", &task)])?))?;
    let result_text = g(py, "str")?.call1((&result_file,))?;
    let mut cmd = PyList::new(
        py,
        [
            executable,
            child,
            lit(py, "--task"),
            task_text,
            lit(py, "--provider"),
            provider.clone(),
            lit(py, "--model"),
            model,
            lit(py, "--result"),
            result_text,
            lit(py, "--session-id"),
            sid.clone(),
        ],
    )?
    .into_any();
    if speed.is_truthy()? {
        let extra = PyList::new(py, [lit(py, "--speed"), g(py, "str")?.call1((&speed,))?])?;
        cmd = add_in_place(&cmd, extra.as_any())?;
    }
    // Tests only: a private port and profile. Left out, the child uses the web
    // agent's own defaults, exactly as `web use` does.
    for (var, flag) in [
        ("AutoCua_BROWSER_AGENT_PORT", "--browser-port"),
        ("AutoCua_BROWSER_AGENT_PROFILE", "--browser-profile"),
    ] {
        if g(py, "os")?.getattr("environ")?.call_method1("get", (var,))?.is_truthy()? {
            let value = g(py, "os")?.getattr("environ")?.get_item(var)?;
            let extra = PyList::new(py, [lit(py, flag), value])?;
            cmd = add_in_place(&cmd, extra.as_any())?;
        }
    }
    let env = g(py, "os")?.getattr("environ")?.call_method0("copy")?;
    env.set_item("PYTHONUNBUFFERED", "1")?;
    // The child's cwd is outside the repo, so make the package importable.
    let path = g(py, "str")?.call1((g(py, "_REPO_ROOT")?,))?.add(g(py, "os")?.getattr("pathsep")?)?;
    let path = path.add(env.call_method1("get", ("PYTHONPATH", ""))?)?;
    env.set_item("PYTHONPATH", path)?;
    if api_key.is_truthy()? && g(py, "_PROVIDER_ENV_KEY")?.contains(&provider)? {
        let key = g(py, "_PROVIDER_ENV_KEY")?.get_item(&provider)?;
        env.set_item(key, &api_key)?;
    }

    // Windows: the child shares the app's console, so closing that terminal ends
    // it too, as it does on macOS and Linux. Only an app with no console gets
    // the child a hidden one, so no console window pops up.
    let mut creationflags = int(py, 0)?;
    if g(py, "sys")?.getattr("platform")?.eq("win32")? {
        let console = g(py, "ctypes")?.getattr("windll")?.getattr("kernel32")?.call_method0("GetConsoleWindow")?;
        if !console.is_truthy()? {
            creationflags = g(py, "subprocess")?.getattr("CREATE_NO_WINDOW")?;
        }
    }

    let started = (|| -> PyResult<Option<Obj<'py>>> {
        // stdout/stderr go to a file: the browser agent prints a lot, and a
        // PIPE nobody drains would deadlock it.
        let log = g(py, "open")?.call1((&log_file, "ab"))?;
        with(&log, |log| -> PyResult<Obj<'py>> {
            let popen = g(py, "subprocess")?.getattr("Popen")?;
            let cwd = g(py, "str")?.call1((&run_dir,))?;
            let stdin = g(py, "subprocess")?.getattr("DEVNULL")?;
            let stderr = g(py, "subprocess")?.getattr("STDOUT")?;
            let kwargs = dict(
                py,
                &[
                    ("cwd", &cwd),
                    ("env", &env),
                    ("creationflags", &creationflags),
                    ("stdin", &stdin),
                    ("stdout", log),
                    ("stderr", &stderr),
                ],
            )?;
            popen.call((&cmd,), Some(&kwargs))
        })
    })();
    let proc = match started {
        Ok(proc) => proc,
        Err(e) => {
            return except(py, e, || g(py, "Exception"), |e| {
                Err(agent_error(py, || {
                    Fmt::new(py).s("the browser agent could not be started: ").v(e.value(py))?.done()
                }))
            });
        }
    };

    // A with block whose __exit__ swallowed the error leaves proc unset.
    let Some(proc) = proc else {
        return Err(unbound_local(py, "proc"));
    };
    let deadline = g(py, "time")?.getattr("monotonic")?.call0()?.add(g(py, "TIMEOUT_SEC")?)?;
    let run = dict(
        py,
        &[
            ("session_id", &sid),
            ("subprocess", &proc),
            ("result_file", &result_file),
            ("log_file", &log_file),
            ("deadline", &deadline),
            ("stopped", &boolean(py, false)),
        ],
    )?
    .into_any();
    g(py, "_runs")?.call_method1("append", (&run,))?;
    g(py, "_cover_update")?.call0()?; // 2 or more at work: the cover in front
    Ok(run)
}

#[pyfunction]
#[pyo3(name = "stop", text_signature = "(run)")]
#[doc = "End a run from outside (Stop, or the end of the main agent's run). The\n\
browser itself stays up, as it does after any web run: only the child is\n\
ended, never its process tree or group, because the browser stays up."]
fn stop<'py>(py: Python<'py>, run: Obj<'py>) -> PyResult<()> {
    run.set_item("stopped", true)?;
    let proc = run.call_method1("get", ("subprocess",))?;
    if !proc.is_none() && proc.call_method0("poll")?.is_none() {
        let ended = (|| -> PyResult<()> {
            proc.call_method0("terminate")?;
            proc.call_method("wait", (), Some(&dict(py, &[("timeout", &int(py, 5)?)])?))?;
            Ok(())
        })();
        if let Err(e) = ended {
            // except subprocess.TimeoutExpired: ... except Exception: pass
            let _handling = Handling::new(py, &e);
            let timed_out = g(py, "subprocess")?.getattr("TimeoutExpired")?;
            if caught(py, &e, &timed_out)? {
                proc.call_method0("kill")?;
                proc.call_method0("wait")?;
            } else if !caught(py, &e, &g(py, "Exception")?)? {
                return Err(e);
            }
        }
    }
    // The run is over for good, so its notes go with it. Done here and not only
    // in `outcome`: when the app quits, nothing calls `outcome` any more.
    clear_notes(py, || run.get_item("session_id"))
}

#[pyfunction]
#[pyo3(name = "outcome", text_signature = "(run)")]
#[doc = "(summary, status) once the run is over, None while it is still working.\n\n\
status is `complete` when the browser agent finished its task, otherwise how\n\
it ended: `incomplete`, `error`, `timeout` or `stopped`. Call it until it\n\
answers; the run's scratchpad is cleared on the way out.\n"]
fn outcome<'py>(py: Python<'py>, run: Obj<'py>) -> PyResult<Obj<'py>> {
    let proc = run.get_item("subprocess")?;
    let sid = run.get_item("session_id")?;
    if proc.call_method0("poll")?.is_none() {
        if g(py, "time")?.getattr("monotonic")?.call0()?.le(run.get_item("deadline")?)? {
            return Ok(none(py));
        }
        // Notes first: `stop` clears the run's scratchpad
        let notes = g(py, "_milestone_notes")?.call1((&sid,))?;
        let notes = if notes.is_truthy()? { notes } else { lit(py, "nothing was recorded before the time limit") };
        g(py, "stop")?.call1((&run,))?;
        run.set_item("stopped", false)?; // it ran out of time, nobody stopped it
        forget(py, &run)?;
        g(py, "_cover_update")?.call0()?;
        let minutes = g(py, "TIMEOUT_SEC")?.floor_div(60)?;
        let summary = Fmt::new(py)
            .s("The browser agent ran out of time (")
            .v(&minutes)?
            .s(" min) before it finished.\n\nRecorded before it stopped:\n")
            .v(&notes)?
            .done()?;
        return pair(summary, lit(py, "timeout"));
    }

    let notes = g(py, "_milestone_notes")?.call1((&sid,))?;
    clear_notes(py, || Ok(sid.clone()))?;
    forget(py, &run)?;
    g(py, "_cover_update")?.call0()?; // one fewer at work: the count goes down, or the cover goes
    if run.call_method1("get", ("stopped",))?.is_truthy()? {
        return pair(lit(py, "Stopped by user"), lit(py, "stopped"));
    }
    let loaded = (|| -> PyResult<Obj<'py>> {
        let loads = g(py, "json")?.getattr("loads")?;
        let kwargs = dict(py, &[("encoding", &lit(py, "utf-8"))])?;
        let text = run.get_item("result_file")?.call_method("read_text", (), Some(&kwargs))?;
        loads.call1((text,))
    })();
    let result = match loaded {
        Ok(result) => result,
        Err(e) => {
            let kinds = || -> PyResult<Obj<'py>> {
                let os_error = g(py, "OSError")?;
                let value_error = g(py, "ValueError")?;
                Ok(PyTuple::new(py, [os_error, value_error])?.into_any())
            };
            return except(py, e, kinds, |_| {
                let summary = Fmt::new(py)
                    .s("The browser agent exited with code ")
                    .v(&proc.getattr("returncode")?)?
                    .s(" without a report.\n\n");
                let tail = g(py, "_tail")?.call1((run.get_item("log_file")?,))?;
                pair(summary.v(&tail)?.done()?, lit(py, "error"))
            });
        }
    };

    // status = str(result.get("status") or "error")
    let str_ = g(py, "str")?;
    let status = result.call_method1("get", ("status",))?;
    let status = str_.call1((if status.is_truthy()? { status } else { lit(py, "error") },))?;
    // message = str(result.get("message") or "").strip() or "no message"
    let str_ = g(py, "str")?;
    let message = result.call_method1("get", ("message",))?;
    let message = str_.call1((if message.is_truthy()? { message } else { lit(py, "") },))?.call_method0("strip")?;
    let mut message = if message.is_truthy()? { message } else { lit(py, "no message") };
    if status.eq("success")? {
        let length = g(py, "len")?.call1((&message,))?;
        if length.lt(g(py, "_SHORT_REPORT_CHARS")?)? && notes.is_truthy()? {
            let extra = lit(py, "\n\nRecorded during the run:\n").add(&notes)?;
            message = add_in_place(&message, &extra)?;
        }
        return pair(message, lit(py, "complete"));
    }
    if notes.is_truthy()? {
        let extra = lit(py, "\n\nRecorded before it stopped:\n").add(&notes)?;
        message = add_in_place(&message, &extra)?;
    }
    pair(message, status)
}

// ---------------------------------------------------------------------------
// service.py's lines, run once the loader bound its imports
// ---------------------------------------------------------------------------

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// service.py's __file__, as it was: the extension's folder (AutoCua) and
/// universal_tool/browser_agent/service.py, a str made with no Python call.
fn service_file(py: Python<'_>) -> PyResult<Obj<'_>> {
    let file = crate::native_dir(py)?.join("universal_tool").join("browser_agent").join("service.py");
    Ok(PyString::new(py, &file.to_string_lossy()).into_any())
}

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // _REPO_ROOT = Path(__file__).resolve().parents[3]
    let path = g(py, "Path")?;
    let root = path.call1((service_file(py)?,))?.call_method0("resolve")?.getattr("parents")?.get_item(3)?;
    store(m, "_REPO_ROOT", root)?;
    let scratchpad = g(py, "_REPO_ROOT")?.div("AutoCua")?.div("browser")?.div("scratchpad")?;
    store(m, "_WEB_SCRATCHPAD", scratchpad)?;
    let path = g(py, "Path")?;
    let folder = path.call1((service_file(py)?,))?.call_method0("resolve")?.getattr("parent")?.getattr("parent")?;
    store(m, "_CHILD", folder.div("browser_agent_child")?.div("service.py")?)?;

    // The child has its own 100-step cap; this bounds wall-clock.
    let int_ = g(py, "int")?;
    let environ = g(py, "os")?.getattr("environ")?;
    let timeout = environ.call_method1("get", ("AutoCua_BROWSER_AGENT_TIMEOUT", "900"))?;
    store(m, "TIMEOUT_SEC", int_.call1((timeout,))?)?;
    // At most this many at once, each in a tab of its own. The controller's
    // browser_agent slots (SUB_AGENT_SLOTS) decide first; start() is the backstop.
    store(m, "MAX_RUNS", 4)?;
    // How long start() holds a new browser agent while the one before it is still
    // opening Chrome (_wait_for_chrome): a few seconds when Chrome is up with the
    // extension, more when Chrome has to start or restart.
    store(m, "START_WAIT_SEC", 90)?;
    // The runtime (frontend) key reaches the child through its env: the web agent's
    // __main__ has no --api_key flag, and its .env loader never overrides a var
    // that is already set.
    let keys = PyDict::new(py);
    for (provider, var) in PROVIDER_ENV_KEY {
        keys.set_item(provider, var)?;
    }
    store(m, "_PROVIDER_ENV_KEY", keys)?;
    // A `done` value shorter than this is almost certainly an activity log, so it
    // is topped up with what the agent recorded along the way.
    store(m, "_SHORT_REPORT_CHARS", 200)?;
    store(m, "_TASK_TEMPLATE", TASK_TEMPLATE)?;

    // class BrowserAgentError(Exception): made as the class statement makes
    // it, the metaclass of its base called with the body's namespace.
    let base = g(py, "Exception")?;
    let namespace = PyDict::new(py);
    namespace.set_item("__module__", g(py, "__name__")?)?;
    namespace.set_item("__qualname__", "BrowserAgentError")?;
    if py.version_info() >= (3, 13) {
        namespace.set_item("__firstlineno__", CLASS_LINE)?;
    }
    namespace.set_item("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        namespace.set_item("__static_attributes__", PyTuple::empty(py))?;
    }
    let bases = PyTuple::new(py, [&base])?;
    let class = base.get_type().call1(("BrowserAgentError", bases, namespace))?;
    store(m, "BrowserAgentError", class)?;

    // Every run started by this process, so none outlives it.
    store(m, "_runs", PyList::empty(py))?;

    // The front tab while 2 or more browser agents work, each in a background tab of
    // its own: the extension's agents.html ("Multiple agents running" over the looping
    // simulation_background.mp4, with the number of agents), the cover parallel web use
    // shows (agent_launcher.run_parallel_web_agents). Under 2 it closes. It goes through
    // a line to the extension of our own, opened when first needed, dropped with it.
    store(m, "_cover", dict(py, &[("bridge", &none(py)), ("shown", &int(py, 0)?)])?)?;
    store(m, "_cover_lock", g(py, "threading")?.getattr("Lock")?.call0()?)?;

    store(m, "_cover_send", wrap_pyfunction!(cover_send, m)?)?;
    store(m, "_cover_update", wrap_pyfunction!(cover_update, m)?)?;
    store(m, "stop_all", wrap_pyfunction!(stop_all, m)?)?;

    // atexit.register(stop_all)
    let register = g(py, "atexit")?.getattr("register")?;
    register.call1((g(py, "stop_all")?,))?;

    store(m, "_milestone_notes", wrap_pyfunction!(milestone_notes, m)?)?;
    store(m, "_tail", wrap_pyfunction!(tail, m)?)?;
    store(m, "_wait_for_chrome", wrap_pyfunction!(wait_for_chrome, m)?)?;
    store(m, "start", wrap_pyfunction!(start, m)?)?;
    store(m, "stop", wrap_pyfunction!(stop, m)?)?;
    store(m, "outcome", wrap_pyfunction!(outcome, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with its docstring and
/// _build, which the loader calls once it has bound the import lines' names
/// (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
