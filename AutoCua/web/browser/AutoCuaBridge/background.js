// AutoCuaBridge: the agent reaches the browser through this extension instead of a
// debugging port. The agent runs a localhost websocket server; whoever launches Chrome
// writes its port and a per-launch token into config.json in this folder, and the
// extension dials it. element.js does the scanning; tools.js does the tools (click,
// input, keyboard, scroll, run_script, the tab moves, the cursor and the glow), the way
// AutoCua/web/controller does them over CDP.
//
// Requests from the agent: {id, type: "scan", tabId?, overlay?, marks?, screenshot?},
// {id, type: "tabs"} and the tool requests listed in tools.js, each naming its tab;
// replies {id, result} or {id, error}. Messages from the extension carry no id or a
// string id: {type: "notice", text} when the browser did something behind the agent's
// back (a dialog answered, a page crashed), {type: "dialog", tabId, params} when a popup
// opens or closes, {type: "reopened", from, to} when a frozen or crashed tab was opened
// again, {type: "keepalive"}, and {id: "x<n>", type: "save", ...} from the Test button
// (below), answered the same way.
importScripts("element.js", "tools.js");

elementTrackRequests();

// The agents on the line, one websocket each, by port. config.json in this folder lists
// them ({"bridges": [{port, token}, ...]}: bridge.rs writes one entry per Bridge and takes
// it out when that bridge goes), and every half second the worker dials each listed port
// it has no line to, so an agent that starts while others are on (the parallel runner's
// children) is on the line within a second. The hand test writes one entry the old way
// ({port, token}), which reads as a list of one.
const lines = new Map(); // port -> {port, token, ws, pending, next, opened}
let lastToken = null;

/// The line of the agent that drives a tab (tools.js toolsOurs), or null for a tab nobody
/// drives or an agent that is gone.
function lineOf(tabId) {
  const line = typeof tabId === "number" ? toolsOurs.get(tabId) : null;
  return line && line.ws && line.ws.readyState === WebSocket.OPEN ? line : null;
}

/// Send to one line, or to every open line when the message has no owner (a notice about
/// the browser as a whole).
function send(line, message) {
  const text = JSON.stringify(message);
  for (const l of line ? [line] : lines.values()) {
    if (l.ws && l.ws.readyState === WebSocket.OPEN) l.ws.send(text);
  }
}

// What the browser did behind an agent's back, sent on the moment it happens to the agent
// whose tab it concerns; the agent side keeps them for the next tool result (browser.rs
// take_notices). A notice about no tab in particular goes to every agent.
toolsOnNotice = (text, tabId) => send(lineOf(tabId), { type: "notice", text });

// A popup (alert, confirm or prompt) that opened on a tab (params) or closed (null): the agent side
// shows it in place of the page (bridge.rs dialogs) without having to ask.
toolsOnDialog = (tabId, params) => send(lineOf(tabId), { type: "dialog", tabId, params });

// A tab of an agent's that froze or crashed was closed and its page opened again in a new
// tab (tools.js toolsReopen): that agent drives the new one from now on.
toolsOnReopened = (from, to) => send(lineOf(from), { type: "reopened", from, to });

// Whether `line` is the only one open (tools.js toolsLetGo: the last agent to go takes the
// glow and the debugger off everything; one among others, off its own tabs only).
toolsLastLine = (line) => [...lines.values()].every((l) => l === line || !l.opened);

async function connect() {
  let cfg = null;
  try {
    cfg = await (await fetch(chrome.runtime.getURL("config.json"))).json();
  } catch (e) {
    cfg = null; // no config yet: nobody to talk to
  }
  const listed = !cfg ? [] : Array.isArray(cfg.bridges) ? cfg.bridges : [cfg];
  for (const entry of listed) {
    if (typeof entry.port === "number" && !lines.has(entry.port)) dial(entry.port, String(entry.token || ""));
  }
  setTimeout(connect, 500);
}

function dial(port, token) {
  const line = { port, token, ws: null, pending: new Map(), next: 0, opened: false };
  lines.set(port, line);
  const ws = new WebSocket(`ws://127.0.0.1:${port}/bridge?token=${encodeURIComponent(token)}`);
  line.ws = ws;
  ws.onopen = () => {
    line.opened = true;
    // A new run (no other agent on the line, and a token this worker has not seen):
    // whatever the person stopped in the last one is theirs to start again (tools.js
    // toolsStoppedByPerson). An agent joining a run already on resets nothing.
    if (token !== lastToken && toolsLastLine(line)) toolsReset();
    lastToken = token;
  };
  ws.onmessage = (ev) => handle(JSON.parse(ev.data), line);
  ws.onclose = () => {
    lines.delete(port);
    for (const [, p] of line.pending) {
      clearTimeout(p.timer);
      p.reject(new Error("the bridge closed"));
    }
    line.pending.clear();
    // The agent is gone: its run ended, or it was stopped with Ctrl-C, killed or crashed
    // (the system closes its socket in every case). Let its tabs go, the way Chrome drops
    // what a debugging-port client set up when its socket closes; the last agent to go
    // lets the whole browser go. The person's Cancel holds until a new token. A port
    // still listed is dialled again on the next pass.
    if (line.opened) toolsLetGo(line, toolsLastLine(line)).catch(() => {});
  };
}

// The tab a request names, or the active tab of the window last in front.
async function targetTab(tabId) {
  if (typeof tabId === "number") return chrome.tabs.get(tabId);
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!tab) throw new Error("no active tab");
  return tab;
}

// Requests this extension makes to the agent side, answered with {id, result} or
// {id, error}; string ids keep them apart from the agent's own numbered requests.
function bridgeRequest(payload, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    const line = [...lines.values()].find((l) => l.opened && l.ws && l.ws.readyState === WebSocket.OPEN);
    if (!line) {
      reject(new Error("the bridge is not connected: run test.command"));
      return;
    }
    const id = `x${++line.next}`;
    const timer = setTimeout(() => {
      line.pending.delete(id);
      reject(new Error("no answer from the bridge"));
    }, timeoutMs);
    line.pending.set(id, { resolve, reject, timer });
    line.ws.send(JSON.stringify({ id, ...payload }));
  });
}

async function handle(msg, line) {
  if (typeof msg.id === "string" && line.pending.has(msg.id)) {
    const p = line.pending.get(msg.id);
    line.pending.delete(msg.id);
    clearTimeout(p.timer);
    if (msg.error) p.reject(new Error(msg.error));
    else p.resolve(msg.result);
    return;
  }
  let reply;
  try {
    let result;
    switch (msg.type) {
      case "scan": {
        const tab = await targetTab(msg.tabId);
        // A page no extension may read, shown or on its way (a redirect to the Web Store
        // still pending): its own scan, which says so.
        const away = [tab.url, tab.pendingUrl].find((u) => u && toolsUnreachable(u));
        if (away) {
          result = toolsChromePageScan({ ...tab, url: away });
          break;
        }
        // A popup holds the page (before the read, or opening during it):
        // nothing can be read until it is answered, so the answer is the popup.
        const held = () => ({ dialog: toolsDialogs.get(tab.id) });
        if (toolsDialogs.has(tab.id)) {
          result = held();
          break;
        }
        // A page that froze is closed and opened again, which ends the read here with an
        // error (tools.js toolsWatchScan: a question just ahead of the read, 5 s for the
        // picture once the read is back, and a backstop for a read that never comes back).
        const watch = toolsWatchScan(tab.id);
        try {
          try {
            // A tab in the background (the person is looking at another one) is read
            // where it is and photographed through the debugger, like element.rs does.
            result = await toolsUnlessDialog(tab.id, elementScan(tab, { ...msg, fallback: toolsScanFallback, watch }), held);
          } catch (e) {
            // When that picture cannot be taken, the tab is put on show for Chrome's own
            // capture; not after the person took the browser back.
            if (tab.active || toolsStoppedByPerson || !/^screenshot failed/.test(String((e && e.message) || e))) throw e;
            await chrome.tabs.update(tab.id, { active: true });
            await new Promise((r) => setTimeout(r, 300));
            result = await elementScan({ ...tab, active: true }, { ...msg, fallback: toolsScanFallback, watch });
          }
        } catch (e) {
          // The tab moved on during the read to a page no extension may read (a redirect
          // that landed on the Web Store): that page's scan instead of the error.
          const now = await chrome.tabs.get(tab.id).catch(() => null);
          const gone = now && [now.url, now.pendingUrl].find((u) => u && toolsUnreachable(u));
          if (gone) {
            result = toolsChromePageScan({ ...now, url: gone });
            break;
          }
          // A read that ran out of time (the screenshot, a frame): a page that does not
          // answer a cheap question within TOOLS_FROZEN_MS either froze during the read,
          // and is opened again (browser.rs page_froze).
          if (/timed out/i.test(String((e && e.message) || e)) && toolsOurs.has(tab.id) &&
              !toolsDialogs.has(tab.id) && !(await toolsAnswers(tab.id, TOOLS_FROZEN_MS))) {
            toolsReopen(tab.id, "froze").catch(() => {});
          }
          // The page crashed or froze under the read and is being opened again: the agent
          // side hears which tab took its place before it hears of this failure.
          await toolsAwaitReopen(tab.id);
          throw e;
        } finally {
          watch.stop();
        }
        break;
      }
      case "tabs":
        result = await chrome.tabs.query({});
        break;
      default:
        result = await toolsHandle(msg, line);
        if (result === undefined) throw new Error(`unknown request: ${msg.type}`);
    }
    reply = { id: msg.id, result };
  } catch (e) {
    reply = { id: msg.id, error: String((e && e.message) || e) };
  }
  // One beat, so what the request set off and the debugger has already reported (a
  // dialog the page opened on this click) is sent as a notice BEFORE the reply, and
  // lands on this action's result rather than the next one's.
  await new Promise((r) => setTimeout(r, 0));
  send(line, reply);
}

// An open socket keeps the service worker alive; the keepalive makes sure it stays open.
// With nobody on the line Chrome would put the worker to sleep after 30 s and the
// dialling would stop, so a later run (bridge.rs take_chrome) would find no one: an
// extension API call every 20 s keeps it awake instead, since Chrome gives a worker
// more time for each one.
setInterval(() => {
  let any = false;
  for (const l of lines.values()) {
    if (l.ws && l.ws.readyState === WebSocket.OPEN) {
      l.ws.send(JSON.stringify({ type: "keepalive" }));
      any = true;
    }
  }
  if (!any) chrome.runtime.getPlatformInfo(() => {});
}, 20000);

connect();

// ============================================================ the Test button (test.html)

// The popup's Test button: scan the page it was opened on with the extension's own
// scanner, hand the result to the agent side over the bridge, which writes it under
// debug/iteration_N/ the way element.rs writes its DEBUG output (bridge_test.rs), and
// give the popup the result with the timings and where it went.
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || msg.type !== "test") return false;
  testScan().then(sendResponse, (e) => sendResponse({ ok: false, error: String((e && e.message) || e) }));
  return true; // sendResponse is called later
});

// The page to test: the active tab of the window in front or, when that is this
// extension's own page (test.html opened as a tab), the web page used last there.
async function testTab() {
  const [active] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!active) throw new Error("no active tab");
  if (!active.url || !active.url.startsWith(chrome.runtime.getURL(""))) return active;
  const pages = await chrome.tabs.query({ windowId: active.windowId, url: ["http://*/*", "https://*/*", "file://*/*"] });
  pages.sort((a, b) => (b.lastAccessed || 0) - (a.lastAccessed || 0));
  if (!pages.length) throw new Error("open a web page in this window first");
  return pages[0];
}

async function testScan() {
  const tab = await testTab();
  // captureVisibleTab needs the page in front; put it there for the scan if it is not
  let back = null;
  if (!tab.active) {
    const [cur] = await chrome.tabs.query({ active: true, windowId: tab.windowId });
    back = cur ? cur.id : null;
    await chrome.tabs.update(tab.id, { active: true });
    await new Promise((r) => setTimeout(r, 400));
  }
  let result;
  try {
    result = await elementScan({ ...tab, active: true });
  } finally {
    if (back !== null) await chrome.tabs.update(back, { active: true }).catch(() => {});
  }
  const out = { ok: true, result, saved: null, save_error: null };
  try {
    out.saved = await bridgeRequest({
      type: "save",
      url: result.url,
      summary: result.summary,
      timings: result.timings,
      tree: result.tree,
      hits: result.hits,
      dpr: result.dpr,
      screenshot: result.screenshot,
    });
  } catch (e) {
    out.save_error = String((e && e.message) || e);
  }
  return out;
}
