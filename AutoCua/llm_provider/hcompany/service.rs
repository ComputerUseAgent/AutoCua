//! service: the H Company provider (HCompanyProvider), which sends the
//! OpenAI-format chat-completions request to H Company's Models API (the Holo
//! models, https://api.hcompany.ai/v1) and, when tools are on, folds the
//! reply's tool calls into the shape the loops read.
//!
//! Python imports it as AutoCua.llm_provider.hcompany.service
//! (llm_provider/hcompany/__init__.py points that name here). It has the
//! shape of the other requests-based providers (llm_provider/groq/service.rs),
//! which are the Rust copies of a service.py: this provider never had one.
//! As in theirs, Python's own rules decide every result, so they are called,
//! not imitated: json, the builtins, the str, dict and list methods, the
//! operators, the comparisons and the f-strings.
//!
//! Two steps, as theirs: register() gives the module _build. Then the loader
//! binds the names this file reads as module globals (json, time, requests,
//! entry_value, http_timeout, out_cap, screenshots) and calls _build(), which
//! makes the class, then the function.
//!
//! Every global a line reads (json, time, requests, entry_value, http_timeout,
//! out_cap, screenshots, _normalize_tool_response, len, min, str, float,
//! print, isinstance, enumerate, ...) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces one
//! is obeyed. The provider keeps its attributes in its __dict__, and its
//! methods reach them through the instance, so a subclass or a patched
//! attribute (the session) is obeyed. An error is raised the way Python's
//! raise statement raises it, so the error being handled at that moment
//! becomes its __context__.
//!
//! What is H Company's own here, each from its docs (hub.hcompany.ai/models-api)
//! or from a request sent to the API on 2026-11-27:
//! - a screenshot goes in "<observation>" tags, the way Holo is trained to
//!   get one, and a request holds 5 images at most;
//! - the screenshots of the two steps before go out again with every
//!   request, each in front of the answer the model gave on it: the docs say
//!   to keep at most the last 3 in context. The loops keep only the current
//!   one, so this provider keeps the two before it by itself (past_shots);
//! - the output cap never passes the model's own (the table's max_output);
//! - temperature is H Company's figure for the kind of call;
//! - a 429 (over the tier's requests a minute: 10 on a free account) is
//!   waited out for the seconds its retry-after header gives, and asked again.
//! Nothing is sent for parallel tool calls: the API's field list has none,
//! and holo3-1-35b-a3b gave several calls in one answer in 6 of 8 requests
//! without the field and in 2 of 8 with it.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::controller::{self, caught, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// HCompanyProvider's docstring.
const CLASS_DOC: &str = "H Company API provider for LLM interactions (OpenAI-compatible chat completions)";

/// The class's __firstlineno__ from Python 3.13, which the other providers'
/// classes have from their service.py: the line theirs is on.
const CLASS_LINE: i32 = 7;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 8] =
    ["api_key", "api_url", "cli_agent", "max_tokens", "model_info", "past_shots", "session", "tools"];

/// Where every request goes.
const API_URL: &str = "https://api.hcompany.ai/v1/chat/completions";

/// The images one request may hold: the API takes 5 (JPEG, PNG or WebP).
const MAX_IMAGES: usize = 5;

/// The screenshots of a run one request holds: this step's own and those of
/// the two steps before it, a window that slides with the run. The docs
/// (hub.hcompany.ai/models-api/build-an-agent/core-concepts): "Keep at most
/// the last 3 screenshots in context: more degrades accuracy."
const SHOTS_IN_MEMORY: usize = 3;

/// What an earlier screenshot is told to be, in front of its <observation>,
/// and this step's own when earlier ones are in the request with it. The
/// boxes drawn on an earlier one carry the [id]s of its own scan, which the
/// next scan gave to other elements.
const EARLIER_SCREEN: &str =
    "Earlier screen: what you saw before the step that follows. Its [id] labels are out of date.\n<observation>\n";
const CURRENT_SCREEN: &str = "Current screen: act on this one.\n<observation>\n";

/// The id _normalize_tool_response gives a reply's first call when the API
/// sent none: it is the same in every reply, so it names no step.
const NO_CALL_ID: &str = "call_0";

/// The output cap of every agent but the coder, when the caller gives none.
const STEP_MAX_TOKENS: i64 = 8192;

/// The most a model may write when its table entry does not say
/// (max_output): the smallest cap H Company serves, the Holo3 models'.
const SMALLEST_MAX_OUTPUT: i64 = 8192;

/// How many times one request waits out a 429 before the 429 is the answer.
const RATE_LIMIT_WAITS: u32 = 3;

/// The seconds one such wait lasts when the 429 does not say (its
/// retry-after header said 6 when it was seen), and the bounds on what it says.
const RATE_LIMIT_WAIT: f64 = 6.0;
const RATE_LIMIT_WAIT_BOUNDS: (f64, f64) = (1.0, 60.0);

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.hcompany.service", super::super::NATIVE)).map(Bound::unbind))
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

/// A str tuple of names.
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

// ---------------------------------------------------------------------------
// HCompanyProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// H Company API provider for LLM interactions (its docstring is CLASS_DOC,
// set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "HCompanyProvider")]
pub struct HCompanyProvider;

#[pymethods]
impl HCompanyProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key, cli_agent=False, model_info=None, tools=None, max_tokens=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        HCompanyProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["api_key", "cli_agent", "model_info", "tools", "max_tokens"];
        let slots = bind("HCompanyProvider.__init__", 1, args, kwargs, &params, 1)?;
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
        // The screenshots of the last steps, oldest first, each with the id
        // of the first call the model made on it: [(call_id, base64 JPEG)].
        // send_request puts them back at their steps (see with_earlier_shots).
        // self.past_shots = []
        me.setattr("past_shots", PyList::empty(py))?;
        Ok(())
    }

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to H Company API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("HCompanyProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // If screenshot is provided and NOT cli_agent, modify the user message to include the annotated image
        // if annotated_screenshot_base64 and not self.cli_agent and len(messages) > 1:
        let several = || -> PyResult<bool> {
            let count = g(py, "len")?.call1((&messages,))?;
            count.rich_compare(1, CompareOp::Gt)?.is_truthy()
        };
        // What goes out: `messages`, or a copy of the list with the earlier
        // screenshots put back. And this step's own screenshot, kept for the
        // steps that follow once the model has answered it.
        let mut sent = messages.clone();
        let mut own_shot: Option<Obj<'py>> = None;
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && several()? {
            let user_message = messages.get_item(-1)?.get_item("content")?;
            // The step's text, then each screenshot the way Holo is trained to
            // get one (hub.hcompany.ai/models-api/build-an-agent/core-concepts:
            // "send the current screenshot ... wrapped in <observation> tags"),
            // and MAX_IMAGES of them at most: the page's own comes first.
            // [{"type": "text", "text": user_message}] + [part
            //     for shot in screenshots(annotated_screenshot_base64)[:MAX_IMAGES]
            //     for part in ({"type": "text", "text": "<observation>\n"},
            //                  {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{shot}"}},
            //                  {"type": "text", "text": "\n</observation>"})]
            let text = list1(dict(py, &[("type", &lit(py, "text")), ("text", &user_message)])?)?;
            let shots = g(py, "screenshots")?.call1((&shot,))?;
            let mut own: Vec<Obj<'py>> = Vec::new();
            for one in comprehension(&shots)?.take(MAX_IMAGES) {
                own.push(one?);
            }
            // The screenshots of the steps before, back at their steps: as
            // many as SHOTS_IN_MEMORY and the API's MAX_IMAGES leave room for
            // beside this step's own.
            // sent, earlier = self._with_earlier_shots(messages, room)
            let room = (SHOTS_IN_MEMORY - 1).min(MAX_IMAGES - own.len());
            let (with_earlier, earlier) = with_earlier_shots(me, &messages, room)?;
            let images = PyList::empty(py);
            for (n, one) in own.iter().enumerate() {
                let url = Fmt::new(py).s("data:image/jpeg;base64,").v(one)?.done()?;
                let image_url = dict(py, &[("url", &url)])?;
                // With earlier ones in the request, the first of this step's
                // own is told to be the current screen.
                let opening = if earlier > 0 && n == 0 { CURRENT_SCREEN } else { "<observation>\n" };
                images.append(dict(py, &[("type", &lit(py, "text")), ("text", &lit(py, opening))])?)?;
                images.append(dict(py, &[("type", &lit(py, "image_url")), ("image_url", &image_url)])?)?;
                images.append(dict(py, &[("type", &lit(py, "text")), ("text", &lit(py, "\n</observation>"))])?)?;
            }
            let content = text.add(&images)?;
            messages.get_item(-1)?.set_item("content", content)?;
            sent = with_earlier;
            own_shot = own.into_iter().next();
        }

        // headers = {"Authorization": f"Bearer {self.api_key}", "Content-Type": "application/json"}
        let auth = Fmt::new(py).s("Bearer ").v(&attr(me, "api_key")?)?.done()?;
        let headers = dict(py, &[("Authorization", &auth), ("Content-Type", &lit(py, "application/json"))])?;

        // Output cap. The caller's own (the handoff manager passes 10k,
        // LLMManager.max_tokens), else STEP_MAX_TOKENS, and for the coder its
        // table entry's coder_max_tokens, else 32768 (out_cap). Never above
        // what the API lets this model write, the table's max_output (the
        // docs: a request for more is a 400); thinking is counted in it.
        // wanted = self.max_tokens or out_cap(self.cli_agent, STEP_MAX_TOKENS, self.model_info)
        // "max_tokens": min(wanted, self.model_info.get("max_output") or SMALLEST_MAX_OUTPUT)
        let wanted = or(attr(me, "max_tokens")?, || {
            let out_cap = g(py, "out_cap")?;
            let cli_agent = attr(me, "cli_agent")?;
            let model_info = attr(me, "model_info")?;
            out_cap.call1((cli_agent, STEP_MAX_TOKENS, model_info))
        })?;
        let most = or(get(&attr(me, "model_info")?, "max_output")?, || {
            Ok(SMALLEST_MAX_OUTPUT.into_pyobject(py)?.into_any())
        })?;
        let max_tokens = g(py, "min")?.call1((wanted, most))?;
        // H Company's own figures (hub.hcompany.ai/models-api/overview: "0.0
        // for single calls, 0.8 in agent loops"): with tools this is a step
        // of an agent's loop; with none (mode="text", the memory handoff) it
        // is one call that stands alone.
        // "temperature": 0.8 if self.tools else 0.0
        let looping = attr(me, "tools")?.is_truthy()?;
        let temperature = PyFloat::new(py, if looping { 0.8 } else { 0.0 }).into_any();
        let data = dict(
            py,
            &[("model", &model), ("messages", &sent), ("temperature", &temperature), ("max_tokens", &max_tokens)],
        )?;
        // The table's level for this caller (the coder reads coder_effort
        // when the entry has one); with none, the API's own default:
        // thinking on. The thinking comes back apart from the answer
        // (message.reasoning), is not read here and never goes back.
        // effort = entry_value(self.model_info, "effort", self.cli_agent)
        let entry_value = g(py, "entry_value")?;
        let model_info = attr(me, "model_info")?;
        let cli_agent = attr(me, "cli_agent")?;
        let effort = entry_value.call1((model_info, "effort", cli_agent))?;
        if effort.is_truthy()? {
            data.set_item("reasoning_effort", &effort)?;
        }

        // No tools (mode="text") -> plain text: omit the tool params entirely.
        if attr(me, "tools")?.is_truthy()? {
            data.set_item("tools", attr(me, "tools")?)?;
            // "required" - every turn must call at least one tool; a text-only
            // turn is never a valid step (even termination is the `exit`
            // tool). Canonical rationale: see openrouter/service.py.
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
            // Over the tier's requests a minute the API answers 429 and says
            // in retry-after how many seconds to wait (a free account has 10
            // requests a minute, which a loop that steps every few seconds
            // passes). The request waits that long and is sent again, at most
            // RATE_LIMIT_WAITS times, so the run slows down instead of ending.
            // for waits in range(RATE_LIMIT_WAITS + 1):
            //     response = self.session.post(...)
            //     if response.status_code != 429 or waits == RATE_LIMIT_WAITS:
            //         break
            //     seconds = <retry-after, see retry_after>
            //     print(f"H Company: ...; waiting {seconds} s", flush=True)
            //     time.sleep(seconds)
            let mut waits = 0;
            let response = loop {
                let response = post.call((&url,), Some(&kw))?;
                let limited = response.getattr("status_code")?.rich_compare(429, CompareOp::Eq)?.is_truthy()?;
                if !limited || waits == RATE_LIMIT_WAITS {
                    break response;
                }
                waits += 1;
                let seconds = retry_after(&response)?;
                let said = format!(
                    "H Company: over the requests a minute this account may send; waiting {seconds:.0} s"
                );
                let flush = PyDict::new(py);
                flush.set_item("flush", true)?;
                g(py, "print")?.call((said,), Some(&flush))?;
                g(py, "time")?.call_method1("sleep", (seconds,))?;
            };
            response.call_method0("raise_for_status")?;
            let result = response.call_method0("json")?;
            if attr(me, "tools")?.is_truthy()? {
                let reply = g(py, "_normalize_tool_response")?.call1((result,))?;
                // The model answered this screenshot: it is kept for the
                // steps that follow, under the id of the answer's first call.
                // self._remember_shot(reply, own_shot)
                if let Some(image) = &own_shot {
                    remember_shot(me, &reply, image)?;
                }
                return Ok(reply);
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
                    // error_msg = f"H Company API request failed: {str(e)}"
                    let text = g(py, "str")?.call1((&error,))?;
                    let mut error_msg = Fmt::new(py).s("H Company API request failed: ").v(&text)?.done()?;
                    // if hasattr(e, 'response') and e.response is not None:
                    if g(py, "hasattr")?.call1((&error, "response"))?.is_truthy()? && !error.getattr("response")?.is_none() {
                        // error_msg += f"\nResponse: {e.response.text}"
                        let text = error.getattr("response")?.getattr("text")?;
                        error_msg = error_msg.add(Fmt::new(py).s("\nResponse: ").v(&text)?.done()?)?;
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
// The functions
// ---------------------------------------------------------------------------

/// Where in `messages` the model's answer is that holds the call `id`: the
/// place of that assistant message, or None when the history has no such
/// answer (it was never kept, or a compression or a new chat took it away).
/// for at, message in enumerate(messages):
///     if message.get("role") == "assistant" and any(
///             (call or {}).get("id") == id for call in message.get("tool_calls") or []):
///         return at
/// return None
fn turn_of_call<'py>(messages: &Obj<'py>, id: &Obj<'py>) -> PyResult<Option<usize>> {
    let py = messages.py();
    for (at, message) in messages.try_iter()?.enumerate() {
        let message = message?;
        if !isinstance(&message, "dict")? || !get(&message, "role")?.eq("assistant")? {
            continue;
        }
        let calls = or(get(&message, "tool_calls")?, || Ok(PyList::empty(py).into_any()))?;
        for call in calls.try_iter()? {
            let call = call?;
            if isinstance(&call, "dict")? && get(&call, "id")?.eq(id)? {
                return Ok(Some(at));
            }
        }
    }
    Ok(None)
}

/// An earlier screenshot as a turn of the history: a user message the way
/// Holo is trained to get an observation, told to be an earlier screen.
/// {"role": "user", "content": [{"type": "text", "text": EARLIER_SCREEN},
///     {"type": "image_url", "image_url": {"url": f"data:image/jpeg;base64,{image}"}},
///     {"type": "text", "text": "\n</observation>"}]}
fn earlier_turn<'py>(image: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = image.py();
    let url = Fmt::new(py).s("data:image/jpeg;base64,").v(image)?.done()?;
    let image_url = dict(py, &[("url", &url)])?;
    let parts = PyList::empty(py);
    parts.append(dict(py, &[("type", &lit(py, "text")), ("text", &lit(py, EARLIER_SCREEN))])?)?;
    parts.append(dict(py, &[("type", &lit(py, "image_url")), ("image_url", &image_url)])?)?;
    parts.append(dict(py, &[("type", &lit(py, "text")), ("text", &lit(py, "\n</observation>"))])?)?;
    dict(py, &[("role", &lit(py, "user")), ("content", parts.as_any())])
}

/// The messages to send, with the screenshots of the steps before put back,
/// and how many were put back. Each goes in front of the answer the model
/// gave on it, so the history reads as Holo's own loop does: the screen, what
/// the model did on it, what came of it, the next screen. `messages` itself
/// is left as it is: the loop keeps it, and sends it again when a request
/// fails.
///
/// This is also where the window slides. A kept screenshot whose step is
/// not in the history is forgotten first (the loop threw that answer away,
/// or a compression or a new chat took the step). Of the rest the newest
/// SHOTS_IN_MEMORY - 1 stay, and the newest `room` of those are put back.
fn with_earlier_shots<'py>(me: &Obj<'py>, messages: &Obj<'py>, room: usize) -> PyResult<(Obj<'py>, usize)> {
    let py = me.py();
    let mut found: Vec<(usize, Obj<'py>, Obj<'py>)> = Vec::new();
    for kept in attr(me, "past_shots")?.try_iter()? {
        let kept = kept?;
        if let Some(at) = turn_of_call(messages, &kept.get_item(0)?)? {
            found.push((at, kept.get_item(1)?, kept));
        }
    }
    let found = &found[found.len().saturating_sub(SHOTS_IN_MEMORY - 1)..];
    let still = PyList::empty(py);
    for (_, _, kept) in found {
        still.append(kept)?;
    }
    me.setattr("past_shots", &still)?;
    let places: Vec<(usize, &Obj<'py>)> =
        found[found.len().saturating_sub(room)..].iter().map(|(at, image, _)| (*at, image)).collect();
    if places.is_empty() {
        return Ok((messages.clone(), 0));
    }
    let sent = PyList::empty(py);
    for (at, message) in messages.try_iter()?.enumerate() {
        for (place, image) in &places {
            if *place == at {
                sent.append(earlier_turn(image)?)?;
            }
        }
        sent.append(message?)?;
    }
    Ok((sent.into_any(), places.len()))
}

/// Keep the screenshot the model just answered, for the steps that follow:
/// under the id of the answer's first call, which the loop sends back in the
/// history. An answer with no call, or with no id of its own, names no step
/// and its screenshot is not kept. Nothing is dropped here: whether the loop
/// keeps this answer is only known at the next request, where the window
/// slides (with_earlier_shots). So at most SHOTS_IN_MEMORY are held.
/// calls = reply["choices"][0]["message"]["tool_calls"]
/// if calls and calls[0]["id"] != NO_CALL_ID:
///     self.past_shots.append((calls[0]["id"], image))
fn remember_shot<'py>(me: &Obj<'py>, reply: &Obj<'py>, image: &Obj<'py>) -> PyResult<()> {
    let py = me.py();
    let calls = reply.get_item("choices")?.get_item(0)?.get_item("message")?.get_item("tool_calls")?;
    if !calls.is_truthy()? {
        return Ok(());
    }
    let id = calls.get_item(0)?.get_item("id")?;
    if id.eq(NO_CALL_ID)? {
        return Ok(());
    }
    attr(me, "past_shots")?.call_method1("append", (PyTuple::new(py, [id, image.clone()])?,))?;
    Ok(())
}

/// The seconds a 429 says to wait: its retry-after header as a number, kept
/// within RATE_LIMIT_WAIT_BOUNDS; RATE_LIMIT_WAIT when it has none or one
/// that is no number.
/// try: seconds = float(response.headers.get("retry-after"))
/// except Exception: seconds = RATE_LIMIT_WAIT
fn retry_after(response: &Obj<'_>) -> PyResult<f64> {
    let py = response.py();
    let said = (|| -> PyResult<f64> {
        let header = response.getattr("headers")?.call_method1("get", ("retry-after",))?;
        g(py, "float")?.call1((header,))?.extract()
    })();
    let (least, most) = RATE_LIMIT_WAIT_BOUNDS;
    match said {
        Ok(seconds) if seconds.is_finite() => Ok(seconds.clamp(least, most)),
        Ok(_) => Ok(RATE_LIMIT_WAIT),
        Err(e) => except_exception(py, e, |_| Ok(RATE_LIMIT_WAIT)),
    }
}

#[pyfunction]
#[pyo3(name = "_normalize_tool_response")]
#[doc = "Normalize an OpenAI-format tool-call response: keep the usual\n\
choices/message shape but replace tool_calls with\n\
[{\"id\", \"name\", \"arguments\": dict}] (arguments JSON-decoded). The `id` is\n\
echoed back by the loop so each tool result matches its call."]
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
    // {"choices": [{"message": {"content": message.get("content") or "", "tool_calls": calls}}],
    //  "usage": result.get("usage", {})}
    let content = or(get(&message, "content")?, || Ok(lit(py, "")))?;
    let reply = dict(py, &[("content", &content), ("tool_calls", calls.as_any())])?;
    let choices = list1(dict(py, &[("message", &reply)])?)?;
    let usage = get_or(&result, "usage", empty_dict(py))?;
    dict(py, &[("choices", &choices), ("usage", &usage)])
}

// ---------------------------------------------------------------------------
// What the loader runs, once it has bound the imported names
// ---------------------------------------------------------------------------

/// `key = value` on the module: its own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Makes the class, then the function, and binds each on the module. The
/// loader calls it once, right after it binds the imported names, and then
/// deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // class HCompanyProvider:
    store(m, "HCompanyProvider", py.get_type::<HCompanyProvider>())?;
    store(m, "_normalize_tool_response", wrap_pyfunction!(normalize_tool_response, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module with _build, which the loader calls once it has bound
/// the imported names (see the top of this file), and makes the class look
/// like the other providers': its docstring and, from Python 3.13, what a
/// class statement gives a class. Like theirs, the module has no docstring.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<HCompanyProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
