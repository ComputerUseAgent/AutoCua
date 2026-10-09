//! Answering a popup: the `dialog` tool.
//!
//! A JavaScript alert, confirm or prompt freezes its page until it is answered, so
//! the scan shows it in place of the page (browser.rs `dialog_view`, `<dialog>` in
//! the model's input) with Chrome's buttons numbered, and the tool list offers
//! `dialog` on that step only. This presses one: [1] OK, after typing a prompt's
//! text when there is some, or [2] Cancel, which an alert does not have. "Leave
//! site?" never comes here: it is accepted the moment it opens (browser.rs
//! note_dialog, tools.js).

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::browser::ScannerInner;
use crate::controller::service::{err, py_int_value, scan_op, ActResult};

/// Press button [id] of the popup holding the current tab; `value` is typed into a
/// prompt's text box first when the button is OK.
pub fn answer(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    value: &Value,
) -> ActResult<Value> {
    let id = py_int_value(raw_id).unwrap_or(0);
    if !(1..=2).contains(&id) {
        return err(format!("there is no button [{id}] - <dialog> has [1] OK and [2] Cancel"));
    }
    let Some(popup) = scan_op(py, scanner, |s| Ok(s.open_dialog()))? else {
        return err("no popup is open on this tab - it closed, or the tab moved on: read the next input");
    };
    let field = |k: &str| popup.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let (kind, question) = (field("type"), field("message"));
    if kind == "alert" && id != 1 {
        return err("an alert has only [1] OK");
    }
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let accept = id == 1;
    let typed = (kind == "prompt" && accept).then(|| text.clone());
    scan_op(py, scanner, move |s| s.answer_dialog(accept, typed.as_deref()))?;
    let button = if accept { "[1] OK" } else { "[2] Cancel" };
    let message = if kind == "prompt" && accept {
        format!("typed \"{text}\" into the prompt \"{question}\" and pressed {button}")
    } else {
        format!("pressed {button} on the {kind} \"{question}\"")
    };
    Ok(json!({"status": "success", "tool": "dialog", "id": id, "message": message}))
}
