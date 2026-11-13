//! service: the vault. The logins the user saved for the phone agents (the
//! "ios" section of AutoCua_data/vault/credentials.json) are typed into a
//! field for the model (fill) or tapped on a PIN keypad (find_pin_keys,
//! enter_pin). The model only ever sees their key names (block), and every
//! value the vault typed reads as [vault] from then on (hide, hide_json,
//! which the Android agent shares).
//!
//! The Rust copy of vault/service.py. Python imports it under its old dotted
//! name, AutoCua.vault.service (vault/__init__.py points that name here), and
//! finds the same names in the same order, with the same values, return
//! values, errors and log records. Python's own rules decide every result,
//! so they are called, not imitated: requests, json, re, ElementTree, quote,
//! the builtins, the str, dict and list methods, the operators and the
//! f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module only _build. Then the loader binds what
//! service.py's import lines bound (json, os, re, time, logging, ET, Path,
//! quote, quote_plus, requests) and calls _build(), which runs the rest of
//! service.py's lines in their order: the logger, the constants, `_filled`,
//! each function where its def was, the class, and last the one
//! vault_service, which loads the credentials as it always did. Loading the
//! extension imports none of them.
//!
//! Every global a line reads (requests, time, _filled, CLIP_KEEP, len,
//! isinstance, ...) is read from this module at that moment, as Python reads
//! a global, so a caller or a test that replaces one is obeyed; `_filled` is
//! the one list the functions change and read. The instance keeps its
//! attributes in its __dict__, and its methods reach them and each other
//! through the instance, so a subclass that replaces one is obeyed. An error
//! is raised the way Python's raise statement raises it, so the error being
//! handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};

use pyo3::exceptions::{PyImportError, PyRuntimeError, PyStopIteration, PySystemError, PyTypeError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyCFunction, PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{self, caught, dict, import_from, now, only_self, os_path, raise, unbound_local};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// VaultService's docstring.
const CLASS_DOC: &str = "Service for managing and retrieving credentials";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 225;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 4] = ["_next_bundle", "_scan_bundle", "credentials", "element_tree_text"];

/// One digit, optionally with its phone letters ("2 ABC", "2, A B C", "0 +").
const PIN_KEY_PATTERN: &str = r"\s*([0-9])(?:[\s,]+[A-Z+][A-Z+ ]{0,8})?\s*";

/// The native passcode field's value, e.g. "4 of 6 values entered".
const PIN_ENTRY_PATTERN: &str = r"(\d+) of (\d+) values entered";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.service", super::NATIVE)).map(Bound::unbind))
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

/// `isinstance(value, <kind>)`: isinstance and the class read from this
/// module now, then the truth of the answer.
fn isinstance(value: &Obj<'_>, kind: &str) -> PyResult<bool> {
    controller::isinstance(&module(value.py())?, value, kind)
}

/// `logger.<level>(message)`: logger and its method are looked up before the
/// message is made, as Python does.
fn log<'py>(py: Python<'py>, level: &str, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<()> {
    controller::log(&module(py)?, level, message)
}

/// `raise <kind>("<message>")`: the global `kind` read first, then the error
/// made and raised, in Python's order.
fn throw(py: Python<'_>, kind: &str, message: &str) -> PyErr {
    match module(py) {
        Ok(m) => controller::throw(&m, kind, || Ok(lit(py, message))),
        Err(e) => e,
    }
}

/// An `except Exception:` block: `body` runs while `e` is being handled,
/// with Exception read from this module as Python reads it there. What does
/// not match passes on untouched.
fn except_exception<T>(py: Python<'_>, e: PyErr, body: impl FnOnce(&PyErr) -> PyResult<T>) -> PyResult<T> {
    controller::except_exception(&module(py)?, e, body)
}

/// The body of an except block whose Handling the caller holds: an error it
/// raises is raised now, so the error being handled is its __context__.
fn handled<T>(py: Python<'_>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    body().map_err(|raised| now(py, raised))
}

/// `logger.error(f"<what>{str(e)}")` inside an `except Exception as e:`.
fn log_error(py: Python<'_>, what: &str, e: &PyErr) -> PyResult<()> {
    log(py, "error", || {
        let words = g(py, "str")?.call1((e.value(py),))?;
        Fmt::new(py).s(what).v(&words)?.done()
    })
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

/// `a or b`: a when it is true, else b, made only then.
fn or<'py>(a: Obj<'py>, b: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    if a.is_truthy()? {
        Ok(a)
    } else {
        b()
    }
}

/// `{}`, a new empty dict.
fn empty_dict(py: Python<'_>) -> Obj<'_> {
    PyDict::new(py).into_any()
}

/// The keyword part of a call, `key=value`.
fn kw<'py>(py: Python<'py>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(key, value)?;
    Ok(kwargs)
}

/// `self.<method>(keep_on_error=True)`, the reload of block, fill and get_pin.
fn reload(slf: &Obj<'_>) -> PyResult<()> {
    let py = slf.py();
    slf.call_method("_load_credentials", (), Some(&kw(py, "keep_on_error", true)?))?;
    Ok(())
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

    fn done(self) -> PyResult<Obj<'py>> {
        let pieces: Vec<&Obj<'py>> = self.pieces.iter().collect();
        Ok(join_str(self.py, &pieces)?.into_any())
    }
}

/// The one argument of a lambda with one parameter (`parameter`), given by
/// position or by name.
fn one_argument(
    args: &Bound<'_, PyTuple>,
    kwargs: Option<&Bound<'_, PyDict>>,
    lambda: &str,
    parameter: &str,
) -> PyResult<Py<PyAny>> {
    let mut by_name = None;
    if let Some(kwargs) = kwargs {
        for (keyword, value) in kwargs.iter() {
            if keyword.eq(parameter)? && by_name.is_none() {
                by_name = Some(value.unbind());
            } else {
                return Err(PyTypeError::new_err(format!(
                    "{lambda}() got an unexpected keyword argument '{}'",
                    keyword.str()?
                )));
            }
        }
    }
    match (args.len(), by_name) {
        (1, None) => Ok(args.get_item(0)?.unbind()),
        (0, Some(value)) => Ok(value),
        (0, None) => Err(PyTypeError::new_err(format!(
            "{lambda}() missing 1 required positional argument: '{parameter}'"
        ))),
        (1, Some(_)) => Err(PyTypeError::new_err(format!("{lambda}() got multiple values for argument '{parameter}'"))),
        (given, _) => Err(PyTypeError::new_err(format!(
            "{lambda}() takes 1 positional argument but {given} were given"
        ))),
    }
}

// ---------------------------------------------------------------------------
// The relative imports (from ..ios.controller.<path> import <name>)
// ---------------------------------------------------------------------------

/// `from ..ios.controller.<path> import <wanted>`, run when the line runs, as
/// Python runs it: one __import__ of "ios.controller.<path>" at level 2 with
/// this module's globals (its package, AutoCua.vault, is where the dots
/// start), then the name from what came back, as CPython's IMPORT_FROM takes
/// it (lib.rs's controller::import_from does the same at level 0).
fn from_ios<'py>(py: Python<'py>, path: &str, wanted: &str) -> PyResult<Obj<'py>> {
    let m = module(py)?;
    let import = py.import("builtins")?.getattr("__import__")?;
    let fromlist = PyTuple::new(py, [wanted])?;
    let found = import.call1((format!("ios.controller.{path}"), m.dict(), py.None(), fromlist, 2))?;
    import_one(&found, wanted)
}

/// One name of a from-import, as CPython's IMPORT_FROM takes it: the
/// attribute, or else the submodule of that name already in sys.modules, or
/// else Python's ImportError.
fn import_one<'py>(found: &Obj<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    let py = found.py();
    if let Some(value) = found.getattr_opt(wanted)? {
        return Ok(value);
    }
    let package = found.getattr_opt("__name__")?.filter(|package| package.is_instance_of::<PyString>());
    if let Some(package) = &package {
        let full = join_str(py, &[package, &lit(py, "."), &lit(py, wanted)])?;
        // SAFETY: a live str; a new reference, or NULL with or without an error set.
        let module = unsafe { Bound::from_owned_ptr_or_opt(py, ffi::PyImport_GetModule(full.as_ptr())) };
        if let Some(module) = module {
            return Ok(module);
        }
        if let Some(e) = PyErr::take(py) {
            return Err(e);
        }
    }
    Err(import_error(found, wanted, package)?)
}

/// The ImportError of a from-import that found nothing, worded as the
/// running Python words it (a copy of lib.rs's controller::import_error,
/// which is private there). Up to 3.12 the location is the module's
/// __file__, and a spec that cannot be read counts as not initializing.
/// From 3.13 the module's __spec__ is read first: its origin when it has a
/// location, else the module's __file__, and an error in reading the spec is
/// raised.
fn import_error<'py>(found: &Obj<'py>, wanted: &str, package: Option<Obj<'py>>) -> PyResult<PyErr> {
    let py = found.py();
    // SAFETY: a live object.
    let is_module = unsafe { ffi::PyModule_Check(found.as_ptr()) } != 0;
    // The module's __file__ when it is a str, else None.
    let file = || -> PyResult<Option<Obj<'py>>> {
        if !is_module {
            return Ok(None);
        }
        // SAFETY: a live module; a new reference, or NULL with an error set.
        match unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyModule_GetFilenameObject(found.as_ptr())) } {
            Ok(file) => Ok(Some(file)),
            Err(e) if e.is_instance_of::<PySystemError>(py) => Ok(None),
            Err(e) => Err(e),
        }
    };
    let (location, initializing) = if py.version_info() >= (3, 13) {
        match found.getattr_opt("__spec__")? {
            None => (None, false),
            Some(spec) => {
                let mut origin = None;
                if let Some(has_location) = spec.getattr_opt("has_location")? {
                    if has_location.is_truthy()? {
                        origin = spec.getattr_opt("origin")?.filter(|origin| origin.is_instance_of::<PyString>());
                    }
                }
                if origin.is_none() {
                    origin = file()?;
                }
                let initializing = match spec.getattr_opt("_initializing")? {
                    Some(flag) => flag.is_truthy()?,
                    None => false,
                };
                (origin, initializing)
            }
        }
    } else {
        let path = file().unwrap_or(None);
        let initializing = path.is_some()
            && found
                .getattr("__spec__")
                .and_then(|spec| spec.getattr("_initializing"))
                .and_then(|flag| flag.is_truthy())
                .unwrap_or(false);
        (path, initializing)
    };
    let shown_package = match &package {
        Some(package) => package.repr()?,
        None => PyString::new(py, "<unknown module name>").repr()?,
    };
    let mut parts = vec![
        lit(py, "cannot import name "),
        PyString::new(py, wanted).repr()?.into_any(),
        lit(py, if initializing { " from partially initialized module " } else { " from " }),
        shown_package.into_any(),
    ];
    if initializing {
        parts.push(lit(py, " (most likely due to a circular import)"));
    }
    match &location {
        Some(location) => {
            parts.push(lit(py, " ("));
            parts.push(location.str()?.into_any());
            parts.push(lit(py, ")"));
        }
        None if !initializing => parts.push(lit(py, " (unknown location)")),
        None => {}
    }
    let message = join_str(py, &parts.iter().collect::<Vec<_>>())?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("name", package)?;
    kwargs.set_item("path", location)?;
    if py.version_info() >= (3, 12) {
        kwargs.set_item("name_from", wanted)?;
    }
    let error = py.get_type::<PyImportError>().call((message,), Some(&kwargs))?;
    Ok(raise(py, &error))
}

// ---------------------------------------------------------------------------
// The two generator expressions
// ---------------------------------------------------------------------------

// The generator expressions of fill and find_pin_keys, run as Python runs
// one (as controller/service.rs runs _ground_point's). Made holding iter()
// of what it loops over, on Python 3.13 its first step takes iter() of that
// again, as that version's generator code does; then it runs its body one
// value per next(). A StopIteration from its own body comes out as
// RuntimeError (PEP 479), and once it has ended or failed it gives nothing
// more. It shows itself as Python showed that generator. A plain comment: a
// doc comment would give the class a docstring, and a generator has none.
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
    /// fill's `(k for k in creds if k.strip().lower() == key.lower())`.
    KeyNamed { key: Py<PyAny> },
    /// find_pin_keys' `(_inside(_centre(b), k) for k in kept)`.
    Inside { b: Py<PyAny> },
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

    /// The body for one item `k`: Some(value) to give it, None to go on.
    fn step<'py>(&self, k: Obj<'py>) -> PyResult<Option<Obj<'py>>> {
        let py = k.py();
        match &self.body {
            Body::KeyNamed { key } => {
                let wanted = k.call_method0("strip")?.call_method0("lower")?;
                if wanted.eq(key.bind(py).call_method0("lower")?)? {
                    Ok(Some(k))
                } else {
                    Ok(None)
                }
            }
            Body::Inside { b } => {
                let inside = g(py, "_inside")?;
                let centre = g(py, "_centre")?;
                let point = centre.call1((b.bind(py),))?;
                Ok(Some(inside.call1((point, k))?))
            }
        }
    }

    /// The function that made it, as its __qualname__ names the generator.
    fn qualname(&self) -> &'static str {
        match self.body {
            Body::KeyNamed { .. } => "VaultService.fill.<locals>.<genexpr>",
            Body::Inside { .. } => "VaultService.find_pin_keys.<locals>.<genexpr>",
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
            for k in items {
                if let Some(value) = me.step(k?)? {
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
// PIN keypads
// ---------------------------------------------------------------------------
// vault {"id": 0, "value": "pin"}: some PIN screens have nothing to type into.
// The iPhone lock screen and app PIN pads (Sky Go's "Sky Device PIN") are ten
// separate keys, so the vault taps the PIN itself and the model never sees it.
//
// Facts established live on an iPhone (iOS 26.6.2):
//   * Lock-screen keys are XCUIElementTypeKey "1".."0" (the scanner lists them
//     as buttons). The "Passcode field" value counts presses: "0 of 6 values
//     entered" -> "4 of 6". That count confirms every press registered.
//   * Sky Go's keys are statictext "1".."0" covering each key cell. Its entered
//     digits are not in the tree, so the only check is whether the pad closed.
//   * A key can appear twice (a button wrapping a same-named label).
//
// Anything that maps to a digit is as secret as the PIN: a key's element id,
// its coordinates, which press failed. None of it is logged or returned.

// def _pin_key(name): (no docstring)
#[pyfunction]
#[pyo3(name = "_pin_key")]
fn pin_key<'py>(name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = name.py();
    // m = PIN_KEY_LABEL.fullmatch(name or "")
    let fullmatch = g(py, "PIN_KEY_LABEL")?.getattr("fullmatch")?;
    let m = fullmatch.call1((or(name, || Ok(lit(py, "")))?,))?;
    // return m.group(1) if m else None
    if m.is_truthy()? {
        m.call_method1("group", (1,))
    } else {
        Ok(none(py))
    }
}

// def _centre(b): (no docstring)
#[pyfunction]
#[pyo3(name = "_centre")]
fn centre<'py>(b: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = b.py();
    // b[<at>] + b[<size>] / 2.0, in that order.
    let half = |at: &str, size: &str| -> PyResult<Obj<'py>> {
        let start = b.get_item(at)?;
        let length = b.get_item(size)?;
        start.add(length.div(PyFloat::new(py, 2.0))?)
    };
    let x = half("x", "width")?;
    let y = half("y", "height")?;
    Ok(PyTuple::new(py, [x, y])?.into_any())
}

// def _inside(point, b): (no docstring)
#[pyfunction]
#[pyo3(name = "_inside")]
fn inside<'py>(point: Obj<'py>, b: Obj<'py>) -> PyResult<Obj<'py>> {
    let (x, y) = unpack2(&point)?;
    // `b[<at>] <= v <= b[<at>] + b[<size>]`, as a chained comparison gives
    // it: the first answer when it is false, else the second.
    let between = |v: &Obj<'py>, at: &str, size: &str| -> PyResult<Obj<'py>> {
        let low = b.get_item(at)?.rich_compare(v, CompareOp::Le)?;
        if !low.is_truthy()? {
            return Ok(low);
        }
        let high = b.get_item(at)?.add(b.get_item(size)?)?;
        v.rich_compare(high, CompareOp::Le)
    };
    // ... and ...: the first chain when it is false, else the second.
    let across = between(&x, "x", "width")?;
    if !across.is_truthy()? {
        return Ok(across);
    }
    between(&y, "y", "height")
}

#[pyfunction]
#[pyo3(name = "_read_pin_screen")]
#[doc = "(entry count or None, keypad on screen) from a fresh WDA source dump."]
fn read_pin_screen<'py>(controller_service: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = controller_service.py();
    // response = requests.get(f"{controller_service.wda_url}/source",
    //                         params={"excluded_attributes": "visible,accessible"},
    //                         timeout=WDA_SOURCE_TIMEOUT)
    let get = g(py, "requests")?.getattr("get")?;
    let url = Fmt::new(py).v(&controller_service.getattr("wda_url")?)?.s("/source").done()?;
    let params = dict(py, &[("excluded_attributes", &lit(py, "visible,accessible"))])?;
    let timeout = g(py, "WDA_SOURCE_TIMEOUT")?;
    let response = get.call((url,), Some(&dict(py, &[("params", params.as_any()), ("timeout", &timeout)])?))?;
    // root = ET.fromstring(response.json()["value"])
    let fromstring = g(py, "ET")?.getattr("fromstring")?;
    let root = fromstring.call1((response.call_method0("json")?.get_item("value")?,))?;
    // count, digits = None, set()
    let mut count = none(py);
    let digits = g(py, "set")?.call0()?;
    for node in root.call_method0("iter")?.try_iter()? {
        let node = node?;
        // m = PIN_ENTRY_COUNT.search(node.get("value") or "")
        let search = g(py, "PIN_ENTRY_COUNT")?.getattr("search")?;
        let m = search.call1((or(node.call_method1("get", ("value",))?, || Ok(lit(py, "")))?,))?;
        if m.is_truthy()? && count.is_none() {
            // count = (int(m.group(1)), int(m.group(2)))
            let entered = g(py, "int")?.call1((m.call_method1("group", (1,))?,))?;
            let total = g(py, "int")?.call1((m.call_method1("group", (2,))?,))?;
            count = PyTuple::new(py, [entered, total])?.into_any();
        }
        // digit = _pin_key(node.get("label") or node.get("name"))
        let read_key = g(py, "_pin_key")?;
        let label = or(node.call_method1("get", ("label",))?, || node.call_method1("get", ("name",)))?;
        let digit = read_key.call1((label,))?;
        if !digit.is_none() {
            digits.call_method1("add", (digit,))?;
        }
    }
    // return count, len(digits) == 10
    let full = g(py, "len")?.call1((&digits,))?.rich_compare(10, CompareOp::Eq)?;
    Ok(PyTuple::new(py, [count, full])?.into_any())
}

#[pyfunction]
#[pyo3(name = "_tap_pin_keys")]
#[doc = "Tap each key with its own request, one after another on one session.\n\n\
Not one chained request: on Sky Go a four-tap chain landed two taps, a late\n\
third and no fourth. One tap per request is the shape controller_service.click\n\
uses, and measured ~0.42s per key, steady, on the device."]
fn tap_pin_keys<'py>(controller_service: Obj<'py>, session: Obj<'py>, points: Obj<'py>) -> PyResult<()> {
    let py = controller_service.py();
    for point in points.try_iter()? {
        let (x, y) = unpack2(&point?)?;
        let post = g(py, "requests")?.getattr("post")?;
        let url = Fmt::new(py)
            .v(&controller_service.getattr("wda_url")?)?
            .s("/session/")
            .v(&session)?
            .s("/actions")
            .done()?;
        let parameters = dict(py, &[("pointerType", &lit(py, "touch"))])?;
        let tap_x = g(py, "int")?.call1((&x,))?;
        let tap_y = g(py, "int")?.call1((&y,))?;
        let zero = int(py, 0)?;
        let steps = [
            dict(py, &[("type", &lit(py, "pointerMove")), ("duration", &zero), ("x", &tap_x), ("y", &tap_y)])?,
            dict(py, &[("type", &lit(py, "pointerDown")), ("button", &zero)])?,
            dict(py, &[("type", &lit(py, "pause")), ("duration", &g(py, "PIN_PRESS_MS")?)])?,
            dict(py, &[("type", &lit(py, "pointerUp")), ("button", &zero)])?,
        ];
        let finger = dict(
            py,
            &[
                ("type", &lit(py, "pointer")),
                ("id", &lit(py, "finger1")),
                ("parameters", parameters.as_any()),
                ("actions", PyList::new(py, steps)?.as_any()),
            ],
        )?;
        let body = dict(py, &[("actions", PyList::new(py, [finger])?.as_any())])?;
        let response = post.call((url,), Some(&dict(py, &[("json", body.as_any()), ("timeout", &int(py, 30)?)])?))?;
        if response.getattr("status_code")?.ne(200)? {
            // Never echo the response: WDA can quote the coordinates back.
            return Err(throw(py, "RuntimeError", "tap request rejected"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Field fills
// ---------------------------------------------------------------------------
// vault {"id": 5, "value": "password"}: the value saved under the key the agent
// names, in the "ios" entry of the app in front (its exact name, case and
// spaces ignored), typed into [5] like `input`. The same rules as the browser vault
// (browser/controller/vault): the agent sees key names only (`block`), a password
// goes only into a password field, and what was typed reads as [vault] from
// then on (`hide`). Right before the keys go out, after the focus tap, the app
// scanned must still be in front: an earlier call in the step can switch apps.

// def _norm(name): (no docstring)
#[pyfunction]
#[pyo3(name = "_norm")]
fn norm<'py>(name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = name.py();
    // str(name or "").lower().replace(" ", "")
    let str_ = g(py, "str")?;
    str_.call1((or(name, || Ok(lit(py, "")))?,))?.call_method0("lower")?.call_method1("replace", (" ", ""))
}

#[pyfunction]
#[pyo3(name = "_text_of")]
#[doc = "A saved value as the text to type: a non-empty string, or a whole\n\
number (a pin saved as 1234). Anything else is not typed."]
fn text_of<'py>(value: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = value.py();
    if isinstance(&value, "bool")? {
        return Ok(none(py));
    }
    if isinstance(&value, "int")? {
        return g(py, "str")?.call1((value,));
    }
    if isinstance(&value, "str")? && value.is_truthy()? {
        return Ok(value);
    }
    Ok(none(py))
}

#[pyfunction]
#[pyo3(name = "_usable_keys")]
#[doc = "The keys of an entry that hold something to type, as the file writes them."]
fn usable_keys<'py>(creds: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = creds.py();
    // [k for k, v in creds.items() if _text_of(v) is not None]
    let kept = PyList::empty(py);
    for pair in comprehension(&creds.call_method0("items")?)? {
        let (k, v) = unpack2(&pair?)?;
        if !g(py, "_text_of")?.call1((v,))?.is_none() {
            kept.append(k)?;
        }
    }
    Ok(kept.into_any())
}

#[pyfunction]
#[doc = "A fresh chat has typed nothing yet. A continuation keeps the list: the\n\
phone may still show what was typed."]
fn new_chat(py: Python<'_>) -> PyResult<()> {
    g(py, "_filled")?.call_method0("clear")?;
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_remember")]
#[doc = "Hide `secret` from here on, in the forms a screen shows it back in: as\n\
typed, with its whitespace collapsed, and url-encoded. Added before a\n\
single key goes out, since a fill that fails halfway may have typed some."]
fn remember(secret: Obj<'_>) -> PyResult<()> {
    let py = secret.py();
    // for form in (secret, " ".join(secret.split()), quote(secret, safe=""), quote_plus(secret)):
    let collapsed = lit(py, " ").call_method1("join", (secret.call_method0("split")?,))?;
    let quote = g(py, "quote")?;
    let quoted = quote.call((&secret,), Some(&kw(py, "safe", "")?))?;
    let quote_plus = g(py, "quote_plus")?;
    let plus = quote_plus.call1((&secret,))?;
    for form in [secret, collapsed, quoted, plus] {
        // if form and form not in _filled: _filled.append(form)
        if form.is_truthy()? && !g(py, "_filled")?.contains(&form)? {
            g(py, "_filled")?.call_method1("append", (&form,))?;
        }
    }
    // _filled.sort(key=len, reverse=True)
    let sort = g(py, "_filled")?.getattr("sort")?;
    let kwargs = kw(py, "key", g(py, "len")?)?;
    kwargs.set_item("reverse", true)?;
    sort.call((), Some(&kwargs))?;
    Ok(())
}

#[pyfunction]
#[doc = "`text` with every value the vault typed this chat shown as [vault]."]
fn hide(text: Obj<'_>) -> PyResult<Obj<'_>> {
    let py = text.py();
    let mut text = text;
    for secret in g(py, "_filled")?.try_iter()? {
        let secret = secret?;
        let hide_one = g(py, "_hide_one")?;
        text = hide_one.call1((&text, secret))?;
    }
    Ok(text)
}

#[pyfunction]
#[doc = "Every string in a tool result, through `hide`."]
fn hide_json(value: Obj<'_>) -> PyResult<Obj<'_>> {
    let py = value.py();
    if !g(py, "_filled")?.is_truthy()? {
        return Ok(value);
    }
    if isinstance(&value, "str")? {
        return g(py, "hide")?.call1((value,));
    }
    if isinstance(&value, "list")? {
        // [hide_json(v) for v in value]
        let out = PyList::empty(py);
        for v in comprehension(&value)? {
            let v = v?;
            out.append(g(py, "hide_json")?.call1((v,))?)?;
        }
        return Ok(out.into_any());
    }
    if isinstance(&value, "dict")? {
        // {k: hide_json(v) for k, v in value.items()}
        let out = PyDict::new(py);
        for pair in comprehension(&value.call_method0("items")?)? {
            let (k, v) = unpack2(&pair?)?;
            let hidden = g(py, "hide_json")?.call1((v,))?;
            out.set_item(k, hidden)?;
        }
        return Ok(out.into_any());
    }
    Ok(value)
}

#[pyfunction]
#[pyo3(name = "_hide_one")]
#[doc = "`secret`, and any run of at least CLIP_KEEP of its first characters, as\n\
[vault]. A match that runs on into a longer word or number is that word,\n\
not the secret (\"admin\" in \"administration\"), and is left alone."]
fn hide_one<'py>(text: Obj<'py>, secret: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = text.py();
    // head = secret[:CLIP_KEEP]
    let head = slice(&secret, None, Some(&g(py, "CLIP_KEEP")?))?;
    if !head.is_truthy()? {
        return Ok(text);
    }
    let one = int(py, 1)?;
    let zero = int(py, 0)?;
    let out = PyList::empty(py);
    let mut i = int(py, 0)?;
    loop {
        // at = text.find(head, i)
        let at = text.call_method1("find", (&head, &i))?;
        if at.lt(&zero)? {
            break;
        }
        // end = at + len(head)
        let mut end = at.add(g(py, "len")?.call1((&head,))?)?;
        // while end - at < len(secret) and end < len(text) and text[end] == secret[end - at]:
        //     end += 1
        loop {
            let run = end.sub(&at)?;
            if !run.lt(g(py, "len")?.call1((&secret,))?)? {
                break;
            }
            if !end.lt(g(py, "len")?.call1((&text,))?)? {
                break;
            }
            let shown = text.get_item(&end)?;
            if !shown.eq(secret.get_item(end.sub(&at)?)?)? {
                break;
            }
            end = add_in_place(&end, &one)?;
        }
        // before = text[at - 1] if at else ""
        let before = if at.is_truthy()? { text.get_item(at.sub(&one)?)? } else { lit(py, "") };
        // after = text[end] if end < len(text) else ""
        let after = if end.lt(g(py, "len")?.call1((&text,))?)? { text.get_item(&end)? } else { lit(py, "") };
        // joins = (before.isalnum() and secret[0].isalnum()) or (after.isalnum() and text[end - 1].isalnum()),
        // each part's truth asked as often as Python asks it.
        let first = before.call_method0("isalnum")?;
        let left = if first.is_truthy()? { secret.get_item(0)?.call_method0("isalnum")? } else { first };
        let joins = if left.is_truthy()? {
            left
        } else {
            let third = after.call_method0("isalnum")?;
            if third.is_truthy()? {
                text.get_item(end.sub(&one)?)?.call_method0("isalnum")?
            } else {
                third
            }
        };
        out.append(slice(&text, Some(&i), Some(&at))?)?;
        if joins.is_truthy()? {
            // Part of a longer word: keep its first character, look on.
            out.append(text.get_item(&at)?)?;
            i = at.add(&one)?;
        } else {
            out.append(g(py, "HIDDEN")?)?;
            i = end;
        }
    }
    out.append(slice(&text, Some(&i), None)?)?;
    lit(py, "").call_method1("join", (out,))
}

// ---------------------------------------------------------------------------
// VaultService
// ---------------------------------------------------------------------------

/// `{**result, "status": <status>, "message": message}`: fill's reply.
fn reply<'py>(result: &Bound<'py, PyDict>, status: &str, message: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = result.py();
    let out = PyDict::new(py);
    out.update(result.as_mapping())?;
    out.set_item("status", status)?;
    out.set_item("message", message)?;
    Ok(out.into_any())
}

/// enter_pin's `result(status, message, pressed, closed=False)`.
fn outcome<'py>(py: Python<'py>, status: &str, message: Obj<'py>, pressed: bool, closed: bool) -> PyResult<Obj<'py>> {
    let out = PyDict::new(py);
    out.set_item("status", status)?;
    out.set_item("message", message)?;
    out.set_item("pressed", pressed)?;
    out.set_item("closed", closed)?;
    Ok(out.into_any())
}

/// fill's `lambda session: self._fill_check(controller_service, session,
/// password)`: a callable that calls _fill_check through the instance.
fn fill_check_lambda<'py>(slf: &Obj<'py>, controller_service: &Obj<'py>, password: &Obj<'py>) -> PyResult<Obj<'py>> {
    let me = slf.clone().unbind();
    let controller = controller_service.clone().unbind();
    let password = password.clone().unbind();
    let lambda = PyCFunction::new_closure(slf.py(), Some(c"<lambda>"), None, move |args, kwargs| -> PyResult<Py<PyAny>> {
        let py = args.py();
        let session = one_argument(args, kwargs, "VaultService.fill.<locals>.<lambda>", "session")?.into_bound(py);
        let check = me.bind(py).getattr("_fill_check")?;
        Ok(check.call1((controller.bind(py), session, password.bind(py)))?.unbind())
    })?;
    Ok(lambda.into_any())
}

/// find_pin_keys' `lambda b: -(b["width"] * b["height"])`: the biggest first.
fn area_lambda(py: Python<'_>) -> PyResult<Obj<'_>> {
    let lambda = PyCFunction::new_closure(py, Some(c"<lambda>"), None, |args, kwargs| -> PyResult<Py<PyAny>> {
        let b = one_argument(args, kwargs, "VaultService.find_pin_keys.<locals>.<lambda>", "b")?.into_bound(args.py());
        Ok(b.get_item("width")?.mul(b.get_item("height")?)?.neg()?.unbind())
    })?;
    Ok(lambda.into_any())
}

/// The credentials file _load_credentials reads: `from AutoCua import
/// vault_file` and `str(vault_file())`, run when they ran.
fn vault_path(py: Python<'_>) -> PyResult<Obj<'_>> {
    let vault_file = import_from(&module(py)?, "AutoCua", &["vault_file"])?.remove(0);
    let str_ = g(py, "str")?;
    str_.call1((vault_file.call0()?,))
}

/// The credentials file when vault_file cannot be had: credentials.json
/// beside service.py, `os.path.join(os.path.dirname(os.path.abspath(
/// __file__)), 'credentials.json')`, with service.py's __file__ made in Rust
/// (the extension's folder, then vault/service.py) and the os.path calls
/// Python made, looked up as it looked them up.
fn beside_service(py: Python<'_>) -> PyResult<Obj<'_>> {
    let m = module(py)?;
    // current_dir = os.path.dirname(os.path.abspath(__file__))
    let dirname = os_path(&m, "dirname")?;
    let abspath = os_path(&m, "abspath")?;
    let file = crate::native_dir(py)?.join("vault").join("service.py");
    let current_dir = dirname.call1((abspath.call1((file.to_string_lossy().into_owned(),))?,))?;
    // credentials_path = os.path.join(current_dir, 'credentials.json')
    os_path(&m, "join")?.call1((current_dir, "credentials.json"))
}

/// Service for managing and retrieving credentials
#[pyclass(frozen, dict, weakref, subclass)]
pub struct VaultService;

#[pymethods]
impl VaultService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        VaultService
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        only_self("VaultService.__init__()", args, kwargs)?;
        slf.setattr("element_tree_text", "")?;
        slf.setattr("credentials", PyDict::new(py))?;
        // The bundle id of the app in front as the last scan read the screen
        // (begin_scan), for `_fill_check`; None after a scan that failed.
        slf.setattr("_next_bundle", py.None())?;
        slf.setattr("_scan_bundle", py.None())?;
        slf.call_method0("_load_credentials")?;
        Ok(())
    }

    #[pyo3(signature = (keep_on_error = None), text_signature = "($self, keep_on_error=False)")]
    #[doc = "Load credentials from JSON file.\n\n\
keep_on_error: a reload while the app runs (get_pin) must not wipe what\n\
normal fills use when it catches a half-saved or mistyped file.\n"]
    fn _load_credentials(slf: &Bound<'_, Self>, keep_on_error: Option<Obj<'_>>) -> PyResult<()> {
        let py = slf.py();
        let loaded = (|| -> PyResult<()> {
            // AutoCua_data/vault/credentials.json: outside the install folder,
            // so uninstalling AutoCua can't delete the user's credentials.
            let credentials_path = match vault_path(py) {
                Ok(path) => path,
                Err(e) => except_exception(py, e, |_| beside_service(py))?,
            };

            let exists = os_path(&module(py)?, "exists")?.call1((&credentials_path,))?;
            if exists.is_truthy()? {
                // with open(credentials_path, 'r', encoding='utf-8') as f:
                //     loaded = json.load(f)
                let open = g(py, "open")?;
                let file = open.call((&credentials_path, "r"), Some(&kw(py, "encoding", "utf-8")?))?;
                let loaded = with(&file, |f| g(py, "json")?.getattr("load")?.call1((f,)))?
                    .ok_or_else(|| unbound_local(py, "loaded"))?;
                if !isinstance(&loaded, "dict")? {
                    return Err(throw(py, "ValueError", "credentials.json must hold an object"));
                }
                // One file for every device, a section each ("ios", "browser", ...):
                // iOS reads only its own, so a site key can never match an app.
                let section = loaded.call_method1("get", ("ios", PyDict::new(py)))?;
                if !isinstance(&section, "dict")? {
                    return Err(throw(py, "ValueError", "credentials.json's \"ios\" must hold an object"));
                }
                slf.setattr("credentials", section)?;
                log(py, "info", || {
                    let count = g(py, "len")?.call1((slf.getattr("credentials")?,))?;
                    Fmt::new(py).s("Loaded credentials for ").v(&count)?.s(" applications").done()
                })?;
            } else {
                log(py, "debug", || Ok(lit(py, "credentials.json not found")))?;
                slf.setattr("credentials", PyDict::new(py))?;
            }
            Ok(())
        })();
        match loaded {
            Ok(()) => Ok(()),
            Err(e) => except_exception(py, e, |e| {
                log_error(py, "Error loading credentials: ", e)?;
                let keep = match &keep_on_error {
                    Some(keep) => keep.is_truthy()?,
                    None => false,
                };
                if !keep {
                    slf.setattr("credentials", PyDict::new(py))?;
                }
                Ok(())
            }),
        }
    }

    #[doc = "A scan starts: note the app in front by bundle id, read just before\n\
the screen is, so the app and the tree belong together. Until the scan\n\
succeeds (update_element_tree) the vault types into nothing."]
    fn begin_scan(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        slf.setattr("_scan_bundle", py.None())?;
        slf.setattr("_next_bundle", py.None())?;
        if slf.getattr("credentials")?.is_truthy()? {
            let app_launcher_service = from_ios(py, "tool.open_app", "app_launcher_service")?;
            let active = or(app_launcher_service.call_method0("active_app")?, || Ok(empty_dict(py)))?;
            slf.setattr("_next_bundle", active.call_method1("get", ("bundleId",))?)?;
        }
        Ok(())
    }

    #[doc = "Update the stored element tree, from the scan begin_scan started"]
    fn update_element_tree(slf: &Bound<'_, Self>, element_tree_text: Obj<'_>) -> PyResult<()> {
        let py = slf.py();
        slf.setattr("element_tree_text", element_tree_text)?;
        slf.setattr("_scan_bundle", slf.getattr("_next_bundle")?)?;
        log(py, "debug", || Ok(lit(py, "Vault element tree updated")))
    }

    #[doc = "(app name, device) of the screen last scanned: the root [1]\n\
application's label (\"\" on the home and lock screen) and the\n\
current_device header (\"iPhone\" when it is missing)."]
    fn _scanned_app<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let mut app_name = lit(py, "");
        let mut device = lit(py, "iPhone");
        for line in slf.getattr("element_tree_text")?.call_method0("splitlines")?.try_iter()? {
            let line = line?;
            if line.call_method1("startswith", ("current_device:",))?.is_truthy()? {
                // device = (line.split(":", 1)[1].split() or [device])[0]
                let words = line.call_method1("split", (":", 1))?.get_item(1)?.call_method0("split")?;
                device = or(words, || Ok(PyList::new(py, [&device])?.into_any()))?.get_item(0)?;
            } else if line.call_method0("lstrip")?.call_method1("startswith", ("[1]<",))?.is_truthy()?
                && line.contains("type=\"application\"")?
            {
                // app_name = ((self._parse_element_line(line) or {}).get("label") or "").strip()
                let attrs = or(slf.call_method1("_parse_element_line", (&line,))?, || Ok(empty_dict(py)))?;
                app_name = or(attrs.call_method1("get", ("label",))?, || Ok(lit(py, "")))?.call_method0("strip")?;
                break;
            }
        }
        Ok(PyTuple::new(py, [app_name, device])?.into_any())
    }

    #[doc = "(entry name, its keys) saved for `app_name`: its exact name, case\n\
and spaces ignored. Never a device entry: those hold the lock-screen\n\
PIN, which only get_pin reads."]
    fn _app_entry<'py>(slf: &Bound<'py, Self>, app_name: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let wanted = g(py, "_norm")?.call1((app_name,))?;
        if !wanted.is_truthy()? || g(py, "DEVICE_ENTRIES")?.contains(&wanted)? {
            return Ok(none(py));
        }
        for pair in slf.getattr("credentials")?.call_method0("items")?.try_iter()? {
            let (key, creds) = unpack2(&pair?)?;
            if g(py, "_norm")?.call1((&key,))?.eq(&wanted)? && isinstance(&creds, "dict")? {
                return Ok(PyTuple::new(py, [key, creds])?.into_any());
            }
        }
        Ok(none(py))
    }

    #[doc = "<vault>: the key names saved for the app just scanned, or the\n\
device's PIN on the home and lock screen. Empty when nothing is saved."]
    fn block<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        reload(slf.as_any())?;
        let (app_name, _) = unpack2(&slf.call_method0("_scanned_app")?)?;
        let found = slf.call_method1("_app_entry", (&app_name,))?;
        let (name, keys) = if found.is_truthy()? {
            // name, keys = found[0], _usable_keys(found[1])
            let name = found.get_item(0)?;
            let usable_keys = g(py, "_usable_keys")?;
            (name, usable_keys.call1((found.get_item(1)?,))?)
        } else if !app_name.is_truthy()? {
            let (name, pin) = unpack2(&slf.call_method0("get_pin")?)?;
            let keys = if pin.is_truthy()? { PyList::new(py, ["pin"])? } else { PyList::empty(py) };
            (name, keys.into_any())
        } else {
            return Ok(lit(py, ""));
        };
        if !keys.is_truthy()? {
            return Ok(lit(py, ""));
        }
        // f'<vault app="{name}">\n{", ".join(keys)}\n</vault>'
        let opened = Fmt::new(py).s("<vault app=\"").v(&name)?.s("\">\n");
        let listed = lit(py, ", ").call_method1("join", (&keys,))?;
        opened.v(&listed)?.s("\n</vault>").done()
    }

    #[doc = "vault {\"id\", \"value\": <key>}: type the value saved under `key` for\n\
the app in front into [id], like `input`. The result names the key,\n\
never the value. A PIN keypad is enter_pin's, not this."]
    fn fill<'py>(
        slf: &Bound<'py, Self>,
        controller_service: Obj<'py>,
        element_id: Obj<'py>,
        key: Obj<'py>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // key = str(key or "").strip()
        let str_ = g(py, "str")?;
        let key = str_.call1((or(key, || Ok(lit(py, "")))?,))?.call_method0("strip")?;
        let result = dict(py, &[("action", &lit(py, "vault")), ("id", &element_id), ("value", &key)])?;
        // def refuse(message): return {**result, "status": "error", "message": message}
        let refuse = |message: Obj<'py>| reply(&result, "error", message);

        // try: idx = int(element_id)
        let idx = match g(py, "int").and_then(|int| int.call1((&element_id,))) {
            Ok(idx) => idx,
            Err(e) => {
                // except (TypeError, ValueError):
                let _handling = Handling::new(py, &e);
                let kinds = handled(py, || Ok(PyTuple::new(py, [g(py, "TypeError")?, g(py, "ValueError")?])?.into_any()))?;
                if !caught(py, &e, &kinds)? {
                    return Err(e);
                }
                return handled(py, || {
                    refuse(Fmt::new(py).s("'").v(&element_id)?.s("' is not a valid element id").done()?)
                });
            }
        };
        // elem = controller_service.elements.get(str(idx))
        let get = controller_service.getattr("elements")?.getattr("get")?;
        let elem = get.call1((g(py, "str")?.call1((&idx,))?,))?;
        if !elem.is_truthy()? {
            return refuse(
                Fmt::new(py)
                    .s("no element [")
                    .v(&idx)?
                    .s("] in the current element_tree. Ids are re-assigned on every scan - re-read the latest tree and use an id from it.")
                    .done()?,
            );
        }
        if !key.is_truthy()? {
            return refuse(lit(py, "vault's `value` is the name of a key in <vault>, like \"username\" or \"password\""));
        }
        reload(slf.as_any())?;
        let (app_name, _) = unpack2(&slf.call_method0("_scanned_app")?)?;
        let found = slf.call_method1("_app_entry", (&app_name,))?;
        if !found.is_truthy()? {
            let opened = Fmt::new(py).s("nothing is saved in the vault for ");
            let shown = or(app_name, || Ok(lit(py, "the home screen")))?;
            return refuse(opened.v(&shown)?.s(": use what the task gives, or ask the user").done()?);
        }
        let (entry, creds) = unpack2(&found)?;
        // name = next((k for k in creds if k.strip().lower() == key.lower()), None)
        let next = g(py, "next")?;
        let matching = Genexpr::made(&creds, Body::KeyNamed { key: key.clone().unbind() })?;
        let name = next.call1((matching, py.None()))?;
        if name.is_none() {
            let opened = Fmt::new(py).s("no \"").v(&key)?.s("\" is saved for ").v(&entry)?.s(": <vault> has ");
            let usable = g(py, "_usable_keys")?.call1((&creds,))?;
            let listed = lit(py, ", ").call_method1("join", (usable,))?;
            return refuse(opened.v(&listed)?.done()?);
        }
        // secret = _text_of(creds[name])
        let read_text = g(py, "_text_of")?;
        let secret = read_text.call1((creds.get_item(&name)?,))?;
        if secret.is_none() {
            return refuse(
                Fmt::new(py)
                    .s("the \"")
                    .v(&name)?
                    .s("\" saved for ")
                    .v(&entry)?
                    .s(" is empty or not text: ask the user to fix it under \"ios\" in AutoCua_data/vault/credentials.json")
                    .done()?,
            );
        }
        let password = name.call_method0("lower")?.contains("password")?;
        let kind = or(elem.call_method1("get", ("type",))?, || Ok(lit(py, "element")))?;
        if !g(py, "TEXT_FIELDS")?.contains(&kind)? {
            let opened = Fmt::new(py).s("[").v(&idx)?.s("] is a ");
            return refuse(opened.v(&kind.call_method0("lower")?)?.s(", not a text field").done()?);
        }
        if password && kind.ne("SecureTextField")? {
            let opened = Fmt::new(py).s("[").v(&idx)?.s("] is a ");
            let message = opened
                .v(&kind.call_method0("lower")?)?
                .s(", and a saved password goes only into a password field")
                .done()?;
            return refuse(message);
        }

        g(py, "_remember")?.call1((&secret,))?;
        let typing_refused = from_ios(py, "service", "TypingRefused")?;
        // try: controller_service.type_text(idx, secret, check=lambda session: ...)
        let typed = (|| -> PyResult<()> {
            let type_text = controller_service.getattr("type_text")?;
            let check = fill_check_lambda(slf.as_any(), &controller_service, PyBool::new(py, password).as_any())?;
            type_text.call((&idx, &secret), Some(&kw(py, "check", check)?))?;
            Ok(())
        })();
        if let Err(e) = typed {
            let _handling = Handling::new(py, &e);
            // except TypingRefused as e:
            if caught(py, &e, &typing_refused)? {
                return handled(py, || refuse(Fmt::new(py).s("nothing typed: ").v(e.value(py).as_any())?.done()?));
            }
            // except Exception:
            let exception = handled(py, || g(py, "Exception"))?;
            if !caught(py, &e, &exception)? {
                return Err(e);
            }
            // Never the error's own text: a WDA reply can quote what was typed.
            return handled(py, || {
                refuse(lit(
                    py,
                    "the typing failed on the phone, and part of the value may be in the field: check the next screenshot",
                ))
            });
        }
        log(py, "info", || Fmt::new(py).s("🔐 Vault filled element ").v(&idx)?.s(" (").v(&name)?.s(")").done())?;
        // label = str(elem.get("name") or "").strip()
        let str_ = g(py, "str")?;
        let label = str_.call1((or(elem.call_method1("get", ("name",))?, || Ok(lit(py, "")))?,))?.call_method0("strip")?;
        let target = if label.is_truthy()? && label.ne("no_label")? {
            Fmt::new(py).s("[").v(&idx)?.s("] (").v(&label)?.s(")").done()?
        } else {
            Fmt::new(py).s("[").v(&idx)?.s("]").done()?
        };
        // return {**result, "status": "success", "message": f"filled {target} with the saved {name} for {entry}"}
        let out = PyDict::new(py);
        out.update(result.as_mapping())?;
        let message = Fmt::new(py)
            .s("filled ")
            .v(&target)?
            .s(" with the saved ")
            .v(&name)?
            .s(" for ")
            .v(&entry)?
            .done()?;
        out.set_item("status", "success")?;
        out.set_item("message", message)?;
        Ok(out.into_any())
    }

    #[doc = "type_text's check, after the focus tap and before a single key: the\n\
field that has focus takes this value, and the app the scan read is\n\
still in front with no system screen up (asked last, right before the\n\
keys). \"\" when all hold. A phone that cannot answer gets nothing."]
    fn _fill_check<'py>(
        slf: &Bound<'py, Self>,
        controller_service: Obj<'py>,
        session: Obj<'py>,
        password: Obj<'py>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // A tap that starts an animation (Settings' search bar slides up) can
        // leave WDA without an answer for a moment: ask again until FOCUS_WAIT.
        let base = Fmt::new(py).v(&controller_service.getattr("wda_url")?)?.s("/session/").v(&session)?.done()?;
        let monotonic = g(py, "time")?.getattr("monotonic")?;
        let deadline = monotonic.call0()?.add(g(py, "FOCUS_WAIT_SECONDS")?)?;
        let kind = loop {
            let asked = (|| -> PyResult<Obj<'py>> {
                // ref = requests.get(f"{base}/element/active", timeout=10).json().get("value") or {}
                let get = g(py, "requests")?.getattr("get")?;
                let url = Fmt::new(py).v(&base)?.s("/element/active").done()?;
                let answer = get.call((url,), Some(&kw(py, "timeout", 10)?))?.call_method0("json")?;
                let reference = or(answer.call_method1("get", ("value",))?, || Ok(empty_dict(py)))?;
                // uid = ref.get("ELEMENT") or ref.get("element-6066-11e4-a52e-4f735466cecf")
                let uid = or(reference.call_method1("get", ("ELEMENT",))?, || {
                    reference.call_method1("get", ("element-6066-11e4-a52e-4f735466cecf",))
                })?;
                if !uid.is_truthy()? {
                    return Ok(lit(py, ""));
                }
                let get = g(py, "requests")?.getattr("get")?;
                let url = Fmt::new(py).v(&base)?.s("/element/").v(&uid)?.s("/attribute/type").done()?;
                let answer = get.call((url,), Some(&kw(py, "timeout", 10)?))?.call_method0("json")?;
                answer.call_method1("get", ("value",))
            })();
            let kind = match asked {
                Ok(kind) => kind,
                Err(e) => except_exception(py, e, |_| Ok(lit(py, "")))?,
            };
            // An error reply is a dict: no answer yet, never text for a message.
            let kind = if isinstance(&kind, "str")? {
                kind.call_method1("replace", ("XCUIElementType", ""))?
            } else {
                lit(py, "")
            };
            if kind.is_truthy()? || g(py, "time")?.getattr("monotonic")?.call0()?.ge(&deadline)? {
                break kind;
            }
            g(py, "time")?.getattr("sleep")?.call1((PyFloat::new(py, 0.2),))?;
        };
        if !kind.is_truthy()? {
            return Ok(lit(py, "no field took focus after the tap"));
        }
        if password.is_truthy()? && kind.ne("SecureTextField")? {
            let opened = Fmt::new(py).s("focus is on a ");
            return opened
                .v(&kind.call_method0("lower")?)?
                .s(", and a saved password goes only into a password field")
                .done();
        }
        if !g(py, "TEXT_FIELDS")?.contains(&kind)? {
            let opened = Fmt::new(py).s("focus is on a ");
            return opened.v(&kind.call_method0("lower")?)?.s(", not a text field").done();
        }
        let app_launcher_service = from_ios(py, "tool.open_app", "app_launcher_service")?;
        let active = or(app_launcher_service.call_method0("active_app")?, || Ok(empty_dict(py)))?;
        let live = active.call_method1("get", ("bundleId",))?;
        if !live.is_truthy()? || live.eq(g(py, "SPRINGBOARD")?)? || live.ne(slf.getattr("_scan_bundle")?)? {
            return Ok(lit(py, "the app that was scanned is no longer in front, or a system screen is up"));
        }
        Ok(lit(py, ""))
    }

    #[doc = "(entry_name, pin) for the screen in front, for vault {\"id\": 0, \"value\": \"pin\"}.\n\n\
Careful, because a PIN pad locks after a few wrong tries:\n\
\x20 * re-reads credentials.json, so a PIN added while the app runs is used\n\
\x20 * matches the app name EXACTLY (case and spaces ignored), like `fill`\n\
\x20 * reads only \"pin\", never \"code\", and only if it is all digits\n\
Screens with no app in front (lock screen, home screen) use the entry\n\
named after the device: \"iPhone\" or \"iPad\". pin is None when nothing\n\
usable is saved; entry_name still says where to add it.\n"]
    fn get_pin<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        reload(slf.as_any())?;
        let (app_name, device) = unpack2(&slf.call_method0("_scanned_app")?)?;
        let name = or(app_name, || Ok(device))?;
        let wanted = name.call_method0("lower")?.call_method1("replace", (" ", ""))?;
        for pair in slf.getattr("credentials")?.call_method0("items")?.try_iter()? {
            let (app_key, app_creds) = unpack2(&pair?)?;
            let key = app_key.call_method0("lower")?.call_method1("replace", (" ", ""))?;
            if key.eq(&wanted)? && isinstance(&app_creds, "dict")? {
                // pin = str(app_creds.get("pin") or "")
                let str_ = g(py, "str")?;
                let pin = str_.call1((or(app_creds.call_method1("get", ("pin",))?, || Ok(lit(py, "")))?,))?;
                // return name, (pin if re.fullmatch(r"[0-9]+", pin) else None)
                let fullmatch = g(py, "re")?.getattr("fullmatch")?;
                let pin = if fullmatch.call1(("[0-9]+", &pin))?.is_truthy()? { pin } else { none(py) };
                return Ok(PyTuple::new(py, [name, pin])?.into_any());
            }
        }
        Ok(PyTuple::new(py, [name, none(py)])?.into_any())
    }

    #[doc = "{digit: (x, y)} for a full on-screen digit keypad, or None.\n\n\
`elements` is the controller's mapping from the latest scan. All ten\n\
digits must be there, each resolving to exactly one key - checking only\n\
the PIN's own digits would happily tap a \"1\" badge four times for 1111.\n"]
    fn find_pin_keys<'py>(slf: &Bound<'py, Self>, elements: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let candidates = PyDict::new(py);
        for elem in or(elements, || Ok(empty_dict(py)))?.call_method0("values")?.try_iter()? {
            let elem = elem?;
            let read_key = g(py, "_pin_key")?;
            let digit = read_key.call1((elem.call_method1("get", ("name",))?,))?;
            if !digit.is_none() && elem.call_method1("get", ("bounds",))?.is_truthy()? {
                // candidates.setdefault(digit, []).append(elem["bounds"])
                let listed = candidates.call_method1("setdefault", (&digit, PyList::empty(py)))?;
                let append = listed.getattr("append")?;
                append.call1((elem.get_item("bounds")?,))?;
            }
        }

        let keys = PyDict::new(py);
        for digit in "0123456789".chars() {
            let digit = PyString::new(py, digit.encode_utf8(&mut [0; 4]));
            let kept = PyList::empty(py);
            // Outermost first: a label whose centre sits inside a kept key is
            // that same key, not a second one.
            let sorted = g(py, "sorted")?;
            let found = candidates.call_method1("get", (&digit, PyList::empty(py)))?;
            let ordered = sorted.call((found,), Some(&kw(py, "key", area_lambda(py)?)?))?;
            for b in ordered.try_iter()? {
                let b = b?;
                // if not any(_inside(_centre(b), k) for k in kept): kept.append(b)
                let any = g(py, "any")?;
                let points = Genexpr::made(kept.as_any(), Body::Inside { b: b.clone().unbind() })?;
                if !any.call1((points,))?.is_truthy()? {
                    kept.append(&b)?;
                }
            }
            if g(py, "len")?.call1((&kept,))?.ne(1)? {
                return Ok(none(py));
            }
            let middle = g(py, "_centre")?.call1((kept.get_item(0)?,))?;
            keys.set_item(&digit, middle)?;
        }
        Ok(keys.into_any())
    }

    #[pyo3(
        signature = (controller_service, keys, pin, device_entry = None),
        text_signature = "($self, controller_service, keys, pin, device_entry=False)"
    )]
    #[doc = "Tap `pin` on the keypad from find_pin_keys, then confirm it registered.\n\n\
Returns {\"status\", \"message\", \"pressed\", \"closed\"}: `pressed` says a\n\
wrong-PIN try may have been spent, `closed` that the PIN screen went\n\
away. Messages never contain a digit, a key id or a coordinate.\n"]
    fn enter_pin<'py>(
        slf: &Bound<'py, Self>,
        controller_service: Obj<'py>,
        keys: Obj<'py>,
        pin: Obj<'py>,
        device_entry: Option<Obj<'py>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // def result(status, message, pressed, closed=False): ...
        let result = |status: &str, message: &str, pressed: bool, closed: bool| outcome(py, status, lit(py, message), pressed, closed);
        // before/after, keypad_open = _read_pin_screen(controller_service)
        let read = || -> PyResult<(Obj<'py>, Obj<'py>)> {
            let read_pin_screen = g(py, "_read_pin_screen")?;
            unpack2(&read_pin_screen.call1((&controller_service,))?)
        };

        // Re-read the screen right before tapping: the keys come from the scan
        // taken before the model thought, and the pad may be gone by now.
        let (before, keypad_open) = match read() {
            Ok(read) => read,
            Err(e) => {
                return except_exception(py, e, |_| {
                    result("error", "Could not read the screen, nothing was pressed. It is safe to try once more.", false, false)
                })
            }
        };
        if !keypad_open.is_truthy()? {
            return result(
                "error",
                "The PIN keypad is no longer on screen, nothing was pressed. Open the PIN screen again first.",
                false,
                false,
            );
        }
        let device_entry = match &device_entry {
            Some(device_entry) => device_entry.is_truthy()?,
            None => false,
        };
        if device_entry && before.is_none() {
            // The device passcode only ever goes into the native passcode field.
            return result(
                "error",
                "No iPhone passcode field on screen, so the device PIN was not used. Open the lock-screen passcode first.",
                false,
                false,
            );
        }
        if !before.is_none()
            && (before.get_item(0)?.ne(0)? || g(py, "len")?.call1((&pin,))?.ne(before.get_item(1)?)?)
        {
            return result(
                "error",
                "The passcode field is not empty, or the saved PIN does not fit this passcode, so nothing was pressed. Ask the user.",
                false,
                false,
            );
        }

        let session = controller_service.call_method0("get_session")?;
        if !session.is_truthy()? {
            return result("error", "No WDA session, nothing was pressed. It is safe to try once more.", false, false);
        }
        let tapped = (|| -> PyResult<()> {
            // _tap_pin_keys(controller_service, session, [keys[d] for d in pin])
            let tap = g(py, "_tap_pin_keys")?;
            let points = PyList::empty(py);
            for d in comprehension(&pin)? {
                points.append(keys.get_item(d?)?)?;
            }
            tap.call1((&controller_service, &session, points))?;
            Ok(())
        })();
        if let Err(e) = tapped {
            return except_exception(py, e, |_| {
                result(
                    "error",
                    "The key presses may have gone through. Do not retry - check the next screenshot, and ask the user if unsure.",
                    true,
                    false,
                )
            });
        }
        let sleep = g(py, "time")?.getattr("sleep")?;
        sleep.call1((g(py, "PIN_SETTLE_SECONDS")?,))?;

        let (after, keypad_open) = match read() {
            Ok(read) => read,
            Err(e) => {
                return except_exception(py, e, |_| {
                    result(
                        "success",
                        "PIN keys pressed, but the screen could not be read back. Check the next screenshot before doing anything else.",
                        true,
                        false,
                    )
                })
            }
        };

        if !before.is_none() {
            if after.is_none() {
                return result(
                    "success",
                    "The passcode screen closed. Confirm on the next screenshot that the device unlocked.",
                    true,
                    true,
                );
            }
            let (entered, total) = unpack2(&after)?;
            if entered.eq(&total)? {
                let message = Fmt::new(py)
                    .s("PIN submitted, every key press registered (")
                    .v(&entered)?
                    .s(" of ")
                    .v(&total)?
                    .s("). Confirm on the next screenshot that it was accepted.")
                    .done()?;
                return outcome(py, "success", message, true, false);
            }
            if entered.eq(0)? {
                // The field was empty and the PIN fills it exactly, so a reset
                // to 0 means iOS rejected the code (a try was spent).
                return result(
                    "error",
                    "The PIN was not accepted. Do not retry - ask the user to check the PIN saved in the vault.",
                    true,
                    false,
                );
            }
            let message = Fmt::new(py)
                .s("Only part of the PIN registered (")
                .v(&entered)?
                .s(" of ")
                .v(&total)?
                .s(" values entered). Do not retry - ask the user.")
                .done()?;
            return outcome(py, "error", message, true, false);
        }

        if !keypad_open.is_truthy()? {
            return result("success", "PIN entered and the keypad closed.", true, true);
        }
        // No entry count to read, and the pad is still up after the whole PIN:
        // it was not accepted, or a press did not land. Never call that success.
        result(
            "error",
            "The keypad is still open after the whole PIN: it was not accepted or a press did not register. Do not retry - if the boxes are full and an OK/Continue button shows, tap it; otherwise ask the user.",
            true,
            false,
        )
    }

    #[doc = "Parse element line to extract attributes"]
    fn _parse_element_line<'py>(slf: &Bound<'py, Self>, line: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let parsed = (|| -> PyResult<Obj<'py>> {
            // Two tree dialects reach here:
            //   iOS  : [1]<element_name="Instagram", type="application" />
            //   older: [1]<type="button", label="Back", value="" />
            // `element_name` is what ios/tree/element.py actually emits today, so
            // read the label from either key rather than only `label=`.
            let attrs = PyDict::new(py);

            // Extract type
            if line.contains("type=\"")? {
                let start = line.call_method1("find", ("type=\"",))?.add(6)?;
                let end = line.call_method1("find", ("\"", &start))?;
                attrs.set_item("type", slice(&line, Some(&start), Some(&end))?)?;
            }

            // Extract label (element_name= wins; it is the iOS spelling)
            for key in ["element_name=\"", "label=\""] {
                if line.contains(key)? {
                    let at = line.call_method1("find", (key,))?;
                    let start = at.add(g(py, "len")?.call1((key,))?)?;
                    let end = line.call_method1("find", ("\"", &start))?;
                    attrs.set_item("label", slice(&line, Some(&start), Some(&end))?)?;
                    break;
                }
            }

            // Extract value
            if line.contains("value=\"")? {
                let start = line.call_method1("find", ("value=\"",))?.add(7)?;
                let end = line.call_method1("find", ("\"", &start))?;
                attrs.set_item("value", slice(&line, Some(&start), Some(&end))?)?;
            }

            Ok(attrs.into_any())
        })();
        match parsed {
            Ok(attrs) => Ok(attrs),
            Err(e) => except_exception(py, e, |e| {
                log_error(py, "Error parsing element line: ", e)?;
                Ok(none(py))
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// service.py's lines, run once the loader bound its imports
// ---------------------------------------------------------------------------

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// A str tuple of names, as a constant tuple of service.py holds them.
fn names<'py>(py: Python<'py>, items: &[&str]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items.iter().map(|item| PyString::intern(py, item)))
}

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // logger = logging.getLogger(__name__)
    let get_logger = g(py, "logging")?.getattr("getLogger")?;
    store(m, "logger", get_logger.call1((g(py, "__name__")?,))?)?;

    // PIN keypads (see the comment above _pin_key).

    // One digit, optionally with its phone letters ("2 ABC", "2, A B C", "0 +").
    // Phone letters are uppercase, so "1 of 3" and "5 min" do not match.
    let compile = g(py, "re")?.getattr("compile")?;
    store(m, "PIN_KEY_LABEL", compile.call1((PIN_KEY_PATTERN,))?)?;

    // The native passcode field's value, e.g. "4 of 6 values entered".
    let compile = g(py, "re")?.getattr("compile")?;
    store(m, "PIN_ENTRY_COUNT", compile.call1((PIN_ENTRY_PATTERN,))?)?;

    // Finger down this long per key. Keys go one request each, back to back: each
    // request takes ~0.4s on the device, which is also what keeps a repeated digit
    // (0000) from reading as a double tap.
    store(m, "PIN_PRESS_MS", 80)?;

    // After the last key, before reading the screen back.
    store(m, "PIN_SETTLE_SECONDS", 1.0)?;

    // Tries per PIN entry per task. A pad that closes resets it; pads lock after a few wrong tries.
    store(m, "MAX_PIN_ATTEMPTS", 2)?;

    // Vault entries for the device itself (lock screen). Only get_pin reads these,
    // and only while the native passcode field is on screen.
    store(m, "DEVICE_ENTRIES", names(py, &["iphone", "ipad"])?)?;

    // (connect, read) seconds for the WDA source dump - same bound as the scanner.
    store(m, "WDA_SOURCE_TIMEOUT", (5, 60))?;

    store(m, "_pin_key", wrap_pyfunction!(pin_key, m)?)?;
    store(m, "_centre", wrap_pyfunction!(centre, m)?)?;
    store(m, "_inside", wrap_pyfunction!(inside, m)?)?;
    store(m, "_read_pin_screen", wrap_pyfunction!(read_pin_screen, m)?)?;
    store(m, "_tap_pin_keys", wrap_pyfunction!(tap_pin_keys, m)?)?;

    // Field fills (see the comment above _norm).

    // The fields a saved value may go into (the controller mapping's type).
    store(m, "TEXT_FIELDS", names(py, &["TextField", "SecureTextField", "TextView", "SearchField"])?)?;

    // The app WDA reports in front while a system screen (an alert) is up.
    store(m, "SPRINGBOARD", "com.apple.springboard")?;

    // How long a tap gets to land focus in a field before the fill gives up.
    store(m, "FOCUS_WAIT_SECONDS", 1.5)?;

    // What a value the vault typed reads as, back on screen.
    store(m, "HIDDEN", "[vault]")?;

    // The tree cuts a long label to its first 47 characters and "..."
    // (ios/tree/element.py), so a longer value is hidden by those too.
    store(m, "CLIP_KEEP", 47)?;

    // The values the vault typed this chat, longest first.
    store(m, "_filled", PyList::empty(py))?;

    store(m, "_norm", wrap_pyfunction!(norm, m)?)?;
    store(m, "_text_of", wrap_pyfunction!(text_of, m)?)?;
    store(m, "_usable_keys", wrap_pyfunction!(usable_keys, m)?)?;
    store(m, "new_chat", wrap_pyfunction!(new_chat, m)?)?;
    store(m, "_remember", wrap_pyfunction!(remember, m)?)?;
    store(m, "hide", wrap_pyfunction!(hide, m)?)?;
    store(m, "hide_json", wrap_pyfunction!(hide_json, m)?)?;
    store(m, "_hide_one", wrap_pyfunction!(hide_one, m)?)?;

    // class VaultService:
    store(m, "VaultService", py.get_type::<VaultService>())?;

    // Create global instance
    let class = g(py, "VaultService")?;
    store(m, "vault_service", class.call0()?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with _build, which the
/// loader calls once it has bound the import lines' names (see the top of
/// this file), and makes the class look like the one service.py made.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<VaultService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
