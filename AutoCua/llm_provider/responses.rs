//! responses: the Responses endpoint, shared by the providers that carry a
//! model whose table entry says `async_tools` (GPT-6 Astra on OpenRouter and
//! on OpenAI direct): the route test (async_route), the tools in the
//! Responses shape (responses_tools) and one request (send).
//!
//! The Rust copy of llm_provider/responses.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.responses (llm_provider/__init__.py
//! points that name here), and finds the same names in the same order, with
//! the same values, return values, errors and printed lines. Python's own
//! rules decide every result, so they are called, not imitated: json, the
//! builtins, the str, dict and list methods, the operators, the comparisons
//! and the f-strings.
//!
//! responses.py did its work when it was imported; here that is two steps.
//! register() gives the module its docstring and _build. Then the loader
//! binds what responses.py's import lines bound (json, Optional, requests,
//! LLM_HTTP_TIMEOUT, http_timeout, out_cap, screenshots) and calls _build(),
//! which runs the rest of responses.py's lines in their order: the constants
//! and each function where its def was. The `timeout=LLM_HTTP_TIMEOUT`
//! defaults of _post_json and _post_responses are read there, when their def
//! runs, as Python reads a default once.
//!
//! Every global a line reads (json, requests, print, _post_responses,
//! SYNC_TOOLS, isinstance, str, dict, len, any, ...) is read from this module
//! at that moment, as Python reads a global, so a caller or a test that
//! replaces one is obeyed. An error is raised the way Python's raise
//! statement raises it, so the error being handled at that moment becomes
//! its __context__.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PySlice, PyString, PyTuple};

use crate::computer::{unpack2, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// responses.py's docstring, the module's __doc__.
const MODULE_DOC: &str = "The Responses endpoint, shared by the providers that carry a model whose\n\
table entry says `async_tools` (GPT-6 Astra on OpenRouter and on OpenAI\n\
direct). Every platform uses it, the browser agent included.\n\
\n\
Under the ordinary tool protocol Astra issues ONE tool call per turn, so the\n\
notes-first batch never happens. On the Responses endpoint a tool declared\n\
`async: true` lets it keep working after the call, and a synchronous call\n\
ends its turn. So every tool the model must batch past is async, the notes\n\
tool included, and only the terminal tools (`done`, `exit`) stay synchronous:\n\
they end the turn, which is exactly the \"alone in its turn\" rule. The\n\
transcript is the same canonical one every provider sees; it is translated\n\
on the way out and the reply is folded back into the same\n\
{content, tool_calls, provider_meta} envelope the loop consumes.\n\
";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

/// _post_json's `timeout=LLM_HTTP_TIMEOUT` default, read when its def ran.
static POST_JSON_TIMEOUT: Mutex<Option<Py<PyAny>>> = Mutex::new(None);

/// _post_responses's `timeout=LLM_HTTP_TIMEOUT` default, read when its def ran.
static POST_RESPONSES_TIMEOUT: Mutex<Option<Py<PyAny>>> = Mutex::new(None);

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.responses", super::NATIVE)).map(Bound::unbind))
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

/// `print("<line>")`: a constant line through the print read now.
fn print_line(py: Python<'_>, line: &str) -> PyResult<()> {
    g(py, "print")?.call1((lit(py, line),))?;
    Ok(())
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

/// `[]`, a new empty list.
fn empty_list(py: Python<'_>) -> Obj<'_> {
    PyList::empty(py).into_any()
}

/// `value.get(key)`, the method looked up when it is called.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// `a == "<b>"`, as an `if` takes it: Python's comparison, then its truth.
fn eq(a: &Obj<'_>, b: &str) -> PyResult<bool> {
    a.rich_compare(b, CompareOp::Eq)?.is_truthy()
}

/// `a != "<b>"`, as an `if` takes it.
fn ne(a: &Obj<'_>, b: &str) -> PyResult<bool> {
    a.rich_compare(b, CompareOp::Ne)?.is_truthy()
}

/// A bool, as Python's `not in` and `is` give one.
fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

/// `json.dumps(<value>, ensure_ascii=False)`: json.dumps read first, then the
/// value made, as Python evaluates the call.
fn dumps<'py>(py: Python<'py>, value: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    let dumps = g(py, "json")?.getattr("dumps")?;
    let value = value()?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("ensure_ascii", false)?;
    dumps.call((value,), Some(&kwargs))
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

    fn done(self) -> PyResult<Obj<'py>> {
        let pieces: Vec<&Obj<'py>> = self.pieces.iter().collect();
        Ok(join_str(self.py, &pieces)?.into_any())
    }
}

/// A str tuple of names, as a constant tuple of responses.py holds them.
fn names<'py>(py: Python<'py>, items: &[&str]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items.iter().map(|item| PyString::intern(py, item)))
}

/// A def's parameters bound to a call's arguments, as Python binds them:
/// the positional ones first, then the keywords; each slot is the argument
/// given, or None where the def's default applies.
fn bind<'py>(
    function: &str,
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
                most,
                if most == 1 { "" } else { "s" },
                given,
                if given == 1 { "was" } else { "were" }
            )
        } else {
            format!("{function}() takes from {required} to {most} positional arguments but {given} were given")
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

/// A `timeout=LLM_HTTP_TIMEOUT` default: the value its def read, or (before
/// _build ran it) the global now.
fn default_timeout<'py>(py: Python<'py>, held: &Mutex<Option<Py<PyAny>>>) -> PyResult<Obj<'py>> {
    let value = held
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|value| value.clone_ref(py));
    match value {
        Some(value) => Ok(value.into_bound(py)),
        None => g(py, "LLM_HTTP_TIMEOUT"),
    }
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
// llm_manager.rs runs its own). Made holding iter() of what it loops over, on
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

// ---------------------------------------------------------------------------
// The route and the request
// ---------------------------------------------------------------------------

#[pyfunction]
#[doc = "True when the resolved table entry routes this model here."]
fn async_route<'py>(info: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = info.py();
    // return bool(isinstance(info, dict) and info.get("async_tools"))
    let to_bool = g(py, "bool")?;
    let check = g(py, "isinstance")?.call1((&info, g(py, "dict")?))?;
    let value = if check.is_truthy()? { get(&info, "async_tools")? } else { check };
    to_bool.call1((value,))
}

#[pyfunction]
#[doc = "The registry flattened to the Responses shape, `async: true` on every\n\
tool but SYNC_TOOLS. Name, description and parameters pass through\n\
untouched. Accepts the chat-completions form ({\"type\": \"function\",\n\
\"function\": {...}}, openrouter/openai) and the already-flat form\n\
({\"type\": \"function\", \"name\": ...}, perplexity)."]
fn responses_tools<'py>(tools: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = tools.py();
    let out = PyList::empty(py);
    for t in or(tools, || Ok(empty_list(py)))?.try_iter()? {
        let t = t?;
        // f = t.get("function") if isinstance(t.get("function"), dict) else t
        let f = if isinstance(&get(&t, "function")?, "dict")? { get(&t, "function")? } else { t };
        let name = or(get(&f, "name")?, || Ok(lit(py, "")))?;
        let tool = PyDict::new(py);
        tool.set_item("type", "function")?;
        tool.set_item("name", &name)?;
        tool.set_item("description", or(get(&f, "description")?, || Ok(lit(py, "")))?)?;
        tool.set_item("parameters", or(get(&f, "parameters")?, || Ok(empty_dict(py)))?)?;
        if !g(py, "SYNC_TOOLS")?.contains(&name)? {
            tool.set_item("async", true)?;
        }
        out.append(tool)?;
    }
    Ok(out.into_any())
}

#[pyfunction]
#[pyo3(name = "_text")]
#[doc = "Text of a message content field (string, or list of blocks with\n\
cache_control): the shared _extract_text shape, first text block wins."]
fn text<'py>(content: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = content.py();
    if isinstance(&content, "str")? {
        return Ok(content);
    }
    if isinstance(&content, "list")? {
        for item in content.try_iter()? {
            let item = item?;
            if isinstance(&item, "dict")? && eq(&get(&item, "type")?, "text")? {
                // return item.get("text", "") or ""
                return or(item.call_method1("get", ("text", ""))?, || Ok(lit(py, "")));
            }
        }
        return dumps(py, || Ok(content.clone()));
    }
    if content.is_none() {
        return Ok(lit(py, ""));
    }
    g(py, "str")?.call1((content,))
}

#[pyfunction]
#[pyo3(name = "_responses_input")]
#[doc = "The canonical transcript in the Responses dialect: the system prompt\n\
as `instructions`, user text as input_text (the screenshot as an\n\
input_image on the LAST user message only), a previous turn from this\n\
path replayed verbatim from `provider_meta.responses_output` (its\n\
function_call and reasoning items), any other assistant turn\n\
reconstructed from its text and tool_calls, and every tool result as a\n\
function_call_output. `provider_meta` never leaves the process."]
fn responses_input<'py>(messages: Obj<'py>, shot: Obj<'py>, cli_agent: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = messages.py();
    let mut instructions = none(py);
    let items = PyList::empty(py);
    for msg in messages.try_iter()? {
        let msg = msg?;
        let role = or(get(&msg, "role")?, || Ok(lit(py, "")))?;
        let content = get(&msg, "content")?;
        if eq(&role, "system")? {
            instructions = g(py, "_text")?.call1((&content,))?;
        } else if eq(&role, "tool")? {
            let item = PyDict::new(py);
            item.set_item("type", "function_call_output")?;
            item.set_item("call_id", or(get(&msg, "tool_call_id")?, || Ok(lit(py, "")))?)?;
            item.set_item("output", g(py, "_text")?.call1((&content,))?)?;
            items.append(item)?;
        } else if eq(&role, "assistant")? {
            let meta = get(&msg, "provider_meta")?;
            let replay = if isinstance(&meta, "dict")? { get(&meta, "responses_output")? } else { none(py) };
            if isinstance(&replay, "list")? {
                items.call_method1("extend", (&replay,))?;
                continue;
            }
            let text = g(py, "_text")?.call1((&content,))?;
            if text.call_method0("strip")?.is_truthy()? {
                let block = PyDict::new(py);
                block.set_item("type", "output_text")?;
                block.set_item("text", &text)?;
                let item = PyDict::new(py);
                item.set_item("role", "assistant")?;
                item.set_item("content", PyList::new(py, [block])?)?;
                items.append(item)?;
            }
            for tc in or(get(&msg, "tool_calls")?, || Ok(empty_list(py)))?.try_iter()? {
                let tc = tc?;
                // fn = (tc or {}).get("function") or {}
                let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
                let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
                let raw = get(&call, "arguments")?;
                let item = PyDict::new(py);
                item.set_item("type", "function_call")?;
                item.set_item("call_id", or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "id")?, || Ok(lit(py, "")))?)?;
                item.set_item("name", &name)?;
                // raw if isinstance(raw, str) else json.dumps(raw or {}, ensure_ascii=False)
                let arguments = if isinstance(&raw, "str")? {
                    raw
                } else {
                    dumps(py, || or(raw, || Ok(empty_dict(py))))?
                };
                item.set_item("arguments", arguments)?;
                if !g(py, "SYNC_TOOLS")?.contains(&name)? {
                    item.set_item("async", true)?;
                }
                items.append(item)?;
            }
        } else if eq(&role, "user")? {
            let block = PyDict::new(py);
            block.set_item("type", "input_text")?;
            block.set_item("text", g(py, "_text")?.call1((&content,))?)?;
            let item = PyDict::new(py);
            item.set_item("role", "user")?;
            item.set_item("content", PyList::new(py, [block])?)?;
            items.append(item)?;
        }
    }
    // if shot and not cli_agent and items and items[-1].get("role") == "user":
    if shot.is_truthy()?
        && !cli_agent.is_truthy()?
        && items.is_truthy()?
        && eq(&get(&items.as_any().get_item(-1)?, "role")?, "user")?
    {
        for one in g(py, "screenshots")?.call1((&shot,))?.try_iter()? {
            let one = one?;
            // items[-1]["content"].append({"type": "input_image", "image_url": f"data:image/jpeg;base64,{one}"})
            let append = items.as_any().get_item(-1)?.get_item("content")?.getattr("append")?;
            let image = PyDict::new(py);
            image.set_item("type", "input_image")?;
            image.set_item("image_url", Fmt::new(py).s("data:image/jpeg;base64,").v(&one)?.done()?)?;
            append.call1((image,))?;
        }
    }
    Ok(PyTuple::new(py, [instructions, items.into_any()])?.into_any())
}

#[pyfunction]
#[pyo3(
    signature = (*args, **kwargs),
    text_signature = "(session, url, api_key, error_prefix, model, messages, shot, cli_agent, tools, reasoning, extra, provider_tag, info=None)"
)]
#[doc = "Build, POST (with one degrade retry) and normalize one Responses\n\
request, on the calling provider's Session (one kept connection per\n\
provider object). `reasoning` is {\"reasoning\": {\"effort\": ...}} or {};\n\
`extra` holds provider-specific top-level fields (routing, store); under a\n\
`provider_tag` the reply's output array is kept as\n\
provider_meta.responses_output so the next request replays it verbatim\n\
(None for a provider whose meta is stripped before every send, see\n\
_META_PROVIDERS in llm_manager.py)."]
fn send<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let params = [
        "session",
        "url",
        "api_key",
        "error_prefix",
        "model",
        "messages",
        "shot",
        "cli_agent",
        "tools",
        "reasoning",
        "extra",
        "provider_tag",
        "info",
    ];
    let slots = bind("send", args, kwargs, &params, 12)?;
    let mut slots = slots.into_iter();
    let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
    let session = next();
    let url = next();
    let api_key = next();
    let error_prefix = next();
    let model = next();
    let messages = next();
    let shot = next();
    let cli_agent = next();
    let tools = next();
    let reasoning = next();
    let extra = next();
    let provider_tag = next();
    let info = next();

    // instructions, items = _responses_input(messages, shot, cli_agent)
    let pair = g(py, "_responses_input")?.call1((&messages, &shot, &cli_agent))?;
    let (instructions, items) = unpack2(&pair)?;
    let data = PyDict::new(py);
    data.set_item("model", &model)?;
    if instructions.is_truthy()? {
        data.set_item("instructions", &instructions)?;
    }
    data.set_item("input", &items)?;
    // data["max_output_tokens"] = out_cap(cli_agent, MAX_OUTPUT_TOKENS, info, CLI_MAX_OUTPUT_TOKENS)
    let out_cap = g(py, "out_cap")?;
    let cap = out_cap.call1((&cli_agent, g(py, "MAX_OUTPUT_TOKENS")?, &info, g(py, "CLI_MAX_OUTPUT_TOKENS")?))?;
    data.set_item("max_output_tokens", cap)?;
    data.call_method1("update", (or(reasoning, || Ok(empty_dict(py)))?,))?;
    data.call_method1("update", (or(extra, || Ok(empty_dict(py)))?,))?;
    data.set_item("tools", g(py, "responses_tools")?.call1((&tools,))?)?;
    // Canonical rationale in openrouter/service.py: every turn must call a
    // tool.
    data.set_item("tool_choice", "required")?;
    let headers = PyDict::new(py);
    headers.set_item("Authorization", Fmt::new(py).s("Bearer ").v(&api_key)?.done()?)?;
    headers.set_item("Content-Type", "application/json")?;
    // try: result = _post_responses(session, url, headers, data, error_prefix, http_timeout(cli_agent))
    let tried = (|| -> PyResult<Obj<'py>> {
        let post = g(py, "_post_responses")?;
        let timeout = g(py, "http_timeout")?.call1((&cli_agent,))?;
        post.call1((&session, &url, &headers, &data, &error_prefix, timeout))
    })();
    let result = match tried {
        Ok(result) => result,
        // except Exception as first:
        Err(e) => except_exception(py, e, |first| {
            // A replayed `reasoning` item is bound to the endpoint that wrote it,
            // so a router pins every retry to that one endpoint; when it is the
            // one rate-limited or failing, the three tries of the manager all hit
            // the same wall. One degrade here: the same request with the
            // reasoning items dropped (function_call, message and
            // function_call_output items stay), which the model still batches on.
            let any = g(py, "any")?;
            let reasoning_items = Genexpr::made(
                &items,
                "send.<locals>.<genexpr>",
                body(|i| {
                    let hit = get(i, "type")?.rich_compare("reasoning", CompareOp::Eq)?;
                    Ok(Some(hit.unbind()))
                }),
            )?;
            if !any.call1((reasoning_items,))?.is_truthy()? {
                return Err(first.clone_ref(py));
            }
            // first_line = (str(first).splitlines() or [""])[0]
            let lines = g(py, "str")?.call1((first.value(py),))?.call_method0("splitlines")?;
            let first_line = or(lines, || Ok(PyList::new(py, [""])?.into_any()))?.get_item(0)?;
            let print = g(py, "print")?;
            let line = Fmt::new(py)
                .s("   Responses request failed (")
                .v(&first_line)?
                .s("); retrying once without the replayed reasoning items")
                .done()?;
            print.call1((line,))?;
            let degraded = g(py, "dict")?.call1((&data,))?;
            // degraded["input"] = [i for i in items if i.get("type") != "reasoning"]
            let kept = PyList::empty(py);
            for i in comprehension(&items)? {
                let i = i?;
                if ne(&get(&i, "type")?, "reasoning")? {
                    kept.append(i)?;
                }
            }
            degraded.set_item("input", kept)?;
            let post = g(py, "_post_responses")?;
            let timeout = g(py, "http_timeout")?.call1((&cli_agent,))?;
            post.call1((&session, &url, &headers, &degraded, &error_prefix, timeout))
        })?,
    };
    g(py, "_normalize_responses")?.call1((result, &provider_tag))
}

#[pyfunction]
#[pyo3(
    name = "_post_json",
    signature = (*args, **kwargs),
    text_signature = "(session, url, headers, data, timeout=(15, 180))"
)]
#[doc = "One JSON POST: (status, body). Transport failures raise."]
fn post_json<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_post_json", args, kwargs, &["session", "url", "headers", "data", "timeout"], 4)?;
    let mut slots = slots.into_iter();
    let mut next = || slots.next().flatten();
    let session = next().unwrap_or_else(|| none(py));
    let url = next().unwrap_or_else(|| none(py));
    let headers = next().unwrap_or_else(|| none(py));
    let data = next().unwrap_or_else(|| none(py));
    let timeout = match next() {
        Some(timeout) => timeout,
        None => default_timeout(py, &POST_JSON_TIMEOUT)?,
    };
    // resp = session.post(url, json=data, headers=headers, timeout=timeout)
    let post = session.getattr("post")?;
    let kw = PyDict::new(py);
    kw.set_item("json", &data)?;
    kw.set_item("headers", &headers)?;
    kw.set_item("timeout", &timeout)?;
    let resp = post.call((&url,), Some(&kw))?;
    // return resp.status_code, resp.text
    let status = resp.getattr("status_code")?;
    let body = resp.getattr("text")?;
    Ok(PyTuple::new(py, [status, body])?.into_any())
}

#[pyfunction]
#[pyo3(
    name = "_post_responses",
    signature = (*args, **kwargs),
    text_signature = "(session, url, headers, data, prefix, timeout=(15, 180))"
)]
#[doc = "POST one Responses request. Besides the HTTP status, the body's own\n\
`error` and `status` are checked: a router answers 200 to an upstream\n\
failure mid-generation with status \"failed\" / \"incomplete\" and a cut\n\
function_call, which must reach the manager's retry ladder as an error,\n\
not the loop as a half call."]
fn post_responses<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let params = ["session", "url", "headers", "data", "prefix", "timeout"];
    let slots = bind("_post_responses", args, kwargs, &params, 5)?;
    let mut slots = slots.into_iter();
    let mut next = || slots.next().flatten();
    let session = next().unwrap_or_else(|| none(py));
    let url = next().unwrap_or_else(|| none(py));
    let headers = next().unwrap_or_else(|| none(py));
    let data = next().unwrap_or_else(|| none(py));
    let prefix = next().unwrap_or_else(|| none(py));
    let timeout = match next() {
        Some(timeout) => timeout,
        None => default_timeout(py, &POST_RESPONSES_TIMEOUT)?,
    };
    // try: status, body = _post_json(session, url, headers, data, timeout)
    let posted = (|| -> PyResult<(Obj<'py>, Obj<'py>)> {
        let post = g(py, "_post_json")?;
        unpack2(&post.call1((&session, &url, &headers, &data, &timeout))?)
    })();
    let (status, body) = match posted {
        Ok(pair) => pair,
        Err(e) => {
            // except requests.exceptions.RequestException as e:
            //     raise Exception(f"{prefix}: {e}")
            let _handling = Handling::new(py, &e);
            let kinds = (|| g(py, "requests")?.getattr("exceptions")?.getattr("RequestException"))()
                .map_err(|raised| now(py, raised))?;
            if !caught(py, &e, &kinds)? {
                return Err(e);
            }
            let raised = throw(py, "Exception", || Fmt::new(py).v(&prefix)?.s(": ").v(e.value(py).as_any())?.done());
            return Err(now(py, raised));
        }
    };
    if status.rich_compare(400, CompareOp::Ge)?.is_truthy()? {
        return Err(throw(py, "Exception", || {
            Fmt::new(py).v(&prefix)?.s(": HTTP ").v(&status)?.s("\nResponse Body: ").v(&body)?.done()
        }));
    }
    // try: result = json.loads(body)
    let loaded = (|| g(py, "json")?.getattr("loads")?.call1((&body,)))();
    let result = match loaded {
        Ok(result) => result,
        Err(e) => {
            // except ValueError as e:
            //     raise Exception(f"{prefix}: {e}")
            let _handling = Handling::new(py, &e);
            let kinds = g(py, "ValueError").map_err(|raised| now(py, raised))?;
            if !caught(py, &e, &kinds)? {
                return Err(e);
            }
            let raised = throw(py, "Exception", || Fmt::new(py).v(&prefix)?.s(": ").v(e.value(py).as_any())?.done());
            return Err(now(py, raised));
        }
    };
    if !get(&result, "error")?.is_none() {
        return Err(throw(py, "Exception", || {
            let line = Fmt::new(py).v(&prefix)?.s(": Responses error: ");
            let error = dumps(py, || result.get_item("error"))?;
            line.v(&error)?.done()
        }));
    }
    let state = or(get(&result, "status")?, || Ok(lit(py, "completed")))?;
    if eq(&state, "completed")? {
        return Ok(result);
    }
    // "incomplete" on max_output_tokens with the calls already complete is
    // the idle tail described at MAX_OUTPUT_TOKENS, not a failure: keep the
    // output up to the last completed function_call and drop the rest (empty
    // reasoning items, a cut item). Any other incomplete reason, or a cut
    // with no complete call, is an error for the manager to retry.
    let details = or(get(&result, "incomplete_details")?, || Ok(empty_dict(py)))?;
    let reason = or(get(&details, "reason")?, || Ok(lit(py, "")))?;
    let output = or(get(&result, "output")?, || Ok(empty_list(py)))?;
    let mut last = none(py);
    for pair in g(py, "enumerate")?.call1((&output,))?.try_iter()? {
        let (i, item) = unpack2(&pair?)?;
        if eq(&get(&item, "type")?, "function_call")?
            && eq(&or(get(&item, "status")?, || Ok(lit(py, "completed")))?, "completed")?
        {
            last = i;
        }
    }
    if eq(&reason, "max_output_tokens")? && !last.is_none() {
        // dropped = len(output) - (last + 1)
        let length = g(py, "len")?.call1((&output,))?;
        let dropped = length.sub(last.add(1)?)?;
        let print = g(py, "print")?;
        let line = Fmt::new(py)
            .s("   Responses status incomplete after ")
            .v(&last.add(1)?)?
            .s(" complete call(s): accepted, ")
            .v(&dropped)?
            .s(" trailing item(s) dropped")
            .done()?;
        print.call1((line,))?;
        let kept = g(py, "dict")?.call1((&result,))?;
        // kept["output"] = output[:last + 1]
        let upto = py.get_type::<PySlice>().call1((py.None(), last.add(1)?))?;
        kept.set_item("output", output.get_item(upto)?)?;
        kept.set_item("status", "completed")?;
        return Ok(kept);
    }
    Err(throw(py, "Exception", || {
        let line = Fmt::new(py).v(&prefix)?.s(": Responses status ").v(&state)?.s(": ");
        let details = dumps(py, || get(&result, "incomplete_details"))?;
        line.v(&details)?.done()
    }))
}

#[pyfunction]
#[pyo3(name = "_normalize_responses")]
#[doc = "A Responses reply folded into the chat-path envelope: function_call\n\
items become tool_calls keyed by call_id, the first output_text becomes\n\
the content, and (under a provider tag) the output array rides along as\n\
`provider_meta.responses_output` so the next request replays it\n\
verbatim, reasoning items included."]
fn normalize_responses<'py>(result: Obj<'py>, provider_tag: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = result.py();
    let output = or(get(&result, "output")?, || Ok(empty_list(py)))?;
    let mut text = lit(py, "");
    let calls = PyList::empty(py);
    for item in output.try_iter()? {
        let item = item?;
        let kind = get(&item, "type")?;
        if eq(&kind, "message")? {
            if !text.is_truthy()? {
                for block in or(get(&item, "content")?, || Ok(empty_list(py)))?.try_iter()? {
                    let block = block?;
                    if eq(&get(&block, "type")?, "output_text")? {
                        text = or(get(&block, "text")?, || Ok(lit(py, "")))?;
                        break;
                    }
                }
            }
        } else if eq(&kind, "function_call")? {
            if ne(&or(get(&item, "status")?, || Ok(lit(py, "completed")))?, "completed")? {
                continue;
            }
            let raw = get(&item, "arguments")?;
            let mut args = if isinstance(&raw, "dict")? {
                raw
            } else {
                // try: args = json.loads(raw or "{}")
                // except Exception: args = {}
                match (|| g(py, "json")?.getattr("loads")?.call1((or(raw, || Ok(lit(py, "{}")))?,)))() {
                    Ok(args) => args,
                    Err(e) => except_exception(py, e, |_| Ok(empty_dict(py)))?,
                }
            };
            if !isinstance(&args, "dict")? {
                args = empty_dict(py);
            }
            // "id": str(item.get("call_id") or item.get("id") or "") or f"call_{len(calls)}"
            let to_str = g(py, "str")?;
            let source = or(get(&item, "call_id")?, || or(get(&item, "id")?, || Ok(lit(py, ""))))?;
            let id = or(to_str.call1((source,))?, || {
                let count = g(py, "len")?.call1((&calls,))?;
                Fmt::new(py).s("call_").v(&count)?.done()
            })?;
            let name = or(get(&item, "name")?, || Ok(lit(py, "")))?;
            let call = PyDict::new(py);
            call.set_item("id", id)?;
            call.set_item("name", name)?;
            call.set_item("arguments", args)?;
            calls.append(call)?;
        }
    }
    // The tell that the route stripped the async flags (a router narrows an
    // async request to the endpoints that support it; one that does not gets
    // plain tools and the model falls back to one call per turn).
    let fc = PyList::empty(py);
    for i in comprehension(&output)? {
        let i = i?;
        if eq(&get(&i, "type")?, "function_call")? {
            fc.append(i)?;
        }
    }
    // if (any((i.get("name") or "") not in SYNC_TOOLS for i in fc)
    //         and not any(i.get("async") is True for i in fc)):
    let any = g(py, "any")?;
    let unsynced = Genexpr::made(
        fc.as_any(),
        "_normalize_responses.<locals>.<genexpr>",
        body(|i| {
            let py = i.py();
            let name = or(get(i, "name")?, || Ok(lit(py, "")))?;
            let outside = !g(py, "SYNC_TOOLS")?.contains(name)?;
            Ok(Some(boolean(py, outside).unbind()))
        }),
    )?;
    let mut warn = any.call1((unsynced,))?.is_truthy()?;
    if warn {
        let any = g(py, "any")?;
        let flagged = Genexpr::made(
            fc.as_any(),
            "_normalize_responses.<locals>.<genexpr>",
            body(|i| {
                let py = i.py();
                let is_true = get(i, "async")?.is(PyBool::new(py, true).as_any());
                Ok(Some(boolean(py, is_true).unbind()))
            }),
        )?;
        warn = !any.call1((flagged,))?.is_truthy()?;
    }
    if warn {
        // Plain ASCII: a browser agent child on Windows may write to a cp1252 log
        // file, where an emoji would raise here and cost the reply.
        print_line(
            py,
            "Warning: Responses route returned no async function_call: the async tool flags \
             were dropped upstream, expect one call per turn",
        )?;
    }
    let message = PyDict::new(py);
    message.set_item("content", &text)?;
    message.set_item("tool_calls", &calls)?;
    if provider_tag.is_truthy()? {
        // Replay only what carries something: completed calls, messages, and
        // reasoning items that hold encrypted content. An empty reasoning
        // item (no content, no summary) is the idle tail and would only bloat
        // every later request.
        let replay = PyList::empty(py);
        for item in output.try_iter()? {
            let item = item?;
            let kind = get(&item, "type")?;
            if eq(&kind, "function_call")? {
                if eq(&or(get(&item, "status")?, || Ok(lit(py, "completed")))?, "completed")? {
                    replay.append(&item)?;
                }
            } else if eq(&kind, "reasoning")? {
                if get(&item, "encrypted_content")?.is_truthy()? {
                    replay.append(&item)?;
                }
            } else {
                replay.append(&item)?;
            }
        }
        if replay.is_truthy()? {
            let meta = PyDict::new(py);
            meta.set_item("provider", &provider_tag)?;
            meta.set_item("responses_output", replay)?;
            message.set_item("provider_meta", meta)?;
        }
    }
    // Usage arrives as input_tokens / output_tokens, which _normalize_usage
    // reads as is; the cached count is mirrored where the TTY line looks.
    let usage = g(py, "dict")?.call1((or(get(&result, "usage")?, || Ok(empty_dict(py)))?,))?;
    let cached = get(&or(get(&usage, "input_tokens_details")?, || Ok(empty_dict(py)))?, "cached_tokens")?;
    if !cached.is_none() {
        let details = PyDict::new(py);
        details.set_item("cached_tokens", &cached)?;
        usage.set_item("prompt_tokens_details", details)?;
    }
    let choice = PyDict::new(py);
    choice.set_item("message", message)?;
    let reply = PyDict::new(py);
    reply.set_item("choices", PyList::new(py, [choice])?)?;
    reply.set_item("usage", usage)?;
    Ok(reply.into_any())
}

// ---------------------------------------------------------------------------
// responses.py's lines, run once the loader bound its imports
// ---------------------------------------------------------------------------

/// `key = value` at the top of responses.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// `def <name>(..., timeout=LLM_HTTP_TIMEOUT)`: the default read now, as the
/// def reads it, and kept for the function.
fn keep_default(py: Python<'_>, held: &Mutex<Option<Py<PyAny>>>) -> PyResult<()> {
    let value = g(py, "LLM_HTTP_TIMEOUT")?.unbind();
    let old = held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).replace(value);
    drop(old);
    Ok(())
}

/// Runs the lines of responses.py after its import lines, in their order,
/// and binds what each one makes on the module. The loader calls it once,
/// right after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // Tools that end the model's turn: the driver's `done` and the coder /
    // minion's `exit`. Everything else is declared async.
    store(m, "SYNC_TOOLS", names(py, &["done", "exit"])?)?;

    // A visible turn is a few hundred tokens (26 to 426 measured on the browser
    // agent, FULL thinking included). After its async calls Astra sometimes
    // idles, emitting empty reasoning items until this ceiling cuts it (about
    // 28 s per 1,500 tokens of budget, billed as almost nothing); the calls are
    // complete by then and _post_responses keeps them, so the ceiling only sets
    // how long that idling can last. 2,000 bounds it to about 40 s while leaving
    // room for the longest legitimate turn.
    store(m, "MAX_OUTPUT_TOKENS", 2000)?;
    // The coder and minion (cli_agent) write far longer turns: a FULL `reasoning`
    // call plus a whole-file `write` or a `plan` set. At 2,000 such a turn was cut
    // mid-call and the trim below kept only the notes call, so the step read as
    // notes-only. The idle tail above grows with this ceiling, so the coder does
    // not get CODER_MAX_TOKENS here: the GPT-6 entries set coder_max_tokens
    // 16000 (the models allow 128,000), which idles out in about 300 s, inside
    // the coder's 600 s read (CODER_HTTP_TIMEOUT), so a cut reply is kept, not
    // timed out. This is the route's default for an entry that sets none.
    store(m, "CLI_MAX_OUTPUT_TOKENS", 16000)?;

    store(m, "async_route", wrap_pyfunction!(async_route, m)?)?;
    store(m, "responses_tools", wrap_pyfunction!(responses_tools, m)?)?;
    store(m, "_text", wrap_pyfunction!(text, m)?)?;
    store(m, "_responses_input", wrap_pyfunction!(responses_input, m)?)?;
    store(m, "send", wrap_pyfunction!(send, m)?)?;
    keep_default(py, &POST_JSON_TIMEOUT)?;
    store(m, "_post_json", wrap_pyfunction!(post_json, m)?)?;
    keep_default(py, &POST_RESPONSES_TIMEOUT)?;
    store(m, "_post_responses", wrap_pyfunction!(post_responses, m)?)?;
    store(m, "_normalize_responses", wrap_pyfunction!(normalize_responses, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes responses.py's place with its docstring and
/// _build, which the loader calls once it has bound the import lines' names
/// (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
