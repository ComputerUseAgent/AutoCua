//! The read of run_script's type "scrape": three screens from where the page
//! is scrolled. On each screen the page is photographed with AutoCua's
//! overlay hidden, then the model's script runs once with
//! `document.querySelectorAll` held to the elements whose top edge is on that
//! screen; the three answers are joined. `more` reads the next three screens
//! with the same script. The pictures go with the next input only
//! (take_shots), and the page is left at the last screen read.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::browser::ScannerInner;
use crate::controller::service::{err, scan_op, settle_after_action, ActResult};

/// Screens per read; `more` reads as many again.
pub const SCREENS: usize = 3;
/// The most one read hands back, in tokens, counted as characters / 3 (JSON
/// is dense). Over it nothing is delivered and the model narrows its script.
pub const MAX_READ_TOKENS: usize = 10_000;
pub const CHARS_PER_TOKEN: usize = 3;

/// Where a read stands, for `more`: the tab, the script, and the top of the
/// next screen. One read at a time; a new scrape replaces it.
struct Read {
    tab: String,
    code: String,
    next_top: f64,
    ended: bool,
}

static READ: Mutex<Option<Read>> = Mutex::new(None);
/// The latest read's pictures (JPEG base64, top to bottom), until the agent
/// loop takes them for the next input.
static SHOTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A run starts with no read and no pictures.
pub fn new_run() {
    if let Ok(mut r) = READ.lock() {
        *r = None;
    }
    if let Ok(mut s) = SHOTS.lock() {
        s.clear();
    }
}

/// The pictures of the latest read, once: the agent loop sends them with the
/// next input, and they are gone after it.
pub fn take_shots() -> Vec<String> {
    SHOTS.lock().map(|mut s| std::mem::take(&mut *s)).unwrap_or_default()
}

/// Scrape mode is over: `more` has nothing to go on with.
pub fn end() {
    if let Ok(mut r) = READ.lock() {
        *r = None;
    }
}

/// Keep a read for `more`: its tab, its script and where the next screen starts.
pub fn remember(tab: &str, code: &str, next_top: f64, ended: bool) {
    if let Ok(mut r) = READ.lock() {
        *r = Some(Read { tab: tab.to_string(), code: code.to_string(), next_top, ended });
    }
}

/// The read `more` goes on with: (tab, script, next top, page ended).
pub fn current() -> Option<(String, String, f64, bool)> {
    let r = READ.lock().ok()?;
    r.as_ref().map(|r| (r.tab.clone(), r.code.clone(), r.next_top, r.ended))
}

/// What one read gathered.
pub struct Window {
    pub records: Vec<Value>,
    pub screens: usize,
    /// Where the read began and where the next one begins (document y).
    pub start_top: f64,
    pub next_top: f64,
    /// The page has nothing below the last screen read.
    pub ended: bool,
    /// A screen's answer was too big for the page to hand over whole.
    pub cut: bool,
    /// The size of what was gathered, in characters of JSON.
    pub chars: usize,
}

/// Read three screens of `tab` with `code`, from `top` (None: from where the
/// page is scrolled now). The overlay is hidden while the screens are
/// photographed and comes back whatever happens; the pictures are kept for
/// the next input even when the read fails or is too long.
pub fn read(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    tab: &str,
    code: &str,
    top: Option<f64>,
) -> ActResult<Window> {
    let mut shots: Vec<String> = Vec::new();
    let mut w = Window {
        records: Vec::new(),
        screens: 0,
        start_top: 0.0,
        next_top: 0.0,
        ended: false,
        cut: false,
        chars: 0,
    };
    let mut seen: HashSet<String> = HashSet::new();
    let outcome = (|| -> ActResult<()> {
        let mut want = top;
        let mut last_y = f64::NEG_INFINITY;
        for i in 0..SCREENS {
            let at = eval(py, scanner, tab, &scroll_js(want))?;
            let y = num(&at, "y");
            let h = num(&at, "h").max(1.0);
            let full = num(&at, "full");
            if i == 0 {
                w.start_top = y;
            } else if y <= last_y + 1.0 {
                // The page did not move: there is no screen below.
                w.ended = true;
                break;
            }
            last_y = y;
            settle_after_action(py, scanner);
            let target = tab.to_string();
            if let Ok(Some(shot)) = scan_op(py, scanner, move |s| Ok(s.frame_of(&target))) {
                shots.push(shot);
            }
            let reply = run_on_screen(py, scanner, tab, code, y, y + h)?;
            if let Some(text) = super::exception_text(&reply) {
                return err(format!("script error on screen {}: {text}", i + 1));
            }
            match super::value_of(&reply) {
                Value::Object(o) if o.contains_key("__cut") => {
                    w.cut = true;
                    w.chars += o.get("__cut").and_then(Value::as_u64).unwrap_or(0) as usize;
                }
                Value::Null => {}
                value => {
                    let items = match value {
                        Value::Array(a) => a,
                        other => vec![other],
                    };
                    for item in items {
                        // map gives null for an element that is not a record.
                        if item.is_null() {
                            continue;
                        }
                        let key = item.to_string();
                        if seen.insert(key.clone()) {
                            w.chars += key.chars().count() + 1;
                            w.records.push(item);
                        }
                    }
                }
            }
            w.screens += 1;
            w.next_top = y + h;
            want = Some(w.next_top);
            if w.next_top >= full - 1.0 {
                w.ended = true;
                break;
            }
        }
        Ok(())
    })();
    let _ = eval(py, scanner, tab, SHOW_OVERLAY);
    if let Ok(mut s) = SHOTS.lock() {
        *s = shots;
    }
    outcome?;
    Ok(w)
}

/// Put the page back where a read began: a read that delivered nothing
/// leaves the next one the same screens.
pub fn back_to(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>, tab: &str, top: f64) {
    let _ = eval(py, scanner, tab, &scroll_js(Some(top)));
    let _ = eval(py, scanner, tab, SHOW_OVERLAY);
}

const SHOW_OVERLAY: &str =
    "(() => { const ov = document.querySelector('[data-AutoCua=\"overlay\"]'); if (ov) ov.style.visibility = ''; return true; })()";

/// Hide the overlay (the pictures are of the page alone), scroll to `top`
/// (None: stay), and say where the page is: {y, h, full}.
fn scroll_js(top: Option<f64>) -> String {
    let to = top.map(|t| t.to_string()).unwrap_or_else(|| "null".to_string());
    format!(
        "(() => {{ const ov = document.querySelector('[data-AutoCua=\"overlay\"]'); \
         if (ov) ov.style.visibility = 'hidden'; const T = {to}; \
         if (T !== null) window.scrollTo({{top: T, left: window.scrollX, behavior: 'instant'}}); \
         const se = document.scrollingElement || document.documentElement; \
         return {{y: window.scrollY, h: window.innerHeight, \
         full: Math.max(se.scrollHeight, document.body ? document.body.scrollHeight : 0)}}; }})()"
    )
}

fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// One of this module's own small scripts in `tab`, its value back.
fn eval(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>, tab: &str, expression: &str) -> ActResult<Value> {
    let tab = tab.to_string();
    let expression = expression.to_string();
    let reply = scan_op(py, scanner, move |s| {
        if s.bridged() {
            return s.bridge_call_on(&tab, json!({"type": "run_script", "code": expression}));
        }
        s.with_tab_on(&tab, |cdp, sess| {
            cdp.rpc(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
                Some(sess),
                10.0,
            )
        })
    })?;
    if let Some(text) = super::exception_text(&reply) {
        return err(format!("the page could not be scrolled for the read: {text}"));
    }
    Ok(super::value_of(&reply))
}

/// The model's script on one screen, run the way an action runs (same
/// evaluate, same limits), with `document.querySelectorAll` held to the
/// elements whose top edge lies in [top, bottom).
fn run_on_screen(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    tab: &str,
    code: &str,
    top: f64,
    bottom: f64,
) -> ActResult<Value> {
    let wrapped = on_screen(code, top, bottom);
    let tab = tab.to_string();
    scan_op(py, scanner, move |s| {
        if s.bridged() {
            return s.bridge_call_on(&tab, json!({"type": "run_script", "code": wrapped}));
        }
        s.with_tab_on(&tab, |cdp, sess| super::evaluate(cdp, sess, &wrapped))
    })
}

/// The script, held to one screen. It is parsed the way an action is (a
/// single expression gives its value, anything else is a function body), and
/// `document.querySelectorAll` answers only with elements whose top edge is
/// on the screen: an element without a box of its own (display: contents)
/// is placed by its first child that has one, a hidden one is left out, and
/// so is anything of AutoCua's overlay. The page's own method is back the
/// moment the script ends.
fn on_screen(code: &str, top: f64, bottom: f64) -> String {
    let expr = serde_json::to_string(&super::expression_form(code)).unwrap_or_default();
    let body = serde_json::to_string(code).unwrap_or_default();
    format!(
        r#"const __AutoCuaQsa = Document.prototype.querySelectorAll;
const __AutoCuaTop = {top}, __AutoCuaBottom = {bottom};
const __AutoCuaAt = el => {{
  for (const box of [el, ...el.children]) {{
    const r = box.getBoundingClientRect();
    if (r.width || r.height) return r.top + window.scrollY;
  }}
  return null;
}};
let __AutoCuaRun;
try {{ __AutoCuaRun = new Function('return (async () => (\n' + {expr} + '\n))')(); }}
catch (e) {{ __AutoCuaRun = new Function('return (async () => {{\n' + {body} + '\n}})')(); }}
document.querySelectorAll = function (selector) {{
  return Array.from(__AutoCuaQsa.call(this, selector)).filter(el => {{
    if (el.closest('[data-AutoCua]')) return false;
    const t = __AutoCuaAt(el);
    return t !== null && t >= __AutoCuaTop && t < __AutoCuaBottom;
  }});
}};
try {{ return await __AutoCuaRun(); }} finally {{ delete document.querySelectorAll; }}"#
    )
}
