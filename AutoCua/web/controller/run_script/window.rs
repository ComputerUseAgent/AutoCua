//! The read of run_script's type "scrape": three screens from where the page
//! is scrolled (or, on an app whose page does not scroll, its main panel). On
//! each screen the page is photographed with AutoCua's overlay hidden, then
//! the model's script runs once with `document.querySelectorAll` held to the
//! elements whose top edge lies below the last element already read and above
//! the screen's bottom; the answers are joined. The read keeps its place (a
//! Spot): after every read the page is left with the next unread point at the
//! top of the screen, and `more` goes on from exactly there, so no record
//! comes twice and none is skipped. A script that never calls
//! `document.querySelectorAll` is a pick: read once, from what the page has
//! loaded. The pictures go with the next input only (take_shots).
//!
//! Also the run's INDEXES: what the agent keeps from each chunk, through the
//! `index` tool of scrape mode. Each one is a file, AutoCua_data/scrapes/
//! <run>/<n>.json, numbered by this module from 1, holding the time, the
//! agent's title and pending lines and its value. The live <scraped_index>
//! block lists them (index_lines), and `index_read` brings one back
//! (read_index).

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
/// How long a read at the bottom of the page waits for it to grow (an
/// infinite feed loading its next batch) before it says the page ends.
const GROW_MS: u64 = 1500;

/// Where a read stands, in the coordinates of the pane it scrolls (document y
/// for the page): `top` is where the next screen begins, the bottom of the
/// last one read; `after` is the top edge of the last row of records read,
/// and `edge` what the elements of that row look like, so the next screen
/// finds the row again (what lies above it may have moved since) and takes
/// only what is below it, or new in it (a grid's next cards). `pane` is the
/// selector of the records, which finds the pane: the page, or the panel or
/// dialog list that holds them.
#[derive(Clone)]
pub struct Spot {
    pub top: f64,
    pub after: f64,
    pub edge: Vec<String>,
    pub pane: Option<String>,
}

/// The read in progress, for `more`: the tab, the script, where it goes on,
/// and whether it was a pick. One read at a time; a new scrape replaces it.
struct Read {
    tab: String,
    code: String,
    next: Spot,
    pick: bool,
}

static READ: Mutex<Option<Read>> = Mutex::new(None);
/// The latest read's pictures (JPEG base64, top to bottom), until the agent
/// loop takes them for the next input.
static SHOTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// One stored index: its number and the two lines the agent gave it. The
/// value is in the file only.
#[derive(Clone)]
struct IndexEntry {
    n: usize,
    title: String,
    pending: String,
}

/// This run's scrapes folder, AutoCua_data/scrapes/<run>/, set by the agent
/// loop when the run starts (new_run) and made on the first `index`. None
/// when AutoCua_data could not be found: `index` then fails and says so.
static RUN_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// This run's indexes, in order.
static INDEXES: Mutex<Vec<IndexEntry>> = Mutex::new(Vec::new());
/// How many indexes existed when the current scrape mode opened. The ones
/// after that mark are the mode's own: on exit they replace its read in the
/// history (main_driver point_scrape_at_indexes).
static MODE_MARK: Mutex<usize> = Mutex::new(0);

/// A run starts with no read, no pictures and no index; `dir` is where its
/// indexes will go.
pub fn new_run(dir: Option<PathBuf>) {
    if let Ok(mut r) = READ.lock() {
        *r = None;
    }
    if let Ok(mut s) = SHOTS.lock() {
        s.clear();
    }
    if let Ok(mut d) = RUN_DIR.lock() {
        *d = dir;
    }
    if let Ok(mut i) = INDEXES.lock() {
        i.clear();
    }
    if let Ok(mut m) = MODE_MARK.lock() {
        *m = 0;
    }
}

/// Scrape mode opened: the indexes stored from here on belong to it.
pub fn mark_mode_start() {
    let n = INDEXES.lock().map(|i| i.len()).unwrap_or(0);
    if let Ok(mut m) = MODE_MARK.lock() {
        *m = n;
    }
}

/// Store one index as this run's next file: the number and the time are the
/// code's, title, pending and value the agent's. `value` is kept as JSON
/// when its text parses as JSON, else as the text it is. Returns (n, path,
/// the file's text), the text being what the scraping card streams.
pub fn store_index(title: &str, pending: &str, value: &str) -> Result<(usize, PathBuf, String), String> {
    let dir = RUN_DIR
        .lock()
        .ok()
        .and_then(|d| d.clone())
        .ok_or_else(|| "no scrapes folder for this run: AutoCua_data could not be found".to_string())?;
    fs::create_dir_all(&dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let mut indexes = INDEXES.lock().map_err(|_| "the index store is poisoned".to_string())?;
    let n = indexes.len() + 1;
    let parsed = serde_json::from_str::<Value>(value).unwrap_or_else(|_| Value::String(value.to_string()));
    let record = json!({
        "index": n,
        "time": chrono::Utc::now().to_rfc3339(),
        "title": title,
        "pending": pending,
        "value": parsed,
    });
    let path = dir.join(format!("{n}.json"));
    let text = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    fs::write(&path, &text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    indexes.push(IndexEntry { n, title: title.to_string(), pending: pending.to_string() });
    Ok((n, path, text))
}

/// The file of index `n`, as its text, for `index_read`.
pub fn read_index(n: usize) -> Result<String, String> {
    let dir = RUN_DIR
        .lock()
        .ok()
        .and_then(|d| d.clone())
        .ok_or_else(|| "no scrapes folder for this run".to_string())?;
    let count = INDEXES.lock().map(|i| i.len()).unwrap_or(0);
    if n == 0 || n > count {
        return Err(if count == 0 {
            "no index has been stored in this run yet".to_string()
        } else {
            format!("no index [{n}] in this run: the indexes are 1 to {count}")
        });
    }
    let path = dir.join(format!("{n}.json"));
    fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// One line per stored index, "[n]: title. pending: ...", for <scraped_index>.
pub fn index_lines() -> Vec<String> {
    INDEXES
        .lock()
        .map(|i| i.iter().map(|e| format!("[{}]: {}. pending: {}", e.n, e.title.trim(), pending_text(&e.pending))).collect())
        .unwrap_or_default()
}

/// The indexes stored since the current scrape mode opened: (n, title,
/// pending). What the mode's read becomes in the history on exit.
pub fn mode_indexes() -> Vec<(usize, String, String)> {
    let mark = MODE_MARK.lock().map(|m| *m).unwrap_or(0);
    INDEXES
        .lock()
        .map(|i| i.iter().skip(mark).map(|e| (e.n, e.title.clone(), pending_text(&e.pending).to_string())).collect())
        .unwrap_or_default()
}

/// This run's scrapes folder, for the pointer a scrape mode leaves in the
/// history (main_driver point_scrape_at_indexes).
pub fn run_dir() -> Option<PathBuf> {
    RUN_DIR.lock().ok().and_then(|d| d.clone())
}

/// A continued run (the UI's follow-up in the same chat): the folder the
/// history's pointer names is this run's folder again, its files are the
/// indexes so far, read back in order, and numbering goes on from them.
pub fn resume(dir: PathBuf) {
    let mut entries = Vec::new();
    for n in 1usize.. {
        let Ok(text) = fs::read_to_string(dir.join(format!("{n}.json"))) else { break };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        entries.push(IndexEntry { n, title: field("title"), pending: field("pending") });
    }
    let count = entries.len();
    if let Ok(mut d) = RUN_DIR.lock() {
        *d = Some(dir);
    }
    if let Ok(mut i) = INDEXES.lock() {
        *i = entries;
    }
    if let Ok(mut m) = MODE_MARK.lock() {
        *m = count;
    }
}

fn pending_text(pending: &str) -> &str {
    let p = pending.trim();
    if p.is_empty() { "none" } else { p }
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

/// Keep a read for `more`: its tab, its script and where it goes on.
pub fn remember(tab: &str, code: &str, next: Spot, pick: bool) {
    if let Ok(mut r) = READ.lock() {
        *r = Some(Read { tab: tab.to_string(), code: code.to_string(), next, pick });
    }
}

/// The read `more` goes on with: (tab, script, where it goes on, a pick).
pub fn current() -> Option<(String, String, Spot, bool)> {
    let r = READ.lock().ok()?;
    r.as_ref().map(|r| (r.tab.clone(), r.code.clone(), r.next.clone(), r.pick))
}

/// What one read gathered.
pub struct Window {
    pub records: Vec<Value>,
    pub screens: usize,
    /// Where the pane was when the read began: a read that delivers nothing
    /// puts it back there.
    pub start: f64,
    /// Where the next read goes on from, and the pane it scrolls.
    pub next: Spot,
    /// Nothing below the last screen read, and nothing loaded while waited for.
    pub ended: bool,
    /// The pane would not scroll on (it snaps back, or holds its place).
    pub blocked: bool,
    /// The script never calls `document.querySelectorAll`: a pick, read once.
    pub pick: bool,
    /// A screen's answer was too big for the page to hand over whole.
    pub cut: bool,
    /// The size of what was gathered, in characters of JSON.
    pub chars: usize,
}

/// Read three screens of `tab` with `code`, from `from` (None: a new read, from
/// the screen the page is on). Every screen but a new read's first puts the
/// next unread point at the top of the screen, and each is read where the
/// pane lands. At the pane's end the read waits for it to grow (grew); when
/// it does not, the screen is looked at once more as it is then, as a
/// loading block at the end may have turned into records. The overlay is
/// hidden while the screens are photographed and comes back whatever
/// happens; the pictures are kept for the next input even when the read
/// fails. A script error puts the page back where the read began.
pub fn read(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    tab: &str,
    code: &str,
    from: Option<Spot>,
) -> ActResult<Window> {
    let fresh = from.is_none();
    let mut shots: Vec<String> = Vec::new();
    let mut w = Window {
        records: Vec::new(),
        screens: 0,
        start: 0.0,
        next: from.unwrap_or(Spot { top: 0.0, after: 0.0, edge: Vec::new(), pane: None }),
        ended: false,
        blocked: false,
        pick: false,
        cut: false,
        chars: 0,
    };
    let mut began = false;
    let outcome = (|| -> ActResult<()> {
        w.start = num(&eval(py, scanner, tab, &scroll_js(None, w.next.pane.as_deref()))?, "y");
        began = true;
        for i in 0..SCREENS {
            // A new read takes the screen as it is; after that, and in `more`,
            // the next unread point goes to the top of the screen.
            let first = fresh && i == 0;
            let top = w.next.top;
            if !first {
                eval(py, scanner, tab, &scroll_js(Some(top), w.next.pane.as_deref()))?;
            }
            settle_after_action(py, scanner);
            let pictures = shots.len();
            let (bottom, full) = screen(py, scanner, tab, code, first, &mut w, &mut shots)?;
            if w.cut || w.pick {
                break;
            }
            if !first && bottom <= top + 1.0 && full > top + 1.0 {
                w.blocked = true;
                break;
            }
            if bottom >= full - 1.0 {
                // The end of the pane, unless a feed loads its next batch.
                if grew(py, scanner, tab, bottom, w.next.pane.as_deref())? {
                    continue;
                }
                // It ends here: one last look at this screen as it is now, as
                // a loading block at the end may have turned into records.
                shots.truncate(pictures);
                screen(py, scanner, tab, code, false, &mut w, &mut shots)?;
                w.screens -= 1;
                w.ended = true;
                break;
            }
        }
        Ok(())
    })();
    if outcome.is_err() && began {
        let _ = eval(py, scanner, tab, &scroll_js(Some(w.start), w.next.pane.as_deref()));
    }
    let _ = eval(py, scanner, tab, RESTORE);
    if let Ok(mut s) = SHOTS.lock() {
        *s = shots;
    }
    outcome?;
    Ok(w)
}

/// One screen of a read, where the pane is now: its picture, then the script
/// held to it (on_screen), its records added to `w` and where the read
/// stands moved on. Returns the screen's bottom and the pane's full height.
fn screen(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    tab: &str,
    code: &str,
    first: bool,
    w: &mut Window,
    shots: &mut Vec<String>,
) -> ActResult<(f64, f64)> {
    let target = tab.to_string();
    if let Ok(Some(shot)) = scan_op(py, scanner, move |s| Ok(s.frame_of(&target))) {
        shots.push(shot);
    }
    let anchor = (!first).then(|| (w.next.after, w.next.edge.clone()));
    let reply = run_on_screen(py, scanner, tab, code, anchor, w.next.pane.as_deref(), first)?;
    if let Some(text) = super::exception_text(&reply) {
        return err(format!("script error on screen {}: {text}", w.screens + 1));
    }
    w.screens += 1;
    let out = super::value_of(&reply);
    if let Some(size) = out.get("__cut").and_then(Value::as_u64) {
        // Too big to hand over: the read fails as too long.
        w.cut = true;
        w.chars += size as usize;
        return Ok((0.0, 0.0));
    }
    let (y, h, full) = (num(&out, "y"), num(&out, "h").max(1.0), num(&out, "full"));
    if first {
        // Now the pane is known: the one that holds the records.
        w.start = y;
        w.next.pane = out.get("pane").and_then(Value::as_str).map(str::to_string);
        // A pick: the script never calls document.querySelectorAll (the page's
        // own calls, while the script waits, say nothing about it).
        w.pick = !code.contains("document.querySelectorAll");
    }
    // `low` is where this screen took records from: the last row read,
    // found again where it is now. `edge` is the last row read after it.
    w.next.after = num(&out, "low");
    if let Some(last) = out.get("last").and_then(Value::as_f64) {
        w.next.after = w.next.after.max(last);
        w.next.edge = strings(out.get("edge"));
    }
    let items = match out.get("value").cloned().unwrap_or(Value::Null) {
        Value::Array(a) => a,
        Value::Null => Vec::new(),
        other => vec![other],
    };
    // map gives null for an element that is not a record.
    for item in items.into_iter().filter(|v| !v.is_null()) {
        w.chars += item.to_string().chars().count() + 1;
        w.records.push(item);
    }
    w.next.top = y + h;
    Ok((y + h, full))
}

/// Leave the pane with `top` at the top of the screen, and the page as it was
/// (RESTORE): after a read, the next unread point, where `more` or a new read
/// goes on; after a read that delivered nothing, where it began.
pub fn place(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>, tab: &str, top: f64, pane: Option<&str>) {
    let _ = eval(py, scanner, tab, &scroll_js(Some(top), pane));
    let _ = eval(py, scanner, tab, RESTORE);
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Wait up to GROW_MS for the pane to grow past `mark` (an infinite feed
/// loading its next batch), looking every 100 ms from here rather than with
/// a timer in the page, which a background tab slows down: true as soon as
/// it does.
fn grew(py: Python<'_>, scanner: &Arc<Mutex<ScannerInner>>, tab: &str, mark: f64, pane: Option<&str>) -> ActResult<bool> {
    let started = Instant::now();
    loop {
        if num(&eval(py, scanner, tab, &scroll_js(None, pane))?, "full") > mark + 1.0 {
            return Ok(true);
        }
        if started.elapsed() >= Duration::from_millis(GROW_MS) {
            return Ok(false);
        }
        py.detach(|| std::thread::sleep(Duration::from_millis(100)));
    }
}

/// The page as it was: the overlay back, and, if a read's held
/// `document.querySelectorAll` is still on the page (a script stopped at the
/// time limit never reaches the `finally` that takes it off), what was there
/// before it.
const RESTORE: &str = "(() => { const ov = document.querySelector('[data-AutoCua=\"overlay\"]'); \
     if (ov) ov.style.visibility = ''; \
     const q = Object.getOwnPropertyDescriptor(document, 'querySelectorAll'); \
     if (q && q.value && q.value.__AutoCuaGive) q.value.__AutoCuaGive(); return true; })()";

/// Shared by the read's scripts. `__AutoCuaUp` is the next box up, across
/// shadow roots and slots. `__AutoCuaPane(sel)` is the pane a read scrolls:
/// whatever scrolls the records `sel` finds (the first one that shows) and
/// fits on the screen, the page when nothing around them does. {el (null: the page), top (where it
/// begins on screen), y (how far it is scrolled), h (how much of it shows),
/// full (all of it), to(t) (scroll it to t)}.
const PRELUDE: &str = r#"const __AutoCuaUp = e => e.assignedSlot || e.parentElement || e.getRootNode().host || null;
const __AutoCuaPane = sel => {
  const se = document.scrollingElement || document.documentElement;
  let rec = null;
  if (sel) {
    try {
      for (const el of Document.prototype.querySelectorAll.call(document, sel)) {
        const r = el.getBoundingClientRect();
        if (!r.width && !r.height) continue;
        rec = rec || el;
        if (r.bottom <= 0 || r.top >= innerHeight || r.right <= 0 || r.left >= innerWidth) continue;
        const hit = document.elementFromPoint(Math.min(Math.max(r.left + r.width / 2, 0), innerWidth - 1),
          Math.min(Math.max(r.top + 1, 0), innerHeight - 1));
        if (hit && (hit === el || el.contains(hit))) { rec = el; break; }
      }
    } catch (e) {}
  }
  const root = getComputedStyle(document.documentElement);
  for (let e = rec && __AutoCuaUp(rec); e && e !== se && e !== document.documentElement; e = __AutoCuaUp(e)) {
    // <body>'s overflow is the viewport's while <html> keeps both of its own visible.
    if (e === document.body && root.overflowX === 'visible' && root.overflowY === 'visible') continue;
    if (e.scrollHeight > e.clientHeight + 1 && e.clientHeight <= innerHeight + 1 &&
        /^(auto|scroll|overlay)$/.test(getComputedStyle(e).overflowY)) {
      const r = e.getBoundingClientRect();
      return {el: e, top: r.top + e.clientTop, y: e.scrollTop, h: e.clientHeight, full: e.scrollHeight,
        to: t => e.scrollTo({top: t, behavior: 'instant'})};
    }
  }
  return {el: null, top: 0, y: window.scrollY, h: window.innerHeight, full: se.scrollHeight,
    to: t => window.scrollTo({top: t, left: window.scrollX, behavior: 'instant'})};
};"#;

/// Hide the overlay (the pictures are of the page alone), scroll the pane of
/// the records `pane` finds to `top` (None: stay), and say where it is:
/// {y, h, full}.
fn scroll_js(top: Option<f64>, pane: Option<&str>) -> String {
    let to = top.map(|t| t.to_string()).unwrap_or_else(|| "null".to_string());
    let sel = serde_json::to_string(&pane).unwrap_or_else(|_| "null".to_string());
    format!(
        "(() => {{ {PRELUDE}\nconst ov = document.querySelector('[data-AutoCua=\"overlay\"]'); \
         if (ov) ov.style.visibility = 'hidden'; const T = {to}, S = {sel}; \
         if (T !== null) __AutoCuaPane(S).to(T); const p = __AutoCuaPane(S); \
         return {{y: p.y, h: p.h, full: p.full}}; }})()"
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
/// evaluate, same limits), held to the screen (on_screen).
fn run_on_screen(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    tab: &str,
    code: &str,
    anchor: Option<(f64, Vec<String>)>,
    pane: Option<&str>,
    first: bool,
) -> ActResult<Value> {
    let wrapped = on_screen(code, anchor, pane, first);
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
/// `document.querySelectorAll` answers only with the elements whose top edge,
/// in the pane, lies below the last row read and above the screen's bottom.
/// `anchor` is that row: where it was and what its elements look like; it is
/// found again where it is now, and an element of the same height that is
/// not one of them is new (a grid's next card). The first call finds the pane
/// when none is known yet (`pane`, the records' selector): elements outside
/// it are left out. An element without a box of its own (display: contents)
/// is placed by the first box inside it; a hidden one is left out, and so is
/// anything of AutoCua's overlay. A sticky box is placed where it sits in the
/// flow, measured with it made static for a moment (nothing paints between),
/// so it is read once, at its own place; one fixed, or stuck over the screen
/// from above, counts only on the first screen of a new read, where it shows
/// (one stuck at the bottom is reached at its own place further down).
/// The answer comes back as {value, low: where this screen took records
/// from, last: the top edge of the last row read, edge: what its elements
/// look like (in a plain map, only elements that gave a record count), pane,
/// and the pane's y, h, full}. The page's own method is back the moment the
/// script ends.
fn on_screen(code: &str, anchor: Option<(f64, Vec<String>)>, pane: Option<&str>, first: bool) -> String {
    let expr = serde_json::to_string(&super::expression_form(code)).unwrap_or_default();
    let body = serde_json::to_string(code).unwrap_or_default();
    let hint = serde_json::to_string(&pane).unwrap_or_else(|_| "null".to_string());
    let anchor = anchor.map_or_else(|| "null".to_string(), |(top, edge)| json!({"top": top, "edge": edge}).to_string());
    format!(
        r#"{PRELUDE}
const __AutoCuaQsa = Document.prototype.querySelectorAll;
const __AutoCuaAnchor = {anchor}, __AutoCuaFirst = {first};
const __AutoCuaEdge = new Set(__AutoCuaAnchor ? __AutoCuaAnchor.edge : []);
let __AutoCuaSel = {hint}, __AutoCuaP = null, __AutoCuaLow = 0, __AutoCuaHigh = 0, __AutoCuaFound = false;
const __AutoCuaCalls = [], __AutoCuaTops = new Map(), __AutoCuaHolds = new Map();
const __AutoCuaBox = el => {{
  const r = el.getBoundingClientRect();
  if (r.width || r.height) return r;
  if (!el.getClientRects().length && getComputedStyle(el).display !== 'contents') return null;
  for (const c of el.children) {{ const b = __AutoCuaBox(c); if (b) return b; }}
  return null;
}};
const __AutoCuaMark = el => {{
  const links = [...new Set([el, ...el.getElementsByTagName('a')].map(a => a.getAttribute && a.getAttribute('href')).filter(Boolean))].slice(0, 3).map(h => h.slice(0, 120));
  const text = el.id || links.length ? '' : Array.from((el.textContent || '').replace(/\s+/g, ' ').trim()).slice(0, 100).join('');
  return [el.tagName, el.id, ...links, text].join('|');
}};
const __AutoCuaHold = e => {{
  if (!e || e === __AutoCuaP.el || e === document.documentElement) return 0;
  if (__AutoCuaHolds.has(e)) return __AutoCuaHolds.get(e);
  const cs = getComputedStyle(e);
  let v = 0;
  if (cs.position === 'fixed') v = true;
  else if (cs.position === 'sticky' && (cs.top !== 'auto' || cs.bottom !== 'auto')) {{
    const drawn = e.getBoundingClientRect().top, a = e.getAttribute('style');
    e.setAttribute('style', (a || '') + ';position:static !important');
    v = drawn - e.getBoundingClientRect().top;
    if (a === null) e.removeAttribute('style'); else e.setAttribute('style', a);
  }}
  const up = __AutoCuaHold(__AutoCuaUp(e));
  v = v === true || up === true ? true : v + up;
  __AutoCuaHolds.set(e, v);
  return v;
}};
const __AutoCuaIn = el => {{
  for (let e = el; e; e = __AutoCuaUp(e)) if (e === __AutoCuaP.el) return true;
  return false;
}};
const __AutoCuaAt = b => b.top - __AutoCuaP.top + __AutoCuaP.y;
const __AutoCuaSeen = (el, b) => {{
  const hit = document.elementFromPoint(Math.min(Math.max(b.left + b.width / 2, 0), innerWidth - 1),
    Math.min(Math.max(b.top + Math.min(b.height / 2, 8), 0), innerHeight - 1));
  return !!hit && (hit === el || el.contains(hit) || hit.contains(el));
}};
const __AutoCuaOpen = sel => {{
  if (__AutoCuaP) return;
  if (__AutoCuaSel === null && sel) __AutoCuaSel = String(sel);
  __AutoCuaP = __AutoCuaPane(__AutoCuaSel);
  __AutoCuaHigh = __AutoCuaP.y + __AutoCuaP.h;
  __AutoCuaLow = __AutoCuaAnchor ? __AutoCuaAnchor.top : __AutoCuaP.y - 0.5;
}};
const __AutoCuaFind = all => {{
  if (__AutoCuaFound || !__AutoCuaEdge.size) return;
  __AutoCuaFound = true;
  let gap = 2 * __AutoCuaP.h, found = null;
  for (const el of all) {{
    const b = __AutoCuaBox(el);
    if (!b) continue;
    const t0 = __AutoCuaAt(b);
    if (Math.abs(t0 - __AutoCuaLow) > gap + innerHeight || !__AutoCuaEdge.has(__AutoCuaMark(el))) continue;
    const hold = __AutoCuaHold(el);
    if (hold === true) continue;
    const t = t0 - hold, d = Math.abs(t - __AutoCuaLow);
    if (d <= gap) {{ found = t; gap = d; }}
  }}
  if (found !== null) __AutoCuaLow = found;
}};
const __AutoCuaOn = el => {{
  if (el.closest('[data-AutoCua]')) return false;
  const b = __AutoCuaBox(el);
  if (!b) return false;
  const shows = b.bottom > 0 && b.top < innerHeight && b.right > 0 && b.left < innerWidth;
  if (!shows && !(__AutoCuaAt(b) > __AutoCuaLow - 0.5 && __AutoCuaAt(b) < __AutoCuaHigh)) return false;
  if (__AutoCuaP.el && !__AutoCuaIn(el)) return false;
  const hold = __AutoCuaHold(el);
  if (hold !== true) {{
    const t = __AutoCuaAt(b) - hold;
    if (t < __AutoCuaHigh && (t > __AutoCuaLow + 0.5 || (t > __AutoCuaLow - 0.5 && !__AutoCuaEdge.has(__AutoCuaMark(el))))) {{
      __AutoCuaTops.set(el, t);
      return true;
    }}
  }}
  return (hold === true || hold > 0) && __AutoCuaFirst && shows && __AutoCuaSeen(el, b);
}};
let __AutoCuaRun;
try {{ __AutoCuaRun = new Function('return (async () => (\n' + {expr} + '\n))')(); }}
catch (e) {{ __AutoCuaRun = new Function('return (async () => {{\n' + {body} + '\n}})')(); }}
const __AutoCuaHeldQsa = function (selector) {{
  __AutoCuaOpen(selector);
  const all = Array.from(__AutoCuaQsa.call(this, selector));
  __AutoCuaFind(all);
  const out = all.filter(__AutoCuaOn);
  __AutoCuaCalls.push(out);
  return out;
}};
const __AutoCuaBefore = Object.getOwnPropertyDescriptor(document, 'querySelectorAll');
__AutoCuaHeldQsa.__AutoCuaGive = () => {{
  if (__AutoCuaBefore) Object.defineProperty(document, 'querySelectorAll', __AutoCuaBefore);
  else delete document.querySelectorAll;
}};
document.querySelectorAll = __AutoCuaHeldQsa;
try {{
  const value = await __AutoCuaRun();
  __AutoCuaOpen(null);
  const one = __AutoCuaCalls.length === 1 && Array.isArray(value) && value.length === __AutoCuaCalls[0].length;
  const read = one ? __AutoCuaCalls[0].filter((el, i) => value[i] !== null && value[i] !== undefined) : __AutoCuaCalls.flat();
  let last = null;
  for (const el of read) {{ const t = __AutoCuaTops.get(el); if (t !== undefined && (last === null || t > last)) last = t; }}
  const edge = last === null ? [] : read.filter(el => Math.abs((__AutoCuaTops.get(el) ?? -1e9) - last) < 0.5).map(__AutoCuaMark);
  if (last !== null && Math.abs(last - __AutoCuaLow) < 0.5) edge.push(...__AutoCuaEdge);
  return {{value, low: __AutoCuaLow, last, edge: [...new Set(edge)],
    pane: __AutoCuaSel, y: __AutoCuaP.y, h: __AutoCuaP.h, full: __AutoCuaP.full}};
}} finally {{ __AutoCuaHeldQsa.__AutoCuaGive(); }}"#
    )
}
