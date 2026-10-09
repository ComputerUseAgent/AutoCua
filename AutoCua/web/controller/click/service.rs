//! Clicking — both click tools live here.
//!
//! `click` and `hold_click` are one concern: press the pointer on an element
//! and let go. The only difference is how long the button stays down.
//!
//! The press itself is dispatched here, over the CDP session the browser side
//! owns and lends out for the length of one operation. Trusted input through
//! Input.dispatchMouseEvent — the same pipeline a physical mouse feeds — so
//! no JavaScript runs in the page and nothing is injected into it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use crate::browser::{web_store_url, Cdp, CdpFail, ScannerInner, WEB_STORE_BLOCKED};
use crate::controller::service::{
    err, label_for, py_float_str, py_float_value, py_int_value, rect_for, reject_viewport,
    resolve_element_id, scan_op, with_cursor, ActResult,
};

/// A hold shorter than this is just a click with extra steps; the model
/// asking for 0 almost certainly meant a normal click.
pub const MIN_HOLD_SECONDS: f64 = 0.1;
/// Beyond a double click there is no web gesture to express, so anything
/// higher is clamped rather than repeated.
pub const MAX_CLICK_TIMES: i64 = 2;
pub const DEFAULT_HOLD_SECONDS: f64 = 1.0;

/// One press or release at a point, with the click count the page reads to
/// tell a double click from two separate ones.
fn button(
    cdp: &mut Cdp,
    sess: &str,
    kind: &str,
    x: f64,
    y: f64,
    count: u32,
) -> Result<Value, CdpFail> {
    cdp.rpc(
        "Input.dispatchMouseEvent",
        json!({
            "type": kind,
            "x": x,
            "y": y,
            "button": "left",
            "clickCount": count,
            "buttons": if kind == "mousePressed" { 1 } else { 0 },
        }),
        Some(sess),
        5.0,
    )
}

/// Press and release on the centre of `rect` (CSS px).
///
/// The centre is derived HERE from the rect, so there is exactly one argument
/// shape and no way to pass a point where a rect is expected.
///
/// `pub(crate)` because typing starts with exactly this gesture: the input
/// tool focuses a field by clicking it, and a second copy of the press would
/// be a second thing to keep right.
pub(crate) fn press(
    cdp: &mut Cdp,
    sess: &str,
    rect: &[f64; 4],
    hold_seconds: f64,
    times: i64,
) -> Result<(), CdpFail> {
    let [x, y, w, h] = *rect;
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    // Move first. Hover handlers open the menus and tooltips the press is then
    // meant to land inside, and some widgets ignore a press that arrives with
    // no prior movement over them.
    cdp.rpc(
        "Input.dispatchMouseEvent",
        json!({"type": "mouseMoved", "x": x, "y": y, "buttons": 0}),
        Some(sess),
        5.0,
    )?;
    // A double click is ONE gesture with a rising clickCount, not two
    // independent clicks: the page distinguishes them by that count, and
    // dblclick only fires when the second press reports 2.
    let times = times.clamp(1, MAX_CLICK_TIMES) as u32;
    let hold = Duration::from_secs_f64(hold_seconds.max(0.0));
    for n in 1..=times {
        button(cdp, sess, "mousePressed", x, y, n)?;
        if !hold.is_zero() {
            std::thread::sleep(hold);
        }
        button(cdp, sess, "mouseReleased", x, y, n)?;
    }
    Ok(())
}

/// A link to the Chrome Web Store is never followed, in either mode (the owner's
/// rule): the press is refused before it is made, as the tab tools refuse the address.
/// The link's address is the href in its scan line's tag (an element's text can quote
/// an href too), read against the address of the page it was scanned on, as the
/// browser reads it (a scheme-relative or relative href included).
fn refuse_web_store_link(elements: &Map<String, Value>, idx: i64, page: &str) -> ActResult<()> {
    let line = elements
        .get(&idx.to_string())
        .and_then(|e| e.get("line"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let tag = line
        .split_once('<')
        .and_then(|(_, rest)| rest.split_once('>'))
        .map(|(tag, _)| tag)
        .unwrap_or("");
    let href = tag
        .split_once("href=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(href, _)| href)
        .unwrap_or("");
    if href.is_empty() {
        return Ok(());
    }
    let address = url::Url::parse(page)
        .and_then(|base| base.join(href))
        .map(String::from)
        .unwrap_or_else(|_| href.to_string());
    if web_store_url(&address) {
        return err(WEB_STORE_BLOCKED);
    }
    Ok(())
}

/// Click [id] once, or twice as a single double-click gesture.
pub fn click(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    times: &Value,
    elements: &Map<String, Value>,
) -> ActResult<Value> {
    let idx = resolve_element_id(elements, raw_id)?;
    reject_viewport(idx, "click")?;
    let page = scan_op(py, scanner, |s| Ok(s.url.clone()))?;
    refuse_web_store_link(elements, idx, &page)?;
    let n = py_int_value(times).map(|n| n.clamp(1, MAX_CLICK_TIMES)).unwrap_or(1);
    let rect = rect_for(elements, idx)?;
    with_cursor(py, scanner, &rect, |s| {
        if s.bridged() {
            // Extension mode: the same press, dispatched by AutoCuaBridge (tools.js).
            return s.bridge_call(json!({"type": "click", "rect": rect, "times": n})).map(|_| ());
        }
        s.with_tab(|cdp, sess| press(cdp, sess, &rect, 0.0, n))
    })?;
    let how = if n == 2 { "double-clicked" } else { "clicked" };
    let message = format!("{how} [{idx}] {}", label_for(elements, idx))
        .trim()
        .to_string();
    Ok(json!({"status": "success", "tool": "click", "id": idx, "times": n,
              "message": message}))
}

/// Press and hold [id] for `seconds`, then release.
pub fn hold_click(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    seconds: &Value,
    elements: &Map<String, Value>,
) -> ActResult<Value> {
    let idx = resolve_element_id(elements, raw_id)?;
    reject_viewport(idx, "hold_click")?;
    let page = scan_op(py, scanner, |s| Ok(s.url.clone()))?;
    refuse_web_store_link(elements, idx, &page)?;
    let secs = py_float_value(seconds)
        .map(|s| s.max(MIN_HOLD_SECONDS))
        .unwrap_or(DEFAULT_HOLD_SECONDS);
    let rect = rect_for(elements, idx)?;
    with_cursor(py, scanner, &rect, |s| {
        if s.bridged() {
            return s
                .bridge_call(json!({"type": "click", "rect": rect, "times": 1, "hold_s": secs}))
                .map(|_| ());
        }
        s.with_tab(|cdp, sess| press(cdp, sess, &rect, secs, 1))
    })?;
    let message = format!(
        "held [{idx}] {} for {}s",
        label_for(elements, idx),
        py_float_str(secs)
    )
    .trim()
    .to_string();
    Ok(json!({"status": "success", "tool": "hold_click", "id": idx, "time": secs,
              "message": message}))
}
