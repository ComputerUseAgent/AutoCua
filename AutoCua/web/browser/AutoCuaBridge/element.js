// AutoCua - browser element scanner, extension side (AutoCuaBridge)
//
// A copy of AutoCua/web/tree/element.rs that runs inside the extension instead of
// over a debugging connection. Same config, same rules, same numbering ([1] is always
// the page), same tree text (pruned the way browser.rs prunes it before the model
// sees it), same summary line and the same numbered marks.
//
// What changes is where the facts come from. element.rs reads DOMSnapshot and the
// accessibility tree over CDP, one session per cross-process frame; elementScanPage()
// reads the live DOM from inside every frame of the tab (one injection, all frames),
// and the screenshot comes from Chrome's own tab capture. No debugger is attached, so
// Chrome shows no "started debugging this browser" bar.
//
// Frames. A frame of the same origin as its parent is read by the parent through
// contentDocument, like element.rs reads a same-process document: inline, at the
// iframe's place in the tree, and with no accessibility data (Chrome's full AX tree
// covers the root document only, and element.rs mirrors that). A frame of another
// origin reads itself and is appended after its parent's records behind a <frame>
// line, like an element.rs session. It learns where it sits from its parent over a
// postMessage handshake (a page cannot see Chrome's frame ids), so its records land
// in page coordinates, clipped to its iframe's content box.
//
// Where that makes the answer differ from element.rs:
// - role, accessible name and state are worked out from the DOM (ARIA attributes,
//   labels, tags) instead of read from Chrome's accessibility tree;
// - DOMSnapshot's isClickable is only partly visible from the page: a <label> tied
//   to a control and editable content are, elements with click listeners are not;
// - occlusion uses a hit test at each record's centre instead of paint order;
// - a frame of another origin that Chrome keeps in its parent's process (a data:
//   frame, a same-site frame) is read like a cross-process one here, behind a
//   <frame> line after its parent, where element.rs reads it inline.
//
// Keep ELEMENT_DEFAULT_CONFIG identical to DEFAULT_CONFIG in element.rs.

const ELEMENT_DEFAULT_CONFIG = {
  interactive_roles: [
    "button", "link", "checkbox", "radio", "tab", "menuitem", "menuitemcheckbox",
    "menuitemradio", "option", "switch", "textbox", "combobox", "searchbox",
    "slider", "spinbutton", "treeitem", "listbox", "scrollbar", "disclosuretriangle",
    "colorwell", "date", "datetime", "inputtime", "radiogroup", "togglebutton",
  ],
  landmark_roles: [
    "list", "listitem", "table", "row", "grid", "gridcell", "navigation", "main",
    "dialog", "alertdialog", "form", "banner", "contentinfo", "tablist", "menu",
    "menubar", "tree", "article", "heading", "alert", "status", "rowgroup",
    "columnheader", "rowheader", "toolbar", "region", "search", "complementary",
  ],
  interactive_tags: ["a", "button", "input", "select", "textarea", "summary", "video", "audio"],
  landmark_tags: [
    "ul", "ol", "li", "table", "tr", "td", "th", "nav", "main", "form", "dialog",
    "header", "footer", "section", "article", "aside", "fieldset",
    "h1", "h2", "h3", "h4", "h5", "h6", "iframe",
  ],
  skip_tags: [
    "script", "style", "noscript", "meta", "link", "head", "title", "template",
    "br", "hr", "path", "defs", "g", "circle", "rect", "polygon", "use", "symbol",
    "clippath", "lineargradient", "stop", "mask", "filter", "ellipse", "line",
  ],
  signals: { ax_role: true, is_clickable: true, interactive_tag: true, cursor_pointer: true, focusable: false },
  limits: {
    max_elements: 300, max_name_chars: 90, max_text_chars: 140, max_href_chars: 60,
    min_width: 2, min_height: 2, viewport_margin: 0,
  },
  occlusion: { enabled: true, min_occluder_viewport_fraction: 0.12 },
  noise: { enabled: true, drop_nested_unnamed: true, drop_nested_same_name: true },
  cross_process_frames: true,
  hosts: {
    "youtube.com": { signals: { cursor_pointer: false }, limits: { max_elements: 200 } },
    "mail.google.com": { signals: { cursor_pointer: true } },
  },
};

// element.rs settle(): how long the page must be free of network activity, and the cap.
const ELEMENT_SETTLE_QUIET_MS = 150;
const ELEMENT_SETTLE_CEILING_MS = 3000;
// Requests already open when a scan starts are waited on for this long at most, and
// these kinds never (element.rs SEED_BUDGET_MS and NEVER_WAIT_FOR).
const ELEMENT_SEED_BUDGET_MS = 1500;
const ELEMENT_NEVER_WAIT_FOR = new Set(["main_frame", "sub_frame", "media", "ping", "csp_report"]);
// A frame of another origin waits this long for its parent to place it before it
// gives up, which only happens when its parent could not be injected. On an ordinary
// page a parent answers within a few ms, even while it is still reading a long page.
const ELEMENT_FRAME_WAIT_MS = 2000;

// ============================================================ config

function elementMerge(base, over) {
  for (const [k, v] of Object.entries(over || {})) {
    const b = base[k];
    if (b && typeof b === "object" && !Array.isArray(b) && v && typeof v === "object" && !Array.isArray(v)) {
      elementMerge(b, v);
    } else {
      base[k] = structuredClone(v);
    }
  }
  return base;
}

function elementHostOf(url) {
  let s = url.includes("://") ? url.split("://")[1] : url;
  s = s.split("/")[0];
  s = s.split("@").pop();
  return s.split(":")[0].toLowerCase();
}

// shallow-merge any matching per-host block over the base
function elementConfigFor(cfg, url) {
  const host = elementHostOf(url || "");
  const out = structuredClone(cfg);
  delete out.hosts;
  for (const [pat, over] of Object.entries(cfg.hosts || {})) {
    const p = pat.toLowerCase();
    if (host === p || host.endsWith("." + p)) elementMerge(out, over);
  }
  return out;
}

function elementBool(cfg, g, k, d) {
  const v = cfg[g] && cfg[g][k];
  return typeof v === "boolean" ? v : d;
}

function elementNum(cfg, g, k, d) {
  const v = cfg[g] && cfg[g][k];
  return typeof v === "number" ? v : d;
}

// ============================================================ settle

// Open requests per tab, from webRequest: what element.rs counts from Network events.
const elementInflight = new Map(); // tabId -> Map(requestId -> type)
const elementLastActivity = new Map(); // tabId -> ms

function elementTrackRequests() {
  const started = (d) => {
    if (d.tabId < 0) return;
    if (!elementInflight.has(d.tabId)) elementInflight.set(d.tabId, new Map());
    elementInflight.get(d.tabId).set(d.requestId, d.type);
    elementLastActivity.set(d.tabId, performance.now());
  };
  const ended = (d) => {
    if (d.tabId < 0) return;
    elementInflight.get(d.tabId)?.delete(d.requestId);
    elementLastActivity.set(d.tabId, performance.now());
  };
  chrome.webRequest.onBeforeRequest.addListener(started, { urls: ["<all_urls>"] });
  chrome.webRequest.onCompleted.addListener(ended, { urls: ["<all_urls>"] });
  chrome.webRequest.onErrorOccurred.addListener(ended, { urls: ["<all_urls>"] });
}

// Block until the page stops fetching, or the ceiling runs out. Starts already
// "quiet", so a page with nothing happening returns on the first look.
async function elementSettle(tabId) {
  const start = performance.now();
  const handedOver = new Set(elementInflight.get(tabId)?.keys() || []);
  for (;;) {
    await new Promise((r) => setTimeout(r, 60));
    const elapsed = performance.now() - start;
    let busy = false;
    for (const [id, type] of elementInflight.get(tabId) || []) {
      if (handedOver.has(id) && (ELEMENT_NEVER_WAIT_FOR.has(type) || elapsed >= ELEMENT_SEED_BUDGET_MS)) continue;
      busy = true;
      break;
    }
    const last = Math.max(elementLastActivity.get(tabId) ?? -Infinity, start - ELEMENT_SETTLE_QUIET_MS);
    if (!busy && performance.now() - last >= ELEMENT_SETTLE_QUIET_MS) break;
    if (elapsed >= ELEMENT_SETTLE_CEILING_MS) break;
  }
  return Math.round(performance.now() - start);
}

// ============================================================ scan (runs in every frame)

// Injected with chrome.scripting.executeScript into every frame of the tab at once, so
// it must be self-contained: everything it uses is defined inside it, and only `cfg`
// and `scan` ({nonce, wait_ms}) come in. Three kinds of frame answer:
// - the page itself reads its document and the same-origin frames in it, and returns
//   its records, its viewport and the frames of another origin it met (it is path
//   "0", they are "0.0", "0.1", ... in document order);
// - a frame its parent can read (same origin) returns {covered: true} at once;
// - a frame of another origin asks its parent where it sits (a postMessage hello,
//   answered with its path, origin and clip in page device px, and the page's
//   viewport area for the occlusion rule), then reads its document the same way, or
//   returns {path, skipped: true} when it has no layout or is off screen.
// The listener that answers hellos outlives the script that installed it, so a child
// that asks after its parent's result went back is still placed. One listener per
// frame: a new scan replaces it, and hellos carrying another scan's nonce are ignored.
function elementScanPage(cfg, scan) {
  let frameElement = null;
  try {
    frameElement = window.frameElement;
  } catch (e) {
    frameElement = null;
  }
  const isTop = window.parent === window;
  if (!isTop && frameElement) return { covered: true };

  // -- placing frames ---------------------------------------------------------
  const kids = []; // my frames of another origin, in walk order
  let mine = isTop ? { path: "0", origin: [0, 0], clip: null, area: 0 } : null;
  let walked = false;
  const pending = [];
  const place = (ev) => {
    const kid = mine && kids.find((k) => k.win === ev.source);
    if (!kid) return;
    ev.source.postMessage(
      { __AutoCua: "place", nonce: scan.nonce, path: kid.path, origin: kid.origin, clip: kid.clip, area: mine.area },
      "*",
    );
  };
  const onMessage = (ev) => {
    const d = ev.data;
    if (!d || typeof d !== "object" || d.__AutoCua !== "hello" || d.nonce !== scan.nonce) return;
    if (walked) place(ev);
    else pending.push(ev);
  };
  if (self.__AutoCuaOnMessage) window.removeEventListener("message", self.__AutoCuaOnMessage);
  self.__AutoCuaOnMessage = onMessage;
  window.addEventListener("message", onMessage);
  const done = (result) => {
    walked = true;
    for (const ev of pending.splice(0)) place(ev);
    return result;
  };
  const failed = (e) => done({ path: mine ? mine.path : null, error: String(e), stack: String(e && e.stack).slice(0, 800) });

  const intersect = (a, b) => {
    const x0 = Math.max(a[0], b[0]);
    const y0 = Math.max(a[1], b[1]);
    const x1 = Math.min(a[0] + a[2], b[0] + b[2]);
    const y1 = Math.min(a[1] + a[3], b[1] + b[3]);
    const w = x1 - x0;
    const h = y1 - y0;
    return w <= 0 || h <= 0 ? null : [x0, y0, w, h];
  };
  // chrome.dom.openOrClosedShadowRoot also reads closed shadow roots, but it only
  // takes HTML elements (an SVG element makes it throw).
  const shadowOf = (el) => {
    if (el.namespaceURI === "http://www.w3.org/1999/xhtml" &&
        typeof chrome !== "undefined" && chrome.dom && chrome.dom.openOrClosedShadowRoot) {
      try {
        return chrome.dom.openOrClosedShadowRoot(el);
      } catch (e) {
        return el.shadowRoot || null;
      }
    }
    return el.shadowRoot || null;
  };
  // A frame of another origin: it reads itself once told where it sits. `box` is its
  // content box in page device px, null when it has no layout. Like element.rs, a
  // frame with no box or nothing of it on screen is skipped.
  const addKid = (el, box) => {
    const clip = box && mine.clip ? intersect(mine.clip, box) : null;
    kids.push({ win: el.contentWindow, path: `${mine.path}.${kids.length}`, origin: box ? [box[0], box[1]] : null, clip });
  };
  const kidOut = (k) => ({ path: k.path, skipped: !k.clip });
  // Frames inside a document that is not read (no layout, off screen) still ask to be
  // placed; listing them without a clip stops them at once.
  const collectKids = (doc, depth) => {
    if (depth > 8) return;
    const stack = [doc];
    while (stack.length) {
      const n = stack.pop();
      if (n.nodeType === 1) {
        const sr = shadowOf(n);
        if (sr) stack.push(sr);
        const t = n.localName;
        if (t === "iframe" || t === "frame") {
          let cd = null;
          try {
            cd = n.contentDocument;
          } catch (e) {
            cd = null;
          }
          if (cd) collectKids(cd, depth + 1);
          else if (n.contentWindow) addKid(n, null);
          continue;
        }
      }
      for (let c = n.firstElementChild; c; c = c.nextElementSibling) stack.push(c);
    }
  };
  const skippedFrame = () => {
    collectKids(document, 0);
    return { path: mine.path, skipped: true, kids: kids.map(kidOut) };
  };

  if (isTop) {
    try {
      return done(run());
    } catch (e) {
      return failed(e);
    }
  }
  return new Promise((resolve) => {
    let timer = null;
    const onPlace = (ev) => {
      const d = ev.data;
      if (ev.source !== window.parent || !d || typeof d !== "object" || d.__AutoCua !== "place" || d.nonce !== scan.nonce) return;
      window.removeEventListener("message", onPlace);
      clearInterval(timer);
      mine = { path: d.path, origin: d.origin, clip: d.clip, area: d.area };
      try {
        resolve(done(mine.clip ? run() : skippedFrame()));
      } catch (e) {
        resolve(failed(e));
      }
    };
    window.addEventListener("message", onPlace);
    const hello = () => window.parent.postMessage({ __AutoCua: "hello", nonce: scan.nonce }, "*");
    const deadline = performance.now() + scan.wait_ms;
    timer = setInterval(() => {
      if (performance.now() < deadline) {
        hello();
        return;
      }
      clearInterval(timer);
      window.removeEventListener("message", onPlace);
      resolve(done({ orphan: true }));
    }, 5);
    hello();
  });

  function run() {
    const t0 = performance.now();
    const dpr = window.devicePixelRatio || 1;
    // layoutViewport in device px: what element.rs clips against and hands back as [1].
    // The scrolling element, not <html>: in quirks mode (no doctype, e.g. Hacker News)
    // <html> reports the whole document's height and <body> the viewport's.
    const se = document.scrollingElement || document.documentElement;
    const vw = se.clientWidth * dpr;
    const vh = se.clientHeight * dpr;

    const set = (k) => new Set(cfg[k] || []);
    const sets = {
      iroles: set("interactive_roles"), lroles: set("landmark_roles"),
      itags: set("interactive_tags"), ltags: set("landmark_tags"), skip: set("skip_tags"),
    };
    const bool = (g, k, d) => {
      const v = cfg[g] && cfg[g][k];
      return typeof v === "boolean" ? v : d;
    };
    const num = (g, k, d) => {
      const v = cfg[g] && cfg[g][k];
      return typeof v === "number" ? v : d;
    };
    const sig = {
      ax_role: bool("signals", "ax_role", true),
      is_clickable: bool("signals", "is_clickable", true),
      interactive_tag: bool("signals", "interactive_tag", true),
      cursor_pointer: bool("signals", "cursor_pointer", true),
      focusable: bool("signals", "focusable", false),
    };
    const lim = {
      max_name: num("limits", "max_name_chars", 90),
      max_text: num("limits", "max_text_chars", 140),
      max_href: num("limits", "max_href_chars", 60),
      min_w: num("limits", "min_width", 2),
      min_h: num("limits", "min_height", 2),
      margin: num("limits", "viewport_margin", 0),
    };

    const clip = (s, n) => {
      const flat = String(s || "").split(/\s+/).filter(Boolean).join(" ");
      const chars = Array.from(flat);
      if (chars.length > n) return chars.slice(0, Math.max(0, n - 1)).join("") + "…";
      return flat;
    };
    const pxv = (s, d) => {
      const v = parseFloat(String(s || "").replace(/px$/, ""));
      return Number.isFinite(v) ? v : d;
    };
    const PHRASING = new Set(["em", "strong", "b", "i", "u", "span", "code", "mark", "small", "sub", "sup", "wbr", "abbr", "time", "data"]);

    const styles = new Map();
    const styleOf = (el) => {
      let s = styles.get(el);
      if (!s) {
        s = (el.ownerDocument.defaultView || window).getComputedStyle(el);
        styles.set(el, s);
      }
      return s;
    };
    const layout = new Map();
    const laidOut = (el) => {
      let v = layout.get(el);
      if (v === undefined) {
        v = el.getClientRects().length > 0;
        layout.set(el, v);
      }
      return v;
    };

    // -- accessibility, worked out from the DOM ---------------------------

    const ARIA_ROLES = new Set([
      "alert", "alertdialog", "application", "article", "banner", "blockquote", "button", "caption", "cell",
      "checkbox", "code", "columnheader", "combobox", "complementary", "contentinfo", "definition", "deletion",
      "dialog", "directory", "document", "emphasis", "feed", "figure", "form", "generic", "grid", "gridcell",
      "group", "heading", "img", "image", "insertion", "link", "list", "listbox", "listitem", "log", "main",
      "marquee", "math", "menu", "menubar", "menuitem", "menuitemcheckbox", "menuitemradio", "meter",
      "navigation", "none", "note", "option", "paragraph", "presentation", "progressbar", "radio", "radiogroup",
      "region", "row", "rowgroup", "rowheader", "scrollbar", "search", "searchbox", "separator", "slider",
      "spinbutton", "status", "strong", "subscript", "superscript", "switch", "tab", "table", "tablist",
      "tabpanel", "term", "textbox", "time", "timer", "toolbar", "tooltip", "tree", "treegrid", "treeitem",
    ]);
    const layoutTables = new Map();
    // Chrome tells data tables from layout tables; a table with headers, a caption or
    // table roles is data, anything else is layout (LayoutTable / Row / Cell).
    const isLayoutTable = (t) => {
      if (!t) return false;
      let v = layoutTables.get(t);
      if (v === undefined) {
        v = !(t.querySelector("th, caption, thead, [role=columnheader], [role=rowheader]") ||
              t.hasAttribute("summary") || t.getAttribute("role"));
        layoutTables.set(t, v);
      }
      return v;
    };
    const inSectioning = (el) => !!(el.parentElement && el.parentElement.closest("article, aside, main, nav, section"));
    const implicitRole = (el, tag) => {
      switch (tag) {
        case "a": case "area": return el.hasAttribute("href") ? "link" : "generic";
        case "button": return "button";
        case "summary": return "DisclosureTriangle";
        case "select": return el.multiple || el.size > 1 ? "listbox" : "combobox";
        case "textarea": return "textbox";
        case "input": {
          const t = (el.getAttribute("type") || "text").toLowerCase();
          switch (t) {
            case "checkbox": return "checkbox";
            case "radio": return "radio";
            case "range": return "slider";
            case "number": return "spinbutton";
            case "search": return el.hasAttribute("list") ? "combobox" : "searchbox";
            case "button": case "submit": case "reset": case "image": case "file": return "button";
            case "color": return "colorwell";
            case "date": return "date";
            case "datetime-local": case "month": case "week": return "datetime";
            case "time": return "inputtime";
            case "hidden": return "none";
            default: return el.hasAttribute("list") ? "combobox" : "textbox";
          }
        }
        case "option": return "option";
        case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": return "heading";
        case "ul": case "ol": case "menu": return "list";
        case "li": return "listitem";
        case "nav": return "navigation";
        case "main": return "main";
        case "header": return inSectioning(el) ? "generic" : "banner";
        case "footer": return inSectioning(el) ? "generic" : "contentinfo";
        case "aside": return "complementary";
        case "form": return "form";
        case "section": return el.hasAttribute("aria-label") || el.hasAttribute("aria-labelledby") ? "region" : "generic";
        case "article": return "article";
        case "dialog": return "dialog";
        case "fieldset": return "group";
        case "table": return isLayoutTable(el) ? "LayoutTable" : "table";
        case "tr": return isLayoutTable(el.closest("table")) ? "LayoutTableRow" : "row";
        case "td": return isLayoutTable(el.closest("table")) ? "LayoutTableCell" : "cell";
        case "th":
          if (isLayoutTable(el.closest("table"))) return "LayoutTableCell";
          return el.getAttribute("scope") === "row" ? "rowheader" : "columnheader";
        case "thead": case "tbody": case "tfoot": return isLayoutTable(el.closest("table")) ? "generic" : "rowgroup";
        case "p": return "paragraph";
        case "img": return el.getAttribute("alt") === "" ? "none" : "image";
        case "svg": return "image";
        case "label": return "LabelText";
        case "iframe": return "Iframe";
        case "video": return "Video";
        case "audio": return "Audio";
        case "strong": return "strong";
        case "em": return "emphasis";
        case "code": return "code";
        case "del": return "deletion";
        case "ins": return "insertion";
        case "sub": return "subscript";
        case "sup": return "superscript";
        case "time": return "time";
        case "mark": return "mark";
        case "blockquote": return "blockquote";
        case "figure": return "figure";
        case "progress": return "progressbar";
        case "meter": return "meter";
        case "output": return "status";
        case "dl": return "list";
        case "dt": return "term";
        case "dd": return "definition";
        case "details": return "group";
        case "caption": return "caption";
        default: return "generic";
      }
    };
    const roleOf = (el, tag) => {
      // Chrome leaves aria-hidden and inert subtrees out; their nodes read as role none.
      if (el.closest('[aria-hidden="true"], [inert]')) return "none";
      const attr = el.getAttribute("role");
      if (attr) {
        for (const tok of attr.trim().toLowerCase().split(/\s+/)) {
          if (ARIA_ROLES.has(tok)) return tok === "presentation" ? "none" : tok === "img" ? "image" : tok;
        }
      }
      return implicitRole(el, tag);
    };
    const NAME_FROM_CONTENT = new Set([
      "button", "link", "checkbox", "radio", "switch", "tab", "menuitem", "menuitemcheckbox", "menuitemradio",
      "option", "treeitem", "heading", "cell", "gridcell", "columnheader", "rowheader", "row", "tooltip",
      "DisclosureTriangle", "LayoutTableCell", "LabelText",
    ]);
    const visibleForName = (el) =>
      typeof el.checkVisibility === "function" ? el.checkVisibility({ visibilityProperty: true }) : laidOut(el);
    // Text alternative of a subtree: text, alt text and embedded control values, with
    // block boundaries as spaces and hidden content left out.
    const textAlt = (root, skip) => {
      let out = "";
      const walk = (n) => {
        for (let c = n.firstChild; c; c = c.nextSibling) {
          if (c.nodeType === 3) {
            out += c.nodeValue;
            continue;
          }
          if (c.nodeType !== 1 || c === skip) continue;
          const t = c.localName.toLowerCase();
          if (t === "script" || t === "style" || t === "template" || t === "noscript") continue;
          if (c.getAttribute("aria-hidden") === "true" || !visibleForName(c)) continue;
          const al = c.getAttribute("aria-label");
          if (al && al.trim()) {
            out += " " + al + " ";
            continue;
          }
          if (t === "img" || t === "area") {
            out += " " + (c.getAttribute("alt") || "") + " ";
            continue;
          }
          if (t === "input" || t === "textarea" || t === "select") {
            out += " " + (c.value || "") + " ";
            continue;
          }
          const block = !String(styleOf(c).display || "").startsWith("inline");
          if (block) out += " ";
          walk(c);
          if (block) out += " ";
        }
      };
      walk(root);
      return out.split(/\s+/).filter(Boolean).join(" ");
    };
    // These take a name only from aria-label / aria-labelledby, never from title or
    // their content: a generic <span title="43,989">44k</span> has no accessible name
    // (the tree shows its text), a generic span with aria-label is named by it.
    const NAME_PROHIBITED = new Set([
      "generic", "none", "caption", "code", "deletion", "emphasis", "insertion",
      "paragraph", "strong", "subscript", "superscript",
    ]);
    const nameOf = (el, tag, role) => {
      if (role === "") return "";
      const lb = el.getAttribute("aria-labelledby");
      if (lb) {
        const t = lb.split(/\s+/).map((id) => {
          const r = el.ownerDocument.getElementById(id);
          return r ? textAlt(r) : "";
        }).join(" ").trim();
        if (t) return t;
      }
      const al = el.getAttribute("aria-label");
      if (al && al.trim()) return al.trim();
      if (NAME_PROHIBITED.has(role)) return "";
      if (tag === "input") {
        const t = (el.getAttribute("type") || "text").toLowerCase();
        if (t === "button" || t === "submit" || t === "reset") {
          return el.value || (t === "submit" ? "Submit" : t === "reset" ? "Reset" : "");
        }
        if (t === "image") return el.getAttribute("alt") || el.value || "Submit";
      }
      if (tag === "input" || tag === "textarea" || tag === "select") {
        const labs = el.labels ? Array.from(el.labels).map((l) => textAlt(l, el)).join(" ").trim() : "";
        if (labs) return labs;
        const ti = el.getAttribute("title");
        if (ti && ti.trim()) return ti.trim();
        const ph = el.getAttribute("placeholder") || el.getAttribute("aria-placeholder");
        if (ph && ph.trim()) return ph.trim();
        return "";
      }
      if (tag === "img" || tag === "area") {
        const a = el.getAttribute("alt");
        if (a && a.trim()) return a.trim();
      }
      if (tag === "fieldset") {
        const lg = el.querySelector(":scope > legend");
        const t = lg ? textAlt(lg) : "";
        if (t) return t;
      }
      if (tag === "table") {
        const cp = el.querySelector(":scope > caption");
        const t = cp ? textAlt(cp) : "";
        if (t) return t;
      }
      if (NAME_FROM_CONTENT.has(role)) {
        const t = textAlt(el);
        if (t) return t;
      }
      const ti = el.getAttribute("title");
      return ti && ti.trim() ? ti.trim() : "";
    };
    const EXPANDABLE = new Set([
      "application", "button", "checkbox", "combobox", "gridcell", "link", "listbox", "menuitem",
      "menuitemcheckbox", "menuitemradio", "row", "rowheader", "columnheader", "switch", "tab", "treeitem",
      "DisclosureTriangle",
    ]);
    const stateOf = (el, tag) => {
      const s = {};
      const ae = el.getAttribute("aria-expanded");
      if (ae !== null) s.expanded = ae === "true";
      else if (tag === "summary" && el.parentElement && el.parentElement.localName === "details") s.expanded = el.parentElement.open;
      else if (tag === "select" && !(el.multiple || el.size > 1)) s.expanded = false;
      if (el.getAttribute("aria-disabled") === "true" || el.matches(":disabled")) s.disabled = true;
      const ac = el.getAttribute("aria-checked");
      if (ac !== null) s.checked = ac === "true";
      return s;
    };
    // Chrome leaves aria-hidden subtrees, inert content and presentational elements out
    // of the accessibility tree; their role does not count.
    const ignoredOf = (el, role) => role === "none" || !!el.closest('[aria-hidden="true"], [inert]');

    // -- document walk ----------------------------------------------------

    const recs = [];
    let nDocs = 0;

    const walkDoc = (doc, docI, origin, frameClip, depth) => {
      if (depth > 8) return;
      // Chrome's full AX tree covers a frame's root document only, so element.rs has
      // no role, name or state for an in-process child document. Neither here.
      const axOn = docI === 0;
      // Nodes in DOMSnapshot order (element, its shadow root, then children), with
      // parent indices, so kinship and depth work on the same terms as element.rs.
      const nodes = [];
      const parent = [];
      const stack = [[doc, -1]];
      while (stack.length) {
        const [n, p] = stack.pop();
        const me = nodes.length;
        nodes.push(n);
        parent.push(p);
        const ch = [];
        if (n.nodeType === 1) {
          const sr = shadowOf(n);
          if (sr) ch.push(sr);
        }
        for (let c = n.firstChild; c; c = c.nextSibling) {
          if (c.nodeType === 1 || c.nodeType === 3) ch.push(c);
        }
        for (let k = ch.length - 1; k >= 0; k--) stack.push([ch[k], me]);
      }
      const kidsOf = new Map();
      for (let i = 0; i < parent.length; i++) {
        if (parent[i] < 0) continue;
        let a = kidsOf.get(parent[i]);
        if (!a) kidsOf.set(parent[i], (a = []));
        a.push(i);
      }
      const box = (r) => [r.left * dpr + origin[0], r.top * dpr + origin[1], r.width * dpr, r.height * dpr];

      // DOMSnapshot puts painted strings on #text nodes; fold phrasing wrappers so
      // <span><em>Gmail</em> is email</span> becomes one readable string.
      const textAt = (ni) => {
        const n = nodes[ni];
        const t = n.nodeValue;
        if (!t || !t.trim()) return null;
        const p = n.parentNode;
        return p && p.nodeType === 1 && laidOut(p) ? t : null;
      };
      const rich = new Map();
      const richOf = (ni) => {
        if (rich.has(ni)) return rich.get(ni);
        let out = "";
        for (const c of kidsOf.get(ni) || []) {
          const n = nodes[c];
          if (n.nodeType === 3) {
            const t = textAt(c);
            if (t) out += t;
          } else if (n.nodeType === 1 && PHRASING.has(n.localName.toLowerCase())) {
            out += richOf(c);
          }
        }
        const flat = out.split(/\s+/).filter(Boolean).join(" ");
        rich.set(ni, flat);
        return flat;
      };

      const emitted = new Set();
      for (let ni = 0; ni < nodes.length; ni++) {
        const el = nodes[ni];
        if (el.nodeType !== 1 || emitted.has(ni)) continue;
        const tag = el.localName.toLowerCase();
        if (sets.skip.has(tag)) continue;
        const b = box(el.getBoundingClientRect());

        if (tag === "iframe" || tag === "frame") {
          let cdoc = null;
          try {
            cdoc = el.contentDocument;
          } catch (e) {
            cdoc = null;
          }
          if (cdoc) {
            // same origin: read here, like element.rs reads a same-process document
            let read = false;
            if (cdoc.documentElement && laidOut(el)) {
              const st = styleOf(el);
              const bl = pxv(st.borderLeftWidth, 0) * dpr;
              const bt = pxv(st.borderTopWidth, 0) * dpr;
              const childBox = [b[0] + bl, b[1] + bt, Math.max(b[2] - 2 * bl, 0), Math.max(b[3] - 2 * bt, 0)];
              const cc = intersect(frameClip, childBox);
              if (cc) {
                nDocs += 1;
                walkDoc(cdoc, nDocs, [childBox[0], childBox[1]], cc, depth + 1);
                read = true;
              }
            }
            if (!read) collectKids(cdoc, depth + 1);
          } else if (el.contentWindow) {
            // another origin: it reads itself, placed at its content box (what
            // element.rs takes from DOM.getBoxModel for an out-of-process frame)
            let cb = null;
            if (laidOut(el)) {
              const st = styleOf(el);
              const l = (pxv(st.borderLeftWidth, 0) + pxv(st.paddingLeft, 0)) * dpr;
              const t = (pxv(st.borderTopWidth, 0) + pxv(st.paddingTop, 0)) * dpr;
              const r = (pxv(st.borderRightWidth, 0) + pxv(st.paddingRight, 0)) * dpr;
              const bo = (pxv(st.borderBottomWidth, 0) + pxv(st.paddingBottom, 0)) * dpr;
              cb = [b[0] + l, b[1] + t, Math.max(b[2] - l - r, 0), Math.max(b[3] - t - bo, 0)];
            }
            addKid(el, cb);
          }
        }

        const r = intersect(b, frameClip);
        if (!r || r[2] < lim.min_w || r[3] < lim.min_h) continue;
        const st = styleOf(el);
        if (st.visibility === "hidden") continue;
        if (st.opacity !== "" && pxv(st.opacity, 1) < 0.05) continue;

        const role = axOn ? roleOf(el, tag) : "";
        const state = axOn ? stateOf(el, tag) : {};
        const ignored = axOn ? ignoredOf(el, role) : false;
        let act =
          (sig.ax_role && sets.iroles.has(role) && !ignored) ||
          (sig.interactive_tag && sets.itags.has(tag)) ||
          // sig.is_clickable: the parts of Chrome's isClickable a page can see (a
          // <label> tied to a control, editable content); click listeners it cannot
          (sig.is_clickable && ((tag === "label" && el.control && !el.control.disabled) || el.isContentEditable)) ||
          (sig.cursor_pointer && st.cursor === "pointer") ||
          (sig.focusable && axOn && el.tabIndex >= 0);
        if (act && (state.disabled === true || st.pointerEvents === "none")) act = false;

        const text = clip(richOf(ni), lim.max_text);
        const land = sets.lroles.has(role) || sets.ltags.has(tag);
        if (!act && !text && !land) continue;
        if (!act && !land && (tag === "body" || tag === "html")) continue;

        const axName = axOn ? nameOf(el, tag, role) : "";
        const kind = act ? "a" : land ? "l" : "t";
        const rec = {
          el, tag, role, parent: parent[ni], node: ni, doc: docI, rect: r, kind,
          name: clip(axName || text, lim.max_name),
          idx: null, ty: null, href: null, checked: false, expanded: null, val: null, editable: false,
        };
        if (act) {
          const ty = el.getAttribute("type");
          rec.ty = ty !== null ? ty.toLowerCase() : null;
          const href = el.getAttribute("href");
          rec.href = href !== null ? clip(href, lim.max_href) : null;
          rec.checked = (tag === "input" && el.checked === true) || state.checked === true;
          // Chrome only reports expanded/collapsed for roles that support it
          if ("expanded" in state && EXPANDABLE.has(role)) rec.expanded = state.expanded;
          // DOMSnapshot's inputValue is there for every <input>, empty or not
          if (tag === "input" && el.type !== "password") rec.val = clip(el.value || "", 40);
          // element.rs reads the AX "editable" property, whose value is a token
          // ("plaintext"), never `true`, so this stays false there as well.
        }
        emitted.add(ni);
        // A text block absorbs the inline descendants its name already folded in,
        // mirroring richOf: links, buttons and controls keep their own records.
        if (kind === "t" && rec.name) {
          const todo = (kidsOf.get(ni) || []).slice();
          while (todo.length) {
            const c = todo.pop();
            const n = nodes[c];
            if (n.nodeType === 3) {
              emitted.add(c);
              continue;
            }
            if (n.nodeType !== 1) continue;
            const ctag = n.localName.toLowerCase();
            if (!PHRASING.has(ctag) || sets.itags.has(ctag)) continue;
            if (axOn && sets.iroles.has(roleOf(n, ctag))) continue;
            if (emitted.has(c)) continue;
            emitted.add(c);
            for (const k of kidsOf.get(c) || []) todo.push(k);
          }
        }
        recs.push(rec);
      }

      // Visible #text nodes the element pass did not cover.
      for (let ni = 0; ni < nodes.length; ni++) {
        const n = nodes[ni];
        if (n.nodeType !== 3 || emitted.has(ni)) continue;
        const p = parent[ni];
        if (p >= 0 && emitted.has(p)) continue;
        const pel = n.parentNode;
        if (!pel || pel.nodeType !== 1 || !laidOut(pel)) continue;
        if (!intersect(box(pel.getBoundingClientRect()), frameClip) && styleOf(pel).position !== "fixed") continue;
        const st = styleOf(pel);
        if (st.visibility === "hidden") continue;
        if (st.opacity !== "" && pxv(st.opacity, 1) < 0.05) continue;
        const name = clip(n.nodeValue, lim.max_name);
        if (!name) continue;
        const range = doc.createRange();
        range.selectNodeContents(n);
        const tr = range.getBoundingClientRect();
        if (tr.width === 0 && tr.height === 0) continue;
        // Clip to every ancestor that hides its overflow (the visually-hidden
        // skip-link pattern, text scrolled out of an overflow:auto panel).
        let rr = box(tr);
        let anc = p;
        let cropped = false;
        for (let k = 0; k < 40 && anc >= 0; k++) {
          const a = nodes[anc];
          if (a.nodeType === 1 && laidOut(a)) {
            const ast = styleOf(a);
            if ((ast.overflowX && ast.overflowX !== "visible") || (ast.overflowY && ast.overflowY !== "visible")) {
              const c = intersect(rr, box(a.getBoundingClientRect()));
              if (!c) {
                cropped = true;
                break;
              }
              rr = c;
            }
          }
          anc = parent[anc];
        }
        if (cropped) continue;
        const v = intersect(rr, frameClip);
        if (!v || v[2] < lim.min_w || v[3] < lim.min_h) continue;
        recs.push({
          el: n, tag: "#text", role: "", parent: p, node: ni, doc: docI, rect: v, kind: "t", name,
          idx: null, ty: null, href: null, checked: false, expanded: null, val: null, editable: false,
        });
      }
    };

    // The page clips to its viewport; a frame of another origin to what its parent
    // handed it: its iframe's content box, cut to the parent's own clip.
    if (isTop) {
      mine.clip = [-lim.margin, -lim.margin, vw + 2 * lim.margin, vh + 2 * lim.margin];
      mine.area = vw * vh;
    }
    const origin = mine.origin;
    walkDoc(document, 0, origin, mine.clip, 0);

    // -- occlusion ----------------------------------------------------------
    // element.rs: a big record (>= 12% of the page's viewport) painted above another
    // one's centre, and not its kin, hides it. Here the hit test says what is on top.
    // Paint order is per frame there, and a hit test cannot leave its frame here.
    const hidden = new Set();
    if (bool("occlusion", "enabled", true)) {
      const minArea = num("occlusion", "min_occluder_viewport_fraction", 0.12) * mine.area;
      const big = new Map();
      recs.forEach((r, i) => {
        if (r.el.nodeType === 1 && r.rect[2] * r.rect[3] >= minArea) big.set(r.el, i);
      });
      if (big.size) {
        const hitAt = (x, y) => {
          let d = document;
          let hit = d.elementFromPoint(x, y);
          for (let guard = 0; hit && guard < 32; guard++) {
            const sr = shadowOf(hit);
            if (sr) {
              const inner = sr.elementFromPoint(x, y);
              if (inner && inner !== hit) {
                hit = inner;
                continue;
              }
            }
            const tag = hit.localName;
            if (tag === "iframe" || tag === "frame") {
              let cd = null;
              try {
                cd = hit.contentDocument;
              } catch (e) {
                cd = null;
              }
              if (cd) {
                const fr = hit.getBoundingClientRect();
                const inner = cd.elementFromPoint(x - fr.left, y - fr.top);
                if (inner) {
                  x -= fr.left;
                  y -= fr.top;
                  d = cd;
                  hit = inner;
                  continue;
                }
              }
            }
            break;
          }
          return hit;
        };
        const up = (n) => {
          if (!n) return null;
          if (n.parentNode && n.parentNode.nodeType === 11) return n.parentNode.host;
          if (n.parentNode && n.parentNode.nodeType === 9) return n.parentNode.defaultView && n.parentNode.defaultView.frameElement;
          return n.parentNode;
        };
        recs.forEach((r, i) => {
          const me = r.el.nodeType === 1 ? r.el : r.el.parentNode;
          // the record's centre, in this frame's own CSS px
          const hit = hitAt((r.rect[0] + r.rect[2] / 2 - origin[0]) / dpr, (r.rect[1] + r.rect[3] / 2 - origin[1]) / dpr);
          for (let h = hit, guard = 0; h && guard < 400; h = up(h), guard++) {
            if (h === me) break;
            const j = big.get(h);
            if (j === undefined || j === i) continue;
            if (!(h.contains(me) || me.contains(h))) hidden.add(i);
            break;
          }
        });
      }
    }
    const nOccluded = hidden.size;
    for (const i of hidden) {
      if (recs[i].kind === "a") recs[i].kind = recs[i].name ? "t" : "x";
    }
    const out = [];
    for (const r of recs) {
      if (r.kind === "x") continue;
      const { el, ...plain } = r;
      out.push(plain);
    }
    return {
      path: mine.path,
      recs: out,
      kids: kids.map(kidOut),
      occluded: nOccluded,
      vw,
      vh,
      dpr,
      url: location.href,
      ms: performance.now() - t0,
    };
  }
}

// ============================================================ assembling (service worker)

// What element.rs does once every session is walked: the noise filter, [1], the
// numbering, the tree text, and the pruning browser.rs applies before the model sees
// it. `frames` are the page and the frames of another origin that were read, in
// element.rs order (the page, then level by level); `vw`/`vh` the page's viewport.
function elementAssemble(frames, cfg, vw, vh) {
  let kept = [];
  frames.forEach((f, i) => {
    for (const r of f.recs) {
      r.frame = i;
      kept.push(r);
    }
  });
  const keyOf = (r) => `${r.frame}:${r.doc}:${r.node}`;
  const parentKey = (r) => `${r.frame}:${r.doc}:${r.parent}`;

  // -- noise ----------------------------------------------------------------
  // Drop marks that carry nothing an interactive ancestor does not already say,
  // and reparent survivors onto the nearest surviving ancestor.
  let nNoise = 0;
  if (elementBool(cfg, "noise", "enabled", true)) {
    const dropUnnamed = elementBool(cfg, "noise", "drop_nested_unnamed", true);
    const dropSame = elementBool(cfg, "noise", "drop_nested_same_name", true);
    const dropGeneric = elementBool(cfg, "noise", "drop_unnamed_generic", true);
    if (dropUnnamed || dropSame || dropGeneric) {
      const byNode = new Map(kept.map((r, k) => [keyOf(r), k]));
      const upRec = (k) => {
        const p = byNode.get(parentKey(kept[k]));
        return p !== undefined && p !== k ? p : null;
      };
      const drop = new Array(kept.length).fill(false);
      for (let k = 0; k < kept.length; k++) {
        const r = kept[k];
        if (r.kind !== "a") continue;
        if (r.tag === "input" || r.tag === "textarea" || r.tag === "select") continue;
        if (dropGeneric && !r.name && r.href === null && r.val === null &&
            (r.role === "" || r.role === "generic" || r.role === "none" || r.role === "presentation")) {
          drop[k] = true;
          continue;
        }
        let cur = k;
        let anc = null;
        for (let g = 0; g < 64; g++) {
          const p = upRec(cur);
          if (p === null) break;
          cur = p;
          if (kept[p].kind === "a") {
            anc = p;
            break;
          }
        }
        if (anc === null) continue;
        if (r.href !== null && r.href !== kept[anc].href) continue;
        const unnamed = !r.name;
        const same = !unnamed && r.name === kept[anc].name;
        if ((unnamed && dropUnnamed) || (same && dropSame)) drop[k] = true;
      }
      const reparent = kept.map((r) => r.parent);
      for (let k = 0; k < kept.length; k++) {
        if (drop[k]) continue;
        let cur = k;
        let par = -1;
        for (let g = 0; g < 200; g++) {
          const p = upRec(cur);
          if (p === null) break;
          if (!drop[p]) {
            par = kept[p].node;
            break;
          }
          cur = p;
        }
        reparent[k] = par;
      }
      kept.forEach((r, k) => {
        r.parent = reparent[k];
      });
      nNoise = drop.filter(Boolean).length;
      kept = kept.filter((_, k) => !drop[k]);
    }
  }

  // [1] is always the page itself: the surface `scroll` acts on, with the viewport rect.
  kept.unshift({
    tag: "page", role: "", parent: -1, node: -1, doc: 0, frame: 0, rect: [0, 0, vw, vh], kind: "v",
    name: "", idx: 1, ty: null, href: null, checked: false, expanded: null, val: null, editable: false,
  });

  // Number interactive elements and substantial text in document order.
  const maxElements = elementNum(cfg, "limits", "max_elements", 300);
  let n = 1;
  for (const r of kept) {
    if (r.kind === "v") continue;
    const substantial = r.kind === "t" && r.name && Array.from(r.name).length >= 12 &&
      r.rect[2] >= 40 && r.rect[3] >= 12 && r.rect[2] * r.rect[3] >= 800;
    if (!(r.kind === "a" || substantial)) continue;
    if (n < maxElements) {
      r.idx = n + 1;
      n += 1;
    } else if (r.kind === "a") {
      r.kind = "t";
    }
  }

  // -- render -----------------------------------------------------------
  const lineOf = (r) => {
    if (r.kind === "a" && r.idx !== null) {
      const bits = [];
      if (r.ty !== null) bits.push(`type="${r.ty}"`);
      if (r.role && r.role !== r.tag) bits.push(`role="${r.role}"`);
      if (r.href !== null) bits.push(`href="${r.href}"`);
      if (r.val !== null) bits.push(`value="${r.val}"`);
      if (r.checked) bits.push("checked");
      if (r.expanded !== null) bits.push(r.expanded ? "expanded" : "collapsed");
      if (r.editable) bits.push("editable");
      const head = bits.length ? `${r.tag} ${bits.join(" ")}` : r.tag;
      const body = r.name ? `<${head}>${r.name}</${r.tag}>` : `<${head} />`;
      return `[${r.idx}] ${body}`;
    }
    if (r.kind === "l") {
      const head = r.role ? `${r.tag} role="${r.role}"` : r.tag;
      return r.name ? `<${head}>${r.name}</${r.tag}>` : `<${head}>`;
    }
    if (r.kind === "v" && r.idx !== null) return `[${r.idx}] <page scrollable>the whole page</page>`;
    if (r.kind === "t" && r.idx !== null) return `[${r.idx}] <text>${r.name}</text>`;
    return r.name;
  };
  const at = new Map(kept.map((r, k) => [keyOf(r), k]));
  const depth = new Array(kept.length).fill(-1);
  for (let k = 0; k < kept.length; k++) {
    if (depth[k] !== -1) continue;
    const chain = [];
    let cur = k;
    let d;
    for (;;) {
      if (depth[cur] !== -1) {
        d = depth[cur];
        break;
      }
      chain.push(cur);
      const p = at.get(parentKey(kept[cur]));
      if (p !== undefined && p !== cur && chain.length < 200) {
        cur = p;
      } else {
        d = -1;
        break;
      }
    }
    for (const c of chain.reverse()) {
      d = d === -1 ? 0 : d + 1;
      depth[c] = d;
    }
  }
  const lines = ["<element>"];
  let prevFrame = null;
  kept.forEach((r, k) => {
    if (prevFrame !== null && prevFrame !== r.frame) lines.push("  <frame>");
    prevFrame = r.frame;
    const txt = lineOf(r);
    if (txt) lines.push("  ".repeat(depth[k] + 1) + txt);
  });
  lines.push("</element>");

  const numbered = kept.filter((r) => r.idx !== null);
  return {
    tree: elementPrune(lines.join("\n").trim()),
    count: n,
    noise: nNoise,
    hits: numbered.map((r) => [r.idx, r.rect]).sort((a, b) => a[0] - b[0]),
    marks: numbered.map((r) => ({ idx: r.idx, kind: r.kind, rect: r.rect })),
  };
}

// browser.rs prune_empty_containers(): a container line with nothing the model can
// see goes. It earns its line only if a NUMBERED line sits beneath it (deeper indent,
// before the tree returns to its level).
function elementPrune(tree) {
  const lines = tree.split("\n");
  const depth = (ln) => (ln.length - ln.replace(/^ +/, "").length) / 2;
  const bare = (ln) => {
    const s = ln.trim();
    return s.startsWith("<") && !s.includes("</") && !s.endsWith("/>") &&
      s !== "<element>" && s !== "</element>" && s !== "<frame>";
  };
  const keep = [];
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    if (!bare(ln)) {
      keep.push(ln);
      continue;
    }
    const d = depth(ln);
    let earned = false;
    for (let j = i + 1; j < lines.length; j++) {
      const s = lines[j].trim();
      if (s === "<element>" || s === "</element>") break;
      if (s !== "<frame>" && depth(lines[j]) <= d) break;
      if (s.startsWith("[")) {
        earned = true;
        break;
      }
    }
    if (earned) keep.push(ln);
  }
  return keep.join("\n");
}

// ============================================================ marks (service worker)

const ELEMENT_PAL = [[225, 29, 72], [14, 165, 233], [22, 163, 74], [217, 119, 6], [124, 58, 237], [8, 145, 178]];
// 3x5 bitmap digits, as element.rs draws them
const ELEMENT_DIGITS = [
  [0b111, 0b101, 0b101, 0b101, 0b111], [0b010, 0b110, 0b010, 0b010, 0b111],
  [0b111, 0b001, 0b111, 0b100, 0b111], [0b111, 0b001, 0b111, 0b001, 0b111],
  [0b101, 0b101, 0b111, 0b001, 0b001], [0b111, 0b100, 0b111, 0b001, 0b111],
  [0b111, 0b100, 0b111, 0b101, 0b111], [0b111, 0b001, 0b001, 0b001, 0b001],
  [0b111, 0b101, 0b111, 0b101, 0b111], [0b111, 0b101, 0b111, 0b001, 0b111],
];

// Marks are painted onto the image, never into the page. Rects and the capture are
// both device pixels of the same viewport, so they land 1:1.
async function elementDrawMarks(jpegB64, marks) {
  const bytes = Uint8Array.from(atob(jpegB64), (c) => c.charCodeAt(0));
  const bmp = await createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
  const canvas = new OffscreenCanvas(bmp.width, bmp.height);
  const g = canvas.getContext("2d");
  g.drawImage(bmp, 0, 0);
  const stroke = 2;
  const sc = 2;
  const fill = (c, x, y, w, h) => {
    g.fillStyle = `rgb(${c[0]},${c[1]},${c[2]})`;
    g.fillRect(x, y, w, h);
  };
  for (const m of marks) {
    if (m.kind === "v") continue;
    const i = m.idx;
    const c = ELEMENT_PAL[i % ELEMENT_PAL.length];
    const x0 = Math.round(m.rect[0]);
    const y0 = Math.round(m.rect[1]);
    const x1 = Math.round(m.rect[0] + m.rect[2]);
    const y1 = Math.round(m.rect[1] + m.rect[3]);
    fill(c, x0, y0, x1 - x0 + 1, stroke);
    fill(c, x0, y1 - stroke + 1, x1 - x0 + 1, stroke);
    fill(c, x0, y0, stroke, y1 - y0 + 1);
    fill(c, x1 - stroke + 1, y0, stroke, y1 - y0 + 1);

    // index badge inside the box, top-left, shrunk to fit a tiny element
    const label = String(i);
    let scl = sc;
    let lw = label.length * (4 * sc) + 2 * sc;
    let lh = 5 * sc + 2 * sc;
    const bw = Math.max(x1 - x0, 0);
    const bh = Math.max(y1 - y0, 0);
    if (lw > bw || lh > bh) {
      const fit = Math.min(1, Math.max(0.35, Math.min(Math.max(bw, 1) / lw, Math.max(bh, 1) / lh)));
      scl = Math.max(1, Math.round(sc * fit));
      lw = label.length * (4 * scl) + 2 * scl;
      lh = 5 * scl + 2 * scl;
    }
    fill(c, x0, y0, lw, lh);
    g.fillStyle = "rgb(255,255,255)";
    for (let di = 0; di < label.length; di++) {
      const d = ELEMENT_DIGITS[label.charCodeAt(di) - 48];
      if (!d) continue;
      const ox = x0 + scl + di * (4 * scl);
      const oy = y0 + scl;
      for (let row = 0; row < 5; row++) {
        for (let col = 0; col < 3; col++) {
          if (d[row] & (1 << (2 - col))) g.fillRect(ox + col * scl, oy + row * scl, scl, scl);
        }
      }
    }
  }
  const blob = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.8 });
  const buf = new Uint8Array(await blob.arrayBuffer());
  let s = "";
  for (let k = 0; k < buf.length; k += 0x8000) s += String.fromCharCode.apply(null, buf.subarray(k, k + 0x8000));
  return btoa(s);
}

// ============================================================ one scan

// Settle, read every frame of the tab while photographing it, then assemble.
// `opts.overlay` is merged over the defaults the way element.config.json is;
// `opts.marks` false skips the marks; `opts.screenshot` false skips the picture.
async function elementScan(tab, opts = {}) {
  const t0 = performance.now();
  const settled = await elementSettle(tab.id);
  const base = elementMerge(structuredClone(ELEMENT_DEFAULT_CONFIG), opts.overlay || {});
  const cfg = elementConfigFor(base, tab.url || "");
  const a = performance.now();
  // The picture is taken while the frames are read, so pixels and geometry are from
  // one moment (element.rs captures right behind the root document's snapshot). The tab
  // on show is Chrome's own capture; a tab in the background is photographed another way
  // when the caller has one (tools.js: through the debugger, as element.rs photographs
  // any tab), so the scan never has to bring it to the front.
  const shotP = opts.screenshot === false ? null
    : tab.active ? chrome.tabs.captureVisibleTab(tab.windowId, { format: "jpeg", quality: 75 }).catch((e) => e)
    : opts.fallback ? opts.fallback.capture(tab).catch((e) => e)
    : null;
  let results = null;
  let failure = null;
  const scan = { nonce: `${Date.now()}.${Math.random()}`, wait_ms: ELEMENT_FRAME_WAIT_MS };
  // tools.js toolsWatchScan: told just ahead of the read, and when the read is back.
  if (opts.watch) opts.watch.ahead();
  try {
    results = await chrome.scripting.executeScript({
      target: { tabId: tab.id, allFrames: true },
      // Into every frame as it is now. Without this Chrome runs the script in a frame
      // only once its document goes idle, and one that never does (an ad slot's
      // javascript:false src, a request that never answers) held the whole scan past
      // its 40 s (measured on macrotrends.net, which has both).
      injectImmediately: true,
      func: elementScanPage,
      args: [cfg, scan],
    });
  } catch (e) {
    failure = e;
  }
  // A page the extension may not script (about:blank: the agent's blank page with
  // the logo painted in) is read another way when the caller has one (tools.js:
  // through the debugger, top frame only).
  if (failure && opts.fallback && /Cannot access|error page/i.test(String((failure && failure.message) || failure))) {
    results = await opts.fallback.read(tab, cfg, scan);
    failure = null;
  }
  if (opts.watch) opts.watch.back();
  const scanMs = performance.now() - a;
  let captured = shotP ? await shotP : null;
  if (shotP && tab.active && typeof captured !== "string" && opts.fallback) {
    captured = await opts.fallback.capture(tab).catch((e) => e);
  }
  if (failure) throw new Error(`scan failed: ${(failure && failure.message) || failure}`);

  // One result per frame Chrome could inject into; same-origin children answered
  // {covered} and were read by their parent.
  const byPath = new Map();
  for (const r of results) {
    const v = r && r.result;
    if (v && v.path) byPath.set(v.path, v);
  }
  const top = byPath.get("0");
  if (!top || !top.recs) throw new Error(`scan failed: ${top && top.error ? top.error : "no result from the page"}`);
  // element.rs order: the page, then every frame of another origin level by level,
  // each in its parent's document order. A frame with no layout, off screen, or one
  // Chrome could not inject into is skipped, as element.rs skips a session it cannot
  // place or read.
  const frames = [top];
  let sessions = 1;
  let skipped = 0;
  for (let q = 0; q < frames.length; q++) {
    for (const k of frames[q].kids || []) {
      sessions += 1;
      const r = k.skipped ? null : byPath.get(k.path);
      if (r && r.recs) frames.push(r);
      else skipped += 1;
    }
  }
  const out = elementAssemble(frames, cfg, top.vw, top.vh);
  const occluded = frames.reduce((s, f) => s + (f.occluded || 0), 0);

  let shot = null;
  let plain = null;
  let shotMs = 0;
  if (shotP) {
    const b = performance.now();
    if (typeof captured !== "string") {
      throw new Error(`screenshot failed: ${captured && captured.message ? captured.message : captured}`);
    }
    plain = captured.slice(captured.indexOf(",") + 1);
    shot = opts.marks === false ? plain : await elementDrawMarks(plain, out.marks);
    shotMs = performance.now() - b;
  }
  return {
    tree: out.tree,
    summary: `${out.count} interactive, ${settled} ms settle, ${sessions} sessions, ` +
      `${skipped} frames skipped, ${occluded} occluded, ${out.noise} noise`,
    count: out.count,
    sessions,
    skipped,
    occluded,
    noise: out.noise,
    url: top.url,
    settled_ms: settled,
    hits: out.hits,
    dpr: top.dpr,
    screenshot: shot,
    screenshot_plain: plain,
    // shot_ms is what the picture still cost once the frames were read: the capture
    // ran alongside them, so this is mostly the marks.
    timings: {
      settle_ms: settled, scan_ms: scanMs, page_ms: top.ms, frames: frames.length, shot_ms: shotMs,
      total_ms: performance.now() - t0,
    },
  };
}
