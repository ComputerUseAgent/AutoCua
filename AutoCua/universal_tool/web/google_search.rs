//! google_search.py: web search on Gemini 3.6 Flash grounded with Google
//! Search, on the Gemini API, or on Vertex AI for a "-vertex" model.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::json;

use super::service::{import_from, key_or_env, kwargs, search_error, system_prompt};

/// Perform web search using Google Gemini 3.6 Flash with grounding via
/// Google Search. `api_key`: the runtime key from the frontend (priority
/// over .env); `vertex`: use the Vertex AI endpoint, with
/// `vertex_project_id` (the GCP project) and `vertex_location` (the GCP
/// region) over the .env ones.
pub fn web_search<'py>(
    py: Python<'py>,
    query: &Bound<'py, PyAny>,
    api_key: &Bound<'py, PyAny>,
    vertex: &Bound<'py, PyAny>,
    vertex_project_id: &Bound<'py, PyAny>,
    vertex_location: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let genai = import_from(py, "google", "genai")?;
        let types = import_from(py, "google.genai", "types")?;
        let os = py.import("os")?;

        let client = if vertex.is_truthy()? {
            let project = if vertex_project_id.is_truthy()? {
                vertex_project_id.clone()
            } else {
                os.call_method1("getenv", ("VERTEX_PROJECT_ID",))?
            };
            let location = if vertex_location.is_truthy()? {
                vertex_location.clone()
            } else {
                os.call_method1("getenv", ("VERTEX_LOCATION", "global"))?
            };
            let client_kwargs = kwargs(py, json!({"vertexai": true, "project": null, "location": null}))?;
            client_kwargs.set_item("project", project)?;
            client_kwargs.set_item("location", location)?;
            genai.getattr("Client")?.call((), Some(&client_kwargs))?
        } else {
            let key = key_or_env(py, api_key, "GOOGLE_API_KEY")?;
            let client_kwargs = PyDict::new(py);
            client_kwargs.set_item("api_key", key)?;
            genai.getattr("Client")?.call((), Some(&client_kwargs))?
        };

        // In Python's order: each callable is looked up before its
        // arguments are made.
        let generate_content_config = types.getattr("GenerateContentConfig")?;
        let tool = types.getattr("Tool")?;
        let search = PyDict::new(py);
        search.set_item("google_search", types.getattr("GoogleSearch")?.call0()?)?;
        let tools = PyList::new(py, [tool.call((), Some(&search))?])?;
        let config_kwargs = kwargs(py, json!({"system_instruction": system_prompt(py)?}))?;
        config_kwargs.set_item("tools", tools)?;
        config_kwargs.set_item("max_output_tokens", 8192)?;
        config_kwargs.set_item("seed", 42)?;
        let config = generate_content_config.call((), Some(&config_kwargs))?;

        // Kept in step with llm_provider/google/view.py, so web search and
        // the driver run the same Gemini version.
        let request = kwargs(py, json!({"model": "gemini-3.6-flash", "contents": null}))?;
        request.set_item("contents", query)?;
        request.set_item("config", config)?;
        let response = client.getattr("models")?.call_method("generate_content", (), Some(&request))?;

        response.getattr("text")
    })())
}
