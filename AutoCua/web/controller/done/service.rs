//! Run completion.
//!
//! `done` touches nothing in the browser — its whole job is to hand the loop
//! the final summary and the one field that ends the run. That field is
//! `action == "done"`: the agent loop breaks on it, records the summary as
//! the run's outcome and pairs it as the last tool result, so it must be
//! present and spelled exactly that way or the loop keeps going.
//!
//! The summary is never dropped or defaulted away silently — it is what the
//! user and the next resumed session both read as "what happened".

use serde_json::{json, Value};

use crate::browser::truthy;
use crate::controller::service::ActResult;
use crate::agent::main_driver::view::py_str_of;

pub const FALLBACK_SUMMARY: &str = "Task completed";

pub fn finish(value: &Value) -> ActResult<Value> {
    let raw = if truthy(value) { py_str_of(value) } else { String::new() };
    let trimmed = raw.trim();
    // An empty summary is a poor ending, not a failed one: the actions really
    // did run. Say so plainly rather than returning a blank.
    let summary = if trimmed.is_empty() { FALLBACK_SUMMARY } else { trimmed };
    Ok(json!({"status": "success", "action": "done", "tool": "done",
              "summary": summary}))
}
