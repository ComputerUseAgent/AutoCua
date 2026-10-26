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
//! Not ported yet, as asked: the video player (play, pause, close,
//! streaming), the web search and the vault answer with an error that says
//! so, the way iOS answers for a service it has not ported, so the model sees
//! why and goes on.

use std::time::Duration;

use pyo3::exceptions::{PyException, PyIOError};
use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use super::scratchpad::service::ScratchpadService;
use super::service::{controller_service, py_float, py_int, ActErr, ControllerService};
use super::task_tracker::service::TaskTrackerService;
use super::tool::open_app::service::app_launcher_service;
use super::tool::shell::service::ShellService;
use crate::agent::android_use::view::{py_str_of, truthy};

/// Python `str(x or "")`.
fn str_or_empty(v: &Value) -> String {
    if truthy(v) { py_str_of(v) } else { String::new() }
}

fn stopped() -> Value {
    json!({"status": "stopped", "action": "stop", "message": "Stopped by user"})
}

/// The central router for all phone actions.
pub struct ControllerView {
    /// LLM provider, model and runtime key, for the web search (when it comes).
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
        })
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
                "click" => {
                    let element_id = get("id");
                    let result = match self.controller_service.click(&element_id) {
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
                    let result = match self.controller_service.ground_click(&x, &y) {
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

                "input" => {
                    let element_id = get("id");
                    let text_value = get("value");
                    let result = match self.controller_service.type_text(&element_id, &py_str_of(&text_value)) {
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
                    let result = match self.controller_service.scroll(&element_id, &py_str_of(&direction)) {
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
                        if launcher.go_home() {
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
                        let launch = launcher.launch_app(&app_name);
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
                    let _ = &self.web_callback;
                    return Ok(json!({
                        "status": "error",
                        "action": "tool",
                        "tool": "web",
                        "query": query,
                        "message": "Web service not available yet on Android",
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

                "vault" => {
                    return Ok(json!({
                        "status": "error",
                        "action": "vault",
                        "results": results,
                        "message": "Vault not available yet on Android",
                    }));
                }

                "video_player" => {
                    let command = get("value");
                    let message = "Video player not available yet on Android";
                    eprintln!("Video player action failed: {message}");
                    results.push(json!({
                        "status": "error",
                        "action": "video_player",
                        "command": command,
                        "message": message,
                        "tool_response": format!("videoplayer: Result - Error: {message}"),
                    }));
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
