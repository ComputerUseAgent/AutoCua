//! chatgpt.py: web search on OpenAI gpt-5.6-terra with the Responses API's
//! web_search tool.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::json;

use super::service::{import_from, key_or_env, kwargs, search_error, str_of, system_prompt};

/// Perform web search using OpenAI gpt-5.6-terra with the Responses API.
/// `api_key`: the runtime key from the frontend (priority over .env).
pub fn web_search<'py>(py: Python<'py>, query: &Bound<'py, PyAny>, api_key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let openai = import_from(py, "openai", "OpenAI")?;

        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "OPENAI_API_KEY")?;

        let client_kwargs = PyDict::new(py);
        client_kwargs.set_item("api_key", key)?;
        let client = openai.call((), Some(&client_kwargs))?;

        let request = kwargs(py, json!({
            "model": "gpt-5.6-terra",
            "instructions": system_prompt(py)?,
            "input": null,
            "text": {"verbosity": "medium"},
            "store": true,
            "tools": [
                {
                    "type": "web_search"
                }
            ],
        }))?;
        request.set_item("input", query)?;
        let response = client.getattr("responses")?.call_method("create", (), Some(&request))?;

        // Extract text from response structure: the output is a list, the
        // message in it carries the text
        if response.hasattr("output")? {
            let output = response.getattr("output")?;
            for item in output.try_iter()? {
                let item = item?;
                if item.hasattr("type")?
                    && item.getattr("type")?.eq("message")?
                    && item.hasattr("content")?
                    && item.getattr("content")?.is_truthy()?
                {
                    for content_item in item.getattr("content")?.try_iter()? {
                        let content_item = content_item?;
                        if content_item.hasattr("text")? {
                            return content_item.getattr("text");
                        }
                    }
                }
            }
        }

        // Fallback: return string representation
        str_of(&response)
    })())
}
