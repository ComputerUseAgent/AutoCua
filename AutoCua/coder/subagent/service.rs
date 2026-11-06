//! The subagent's loop: the Rust copy of mac/agent/minions/service.py (the
//! mac coder's minion), the non-Claude path. It runs in the child process the
//! coder starts for each `subagent` call: `python -m AutoCua.coder.subagent`
//! (__main__.py beside this file) calls run_subagent.
//!
//! A read-only scout with the coder's loop shape: every step rebuilds the
//! request from the native transcript (each past step as the assistant turn
//! with its own tool calls, then one result per call) plus a live tail with
//! the workspace and the scratchpad notes. Its own prompt (system_prompt.md
//! beside this file, a byte copy of the mac minion's, read at run time), its
//! own tools (the `reasoning` notes tool, shell, view, grep, glob, scratchpad
//! and exit) and its own notes file (cli_subagent/<session>/milestone.md).
//!
//! It ends on `exit`, on step 101, after three turns in a row without a tool
//! call, or on an error, and it ALWAYS leaves its report in the result file
//! (written whole, then renamed into place), so the coder never waits on a
//! subagent that died. Left out of the mac copy: the Claude path, the stop
//! event (nothing ever set it) and the debug log (a no-op).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyAttributeError, PyException, PyKeyboardInterrupt, PyOSError};
use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use crate::controller::cli::service::render_tool_response;
use crate::controller::service::ControllerView;
use crate::main::view::{
    decode_results, decode_step, encode_results, encode_step, snapshot_turn, wire_calls_from,
    REASONING_RESULT,
};
use crate::sandbox::service::py_os_error;
use crate::subagent::view::{extract_json, format_stream_json};
use crate::tool_registry::service::{
    action_defaults, drop_bundled_terminal, subagent_reasoning_fields, subagent_tool_names,
    tool_calls_to_steps, LLMManager, SendOutcome,
};
use crate::{dumps_pretty, py_err_str, py_str_of, py_strip, truthy};

/// A subagent run is bounded: its job is finding things, not multi-step
/// coding. Step 101 ends it with a partial report.
const MAX_SUBAGENT_ITERATIONS: i64 = 100;

/// The notes tool: its call is answered "ok" and never routed.
const NOTES_TOOL: &str = "reasoning";

/// Providers whose request gets the prompt-cache marker. Matched against the
/// provider string as given, as Python does ("OpenRouter" gets none).
const CACHE_PROVIDERS: [&str; 3] = ["openrouter", "anthropic", "aws"];

/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra):
/// its tool calls run after the turn ends, and the block tells it what that
/// changes about the rules above it.
const ASYNC_TOOLS_ADDENDUM: &str = concat!(
    "\n<async_tools>\n",
    "Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's <Tool_response>. The rules above are unchanged: one `reasoning` call first, then the calls that do the step, batched per <efficiency_guideline>. Because nothing comes back mid-turn:\n",
    "- Never write a text message in a turn that contains tool calls, and never state that a probe succeeded or failed: that verdict belongs to the next step's memory, judged on the <Tool_response>.\n",
    "- `exit` stays the only call of its turn. Never add it after this turn's calls: their results have not arrived, so nothing is verified yet.\n",
    "</async_tools>\n",
);

/// The result of a turn with no tool call at all. No call means no call id
/// to key a tool result to, so it rides a plain user turn (decode_results
/// replays a bare string as exactly that).
fn no_tool_called() -> String {
    format!("<Tool_response>\n{}\n</Tool_response>", dumps_pretty(&json!({"message": "no tool called"})))
}

/// Python's type name of a JSON value, for the error texts Python gives.
fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// sys.stdout.isatty(), asked each time as Python does.
fn is_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// An OSError with Python's text: "[Errno 2] No such file or directory: 'x'".
fn os_err(e: &std::io::Error, path: &Path) -> PyErr {
    PyOSError::new_err(py_os_error(e, path))
}

/// uuid.uuid4().hex[:8], asked of Python's uuid module.
fn new_session_id(py: Python<'_>) -> PyResult<String> {
    let hex: String = py.import("uuid")?.call_method0("uuid4")?.getattr("hex")?.extract()?;
    Ok(hex.chars().take(8).collect())
}

/// str(datetime.now()), asked of Python: "2026-11-06 14:03:12.123456", and
/// without the fraction when the microseconds are 0.
fn py_now(py: Python<'_>) -> PyResult<String> {
    let now = py.import("datetime")?.getattr("datetime")?.call_method0("now")?;
    Ok(now.str()?.to_string_lossy().into_owned())
}

/// The report of a loop that ended without `exit`.
fn ended_summary(findings: &str) -> String {
    format!(
        "Subagent ended without a final report. Findings so far:\n{}",
        if findings.is_empty() { "none" } else { findings }
    )
}

/// The report when the run itself failed (it could not start, or an error
/// or Ctrl+C ended it), with the reason in it.
fn failed_summary(reason: &str, findings: &str) -> String {
    format!(
        "Subagent ended without a final report ({reason}). Findings so far:\n{}",
        if findings.is_empty() { "none" } else { findings }
    )
}

/// The result file {"task", "summary", "status"} (indent 2, UTF-8), written
/// whole beside its final name and then renamed into place, so a reader
/// never sees it half-written. Its folder is made first.
fn write_result(path: &Path, task: &str, summary: &str, status: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = dumps_pretty(&json!({"task": task, "summary": summary, "status": status}));
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if let Err(e) = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The step's notes, harvested from its calls (tool_calls_to_steps' track).
struct Notes {
    thinking: String,
    memory: String,
    next_goal: String,
}

impl Notes {
    fn from_track(track: &[(String, String)]) -> Notes {
        let get = |key: &str| {
            track
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        Notes { thinking: get("thinking"), memory: get("memory"), next_goal: get("next_goal") }
    }
}

/// `str(leaked.get(key) or "").strip() or current`: a note field of a step
/// written as JSON text.
fn leaked_field(leaked: &Map<String, Value>, key: &str, current: &str) -> String {
    let value = match leaked.get(key) {
        Some(v) if truthy(v) => py_str_of(v),
        _ => String::new(),
    };
    let value = py_strip(&value);
    if value.is_empty() { current.to_string() } else { value.to_string() }
}

/// The per-step print: the step as pretty JSON, `thinking` (the step's own
/// prose when it had none), `memory`, `next_goal` and the routed actions.
fn display(notes: &Notes, resp_text: &str, actions: &[Value]) -> String {
    let thinking = if notes.thinking.is_empty() { resp_text } else { notes.thinking.as_str() };
    let payload = dumps_pretty(&json!({
        "thinking": thinking,
        "memory": notes.memory,
        "next_goal": notes.next_goal,
        "action": actions,
    }));
    format_stream_json(&payload)
}

/// One `role: "tool"` result per call the model made, each keyed to that
/// call's id. When a batch produced one combined result (it returned early),
/// the first id carries it and the rest point at it: the pairing must stay
/// complete or providers reject the turn.
fn pair_results(action_result: &Value, calls: &[Value]) -> Vec<Value> {
    let id_of = |c: &Value| c.get("id").cloned().unwrap_or(Value::Null);
    if calls.len() <= 1 {
        let id = calls.first().map(id_of).unwrap_or_else(|| json!("call_0"));
        return vec![json!({"tool_call_id": id, "content": render_tool_response(action_result)})];
    }
    let per = if action_result.get("action").and_then(Value::as_str) == Some("multiple") {
        action_result.get("results").and_then(Value::as_array)
    } else {
        None
    };
    if let Some(per) = per.filter(|p| p.len() == calls.len()) {
        return calls
            .iter()
            .zip(per)
            .map(|(c, r)| json!({"tool_call_id": id_of(c), "content": render_tool_response(r)}))
            .collect();
    }
    let mut out = Vec::with_capacity(calls.len());
    let mut rest = calls.iter();
    if let Some(first) = rest.next() {
        out.push(json!({"tool_call_id": id_of(first), "content": render_tool_response(action_result)}));
    }
    for c in rest {
        out.push(json!({
            "tool_call_id": id_of(c),
            "content": "(result included in the first tool result of this batch)",
        }));
    }
    out
}

/// What one step tells the loop.
enum Flow {
    /// The turn was recorded: on to the next step.
    Next,
    /// An empty turn: nothing recorded, and the step number is used again.
    Retry,
    /// The third turn in a row without a tool call: the loop ends.
    Stop,
    /// `exit` ran: the run is complete with this report.
    Exit(String),
}

/// The loop's running state.
struct LoopState {
    /// (persisted assistant turn, persisted results) per recorded step.
    history: Vec<(String, String)>,
    /// Turns in a row without a tool call (text only or empty).
    json_fail_count: u32,
    /// Turns in a row that called only the notes tool.
    reasoning_only_count: u32,
}

/// One subagent run: the mac minion's AgentService.
struct Subagent {
    /// As given: the prompt-cache check reads it raw.
    provider: String,
    task: String,
    llm: LLMManager,
    controller: ControllerView,
    system_prompt: String,
    /// cli_subagent_conversation/<session> in the cwd, with raw_reasoning/
    /// in it; None unless save_conversation.
    conversation_dir: Option<PathBuf>,
}

impl Subagent {
    /// In Python's order: the session id, the LLM manager (it raises when the
    /// provider cannot be built, for example without an API key), the
    /// controller (it makes the session's folders), the prompt, then the
    /// conversation folders.
    fn new(
        py: Python<'_>,
        task: &str,
        provider: &str,
        model: &str,
        api_key: Option<&str>,
        save_conversation: bool,
    ) -> PyResult<Subagent> {
        let session_id = new_session_id(py)?;
        let llm = LLMManager::build(py, provider, model, api_key, true, "subagent", "quality")?;
        let controller = ControllerView::new(py, provider, model, &session_id, api_key, true)?;
        let prompt_path = crate::coder_dir(py)?.join("subagent").join("system_prompt.md");
        let mut system_prompt = crate::read_text(&prompt_path).map_err(|e| os_err(&e, &prompt_path))?;
        // An `async_tools` model runs its calls after the turn ends; the
        // block tells it what that changes.
        if llm.async_tools() {
            system_prompt.push_str(ASYNC_TOOLS_ADDENDUM);
        }
        let conversation_dir = if save_conversation {
            let dir = PathBuf::from("cli_subagent_conversation").join(&session_id);
            let raw_dir = dir.join("raw_reasoning");
            std::fs::create_dir_all(&raw_dir).map_err(|e| os_err(&e, &raw_dir))?;
            Some(dir)
        } else {
            None
        };
        Ok(Subagent {
            provider: provider.to_string(),
            task: task.to_string(),
            llm,
            controller,
            system_prompt,
            conversation_dir,
        })
    }

    /// The subagent's notes (cli_subagent/<session>/milestone.md), stripped;
    /// "" when there are none.
    fn read_scratchpad(&self) -> String {
        self.controller.scratchpad_service.read_scratchpad()
    }

    /// Workspace and current folder as pure JSON, the coder's shape.
    fn agent_sitting(&self) -> String {
        let sandbox = &self.controller.cli_service.sandbox;
        dumps_pretty(&json!({
            "your_workspace": sandbox.sandbox_root.to_string_lossy(),
            "current_sitting": sandbox.get_cwd(),
        }))
    }

    /// The request, rebuilt from scratch every step: the system prompt, the
    /// opening user turn carrying the request ONCE, every past step as the
    /// assistant turn with its own tool_calls followed by its results, and
    /// the live tail (sitting and scratchpad, never stored in history).
    fn build_messages(&self, history: &[(String, String)]) -> Vec<Value> {
        let agent_sitting = self.agent_sitting();
        let scratchpad_content = self.read_scratchpad();
        let mut pm = vec![format!("<agent_sitting>\n{agent_sitting}\n</agent_sitting>")];
        if !scratchpad_content.is_empty() {
            pm.push(format!("<scratchpad>\n{scratchpad_content}\n</scratchpad>"));
        }
        let user_message = format!("<persistent_memory>\n{}\n</persistent_memory>", pm.join("\n\n"));

        let mut messages = vec![
            json!({"role": "system", "content": self.system_prompt}),
            json!({"role": "user", "content": format!("<user_request>\n{}\n</user_request>", self.task)}),
        ];
        for (raw_entry, raw_results) in history {
            let entry = decode_step(raw_entry);
            let mut asst = Map::new();
            asst.insert("role".into(), json!("assistant"));
            asst.insert("content".into(), Value::String(entry.content));
            if truthy(&entry.tool_calls) {
                asst.insert("tool_calls".into(), entry.tool_calls);
            }
            if truthy(&entry.meta) {
                // The provider's own metadata for this turn (Gemini thought
                // signatures, OpenRouter reasoning blocks).
                asst.insert("provider_meta".into(), entry.meta);
            }
            messages.push(Value::Object(asst));
            messages.extend(decode_results(raw_results));
        }
        messages.push(json!({"role": "user", "content": user_message}));

        // Prompt caching: mark the newest PERSISTENT turn, the last tool
        // result (or the opening user turn on step 1), never the live tail.
        if CACHE_PROVIDERS.contains(&self.provider.as_str()) && messages.len() > 2 {
            let last = messages.len() - 1;
            for (i, msg) in messages.iter_mut().enumerate().rev() {
                let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
                if role != "tool" && role != "user" {
                    continue;
                }
                if role == "user" && i == last {
                    continue;
                }
                if let Some(obj) = msg.as_object_mut() {
                    let text = obj.get("content").and_then(Value::as_str).unwrap_or("").to_string();
                    if !text.is_empty() {
                        obj.insert(
                            "content".into(),
                            json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]),
                        );
                    }
                }
                break;
            }
        }
        messages
    }

    /// The step's raw reply as the model gave it (save_conversation only).
    fn save_raw_response(&self, raw: &str, step_number: i64) {
        let Some(dir) = &self.conversation_dir else { return };
        let path = dir.join("raw_reasoning").join(format!("raw_response_{step_number}.txt"));
        if let Err(e) = std::fs::write(&path, raw) {
            crate::say!("⚠ Error saving raw response: {}", py_os_error(&e, &path));
        }
    }

    /// Exactly what the LLM received at this step, plus the step itself
    /// (save_conversation only). A write error ends the step like Python's.
    fn save_snapshot(
        &self,
        py: Python<'_>,
        messages: &[Value],
        current: &str,
        step_number: i64,
    ) -> PyResult<()> {
        let Some(dir) = &self.conversation_dir else { return Ok(()) };
        let path = dir.join(format!("conversation_{step_number}.txt"));
        let bar = "=".repeat(60);
        let mut out = String::new();
        out.push_str("=== SUBAGENT MEMORY SNAPSHOT ===\n");
        out.push_str(&format!("Step: {step_number}\n"));
        out.push_str(&format!("Time: {}\n", py_now(py)?));
        out.push_str(&format!("{bar}\n\n"));
        // Every message as sent, except the notes call's "ok", which says
        // nothing.
        let mut n = 0;
        for msg in messages {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            let content = msg.get("content").unwrap_or(&Value::Null);
            if role == "tool" && content.as_str() == Some(REASONING_RESULT) {
                continue;
            }
            let text = match content {
                Value::String(s) => s.clone(),
                Value::Array(parts) => match parts.first() {
                    Some(Value::Object(first)) if first.contains_key("text") => {
                        py_str_of(first.get("text").unwrap_or(&Value::Null))
                    }
                    _ => py_str_of(content),
                },
                other => py_str_of(other),
            };
            out.push_str(&format!("=== MESSAGE {n} (role='{role}') ===\n"));
            n += 1;
            if role == "assistant" {
                out.push_str(&snapshot_turn(&text, msg.get("tool_calls").unwrap_or(&Value::Null)));
            } else {
                out.push_str(&text);
            }
            out.push_str(&format!("\n\n{bar}\n\n"));
        }
        let entry = decode_step(current);
        out.push_str("=== CURRENT ASSISTANT RESPONSE (role='assistant') ===\n");
        out.push_str(&snapshot_turn(&entry.content, &entry.tool_calls));
        out.push('\n');
        std::fs::write(&path, out).map_err(|e| os_err(&e, &path))
    }

    /// The loop. Returns the report and its status ("complete" after `exit`,
    /// "partial" otherwise). An error inside a step that is a Python
    /// Exception ends the loop with the partial report, as Python's `except
    /// Exception`; anything else (Ctrl+C) is returned as the error.
    fn run_loop(&mut self, py: Python<'_>) -> PyResult<(String, &'static str)> {
        let mut st = LoopState { history: Vec::new(), json_fail_count: 0, reasoning_only_count: 0 };
        let mut step_number: i64 = 0;
        loop {
            step_number += 1;
            if step_number > MAX_SUBAGENT_ITERATIONS {
                crate::say!("Subagent reached max iterations ({MAX_SUBAGENT_ITERATIONS})");
                let scratchpad_status = self.read_scratchpad();
                let summary = format!(
                    "Max iterations ({MAX_SUBAGENT_ITERATIONS}) reached. Findings so far:\n{scratchpad_status}"
                );
                return Ok((summary, "partial"));
            }
            let messages = self.build_messages(&st.history);
            match self.step(py, &messages, step_number, &mut st) {
                Ok(Flow::Next) => {}
                Ok(Flow::Retry) => step_number -= 1,
                Ok(Flow::Stop) => break,
                Ok(Flow::Exit(summary)) => return Ok((summary, "complete")),
                Err(e) if e.is_instance_of::<PyException>(py) => {
                    crate::say!("Error: {}", py_err_str(py, &e));
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        // The loop ended WITHOUT an exit call (3 strikes or an error). The
        // coder is waiting on this report, so whatever the scratchpad holds
        // goes back as a partial one.
        Ok((ended_summary(&self.read_scratchpad()), "partial"))
    }

    /// One step: ask the model, record its turn, route its calls.
    fn step(
        &mut self,
        py: Python<'_>,
        messages: &[Value],
        step_number: i64,
        st: &mut LoopState,
    ) -> PyResult<Flow> {
        let llm = &self.llm;
        let outcome = py.detach(|| llm.send_rust(messages, &[])).map_err(PyErr::from)?;
        // Native tool calling: the whole message, {"text", "tool_calls",
        // "provider_meta"}. (A plain text reply never comes in this mode;
        // Python reads it as {"content": ...}, so it has no text.)
        let (message, raw) = match outcome {
            SendOutcome::Native(v) => {
                let raw = dumps_pretty(&v);
                (v, raw)
            }
            SendOutcome::Text(s) => (json!({"content": s}), s),
        };
        self.save_raw_response(&raw, step_number);

        // The subagent's name set answers a coder-only call (write, replace,
        // subagent, ...) with an error result instead of ever routing it:
        // the read-only guarantee.
        let tool_calls = message.get("tool_calls").cloned().unwrap_or(Value::Null);
        let (actions, calls, rejects, track) = tool_calls_to_steps(
            &tool_calls,
            subagent_tool_names(),
            action_defaults(),
            subagent_reasoning_fields(),
        );
        let mut notes = Notes::from_track(&track);
        // `(message.get("text") or "").strip()`: text that is not a string
        // (a provider's list of content parts) fails the step, as in Python.
        let mut resp_text = match message.get("text") {
            Some(Value::String(s)) => py_strip(s).to_string(),
            Some(v) if truthy(v) => {
                return Err(PyAttributeError::new_err(format!(
                    "'{}' object has no attribute 'strip'",
                    py_type_name(v)
                )))
            }
            _ => String::new(),
        };
        let step_meta = match message.get("provider_meta") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!({}),
        };

        // `reasoning` is the step's note to itself, not a probe. Its fields
        // are already in `notes`; the call leaves the batch but stays in
        // all_calls, which the assistant turn replays, and is answered "ok".
        let all_calls = calls.clone();
        let mut note_ids: Vec<Value> = Vec::new();
        let mut kept_actions: Vec<Value> = Vec::new();
        let mut kept_calls: Vec<Value> = Vec::new();
        for (a, c) in actions.into_iter().zip(calls) {
            if a.get("type").and_then(Value::as_str) == Some(NOTES_TOOL) {
                note_ids.push(c.get("id").cloned().unwrap_or(Value::Null));
            } else {
                kept_actions.push(a);
                kept_calls.push(c);
            }
        }
        let mut note_results: Vec<Value> = note_ids
            .iter()
            .map(|id| json!({"tool_call_id": id, "content": REASONING_RESULT}))
            .collect();
        let mut actions = kept_actions;
        let mut calls = kept_calls;

        // Backstop for an `async_tools` model: an `exit` bundled after this
        // turn's calls is dropped from the batch and answered with an error
        // result, which rides with the rejects below.
        let mut exit_results: Vec<Value> = Vec::new();
        if self.llm.async_tools() {
            let (a, c, r) = drop_bundled_terminal(actions, calls, &rejects, "exit");
            actions = a;
            calls = c;
            exit_results = r;
            if !exit_results.is_empty() && is_tty() {
                crate::say!("⚠️ `exit` bundled with other calls: ignored, it must be the only call of its turn");
            }
        }

        if actions.is_empty() && rejects.is_empty() && !resp_text.is_empty() {
            // Leaked-step salvage: a model that writes its whole step as JSON
            // text instead of calling tools. A dict with an action list is
            // adopted, filtered to the subagent's tools (no defaults, no
            // coercion).
            let leaked = match serde_json::from_str::<Value>(&resp_text) {
                Ok(v) => Some(v),
                Err(_) => extract_json(&resp_text),
            };
            if let Some(Value::Object(leaked)) = leaked {
                if let Some(Value::Array(items)) = leaked.get("action") {
                    let salvaged: Vec<Value> = items
                        .iter()
                        .filter(|a| {
                            a.get("type")
                                .and_then(Value::as_str)
                                .is_some_and(|t| subagent_tool_names().contains(t))
                        })
                        .cloned()
                        .collect();
                    if !salvaged.is_empty() {
                        actions = salvaged;
                        notes = Notes {
                            thinking: leaked_field(&leaked, "thinking", &notes.thinking),
                            memory: leaked_field(&leaked, "memory", &notes.memory),
                            next_goal: leaked_field(&leaked, "next_goal", &notes.next_goal),
                        };
                        resp_text = String::new();
                        if is_tty() {
                            crate::say!("⚠️ subagent wrote its step as text — salvaged the action");
                        }
                    }
                }
            }
        }

        // The assistant turn exactly as the model produced it, echoed back
        // next step. Rejected calls ride along too: every result written
        // below must pair with a call of the same id.
        let mut replayed = all_calls;
        replayed.extend(rejects.iter().cloned());
        let wire_calls = Value::Array(wire_calls_from(&replayed).map_err(PyException::new_err)?);
        let mut reject_results: Vec<Value> = rejects
            .iter()
            .map(|r| {
                json!({
                    "tool_call_id": r.get("id").cloned().unwrap_or(Value::Null),
                    "content": format!("error: {}", r.get("error").and_then(Value::as_str).unwrap_or("")),
                })
            })
            .collect();
        reject_results.extend(exit_results);
        let entry = encode_step(&resp_text, &wire_calls, Some(&step_meta));

        if actions.is_empty() {
            if let Some(first) = rejects.first() {
                // A disallowed or unknown tool: keep the turn, answer each bad
                // call with an error keyed to ITS id, and let the subagent
                // correct itself on the next step.
                if is_tty() {
                    let name = first.get("name").map(py_str_of).unwrap_or_default();
                    crate::say!("⚠️ subagent called disallowed tool '{name}' — error returned");
                }
                note_results.extend(reject_results);
                st.history.push((entry, encode_results(&note_results)));
                return Ok(Flow::Next);
            }
            if !note_ids.is_empty() {
                // A notes-only step: the subagent chose to think before it
                // probes, a normal step. Only a RUN of them is bounded: from
                // the third in a row, the result carries a reminder to act.
                st.reasoning_only_count += 1;
                if st.reasoning_only_count >= 3 {
                    let content = render_tool_response(&json!({
                        "status": "success",
                        "action": NOTES_TOOL,
                        "message": format!(
                            "{} steps in a row without a probe: call a tool that does the work in this turn.",
                            st.reasoning_only_count
                        ),
                    }));
                    note_results = note_ids
                        .iter()
                        .map(|id| json!({"tool_call_id": id, "content": content}))
                        .collect();
                }
                crate::out(&display(&notes, &resp_text, &[]));
                self.save_snapshot(py, messages, &entry, step_number)?;
                st.history.push((entry, encode_results(&note_results)));
                return Ok(Flow::Next);
            }
            // No call at all: a talk-only turn is kept and answered "no tool
            // called"; an empty one is retried. Both count toward the three
            // strikes.
            st.json_fail_count += 1;
            if is_tty() {
                crate::say!(
                    "⚠️ subagent produced no tool call — asking it to act ({}/3)",
                    st.json_fail_count
                );
            }
            if st.json_fail_count >= 3 {
                return Ok(Flow::Stop);
            }
            if resp_text.is_empty() {
                return Ok(Flow::Retry);
            }
            st.history.push((encode_step(&resp_text, &json!([]), Some(&step_meta)), no_tool_called()));
            return Ok(Flow::Next);
        }

        st.json_fail_count = 0;
        st.reasoning_only_count = 0;

        crate::out(&display(&notes, &resp_text, &actions));
        self.save_snapshot(py, messages, &entry, step_number)?;

        let action_result = self.controller.route_action(py, &actions)?;

        if action_result.get("action").and_then(Value::as_str) == Some("exit") {
            // Python also closes the call/result pairing of this last turn in
            // the history, which is never read again; left out.
            let summary = match action_result.get("summary") {
                None => "Findings ready.".to_string(),
                Some(Value::Null) => String::new(),
                Some(v) => py_str_of(v),
            };
            return Ok(Flow::Exit(summary));
        }

        // ONE canonical result shape, the coder's: a JSON head plus the raw
        // output in its own clipped <output> block.
        let action_results = pair_results(&action_result, &calls);
        note_results.extend(reject_results);
        note_results.extend(action_results);
        st.history.push((entry, encode_results(&note_results)));
        Ok(Flow::Next)
    }
}

/// The subagent child's whole run (coder/subagent/__main__.py calls it): the
/// loop on `task`, then the result file {"task", "summary", "status":
/// "complete" | "partial"} at `result_path`, ALWAYS written (also when the
/// run could not start, failed or was interrupted, with the reason in the
/// summary), then "Result written to: <path>". Returns the summary.
/// KeyboardInterrupt (and any other non-Exception) is raised again once the
/// partial result is written; a result file that cannot be written raises
/// OSError.
#[pyfunction]
#[pyo3(signature = (task, provider, model, result_path, api_key=None, save_conversation=false))]
pub fn run_subagent(
    py: Python<'_>,
    task: String,
    provider: String,
    model: String,
    result_path: String,
    api_key: Option<String>,
    save_conversation: bool,
) -> PyResult<String> {
    let mut findings = String::new();
    let outcome = match Subagent::new(py, &task, &provider, &model, api_key.as_deref(), save_conversation) {
        Ok(mut agent) => {
            let out = agent.run_loop(py);
            if out.is_err() {
                findings = agent.read_scratchpad();
            }
            out
        }
        Err(e) => Err(e),
    };
    let (summary, status, pending) = match outcome {
        Ok((summary, status)) => (summary, status, None),
        Err(e) => {
            let reason = if e.is_instance_of::<PyKeyboardInterrupt>(py) {
                "interrupted".to_string()
            } else {
                let msg = py_err_str(py, &e);
                crate::say!("Error: {msg}");
                format!("error: {msg}")
            };
            // An Exception is reported in the result and the run returns;
            // Ctrl+C (and the like) goes on up once the result is written.
            let pending = if e.is_instance_of::<PyException>(py) { None } else { Some(e) };
            (failed_summary(&reason, &findings), "partial", pending)
        }
    };
    let path = Path::new(&result_path);
    if let Err(e) = write_result(path, &task, &summary, status) {
        let text = py_os_error(&e, path);
        return Err(match pending {
            // Ctrl+C still goes on up; the write error is only told.
            Some(p) => {
                let _ = writeln!(std::io::stderr().lock(), "Could not write the result: {text}");
                p
            }
            None => PyOSError::new_err(text),
        });
    }
    crate::say!("Result written to: {result_path}");
    match pending {
        Some(e) => Err(e),
        None => Ok(summary),
    }
}
