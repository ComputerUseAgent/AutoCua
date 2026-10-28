//! groq_search.py: web search on Groq's compound system with its own web
//! tools.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using Groq compound. `api_key`: the runtime key from
/// the frontend (priority over .env).
pub fn web_search(py: Python<'_>, query: &str, api_key: Option<&str>) -> PyResult<String> {
    search_error(py, (|| {
        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "GROQ_API_KEY")?;

        let client = py.import("groq")?.getattr("Groq")?.call(
            (),
            Some(&kwargs(py, json!({
                "api_key": key,
                "default_headers": {
                    "Groq-Model-Version": "latest"
                },
            }))?),
        )?;
        let completion = client.getattr("chat")?.getattr("completions")?.call_method(
            "create",
            (),
            Some(&kwargs(py, json!({
                "model": "groq/compound",
                "messages": [
                    {
                        "role": "system",
                        "content": system_prompt(py)?
                    },
                    {
                        "role": "user",
                        "content": query
                    }
                ],
                "temperature": 0.2,
                "max_completion_tokens": 8192,
                "top_p": 1,
                "stream": false,
                "stop": null,
                "seed": 42,
                "compound_custom": {"tools": {"enabled_tools": ["visit_website", "web_search", "browser_automation"]}},
            }))?),
        )?;

        text_of(&completion.getattr("choices")?.get_item(0)?.getattr("message")?.getattr("content")?)
    })())
}
