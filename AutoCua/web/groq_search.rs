//! groq_search.py: web search on Groq's compound system with its own web
//! tools.

use pyo3::prelude::*;
use serde_json::json;

use super::service::{import_from, key_or_env, kwargs, search_error, system_prompt};

/// Perform web search using Groq compound. `api_key`: the runtime key from
/// the frontend (priority over .env).
pub fn web_search<'py>(py: Python<'py>, query: &Bound<'py, PyAny>, api_key: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let groq = import_from(py, "groq", "Groq")?;

        // Priority: frontend key > .env fallback
        let key = key_or_env(py, api_key, "GROQ_API_KEY")?;

        let client_kwargs = kwargs(py, json!({
            "api_key": null,
            "default_headers": {
                "Groq-Model-Version": "latest"
            },
        }))?;
        client_kwargs.set_item("api_key", key)?;
        let client = groq.call((), Some(&client_kwargs))?;
        let request = kwargs(py, json!({
            "model": "groq/compound",
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt(py)?
                },
                {
                    "role": "user",
                    "content": null
                }
            ],
            "temperature": 0.2,
            "max_completion_tokens": 8192,
            "top_p": 1,
            "stream": false,
            "stop": null,
            "seed": 42,
            "compound_custom": {"tools": {"enabled_tools": ["visit_website", "web_search", "browser_automation"]}},
        }))?;
        request.as_any().get_item("messages")?.get_item(1)?.set_item("content", query)?;
        let completion = client.getattr("chat")?.getattr("completions")?.call_method("create", (), Some(&request))?;

        completion.getattr("choices")?.get_item(0)?.getattr("message")?.getattr("content")
    })())
}
