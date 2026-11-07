//! The coder's transcript codec, its two compression callables and its
//! terminal display: the Rust copy of mac/agent/coder/view.py.
//!
//! A step is persisted as two aligned strings: the assistant turn (its prose
//! plus the tool_calls it made) and the matching results. Both are JSON, so
//! the turn is rebuilt EXACTLY on the next request: the model sees itself
//! having called tools in the shape every provider was trained on, not a
//! text transcript describing that it did. Anything that is not our JSON (a
//! bridge note, a legacy envelope step, a request marker) degrades to plain
//! text, so resumed conversations keep replaying.
//!
//! Every JSON string here is written like Python's json.dumps (", " and
//! ": ", ensure_ascii=False), so an entry saved by this port and one saved
//! by the Python coder are the same bytes.

use std::sync::OnceLock;

use pyo3::prelude::*;
use pyo3::types::{PySlice, PyString};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::{dumps, dumps_pretty, py_str_of, py_strip, truthy};

/// Python `str(x or "")`: a missing or falsy value gives "".
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => py_str_of(v),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// The native transcript codec (the subagent uses it too)
// ---------------------------------------------------------------------------

/// One persisted assistant turn, decoded.
pub struct Step {
    pub content: String,
    /// Always a JSON array.
    pub tool_calls: Value,
    /// Always a JSON object.
    pub meta: Value,
}

/// Persisted assistant turn -> content, tool_calls and meta. `meta` is the
/// provider's own opaque per-turn metadata (see encode_step); it is {} for
/// every turn saved before it existed and for providers that emit none, so
/// old history replays exactly as before.
pub fn decode_step(raw: &str) -> Step {
    let fallback = || Step {
        content: raw.to_string(),
        tool_calls: json!([]),
        meta: json!({}),
    };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(raw) else {
        return fallback();
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
    // Legacy 4-block envelope step (or a bridge note): replay its prose.
    fallback()
}

/// Assistant turn -> persisted string. `calls` are OpenAI-shaped:
/// {"id", "type": "function", "function": {"name", "arguments": json}}.
///
/// `meta` is the PROVIDER's opaque per-turn metadata, carried so the turn can
/// be echoed back exactly as the model produced it: Gemini 3 thought
/// signatures, OpenRouter reasoning blocks. Reasoning-model tool calling
/// breaks without it: Gemini 3 binds a function RESULT to the call through
/// the signature on that call. Written only when non-empty, and each
/// provider tags and checks its own name, so a resumed chat can never hand
/// one provider another's blob.
pub fn encode_step(text: &str, calls: &Value, meta: Option<&Value>) -> String {
    let mut data = Map::new();
    data.insert("content".into(), Value::String(text.to_string()));
    let calls = if truthy(calls) { calls.clone() } else { json!([]) };
    data.insert("tool_calls".into(), calls);
    if let Some(m) = meta.filter(|m| truthy(m)) {
        data.insert("provider_meta".into(), m.clone());
    }
    dumps(&Value::Object(data))
}

/// Persisted results -> the `role: "tool"` messages for one step. A legacy
/// plain string replays as a user turn, unchanged, so old saves still work.
/// "" stands for Python's None (no results).
pub fn decode_results(raw: &str) -> Vec<Value> {
    if raw.is_empty() {
        return Vec::new();
    }
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) {
        let all_keyed = items
            .iter()
            .all(|d| d.as_object().is_some_and(|o| o.contains_key("tool_call_id")));
        if all_keyed {
            return items
                .iter()
                .map(|d| {
                    json!({
                        "role": "tool",
                        "tool_call_id": d.get("tool_call_id").cloned().unwrap_or(Value::Null),
                        "content": py_str_or_empty(d.get("content")),
                    })
                })
                .collect();
        }
    }
    vec![json!({"role": "user", "content": raw})]
}

/// [{"tool_call_id", "content"}] -> persisted string.
pub fn encode_results(results: &[Value]) -> String {
    dumps(&Value::Array(results.to_vec()))
}

/// Normalized calls -> the OpenAI-shaped `tool_calls` the assistant turn
/// carries on the wire. Arguments are re-serialized EXACTLY as the model sent
/// them (tracking params included) so the echoed transcript never
/// contradicts the schema that required them. A call without "id" or "name"
/// is Python's KeyError: Err("'id'") or Err("'name'").
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

/// The compressed-memory entry: after a handoff lands, steps [0..k] are
/// replaced by ONE assistant turn whose content is
/// {"memory_compression": "<document>"} and nothing else.
pub const COMPRESSION_KEY: &str = "memory_compression";

/// What the snapshot shows for `thinking` on a step that had none. The model
/// never writes this word: a route step omits the field (or passes ""), and
/// the renderer fills it in so a reader can tell "nothing thought, on
/// purpose" from "nothing recorded".
pub const SKIPPED_THINKING: &str = "skipped";

/// The three note fields, in the snapshot's order.
const NOTE_KEYS: [&str; 3] = ["thinking", "memory", "next_goal"];

/// A wire call's arguments as an object: the JSON text decoded (text that is
/// not JSON is kept as `raw_arguments`, JSON that is not an object gives
/// none), or an object as it is.
fn call_args(func: &Map<String, Value>) -> Map<String, Value> {
    match func.get("arguments") {
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
    }
}

/// True for a compressed-memory entry: a turn with no calls whose text is
/// {"memory_compression": ...}.
fn is_compression_entry(text: &str, calls: &Value) -> bool {
    !truthy(calls)
        && matches!(serde_json::from_str::<Value>(text), Ok(Value::Object(data)) if data.contains_key(COMPRESSION_KEY))
}

/// DISPLAY-ONLY rendering of one assistant turn as ONE JSON object:
/// {"thinking", "memory", "next_goal", "action": [{type, ...}]}, always these
/// four keys. The `reasoning` (or `agent_memory`) call is folded into the
/// first three fields and is NOT listed under action; the other calls are
/// listed in order. Older saves that carried the notes as arguments on every
/// call render the same way. `thinking` renders as "skipped" when the step
/// had none and wrote no message. A compressed-memory entry renders as
/// itself. The API payload is untouched: the model keeps receiving the
/// native transcript. Only replayed turns that carry a `reasoning` call
/// render this way now; coder_snapshot_value renders every other turn.
pub fn snapshot_value(text: &str, calls: &Value) -> Value {
    if !truthy(calls) {
        if let Ok(Value::Object(data)) = serde_json::from_str::<Value>(text) {
            if data.contains_key(COMPRESSION_KEY) {
                return Value::Object(data);
            }
        }
    }
    let list: &[Value] = match calls {
        Value::Array(list) => list,
        _ => &[],
    };
    let mut notes = [String::new(), String::new(), String::new()];
    let mut actions: Vec<Value> = Vec::new();
    for tc in list {
        let func = tc
            .get("function")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut args = call_args(&func);
        // Pop UNCONDITIONALLY, then keep the first non-empty value: later
        // calls carry "" and must not render as bogus action fields.
        // shift_remove, not remove: remove is a swap_remove under
        // preserve_order and would reorder the other arguments.
        for (i, key) in NOTE_KEYS.iter().enumerate() {
            let value = py_strip(&py_str_or_empty(args.shift_remove(*key).as_ref())).to_string();
            if notes[i].is_empty() {
                notes[i] = value;
            }
        }
        let name = func.get("name").and_then(Value::as_str).unwrap_or("");
        if name == "reasoning" || name == "agent_memory" {
            continue; // its whole content is the three fields above
        }
        let mut action = Map::new();
        let type_value = match func.get("name") {
            Some(v) if truthy(v) => v.clone(),
            _ => Value::String(String::new()),
        };
        action.insert("type".into(), type_value);
        for (k, v) in args {
            action.insert(k, v);
        }
        actions.push(Value::Object(action));
    }
    let [thinking, memory, next_goal] = notes;
    let thinking = if !thinking.is_empty() {
        thinking
    } else {
        let t = py_strip(text);
        if t.is_empty() { SKIPPED_THINKING.to_string() } else { t.to_string() }
    };
    json!({"thinking": thinking, "memory": memory, "next_goal": next_goal, "action": actions})
}

/// The coder's rendering of one assistant turn, for its snapshot file and
/// its compression dump. Only what the turn has is kept: "text" when it
/// wrote a message, "insight" ({"kind", "content"}) when it made one,
/// "next_goal" when it set one, "action" ([{type, ...}]) when it called other
/// tools. The `insight` and `next_goal` calls are folded into their keys and
/// are NOT listed under action; each field is the first non-empty one of its
/// calls, stripped. A turn that carries a `reasoning` (or `agent_memory`)
/// call, and a compressed-memory entry, render as snapshot_value renders them.
pub fn coder_snapshot_value(text: &str, calls: &Value) -> Value {
    let list: &[Value] = match calls {
        Value::Array(list) => list,
        _ => &[],
    };
    let name_of = |tc: &Value| {
        tc.get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    if is_compression_entry(text, calls)
        || list.iter().any(|tc| matches!(name_of(tc).as_str(), "reasoning" | "agent_memory"))
    {
        return snapshot_value(text, calls);
    }
    let mut insight: Option<(String, String)> = None;
    let mut next_goal = String::new();
    let mut actions: Vec<Value> = Vec::new();
    for tc in list {
        let func = tc
            .get("function")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let args = call_args(&func);
        let field = |key: &str| py_strip(&py_str_or_empty(args.get(key))).to_string();
        match name_of(tc).as_str() {
            "insight" => {
                let (kind, content) = insight.get_or_insert_with(Default::default);
                if kind.is_empty() {
                    *kind = field("kind");
                }
                if content.is_empty() {
                    *content = field("content");
                }
            }
            "next_goal" => {
                if next_goal.is_empty() {
                    next_goal = field("value");
                }
            }
            _ => {
                let mut action = Map::new();
                let type_value = match func.get("name") {
                    Some(v) if truthy(v) => v.clone(),
                    _ => Value::String(String::new()),
                };
                action.insert("type".into(), type_value);
                for (k, v) in args {
                    action.insert(k, v);
                }
                actions.push(Value::Object(action));
            }
        }
    }
    let mut out = Map::new();
    let message = py_strip(text);
    if !message.is_empty() {
        out.insert("text".into(), Value::String(message.to_string()));
    }
    if let Some(insight) = insight.and_then(|(kind, content)| insight_value(&kind, &content)) {
        out.insert("insight".into(), insight);
    }
    if !next_goal.is_empty() {
        out.insert("next_goal".into(), Value::String(next_goal));
    }
    if !actions.is_empty() {
        out.insert("action".into(), Value::Array(actions));
    }
    Value::Object(out)
}

/// An `insight` as it is saved and shown: {"kind", "content"} with only the
/// fields that have something in them, or None when both are empty.
pub fn insight_value(kind: &str, content: &str) -> Option<Value> {
    let mut map = Map::new();
    if !kind.is_empty() {
        map.insert("kind".into(), Value::String(kind.to_string()));
    }
    if !content.is_empty() {
        map.insert("content".into(), Value::String(content.to_string()));
    }
    (!map.is_empty()).then_some(Value::Object(map))
}

/// coder_snapshot_value() as JSON text with indent=2.
pub fn coder_snapshot_turn(text: &str, calls: &Value) -> String {
    dumps_pretty(&coder_snapshot_value(text, calls))
}

// ---------------------------------------------------------------------------
// HANDOFF COMPRESSION (coder flavor): the two callables CompressionController
// takes to serve the coder's NATIVE history. compression_dump renders steps
// [0..k] of the parallel lists into the compressor prompt's <input> format,
// and compression_entry builds the splice that replaces them.
// ---------------------------------------------------------------------------

/// A notes call's result on the wire (the `insight` and `next_goal` of the
/// coder and the subagent). Every provider needs a result for
/// every call, so it cannot be dropped; this is the smallest one. It carries
/// no information, so the snapshot file and the compression dump hide it.
pub const REASONING_RESULT: &str = "ok";

/// Tool slot paired with the synthetic handoff entry: a plain string, so
/// decode_results replays it as a USER turn. The splice never orphans a
/// role:"tool" result and never leaves two assistant turns adjacent.
pub const COMPRESSION_NOTE: &str = "<history_compressed>\nEarlier steps of this session were \
compressed into the memory_compression block above. Trust it as accurate memory and \
continue from the state it describes.\n</history_compressed>";

/// A compiled pattern; None only if it could not compile, which the callers
/// treat as "no match".
fn regex_once(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// memory_compression.agent.service.extract_handoff: the body of the first
/// <handoff> block, else None.
pub fn extract_handoff(text: &str) -> Option<String> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    let re = regex_once(&RE, r"(?s)<handoff>\n?(.*?)\n?</handoff>")?;
    re.captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// The handoff document inside a compressed-memory entry's content, else
/// None. Reads the {"memory_compression": ...} shape, and the older
/// <handoff> tag for chats saved before it.
pub fn compressed_memory(content: &str) -> Option<String> {
    if let Ok(Value::Object(data)) = serde_json::from_str::<Value>(content) {
        if let Some(Value::String(doc)) = data.get(COMPRESSION_KEY) {
            return Some(doc.clone());
        }
    }
    extract_handoff(content)
}

/// Python's _clip: str(value), cut to `limit` characters with a note of how
/// many were cut.
pub(crate) fn clip(value: &Value, limit: usize) -> String {
    let s = py_str_of(value);
    let count = s.chars().count();
    if count <= limit {
        s
    } else {
        let head: String = s.chars().take(limit).collect();
        format!("{head}... [clipped {} chars]", count - limit)
    }
}

/// True when `raw` is our native JSON: an object with "content" and a list
/// "tool_calls".
pub fn looks_native(raw: &str) -> bool {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(d)) => {
            d.contains_key("content") && matches!(d.get("tool_calls"), Some(Value::Array(_)))
        }
        _ => false,
    }
}

/// One persisted assistant entry -> compressor-readable text. Native turns
/// render as their snapshot (coder_snapshot_value: {insight, next_goal,
/// action}); older steps (`full` false) clip every action field to 300
/// characters, so commands and paths survive while file bodies don't. Bridge
/// notes and legacy prose replay verbatim (the compressor prompt knows those
/// shapes).
pub(crate) fn dump_step(raw_entry: &str, full: bool) -> String {
    let step = decode_step(raw_entry);
    let has_calls = matches!(&step.tool_calls, Value::Array(a) if !a.is_empty());
    if !has_calls && !looks_native(raw_entry) {
        return step.content;
    }
    let mut rendered = coder_snapshot_value(&step.content, &step.tool_calls);
    if !full {
        if let Value::Object(map) = &mut rendered {
            let clipped: Vec<Value> = match map.get("action") {
                Some(Value::Array(actions)) => actions
                    .iter()
                    .map(|action| match action.as_object() {
                        Some(o) => Value::Object(
                            o.iter()
                                .map(|(k, v)| (k.clone(), Value::String(clip(v, 300))))
                                .collect(),
                        ),
                        None => action.clone(),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // A compression entry has no "action": Python adds it, last.
            map.insert("action".into(), Value::Array(clipped));
        }
    }
    dumps_pretty(&rendered)
}

/// One `<user_request=N>` block found in a tool slot: its byte span, its
/// number as written and its body.
struct UserRequest<'a> {
    start: usize,
    end: usize,
    num: &'a str,
    body: &'a str,
}

/// Python's `_USER_REQ_RE.search(raw)` with
/// `<user_request=(\d+)>\n?(.*?)\n?</user_request=\1>` (DOTALL). The regex
/// crate has no backreference (`\1`), so each opening tag, left to right,
/// looks for its own closing tag by hand, with the same result. The
/// seed/bridge fill writes these tags into tool slots.
fn find_user_request(raw: &str) -> Option<UserRequest<'_>> {
    static OPEN: OnceLock<Option<Regex>> = OnceLock::new();
    let open = regex_once(&OPEN, r"<user_request=(\d+)>")?;
    let bytes = raw.as_bytes();
    for caps in open.captures_iter(raw) {
        let (Some(tag), Some(num)) = (caps.get(0), caps.get(1)) else {
            continue;
        };
        let num = num.as_str();
        let close = format!("</user_request={num}>");
        let after = tag.end();
        let body_start = if bytes.get(after) == Some(&b'\n') { after + 1 } else { after };
        let Some(found) = raw.get(body_start..).and_then(|rest| rest.find(&close)) else {
            continue;
        };
        let q = body_start + found;
        let body_end = if q > body_start && bytes.get(q - 1) == Some(&b'\n') { q - 1 } else { q };
        return Some(UserRequest {
            start: tag.start(),
            end: q + close.len(),
            num,
            body: raw.get(body_start..body_end).unwrap_or(""),
        });
    }
    None
}

/// compression_dump on plain Rust data: `entries` are str() of the first
/// k + 1 history entries, `tools` the matching slots (None for a falsy slot;
/// it may be shorter than `entries`).
pub fn compression_dump_core(entries: &[String], tools: &[Option<String>], task: &str) -> String {
    let mut out: Vec<String> = vec![
        "Session: live (in-run rolling compression, coder/shell session)".to_string(),
        format!("Task: {task}"),
        "Trigger: rolling".to_string(),
    ];

    // PREVIOUS HANDOFF: entry 0 is a prior compressed-memory entry when its
    // decoded content carries the document. Lift it into its own section and
    // skip the entry (and its boilerplate note slot) so it isn't summarized
    // twice.
    let mut start = 0usize;
    if let Some(prev) = entries
        .first()
        .and_then(|e| compressed_memory(&decode_step(e).content))
    {
        out.push(String::new());
        out.push("=== PREVIOUS HANDOFF ===".to_string());
        out.push(prev);
        start = 1;
    }

    // First-task marker: request 1's tag never sits in a tool slot (it rides
    // the live opening turn), so the dump prepends it.
    out.push(String::new());
    out.push("--- USER ---".to_string());
    out.push(format!(
        "<updated_user_request no=\"1\">\n{task}\n</updated_user_request no=\"1\">"
    ));

    let mut step_no: u64 = 0;
    for (i, entry) in entries.iter().enumerate().skip(start) {
        step_no += 1;
        out.push(String::new());
        out.push("--- ASSISTANT ---".to_string());
        out.push(format!("<Step_no={step_no} />"));
        out.push(dump_step(entry, i + 1 == entries.len()));
        let Some(tr) = tools.get(i).and_then(|t| t.as_deref()).filter(|t| !t.is_empty()) else {
            continue;
        };
        let results = decode_results(tr);
        let first_is_tool = results
            .first()
            .and_then(|r| r.get("role"))
            .and_then(Value::as_str)
            == Some("tool");
        if first_is_tool {
            // The notes calls' placeholder result says nothing: skip it.
            let body: Vec<String> = results
                .iter()
                .map(|r| py_str_or_empty(r.get("content")))
                .filter(|c| c != REASONING_RESULT)
                .collect();
            if !body.is_empty() {
                out.push(String::new());
                out.push("--- USER ---".to_string());
                out.push(body.join("\n"));
            }
        } else if let Some(m) = find_user_request(tr) {
            step_no = 0; // step numbering restarts after every new task
            let req = format!(
                "<updated_user_request no=\"{n}\">\n{b}\n</updated_user_request no=\"{n}\">",
                n = m.num,
                b = m.body
            );
            // A bridge slot can carry more than the request, e.g. the
            // <manual_mode> block of commands the user typed between runs.
            // Keep it, or the handoff summary loses what the user did.
            let rest = format!(
                "{}{}",
                tr.get(..m.start).unwrap_or(""),
                tr.get(m.end..).unwrap_or("")
            );
            let extra = py_strip(&rest);
            out.push(String::new());
            out.push("--- USER ---".to_string());
            out.push(if extra.is_empty() { req } else { format!("{extra}\n\n{req}") });
        } else {
            out.push(String::new());
            out.push("--- USER ---".to_string());
            out.push(tr.to_string());
        }
    }
    out.join("\n") + "\n"
}

/// Coder flavor of memory_compression's build_dump: steps [0..k] of the
/// native parallel lists in the compressor prompt's <input> format. Called
/// by CompressionController as `dump_builder(assistant_messages,
/// tool_responses, k, task)`.
///
/// MAIN-THREAD ONLY: it snapshots the list content into one string so the
/// worker thread never reads the live lists.
#[pyfunction]
pub fn compression_dump(
    py: Python<'_>,
    assistant_messages: &Bound<'_, PyAny>,
    tool_responses: &Bound<'_, PyAny>,
    k: i64,
    task: &Bound<'_, PyAny>,
) -> PyResult<String> {
    // Python's `[:k + 1]`, a negative k included.
    let stop = k.saturating_add(1);
    let stop = isize::try_from(stop).unwrap_or(if stop < 0 { isize::MIN } else { isize::MAX });
    let head = PySlice::new(py, 0, stop, 1);
    let mut entries: Vec<String> = Vec::new();
    for item in assistant_messages.get_item(&head)?.try_iter()? {
        entries.push(item?.str()?.to_string_lossy().into_owned());
    }
    let mut tools: Vec<Option<String>> = Vec::new();
    for item in tool_responses.get_item(&head)?.try_iter()? {
        let item = item?;
        tools.push(if item.is_truthy()? {
            Some(item.str()?.to_string_lossy().into_owned())
        } else {
            None
        });
    }
    let task = task.str()?.to_string_lossy().into_owned();
    Ok(compression_dump_core(&entries, &tools, &task))
}

/// (entry, tool_slot) replacing entries [0..k] after a handoff lands: ONE
/// content-only native assistant turn whose content is
/// {"memory_compression": <the whole handoff document>} and nothing else,
/// paired with the plain-string note slot. step_k_entry is unused: signature
/// parity with the controller's main-agent default.
#[pyfunction]
#[pyo3(signature = (step_k_entry, handoff_text))]
pub fn compression_entry(
    step_k_entry: &Bound<'_, PyAny>,
    handoff_text: &Bound<'_, PyAny>,
) -> PyResult<(String, String)> {
    let _ = step_k_entry;
    let doc = match handoff_text.cast::<PyString>() {
        Ok(s) => Value::String(s.to_string_lossy().into_owned()),
        Err(_) => pythonize::depythonize(handoff_text)?,
    };
    let mut wrapper = Map::new();
    wrapper.insert(COMPRESSION_KEY.into(), doc);
    let content = dumps_pretty(&Value::Object(wrapper));
    Ok((encode_step(&content, &json!([]), None), COMPRESSION_NOTE.to_string()))
}

// ---------------------------------------------------------------------------
// The display (CLIAgentResponseFormatter's two live methods)
// ---------------------------------------------------------------------------

/// Per-step payload for a piped stdout (the streaming UI, a parent process).
/// The turn is `text` (the model's own prose) plus `action` (the tools it
/// called); `text` is ALSO given as `thinking`, the key the UI already
/// renders, unless the payload has a thinking of its own. Input that is not
/// JSON passes through unchanged. A real terminal uses format_terminal.
pub fn format_stream_json(normalized_response: &str) -> String {
    let Ok(mut data) = serde_json::from_str::<Value>(normalized_response) else {
        return normalized_response.to_string();
    };
    if let Value::Object(map) = &mut data {
        if let Some(text) = map.get("text").filter(|t| truthy(t)).cloned() {
            if !map.get("thinking").is_some_and(truthy) {
                // An existing "thinking" keeps its place; a new one goes last.
                map.insert("thinking".into(), text);
            }
        }
    }
    dumps_pretty(&data)
}

/// Human-readable rendering of one turn for a real terminal: the model's
/// message, then the notes, then the tools it called. Input that is not a
/// JSON object passes through unchanged, and so does a payload with nothing
/// to show.
pub fn format_terminal(normalized_response: &str) -> String {
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(normalized_response) else {
        return normalized_response.to_string();
    };
    let mut parts: Vec<String> = Vec::new();
    let text = data.get("text");
    if let Some(t) = text.filter(|t| truthy(t)) {
        let t = py_str_of(t);
        let t = py_strip(&t);
        if !t.is_empty() {
            parts.push(format!("\u{1f4ac} {t}"));
        }
    }
    // Legacy envelope fields: only present on older or replayed steps.
    if let Some(thinking) = data.get("thinking").filter(|t| truthy(t)) {
        let lowered = py_strip(&py_str_of(thinking)).to_lowercase();
        if lowered != "null" && !lowered.is_empty() && Some(thinking) != text {
            parts.push(format!("\u{1f9e0} thinking:\n{}", py_str_of(thinking)));
        }
    }
    if let Some(memory) = data.get("memory").filter(|v| truthy(v)) {
        parts.push(format!("\u{1f4dd} memory: {}", py_str_of(memory)));
    }
    // The coder's `insight`: its content under a line naming its kind.
    if let Some(insight) = data.get("insight").filter(|v| truthy(v)) {
        let (kind, content) = match insight {
            Value::Object(o) => (
                py_strip(&py_str_or_empty(o.get("kind"))).to_string(),
                py_str_or_empty(o.get("content")),
            ),
            other => (String::new(), py_str_of(other)),
        };
        let head = if kind.is_empty() {
            "\u{1f4a1} insight:".to_string()
        } else {
            format!("\u{1f4a1} insight ({kind}):")
        };
        parts.push(format!("{head}\n{content}"));
    }
    if let Some(next_goal) = data.get("next_goal").filter(|v| truthy(v)) {
        parts.push(format!("\u{1f3af} next_goal: {}", py_str_of(next_goal)));
    }
    if let Some(action) = data.get("action").filter(|v| truthy(v)) {
        parts.push(format!("\u{2699}\u{fe0f}  action:\n{}", dumps_pretty(action)));
    }
    if parts.is_empty() {
        normalized_response.to_string()
    } else {
        parts.join("\n")
    }
}
