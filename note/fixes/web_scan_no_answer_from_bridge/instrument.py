"""Instrument the scratch COPY of AutoCuaBridge (never the repo's) with a trace log.

Every patch is an exact anchor replaced once; an anchor that is missing or
repeated stops the script, so the copy cannot silently drift from the repo.
"""
import json
import sys
from pathlib import Path

ext = Path(sys.argv[1])


def patch(name, pairs):
    p = ext / name
    s = p.read_text()
    for old, new in pairs:
        n = s.count(old)
        if n != 1:
            raise SystemExit(f"{name}: anchor found {n} times: {old[:80]!r}")
        s = s.replace(old, new)
    p.write_text(s)


TRACE = r'''
// ---- repro instrumentation (scratch copy only) ----
const __trace = [];
function T(label, extra) {
  const e = { t: Date.now(), label };
  if (extra !== undefined) e.x = extra;
  __trace.push(e);
  if (__trace.length > 4000) __trace.splice(0, __trace.length - 4000);
  try { console.log("[trace]", e.t, label, extra === undefined ? "" : JSON.stringify(extra)); } catch (err) {}
}
const __frameSum = (rs) => (rs || []).map((r) => {
  const v = r && r.result;
  return {
    f: r && r.frameId, doc: r && r.documentId ? String(r.documentId).slice(0, 8) : undefined,
    path: v && v.path, covered: v && v.covered, orphan: v && v.orphan, skipped: v && v.skipped,
    err: v && v.error, recs: v && v.recs ? v.recs.length : undefined, url: v && v.url,
    kids: v && v.kids ? v.kids.length : undefined, ms: v && v.ms,
  };
});
'''

# element.js is the first script background.js imports: the trace lives there.
patch("element.js", [
    ("const ELEMENT_DEFAULT_CONFIG = {", TRACE + "\nconst ELEMENT_DEFAULT_CONFIG = {"),
    # page side: a marker in the extension's world of each frame, read by the trace request
    ("""function elementScanPage(cfg, scan) {
  let frameElement = null;""",
     """function elementScanPage(cfg, scan) {
  const __st = { nonce: scan.nonce, href: String(location.href), top: window.parent === window, t0: Date.now(), phase: "start", vis: document.visibilityState, ready: document.readyState };
  try { self.__AutoCuaScanState = __st; } catch (e) {}
  let frameElement = null;"""),
    ("""  const done = (result) => {
    walked = true;""",
     """  const done = (result) => {
    __st.phase = "done"; __st.t1 = Date.now();
    walked = true;"""),
    ("""    window.addEventListener("message", onPlace);
    const hello = () =>""",
     """    window.addEventListener("message", onPlace);
    __st.phase = "waiting-place";
    const hello = () =>"""),
    ("""      clearInterval(timer);
      window.removeEventListener("message", onPlace);
      resolve(done({ orphan: true }));""",
     """      clearInterval(timer);
      window.removeEventListener("message", onPlace);
      __st.orphan = true;
      resolve(done({ orphan: true }));"""),
    # service worker side: every await of one scan
    ("""  const settled = await elementSettle(tab.id);""",
     """  T("scan.settle.start", { tab: tab.id });
  const settled = await elementSettle(tab.id);
  T("scan.settle.done", { ms: settled });"""),
    ("""  let results = null;
  let failure = null;
  const scan = {""",
     """  T("scan.shot.start", { active: tab.active, how: shotP === null ? "none" : tab.active ? "captureVisibleTab" : "fallback" });
  if (shotP) shotP.then((v) => T("scan.shot.settled", typeof v === "string" ? { ok: v.length } : { err: String((v && v.message) || v) }));
  let results = null;
  let failure = null;
  const scan = {"""),
    ("""  if (opts.watch) opts.watch.ahead();
  try {
    results = await chrome.scripting.executeScript({""",
     """  if (opts.watch) opts.watch.ahead();
  T("scan.exec.start", { nonce: scan.nonce });
  try {
    results = await chrome.scripting.executeScript({"""),
    ("""  } catch (e) {
    failure = e;
  }
  // A page the extension may not script""",
     """  } catch (e) {
    failure = e;
  }
  T("scan.exec.done", { failure: failure ? String((failure && failure.message) || failure) : null, frames: __frameSum(results) });
  // A page the extension may not script"""),
    ("""    results = await opts.fallback.read(tab, cfg, scan);""",
     """    T("scan.fallback.read.start");
    results = await opts.fallback.read(tab, cfg, scan);
    T("scan.fallback.read.done", { frames: __frameSum(results) });"""),
    ("""  let captured = shotP ? await shotP : null;""",
     """  T("scan.shot.await");
  let captured = shotP ? await shotP : null;
  T("scan.shot.got", typeof captured === "string" ? "ok" : String((captured && captured.message) || captured));"""),
    ("""    captured = await opts.fallback.capture(tab).catch((e) => e);""",
     """    T("scan.shot.fallback.start");
    captured = await opts.fallback.capture(tab).catch((e) => e);
    T("scan.shot.fallback.done", typeof captured === "string" ? "ok" : String((captured && captured.message) || captured));"""),
    ("""    shot = opts.marks === false ? plain : await elementDrawMarks(plain, out.marks);""",
     """    T("scan.marks.start");
    shot = opts.marks === false ? plain : await elementDrawMarks(plain, out.marks);
    T("scan.marks.done");"""),
])

patch("tools.js", [
    ("""function toolsWatchScan(tabId) {
  if (!toolsOurs.has(tabId)) return { ahead() {}, back() {}, stop() {} };""",
     """function toolsWatchScan(tabId) {
  T("watch.new", { tabId, ours: toolsOurs.has(tabId) });
  if (!toolsOurs.has(tabId)) return { ahead() {}, back() {}, stop() {} };"""),
    ("""    timer = setTimeout(async () => {
      if (toolsDialogs.has(tabId) || (await toolsAnswers(tabId))) return;""",
     """    T("watch.arm", { ms });
    timer = setTimeout(async () => {
      T("watch.fire", { ms });
      const __a = toolsDialogs.has(tabId) || (await toolsAnswers(tabId));
      T("watch.probe", { answered: __a });
      if (__a) return;"""),
    ("""      toolsAnswers(tabId, TOOLS_FROZEN_MS + TOOLS_PROBE_MS).then((answered) => {""",
     """      T("watch.ahead");
      toolsAnswers(tabId, TOOLS_FROZEN_MS + TOOLS_PROBE_MS).then((answered) => {
        T("watch.ahead.answered", { answered });"""),
    ("""function toolsReopen(tabId, why) {""",
     """function toolsReopen(tabId, why) {
  T("reopen.called", { tabId, why });"""),
    ("""  if (method === "Inspector.targetCrashed") {""",
     """  if (method === "Page.frameNavigated" && params && params.frame && !params.frame.parentId) {
    T("cdp.frameNavigated", { tabId, url: params.frame.url, type: params.type });
  }
  if (method === "Page.frameStartedNavigating" && params && !params.parentId) {
    T("cdp.frameStartedNavigating", { tabId, url: params.url, navigationType: params.navigationType });
  }
  if (method === "Inspector.targetCrashed") {"""),
    ("""    if (kind !== "beforeunload") {
      toolsDialogs.set(tabId, params);""",
     """    T("cdp.dialog", { tabId, kind });
    if (kind !== "beforeunload") {
      toolsDialogs.set(tabId, params);"""),
])

patch("background.js", [
    ("""async function handle(msg, line) {
  if (typeof msg.id === "string" && line.pending.has(msg.id)) {""",
     """// Trace of the navigation itself, frame by frame (webNavigation is added to this copy's manifest).
for (const ev of ["onBeforeNavigate", "onCommitted", "onDOMContentLoaded", "onCompleted", "onErrorOccurred"]) {
  try {
    chrome.webNavigation[ev].addListener((d) => {
      if (d.frameId !== 0 && ev !== "onCommitted" && ev !== "onErrorOccurred") return;
      T("nav." + ev, { tab: d.tabId, f: d.frameId, pf: d.parentFrameId, url: String(d.url || "").slice(0, 120), life: d.documentLifecycle, ftype: d.frameType, err: d.error, doc: d.documentId ? String(d.documentId).slice(0, 8) : undefined, tt: d.transitionType });
    });
  } catch (e) {}
}
chrome.tabs.onUpdated.addListener((id, info) => T("tab.updated", { id, info }));

// The trace request: what this worker saw, and every frame of the tab as it is now.
async function traceDump(tabId) {
  const out = { now: Date.now(), trace: __trace.slice() };
  out.tab = await chrome.tabs.get(tabId).catch((e) => String(e));
  out.frames = await chrome.webNavigation.getAllFrames({ tabId }).catch((e) => String(e));
  const probe = chrome.scripting.executeScript({
    target: { tabId, allFrames: true }, injectImmediately: true,
    func: () => ({ href: String(location.href).slice(0, 120), vis: document.visibilityState, ready: document.readyState, st: self.__AutoCuaScanState || null }),
  }).then((rs) => rs.map((r) => ({ f: r.frameId, doc: r.documentId ? String(r.documentId).slice(0, 8) : undefined, v: r.result })), (e) => "probe failed: " + String(e && e.message || e));
  out.states = await Promise.race([probe, new Promise((r) => setTimeout(() => r("probe gave no answer in 3 s"), 3000))]);
  const top = chrome.scripting.executeScript({ target: { tabId }, injectImmediately: true, func: () => 1 })
    .then(() => "answered", (e) => "refused: " + String(e && e.message || e));
  out.topProbe = await Promise.race([top, new Promise((r) => setTimeout(() => r("no answer in 2 s"), 2000))]);
  out.attached = [...toolsAttached];
  out.ours = [...toolsOurs.keys()];
  return out;
}

async function handle(msg, line) {
  if (msg && msg.type !== "trace") T("req", { id: msg.id, type: msg.type, tabId: msg.tabId });
  if (typeof msg.id === "string" && line.pending.has(msg.id)) {"""),
    ("""        const tab = await targetTab(msg.tabId);
        // A page no extension may read,""",
     """        const tab = await targetTab(msg.tabId);
        T("scan.tab", { id: tab.id, url: tab.url, pendingUrl: tab.pendingUrl, status: tab.status, active: tab.active });
        // A page no extension may read,"""),
    ("""        } catch (e) {
          // The tab moved on during the read to a page no extension may read""",
     """        } catch (e) {
          T("scan.caught", String((e && e.message) || e));
          // The tab moved on during the read to a page no extension may read"""),
    ("""      case "tabs":
        result = await chrome.tabs.query({});
        break;""",
     """      case "tabs":
        result = await chrome.tabs.query({});
        break;
      case "trace":
        result = await traceDump(msg.tabId);
        break;
      case "trace_clear":
        __trace.length = 0;
        result = { cleared: true };
        break;"""),
    ("""  await new Promise((r) => setTimeout(r, 0));
  send(line, reply);""",
     """  await new Promise((r) => setTimeout(r, 0));
  if (msg && msg.type !== "trace") T("reply", { id: msg.id, type: msg.type, error: reply.error });
  send(line, reply);"""),
])

m = json.loads((ext / "manifest.json").read_text())
if "webNavigation" not in m["permissions"]:
    m["permissions"].append("webNavigation")
(ext / "manifest.json").write_text(json.dumps(m, indent=2) + "\n")
print("instrumented", ext)
