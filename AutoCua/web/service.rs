//! The `web` tool: ONE web search, shared by every agent. Computer use on
//! macOS, Windows and Linux and the iPhone agent call it from their Python
//! controllers (`from AutoCua.web import WebService`); the Android agent and
//! the coder call it from their Rust controllers
//! (crate::web::service::WebService). It routes a query to the agent's own
//! provider. The searches sit beside this file, one file each (anthropic.rs,
//! chatgpt.rs, google_search.rs, groq_search.rs, openrouter.rs,
//! perplexity_search.rs), with web.md, their system prompt; the providers
//! with no web-search API go to the headless browser agent instead
//! (web_agent.rs). Either way the answer is the text
//! `{\ntool: web,\nstatus: ...}`, which the controller reports as the tool's
//! result.
//!
//! It is the Rust copy of the Mac's Python web tool, which each agent used
//! to have a copy of. The SDKs are the Python ones the Mac called
//! (anthropic, openai, google.genai, groq, requests), called over the GIL
//! with the same arguments, so a request goes out exactly as it did from the
//! Mac; they give the GIL back while they wait on the network, and so does
//! every wait here (Python's time.sleep). Each SDK is imported at its first
//! use, where the Mac imported them all at start. What a caller
//! gives (the query, the key, the model) stays a Python object, and the
//! answers are made by Python's str.format, which writes a value the way an
//! f-string does, so a caller gets the Mac's text, value for value. What the
//! searches share sits here: the prompt and the .env each Mac search module
//! loaded at import, the imports, the keyword arguments and the error each
//! one raises.

use std::path::PathBuf;
use std::sync::OnceLock;

use pyo3::call::PyCallArgs;
use pyo3::exceptions::{PyAttributeError, PyException, PyImportError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyString};
use serde_json::Value;

use super::anthropic::web_search as anthropic_web_search;
use super::chatgpt::web_search as chatgpt_web_search;
use super::google_search::web_search as google_web_search;
use super::groq_search::web_search as groq_web_search;
use super::openrouter::web_search as openrouter_web_search;
use super::perplexity_search::web_search as perplexity_web_search;
use super::web_agent::{web_search as web_agent_web_search, Failure};

/// Providers with no native web-search API: the query goes to the browser
/// agent on the same provider+model instead (see web_agent.rs). Explicit
/// membership, not a catch-all: a typo'd provider must still fail fast below.
const BROWSER_FALLBACK_PROVIDERS: [&str; 5] = ["together", "cerebras", "requesty", "aws", "ollama"];

// The answers, filled in by Python's str.format: each {} is written as an
// f-string writes it, and {{ }} are the braces themselves.

/// A search that found something.
const SUCCESS: &str = "{{\ntool: web,\nstatus: success,\nquery: \"{}\",\nInformation: \"{}\"\n}}";
/// A native search that failed three times.
const FAILED: &str = "{{\ntool: web,\nstatus: error,\nquery: \"{}\",\nNote: Search failed after 3 attempts,\nInformation: \"{}\"\n}}";
/// A browser agent that gave no report: its reason, then the detail.
const NO_REPORT: &str = "{{\ntool: web,\nstatus: error,\nquery: \"{}\",\nNote: {},\nInformation: \"{}\"\n}}";
/// A browser agent that could not be run at all.
const NOT_STARTED: &str =
    "{{\ntool: web,\nstatus: error,\nquery: \"{}\",\nNote: Browser agent failed to start,\nInformation: \"{}\"\n}}";

/// AutoCua/web: this tool's folder, beside the extension module.
fn web_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(crate::native_dir(py)?.join("web"))
}

/// web.md beside this file: every search's system prompt. Read once, as the
/// Mac search modules read it at import, and through Python's open as they
/// did (`with open(web_md_path, "r") as f`), so the packaged app's patched
/// open serves its embedded copy. Text mode, UTF-8 on every OS: "\r\n" reads
/// as "\n".
pub(super) fn system_prompt(py: Python<'_>) -> PyResult<&'static str> {
    static PROMPT: OnceLock<String> = OnceLock::new();
    if let Some(prompt) = PROMPT.get() {
        return Ok(prompt);
    }
    let path = web_dir(py)?.join("web.md");
    let encoding = PyDict::new(py);
    encoding.set_item("encoding", "utf-8")?;
    let file = py
        .import("builtins")?
        .getattr("open")?
        .call((path.to_string_lossy().into_owned(), "r"), Some(&encoding))?;
    let text = file.call_method0("read");
    file.call_method0("close")?;
    let text: String = text?.extract()?;
    Ok(PROMPT.get_or_init(|| text))
}

/// Each Mac search module ran load_dotenv() at import: the .env nearest its
/// folder fills in what the environment lacks, never overriding. Here the
/// search starts at this tool's folder and finds the same file (the repo's
/// .env). Once per process.
fn load_dotenv(py: Python<'_>) -> PyResult<()> {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return Ok(());
    }
    let start = web_dir(py)?;
    if let Some(env) = start.ancestors().map(|dir| dir.join(".env")).find(|p| p.is_file()) {
        py.import("dotenv")?.call_method1("load_dotenv", (env.to_string_lossy().into_owned(),))?;
    }
    let _ = DONE.set(());
    Ok(())
}

/// `template.format(*values)`: Python's str.format, which writes each value
/// the way an f-string does (format(value, "")).
pub(super) fn fill<'py>(py: Python<'py>, template: &str, values: impl PyCallArgs<'py>) -> PyResult<Bound<'py, PyAny>> {
    PyString::new(py, template).call_method1("format", values)
}

/// Python's `str(x)`.
pub(super) fn str_of<'py>(value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    Ok(value.str()?.into_any())
}

/// `print(text, flush=True)` with Python's print: the line goes to
/// sys.stdout wherever it points, in its place among the agent's own prints,
/// whether the agent is Python or Rust.
pub(super) fn print_line(py: Python<'_>, text: &Bound<'_, PyAny>) -> PyResult<()> {
    let flush = PyDict::new(py);
    flush.set_item("flush", true)?;
    py.import("builtins")?.getattr("print")?.call((text,), Some(&flush))?;
    Ok(())
}

/// `from package import name`, as Python runs it: __import__ with the name
/// in its fromlist (which imports a submodule of that name), then the
/// attribute, or else the submodule in sys.modules.
pub(super) fn import_from<'py>(py: Python<'py>, package: &str, name: &str) -> PyResult<Bound<'py, PyAny>> {
    let module = py
        .import("builtins")?
        .getattr("__import__")?
        .call1((package, py.None(), py.None(), (name,), 0))?;
    match module.getattr(name) {
        Ok(value) => Ok(value),
        Err(e) if e.is_instance_of::<PyAttributeError>(py) => {
            let package_name = module.getattr("__name__")?;
            let found = py
                .import("sys")?
                .getattr("modules")?
                .call_method1("get", (fill(py, "{}.{}", (&package_name, name))?,))?;
            if !found.is_none() {
                return Ok(found);
            }
            let place = match module.getattr("__file__") {
                Ok(file) if file.is_instance_of::<PyString>() => file,
                _ => PyString::new(py, "unknown location").into_any(),
            };
            Err(PyImportError::new_err(
                fill(py, "cannot import name {!r} from {!r} ({})", (name, &package_name, place))?.unbind(),
            ))
        }
        Err(e) => Err(e),
    }
}

/// `api_key or os.getenv(var)`: the runtime key from the frontend wins over
/// the .env one.
pub(super) fn key_or_env<'py>(py: Python<'py>, api_key: &Bound<'py, PyAny>, var: &str) -> PyResult<Bound<'py, PyAny>> {
    if api_key.is_truthy()? {
        Ok(api_key.clone())
    } else {
        py.import("os")?.call_method1("getenv", (var,))
    }
}

/// Keyword arguments from a JSON literal: the same Python values the Mac
/// call's literals make (str, int, float, bool, None, list, dict), in the
/// same order. A value the caller gave (the query, a key, the model) is a
/// null in the literal, set over it afterwards, so it keeps its place.
pub(super) fn kwargs<'py>(py: Python<'py>, value: Value) -> PyResult<Bound<'py, PyDict>> {
    Ok(pythonize::pythonize(py, &value)?.cast_into::<PyDict>()?)
}

/// Each Mac search's `except Exception as e: raise Exception(f"Error in web
/// search: {str(e)}")`. KeyboardInterrupt and SystemExit fly as they are.
pub(super) fn search_error<'py>(py: Python<'py>, result: PyResult<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    match result {
        Err(e) if e.is_instance_of::<PyException>(py) => {
            let message = PyString::new(py, "Error in web search: ").add(e.value(py).str()?)?;
            Err(PyErr::from_value(py.get_type::<PyException>().call1((message,))?))
        }
        other => other,
    }
}

/// `f"Web service error: {str(e)}"`.
fn service_error<'py>(py: Python<'py>, e: &PyErr) -> PyResult<Bound<'py, PyAny>> {
    PyString::new(py, "Web service error: ").add(e.value(py).str()?)
}

/// Web service to route queries to the appropriate provider. The same class
/// for the Python callers (AutoCua.web.WebService) and the Rust ones. Its
/// attributes hold what the Python class held, as Python objects.
#[pyclass(frozen, module = "AutoCua.web", name = "WebService")]
pub struct WebService {
    /// provider.lower(), as the caller's object made it.
    #[pyo3(get)]
    provider: Py<PyAny>,
    /// The same as Rust text, when it is a str: what picks the search.
    name: Option<String>,
    /// The model, resolved to its full API name for OpenRouter.
    #[pyo3(get)]
    model: Py<PyAny>,
    /// Runtime key from the frontend (priority over .env).
    #[pyo3(get)]
    api_key: Py<PyAny>,
    /// Only the browser-agent fallback can be interrupted. None: no event.
    #[pyo3(get)]
    stop_event: Py<PyAny>,
    /// Gemini on Vertex AI: asked for, or told by the model name.
    #[pyo3(get)]
    vertex: Py<PyAny>,
    #[pyo3(get)]
    vertex_project_id: Py<PyAny>,
    #[pyo3(get)]
    vertex_location: Py<PyAny>,
}

impl WebService {
    /// The service for a Rust caller (the Android agent, the coder): no
    /// Vertex settings of its own, so a "-vertex" Google model reads the
    /// project and the region from the .env.
    pub fn new(
        py: Python<'_>,
        provider: &str,
        model: &str,
        api_key: Option<String>,
        stop_event: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let api_key = match api_key {
            Some(key) => PyString::new(py, &key).into_any(),
            None => py.None().into_bound(py),
        };
        let stop_event = stop_event.unwrap_or_else(|| py.None()).into_bound(py);
        let none = py.None().into_bound(py);
        Self::build(
            py,
            PyString::new(py, provider).as_any(),
            PyString::new(py, model).as_any(),
            api_key,
            None,
            none.clone(),
            none,
            stop_event,
        )
    }

    /// Route the web search to the appropriate provider: the answer as Rust
    /// text, for a Rust caller. A failed search is that text with `status:
    /// error`, never an Err; only KeyboardInterrupt and the like fly, and an
    /// error from Python's print (a closed stdout), as on the Mac.
    pub fn search(&self, py: Python<'_>, query: &str) -> PyResult<String> {
        let answer = self.answer(py, PyString::new(py, query).as_any())?;
        Ok(answer.str()?.to_string_lossy().into_owned())
    }

    /// The Python class's __init__, in its order.
    #[allow(clippy::too_many_arguments)]
    fn build<'py>(
        py: Python<'py>,
        provider: &Bound<'py, PyAny>,
        model: &Bound<'py, PyAny>,
        api_key: Bound<'py, PyAny>,
        vertex: Option<Bound<'py, PyAny>>,
        vertex_project_id: Bound<'py, PyAny>,
        vertex_location: Bound<'py, PyAny>,
        stop_event: Bound<'py, PyAny>,
    ) -> PyResult<Self> {
        // What the Mac search modules did once at import: the .env, web.md.
        load_dotenv(py)?;
        system_prompt(py)?;

        let provider = provider.call_method0("lower")?;
        let name = provider.cast::<PyString>().ok().and_then(|s| s.to_str().ok()).map(str::to_owned);
        // Auto-detect Vertex from model name (e.g. "gemini-4-argon-vertex"):
        // `vertex or (provider == "google" and model and model.endswith(...))`
        let vertex = match vertex {
            Some(vertex) if vertex.is_truthy()? => vertex,
            _ if name.as_deref() != Some("google") => PyBool::new(py, false).to_owned().into_any(),
            _ if !model.is_truthy()? => model.clone(),
            _ => model.call_method1("endswith", ("-vertex",))?,
        };

        // Resolve short model name to full API name for OpenRouter
        let model = if name.as_deref() == Some("openrouter") {
            import_from(py, "AutoCua.llm_provider.openrouter.view", "get_model_info")?
                .call1((model,))?
                .get_item("api_name")?
        } else {
            model.clone()
        };

        Ok(WebService {
            provider: provider.unbind(),
            name,
            model: model.unbind(),
            api_key: api_key.unbind(),
            stop_event: stop_event.unbind(),
            vertex: vertex.unbind(),
            vertex_project_id: vertex_project_id.unbind(),
            vertex_location: vertex_location.unbind(),
        })
    }

    /// Route the web search to the appropriate provider and format the
    /// response as the Mac JSON-like object (no wrapper tags: the agent adds
    /// the <tool> wrapper), a Python str.
    fn answer<'py>(&self, py: Python<'py>, query: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let name = self.name.as_deref();
        // Single attempt: a browse run is minutes long, and the browser
        // agent's own LLM manager already retries each call.
        if name.is_some_and(|name| BROWSER_FALLBACK_PROVIDERS.contains(&name)) {
            return self.search_via_web_agent(py, query);
        }

        // Retry up to 3 times with 1 second delay
        let key = self.api_key.bind(py);
        let mut attempt = 0;
        loop {
            let tried = (|| {
                let result = match name {
                    Some("openrouter") => openrouter_web_search(py, query, self.model.bind(py), key)?,
                    Some("groq") => groq_web_search(py, query, key)?, // Groq always uses compound
                    Some("openai") => chatgpt_web_search(py, query, key)?, // OpenAI always uses gpt-5.6-terra
                    Some("anthropic") => anthropic_web_search(py, query, key)?, // Anthropic uses Haiku 4.5 with native web_search
                    // Google uses Gemini 3.6 Flash with grounding
                    Some("google") => google_web_search(
                        py,
                        query,
                        key,
                        self.vertex.bind(py),
                        self.vertex_project_id.bind(py),
                        self.vertex_location.bind(py),
                    )?,
                    Some("perplexity") => perplexity_web_search(py, query, key)?, // Perplexity uses Sonar with native web search
                    _ => fill(py, "Unsupported provider: {}", (self.provider.bind(py),))?,
                };
                // Format as JSON object (no wrapper tags - agent adds <tool> wrapper)
                fill(py, SUCCESS, (query, result))
            })();

            match tried {
                Ok(answer) => return Ok(answer),
                // Only an Exception is caught: KeyboardInterrupt and the like fly.
                Err(e) if !e.is_instance_of::<PyException>(py) => return Err(e),
                // If not the last attempt
                Err(_) if attempt < 2 => {
                    let line = format!("⚠️ Web search failed (attempt {}/3), retrying in 1 second...", attempt + 1);
                    print_line(py, PyString::new(py, &line).as_any())?;
                    py.import("time")?.call_method1("sleep", (1,))?;
                    attempt += 1;
                }
                // Last attempt failed, return error
                Err(e) => {
                    let error_msg = service_error(py, &e)?;
                    return fill(py, FAILED, (query, error_msg));
                }
            }
        }
    }

    /// Same result shapes as the native path, produced by the browser agent.
    fn search_via_web_agent<'py>(&self, py: Python<'py>, query: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let outcome = web_agent_web_search(
            py,
            query,
            self.provider.bind(py),
            self.model.bind(py),
            self.api_key.bind(py),
            self.stop_event.bind(py),
        )
        .and_then(|report| Ok(fill(py, SUCCESS, (query, report))?));
        match outcome {
            Ok(answer) => Ok(answer),
            Err(Failure::Search(e)) => fill(py, NO_REPORT, (query, e.note.bind(py), e.message(py)?)),
            Err(Failure::Py(e)) if !e.is_instance_of::<PyException>(py) => Err(e),
            Err(Failure::Py(e)) => {
                let error_msg = service_error(py, &e)?;
                fill(py, NOT_STARTED, (query, error_msg))
            }
        }
    }
}

#[pymethods]
impl WebService {
    /// WebService(provider, model, api_key=None, vertex=False,
    /// vertex_project_id=None, vertex_location=None, stop_event=None), the
    /// Python class's signature.
    #[new]
    #[pyo3(
        signature = (provider, model, api_key = None, vertex = None, vertex_project_id = None,
                     vertex_location = None, stop_event = None),
        text_signature = "(provider, model, api_key=None, vertex=False, vertex_project_id=None, vertex_location=None, stop_event=None)"
    )]
    #[allow(clippy::too_many_arguments)]
    fn py_new<'py>(
        py: Python<'py>,
        provider: &Bound<'py, PyAny>,
        model: &Bound<'py, PyAny>,
        api_key: Option<Bound<'py, PyAny>>,
        vertex: Option<Bound<'py, PyAny>>,
        vertex_project_id: Option<Bound<'py, PyAny>>,
        vertex_location: Option<Bound<'py, PyAny>>,
        stop_event: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Self> {
        let or_none = |value: Option<Bound<'py, PyAny>>| value.unwrap_or_else(|| py.None().into_bound(py));
        Self::build(
            py,
            provider,
            model,
            or_none(api_key),
            vertex,
            or_none(vertex_project_id),
            or_none(vertex_location),
            or_none(stop_event),
        )
    }

    /// Route web search to appropriate provider and format response as JSON
    /// object
    #[pyo3(name = "search", signature = (query))]
    fn py_search<'py>(&self, py: Python<'py>, query: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        self.answer(py, query)
    }
}
