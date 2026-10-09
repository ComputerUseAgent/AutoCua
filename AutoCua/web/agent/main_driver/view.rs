//! NATIVE TRANSCRIPT CODEC + display formatter — Rust port of view.py.
//!
//! A step is persisted as two aligned strings: the assistant turn (its prose
//! plus the tool_calls it made) and the matching results — both JSON, so the
//! turn is rebuilt EXACTLY on the next request. Anything that ISN'T our JSON —
//! a bridge note, a legacy "<Step_no=N />\n{...}" step, a request marker —
//! degrades to plain text so resumed conversations keep replaying.
//!
//! All JSON here is emitted with Python's json.dumps separators (", " / ": ")
//! so entries persisted by this port stay byte-compatible with ones the
//! Python implementation wrote (and vice versa on resume).

use std::io;
use std::sync::OnceLock;

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Python-json.dumps-compatible serialization
// ---------------------------------------------------------------------------

/// Compact with Python's default separators: `{"a": 1, "b": 2}`.
struct PySeparators;

impl serde_json::ser::Formatter for PySeparators {
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { w.write_all(b", ") }
    }
}

/// json.dumps(value, ensure_ascii=False) with default separators.
pub fn dumps(v: &Value) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PySeparators);
    v.serialize(&mut ser).expect("serde_json::Value is always serializable");
    String::from_utf8(out).expect("serde_json emits UTF-8")
}

/// json.dumps(value, indent=2, ensure_ascii=False).
pub fn dumps_pretty(v: &Value) -> String {
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    v.serialize(&mut ser).expect("serde_json::Value is always serializable");
    String::from_utf8(out).expect("serde_json emits UTF-8")
}

/// Post-pass for json.dumps' default ensure_ascii=True: every non-ASCII char
/// becomes \uXXXX (surrogate pairs above the BMP). Safe as a post-pass since
/// non-ASCII bytes only ever occur inside JSON string literals.
pub fn ensure_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if cp < 0x80 {
            out.push(ch);
        } else if cp <= 0xFFFF {
            out.push_str(&format!("\\u{cp:04x}"));
        } else {
            let v = cp - 0x10000;
            out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)));
        }
    }
    out
}

/// Python truthiness for a JSON value (shared with browser.rs).
pub use crate::browser::truthy;

/// Python `str()` of a JSON value ("None"/"True"/"False", numbers via repr).
pub fn py_str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        other => dumps(other), // str(dict/list) approximation — never hit by our own data
    }
}

/// Python `str(x or "")` — falsy values collapse to "".
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => py_str_of(v),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------

pub struct Step {
    pub content: String,
    /// Always a JSON array.
    pub tool_calls: Value,
    /// Always a JSON object.
    pub meta: Value,
}

/// Persisted assistant turn -> content + tool_calls + provider meta. `meta`
/// is {} for turns saved before it existed and for providers that emit none.
pub fn decode_step(raw: &str) -> Step {
    let fallback = |raw: &str| Step {
        content: raw.to_string(),
        tool_calls: json!([]),
        meta: json!({}),
    };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(raw) else {
        return fallback(raw);
    };
    if let Some(Value::Array(calls)) = data.get("tool_calls") {
        if data.contains_key("content") || !calls.is_empty() {
            let meta = match data.get("provider_meta") {
                Some(Value::Object(m)) => Value::Object(m.clone()),
                _ => json!({}),
            };
            return Step {
                content: py_str_or_empty(data.get("content")),
                tool_calls: Value::Array(calls.clone()),
                meta,
            };
        }
    }
    // Legacy 4-block schema step (or a bridge note) — replay its prose.
    fallback(raw)
}

/// Assistant turn -> persisted string. `meta` is the PROVIDER's opaque
/// per-turn metadata, written only when non-empty.
pub fn encode_step(text: &str, calls: &Value, meta: Option<&Value>) -> String {
    let mut data = Map::new();
    data.insert("content".into(), Value::String(text.to_string()));
    let calls_out = match calls {
        Value::Array(_) => calls.clone(),
        _ => json!([]),
    };
    data.insert("tool_calls".into(), calls_out);
    if let Some(m) = meta {
        if truthy(m) {
            data.insert("provider_meta".into(), m.clone());
        }
    }
    dumps(&Value::Object(data))
}

/// Slot strings that must replay BARE (not re-wrapped): run-boundary request
/// markers, already-wrapped legacy results, and the compression note.
const BARE_SLOT_PREFIXES: &[&str] = &[
    "<updated_user_request",
    "<tool_response",
    "<history_compressed",
];

/// Persisted results -> the `role: "tool"` messages for one step. A legacy
/// plain string replays as a USER turn, re-wrapped in <tool_response> unless
/// it is a request marker (or already wrapped).
pub fn decode_results(raw: &str) -> Vec<Value> {
    if raw.is_empty() {
        return Vec::new();
    }
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) {
        let all_keyed = items.iter().all(|d| {
            d.as_object()
                .map(|o| o.contains_key("tool_call_id"))
                .unwrap_or(false)
        });
        if all_keyed {
            return items
                .iter()
                .map(|d| {
                    json!({
                        "role": "tool",
                        "tool_call_id": d["tool_call_id"].clone(),
                        "content": py_str_or_empty(d.get("content")),
                    })
                })
                .collect();
        }
    }
    let trimmed = raw.trim_start();
    let text = if BARE_SLOT_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        raw.to_string()
    } else {
        format!("<tool_response>\n{raw}\n</tool_response>")
    };
    vec![json!({"role": "user", "content": text})]
}

/// [{"tool_call_id", "content"}] -> persisted string.
pub fn encode_results(results: &[Value]) -> String {
    dumps(&Value::Array(results.to_vec()))
}

/// Normalized calls -> the OpenAI-shaped `tool_calls` the assistant turn
/// carries on the wire. Arguments are re-serialized EXACTLY as the model sent
/// them (tracking params included).
pub fn wire_calls_from(calls: &[Value]) -> Result<Vec<Value>, String> {
    calls
        .iter()
        .map(|c| {
            let id = c.get("id").ok_or("'id'")?.clone();
            let name = c.get("name").ok_or("'name'")?.clone();
            let args = match c.get("arguments") {
                Some(v) if truthy(v) => v.clone(),
                _ => json!({}),
            };
            Ok(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": dumps(&args)},
            }))
        })
        .collect()
}

/// What the snapshot shows for `thinking` on a step that had none. The model
/// never writes this word: a route step omits the field (or passes ""), and
/// the renderer fills it in so a reader can tell "nothing thought, on purpose"
/// from "nothing recorded".
pub const SKIPPED_THINKING: &str = "skipped";

/// The `reasoning` call's result on the wire. Every provider needs a result
/// for every call; this is the smallest one. The snapshot and the compression
/// dump hide it.
pub const REASONING_RESULT: &str = "ok";

/// DISPLAY-ONLY rendering of one native assistant turn as the familiar
/// four-block shape {"thinking", "memory", "next_goal", "action"}. The notes
/// are pulled OUT of the call arguments (the `reasoning` call in quality
/// mode, the first action call in fast mode and in older saves) and shown as
/// their own fields; the notes call itself is NOT listed under action, and a
/// missing `thinking` renders as "skipped". A fast turn (its notes on the
/// `memory` call, `next_goal` alone) renders only {"next_goal", "action"}.
/// Returned as a Value; snapshot_turn() renders it with indent=2.
pub fn snapshot_value(text: &str, calls: &Value) -> Value {
    let mut actions: Vec<Value> = Vec::new();
    let mut fast_notes = false;
    let mut thinking = String::new();
    let mut memory = String::new();
    let mut next_goal = String::new();
    if let Value::Array(list) = calls {
        for tc in list {
            let func = tc
                .get("function")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut args: Map<String, Value> = match func.get("arguments") {
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                    Ok(Value::Object(o)) => o,
                    Ok(_) => Map::new(),
                    Err(_) => {
                        let mut m = Map::new();
                        m.insert("raw_arguments".into(), Value::String(s.clone()));
                        m
                    }
                },
                Some(Value::Object(o)) => o.clone(),
                _ => Map::new(),
            };
            // Pop UNCONDITIONALLY, then keep the first non-empty value —
            // later calls carry "" and must not render as bogus action fields.
            // shift_remove, NOT remove: with preserve_order, plain remove is a
            // swap_remove that scrambles the remaining keys' order.
            let step_thinking = py_str_or_empty(args.shift_remove("thinking").as_ref()).trim().to_string();
            // Claude's `assessment` renders in the thinking slot.
            let step_assessment = py_str_or_empty(args.shift_remove("assessment").as_ref()).trim().to_string();
            let step_memory = py_str_or_empty(args.shift_remove("memory").as_ref()).trim().to_string();
            let step_next_goal = py_str_or_empty(args.shift_remove("next_goal").as_ref()).trim().to_string();
            if thinking.is_empty() {
                thinking = if step_thinking.is_empty() { step_assessment } else { step_thinking };
            }
            if memory.is_empty() {
                memory = step_memory;
            }
            if next_goal.is_empty() {
                next_goal = step_next_goal;
            }
            let name_str = func.get("name").and_then(Value::as_str).unwrap_or("");
            if name_str == "memory" {
                fast_notes = true;
            }
            // `agent_memory` is Claude's notes tool (quality mode).
            if name_str == "reasoning" || name_str == "agent_memory" || name_str == "memory" {
                continue; // its whole content is the note fields above
            }
            let mut action = Map::new();
            let name = match func.get("name") {
                Some(v) if truthy(v) => v.clone(),
                _ => Value::String(String::new()),
            };
            action.insert("type".into(), name);
            for (k, v) in args {
                // run_script's own `type` argument ("action" or "scrape") is
                // shown as the parser keeps it, `script_type`, so the action
                // keeps the tool's name.
                let k = if k == "type" { "script_type".to_string() } else { k };
                action.insert(k, v);
            }
            actions.push(Value::Object(action));
        }
    }
    if fast_notes {
        return json!({"next_goal": next_goal, "action": actions});
    }
    let thinking_final = if !thinking.is_empty() {
        thinking
    } else {
        let t = text.trim();
        if t.is_empty() { SKIPPED_THINKING.to_string() } else { t.to_string() }
    };
    json!({
        "thinking": thinking_final,
        "memory": memory,
        "next_goal": next_goal,
        "action": actions,
    })
}

pub fn snapshot_turn(text: &str, calls: &Value) -> String {
    dumps_pretty(&snapshot_value(text, calls))
}

// ---------------------------------------------------------------------------
// HANDOFF COMPRESSION (main-driver flavor) — the two callables
// CompressionController takes. Handles MIXED histories: legacy schema-era
// steps render like the old default builder, native steps via snapshot_turn.
// ---------------------------------------------------------------------------

fn re_step_prefix() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)^(<Step_no=\d+ />\n)(.*)").unwrap())
}

/// Tool slot paired with the synthetic handoff entry: a plain string that
/// replays as a bare user turn — never an orphaned role:"tool" result.
pub const COMPRESSION_NOTE: &str = "<history_compressed>\nEarlier steps of this session were \
compressed into the handoff document above. Trust it as \
accurate memory and continue from the state it describes.\n\
</history_compressed>";

fn clip(value: &Value, limit: usize) -> String {
    let s = py_str_of(value);
    let count = s.chars().count();
    if count <= limit {
        s
    } else {
        let head: String = s.chars().take(limit).collect();
        format!("{head}... [clipped {} chars]", count - limit)
    }
}

pub fn looks_native(raw: &str) -> bool {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(d)) => {
            d.contains_key("content") && matches!(d.get("tool_calls"), Some(Value::Array(_)))
        }
        _ => false,
    }
}

/// A schema-era entry's JSON body, with any '<Step_no=N />' prefix removed.
fn legacy_dump_body(raw_entry: &str) -> &str {
    match re_step_prefix().captures(raw_entry) {
        Some(m) => m.get(2).map(|g| g.as_str()).unwrap_or(raw_entry),
        None => raw_entry,
    }
}

/// One persisted assistant entry -> compressor-readable text.
fn dump_step(raw_entry: &str, full: bool) -> String {
    let step = decode_step(raw_entry);
    let has_calls = matches!(&step.tool_calls, Value::Array(a) if !a.is_empty());
    if has_calls || looks_native(raw_entry) {
        let mut rendered = snapshot_value(&step.content, &step.tool_calls);
        if !full {
            if let Some(Value::Array(actions)) = rendered.get("action").cloned() {
                let clipped: Vec<Value> = actions
                    .iter()
                    .map(|action| match action.as_object() {
                        Some(o) => {
                            let mut m = Map::new();
                            for (k, v) in o {
                                m.insert(k.clone(), Value::String(clip(v, 300)));
                            }
                            Value::Object(m)
                        }
                        None => action.clone(),
                    })
                    .collect();
                rendered["action"] = Value::Array(clipped);
            }
        }
        return dumps_pretty(&rendered);
    }
    let body = legacy_dump_body(raw_entry);
    if full {
        return body.to_string();
    }
    let Ok(Value::Object(mut data)) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    for key in ["thinking", "eval", "action"] {
        data.shift_remove(key); // order-preserving, like dict.pop
    }
    dumps_pretty(&Value::Object(data))
}

// -- Python bridge to memory_compression.agent.service ----------------------

fn mc_attr(py: Python<'_>, cell: &PyOnceLock<Py<PyAny>>, name: &str) -> PyResult<Py<PyAny>> {
    cell.get_or_try_init(py, || {
        py.import("AutoCua.memory_compression.agent.service")?
            .getattr(name)
            .map(Into::into)
    })
    .map(|f| f.clone_ref(py))
}

pub fn wrap_handoff(py: Python<'_>, text: &str) -> PyResult<String> {
    static F: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    mc_attr(py, &F, "wrap_handoff")?.call1(py, (text,))?.extract(py)
}

pub fn extract_handoff(py: Python<'_>, text: &str) -> PyResult<Option<String>> {
    static F: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    mc_attr(py, &F, "extract_handoff")?.call1(py, (text,))?.extract(py)
}

/// A previous compression's handoff doc, whichever generation wrote it:
/// native synthetic entries carry it in their CONTENT; legacy ones carried it
/// in the step's `memory` field.
fn prior_handoff(py: Python<'_>, entry0: &str) -> PyResult<Option<String>> {
    let step = decode_step(entry0);
    let has_calls = matches!(&step.tool_calls, Value::Array(a) if !a.is_empty());
    if has_calls || looks_native(entry0) {
        return extract_handoff(py, &step.content);
    }
    let Ok(data) = serde_json::from_str::<Value>(legacy_dump_body(entry0)) else {
        return Ok(None);
    };
    if let Value::Object(map) = data {
        return extract_handoff(py, &py_str_or_empty(map.get("memory")));
    }
    Ok(None)
}

/// Main-driver flavor of memory_compression's build_dump: steps [0..k] in the
/// compressor prompt's <input> format, mixed-generation aware. MAIN-THREAD
/// ONLY: snapshots the list content into one string so the worker thread
/// never reads the live lists.
#[pyfunction]
pub fn compression_dump(
    py: Python<'_>,
    assistant_messages: Bound<'_, PyAny>,
    tool_responses: Bound<'_, PyAny>,
    k: i64,
    task: Bound<'_, PyAny>,
) -> PyResult<String> {
    let take = (k + 1).max(0) as usize;
    let mut entries: Vec<String> = Vec::new();
    for (i, item) in assistant_messages.try_iter()?.enumerate() {
        if i >= take {
            break;
        }
        entries.push(item?.str()?.extract()?);
    }
    let mut tools: Vec<Option<String>> = Vec::new();
    for (i, item) in tool_responses.try_iter()?.enumerate() {
        if i >= take {
            break;
        }
        let obj = item?;
        if obj.is_truthy()? {
            tools.push(Some(obj.str()?.extract()?));
        } else {
            tools.push(None);
        }
    }
    while tools.len() < entries.len() {
        tools.push(None);
    }
    let task: String = task.str()?.extract()?;

    let mut out: Vec<String> = vec![
        "Session: live (in-run rolling compression)".to_string(),
        format!("Task: {task}"),
        "Trigger: rolling".to_string(),
    ];

    // PREVIOUS HANDOFF: entry 0 is a prior synthetic entry when it carries a
    // <handoff> — lift the doc into its own section and skip the entry so it
    // isn't summarized twice.
    let mut start = 0usize;
    let prev = if !entries.is_empty() {
        prior_handoff(py, &entries[0])?
    } else {
        None
    };
    if let Some(prev) = prev {
        out.push(String::new());
        out.push("=== PREVIOUS HANDOFF ===".to_string());
        out.push(prev);
        start = 1;
    }

    out.push(String::new());
    out.push("--- USER ---".to_string());
    out.push(format!(
        "<updated_user_request no=\"1\">\n{task}\n</updated_user_request no=\"1\">"
    ));

    let mut step_no = 0i64;
    let last = entries.len().saturating_sub(1);
    for i in start..entries.len() {
        step_no += 1;
        out.push(String::new());
        out.push("--- ASSISTANT ---".to_string());
        out.push(format!("<Step_no={step_no} />"));
        out.push(dump_step(&entries[i], i == last));
        let Some(tr) = &tools[i] else { continue };
        let results = decode_results(tr);
        let first_is_tool = results
            .first()
            .and_then(|r| r.get("role"))
            .and_then(Value::as_str)
            == Some("tool");
        if !results.is_empty() && first_is_tool {
            // The `reasoning` call's bare `ok` is wire plumbing, not a result.
            let contents: Vec<String> = results
                .iter()
                .map(|r| py_str_or_empty(r.get("content")))
                .filter(|c| c.as_str() != REASONING_RESULT)
                .collect();
            if contents.is_empty() {
                continue;
            }
            let joined = contents.join("\n");
            out.push(String::new());
            out.push("--- USER ---".to_string());
            out.push(format!("<tool_response>\n{joined}\n</tool_response>"));
        } else {
            let trimmed = tr.trim_start();
            if trimmed.starts_with("<updated_user_request") {
                step_no = 0; // step numbering restarts after every new task
                out.push(String::new());
                out.push("--- USER ---".to_string());
                out.push(tr.clone());
            } else if trimmed.starts_with("<tool_response") {
                out.push(String::new());
                out.push("--- USER ---".to_string());
                out.push(tr.clone());
            } else {
                out.push(String::new());
                out.push("--- USER ---".to_string());
                out.push(format!("<tool_response>\n{tr}\n</tool_response>"));
            }
        }
    }
    Ok(out.join("\n") + "\n")
}

/// (entry, tool_slot) replacing entries [0..k] after a handoff lands: ONE
/// content-only native assistant turn carrying the wrapped handoff, paired
/// with the plain-string note slot. step_k_entry is unused — signature parity
/// with the controller's default.
#[pyfunction]
#[pyo3(signature = (step_k_entry, handoff_text))]
pub fn compression_entry(
    py: Python<'_>,
    step_k_entry: Bound<'_, PyAny>,
    handoff_text: Bound<'_, PyAny>,
) -> PyResult<(String, String)> {
    let _ = step_k_entry;
    let text: String = handoff_text.str()?.extract()?;
    let wrapped = wrap_handoff(py, &text)?;
    Ok((
        encode_step(&wrapped, &json!([]), None),
        COMPRESSION_NOTE.to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Python-facing wrappers for the codec (differential testing + facade parity)
// ---------------------------------------------------------------------------

fn any_to_value(v: &Bound<'_, PyAny>) -> PyResult<Value> {
    pythonize::depythonize(v).map_err(Into::into)
}

#[pyfunction(name = "decode_step")]
pub fn decode_step_py<'py>(py: Python<'py>, raw: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let raw: String = raw.str()?.extract()?;
    let step = decode_step(&raw);
    let out = json!({"content": step.content, "tool_calls": step.tool_calls, "meta": step.meta});
    pythonize::pythonize(py, &out).map_err(Into::into)
}

#[pyfunction(name = "encode_step")]
#[pyo3(signature = (text, calls, meta=None))]
pub fn encode_step_py(
    text: Option<Bound<'_, PyAny>>,
    calls: Option<Bound<'_, PyAny>>,
    meta: Option<Bound<'_, PyAny>>,
) -> PyResult<String> {
    let text = match &text {
        Some(v) if v.is_truthy()? => v.str()?.extract::<String>()?,
        _ => String::new(),
    };
    let calls = match &calls {
        Some(v) if !v.is_none() => any_to_value(v)?,
        _ => json!([]),
    };
    let meta = match &meta {
        Some(v) if !v.is_none() => Some(any_to_value(v)?),
        _ => None,
    };
    Ok(encode_step(&text, &calls, meta.as_ref()))
}

#[pyfunction(name = "decode_results")]
#[pyo3(signature = (raw=None))]
pub fn decode_results_py<'py>(
    py: Python<'py>,
    raw: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let text = match &raw {
        Some(v) if v.is_truthy()? => v.str()?.extract::<String>()?,
        _ => String::new(),
    };
    let results = decode_results(&text);
    pythonize::pythonize(py, &Value::Array(results)).map_err(Into::into)
}

#[pyfunction(name = "encode_results")]
#[pyo3(signature = (results=None))]
pub fn encode_results_py(results: Option<Bound<'_, PyAny>>) -> PyResult<String> {
    let values = match &results {
        Some(v) if !v.is_none() => match any_to_value(v)? {
            Value::Array(a) => a,
            other if !truthy(&other) => Vec::new(),
            other => vec![other],
        },
        _ => Vec::new(),
    };
    Ok(encode_results(&values))
}

#[pyfunction(name = "wire_calls_from")]
#[pyo3(signature = (calls=None))]
pub fn wire_calls_from_py<'py>(
    py: Python<'py>,
    calls: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let values = match &calls {
        Some(v) if !v.is_none() => match any_to_value(v)? {
            Value::Array(a) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let wired = wire_calls_from(&values)
        .map_err(|k| pyo3::exceptions::PyKeyError::new_err(k.to_string()))?;
    pythonize::pythonize(py, &Value::Array(wired)).map_err(Into::into)
}

#[pyfunction(name = "snapshot_turn")]
pub fn snapshot_turn_py(
    text: Option<Bound<'_, PyAny>>,
    calls: Option<Bound<'_, PyAny>>,
) -> PyResult<String> {
    let text = match &text {
        Some(v) if !v.is_none() => v.str()?.extract::<String>()?,
        _ => String::new(),
    };
    let calls = match &calls {
        Some(v) if !v.is_none() => any_to_value(v)?,
        _ => json!([]),
    };
    Ok(snapshot_turn(&text, &calls))
}

#[pyfunction(name = "_looks_native")]
pub fn looks_native_py(raw: Bound<'_, PyAny>) -> PyResult<bool> {
    let raw: String = raw.str()?.extract()?;
    Ok(looks_native(&raw))
}

// ---------------------------------------------------------------------------
// AgentResponseFormatter — terminal/frontend display of agent JSON responses
// ---------------------------------------------------------------------------

/// (field, emoji label) in the class's source order — the order that drives
/// format_response's output lines.
const FIELD_EMOJIS: &[(&str, &str)] = &[
    ("assessment", "🧠 Assessment"),
    ("thinking", "🧠 Thinking"),
    ("next_goal", "🎯 Next Goal"),
    ("memory", "💾 Memory"),
    ("action", "⚡ Action"),
];

#[pyclass(frozen)]
pub struct AgentResponseFormatter;

pub fn extract_tools_impl(normalized_response: &str) -> Vec<Value> {
    let mut tools: Vec<Value> = Vec::new();
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(normalized_response) else {
        return tools;
    };
    let actions: Vec<Value> = match data.get("action") {
        Some(Value::Object(o)) => vec![Value::Object(o.clone())],
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    for item in actions {
        let Some(obj) = item.as_object() else { continue };
        let name = match obj.get("type") {
            Some(v) if truthy(v) => v.clone(),
            _ => continue,
        };
        let mut tool = Map::new();
        tool.insert("name".into(), name);
        if let Some(t) = obj.get("time") {
            tool.insert("time".into(), t.clone());
        }
        if let Some(e) = obj.get("enter") {
            tool.insert("enter".into(), e.clone());
        }
        tools.push(Value::Object(tool));
    }
    tools
}

pub fn format_response_impl(normalized_response: &str, include_action: bool) -> String {
    let parsed = serde_json::from_str::<Value>(normalized_response);
    let data = match parsed {
        Ok(Value::Object(data)) => data,
        // Python quirk fidelity: `field in data` on a parsed non-dict is a
        // membership/substring test, not a KeyError — a list or string that
        // happens to contain a field name then raises on data[field] (original
        // returned); otherwise every field misses and the output is "".
        Ok(Value::Array(items)) => {
            let hit = FIELD_EMOJIS.iter().any(|(f, _)| {
                items.iter().any(|it| it.as_str() == Some(*f))
            });
            return if hit { normalized_response.to_string() } else { String::new() };
        }
        Ok(Value::String(s)) => {
            let hit = FIELD_EMOJIS.iter().any(|(f, _)| s.contains(*f));
            return if hit { normalized_response.to_string() } else { String::new() };
        }
        _ => return normalized_response.to_string(),
    };
    let mut lines: Vec<String> = Vec::new();
    for (field, emoji_label) in FIELD_EMOJIS {
        let Some(value) = data.get(*field) else { continue };
        // Skip action field unless include_action (frontend should not stream action)
        if *field == "action" && !include_action {
            continue;
        }
        let rendered = match value {
            Value::Object(_) | Value::Array(_) => ensure_ascii(&dumps_pretty(value)),
            other => py_str_of(other),
        };
        lines.push(format!("- {emoji_label}: {rendered}"));
    }
    lines.join("\n")
}

#[pymethods]
impl AgentResponseFormatter {
    #[new]
    fn new() -> Self {
        AgentResponseFormatter
    }

    #[classattr]
    #[pyo3(name = "FIELD_EMOJIS")]
    fn field_emojis(py: Python<'_>) -> PyResult<Py<PyDict>> {
        let d = PyDict::new(py);
        for (field, label) in FIELD_EMOJIS {
            d.set_item(field, label)?;
        }
        Ok(d.into())
    }

    /// Pull this turn's tools from the parsed action block, in execution
    /// order, for the frontend tool-flow chain.
    #[staticmethod]
    fn extract_tools<'py>(
        py: Python<'py>,
        normalized_response: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let text: String = match normalized_response.str() {
            Ok(s) => s.extract()?,
            Err(_) => return Ok(PyList::empty(py).into_any()),
        };
        let tools = extract_tools_impl(&text);
        pythonize::pythonize(py, &Value::Array(tools)).map_err(Into::into)
    }

    /// Format normalized JSON response into readable terminal output with
    /// emojis. include_action: If True, include the action block (terminal);
    /// if False, omit it (frontend stream).
    #[staticmethod]
    #[pyo3(signature = (normalized_response, include_action=false))]
    fn format_response(normalized_response: Bound<'_, PyAny>, include_action: bool) -> PyResult<String> {
        let text: String = normalized_response.str()?.extract()?;
        Ok(format_response_impl(&text, include_action))
    }
}
