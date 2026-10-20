//! Low-level browser control — shared helpers for every tool.
//!
//! The desktop platforms drive a real mouse and keyboard here; a browser has
//! no such state: every event is dispatched into the page through CDP by the
//! scanner and is over the moment it returns, so this stays thin on purpose.
//!
//! Error strings here are model-visible tool results — they are contracts,
//! ported byte-for-byte from the Python originals.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyo3::prelude::*;
use serde_json::{Map, Value};

use crate::browser::{truthy, SResult, ScanErr, ScannerInner};
use crate::agent::main_driver::view::py_str_of;

/// [1] is the page itself on every scan — the scanner reserves it. It is a
/// scroll target, never a control.
pub const VIEWPORT_ID: i64 = 1;

/// How long the box stays on screen after the action. Long enough to register
/// on a headful screen, short enough not to pace the agent.
/// How long the cursor takes to travel to its target. MUST match the
/// transition duration on `.AutoCua-cursor` in browser/glow/glow.css: waiting
/// exactly one glide is what guarantees the arrow has ARRIVED before the press
/// is dispatched, and the stylesheet is the only thing that knows how long the
/// glide takes. Change one and the click lands mid-flight.
pub const CURSOR_MOVE_SECONDS: f64 = 0.65;

/// How long the pressed cursor is held before the real event goes out. The
/// shrink itself takes 100ms, so most of this is the arrow sitting visibly
/// pressed. A real click is far too fast to see, and a press animation that
/// plays only after the page has already reacted tells the human nothing about
/// what is coming — which is the whole point of showing it.
pub const CURSOR_PRESS_SECONDS: f64 = 0.35;

/// A tool failure: either a message the model reads back (Python's caught
/// Exception -> error result), or a raised Python error (KeyboardInterrupt)
/// that must fly past the per-action catch exactly as it did in Python.
pub enum ActErr {
    Msg(String),
    Py(PyErr),
}

impl From<ScanErr> for ActErr {
    fn from(e: ScanErr) -> ActErr {
        match e {
            ScanErr::Scanner(m) => ActErr::Msg(m),
            ScanErr::Py(err) => ActErr::Py(err),
        }
    }
}

pub type ActResult<T> = Result<T, ActErr>;

pub fn err<T>(msg: impl Into<String>) -> ActResult<T> {
    Err(ActErr::Msg(msg.into()))
}

/// Python `int(x)` over a JSON value: numbers truncate, strings parse as a
/// whole number, bools count as 0/1, anything else fails.
pub fn py_int_value(v: &Value) -> Result<i64, ()> {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else if let Some(f) = n.as_f64() {
                Ok(f as i64)
            } else {
                Err(())
            }
        }
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| ()),
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        _ => Err(()),
    }
}

/// Python `float(x)`: numbers pass, strings parse, bools count as 0/1.
pub fn py_float_value(v: &Value) -> Result<f64, ()> {
    match v {
        Value::Number(n) => n.as_f64().ok_or(()),
        Value::String(s) => s.trim().parse::<f64>().map_err(|_| ()),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(()),
    }
}

/// Python `str(float)` — "2.0" for a whole number, the way f-strings render it.
pub fn py_float_str(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("nan") {
        s
    } else {
        format!("{s}.0")
    }
}

/// Refuse a tool that cannot act on the page as a whole. [1] carries the
/// viewport rect, so a click aimed at it would land on whatever happens to
/// sit in the middle of the page — a silent wrong action rather than a
/// visible error.
pub fn reject_viewport(idx: i64, tool: &str) -> ActResult<()> {
    if idx == VIEWPORT_ID {
        return err(format!(
            "[1] is the page itself, not an element — `{tool}` needs a real \
             [id] from <element_tree>. Use [1] with `scroll` only."
        ));
    }
    Ok(())
}

/// Validate an [id] against the CURRENT scan, shared by every tool that takes
/// one. Ids are re-assigned on every scan, so an id carried over from an
/// earlier tree is a real mistake — catching it here turns it into a tool
/// error the model can recover from.
pub fn resolve_element_id(elements: &Map<String, Value>, raw: &Value) -> ActResult<i64> {
    let idx = match py_int_value(raw) {
        Ok(i) => i,
        Err(_) => {
            return err(format!("'{}' is not a valid element id", py_str_of(raw)));
        }
    };
    if !elements.is_empty() && !elements.contains_key(&idx.to_string()) {
        let mut known: Vec<i64> = elements
            .keys()
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()))
            .filter_map(|k| k.parse::<i64>().ok())
            .collect();
        known.sort_unstable();
        let rng = if known.is_empty() {
            "none".to_string()
        } else {
            format!("{}-{}", known[0], known[known.len() - 1])
        };
        return err(format!(
            "no element [{idx}] in the current element_tree \
             (available ids: {rng}). Ids are re-assigned on every scan — \
             re-read the latest tree and use an id from it."
        ));
    }
    Ok(idx)
}

/// The [x, y, w, h] of [id], in CSS pixels, from the scan the model saw.
pub fn rect_for(elements: &Map<String, Value>, idx: i64) -> ActResult<[f64; 4]> {
    let rect = elements
        .get(&idx.to_string())
        .and_then(|e| e.get("rect"))
        .and_then(Value::as_array);
    if let Some(vals) = rect {
        if vals.len() == 4 {
            let mut out = [0.0f64; 4];
            let mut ok = true;
            for (i, v) in vals.iter().enumerate() {
                match v.as_f64() {
                    Some(f) => out[i] = f,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return Ok(out);
            }
        }
    }
    err(format!(
        "element [{idx}] has no geometry in the current scan — \
         rescan before acting on it"
    ))
}

/// Short human label for an [id], for the action's result message.
pub fn label_for(elements: &Map<String, Value>, idx: i64) -> String {
    let info = elements.get(&idx.to_string());
    let name = ["name", "role", "tag"].iter().find_map(|k| {
        let v = info.and_then(|e| e.get(k))?;
        if truthy(v) {
            Some(py_str_of(v))
        } else {
            None
        }
    });
    match name {
        Some(n) => format!("({n})"),
        None => String::new(),
    }
}

/// Run `act` on the scanner without the GIL held — every page action is I/O.
pub fn scan_op<T: Send>(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    act: impl FnOnce(&mut ScannerInner) -> SResult<T> + Send,
) -> ActResult<T> {
    let scanner = scanner.clone();
    py.detach(move || {
        let mut guard = scanner
            .lock()
            .map_err(|_| ScanErr::s("scanner state poisoned by an earlier panic"))?;
        act(&mut guard)
    })
    .map_err(ActErr::from)
}

/// Walk the agent's cursor to `rect` (CSS px), press it, run `act`, then let
/// it back up. Shared by every tool that touches an element — "show the human
/// what is about to be hit" is one concern and lives once.
///
/// The ORDER is the point. The cursor travels and presses BEFORE the real
/// event is dispatched, so a human watching sees where the agent is going
/// while there is still time to stop it; a mark drawn afterwards can only
/// report what already happened. That costs about a second per action, which
/// is a deliberate trade: the agent is not racing anyone, and an action nobody
/// can follow is worse than a slow one.
///
/// Still best-effort. If the cursor cannot be drawn at all — a page that never
/// ran the overlay, a target that died mid-call — `cursor_to` says so and every
/// wait below is skipped, so the action runs at full speed rather than paying
/// a second for an animation that is not on screen.
pub fn with_cursor<T: Send>(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    rect: &[f64; 4],
    act: impl FnOnce(&mut ScannerInner) -> SResult<T> + Send,
) -> ActResult<T> {
    // 1. Send the arrow off, and give it the length of its own glide to land.
    let moved = {
        let scanner = scanner.clone();
        let rect = *rect;
        py.detach(move || match scanner.lock() {
            Ok(mut g) => g.cursor_to(&rect),
            Err(_) => false,
        })
    };
    // 2. Press, and hold the press long enough to actually be seen.
    if moved {
        let scanner = scanner.clone();
        py.detach(move || {
            std::thread::sleep(Duration::from_secs_f64(CURSOR_MOVE_SECONDS));
            if let Ok(mut g) = scanner.lock() {
                g.cursor_press(true);
            }
            std::thread::sleep(Duration::from_secs_f64(CURSOR_PRESS_SECONDS));
        });
    }
    // 3. The real action, dispatched while the arrow is still visibly down.
    let result = {
        let scanner = scanner.clone();
        py.detach(move || {
            let mut guard = scanner
                .lock()
                .map_err(|_| ScanErr::s("scanner state poisoned by an earlier panic"))?;
            act(&mut guard)
        })
    };
    // 4. Release — on the failure path too, or a tool that errors leaves the
    //    arrow pressed for the rest of the run.
    if moved {
        let scanner = scanner.clone();
        py.detach(move || {
            if let Ok(mut g) = scanner.lock() {
                g.cursor_press(false);
            }
        });
    }
    result.map_err(ActErr::from)
}

/// After every page action, the last of a step included, wait until the
/// page has taken it: one animation frame, then ACTION_QUIET_MS with no DOM
/// mutation and no CSS animation or transition still running, capped at
/// ACTION_SETTLE_CAP_MS. Whatever comes next reads the page — the next
/// action of a batch, or the scan that ends the step, whose own settle
/// counts network requests only and so cannot see a dialog closing on a
/// timer (see ControllerView::settle_after for the measured case).
/// An idle page answers in about a tenth of a second; a dropdown that opens
/// with a transition holds the next action until the transition is over,
/// which is when a page like Google Flights moves focus into the list and a
/// key pressed after the click reaches it (measured: keys sent before that
/// point are lost). A transition never touches the DOM, so the mutation
/// observer alone missed it; document.getAnimations() sees it. Anything
/// inside the overlay (the cursor's bubble, the glow's own animations) and
/// any infinite animation (a spinner) do not count. Best-effort: a page that
/// cannot run it, or a tab that is gone, costs nothing. Returns the page's
/// own account ("quiet 96ms" / "cap 600ms").
pub const ACTION_QUIET_MS: u64 = 80;
pub const ACTION_SETTLE_CAP_MS: u64 = 600;

pub fn settle_after_action(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>) -> Option<String> {
    let expression = format!(
        "new Promise(function (done) {{\
           var QUIET = {q}, CAP = {cap}, t0 = performance.now(), last = t0, timer = null, mo = null, over = false;\
           var layer = document.querySelector('[data-AutoCua=\"overlay\"]');\
           function finish(why) {{ if (over) return; over = true; if (mo) mo.disconnect(); clearTimeout(timer); done(why + ' ' + Math.round(performance.now() - t0) + 'ms'); }}\
           try {{\
             mo = new MutationObserver(function (records) {{\
               for (var i = 0; i < records.length; i++) {{\
                 if (!layer || !layer.contains(records[i].target)) {{ last = performance.now(); return; }}\
               }}\
             }});\
             mo.observe(document.documentElement, {{ childList: true, subtree: true, attributes: true, characterData: true }});\
           }} catch (e) {{}}\
           function animating() {{\
             try {{\
               var all = document.getAnimations ? document.getAnimations() : [];\
               for (var i = 0; i < all.length; i++) {{\
                 var a = all[i];\
                 if (a.playState !== 'running') continue;\
                 var target = a.effect && a.effect.target;\
                 if (target && layer && layer.contains(target)) continue;\
                 var timing = a.effect && a.effect.getTiming ? a.effect.getTiming() : null;\
                 if (timing && timing.iterations === Infinity) continue;\
                 return true;\
               }}\
             }} catch (e) {{}}\
             return false;\
           }}\
           function check() {{\
             var now = performance.now();\
             if (now - t0 >= CAP) return finish('cap');\
             if (animating()) {{ last = now; timer = setTimeout(check, 30); return; }}\
             if (now - last >= QUIET) return finish('quiet');\
             timer = setTimeout(check, Math.max(10, QUIET - (now - last)));\
           }}\
           requestAnimationFrame(function () {{ last = Math.max(last, performance.now()); check(); }});\
           setTimeout(function () {{ finish('cap'); }}, CAP + 50);\
         }})",
        q = ACTION_QUIET_MS,
        cap = ACTION_SETTLE_CAP_MS
    );
    let timeout = (ACTION_SETTLE_CAP_MS as f64 + 900.0) / 1000.0;
    scan_op(py, scanner, move |s| {
        if s.bridged() {
            // Extension mode: AutoCuaBridge runs the same wait in the page
            // (tools.js settle) and answers the page's own account.
            let r = s.bridge_call(serde_json::json!({"type": "settle"}))?;
            let value = r.get("settled").cloned().unwrap_or(Value::Null);
            return Ok(serde_json::json!({"result": {"value": value}}));
        }
        // This wait reads the socket; the requests it takes off the wire are
        // the scan's to wait for, not the bin's.
        s.keep_action_requests();
        s.with_tab(|cdp, sess| {
            cdp.rpc(
                "Runtime.evaluate",
                serde_json::json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
                Some(sess),
                timeout,
            )
        })
    })
    .ok()
    .and_then(|r| r.get("result").and_then(|v| v.get("value")).and_then(Value::as_str).map(str::to_string))
}

/// `release_all_inputs()` — kept because the agent loop calls it on every
/// stop path. Still a no-op, but for a better reason than before: the only
/// action that can leave a button pressed is `hold_click`, and its press and
/// release are now two calls in one synchronous function on this side's own
/// session (see controller/click). There is no longer a window in which the
/// button is down and this side has no way to lift it.
pub struct ControllerService;

impl ControllerService {
    pub fn release_all_inputs(&self) {}
}
