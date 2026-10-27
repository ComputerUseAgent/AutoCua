//! chatgpt.py: web search on OpenAI gpt-5.6-terra with the Responses API's
//! web_search tool.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using OpenAI gpt-5.6-terra with the Responses API.
/// `api_key`: the runtime key from the frontend (priority over .env).
pub fn web_search(py: Python<'_>, query: &str, api_key: Option<&str>) -> PyResult<String> {
    search_error(py, (|| {
        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "OPENAI_API_KEY")?;

        let client = py.import("openai")?.getattr("OpenAI")?.call((), Some(&kwargs(py, json!({"api_key": key}))?))?;

        let response = client.getattr("responses")?.call_method(
            "create",
            (),
            Some(&kwargs(py, json!({
                "model": "gpt-5.6-terra",
                "instructions": system_prompt(py)?,
                "input": query,
                "text": {"verbosity": "medium"},
                "store": true,
                "tools": [
                    {
                        "type": "web_search"
                    }
                ],
            }))?),
        )?;

        // Extract text from response structure: the output is a list, the
        // message in it carries the text
        if response.hasattr("output")? {
            for item in response.getattr("output")?.try_iter()? {
                let item = item?;
                if item.hasattr("type")? && item.getattr("type")?.eq("message")? {
                    if item.hasattr("content")? && item.getattr("content")?.is_truthy()? {
                        for content_item in item.getattr("content")?.try_iter()? {
                            let content_item = content_item?;
                            if content_item.hasattr("text")? {
                                return text_of(&content_item.getattr("text")?);
                            }
                        }
                    }
                }
            }
        }

        // Fallback: return string representation
        text_of(&response)
    })())
}
