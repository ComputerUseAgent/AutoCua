//! service: the Anthropic provider (AnthropicProvider), which translates the
//! canonical transcript into Anthropic's Messages format, sends one request
//! and folds the reply back into the OpenAI-shaped envelope the loops read.
//!
//! The Rust copy of llm_provider/anthropic/service.py. Python imports it
//! under its old dotted name, AutoCua.llm_provider.anthropic.service
//! (llm_provider/anthropic/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values, errors
//! and printed lines. Python's own rules decide every result, so they are
//! called, not imitated: json, copy, the builtins, the str, dict and list
//! methods, the operators, the comparisons and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (json, requests, copy, Dict, Any,
//! Optional, the four view functions, RefusalError, entry_value,
//! http_timeout, out_cap, screenshots) and calls _build(), which runs the
//! rest of service.py's lines in their order: the two functions and the
//! class.
//!
//! Every global a line reads (json, requests, copy, print, _as_text,
//! get_tool_choice, RefusalError, AnthropicProvider, isinstance, str, any,
//! ...) is read from this module at that moment, as Python reads a global, so
//! a caller or a test that replaces one is obeyed. The view functions are
//! this module's own names, bound from view when it was imported. The
//! provider keeps its attributes in its __dict__, and its methods reach them
//! through the instance, so a subclass or a patched attribute (the session)
//! is obeyed. An error is raised the way Python's raise statement raises it,
//! so the error being handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PySet, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// AnthropicProvider's docstring.
const CLASS_DOC: &str = "Anthropic API provider for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 32;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 7] = ["api_key", "api_url", "cli_agent", "max_tokens", "model_info", "session", "tools"];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.anthropic.service", super::super::NATIVE)).map(Bound::unbind))
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

/// What a list comprehension loops over: iter() of `source`, then on Python
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

    /// A {value!r} part: repr(value), then formatted.
    fn r(self, value: &Obj<'py>) -> PyResult<Self> {
        let shown = value.repr()?.into_any();
        self.v(&shown)
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

// The generator expressions handed to any(), run as Python runs one (as
// responses.rs runs its own). Made holding iter() of what it loops over, on
// Python 3.13 its first step takes iter() of that again, as that version's
// generator code does; then it runs its body one value per next(). A
// StopIteration from its own body comes out as RuntimeError (PEP 479), and
// once it has ended or failed it gives nothing more. A plain comment: a doc
// comment would give the class a docstring, and a generator has none.
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

/// The qualname of send_request's generator expressions.
const SEND_GENEXPR: &str = "AnthropicProvider.send_request.<locals>.<genexpr>";

// ---------------------------------------------------------------------------
// The two functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_as_text")]
#[doc = "Flatten a message content field (string, or list of content blocks) to\n\
plain text \u{2014} used when translating the canonical OpenAI-shaped transcript\n\
into Anthropic's block format."]
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

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// Anthropic API provider for LLM interactions (its docstring is CLASS_DOC,
// set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "AnthropicProvider")]
pub struct AnthropicProvider;

#[pymethods]
impl AnthropicProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, tools=None, max_tokens=None, model_info=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AnthropicProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "tools", "max_tokens", "model_info"];
        let slots = bind("AnthropicProvider.__init__", 1, args, kwargs, &params, 1)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));
        let model_info = next().unwrap_or_else(|| none(py));

        me.setattr("api_key", &api_key)?;
        me.setattr("max_tokens", &max_tokens)?;
        me.setattr("model_info", or(model_info, || Ok(empty_dict(py)))?)?;
        me.setattr("api_url", "https://api.anthropic.com/v1/messages")?;
        me.setattr("session", g(py, "requests")?.getattr("Session")?.call0()?)?;
        me.setattr("cli_agent", &cli_agent)?;
        // Native tool calling: Messages-format tools ({name, input_schema,
        // description}) — the model's output contract. None only for
        // mode="text" (plain prose).
        // Deepcopy before stripping: the dialect converter aliases the shared
        // registry's parameter dicts — stripping in place would mutate the
        // registry for every other provider.
        me.setattr("tools", py.None())?;
        if tools.is_truthy()? {
            me.setattr("tools", g(py, "copy")?.getattr("deepcopy")?.call1((&tools,))?)?;
            for tool in attr(me, "tools")?.try_iter()? {
                let tool = tool?;
                let strip = attr(me, "_strip_unsupported_keywords")?;
                strip.call1((get_or(&tool, "input_schema", empty_dict(py))?,))?;
            }
        }
        Ok(())
    }

    #[staticmethod]
    #[pyo3(name = "_strip_unsupported_keywords")]
    #[doc = "Recursively strip JSON Schema keywords not supported by Anthropic structured outputs"]
    fn strip_unsupported_keywords(obj: Obj<'_>) -> PyResult<()> {
        let py = obj.py();
        let unsupported = PySet::new(py, ["maxItems", "minItems", "strict"])?;
        if isinstance(&obj, "dict")? {
            let keys = g(py, "list")?.call1((obj.call_method0("keys")?,))?;
            for key in keys.try_iter()? {
                let key = key?;
                if unsupported.as_any().contains(&key)? {
                    obj.del_item(&key)?;
                } else {
                    let strip = g(py, "AnthropicProvider")?.getattr("_strip_unsupported_keywords")?;
                    strip.call1((obj.get_item(&key)?,))?;
                }
            }
        } else if isinstance(&obj, "list")? {
            for item in obj.try_iter()? {
                let item = item?;
                let strip = g(py, "AnthropicProvider")?.getattr("_strip_unsupported_keywords")?;
                strip.call1((item,))?;
            }
        }
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to Anthropic API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("AnthropicProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // Extract system prompt from messages (Anthropic uses top-level 'system' field)
        let mut system_content = none(py);
        let api_messages = PyList::empty(py);

        for msg in messages.try_iter()? {
            let msg = msg?;
            let role = get(&msg, "role")?;
            if eq(&role, "system")? {
                system_content = msg.get_item("content")?;
                continue;
            }

            // NATIVE TRANSCRIPT translation. The coder speaks the canonical
            // OpenAI shape (assistant messages carrying `tool_calls`, plus
            // `role: "tool"` results keyed by tool_call_id). Anthropic carries
            // the same information as content BLOCKS: tool_use on the
            // assistant turn, tool_result on a following user turn.
            if eq(&role, "tool")? {
                let tool_use_id = or(get(&msg, "tool_call_id")?, || Ok(lit(py, "")))?;
                let as_text = g(py, "_as_text")?;
                let content = as_text.call1((get(&msg, "content")?,))?;
                let block = PyDict::new(py);
                block.set_item(PyString::intern(py, "type"), "tool_result")?;
                block.set_item(PyString::intern(py, "tool_use_id"), tool_use_id)?;
                block.set_item(PyString::intern(py, "content"), content)?;
                if get(&msg, "is_error")?.is_truthy()? {
                    block.set_item("is_error", true)?;
                }
                // Cache breakpoint: the loop marks its newest persistent turn
                // (a tool message) with a parts-array cache_control, which
                // _as_text would silently drop — lift it onto the tool_result
                // block instead (Anthropic supports cache_control there).
                let raw = get(&msg, "content")?;
                // if isinstance(raw, list) and any(
                //         isinstance(p, dict) and p.get("cache_control") for p in raw):
                let marked = isinstance(&raw, "list")? && {
                    let any = g(py, "any")?;
                    let marks = Genexpr::made(
                        &raw,
                        SEND_GENEXPR,
                        body(|p| {
                            let check = isinstance_value(p, "dict")?;
                            if !check.is_truthy()? {
                                return Ok(Some(check.unbind()));
                            }
                            Ok(Some(get(p, "cache_control")?.unbind()))
                        }),
                    )?;
                    any.call1((marks,))?.is_truthy()?
                };
                if marked {
                    let ephemeral = dict(py, &[("type", &lit(py, "ephemeral"))])?;
                    block.set_item("cache_control", ephemeral)?;
                }
                // Results for a batch of calls must ride on ONE user turn.
                let prev = if api_messages.is_truthy()? { Some(api_messages.as_any().get_item(-1)?) } else { None };
                // if (prev and prev.get("role") == "user"
                //         and isinstance(prev.get("content"), list) and prev["content"]
                //         and isinstance(prev["content"][0], dict)
                //         and prev["content"][0].get("type") == "tool_result"):
                let merge = match &prev {
                    None => false,
                    Some(prev) => {
                        prev.is_truthy()?
                            && eq(&get(prev, "role")?, "user")?
                            && isinstance(&get(prev, "content")?, "list")?
                            && prev.get_item("content")?.is_truthy()?
                            && isinstance(&prev.get_item("content")?.get_item(0)?, "dict")?
                            && eq(&get(&prev.get_item("content")?.get_item(0)?, "type")?, "tool_result")?
                    }
                };
                match prev {
                    Some(prev) if merge => {
                        prev.get_item("content")?.call_method1("append", (&block,))?;
                    }
                    _ => {
                        let content = PyList::new(py, [&block])?.into_any();
                        api_messages.append(dict(py, &[("role", &lit(py, "user")), ("content", &content)])?)?;
                    }
                }
                continue;
            }

            if eq(&role, "assistant")? && get(&msg, "tool_calls")?.is_truthy()? {
                let blocks = PyList::empty(py);
                let as_text = g(py, "_as_text")?;
                let text = as_text.call1((get(&msg, "content")?,))?;
                if text.call_method0("strip")?.is_truthy()? {
                    blocks.append(dict(py, &[("type", &lit(py, "text")), ("text", &text)])?)?;
                }
                for tc in msg.get_item("tool_calls")?.try_iter()? {
                    let tc = tc?;
                    // fn = (tc or {}).get("function") or {}
                    let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
                    let id = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "id")?, || Ok(lit(py, "")))?;
                    let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
                    let args_dict = g(py, "_args_dict")?;
                    let input = args_dict.call1((get(&call, "arguments")?,))?;
                    blocks.append(dict(
                        py,
                        &[("type", &lit(py, "tool_use")), ("id", &id), ("name", &name), ("input", &input)],
                    )?)?;
                }
                api_messages.append(dict(py, &[("role", &lit(py, "assistant")), ("content", blocks.as_any())])?)?;
                continue;
            }

            let content = msg.get_item("content")?;
            api_messages.append(dict(py, &[("role", &role), ("content", &content)])?)?;
        }

        // If screenshot is provided and NOT cli_agent, modify the last user message to include the annotated image
        let splice = shot.is_truthy()?
            && !attr(me, "cli_agent")?.is_truthy()?
            && g(py, "len")?.call1((&api_messages,))?.rich_compare(0, CompareOp::Gt)?.is_truthy()?;
        if splice {
            let last_msg = api_messages.as_any().get_item(-1)?;
            // A native transcript can end on a user turn whose content is a list
            // of tool_result blocks. The splice below REBUILDS content as
            // [text, image], which would destroy those results (and leave the
            // model's tool_use calls dangling), so skip it in that case - the
            // driver never ends a request on tool results anyway, since the live
            // user message always comes last.
            let content = get(&last_msg, "content")?;
            // is_tool_result = (isinstance(content, list) and content
            //                   and isinstance(content[0], dict)
            //                   and content[0].get("type") == "tool_result")
            let mut is_tool_result = isinstance_value(&content, "list")?;
            if is_tool_result.is_truthy()? {
                is_tool_result = content.clone();
                if is_tool_result.is_truthy()? {
                    is_tool_result = isinstance_value(&content.get_item(0)?, "dict")?;
                    if is_tool_result.is_truthy()? {
                        is_tool_result = get(&content.get_item(0)?, "type")?.rich_compare("tool_result", CompareOp::Eq)?;
                    }
                }
            }
            if eq(&get(&last_msg, "role")?, "user")? && !is_tool_result.is_truthy()? {
                let mut user_text = content;

                // Handle case where content is already a list
                if isinstance(&user_text, "list")? {
                    let mut text_content = lit(py, "");
                    for item in user_text.try_iter()? {
                        let item = item?;
                        if isinstance(&item, "dict")? && eq(&get(&item, "type")?, "text")? {
                            text_content = get_or(&item, "text", lit(py, ""))?;
                            break;
                        }
                    }
                    user_text = text_content;
                }

                // Anthropic uses source-based image format (not image_url)
                let first = PyList::new(py, [dict(py, &[("type", &lit(py, "text")), ("text", &user_text)])?])?;
                let images = PyList::empty(py);
                let shots = g(py, "screenshots")?.call1((&shot,))?;
                for one in comprehension(&shots)? {
                    let one = one?;
                    let source = dict(
                        py,
                        &[
                            ("type", &lit(py, "base64")),
                            ("media_type", &lit(py, "image/jpeg")), // LLM_IMAGE_FORMAT in tree/element.py
                            ("data", &one),
                        ],
                    )?;
                    images.append(dict(py, &[("type", &lit(py, "image")), ("source", &source)])?)?;
                }
                let parts = first.as_any().add(images)?;
                api_messages.as_any().get_item(-1)?.set_item("content", parts)?;
            }
        }

        // Build system as list with cache_control for prompt caching
        let mut system_param = none(py);
        if system_content.is_truthy()? {
            let ephemeral = dict(py, &[("type", &lit(py, "ephemeral"))])?;
            let part = dict(
                py,
                &[("type", &lit(py, "text")), ("text", &system_content), ("cache_control", &ephemeral)],
            )?;
            system_param = PyList::new(py, [part])?.into_any();
        }

        let headers = dict(
            py,
            &[
                ("x-api-key", &attr(me, "api_key")?),
                ("anthropic-version", &lit(py, "2023-06-01")),
                ("content-type", &lit(py, "application/json")),
            ],
        )?;

        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default.
        let max_tokens = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            out_cap.call1((attr(me, "cli_agent")?, 4000))
        })?;
        let data = PyDict::new(py);
        data.set_item(PyString::intern(py, "model"), &model)?;
        data.set_item(PyString::intern(py, "messages"), &api_messages)?;
        data.set_item(PyString::intern(py, "max_tokens"), max_tokens)?;

        // top_p / top_k per model — never temperature. Opus 5 and Sonnet 5
        // removed all three knobs, so their registry entries opt out and get
        // none of them; sending any one is a 400 that fails every attempt of
        // every step.
        let sampling = g(py, "get_sampling_params")?.call1((&model,))?;
        data.call_method1("update", (sampling,))?;

        // Adaptive thinking + effort, per model. The inverse of the line
        // above: the 5-series takes these and not the sampling knobs, Haiku
        // takes the knobs and not these. Left off entirely for Haiku, which
        // 400s on both fields.
        let thinking = g(py, "get_thinking_params")?.call1((&model, attr(me, "cli_agent")?))?;
        data.call_method1("update", (thinking,))?;

        if system_param.is_truthy()? {
            data.set_item("system", &system_param)?;
        }

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            data.set_item("tools", attr(me, "tools")?)?;
            // {"type": "any"} - every turn must call at least one tool; a
            // text-only turn is never a valid step (even termination is the
            // `exit` tool). Note: under "any" Anthropic usually suppresses
            // plain text blocks - fine here, thinking/next_goal ride as tool
            // params. Canonical rationale: see openrouter/service.py.
            // A table entry may override it (Sonnet 5.5: "auto", since it
            // rejects "any"; see view.rs).
            // The manager may override per line (the scrape optimizer runs on
            // "auto": its only tool is `more` and its answer is the text reply);
            // otherwise the table decides, the coder through its entry's
            // coder_tool_choice ("auto" where a forced call would skip the
            // thinking).
            let entry_value = g(py, "entry_value")?;
            let choice = entry_value.call1((attr(me, "model_info")?, "tool_choice", attr(me, "cli_agent")?))?;
            let tool_choice = if choice.is_truthy()? {
                dict(py, &[("type", &choice)])?
            } else {
                g(py, "get_tool_choice")?.call1((&model,))?
            };
            data.set_item("tool_choice", tool_choice)?;
        }

        let tried = (|| -> PyResult<Obj<'py>> {
            let post = attr(me, "session")?.getattr("post")?;
            let url = attr(me, "api_url")?;
            let kw = PyDict::new(py);
            kw.set_item("json", &data)?;
            kw.set_item("headers", &headers)?;
            let timeout = g(py, "http_timeout")?.call1((attr(me, "cli_agent")?,))?;
            kw.set_item("timeout", timeout)?;
            let response = post.call((url,), Some(&kw))?;
            response.call_method0("raise_for_status")?;
            let result = response.call_method0("json")?;

            if eq(&get(&result, "stop_reason")?, "refusal")? {
                // The API refused this request (no content, 0 output tokens);
                // stop_details says which policy. Raised, not returned empty:
                // the manager skips its retries and falls back where it can.
                let details = or(get(&result, "stop_details")?, || Ok(empty_dict(py)))?;
                return Err(throw(py, "RefusalError", || {
                    let line = Fmt::new(py).s("stop_reason=refusal, category=").r(&get(&details, "category")?)?;
                    let explanation = or(get(&details, "explanation")?, || Ok(lit(py, "no explanation given")))?;
                    line.s(": ").v(&explanation)?.done()
                }));
            }

            // Normalize response to match OpenAI-style format (choices[0].message.content)
            // Anthropic returns: content: [{type: "thinking", ...}, {type: "text", text: "..."},
            // {type: "tool_use", name, input}, ...]
            if attr(me, "tools")?.is_truthy()? {
                // Tools mode: collect ALL text blocks (don't stop at the first —
                // prose can interleave with tool_use) and every tool_use block
                // as {"name", "arguments": dict}.
                let texts = PyList::empty(py);
                let calls = PyList::empty(py);
                let enumerate = g(py, "enumerate")?;
                let blocks = get_or(&result, "content", PyList::empty(py).into_any())?;
                for pair in enumerate.call1((blocks,))?.try_iter()? {
                    let (i, block) = unpack2(&pair?)?;
                    if eq(&get(&block, "type")?, "text")? {
                        texts.append(get_or(&block, "text", lit(py, ""))?)?;
                    } else if eq(&get(&block, "type")?, "tool_use")? {
                        let args = get(&block, "input")?;
                        // "id": str(block.get("id") or "") or f"call_{i}"
                        let to_str = g(py, "str")?;
                        let id = or(to_str.call1((or(get(&block, "id")?, || Ok(lit(py, "")))?,))?, || {
                            Fmt::new(py).s("call_").v(&i)?.done()
                        })?;
                        let name = or(get(&block, "name")?, || Ok(lit(py, "")))?;
                        let arguments = if isinstance(&args, "dict")? { args } else { empty_dict(py) };
                        calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &arguments)])?)?;
                    }
                }
                // if not calls and not any(t.strip() for t in texts):
                let mut empty = !calls.is_truthy()?;
                if empty {
                    let any = g(py, "any")?;
                    let stripped = Genexpr::made(
                        texts.as_any(),
                        SEND_GENEXPR,
                        body(|t| Ok(Some(t.call_method0("strip")?.unbind()))),
                    )?;
                    empty = !any.call1((stripped,))?.is_truthy()?;
                }
                if empty {
                    // The loops only see "empty response"; why it is empty
                    // (an immediate "end_turn", thinking only) is in stop_reason.
                    let print = g(py, "print")?;
                    let line = Fmt::new(py)
                        .s("   Anthropic returned no text and no tool call: stop_reason=")
                        .r(&get(&result, "stop_reason")?)?
                        .s(", content=");
                    // json.dumps([b.get('type') for b in result.get('content', [])])
                    let dumps = g(py, "json")?.getattr("dumps")?;
                    let types = PyList::empty(py);
                    for b in comprehension(&get_or(&result, "content", PyList::empty(py).into_any())?)? {
                        types.append(get(&b?, "type")?)?;
                    }
                    let line = line.v(&dumps.call1((types,))?)?.done()?;
                    let tail = if get(&result, "stop_details")?.is_truthy()? {
                        let dumps = g(py, "json")?.getattr("dumps")?;
                        let kw = PyDict::new(py);
                        kw.set_item("ensure_ascii", false)?;
                        let details = dumps.call((get(&result, "stop_details")?,), Some(&kw))?;
                        Fmt::new(py).s(", stop_details=").v(&details)?.done()?
                    } else {
                        lit(py, "")
                    };
                    print.call1((line.add(tail)?,))?;
                }
                let content = join_true(&texts)?;
                let message = dict(py, &[("content", &content), ("tool_calls", calls.as_any())])?;
                let choice = dict(py, &[("message", &message)])?;
                let usage = get_or(&result, "usage", empty_dict(py))?;
                let choices = PyList::new(py, [choice])?.into_any();
                return dict(py, &[("choices", &choices), ("usage", &usage)]);
            }

            let mut text_content = lit(py, "");
            for block in get_or(&result, "content", PyList::empty(py).into_any())?.try_iter()? {
                let block = block?;
                if eq(&get(&block, "type")?, "text")? {
                    text_content = get_or(&block, "text", lit(py, ""))?;
                    break;
                }
            }

            let message = dict(py, &[("content", &text_content)])?;
            let choice = dict(py, &[("message", &message)])?;
            let usage = get_or(&result, "usage", empty_dict(py))?;
            let choices = PyList::new(py, [choice])?.into_any();
            dict(py, &[("choices", &choices), ("usage", &usage)])
        })();
        let e = match tried {
            Ok(reply) => return Ok(reply),
            Err(e) => e,
        };
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
            let mut error_msg = Fmt::new(py).s("Anthropic API request failed: ").v(&text)?.done()?;
            if g(py, "hasattr")?.call1((&error, "response"))?.is_truthy()? && !error.getattr("response")?.is_none() {
                let body = error.getattr("response")?.getattr("text")?;
                error_msg = error_msg.add(Fmt::new(py).s("\nResponse Body: ").v(&body)?.done()?)?;
            }
            Ok(error_msg)
        })();
        let error_msg = made.map_err(|raised| now(py, raised))?;
        Err(now(py, throw(py, "Exception", || Ok(error_msg))))
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
    store(m, "_as_text", wrap_pyfunction!(as_text, m)?)?;
    store(m, "_args_dict", wrap_pyfunction!(args_dict, m)?)?;
    // class AnthropicProvider:
    store(m, "AnthropicProvider", py.get_type::<AnthropicProvider>())?;
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
    let class = py.get_type::<AnthropicProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
