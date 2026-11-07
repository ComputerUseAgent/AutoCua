//! openrouter.py: web search on the agent's own OpenRouter model with the
//! web plugin.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using OpenRouter with a dynamic model. `model`: the
/// full OpenRouter model name (e.g. "google/gemini-3.6-flash"), resolved by
/// service.rs through llm_provider/openrouter/view.py, so this path follows
/// that map and needs no model of its own. `api_key`: the runtime key from
/// the frontend (priority over .env).
pub fn web_search(py: Python<'_>, query: &str, model: &str, api_key: Option<&str>) -> PyResult<String> {
    search_error(py, (|| {
        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "OPENROUTER_API_KEY")?;

        let request = kwargs(py, json!({
            "headers": {
                "Authorization": format!("Bearer {}", key.as_deref().unwrap_or("None")),
                "Content-Type": "application/json",
                "HTTP-Referer": "http://localhost:3000"
            },
            "json": {
                "model": model,
                "messages": [
                    {"role": "system", "content": system_prompt(py)?},
                    {"role": "user", "content": query}
                ],
                "plugins": [{"id": "web", "engine": "exa", "max_results": 20}]
            },
        }))?;
        request.set_item("timeout", py.import("AutoCua.llm_provider")?.getattr("LLM_HTTP_TIMEOUT")?)?;
        let response = py
            .import("requests")?
            .call_method("post", ("https://openrouter.ai/api/v1/chat/completions",), Some(&request))?;
        response.call_method0("raise_for_status")?;
        text_of(
            &response
                .call_method0("json")?
                .get_item("choices")?
                .get_item(0)?
                .get_item("message")?
                .get_item("content")?,
        )
    })())
}
