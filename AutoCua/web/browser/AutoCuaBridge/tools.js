// AutoCuaBridge: the tools, extension side.
//
// The browser half of the agent's tools, for the tools in AutoCua/web/controller to
// run through this extension instead of a debugging port. The tool itself stays in
// Rust (ids, messages, the cursor choreography, batches): a tool there sends one
// request here for the part that touches the browser, and this file does what the
// Rust tool does over CDP, with the same commands and the same parameters:
//
//   click, input, keyboard, scroll   trusted input through chrome.debugger: the same
//                                    Input.* commands as controller/{click,input,
//                                    keyboard,scroll}/service.rs
//   run_script                       Runtime.evaluate through chrome.debugger, exempt
//                                    from the page's CSP like the real tool (controller/
//                                    run_script/service.rs)
//   navigate, reload, history        Page.* through chrome.debugger: errorText and the
//                                    load event, as controller/tab/service.rs
//   new_tab, activate, wait_load,    chrome.tabs; a tab is shown by activating it in its
//   close_tab, tabs                  window, which never brings the window forward
//   blank_page                       Page.setDocumentContent, as browser.rs show_blank_page
//   settle, scroll_probe, cursor,    chrome.scripting in the extension's own world:
//   glow                             browser.rs settle_after_action, scroll_probe and the
//                                    cursor calls; the glow overlay (browser/glow) is a
//                                    content script here instead of a document-start
//                                    script registered over CDP
//   dialog                           Page.handleJavaScriptDialog: the model's answer to an
//                                    alert, confirm or prompt left open (controller/dialog)
//   release                          let the browser go (glow off, debugger off)
//
// The debugger is attached to a tab the first time the agent drives it (activate,
// new_tab) and stays attached while the agent runs, like the CDP session browser.rs
// holds per tab: Page.enable from then on means every JavaScript dialog is heard the
// moment it opens. An alert, a confirm or a prompt is left for the model, which sees it
// in place of the page and answers it with `dialog`, and nothing else goes into that
// frozen page meanwhile (toolsDialogs); a "Leave site?" is answered at once and reported
// as a notice. While it is attached Chrome shows its own "started debugging this browser"
// bar. Its Cancel button is the person taking the browser back: nothing is attached
// again until the agent side starts a new run, and every action that needs the
// debugger says so to the model. When the agent side goes away without a release
// (Ctrl-C, killed, crashed), background.js lets the browser go the same way the moment
// its socket closes (toolsLetGo). A tab of the agent's whose page froze (silent for
// TOOLS_FROZEN_MS) or crashed is closed and the same page opened again in its place
// (toolsReopen). Errors carry the same text as the Rust tools, so the model reads the
// same message in both modes.

// ============================================================ the debugger

const TOOLS_PROTOCOL = "1.3";
/// The Rust tools' per-call timeout for an input or page command (controller/*: 5.0).
const TOOLS_RPC_MS = 5000;
/// controller/tab/service.rs LOAD_TIMEOUT: how long a navigation waits for the load event.
const TOOLS_LOAD_MS = 30000;

const toolsAttached = new Set();
const toolsAttaching = new Map();
/// Set when the person presses Cancel on Chrome's debugging bar; cleared when the agent
/// side lets go (release) or a new run connects (background.js).
let toolsStoppedByPerson = false;
const TOOLS_STOPPED = "the person pressed Cancel on Chrome's \"started debugging this browser\" bar, " +
  "which takes the browser back from the agent, so this action did not run. Do not retry it: " +
  "finish with `done` and tell them where the task stands";

function toolsReset() {
  toolsStoppedByPerson = false;
  toolsReopenedUrls.clear();
  toolsReopenedTabs.clear();
}
/// tabId -> the load waiters of that tab (toolsLoadWaiter).
const toolsLoadWaiters = new Map();
/// Popups (alerts, confirms, prompts) open right now, by tab id: their opening event's params (type,
/// message, defaultPrompt). The page is frozen until one is answered (the `dialog`
/// request), so nothing is sent into it meanwhile (toolsUnlessDialog); browser.rs keeps
/// the same in its Cdp `dialogs`.
const toolsDialogs = new Map();
/// tabId -> the calls out on that tab, told when a popup opens there.
const toolsDialogWaiters = new Map();
/// browser.rs DIALOG_OPEN, verbatim.
const TOOLS_DIALOG_OPEN = "a popup (a JavaScript alert, confirm or prompt) is open on that tab " +
  "and freezes the page until it is answered: answer it with `dialog` first";
/// background.js: tells the agent side when a popup opens on a tab (params) or closes
/// (null), so it shows the popup in place of the page without asking.
let toolsOnDialog = null;

/// `work` (a promise of something done in the tab), unless a popup holds that tab or opens
/// there first: then `onOpen()` answers instead, since `work` would wait for the popup.
function toolsUnlessDialog(tabId, work, onOpen) {
  if (toolsDialogs.has(tabId)) {
    work.catch(() => {});
    return Promise.resolve().then(onOpen);
  }
  let told = null;
  const opened = new Promise((resolve, reject) => {
    told = () => {
      try { resolve(onOpen()); } catch (e) { reject(e); }
    };
    const set = toolsDialogWaiters.get(tabId) || new Set();
    set.add(told);
    toolsDialogWaiters.set(tabId, set);
  });
  return Promise.race([work, opened]).finally(() => {
    const set = toolsDialogWaiters.get(tabId);
    if (set) {
      set.delete(told);
      if (!set.size) toolsDialogWaiters.delete(tabId);
    }
  });
}

/// Tabs this side is closing (close_tab): the "Leave site?" one asks for is the agent's own
/// doing, answered without a notice, as a debugging port closes a tab without asking.
const toolsClosing = new Set();

/// Forget a tab's popup: answered, closed with its tab, or no longer ours to see.
function toolsDialogGone(tabId) {
  if (toolsDialogs.delete(tabId) && toolsOnDialog) toolsOnDialog(tabId, null);
}
/// Things the browser did behind the agent's back, in the words the model should read
/// (browser.rs notice()); background.js sends each one on as it happens, and the agent
/// side keeps them for the next tool result (and drops a repeat still waiting there).
let toolsOnNotice = null;
/// background.js: whether a line is the only agent on the line (toolsLetGo).
let toolsLastLine = () => true;

/// `tabId`: the tab the notice is about, so it reaches the agent driving it; none for the
/// browser as a whole (every agent hears it).
function toolsNotice(text, tabId) {
  if (toolsOnNotice) toolsOnNotice(text, tabId);
}

const toolsSleep = (ms) => new Promise((r) => setTimeout(r, ms));

function toolsErr(e) {
  return String((e && e.message) || e);
}

/// Attach the debugger to a tab once, and turn on what browser.rs turns on per session
/// (Cdp::attach): Page events, so navigations and dialogs are heard, and focus emulation,
/// so a tab driven in the background believes it is focused.
async function toolsAttach(tabId) {
  if (toolsStoppedByPerson) throw new Error(TOOLS_STOPPED);
  if (toolsAttached.has(tabId)) return;
  if (toolsAttaching.has(tabId)) return toolsAttaching.get(tabId);
  const p = (async () => {
    try {
      await chrome.debugger.attach({ tabId }, TOOLS_PROTOCOL);
    } catch (e) {
      // Already attached by an earlier worker that was not told: carry on with it.
      if (/already attached/i.test(toolsErr(e))) {
        // fall through
      } else if (/Cannot access|Cannot attach/i.test(toolsErr(e))) {
        // One of Chrome's own pages: no extension may act on it.
        const tab = await chrome.tabs.get(tabId).catch(() => null);
        const url = tab ? tab.url || tab.pendingUrl || "this tab" : "this tab";
        if (toolsWebStore(url)) throw new Error(`${TOOLS_WEB_STORE}. Leave it with navigate_tab back, update_tab or new_tab`);
        throw new Error(`${url} is a Chrome page, which an extension cannot act on: go to a web page with update_tab or new_tab`);
      } else {
        throw e;
      }
    }
    toolsAttached.add(tabId);
    await chrome.debugger.sendCommand({ tabId }, "Page.enable").catch(() => {});
    await chrome.debugger.sendCommand({ tabId }, "Emulation.setFocusEmulationEnabled", { enabled: true }).catch(() => {});
  })();
  toolsAttaching.set(tabId, p);
  try {
    await p;
  } finally {
    toolsAttaching.delete(tabId);
  }
}

async function toolsDetach(tabId) {
  toolsAttached.delete(tabId);
  toolsDialogGone(tabId);
  await chrome.debugger.detach({ tabId }).catch(() => {});
}

/// One CDP command on a tab, with the Rust tools' timeout; the message of a failure is
/// "<method>: <reason>", and a command that never answers reads "timed out", which the
/// run_script port below keys on like the Rust one does.
async function toolsCdp(tabId, method, params = {}, timeoutMs = TOOLS_RPC_MS) {
  // A popup holds the page: nothing sent into it is answered until the popup
  // is (browser.rs rpc), so the call fails at once. Answering the popup is the one that goes.
  // The release of a key or button whose press opened the popup counts as done (browser.rs).
  const held = method !== "Page.handleJavaScriptDialog";
  if (held && toolsDialogs.has(tabId)) {
    const release = (method === "Input.dispatchKeyEvent" && params.type === "keyUp") ||
      (method === "Input.dispatchMouseEvent" && params.type === "mouseReleased");
    if (release) return {};
    throw new Error(`${method}: ${TOOLS_DIALOG_OPEN}`);
  }
  await toolsAttach(tabId);
  let timer = null;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${method}: timed out after ${timeoutMs / 1000}s`)), timeoutMs);
  });
  try {
    const sent = chrome.debugger.sendCommand({ tabId }, method, params);
    // A popup that opens while this is out: an input event was delivered (it is what
    // opened the popup), so it counts as done; anything else fails like a later call.
    const answer = held
      ? toolsUnlessDialog(tabId, sent, () => {
        if (method.startsWith("Input.")) return {};
        throw new Error(`${method}: ${TOOLS_DIALOG_OPEN}`);
      })
      : sent;
    return await Promise.race([answer, late]);
  } catch (e) {
    const m = toolsErr(e);
    if (m === TOOLS_STOPPED || / is a Chrome page, /.test(m)) throw e;
    throw new Error(m.startsWith(method) ? m : `${method}: ${m}`);
  } finally {
    clearTimeout(timer);
  }
}

/// A wait for this tab's next load event, armed BEFORE the navigation is sent so the
/// event cannot slip past (tab/service.rs clears the buffered ones, then navigates, then
/// waits). Resolves true on the event, false when the wait lapses: a lapse is not an
/// error, the scan settles the page again before it is read.
function toolsLoadWaiter(tabId, timeoutMs = TOOLS_LOAD_MS) {
  let set = toolsLoadWaiters.get(tabId);
  if (!set) toolsLoadWaiters.set(tabId, (set = new Set()));
  let done = null;
  const promise = new Promise((resolve) => {
    let timer = null;
    done = (ok) => {
      set.delete(done);
      clearTimeout(timer);
      resolve(ok);
    };
    timer = setTimeout(() => done(false), timeoutMs);
    set.add(done);
  });
  return { promise, cancel: () => done(false) };
}

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source.tabId;
  // The load event, or a main-frame restore from the back/forward cache: a restored
  // page comes back whole and fires no load event (browser.rs wait_loaded).
  const restored = method === "Page.frameNavigated" && params && params.type === "BackForwardCacheRestore" &&
    params.frame && !params.frame.parentId;
  if (method === "Page.loadEventFired" || restored) {
    for (const done of [...(toolsLoadWaiters.get(tabId) || [])]) done(true);
    return;
  }
  if (method === "Page.javascriptDialogOpening") {
    // browser.rs note_dialog: an alert, a confirm or a prompt is left for the model (the
    // page frozen behind it); a "Leave site?" is answered at once and told to the model.
    const kind = (params && params.type) || "dialog";
    if (kind !== "beforeunload") {
      toolsDialogs.set(tabId, params);
      for (const told of [...(toolsDialogWaiters.get(tabId) || [])]) told();
      if (toolsOnDialog) toolsOnDialog(tabId, params);
      return;
    }
    const text = ((params && params.message) || "").trim();
    chrome.debugger.sendCommand({ tabId }, "Page.handleJavaScriptDialog", { accept: true }).catch(() => {});
    if (kind === "beforeunload" && toolsClosing.has(tabId)) return;
    toolsNotice(text
      ? `the page opened a JavaScript ${kind} saying "${text}" and it was accepted`
      : `the page opened a JavaScript ${kind} and it was accepted`, tabId);
    return;
  }
  if (method === "Page.javascriptDialogClosed") {
    toolsDialogGone(tabId);
    return;
  }
  if (method === "Inspector.targetCrashed") {
    // browser.rs recover_tab: the tab is closed and its page opened again.
    toolsReopen(tabId, "crashed").catch(() => {});
  }
});

chrome.debugger.onDetach.addListener((source, reason) => {
  toolsAttached.delete(source.tabId);
  for (const done of [...(toolsLoadWaiters.get(source.tabId) || [])]) done(false);
  if (reason === "canceled_by_user" && !toolsStoppedByPerson) {
    toolsStoppedByPerson = true;
    toolsNotice("the person pressed Cancel on Chrome's \"started debugging this browser\" bar and " +
      "took the browser back: actions on pages will not run again in this run. Finish with `done` " +
      "and tell them where the task stands");
  }
});

chrome.tabs.onRemoved.addListener((tabId) => {
  toolsAttached.delete(tabId);
  toolsOurs.delete(tabId);
  toolsReopenedTabs.delete(tabId);
  for (const done of [...(toolsLoadWaiters.get(tabId) || [])]) done(false);
  toolsLoadWaiters.delete(tabId);
  toolsDialogGone(tabId);
});

// ============================================================ mouse (click/service.rs)

function toolsCentre(rect) {
  const [x, y, w, h] = rect;
  return [x + w / 2, y + h / 2];
}

/// click/service.rs button(): one press or release at a point.
function toolsButton(tabId, kind, x, y, count) {
  return toolsCdp(tabId, "Input.dispatchMouseEvent", {
    type: kind, x, y, button: "left", clickCount: count, buttons: kind === "mousePressed" ? 1 : 0,
  });
}

/// click/service.rs press(): move, then press and release on the centre of `rect` (CSS
/// px); a double click is one gesture with a rising clickCount; `holdSeconds` keeps the
/// button down (hold_click).
async function toolsPress(tabId, rect, holdSeconds, times) {
  const [x, y] = toolsCentre(rect);
  await toolsCdp(tabId, "Input.dispatchMouseEvent", { type: "mouseMoved", x, y, buttons: 0 });
  const n = Math.min(Math.max(Math.trunc(times) || 1, 1), 2);
  const hold = Math.max(Number(holdSeconds) || 0, 0) * 1000;
  for (let i = 1; i <= n; i++) {
    await toolsButton(tabId, "mousePressed", x, y, i);
    if (hold > 0) await toolsSleep(hold);
    await toolsButton(tabId, "mouseReleased", x, y, i);
  }
}

// ============================================================ typing (input/service.rs)

/// input/service.rs FOCUS_WAIT_MS (= ACTION_SETTLE_CAP_MS).
const TOOLS_FOCUS_WAIT_MS = 600;

/// input/service.rs FOCUS_HOLDER, verbatim.
const TOOLS_FOCUS_HOLDER = "(function () {" +
  "var a = document.activeElement;" +
  "while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;" +
  "if (!a) return 'nothing';" +
  "var t = a.tagName, ty = (a.getAttribute('type') || '').toLowerCase();" +
  "if (t === 'INPUT' && !/^(button|submit|reset|checkbox|radio|file|image|range|color|hidden)$/.test(ty)) return '';" +
  "if (t === 'TEXTAREA' || t === 'IFRAME' || a.isContentEditable === true) return '';" +
  "var role = a.getAttribute('role');" +
  "return t.toLowerCase() + (ty ? '[type=' + ty + ']' : role ? '[role=' + role + ']' : '');" +
  "})()";

/// input/service.rs focus_elsewhere(): null when a field has focus (or the page could
/// not say), else what has it, after waiting up to TOOLS_FOCUS_WAIT_MS for it to land.
async function toolsFocusElsewhere(tabId) {
  const deadline = performance.now() + TOOLS_FOCUS_WAIT_MS;
  for (;;) {
    let holder = null;
    try {
      const r = await toolsCdp(tabId, "Runtime.evaluate", { expression: TOOLS_FOCUS_HOLDER, returnByValue: true });
      holder = r && r.result && typeof r.result.value === "string" ? r.result.value : null;
    } catch (e) {
      holder = null;
    }
    if (!holder) return null;
    if (performance.now() >= deadline) return holder;
    await toolsSleep(30);
  }
}

/// input/service.rs key(): one key down + up; `text` is what the key inserts.
async function toolsKey(tabId, key, vk, text) {
  for (const kind of ["keyDown", "keyUp"]) {
    const p = { type: kind, key, code: key, windowsVirtualKeyCode: vk, nativeVirtualKeyCode: vk };
    if (kind === "keyDown" && text) p.text = text;
    await toolsCdp(tabId, "Input.dispatchKeyEvent", p);
  }
}

/// input/service.rs type_into_rect(): focus the field with a click (twice for a box that
/// only wakes up on the first), select all through Chrome's editing channel, replace the
/// selection in one insertText (a Backspace when there is nothing to type), Enter when asked.
async function toolsTypeInto(tabId, rect, text, enter) {
  await toolsPress(tabId, rect, 0, 1);
  if ((await toolsFocusElsewhere(tabId)) !== null) {
    await toolsPress(tabId, rect, 0, 1);
    const what = await toolsFocusElsewhere(tabId);
    if (what !== null) {
      throw new Error(`the field did not take focus, nothing typed: focus is on ${what}. ` +
        "Something may be covering it, or it is not a text field");
    }
  }
  await toolsCdp(tabId, "Input.dispatchKeyEvent", {
    type: "keyDown", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, nativeVirtualKeyCode: 65, commands: ["selectAll"],
  });
  await toolsCdp(tabId, "Input.dispatchKeyEvent", {
    type: "keyUp", key: "a", code: "KeyA", windowsVirtualKeyCode: 65, nativeVirtualKeyCode: 65,
  });
  if (!text) await toolsKey(tabId, "Backspace", 8, "");
  else await toolsCdp(tabId, "Input.insertText", { text });
  if (enter) await toolsKey(tabId, "Enter", 13, "\r");
}

// ============================================================ keys (keyboard/service.rs)

const TOOLS_ALT = 1, TOOLS_CTRL = 2, TOOLS_META = 4, TOOLS_SHIFT = 8;

/// keyboard/service.rs NAMED: aliases, DOM key, DOM code, virtual key code, inserted text.
const TOOLS_NAMED = [
  [["esc", "escape"], "Escape", "Escape", 27, ""],
  [["enter", "return"], "Enter", "Enter", 13, "\r"],
  [["tab"], "Tab", "Tab", 9, ""],
  [["space"], " ", "Space", 32, " "],
  [["backspace"], "Backspace", "Backspace", 8, ""],
  [["delete", "del"], "Delete", "Delete", 46, ""],
  [["up"], "ArrowUp", "ArrowUp", 38, ""],
  [["down"], "ArrowDown", "ArrowDown", 40, ""],
  [["left"], "ArrowLeft", "ArrowLeft", 37, ""],
  [["right"], "ArrowRight", "ArrowRight", 39, ""],
  [["home"], "Home", "Home", 36, ""],
  [["end"], "End", "End", 35, ""],
  [["pageup", "pgup"], "PageUp", "PageUp", 33, ""],
  [["pagedown", "pgdn"], "PageDown", "PageDown", 34, ""],
  ...Array.from({ length: 12 }, (_, i) => [[`f${i + 1}`], `F${i + 1}`, `F${i + 1}`, 112 + i, ""]),
];
/// keyboard/service.rs PUNCTUATION: the characters on a key (plain, shifted), code, vk.
const TOOLS_PUNCTUATION = [
  ["`~", "Backquote", 192], ["-_", "Minus", 189], ["=+", "Equal", 187], ["[{", "BracketLeft", 219],
  ["]}", "BracketRight", 221], ["\\|", "Backslash", 220], [";:", "Semicolon", 186], ["'\"", "Quote", 222],
  [",<", "Comma", 188], [".>", "Period", 190], ["/?", "Slash", 191],
];
const TOOLS_SHIFTED_DIGITS = ")!@#$%^&*(";
const TOOLS_MODIFIERS = [
  [["shift"], "Shift", "ShiftLeft", 16, TOOLS_SHIFT],
  [["ctrl", "control"], "Control", "ControlLeft", 17, TOOLS_CTRL],
  [["alt", "option", "opt"], "Alt", "AltLeft", 18, TOOLS_ALT],
  [["cmd", "command", "meta", "win", "super"], "Meta", "MetaLeft", 91, TOOLS_META],
];

/// keyboard/service.rs resolve(): a name to a key {key, code, vk, text, modifier}, or null.
function toolsResolveKey(name) {
  const named = TOOLS_NAMED.find(([a]) => a.includes(name));
  if (named) return { key: named[1], code: named[2], vk: named[3], text: named[4], modifier: 0 };
  const mod = TOOLS_MODIFIERS.find(([a]) => a.includes(name));
  if (mod) return { key: mod[1], code: mod[2], vk: mod[3], text: "", modifier: mod[4] };
  const chars = [...name];
  if (chars.length !== 1) return null;
  const c = chars[0];
  if (/\s/.test(c) || (c.codePointAt(0) < 32) || c.codePointAt(0) === 127) return null;
  let code = "", vk = 0;
  if (/^[a-zA-Z]$/.test(c)) {
    code = `Key${c.toUpperCase()}`;
    vk = c.toUpperCase().charCodeAt(0);
  } else if (/^[0-9]$/.test(c)) {
    code = `Digit${c}`;
    vk = c.charCodeAt(0);
  } else if (TOOLS_SHIFTED_DIGITS.includes(c)) {
    const i = TOOLS_SHIFTED_DIGITS.indexOf(c);
    code = `Digit${i}`;
    vk = "0".charCodeAt(0) + i;
  } else {
    const p = TOOLS_PUNCTUATION.find(([cs]) => cs.includes(c));
    if (p) [, code, vk] = p;
  }
  return { key: c, code, vk, text: c, modifier: 0 };
}

/// keyboard/service.rs split(): "ctrl+shift+p" to its names; a "+" that begins a key is
/// the plus key itself.
function toolsSplitCombo(combo) {
  const keys = [];
  let cur = "";
  for (const c of combo.replace(/\s+/g, "")) {
    if (c === "+" && cur) {
      keys.push(cur);
      cur = "";
    } else {
      cur += c;
    }
  }
  if (cur) keys.push(cur);
  return keys;
}

/// keyboard/service.rs edit_command(): the editing shortcuts, by Chrome's own names.
function toolsEditCommand(key, held) {
  if ((held & (TOOLS_CTRL | TOOLS_META)) === 0 || (held & TOOLS_ALT) !== 0) return null;
  const shift = (held & TOOLS_SHIFT) !== 0;
  if (key === "a" && !shift) return "selectAll";
  if (key === "c" && !shift) return "copy";
  if (key === "v" && !shift) return "paste";
  if (key === "x" && !shift) return "cut";
  if (key === "z" && !shift) return "undo";
  if ((key === "z" && shift) || (key === "y" && !shift)) return "redo";
  return null;
}

/// keyboard/service.rs event(): one key event carrying the modifiers held at that moment.
async function toolsKeyEvent(tabId, kind, k, held) {
  const shifted = (held & TOOLS_SHIFT) !== 0 && k.key.length === 1 && /^[a-z]$/.test(k.key);
  const key = shifted ? k.key.toUpperCase() : k.key;
  const p = {
    type: kind, key, code: k.code, windowsVirtualKeyCode: k.vk, nativeVirtualKeyCode: k.vk, modifiers: held,
  };
  if (kind === "keyDown") {
    if (k.text && (held & (TOOLS_ALT | TOOLS_CTRL | TOOLS_META)) === 0) p.text = shifted ? key : k.text;
    const cmd = toolsEditCommand(k.key, held);
    if (cmd) p.commands = [cmd];
  }
  await toolsCdp(tabId, "Input.dispatchKeyEvent", p);
}

/// keyboard/service.rs chord(): hold the keys in order, let go in reverse.
async function toolsChord(tabId, keys) {
  let held = 0;
  for (const k of keys) {
    held |= k.modifier;
    await toolsKeyEvent(tabId, "keyDown", k, held);
  }
  for (const k of [...keys].reverse()) {
    held &= ~k.modifier;
    await toolsKeyEvent(tabId, "keyUp", k, held);
  }
}

/// The keys of a `keyboard` request: the Rust tool sends them resolved (`keys`); a
/// request written by hand may send the combo as the model writes it (`value`).
function toolsKeysOf(msg) {
  if (Array.isArray(msg.keys)) {
    return msg.keys.map((k) => ({
      key: String(k.key), code: String(k.code || ""), vk: Number(k.vk) || 0, text: String(k.text || ""),
      modifier: Number(k.modifier) || 0,
    }));
  }
  const combo = String(msg.value || "").trim().toLowerCase();
  const names = toolsSplitCombo(combo);
  if (!names.length) throw new Error("keyboard needs a key in `value` - e.g. \"esc\", \"enter\" or \"ctrl+a\"");
  if (names.length > 3) throw new Error(`'${combo}' has ${names.length} keys - a shortcut is at most 3, joined with '+'`);
  return names.map((n) => {
    const k = toolsResolveKey(n);
    if (!k) throw new Error(`unknown key '${n}' in '${combo}'`);
    return k;
  });
}

// ============================================================ scrolling (scroll/service.rs)

/// scroll/service.rs wheel(): move the pointer over the centre of `rect`, then turn the
/// wheel by (dx, dy) CSS px; the browser decides which surface takes it.
async function toolsWheel(tabId, rect, dx, dy) {
  const [x, y] = toolsCentre(rect);
  await toolsCdp(tabId, "Input.dispatchMouseEvent", { type: "mouseMoved", x, y, buttons: 0 });
  await toolsCdp(tabId, "Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: dx, deltaY: dy, buttons: 0 });
}

/// Python's round(): halves go to the even neighbour (browser.rs py_round).
function toolsPyRound(v) {
  const f = Math.floor(v);
  const d = v - f;
  if (d > 0.5) return f + 1;
  if (d < 0.5) return f;
  return f % 2 === 0 ? f : f + 1;
}

/// browser.rs scroll_probe(): where the surfaces under the centre of `rect` sit, as
/// [innerX, innerY, pageX, pageY] from the overlay, else the page's own offsets from
/// Page.getLayoutMetrics; null when neither can be read.
async function toolsScrollProbe(tabId, rect) {
  const [x, y] = toolsCentre(rect);
  try {
    const v = await toolsRunInPage(
      tabId,
      (px, py) => (window.__AutoCuaScrollProbe ? window.__AutoCuaScrollProbe(px, py) : null),
      [Math.round(x * 10) / 10, Math.round(y * 10) / 10],
    );
    if (Array.isArray(v) && v.length === 4) return v;
  } catch (e) {
    // no overlay, or a page the extension cannot script: the layout metrics below
  }
  try {
    const m = await toolsCdp(tabId, "Page.getLayoutMetrics");
    const vp = (m && (m.cssVisualViewport || m.visualViewport)) || {};
    return [-1, -1, toolsPyRound(Number(vp.pageX) || 0), toolsPyRound(Number(vp.pageY) || 0)];
  } catch (e) {
    return null;
  }
}

// ============================================================ scripts (run_script/service.rs)

const TOOLS_MAX_RESULT_CHARS = 30000;
const TOOLS_SCRIPT_TIMEOUT_MS = 10000;

/// run_script/service.rs TO_DATA, verbatim: turns whatever the script produced into data,
/// in the page.
const TOOLS_TO_DATA = `function () {
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
}`;

/// run_script/service.rs expression_form(): the code as a bare expression, trailing
/// comment lines and a comment after the final semicolon dropped, then the semicolon.
function toolsExpressionForm(code) {
  let s = code.trim();
  for (;;) {
    const t = s.trimEnd();
    const i = t.lastIndexOf("\n");
    if (i >= 0 && t.slice(i + 1).trimStart().startsWith("//")) s = t.slice(0, i);
    else break;
  }
  const i = s.lastIndexOf("//");
  if (i >= 0) {
    const head = s.slice(0, i).trimEnd();
    const tail = s.slice(i);
    if (head.endsWith(";") && !tail.includes("\n") && !/['"`]/.test(tail)) s = head;
  }
  return s.trim().replace(/;+$/, "").trim();
}

/// run_script/service.rs is_expression(): parse with `new Function`, run nothing.
async function toolsIsExpression(tabId, expression) {
  const literal = JSON.stringify(`(\n${expression}\n)`);
  const reply = await toolsCdp(tabId, "Runtime.evaluate", {
    expression: `(new Function(${literal}), true)`, returnByValue: true,
  });
  return !(reply && reply.exceptionDetails);
}

/// run_script/service.rs evaluate(): the code once, in the tab, raced against the page's
/// own timer; the reply in Chrome's shape (exceptionDetails, or result with the object
/// result already turned into data), for the Rust tool to read as it reads CDP's.
async function toolsEvaluate(tabId, code) {
  const expression = toolsExpressionForm(code);
  const body = (await toolsIsExpression(tabId, expression)) ? `(\n${expression}\n)` : `(async () => {\n${code}\n})()`;
  const secs = TOOLS_SCRIPT_TIMEOUT_MS / 1000;
  const raced = `Promise.race([${body}, new Promise((_, reject) => setTimeout(() => ` +
    `reject(new Error('stopped after ${secs}s without finishing - work that never settles')), ` +
    `${TOOLS_SCRIPT_TIMEOUT_MS}))])`;
  let reply;
  try {
    reply = await toolsCdp(tabId, "Runtime.evaluate", {
      expression: raced, returnByValue: false, awaitPromise: true, userGesture: true, timeout: TOOLS_SCRIPT_TIMEOUT_MS,
    }, TOOLS_SCRIPT_TIMEOUT_MS + 5000);
  } catch (e) {
    const m = toolsErr(e);
    if (m.includes("Internal error") || m.includes("timed out")) {
      await toolsCdp(tabId, "Runtime.terminateExecution").catch(() => {});
      throw new Error(`stopped after ${secs}s without finishing - an endless loop`);
    }
    throw e;
  }
  if (reply && reply.exceptionDetails) return reply;
  const oid = reply && reply.result && reply.result.objectId;
  if (oid) {
    const data = await toolsCdp(tabId, "Runtime.callFunctionOn", {
      objectId: oid,
      functionDeclaration: TOOLS_TO_DATA.replaceAll("__MAX__", String(TOOLS_MAX_RESULT_CHARS)),
      returnByValue: true,
    }, 15000);
    await toolsCdp(tabId, "Runtime.releaseObject", { objectId: oid }).catch(() => {});
    return data;
  }
  return reply;
}

// ============================================================ navigation (tab/service.rs)

/// A page the extension can neither script nor attach to: Chrome's own pages, ours, and
/// the Chrome Web Store.
function toolsUnreachable(url) {
  return /^(chrome|chrome-extension|chrome-untrusted|devtools|edge|brave|view-source):/.test(url || "") ||
    toolsWebStore(url);
}

/// browser.rs web_store_url(): the Chrome Web Store, at either of its addresses. The
/// agent may not use it (the tab tools refuse it, a scan shows it as blocked), and no
/// extension may read it or put the debugger on it anyway.
function toolsWebStore(url) {
  let u;
  try {
    u = new URL(url || "");
  } catch (e) {
    return false;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return false;
  const host = u.hostname.replace(/\.$/, "").toLowerCase();
  return host === "chromewebstore.google.com" || (host === "chrome.google.com" && /^\/webstore(\/|$)/.test(u.pathname));
}

/// browser.rs WEB_STORE_BLOCKED, verbatim.
const TOOLS_WEB_STORE = "the Chrome Web Store is not allowed: the agent may not open it or install anything from it";

/// Chrome's New Tab page, which browser.rs's blank_url counts as an empty surface.
function toolsNewTabPage(url) {
  return /^(chrome:\/\/newtab\/?|chrome:\/\/new-tab-page\/?|edge:\/\/newtab\/?)$/.test((url || "").trim());
}

/// Chrome refuses to run an extension's script in this page (about:blank, an error page),
/// though the debugger can still reach it.
function toolsScriptingRefused(e) {
  return /Cannot access|error page/i.test(toolsErr(e));
}

/// The tab's next finished load, by Chrome's own tab status: armed before the navigation
/// is sent, so a page that was complete already does not answer for the new one.
function toolsNextLoad(tabId, timeoutMs = TOOLS_LOAD_MS) {
  let done = null;
  const promise = new Promise((resolve) => {
    let timer = null;
    const listener = (id, info) => {
      if (id === tabId && info.status === "complete") done(true);
    };
    done = (ok) => {
      chrome.tabs.onUpdated.removeListener(listener);
      clearTimeout(timer);
      resolve(ok);
    };
    chrome.tabs.onUpdated.addListener(listener);
    timer = setTimeout(() => done(false), timeoutMs);
  });
  return { promise, cancel: () => done(false) };
}

/// Wait until a tab stops loading, by Chrome's own tab status (the load wait for a tab
/// the debugger is not on, and for switch_tab's mid-load tab: browser.rs is_loading).
function toolsWaitStatus(tabId, timeoutMs = TOOLS_LOAD_MS) {
  return new Promise((resolve) => {
    let timer = null;
    const listener = (id, info) => {
      if (id === tabId && info.status === "complete") done(true);
    };
    const done = (ok) => {
      chrome.tabs.onUpdated.removeListener(listener);
      clearTimeout(timer);
      resolve(ok);
    };
    chrome.tabs.onUpdated.addListener(listener);
    timer = setTimeout(() => done(false), timeoutMs);
    chrome.tabs.get(tabId).then((t) => {
      if (t.status === "complete") done(true);
    }, () => done(false));
  });
}

/// tab/service.rs navigate_to(): Page.navigate, a failure read off errorText (Chrome
/// renders its own error page and fires a load event, so the wait alone would call a dead
/// site a success), then the load event. A tab on one of Chrome's own pages cannot take
/// the debugger, so it is sent on its way through the tabs API instead.
async function toolsNavigate(tabId, url) {
  toolsReopenedTabs.delete(tabId);
  const tab = await chrome.tabs.get(tabId);
  if (toolsUnreachable(tab.url || tab.pendingUrl) || toolsUnreachable(url)) {
    const load = toolsNextLoad(tabId);
    try {
      await chrome.tabs.update(tabId, { url });
    } catch (e) {
      load.cancel();
      throw new Error(`could not load ${url}: ${toolsErr(e)}`);
    }
    await load.promise;
    await toolsReattach(tabId);
    return;
  }
  const load = toolsLoadWaiter(tabId);
  let reply;
  try {
    reply = await toolsCdp(tabId, "Page.navigate", { url }, 10000);
  } catch (e) {
    load.cancel();
    throw e;
  }
  if (reply && reply.errorText) {
    load.cancel();
    throw new Error(`could not load ${url}: ${reply.errorText}`);
  }
  await load.promise;
}

/// After the tabs API moved a tab off a page the debugger cannot reach (one of Chrome's
/// pages, the Web Store), the debugger goes back on the page it reached, as toolsNewTab
/// and toolsActivate put it on, so that page's popups are heard and its freezes watched.
async function toolsReattach(tabId) {
  const tab = await chrome.tabs.get(tabId).catch(() => null);
  if (tab && !toolsUnreachable(tab.url || tab.pendingUrl || "")) await toolsAttach(tabId).catch(() => {});
}

/// tab/service.rs reload_tab().
async function toolsReload(tabId) {
  const tab = await chrome.tabs.get(tabId);
  if (toolsUnreachable(tab.url || tab.pendingUrl)) {
    const load = toolsNextLoad(tabId);
    try {
      await chrome.tabs.reload(tabId);
    } catch (e) {
      load.cancel();
      throw e;
    }
    await load.promise;
    await toolsReattach(tabId);
    return;
  }
  const load = toolsLoadWaiter(tabId);
  try {
    await toolsCdp(tabId, "Page.reload", {}, 10000);
  } catch (e) {
    load.cancel();
    throw e;
  }
  await load.promise;
}

/// tab/service.rs step_history(): -1 is back, +1 is forward, refused at the ends.
async function toolsHistory(tabId, delta) {
  const tab = await chrome.tabs.get(tabId);
  if (toolsUnreachable(tab.url || tab.pendingUrl)) {
    // A Chrome page takes no debugger: the tabs API steps the history instead.
    // Chrome refuses it from its own pages (measured on chrome://settings with a web
    // page behind it: "Cannot find a next page in history"), so say what is known.
    const load = toolsNextLoad(tabId);
    try {
      await (delta < 0 ? chrome.tabs.goBack(tabId) : chrome.tabs.goForward(tabId));
    } catch (e) {
      load.cancel();
      throw new Error(`${tab.url || tab.pendingUrl} is a Chrome page, where an extension cannot go ` +
        `${delta < 0 ? "back" : "forward"}: go to a web page with update_tab or new_tab`);
    }
    await load.promise;
    await toolsReattach(tabId);
    return;
  }
  const unreadable = () => new Error("could not read the tab's history");
  const h = await toolsCdp(tabId, "Page.getNavigationHistory");
  const cur = h && Number.isInteger(h.currentIndex) ? h.currentIndex : null;
  const entries = h && Array.isArray(h.entries) ? h.entries : null;
  if (cur === null || !entries) throw unreadable();
  const want = cur + delta;
  if (want < 0 || want >= entries.length) {
    throw new Error(`no page to go ${delta < 0 ? "back" : "forward"} to - this tab is at the ` +
      `${delta < 0 ? "start" : "end"} of its history`);
  }
  const id = entries[want] && entries[want].id;
  if (!Number.isInteger(id)) throw new Error("history entry has no id");
  const load = toolsLoadWaiter(tabId);
  try {
    await toolsCdp(tabId, "Page.navigateToHistoryEntry", { entryId: id }, 10000);
  } catch (e) {
    load.cancel();
    throw e;
  }
  await load.promise;
}

// ============================================================ tabs

/// The window a new tab goes in: the one in front, else the first normal window, else a
/// new one. Named on every chrome.tabs.create: from a worker Chrome falls back to its
/// "current window", and a Chrome left running with every window closed (macOS) has none
/// (measured: "No current window", which ended a run on its first tab). Right after a
/// launch the window can take a moment, so it is waited for before one is made.
async function toolsWindowId() {
  for (let i = 0; i < 12; i++) {
    const all = await chrome.windows.getAll({ windowTypes: ["normal"] }).catch(() => []);
    const front = all.find((w) => w.focused) || all[0];
    if (front) return front.id;
    await toolsSleep(250);
  }
  const made = await chrome.windows.create({ url: "about:blank", focused: true, type: "normal" });
  return made.id;
}

/// browser.rs open_tab_impl(): a tab in the background, so the browser stays where the
/// person left it. The caller shows it (activate) and navigates it.
async function toolsNewTab(url, line) {
  const windowId = await toolsWindowId();
  const tab = await chrome.tabs.create({ url: url || "about:blank", active: false, windowId });
  toolsOurs.set(tab.id, line || null);
  if (!toolsUnreachable(tab.url || tab.pendingUrl || url)) await toolsAttach(tab.id).catch(() => {});
  return { id: tab.id, windowId: tab.windowId, index: tab.index };
}

/// browser.rs show_tab(): put the tab on show in its window, without bringing the window
/// forward; the debugger goes on it now, so dialogs are heard from the start.
async function toolsActivate(tabId, waitLoad, line) {
  const tab = await chrome.tabs.update(tabId, { active: true });
  toolsOurs.set(tabId, line || toolsOurs.get(tabId) || null);
  if (!toolsUnreachable(tab.url || tab.pendingUrl)) await toolsAttach(tabId).catch(() => {});
  if (waitLoad && tab.status === "loading") await toolsWaitStatus(tabId);
  return { id: tab.id, url: tab.url, status: tab.status };
}

/// browser.rs close_target(): close one tab and say whether it left.
async function toolsCloseTab(tabId) {
  const all = await chrome.tabs.query({});
  if (all.length <= 1) throw new Error("cannot close the last tab - navigate it instead");
  // The debugger stays on through the close, so a "Leave site?" the page asks for is
  // answered Leave like any other (onEvent); the tab's state goes with it (onRemoved).
  // Chrome answers the removal only once the tab is gone, so it is not waited on past
  // the check below, which always replies.
  toolsClosing.add(tabId);
  try {
    chrome.tabs.remove(tabId).catch(() => {});
    for (let i = 0; i < 20; i++) {
      const gone = await chrome.tabs.get(tabId).then(() => false, () => true);
      if (gone) return { closed: true };
      await toolsSleep(100);
    }
    return { closed: false };
  } finally {
    toolsClosing.delete(tabId);
  }
}

/// browser.rs show_blank_page(): paint the logo into a tab that is really blank, over
/// Page.setDocumentContent, so the address bar stays about:blank.
async function toolsBlankPage(tabId, html) {
  // Chrome's New Tab page takes no extension: the tab goes to about:blank first, the same
  // empty surface for browser.rs, and the logo is painted there.
  const tab = await chrome.tabs.get(tabId);
  if (toolsNewTabPage(tab.url || tab.pendingUrl)) {
    const load = toolsNextLoad(tabId, 10000);
    await chrome.tabs.update(tabId, { url: "about:blank" });
    await load.promise;
  }
  const tree = await toolsCdp(tabId, "Page.getFrameTree");
  const frame = (tree && tree.frameTree && tree.frameTree.frame) || {};
  if (!frame.id) throw new Error("no frame to render into");
  const blank = /^(about:blank|chrome:\/\/newtab\/?|chrome:\/\/new-tab-page\/?|edge:\/\/newtab\/?)?$/.test((frame.url || "").trim());
  if (!frame.url || !blank) return { painted: false };
  await toolsCdp(tabId, "Page.setDocumentContent", { frameId: frame.id, html });
  return { painted: true };
}

// ============================================================ pages the extension may not script

/// Run `func(...args)` in the page: in the extension's world by chrome.scripting, or,
/// on a page the extension may not script (about:blank, the agent's logo page) but
/// the debugger can reach, by Runtime.evaluate in the page's own world. The overlay
/// on such a page is installed the same way (toolsGlow), so its entry points are
/// found where this looks.
async function toolsRunInPage(tabId, func, args, timeoutMs = TOOLS_RPC_MS) {
  if (toolsDialogs.has(tabId)) throw new Error(TOOLS_DIALOG_OPEN);
  try {
    // Now, in whatever document is there, like Runtime.evaluate: by default Chrome holds
    // an injection until the document is idle, which made the settle after a click that
    // loads a page wait for that page (measured: +1.2 s). Never into a page a popup holds.
    const ran = chrome.scripting.executeScript({ target: { tabId }, func, args, injectImmediately: true });
    const [r] = await toolsUnlessDialog(tabId, ran, () => { throw new Error(TOOLS_DIALOG_OPEN); });
    return r ? r.result : undefined;
  } catch (e) {
    if (!toolsScriptingRefused(e)) throw e;
  }
  const call = `(${func.toString()})(${args.map((a) => JSON.stringify(a === undefined ? null : a)).join(",")})`;
  const r = await toolsCdp(tabId, "Runtime.evaluate", { expression: call, awaitPromise: true, returnByValue: true }, timeoutMs);
  if (r && r.exceptionDetails) {
    const ex = r.exceptionDetails;
    throw new Error((ex.exception && ex.exception.description) || ex.text || "the page threw");
  }
  return r && r.result ? r.result.value : undefined;
}

/// A scan of one of Chrome's own pages, which no extension may read: the page as [1] and
/// a line saying so, so the agent can still go on to a web page (debug mode reads these).
function toolsChromePageScan(tab) {
  const url = tab.url || tab.pendingUrl || "";
  const w = tab.width || 0;
  const h = tab.height || 0;
  return {
    tree: "<element>\n  [1] <page scrollable>the whole page</page>\n" +
      `    ${url} is a Chrome page, which an extension cannot read or act on: go to a web page with update_tab or new_tab\n` +
      "</element>",
    summary: "1 interactive, 0 ms settle, 1 sessions, 0 frames skipped, 0 occluded, 0 noise",
    count: 1, sessions: 1, skipped: 0, occluded: 0, noise: 0, url, settled_ms: 0,
    hits: [[1, [0, 0, w, h]]], dpr: 1, screenshot: null, screenshot_plain: null,
    timings: { settle_ms: 0, scan_ms: 0, page_ms: 0, frames: 0, shot_ms: 0, total_ms: 0 },
  };
}

/// element.js elementScan() falls back on these for such a page: the page script run
/// through the debugger in the top frame, and Chrome's own screenshot of the tab.
const toolsScanFallback = {
  async read(tab, cfg, scan) {
    const value = await toolsRunInPage(tab.id, elementScanPage, [cfg, scan], 20000);
    return [{ frameId: 0, result: value }];
  },
  async capture(tab) {
    const r = await toolsCdp(tab.id, "Page.captureScreenshot", { format: "jpeg", quality: 75, captureBeyondViewport: false }, 10000);
    return `data:image/jpeg;base64,${r.data}`;
  },
};

// ============================================================ the page after an action

/// controller/service.rs settle_after_action(), the page side: one animation frame, then
/// ACTION_QUIET_MS with no DOM mutation outside the overlay and no finite animation or
/// transition running, capped at ACTION_SETTLE_CAP_MS; answers "quiet 96ms" / "cap 600ms".
function toolsSettlePage(QUIET, CAP) {
  return new Promise(function (done) {
    var t0 = performance.now(), last = t0, timer = null, mo = null, over = false;
    var layer = document.querySelector('[data-AutoCua="overlay"]');
    function finish(why) {
      if (over) return;
      over = true;
      if (mo) mo.disconnect();
      clearTimeout(timer);
      done(why + " " + Math.round(performance.now() - t0) + "ms");
    }
    try {
      mo = new MutationObserver(function (records) {
        for (var i = 0; i < records.length; i++) {
          if (!layer || !layer.contains(records[i].target)) { last = performance.now(); return; }
        }
      });
      mo.observe(document.documentElement, { childList: true, subtree: true, attributes: true, characterData: true });
    } catch (e) {}
    function animating() {
      try {
        var all = document.getAnimations ? document.getAnimations() : [];
        for (var i = 0; i < all.length; i++) {
          var a = all[i];
          if (a.playState !== "running") continue;
          var target = a.effect && a.effect.target;
          if (target && layer && layer.contains(target)) continue;
          var timing = a.effect && a.effect.getTiming ? a.effect.getTiming() : null;
          if (timing && timing.iterations === Infinity) continue;
          return true;
        }
      } catch (e) {}
      return false;
    }
    function check() {
      var now = performance.now();
      if (now - t0 >= CAP) return finish("cap");
      if (animating()) { last = now; timer = setTimeout(check, 30); return; }
      if (now - last >= QUIET) return finish("quiet");
      timer = setTimeout(check, Math.max(10, QUIET - (now - last)));
    }
    requestAnimationFrame(function () { last = Math.max(last, performance.now()); check(); });
    setTimeout(function () { finish("cap"); }, CAP + 50);
  });
}

const TOOLS_ACTION_QUIET_MS = 80;
const TOOLS_ACTION_SETTLE_CAP_MS = 600;

async function toolsSettle(tabId) {
  // Through the debugger, as controller/service.rs settle_after_action runs it over CDP:
  // a page that navigates away mid-wait ends the wait at once (its context is gone),
  // where a script injected into it was left waiting out the timer (measured: +1.3 s on
  // every click that loads a page). Any failure is "not settled", as there.
  const expression = `(${toolsSettlePage.toString()})(${TOOLS_ACTION_QUIET_MS}, ${TOOLS_ACTION_SETTLE_CAP_MS})`;
  try {
    const r = await toolsCdp(tabId, "Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true },
      TOOLS_ACTION_SETTLE_CAP_MS + 900);
    return r && r.result && typeof r.result.value === "string" ? r.result.value : null;
  } catch (e) {
    return null;
  }
}

// ============================================================ a tab that froze or crashed

/// How long one of the agent's tabs may go without answering a cheap request (the glow,
/// sent as each scan starts) before it counts as frozen (the owner's rule): it is then
/// closed and its page opened again in its place.
const TOOLS_FROZEN_MS = 5000;
/// How long a scan's read may run, once the page has started on it, before its tab is
/// asked whether it answers at all (toolsWatchScan). Long, like browser.rs's scan calls
/// (RPC_TIMEOUT): a question sent sooner waits behind a heavy page's own long read, and a
/// healthy page would look frozen. A page that never started on the read, or froze once
/// the read was back, is caught long before this (toolsWatchScan's ahead and back).
const TOOLS_SCAN_STUCK_MS = 30000;
/// How long a tab suspected of being frozen gets to answer a request with nothing to do
/// (toolsAnswers).
const TOOLS_PROBE_MS = 1000;
/// The agents' own tabs, each with the line (background.js) of the agent that drives it:
/// opened by it (new_tab), put on show by it (activate: its first tab and switch_tab), or
/// opened in place of one (toolsReopen). Only these are ever closed for freezing or
/// crashing; the person's tabs never are, even when the debugger went on one for some
/// other call. Several agents at once (a parallel run) each own their tabs here.
const toolsOurs = new Map();
/// Tabs of the person's whose glow was asked for and has not come back yet: a page that
/// stays frozen is asked once, not again on every step.
const toolsDressing = new Set();
/// Every glow request still out (toolsGlow), so letting go can wait for them before it
/// takes the glow off: one that landed after that put the glow back (measured).
const toolsDressings = new Set();
/// Tabs closed and opened again, or being: each is replaced once, however many
/// requests found it frozen.
const toolsReplaced = new Set();
/// tabId -> the reopen under way for it (toolsAwaitReopen).
const toolsReopens = new Map();
/// Pages opened again once already in this run, and the tabs opened for them: one that
/// freezes or crashes again gets a blank tab instead (as browser.rs recover_tab), even
/// when its page has moved to another address since. A tab the agent sends somewhere
/// else is a fresh start (toolsNavigate).
const toolsReopenedUrls = new Set();
const toolsReopenedTabs = new Set();
/// The lines letting go right now (toolsLetGo): nothing is opened again for an agent that
/// is gone.
const toolsLettingGo = new Set();
/// background.js: tells the agent side which tab took the place of one closed here, so
/// it drives the new one (bridge.rs reopened).
let toolsOnReopened = null;

/// Close a tab at once, the way debug mode's /json/close does: Target.closeTarget asks
/// no "Leave site?", which a frozen page could never answer (chrome.tabs.remove waits on
/// it for good while the debugger is on the tab, measured).
async function toolsForceClose(tabId) {
  const target = (await chrome.debugger.getTargets()).find((t) => t.tabId === tabId);
  if (target && toolsAttached.has(tabId)) {
    try {
      await chrome.debugger.sendCommand({ tabId }, "Target.closeTarget", { targetId: target.id });
      return;
    } catch (e) {
      // the tabs API below
    }
  }
  await chrome.tabs.remove(tabId);
}

/// What the model is told when a tab was closed and opened again (browser.rs
/// reopen_text says the same).
function toolsReopenText(why, url, reopened, again) {
  const page = url ? `the page on ${url}` : "the page";
  const what = why === "crashed" ? "crashed (its renderer died)" :
    `stopped answering for ${TOOLS_FROZEN_MS / 1000} s (it froze)`;
  if (reopened) {
    return `${page} ${what}, so its tab was closed and the same page opened again in a new tab in its ` +
      "place: anything typed into it and not submitted is gone, and so is its back and forward history";
  }
  return `${page} ${what}${again ? " again after it was opened again" : ""}, so its tab was closed and ` +
    "a blank tab opened in its place";
}

/// One of the agent's tabs froze or crashed: close it, open the same page in a new tab in
/// the same place (its back and forward history does not come back), tell the agent side
/// which tab took its place, and tell the model. Anything but a web page, and a page or
/// tab opened again once already, gets a blank tab instead. Never the person's tab, and
/// nothing once the agent is gone or the person took the browser back.
function toolsReopen(tabId, why) {
  if (!toolsOurs.has(tabId) || toolsReplaced.has(tabId) || toolsLettingGo.has(toolsOurs.get(tabId)) || toolsStoppedByPerson) {
    return Promise.resolve();
  }
  toolsReplaced.add(tabId);
  const work = (async () => {
    const tab = await chrome.tabs.get(tabId).catch(() => null);
    if (!tab || toolsLettingGo.has(toolsOurs.get(tabId))) return;
    const url = tab.url || tab.pendingUrl || "";
    const again = toolsReopenedUrls.has(url) || toolsReopenedTabs.has(tabId);
    const reopen = /^(https?|file):/i.test(url) && !again;
    if (reopen) toolsReopenedUrls.add(url);
    // The new tab first, right of the old one, which it then replaces: closing the
    // window's last tab first would close the window, and on Windows and Linux the last
    // window takes Chrome with it.
    const fresh = await chrome.tabs.create({
      url: reopen ? url : "about:blank", index: tab.index + 1, windowId: tab.windowId, active: tab.active,
    });
    toolsOurs.set(fresh.id, toolsOurs.get(tabId) || null);
    if (reopen) toolsReopenedTabs.add(fresh.id);
    // Told before the old tab goes, so the agent side knows where to look by the time a
    // call cut short by the close comes back.
    if (toolsOnReopened) toolsOnReopened(tabId, fresh.id);
    await toolsForceClose(tabId).catch(() => {});
    await toolsAttach(fresh.id).catch(() => {});
    toolsNotice(toolsReopenText(why, url, reopen, again), fresh.id);
  })().finally(() => toolsReopens.delete(tabId));
  toolsReopens.set(tabId, work);
  return work;
}

/// A call into one of the agent's tabs failed (a scan). When the tab crashed or froze
/// and is being opened again (Chrome can report the crash a moment after the call
/// failed), wait for that first, so the agent side hears which tab took its place before
/// it hears of the failure, and follows it rather than giving up.
async function toolsAwaitReopen(tabId, ms = 1000) {
  if (!toolsOurs.has(tabId) && !toolsReopens.has(tabId)) return;
  const until = Date.now() + ms;
  while (!toolsReopens.has(tabId) && !toolsReplaced.has(tabId) && Date.now() < until) await toolsSleep(50);
  const work = toolsReopens.get(tabId);
  if (work) await work.catch(() => {});
}

/// Whether a tab answers a request with nothing to do within `ms`. A page the extension
/// may not script is asked through the debugger; a tab that is gone, or a page that
/// refuses for any other reason, is not frozen.
async function toolsAnswers(tabId, ms = TOOLS_PROBE_MS) {
  const asked = chrome.scripting.executeScript({ target: { tabId }, func: () => 1, injectImmediately: true })
    .then(() => true, (e) => !toolsScriptingRefused(e) ||
      toolsCdp(tabId, "Runtime.evaluate", { expression: "1" }, ms).then(() => true, () => false));
  return Promise.race([asked, toolsSleep(ms).then(() => false)]);
}

/// Watch a scan of one of the agent's tabs for a page that froze under it, by the owner's
/// rule (silent for TOOLS_FROZEN_MS: frozen), at the two points where the scan hands the
/// page work. `ahead()`, called by element.js elementScan right before its read goes out,
/// sends a cheap question ahead of the read, as browser.rs rpc does ahead of each watched
/// call: the page answers it before it starts on the read, so the read's own length (a
/// heavy page) cannot hold the answer up, and a page that has not answered after
/// TOOLS_FROZEN_MS + TOOLS_PROBE_MS could not start on anything: it froze. `back()`,
/// called when the read is back and only the picture is still out, gives the picture
/// TOOLS_FROZEN_MS: a page that has not sent it by then, and does not answer a question
/// either (toolsAnswers), froze under the picture (a question asked the moment the read
/// was back caught nothing: measured, the page answered it and froze right after). Behind
/// both, a read still out after TOOLS_SCAN_STUCK_MS on a page that does not answer froze
/// inside the read (in a wait for one of its frames). A frozen page is opened again
/// (toolsReopen), which ends the scan. Nothing here is waited on, and a page held by a
/// popup is not frozen. Measured before this: a page that froze just after its glow was
/// answered held the scan for the whole 30 s, and one that froze under the picture 15 s.
function toolsWatchScan(tabId) {
  if (!toolsOurs.has(tabId)) return { ahead() {}, back() {}, stop() {} };
  let timer = null;
  let stopped = false;
  const arm = (ms) => {
    clearTimeout(timer);
    if (stopped) return;
    timer = setTimeout(async () => {
      if (toolsDialogs.has(tabId) || (await toolsAnswers(tabId))) return;
      if (!toolsDialogs.has(tabId)) toolsReopen(tabId, "froze").catch(() => {});
    }, ms);
  };
  arm(TOOLS_SCAN_STUCK_MS);
  return {
    ahead() {
      toolsAnswers(tabId, TOOLS_FROZEN_MS + TOOLS_PROBE_MS).then((answered) => {
        if (!answered && !toolsDialogs.has(tabId)) toolsReopen(tabId, "froze").catch(() => {});
      });
    },
    back: () => arm(TOOLS_FROZEN_MS),
    stop: () => {
      stopped = true;
      clearTimeout(timer);
    },
  };
}

// ============================================================ a scan reads a loaded page

/// How long one scan may spend waiting for its page and following the tab to new pages:
/// under the agent side's 40 s for the whole scan (browser.rs), so it answers in time.
const TOOLS_SCAN_BUDGET_MS = 30000;
/// How long a scan waits for one page to load before it reads what is there anyway.
const TOOLS_SCAN_LOAD_MS = 10000;
const TOOLS_PAGE_LEFT = Symbol("page left");

/// The owner's rule for a scan: read the page once it has loaded, never across a page
/// change. A tab with a new page on its way (a click that submitted a form) is waited
/// for, and a tab that moves on to a new page during the read is read again once that
/// page has loaded. The read of the page that went away is let go, not waited for:
/// Chrome keeps that page in its back/forward cache, frozen, and a frame of it caught
/// mid-read (a cross-site ad frame still waiting for its parent) never answers, so the
/// read never came back and the agent gave up after 40 s (measured: 4 of 8 such scans
/// hung, 0 of 12 with that cache off, 0 of 12 with no cross-site frames). A page that
/// stops answering is toolsWatchScan's (5 s, then opened again).
async function toolsScanLoaded(tabId, scan) {
  const until = Date.now() + TOOLS_SCAN_BUDGET_MS;
  for (;;) {
    await toolsPageLoaded(tabId, until);
    const left = toolsPageLeaves(tabId);
    const read = scan(await chrome.tabs.get(tabId));
    let outcome;
    try {
      outcome = await Promise.race([read, left.promise]);
    } catch (e) {
      if (!left.happened) throw e;
      outcome = TOOLS_PAGE_LEFT; // the read failed because its page went away
    } finally {
      left.stop();
    }
    if (outcome !== TOOLS_PAGE_LEFT) return outcome;
    read.catch(() => {}); // it ends, if ever, only when that page comes back
    if (Date.now() >= until) throw new Error("scan failed: the page kept moving on to new pages");
  }
}

/// Until the tab has its page: none on its way (Chrome's pendingUrl) and the page's
/// document parsed (past readyState "loading"). After TOOLS_SCAN_LOAD_MS the scan reads
/// what is there; its settle waits out the rest of the loading.
async function toolsPageLoaded(tabId, until) {
  const stop = Math.min(until, Date.now() + TOOLS_SCAN_LOAD_MS);
  while (Date.now() < stop) {
    const tab = await chrome.tabs.get(tabId);
    if (!tab.pendingUrl && (await toolsDocumentParsed(tabId))) return;
    await toolsSleep(100);
  }
}

/// Whether the tab's page has parsed its document. A page that does not answer within
/// TOOLS_PROBE_MS, or one the extension may not script, counts as parsed: the scan has
/// its own ways with both (toolsWatchScan, the debugger read).
function toolsDocumentParsed(tabId) {
  const asked = chrome.scripting
    .executeScript({ target: { tabId }, injectImmediately: true, func: () => document.readyState })
    .then((r) => !(r && r[0] && r[0].result === "loading"), () => true);
  return Promise.race([asked, toolsSleep(TOOLS_PROBE_MS).then(() => true)]);
}

/// Resolves with TOOLS_PAGE_LEFT once a new page starts or commits in the tab: its own
/// main frame navigating, not a frame of the page loading something (an ad) and not the
/// page changing its own address (history.pushState).
function toolsPageLeaves(tabId) {
  const out = { happened: false };
  let seen = null;
  out.promise = new Promise((resolve) => {
    seen = (d) => {
      if (d.tabId !== tabId || d.frameId !== 0) return;
      out.happened = true;
      resolve(TOOLS_PAGE_LEFT);
    };
    chrome.webNavigation.onBeforeNavigate.addListener(seen);
    chrome.webNavigation.onCommitted.addListener(seen);
  });
  out.stop = () => {
    chrome.webNavigation.onBeforeNavigate.removeListener(seen);
    chrome.webNavigation.onCommitted.removeListener(seen);
  };
  return out;
}

// ============================================================ the glow and the cursor

/// The overlay's two files, staged beside this one by stage_bridge.rs: glow.css.js
/// defines AutoCua_CSS, glow.js adopts it and draws the edges and the cursor. They run
/// in the extension's own world, so the page never sees them.
const TOOLS_GLOW_FILES = ["glow.css.js", "glow.js"];
const TOOLS_GLOW_SCRIPT = "AutoCua-glow";
let toolsGlowRegistered = false;
let toolsGlowSource = null;

/// The overlay's source as one script, for a page the extension may not script but
/// the debugger can reach (the logo page): browser.rs glow_source, done here.
async function toolsGlowText() {
  if (toolsGlowSource === null) {
    const parts = [];
    for (const f of TOOLS_GLOW_FILES) parts.push(await (await fetch(chrome.runtime.getURL(f))).text());
    toolsGlowSource = parts.join("\n");
  }
  return toolsGlowSource;
}

/// Dress one open tab: as a content script, or through the debugger on a page the
/// extension may not script. False when neither way is open (Chrome's own pages).
async function toolsDress(tabId) {
  if (toolsDialogs.has(tabId)) return false; // frozen until answered; dressed on a later call
  try {
    const ran = chrome.scripting.executeScript({ target: { tabId }, files: TOOLS_GLOW_FILES, injectImmediately: true });
    await toolsUnlessDialog(tabId, ran, () => { throw new Error(TOOLS_DIALOG_OPEN); });
    return true;
  } catch (e) {
    if (toolsDialogs.has(tabId)) return false;
    if (!toolsScriptingRefused(e)) return false;
  }
  try {
    // Only into a document no content script reaches. The glow is not waited on, so this
    // can land after the tab has moved on to a web page, which has the content script's
    // overlay already: a second one in the page's own world was left behind by letting
    // go (measured).
    const expression = `if (!/^https?:$/.test(location.protocol)) {\n${await toolsGlowText()}\n}`;
    await toolsCdp(tabId, "Runtime.evaluate", { expression, returnByValue: true });
    return true;
  } catch (e) {
    return false;
  }
}

/// Pages a content script can run in; the rest (Chrome's pages, this extension's) are
/// skipped without a word, like browser.rs skips a tab it cannot arm.
function toolsScriptable(tab) {
  return !toolsUnreachable(tab.url || tab.pendingUrl || "");
}

/// browser.rs glow_tabs(): arm the overlay so every document from now on glows (a
/// registered document-start content script) and dress the pages already open: one tab,
/// or all of them. `only` (single-tab mode): this one tab and nothing else, as browser.rs
/// never decorates the tabs of parallel agents; no content script is registered then,
/// and the tab is dressed again on every call.
async function toolsGlow(tabId, only) {
  if (!only && !toolsGlowRegistered) {
    const have = await chrome.scripting.getRegisteredContentScripts({ ids: [TOOLS_GLOW_SCRIPT] }).catch(() => []);
    if (!have.length) {
      await chrome.scripting.registerContentScripts([{
        id: TOOLS_GLOW_SCRIPT, js: TOOLS_GLOW_FILES, matches: ["<all_urls>"], runAt: "document_start",
        allFrames: false, persistAcrossSessions: false,
      }]);
    }
    toolsGlowRegistered = true;
  }
  const tabs = typeof tabId === "number"
    ? [await chrome.tabs.get(tabId)]
    : await chrome.tabs.query({});
  // Nothing here is waited on, as browser.rs glow_tabs waits on nothing: one page that
  // did not answer held up every step (measured: the request ran out its whole time).
  // One of the agent's own tabs (toolsOurs) still silent after TOOLS_FROZEN_MS is
  // frozen, and is closed and opened again (toolsReopen). The person's own tabs are
  // never closed: a page showing an alert does not answer either, and Chrome does not
  // report an alert that opened before the debugger came (measured).
  let asked = 0;
  for (const t of tabs.filter(toolsScriptable)) {
    // Frozen or unloaded by Chrome itself to save power: not stuck, and it wakes when shown.
    if (t.frozen || t.discarded) continue;
    const ours = toolsOurs.has(t.id);
    // The person's own tab: asked one request at a time, so one that stays frozen does
    // not collect them.
    if (!ours && toolsDressing.has(t.id)) continue;
    if (!ours) toolsDressing.add(t.id);
    let answered = false;
    const dressing = toolsDress(t.id).finally(() => {
      answered = true;
      toolsDressings.delete(dressing);
      if (!ours) toolsDressing.delete(t.id);
    });
    toolsDressings.add(dressing);
    asked += 1;
    if (!ours) continue;
    // One of the agent's own tabs: asked every time, every request watched. Skipping a
    // tab whose last request was still out let a page that froze just after that one
    // was answered go unwatched (measured: the scan then ran out its 40 s).
    setTimeout(() => {
      if (!answered && !toolsDialogs.has(t.id)) toolsReopen(t.id, "froze").catch(() => {});
    }, TOOLS_FROZEN_MS);
  }
  return { asked };
}

async function toolsUnglow() {
  toolsGlowRegistered = false;
  await chrome.scripting.unregisterContentScripts({ ids: [TOOLS_GLOW_SCRIPT] }).catch(() => {});
}

/// How long letting go waits on one page to take its glow off: a frozen page keeps it
/// rather than hold up the rest.
const TOOLS_STRIP_MS = 1000;

/// How long the app's picture on a scraping step (the "frame" request) may take: it is
/// decoration, so a page that will not draw is given up on rather than waited for.
const TOOLS_FRAME_MS = 1500;

/// Take the glow off one open page (glow.js __AutoCuaGlowOff), the way it was put on:
/// as a content script, or through the debugger on a page the extension may not script.
async function toolsStripGlow(tabId) {
  if (toolsDialogs.has(tabId)) return; // frozen: the glow goes with its next document
  const off = () => { if (typeof window.__AutoCuaGlowOff === "function") window.__AutoCuaGlowOff(); };
  const run = chrome.scripting.executeScript({ target: { tabId }, func: off, injectImmediately: true })
    .catch((e) => {
      if (!toolsScriptingRefused(e) || !toolsAttached.has(tabId)) return;
      return chrome.debugger.sendCommand({ tabId }, "Runtime.evaluate", { expression: `(${off})()` }).catch(() => {});
    });
  await Promise.race([run, toolsSleep(TOOLS_STRIP_MS)]);
}

/// Let the browser go for one agent (`line`): on `release`, and from background.js when
/// the agent side goes away without one (Ctrl-C, killed, crashed: its socket closes in
/// every case). The last agent on the line (`last`) lets the whole browser go: no glow on
/// new pages, the glow taken off the open ones, then the debugger off every tab it is on,
/// so Chrome's bar goes and no dialog is answered for the person any more; tabs the
/// debugger is on that this worker does not know of (it restarted) go too. An agent among
/// others (a parallel run) lets only its own tabs go.
async function toolsLetGo(line, last = true) {
  toolsLettingGo.add(line);
  try {
    await toolsLetGoNow(line, last);
  } finally {
    for (const [id, l] of [...toolsOurs]) {
      if (l === line || last) toolsOurs.delete(id);
    }
    toolsLettingGo.delete(line);
  }
}

async function toolsLetGoNow(line, last) {
  if (last) await toolsUnglow();
  // A glow request still out would put the glow back once it is taken off.
  await Promise.race([Promise.allSettled([...toolsDressings]), toolsSleep(TOOLS_STRIP_MS)]);
  const mine = [...toolsOurs].filter(([, l]) => l === line).map(([id]) => id);
  const tabs = last
    ? (await chrome.tabs.query({}).catch(() => [])).filter(toolsScriptable).map((t) => t.id)
    : mine;
  await Promise.all(tabs.map((id) => toolsStripGlow(id)));
  if (last) {
    const targets = await chrome.debugger.getTargets().catch(() => []);
    const on = targets.filter((t) => t.attached && typeof t.tabId === "number").map((t) => t.tabId);
    for (const id of new Set([...toolsAttached, ...on])) await toolsDetach(id);
  } else {
    for (const id of mine) await toolsDetach(id);
  }
}

/// The front tab of a parallel run (agent_launcher.run_parallel_web_agents): this
/// extension's own agents.html (staged beside this file with the video it plays, from
/// AutoCua/logo), "Multiple agents running" over the looping background, with the number
/// of agents. The agents work in background tabs of their own and never take the front
/// from it; it is nobody's tab (not in toolsOurs), so no agent drives it. Reused when it
/// is open; a count under 2 closes it (the run is over).
async function toolsCover(count) {
  // Right after a launch the window can still be on its way ("No current window"): a
  // few more tries over 3 s before giving up.
  let last = null;
  for (let i = 0; i < 10; i++) {
    try {
      return await toolsCoverNow(count);
    } catch (e) {
      last = e;
      await toolsSleep(300);
    }
  }
  throw last;
}

async function toolsCoverNow(count) {
  const base = chrome.runtime.getURL("agents.html");
  await toolsWindowId(); // a window to work in, made if the browser has none
  const tabs = await chrome.tabs.query({});
  const open = tabs.filter((t) => (t.url || t.pendingUrl || "").startsWith(base));
  if (count < 2) {
    for (const t of open) {
      // The last tab of its window would take the window with it (and on Windows and
      // Linux the last window takes Chrome): that one goes back to an empty page instead.
      if (tabs.filter((o) => o.windowId === t.windowId).length <= 1) {
        await chrome.tabs.update(t.id, { url: "chrome://newtab/" })
          .catch(() => chrome.tabs.update(t.id, { url: "about:blank" }).catch(() => {}));
      } else {
        await chrome.tabs.remove(t.id).catch(() => {});
      }
    }
    return { shown: false };
  }
  const url = `${base}?n=${count}`;
  if (open.length) {
    await chrome.tabs.update(open[0].id, { url, active: true });
    for (const t of open.slice(1)) await chrome.tabs.remove(t.id).catch(() => {});
    return { shown: true, id: open[0].id };
  }
  // The empty tab the browser started on (about:blank, the New Tab page) takes the
  // cover, rather than an extra tab opening beside it (owner: "use the existing one").
  const blank = tabs.find((t) => /^(about:blank)?$/.test(t.url || t.pendingUrl || "") || toolsNewTabPage(t.url || t.pendingUrl));
  if (blank) {
    await chrome.tabs.update(blank.id, { url, active: true });
    return { shown: true, id: blank.id };
  }
  const windowId = await toolsWindowId();
  const tab = await chrome.tabs.create({ url, active: true, windowId });
  return { shown: true, id: tab.id };
}

/// The overlay's entry points, run in the extension's world of a page.
function toolsCursorCall(op, x, y, text) {
  const w = window;
  switch (op) {
    case "to": return !!(w.__AutoCuaCursor && w.__AutoCuaCursor(x, y));
    case "down": w.__AutoCuaCursorDown && w.__AutoCuaCursorDown(); return true;
    case "up": w.__AutoCuaCursorUp && w.__AutoCuaCursorUp(); return true;
    case "place": w.__AutoCuaCursorPlace && w.__AutoCuaCursorPlace(x, y); return true;
    case "say": w.__AutoCuaSay && w.__AutoCuaSay(text); return true;
    case "hide": w.__AutoCuaCursorHide && w.__AutoCuaCursorHide(); return true;
    case "scrape_on": w.__AutoCuaScrapeGlow && w.__AutoCuaScrapeGlow(true); return true;
    case "scrape_off": w.__AutoCuaScrapeGlow && w.__AutoCuaScrapeGlow(false); return true;
    case "scrape_feed": w.__AutoCuaScrapeFeed && w.__AutoCuaScrapeFeed(text); return true;
    default: return false;
  }
}

/// browser.rs cursor_to / cursor_press / cursor_sync / cursor_say / cursor_hide, and the
/// scraping-mode look (scrape_glow_on / _sync / _off) with the data its card streams
/// (scrape_feed, in `text`). `to` addresses the driven tab and answers whether the arrow
/// moved; the others reach every tab and wait for none of them, since a click can navigate
/// and a run can span tabs. The scraping look comes addressed (`only`) to the one tab being
/// read.
async function toolsCursor(msg) {
  const op = String(msg.op || "");
  const x = typeof msg.x === "number" ? Math.round(msg.x * 10) / 10 : null;
  const y = typeof msg.y === "number" ? Math.round(msg.y * 10) / 10 : null;
  const text = typeof msg.text === "string" ? msg.text : "";
  if (op === "to") {
    if (typeof msg.tabId !== "number") throw new Error("cursor to: no tab");
    const moved = await toolsRunInPage(msg.tabId, toolsCursorCall, [op, x, y, text]).catch(() => false);
    return { moved: !!moved };
  }
  // single-tab mode: this agent's tab only (browser.rs reaches its own sessions only)
  const tabs = msg.only && typeof msg.tabId === "number"
    ? [await chrome.tabs.get(msg.tabId)]
    : await chrome.tabs.query({});
  for (const t of tabs.filter(toolsScriptable)) {
    toolsRunInPage(t.id, toolsCursorCall, [op, x, y, text]).catch(() => {});
  }
  return { sent: true };
}

// ============================================================ dispatch

/// The tab a tool request names; the Rust side always names one.
async function toolsTab(msg) {
  if (typeof msg.tabId === "number") return chrome.tabs.get(msg.tabId);
  const [tab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });
  if (!tab) throw new Error("no active tab");
  return tab;
}

function toolsRect(msg) {
  const r = msg.rect;
  if (!Array.isArray(r) || r.length !== 4 || r.some((v) => typeof v !== "number")) {
    throw new Error("the request needs rect: [x, y, w, h] in CSS px");
  }
  return r;
}

/// One tool request from the agent side, answered with its result; a failure throws
/// with the message the model should read.
async function toolsHandle(msg, line) {
  switch (msg.type) {
    case "click": {
      const tab = await toolsTab(msg);
      await toolsPress(tab.id, toolsRect(msg), Number(msg.hold_s) || 0, Number(msg.times) || 1);
      return { clicked: true };
    }
    case "input": {
      const tab = await toolsTab(msg);
      await toolsTypeInto(tab.id, toolsRect(msg), msg.text == null ? "" : String(msg.text), !!msg.enter);
      return { typed: true };
    }
    case "keyboard": {
      const tab = await toolsTab(msg);
      const keys = toolsKeysOf(msg);
      await toolsChord(tab.id, keys);
      return { pressed: keys.map((k) => k.key) };
    }
    case "scroll": {
      const tab = await toolsTab(msg);
      await toolsWheel(tab.id, toolsRect(msg), Number(msg.dx) || 0, Number(msg.dy) || 0);
      return { scrolled: true };
    }
    case "scroll_probe": {
      const tab = await toolsTab(msg);
      return { probe: await toolsScrollProbe(tab.id, toolsRect(msg)) };
    }
    case "run_script": {
      const tab = await toolsTab(msg);
      const code = msg.code == null ? "" : String(msg.code);
      if (!code.trim()) throw new Error("run_script needs JavaScript in `value`");
      return toolsEvaluate(tab.id, code);
    }
    case "navigate": {
      const tab = await toolsTab(msg);
      const url = String(msg.url || "");
      if (!url) throw new Error("navigate needs a url");
      await toolsNavigate(tab.id, url);
      return { loaded: true };
    }
    case "reload": {
      const tab = await toolsTab(msg);
      await toolsReload(tab.id);
      return { loaded: true };
    }
    case "history": {
      const tab = await toolsTab(msg);
      await toolsHistory(tab.id, Number(msg.delta) || 0);
      return { loaded: true };
    }
    case "wait_load": {
      const tab = await toolsTab(msg);
      if (tab.status === "loading") await toolsWaitStatus(tab.id, typeof msg.timeoutMs === "number" ? msg.timeoutMs : TOOLS_LOAD_MS);
      return { status: (await chrome.tabs.get(tab.id)).status };
    }
    case "new_tab":
      return toolsNewTab(msg.url ? String(msg.url) : "", line);
    case "activate": {
      const tab = await toolsTab(msg);
      return toolsActivate(tab.id, !!msg.wait_load, line);
    }
    case "close_tab": {
      const tab = await toolsTab(msg);
      return toolsCloseTab(tab.id);
    }
    case "dialog": {
      // controller/dialog: the model's answer to the popup holding this tab. Forgotten
      // BEFORE the answer goes: the page it frees may open the next one at once (a page
      // that reopens its alert), and that one must stay.
      const tab = await toolsTab(msg);
      if (!toolsDialogs.has(tab.id)) throw new Error("no popup is open on this tab");
      const answer = { accept: !!msg.accept };
      if (msg.accept && typeof msg.promptText === "string") answer.promptText = msg.promptText;
      toolsDialogGone(tab.id);
      await toolsCdp(tab.id, "Page.handleJavaScriptDialog", answer);
      return { answered: true };
    }
    case "blank_page": {
      const tab = await toolsTab(msg);
      return toolsBlankPage(tab.id, String(msg.html || ""));
    }
    case "settle": {
      const tab = await toolsTab(msg);
      return { settled: await toolsSettle(tab.id) };
    }
    case "cursor":
      return toolsCursor(msg);
    case "frame": {
      // browser.rs scrape_frame: the page as it is, for the app's picture on a step of
      // scraping mode, when no scan runs. Photographed the way a scan photographs it
      // (element.js elementScan): Chrome's own capture of the tab on show, the debugger's
      // when that fails (a minimized window) or the tab is behind another. The app's
      // picture alone, so a page that will not draw costs the step TOOLS_FRAME_MS at
      // most. Not through a popup holding the page.
      const tab = await toolsTab(msg);
      if (toolsDialogs.has(tab.id)) return { frame: null };
      const take = async () => {
        const shown = tab.active
          ? await chrome.tabs.captureVisibleTab(tab.windowId, { format: "jpeg", quality: 75 }).catch(() => null)
          : null;
        return typeof shown === "string" ? shown : toolsScanFallback.capture(tab).catch(() => null);
      };
      const shot = await Promise.race([take(), toolsSleep(TOOLS_FRAME_MS).then(() => null)]);
      return { frame: typeof shot === "string" ? shot.slice(shot.indexOf(",") + 1) : null };
    }
    case "glow":
      return toolsGlow(typeof msg.tabId === "number" ? msg.tabId : null, !!msg.only);
    case "release": {
      const last = toolsLastLine(line);
      await toolsLetGo(line, last);
      if (last) toolsReset();
      return { released: true };
    }
    case "cover":
      return toolsCover(Number(msg.count) || 0);
    default:
      return undefined; // not a tool request
  }
}
