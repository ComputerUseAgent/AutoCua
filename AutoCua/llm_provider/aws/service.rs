//! service: the AWS provider (AWSProvider), which translates the canonical
//! transcript into Amazon Bedrock's Converse format, sends one request and
//! folds the reply back into the OpenAI-shaped envelope the loops read.
//!
//! The Rust copy of llm_provider/aws/service.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.aws.service
//! (llm_provider/aws/__init__.py points that name here), and finds the same
//! names in the same order, with the same values, return values and errors.
//! Python's own rules decide every result, so they are called, not
//! imitated: json, re, quote, the builtins, the str, dict, list and set
//! methods, the operators, the comparisons, the slices and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (json, os, re, Dict, Any, Optional,
//! quote, requests, the view's DEFAULT_MAX_TOKENS, DEFAULT_REGION,
//! find_entry and get_request_fields, entry_value, http_timeout, out_cap,
//! screenshots) and calls _build(), which runs the rest of service.py's
//! lines in their order: the two constants, the eight functions and the
//! class.
//!
//! Every global a line reads (json, os, re, quote, requests, find_entry,
//! _CACHE_POINT, _as_text, _bedrock_token, set, sorted, isinstance, str, ...)
//! is read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces one is obeyed. The view's names are this
//! module's own, bound from view when it was imported. The provider keeps
//! its attributes in its __dict__, and its methods reach them through the
//! instance, so a subclass or a patched attribute (the session) is obeyed.
//! An error is raised the way Python's raise statement raises it, so the
//! error being handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PySet, PySlice, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// AWSProvider's docstring.
const CLASS_DOC: &str = "Amazon Bedrock provider (Converse API) for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 118;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 8] =
    ["api_base", "api_key", "cli_agent", "max_tokens", "model_info", "region", "session", "tools"];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.aws.service", super::super::NATIVE)).map(Bound::unbind))
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

/// A str constant.
fn lit<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
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

/// The slice `[start:stop]` makes: slice(start, stop, None), None where a
/// bound is left out.
fn slice<'py>(py: Python<'py>, start: Obj<'py>, stop: Obj<'py>) -> PyResult<Obj<'py>> {
    py.get_type::<PySlice>().call1((start, stop, py.None()))
}

/// `"\n".join(p for p in parts if p)`: the true items of `parts`, joined by
/// str.join (which takes the whole generator before it joins).
fn join_true<'py>(parts: &Bound<'py, PyList>) -> PyResult<Obj<'py>> {
    let py = parts.py();
    let kept = PyList::empty(py);
    for part in parts.iter() {
        if part.is_truthy()? {
            kept.append(part)?;
        }
    }
    lit(py, "\n").call_method1("join", (kept,))
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

// ---------------------------------------------------------------------------
// The generator expressions
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

// The generator expressions handed to any() and str.join, run as Python runs
// one (as responses.rs runs its own). Made holding iter() of what it loops
// over, on Python 3.13 its first step takes iter() of that again, as that
// version's generator code does; then it runs its body one value per
// next(). A StopIteration from its own body comes out as RuntimeError (PEP
// 479), and once it has ended or failed it gives nothing more. A plain
// comment: a doc comment would give the class a docstring, and a generator
// has none.
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

/// The qualname of _cache_marked's generator expression.
const CACHE_GENEXPR: &str = "_cache_marked.<locals>.<genexpr>";

/// The qualname of send_request's generator expression.
const SEND_GENEXPR: &str = "AWSProvider.send_request.<locals>.<genexpr>";

// ---------------------------------------------------------------------------
// The functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_as_text")]
#[doc = "Flatten a message content field (string, or list of content blocks) to\n\
plain text \u{2014} used when translating the canonical OpenAI-shaped transcript\n\
into Converse content blocks."]
fn as_text<'py>(content: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = content.py();
    if isinstance(&content, "str")? {
        return Ok(content);
    }
    if isinstance(&content, "list")? {
        // parts = [b.get("text", "") for b in content
        //          if isinstance(b, dict) and b.get("type") == "text"]
        let parts = PyList::empty(py);
        for b in comprehension(&content)? {
            let b = b?;
            if isinstance(&b, "dict")? && eq(&get(&b, "type")?, "text")? {
                parts.append(get_or(&b, "text", lit(py, ""))?)?;
            }
        }
        return join_true(&parts);
    }
    if content.is_none() {
        return Ok(lit(py, ""));
    }
    g(py, "str")?.call1((content,))
}

#[pyfunction]
#[pyo3(name = "_cache_marked")]
#[doc = "True when the agent loop marked this turn as its cache breakpoint: a\n\
parts array carrying cache_control (see the agents' prompt-caching step)."]
fn cache_marked<'py>(content: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = content.py();
    // return isinstance(content, list) and any(
    //     isinstance(p, dict) and p.get("cache_control") for p in content)
    let check = isinstance_value(&content, "list")?;
    if !check.is_truthy()? {
        return Ok(check);
    }
    let any = g(py, "any")?;
    let marks = Genexpr::made(
        &content,
        CACHE_GENEXPR,
        body(|p| {
            let check = isinstance_value(p, "dict")?;
            if !check.is_truthy()? {
                return Ok(Some(check.unbind()));
            }
            Ok(Some(get(p, "cache_control")?.unbind()))
        }),
    )?;
    any.call1((marks,))
}

// No docstring, as _args_dict had none.
#[pyfunction]
#[pyo3(name = "_args_dict")]
fn args_dict<'py>(raw: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = raw.py();
    if isinstance(&raw, "dict")? {
        return Ok(raw);
    }
    // try: parsed = json.loads(raw or "{}")
    // except Exception: return {}
    let parsed = match (|| g(py, "json")?.getattr("loads")?.call1((or(raw, || Ok(lit(py, "{}")))?,)))() {
        Ok(parsed) => parsed,
        Err(e) => return except_exception(py, e, |_| Ok(empty_dict(py))),
    };
    if isinstance(&parsed, "dict")? {
        Ok(parsed)
    } else {
        Ok(empty_dict(py))
    }
}

#[pyfunction]
#[pyo3(name = "_bedrock_token")]
#[doc = "A tool call id or name Bedrock accepts. Bedrock checks every name in the\n\
history against [a-zA-Z0-9_-]+ on every model (a replayed rejected call\n\
named \"(unnamed)\" is a 400 for the whole request); Claude checks ids the\n\
same way (a Kimi-style `functions.click:0` is a 400 there), and ids are\n\
capped at 64 characters. A call and its result go through the same\n\
mapping, so the pair stays matched."]
fn bedrock_token<'py>(raw: Obj<'py>, empty: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = raw.py();
    // return re.sub(r"[^a-zA-Z0-9_-]", "_", str(raw or ""))[:64] or empty
    let sub = g(py, "re")?.getattr("sub")?;
    let to_str = g(py, "str")?;
    let text = to_str.call1((or(raw, || Ok(lit(py, "")))?,))?;
    let cleaned = sub.call1((lit(py, "[^a-zA-Z0-9_-]"), lit(py, "_"), text))?;
    let cut = cleaned.get_item(slice(py, none(py), 64i32.into_pyobject(py)?.into_any())?)?;
    or(cut, || Ok(empty))
}

// No docstring, as _tool_use_id had none.
#[pyfunction]
#[pyo3(name = "_tool_use_id")]
fn tool_use_id<'py>(raw: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = raw.py();
    g(py, "_bedrock_token")?.call1((raw, "call"))
}

#[pyfunction]
#[pyo3(name = "_pair_tool_results")]
#[doc = "Send a tool result that answers no call of the turn before it as plain\n\
text. Bedrock rejects the whole request over one (\"The number of\n\
toolResult blocks ... exceeds the number of toolUse blocks of previous\n\
turn\"), and the transcript keeps the step, so every later request of the\n\
run would fail too. The minion's leaked-envelope salvage can write one."]
fn pair_tool_results(api_messages: Obj<'_>) -> PyResult<()> {
    let py = api_messages.py();
    let mut open_ids = g(py, "set")?.call0()?;
    for msg in api_messages.try_iter()? {
        let msg = msg?;
        if eq(&msg.get_item("role")?, "assistant")? {
            // open_ids = {b["toolUse"]["toolUseId"] for b in msg["content"] if "toolUse" in b}
            let ids = PySet::empty(py)?;
            for b in comprehension(&msg.get_item("content")?)? {
                let b = b?;
                if b.contains("toolUse")? {
                    ids.add(b.get_item("toolUse")?.get_item("toolUseId")?)?;
                }
            }
            open_ids = ids.into_any();
            continue;
        }
        let paired = PyList::empty(py);
        let orphans = PyList::empty(py);
        let rest = PyList::empty(py);
        for block in msg.get_item("content")?.try_iter()? {
            let block = block?;
            let result = get(&block, "toolResult")?;
            if result.is_none() {
                rest.append(&block)?;
            } else if open_ids.contains(result.get_item("toolUseId")?)? {
                let discard = open_ids.getattr("discard")?;
                discard.call1((result.get_item("toolUseId")?,))?;
                paired.append(&block)?;
            } else {
                // {"text": f"Result of tool call {result['toolUseId']}: "
                //          f"{result['content'][0]['text']}"}
                let line = Fmt::new(py).s("Result of tool call ").v(&result.get_item("toolUseId")?)?.s(": ");
                let text = result.get_item("content")?.get_item(0)?.get_item("text")?;
                let text = line.v(&text)?.done()?;
                orphans.append(dict(py, &[("text", &text)])?)?;
            }
        }
        if orphans.is_truthy()? {
            // Results first, as Bedrock requires; everything else after them.
            let joined = paired.as_any().add(&orphans)?.add(&rest)?;
            msg.set_item("content", joined)?;
        }
        open_ids = g(py, "set")?.call0()?;
    }
    Ok(())
}

#[pyfunction]
#[pyo3(name = "_tool_spec")]
#[doc = "One chat-completions function tool as a Converse toolSpec."]
fn tool_spec<'py>(tool: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = tool.py();
    // fn = tool.get("function") or tool
    let call = or(get(&tool, "function")?, || Ok(tool.clone()))?;
    let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
    let parameters = or(get(&call, "parameters")?, || {
        dict(py, &[("type", &lit(py, "object")), ("properties", &empty_dict(py))])
    })?;
    let schema = dict(py, &[("json", &parameters)])?;
    let spec = dict(py, &[("name", &name), ("inputSchema", &schema)])?;
    if get(&call, "description")?.is_truthy()? {
        spec.set_item("description", call.get_item("description")?)?;
    }
    dict(py, &[("toolSpec", &spec)])
}

#[pyfunction]
#[pyo3(name = "_usage")]
#[doc = "Bedrock usage in the Anthropic key style the manager reads.\n\
\n\
inputTokens leaves the cached tokens out, as Anthropic's input_tokens does\n\
(measured: a 12777-token cache read came back as inputTokens 25), so the\n\
manager's context size adds the two cache classes back."]
fn usage<'py>(u: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = u.py();
    let u = or(u, || Ok(empty_dict(py)))?;
    let zero = || -> PyResult<Obj<'py>> { Ok(0i32.into_pyobject(py)?.into_any()) };
    // u.get(<key>, 0) or 0
    let count = |key: &str| -> PyResult<Obj<'py>> { or(get_or(&u, key, zero()?)?, zero) };
    let input = count("inputTokens")?;
    let output = count("outputTokens")?;
    let total = count("totalTokens")?;
    let read = count("cacheReadInputTokens")?;
    let write = count("cacheWriteInputTokens")?;
    dict(
        py,
        &[
            ("input_tokens", &input),
            ("output_tokens", &output),
            ("total_tokens", &total),
            ("cache_read_input_tokens", &read),
            ("cache_creation_input_tokens", &write),
        ],
    )
}

// ---------------------------------------------------------------------------
// AWSProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// Amazon Bedrock provider (Converse API) for LLM interactions (its docstring
// is CLASS_DOC, set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "AWSProvider")]
pub struct AWSProvider;

#[pymethods]
impl AWSProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, model_info=None, tools=None, max_tokens=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AWSProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "model_info", "tools", "max_tokens"];
        let slots = bind("AWSProvider.__init__", 1, args, kwargs, &params, 1)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
        let model_info = next().unwrap_or_else(|| none(py));
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));

        me.setattr("api_key", &api_key)?;
        me.setattr("cli_agent", &cli_agent)?;
        me.setattr("model_info", or(model_info, || Ok(empty_dict(py)))?)?;
        me.setattr("max_tokens", &max_tokens)?;
        // The regional endpoint every call goes through (see view.py).
        // self.region = (os.getenv("AWS_BEDROCK_REGION") or "").strip().lower() or DEFAULT_REGION
        let getenv = g(py, "os")?.getattr("getenv")?;
        let raw = or(getenv.call1(("AWS_BEDROCK_REGION",))?, || Ok(lit(py, "")))?;
        let region = or(raw.call_method0("strip")?.call_method0("lower")?, || g(py, "DEFAULT_REGION"))?;
        me.setattr("region", region)?;
        let api_base = Fmt::new(py).s("https://bedrock-runtime.").v(&attr(me, "region")?)?.s(".amazonaws.com").done()?;
        me.setattr("api_base", api_base)?;
        me.setattr("session", g(py, "requests")?.getattr("Session")?.call0()?)?;
        // Native tool calling: chat-completions function tools — the format
        // the manager hands every provider without a dialect of its own, and
        // the one the browser agent's Rust manager sends. They become Converse
        // toolSpecs per request, because show_screen_tools swaps this list
        // between calls. None only for mode="text" (plain prose).
        me.setattr("tools", or(tools, || Ok(none(py)))?)?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to the Bedrock Converse API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("AWSProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // The table entry for the model on the wire, looked up by api_name, so
        // the settings follow the model actually called (the manager's one-call
        // fallback swaps it) and also apply when the model was given by its
        // Bedrock id rather than its short name. An unregistered ID gets none.
        // info = find_entry(model) or (self.model_info if self.model_info.get("api_name") == model else {})
        let found = g(py, "find_entry")?.call1((&model,))?;
        let info = or(found, || {
            let wanted = attr(me, "model_info")?.call_method1("get", ("api_name",))?;
            if wanted.rich_compare(&model, CompareOp::Eq)?.is_truthy()? {
                attr(me, "model_info")
            } else {
                Ok(empty_dict(py))
            }
        })?;
        // cache = bool(info.get("cache"))
        let to_bool = g(py, "bool")?;
        let cache = to_bool.call1((get(&info, "cache")?,))?;

        // NATIVE TRANSCRIPT translation. The agents speak the canonical OpenAI
        // shape (assistant messages carrying `tool_calls`, plus `role: "tool"`
        // results keyed by tool_call_id). Converse carries the same as content
        // blocks: toolUse on the assistant turn, toolResult on a following
        // user turn. A blank text block is a 400 on every model, so empty text
        // is left out and a turn left with nothing gets a placeholder.
        let system = PyList::empty(py);
        let api_messages = PyList::empty(py);
        let marked = g(py, "set")?.call0()?; // api_messages indexes of the turns the loop marked for caching
        for msg in messages.try_iter()? {
            let msg = msg?;
            let role = get(&msg, "role")?;
            let content = get(&msg, "content")?;
            if eq(&role, "system")? {
                let text = g(py, "_as_text")?.call1((&content,))?;
                if text.call_method0("strip")?.is_truthy()? {
                    system.append(dict(py, &[("text", &text)])?)?;
                }
                continue;
            }

            if eq(&role, "tool")? {
                // result = {
                //     "toolUseId": _tool_use_id(msg.get("tool_call_id")),
                //     "content": [{"text": _as_text(content) or "(no output)"}],
                // }
                let tool_use_id = g(py, "_tool_use_id")?;
                let id = tool_use_id.call1((get(&msg, "tool_call_id")?,))?;
                let as_text = g(py, "_as_text")?;
                let text = or(as_text.call1((&content,))?, || Ok(lit(py, "(no output)")))?;
                let parts = list1(dict(py, &[("text", &text)])?)?;
                let result = dict(py, &[("toolUseId", &id), ("content", &parts)])?;
                if get(&msg, "is_error")?.is_truthy()? {
                    result.set_item("status", "error")?;
                }
                // Results for a batch of calls must ride on ONE user turn.
                // prev = api_messages[-1] if api_messages else None
                let prev = if api_messages.is_truthy()? { Some(api_messages.as_any().get_item(-1)?) } else { None };
                // if prev and prev["role"] == "user" and "toolResult" in prev["content"][0]:
                let merge = match &prev {
                    None => false,
                    Some(prev) => {
                        prev.is_truthy()?
                            && eq(&prev.get_item("role")?, "user")?
                            && prev.get_item("content")?.get_item(0)?.contains("toolResult")?
                    }
                };
                match prev {
                    Some(prev) if merge => {
                        let append = prev.get_item("content")?.getattr("append")?;
                        append.call1((dict(py, &[("toolResult", &result)])?,))?;
                    }
                    _ => {
                        let content = list1(dict(py, &[("toolResult", &result)])?)?;
                        api_messages.append(dict(py, &[("role", &lit(py, "user")), ("content", &content)])?)?;
                    }
                }
            } else if eq(&role, "assistant")? {
                let blocks = PyList::empty(py);
                let text = g(py, "_as_text")?.call1((&content,))?;
                if text.call_method0("strip")?.is_truthy()? {
                    blocks.append(dict(py, &[("text", &text)])?)?;
                }
                // for tc in msg.get("tool_calls") or []:
                let calls = or(get(&msg, "tool_calls")?, || Ok(PyList::empty(py).into_any()))?;
                for tc in calls.try_iter()? {
                    let tc = tc?;
                    // fn = (tc or {}).get("function") or {}
                    let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
                    // {"toolUse": {
                    //     "toolUseId": _tool_use_id((tc or {}).get("id")),
                    //     "name": _bedrock_token(fn.get("name"), "unnamed"),
                    //     "input": _args_dict(fn.get("arguments")),
                    // }}
                    let tool_use_id = g(py, "_tool_use_id")?;
                    let id = tool_use_id.call1((get(&or(tc, || Ok(empty_dict(py)))?, "id")?,))?;
                    let bedrock_token = g(py, "_bedrock_token")?;
                    let name = bedrock_token.call1((get(&call, "name")?, "unnamed"))?;
                    let args_dict = g(py, "_args_dict")?;
                    let input = args_dict.call1((get(&call, "arguments")?,))?;
                    let call_use = dict(py, &[("toolUseId", &id), ("name", &name), ("input", &input)])?;
                    blocks.append(dict(py, &[("toolUse", &call_use)])?)?;
                }
                // "content": blocks or [{"text": "(no content)"}]
                let content = or(blocks.into_any(), || list1(dict(py, &[("text", &lit(py, "(no content)"))])?))?;
                api_messages.append(dict(py, &[("role", &lit(py, "assistant")), ("content", &content)])?)?;
            } else {
                let text = g(py, "_as_text")?.call1((&content,))?;
                // {"text": text if text.strip() else "(no content)"}
                let shown = if text.call_method0("strip")?.is_truthy()? { text } else { lit(py, "(no content)") };
                let parts = list1(dict(py, &[("text", &shown)])?)?;
                api_messages.append(dict(py, &[("role", &lit(py, "user")), ("content", &parts)])?)?;
            }

            // if role in ("tool", "user") and _cache_marked(content):
            if names(py, &["tool", "user"])?.as_any().contains(&role)? && g(py, "_cache_marked")?.call1((&content,))?.is_truthy()? {
                let add = marked.getattr("add")?;
                let index = g(py, "len")?.call1((&api_messages,))?.sub(1)?;
                add.call1((index,))?;
            }
        }
        g(py, "_pair_tool_results")?.call1((&api_messages,))?;

        // Prompt caching (Claude): a checkpoint after the system prompt, which
        // also covers the tools ahead of it, and one at the end of each turn the
        // loop marked — the newest ones only, within Claude's limit of 4.
        if cache.is_truthy()? {
            // for i in sorted(marked)[-_MAX_MESSAGE_CACHE_POINTS:]:
            let ordered = g(py, "sorted")?.call1((&marked,))?;
            let newest = g(py, "_MAX_MESSAGE_CACHE_POINTS")?.neg()?;
            for i in ordered.get_item(slice(py, newest, none(py))?)?.try_iter()? {
                let i = i?;
                let append = api_messages.as_any().get_item(&i)?.get_item("content")?.getattr("append")?;
                let point = g(py, "dict")?.call1((g(py, "_CACHE_POINT")?,))?;
                append.call1((point,))?;
            }
            if system.is_truthy()? {
                let point = g(py, "dict")?.call1((g(py, "_CACHE_POINT")?,))?;
                system.append(point)?;
            }
        }

        // If screenshot is provided and NOT cli_agent, add it to the live user
        // message — after any checkpoint: it changes every step, so caching it
        // would write a prefix that is never read back. Skipped when the
        // transcript ends on tool results; the driver always ends on its live
        // user message anyway.
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && api_messages.is_truthy()? {
            let last = api_messages.as_any().get_item(-1)?;
            if eq(&last.get_item("role")?, "user")? && !last.get_item("content")?.get_item(0)?.contains("toolResult")? {
                for one in g(py, "screenshots")?.call1((&shot,))?.try_iter()? {
                    let one = one?;
                    let append = last.get_item("content")?.getattr("append")?;
                    let source = dict(py, &[("bytes", &one)])?;
                    // image/jpeg: LLM_IMAGE_FORMAT in tree/element.py
                    let image = dict(py, &[("format", &lit(py, "jpeg")), ("source", &source)])?;
                    append.call1((dict(py, &[("image", &image)])?,))?;
                }
            }
        }

        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps the model's cap from view.py.
        // "maxTokens": self.max_tokens or out_cap(self.cli_agent, info.get("max_tokens") or DEFAULT_MAX_TOKENS, info)
        let max_tokens = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            let cli_agent = attr(me, "cli_agent")?;
            let cap = or(get(&info, "max_tokens")?, || g(py, "DEFAULT_MAX_TOKENS"))?;
            out_cap.call1((cli_agent, cap, &info))
        })?;
        let config = dict(py, &[("maxTokens", &max_tokens)])?;
        // body = {"messages": api_messages, "inferenceConfig": {"maxTokens": ...}}
        let request = dict(py, &[("messages", api_messages.as_any()), ("inferenceConfig", &config)])?;
        if system.is_truthy()? {
            request.set_item("system", &system)?;
        }

        // Claude's effort or Kimi/Grok's reasoning level, per view.py.
        let fields = g(py, "get_request_fields")?.call1((&info, attr(me, "cli_agent")?))?;
        if fields.is_truthy()? {
            request.set_item("additionalModelRequestFields", &fields)?;
        }

        // No tools (mode="text") -> plain text: omit the tool config entirely.
        if attr(me, "tools")?.is_truthy()? {
            // {"any": {}} - every turn must call at least one tool; a text-only
            // turn is never a valid step (even termination is the `exit`
            // tool). Canonical rationale: see openrouter/service.py. The
            // manager may override it per line (the scrape optimizer runs on
            // "auto": its only tool is `more` and its answer is the text reply).
            let entry_value = g(py, "entry_value")?;
            let picked = entry_value.call1((attr(me, "model_info")?, "tool_choice", attr(me, "cli_agent")?))?;
            let choice = if eq(&picked, "auto")? { lit(py, "auto") } else { lit(py, "any") };
            // "tools": [_tool_spec(t) for t in self.tools]
            let specs = PyList::empty(py);
            for t in comprehension(&attr(me, "tools")?)? {
                let t = t?;
                specs.append(g(py, "_tool_spec")?.call1((t,))?)?;
            }
            let choice_map = PyDict::new(py);
            choice_map.set_item(&choice, empty_dict(py))?;
            let tool_config = dict(py, &[("tools", specs.as_any()), ("toolChoice", choice_map.as_any())])?;
            request.set_item("toolConfig", tool_config)?;
        }

        let auth = Fmt::new(py).s("Bearer ").v(&attr(me, "api_key")?)?.done()?;
        let headers = dict(py, &[("Authorization", &auth), ("Content-Type", &lit(py, "application/json"))])?;
        // url = f"{self.api_base}/model/{quote(model, safe='')}/converse"
        let line = Fmt::new(py).v(&attr(me, "api_base")?)?.s("/model/");
        let quote = g(py, "quote")?;
        let safe = PyDict::new(py);
        safe.set_item("safe", "")?;
        let quoted = quote.call((&model,), Some(&safe))?;
        let url = line.v(&quoted)?.s("/converse").done()?;

        let tried = (|| -> PyResult<Obj<'py>> {
            let post = attr(me, "session")?.getattr("post")?;
            let kw = PyDict::new(py);
            kw.set_item("json", &request)?;
            kw.set_item("headers", &headers)?;
            let timeout = g(py, "http_timeout")?.call1((attr(me, "cli_agent")?,))?;
            kw.set_item("timeout", timeout)?;
            let response = post.call((&url,), Some(&kw))?;
            response.call_method0("raise_for_status")?;
            response.call_method0("json")
        })();
        let result = match tried {
            Ok(result) => result,
            Err(e) => {
                // except requests.exceptions.RequestException as e:
                let _handling = Handling::new(py, &e);
                let kinds = (|| g(py, "requests")?.getattr("exceptions")?.getattr("RequestException"))()
                    .map_err(|raised| now(py, raised))?;
                if !caught(py, &e, &kinds)? {
                    return Err(e);
                }
                let error = e.value(py).clone().into_any();
                let made = (|| -> PyResult<Obj<'py>> {
                    let text = g(py, "str")?.call1((&error,))?;
                    let mut error_msg = Fmt::new(py).s("AWS Bedrock API request failed: ").v(&text)?.done()?;
                    if g(py, "hasattr")?.call1((&error, "response"))?.is_truthy()? && !error.getattr("response")?.is_none() {
                        let text = error.getattr("response")?.getattr("text")?;
                        error_msg = error_msg.add(Fmt::new(py).s("\nResponse Body: ").v(&text)?.done()?)?;
                    }
                    Ok(error_msg)
                })();
                let error_msg = made.map_err(|raised| now(py, raised))?;
                return Err(now(py, throw(py, "Exception", || Ok(error_msg))));
            }
        };

        // Normalize to the OpenAI-style shape the manager reads
        // (choices[0].message). Kimi K3 and Grok 4.7 also return their
        // reasoning as reasoningContent blocks; those are neither text nor a
        // call and are dropped, as the other providers drop their thinking.
        // blocks = ((result.get("output") or {}).get("message") or {}).get("content") or []
        let output = or(get(&result, "output")?, || Ok(empty_dict(py)))?;
        let reply = or(get(&output, "message")?, || Ok(empty_dict(py)))?;
        let blocks = or(get(&reply, "content")?, || Ok(PyList::empty(py).into_any()))?;
        // "\n".join(b["text"] for b in blocks if isinstance(b, dict) and b.get("text"))
        let texts = Genexpr::made(
            &blocks,
            SEND_GENEXPR,
            body(|b| {
                if !isinstance(b, "dict")? || !get(b, "text")?.is_truthy()? {
                    return Ok(None);
                }
                Ok(Some(b.get_item("text")?.unbind()))
            }),
        )?;
        let content = lit(py, "\n").call_method1("join", (texts,))?;
        let message = dict(py, &[("content", &content)])?;
        if attr(me, "tools")?.is_truthy()? {
            let calls = PyList::empty(py);
            let enumerate = g(py, "enumerate")?;
            for pair in enumerate.call1((&blocks,))?.try_iter()? {
                let (i, block) = unpack2(&pair?)?;
                // use = block.get("toolUse") if isinstance(block, dict) else None
                let call_use = if isinstance(&block, "dict")? { get(&block, "toolUse")? } else { none(py) };
                if call_use.is_truthy()? {
                    // "id": str(use.get("toolUseId") or "") or f"call_{i}"
                    let to_str = g(py, "str")?;
                    let id = or(to_str.call1((or(get(&call_use, "toolUseId")?, || Ok(lit(py, "")))?,))?, || {
                        Fmt::new(py).s("call_").v(&i)?.done()
                    })?;
                    let name = or(get(&call_use, "name")?, || Ok(lit(py, "")))?;
                    let args_dict = g(py, "_args_dict")?;
                    let arguments = args_dict.call1((get(&call_use, "input")?,))?;
                    calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &arguments)])?)?;
                }
            }
            message.set_item("tool_calls", calls)?;
        }
        let choices = list1(dict(py, &[("message", &message)])?)?;
        let usage = g(py, "_usage")?.call1((get(&result, "usage")?,))?;
        dict(py, &[("choices", &choices), ("usage", &usage)])
    }
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
    // One cache checkpoint: Bedrock caches everything in the request before it.
    let kind = dict(py, &[("type", &lit(py, "default"))])?;
    store(m, "_CACHE_POINT", dict(py, &[("cachePoint", &kind)])?)?;
    // Claude takes at most 4 checkpoints per request, and the system prompt's is one.
    store(m, "_MAX_MESSAGE_CACHE_POINTS", 3)?;
    store(m, "_as_text", wrap_pyfunction!(as_text, m)?)?;
    store(m, "_cache_marked", wrap_pyfunction!(cache_marked, m)?)?;
    store(m, "_args_dict", wrap_pyfunction!(args_dict, m)?)?;
    store(m, "_bedrock_token", wrap_pyfunction!(bedrock_token, m)?)?;
    store(m, "_tool_use_id", wrap_pyfunction!(tool_use_id, m)?)?;
    store(m, "_pair_tool_results", wrap_pyfunction!(pair_tool_results, m)?)?;
    store(m, "_tool_spec", wrap_pyfunction!(tool_spec, m)?)?;
    store(m, "_usage", wrap_pyfunction!(usage, m)?)?;
    // class AWSProvider:
    store(m, "AWSProvider", py.get_type::<AWSProvider>())?;
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
    let class = py.get_type::<AWSProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
