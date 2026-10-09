/* The "agent is driving" overlay — builder, the edge glow, and the agent's
   cursor. Styling lives beside this in glow.css; glow.html renders
   both on their own so you can tweak the look without running the agent.

   browser.rs registers this as a document-start script on every tab, so it
   runs at the birth of every document — the overlay is part of the first
   paint on every navigation, including ones the agent causes by clicking a
   link. When injected it is handed the stylesheet text in AutoCua_CSS and
   adopts it into the document (a constructed stylesheet, which page CSP
   cannot block); glow.html instead links glow.css itself and leaves
   AutoCua_CSS undefined.

   Everything it adds is one fixed, pointer-events:none container that takes
   part in no layout, so the page underneath is untouched and the scanner sees
   the same tree with the overlay present or absent. */

(function () {
  // TOP FRAME ONLY. Page.addScriptToEvaluateOnNewDocument runs this at the
  // birth of every same-process frame, not just the tab's main document — so
  // without this guard a same-origin iframe (YouTube's live-chat panel, an
  // embedded checkout form) grows its own full set of edges
  // sized to the panel. The glow marks the BROWSER as driven, so it belongs
  // on the viewport alone; cross-origin iframes are separate CDP targets
  // that never get armed, and this makes same-process ones match. The
  // try/catch treats any exotic frame that hides window.top as framed.
  try {
    if (window.self !== window.top) return;
  } catch (e) {
    return;
  }
  if (window.__AutoCuaOverlay) {
    // Armed again over a document that already has the overlay: a new run
    // taking the browser over, or a controller re-arming its tabs. Scraping
    // mode never carries over from before; browser.rs switches it on for the
    // read in progress.
    if (typeof window.__AutoCuaScrapeGlow === 'function') window.__AutoCuaScrapeGlow(false);
    return;
  }

  var reduceMotion = false;
  try {
    reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch (e) {}

  // -- the layer ------------------------------------------------------------

  var layer = document.createElement('div');
  layer.className = 'AutoCua-layer';
  // aria-hidden and inert: decoration must not reach the accessibility tree,
  // which is where the scanner reads the page from.
  layer.setAttribute('aria-hidden', 'true');
  layer.setAttribute('data-AutoCua', 'overlay');

  ['top', 'bottom', 'left', 'right'].forEach(function (side) {
    var edge = document.createElement('div');
    edge.className = 'AutoCua-edge AutoCua-edge-' + side;
    layer.appendChild(edge);
  });

  // The agent's pointer, assembled with the DOM API rather than written as
  // markup: a page that enforces Trusted Types (Google's do, with
  // `require-trusted-types-for 'script'`) throws on any string assigned to
  // innerHTML, and that one throw used to end this script before the layer
  // was attached or a single cursor function existed - no glow, no cursor,
  // no press animation on those sites. The gradient is namespaced as
  // AutoCua-* because url(#id) resolves against the WHOLE document, and a
  // page is entitled to own an id called "lavender-flow".
  var SVG_NS = 'http://www.w3.org/2000/svg';
  function svgEl(name, attrs) {
    var el = document.createElementNS(SVG_NS, name);
    for (var k in attrs) el.setAttribute(k, attrs[k]);
    return el;
  }
  function gradientStop(offset, color, values) {
    var stop = svgEl('stop', { offset: offset, 'stop-color': color });
    stop.appendChild(svgEl('animate', {
      attributeName: 'stop-color', values: values, dur: '2.4s', repeatCount: 'indefinite'
    }));
    return stop;
  }
  function cursorSvg() {
    var svg = svgEl('svg', { viewBox: '0 0 32 32' });
    var defs = svgEl('defs', {});
    var grad = svgEl('linearGradient', { id: 'AutoCua-lavender-flow', x1: '0', y1: '0', x2: '1', y2: '1' });
    grad.appendChild(gradientStop('0', '#b6a3eb', '#b6a3eb;#8f7bc9;#d6ccf5;#b6a3eb'));
    grad.appendChild(gradientStop('0.5', '#8f7bc9', '#8f7bc9;#d6ccf5;#b6a3eb;#8f7bc9'));
    grad.appendChild(gradientStop('1', '#d6ccf5', '#d6ccf5;#b6a3eb;#8f7bc9;#d6ccf5'));
    defs.appendChild(grad);
    svg.appendChild(defs);
    svg.appendChild(svgEl('path', {
      d: 'M8 4 C5 2 3 3 3 7 L4 24 C4 28 7 28 9 25 L13 20 C14 19 15 18 17 18 L24 18 C28 18 29 15 25 13 Z',
      fill: 'none', stroke: 'url(#AutoCua-lavender-flow)', 'stroke-width': '2.5',
      'stroke-linejoin': 'round', 'stroke-linecap': 'round'
    }));
    return svg;
  }

  var cursor = document.createElement('div');
  cursor.className = 'AutoCua-cursor';
  var bubbleEl = document.createElement('div');
  bubbleEl.className = 'AutoCua-bubble';
  var wordEl = document.createElement('span');
  wordEl.className = 'AutoCua-bubble-word';
  // The words sit whole behind a window that opens left to right over them
  // (sayNext): the bubble widens with it, one glide per word.
  var clipEl = document.createElement('div');
  clipEl.className = 'AutoCua-bubble-clip';
  clipEl.appendChild(wordEl);
  bubbleEl.appendChild(clipEl);
  var art = document.createElement('div');
  art.className = 'AutoCua-cursor-art';
  art.appendChild(cursorSvg());
  cursor.appendChild(bubbleEl);
  cursor.appendChild(art);
  layer.appendChild(cursor);

  var bubble = cursor.querySelector('.AutoCua-bubble');
  var bubbleWord = cursor.querySelector('.AutoCua-bubble-word');
  var bubbleClip = clipEl;

  // What installStyle added, and whether the overlay was switched off (__AutoCuaGlowOff).
  var styleSheet = null, styleEl = null, off = false;

  function installStyle() {
    if (typeof AutoCua_CSS !== 'string' || !AutoCua_CSS) return;   // glow.html links it instead
    try {
      var sheet = new CSSStyleSheet();
      sheet.replaceSync(AutoCua_CSS);
      document.adoptedStyleSheets = document.adoptedStyleSheets.concat(sheet);
      styleSheet = sheet;
    } catch (e) {
      try {
        var el = document.createElement('style');
        el.textContent = AutoCua_CSS;
        (document.head || document.documentElement).appendChild(el);
        styleEl = el;
      } catch (e2) {}
    }
  }

  function attach() {
    if (off) return false;
    var host = document.body || document.documentElement;
    if (host && !layer.isConnected) host.appendChild(layer);
    return layer.isConnected;
  }

  // -- keeping the layer on the page ----------------------------------------

  // A page that rebuilds its body (SPA route change, framework hydration) can
  // take the layer with it; putting it back costs one property read a frame.
  function tick() {
    if (off) return;
    if (!layer.isConnected) attach();
    requestAnimationFrame(tick);
  }

  // -- the agent's cursor ---------------------------------------------------

  // Where the arrow's TIP sits inside its own 26px box. The path starts at
  // roughly (3,3) in a 32-unit viewBox, which is ~2.4px once scaled down, so
  // without this the controller's coordinates would land the box's top-left
  // corner on the target and the point itself would sit down and to the right
  // of everything it touches.
  var TIP = 2;

  // Set by the done tool, and cleared only by a real move. A sync must not
  // undo it: the controller re-places the cursor on every scan, so without
  // this the arrow would reappear one scan after the run ended.
  var dismissed = false;

  // Carried across same-origin navigations so the overlay comes back with the
  // page instead of waiting on a round trip from the controller. Best-effort
  // in every direction: sessionStorage throws outright in some sandboxes and
  // privacy modes, and is per-ORIGIN, so a hop to another site simply finds
  // nothing here and falls back to the controller's sync.
  var STORE = 'AutoCua-cursor';

  function remember() {
    try {
      window.sessionStorage.setItem(STORE, JSON.stringify({
        x: lastX, y: lastY, line: bubbleWord.textContent || ''
      }));
    } catch (e) {}
  }

  function recall() {
    try {
      var v = JSON.parse(window.sessionStorage.getItem(STORE) || 'null');
      return (v && typeof v.x === 'number' && typeof v.y === 'number') ? v : null;
    } catch (e) {
      return null;
    }
  }

  var lastX = 0, lastY = 0;

  function setPos(x, y) {
    lastX = x;
    lastY = y;
    cursor.style.setProperty(
      'transform',
      'translate3d(' + (x - TIP) + 'px,' + (y - TIP) + 'px,0)',
      'important'
    );
    remember();
  }

  function viewportCentre() {
    var w = window.innerWidth || layer.clientWidth || 0;
    var h = window.innerHeight || layer.clientHeight || 0;
    if (!w || !h) return null;
    return [Math.round(w / 2), Math.round(h / 2)];
  }

  // Put the arrow somewhere WITHOUT gliding there, then reveal it.
  //
  // This is what carries the cursor across documents. glow.js runs afresh in
  // every new tab and after every navigation, so the position lives on the
  // controller's side and is replayed in here; teleporting rather than gliding
  // is the point, because the arrow did not travel across that boundary, it
  // was already there.
  function placeInstant(x, y) {
    cursor.style.setProperty('transition', 'none', 'important');
    setPos(x, y);
    // Force the untransitioned transform to commit before the glide is handed
    // back, or the browser coalesces both changes and animates the teleport
    // after all. Synchronous on purpose: the obvious alternative is a pair of
    // requestAnimationFrame callbacks, and rAF does not run in a BACKGROUND
    // tab — which is exactly where a sync lands most often, since the agent
    // syncs every tab it has open and can only be looking at one of them.
    // That would leave the arrow in every other tab permanently invisible.
    void cursor.offsetWidth;
    cursor.style.removeProperty('transition');
    if (!dismissed) cursor.classList.add('AutoCua-cursor-live');
  }

  // Replay a known position into this document. Null coordinates mean "no
  // position recorded yet" — the controller has not moved the cursor even
  // once — so it opens at the centre of the viewport rather than nowhere.
  window.__AutoCuaCursorPlace = function (x, y) {
    var at = (x === null || x === undefined || y === null || y === undefined)
      ? viewportCentre()
      : [x, y];
    if (!at) return false;              // no viewport yet; the next scan retries
    placeInstant(at[0], at[1]);
    return true;
  };

  // Send the tip to a viewport point, gliding. The travel is the stylesheet's
  // transition; the controller waits CURSOR_MOVE_SECONDS after calling this,
  // so the arrow has arrived before the press is dispatched. Returns whether
  // it moved, and the controller skips its waits entirely if it did not — a
  // decoration that cannot be drawn must never hold up the action.
  window.__AutoCuaCursor = function (x, y) {
    // A real action means the run is going again, so an earlier done no longer
    // applies. Reveal without a glide if this document has never placed it.
    var fresh = !cursor.classList.contains('AutoCua-cursor-live');
    dismissed = false;
    if (fresh) {
      placeInstant(x, y);
    } else {
      setPos(x, y);
    }
    return true;
  };

  window.__AutoCuaCursorDown = function () {
    cursor.classList.add('AutoCua-cursor-down');
    return true;
  };

  window.__AutoCuaCursorUp = function () {
    cursor.classList.remove('AutoCua-cursor-down');
    return true;
  };

  // -- what it is thinking --------------------------------------------------

  // A phrase at a time, revealed left to right.
  //
  // These calls do NOT stream: the model's reply lands whole, so there is no
  // token feed to follow. What arrives is a finished block of text and this
  // plays it out at reading pace, which is why the pacing is ours to pick
  // rather than the network's. Fast on purpose: a step is often over in a
  // couple of seconds, and words that arrive after it are words about
  // something that already happened.
  //
  // A line (up to WORDS_PER_LINE words) is laid in whole, and the window
  // over it (the clip) opens across it in one steady sweep, left to right,
  // at a constant speed per letter: the letters come in as one smooth
  // motion with a soft edge, and the bubble widens with them. Setting the
  // text a letter at a time made the bubble jump a few pixels every 26 ms
  // (measured: 94 jumps in one thought, then a snap back to nothing at every
  // new line), which is what read as jitter; a glide per word still pulsed.
  //
  // A finished line is held long enough to actually be read, then it fades
  // as the bubble closes, and the next line opens from the left. Five words
  // that stay put are a phrase, and a phrase is the smallest thing that
  // carries a thought.
  var WORDS_PER_LINE = 5;
  var MS_PER_CHAR = 28;      // the sweep's pace, spaces included
  var LINE_HOLD_MS = 800;    // how long a finished line stays before it clears
  var CLEAR_MS = 200;        // the line fading out as the bubble closes
  // px: the window's soft right edge, which is also the bubble's right
  // padding (glow.css), so at rest the fade falls where there is no text.
  var EDGE = 10;
  var CLOSE = 'cubic-bezier(0.4, 0, 0.2, 1)';

  var words = [];          // still to say, in order
  var line = '';           // what is on screen right now
  var wordTimer = null;

  function stopTimers() {
    if (wordTimer) { clearTimeout(wordTimer); wordTimer = null; }
  }

  // Open (or close) the window to `px` over `ms` with `timing`; 0 is at once.
  function openTo(px, ms, timing) {
    if (reduceMotion) ms = 0;
    bubbleClip.style.setProperty('transition', ms ? 'width ' + ms + 'ms ' + timing : 'none', 'important');
    bubbleClip.style.setProperty('width', px + 'px', 'important');
  }

  // The window's width with the whole line in view. offsetWidth, not the
  // bounding box: the bubble pops in scaled, and the box would be too.
  function lineWidth() {
    return bubbleWord.offsetWidth + EDGE;
  }

  // The line shown in full at once (a new document carrying it over).
  function fitNow() {
    if (line) openTo(lineWidth(), 0);
  }

  // Stop what is queued. The line on screen stays, so the next thought can
  // clear it smoothly instead of it vanishing.
  function bubbleReset() {
    stopTimers();
    words.length = 0;
  }

  function bubbleHide() {
    bubbleReset();
    line = '';
    bubbleWord.textContent = '';
    openTo(0, 0);
    bubble.classList.remove('AutoCua-bubble-on');
    remember();
  }

  function sayNext() {
    if (!words.length) {
      // Everything in this packet has been said. The line STAYS on screen:
      // running out of words means the agent is between packets, not finished,
      // and a bubble that blinked out every time would read as "stopped" at
      // exactly the moments it is thinking hardest. Only done clears it.
      return;
    }
    if (line) {
      // The line on screen fades as the bubble closes; the next one then
      // opens from the left.
      line = '';
      bubbleWord.style.setProperty('opacity', '0', 'important');
      openTo(0, CLEAR_MS, CLOSE);
      wordTimer = setTimeout(sayNext, reduceMotion ? 0 : CLEAR_MS);
      return;
    }
    line = words.splice(0, WORDS_PER_LINE).join(' ');
    bubbleWord.textContent = line;
    bubbleWord.style.setProperty('opacity', '1', 'important');
    bubble.classList.add('AutoCua-bubble-on');
    var ms = line.length * MS_PER_CHAR;
    openTo(lineWidth(), ms, 'linear');
    remember();                          // so a navigation resumes mid-thought
    // Held, the last line of a thought as much as any: without the hold the
    // last thing the agent said would clear the moment it was shown.
    wordTimer = setTimeout(sayNext, ms + LINE_HOLD_MS);
  }

  // Hand the bubble a block of text to work through. Replaces whatever was
  // still queued rather than appending: this is called once per step, and the
  // leftovers of the previous step are, by definition, about a page state that
  // is already gone. Better to drop them than to narrate the wrong thing.
  window.__AutoCuaSay = function (text) {
    bubbleReset();
    words = String(text == null ? '' : text)
      .split(/\s+/)
      .map(function (w) {
        // Punctuation is noise a word at a time — "PLAN:" and "PLAN" say the
        // same thing, and the colon only costs a letter of typing time.
        return w.replace(/^[^\w'-]+|[^\w'-]+$/g, '');
      })
      .filter(function (w) { return w.length > 0; });
    if (!words.length) { bubbleHide(); return false; }
    if (dismissed) { words.length = 0; return false; }   // the run already ended
    sayNext();
    return true;
  };

  window.__AutoCuaSayStop = function () {
    bubbleHide();
    return true;
  };

  // The run is over: fade the arrow out and keep it out.
  window.__AutoCuaCursorHide = function () {
    dismissed = true;
    bubbleHide();
    cursor.classList.remove('AutoCua-cursor-live');
    cursor.classList.remove('AutoCua-cursor-down');
    return true;
  };

  // -- "did anything actually move?" ---------------------------------------

  // Where the surfaces under a point currently sit, as [innerX, innerY,
  // pageX, pageY]. The scroll tool reads it before and after a wheel: same
  // numbers means nothing moved, which is a fact the agent cannot otherwise
  // recover — it is never shown the previous screenshot.
  //
  // Both the nearest scroller AND the document are reported because a wheel
  // chains: a panel already at its end passes the scroll to the page, and
  // that is still something moving.
  window.__AutoCuaScrollProbe = function (x, y) {
    try {
      var doc = document.scrollingElement || document.documentElement;
      var inner = null;
      // The overlay is pointer-events:none, so this returns the page's own
      // element rather than anything of ours.
      for (var n = document.elementFromPoint(x, y); n && n !== doc; n = n.parentElement) {
        var cs = getComputedStyle(n);
        var canY = /^(auto|scroll|overlay)$/.test(cs.overflowY) &&
                   n.scrollHeight > n.clientHeight + 1;
        var canX = /^(auto|scroll|overlay)$/.test(cs.overflowX) &&
                   n.scrollWidth > n.clientWidth + 1;
        if (canY || canX) { inner = n; break; }
      }
      return [
        inner ? Math.round(inner.scrollLeft) : -1,
        inner ? Math.round(inner.scrollTop) : -1,
        Math.round(doc.scrollLeft),
        Math.round(doc.scrollTop)
      ];
    } catch (e) {
      return null;
    }
  };

  // -- scraping mode ----------------------------------------------------------

  // The look of scraping mode (glow.css "scraping mode"), switched on by
  // browser.rs as `scrape` starts reading this page and off when the mode
  // ends. Built the first time it is asked for, so a page that is never
  // scraped carries none of it; inside `layer`, so __AutoCuaGlowOff takes it
  // with the rest; and under the cursor, so the arrow and its bubble stay on
  // top. Every switch is instant: the step after exit_scrape_mode photographs
  // the page, and a fade still on screen would be in that picture.
  //
  // The canvases are what glow.css cannot draw: the orb beside the label and
  // the read's data in the card. One animation-frame loop draws both, and it
  // runs only while the mode is on.
  var scrapeEl = null, scraping = false, scrapeReady = false;
  var scrapeRaf = 0, scrapeLast = 0;
  var orbCanvas = null, orbCtx = null, orbPx = 26, orbDpr = 1, orbRgb = [232, 232, 238];
  var ORB_SPEED = 3.315;      // the orb's "connecting" preset

  // "#9b7dff" -> [155, 125, 255], or `fallback` when a colour cannot be read.
  function hexRgb(value, fallback) {
    var m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(String(value || '').trim());
    return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : fallback;
  }

  function scrapeBuild() {
    var root = document.createElement('div');
    root.className = 'AutoCua-scrape';
    var glow = document.createElement('div');
    glow.className = 'AutoCua-scrape-glow';
    var status = document.createElement('div');
    status.className = 'AutoCua-scrape-status';
    var head = document.createElement('div');
    head.className = 'AutoCua-scrape-head';
    orbCanvas = document.createElement('canvas');
    orbCanvas.className = 'AutoCua-scrape-orb';
    // Empty: glow.css writes "Scraping" into it as CSS text, which no read of
    // the page's DOM or text, the scrape's own included, ever sees.
    var label = document.createElement('span');
    label.className = 'AutoCua-scrape-label';
    head.appendChild(orbCanvas);
    head.appendChild(label);
    // The read's data is painted on this canvas, never put in as text, for
    // the same reason.
    var feed = document.createElement('div');
    feed.className = 'AutoCua-scrape-feed';
    feedCanvas = document.createElement('canvas');
    feedCanvas.className = 'AutoCua-scrape-json';
    feedCanvas.width = 0;               // no bitmap until the card unfolds
    feedCanvas.height = 0;
    feed.appendChild(feedCanvas);
    status.appendChild(head);
    status.appendChild(feed);
    root.appendChild(glow);
    root.appendChild(status);
    layer.insertBefore(root, cursor);
    scrapeEl = root;
    cardEl = status;
    headEl = head;
    // A document never leaves the tab wearing the look: one kept in the
    // back/forward cache would come back with it on a later step, when no
    // scraping mode is there to take it off, and be photographed for the
    // model. pagehide, unlike unload, leaves the page fit for that cache. If
    // the mode is still on, the next scraping step puts the look back
    // (browser.rs scrape_glow_sync).
    window.addEventListener('pagehide', scrapePagehide);
  }

  // Named, so __AutoCuaGlowOff can take it away: a listener left on the
  // window would keep this whole overlay alive after it is switched off.
  function scrapePagehide() {
    if (scraping) scrapeGlow(false);
  }

  // Once, with the look on screen: the colours are glow.css's, so a tweak
  // there reaches the canvases.
  function scrapeCanvases() {
    var cs = getComputedStyle(scrapeEl);
    orbRgb = hexRgb(cs.getPropertyValue('--AutoCua-scrape-ink'), orbRgb);
    orbCtx = orbCanvas.getContext('2d');
    feedCtx = feedCanvas.getContext('2d');
    var panel = parseFloat(cs.getPropertyValue('--AutoCua-scrape-panel-h'));
    feedPanelH = panel > 0 ? panel : 460;
    feedInk = 'rgb(' + orbRgb[0] + ',' + orbRgb[1] + ',' + orbRgb[2] + ')';
    if (feedCtx) feedMeasure();
  }

  // Sized every time the look comes on, never kept from an earlier time: the
  // window may have been zoomed in between.
  function orbResize() {
    orbPx = parseFloat(getComputedStyle(orbCanvas).width) || 26;   // --AutoCua-scrape-orb
    orbDpr = Math.min(2, window.devicePixelRatio || 1);
    orbCanvas.width = Math.round(orbPx * orbDpr);
    orbCanvas.height = Math.round(orbPx * orbDpr);
  }

  /*
   * The orb beside the label: the "connecting" constellation from
   * thinking-orbs v0.3.2 (https://github.com/Jakubantalik/Libraries.dev),
   * ported from solving-orb-2.html. Change from the original: drawn in the
   * label's grey, with depth carried by opacity.
   *
   * MIT License
   * Copyright (c) 2026 Jakub Antalik
   *
   * Permission is hereby granted, free of charge, to any person obtaining a copy
   * of this software and associated documentation files (the "Software"), to deal
   * in the Software without restriction, including without limitation the rights
   * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
   * copies of the Software, and to permit persons to whom the Software is
   * furnished to do so, subject to the following conditions:
   *
   * The above copyright notice and this permission notice shall be included in all
   * copies or substantial portions of the Software.
   *
   * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
   * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
   * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
   * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
   * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
   * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
   * SOFTWARE.
   */
  var orbDraw = (function () {
    // The library's "connecting" preset, already resolved
    var OPTS = {
      nodeN: 41,              // constellation nodes
      signals: 7,             // packets travelling between nodes
      thr: 0.72,              // max link length (on the unit sphere)
      nodeR: 1.4 * 0.95,
      nodeRDepth: 1.8 * 0.95,
      lineW: 0.8,
      rsPow: 0.6,
      rMin: 0.3
    };

    function lerp(a, b, f) { return a + (b - a) * f; }
    function frac(x) { return x - Math.floor(x); }

    // Deterministic hash in [0, 1)
    function hashD(a, b) {
      var h = Math.sin(a * 12.9898 + b * 78.233) * 43758.5453;
      return h - Math.floor(h);
    }

    // Value noise on a 2D lattice: smooth, deterministic, cheap
    function vnoise(x, y) {
      var xi = Math.floor(x), yi = Math.floor(y);
      var fx = x - xi, fy = y - yi;
      fx = fx * fx * (3 - 2 * fx);
      fy = fy * fy * (3 - 2 * fy);
      var a = hashD(xi, yi), b = hashD(xi + 1, yi);
      var c = hashD(xi, yi + 1), d = hashD(xi + 1, yi + 1);
      return a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy;
    }

    // Evenly spread directions on a unit sphere (Fibonacci lattice)
    function fibDir(i, n) {
      var golden = Math.PI * (3 - Math.sqrt(5));
      var y = 1 - 2 * (i + 0.5) / n;
      var rad = Math.sqrt(1 - y * y);
      var a = i * golden;
      return [rad * Math.cos(a), y, rad * Math.sin(a)];
    }

    // Spin (yaw) + tilt + orthographic projection
    function makeProj(yaw, tilt, cx, cy, scale) {
      var st = Math.sin(tilt), ct = Math.cos(tilt);
      var sy = Math.sin(yaw), cyw = Math.cos(yaw);
      return function (x, y, z) {
        var x1 = x * cyw + z * sy;
        var z1 = -x * sy + z * cyw;
        var y1 = y * ct - z1 * st;
        var z2 = y * st + z1 * ct;
        return [cx + x1 * scale, cy - y1 * scale, z2];
      };
    }

    // Radii were tuned for a 300px frame; sub-linear scaling keeps small orbs legible
    function radiusScale(size, pow) { return Math.pow(size / 300, pow); }

    // Constellation: nodes drift on a sphere, nearby ones wire up, packets run the edges
    function frameConnecting(size, t, o) {
      var cx = size / 2, cy = size / 2;
      var R = size / 2 * 0.8;
      var pt = makeProj(t * 0.12, 0.32, cx, cy, R);
      var rs = radiusScale(size, o.rsPow);
      var N = o.nodeN, thr = o.thr;
      var i, j, p, q;

      var nodes = [];
      for (i = 0; i < N; i++) {
        var dir = fibDir(i, N);
        var x = dir[0] + 0.3 * (vnoise(i * 0.31 + 9, t * 0.24) - 0.5) * 2;
        var y = dir[1] + 0.3 * (vnoise(i * 0.53 + 27, t * 0.21) - 0.5) * 2;
        var z = dir[2] + 0.3 * (vnoise(i * 0.77 + 55, t * 0.27) - 0.5) * 2;
        var len = Math.sqrt(x * x + y * y + z * z);
        nodes.push([x / len, y / len, z / len]);
      }

      var lines = [];
      for (i = 0; i < N; i++) {
        for (j = i + 1; j < N; j++) {
          var dx = nodes[i][0] - nodes[j][0];
          var dy = nodes[i][1] - nodes[j][1];
          var dz = nodes[i][2] - nodes[j][2];
          var dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
          if (dist >= thr) continue;
          p = pt(nodes[i][0], nodes[i][1], nodes[i][2]);
          q = pt(nodes[j][0], nodes[j][1], nodes[j][2]);
          var depth = ((p[2] + q[2]) / 2 + 1) / 2;
          var alpha = (1 - dist / thr) * (0.3 + 0.55 * depth);
          if (alpha < 0.02) continue;
          lines.push({ x1: p[0], y1: p[1], x2: q[0], y2: q[1], white: 0.42, a: alpha,
                       w: Math.max(0.6, o.lineW * rs) });
        }
      }

      var dots = [];
      for (i = 0; i < N; i++) {
        p = pt(nodes[i][0], nodes[i][1], nodes[i][2]);
        var near = (p[2] + 1) / 2;
        var pulse = 1 + 0.25 * Math.sin(t * 1.4 + i * 2.7);
        dots.push({ x: p[0], y: p[1], z: p[2],
                    r: Math.max(o.rMin, (o.nodeR + o.nodeRDepth * near) * pulse * rs),
                    white: 0.55 - 0.45 * near, a: 1 });
      }

      for (var s = 0; s < o.signals; s++) {
        var seg = Math.floor(t * 0.55 + s * 7.31);
        var from = Math.floor(hashD(seg, s * 3.1 + 1.7) * N);
        var to = Math.floor(hashD(seg, s * 5.7 + 4.2) * N);
        if (from === to) continue;
        var f = frac(t * 0.55 + s * 7.31);
        var sx = lerp(nodes[from][0], nodes[to][0], f);
        var sy = lerp(nodes[from][1], nodes[to][1], f);
        var sz = lerp(nodes[from][2], nodes[to][2], f);
        var sl = Math.max(1e-6, Math.sqrt(sx * sx + sy * sy + sz * sz));
        p = pt(sx / sl, sy / sl, sz / sl);
        var lit = (p[2] + 1) / 2;
        dots.push({ x: p[0], y: p[1], z: p[2],
                    r: Math.max(o.rMin, (o.nodeR * 1.5 + o.nodeRDepth * lit) * rs),
                    white: 0.05, a: 0.5 + 0.5 * lit });
      }

      dots.sort(function (m, n) { return m.z - n.z; });   // paint far -> near
      return { dots: dots, lines: lines };
    }

    // the label's grey as [r, g, b], with white mixed in as transparency
    var DARKEN = 2;                     // >1 = a more solid orb
    function ink(white, alpha) {
      return 'rgba(' + orbRgb[0] + ',' + orbRgb[1] + ',' + orbRgb[2] + ',' +
        Math.min(1, DARKEN * alpha * (1 - white)) + ')';
    }

    // Edges first, so the nodes sit on top of them
    function paint(frame) {
      var i, l, d;
      for (i = 0; i < frame.lines.length; i++) {
        l = frame.lines[i];
        orbCtx.strokeStyle = ink(l.white, l.a);
        orbCtx.lineWidth = l.w;
        orbCtx.beginPath();
        orbCtx.moveTo(l.x1, l.y1);
        orbCtx.lineTo(l.x2, l.y2);
        orbCtx.stroke();
      }
      for (i = 0; i < frame.dots.length; i++) {
        d = frame.dots[i];
        orbCtx.fillStyle = ink(d.white, d.a);
        orbCtx.beginPath();
        orbCtx.arc(d.x, d.y, d.r, 0, Math.PI * 2);
        orbCtx.fill();
      }
    }

    return function (t) {
      if (!orbCtx) return;
      orbCtx.setTransform(orbDpr, 0, 0, orbDpr, 0, 0);
      orbCtx.clearRect(0, 0, orbPx, orbPx);
      paint(frameConnecting(orbPx, t, OPTS));
    };
  })();

  // -- scraping mode: the card's data -----------------------------------------

  // Each read's data, the records a run_script scrape brought (browser.rs scrape_feed),
  // streamed into the card a letter at a time: the card is at its full size
  // the moment the data comes, and the letters come in from a blur at once
  // while the lines glide up to keep the newest in view. A later read fades
  // the old text out and streams its own; the end of the mode stops it
  // wherever it is (feedReset). The letters run on the frame loop's clock,
  // which only moves while the page draws, so a page that was not drawing for
  // a while carries on where it was instead of jumping ahead.
  //
  // The text is kept here and nowhere else. It is painted on a canvas in the
  // overlay, so the page gains no text, no element per letter and no DOM
  // churn: nothing for a read of the page to take, and nothing for the
  // scrape's wait for a settled page to wait on (the scrape skips
  // [data-AutoCua="overlay"] outright as well).
  var cardEl = null, headEl = null, feedCanvas = null, feedCtx = null;
  var feedW = 0, feedH = 0, feedDpr = 1, feedPanelH = 460, feedBase = 13.8, feedInk = '#4d4d55';
  var feedPillW = 0, feedPillH = 0;   // the pill's size, as it was when the card opened
  var feedText = '', feedNext = '';   // the data streaming, and a later read's, waiting for it to fade
  var feedG = [];                     // its letters
  var feedX = [], feedOff = [], feedLineOf = [], feedLines = [];   // where each sits (feedLayout)
  var feedOpen = false;
  var feedClock = 0;                  // seconds of frames since the look came on
  var feedT0 = -1;                    // the clock at the first letter; -1 before it is set
  var feedSwap = -1, feedSwapFrom = 1;   // the clock the old text began to fade at, and from what
  var feedIn = -1;                    // the clock the new text began to fade in at
  var feedY = 0;                      // how far the text has glided up, px
  var feedRest = false;               // painted with nothing moving: no need to paint again
  var feedSeg = null;
  var FEED_FONT = '12px ui-monospace, "SF Mono", Menlo, Consolas, monospace';
  var FEED_LH = 19.2;                 // 12px at line-height 1.6
  var FEED_TOP = 10, FEED_SIDE = 12, FEED_FOOT = 12;   // margins round the text
  var FEED_RATE = 120;                // letters per second
  // Seconds a letter takes to come in: some twenty letters are on their way
  // in at any moment, whatever the pace, so the trail looks the same and
  // costs the same at any speed.
  var FEED_LIVE = 20 / FEED_RATE;
  var FEED_BLUR = 2;                  // px of blur a letter comes in from
  var FEED_FADE = 0.45;               // a later read: the old text fades out,
  var FEED_SWAP = 0.5;                // and the new one starts after this
  // Right-to-left or joined writing (Hebrew, Arabic, Syriac, Thaana, N'Ko,
  // Mongolian, their presentation forms, the astral right-to-left blocks) and
  // the marks that turn text right to left. The canvas orders and joins it
  // only when a line is drawn whole, so a line holding any comes in whole:
  // letter by letter it would stream backwards and unjoined, then jump.
  var FEED_RTL = /[֐-ࣿ᠀-᢯‏‫‮⁧יִ-﷿ﹰ-﻾]|[\uD802\uD803\uD83A\uD83B]/;

  // A CSS cubic-bezier() timing function: progress 0..1 to eased 0..1.
  function cubicBezier(x1, y1, x2, y2) {
    function at(a, b, t) { return ((1 + 3 * a - 3 * b) * t + 3 * b - 6 * a) * t * t + 3 * a * t; }
    return function (p) {
      if (!(p > 0)) return 0;
      if (p >= 1) return 1;
      var lo = 0, hi = 1;
      for (var i = 0; i < 20; i++) {
        var mid = (lo + hi) / 2;
        if (at(x1, x2, mid) < p) lo = mid; else hi = mid;
      }
      return at(y1, y2, (lo + hi) / 2);
    };
  }
  var feedEaseOut = cubicBezier(0, 0, 0.58, 1);     // a letter coming in (ease-out)
  var feedEase = cubicBezier(0.25, 0.1, 0.25, 1);   // the text fading (ease)

  // Where the text sits in each line, as CSS sets a line of 12px/1.6: the
  // font's height centred in it.
  function feedMeasure() {
    feedCtx.font = FEED_FONT;
    var m = feedCtx.measureText('M');
    var asc = m.fontBoundingBoxAscent, desc = m.fontBoundingBoxDescent;
    if (asc > 0 && desc >= 0) feedBase = (FEED_LH - asc - desc) / 2 + asc;
  }

  // The text as letters, a letter being what reads as one (an emoji, an
  // accented letter), so none is ever drawn in halves.
  function feedSplit(text) {
    try {
      if (!feedSeg && window.Intl && Intl.Segmenter) feedSeg = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
      if (feedSeg) return Array.from(feedSeg.segment(text), function (s) { return s.segment; });
    } catch (e) {}
    return Array.from(text);
  }

  // Where each letter sits at the card's width. A line too long for the card
  // wraps under its own indent, after a space where one is near, so no letter
  // streams out of view.
  function feedLayout() {
    var G = feedG, n = G.length, widths = {};
    function wOf(s) {
      var v = widths[s];
      if (v === undefined) v = widths[s] = feedCtx.measureText(s).width;
      return v;
    }
    feedCtx.font = FEED_FONT;
    var maxW = Math.max(60, feedW - 2 * FEED_SIDE);
    var space = wOf(' ');
    feedX = new Array(n); feedOff = new Array(n); feedLineOf = new Array(n); feedLines = [];
    var i = 0;
    for (;;) {
      var j = i;
      while (j < n && G[j] !== '\n') j++;             // one line of the text: G[i, j), then its '\n'
      var k = i, indent = 0;
      while (k < j && G[k] === ' ') { indent += space; k++; }
      var hang = Math.min(indent + 2 * space, maxW * 0.4);
      var s = i, x0 = 0;
      for (;;) {
        var x = x0, e = s, brk = -1, brkX = 0;
        while (e < j) {
          var w = wOf(G[e]);
          if (x + w > maxW && e > s) break;
          x += w;
          e++;
          if (G[e - 1] === ' ' && e > k) { brk = e; brkX = x; }   // a space past the indent: a place to wrap
        }
        // Wrap at the last space, unless that leaves the line half empty.
        if (e < j && brk > s && brkX > x0 + (maxW - x0) / 2) e = brk;
        feedLinePush(s, e, e < j ? e : Math.min(j + 1, n), x0, wOf);
        if (e >= j) break;
        s = e;
        x0 = hang;
      }
      if (j >= n) break;
      i = j + 1;
    }
  }

  // One line on the card: letters [s, e) drawn from x0, and up to `end` the
  // '\n' that closes it, which sits there undrawn.
  function feedLinePush(s, e, end, x0, wOf) {
    var li = feedLines.length, x = x0, str = '', g;
    for (g = s; g < e; g++) {
      feedX[g] = x; feedOff[g] = str.length; feedLineOf[g] = li;
      x += wOf(feedG[g]);
      str += feedG[g];
    }
    for (g = e; g < end; g++) {
      feedX[g] = x; feedOff[g] = str.length; feedLineOf[g] = li;
    }
    feedLines.push({ start: s, end: end, x0: x0, str: str, whole: FEED_RTL.test(str) });
  }

  function feedSet() {
    feedG = feedSplit(feedText);
    feedLayout();
    feedRest = false;
  }

  function cardSize(w, h) {
    cardEl.style.setProperty('width', w + 'px', 'important');
    cardEl.style.setProperty('height', h + 'px', 'important');
  }

  // The canvas is the unfolded card's size from the start, clipped while the
  // card grows round it, so it is never reallocated frame by frame.
  function feedResize(w, h) {
    feedDpr = window.devicePixelRatio || 1;
    feedW = w;
    feedH = Math.max(0, h);
    feedCanvas.style.setProperty('width', feedW + 'px', 'important');
    feedCanvas.style.setProperty('height', feedH + 'px', 'important');
    feedCanvas.width = Math.round(feedW * feedDpr);
    feedCanvas.height = Math.round(feedH * feedDpr);
    feedRest = false;
  }

  // The open card: right to the same gap from the far edge as from this one,
  // and down to its panel height, or to that gap from the bottom; never
  // smaller than the pill. (The pill's size is kept from when it opened: the
  // head is as wide as the card once the card is open.)
  function feedFit() {
    var rr = scrapeEl.getBoundingClientRect();
    var cr = cardEl.getBoundingClientRect();
    var left = cr.left - rr.left, top = cr.top - rr.top;
    var w = Math.max(feedPillW, rr.width - 2 * left);
    var h = Math.max(feedPillH, Math.min(feedPanelH, rr.height - top - left));
    cardSize(w, h);
    feedResize(w, h - feedPillH);
  }

  function feedUnfold() {
    feedOpen = true;
    var hr = headEl.getBoundingClientRect();
    feedPillW = hr.width;
    feedPillH = hr.height;
    feedFit();
    feedSet();
  }

  // The window changed size with the card open: it follows, and the text
  // wraps afresh at its new width.
  function feedRefit() {
    if (!feedOpen || !feedCtx) return;
    try {
      feedFit();
      if (feedG.length) feedLayout();
      if (reduceMotion) feedPaint(0, true);
    } catch (e) {}
  }

  // The filter for a letter (or a right-to-left line) `e` of the way in: a
  // blur in the bitmap's pixels, not the page's, and none once it is under
  // half a pixel, where it cannot be seen and would only cost.
  function feedBlur(e) {
    var b = FEED_BLUR * (1 - e);
    return b > 0.5 ? 'blur(' + (b * feedDpr).toFixed(2) + 'px)' : 'none';
  }

  function feedAlpha(c) {
    if (feedSwap >= 0) return feedSwapFrom * (1 - feedEase(Math.min(1, (c - feedSwap) / FEED_FADE)));
    if (feedIn >= 0 && c - feedIn < FEED_FADE) return feedEase((c - feedIn) / FEED_FADE);
    return 1;
  }

  // One frame: the card opening with the first data, a later read's fade,
  // then the letters.
  function feedTick(dt) {
    feedClock += dt;
    if (!feedText || !feedCtx) return;           // a pill until a read's data comes
    var c = feedClock;
    if (!feedOpen) {
      feedUnfold();
      feedT0 = c;
    }
    if (feedSwap >= 0 && c - feedSwap >= FEED_SWAP) {
      feedText = feedNext;
      feedNext = '';
      feedSwap = -1;
      feedSet();
      feedT0 = c;
      feedIn = c;
      feedY = 0;
    }
    feedPaint(dt, false);
  }

  // The letters out so far: those still coming in one by one, from a blur,
  // the rest as whole lines. `still` paints all of it at once and at rest
  // (reduced motion).
  function feedPaint(dt, still) {
    var ctx = feedCtx, n = feedG.length;
    var t = still ? Infinity : feedClock - feedT0;
    var shown = feedT0 < 0 || !(t >= 0) ? 0 : Math.min(n, Math.floor(t * FEED_RATE) + 1);
    var settled = !(t >= FEED_LIVE) ? 0 : Math.min(n, Math.floor((t - FEED_LIVE) * FEED_RATE) + 1);
    var target = feedY;
    if (shown && !still) {                       // the glide: the newest line stays in view
      target = Math.max(0, FEED_TOP + (feedLineOf[shown - 1] + 1) * FEED_LH + FEED_FOOT - feedH);
      feedY += (target - feedY) * (1 - Math.pow(0.9, dt * 60));
      if (Math.abs(target - feedY) < 0.01) feedY = target;
    }
    var alpha = feedAlpha(feedClock);
    var moving = shown > 0 && (settled < n || feedY !== target || alpha < 1 || feedSwap >= 0);
    if (!moving && feedRest) return;
    feedRest = !moving;
    ctx.setTransform(feedDpr, 0, 0, feedDpr, 0, 0);
    ctx.clearRect(0, 0, feedW, feedH);
    if (!shown || alpha <= 0) return;
    ctx.font = FEED_FONT;
    ctx.fillStyle = feedInk;
    ctx.textBaseline = 'alphabetic';
    var first = Math.max(0, Math.floor((feedY - FEED_TOP) / FEED_LH));
    var last = Math.min(feedLines.length - 1, Math.floor((feedY + feedH - FEED_TOP) / FEED_LH));
    for (var k = first; k <= last; k++) {
      var ln = feedLines[k];
      if (ln.start >= shown) break;
      var y = FEED_TOP + k * FEED_LH - feedY + feedBase;
      if (ln.whole) {                            // FEED_RTL: all of it, on its first letter's clock
        var ew = feedEaseOut((t - ln.start / FEED_RATE) / FEED_LIVE);
        ctx.globalAlpha = alpha * ew;
        ctx.filter = feedBlur(ew);
        ctx.fillText(ln.str, FEED_SIDE + ln.x0, y);
        continue;
      }
      var upTo = Math.min(ln.end, settled);
      if (upTo > ln.start && ln.str) {
        ctx.globalAlpha = alpha;
        ctx.filter = 'none';
        ctx.fillText(upTo >= ln.end ? ln.str : ln.str.slice(0, feedOff[upTo]), FEED_SIDE + ln.x0, y);
      }
      var end = Math.min(ln.end, shown);
      for (var i = Math.max(ln.start, settled); i < end; i++) {
        var ch = feedG[i];
        if (ch === ' ' || ch === '\n') continue;
        var e = feedEaseOut((t - i / FEED_RATE) / FEED_LIVE);
        ctx.globalAlpha = alpha * e;
        ctx.filter = feedBlur(e);
        ctx.fillText(ch, FEED_SIDE + feedX[i], y);
      }
    }
    ctx.filter = 'none';
    ctx.globalAlpha = 1;
  }

  // Back to the pill with nothing kept: the mode ended, or the page is going,
  // wherever the stream had got to.
  function feedReset() {
    feedText = '';
    feedNext = '';
    feedG = []; feedX = []; feedOff = []; feedLineOf = []; feedLines = [];
    feedOpen = false;
    feedClock = 0;
    feedT0 = -1;
    feedSwap = -1;
    feedIn = -1;
    feedY = 0;
    feedRest = false;
    if (cardEl) {
      cardEl.style.removeProperty('width');
      cardEl.style.removeProperty('height');
    }
    if (feedCanvas) {
      feedCanvas.width = 0;
      feedCanvas.height = 0;
    }
  }

  // A read's data for the card (browser.rs scrape_feed): streamed in, or, if
  // the card is already showing a read, the next one once that has faded.
  // Only while the look is on; data for a page whose look is off is dropped,
  // and never turns it on. Answers whether it was taken.
  window.__AutoCuaScrapeFeed = scrapeFeed;
  function scrapeFeed(text) {
    if (off || !scraping || !feedCtx || typeof text !== 'string' || !text) return false;
    try {
      if (reduceMotion) {                        // all of it at once, at rest
        feedText = text;
        if (feedOpen) feedSet(); else feedUnfold();
        feedT0 = 0;
        feedPaint(0, true);
        return true;
      }
      if (!feedOpen || feedT0 < 0 || feedClock < feedT0) {
        feedText = text;                         // nothing on the card yet: this streams instead
        if (feedOpen) feedSet();
        return true;
      }
      if (feedSwap < 0) {
        feedSwapFrom = feedAlpha(feedClock);
        feedSwap = feedClock;
        feedIn = -1;
      }
      feedNext = text;
      return true;
    } catch (e) {
      return false;
    }
  }

  function scrapeFrame(now) {
    scrapeRaf = 0;
    if (off || !scraping) return;
    var dt = Math.max(0, Math.min((now - scrapeLast) / 1000, 0.05));
    scrapeLast = now;
    orbDraw(now / 1000 * ORB_SPEED);
    try {
      feedTick(dt);
    } catch (e) {}
    scrapeRaf = requestAnimationFrame(scrapeFrame);
  }

  function scrapeStart() {
    orbResize();
    window.addEventListener('resize', feedRefit);
    if (reduceMotion) {        // a still orb; glow.css holds the rest steady
      orbDraw(0.6);
      return;
    }
    if (!scrapeRaf) {
      scrapeLast = performance.now();
      scrapeRaf = requestAnimationFrame(scrapeFrame);
    }
  }

  function scrapeHalt() {
    window.removeEventListener('resize', feedRefit);
    if (scrapeRaf) {
      cancelAnimationFrame(scrapeRaf);
      scrapeRaf = 0;
    }
    feedReset();
  }

  // On (true) or off (false); answers whether the overlay is still here to
  // take it. The look is glow.css's and needs nothing but the class: a canvas
  // that cannot be set up costs its own motion, never the rest of the overlay.
  window.__AutoCuaScrapeGlow = scrapeGlow;
  function scrapeGlow(on) {
    if (off) return false;
    on = !!on;
    if (on === scraping) return true;
    scraping = on;
    if (!on) {
      layer.classList.remove('AutoCua-scraping');
      scrapeHalt();
      return true;
    }
    try {
      if (!scrapeEl) scrapeBuild();
    } catch (e) {}
    if (!scrapeEl) return true;
    layer.classList.add('AutoCua-scraping');
    try {
      if (!scrapeReady) {
        scrapeReady = true;
        scrapeCanvases();
      }
      scrapeStart();
    } catch (e) {}
    return true;
  }

  // -- go -------------------------------------------------------------------

  // The agent let go of the browser (AutoCuaBridge, when a run ends or its agent goes
  // away): the overlay leaves the page, cursor and all, and stays gone. A later run
  // dresses the page again from scratch.
  window.__AutoCuaGlowOff = function () {
    off = true;
    scrapeHalt();
    window.removeEventListener('pagehide', scrapePagehide);
    layer.remove();
    if (styleSheet) {
      document.adoptedStyleSheets = document.adoptedStyleSheets.filter(function (s) { return s !== styleSheet; });
    }
    if (styleEl) styleEl.remove();
    if (mo) mo.disconnect();
    window.__AutoCuaOverlay = 0;
  };

  window.__AutoCuaOverlay = 1;
  installStyle();

  if (!attach()) {
    // document-start: <body> does not exist yet. Take the first chance the
    // parser gives us rather than waiting for DOMContentLoaded, which on a
    // slow page is a visible delay.
    try {
      var mo = new MutationObserver(function () {
        if (attach()) { mo.disconnect(); fitNow(); }
      });
      mo.observe(document.documentElement, { childList: true, subtree: true });
    } catch (e) {}
    document.addEventListener('DOMContentLoaded', function () {
      attach(); fitNow();
    }, { once: true });
  }

  // A same-origin navigation lands here with the overlay rebuilt from nothing.
  // If this tab left a position behind, take it NOW rather than waiting for the
  // controller's next sync: that sync arrives one scan later at best, and if it
  // fires while this document is still loading it lands on a page that has not
  // run this script yet and is simply lost. That gap is what made the cursor
  // vanish after a search.
  (function () {
    var was = recall();
    if (!was) return;
    placeInstant(was.x, was.y);
    if (was.line) {
      line = was.line;
      bubbleWord.textContent = was.line;
      bubble.classList.add('AutoCua-bubble-on');
      fitNow();
    }
  })();

  requestAnimationFrame(tick);
})();
