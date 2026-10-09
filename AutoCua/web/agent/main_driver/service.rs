//! AgentService — Rust port of main_driver/service.py (the agent loop).
//!
//! PyO3 hybrid: this loop is Rust, but it drives the SAME Python modules the
//! original did — tool_registry.service.LLMManager, controller.ControllerView and
//! memory_compression.CompressionController are imported and called over the
//! GIL. The two transcript lists are real Python lists because
//! CompressionController.apply_pending splices them IN PLACE, and they are
//! exposed post-run exactly as before (assistant_messages / tool_responses /
//! last_messages) for the conversation service to persist.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use pyo3::exceptions::{PyException, PyFileNotFoundError, PyIOError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use regex::Regex;
use serde_json::{json, Map, Value};

use super::view;
use crate::browser::{truthy, BrowserScanner, CHROME_PORT};

// Run-boundary request markers: on resume, the bridge entry that ended the
// prior run gets its empty tool slot filled with the NEW run's request, so the
// persisted memory attributes every run's steps to the request that drove
// them. The sentence rides in the bridge's next_goal (prefix match).
const BRIDGE_SIGNATURE: &str = "\"next_goal\": \"Previous run concluded.";

fn request_marker(n: i64, task: &str) -> String {
    format!("<updated_user_request no=\"{n}\">\n{task}\n</updated_user_request no=\"{n}\">")
}

fn re_marker_no() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"<updated_user_request no="(\d+)">"#).unwrap())
}

fn re_step_prefix() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)^(<Step_no=\d+ />\n)(.*)").unwrap())
}

fn py_err_str(py: Python<'_>, e: &PyErr) -> String {
    e.value(py)
        .str()
        .and_then(|s| s.extract::<String>())
        .unwrap_or_else(|_| e.to_string())
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// The placeholder in system_prompt.md that is filled with the host OS when
/// the prompt is loaded.
/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra on
/// the Responses endpoint): its calls run after the turn ends and no result
/// reaches it mid-turn, which invites three habits the loop cannot absorb.
/// The shared rules stay as written; the prompt on disk is untouched.
const ASYNC_TOOLS_ADDENDUM: &str = "\n<async_tools>\n\
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's input. The rules above are unchanged: one notes call first, then every action whose target is on the current page, stopping where the page must change. Because nothing comes back mid-turn:\n\
- Never write a text message in a turn that contains tool calls, and never state that an action succeeded or failed: that verdict belongs to the next step's Memory, judged on the next input.\n\
- `done` stays the only call of its turn. Never add it after this turn's actions: their results have not arrived, so nothing is verified yet.\n\
- Do not add a `wait` after your actions to let the page load: the scanner waits for the page before the next input.\n\
</async_tools>\n";

const OS_PLACEHOLDER: &str = "{current_os}";

/// Appended to the live user message for a Claude model (quality mode).
/// Claude treats a notes tool as a think step: it calls `agent_memory`, ends
/// its turn to wait for the result, and acts on the NEXT turn - one call per
/// turn and a wasted scan per action. Saying in the live message that the
/// notes never end a turn puts the notes and the actions in one message
/// (measured 2026-10-05 on the desktop driver: 0/5 -> 6/6 together with the
/// matching <blocks> rule in claude_prompt.md and the agent_memory
/// description). Live message only, never persisted.
const CLAUDE_BATCH_REMINDER: &str = "\n\n<critical>This turn must carry the `agent_memory` call AND the action calls that do its \"Doing\" in this one response. `agent_memory` alone is not a step: it has no result, so never end the turn after it.</critical>";

/// The OS this browser runs on, in the prompt's own words: mac, windows or
/// linux.
fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        os => os,
    }
}

/// Clear all contents inside AutoCua/web/scratchpad/ for a fresh start.
/// With a session_id, only that session's subdirectory is wiped — parallel
/// agents each own scratchpad/{session_id}/ and must not touch each other's.
/// Without one (single-agent run), the whole scratchpad is cleared exactly as
/// before, which also garbage-collects stale session subdirs.
fn cleanup_scratchpad(agent_dir: &PathBuf, session_id: Option<&str>) -> PyResult<()> {
    let mut scratchpad = agent_dir
        .parent()
        .map(|p| p.join("scratchpad"))
        .unwrap_or_else(|| PathBuf::from("scratchpad"));
    if let Some(sid) = session_id {
        scratchpad = scratchpad.join(sid);
    }
    if scratchpad.exists() {
        let entries = std::fs::read_dir(&scratchpad)
            .map_err(|e| PyIOError::new_err(format!("{}: {e}", scratchpad.display())))?;
        for item in entries.flatten() {
            let path = item.path();
            let res = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            res.map_err(|e| PyIOError::new_err(format!("{}: {e}", path.display())))?;
        }
    } else {
        std::fs::create_dir_all(&scratchpad)
            .map_err(|e| PyIOError::new_err(format!("{}: {e}", scratchpad.display())))?;
    }
    Ok(())
}

/// Trim an OLDER legacy history step for context: drop 'action' only.
/// LEGACY ONLY: native turns are append-only and never rewritten.
fn trim_history_entry(entry: &str) -> String {
    let (prefix, json_part) = match re_step_prefix().captures(entry) {
        Some(m) => (
            m.get(1).map(|g| g.as_str()).unwrap_or(""),
            m.get(2).map(|g| g.as_str()).unwrap_or(entry),
        ),
        None => ("", entry),
    };
    match serde_json::from_str::<Value>(json_part) {
        Ok(Value::Object(mut data)) => {
            data.shift_remove("action");
            format!("{prefix}{}", view::dumps_pretty(&Value::Object(data)))
        }
        _ => entry.to_string(),
    }
}

/// <persistent_memory>: the agent's OWN live state, rebuilt fresh EVERY step:
/// the todo, the scratchpad, then <skills> (the skills by id, loaded and
/// removed only when the agent asks) and <domain_knowledge> (what is known
/// about the current tab's site, there on its own while the tab is on it),
/// both from controller/skills and each left out when empty.
fn persistent_memory(todo_list: &str, scratchpad: &str, skills: &str, domain: &str) -> String {
    let todo = todo_list.trim();
    let pad = scratchpad.trim();
    let tail = |block: &str| if block.is_empty() { String::new() } else { format!("\n\n{block}") };
    format!(
        "<persistent_memory>\n<todo_list>\n{}\n</todo_list>\n\n<scratchpad>\n{}\n</scratchpad>{}{}\n</persistent_memory>",
        if todo.is_empty() { "none" } else { todo },
        if pad.is_empty() { "none" } else { pad },
        tail(skills),
        tail(domain),
    )
}

/// The <skills> and <domain_knowledge> blocks of <persistent_memory>
/// (controller/skills): the domain knowledge follows the current tab's url
/// first. Each is empty when it has nothing.
fn skills_blocks(py: Python<'_>, scanner: &crate::browser::BrowserScanner) -> (String, String) {
    let url = scanner.current_url(py).unwrap_or_default();
    crate::controller::skills::service::follow_url(&url);
    (crate::controller::skills::service::block(), crate::controller::skills::service::domain_block())
}

/// <all_tabs>: every open tab with its url — empty string on a digest step.
fn all_tabs_block(all_tabs: &str) -> String {
    let text = all_tabs.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!("<all_tabs>\n{text}\n</all_tabs>")
    }
}

/// Per-step context that survives into the outer error handler so a run dying
/// mid-step can still close the call/result pairing (Python guards the same
/// spot with a NameError check on its closures).
struct StepCtx {
    calls: Vec<Value>,
    /// The `reasoning` call's `ok` results (quality mode), first in every slot.
    note_results: Vec<Value>,
    reject_results: Vec<Value>,
}

enum Flow {
    Continue,
    Break,
}

/// In place of <element_tree> on the steps of scrape mode
/// (RunState::scrape_mode): where the data is and which tools this step has.
/// The scraping skill is in <skills>, loaded as the mode opened.
fn scrape_mode_block() -> String {
    "<scrape_mode>\nYou are in scrape mode. No page was scanned this step: the records your last read \
     returned, attached to that call above, and a clean screenshot of each screen it covered (with this input, \
     top to bottom, after any screenshot of the page) are the whole input. The only tools this step are `more`, \
     `scratchpad`, `update_todo`, `skills` and `exit_scrape_mode`; there is no notes call and no page tool. The \
     scraping skill is loaded in <skills>: follow it. Check the records against the screenshots and write what the \
     task needs to `scratchpad`, then, in the same turn, `more` for the next 3 screens while what the task needs \
     goes on below, or `exit_scrape_mode` once it is met. The screenshots are for this step only. After the exit, \
     your run_script call and its data stay in your history; the steps of the mode leave it, and the scratchpad \
     keeps what you wrote.\n</scrape_mode>"
        .to_string()
}

/// Whether a step's results, single or batched, include a successful call
/// of one of `tools`.
fn step_called(action_result: &Value, tools: &[&str]) -> bool {
    let is = |r: &Value| {
        r.get("tool").and_then(Value::as_str).is_some_and(|t| tools.contains(&t))
            && r.get("status").and_then(Value::as_str) == Some("success")
    };
    is(action_result)
        || action_result
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|rs| rs.iter().any(is))
}

/// A run_script of type "scrape", or a `more`, brought records back (its
/// result carries `scrape_mode`, controller/run_script): scrape mode from the
/// next step. A read that brought nothing opens no mode.
fn called_scrape(action_result: &Value) -> bool {
    let opened = |r: &Value| {
        matches!(r.get("tool").and_then(Value::as_str), Some("run_script" | "more"))
            && r.get("status").and_then(Value::as_str) == Some("success")
            && r.get("scrape_mode").and_then(Value::as_bool) == Some(true)
    };
    opened(action_result)
        || action_result
            .get("results")
            .and_then(Value::as_array)
            .is_some_and(|rs| rs.iter().any(opened))
}

/// `exit_scrape_mode` ran: back to the page from the next step.
fn called_exit(action_result: &Value) -> bool {
    step_called(action_result, &["exit_scrape_mode"])
}

/// `exit_scrape_mode` ran: the steps of scrape mode leave the history,
/// assistant turns and results alike. The step whose run_script read opened
/// the mode stays, with its data; what the mode's steps did is on disk
/// already (the scratchpad, the todo).
fn retire_scrape_mode(am: &Bound<'_, PyList>, tr: &Bound<'_, PyList>, start: usize) -> PyResult<()> {
    let n = am.len();
    if start + 1 < n {
        am.del_slice(start + 1, n)?;
    }
    let n = tr.len();
    if start + 1 < n {
        tr.del_slice(start + 1, n)?;
    }
    Ok(())
}

struct RunState {
    assistant_messages: Py<PyAny>,
    tool_responses: Py<PyAny>,
    last_messages: Py<PyAny>,
    /// Scrape mode: set when a step's run_script scrape (or `more`) brought
    /// records, cleared when `exit_scrape_mode` runs. While set, a step
    /// carries the read alone - no scan, no tree, no element ids - and the
    /// schema holds only the tools of the job (LLMManager::set_scrape_step).
    scrape_mode: bool,
    /// The history slot of the step that opened the mode: at
    /// `exit_scrape_mode` everything after it leaves the history
    /// (retire_scrape_mode).
    scrape_start: Option<usize>,
    /// The pictures of the screens the last scrape read covered
    /// (run_script window.rs): sent with the next input, once.
    scrape_shots: Vec<String>,
}

#[pyclass(frozen)]
pub struct AgentService {
    speed: String,
    headless: bool,
    save_conversation: bool,
    browser_port: u16,
    system_prompt: String,
    llm_manager: Py<crate::tool_registry::service::LLMManager>,
    stop_event: Option<Py<PyAny>>,
    scanner: Py<BrowserScanner>,
    controller: crate::controller::view::ControllerView,
    compression: Py<PyAny>,
    text_callback: Option<Py<PyAny>>,
    tool_callback: Option<Py<PyAny>>,
    token_callback: Option<Py<PyAny>>,
    prior_history: Option<Py<PyAny>>,
    state: Mutex<RunState>,
}

impl AgentService {
    fn stop_set(&self, py: Python<'_>) -> PyResult<bool> {
        match &self.stop_event {
            Some(ev) => ev.bind(py).call_method0("is_set")?.is_truthy(),
            None => Ok(false),
        }
    }

    /// Push a tool-flow event to the frontend bottom chain (best-effort).
    fn emit_flow(&self, py: Python<'_>, event: &str, payload: Option<&Value>) {
        let Some(cb) = &self.tool_callback else { return };
        let payload_obj: Py<PyAny> = match payload {
            Some(v) => match pythonize::pythonize(py, v) {
                Ok(o) => o.unbind(),
                Err(_) => return,
            },
            None => py.None(),
        };
        let _ = cb.call1(py, (event, payload_obj));
    }

    fn release_all_inputs(&self) -> PyResult<()> {
        self.controller.controller_service.release_all_inputs();
        Ok(())
    }

    fn provider_name(&self) -> String {
        self.llm_manager.get().provider.clone()
    }

    fn model_name(&self) -> String {
        self.llm_manager.get().model_short_name.clone()
    }

    /// Read the current todo list from the task tracker's todo.md file.
    fn read_todo_from_file(&self) -> String {
        let p = &self.controller.todo_tracker.todo_file;
        if !p.exists() {
            return String::new();
        }
        match std::fs::read_to_string(p) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                println!("Error reading todo file: {e}");
                String::new()
            }
        }
    }

    /// Read the current scratchpad entries from the scratchpad service's file.
    fn read_scratchpad_from_file(&self) -> String {
        let p = &self.controller.scratchpad_service.scratchpad_file;
        if !p.exists() {
            return String::new();
        }
        match std::fs::read_to_string(p) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                println!("Error reading scratchpad file: {e}");
                String::new()
            }
        }
    }

    /// Save raw LLM response before any parsing/normalization.
    fn save_raw_response(&self, raw_response: &str, step_number: i64) {
        if !self.save_conversation {
            return;
        }
        let path = PathBuf::from("raw_reasoning").join(format!("raw_response_{step_number}.txt"));
        if let Err(e) = std::fs::write(&path, raw_response) {
            println!("Error saving raw response: {e}");
        }
    }

    /// Cached messages carry content as a list of {type,text,...} blocks.
    fn message_text(content: &Bound<'_, PyAny>) -> PyResult<String> {
        if let Ok(list) = content.downcast::<PyList>() {
            let mut parts: Vec<String> = Vec::new();
            for item in list.iter() {
                if let Ok(d) = item.downcast::<PyDict>() {
                    let text = match d.get_item("text")? {
                        Some(t) => t.str()?.extract()?,
                        None => String::new(),
                    };
                    parts.push(text);
                }
            }
            return Ok(parts.join("\n"));
        }
        if let Ok(s) = content.downcast::<PyString>() {
            return s.extract();
        }
        content.str()?.extract()
    }

    /// Save a numbered conversation file rendering the EXACT payload sent
    /// this step plus this step's freshly generated response.
    fn save_conversation_snapshot(
        &self,
        py: Python<'_>,
        messages: &Bound<'_, PyList>,
        current_assistant_response: &str,
        image_sent: bool,
        interaction_count: i64,
    ) {
        let attempt = (|| -> PyResult<()> {
            let file = PathBuf::from("conversation")
                .join(format!("conversation_{interaction_count}.txt"));
            let mut out = String::new();
            out.push_str("=== CONVERSATION LOG ===\n");
            out.push_str(&format!("Session Started: {}\n", now_stamp()));
            out.push_str(&format!("Provider: {}\n", self.provider_name()));
            out.push_str(&format!("Model: {}\n", self.model_name()));
            out.push_str(&format!("Current Interaction: #{interaction_count}\n"));
            out.push_str(&"=".repeat(60));
            out.push_str("\n\n");

            for m in messages.iter() {
                let role: String = match m.call_method1("get", ("role", "?")) {
                    Ok(r) => r.str()?.extract::<String>()?.to_uppercase(),
                    Err(_) => "?".to_string(),
                };
                let content = m.call_method1("get", ("content", ""))?;
                if role == "SYSTEM" {
                    out.push_str("=== SYSTEM PROMPT ===\n");
                    out.push_str(&Self::message_text(&content)?);
                    out.push_str("\n\n");
                    out.push_str(&"=".repeat(60));
                    out.push_str("\n\n");
                } else {
                    if role == "TOOL" && Self::message_text(&content)? == view::REASONING_RESULT {
                        continue; // the `reasoning` call's `ok`: wire plumbing, not memory
                    }
                    out.push_str(&format!("--- {role} ---\n"));
                    let tool_calls = m.call_method1("get", ("tool_calls",))?;
                    if tool_calls.is_truthy()? {
                        // A native assistant turn carries its substance in the
                        // tool_calls (the `reasoning` call, or the notes on the
                        // action calls in fast mode and older saves) — render
                        // it the readable four-block way.
                        let calls: Value = pythonize::depythonize(&tool_calls)?;
                        out.push_str(&view::snapshot_turn(&Self::message_text(&content)?, &calls));
                    } else {
                        out.push_str(&Self::message_text(&content)?);
                    }
                    out.push_str("\n\n");
                }
            }

            if image_sent {
                out.push_str("[Screenshot sent with the latest user message]\n\n");
            }
            out.push_str(&format!(
                "--- ASSISTANT (response - step {interaction_count}) ---\n"
            ));
            out.push_str(current_assistant_response);
            out.push('\n');

            std::fs::write(&file, out).map_err(|e| PyIOError::new_err(e.to_string()))?;
            println!("Memory snapshot saved: conversation_{interaction_count}.txt");
            Ok(())
        })();
        if let Err(e) = attempt {
            println!("Error saving conversation snapshot: {}", py_err_str(py, &e));
        }
    }

    fn save_conversation_files(
        &self,
        py: Python<'_>,
        messages: &Bound<'_, PyList>,
        current_assistant_response: &str,
        image_sent: bool,
        interaction_count: i64,
    ) {
        if self.save_conversation {
            self.save_conversation_snapshot(
                py,
                messages,
                current_assistant_response,
                image_sent,
                interaction_count,
            );
        }
    }
}

/// The call/result pairing closers — a saved tool call with no matching
/// result is a malformed transcript every provider rejects on resume.
fn pair_results(
    tr: &Bound<'_, PyList>,
    calls: &[Value],
    note_results: &[Value],
    reject_results: &[Value],
    action_result: &Value,
) -> PyResult<()> {
    let batch = action_result.get("results").and_then(Value::as_array);
    let paired: Vec<Value> = if let (false, Some(batch)) = (calls.is_empty(), batch) {
        if batch.len() == calls.len() {
            calls
                .iter()
                .enumerate()
                .map(|(idx, c)| {
                    json!({
                        "tool_call_id": c["id"].clone(),
                        "content": view::dumps_pretty(&batch[idx]),
                    })
                })
                .collect()
        } else {
            whole_envelope(calls, action_result)
        }
    } else if !calls.is_empty() {
        whole_envelope(calls, action_result)
    } else {
        Vec::new()
    };
    if !tr.is_empty() {
        let mut all: Vec<Value> = note_results.to_vec();
        all.extend(reject_results.iter().cloned());
        all.extend(paired);
        tr.set_item(tr.len() - 1, view::encode_results(&all))?;
    }
    Ok(())
}

fn whole_envelope(calls: &[Value], compacted: &Value) -> Vec<Value> {
    let envelope = view::dumps_pretty(compacted);
    let mut paired = vec![json!({
        "tool_call_id": calls[0]["id"].clone(),
        "content": envelope,
    })];
    for c in &calls[1..] {
        paired.push(json!({
            "tool_call_id": c["id"].clone(),
            "content": "(result included in the first tool result of this batch)",
        }));
    }
    paired
}

/// Drop this step's turn and its empty slot — nothing ran, so there is
/// nothing to remember. Returns true when it dropped something.
fn discard_step(
    am: &Bound<'_, PyList>,
    tr: &Bound<'_, PyList>,
    normalized: &str,
) -> PyResult<bool> {
    if am.is_empty() || tr.is_empty() {
        return Ok(false);
    }
    let last_slot_none = tr.get_item(tr.len() - 1)?.is_none();
    let last_entry: Option<String> = am.get_item(am.len() - 1)?.extract().ok();
    if last_slot_none && last_entry.as_deref() == Some(normalized) {
        am.call_method0("pop")?;
        tr.call_method0("pop")?;
        return Ok(true);
    }
    Ok(false)
}

/// Last resort for a run dying mid-step: record the note against every call
/// so nothing dangles.
fn close_pairing(
    tr: &Bound<'_, PyList>,
    calls: &[Value],
    note_results: &[Value],
    reject_results: &[Value],
    note: &str,
) -> PyResult<()> {
    if tr.is_empty() {
        return Ok(());
    }
    let last_is_none = tr.get_item(tr.len() - 1)?.is_none();
    if last_is_none && (!calls.is_empty() || !reject_results.is_empty() || !note_results.is_empty()) {
        let mut all: Vec<Value> = note_results.to_vec();
        all.extend(reject_results.iter().cloned());
        for c in calls {
            all.push(json!({"tool_call_id": c["id"].clone(), "content": note}));
        }
        tr.set_item(tr.len() - 1, view::encode_results(&all))?;
    }
    Ok(())
}

#[pymethods]
impl AgentService {
    /// cli_callback is accepted for GUI parity with the desktop agents; the
    /// browser driver has no CLI agent, so it is ignored. **_extra absorbs
    /// kwargs meant for other agents (external_terminal) — agent_launcher's
    /// signature filter forwards everything to an extension type.
    #[new]
    #[pyo3(signature = (provider, model, save_conversation=false, frontend_callback=None,
        text_callback=None, web_callback=None, shell_callback=None, cli_callback=None,
        tool_callback=None, token_callback=None, api_key=None, stop_event=None,
        prior_history=None, speed=None, headless=false, browser_port=None,
        session_id=None, single_tab=false, browser_profile=None, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        provider: String,
        model: String,
        save_conversation: bool,
        frontend_callback: Option<Py<PyAny>>,
        text_callback: Option<Py<PyAny>>,
        web_callback: Option<Py<PyAny>>,
        shell_callback: Option<Py<PyAny>>,
        cli_callback: Option<Py<PyAny>>,
        tool_callback: Option<Py<PyAny>>,
        token_callback: Option<Py<PyAny>>,
        api_key: Option<Py<PyAny>>,
        stop_event: Option<Py<PyAny>>,
        prior_history: Option<Bound<'_, PyAny>>,
        speed: Option<Bound<'_, PyAny>>,
        headless: bool,
        browser_port: Option<Bound<'_, PyAny>>,
        session_id: Option<String>,
        single_tab: bool,
        browser_profile: Option<String>,
        _extra: Option<Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let _ = cli_callback;
        let agent_dir = crate::agent_dir(py)?;

        // Parallel runs hand each agent a session id (scopes the shared
        // scratchpad) and single_tab=true (one dedicated tab, no tab tools).
        let session_id = session_id.filter(|s| !s.trim().is_empty());

        // Clean up scratchpad for a fresh start
        cleanup_scratchpad(&agent_dir, session_id.as_deref())?;

        // Speed mode: "fast" swaps in fast_system_prompt.md AND the `memory`
        // notes tool (next_goal alone) for the `reasoning` one. The two halves
        // move together or not at all: without the fast prompt the WHOLE mode
        // falls back to quality rather than run that contradiction.
        let speed_str = match &speed {
            Some(v) if !v.is_none() => v.str()?.extract::<String>()?.to_lowercase(),
            _ => "quality".to_string(),
        };
        let requested = if speed_str == "fast" { "fast" } else { "quality" };
        let mut speed_final = requested.to_string();
        if requested == "fast"
            && !agent_dir.join("main_driver").join("fast_system_prompt.md").exists()
        {
            println!(
                "speed=\"fast\" needs fast_system_prompt.md, which the browser agent \
                 does not have yet - running in quality mode \
                 (thinking + memory + next_goal)."
            );
            speed_final = "quality".to_string();
        }

        // LLM manager — Rust, same crate. It stays a pyclass because the
        // compression controller receives the INSTANCE and the CLASS and
        // builds its own second text-mode manager from them.
        let api_key_str: Option<String> = match &api_key {
            Some(k) => {
                let b = k.bind(py);
                if b.is_none() { None } else { Some(b.str()?.extract::<String>()?) }
            }
            None => None,
        };
        let llm_manager = Py::new(
            py,
            crate::tool_registry::service::LLMManager::build(
                py, &provider, &model, api_key_str, false, "main", &speed_final, single_tab,
            )?,
        )?;

        // The browser. Extension mode (the owner, 2026-10-03: "debug only for the
        // headless, everything else extension based"): a headful run, a parallel run's
        // agents included, drives the person's own Chrome through the AutoCua
        // extension, with no debugging port (bridge.rs take_chrome: a Chrome already
        // running with the extension is used as it is, one running without it is quit
        // and started again with its session, none is started; several agents are each
        // on a line of their own, bridge.rs config.json). The debugging port is for a
        // headless run alone: nothing of the person's to share, a browser of the
        // agent's own on its own profile folder.
        let port_named = match &browser_port {
            Some(v) => !v.is_none() && v.is_truthy()?,
            None => false,
        };
        let browser_port_num: u16 = match &browser_port {
            Some(v) if port_named => v.extract::<i64>()? as u16,
            _ => CHROME_PORT,
        };
        let headless_flag = headless;
        let browser_dir = crate::browser_dir(py)?;
        // Chrome itself, installed now if it is missing (utils/chrome.py).
        crate::ensure_chrome(py);
        let scanner = if !headless_flag {
            // A named profile (tests) is a user data folder of the agent's own; none
            // names the person's Chrome. Resolved on the PYTHON side: AutoCua/__init__.py
            // alone knows where AutoCua_data lives.
            let user_data = match browser_profile.as_deref() {
                Some(name) => Some(crate::browser_profile_dir(py, Some(name))?),
                None => None,
            };
            let src = browser_dir.join("AutoCuaBridge");
            let bridge = py
                .detach(move || crate::bridge::open_on_chrome(&src, user_data))
                .map_err(PyErr::from)?;
            let scanner = BrowserScanner::create(&browser_dir, 0, frontend_callback, None, single_tab);
            if let Ok(mut inner) = scanner.inner.lock() {
                inner.set_bridge(bridge);
            }
            Py::new(py, scanner)?
        } else {
            // `browser_profile` names a separate user-data-dir (the `web` tool's
            // browser-agent fallback passes "web_fallback") so a headless run never
            // collides with a visible Chrome already holding the default profile: on
            // Windows that collision is silent, Chrome hands the launch to the running
            // instance and the debug port never opens.
            crate::ensure_browser(py, browser_port_num, headless_flag);
            let profile = crate::browser_profile_dir(py, browser_profile.as_deref()).ok();
            let launch_profile = profile.clone();
            py.detach(move || {
                crate::browser::launch_chrome_impl(browser_port_num, headless_flag, launch_profile)
            })
            .map_err(PyErr::from)?;
            let scanner = Py::new(
                py,
                BrowserScanner::create(&browser_dir, browser_port_num, frontend_callback, None, single_tab),
            )?;
            // If the browser goes away mid-run, the scanner starts it again the
            // same way and reopens the agent's tabs (ScannerInner::revive).
            if let Ok(mut inner) = scanner.get().inner.lock() {
                inner.set_relaunch(headless_flag, profile);
            }
            scanner
        };

        // Fast mode draws no cursor: the point of the mode is that every tool
        // fires the moment the previous one has landed, and the cursor's glide
        // and press were the one thing that paced actions by the clock.
        if speed_final == "fast" {
            if let Ok(mut inner) = scanner.get().inner.lock() {
                inner.set_cursor_enabled(false);
            }
        }

        // Controller — pure Rust in the same crate, driving the same scanner.
        // web_callback/shell_callback are accepted for GUI parity only; no
        // current tool uses them.
        let _ = (&web_callback, &shell_callback, &api_key);
        let web_dir = agent_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        let controller = crate::controller::view::ControllerView::new(
            &web_dir,
            session_id.as_deref(),
            single_tab,
            scanner.get().inner.clone(),
            stop_event.as_ref().map(|e| e.clone_ref(py)),
        )
        .map_err(|e| PyIOError::new_err(e.to_string()))?;

        // Load system prompt — __init__ already downgraded the speed, so the
        // prompt and the tool schema are guaranteed to agree here.
        // A Claude model in quality mode runs on its own prompt, with the
        // `agent_memory` notes tool the manager picked for it, as on desktop.
        let prompt_name = if speed_final == "fast" {
            "fast_system_prompt.md"
        } else if llm_manager.get().claude {
            "claude_prompt.md"
        } else {
            "system_prompt.md"
        };
        let prompt_path = agent_dir.join("main_driver").join(prompt_name);
        let mut system_prompt = match std::fs::read_to_string(&prompt_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(PyFileNotFoundError::new_err(format!(
                    "{prompt_name} file not found in the agent directory"
                )));
            }
            Err(e) => {
                return Err(PyException::new_err(format!(
                    "Error loading system prompt: {e}"
                )));
            }
        };
        // One prompt serves every platform, so the OS is filled in here:
        // shortcuts differ (cmd on mac, ctrl elsewhere) and the model has to
        // know which keyboard it is driving. A prompt without the placeholder
        // is left exactly as written.
        if system_prompt.contains(OS_PLACEHOLDER) {
            system_prompt = system_prompt.replace(OS_PLACEHOLDER, current_os());
        }
        if single_tab {
            // Overrides the base prompt's tab guidance without editing the
            // file: this agent has no tab tools and must never plan around
            // them. It never learns that parallel agents exist.
            system_prompt.push_str(
                "\n<single_tab_mode>\n\
                 This session drives ONE dedicated tab for the entire run. \
                 `new_tab`, `switch_tab` and `close_tab` DO NOT EXIST here - \
                 ignore any guidance mentioning them. To reach a different \
                 page, navigate the current tab: `update_tab` (open a url) or \
                 `navigate_tab` (back/forward/reload). <all_tabs> always \
                 shows exactly your one tab.\n\
                 </single_tab_mode>\n",
            );
        }
        if llm_manager.get().async_tools() {
            system_prompt.push_str(ASYNC_TOOLS_ADDENDUM);
        }

        // Clear previous conversation/debug/raw_reasoning folders (CWD-relative).
        for folder in ["conversation", "debug", "raw_reasoning"] {
            let p = PathBuf::from(folder);
            if p.exists() {
                std::fs::remove_dir_all(&p)
                    .map_err(|e| PyIOError::new_err(format!("{folder}: {e}")))?;
            }
        }

        if save_conversation {
            std::fs::create_dir_all("conversation")
                .map_err(|e| PyIOError::new_err(format!("conversation: {e}")))?;
            // Initialize the conversation file with header information.
            let init = (|| -> PyResult<()> {
                let mut head = String::new();
                head.push_str("=== CONVERSATION LOG ===\n");
                head.push_str(&format!("Session Started: {}\n", now_stamp()));
                head.push_str(&format!("Provider: {}\n", llm_manager.get().provider));
                head.push_str(&format!("Model: {}\n", llm_manager.get().model_short_name));
                head.push_str(&"=".repeat(60));
                head.push_str("\n\n=== SYSTEM PROMPT ===\n");
                head.push_str(&system_prompt);
                head.push_str("\n\n");
                head.push_str(&"=".repeat(60));
                head.push_str("\n\n");
                std::fs::write(PathBuf::from("conversation").join("conversation.txt"), head)
                    .map_err(|e| PyIOError::new_err(e.to_string()))
            })();
            if let Err(e) = init {
                println!("Error initializing conversation file: {}", py_err_str(py, &e));
            }
            std::fs::create_dir_all("raw_reasoning")
                .map_err(|e| PyIOError::new_err(format!("raw_reasoning: {e}")))?;
        }

        // Resumable chat memory (UI path only) — only a dict counts.
        let prior = match &prior_history {
            Some(v) if v.downcast::<PyDict>().is_ok() => Some(v.clone().unbind()),
            _ => None,
        };

        // Runtime memory compression (rolling handoff): the dump/entry hooks
        // are this driver's OWN — the pyfunctions exported by this module.
        let own_mod = py
            .import("AutoCua.web.agent_native")
            .or_else(|_| py.import("agent_native"))?;
        let compression_cls =
            py.import("AutoCua.memory_compression.controller")?
                .getattr("CompressionController")?;
        let comp_kwargs = PyDict::new(py);
        comp_kwargs.set_item("dump_builder", own_mod.getattr("compression_dump")?)?;
        comp_kwargs.set_item("synthetic_entry", own_mod.getattr("compression_entry")?)?;
        let compression = compression_cls.call(
            (
                llm_manager.clone_ref(py),
                py.get_type::<crate::tool_registry::service::LLMManager>(),
                match &token_callback {
                    Some(c) => c.clone_ref(py).into_any(),
                    None => py.None(),
                },
                match &stop_event {
                    Some(e) => e.clone_ref(py).into_any(),
                    None => py.None(),
                },
            ),
            Some(&comp_kwargs),
        )?;

        Ok(AgentService {
            speed: speed_final,
            headless,
            save_conversation,
            browser_port: browser_port_num,
            system_prompt,
            llm_manager,
            stop_event,
            scanner,
            controller,
            compression: compression.unbind(),
            text_callback,
            tool_callback,
            token_callback,
            prior_history: prior,
            state: Mutex::new(RunState {
                scrape_mode: false,
                scrape_start: None,
                scrape_shots: Vec::new(),
                assistant_messages: PyList::empty(py).into_any().unbind(),
                tool_responses: PyList::empty(py).into_any().unbind(),
                last_messages: py.None(),
            }),
        })
    }

    #[getter]
    fn assistant_messages(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().assistant_messages.clone_ref(py)
    }

    #[getter]
    fn tool_responses(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().tool_responses.clone_ref(py)
    }

    #[getter]
    fn last_messages(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().last_messages.clone_ref(py)
    }

    #[getter]
    fn speed(&self) -> &str {
        &self.speed
    }

    #[getter]
    fn headless(&self) -> bool {
        self.headless
    }

    #[getter]
    fn save_conversation(&self) -> bool {
        self.save_conversation
    }

    #[getter]
    fn browser_port(&self) -> u16 {
        self.browser_port
    }

    #[getter]
    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Process a user request in an iterative loop until completion.
    /// Returns {"status", "message"}: "success" (a `done` action ran),
    /// "error" (a critical exception ended the loop), or "incomplete".
    fn process_request<'py>(
        &self,
        py: Python<'py>,
        task: Bound<'py, PyAny>,
    ) -> PyResult<Py<PyDict>> {
        let task: String = task.str()?.extract()?;
        // A run's scrapes are numbered from 1 and stored in a folder of their own.
        crate::controller::run_script::service::window::new_run();
        if let Some(web) = crate::web_dir(py).ok().and_then(|d| d.parent().map(|p| p.join("default_skills").join("web"))) {
            crate::controller::skills::service::new_run(&web.join("skills"), &web.join("domain_skill"));
        }

        let mut step_number: i64 = 0;
        let mut is_first_iteration = true;
        let mut am = PyList::empty(py); // assistant turns, aligned 1:1 with...
        let mut tr = PyList::empty(py); // ...per-step tool results
        let mut json_fail_count = 0i32;
        // Consecutive `reasoning`-only steps (a reminder rides on the third).
        let mut reasoning_only_count = 0i32;
        let mut final_status = "incomplete".to_string();
        let mut final_message = "Agent stopped before completing the task".to_string();

        // Runtime compression: new call invalidates any stale worker.
        self.compression.bind(py).call_method0("reset")?;

        // ---- Resumable memory seed (UI continuation) ------------------------
        let mut is_resumed = false;
        let mut original_task = task.clone();
        if let Some(prior) = &self.prior_history {
            let prior = prior.bind(py);
            let seeded = prior.call_method1("get", ("assistant_messages",))?;
            if seeded.is_truthy()? {
                let seeded_list = PyList::empty(py);
                for item in seeded.try_iter()? {
                    seeded_list.append(item?)?;
                }
                am = seeded_list;
                let prior_tools = prior.call_method1("get", ("tool_responses",))?;
                let tools_list = PyList::empty(py);
                if prior_tools.is_truthy()? {
                    for (i, item) in prior_tools.try_iter()?.enumerate() {
                        if i >= am.len() {
                            break;
                        }
                        tools_list.append(item?)?;
                    }
                }
                while tools_list.len() < am.len() {
                    tools_list.append(py.None())?;
                }
                tr = tools_list;
                is_resumed = true;
                is_first_iteration = false; // continuation, not a fresh dialogue
                let prior_task = prior.call_method1("get", ("task",))?;
                if prior_task.is_truthy()? {
                    original_task = prior_task.str()?.extract()?;
                }
                // Label this run boundary: fill the terminal bridge's empty
                // tool slot with a numbered request marker.
                let last_slot_none = tr.get_item(tr.len() - 1)?.is_none();
                let last_entry: String = am.get_item(am.len() - 1)?.extract()?;
                if last_slot_none && last_entry.contains(BRIDGE_SIGNATURE) {
                    let mut n_bridges: i64 = 0;
                    for item in am.iter() {
                        let s: String = item.str()?.extract()?;
                        if s.contains(BRIDGE_SIGNATURE) {
                            n_bridges += 1;
                        }
                    }
                    let mut max_no = n_bridges;
                    for item in tr.iter() {
                        if let Ok(s) = item.downcast::<PyString>() {
                            let text: String = s.extract()?;
                            for cap in re_marker_no().captures_iter(&text) {
                                if let Ok(n) = cap[1].parse::<i64>() {
                                    max_no = max_no.max(n);
                                }
                            }
                        }
                    }
                    tr.set_item(tr.len() - 1, request_marker(max_no + 1, &task))?;
                }
                println!("Resumed memory: {} prior step(s) loaded", am.len());
            }
        }
        // ---------------------------------------------------------------------

        println!("\nProcessing with {}", self.model_name());
        self.emit_flow(py, "run_start", None); // clear + take over the demo

        // Main agent loop
        loop {
            // Check for stop signal
            if self.stop_set(py)? {
                println!("\nAgent stopped by user.");
                self.release_all_inputs()?;
                final_status = "incomplete".into();
                final_message = "Stopped by user".into();
                break;
            }

            // Apply a finished background compression: the controller splices
            // the lists IN PLACE here on the main thread. The third argument
            // was the research-digest memory index; the feature is gone, so
            // None goes in and the shifted return is ignored.
            self.compression
                .bind(py)
                .call_method1("apply_pending", (&am, &tr, py.None()))?;

            step_number += 1;

            // Max step limit - prevent infinite loops
            if step_number > 100 {
                println!("\nMax step limit reached (100). Exiting agent loop.");
                final_status = "incomplete".into();
                final_message = "Reached the 100-step limit without finishing".into();
                break;
            }

            // Scrape mode (controller/run_script): the records attached to the
            // last run_script scrape or `more` are the input, the page is not
            // read on top of them, and the schema holds the tools of the job.
            let scrape_mode = self
                .state
                .lock()
                .map(|st| st.scrape_mode)
                .unwrap_or(false);
            self.llm_manager.get().set_scrape_step(scrape_mode);

            // Tool-flow chain: announce the turn.
            self.emit_flow(py, "turn", Some(&json!({"hasImage": !scrape_mode})));

            // Assigned inside the block below on every path that survives it
            // (a scan failure or a Stop breaks the loop instead), which the
            // compiler checks — a dead default here would hide a path that
            // forgot to.
            let annotated_image_base64: Option<String>;
            let image_sent: bool;
            let formatted_element_tree: String;
            let all_tabs_text: String;

            if scrape_mode {
                println!("\n{}", "=".repeat(60));
                println!("Step {step_number}: scraping mode - no scan, no picture.");
                if self.stop_set(py)? {
                    self.release_all_inputs()?;
                    final_status = "incomplete".into();
                    final_message = "Stopped by user".into();
                    break;
                }
                let scanner = self.scanner.get();
                // No scan this step to carry the overlay or the app's picture:
                // the scrape look goes back on the page being read (a page that
                // reloaded comes back without it), and the app is handed that
                // page as it is now. Neither reaches the model.
                let _ = scanner.scrape_glow_sync(py);
                let _ = scanner.scrape_frame(py);
                self.llm_manager.get().set_dialog_step(false);
                annotated_image_base64 = None;
                image_sent = false;
                all_tabs_text = scanner.tabs(py)?;
                formatted_element_tree = scrape_mode_block();
            } else {
                println!("\n{}", "=".repeat(60));
                println!("Step {step_number}: Scanning snapshot.");
                let scanner = self.scanner.get();
                // A scan failure must not escape `process_request`. As a bare
                // `?` this returned straight out of the function, skipping
                // stop(), emit_flow("run_end"), compression.finish_run() and
                // the result dict — so the caller got an exception and NO
                // result at all, and agent_launcher has no `except` to catch
                // it. The triggers are ordinary: a screenshot timing out on a
                // self-reloading page, or a tab whose renderer will not answer.
                // Handled the same way a failing step is, so the cleanup tail
                // runs and the caller is told what happened.
                if let Err(e) = scanner.scan_elements(py) {
                    // KeyboardInterrupt / SystemExit are BaseException — let
                    // them fly, exactly as the step guard below does.
                    if !e.is_instance_of::<PyException>(py) {
                        // Past the cleanup tail: the scrape look comes off
                        // here, as stop() would have taken it off.
                        let _ = scanner.scrape_glow_off(py);
                        return Err(e);
                    }
                    let msg = py_err_str(py, &e);
                    println!("Error processing request: {msg}");
                    final_status = "error".into();
                    final_message = format!("could not read the page: {msg}");
                    break;
                }

                // Check Stop AFTER Scan
                if self.stop_set(py)? {
                    self.release_all_inputs()?;
                    final_status = "incomplete".into();
                    final_message = "Stopped by user".into();
                    break;
                }

                let (element_tree_text, img, tabs) = scanner.get_scan_data(py)?;
                // A popup (a JavaScript alert, confirm or prompt) holds the page: no tree and
                // no picture this step, the popup in their place, and the `dialog`
                // tool on offer to answer it (controller/dialog).
                let popup = scanner.dialog_view(py)?;
                self.llm_manager.get().set_dialog_step(popup.is_some());
                // Fast mode sends no screenshot: the model works from the
                // tree alone (fast_system_prompt.md) and every request is
                // hundreds of KB lighter. The capture itself is unchanged;
                // only the model stops receiving it.
                annotated_image_base64 = if self.speed == "fast" { None } else { img };
                all_tabs_text = tabs;

                let took = format!(
                    "total time - {:.2}s",
                    scanner.last_scan_seconds(py)?
                );
                image_sent = annotated_image_base64.is_some();
                if let Some(b64) = &annotated_image_base64 {
                    println!("Image captured - annotated: {} chars ({took})", b64.len());
                } else if popup.is_some() {
                    println!("Popup open - shown in place of the scan ({took})");
                } else if self.speed == "fast" {
                    println!("Tree only - fast mode sends no screenshot ({took})");
                } else {
                    println!("NO IMAGE - annotated image is None! ({took})");
                }

                formatted_element_tree = match &popup {
                    Some(view) => format!("<dialog>\n{view}\n</dialog>"),
                    None => format!("<element_tree>\n{element_tree_text}\n</element_tree>"),
                };
            }

            // -- Live context, rebuilt fresh EVERY step ------------------------
            let todo_list = self.read_todo_from_file();
            let scratchpad_content = self.read_scratchpad_from_file();
            let (skills, domain) = skills_blocks(py, self.scanner.get());
            let state_block = [
                persistent_memory(&todo_list, &scratchpad_content, &skills, &domain),
            ]
            .into_iter()
            .filter(|b| !b.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

            let page_block = [
                all_tabs_block(&all_tabs_text),
                formatted_element_tree.clone(),
            ]
            .into_iter()
            .filter(|b| !b.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");

            // Construct user message based on iteration
            let mut user_message: String;
            if is_first_iteration {
                user_message = format!(
                    "<user_request>\n{task}\n</user_request>\n\n<browser_mode>\n{}\n</browser_mode>\n\n{state_block}\n\n{page_block}",
                    if self.headless { "headless" } else { "headful" }
                );
            } else {
                let request_tag = if is_resumed { "updated_user_request" } else { "user_request" };
                let base = format!("<{request_tag}>\n{task}\n</{request_tag}>");
                let mut msg = format!("{base}\n\n{state_block}\n\n{page_block}");
                if image_sent {
                    msg.push_str("\n\n<image>Annotated screenshot with bounding boxes</image>");
                    msg.push_str("\n\n<critical>Pure vision first: decide which element to interact with from the screenshot alone, then refer to <element_tree> for its [id].</critical>");
                }
                user_message = msg;
            }
            // Claude: the notes call and the action calls belong in ONE
            // response (see CLAUDE_BATCH_REMINDER). Last in the live message.
            if self.speed != "fast" && self.llm_manager.get().claude && !scrape_mode {
                user_message.push_str(CLAUDE_BATCH_REMINDER);
            }

            // Build the API messages as a real NATIVE dialogue. APPEND-ONLY:
            // nothing already in the transcript is ever rewritten, so the
            // prompt-cache prefix stays valid across steps.
            let messages = PyList::empty(py);
            let sys_msg = PyDict::new(py);
            sys_msg.set_item("role", "system")?;
            sys_msg.set_item("content", &self.system_prompt)?;
            messages.append(sys_msg)?;

            if !am.is_empty() {
                let opening = PyDict::new(py);
                opening.set_item("role", "user")?;
                opening.set_item("content", request_marker(1, &original_task))?;
                messages.append(opening)?;
            }

            let n_hist = am.len();
            for i in 0..n_hist {
                let is_recent = i == n_hist - 1;
                let step_msg: String = am.get_item(i)?.str()?.extract()?;
                let step = view::decode_step(&step_msg);
                let has_calls = matches!(&step.tool_calls, Value::Array(a) if !a.is_empty());
                let native = has_calls || view::looks_native(&step_msg);
                let asst = PyDict::new(py);
                asst.set_item("role", "assistant")?;
                if native {
                    asst.set_item("content", &step.content)?;
                    if has_calls {
                        asst.set_item("tool_calls", pythonize::pythonize(py, &step.tool_calls)?)?;
                    }
                    if truthy(&step.meta) {
                        // The provider's OWN metadata for this turn — the
                        // provider translates it back into its dialect.
                        asst.set_item("provider_meta", pythonize::pythonize(py, &step.meta)?)?;
                    }
                } else {
                    // Legacy step: keep the old trim for older entries.
                    let content = if is_recent {
                        step_msg.clone()
                    } else {
                        trim_history_entry(&step_msg)
                    };
                    asst.set_item("content", content)?;
                }
                messages.append(asst)?;

                if i < tr.len() {
                    let slot = tr.get_item(i)?;
                    if slot.is_truthy()? {
                        let slot_str: String = slot.str()?.extract()?;
                        for result in view::decode_results(&slot_str) {
                            messages.append(pythonize::pythonize(py, &result)?)?;
                        }
                    }
                }
            }

            let live = PyDict::new(py);
            live.set_item("role", "user")?;
            live.set_item("content", &user_message)?;
            messages.append(live)?;

            // Capture the exact payload for the debug memory log.
            self.state.lock().unwrap().last_messages = messages.clone().into_any().unbind();

            // Prompt caching (OpenRouter/Anthropic/AWS): mark the newest
            // PERSISTENT turn - never the live user message, never an
            // assistant turn.
            let provider_name = self.provider_name();
            if (provider_name == "openrouter" || provider_name == "anthropic" || provider_name == "aws")
                && messages.len() > 2
            {
                for i in (0..messages.len()).rev() {
                    if i == messages.len() - 1 {
                        continue; // skip the ephemeral live user message
                    }
                    let cache_msg = messages.get_item(i)?;
                    let role: String = match cache_msg.call_method1("get", ("role",)) {
                        Ok(r) if !r.is_none() => r.str()?.extract()?,
                        _ => String::new(),
                    };
                    if role != "tool" && role != "user" {
                        continue;
                    }
                    let content = cache_msg.call_method1("get", ("content",))?;
                    if let Ok(s) = content.downcast::<PyString>() {
                        let text: String = s.extract()?;
                        if !text.is_empty() {
                            let block = pythonize::pythonize(
                                py,
                                &json!([{
                                    "type": "text",
                                    "text": text,
                                    "cache_control": {"type": "ephemeral"}
                                }]),
                            )?;
                            cache_msg.set_item("content", block)?;
                        }
                    }
                    break;
                }
            }

            // ---- The guarded section (Python's outer try) --------------------
            let mut step_ctx: Option<StepCtx> = None;
            let outcome = self.run_step(
                py,
                &task,
                &original_task,
                &messages,
                &am,
                &tr,
                &mut step_number,
                &mut is_first_iteration,
                &mut json_fail_count,
                &mut reasoning_only_count,
                image_sent,
                annotated_image_base64.as_deref(),
                &mut step_ctx,
                &mut final_status,
                &mut final_message,
            );

            match outcome {
                Ok(Flow::Continue) => continue,
                Ok(Flow::Break) => break,
                Err(e) => {
                    // KeyboardInterrupt / SystemExit are BaseException, not
                    // Exception — Python's `except Exception` lets them fly.
                    if !e.is_instance_of::<PyException>(py) {
                        // Past the cleanup tail: a Ctrl+C in scraping mode
                        // takes the scrape look off here, as stop() would.
                        let _ = self.scanner.get().scrape_glow_off(py);
                        return Err(e);
                    }
                    let msg = py_err_str(py, &e);
                    println!("Error processing request: {msg}");
                    // Same pairing guarantee for a failure ANYWHERE in the
                    // step (the NameError guard in Python = ctx presence here).
                    if let Some(ctx) = &step_ctx {
                        close_pairing(&tr, &ctx.calls, &ctx.note_results, &ctx.reject_results, &format!("error: {msg}"))?;
                    }
                    final_status = "error".into();
                    final_message = msg;
                    break;
                }
            }
        }

        // Cleanup: close the scanner subprocess. Chrome deliberately stays up.
        let _ = self.scanner.get().stop(py);

        if final_status == "error" {
            self.emit_flow(py, "error", Some(&json!({"text": "llm service not responding"})));
        } else if final_status == "incomplete" && final_message.to_lowercase().contains("stop") {
            self.emit_flow(py, "error", Some(&json!({"text": "agent interrupted"})));
        }
        self.emit_flow(py, "run_end", None);

        // Compression still pending at run end — result discarded; make sure
        // the indicator stops.
        self.compression.bind(py).call_method0("finish_run")?;

        {
            let mut state = self.state.lock().unwrap();
            state.assistant_messages = am.into_any().unbind();
            state.tool_responses = tr.into_any().unbind();
        }

        let out = PyDict::new(py);
        out.set_item("status", final_status)?;
        out.set_item("message", final_message)?;
        Ok(out.unbind())
    }
}

impl AgentService {
    /// One step's guarded section — everything inside Python's outer `try`.
    #[allow(clippy::too_many_arguments)]
    fn run_step<'py>(
        &self,
        py: Python<'py>,
        task: &str,
        original_task: &str,
        messages: &Bound<'py, PyList>,
        am: &Bound<'py, PyList>,
        tr: &Bound<'py, PyList>,
        step_number: &mut i64,
        is_first_iteration: &mut bool,
        json_fail_count: &mut i32,
        reasoning_only_count: &mut i32,
        image_sent: bool,
        annotated_image_base64: Option<&str>,
        step_ctx: &mut Option<StepCtx>,
        final_status: &mut String,
        final_message: &mut String,
    ) -> PyResult<Flow> {
        let _ = task;
        if image_sent {
            println!("Screenshot sent to LLM (annotated)");
        }

        // Check Stop BEFORE LLM
        if self.stop_set(py)? {
            *final_status = "incomplete".into();
            *final_message = "Stopped by user".into();
            return Ok(Flow::Break);
        }

        // Get raw response from LLM - pass annotated image. The HTTP round
        // trip runs without the GIL, like Python's requests did.
        let msgs: Vec<Value> = match pythonize::depythonize(messages.as_any())? {
            Value::Array(a) => a,
            other => vec![other],
        };
        let core = self.llm_manager.get();
        // The screens the last scrape read covered go with this input, once,
        // after the page's own screenshot (run_script window.rs). Fast mode
        // sends no picture at all.
        let scrape_shots: Vec<String> = self
            .state
            .lock()
            .map(|mut st| std::mem::take(&mut st.scrape_shots))
            .unwrap_or_default();
        let mut shots: Vec<String> = annotated_image_base64.map(str::to_string).into_iter().collect();
        if self.speed != "fast" && !scrape_shots.is_empty() {
            println!("{} screenshot(s) of the scrape read sent to LLM", scrape_shots.len());
            shots.extend(scrape_shots);
        }
        let outcome = py
            .detach(|| core.send_rust(&msgs, &shots))
            .map_err(PyErr::from)?;
        let raw_response: Value = match outcome {
            crate::tool_registry::service::SendOutcome::Native(v) => v,
            crate::tool_registry::service::SendOutcome::Text(s) => Value::String(s),
        };

        // Memory bar: forward this call's exact token usage.
        if let Some(cb) = &self.token_callback {
            let usage = pythonize::pythonize(py, &core.last_usage_value())?;
            let _ = cb.call1(py, (usage,));
        }

        // CRITICAL Check Stop AFTER LLM (discards result if stopped while waiting)
        if self.stop_set(py)? {
            println!("\nAgent stopped by user (response discarded).");
            self.release_all_inputs()?;
            *final_status = "incomplete".into();
            *final_message = "Stopped by user".into();
            return Ok(Flow::Break);
        }

        // Runtime compression trigger: context size is fresh in last_usage.
        self.compression
            .bind(py)
            .call_method1("maybe_trigger", (am, tr, original_task))?;

        println!("LLM response received");

        // Save raw response before any parsing (for debugging).
        if self.save_conversation {
            let raw_text = match &raw_response {
                Value::Object(_) => view::dumps_pretty(&raw_response),
                other => view::py_str_of(other),
            };
            self.save_raw_response(&raw_text, *step_number);
        }

        // NATIVE TOOL CALLING: convert the model's own calls into the same
        // [{type, ...}] action dicts route_action has always consumed.
        // Where this mode's notes live. Quality: on the `reasoning` tool
        // (thinking optional, memory + next_goal every step), action tools
        // carry only their own fields. Fast: `memory` alone, on every call,
        // no `reasoning` tool - unchanged.
        // A Claude model in quality mode: `agent_memory`, the same notes with
        // `assessment` in place of `thinking`.
        let fast = self.speed == "fast";
        let claude = !fast && self.llm_manager.get().claude;
        // Scraping mode has no notes tool and a short registry of its own.
        let in_scrape_mode = self.state.lock().map(|st| st.scrape_mode).unwrap_or(false);
        let track_params: &[(String, String)] = if in_scrape_mode {
            &[]
        } else if fast {
            crate::tool_registry::service::main_memory_fields_fast()
        } else if claude {
            crate::tool_registry::service::claude_reasoning_fields()
        } else {
            crate::tool_registry::service::main_reasoning_fields()
        };
        let names = if in_scrape_mode {
            crate::tool_registry::service::scrape_mode_names()
        } else if fast {
            crate::tool_registry::service::main_tool_names_fast()
        } else if claude {
            crate::tool_registry::service::main_tool_names_claude()
        } else {
            crate::tool_registry::service::main_tool_names()
        };
        let (actions_val, calls, rejects, track_vec) =
            crate::tool_registry::service::tool_calls_to_steps(
                raw_response.get("tool_calls").unwrap_or(&Value::Null),
                track_params,
                names,
            );
        let track: Map<String, Value> = track_vec
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let resp_text: String = raw_response
            .get("text")
            .filter(|v| truthy(v))
            .map(|v| view::py_str_of(v).trim().to_string())
            .unwrap_or_default();
        let provider_meta: Option<Value> = raw_response.get("provider_meta").cloned();

        // The notes call (`reasoning` in quality mode, `memory` in fast) is
        // the step's notes, not an action: pull it out before routing (the controller has no handler
        // for it) but keep it in `replay_calls`, which is what the assistant
        // turn replays - and what Gemini's thought signature binds to. Each
        // note call is answered with the bare REASONING_RESULT; the snapshot
        // and the compression dump hide it.
        let notes_tool = if in_scrape_mode {
            ""
        } else if fast {
            "memory"
        } else if claude {
            "agent_memory"
        } else {
            "reasoning"
        };
        let is_note = |a: &Value| a.get("type").and_then(Value::as_str) == Some(notes_tool);
        let note_ids: Vec<Value> = actions_val
            .iter()
            .zip(calls.iter())
            .filter(|(a, _)| is_note(a))
            .map(|(_, c)| c["id"].clone())
            .collect();
        let mut note_results: Vec<Value> = note_ids
            .iter()
            .map(|id| json!({"tool_call_id": id, "content": view::REASONING_RESULT}))
            .collect();
        let replay_calls: Vec<Value> = calls.clone();
        let (actions_val, calls): (Vec<Value>, Vec<Value>) = actions_val
            .into_iter()
            .zip(calls)
            .filter(|(a, _)| !is_note(a))
            .unzip();

        // Backstop for an `async_tools` model (Astra): with every tool async,
        // nothing stops it from bundling `done` after this turn's actions,
        // whose results it has not seen. Such a `done` is dropped from the
        // batch and answered with an error result: the actions run, the
        // replay stays complete (the call is already in replay_calls) and
        // the model calls `done` alone once it has verified them. No other
        // model enters this branch.
        let mut extra_results: Vec<Value> = Vec::new();
        let is_done = |a: &Value| a.get("type").and_then(Value::as_str) == Some("done");
        let (actions_val, calls): (Vec<Value>, Vec<Value>) = if self.llm_manager.get().async_tools()
            && actions_val.len() + rejects.len() > 1
            && actions_val.iter().any(|a| is_done(a))
        {
            let mut kept_actions = Vec::new();
            let mut kept_calls = Vec::new();
            for (a, c) in actions_val.into_iter().zip(calls) {
                if is_done(&a) {
                    println!("`done` bundled with other actions: ignored, it must be the only call of its turn");
                    extra_results.push(json!({
                        "tool_call_id": c["id"].clone(),
                        "content": "error: done ignored, it must be the only call of its turn. This turn's other actions ran; verify them on the next input, then call done alone.",
                    }));
                } else {
                    kept_actions.push(a);
                    kept_calls.push(c);
                }
            }
            (kept_actions, kept_calls)
        } else {
            (actions_val, calls)
        };

        if actions_val.is_empty() && rejects.is_empty() && !note_ids.is_empty() {
            // A reasoning-only step: the model chose to think before acting.
            // Accepted as a normal step (it costs a fresh scan, so only a RUN
            // of them is bounded: from the third in a row the result carries
            // a reminder).
            *reasoning_only_count += 1;
            // An `async_tools` model that never acts is the route having
            // dropped the async flags (one call per turn again): the
            // reminder cannot help it, so the run stops at six instead of
            // scanning to the step cap.
            if self.llm_manager.get().async_tools() && *reasoning_only_count >= 6 {
                *final_status = "incomplete".into();
                *final_message = format!(
                    "{} steps in a row without an action on an async-tools model: the route dropped the async tool flags (see the raw responses), so the model can only emit one call per turn.",
                    reasoning_only_count
                );
                return Ok(Flow::Break);
            }
            if *reasoning_only_count >= 3 {
                let reminder = view::dumps_pretty(&json!({
                    "status": "success",
                    "action": notes_tool,
                    "message": format!(
                        "{} steps in a row without an action: call a tool that does the work in this turn.",
                        reasoning_only_count
                    ),
                }));
                note_results = note_ids
                    .iter()
                    .map(|id| json!({"tool_call_id": id, "content": reminder}))
                    .collect();
            }
            let wired = view::wire_calls_from(&replay_calls)
                .map_err(|k| pyo3::exceptions::PyKeyError::new_err(k.to_string()))?;
            let normalized =
                view::encode_step(&resp_text, &Value::Array(wired), provider_meta.as_ref());
            // Only the fields this mode carries (fast has next_goal alone).
            let mut display = Map::new();
            let mut thinking: Option<String> = None;
            for key in ["thinking", "assessment"] {
                if let Some(t) = track.get(key) {
                    let value = if truthy(t) {
                        py_str_of_value(t)
                    } else if !resp_text.is_empty() {
                        resp_text.clone()
                    } else {
                        view::SKIPPED_THINKING.to_string()
                    };
                    thinking = Some(value.clone());
                    display.insert(key.into(), Value::String(value));
                }
            }
            if let Some(m) = track.get("memory") {
                display.insert("memory".into(), m.clone());
            }
            let next_goal = track.get("next_goal").cloned().unwrap_or_else(|| json!(""));
            if track.contains_key("next_goal") {
                display.insert("next_goal".into(), next_goal.clone());
            }
            display.insert("action".into(), json!([]));
            let display_payload = view::dumps_pretty(&Value::Object(display));
            // The cursor bubble: this is the step where the thinking is the
            // point, so show it; fall back to the plain "Doing" line.
            let say = match &thinking {
                Some(t) if t != view::SKIPPED_THINKING => t.clone(),
                _ => py_str_of_value(&next_goal).trim().to_string(),
            };
            if !say.is_empty() {
                let _ = self.scanner.get().cursor_say(py, &say);
            }
            self.save_conversation_files(py, messages, &display_payload, image_sent, *step_number);
            am.append(&normalized)?;
            tr.append(view::encode_results(&note_results))?;
            println!("{}", view::format_response_impl(&display_payload, true));
            if let Some(cb) = &self.text_callback {
                cb.call1(py, (view::format_response_impl(&display_payload, false),))?;
            }
            self.emit_flow(py, "received", Some(&json!({"tools": []})));
            *is_first_iteration = false;
            return Ok(Flow::Continue);
        }

        if actions_val.is_empty() && rejects.is_empty() {
            // REPAIR, not exit: a message with NO tool calls is always a
            // dropped-calls mistake. Keep the turn, answer it with a
            // corrective result turn, and let the model continue.
            *json_fail_count += 1;
            if !resp_text.is_empty() {
                println!(
                    "message had no tool calls - asking the model to act ({}/3)",
                    json_fail_count
                );
                if *json_fail_count >= 3 {
                    *final_status = "incomplete".into();
                    *final_message =
                        "Model kept responding without calling any tool (3 consecutive turns)"
                            .into();
                    return Ok(Flow::Break);
                }
                let normalized =
                    view::encode_step(&resp_text, &json!([]), provider_meta.as_ref());
                // snapshot shows the prose the model wrote, not the codec JSON
                self.save_conversation_files(py, messages, &resp_text, image_sent, *step_number);
                am.append(&normalized)?;
                tr.append(format!(
                    "<tool_response>\n{}\n</tool_response>",
                    view::dumps_pretty(&json!({"message": "no tool called"}))
                ))?;
                *is_first_iteration = false;
                return Ok(Flow::Continue);
            }
            // Genuinely empty turn (no text, no calls) - retry the step.
            println!("empty response from the model - retrying ({}/3)", json_fail_count);
            if *json_fail_count >= 3 {
                *final_status = "incomplete".into();
                *final_message =
                    "Could not get a valid model response (3 consecutive failures)".into();
                return Ok(Flow::Break);
            }
            *step_number -= 1;
            return Ok(Flow::Continue);
        }

        // Reset the strike counter only on a turn that actually ROUTED
        // something — a turn made entirely of rejects reaches here too.
        if !actions_val.is_empty() {
            *json_fail_count = 0;
            *reasoning_only_count = 0;
        }

        // The assistant turn EXACTLY as the model produced it — the
        // `reasoning` call and REJECTED calls included, so every result
        // written below pairs with a call.
        let mut all_calls = replay_calls.clone();
        all_calls.extend(rejects.iter().cloned());
        let wired = view::wire_calls_from(&all_calls)
            .map_err(|k| pyo3::exceptions::PyKeyError::new_err(k.to_string()))?;
        let normalized =
            view::encode_step(&resp_text, &Value::Array(wired), provider_meta.as_ref());

        // Display payload — the four-block view the terminal print, the
        // frontend text stream and the tool-icon chain read.
        let mut display = Map::new();
        for key in ["thinking", "assessment"] {
            if let Some(t) = track.get(key) {
                let value = if truthy(t) {
                    t.clone()
                } else if !resp_text.is_empty() {
                    Value::String(resp_text.clone())
                } else {
                    Value::String(view::SKIPPED_THINKING.to_string())
                };
                display.insert(key.into(), value);
            }
        }
        if let Some(m) = track.get("memory") {
            display.insert("memory".into(), m.clone());
        }
        if let Some(g) = track.get("next_goal") {
            display.insert("next_goal".into(), g.clone());
        }
        display.insert("action".into(), Value::Array(actions_val.clone()));

        // Tell the browser's cursor what this step is about, so a human
        // watching the page sees the reasoning rather than just the clicks.
        //
        // `thinking` is the real article but it is only filled on FULL steps —
        // a route step omits it and the display shows "skipped", which is most
        // steps. `next_goal` carries a plain "Doing: ..." line on every single
        // step, so it is what the bubble falls back to. Without the fallback
        // the bubble would be silent for long stretches of a run that is very
        // much still happening.
        let say = {
            let block = |k: &str| -> String {
                display
                    .get(k)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };
            let thinking = match block("thinking") {
                t if t.is_empty() => block("assessment"),
                t => t,
            };
            if !thinking.is_empty() && thinking != view::SKIPPED_THINKING {
                thinking
            } else {
                block("next_goal")
            }
        };
        if !say.is_empty() {
            // Best-effort, and swallowed on failure: this is commentary on the
            // step, never a reason to fail one.
            let _ = self.scanner.get().cursor_say(py, &say);
        }

        let display_payload = view::dumps_pretty(&Value::Object(display));

        // Save a faithful snapshot of the EXACT payload sent this step.
        self.save_conversation_files(py, messages, &display_payload, image_sent, *step_number);

        // Record the turn with an empty result slot, backfilled after routing.
        am.append(&normalized)?;
        tr.append(py.None())?;

        // A call naming a tool that doesn't exist is answered with an error
        // result instead of being dropped.
        let mut reject_results: Vec<Value> = rejects
            .iter()
            .map(|r| {
                json!({
                    "tool_call_id": r["id"].clone(),
                    "content": format!("error: {}", py_str_of_value(&r["error"])),
                })
            })
            .collect();
        reject_results.extend(extra_results);

        *step_ctx = Some(StepCtx {
            calls: calls.clone(),
            note_results: note_results.clone(),
            reject_results: reject_results.clone(),
        });

        // Format the response with emojis for console output.
        let formatted_response = view::format_response_impl(&display_payload, true);
        println!("{formatted_response}");

        // Send to frontend if callback exists (omit action from stream).
        // Deliberately UNGUARDED, like the Python original: an exception here
        // lands in the outer error handler.
        if let Some(cb) = &self.text_callback {
            cb.call1(py, (view::format_response_impl(&display_payload, false),))?;
        }

        // Tool-flow chain: the packet arrived.
        self.emit_flow(
            py,
            "received",
            Some(&json!({"tools": view::extract_tools_impl(&display_payload)})),
        );

        if !rejects.is_empty() {
            for r in &rejects {
                println!(
                    "invalid tool call '{}' - error returned to the model",
                    py_str_of_value(&r["name"])
                );
            }
            if actions_val.is_empty() {
                // Nothing valid to route — counts as the SAME strike.
                *json_fail_count += 1;
                let mut only_rejects: Vec<Value> = note_results.clone();
                only_rejects.extend(reject_results.iter().cloned());
                tr.set_item(tr.len() - 1, view::encode_results(&only_rejects))?;
                println!("no valid tool call this turn ({}/3)", json_fail_count);
                if *json_fail_count >= 3 {
                    *final_status = "incomplete".into();
                    *final_message =
                        "Model kept sending invalid tool calls (3 consecutive turns)".into();
                    return Ok(Flow::Break);
                }
                *is_first_iteration = false;
                return Ok(Flow::Continue);
            }
        }

        // ---- Inner try: execute the actions ---------------------------------
        let inner: PyResult<Flow> = (|| {
            // Check Stop BEFORE Action - NOTHING ran this step, so discard
            // the turn instead of saving a result for a call that never ran.
            if self.stop_set(py)? {
                discard_step(am, tr, &normalized)?;
                println!("\nAgent stopped by user (step discarded - no action ran).");
                *final_status = "incomplete".into();
                *final_message = "Stopped by user".into();
                return Ok(Flow::Break);
            }

            println!("\nExecuting action...");

            // Pass elements mapping to controller
            let (elements_mapping, app_name) = {
                let inner = self.scanner.get().inner.clone();
                let guard = inner.lock().map_err(|_| {
                    pyo3::exceptions::PyRuntimeError::new_err(
                        "scanner state poisoned by an earlier panic",
                    )
                })?;
                (guard.mapping.clone(), guard.application_name())
            };
            // In scraping mode there was no scan, so no [id] is valid: the
            // controller gets an empty map and refuses them as it refuses any
            // id that is not in the tree.
            if in_scrape_mode {
                self.controller.set_elements(Map::new(), &app_name);
            } else {
                self.controller.set_elements(elements_mapping, &app_name);
            }

            // Send action to controller.
            let action_result: Value = self
                .controller
                .route_action(py, &Value::Array(actions_val.clone()))?;
            // The pictures a scrape read took this step go with the next
            // input; a step without a read leaves none (run_script window.rs).
            let shots = crate::controller::run_script::service::window::take_shots();
            if let Ok(mut st) = self.state.lock() {
                st.scrape_shots = shots;
            }

            // Check if action was stopped mid-execution — the actions DID
            // partially run, so record what the controller returned.
            if action_result.get("status").and_then(Value::as_str) == Some("stopped") {
                println!("\nAgent stopped by user (action interrupted).");
                pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;
                *final_status = "incomplete".into();
                *final_message = "Stopped by user".into();
                return Ok(Flow::Break);
            }

            // Check if task completed (done action was executed)
            if action_result.get("action").and_then(Value::as_str) == Some("done") {
                let summary = action_result
                    .get("summary")
                    .map(py_str_of_value)
                    .unwrap_or_else(|| "Task completed".to_string());
                println!("\nTask Complete: {summary}");
                println!("Agent has finished all tasks. Exiting loop.");
                pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;
                *final_status = "success".into();
                *final_message = summary;
                return Ok(Flow::Break);
            }

            // Preserve this step's results as role:"tool" turns keyed to the
            // calls that produced them.
            pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;

            // A run_script scrape or `more` brought records: scrape mode from
            // the next step. `exit_scrape_mode` ran: back to the page, and the
            // mode's steps leave the history.
            let entered = called_scrape(&action_result);
            let left = called_exit(&action_result);
            if entered || left {
                if let Ok(mut st) = self.state.lock() {
                    st.scrape_mode = entered && !left;
                    if entered && st.scrape_start.is_none() {
                        st.scrape_start = Some(am.len() - 1);
                    }
                    if entered {
                        // The mode's rules: the agent may remove them after.
                        crate::controller::skills::service::load_named(
                            crate::controller::skills::service::SCRAPING_SKILL,
                        );
                    }
                }
            }
            if left {
                let retire = self.state.lock().ok().and_then(|mut st| st.scrape_start.take());
                if let Some(start) = retire {
                    let retired = am.len().saturating_sub(start + 1);
                    retire_scrape_mode(am, tr, start)?;
                    println!("Scraping mode left: {retired} step(s) retired from the history.");
                }
            }
            // The scrape look follows the mode: a run_script scrape put it on
            // the page (controller/run_script), and it comes off here when
            // this step leaves the mode off, because exit_scrape_mode ran,
            // rather than at the next scan.
            if !self.state.lock().map(|st| st.scrape_mode).unwrap_or(false) {
                let _ = self.scanner.get().scrape_glow_off(py);
            }

            // No fixed pause between an action and the next scan — the PAGE
            // is waited for, twice over: the controller settles after every
            // page action (DOM and animations quiet, route_action), and the
            // scanner settles on network quiet. An explicit `wait` is honoured:
            // one that ENDS a batch is slept here, before the scan; one in
            // the middle of a batch was already slept in place by the
            // controller, between the actions it separates (route_action),
            // so only the last result counts here.
            let mut wait_value: Option<Value> = None;
            if action_result.get("tool").and_then(Value::as_str) == Some("wait") {
                wait_value = Some(action_result.get("duration").cloned().unwrap_or(json!(0.0)));
            } else if action_result.get("action").and_then(Value::as_str) == Some("multiple") {
                if let Some(Value::Array(results)) = action_result.get("results") {
                    if let Some(result) = results.last() {
                        if result.get("tool").and_then(Value::as_str) == Some("wait") {
                            wait_value =
                                Some(result.get("duration").cloned().unwrap_or(json!(0.0)));
                        }
                    }
                }
            }
            let wait_time = wait_value
                .as_ref()
                .and_then(Value::as_f64)
                .unwrap_or(0.0);

            if wait_time > 0.0 {
                println!(
                    "Waiting {}s (requested) before next scan...",
                    py_str_of_value(wait_value.as_ref().unwrap())
                );
                let mut elapsed = 0.0f64;
                while elapsed < wait_time {
                    if self.stop_set(py)? {
                        break;
                    }
                    let chunk = (wait_time - elapsed).min(0.5).max(0.0);
                    py.detach(|| std::thread::sleep(Duration::from_secs_f64(chunk)));
                    elapsed += 0.5;
                }
            }

            // Print action result
            if action_result.get("status").and_then(Value::as_str) == Some("success") {
                println!("Action executed successfully");
                match action_result.get("action").and_then(Value::as_str) {
                    Some("todo_created") => println!("Todo list created"),
                    Some("todo_updated") => println!("Todo task marked complete"),
                    _ => {}
                }
            } else {
                // A batch envelope carries no message of its own: name the
                // call that failed and what it said.
                let failed = action_result.get("results").and_then(Value::as_array).and_then(|rs| {
                    rs.iter()
                        .find(|r| r.get("status").and_then(Value::as_str) != Some("success"))
                        .map(|r| {
                            format!(
                                "{}: {}",
                                r.get("tool").and_then(Value::as_str).unwrap_or("call"),
                                r.get("message").map(py_str_of_value).unwrap_or_default()
                            )
                        })
                });
                let msg = failed.or_else(|| action_result.get("message").map(py_str_of_value)).unwrap_or_else(|| {
                    format!("no message in the result: {}", action_result.to_string().chars().take(300).collect::<String>())
                });
                println!("Action result: {msg}");
            }

            *is_first_iteration = false;
            Ok(Flow::Continue)
        })();

        match inner {
            Ok(flow) => Ok(flow),
            Err(e) => {
                if !e.is_instance_of::<PyException>(py) {
                    return Err(e); // BaseException — fly past both handlers
                }
                let msg = py_err_str(py, &e);
                println!("Error processing action: {msg}");
                // The step died mid-flight: record the error against every
                // call so the saved chat never carries a dangling tool call.
                close_pairing(tr, &calls, &note_results, &reject_results, &format!("error: {msg}"))?;
                *is_first_iteration = false;
                Ok(Flow::Continue)
            }
        }
    }
}

/// Python `str()` of a JSON value (module-local alias of view's helper).
fn py_str_of_value(v: &Value) -> String {
    view::py_str_of(v)
}

