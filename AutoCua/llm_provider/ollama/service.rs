//! service: the Ollama provider (OllamaProvider), which sends one OpenAI-
//! format chat completions request to the Ollama at OLLAMA_HOST (a model on
//! this machine by default) and folds the reply's tool calls into the shape
//! the loops read; and the helpers the app and Telegram call to find that
//! Ollama and the models it has pulled (base_url, reachable, pulled_models).
//!
//! The Rust copy of llm_provider/ollama/service.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.ollama.service
//! (llm_provider/ollama/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values and
//! errors. Python's own rules decide every result, so they are called, not
//! imitated: requests, ipaddress, json, urlsplit, the builtins, the str,
//! dict, list and set methods, the operators, the comparisons, the slices
//! and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (ipaddress, json, os, requests, Dict,
//! Any, Optional, urlsplit, entry_value, http_timeout, out_cap, screenshots)
//! and calls _build(), which runs the rest of service.py's lines in their
//! order: the three constants, the six functions, the class and the last
//! function.
//!
//! Every global a line reads (os, requests, ipaddress, json, urlsplit,
//! base_url, _session, _key, READ_TIMEOUT, print, isinstance, str, ...) is
//! read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces one is obeyed. _key and _check_context
//! import what they need when they run, as service.py's did. The provider
//! keeps its attributes in its __dict__, and its methods reach them through
//! the instance, so a subclass or a patched attribute (the session) is
//! obeyed. An error is raised the way Python's raise statement raises it, so
//! the error being handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::{PyAttributeError, PyKeyError, PyRuntimeError, PyStopIteration, PyTypeError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyIterator, PyList, PySlice, PyString, PyTuple};

use super::view::import_from_level;
use crate::computer::{as_python_class, type_name, unpack2, Handling};
use crate::ios::controller::{self, caught, import_from, now, unpack3};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// OllamaProvider's docstring.
const CLASS_DOC: &str = "Ollama provider: a model on this machine, or the Ollama at OLLAMA_HOST\n\
(OpenAI-compatible chat completions at <host>/v1)";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 152;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 11] = [
    "_context_checked",
    "_shown",
    "_too_small",
    "api_key",
    "api_url",
    "base_url",
    "cli_agent",
    "max_tokens",
    "model_info",
    "session",
    "tools",
];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.ollama.service", super::super::NATIVE)).map(Bound::unbind))
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

/// `isinstance(value, <kind>)` as a value (an `and` chain keeps it): what
/// isinstance answered, isinstance and the class read from this module now.
fn isinstance_value<'py>(value: &Obj<'py>, kind: &str) -> PyResult<Obj<'py>> {
    let py = value.py();
    let check = g(py, "isinstance")?;
    let class = g(py, kind)?;
    check.call1((value, class))
}

/// `raise <kind>(<message>)`: the global `kind` read first, then the message
/// made, then the error made and raised, in Python's order.
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

/// An `except <kinds>:` test of `e`, made while `e` is being handled (the
/// caller holds its Handling): `kinds` reads the clause's classes from this
/// module now, and an error in reading them carries `e` as its __context__.
fn matches<'py>(py: Python<'py>, e: &PyErr, kinds: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<bool> {
    let kinds = kinds().map_err(|raised| now(py, raised))?;
    caught(py, e, &kinds)
}

/// `requests.exceptions.<name>`, read now.
fn request_error<'py>(py: Python<'py>, name: &str) -> PyResult<Obj<'py>> {
    g(py, "requests")?.getattr("exceptions")?.getattr(name)
}

/// `(requests.exceptions.RequestException, ValueError, AttributeError)`,
/// the tuple an except clause of service.py builds, read now.
fn request_value_attribute(py: Python<'_>) -> PyResult<Obj<'_>> {
    let request = request_error(py, "RequestException")?;
    let value = g(py, "ValueError")?;
    let attribute = g(py, "AttributeError")?;
    Ok(PyTuple::new(py, [request, value, attribute])?.into_any())
}

/// A str constant.
fn lit<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

fn false_(py: Python<'_>) -> Obj<'_> {
    PyBool::new(py, false).to_owned().into_any()
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

/// `[]`, a new empty list.
fn empty_list(py: Python<'_>) -> Obj<'_> {
    PyList::empty(py).into_any()
}

/// `value.get(key)`, the method looked up when it is called.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// `value.get(key, default)`.
fn get_or<'py>(value: &Obj<'py>, key: &str, default: Obj<'py>) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key, default))
}

/// `a == "<b>"`, as an `if` takes it: Python's comparison, then its truth.
fn eq(a: &Obj<'_>, b: &str) -> PyResult<bool> {
    a.rich_compare(b, CompareOp::Eq)?.is_truthy()
}

/// A dict display with str keys, built in the display's order.
fn dict<'py>(py: Python<'py>, items: &[(&str, &Obj<'py>)]) -> PyResult<Obj<'py>> {
    let made = PyDict::new(py);
    for (key, value) in items {
        made.set_item(PyString::intern(py, key), value)?;
    }
    Ok(made.into_any())
}

/// A list display of one item.
fn list1<'py>(item: Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(PyList::new(item.py(), [item])?.into_any())
}

/// A tuple display of two items.
fn pair<'py>(a: Obj<'py>, b: Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(PyTuple::new(a.py(), [a, b])?.into_any())
}

/// A whole number constant.
fn int(py: Python<'_>, value: i64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

/// A float constant.
fn float(py: Python<'_>, value: f64) -> Obj<'_> {
    PyFloat::new(py, value).into_any()
}

/// The slice `[start:stop]` makes: slice(start, stop, None), None where a
/// bound is left out.
fn slice<'py>(py: Python<'py>, start: Obj<'py>, stop: Obj<'py>) -> PyResult<Obj<'py>> {
    py.get_type::<PySlice>().call1((start, stop, py.None()))
}

/// Whether a comprehension or a generator takes iter() a second time of the
/// iterator it loops over: Python 3.13's code does, 3.12's and 3.14's loop
/// over it as it is.
fn iter_again(py: Python<'_>) -> bool {
    let version = py.version_info();
    version >= (3, 13) && version < (3, 14)
}

/// What a comprehension loops over: iter() of `source`, then on Python
/// 3.13 iter() of that again.
fn comprehension<'py>(source: &Obj<'py>) -> PyResult<Bound<'py, PyIterator>> {
    let items = source.try_iter()?;
    if iter_again(source.py()) {
        return items.try_iter();
    }
    Ok(items)
}

/// An f-string, made as Python makes one: each {value} is formatted the
/// moment it is reached (format(value, '')), and the pieces are joined at
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

/// A str tuple of names, as a constant tuple of service.py holds them.
fn names<'py>(py: Python<'py>, items: &[&str]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items.iter().map(|item| PyString::intern(py, item)))
}

/// A def's parameters bound to a call's arguments, as Python binds them:
/// the positional ones first, then the keywords; each slot is the argument
/// given, or None where the def's default applies. `selfs` is 1 for a
/// method, whose self Python counts in its messages.
fn bind<'py>(
    function: &str,
    selfs: usize,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
    params: &[&str],
    required: usize,
) -> PyResult<Vec<Option<Obj<'py>>>> {
    let most = params.len();
    let given = args.len();
    if given > most {
        let message = if required == most {
            format!(
                "{function}() takes {} positional argument{} but {} {} given",
                most + selfs,
                if most + selfs == 1 { "" } else { "s" },
                given + selfs,
                if given + selfs == 1 { "was" } else { "were" }
            )
        } else {
            format!(
                "{function}() takes from {} to {} positional arguments but {} were given",
                required + selfs,
                most + selfs,
                given + selfs
            )
        };
        return Err(PyTypeError::new_err(message));
    }
    let mut slots: Vec<Option<Obj<'py>>> = vec![None; most];
    for (slot, value) in slots.iter_mut().zip(args.iter()) {
        *slot = Some(value);
    }
    if let Some(kwargs) = kwargs {
        for (keyword, value) in kwargs.iter() {
            let keyword = keyword.str()?.to_string();
            match params.iter().position(|param| *param == keyword) {
                Some(at) if slots[at].is_some() => {
                    return Err(PyTypeError::new_err(format!(
                        "{function}() got multiple values for argument '{keyword}'"
                    )));
                }
                Some(at) => slots[at] = Some(value),
                None => {
                    return Err(PyTypeError::new_err(format!(
                        "{function}() got an unexpected keyword argument '{keyword}'"
                    )));
                }
            }
        }
    }
    let missing: Vec<String> = params[..required]
        .iter()
        .zip(&slots)
        .filter(|(_, slot)| slot.is_none())
        .map(|(param, _)| format!("'{param}'"))
        .collect();
    if !missing.is_empty() {
        let listed = match missing.len() {
            1 => missing[0].clone(),
            2 => format!("{} and {}", missing[0], missing[1]),
            n => format!("{}, and {}", missing[..n - 1].join(", "), missing[n - 1]),
        };
        return Err(PyTypeError::new_err(format!(
            "{function}() missing {} required positional argument{}: {listed}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        )));
    }
    Ok(slots)
}

/// A special method the way a statement finds it: on the type, never on the
/// instance, and bound to the object.
fn special<'py>(value: &Obj<'py>, name: &str) -> PyResult<Option<Obj<'py>>> {
    let py = value.py();
    let kind = value.get_type();
    for base in kind.getattr("__mro__")?.try_iter()? {
        let found = match base?.getattr("__dict__")?.get_item(name) {
            Ok(found) => found,
            Err(e) if e.is_instance_of::<PyKeyError>(py) => continue,
            Err(e) => return Err(e),
        };
        return match found.get_type().getattr("__get__") {
            Ok(get) => Ok(Some(get.call1((&found, value, &kind))?)),
            Err(e) if e.is_instance_of::<PyAttributeError>(py) => Ok(Some(found)),
            Err(e) => Err(e),
        };
    }
    Ok(None)
}

/// Whether the type of `value` (or a base) has `name` in its own dict, as
/// the interpreter checks it: nothing bound, the instance not asked.
fn on_type(value: &Obj<'_>, name: &str) -> PyResult<bool> {
    for base in value.get_type().getattr("__mro__")?.try_iter()? {
        if base?.getattr("__dict__")?.contains(name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The type of `value` as an error text of Python 3.13+ names it (%T): a
/// built-in type's C name; else module.qualname, or the bare qualname for a
/// class of builtins or __main__.
fn full_name(value: &Obj<'_>) -> PyResult<String> {
    let kind = value.get_type();
    // SAFETY: a valid type object.
    let flags = unsafe { ffi::PyType_GetFlags(kind.as_type_ptr()) };
    if flags & ffi::Py_TPFLAGS_HEAPTYPE == 0 {
        return type_name(value);
    }
    let qualname: String = kind.getattr("__qualname__")?.extract()?;
    if let Ok(module) = kind.getattr("__module__")?.extract::<String>() {
        if module != "builtins" && module != "__main__" {
            return Ok(format!("{module}.{qualname}"));
        }
    }
    Ok(qualname)
}

/// `with manager as value: body(value)`. None when the body failed and
/// __exit__ swallowed the error: Python then goes on after the block.
/// Python 3.14 looks __exit__ up first and names the method it missed.
fn with<'py, T>(manager: &Obj<'py>, body: impl FnOnce(&Obj<'py>) -> PyResult<T>) -> PyResult<Option<T>> {
    let py = manager.py();
    let (enter, exit) = if py.version_info() >= (3, 14) {
        let missed = |method: &str| -> PyResult<PyErr> {
            let mut message = format!(
                "'{}' object does not support the context manager protocol (missed {method} method)",
                full_name(manager)?
            );
            if on_type(manager, "__aenter__")? && on_type(manager, "__aexit__")? {
                message.push_str(
                    " but it supports the asynchronous context manager protocol. Did you mean to use 'async with'?",
                );
            }
            Ok(PyTypeError::new_err(message))
        };
        let Some(exit) = special(manager, "__exit__")? else {
            return Err(missed("__exit__")?);
        };
        let Some(enter) = special(manager, "__enter__")? else {
            return Err(missed("__enter__")?);
        };
        (enter, exit)
    } else {
        let Some(enter) = special(manager, "__enter__")? else {
            return Err(PyTypeError::new_err(format!(
                "'{}' object does not support the context manager protocol",
                type_name(manager)?
            )));
        };
        let Some(exit) = special(manager, "__exit__")? else {
            return Err(PyTypeError::new_err(format!(
                "'{}' object does not support the context manager protocol (missed __exit__ method)",
                type_name(manager)?
            )));
        };
        (enter, exit)
    };
    let value = enter.call0()?;
    match body(&value) {
        Ok(result) => {
            exit.call1((py.None(), py.None(), py.None()))?;
            Ok(Some(result))
        }
        Err(e) => {
            // __exit__ runs while the body's error is being handled.
            let _handling = Handling::new(py, &e);
            let traceback = e.traceback(py).map(|t| t.into_any()).unwrap_or_else(|| none(py));
            if exit.call1((e.get_type(py), e.value(py), traceback))?.is_truthy()? {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The generator expression
// ---------------------------------------------------------------------------

/// What a generator expression's body does with one item: Some(value) to
/// give it, None to go on to the next item.
type Body = Box<dyn FnMut(&Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> + Send>;

/// A body, typed so the closure takes any item.
fn body<F>(step: F) -> Body
where
    F: FnMut(&Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> + Send + 'static,
{
    Box::new(step)
}

// The generator expression handed to next() in _check_context, run as
// Python runs one (as responses.rs runs its own). Made holding iter() of
// what it loops over, on Python 3.13 its first step takes iter() of that
// again, as that version's generator code does; then it runs its body one
// value per next(). A StopIteration from its own body comes out as
// RuntimeError (PEP 479), and once it has ended or failed it gives nothing
// more. A plain comment: a doc comment would give the class a docstring, and
// a generator has none.
#[pyclass(frozen, name = "generator", module = "builtins")]
struct Genexpr {
    /// The function that made it, as its __qualname__ names the generator.
    qualname: &'static str,
    /// iter() of what it loops over, made when the generator was made.
    given: Py<PyIterator>,
    /// iter() of `given` on Python 3.13, taken when the generator first runs.
    items: PyOnceLock<Py<PyIterator>>,
    /// The body; held while a step runs.
    body: Mutex<Body>,
    /// The generator has ended or failed.
    done: AtomicBool,
}

impl Genexpr {
    /// The generator made: iter() of `source` taken now.
    fn made<'py>(source: &Obj<'py>, qualname: &'static str, body: Body) -> PyResult<Obj<'py>> {
        let given = source.try_iter()?;
        let generator = Genexpr {
            qualname,
            given: given.unbind(),
            items: PyOnceLock::new(),
            body: Mutex::new(body),
            done: AtomicBool::new(false),
        };
        Ok(Bound::new(source.py(), generator)?.into_any())
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
        let Ok(mut step) = me.body.try_lock() else {
            return Err(now(py, PyValueError::new_err("generator already executing")));
        };
        if me.done.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let ran = (|| -> PyResult<Option<Obj<'py>>> {
            let items = if iter_again(py) {
                let again = || me.given.bind(py).as_any().try_iter().map(Bound::unbind);
                me.items.get_or_try_init(py, again)?.bind(py).clone()
            } else {
                me.given.bind(py).clone()
            };
            for item in items {
                if let Some(value) = (*step)(&item?)? {
                    return Ok(Some(value.into_bound(py)));
                }
            }
            Ok(None)
        })();
        drop(step);
        match ran {
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
        format!("<generator object {} at {:p}>", slf.get().qualname, slf.as_ptr())
    }
}

/// The qualname of _check_context's generator expression.
const CHECK_GENEXPR: &str = "OllamaProvider._check_context.<locals>.<genexpr>";

// ---------------------------------------------------------------------------
// The functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[doc = "Ollama's address from OLLAMA_HOST, read as Ollama's own client reads\n\
it (envconfig.Host): surrounding spaces and quotes dropped; no scheme\n\
means http and port 11434, while http:// and https:// default to 80 and\n\
443; a bare IPv6 address is bracketed; an unreadable port falls back to\n\
the default; a bare \"ollama.com\" means https://ollama.com:443. Unlike\n\
Ollama, 0.0.0.0 and :: (a server listening on every address) mean this\n\
machine, which Windows cannot dial as such. Unset: 127.0.0.1:11434."]
fn base_url(py: Python<'_>) -> PyResult<Obj<'_>> {
    // raw = (os.getenv("OLLAMA_HOST") or "").strip().strip("\"'")
    let getenv = g(py, "os")?.getattr("getenv")?;
    let raw = or(getenv.call1(("OLLAMA_HOST",))?, || Ok(lit(py, "")))?;
    let raw = raw.call_method0("strip")?.call_method1("strip", ("\"'",))?;
    // scheme, sep, hostport = raw.partition("://")
    let (mut scheme, sep, mut hostport) = unpack3(&raw.call_method1("partition", ("://",))?)?;
    let default_port;
    if !sep.is_truthy()? {
        // scheme, hostport, default_port = "http", raw, "11434"
        scheme = lit(py, "http");
        hostport = raw.clone();
        default_port = lit(py, "11434");
        if eq(&raw, "ollama.com")? {
            scheme = lit(py, "https");
            hostport = lit(py, "ollama.com:443");
        }
    } else {
        // default_port = {"http": "80", "https": "443"}.get(scheme, "11434")
        let ports = dict(py, &[("http", &lit(py, "80")), ("https", &lit(py, "443"))])?;
        default_port = ports.call_method1("get", (&scheme, "11434"))?;
    }
    // hostport, _, path = hostport.partition("/")
    let (hostport, _, path) = unpack3(&hostport.call_method1("partition", ("/",))?)?;
    // host, port = _split_host_port(hostport)
    let (mut host, mut port) = unpack2(&g(py, "_split_host_port")?.call1((&hostport,))?)?;
    if host.is_none() {
        host = lit(py, "127.0.0.1");
        port = default_port.clone();
        // try: host = str(ipaddress.ip_address(hostport.strip("[]")))
        let tried = (|| -> PyResult<Obj<'_>> {
            let to_str = g(py, "str")?;
            let ip_address = g(py, "ipaddress")?.getattr("ip_address")?;
            let address = ip_address.call1((hostport.call_method1("strip", ("[]",))?,))?;
            to_str.call1((address,))
        })();
        match tried {
            Ok(found) => host = found,
            Err(e) => {
                // except ValueError: if hostport: host = hostport
                let _handling = Handling::new(py, &e);
                if !matches(py, &e, || g(py, "ValueError"))? {
                    return Err(e);
                }
                if hostport.is_truthy().map_err(|raised| now(py, raised))? {
                    host = hostport.clone();
                }
            }
        }
    }
    // if not port.isdigit() or int(port) > 65535: port = default_port
    let unreadable = !port.call_method0("isdigit")?.is_truthy()?
        || g(py, "int")?.call1((&port,))?.rich_compare(65535, CompareOp::Gt)?.is_truthy()?;
    if unreadable {
        port = default_port;
    }
    // if host in ("", "0.0.0.0", "::"): host = "127.0.0.1"
    if names(py, &["", "0.0.0.0", "::"])?.as_any().contains(&host)? {
        host = lit(py, "127.0.0.1");
    }
    // if ":" in host: host = f"[{host}]"
    if host.contains(":")? {
        host = Fmt::new(py).s("[").v(&host)?.s("]").done()?;
    }
    let path = path.call_method1("rstrip", ("/",))?;
    // return f"{scheme}://{host}:{port}" + (f"/{path}" if path else "")
    let head = Fmt::new(py).v(&scheme)?.s("://").v(&host)?.s(":").v(&port)?.done()?;
    let tail = if path.is_truthy()? { Fmt::new(py).s("/").v(&path)?.done()? } else { lit(py, "") };
    head.add(tail)
}

#[pyfunction]
#[pyo3(name = "_split_host_port")]
#[doc = "(host, port) like Go's net.SplitHostPort, or (None, None) when\n\
`hostport` has no port or too many colons (a bare IPv6 address)."]
fn split_host_port(hostport: Obj<'_>) -> PyResult<Obj<'_>> {
    let py = hostport.py();
    if hostport.call_method1("startswith", ("[",))?.is_truthy()? {
        // end = hostport.find("]")
        let end = hostport.call_method1("find", ("]",))?;
        // if end < 0 or not hostport[end + 1:].startswith(":"): return None, None
        let unclosed = end.rich_compare(0, CompareOp::Lt)?.is_truthy()?
            || !hostport
                .get_item(slice(py, end.add(1)?, none(py))?)?
                .call_method1("startswith", (":",))?
                .is_truthy()?;
        if unclosed {
            return pair(none(py), none(py));
        }
        // return hostport[1:end], hostport[end + 2:]
        let host = hostport.get_item(slice(py, int(py, 1)?, end.clone())?)?;
        let port = hostport.get_item(slice(py, end.add(2)?, none(py))?)?;
        return pair(host, port);
    }
    // if hostport.count(":") != 1: return None, None
    if hostport.call_method1("count", (":",))?.rich_compare(1, CompareOp::Ne)?.is_truthy()? {
        return pair(none(py), none(py));
    }
    // host, _, port = hostport.partition(":")
    let (host, _, port) = unpack3(&hostport.call_method1("partition", (":",))?)?;
    pair(host, port)
}

#[pyfunction]
#[pyo3(name = "_key")]
#[doc = "OLLAMA_API_KEY from the environment, .env or the app's Settings, the\n\
way build_provider looks it up; None for an Ollama without one."]
fn key(py: Python<'_>) -> PyResult<Obj<'_>> {
    // from ..llm_manager import _key as lookup
    let lookup = import_from_level(&module(py)?, "llm_manager", 2, "_key")?;
    lookup.call1((none(py), "OLLAMA_API_KEY"))
}

#[pyfunction]
#[pyo3(name = "_session", signature = (*args, **kwargs), text_signature = "(url, api_key=None)")]
#[doc = "A session for the Ollama at `url`, sending `api_key` (an Ollama\n\
behind a key) on every call. One on this machine is reached directly:\n\
requests otherwise sends every call through the proxy the environment or\n\
macOS names, and macOS's default proxy exceptions do not cover\n\
127.0.0.1."]
fn make_session<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_session", 0, args, kwargs, &["url", "api_key"], 1)?;
    let mut slots = slots.into_iter();
    let url = slots.next().flatten().unwrap_or_else(|| none(py));
    let api_key = slots.next().flatten().unwrap_or_else(|| none(py));
    // session = requests.Session()
    let session = g(py, "requests")?.getattr("Session")?.call0()?;
    if api_key.is_truthy()? {
        // session.headers["Authorization"] = f"Bearer {api_key}"
        let value = Fmt::new(py).s("Bearer ").v(&api_key)?.done()?;
        session.getattr("headers")?.set_item("Authorization", value)?;
    }
    // host = urlsplit(url).hostname or ""
    let urlsplit = g(py, "urlsplit")?;
    let host = or(urlsplit.call1((&url,))?.getattr("hostname")?, || Ok(lit(py, "")))?;
    // try: local = host == "localhost" or ipaddress.ip_address(host).is_loopback
    let tried = (|| -> PyResult<Obj<'py>> {
        let same = host.rich_compare("localhost", CompareOp::Eq)?;
        or(same, || g(py, "ipaddress")?.getattr("ip_address")?.call1((&host,))?.getattr("is_loopback"))
    })();
    let local = match tried {
        Ok(local) => local,
        Err(e) => {
            // except ValueError: local = False
            let _handling = Handling::new(py, &e);
            if !matches(py, &e, || g(py, "ValueError"))? {
                return Err(e);
            }
            false_(py)
        }
    };
    if local.is_truthy()? {
        session.setattr("trust_env", false)?;
    }
    Ok(session)
}

/// `timeout=(0.5, timeout)`, the keywords of a call that waits `timeout`.
fn short_wait<'py>(py: Python<'py>, timeout: &Obj<'py>) -> PyResult<Bound<'py, PyDict>> {
    let kw = PyDict::new(py);
    kw.set_item("timeout", pair(float(py, 0.5), timeout.clone())?)?;
    Ok(kw)
}

/// `with _session(url, _key()) as session:`, the line reachable and
/// pulled_models start their work with: the names read, the key looked up
/// and the session made in Python's order, then `work` run in the block.
fn with_session<'py>(
    py: Python<'py>,
    url: &Obj<'py>,
    work: impl FnOnce(&Obj<'py>) -> PyResult<Obj<'py>>,
) -> PyResult<Option<Obj<'py>>> {
    let make = g(py, "_session")?;
    let api_key = g(py, "_key")?.call0()?;
    let manager = make.call1((url, api_key))?;
    with(&manager, work)
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(timeout=1.0)")]
#[doc = "True when an Ollama answers at base_url(): Telegram offers the\n\
provider only then."]
fn reachable<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("reachable", 0, args, kwargs, &["timeout"], 0)?;
    let timeout = slots.into_iter().next().flatten().unwrap_or_else(|| float(py, 1.0));
    // url = base_url()
    let url = g(py, "base_url")?.call0()?;
    let tried = with_session(py, &url, |session| {
        // return session.get(f"{url}/api/version", timeout=(0.5, timeout)).ok
        let get = session.getattr("get")?;
        let address = Fmt::new(py).v(&url)?.s("/api/version").done()?;
        get.call((address,), Some(&short_wait(py, &timeout)?))?.getattr("ok")
    });
    match tried {
        Ok(Some(ok)) => Ok(ok),
        // __exit__ swallowed an error: the function ran off its end.
        Ok(None) => Ok(none(py)),
        Err(e) => {
            // except requests.exceptions.RequestException: return False
            let _handling = Handling::new(py, &e);
            if !matches(py, &e, || request_error(py, "RequestException"))? {
                return Err(e);
            }
            Ok(false_(py))
        }
    }
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(timeout=2.0)")]
#[doc = "The models this Ollama has pulled that can call tools (every agent\n\
needs it), in `ollama list` order, as (name, capabilities) with\n\
capabilities None when Ollama does not say: the app's and Telegram's\n\
model lists (view.listed_models). Raises when Ollama does not answer."]
fn pulled_models<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("pulled_models", 0, args, kwargs, &["timeout"], 0)?;
    let timeout = slots.into_iter().next().flatten().unwrap_or_else(|| float(py, 2.0));
    // url = base_url()
    let url = g(py, "base_url")?.call0()?;
    let done = with_session(py, &url, |session| {
        // r = session.get(f"{url}/api/tags", timeout=(0.5, timeout))
        let get_tags = session.getattr("get")?;
        let address = Fmt::new(py).v(&url)?.s("/api/tags").done()?;
        let r = get_tags.call((address,), Some(&short_wait(py, &timeout)?))?;
        r.call_method0("raise_for_status")?;
        let models = PyList::empty(py);
        // for m in r.json().get("models") or []:
        let listed = or(get(&r.call_method0("json")?, "models")?, || Ok(empty_list(py)))?;
        for m in listed.try_iter()? {
            let m = m?;
            // name = isinstance(m, dict) and (m.get("name") or m.get("model"))
            let check = isinstance_value(&m, "dict")?;
            let name = if check.is_truthy()? { or(get(&m, "name")?, || get(&m, "model"))? } else { check };
            if !name.is_truthy()? {
                continue;
            }
            let tried = (|| -> PyResult<Obj<'py>> {
                // shown = session.post(f"{url}/api/show", json={"model": name}, timeout=(0.5, timeout))
                let post = session.getattr("post")?;
                let address = Fmt::new(py).v(&url)?.s("/api/show").done()?;
                let kw = PyDict::new(py);
                kw.set_item("json", dict(py, &[("model", &name)])?)?;
                kw.set_item("timeout", pair(float(py, 0.5), timeout.clone())?)?;
                let shown = post.call((address,), Some(&kw))?;
                // caps = shown.json().get("capabilities") if shown.ok else None
                if shown.getattr("ok")?.is_truthy()? {
                    get(&shown.call_method0("json")?, "capabilities")
                } else {
                    Ok(none(py))
                }
            })();
            let caps = match tried {
                Ok(caps) => caps,
                Err(e) => {
                    // except (requests.exceptions.RequestException, ValueError, AttributeError): caps = None
                    let _handling = Handling::new(py, &e);
                    if !matches(py, &e, || request_value_attribute(py))? {
                        return Err(e);
                    }
                    none(py)
                }
            };
            // caps = caps if isinstance(caps, list) else None
            let caps = if isinstance(&caps, "list")? { caps } else { none(py) };
            // An Ollama too old to say what a model can do lists it anyway.
            if caps.is_none() || caps.contains("tools")? {
                models.append(pair(name, caps)?)?;
            }
        }
        Ok(models.into_any())
    })?;
    // None when __exit__ swallowed an error: the function ran off its end.
    Ok(done.unwrap_or_else(|| none(py)))
}

// ---------------------------------------------------------------------------
// OllamaProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// Ollama provider: a model on this machine, or the Ollama at OLLAMA_HOST
// (its docstring is CLASS_DOC, set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "OllamaProvider")]
pub struct OllamaProvider;

#[pymethods]
impl OllamaProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key=None, cli_agent=False, model_info=None, tools=None, max_tokens=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        OllamaProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "model_info", "tools", "max_tokens"];
        let slots = bind("OllamaProvider.__init__", 1, args, kwargs, &params, 0)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| false_(py));
        let model_info = next().unwrap_or_else(|| none(py));
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));

        // A local Ollama takes no key. One is only sent when given (an Ollama
        // behind a key: ollama.com, a proxy), on every call.
        me.setattr("api_key", &api_key)?;
        me.setattr("max_tokens", &max_tokens)?;
        me.setattr("base_url", g(py, "base_url")?.call0()?)?;
        // self.api_url = f"{self.base_url}/v1/chat/completions"
        let api_url = Fmt::new(py).v(&attr(me, "base_url")?)?.s("/v1/chat/completions").done()?;
        me.setattr("api_url", api_url)?;
        // self.session = _session(self.base_url, api_key)
        let make = g(py, "_session")?;
        me.setattr("session", make.call1((attr(me, "base_url")?, &api_key))?)?;
        me.setattr("cli_agent", &cli_agent)?;
        me.setattr("model_info", or(model_info, || Ok(empty_dict(py)))?)?;
        // Native tool calling: OpenAI-format function tools, the model's
        // output contract. None only for mode="text" (plain prose).
        me.setattr("tools", or(tools, || Ok(none(py)))?)?;
        // Per model: what /api/show says (_show), whether its context window
        // was read (_check_context), and the error a window too small for
        // any step raised, so every later call fails as fast.
        me.setattr("_shown", empty_dict(py))?;
        me.setattr("_context_checked", g(py, "set")?.call0()?)?;
        me.setattr("_too_small", empty_dict(py))?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to Ollama"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("OllamaProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // if model in self._too_small: raise Exception(self._too_small[model])
        if attr(me, "_too_small")?.contains(&model)? {
            return Err(throw(py, "Exception", || attr(me, "_too_small")?.get_item(&model)));
        }
        // caps = self._show(model).get("capabilities")
        let caps = get(&attr(me, "_show")?.call1((&model,))?, "capabilities")?;
        if !caps.is_none() && attr(me, "tools")?.is_truthy()? && !caps.contains("tools")? {
            return Err(throw(py, "Exception", || {
                Fmt::new(py)
                    .v(&model)?
                    .s(" cannot call tools, and every AutoCua agent acts through them. `ollama show ")
                    .v(&model)?
                    .s("` lists what it can do; pick a model with \"tools\".")
                    .done()
            }));
        }
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && !caps.is_none() && !caps.contains("vision")? {
            return Err(throw(py, "Exception", || {
                Fmt::new(py)
                    .v(&model)?
                    .s(" has no vision, and this agent works from screenshots: computer use, browser and \
                        mobile use need a model with \"vision\" in `ollama show ")
                    .v(&model)?
                    .s("`. The coder works without it.")
                    .done()
            }));
        }

        // If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        // if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
        if shot.is_truthy()?
            && !attr(me, "cli_agent")?.is_truthy()?
            && g(py, "len")?.call1((&messages,))?.rich_compare(1, CompareOp::Gt)?.is_truthy()?
        {
            // user_message = messages[-1]["content"]
            let user_message = messages.get_item(-1)?.get_item("content")?;
            // [{"type": "text", "text": user_message}] + [{"type": "image_url",
            //  "image_url": {"url": f"data:image/jpeg;base64,{shot}"}}
            //  for shot in screenshots(annotated_screenshot_base64)]
            let text = list1(dict(py, &[("type", &lit(py, "text")), ("text", &user_message)])?)?;
            let images = PyList::empty(py);
            let screenshots = g(py, "screenshots")?;
            for one in comprehension(&screenshots.call1((&shot,))?)? {
                let one = one?;
                let url = Fmt::new(py).s("data:image/jpeg;base64,").v(&one)?.done()?;
                let image_url = dict(py, &[("url", &url)])?;
                images.append(dict(py, &[("type", &lit(py, "image_url")), ("image_url", &image_url)])?)?;
            }
            let content = text.add(&images)?;
            // messages[-1]["content"] = ...
            messages.get_item(-1)?.set_item("content", content)?;
        }

        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default. Thinking counts against it.
        // "max_tokens": self.max_tokens or out_cap(self.cli_agent, 10000, self.model_info)
        let max_tokens = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            let cli_agent = attr(me, "cli_agent")?;
            let info = attr(me, "model_info")?;
            out_cap.call1((cli_agent, 10000, info))
        })?;
        let data = dict(py, &[("model", &model), ("messages", &messages), ("max_tokens", &max_tokens)])?;
        // How hard the model thinks, from view.py's table; Ollama maps the
        // level onto the model's own. An entry with no level keeps its default.
        // effort = entry_value(self.model_info, "effort", self.cli_agent)
        let entry_value = g(py, "entry_value")?;
        let effort = entry_value.call1((attr(me, "model_info")?, "effort", attr(me, "cli_agent")?))?;
        if effort.is_truthy()? {
            data.set_item("reasoning_effort", &effort)?;
        }

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            data.set_item("tools", attr(me, "tools")?)?;
            // "required" - every turn must call at least one tool; a text-only
            // turn is never a valid step. Canonical rationale: see
            // openrouter/service.py. Ollama takes the field but does not
            // enforce it: a model can still answer in text, which the
            // agents' loops answer with a "no tool called" nudge (and stop
            // after three in a row).
            // data["tool_choice"] = entry_value(self.model_info, "tool_choice", self.cli_agent) or "required"
            let entry_value = g(py, "entry_value")?;
            let picked = entry_value.call1((attr(me, "model_info")?, "tool_choice", attr(me, "cli_agent")?))?;
            data.set_item("tool_choice", or(picked, || Ok(lit(py, "required")))?)?;
        }

        // connect, read = http_timeout(self.cli_agent)
        let (connect, read) = unpack2(&g(py, "http_timeout")?.call1((attr(me, "cli_agent")?,))?)?;
        let tried = (|| -> PyResult<Obj<'py>> {
            // response = self.session.post(self.api_url, json=data,
            //                              timeout=(connect, max(read, READ_TIMEOUT)))
            let post = attr(me, "session")?.getattr("post")?;
            let address = attr(me, "api_url")?;
            let max = g(py, "max")?;
            let bound = max.call1((&read, g(py, "READ_TIMEOUT")?))?;
            let kw = PyDict::new(py);
            kw.set_item("json", &data)?;
            kw.set_item("timeout", pair(connect.clone(), bound)?)?;
            let response = post.call((address,), Some(&kw))?;
            response.call_method0("raise_for_status")?;
            response.call_method0("json")
        })();
        let result = match tried {
            Ok(result) => result,
            Err(e) => {
                let _handling = Handling::new(py, &e);
                // except requests.exceptions.ConnectionError as e:
                if matches(py, &e, || request_error(py, "ConnectionError"))? {
                    let error = e.value(py).clone().into_any();
                    return Err(now(
                        py,
                        throw(py, "Exception", || {
                            Fmt::new(py)
                                .s("Could not reach Ollama at ")
                                .v(&attr(me, "base_url")?)?
                                .s(" (")
                                .v(&error)?
                                .s("). Start the Ollama app (or `ollama serve`), or set OLLAMA_HOST to where it runs.")
                                .done()
                        }),
                    ));
                }
                // except requests.exceptions.RequestException as e:
                if !matches(py, &e, || request_error(py, "RequestException"))? {
                    return Err(e);
                }
                let error = e.value(py).clone().into_any();
                let made = (|| -> PyResult<Obj<'py>> {
                    // error_msg = f"Ollama request failed: {str(e)}"
                    let text = g(py, "str")?.call1((&error,))?;
                    let mut error_msg = Fmt::new(py).s("Ollama request failed: ").v(&text)?.done()?;
                    if g(py, "hasattr")?.call1((&error, "response"))?.is_truthy()? && !error.getattr("response")?.is_none() {
                        // error_msg += f"\nResponse: {e.response.text}"
                        let body = error.getattr("response")?.getattr("text")?;
                        error_msg = error_msg.add(Fmt::new(py).s("\nResponse: ").v(&body)?.done()?)?;
                        if error.getattr("response")?.getattr("status_code")?.rich_compare(404, CompareOp::Eq)?.is_truthy()? {
                            let hint = Fmt::new(py)
                                .s("\nIs ")
                                .v(&model)?
                                .s(" pulled? `ollama list` shows the models here; `ollama pull ")
                                .v(&model)?
                                .s("` fetches it.")
                                .done()?;
                            error_msg = error_msg.add(hint)?;
                        }
                    }
                    Ok(error_msg)
                })();
                let error_msg = made.map_err(|raised| now(py, raised))?;
                return Err(now(py, throw(py, "Exception", || Ok(error_msg))));
            }
        };
        // After the answer, as Ollama has the model loaded only now. A window
        // too small for any step raises, so this answer, read from a cut
        // prompt, is never acted on.
        attr(me, "_check_context")?.call1((&model,))?;
        if attr(me, "tools")?.is_truthy()? {
            return g(py, "_normalize_tool_response")?.call1((&result,));
        }
        Ok(result)
    }

    #[pyo3(name = "_show")]
    #[doc = "What /api/show says of `model`: {\"capabilities\": [...] or None,\n\
\"runner\": \"mlx\", \"llamacpp\" or None}. Kept once Ollama answers; {}\n\
while it cannot be asked (not running, the model not pulled, a key\n\
refused), in which case the request itself reports what is wrong."]
    fn show<'py>(slf: &Bound<'py, Self>, model: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        if attr(me, "_shown")?.contains(&model)? {
            return attr(me, "_shown")?.get_item(&model);
        }
        let tried = (|| -> PyResult<Obj<'py>> {
            // r = self.session.post(f"{self.base_url}/api/show", json={"model": model}, timeout=(15, 60))
            let post = attr(me, "session")?.getattr("post")?;
            let address = Fmt::new(py).v(&attr(me, "base_url")?)?.s("/api/show").done()?;
            let kw = PyDict::new(py);
            kw.set_item("json", dict(py, &[("model", &model)])?)?;
            kw.set_item("timeout", pair(int(py, 15)?, int(py, 60)?)?)?;
            post.call((address,), Some(&kw))
        })();
        let r = match tried {
            Ok(r) => r,
            Err(e) => {
                // except requests.exceptions.RequestException: return {}
                let _handling = Handling::new(py, &e);
                if !matches(py, &e, || request_error(py, "RequestException"))? {
                    return Err(e);
                }
                return Ok(empty_dict(py));
            }
        };
        if !r.getattr("ok")?.is_truthy()? {
            return Ok(empty_dict(py));
        }
        let body = match r.call_method0("json") {
            Ok(body) => body,
            Err(e) => {
                // except ValueError: body = {}
                let _handling = Handling::new(py, &e);
                if !matches(py, &e, || g(py, "ValueError"))? {
                    return Err(e);
                }
                empty_dict(py)
            }
        };
        let body = if isinstance(&body, "dict")? { body } else { empty_dict(py) };
        let caps = get(&body, "capabilities")?;
        // details = body.get("details") if isinstance(body.get("details"), dict) else {}
        let details = if isinstance(&get(&body, "details")?, "dict")? { get(&body, "details")? } else { empty_dict(py) };
        // shown = {"capabilities": caps if isinstance(caps, list) else None,
        //          "runner": details.get("runner")}
        let kept = if isinstance(&caps, "list")? { caps } else { none(py) };
        let runner = get(&details, "runner")?;
        let shown = dict(py, &[("capabilities", &kept), ("runner", &runner)])?;
        attr(me, "_shown")?.set_item(&model, &shown)?;
        Ok(shown)
    }

    #[pyo3(name = "_check_context")]
    #[doc = "Once per model, after its first answer: read the context window\n\
Ollama gave it (/api/ps) and compare it with what a run needs (see\n\
COMPRESSION_HEADROOM). Too small for even a first step: raise, and\n\
every later call raises the same at once. Too small for a long run:\n\
say what to set, once, and go on."]
    fn check_context<'py>(slf: &Bound<'py, Self>, model: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        if attr(me, "_context_checked")?.contains(&model)? {
            return Ok(());
        }
        attr(me, "_context_checked")?.call_method1("add", (&model,))?;
        let tried = (|| -> PyResult<Obj<'py>> {
            // r = self.session.get(f"{self.base_url}/api/ps", timeout=(5, 10))
            let get_ps = attr(me, "session")?.getattr("get")?;
            let address = Fmt::new(py).v(&attr(me, "base_url")?)?.s("/api/ps").done()?;
            let kw = PyDict::new(py);
            kw.set_item("timeout", pair(int(py, 5)?, int(py, 10)?)?)?;
            let r = get_ps.call((address,), Some(&kw))?;
            // loaded = r.json().get("models") or []
            or(get(&r.call_method0("json")?, "models")?, || Ok(empty_list(py)))
        })();
        let loaded = match tried {
            Ok(loaded) => loaded,
            Err(e) => {
                // except (requests.exceptions.RequestException, ValueError, AttributeError): return
                let _handling = Handling::new(py, &e);
                if !matches(py, &e, || request_value_attribute(py))? {
                    return Err(e);
                }
                return Ok(());
            }
        };
        // names = (model, f"{model}:latest")
        let latest = Fmt::new(py).v(&model)?.s(":latest").done()?;
        let wanted = pair(model.clone(), latest)?.unbind();
        // size = next((m.get("context_length") for m in loaded
        //              if isinstance(m, dict) and (m.get("name") in names or m.get("model") in names)), None)
        let next = g(py, "next")?;
        let sizes = Genexpr::made(
            &loaded,
            CHECK_GENEXPR,
            body(move |m| {
                let names = wanted.bind(m.py());
                if !isinstance(m, "dict")? {
                    return Ok(None);
                }
                if !(names.contains(get(m, "name")?)? || names.contains(get(m, "model")?)?) {
                    return Ok(None);
                }
                Ok(Some(get(m, "context_length")?.unbind()))
            }),
        )?;
        let size = next.call1((sizes, none(py)))?;
        if !isinstance(&size, "int")? || size.rich_compare(0, CompareOp::Le)?.is_truthy()? {
            return Ok(());
        }
        // try: from AutoCua.memory_compression.controller import COMPRESS_THRESHOLD as threshold
        // except Exception: threshold = 110_000
        let threshold = match import_from(&module(py)?, "AutoCua.memory_compression.controller", &["COMPRESS_THRESHOLD"]) {
            Ok(mut found) => found.remove(0),
            Err(e) => except_exception(py, e, |_| int(py, 110_000))?,
        };
        // if size >= threshold + COMPRESSION_HEADROOM: return
        let needed = threshold.add(g(py, "COMPRESSION_HEADROOM")?)?;
        if size.rich_compare(needed, CompareOp::Ge)?.is_truthy()? {
            return Ok(());
        }
        // mlx = self._show(model).get("runner") == "mlx"
        let mlx = get(&attr(me, "_show")?.call1((&model,))?, "runner")?.rich_compare("mlx", CompareOp::Eq)?;
        // window = 1 << (threshold + COMPRESSION_HEADROOM - 1).bit_length()   # the next power of two
        let span = threshold.add(g(py, "COMPRESSION_HEADROOM")?)?.sub(1)?;
        let window = int(py, 1)?.lshift(span.call_method0("bit_length")?)?;
        // fits = size - COMPRESSION_HEADROOM      # the compression threshold this window holds
        let fits = size.sub(g(py, "COMPRESSION_HEADROOM")?)?;
        if size.rich_compare(g(py, "MIN_CONTEXT")?, CompareOp::Lt)?.is_truthy()? {
            let fix = if mlx.is_truthy()? {
                lit(py, "Pick a model with a larger window: on Ollama's MLX engine a model gets its own maximum.")
            } else {
                Fmt::new(py).s("Restart Ollama with OLLAMA_CONTEXT_LENGTH=").v(&window)?.s(".").done()?
            };
            let message = Fmt::new(py)
                .s("Ollama runs ")
                .v(&model)?
                .s(" with a ")
                .v(&size)?
                .s("-token context window, too small for even an agent's first step (its instructions and \
                    tools alone are about 16k tokens). ")
                .v(&fix)?
                .done()?;
            attr(me, "_too_small")?.set_item(&model, message)?;
            return Err(throw(py, "Exception", || attr(me, "_too_small")?.get_item(&model)));
        }
        let options = PyList::empty(py);
        if !mlx.is_truthy()? {
            options.append(Fmt::new(py).s("restart Ollama with OLLAMA_CONTEXT_LENGTH=").v(&window)?.done()?)?;
        }
        if fits.rich_compare(g(py, "MIN_CONTEXT")?, CompareOp::Ge)?.is_truthy()? {
            let line = Fmt::new(py).s("set AutoCua_COMPRESS_THRESHOLD=").v(&fits)?.s(" so compression starts in time").done()?;
            options.append(line)?;
        }
        // advice = (" To fix it, " + ", or ".join(options) + "." if options else
        //           " This model's window is too small for long runs; pick one with a larger window.")
        let advice = if options.is_truthy()? {
            let joined = lit(py, ", or ").call_method1("join", (&options,))?;
            lit(py, " To fix it, ").add(joined)?.add(".")?
        } else {
            lit(py, " This model's window is too small for long runs; pick one with a larger window.")
        };
        let print = g(py, "print")?;
        let line = Fmt::new(py)
            .s("\u{26a0}\u{fe0f} Ollama runs ")
            .v(&model)?
            .s(" with a ")
            .v(&size)?
            .s("-token context window, and AutoCua compresses a run's memory only at ")
            .v(&threshold)?
            .s(" tokens, so a long run fails once its prompt passes the window.")
            .v(&advice)?
            .done()?;
        let kw = PyDict::new(py);
        kw.set_item("flush", true)?;
        print.call((line,), Some(&kw))?;
        Ok(())
    }
}

#[pyfunction]
#[pyo3(name = "_normalize_tool_response")]
#[doc = "Normalize an OpenAI-format tool-call response: keep the usual\n\
choices/message shape but replace tool_calls with\n\
[{\"id\", \"name\", \"arguments\": dict}] (arguments JSON-decoded). The `id` is\n\
echoed back by the loop so each tool result matches its call."]
fn normalize_tool_response(result: Obj<'_>) -> PyResult<Obj<'_>> {
    let py = result.py();
    // message = ((result.get("choices") or [{}])[0].get("message")) or {}
    let choices = or(get(&result, "choices")?, || list1(empty_dict(py)))?;
    let message = or(get(&choices.get_item(0)?, "message")?, || Ok(empty_dict(py)))?;
    let calls = PyList::empty(py);
    // for i, tc in enumerate(message.get("tool_calls") or []):
    let enumerate = g(py, "enumerate")?;
    let listed = or(get(&message, "tool_calls")?, || Ok(empty_list(py)))?;
    for numbered in enumerate.call1((listed,))?.try_iter()? {
        let (i, tc) = unpack2(&numbered?)?;
        // fn = (tc or {}).get("function") or {}
        let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
        let raw_args = get(&call, "arguments")?;
        let args = if isinstance(&raw_args, "dict")? {
            raw_args
        } else {
            // try: args = json.loads(raw_args or "{}")
            // except Exception: args = {}
            match (|| g(py, "json")?.getattr("loads")?.call1((or(raw_args, || Ok(lit(py, "{}")))?,)))() {
                Ok(parsed) => parsed,
                Err(e) => except_exception(py, e, |_| Ok(empty_dict(py)))?,
            }
        };
        let args = if isinstance(&args, "dict")? { args } else { empty_dict(py) };
        // "id": str((tc or {}).get("id") or "") or f"call_{i}"
        let to_str = g(py, "str")?;
        let raw_id = or(get(&or(tc, || Ok(empty_dict(py)))?, "id")?, || Ok(lit(py, "")))?;
        let id = or(to_str.call1((raw_id,))?, || Fmt::new(py).s("call_").v(&i)?.done())?;
        // "name": fn.get("name") or ""
        let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
        calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &args)])?)?;
    }
    // "content": message.get("content") or ""
    let content = or(get(&message, "content")?, || Ok(lit(py, "")))?;
    let reply = dict(py, &[("content", &content), ("tool_calls", calls.as_any())])?;
    let choices = list1(dict(py, &[("message", &reply)])?)?;
    // "usage": result.get("usage", {})
    let usage = get_or(&result, "usage", empty_dict(py))?;
    dict(py, &[("choices", &choices), ("usage", &usage)])
}

// ---------------------------------------------------------------------------
// service.py's lines, run once the loader bound its imports
// ---------------------------------------------------------------------------

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // Read bound (seconds) for every call, the coder's included, instead of
    // LLM_HTTP_TIMEOUT's 180. The calls are non-streaming, so the socket is
    // silent until the whole answer is ready, and a model on this machine
    // reads a long prompt slowly (about 400 tokens a second for a 27B model
    // on an M5 Pro: the memory handoff's 110k-token transcript takes minutes
    // before the first output token), waits behind any other call to the
    // same model (Ollama runs them one at a time), and loads 20-odd GB from
    // disk after five idle minutes. Ollama drops the work when the client
    // gives up, so a retry starts it over from scratch.
    store(m, "READ_TIMEOUT", 900)?;
    // A run's prompt grows until memory compression starts (memory_compression/
    // controller.rs COMPRESS_THRESHOLD: 110k tokens unless
    // AutoCua_COMPRESS_THRESHOLD says otherwise), so the model's context
    // window has to hold that much plus COMPRESSION_HEADROOM: the handoff's
    // output and the steps taken while it runs. _check_context reads the
    // window off Ollama. A model on Ollama's MLX engine (Apple Silicon
    // builds, details.runner "mlx") runs with its own maximum window
    // (qwen3.8:27b: 262144); on the llama.cpp engine (GGUF) the window is
    // OLLAMA_CONTEXT_LENGTH, else a default Ollama sizes to the machine's
    // memory, and a longer prompt is cut from the START, the agent's
    // instructions first, without an error. Raising the window is therefore
    // advice for llama.cpp models only.
    store(m, "COMPRESSION_HEADROOM", 20000)?;
    // The smallest window even a run's first step fits in: the driver's
    // system prompt and tool definitions (about 16k tokens in quality mode),
    // a screenshot (about 2k) and the screen's element list (up to about
    // 7.5k). Below it the run stops (_check_context).
    store(m, "MIN_CONTEXT", 32768)?;
    store(m, "base_url", wrap_pyfunction!(base_url, m)?)?;
    store(m, "_split_host_port", wrap_pyfunction!(split_host_port, m)?)?;
    store(m, "_key", wrap_pyfunction!(key, m)?)?;
    store(m, "_session", wrap_pyfunction!(make_session, m)?)?;
    store(m, "reachable", wrap_pyfunction!(reachable, m)?)?;
    store(m, "pulled_models", wrap_pyfunction!(pulled_models, m)?)?;
    // class OllamaProvider:
    store(m, "OllamaProvider", py.get_type::<OllamaProvider>())?;
    store(m, "_normalize_tool_response", wrap_pyfunction!(normalize_tool_response, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with _build, which the
/// loader calls once it has bound the import lines' names (see the top of
/// this file), and makes the class look like the one service.py made: its
/// docstring and, from Python 3.13, what the class statement gave it.
/// service.py had no docstring, so neither has the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<OllamaProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
