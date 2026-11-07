//! google_search.py: web search on Gemini 3.6 Flash grounded with Google
//! Search, on the Gemini API, or on Vertex AI for a "-vertex" model.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::json;

use super::service::{key_or_env, kwargs, search_error, system_prompt, text_of};

/// Perform web search using Google Gemini 3.6 Flash with grounding via
/// Google Search. `api_key`: the runtime key from the frontend (priority
/// over .env); `vertex`: use the Vertex AI endpoint, with
/// `vertex_project_id` (the GCP project) and `vertex_location` (the GCP
/// region) over the .env ones.
pub fn web_search(
    py: Python<'_>,
    query: &str,
    api_key: Option<&str>,
    vertex: bool,
    vertex_project_id: Option<&str>,
    vertex_location: Option<&str>,
) -> PyResult<String> {
    search_error(py, (|| {
        let genai = py.import("google.genai")?;
        let types = py.import("google.genai.types")?;
        let os = py.import("os")?;

        let client = if vertex {
            let project: Option<String> = match vertex_project_id {
                Some(id) if !id.is_empty() => Some(id.to_string()),
                _ => os.call_method1("getenv", ("VERTEX_PROJECT_ID",))?.extract()?,
            };
            let location: Option<String> = match vertex_location {
                Some(region) if !region.is_empty() => Some(region.to_string()),
                _ => os.call_method1("getenv", ("VERTEX_LOCATION", "global"))?.extract()?,
            };
            genai
                .getattr("Client")?
                .call((), Some(&kwargs(py, json!({"vertexai": true, "project": project, "location": location}))?))?
        } else {
            let key = key_or_env(py, api_key, "GOOGLE_API_KEY")?;
            genai.getattr("Client")?.call((), Some(&kwargs(py, json!({"api_key": key}))?))?
        };

        let search = PyDict::new(py);
        search.set_item("google_search", types.getattr("GoogleSearch")?.call0()?)?;
        let config_kwargs = kwargs(py, json!({"system_instruction": system_prompt(py)?}))?;
        config_kwargs.set_item("tools", PyList::new(py, [types.getattr("Tool")?.call((), Some(&search))?])?)?;
        config_kwargs.set_item("max_output_tokens", 8192)?;
        config_kwargs.set_item("seed", 42)?;
        let config = types.getattr("GenerateContentConfig")?.call((), Some(&config_kwargs))?;

        // Kept in step with llm_provider/google/view.py, so web search and
        // the driver run the same Gemini version.
        let request = kwargs(py, json!({"model": "gemini-3.6-flash", "contents": query}))?;
        request.set_item("config", config)?;
        let response = client.getattr("models")?.call_method("generate_content", (), Some(&request))?;

        text_of(&response.getattr("text")?)
    })())
}
