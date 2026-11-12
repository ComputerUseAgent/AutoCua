//! perplexity_search.py: web search on Perplexity Sonar through the Agent
//! API.

use pyo3::prelude::*;
use pyo3::types::PyList;
use serde_json::json;

use super::service::{fill, import_from, key_or_env, kwargs, search_error, str_of, system_prompt};

/// Perform web search using Perplexity Sonar via the Agent API. `api_key`:
/// the runtime key from the frontend (priority over .env).
pub fn web_search<'py>(py: Python<'py>, query: &Bound<'py, PyAny>, api_key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let requests = py.import("requests")?;
        let timeout = import_from(py, "AutoCua.llm_provider", "LLM_HTTP_TIMEOUT")?;

        let key = key_or_env(py, api_key, "PERPLEXITY_API_KEY")?;

        let request = kwargs(py, json!({
            "headers": {
                "Authorization": null,
                "Content-Type": "application/json"
            },
            "json": {
                "model": "perplexity/sonar",
                "instructions": system_prompt(py)?,
                "input": null,
                "tools": [{"type": "web_search"}],
                "max_output_tokens": 4096,
            },
        }))?;
        let request_any = request.as_any();
        request_any.get_item("headers")?.set_item("Authorization", fill(py, "Bearer {}", (key,))?)?;
        request_any.get_item("json")?.set_item("input", query)?;
        request.set_item("timeout", timeout)?;
        let response = requests.call_method("post", ("https://api.perplexity.ai/v1/agent",), Some(&request))?;
        response.call_method0("raise_for_status")?;
        let result = response.call_method0("json")?;

        // Extract text from Agent API response
        for output_item in result.call_method1("get", ("output", PyList::empty(py)))?.try_iter()? {
            let output_item = output_item?;
            if output_item.call_method1("get", ("type",))?.eq("message")? {
                for block in output_item.call_method1("get", ("content", PyList::empty(py)))?.try_iter()? {
                    let block = block?;
                    if block.call_method1("get", ("type",))?.eq("output_text")? {
                        return block.call_method1("get", ("text", ""));
                    }
                }
            }
        }

        str_of(&result)
    })())
}
