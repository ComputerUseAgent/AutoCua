//! service: the Perplexity provider (PerplexityProvider), which sends the
//! Responses-style request to Perplexity's Agent API (or, for a model the
//! table routes there, the shared Responses sender) and, when tools are on,
//! folds the reply's function calls into the shape the loops read.
//!
//! The Rust copy of llm_provider/perplexity/service.py. Python imports it
//! under its old dotted name, AutoCua.llm_provider.perplexity.service
//! (llm_provider/perplexity/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values and
//! errors. Python's own rules decide every result, so they are called, not
//! imitated: json, the builtins, the str, dict and list methods, the
//! operators, the comparisons and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (json, requests, Dict, Any, Optional, the
//! view's get_model_info and get_reasoning_params, the Responses module's
//! async_route and send_responses, http_timeout, out_cap, screenshots) and
//! calls _build(), which runs the rest of service.py's lines: the class.
//!
//! Every global a line reads (json, requests, get_reasoning_params,
//! async_route, send_responses, http_timeout, out_cap, screenshots, len,
//! str, isinstance, hasattr, Exception, ...) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces one
//! is obeyed. The view's names are this module's own, bound from view when
//! it was imported. The provider keeps its attributes in its __dict__, and
//! its methods reach them (and _extract_text) through the instance, so a
//! subclass or a patched attribute (the session) is obeyed. An error is
//! raised the way Python's raise statement raises it, so the error being
//! handled at that moment becomes its __context__.

use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyList, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// PerplexityProvider's docstring.
const CLASS_DOC: &str = "Perplexity Agent API provider for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 10;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 7] = ["api_key", "api_url", "cli_agent", "max_tokens", "model_info", "session", "tools"];

/// Where every request goes, the Responses route included.
const API_URL: &str = "https://api.perplexity.ai/v1/agent";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.perplexity.service", super::super::NATIVE)).map(Bound::unbind))
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
// PerplexityProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

/// `self._extract_text(<content>)`: the method read through the instance
/// first, then `content()` evaluated, then the call, in Python's order.
fn extract<'py>(me: &Obj<'py>, content: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    let method = attr(me, "_extract_text")?;
    method.call1((content()?,))
}

// Perplexity Agent API provider for LLM interactions (its docstring is
// CLASS_DOC, set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "PerplexityProvider")]
pub struct PerplexityProvider;

#[pymethods]
impl PerplexityProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, model_info=None, tools=None, max_tokens=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        PerplexityProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "model_info", "tools", "max_tokens"];
        let slots = bind("PerplexityProvider.__init__", 1, args, kwargs, &params, 1)?;
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
        // Native tool calling: Responses-style flat function tools ({type,
        // name, description, parameters}) — the model's output contract.
        // None only for mode="text" (plain prose).
        me.setattr("tools", or(tools, || Ok(none(py)))?)?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to Perplexity Agent API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("PerplexityProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // GPT-6 Astra (table flag `async_tools`): the same Responses-style
        // request this provider already speaks, but with the tools declared
        // async so the model emits the notes call and the batch in one turn
        // (see ..responses). Probed live 2026-10-01: Perplexity strips the
        // `async` flag, so no entry in view.py carries `async_tools`.
        // if self.tools and async_route(self.model_info):
        let routed = attr(me, "tools")?.is_truthy()? && {
            let async_route = g(py, "async_route")?;
            async_route.call1((attr(me, "model_info")?,))?.is_truthy()?
        };
        if routed {
            // return send_responses(session=self.session, url=self.api_url, ...),
            // the function read first, then each argument in its order.
            let send = g(py, "send_responses")?;
            let kw = PyDict::new(py);
            kw.set_item("session", attr(me, "session")?)?;
            kw.set_item("url", attr(me, "api_url")?)?;
            kw.set_item("api_key", attr(me, "api_key")?)?;
            kw.set_item("error_prefix", "Perplexity API request failed")?;
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
            kw.set_item("extra", empty_dict(py))?;
            // provider_meta is stripped before every send on this provider
            // (_META_PROVIDERS): the replay reconstructs the calls.
            kw.set_item("provider_tag", none(py))?;
            return send.call((), Some(&kw));
        }

        // Separate system prompt from conversation messages
        let mut instructions = none(py);
        let input_messages = PyList::empty(py);

        // for msg in messages:
        for msg in messages.try_iter()? {
            let msg = msg?;
            if eq(&msg.get_item("role")?, "system")? {
                let content = msg.get_item("content")?;
                if isinstance(&content, "list")? {
                    for item in content.try_iter()? {
                        let item = item?;
                        if isinstance(&item, "dict")? && eq(&get(&item, "type")?, "text")? {
                            instructions = get_or(&item, "text", lit(py, ""))?;
                            break;
                        }
                    }
                } else {
                    instructions = content;
                }
            } else if eq(&msg.get_item("role")?, "tool")? {
                // NATIVE TRANSCRIPT: the Responses dialect carries a tool
                // result as its own flat output item keyed by call_id.
                // {"type": "function_call_output",
                //  "call_id": msg.get("tool_call_id") or "",
                //  "output": self._extract_text(msg.get("content"))}
                let call_id = or(get(&msg, "tool_call_id")?, || Ok(lit(py, "")))?;
                let output = extract(me, || get(&msg, "content"))?;
                let item = dict(
                    py,
                    &[("type", &lit(py, "function_call_output")), ("call_id", &call_id), ("output", &output)],
                )?;
                input_messages.append(item)?;
            } else if eq(&msg.get_item("role")?, "assistant")? {
                // text = self._extract_text(msg["content"])
                let text = extract(me, || msg.get_item("content"))?;
                if text.call_method0("strip")?.is_truthy()? {
                    let part = dict(py, &[("type", &lit(py, "output_text")), ("text", &text)])?;
                    input_messages.append(dict(py, &[("role", &lit(py, "assistant")), ("content", &list1(part)?)])?)?;
                }
                // ...and one function_call item per tool call the model made,
                // so it sees its OWN calls replayed in the canonical shape.
                // for tc in (msg.get("tool_calls") or []):
                for tc in or(get(&msg, "tool_calls")?, || Ok(empty_list(py)))?.try_iter()? {
                    let tc = tc?;
                    // fn = (tc or {}).get("function") or {}
                    let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
                    // raw = fn.get("arguments")
                    let raw = get(&call, "arguments")?;
                    // {"type": "function_call",
                    //  "call_id": (tc or {}).get("id") or "",
                    //  "name": fn.get("name") or "",
                    //  "arguments": raw if isinstance(raw, str) else json.dumps(raw or {})}
                    let call_id = or(get(&or(tc, || Ok(empty_dict(py)))?, "id")?, || Ok(lit(py, "")))?;
                    let name = or(get(&call, "name")?, || Ok(lit(py, "")))?;
                    let arguments = if isinstance(&raw, "str")? {
                        raw
                    } else {
                        let dumps = g(py, "json")?.getattr("dumps")?;
                        dumps.call1((or(raw, || Ok(empty_dict(py)))?,))?
                    };
                    let item = dict(
                        py,
                        &[
                            ("type", &lit(py, "function_call")),
                            ("call_id", &call_id),
                            ("name", &name),
                            ("arguments", &arguments),
                        ],
                    )?;
                    input_messages.append(item)?;
                }
            } else if eq(&msg.get_item("role")?, "user")? {
                // text = self._extract_text(msg["content"])
                let text = extract(me, || msg.get_item("content"))?;
                let part = dict(py, &[("type", &lit(py, "input_text")), ("text", &text)])?;
                input_messages.append(dict(py, &[("role", &lit(py, "user")), ("content", &list1(part)?)])?)?;
            }
        }

        // Add screenshot to last user message if provided and NOT cli_agent
        // if annotated_screenshot_base64 and not self.cli_agent and len(input_messages) > 0:
        let some = || -> PyResult<bool> {
            let count = g(py, "len")?.call1((&input_messages,))?;
            count.rich_compare(0, CompareOp::Gt)?.is_truthy()
        };
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && some()? {
            let last = input_messages.as_any().get_item(-1)?;
            // `last` may be a flat function_call* item with no "role" key.
            if eq(&get(&last, "role")?, "user")? {
                // for shot in screenshots(annotated_screenshot_base64):
                let shots = g(py, "screenshots")?.call1((&shot,))?;
                for one in shots.try_iter()? {
                    let one = one?;
                    // last["content"].append({"type": "input_image",
                    //     "image_url": f"data:image/jpeg;base64,{shot}"})
                    let append = last.get_item("content")?.getattr("append")?;
                    let url = Fmt::new(py).s("data:image/jpeg;base64,").v(&one)?.done()?;
                    append.call1((dict(py, &[("type", &lit(py, "input_image")), ("image_url", &url)])?,))?;
                }
            }
        }

        // headers = {"Authorization": f"Bearer {self.api_key}", "Content-Type": "application/json"}
        let auth = Fmt::new(py).s("Bearer ").v(&attr(me, "api_key")?)?.done()?;
        let headers = dict(py, &[("Authorization", &auth), ("Content-Type", &lit(py, "application/json"))])?;

        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default.
        // "max_output_tokens": self.max_tokens or out_cap(self.cli_agent, 10000, self.model_info)
        let max_tokens = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            let cli_agent = attr(me, "cli_agent")?;
            let model_info = attr(me, "model_info")?;
            out_cap.call1((cli_agent, 10000, model_info))
        })?;
        let data = dict(
            py,
            &[("model", &model), ("input", input_messages.as_any()), ("max_output_tokens", &max_tokens)],
        )?;

        // How hard the model thinks, per view.py's per-model table, carried as
        // the Agent API's reasoning config. Omitted for a hand-typed model
        // name, which then keeps Perplexity's own default.
        // data.update(get_reasoning_params(model, self.model_info, self.cli_agent))
        let update = data.getattr("update")?;
        let levels = g(py, "get_reasoning_params")?;
        update.call1((levels.call1((&model, attr(me, "model_info")?, attr(me, "cli_agent")?))?,))?;

        if instructions.is_truthy()? {
            data.set_item("instructions", &instructions)?;
        }

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            data.set_item("tools", attr(me, "tools")?)?;
            // "required" - every turn must call at least one tool; a text-only
            // turn is never a valid step (even termination is the `exit`
            // tool). Responses-dialect value; if Perplexity's validator ever
            // rejects it, the request 400s loudly - flip back to "auto".
            // Canonical rationale: see openrouter/service.py.
            // data["tool_choice"] = self.model_info.get("tool_choice") or "required"
            let picked = get(&attr(me, "model_info")?, "tool_choice")?;
            data.set_item("tool_choice", or(picked, || Ok(lit(py, "required")))?)?;
        }

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

            // Normalize to choices[0].message.content format
            let mut text_content = lit(py, "");
            let calls = PyList::empty(py);
            // for output_item in result.get("output", []):
            for output_item in get_or(&result, "output", empty_list(py))?.try_iter()? {
                let output_item = output_item?;
                if eq(&get(&output_item, "type")?, "message")? {
                    // for content_block in output_item.get("content", []):
                    for content_block in get_or(&output_item, "content", empty_list(py))?.try_iter()? {
                        let content_block = content_block?;
                        if eq(&get(&content_block, "type")?, "output_text")? && !text_content.is_truthy()? {
                            text_content = get_or(&content_block, "text", lit(py, ""))?;
                        }
                    }
                } else if attr(me, "tools")?.is_truthy()? && eq(&get(&output_item, "type")?, "function_call")? {
                    // Responses dialect: arguments is a JSON string
                    // (malformed -> {}).
                    let raw_args = get(&output_item, "arguments")?;
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
                    //     "id": str(output_item.get("call_id") or output_item.get("id") or "")
                    //           or f"call_{len(calls)}",
                    //     "name": output_item.get("name") or "",
                    //     "arguments": args,
                    // })
                    let to_str = g(py, "str")?;
                    let raw_id = or(get(&output_item, "call_id")?, || {
                        or(get(&output_item, "id")?, || Ok(lit(py, "")))
                    })?;
                    let id = or(to_str.call1((raw_id,))?, || {
                        let count = g(py, "len")?.call1((&calls,))?;
                        Fmt::new(py).s("call_").v(&count)?.done()
                    })?;
                    let name = or(get(&output_item, "name")?, || Ok(lit(py, "")))?;
                    calls.append(dict(py, &[("id", &id), ("name", &name), ("arguments", &args)])?)?;
                }
            }

            // message: Dict[str, Any] = {"content": text_content}
            let message = dict(py, &[("content", &text_content)])?;
            if attr(me, "tools")?.is_truthy()? {
                message.set_item("tool_calls", &calls)?;
            }
            // return {"choices": [{"message": message}], "usage": result.get("usage", {})}
            let choices = list1(dict(py, &[("message", &message)])?)?;
            let usage = get_or(&result, "usage", empty_dict(py))?;
            dict(py, &[("choices", &choices), ("usage", &usage)])
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
                    // error_msg = f"Perplexity API request failed: {str(e)}"
                    let text = g(py, "str")?.call1((&error,))?;
                    let mut error_msg = Fmt::new(py).s("Perplexity API request failed: ").v(&text)?.done()?;
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

    #[staticmethod]
    #[pyo3(name = "_extract_text")]
    #[doc = "Extract text from message content (string or list format with cache_control)"]
    fn extract_text(content: Obj<'_>) -> PyResult<Obj<'_>> {
        let py = content.py();
        if isinstance(&content, "str")? {
            return Ok(content);
        }
        if isinstance(&content, "list")? {
            for item in content.try_iter()? {
                let item = item?;
                if isinstance(&item, "dict")? && eq(&get(&item, "type")?, "text")? {
                    return get_or(&item, "text", lit(py, ""));
                }
            }
            return g(py, "str")?.call1((content,));
        }
        g(py, "str")?.call1((content,))
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
    // class PerplexityProvider:
    store(m, "PerplexityProvider", py.get_type::<PerplexityProvider>())?;
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
    let class = py.get_type::<PerplexityProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
