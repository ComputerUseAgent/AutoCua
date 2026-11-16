//! specstream_search.rs: web search through SpecStream's Search API.

use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyString;
use serde_json::{json, Value};

use super::service::{fill, import_from, key_or_env, kwargs, search_error, str_of};

const SEARCH_URL: &str = "https://api.specstream.ai/v1/web-search";
const MAX_QUERY_CHARS: usize = 2000;

fn non_empty_str<'a>(obj: &'a Value, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn scalar_text(val: &Value) -> Option<String> {
    match val {
        Value::Null => None,
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t.to_owned()) }
        }
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Object(map) => map.get("value").and_then(scalar_text),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(scalar_text).collect();
            if parts.is_empty() { None } else { Some(parts.join(", ")) }
        }
    }
}

/// Format a SpecStream `/v1/web-search` JSON payload into readable Markdown
/// for the agent's `web` tool output.
pub(crate) fn format_response(payload: &Value) -> String {
    let data = payload.get("data").unwrap_or(payload);
    let mut sections: Vec<String> = Vec::new();

    if let Some(answer) = data.get("answer").filter(|v| v.is_object()) {
        let mut lines = Vec::new();
        if let Some(value) = non_empty_str(answer, "value") {
            lines.push(value.to_owned());
        }
        if let Some(page) = non_empty_str(answer, "page") {
            lines.push(format!("Source: {page}"));
        }
        if let Some(parts) = answer.get("parts").and_then(Value::as_array) {
            for part in parts {
                if let (Some(q), Some(v)) = (non_empty_str(part, "question"), non_empty_str(part, "value")) {
                    match non_empty_str(part, "page") {
                        Some(page) => lines.push(format!("- {q}: {v} ({page})")),
                        None => lines.push(format!("- {q}: {v}")),
                    }
                }
            }
        }
        if !lines.is_empty() {
            sections.push(format!("## Answer\n{}", lines.join("\n")));
        }
    }

    if let Some(rows) = data.get("rows").and_then(Value::as_array) {
        let mut lines = Vec::new();
        for row in rows {
            let name = non_empty_str(row, "name");
            let page = non_empty_str(row, "page");
            let snippet = non_empty_str(row, "snippet");
            let mut item = match (name, page) {
                (Some(n), Some(p)) => vec![format!("- {n} ({p})")],
                (Some(n), None) => vec![format!("- {n}")],
                (None, Some(p)) => vec![format!("- {p}")],
                (None, None) => continue,
            };
            if let Some(snip) = snippet {
                item.push(format!("  {snip}"));
            }
            if let Some(fields) = row.get("fields").and_then(Value::as_object) {
                for (k, v) in fields {
                    if let Some(text) = scalar_text(v) {
                        item.push(format!("  - {k}: {text}"));
                    }
                }
            }
            lines.push(item.join("\n"));
        }
        if !lines.is_empty() {
            sections.push(format!("## Results\n{}", lines.join("\n")));
        }
    }

    let mut has_passages = false;
    if let Some(passages) = data.get("passages").and_then(Value::as_array) {
        let mut blocks = Vec::new();
        for passage in passages {
            let text = ["text", "passage", "content", "excerpt"]
                .iter()
                .find_map(|k| non_empty_str(passage, k));
            let Some(text) = text else { continue };
            let title = non_empty_str(passage, "title");
            let url = non_empty_str(passage, "url");
            let mut block = Vec::new();
            match (title, url) {
                (Some(t), Some(u)) => {
                    block.push(format!("### {t}"));
                    block.push(format!("URL: {u}"));
                }
                (Some(t), None) => block.push(format!("### {t}")),
                (None, Some(u)) => block.push(format!("### {u}")),
                (None, None) => {}
            }
            block.push(text.to_owned());
            blocks.push(block.join("\n"));
        }
        if !blocks.is_empty() {
            has_passages = true;
            sections.push(format!("## Passages\n{}", blocks.join("\n\n")));
        }
    }

    if let Some(sources) = data.get("sources").and_then(Value::as_array) {
        let mut lines = Vec::new();
        for source in sources {
            let title = non_empty_str(source, "title");
            let url = non_empty_str(source, "url");
            let mut header = match (title, url) {
                (Some(t), Some(u)) => format!("- [{t}]({u})"),
                (None, Some(u)) => format!("- {u}"),
                (Some(t), None) => format!("- {t}"),
                (None, None) => continue,
            };
            let published = non_empty_str(source, "published");
            let modified = non_empty_str(source, "modified");
            match (published, modified) {
                (Some(p), Some(m)) => header.push_str(&format!(" (published {p}, modified {m})")),
                (Some(p), None) => header.push_str(&format!(" (published {p})")),
                (None, Some(m)) => header.push_str(&format!(" (modified {m})")),
                (None, None) => {}
            }
            let detail = if has_passages {
                non_empty_str(source, "snippet")
            } else {
                non_empty_str(source, "excerpt").or_else(|| non_empty_str(source, "snippet"))
            };
            if let Some(text) = detail {
                lines.push(format!("{header}\n  {text}"));
            } else {
                lines.push(header);
            }
        }
        if !lines.is_empty() {
            sections.push(format!("## Sources\n{}", lines.join("\n")));
        }
    }

    if sections.is_empty() {
        "No web results found.".to_owned()
    } else {
        sections.join("\n\n")
    }
}

/// Perform web search using SpecStream's Search API (`GET /v1/web-search`).
/// `api_key`: explicit key when called directly, or None to read `SPECSTREAM_API_KEY`.
pub fn web_search<'py>(
    py: Python<'py>,
    query: &Bound<'py, PyAny>,
    api_key: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    search_error(py, (|| {
        let requests = py.import("requests")?;
        let timeout = import_from(py, "AutoCua.llm_provider", "LLM_HTTP_TIMEOUT")?;

        let key = key_or_env(py, api_key, "SPECSTREAM_API_KEY")?;
        let key_str = if key.is_none() {
            String::new()
        } else {
            str_of(&key)?.extract::<String>()?
        };
        let key_trimmed = key_str.trim();
        if key_trimmed.is_empty() {
            return Err(PyValueError::new_err("SPECSTREAM_API_KEY is not set"));
        }
        // A key has no space or line break in it. One pasted with a line
        // break inside makes requests refuse the header with an error that
        // quotes the whole key, and that error goes back to the model, so it
        // is refused here with words that do not show it.
        if key_trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(PyValueError::new_err(
                "SPECSTREAM_API_KEY has a space or line break inside it: save the key again",
            ));
        }

        let raw_query: String = str_of(query)?.extract()?;
        let trimmed_query = raw_query.trim();
        if trimmed_query.is_empty() {
            return Err(PyValueError::new_err("Search query is empty"));
        }
        let bounded_query: String = trimmed_query.chars().take(MAX_QUERY_CHARS).collect();

        let request = kwargs(py, json!({
            "headers": {
                "Authorization": null,
                "Accept": "application/json"
            },
            "params": {
                "q": null,
                "passages": "true"
            }
        }))?;
        let request_any = request.as_any();
        request_any
            .get_item("headers")?
            .set_item("Authorization", fill(py, "Bearer {}", (key_trimmed,))?)?;
        request_any.get_item("params")?.set_item("q", &bounded_query)?;
        request.set_item("timeout", timeout)?;

        let response = requests.call_method("get", (SEARCH_URL,), Some(&request))?;
        if !response.getattr("ok")?.is_truthy()? {
            let status: u16 = response.getattr("status_code")?.extract()?;
            let body: String = response.getattr("text")?.extract()?;
            if let Ok(parsed) = serde_json::from_str::<Value>(&body) {
                if let Some(msg) = parsed
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    return Err(PyException::new_err(format!("HTTP {status}: {msg}")));
                }
            }
            response.call_method0("raise_for_status")?;
        }

        let body: String = response.getattr("text")?.extract()?;
        let payload: Value = serde_json::from_str(&body)
            .map_err(|e| PyValueError::new_err(format!("Invalid JSON response from SpecStream: {e}")))?;
        Ok(PyString::new(py, &format_response(&payload)).into_any())
    })())
}
