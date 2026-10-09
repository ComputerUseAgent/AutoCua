//! Browser session — opening Chrome, owning the CDP link, and lending it out.
//!
//! THIS side owns the browser. It launches Chrome with the remote-debugging
//! port open, opens tabs in the background over CDP and closes them over
//! Chrome's HTTP endpoint, and holds the ONE CDP session per tab that
//! everything else borrows: the glow rides on it, and every controller tool
//! takes it for the length of a single operation to click, type, scroll or
//! navigate.
//!
//! AutoCua/web/tree/element.rs is a SCANNER, not a driver, and not a process:
//! `scan_core` below calls it with the session and the tab to read, and gets
//! back a tree, its geometry and a screenshot. It cannot navigate, click,
//! type, open a tab or launch Chrome, and it holds no connection of its own.
//!
//! It used to be a second binary driven as a subprocess REPL, answering over
//! stdin/stdout with its results left in three files on disk. That meant two
//! sockets onto one browser, two ideas of which tab was current, and a scan
//! whose tree, geometry and screenshot were read back separately.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use regex::Regex;
use serde_json::{json, Value};

use crate::bridge::Bridge;
use crate::tree::element;
use crate::ScannerError;

pub const CHROME_PORT: u16 = 9222;
// A blank tab is always about:blank — the URL the ADDRESS BAR shows. The logo
// is painted into it afterwards with Page.setDocumentContent.
const BLANK_URL: &str = "about:blank";

// Ceiling on buffered CDP events, as a last-resort guard against a run that
// buffers forever. It should not normally be reached: WAITED_METHODS below is
// what actually keeps the buffer small.
//
// Reaching it must drop the OLDEST event, never the newest. A cap that drops
// the newest is a trap: once full, the very event a caller is waiting for is
// the one thrown away, and because take_events only removes the method it was
// asked for, a buffer full of anything else never drains — so the wait stays
// deaf until the next clear_events. That turned every navigation on a
// subresource-heavy page into a silent 30-second timeout.
const EVENT_CAP: usize = 2000;

/// How many pending frames to read before answering "did anything happen?".
/// Non-blocking — `drain` stops at the first WouldBlock — so this is a ceiling
/// on a backlog, not a wait.
const NOTICE_DRAIN: usize = 256;

/// The backlog `begin_action` clears: everything the page said since the last
/// read — while the model was thinking, say. Non-blocking like NOTICE_DRAIN,
/// so a ceiling and never a wait; generous because seconds of a busy page
/// are thousands of frames.
const BACKLOG_DRAIN: usize = 20_000;

/// How long a page has to answer the cheap question sent ahead of each watched call (a
/// scan's, `Cdp::watch`): FROZEN_AFTER, the owner's rule, and PROBE_FOR on top for the
/// round trip. A page silent through both froze, and its tab is closed and the page
/// opened again (`ScannerInner::recover_tab`). AutoCuaBridge's TOOLS_FROZEN_MS and
/// TOOLS_PROBE_MS are the same.
const FROZEN_AFTER: Duration = Duration::from_secs(5);
const PROBE_FOR: Duration = Duration::from_secs(1);

/// What a call into a page that froze answers (`Cdp::rpc`).
const PAGE_FROZE: &str = "the page stopped answering (it froze)";

// The only CDP events anything in this crate ever waits on:
//   Page.loadEventFired        - navigation, reload and history waits
//   Network.requestWillBeSent  - settle()'s in-flight set
//   Network.loadingFinished    -   "
//   Network.loadingFailed      -   "
//   Target.attachedToTarget    - discovering cross-origin iframe sessions
//
// Everything else Chrome sends is never read by anyone. Buffering it only
// crowded out the events that matter: with Network.enable on, Chrome emits
// roughly eight events per request, so a page with a few hundred subresources
// used to bury the load event under its own noise.
//
// The last three are not waited on by anybody. They are here because `stash`
// is the one place that sees every frame Chrome sends, and each of them needs
// answering the moment it arrives rather than whenever someone next looks.
const WAITED_METHODS: [&str; 10] = [
    "Page.loadEventFired",
    // Not waited on directly — it is what tells us a tab STARTED loading, so
    // switching to that tab knows to wait for it. See `loading`.
    "Page.frameNavigated",
    "Network.requestWillBeSent",
    "Network.loadingFinished",
    "Network.loadingFailed",
    "Target.attachedToTarget",
    // A modal dialog BLOCKS the renderer. Until it is answered the tab replies
    // to nothing, so this has to be handled on arrival, not on demand.
    "Page.javascriptDialogOpening",
    // A popup left for the model closed (answered, or the tab moved on): its
    // page takes calls again. See `dialogs`.
    "Page.javascriptDialogClosed",
    // A crashed target stays in /json/list, so `tab_exists` keeps saying the
    // tab is fine while every call against it fails.
    "Inspector.targetCrashed",
    "Target.detachedFromTarget",
];

// Relative install paths under Program Files / Program Files (x86) /
// LOCALAPPDATA, in the same browser preference order as the macOS list below.
// Windows takes one of these three roots depending on installer and per-user
// vs per-machine install, so each browser is probed in all three.
// Google Chrome is the agent's browser in both of its modes (the owner,
// 2026-10-03; Brave, which came first until then, is gone). Chromium and Edge
// remain the fallbacks.
const CHROME_RELATIVE_WIN: &[&str] = &[
    r"Google\Chrome\Application\chrome.exe",
    r"Chromium\Application\chrome.exe",
    r"Microsoft\Edge\Application\msedge.exe",
];

const CHROME_PATHS_UNIX: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/opt/google/chrome/chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];

/// The browser's name for messages, from its executable.
fn browser_name(path: &std::path::Path) -> &'static str {
    let s = path.to_string_lossy().to_ascii_lowercase();
    if s.contains("msedge") {
        "Edge"
    } else if s.contains("chromium") {
        "Chromium"
    } else {
        "Chrome"
    }
}


// ---------------------------------------------------------------------------
// Errors. ScanErr::Scanner surfaces to Python as the module's ScannerError;
// ScanErr::Py passes an already-raised Python error (KeyboardInterrupt from a
// signal check) through untouched.
// ---------------------------------------------------------------------------

pub enum ScanErr {
    Scanner(String),
    Py(PyErr),
}

impl ScanErr {
    pub fn s(msg: impl Into<String>) -> Self {
        ScanErr::Scanner(msg.into())
    }
}

impl From<ScanErr> for PyErr {
    fn from(e: ScanErr) -> PyErr {
        match e {
            ScanErr::Scanner(msg) => ScannerError::new_err(msg),
            ScanErr::Py(err) => err,
        }
    }
}

pub type SResult<T> = Result<T, ScanErr>;

/// A CDP call's two failure shapes: a clean error reply (connection still in
/// step — Python's _CdpError) vs. a torn/late transport (connection dropped).
/// Cosmetics callers swallow both, so the messages are never surfaced —
/// they exist to keep the two paths legible.
#[allow(dead_code)]
pub enum CdpFail {
    Clean(String),
    Lost(String),
}

impl From<CdpFail> for ScanErr {
    fn from(e: CdpFail) -> ScanErr {
        match e {
            CdpFail::Clean(m) | CdpFail::Lost(m) => ScanErr::Scanner(m),
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// `name` plus the platform's executable suffix — "cargo" stays "cargo" on
/// Unix and becomes "cargo.exe" on Windows, where the file on disk carries a
/// suffix callers never write.
fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exe = exe_name(name);
    std::env::split_paths(&path)
        .map(|d| d.join(&exe))
        .find(|c| c.is_file())
}

/// Windows sets USERPROFILE rather than HOME. Falling through to an empty path
/// would silently drop the Chrome profile into the current directory instead of
/// the user's home, so check both.
fn home_dir() -> PathBuf {
    match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()),
    }
}

fn sleep_s(secs: f64) {
    std::thread::sleep(Duration::from_secs_f64(secs));
}

/// Check for a pending Python signal (Ctrl+C) from GIL-released Rust code, so
/// a long poll loop stays as interruptible as Python's `select` loop was.
pub(crate) fn check_py_signals() -> SResult<()> {
    Python::attach(|py| py.check_signals()).map_err(ScanErr::Py)
}

// ---------------------------------------------------------------------------
// Chrome's HTTP control endpoint: /json/list, /json/version. Not CDP.
// ---------------------------------------------------------------------------

/// The running browser's id, the last part of its DevTools url: a browser
/// started again on the same port has a new one.
fn browser_id(port: u16) -> Option<String> {
    let version = chrome_http(port, "/json/version", "GET").ok()?;
    let url = version.get("webSocketDebuggerUrl")?.as_str()?;
    url.rsplit('/').next().filter(|id| !id.is_empty()).map(str::to_string)
}

pub(crate) fn chrome_http(port: u16, path: &str, method: &str) -> SResult<Value> {
    let addr = format!("127.0.0.1:{port}");
    let sock_addr = addr
        .parse()
        .map_err(|e| ScanErr::s(format!("bad address {addr}: {e}")))?;
    let mut s = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
        .map_err(|e| ScanErr::s(format!("{method} {path}: {e}")))?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    s.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes())
        .map_err(|e| ScanErr::s(format!("{method} {path}: {e}")))?;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = s
            .read(&mut tmp)
            .map_err(|e| ScanErr::s(format!("{method} {path}: {e}")))?;
        if n == 0 {
            return Err(ScanErr::s(format!(
                "{method} {path}: connection closed before http headers"
            )));
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Err(ScanErr::s(format!("{method} {path}: http headers too large")));
        }
    };

    let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let content_len = headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.eq_ignore_ascii_case("content-length") {
            v.trim().parse::<usize>().ok()
        } else {
            None
        }
    });

    match content_len {
        Some(len) => {
            while buf.len() < header_end + len {
                let n = s
                    .read(&mut tmp)
                    .map_err(|e| ScanErr::s(format!("{method} {path}: {e}")))?;
                if n == 0 {
                    return Err(ScanErr::s(format!(
                        "{method} {path}: connection closed before full body"
                    )));
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            buf.truncate(header_end + len);
        }
        None => {
            // Connection: close was requested — read to EOF.
            loop {
                match s.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(_) => break,
                }
            }
        }
    }

    let body = String::from_utf8_lossy(&buf[header_end..]).into_owned();
    // Python returns the raw body when it isn't JSON.
    Ok(serde_json::from_str(body.trim()).unwrap_or(Value::String(body)))
}

/// Chrome's page targets, telling "no tabs" apart from "could not ask".
///
/// `page_targets` collapses every failure — refused connection, read timeout,
/// non-JSON body — into an empty Vec, which reads as "this browser has zero
/// tabs". Anything that then decides what to do about having no tabs acts on a
/// transient hiccup as though the browser were empty. `start` is exactly that
/// caller, and getting it wrong there means abandoning the bound tab.
fn page_targets_checked(port: u16) -> SResult<Vec<Value>> {
    match chrome_http(port, "/json/list", "GET") {
        Ok(Value::Array(items)) => Ok(items
            .into_iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
            .collect()),
        Ok(other) => Err(ScanErr::s(format!(
            "/json/list on port {port} did not answer with a list: {other}"
        ))),
        Err(e) => Err(e),
    }
}

fn page_targets(port: u16) -> Vec<Value> {
    match chrome_http(port, "/json/list", "GET") {
        Ok(Value::Array(items)) => items
            .into_iter()
            .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
            .collect(),
        _ => Vec::new(),
    }
}

/// A tab's CDP target id, or "" — every caller wants the same field out of
/// the Value /json/list hands back.
pub fn target_id_of(target: &Value) -> &str {
    target.get("id").and_then(Value::as_str).unwrap_or("")
}

/// The head of Chrome's tab listing, by target id: the page with the newest
/// activity. Shown in any window (and with several windows, whichever was
/// shown last), or just opened in the background, which counts as activity
/// too (measured): a tab this agent has just opened heads the list without
/// being on show. A failed listing is an error, not "no tab".
fn listed_first(port: u16) -> SResult<Option<String>> {
    Ok(page_targets_checked(port)?.first().map(|t| target_id_of(t).to_string()))
}

/// Whether that tab is still open.
pub fn tab_exists(port: u16, target_id: &str) -> bool {
    page_targets(port)
        .iter()
        .any(|t| target_id_of(t) == target_id)
}

/// Close one tab by target id, and report whether it actually left Chrome's
/// list. /json/close answers with plain text ("Target is closing"), so the
/// only thing worth judging is the target going away.
pub fn close_target(port: u16, target_id: &str) -> SResult<bool> {
    chrome_http(port, &format!("/json/close/{target_id}"), "GET")?;
    for _ in 0..20 {
        if !tab_exists(port, target_id) {
            return Ok(true);
        }
        sleep_s(0.1);
    }
    Ok(false)
}

/// True for the surfaces that carry no page of their own — including Chrome's
/// New Tab page, which is browser chrome, not a document.
pub fn blank_url(url: &str) -> bool {
    matches!(
        url.trim(),
        "" | "about:blank"
            | "chrome://newtab/"
            | "chrome://new-tab-page/"
            | "chrome://new-tab-page"
            | "edge://newtab/"
    )
}

/// Guarantee the browser has a tab for the agent to work in. Returns true if
/// a tab had to be created.
fn ensure_tab_impl(port: u16) -> SResult<bool> {
    if !page_targets(port).is_empty() {
        return Ok(false);
    }
    open_tab_impl(port, BLANK_URL)?;
    Ok(true)
}

/// Create a fresh blank tab and return its target id — used by single-tab
/// agents so each one drives a tab of its own instead of whatever tab happens
/// to be frontmost in the shared browser.
fn create_tab_impl(port: u16) -> SResult<String> {
    open_tab_impl(port, BLANK_URL)
}

/// Open a tab on `url` and return its target id, once Chrome lists it.
///
/// In the background, so the browser stays where the person left it: in
/// front or behind. Every other way to open a tab (/json/new, a foreground
/// Target.createTarget) made Brave the active app on macOS, over whatever the
/// person or computer use was working in (measured, Brave 154). The tab is
/// then shown by `show_tab`, which moves the tab strip without touching the
/// window. A background tab is live for the agent either way: the focus
/// emulation `attach` turns on keeps it rendering, and its animations,
/// clicks, typing and screenshots ran as in the front tab (measured).
fn open_tab_impl(port: u16, url: &str) -> SResult<String> {
    let created = Cdp::new(port)
        .rpc("Target.createTarget", json!({"url": url, "background": true}), None, 5.0)
        .map_err(|(CdpFail::Clean(m) | CdpFail::Lost(m))| {
            ScanErr::s(format!("could not open a tab on port {port}: {m}"))
        })?;
    let id = created
        .get("targetId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ScanErr::s("Chrome did not return the new tab's target id"))?;
    for _ in 0..20 {
        if page_targets(port)
            .iter()
            .any(|t| t.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            return Ok(id);
        }
        sleep_s(0.25);
    }
    Err(ScanErr::s(format!("opened tab {id} on port {port} but it never appeared")))
}

/// The logo page as one line of HTML, ready to render into a blank tab.
/// Empty string if the logo is missing — the tab then stays honestly blank.
fn blank_html_impl(logo_page: &PathBuf) -> String {
    match std::fs::read_to_string(logo_page) {
        Ok(raw) => raw.split_whitespace().collect::<Vec<_>>().join(" "),
        Err(_) => String::new(),
    }
}

/// Drop container lines that contain nothing the model can see. A container
/// earns its line only if a NUMBERED line sits beneath it (deeper indent,
/// before the tree returns to its level).
fn prune_empty_containers(tree: &str) -> String {
    let lines: Vec<&str> = tree.lines().collect();

    fn depth(ln: &str) -> usize {
        (ln.len() - ln.trim_start_matches(' ').len()) / 2
    }

    fn bare_container(ln: &str) -> bool {
        let s = ln.trim();
        s.starts_with('<')
            && !s.contains("</")
            && !s.ends_with("/>")
            && !matches!(s, "<element>" | "</element>" | "<frame>")
    }

    let mut keep: Vec<&str> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if !bare_container(ln) {
            keep.push(ln);
            continue;
        }
        let d = depth(ln);
        let mut earned = false;
        for nxt in &lines[i + 1..] {
            let s = nxt.trim();
            if s == "<element>" || s == "</element>" {
                break;
            }
            if s != "<frame>" && depth(nxt) <= d {
                break;
            }
            if s.starts_with('[') {
                earned = true;
                break;
            }
        }
        if earned {
            keep.push(ln);
        }
    }
    keep.join("\n")
}

/// glow.css + glow.js as one page script, read once per process. Empty string
/// when either asset is missing or unreadable — the overlay is then simply
/// absent, and nothing else about the run changes.
fn glow_source(glow_css: &PathBuf, glow_js: &PathBuf) -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        match (
            std::fs::read_to_string(glow_css),
            std::fs::read_to_string(glow_js),
        ) {
            (Ok(css), Ok(js)) => {
                // glow.js reads AutoCua_CSS and adopts it; glow.html instead
                // links the stylesheet and leaves the name undefined.
                let quoted = serde_json::to_string(&css).unwrap_or_default();
                format!("var AutoCua_CSS = {quoted};\n{js}")
            }
            _ => String::new(),
        }
    })
}

// ---------------------------------------------------------------------------
// THE line to Chrome. Not "this side's cosmetics socket" any more: the tab
// tools, the scanner and the glow all ride this one connection, so a rule
// written when nobody was listening — "it can fail freely, it need not know
// which tab" — is now load-bearing for the whole agent. Deliberately NO
// Origin header — tungstenite's plain client
// handshake sends none, and a socket with no Origin needs no
// --remote-allow-origins flag. Cosmetics-only by contract: callers swallow
// every failure, and a dead socket just gets dropped and lazily redialed.
// ---------------------------------------------------------------------------

/// What a call into a page held by a JavaScript popup (alert, confirm or prompt)
/// answers, in both modes (Cdp::rpc here, TOOLS_DIALOG_OPEN in AutoCuaBridge's
/// tools.js): the page is frozen until the popup is answered, the `dialog` tool's job.
pub const DIALOG_OPEN: &str = "a popup (a JavaScript alert, confirm or prompt) is open on that tab \
     and freezes the page until it is answered: answer it with `dialog` first";

/// What the model reads in place of the page while a JavaScript popup holds it
/// (`<dialog>` in the agent loop): the message and Chrome's buttons, numbered for
/// the `dialog` tool: OK alone for an alert, OK and Cancel for a confirm or a
/// prompt, which adds its text box. A page cannot add a button or rename one.
/// `d` is the popup's opening event (type, message, defaultPrompt).
pub fn dialog_view(d: &Value) -> String {
    let field = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let kind = match field("type") {
        k if k.is_empty() => "dialog".to_string(),
        k => k,
    };
    let mut lines = vec![
        "A popup is open on this tab: the page is frozen and cannot be scanned until it is \
         answered. Answer it with `dialog`."
            .to_string(),
        format!("{kind}: \"{}\"", field("message")),
    ];
    if kind == "prompt" {
        let default = field("defaultPrompt");
        lines.push(if default.is_empty() {
            "a text box: `dialog`'s value is typed into it before OK".to_string()
        } else {
            format!("a text box holding \"{default}\": `dialog`'s value is typed into it before OK")
        });
    }
    lines.push("[1] OK".to_string());
    if kind != "alert" {
        lines.push("[2] Cancel".to_string());
    }
    lines.join("\n")
}

/// The owner's rule, in both modes: the agent never uses the Chrome Web Store. The tab
/// tools refuse it with this (controller/tab), and a tab that gets there anyway (a
/// link, a redirect) is shown as blocked (`web_store_scan`). TOOLS_WEB_STORE in
/// AutoCuaBridge's tools.js, verbatim.
pub const WEB_STORE_BLOCKED: &str =
    "the Chrome Web Store is not allowed: the agent may not open it or install anything from it";

/// Whether `url` is on the Chrome Web Store, at either of its addresses (tools.js
/// toolsWebStore is the same test). Read the way the browser reads an address (the URL
/// standard: backslashes, dot segments, encoded or full-width hosts), so no spelling
/// the browser would open as the store gets past.
pub fn web_store_url(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    let host = u.host_str().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    host == "chromewebstore.google.com"
        || (host == "chrome.google.com" && (u.path() == "/webstore" || u.path().starts_with("/webstore/")))
}

/// What a tab on the Chrome Web Store shows the model in place of the page, in both
/// modes: the agent may not use it, and no extension may read it anyway. The page stays
/// [1], so the tools that need an element still have one.
fn web_store_scan(out: &mut element::ScanOut) {
    out.tree = format!(
        "<element>\n  [1] <page scrollable>the whole page</page>\n    {} is the Chrome Web \
         Store, which the agent is not allowed to use: leave it with navigate_tab back, \
         update_tab or new_tab\n</element>",
        out.url
    );
    out.hits.retain(|(i, _)| *i == 1);
    out.count = 1;
    out.occluded = 0;
    out.noise = 0;
    out.screenshot = None;
    out.screenshot_plain = None;
}

/// A page that can be opened again by its address (`recover_tab`): a web page or a
/// file, not one of the browser's own pages (chrome://crash would crash again).
fn reopenable(url: &str) -> bool {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
    matches!(scheme.as_str(), "http" | "https" | "file")
}

/// What the model is told when the tab of a page that crashed or froze was closed and a
/// new one put in its place (`recover_tab`); tools.js toolsReopenText says the same.
fn reopen_text(why: &str, url: &str, reopened: bool, again: bool) -> String {
    let page = if url.is_empty() { "the page".to_string() } else { format!("the page on {url}") };
    let what = if why == "crashed" {
        "crashed (its renderer died)".to_string()
    } else {
        format!("stopped answering for {} s (it froze)", FROZEN_AFTER.as_secs())
    };
    if reopened {
        return format!(
            "{page} {what}, so its tab was closed and the same page opened again in a new tab in \
             its place: anything typed into it and not submitted is gone, and so is its back and \
             forward history"
        );
    }
    let again = if again { " again after it was opened again" } else { "" };
    format!("{page} {what}{again}, so its tab was closed and a blank tab opened in its place")
}

pub struct Cdp {
    port: u16,
    ws: Option<tungstenite::WebSocket<TcpStream>>,
    id: u64,
    /// Events seen while waiting for a reply. CDP interleaves them with
    /// answers, and an action that has to wait for one (a navigation's
    /// loadEventFired) would never see it if they were dropped on the floor.
    events: VecDeque<Value>,
    /// Answers that arrived for a request other than the one being waited on.
    /// One call is in flight at a time, so this only ever holds a reply the
    /// event drain happened to read first — but dropping it would strand the
    /// caller waiting for an answer already off the wire.
    replies: HashMap<u64, Value>,
    /// targetId -> sessionId (flatten mode)
    sessions: HashMap<String, String>,
    /// Sessions whose MAIN frame has started navigating and has not reported
    /// its load event yet.
    ///
    /// This is the only way to answer "is that tab still loading?" without
    /// running JavaScript in it. It matters on switch_tab: binding to a tab
    /// that is mid-load and scanning it immediately hands the model half a
    /// page with no hint that the rest is still coming. `settle()` cannot
    /// cover this — it only knows about requests whose start it witnessed
    /// itself, and a navigation begun before the scan is invisible to it.
    loading: HashSet<String>,
    /// Things that happened to the browser nobody asked for, in the words the
    /// MODEL should read: a dialog that was dismissed, a page that crashed.
    ///
    /// Handling these silently is what browser-use does, and it leaves the
    /// model's picture of the page quietly wrong — it never learns why the
    /// form it filled reset, or why the tab went blank. Draining them into the
    /// next tool result is what turns "the tool lied" into "the tool told me".
    notices: VecDeque<String>,
    /// Sessions Chrome has explicitly told us are finished — their renderer
    /// crashed, or they detached. Kept apart from `sessions` on purpose: an
    /// absence there only means nobody has attached, while membership here
    /// means the page is confirmed gone and no call to it can ever answer.
    dead: HashSet<String>,
    /// Targets whose renderer died. Recovery is the caller's business — this
    /// only remembers, because the crash event and the next scan can be many
    /// calls apart.
    crashed: HashSet<String>,
    /// The session whose calls are watched for a page that stops answering (`watch`,
    /// set for a scan), and the sessions found frozen that way (`take_frozen`). Every
    /// later call into a frozen one fails at once rather than waiting again.
    watching: Option<String>,
    frozen: HashSet<String>,
    /// Popups (alerts, confirms, prompts) open right now, by session: their
    /// opening event's params (type, message, defaultPrompt). Left for the model
    /// to answer (the `dialog` tool); until then that page is frozen, so `rpc`
    /// sends it nothing.
    dialogs: HashMap<String, Value>,
    /// Request ids whose caller stopped waiting.
    ///
    /// A per-call timeout no longer hangs up (see `rpc`), so the answer it gave
    /// up on may still turn up later. Nothing will ever claim it, and parking
    /// it in `replies` would pin a slot there for the life of the run — with
    /// enough timeouts that fills the map and starts dropping answers callers
    /// ARE waiting for. So the id is recorded here and its reply discarded on
    /// arrival.
    abandoned: HashSet<u64>,
    /// Bumped on every hang-up. Sessions and anything registered inside them
    /// die with the connection, so a caller holding session-keyed state
    /// watches this to know its state is stale.
    generation: u64,
}

impl Cdp {
    fn new(port: u16) -> Self {
        Cdp {
            port,
            ws: None,
            id: 0,
            events: VecDeque::new(),
            replies: HashMap::new(),
            sessions: HashMap::new(),
            loading: HashSet::new(),
            abandoned: HashSet::new(),
            notices: VecDeque::new(),
            crashed: HashSet::new(),
            watching: None,
            frozen: HashSet::new(),
            dead: HashSet::new(),
            dialogs: HashMap::new(),
            generation: 0,
        }
    }

    fn connect(&mut self) -> Result<(), CdpFail> {
        if self.ws.is_some() {
            return Ok(());
        }
        let ver = chrome_http(self.port, "/json/version", "GET")
            .map_err(|e| match e {
                ScanErr::Scanner(m) => CdpFail::Lost(m),
                ScanErr::Py(_) => CdpFail::Lost("interrupted".into()),
            })?;
        let url = ver
            .get("webSocketDebuggerUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpFail::Lost("no webSocketDebuggerUrl".into()))?
            .to_string();
        let rest = url
            .strip_prefix("ws://")
            .ok_or_else(|| CdpFail::Lost(format!("unexpected ws url: {url}")))?;
        let (hostport, _path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| CdpFail::Lost(format!("bad ws port in {url}")))?,
            ),
            None => (hostport.to_string(), 80),
        };
        let sock_addr = format!("{host}:{port}")
            .parse()
            .map_err(|_| CdpFail::Lost(format!("bad ws host in {url}")))?;
        let stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
            .map_err(|e| CdpFail::Lost(format!("CDP dial failed: {e}")))?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let (ws, _resp) = tungstenite::client(url.as_str(), stream)
            .map_err(|e| CdpFail::Lost(format!("CDP handshake refused: {e}")))?;
        self.ws = Some(ws);
        Ok(())
    }

    fn drop_conn(&mut self) {
        self.ws = None; // dropping the WebSocket closes the TcpStream
        self.sessions.clear();
        self.loading.clear();
        self.events.clear();
        self.replies.clear();
        self.abandoned.clear();
        self.dead.clear();
        self.dialogs.clear();
        self.frozen.clear();
        // `notices` deliberately SURVIVES a hang-up: a crash is often what
        // caused it, and that is exactly the sentence the model needs.
        self.generation += 1;
    }

    /// Hold a reply that arrived for some call other than the one being
    /// waited on — unless that call has already given up, in which case the
    /// answer is dead post and goes in the bin.
    fn hold_reply(&mut self, id: u64, m: Value) {
        if self.abandoned.remove(&id) {
            return;
        }
        if self.replies.len() < EVENT_CAP {
            self.replies.insert(id, m);
        }
    }

    /// Keep an event for whoever is waiting on one.
    ///
    /// Anything nobody waits on is dropped here rather than buffered — see
    /// WAITED_METHODS. If the cap is somehow still reached, the OLDEST event
    /// goes, so the event that just arrived is always kept.
    fn stash(&mut self, m: Value) {
        let Some(method) = m.get("method").and_then(Value::as_str).map(str::to_string) else {
            return;
        };
        if !WAITED_METHODS.contains(&method.as_str()) {
            return;
        }
        // Answered here and NOT buffered: nothing waits on these, and a
        // dialog left unanswered blocks its renderer for as long as it sits.
        match method.as_str() {
            "Page.javascriptDialogOpening" => return self.note_dialog(&m),
            "Page.javascriptDialogClosed" => {
                if let Some(sess) = m.get("sessionId").and_then(Value::as_str) {
                    self.dialogs.remove(sess);
                }
                return;
            }
            "Inspector.targetCrashed" | "Target.detachedFromTarget" => {
                return self.note_gone(&method, &m)
            }
            _ => {}
        }
        self.note_load_state(&m);
        while self.events.len() >= EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(m);
    }

    /// Answer a modal dialog immediately, and remember what it said.
    ///
    /// An open alert/confirm/prompt blocks its renderer completely: the tab
    /// answers no CDP call at all until the dialog is closed. Nothing in this
    /// crate used to listen, so the tab simply stopped responding and every
    /// call against it ran out its timeout — the agent burned its whole step
    /// budget on a box saying "are you sure?".
    ///
    /// An alert, a confirm or a prompt is LEFT OPEN for the model and kept in
    /// `dialogs`: the next scan shows it in place of the page and the `dialog`
    /// tool answers it (controller/dialog). Until then no call goes into that page
    /// (`rpc`). Only a "Leave site?" (beforeunload) is accepted here at once: it
    /// comes from the agent's own move away.
    ///
    /// The text is KEPT. Dismissing a dialog without telling the model what it
    /// said makes the model's picture of the page silently wrong — it sees a
    /// form reset with no explanation. `send` is used rather than `rpc` so a
    /// dialog can never make this the slow path.
    fn note_dialog(&mut self, m: &Value) {
        let params = m.get("params");
        let kind = params
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("dialog")
            .to_string();
        let text = params
            .and_then(|p| p.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let sess = m.get("sessionId").and_then(Value::as_str).map(str::to_string);
        if kind != "beforeunload" {
            if let (Some(sess), Some(params)) = (&sess, params) {
                self.dialogs.insert(sess.clone(), params.clone());
                return;
            }
        }
        let _ = self.send(
            "Page.handleJavaScriptDialog",
            json!({"accept": true}),
            sess.as_deref(),
        );
        self.notice(if text.is_empty() {
            format!("the page opened a JavaScript {kind} and it was accepted")
        } else {
            format!("the page opened a JavaScript {kind} saying \"{text}\" and it was accepted")
        });
    }

    /// The popup open on that tab, if one is (its opening event's params).
    pub fn dialog_on(&self, target_id: &str) -> Option<Value> {
        let sess = self.sessions.get(target_id)?;
        self.dialogs.get(sess).cloned()
    }

    /// Answer the popup open on `session`: OK, with a prompt's text when there is
    /// some, or Cancel. It is forgotten BEFORE the answer goes: the page it frees
    /// may open the next one at once (a page that reopens its alert), and that one
    /// must stay.
    pub fn answer_dialog(
        &mut self,
        session: &str,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<(), CdpFail> {
        let mut params = json!({"accept": accept});
        if let Some(text) = prompt_text {
            params["promptText"] = json!(text);
        }
        self.dialogs.remove(session);
        self.rpc("Page.handleJavaScriptDialog", params, Some(session), 5.0).map(|_| ())
    }

    /// A target died — its renderer crashed, or its session went away.
    fn note_gone(&mut self, method: &str, m: &Value) {
        if method == "Inspector.targetCrashed" {
            let Some(sess) = m.get("sessionId").and_then(Value::as_str).map(str::to_string) else {
                return;
            };
            // Reverse the map BEFORE forgetting the session, or the target id
            // this crash belongs to is lost with it.
            let target = self
                .sessions
                .iter()
                .find(|(_, s)| **s == sess)
                .map(|(t, _)| t.clone());
            if let Some(t) = target {
                self.crashed.insert(t);
            }
            self.dead.insert(sess.clone());
            self.forget_session(&sess);
            // The model is told when the tab is replaced, with what came back in its
            // place (ScannerInner::recover_tab).
            return;
        }
        // Target.detachedFromTarget names the session in its params, not the
        // envelope: the envelope is the PARENT the detach was reported to.
        if let Some(gone) = m
            .get("params")
            .and_then(|p| p.get("sessionId"))
            .and_then(Value::as_str)
        {
            self.dead.insert(gone.to_string());
            self.forget_session(gone);
        }
    }

    /// Queue something for the model, newest kept. Capped, because a page in a
    /// dialog loop would otherwise fill the run's memory with the same
    /// sentence — browser-use never clears theirs and it pollutes every later
    /// prompt.
    fn notice(&mut self, msg: String) {
        if self.notices.iter().any(|n| *n == msg) {
            return;
        }
        while self.notices.len() >= 8 {
            self.notices.pop_front();
        }
        self.notices.push_back(msg);
    }

    /// Everything that has happened since anyone last asked, and clear.
    pub fn take_notices(&mut self) -> Vec<String> {
        self.drain(NOTICE_DRAIN);
        self.notices.drain(..).collect()
    }

    /// Whether that tab's renderer died since the last time this was asked.
    ///
    /// Reads the wire FIRST. These events are only ever noticed by `stash`,
    /// which only runs when something reads the socket — so the answer used to
    /// depend on whether some unrelated call had happened to read recently.
    /// It worked while every navigation ended in a load wait that did the
    /// reading; the moment `navigate_to` learned to fail fast on errorText,
    /// nothing drained and a crash went unnoticed until the scan hit the dead
    /// renderer. Same reasoning as `is_loading` above: answer about the
    /// browser NOW, not about the last time somebody happened to look.
    pub fn take_crashed(&mut self, target_id: &str) -> bool {
        self.drain(NOTICE_DRAIN);
        self.crashed.remove(target_id)
    }

    /// Drop ONE session. The connection and every other session survive — the
    /// distinction `drop_conn` used not to make.
    fn forget_session(&mut self, session: &str) {
        self.sessions.retain(|_, s| s != session);
        self.loading.remove(session);
        self.dialogs.remove(session);
        self.frozen.remove(session);
    }

    /// Watch the calls into this session for a page that stops answering (`rpc`), or
    /// stop (None). Set for a scan (ScannerInner::on_page) and for `page_froze`; every
    /// other call keeps its own timeout.
    pub fn watch(&mut self, session: Option<&str>) {
        self.watching = session.map(str::to_string);
    }

    /// Whether that tab's page stopped answering during a watched call since the last
    /// time this was asked.
    pub fn take_frozen(&mut self, target_id: &str) -> bool {
        match self.sessions.get(target_id) {
            Some(sess) => self.frozen.remove(sess),
            None => false,
        }
    }

    /// Follow a session's main-frame navigation so `is_loading` can answer.
    ///
    /// Only the MAIN frame counts: a sub-frame navigating (an ad, an embed)
    /// does not mean the page the model is about to read is unfinished.
    fn note_load_state(&mut self, m: &Value) {
        let sess = match m.get("sessionId").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => return,
        };
        match m.get("method").and_then(Value::as_str) {
            Some("Page.frameNavigated") => {
                let is_main = m
                    .get("params")
                    .and_then(|p| p.get("frame"))
                    .map(|f| f.get("parentId").is_none())
                    .unwrap_or(false);
                // A page restored from the back/forward cache comes back whole
                // and fires no load event: marked loading, the tab stayed so
                // for good, and every switch to it waited out LOAD_TIMEOUT.
                if is_main && is_bfcache_restore(m, &sess) {
                    self.loading.remove(&sess);
                } else if is_main {
                    self.loading.insert(sess);
                }
            }
            Some("Page.loadEventFired") => {
                self.loading.remove(&sess);
            }
            _ => {}
        }
    }

    /// True while this tab's main frame has a navigation in flight.
    pub fn is_loading(&mut self, session: &str) -> bool {
        // Read whatever is already on the wire first, so the answer reflects
        // the browser now rather than the last time somebody happened to drain.
        self.drain(0);
        self.loading.contains(session)
    }

    /// Take every buffered event with this method name, oldest first.
    ///
    /// `session` scopes the take to ONE page. This socket is shared by every
    /// tab the agent has ever touched, so without it a sibling tab's event
    /// answers a wait meant for this one: another tab's `Page.loadEventFired`
    /// ends this tab's navigation wait early, and another tab's network
    /// chatter keeps `settle` from ever going quiet.
    ///
    /// `None` means "from any session", which is what cross-session discovery
    /// (`Target.attachedToTarget`, whose envelope names the PARENT session)
    /// actually wants.
    pub fn take_events(&mut self, method: &str, session: Option<&str>) -> Vec<Value> {
        let mut hit = Vec::new();
        self.events.retain(|e| {
            let matches = e.get("method").and_then(Value::as_str) == Some(method)
                && match session {
                    None => true,
                    Some(want) => e.get("sessionId").and_then(Value::as_str) == Some(want),
                };
            if matches {
                hit.push(e.clone());
                false
            } else {
                true
            }
        });
        hit
    }

    /// Drop every buffered event. Whatever is left after a wait is stale by
    /// definition, and left in place it grows for the life of the run.
    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// Drop every buffered event except those with one of these methods.
    pub fn clear_events_except(&mut self, keep: &[&str]) {
        self.events.retain(|e| {
            e.get("method")
                .and_then(Value::as_str)
                .is_some_and(|m| keep.contains(&m))
        });
    }

    /// Read what is waiting, stopping once the socket has been quiet for IDLE
    /// or `ms` total has elapsed — `ms` is a cap, not a sleep.
    ///
    /// The count-based `drain` above never blocks, which is right for a
    /// cosmetics pass tidying an idle socket. A loop WAITING for events (the
    /// scanner's settle) needs the opposite: give the wire a moment to speak,
    /// then come back. Spinning on a non-blocking drain would burn a core and
    /// still see nothing.
    pub fn drain_for(&mut self, ms: u64) {
        const IDLE: Duration = Duration::from_millis(60);
        let end = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(ws) = self.ws.as_mut() else { return };
            if ws
                .get_ref()
                .set_read_timeout(Some(IDLE.min(left)))
                .is_err()
            {
                self.drop_conn();
                return;
            }
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    match serde_json::from_str::<Value>(t.as_ref()) {
                        Ok(m) if m.get("method").is_some() => self.stash(m),
                        Ok(m) => {
                            if let Some(id) = m.get("id").and_then(Value::as_u64) {
                                self.hold_reply(id, m);
                            }
                        }
                        Err(_) => continue,
                    }
                }
                Ok(_) => continue,
                Err(_) => break, // read timeout: the socket has gone quiet
            }
        }
    }

    /// Block until this tab's main frame has finished loading: its load event,
    /// or its restore from the back/forward cache. A restored page comes back
    /// whole and fires no load event (Chrome reports a main-frame
    /// frameNavigated of type BackForwardCacheRestore instead), so waiting for
    /// the load event alone sat out the whole timeout on every back or forward
    /// to a cached page (measured: 30 s). True if either arrived.
    pub fn wait_loaded(&mut self, session: &str, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        loop {
            if self.take_loaded(session) {
                return true;
            }
            if Instant::now() >= deadline || self.ws.is_none() {
                return false;
            }
            // Gone (crashed, detached): never going to load. See wait_event.
            if !self.sessions.values().any(|v| v == session) {
                return false;
            }
            self.drain(250);
            if self.take_loaded(session) {
                return true;
            }
            sleep_s(0.02);
        }
    }

    /// Whether this tab's load event, or a main-frame restore from the
    /// back/forward cache, is buffered; taken if so.
    fn take_loaded(&mut self, session: &str) -> bool {
        let loaded = !self.take_events("Page.loadEventFired", Some(session)).is_empty();
        let before = self.events.len();
        self.events.retain(|e| !is_bfcache_restore(e, session));
        loaded || self.events.len() != before
    }

    /// Drop this tab's buffered load events, the restores included: they
    /// belong to an earlier navigation, and one of them would end the wait for
    /// the next one before it has started.
    pub fn forget_loads(&mut self, session: &str) {
        self.take_loaded(session);
    }

    /// Block until one `method` event arrives, or `timeout` runs out. True if
    /// it arrived. Used by navigation, which is the one action whose result
    /// the caller genuinely has to wait for.
    pub fn wait_event(&mut self, method: &str, session: Option<&str>, timeout: f64) -> bool {
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        loop {
            if !self.take_events(method, session).is_empty() {
                return true;
            }
            if Instant::now() >= deadline || self.ws.is_none() {
                return false;
            }
            // A session that has gone away — crashed, or detached — is never
            // going to fire this. `note_gone` drops it from the map the moment
            // Chrome says so, so its absence is the signal. Without this a
            // navigation whose renderer died still served out the full
            // LOAD_TIMEOUT: 30 seconds of waiting on a dead page.
            if session.is_some_and(|s| !self.sessions.values().any(|v| v == s)) {
                return false;
            }
            self.drain(250);
            if !self.take_events(method, session).is_empty() {
                return true;
            }
            sleep_s(0.02);
        }
    }

    /// Send a command and do NOT wait for its answer.
    ///
    /// CDP runs the commands on a session in the order they arrive, so a
    /// forgotten send still lands, and still lands in order relative to
    /// anything else sent on that session. The only thing given up is knowing
    /// whether it worked — which is precisely the contract cosmetics already
    /// have, since every caller swallowed the result anyway.
    ///
    /// Waiting is what made the glow expensive. A tab whose renderer is wedged
    /// answers nothing, so decorating it cost a full RPC timeout EACH PASS,
    /// and the scan queued behind it paid that. Measured: 45s per scan with a
    /// single looping sibling tab open. A send costs one write into a socket
    /// buffer whatever state the renderer is in.
    ///
    /// The id goes straight into `abandoned`, so if an answer does turn up
    /// `hold_reply` bins it instead of parking it in `replies` for the run.
    pub fn send(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<(), CdpFail> {
        self.connect()?;
        self.id += 1;
        let mid = self.id;
        let mut msg = json!({"id": mid, "method": method, "params": params});
        if let Some(sid) = session {
            msg["sessionId"] = Value::String(sid.to_string());
        }
        self.abandoned.insert(mid);
        let ws = self.ws.as_mut().expect("connected above");
        if ws
            .send(tungstenite::Message::Text(msg.to_string().into()))
            .is_err()
        {
            self.drop_conn();
            return Err(CdpFail::Lost(format!("{method}: connection lost")));
        }
        Ok(())
    }

    pub fn rpc(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: f64,
    ) -> Result<Value, CdpFail> {
        // A popup holds this page (see note_dialog): nothing sent into
        // it can be answered until the popup is, so the call fails at once rather
        // than waiting out its timeout. Answering the popup is the one call that goes.
        // The release of a key or button whose press opened the popup (Enter on a
        // form, a click on "Delete") counts as done: the press did the work, and
        // the frozen page could not take the release anyway.
        if session.is_some_and(|sid| self.dialogs.contains_key(sid))
            && method != "Page.handleJavaScriptDialog"
        {
            let kind = params.get("type").and_then(Value::as_str);
            let release = (method == "Input.dispatchKeyEvent" && kind == Some("keyUp"))
                || (method == "Input.dispatchMouseEvent" && kind == Some("mouseReleased"));
            if release {
                return Ok(json!({}));
            }
            return Err(CdpFail::Clean(format!("{method}: {DIALOG_OPEN}")));
        }
        if session.is_some_and(|sid| self.frozen.contains(sid)) {
            return Err(CdpFail::Clean(format!("{method}: {PAGE_FROZE}")));
        }
        // A watched call (`watch`: a scan's) sends a cheap question ahead of itself. The
        // page answers it before it starts on the call, so the call's own length (a heavy
        // page's snapshot) cannot hold the answer up; a page that has not answered it
        // after FROZEN_AFTER + PROBE_FOR could not start on anything: it froze, and the
        // call fails then rather than at its own timeout (measured: a frozen page held
        // each scan about 60 s). The answer comes in while the call is waited on anyway,
        // so on a page that answers it costs no time.
        let watched = session.is_some() && session == self.watching.as_deref();
        let mut asked: Option<(u64, Instant)> = None;
        if watched {
            // `send` files the id under `abandoned`, so an answer that comes after this
            // call is over is binned; one that comes during it is caught below.
            self.send("Runtime.evaluate", json!({"expression": "1"}), session)?;
            asked = Some((self.id, Instant::now() + FROZEN_AFTER + PROBE_FOR));
        }
        self.connect()?;
        self.id += 1;
        let mid = self.id;
        let mut msg = json!({"id": mid, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let ws = self.ws.as_mut().expect("connected above");
        ws.get_ref()
            .set_read_timeout(Some(Duration::from_secs_f64(timeout.max(0.001))))
            .ok();
        if ws
            .send(tungstenite::Message::Text(msg.to_string().into()))
            .is_err()
        {
            self.drop_conn();
            return Err(CdpFail::Lost(format!("{method}: connection lost")));
        }
        // The answer may already be in hand: a drain reads whatever is on the
        // wire, and that can include the reply to this very call.
        if let Some(m) = self.replies.remove(&mid) {
            if let Some(err) = m.get("error") {
                return Err(CdpFail::Clean(format!("{method} -> {err}")));
            }
            return Ok(m.get("result").cloned().unwrap_or(json!({})));
        }
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        while Instant::now() < deadline {
            // A confirmed-dead page will never answer, so waiting out the full
            // timeout is time spent learning nothing. The scan's RPCs use a 30s
            // ceiling, so without this a crash cost a full minute of nothing
            // before the retry could even begin.
            if session.is_some_and(|sid| self.dead.contains(sid)) {
                self.abandoned.insert(mid);
                return Err(CdpFail::Clean(format!(
                    "{method}: the page crashed while it was being read"
                )));
            }
            if watched {
                // Wake for the question's time as well as for the call's own deadline.
                let wake = asked.map_or(deadline, |(_, by)| by.min(deadline));
                let left = wake.saturating_duration_since(Instant::now());
                if let Some(ws) = self.ws.as_mut() {
                    ws.get_ref().set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok();
                }
            }
            let ws = self.ws.as_mut().expect("still connected in loop");
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    let m: Value = match serde_json::from_str(t.as_ref()) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    // Anything carrying a `method` is an event, not our
                    // answer — keep it for a caller that is waiting on one.
                    if m.get("method").is_some() {
                        self.stash(m);
                        // A popup opened on this very page while the call was out:
                        // the page is frozen until it is answered, so no reply will
                        // come before then. An input event was delivered (it is what
                        // opened the popup: a click on "Delete", an Enter on a form),
                        // so it counts as done; anything else fails like a call made
                        // after the popup opened.
                        if session.is_some_and(|sid| self.dialogs.contains_key(sid))
                            && method != "Page.handleJavaScriptDialog"
                        {
                            self.abandoned.insert(mid);
                            if method.starts_with("Input.") {
                                return Ok(json!({}));
                            }
                            return Err(CdpFail::Clean(format!("{method}: {DIALOG_OPEN}")));
                        }
                        continue;
                    }
                    // Flatten-mode ids are scoped per session; ours are
                    // globally unique AND used strictly one-in-flight, so
                    // id+session matches exactly one reply.
                    if m.get("id").and_then(Value::as_u64) == Some(mid)
                        && m.get("sessionId").and_then(Value::as_str) == session
                    {
                        if let Some(err) = m.get("error") {
                            // A clean refusal — the frame stream is still in
                            // step, so the connection survives.
                            return Err(CdpFail::Clean(format!("{method} -> {err}")));
                        }
                        return Ok(m.get("result").cloned().unwrap_or(json!({})));
                    }
                    // The page answered the question: it is working on the call, which
                    // now has its own time.
                    if let Some((probe, _)) = asked {
                        if m.get("id").and_then(Value::as_u64) == Some(probe) {
                            self.abandoned.remove(&probe);
                            asked = None;
                            continue;
                        }
                    }
                    // Someone else's answer — hold it rather than drop it.
                    if let Some(other) = m.get("id").and_then(Value::as_u64) {
                        self.hold_reply(other, m);
                    }
                }
                Ok(tungstenite::Message::Close(_)) => {
                    self.drop_conn();
                    return Err(CdpFail::Lost(format!("{method}: connection lost")));
                }
                Ok(_) => continue, // ping/pong/binary — tungstenite answers pings
                // OUR OWN read deadline, not a broken socket. Unix reports it
                // as WouldBlock and Windows as TimedOut; both mean "nothing
                // arrived in time". tungstenite keeps any partly-read frame in
                // its own buffer, so the stream is still in step and the next
                // read resumes exactly where this one stopped — which is why
                // `drain` has always been able to break here and carry on.
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let now = Instant::now();
                    if !watched || now >= deadline {
                        break;
                    }
                    if asked.is_some_and(|(_, by)| now >= by) {
                        // The question went unanswered: the page froze.
                        self.abandoned.insert(mid);
                        if let Some(sid) = session {
                            self.frozen.insert(sid.to_string());
                        }
                        return Err(CdpFail::Clean(format!("{method}: {PAGE_FROZE}")));
                    }
                    continue;
                }
                Err(_) => {
                    // Torn transport mid-frame: the stream can no longer be
                    // trusted to start at a frame boundary, so it must go.
                    self.drop_conn();
                    return Err(CdpFail::Lost(format!("{method}: connection lost")));
                }
            }
        }
        // One call did not answer in time. That says something about THIS
        // request — a busy renderer, a tab blocked on a modal dialog — and
        // nothing at all about the socket, so hanging up here was wrong: it
        // took every other tab's session, the event buffer and the pending
        // replies with it, and the next call rebuilt all of them only to time
        // out again the same way. That is how a cosmetic call against an
        // unrelated tab turned a 0.20s scan into 30.23s, and how one blocked
        // renderer wedged the whole agent.
        //
        // The frame stream is still in step, so the connection stays. The only
        // loose end is this call's answer, which may still arrive: its id goes
        // to `abandoned` so `hold_reply` bins it instead of parking it forever.
        self.abandoned.insert(mid);
        Err(CdpFail::Clean(format!("{method} timed out")))
    }

    /// Read whatever is already waiting, so an idle socket can never back up
    /// between passes. Events are buffered, not dropped.
    pub fn drain(&mut self, cap: usize) {
        let Some(ws) = self.ws.as_mut() else { return };
        if ws.get_ref().set_nonblocking(true).is_err() {
            self.drop_conn();
            return;
        }
        let mut seen: Vec<Value> = Vec::new();
        for _ in 0..cap {
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(t.as_ref()) {
                        if v.get("method").is_some() {
                            seen.push(v);
                        }
                    }
                }
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    break;
                }
                Err(_) => {
                    self.drop_conn();
                    return;
                }
            }
        }
        for v in seen {
            self.stash(v);
        }
        if let Some(ws) = self.ws.as_mut() {
            ws.get_ref().set_nonblocking(false).ok();
        }
    }

    /// SessionId for a tab, dialing and attaching only when needed.
    ///
    /// This is THE session for that tab: the glow rides on it and every tool
    /// borrows it to act. Nothing else opens one.
    pub fn attach(&mut self, target_id: &str) -> Result<String, CdpFail> {
        if let Some(s) = self.sessions.get(target_id) {
            return Ok(s.clone());
        }
        let r = self.rpc(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            None,
            5.0,
        )?;
        let s = r
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpFail::Lost("attach returned no sessionId".into()))?
            .to_string();
        self.sessions.insert(target_id.to_string(), s.clone());
        // A brand-new session is alive by definition, even if Chrome happens
        // to reuse an id we once buried.
        self.dead.remove(&s);
        // Page.enable is what makes navigation events (loadEventFired) arrive
        // at all, and the glow's document-start script fire. Focus emulation
        // keeps a backgrounded tab believing it is focused, which matters the
        // moment several agents share one browser: only one tab can really be
        // in front, and pages that gate on focus would misbehave for the rest.
        // Both are best-effort — a session that refuses either is still
        // perfectly usable for reading and clicking — so neither is worth
        // waiting on. Attaching to a tab with a wedged renderer used to cost
        // two full timeouts here before the caller got its session back.
        let _ = self.send("Page.enable", json!({}), Some(&s));
        let _ = self.send(
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled": true}),
            Some(&s),
        );
        Ok(s)
    }

    /// Evaluate an expression in the tab and hand back its value. The only
    /// things evaluated this way are the overlay's own entry points, which
    /// answer `undefined` on a page that has no overlay.
    pub fn eval(&mut self, session: &str, expression: String) -> Result<Value, CdpFail> {
        let r = self.rpc(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true}),
            Some(session),
            5.0,
        )?;
        Ok(r.get("result")
            .and_then(|v| v.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub fn forget(&mut self, target_id: &str) {
        self.sessions.remove(target_id);
    }
}

// ---------------------------------------------------------------------------
// Chrome launch
// ---------------------------------------------------------------------------

fn port_open(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}").parse();
    match addr {
        Ok(a) => TcpStream::connect_timeout(&a, Duration::from_millis(250)).is_ok(),
        Err(_) => false,
    }
}

/// How long Chrome gets to become driveable before we give up on it.
const READY_TIMEOUT: f64 = 30.0;

/// Viewport for a headless run, where there is no screen to maximise to.
const HEADLESS_WINDOW: &str = "1920,1080";
/// A headful window is placed on this share of the screen, centred, instead
/// of the whole of it, so the person has room around it. The page is drawn
/// at its own size: drawing it smaller (Emulation.setDeviceMetricsOverride
/// with a scale) made every scan's screenshot flash it to full size for a
/// few frames, since Chrome re-applies the emulation unscaled to capture.
const WINDOW_SHARE: f64 = 0.9;

/// Whether Chrome can actually be DRIVEN, not merely dialled.
///
/// `port_open` does not answer the question it looks like it answers. Chrome
/// binds the debug port and accepts connections on it well before the DevTools
/// HTTP endpoint will serve anything — measured at ~3.1s on a cold headful
/// start, and longer on a cold disk. Returning on `port_open` alone hands that
/// gap to whoever speaks first, which is the first scan: it sits inside
/// `chrome_http` waiting on a socket that is accepting but silent, and the wait
/// gets reported to the user as scan time.
///
/// The honest gate is the two things every caller after this needs: a socket
/// url to connect to, and a page to attach to. Nothing further is worth
/// waiting for — the tab Chrome starts on is about:blank, which is already
/// loaded by the time it is listed.
fn browser_ready(port: u16) -> bool {
    let Ok(ver) = chrome_http(port, "/json/version", "GET") else {
        return false;
    };
    ver.get("webSocketDebuggerUrl")
        .and_then(Value::as_str)
        .is_some_and(|u| !u.is_empty())
        && !page_targets(port).is_empty()
}

/// Block until `browser_ready`, or `secs` runs out. True if it came up.
///
/// A deadline, not a try count: `chrome_http` blocks for up to its own 5s read
/// timeout against a port that is accepting but silent, so counting attempts
/// would bound this at minutes rather than seconds.
fn await_browser_ready(port: u16, secs: f64) -> SResult<bool> {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    loop {
        if browser_ready(port) {
            return Ok(true);
        }
        if Instant::now() >= end {
            return Ok(false);
        }
        check_py_signals()?;
        sleep_s(0.05);
    }
}

/// Every place a Chrome-family browser may be installed, most preferred first.
fn chrome_candidates() -> Vec<PathBuf> {
    if !cfg!(windows) {
        let mut v: Vec<PathBuf> = CHROME_PATHS_UNIX.iter().map(PathBuf::from).collect();
        // Where utils/chrome.py puts Chrome when /Applications is not writable.
        v.insert(1, home_dir().join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        return v;
    }
    let roots: Vec<String> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .collect();
    let mut v = Vec::with_capacity(CHROME_RELATIVE_WIN.len() * roots.len());
    // Browser-major order: Chrome under any root beats Chromium under any root.
    for rel in CHROME_RELATIVE_WIN {
        for root in &roots {
            v.push(PathBuf::from(root).join(rel));
        }
    }
    v
}

/// The browser both modes drive: Google Chrome, or a Chromium or Edge found in
/// its place. Extension mode's launch (bridge.rs) starts the same one.
pub(crate) fn find_chrome() -> SResult<PathBuf> {
    for path in chrome_candidates() {
        if path.exists() {
            return Ok(path);
        }
    }
    // Linux installs usually expose a launcher on PATH rather than a fixed
    // location. which() appends .exe on Windows, so these cost nothing there.
    which("google-chrome")
        .or_else(|| which("google-chrome-stable"))
        .or_else(|| which("chromium"))
        .or_else(|| which("chrome"))
        .ok_or_else(|| ScanErr::s("no browser found: install Google Chrome to use web mode"))
}

/// Name the profile and give it a face, before Chrome ever opens it.
///
/// Chrome writes `Local State` from MEMORY when it exits, so editing the file
/// while it is running is simply thrown away — measured. Seeding the file
/// BEFORE the first launch works: Chrome reads the entry, merges its own keys
/// around it, and keeps the name and the avatar.
///
/// Also measured, and worth writing down so nobody tries it again: a CUSTOM
/// avatar image cannot be set this way. Chrome only honours
/// `Google Profile Picture.png` when the profile is signed in to a Google
/// account — with an empty `gaia_id` it keeps the setting, ignores the file,
/// and the toolbar still reads "Sign in to Chrome?". A stock
/// `IDR_PROFILE_AVATAR_*` does render, so that is what is used. AutoCua's own
/// logo reaches the browser where it actually can: as the favicon of the
/// agent's page, embedded in logo/logo.html.
fn brand_profile(profile: &PathBuf) {
    let state_path = profile.join("Local State");
    let mut state: Value = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));

    let entry = state
        .as_object_mut()
        .map(|o| o.entry("profile").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut)
        .map(|o| o.entry("info_cache").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut)
        .map(|o| o.entry("Default").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut);
    let Some(entry) = entry else { return };

    // Only ever brand a profile still carrying Chrome's own default name.
    // Renaming one the user has since renamed themselves would be rude, and
    // this runs on every launch, not just the first.
    let default_named = entry
        .get("is_using_default_name")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !default_named {
        return;
    }
    entry.insert("name".into(), json!("AutoCua"));
    entry.insert("is_using_default_name".into(), json!(false));
    entry.insert(
        "avatar_icon".into(),
        json!("chrome://theme/IDR_PROFILE_AVATAR_31"),
    );
    entry.insert("is_using_default_avatar".into(), json!(false));
    if let Ok(text) = serde_json::to_string(&state) {
        let _ = std::fs::write(&state_path, text);
    }
}

/// Whether another live Chrome already holds this profile directory.
///
/// Chrome's ProcessSingleton locks a user-data-dir: a second Chrome launched
/// against a locked one hands its command line to the first instance and
/// EXITS, so its --remote-debugging-port never binds. Without this check that
/// surfaces as `await_browser_ready` serving out its full timeout and then
/// reporting "Chrome did not open the debug port", which says nothing about
/// the actual cause. On macOS and Linux the lock is a symlink named
/// `<host>-<pid>`; a stale one from a crash points at a pid that is gone.
/// Windows has no such symlink: there Chrome keeps `lockfile` open for
/// writing, shared for reading only and deleted when it closes, so opening it
/// for writing fails with a sharing violation (32) while that Chrome lives and
/// finds no file once it is gone.
pub(crate) fn profile_locked_by_live_chrome(profile: &PathBuf) -> bool {
    if cfg!(windows) {
        return std::fs::OpenOptions::new()
            .write(true)
            .open(profile.join("lockfile"))
            .is_err_and(|e| e.raw_os_error() == Some(32));
    }
    let Ok(target) = std::fs::read_link(profile.join("SingletonLock")) else {
        return false;
    };
    let text = target.to_string_lossy().to_string();
    let Some(pid) = text.rsplit('-').next().and_then(|p| p.parse::<u32>().ok()) else {
        return false;
    };
    // Running, and not a zombie. A browser that crashed or was killed stays in
    // the process table as a zombie until the process that started it
    // collects it, which this crate never does: counted as alive (`kill -0`
    // does), it blocked every relaunch from that process, the desktop app's
    // included, until the app restarted (measured). ps shows a zombie's state
    // as Z, and nothing at all for a pid that is gone. A subprocess rather
    // than a libc call so the crate keeps its dependency list; this runs once
    // per launch, never in a loop.
    Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
        .map(|out| {
            let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        })
        .unwrap_or(false)
}

/// Place a headful window on WINDOW_SHARE of its screen, centred. The screen
/// is read from the browser itself (screen.avail*), so it holds on every
/// platform and display. Best-effort: a window that stays maximised costs
/// nothing but room around it.
fn place_window(port: u16) {
    let attempt = (|| -> Result<(), CdpFail> {
        let mut cdp = Cdp::new(port);
        let targets = cdp.rpc("Target.getTargets", json!({}), None, 5.0)?;
        let page = targets
            .get("targetInfos")
            .and_then(Value::as_array)
            .and_then(|ts| ts.iter().find(|t| t.get("type").and_then(Value::as_str) == Some("page")))
            .and_then(|t| t.get("targetId").and_then(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| CdpFail::Clean("no page to measure the screen from".into()))?;
        let sess = cdp.attach(&page)?;
        let r = cdp.rpc(
            "Runtime.evaluate",
            json!({"expression": "[screen.availLeft, screen.availTop, screen.availWidth, screen.availHeight]",
                   "returnByValue": true}),
            Some(&sess),
            5.0,
        )?;
        let avail: Vec<f64> = r
            .get("result")
            .and_then(|v| v.get("value"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let [left, top, width, height] = avail[..] else {
            return Err(CdpFail::Clean("no screen size".into()));
        };
        if width < 400.0 || height < 300.0 {
            return Ok(()); // too small to be worth leaving room on
        }
        let window = cdp.rpc("Browser.getWindowForTarget", json!({"targetId": page}), None, 5.0)?;
        let id = window
            .get("windowId")
            .cloned()
            .ok_or_else(|| CdpFail::Clean("no window for the page".into()))?;
        // Out of "maximized" first: bounds handed to a maximised window are
        // ignored. And wait until it is out: on X11 the window manager
        // answers the un-maximize after this call returns, and bounds sent
        // before that were replaced by its own restore size (measured on
        // GNOME: 945x961 for bounds sent back to back, the right size once
        // the state read "normal").
        cdp.rpc(
            "Browser.setWindowBounds",
            json!({"windowId": id, "bounds": {"windowState": "normal"}}),
            None,
            5.0,
        )?;
        for _ in 0..20 {
            let now = cdp.rpc("Browser.getWindowBounds", json!({"windowId": id}), None, 5.0)?;
            if now.pointer("/bounds/windowState").and_then(Value::as_str) == Some("normal") {
                break;
            }
            sleep_s(0.05);
        }
        let margin = (1.0 - WINDOW_SHARE) / 2.0;
        cdp.rpc(
            "Browser.setWindowBounds",
            json!({"windowId": id, "bounds": {
                "left": (left + width * margin) as i64,
                "top": (top + height * margin) as i64,
                "width": (width * WINDOW_SHARE) as i64,
                "height": (height * WINDOW_SHARE) as i64,
                "windowState": "normal",
            }}),
            None,
            5.0,
        )?;
        Ok(())
    })();
    let _ = attempt;
}

/// "On startup: Open the New Tab page" (session.restore_on_startup). Brave's
/// own default is 1, "Continue where you left off" (measured): every launch
/// reopened every tab of the one before, and they piled up run after run.
const RESTORE_NEW_TAB: i64 = 5;

/// Whether the profile is already set to open on the New Tab page, read
/// before launch. Brave keeps the setting in Secure Preferences (measured);
/// Preferences is checked too, for a browser that keeps it there.
fn opens_on_new_tab(profile: &PathBuf) -> bool {
    ["Secure Preferences", "Preferences"].iter().any(|file| {
        std::fs::read_to_string(profile.join("Default").join(file))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|p| p.pointer("/session/restore_on_startup").and_then(Value::as_i64))
            == Some(RESTORE_NEW_TAB)
    })
}

/// Set "On startup" to "Open the New Tab page" the way the settings page
/// does, through the browser itself: a settings tab in the background, one
/// setPref, then the tab closes. Writing the value into Preferences does not
/// work: the setting is signed (Secure Preferences), and the browser throws an
/// edited value away on load (measured). Best-effort.
fn set_startup_new_tab(port: u16) {
    let mut cdp = Cdp::new(port);
    let Ok(created) = cdp.rpc(
        "Target.createTarget",
        json!({"url": "chrome://settings/", "background": true}),
        None,
        5.0,
    ) else {
        return;
    };
    let Some(id) = created.get("targetId").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let _ = (|| -> Result<(), CdpFail> {
        let sess = cdp.attach(&id)?;
        // The settings page's API exists once the page has loaded.
        for _ in 0..50 {
            let ready = cdp
                .eval(&sess, "typeof chrome !== 'undefined' && !!chrome.settingsPrivate".into())
                .unwrap_or(Value::Null);
            if ready == Value::Bool(true) {
                break;
            }
            sleep_s(0.1);
        }
        cdp.rpc(
            "Runtime.evaluate",
            json!({"expression": format!(
                "new Promise(r => chrome.settingsPrivate.setPref(\
                 'session.restore_on_startup', {RESTORE_NEW_TAB}, '', ok => r(ok)))"),
                   "awaitPromise": true, "returnByValue": true}),
            Some(&sess),
            5.0,
        )?;
        Ok(())
    })();
    let _ = cdp.rpc("Target.closeTarget", json!({"targetId": id}), None, 5.0);
    cdp.forget(&id);
}

pub fn launch_chrome_impl(
    port: u16,
    headless: bool,
    profile_dir: Option<PathBuf>,
) -> SResult<bool> {
    if port_open(port) {
        // Attached, not launched — but a browser someone started a moment ago
        // is in exactly the state `browser_ready` describes, so this path has
        // to wait it out too rather than assume an open port means a usable one.
        if !await_browser_ready(port, READY_TIMEOUT)? {
            return Err(ScanErr::s(format!(
                "Chrome holds port {port} but its DevTools endpoint never answered"
            )));
        }
        println!("Attached to Chrome already on port {port}");
        return Ok(false);
    }

    // Keyed by NAME, not by port. The old `~/.AutoCua/chrome-<port>` scheme
    // made a brand-new profile for every port the agent ever used — 22 of them
    // and 3.6 GB on the machine this was written on — so nothing was ever
    // logged in twice and every run started from a cold, empty browser.
    let profile = match profile_dir {
        Some(p) => p,
        None => home_dir().join(".AutoCua").join(format!("chrome-{port}")),
    };
    let is_new = !profile.join("Local State").exists();
    std::fs::create_dir_all(&profile)
        .map_err(|e| ScanErr::s(format!("could not create {}: {e}", profile.display())))?;
    if profile_locked_by_live_chrome(&profile) {
        return Err(ScanErr::s(format!(
            "another Chrome is already using the profile at {} — close it, or run \
             with a different browser profile",
            profile.display()
        )));
    }
    // Before the spawn, while nothing owns the file. Chrome would overwrite an
    // edit made after it starts.
    brand_profile(&profile);
    // Not on the New Tab page yet (a new profile, or one from before this was
    // set): clear the saved session too, so nothing the last run left open
    // comes back even on this one launch. The setting itself is set once the
    // browser is up (set_startup_new_tab), and from then on this is skipped.
    let on_new_tab = opens_on_new_tab(&profile);
    if !on_new_tab {
        let _ = std::fs::remove_dir_all(profile.join("Default").join("Sessions"));
    }
    let chrome = find_chrome()?;
    let name = browser_name(&chrome);
    let mut cmd = Command::new(chrome);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-backgrounding-occluded-windows")
        .arg("--disable-renderer-backgrounding")
        .arg("--disable-background-timer-throttling");
    // Windows: Chrome's field-trial seed turns on Skia Graphite (its newer
    // GPU rasterizer) on some machines, and with it every
    // Page.captureScreenshot takes about 1.4 s instead of 0.04 s (measured
    // on Chrome 153, a 2560x1600 page, an RTX 5080 laptop). The agent
    // takes one per scan, so stay on the rasterizer that answers at once.
    // Windows only: on Apple Silicon, Graphite (Metal) is Chrome's only GPU
    // path, and this flag drops the whole browser to software rendering
    // (measured: captures 0.06 s -> 0.14 s on an M5).
    if cfg!(windows) {
        cmd.arg("--disable-features=SkiaGraphite");
    }
    // Linux, headful: frames on a timer instead of vsync. Under XWayland a
    // window that is fully covered gets no frame callbacks, and every
    // screenshot of it then waited 1.3 to 3 s for a frame (measured, GNOME
    // 50, Chrome 153); on a timer they take about 50 ms, covered or not. A
    // native Wayland window, the default on a Wayland desktop, was not
    // measured either way.
    if cfg!(target_os = "linux") && !headless {
        cmd.arg("--disable-gpu-vsync");
    }
    // The window is not just where the agent works — it IS what the model
    // sees. The screenshot is the viewport, so Chrome's default restore size
    // costs the model page area and elements on every single scan. Ask for
    // the whole screen rather than accept it: a size that exists on any
    // display. Once the browser is up, a headful window is then placed on
    // WINDOW_SHARE of the screen (place_window), leaving room around it.
    if headless {
        // Nothing to maximise against without a display, so name a size.
        cmd.arg(format!("--window-size={HEADLESS_WINDOW}"));
    } else {
        cmd.arg("--start-maximized");
    }
    if headless {
        cmd.arg("--headless=new");
    }
    // Start ON about:blank rather than letting Chrome show its New Tab page,
    // so the very first surface the agent sees is inert.
    cmd.arg(BLANK_URL);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn()
        .map_err(|e| ScanErr::s(format!("could not launch Chrome: {e}")))?;

    if await_browser_ready(port, READY_TIMEOUT)? {
        if !headless {
            place_window(port);
        }
        if !on_new_tab {
            set_startup_new_tab(port);
        }
        let mode = if headless { "headless" } else { "headful" };
        if is_new {
            println!("Created browser profile at {}", profile.display());
        }
        println!("{name} launched on port {port} ({mode})");
        return Ok(true);
    }
    Err(ScanErr::s(format!("{name} did not open the debug port {port}")))
}

// ---------------------------------------------------------------------------
// Windows: the tab on show, moved with Chrome's own browser commands.
// ---------------------------------------------------------------------------

/// The Windows side of `show_tab`.
///
/// A key chord sent over CDP (Input.dispatchKeyEvent) reaches the tab strip
/// on Windows only while the browser window is the active one: behind
/// another window, Ctrl+PageDown, Ctrl+Tab and Ctrl+2 all did nothing
/// (measured, Brave 154, Windows 11), and that is exactly when the person is
/// watching from another window. Chrome's window does take its own browser
/// commands as a plain WM_COMMAND (HWNDMessageHandler::OnCommand, then
/// BrowserView::ExecuteWindowsCommand, then chrome::ExecuteCommand), active
/// or not: IDC_SELECT_NEXT_TAB switched the tab in 6 to 8 ms, /json/list
/// listed the new tab first about 10 ms later, the window never became the
/// foreground one, and a text field focused in the page kept its focus
/// (measured). Also measured and not used: Target.activateTarget and a UI
/// Automation press on the tab in the strip both switched the tab, and both
/// pulled Brave to the front from behind.
#[cfg(windows)]
pub(crate) mod win_tabs {
    use std::ffi::c_void;

    type Hwnd = *mut c_void;
    type EnumProc = unsafe extern "system" fn(Hwnd, isize) -> i32;

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(callback: EnumProc, lparam: isize) -> i32;
        fn GetWindow(hwnd: Hwnd, cmd: u32) -> Hwnd;
        fn IsWindowVisible(hwnd: Hwnd) -> i32;
        fn GetClassNameW(hwnd: Hwnd, name: *mut u16, len: i32) -> i32;
        fn GetWindowTextLengthW(hwnd: Hwnd) -> i32;
        fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, len: i32) -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn GetWindowRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
        fn GetDpiForWindow(hwnd: Hwnd) -> u32;
        fn SendMessageTimeoutW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize, flags: u32,
                               timeout_ms: u32, result: *mut usize) -> isize;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLastError() -> u32;
    }

    const GW_OWNER: u32 = 4;
    const WM_COMMAND: u32 = 0x0111;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;
    const ERROR_ACCESS_DENIED: u32 = 5;
    /// The class of every Chromium top-level window (Chrome and Brave alike).
    const WINDOW_CLASS: &str = "Chrome_WidgetWin_1";
    /// chrome/app/chrome_command_ids.h; the same in Brave.
    pub const IDC_SELECT_NEXT_TAB: usize = 34016;

    /// Every top-level window, front to back, as one snapshot. Walking the
    /// z-order with GetWindow(GW_HWNDNEXT) instead can revisit a window that
    /// moved up meanwhile or stop at one that closed (Microsoft's own note
    /// on GetWindow), and a Brave behind the person's window is exactly the
    /// one such a walk would drop.
    fn all_windows() -> Vec<usize> {
        unsafe extern "system" fn push(hwnd: Hwnd, list: isize) -> i32 {
            // `list` is the Vec below, which outlives the EnumWindows call.
            (*(list as *mut Vec<usize>)).push(hwnd as usize);
            1
        }
        let mut out: Vec<usize> = Vec::new();
        unsafe { EnumWindows(push, &mut out as *mut Vec<usize> as isize) };
        out
    }

    fn class_of(hwnd: Hwnd) -> String {
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.clamp(0, buf.len() as i32) as usize])
    }

    /// A window's caption, however long: a tab with no title of its own is
    /// titled by its url, which can run to thousands of characters, and a
    /// caption cut short loses the " - Brave" that shown_title splits on.
    fn caption_of(hwnd: Hwnd) -> String {
        let len = unsafe { GetWindowTextLengthW(hwnd) }.max(0);
        let mut buf = vec![0u16; len as usize + 1];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.clamp(0, len) as usize])
    }

    /// The browser windows (frames) of process `pid`, front to back, each
    /// with its caption: the tab it shows, then " - Brave". Only windows
    /// with no owner: Chrome's bubbles (a download, a translate or a
    /// save-password prompt), its menus, the omnibox drop-down and tooltips
    /// are windows of the same class and visible while open, but each is
    /// owned by its frame, where a frame, a pop-up window a page opened
    /// included, is owned by nothing.
    pub fn windows_of(pid: u32) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for hwnd in all_windows() {
            let h = hwnd as Hwnd;
            let mut owner_pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, &mut owner_pid) };
            if owner_pid == pid
                && unsafe { IsWindowVisible(h) } != 0
                && unsafe { GetWindow(h, GW_OWNER) }.is_null()
                && class_of(h) == WINDOW_CLASS
            {
                out.push((hwnd, caption_of(h)));
            }
        }
        out
    }

    /// The title of the tab a browser window's caption names: the caption
    /// less the browser's name after the last " - " and less the direction
    /// marks a right-to-left UI wraps it in. Chromium is said to write every
    /// & of a caption doubled, so Windows does not read it as a mnemonic;
    /// Brave 154 did not (measured: "sony & 'audio'" and "Q&&A" came
    /// through as written). Both forms come back, so either behaviour
    /// matches, and a title holding "&&" of its own matches as itself.
    pub fn shown_titles(caption: &str) -> [String; 2] {
        let mark = |c: char| {
            matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        };
        let caption = caption.trim_matches(mark);
        let title = caption.rsplit_once(" - ").map_or(caption, |(t, _)| t).trim_matches(mark);
        [title.to_string(), title.replace("&&", "&")]
    }

    /// A /json/list title as the page has it. Chrome serves that listing
    /// HTML-escaped (& as &amp;, ' as &#39;, and < > " likewise), where a
    /// window's caption is not.
    pub fn listed_title(title: &str) -> String {
        title
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    /// The one window of `windows` whose frame sits where Chrome says the
    /// target's window does (`bounds`: left, top, width and height in DIPs,
    /// from Browser.getWindowForTarget), or None when none or several do.
    /// Chrome's DIPs are the window's pixels over its monitor's scale
    /// (measured: 1722 wide by Chrome, 2582 px at 150%); a process not
    /// declared DPI-aware is handed the pixels scaled already, so both
    /// readings are tried. Two windows the person cascaded sit 10 px apart,
    /// so the tolerance stays under that.
    pub fn frame_at(windows: &[(usize, String)], bounds: [f64; 4]) -> Option<usize> {
        const TOLERANCE: f64 = 4.0;
        let close = |rect: [f64; 4]| {
            rect.iter()
                .zip(bounds.iter())
                .all(|(a, b)| (a - b).abs() <= TOLERANCE)
        };
        let mut found = windows.iter().filter(|(hwnd, _)| {
            let h = *hwnd as Hwnd;
            let mut r = Rect { left: 0, top: 0, right: 0, bottom: 0 };
            if unsafe { GetWindowRect(h, &mut r) } == 0 {
                return false;
            }
            let raw = [
                r.left as f64,
                r.top as f64,
                (r.right - r.left) as f64,
                (r.bottom - r.top) as f64,
            ];
            let scale = unsafe { GetDpiForWindow(h) }.max(96) as f64 / 96.0;
            close(raw) || close(raw.map(|v| v / scale))
        });
        match (found.next(), found.next()) {
            (Some((hwnd, _)), None) => Some(*hwnd),
            _ => None,
        }
    }

    /// How a browser command fared.
    pub enum Sent {
        /// The browser handled it.
        Done,
        /// Never delivered: Windows refused the message (UIPI, a browser
        /// running elevated takes none from a normal process).
        Refused,
        /// Delivered but not handled within a second, or the window is
        /// hung. The message stays in the browser's queue, so it may still
        /// act on it later.
        Late,
    }

    /// Run one of Chrome's browser commands in that window.
    pub fn command(hwnd: usize, id: usize) -> Sent {
        let mut result = 0usize;
        let handled = unsafe {
            SendMessageTimeoutW(hwnd as Hwnd, WM_COMMAND, id, 0, SMTO_ABORTIFHUNG, 1000, &mut result)
        };
        if handled != 0 {
            Sent::Done
        } else if unsafe { GetLastError() } == ERROR_ACCESS_DENIED {
            Sent::Refused
        } else {
            Sent::Late
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: the tab on show, moved through the browser's own tabs API.
// ---------------------------------------------------------------------------

/// A page of the browser's own that is handed the tabs API (chrome.tabs):
/// the bookmarks page, in Brave and in Chrome alike.
#[cfg(target_os = "linux")]
const TABS_API_PAGE: &str = "chrome://bookmarks/";

/// The Linux side of `show_tab`: make one tab the tab its window shows, and
/// say whether it is.
///
/// The key chord reaches the tab strip on Linux only while the browser
/// window is the active one, as on Windows: behind another window
/// Ctrl+PageDown over CDP moved nothing, and the page was handed the key
/// press instead (measured, Brave 154, GNOME 50 on Wayland). There is no
/// window message to send here, and a Wayland client cannot reach another
/// client's window at all. The browser's tabs API does what the strip does:
/// chrome.tabs.update(id, {active: true}) shows the tab and never asks for
/// the window. Its own pages are handed that API, so a bookmarks tab is
/// opened in the background, runs one script and is closed: a switch_tab
/// took 69 to 108 ms in all, the window's title followed every switch, the
/// browser stayed behind and gnome-shell showed no "is ready" banner
/// (measured, as a Wayland window and as an X11 one, and in Chrome 153).
/// Not a hidden target (Target.createTarget hidden): one on this page took
/// the whole browser down (measured).
///
/// The tabs API counts tabs in its own ids, not in CDP's. The tab is found
/// by its window (CDP's window id is the API's) and its url. Several tabs
/// of that window on one url (`twins` of them: two blank tabs, say) are told
/// apart by their order: Chrome's listing and the API's lastAccessed are the
/// same clock, so the target is the one in the same `place`, newest first.
/// Anything that does not add up is false, and show_tab falls back to the
/// chord.
#[cfg(target_os = "linux")]
fn show_by_tabs_api(port: u16, window: &Value, url: &str, twins: usize, place: usize) -> bool {
    let mut cdp = Cdp::new(port);
    let Ok(created) = cdp.rpc(
        "Target.createTarget",
        json!({"url": TABS_API_PAGE, "background": true}),
        None,
        5.0,
    ) else {
        return false;
    };
    let Some(id) = created.get("targetId").and_then(Value::as_str).map(str::to_string) else {
        return false;
    };
    let shown = (|| -> Result<bool, CdpFail> {
        let sess = cdp.attach(&id)?;
        // The API exists once the page's own document is in (35 ms after
        // the tab opened, measured).
        let opened = Instant::now();
        loop {
            let api = cdp
                .eval(&sess, "typeof chrome !== 'undefined' && !!chrome.tabs && !!chrome.tabs.query".into())
                .unwrap_or(Value::Null);
            if api == Value::Bool(true) {
                break;
            }
            if opened.elapsed() > Duration::from_secs(2) {
                return Ok(false);
            }
            sleep_s(0.01);
        }
        let url = Value::String(url.to_string());
        let reply = cdp.rpc(
            "Runtime.evaluate",
            json!({"expression": format!(
                "new Promise(done => chrome.tabs.getCurrent(me => \
                 chrome.tabs.query({{windowId: {window}}}, tabs => {{\
                   const twins = (tabs || [])\
                     .filter(t => !(me && t.id === me.id) && (t.url === {url} || t.pendingUrl === {url}))\
                     .sort((a, b) => b.lastAccessed - a.lastAccessed);\
                   const tab = twins.length === {twins} ? twins[{place}] : null;\
                   if (!tab) return done(false);\
                   if (tab.active) return done(true);\
                   chrome.tabs.update(tab.id, {{active: true}}, () => done(!chrome.runtime.lastError));\
                 }})))"),
                   "awaitPromise": true, "returnByValue": true}),
            Some(&sess),
            5.0,
        )?;
        Ok(reply.pointer("/result/value") == Some(&Value::Bool(true)))
    })()
    .unwrap_or(false);
    let _ = cdp.rpc("Target.closeTarget", json!({"targetId": id}), None, 5.0);
    // Out of Chrome's listing before this returns: the callers list the
    // tabs next, and the closed tab stayed listed for up to 10 ms after the
    // close was answered (measured).
    for _ in 0..200 {
        if !tab_exists(port, &id) {
            break;
        }
        sleep_s(0.005);
    }
    shown
}

// ---------------------------------------------------------------------------
// The scanner process + its scan state. Pure Rust, no GIL held during I/O —
// the pyclass wrapper below releases the GIL around every call in here.
// ---------------------------------------------------------------------------

pub struct ScannerInner {
    pub port: u16,
    pub out_dir: PathBuf,
    logo_page: PathBuf,
    glow_css: PathBuf,
    glow_js: PathBuf,
    /// Scan tuning: the embedded defaults with tree/element.config.json
    /// merged over them, loaded once and reloadable by hand.
    cfg: Value,
    cfg_path: PathBuf,
    /// Sessions whose scan domains are already on. A session is long-lived and
    /// enabling them is a round trip, so it happens once per tab.
    prepared: HashSet<String>,
    /// Whether scans come back with numbered marks painted on.
    marks: bool,
    /// Scans so far, for DEBUG's per-scan folders.
    scan_count: usize,
    pub tree_text: String,
    /// The popup the last scan showed in place of the page (`dialog_view`), when
    /// one held it; None after a scan of the page itself.
    pub dialog_shown: Option<String>,
    pub image_b64: Option<String>,
    /// The same frame with no marks on it, for the app's image panel. See
    /// scan_elements for which of the two the frontend callback is handed.
    pub plain_b64: Option<String>,
    pub all_tabs: String,
    pub url: String,
    pub mapping: serde_json::Map<String, Value>,
    /// device px per CSS px, from the most recent scan.
    pub dpr: f64,
    /// Wall time of the most recent scan_elements(), in seconds.
    pub last_scan_seconds: f64,
    /// True once the controller has waited on the page after the latest
    /// action (see `keep_action_requests`): the next scan keeps that
    /// action's request events.
    keep_requests: bool,
    cdp: Cdp,
    /// Extension mode: the bridge to AutoCuaBridge, in place of the port and
    /// `cdp` (see "extension mode" below). None in debugging-port mode.
    bridge: Option<Arc<Mutex<Bridge>>>,
    /// Where the cursor was last sent, in CSS px. glow.js is re-run from
    /// scratch in every new tab and after every navigation, so the position
    /// cannot live in the page — this is the only side that outlives a
    /// document, and `cursor_sync` replays it into whatever is on screen now.
    last_cursor: Option<(f64, f64)>,
    /// False in fast mode: no cursor is drawn, placed or spoken for, and
    /// `cursor_to` answers false so every tool skips its animation waits
    /// and the batch runs at the page's own pace (see controller settle).
    cursor_enabled: bool,
    /// The tab wearing scraping mode's look (glow.css "scraping mode"): set
    /// when `scrape` starts reading it, None once the mode is over. Held here
    /// for the reason `last_cursor` is: a document that loads during the mode
    /// starts without the look, and `scrape_glow_sync` puts it back.
    scrape_tab: Option<String>,
    /// sessionId -> targetId for tabs whose glow script is registered.
    glow_armed: HashMap<String, String>,
    glow_gen: u64,
    /// Parallel-run mode: this agent creates and drives exactly ONE tab of
    /// its own in a browser shared with other agents. Tab listing, cosmetics
    /// and the current-target lookup are all scoped to that tab.
    single_tab: bool,
    /// The tabs of the CURRENT listing, in the order `<all_tabs>` shows them.
    ///
    /// Filled by `read_tabs`, so the ids and the text the model reads come out
    /// of ONE listing. `[n]` resolves against this and never against a fresh
    /// /json/list: Chrome orders that most-recently-USED and every tab switch
    /// reorders it, so re-reading it between the scan and the action can hand
    /// back a different tab than the one the model picked. Bounds-checking the
    /// old list and then indexing the new one is how `close_tab` could close a
    /// tab nobody asked to close.
    tab_ids: Vec<String>,
    /// Every tab this agent has seen, in the order it first saw them — the
    /// order `<all_tabs>` numbers against, and the only stable one available.
    tab_order: Vec<String>,
    /// The tab this agent is driving, by CDP target id.
    ///
    /// Tracked in BOTH modes, and authoritative: the scanner is bound to it by
    /// id, every tool acts on it, and the glow decorates it. It used to be
    /// single-tab-only, with multi-tab runs guessing "whichever target
    /// /json/list puts first" — a guess that was only ever right because the
    /// scanner had just called bringToFront on its own tab.
    tab_id: Option<String>,
    /// How to start the browser again if it goes away mid-run (crashed,
    /// killed, quit): headless or not, and its profile. Set by AgentService;
    /// None means this scanner never relaunches it (see `revive`).
    relaunch: Option<(bool, Option<PathBuf>)>,
    /// This agent's tabs as of the last listing, left to right, as (target
    /// id, url), and which of them it was driving: what `revive` reopens.
    saved_tabs: Vec<(String, String)>,
    saved_current: usize,
    /// Where the agent last sent each tab (new_tab, update_tab), by target
    /// id: a tab still blank because that page was loading is reopened there.
    sent: HashMap<String, String>,
    /// Pages opened again once already after their tab crashed or froze
    /// (`recover_tab`), and the tabs opened for them: one that does it again gets a
    /// blank tab. A tab the agent sends somewhere else is a fresh start (`sent_to`).
    reopened_urls: HashSet<String>,
    reopened_tabs: HashSet<String>,
    /// The browser this agent last worked in, by the id in its DevTools
    /// url (new with every launch). See `start`.
    browser: Option<String>,
    /// Windows: the browser's process id, for finding its windows (see
    /// win_tabs). Asked of the browser when first needed; a relaunch
    /// (revive) forgets it, since the new browser has another.
    #[cfg(windows)]
    browser_pid: Option<u32>,
}

// No Drop impl: the pump threads own the read ends now, and they exit on EOF
// when the child's pipes close. There is no raw fd left to hand back.

impl ScannerInner {
    pub fn new(browser_dir: &PathBuf, port: u16, out_dir: Option<PathBuf>, single_tab: bool) -> Self {
        // web/browser -> parent=web; web/tree holds the scanner crate, and
        // parent.parent=AutoCua holds the shared logo.
        let web_dir = browser_dir.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let cfg_path = web_dir.join("tree").join("element.config.json");
        let logo_page = web_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
            .join("logo")
            .join("logo.html");
        ScannerInner {
            port,
            // Scan output is run data, not source: it lives under the CWD's
            // debug/ — the folder the agent wipes at the start of every run —
            // never inside web/tree.
            out_dir: out_dir.unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_default()
                    .join("debug")
                    .join("scans")
            }),
            logo_page,
            glow_css: browser_dir.join("glow").join("glow.css"),
            glow_js: browser_dir.join("glow").join("glow.js"),
            last_cursor: None,
            cursor_enabled: true,
            scrape_tab: None,
            cfg: element::load_config(&cfg_path.display().to_string()),
            cfg_path,
            prepared: HashSet::new(),
            marks: true,
            scan_count: 0,
            tree_text: String::new(),
            dialog_shown: None,
            image_b64: None,
            plain_b64: None,
            all_tabs: String::new(),
            url: String::new(),
            mapping: serde_json::Map::new(),
            dpr: 1.0,
            last_scan_seconds: 0.0,
            keep_requests: false,
            cdp: Cdp::new(port),
            bridge: None,
            glow_armed: HashMap::new(),
            glow_gen: 0,
            single_tab,
            tab_ids: Vec::new(),
            tab_order: Vec::new(),
            tab_id: None,
            relaunch: None,
            saved_tabs: Vec::new(),
            saved_current: 0,
            sent: HashMap::new(),
            reopened_urls: HashSet::new(),
            reopened_tabs: HashSet::new(),
            browser: None,
            #[cfg(windows)]
            browser_pid: None,
        }
    }

    /// The agent just sent the tab it drives to `url` (new_tab, update_tab).
    pub fn sent_to(&mut self, url: &str) {
        if let Some(id) = self.tab_id.clone() {
            self.reopened_tabs.remove(&id);
            self.sent.insert(id, url.to_string());
        }
    }

    /// Relaunch the browser this way if it goes away mid-run (`revive`).
    pub fn set_relaunch(&mut self, headless: bool, profile: Option<PathBuf>) {
        self.relaunch = Some((headless, profile));
    }

    // -- extension mode ------------------------------------------------------
    //
    // With a bridge set, the browser is reached through AutoCuaBridge
    // (browser/AutoCuaBridge, bridge.rs) and there is no debugging port: the
    // scan is the extension's own (element.js), and each tool sends the
    // extension the part of its work that touches the browser (tools.js)
    // instead of running CDP here. Everything else is the same code in both
    // modes: ids, messages, the cursor choreography, the tab listing the
    // model reads. Tab ids are Chrome's tab ids, carried as strings where a
    // CDP target id would be. The debugging-port mode is untouched: every
    // branch below is `if self.bridged()`.

    /// Drive the browser through AutoCuaBridge from now on.
    pub fn set_bridge(&mut self, bridge: Arc<Mutex<Bridge>>) {
        self.bridge = Some(bridge);
    }

    pub fn bridged(&self) -> bool {
        self.bridge.is_some()
    }

    /// A tab id as the extension wants it (a number), from the string kept here.
    fn bridge_tab_id(id: &str) -> Value {
        id.parse::<i64>().map(Value::from).unwrap_or(Value::Null)
    }

    /// One request to the extension, for the tab this agent drives.
    pub fn bridge_call(&mut self, req: Value) -> SResult<Value> {
        let tid = self.current_target_id()?;
        self.bridge_call_on(&tid, req)
    }

    /// The same, for tab [index] of <all_tabs> (run_script reads any tab).
    pub fn bridge_call_at(&mut self, index: i64, req: Value) -> SResult<Value> {
        let tid = self.tab_target(index)?;
        self.bridge_call_on(&tid, req)
    }

    pub fn bridge_call_on(&mut self, tab: &str, mut req: Value) -> SResult<Value> {
        if req.get("tabId").is_none() {
            req["tabId"] = Self::bridge_tab_id(tab);
        }
        self.bridge_request(req)
    }

    /// A cursor call for every tab, or in single-tab mode for this agent's tab
    /// only (cursor_broadcast reaches this agent's own sessions only).
    fn bridge_broadcast(&mut self, mut req: Value) -> SResult<Value> {
        if self.single_tab {
            if let Some(id) = self.tab_id.as_deref() {
                req["tabId"] = Self::bridge_tab_id(id);
                req["only"] = Value::Bool(true);
            }
        }
        self.bridge_request(req)
    }

    /// A request that names no tab (tabs, new_tab, the cursor broadcasts, glow,
    /// release). Answered, or an error the tool reports as it reports any other.
    pub fn bridge_request(&mut self, req: Value) -> SResult<Value> {
        let Some(bridge) = self.bridge.clone() else {
            return Err(ScanErr::s("not in extension mode"));
        };
        let timeout = bridge_timeout(&req);
        let mut b = bridge
            .lock()
            .map_err(|_| ScanErr::s("bridge state poisoned by an earlier panic"))?;
        b.request(req, timeout)
    }

    /// Chrome's open tabs as targets: /json/list over the port, the extension's
    /// tab list over the bridge, with the same fields (id, url, title, type).
    fn targets(&mut self) -> Vec<Value> {
        if self.bridged() {
            return self.bridge_targets().unwrap_or_default();
        }
        page_targets(self.port)
    }

    /// The same, with a failed listing an error rather than an answer.
    fn targets_checked(&mut self) -> SResult<Vec<Value>> {
        if self.bridged() {
            return self.bridge_targets();
        }
        page_targets_checked(self.port)
    }

    fn bridge_targets(&mut self) -> SResult<Vec<Value>> {
        let tabs = self.bridge_request(json!({"type": "tabs"}))?;
        Ok(tabs
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let id = t.get("id")?.as_i64()?;
                let text = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("");
                let url = if text("url").is_empty() { text("pendingUrl") } else { text("url") };
                Some(json!({
                    "id": id.to_string(),
                    "url": url,
                    "title": text("title"),
                    "type": "page",
                    "active": t.get("active").cloned().unwrap_or(Value::Bool(false)),
                    "windowId": t.get("windowId").cloned().unwrap_or(Value::Null),
                    "index": t.get("index").cloned().unwrap_or(Value::Null),
                }))
            })
            .collect())
    }

    /// Whether that tab is still open (tab_exists, either mode).
    fn tab_open(&mut self, id: &str) -> bool {
        self.targets().iter().any(|t| target_id_of(t) == id)
    }

    /// Close one tab and report whether it left (close_target, either mode).
    pub fn close_tab_target(&mut self, id: &str) -> SResult<bool> {
        if self.bridged() {
            let r = self.bridge_call_on(id, json!({"type": "close_tab"}))?;
            return Ok(r.get("closed").and_then(Value::as_bool).unwrap_or(false));
        }
        close_target(self.port, id)
    }

    /// A fresh blank tab in the background, by id (create_tab_impl, either mode).
    fn create_tab_raw(&mut self) -> SResult<String> {
        if self.bridged() {
            let r = self.bridge_request(json!({"type": "new_tab"}))?;
            return r
                .get("id")
                .and_then(Value::as_i64)
                .map(|i| i.to_string())
                .ok_or_else(|| ScanErr::s("the extension did not return the new tab's id"));
        }
        create_tab_impl(self.port)
    }

    /// A fresh tab on `url` in the background, by id (open_tab_impl, either mode).
    pub fn open_tab_raw(&mut self, url: &str) -> SResult<String> {
        if self.bridged() {
            let r = self.bridge_request(json!({"type": "new_tab", "url": url}))?;
            return r
                .get("id")
                .and_then(Value::as_i64)
                .map(|i| i.to_string())
                .ok_or_else(|| ScanErr::s("the extension did not return the new tab's id"));
        }
        open_tab_impl(self.port, url)
    }

    /// Extension mode: tabs the extension closed and opened again because their page
    /// froze or crashed (tools.js toolsReopen). Each new tab takes its old one's place:
    /// its number in `<all_tabs>`, and the driving when it was this agent's tab. True
    /// when it was.
    fn follow_reopened(&mut self) -> bool {
        let Some(bridge) = self.bridge.clone() else {
            return false;
        };
        let mut ids: Vec<i64> = self.tab_order.iter().filter_map(|t| t.parse().ok()).collect();
        ids.extend(self.tab_id.as_deref().and_then(|t| t.parse::<i64>().ok()));
        let moved = match bridge.lock() {
            Ok(mut b) => b.reopened(&ids),
            Err(_) => return false,
        };
        let mut mine = false;
        for (old, new) in moved {
            let (old, new) = (old.to_string(), new.to_string());
            for slot in self.tab_order.iter_mut().chain(self.tab_ids.iter_mut()).filter(|t| **t == old) {
                *slot = new.clone();
            }
            for (id, _) in self.saved_tabs.iter_mut().filter(|(id, _)| *id == old) {
                *id = new.clone();
            }
            if let Some(to) = self.sent.remove(&old) {
                self.sent.insert(new.clone(), to);
            }
            if self.tab_id.as_deref() == Some(old.as_str()) {
                self.tab_id = Some(new);
                mine = true;
            }
        }
        // A new tab listed already (at the end) keeps the old one's place only.
        let mut seen = HashSet::new();
        self.tab_order.retain(|t| seen.insert(t.clone()));
        mine
    }

    /// `follow_reopened` before a scan: when this agent's own tab was opened again, its
    /// page gets up to 10 s to load first, as `revive` waits (a tab still blank while
    /// its page loads reads to the model as a lost page).
    fn follow_reopened_tab(&mut self) -> bool {
        if !self.bridged() || !self.follow_reopened() {
            return false;
        }
        let _ = self.bridge_call(json!({"type": "wait_load", "timeoutMs": 10000}));
        true
    }

    /// The extension's scan of the driven tab, in the shape element.rs hands back,
    /// so everything after the read is the same code in both modes. The scan
    /// tuning goes along as the overlay element.js merges over its own defaults
    /// (the same defaults as element.rs, so the merge gives this side's config).
    fn bridge_scan(&mut self) -> SResult<element::ScanOut> {
        let r = self.bridge_call(json!({
            "type": "scan", "overlay": self.cfg.clone(), "marks": self.marks, "screenshot": true,
        }))?;
        // A popup holds the tab (it opened just before or during the read): scan_core
        // shows it in place of the page.
        if r.get("dialog").is_some_and(|d| !d.is_null()) {
            return Err(ScanErr::s(DIALOG_OPEN));
        }
        let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let count = |k: &str| r.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
        let bytes = |k: &str| {
            r.get(k)
                .and_then(Value::as_str)
                .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        };
        let hits = r
            .get("hits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|h| {
                let i = h.get(0)?.as_u64()? as usize;
                let rect = h.get(1)?.as_array()?;
                if rect.len() != 4 {
                    return None;
                }
                let mut out = [0.0f64; 4];
                for (k, v) in rect.iter().enumerate() {
                    out[k] = v.as_f64()?;
                }
                Some((i, out))
            })
            .collect();
        Ok(element::ScanOut {
            tree: text("tree"),
            count: count("count"),
            sessions: count("sessions"),
            skipped: count("skipped"),
            occluded: count("occluded"),
            noise: count("noise"),
            screenshot: bytes("screenshot"),
            screenshot_plain: bytes("screenshot_plain"),
            url: text("url"),
            settled_ms: r.get("settled_ms").and_then(Value::as_f64).unwrap_or(0.0).round() as u64,
            hits,
            dpr: r.get("dpr").and_then(Value::as_f64).unwrap_or(1.0),
        })
    }

    // -- the scanned tab ---------------------------------------------------

    /// Make sure there IS a tab to read, and that this agent is bound to it.
    ///
    /// There is no process to start any more: scanning is a call on the
    /// session below, so "start" means only "have somewhere to point it".
    pub fn start(&mut self) -> SResult<()> {
        // The browser itself is gone (crashed, killed, quit): bring it back
        // with this agent's tabs before anything else.
        if self.relaunch.is_some() && !port_open(self.port) {
            self.revive()?;
        }
        // Extension mode: the same for the Chrome the bridge started.
        if self.bridged() && self.bridge_chrome_gone() {
            self.revive_bridged()?;
        }
        // Extension mode: this agent's tab froze or crashed, and the extension opened
        // its page again in a new tab (tools.js toolsReopen): drive that one.
        self.follow_reopened_tab();
        // A crashed renderer leaves its target sitting in /json/list, so the
        // `tab_exists` check below hands it straight back and every call
        // against it fails for the rest of the run. Measured before this:
        // update_tab to chrome://crash reported success, then every scan
        // raised "Page.enable: connection lost" at 45s, 55s, indefinitely.
        let crashed = self
            .tab_id
            .clone()
            .is_some_and(|id| self.cdp.take_crashed(&id));
        if crashed {
            self.recover_tab("crashed")?;
        }
        // ONE listing, and a failure to get it is an error rather than an
        // answer. Asking twice (tab_exists, then page_targets) also let the
        // browser change between the two questions.
        let mut targets = self.targets_checked()?;
        let tab_in = |targets: &[Value], id: Option<&str>| {
            id.is_some_and(|id| targets.iter().any(|t| target_id_of(t) == id))
        };
        let mut alive = tab_in(&targets, self.tab_id.as_deref());
        // Another agent on this browser (a parallel run) may have brought it
        // back before this one looked: the port is up, but it is a new browser
        // and this agent's tabs went with the old one. Without this, the agent
        // got a blank tab where its page had been (measured on Windows, where
        // the relaunch is back in about a second). Told apart from a closed tab
        // by the browser's own id, read only on the first step and when the
        // tab is missing: a tab still there is proof enough of the same one.
        if self.relaunch.is_some() && (self.browser.is_none() || !alive) {
            let now = browser_id(self.port);
            if !alive && self.browser.is_some() && now.is_some() && now != self.browser {
                self.revive()?;
                targets = self.targets_checked()?;
                alive = tab_in(&targets, self.tab_id.as_deref());
            } else if now.is_some() {
                self.browser = now;
            }
        }
        if !alive {
            self.tab_id = if self.single_tab {
                // Parallel mode: this agent drives ONE tab of its own. Create
                // it (or re-create it if it died) and never point at another
                // agent's tab.
                Some(self.create_tab_raw()?)
            } else {
                // Shared with the human: take over a BLANK surface if there is
                // one — that is the tab Chrome opens on, and nobody's work —
                // and otherwise open our own.
                //
                // This used to be `page_targets().first()`, which is Chrome's
                // most-recently-USED head: whatever the human was last looking
                // at. Two measured consequences. It adopted a tab running an
                // infinite loop, and `prepare_session`'s 30s Page.enable then
                // ended the whole run. And on a blank tab it went on to paint
                // the logo over it via show_blank_page — defacing a tab this
                // process never opened.
                match targets
                    .iter()
                    .find(|t| blank_url(t.get("url").and_then(Value::as_str).unwrap_or("")))
                    .map(|t| target_id_of(t).to_string())
                    .filter(|id| !id.is_empty())
                {
                    Some(id) => Some(id),
                    None => Some(self.create_tab_raw()?),
                }
            };
            self.prepared.clear();
            // A blank tab taken over was on show already, or the person's
            // to leave; one opened here is fresh (see show_tab).
            let fresh = self.single_tab
                || !targets.iter().any(|t| Some(target_id_of(t)) == self.tab_id.as_deref());
            self.show_tab(fresh);
        }
        // The scan dumps (tree.txt / hits.json / shot.jpg under out_dir) are a
        // DEBUG aid, off by default like the desktop scanners' DEBUG flag —
        // nothing is created on disk unless element::DEBUG is true.
        if element::DEBUG {
            std::fs::create_dir_all(&self.out_dir)
                .map_err(|e| ScanErr::s(format!("could not create {}: {e}", self.out_dir.display())))?;
        }

        // If the surface we landed on is blank, put our own page on it. Only a
        // BLANK surface is replaced; a real page someone left open is never
        // touched.
        let url = self.current_tab_url();
        // An empty url is no answer (the listing failed, or missed the tab),
        // not a blank page: taken as blank, it painted the logo over Google's
        // results as they loaded (measured).
        if !url.is_empty() && blank_url(&url) {
            match self.show_blank_page() {
                Ok(_) => {}
                Err(ScanErr::Scanner(_)) => {} // fails louder later
                Err(e) => return Err(e),
            }
        }

        // Glow from second zero: the browser should read as driven the moment
        // it is driven — blank tab included.
        self.glow_tabs();
        Ok(())
    }

    /// Replace the driven tab after its renderer died (`why`: "crashed") or its page
    /// stopped answering ("froze", Cdp::rpc's watch): the page it showed opens again in
    /// a new tab that takes its place and its number (the owner's rule; its back and
    /// forward history does not come back). Anything but a web page (chrome://crash),
    /// and a page that crashes or freezes again after being opened again, gets a blank
    /// tab instead. tools.js toolsReopen does the same in extension mode.
    ///
    /// Deliberately CREATES rather than reusing the adopt path: recovering
    /// from our own crash by taking over whatever tab the human happens to be
    /// looking at would be a worse outcome than the crash was. The new tab opens
    /// before the old one closes: closing a window's last tab first would close the
    /// window, and on Windows and Linux the last window takes the browser with it.
    fn recover_tab(&mut self, why: &str) -> SResult<()> {
        let url = self.current_tab_url();
        // Again: the same address, or a tab that is itself one opened again (its page
        // may have moved to another address since, by a redirect or its own script).
        let again = self.reopened_urls.contains(&url)
            || self.tab_id.as_ref().is_some_and(|t| self.reopened_tabs.contains(t));
        let opened = if reopenable(&url) && !again {
            self.reopened_urls.insert(url.clone());
            self.open_tab_raw(&url).ok()
        } else {
            None
        };
        let reopened = opened.is_some();
        let id = match opened {
            Some(id) => {
                self.reopened_tabs.insert(id.clone());
                id
            }
            None => self.create_tab_raw()?,
        };
        if let Some(old) = self.tab_id.take() {
            let _ = self.close_tab_target(&old);
            self.cdp.forget(&old);
            self.glow_armed.retain(|_, t| *t != old);
            for slot in self.tab_order.iter_mut().filter(|t| **t == old) {
                *slot = id.clone();
            }
        }
        self.prepared.clear();
        self.tab_id = Some(id.clone());
        self.show_tab(true);
        // Up to 10 s for the page to show, as `revive` waits: read at once, a tab still
        // on its blank start page reads to the model as a lost page.
        if reopened {
            for _ in 0..40 {
                let showing = self.targets().iter().any(|t| {
                    target_id_of(t) == id && !blank_url(t.get("url").and_then(Value::as_str).unwrap_or(""))
                });
                if showing {
                    break;
                }
                sleep_s(0.25);
            }
        }
        self.cdp.notice(reopen_text(why, &url, reopened, again));
        Ok(())
    }

    /// The browser is gone mid-run (crashed, killed or quit): start it again
    /// on the same port and profile, reopen this agent's tabs as the last
    /// listing had them, and drive the same one. With several agents on one
    /// browser, the first to notice starts it; the others find it up (or
    /// holding the profile) and only reopen their own tab. The model is told
    /// on its next action's result.
    fn revive(&mut self) -> SResult<()> {
        let Some((headless, profile)) = self.relaunch.clone() else {
            return Ok(());
        };
        let launched = match launch_chrome_impl(self.port, headless, profile) {
            Ok(launched) => launched,
            // Another agent's relaunch holds the profile: use its browser.
            Err(e) => {
                if !await_browser_ready(self.port, READY_TIMEOUT)? {
                    return Err(e);
                }
                false
            }
        };
        // The page-tab guard exits a minute after its browser has gone, and
        // the agent's next step can come later than that.
        if !headless {
            crate::page_tab_guard::restart(self.port);
        }
        // Found up, with this agent's tab still in it: the browser never went
        // away (a port check that failed for a moment), so nothing reopens.
        if !launched && self.tab_id.as_deref().is_some_and(|id| tab_exists(self.port, id)) {
            return Ok(());
        }
        println!("The browser closed unexpectedly: reopened it, bringing back the agent's tabs");
        // Everything that belonged to the old browser went with it.
        self.cdp.drop_conn();
        #[cfg(windows)]
        {
            self.browser_pid = None;
        }
        self.prepared.clear();
        self.glow_armed.clear();
        self.tab_order.clear();
        self.tab_id = None;
        // The blank tab a browser starts on, for a shared one to close once
        // the agent's tabs are back. Never in single-tab mode: other agents'
        // tabs are opening in the same browser right now.
        let startup: Vec<String> = if self.single_tab {
            Vec::new()
        } else {
            page_targets(self.port)
                .iter()
                .filter(|t| blank_url(t.get("url").and_then(Value::as_str).unwrap_or("")))
                .map(|t| target_id_of(t).to_string())
                .collect()
        };
        // A tab still blank when it was noted is reopened where the agent
        // last sent it: that page had not loaded yet.
        let urls: Vec<String> = if self.saved_tabs.is_empty() {
            vec![BLANK_URL.to_string()]
        } else {
            self.saved_tabs
                .iter()
                .map(|(id, url)| match self.sent.get(id) {
                    Some(to) if blank_url(url) => to.clone(),
                    _ => url.clone(),
                })
                .collect()
        };
        self.sent.clear();
        let mut ids = Vec::new();
        for url in &urls {
            // A page that will not reopen by its url still gets its slot.
            let id = match open_tab_impl(self.port, url) {
                Ok(id) => id,
                Err(_) => create_tab_impl(self.port)?,
            };
            ids.push(id);
        }
        // Up to 10 s for the pages to show: a tab still blank while its page
        // loads reads to the model as a lost tab (measured: it opened the
        // page again itself).
        for _ in 0..40 {
            let live = page_targets(self.port);
            let showing = ids.iter().zip(&urls).all(|(id, url)| {
                blank_url(url)
                    || live.iter().any(|t| {
                        target_id_of(t) == id
                            && !blank_url(t.get("url").and_then(Value::as_str).unwrap_or(""))
                    })
            });
            if showing {
                break;
            }
            sleep_s(0.25);
        }
        self.tab_order = ids.clone();
        self.tab_id = ids.get(self.saved_current).or(ids.first()).cloned();
        self.show_tab(true);
        self.browser = browser_id(self.port);
        for id in startup {
            let _ = close_target(self.port, &id);
        }
        self.cdp.notice(
            "The browser closed unexpectedly and was reopened with the same tabs. \
             Anything typed into a page and not submitted is gone: check the page \
             before carrying on."
                .into(),
        );
        Ok(())
    }

    /// Extension mode: the Chrome the bridge started went away mid-run (crashed,
    /// killed or quit), so `revive_bridged` can start it again.
    fn bridge_chrome_gone(&mut self) -> bool {
        self.bridge
            .as_ref()
            .is_some_and(|b| b.lock().map(|mut b| b.chrome_gone()).unwrap_or(false))
    }

    /// `revive` in extension mode: start Chrome again the way the bridge started it,
    /// wait for the extension to dial back in, and reopen this agent's tabs as the
    /// last listing had them. A tab Chrome brought back by itself (a launch that
    /// restores the last session) is taken as it is rather than opened twice.
    fn revive_bridged(&mut self) -> SResult<()> {
        let Some(bridge) = self.bridge.clone() else {
            return Ok(());
        };
        let back = bridge
            .lock()
            .map_err(|_| ScanErr::s("bridge state poisoned by an earlier panic"))?
            .relaunch()?;
        if !back {
            return Err(ScanErr::s(
                "Chrome closed unexpectedly and was started again, but AutoCuaBridge did not dial back in",
            ));
        }
        println!("The browser closed unexpectedly: reopened it, bringing back the agent's tabs");
        self.prepared.clear();
        self.glow_armed.clear();
        self.tab_order.clear();
        self.tab_id = None;
        // A tab still blank when it was noted is reopened where the agent last sent
        // it: that page had not loaded yet.
        let urls: Vec<String> = if self.saved_tabs.is_empty() {
            vec![BLANK_URL.to_string()]
        } else {
            self.saved_tabs
                .iter()
                .map(|(id, url)| match self.sent.get(id) {
                    Some(to) if blank_url(url) => to.clone(),
                    _ => url.clone(),
                })
                .collect()
        };
        self.sent.clear();
        // What Chrome opened by itself: the tab it starts on, or the session it restored.
        let mut found: Vec<(String, String)> = self
            .targets_checked()?
            .iter()
            .map(|t| {
                let url = t.get("url").and_then(Value::as_str).unwrap_or("");
                (target_id_of(t).to_string(), url.to_string())
            })
            .collect();
        let mut ids = Vec::new();
        for url in &urls {
            let id = match found.iter().position(|(_, u)| u == url) {
                Some(i) => found.remove(i).0,
                None => {
                    let opened = self.bridge_request(json!({"type": "new_tab", "url": url}));
                    match opened.ok().and_then(|r| r.get("id").and_then(Value::as_i64)) {
                        Some(id) => id.to_string(),
                        // A page that will not reopen by its url still gets its slot.
                        None => self.create_tab_raw()?,
                    }
                }
            };
            ids.push(id);
        }
        // Up to 10 s for the pages to show, as `revive` waits.
        for _ in 0..40 {
            let live = self.targets();
            let showing = ids.iter().zip(&urls).all(|(id, url)| {
                blank_url(url)
                    || live.iter().any(|t| {
                        target_id_of(t) == id
                            && !blank_url(t.get("url").and_then(Value::as_str).unwrap_or(""))
                    })
            });
            if showing {
                break;
            }
            sleep_s(0.25);
        }
        self.tab_order = ids.clone();
        self.tab_id = ids.get(self.saved_current).or(ids.first()).cloned();
        self.show_tab(true);
        // The blank tab Chrome started on goes once the agent's tabs are back. Never in
        // single-tab mode: other agents' tabs may be opening in the same browser now.
        if !self.single_tab {
            for (id, url) in found {
                if blank_url(&url) {
                    let _ = self.close_tab_target(&id);
                }
            }
        }
        self.cdp.notice(
            "The browser closed unexpectedly and was reopened with the same tabs. \
             Anything typed into a page and not submitted is gone: check the page \
             before carrying on."
                .into(),
        );
        Ok(())
    }

    /// The popup open on the tab this agent drives, if any (its
    /// opening event's params: type, message, defaultPrompt). Both modes keep it
    /// from the popup's own event, so this reads only what already arrived.
    pub fn open_dialog(&mut self) -> Option<Value> {
        let tab = self.tab_id.clone()?;
        if let Some(bridge) = self.bridge.clone() {
            let id = tab.parse::<i64>().ok()?;
            let mut b = bridge.lock().ok()?;
            return b.dialog(id);
        }
        self.cdp.drain(NOTICE_DRAIN);
        self.cdp.dialog_on(&tab)
    }

    /// Answer the popup open on this agent's tab (controller/dialog): OK, with the
    /// prompt's text when there is some, or Cancel.
    pub fn answer_dialog(&mut self, accept: bool, prompt_text: Option<&str>) -> SResult<()> {
        if self.bridged() {
            // Forgotten before the answer goes: the next popup may open at once,
            // and the extension reports it while the answer is out.
            let tab = self.tab_id.as_deref().and_then(|t| t.parse::<i64>().ok());
            if let (Some(bridge), Some(tab)) = (self.bridge.clone(), tab) {
                if let Ok(mut b) = bridge.lock() {
                    b.forget_dialog(tab);
                }
            }
            let mut req = json!({"type": "dialog", "accept": accept});
            if let Some(text) = prompt_text {
                req["promptText"] = json!(text);
            }
            self.bridge_call(req)?;
            return Ok(());
        }
        self.with_tab(|cdp, sess| cdp.answer_dialog(sess, accept, prompt_text))
    }

    /// The scan while a popup holds this tab (`scan_once`): no tree,
    /// no picture and no ids, the popup in their place (`dialog_view`, which the
    /// agent loop shows as `<dialog>`), and the tabs as usual.
    fn dialog_scan(&mut self, d: &Value) -> String {
        self.dialog_shown = Some(dialog_view(d));
        self.tree_text.clear();
        self.image_b64 = None;
        self.plain_b64 = None;
        self.mapping = serde_json::Map::new();
        self.all_tabs = self.read_tabs();
        self.scan_count += 1;
        let kind = d.get("type").and_then(Value::as_str).unwrap_or("dialog");
        format!("a JavaScript {kind} holds the page: the popup is shown in place of the scan")
    }

    /// Everything the browser did behind the agent's back since anyone last
    /// asked — a dialog answered, a page that crashed — in the model's words.
    pub fn take_notices(&mut self) -> Vec<String> {
        let mut notes = self.cdp.take_notices();
        if let Some(bridge) = &self.bridge {
            if let Ok(mut b) = bridge.lock() {
                // A notice already on the wire (the dialog this action set off)
                // is read now, so it lands on this action's result.
                let _ = b.poll(Duration::from_millis(5));
                notes.extend(b.take_notices());
            }
        }
        notes
    }

    /// Let the tab go. Chrome stays up — the browser outlives the run — and so
    /// does the tab; only this side's session is dropped.
    pub fn stop(&mut self) {
        // A run can end inside scraping mode (Stop, an error, the step limit),
        // and the page it was reading must not keep the look: off before this
        // side lets go, waited for, so the hang-up below cannot overtake it.
        if let Some(tab) = self.scrape_tab.take() {
            self.scrape_glow_send(&tab, false, true);
        }
        if self.bridged() {
            // The extension lets the tabs go: debugger off, glow stopped.
            let _ = self.bridge_request(json!({"type": "release"}));
        }
        self.prepared.clear();
        self.cdp.drop_conn();
    }

    /// Reload tree/element.config.json over the embedded defaults.
    pub fn reload_config(&mut self) {
        self.cfg = element::load_config(&self.cfg_path.display().to_string());
    }

    /// Numbered marks on the screenshot, on or off.
    pub fn set_marks(&mut self, on: bool) {
        self.marks = on;
    }

    // -- scan --------------------------------------------------------------

    /// Borrow the session for a read of the driven tab.
    ///
    /// The scan domains go on the first time a given tab is read: a session
    /// outlives any one scan, so enabling them per scan would be a round trip
    /// paid over and over for nothing.
    fn on_page<T>(
        &mut self,
        f: impl FnOnce(&mut Cdp, &str) -> Result<T, String>,
    ) -> SResult<T> {
        let tid = self.current_target_id()?;
        let sess = self.cdp.attach(&tid)?;
        // A page that stops answering before any call of the scan is found out after
        // FROZEN_AFTER + PROBE_FOR (Cdp::rpc) rather than at the call's own timeout, and
        // scan_core replaces it. One that freezes in the middle of a call is checked once
        // that call runs out of time (`page_froze`).
        self.cdp.watch(Some(&sess));
        let read = self.read_page(&sess, f);
        self.cdp.watch(None);
        read
    }

    /// `on_page` once the scan is watched: the session's one-time setup, the stale
    /// events dropped, then the read.
    fn read_page<T>(
        &mut self,
        sess: &str,
        f: impl FnOnce(&mut Cdp, &str) -> Result<T, String>,
    ) -> SResult<T> {
        if !self.prepared.contains(sess) {
            element::prepare_session(&mut self.cdp, sess).map_err(ScanErr::s)?;
            self.prepared.insert(sess.to_string());
        }
        // Whatever is buffered predates this scan and says nothing about it —
        // and `settle` is about to count events to decide the page is quiet.
        // Except, after the controller has waited on the page, the requests
        // the last action started: that wait read them off the socket, and a
        // request still open when it ended is exactly what the scan's own
        // settle has to wait for (see `keep_action_requests`).
        if std::mem::take(&mut self.keep_requests) {
            self.cdp.clear_events_except(&[
                "Network.requestWillBeSent",
                "Network.loadingFinished",
                "Network.loadingFailed",
            ]);
        } else {
            self.cdp.clear_events();
        }
        f(&mut self.cdp, sess).map_err(ScanErr::s)
    }

    /// An action begins: nothing that happened before it is its doing.
    ///
    /// Reads whatever is waiting on the socket — the page kept talking all
    /// the while the model was thinking — and drops it along with the
    /// buffer, so only what THIS action sets off can reach the scan. Per
    /// action, not per step: a batch that loads a page and then clicks must
    /// not hand the whole load to the scan (measured: that held it at the
    /// 3s ceiling). Everything read here would have been read by the
    /// action's own call anyway, so this costs nothing it did not already.
    pub fn begin_action(&mut self) {
        if self.bridged() {
            // No socket of page events here: the extension's settle counts its
            // own requests (element.js), from the moment it is asked.
            self.keep_requests = false;
            return;
        }
        self.cdp.drain(BACKLOG_DRAIN);
        self.cdp.clear_events();
        self.keep_requests = false;
    }

    /// The controller is about to wait on the page after an action.
    ///
    /// That wait is a Runtime.evaluate, and while it runs the socket is read
    /// for its answer: every request the action set off is taken off the wire
    /// then and buffered, where the scan used to throw it away. The scan's
    /// settle, which counts network requests, then saw a page with nothing
    /// in flight and read it mid-load. Measured on a local page whose button
    /// plays a 200ms transition and then fetches: the tree said LOADING in
    /// 3 of 3 runs. This keeps the action's request events for the scan,
    /// which waits for the ones still open — within a budget, and never for
    /// kinds that do not end (element.rs, SEED_BUDGET_MS / NEVER_WAIT_FOR).
    pub fn keep_action_requests(&mut self) {
        self.keep_requests = true;
    }

    /// Everything scan_elements does except the frontend callback, the glow
    /// pass and the timing stamp — those run in the pyclass wrapper so the
    /// callback fires without this lock held.
    ///
    /// The scan is a CALL now, on the session this side already holds. It used
    /// to be a line written to a subprocess whose answer came back through
    /// three files on disk, which meant the tree, the geometry and the
    /// screenshot could each be from a different moment if anything went wrong
    /// between writing and reading them.
    pub fn scan_core(&mut self) -> SResult<String> {
        // A scan that fails on something that can be put right is put right and taken
        // again, a few times at most: a failed scan ends the run (the agent loop), and a
        // page that freezes or crashes a second time gets a blank tab, which does neither.
        let mut tries = 0;
        loop {
            let scanned = self.scan_once();
            let tid = self.tab_id.clone().unwrap_or_default();
            // Debug mode: the page stopped answering (Cdp::rpc's watch). A read that
            // came back anyway came back short.
            let mut froze = !tid.is_empty() && self.cdp.take_frozen(&tid);
            let e = match scanned {
                Ok(summary) if !froze => return Ok(summary),
                Ok(_) => ScanErr::s(PAGE_FROZE),
                Err(e) => e,
            };
            tries += 1;
            if tries > 3 {
                return Err(e);
            }
            // The whole browser went away mid-scan: bring it back with
            // this agent's tabs and take the scan again.
            if self.relaunch.is_some() && !port_open(self.port) {
                self.revive()?;
                continue;
            }
            if self.bridged() && self.bridge_chrome_gone() {
                self.revive_bridged()?;
                continue;
            }
            // Extension mode: the page froze or crashed under the scan, and the
            // extension opened it again in a new tab: scan that one.
            if self.follow_reopened_tab() {
                continue;
            }
            // A popup opened while the page was being read: show it instead.
            if let Some(d) = self.open_dialog() {
                return Ok(self.dialog_scan(&d));
            }
            // Debug mode: a call the scan waited on ran out its time. A page that
            // does not answer a cheap question either froze during the read.
            if !froze && matches!(&e, ScanErr::Scanner(m) if m.contains("timed out")) {
                froze = self.page_froze();
            }
            if froze {
                self.recover_tab("froze")?;
                continue;
            }
            // The scan may have failed because the renderer died under it.
            // `start`'s pre-check cannot be relied on for that: the crash
            // event is only noticed while something is reading the socket,
            // and a navigation that now fails fast on errorText returns
            // before anything has read. By the time a scan has timed out
            // the event has certainly arrived — so ask again HERE, where
            // the failure is, and take another run at it on a fresh tab.
            if !tid.is_empty() && self.cdp.take_crashed(&tid) {
                self.recover_tab("crashed")?;
                continue;
            }
            return Err(e);
        }
    }

    /// Debug mode, after a scan call ran out its time: whether the page answers a cheap
    /// question by the watch's rule (FROZEN_AFTER + PROBE_FOR). A slow read the page was
    /// still busy with ends within that time and the question is answered; a frozen page
    /// never answers.
    fn page_froze(&mut self) -> bool {
        if self.bridged() {
            return false;
        }
        let Some(tid) = self.tab_id.clone() else {
            return false;
        };
        let Ok(sess) = self.cdp.attach(&tid) else {
            return false;
        };
        self.cdp.watch(Some(&sess));
        let _ = self.cdp.rpc("Runtime.evaluate", json!({"expression": "1"}), Some(&sess), 10.0);
        self.cdp.watch(None);
        self.cdp.take_frozen(&tid)
    }

    fn scan_once(&mut self) -> SResult<String> {
        self.start()?;
        // Scraping mode's look is never in a picture: the agent loop takes it
        // off when the mode ends, and this catches any way out of the mode
        // that missed that (a step that failed after its read began).
        self.scrape_glow_off();
        // A popup holds this tab: the page is frozen, so it is neither
        // read nor photographed, and the popup is shown in its place. Both modes
        // already have it from the popup's own event, so asking waits for nothing.
        if let Some(d) = self.open_dialog() {
            return Ok(self.dialog_scan(&d));
        }
        self.dialog_shown = None;
        // Put the cursor back where it belongs before anything is captured. A
        // tab that just opened, or a page that just navigated, is running a
        // brand-new glow.js that has never heard of the cursor.
        self.cursor_sync();
        // Nothing is hidden for the scan. The old click bloom was, because it
        // was a per-action mark that would have frozen into the screenshot as
        // if it were part of the page; the cursor that replaced it is a
        // standing part of the overlay, like the edge glow, and the model
        // reads the page from the element tree and its marks either way.

        let (marks, cfg) = (self.marks, self.cfg.clone());
        let mut out = if self.bridged() {
            self.bridge_scan()?
        } else {
            self.on_page(|cdp, sess| element::scan_page(cdp, sess, &cfg, marks))?
        };
        if web_store_url(&out.url) {
            web_store_scan(&mut out);
        }

        self.url = out.url.clone();
        self.tree_text = prune_empty_containers(out.tree.trim());
        self.image_b64 = out
            .screenshot
            .as_deref()
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
        self.plain_b64 = out
            .screenshot_plain
            .as_deref()
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes));
        // Sync AGAIN, now that the page is definitely loaded and has definitely
        // run the overlay script. The sync above runs before the scan so the
        // cursor is in the screenshot, but if this step followed a navigation
        // that call landed on a document mid-load — where __AutoCuaCursorPlace
        // did not exist yet — and was silently dropped by its own `&&` guard.
        // Nothing else would have revealed the cursor until the next action,
        // which is how a search could leave the page with no pointer on it.
        self.cursor_sync();

        self.mapping = parse_mapping(&self.tree_text);
        self.load_hits(&out);
        self.all_tabs = self.read_tabs();

        self.scan_count += 1;
        let summary = format!(
            "{} interactive, {} ms settle, {} sessions, {} frames skipped, \
             {} occluded, {} noise",
            out.count, out.settled_ms, out.sessions, out.skipped, out.occluded, out.noise
        );
        self.write_scan_files(&out, &summary);
        Ok(summary)
    }

    /// This scan on disk: one always-overwritten set under `out_dir`.
    ///
    /// Nothing reads it back — the scan's results are already in hand. It is
    /// written to be LOOKED at, which is why a failure here is swallowed: a
    /// full disk must not cost the agent a step. Only with element::DEBUG.
    fn write_scan_files(&self, out: &element::ScanOut, summary: &str) {
        if !element::DEBUG {
            return;
        }
        let header = format!("# {}\n# {summary}\n\n", out.url);
        let _ = std::fs::write(
            self.out_dir.join("tree.txt"),
            format!("{header}{}\n", out.tree),
        );
        // Geometry beside the tree, NOT inside it: tree.txt goes to the model
        // verbatim and coordinates would be noise there.
        let mut hits = serde_json::Map::new();
        for (i, r) in &out.hits {
            hits.insert(i.to_string(), json!([r[0], r[1], r[2], r[3]]));
        }
        let _ = std::fs::write(
            self.out_dir.join("hits.json"),
            serde_json::to_string(&json!({"dpr": out.dpr, "hits": hits})).unwrap_or_default(),
        );
        if let Some(bytes) = out.screenshot.as_deref() {
            let _ = std::fs::write(self.out_dir.join("shot.jpg"), bytes);
        }
        if element::DEBUG {
            let _ = element::write_debug(
                self.scan_count,
                &header,
                &out.tree,
                out.screenshot.as_deref(),
            );
        }
    }

    /// Fold the scan's geometry (DEVICE px) into the element mapping,
    /// converted to CSS px exactly once, here.
    fn load_hits(&mut self, out: &element::ScanOut) {
        self.dpr = if out.dpr == 0.0 { 1.0 } else { out.dpr };
        for (idx, rect) in &out.hits {
            let Some(entry) = self.mapping.get_mut(&idx.to_string()) else { continue };
            let [x, y, w, h] = rect.map(|v| v / self.dpr);
            entry["rect"] = json!([x, y, w, h]);
            entry["point"] = json!([x + w / 2.0, y + h / 2.0]);
        }
    }

    /// One `<all_tabs>` line for a tab: `[n] url (current) - title`.
    fn tab_line(&self, n: usize, target: &Value, current: bool) -> String {
        let url = target.get("url").and_then(Value::as_str).unwrap_or("");
        let title = target.get("title").and_then(Value::as_str).unwrap_or("");
        let mut line = format!("[{n}] {}", if url.is_empty() { BLANK_URL } else { url });
        if current {
            line.push_str(" (current)");
        }
        if !title.is_empty() {
            line.push_str(&format!(" - {title}"));
        }
        line
    }

    /// `<all_tabs>` body: one line per open tab, the driven one marked.
    ///
    /// Read straight from Chrome's /json/list — the SAME list close_tab and
    /// switch_tab index into, so the model-facing [n] and the tab those tools
    /// act on cannot drift apart. It used to come from the scanner
    /// subprocess's own `t` command and get regex-parsed back out of its
    /// stdout — a second listing of the same thing that only agreed by luck.
    /// Chrome's open tabs in a STABLE, left-to-right order.
    ///
    /// /json/list is most-recently-USED order, not tab-strip order. Measured:
    /// with four tabs open it lists them newest-first, and activating the
    /// OLDEST moves that one to the head. Numbering straight off it therefore
    /// renumbers every tab each time one is activated (the person clicking a
    /// tab, or a page opening one), so the tab the model called [2] last step
    /// can be a different tab this step, and it is told nothing.
    ///
    /// Chrome exposes no tab-strip index over CDP, so the order is KEPT here
    /// rather than asked for: an id is appended the first time it is seen and
    /// never moves again, and a closed tab closes the gap behind it. A new tab
    /// opens on the right and takes the next number up, which is what the
    /// model is told to expect.
    fn ordered_tabs(&mut self) -> Vec<Value> {
        // A tab the extension opened again keeps its old one's number.
        self.follow_reopened();
        let live = self.open_tabs();
        // Reversed, because Chrome hands them over newest-first: tabs seen for
        // the first time this pass then land oldest-first, and the very first
        // listing of a browser we did not open reads left-to-right too. The
        // extension lists tabs in strip order already, so there they stay.
        let mut fresh: Vec<String> = live.iter().map(|t| target_id_of(t).to_string()).collect();
        if !self.bridged() {
            fresh.reverse();
        }
        fresh.retain(|id| !id.is_empty());
        self.tab_order.retain(|id| fresh.contains(id));
        for id in fresh {
            if !self.tab_order.contains(&id) {
                self.tab_order.push(id);
            }
        }
        self.tab_order
            .iter()
            .filter_map(|id| live.iter().find(|t| target_id_of(t) == id).cloned())
            .collect()
    }

    /// Keep (target id, url) pairs, left to right, as what `revive` reopens
    /// if the browser goes away. An empty list is the browser gone, not the
    /// tabs: the last good one is kept.
    fn save_tabs(&mut self, tabs: &[(String, String)]) {
        if tabs.is_empty() {
            return;
        }
        let current = self.tab_id.clone().unwrap_or_default();
        self.saved_tabs = tabs.to_vec();
        self.saved_current = tabs.iter().position(|(id, _)| *id == current).unwrap_or(0);
    }

    /// Note where this agent's tabs are now, after every action, so a page an
    /// action just opened is reopened by `revive` even if the browser dies
    /// before the next scan (measured: a tab noted right after new_tab came
    /// back blank). Over the open socket, one Target.getTargets: no
    /// /json/list, and `<all_tabs>` and its [n] are left as the model saw them.
    pub fn note_tabs(&mut self) {
        if self.bridged() {
            // The extension's list is in strip order, oldest first; turned round
            // here so the pass below, written for Chrome's newest-first lists,
            // appends tabs seen for the first time in that order.
            let pages: Vec<(String, String)> = self
                .targets()
                .iter()
                .rev()
                .map(|t| {
                    let field = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                    (field("id"), field("url"))
                })
                .collect();
            self.note_pages(pages);
            return;
        }
        let Ok(r) = self.cdp.rpc("Target.getTargets", json!({}), None, 2.0) else {
            return;
        };
        let pages: Vec<(String, String)> = r
            .get("targetInfos")
            .and_then(Value::as_array)
            .map(|ts| {
                ts.iter()
                    .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                    .map(|t| {
                        let field = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        (field("targetId"), field("url"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.note_pages(pages);
    }

    /// `note_tabs`'s bookkeeping over a (target id, url) list, newest first.
    fn note_pages(&mut self, pages: Vec<(String, String)>) {
        // The listing's order for tabs still open, then any opened since.
        let mut order: Vec<String> = self
            .tab_order
            .iter()
            .filter(|id| pages.iter().any(|(p, _)| p == *id))
            .cloned()
            .collect();
        for (id, _) in pages.iter().rev() {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        if self.single_tab {
            let mine = self.tab_id.clone().unwrap_or_default();
            order.retain(|id| *id == mine);
        }
        let tabs: Vec<(String, String)> = order
            .iter()
            .filter_map(|id| pages.iter().find(|(p, _)| p == id).cloned())
            .collect();
        self.save_tabs(&tabs);
    }

    pub fn read_tabs(&mut self) -> String {
        let current = self.tab_id.clone().unwrap_or_default();
        let targets = self.ordered_tabs();
        // The ids behind the lines, captured in the same pass that renders
        // them. This is the whole point: one listing, two views of it.
        self.tab_ids = targets.iter().map(|t| target_id_of(t).to_string()).collect();
        let tabs: Vec<(String, String)> = targets
            .iter()
            .map(|t| {
                let url = t.get("url").and_then(Value::as_str).unwrap_or("");
                (target_id_of(t).to_string(), url.to_string())
            })
            .collect();
        self.save_tabs(&tabs);
        if self.single_tab {
            // One dedicated tab: the model always sees exactly its own tab as
            // [1]. Other agents' tabs in the shared browser never appear.
            return match targets.first() {
                Some(t) => self.tab_line(1, t, true),
                None => String::new(),
            };
        }
        targets
            .iter()
            .enumerate()
            .map(|(i, t)| self.tab_line(i + 1, t, target_id_of(t) == current))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The tab `[n]` from the listing the model was shown.
    ///
    /// Three different failures, each worth telling apart: a number that is not
    /// a tab number at all, one that was never on the list, and one that was on
    /// it but whose tab has closed since.
    pub fn tab_target(&mut self, index: i64) -> SResult<String> {
        if index < 1 {
            return Err(ScanErr::s(format!(
                "tab numbers start at [1] - there is no tab [{index}]"
            )));
        }
        let id = self.tab_ids.get((index - 1) as usize).cloned().ok_or_else(|| {
            ScanErr::s(format!(
                "no tab [{index}] - the tab list has {} tab(s)",
                self.tab_ids.len()
            ))
        })?;
        if !self.tab_open(&id) {
            return Err(ScanErr::s(format!(
                "tab [{index}] has been closed since that list was made - read \
                 the fresh <all_tabs> in the next input before acting on a tab"
            )));
        }
        Ok(id)
    }

    /// The tab `[n]` of that listing, by id, without asking whether it is still open
    /// (`tab_target` asks).
    pub fn listed_tab(&self, n: i64) -> Option<String> {
        if n < 1 {
            return None;
        }
        self.tab_ids.get((n - 1) as usize).cloned()
    }

    /// How many tabs that listing showed.
    pub fn tab_count(&self) -> usize {
        self.tab_ids.len()
    }

    /// Where to go when the driven tab is closed: the neighbour that now holds
    /// its slot in the listing, else the nearest tab still open on either side
    /// of it, else anything at all.
    pub fn neighbour_tab(&mut self, index: i64, closed: &str) -> Option<String> {
        let i = ((index - 1).max(0) as usize).min(self.tab_ids.len());
        let candidates: Vec<String> = self.tab_ids[(i + 1).min(self.tab_ids.len())..]
            .iter()
            .chain(self.tab_ids[..i].iter().rev())
            .cloned()
            .collect();
        // One listing for the whole decision.
        let live = self.targets();
        let open = |id: &str| live.iter().any(|t| target_id_of(t) == id);
        candidates
            .into_iter()
            .find(|id| id.as_str() != closed && open(id))
            .or_else(|| {
                live.iter()
                    .map(|t| target_id_of(t).to_string())
                    .find(|id| id != closed)
            })
    }

    /// Re-read the listing, so `<all_tabs>` and the ids `[n]` resolves against
    /// both reflect a tab that was just opened or closed.
    pub fn refresh_tabs(&mut self) {
        self.all_tabs = self.read_tabs();
    }

    /// The tabs this agent may see and act on, in Chrome's own order. In
    /// single-tab mode that is exactly one tab: its own.
    pub fn open_tabs(&mut self) -> Vec<Value> {
        let mut targets = self.targets();
        if self.single_tab {
            let mine = self.tab_id.clone().unwrap_or_default();
            targets.retain(|t| target_id_of(t) == mine);
        }
        targets
    }

    /// Url of the tab this agent is driving, read live from Chrome's list.
    ///
    /// Live, not `self.url`: that one is the url as of the last SCAN, and an
    /// action that navigates between scans leaves it a step behind.
    /// It used to be recovered by regex out of the rendered `<all_tabs>`
    /// text — parsing a string this side had just formatted.
    pub fn current_tab_url(&mut self) -> String {
        let Ok(id) = self.current_target_id() else {
            return String::new();
        };
        self.targets()
            .iter()
            .find(|t| target_id_of(t) == id)
            .and_then(|t| t.get("url").and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    }

    /// Where tab `id` is right now, asked of the browser (Target.getTargetInfo over the
    /// open socket, or the extension's tab list): the last scan's url can be a step
    /// behind a page that moved since. Empty when it cannot be read.
    pub fn tab_url_now(&mut self, id: &str) -> String {
        if self.bridged() {
            return self
                .targets()
                .iter()
                .find(|t| target_id_of(t) == id)
                .and_then(|t| t.get("url").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
        }
        self.cdp
            .rpc("Target.getTargetInfo", json!({"targetId": id}), None, 2.0)
            .ok()
            .and_then(|r| r.get("targetInfo").and_then(|i| i.get("url")).and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }

    /// Current page's host — the browser's answer to macOS's app name.
    pub fn application_name(&self) -> String {
        let netloc = self
            .url
            .find("://")
            .map(|i| {
                self.url[i + 3..]
                    .split(['/', '?', '#'])
                    .next()
                    .unwrap_or("")
            })
            .unwrap_or("");
        if netloc.is_empty() {
            "browser".to_string()
        } else {
            netloc.to_string()
        }
    }

    /// [x, y, w, h] of the page, in CSS px — element [1] of the last scan.
    pub fn viewport_rect(&self) -> Option<[f64; 4]> {
        let rect = self.mapping.get("1")?.get("rect")?.as_array()?;
        if rect.len() != 4 {
            return None;
        }
        let mut out = [0.0f64; 4];
        for (i, v) in rect.iter().enumerate() {
            out[i] = v.as_f64()?;
        }
        Some(out)
    }

    // -- cosmetics -----------------------------------------------------------
    //
    // Best-effort dressing of the browser the human watches; none of it may
    // ever cost a scan, so every pass swallows its failures and moves on.

    /// Make Chrome accept the painted logo page's tab icon: touching the icon
    /// link's href after the paint re-fires the announcement at a moment
    /// Chrome accepts it. Plain DOM protocol: no script runs in the page.
    fn nudge_favicon(&mut self, sess: &str) -> Result<(), CdpFail> {
        let root = self.cdp.rpc("DOM.getDocument", json!({"depth": 0}), Some(sess), 5.0)?;
        let root_id = root
            .get("root")
            .and_then(|r| r.get("nodeId"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let node = self
            .cdp
            .rpc(
                "DOM.querySelector",
                json!({"nodeId": root_id, "selector": "link[rel~=icon]"}),
                Some(sess),
                5.0,
            )?
            .get("nodeId")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if node == 0 {
            return Ok(());
        }
        let attrs = self
            .cdp
            .rpc("DOM.getAttributes", json!({"nodeId": node}), Some(sess), 5.0)?;
        let attrs = attrs
            .get("attributes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut href = String::new();
        let mut i = 0;
        while i + 1 < attrs.len() {
            if attrs[i].as_str() == Some("href") {
                href = attrs[i + 1].as_str().unwrap_or("").to_string();
                break;
            }
            i += 2;
        }
        if href.is_empty() {
            return Ok(());
        }
        self.cdp.rpc(
            "DOM.removeAttribute",
            json!({"nodeId": node, "name": "href"}),
            Some(sess),
            5.0,
        )?;
        self.cdp.rpc(
            "DOM.setAttributeValue",
            json!({"nodeId": node, "name": "href", "value": href}),
            Some(sess),
            5.0,
        )?;
        Ok(())
    }

    /// Target id of the tab this agent is driving.
    ///
    /// The tracked id, in both modes. This used to guess in multi-tab runs —
    /// "whichever target /json/list puts first", which is Chrome's
    /// most-recently-USED order and was only ever right because the scanner
    /// had just called bringToFront on its own tab. A user clicking another
    /// tab between steps was enough to break it.
    pub fn current_target_id(&self) -> Result<String, CdpFail> {
        self.tab_id
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| CdpFail::Lost("no tab bound yet".into()))
    }

    /// Send the agent's cursor to the centre of `rect` (CSS px). The glide is
    /// the overlay's own CSS transition, so this returns the moment the move
    /// has been *started*; the caller waits CURSOR_MOVE_SECONDS for it to
    /// land. Returns whether it moved — a cursor that cannot be drawn (a page
    /// that never ran the overlay, a target that died) must never stop the
    /// action it was going to announce, so the caller drops its waits instead.
    pub fn cursor_to(&mut self, rect: &[f64; 4]) -> bool {
        if !self.cursor_enabled {
            return false;
        }
        let [x, y, w, h] = rect;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        // Recorded even if the call below fails: it is where the cursor
        // BELONGS, and the next document to ask should be told that, not the
        // stale position of the last page that happened to answer.
        self.last_cursor = Some((cx, cy));
        if self.bridged() {
            return self
                .bridge_call(json!({"type": "cursor", "op": "to", "x": cx, "y": cy}))
                .ok()
                .and_then(|r| r.get("moved").and_then(Value::as_bool))
                .unwrap_or(false);
        }
        let attempt = (|| -> Result<bool, CdpFail> {
            let tid = self.current_target_id()?;
            let sess = self.cdp.attach(&tid)?;
            let r = self.cdp.rpc(
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "window.__AutoCuaCursor && window.__AutoCuaCursor({cx:.1},{cy:.1})"),
                    "returnByValue": true
                }),
                Some(&sess),
                5.0,
            )?;
            Ok(r.get("result")
                .and_then(|v| v.get("value"))
                .map(truthy)
                .unwrap_or(false))
        })();
        attempt.unwrap_or(false)
    }

    /// Press the cursor down, or let it back up. Fire-and-forget across every
    /// session for the same reason the old bloom teardown was: this runs on
    /// the action path, and a pointer that cannot be redrawn is worth nothing
    /// next to a tool call that blocks 5s per unresponsive tab waiting for it.
    ///
    /// Every session, not just the current one: a click can navigate, and the
    /// release that follows would otherwise land on a document that is already
    /// gone, leaving the arrow pressed for good on whatever comes next.
    /// Replay the cursor's position into whatever documents are on screen.
    ///
    /// Called before every scan, which is what carries the arrow across a
    /// navigation or into a tab that has only just opened: those documents run
    /// glow.js from scratch with no idea where the cursor was, and would
    /// otherwise open it at the centre of the screen. Null coordinates say
    /// "never moved yet", and the page opens it centred on purpose.
    pub fn cursor_sync(&mut self) {
        if !self.cursor_enabled {
            return;
        }
        if self.bridged() {
            let (x, y) = match self.last_cursor {
                Some((x, y)) => (json!(x), json!(y)),
                None => (Value::Null, Value::Null),
            };
            let _ = self.bridge_broadcast(json!({"type": "cursor", "op": "place", "x": x, "y": y}));
            return;
        }
        let call = match self.last_cursor {
            Some((x, y)) => format!("window.__AutoCuaCursorPlace && window.__AutoCuaCursorPlace({x:.1},{y:.1})"),
            None => "window.__AutoCuaCursorPlace && window.__AutoCuaCursorPlace(null,null)".to_string(),
        };
        self.cursor_broadcast(&call);
    }

    /// Hand the cursor's bubble a block of text to say, a word at a time.
    ///
    /// Called once per step with whatever the model actually reasoned. The
    /// text is serialised through serde rather than pasted into the
    /// expression: it is MODEL OUTPUT going into a JavaScript string, and it
    /// routinely contains quotes, newlines and backslashes. Interpolating it
    /// raw would break the call on an apostrophe and would be an injection
    /// hole on anything worse.
    pub fn cursor_say(&mut self, text: &str) {
        if !self.cursor_enabled {
            return;
        }
        if self.bridged() {
            let _ = self.bridge_broadcast(json!({"type": "cursor", "op": "say", "text": text}));
            return;
        }
        let literal = match serde_json::to_string(text) {
            Ok(v) => v,
            Err(_) => return,
        };
        self.cursor_broadcast(&format!(
            "window.__AutoCuaSay && window.__AutoCuaSay({literal})"
        ));
    }

    /// The run is over — take the cursor off the page. `done` is the agent
    /// saying it has let go of the browser, and leaving a pointer behind that
    /// claims otherwise is the one thing this overlay must never do.
    pub fn cursor_hide(&mut self) {
        // Gated like every other cursor call: with the cursor off, the page's
        // hide routine would still record a (0, 0) position that the next
        // same-origin document replays as a visible arrow in the corner.
        if !self.cursor_enabled {
            return;
        }
        if self.bridged() {
            let _ = self.bridge_broadcast(json!({"type": "cursor", "op": "hide"}));
            return;
        }
        self.cursor_broadcast("window.__AutoCuaCursorHide && window.__AutoCuaCursorHide()");
    }

    /// Fire one cursor call at every session, and wait for none of them.
    ///
    /// Every session rather than the current one because a click can navigate
    /// and a run can span tabs: a release or a hide that reached only the
    /// active target would strand the arrow, pressed or visible, on all the
    /// others. Fire-and-forget because this runs on the action path, and a
    /// pointer that cannot be redrawn is worth nothing next to a tool call
    /// that blocks 5s per unresponsive tab waiting for it.
    fn cursor_broadcast(&mut self, expression: &str) {
        let sessions: Vec<String> = self.cdp.sessions.values().cloned().collect();
        for sess in sessions {
            let _ = self.cdp.send(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
                Some(&sess),
            );
        }
    }

    /// Fast mode switches the cursor off (see `cursor_enabled`). Switching
    /// it off mid-run takes an arrow already on screen down first.
    pub fn set_cursor_enabled(&mut self, on: bool) {
        if !on && self.cursor_enabled {
            self.cursor_hide();
        }
        self.cursor_enabled = on;
    }

    pub fn cursor_press(&mut self, down: bool) {
        if !self.cursor_enabled {
            return;
        }
        if self.bridged() {
            let op = if down { "down" } else { "up" };
            let _ = self.bridge_broadcast(json!({"type": "cursor", "op": op}));
            return;
        }
        let call = if down {
            "window.__AutoCuaCursorDown && window.__AutoCuaCursorDown()"
        } else {
            "window.__AutoCuaCursorUp && window.__AutoCuaCursorUp()"
        };
        self.cursor_broadcast(call);
    }

    /// Where the surfaces under `rect`'s centre currently sit, as
    /// [innerX, innerY, pageX, pageY]; None when it cannot be read.
    pub fn scroll_probe(&mut self, rect: &[f64; 4]) -> Option<Value> {
        if self.bridged() {
            let r = self.bridge_call(json!({"type": "scroll_probe", "rect": rect})).ok()?;
            return r.get("probe").filter(|p| p.is_array()).cloned();
        }
        let [x, y, w, h] = rect;
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let attempt = (|| -> Result<Value, CdpFail> {
            let tid = self.current_target_id()?;
            let sess = self.cdp.attach(&tid)?;
            let r = self.cdp.rpc(
                "Runtime.evaluate",
                json!({
                    "expression": format!(
                        "window.__AutoCuaScrollProbe && window.__AutoCuaScrollProbe({cx:.1},{cy:.1})"),
                    "returnByValue": true
                }),
                Some(&sess),
                5.0,
            )?;
            let value = r.get("result").and_then(|v| v.get("value"));
            if let Some(Value::Array(items)) = value {
                if items.len() == 4 {
                    return Ok(Value::Array(items.clone()));
                }
            }
            // No overlay: the page's own offsets still catch a page scroll.
            let m = self.cdp.rpc("Page.getLayoutMetrics", json!({}), Some(&sess), 5.0)?;
            let vp = m
                .get("cssVisualViewport")
                .or_else(|| m.get("visualViewport"))
                .cloned()
                .unwrap_or(json!({}));
            let px = vp.get("pageX").and_then(Value::as_f64).unwrap_or(0.0);
            let py_ = vp.get("pageY").and_then(Value::as_f64).unwrap_or(0.0);
            Ok(json!([-1, -1, py_round(px), py_round(py_)]))
        })();
        attempt.ok()
    }

    // -- scraping mode -------------------------------------------------------

    /// Put scraping mode's look (glow.css "scraping mode") on `tab`, the tab
    /// `scrape` is starting to read. One tab, unlike the cursor's calls: the
    /// look covers the page it is about, and the other tabs (the person's
    /// own, in extension mode) keep the plain glow. The model never sees it:
    /// no step of scraping mode takes a screenshot, the agent loop takes it
    /// off when the mode ends, and a scan takes it off first (scan_once).
    pub fn scrape_glow_on(&mut self, tab: &str) {
        if let Some(old) = self.scrape_tab.replace(tab.to_string()) {
            if old != tab {
                self.scrape_glow_send(&old, false, false);
            }
        }
        self.scrape_glow_send(tab, true, false);
    }

    /// The look again on the tab being read, for a step of scraping mode:
    /// glow.js starts every document without it, and with no scan in the
    /// mode nothing else would put it back on a page that reloaded. Nothing
    /// to do outside the mode.
    pub fn scrape_glow_sync(&mut self) {
        if let Some(tab) = self.scrape_tab.clone() {
            self.scrape_glow_send(&tab, true, false);
        }
    }

    /// Scraping mode is over: the look comes off, at once. Nothing to do
    /// when it is not on, which is every step outside the mode.
    pub fn scrape_glow_off(&mut self) {
        if let Some(tab) = self.scrape_tab.take() {
            self.scrape_glow_send(&tab, false, false);
        }
    }

    /// A read's data for the card on the tab being read: the records a
    /// run_script scrape brought, which glow.js unfolds the card for and
    /// streams in a letter at a time. Only to the tab the look is on, and
    /// only while it is; fire-and-forget like the switch. glow.js keeps the
    /// text to itself and paints it on a canvas in the overlay, so the page,
    /// and the next read of it, gain none of it.
    pub fn scrape_feed(&mut self, text: &str) {
        let Some(tab) = self.scrape_tab.clone() else {
            return;
        };
        let text = Self::scrape_feed_cut(text);
        if self.bridged() {
            let _ = self.bridge_call_on(&tab, json!({"type": "cursor", "op": "scrape_feed", "text": text, "only": true}));
            return;
        }
        let Ok(sess) = self.cdp.attach(&tab) else {
            return;
        };
        let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        let call = json!({
            "expression": format!("window.__AutoCuaScrapeFeed && window.__AutoCuaScrapeFeed({literal})"),
            "returnByValue": true
        });
        let _ = self.cdp.send("Runtime.evaluate", call, Some(&sess));
    }

    /// What the card is given of one chunk: some three and a half minutes
    /// of letters at its pace, far more than it streams before the next read
    /// comes, cut at a line's end.
    fn scrape_feed_cut(text: &str) -> &str {
        const CHARS: usize = 12_000;
        match text.char_indices().nth(CHARS) {
            None => text,
            Some((end, _)) => {
                let head = &text[..end];
                head.rfind('\n').map_or(head, |i| &head[..i])
            }
        }
    }

    /// One switch of the look on one tab. Fire-and-forget like the cursor's
    /// calls, since a page that cannot take it loses decoration, never a
    /// step; `wait` holds until the page has run it, for the last word
    /// before the connection is dropped (stop).
    fn scrape_glow_send(&mut self, tab: &str, on: bool, wait: bool) {
        if self.bridged() {
            let op = if on { "scrape_on" } else { "scrape_off" };
            let _ = self.bridge_call_on(tab, json!({"type": "cursor", "op": op, "only": true}));
            return;
        }
        let Ok(sess) = self.cdp.attach(tab) else {
            return;
        };
        let call = json!({
            "expression": format!("window.__AutoCuaScrapeGlow && window.__AutoCuaScrapeGlow({on})"),
            "returnByValue": true
        });
        if wait {
            let _ = self.cdp.rpc("Runtime.evaluate", call, Some(&sess), 2.0);
        } else {
            let _ = self.cdp.send("Runtime.evaluate", call, Some(&sess));
        }
    }

    /// The tab being read, as the app's picture on a step of scraping mode:
    /// no scan runs then, so nothing else hands the app a frame. A plain
    /// JPEG as base64, like a scan's clean frame (element.rs capture_frame),
    /// of the tab being read, or of the driven tab before any read. It goes
    /// to the frontend callback alone, never to the model. None when it
    /// cannot be taken, and never long: a page that will not draw is given
    /// up on after 2 s rather than holding the step.
    pub fn scrape_frame(&mut self) -> Option<String> {
        let tab = match self.scrape_tab.clone() {
            Some(t) => t,
            None => self.current_target_id().ok()?,
        };
        self.frame_of(&tab)
    }

    /// `tab` as it is drawn now: a plain JPEG as base64, taken the way
    /// scrape_frame takes it, or None when it cannot be had within 2 s. The
    /// screens a run_script scrape reads are photographed with it.
    pub fn frame_of(&mut self, tab: &str) -> Option<String> {
        let tab = tab.to_string();
        if self.bridged() {
            let r = self.bridge_call_on(&tab, json!({"type": "frame"})).ok()?;
            return r.get("frame").and_then(Value::as_str).filter(|b| !b.is_empty()).map(str::to_string);
        }
        let sess = self.cdp.attach(&tab).ok()?;
        let r = self
            .cdp
            .rpc(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 75, "captureBeyondViewport": false}),
                Some(&sess),
                2.0,
            )
            .ok()?;
        r.get("data").and_then(Value::as_str).filter(|b| !b.is_empty()).map(str::to_string)
    }

    /// Arm every open tab so its every document glows, and give a blank tab
    /// still showing the default globe its favicon nudge. Idempotent and
    /// cheap, which is why the few callers can just call it.
    pub fn glow_tabs(&mut self) {
        if self.bridged() {
            // The extension arms the overlay as a content script of its own
            // and dresses the open tabs; in single-tab mode it dresses this
            // agent's tab alone and arms nothing else.
            let req = match (self.single_tab, self.tab_id.as_deref()) {
                (true, Some(id)) => json!({"type": "glow", "tabId": Self::bridge_tab_id(id), "only": true}),
                _ => json!({"type": "glow"}),
            };
            let _ = self.bridge_request(req);
            return;
        }
        let src = glow_source(&self.glow_css, &self.glow_js).to_string();
        if src.is_empty() {
            return;
        }
        // A hung-up connection took its sessions — and everything registered
        // inside them — with it, so those records are worthless.
        if self.cdp.generation != self.glow_gen {
            self.glow_armed.clear();
            self.glow_gen = self.cdp.generation;
        }
        let mut targets = self.targets();
        if self.single_tab {
            // Decorate only this agent's own tab — never inject scripts into
            // (or hold sessions on) tabs that belong to parallel agents.
            let mine = self.tab_id.clone().unwrap_or_default();
            targets.retain(|t| t.get("id").and_then(Value::as_str) == Some(mine.as_str()));
        }
        self.cdp.drain(500);
        let driven = self.tab_id.clone().unwrap_or_default();
        for t in targets {
            let tid = target_id_of(&t).to_string();
            if tid.is_empty() {
                continue;
            }
            // The one call here that must be answered: without the sessionId
            // there is nothing to address. It is a BROWSER-level command, so
            // a wedged renderer does not delay it, and the result is cached.
            let Ok(sess) = self.cdp.attach(&tid) else {
                // Tabs come and go mid-pass by nature; the next call rebuilds
                // whatever still exists.
                self.cdp.forget(&tid);
                continue;
            };
            if !self.glow_armed.contains_key(&sess) {
                // Armed ONCE per session, then left alone. A script registered
                // with addScriptToEvaluateOnNewDocument is re-run by Chrome on
                // every document that tab loads afterwards, so navigation,
                // redirects and SPA route changes keep the glow with no
                // further call from here. Re-walking every tab every scan was
                // never what kept it on screen — this registration is.
                //
                // Page.enable is not optional and must STAY on: without it the
                // registered script never fires. Its events are drained.
                let _ = self.cdp.send("Page.enable", json!({}), Some(&sess));
                let _ = self.cdp.send(
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({"source": src}),
                    Some(&sess),
                );
                // The document already open predates the registration.
                let _ = self.cdp.send(
                    "Runtime.evaluate",
                    json!({"expression": src, "returnByValue": true}),
                    Some(&sess),
                );
                self.glow_armed.insert(sess.clone(), tid.clone());
            }
            // The favicon nudge is the only part that has to read the page
            // back, so it is the only part that can still cost a timeout.
            // Restrict it to the tab this agent painted: the icon it re-fires
            // is the logo page's own, and another tab's favicon was never this
            // code's business.
            if tid == driven {
                let url = t.get("url").and_then(Value::as_str).unwrap_or("");
                let has_favicon = t.get("faviconUrl").map(|v| truthy(v)).unwrap_or(false);
                if blank_url(url) && !has_favicon {
                    let _ = self.nudge_favicon(&sess);
                }
            }
        }
    }

    pub fn drop_cosmetics(&mut self) {
        self.cdp.drop_conn();
    }

    // -- the shared session --------------------------------------------------
    //
    // ONE CDP session per tab, dialled by this side and lent out. Every tool
    // that acts on the page borrows it for the length of a single operation
    // and does its own protocol work; the scanner borrows the same one to
    // read. Nothing else opens a connection to Chrome — there is exactly one
    // socket, and this side owns it.

    /// Borrow this agent's session, attached to the tab it is driving.
    ///
    /// The closure gets the live `Cdp` and the tab's sessionId. Failures come
    /// back as ordinary scanner errors, so a tool can report "the tab went
    /// away" the same way it reports anything else.
    pub fn with_tab<T>(
        &mut self,
        f: impl FnOnce(&mut Cdp, &str) -> Result<T, CdpFail>,
    ) -> SResult<T> {
        if self.bridged() {
            return Err(ScanErr::s("this action has no extension path yet"));
        }
        let tid = self.current_target_id()?;
        let sess = self.cdp.attach(&tid)?;
        Ok(f(&mut self.cdp, &sess)?)
    }

    /// The same loan as `with_tab`, for tab [index] of <all_tabs> — any open
    /// tab, not just the one being driven, which stays where it is. The
    /// number is resolved against the listing the model actually saw, so a
    /// stale one is a message rather than a wrong tab.
    pub fn with_tab_at<T>(
        &mut self,
        index: i64,
        f: impl FnOnce(&mut Cdp, &str) -> Result<T, CdpFail>,
    ) -> SResult<T> {
        if self.bridged() {
            return Err(ScanErr::s("this action has no extension path yet"));
        }
        let tid = self.tab_target(index)?;
        let sess = self.cdp.attach(&tid)?;
        Ok(f(&mut self.cdp, &sess)?)
    }

    /// The same loan as `with_tab_at`, for a tab held by its target id rather
    /// than by a number in <all_tabs> — a tab a tool opened itself, which the
    /// model never saw and which has no number. The driven tab stays where it
    /// is.
    pub fn with_tab_on<T>(
        &mut self,
        target_id: &str,
        f: impl FnOnce(&mut Cdp, &str) -> Result<T, CdpFail>,
    ) -> SResult<T> {
        if self.bridged() {
            return Err(ScanErr::s("this action has no extension path yet"));
        }
        let sess = self.cdp.attach(target_id)?;
        Ok(f(&mut self.cdp, &sess)?)
    }

    /// Point this agent at another tab: the scanner re-binds to it, the
    /// window shows it, and the glow is re-armed on it.
    pub fn bind_tab(&mut self, target_id: &str) -> SResult<()> {
        self.bind(target_id, false)
    }

    /// `fresh`: this agent has just opened the tab (see show_tab).
    fn bind(&mut self, target_id: &str, fresh: bool) -> SResult<()> {
        if !self.tab_open(target_id) {
            return Err(ScanErr::s("that tab is no longer open"));
        }
        self.tab_id = Some(target_id.to_string());
        // Nothing to tell a subprocess any more: the next scan reads whatever
        // tab this points at, over the session already open on it.
        self.show_tab(fresh);
        self.glow_tabs();
        Ok(())
    }

    /// Windows: the browser's process id, asked of the browser once. A
    /// relaunch (revive) forgets it, and so does tab_window when no window
    /// answers to it any more.
    #[cfg(windows)]
    fn browser_pid(&mut self) -> Option<u32> {
        if self.browser_pid.is_none() {
            let info = self
                .cdp
                .rpc("SystemInfo.getProcessInfo", json!({}), None, 2.0)
                .ok()?;
            self.browser_pid = info
                .get("processInfo")
                .and_then(Value::as_array)
                .and_then(|list| {
                    list.iter()
                        .find(|p| p.get("type").and_then(Value::as_str) == Some("browser"))
                        .and_then(|p| p.get("id"))
                        .and_then(Value::as_u64)
                })
                .map(|id| id as u32);
        }
        self.browser_pid
    }

    /// Windows: the window holding the driven tab `target`, as an HWND, or
    /// None. `mates` are the other tabs of that window, `targets` the whole
    /// listing and `bounds` where Chrome puts that window, as show_tab has
    /// them.
    ///
    /// The browser's windows are found by its process id, and one window is
    /// the usual case. Several (a pop-up window, or a second window the
    /// person opened in this browser) are told apart by title first: a
    /// Chromium window is titled after the tab it shows, with the browser's
    /// name last, so the one window titled after a tab of the target's
    /// window and after no tab outside it is the one. A title shared across
    /// windows ("Google" on show in the agent's window and in one the
    /// person opened) names no window; then the one window sitting where
    /// Chrome says the target's window is does. Anything less certain is
    /// None, and show_tab falls back to the key chord, which works while
    /// the window is the active one.
    #[cfg(windows)]
    fn tab_window(
        &mut self,
        target: &str,
        targets: &[Value],
        mates: &[String],
        bounds: Option<[f64; 4]>,
    ) -> Option<usize> {
        let mut windows = win_tabs::windows_of(self.browser_pid()?);
        if windows.is_empty() {
            // No window for that pid: the browser was relaunched since it
            // was read, by another agent or by hand.
            self.browser_pid = None;
            windows = win_tabs::windows_of(self.browser_pid()?);
        }
        if windows.len() == 1 {
            return Some(windows[0].0);
        }
        let title_of = |id: &str| -> String {
            targets
                .iter()
                .find(|t| target_id_of(t) == id)
                .and_then(|t| t.get("title"))
                .and_then(Value::as_str)
                .map(win_tabs::listed_title)
                .unwrap_or_default()
        };
        let (inside, outside): (Vec<&str>, Vec<&str>) = targets
            .iter()
            .map(target_id_of)
            .partition(|id| *id == target || mates.iter().any(|m| m == id));
        let inside: Vec<String> = inside.into_iter().map(title_of).collect();
        let outside: Vec<String> = outside.into_iter().map(title_of).collect();
        let mut found = windows.iter().filter(|(_, caption)| {
            let shown = win_tabs::shown_titles(caption);
            inside.iter().any(|t| shown.contains(t)) && !outside.iter().any(|t| shown.contains(t))
        });
        let by_title = match (found.next(), found.next()) {
            (Some((hwnd, _)), None) => Some(*hwnd),
            _ => None,
        };
        let found = by_title.or_else(|| bounds.and_then(|b| win_tabs::frame_at(&windows, b)));
        // Behind another window the chord does nothing, so the fallback is
        // otherwise invisible.
        if found.is_none() && element::DEBUG {
            println!(
                "show_tab: {} browser windows and none told apart, the key chord is used",
                windows.len()
            );
        }
        found
    }

    /// Make the driven tab the one its window shows, so the person and the
    /// run recorder see what the agent sees.
    ///
    /// Not with Page.bringToFront or Target.activateTarget: both activate the
    /// browser window as well, and on macOS that pulls Brave over whatever
    /// the person or computer use is working in (measured, Brave 154). The
    /// browser's own next-tab shortcut, sent to the tab over CDP, only moves
    /// the tab strip: in a tabbed window the browser handles it before any
    /// page sees it, so a page that swallows key presses cannot stop it, and
    /// it never asks for the window. Each press took 5 to 8 ms (measured),
    /// and the tabs passed on the way show for less than a frame. Nothing
    /// here moves, reorders or closes a tab.
    ///
    /// On Windows the chord reaches the strip only while the browser window
    /// is the active one: behind another window, which is where it sits
    /// while the person watches from the app or a terminal, Ctrl+PageDown,
    /// Ctrl+Tab and Ctrl+2 over CDP all did nothing (measured, Brave 154,
    /// Windows 11), and the window stayed on its first tab. So there each
    /// press is Chrome's own next-tab command, sent to the window as a
    /// WM_COMMAND (see win_tabs): it works in front or behind, 6 to 8 ms a
    /// press, and never activates the window. The chord stays as the
    /// fallback for a window that cannot be told apart, or refuses the
    /// command.
    ///
    /// On Linux the chord has the same limit (measured, Brave 154, GNOME 50
    /// on Wayland), and there the tab is shown in one step through the
    /// browser's tabs API (see show_by_tabs_api), in front or behind. The
    /// chord stays as the fallback for a tab that API cannot tell apart.
    ///
    /// Whether the tab is on show is read from Chrome's listing (see
    /// listed_first). A tab this agent has just opened (`fresh`) heads the
    /// listing without being on show, so it is pressed for at least once. A
    /// tab the person opened in the background is the one case this reads
    /// wrong: as the target, it is taken for shown and the window stays as
    /// it was; at the head while the target is on show, it costs one trip
    /// round the strip, which ends on the target. Only the tab's own window
    /// is cycled, and only when it holds another tab: a pop-up alone in its
    /// window is already on show, and there the shortcut, which the browser
    /// does not take, would reach the page. A tab the shortcut skips (a
    /// collapsed tab group) is given up on once the strip has gone round.
    /// Best-effort throughout: a listing or a press that fails leaves the
    /// window as it is, and a tab that stays behind is still perfectly
    /// readable and clickable.
    pub fn show_tab(&mut self, fresh: bool) {
        let Some(target) = self.tab_id.clone() else {
            return;
        };
        // A headless browser has no window to show anything in. Parallel
        // agents (single_tab) share one window, which can show only one of
        // them: none of them takes it, and each works in a background tab.
        if self.single_tab || self.relaunch.as_ref().is_some_and(|(headless, _)| *headless) {
            return;
        }
        if self.bridged() {
            // The extension activates the tab in its window, which never
            // brings the window forward (chrome.tabs.update).
            let _ = self.bridge_call(json!({"type": "activate", "wait_load": false}));
            return;
        }
        let Ok(targets) = page_targets_checked(self.port) else {
            return;
        };
        // The tabs of the target's window: the strip one press moves along.
        let window_of = |cdp: &mut Cdp, id: &str| {
            cdp.rpc("Browser.getWindowForTarget", json!({"targetId": id}), None, 2.0)
                .ok()
        };
        let Some(reply) = window_of(&mut self.cdp, &target) else {
            return;
        };
        let Some(window) = reply.get("windowId").cloned() else {
            return;
        };
        // Where Chrome puts the target's window, for tab_window.
        #[cfg(windows)]
        let bounds: Option<[f64; 4]> = reply.get("bounds").and_then(|b| {
            Some([
                b.get("left")?.as_f64()?,
                b.get("top")?.as_f64()?,
                b.get("width")?.as_f64()?,
                b.get("height")?.as_f64()?,
            ])
        });
        let mates: Vec<String> = targets
            .iter()
            .map(|t| target_id_of(t).to_string())
            .filter(|id| *id != target)
            .filter(|id| {
                window_of(&mut self.cdp, id).and_then(|w| w.get("windowId").cloned()).as_ref()
                    == Some(&window)
            })
            .collect();
        if mates.is_empty() {
            return;
        }
        // Linux: the tab is shown through the browser's tabs API, in front
        // or behind (see show_by_tabs_api). Only for a tab not on show yet,
        // read as below: one that heads the listing is, unless this agent
        // has just opened it. The chord below stays as the fallback.
        #[cfg(target_os = "linux")]
        {
            if !fresh && targets.first().map(target_id_of) == Some(target.as_str()) {
                return;
            }
            let url_of = |id: &str| {
                targets
                    .iter()
                    .find(|t| target_id_of(t) == id)
                    .and_then(|t| t.get("url"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
            };
            // The tabs of this window on the target's url, newest first, as
            // the listing has them.
            let twins: Vec<&str> = targets
                .iter()
                .map(target_id_of)
                .filter(|id| *id == target || mates.iter().any(|m| m == id))
                .filter(|id| url_of(id) == url_of(&target))
                .collect();
            if let Some(place) = twins.iter().position(|id| *id == target) {
                if show_by_tabs_api(self.port, &window, url_of(&target), twins.len(), place) {
                    return;
                }
            }
        }
        // Windows: the window itself, looked up once the first press is due
        // (see win_tabs). None inside means the chord is used instead.
        #[cfg(windows)]
        let mut hwnd: Option<Option<usize>> = None;
        let mut must_press = fresh;
        // The heads seen so far, the starting one included: back at any of
        // them, the strip has gone round.
        let mut passed: Vec<String> = Vec::new();
        for _ in 0..=mates.len() {
            let Ok(before) = listed_first(self.port) else {
                return;
            };
            if !must_press && before.as_deref() == Some(target.as_str()) {
                return;
            }
            must_press = false;
            if passed.is_empty() {
                passed.extend(before.clone());
            }
            // Windows: Chrome's next-tab command to the window (see
            // win_tabs). The chord otherwise: for a window not found, and
            // from the first command Windows refuses to deliver (a browser
            // running elevated takes no message from this process), since
            // the chord still works while the window is the active one. A
            // command delivered but not handled within a second is left to
            // land: a second press of any kind on top of it could carry the
            // strip past the target.
            #[cfg(windows)]
            let mut by_command = false;
            #[cfg(windows)]
            if let Some(h) =
                *hwnd.get_or_insert_with(|| self.tab_window(&target, &targets, &mates, bounds))
            {
                match win_tabs::command(h, win_tabs::IDC_SELECT_NEXT_TAB) {
                    win_tabs::Sent::Done => by_command = true,
                    win_tabs::Sent::Refused => hwnd = Some(None),
                    win_tabs::Sent::Late => return,
                }
            }
            #[cfg(not(windows))]
            let by_command = false;
            let pressed = by_command
                || self
                    .with_tab(|cdp, sess| {
                        // macOS: Cmd+Shift+]. The Linux and Windows fallback:
                        // Ctrl+PageDown.
                        let (key, code, vk, modifiers) = if cfg!(target_os = "macos") {
                            ("}", "BracketRight", 221, 12)
                        } else {
                            ("PageDown", "PageDown", 34, 2)
                        };
                        for kind in ["rawKeyDown", "keyUp"] {
                            cdp.rpc(
                                "Input.dispatchKeyEvent",
                                json!({"type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk,
                                       "nativeVirtualKeyCode": vk, "modifiers": modifiers}),
                                Some(sess),
                                1.0,
                            )?;
                        }
                        Ok(())
                    })
                    .is_ok();
            if !pressed {
                return;
            }
            // The strip has moved once Chrome lists another tab first
            // (within a millisecond or two on the Mac, about 10 ms on
            // Windows, measured); a fresh target that headed the list
            // already is on show once it still does. Nothing moving means
            // the press did not reach the strip, so pressing on would only
            // cost time; a tab shown once already means the strip has gone
            // round without reaching the target.
            let mut now = None;
            for _ in 0..25 {
                match listed_first(self.port) {
                    Ok(head) if head.as_deref() == Some(target.as_str()) => return,
                    Ok(head) if head != before => {
                        now = head;
                        break;
                    }
                    Ok(_) => sleep_s(0.002),
                    Err(_) => return,
                }
            }
            let Some(now) = now else {
                return;
            };
            if passed.contains(&now) {
                return;
            }
            passed.push(now);
        }
    }

    /// Forget a tab's session — it closed, and everything inside it died.
    pub fn forget_tab(&mut self, target_id: &str) {
        self.cdp.forget(target_id);
    }

    /// Adopt a freshly created tab: bind to it and dress it before anything
    /// navigates, so its first real page glows from its first paint.
    pub fn adopt_tab(&mut self, target_id: &str) -> SResult<()> {
        self.bind(target_id, true)
    }

    /// Open a tab and drive it. The caller navigates it afterwards.
    pub fn create_tab(&mut self) -> SResult<String> {
        let id = self.create_tab_raw()?;
        self.adopt_tab(&id)?;
        // The listing changed under the model's feet — re-take it now rather
        // than leaving `[n]` pointing into a list that predates this tab.
        self.refresh_tabs();
        Ok(id)
    }

    // -- page dressing ---------------------------------------------------

    /// Paint the logo into the driven tab, leaving the address bar alone.
    ///
    /// Page.setDocumentContent, not a navigation and not script execution: a
    /// file:// url would show its whole path in the address bar, while this
    /// swaps only the content and leaves about:blank in the bar.
    pub fn show_blank_page(&mut self) -> SResult<String> {
        let html = blank_html_impl(&self.logo_page);
        if html.is_empty() {
            return Ok(String::new());
        }
        if self.bridged() {
            // The extension paints it the same way, over a blank document only.
            self.bridge_call(json!({"type": "blank_page", "html": html}))?;
            return Ok(String::new());
        }
        self.with_tab(|cdp, sess| {
            let frame = cdp
                .rpc("Page.getFrameTree", json!({}), Some(sess), 5.0)?
                .get("frameTree")
                .and_then(|f| f.get("frame"))
                .cloned()
                .unwrap_or_default();
            let fid = frame.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            if fid.is_empty() {
                return Err(CdpFail::Lost("no frame to render into".into()));
            }
            // Only ever over a blank document, as the page itself reports it:
            // the tab list can call a tab blank that is not, and the logo
            // must never cover a real page.
            let loaded = frame.get("url").and_then(Value::as_str).unwrap_or("");
            if loaded.is_empty() || !blank_url(loaded) {
                return Ok(Value::Null);
            }
            cdp.rpc(
                "Page.setDocumentContent",
                json!({"frameId": fid, "html": html}),
                Some(sess),
                5.0,
            )
        })?;
        // The paint carries the icon link but Chrome dropped its announcement
        // mid-rewrite. `start()` calls glow_tabs immediately after this, and
        // that pass re-fires the icon — so a third walk from here was pure
        // duplication of the pass about to happen anyway.
        Ok(String::new())
    }
}

/// A main-frame frameNavigated for `session` that restored the page from the
/// back/forward cache: the page is back whole, and no load event follows.
fn is_bfcache_restore(e: &Value, session: &str) -> bool {
    e.get("method").and_then(Value::as_str) == Some("Page.frameNavigated")
        && e.get("sessionId").and_then(Value::as_str) == Some(session)
        && e.get("params").is_some_and(|p| {
            p.get("type").and_then(Value::as_str) == Some("BackForwardCacheRestore")
                && p.get("frame").is_some_and(|f| f.get("parentId").is_none())
        })
}

/// How long a request to the extension may take, by what it does: a scan
/// settles for up to 3 s and reads every frame; a navigation waits up to 30 s
/// for the load event; a script runs for 10 s at most, then is stopped.
fn bridge_timeout(req: &Value) -> Duration {
    let secs = match req.get("type").and_then(Value::as_str).unwrap_or("") {
        "scan" => 40.0,
        "run_script" => 20.0,
        "navigate" | "reload" | "history" | "wait_load" | "activate" => 35.0,
        "settle" => 3.0,
        // The app's picture on a scraping step: tools.js gives up on it at 1.5 s.
        "frame" => 3.0,
        "blank_page" => 15.0,
        "click" => 10.0 + req.get("hold_s").and_then(Value::as_f64).unwrap_or(0.0),
        "input" => 15.0,
        _ => 10.0,
    };
    Duration::from_secs_f64(secs)
}

/// Python truthiness for a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python round() — banker's rounding, returns an integer Value.
fn py_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

// ---------------------------------------------------------------------------
// Regexes (compiled once)
// ---------------------------------------------------------------------------

fn re_mapping_line() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*\[(\d+)\]\s*<([a-zA-Z0-9_-]+)(.*)$").unwrap())
}

fn re_name() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r">([^<]*)</").unwrap())
}

fn re_role() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"role="([^"]*)""#).unwrap())
}

/// `[N] <tag ...>name</tag>` lines -> {"N": {...}}. Only ids the model was
/// actually shown land here, so the controller can reject a hallucinated id
/// before it reaches the browser.
fn parse_mapping(tree_text: &str) -> serde_json::Map<String, Value> {
    let mut mapping = serde_json::Map::new();
    for line in tree_text.lines() {
        let Some(m) = re_mapping_line().captures(line) else { continue };
        let idx = m.get(1).map(|g| g.as_str()).unwrap_or("");
        let tag = m.get(2).map(|g| g.as_str()).unwrap_or("");
        let rest = m.get(3).map(|g| g.as_str()).unwrap_or("");
        let name = re_name()
            .captures(rest)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().trim().to_string())
            .unwrap_or_default();
        let role = re_role()
            .captures(rest)
            .and_then(|c| c.get(1))
            .map(|g| g.as_str().to_string())
            .unwrap_or_default();
        let index: i64 = idx.parse().unwrap_or(0);
        mapping.insert(
            idx.to_string(),
            json!({
                "index": index,
                "tag": tag,
                "role": role,
                "name": name,
                "line": line.trim(),
            }),
        );
    }
    mapping
}

// ---------------------------------------------------------------------------
// Python-facing argument coercion helpers (mirror int()/float()/str())
// ---------------------------------------------------------------------------

fn py_str(v: &Bound<'_, PyAny>) -> PyResult<String> {
    v.str()?.extract()
}

fn py_float(v: &Bound<'_, PyAny>) -> PyResult<f64> {
    if let Ok(f) = v.extract::<f64>() {
        return Ok(f);
    }
    let s: String = py_str(v)?;
    s.trim()
        .parse::<f64>()
        .map_err(|_| PyValueError::new_err(format!("could not convert string to float: '{s}'")))
}

fn rect4(v: &Bound<'_, PyAny>) -> PyResult<[f64; 4]> {
    let items: Vec<f64> = v
        .try_iter()?
        .map(|item| py_float(&item?))
        .collect::<PyResult<Vec<f64>>>()?;
    if items.len() != 4 {
        return Err(PyValueError::new_err(format!(
            "expected a [x, y, w, h] rect, got {} value(s)",
            items.len()
        )));
    }
    Ok([items[0], items[1], items[2], items[3]])
}

// ---------------------------------------------------------------------------
// Module-level pyfunctions
// ---------------------------------------------------------------------------

/// Open the CDP session the whole run hangs off. True if we launched it.
/// Already listening -> attach to what is there, so a browser the user
/// already has open (or a previous run's) is reused rather than duplicated.
#[pyfunction]
#[pyo3(signature = (port=CHROME_PORT, headless=false, profile_dir=None))]
pub fn launch_chrome(
    py: Python<'_>,
    port: u16,
    headless: bool,
    profile_dir: Option<String>,
) -> PyResult<bool> {
    crate::ensure_browser(py, port, headless);
    let dir = profile_dir.map(PathBuf::from);
    py.detach(move || launch_chrome_impl(port, headless, dir))
        .map_err(PyErr::from)
}

/// True for the surfaces that carry no page of their own.
#[pyfunction]
#[pyo3(signature = (url=None))]
pub fn is_blank_page(url: Option<Bound<'_, PyAny>>) -> PyResult<bool> {
    let text = match url {
        Some(v) if !v.is_none() => py_str(&v)?,
        _ => String::new(),
    };
    Ok(blank_url(&text))
}

/// The logo page as one line of HTML, ready to render into a blank tab.
#[pyfunction]
pub fn blank_html(py: Python<'_>) -> PyResult<String> {
    let browser = crate::browser_dir(py)?;
    let logo = browser
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("logo").join("logo.html"))
        .unwrap_or_default();
    Ok(blank_html_impl(&logo))
}

/// Guarantee the browser has a tab for the agent to work in.
#[pyfunction]
pub fn ensure_tab(py: Python<'_>, port: u16) -> PyResult<bool> {
    py.detach(|| ensure_tab_impl(port)).map_err(PyErr::from)
}

// ---------------------------------------------------------------------------
// BrowserScanner — the object the agent loop and the Python controller share.
// frozen + Mutex so re-entrant calls (service loop -> Python controller ->
// back into the scanner) can never hit a borrow panic, and so every method
// can release the GIL around its I/O.
// ---------------------------------------------------------------------------

#[pyclass(frozen)]
pub struct BrowserScanner {
    pub inner: Arc<Mutex<ScannerInner>>,
    pub frontend_callback: Option<Py<PyAny>>,
    port: u16,
}

impl BrowserScanner {
    /// Rust-side constructor (the #[new] pymethod wraps this) — service.rs
    /// builds the scanner directly during AgentService construction.
    pub fn create(
        browser_dir: &PathBuf,
        port: u16,
        frontend_callback: Option<Py<PyAny>>,
        out_dir: Option<PathBuf>,
        single_tab: bool,
    ) -> Self {
        BrowserScanner {
            inner: Arc::new(Mutex::new(ScannerInner::new(browser_dir, port, out_dir, single_tab))),
            frontend_callback,
            port,
        }
    }

    fn locked<T>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&mut ScannerInner) -> SResult<T> + Send,
    ) -> PyResult<T>
    where
        T: Send,
    {
        let inner = self.inner.clone();
        py.detach(move || {
            let mut guard = inner
                .lock()
                .map_err(|_| ScanErr::s("scanner state poisoned by an earlier panic"))?;
            f(&mut guard)
        })
        .map_err(PyErr::from)
    }
}

#[pymethods]
impl BrowserScanner {
    #[new]
    #[pyo3(signature = (port=CHROME_PORT, frontend_callback=None, out_dir=None, single_tab=false))]
    fn new(
        py: Python<'_>,
        port: u16,
        frontend_callback: Option<Py<PyAny>>,
        out_dir: Option<String>,
        single_tab: bool,
    ) -> PyResult<Self> {
        let browser = crate::browser_dir(py)?;
        Ok(BrowserScanner::create(
            &browser,
            port,
            frontend_callback,
            out_dir.map(PathBuf::from),
            single_tab,
        ))
    }

    /// A scanner that drives the browser through AutoCuaBridge (extension
    /// mode): the Chrome `bridge` was started for, with no debugging port.
    #[staticmethod]
    #[pyo3(signature = (bridge, frontend_callback=None, out_dir=None, single_tab=false))]
    fn over_bridge(
        py: Python<'_>,
        bridge: PyRef<'_, crate::bridge::PyBridge>,
        frontend_callback: Option<Py<PyAny>>,
        out_dir: Option<String>,
        single_tab: bool,
    ) -> PyResult<Self> {
        let browser = crate::browser_dir(py)?;
        let scanner = BrowserScanner::create(
            &browser,
            0,
            frontend_callback,
            out_dir.map(PathBuf::from),
            single_tab,
        );
        if let Ok(mut inner) = scanner.inner.lock() {
            inner.set_bridge(bridge.inner.clone());
        }
        Ok(scanner)
    }

    fn start(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| s.start())
    }

    /// Quit the scanner. Chrome stays up — the browser outlives the run.
    pub fn stop(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| {
            s.stop();
            Ok(())
        })
    }

    pub fn scan_elements(&self, py: Python<'_>) -> PyResult<String> {
        // Wall clock for the WHOLE scan — the number the agent actually waits
        // each step, wider than the binary's own "N interactive, M ms" line.
        let t0 = Instant::now();
        // Two frames come back: the marked one the model is handed, and the
        // clean one for the app. DEBUG picks which the app gets, the same
        // switch and the same meaning as mac/tree/element.py's: off is the
        // production look (a clean page), on shows exactly what the model
        // sees. It falls back to the marked frame only if a plain one somehow
        // is not there, so the panel never goes blank over this.
        let (out, shown_b64) = self.locked(py, |s| {
            let out = s.scan_core()?;
            let shown = if element::DEBUG {
                s.image_b64.clone()
            } else {
                s.plain_b64.clone().or_else(|| s.image_b64.clone())
            };
            Ok((out, shown))
        })?;

        // The frontend callback runs OUTSIDE the scanner lock, so a callback
        // that turns around and calls the scanner can never deadlock.
        if let (Some(cb), Some(b64)) = (&self.frontend_callback, &shown_b64) {
            let _ = cb.call1(py, (b64.as_str(),));
        }

        self.locked(py, move |s| {
            s.glow_tabs();
            s.last_scan_seconds = t0.elapsed().as_secs_f64();
            Ok(())
        })?;
        Ok(out)
    }

    /// (element_tree_text, annotated_image_base64, all_tabs_text).
    pub fn get_scan_data(&self, py: Python<'_>) -> PyResult<(String, Option<String>, String)> {
        self.locked(py, |s| {
            Ok((s.tree_text.clone(), s.image_b64.clone(), s.all_tabs.clone()))
        })
    }

    /// The popup the last scan showed in place of the page, when a JavaScript
    /// alert, confirm or prompt held it (the agent loop's `<dialog>`); None otherwise.
    pub fn dialog_view(&self, py: Python<'_>) -> PyResult<Option<String>> {
        self.locked(py, |s| Ok(s.dialog_shown.clone()))
    }

    pub fn get_elements_mapping<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mapping = self.locked(py, |s| Ok(Value::Object(s.mapping.clone())))?;
        pythonize::pythonize(py, &mapping).map_err(Into::into)
    }

    /// Paint the logo into the current tab, leaving the address bar alone.
    fn show_blank_page(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| s.show_blank_page())
    }

    /// Where the surfaces under `rect`'s centre currently sit, as
    /// [innerX, innerY, pageX, pageY]; None when it cannot be read.
    fn scroll_probe<'py>(
        &self,
        py: Python<'py>,
        rect: &Bound<'py, PyAny>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let r = match rect4(rect) {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        let value = self.locked(py, move |s| Ok(s.scroll_probe(&r)))?;
        match value {
            Some(v) => Ok(Some(pythonize::pythonize(py, &v)?)),
            None => Ok(None),
        }
    }

    /// Send the cursor to `rect` — returns whether it moved, and never raises.
    fn cursor_to(&self, py: Python<'_>, rect: &Bound<'_, PyAny>) -> PyResult<bool> {
        let r = match rect4(rect) {
            Ok(r) => r,
            Err(_) => return Ok(false),
        };
        self.locked(py, move |s| Ok(s.cursor_to(&r)))
    }

    fn cursor_press(&self, py: Python<'_>, down: bool) -> PyResult<()> {
        self.locked(py, move |s| {
            s.cursor_press(down);
            Ok(())
        })
    }

    /// Switch the cursor off (fast mode) or back on. Never raises.
    fn set_cursor(&self, py: Python<'_>, enabled: bool) -> PyResult<()> {
        self.locked(py, move |s| {
            s.set_cursor_enabled(enabled);
            Ok(())
        })
    }

    /// Say `text` in the cursor's bubble, a word at a time. Never raises: the
    /// bubble is commentary, and commentary must not fail a step.
    pub fn cursor_say(&self, py: Python<'_>, text: &str) -> PyResult<()> {
        let text = text.to_string();
        self.locked(py, move |s| {
            s.cursor_say(&text);
            Ok(())
        })
    }

    /// Take the cursor off the page — the run is over. Never raises.
    fn cursor_hide(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| {
            s.cursor_hide();
            Ok(())
        })
    }

    /// Scraping mode's look back on the page being read, on a step of the
    /// mode. Never raises.
    pub fn scrape_glow_sync(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| {
            s.scrape_glow_sync();
            Ok(())
        })
    }

    /// Scraping mode is over: its look comes off the page. Never raises.
    pub fn scrape_glow_off(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| {
            s.scrape_glow_off();
            Ok(())
        })
    }

    /// The app's picture on a step of scraping mode (ScannerInner::
    /// scrape_frame), handed to the frontend callback as a scan's is. Nothing
    /// at all without a callback (a terminal run). Never raises.
    pub fn scrape_frame(&self, py: Python<'_>) -> PyResult<()> {
        let Some(cb) = &self.frontend_callback else {
            return Ok(());
        };
        let frame = self.locked(py, |s| Ok(s.scrape_frame())).unwrap_or(None);
        // Outside the scanner lock, as scan_elements calls it.
        if let Some(b64) = frame {
            let _ = cb.call1(py, (b64.as_str(),));
        }
        Ok(())
    }

    #[getter]
    pub fn current_url(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| Ok(s.url.clone()))
    }

    /// Current page's host — the browser's answer to macOS's app name.
    #[getter]
    pub fn application_name(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| Ok(s.application_name()))
    }

    /// [x, y, w, h] of the page, in CSS px — element [1] of the last scan.
    /// None before the first scan.
    #[getter]
    fn viewport_rect(&self, py: Python<'_>) -> PyResult<Option<Vec<f64>>> {
        self.locked(py, |s| Ok(s.viewport_rect().map(|r| r.to_vec())))
    }

    /// The open tabs as `<all_tabs>` would show them, read fresh.
    pub fn tabs(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| Ok(s.read_tabs()))
    }

    /// Read tab [n] from `tabs()` instead of the current one.
    ///
    /// The agent uses the switch_tab TOOL, which does this and reports it to
    /// the model. This is the same move without the reporting, for driving the
    /// scanner by hand.
    fn bind_tab(&self, py: Python<'_>, index: i64) -> PyResult<String> {
        self.locked(py, move |s| {
            let target = s.tab_target(index)?;
            s.bind_tab(&target)?;
            Ok(target)
        })
    }

    /// Numbered marks painted on the screenshot, on or off.
    fn set_marks(&self, py: Python<'_>, on: bool) -> PyResult<()> {
        self.locked(py, move |s| {
            s.set_marks(on);
            Ok(())
        })
    }

    /// Re-read tree/element.config.json without restarting.
    fn reload_config(&self, py: Python<'_>) -> PyResult<()> {
        self.locked(py, |s| {
            s.reload_config();
            Ok(())
        })
    }

    #[getter("_all_tabs")]
    fn all_tabs(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| Ok(s.all_tabs.clone()))
    }

    #[getter]
    fn dpr(&self, py: Python<'_>) -> PyResult<f64> {
        self.locked(py, |s| Ok(s.dpr))
    }

    #[getter]
    pub fn last_scan_seconds(&self, py: Python<'_>) -> PyResult<f64> {
        self.locked(py, |s| Ok(s.last_scan_seconds))
    }

    #[getter]
    fn port(&self) -> u16 {
        self.port
    }

    #[getter]
    fn out_dir(&self, py: Python<'_>) -> PyResult<String> {
        self.locked(py, |s| Ok(s.out_dir.display().to_string()))
    }
}
