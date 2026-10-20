//! Typing into an element.
//!
//! Focus the field, clear it, type, optionally submit — dispatched here over
//! the CDP session the browser side owns and lends out. Trusted keyboard
//! input through Input.dispatchKeyEvent / Input.insertText: no JavaScript in
//! the page, nothing injected. The one thing read from the page is whether
//! a field has focus before anything is typed.
//!
//! `enter` stays part of typing on purpose: filling a search box and
//! submitting it is one action here, so a field is never left half-done
//! between steps. Every other key is the `keyboard` tool's (controller/
//! keyboard), which presses keys and never inserts text.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use crate::browser::{truthy, Cdp, CdpFail, ScannerInner};
use crate::controller::click::service::press;
use crate::controller::service::{
    label_for, rect_for, reject_viewport, resolve_element_id, with_cursor, ActResult,
    ACTION_SETTLE_CAP_MS,
};
use crate::agent::main_driver::view::py_str_of;

/// How long a click gets to land focus in a field: the settle's own cap,
/// since a widget that moves focus when its transition ends (Google Flights'
/// pickers) takes up to that long. A plain field has it on the first read.
const FOCUS_WAIT_MS: u64 = ACTION_SETTLE_CAP_MS;

/// What has focus, read through shadow roots: "" when it is something typed
/// text goes into (a text-like input, a textarea, contenteditable, or an
/// iframe, whose own document holds the focus), otherwise a name for it
/// ("body", "div[role=region]", "input[type=checkbox]") for the message.
const FOCUS_HOLDER: &str = "(function () {\
    var a = document.activeElement;\
    while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;\
    if (!a) return 'nothing';\
    var t = a.tagName, ty = (a.getAttribute('type') || '').toLowerCase();\
    if (t === 'INPUT' && !/^(button|submit|reset|checkbox|radio|file|image|range|color|hidden)$/.test(ty)) return '';\
    if (t === 'TEXTAREA' || t === 'IFRAME' || a.isContentEditable === true) return '';\
    var role = a.getAttribute('role');\
    return t.toLowerCase() + (ty ? '[type=' + ty + ']' : role ? '[role=' + role + ']' : '');\
})()";

/// Wait up to FOCUS_WAIT_MS for a field to have focus. None when one has
/// it, or when the page could not say (no script context: the typing goes
/// ahead as it always did); Some(what) names what has it instead.
fn focus_elsewhere(cdp: &mut Cdp, sess: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(FOCUS_WAIT_MS);
    loop {
        let holder = cdp
            .rpc(
                "Runtime.evaluate",
                json!({"expression": FOCUS_HOLDER, "returnByValue": true}),
                Some(sess),
                5.0,
            )
            .ok()
            .and_then(|r| r.get("result")?.get("value")?.as_str().map(str::to_string));
        match holder {
            Some(what) if !what.is_empty() => {
                if Instant::now() >= deadline {
                    return Some(what);
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            _ => return None,
        }
    }
}

/// One key down + up. `text` is what the key inserts, empty for keys that
/// insert nothing.
fn key(cdp: &mut Cdp, sess: &str, key: &str, vk: i64, text: &str) -> Result<(), CdpFail> {
    for kind in ["keyDown", "keyUp"] {
        let mut p = json!({
            "type": kind,
            "key": key,
            "code": key,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        });
        if kind == "keyDown" && !text.is_empty() {
            p["text"] = json!(text);
        }
        cdp.rpc("Input.dispatchKeyEvent", p, Some(sess), 5.0)?;
    }
    Ok(())
}

/// Focus the element at `rect`, clear whatever is in it, type `text`, and
/// submit when asked.
fn type_into_rect(
    cdp: &mut Cdp,
    sess: &str,
    rect: &[f64; 4],
    text: &str,
    enter: bool,
) -> Result<(), CdpFail> {
    press(cdp, sess, rect, 0.0, 1)?; // focus the field

    // Typing goes to whatever has focus, so make sure that is a field first.
    // A box that only wakes up on a click (Booking.com's search box is inert
    // until then) takes the first click without focusing and the second like
    // any field, so one more click is tried; a covered field takes neither.
    // Without this, selectAll below fell on the whole page and the text on
    // nothing, and the tool still reported it typed.
    if focus_elsewhere(cdp, sess).is_some() {
        press(cdp, sess, rect, 0.0, 1)?;
        if let Some(what) = focus_elsewhere(cdp, sess) {
            return Err(CdpFail::Clean(format!(
                "the field did not take focus, nothing typed: focus is on {what}. \
                 Something may be covering it, or it is not a text field"
            )));
        }
    }

    // `commands` is Chrome's editing-command channel. selectAll through it
    // works whatever the platform modifier is, so there is no cmd-vs-ctrl
    // branch here and no dependence on the page honouring a synthetic ctrl+a
    // that a JS keydown handler may well swallow.
    cdp.rpc(
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "a", "code": "KeyA",
               "windowsVirtualKeyCode": 65, "nativeVirtualKeyCode": 65,
               "commands": ["selectAll"]}),
        Some(sess),
        5.0,
    )?;
    cdp.rpc(
        "Input.dispatchKeyEvent",
        json!({"type": "keyUp", "key": "a", "code": "KeyA",
               "windowsVirtualKeyCode": 65, "nativeVirtualKeyCode": 65}),
        Some(sess),
        5.0,
    )?;

    if text.is_empty() {
        // insertText("") is a no-op, so an explicit clear needs a real
        // Backspace against the selection.
        key(cdp, sess, "Backspace", 8, "")?;
    } else {
        // insertText replaces the selection in ONE event. Per-character key
        // events are both far slower and far less reliable on React-style
        // inputs, which re-render between keystrokes.
        cdp.rpc("Input.insertText", json!({"text": text}), Some(sess), 5.0)?;
    }

    if enter {
        key(cdp, sess, "Enter", 13, "\r")?;
    }
    Ok(())
}

/// Clear [id], type `value`, and press Enter when `enter` is set.
pub fn type_into(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    value: &Value,
    enter: &Value,
    elements: &Map<String, Value>,
) -> ActResult<Value> {
    let idx = resolve_element_id(elements, raw_id)?;
    reject_viewport(idx, "input")?;
    let text = if value.is_null() {
        String::new()
    } else {
        py_str_of(value)
    };
    let submit = truthy(enter);
    let rect = rect_for(elements, idx)?;
    {
        let text = text.clone();
        with_cursor(py, scanner, &rect, move |s| {
            if s.bridged() {
                // Extension mode: the same focus, select-all, insert and Enter,
                // dispatched by AutoCuaBridge (tools.js), its focus check included.
                return s
                    .bridge_call(json!({"type": "input", "rect": rect, "text": text, "enter": submit}))
                    .map(|_| ());
            }
            s.with_tab(|cdp, sess| type_into_rect(cdp, sess, &rect, &text, submit))
        })?;
    }
    let mut message = format!("typed into [{idx}] {}", label_for(elements, idx))
        .trim()
        .to_string();
    if submit {
        message.push_str(" and pressed Enter");
    }
    Ok(json!({"status": "success", "tool": "input", "id": idx,
              "value": text, "enter": submit, "message": message}))
}
