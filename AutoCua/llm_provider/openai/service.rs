//! service: the OpenAI provider (OpenAIProvider), which sends one request
//! through the openai SDK's chat completions (or, for a model the table
//! routes there, the shared Responses endpoint) and folds the reply into
//! the OpenAI-shaped envelope the loops read.
//!
//! The Rust copy of llm_provider/openai/service.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.openai.service
//! (llm_provider/openai/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values and
//! errors. Python's own rules decide every result, so they are called, not
//! imitated: json, the openai SDK, the builtins, the str, dict and list
//! methods, the operators, the comparisons and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (json, requests, Dict, Any, Optional,
//! OpenAI, Timeout, the view's get_reasoning_effort, the Responses module's
//! async_route and send_responses, http_timeout, out_cap, screenshots) and
//! calls _build(), which runs the rest of service.py's lines: the class.
//!
//! Every global a line reads (json, requests, OpenAI, Timeout,
//! get_reasoning_effort, async_route, send_responses, out_cap, len, str,
//! getattr, isinstance, ...) is read from this module at that moment, as
//! Python reads a global, so a caller or a test that replaces one is
//! obeyed. The view's names are this module's own, bound from view when it
//! was imported. The provider keeps its attributes in its __dict__, and its
//! methods reach them through the instance, so a subclass or a patched
//! attribute (the client, the session) is obeyed. An error is raised the
//! way Python's raise statement raises it, so the error being handled at
//! that moment becomes its __context__.

use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, unpack2};
use crate::ios::controller;
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// OpenAIProvider's docstring.
const CLASS_DOC: &str = "OpenAI API provider for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 11;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 7] = ["api_key", "cli_agent", "client", "max_tokens", "model_info", "session", "tools"];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.openai.service", super::super::NATIVE)).map(Bound::unbind))
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

/// A whole-number constant.
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

/// Whether a comprehension takes iter() a second time of the iterator it
/// loops over: Python 3.13's code does, 3.12's and 3.14's loop over it as
/// it is.
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
// OpenAIProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// OpenAI API provider for LLM interactions (its docstring is CLASS_DOC, set
// in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "OpenAIProvider")]
pub struct OpenAIProvider;

#[pymethods]
impl OpenAIProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, tools=None, max_tokens=None, model_info=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        OpenAIProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "tools", "max_tokens", "model_info"];
        let slots = bind("OpenAIProvider.__init__", 1, args, kwargs, &params, 1)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));
        let model_info = next().unwrap_or_else(|| none(py));

        // http_timeout() in the SDK's own Timeout class (httpx in openai 2.x,
        // httpx2 in 3.x): connect 15 s, each read and write 180 s (600 s for
        // the coder), in place of the SDK's connect 5 s, read 600 s.
        // max_retries stays at the SDK default of 2, so a server that stays
        // silent costs 3 reads before the manager's own retry.
        // connect, read = http_timeout(cli_agent)
        let (connect, read) = unpack2(&g(py, "http_timeout")?.call1((&cli_agent,))?)?;
        // self.client = OpenAI(api_key=api_key, timeout=Timeout(read, connect=connect))
        let client_class = g(py, "OpenAI")?;
        let timeout_class = g(py, "Timeout")?;
        let timeout_kw = PyDict::new(py);
        timeout_kw.set_item("connect", &connect)?;
        let timeout = timeout_class.call((&read,), Some(&timeout_kw))?;
        let client_kw = PyDict::new(py);
        client_kw.set_item("api_key", &api_key)?;
        client_kw.set_item("timeout", timeout)?;
        me.setattr("client", client_class.call((), Some(&client_kw))?)?;
        // Kept for the Responses path, which posts without the SDK.
        me.setattr("api_key", &api_key)?;
        me.setattr("session", g(py, "requests")?.getattr("Session")?.call0()?)?;
        // The resolved table entry; the manager re-points it on a fallback swap.
        me.setattr("model_info", or(model_info, || Ok(empty_dict(py)))?)?;
        me.setattr("max_tokens", &max_tokens)?;
        me.setattr("cli_agent", &cli_agent)?;
        // Native tool calling: OpenAI-format function tools — the model's
        // output contract. None only for mode="text" (plain prose).
        me.setattr("tools", or(tools, || Ok(none(py)))?)?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to OpenAI API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("OpenAIProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // GPT-6 Astra (table flag `async_tools`): /v1/chat/completions refuses
        // function tools on this model with any reasoning_effort, and `none`
        // is not a level it accepts, so with tools it can only be driven on
        // /v1/responses, where async tool definitions also give the notes
        // call plus the batch in one turn (see ..responses). mode="text" (no
        // tools) stays on chat/completions.
        // if self.tools and async_route(self.model_info):
        let routed = attr(me, "tools")?.is_truthy()? && {
            let async_route = g(py, "async_route")?;
            async_route.call1((attr(me, "model_info")?,))?.is_truthy()?
        };
        if routed {
            // effort = get_reasoning_effort(model, self.model_info, self.cli_agent).get("reasoning_effort")
            let levels = g(py, "get_reasoning_effort")?;
            let found = levels.call1((&model, attr(me, "model_info")?, attr(me, "cli_agent")?))?;
            let effort = get(&found, "reasoning_effort")?;
            let send = g(py, "send_responses")?;
            let kw = PyDict::new(py);
            kw.set_item("session", attr(me, "session")?)?;
            kw.set_item("url", "https://api.openai.com/v1/responses")?;
            kw.set_item("api_key", attr(me, "api_key")?)?;
            kw.set_item("error_prefix", "OpenAI API request failed")?;
            kw.set_item("model", &model)?;
            kw.set_item("messages", &messages)?;
            kw.set_item("shot", &shot)?;
            kw.set_item("cli_agent", attr(me, "cli_agent")?)?;
            kw.set_item("tools", attr(me, "tools")?)?;
            kw.set_item("info", attr(me, "model_info")?)?;
            // The table's `reasoning_effort` level in the Responses spelling.
            // reasoning={"reasoning": {"effort": effort}} if effort else {}
            let reasoning = if effort.is_truthy()? {
                dict(py, &[("reasoning", &dict(py, &[("effort", &effort)])?)])?
            } else {
                empty_dict(py)
            };
            kw.set_item("reasoning", reasoning)?;
            // Nothing stored server-side: the transcript is replayed from
            // our side (function calls and their outputs), and the model
            // batches on that replay.
            kw.set_item("extra", dict(py, &[("store", PyBool::new(py, false).as_any())])?)?;
            // provider_meta is stripped before every send on this provider
            // (_META_PROVIDERS): the replay reconstructs the calls.
            kw.set_item("provider_tag", none(py))?;
            return send.call((), Some(&kw));
        }

        // If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        // if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
        if shot.is_truthy()?
            && !attr(me, "cli_agent")?.is_truthy()?
            && g(py, "len")?.call1((&messages,))?.rich_compare(1i32, CompareOp::Gt)?.is_truthy()?
        {
            let mut user_message = messages.get_item(-1i32)?.get_item("content")?;

            // Handle case where content might already be a list
            if isinstance(&user_message, "list")? {
                // Extract text from existing list structure
                let mut text_content = lit(py, "");
                for item in user_message.try_iter()? {
                    let item = item?;
                    if isinstance(&item, "dict")? && eq(&get(&item, "type")?, "text")? {
                        text_content = get_or(&item, "text", lit(py, ""))?;
                        break;
                    }
                }
                user_message = text_content;
            }

            // messages[-1]["content"] = [{"type": "text", "text": user_message}] + [
            //     {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{shot}"}}
            //     for shot in screenshots(annotated_screenshot_base64)
            // ]
            let first = list1(dict(py, &[("type", &lit(py, "text")), ("text", &user_message)])?)?;
            let images = PyList::empty(py);
            let screenshots = g(py, "screenshots")?;
            for one in comprehension(&screenshots.call1((&shot,))?)? {
                let one = one?;
                let url = Fmt::new(py).s("data:image/jpeg;base64,").v(&one)?.done()?;
                let image_url = dict(py, &[("url", &url)])?;
                images.append(dict(py, &[("type", &lit(py, "image_url")), ("image_url", &image_url)])?)?;
            }
            let content = first.add(&images)?;
            messages.get_item(-1i32)?.set_item("content", content)?;
        }

        // Prepare API call parameters
        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default.
        // "max_completion_tokens": self.max_tokens or out_cap(self.cli_agent, 4000, self.model_info)
        let cap = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            out_cap.call1((attr(me, "cli_agent")?, 4000i32, attr(me, "model_info")?))
        })?;
        let request = PyDict::new(py);
        request.set_item("model", &model)?;
        request.set_item("messages", &messages)?;
        request.set_item("max_completion_tokens", cap)?;
        request.set_item("verbosity", "medium")?; // Set verbosity to medium
        // How hard the model thinks, per view.py's table. Kept out of the dict
        // above because it is per-model, not a constant: an unregistered model
        // contributes nothing and keeps OpenAI's default. No sampling params
        // are ever added here — GPT-6 Astra rejects temperature/top_p/seed.
        // params.update(get_reasoning_effort(model, self.model_info))
        let update = request.as_any().getattr("update")?;
        let levels = g(py, "get_reasoning_effort")?;
        update.call1((levels.call1((&model, attr(me, "model_info")?))?,))?;

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            request.set_item("tools", attr(me, "tools")?)?;
            // "required" - every turn must call at least one tool; a text-only
            // turn is never a valid step (even termination is the `exit`
            // tool). Canonical rationale: see openrouter/service.py.
            let choice = or(get(&attr(me, "model_info")?, "tool_choice")?, || Ok(lit(py, "required")))?;
            request.set_item("tool_choice", choice)?;
        }

        let tried = (|| -> PyResult<Obj<'py>> {
            // response = self.client.chat.completions.create(**params)
            let create = attr(me, "client")?.getattr("chat")?.getattr("completions")?.getattr("create")?;
            let response = create.call((), Some(&request.copy()?))?;

            // Return in the same format as other providers
            let msg = response.getattr("choices")?.get_item(0i32)?.getattr("message")?;
            let content = or(msg.getattr("content")?, || Ok(lit(py, "")))?;
            let message = dict(py, &[("content", &content)])?;
            if attr(me, "tools")?.is_truthy()? {
                // SDK tool-call objects -> [{"id", "name", "arguments": dict}]
                // (arguments arrive as a JSON string; malformed -> {}). The id
                // is echoed back by the loop to match each result to its call.
                let calls = PyList::empty(py);
                let enumerate = g(py, "enumerate")?;
                let source = or(msg.getattr("tool_calls")?, || Ok(PyList::empty(py).into_any()))?;
                for pair in enumerate.call1((source,))?.try_iter()? {
                    let (i, tc) = unpack2(&pair?)?;
                    let call = tc.getattr("function")?;
                    // try: args = json.loads(fn.arguments or "{}")
                    // except Exception: args = {}
                    let parsed = (|| {
                        let loads = g(py, "json")?.getattr("loads")?;
                        loads.call1((or(call.getattr("arguments")?, || Ok(lit(py, "{}")))?,))
                    })();
                    let mut arguments = match parsed {
                        Ok(parsed) => parsed,
                        Err(e) => except_exception(py, e, |_| Ok(empty_dict(py)))?,
                    };
                    if !isinstance(&arguments, "dict")? {
                        arguments = empty_dict(py);
                    }
                    // "id": str(getattr(tc, "id", "") or "") or f"call_{i}"
                    let to_str = g(py, "str")?;
                    let read = g(py, "getattr")?;
                    let raw = or(read.call1((&tc, "id", ""))?, || Ok(lit(py, "")))?;
                    let id = or(to_str.call1((raw,))?, || Fmt::new(py).s("call_").v(&i)?.done())?;
                    let name = or(call.getattr("name")?, || Ok(lit(py, "")))?;
                    calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &arguments)])?)?;
                }
                message.set_item("tool_calls", calls)?;
            }
            // usage = getattr(response, "usage", None)
            let usage = g(py, "getattr")?.call1((&response, "usage", none(py)))?;
            let choices = list1(dict(py, &[("message", &message)])?)?;
            let counts = if usage.is_truthy()? {
                // getattr(usage, <field>, 0) or 0
                let count = |field: &str| -> PyResult<Obj<'py>> {
                    let read = g(py, "getattr")?;
                    or(read.call1((&usage, field, 0i32))?, || int(py, 0))
                };
                let input = count("prompt_tokens")?;
                let output = count("completion_tokens")?;
                let total = count("total_tokens")?;
                dict(py, &[("input_tokens", &input), ("output_tokens", &output), ("total_tokens", &total)])?
            } else {
                empty_dict(py)
            };
            dict(py, &[("choices", &choices), ("usage", &counts)])
        })();
        match tried {
            Ok(result) => Ok(result),
            // except Exception as e:
            //     error_msg = f"OpenAI API request failed: {str(e)}"
            //     raise Exception(error_msg)
            Err(e) => except_exception(py, e, |e| {
                let text = g(py, "str")?.call1((e.value(py),))?;
                let error_msg = Fmt::new(py).s("OpenAI API request failed: ").v(&text)?.done()?;
                Err(throw(py, "Exception", || Ok(error_msg)))
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

/// Runs the lines of service.py after its import lines and binds what they
/// make on the module: the class. The loader calls it once, right after it
/// binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // class OpenAIProvider:
    store(m, "OpenAIProvider", py.get_type::<OpenAIProvider>())?;
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
    let class = py.get_type::<OpenAIProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
