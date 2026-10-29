//! AgentService: the Rust copy of ios/agent/iphone_use/service.py (the agent
//! loop), for the Android driver.
//!
//! The same PyO3 hybrid as the web agent: the loop is Rust, and it drives the
//! SAME Python modules the iOS loop drives, over the GIL:
//! memory_compression.CompressionController (the two transcript lists are
//! real Python lists because apply_pending splices them IN PLACE), the shared
//! vault (AutoCua/vault/service.py: new_chat, hide, hide_json, block) and the
//! LLM providers behind the tool registry's LLMManager. The scanner, the
//! controller and the tool registry are Rust, in this crate; they are the
//! next steps, and this file names what it needs from them:
//!
//!   crate::tool_registry::service
//!     LLMManager::build(py, provider, model, api_key, cli_agent, mode, speed)
//!       -> PyResult<LLMManager>, a #[pyclass(frozen)] with `pub provider`
//!       (lowercased: the cache_control check below compares it to
//!       "openrouter", "anthropic" and "aws"), `pub model_short_name`,
//!       `pub claude`, async_tools(), last_usage_value() and
//!       send_rust(&[Value], &[String]) -> Result<SendOutcome, SendErr>
//!       (SendOutcome::Native(Value) | SendOutcome::Text(String)), which is
//!       called inside py.detach and so attaches the GIL itself for the
//!       provider call; impl From<SendErr> for PyErr.
//!       Python-facing too, exactly like the web crate's, because the
//!       INSTANCE and the CLASS go to memory_compression.CompressionController,
//!       which reads the instance every step and builds a second text-mode
//!       manager from the class for MemoryCompressionAgent: #[new] with the
//!       signature (provider, model, api_key=None, cli_agent=false, mode=None,
//!       speed=None); #[getter]s provider, model_short_name, runtime_api_key
//!       and last_usage (a dict with context_tokens); and send_request(
//!       messages, annotated_screenshot_base64=None), returning the text in
//!       text mode. Without them compression never fires: the controller
//!       swallows the AttributeError.
//!     tool_calls_to_steps(tool_calls: &Value, allowed: &HashSet<String>,
//!       defaults_map: &Value, track_params: &[(String, String)])
//!       -> (actions, calls, rejects, track);
//!     drop_bundled_terminal(actions, calls, rejects: &[Value], terminal: &str)
//!       -> (actions, calls, results);
//!     main_tool_names() / main_tool_names_fast() / main_tool_names_claude()
//!       -> &'static HashSet<String>;
//!     main_action_defaults() / main_action_defaults_fast() /
//!       main_action_defaults_claude() -> &'static Value;
//!     main_reasoning_fields() / main_notes_fields_fast() /
//!       claude_reasoning_fields() -> &'static Vec<(String, String)>;
//!     NOTE_TOOLS: the notes tools' names.
//!   crate::controller::view
//!     ControllerView::new(py, provider, model, web_callback, shell_callback,
//!       api_key, stop_event) -> PyResult<ControllerView>, Send + Sync (it is
//!       held by value in a frozen pyclass: a Mutex around whatever changes);
//!     route_action(py, &Value) -> PyResult<Value>;
//!     pub task_tracker.todo_file: PathBuf;
//!     pub scratchpad_service.scratchpad_file: PathBuf.
//!   crate::controller::tool::open_app::service
//!     installed_apps() -> Vec<(String, String)>: (the app's name as the
//!       launcher shows it, its package) for every installed app, from the
//!       launcher's own cache, no device request (the skills lookup).
//!   crate::tree::element
//!     UIElementScanner::new(py, frontend_callback) -> PyResult<UIElementScanner>,
//!       Send + Sync like the controller;
//!     scan_elements(py) -> PyResult<()>;
//!     get_scan_data(py) -> PyResult<(String, Option<String>)>: the element
//!       tree text and the annotated screenshot, base64;
//!     application_name() -> String: the app in front, "" on the home screen.
//!   crate (lib.rs)
//!     android_dir(py) -> PyResult<&'static PathBuf>, agent_dir(py) and
//!     data_root(py) -> PyResult<PathBuf>; the pymodule registers
//!     view::compression_dump and view::compression_entry, which the
//!     constructor imports from AutoCua.android.agent_native.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use pyo3::exceptions::{PyException, PyFileNotFoundError, PyIOError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use regex::Regex;
use serde_json::{json, Map, Value};

use super::view;
use crate::agent::skills::service::DomainKnowledgeService;
use crate::controller::view::ControllerView;
use crate::tool_registry::service::{self as registry, LLMManager, SendOutcome};
use crate::tree::element::UIElementScanner;
use view::{py_str_of, truthy};

// Run-boundary request markers: on resume, the bridge entry that ended the
// prior run ("Previous run concluded.", saved by agent_conversation
// _build_history with an empty tool slot) gets its slot filled with the NEW
// run's request, so the persisted memory attributes every run's steps to the
// request that drove them. The sentence rides in the bridge's next_goal
// (prefix match: it continues).
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

/// Parallel runs set this to scope the scratchpad (and the todo under it) to
/// scratchpad/<session>/, as AutoCua_IOS_SESSION does for simulator tasks.
const SESSION_ENV: &str = "AutoCua_ANDROID_SESSION";

fn py_err_str(py: Python<'_>, e: &PyErr) -> String {
    e.value(py)
        .str()
        .and_then(|s| s.extract::<String>())
        .unwrap_or_else(|_| e.to_string())
}

fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra, see
/// llm_provider/responses.py): its tool calls run after the turn ends, and
/// the block tells it what that changes about the rules above it.
const ASYNC_TOOLS_ADDENDUM: &str = "\n<async_tools>\n\
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's input. The rules above are unchanged: one notes call first, then every action whose target is on the current screen, stopping where the screen must change. Because nothing comes back mid-turn:\n\
- Never write a text message in a turn that contains tool calls, and never state that an action succeeded or failed: that verdict belongs to the next step's memory, judged on the next screenshot.\n\
- `done` stays the only call of its turn. Never add it after this turn's actions: their results have not arrived, so nothing is verified yet.\n\
</async_tools>\n";

/// Appended to the live user message for a Claude model (quality mode).
/// Claude treats a notes tool as a think step: it calls `agent_memory`, ends
/// its turn to wait for the result, and acts on the NEXT turn - one call per
/// turn and a wasted screen scan per action. Saying in the live message that
/// the notes never end a turn is what puts the notes and the actions in one
/// message. Measured 2026-10-05 on step 1 of "open calculator and compute
/// 23*47" (Sonnet 5 via OpenRouter): notes+action together 0/5 before, 6/6
/// with this reminder plus the matching <blocks> rule in claude_prompt.md and
/// the agent_memory description (tool_registry, `batched=True`). Anthropic's
/// own guidance: a sentence at the end of the current request moves batching
/// far more than the same text in the system prompt, so it rides here, every
/// step, on the live message only (never persisted, so the cached prefix is
/// unchanged).
const CLAUDE_BATCH_REMINDER: &str = "\n\n<critical>This turn must carry the `agent_memory` call AND the action calls that do its \
\"Doing\" in this one response. `agent_memory` alone is not a step: it has no result, \
so never end the turn after it.</critical>";

/// Clear all contents inside AutoCua/android/scratchpad/ for a fresh start.
///
/// Parallel tasks set AutoCua_ANDROID_SESSION and own only their own
/// scratchpad/<session>/ subtree: wiping the whole folder there would delete
/// a sibling task's notes mid-run. The todo folder (android/todo, not under
/// scratchpad/ like macOS) gets its own wipe for the same fresh start.
fn cleanup_scratchpad(android_dir: &PathBuf) -> PyResult<()> {
    let mut scratchpad_dir = android_dir.join("scratchpad");
    let session = std::env::var(SESSION_ENV).unwrap_or_default();
    if !session.is_empty() {
        scratchpad_dir = scratchpad_dir.join(session);
    }
    if scratchpad_dir.exists() {
        let entries = std::fs::read_dir(&scratchpad_dir)
            .map_err(|e| PyIOError::new_err(format!("{}: {e}", scratchpad_dir.display())))?;
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
        std::fs::create_dir_all(&scratchpad_dir)
            .map_err(|e| PyIOError::new_err(format!("{}: {e}", scratchpad_dir.display())))?;
    }

    let todo_dir = android_dir.join("todo");
    if todo_dir.is_dir() {
        std::fs::remove_dir_all(&todo_dir)
            .map_err(|e| PyIOError::new_err(format!("{}: {e}", todo_dir.display())))?;
    }
    Ok(())
}

/// <persistent_memory>: the agent's own ToDo + scratchpad, then <vault> (the
/// login keys saved for the app in front, when there are some), rebuilt
/// fresh every step: the tag the system prompt's <input> describes.
fn persistent_memory(todo_list: &str, scratchpad_content: &str, vault_block: &str) -> String {
    let scratch = if scratchpad_content.is_empty() {
        "<scratchpad>none</scratchpad>".to_string()
    } else {
        format!("<scratchpad>\n{scratchpad_content}\n</scratchpad>")
    };
    let tail = if vault_block.is_empty() { String::new() } else { format!("\n\n{vault_block}") };
    format!("<persistent_memory>\n<todo_list>\n{todo_list}\n</todo_list>\n\n{scratch}{tail}\n</persistent_memory>")
}

/// Build the compact tool_response preserved in agent memory for a step.
///
/// Only web-tool results are compacted: their raw text is already removed
/// and digested into the scratchpad, so memory keeps just the query + a
/// pointer. Web is compacted ONLY when it succeeded: a failed web call keeps
/// its exact response so the agent can see what went wrong. Every other
/// result (shell output, click/input results, etc.) is preserved verbatim.
fn tool_response_for_memory(action_result: &Value) -> Value {
    fn compact(entry: &Value) -> Value {
        if entry.get("tool").and_then(Value::as_str) == Some("web")
            && entry.get("status").and_then(Value::as_str) == Some("success")
        {
            return json!({
                "action": "tool",
                "tool": "web",
                "query": entry.get("query").cloned().unwrap_or_else(|| json!("")),
                "message": "memory optimized — refer to scratchpad for the web result",
            });
        }
        entry.clone()
    }
    if action_result.get("action").and_then(Value::as_str) == Some("multiple") {
        if let Some(Value::Array(results)) = action_result.get("results") {
            let mut res = action_result.clone();
            res["results"] = Value::Array(results.iter().map(compact).collect());
            return res;
        }
    }
    compact(action_result)
}

/// Replace a web step's 'refer to scratchpad' placeholder with the actual
/// numbered findings the agent distilled into the scratchpad on the digest
/// step. The scratchpad is wiped on the next user request, so this keeps the
/// distilled web info in durable conversation memory. Preserves the
/// surrounding shape (a single web result or a 'multiple' results list).
fn backfill_web_findings(tool_response_str: &str, findings: &str) -> String {
    let Ok(mut data) = serde_json::from_str::<Value>(tool_response_str) else {
        return tool_response_str.to_string();
    };
    fn fill(entry: &mut Value, findings: &str) {
        if entry.get("tool").and_then(Value::as_str) == Some("web") {
            if let Some(obj) = entry.as_object_mut() {
                obj.shift_remove("message");
                obj.insert("web_result".into(), Value::String(findings.to_string()));
            }
        }
    }
    let multiple = data.get("action").and_then(Value::as_str) == Some("multiple")
        && data.get("results").is_some();
    if multiple {
        if let Some(Value::Array(results)) = data.get_mut("results") {
            for r in results.iter_mut() {
                fill(r, findings);
            }
        }
    } else {
        fill(&mut data, findings);
    }
    view::dumps_pretty(&data)
}

/// Trim an OLDER history step for context: drop 'action' only. 'thinking'
/// is RETAINED in history (a skipped one costs nothing; FULL thinking is the
/// durable route rationale the prompt tells the agent to consult when
/// re-routing). Handles the leading '<Step_no=N />' marker. LEGACY ONLY:
/// native turns are append-only and never rewritten.
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

/// The vault, called as the iOS loop calls it: what it typed this chat reads
/// as [vault] (the shared hide, AutoCua/vault/service.py), and <vault> lists
/// the keys saved for the app in front (the Android vault,
/// controller/tool/vault, which reads the "android" section).
mod vault {
    use super::*;
    use crate::controller::tool::vault::service::vault_service;

    fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
        py.import("AutoCua.vault.service")
    }

    /// A fresh chat has typed nothing from the vault yet.
    pub fn new_chat(py: Python<'_>) -> PyResult<()> {
        module(py)?.call_method0("new_chat")?;
        Ok(())
    }

    /// `text` with every value the vault typed this chat shown as [vault].
    pub fn hide(py: Python<'_>, text: &str) -> PyResult<String> {
        module(py)?.call_method1("hide", (text,))?.extract()
    }

    /// Every string in a tool result, through `hide`.
    pub fn hide_json(py: Python<'_>, value: &Value) -> PyResult<Value> {
        let obj = pythonize::pythonize(py, value)?;
        let out = module(py)?.call_method1("hide_json", (obj,))?;
        Ok(pythonize::depythonize(&out)?)
    }

    /// <vault>: the key names saved for the app just scanned, "" when none.
    pub fn block(py: Python<'_>) -> PyResult<String> {
        Ok(vault_service().block(py))
    }

    /// A scan starts: until it succeeds the vault types into nothing.
    pub fn begin_scan() {
        vault_service().begin_scan();
    }

    /// The tree a scan just read, which `block` and the vault tool name the
    /// app from. On iOS the scanner pushes it (ios/tree/element.py); here the
    /// loop does, from the scan it just took.
    pub fn update_element_tree(_py: Python<'_>, text: &str) -> PyResult<()> {
        vault_service().update_element_tree(text);
        Ok(())
    }
}

/// Python's text-mode read (universal newlines): "\r\n" and a lone "\r" read
/// as "\n", so a CRLF checkout (Windows, autocrlf) hands the model the same
/// prompt bytes as an LF one.
pub(crate) fn read_text(p: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(p).map(|s| s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Per-step context that survives into the outer error handler so a run
/// dying mid-step can still close the call/result pairing (Python guards the
/// same spot with a NameError check on its closures).
struct StepCtx {
    calls: Vec<Value>,
    /// The notes call's `ok` results, first in every slot.
    note_results: Vec<Value>,
    reject_results: Vec<Value>,
}

enum Flow {
    Continue,
    Break,
}

struct RunState {
    assistant_messages: Py<PyAny>,
    tool_responses: Py<PyAny>,
    last_messages: Py<PyAny>,
}

/// Service for the Android automation agent.
#[pyclass(frozen)]
pub struct AgentService {
    /// "fast" swaps in fast_system_prompt.md and trims the tracking params
    /// the driver's tools carry.
    speed: String,
    save_conversation: bool,
    system_prompt: String,
    llm_manager: Py<LLMManager>,
    /// Where this mode's notes live: on a dedicated notes tool, called first
    /// in every step, while the action tools carry only their own fields.
    /// Quality: `reasoning` (thinking optional, memory + next_goal every
    /// step). Fast: `memory` (`next_goal` alone; fast_system_prompt.md folds
    /// the forward plan and the Expect guard into next_goal). Quality on
    /// Claude (any Claude model, any provider): `agent_memory` (memory +
    /// next_goal every step, no thinking field) and its own prompt,
    /// claude_prompt.md.
    claude: bool,
    notes_tool: &'static str,
    track_params: &'static Vec<(String, String)>,
    allowed_tools: &'static HashSet<String>,
    action_defaults: &'static Value,
    stop_event: Option<Py<PyAny>>,
    scanner: UIElementScanner,
    skills: DomainKnowledgeService,
    controller: ControllerView,
    compression: Py<PyAny>,
    /// Streams the agent's responses to the frontend. The web callback (globe
    /// animation) and the shell callback (terminal animation) go to the
    /// controller, which is what uses them.
    text_callback: Option<Py<PyAny>>,
    /// The bottom "Tool response" chain animation. Signature:
    /// tool_callback(event: str, payload: dict|None); events: run_start |
    /// turn{hasImage} | received{tools:[{name,clicks?}]} | run_end. The
    /// per-turn tools come straight from the parsed action block (this
    /// driver), so the controller needs no per-action plumbing.
    tool_callback: Option<Py<PyAny>>,
    /// Memory bar: called after each LLM call with the provider's exact token
    /// usage (llm_manager.last_usage). The agent only forwards; accumulation +
    /// persistence live in app.py / memory_compression.
    token_callback: Option<Py<PyAny>>,
    /// Resumable chat memory (UI path only): an optimized snapshot from a
    /// PRIOR run of this session, built + loaded by
    /// AutoCua.agent_conversation.service (ALL memory management lives there,
    /// not here). Scratchpad/todo are still wiped on start; continuity comes
    /// ONLY from this memory, not files.
    prior_history: Option<Py<PyAny>>,
    /// The sandbox workspace on the Desktop, made on start.
    sandbox_root: PathBuf,
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

    fn provider_name(&self) -> String {
        self.llm_manager.get().provider.clone()
    }

    fn model_name(&self) -> String {
        self.llm_manager.get().model_short_name.clone()
    }

    /// Read the current todo list from the task tracker's todo.md file: the
    /// task tracker's own file path (single source of truth, exactly where it
    /// writes), so read can never drift from write.
    fn read_todo_from_file(&self) -> String {
        let p = &self.controller.task_tracker.todo_file;
        if !p.exists() {
            return String::new();
        }
        match read_text(p) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                println!("⚠ Error reading todo file: {e}");
                String::new()
            }
        }
    }

    /// Read the current scratchpad entries from the scratchpad service's own
    /// file (scratchpad/milestone/milestone.md, the same layout as macOS and
    /// iOS), where the `scratchpad` tool writes them.
    fn read_scratchpad_from_file(&self) -> String {
        let p = &self.controller.scratchpad_service.scratchpad_file;
        if !p.exists() {
            return String::new();
        }
        match read_text(p) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                println!("⚠ Error reading scratchpad file: {e}");
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
            println!("⚠ Error saving raw response: {e}");
        }
    }

    /// Cached messages carry content as a list of {type,text,...} blocks.
    fn message_text(content: &Bound<'_, PyAny>) -> PyResult<String> {
        if let Ok(list) = content.cast::<PyList>() {
            let mut parts: Vec<String> = Vec::new();
            for item in list.iter() {
                if let Ok(d) = item.cast::<PyDict>() {
                    let text = match d.get_item("text")? {
                        Some(t) => t.str()?.extract()?,
                        None => String::new(),
                    };
                    parts.push(text);
                }
            }
            return Ok(parts.join("\n"));
        }
        if let Ok(s) = content.cast::<PyString>() {
            return s.extract();
        }
        content.str()?.extract()
    }

    /// Save a numbered conversation file rendering the EXACT payload sent
    /// this step (system + interleaved assistant/user turns + current user
    /// message) plus this step's freshly generated response: a faithful peek
    /// into agent memory.
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

            // Render every message in the exact order it was sent to the model.
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
                        // action calls in fast mode), so render it the
                        // readable block way - otherwise every replayed step
                        // shows up blank.
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

            // This step's freshly generated response (the reply, not part of the request).
            out.push_str(&format!("--- ASSISTANT (response · step {interaction_count}) ---\n"));
            out.push_str(current_assistant_response);
            out.push('\n');

            std::fs::write(&file, out).map_err(|e| PyIOError::new_err(e.to_string()))?;
            println!("✓ Memory snapshot saved: conversation_{interaction_count}.txt");
            Ok(())
        })();
        if let Err(e) = attempt {
            println!("Error saving conversation snapshot: {}", py_err_str(py, &e));
        }
    }

    /// Save conversation snapshot to file - simple and direct.
    fn save_conversation_files(
        &self,
        py: Python<'_>,
        messages: &Bound<'_, PyList>,
        current_assistant_response: &str,
        image_sent: bool,
        interaction_count: i64,
    ) {
        if self.save_conversation {
            self.save_conversation_snapshot(py, messages, current_assistant_response, image_sent, interaction_count);
        }
    }
}

// -- Closing a step's call/result pairing ------------------------------------
// A saved tool call with no matching result is a malformed transcript every
// provider rejects on resume, so the slot must never stay None once the turn
// is recorded. There are exactly three honest ways to close it, and each
// break path picks the one that matches what actually happened:
//   pair_results  - the actions RAN: record what they returned.
//   discard_step  - nothing ran: drop the turn entirely rather than invent a
//                   result for a call that never executed (same contract as
//                   the post-LLM stop check, which discards the response).
//   close_pairing - the run died mid-flight: record the error.

/// Record what the calls actually returned, keyed per call: one result each
/// when the controller returned a matching batch, otherwise the first call
/// carries the whole envelope and the rest point at it. Web results are
/// compacted to a scratchpad pointer by tool_response_for_memory.
fn pair_results(
    tr: &Bound<'_, PyList>,
    calls: &[Value],
    note_results: &[Value],
    reject_results: &[Value],
    action_result: &Value,
) -> PyResult<()> {
    let compacted = tool_response_for_memory(action_result);
    let batch = compacted.get("results").and_then(Value::as_array);
    let paired: Vec<Value> = match batch {
        Some(batch) if !calls.is_empty() && batch.len() == calls.len() => calls
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                json!({
                    "tool_call_id": c["id"].clone(),
                    "content": view::dumps_pretty(&batch[idx]),
                })
            })
            .collect(),
        _ if !calls.is_empty() => {
            let envelope = view::dumps_pretty(&compacted);
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
        _ => Vec::new(),
    };
    if !tr.is_empty() {
        let mut all: Vec<Value> = note_results.to_vec();
        all.extend(reject_results.iter().cloned());
        all.extend(paired);
        tr.set_item(tr.len() - 1, view::encode_results(&all))?;
    }
    Ok(())
}

/// Drop this step's turn and its empty slot - nothing ran, so there is
/// nothing to remember. Keeps the two lists 1:1 and leaves the saved chat
/// ending on the last step that really happened. Returns true when it
/// dropped something.
fn discard_step(am: &Bound<'_, PyList>, tr: &Bound<'_, PyList>, normalized: &str) -> PyResult<bool> {
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

/// Last resort for a run dying mid-step (exceptions): record the note
/// against every call so nothing dangles.
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
    /// cli_callback is accepted for GUI parity with the desktop agents (the
    /// web UI always passes it); Android has no CLI agent, so it is ignored.
    /// **_extra absorbs kwargs meant for other agents: agent_launcher's
    /// signature filter forwards everything to an extension type.
    #[new]
    #[pyo3(signature = (provider, model, save_conversation=false, frontend_callback=None,
        text_callback=None, web_callback=None, shell_callback=None, cli_callback=None,
        tool_callback=None, token_callback=None, api_key=None, stop_event=None,
        prior_history=None, speed=None, **_extra))]
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
        _extra: Option<Bound<'_, PyDict>>,
    ) -> PyResult<Self> {
        let _ = cli_callback;
        let android_dir = crate::android_dir(py)?.clone();
        let agent_dir = crate::agent_dir(py)?;

        // Clean up scratchpad for a fresh start
        cleanup_scratchpad(&android_dir)?;

        // Ensure sandbox workspace exists on Desktop
        let home: String = py
            .import("pathlib")?
            .getattr("Path")?
            .call_method0("home")?
            .str()?
            .extract()?;
        let sandbox_root = PathBuf::from(home).join("Desktop").join("sandbox_workspace");
        if !sandbox_root.exists() {
            std::fs::create_dir_all(&sandbox_root)
                .map_err(|e| PyIOError::new_err(format!("{}: {e}", sandbox_root.display())))?;
        }

        // Speed mode: "fast" swaps in fast_system_prompt.md and trims the
        // tracking params the driver's tools carry.
        let speed_str = match &speed {
            Some(v) if !v.is_none() => v.str()?.extract::<String>()?.to_lowercase(),
            _ => "quality".to_string(),
        };
        let speed = if speed_str == "fast" { "fast" } else { "quality" }.to_string();

        // Initialize LLM Manager with optional runtime API key. The driver
        // speaks NATIVE TOOL CALLING: its action tools ARE the output
        // contract (no response_format schema, nothing to parse). It stays
        // cli_agent=false, which is what keeps every provider's screenshot
        // splice (gated on `not cli_agent`) working.
        let api_key_str: Option<String> = match &api_key {
            Some(k) => {
                let b = k.bind(py);
                if b.is_none() { None } else { Some(b.str()?.extract::<String>()?) }
            }
            None => None,
        };
        let llm_manager = Py::new(
            py,
            LLMManager::build(py, &provider, &model, api_key_str.clone(), false, "main", &speed)?,
        )?;
        let fast = speed == "fast";
        let claude = !fast && llm_manager.get().claude;
        let (notes_tool, track_params, allowed_tools, action_defaults): (
            &'static str,
            &'static Vec<(String, String)>,
            &'static HashSet<String>,
            &'static Value,
        ) = if fast {
            ("memory", registry::main_notes_fields_fast(), registry::main_tool_names_fast(), registry::main_action_defaults_fast())
        } else if claude {
            ("agent_memory", registry::claude_reasoning_fields(), registry::main_tool_names_claude(), registry::main_action_defaults_claude())
        } else {
            ("reasoning", registry::main_reasoning_fields(), registry::main_tool_names(), registry::main_action_defaults())
        };
        assert!(registry::NOTE_TOOLS.contains(&notes_tool));

        // Initialize UI Element Scanner with optional frontend callback for image streaming
        let scanner = UIElementScanner::new(py, frontend_callback)?;
        let skills = DomainKnowledgeService::new(py);

        // Initialize Controller: forward the GUI callbacks and stop_event so
        // the web/shell containers animate, web search gets its
        // provider/model, and mid-action stop works (macOS parity; Android has
        // no cli/external_terminal).
        let controller = ControllerView::new(
            py,
            &provider,
            &llm_manager.get().model_short_name,
            web_callback,
            shell_callback,
            api_key_str.clone(),
            stop_event.as_ref().map(|e| e.clone_ref(py)),
        )?;

        // Load the system prompt matching the speed mode (quality/fast), and
        // Claude's own prompt for quality mode on any Claude model.
        let prompt_name = if fast {
            "fast_system_prompt.md"
        } else if claude {
            "claude_prompt.md"
        } else {
            "system_prompt.md"
        };
        let prompt_path = agent_dir.join("android_use").join(prompt_name);
        let mut system_prompt = match read_text(&prompt_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(PyFileNotFoundError::new_err(format!(
                    "{prompt_name} file not found in the agent directory"
                )));
            }
            Err(e) => {
                return Err(PyException::new_err(format!("Error loading system prompt: {e}")));
            }
        };
        // An `async_tools` model (GPT-6 Astra) runs its calls after the turn
        // ends; the block tells it what that changes.
        if llm_manager.get().async_tools() {
            system_prompt.push_str(ASYNC_TOOLS_ADDENDUM);
        }

        // Clear the previous debug folder to start fresh (CWD-relative), and
        // the conversation and raw_reasoning ones only when this run saves its
        // conversation: one that does not leaves the last saved run where it is.
        let folders: &[&str] = if save_conversation {
            &["conversation", "debug", "raw_reasoning"]
        } else {
            &["debug"]
        };
        for &folder in folders {
            let p = PathBuf::from(folder);
            if p.exists() {
                std::fs::remove_dir_all(&p).map_err(|e| PyIOError::new_err(format!("{folder}: {e}")))?;
            }
        }

        // Create conversation directory and initialize fresh conversation file
        if save_conversation {
            std::fs::create_dir_all("conversation")
                .map_err(|e| PyIOError::new_err(format!("conversation: {e}")))?;
            // Always start fresh - each program run is a new session
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
            // Create raw_reasoning directory for storing raw LLM outputs
            std::fs::create_dir_all("raw_reasoning")
                .map_err(|e| PyIOError::new_err(format!("raw_reasoning: {e}")))?;
        }

        // Resumable chat memory (UI path only): only a dict counts.
        let prior = match &prior_history {
            Some(v) if v.cast::<PyDict>().is_ok() => Some(v.clone().unbind()),
            _ => None,
        };

        // Runtime memory compression (rolling handoff at 110k tokens). The
        // compression agent is a SEPARATE agent: ALL orchestration (trigger,
        // worker thread, indicator, splice policy) lives in its controller,
        // shared by every platform. This loop only calls the four hooks:
        // reset / maybe_trigger / apply_pending / finish_run. LLMManager is
        // passed as a class: the controller lazily builds its own 2nd
        // text-mode manager from it. The dump/entry hooks are this driver's
        // OWN: the pyfunctions exported by this module.
        let own_mod = py
            .import("AutoCua.android.agent_native")
            .or_else(|_| py.import("agent_native"))?;
        let compression_cls = py
            .import("AutoCua.memory_compression.controller")?
            .getattr("CompressionController")?;
        let comp_kwargs = PyDict::new(py);
        comp_kwargs.set_item("dump_builder", own_mod.getattr("compression_dump")?)?;
        comp_kwargs.set_item("synthetic_entry", own_mod.getattr("compression_entry")?)?;
        let compression = compression_cls.call(
            (
                llm_manager.clone_ref(py),
                py.get_type::<LLMManager>(),
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
            speed,
            save_conversation,
            system_prompt,
            llm_manager,
            claude,
            notes_tool,
            track_params,
            allowed_tools,
            action_defaults,
            stop_event,
            scanner,
            skills,
            controller,
            compression: compression.unbind(),
            text_callback,
            tool_callback,
            token_callback,
            prior_history: prior,
            sandbox_root,
            state: Mutex::new(RunState {
                assistant_messages: PyList::empty(py).into_any().unbind(),
                tool_responses: PyList::empty(py).into_any().unbind(),
                last_messages: py.None(),
            }),
        })
    }

    /// The final conversation lists, exposed after the run so the
    /// conversation service (NOT the agent) can optimize + persist them.
    #[getter]
    fn assistant_messages(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().assistant_messages.clone_ref(py)
    }

    #[getter]
    fn tool_responses(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().tool_responses.clone_ref(py)
    }

    /// The exact final messages payload sent to the model (system +
    /// interleaved history + the live user message), captured for the debug
    /// memory log so the download is the TRUE conversation.
    #[getter]
    fn last_messages(&self, py: Python<'_>) -> Py<PyAny> {
        self.state.lock().unwrap().last_messages.clone_ref(py)
    }

    #[getter]
    fn speed(&self) -> &str {
        &self.speed
    }

    #[getter]
    fn save_conversation(&self) -> bool {
        self.save_conversation
    }

    #[getter]
    fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    #[getter]
    fn sandbox_root(&self) -> String {
        self.sandbox_root.to_string_lossy().into_owned()
    }

    #[getter]
    fn llm_manager(&self, py: Python<'_>) -> Py<LLMManager> {
        self.llm_manager.clone_ref(py)
    }

    /// Process a user request in an iterative loop until completion.
    ///
    /// Returns a structured outcome {"status", "message"} so callers
    /// (Telegram, web UI) can tell a real completion from a failure instead
    /// of always reporting "done":
    ///   - "success"    a `done` action ran
    ///   - "error"      a critical exception ended the loop (e.g. API failure)
    ///   - "incomplete" the loop ended otherwise (stopped, max steps, parse fails)
    fn process_request<'py>(&self, py: Python<'py>, task: Bound<'py, PyAny>) -> PyResult<Py<PyDict>> {
        let task: String = task.str()?.extract()?;
        // Initialize tracking variables
        let mut step_number: i64 = 0;
        let mut is_first_iteration = true;
        let mut am = PyList::empty(py); // all assistant responses, for memory
        let mut tr = PyList::empty(py); // per-step tool result, aligned 1:1 with am; replayed as tool turns
        let mut pending_web_response: Option<String> = None; // web tool response for the light digest iteration
        let mut web_memory_index: Option<usize> = None; // tr slot of the web step to backfill with the digest's scratchpad
        let mut json_fail_count = 0i32; // consecutive parse failures (max 3 before exit)
        let mut reasoning_only_count = 0i32; // consecutive `reasoning`-only steps (a reminder rides on the third)
        // Final outcome reported to the caller. Defaults to "incomplete" so
        // any loop exit that doesn't explicitly set success/error is reported
        // as not-finished rather than silently looking like a completion.
        let mut final_status = "incomplete".to_string();
        let mut final_message = "Agent stopped before completing the task".to_string();

        // Runtime compression: new call invalidates any stale worker from a
        // prior process_request on this instance.
        self.compression.bind(py).call_method0("reset")?;

        // ---- Resumable memory seed (UI continuation) -------------------------
        // If a prior optimized snapshot was supplied (continuing a saved
        // chat), replay it into the two memory lists so the existing
        // message-build loop re-emits the earlier conversation. Entries are
        // ALREADY trimmed and tool_responses already compacted, so no
        // re-processing. The saved memory ends with a terminal-note step, so
        // on resume that note is the most-recent entry, which makes the loop
        // replay EVERY real step's tool_response.
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
                // The chat's ORIGINAL objective (frozen to run 1 by
                // _build_history); the no="1" request prepend must carry
                // this, not the newest request.
                let prior_task = prior.call_method1("get", ("task",))?;
                if prior_task.is_truthy()? {
                    original_task = prior_task.str()?.extract()?;
                }
                // Label this run boundary: fill the terminal bridge's empty
                // tool slot with a numbered request marker so this run's task
                // is durably attributed to the steps that follow it (the
                // filled slot is part of the persisted lists, so it survives
                // every save/seed cycle).
                let last_slot_none = tr.get_item(tr.len() - 1)?.is_none();
                let last_entry: String = am.get_item(am.len() - 1)?.str()?.extract()?;
                if last_slot_none && last_entry.contains(BRIDGE_SIGNATURE) {
                    // Next marker number: compression can swallow bridge
                    // entries, so count BOTH remaining bridges and existing
                    // marker numbers and continue past the highest.
                    let mut n_bridges: i64 = 0;
                    for item in am.iter() {
                        let s: String = item.str()?.extract()?;
                        if s.contains(BRIDGE_SIGNATURE) {
                            n_bridges += 1;
                        }
                    }
                    let mut max_no = n_bridges;
                    for item in tr.iter() {
                        if let Ok(s) = item.cast::<PyString>() {
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
                println!("🧠 Resumed memory: {} prior step(s) loaded", am.len());
            }
        }
        // A fresh chat has typed nothing from the vault yet; a continuation
        // keeps what it typed hidden, since the phone may still show it.
        if !is_resumed {
            vault::new_chat(py)?;
        }
        // ----------------------------------------------------------------------

        // Print model info once at the start
        println!("\n🔄 Processing with {}", self.model_name());
        self.emit_flow(py, "run_start", None); // tool-flow chain: clear + take over the demo

        // Main agent loop
        loop {
            // Check for stop signal
            if self.stop_set(py)? {
                println!("\n🛑 Agent stopped by user.");
                final_status = "incomplete".into();
                final_message = "Stopped by user".into();
                // Don't send callback to frontend to avoid re-opening the strip
                break;
            }

            // Apply a finished background compression: the controller splices
            // the lists IN PLACE here on the main thread (its worker only
            // deposits) and returns the shifted web_memory_index.
            let shifted = self.compression.bind(py).call_method1(
                "apply_pending",
                (&am, &tr, web_memory_index.map(|i| i as i64)),
            )?;
            web_memory_index = if shifted.is_none() {
                None
            } else {
                Some(shifted.extract::<i64>()?.max(0) as usize)
            };

            step_number += 1;
            let mut is_web_digest = false; // true only on the iteration that digests a web result

            // Max step limit - prevent infinite loops
            if step_number > 100 {
                println!("\n🛑 Max step limit reached (100). Exiting agent loop.");
                final_status = "incomplete".into();
                final_message = "Reached the 100-step limit without finishing".into();
                break;
            }

            // Tool-flow chain: announce the turn. Digest iterations send no
            // image, so the chain skips screenshot+mapping (true to the
            // agent's behaviour).
            self.emit_flow(py, "turn", Some(&json!({"hasImage": pending_web_response.is_none()})));

            let mut annotated_image_base64: Option<String>;
            let mut image_sent: bool;
            let formatted_element_tree: String;
            let skills_block: String;
            let vault_block: String;

            // Skip scan for light digest iterations (web tool response)
            if pending_web_response.is_some() {
                println!("\n{}", "=".repeat(60));
                println!("🌐 Step {step_number}: Web digest iteration (no scan)");
                annotated_image_base64 = None;
                image_sent = false;
                formatted_element_tree = String::new();
                skills_block = String::new();
                vault_block = String::new();
            } else {
                // Scan UI elements and get annotated screenshot
                println!("\n{}", "=".repeat(60));
                println!("🔍 Step {step_number}: Scanning snapshot.");
                // Until this scan succeeds the vault types into nothing
                // (iOS's scanner does the same at the start of a scan).
                vault::begin_scan();
                let scanned = match self.scanner.scan_elements(py) {
                    Ok(()) => true,
                    Err(e) => {
                        // KeyboardInterrupt / SystemExit are BaseException: let
                        // them fly. A failed scan is reported and the step goes
                        // on with what the scanner holds, as the iOS scanner does.
                        if !e.is_instance_of::<PyException>(py) {
                            return Err(e);
                        }
                        println!("Error: {}", py_err_str(py, &e));
                        false
                    }
                };

                // Check Stop AFTER Scan
                if self.stop_set(py)? {
                    final_status = "incomplete".into();
                    final_message = "Stopped by user".into();
                    break;
                }

                // Already encoded at its final size/format by the scanner:
                // re-compressing here would only add a decode/encode round
                // trip and destroy the labels.
                let (element_tree_text, img) = self.scanner.get_scan_data(py)?;
                annotated_image_base64 = img;
                image_sent = annotated_image_base64.is_some();

                if let Some(b64) = &annotated_image_base64 {
                    println!("✅ Image captured - annotated: {} chars", b64.len());
                } else {
                    println!("❌ NO IMAGE - annotated image is None!");
                }

                // Wrap element tree in proper tags. A value the vault typed
                // shows in the field it went into (and anywhere the app
                // echoes it): it reads as [vault].
                formatted_element_tree =
                    format!("<element_tree>\n{}\n</element_tree>", vault::hide(py, &element_tree_text)?);
                // <vault>: the login keys saved for the app just scanned,
                // which the vault reads off the tree it is handed first (only
                // a tree this scan read: after a failed one it keeps the last).
                if scanned {
                    vault::update_element_tree(py, &element_tree_text)?;
                }
                vault_block = vault::block(py)?;
                // Skills are matched against the screen we JUST scanned: the
                // app in front is the root of the tree the scanner parsed.
                let application_name = self.scanner.application_name();
                let skills_text = self.skills.get_knowledge(&application_name);
                skills_block = if skills_text.is_empty() {
                    String::new()
                } else {
                    format!("<skills>\n{skills_text}\n</skills>")
                };
                if !skills_block.is_empty() {
                    println!("📘 skills: loaded for {application_name}");
                }
            }

            // Construct user message based on iteration
            let mut user_message: String;
            if is_first_iteration {
                // First iteration - user_request + element tree only. ToDo
                // creation and timing are governed by <todo_capability> in
                // the system prompt (flexible: iteration 1 by default, may
                // create later), so no hard-coded todo rules are injected here.
                user_message = format!("<user_request>\n{task}\n</user_request>\n");
                if !skills_block.is_empty() {
                    user_message.push_str(&format!("\n{skills_block}\n"));
                }
                if !vault_block.is_empty() {
                    user_message.push_str(&format!("\n{vault_block}\n"));
                }
                user_message.push_str(&format!("\n{formatted_element_tree}"));
                if image_sent {
                    user_message.push_str("\n\n<image>Annotated screenshot with bounding boxes</image>");
                }
            } else {
                // Fetch fresh todo and scratchpad entries from the file system
                let todo_list = self.read_todo_from_file();
                let scratchpad_content = self.read_scratchpad_from_file();

                // Subsequent iterations - request + todo_list. <last_response>
                // is GONE: under native tool calling each result arrives in
                // its canonical place, as a role:"tool" turn keyed to the
                // call that produced it, so re-injecting it here would
                // duplicate it. On a RESUMED session, mark the request as
                // <updated_user_request> so the agent knows this is a
                // continuation of the same session (its prior steps are
                // already in the replayed history above).
                let request_tag = if is_resumed { "updated_user_request" } else { "user_request" };
                user_message = format!("<{request_tag}>\n{task}\n</{request_tag}>");

                if let Some(web) = pending_web_response.take() {
                    user_message = format!(
                        "<critical>\n\
                         No image and element tree provided. Focus on digesting the web response below.\n\
                         1. Analyze thoroughly - extract all relevant data (numbers, dates, names, URLs, prices, etc.)\n\
                         2. Save ALL important findings to scratchpad in this step's action.\n\
                         </critical>\n\n{web}\n\n<user_request>\n{task}\n</user_request>\n\n{}",
                        persistent_memory(&todo_list, &scratchpad_content, "")
                    );

                    image_sent = false;
                    annotated_image_base64 = None;

                    // Flag consumed above; mark this as the web-digest step so
                    // its scratchpad response can be folded into the web
                    // memory below.
                    is_web_digest = true;
                } else {
                    // Normal iteration - include full context
                    user_message.push_str(&format!(
                        "\n\n{}",
                        persistent_memory(&todo_list, &scratchpad_content, &vault_block)
                    ));
                    if !skills_block.is_empty() {
                        user_message.push_str(&format!("\n\n{skills_block}"));
                    }
                    user_message.push_str(&format!("\n\n{formatted_element_tree}"));
                    // Add image tag if image is provided
                    if image_sent {
                        user_message.push_str("\n\n<image>Annotated screenshot with bounding boxes</image>");
                        user_message.push_str("\n\n<critical>Pure vision first: decide which element to interact with from the screenshot alone, then refer to <element_tree> for its [id].</critical>");
                    }
                }
            }

            // Claude: the notes call and the action calls belong in ONE
            // response (see CLAUDE_BATCH_REMINDER). Last in the live message.
            if self.claude {
                user_message.push_str(CLAUDE_BATCH_REMINDER);
            }

            // Build the API messages as a real NATIVE dialogue:
            //   system -> opening user turn (the objective) -> for each past
            //   step: assistant(prose + its tool_calls) -> one role:"tool"
            //   result per call -> finally the live user message (screen +
            //   state).
            //
            // APPEND-ONLY: nothing already in the transcript is ever
            // rewritten, so the prompt-cache prefix stays valid across steps.
            //
            // EVERY step replays its results, including the most recent: a
            // saved tool call with no matching result is a malformed
            // transcript that every provider rejects.
            //
            // Legacy (schema-era) entries degrade to content-only assistant
            // turns and keep the old sliding-window trim + <tool_response>
            // user-turn replay, so resumed chats look exactly as they did
            // before.
            let messages = PyList::empty(py);
            let sys_msg = PyDict::new(py);
            sys_msg.set_item("role", "system")?;
            sys_msg.set_item("content", &self.system_prompt)?;
            messages.append(sys_msg)?;

            // Opening turn: the chat's ORIGINAL objective in the same
            // numbered tag the run-boundary markers use, so the transcript
            // head is stable for the whole chat and follow-ups extend the
            // cached prefix.
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
                        // The provider's OWN metadata for this turn (Gemini 3
                        // thought signatures, OpenRouter reasoning blocks):
                        // the provider translates it back into its dialect;
                        // every other provider never sees the key.
                        asst.set_item("provider_meta", pythonize::pythonize(py, &step.meta)?)?;
                    }
                } else {
                    // Legacy step: keep the old trim for older entries.
                    let content = if is_recent { step_msg.clone() } else { trim_history_entry(&step_msg) };
                    asst.set_item("content", content)?;
                }
                messages.append(asst)?;

                if i < tr.len() {
                    let slot = tr.get_item(i)?;
                    if slot.is_truthy()? {
                        // decode_results emits role:"tool" turns for native
                        // results, a bare user turn for a run-boundary marker,
                        // and a <tool_response>-wrapped user turn for legacy
                        // slots.
                        let slot_str: String = slot.str()?.extract()?;
                        for result in view::decode_results(&slot_str) {
                            messages.append(pythonize::pythonize(py, &result)?)?;
                        }
                    }
                }
            }

            // Live user message (screen + todo + scratchpad + element tree).
            // Rebuilt every step and never persisted; the screenshot attaches
            // to it. The system prompt is added exactly once (above): the
            // resumable seed never carries a system prompt, so a continued
            // session can't double it.
            let live = PyDict::new(py);
            live.set_item("role", "user")?;
            live.set_item("content", &user_message)?;
            messages.append(live)?;

            // Capture the exact payload for the debug memory log (the last
            // iteration's value is retained, i.e. the agent's final true memory).
            self.state.lock().unwrap().last_messages = messages.clone().into_any().unbind();

            // Prompt caching (OpenRouter/Anthropic/AWS): mark the newest
            // PERSISTENT turn - the last tool result, or the opening user
            // turn on step 1. Never the live user message: it is rebuilt and
            // dropped every request, so a breakpoint there is written once
            // and never read back. Never an assistant turn either:
            // Anthropic's translation flattens those and would silently drop
            // the breakpoint. The last tool result survives into the next
            // request's prefix, so each call reads the cache the previous
            // one wrote.
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
                    if let Ok(s) = content.cast::<PyString>() {
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
                is_web_digest,
                &mut pending_web_response,
                &mut web_memory_index,
                &mut step_ctx,
                &mut final_status,
                &mut final_message,
            );

            match outcome {
                Ok(Flow::Continue) => continue,
                Ok(Flow::Break) => break,
                Err(e) => {
                    // KeyboardInterrupt / SystemExit are BaseException, not
                    // Exception: Python's `except Exception` lets them fly.
                    if !e.is_instance_of::<PyException>(py) {
                        return Err(e);
                    }
                    let msg = py_err_str(py, &e);
                    println!("❌ Error processing request: {msg}");
                    // Same pairing guarantee for a failure ANYWHERE in the
                    // step. The NameError guard in Python = ctx presence here
                    // (an exception raised before the closers were defined,
                    // e.g. the LLM call itself failing).
                    if let Some(ctx) = &step_ctx {
                        close_pairing(&tr, &ctx.calls, &ctx.note_results, &ctx.reject_results, &format!("error: {msg}"))?;
                    }
                    // On critical error, record it and break the loop so the
                    // caller can report the failure instead of a false "done".
                    final_status = "error".into();
                    final_message = msg;
                    break;
                }
            }
        }

        // tool-flow chain: if the run didn't finish cleanly, cap it with a "!" drop.
        if final_status == "error" {
            self.emit_flow(py, "error", Some(&json!({"text": "llm service not responding"})));
        } else if final_status == "incomplete" && final_message.to_lowercase().contains("stop") {
            self.emit_flow(py, "error", Some(&json!({"text": "agent interrupted"})));
        }
        self.emit_flow(py, "run_end", None); // tool-flow chain: run finished

        // Expose the final conversation lists so the conversation service
        // (NOT the agent) can optimize + persist them after the run. Memory
        // management is kept entirely out of the agent. Return shape is
        // unchanged. Compression still pending at run end (in flight or
        // deposited but never applied): the result is discarded; make sure
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
    /// One step's guarded section: everything inside Python's outer `try`.
    #[allow(clippy::too_many_arguments)]
    fn run_step<'py>(
        &self,
        py: Python<'py>,
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
        is_web_digest: bool,
        pending_web_response: &mut Option<String>,
        web_memory_index: &mut Option<usize>,
        step_ctx: &mut Option<StepCtx>,
        final_status: &mut String,
        final_message: &mut String,
    ) -> PyResult<Flow> {
        // Make API request through LLM Manager
        if image_sent {
            println!("📸 Screenshot sent to LLM (annotated)");
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
        let shots: Vec<String> = annotated_image_base64.map(str::to_string).into_iter().collect();
        let outcome = py.detach(|| core.send_rust(&msgs, &shots)).map_err(PyErr::from)?;
        let raw_response: Value = match outcome {
            SendOutcome::Native(v) => v,
            SendOutcome::Text(s) => Value::String(s),
        };

        // Memory bar: forward this call's exact token usage (input+output).
        if let Some(cb) = &self.token_callback {
            let usage = pythonize::pythonize(py, &core.last_usage_value())?;
            let _ = cb.call1(py, (usage,));
        }

        // CRITICAL Check Stop AFTER LLM (discards result if stopped while waiting)
        if self.stop_set(py)? {
            println!("\n🛑 Agent stopped by user (response discarded).");
            *final_status = "incomplete".into();
            *final_message = "Stopped by user".into();
            return Ok(Flow::Break);
        }

        // Runtime compression trigger: context size is fresh in last_usage.
        self.compression
            .bind(py)
            .call_method1("maybe_trigger", (am, tr, original_task))?;

        println!("✓ LLM response received");

        // Save raw response before any parsing (for debugging). Under native
        // tool calling this is a dict, not a string.
        let raw_text = match &raw_response {
            Value::Object(_) => view::dumps_pretty(&raw_response),
            other => py_str_of(other),
        };
        self.save_raw_response(&raw_text, *step_number);

        // NATIVE TOOL CALLING: nothing to parse. Convert the model's own
        // calls into the same [{type, ...}] action dicts route_action has
        // always consumed, keeping their ids so each result pairs back to the
        // call that produced it in the next request.
        let (actions, calls, rejects, track_vec) = registry::tool_calls_to_steps(
            raw_response.get("tool_calls").unwrap_or(&Value::Null),
            self.allowed_tools,
            self.action_defaults,
            self.track_params,
        );
        let track: Map<String, Value> = track_vec
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let resp_text: String = raw_response
            .get("text")
            .filter(|v| truthy(v))
            .map(|v| py_str_of(v).trim().to_string())
            .unwrap_or_default();
        let provider_meta: Option<Value> = raw_response.get("provider_meta").cloned();

        // The notes call (`reasoning` in quality mode, `memory` in fast) is
        // the step's notes, not an action: pull it out before routing (the
        // controller has no handler for it) but keep it in `all_calls`,
        // which is what the assistant turn replays - and what Gemini's
        // thought signature binds to. Each note call is answered with the
        // bare REASONING_RESULT; the snapshot and the compression dump hide it.
        let notes_tool = self.notes_tool;
        let is_note = |a: &Value| a.get("type").and_then(Value::as_str) == Some(notes_tool);
        let all_calls: Vec<Value> = calls.clone();
        let note_ids: Vec<Value> = actions
            .iter()
            .zip(calls.iter())
            .filter(|(a, _)| is_note(a))
            .map(|(_, c)| c["id"].clone())
            .collect();
        let mut note_results: Vec<Value> = note_ids
            .iter()
            .map(|id| json!({"tool_call_id": id, "content": view::REASONING_RESULT}))
            .collect();
        let (actions, calls): (Vec<Value>, Vec<Value>) = actions
            .into_iter()
            .zip(calls)
            .filter(|(a, _)| !is_note(a))
            .unzip();

        // Backstop for an `async_tools` model (Astra): a `done` bundled after
        // this turn's actions is dropped from the batch and answered with an
        // error result (see drop_bundled_terminal); the result rides with
        // the rejects below.
        let mut done_results: Vec<Value> = Vec::new();
        let (actions, calls) = if core.async_tools() {
            let (actions, calls, dropped) = registry::drop_bundled_terminal(actions, calls, &rejects, "done");
            if !dropped.is_empty() {
                println!("`done` bundled with other actions: ignored, it must be the only call of its turn");
            }
            done_results = dropped;
            (actions, calls)
        } else {
            (actions, calls)
        };

        if actions.is_empty() && rejects.is_empty() {
            if !note_ids.is_empty() {
                // A reasoning-only step: the model chose to think before
                // acting. Accepted as a normal step (it costs a fresh scan,
                // so only a RUN of them is bounded: from the third in a row
                // the result carries a reminder).
                *reasoning_only_count += 1;
                // An `async_tools` model that never acts is the route having
                // dropped the async flags (one call per turn again): the
                // reminder cannot help it, so the run stops at six instead
                // of scanning to the step cap.
                if core.async_tools() && *reasoning_only_count >= 6 {
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
                let wired = view::wire_calls_from(&all_calls)
                    .map_err(|k| pyo3::exceptions::PyKeyError::new_err(k.to_string()))?;
                let normalized = view::encode_step(&resp_text, &Value::Array(wired), provider_meta.as_ref());
                // Only the fields this mode carries (fast and Claude have no thinking).
                let display_payload = view::dumps_pretty(&display_block(&track, &resp_text, json!([])));
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
            // REPAIR, not exit: `done` is an explicit dedicated final call,
            // so a message with NO tool calls is always a dropped-calls
            // mistake - the model narrated its move instead of making it.
            // Keep the turn so the reasoning isn't lost, answer it with a
            // corrective result turn, and let the model continue. The strike
            // counter still bounds a model that keeps talking without acting.
            *json_fail_count += 1;
            if !resp_text.is_empty() {
                println!("message had no tool calls - asking the model to act ({}/3)", json_fail_count);
                if *json_fail_count >= 3 {
                    *final_status = "incomplete".into();
                    *final_message = "Model kept responding without calling any tool (3 consecutive turns)".into();
                    return Ok(Flow::Break);
                }
                let normalized = view::encode_step(&resp_text, &json!([]), provider_meta.as_ref());
                // snapshot shows the prose the model wrote, not the codec JSON
                self.save_conversation_files(py, messages, &resp_text, image_sent, *step_number);
                am.append(&normalized)?;
                // Feedback in the SAME envelope every real result uses: the
                // environment reporting "nothing ran", not an instruction
                // breaking the loop's rhythm.
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
                *final_message = "Could not get a valid model response (3 consecutive failures)".into();
                return Ok(Flow::Break);
            }
            *step_number -= 1;
            return Ok(Flow::Continue);
        }

        // Reset the consecutive counters on a real, actionable turn
        *json_fail_count = 0;
        *reasoning_only_count = 0;

        // The assistant turn EXACTLY as the model produced it: prose on the
        // text channel, its tool calls on the tool channel - this is what
        // gets replayed next step. REJECTED calls are included too: the turn
        // must mirror what the model emitted, and every result written below
        // (errors included) has to pair with a call of the same id or the
        // next request is malformed.
        let mut replay = all_calls.clone();
        replay.extend(rejects.iter().cloned());
        let wired = view::wire_calls_from(&replay)
            .map_err(|k| pyo3::exceptions::PyKeyError::new_err(k.to_string()))?;
        let normalized = view::encode_step(&resp_text, &Value::Array(wired), provider_meta.as_ref());

        // Display payload - the terminal print, the frontend text stream and
        // the tool-icon chain read this block view; the transcript sent to
        // the model does not. Only the params this mode actually carries:
        // quality has all three (on the `reasoning` call; memory + next_goal
        // on Claude), fast has `next_goal` alone. Rendering one the mode
        // dropped would print an empty block in the terminal and the UI
        // stream.
        let display_payload =
            view::dumps_pretty(&display_block(&track, &resp_text, Value::Array(actions.clone())));

        // Save a faithful snapshot of the EXACT payload sent this step plus
        // this step's response - a true peek into agent memory. The response
        // is rendered as the readable block view, not the codec JSON (whose
        // call arguments are JSON escaped inside JSON).
        self.save_conversation_files(py, messages, &display_payload, image_sent, *step_number);

        // Record the turn with an empty result slot, backfilled after
        // routing - so a `done` turn also lands in the conversation.
        am.append(&normalized)?;
        tr.append(py.None())?;

        // A call naming a tool that doesn't exist is answered with an error
        // result instead of being dropped, so the model sees what went wrong
        // - keyed to its own call id - and corrects next turn.
        let mut reject_results: Vec<Value> = rejects
            .iter()
            .map(|r| {
                json!({
                    "tool_call_id": r["id"].clone(),
                    "content": format!("error: {}", py_str_of(&r["error"])),
                })
            })
            .collect();
        reject_results.extend(done_results);

        *step_ctx = Some(StepCtx {
            calls: calls.clone(),
            note_results: note_results.clone(),
            reject_results: reject_results.clone(),
        });

        // Format the response with emojis for console output (terminal: include action block)
        let formatted_response = view::format_response_impl(&display_payload, true);
        println!("{formatted_response}");

        // Send to frontend if callback exists (omit action from stream).
        // Deliberately UNGUARDED, like the Python original: an exception here
        // lands in the outer error handler.
        if let Some(cb) = &self.text_callback {
            cb.call1(py, (view::format_response_impl(&display_payload, false),))?;
        }

        // Tool-flow chain: the packet arrived: tick "packet received" and
        // hand the frontend this turn's tools, read straight from the action
        // block.
        self.emit_flow(
            py,
            "received",
            Some(&json!({"tools": view::extract_tools_impl(&display_payload)})),
        );

        if !rejects.is_empty() {
            for r in &rejects {
                println!("unknown tool '{}' - error returned to the model", py_str_of(&r["name"]));
            }
            if actions.is_empty() {
                // Nothing valid to route: persist just the errors and let the
                // model correct itself on the next turn.
                let mut only_rejects: Vec<Value> = note_results.clone();
                only_rejects.extend(reject_results.iter().cloned());
                tr.set_item(tr.len() - 1, view::encode_results(&only_rejects))?;
                *is_first_iteration = false;
                return Ok(Flow::Continue);
            }
        }

        // The actions came straight from the model's native tool calls
        let inner: PyResult<Flow> = (|| {
            // Check Stop BEFORE Action - NOTHING ran this step, so discard
            // the turn instead of saving a result for a call that never
            // executed. The chat then ends on the last step that really
            // happened.
            if self.stop_set(py)? {
                discard_step(am, tr, &normalized)?;
                println!("\n🛑 Agent stopped by user (step discarded - no action ran).");
                *final_status = "incomplete".into();
                *final_message = "Stopped by user".into();
                return Ok(Flow::Break);
            }

            // Execute the action
            println!("\n⚡ Executing action...");

            // (The scanner already handed the elements mapping to the
            // controller during scan_elements, so no set_elements handoff
            // is needed here.)

            // Send action to controller. A value the vault typed can come
            // back in a result: it reads as [vault]. Not in `done`: that is
            // the model's own answer to the user.
            let mut action_result: Value = self.controller.route_action(py, &Value::Array(actions.clone()))?;
            if action_result.get("action").and_then(Value::as_str) != Some("done") {
                action_result = vault::hide_json(py, &action_result)?;
            }

            // Check if action was stopped mid-execution. Here the actions
            // DID partially run, so record what the controller actually
            // returned - that partial effect is real and the next run must
            // see it.
            if action_result.get("status").and_then(Value::as_str) == Some("stopped") {
                println!("\n🛑 Agent stopped by user (action interrupted).");
                pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;
                *final_status = "incomplete".into();
                *final_message = "Stopped by user".into();
                return Ok(Flow::Break);
            }

            // Check if web tool was used: store for the light digest
            // iteration. A failed search stays in its own result, so its
            // error is kept in memory verbatim instead of going to a digest.
            let mut web_results_list: Vec<String> = Vec::new();
            let is_web_hit = |r: &Value| {
                r.get("tool").and_then(Value::as_str) == Some("web")
                    && r.get("result").is_some()
                    && r.get("status").and_then(Value::as_str) == Some("success")
            };
            if is_web_hit(&action_result) {
                if let Some(obj) = action_result.as_object_mut() {
                    if let Some(result) = obj.shift_remove("result") {
                        web_results_list.push(py_str_of(&result));
                    }
                }
            } else if action_result.get("action").and_then(Value::as_str) == Some("multiple")
                && action_result.get("results").is_some()
            {
                if let Some(Value::Array(results)) = action_result.get_mut("results") {
                    for result in results.iter_mut() {
                        if is_web_hit(result) {
                            if let Some(obj) = result.as_object_mut() {
                                if let Some(r) = obj.shift_remove("result") {
                                    web_results_list.push(py_str_of(&r));
                                }
                            }
                        }
                    }
                }
            }

            if !web_results_list.is_empty() {
                *pending_web_response = Some(format!("<tool>\n{}\n</tool>", web_results_list.join("\n")));
                println!("🌐 Web results captured for digest iteration");
            } else {
                *pending_web_response = None;
            }

            // Check if task completed (done action was executed)
            if action_result.get("action").and_then(Value::as_str) == Some("done") {
                let summary = action_result
                    .get("summary")
                    .map(py_str_of)
                    .unwrap_or_else(|| "Task completed".to_string());
                println!("\n🎉 Task Complete: {summary}");
                println!("✅ Agent has finished all tasks. Exiting loop.");
                // `done` ran, so record the real outcome (the summary) rather
                // than a filler - this is the final step the next resume reads.
                pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;
                *final_status = "success".into();
                *final_message = summary;
                return Ok(Flow::Break);
            }

            // Preserve this step's results as role:"tool" turns keyed to the
            // calls that produced them.
            pair_results(tr, &calls, &note_results, &reject_results, &action_result)?;

            // Web-result memory: on the digest step, fold the numbered
            // scratchpad findings the agent JUST wrote into the original web
            // step's tool_response, replacing the 'refer to scratchpad'
            // pointer. The scratchpad is wiped on the next user request, so
            // this keeps the distilled web info in durable conversation memory.
            if is_web_digest {
                if let Some(idx) = *web_memory_index {
                    if idx < tr.len() && tr.get_item(idx)?.is_truthy()? {
                        let entries: Vec<String> = actions
                            .iter()
                            .filter(|a| {
                                a.get("type").and_then(Value::as_str) == Some("scratchpad")
                                    && a.get("value").is_some_and(truthy)
                            })
                            .map(|a| py_str_of(&a["value"]).trim().to_string())
                            .collect();
                        if !entries.is_empty() {
                            let numbered: Vec<String> =
                                entries.iter().enumerate().map(|(i, e)| format!("{}. {e}", i + 1)).collect();
                            let numbered = numbered.join("\n");
                            // The web step's slot is a native result list.
                            // Fold the findings into EVERY entry (the web
                            // call may sit at any index - results are
                            // per-call now, and reject errors ride ahead of
                            // them; backfill_web_findings is a no-op on
                            // anything that isn't a web result) and re-encode
                            // onto the SAME call ids, so the pairing that step
                            // wrote stays intact.
                            let slot: String = tr.get_item(idx)?.str()?.extract()?;
                            let stored = serde_json::from_str::<Value>(&slot).ok();
                            let keyed = match &stored {
                                Some(Value::Array(items)) => {
                                    !items.is_empty()
                                        && items.iter().all(|d| {
                                            d.as_object().is_some_and(|o| o.contains_key("tool_call_id"))
                                        })
                                }
                                _ => false,
                            };
                            if keyed {
                                let Some(Value::Array(mut items)) = stored else { unreachable!() };
                                for entry in items.iter_mut() {
                                    let content = match entry.get("content") {
                                        Some(c) if truthy(c) => py_str_of(c),
                                        _ => String::new(),
                                    };
                                    entry["content"] = Value::String(backfill_web_findings(&content, &numbered));
                                }
                                tr.set_item(idx, view::encode_results(&items))?;
                            } else {
                                // Legacy (schema-era) slot: plain JSON string.
                                tr.set_item(idx, backfill_web_findings(&slot, &numbered))?;
                            }
                        }
                        *web_memory_index = None;
                    }
                }
            }

            // A web call this step points the pointer at THIS step, for the
            // next digest to fold into. Set AFTER the fold above, which still
            // needed the previous web step.
            if !web_results_list.is_empty() {
                *web_memory_index = Some(tr.len() - 1);
            }

            // Wait before next scan (default 1 second, unless wait action was used)
            let mut wait_time = 1.0f64;
            if action_result.get("tool").and_then(Value::as_str) == Some("wait") {
                // If wait was explicitly called, use that duration
                wait_time = action_result.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
            } else if action_result.get("action").and_then(Value::as_str) == Some("multiple") {
                // Check if wait was in multiple actions
                if let Some(Value::Array(results)) = action_result.get("results") {
                    for result in results {
                        if result.get("tool").and_then(Value::as_str) == Some("wait") {
                            wait_time = result.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
                            break;
                        }
                    }
                }
            }

            println!("⏳ Waiting {wait_time:?}s before next scan...");
            let mut elapsed = 0.0f64;
            while elapsed < wait_time {
                if self.stop_set(py)? {
                    break;
                }
                let chunk = (wait_time - elapsed).min(0.5).max(0.0);
                py.detach(|| std::thread::sleep(Duration::from_secs_f64(chunk)));
                elapsed += 0.5;
            }

            // Print action result
            if action_result.get("status").and_then(Value::as_str) == Some("success") {
                println!("✓ Action executed successfully");
                match action_result.get("action").and_then(Value::as_str) {
                    // Check if this was a todo creation
                    Some("todo_created") => println!("📋 Todo list created"),
                    // Check if this was a todo update
                    Some("todo_updated") => println!("✓ Todo task marked complete"),
                    _ => {}
                }
            } else {
                let message = action_result
                    .get("message")
                    .map(py_str_of)
                    .unwrap_or_else(|| "Unknown error".to_string());
                println!("⚠ Action result: {message}");
            }

            // Mark first iteration as done
            *is_first_iteration = false;
            Ok(Flow::Continue)
        })();

        match inner {
            Ok(flow) => Ok(flow),
            Err(e) => {
                if !e.is_instance_of::<PyException>(py) {
                    return Err(e); // BaseException: fly past both handlers
                }
                let msg = py_err_str(py, &e);
                println!("⚠ Error processing action: {msg}");
                // The step died mid-flight: record the error against every
                // call so the saved chat never carries a dangling tool call.
                close_pairing(tr, &calls, &note_results, &reject_results, &format!("error: {msg}"))?;
                // Even on error, continue the loop
                *is_first_iteration = false;
                Ok(Flow::Continue)
            }
        }
    }
}

/// The display block {thinking?, memory?, next_goal?, action}: only the
/// fields this mode carries (fast has next_goal alone, Claude's
/// `agent_memory` has no thinking), a `thinking` the model left out shown as
/// the prose it wrote, else "skipped".
fn display_block(track: &Map<String, Value>, resp_text: &str, actions: Value) -> Value {
    let mut display = Map::new();
    if let Some(t) = track.get("thinking") {
        let value = if truthy(t) {
            t.clone()
        } else if !resp_text.is_empty() {
            Value::String(resp_text.to_string())
        } else {
            Value::String(view::SKIPPED_THINKING.to_string())
        };
        display.insert("thinking".into(), value);
    }
    if let Some(m) = track.get("memory") {
        display.insert("memory".into(), m.clone());
    }
    if let Some(g) = track.get("next_goal") {
        display.insert("next_goal".into(), g.clone());
    }
    display.insert("action".into(), actions);
    Value::Object(display)
}
