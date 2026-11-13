//! anthropic.py: web search on Claude Haiku 4.5 with Anthropic's native
//! web_search tool.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, str_of, system_prompt};

/// Perform web search using Anthropic Claude Haiku 4.5 with the native
/// web_search tool. `api_key`: the runtime key from the frontend (priority
/// over .env).
pub fn web_search<'py>(py: Python<'py>, query: &Bound<'py, PyAny>, api_key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let anthropic = py.import("anthropic")?;

        let key = key_or_env(py, api_key, "ANTHROPIC_API_KEY")?;

        let client_kwargs = PyDict::new(py);
        client_kwargs.set_item("api_key", key)?;
        let client = anthropic.getattr("Anthropic")?.call((), Some(&client_kwargs))?;

        let request = kwargs(py, json!({
            "model": "claude-haiku-4-5-20251001",
            "max_tokens": 4096,
            "system": system_prompt(py)?,
            "messages": [
                {"role": "user", "content": null}
            ],
            "tools": [
                {"type": "web_search_20250305", "name": "web_search"}
            ],
        }))?;
        request.as_any().get_item("messages")?.get_item(0)?.set_item("content", query)?;
        let response = client.getattr("messages")?.call_method("create", (), Some(&request))?;

        let text_parts = PyList::empty(py);
        for block in response.getattr("content")?.try_iter()? {
            let block = block?;
            if block.getattr("type")?.eq("text")? {
                text_parts.append(block.getattr("text")?)?;
            }
        }

        if text_parts.is_empty() {
            str_of(&response)
        } else {
            PyString::new(py, "\n").call_method1("join", (text_parts,))
        }
    })())
}
