//! Pausing between steps.
//!
//! Nothing is dispatched to the browser here. This only parses and bounds
//! the duration and reports it back; the sleep happens where the wait sits.
//! A `wait` that ENDS a step is slept by the agent loop, which reads
//! `duration` off the result and keeps checking the stop event every half
//! second; a `wait` in the MIDDLE of a batch is slept by route_action, in
//! place between the actions it separates (see controller/view.rs).

use serde_json::{json, Value};

use crate::browser::truthy;
use crate::controller::service::{py_float_str, ActResult};
use crate::agent::main_driver::view::py_str_of;

pub const DEFAULT_WAIT_SECONDS: f64 = 1.0;
/// A model that asks for a minute has almost always misread a loading page.
/// The next scan re-reads the page anyway, so waiting longer buys nothing.
pub const MAX_WAIT_SECONDS: f64 = 30.0;

pub fn wait(value: &Value) -> ActResult<Value> {
    let raw = if truthy(value) {
        py_str_of(value)
    } else {
        py_float_str(DEFAULT_WAIT_SECONDS)
    };
    let secs = raw
        .trim()
        .parse::<f64>()
        .unwrap_or(DEFAULT_WAIT_SECONDS)
        .clamp(0.0, MAX_WAIT_SECONDS);
    Ok(json!({"status": "success", "tool": "wait", "duration": secs,
              "message": format!("waiting {}s", py_float_str(secs))}))
}
