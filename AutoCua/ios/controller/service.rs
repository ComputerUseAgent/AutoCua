//! service: the iPhone controller's core service. It holds only the element
//! primitives (click, type, scroll, and ground_click and ground_scroll at a
//! raw point) and the WebDriverAgent session and element map they share.
//! Everything else is a tool in controller/tool/ (open_app, videoplayer,
//! web, ...).
//!
//! The Rust copy of controller/service.py. Python imports it under its old
//! dotted name, AutoCua.ios.controller.service (controller/__init__.py
//! points that name here), and gets the same exception, class, methods,
//! attributes, return values, errors and log records. Python's own rules
//! decide every result, so they are called, not imitated: requests, time,
//! int(), str(), float(), round(), min(), max(), abs(), next(), list(),
//! len(), the operators, the comparisons and the f-strings.
//!
//! The names service.py imported (requests, time, logging), its logger and
//! its one controller_service are bound on this module by
//! controller/__init__.py when the module is first imported, as the .py did
//! then. Loading the extension imports none of them. Every global the .py
//! read (those, TypingRefused, Exception, ValueError and the builtins) is
//! read from this module each time it is used, so a caller or a test that
//! replaces one is obeyed.
//!
//! The instance keeps its attributes (wda_url, elements, session_id) in its
//! __dict__, as a Python instance does, and its methods reach them and each
//! other through the instance, so a subclass that replaces one is obeyed.
//! An error is raised the way Python's raise statement raises it, so the
//! error being handled at that moment (in an except block here, or by
//! whoever called) becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::exceptions::{PyException, PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple, PyType};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::controller::{self, caught, dict, import_from, now, only_self, unpack3};
use crate::ios::NATIVE;
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// service.py's docstring.
const MODULE_DOC: &str = "\nservice.py - Core iPhone interaction service via WebDriverAgent.\n\n\
Holds ONLY the element interaction primitives (click, type, scroll) plus the\n\
shared WDA session/element-mapping plumbing they need. Everything else lives\n\
as a tool in controller/tool/ (open_app, videoplayer, web, ...).\n";

/// ControllerService's docstring.
const CLASS_DOC: &str = "Service for executing iPhone actions via WebDriverAgent";

/// TypingRefused's docstring.
const REFUSED_DOC: &str = "type_text's `check` said no: nothing was typed.";

/// The module __init__ imported wda_url from.
const SESSION: &str = "AutoCua.ios_connector.session";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.controller.service")).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// Python's statements (the shared ones in lib.rs's ios::controller, given
// this module)
// ---------------------------------------------------------------------------

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn name<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `raise <kind>(<message>)`: the global `kind` read first, then the message
/// made, then the error made and raised, in Python's order.
fn throw<'py>(py: Python<'py>, kind: &str, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyErr {
    match module(py) {
        Ok(module) => controller::throw(&module, kind, message),
        Err(e) => e,
    }
}

/// An `except Exception as e:` block: `body` runs while `e` is being
/// handled, with Exception read from this module as Python reads it there.
/// What does not match passes on untouched.
fn except_exception<T>(py: Python<'_>, e: PyErr, body: impl FnOnce(&PyErr) -> PyResult<T>) -> PyResult<T> {
    controller::except_exception(&module(py)?, e, body)
}

/// The `except Exception as e:` of the taps, the swipes and the typing:
/// `logger.error(f"{logged}{str(e)}")` when `logged` is given, then
/// `raise Exception(f"{prefix}{e}")`.
fn failed<T>(py: Python<'_>, e: PyErr, logged: Option<&str>, prefix: &str) -> PyResult<T> {
    except_exception(py, e, |e| {
        if let Some(logged) = logged {
            log(py, "error", || {
                let words = name(py, "str")?.call1((e.value(py),))?;
                Fmt::new(py).s(logged).v(&words)?.done()
            })?;
        }
        Err(throw(py, "Exception", || Fmt::new(py).s(prefix).v(e.value(py).as_any())?.done()))
    })
}

/// An f-string, made as Python makes one: each {value} is formatted the
/// moment it is reached (format(value, spec)), and the pieces are joined at
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
    fn v(self, value: &Obj<'py>) -> PyResult<Self> {
        self.spec(value, "")
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

/// `logger.<level>(message)`: logger and its method are looked up before the
/// message is made, as Python does.
fn log<'py>(py: Python<'py>, level: &str, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<()> {
    controller::log(&module(py)?, level, message)
}

fn lit<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn int(py: Python<'_>, value: i64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

/// `from AutoCua.ios_connector.session import <wanted>`, run when the line
/// ran, as Python runs it (lib.rs's controller::import_from, with its
/// ImportError).
fn from_session<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    let mut found = import_from(&module(py)?, SESSION, &[wanted])?;
    Ok(found.remove(0))
}

// ---------------------------------------------------------------------------
// The lines the methods share
// ---------------------------------------------------------------------------

/// `str(elem_num) in self.elements`: the key made first, then the mapping
/// read.
fn loaded(slf: &Obj<'_>, elem_num: &Obj<'_>) -> PyResult<bool> {
    let key = name(slf.py(), "str")?.call1((elem_num,))?;
    slf.getattr("elements")?.contains(key)
}

/// `self.elements[str(elem_num)]`: the mapping read first, then the key made.
fn element<'py>(slf: &Obj<'py>, elem_num: &Obj<'py>) -> PyResult<Obj<'py>> {
    let elements = slf.getattr("elements")?;
    let key = name(slf.py(), "str")?.call1((elem_num,))?;
    elements.get_item(key)
}

/// What click and scroll log for an element number that is not loaded.
fn log_missing(slf: &Obj<'_>, elem_num: &Obj<'_>) -> PyResult<()> {
    let py = slf.py();
    log(py, "error", || {
        Fmt::new(py)
            .s("Element #")
            .v(elem_num)?
            .s(" not in loaded elements: ")
            .v(&name(py, "list")?.call1((slf.getattr("elements")?.call_method0("keys")?,))?)?
            .done()
    })
}

/// `raise ValueError(f"Element #{elem_num} not found")`.
fn not_found<'py>(py: Python<'py>, elem_num: &Obj<'py>) -> PyErr {
    throw(py, "ValueError", || Fmt::new(py).s("Element #").v(elem_num)?.s(" not found").done())
}

/// `bounds[<at>] + bounds[<size>] // 2`: the centre on one axis.
fn centre<'py>(bounds: &Obj<'py>, at: &str, size: &str) -> PyResult<Obj<'py>> {
    bounds.get_item(at)?.add(bounds.get_item(size)?.floor_div(2)?)
}

/// `session = self.get_session()`, then `raise Exception("Failed to create
/// WDA session")` when it gave nothing.
fn session<'py>(slf: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = slf.py();
    let session = slf.call_method0("get_session")?;
    if !session.is_truthy()? {
        return Err(throw(py, "Exception", || Ok(lit(py, "Failed to create WDA session"))));
    }
    Ok(session)
}

/// `time.sleep(0.1)`, time read now.
fn sleep(py: Python<'_>) -> PyResult<()> {
    name(py, "time")?.getattr("sleep")?.call1((PyFloat::new(py, 0.1),))?;
    Ok(())
}

/// `f"{self.wda_url}/session/{session}<path>"`.
fn session_url<'py>(slf: &Obj<'py>, session: &Obj<'py>, path: &str) -> PyResult<Obj<'py>> {
    Fmt::new(slf.py()).v(&slf.getattr("wda_url")?)?.s("/session/").v(session)?.s(path).done()
}

/// `requests.post(url, json=body, timeout=seconds)`: requests.post looked up
/// first, then the url and the body made, as Python evaluates the call.
fn post<'py>(
    py: Python<'py>,
    url: impl FnOnce() -> PyResult<Obj<'py>>,
    body: impl FnOnce() -> PyResult<Obj<'py>>,
    timeout: i64,
) -> PyResult<Obj<'py>> {
    let post = name(py, "requests")?.getattr("post")?;
    let url = url()?;
    let body = body()?;
    post.call((url,), Some(&dict(py, &[("json", &body), ("timeout", &int(py, timeout)?)])?))
}

/// `response.status_code == 200`.
fn ok(response: &Obj<'_>) -> PyResult<bool> {
    response.getattr("status_code")?.eq(200)
}

/// `logger.error(f"✗ WDA tap failed: {response.status_code} - {response.text}")`.
fn log_tap_failed(py: Python<'_>, response: &Obj<'_>) -> PyResult<()> {
    log(py, "error", || {
        Fmt::new(py)
            .s("✗ WDA tap failed: ")
            .v(&response.getattr("status_code")?)?
            .s(" - ")
            .v(&response.getattr("text")?)?
            .done()
    })
}

/// `raise Exception(f"{what}{response.text}")`, inside the try block.
fn refused<'py>(py: Python<'py>, what: &str, response: &Obj<'py>) -> PyErr {
    throw(py, "Exception", || Fmt::new(py).s(what).v(&response.getattr("text")?)?.done())
}

/// `{"type": "pointerMove", "duration": <duration>, "x": x, "y": y}`.
fn pointer_move<'py>(py: Python<'py>, duration: i64, x: &Obj<'py>, y: &Obj<'py>) -> PyResult<Obj<'py>> {
    let row = [("type", &lit(py, "pointerMove")), ("duration", &int(py, duration)?), ("x", x), ("y", y)];
    Ok(dict(py, &row)?.into_any())
}

/// `{"type": "pointerDown", "button": 0}` or `{"type": "pointerUp", "button": 0}`.
fn pointer_button<'py>(py: Python<'py>, kind: &str) -> PyResult<Obj<'py>> {
    Ok(dict(py, &[("type", &lit(py, kind)), ("button", &int(py, 0)?)])?.into_any())
}

/// `{"type": "pause", "duration": 100}`.
fn pause(py: Python<'_>) -> PyResult<Obj<'_>> {
    Ok(dict(py, &[("type", &lit(py, "pause")), ("duration", &int(py, 100)?)])?.into_any())
}

/// The W3C Actions body of one finger on the touch screen doing `steps`.
fn touch<'py, const N: usize>(py: Python<'py>, steps: [Obj<'py>; N]) -> PyResult<Obj<'py>> {
    let parameters = dict(py, &[("pointerType", &lit(py, "touch"))])?;
    let actions = PyList::new(py, steps)?;
    let finger = dict(
        py,
        &[
            ("type", &lit(py, "pointer")),
            ("id", &lit(py, "finger1")),
            ("parameters", parameters.as_any()),
            ("actions", actions.as_any()),
        ],
    )?;
    Ok(dict(py, &[("actions", PyList::new(py, [finger])?.as_any())])?.into_any())
}

/// The tap of click and ground_click: move to (x, y), down, a 100 ms pause,
/// up.
fn tap<'py>(py: Python<'py>, x: &Obj<'py>, y: &Obj<'py>) -> PyResult<Obj<'py>> {
    touch(
        py,
        [pointer_move(py, 0, x, y)?, pointer_button(py, "pointerDown")?, pause(py)?, pointer_button(py, "pointerUp")?],
    )
}

/// The swipe of scroll and ground_scroll: down at the start, a 100 ms pause,
/// a 500 ms move to the end, up.
fn swipe<'py>(
    py: Python<'py>,
    start_x: &Obj<'py>,
    start_y: &Obj<'py>,
    end_x: &Obj<'py>,
    end_y: &Obj<'py>,
) -> PyResult<Obj<'py>> {
    touch(
        py,
        [
            pointer_move(py, 0, start_x, start_y)?,
            pointer_button(py, "pointerDown")?,
            pause(py)?,
            pointer_move(py, 500, end_x, end_y)?,
            pointer_button(py, "pointerUp")?,
        ],
    )
}

/// `{'value': [value]}`, the body of WDA's keys endpoint.
fn keys<'py>(py: Python<'py>, value: &Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(dict(py, &[("value", PyList::new(py, [value])?.as_any())])?.into_any())
}

// ---------------------------------------------------------------------------
// _ground_point's generator
// ---------------------------------------------------------------------------

/// The generator expression _ground_point gives to next():
/// `(e['bounds'] for e in self.elements.values() if e.get('type') == 'Application')`.
/// It runs as that generator ran: one step per next(). On Python 3.13 its
/// first step takes iter() of what it was given, as that version's
/// generator code does (the others loop over it as it is). A StopIteration
/// from its own body comes out as RuntimeError (PEP 479), and once it has
/// ended or failed it gives nothing more. It shows itself as Python showed
/// that generator.
#[pyclass(frozen, name = "generator", module = "builtins")]
struct Screens {
    /// iter(self.elements.values()), made when the generator was made.
    given: Py<PyIterator>,
    /// iter() of `given` on Python 3.13, taken when the generator first runs.
    items: PyOnceLock<Py<PyIterator>>,
    /// The generator has ended or failed.
    done: AtomicBool,
    /// A step is running.
    running: AtomicBool,
}

#[pymethods]
impl Screens {
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
            let version = py.version_info();
            let items = if version >= (3, 13) && version < (3, 14) {
                let again = || me.given.bind(py).as_any().try_iter().map(Bound::unbind);
                me.items.get_or_try_init(py, again)?.bind(py).clone()
            } else {
                me.given.bind(py).clone()
            };
            for e in items {
                let e = e?;
                if e.call_method1("get", ("type",))?.eq("Application")? {
                    return Ok(Some(e.get_item("bounds")?));
                }
            }
            Ok(None)
        })();
        me.running.store(false, Ordering::SeqCst);
        match step {
            Ok(Some(bounds)) => Ok(Some(bounds)),
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
        format!("<generator object ControllerService._ground_point.<locals>.<genexpr> at {:p}>", slf.as_ptr())
    }
}

// ---------------------------------------------------------------------------
// ControllerService
// ---------------------------------------------------------------------------

/// Service for executing iPhone actions via WebDriverAgent
#[pyclass(frozen, dict, weakref, subclass)]
pub struct ControllerService;

#[pymethods]
impl ControllerService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        ControllerService
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        only_self("ControllerService.__init__()", args, kwargs)?;
        let wda_url = from_session(py, "wda_url")?;
        slf.setattr("wda_url", wda_url.call0()?)?;
        slf.setattr("elements", PyDict::new(py))?;
        slf.setattr("session_id", py.None())?;
        Ok(())
    }

    /// Update element mapping from scanner
    fn update_elements(slf: &Bound<'_, Self>, elements_mapping: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        slf.setattr("elements", elements_mapping)?;
        log(py, "debug", || {
            let count = name(py, "len")?.call1((slf.getattr("elements")?,))?;
            Fmt::new(py).s("Updated elements: ").v(&count)?.s(" elements").done()
        })
    }

    /// Get or create WDA session
    fn get_session<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let attempt = (|| -> PyResult<Obj<'py>> {
            // Create new session
            let post = name(py, "requests")?.getattr("post")?;
            let url = Fmt::new(py).v(&slf.getattr("wda_url")?)?.s("/session").done()?;
            let always_match = dict(py, &[("alwaysMatch", PyDict::new(py).as_any())])?;
            let capabilities = dict(py, &[("capabilities", always_match.as_any())])?;
            let kwargs = dict(py, &[("json", capabilities.as_any()), ("timeout", &int(py, 10)?)])?;
            let response = post.call((url,), Some(&kwargs))?;
            if ok(&response)? {
                let data = response.call_method0("json")?;
                if data.contains("sessionId")? {
                    slf.setattr("session_id", data.get_item("sessionId")?)?;
                } else if data.contains("value")? && data.get_item("value")?.contains("sessionId")? {
                    slf.setattr("session_id", data.get_item("value")?.get_item("sessionId")?)?;
                }
                return slf.getattr("session_id");
            }
            Ok(none(py))
        })();
        attempt.or_else(|e| {
            except_exception(py, e, |e| {
                log(py, "error", || Fmt::new(py).s("Session error: ").v(e.value(py).as_any())?.done())?;
                Ok(none(py))
            })
        })
    }

    /// Click on element by number
    fn click<'py>(slf: &Bound<'py, Self>, element_number: &Obj<'py>) -> PyResult<bool> {
        let py = slf.py();
        let me = slf.as_any();
        let elem_num = name(py, "int")?.call1((element_number,))?;

        if !loaded(me, &elem_num)? {
            log_missing(me, &elem_num)?;
            return Err(not_found(py, &elem_num));
        }

        let elem_data = element(me, &elem_num)?;
        let bounds = elem_data.get_item("bounds")?;

        // Calculate center point
        let mut center_x = centre(&bounds, "x", "width")?;
        let center_y = centre(&bounds, "y", "height")?;

        // Some apps report a Switch as its whole row (Google Calendar's "All
        // day" is 390pt wide): a tap on its centre hits the label and does
        // nothing, only the knob at the right end toggles it. A knob is ~63pt
        // wide, so a Switch over 100pt is a row: tap 50pt in from its right
        // edge. Knob-sized switches (Apple's) keep the centre tap.
        if elem_data.call_method1("get", ("type",))?.eq("Switch")? && bounds.get_item("width")?.gt(100)? {
            center_x = bounds.get_item("x")?.add(bounds.get_item("width")?)?.sub(50)?;
        }

        log(py, "debug", || Fmt::new(py).s("Clicking at (").v(&center_x)?.s(", ").v(&center_y)?.s(")").done())?;

        // Get or create session
        let session = session(me)?;

        // Use W3C Actions API for tapping
        let tapped = (|| -> PyResult<bool> {
            let response = post(py, || session_url(me, &session, "/actions"), || tap(py, &center_x, &center_y), 5)?;

            if ok(&response)? {
                log(py, "info", || {
                    Fmt::new(py)
                        .s("✓ Click successful on element ")
                        .v(&elem_num)?
                        .s(" at (")
                        .v(&center_x)?
                        .s(", ")
                        .v(&center_y)?
                        .s(")")
                        .done()
                })?;
                return Ok(true);
            }
            log_tap_failed(py, &response)?;
            Err(refused(py, "WDA tap failed: ", &response))
        })();
        tapped.or_else(|e| failed(py, e, Some("✗ Click exception: "), "Click failed: "))
    }

    #[doc = "A 0-1000 spot on the screenshot as WDA's points, and the frame it\n\
was mapped onto: (point x, point y, frame bounds). The scanner draws\n\
the screenshot from the app frame (the Application element), so a\n\
fraction of the image is the same fraction of that frame."]
    fn _ground_point<'py>(slf: &Bound<'py, Self>, x: &Obj<'py>, y: &Obj<'py>, tool: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // x, y = float(x), float(y): both are made before either name changes.
        let numbers = (|| -> PyResult<(Obj<'py>, Obj<'py>)> {
            let first = name(py, "float")?.call1((x,))?;
            let second = name(py, "float")?.call1((y,))?;
            Ok((first, second))
        })();
        let (x, y) = match numbers {
            Ok(numbers) => numbers,
            Err(e) => {
                // except (TypeError, ValueError):
                let _handling = Handling::new(py, &e);
                let kinds = PyTuple::new(py, [name(py, "TypeError")?, name(py, "ValueError")?])?;
                if !caught(py, &e, kinds.as_any())? {
                    return Err(e);
                }
                return Err(throw(py, "ValueError", || {
                    Fmt::new(py).v(tool)?.s(" needs x and y numbers, got (").v(x)?.s(", ").v(y)?.s(")").done()
                }));
            }
        };
        let (zero, top) = (int(py, 0)?, int(py, 1000)?);
        if !(zero.le(&x)? && x.le(&top)? && zero.le(&y)? && y.le(&top)?) {
            return Err(throw(py, "ValueError", || {
                Fmt::new(py)
                    .v(tool)?
                    .s(" x and y must be 0-1000, got (")
                    .spec(&x, "g")?
                    .s(", ")
                    .spec(&y, "g")?
                    .s(")")
                    .done()
            }));
        }

        let next = name(py, "next")?;
        let given = slf.getattr("elements")?.call_method0("values")?.try_iter()?;
        let screens = Screens {
            given: given.unbind(),
            items: PyOnceLock::new(),
            done: AtomicBool::new(false),
            running: AtomicBool::new(false),
        };
        let screen = next.call1((Bound::new(py, screens)?, py.None()))?;
        if !screen.is_truthy()? {
            return Err(throw(py, "Exception", || Ok(lit(py, "No scanned screen to map the point onto"))));
        }
        // int(round(<point> / 1000 * screen[<size>])): int and round read first.
        let scale = |point: &Obj<'py>, size: &str| -> PyResult<Obj<'py>> {
            let int_ = name(py, "int")?;
            let round = name(py, "round")?;
            let scaled = point.div(1000)?.mul(screen.get_item(size)?)?;
            int_.call1((round.call1((scaled,))?,))
        };
        let point_x = scale(&x, "width")?;
        let point_y = scale(&y, "height")?;
        Ok(PyTuple::new(py, [point_x, point_y, screen.clone()])?.into_any())
    }

    #[doc = "Tap a raw point: x and y on a 0-1000 scale of the screenshot.\n\n\
For targets with no [id] (one chip inside a wide element, a switch\n\
knob), in the points WDA taps in (_ground_point).\n"]
    fn ground_click<'py>(slf: &Bound<'py, Self>, x: &Obj<'py>, y: &Obj<'py>) -> PyResult<bool> {
        let py = slf.py();
        let me = slf.as_any();
        let (tap_x, tap_y, _screen) = unpack3(&me.call_method1("_ground_point", (x, y, "ground_click"))?)?;

        log(py, "debug", || {
            Fmt::new(py)
                .s("Ground clicking (")
                .v(x)?
                .s(", ")
                .v(y)?
                .s(") at (")
                .v(&tap_x)?
                .s(", ")
                .v(&tap_y)?
                .s(")")
                .done()
        })?;

        // Get or create session
        let session = session(me)?;

        // Same W3C tap as click()
        let tapped = (|| -> PyResult<bool> {
            let response = post(py, || session_url(me, &session, "/actions"), || tap(py, &tap_x, &tap_y), 5)?;

            if ok(&response)? {
                log(py, "info", || {
                    Fmt::new(py).s("✓ Ground click successful at (").v(&tap_x)?.s(", ").v(&tap_y)?.s(")").done()
                })?;
                return Ok(true);
            }
            log_tap_failed(py, &response)?;
            Err(refused(py, "WDA tap failed: ", &response))
        })();
        tapped.or_else(|e| failed(py, e, Some("✗ Ground click exception: "), "Ground click failed: "))
    }

    #[doc = "Swipe from a raw point: x and y on the 0-1000 scale of the\n\
screenshot (ground_click's), `direction` the way the finger moves\n\
(up, down, left or right, as scroll: \"up\" reveals what is below).\n\n\
For a list or a pane with no [id]. The swipe is 200 points, or up to\n\
the screen's edge; a point too close to that edge is refused.\n"]
    fn ground_scroll<'py>(slf: &Bound<'py, Self>, x: &Obj<'py>, y: &Obj<'py>, direction: &Obj<'py>) -> PyResult<bool> {
        let py = slf.py();
        let me = slf.as_any();
        let (start_x, start_y, screen) = unpack3(&me.call_method1("_ground_point", (x, y, "ground_scroll"))?)?;
        let moves = PyDict::new(py);
        for (way, step) in [("up", (0, -1)), ("down", (0, 1)), ("left", (-1, 0)), ("right", (1, 0))] {
            moves.set_item(way, step)?;
        }
        // direction = str(direction or "").lower()
        let str_ = name(py, "str")?;
        let given = if direction.is_truthy()? { direction.clone() } else { lit(py, "") };
        let direction = str_.call1((given,))?.call_method0("lower")?;
        if !moves.as_any().contains(&direction)? {
            return Err(throw(py, "ValueError", || {
                Fmt::new(py).s("Invalid direction: ").v(&direction)?.s(". Use: up, down, left, or right").done()
            }));
        }
        let (dx, dy) = unpack2(&moves.as_any().get_item(&direction)?)?;
        let (margin, distance) = (int(py, 20)?, int(py, 200)?);
        // min(max(<start> + <step> * distance, margin), screen[<size>] - margin):
        // min and max read first.
        let end = |start: &Obj<'py>, step: &Obj<'py>, size: &str| -> PyResult<Obj<'py>> {
            let min = name(py, "min")?;
            let max = name(py, "max")?;
            let reached = max.call1((start.add(step.mul(&distance)?)?, &margin))?;
            min.call1((reached, screen.get_item(size)?.sub(&margin)?))
        };
        let end_x = end(&start_x, &dx, "width")?;
        let end_y = end(&start_y, &dy, "height")?;
        let across = name(py, "abs")?.call1((end_x.sub(&start_x)?,))?;
        let along = name(py, "abs")?.call1((end_y.sub(&start_y)?,))?;
        if across.add(along)?.lt(50)? {
            return Err(throw(py, "ValueError", || {
                Fmt::new(py)
                    .s("(")
                    .v(x)?
                    .s(", ")
                    .v(y)?
                    .s(") is too close to the screen's edge to swipe ")
                    .v(&direction)?
                    .s(" from")
                    .done()
            }));
        }

        log(py, "debug", || {
            Fmt::new(py)
                .s("Ground scrolling ")
                .v(&direction)?
                .s(" from (")
                .v(&start_x)?
                .s(", ")
                .v(&start_y)?
                .s(") to (")
                .v(&end_x)?
                .s(", ")
                .v(&end_y)?
                .s(")")
                .done()
        })?;

        let session = session(me)?;

        // The same W3C swipe as scroll()
        let swiped = (|| -> PyResult<bool> {
            let body = || swipe(py, &start_x, &start_y, &end_x, &end_y);
            let response = post(py, || session_url(me, &session, "/actions"), body, 5)?;
            if ok(&response)? {
                return Ok(true);
            }
            Err(refused(py, "WDA swipe failed: ", &response))
        })();
        swiped.or_else(|e| failed(py, e, None, "Ground scroll failed: "))
    }

    #[pyo3(signature = (element_number, text, check = None))]
    #[doc = "Type text into element by number\n\n\
If element already has text, it will be deleted first before typing new text.\n\
Sends delete/backspace key for each existing character including spaces.\n\n\
`check(session)`, when given (the vault's, vault/service.py), runs after\n\
the focus tap and before any key: a non-empty answer stops here with\n\
TypingRefused, nothing deleted or typed.\n"]
    fn type_text<'py>(
        slf: &Bound<'py, Self>,
        element_number: &Obj<'py>,
        text: &Obj<'py>,
        check: Option<&Obj<'py>>,
    ) -> PyResult<bool> {
        let py = slf.py();
        let me = slf.as_any();
        let elem_num = name(py, "int")?.call1((element_number,))?;

        if !loaded(me, &elem_num)? {
            return Err(not_found(py, &elem_num));
        }

        // Get current value of the element
        let current_value = element(me, &elem_num)?.call_method1("get", ("value", ""))?;

        // First click on the element to focus it
        me.call_method1("click", (element_number,))?;
        sleep(py)?; // Small delay for focus

        // Get session
        let session = session(me)?;

        if let Some(check) = check {
            if check.is_truthy()? {
                let problem = check.call1((&session,))?;
                if problem.is_truthy()? {
                    return Err(throw(py, "TypingRefused", || Ok(problem.clone())));
                }
            }
        }

        // If element has existing text (not blank), delete it first
        if current_value.is_truthy()? && current_value.call_method0("strip")?.is_truthy()? {
            // Send delete/backspace key for each character including spaces
            let delete_keys = lit(py, "\u{8}").mul(name(py, "len")?.call1((&current_value,))?)?;

            let cleared = (|| -> PyResult<()> {
                post(py, || session_url(me, &session, "/wda/keys"), || keys(py, &delete_keys), 10)?;
                log(py, "debug", || {
                    let count = name(py, "len")?.call1((&current_value,))?;
                    Fmt::new(py).s("Cleared ").v(&count)?.s(" characters from element").done()
                })
            })();
            if let Err(e) = cleared {
                except_exception(py, e, |e| {
                    log(py, "warning", || Fmt::new(py).s("Clear text error: ").v(e.value(py).as_any())?.done())
                })?;
            }

            sleep(py)?; // Small delay after deletion
        }

        // Now type the new text
        let typed = (|| -> PyResult<bool> {
            let response = post(py, || session_url(me, &session, "/wda/keys"), || keys(py, text), 10)?;

            if ok(&response)? {
                return Ok(true);
            }
            Err(refused(py, "WDA type failed: ", &response))
        })();
        typed.or_else(|e| failed(py, e, None, "Type failed: "))
    }

    #[doc = "Scroll/swipe within element bounds in specified direction\n\n\
Args:\n    element_number: The element to scroll within\n    direction: 'up', 'down', 'left', or 'right'\n"]
    fn scroll<'py>(slf: &Bound<'py, Self>, element_number: &Obj<'py>, direction: &Obj<'py>) -> PyResult<bool> {
        let py = slf.py();
        let me = slf.as_any();
        let elem_num = name(py, "int")?.call1((element_number,))?;

        if !loaded(me, &elem_num)? {
            log_missing(me, &elem_num)?;
            return Err(not_found(py, &elem_num));
        }

        let elem_data = element(me, &elem_num)?;
        let bounds = elem_data.get_item("bounds")?;
        let direction = direction.call_method0("lower")?;

        // Dynamic approach: Start from edge in SAME direction, swipe in that direction
        // This avoids center overlays and ensures the swipe starts from the correct edge

        // Margins and swipe distances
        let margin = int(py, 20)?; // Safe margin from edge
        let vertical_swipe_distance = int(py, 200)?; // More power for up/down swipes
        let horizontal_swipe_distance = int(py, 200)?; // More power for left/right swipes

        // Calculate center for positioning
        let center_x = centre(&bounds, "x", "width")?;
        let center_y = centre(&bounds, "y", "height")?;

        // Calculate start and end points based on direction
        // Start from edge in the SAME direction and swipe in that direction
        let (start_x, start_y, end_x, end_y) = if direction.eq("up")? {
            // Start from top area, swipe upward (toward top edge)
            (
                center_x.clone(),
                bounds.get_item("y")?.add(&margin)?.add(&vertical_swipe_distance)?,
                center_x.clone(),
                bounds.get_item("y")?.add(&margin)?,
            )
        } else if direction.eq("down")? {
            // Start from bottom area, swipe downward (toward bottom edge)
            (
                center_x.clone(),
                bounds.get_item("y")?.add(bounds.get_item("height")?)?.sub(&margin)?.sub(&vertical_swipe_distance)?,
                center_x.clone(),
                bounds.get_item("y")?.add(bounds.get_item("height")?)?.sub(&margin)?,
            )
        } else if direction.eq("left")? {
            // Start from left area, swipe leftward (toward left edge) - MORE POWER
            (
                bounds.get_item("x")?.add(&margin)?.add(&horizontal_swipe_distance)?,
                center_y.clone(),
                bounds.get_item("x")?.add(&margin)?,
                center_y.clone(),
            )
        } else if direction.eq("right")? {
            // Start from right area, swipe rightward (toward right edge) - MORE POWER
            (
                bounds.get_item("x")?.add(bounds.get_item("width")?)?.sub(&margin)?.sub(&horizontal_swipe_distance)?,
                center_y.clone(),
                bounds.get_item("x")?.add(bounds.get_item("width")?)?.sub(&margin)?,
                center_y.clone(),
            )
        } else {
            return Err(throw(py, "ValueError", || {
                Fmt::new(py).s("Invalid direction: ").v(&direction)?.s(". Use: up, down, left, or right").done()
            }));
        };

        log(py, "debug", || {
            Fmt::new(py)
                .s("Scrolling ")
                .v(&direction)?
                .s(" from (")
                .v(&start_x)?
                .s(", ")
                .v(&start_y)?
                .s(") to (")
                .v(&end_x)?
                .s(", ")
                .v(&end_y)?
                .s(")")
                .done()
        })?;

        // Get or create session
        let session = session(me)?;

        // Use W3C Actions API for swipe gesture
        let swiped = (|| -> PyResult<bool> {
            let body = || swipe(py, &start_x, &start_y, &end_x, &end_y);
            let response = post(py, || session_url(me, &session, "/actions"), body, 5)?;

            if ok(&response)? {
                return Ok(true);
            }
            Err(refused(py, "WDA scroll failed: ", &response))
        })();
        swiped.or_else(|e| failed(py, e, None, "Scroll failed: "))
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with its docstring, its
/// exception and its class. controller/__init__.py binds the rest when the
/// module is first imported (see the top of this file), the one global
/// instance, controller_service, included.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    let since_3_13 = py.version_info() >= (3, 13);
    m.setattr("__doc__", MODULE_DOC)?;

    // class TypingRefused(Exception), made as the class statement made it:
    // type() given the namespace Python gave its body.
    let namespace = PyDict::new(py);
    namespace.set_item("__module__", &python_name)?;
    namespace.set_item("__qualname__", "TypingRefused")?;
    if since_3_13 {
        namespace.set_item("__firstlineno__", 16)?;
    }
    namespace.set_item("__doc__", REFUSED_DOC)?;
    if since_3_13 {
        namespace.set_item("__static_attributes__", PyTuple::empty(py))?;
    }
    let bases = PyTuple::new(py, [py.get_type::<PyException>()])?;
    let refused = py.get_type::<PyType>().call1(("TypingRefused", bases, namespace))?;
    m.add("TypingRefused", refused)?;

    m.add_class::<ControllerService>()?;
    let class = py.get_type::<ControllerService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if since_3_13 {
        class.setattr("__firstlineno__", 20)?;
        class.setattr("__static_attributes__", ("elements", "session_id", "wda_url"))?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
