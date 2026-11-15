//! service: the OpenRouter provider (OpenRouterProvider), which sends the
//! OpenAI-format chat-completions request to OpenRouter (or, for a model the
//! table routes there, the shared Responses endpoint) and, when tools are on,
//! folds the reply's tool calls into the shape the loops read.
//!
//! The Rust copy of llm_provider/openrouter/service.py. Python imports it
//! under its old dotted name, AutoCua.llm_provider.openrouter.service
//! (llm_provider/openrouter/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values and
//! errors. Python's own rules decide every result, so they are called, not
//! imitated: json, the builtins, the str, dict and list methods, the
//! operators, the comparisons and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (json, requests, base64, Dict, Any,
//! Optional, the view's get_reasoning_params, the Responses module's
//! async_route and send_responses, entry_value, http_timeout, out_cap,
//! screenshots) and calls _build(), which runs the rest of service.py's lines
//! in their order: the two Gemini helpers, the class, then the function.
//!
//! Every global a line reads (json, requests, get_reasoning_params,
//! async_route, send_responses, entry_value, http_timeout, out_cap,
//! screenshots, _is_gemini_route, _wrap_for_gemini, _normalize_tool_response,
//! len, str, isinstance, enumerate, ...) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces one
//! is obeyed. The view's names are this module's own, bound from view when
//! it was imported. The provider keeps its attributes in its __dict__, and
//! its methods reach them through the instance, so a subclass or a patched
//! attribute (the session) is obeyed. An error is raised the way Python's
//! raise statement raises it, so the error being handled at that moment
//! becomes its __context__.

use pyo3::exceptions::{PyAttributeError, PyTypeError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, type_name, unpack2, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// OpenRouterProvider's docstring.
const CLASS_DOC: &str = "OpenRouter API provider for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 40;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 7] = ["api_key", "api_url", "cli_agent", "max_tokens", "model_info", "session", "tools"];

/// Where every chat-completions request goes.
const API_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.openrouter.service", super::super::NATIVE)).map(Bound::unbind))
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

/// `{**part, ...}`: a new dict with `part` merged in first, as the
/// interpreter merges a `**` part (DICT_UPDATE): PyDict_Update, and a part
/// that is not a mapping refused in its words. The display's other keys are
/// set on it after.
fn merged<'py>(part: &Obj<'py>) -> PyResult<Bound<'py, PyDict>> {
    let py = part.py();
    let built = PyDict::new(py);
    // SAFETY: two valid objects, with the GIL held.
    if unsafe { ffi::PyDict_Update(built.as_ptr(), part.as_ptr()) } < 0 {
        let e = PyErr::fetch(py);
        if !e.is_instance_of::<PyAttributeError>(py) {
            return Err(e);
        }
        // "'%.200s' object is not a mapping": at most 200 bytes of the name.
        let mut kind = type_name(part)?;
        if kind.len() > 200 {
            let mut end = 200;
            while !kind.is_char_boundary(end) {
                end -= 1;
            }
            kind.truncate(end);
        }
        return Err(PyTypeError::new_err(format!("'{kind}' object is not a mapping")));
    }
    Ok(built)
}

/// Whether a comprehension takes iter() a second time of the iterator it
/// loops over: Python 3.13's code does, 3.12's and 3.14's loop over it as it
/// is.
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
// The Gemini helpers
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_is_gemini_route")]
#[doc = "True when OpenRouter will hand this request to Google's backend, where a\n\
tool result stops being a string and becomes a protobuf Struct."]
fn is_gemini_route<'py>(model: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = model.py();
    // name = str(model or "").lower()
    let to_str = g(py, "str")?;
    let name = to_str.call1((or(model, || Ok(lit(py, "")))?,))?.call_method0("lower")?;
    // return name.startswith("google/") or "gemini" in name
    let google = name.call_method1("startswith", ("google/",))?;
    if google.is_truthy()? {
        return Ok(google);
    }
    Ok(PyBool::new(py, name.contains("gemini")?).to_owned().into_any())
}

#[pyfunction]
#[pyo3(name = "_wrap_for_gemini")]
#[doc = "Carry one tool result to a Gemini route as a SINGLE JSON object.\n\
\n\
Gemini's functionResponse body is a Struct, so the text we send is coerced\n\
into JSON on the way in. A plain envelope survives that trip only while it\n\
contains nothing JSON-shaped: measured against\n\
google/gemini-3.1-pro-preview, `<Tool_response>` + a JSON head + an\n\
<output> block came back `MALFORMED_RESPONSE` with an EMPTY turn, and an\n\
envelope whose output body merely CONTAINED braces (a viewed .json file,\n\
`read`'s own [N] line numbers, any code) reached the model as scraped\n\
fragments — `[{},[2],[3],{\"react\":\"18.0.0\"},[4],[5]]` in place of the file.\n\
That is the bug behind a coder that reads every command as \"no output\" and\n\
then invents paths it never saw.\n\
\n\
Wrapping the whole envelope as one JSON object makes the coercion lossless:\n\
the Struct holds a single string field, and the model receives the envelope\n\
byte-for-byte (verified: plain output, a JSON file with [N] numbers, and a\n\
multi-result batch all echoed back verbatim). Only Google routes are\n\
wrapped — every other model on OpenRouter keeps the raw envelope."]
fn wrap_for_gemini<'py>(content: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = content.py();
    // text = content if isinstance(content, str) else str(content or "")
    let text = if isinstance(&content, "str")? {
        content
    } else {
        let to_str = g(py, "str")?;
        to_str.call1((or(content, || Ok(lit(py, "")))?,))?
    };
    // return json.dumps({"tool_response": text}, ensure_ascii=False)
    let dumps = g(py, "json")?.getattr("dumps")?;
    let body = dict(py, &[("tool_response", &text)])?;
    let kw = PyDict::new(py);
    kw.set_item("ensure_ascii", false)?;
    dumps.call((body,), Some(&kw))
}

// ---------------------------------------------------------------------------
// OpenRouterProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// OpenRouter API provider for LLM interactions (its docstring is CLASS_DOC,
// set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "OpenRouterProvider")]
pub struct OpenRouterProvider;

#[pymethods]
impl OpenRouterProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, model_info=None, tools=None, max_tokens=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        OpenRouterProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "model_info", "tools", "max_tokens"];
        let slots = bind("OpenRouterProvider.__init__", 1, args, kwargs, &params, 1)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
        let model_info = next().unwrap_or_else(|| none(py));
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));

        me.setattr("api_key", &api_key)?;
        me.setattr("max_tokens", &max_tokens)?;
        me.setattr("api_url", lit(py, API_URL))?;
        me.setattr("session", g(py, "requests")?.getattr("Session")?.call0()?)?;
        me.setattr("cli_agent", &cli_agent)?;
        me.setattr("model_info", or(model_info, || Ok(empty_dict(py)))?)?;
        // Native tool calling: OpenAI-format function tools — the model's
        // output contract. None only for mode="text" (plain prose).
        me.setattr("tools", or(tools, || Ok(none(py)))?)?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to OpenRouter API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("OpenRouterProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // GPT-6 Astra (table flag `async_tools`): the Responses endpoint with
        // async tool definitions is the one request shape in which it emits
        // the notes call AND the actions in a single turn; on chat/completions
        // it emits one tool call per turn. mode="text" (no tools) stays on
        // chat/completions like every other model.
        // if self.tools and async_route(self.model_info):
        let routed = attr(me, "tools")?.is_truthy()? && {
            let async_route = g(py, "async_route")?;
            async_route.call1((attr(me, "model_info")?,))?.is_truthy()?
        };
        if routed {
            // return send_responses(session=self.session, url=..., ...), the
            // function read first, then each argument in its order.
            let send = g(py, "send_responses")?;
            let kw = PyDict::new(py);
            kw.set_item("session", attr(me, "session")?)?;
            kw.set_item("url", "https://openrouter.ai/api/v1/responses")?;
            kw.set_item("api_key", attr(me, "api_key")?)?;
            kw.set_item("error_prefix", "OpenRouter API request failed")?;
            kw.set_item("model", &model)?;
            kw.set_item("messages", &messages)?;
            kw.set_item("shot", &shot)?;
            kw.set_item("cli_agent", attr(me, "cli_agent")?)?;
            kw.set_item("tools", attr(me, "tools")?)?;
            // reasoning=get_reasoning_params(model, self.model_info, self.cli_agent)
            let levels = g(py, "get_reasoning_params")?;
            let reasoning = levels.call1((&model, attr(me, "model_info")?, attr(me, "cli_agent")?))?;
            kw.set_item("reasoning", reasoning)?;
            kw.set_item("info", attr(me, "model_info")?)?;
            // Same routing preference as the chat path. No temperature
            // (Astra rejects it) and no route:fallback (unverified on this
            // endpoint).
            // extra={"provider": {"sort": "throughput"}}
            let sort = dict(py, &[("sort", &lit(py, "throughput"))])?;
            kw.set_item("extra", dict(py, &[("provider", &sort)])?)?;
            kw.set_item("provider_tag", "openrouter")?;
            return send.call((), Some(&kw));
        }

        // A `role: "tool"` message must carry a PLAIN STRING here. The agent
        // loop marks its newest persistent turn with a parts-array
        // cache_control breakpoint, and in a native transcript that turn is a
        // tool result — but OpenRouter forwards messages verbatim to whatever
        // backend it routes to, and a tool message whose content is an array
        // carrying the Anthropic-only `cache_control` key is not something a
        // non-Anthropic backend (Gemini, GPT, …) is obliged to read. The
        // direct Anthropic provider flattens this itself (see _as_text there)
        // and google/perplexity flatten via _extract_text; OpenRouter was the
        // one passthrough path, so the newest tool result — the exact one the
        // model has to judge each step — was the one at risk of arriving
        // unreadable. Flatten it back to text; the breakpoint is dropped for
        // tool turns on this provider, which costs some cache reuse and buys
        // the model actually seeing its tool output.
        // Rebuilt, not mutated in place: the caller's message dicts are shared
        // with the agent loop's own history.
        //
        // The same pass translates the transcript's private `provider_meta`
        // into this dialect: a reasoning model's own blocks go back on the
        // assistant turn as `reasoning_details`. Gemini 3 (and every other
        // reasoning model routed here) binds a function RESULT to the call
        // through the encrypted signature inside those blocks — strip them and
        // the model either rejects the turn or reads its tool output as empty.
        // The key itself is ALWAYS removed, tagged for this provider or not —
        // it is ours, not OpenRouter's, and an unknown message key is a 400
        // risk. Tool results bound for a Gemini route are wrapped as one JSON
        // object (see _wrap_for_gemini).
        // gemini_route = _is_gemini_route(model)
        let gemini_route = g(py, "_is_gemini_route")?.call1((&model,))?;
        let prepared = PyList::empty(py);
        // for msg in messages:
        for msg in messages.try_iter()? {
            let mut msg = msg?;
            if eq(&get(&msg, "role")?, "tool")? {
                if isinstance(&get(&msg, "content")?, "list")? {
                    // msg = {**msg, "content": "\n".join(
                    //     part.get("text", "") for part in msg["content"]
                    //     if isinstance(part, dict) and part.get("type") == "text")}
                    let made = merged(&msg)?;
                    let join = lit(py, "\n").getattr("join")?;
                    let parts = msg.get_item("content")?.try_iter()?;
                    let texts = PyList::empty(py);
                    for part in parts {
                        let part = part?;
                        if isinstance(&part, "dict")? && eq(&get(&part, "type")?, "text")? {
                            texts.append(get_or(&part, "text", lit(py, ""))?)?;
                        }
                    }
                    made.set_item("content", join.call1((texts,))?)?;
                    msg = made.into_any();
                }
                if gemini_route.is_truthy()? {
                    // msg = {**msg, "content": _wrap_for_gemini(msg.get("content"))}
                    let made = merged(&msg)?;
                    let wrap = g(py, "_wrap_for_gemini")?;
                    made.set_item("content", wrap.call1((get(&msg, "content")?,))?)?;
                    msg = made.into_any();
                }
            }
            // meta = msg.get("provider_meta")
            let meta = get(&msg, "provider_meta")?;
            if !meta.is_none() {
                // msg = {k: v for k, v in msg.items() if k != "provider_meta"}
                let kept = PyDict::new(py);
                for pair in comprehension(&msg.call_method0("items")?)? {
                    let (k, v) = unpack2(&pair?)?;
                    if k.rich_compare("provider_meta", CompareOp::Ne)?.is_truthy()? {
                        kept.set_item(k, v)?;
                    }
                }
                let kept = kept.into_any();
                // if (msg.get("role") == "assistant" and isinstance(meta, dict)
                //         and meta.get("provider") == "openrouter"
                //         and meta.get("reasoning_details")):
                if eq(&get(&kept, "role")?, "assistant")?
                    && isinstance(&meta, "dict")?
                    && eq(&get(&meta, "provider")?, "openrouter")?
                    && get(&meta, "reasoning_details")?.is_truthy()?
                {
                    // msg["reasoning_details"] = meta["reasoning_details"]
                    let details = meta.get_item("reasoning_details")?;
                    kept.set_item("reasoning_details", details)?;
                }
                msg = kept;
            }
            prepared.append(&msg)?;
        }
        // messages = prepared
        let messages = prepared.into_any();

        // If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        // if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
        let several = || -> PyResult<bool> {
            let count = g(py, "len")?.call1((&messages,))?;
            count.rich_compare(1, CompareOp::Gt)?.is_truthy()
        };
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && several()? {
            let user_message = messages.get_item(-1)?.get_item("content")?;
            // [{"type": "text", "text": user_message}] + [
            //     {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{shot}"}}
            //     for shot in screenshots(annotated_screenshot_base64)]
            let text = list1(dict(py, &[("type", &lit(py, "text")), ("text", &user_message)])?)?;
            let images = PyList::empty(py);
            let shots = g(py, "screenshots")?.call1((&shot,))?;
            for one in comprehension(&shots)? {
                let one = one?;
                let url = Fmt::new(py).s("data:image/jpeg;base64,").v(&one)?.done()?;
                let image_url = dict(py, &[("url", &url)])?;
                images.append(dict(py, &[("type", &lit(py, "image_url")), ("image_url", &image_url)])?)?;
            }
            let content = text.add(&images)?;
            messages.get_item(-1)?.set_item("content", content)?;
        }

        // 0.2, not 0.5: this loop wants the confident, repeatable choice
        // every step, and its output is tool-call JSON rather than prose.
        // Models that no longer accept the knob at all (the Anthropic
        // entries) are unaffected — OpenRouter drops parameters a route
        // does not support rather than erroring, verified against
        // claude-sonnet-5.
        let temperature = PyFloat::new(py, 0.2).into_any();
        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default.
        // "max_tokens": self.max_tokens or out_cap(self.cli_agent, 10000, self.model_info)
        let max_tokens = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            let cli_agent = attr(me, "cli_agent")?;
            let model_info = attr(me, "model_info")?;
            out_cap.call1((cli_agent, 10000, model_info))
        })?;
        // Without this, OpenRouter load-balances every call across backends
        // by price — so identical steps land on different upstreams and the
        // same call has been measured at 20s, 38s and 63s. Sorting turns
        // load balancing OFF and pins the route by throughput, which also
        // keeps the prefix cache warm (caching is per-backend: a rebalanced
        // step pays a cold prefill on top of a slow backend).
        // "provider": {"sort": "throughput"}
        let provider = dict(py, &[("sort", &lit(py, "throughput"))])?;
        let data = dict(
            py,
            &[
                ("model", &model),
                ("messages", &messages),
                ("temperature", &temperature),
                ("max_tokens", &max_tokens),
                ("route", &lit(py, "fallback")),
                ("provider", &provider),
            ],
        )?;
        // How hard the model thinks, per view.py's per-model table. Carried in
        // OpenRouter's unified `reasoning` field, which it translates to each
        // backend's own control. Left off for a model with no level set and
        // for any hand-typed name, which then keeps its published default.
        // data.update(get_reasoning_params(model, self.model_info, self.cli_agent))
        let update = data.getattr("update")?;
        let levels = g(py, "get_reasoning_params")?;
        update.call1((levels.call1((&model, attr(me, "model_info")?, attr(me, "cli_agent")?))?,))?;

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            data.set_item("tools", attr(me, "tools")?)?;
            // "required" - every turn MUST call at least one tool. A text-only
            // turn is never a valid step in this loop: every step routes an
            // action and even termination is a tool (`exit` carries the final
            // report in its value), so there is no "final answer on the text
            // channel" ending. Forcing makes the text-only failure mode
            // unrepresentable instead of salvageable; prose still rides the
            // text channel alongside the calls. OpenRouter passes tool_choice
            // through to the backend and upstream support is uneven, so the
            // no-tool-called nudge + leaked-envelope salvage stay as backstop.
            // A table entry may override the value (Qwen: "auto", the one
            // value its backend accepts with thinking on; see view.py).
            // data["tool_choice"] = entry_value(self.model_info, "tool_choice", self.cli_agent) or "required"
            let entry_value = g(py, "entry_value")?;
            let picked = entry_value.call1((attr(me, "model_info")?, "tool_choice", attr(me, "cli_agent")?))?;
            data.set_item("tool_choice", or(picked, || Ok(lit(py, "required")))?)?;
        }

        // headers = {"Authorization": f"Bearer {self.api_key}", "Content-Type": "application/json"}
        let auth = Fmt::new(py).s("Bearer ").v(&attr(me, "api_key")?)?.done()?;
        let headers = dict(py, &[("Authorization", &auth), ("Content-Type", &lit(py, "application/json"))])?;

        let tried = (|| -> PyResult<Obj<'py>> {
            // response = self.session.post(self.api_url, json=data, headers=headers,
            //                              timeout=http_timeout(self.cli_agent))
            let post = attr(me, "session")?.getattr("post")?;
            let url = attr(me, "api_url")?;
            let kw = PyDict::new(py);
            kw.set_item("json", &data)?;
            kw.set_item("headers", &headers)?;
            let http_timeout = g(py, "http_timeout")?;
            let timeout = http_timeout.call1((attr(me, "cli_agent")?,))?;
            kw.set_item("timeout", timeout)?;
            let response = post.call((url,), Some(&kw))?;
            response.call_method0("raise_for_status")?;
            let result = response.call_method0("json")?;
            if attr(me, "tools")?.is_truthy()? {
                return g(py, "_normalize_tool_response")?.call1((result,));
            }
            Ok(result)
        })();
        match tried {
            Ok(result) => Ok(result),
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
                    // error_msg = f"OpenRouter API request failed: {str(e)}"
                    let text = g(py, "str")?.call1((&error,))?;
                    let mut error_msg = Fmt::new(py).s("OpenRouter API request failed: ").v(&text)?.done()?;
                    // if hasattr(e, 'response') and e.response is not None:
                    if g(py, "hasattr")?.call1((&error, "response"))?.is_truthy()? && !error.getattr("response")?.is_none() {
                        // error_msg += f"\nResponse Body: {e.response.text}"
                        let text = error.getattr("response")?.getattr("text")?;
                        error_msg = error_msg.add(Fmt::new(py).s("\nResponse Body: ").v(&text)?.done()?)?;
                    }
                    Ok(error_msg)
                })();
                let error_msg = made.map_err(|raised| now(py, raised))?;
                Err(now(py, throw(py, "Exception", || Ok(error_msg))))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The function
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_normalize_tool_response")]
#[doc = "Normalize an OpenAI-format tool-call response: keep the usual\n\
choices/message shape but replace tool_calls with\n\
[{\"id\", \"name\", \"arguments\": dict}] (arguments JSON-decoded). The `id` is\n\
load-bearing — the loop echoes it back so each tool result is matched to\n\
the call that produced it.\n\
\n\
`reasoning_details` is preserved the same way, as `provider_meta`: it\n\
carries the reasoning blocks (including Gemini 3's encrypted thought\n\
signature, keyed to the first tool call's id) that this turn must be\n\
replayed with. Dropping it here is what left the model unable to bind its\n\
tool results to the calls that asked for them."]
fn normalize_tool_response<'py>(result: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = result.py();
    // message = ((result.get("choices") or [{}])[0].get("message")) or {}
    let choices = or(get(&result, "choices")?, || list1(empty_dict(py)))?;
    let message = or(get(&choices.get_item(0)?, "message")?, || Ok(empty_dict(py)))?;
    let calls = PyList::empty(py);
    // for i, tc in enumerate(message.get("tool_calls") or []):
    let enumerate = g(py, "enumerate")?;
    let listed = or(get(&message, "tool_calls")?, || Ok(PyList::empty(py).into_any()))?;
    for pair in enumerate.call1((listed,))?.try_iter()? {
        let (i, tc) = unpack2(&pair?)?;
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
        // calls.append({
        //     "id": str((tc or {}).get("id") or "") or f"call_{i}",
        //     "name": fn.get("name") or "",
        //     "arguments": args,
        // })
        let to_str = g(py, "str")?;
        let raw_id = or(get(&or(tc, || Ok(empty_dict(py)))?, "id")?, || Ok(lit(py, "")))?;
        let id = or(to_str.call1((raw_id,))?, || Fmt::new(py).s("call_").v(&i)?.done())?;
        let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
        calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &args)])?)?;
    }
    // out = {"content": message.get("content") or "", "tool_calls": calls}
    let content = or(get(&message, "content")?, || Ok(lit(py, "")))?;
    let out = dict(py, &[("content", &content), ("tool_calls", calls.as_any())])?;
    // details = message.get("reasoning_details")
    let details = get(&message, "reasoning_details")?;
    // if isinstance(details, list) and details:
    if isinstance(&details, "list")? && details.is_truthy()? {
        // out["provider_meta"] = {"provider": "openrouter", "reasoning_details": details}
        let meta = dict(py, &[("provider", &lit(py, "openrouter")), ("reasoning_details", &details)])?;
        out.set_item("provider_meta", meta)?;
    }
    // return {"choices": [{"message": out}], "usage": result.get("usage", {})}
    let choices = list1(dict(py, &[("message", &out)])?)?;
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
    store(m, "_is_gemini_route", wrap_pyfunction!(is_gemini_route, m)?)?;
    store(m, "_wrap_for_gemini", wrap_pyfunction!(wrap_for_gemini, m)?)?;
    // class OpenRouterProvider:
    store(m, "OpenRouterProvider", py.get_type::<OpenRouterProvider>())?;
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
    let class = py.get_type::<OpenRouterProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
