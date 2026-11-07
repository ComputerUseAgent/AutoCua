//! perplexity_search.py: web search on Perplexity Sonar through the Agent
//! API.

use pyo3::prelude::*;
use pyo3::types::PyList;
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using Perplexity Sonar via the Agent API. `api_key`:
/// the runtime key from the frontend (priority over .env).
pub fn web_search(py: Python<'_>, query: &str, api_key: Option<&str>) -> PyResult<String> {
    search_error(py, (|| {
        let key = key_or_env(py, api_key, "PERPLEXITY_API_KEY")?;

        let request = kwargs(py, json!({
            "headers": {
                "Authorization": format!("Bearer {}", key.as_deref().unwrap_or("None")),
                "Content-Type": "application/json"
            },
            "json": {
                "model": "perplexity/sonar",
                "instructions": system_prompt(py)?,
                "input": query,
                "tools": [{"type": "web_search"}],
                "max_output_tokens": 4096,
            },
        }))?;
        request.set_item("timeout", py.import("AutoCua.llm_provider")?.getattr("LLM_HTTP_TIMEOUT")?)?;
        let response = py.import("requests")?.call_method("post", ("https://api.perplexity.ai/v1/agent",), Some(&request))?;
        response.call_method0("raise_for_status")?;
        let result = response.call_method0("json")?;

        // Extract text from Agent API response
        for output_item in result.call_method1("get", ("output", PyList::empty(py)))?.try_iter()? {
            let output_item = output_item?;
            if output_item.call_method1("get", ("type",))?.eq("message")? {
                for block in output_item.call_method1("get", ("content", PyList::empty(py)))?.try_iter()? {
                    let block = block?;
                    if block.call_method1("get", ("type",))?.eq("output_text")? {
                        return text_of(&block.call_method1("get", ("text", ""))?);
                    }
                }
            }
        }

        text_of(&result)
    })())
}
