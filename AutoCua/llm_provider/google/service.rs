//! service: the Google provider (GoogleProvider), which translates the
//! canonical transcript into Gemini's contents and parts, sends one request
//! through the google-genai SDK (AI Studio, or Vertex AI for a `-vertex`
//! model) and folds the reply back into the OpenAI-shaped envelope the loops
//! read.
//!
//! The Rust copy of llm_provider/google/service.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.google.service
//! (llm_provider/google/__init__.py points that name here), and finds the
//! same names in the same order, with the same values, return values and
//! errors. Python's own rules decide every result, so they are called, not
//! imitated: base64, json, the SDK's genai and types, the builtins, the str,
//! dict, list and set methods, the operators, the comparisons and the
//! f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what
//! service.py's import lines bound (os, base64, json, Dict, Any, Optional,
//! genai, types, load_dotenv, the view's get_model_info, get_thinking_level
//! and MODEL_MAPPINGS, http_timeout, out_cap, screenshots, tls_verify) and
//! calls _build(), which runs the rest of service.py's lines in their order:
//! load_dotenv(), the six functions and the class. load_dotenv() finds its
//! .env from the loader's frame, google/__init__.py, in the folder
//! service.py was in.
//!
//! Every global a line reads (os, base64, json, genai, types, getattr,
//! get_thinking_level, _clean_schema_for_google, isinstance, str, ...) is
//! read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces one is obeyed. The view's names are this
//! module's own, bound from view when it was imported. The provider keeps
//! its attributes in its __dict__, and its methods reach them through the
//! instance, so a subclass or a patched attribute (the client) is obeyed.
//! An error is raised the way Python's raise statement raises it, so the
//! error being handled at that moment becomes its __context__.

use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyList, PyString, PyTuple};

use super::view::bind;
use crate::computer::{as_python_class, unpack2};
use crate::ios::controller;
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// GoogleProvider's docstring.
const CLASS_DOC: &str = "Google Gemini API provider for LLM interactions";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 102;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 6] = ["cli_agent", "client", "is_vertex", "max_tokens", "tool_mode", "tools"];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.google.service", super::super::NATIVE)).map(Bound::unbind))
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

/// `getattr(value, name, default)`, getattr read from this module now.
fn getattr_or<'py>(value: &Obj<'py>, name: &str, default: Obj<'py>) -> PyResult<Obj<'py>> {
    g(value.py(), "getattr")?.call1((value, name, default))
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

/// `callable(key=value, ...)`: a call with keyword arguments only, given in
/// the call's order.
fn call_kw<'py>(callable: &Obj<'py>, items: &[(&str, &Obj<'py>)]) -> PyResult<Obj<'py>> {
    let py = callable.py();
    let kwargs = PyDict::new(py);
    for (key, value) in items {
        kwargs.set_item(PyString::intern(py, key), value)?;
    }
    callable.call((), Some(&kwargs))
}

/// `types.<name>`: the SDK's types module read from this module now, then
/// the name on it.
fn sdk_type<'py>(py: Python<'py>, name: &str) -> PyResult<Obj<'py>> {
    g(py, "types")?.getattr(name)
}

/// The int constant 0.
fn zero(py: Python<'_>) -> PyResult<Obj<'_>> {
    Ok(0i32.into_pyobject(py)?.into_any())
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
// The functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_args_dict")]
#[doc = "Tool-call arguments as a dict (they ride as a JSON string in the\n\
canonical transcript; malformed -> {})."]
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
#[pyo3(name = "_sig_to_text")]
#[doc = "A thought signature as a JSON-safe string (the SDK hands over bytes)."]
fn sig_to_text<'py>(sig: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = sig.py();
    if isinstance(&sig, "str")? {
        return Ok(sig);
    }
    // try: return base64.b64encode(sig).decode("ascii")
    // except Exception: return ""
    match (|| g(py, "base64")?.getattr("b64encode")?.call1((&sig,))?.call_method1("decode", ("ascii",)))() {
        Ok(text) => Ok(text),
        Err(e) => except_exception(py, e, |_| Ok(lit(py, ""))),
    }
}

#[pyfunction]
#[pyo3(name = "_function_call_part", signature = (*args, **kwargs), text_signature = "(name, args, signature='')")]
#[doc = "One functionCall part, carrying back the thought signature Gemini 3\n\
minted for it. That signature is how the model re-attaches its own call —\n\
and the functionResponse that follows — to the reasoning that produced it;\n\
rebuilding the call without it makes Gemini 3 reject the turn or read the\n\
tool result as if it were empty. Best-effort by design: an SDK build\n\
without the field sends exactly the part it sent before."]
fn function_call_part<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_function_call_part", 0, args, kwargs, &["name", "args", "signature"], 2)?;
    let mut slots = slots.into_iter();
    let name = slots.next().flatten().unwrap_or_else(|| none(py));
    let call_args = slots.next().flatten().unwrap_or_else(|| none(py));
    let signature = slots.next().flatten().unwrap_or_else(|| lit(py, ""));
    // part = types.Part.from_function_call(name=name, args=args)
    let from_function_call = sdk_type(py, "Part")?.getattr("from_function_call")?;
    let part = call_kw(&from_function_call, &[("name", &name), ("args", &call_args)])?;
    if signature.is_truthy()? {
        // try: part.thought_signature = base64.b64decode(signature)
        // except Exception: pass
        let tried = (|| -> PyResult<()> {
            let decoded = g(py, "base64")?.getattr("b64decode")?.call1((&signature,))?;
            part.setattr("thought_signature", decoded)
        })();
        if let Err(e) = tried {
            except_exception(py, e, |_| Ok(()))?;
        }
    }
    Ok(part)
}

#[pyfunction]
#[pyo3(name = "_function_response_part", signature = (*args, **kwargs), text_signature = "(name, output, call_id='')")]
#[doc = "One functionResponse part. Gemini 3 mints an `id` on every functionCall\n\
and maps the result back BY that id, so echo it whenever the model gave us\n\
one — name-only matching (the pre-Gemini-3 contract) is ambiguous the\n\
moment one step calls the same tool twice, which this agent does routinely\n\
(parallel minions, several scratchpad writes). Falls back to the name-only\n\
part for ids we synthesized ourselves and for older SDKs."]
fn function_response_part<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_function_response_part", 0, args, kwargs, &["name", "output", "call_id"], 2)?;
    let mut slots = slots.into_iter();
    let name = slots.next().flatten().unwrap_or_else(|| none(py));
    let output = slots.next().flatten().unwrap_or_else(|| none(py));
    let call_id = slots.next().flatten().unwrap_or_else(|| lit(py, ""));
    if call_id.is_truthy()? {
        // try:
        //     return types.Part(function_response=types.FunctionResponse(
        //         id=call_id, name=name, response={"output": output}))
        // except Exception:
        //     pass
        let tried = (|| -> PyResult<Obj<'py>> {
            let part_class = sdk_type(py, "Part")?;
            let response_class = sdk_type(py, "FunctionResponse")?;
            let response = dict(py, &[("output", &output)])?;
            let made = call_kw(&response_class, &[("id", &call_id), ("name", &name), ("response", &response)])?;
            call_kw(&part_class, &[("function_response", &made)])
        })();
        match tried {
            Ok(part) => return Ok(part),
            Err(e) => except_exception(py, e, |_| Ok(()))?,
        }
    }
    // return types.Part.from_function_response(name=name, response={"output": output})
    let from_function_response = sdk_type(py, "Part")?.getattr("from_function_response")?;
    let response = dict(py, &[("output", &output)])?;
    call_kw(&from_function_response, &[("name", &name), ("response", &response)])
}

#[pyfunction]
#[pyo3(name = "_http_options", signature = (*args, **kwargs), text_signature = "(cli_agent=False)")]
#[doc = "The read timeout (LLM_HTTP_TIMEOUT; CODER_HTTP_TIMEOUT for the coder)\n\
for the Gemini SDK, which takes ONE timeout, in milliseconds. httpx\n\
applies it to the connect and to every read and write (so the connect\n\
bound here is 180 s, or 600 s for the coder, not 15 s), and the SDK also\n\
sends it to Google as the X-Server-Timeout deadline. Without it a server\n\
that goes silent is waited on forever. The read half is the one to use:\n\
the call is not streamed, so nothing arrives until the answer is\n\
complete, and the read bound is the cap on that wait, as for every other\n\
provider. A fresh object per client: genai.Client can write a base URL\n\
into the options it is handed. The SDK builds its httpx clients with a\n\
verify of its own (certifi, or SSL_CERT_FILE), so the Windows probe's\n\
switch to the OS store (tls_verify) has to be handed over here."]
fn http_options<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_http_options", 0, args, kwargs, &["cli_agent"], 0)?;
    let cli_agent = slots.into_iter().next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    // options = {"timeout": http_timeout(cli_agent)[1] * 1000}
    let read = g(py, "http_timeout")?.call1((&cli_agent,))?.get_item(1)?;
    let options = PyDict::new(py);
    options.set_item(PyString::intern(py, "timeout"), read.mul(1000)?)?;
    let verify = g(py, "tls_verify")?.call0()?;
    if !verify.is_none() {
        options.set_item("client_args", dict(py, &[("verify", &verify)])?)?;
        options.set_item("async_client_args", dict(py, &[("verify", &verify)])?)?;
    }
    // return types.HttpOptions(**options)
    sdk_type(py, "HttpOptions")?.call((), Some(&options))
}

#[pyfunction]
#[pyo3(name = "_clean_schema_for_google")]
#[doc = "Recursively remove 'additionalProperties' which Gemini API doesn't support."]
fn clean_schema_for_google<'py>(schema: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = schema.py();
    if !isinstance(&schema, "dict")? {
        return Ok(schema);
    }
    // cleaned = {k: _clean_schema_for_google(v) for k, v in schema.items() if k != "additionalProperties"}
    let cleaned = PyDict::new(py);
    for pair in comprehension(&schema.call_method0("items")?)? {
        let (k, v) = unpack2(&pair?)?;
        if k.rich_compare("additionalProperties", CompareOp::Ne)?.is_truthy()? {
            let value = g(py, "_clean_schema_for_google")?.call1((v,))?;
            cleaned.set_item(k, value)?;
        }
    }
    // for k, v in cleaned.items():
    //     if isinstance(v, list):
    //         cleaned[k] = [_clean_schema_for_google(item) for item in v]
    for pair in cleaned.call_method0("items")?.try_iter()? {
        let (k, v) = unpack2(&pair?)?;
        if isinstance(&v, "list")? {
            let items = PyList::empty(py);
            for item in comprehension(&v)? {
                items.append(g(py, "_clean_schema_for_google")?.call1((item?,))?)?;
            }
            cleaned.set_item(k, items)?;
        }
    }
    Ok(cleaned.into_any())
}

// ---------------------------------------------------------------------------
// GoogleProvider
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

// Google Gemini API provider for LLM interactions (its docstring is
// CLASS_DOC, set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "GoogleProvider")]
pub struct GoogleProvider;

#[pymethods]
impl GoogleProvider {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(api_key=None, cli_agent=False, model=None, vertex_project_id=None, vertex_location=None, tools=None, max_tokens=None, model_info=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        GoogleProvider
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = [
            "api_key",
            "cli_agent",
            "model",
            "vertex_project_id",
            "vertex_location",
            "tools",
            "max_tokens",
            "model_info",
        ];
        let slots = bind("GoogleProvider.__init__", 1, args, kwargs, &params, 0)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
        let model = next().unwrap_or_else(|| none(py));
        let vertex_project_id = next().unwrap_or_else(|| none(py));
        let vertex_location = next().unwrap_or_else(|| none(py));
        let tools = next().unwrap_or_else(|| none(py));
        let max_tokens = next().unwrap_or_else(|| none(py));
        let model_info = next().unwrap_or_else(|| none(py));

        me.setattr("cli_agent", &cli_agent)?;
        me.setattr("max_tokens", &max_tokens)?;
        // ANY forces a call every turn; AUTO lets a text reply end a turn (the
        // scrape optimizer, whose only tool is `more`). Same override as the
        // other providers' tool_choice, in this API's words.
        // self.tool_mode = "AUTO" if (model_info or {}).get("tool_choice") == "auto" else "ANY"
        let info = or(model_info, || Ok(empty_dict(py)))?;
        let mode = if eq(&get(&info, "tool_choice")?, "auto")? { "AUTO" } else { "ANY" };
        me.setattr("tool_mode", lit(py, mode))?;
        // Native tool calling: function declarations ({name, description,
        // parameters}) — the model's output contract. None only for
        // mode="text" (plain prose). _clean_schema_for_google is
        // non-mutating, so the shared registries are safe.
        // self.tools = [_clean_schema_for_google(t) for t in tools] if tools else None
        let cleaned = if tools.is_truthy()? {
            let made = PyList::empty(py);
            for t in comprehension(&tools)? {
                made.append(g(py, "_clean_schema_for_google")?.call1((t?,))?)?;
            }
            made.into_any()
        } else {
            none(py)
        };
        me.setattr("tools", cleaned)?;

        // Check if model is vertex
        // model_info = get_model_info(model) if model else {}
        let model_info = if model.is_truthy()? { g(py, "get_model_info")?.call1((&model,))? } else { empty_dict(py) };
        let vertex = get_or(&model_info, "vertex", PyBool::new(py, false).to_owned().into_any())?;
        me.setattr("is_vertex", vertex)?;

        if attr(me, "is_vertex")?.is_truthy()? {
            // project = vertex_project_id or os.getenv("VERTEX_PROJECT_ID")
            let project = or(vertex_project_id, || g(py, "os")?.getattr("getenv")?.call1(("VERTEX_PROJECT_ID",)))?;
            // location = vertex_location or os.getenv("VERTEX_LOCATION", "global")
            let location = or(vertex_location, || g(py, "os")?.getattr("getenv")?.call1(("VERTEX_LOCATION", "global")))?;
            // self.client = genai.Client(vertexai=True, project=project, location=location,
            //                            http_options=_http_options(cli_agent))
            let client_class = g(py, "genai")?.getattr("Client")?;
            let vertexai = PyBool::new(py, true).to_owned().into_any();
            let options = g(py, "_http_options")?.call1((&cli_agent,))?;
            let client = call_kw(
                &client_class,
                &[("vertexai", &vertexai), ("project", &project), ("location", &location), ("http_options", &options)],
            )?;
            me.setattr("client", client)?;
        } else {
            // key = api_key or os.getenv("GOOGLE_API_KEY")
            let key = or(api_key, || g(py, "os")?.getattr("getenv")?.call1(("GOOGLE_API_KEY",)))?;
            // self.client = genai.Client(api_key=key, http_options=_http_options(cli_agent))
            let client_class = g(py, "genai")?.getattr("Client")?;
            let options = g(py, "_http_options")?.call1((&cli_agent,))?;
            let client = call_kw(&client_class, &[("api_key", &key), ("http_options", &options)])?;
            me.setattr("client", client)?;
        }
        Ok(())
    }

    #[pyo3(name = "_extract_text")]
    #[doc = "Extract text from message content (string or list format with cache_control)"]
    fn extract_text<'py>(slf: &Bound<'py, Self>, content: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
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

    #[pyo3(
        signature = (*args, **kwargs),
        text_signature = "($self, messages, model, annotated_screenshot_base64=None)"
    )]
    #[doc = "Send request to Google Gemini API"]
    fn send_request<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["messages", "model", "annotated_screenshot_base64"];
        let slots = bind("GoogleProvider.send_request", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten().unwrap_or_else(|| none(py));
        let messages = next();
        let model = next();
        let shot = next();

        // Convert OpenAI-style messages to Gemini format
        let mut system_instruction = none(py);
        let contents = PyList::empty(py);

        // NATIVE TRANSCRIPT translation. The coder speaks the canonical OpenAI
        // shape (assistant messages carrying `tool_calls`, `role: "tool"`
        // results keyed by tool_call_id). Gemini carries the same information
        // as function_call / function_response PARTS, and matches results to
        // calls by NAME — so track id -> name as the transcript is walked.
        let call_names = PyDict::new(py);
        // Gemini-3 binding state, filled as the assistant turns are walked:
        // the ids Gemini itself minted, which are the only ones that may be
        // echoed back on a functionResponse.
        let native_ids = g(py, "set")?.call0()?;
        for msg in messages.try_iter()? {
            let msg = msg?;
            let role = get(&msg, "role")?;
            let raw_content = or(get(&msg, "content")?, || Ok(lit(py, "")))?;

            if eq(&role, "system")? {
                system_instruction = attr(me, "_extract_text")?.call1((&raw_content,))?;
            } else if eq(&role, "tool")? {
                let call_id = or(get(&msg, "tool_call_id")?, || Ok(lit(py, "")))?;
                // name = call_names.get(call_id) or msg.get("name") or "tool"
                let name = or(call_names.call_method1("get", (&call_id,))?, || {
                    or(get(&msg, "name")?, || Ok(lit(py, "tool")))
                })?;
                // contents.append(types.Content(role="user", parts=[
                //     _function_response_part(name=name, output=self._extract_text(raw_content),
                //                             call_id=call_id if call_id in native_ids else "")]))
                let content_class = sdk_type(py, "Content")?;
                let response_part = g(py, "_function_response_part")?;
                let output = attr(me, "_extract_text")?.call1((&raw_content,))?;
                let echoed = if native_ids.contains(&call_id)? { call_id.clone() } else { lit(py, "") };
                let part = call_kw(&response_part, &[("name", &name), ("output", &output), ("call_id", &echoed)])?;
                let parts = list1(part)?;
                contents.append(call_kw(&content_class, &[("role", &lit(py, "user")), ("parts", &parts)])?)?;
            } else if eq(&role, "assistant")? {
                let text = attr(me, "_extract_text")?.call1((&raw_content,))?;
                let mut meta = get(&msg, "provider_meta")?;
                // if not (isinstance(meta, dict) and meta.get("provider") == "google"): meta = {}
                if !(isinstance(&meta, "dict")? && eq(&get(&meta, "provider")?, "google")?) {
                    meta = empty_dict(py);
                }
                let sigs = or(get(&meta, "signatures")?, || Ok(empty_dict(py)))?;
                // native_ids.update(meta.get("native_ids") or [])
                let update = native_ids.getattr("update")?;
                update.call1((or(get(&meta, "native_ids")?, || Ok(empty_list(py)))?,))?;
                let parts = PyList::empty(py);
                if text.call_method0("strip")?.is_truthy()? {
                    parts.append(call_kw(&sdk_type(py, "Part")?, &[("text", &text)])?)?;
                }
                // for tc in (msg.get("tool_calls") or []):
                let calls = or(get(&msg, "tool_calls")?, || Ok(empty_list(py)))?;
                for tc in calls.try_iter()? {
                    let tc = tc?;
                    // fn = (tc or {}).get("function") or {}
                    let call = or(get(&or(tc.clone(), || Ok(empty_dict(py)))?, "function")?, || Ok(empty_dict(py)))?;
                    // fname = fn.get("name") or ""
                    let fname = or(get(&call, "name")?, || Ok(lit(py, "")))?;
                    // tc_id = (tc or {}).get("id")
                    let tc_id = get(&or(tc, || Ok(empty_dict(py)))?, "id")?;
                    call_names.set_item(&tc_id, &fname)?;
                    // parts.append(_function_call_part(
                    //     fname, _args_dict(fn.get("arguments")), sigs.get(tc_id, "")))
                    let call_part = g(py, "_function_call_part")?;
                    let args_dict = g(py, "_args_dict")?;
                    let arguments = args_dict.call1((get(&call, "arguments")?,))?;
                    let signature = sigs.call_method1("get", (&tc_id, ""))?;
                    parts.append(call_part.call1((&fname, arguments, signature))?)?;
                }
                if parts.is_empty() {
                    parts.append(call_kw(&sdk_type(py, "Part")?, &[("text", &text)])?)?;
                }
                let content_class = sdk_type(py, "Content")?;
                contents.append(call_kw(&content_class, &[("role", &lit(py, "model")), ("parts", parts.as_any())])?)?;
            } else if eq(&role, "user")? {
                let text = attr(me, "_extract_text")?.call1((&raw_content,))?;
                // contents.append(types.Content(role="user", parts=[types.Part(text=text)]))
                let content_class = sdk_type(py, "Content")?;
                let part = call_kw(&sdk_type(py, "Part")?, &[("text", &text)])?;
                contents.append(call_kw(&content_class, &[("role", &lit(py, "user")), ("parts", &list1(part)?)])?)?;
            }
        }

        // Add screenshot to last user message if provided and NOT cli_agent
        if shot.is_truthy()? && !attr(me, "cli_agent")?.is_truthy()? && g(py, "len")?.call1((&contents,))?.gt(0)? {
            let last = contents.as_any().get_item(-1)?;
            if eq(&last.getattr("role")?, "user")? {
                for one in g(py, "screenshots")?.call1((&shot,))?.try_iter()? {
                    let one = one?;
                    // image_bytes = base64.b64decode(shot)
                    let image_bytes = g(py, "base64")?.getattr("b64decode")?.call1((&one,))?;
                    // last.parts.append(types.Part.from_bytes(data=image_bytes, mime_type="image/jpeg"))
                    let append = last.getattr("parts")?.getattr("append")?;
                    let from_bytes = sdk_type(py, "Part")?.getattr("from_bytes")?;
                    let part = call_kw(&from_bytes, &[("data", &image_bytes), ("mime_type", &lit(py, "image/jpeg"))])?;
                    append.call1((part,))?;
                }
            }
        }

        // Build generation config
        // Output cap. The handoff manager passes 10k (LLMManager.max_tokens);
        // everything else keeps this default.
        // "max_output_tokens": self.max_tokens or out_cap(self.cli_agent, 10000),
        let cap = or(attr(me, "max_tokens")?, || g(py, "out_cap")?.call1((attr(me, "cli_agent")?, 10000)))?;
        let config_params = PyDict::new(py);
        config_params.set_item(PyString::intern(py, "max_output_tokens"), cap)?;

        // How hard the model thinks, per view.py's table. Gemini 3 takes a
        // named level rather than a token budget, so this is thinking_level
        // and not the older thinking_budget. Omitted for an unregistered
        // model, which then keeps Google's default — worth having, since the
        // defaults are expensive: Pro's is `high`, the top of its range.
        let thinking_level = g(py, "get_thinking_level")?.call1((&model, attr(me, "cli_agent")?))?;
        if thinking_level.is_truthy()? {
            let thinking_config = sdk_type(py, "ThinkingConfig")?;
            config_params.set_item("thinking_config", call_kw(&thinking_config, &[("thinking_level", &thinking_level)])?)?;
        }

        // System instruction
        if system_instruction.is_truthy()? {
            config_params.set_item("system_instruction", &system_instruction)?;
        }

        // No tools (mode="text") -> plain text: no tool config, and no JSON
        // mime coercion either.
        if attr(me, "tools")?.is_truthy()? {
            // config_params["tools"] = [types.Tool(function_declarations=self.tools)]
            let tool_class = sdk_type(py, "Tool")?;
            let tool = call_kw(&tool_class, &[("function_declarations", &attr(me, "tools")?)])?;
            config_params.set_item("tools", list1(tool)?)?;
            // mode="ANY" - every turn must call at least one function; a
            // text-only turn is never a valid step (even termination is the
            // `exit` tool). Canonical rationale: see openrouter/service.py.
            let tool_config = sdk_type(py, "ToolConfig")?;
            let calling_config = sdk_type(py, "FunctionCallingConfig")?;
            let calling = call_kw(&calling_config, &[("mode", &attr(me, "tool_mode")?)])?;
            config_params.set_item("tool_config", call_kw(&tool_config, &[("function_calling_config", &calling)])?)?;
        }

        // config = types.GenerateContentConfig(**config_params)
        let config = sdk_type(py, "GenerateContentConfig")?.call((), Some(&config_params))?;

        let tried = (|| -> PyResult<Obj<'py>> {
            // response = self.client.models.generate_content(model=model, contents=contents, config=config)
            let generate = attr(me, "client")?.getattr("models")?.getattr("generate_content")?;
            let response = call_kw(&generate, &[("model", &model), ("contents", contents.as_any()), ("config", &config)])?;

            // Normalize to OpenAI-style format
            let um = getattr_or(&response, "usage_metadata", none(py))?;
            let usage = if um.is_truthy()? {
                let input = or(getattr_or(&um, "prompt_token_count", zero(py)?)?, || zero(py))?;
                let output = or(getattr_or(&um, "candidates_token_count", zero(py)?)?, || zero(py))?;
                dict(py, &[("input_tokens", &input), ("output_tokens", &output)])?
            } else {
                empty_dict(py)
            };
            // Gemini caches the shared start of a request on its own (implicit
            // caching). prompt_token_count already includes the cached part,
            // so it rides in the OpenAI-style field, as responses.py does: the
            // LLM call line shows it and the memory bar does not count it twice.
            // cached = (getattr(um, "cached_content_token_count", 0) or 0) if um else 0
            let cached = if um.is_truthy()? {
                or(getattr_or(&um, "cached_content_token_count", zero(py)?)?, || zero(py))?
            } else {
                zero(py)?
            };
            if cached.is_truthy()? {
                usage.set_item("prompt_tokens_details", dict(py, &[("cached_tokens", &cached)])?)?;
            }

            if attr(me, "tools")?.is_truthy()? {
                // Tools mode: response.text is None when parts carry function
                // calls — walk the parts, collecting text and function_call
                // parts (fc.args is already a dict-like mapping).
                let texts = PyList::empty(py);
                let calls = PyList::empty(py);
                // Gemini 3 hangs a thought signature off each function_call
                // part. It has to ride back on that same part next request or
                // the model loses the link between its call and the result we
                // return for it — keep it with the call's id (see
                // _function_call_part).
                let signatures = PyDict::new(py);
                let native_ids = PyList::empty(py);
                // candidates = getattr(response, "candidates", None) or []
                let candidates = or(getattr_or(&response, "candidates", none(py))?, || Ok(empty_list(py)))?;
                // content = getattr(candidates[0], "content", None) if candidates else None
                let content = if candidates.is_truthy()? {
                    let getattr = g(py, "getattr")?;
                    getattr.call1((candidates.get_item(0)?, "content", none(py)))?
                } else {
                    none(py)
                };
                // for i, part in enumerate(getattr(content, "parts", None) or []):
                let enumerate = g(py, "enumerate")?;
                let parts = or(getattr_or(&content, "parts", none(py))?, || Ok(empty_list(py)))?;
                for pair in enumerate.call1((parts,))?.try_iter()? {
                    let (i, part) = unpack2(&pair?)?;
                    if getattr_or(&part, "thought", PyBool::new(py, false).to_owned().into_any())?.is_truthy()? {
                        continue;
                    }
                    let fc = getattr_or(&part, "function_call", none(py))?;
                    if !fc.is_none() {
                        // Gemini 3 mints its own call id; older models don't,
                        // so fall back to a stable synthetic one and remember
                        // which is which (only real ids may be echoed back).
                        // native_id = str(getattr(fc, "id", "") or "")
                        let to_str = g(py, "str")?;
                        let native_id = to_str.call1((or(getattr_or(&fc, "id", lit(py, ""))?, || Ok(lit(py, "")))?,))?;
                        // call_id = native_id or f"call_{i}"
                        let call_id = or(native_id.clone(), || Fmt::new(py).s("call_").v(&i)?.done())?;
                        // calls.append({"id": call_id, "name": getattr(fc, "name", "") or "",
                        //               "arguments": dict(getattr(fc, "args", None) or {})})
                        let name = or(getattr_or(&fc, "name", lit(py, ""))?, || Ok(lit(py, "")))?;
                        let to_dict = g(py, "dict")?;
                        let arguments = to_dict.call1((or(getattr_or(&fc, "args", none(py))?, || Ok(empty_dict(py)))?,))?;
                        calls.append(dict(py, &[("id", &call_id), ("name", &name), ("arguments", &arguments)])?)?;
                        if native_id.is_truthy()? {
                            native_ids.append(&call_id)?;
                        }
                        // sig = _sig_to_text(getattr(part, "thought_signature", None))
                        let sig_to_text = g(py, "_sig_to_text")?;
                        let sig = sig_to_text.call1((getattr_or(&part, "thought_signature", none(py))?,))?;
                        if sig.is_truthy()? {
                            signatures.set_item(&call_id, &sig)?;
                        }
                    } else if getattr_or(&part, "text", none(py))?.is_truthy()? {
                        texts.append(part.getattr("text")?)?;
                    }
                }
                // message = {"content": "\n".join(texts), "tool_calls": calls}
                let joined = lit(py, "\n").call_method1("join", (&texts,))?;
                let message = dict(py, &[("content", &joined), ("tool_calls", calls.as_any())])?;
                if signatures.is_truthy()? || native_ids.is_truthy()? {
                    let meta = dict(
                        py,
                        &[
                            ("provider", &lit(py, "google")),
                            ("signatures", signatures.as_any()),
                            ("native_ids", native_ids.as_any()),
                        ],
                    )?;
                    message.set_item("provider_meta", meta)?;
                }
                let choices = list1(dict(py, &[("message", &message)])?)?;
                return dict(py, &[("choices", &choices), ("usage", &usage)]);
            }

            // return {"choices": [{"message": {"content": response.text}}], "usage": usage}
            let text = response.getattr("text")?;
            let message = dict(py, &[("content", &text)])?;
            let choices = list1(dict(py, &[("message", &message)])?)?;
            dict(py, &[("choices", &choices), ("usage", &usage)])
        })();
        match tried {
            Ok(result) => Ok(result),
            // except Exception as e:
            //     raise Exception(f"Google Gemini API request failed: {str(e)}")
            Err(e) => except_exception(py, e, |e| {
                Err(throw(py, "Exception", || {
                    let text = g(py, "str")?.call1((e.value(py),))?;
                    Fmt::new(py).s("Google Gemini API request failed: ").v(&text)?.done()
                }))
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

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // load_dotenv(): the .env found from the loader's frame, in this folder.
    g(py, "load_dotenv")?.call0()?;
    store(m, "_args_dict", wrap_pyfunction!(args_dict, m)?)?;
    store(m, "_sig_to_text", wrap_pyfunction!(sig_to_text, m)?)?;
    store(m, "_function_call_part", wrap_pyfunction!(function_call_part, m)?)?;
    store(m, "_function_response_part", wrap_pyfunction!(function_response_part, m)?)?;
    store(m, "_http_options", wrap_pyfunction!(http_options, m)?)?;
    store(m, "_clean_schema_for_google", wrap_pyfunction!(clean_schema_for_google, m)?)?;
    // class GoogleProvider:
    store(m, "GoogleProvider", py.get_type::<GoogleProvider>())?;
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
    let class = py.get_type::<GoogleProvider>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
