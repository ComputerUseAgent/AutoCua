//! The `web` tool: the copy of ios/controller/tool/web/service.py for the
//! Android agent, which routes a query to the main agent's own provider. The
//! searches sit beside it, one file each as on iOS and the desktops
//! (anthropic.rs, chatgpt.rs, google_search.rs, groq_search.rs,
//! openrouter.rs, perplexity_search.rs), with web.md, their system prompt;
//! the providers with no web-search API go to the headless browser agent
//! instead (web_agent.rs). Either way the answer is the iOS text,
//! `{\ntool: web,\nstatus: ...}`, which the controller reports as the
//! tool's result and the loop digests.
//!
//! The SDKs are the Python ones iOS calls (anthropic, openai, google.genai,
//! groq, requests), called over the GIL with the same arguments, so a
//! request goes out exactly as it does from iOS; they give the GIL back
//! while they wait on the network. What the searches share sits here: the
//! prompt and the .env each iOS search module loads at import, the keyword
//! arguments, and the error each one raises.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use pyo3::exceptions::{PyException, PyOSError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::Value;

use super::anthropic::web_search as anthropic_web_search;
use super::chatgpt::web_search as chatgpt_web_search;
use super::google_search::web_search as google_web_search;
use super::groq_search::web_search as groq_web_search;
use super::openrouter::web_search as openrouter_web_search;
use super::perplexity_search::web_search as perplexity_web_search;
use super::web_agent::{web_search as web_agent_web_search, Failure};
use crate::agent::android_use::service::read_text;

/// Providers with no native web-search API: the query goes to the browser
/// agent on the same provider+model instead (see web_agent.rs). Explicit
/// membership, not a catch-all: a typo'd provider must still fail fast below.
const BROWSER_FALLBACK_PROVIDERS: [&str; 3] = ["together", "cerebras", "aws"];

/// AutoCua/android/controller/tool/web: this tool's folder.
fn web_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(crate::android_dir(py)?.join("controller").join("tool").join("web"))
}

/// web.md beside this file: every search's system prompt. Read once, as the
/// iOS search modules read it at import.
pub(super) fn system_prompt(py: Python<'_>) -> PyResult<&'static str> {
    static PROMPT: OnceLock<String> = OnceLock::new();
    if let Some(prompt) = PROMPT.get() {
        return Ok(prompt);
    }
    let path = web_dir(py)?.join("web.md");
    let text = read_text(&path).map_err(|e| PyOSError::new_err(format!("{}: {e}", path.display())))?;
    Ok(PROMPT.get_or_init(|| text))
}

/// Each iOS search module runs load_dotenv() at import: the .env nearest its
/// folder fills in what the environment lacks, never overriding. The agent's
/// LLM manager has made the same call by then, so this finds the same file
/// and changes nothing; it stays for a caller that comes without one. Once
/// per process.
fn load_dotenv(py: Python<'_>) -> PyResult<()> {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.get().is_some() {
        return Ok(());
    }
    let start = web_dir(py)?;
    if let Some(env) = start.ancestors().map(|dir| dir.join(".env")).find(|p| p.is_file()) {
        py.import("dotenv")?.call_method1("load_dotenv", (path_str(&env),))?;
    }
    let _ = DONE.set(());
    Ok(())
}

/// A path as Python's str(Path) gives it.
pub(super) fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Python's str(x), as an f-string writes a value into the result.
pub(super) fn text_of(value: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(value.str()?.to_string_lossy().into_owned())
}

/// `api_key or os.getenv(var)`: the runtime key from the frontend wins over
/// the .env one.
pub(super) fn key_or_env(py: Python<'_>, api_key: Option<&str>, var: &str) -> PyResult<Option<String>> {
    match api_key {
        Some(key) if !key.is_empty() => Ok(Some(key.to_string())),
        _ => py.import("os")?.call_method1("getenv", (var,))?.extract(),
    }
}

/// Keyword arguments from a JSON literal: the same Python values the iOS
/// call's literals make (str, int, float, bool, None, list, dict), in the
/// same order.
pub(super) fn kwargs<'py>(py: Python<'py>, value: Value) -> PyResult<Bound<'py, PyDict>> {
    Ok(pythonize::pythonize(py, &value)?.cast_into::<PyDict>()?)
}

/// Each iOS search's `except Exception as e: raise Exception(f"Error in web
/// search: {str(e)}")`. KeyboardInterrupt and SystemExit fly as they are.
pub(super) fn search_error(py: Python<'_>, result: PyResult<String>) -> PyResult<String> {
    result.map_err(|e| {
        if e.is_instance_of::<PyException>(py) {
            PyException::new_err(format!("Error in web search: {}", e.value(py)))
        } else {
            e
        }
    })
}

/// Web service to route queries to the appropriate provider.
pub struct WebService {
    provider: String,
    model: String,
    /// Runtime key from the frontend (priority over .env).
    api_key: Option<String>,
    /// Only the browser-agent fallback can be interrupted.
    stop_event: Option<Py<PyAny>>,
    /// Gemini on Vertex AI, told by the model name.
    vertex: bool,
}

impl WebService {
    pub fn new(
        py: Python<'_>,
        provider: &str,
        model: &str,
        api_key: Option<String>,
        stop_event: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        // What the iOS search modules do once at import: the .env, web.md.
        load_dotenv(py)?;
        system_prompt(py)?;

        let provider = provider.to_lowercase();
        // Auto-detect Vertex from model name (e.g. "gemini-3.1-pro-vertex")
        let vertex = provider == "google" && model.ends_with("-vertex");

        // Resolve short model name to full API name for OpenRouter
        let model = if provider == "openrouter" {
            py.import("AutoCua.llm_provider.openrouter.view")?
                .call_method1("get_model_info", (model,))?
                .get_item("api_name")?
                .extract()?
        } else {
            model.to_string()
        };

        Ok(WebService { provider, model, api_key, stop_event, vertex })
    }

    /// Route the web search to the appropriate provider and format the
    /// response as the iOS JSON-like object (no wrapper tags: the agent adds
    /// the <tool> wrapper). A failed search is that text with `status:
    /// error`, never an Err; only KeyboardInterrupt and the like fly.
    pub fn search(&self, py: Python<'_>, query: &str) -> PyResult<String> {
        // Single attempt: a browse run is minutes long, and the browser
        // agent's own LLM manager already retries each call.
        if BROWSER_FALLBACK_PROVIDERS.contains(&self.provider.as_str()) {
            return self.search_via_web_agent(py, query);
        }

        // Retry up to 3 times with 1 second delay
        let key = self.api_key.as_deref();
        for attempt in 0..3 {
            let result = match self.provider.as_str() {
                "openrouter" => openrouter_web_search(py, query, &self.model, key),
                "groq" => groq_web_search(py, query, key), // Groq always uses compound
                "openai" => chatgpt_web_search(py, query, key), // OpenAI always uses gpt-5.6-terra
                "anthropic" => anthropic_web_search(py, query, key), // Anthropic uses Haiku 4.5 with native web_search
                // Google uses Gemini 3.6 Flash with grounding; the project
                // and region come from the .env, as on iOS
                "google" => google_web_search(py, query, key, self.vertex, None, None),
                "perplexity" => perplexity_web_search(py, query, key), // Perplexity uses Sonar with native web search
                _ => Ok(format!("Unsupported provider: {}", self.provider)),
            };

            match result {
                // Format as JSON object (no wrapper tags - agent adds <tool> wrapper)
                Ok(result) => {
                    return Ok(format!("{{\ntool: web,\nstatus: success,\nquery: \"{query}\",\nInformation: \"{result}\"\n}}"));
                }
                Err(e) if !e.is_instance_of::<PyException>(py) => return Err(e),
                Err(e) => {
                    if attempt < 2 {
                        // If not the last attempt
                        println!("⚠️ Web search failed (attempt {}/3), retrying in 1 second...", attempt + 1);
                        py.detach(|| std::thread::sleep(Duration::from_secs(1)));
                        py.check_signals()?;
                        continue;
                    }
                    // Last attempt failed, return error
                    let error_msg = format!("Web service error: {}", e.value(py));
                    return Ok(format!(
                        "{{\ntool: web,\nstatus: error,\nquery: \"{query}\",\nNote: Search failed after 3 attempts,\nInformation: \"{error_msg}\"\n}}"
                    ));
                }
            }
        }
        unreachable!("the third attempt always returns")
    }

    /// Same result shapes as the native path, produced by the browser agent.
    fn search_via_web_agent(&self, py: Python<'_>, query: &str) -> PyResult<String> {
        let outcome = web_agent_web_search(
            py,
            query,
            &self.provider,
            &self.model,
            self.api_key.as_deref(),
            self.stop_event.as_ref(),
        );
        match outcome {
            Ok(report) => Ok(format!("{{\ntool: web,\nstatus: success,\nquery: \"{query}\",\nInformation: \"{report}\"\n}}")),
            Err(Failure::Search(e)) => Ok(format!(
                "{{\ntool: web,\nstatus: error,\nquery: \"{query}\",\nNote: {},\nInformation: \"{}\"\n}}",
                e.note,
                e.message()
            )),
            Err(Failure::Py(e)) if !e.is_instance_of::<PyException>(py) => Err(e),
            Err(Failure::Py(e)) => {
                let error_msg = format!("Web service error: {}", e.value(py));
                Ok(format!(
                    "{{\ntool: web,\nstatus: error,\nquery: \"{query}\",\nNote: Browser agent failed to start,\nInformation: \"{error_msg}\"\n}}"
                ))
            }
        }
    }
}
