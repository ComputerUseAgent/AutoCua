//! service: computer use's android_agent sub-agent tool (macOS, Windows,
//! Linux). AndroidLink is one request's phone check (checking, then online or
//! offline, with a standby child started once the phone answers); start,
//! outcome and stop are the runs, each a child process
//! (android_agent_child/service.py) in a run folder of its own.
//!
//! The Rust copy of universal_tool/android_agent/service.py. Python imports
//! it under its old dotted name, AutoCua.universal_tool.android_agent.service
//! (android_agent/__init__.py points that name here), and finds the same
//! names in the same order, with the same values, return values, errors,
//! prints, log lines and calls. Python's own rules decide every result, so
//! they are called, not imitated: Path, subprocess, os, json, the builtins,
//! the str, dict and list methods, the operators and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module its docstring and _build. Then the loader
//! binds what service.py's import lines bound (atexit, ctypes, json, os,
//! shutil, signal, subprocess, sys, threading, time, uuid, Path, data_root,
//! IS_COMPILED, _PROVIDER_ENV_KEY) and calls _build(), which runs the rest of
//! service.py's lines in their order: the constants, AndroidAgentError,
//! `_links` and `_runs`, stop_all and its atexit.register, each function
//! where its def was, and the class. The __file__-based constants are made
//! from the extension's folder and universal_tool/android_agent/service.py
//! with the module's own Path, so they come out as they did.
//!
//! Every global a line reads (subprocess, time, _runs, TIMEOUT_SEC, len,
//! str, ...) is read from this module at that moment, as Python reads a
//! global, so a caller or a test that replaces one is obeyed. The class
//! keeps its attributes in its __dict__, and its methods reach them and each
//! other through the instance, so a subclass that replaces one is obeyed. An
//! error is raised the way Python's raise statement raises it, so the error
//! being handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PyString, PyTuple, PyType};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{self, caught, import_from, now, only_self, unbound_local};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// service.py's docstring.
const MODULE_DOC: &str = "The Android agent as a sub-agent of the main agent, on the person's
Android phone plugged into this computer by USB (USB debugging on), or, when
the desktop app's Wireless / Wired toggle says Wireless, one Settings
connected that is on the same Wi-Fi (Wireless debugging on).

Two halves, as mac/controller/tool/ios_agent.py has:

AndroidLink: the request's phone check. The moment the main agent starts a
request, it looks for an Android phone on USB (the newest one Settings \u{2192}
Connect Device remembered, else the one plugged in), or, with the app's
toggle on Wireless, a remembered one on the Wi-Fi only (the connector's
WIRELESS; a terminal run keeps the cable), and checks it in the background:
the phone answers over adb, its
helper starts and reads one screen, then stops again
(android_connector/session.py's confirm). Meanwhile android_agent's slot in
<sub_agents> reads `connecting`; once the check passes, it reads `empty`. No
phone, or a check that fails, and it reads `offline`. While it is online the
phone is looked at again every WATCH_SEC, so a pulled cable (or a phone gone
from the Wi-Fi) takes it offline.

The runs: `sub_agent {\"agent_type\": \"android_agent\"}` starts the Android agent
on a task and returns at once; the controller polls `outcome` until the run is
over. Each run is a subprocess (android_agent_child/service.py), because the
Android AgentService constructor deletes CWD-relative conversation/, debug/ and
raw_reasoning/ (the main agent's LIVE folders), so the child gets a working
directory of its own. The child starts the phone's helper itself: a phone has
one, and this process holds none while a run works.

adb is never fetched from here: a computer that has not set up Android yet
(adb on PATH, or fetched once on a terminal, after its y/N) just has no
android_agent, without a download at every request; one small line says how
to install it.
";

/// AndroidAgentError's docstring.
const ERROR_DOC: &str = "android_agent could not be started.";

/// AndroidLink's docstring.
const LINK_DOC: &str = "One request's phone check: \"checking\", then \"online\" or \"offline\".";

/// The lines of service.py the two class statements are on: each class's
/// __firstlineno__ from Python 3.13.
const ERROR_LINE: i32 = 70;
const LINK_LINE: i32 = 132;

/// What AndroidLink's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 10] =
    ["_lock", "_released", "_standby", "_standby_for", "_state", "_t0", "address", "name", "reason", "serial"];

/// _TASK_TEMPLATE.
const TASK_TEMPLATE: &str = "{task}

How to finish:
- Your final `done` call's value is the report the main agent receives, and it is all it receives. State what you did, the result with the exact values and names that matter, what the screen shows at the end, and anything you could not do.
";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.android_agent.service", super::super::NATIVE)).map(Bound::unbind))
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

/// `raise <kind>(<message>)`: the global `kind` read first, then the
/// message made, then the error made and raised, in Python's order.
fn throw<'py>(py: Python<'py>, kind: &str, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyErr {
    match module(py) {
        Ok(m) => controller::throw(&m, kind, message),
        Err(e) => e,
    }
}

/// An `except Exception:` block: `body` runs while `e` is being handled,
/// with Exception read from this module as Python reads it there. What does
/// not match passes on untouched.
fn except_exception<T>(py: Python<'_>, e: PyErr, body: impl FnOnce(&PyErr) -> PyResult<T>) -> PyResult<T> {
    controller::except_exception(&module(py)?, e, body)
}

/// An `except <kinds>:` block: `kinds` (a class, or a tuple of them) is
/// made while `e` is being handled, as Python reads it at that moment, then
/// `body` runs when `e` matches. What does not match passes on untouched.
fn except_kinds<'py, T>(
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

/// `(<a>, <b>)` of two globals, as an except clause's tuple reads them.
fn pair_of<'py>(py: Python<'py>, a: &str, b: &str) -> PyResult<Obj<'py>> {
    let first = g(py, a)?;
    let second = g(py, b)?;
    Ok(PyTuple::new(py, [first, second])?.into_any())
}

/// `from <from> import <wanted>, ...`, run when the line runs.
fn imported<'py, const N: usize>(py: Python<'py>, from: &str, wanted: [&str; N]) -> PyResult<[Obj<'py>; N]> {
    let names = import_from(&module(py)?, from, &wanted)?;
    names
        .try_into()
        .map_err(|_| PyRuntimeError::new_err("from-import gave the wrong count of names"))
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

/// `a or b`: a when it is true, else b, made only then.
fn or<'py>(a: Obj<'py>, b: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    if a.is_truthy()? {
        Ok(a)
    } else {
        b()
    }
}

/// `a == b` in an `if`: Python's == on the two, then the truth of the answer.
fn eq<'py>(a: &Obj<'py>, b: impl IntoPyObject<'py>) -> PyResult<bool> {
    a.rich_compare(b, CompareOp::Eq)?.is_truthy()
}

/// `a != b` in an `if`.
fn ne<'py>(a: &Obj<'py>, b: impl IntoPyObject<'py>) -> PyResult<bool> {
    a.rich_compare(b, CompareOp::Ne)?.is_truthy()
}

/// The keyword part of a call, in the order given.
fn kwargs<'py>(py: Python<'py>, items: &[(&str, &Obj<'py>)]) -> PyResult<Bound<'py, PyDict>> {
    controller::dict(py, items)
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

/// Whether a comprehension or a generator takes iter() a second time of the
/// iterator it loops over: Python 3.13's code does, 3.12's and 3.14's loop
/// over it as it is.
fn iter_again(py: Python<'_>) -> bool {
    let version = py.version_info();
    version >= (3, 13) && version < (3, 14)
}

/// What a list or dict comprehension loops over: iter() of `source`, then on
/// Python 3.13 iter() of that again.
fn comprehension<'py>(source: &Obj<'py>) -> PyResult<Bound<'py, PyIterator>> {
    let items = source.try_iter()?;
    if iter_again(source.py()) {
        return items.try_iter();
    }
    Ok(items)
}

/// An argument as the call gave it, or Missing when the call left it out
/// and the default Python's def bound applies.
enum Given<'py> {
    Value(Obj<'py>),
    Missing,
}

impl<'py> FromPyObject<'_, 'py> for Given<'py> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, 'py, PyAny>) -> PyResult<Self> {
        Ok(Given::Value(obj.to_owned()))
    }
}

/// An f-string, made as Python makes one: each {value} is formatted the
/// moment it is reached (format(value, "")), and the pieces are joined at
/// the end.
struct Fmt<'py> {
    py: Python<'py>,
    pieces: Vec<Obj<'py>>,
}

impl<'py> Fmt<'py> {
    fn new(py: Python<'py>) -> Self {
        Fmt { py, pieces: Vec::new() }
    }

    /// A literal part.
    fn s(mut self, part: &str) -> Self {
        self.pieces.push(lit(self.py, part));
        self
    }

    /// A {value} part.
    fn v(mut self, value: &Obj<'py>) -> PyResult<Self> {
        self.pieces.push(format_value(value, "")?.into_any());
        Ok(self)
    }

    /// A {value:spec} part.
    fn spec(mut self, value: &Obj<'py>, spec: &str) -> PyResult<Self> {
        self.pieces.push(format_value(value, spec)?.into_any());
        Ok(self)
    }

    fn done(self) -> PyResult<Obj<'py>> {
        let pieces: Vec<&Obj<'py>> = self.pieces.iter().collect();
        Ok(join_str(self.py, &pieces)?.into_any())
    }
}

/// `with self._lock:` around `body`: Some(answer) when the block ran to its
/// end (or left it with a return), None when the lock's __exit__ swallowed
/// an error the block raised, so the lines after the block run.
fn locked<'py, T>(slf: &Obj<'py>, body: impl FnOnce() -> PyResult<T>) -> PyResult<Option<T>> {
    let lock = slf.getattr("_lock")?;
    with(&lock, |_| body())
}

// ---------------------------------------------------------------------------
// The generator expressions
// ---------------------------------------------------------------------------

// The generator expressions of _check, _watch and start, run as Python runs
// one (as vault/service.rs runs its own). Made holding iter() of what it
// loops over, on Python 3.13 its first step takes iter() of that again, as
// that version's generator code does; then it runs its body one value per
// next(). A StopIteration from its own body comes out as RuntimeError (PEP
// 479), and once it has ended or failed it gives nothing more. It shows
// itself as Python showed that generator. A plain comment: a doc comment
// would give the class a docstring, and a generator has none.
#[pyclass(frozen, name = "generator", module = "builtins")]
struct Genexpr {
    body: Body,
    /// iter() of what it loops over, made when the generator was made.
    given: Py<PyIterator>,
    /// iter() of `given` on Python 3.13, taken when the generator first runs.
    items: PyOnceLock<Py<PyIterator>>,
    /// The generator has ended or failed.
    done: AtomicBool,
    /// A step is running.
    running: AtomicBool,
}

/// Which generator expression a Genexpr runs, with the variable it reads
/// from the function that made it.
enum Body {
    /// _check's `(s for s in saved if s in ready)`.
    Ready { ready: Py<PyAny> },
    /// _check's `(str(p) for p in (res.get("error"), res.get("hint")) if p)`.
    Shown,
    /// _watch's `(line.split("\t", 1) for line in rows if "\t" in line)`.
    Pairs,
    /// start's `(r["subprocess"].poll() is None for r in _runs)`.
    Working,
}

impl Genexpr {
    /// The generator made: iter() of `source` taken now.
    fn made<'py>(source: &Obj<'py>, body: Body) -> PyResult<Bound<'py, Genexpr>> {
        let given = source.try_iter()?;
        let generator = Genexpr {
            body,
            given: given.unbind(),
            items: PyOnceLock::new(),
            done: AtomicBool::new(false),
            running: AtomicBool::new(false),
        };
        Bound::new(source.py(), generator)
    }

    /// The body for one item: Some(value) to give it, None to go on.
    fn step<'py>(&self, item: Obj<'py>) -> PyResult<Option<Obj<'py>>> {
        let py = item.py();
        match &self.body {
            Body::Ready { ready } => {
                if ready.bind(py).contains(&item)? {
                    Ok(Some(item))
                } else {
                    Ok(None)
                }
            }
            Body::Shown => {
                if !item.is_truthy()? {
                    return Ok(None);
                }
                let str_ = g(py, "str")?;
                Ok(Some(str_.call1((item,))?))
            }
            Body::Pairs => {
                if !item.contains("\t")? {
                    return Ok(None);
                }
                Ok(Some(item.call_method1("split", ("\t", 1))?))
            }
            Body::Working => {
                let running = item.get_item("subprocess")?.call_method0("poll")?.is_none();
                Ok(Some(boolean(py, running)))
            }
        }
    }

    /// The function that made it, as its __qualname__ names the generator.
    fn qualname(&self) -> &'static str {
        match self.body {
            Body::Ready { .. } | Body::Shown => "AndroidLink._check.<locals>.<genexpr>",
            Body::Pairs => "AndroidLink._watch.<locals>.<genexpr>",
            Body::Working => "start.<locals>.<genexpr>",
        }
    }
}

#[pymethods]
impl Genexpr {
    fn __iter__<'py>(slf: &Bound<'py, Self>) -> Bound<'py, Self> {
        slf.clone()
    }

    fn __next__<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Obj<'py>>> {
        let py = slf.py();
        let me = slf.get();
        if me.running.load(Ordering::SeqCst) {
            return Err(now(py, PyValueError::new_err("generator already executing")));
        }
        if me.done.load(Ordering::SeqCst) {
            return Ok(None);
        }
        me.running.store(true, Ordering::SeqCst);
        let step = (|| -> PyResult<Option<Obj<'py>>> {
            let items = if iter_again(py) {
                let again = || me.given.bind(py).as_any().try_iter().map(Bound::unbind);
                me.items.get_or_try_init(py, again)?.bind(py).clone()
            } else {
                me.given.bind(py).clone()
            };
            for item in items {
                if let Some(value) = me.step(item?)? {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        })();
        me.running.store(false, Ordering::SeqCst);
        match step {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => {
                me.done.store(true, Ordering::SeqCst);
                Ok(None)
            }
            Err(e) => {
                me.done.store(true, Ordering::SeqCst);
                if !e.is_instance_of::<PyStopIteration>(py) {
                    return Err(e);
                }
                let error = py.get_type::<PyRuntimeError>().call1(("generator raised StopIteration",))?;
                error.setattr("__cause__", e.value(py))?;
                error.setattr("__context__", e.value(py))?;
                Err(PyErr::from_value(error))
            }
        }
    }

    fn __repr__(slf: &Bound<'_, Self>) -> String {
        format!("<generator object {} at {:p}>", slf.get().qualname(), slf.as_ptr())
    }
}

// ---------------------------------------------------------------------------
// stop_all, _log, _adb, _usb_phones
// ---------------------------------------------------------------------------

#[pyfunction]
#[doc = "Quitting mid-run must not leave the phone driven: the run acts on the\n\
phone and calls the model with nobody left to report to.\n\n\
Runs at exit, and from the app window's closing event too, because not\n\
every quit runs atexit: Cmd+Q on macOS leaves through NSApplication\n\
terminate:, which calls C exit()."]
fn stop_all(py: Python<'_>) -> PyResult<()> {
    // for run in list(_runs):
    let list = g(py, "list")?;
    let runs = list.call1((g(py, "_runs")?,))?;
    for run in runs.try_iter()? {
        let run = run?;
        let stopped = (|| -> PyResult<()> {
            g(py, "stop")?.call1((&run,))?;
            Ok(())
        })();
        if let Err(e) = stopped {
            except_exception(py, e, |_| Ok(()))?;
        }
    }
    // for link in list(_links):
    let list = g(py, "list")?;
    let links = list.call1((g(py, "_links")?,))?;
    for link in links.try_iter()? {
        let link = link?;
        if let Err(e) = link.call_method0("release") {
            except_exception(py, e, |_| Ok(()))?;
        }
    }
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_log")]
#[doc = "What the phone check did and why, in android_agent/phone_link.log under\n\
the data folder (and on stdout). The main agent only ever sees \"online\" or\n\
\"offline\": without this, a failed check leaves no trace anywhere."]
fn log_line<'py>(py: Python<'py>, msg: Obj<'py>) -> PyResult<()> {
    // line = f"{time.strftime('%H:%M:%S')} {msg}"
    let stamp = g(py, "time")?.getattr("strftime")?.call1(("%H:%M:%S",))?;
    let line = Fmt::new(py).v(&stamp)?.s(" ").v(&msg)?.done()?;
    // print(f"[android_agent] {line}", flush=True)
    let print = g(py, "print")?;
    let shown = Fmt::new(py).s("[android_agent] ").v(&line)?.done()?;
    print.call((shown,), Some(&kwargs(py, &[("flush", &boolean(py, true))])?))?;
    let wrote = (|| -> PyResult<()> {
        // path = data_root() / "android_agent" / "phone_link.log"
        let path = g(py, "data_root")?.call0()?.div("android_agent")?.div("phone_link.log")?;
        // path.parent.mkdir(parents=True, exist_ok=True)
        let mkdir = path.getattr("parent")?.getattr("mkdir")?;
        let yes = boolean(py, true);
        mkdir.call((), Some(&kwargs(py, &[("parents", &yes), ("exist_ok", &yes)])?))?;
        // with open(path, "a", encoding="utf-8") as f:
        //     f.write(line + "\n")
        let open = g(py, "open")?;
        let file = open.call((&path, "a"), Some(&kwargs(py, &[("encoding", &lit(py, "utf-8"))])?))?;
        with(&file, |f| -> PyResult<()> {
            let write = f.getattr("write")?;
            write.call1((line.add("\n")?,))?;
            Ok(())
        })?;
        Ok(())
    })();
    match wrote {
        Ok(()) => Ok(()),
        Err(e) => except_exception(py, e, |_| Ok(())),
    }
}

#[pyfunction]
#[pyo3(name = "_adb")]
#[doc = "adb when this computer already has it (on PATH, or fetched once on a\n\
terminal), else None: never a download from here."]
fn adb_path(py: Python<'_>) -> PyResult<Obj<'_>> {
    let [own_adb] = imported(py, "AutoCua.android_connector.adb", ["OWN_ADB"])?;
    // return shutil.which("adb") or (str(OWN_ADB) if OWN_ADB.is_file() else None)
    let found = g(py, "shutil")?.getattr("which")?.call1(("adb",))?;
    if found.is_truthy()? {
        return Ok(found);
    }
    if own_adb.call_method0("is_file")?.is_truthy()? {
        let str_ = g(py, "str")?;
        str_.call1((own_adb,))
    } else {
        Ok(none(py))
    }
}

#[pyfunction]
#[pyo3(name = "_usb_phones")]
#[doc = "{serial: state} for the real phones adb sees on USB (no emulators, as\n\
ios_agent never drives a simulator)."]
fn usb_phones<'py>(py: Python<'py>, adb: Obj<'py>) -> PyResult<Obj<'py>> {
    let [run_adb, wireless, devices] =
        imported(py, "AutoCua.android_connector.adb", ["_adb", "_wireless", "devices"])?;
    run_adb.call1((&adb, "start-server"))?;
    // {serial: state for serial, state in devices(adb)
    //  if not _wireless(serial) and not serial.startswith("emulator-")}
    let phones = PyDict::new(py);
    for pair in comprehension(&devices.call1((&adb,))?)? {
        let (serial, state) = unpack2(&pair?)?;
        if wireless.call1((&serial,))?.is_truthy()? {
            continue;
        }
        if serial.call_method1("startswith", ("emulator-",))?.is_truthy()? {
            continue;
        }
        phones.set_item(serial, state)?;
    }
    Ok(phones.into_any())
}

// ---------------------------------------------------------------------------
// AndroidLink
// ---------------------------------------------------------------------------

// One request's phone check (its docstring is set in register()).
#[pyclass(frozen, dict, weakref, subclass)]
pub struct AndroidLink;

/// `self._fail(<reason>)`: the method looked up first, then the reason made.
fn fail<'py>(slf: &Obj<'py>, reason: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<()> {
    let method = slf.getattr("_fail")?;
    method.call1((reason()?,))?;
    Ok(())
}

/// `self._fail(HINTS.get(state, f"the phone is in the {state!r} state"))`.
fn fail_state<'py>(slf: &Obj<'py>, hints: &Obj<'py>, state: &Obj<'py>) -> PyResult<()> {
    let py = slf.py();
    fail(slf, || {
        let get = hints.getattr("get")?;
        let fallback = Fmt::new(py).s("the phone is in the ").v(&state.repr()?.into_any())?.s(" state").done()?;
        get.call1((state, fallback))
    })
}

/// `child, self._standby = self._standby, None` under the lock: the standby
/// taken. A lock that swallowed an error leaves `child` unbound.
fn take_child<'py>(slf: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = slf.py();
    let taken = locked(slf, || -> PyResult<Obj<'py>> {
        let child = slf.getattr("_standby")?;
        slf.setattr("_standby", py.None())?;
        Ok(child)
    })?;
    taken.ok_or_else(|| unbound_local(py, "child"))
}

#[pymethods]
impl AndroidLink {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AndroidLink
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        only_self("AndroidLink.__init__()", args, kwargs)?;
        let lock = g(py, "threading")?.getattr("Lock")?.call0()?;
        slf.setattr("_lock", lock)?;
        slf.setattr("_state", "offline")?;
        slf.setattr("serial", py.None())?;
        // adb's serial for it: the USB serial, or "ip:port" on Wi-Fi
        slf.setattr("address", py.None())?;
        slf.setattr("name", "Android phone")?;
        slf.setattr("reason", "no Android phone is plugged in by USB with USB debugging on")?;
        slf.setattr("_released", false)?;
        let t0 = g(py, "time")?.getattr("monotonic")?.call0()?;
        slf.setattr("_t0", t0)?;
        // The main agent's provider, model, api_key and speed: an
        // android_agent child is started with them when the phone answers
        slf.setattr("_standby_for", py.None())?;
        slf.setattr("_standby", py.None())?;
        Ok(())
    }

    #[classmethod]
    #[pyo3(signature = (standby = None), text_signature = "($cls, standby=None)")]
    #[doc = "Check for the phone in the background. Returns at once. standby:\n\
{\"provider\", \"model\", \"api_key\", \"speed\"}, as start() will get them,\n\
to have the child ready when the phone answers."]
    fn start<'py>(cls: &Bound<'py, PyType>, standby: Option<Obj<'py>>) -> PyResult<Obj<'py>> {
        let py = cls.py();
        let link = cls.call0()?;
        link.setattr("_standby_for", standby.unwrap_or_else(|| none(py)))?;
        if g(py, "IS_COMPILED")?.is_truthy()? {
            link.setattr("reason", "android_agent is unavailable in the packaged app")?;
            return Ok(link);
        }
        let adb = match g(py, "_adb").and_then(|find| find.call0()) {
            Ok(adb) => adb,
            Err(e) => except_exception(py, e, |_| Ok(none(py)))?,
        };
        if adb.is_none() {
            // No adb yet: offline at once, no download. One small terminal line
            // says so, with the command that installs it (after its own y/N).
            let [fetch] = imported(py, "AutoCua.android_connector.adb", ["FETCH"])?;
            link.setattr("reason", "Android automation paused: adb is not installed")?;
            let print = g(py, "print")?;
            let line = Fmt::new(py)
                .s("\u{1F916} ")
                .v(&link.getattr("reason")?)?
                .s(". Install it with:\n  ")
                .v(&fetch)?
                .done()?;
            print.call((line,), Some(&kwargs(py, &[("flush", &boolean(py, true))])?))?;
            return Ok(link);
        }
        link.setattr("_state", "checking")?;
        g(py, "_links")?.call_method1("append", (&link,))?;
        // threading.Thread(target=link._check, args=(adb,), daemon=True).start()
        let thread = g(py, "threading")?.getattr("Thread")?;
        let target = link.getattr("_check")?;
        let args = PyTuple::new(py, [&adb])?.into_any();
        let started = thread.call((), Some(&kwargs(py, &[("target", &target), ("args", &args), ("daemon", &boolean(py, true))])?))?;
        started.call_method0("start")?;
        Ok(link)
    }

    #[doc = "checking, online or offline."]
    fn state<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let state = locked(slf.as_any(), || slf.getattr("_state"))?;
        Ok(state.unwrap_or_else(|| none(py)))
    }

    #[doc = "The request is over. Nothing is held: the check stopped its helper,\n\
and a run's helper goes with its child."]
    fn release(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let left = locked(slf.as_any(), || -> PyResult<bool> {
            if slf.getattr("_released")?.is_truthy()? {
                return Ok(true);
            }
            slf.setattr("_released", true)?;
            slf.setattr("_state", "offline")?;
            Ok(false)
        })?;
        if left == Some(true) {
            return Ok(());
        }
        if slf.getattr("serial")?.is_truthy()? {
            let log = g(py, "_log")?;
            let line = Fmt::new(py).s("released after ").v(&slf.call_method0("_elapsed")?)?.done()?;
            log.call1((line,))?;
        }
        slf.call_method0("_drop_standby")?;
        slf.call_method0("_forget")?;
        Ok(())
    }

    fn _elapsed<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // f"{time.monotonic() - self._t0:.1f}s"
        let seconds = g(py, "time")?.getattr("monotonic")?.call0()?.sub(slf.getattr("_t0")?)?;
        Fmt::new(py).spec(&seconds, ".1f")?.s("s").done()
    }

    // -- the check thread --
    fn _check<'py>(slf: &Bound<'py, Self>, adb: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let checked = (|| -> PyResult<()> {
            let [hints] = imported(py, "AutoCua.android_connector.adb", ["HINTS"])?;
            let [connector] = imported(py, "AutoCua.android_connector", ["session"])?;
            let [android_session, paired_devices, wifi_saved] = imported(
                py,
                "AutoCua.android_connector.session",
                ["android_session", "paired_devices", "wifi_saved"],
            )?;
            let wireless = connector.getattr("WIRELESS")?.is_truthy()?;
            let link = lit(py, if wireless { "wireless" } else { "wired" });
            let serial;
            if eq(&link, "wireless")? {
                // The app's toggle says Wireless: the Wi-Fi only, a phone
                // Settings connected that announces Wireless debugging. The
                // cable is left alone, so with none on the Wi-Fi the agent is
                // offline, cable or not.
                serial = wifi_saved.call1((&adb,))?;
                if serial.is_none() {
                    fail(me, || {
                        Ok(lit(
                            py,
                            "no Android phone connected in Settings is on Wi-Fi (Wireless \
                             debugging on, the same Wi-Fi as this computer)",
                        ))
                    })?;
                    return Ok(());
                }
            } else {
                // Wired, the default (and every terminal run): the cable only.
                let plugged = g(py, "_usb_phones")?.call1((&adb,))?;
                // ready = [s for s, state in plugged.items() if state == "device"]
                let ready = PyList::empty(py);
                for pair in comprehension(&plugged.call_method0("items")?)? {
                    let (s, state) = unpack2(&pair?)?;
                    if eq(&state, "device")? {
                        ready.append(s)?;
                    }
                }
                // saved = [d["serial"] for d in paired_devices() if d.get("serial") in plugged]
                let saved = PyList::empty(py);
                for d in comprehension(&paired_devices.call0()?)? {
                    let d = d?;
                    if plugged.contains(d.call_method1("get", ("serial",))?)? {
                        saved.append(d.get_item("serial")?)?;
                    }
                }
                if saved.is_truthy()? {
                    // A phone Settings connected is plugged in: that one, never another.
                    let next = g(py, "next")?;
                    let first = Genexpr::made(saved.as_any(), Body::Ready { ready: ready.clone().into_any().unbind() })?;
                    let found = next.call1((first, py.None()))?;
                    if found.is_none() {
                        let state = plugged.get_item(saved.as_any().get_item(0)?)?;
                        fail_state(me, &hints, &state)?;
                        return Ok(());
                    }
                    serial = found;
                } else if eq(&g(py, "len")?.call1((&ready,))?, 1)? {
                    serial = ready.as_any().get_item(0)?;
                } else if ready.is_truthy()? {
                    fail(me, || {
                        Ok(lit(
                            py,
                            "more than one Android phone is plugged in: connect the one to use in \
                             Settings \u{2192} Connect Device \u{2192} Android",
                        ))
                    })?;
                    return Ok(());
                } else if plugged.is_truthy()? {
                    let next = g(py, "next")?;
                    let values = g(py, "iter")?.call1((plugged.call_method0("values")?,))?;
                    let state = next.call1((values,))?;
                    fail_state(me, &hints, &state)?;
                    return Ok(());
                } else {
                    fail(me, || Ok(lit(py, "no Android phone is plugged in by USB with USB debugging on")))?;
                    return Ok(());
                }
            }
            slf.setattr("serial", &serial)?;
            // _log(f"checking {serial}{' on Wi-Fi' if link == 'wireless' else ''}")
            let log = g(py, "_log")?;
            let line = Fmt::new(py).s("checking ").v(&serial)?;
            let on_wifi = lit(py, if eq(&link, "wireless")? { " on Wi-Fi" } else { "" });
            log.call1((line.v(&on_wifi)?.done()?,))?;
            // A phone this process already reads (the app's own Mobile use →
            // Android pick) answers as it is: confirm() leaves it running,
            // unless it reads it the other way.
            let confirm = android_session.getattr("confirm")?;
            let res = confirm.call((&serial,), Some(&kwargs(py, &[("keep", &boolean(py, false)), ("link", &link)])?))?;
            if !res.call_method1("get", ("ok",))?.is_truthy()? {
                fail(me, || {
                    let join = lit(py, " - ").getattr("join")?;
                    let error = res.call_method1("get", ("error",))?;
                    let hint = res.call_method1("get", ("hint",))?;
                    let parts = PyTuple::new(py, [error, hint])?;
                    let shown = Genexpr::made(parts.as_any(), Body::Shown)?;
                    or(join.call1((shown,))?, || Ok(lit(py, "the phone could not be checked")))
                })?;
                return Ok(());
            }
            let name = or(res.call_method1("get", ("model",))?, || slf.getattr("name"))?;
            slf.setattr("name", name)?;
            // What the watch looks for in `adb devices`: "ip:port" on Wi-Fi.
            let address = or(res.call_method1("get", ("serial",))?, || Ok(serial.clone()))?;
            slf.setattr("address", address)?;
            let left = locked(me, || -> PyResult<bool> {
                if slf.getattr("_released")?.is_truthy()? {
                    return Ok(true);
                }
                slf.setattr("_state", "online")?;
                Ok(false)
            })?;
            if left == Some(true) {
                return Ok(());
            }
            // _log(f"online after {self._elapsed()}: {self.name} ({serial}"
            //      f"{'' if self.address == serial else ', Wi-Fi ' + self.address})")
            let log = g(py, "_log")?;
            let line = Fmt::new(py)
                .s("online after ")
                .v(&slf.call_method0("_elapsed")?)?
                .s(": ")
                .v(&slf.getattr("name")?)?
                .s(" (")
                .v(&serial)?;
            let wifi = if eq(&slf.getattr("address")?, &serial)? {
                lit(py, "")
            } else {
                lit(py, ", Wi-Fi ").add(slf.getattr("address")?)?
            };
            log.call1((line.v(&wifi)?.s(")").done()?,))?;
            slf.getattr("_start_standby")?.call1((&android_session,))?;
            slf.getattr("_watch")?.call1((&adb,))?;
            Ok(())
        })();
        match checked {
            Ok(()) => Ok(()),
            Err(e) => except_exception(py, e, |e| {
                fail(me, || Fmt::new(py).s("the phone check failed: ").v(e.value(py))?.done())
            }),
        }
    }

    #[doc = "Keep the phone's state true: a pulled cable takes android_agent offline."]
    fn _watch<'py>(slf: &Bound<'py, Self>, adb: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let [run_adb] = imported(py, "AutoCua.android_connector.adb", ["_adb"])?;
        loop {
            let sleep = g(py, "time")?.getattr("sleep")?;
            sleep.call1((g(py, "WATCH_SEC")?,))?;
            let left = locked(me, || -> PyResult<bool> {
                if slf.getattr("_released")?.is_truthy()? {
                    return Ok(true);
                }
                ne(&slf.getattr("_state")?, "online")
            })?;
            if left == Some(true) {
                return Ok(());
            }
            let read = (|| -> PyResult<Obj<'py>> {
                let lines = run_adb.call1((&adb, "devices"))?.getattr("stdout")?.call_method0("splitlines")?;
                slice(&lines, Some(&int(py, 1)?), None)
            })();
            let rows = match read {
                Ok(rows) => rows,
                Err(e) => {
                    except_exception(py, e, |_| Ok(()))?;
                    continue;
                }
            };
            // states = dict(line.split("\t", 1) for line in rows if "\t" in line)
            let dict = g(py, "dict")?;
            let states = dict.call1((Genexpr::made(&rows, Body::Pairs)?,))?;
            let get = states.getattr("get")?;
            let state = get.call1((slf.getattr("address")?, ""))?.call_method0("strip")?;
            if ne(&state, "device")? {
                fail(me, || {
                    let unplugged = eq(&slf.getattr("address")?, slf.getattr("serial")?)?;
                    Ok(lit(py, if unplugged { "the phone was unplugged" } else { "the phone left the Wi-Fi" }))
                })?;
                return Ok(());
            }
        }
    }

    fn _fail<'py>(slf: &Bound<'py, Self>, reason: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        locked(slf.as_any(), || -> PyResult<()> {
            slf.setattr("_state", "offline")?;
            if !slf.getattr("_released")?.is_truthy()? {
                let kept = or(reason.clone(), || slf.getattr("reason"))?;
                slf.setattr("reason", kept)?;
            }
            Ok(())
        })?;
        let log = g(py, "_log")?;
        let line = Fmt::new(py)
            .s("offline after ")
            .v(&slf.call_method0("_elapsed")?)?
            .s(": ")
            .v(&reason)?
            .done()?;
        log.call1((line,))?;
        slf.call_method0("_drop_standby")?;
        slf.call_method0("_forget")?;
        Ok(())
    }

    // -- the standby child --
    #[doc = "Start the request's android_agent child now, built (the phone\n\
connected, as its scanner does) and waiting for its task\n\
(android_agent_child/service.py --standby): when the main agent sends one,\n\
nothing loads first (a busy Mac took 43 s to). Not while this process\n\
holds the phone's one helper itself (the app's Mobile use \u{2192} Android\n\
pick): start() lets that go only when a task really comes."]
    fn _start_standby<'py>(slf: &Bound<'py, Self>, android_session: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let args = slf.getattr("_standby_for")?;
        if !args.is_truthy()? {
            return Ok(());
        }
        let state = android_session.call_method0("status")?.call_method1("get", ("state",))?;
        if eq(&state, "connected")? {
            return Ok(());
        }
        let mut key = Vec::with_capacity(4);
        for field in ["provider", "model", "api_key", "speed"] {
            key.push(args.call_method1("get", (field,))?);
        }
        let key = PyTuple::new(py, key)?;
        let spawned = (|| -> PyResult<Obj<'py>> {
            let spawn = g(py, "_spawn")?;
            let serial = or(slf.getattr("address")?, || slf.getattr("serial"))?;
            let mut given = vec![serial];
            given.extend(key.iter());
            spawn.call1(PyTuple::new(py, given)?)
        })();
        let child = match spawned {
            Ok(child) => child,
            Err(e) => {
                return except_exception(py, e, |e| {
                    let log = g(py, "_log")?;
                    log.call1((Fmt::new(py).s("no android_agent started ahead: ").v(e.value(py))?.done()?,))?;
                    Ok(())
                });
            }
        };
        child.set_item("key", &key)?;
        let pid = child.get_item("proc")?.getattr("pid")?;
        let mut child = Some(child);
        locked(slf.as_any(), || -> PyResult<()> {
            if !slf.getattr("_released")?.is_truthy()? && eq(&slf.getattr("_state")?, "online")? {
                // self._standby, child = child, None
                let kept = child.take().map_or_else(|| none(py), |c| c);
                slf.setattr("_standby", kept)?;
            }
            Ok(())
        })?;
        match child {
            Some(child) => {
                // the request ended meanwhile
                g(py, "_end_standby")?.call1((child,))?;
            }
            None => {
                let log = g(py, "_log")?;
                let line = Fmt::new(py)
                    .s("android_agent started ahead, waiting for its task (pid ")
                    .v(&pid)?
                    .s(")")
                    .done()?;
                log.call1((line,))?;
            }
        }
        Ok(())
    }

    #[doc = "The standby child when it was built the way start() asks and still\n\
runs: it is that run's now. None otherwise, and start() starts one."]
    fn take_standby<'py>(
        slf: &Bound<'py, Self>,
        provider: Obj<'py>,
        model: Obj<'py>,
        api_key: Obj<'py>,
        speed: Obj<'py>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let child = take_child(slf.as_any())?;
        if child.is_none() {
            return Ok(none(py));
        }
        let key = child.get_item("key")?;
        let wanted = PyTuple::new(py, [provider, model, api_key, speed])?;
        if eq(&key, wanted)? && child.get_item("proc")?.call_method0("poll")?.is_none() {
            return Ok(child);
        }
        g(py, "_end_standby")?.call1((&child,))?;
        Ok(none(py))
    }

    fn _drop_standby(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let child = take_child(slf.as_any())?;
        if !child.is_none() {
            g(py, "_end_standby")?.call1((child,))?;
        }
        Ok(())
    }

    fn _forget(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        let removed = (|| -> PyResult<()> {
            g(py, "_links")?.call_method1("remove", (slf,))?;
            Ok(())
        })();
        match removed {
            Ok(()) => Ok(()),
            Err(e) => except_kinds(py, e, || g(py, "ValueError"), |_| Ok(())),
        }
    }
}

// ---------------------------------------------------------------- the runs --

#[pyfunction]
#[pyo3(name = "_milestone_notes")]
#[doc = "What the Android agent wrote to its scratchpad during this run: the\n\
salvage when the `done` value is thin or the run never reached `done`."]
fn milestone_notes<'py>(py: Python<'py>, run: Obj<'py>) -> PyResult<Obj<'py>> {
    let p = g(py, "_ANDROID_SCRATCHPAD")?.div("milestone")?.div("milestone.md")?;
    let read = (|| -> PyResult<Option<Obj<'py>>> {
        if p.call_method0("exists")?.is_truthy()? {
            let mtime = p.call_method0("stat")?.getattr("st_mtime")?;
            if mtime.rich_compare(run.get_item("started")?, CompareOp::Ge)?.is_truthy()? {
                let text = p.call_method("read_text", (), Some(&kwargs(py, &[("encoding", &lit(py, "utf-8"))])?))?;
                return Ok(Some(text.call_method0("strip")?));
            }
        }
        Ok(None)
    })();
    match read {
        Ok(Some(text)) => return Ok(text),
        Ok(None) => {}
        Err(e) => except_kinds(py, e, || g(py, "OSError"), |_| Ok(()))?,
    }
    Ok(lit(py, ""))
}

#[pyfunction]
#[pyo3(name = "_tail", signature = (path, lines = Given::Missing), text_signature = "(path, lines=30)")]
fn tail<'py>(py: Python<'py>, path: Obj<'py>, lines: Given<'py>) -> PyResult<Obj<'py>> {
    let lines = match lines {
        Given::Value(lines) => lines,
        Given::Missing => int(py, 30)?,
    };
    let read = (|| -> PyResult<Obj<'py>> {
        // "\n".join(path.read_text(encoding="utf-8", errors="replace").splitlines()[-lines:])
        let join = lit(py, "\n").getattr("join")?;
        let options = kwargs(py, &[("encoding", &lit(py, "utf-8")), ("errors", &lit(py, "replace"))])?;
        let all = path.call_method("read_text", (), Some(&options))?.call_method0("splitlines")?;
        let last = slice(&all, Some(&lines.neg()?), None)?;
        join.call1((last,))
    })();
    match read {
        Ok(text) => Ok(text),
        Err(e) => except_kinds(py, e, || g(py, "OSError"), |_| Ok(lit(py, ""))),
    }
}

#[pyfunction]
#[pyo3(name = "start", signature = (task, provider, model, api_key = None, speed = None, link = None))]
#[doc = "Start one android_agent on `task` on the request's checked phone and\n\
return its run record, the dict `outcome` and `stop` take. Raises\n\
AndroidAgentError when it cannot start."]
fn start_run<'py>(
    py: Python<'py>,
    task: Obj<'py>,
    provider: Obj<'py>,
    model: Obj<'py>,
    api_key: Option<Obj<'py>>,
    speed: Option<Obj<'py>>,
    link: Option<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    let api_key = api_key.unwrap_or_else(|| none(py));
    let speed = speed.unwrap_or_else(|| none(py));
    if g(py, "IS_COMPILED")?.is_truthy()? {
        return Err(throw(py, "AndroidAgentError", || Ok(lit(py, "android_agent is unavailable in the packaged app"))));
    }
    let link = match link {
        Some(link) if !link.is_none() && !ne(&link.call_method0("state")?, "online")? => link,
        _ => return Err(throw(py, "AndroidAgentError", || Ok(lit(py, "android_agent is currently offline")))),
    };
    // One phone, one agent: a second one would tap over the first.
    let any = g(py, "any")?;
    if any.call1((Genexpr::made(&g(py, "_runs")?, Body::Working)?,))?.is_truthy()? {
        return Err(throw(py, "AndroidAgentError", || {
            Ok(lit(
                py,
                "an android_agent is still at work and only one runs at a time: \
                 hold for it with agent_wait, then start the next",
            ))
        }));
    }

    // The phone has one helper, and the child starts its own: a helper this
    // process still holds (the app's Mobile use → Android pick) goes first.
    let released = (|| -> PyResult<()> {
        let [android_session] = imported(py, "AutoCua.android_connector.session", ["android_session"])?;
        android_session.call_method0("deactivate")?;
        Ok(())
    })();
    if let Err(e) = released {
        except_exception(py, e, |_| Ok(()))?;
    }

    let format = g(py, "_TASK_TEMPLATE")?.getattr("format")?;
    let text = format.call((), Some(&kwargs(py, &[("task", &task)])?))?;
    // The child the link started when the phone answered, built and
    // waiting: it gets the task as one line and begins at once.
    let take = link.getattr("take_standby")?;
    let mut child = take.call1((&provider, &model, &api_key, &speed))?;
    if !child.is_none() {
        let sent = (|| -> PyResult<()> {
            let write = child.get_item("proc")?.getattr("stdin")?.getattr("write")?;
            let dumps = g(py, "json")?.getattr("dumps")?;
            let line = dumps.call1((controller::dict(py, &[("task", &text)])?,))?.add("\n")?;
            write.call1((line.call_method1("encode", ("utf-8",))?,))?;
            child.get_item("proc")?.getattr("stdin")?.call_method0("close")?;
            Ok(())
        })();
        if let Err(e) = sent {
            except_kinds(py, e, || pair_of(py, "OSError", "ValueError"), |_| {
                g(py, "_end_standby")?.call1((&child,))?;
                Ok(())
            })?;
            child = none(py);
        }
    }
    if child.is_none() {
        let spawned = (|| -> PyResult<Obj<'py>> {
            let spawn = g(py, "_spawn")?;
            let serial = or(link.getattr("address")?, || link.getattr("serial"))?;
            spawn.call((serial, &provider, &model, &api_key, &speed), Some(&kwargs(py, &[("task", &text)])?))
        })();
        child = match spawned {
            Ok(child) => child,
            Err(e) => {
                return except_exception(py, e, |e| {
                    Err(throw(py, "AndroidAgentError", || {
                        Fmt::new(py).s("android_agent could not be started: ").v(e.value(py))?.done()
                    }))
                });
            }
        };
    }

    let run_dir = child.get_item("run_dir")?;
    let session_id = child.get_item("session_id")?;
    let subprocess = child.get_item("proc")?;
    let result_file = run_dir.div("result.json")?;
    let log_file = run_dir.div("agent.log")?;
    let started = g(py, "time")?.getattr("time")?.call0()?;
    let deadline = g(py, "time")?.getattr("monotonic")?.call0()?.add(g(py, "TIMEOUT_SEC")?)?;
    let run = controller::dict(
        py,
        &[
            ("session_id", &session_id),
            ("subprocess", &subprocess),
            ("result_file", &result_file),
            ("log_file", &log_file),
            ("started", &started),
            ("deadline", &deadline),
            ("stopped", &boolean(py, false)),
        ],
    )?;
    g(py, "_runs")?.call_method1("append", (&run,))?;
    Ok(run.into_any())
}

#[pyfunction]
#[pyo3(name = "_spawn", signature = (serial, provider, model, api_key = None, speed = None, task = None))]
#[doc = "One child in a new run folder: on `task`, or, with no task, a standby\n\
that gets ready and reads its task from stdin. Returns {\"session_id\",\n\
\"run_dir\", \"proc\"}."]
fn spawn<'py>(
    py: Python<'py>,
    serial: Obj<'py>,
    provider: Obj<'py>,
    model: Obj<'py>,
    api_key: Option<Obj<'py>>,
    speed: Option<Obj<'py>>,
    task: Option<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    let api_key = api_key.unwrap_or_else(|| none(py));
    let speed = speed.unwrap_or_else(|| none(py));
    let task = task.unwrap_or_else(|| none(py));
    // sid = f"android_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    let int_ = g(py, "int")?;
    let seconds = int_.call1((g(py, "time")?.getattr("time")?.call0()?,))?;
    let sid = Fmt::new(py).s("android_").v(&seconds)?.s("_");
    let hex = g(py, "uuid")?.getattr("uuid4")?.call0()?.getattr("hex")?;
    let sid = sid.v(&slice(&hex, None, Some(&int(py, 6)?))?)?.done()?;
    let run_dir = g(py, "data_root")?.call0()?.div("android_agent")?.div(&sid)?;
    let yes = boolean(py, true);
    let mkdir = run_dir.getattr("mkdir")?;
    mkdir.call((), Some(&kwargs(py, &[("parents", &yes), ("exist_ok", &yes)])?))?;

    let executable = g(py, "sys")?.getattr("executable")?;
    let str_ = g(py, "str")?;
    let child = str_.call1((g(py, "_CHILD")?,))?;
    let str_ = g(py, "str")?;
    let result = str_.call1((run_dir.div("result.json")?,))?;
    let mut cmd = PyList::new(
        py,
        [
            executable,
            child,
            lit(py, "--provider"),
            provider.clone(),
            lit(py, "--model"),
            model,
            lit(py, "--result"),
            result,
        ],
    )?
    .into_any();
    let more = if !task.is_none() {
        PyList::new(py, [lit(py, "--task"), task.clone()])?
    } else {
        PyList::new(py, [lit(py, "--standby")])?
    };
    cmd = add_in_place(&cmd, more.as_any())?;
    if speed.is_truthy()? {
        let str_ = g(py, "str")?;
        let more = PyList::new(py, [lit(py, "--speed"), str_.call1((&speed,))?])?;
        cmd = add_in_place(&cmd, more.as_any())?;
    }
    let env = g(py, "os")?.getattr("environ")?.call_method0("copy")?;
    env.set_item("PYTHONUNBUFFERED", "1")?;
    // The child's cwd is outside the repo, so make the package importable.
    let str_ = g(py, "str")?;
    let root = str_.call1((g(py, "_REPO_ROOT")?,))?;
    let path = root
        .add(g(py, "os")?.getattr("pathsep")?)?
        .add(env.call_method1("get", ("PYTHONPATH", ""))?)?;
    env.set_item("PYTHONPATH", path)?;
    // The phone this request checked, on the connection the check used and
    // the watch follows: its USB serial, or "ip:port" on Wi-Fi.
    env.set_item("AutoCua_ANDROID_SERIAL", &serial)?;
    // This process: the child ends itself (and the phone helper) if it dies
    // without stopping the run (android_agent_child/service.py _watch_parent).
    let str_ = g(py, "str")?;
    let parent = str_.call1((g(py, "os")?.getattr("getpid")?.call0()?,))?;
    env.set_item("AutoCua_ANDROID_PARENT", parent)?;
    env.call_method1("pop", ("AutoCua_ANDROID_SESSION", py.None()))?;
    if api_key.is_truthy()? && g(py, "_PROVIDER_ENV_KEY")?.contains(&provider)? {
        let name = g(py, "_PROVIDER_ENV_KEY")?.get_item(&provider)?;
        env.set_item(name, &api_key)?;
    }

    // Hidden on Windows when the app has no console of its own, so no console
    // window pops up (as browser_agent/service.py starts its child)
    let mut creationflags = int(py, 0)?;
    if eq(&g(py, "sys")?.getattr("platform")?, "win32")? {
        let window = g(py, "ctypes")?.getattr("windll")?.getattr("kernel32")?.call_method0("GetConsoleWindow")?;
        if !window.is_truthy()? {
            creationflags = g(py, "subprocess")?.getattr("CREATE_NO_WINDOW")?;
        }
    }
    // stdout/stderr go to a file: the Android agent prints a lot, and a
    // PIPE nobody drains would deadlock it. Its own process group on
    // POSIX, so stop() ends the phone helper's adb client with it.
    let open = g(py, "open")?;
    let file = open.call1((run_dir.div("agent.log")?, "ab"))?;
    let proc = with(&file, |log| -> PyResult<Obj<'py>> {
        let popen = g(py, "subprocess")?.getattr("Popen")?;
        let str_ = g(py, "str")?;
        let cwd = str_.call1((&run_dir,))?;
        let stdin = if !task.is_none() {
            g(py, "subprocess")?.getattr("DEVNULL")?
        } else {
            g(py, "subprocess")?.getattr("PIPE")?
        };
        let stderr = g(py, "subprocess")?.getattr("STDOUT")?;
        let new_session = g(py, "sys")?.getattr("platform")?.rich_compare("win32", CompareOp::Ne)?;
        let options = kwargs(
            py,
            &[
                ("cwd", &cwd),
                ("env", &env),
                ("stdin", &stdin),
                ("stdout", log),
                ("stderr", &stderr),
                ("start_new_session", &new_session),
                ("creationflags", &creationflags),
            ],
        )?;
        popen.call((&cmd,), Some(&options))
    })?
    .ok_or_else(|| unbound_local(py, "proc"))?;
    Ok(controller::dict(py, &[("session_id", &sid), ("run_dir", &run_dir), ("proc", &proc)])?.into_any())
}

#[pyfunction]
#[pyo3(name = "_end_standby")]
#[doc = "A standby that got no task: it goes with its phone helper (the whole\n\
tree, as stop() ends a run), and its run folder with it."]
fn end_standby<'py>(py: Python<'py>, child: Obj<'py>) -> PyResult<()> {
    let proc = child.get_item("proc")?;
    let ended = (|| -> PyResult<()> {
        if proc.call_method0("poll")?.is_none() {
            let kill_tree = g(py, "_kill_tree")?;
            kill_tree.call1((&proc, g(py, "signal")?.getattr("SIGTERM")?))?;
            proc.call_method("wait", (), Some(&kwargs(py, &[("timeout", &int(py, 5)?)])?))?;
        }
        Ok(())
    })();
    if let Err(e) = ended {
        except_exception(py, e, |_| {
            if let Err(e) = proc.call_method0("kill") {
                except_exception(py, e, |_| Ok(()))?;
            }
            Ok(())
        })?;
    }
    let rmtree = g(py, "shutil")?.getattr("rmtree")?;
    rmtree.call((child.get_item("run_dir")?,), Some(&kwargs(py, &[("ignore_errors", &boolean(py, true))])?))?;
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_kill_tree")]
#[doc = "Signal the child AND what it started: the phone helper runs as its\n\
`adb shell` client, and a killed child runs no atexit, so the client\n\
would stay and keep the helper (and Android's one UiAutomation) on the\n\
phone. taskkill /T on Windows; the child's own process group on POSIX\n\
(start() gives it one), never this process's own group."]
fn kill_tree<'py>(py: Python<'py>, proc: Obj<'py>, sig: Obj<'py>) -> PyResult<()> {
    if eq(&g(py, "sys")?.getattr("platform")?, "win32")? {
        let run = g(py, "subprocess")?.getattr("run")?;
        let str_ = g(py, "str")?;
        let pid = str_.call1((proc.getattr("pid")?,))?;
        let cmd = PyList::new(py, [lit(py, "taskkill"), lit(py, "/PID"), pid, lit(py, "/T"), lit(py, "/F")])?;
        let options = kwargs(py, &[("capture_output", &boolean(py, true)), ("timeout", &int(py, 10)?)])?;
        run.call((cmd,), Some(&options))?;
        return Ok(());
    }
    let sent = (|| -> PyResult<()> {
        // start() made the child its group's leader: the group is its pid, and
        // it holds what the child left behind even once the child is gone.
        let pid = proc.getattr("pid")?;
        if ne(&pid, g(py, "os")?.getattr("getpgrp")?.call0()?)? {
            let killpg = g(py, "os")?.getattr("killpg")?;
            killpg.call1((proc.getattr("pid")?, &sig))?;
        } else {
            proc.call_method1("send_signal", (&sig,))?;
        }
        Ok(())
    })();
    match sent {
        Ok(()) => Ok(()),
        // nothing of it is left
        Err(e) => except_kinds(py, e, || g(py, "ProcessLookupError"), |_| Ok(())),
    }
}

#[pyfunction]
#[doc = "End a run from outside (Stop, the end of the main agent's request, or\n\
the phone going away). The child's helper goes with it: the whole tree is\n\
ended, the helper's adb client too."]
fn stop<'py>(py: Python<'py>, run: Obj<'py>) -> PyResult<()> {
    run.set_item("stopped", true)?;
    let proc = run.call_method1("get", ("subprocess",))?;
    if proc.is_none() || !proc.call_method0("poll")?.is_none() {
        return Ok(());
    }
    let ended = (|| -> PyResult<()> {
        let kill_tree = g(py, "_kill_tree")?;
        kill_tree.call1((&proc, g(py, "signal")?.getattr("SIGTERM")?))?;
        proc.call_method("wait", (), Some(&kwargs(py, &[("timeout", &int(py, 5)?)])?))?;
        Ok(())
    })();
    let Err(e) = ended else {
        return Ok(());
    };
    let _handling = Handling::new(py, &e);
    let timeout = (|| g(py, "subprocess")?.getattr("TimeoutExpired"))().map_err(|raised| now(py, raised))?;
    if caught(py, &e, &timeout)? {
        let killed = (|| -> PyResult<()> {
            let kill_tree = g(py, "_kill_tree")?;
            let sig = if ne(&g(py, "sys")?.getattr("platform")?, "win32")? {
                g(py, "signal")?.getattr("SIGKILL")?
            } else {
                g(py, "signal")?.getattr("SIGTERM")?
            };
            kill_tree.call1((&proc, sig))?;
            Ok(())
        })();
        if let Err(e) = killed {
            except_exception(py, e, |_| {
                proc.call_method0("kill")?;
                Ok(())
            })?;
        }
        proc.call_method0("wait").map_err(|raised| now(py, raised))?;
        return Ok(());
    }
    let exception = g(py, "Exception").map_err(|raised| now(py, raised))?;
    if caught(py, &e, &exception)? {
        return Ok(());
    }
    Err(e)
}

#[pyfunction]
#[pyo3(name = "_drop")]
fn drop_run<'py>(py: Python<'py>, run: Obj<'py>) -> PyResult<()> {
    if g(py, "_runs")?.contains(&run)? {
        g(py, "_runs")?.call_method1("remove", (&run,))?;
    }
    Ok(())
}

#[pyfunction]
#[pyo3(signature = (run, phone_online = Given::Missing), text_signature = "(run, phone_online=True)")]
#[doc = "(summary, status) once the run is over, None while it is still working.\n\n\
status is `complete` when the Android agent finished its task, otherwise\n\
how it ended: `incomplete`, `error`, `timeout` or `stopped`. Call it until\n\
it answers. phone_online=False (the phone went away) ends a run at once.\n"]
fn outcome<'py>(py: Python<'py>, run: Obj<'py>, phone_online: Given<'py>) -> PyResult<Obj<'py>> {
    let phone_online = match phone_online {
        Given::Value(value) => value,
        Given::Missing => boolean(py, true),
    };
    let pair = |summary: Obj<'py>, status: Obj<'py>| -> PyResult<Obj<'py>> { Ok(PyTuple::new(py, [summary, status])?.into_any()) };
    let proc = run.get_item("subprocess")?;
    if proc.call_method0("poll")?.is_none() {
        if !phone_online.is_truthy()? && !run.get_item("stopped")?.is_truthy()? {
            // Notes first: the run is over for good once it is stopped
            let notes = or(g(py, "_milestone_notes")?.call1((&run,))?, || {
                Ok(lit(py, "nothing was recorded before the phone went away"))
            })?;
            g(py, "stop")?.call1((&run,))?;
            // the phone went away, nobody stopped it
            run.set_item("stopped", false)?;
            g(py, "_drop")?.call1((&run,))?;
            let summary = Fmt::new(py)
                .s("The phone disconnected while android_agent was working.\n\nRecorded before it stopped:\n")
                .v(&notes)?
                .done()?;
            return pair(summary, lit(py, "error"));
        }
        let clock = g(py, "time")?.getattr("monotonic")?.call0()?;
        if clock.rich_compare(run.get_item("deadline")?, CompareOp::Le)?.is_truthy()? {
            return Ok(none(py));
        }
        let notes = or(g(py, "_milestone_notes")?.call1((&run,))?, || {
            Ok(lit(py, "nothing was recorded before the time limit"))
        })?;
        g(py, "stop")?.call1((&run,))?;
        // it ran out of time, nobody stopped it
        run.set_item("stopped", false)?;
        g(py, "_drop")?.call1((&run,))?;
        let summary = Fmt::new(py)
            .s("android_agent ran out of time (")
            .v(&g(py, "TIMEOUT_SEC")?.floor_div(60)?)?
            .s(" min) before it finished.\n\nRecorded before it stopped:\n")
            .v(&notes)?
            .done()?;
        return pair(summary, lit(py, "timeout"));
    }

    let notes = g(py, "_milestone_notes")?.call1((&run,))?;
    g(py, "_drop")?.call1((&run,))?;
    // A child that ended without its own clean-up (a crash) can leave the phone
    // helper's adb client running: what is left of its tree goes too.
    if ne(&g(py, "sys")?.getattr("platform")?, "win32")? {
        let killed = (|| -> PyResult<()> {
            let kill_tree = g(py, "_kill_tree")?;
            kill_tree.call1((&proc, g(py, "signal")?.getattr("SIGTERM")?))?;
            Ok(())
        })();
        if let Err(e) = killed {
            except_exception(py, e, |_| Ok(()))?;
        }
    }
    if run.call_method1("get", ("stopped",))?.is_truthy()? {
        return pair(lit(py, "Stopped by user"), lit(py, "stopped"));
    }
    let loaded = (|| -> PyResult<Obj<'py>> {
        let loads = g(py, "json")?.getattr("loads")?;
        let text = run
            .get_item("result_file")?
            .call_method("read_text", (), Some(&kwargs(py, &[("encoding", &lit(py, "utf-8"))])?))?;
        loads.call1((text,))
    })();
    let result = match loaded {
        Ok(result) => result,
        Err(e) => {
            return except_kinds(py, e, || pair_of(py, "OSError", "ValueError"), |_| {
                let summary = Fmt::new(py)
                    .s("android_agent exited with code ")
                    .v(&proc.getattr("returncode")?)?
                    .s(" without a report.\n\n")
                    .v(&g(py, "_tail")?.call1((run.get_item("log_file")?,))?)?
                    .done()?;
                pair(summary, lit(py, "error"))
            });
        }
    };

    let str_ = g(py, "str")?;
    let status = str_.call1((or(result.call_method1("get", ("status",))?, || Ok(lit(py, "error")))?,))?;
    let str_ = g(py, "str")?;
    let message = str_.call1((or(result.call_method1("get", ("message",))?, || Ok(lit(py, "")))?,))?;
    let mut message = or(message.call_method0("strip")?, || Ok(lit(py, "no message")))?;
    if eq(&status, "success")? {
        let length = g(py, "len")?.call1((&message,))?;
        if length.rich_compare(g(py, "_SHORT_REPORT_CHARS")?, CompareOp::Lt)?.is_truthy()? && notes.is_truthy()? {
            let more = lit(py, "\n\nRecorded during the run:\n").add(&notes)?;
            message = add_in_place(&message, &more)?;
        }
        return pair(message, lit(py, "complete"));
    }
    if notes.is_truthy()? {
        let more = lit(py, "\n\nRecorded before it stopped:\n").add(&notes)?;
        message = add_in_place(&message, &more)?;
    }
    pair(message, status)
}

// ---------------------------------------------------------------------------
// The lines of service.py, in order
// ---------------------------------------------------------------------------

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// service.py's __file__, as it was: the extension's folder, then
/// universal_tool/android_agent/service.py, a str made in Rust.
fn service_file(py: Python<'_>) -> PyResult<String> {
    let file = crate::native_dir(py)?.join("universal_tool").join("android_agent").join("service.py");
    Ok(file.to_string_lossy().into_owned())
}

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let since_3_13 = py.version_info() >= (3, 13);
    // _REPO_ROOT = Path(__file__).resolve().parents[3]
    let path = g(py, "Path")?;
    let root = path.call1((service_file(py)?,))?.call_method0("resolve")?.getattr("parents")?.get_item(3)?;
    store(m, "_REPO_ROOT", root)?;
    // _ANDROID_SCRATCHPAD = _REPO_ROOT / "AutoCua" / "android" / "scratchpad"
    let scratchpad = g(py, "_REPO_ROOT")?.div("AutoCua")?.div("android")?.div("scratchpad")?;
    store(m, "_ANDROID_SCRATCHPAD", scratchpad)?;
    // _CHILD = Path(__file__).resolve().parent.parent / "android_agent_child" / "service.py"
    let path = g(py, "Path")?;
    let here = path.call1((service_file(py)?,))?.call_method0("resolve")?.getattr("parent")?.getattr("parent")?;
    store(m, "_CHILD", here.div("android_agent_child")?.div("service.py")?)?;

    // How often an online phone is looked at again (a pulled cable).
    store(m, "WATCH_SEC", 5)?;
    // The child has its own 100-step cap; this bounds wall-clock.
    let int_ = g(py, "int")?;
    let get = g(py, "os")?.getattr("environ")?.getattr("get")?;
    store(m, "TIMEOUT_SEC", int_.call1((get.call1(("AutoCua_ANDROID_AGENT_TIMEOUT", "900"))?,))?)?;
    // A `done` value shorter than this is almost certainly an activity log, so it
    // is topped up with what the agent recorded along the way.
    store(m, "_SHORT_REPORT_CHARS", 200)?;

    store(m, "_TASK_TEMPLATE", TASK_TEMPLATE)?;

    // class AndroidAgentError(Exception), made as the class statement made
    // it: type() given the namespace Python gave its body.
    let bases = PyTuple::new(py, [g(py, "Exception")?])?;
    let namespace = PyDict::new(py);
    namespace.set_item("__module__", g(py, "__name__")?)?;
    namespace.set_item("__qualname__", "AndroidAgentError")?;
    if since_3_13 {
        namespace.set_item("__firstlineno__", ERROR_LINE)?;
    }
    namespace.set_item("__doc__", ERROR_DOC)?;
    if since_3_13 {
        namespace.set_item("__static_attributes__", PyTuple::empty(py))?;
    }
    let error = py.get_type::<PyType>().call1(("AndroidAgentError", bases, namespace))?;
    store(m, "AndroidAgentError", error)?;

    // Every phone link and every run of this process, so none outlives it.
    store(m, "_links", PyList::empty(py))?;
    store(m, "_runs", PyList::empty(py))?;

    store(m, "stop_all", wrap_pyfunction!(stop_all, m)?)?;

    // atexit.register(stop_all)
    let register = g(py, "atexit")?.getattr("register")?;
    register.call1((g(py, "stop_all")?,))?;

    store(m, "_log", wrap_pyfunction!(log_line, m)?)?;
    store(m, "_adb", wrap_pyfunction!(adb_path, m)?)?;
    store(m, "_usb_phones", wrap_pyfunction!(usb_phones, m)?)?;

    // class AndroidLink:
    store(m, "AndroidLink", py.get_type::<AndroidLink>())?;

    store(m, "_milestone_notes", wrap_pyfunction!(milestone_notes, m)?)?;
    store(m, "_tail", wrap_pyfunction!(tail, m)?)?;
    store(m, "start", wrap_pyfunction!(start_run, m)?)?;
    store(m, "_spawn", wrap_pyfunction!(spawn, m)?)?;
    store(m, "_end_standby", wrap_pyfunction!(end_standby, m)?)?;
    store(m, "_kill_tree", wrap_pyfunction!(kill_tree, m)?)?;
    store(m, "stop", wrap_pyfunction!(stop, m)?)?;
    store(m, "_drop", wrap_pyfunction!(drop_run, m)?)?;
    store(m, "outcome", wrap_pyfunction!(outcome, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with its docstring and
/// _build, which the loader calls once it has bound the import lines' names
/// (see the top of this file), and makes the class look like the one
/// service.py made.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<AndroidLink>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", LINK_DOC)?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", LINK_LINE)?;
        let names = PyTuple::new(py, STATIC_ATTRIBUTES.iter().map(|name| PyString::intern(py, name)))?;
        class.setattr("__static_attributes__", names)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
