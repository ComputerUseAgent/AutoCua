//! Running a script inside a tab — the `run_script` tool.
//!
//! The ONE place this crate runs code in a page, and only when the model asks
//! for it: a single Runtime.evaluate on the named tab's session. Nothing is
//! installed, nothing persists, and `Runtime.enable` is still never called —
//! the console-getter family of bot detection keys on that, not on one
//! evaluate. The result comes back as data, capped so one careless
//! `outerHTML` cannot flood a step.
//!
//! Two types, named in the call's `type`: "action" for DOM-level moves such
//! as focusing a field, "scrape" for reading what the page holds (records,
//! text, attributes, tables, application state) when the element tree is
//! not enough. What it is not for: typing or clicking.
//! Events made by page JavaScript are untrusted and apps such as Google
//! Sheets ignore them; trusted input is `click`, `keyboard` and `input`.

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::agent::main_driver::view::py_str_of;
use crate::browser::{Cdp, CdpFail, ScannerInner};
use crate::controller::service::{err, py_int_value, scan_op, ActResult};

// Every guardrail of this tool, in one file beside this one: checked before a
// script runs and on the reply before the response is packed, for both
// types. The path attribute keeps the file in this folder and mod.rs
// untouched.
#[path = "guardrails_check.rs"]
pub mod guardrails_check;

// The read of type "scrape" (three screens, their pictures, `more`), in a
// file of its own beside this one.
#[path = "window.rs"]
pub mod window;

/// Longest result handed back to the model, in characters.
pub const MAX_RESULT_CHARS: usize = 30_000;
/// Where the page cuts a result before the guardrails see it (port mode).
/// Larger than MAX_RESULT_CHARS so the screen reads the whole text and a
/// token on the 30000 boundary is not half a token; capped() cuts after.
pub const PREFILTER_CHARS: usize = 256_000;
/// A script still running after this is stopped.
pub const SCRIPT_TIMEOUT_MS: u64 = 10_000;

/// Turns whatever the script produced into data, IN the page: DOM nodes,
/// windows and functions become a short description, NodeLists become
/// arrays, undefined becomes null, Map/Set/Date/Error and anything with a
/// toJSON (DOMRect, URL) become their plain form, cycles and very deep
/// nesting are cut, and a value that cannot be read (a cross-origin window,
/// a throwing getter) is named rather than allowed to fail the whole result.
/// Run through callFunctionOn on the result object, so the model always
/// receives JSON. A size cap is applied HERE too, so a huge result is
/// trimmed before it crosses the socket rather than after: at
/// PREFILTER_CHARS, wide enough for the guardrails to screen the whole text,
/// then capped() cuts to MAX_RESULT_CHARS for the model.
const TO_DATA: &str = r#"function () {
  const path = new WeakSet();
  const isNode = v => typeof v.nodeType === 'number' && typeof v.nodeName === 'string';
  const conv = (v, depth) => {
    try {
      if (v === undefined || v === null) return null;
      const t = typeof v;
      if (t === 'string' || t === 'number' || t === 'boolean') return v;
      if (t === 'bigint' || t === 'symbol') return String(v);
      if (t === 'function') return '<function ' + (v.name || 'anonymous') + '> is not data';
      if (typeof v.document === 'object' && v.window === v) return '<Window> is not data';
      if (isNode(v)) {
        const name = (v.constructor && v.constructor.name) || v.nodeName;
        return '<' + name + (v.id ? '#' + v.id : '') + '> is not data - map it to text/attributes first';
      }
      if (depth > 8) return '<nested too deep>';
      if (path.has(v)) return '<circular>';
      path.add(v);
      try {
        if (Array.isArray(v) || (typeof v.length === 'number' && typeof v.item === 'function')) {
          return Array.from(v, x => conv(x, depth + 1));
        }
        if (v instanceof Map) return Object.fromEntries(Array.from(v, ([k, x]) => [String(k), conv(x, depth + 1)]));
        if (v instanceof Set) return Array.from(v, x => conv(x, depth + 1));
        if (v instanceof Error) return v.name + ': ' + v.message;
        if (typeof v.toJSON === 'function') return conv(v.toJSON(), depth + 1);
        const out = {};
        for (const k of Object.keys(v)) {
          let x;
          try { x = v[k]; } catch (e) { out[k] = '<unreadable: ' + ((e && e.message) || e) + '>'; continue; }
          out[k] = conv(x, depth + 1);
        }
        return out;
      } finally {
        path.delete(v);
      }
    } catch (e) {
      return '<unreadable: ' + ((e && e.message) || e) + '>';
    }
  };
  const data = conv(this, 0);
  const text = typeof data === 'string' ? data : JSON.stringify(data);
  if (text !== undefined && text.length > __MAX__) return { __cut: text.length, text: text.slice(0, __MAX__) };
  return data;
}"#;

/// The code as a bare expression: trailing comment lines and a comment after
/// the final semicolon are dropped (models add them by habit), then the
/// semicolon itself.
fn expression_form(code: &str) -> String {
    let mut s = code.trim();
    loop {
        let t = s.trim_end();
        match t.rfind('\n') {
            Some(i) if t[i + 1..].trim_start().starts_with("//") => s = &t[..i],
            _ => break,
        }
    }
    if let Some(i) = s.rfind("//") {
        let (head, tail) = (s[..i].trim_end(), &s[i..]);
        if head.ends_with(';') && !tail.contains('\n') && !tail.contains(['\'', '"', '`']) {
            s = head;
        }
    }
    s.trim().trim_end_matches(';').trim().to_string()
}

/// Decide the shape WITHOUT running anything. `new Function` parses the
/// parenthesised code and throws a SyntaxError for anything that is not a
/// single expression — statements, `return`, top-level `await`, a `const`
/// clashing with a page global — while executing none of it. A runtime
/// SyntaxError (JSON.parse on an HTML reply) therefore cannot be mistaken for
/// a parse error and cause a second run. DevTools evaluations are exempt
/// from the page's CSP, so the probe works on strict pages too.
fn is_expression(cdp: &mut Cdp, sess: &str, expression: &str) -> Result<bool, CdpFail> {
    let literal = serde_json::to_string(&format!("(\n{expression}\n)")).unwrap_or_default();
    let reply = cdp.rpc(
        "Runtime.evaluate",
        json!({"expression": format!("(new Function({literal}), true)"), "returnByValue": true}),
        Some(sess),
        5.0,
    )?;
    Ok(reply.get("exceptionDetails").is_none())
}

/// Evaluate `code` in the tab, exactly once, and hand back Chrome's reply
/// with any object result already converted to data.
///
/// Two shapes, one rule the model can hold: a single EXPRESSION evaluates to
/// its value (`document.title`); anything else runs inside an async function
/// body, where `return` hands the value back. Both are raced against an
/// in-page timer, so awaited work that never settles is reported at the
/// same deadline as a synchronous loop, which Chrome's own `timeout` stops.
fn evaluate(cdp: &mut Cdp, sess: &str, code: &str) -> Result<Value, CdpFail> {
    let expression = expression_form(code);
    let body = if is_expression(cdp, sess, &expression)? {
        format!("(\n{expression}\n)")
    } else {
        format!("(async () => {{\n{code}\n}})()")
    };
    let secs = SCRIPT_TIMEOUT_MS / 1000;
    let raced = format!(
        "Promise.race([{body}, new Promise((_, reject) => setTimeout(() => \
         reject(new Error('stopped after {secs}s without finishing - work that never settles')), \
         {SCRIPT_TIMEOUT_MS}))])"
    );
    // No user activation: a read never needs one, and with it a script could
    // read the clipboard, open popups or start a download unattended.
    let params = json!({
        "expression": raced,
        "returnByValue": false,
        "awaitPromise": true,
        "userGesture": false,
        "timeout": SCRIPT_TIMEOUT_MS,
    });
    let budget = SCRIPT_TIMEOUT_MS as f64 / 1000.0 + 5.0;
    let reply = match cdp.rpc("Runtime.evaluate", params, Some(sess), budget) {
        // Chrome stops a synchronous loop at `timeout` and answers with a bare
        // internal error. A loop that starts AFTER an await is outside that
        // guard and wedges the renderer until we time out ourselves — so end
        // it explicitly, which frees the tab for the next action.
        Err(CdpFail::Clean(m)) if m.contains("Internal error") || m.contains("timed out") => {
            let _ = cdp.rpc("Runtime.terminateExecution", json!({}), Some(sess), 5.0);
            return Err(CdpFail::Clean(format!(
                "stopped after {secs}s without finishing - an endless loop"
            )));
        }
        other => other?,
    };
    if exception_text(&reply).is_some() {
        return Ok(reply);
    }
    // Objects come back as a reference — turn them into data where they live.
    let object_id = reply
        .get("result")
        .and_then(|r| r.get("objectId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(oid) = object_id {
        let data = cdp.rpc(
            "Runtime.callFunctionOn",
            json!({
                "objectId": oid,
                "functionDeclaration": TO_DATA.replace("__MAX__", &PREFILTER_CHARS.to_string()),
                "returnByValue": true,
            }),
            Some(sess),
            15.0,
        )?;
        let _ = cdp.rpc("Runtime.releaseObject", json!({"objectId": oid}), Some(sess), 5.0);
        return Ok(data);
    }
    Ok(reply)
}

/// The first line of what the script threw — an Error's description, a
/// thrown string or number as itself, or Chrome's own summary.
fn exception_text(reply: &Value) -> Option<String> {
    let ex = reply.get("exceptionDetails")?;
    let thrown = ex.get("exception");
    let text = thrown
        .and_then(|e| e.get("description"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| thrown.and_then(|e| e.get("value")).map(py_str_of))
        .or_else(|| thrown.and_then(|e| e.get("unserializableValue")).and_then(Value::as_str).map(str::to_string))
        .or_else(|| ex.get("text").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "script threw".to_string());
    Some(text.lines().next().unwrap_or(&text).to_string())
}

/// The value in a reply: a plain value, an unserializable one (NaN, 10n) as
/// text, or null for undefined.
fn value_of(reply: &Value) -> Value {
    let r = reply.get("result").cloned().unwrap_or(Value::Null);
    if let Some(v) = r.get("value") {
        return v.clone();
    }
    if let Some(u) = r.get("unserializableValue").and_then(Value::as_str) {
        return json!(u);
    }
    Value::Null
}

/// Cap the result. Returns the value to report and how many characters were
/// dropped (0 when it fit). A result the page already trimmed arrives as
/// {__cut, text}; a primitive is trimmed here.
fn capped(value: Value) -> (Value, usize) {
    if let (Some(total), Some(text)) = (
        value.get("__cut").and_then(Value::as_u64),
        value.get("text").and_then(Value::as_str),
    ) {
        // The page cut at PREFILTER_CHARS (or the extension at 30000); the
        // model's cap is still MAX_RESULT_CHARS.
        let kept: String = text.chars().take(MAX_RESULT_CHARS).collect();
        let dropped = (total as usize).saturating_sub(kept.chars().count());
        return (json!(kept), dropped);
    }
    let text = match &value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let total = text.chars().count();
    if total <= MAX_RESULT_CHARS {
        return (value, 0);
    }
    let kept: String = text.chars().take(MAX_RESULT_CHARS).collect();
    (Value::String(kept), total - MAX_RESULT_CHARS)
}

/// run_script of type "action": do something in the page the ordinary tools
/// cannot reach (focus a field, take away what covers a control), or take a
/// small look at how the page is built.
pub fn action(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    code: &Value,
) -> ActResult<Value> {
    run(py, scanner, raw_id, code)
}

/// run_script of type "scrape": read the data the task needs out of the page,
/// three screens from where it is scrolled (window.rs), with a clean picture
/// of each screen for the next input. A read that brings records opens scrape
/// mode: scraping mode's look goes on the tab, the records stream into its
/// card, and the result carries `scrape_mode`, which the agent loop opens the
/// mode on. The mode lasts until `exit_scrape_mode`; `more` reads the next
/// three screens with the same script.
pub fn scrape(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    code: &Value,
) -> ActResult<Value> {
    let idx = match py_int_value(raw_id) {
        Ok(i) => i,
        Err(_) => return err(format!("'{}' is not a valid tab number", py_str_of(raw_id))),
    };
    let source = if code.is_null() { String::new() } else { py_str_of(code) };
    if source.trim().is_empty() {
        return err("run_script needs JavaScript in `value`");
    }
    let Some(tab) = scan_op(py, scanner, move |s| Ok(s.listed_tab(idx)))? else {
        return err(format!(
            "tab [{idx}] is not on the tab list - read the fresh <all_tabs> before acting on a tab"
        ));
    };
    read_screens(py, scanner, idx, &tab, &source, None, "run_script")
}

/// `more`, in scrape mode: the next three screens of the page being read,
/// with the same script, and their pictures.
pub fn more(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>) -> ActResult<Value> {
    let Some((tab, code, next_top, ended)) = window::current() else {
        return err("no read is in progress: run_script with type \"scrape\" reads first, then `more` goes on");
    };
    if ended {
        return err("the page ends at the last screen read: there is nothing below it - exit_scrape_mode");
    }
    let target = tab.clone();
    let idx = scan_op(py, scanner, move |s| {
        let mut n = 1;
        Ok(loop {
            match s.listed_tab(n) {
                Some(t) if t == target => break Some(n),
                Some(_) => n += 1,
                None => break None,
            }
        })
    })?;
    let Some(idx) = idx else {
        return err("the tab being read is gone - exit_scrape_mode");
    };
    read_screens(py, scanner, idx, &tab, &code, Some(next_top), "more")
}

/// One read, from the guardrails to the packed answer: the script checked
/// before it runs, three screens read, the size held to MAX_READ_TOKENS, the
/// records screened before they are packed.
fn read_screens(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    idx: i64,
    tab: &str,
    code: &str,
    top: Option<f64>,
    tool: &str,
) -> ActResult<Value> {
    let gate = match guardrails_check::before(py, scanner, idx, code) {
        Ok(gate) => gate,
        Err(message) => return err(message),
    };
    let w = window::read(py, scanner, tab, code, top)?;
    let tokens = w.chars / window::CHARS_PER_TOKEN;
    if w.cut || tokens > window::MAX_READ_TOKENS {
        window::back_to(py, scanner, tab, w.start_top);
        let message = format!(
            "result too long: about {tokens} tokens ({} characters) from {} screen(s) of tab [{idx}], and one \
             read hands back at most {} tokens. Nothing was delivered and the page is back where this read \
             began. Narrow the script: fewer fields, shorter text (.slice), a narrower selector. A clean \
             screenshot of each screen comes with the next input.",
            w.chars,
            w.screens,
            window::MAX_READ_TOKENS
        );
        let none = guardrails_check::Report::default();
        guardrails_check::audit(&gate, "too long", &none, Some(&message), w.chars, w.chars);
        return err(message);
    }
    let (value, report) = guardrails_check::screen_result(Value::Array(w.records));
    if let Some(reason) = report.withheld.clone() {
        guardrails_check::audit(&gate, "withheld", &report, Some(&reason), report.chars, 0);
        return err(format!("tab [{idx}]: {reason}"));
    }
    let n = value.as_array().map(Vec::len).unwrap_or(0);
    let mut message = if n == 0 {
        format!(
            "read {} screen(s) of tab [{idx}]: nothing matched there - check the selector against the \
             screenshots that come with the next input",
            w.screens
        )
    } else {
        let end = if w.ended { ", and the page ends there" } else { "" };
        format!("read {} screen(s) of tab [{idx}]: {n} record(s){end}", w.screens)
    };
    if report.redacted() > 0 {
        message.push_str(" - ");
        message.push_str(&report.note());
    }
    guardrails_check::audit(&gate, "ok", &report, None, report.chars, 0);
    // `more` goes on from here even past screens without records; a first
    // read that found nothing opens no mode, so nothing goes on from it.
    if tool == "more" || n > 0 {
        window::remember(tab, code, w.next_top, w.ended);
    }
    let mut out = json!({
        "status": "success", "tool": tool, "id": idx, "result": value,
        "read": {"screens": w.screens, "records": n, "more_below": !w.ended},
        "message": message,
    });
    if report.redacted() > 0 {
        out["redacted"] = report.classes_json();
    }
    if n > 0 {
        let text = serde_json::to_string_pretty(&out["result"]).unwrap_or_default();
        let target = tab.to_string();
        // Decoration: a tab that cannot take the look still opens the mode.
        let _ = scan_op(py, scanner, move |s| {
            s.scrape_glow_on(&target);
            s.scrape_feed(&text);
            Ok(())
        });
        let next = if w.ended {
            "the page ends here, so exit_scrape_mode next"
        } else {
            "`more` reads the next 3 screens"
        };
        out["message"] = json!(format!(
            "{message} - scrape mode is on: a clean screenshot of each screen comes with the next input; {next}"
        ));
        out["scrape_mode"] = json!(true);
    }
    Ok(out)
}

/// Leave scrape mode. `value` is the model's closing note. The agent loop
/// takes the look off and scans the page again from the next step.
pub fn exit_scrape_mode(value: &Value) -> ActResult<Value> {
    window::end();
    let note = if value.is_null() { String::new() } else { py_str_of(value).trim().to_string() };
    let message = format!("scrape mode left. {note}");
    Ok(json!({"status": "success", "tool": "exit_scrape_mode", "message": message.trim()}))
}

/// Run `code` in tab [index] and report what it returned: both types.
fn run(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    code: &Value,
) -> ActResult<Value> {
    let idx = match py_int_value(raw_id) {
        Ok(i) => i,
        Err(_) => return err(format!("'{}' is not a valid tab number", py_str_of(raw_id))),
    };
    let source = if code.is_null() { String::new() } else { py_str_of(code) };
    if source.trim().is_empty() {
        return err("run_script needs JavaScript in `value`");
    }
    // Guardrails before anything runs: the tab, the page, the script text.
    let gate = match guardrails_check::before(py, scanner, idx, &source) {
        Ok(gate) => gate,
        Err(message) => return err(message),
    };
    let reply = scan_op(py, scanner, move |s| {
        if s.bridged() {
            // Extension mode: the same evaluate, run by AutoCuaBridge (tools.js)
            // and answered in Chrome's shape, read below as CDP's reply is.
            return s.bridge_call_at(idx, json!({"type": "run_script", "code": source}));
        }
        s.with_tab_at(idx, |cdp, sess| evaluate(cdp, sess, &source))
    })?;
    if let Some(text) = exception_text(&reply) {
        // The error channel carries data too: screened and capped like a result.
        let (text, report) = guardrails_check::screen_error(&text);
        guardrails_check::audit(&gate, "error", &report, Some(&text), 0, 0);
        return err(format!("script error in tab [{idx}]: {text}"));
    }
    // Guardrails on the reply, before the response is packed.
    let (value, report) = guardrails_check::screen_result(value_of(&reply));
    if let Some(reason) = report.withheld.clone() {
        guardrails_check::audit(&gate, "withheld", &report, Some(&reason), report.chars, 0);
        return err(format!("tab [{idx}]: {reason}"));
    }
    let (result, dropped) = capped(value);
    let mut message = if dropped > 0 {
        format!(
            "ran in tab [{idx}] - result cut at {MAX_RESULT_CHARS} characters ({dropped} more \
             dropped): return less - pick fields, slice text, or page through it"
        )
    } else if result.is_null() {
        format!(
            "ran in tab [{idx}] - result is null: nothing matched, or, in code with statements, \
             nothing was returned (`return` hands a value back)"
        )
    } else {
        format!("ran in tab [{idx}]")
    };
    if report.redacted() > 0 {
        message.push_str(" - ");
        message.push_str(&report.note());
    }
    guardrails_check::audit(&gate, "ok", &report, None, report.chars, dropped);
    let mut out = json!({"status": "success", "tool": "run_script", "id": idx,
                         "result": result, "message": message});
    if report.redacted() > 0 {
        out["redacted"] = report.classes_json();
    }
    Ok(out)
}
