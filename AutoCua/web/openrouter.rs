//! openrouter.py: web search on the agent's own OpenRouter model with the
//! web plugin.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{fill, import_from, key_or_env, kwargs, search_error, system_prompt};

/// Perform web search using OpenRouter with a dynamic model. `model`: the
/// full OpenRouter model name (e.g. "google/gemini-3.6-flash"), resolved by
/// service.rs through llm_provider/openrouter/view.py, so this path follows
/// that map and needs no model of its own. `api_key`: the runtime key from
/// the frontend (priority over .env).
pub fn web_search<'py>(
    py: Python<'py>,
    query: &Bound<'py, PyAny>,
    model: &Bound<'py, PyAny>,
    api_key: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let requests = py.import("requests")?;
        let timeout = import_from(py, "AutoCua.llm_provider", "LLM_HTTP_TIMEOUT")?;

        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "OPENROUTER_API_KEY")?;

        let request = kwargs(py, json!({
            "headers": {
                "Authorization": null,
                "Content-Type": "application/json",
                "HTTP-Referer": "http://localhost:3000"
            },
            "json": {
                "model": null,
                "messages": [
                    {"role": "system", "content": system_prompt(py)?},
                    {"role": "user", "content": null}
                ],
                "plugins": [{"id": "web", "engine": "exa", "max_results": 20}]
            },
        }))?;
        let request_any = request.as_any();
        request_any.get_item("headers")?.set_item("Authorization", fill(py, "Bearer {}", (key,))?)?;
        let body = request_any.get_item("json")?;
        body.set_item("model", model)?;
        body.get_item("messages")?.get_item(1)?.set_item("content", query)?;
        request.set_item("timeout", timeout)?;
        let response = requests.call_method("post", ("https://openrouter.ai/api/v1/chat/completions",), Some(&request))?;
        response.call_method0("raise_for_status")?;
        response
            .call_method0("json")?
            .get_item("choices")?
            .get_item(0)?
            .get_item("message")?
            .get_item("content")
    })())
}
