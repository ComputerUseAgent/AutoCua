//! The subagent's loop: the read-only investigator the coder starts as a
//! child process for one `subagent` call, in one of two modes, workflow or
//! critic. It runs in that child: `python -m AutoCua.coder.subagent`
//! (__main__.py beside this file) calls run_subagent.
//!
//! The loop has the coder's shape (main/service.rs): every step rebuilds the
//! request from the native transcript (each past step as the assistant turn
//! with its own tool calls, then one result per call) plus a live tail with
//! the workspace and the scratchpad. The notes tools `insight` and
//! `next_goal` are answered `ok` under their own ids and never routed, and a
//! turn with only notes calls just goes on. Its prompt is system_prompt.md
//! beside this file with the mode's block (workflow/workflow.md or
//! critic/critic.md) in place of `{mode}` and the OS filled in, read at run
//! time; its tools are the coder's two notes tools plus shell, read, grep,
//! glob, scratchpad and exit; its notes file is
//! cli_subagent/<session>/milestone.md.
//!
//! It ends on `exit`, on step 101, after three turns in a row without a tool
//! call, or on an error, and it ALWAYS leaves its report in the result file
//! (written whole, then renamed into place), so the coder never waits on a
//! subagent that died. A run that cannot start on its mode (an unknown mode,
//! a mode file that does not read) leaves a partial report too, and its
//! error then goes on up, so the child exits 1.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyAttributeError, PyException, PyKeyboardInterrupt, PyOSError, PyValueError};
use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use crate::coder::controller::cli::service::render_tool_response;
use crate::coder::controller::service::ControllerView;
use crate::coder::main::service::{current_os, display_payload, print_turn, turn_notes};
use crate::coder::main::view::{
    coder_snapshot_turn, decode_results, decode_step, encode_results, encode_step, wire_calls_from,
    REASONING_RESULT,
};
use crate::coder::sandbox::service::py_os_error;
use crate::coder::tool_registry::service::{
    action_defaults, drop_bundled_terminal, subagent_tool_names, tool_calls_to_steps, LLMManager,
    SendOutcome, NOTE_TOOLS,
};
use crate::coder::{dumps_pretty, py_err_str, py_str_of, py_strip, truthy};

/// A subagent run is bounded: its job is finding things, not multi-step
/// coding. Step 101 ends it with a partial report.
const MAX_SUBAGENT_ITERATIONS: i64 = 100;

/// The modes and the block file of each, relative to this folder, with `/`
/// between the parts on every OS.
const MODE_FILES: [(&str, &str); 2] = [("workflow", "workflow/workflow.md"), ("critic", "critic/critic.md")];

/// The line of system_prompt.md the mode's block replaces.
const MODE_PLACEHOLDER: &str = "{mode}";

/// The word in system_prompt.md that current_os() replaces.
const OS_PLACEHOLDER: &str = "{current_os}";

/// Providers whose request gets the prompt-cache marker. Matched against the
/// provider string as given, as Python does ("OpenRouter" gets none).
const CACHE_PROVIDERS: [&str; 3] = ["openrouter", "anthropic", "aws"];

/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra):
/// its tool calls run after the turn ends, and the block tells it what that
/// changes about the rules above it. The coder's text (main/service.rs).
const ASYNC_TOOLS_ADDENDUM: &str = "\n<async_tools>\n\
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's <Tool_response>. Because nothing comes back mid-turn:\n\
- Never write a text message in a turn that contains tool calls, and never state that a command succeeded or failed: the next step judges that on the <Tool_response>.\n\
- `exit` stays the only call of its turn. Never add it after this turn's calls: their results have not arrived, so nothing is verified yet.\n\
</async_tools>\n";

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

/// A call's "id" (null when it has none).
fn call_id(call: &Value) -> Value {
    call.get("id").cloned().unwrap_or(Value::Null)
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

/// The subagent's prompt for `mode`: subagent/system_prompt.md with the
/// mode's block (MODE_FILES) in place of `{mode}` and the OS in place of
/// `{current_os}`, both files read fresh from this folder. A mode that is
/// not workflow or critic is a ValueError; a file that does not read is its
/// OSError. Either ends the run before it starts (see run_subagent).
fn load_subagent_prompt(py: Python<'_>, mode: &str) -> PyResult<String> {
    let Some((_, block_file)) = MODE_FILES.iter().find(|(name, _)| *name == mode) else {
        return Err(PyValueError::new_err(format!(
            "subagent mode must be \"workflow\" or \"critic\", not '{mode}'"
        )));
    };
    let dir = crate::coder::coder_dir(py)?.join("subagent");
    let base_path = dir.join("system_prompt.md");
    let base = crate::coder::read_text(&base_path).map_err(|e| os_err(&e, &base_path))?;
    // Part by part, so the path is written the way this OS writes it.
    let mut block_path = dir;
    for part in block_file.split('/') {
        block_path.push(part);
    }
    let block = crate::coder::read_text(&block_path).map_err(|e| os_err(&e, &block_path))?;
    Ok(base.replace(MODE_PLACEHOLDER, &block).replace(OS_PLACEHOLDER, current_os()))
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

/// One `role: "tool"` result per call the model made, each keyed to that
/// call's id. When a batch produced one combined result (it returned early),
/// the first id carries it and the rest point at it: the pairing must stay
/// complete or providers reject the turn.
fn pair_results(action_result: &Value, calls: &[Value]) -> Vec<Value> {
    if calls.len() <= 1 {
        let id = calls.first().map(call_id).unwrap_or_else(|| json!("call_0"));
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
            .map(|(c, r)| json!({"tool_call_id": call_id(c), "content": render_tool_response(r)}))
            .collect();
    }
    let mut out = Vec::with_capacity(calls.len());
    let mut rest = calls.iter();
    if let Some(first) = rest.next() {
        out.push(json!({"tool_call_id": call_id(first), "content": render_tool_response(action_result)}));
    }
    for c in rest {
        out.push(json!({
            "tool_call_id": call_id(c),
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
}

/// One subagent run.
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
    /// controller (it makes the session's folders), then the conversation
    /// folders. `system_prompt` is the mode's prompt (load_subagent_prompt).
    fn new(
        py: Python<'_>,
        task: &str,
        provider: &str,
        model: &str,
        api_key: Option<&str>,
        save_conversation: bool,
        mut system_prompt: String,
    ) -> PyResult<Subagent> {
        let session_id = new_session_id(py)?;
        let llm = LLMManager::build(py, provider, model, api_key, true, "subagent", "quality")?;
        let controller = ControllerView::new(py, provider, model, &session_id, api_key, true, false, false)?;
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
    /// (save_conversation only), each assistant turn rendered as the coder
    /// renders its own (coder_snapshot_turn: only the keys the turn has).
    /// A write error ends the step like Python's.
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
        // Every message as sent, except a notes call's "ok", which says
        // nothing.
        let mut n = 0;
        for msg in messages {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
            let content = msg.get("content").unwrap_or(&Value::Null);
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
            // Also when it is the cache-marked newest result.
            if role == "tool" && text == REASONING_RESULT {
                continue;
            }
            out.push_str(&format!("=== MESSAGE {n} (role='{role}') ===\n"));
            n += 1;
            if role == "assistant" {
                out.push_str(&coder_snapshot_turn(&text, msg.get("tool_calls").unwrap_or(&Value::Null)));
            } else {
                out.push_str(&text);
            }
            out.push_str(&format!("\n\n{bar}\n\n"));
        }
        let entry = decode_step(current);
        out.push_str("=== CURRENT ASSISTANT RESPONSE (role='assistant') ===\n");
        out.push_str(&coder_snapshot_turn(&entry.content, &entry.tool_calls));
        out.push('\n');
        std::fs::write(&path, out).map_err(|e| os_err(&e, &path))
    }

    /// The loop. Returns the report and its status ("complete" after `exit`,
    /// "partial" otherwise). An error inside a step that is a Python
    /// Exception ends the loop with the partial report, as Python's `except
    /// Exception`; anything else (Ctrl+C) is returned as the error.
    fn run_loop(&mut self, py: Python<'_>) -> PyResult<(String, &'static str)> {
        let mut st = LoopState { history: Vec::new(), json_fail_count: 0 };
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

    /// One step, the coder's way: ask the model, record its turn, answer its
    /// notes calls, route its probes and pair the results back to the calls.
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
        // the read-only guarantee. No track params: the notes are read off
        // the notes calls below.
        let tool_calls = message.get("tool_calls").cloned().unwrap_or(Value::Null);
        let (actions, calls, rejects, _) =
            tool_calls_to_steps(&tool_calls, subagent_tool_names(), action_defaults(), &[]);
        // `(message.get("text") or "").strip()`: text that is not a string
        // (a provider's list of content parts) fails the step, as in Python.
        let resp_text = match message.get("text") {
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

        // The notes calls (`insight`, `next_goal`) are the step's notes to
        // itself, not probes: they are replayed with the turn (all_calls)
        // and answered `ok`, never routed.
        let is_note = |a: &Value| a.get("type").and_then(Value::as_str).is_some_and(|t| NOTE_TOOLS.contains(&t));
        let all_calls = calls.clone();
        let (insight, next_goal) = turn_notes(&actions, &rejects);
        let note_results: Vec<Value> = actions
            .iter()
            .zip(&calls)
            .filter(|(a, _)| is_note(a))
            .map(|(_, c)| json!({"tool_call_id": call_id(c), "content": REASONING_RESULT}))
            .collect();
        let (actions, calls): (Vec<Value>, Vec<Value>) =
            actions.into_iter().zip(calls).filter(|(a, _)| !is_note(a)).unzip();

        // Backstop for an `async_tools` model: an `exit` bundled after this
        // turn's calls is dropped from the batch and answered with an error
        // result, which rides with the rejects below.
        let mut exit_results: Vec<Value> = Vec::new();
        let (actions, calls) = if self.llm.async_tools() {
            let (actions, calls, dropped) = drop_bundled_terminal(actions, calls, &rejects, "exit");
            if !dropped.is_empty() && is_tty() {
                crate::say!("⚠️ `exit` bundled with other calls: ignored, it must be the only call of its turn");
            }
            exit_results = dropped;
            (actions, calls)
        } else {
            (actions, calls)
        };

        // No tool call at all: a text-only or an empty turn, taken as the
        // coder takes them. Both count toward the three strikes.
        if actions.is_empty() && rejects.is_empty() && note_results.is_empty() {
            st.json_fail_count += 1;
            if !resp_text.is_empty() {
                // Repair, not exit: prose with no tool calls is a dropped-calls
                // mistake. Keep the turn, answer it, let the model go on.
                if is_tty() {
                    crate::say!(
                        "⚠️ message had no tool calls \u{2014} asking the model to act ({}/3)",
                        st.json_fail_count
                    );
                }
                if st.json_fail_count >= 3 {
                    return Ok(Flow::Stop);
                }
                let entry = encode_step(&resp_text, &json!([]), Some(&step_meta));
                print_turn(&display_payload(&resp_text, &Value::Null, "", json!([])));
                self.save_snapshot(py, messages, &entry, step_number)?;
                // A plain string slot: it replays as a user turn.
                st.history.push((entry, no_tool_called()));
                return Ok(Flow::Next);
            }
            // A truly empty turn: retry it under the same step number.
            if is_tty() {
                crate::say!("⚠️ empty response from the model \u{2014} retrying ({}/3)", st.json_fail_count);
            }
            if st.json_fail_count >= 3 {
                return Ok(Flow::Stop);
            }
            return Ok(Flow::Retry);
        }

        st.json_fail_count = 0;

        // The turn exactly as the model made it, rejected calls included:
        // every result written below must pair with a call of the same id.
        let mut replayed = all_calls;
        replayed.extend(rejects.iter().cloned());
        let wire_calls = Value::Array(wire_calls_from(&replayed).map_err(PyException::new_err)?);
        let entry = encode_step(&resp_text, &wire_calls, Some(&step_meta));

        print_turn(&display_payload(&resp_text, &insight, &next_goal, Value::Array(actions.clone())));
        self.save_snapshot(py, messages, &entry, step_number)?;

        // The notes' ok first, then each rejected call's error keyed to ITS
        // id, then the routed results.
        let mut results = note_results;
        results.extend(rejects.iter().map(|r| {
            json!({
                "tool_call_id": call_id(r),
                "content": format!("error: {}", py_str_of(r.get("error").unwrap_or(&Value::Null))),
            })
        }));
        results.extend(exit_results);
        if is_tty() {
            for r in &rejects {
                crate::say!(
                    "⚠️ {} \u{2014} error returned to the model",
                    py_str_of(r.get("error").unwrap_or(&Value::Null))
                );
            }
        }
        if actions.is_empty() {
            // Nothing to route (only notes calls or rejected calls): persist
            // their results and the errors, and go on to the next step.
            st.history.push((entry, encode_results(&results)));
            return Ok(Flow::Next);
        }

        let action_result = self.controller.route_action(py, &actions)?;

        if action_result.get("action").and_then(Value::as_str) == Some("exit") {
            // The run is over: this last turn is never read again, so its
            // call/result pairing is not closed.
            let summary = match action_result.get("summary") {
                None => "Findings ready.".to_string(),
                Some(Value::Null) => String::new(),
                Some(v) => py_str_of(v),
            };
            return Ok(Flow::Exit(summary));
        }

        // ONE canonical result shape, the coder's: a JSON head plus the raw
        // output in its own clipped <output> block.
        results.extend(pair_results(&action_result, &calls));
        st.history.push((entry, encode_results(&results)));
        Ok(Flow::Next)
    }
}

/// The subagent child's whole run (coder/subagent/__main__.py calls it): the
/// loop on `task` in `mode` ("workflow" or "critic"), then the result file
/// {"task", "summary", "status": "complete" | "partial"} at `result_path`,
/// ALWAYS written (also when the run could not start, failed or was
/// interrupted, with the reason in the summary), then "Result written to:
/// <path>". Returns the summary. KeyboardInterrupt (and any other
/// non-Exception), and the error of a run that could not start on its mode,
/// are raised again once the partial result is written; a result file that
/// cannot be written raises OSError.
#[pyfunction]
#[pyo3(signature = (task, provider, model, result_path, mode, api_key=None, save_conversation=false))]
#[allow(clippy::too_many_arguments)]
pub fn run_subagent(
    py: Python<'_>,
    task: String,
    provider: String,
    model: String,
    result_path: String,
    mode: String,
    api_key: Option<String>,
    save_conversation: bool,
) -> PyResult<String> {
    let mut findings = String::new();
    // An error that goes on up once the result is written, whatever its
    // type: the run could not start on its mode, so the child must exit 1.
    let mut flies = false;
    let outcome = match load_subagent_prompt(py, &mode) {
        Ok(prompt) => {
            match Subagent::new(py, &task, &provider, &model, api_key.as_deref(), save_conversation, prompt) {
                Ok(mut agent) => {
                    let out = agent.run_loop(py);
                    if out.is_err() {
                        findings = agent.read_scratchpad();
                    }
                    out
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => {
            flies = true;
            Err(e)
        }
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
            let pending = if flies || !e.is_instance_of::<PyException>(py) { Some(e) } else { None };
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
