"""Universal floating pill/orb banner — surface-agnostic (Linux).

Lives at remote_connection/ (outside any single surface folder) so the pill +
orb visual engine is written once and reused by every surface — telegram/
today, discord/ and whatsapp/ later. Each surface's own folder holds the
surface-specific behaviour (setup wizard steps, agent stream) and drives this
banner over the same `StatusBanner` API as the macOS and Windows twins.

A small always-on-top pill at the top-right of the screen that contains:
  - the animated stop-orb on the left,
  - a status message in the middle (multi-line capable; pill grows downward),
  - a clickable "Next" button on the right (only visible when the script is
    waiting for the user — hidden during processing steps).

setup.py calls show() once, then alternates update("…") + wait_for_next()
to pace the user. close() tears it down. The Next button is shown
automatically inside wait_for_next() and hidden as soon as it returns, so
callers don't have to manage visibility manually.

HOW IT RUNS ON LINUX — its own process, on X11 (XWayland):

  The pill must sit at the top-right of the screen and stay above whatever
  the user is doing in the browser. A Wayland client can do neither: the
  protocol gives it no way to place a toplevel or to keep it above others —
  GNOME puts a new window where IT wants. An X11 client under XWayland can
  do both, and GDK picks its backend per PROCESS (GDK_BACKEND) — the main app
  already runs its pywebview window on Wayland, so the banner is spawned as
  a separate process with GDK_BACKEND=x11. Measured on GNOME 50 Wayland: a
  440x44 undecorated window that asked for (1460, 56) landed at (1460, 56)
  and stayed put when it grew to 440x120.

  The page is the same HTML as macOS (BANNER_HTML / COMPACT_HTML below,
  unchanged) in a WebKit2GTK view — its `webkit.messageHandlers.*` bridge is
  the very API WebKitGTK exposes. The two halves talk the way the Windows
  banner does:

    parent (StatusBanner, any thread)  →  stdin   {"cmd": "JS", "js": …}
                                                  {"cmd": "CODER", "active": …}
                                                  {"cmd": "CLOSE"}
    banner process                     →  stdout  {"event": "READY" | "NEXT" |
                                                   "CHOICE", "value" |
                                                   "SAVE", "value" |
                                                   "REVEAL" | "CLOSED"}

  Window geometry — the pill growing to fit its text, the compact orb
  widening into a ticker or a coder panel — is handled entirely inside the
  banner process from the page's height_changed / size_changed messages,
  with the top-right corner anchored.

If the process cannot be started (no GTK, no WebKit typelib) the class
becomes a no-op so the automation still completes without a banner.
"""
import json
import logging
import os
import subprocess
import sys
import threading
import weakref
from pathlib import Path

logger = logging.getLogger(__name__)

_REPO_ROOT = Path(__file__).resolve().parents[3]

BANNER_HTML = """
<!DOCTYPE html>
<html><head><meta charset="utf-8"><style>
html, body { margin: 0; padding: 0; width: 100%; background: transparent;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; }
html { height: 100%; }
/* The orb is absolute-positioned (top-left, anchored), and the body has
   extra left padding (= orb-width 36 + gap 8 = 44) so flex content starts
   to the right of the orb. This decouples orb position from message
   height: no matter how many lines the message wraps to, the orb stays
   exactly where it started — first line of text stays next to it,
   additional lines flow below. */
body { display: flex; flex-wrap: wrap; align-items: center; gap: 8px;
  padding: 6px 10px 6px 54px; box-sizing: border-box;
  min-height: 44px; overflow: hidden; position: relative; }

.orb-wrap { position: absolute; top: 6px; left: 10px;
  width: 36px; height: 36px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: center; }
/* The orb itself is pc_button.html embedded in an iframe (single source of
   truth); centred + click-through, a touch bigger so the glow isn't clipped. */
.orb-frame { position: absolute; top: 50%; left: 50%;
  width: 50px; height: 50px; transform: translate(-50%, -50%);
  border: 0; background: transparent; pointer-events: none; }
.stop-circle-1 {
  width: 36px; height: 36px; border-radius: 50%; position: absolute; background: transparent;
  display: flex; align-items: center; justify-content: center;
  animation: stop-pulse 4.2s ease-in-out infinite 0.3s; z-index: 1;
}
.stop-circle-1::before, .stop-circle-1::after {
  content: ""; position: absolute; border-radius: 50%; filter: blur(7px); width: 30%; height: 30%;
}
.stop-circle-1::before { background: #ff0073; top: 30%; right: 30%; }
.stop-circle-1::after  { background: #00baff; bottom: 10%; left: 30%; }
.stop-circle-2 {
  width: 28px; height: 28px; border-radius: 50%; position: absolute; inset: 0; margin: auto;
  background-color: white; z-index: 9;
  animation: stop-pulse2 4.2s ease-in-out infinite;
}
.stop-circle-2::before, .stop-circle-2::after {
  content: ""; position: absolute; border-radius: 50%; filter: blur(5px); z-index: 1;
}
.stop-circle-2::before { background: #ff0073; width: 30%; height: 30%; top: 20%; right: 20%; }
.stop-circle-2::after  { background: #00bbff; width: 20%; height: 20%; bottom: 10%; left: 40%; }
.stop-bg {
  position: absolute; inset: 0; border-radius: 50%;
  box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8), 0 0 2px 2px rgba(255,255,255,0.9);
  background-color: #9292d8; animation: stop-bgRotate 2.5s linear infinite;
}
.stop-bg::before {
  content: ""; position: absolute; inset: 0; border-radius: inherit;
  animation: stop-bgColor 4s linear infinite;
  box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8); opacity: 0.2;
}
.stop-pc {
  position: absolute; inset: 0; margin: auto; width: 28px; height: 28px; z-index: 10;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  box-sizing: border-box; gap: 1px;
}
.stop-monitor { width: 11px; height: 9px; background: transparent; border-radius: 1px; padding: 0;
  border: 1px solid white; box-sizing: border-box; }
.stop-screen { width: 100%; height: 100%; display: flex; justify-content: center; align-items: center; gap: 1px; }
.stop-eye { width: 2px; height: 3px; border-radius: 1px; background: white; position: relative; top: -1px; animation: stop-blink 4s infinite; }
.stop-base { width: 14px; height: 1px; background: white; border-radius: 0.5px; }

/* min-width: 0 is the flexbox shrink-below-content-size fix — without it a
   long message refuses to shrink and pushes the Next button off the pill.
   align-self + padding-top pin the first line to the same vertical spot it
   sits at when single-line — so when the text wraps, the first line stays
   put and the new line flows below it instead of the whole block sliding
   down to stay centered. */
.msg { flex: 1 1 auto; min-width: 0; font-size: 12.5px; color: #6b6b75;
  padding: 10px 4px 0; line-height: 1.35;
  word-wrap: break-word; overflow-wrap: break-word;
  align-self: flex-start; }

.next-btn { flex-shrink: 0; height: 28px; padding: 0 14px; border: none; border-radius: 14px;
  background: #5e6ad2; color: white; font-size: 12px; font-weight: 600; cursor: pointer;
  font-family: inherit; transition: background 0.15s ease; align-self: center; }
.next-btn:hover  { background: #6e7ce3; }
.next-btn:active { background: #4e5ac2; }

.choice-row { display: none; flex-shrink: 0; gap: 6px; align-self: center; }
.input-row { display: none; flex-basis: 100%; flex-direction: column; gap: 4px;
  padding: 2px 4px 0; order: 1; }
.input-line { display: flex; gap: 6px; align-items: center; }
#token-input { flex: 1 1 auto; height: 28px; border: 1px solid #d4d4dc; border-radius: 14px;
  padding: 0 10px; font-size: 12px; font-family: inherit; outline: none; color: #333;
  background: white; }
#token-input:focus { border-color: #5e6ad2; }
.input-error { display: none; color: #d23; font-size: 11px; padding: 0 4px; }
/* Continuous shimmer once a message has landed: the text becomes a mask over
   a gradient in its own grey, and a lighter band sweeps left to right through
   it, forever. Only the finished text carries it — during the reveal each
   letter fades in on its own (see setMsg). */
.msg.shimmer { color: transparent;
  background-image: linear-gradient(90deg, #6b6b75 0%, #6b6b75 42%, #dcdce6 50%, #6b6b75 58%, #6b6b75 100%);
  background-size: 250% 100%; background-repeat: no-repeat;
  -webkit-background-clip: text; background-clip: text;
  animation: msg-shimmer 2.4s linear infinite; }
@keyframes msg-shimmer { from { background-position: 100% 0; } to { background-position: 0% 0; } }

@keyframes stop-pulse  { 0%{transform:scale(.97)} 15%{transform:scale(1)} 30%{transform:scale(.98)} 45%{transform:scale(1)} 60%{transform:scale(.97)} 85%{transform:scale(1)} 100%{transform:scale(.97)} }
@keyframes stop-pulse2 { 0%{transform:scale(1)} 15%{transform:scale(1.03)} 30%{transform:scale(.98)} 45%{transform:scale(1.04)} 60%{transform:scale(.97)} 85%{transform:scale(1.03)} 100%{transform:scale(1)} }
@keyframes stop-bgRotate { 0%{transform:rotate(0)} 20%{transform:rotate(90deg)} 40%{transform:rotate(180deg) scale(.95,1)} 60%,100%{transform:rotate(360deg)} }
@keyframes stop-bgColor  { 20%{background-color:red} 40%{background-color:#5eff7e} 60%{background-color:#2cb5ff} 80%{background-color:#fc63ff} }
@keyframes stop-blink    { 0%,85%,100%{transform:scaleY(1)} 92%{transform:scaleY(.1)} }
</style></head>
<body>
<div class="orb-wrap">
  <iframe id="orbFrame" class="orb-frame" src="http://127.0.0.1:5000/chat_input/pc_button.html"
          scrolling="no" frameborder="0"></iframe>
</div>
<span class="msg" id="msg">Starting…</span>
<button class="next-btn" id="next"
        onclick="webkit.messageHandlers.next_clicked.postMessage(1)">Next</button>
<div class="choice-row" id="choice-row">
  <button class="next-btn" id="choice-left"
          onclick="webkit.messageHandlers.choice_clicked.postMessage('left')">Left</button>
  <button class="next-btn" id="choice-right"
          onclick="webkit.messageHandlers.choice_clicked.postMessage('right')">Right</button>
</div>
<div class="input-row" id="input-row">
  <div class="input-line">
    <input type="text" id="token-input" placeholder="Paste your BotFather token here" />
    <button class="next-btn" id="save-btn"
            onclick="(function(){var v=document.getElementById('token-input').value;
                     webkit.messageHandlers.save_clicked.postMessage(v);})()">Save</button>
  </div>
  <div class="input-error" id="input-error"></div>
</div>
<script>
  // The orb is pc_button.html in an iframe; it starts hidden and shows when
  // told, so nudge it visible once the iframe has loaded.
  (function () {
    var f = document.getElementById('orbFrame');
    if (f) f.addEventListener('load', function () {
      try { f.contentWindow.postMessage('pcbtn:show', '*'); } catch (e) {}
    });
  })();
  // Char-by-char reveal: Python calls setMsg("…") with the full text; we
  // animate it letter-at-a-time so the pill types out smoothly. A new call
  // cancels any in-flight animation and starts over with the latest text.
  //
  // Paced by the CLOCK, not by a per-letter timer: a setTimeout chain is
  // clamped to ~4 ms and jitters, and on Linux it ran a 54-char line in
  // ~0.5 s — slow enough that the Next button felt late. Every animation
  // frame appends whatever letters are due by elapsed time, so the pace is
  // the same on every machine and a line lands in well under a quarter
  // second. The shimmer is the fade: it is longer than the cadence, so a
  // handful of letters are always mid-fade at the leading edge.
  const _CHARS_PER_SEC = 320;   // 54 chars in ~170 ms
  const _FADE_MS = 90;          // per-letter fade-in; ~29 letters shimmer at once
  let _revealRaf = null;
  function setMsg(fullText) {
    if (_revealRaf) { cancelAnimationFrame(_revealRaf); _revealRaf = null; }
    const el = document.getElementById('msg');
    if (!el) return;
    const chars = Array.from((fullText || '').toString());  // split by code point so emoji stay intact
    el.classList.remove('shimmer');
    el.textContent = '';
    let shown = 0;
    const t0 = performance.now();
    const frame = (now) => {
      const due = Math.min(chars.length,
                           Math.floor((now - t0) / 1000 * _CHARS_PER_SEC) + 1);
      while (shown < due) {
        // Each letter in its own span, faded in. Several are in their
        // transition at once — that overlap is what makes the stream read
        // as a shimmer rather than as discrete pops.
        const span = document.createElement('span');
        span.textContent = chars[shown];
        span.style.opacity = '0';
        span.style.transition = 'opacity ' + _FADE_MS + 'ms ease-out';
        el.appendChild(span);
        requestAnimationFrame(() => { span.style.opacity = '1'; });
        shown++;
      }
      if (shown >= chars.length) {
        _revealRaf = null;
        // Landed: collapse the per-letter spans into plain text so the
        // shimmer gradient can mask the whole line as one, then keep it
        // shimmering until the next message.
        el.textContent = chars.join('');
        el.classList.add('shimmer');
        // Tell Python the streaming reveal has finished so it can now show
        // whichever control set (Next / choice / input) is appropriate for
        // this step. Without this signal the button would pop in while the
        // text is still being typed out.
        try { webkit.messageHandlers.reveal_done.postMessage(1); } catch (e) {}
        return;
      }
      _revealRaf = requestAnimationFrame(frame);
    };
    _revealRaf = requestAnimationFrame(frame);
  }
  window.setMsg = setMsg;

  // ── choice / input UI controls (paired with wait_for_choice / wait_for_input
  //    on the Python side). All three rows — #next, #choice-row, #input-row —
  //    are mutually exclusive: showing one hides the others.
  function setChoice(leftLabel, rightLabel) {
    document.getElementById('choice-left').textContent = leftLabel;
    document.getElementById('choice-right').textContent = rightLabel;
    document.getElementById('choice-row').style.display = 'flex';
    document.getElementById('next').style.display = 'none';
    document.getElementById('input-row').style.display = 'none';
  }
  function setInput(saveLabel) {
    document.getElementById('save-btn').textContent = saveLabel || 'Save';
    document.getElementById('input-row').style.display = 'flex';
    document.getElementById('choice-row').style.display = 'none';
    document.getElementById('next').style.display = 'none';
    document.getElementById('input-error').style.display = 'none';
    var inp = document.getElementById('token-input');
    inp.value = '';
    setTimeout(function(){ inp.focus(); }, 30);
  }
  function setInputError(msg) {
    var el = document.getElementById('input-error');
    if (msg) { el.textContent = msg; el.style.display = 'block'; }
    else     { el.style.display = 'none'; }
  }
  function clearAll() {
    document.getElementById('choice-row').style.display = 'none';
    document.getElementById('input-row').style.display = 'none';
    document.getElementById('input-error').style.display = 'none';
  }
  window.setChoice = setChoice;
  window.setInput = setInput;
  window.setInputError = setInputError;
  window.clearAll = clearAll;

  // Enter in the token input acts as Save.
  document.getElementById('token-input').addEventListener('keydown', function(e) {
    if (e.key === 'Enter') {
      webkit.messageHandlers.save_clicked.postMessage(this.value);
    }
  });

  // Tell Python whenever the body's natural height changes so the NSWindow
  // can grow/shrink to fit. Debounced to the last reported value to avoid
  // a resize loop (window resize → WebView resize → body re-measure → fire).
  (function () {
    let last = -1;
    const report = () => {
      const h = Math.ceil(document.body.scrollHeight);
      if (h === last) return;
      last = h;
      try { webkit.messageHandlers.height_changed.postMessage(h); } catch (e) {}
    };
    window.addEventListener('load', () => setTimeout(report, 30));
    const ro = new ResizeObserver(report);
    ro.observe(document.body);
    ro.observe(document.getElementById('msg'));
  })();
</script>
</body></html>
"""


# Compact HTML — used when StatusBanner(compact=True).
#
# Visual model:
#   Empty state (no task running):
#     ┌──┐
#     │○ │   44×44 white circle, just the orb.
#     └──┘
#
#   Text streaming (during a task):
#     ┌────────────────────────────────────────┐
#     │○  single-line text streams to the right│
#     └────────────────────────────────────────┘
#                                            440 px
#
# Pre-expand width, then stream (no jitter):
#  - body has fixed empty width (44) and fixed has-text width (440); the
#    height stays at 44 in both states. On first setMsg(), JS toggles
#    body.has-text, body width snaps 44→440, ResizeObserver fires once,
#    Python animates the NSPanel over ~0.25 s. JS then waits 350 ms
#    before appending the first word, so streaming starts AFTER the
#    banner finishes expanding.
#
# Paging through long messages:
#  - .msg is `white-space: nowrap; overflow: hidden` and capped at
#    max-width: 388 px. After each word, JS does a sync layout read to
#    check if scrollWidth has exceeded the visible width. If yes, the
#    word that overflowed is removed, we hold the current line briefly,
#    then clear and continue streaming the remaining words on a fresh
#    line. Loops until every word has been displayed.
COMPACT_HTML = """
<!DOCTYPE html>
<html><head><meta charset="utf-8"><style>
html { margin: 0; padding: 0; background: transparent;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; }

/* Empty state: 44×44 perfect circle (orb only). has-text: 440×44
   stadium — height stays the same, only width animates. Python's
   setFrame_display_animate_ handles the smooth NSPanel animation while
   the WKWebView frame follows it via its autoresizing mask. */
body { margin: 0; padding: 4px; box-sizing: border-box;
  background: transparent;
  display: flex; flex-direction: column; align-items: stretch; gap: 0;
  width: 44px; height: 44px; overflow: hidden; }
body.has-text { width: 440px; }
/* Coder terminal open: the pill expands into a wide, tall card. Declared
   AFTER .has-text so its width/height win regardless of the orb ticker's
   has-text toggle. Height is content-driven; Python clamps the window to
   COMPACT_CODER_MAX_H. */
body.coder { width: 540px; height: auto; padding-bottom: 12px; }

/* Top row = orb + the single-line step ticker (the original compact pill). */
.toprow { display: flex; align-items: center; gap: 8px; height: 36px;
  width: 100%; flex-shrink: 0; }

.orb-wrap { position: relative; width: 36px; height: 36px;
  flex-shrink: 0;
  display: flex; align-items: center; justify-content: center; }
/* The orb is telergam_animation.html embedded in an iframe (single source of
   truth); centred + click-through, a touch bigger so the glow isn't clipped. */
.orb-frame { position: absolute; top: 50%; left: 50%;
  width: 50px; height: 50px; transform: translate(-50%, -50%);
  border: 0; background: transparent; pointer-events: none; }

.stop-circle-1 {
  width: 36px; height: 36px; border-radius: 50%; position: absolute; background: transparent;
  display: flex; align-items: center; justify-content: center;
  animation: stop-pulse 4.2s ease-in-out infinite 0.3s; z-index: 1;
}
.stop-circle-1::before, .stop-circle-1::after {
  content: ""; position: absolute; border-radius: 50%; filter: blur(7px); width: 30%; height: 30%;
}
.stop-circle-1::before { background: #ff0073; top: 30%; right: 30%; }
.stop-circle-1::after  { background: #00baff; bottom: 10%; left: 30%; }
.stop-circle-2 {
  width: 28px; height: 28px; border-radius: 50%; position: absolute; inset: 0; margin: auto;
  background-color: white; z-index: 9;
  animation: stop-pulse2 4.2s ease-in-out infinite;
}
.stop-circle-2::before, .stop-circle-2::after {
  content: ""; position: absolute; border-radius: 50%; filter: blur(5px); z-index: 1;
}
.stop-circle-2::before { background: #ff0073; width: 30%; height: 30%; top: 20%; right: 20%; }
.stop-circle-2::after  { background: #00bbff; width: 20%; height: 20%; bottom: 10%; left: 40%; }
.stop-bg {
  position: absolute; inset: 0; border-radius: 50%;
  box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8), 0 0 2px 2px rgba(255,255,255,0.9);
  background-color: #9292d8; animation: stop-bgRotate 2.5s linear infinite;
}
.stop-bg::before {
  content: ""; position: absolute; inset: 0; border-radius: inherit;
  animation: stop-bgColor 4s linear infinite;
  box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8); opacity: 0.2;
}

/* Both icons stacked at the same spot; opposing keyframes cross-fade them. */
.icon-stack {
  position: absolute; inset: 0; margin: auto; width: 28px; height: 28px; z-index: 10;
  display: flex; align-items: center; justify-content: center;
}
.icon-layer {
  position: absolute; inset: 0;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  gap: 1px; box-sizing: border-box;
  /* Force each layer onto its own GPU compositor layer up-front so the
     opacity cross-fade doesn't trigger a one-frame promotion artifact (the
     "small square" flash). */
  will-change: opacity;
  transform: translateZ(0);
  -webkit-backface-visibility: hidden;
  backface-visibility: hidden;
}
.icon-pc { animation: icon-cycle-pc 10s ease-in-out infinite; }
.icon-tg { animation: icon-cycle-tg 10s ease-in-out infinite; color: white; }

.stop-monitor { width: 11px; height: 9px; background: transparent; border-radius: 1px; padding: 0;
  border: 1px solid white; box-sizing: border-box; }
.stop-screen { width: 100%; height: 100%; display: flex; justify-content: center; align-items: center; gap: 1px; }
.stop-eye { width: 2px; height: 3px; border-radius: 1px; background: white; position: relative; top: -1px; animation: stop-blink 4s infinite; }
.stop-base { width: 14px; height: 1px; background: white; border-radius: 0.5px; }

/* Single-line streaming text. white-space: nowrap means tokens line up
   left-to-right and never wrap; overflow: hidden clips anything past it.
   flex:1 + min-width:0 makes the line fill ALL remaining width of the
   top row, so it stretches to the card's right edge in both the 440px
   pill and the 540px coder card (a fixed max-width would cap it short of
   the wider card). The JS pager watches scrollWidth vs the live clientWidth
   and starts a new "page" before content actually overflows. */
.msg { flex: 1 1 auto; min-width: 0; font-size: 12.5px; color: #6b6b75; line-height: 1.35;
  white-space: nowrap; overflow: hidden; padding: 0; }
.msg:empty { display: none; }

/* ── embedded coder terminal panel (shown only while a CLI/coder runs) ── */
/* #coderPanel is just the STACK container; each coder is its own bordered,
   glowing terminal box (.cp-term) one-below-the-other. */
#coderPanel { display: none; width: 100%; margin-top: 8px; box-sizing: border-box; }
body.coder #coderPanel { display: flex; flex-direction: column; gap: 8px; }
.cp-term { position: relative; width: 100%; box-sizing: border-box;
  border-radius: 12px; overflow: hidden; background: #f7f7f9;
  box-shadow: inset 0 0 0 1px rgba(139,92,246,0.55), inset 0 0 8px rgba(139,92,246,0.45);
  animation: cp-edgeGlow 3s ease-in-out infinite; }
.cp-body { position: relative; z-index: 1; padding: 14px 14px 18px; color: rgba(0,0,0,0.82);
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Courier New", monospace;
  font-size: 12px; line-height: 1.5; }
/* this coder's own streaming output line (paginated; '>' prompt re-added per
   page). It shows real output, or filler while the coder waits on its minions. */
.cp-out { min-height: 22px; overflow: hidden; }
.cp-line { white-space: nowrap; overflow: hidden; margin: 2px 0; }
.cp-mrow .cp-line { margin: 0; }
.cp-p { color: rgba(0,0,0,0.4); }
.cp-todos { margin: 9px 0 4px 22px; display: flex; flex-direction: column; gap: 6px; }
.cp-todos.hidden { display: none; }
.cp-item { display: flex; align-items: center; gap: 8px; }
.cp-item .lbl { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.cp-chk { width: 14px; height: 14px; min-width: 14px; border-radius: 4px; border: 1px solid rgba(0,0,0,0.3); }
/* Completed task: a subtle glowing purple checkmark (no box) — the tick
   itself is the marker. Drawn as a rotated element with right+bottom borders;
   the glow gently pulses via drop-shadow. */
.cp-chk.done { background: transparent; border-color: transparent; position: relative; }
.cp-chk.done::after { content: ""; position: absolute; left: 5px; top: 1px;
  width: 4px; height: 8px; box-sizing: border-box;
  border: solid #8b5cf6; border-width: 0 2px 2px 0;
  transform: rotate(45deg); transform-origin: center;
  animation: cp-tickGlow 2.4s ease-in-out infinite; }
/* Current (first not-yet-done) task: a spinning loading circle. */
.cp-loading { width: 14px; height: 14px; min-width: 14px; box-sizing: border-box; border-radius: 50%;
  border: 2px solid rgba(139,92,246,0.3); border-top-color: #8b5cf6;
  animation: cp-spin 1s linear infinite; display: inline-block; }
/* Minions render as a TREE: a single vertical trunk down the left with a
   horizontal elbow into each row. The trunk height is driven from JS and
   animates (stretch/shrink) as minions are added/removed. Per-row margin
   (not container gap) so a removed row can animate its spacing to 0 and let
   the rows below slide up smoothly. */
.cp-minions { position: relative; display: flex; flex-direction: column;
  margin: 6px 0 2px 0; padding-left: 22px; }
.cp-mrow:last-child { margin-bottom: 0; }
/* single trunk line — a child of .cp-body so it can run from the `>` output
   line (its L-shape origin) down to the last minion. left/top/height are all
   set imperatively in JS (layoutTrunk): the top anchor (just below the `>`,
   nudged left) is computed ONCE and cached so it never jitters; only the
   height changes as minions are added/removed. */
.cp-trunk { position: absolute; left: 16px; top: 0; width: 1px; height: 0;
  background: rgba(0,0,0,0.22); transform-origin: top;
  transition: height 0.34s cubic-bezier(.22,1,.36,1); will-change: height; }
.cp-trunk.draw-in { animation: cp-drawTrunk 0.34s ease forwards; }
/* min-height reserves the FILLED row height from the moment the row is added
   empty (loader only), so the panel doesn't grow a few px when the minion's
   first line arrives — that growth (then settle) was the bottom "jiggle". */
.cp-mrow { position: relative; display: flex; align-items: center; gap: 8px;
  min-height: 20px; margin-bottom: 6px; color: rgba(0,0,0,0.7); }
/* removed row: collapse height + spacing + fade so the rows below slide up.
   min-height:0 lets the height actually animate to 0 (the row's own 20px
   min-height would otherwise pin it open). */
.cp-mrow.removing { min-height: 0; overflow: hidden; pointer-events: none;
  transition: height 0.3s ease, margin-bottom 0.3s ease, opacity 0.26s ease; }
/* per-row horizontal elbow: starts at the trunk (~2px from the cp-body content
   edge) and stops ~4px short of the loader (row content edge at 22px) so there
   is a small gap before the loader. A transition (origin left) lets it draw in
   toward the loader (.elbow-in) and retract right-to-left on exit. */
.cp-mrow::after { content: ""; position: absolute; left: -20px; top: 50%;
  width: 16px; height: 1px; background: rgba(0,0,0,0.22);
  transform: scaleX(0); transform-origin: left; transition: transform 0.26s ease; }
.cp-mrow.elbow-in::after { transform: scaleX(1); }
.cp-mrow .mline { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  opacity: 0.85; transition: opacity 0.25s ease; }
/* orbiting 5-dot loader (port of coder_animation.html .loader/.orbe);
   faded in per-add via the .show class. */
.cp-loader { --size-loader: 14px; --size-orbe: 4px; width: var(--size-loader);
  height: var(--size-loader); min-width: var(--size-loader); position: relative;
  transform: rotate(45deg); display: inline-block; vertical-align: middle;
  opacity: 0; transition: opacity 0.3s ease; }
.cp-loader.show { opacity: 1; }
.cp-loader > i { position: absolute; width: 100%; height: 100%;
  --delay: calc(var(--index) * 0.1s);
  animation: cp-orbit 1.5s var(--delay) ease-in-out infinite;
  opacity: calc(1 - (0.2 * var(--index))); }
.cp-loader > i::after { content: ""; position: absolute; top: 0; left: 0;
  width: var(--size-orbe); height: var(--size-orbe); background-color: #8b5cf6;
  box-shadow: 0 0 6px 0 rgba(139,92,246,0.6); border-radius: 50%; }
.cp-progress { margin-top: 14px; height: 7px; border-radius: 999px; position: relative; overflow: hidden;
  background: rgba(0,0,0,0.06); border: 1px solid rgba(0,0,0,0.04); }
.cp-fill { position: absolute; inset: 0;
  background: linear-gradient(90deg, rgba(10,160,190,0), rgba(10,160,190,0.6), rgba(130,80,220,0.6), rgba(10,160,190,0));
  transform: translateX(-70%); animation: cp-flow 1.05s cubic-bezier(0.2,0.8,0.2,1) infinite; }
@keyframes cp-tickGlow { 0%,100%{filter:drop-shadow(0 0 1px rgba(139,92,246,0.55))} 50%{filter:drop-shadow(0 0 4px rgba(139,92,246,1))} }
@keyframes cp-spin { to { transform: rotate(360deg); } }
@keyframes cp-edgeGlow {
  0%, 100% { box-shadow: inset 0 0 0 1px rgba(139,92,246,0.45), inset 0 0 6px rgba(139,92,246,0.35); }
  50%      { box-shadow: inset 0 0 0 1px rgba(139,92,246,0.85), inset 0 0 12px rgba(139,92,246,0.6); }
}
@keyframes cp-orbit { 80%, 100% { transform: rotate(360deg); } }
@keyframes cp-drawTrunk { from { transform: scaleY(0); } to { transform: scaleY(1); } }
@keyframes cp-flow { 0%{transform:translateX(-75%);opacity:0.8} 50%{opacity:1} 100%{transform:translateX(75%);opacity:0.8} }

@keyframes stop-pulse  { 0%{transform:scale(.97)} 15%{transform:scale(1)} 30%{transform:scale(.98)} 45%{transform:scale(1)} 60%{transform:scale(.97)} 85%{transform:scale(1)} 100%{transform:scale(.97)} }
@keyframes stop-pulse2 { 0%{transform:scale(1)} 15%{transform:scale(1.03)} 30%{transform:scale(.98)} 45%{transform:scale(1.04)} 60%{transform:scale(.97)} 85%{transform:scale(1.03)} 100%{transform:scale(1)} }
@keyframes stop-bgRotate { 0%{transform:rotate(0)} 20%{transform:rotate(90deg)} 40%{transform:rotate(180deg) scale(.95,1)} 60%,100%{transform:rotate(360deg)} }
@keyframes stop-bgColor  { 20%{background-color:red} 40%{background-color:#5eff7e} 60%{background-color:#2cb5ff} 80%{background-color:#fc63ff} }
@keyframes stop-blink    { 0%,85%,100%{transform:scaleY(1)} 92%{transform:scaleY(.1)} }
@keyframes icon-cycle-pc { 0%, 40% { opacity: 1 } 50%, 90% { opacity: 0 } 100% { opacity: 1 } }
@keyframes icon-cycle-tg { 0%, 40% { opacity: 0 } 50%, 90% { opacity: 1 } 100% { opacity: 0 } }
</style></head>
<body>
<div class="toprow">
  <div class="orb-wrap">
    <iframe class="orb-frame" src="http://127.0.0.1:5000/telegram/telergam_animation.html"
            scrolling="no" frameborder="0"></iframe>
  </div>
  <span class="msg" id="msg"></span>
</div>
<div id="coderPanel"></div>
<script>
  // Stream `text` LETTER-BY-LETTER into the msg element. Long messages
  // are PAGED — after each letter we check if scrollWidth has exceeded
  // the visible line width; if so we yank the offending letter, hold
  // the line briefly so the user can read it, clear, and continue on a
  // fresh line. Loops until every letter has been displayed.
  //
  // First setMsg() of a task waits ~350 ms before streaming so the
  // NSPanel finishes its 44→440 width expansion before any text appears.
  // Subsequent calls (next agent step, banner already wide) start
  // immediately.
  const _CHAR_DELAY_MS = 4;      // per-TICK cadence (timers clamp to ~4ms)
  const _REVEAL_STEP = 5;        // letters revealed per tick — batching, not a
                                 // smaller delay, is what makes it stream fast
  const _FADE_MS = 60;           // per-letter fade-in duration
  const _PAGE_HOLD_MS = 400;     // how long a full line lingers before clearing
  let _revealTimer = null;

  function setMsg(fullText) {
    if (_revealTimer) { clearTimeout(_revealTimer); _revealTimer = null; }
    const el = document.getElementById('msg');
    if (!el) return;
    const text = (fullText || '').toString();
    const wasEmpty = !document.body.classList.contains('has-text');
    document.body.classList.toggle('has-text', !!text);
    el.textContent = '';
    if (!text) return;

    // Array.from splits by code point so 🧠 / 🎯 / etc. stay intact
    // (text.split('') would split them into surrogate halves).
    const chars = Array.from(text);
    let i = 0;

    const streamChar = () => {
      // Reveal up to _REVEAL_STEP letters this tick.
      for (let n = 0; n < _REVEAL_STEP; n++) {
        if (i >= chars.length) {
          // End of message — hold the final page briefly so the user can
          // read it, then clear and drop the has-text class so body
          // shrinks back to the 44×44 circle. A new setMsg() during the
          // hold cancels this timer (cleared at the top of setMsg).
          _revealTimer = setTimeout(() => {
            el.textContent = '';
            document.body.classList.remove('has-text');
            _revealTimer = null;
          }, _PAGE_HOLD_MS);
          return;
        }

        const span = document.createElement('span');
        span.textContent = chars[i];
        span.style.opacity = '0';
        span.style.transition = 'opacity ' + _FADE_MS + 'ms ease-out';
        el.appendChild(span);

        // Sync layout read forces reflow → we see whether this letter
        // overflowed the visible line. clientWidth is the live line width,
        // scrollWidth is the natural width of all spans appended so far. If
        // scrollWidth > clientWidth this letter doesn't fit — yank it out,
        // hold the line briefly, then resume on a fresh page from the same
        // letter.
        if (el.scrollWidth > el.clientWidth + 0.5) {
          // Defensive: single letter wider than the line (huge font?)
          // can't be paged out — keep it (overflow:hidden clips) and
          // advance so we don't loop forever.
          if (el.children.length === 1) {
            requestAnimationFrame(() => { span.style.opacity = '1'; });
            i++;
            continue;
          }
          el.removeChild(span);
          _revealTimer = setTimeout(() => {
            el.textContent = '';
            // Skip leading whitespace so the new page doesn't open with
            // a space gap.
            while (i < chars.length && /\\s/.test(chars[i])) i++;
            streamChar();
          }, _PAGE_HOLD_MS);
          return;
        }

        requestAnimationFrame(() => { span.style.opacity = '1'; });
        i++;
      }
      _revealTimer = setTimeout(streamChar, _CHAR_DELAY_MS);
    };

    const startDelay = wasEmpty ? 350 : 0;
    _revealTimer = setTimeout(streamChar, startDelay);
  }
  window.setMsg = setMsg;

  // Report body size to Python whenever it changes (basically just on
  // the empty ↔ has-text toggle, since the dimensions are fixed in
  // both states). Last-value debounce avoids a resize feedback loop.
  (function () {
    let lastW = -1, lastH = -1;
    const report = () => {
      const w = Math.ceil(document.body.getBoundingClientRect().width);
      const h = Math.ceil(document.body.getBoundingClientRect().height);
      // Ignore sub-2px height flutter so a content sub-pixel fluctuation can't
      // re-trigger the NSWindow resize animation (the shaky bottom edge). Real
      // content changes (a minion row / todo line ~24px) clear the threshold.
      if (w === lastW && Math.abs(h - lastH) < 2) return;
      lastW = w; lastH = h;
      try {
        webkit.messageHandlers.size_changed.postMessage({w: w, h: h});
      } catch (e) {}
    };
    window.addEventListener('load', () => setTimeout(report, 30));
    const ro = new ResizeObserver(report);
    ro.observe(document.body);
  })();

  // ── embedded coder terminal ──────────────────────────────────────────────
  // Faithful port of the web frontend's CLI pill (frontend/script.js). Every
  // incoming line is QUEUED and streamed letter-by-letter with pagination
  // (overflow -> hold -> clear -> continue), so the full real content flows by
  // rather than just the latest fragment. The top line shows the coder's REAL
  // output; only while a minion is running AND the coder is idle does it stream
  // playful filler phrases (CP_MSGS). Each minion row shows that minion's REAL
  // per-iteration output. Python toggles the panel via coderShow()/coderHide().
  (function () {
    const CHAR_STAGGER    = 8;     // ms between letters (fallback)
    const REAL_STAGGER    = 4;     // the agent's real output streams FAST
    const REAL_STEP       = 5;     // …and emits this many chars per tick (timers
                                   // clamp to ~4ms, so batching chars — not a
                                   // smaller delay — is what makes it fast)
    const FILLER_STAGGER  = 18;    // playful filler streams a little slower
    const CHAR_FADE       = 60;    // ms opacity fade-in per letter
    const PAGE_HOLD       = 550;   // ms a full page lingers before clearing
    const LINE_HOLD       = 260;   // ms between distinct lines
    const FILLER_IDLE     = 1800;  // ms of coder silence before filler starts
    const FILLER_HOLD     = 3000;  // ms a filler phrase lingers AFTER it finishes streaming

    // Playful "thinking" lines each minion row cycles through (one at a time).
    // A backtick template literal avoids escaping the many apostrophes/quotes;
    // trim()+filter() strips the code indentation and blank edges.
    const CP_MSGS = (`
      summoned minions…
      digging through the codebase…
      don't bother me, i'm busy still
      reticulating splines…
      consulting the rubber duck…
      untangling spaghetti…
      watching minions go brrr…
      this is fine, everything is fine…
      spinning up neurons…
      arguing with the linter…
      minions are reading… patience, human
      pondering the orb…
      grepping the unknown…
      feeding the hamsters…
      still thinking, hold tight…
      writing tiny love letters to stdout…
      asking stack overflow nicely…
      looking for the missing semicolon…
      blaming the intern…
      performing arcane git rituals…
      have you tried turning it off and on…
      just one more refactor, i promise…
      regex go brrr…
      negotiating with the type checker…
      Schrödinger's bug: works on my machine…
      pretending to understand recursion…
      this codebase has feelings too…
      arguing with prettier…
      i swear i tested this earlier…
      checking if it's a feature, not a bug…
      the code works, nobody knows why…
      reading documentation as last resort…
      blaming the cache…
      praying to the build gods…
      git blame says it was past me…
      compiling existential dread…
      tabs vs spaces war ongoing…
      training a goldfish to write tests…
      minions arguing over indentation…
      still cheaper than a senior dev…
      pushing to prod on a friday…
      one does not simply async in python…
      404: motivation not found…
      rewriting it in rust… mentally…
      petting the dog, brb…
      yelling politely at the json…
      checking if it's plugged in…
      the cake is a bug…
      explaining mondays to the AI…
      deploying vibes…
      this stack trace feels personal…
      i was promised flying cars, got jira…
      writing tests… eventually…
      minions on coffee break…
      this wasn't in the spec…
      ctrl-z is my therapist…
      running from technical debt…
      console.log debugger gang…
      the docs lied to us…
      trying to remember what i was doing…
      rebooting reality…
      yes, that's a feature now…
      speedrun: any% blame git…
      thinking too hard, please wait…
      yet another deeply nested if…
      promise resolved with disappointment…
      hot reload, cold coffee…
      aligning ducks in rows…
      one liner that took two hours…
      naming things, the hardest problem…
      off-by-one somewhere, definitely…
      loading more excuses…
      the bug is in another castle…
      your code is fine, the universe is broken…
      the linter has strong opinions…
      minion overheard saying lgtm…
      convincing the tests to pass…
      renaming the variable to fix it…
      drowning in callback hell…
      73 unread warnings, vibes only…
      yes it works, no i don't know why…
      minions found 47 todos, ignored all…
      scrolling error logs like reels…
      two minions, one task…
      sacrificing a keyboard to the demo gods…
      undoing the undo…
      reading the error message, finally…
      minions whispering to each other…
      the algorithm has thoughts…
      trying not to break prod…
      minion union meeting in progress…
      shaking the magic 8-ball…
      asking the cat for code review…
      running tests with fingers crossed…
      exorcising the legacy code…
      i promise this is the last bug…
      binary search through 200 tabs…
      feature creep is a feature now…
      minions found a TODO from 2014…
      deprecated, but still working…
      putting console.logs in production…
      redefining what "done" means…
    `).split('\\n').map((s) => s.trim()).filter(Boolean);

    const $ = (id) => document.getElementById(id);
    const escHtml = (s) => (s == null ? '' : String(s))
      .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
    const mid = (id) => 'cpm_' + String(id).replace(/[^a-zA-Z0-9_]/g, '_');
    const cid = (id) => 'cpc_' + String(id).replace(/[^a-zA-Z0-9_]/g, '_');

    // ── line runner: queue + paginated letter-by-letter streaming ──
    // Streams one queued line at a time into `outEl`, replacing its children
    // with one page; on overflow it holds, clears, and continues on a new page.
    // `prompt` (e.g. "> ") is re-rendered at the start of each page.
    function makeRunner() { return { queue: [], running: false, timer: null, onIdle: null }; }

    function pump(outEl, runner, prompt) {
      if (!outEl || runner.running || !runner.queue.length) return;
      runner.running = true;
      const item = runner.queue.shift();
      const text = (item && item.text != null) ? item.text : item;
      const stagger = (item && item.stagger) || CHAR_STAGGER;
      const step = (item && item.step) || 1;   // chars per tick (>1 = faster)
      const chars = Array.from(String(text));
      if (!chars.length) { runner.running = false; pump(outEl, runner, prompt); return; }

      let page = null;
      const startPage = () => {
        page = document.createElement('div');
        page.className = 'cp-line';
        if (prompt) {
          const p = document.createElement('span');
          p.className = 'cp-p'; p.textContent = prompt;
          page.appendChild(p);
        }
        outEl.replaceChildren(page);
      };
      startPage();

      let i = 0;
      const tick = () => {
        // Emit up to `step` chars this tick (timers clamp to ~4ms, so batching
        // — not a smaller stagger — is what makes real output stream fast).
        for (let n = 0; n < step; n++) {
          if (i >= chars.length) {
            runner.timer = setTimeout(() => {
              runner.running = false;
              if (runner.queue.length) {
                pump(outEl, runner, prompt);
              } else if (runner.onIdle) {
                const cb = runner.onIdle; runner.onIdle = null; cb();  // line finished, queue empty
              }
            }, LINE_HOLD);
            return;
          }
          const firstChar = page.querySelector('.cp-char') == null;
          const span = document.createElement('span');
          span.className = 'cp-char';
          span.textContent = chars[i];
          page.appendChild(span);
          if (page.scrollWidth > page.clientWidth + 1 && !firstChar) {
            page.removeChild(span);
            runner.timer = setTimeout(() => {
              startPage();
              while (i < chars.length && /\\s/.test(chars[i])) i++;  // drop leading space on the new page
              tick();
            }, PAGE_HOLD);
            return;
          }
          // First char of a page shows instantly (no empty flash); the rest fade.
          if (firstChar) {
            span.style.opacity = '1';
          } else {
            span.style.opacity = '0';
            span.style.transition = 'opacity ' + CHAR_FADE + 'ms ease-out';
            requestAnimationFrame(() => { span.style.opacity = '1'; });
          }
          i++;
        }
        runner.timer = setTimeout(tick, stagger);
      };
      tick();
    }

    function stopRunner(runner) {
      if (!runner) return;
      if (runner.timer) { clearTimeout(runner.timer); runner.timer = null; }
      runner.queue = []; runner.running = false; runner.onIdle = null;
    }

    // ── coder terminals + filler ──
    // Each coder (CLI agent) is its OWN independent terminal box (.cp-term) inside
    // #coderPanel, stacked one-below-the-other. Each box owns its output line, its
    // todo list, and its OWN minion tree (trunk + rows) — so two concurrent coders
    // never share a line, and every minion nests under the coder that spawned it.
    // All per-box state (runner, minion count, trunk geometry) hangs off the box.
    const filler = { active: false, bag: [], lastIdx: -1, idleTimer: null, tickTimer: null };

    const coderBoxes = () => Array.from(document.querySelectorAll('#coderPanel .cp-term'));
    const boxBody = (box) => box.querySelector('.cp-body');
    const boxOut = (box) => box.querySelector('.cp-out');
    const boxMinions = (box) => box.querySelector('.cp-minions');
    const boxTrunk = (box) => box.querySelector('.cp-trunk');
    const anyMinions = () => coderBoxes().some((b) => (b._minions || 0) > 0);

    function fillerPhrase() {
      if (!filler.bag.length) {
        const bag = CP_MSGS.map((_, i) => i);
        for (let i = bag.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); const x = bag[i]; bag[i] = bag[j]; bag[j] = x; }
        if (filler.lastIdx >= 0 && bag.length > 1 && bag[bag.length - 1] === filler.lastIdx) { const x = bag[bag.length - 1]; bag[bag.length - 1] = bag[0]; bag[0] = x; }
        filler.bag = bag;
      }
      const idx = filler.bag.pop(); filler.lastIdx = idx; return CP_MSGS[idx];
    }

    function stopFiller() {
      filler.active = false;
      if (filler.idleTimer) { clearTimeout(filler.idleTimer); filler.idleTimer = null; }
      if (filler.tickTimer) { clearTimeout(filler.tickTimer); filler.tickTimer = null; }
    }

    function scheduleFiller() {
      if (!anyMinions()) return;
      if (filler.idleTimer) clearTimeout(filler.idleTimer);
      filler.idleTimer = setTimeout(() => {
        filler.idleTimer = null;
        if (!anyMinions()) return;
        filler.active = true;
        // Fill the output line of every IDLE coder box that is waiting on its own
        // minions (it has nothing to print meanwhile). Boxes busy streaming real
        // output — or with no running minions — are skipped, so filler never
        // stomps live output. Each phrase lingers FILLER_HOLD before the next tick.
        const tickFiller = () => {
          if (!filler.active) return;
          coderBoxes().forEach((box) => {
            if ((box._minions || 0) <= 0) return;
            const out = boxOut(box); if (!out) return;
            if (!box._runner) box._runner = makeRunner();
            const r = box._runner;
            if (r.running || r.queue.length) return;
            r._filler = true;
            r.queue.push({ text: fillerPhrase(), stagger: FILLER_STAGGER, filler: true });
            pump(out, r, '> ');
          });
          filler.tickTimer = setTimeout(tickFiller, FILLER_HOLD);
        };
        tickFiller();
      }, FILLER_IDLE);
    }

    window.coderShow = function () {
      const wasHidden = !document.body.classList.contains('coder');
      document.body.classList.add('coder');
      if (!wasHidden) return;   // already visible — nothing was deferred
      // Boxes (and their minions/trunks) may have been created while the panel was
      // display:none, when all geometry measured as 0. Now that it's visible,
      // recompute EACH box's trunk against the real layout and reveal every
      // existing row, staggered, so it appears correct and animates in.
      coderBoxes().forEach((box) => {
        box._trunkTop = null; box._trunkLastH = -1;
        requestAnimationFrame(() => {
          const wrap = boxMinions(box);
          const trunk = boxTrunk(box);
          if (!wrap || !trunk) return;
          const rows = wrap.querySelectorAll('.cp-mrow');
          if (!rows.length) return;
          drawTrunkIn(box, trunk);
          rows.forEach((row, idx) => setTimeout(() => revealRow(row), 220 + idx * 140));
        });
      });
    };

    window.coderHide = function () {
      document.body.classList.remove('coder');
      stopFiller();
      filler.bag = []; filler.lastIdx = -1;
      const panel = $('coderPanel');
      if (panel) {
        panel.querySelectorAll('.cp-term').forEach((box) => {
          if (box._runner) stopRunner(box._runner);
          if (box._trunkRAF) cancelAnimationFrame(box._trunkRAF);
          box.querySelectorAll('.cp-mrow').forEach((row) => { if (row._runner) stopRunner(row._runner); });
        });
        panel.innerHTML = '';   // drop every coder terminal box
      }
    };

    // Each coder is its own independent terminal box (.cp-term) in #coderPanel,
    // stacked one-below-the-other, keyed by cid(id). The box owns its output line,
    // todos, and minion tree; all per-box state hangs off the element.
    window.addCoder = function (id) {
      const panel = $('coderPanel'); if (!panel || id == null) return;
      const sid = cid(id);
      if (document.getElementById(sid)) return;
      const box = document.createElement('div');
      box.className = 'cp-term'; box.id = sid;
      box.innerHTML =
        '<div class="cp-body">'
        + '<div class="cp-out"></div>'
        + '<div class="cp-todos hidden"></div>'
        + '<div class="cp-minions"></div>'
        + '<div class="cp-progress"><span class="cp-fill"></span></div>'
        + '</div>';
      panel.appendChild(box);
      box._runner = makeRunner();
      box._minions = 0;
      box._trunkTop = null; box._trunkLastH = -1; box._trunkRAF = null;
    };

    // A real coder line streams into THAT coder's own output line; real output
    // supersedes any filler phrase the box was showing.
    window.setCoderLine = function (id, text) {
      let box = document.getElementById(cid(id));
      if (!box) { window.addCoder(id); box = document.getElementById(cid(id)); }
      if (!box) return;
      const out = boxOut(box); if (!out) return;
      const t = (text == null ? '' : String(text));
      if (!t.trim()) return;
      if (!box._runner) box._runner = makeRunner();
      const r = box._runner;
      if (r._filler) { stopRunner(r); r._filler = false; }
      r.queue.push({ text: t, stagger: REAL_STAGGER, step: REAL_STEP });
      pump(out, r, '> ');
    };

    window.removeCoder = function (id) {
      const box = document.getElementById(cid(id));
      if (!box) return;
      if (box._runner) stopRunner(box._runner);
      if (box._trunkRAF) cancelAnimationFrame(box._trunkRAF);
      box.querySelectorAll('.cp-mrow').forEach((row) => { if (row._runner) stopRunner(row._runner); });
      if (box.parentNode) box.parentNode.removeChild(box);
      if (!anyMinions()) stopFiller();
    };

    window.setTodo = function (id, todoText) {
      const box = document.getElementById(cid(id)); if (!box) return;
      const el = box.querySelector('.cp-todos'); if (!el) return;
      const raw = (todoText == null ? '' : String(todoText)).split('\\n');
      const items = [];
      for (let i = 0; i < raw.length; i++) {
        const ln = raw[i].trim();
        if (!ln) continue;
        if (/^objective\\s*:/i.test(ln)) continue;
        let m = ln.match(/^#\\d+\\.\\s*-\\s*\\[([ xX])\\]\\s*(.*)$/);
        if (!m) m = ln.match(/^-\\s*\\[([ xX])\\]\\s*(.*)$/);
        if (!m) continue;
        items.push({ done: m[1].toLowerCase() === 'x', text: m[2] });
      }
      if (!items.length) {
        el.innerHTML = ''; el.classList.add('hidden');
        if (boxTrunk(box)) requestAnimationFrame(() => layoutTrunk(box));
        return;
      }
      // The current task is the first not-yet-done one: it gets the spinning
      // loading circle. Once it's marked done, the next pending task becomes
      // current. Done tasks show the breathing gradient box; the rest are empty.
      let activeIdx = -1;
      for (let k = 0; k < items.length; k++) { if (!items[k].done) { activeIdx = k; break; } }
      let html = '';
      for (let j = 0; j < items.length; j++) {
        let marker;
        if (items[j].done) marker = '<span class="cp-chk done"></span>';
        else if (j === activeIdx) marker = '<span class="cp-loading"></span>';
        else marker = '<span class="cp-chk"></span>';
        html += '<div class="cp-item">' + marker + '<span class="lbl">' + escHtml(items[j].text) + '</span></div>';
      }
      el.innerHTML = html;
      el.classList.remove('hidden');
      // todo rows just shifted the minions down -> re-anchor this box's trunk.
      if (boxTrunk(box)) requestAnimationFrame(() => layoutTrunk(box));
    };

    // ── per-coder minion tree (one trunk + per-row elbow, scoped to each box) ──
    // Each coder box owns its OWN trunk line. Its TOP anchor (just below that
    // box's `>` output line, with a small gap) is computed ONCE per box, ROUNDED,
    // and cached on the element (box._trunkTop) so it never jitters; only the
    // HEIGHT changes, tracked to that box's LAST non-removing row's centre. All
    // trunk state hangs off the box: box._trunkTop / _trunkLastH / _trunkRAF.
    function layoutTrunk(box) {
      if (!box) return;
      const body = boxBody(box);
      const out = boxOut(box);
      const wrap = boxMinions(box);
      const trunk = boxTrunk(box);
      if (!body || !out || !wrap || !trunk) return;
      // While the panel is display:none (minions can be added before the agent
      // halts at cli_await) every rect is 0 — measuring would cache a bogus top
      // and zero height. coderShow() lays each box's trunk out once it's visible.
      if (!document.body.classList.contains('coder')) return;
      const all = wrap.querySelectorAll('.cp-mrow');
      if (!all.length) { applyTrunkHeight(box, trunk, 0); return; }
      const bRect = body.getBoundingClientRect();
      if (box._trunkTop == null) {
        const oRect = out.getBoundingClientRect();
        box._trunkTop = Math.round((oRect.bottom - bRect.top) + 1);   // small gap below the `>`
        trunk.style.top = box._trunkTop + 'px';
      }
      // Track the last row that isn't being removed; while one collapses, fall
      // back to it so the trunk follows the collapse down to nothing.
      let last = null;
      for (let i = all.length - 1; i >= 0; i--) {
        if (!all[i].classList.contains('removing')) { last = all[i]; break; }
      }
      if (!last) last = all[all.length - 1];
      const lRect = last.getBoundingClientRect();
      const bottom = (lRect.top - bRect.top) + lRect.height / 2;
      applyTrunkHeight(box, trunk, bottom - box._trunkTop);
    }

    // Set the trunk height as a whole pixel, skipping no-op integer changes so
    // a stable layout never re-triggers the height transition (no shaky line).
    function applyTrunkHeight(box, trunk, h) {
      h = Math.max(0, Math.round(h));
      if (h === box._trunkLastH) return;
      box._trunkLastH = h;
      trunk.style.height = h + 'px';
    }

    // Drive the box's trunk height per-frame for `durationMs` (used during a row
    // collapse so the trunk shrinks in lock-step with the rows sliding up).
    function trackTrunk(box, durationMs) {
      const trunk = boxTrunk(box);
      if (!trunk) return;
      if (box._trunkRAF) cancelAnimationFrame(box._trunkRAF);
      trunk.style.transition = 'none';
      const startedAt = performance.now();
      const step = () => {
        layoutTrunk(box);
        if (performance.now() - startedAt < durationMs) {
          box._trunkRAF = requestAnimationFrame(step);
        } else {
          box._trunkRAF = null;
          trunk.style.transition = '';
        }
      };
      box._trunkRAF = requestAnimationFrame(step);
    }

    // Reveal a row: draw its elbow toward the loader, then fade the loader in.
    function revealRow(row) {
      if (!row) return;
      row.classList.add('elbow-in');
      const ld = row.querySelector('.cp-loader');
      setTimeout(() => { if (ld) ld.classList.add('show'); }, 160);
    }

    // Snap the box's trunk to full height, then play the scaleY draw-in from top.
    function drawTrunkIn(box, trunk) {
      if (!trunk) return;
      trunk.style.transition = 'none';
      layoutTrunk(box);
      void trunk.offsetHeight;
      trunk.style.transition = '';
      trunk.classList.add('draw-in');
      trunk.addEventListener('animationend',
        () => trunk.classList.remove('draw-in'), { once: true });
    }

    window.addMinion = function (parentId, id) {
      // Resolve the parent coder's box; create it defensively if a minion's
      // start somehow beats its coder's task_start.
      let box = parentId != null ? document.getElementById(cid(parentId)) : null;
      if (!box && parentId != null) { window.addCoder(parentId); box = document.getElementById(cid(parentId)); }
      if (!box || id == null) return;
      const wrap = boxMinions(box); if (!wrap) return;
      const sid = mid(id);
      if (document.getElementById(sid)) return;

      // One trunk line PER BOX, lazily created as a child of that box's .cp-body
      // so it can run up to the `>` output line. The first one draws-in via scaleY.
      const body = boxBody(box);
      let trunk = boxTrunk(box);
      const firstTrunk = !trunk;
      if (firstTrunk && body) {
        trunk = document.createElement('span');
        trunk.className = 'cp-trunk';
        body.appendChild(trunk);
      }

      const row = document.createElement('div');
      row.className = 'cp-mrow'; row.id = sid;
      // The orbiting 5-dot loader + the row's REAL output line (queued +
      // paginated) so the full thinking/goal/action flows by.
      row.innerHTML =
        '<span class="cp-loader">'
        + '<i style="--index:0"></i><i style="--index:1"></i><i style="--index:2"></i>'
        + '<i style="--index:3"></i><i style="--index:4"></i>'
        + '</span><span class="mline"></span>';
      wrap.appendChild(row);
      row._runner = makeRunner();

      // Lay out + animate only if the panel is visible. If it's still hidden
      // (the agent spawned this minion before halting at cli_await), defer: all
      // geometry measures as 0 while display:none. coderShow() recomputes each
      // box's trunk and reveals every deferred row once the panel appears.
      if (document.body.classList.contains('coder')) {
        requestAnimationFrame(() => {
          if (firstTrunk && trunk) drawTrunkIn(box, trunk);  // first trunk: scaleY draw-in
          else layoutTrunk(box);                             // others: height transition
          setTimeout(() => revealRow(row), 340);
        });
      }

      // A running minion means this coder's line may go idle -> arm the filler.
      box._minions = (box._minions || 0) + 1;
      scheduleFiller();
    };

    window.setMinionLine = function (id, text) {
      const row = document.getElementById(mid(id)); if (!row) return;
      const ml = row.querySelector('.mline'); if (!ml) return;
      const t = (text == null ? '' : String(text));
      if (!t.trim()) return;
      if (!row._runner) row._runner = makeRunner();
      row._runner.queue.push({ text: t, stagger: REAL_STAGGER, step: REAL_STEP });
      pump(ml, row._runner);
    };

    // Smooth exit: (1) retract the elbow right-to-left, (2) collapse the row's
    // height + spacing so the rows below slide up, with the trunk shrinking in
    // step, (3) drop the row (and the trunk if it was the last one).
    const COLLAPSE_MS = 300;
    window.removeMinion = function (id) {
      const row = document.getElementById(mid(id));
      if (!row) return;
      if (row.classList.contains('removing')) return;
      if (row._runner) stopRunner(row._runner);

      const box = row.closest('.cp-term');
      if (box) box._minions = Math.max(0, (box._minions || 0) - 1);
      if (!anyMinions()) stopFiller();

      const hasRowsBelow = !!row.nextElementSibling;
      const prevRow = row.previousElementSibling;
      const trunk = box ? boxTrunk(box) : null;

      // (1) retract the elbow (origin left -> shrinks right-to-left) and fade.
      row.classList.add('removing');
      row.classList.remove('elbow-in');
      const loader = row.querySelector('.cp-loader');
      if (loader) loader.classList.remove('show');

      // (2) once the elbow has retracted, collapse the row so siblings slide up.
      setTimeout(() => {
        row.style.height = row.offsetHeight + 'px';
        void row.offsetHeight;                 // commit start height
        row.style.height = '0px';
        row.style.marginBottom = '0px';
        row.style.opacity = '0';

        if (box && trunk) {
          if (hasRowsBelow) {
            trackTrunk(box, COLLAPSE_MS + 40);  // follow the sliding rows per-frame
          } else if (prevRow) {
            const body = boxBody(box);
            const bRect = body.getBoundingClientRect();
            const pr = prevRow.getBoundingClientRect();
            const newBottom = (pr.top - bRect.top) + pr.height / 2;
            trunk.style.transition = '';        // smooth shrink to the new last row
            applyTrunkHeight(box, trunk, newBottom - (box._trunkTop != null ? box._trunkTop : 0));
          } else {
            trunk.style.transition = '';
            applyTrunkHeight(box, trunk, 0);     // only row -> shrink to nothing
          }
        }

        // (3) drop the row; drop+reset this box's trunk if none remain.
        setTimeout(() => {
          if (row.parentNode) row.parentNode.removeChild(row);
          if (box) {
            const minions = boxMinions(box);
            const tr = boxTrunk(box);
            if (minions && tr) {
              if (!minions.querySelector('.cp-mrow')) {
                if (tr.parentNode) tr.parentNode.removeChild(tr);
                box._trunkTop = null; box._trunkLastH = -1;
              } else {
                layoutTrunk(box);
              }
            }
          }
        }, COLLAPSE_MS + 60);
      }, 240);
    };
  })();
</script>
</body></html>
"""


# =============================================================================
# The banner process (python -m AutoCua.linux.remote_connection.banner)
# =============================================================================

# What the page posts to. The compact pill only has size_changed; the
# standard banner has the full set (see BANNER_HTML).
_BRIDGE = ("next_clicked", "choice_clicked", "save_clicked", "reveal_done",
           "height_changed", "size_changed")

# AutoCua_BANNER_DEBUG=1 makes the banner process narrate every resize on
# stderr (what the page asked for, what the window became).
_DEBUG = bool(os.environ.get("AutoCua_BANNER_DEBUG"))


def _emit(event, **kwargs):
    """Banner process → parent: one JSON line on stdout."""
    payload = {"event": event}
    payload.update(kwargs)
    try:
        sys.stdout.write(json.dumps(payload) + "\n")
        sys.stdout.flush()
    except Exception:
        pass


def _run_subprocess_banner():
    """The banner process. Builds the window, serves the page, relays the
    page's clicks to stdout and the parent's JS to the page until CLOSE or
    stdin-EOF (the parent died — go with it)."""
    compact = "--compact" in sys.argv
    try:
        import gi
    except ImportError:
        # gi is a system package the venv cannot see; the scanner knows
        # where it lives (element._add_system_gi_to_path).
        import importlib
        importlib.import_module("AutoCua.linux.tree.element")
        import gi
    gi.require_version("Gtk", "3.0")
    gi.require_version("Gdk", "3.0")
    gi.require_version("WebKit2", "4.1")
    from gi.repository import Gtk, Gdk, GLib, WebKit2
    try:
        import cairo
    except ImportError:
        cairo = None

    G = StatusBanner        # the geometry constants
    if compact:
        w_px, h_px = G.COMPACT_MIN_W, G.COMPACT_MIN_H
        html = COMPACT_HTML
    else:
        w_px, h_px = G.W, G.MIN_H
        html = BANNER_HTML
    state = {"w": w_px, "h": h_px, "coder": False, "ready": False,
             "pending": []}

    win = Gtk.Window(title="AutoCua")
    win.set_decorated(False)
    win.set_keep_above(True)
    win.set_skip_taskbar_hint(True)
    win.set_skip_pager_hint(True)
    win.set_focus_on_map(False)      # never take focus from the browser
    win.stick()                      # on every workspace
    screen = win.get_screen()
    visual = screen.get_rgba_visual()
    if visual is not None:
        win.set_visual(visual)       # transparent corners outside the pill

    # The pill body: white at 0.96 with rounded corners, like the macOS
    # panel's content layer. The page itself is transparent.
    css = Gtk.CssProvider()

    def _style(corner):
        css.load_from_data(
            f"window {{ background-color: rgba(255,255,255,0.96); "
            f"border-radius: {corner:.1f}px; }}".encode())

    _style(h_px / 2.0)
    Gtk.StyleContext.add_provider_for_screen(
        screen, css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)

    ucm = WebKit2.UserContentManager()
    wv = WebKit2.WebView.new_with_user_content_manager(ucm)
    wv.set_background_color(Gdk.RGBA(0, 0, 0, 0))
    # GPU compositing, WebKitGTK's default. Its software path was tried for
    # the orb's blur filters and bleached the 42-px orb into a white disc
    # (measured against Firefox and macOS: a violet disc); at this size the
    # GPU path draws it right.
    win.add(wv)

    # Anchor to the PRIMARY monitor's top-right corner (the one with the top
    # bar), whatever the browser is doing on another screen.
    display = Gdk.Display.get_default()
    mon = display.get_primary_monitor() or display.get_monitor(0)
    geo = mon.get_geometry()
    right = geo.x + geo.width - G.RIGHT_MARGIN
    top = geo.y + G.TOP_MARGIN

    def _click_through():
        # The compact pill is purely visual: clicks fall through to
        # whatever the agent is working on underneath it.
        if not compact or cairo is None:
            return
        gw = win.get_window()
        if gw is not None:
            gw.input_shape_combine_region(cairo.Region(), 0, 0)

    def _apply_size(new_w, new_h, corner):
        """Resize keeping the top-right corner where it is.

        One atomic GDK move_resize, not gtk resize()+move(): measured on
        X11 under Mutter, the pair grows fine but after a SHRINK the move
        is dropped — the 44x44 orb stayed at the wide pill's left edge
        (x=1480) instead of returning to the corner (x=1856)."""
        if new_w == state["w"] and new_h == state["h"]:
            return
        state["w"], state["h"] = new_w, new_h
        win.set_size_request(new_w, new_h)
        gw = win.get_window()
        if gw is not None:
            gw.move_resize(right - new_w, top, new_w, new_h)
        else:
            win.resize(new_w, new_h)
            win.move(right - new_w, top)
        _style(corner)
        _click_through()
        if _DEBUG:
            def _report(tag=f"{new_w}x{new_h}"):
                print(f"banner: asked {tag} -> gtk {tuple(win.get_size())} "
                      f"at {tuple(win.get_position())}", file=sys.stderr, flush=True)
                return False
            GLib.timeout_add(400, _report)

    def _on_message(_ucm, result, name):
        try:
            val = result.get_js_value()
            if name == "next_clicked":
                _emit("NEXT")
            elif name == "choice_clicked":
                _emit("CHOICE", value=val.to_string())
            elif name == "save_clicked":
                _emit("SAVE", value=val.to_string())
            elif name == "reveal_done":
                _emit("REVEAL")
            elif name == "height_changed" and not compact:
                # Standard banner: width is fixed, height follows the text.
                new_h = max(G.MIN_H, min(int(val.to_double()), G.MAX_H))
                _apply_size(state["w"], new_h, G.MIN_H / 2.0)
            elif name == "size_changed" and compact:
                d = json.loads(val.to_json(0) or "{}")
                if _DEBUG:
                    print(f"banner: page size_changed {d}", file=sys.stderr, flush=True)
                if state["coder"]:
                    max_w, max_h = G.COMPACT_CODER_MAX_W, G.COMPACT_CODER_MAX_H
                else:
                    max_w, max_h = G.COMPACT_MAX_W, G.COMPACT_MAX_H
                new_w = max(G.COMPACT_MIN_W, min(int(d.get("w", 0)), max_w))
                new_h = max(G.COMPACT_MIN_H, min(int(d.get("h", 0)), max_h))
                # A circle at 44x44, a stadium as the ticker widens, a card
                # while the coder panel is up.
                corner = 16.0 if state["coder"] else min(new_w, new_h) / 2.0
                _apply_size(new_w, new_h, corner)
        except Exception as e:
            print(f"banner: {name} handler failed ({e})", file=sys.stderr, flush=True)

    for name in (("size_changed",) if compact else _BRIDGE):
        ucm.register_script_message_handler(name)
        ucm.connect(f"script-message-received::{name}", _on_message, name)

    def _run_js(js):
        # JS that arrives before the page has loaded would find no setMsg and
        # no #msg — hold it and replay it on load.
        if not state["ready"]:
            state["pending"].append(js)
            return
        try:
            # WebKitGTK 2.40+; run_javascript is the deprecated older spelling.
            wv.evaluate_javascript(js, -1, None, None, None, None, None)
        except Exception:
            try:
                wv.run_javascript(js, None, None, None)
            except Exception as e:
                print(f"banner: JS failed ({e})", file=sys.stderr, flush=True)

    def _on_load(_wv, event):
        if event == WebKit2.LoadEvent.FINISHED and not state["ready"]:
            state["ready"] = True
            pending, state["pending"] = state["pending"], []
            for js in pending:
                _run_js(js)
            _emit("READY")

    wv.connect("load-changed", _on_load)

    def _quit():
        _emit("CLOSED")
        Gtk.main_quit()
        return False

    def _reader():
        try:
            for line in sys.stdin:
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                cmd = msg.get("cmd")
                if cmd == "JS":
                    GLib.idle_add(_run_js, msg.get("js", ""))
                elif cmd == "CODER":
                    state["coder"] = bool(msg.get("active"))
                elif cmd == "CLOSE":
                    break
        except Exception:
            pass
        GLib.idle_add(_quit)

    threading.Thread(target=_reader, daemon=True).start()

    win.set_size_request(w_px, h_px)
    win.resize(w_px, h_px)
    win.move(right - w_px, top)
    win.connect("destroy", lambda *_a: Gtk.main_quit())
    wv.load_html(html, None)
    win.show_all()
    _click_through()
    Gtk.main()


# =============================================================================
# The parent side
# =============================================================================

_LIVE = weakref.WeakSet()


def close_all_banners():
    """Close every banner this process opened — the app's closing hook."""
    for banner in list(_LIVE):
        try:
            banner.close()
        except Exception:
            pass


class StatusBanner:
    W, MIN_H, MAX_H, TOP_MARGIN, RIGHT_MARGIN = 440, 44, 200, 56, 20
    # Compact variant: starts as a 44×44 white circle (orb only). When
    # streaming text arrives the pill grows rightward into a 440×44
    # stadium — a single-line ticker. Height never changes. Long messages
    # page through one line at a time (fill, pause, clear, continue).
    # Top-right corner stays anchored so width grows leftward only.
    COMPACT_MIN_W = 44      # square → circle when only the orb is visible
    COMPACT_MIN_H = 44
    COMPACT_MAX_W = 440     # = self.W (the setup-wizard banner's max width)
    COMPACT_MAX_H = 44      # single-line height — pill never grows taller

    # Coder terminal: while a CLI/coder sub-agent runs, the compact orb pill
    # expands (wider + taller) to host an embedded terminal panel BELOW the orb
    # (streamed lines + todo checklist + minion rows). Same single top-right
    # anchored window — it just grows leftward and downward. These bigger
    # clamps apply inside the banner process while the panel is active.
    COMPACT_CODER_MAX_W = 560
    COMPACT_CODER_MAX_H = 470

    # Cap the wait-for-reveal so a JS hiccup that drops the reveal_done
    # message can never deadlock us. Realistic banner messages stream out
    # in well under this — and shorter is better, because the wait is what
    # the user experiences between the message finishing and the button
    # showing.
    _REVEAL_WAIT_SEC = 3.0

    def __init__(self, compact: bool = False):
        self._compact = compact
        self._coder_active = False  # True while the embedded terminal panel shows
        self._proc = None
        self._alive = False
        self._lock = threading.Lock()
        self._next_event = threading.Event()
        self._choice_event = threading.Event()
        self._save_event = threading.Event()
        # Set initially: no streaming reveal is pending until update() is called.
        # update() clears this; the page's reveal_done re-sets it.
        self._reveal_event = threading.Event()
        self._reveal_event.set()
        self._choice_value = None
        self._save_value = ""

    # ---- public API (callable from any thread) ----

    def show(self):
        if self._proc is not None:
            return
        argv = [sys.executable, "-m", "AutoCua.linux.remote_connection.banner"]
        if self._compact:
            argv.append("--compact")
        env = dict(os.environ)
        env["GDK_BACKEND"] = "x11"       # see the module docstring
        env["PYTHONUNBUFFERED"] = "1"
        env["PYTHONPATH"] = str(_REPO_ROOT) + (
            os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else "")
        try:
            self._proc = subprocess.Popen(
                argv, cwd=str(_REPO_ROOT), env=env,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                text=True, bufsize=1)
        except Exception as e:
            logger.warning(f"banner: could not start the banner process ({e}); popup disabled")
            self._proc = None
            return
        self._alive = True
        _LIVE.add(self)
        threading.Thread(target=self._read_events, daemon=True).start()

    def update(self, text):
        if not self._alive:
            return
        # A streaming reveal is about to start in the page; clear the event so
        # any following wait_for_* call blocks until it posts reveal_done. Only
        # relevant in standard mode (compact pill never waits on reveals).
        if not self._compact:
            self._reveal_event.clear()
        safe = self._js_escape(text)
        # Primary path: hand the full text to setMsg, which animates it
        # word-by-word and fires reveal_done when finished. Fallback if the
        # page-side script is not there: set the text directly and post
        # reveal_done ourselves so wait_for_next doesn't sit on its timeout.
        self._js(f"if (window.setMsg) {{ setMsg('{safe}'); }}"
                 f" else {{"
                 f"   var m = document.getElementById('msg');"
                 f"   if (m) m.textContent = '{safe}';"
                 f"   try {{ webkit.messageHandlers.reveal_done.postMessage(1); }}"
                 f"   catch (e) {{}}"
                 f" }}")

    def _await_reveal(self):
        """Block until the most recent update()'s reveal animation has
        finished (or the safety timeout fires). No-op if no update() is
        pending — the event stays set in that case."""
        self._reveal_event.wait(self._REVEAL_WAIT_SEC)

    def wait_for_next(self, timeout=None):
        """Block calling thread until user clicks Next (or timeout). Returns True if clicked.

        Shows the Next button on entry and hides it on exit, so during normal
        update() calls the button stays hidden — only the entry/exit boundaries
        of a wait_for_next show a clickable Next.
        """
        if not self._alive:
            return True  # no banner → don't block forever
        if self._compact:
            # No Next button in compact mode — return immediately so callers
            # that accidentally chain it don't hang forever.
            return True
        # Clear the click event BEFORE the reveal wait, so a click that lands
        # during streaming is not wiped afterwards.
        self._next_event.clear()
        self._await_reveal()
        self._clear_extra_ui()
        self._set_next_visible(True)
        clicked = self._next_event.wait(timeout)
        self._set_next_visible(False)
        return clicked

    def wait_for_choice(self, left_label, right_label, timeout=None):
        """Show two side-by-side buttons; block until one is clicked.
        Returns 'left' or 'right', or None on timeout / no banner."""
        if not self._alive or self._compact:
            return None
        self._choice_event.clear()
        self._choice_value = None
        self._await_reveal()
        self._set_next_visible(False)
        l, r = self._js_escape(left_label), self._js_escape(right_label)
        self._js(f"if (window.setChoice) setChoice('{l}', '{r}');")
        clicked = self._choice_event.wait(timeout)
        value = self._choice_value if (clicked and self._alive) else None
        self._clear_extra_ui()
        return value

    def wait_for_input(self, save_label="Save", validate=None,
                       error_msg="Token can't be empty"):
        """Show a text input + Save button; block until user submits a value
        that passes `validate` (default: non-empty after strip). Failed
        validation surfaces `error_msg` in red below the input and keeps
        waiting. Returns the accepted value, or None on no banner."""
        if not self._alive or self._compact:
            return None
        if validate is None:
            validate = lambda v: bool((v or "").strip())
        self._save_event.clear()
        self._await_reveal()
        self._set_next_visible(False)
        self._js(f"if (window.setInput) setInput('{self._js_escape(save_label)}');")
        try:
            while True:
                self._save_event.wait()
                # A banner torn down out from under us sets the event too.
                if not self._alive:
                    return None
                value = self._save_value or ""
                if validate(value):
                    return value
                self._js(f"if (window.setInputError) setInputError('{self._js_escape(error_msg or '')}');")
                self._save_event.clear()
        finally:
            self._clear_extra_ui()

    def close(self):
        proc = self._proc
        if proc is None:
            return
        self._send({"cmd": "CLOSE"})
        try:
            proc.wait(timeout=2)
        except Exception:
            try:
                proc.terminate()
                proc.wait(timeout=1)
            except Exception:
                pass
        self._proc = None
        self._release()

    # ---- the pipe ----

    def _send(self, payload):
        proc = self._proc
        if proc is None or not self._alive:
            return
        try:
            with self._lock:
                proc.stdin.write(json.dumps(payload) + "\n")
                proc.stdin.flush()
        except Exception:
            self._release()

    def _js(self, js):
        self._send({"cmd": "JS", "js": js})

    def _read_events(self):
        proc = self._proc
        try:
            for line in proc.stdout:
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                ev = msg.get("event")
                if ev == "NEXT":
                    self._next_event.set()
                elif ev == "CHOICE":
                    self._choice_value = msg.get("value")
                    self._choice_event.set()
                elif ev == "SAVE":
                    self._save_value = msg.get("value") or ""
                    self._save_event.set()
                elif ev == "REVEAL":
                    self._reveal_event.set()
                elif ev == "CLOSED":
                    break
        except Exception:
            pass
        finally:
            self._release()

    def _release(self):
        """The banner is gone: nobody may keep waiting on it."""
        self._alive = False
        for ev in (self._next_event, self._choice_event,
                   self._save_event, self._reveal_event):
            ev.set()
        _LIVE.discard(self)

    # ---- page helpers ----

    @staticmethod
    def _js_escape(text):
        return (str(text)
                .replace("\\", "\\\\")
                .replace("'", "\\'")
                .replace("\n", " ")
                .replace("\r", " "))

    @staticmethod
    def _js_multiline(text):
        """Escape a string for a single-quoted JS literal while PRESERVING
        newlines as \\n (so JS-side .split('\\n') still works). Used for the
        todo blob; _js_escape collapses newlines and must not be used for
        multi-line payloads."""
        return (str(text)
                .replace("\\", "\\\\")
                .replace("'", "\\'")
                .replace("\r", "")
                .replace("\n", "\\n"))

    def _set_next_visible(self, visible):
        disp = "inline-block" if visible else "none"
        self._js(f"var b=document.getElementById('next'); if (b) b.style.display='{disp}';")

    def _clear_extra_ui(self):
        self._js("if (window.clearAll) clearAll();")

    # ---- embedded coder terminal API (compact mode; callable from any thread) ----
    #
    # While a CLI/coder sub-agent runs, the compact orb pill expands to host a
    # terminal panel below the orb. These methods drive the JS in COMPACT_HTML.
    # All are no-ops unless this is a compact banner.

    def coder_start(self):
        """Reveal the embedded terminal panel (the pill expands wider+taller)."""
        if not self._alive or not self._compact:
            return
        self._coder_active = True
        self._send({"cmd": "CODER", "active": True})
        self._js("if (window.coderShow) coderShow();")

    def coder_stop(self):
        """Hide the terminal panel and collapse the pill back to the orb pill."""
        if not self._alive or not self._compact:
            return
        self._coder_active = False
        self._send({"cmd": "CODER", "active": False})
        self._js("if (window.coderHide) coderHide();")

    def add_coder(self, coder_id, label="coder"):
        """Add a streaming row for a coder (CLI agent). One row per coder so
        concurrent coders stack one-below-the-other instead of sharing a line."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.addCoder) addCoder('{self._js_escape(coder_id)}');")

    def push_cli_line(self, coder_id, line):
        """Stream one of a coder's real output lines into THAT coder's own row."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.setCoderLine) setCoderLine('{self._js_escape(coder_id)}', '{self._js_escape(line)}');")

    def remove_coder(self, coder_id):
        """Remove a coder's row (it finished)."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.removeCoder) removeCoder('{self._js_escape(coder_id)}');")

    def set_todo(self, coder_id, todo_text):
        """(Re)render a coder's todo checklist inside its own terminal box."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.setTodo) setTodo('{self._js_escape(coder_id)}', '{self._js_multiline(todo_text)}');")

    def add_minion(self, parent_id, minion_id, label="minion"):
        """Add a spinner row for a minion, nested inside its parent coder's box."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.addMinion) addMinion('{self._js_escape(parent_id)}', '{self._js_escape(minion_id)}');")

    def set_minion_line(self, minion_id, line):
        """Stream the minion's latest real output line into its row."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.setMinionLine) setMinionLine('{self._js_escape(minion_id)}', '{self._js_escape(line)}');")

    def remove_minion(self, minion_id):
        """Remove a minion row (it exited)."""
        if not self._alive or not self._compact:
            return
        self._js(f"if (window.removeMinion) removeMinion('{self._js_escape(minion_id)}');")


class CoderBannerManager:
    """Turns the agent's `cli_callback` event stream into the embedded terminal
    panel inside the compact orb banner.

    It does NOT own a window — it drives an existing compact StatusBanner (the
    "task in progress" orb): while a CLI/coder sub-agent runs, the orb pill
    expands to show a terminal panel (streamed lines + todo checklist + minion
    spinner rows), then collapses back when the CLI agent is done.

    Surface-agnostic (knows nothing about Telegram). A caller constructs it with
    the compact banner and passes `cli_callback=mgr.handle_event` to
    AgentService, then calls `mgr.close_all()` when the run finishes.

    Lifecycle — the panel stays up for the whole cli_await halt and only closes
    when the CLI agent is actually done:

        await_start(reason)                        main agent halts → SHOW panel
        task_start(task_id, desc)                  coder begins      → track only
                                                   (panel waits for cli_await so it
                                                    can't occlude live OS actions)
        task_line(task_id, line, stream)           coder stdout      → stream the top line
                                                   (filler phrases fill in only
                                                    while a minion runs + coder idle)
        todo_update(task_id, todo_text)            todo changed      → render checklist
        task_end(task_id, status, summary)         coder done        → hide iff no await
        minion_start(parent_id, m_id, query)       minion begins     → add spinner row
        minion_line(m_id, line, stream)            minion stdout     → stream into the row
        minion_end(m_id, status, summary)          minion done       → remove the row
        await_end()                                halt over         → hide panel

    `task_start` whose description starts with "[minion] " is a bare top-level
    minion (no coder stream) and is ignored; coder-spawned minions arrive as
    `minion_*`. Every StatusBanner method just writes a line to the banner
    process, so handle_event is safe to call from the agent/pipe-reader
    threads.
    """

    def __init__(self, banner):
        self._banner = banner            # the compact StatusBanner (orb pill)
        self._lock = threading.Lock()
        self._coder_tasks = set()        # active coder task_ids (own a terminal stream)
        self._minion_ids = set()         # active minion ids (own a spinner row)
        self._await_active = False       # True between await_start and await_end

    def handle_event(self, event_type, *args):
        try:
            self._dispatch(event_type, args)
        except Exception:
            logger.warning("CoderBannerManager.handle_event(%s) failed", event_type, exc_info=True)

    def close_all(self):
        """Hide the panel and reset (call from the caller's finally block)."""
        with self._lock:
            self._coder_tasks.clear()
            self._minion_ids.clear()
            self._await_active = False
        try:
            self._banner.coder_stop()
        except Exception:
            pass

    def _dispatch(self, event_type, args):
        if event_type == "await_start":
            with self._lock:
                self._await_active = True
            self._banner.coder_start()

        elif event_type == "await_end":
            with self._lock:
                self._await_active = False
            self._banner.coder_stop()

        elif event_type == "task_start":
            task_id = args[0] if len(args) > 0 else None
            desc = args[1] if len(args) > 1 else ""
            if task_id is None:
                return
            if isinstance(desc, str) and desc.startswith("[minion] "):
                return  # bare top-level minion — no coder stream of its own
            with self._lock:
                self._coder_tasks.add(task_id)
            # Create this coder's own stacked row (keyed by task_id). Built even
            # while the panel is hidden — coderShow reveals it later, like minions.
            self._banner.add_coder(task_id, desc)
            # Deliberately do NOT show the panel here. A cli_agent dispatch is
            # non-blocking — the main agent keeps performing on-screen OS actions
            # until it runs cli_await. Expanding the panel now would occlude the
            # screen the agent is automating. The panel expands only on
            # await_start (main agent halted); lines stream into it meanwhile.

        elif event_type == "task_line":
            task_id = args[0] if len(args) > 0 else None
            line = args[1] if len(args) > 1 else ""
            with self._lock:
                is_coder = task_id in self._coder_tasks
            if is_coder and line is not None and str(line).strip():
                self._banner.push_cli_line(task_id, str(line))

        elif event_type == "todo_update":
            parent_id = args[0] if len(args) > 0 else None
            todo_text = args[1] if len(args) > 1 else ""
            self._banner.set_todo(parent_id, str(todo_text))

        elif event_type == "task_end":
            task_id = args[0] if len(args) > 0 else None
            with self._lock:
                self._coder_tasks.discard(task_id)
                # Hide only if no cli_await is holding the panel open and no
                # other coder is still streaming — otherwise keep it up until
                # await_end (the CLI agent isn't "done" until the halt lifts).
                hide = (not self._await_active) and (not self._coder_tasks)
            # Drop this coder's row whether or not the panel hides (other coders
            # or a cli_await may keep it open).
            self._banner.remove_coder(task_id)
            if hide:
                self._banner.coder_stop()

        elif event_type == "minion_start":
            parent_id = args[0] if len(args) > 0 else None
            minion_id = args[1] if len(args) > 1 else None
            if minion_id is None:
                return
            with self._lock:
                self._minion_ids.add(minion_id)
            # Nest the minion row inside its PARENT coder's terminal box. Do NOT
            # expand the panel (same reason as task_start — a minion can spawn
            # before the agent halts at cli_await); it reveals on await_start.
            self._banner.add_minion(parent_id, minion_id, "minion")

        elif event_type == "minion_line":
            minion_id = args[0] if len(args) > 0 else None
            line = args[1] if len(args) > 1 else ""
            with self._lock:
                known = minion_id in self._minion_ids
            if known and line is not None and str(line).strip():
                self._banner.set_minion_line(minion_id, str(line))

        elif event_type == "minion_end":
            minion_id = args[0] if len(args) > 0 else None
            with self._lock:
                self._minion_ids.discard(minion_id)
            self._banner.remove_minion(minion_id)

        # pill_web_loading_* and other events: nothing to do.

if __name__ == "__main__":
    _run_subprocess_banner()
