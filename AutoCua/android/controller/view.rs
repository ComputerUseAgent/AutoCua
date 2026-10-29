//! route_action: the copy of ios/controller/view.py, the central router for
//! all phone actions. Takes the `[{type, ...}]` action dicts the model's
//! native tool calls were converted into, and carries each one out. Return
//! shapes are the iOS ones, which the agent loop keys on:
//!
//!     done        -> {"action": "done", "summary": ...}        ends the loop
//!     wait        -> {"tool": "wait", "duration": N}           sets the inter-step delay
//!     web         -> {"tool": "web", "result": ...}            a success goes to the digest step
//!     todo_list   -> {"action": "todo_created"}
//!     update_todo -> {"action": "todo_updated"}
//!     a batch     -> {"action": "multiple", "results": [...]}  one entry per action that ran
//!
//! The iPhone's video_player close is `back` here (the system Back, which
//! also takes the page back); video_player keeps streaming, pause and play.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use pyo3::exceptions::{PyException, PyIOError};
use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use super::scratchpad::service::ScratchpadService;
use super::service::{controller_service, py_float, py_int, ActErr, ControllerService};
use super::task_tracker::service::TaskTrackerService;
use super::tool::open_app::service::app_launcher_service;
use super::tool::shell::service::ShellService;
use super::tool::back::service as back;
use super::tool::vault::service::{vault_service, DEVICE_ENTRIES, MAX_PIN_ATTEMPTS, TEXT_FIELDS};
use super::tool::videoplayer::service as videoplayer;
use super::tool::web::service::WebService;
use crate::agent::android_use::view::{py_str_of, truthy};

/// Python `str(x or "")`.
fn str_or_empty(v: &Value) -> String {
    if truthy(v) { py_str_of(v) } else { String::new() }
}

fn stopped() -> Value {
    json!({"status": "stopped", "action": "stop", "message": "Stopped by user"})
}

/// The animated "🌐 Web..." line while a web search runs. Terminal only: the
/// `\r` overwrite only works in a real terminal, and in the UI's piped
/// subprocess it would just dump carriage returns into the reader.
struct WebLoading {
    done: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WebLoading {
    fn start() -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let thread = std::io::stdout().is_terminal().then(|| {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let dots = ["", ".", "..", "..."];
                let mut idx = 0;
                while !done.load(Ordering::Relaxed) {
                    let mut out = std::io::stdout();
                    let _ = write!(out, "\r🌐 Web{}   ", dots[idx % dots.len()]);
                    let _ = out.flush();
                    idx += 1;
                    std::thread::sleep(Duration::from_millis(500));
                }
            })
        });
        WebLoading { done, thread }
    }

    /// Stop the line and clear the in-place text (terminal only).
    fn stop(mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if std::io::stdout().is_terminal() {
            let mut out = std::io::stdout();
            let _ = write!(out, "\r{}\r", " ".repeat(50));
            let _ = out.flush();
        }
    }
}

/// The central router for all phone actions.
pub struct ControllerView {
    /// LLM provider, model and runtime key, for the web search.
    pub provider: String,
    pub model: String,
    pub api_key: Option<String>,
    /// Web search status (start/end) and shell status (start/result/end)
    /// callbacks for the frontend's animations.
    web_callback: Option<Py<PyAny>>,
    shell_callback: Option<Py<PyAny>>,
    /// For stopping actions mid-execution.
    stop_event: Option<Py<PyAny>>,
    /// The one instance the scanner feeds element mappings into.
    pub controller_service: &'static ControllerService,
    pub task_tracker: TaskTrackerService,
    pub scratchpad_service: ScratchpadService,
    shell_service: ShellService,
    /// Vault entry name -> PIN tries that may have been spent.
    pin_attempts: Mutex<HashMap<String, u32>>,
}

impl ControllerView {
    pub fn new(
        py: Python<'_>,
        provider: &str,
        model: &str,
        web_callback: Option<Py<PyAny>>,
        shell_callback: Option<Py<PyAny>>,
        api_key: Option<String>,
        stop_event: Option<Py<PyAny>>,
    ) -> PyResult<Self> {
        let android_dir = crate::android_dir(py)?;
        let task_tracker = TaskTrackerService::new(android_dir).map_err(|e| PyIOError::new_err(e.to_string()))?;
        let scratchpad_service = ScratchpadService::new(android_dir).map_err(|e| PyIOError::new_err(e.to_string()))?;
        Ok(ControllerView {
            provider: provider.to_string(),
            model: model.to_string(),
            api_key,
            web_callback,
            shell_callback,
            stop_event,
            controller_service: controller_service(),
            task_tracker,
            scratchpad_service,
            shell_service: ShellService::new(py),
            pin_attempts: Mutex::new(HashMap::new()),
        })
    }

    /// vault {"id": 0, "value": "pin"}: the vault taps its PIN on the
    /// on-screen keypad. Every refusal happens before the first tap. Results
    /// and logs never carry a digit, a key id or a coordinate (see the PIN
    /// part of tool/vault/service.rs).
    fn enter_vault_pin(&self, py: Python<'_>, action_count: usize) -> Value {
        let base = |status: &str, message: String| {
            json!({"action": "vault", "element": 0, "kind": "pin", "status": status, "message": message})
        };
        if action_count > 1 {
            // The keys come from this step's scan; another action could move the screen first.
            return base("error", "A PIN vault must be the only action in its step. Re-issue it alone.".to_string());
        }
        let vault = vault_service();
        // Keypad first, vault second: asking where no keypad is must not reveal whether a PIN is saved.
        let Some(keys) = vault.find_pin_keys(&self.controller_service.elements()) else {
            return base("error", "No digit keypad (0-9) recognised on screen. Open the PIN screen first.".to_string());
        };

        let (name, pin) = vault.get_pin(py);
        // The phone's own lock-screen PIN is not tapped on Android yet: the
        // home screen and System UI also show widgets and notifications,
        // which can draw ten digit keys and a press count of their own, and
        // the real keyguard has not been read on a phone to tell it apart.
        let device_entry = DEVICE_ENTRIES.contains(&name.to_lowercase().replace(' ', "").as_str());
        if device_entry {
            return base(
                "error",
                "The vault does not enter the phone's own lock-screen PIN on Android. Ask the user to unlock the phone."
                    .to_string(),
            );
        }
        let Some(pin) = pin else {
            return base(
                "error",
                format!(
                    "No PIN saved in the vault for \"{name}\". Ask the user to add \"{name}\": {{\"pin\": \"...\"}} under \"android\" in AutoCua_data/vault/credentials.json."
                ),
            );
        };
        let tries = self.pin_attempts.lock().unwrap().get(&name).copied().unwrap_or(0);
        if tries >= MAX_PIN_ATTEMPTS {
            return base(
                "error",
                format!(
                    "The \"{name}\" PIN was already tried twice in this task. Stop and ask the user, another wrong try could lock the keypad."
                ),
            );
        }

        let outcome = vault.enter_pin(py, &keys, &pin, device_entry);
        // A pad that closed starts fresh; only a try that may have reached the pad counts.
        let mut attempts = self.pin_attempts.lock().unwrap();
        if outcome.closed {
            attempts.insert(name, 0);
        } else if outcome.pressed {
            attempts.insert(name, tries + 1);
        }
        base(outcome.status, outcome.message)
    }

    /// Set the elements mapping in the controller service.
    pub fn set_elements(&self, elements_mapping: Map<String, Value>, _application_name: &str) {
        self.controller_service.update_elements(elements_mapping);
    }

    fn stopped(&self, py: Python<'_>) -> PyResult<bool> {
        match &self.stop_event {
            Some(ev) => ev.bind(py).call_method0("is_set")?.is_truthy(),
            None => Ok(false),
        }
    }

    fn sleep(py: Python<'_>, seconds: f64) {
        if seconds > 0.0 {
            py.detach(|| std::thread::sleep(Duration::from_secs_f64(seconds)));
        }
    }

    /// Route actions to the appropriate service based on action type.
    ///
    /// Flat action format: [{"type": "click", "id": 4}, {"type": "wait",
    /// "value": "2"}, ...]; a single flat action dict is also accepted and
    /// wrapped. Returns the result of the action execution (a single result,
    /// or {"action": "multiple", "results": [...]} for a sequence). Never a
    /// Python error for a failed action: that becomes an error RESULT the
    /// model sees (a KeyboardInterrupt still flies).
    pub fn route_action(&self, py: Python<'_>, action_data: &Value) -> PyResult<Value> {
        match self.route(py, action_data) {
            Ok(result) => Ok(result),
            Err(ActErr::Msg(message)) => {
                eprintln!("Error routing action: {message}");
                Ok(json!({"status": "error", "message": message}))
            }
            Err(ActErr::Py(e)) => {
                if !e.is_instance_of::<PyException>(py) {
                    return Err(e);
                }
                let message = e.value(py).to_string();
                eprintln!("Error routing action: {message}");
                Ok(json!({"status": "error", "message": message}))
            }
        }
    }

    /// Web search status for the frontend's web animation: "start" or "end",
    /// the one argument the web callback takes.
    fn web_status(&self, py: Python<'_>, status: &str) -> PyResult<()> {
        if let Some(cb) = &self.web_callback {
            cb.call1(py, (status,))?;
        }
        Ok(())
    }

    fn callback(&self, py: Python<'_>, cb: &Option<Py<PyAny>>, args: (&str, Value)) -> Result<(), ActErr> {
        if let Some(cb) = cb {
            let payload = pythonize::pythonize(py, &args.1).map_err(PyErr::from)?;
            cb.call1(py, (args.0, payload))?;
        }
        Ok(())
    }

    fn route(&self, py: Python<'_>, action_data: &Value) -> Result<Value, ActErr> {
        // Accept a single flat action dict by wrapping it in a list
        let actions: Vec<Value> = match action_data {
            Value::Object(_) => vec![action_data.clone()],
            Value::Array(items) => items.clone(),
            _ => Vec::new(),
        };

        let mut results: Vec<Value> = Vec::new();

        for action_item in &actions {
            // Check stop before every action
            if self.stopped(py)? {
                return Ok(stopped());
            }

            let kind = action_item
                .as_object()
                .and_then(|o| o.get("type"))
                .filter(|t| truthy(t))
                .map(py_str_of);
            let Some(action_type) = kind else {
                eprintln!("Action item missing 'type' field: {action_item}");
                continue;
            };
            let get = |key: &str| action_item.get(key).cloned().unwrap_or(Value::Null);

            match action_type.as_str() {
                // The phone calls below run adb (a tap is Android's own `input`
                // tool, ~0.2 s; open_app waits up to 3 s for the app): Python's
                // lock is let go meanwhile, so the app's Stop, its watchers and
                // its window never wait on the phone.
                "click" => {
                    let element_id = get("id");
                    let result = match py.detach(|| self.controller_service.click(&element_id)) {
                        Ok(()) => json!({"status": "success", "action": "click", "element": py_int(&element_id).unwrap_or(0)}),
                        Err(e) => {
                            eprintln!("✗ Click failed: {e}");
                            json!({"status": "error", "action": "click", "element": element_id, "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result.clone());
                    if failed {
                        return Ok(result);
                    }
                }

                "ground_click" => {
                    let (x, y) = (get("x"), get("y"));
                    let result = match py.detach(|| self.controller_service.ground_click(&x, &y)) {
                        Ok(()) => json!({"status": "success", "action": "ground_click", "x": x, "y": y}),
                        Err(e) => {
                            eprintln!("✗ Ground click failed: {e}");
                            json!({"status": "error", "action": "ground_click", "x": x, "y": y, "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result.clone());
                    if failed {
                        return Ok(result);
                    }
                }

                "ground_scroll" => {
                    let (x, y, direction) = (get("x"), get("y"), get("value"));
                    let way = py_str_of(&direction);
                    let result = match py.detach(|| self.controller_service.ground_scroll(&x, &y, &way)) {
                        Ok(()) => json!({"status": "success", "action": "ground_scroll", "x": x, "y": y, "direction": direction}),
                        Err(e) => {
                            eprintln!("✗ Ground scroll failed: {e}");
                            json!({"status": "error", "action": "ground_scroll", "x": x, "y": y, "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result.clone());
                    if failed {
                        return Ok(result);
                    }
                }

                "input" => {
                    let element_id = get("id");
                    let text_value = get("value");
                    let text = py_str_of(&text_value);
                    let result = match py.detach(|| self.controller_service.type_text(&element_id, &text)) {
                        Ok(()) => json!({"status": "success", "action": "input", "element": py_int(&element_id).unwrap_or(0), "text": text_value}),
                        Err(e) => {
                            eprintln!("✗ Input failed: {e}");
                            json!({"status": "error", "action": "input", "element": element_id, "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result);
                    if failed {
                        return Ok(json!({
                            "status": "error",
                            "action": "input",
                            "results": results,
                            "message": format!("Input insertion in element {} failed", py_str_of(&element_id)),
                        }));
                    }
                }

                "scroll" => {
                    let element_id = get("id");
                    let direction = get("value");
                    let way = py_str_of(&direction);
                    let result = match py.detach(|| self.controller_service.scroll(&element_id, &way)) {
                        Ok(()) => json!({"status": "success", "action": "scroll", "element": py_int(&element_id).unwrap_or(0), "direction": direction}),
                        Err(e) => {
                            eprintln!("✗ Scroll failed: {e}");
                            json!({"status": "error", "action": "scroll", "element": element_id, "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result);
                    if failed {
                        return Ok(json!({
                            "status": "error",
                            "action": "scroll",
                            "results": results,
                            "message": format!("Scroll on element {} failed", py_str_of(&element_id)),
                        }));
                    }
                }

                "open_app" => {
                    let app_name = str_or_empty(&get("value")).trim().to_string();
                    let launcher = app_launcher_service();
                    // Special 'home' case: return to the home screen
                    if app_name.to_lowercase() == "home" {
                        if py.detach(|| launcher.go_home()) {
                            results.push(json!({"status": "success", "action": "tool", "tool": "open_app", "app": "home"}));
                        } else {
                            return Ok(json!({
                                "status": "error",
                                "action": "tool",
                                "tool": "open_app",
                                "app": "home",
                                "message": "Failed to navigate to home screen",
                            }));
                        }
                    } else {
                        let launch = py.detach(|| launcher.launch_app(&app_name));
                        let ok = launch.get("ok").is_some_and(truthy);
                        // Report what was ACTUALLY launched (resolved display
                        // name + package), never just the query, and success
                        // only when the launcher confirmed the foreground app.
                        let mut result = json!({
                            "status": if ok { "success" } else { "error" },
                            "action": "tool",
                            "tool": "open_app",
                            "requested": app_name,
                            "message": launch.get("message").cloned().unwrap_or_else(|| json!("")),
                        });
                        if launch.get("display_name").is_some_and(truthy) {
                            result["app"] = launch["display_name"].clone();
                            result["bundle_id"] = launch.get("bundle_id").cloned().unwrap_or(Value::Null);
                        }
                        if ok {
                            if launch.get("verified") == Some(&Value::Bool(false)) {
                                result["verified"] = json!(false);
                            }
                            results.push(result);
                        } else {
                            eprintln!("Failed to open {app_name}: {}", py_str_of(&result["message"]));
                            return Ok(result);
                        }
                    }
                }

                "wait" => {
                    let raw = get("value");
                    let one = Value::String("1".into());
                    let wait_time = py_float(if raw.is_null() { &one } else { &raw }).map_err(ActErr::Msg)?;
                    let mut elapsed = 0.0f64;
                    while elapsed < wait_time {
                        if self.stopped(py)? {
                            return Ok(stopped());
                        }
                        Self::sleep(py, (wait_time - elapsed).min(0.5));
                        elapsed += 0.5;
                    }
                    results.push(json!({"status": "success", "action": "tool", "tool": "wait", "duration": wait_time}));
                }

                "web" => {
                    let query = get("value");

                    self.web_status(py, "start")?;
                    let loading = WebLoading::start();

                    let stop_event = self.stop_event.as_ref().map(|e| e.clone_ref(py));
                    let web_result = WebService::new(py, &self.provider, &self.model, self.api_key.clone(), stop_event)
                        .and_then(|web_service| web_service.search(py, &py_str_of(&query)));

                    loading.stop();
                    if self.web_callback.is_some() {
                        self.web_status(py, "end")?;
                        // Wait for CSS fade-out to complete before next action
                        Self::sleep(py, 0.7);
                    }
                    let web_result = web_result?;

                    // WebService reports a failed search inside its text
                    let failed = web_result.starts_with("{\ntool: web,\nstatus: error,");
                    results.push(json!({
                        "status": if failed { "error" } else { "success" },
                        "action": "tool",
                        "tool": "web",
                        "query": query,
                        "result": web_result,
                    }));
                }

                "shell" => {
                    // Runs on the host computer this agent runs on, not the phone.
                    let command = str_or_empty(&get("value"));

                    // Signal frontend: terminal card appears
                    self.callback(py, &self.shell_callback, ("start", json!(command)))?;

                    let result = py.detach(|| self.shell_service.run(&command));

                    // Signal frontend: show success/fail result
                    if self.shell_callback.is_some() {
                        let shell_status = result.get("status").cloned().unwrap_or_else(|| json!("failed"));
                        let shell_output = result
                            .get("output")
                            .or_else(|| result.get("error"))
                            .cloned()
                            .unwrap_or_else(|| json!(""));
                        self.callback(py, &self.shell_callback, ("result", json!({"status": shell_status, "output": shell_output})))?;
                        // Hold result on screen for 2 seconds
                        Self::sleep(py, 2.0);
                        self.callback(py, &self.shell_callback, ("end", Value::Null))?;
                        // Wait for CSS fade-out to complete before next action
                        Self::sleep(py, 0.7);
                    }

                    let fatal = result.get("status").and_then(Value::as_str) == Some("error")
                        && result.get("error").is_some_and(truthy);
                    results.push(result.clone());
                    if fatal {
                        return Ok(result);
                    }
                }

                "vault"
                    if matches!(str_or_empty(&get("value")).trim().to_lowercase().as_str(), "pin" | "passcode")
                        && !self
                            .controller_service
                            .elements()
                            .get(&py_str_of(&get("id")))
                            .and_then(|e| e.get("type"))
                            .and_then(Value::as_str)
                            .is_some_and(|t| TEXT_FIELDS.contains(&t)) =>
                {
                    // A PIN aimed at no text field (id 0) goes to the keypad
                    // path: the lock screen and app PIN pads have no field to
                    // type into. A PIN field is filled like any other key, below.
                    let result = self.enter_vault_pin(py, actions.len());
                    if result["status"] != "success" {
                        return Ok(result);
                    }
                    results.push(result);
                }

                "vault" => {
                    // The value saved under the key the agent names, typed like
                    // `input` (tool/vault/service.rs fill). Batches like `input` too.
                    let result = vault_service().fill(py, &get("id"), &get("value"))?;
                    let failed = result["status"] != "success";
                    results.push(result.clone());
                    if failed {
                        return Ok(json!({"status": "error", "action": "vault", "results": results, "message": result["message"]}));
                    }
                }

                "back" => {
                    // The system Back, `value` times (tool/back/service.rs).
                    let result = match back::back(py, &get("value")) {
                        Ok(presses) => json!({"status": "success", "action": "back", "presses": presses}),
                        Err(e) => {
                            eprintln!("✗ Back failed: {e}");
                            json!({"status": "error", "action": "back", "message": e})
                        }
                    };
                    let failed = result["status"] == "error";
                    results.push(result.clone());
                    if failed {
                        return Ok(result);
                    }
                }

                "video_player" => {
                    // Watched on the screen (tool/videoplayer/service.rs); a
                    // player closes with `back`.
                    let command = get("value");
                    let tool_response = match command.as_str() {
                        Some("streaming") => Ok(if videoplayer::streaming(py) { "Streaming" } else { "Not streaming" }),
                        Some("pause") => Ok(if videoplayer::pause(py) { "Paused" } else { "Failed to pause" }),
                        Some("play") => Ok(if videoplayer::play(py) { "Playing" } else { "Failed to play" }),
                        Some("close") => Err("video_player has no close on Android: use back".to_string()),
                        _ => Err(format!("Unknown video_player command: {}", py_str_of(&command))),
                    };
                    results.push(match tool_response {
                        Ok(said) => json!({
                            "status": "success",
                            "action": "video_player",
                            "command": command,
                            "tool_response": format!("videoplayer: Result - {said}"),
                        }),
                        Err(message) => {
                            eprintln!("Video player action failed: {message}");
                            json!({
                                "status": "error",
                                "action": "video_player",
                                "command": command,
                                "message": message,
                                "tool_response": format!("videoplayer: Result - Error: {message}"),
                            })
                        }
                    });
                }

                "todo_list" => {
                    let todo_value = py_str_of(&get("value"));
                    self.task_tracker.save_todo(&todo_value);
                    results.push(json!({"status": "success", "action": "todo_created"}));
                }

                "update_todo" => {
                    let raw = get("value");
                    let zero = Value::String("0".into());
                    let task_number = py_int(if raw.is_null() { &zero } else { &raw }).map_err(ActErr::Msg)?;
                    if self.task_tracker.update_task(task_number) {
                        results.push(json!({"status": "success", "action": "todo_updated", "task": task_number}));
                    } else {
                        // Reported on its own call; the rest of the batch still runs
                        results.push(json!({
                            "status": "error",
                            "action": "todo_update_failed",
                            "message": format!("Task {task_number} is not in the todo list"),
                        }));
                    }
                }

                "scratchpad" => {
                    let scratchpad_value = py_str_of(&get("value"));
                    if self.scratchpad_service.append_scratchpad(&scratchpad_value) {
                        results.push(json!({"status": "success", "action": "scratchpad_added", "scratchpad": scratchpad_value}));
                    } else {
                        return Ok(json!({
                            "status": "error",
                            "action": "scratchpad_failed",
                            "message": "Could not add scratchpad entry",
                        }));
                    }
                }

                "done" => {
                    let summary = get("value");
                    return Ok(json!({"status": "success", "action": "done", "summary": summary}));
                }

                _ => {
                    eprintln!("Unknown action type: {action_type}");
                    results.push(json!({
                        "status": "error",
                        "action": action_type,
                        "message": format!("Unknown action type: {action_type}"),
                    }));
                }
            }

            // Check if last action was stopped
            if results.last().and_then(|r| r.get("status")).and_then(Value::as_str) == Some("stopped") {
                return Ok(results.pop().unwrap());
            }
        }

        if results.is_empty() {
            Ok(json!({"status": "error", "message": "No valid action found"}))
        } else if results.len() == 1 {
            Ok(results.pop().unwrap())
        } else {
            Ok(json!({"status": "success", "action": "multiple", "results": results}))
        }
    }
}
