//! The coder's main agent: its loop and its prompts.
//!
//! The Rust copy of mac/agent/coder/service.py: AgentService and its agent
//! loop, on the non-Claude path (one prompt and one tool list for every
//! model). Shaped like ../../android/agent/android_use/service.rs: the loop
//! is Rust, and it drives two Python pieces over the GIL. One is the shared
//! memory_compression.CompressionController: the two transcript lists are
//! real Python lists, because its apply_pending splices them in place. The
//! other is the LLM provider behind the tool registry's LLMManager, which is
//! called with the GIL released.
//!
//! Each step rebuilds the whole request from the two lists: the system
//! prompt, the `<user_request=1>` turn, every past step as an assistant turn
//! with its tool calls plus one tool result per call, and a live
//! `<persistent_memory>` turn at the end (workspace and cwd, plan, todo,
//! scratchpad, the skill of each mode that is on, a web result shown once).
//! Nothing in the lists is rewritten, so the prompt-cache prefix stays
//! valid.
//!
//! Left out, as a terminal run never uses them: the app's history file, the
//! result file and on_complete, the token-usage markers, the request number
//! and the subagents' own terminal windows. AgentService still takes those
//! keyword arguments and ignores them.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use pyo3::exceptions::{PyAttributeError, PyException, PyKeyError, PyOSError, PyRuntimeError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::{json, Map, Value};

use super::view;
use crate::coder::controller::cli::service::render_tool_response;
use crate::coder::controller::service::ControllerView;
use crate::coder::tool_registry::service::{self as registry, LLMManager, Mode, SendOutcome, MODES};
use crate::coder::{dumps_pretty, py_str_of, py_strip, truthy};

/// The word in system_prompt.md that current_os() replaces.
const OS_PLACEHOLDER: &str = "{current_os}";

/// The OS this coder runs on, in the prompt's own words: mac, windows or linux.
pub fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        os => os,
    }
}

/// system_prompt.md from `main_dir` (this folder, AutoCua/coder/main) with the
/// OS filled in. One prompt serves every OS, so the OS is filled in here; a
/// prompt without the placeholder comes back exactly as written.
pub fn load_system_prompt(main_dir: &Path) -> std::io::Result<String> {
    let prompt = std::fs::read_to_string(main_dir.join("system_prompt.md"))?;
    Ok(prompt.replace(OS_PLACEHOLDER, current_os()))
}

/// Steps before the coder stops on its own, so a looping model ends.
const MAX_CLI_ITERATIONS: i64 = 100;

/// The coder's notes tools: an `insight` when something new came up and the
/// step's `next_goal`. Each call is answered `ok` and never routed.
const NOTES_TOOLS: [&str; 2] = ["insight", "next_goal"];

/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra):
/// its tool calls run after the turn ends, and the block tells it what that
/// changes about the rules above it.
const ASYNC_TOOLS_ADDENDUM: &str = "\n<async_tools>\n\
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's <Tool_response>. Because nothing comes back mid-turn:\n\
- Never write a text message in a turn that contains tool calls, and never state that a command succeeded or failed: the next step judges that on the <Tool_response>.\n\
- `exit` stays the only call of its turn. Never add it after this turn's calls: their results have not arrived, so nothing is verified yet.\n\
</async_tools>\n";

/// Put before a web result in the live turn: the raw result is shown once,
/// and the scratchpad entries the model records this step become the web
/// call's durable `result`.
const WEB_DIGEST_CRITICAL: &str = "\n\n<critical>\nA web result arrived below. Digest it NOW: extract every \
fact you will need later (numbers, dates, names, paths, URLs) and record \
each as its own `scratchpad` call in this step. Those entries become the \
web call's durable `result` in <agent_history>; the raw text below is \
shown once and never stored, so anything you do not record is gone.\n\
</critical>\n\n";

/// A successful web call's `result` in memory until the digest step's
/// scratchpad entries replace it.
const WEB_POINTER: &str =
    "delivered in the next step's <tool> block; the scratchpad entries recorded there replace this line";

/// The result of every call but the first when one combined result came back.
const IN_FIRST_RESULT: &str = "(result included in the first tool result of this batch)";

/// What one guarded step tells the loop to do next.
enum Flow {
    Continue,
    Break,
    /// `exit` ran: the run is over.
    Exit,
}

/// The loop's counters and web bookkeeping, under the mac loop's names.
struct LoopState {
    step_number: i64,
    is_first: bool,
    /// Text-only and empty turns since the last turn with calls (3 ends the run).
    json_fail_count: i64,
    /// Which modes are on, one flag per MODES entry, set by the modes'
    /// switch calls; all off at the start of every run.
    modes_on: Vec<bool>,
    /// The tool slot of the web step the digest step backfills.
    web_memory_index: Option<i64>,
    /// (call id, compacted result) of that web step, for the backfill.
    web_pending: Option<Vec<(Value, Value)>>,
}

/// What the instance keeps between steps, as the Python attributes do.
struct RunState {
    assistant_messages: Py<PyList>,
    tool_responses: Py<PyList>,
    /// The <tool> block waiting for the next step.
    pending_web_response: String,
}

/// The coder agent service: the agent loop over the coder's tools.
#[pyclass(frozen)]
pub struct AgentService {
    /// As given (not lowercased): the banner and the cache-marker check read it.
    provider: String,
    model: String,
    save_conversation: bool,
    /// A threading.Event-like object: checked at the top of each step and
    /// right after each LLM call. Python hands it to the compression only.
    stop_event: Option<Py<PyAny>>,
    session_id: String,
    llm: Py<LLMManager>,
    controller: Mutex<ControllerView>,
    compression: Py<PyAny>,
    system_prompt: String,
    state: Mutex<RunState>,
}

impl AgentService {
    fn state(&self) -> MutexGuard<'_, RunState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// ./cli_conversation/<session>, beside the process cwd.
    fn conversation_dir(&self) -> PathBuf {
        PathBuf::from("cli_conversation").join(&self.session_id)
    }

    fn stop_set(&self, py: Python<'_>) -> PyResult<bool> {
        match &self.stop_event {
            Some(event) => {
                let event = event.bind(py);
                Ok(event.is_truthy()? && event.call_method0("is_set")?.is_truthy()?)
            }
            None => Ok(false),
        }
    }

    /// The one-time "agent started" banner, on a terminal only.
    fn print_start_banner(&self) {
        if !stdout_tty() {
            return;
        }
        let bar = "\u{2500}".repeat(44);
        crate::coder::out(&format!("\n{bar}"));
        crate::coder::out("\u{1f916} AutoCua  \u{b7}  agent started");
        crate::coder::out(&format!("   provider : {}", self.provider));
        crate::coder::out(&format!("   model    : {}", self.model));
        crate::coder::out(&format!("{bar}\n"));
    }

    /// The step's true memory: every message exactly as sent, except that an
    /// assistant turn reads as one {insight, next_goal, action} JSON
    /// (view::coder_snapshot_value) and the notes calls' `ok` is left out,
    /// then this step's reply. A write error ends the run, as in Python.
    fn save_conversation_snapshot(
        &self,
        messages: &[Value],
        current_assistant_response: &str,
        interaction_count: i64,
    ) -> PyResult<()> {
        if !self.save_conversation {
            return Ok(());
        }
        let file = self.conversation_dir().join(format!("conversation_{interaction_count}.txt"));
        let rule = "=".repeat(60);
        let mut out = String::new();
        out.push_str("=== CLI AGENT MEMORY SNAPSHOT ===\n");
        out.push_str(&format!("Step: {interaction_count}\n"));
        out.push_str(&format!("Time: {}\n", py_now()));
        out.push_str(&rule);
        out.push_str("\n\n");
        let mut n = 0;
        for msg in messages {
            let role = msg.get("role").map(py_str_of).unwrap_or_default();
            let content = msg.get("content").unwrap_or(&Value::Null);
            // A cache-marked turn carries its text in a parts list.
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
            // A notes call's `ok`, also when it is the cache-marked newest result.
            if role == "tool" && text == view::REASONING_RESULT {
                continue;
            }
            out.push_str(&format!("=== MESSAGE {n} (role='{role}') ===\n"));
            n += 1;
            if role == "assistant" {
                out.push_str(&view::coder_snapshot_turn(&text, msg.get("tool_calls").unwrap_or(&Value::Null)));
            } else {
                out.push_str(&text);
            }
            out.push_str("\n\n");
            out.push_str(&rule);
            out.push_str("\n\n");
        }
        let entry = view::decode_step(current_assistant_response);
        out.push_str("=== CURRENT ASSISTANT RESPONSE (role='assistant') ===\n");
        out.push_str(&view::coder_snapshot_turn(&entry.content, &entry.tool_calls));
        out.push('\n');
        std::fs::write(&file, out).map_err(|e| os_error(&e, &file))
    }

    /// The LLM's reply before any parsing, for debugging.
    fn save_raw_response(&self, raw_response: &str, step_number: i64) {
        if !self.save_conversation {
            return;
        }
        let raw_file = self
            .conversation_dir()
            .join("raw_reasoning")
            .join(format!("raw_response_{step_number}.txt"));
        if let Err(e) = std::fs::write(&raw_file, raw_response) {
            crate::say!(
                "\u{26a0} Error saving raw response: {}",
                crate::coder::sandbox::service::py_os_error(&e, &raw_file)
            );
        }
    }

    /// The whole request, rebuilt from the two lists: system prompt, the
    /// opening request, each past step and its results, then the live turn.
    fn build_messages(
        &self,
        task: &str,
        am: &Bound<'_, PyList>,
        tr: &Bound<'_, PyList>,
        live: String,
    ) -> PyResult<Vec<Value>> {
        let mut messages = vec![
            json!({"role": "system", "content": self.system_prompt}),
            json!({"role": "user", "content": format!("<user_request=1>\n{task}\n</user_request=1>")}),
        ];
        for i in 0..am.len() {
            let raw_entry = am.get_item(i)?.str()?.to_string_lossy().into_owned();
            let entry = view::decode_step(&raw_entry);
            let mut asst = Map::new();
            asst.insert("role".into(), json!("assistant"));
            asst.insert("content".into(), Value::String(entry.content));
            if truthy(&entry.tool_calls) {
                asst.insert("tool_calls".into(), entry.tool_calls);
            }
            if truthy(&entry.meta) {
                // The provider's own metadata for this turn (Gemini thought
                // signatures, OpenRouter reasoning blocks); LLMManager strips
                // it for every other provider.
                asst.insert("provider_meta".into(), entry.meta);
            }
            messages.push(Value::Object(asst));
            let slot = if i < tr.len() { slot_text(&tr.get_item(i)?)? } else { String::new() };
            messages.extend(view::decode_results(&slot));
        }
        messages.push(json!({"role": "user", "content": live}));
        mark_cache(&self.provider, &mut messages);
        Ok(messages)
    }

    /// The mac loop: one iteration per step until `exit`, the step cap, a
    /// third turn without calls, or an error inside a step.
    fn run_agent_loop(&self, py: Python<'_>, ctl: &mut ControllerView, task: &str) -> PyResult<String> {
        self.print_start_banner();

        let am = PyList::empty(py);
        let tr = PyList::empty(py);
        {
            let mut state = self.state();
            state.assistant_messages = am.clone().unbind();
            state.tool_responses = tr.clone().unbind();
        }
        let compression = self.compression.bind(py);
        compression.call_method0("reset")?;

        let mut lp = LoopState {
            step_number: 0,
            is_first: true,
            json_fail_count: 0,
            modes_on: vec![false; MODES.len()],
            web_memory_index: None,
            web_pending: None,
        };

        loop {
            // Outside the step's guard: an error here ends process_request.
            py.check_signals()?;
            if self.stop_set(py)? {
                crate::coder::out("Agent stopped.");
                break;
            }

            // Splice a finished background compression in place (loop thread only).
            let shifted = compression.call_method1("apply_pending", (&am, &tr, lp.web_memory_index))?;
            lp.web_memory_index = if shifted.is_none() { None } else { Some(shifted.extract::<i64>()?) };

            lp.step_number += 1;
            let mut is_web_digest = false;

            if lp.step_number > MAX_CLI_ITERATIONS {
                crate::say!("CLI Agent reached max iterations ({MAX_CLI_ITERATIONS})");
                return Ok("Max iterations reached".to_string());
            }

            // Fresh from disk every step.
            let agent_sitting = get_agent_sitting(ctl);
            let todo_list = read_todo_from_file(ctl);
            let plan_doc = read_plan_from_file(ctl);
            let scratchpad_notes = ctl.scratchpad_service.read_scratchpad();

            // A pending web result rides into this step once, then it is gone.
            let mut pending_web = String::new();
            if !lp.is_first {
                let mut state = self.state();
                if !state.pending_web_response.is_empty() {
                    pending_web = std::mem::take(&mut state.pending_web_response);
                    is_web_digest = true;
                }
            }

            let mut pm = vec![format!("<agent_sitting>\n{agent_sitting}\n</agent_sitting>")];
            if !plan_doc.is_empty() {
                pm.push(plan_doc);
            }
            if !todo_list.is_empty() {
                pm.push(format!("<todo_list>\n{todo_list}\n</todo_list>"));
            }
            if !scratchpad_notes.is_empty() {
                pm.push(format!("<scratchpad>\n{scratchpad_notes}\n</scratchpad>"));
            }
            // Last, the block of each mode that is on, in table order.
            for mode in active_modes(&lp.modes_on) {
                pm.push(mode_block(py, mode)?);
            }
            let mut live = format!("<persistent_memory>\n{}\n</persistent_memory>", pm.join("\n\n"));
            if !pending_web.is_empty() {
                live.push_str(WEB_DIGEST_CRITICAL);
                live.push_str(&pending_web);
            }
            let messages = self.build_messages(task, &am, &tr, live)?;

            match self.run_step(py, ctl, &am, &tr, task, &messages, is_web_digest, &mut lp) {
                Ok(Flow::Continue) => {}
                Ok(Flow::Break) => break,
                Ok(Flow::Exit) => return Ok("Exit".to_string()),
                Err(e) => {
                    // Python's `except Exception`: KeyboardInterrupt and
                    // other BaseExceptions go on up.
                    if !e.is_instance_of::<PyException>(py) {
                        return Err(e);
                    }
                    crate::say!("Error: {}", crate::coder::py_err_str(py, &e));
                    break;
                }
            }
        }
        Ok("Agent loop ended".to_string())
    }

    /// One step inside Python's `try`: the LLM call, the turn recorded, the
    /// actions routed and their results paired back to the calls.
    #[allow(clippy::too_many_arguments)]
    fn run_step(
        &self,
        py: Python<'_>,
        ctl: &mut ControllerView,
        am: &Bound<'_, PyList>,
        tr: &Bound<'_, PyList>,
        task: &str,
        messages: &[Value],
        is_web_digest: bool,
        lp: &mut LoopState,
    ) -> PyResult<Flow> {
        let llm = self.llm.get();
        // The model can only call the tools this request gives it, so the
        // modes work on the request alone: the provider reads its tool list
        // every request.
        let hidden = hidden_tools(&lp.modes_on);
        llm.hide_tools(py, &hidden)?;

        let outcome = if stdout_tty() {
            let spinner = Spinner::start();
            let outcome = py.detach(|| llm.send_rust(messages, &[]));
            if let Some(spinner) = spinner {
                spinner.stop();
            }
            outcome
        } else {
            py.detach(|| llm.send_rust(messages, &[]))
        };
        let raw_response = match outcome.map_err(PyErr::from)? {
            SendOutcome::Native(v) => v,
            SendOutcome::Text(s) => Value::String(s),
        };

        if self.stop_set(py)? {
            return Ok(Flow::Break);
        }

        // Context size is fresh in last_usage, and the lists hold only the
        // finished steps here (this one is appended below).
        self.compression.bind(py).call_method1("maybe_trigger", (am, tr, task))?;

        self.save_raw_response(&dumps_pretty(&raw_response), lp.step_number);

        // No track params: the notes are read off the notes calls below. A
        // call to a tool a mode took away is rejected like an unknown tool.
        let allowed: HashSet<String> =
            registry::coder_tool_names().iter().filter(|name| !hidden.contains(&name.as_str())).cloned().collect();
        let (actions, calls, rejects, _) = registry::tool_calls_to_steps(
            raw_response.get("tool_calls").unwrap_or(&Value::Null),
            &allowed,
            registry::action_defaults(),
            &[],
        );
        let resp_text = response_text(&raw_response)?;
        let provider_meta = raw_response.get("provider_meta");

        // The notes calls (`insight`, `next_goal`) are the step's notes to
        // itself, not actions: they are replayed with the turn (all_calls)
        // and answered `ok`, never routed.
        let is_note = |a: &Value| a.get("type").and_then(Value::as_str).is_some_and(|t| NOTES_TOOLS.contains(&t));
        let all_calls = calls.clone();
        let (insight, next_goal) = turn_notes(&actions, &rejects);
        let mut note_results: Vec<Value> = actions
            .iter()
            .zip(&calls)
            .filter(|(a, _)| is_note(a))
            .map(|(_, c)| json!({"tool_call_id": call_id(c), "content": view::REASONING_RESULT}))
            .collect();
        let (actions, calls): (Vec<Value>, Vec<Value>) =
            actions.into_iter().zip(calls).filter(|(a, _)| !is_note(a)).unzip();

        // Async model backstop: an `exit` bundled with other calls is dropped
        // and answered with an error result that rides with the rejects.
        let mut exit_results: Vec<Value> = Vec::new();
        let (actions, calls) = if llm.async_tools() {
            let (actions, calls, dropped) = registry::drop_bundled_terminal(actions, calls, &rejects, "exit");
            if !dropped.is_empty() && stdout_tty() {
                crate::coder::out("\u{26a0}\u{fe0f} `exit` bundled with other calls: ignored, it must be the only call of its turn");
            }
            exit_results = dropped;
            (actions, calls)
        } else {
            (actions, calls)
        };

        // No tool call at all: a text-only or an empty turn.
        if actions.is_empty() && rejects.is_empty() && note_results.is_empty() {
            if !resp_text.is_empty() {
                // Repair, not exit: prose with no tool calls is a dropped-calls
                // mistake. Keep the turn, answer it, let the model go on.
                lp.json_fail_count += 1;
                if stdout_tty() {
                    crate::say!(
                        "\u{26a0}\u{fe0f} message had no tool calls \u{2014} asking the model to act ({}/3)",
                        lp.json_fail_count
                    );
                }
                if lp.json_fail_count >= 3 {
                    return Ok(Flow::Break);
                }
                let normalized = view::encode_step(&resp_text, &json!([]), provider_meta);
                print_turn(&display_payload(&resp_text, &Value::Null, "", json!([])));
                self.save_conversation_snapshot(messages, &normalized, lp.step_number)?;
                am.append(&normalized)?;
                // A plain string slot: it replays as a user turn.
                tr.append(format!(
                    "<Tool_response>\n{}\n</Tool_response>",
                    dumps_pretty(&json!({"message": "no tool called"}))
                ))?;
                lp.is_first = false;
                return Ok(Flow::Continue);
            }
            // A truly empty turn: retry it under the same step number.
            lp.json_fail_count += 1;
            if stdout_tty() {
                crate::say!(
                    "\u{26a0}\u{fe0f} empty response from the model \u{2014} retrying ({}/3)",
                    lp.json_fail_count
                );
            }
            if lp.json_fail_count >= 3 {
                return Ok(Flow::Break);
            }
            lp.step_number -= 1;
            return Ok(Flow::Continue);
        }

        lp.json_fail_count = 0;

        // The turn exactly as the model made it, rejected calls included:
        // every result written below must pair with a call of the same id.
        let mut replay = all_calls.clone();
        replay.extend(rejects.iter().cloned());
        let normalized = view::encode_step(&resp_text, &wire(&replay)?, provider_meta);

        print_turn(&display_payload(&resp_text, &insight, &next_goal, Value::Array(actions.clone())));
        self.save_conversation_snapshot(messages, &normalized, lp.step_number)?;

        // Recorded with an empty slot, filled after routing, so an `exit`
        // turn also lands in the conversation.
        am.append(&normalized)?;
        tr.append(py.None())?;

        let mut reject_results: Vec<Value> = rejects
            .iter()
            .map(|r| {
                json!({
                    "tool_call_id": call_id(r),
                    "content": format!("error: {}", py_str_of(r.get("error").unwrap_or(&Value::Null))),
                })
            })
            .collect();
        reject_results.extend(exit_results);
        if !rejects.is_empty() && stdout_tty() {
            for r in &rejects {
                crate::say!(
                    "\u{26a0}\u{fe0f} {} \u{2014} error returned to the model",
                    py_str_of(r.get("error").unwrap_or(&Value::Null))
                );
            }
        }
        // The modes' switch calls are taken out here, after the turn is
        // shown and recorded, and answered on their own ids beside the
        // notes' results. The flags they leave shape the next request only.
        let mut routed = Vec::new();
        for (a, c) in actions.into_iter().zip(calls) {
            match mode_of(&a) {
                Some(i) => {
                    let answer = switch_mode(&MODES[i], &mut lp.modes_on[i], &a);
                    note_results.push(json!({"tool_call_id": call_id(&c), "content": render_tool_response(&answer)}));
                }
                None => routed.push((a, c)),
            }
        }
        let (actions, calls): (Vec<Value>, Vec<Value>) = routed.into_iter().unzip();
        if actions.is_empty() {
            // Nothing to route (only notes calls, switch calls or rejected
            // calls): persist their results and the errors, and go on to the
            // next step.
            let mut all = note_results;
            all.extend(reject_results);
            set_last(tr, view::encode_results(&all))?;
            lp.is_first = false;
            return Ok(Flow::Continue);
        }

        let mut action_result = ctl.route_action(py, &actions)?;

        if action_result.get("action").and_then(Value::as_str) == Some("exit") {
            // Close the pairing: a call with no result is a malformed
            // transcript every provider rejects.
            if !calls.is_empty() || !note_results.is_empty() {
                let mut all = note_results;
                all.extend(reject_results);
                all.extend(calls.iter().map(|c| json!({"tool_call_id": call_id(c), "content": "run ended"})));
                set_last(tr, view::encode_results(&all))?;
            }
            return Ok(Flow::Exit);
        }

        // A web result leaves the result here and rides into the next step
        // as a <tool> block.
        let mut web_results_list: Vec<String> = Vec::new();
        if action_result.get("tool").and_then(Value::as_str) == Some("web") && action_result.get("result").is_some() {
            if let Some(result) = action_result.as_object_mut().and_then(|o| o.shift_remove("result")) {
                web_results_list.push(py_str_of(&result));
            }
        } else if action_result.get("action").and_then(Value::as_str) == Some("multiple")
            && action_result.get("results").is_some()
        {
            if let Some(Value::Array(results)) = action_result.get_mut("results") {
                for result in results.iter_mut() {
                    if result.get("tool").and_then(Value::as_str) == Some("web") && result.get("result").is_some() {
                        if let Some(r) = result.as_object_mut().and_then(|o| o.shift_remove("result")) {
                            web_results_list.push(py_str_of(&r));
                        }
                    }
                }
            }
        }
        let mut web_tool_response = String::new();
        if !web_results_list.is_empty() {
            web_tool_response = format!("<tool>\n{}\n</tool>", web_results_list.join("\n"));
            // This step's slot, for the next (digest) step to backfill.
            lp.web_memory_index = Some(tr.len() as i64 - 1);
        }
        self.state().pending_web_response = web_tool_response;

        let compacted_result = tool_response_for_memory(&action_result);

        // The digest step: fold the numbered scratchpad entries the model
        // just wrote into the web step's slot as the web call's `result`.
        // Only the web call's own result is re-rendered, on its own id.
        if is_web_digest {
            if let Some(idx) = lp.web_memory_index.filter(|&i| i >= 0 && (i as usize) < tr.len()) {
                let slot = tr.get_item(idx as usize)?;
                if slot.is_truthy()? && lp.web_pending.as_ref().is_some_and(|p| !p.is_empty()) {
                    let entries: Vec<String> = actions
                        .iter()
                        .filter(|a| {
                            a.get("type").and_then(Value::as_str) == Some("scratchpad")
                                && a.get("value").is_some_and(truthy)
                        })
                        .map(|a| py_strip(&py_str_of(a.get("value").unwrap_or(&Value::Null))).to_string())
                        .collect();
                    let prior = view::decode_results(&slot_text(&slot)?);
                    let keyed = prior.iter().all(|m| m.get("tool_call_id").is_some_and(truthy));
                    if !entries.is_empty() && !prior.is_empty() && keyed {
                        let numbered: Vec<String> =
                            entries.iter().enumerate().map(|(i, e)| format!("{}. {e}", i + 1)).collect();
                        let numbered = numbered.join("\n");
                        let mut refreshed: Vec<(Value, String)> = Vec::new();
                        for (id, d) in lp.web_pending.iter_mut().flatten() {
                            fill_web_findings(d, &numbered);
                            refreshed.push((id.clone(), render_tool_response(d)));
                        }
                        let folded: Vec<Value> = prior
                            .iter()
                            .map(|m| {
                                let id = m.get("tool_call_id").cloned().unwrap_or(Value::Null);
                                // A Python dict: the last write for an id wins.
                                let content = match refreshed.iter().rev().find(|(k, _)| *k == id) {
                                    Some((_, text)) => Value::String(text.clone()),
                                    None => m.get("content").cloned().unwrap_or(Value::Null),
                                };
                                json!({"tool_call_id": id, "content": content})
                            })
                            .collect();
                        tr.set_item(idx as usize, view::encode_results(&folded))?;
                    }
                    lp.web_memory_index = None;
                    lp.web_pending = None;
                }
            }
        }

        // One result per call, keyed to its id. When one combined result
        // came back, the first id carries it and the rest point at it.
        let per = if compacted_result.get("action").and_then(Value::as_str) == Some("multiple") {
            compacted_result.get("results").and_then(Value::as_array)
        } else {
            None
        };
        let first_id = calls.first().map(call_id);
        let pairs: Vec<(Value, Value)> = if calls.len() <= 1 {
            vec![(first_id.unwrap_or_else(|| json!("call_0")), compacted_result.clone())]
        } else if let Some(per) = per.filter(|p| p.len() == calls.len()) {
            calls.iter().zip(per).map(|(c, d)| (call_id(c), d.clone())).collect()
        } else {
            vec![(first_id.unwrap_or(Value::Null), compacted_result.clone())]
        };
        let mut action_results: Vec<Value> = pairs
            .iter()
            .map(|(id, d)| json!({"tool_call_id": id, "content": render_tool_response(d)}))
            .collect();
        if pairs.len() < calls.len() {
            action_results.extend(
                calls.iter().skip(1).map(|c| json!({"tool_call_id": call_id(c), "content": IN_FIRST_RESULT})),
            );
        }
        if !web_results_list.is_empty() {
            // The web call's own compacted dict(s), keyed to the id each was
            // rendered on, for the digest step's fold.
            let pending: Vec<(Value, Value)> = pairs.iter().filter(|(_, d)| has_web(d)).cloned().collect();
            lp.web_pending = if pending.is_empty() { None } else { Some(pending) };
        }
        let mut all = note_results;
        all.extend(reject_results);
        all.extend(action_results);
        set_last(tr, view::encode_results(&all))?;

        lp.is_first = false;
        Ok(Flow::Continue)
    }
}

#[pymethods]
impl AgentService {
    /// The mac signature. Only provider, model, save_conversation, api_key,
    /// stop_event and external_terminal are used; the app's arguments are taken and ignored,
    /// and **_extra takes what agent_launcher forwards for other agents (its
    /// signature filter cannot read an extension type's arguments).
    #[new]
    #[pyo3(signature = (provider, model, save_conversation=None, api_key=None, stop_event=None,
        task=None, on_complete=None, external_terminal=None, report_usage=None, request_no=None,
        history_file=None, **_extra))]
    #[pyo3(text_signature = "(provider, model, save_conversation=False, api_key=None, stop_event=None, task=None, on_complete=None, external_terminal=True, report_usage=False, request_no=1, history_file=None, **_extra)")]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        provider: String,
        model: String,
        save_conversation: Option<Bound<'_, PyAny>>,
        api_key: Option<Bound<'_, PyAny>>,
        stop_event: Option<Bound<'_, PyAny>>,
        task: Option<Bound<'_, PyAny>>,
        on_complete: Option<Bound<'_, PyAny>>,
        external_terminal: Option<Bound<'_, PyAny>>,
        report_usage: Option<Bound<'_, PyAny>>,
        request_no: Option<Bound<'_, PyAny>>,
        history_file: Option<Bound<'_, PyAny>>,
        _extra: Option<Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let _ = (task, on_complete, report_usage, request_no, history_file);
        let save_conversation = match &save_conversation {
            Some(v) => v.is_truthy()?,
            None => false,
        };
        // Each subagent in a terminal window of its own, the mac default;
        // None too (the app passes False for its piped runs).
        let external_terminal = match &external_terminal {
            Some(v) => v.is_truthy()?,
            None => true,
        };
        let api_key: Option<String> = match &api_key {
            Some(v) => Some(v.str()?.to_string_lossy().into_owned()),
            None => None,
        };
        let stop_event = stop_event.map(Bound::unbind);

        // A unique session id isolates this run's folders: uuid4().hex[:8].
        let hex: String = py.import("uuid")?.call_method0("uuid4")?.getattr("hex")?.extract()?;
        let session_id: String = hex.chars().take(8).collect();

        // The coder's tools (cli_agent: the one-call fallback model is on).
        let llm = Py::new(
            py,
            LLMManager::build(py, &provider, &model, api_key.as_deref(), true, "main", "quality")?,
        )?;

        let controller =
            ControllerView::new(py, &provider, &model, &session_id, api_key.as_deref(), false, external_terminal, save_conversation)?;

        // Rolling handoff compression: the shared controller with this
        // crate's two transcript callables. It reads the instance and
        // builds its own text-mode manager from the class. No UI, so no
        // token callback: the controller skips a missing one.
        let compression = {
            let cls = py
                .import("AutoCua.memory_compression.controller")?
                .getattr("CompressionController")?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("dump_builder", pyo3::wrap_pyfunction!(crate::coder::main::view::compression_dump, py)?)?;
            kwargs.set_item("synthetic_entry", pyo3::wrap_pyfunction!(crate::coder::main::view::compression_entry, py)?)?;
            let stop: Py<PyAny> = match &stop_event {
                Some(event) => event.clone_ref(py),
                None => py.None(),
            };
            cls.call((llm.clone_ref(py), py.get_type::<LLMManager>(), py.None(), stop), Some(&kwargs))?
                .unbind()
        };

        let main_dir = crate::coder::main_dir(py)?;
        let mut system_prompt =
            load_system_prompt(&main_dir).map_err(|e| os_error(&e, &main_dir.join("system_prompt.md")))?;
        if llm.get().async_tools() {
            system_prompt.push_str(ASYNC_TOOLS_ADDENDUM);
        }

        // ./cli_conversation/<session>/ and its raw_reasoning/, only when
        // saving; nothing is ever cleared.
        if save_conversation {
            let conversation_dir = PathBuf::from("cli_conversation").join(&session_id);
            std::fs::create_dir_all(&conversation_dir).map_err(|e| os_error(&e, &conversation_dir))?;
            let raw_reasoning_dir = conversation_dir.join("raw_reasoning");
            std::fs::create_dir_all(&raw_reasoning_dir).map_err(|e| os_error(&e, &raw_reasoning_dir))?;
        }

        Ok(AgentService {
            provider,
            model,
            save_conversation,
            stop_event,
            session_id,
            llm,
            controller: Mutex::new(controller),
            compression,
            system_prompt,
            state: Mutex::new(RunState {
                assistant_messages: PyList::empty(py).unbind(),
                tool_responses: PyList::empty(py).unbind(),
                pending_web_response: String::new(),
            }),
        })
    }

    #[getter]
    fn session_id(&self) -> &str {
        &self.session_id
    }

    #[getter]
    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// The persisted assistant turns of the current or last run (str each).
    #[getter]
    fn assistant_messages(&self, py: Python<'_>) -> Py<PyList> {
        self.state().assistant_messages.clone_ref(py)
    }

    /// The result slots parallel to assistant_messages (str or None).
    #[getter]
    fn tool_responses(&self, py: Python<'_>) -> Py<PyList> {
        self.state().tool_responses.clone_ref(py)
    }

    /// Run the agent loop on `task` until it ends: "Exit", "Max iterations
    /// reached" or "Agent loop ended". Ctrl+C raises KeyboardInterrupt.
    fn process_request(&self, py: Python<'_>, task: Bound<'_, PyAny>) -> PyResult<String> {
        let task = task.str()?.to_string_lossy().into_owned();
        let mut ctl = match self.controller.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return Err(PyRuntimeError::new_err("this AgentService is already running a request"));
            }
        };
        let result = self.run_agent_loop(py, &mut ctl, &task);
        // Python's finally: stop a dangling compression indicator.
        let finished = self.compression.bind(py).call_method0("finish_run");
        // On every way out (exit, an error that ended the run, Ctrl+C) no
        // subagent child is left running. Without the GIL: a child gets up
        // to 3 s to stop.
        let ctl: &mut ControllerView = &mut ctl;
        py.detach(|| ctl.kill_subagents());
        finished?;
        result
    }
}

// ---------------------------------------------------------------------------
// The step's pieces
// ---------------------------------------------------------------------------

fn stdout_tty() -> bool {
    std::io::stdout().is_terminal()
}

/// One step's display: the model's text, its notes (the `insight` and the
/// `next_goal`, see turn_notes) and the routed actions, printed for a person
/// on a terminal and as JSON into a pipe. Only what the step has is shown.
pub(crate) fn display_payload(resp_text: &str, insight: &Value, next_goal: &str, actions: Value) -> String {
    let mut payload = Map::new();
    if !crate::coder::py_strip(resp_text).is_empty() {
        payload.insert("text".into(), Value::String(resp_text.to_string()));
    }
    let insight = insight.as_object().and_then(|o| {
        let field = |key: &str| o.get(key).and_then(Value::as_str).unwrap_or("");
        view::insight_value(field("kind"), field("content"))
    });
    if let Some(insight) = insight {
        payload.insert("insight".into(), insight);
    }
    if !next_goal.is_empty() {
        payload.insert("next_goal".into(), Value::String(next_goal.to_string()));
    }
    if truthy(&actions) {
        payload.insert("action".into(), actions);
    }
    dumps_pretty(&Value::Object(payload))
}

/// The turn's notes, read off its notes calls, the rejected ones too (a
/// notes call with no arguments at all is rejected): the `insight`
/// as {"kind", "content"} ("" when the turn has no `insight` call) and the
/// `next_goal` value ("" when it has none). Each field is the first
/// non-empty one of its calls, stripped.
pub(crate) fn turn_notes(actions: &[Value], rejects: &[Value]) -> (Value, String) {
    // (tool name, its fields): a routed action carries them at its top level.
    let name = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let calls: Vec<(String, &Value)> = actions
        .iter()
        .map(|a| (name(a, "type"), a))
        .chain(rejects.iter().map(|r| (name(r, "name"), r.get("arguments").unwrap_or(&Value::Null))))
        .collect();
    let first = |tool: &str, key: &str| {
        calls
            .iter()
            .filter(|(n, _)| n == tool)
            .filter_map(|(_, fields)| fields.get(key).filter(|v| truthy(v)))
            .map(|v| py_strip(&py_str_of(v)).to_string())
            .find(|v| !v.is_empty())
            .unwrap_or_default()
    };
    let insight = if calls.iter().any(|(n, _)| n == "insight") {
        json!({"kind": first("insight", "kind"), "content": first("insight", "content")})
    } else {
        Value::String(String::new())
    };
    (insight, first("next_goal", "value"))
}

/// The modes that are on, in table order.
fn active_modes(modes_on: &[bool]) -> impl Iterator<Item = &'static Mode> + '_ {
    MODES.iter().zip(modes_on).filter(|(_, on)| **on).map(|(mode, _)| mode)
}

/// The tools the modes that are on take away: all their `hides`, in table
/// order, each name once.
fn hidden_tools(modes_on: &[bool]) -> Vec<&'static str> {
    let mut hidden: Vec<&'static str> = Vec::new();
    for mode in active_modes(modes_on) {
        for &name in mode.hides {
            if !hidden.contains(&name) {
                hidden.push(name);
            }
        }
    }
    hidden
}

/// The index in MODES of the mode whose switch the action calls, if any.
fn mode_of(action: &Value) -> Option<usize> {
    let tool = action.get("type").and_then(Value::as_str)?;
    MODES.iter().position(|mode| mode.tool == tool)
}

/// One switch call: the mode's flag set as `value` asks ("on" or "off"),
/// and the result the call is answered with. Any other value is an error
/// and leaves the flag as it is.
fn switch_mode(mode: &Mode, on: &mut bool, action: &Value) -> Value {
    let (now_on, message) = match action.get("value").and_then(Value::as_str) {
        Some("on") => (true, mode.on_message),
        Some("off") => (false, mode.off_message),
        _ => {
            let message = format!("{} value must be \"on\" or \"off\".", mode.tool);
            return json!({"status": "error", "action": mode.tool, "message": message});
        }
    };
    *on = now_on;
    json!({"status": "success", "action": mode.tool, "message": message})
}

/// A mode's block, last in the live <persistent_memory> while the mode is
/// on: its on message and its skill, the skill file read fresh every step
/// (or why it could not be read).
fn mode_block(py: Python<'_>, mode: &Mode) -> PyResult<String> {
    // Part by part, so the path is written the way this OS writes it.
    let mut file = crate::coder::coder_dir(py)?.clone();
    for part in mode.skill_file.split('/') {
        file.push(part);
    }
    let skill = match crate::coder::read_text(&file) {
        Ok(text) => format!("<skill name=\"{}\">\n{text}\n</skill>", mode.skill_name),
        Err(e) => format!(
            "The {} skill could not be read: {}.",
            mode.skill_name,
            crate::coder::sandbox::service::py_os_error(&e, &file)
        ),
    };
    Ok(format!("<{tool}>\n{on}\n\n{skill}\n</{tool}>", tool = mode.tool, on = mode.on_message))
}

pub(crate) fn print_turn(payload: &str) {
    if stdout_tty() {
        crate::coder::out(&view::format_terminal(payload));
    } else {
        crate::coder::out(&view::format_stream_json(payload));
    }
}

/// `(raw_response.get("text") or "").strip()`. Python raises for a text that
/// is not a string, which ends the run like any error in a step.
fn response_text(raw_response: &Value) -> PyResult<String> {
    match raw_response.get("text") {
        Some(Value::String(s)) => Ok(py_strip(s).to_string()),
        Some(other) if truthy(other) => Err(PyAttributeError::new_err(format!(
            "'{}' object has no attribute 'strip'",
            py_type_name(other)
        ))),
        _ => Ok(String::new()),
    }
}

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

/// A call's "id" (null when it has none).
fn call_id(call: &Value) -> Value {
    call.get("id").cloned().unwrap_or(Value::Null)
}

/// The calls in the OpenAI shape the assistant turn carries. A call without
/// an id or a name is Python's KeyError.
fn wire(calls: &[Value]) -> PyResult<Value> {
    view::wire_calls_from(calls)
        .map(Value::Array)
        .map_err(|key| PyKeyError::new_err(key.trim_matches('\'').to_string()))
}

/// `tool_responses[-1] = value`.
fn set_last(list: &Bound<'_, PyList>, value: String) -> PyResult<()> {
    match list.len().checked_sub(1) {
        Some(last) => list.set_item(last, value),
        None => Ok(()),
    }
}

/// A tool slot as decode_results takes it: "" for None or another falsy slot.
fn slot_text(slot: &Bound<'_, PyAny>) -> PyResult<String> {
    if !slot.is_truthy()? {
        return Ok(String::new());
    }
    Ok(slot.str()?.to_string_lossy().into_owned())
}

/// Prompt caching: mark the newest persistent turn, the last tool result (or
/// the opening request on step 1), never the live tail, which is rebuilt
/// every request. OpenRouter passes the parts-list marker on, the Anthropic
/// translation lifts it onto the tool_result block, and aws makes a Bedrock
/// cachePoint of it. The provider is compared as given, as in Python.
fn mark_cache(provider: &str, messages: &mut [Value]) {
    if !matches!(provider, "openrouter" | "anthropic" | "aws") || messages.len() <= 2 {
        return;
    }
    let last = messages.len() - 1;
    for (i, msg) in messages.iter_mut().enumerate().rev() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "tool" && role != "user" {
            continue;
        }
        if role == "user" && i == last {
            continue;
        }
        let text = match msg.get("content") {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        if let (Some(text), Some(obj)) = (text, msg.as_object_mut()) {
            obj.insert(
                "content".into(),
                json!([{"type": "text", "text": text, "cache_control": {"type": "ephemeral"}}]),
            );
        }
        break;
    }
}

/// <agent_sitting>: the workspace and the current folder, as JSON.
fn get_agent_sitting(ctl: &ControllerView) -> String {
    let sandbox = &ctl.cli_service.sandbox;
    let mut sitting = Map::new();
    sitting.insert(
        "your_workspace".into(),
        Value::String(sandbox.sandbox_root.to_string_lossy().into_owned()),
    );
    sitting.insert("current_sitting".into(), Value::String(sandbox.get_cwd()));
    dumps_pretty(&Value::Object(sitting))
}

/// The todo list from the task tracker's own file, stripped; "" when none.
fn read_todo_from_file(ctl: &ControllerView) -> String {
    let todo_file = &ctl.task_tracker.todo_file;
    if !todo_file.exists() {
        return String::new();
    }
    match crate::coder::read_text(todo_file) {
        Ok(text) => py_strip(&text).to_string(),
        Err(_) => String::new(),
    }
}

/// The plan as a <plan no="N"> block, its lines numbered like `view`; ""
/// when there is no plan yet.
fn read_plan_from_file(ctl: &ControllerView) -> String {
    let rendered = ctl.plan_service.render_plan();
    if rendered.is_empty() {
        return String::new();
    }
    format!("<plan no=\"{}\">\n{rendered}\n</plan>", ctl.plan_service.get_plan_no())
}

/// The result kept in memory for a step: a successful web result shrinks to
/// its query and a pointer (its raw text rides the next step's <tool> block
/// once, and the digest step's findings replace the pointer). Everything
/// else is kept as it is.
fn tool_response_for_memory(action_result: &Value) -> Value {
    fn compact(entry: &Value) -> Value {
        if entry.get("tool").and_then(Value::as_str) == Some("web")
            && entry.get("status").and_then(Value::as_str) == Some("success")
        {
            let mut web = Map::new();
            web.insert("status".into(), json!("success"));
            web.insert("action".into(), json!("web"));
            web.insert("query".into(), entry.get("query").cloned().unwrap_or_else(|| json!("")));
            web.insert("result".into(), json!(WEB_POINTER));
            return Value::Object(web);
        }
        entry.clone()
    }
    let mut res = action_result.clone();
    if res.get("action").and_then(Value::as_str) == Some("multiple") && res.get("results").is_some() {
        let compacted = match res.get("results") {
            Some(Value::Array(results)) => Some(Value::Array(results.iter().map(compact).collect())),
            _ => None,
        };
        if let (Some(compacted), Some(obj)) = (compacted, res.as_object_mut()) {
            obj.insert("results".into(), compacted);
        }
        return res;
    }
    compact(&res)
}

/// The digest step's numbered findings into a stashed web result, in place:
/// they become the web entry's `result`.
fn fill_web_findings(data: &mut Value, findings: &str) {
    fn fill(entry: &mut Value, findings: &str) {
        let Some(obj) = entry.as_object_mut() else { return };
        let is_web = obj.get("action").and_then(Value::as_str) == Some("web")
            || obj.get("tool").and_then(Value::as_str) == Some("web");
        if is_web {
            obj.shift_remove("message");
            obj.shift_remove("web_result");
            obj.insert("result".into(), Value::String(findings.to_string()));
        }
    }
    let multiple = data.get("action").and_then(Value::as_str) == Some("multiple");
    match data.get_mut("results") {
        Some(Value::Array(results)) if multiple => {
            for r in results.iter_mut() {
                fill(r, findings);
            }
        }
        _ => fill(data, findings),
    }
}

/// True when a (compacted) result is, or contains, a web result.
fn has_web(data: &Value) -> bool {
    let Some(obj) = data.as_object() else { return false };
    let action = obj.get("action").and_then(Value::as_str);
    if action == Some("web") || obj.get("tool").and_then(Value::as_str) == Some("web") {
        return true;
    }
    action == Some("multiple") && matches!(obj.get("results"), Some(Value::Array(r)) if r.iter().any(has_web))
}

/// Python's OSError for a failed file call on `path`. OSError(errno,
/// strerror, filename) makes the matching subclass (FileNotFoundError, ...)
/// with Python's text: "[Errno 2] No such file or directory: 'path'".
fn os_error(e: &std::io::Error, path: &Path) -> PyErr {
    let Some(code) = e.raw_os_error() else {
        return PyOSError::new_err(e.to_string());
    };
    let name = path.to_string_lossy().into_owned();
    let full = e.to_string();
    let tail = format!(" (os error {code})");
    let message = full.strip_suffix(&tail).unwrap_or(&full).to_string();
    if cfg!(windows) {
        // On Windows the fourth argument, winerror, picks the subclass.
        PyOSError::new_err((0, message, name, code))
    } else {
        PyOSError::new_err((code, message, name))
    }
}

/// str(datetime.now()): local time, with microseconds unless they are 0.
fn py_now() -> String {
    let now = chrono::Local::now();
    let micros = now.timestamp_subsec_micros() % 1_000_000;
    let base = now.format("%Y-%m-%d %H:%M:%S").to_string();
    if micros == 0 {
        base
    } else {
        format!("{base}.{micros:06}")
    }
}

/// The "⚡ Working" line with cycling dots while the LLM call blocks, so the
/// terminal does not look frozen. On a terminal only, on its own thread.
struct Spinner {
    stop: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
}

impl Spinner {
    /// None when the thread cannot be started: the call then runs without it.
    fn start() -> Option<Spinner> {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("coder-spinner".into())
            .spawn(move || {
                let dots = ["", ".", "..", "..."];
                let mut idx = 0usize;
                while matches!(stop_rx.try_recv(), Err(TryRecvError::Empty)) {
                    crate::coder::out_raw(&format!("\r\u{26a1} Working{:<3}", dots[idx % dots.len()]));
                    idx += 1;
                    if !matches!(stop_rx.recv_timeout(Duration::from_millis(400)), Err(RecvTimeoutError::Timeout)) {
                        break;
                    }
                }
                // Clear the line so the step prints cleanly.
                crate::coder::out_raw(&format!("\r{}\r", " ".repeat(24)));
                let _ = done_tx.send(());
            })
            .ok()?;
        Some(Spinner { stop: stop_tx, done: done_rx })
    }

    /// Stop it and wait for the line to clear, at most 1 second.
    fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.done.recv_timeout(Duration::from_secs(1));
    }
}
