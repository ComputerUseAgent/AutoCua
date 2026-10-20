// AutoCua - browser element scanner (pure CDP, Rust)
//
// SCANNING ONLY, and not even its own connection.
//
// This module reads a page and says what is on it. It does not own a socket,
// a tab, or a browser: the caller hands it the CDP session the browser side
// holds (AutoCua/web/browser) plus the tab to read, and gets back a tree, the
// geometry behind it, and a screenshot. Launching Chrome, opening and closing
// tabs, clicking, typing, scrolling and navigating all happen on that side,
// over that same one session.
//
// It used to be a separate binary with a CDP socket of its own, driven as a
// subprocess REPL. Two connections to one browser meant two things that could
// disagree about which tab was current; now there is one.
//
// No JavaScript is executed in the page. Everything comes from:
//   DOMSnapshot.captureSnapshot  -> tree, geometry, paint order, computed styles
//   Accessibility.getFullAXTree  -> role, accessible name, state
//
// Nothing injected, no isolated worlds, no Runtime.enable, no page globals touched.
// Cross-process iframes (OOPIFs) reached via Target.setAutoAttach(flatten=true).
//
// Raw CDP over WebSocket against real installed Chrome.
// No playwright / selenium / puppeteer / chromedriver.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::browser::{Cdp, CdpFail};

/// Ceiling on one CDP call. A cold DOMSnapshot of a heavy page is seconds of
/// real work, so this is generous by design — it is a guard against a wedged
/// socket, not a performance budget.
const RPC_TIMEOUT: f64 = 30.0;

/// Cosmetic failures are swallowed by their callers; a scan's are not, so the
/// message has to survive the trip.
fn fail_msg(e: CdpFail) -> String {
    match e {
        CdpFail::Clean(m) | CdpFail::Lost(m) => m,
    }
}

// ============================================================ toggles
// Same switch, same semantics, as mac/tree/element.py's DEBUG. A compile-time
// const rather than a runtime flag: the branches it gates are pure I/O, so a
// `false` here costs nothing at all in the scan path.

/// Set to true to keep a per-scan record on disk, false for direct-LLM only.
///
/// Every scan writes its OWN folder `debug/iteration_<n>/` holding the tree
/// and the annotated screenshot — the SAME bytes handed to the model, so the
/// dump is a byte-identical record of the payload rather than a re-render.
/// Independent of `--out`, which keeps exactly one always-overwritten pair
/// (`scans/tree.txt` + `scans/shot.jpg`) as the "latest scan" view the REPL
/// prints; that pair is what test.py drives and is unaffected by this flag.
pub const DEBUG: bool = false;

/// Per-scan debug folder root, relative to the process CWD — same layout and
/// the same relative anchor as macOS, so the agent's `debug/` wipe at startup
/// clears it.
const DEBUG_DIR: &str = "debug";

/// How long the page must be free of network activity before it counts as
/// settled. Short: this is "nothing is in flight right now", not "the site has
/// finished being a website".
const SETTLE_QUIET_MS: u64 = 150;

/// Hard ceiling on waiting for that quiet. Plenty of pages NEVER go quiet —
/// analytics beacons, websockets, long-polling, autoplaying video — so idle is
/// always "whichever comes first, idle or this".
const SETTLE_CEILING_MS: u64 = 3000;

// ============================================================ config

const DEFAULT_CONFIG: &str = r##"{
  "interactive_roles": [
    "button","link","checkbox","radio","tab","menuitem","menuitemcheckbox",
    "menuitemradio","option","switch","textbox","combobox","searchbox",
    "slider","spinbutton","treeitem","listbox","scrollbar","disclosuretriangle",
    "colorwell","date","datetime","inputtime","radiogroup","togglebutton"
  ],
  "landmark_roles": [
    "list","listitem","table","row","grid","gridcell","navigation","main",
    "dialog","alertdialog","form","banner","contentinfo","tablist","menu",
    "menubar","tree","article","heading","alert","status","rowgroup",
    "columnheader","rowheader","toolbar","region","search","complementary"
  ],
  "interactive_tags": ["a","button","input","select","textarea","summary","video","audio"],
  "landmark_tags": [
    "ul","ol","li","table","tr","td","th","nav","main","form","dialog",
    "header","footer","section","article","aside","fieldset",
    "h1","h2","h3","h4","h5","h6","iframe"
  ],
  "skip_tags": [
    "script","style","noscript","meta","link","head","title","template",
    "br","hr","path","defs","g","circle","rect","polygon","use","symbol",
    "clippath","lineargradient","stop","mask","filter","ellipse","line"
  ],
  "signals": {
    "ax_role": true,
    "is_clickable": true,
    "interactive_tag": true,
    "cursor_pointer": true,
    "focusable": false
  },
  "limits": {
    "max_elements": 300,
    "max_name_chars": 90,
    "max_text_chars": 140,
    "max_href_chars": 60,
    "min_width": 2,
    "min_height": 2,
    "viewport_margin": 0
  },
  "occlusion": {
    "enabled": true,
    "min_occluder_viewport_fraction": 0.12
  },
  "noise": {
    "enabled": true,
    "drop_nested_unnamed": true,
    "drop_nested_same_name": true
  },
  "cross_process_frames": true,

  "hosts": {
    "youtube.com": {
      "signals": { "cursor_pointer": false },
      "limits": { "max_elements": 200 }
    },
    "mail.google.com": {
      "signals": { "cursor_pointer": true }
    }
  }
}"##;

/// computed styles requested with the snapshot, in this exact order
const STYLE_PROPS: [&str; 8] = [
    "cursor",
    "pointer-events",
    "visibility",
    "opacity",
    "border-left-width",
    "border-top-width",
    // longhands: the `overflow` shorthand does not always resolve in a snapshot
    "overflow-x",
    "overflow-y",
];
const S_CURSOR: usize = 0;
const S_POINTER: usize = 1;
const S_VIS: usize = 2;
const S_OPACITY: usize = 3;
const S_BL: usize = 4;
const S_BT: usize = 5;
const S_OX: usize = 6;
const S_OY: usize = 7;

fn merge(base: &mut Value, over: &Value) {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            match (b.get_mut(k), v) {
                (Some(bv), Value::Object(_)) if bv.is_object() => merge(bv, v),
                _ => {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

pub fn load_config(path: &str) -> Value {
    let mut cfg: Value = serde_json::from_str(DEFAULT_CONFIG).expect("bad embedded config");
    if let Ok(txt) = std::fs::read_to_string(path) {
        match serde_json::from_str::<Value>(&txt) {
            Ok(over) => {
                merge(&mut cfg, &over);
                eprintln!("  config overlay: {}", path);
            }
            Err(e) => eprintln!("  ! {} ignored: {}", path, e),
        }
    }
    cfg
}

fn host_of(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    let s = s.split('/').next().unwrap_or("");
    let s = s.rsplit('@').next().unwrap_or(s);
    s.split(':').next().unwrap_or("").to_lowercase()
}

/// shallow-merge any matching per-host block over the base
fn config_for(cfg: &Value, url: &str) -> Value {
    let host = host_of(url);
    let mut out = cfg.clone();
    if let Some(o) = out.as_object_mut() {
        o.remove("hosts");
    }
    if let Some(hosts) = cfg.get("hosts").and_then(|h| h.as_object()) {
        for (pat, over) in hosts {
            let p = pat.to_lowercase();
            if host == p || host.ends_with(&format!(".{}", p)) {
                merge(&mut out, over);
            }
        }
    }
    out
}

fn cfg_set(c: &Value, key: &str) -> HashSet<String> {
    c.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn cfg_bool(c: &Value, group: &str, key: &str, d: bool) -> bool {
    c.get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(d)
}

fn cfg_f64(c: &Value, group: &str, key: &str, d: f64) -> f64 {
    c.get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_f64())
        .unwrap_or(d)
}

// ============================================================ snapshot decoding

fn sid(strings: &[Value], i: i64) -> &str {
    if i >= 0 {
        strings
            .get(i as usize)
            .and_then(|v| v.as_str())
            .unwrap_or("")
    } else {
        ""
    }
}

fn arr_i64(v: Option<&Value>) -> Vec<i64> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().map(|x| x.as_i64().unwrap_or(-1)).collect())
        .unwrap_or_default()
}

fn rare_bool(v: Option<&Value>) -> HashSet<usize> {
    v.and_then(|x| x.get("index"))
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as usize).collect())
        .unwrap_or_default()
}

fn rare_int(v: Option<&Value>) -> HashMap<usize, i64> {
    let idx = arr_i64(v.and_then(|x| x.get("index")));
    let val = arr_i64(v.and_then(|x| x.get("value")));
    idx.iter()
        .zip(val.iter())
        .map(|(i, x)| (*i as usize, *x))
        .collect()
}

fn rare_str(v: Option<&Value>, strings: &[Value]) -> HashMap<usize, String> {
    let idx = arr_i64(v.and_then(|x| x.get("index")));
    let val = arr_i64(v.and_then(|x| x.get("value")));
    idx.iter()
        .zip(val.iter())
        .map(|(i, x)| (*i as usize, sid(strings, *x).to_string()))
        .collect()
}

fn clip(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > n {
        let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
        out.push('\u{2026}');
        out
    } else {
        flat
    }
}

fn px(s: &str, d: f64) -> f64 {
    s.trim_end_matches("px").trim().parse::<f64>().unwrap_or(d)
}

// ============================================================ records

#[derive(Clone)]
struct Rec {
    tag: String,
    role: String,
    parent_node: i64,
    node: usize,
    /// Snapshot document index (local to the frame/session).
    doc: usize,
    /// Target/session index — unique across OOPIFs so kinship/occlusion
    /// do not mix parent-page nodes with iframe nodes that share doc/node ids.
    frame: usize,
    rect: [f64; 4],
    paint: i64,
    kind: char, // 'a' interactive | 'l' landmark | 't' text
    name: String,
    idx: Option<usize>,
    ty: Option<String>,
    href: Option<String>,
    checked: bool,
    expanded: Option<bool>,
    val: Option<String>,
    editable: bool,
}

fn intersect_rect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let w = x1 - x0;
    let h = y1 - y0;
    if w <= 0.0 || h <= 0.0 {
        None
    } else {
        Some([x0, y0, w, h])
    }
}

struct Limits {
    max_elements: usize,
    max_name: usize,
    max_text: usize,
    max_href: usize,
    min_w: f64,
    min_h: f64,
    margin: f64,
}

struct Sets {
    iroles: HashSet<String>,
    lroles: HashSet<String>,
    itags: HashSet<String>,
    ltags: HashSet<String>,
    skip: HashSet<String>,
}

struct Signals {
    ax_role: bool,
    is_clickable: bool,
    interactive_tag: bool,
    cursor_pointer: bool,
    focusable: bool,
}

// ============================================================ scanner

type AxEntry = (String, String, HashMap<String, Value>, bool);

/// A scan in progress: the borrowed session, the tab being read, and the
/// config that decides what counts as interactive.
///
/// Borrowed, not owned — the session outlives any one scan and belongs to the
/// browser side, which is also using it to drive the page.
struct Scanner<'a> {
    cdp: &'a mut Cdp,
    /// CDP sessionId of the tab being scanned. Sub-frames get their own
    /// sessions; `None` anywhere below means "this tab".
    page: String,
    cfg: Value,
}

/// Turn on the domains a scan reads from.
///
/// Separate from scanning because a session is long-lived and this is a round
/// trip: the caller does it once, when it first scans a given tab.
///
/// Runtime.enable is intentionally never called, and focus emulation is set by
/// the browser side on the same session — one owner for one page-wide switch.
pub fn prepare_session(cdp: &mut Cdp, page: &str) -> Result<(), String> {
    let mut go = |method: &str| {
        cdp.rpc(method, json!({}), Some(page), RPC_TIMEOUT)
            .map(|_| ())
            .map_err(fail_msg)
    };
    go("Page.enable")?;
    go("DOM.enable")?;
    // Best-effort: a page that refuses either is still readable, just with
    // less to say. Network events are what `settle` counts — reporting only,
    // no interception and no request modification.
    let _ = go("Accessibility.enable");
    let _ = go("Network.enable");
    Ok(())
}

/// Longest `settle` waits on requests handed over from before the scan
/// (see `settle`): long enough for a search's results to arrive — Google
/// Flights' took 0.6–1.0s — and half the ceiling, so a request that was
/// never going to end costs at most this rather than the full 3s.
const SEED_BUDGET_MS: u64 = 1500;

/// Request types that stay open by design, or that finish somewhere this
/// page's session never hears about. A handed-over request of one of these
/// kinds is never waited for: an audio or video stream, an EventSource, a
/// beacon, a prefetch — and a Document, which is either an embed from
/// another site (its load finishes on the embed's own session, so the page
/// would wait out the whole budget for nothing) or a navigation, whose new
/// page the scan's own wait already sees arrive.
const NEVER_WAIT_FOR: [&str; 5] = ["Document", "Media", "EventSource", "Ping", "Prefetch"];

/// Fold `page`'s buffered request events into `inflight`: a request that
/// started goes in, one that finished or failed comes out. True if there
/// were any, which is what `settle` counts as the page still being busy.
/// `handed_over` leaves out the NEVER_WAIT_FOR kinds.
fn tally_requests(
    cdp: &mut Cdp,
    page: &str,
    inflight: &mut HashSet<String>,
    handed_over: bool,
) -> bool {
    let id_of = |e: &Value| -> Option<String> {
        e.get("params")?
            .get("requestId")?
            .as_str()
            .map(|s| s.to_string())
    };
    let mut saw = false;
    for e in cdp.take_events("Network.requestWillBeSent", Some(page)) {
        saw = true;
        if handed_over {
            let kind = e
                .get("params")
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if NEVER_WAIT_FOR.contains(&kind) {
                continue;
            }
        }
        if let Some(id) = id_of(&e) {
            inflight.insert(id);
        }
    }
    for m in ["Network.loadingFinished", "Network.loadingFailed"] {
        for e in cdp.take_events(m, Some(page)) {
            saw = true;
            if let Some(id) = id_of(&e) {
                inflight.remove(&id);
            }
        }
    }
    saw
}

/// Read `page` and return everything the caller needs to show a model: the
/// tree, the geometry behind it, and the screenshot.
pub fn scan_page(cdp: &mut Cdp, page: &str, cfg: &Value, marks: bool) -> Result<ScanOut, String> {
    Scanner { cdp, page: page.to_string(), cfg: cfg.clone() }.scan(marks, true)
}

impl<'a> Scanner<'a> {
    /// One CDP call against this tab, or against one of its sub-frame
    /// sessions. `None` means the tab itself — which is what lets every walk
    /// below treat the root frame and an OOPIF the same way.
    fn rpc(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        let target = match session {
            Some(s) => s.to_string(),
            None => self.page.clone(),
        };
        self.cdp
            .rpc(method, params, Some(&target), RPC_TIMEOUT)
            .map_err(fail_msg)
    }

    /// Block until the page stops fetching, or the ceiling runs out.
    ///
    /// Replaces the caller's fixed post-action sleep. A guessed constant is
    /// wrong in both directions at once: too long for a page that was ready in
    /// 80ms, too short for one still fetching at 1.2s — and it cannot tell the
    /// difference, so it pays the worst case every single step.
    ///
    /// The signal is in-flight requests, counted from Network events.
    /// document.readyState would be the obvious answer anywhere else, but
    /// reading it means Runtime.evaluate — JavaScript in the page, which is
    /// the one thing this scanner does not do.
    ///
    /// Deliberately NOT waiting on `load`: a modern app fires that with an
    /// empty shell and renders afterwards, so it would report ready on a page
    /// with nothing in it yet.
    ///
    /// Returns how long it waited, so the caller can see the real cost.
    fn settle(&mut self) -> u64 {
        let start = Instant::now();
        let end = start + Duration::from_millis(SETTLE_CEILING_MS);
        let quiet_for = Duration::from_millis(SETTLE_QUIET_MS);
        // Start already "quiet" so a page with nothing happening returns on the
        // first pass instead of serving out the quiet window for no reason.
        let mut last_activity = start - quiet_for;
        // A SET of request ids, not a counter. Counting leaks on redirects:
        // a redirect re-fires requestWillBeSent for the SAME requestId with a
        // `redirectResponse` attached, and the first leg never gets its own
        // loadingFinished — so a counter climbs by two and comes down by one,
        // never reaches zero, and every scan after a redirect serves out the
        // full ceiling. A set is idempotent, so the redirect is a no-op.
        let mut inflight: HashSet<String> = HashSet::new();
        // Only THIS page's requests count. The socket carries every tab the
        // agent has touched, and a sibling tab fetching on a timer would
        // otherwise hold this scan at its ceiling forever.
        let page = self.page.clone();

        // Requests the step's LAST action set off, handed over in the buffer
        // by the browser side (ScannerInner::keep_action_requests): the
        // controller's wait after that action read them off the socket before
        // this ran. Only the ones still OPEN are kept, and they do not count
        // as activity — a request that already finished must not cost the
        // quiet window, so a page with nothing in flight returns exactly as
        // fast as before. They are waited on for SEED_BUDGET_MS at most: a
        // click that opened a stream the page never closes must not cost the
        // whole ceiling. Anything that starts from here on is counted as it
        // always was.
        tally_requests(self.cdp, &page, &mut inflight, true);
        let mut handed_over: HashSet<String> = inflight.clone();
        let budget = Duration::from_millis(SEED_BUDGET_MS);

        loop {
            self.cdp.drain_for(60);
            if tally_requests(self.cdp, &page, &mut inflight, false) {
                last_activity = Instant::now();
            }
            if !handed_over.is_empty() && start.elapsed() >= budget {
                for id in handed_over.drain() {
                    inflight.remove(&id);
                }
            }
            if inflight.is_empty() && last_activity.elapsed() >= quiet_for {
                break;
            }
            if Instant::now() >= end {
                break;
            }
        }
        // Everything else buffered while waiting is stale by definition, and
        // left in place it grows for the life of the process.
        self.cdp.clear_events();
        start.elapsed().as_millis() as u64
    }

    fn url(&mut self) -> String {
        self.rpc("Page.getFrameTree", json!({}), None)
            .ok()
            .and_then(|v| {
                v.get("frameTree")?
                    .get("frame")?
                    .get("url")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }

    // -- sessions (OOPIF) -------------------------------------------------

    /// (session, parent_session, target/frame id) in tree order; root first
    fn attach_all(&mut self) -> Vec<(Option<String>, Option<String>, Option<String>)> {
        let mut found = vec![(None, None, None)];
        if !self
            .cfg
            .get("cross_process_frames")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
        {
            return found;
        }
        let mut pending: Vec<Option<String>> = vec![None];
        let mut seen_sess: HashSet<String> = HashSet::new();
        let mut seen_tid: HashSet<String> = HashSet::new();

        for _ in 0..6 {
            // depth cap on nested OOPIFs
            if pending.is_empty() {
                break;
            }
            for sess in &pending {
                let _ = self.rpc(
                    "Target.setAutoAttach",
                    json!({
                        "autoAttach": true,
                        "waitForDebuggerOnStart": false,
                        "flatten": true
                    }),
                    sess.as_deref(),
                );
            }
            self.cdp.drain_for(500);
            let mut next = Vec::new();
            for e in self.cdp.take_events("Target.attachedToTarget", None) {
                // envelope sessionId is the PARENT session
                let parent = e
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let p = match e.get("params") {
                    Some(p) => p,
                    None => continue,
                };
                let sid_ = match p.get("sessionId").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let info = p.get("targetInfo").cloned().unwrap_or(json!({}));
                let ttype = info.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if seen_sess.contains(&sid_) || (ttype != "iframe" && ttype != "page") {
                    continue;
                }
                seen_sess.insert(sid_.clone());
                let tid = info
                    .get("targetId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                if let Some(t) = &tid {
                    seen_tid.insert(t.clone());
                }
                found.push((Some(sid_.clone()), parent, tid));
                next.push(Some(sid_));
            }
            pending = next;
        }

        // Chrome often does not re-emit attachedToTarget for OOPIFs that already
        // exist; pull them explicitly. targetId == frameId for out-of-process frames.
        if let Ok(tg) = self.rpc("Target.getTargets", json!({}), None) {
            for info in tg
                .get("targetInfos")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default()
            {
                let ttype = info.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if ttype != "iframe" {
                    continue;
                }
                let tid = match info.get("targetId").and_then(|v| v.as_str()) {
                    Some(t) => t.to_string(),
                    None => continue,
                };
                if !seen_tid.insert(tid.clone()) {
                    continue;
                }
                let sid_ = match self.rpc(
                    "Target.attachToTarget",
                    json!({ "targetId": tid, "flatten": true }),
                    None,
                ) {
                    Ok(v) => match v.get("sessionId").and_then(|s| s.as_str()) {
                        Some(s) => s.to_string(),
                        None => continue,
                    },
                    Err(_) => continue,
                };
                if !seen_sess.insert(sid_.clone()) {
                    continue;
                }
                found.push((Some(sid_.clone()), None, Some(tid)));
            }
        }
        found
    }

    fn prepare(&mut self, sess: Option<&str>) {
        let _ = self.rpc("Page.enable", json!({}), sess);
        let _ = self.rpc("DOM.enable", json!({}), sess);
        let _ = self.rpc("Accessibility.enable", json!({}), sess);
    }

    fn snapshot(&mut self, sess: Option<&str>) -> Result<Value, String> {
        self.rpc(
            "DOMSnapshot.captureSnapshot",
            json!({
                "computedStyles": STYLE_PROPS,
                "includePaintOrder": true,
                "includeDOMRects": false
            }),
            sess,
        )
    }

    /// The viewport as JPEG bytes, nothing drawn on it.
    fn capture_frame(&mut self) -> Result<Vec<u8>, String> {
        let b64 = self
            .rpc(
                "Page.captureScreenshot",
                json!({ "format": "jpeg", "quality": 75, "captureBeyondViewport": false }),
                None,
            )?
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| e.to_string())
    }

    /// backendDOMNodeId -> (role, name, state, ignored)
    fn axmap(&mut self, sess: Option<&str>) -> HashMap<i64, AxEntry> {
        let mut out = HashMap::new();
        let nodes = match self.rpc("Accessibility.getFullAXTree", json!({}), sess)
        {
            Ok(v) => v.get("nodes").cloned().unwrap_or(json!([])),
            Err(_) => return out,
        };
        for n in nodes.as_array().unwrap_or(&vec![]) {
            let b = match n.get("backendDOMNodeId").and_then(|v| v.as_i64()) {
                Some(b) => b,
                None => continue,
            };
            let role = n
                .get("role")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = n
                .get("name")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut state = HashMap::new();
            if let Some(props) = n.get("properties").and_then(|v| v.as_array()) {
                for p in props {
                    if let Some(k) = p.get("name").and_then(|v| v.as_str()) {
                        let v = p
                            .get("value")
                            .and_then(|x| x.get("value"))
                            .cloned()
                            .unwrap_or(Value::Null);
                        state.insert(k.to_string(), v);
                    }
                }
            }
            let ignored = n.get("ignored").and_then(|v| v.as_bool()).unwrap_or(false);
            out.insert(b, (role, name, state, ignored));
        }
        out
    }

    // -- document walk ----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn walk_doc(
        snap: &Value,
        doc_i: usize,
        frame: usize,
        origin: (f64, f64),
        frame_clip: [f64; 4],
        ax: &HashMap<i64, AxEntry>,
        sets: &Sets,
        sig: &Signals,
        lim: &Limits,
        out: &mut Vec<Rec>,
        abs_bounds: &mut HashMap<i64, [f64; 4]>,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        let empty: Vec<Value> = Vec::new();
        let strings = snap
            .get("strings")
            .and_then(|v| v.as_array())
            .unwrap_or(&empty);
        let docs = match snap.get("documents").and_then(|v| v.as_array()) {
            Some(d) => d,
            None => return,
        };
        let doc = match docs.get(doc_i) {
            Some(d) => d,
            None => return,
        };
        let nodes = doc.get("nodes").cloned().unwrap_or(json!({}));
        let layout = doc.get("layout").cloned().unwrap_or(json!({}));

        let parent_idx = arr_i64(nodes.get("parentIndex"));
        let node_name = arr_i64(nodes.get("nodeName"));
        let node_type = arr_i64(nodes.get("nodeType"));
        let backend = arr_i64(nodes.get("backendNodeId"));
        let clickable = rare_bool(nodes.get("isClickable"));
        let content_doc = rare_int(nodes.get("contentDocumentIndex"));
        let input_value = rare_str(nodes.get("inputValue"), strings);
        let input_checked = rare_bool(nodes.get("inputChecked"));
        let attrs = nodes
            .get("attributes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let lay_node = arr_i64(layout.get("nodeIndex"));
        let lay_bounds = layout
            .get("bounds")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let lay_styles = layout
            .get("styles")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let lay_paint = arr_i64(layout.get("paintOrders"));
        let lay_text = arr_i64(layout.get("text"));

        // node index -> layout index (first wins)
        let mut n2l: HashMap<usize, usize> = HashMap::new();
        for (li, ni) in lay_node.iter().enumerate() {
            n2l.entry(*ni as usize).or_insert(li);
        }

        let style = |li: usize, k: usize| -> &str {
            lay_styles
                .get(li)
                .and_then(|r| r.as_array())
                .and_then(|r| r.get(k))
                .and_then(|v| v.as_i64())
                .map(|i| sid(strings, i))
                .unwrap_or("")
        };
        let attr = |ni: usize, name: &str| -> Option<String> {
            let a = attrs.get(ni)?.as_array()?;
            let mut k = 0;
            while k + 1 < a.len() {
                if sid(strings, a[k].as_i64().unwrap_or(-1)) == name {
                    return Some(sid(strings, a[k + 1].as_i64().unwrap_or(-1)).to_string());
                }
                k += 2;
            }
            None
        };
        let bounds = |li: usize| -> [f64; 4] {
            lay_bounds
                .get(li)
                .and_then(|b| b.as_array())
                .map(|b| {
                    let g = |i: usize| b.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    [g(0), g(1), g(2), g(3)]
                })
                .unwrap_or([0.0; 4])
        };

        // DOMSnapshot puts painted strings on #text nodes, not the parent element.
        // Walk children in order and fold phrasing wrappers (em/span/…) so
        // <span><em>Gmail</em> is email…</span> becomes one readable string.
        let mut text_at: HashMap<usize, String> = HashMap::new();
        for ni in 0..node_name.len() {
            if node_type.get(ni).copied().unwrap_or(0) != 3 {
                continue;
            }
            let li = match n2l.get(&ni) {
                Some(l) => *l,
                None => continue,
            };
            let t = sid(strings, lay_text.get(li).copied().unwrap_or(-1));
            if t.trim().is_empty() {
                continue;
            }
            text_at.insert(ni, t.to_string());
        }

        let mut kids: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, p) in parent_idx.iter().enumerate() {
            if *p >= 0 {
                kids.entry(*p as usize).or_default().push(i);
            }
        }
        let is_phrasing = |tag: &str| -> bool {
            matches!(
                tag,
                "em" | "strong" | "b" | "i" | "u" | "span" | "code" | "mark"
                    | "small" | "sub" | "sup" | "wbr" | "abbr" | "time" | "data"
            )
        };
        let mut rich_text: HashMap<usize, String> = HashMap::new();
        fn rich_of(
            ni: usize,
            node_type: &[i64],
            node_name: &[i64],
            strings: &[Value],
            kids: &HashMap<usize, Vec<usize>>,
            text_at: &HashMap<usize, String>,
            rich_text: &mut HashMap<usize, String>,
            is_phrasing: &dyn Fn(&str) -> bool,
        ) -> String {
            if let Some(t) = rich_text.get(&ni) {
                return t.clone();
            }
            let mut out = String::new();
            if let Some(ch) = kids.get(&ni) {
                for &c in ch {
                    match node_type.get(c).copied().unwrap_or(0) {
                        3 => {
                            if let Some(t) = text_at.get(&c) {
                                out.push_str(t);
                            }
                        }
                        1 => {
                            let tag =
                                sid(strings, node_name.get(c).copied().unwrap_or(-1)).to_lowercase();
                            if is_phrasing(&tag) {
                                out.push_str(&rich_of(
                                    c,
                                    node_type,
                                    node_name,
                                    strings,
                                    kids,
                                    text_at,
                                    rich_text,
                                    is_phrasing,
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
            rich_text.insert(ni, flat.clone());
            flat
        }

        let mut emitted_els: HashSet<usize> = HashSet::new();
        for ni in 0..node_name.len() {
            if node_type.get(ni).copied().unwrap_or(0) != 1 {
                continue; // elements only
            }
            if emitted_els.contains(&ni) {
                continue;
            }
            let tag = sid(strings, node_name[ni]).to_lowercase();
            if sets.skip.contains(&tag) {
                continue;
            }
            let li = match n2l.get(&ni) {
                Some(l) => *l,
                None => continue,
            };
            let bn = backend.get(ni).copied().unwrap_or(-1);
            let b = bounds(li);

            // recurse into same-process child document
            if let Some(cd) = content_doc.get(&ni) {
                let cdi = *cd as usize;
                if let Some(cdoc) = docs.get(cdi) {
                    let sx = cdoc
                        .get("scrollOffsetX")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let sy = cdoc
                        .get("scrollOffsetY")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let bl = px(style(li, S_BL), 0.0);
                    let bt = px(style(li, S_BT), 0.0);
                    let child_origin = (
                        origin.0 + b[0] + bl - sx,
                        origin.1 + b[1] + bt - sy,
                    );
                    // Same-process iframe: clip to its content box so overflow is
                    // not painted onto the parent page.
                    let child_box = [
                        origin.0 + b[0] + bl,
                        origin.1 + b[1] + bt,
                        (b[2] - 2.0 * bl).max(0.0),
                        (b[3] - 2.0 * bt).max(0.0),
                    ];
                    if let Some(child_clip) = intersect_rect(frame_clip, child_box) {
                        Self::walk_doc(
                            snap,
                            cdi,
                            frame,
                            child_origin,
                            child_clip,
                            ax,
                            sets,
                            sig,
                            lim,
                            out,
                            abs_bounds,
                            depth + 1,
                        );
                    }
                }
            }

            let x = b[0] + origin.0;
            let y = b[1] + origin.1;
            let (w, h) = (b[2], b[3]);
            if bn >= 0 {
                abs_bounds.insert(bn, [x, y, w, h]);
            }
            let Some([x, y, w, h]) = intersect_rect([x, y, w, h], frame_clip) else {
                continue;
            };
            if w < lim.min_w || h < lim.min_h {
                continue;
            }
            if style(li, S_VIS) == "hidden" {
                continue;
            }
            let op = style(li, S_OPACITY);
            if !op.is_empty() && px(op, 1.0) < 0.05 {
                continue;
            }

            let (role, ax_name, state, ignored) = ax
                .get(&bn)
                .cloned()
                .unwrap_or_else(|| (String::new(), String::new(), HashMap::new(), false));
            let pointer = style(li, S_POINTER);
            let cursor = style(li, S_CURSOR);

            let truthy = |v: Option<&Value>| -> bool {
                matches!(v, Some(Value::Bool(true))) || matches!(v, Some(Value::String(s)) if s == "true")
            };

            let mut act = if sig.ax_role && sets.iroles.contains(&role) && !ignored {
                true
            } else if sig.interactive_tag && sets.itags.contains(&tag) {
                true
            } else if sig.is_clickable && clickable.contains(&ni) {
                true
            } else if sig.cursor_pointer && cursor == "pointer" {
                true
            } else {
                sig.focusable && truthy(state.get("focusable"))
            };
            if act && (truthy(state.get("disabled")) || pointer == "none") {
                act = false;
            }

            let mut text = sid(strings, lay_text.get(li).copied().unwrap_or(-1)).to_string();
            if text.trim().is_empty() {
                text = rich_of(
                    ni,
                    &node_type,
                    &node_name,
                    strings,
                    &kids,
                    &text_at,
                    &mut rich_text,
                    &is_phrasing,
                );
            }
            let text = clip(&text, lim.max_text);
            let land = sets.lroles.contains(&role) || sets.ltags.contains(&tag);
            if !act && text.is_empty() && !land {
                continue;
            }
            // body/html own-text is almost always inter-tag whitespace labels; emitting
            // them as records warps the tree (everything nests under <body>). Real copy
            // is emitted below as #text nodes instead.
            if !act && !land && (tag == "body" || tag == "html") {
                continue;
            }

            let display_name = if ax_name.is_empty() { &text } else { &ax_name };
            let kind = if act {
                'a'
            } else if land {
                'l'
            } else {
                't'
            };
            let mut rec = Rec {
                tag: tag.clone(),
                role: role.clone(),
                parent_node: parent_idx.get(ni).copied().unwrap_or(-1),
                node: ni,
                doc: doc_i,
                frame,
                rect: [x, y, w, h],
                paint: lay_paint.get(li).copied().unwrap_or(0),
                kind,
                name: clip(display_name, lim.max_name),
                idx: None,
                ty: None,
                href: None,
                checked: false,
                expanded: None,
                val: None,
                editable: false,
            };
            if act {
                rec.ty = attr(ni, "type").map(|s| s.to_lowercase());
                rec.href = attr(ni, "href").map(|s| clip(&s, lim.max_href));
                rec.checked = input_checked.contains(&ni) || truthy(state.get("checked"));
                if state.contains_key("expanded") {
                    rec.expanded = Some(truthy(state.get("expanded")));
                }
                rec.val = input_value.get(&ni).map(|v| clip(v, 40));
                rec.editable = truthy(state.get("editable"));
            }
            emitted_els.insert(ni);
            // Text-block containers absorb the inline descendants whose text
            // rich_of already folded into this name (<em>Gmail</em>, leftover
            // #text). It has to mirror rich_of's traversal exactly: rich_of
            // only descends through phrasing tags, so <a>/<button> text is
            // deliberately left out of the name for the child to emit itself.
            // Absorbing those subtrees anyway swallowed every inline link and
            // its text with it — a Wikipedia paragraph came out as
            // "encyklopedii, którą ." with each link punched out of it.
            if kind == 't' && !rec.name.is_empty() {
                let mut stack: Vec<usize> = kids.get(&ni).cloned().unwrap_or_default();
                while let Some(c) = stack.pop() {
                    match node_type.get(c).copied().unwrap_or(0) {
                        3 => {
                            emitted_els.insert(c);
                        }
                        1 => {
                            let ctag = sid(strings, node_name.get(c).copied().unwrap_or(-1))
                                .to_lowercase();
                            // rich_of skipped it, so it still owns its own text
                            if !is_phrasing(&ctag) || sets.itags.contains(&ctag) {
                                continue;
                            }
                            // a clickable <span> is phrasing but still a control
                            let cbn = backend.get(c).copied().unwrap_or(-1);
                            let crole = ax.get(&cbn).map(|e| e.0.as_str()).unwrap_or("");
                            if clickable.contains(&c) || sets.iroles.contains(crole) {
                                continue;
                            }
                            if !emitted_els.insert(c) {
                                continue;
                            }
                            if let Some(ch) = kids.get(&c) {
                                stack.extend(ch.iter().copied());
                            }
                        }
                        _ => {}
                    }
                }
            }
            out.push(rec);
        }

        // Visible #text nodes (e.g. "Initial page" / "Frame 1:") — elements-only
        // walks miss these because layout.text lives on the text node.
        for ni in 0..node_name.len() {
            if node_type.get(ni).copied().unwrap_or(0) != 3 {
                continue;
            }
            if emitted_els.contains(&ni) {
                continue;
            }
            let p = parent_idx.get(ni).copied().unwrap_or(-1);
            // skip labels already surfaced via the parent element's name
            if p >= 0 && emitted_els.contains(&(p as usize)) {
                continue;
            }
            let li = match n2l.get(&ni) {
                Some(l) => *l,
                None => continue,
            };
            // Same visibility gate the element loop applies. Without it every
            // collapsed menu still has layout boxes (visibility:hidden reserves
            // space, unlike display:none) and its labels were marked as if they
            // were on screen — Wikipedia's folded main menu and Tools panel.
            if style(li, S_VIS) == "hidden" {
                continue;
            }
            let op = style(li, S_OPACITY);
            if !op.is_empty() && px(op, 1.0) < 0.05 {
                continue;
            }
            let raw = sid(strings, lay_text.get(li).copied().unwrap_or(-1));
            let name = clip(raw, lim.max_name);
            if name.is_empty() {
                continue;
            }
            let b = bounds(li);
            // Clip to every ancestor that hides its overflow. A text node keeps
            // its full natural layout box even when the parent crops it, so the
            // "visually hidden" skip-link pattern (1x1 box, overflow:hidden,
            // clip rect) otherwise reads as a full-width on-screen label. This
            // also drops text scrolled out of an overflow:auto panel.
            let mut r = [b[0] + origin.0, b[1] + origin.1, b[2], b[3]];
            let mut anc = p;
            let mut cropped = false;
            for _ in 0..40 {
                if anc < 0 {
                    break;
                }
                let ai = anc as usize;
                if let Some(&ali) = n2l.get(&ai) {
                    let ox = style(ali, S_OX);
                    let oy = style(ali, S_OY);
                    if (!ox.is_empty() && ox != "visible") || (!oy.is_empty() && oy != "visible") {
                        let ab = bounds(ali);
                        match intersect_rect(
                            r,
                            [ab[0] + origin.0, ab[1] + origin.1, ab[2], ab[3]],
                        ) {
                            Some(c) => r = c,
                            None => {
                                cropped = true;
                                break;
                            }
                        }
                    }
                }
                anc = parent_idx.get(ai).copied().unwrap_or(-1);
            }
            if cropped {
                continue;
            }
            let Some([x, y, w, h]) = intersect_rect(r, frame_clip) else {
                continue;
            };
            if w < lim.min_w || h < lim.min_h {
                continue;
            }
            out.push(Rec {
                tag: "#text".into(),
                role: String::new(),
                parent_node: p,
                node: ni,
                doc: doc_i,
                frame,
                rect: [x, y, w, h],
                paint: lay_paint.get(li).copied().unwrap_or(0),
                kind: 't',
                name,
                idx: None,
                ty: None,
                href: None,
                checked: false,
                expanded: None,
                val: None,
                editable: false,
            });
        }
    }

    // -- occlusion --------------------------------------------------------

    /// No elementFromPoint without JS. Approximate with paint order + containment.
    /// Drop marks that carry no information the user cannot already get from an
    /// interactive ancestor.
    ///
    /// `cursor: pointer` inherits and DOMSnapshot's isClickable is set on
    /// descendants too, so every <svg>, wrapper <div role="generic"> and
    /// <span role="none"> inside a button scores as interactive and gets its
    /// own numbered box stacked on the same pixels. On a Google results page
    /// that was most of the marks: one "AI Mode" link produced four.
    ///
    /// Survivors are reparented onto the nearest surviving ancestor so removing
    /// a wrapper does not flatten its children to the root of the tree.
    fn drop_noise(recs: &mut Vec<Rec>, cfg: &Value) -> usize {
        if !cfg_bool(cfg, "noise", "enabled", true) {
            return 0;
        }
        let drop_unnamed = cfg_bool(cfg, "noise", "drop_nested_unnamed", true);
        let drop_same = cfg_bool(cfg, "noise", "drop_nested_same_name", true);
        let drop_generic = cfg_bool(cfg, "noise", "drop_unnamed_generic", true);
        if !drop_unnamed && !drop_same && !drop_generic {
            return 0;
        }

        let by_node: HashMap<(usize, usize, i64), usize> = recs
            .iter()
            .enumerate()
            .map(|(k, r)| ((r.frame, r.doc, r.node as i64), k))
            .collect();
        let up = |k: usize| -> Option<usize> {
            by_node
                .get(&(recs[k].frame, recs[k].doc, recs[k].parent_node))
                .copied()
                .filter(|&p| p != k)
        };

        let mut drop = vec![false; recs.len()];
        for k in 0..recs.len() {
            if recs[k].kind != 'a' {
                continue;
            }
            // a real form control is never just decoration around its parent
            if matches!(recs[k].tag.as_str(), "input" | "textarea" | "select") {
                continue;
            }
            // An unlabelled wrapper with no role of its own and nowhere to go
            // is not something anyone can act on -- an icon button keeps its
            // real role (button/link/...) and survives this.
            if drop_generic
                && recs[k].name.is_empty()
                && recs[k].href.is_none()
                && recs[k].val.is_none()
                && matches!(recs[k].role.as_str(), "" | "generic" | "none" | "presentation")
            {
                drop[k] = true;
                continue;
            }
            // nearest interactive ancestor
            let mut cur = k;
            let mut anc = None;
            for _ in 0..64 {
                match up(cur) {
                    Some(p) => {
                        cur = p;
                        if recs[p].kind == 'a' {
                            anc = Some(p);
                            break;
                        }
                    }
                    None => break,
                }
            }
            let Some(a) = anc else { continue };
            // a different destination is its own target, keep it
            if recs[k].href.is_some() && recs[k].href != recs[a].href {
                continue;
            }
            let unnamed = recs[k].name.is_empty();
            let same = !unnamed && recs[k].name == recs[a].name;
            if (unnamed && drop_unnamed) || (same && drop_same) {
                drop[k] = true;
            }
        }

        let mut reparent: Vec<i64> = recs.iter().map(|r| r.parent_node).collect();
        for k in 0..recs.len() {
            if drop[k] {
                continue;
            }
            let mut cur = k;
            let mut par = -1i64;
            for _ in 0..200 {
                match up(cur) {
                    Some(p) => {
                        if !drop[p] {
                            par = recs[p].node as i64;
                            break;
                        }
                        cur = p;
                    }
                    None => break,
                }
            }
            reparent[k] = par;
        }
        for (k, r) in recs.iter_mut().enumerate() {
            r.parent_node = reparent[k];
        }

        let dropped = drop.iter().filter(|d| **d).count();
        let mut k = 0;
        recs.retain(|_| {
            let keep = !drop[k];
            k += 1;
            keep
        });
        dropped
    }

    fn occluded(recs: &[Rec], vw: f64, vh: f64, cfg: &Value) -> HashSet<usize> {
        let mut hidden = HashSet::new();
        if !cfg_bool(cfg, "occlusion", "enabled", true) {
            return hidden;
        }
        let frac = cfg_f64(cfg, "occlusion", "min_occluder_viewport_fraction", 0.12);
        let min_area = frac * vw * vh;
        let big: Vec<usize> = (0..recs.len())
            .filter(|&i| recs[i].rect[2] * recs[i].rect[3] >= min_area)
            .collect();
        if big.is_empty() {
            return hidden;
        }

        // (frame, doc, node) -> record index, so a child never occludes its own parent
        let by_node: HashMap<(usize, usize, i64), usize> = recs
            .iter()
            .enumerate()
            .map(|(k, r)| ((r.frame, r.doc, r.node as i64), k))
            .collect();
        let is_kin = |a: usize, b: usize| -> bool {
            for (from, to) in [(a, b), (b, a)] {
                let mut cur = from;
                for _ in 0..40 {
                    if cur == to {
                        return true;
                    }
                    match by_node.get(&(
                        recs[cur].frame,
                        recs[cur].doc,
                        recs[cur].parent_node,
                    )) {
                        Some(p) => cur = *p,
                        None => break,
                    }
                }
            }
            false
        };

        for i in 0..recs.len() {
            let r = &recs[i];
            let cx = r.rect[0] + r.rect[2] / 2.0;
            let cy = r.rect[1] + r.rect[3] / 2.0;
            for &j in &big {
                // Paint order is per-frame; never let a parent-page box "occlude"
                // iframe content (or the reverse).
                if j == i || recs[j].frame != r.frame || recs[j].paint <= r.paint {
                    continue;
                }
                let o = &recs[j].rect;
                if !(o[0] <= cx && cx <= o[0] + o[2] && o[1] <= cy && cy <= o[1] + o[3]) {
                    continue;
                }
                if is_kin(i, j) {
                    continue;
                }
                hidden.insert(i);
                break;
            }
        }
        hidden
    }

    // -- scan -------------------------------------------------------------

    fn scan(&mut self, marks: bool, screenshot: bool) -> Result<ScanOut, String> {
        // Wait for the page rather than for a clock. Anything the previous
        // action set in motion — a navigation, an XHR, a lazy image — is still
        // arriving when this is called.
        let settled_ms = self.settle();
        let url = self.url();
        let cfg = config_for(&self.cfg, &url);

        let sets = Sets {
            iroles: cfg_set(&cfg, "interactive_roles"),
            lroles: cfg_set(&cfg, "landmark_roles"),
            itags: cfg_set(&cfg, "interactive_tags"),
            ltags: cfg_set(&cfg, "landmark_tags"),
            skip: cfg_set(&cfg, "skip_tags"),
        };
        let sig = Signals {
            ax_role: cfg_bool(&cfg, "signals", "ax_role", true),
            is_clickable: cfg_bool(&cfg, "signals", "is_clickable", true),
            interactive_tag: cfg_bool(&cfg, "signals", "interactive_tag", true),
            cursor_pointer: cfg_bool(&cfg, "signals", "cursor_pointer", true),
            focusable: cfg_bool(&cfg, "signals", "focusable", false),
        };
        let lim = Limits {
            max_elements: cfg_f64(&cfg, "limits", "max_elements", 300.0) as usize,
            max_name: cfg_f64(&cfg, "limits", "max_name_chars", 90.0) as usize,
            max_text: cfg_f64(&cfg, "limits", "max_text_chars", 140.0) as usize,
            max_href: cfg_f64(&cfg, "limits", "max_href_chars", 60.0) as usize,
            min_w: cfg_f64(&cfg, "limits", "min_width", 2.0),
            min_h: cfg_f64(&cfg, "limits", "min_height", 2.0),
            margin: cfg_f64(&cfg, "limits", "viewport_margin", 0.0),
        };

        let metrics = self.rpc("Page.getLayoutMetrics", json!({}), None)?;
        // DOMSnapshot layout.bounds are in device pixels (same as layoutViewport).
        // cssLayoutViewport is CSS pixels — using it clips everything below ~vh/dpr
        // (e.g. buttons at y=770 dropped when css vh=733 on a 2x display).
        let vp = metrics
            .get("layoutViewport")
            .cloned()
            .or_else(|| metrics.get("cssLayoutViewport").cloned())
            .unwrap_or(json!({}));
        let vw = vp.get("clientWidth").and_then(|v| v.as_f64()).unwrap_or(1280.0);
        let vh = vp.get("clientHeight").and_then(|v| v.as_f64()).unwrap_or(800.0);
        let css_vw = metrics
            .get("cssLayoutViewport")
            .and_then(|v| v.get("clientWidth"))
            .and_then(|v| v.as_f64())
            .unwrap_or(vw);
        let dpr = if css_vw > 0.0 { vw / css_vw } else { 1.0 };

        let sessions = self.attach_all();
        let n_sessions = sessions.len();
        let mut recs: Vec<Rec> = Vec::new();
        let mut abs_bounds: HashMap<i64, [f64; 4]> = HashMap::new();
        let mut origins: HashMap<String, (f64, f64)> = HashMap::new();
        let mut skipped = 0usize;
        // The viewport, captured right behind the root document's snapshot
        // (below) so the pixels and the geometry are from one moment.
        let mut shot_raw: Option<Vec<u8>> = None;

        let page_clip = [
            -lim.margin,
            -lim.margin,
            vw + 2.0 * lim.margin,
            vh + 2.0 * lim.margin,
        ];
        for (frame_i, (sess, parent, frame_id)) in sessions.iter().enumerate() {
            let mut base = (0.0, 0.0);
            let mut frame_clip = page_clip;
            let mut iframe_box: Option<[f64; 4]> = None;
            if let Some(s) = sess {
                self.prepare(Some(s));
                // locate this OOPIF's <iframe> box inside its parent session
                let owner = frame_id.as_ref().and_then(|f| {
                    self.rpc(
                            "DOM.getFrameOwner",
                            json!({ "frameId": f }),
                            parent.as_deref(),
                        )
                        .ok()
                        .and_then(|v| v.get("backendNodeId").and_then(|x| x.as_i64()))
                });
                let Some(oid) = owner else {
                    skipped += 1;
                    continue;
                };
                // Prefer content box (CSS → device). Border-box from the snapshot
                // is a few device-px too large and shifts every OOPIF mark.
                let content = self.rpc(
                        "DOM.getBoxModel",
                        json!({ "backendNodeId": oid }),
                        parent.as_deref(),
                    )
                    .ok()
                    .and_then(|v| {
                        let c = v.get("model")?.get("content")?.as_array()?;
                        let g = |i: usize| c.get(i)?.as_f64();
                        let x0 = g(0)?;
                        let y0 = g(1)?;
                        let x1 = g(2)?;
                        let y1 = g(5)?;
                        Some([
                            x0 * dpr,
                            y0 * dpr,
                            (x1 - x0) * dpr,
                            (y1 - y0) * dpr,
                        ])
                    });
                let ob = match content.or_else(|| abs_bounds.get(&oid).copied()) {
                    Some(ob) => ob,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                base = (ob[0], ob[1]);
                iframe_box = Some(ob);
                origins.insert(s.clone(), base);
            }

            let snap = self.snapshot(sess.as_deref());
            // The frame goes with the root snapshot, not at the end of the
            // scan where it used to be. Between the two sat the root's AX
            // tree, every other frame's snapshot, AX tree and walk, and the
            // occlusion and noise passes — a few hundred ms on a busy page,
            // long enough for a closing dialog to be in the tree and gone
            // from the picture, so the marks outlined a calendar the
            // screenshot no longer showed. Same call, same cost; only the
            // moment changes.
            if screenshot && sess.is_none() && shot_raw.is_none() {
                shot_raw = Some(self.capture_frame()?);
            }
            let snap = match snap {
                Ok(s) => s,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let d0 = match snap.get("documents").and_then(|d| d.as_array()) {
                Some(a) if !a.is_empty() => a[0].clone(),
                _ => continue,
            };
            let ax = self.axmap(sess.as_deref());
            let sx = d0.get("scrollOffsetX").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sy = d0.get("scrollOffsetY").and_then(|v| v.as_f64()).unwrap_or(0.0);
            // OOPIF: clip to the iframe content box so scrolled-off iframe content
            // is not painted as ghost marks on the parent page below the frame.
            if let Some(ob) = iframe_box {
                frame_clip = match intersect_rect(page_clip, ob) {
                    Some(c) => c,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
            }
            let origin = (base.0 - sx, base.1 - sy);
            Self::walk_doc(
                &snap,
                0,
                frame_i,
                origin,
                frame_clip,
                &ax,
                &sets,
                &sig,
                &lim,
                &mut recs,
                &mut abs_bounds,
                0,
            );
        }

        let hidden = Self::occluded(&recs, vw, vh, &cfg);
        let n_occluded = hidden.len();
        for i in &hidden {
            if recs[*i].kind == 'a' {
                recs[*i].kind = if recs[*i].name.is_empty() { 'x' } else { 't' };
            }
        }
        recs.retain(|r| r.kind != 'x');
        let n_noise = Self::drop_noise(&mut recs, &cfg);

        // [1] is ALWAYS the page itself, on every scan of every page: the
        // surface `scroll` acts on when the whole document should move, and
        // the one id a model can count on without having seen the tree yet.
        // Reserved here, before anything else is numbered, so a page's own
        // first element is [2] and the reservation can never shift.
        //
        // It carries the viewport rect, which makes it resolve like any other
        // id — a scroll aimed at [1] lands in the middle of the page — and it
        // gives the caller the viewport size to scale a scroll step by,
        // without a second round trip to ask for it.
        recs.insert(
            0,
            Rec {
                tag: "page".into(),
                role: String::new(),
                // No parent, and a node id no real element can hold, so the
                // synthetic record cannot collide with the DOM in the depth,
                // kinship or occlusion maps.
                parent_node: -1,
                node: usize::MAX,
                doc: 0,
                // Frame 0: render() starts a `<frame>` block whenever this
                // changes, and the page is not a frame boundary.
                frame: 0,
                rect: [0.0, 0.0, vw, vh],
                paint: -1,
                kind: 'v',
                name: String::new(),
                idx: Some(1),
                ty: None,
                href: None,
                checked: false,
                expanded: None,
                val: None,
                editable: false,
            },
        );

        // Number interactive elements and substantial text in document order
        // so search snippets sit next to their result in the index sequence.
        let mut n = 1usize;   // [1] is the page, above
        for r in recs.iter_mut() {
            if r.kind == 'v' {
                continue;     // already numbered, and never re-numbered
            }
            let substantial_text = r.kind == 't'
                && !r.name.is_empty()
                && r.name.chars().count() >= 12
                && r.rect[2] >= 40.0
                && r.rect[3] >= 12.0
                && r.rect[2] * r.rect[3] >= 800.0;
            let want = r.kind == 'a' || substantial_text;
            if !want {
                continue;
            }
            if n < lim.max_elements {
                // 1-based on purpose: [0] never exists — matching the tab
                // numbering and the prompt's index rule, and making an
                // unfilled/defaulted id=0 a clean miss instead of a silent
                // click on the first element.
                r.idx = Some(n + 1);
                n += 1;
            } else if r.kind == 'a' {
                r.kind = 't';
            }
        }

        // This scan's [id] -> rect map, published to the caller and kept
        // nowhere: the browser side resolves ids to points against the tree it
        // was handed, so the tree the model saw and the point acted on are
        // provably from one scan. Holding a copy here would be a second
        // answer to the same question, one scan out of date the moment the
        // page moves.
        let mut hits: Vec<(usize, [f64; 4])> =
            recs.iter().filter_map(|r| r.idx.map(|i| (i, r.rect))).collect();
        hits.sort_by_key(|(k, _)| *k);

        let mut shot: Option<Vec<u8>> = None;
        let mut shot_plain: Option<Vec<u8>> = None;
        if screenshot {
            // In hand since the root snapshot: the root is always the first
            // session and is never skipped before that point. The capture
            // here only guards a future reordering of that loop.
            let raw = match shot_raw.take() {
                Some(raw) => raw,
                None => self.capture_frame()?,
            };
            shot = Some(if marks {
                draw_marks(&raw, &recs).unwrap_or_else(|| raw.clone())
            } else {
                raw.clone()
            });
            // The same frame with nothing drawn on it. The marks are for the
            // MODEL: it reads the page by index, so it needs them. A person
            // watching the run does not, and a screen of numbered boxes is
            // harder to read than the page itself, so the app is shown this
            // one instead (browser.rs gates on element::DEBUG, exactly as
            // mac/tree/element.py does). One extra clone of a viewport JPEG
            // per scan, against a scan that already costs a round trip.
            shot_plain = Some(raw);
        }

        Ok(ScanOut {
            tree: render(&recs),
            count: n,
            sessions: n_sessions,
            skipped,
            occluded: n_occluded,
            noise: n_noise,
            screenshot: shot,
            screenshot_plain: shot_plain,
            url,
            settled_ms,
            hits,
            dpr,
        })
    }
}

pub struct ScanOut {
    pub tree: String,
    pub count: usize,
    pub sessions: usize,
    pub skipped: usize,
    pub occluded: usize,
    pub noise: usize,
    /// The frame WITH the numbered marks painted on: what the model is handed.
    pub screenshot: Option<Vec<u8>>,
    /// The same frame with nothing drawn on it: what the app's image panel is
    /// shown when DEBUG is off. Always captured, so the choice stays a read.
    pub screenshot_plain: Option<Vec<u8>>,
    pub url: String,
    /// ms spent waiting for the page to go quiet before this scan.
    pub settled_ms: u64,
    /// [id] -> viewport rect in DEVICE px, and the device-px-per-CSS-px ratio
    /// to convert them. Published so the CALLER can resolve an id to a point
    /// itself: the tree it was handed and the geometry behind that tree then
    /// come from one scan, instead of the caller holding names while this
    /// process privately holds the coordinates they refer to.
    pub hits: Vec<(usize, [f64; 4])>,
    pub dpr: f64,
}

// ============================================================ debug dump

/// One scan's own folder under `debug/iteration_<n>/`, holding the tree and
/// the EXACT annotated bytes the model is handed — so the record is
/// byte-identical to the payload by construction, not a re-render of it.
/// Mirrors mac/tree/element.py's debug layout. Callers gate on DEBUG.
pub fn write_debug(
    iteration: usize,
    header: &str,
    tree: &str,
    shot: Option<&[u8]>,
) -> Result<String, String> {
    let dir = format!("{}/iteration_{}", DEBUG_DIR, iteration);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(format!("{}/tree.txt", dir), format!("{}{}\n", header, tree))
        .map_err(|e| e.to_string())?;
    if let Some(bytes) = shot {
        std::fs::write(format!("{}/annotated_screenshot.jpg", dir), bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

// ============================================================ render

fn line_of(r: &Rec) -> String {
    match (r.kind, r.idx) {
        ('a', Some(i)) => {
            let mut bits: Vec<String> = Vec::new();
            if let Some(t) = &r.ty {
                bits.push(format!("type=\"{}\"", t));
            }
            if !r.role.is_empty() && r.role != r.tag {
                bits.push(format!("role=\"{}\"", r.role));
            }
            if let Some(h) = &r.href {
                bits.push(format!("href=\"{}\"", h));
            }
            if let Some(v) = &r.val {
                bits.push(format!("value=\"{}\"", v));
            }
            if r.checked {
                bits.push("checked".into());
            }
            if let Some(e) = r.expanded {
                bits.push(if e { "expanded".into() } else { "collapsed".to_string() });
            }
            if r.editable {
                bits.push("editable".into());
            }
            let head = if bits.is_empty() {
                r.tag.clone()
            } else {
                format!("{} {}", r.tag, bits.join(" "))
            };
            let body = if r.name.is_empty() {
                format!("<{} />", head)
            } else {
                format!("<{}>{}</{}>", head, r.name, r.tag)
            };
            format!("[{}] {}", i, body)
        }
        ('l', _) => {
            let head = if r.role.is_empty() {
                r.tag.clone()
            } else {
                format!("{} role=\"{}\"", r.tag, r.role)
            };
            if r.name.is_empty() {
                format!("<{}>", head)
            } else {
                format!("<{}>{}</{}>", head, r.name, r.tag)
            }
        }
        // The page itself. Named rather than left bare so the model can see
        // what [1] is without being told, and marked scrollable because that
        // is the only thing it is for.
        ('v', Some(i)) => format!("[{}] <page scrollable>the whole page</page>", i),
        ('t', Some(i)) => format!("[{}] <text>{}</text>", i, r.name),
        _ => r.name.clone(),
    }
}

fn render(recs: &[Rec]) -> String {
    let idx: HashMap<(usize, usize, i64), usize> = recs
        .iter()
        .enumerate()
        .map(|(k, r)| ((r.frame, r.doc, r.node as i64), k))
        .collect();

    // iterative depth so a deep DOM cannot blow the stack
    let mut depth = vec![usize::MAX; recs.len()];
    for k in 0..recs.len() {
        if depth[k] != usize::MAX {
            continue;
        }
        let mut chain = Vec::new();
        let mut cur = k;
        let mut d;
        loop {
            if depth[cur] != usize::MAX {
                d = depth[cur];
                break;
            }
            chain.push(cur);
            match idx.get(&(recs[cur].frame, recs[cur].doc, recs[cur].parent_node)) {
                Some(&p) if p != cur && chain.len() < 200 => cur = p,
                _ => {
                    d = usize::MAX;
                    break;
                }
            }
        }
        for c in chain.into_iter().rev() {
            d = if d == usize::MAX { 0 } else { d + 1 };
            depth[c] = d;
        }
    }

    let mut lines = vec!["<element>".to_string()];
    let mut prev_frame: Option<usize> = None;
    for (k, r) in recs.iter().enumerate() {
        if let Some(p) = prev_frame {
            if p != r.frame {
                lines.push("  <frame>".to_string());
            }
        }
        prev_frame = Some(r.frame);
        let txt = line_of(r);
        if !txt.is_empty() {
            lines.push(format!("{}{}", "  ".repeat(depth[k] + 1), txt));
        }
    }
    lines.push("</element>".to_string());
    lines.join("\n")
}

// ============================================================ marks

const PAL: [[u8; 3]; 6] = [
    [225, 29, 72],
    [14, 165, 233],
    [22, 163, 74],
    [217, 119, 6],
    [124, 58, 237],
    [8, 145, 178],
];

/// 3x5 bitmap digits - avoids pulling in a font crate
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// Marks are painted onto the image here. The page is never touched.
fn draw_marks(jpeg: &[u8], recs: &[Rec]) -> Option<Vec<u8>> {
    use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, ImageFormat};
    let img = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg).ok()?;
    let mut rgb = img.to_rgb8();
    let (iw, ih) = (rgb.width() as f64, rgb.height() as f64);
    // Snapshot bounds and the screenshot are both device pixels of the same
    // viewport, so rects land on the image 1:1 — do NOT rescale them.
    // The old `iw / vw` "measured dpr" was wrong: layoutViewport.clientWidth
    // excludes the scrollbar (2370) while the shot is the full width (2400),
    // so every mark was stretched ~1.3% and drifted right/down the further it
    // sat from the origin — ~30px off at the right edge of a 2x display.
    let stroke = 2i64;
    let sc = 2i64; // font pixel size

    let mut put = |x: i64, y: i64, c: [u8; 3]| {
        if x >= 0 && y >= 0 && (x as f64) < iw && (y as f64) < ih {
            rgb.put_pixel(x as u32, y as u32, image::Rgb(c));
        }
    };

    for r in recs {
        let i = match r.idx {
            Some(i) => i,
            None => continue,
        };
        // The page carries an id but no box: an outline around the whole
        // screenshot marks nothing and would just crowd the real elements.
        if r.kind == 'v' {
            continue;
        }
        let c = PAL[i % PAL.len()];
        let x0 = r.rect[0].round() as i64;
        let y0 = r.rect[1].round() as i64;
        let x1 = (r.rect[0] + r.rect[2]).round() as i64;
        let y1 = (r.rect[1] + r.rect[3]).round() as i64;

        for t in 0..stroke {
            for x in x0..=x1 {
                put(x, y0 + t, c);
                put(x, y1 - t, c);
            }
            for y in y0..=y1 {
                put(x0 + t, y, c);
                put(x1 - t, y, c);
            }
        }

        // index badge inside the box, top-left
        let label = i.to_string();
        let pad = sc;
        let lw = label.len() as i64 * (4 * sc) + 2 * pad;
        let lh = 5 * sc + 2 * pad;
        let box_w = (x1 - x0).max(0);
        let box_h = (y1 - y0).max(0);
        // shrink badge if the element is tiny so it still fits inside
        let (lw, lh, sc_l) = if lw > box_w || lh > box_h {
            let fit = ((box_w.max(1) as f64 / lw as f64).min(box_h.max(1) as f64 / lh as f64))
                .clamp(0.35, 1.0);
            let sc2 = ((sc as f64) * fit).round().max(1.0) as i64;
            let pad2 = sc2;
            (
                label.len() as i64 * (4 * sc2) + 2 * pad2,
                5 * sc2 + 2 * pad2,
                sc2,
            )
        } else {
            (lw, lh, sc)
        };
        let lx = x0;
        let ly = y0;
        for yy in ly..ly + lh {
            for xx in lx..lx + lw {
                put(xx, yy, c);
            }
        }
        for (di, ch) in label.chars().enumerate() {
            let d = match ch.to_digit(10) {
                Some(d) => DIGITS[d as usize],
                None => continue,
            };
            let ox = lx + sc_l + di as i64 * (4 * sc_l);
            let oy = ly + sc_l;
            for (row, bits) in d.iter().enumerate() {
                for col in 0..3 {
                    if bits & (1 << (2 - col)) != 0 {
                        for py in 0..sc_l {
                            for px_ in 0..sc_l {
                                put(
                                    ox + col * sc_l + px_,
                                    oy + row as i64 * sc_l + py,
                                    [255, 255, 255],
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 80)
        .encode(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}
