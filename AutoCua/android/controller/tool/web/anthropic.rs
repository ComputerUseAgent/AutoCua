//! anthropic.py: web search on Claude Haiku 4.5 with Anthropic's native
//! web_search tool.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using Anthropic Claude Haiku 4.5 with the native
/// web_search tool. `api_key`: the runtime key from the frontend (priority
/// over .env).
pub fn web_search(py: Python<'_>, query: &str, api_key: Option<&str>) -> PyResult<String> {
    search_error(py, (|| {
        let key = key_or_env(py, api_key, "ANTHROPIC_API_KEY")?;

        let client = py.import("anthropic")?.getattr("Anthropic")?.call((), Some(&kwargs(py, json!({"api_key": key}))?))?;

        let response = client.getattr("messages")?.call_method(
            "create",
            (),
            Some(&kwargs(py, json!({
                "model": "claude-haiku-4-5-20251001",
                "max_tokens": 4096,
                "system": system_prompt(py)?,
                "messages": [
                    {"role": "user", "content": query}
                ],
                "tools": [
                    {"type": "web_search_20250305", "name": "web_search"}
                ],
            }))?),
        )?;

        let mut text_parts: Vec<String> = Vec::new();
        for block in response.getattr("content")?.try_iter()? {
            let block = block?;
            if block.getattr("type")?.eq("text")? {
                text_parts.push(block.getattr("text")?.extract()?);
            }
        }

        if text_parts.is_empty() { text_of(&response) } else { Ok(text_parts.join("\n")) }
    })())
}
