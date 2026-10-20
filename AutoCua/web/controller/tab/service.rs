//! Tab handling — every tab tool lives here.
//!
//! They are separate tools to the model because they are separate intents.
//! Underneath they are the same concern: which tab the agent is pointed at,
//! and what that tab is showing.
//!
//! Two wires, both owned by the browser side and borrowed here:
//!   - CDP, for moving a tab (navigate, reload, history) over its session,
//!     for opening one (Target.createTarget in the background, open_tab_impl,
//!     never /json/new, which brings the browser forward) and for showing
//!     one (show_tab: the browser's own next-tab shortcut over CDP on
//!     macOS, Chrome's next-tab command as a WM_COMMAND to the window on
//!     Windows, the browser's tabs API from a bookmarks tab of its own on
//!     Linux; never Page.bringToFront, which brings the browser forward as
//!     well);
//!   - Chrome's HTTP endpoint, for closing one (/json/close).
//! Whatever opened a tab, one list, the /json/list that <all_tabs> is
//! rendered from, decides what [n] means.

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::browser::{web_store_url, Cdp, CdpFail, SResult, ScanErr, ScannerInner, WEB_STORE_BLOCKED};
use crate::controller::service::{err, py_int_value, scan_op, ActResult};
use crate::agent::main_driver::view::py_str_of;

/// How long a navigation waits for the page's load event. A lapse is not an
/// error: the scanner settles the page again before the next scan, so a slow
/// site costs a wait, never a wrong tree.
const LOAD_TIMEOUT: f64 = 30.0;

/// Wait out the navigation just asked for: the load event, or a restore from
/// the back/forward cache, which brings the page back whole and fires none.
///
/// Buffered load events are cleared FIRST. They are not ours: an earlier
/// navigation on the same session leaves them behind, and one of those would
/// otherwise satisfy this wait instantly and hand back a tab still showing
/// the old page.
fn await_load(cdp: &mut Cdp, sess: &str) {
    cdp.wait_loaded(sess, LOAD_TIMEOUT);
}

fn navigate_to(cdp: &mut Cdp, sess: &str, url: &str) -> Result<(), CdpFail> {
    cdp.forget_loads(sess);
    let reply = cdp.rpc("Page.navigate", json!({"url": url}), Some(sess), 10.0)?;
    // Page.navigate answers successfully even when the navigation failed — the
    // failure is in `errorText`, and throwing the reply away made an
    // unreachable host indistinguishable from a live one. Chrome then renders
    // its OWN error page and fires an ordinary load event, so the wait below is
    // satisfied and the tool reported {"status":"success"} for a site that was
    // never coming. Because a batch only stops on a non-success status, this
    // was the one failure that structurally could not stop one.
    //
    // Note the deliberate asymmetry with the load wait below, and keep it: a
    // wait that lapses is NOT an error, because the scanner settles the page
    // again before the next scan, so a slow site costs a wait and never a
    // wrong tree. errorText is different. That page is never arriving.
    if let Some(text) = reply.get("errorText").and_then(Value::as_str) {
        if !text.is_empty() {
            return Err(CdpFail::Clean(format!("could not load {url}: {text}")));
        }
    }
    await_load(cdp, sess);
    Ok(())
}

/// Navigate the driven tab to `url` and wait out the load: over CDP here, or
/// by AutoCuaBridge in extension mode (tools.js navigate: the same errorText
/// check and the same load wait).
fn go_to(s: &mut ScannerInner, url: &str) -> SResult<()> {
    if s.bridged() {
        return s.bridge_call(json!({"type": "navigate", "url": url})).map(|_| ());
    }
    s.with_tab(|cdp, sess| navigate_to(cdp, sess, url))
}

fn reload_tab(cdp: &mut Cdp, sess: &str) -> Result<(), CdpFail> {
    cdp.forget_loads(sess);
    cdp.rpc("Page.reload", json!({}), Some(sess), 10.0)?;
    await_load(cdp, sess);
    Ok(())
}

/// Step through this tab's session history: -1 is back, +1 is forward.
///
/// Page.navigateToHistoryEntry rather than a synthetic Alt+Left: the key only
/// works when the page has focus and nothing on it swallows the shortcut,
/// while the history API is the browser's own move and reports honestly when
/// there is nowhere to go.
///
/// Refusing at the end of the history is deliberate. Silently doing nothing
/// would leave the model unable to tell "went back" from "was already at the
/// first page" — the same blindness the scroll tool had to solve, except here
/// the answer is known up front.
fn step_history(cdp: &mut Cdp, sess: &str, delta: i64) -> Result<(), CdpFail> {
    let unreadable = || CdpFail::Clean("could not read the tab's history".into());
    let h = cdp.rpc("Page.getNavigationHistory", json!({}), Some(sess), 5.0)?;
    let cur = h.get("currentIndex").and_then(Value::as_i64).ok_or_else(unreadable)?;
    let entries = h.get("entries").and_then(Value::as_array).ok_or_else(unreadable)?;
    let want = cur + delta;
    if want < 0 || want as usize >= entries.len() {
        return Err(CdpFail::Clean(format!(
            "no page to go {} to - this tab is at the {} of its history",
            if delta < 0 { "back" } else { "forward" },
            if delta < 0 { "start" } else { "end" }
        )));
    }
    let id = entries[want as usize]
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| CdpFail::Clean("history entry has no id".into()))?;
    cdp.forget_loads(sess);
    cdp.rpc("Page.navigateToHistoryEntry", json!({"entryId": id}), Some(sess), 10.0)?;
    await_load(cdp, sess);
    Ok(())
}

/// `[n]` against the listing the model was actually shown. `Some(message)`
/// when it is not a tab the model could have picked.
///
/// The check and the tab it guards now come from the SAME listing. They used
/// to come from two: this checked the rendered `<all_tabs>` text from the last
/// scan, and the caller then indexed a fresh /json/list. Chrome orders that
/// most-recently-used, so a tab merely being brought to the front reordered it
/// — and `[2]` could be bounds-checked against one tab and acted on another.
fn out_of_range(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    idx: i64,
) -> ActResult<Option<String>> {
    let n = scan_op(py, scanner, |s| Ok(s.tab_count()))? as i64;
    if n == 0 {
        // Nothing listed yet — let the resolve give the sharper error.
        return Ok(None);
    }
    if idx < 1 || idx > n {
        return Ok(Some(format!(
            "no tab [{idx}] — <all_tabs> lists {n} tab(s), 1-{n}. Use the [n] \
             from <all_tabs>, not an [id] from <element_tree>."
        )));
    }
    Ok(None)
}

/// The three ways to move the CURRENT tab without naming a destination.
pub const MOVES: [&str; 3] = ["back", "forward", "reload"];

/// A bare host into something Chrome will actually navigate to. The model
/// writes "amazon.com" as often as the full url, and Chrome treats a
/// scheme-less string as a search term rather than an address.
pub fn normalize_url(value: &Value) -> String {
    let url = if value.is_null() {
        String::new()
    } else {
        py_str_of(value).trim().to_string()
    };
    if !url.is_empty() && !url.contains("://") {
        format!("https://{url}")
    } else {
        url
    }
}

/// Open `value` in a new tab. A blank value opens the blank page.
pub fn open_new(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    value: &Value,
) -> ActResult<Value> {
    let url = normalize_url(value);
    // The Chrome Web Store is never opened, in either mode (the owner's rule).
    if web_store_url(&url) {
        return err(WEB_STORE_BLOCKED);
    }
    {
        let url = url.clone();
        scan_op(py, scanner, move |s| {
            // Blank-first, even when there IS a destination: the tab is
            // created empty and armed with the glow before anything
            // navigates, so its first real page glows from its first paint
            // instead of arriving bare and being dressed a beat later.
            s.create_tab()?;
            if url.is_empty() {
                s.show_blank_page()?;
                Ok(())
            } else {
                s.sent_to(&url);
                go_to(s, &url)
            }
        })?;
    }
    // Report what the model asked for. Saying "opened a new tab on
    // file:///.../logo.html" would read as a navigation it did not request.
    let where_ = if url.is_empty() { "a blank page" } else { url.as_str() };
    let message = format!("opened a new tab on {where_}");
    Ok(json!({"status": "success", "tool": "new_tab", "url": url, "message": message}))
}

/// Navigate the CURRENT tab to `value`, replacing its page.
pub fn update(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    value: &Value,
) -> ActResult<Value> {
    let url = normalize_url(value);
    if url.is_empty() {
        return err("update_tab needs a url in `value`");
    }
    if web_store_url(&url) {
        return err(WEB_STORE_BLOCKED);
    }
    {
        let url = url.clone();
        scan_op(py, scanner, move |s| {
            s.sent_to(&url);
            go_to(s, &url)
        })?;
    }
    let message = format!("navigated the current tab to {url}");
    Ok(json!({"status": "success", "tool": "update_tab", "url": url, "message": message}))
}

/// Move the CURRENT tab: `back`, `forward` or `reload`. Errors are worth more
/// than silence here: "there is no page to go back to" comes back as a failed
/// action the model can re-plan from, rather than a success that changed
/// nothing.
pub fn navigate(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    value: &Value,
) -> ActResult<Value> {
    let mv = if value.is_null() {
        String::new()
    } else {
        py_str_of(value).trim().to_lowercase()
    };
    if !MOVES.contains(&mv.as_str()) {
        return err(format!(
            "'{}' is not a tab move — use one of {}",
            py_str_of(value),
            MOVES.join(", ")
        ));
    }
    // The page being LEFT, read live. `s.url` is the url as of the last scan,
    // so after an update_tab in the same turn it names the page before that
    // one — and the model would read "went back from A" while sitting on A.
    let current = scan_op(py, scanner, |s| Ok(s.current_tab_url()))?;
    let before = if current.is_empty() {
        "the current tab".to_string()
    } else {
        current
    };

    let message = if mv == "reload" {
        scan_op(py, scanner, |s| {
            if s.bridged() {
                return s.bridge_call(json!({"type": "reload"})).map(|_| ());
            }
            s.with_tab(reload_tab)
        })?;
        format!("reloaded {before}")
    } else {
        {
            let delta = if mv == "back" { -1 } else { 1 };
            scan_op(py, scanner, move |s| {
                if s.bridged() {
                    // Extension mode: the same history step, refused at the
                    // ends with the same words (tools.js history).
                    return s.bridge_call(json!({"type": "history", "delta": delta})).map(|_| ());
                }
                s.with_tab(|cdp, sess| step_history(cdp, sess, delta))
            })?;
        }
        format!(
            "went {mv} from {before}. The tab is on a different \
             page now — read the new element_tree before acting."
        )
    };
    Ok(json!({"status": "success", "tool": "navigate_tab", "value": mv,
              "url": before, "message": message}))
}

/// Make the tab at `index` current. `index` is the [n] from <all_tabs>, NOT
/// an [id] from <element_tree> — bounds-check it against the tab list the
/// model was actually shown so a miss becomes a tool error it can recover
/// from.
pub fn switch(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    index: &Value,
) -> ActResult<Value> {
    let idx = match py_int_value(index) {
        Ok(i) => i,
        Err(_) => {
            return err(format!("'{}' is not a valid tab number", py_str_of(index)));
        }
    };
    if let Some(msg) = out_of_range(py, scanner, idx)? {
        return err(msg);
    }
    scan_op(py, scanner, move |s| {
        let target = s.tab_target(idx)?;
        s.bind_tab(&target)?;
        // A tab can be mid-load when we arrive — the human opened it a moment
        // ago, or it navigated itself. Scanning it now would hand the model
        // half a page and no way to tell. Waiting costs nothing on a tab that
        // has already finished: is_loading is false and this returns at once.
        if s.bridged() {
            return s.bridge_call(json!({"type": "wait_load"})).map(|_| ());
        }
        s.with_tab(|cdp, sess| {
            if cdp.is_loading(sess) {
                await_load(cdp, sess);
            }
            Ok(())
        })
    })?;
    Ok(json!({"status": "success", "tool": "switch_tab", "id": idx,
              "message": format!("switched to tab [{idx}]")}))
}

/// Close the tab at `index` — the [n] from <all_tabs>, bounds-checked against
/// the list the model was actually shown, exactly like `switch`. The scanner
/// refuses to close the LAST tab (an error the model can re-plan from: the
/// right move there is `update_tab`), and re-binds itself to a neighbouring
/// tab when the closed one was current. Closing RE-NUMBERS the tab list, so
/// the message says so — an id held over from before the close is a real
/// mistake, the same trap the element-tree ids have.
pub fn close(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    index: &Value,
) -> ActResult<Value> {
    let idx = match py_int_value(index) {
        Ok(i) => i,
        Err(_) => {
            return err(format!("'{}' is not a valid tab number", py_str_of(index)));
        }
    };
    if let Some(msg) = out_of_range(py, scanner, idx)? {
        return err(msg);
    }
    scan_op(py, scanner, move |s| {
        // Resolve BEFORE anything is closed, against the listing the model
        // read — closing is not undoable, so the one tab this may touch has to
        // be the one the model named.
        let victim = s.tab_target(idx)?;
        // The LAST tab is refused rather than closed: a browser with zero page
        // targets leaves the scanner nothing to bind to, and "close the last
        // tab" almost always means "navigate it".
        if s.open_tabs().len() <= 1 {
            return Err(ScanErr::s("cannot close the last tab - navigate it instead"));
        }
        let was_current = s.current_target_id().map(|id| id == victim).unwrap_or(false);
        if !s.close_tab_target(&victim)? {
            return Err(ScanErr::s("tab did not close"));
        }
        // The session died with the tab.
        s.forget_tab(&victim);
        if was_current {
            // Re-bind the way a browser does: to the neighbour that held the
            // next slot in the list the model was shown.
            if let Some(id) = s.neighbour_tab(idx, &victim) {
                s.bind_tab(&id)?;
            }
        }
        // Closing RE-NUMBERS the list, so re-take it now: `[n]` must not keep
        // resolving against a listing that still contains the closed tab.
        s.refresh_tabs();
        Ok(())
    })?;
    Ok(json!({"status": "success", "tool": "close_tab", "id": idx,
              "message": format!(
                  "closed tab [{idx}]. The remaining tabs are re-numbered — \
                   read the fresh <all_tabs> in the next input before any \
                   other tab action.")}))
}
