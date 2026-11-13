//! The child process of one android_agent run. Computer use's android_agent
//! tool (universal_tool/android_agent/service.py) starts
//! android_agent_child/service.py by path, in a process group of its own, and
//! that file only calls main() here. main() runs the Android agent on the
//! phone the parent checked and writes what it returned to --result; the
//! agent's tool-flow events and the phone's screens go to events.jsonl and
//! frame.jpg beside it, where the app shows them live (_LiveView).
//!
//! The Rust copy of the runner service.py was. Its module is
//! AutoCua.universal_tool.android_agent_child.service (lib.rs makes it so),
//! also in sys.modules as
//! AutoCua.agent_native.universal_tool.android_agent_child.service, and it has
//! the names the .py had, in the .py's order: its docstring, the standard
//! library modules it imported, FRAME_EDGE, FRAME_QUALITY, _LiveView,
//! _watch_parent and main. (_HERE stays in the .py.) Python's own rules decide
//! every result, so they are called, not imitated: int(), str(), type(),
//! argparse, json, base64, io, PIL, pathlib's Path and its `/`, os.replace,
//! the `with` blocks, `+=`, the comparisons and the f-string.
//!
//! Every module global is read from this module at the moment it is used
//! (lib.rs's ios::controller::name), so a test or a caller that replaces one
//! (os, time, json, threading, sys, FRAME_EDGE, _LiveView, ...) is obeyed. A
//! _LiveView keeps folder, frames and lock in its __dict__, in that order, and
//! its methods reach them and each other through the instance, so a subclass
//! that replaces one is obeyed.

use pyo3::exceptions::PyTypeError;
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyCFunction, PyDict, PyString, PyTuple};

use crate::computer::{as_python_class, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{self, caught, except_exception, fstring, import_from, text};
use crate::memory_compression::join_str;
use crate::universal_tool::NATIVE;

type Obj<'py> = Bound<'py, PyAny>;

/// A dict display, `{"key": value, ...}`, or the keywords of a call: a new
/// dict with the keys in the order written. A value is anything PyO3 turns
/// into a Python object (a &str is a str, a bool a bool, an i32 an int).
macro_rules! display {
    ($py:expr, {$($key:literal: $value:expr),* $(,)?}) => {{
        let built = PyDict::new($py);
        $(built.set_item($key, $value)?;)*
        built
    }};
}

/// service.py's docstring.
const MODULE_DOC: &str = "Android Agent Child Runner\n\
==========================\n\
Subprocess entry for one android_agent run started by the main agent\n\
(../android_agent/service.py spawns it; it is run BY PATH, never imported).\n\
\n\
It runs the Android agent (the Rust extension in AutoCua/android) on the phone\n\
the parent checked: AutoCua_ANDROID_SERIAL names it, and the agent's screen\n\
reader starts the phone's helper itself and stops it when this process ends.\n\
\n\
The process_request result dict is always written to --result.\n\
\n\
The app shows the run live, in its box in the main agent's chat: the agent's\n\
tool-flow events and the phone's screens go to events.jsonl and frame.jpg next\n\
to --result (the run's folder), where the main agent's controller reads them.\n\
\n\
Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.\n";

/// _LiveView's docstring, as Python 3.13 keeps it (indents taken out).
const CLASS_DOC: &str = "The run's live view, written for the app (controller/view.py reads it):\n\
each tool-flow event as one JSON line in events.jsonl, and each screen,\n\
made small, as frame.jpg plus a line saying a new one is there. The same\n\
as ios_agent_child.py's.\n\
\n\
Both are the agent's callbacks, so every call catches everything: the run\n\
never fails over its live view. Nothing goes to stdout, which is agent.log,\n\
the main agent's error report.";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.android_agent_child.service")).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now (lib.rs's
/// controller::name: a missing one is NameError, naming the name).
fn name<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

/// Runs `body` while `e` (when there is one) is the exception being
/// handled, as an except or a finally block runs. An error the body raises
/// is made into its exception before `e` stops being handled, so one PyO3
/// makes lazily gets `e` as its __context__ too, as any raise inside the
/// block does in Python. (The same as ios/controller/view.rs's, kept here.)
fn handling<'py, T>(py: Python<'py>, e: Option<&PyErr>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let _handling = e.map(|e| Handling::new(py, e));
    let done = body();
    if let Err(raised) = &done {
        raised.value(py);
    }
    done
}

/// `try: <tried> except Exception: pass`. Exception is read from this module
/// while the error is being handled, as Python reads it there, and what does
/// not match it (a KeyboardInterrupt, a SystemExit) goes through.
fn except_exception_pass(py: Python<'_>, tried: PyResult<()>) -> PyResult<()> {
    match tried {
        Ok(()) => Ok(()),
        Err(e) => except_exception(&module(py)?, e, |_| Ok(())),
    }
}

/// `value += other`, as Python runs it: PyNumber_InPlaceAdd, so an object's
/// own __iadd__ is used where it has one.
fn inplace_add<'py>(value: &Obj<'py>, other: &Obj<'py>) -> PyResult<Obj<'py>> {
    // SAFETY: two live objects, with the GIL held; a new reference, or NULL
    // with the error set.
    unsafe { Bound::from_owned_ptr_or_err(value.py(), ffi::PyNumber_InPlaceAdd(value.as_ptr(), other.as_ptr())) }
}

// ---------------------------------------------------------------------------
// _LiveView
// ---------------------------------------------------------------------------

// The run's live view. A plain comment: register() gives the class its
// docstring, the .py's.
#[pyclass(frozen, dict, weakref, subclass, name = "_LiveView")]
pub struct LiveView;

#[pymethods]
impl LiveView {
    // A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "(folder)")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        LiveView
    }

    // The .py's __init__(self, folder: Path), which had no docstring.
    #[pyo3(signature = (folder))]
    fn __init__(slf: &Bound<'_, Self>, folder: &Bound<'_, PyAny>) -> PyResult<()> {
        slf.setattr("folder", folder)?;
        slf.setattr("frames", 0)?;
        slf.setattr("lock", name(slf.py(), "threading")?.getattr("Lock")?.call0()?)?;
        Ok(())
    }

    /// tool_callback: run_start, turn, received, error or run_end
    #[pyo3(signature = (event, payload = None))]
    fn flow(slf: &Bound<'_, Self>, event: &Bound<'_, PyAny>, payload: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
        let py = slf.py();
        let tried = (|| -> PyResult<()> {
            let p = match payload {
                None => none(py),
                Some(payload) => {
                    let dumps = name(py, "json")?.getattr("dumps")?;
                    dumps.call((payload,), Some(&display!(py, {"ensure_ascii": false})))?
                }
            };
            with(&slf.getattr("lock")?, |_| {
                let write = slf.getattr("_write")?;
                let ev = name(py, "str")?.call1((event,))?;
                write.call1((display!(py, {"k": "flow", "ev": ev, "p": p}),))?;
                Ok(())
            })?;
            Ok(())
        })();
        except_exception_pass(py, tried)
    }

    /// frontend_callback: the phone's plain screen of a scan, as base64 JPEG
    fn frame(slf: &Bound<'_, Self>, image_b64: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let tried = (|| -> PyResult<()> {
            // from PIL import Image, each call, as the .py did.
            let image_module = import_from(&module(py)?, "PIL", &["Image"])?.remove(0);
            let open = image_module.getattr("open")?;
            let bytes_io = name(py, "io")?.getattr("BytesIO")?;
            let b64decode = name(py, "base64")?.getattr("b64decode")?;
            let mut image = open.call1((bytes_io.call1((b64decode.call1((image_b64,))?,))?,))?;
            let thumbnail = image.getattr("thumbnail")?;
            let edge = PyTuple::new(py, [name(py, "FRAME_EDGE")?, name(py, "FRAME_EDGE")?])?;
            thumbnail.call1((edge,))?;
            if image.getattr("mode")?.ne("RGB")? {
                image = image.call_method1("convert", ("RGB",))?;
            }
            with(&slf.getattr("lock")?, |_| {
                // Written whole, then swapped in: the app never reads half a picture
                let tmp = slf.getattr("folder")?.div("frame.tmp")?;
                let save = image.getattr("save")?;
                save.call((&tmp,), Some(&display!(py, {"format": "JPEG", "quality": name(py, "FRAME_QUALITY")?})))?;
                let replace = name(py, "os")?.getattr("replace")?;
                replace.call1((&tmp, slf.getattr("folder")?.div("frame.jpg")?))?;
                let frames = slf.getattr("frames")?;
                slf.setattr("frames", inplace_add(&frames, &1i32.into_pyobject(py)?.into_any())?)?;
                let write = slf.getattr("_write")?;
                write.call1((display!(py, {"k": "frame", "n": slf.getattr("frames")?}),))?;
                Ok(())
            })?;
            Ok(())
        })();
        except_exception_pass(py, tried)
    }

    // One line of events.jsonl. No docstring, as in the .py.
    fn _write(slf: &Bound<'_, Self>, row: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        let open = name(py, "open")?;
        let f = open.call1((slf.getattr("folder")?.div("events.jsonl")?, "ab"))?;
        with(&f, |f| {
            let write = f.getattr("write")?;
            let line = name(py, "json")?.getattr("dumps")?.call1((row,))?.add("\n")?;
            write.call1((line.call_method1("encode", ("utf-8",))?,))?;
            Ok(())
        })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// _watch_parent
// ---------------------------------------------------------------------------

/// The watcher thread's target takes no arguments, as the `def watch():`
/// inside _watch_parent took none. A call that gives some is refused in
/// Python's words, an unexpected keyword before the count, as Python checks
/// them.
fn takes_nothing(args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
    const QUALNAME: &str = "_watch_parent.<locals>.watch";
    if let Some((keyword, _)) = kwargs.and_then(|kwargs| kwargs.iter().next()) {
        let py = args.py();
        let open = PyString::new(py, &format!("{QUALNAME}() got an unexpected keyword argument '"));
        let close = PyString::new(py, "'");
        let message = join_str(py, &[open.as_any(), keyword.str()?.as_any(), close.as_any()])?;
        return Err(PyTypeError::new_err((message.unbind(),)));
    }
    match args.len() {
        0 => Ok(()),
        1 => Err(PyTypeError::new_err(format!("{QUALNAME}() takes 0 positional arguments but 1 was given"))),
        given => Err(PyTypeError::new_err(format!(
            "{QUALNAME}() takes 0 positional arguments but {given} were given"
        ))),
    }
}

/// The process that started this run can die without stopping it: a
/// closed terminal sends it SIGHUP, which this run, in a process group of
/// its own (so Stop reaches the phone helper's adb client), no longer gets.
/// Then this run ends itself, its whole group with it, so nothing goes on
/// driving the phone with nobody to report to. POSIX: a dead parent's
/// children are handed to another one, which getppid() shows.
#[pyfunction]
#[pyo3(name = "_watch_parent")]
fn watch_parent(py: Python<'_>) -> PyResult<()> {
    // parent = int(os.environ.get("AutoCua_ANDROID_PARENT") or 0): int is
    // read first, and a bad value is int's ValueError.
    let int = name(py, "int")?;
    let given = name(py, "os")?.getattr("environ")?.call_method1("get", ("AutoCua_ANDROID_PARENT",))?;
    let given = if given.is_truthy()? { given } else { 0i32.into_pyobject(py)?.into_any() };
    let parent = int.call1((given,))?;
    if !parent.is_truthy()? || name(py, "sys")?.getattr("platform")?.eq("win32")? {
        return Ok(());
    }

    // def watch(): named "watch", so the thread is "Thread-N (watch)".
    let parent = parent.unbind();
    let watch = PyCFunction::new_closure(py, Some(c"watch"), None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let py = args.py();
        takes_nothing(args, kwargs)?;
        let parent = parent.bind(py);
        while name(py, "os")?.getattr("getppid")?.call0()?.eq(parent)? {
            name(py, "time")?.getattr("sleep")?.call1((1,))?;
        }
        let killpg = name(py, "os")?.getattr("killpg")?;
        let group = name(py, "os")?.getattr("getpgrp")?.call0()?;
        killpg.call1((group, name(py, "signal")?.getattr("SIGTERM")?))?;
        Ok(py.None())
    })?;
    // A def in this module reports the module's name as its __module__.
    watch.setattr("__module__", module(py)?.name()?)?;

    let thread = name(py, "threading")?.getattr("Thread")?;
    thread.call((), Some(&display!(py, {"target": &watch, "daemon": true})))?.call_method0("start")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

/// How the try block's error was handled: by `except KeyboardInterrupt`, or
/// by `except Exception as e` with the result it made.
enum Caught<'py> {
    Interrupted,
    Failed(Obj<'py>),
}

/// main's try block, up to the result it gives. The names it binds
/// (AgentService, live, task, line, android_session) go into `kept`, and the
/// agent into `agent`: Python keeps them in main's frame until main ends,
/// through the except and finally blocks too, and so does main here. A
/// second agent takes the first one's place, as `agent = build()` did.
fn try_block<'py>(
    py: Python<'py>,
    args: &Obj<'py>,
    kept: &mut Vec<Obj<'py>>,
    agent: &mut Option<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    let agent_service = import_from(&module(py)?, "AutoCua.android.agent", &["AgentService"])?.remove(0);
    kept.push(agent_service.clone());

    // Only these two: web_callback and shell_callback add sleeps, and
    // token_callback would move the main agent's memory bar
    let live = if args.getattr("result")?.is_truthy()? {
        let live_view = name(py, "_LiveView")?;
        let path = name(py, "Path")?.call1((args.getattr("result")?,))?;
        live_view.call1((path.call_method0("resolve")?.getattr("parent")?,))?
    } else {
        none(py)
    };
    kept.push(live.clone());

    // def build(): `live` is asked for its truth once for each callback.
    let build = || -> PyResult<Obj<'py>> {
        let provider = args.getattr("provider")?;
        let model = args.getattr("model")?;
        let speed = args.getattr("speed")?;
        let tool_callback = if live.is_truthy()? { live.getattr("flow")? } else { none(py) };
        let frontend_callback = if live.is_truthy()? { live.getattr("frame")? } else { none(py) };
        let kwargs = display!(py, {
            "provider": provider,
            "model": model,
            "speed": speed,
            "tool_callback": tool_callback,
            "frontend_callback": frontend_callback,
        });
        agent_service.call((), Some(&kwargs))
    };

    let mut current = build()?;
    *agent = Some(current.clone());
    let mut task = args.getattr("task")?;
    kept.push(task.clone());
    if args.getattr("standby")?.is_truthy()? {
        // Started when the phone came online (android_agent/service.py), so
        // the run begins the moment the main agent sends its task. No
        // line: the request ended without one.
        let line = name(py, "sys")?.getattr("stdin")?.call_method0("readline")?;
        kept.push(line.clone());
        task = if line.call_method0("strip")?.is_truthy()? {
            name(py, "json")?.getattr("loads")?.call1((&line,))?.get_item("task")?
        } else {
            none(py)
        };
        kept.push(task.clone());
        // The helper this agent started while it waited may be gone (a
        // phone that dozed off the Wi-Fi): connect again, as a fresh child.
        let android_session =
            import_from(&module(py)?, "AutoCua.android_connector.session", &["android_session"])?.remove(0);
        kept.push(android_session.clone());
        if !task.is_none() && android_session.call_method0("status")?.call_method1("get", ("state",))?.ne("connected")? {
            current = build()?;
            *agent = Some(current.clone());
        }
    }
    if task.is_none() {
        Ok(display!(py, {"status": "stopped", "message": "no task came"}).into_any())
    } else {
        current.call_method1("process_request", (&task,))
    }
}

/// main's finally block: the result, whatever it is, written to --result.
/// An OSError (the module's OSError, read while it is being handled) is
/// dropped; anything else goes on.
fn write_result<'py>(py: Python<'py>, args: &Obj<'py>, result: &Obj<'py>) -> PyResult<()> {
    if !args.getattr("result")?.is_truthy()? {
        return Ok(());
    }
    let written = (|| -> PyResult<()> {
        let path = name(py, "Path")?.call1((args.getattr("result")?,))?;
        let mkdir = path.getattr("parent")?.getattr("mkdir")?;
        mkdir.call((), Some(&display!(py, {"parents": true, "exist_ok": true})))?;
        let write_text = path.getattr("write_text")?;
        let dumps = name(py, "json")?.getattr("dumps")?;
        let text = dumps.call((result,), Some(&display!(py, {"indent": 2, "ensure_ascii": false})))?;
        write_text.call((text,), Some(&display!(py, {"encoding": "utf-8"})))?;
        Ok(())
    })();
    match written {
        Ok(()) => Ok(()),
        Err(e) => {
            if handling(py, Some(&e), || caught(py, &e, &name(py, "OSError")?))? {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

// The .py's main(), which had no docstring: the arguments, the agent's run,
// the result written, then sys.exit with the exit code.
#[pyfunction]
#[pyo3(name = "main")]
fn run_main<'py>(py: Python<'py>) -> PyResult<()> {
    name(py, "_watch_parent")?.call0()?;
    let parser = name(py, "argparse")?.getattr("ArgumentParser")?;
    let parser = parser.call(
        (),
        Some(&display!(py, {
            "prog": "android_agent_child",
            "description": "One android_agent run (spawned by android_agent/service.py)",
        })),
    )?;
    let add = parser.getattr("add_argument")?;
    add.call(("--task",), Some(&display!(py, {"type": name(py, "str")?, "default": none(py), "help": "The task to run"})))?;
    let add = parser.getattr("add_argument")?;
    add.call(
        ("--standby",),
        Some(&display!(py, {
            "action": "store_true",
            "help": "Get ready first, then read the task as one JSON line on stdin",
        })),
    )?;
    let add = parser.getattr("add_argument")?;
    add.call(("--provider",), Some(&display!(py, {"type": name(py, "str")?, "required": true, "help": "LLM provider"})))?;
    let add = parser.getattr("add_argument")?;
    add.call(("--model",), Some(&display!(py, {"type": name(py, "str")?, "required": true, "help": "LLM model name"})))?;
    let add = parser.getattr("add_argument")?;
    add.call(("--speed",), Some(&display!(py, {"type": name(py, "str")?, "default": none(py), "help": "\"quality\" or \"fast\""})))?;
    let add = parser.getattr("add_argument")?;
    add.call(
        ("--result",),
        Some(&display!(py, {
            "type": name(py, "str")?,
            "default": none(py),
            "help": "Path to write the result JSON when complete",
        })),
    )?;
    let args = parser.call_method0("parse_args")?;
    if args.getattr("task")?.is_none() && !args.getattr("standby")?.is_truthy()? {
        parser.call_method1("error", ("--task is required",))?;
    }

    let mut result = display!(py, {"status": "error", "message": "child crashed before the agent returned"}).into_any();
    let mut exit_code = 1;
    let mut kept = Vec::new();
    let mut agent = None;
    // try: ... except KeyboardInterrupt: ... except Exception as e: ...
    // What is still being raised once the except blocks are done (nothing
    // matched, or a handler raised) is `in_flight`: the finally block runs
    // while it is handled, and then it goes on.
    let in_flight = match try_block(py, &args, &mut kept, &mut agent) {
        Ok(returned) => {
            result = returned;
            exit_code = 0;
            None
        }
        Err(e) => {
            let handled = handling(py, Some(&e), || -> PyResult<Option<Caught<'py>>> {
                if caught(py, &e, &name(py, "KeyboardInterrupt")?)? {
                    return Ok(Some(Caught::Interrupted));
                }
                // the result file is the error channel
                if caught(py, &e, &name(py, "Exception")?)? {
                    let kind = name(py, "type")?.call1((e.value(py),))?.getattr("__name__")?;
                    let message = fstring(py, &[&kind, &text(py, ": "), e.value(py).as_any()])?;
                    return Ok(Some(Caught::Failed(display!(py, {"status": "error", "message": message}).into_any())));
                }
                Ok(None)
            });
            match handled {
                Ok(Some(Caught::Interrupted)) => {
                    result = display!(py, {"status": "stopped", "message": "interrupted (Ctrl+C)"}).into_any();
                    exit_code = 130;
                    None
                }
                Ok(Some(Caught::Failed(failed))) => {
                    result = failed;
                    None
                }
                Ok(None) => Some(e),
                Err(raised) => Some(raised),
            }
        }
    };
    // finally: an error it raises takes the place of the one in flight.
    handling(py, in_flight.as_ref(), || write_result(py, &args, &result))?;
    if let Some(e) = in_flight {
        return Err(e);
    }
    name(py, "sys")?.call_method1("exit", (exit_code,))?;
    // The try block's names go when main ends, as its frame's did.
    drop((kept, agent));
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes the runner service.py was: its docstring and
/// its names in the .py's order, the standard library modules it imported
/// included (imported here, as the other light ones are).
///
/// `m` carries Python's name for the module (lib.rs makes it so), which the
/// class and the functions then report as their __module__.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    m.add("sys", py.import("sys")?)?;
    m.add("Path", py.import("pathlib")?.getattr("Path")?)?;
    for name in ["argparse", "base64", "io", "json", "os", "signal", "threading", "time"] {
        m.add(name, py.import(name)?)?;
    }
    // The phone's screen as the app gets it: its longest side in px, and JPEG quality
    m.add("FRAME_EDGE", 900)?;
    m.add("FRAME_QUALITY", 70)?;
    m.add_class::<LiveView>()?;
    let class = py.get_type::<LiveView>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    // What Python 3.13 gave the class statement: its line in the .py and the
    // attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 41)?;
        class.setattr("__static_attributes__", ("folder", "frames", "lock"))?;
    }
    m.add_function(pyo3::wrap_pyfunction!(watch_parent, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(run_main, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
