"""Universal floating pill/orb banner — surface-agnostic.

This module lives OUTSIDE any single remote-connection surface
(telegram/, discord/, whatsapp/) on purpose: the pill + orb are the
*visual engine*, written once and reused by every surface. The
surface-specific behaviour (what text to show, the setup wizard steps,
the agent stream) lives in each surface's own folder and drives this
banner over the JSON-over-stdio protocol below. Telegram is the first
caller; Discord/WhatsApp can import `StatusBanner` unchanged.

Like the old telegram banner, this module plays two roles in one file:

  1. **Imported** (from a surface's setup.py / service.py) — exposes the
     `StatusBanner` class. Side-effect-free: PySide6 is NOT imported at
     module load, only inside `_run_subprocess_banner`, which the parent
     never calls.

  2. **Run as `python -m …banner`** (spawned by `StatusBanner.show()`
     via `subprocess.Popen`, or re-exec'd as `AutoCua.exe --banner-mode`
     in the compiled build) — falls through `if __name__ == "__main__"`
     into `_run_subprocess_banner`, which boots a PySide6
     `QWebEngineView` hosting the pill HTML and parks on `app.exec()`.
     Reads JSON commands from stdin, emits JSON events on stdout.

Why a subprocess? Running a second webview window inside the already-
running AutoCua process landed the pill off-screen on DPI-scaled
displays; a fresh interpreter (the subprocess) dodges that confusion.

Why Qt WebEngine (not pywebview)? pywebview cannot create a transparent
window on Windows, so the previous banner fell back to Win32 layered
windows + Pillow. Qt's WebEngine CAN host a transparent page in a
frameless translucent window, so the pill's smooth rounded edges and the
animated CSS orb render straight against the desktop — live HTML/CSS,
the same model macOS uses with its WKWebView.

Wire protocol (one JSON message per line):

  → stdin   {"cmd": "MSG"|"SHOW_NEXT"|"HIDE_NEXT"|"SHOW_CHOICE"|
                    "SHOW_INPUT"|"CLEAR"|"CLOSE", ...}
  ← stdout  {"event": "READY"|"NEXT"|"CHOICE"|"SAVE"|"REVEAL"|"CLOSED", ...}

REVEAL (setup pill only) fires when the streaming reveal of a MSG has landed
and echoes that MSG's "seq"; the parent gates SHOW_NEXT / SHOW_CHOICE /
SHOW_INPUT on the REVEAL for its latest MSG (a stale seq is ignored) so a
control never pops in while the text is still typing (as on macOS/Linux).
"""
import ctypes
import datetime
import json
import logging
import os
import re
import subprocess
import sys
import threading
from queue import Queue, Empty

from AutoCua import app_origin

logger = logging.getLogger(__name__)

# True when this module is running inside the Nuitka-compiled AutoCua.exe
# (i.e. sys.executable is the exe, not a Python interpreter). In that case
# `python -m …banner` is meaningless — the binary has no -m loader — so
# StatusBanner.show() must re-exec AutoCua.exe with --banner-mode, which
# app.py's main() picks up and routes to _run_subprocess_banner() directly.
_IS_COMPILED = getattr(sys, "frozen", False) or "__compiled__" in globals()


# ── Pill geometry ─────────────────────────────────────────────────────────
# Setup pill: a fixed-WIDTH white pill (orb on the left cap + streaming wizard
# text + controls). Its HEIGHT is dynamic — the message wraps to multiple
# lines and the window is animated taller from Python (see _animate_height)
# so the pill grows smoothly DOWNWARD and the whole message stays visible.
SETUP_WIN_W = 440
SETUP_MIN_H = 56    # single-line height (orb + vertical padding)
SETUP_MAX_H = 170   # cap — an unusually long message clips rather than fill the screen
# Compact task pill: starts as a 50×50 white circle (orb only) at the top-
# right, then grows LEFT into a stadium as the agent streams text. The
# window is a fixed COMPACT_WIN_W×COMPACT_WIN_H transparent canvas anchored
# top-right; the pill itself is right-anchored inside it and the CSS width
# transition does the circle⇄stadium animation (the window never resizes —
# it's click-through, so the empty transparent area is harmless).
COMPACT_WIN_W = 580
COMPACT_WIN_H = 50
SCREEN_MARGIN = 20
# Coder terminal: while a CLI/coder sub-agent runs, the compact pill expands
# into a tall card hosting an embedded terminal panel. The window grows
# (top-right anchored, downward) to fit the panel, clamped to these bounds,
# then snaps back to COMPACT_WIN_W×COMPACT_WIN_H when the panel closes. The
# width never actually changes (panel fits in the 580 stadium width); only the
# height grows. Mirrors the macOS COMPACT_CODER_MAX_W/H clamps.
COMPACT_CODER_WIN_W = 580
COMPACT_CODER_WIN_H = 520

# Win32 extended-window-style constants for click-through (compact pill).
# WS_EX_TRANSPARENT removes the window from mouse hit-testing → clicks fall
# through to whatever app is behind the pill; the pill is a passive cue.
GWL_EXSTYLE       = -20
WS_EX_LAYERED     = 0x00080000
WS_EX_TRANSPARENT = 0x00000020
WS_EX_NOACTIVATE  = 0x08000000
WS_EX_TOOLWINDOW  = 0x00000080


# ── active-banner registry (parent-side) ──────────────────────────────────
# Every live StatusBanner (one whose subprocess is running) registers itself
# here so the app can dismiss them all the instant the user closes the main
# window — see close_all_banners(), wired to pywebview's `closing` event +
# atexit in app.py. Without this, a banner whose owning thread is a daemon
# (e.g. the Telegram setup wizard) only learns to quit via stdin-EOF, which
# fires only AFTER the parent's slow webview/Qt teardown — leaving the pill
# painted on screen for a few seconds.
_ACTIVE_BANNERS: "set[StatusBanner]" = set()
_ACTIVE_BANNERS_LOCK = threading.Lock()


def close_all_banners() -> None:
    """Close every live banner now. Safe to call from any thread and more
    than once (StatusBanner.close() is idempotent). Snapshot under the lock so
    a banner deregistering mid-iteration can't mutate the set we're walking."""
    with _ACTIVE_BANNERS_LOCK:
        banners = list(_ACTIVE_BANNERS)
    for banner in banners:
        try:
            banner.close()
        except Exception:
            pass


# ── stdio helpers (parent + subprocess) ───────────────────────────────────

def _stderr(msg: str) -> None:
    """Loud print to whichever stderr we're attached to. Used both by
    the parent (for `[banner] spawned subprocess pid=…` etc.) and by
    the subprocess (which inherits the parent's stderr so the messages
    land in the same terminal)."""
    print(f"[banner] {msg}", file=sys.stderr, flush=True)


def _emit(event: str, **kwargs) -> None:
    """Subprocess → parent: write a JSON event to stdout (one line)."""
    try:
        payload = {"event": event, **kwargs}
        sys.stdout.write(json.dumps(payload) + "\n")
        sys.stdout.flush()
    except Exception:
        pass


def _log(msg: str) -> None:
    """File-based event log for the subprocess, at
    %LOCALAPPDATA%\\AutoCua\\banner_debug.log so it survives whatever happens
    to the subprocess's stdio. Best effort: any failure to write is
    swallowed."""
    try:
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        path = os.path.join(base, "AutoCua", "banner_debug.log")
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(
                f"[{datetime.datetime.now().isoformat()}] pid={os.getpid()} {msg}\n"
            )
    except Exception:
        pass


def _js_escape(text: str) -> str:
    return (
        (text or "")
        .replace("\\", "\\\\")
        .replace("'", "\\'")
        .replace("\n", "\\n")
        .replace("\r", "")
    )


# ── pill content hygiene ───────────────────────────────────────────────────
# The compact pill streams whatever a sub-agent writes to stdout/stderr. Two
# kinds of content must never reach the user-facing pill:
#   1. Our own internal diagnostics — lines our _stderr helpers prefix with
#      "[telegram]" / "[banner]". These are for the terminal/log, not the pill.
#   2. Secrets — a Telegram bot token (digits ":" 35-char base64url) must never
#      render even partially, regardless of which surface produced the line.
# Both are belt-and-suspenders on top of the app.py guard that stops sub-agent
# children from booting a bot in the first place.

# Telegram bot token: "<bot_id>:<auth>" e.g. 7123456789:AAH…35chars… .
_TG_TOKEN_RE = re.compile(r"\b\d{6,}:[A-Za-z0-9_-]{30,}\b")
_INTERNAL_LOG_PREFIXES = ("[telegram]", "[banner]", "[tls]")


def _is_internal_log_line(text) -> bool:
    """True for our own diagnostic lines (``[telegram] …`` / ``[banner] …``)
    that should be filtered out of the user-facing pill stream."""
    return str(text or "").lstrip().startswith(_INTERNAL_LOG_PREFIXES)


def _redact_secrets(text) -> str:
    """Mask anything that looks like a Telegram bot token so no fragment can
    ever render on the pill."""
    return _TG_TOKEN_RE.sub("…<redacted>", str(text or ""))


# ── shared orb (pure CSS — the default AutoCua orb) ───────────────────────
# A soft lavender (#9292d8) disc with a bright inset white rim-glow, fixed
# pink (top-right) / blue (bottom-left) accents, and a white icon that
# cross-fades between a PC monitor (with blinking eyes) and the Telegram
# plane. This is the project's standard orb — identical to the frontend's
# .stop-agent-button and the macOS pill. Reused by both pills below.
#
# `.stop-agent-button` positioning differs per pill (flex child in setup,
# absolutely pinned to the pill's left cap in compact), so it is set in each
# document; everything below is shared and inlined verbatim into both.
_ORB_CSS = r"""
  .stop-orb { position: relative; width: 100%; height: 100%;
    display: flex; align-items: center; justify-content: center; pointer-events: none; }
  .stop-circle-1 { width: 42px; height: 42px; border-radius: 50%; position: absolute;
    background: transparent; animation: stop-pulse 4.2s ease-in-out infinite 0.3s; z-index: 1; }
  .stop-circle-1::before, .stop-circle-1::after {
    content: ""; position: absolute; border-radius: 50%; filter: blur(8px); width: 30%; height: 30%; }
  .stop-circle-1::before { background: #ff0073; top: 30%; right: 30%; }
  .stop-circle-1::after  { background: #00baff; bottom: 10%; left: 30%; }
  .stop-circle-2 { width: 32px; height: 32px; border-radius: 50%; position: absolute;
    inset: 0; margin: auto; background-color: white; z-index: 9;
    animation: stop-pulse2 4.2s ease-in-out infinite; }
  .stop-bg { position: absolute; inset: 0; border-radius: 50%;
    box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8), 0 0 2px 2px rgba(255,255,255,0.9);
    background-color: #9292d8; animation: stop-bgRotate 2.5s linear infinite; }
  .stop-bg::before { content: ""; position: absolute; inset: 0; border-radius: inherit;
    animation: stop-bgColor 4s linear infinite;
    box-shadow: inset 0 0 5px 2px rgba(255,255,255,0.8); opacity: 0.2; }
  .icon-stack { position: absolute; inset: 0; margin: auto;
    width: 32px; height: 32px; z-index: 10;
    display: flex; align-items: center; justify-content: center; }
  .icon-layer { position: absolute; inset: 0;
    display: flex; flex-direction: column;
    align-items: center; justify-content: center;
    gap: 1px; box-sizing: border-box;
    will-change: opacity; transform: translateZ(0); backface-visibility: hidden; }
  .icon-pc { animation: icon-cycle-pc 6s ease-in-out infinite; }
  .icon-tg { animation: icon-cycle-tg 6s ease-in-out infinite; color: white; }
  .stop-monitor { width: 12px; height: 10px; border: 1px solid white; box-sizing: border-box; }
  /* Even, integer layout (2px eye + 2px gap + 2px eye = 6px in the 10px screen
     → 2px clearance each side). The clearance keeps both eyes off the monitor
     border even when the expanded/streaming pill renders the orb at a slightly
     different sub-pixel position than the idle circle (otherwise the tighter
     right eye rounds into the border and looks like it's touching it). */
  .stop-screen { width: 100%; height: 100%; display: flex;
    justify-content: center; align-items: center; gap: 2px; }
  .stop-eye { width: 2px; height: 3px; border-radius: 1px; background: white;
    position: relative; top: -1px;
    animation: stop-blink 4s infinite; }
  .stop-base { width: 16px; height: 1px; background: white; border-radius: 0.5px; }
  @keyframes stop-pulse  { 0%{transform:scale(.97)} 15%{transform:scale(1)} 30%{transform:scale(.98)} 45%{transform:scale(1)} 60%{transform:scale(.97)} 85%{transform:scale(1)} 100%{transform:scale(.97)} }
  @keyframes stop-pulse2 { 0%{transform:scale(1)} 15%{transform:scale(1.03)} 30%{transform:scale(.98)} 45%{transform:scale(1.04)} 60%{transform:scale(.97)} 85%{transform:scale(1.03)} 100%{transform:scale(1)} }
  @keyframes stop-bgRotate { 0%{transform:rotate(0)} 20%{transform:rotate(90deg)} 40%{transform:rotate(180deg) scale(.95,1)} 60%,100%{transform:rotate(360deg)} }
  @keyframes stop-bgColor  { 20%{background-color:red} 40%{background-color:#5eff7e} 60%{background-color:#2cb5ff} 80%{background-color:#fc63ff} }
  @keyframes stop-blink    { 0%,85%,100%{transform:scaleY(1)} 92%{transform:scaleY(.1)} }
  @keyframes icon-cycle-pc { 0%, 40% { opacity: 1 } 50%, 90% { opacity: 0 } 100% { opacity: 1 } }
  @keyframes icon-cycle-tg { 0%, 40% { opacity: 0 } 50%, 90% { opacity: 1 } 100% { opacity: 0 } }
"""

_ORB_MARKUP = r"""
  <div class="stop-agent-button">
    <div class="stop-orb">
      <div class="stop-circle-1"></div>
      <div class="stop-circle-2"><div class="stop-bg"></div></div>
      <div class="icon-stack">
        <div class="icon-layer icon-pc">
          <div class="stop-monitor"><div class="stop-screen"><div class="stop-eye"></div><div class="stop-eye"></div></div></div>
          <div class="stop-base"></div>
        </div>
        <div class="icon-layer icon-tg">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M21.5 4.5L2.5 12l5.5 2 2 6 3-3.5 5.5 4 3-16zM10 14l8.5-7L11 14.5l-1 4.5L10 14z"/></svg>
        </div>
      </div>
    </div>
  </div>
"""

# The QWebChannel bootstrap (test.py pattern): the qwebchannel.js shim is
# served by Qt at qrc:///, so the page must be loaded with QUrl("qrc:///")
# as its base for this <script> to resolve. window.bridge gets the exposed
# Bridge QObject; every JS→Python call guards on `window.bridge` because the
# channel connects a frame or two after the inline scripts run.
_CHANNEL_BOOT = r"""
  <script src="qrc:///qtwebchannel/qwebchannel.js"></script>
"""
_CHANNEL_CONNECT = r"""
    if (window.qt && qt.webChannelTransport) {
      new QWebChannel(qt.webChannelTransport, function (channel) {
        window.bridge = channel.objects.bridge;
      });
    }
"""


# ── HTML: interactive setup pill (orb + streaming text + Next/Choice/Input) ─
BANNER_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8" />
""" + _CHANNEL_BOOT + r"""
<style>
  html, body {
    margin: 0; padding: 0; height: 100%; width: 100%;
    background: transparent; overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  /* The white pill FILLS the window (so its rounded corners always match the
     window edge); the window's height is animated from Python to fit the
     content, so the pill grows downward as the message wraps. A fixed
     border-radius keeps a grown pill a clean rounded-rectangle, not an oval. */
  .banner {
    width: 100%; height: 100%; background: #ffffff; border-radius: 28px;
    overflow: hidden; box-sizing: border-box;
  }
  /* Natural-height content row (orb + text/controls). align-items:flex-start
     TOP-anchors the content so the first line keeps its position when the
     message wraps or the Next button drops to a second line — extra lines grow
     DOWNWARD instead of re-centring the whole block upward. .body's padding-top
     drops that first line onto the orb's axis for the common single-line case. */
  .measure {
    display: flex; align-items: flex-start; gap: 9px;
    padding: 6px 18px 8px 7px; box-sizing: border-box;
  }
  /* Orb slot reserves the 42px the layout expects; the actual orb is the
     pc_button.html file embedded in an iframe (single source of truth). The
     iframe is a touch bigger and centred so the orb's glow isn't clipped, and
     click-through so it never eats the wizard's button clicks. */
  .stop-agent-button { position: relative; flex-shrink: 0;
    width: 42px; height: 42px; background: transparent; }
  .orb-frame { position: absolute; top: 50%; left: 50%;
    width: 50px; height: 50px; transform: translate(-50%, -50%);
    border: 0; background: transparent; pointer-events: none; }

  /* Text + controls flow inline and WRAP. padding-top drops the first line onto
     the orb's axis (matching the old centred single-line look); because the row
     is now top-anchored (see .measure), wrapped/extra lines grow downward and
     the first line never moves — so the prompt sits on line 1 and the choice
     buttons / token field flow onto line 2 below it. */
  .body { flex: 1; min-width: 0; line-height: 20px; padding-top: 11px; }
  .banner-text { color: #333333; font-size: 13px; font-weight: 600;
    line-height: 20px; overflow-wrap: break-word; }
  /* Continuous shimmer once a message has landed: the text becomes a mask
     over a gradient in its own grey (#333) and a lighter band sweeps left to
     right through it, forever — until the next message replaces it. Only the
     finished text carries it; during the reveal each letter fades in on its
     own (see setMsg). Same shine as the Linux pill. `clone` gives every
     wrapped line its own full gradient so the band crosses all lines together
     instead of continuing from where the previous line's fragment ended. */
  .banner-text.shimmer { color: transparent; -webkit-text-fill-color: transparent;
    background-image: linear-gradient(90deg, #333333 0%, #333333 42%, #b9bcc6 50%, #333333 58%, #333333 100%);
    background-size: 250% 100%; background-repeat: no-repeat;
    -webkit-background-clip: text; background-clip: text;
    -webkit-box-decoration-break: clone; box-decoration-break: clone;
    animation: msg-shimmer 2.4s linear infinite; }
  @keyframes msg-shimmer { from { background-position: 100% 0; } to { background-position: 0% 0; } }
  /* Purple pill button, sized to match the macOS banner (#5e6ad2, ~28px tall)
     so both surfaces look identical. Inline after the wizard line; if a long
     line leaves no trailing room it simply wraps below and the pill grows. */
  .next-btn {
    display: inline-block; vertical-align: middle; margin-left: 6px;
    background: #5e6ad2; color: #ffffff; border: none; font-family: inherit;
    font-size: 12px; font-weight: 600; padding: 4px 14px; border-radius: 999px;
    cursor: pointer; transition: background 0.15s ease;
    /* Controls only appear once the message has finished streaming (Python
       gates SHOW_* on the REVEAL event); fade them in so they land softly
       after the text instead of popping. Opacity-only, so the hit-rects JS
       reports (the window's non-click-through regions) are exact from frame
       one. A CSS animation re-runs every time display flips from none. */
    animation: ctl-in 0.22s ease-out;
  }
  @keyframes ctl-in { from { opacity: 0; } to { opacity: 1; } }
  .next-btn:hover  { background: #6e7ce3; }
  .next-btn:active { background: #4e5ac2; }
  .choice-row { display: none; }
  .choice-row .next-btn { margin-left: 0; margin-right: 6px; }
  .input-row { display: none; align-items: center; gap: 6px; }
  #token-input {
    flex: 1; height: 28px; border: 1px solid #d1d5db; border-radius: 14px;
    padding: 0 12px; font-size: 12px; font-family: inherit; color: #374151;
    background: #ffffff; outline: none;
    animation: ctl-in 0.22s ease-out;
  }
  #token-input:focus { border-color: #5e6ad2; }
</style>
</head>
<body>
  <div class="banner">
    <div class="measure">
      <div class="stop-agent-button">
        <iframe id="orbFrame" class="orb-frame" src="__APP_ORIGIN__/chat_input/pc_button.html"
                scrolling="no" frameborder="0"></iframe>
      </div>
      <div class="body">
        <span class="banner-text" id="msg">Starting…</span><button class="next-btn" id="next" style="display:none"
              onclick="if(window.bridge) bridge.next_clicked()">Next</button>
        <div class="choice-row" id="choice-row">
          <button class="next-btn" id="choice-left"
                  onclick="if(window.bridge) bridge.choice_clicked('left')">Left</button>
          <button class="next-btn" id="choice-right"
                  onclick="if(window.bridge) bridge.choice_clicked('right')">Right</button>
        </div>
        <div class="input-row" id="input-row">
          <input type="text" id="token-input" placeholder="Paste your BotFather token here" />
          <button class="next-btn" id="save-btn"
                  onclick="(function(){var v=document.getElementById('token-input').value;
                           if(window.bridge) bridge.save_clicked(v);})()">Save</button>
        </div>
      </div>
    </div>
  </div>

  <script>
""" + _CHANNEL_CONNECT + r"""
    // The orb is pc_button.html in an iframe; it starts hidden and shows when
    // told, so nudge it visible once the iframe has loaded.
    var __orb = document.getElementById('orbFrame');
    if (__orb) __orb.addEventListener('load', function () {
      try { __orb.contentWindow.postMessage('pcbtn:show', '*'); } catch (e) {}
    });
    // Multi-line streaming pill. The message reveals letter-by-letter and
    // WRAPS naturally; as it grows past a line the pill expands downward (the
    // window height is animated from Python on the height the observer below
    // reports). No paging — the whole message stays visible.
    //
    // Paced by the CLOCK, not by a per-letter timer (same as the Linux
    // pill): a setTimeout chain is clamped to ~4 ms and jitters, and at
    // 8 ms/letter a 54-char line took ~0.5 s — long enough that the Next
    // button, which now waits for the text to land, felt late. Every
    // animation frame appends whatever letters are due by elapsed time, so
    // the pace is the same on every machine and a line lands in well under
    // a quarter second. The fade is longer than the cadence, so a handful of
    // letters are always mid-fade at the leading edge — the stream reads as
    // a shimmer rather than as discrete pops.
    const _CHARS_PER_SEC = 320;   // 54 chars in ~170 ms
    const _FADE_MS = 90;          // per-letter fade-in; ~29 letters mid-fade at once
    let _revealRaf = null;
    let _landTimer = null;
    // Reveal sequence number. Python stamps each MSG with its own counter and
    // setMsg adopts it (a call without one just bumps the last value); the
    // page echoes it in reveal_done(seq) so the parent can drop a REVEAL that
    // belongs to an OLDER message — e.g. one whose REVEAL was still in the
    // stdout pipe when the next update() cleared the gate.
    let _revealSeq = 0;

    // Tell Python the streaming reveal has finished so it can now show
    // whichever control set (Next / choice / input) belongs to this step —
    // without this the button would pop in while the text is still typing.
    // window.bridge connects a frame or two after the inline scripts run, so
    // if it isn't there yet keep trying briefly rather than dropping the
    // event (Python falls back to a 3 s cap, but that would read as a slow
    // pill on the very first message). `seq` pins the retry chain to the
    // message that landed: if a newer setMsg has started since, this REVEAL
    // must NOT fire — it would release the parent's gate for the new text
    // while it is still typing.
    function notifyRevealDone(seq, attempt) {
      attempt = attempt || 0;
      if (seq !== _revealSeq) return;
      if (window.bridge) { try { bridge.reveal_done(seq); } catch (e) {} return; }
      if (attempt < 40) setTimeout(function () { notifyRevealDone(seq, attempt + 1); }, 50);
    }

    function setMsg(fullText, msgSeq) {
      if (_revealRaf) { cancelAnimationFrame(_revealRaf); _revealRaf = null; }
      if (_landTimer) { clearTimeout(_landTimer); _landTimer = null; }
      const seq = (msgSeq > 0) ? (msgSeq | 0) : (_revealSeq + 1);
      _revealSeq = seq;
      const el = document.getElementById('msg');
      if (!el) return;
      const chars = Array.from((fullText || '').toString());   // by code point so emoji stay intact
      // New message: drop the finished-text shine so the letters stream in
      // dark, then shimmer again once this one lands.
      el.classList.remove('shimmer');
      el.textContent = '';
      if (!chars.length) { notifyRevealDone(seq); return; }
      let shown = 0;
      const t0 = performance.now();

      const land = () => {
        _landTimer = null;
        // Landed: collapse the per-letter spans into plain text so the
        // shimmer gradient can mask the whole line as one (a span mid-fade
        // paints in its own layer and would fall out of the text mask), then
        // keep it shimmering until the next message. Only now does Python
        // get to show the step's control, so it lands after the text.
        el.textContent = chars.join('');
        el.classList.add('shimmer');
        notifyRevealDone(seq);
      };
      const frame = (now) => {
        const due = Math.min(chars.length,
                             Math.floor((now - t0) / 1000 * _CHARS_PER_SEC) + 1);
        while (shown < due) {
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
          // Let the trailing letters finish their fade before the flatten
          // snaps them to full opacity — otherwise the tail visibly pops.
          // Each span's opacity only flips to 1 in a nested rAF (next frame),
          // so its fade ends ~1 frame after _FADE_MS from here; allow two.
          _landTimer = setTimeout(land, _FADE_MS + 34);
          return;
        }
        _revealRaf = requestAnimationFrame(frame);
      };
      _revealRaf = requestAnimationFrame(frame);
    }
    function showNext()  {
      clearAll();
      document.getElementById('next').style.display = 'inline-block';
      document.getElementById('msg').style.display = 'inline';
      reportHitRects();
    }
    function hideNext() {
      document.getElementById('next').style.display = 'none';
      reportHitRects();
    }
    function setChoice(leftLabel, rightLabel) {
      // Keep the prompt (e.g. "How do you want to set up the bot?") visible
      // ABOVE the buttons, like the macOS banner — clearAll already shows msg.
      clearAll();
      document.getElementById('choice-left').textContent = leftLabel;
      document.getElementById('choice-right').textContent = rightLabel;
      document.getElementById('choice-row').style.display = 'block';
      reportHitRects();
    }
    function setInput(saveLabel) {
      // Keep the prompt (e.g. "Paste your BotFather token…") visible above the
      // field, like the macOS banner — clearAll already shows msg.
      clearAll();
      document.getElementById('save-btn').textContent = saveLabel || 'Save';
      document.getElementById('input-row').style.display = 'flex';
      var inp = document.getElementById('token-input');
      inp.value = '';
      setTimeout(function(){ inp.focus(); }, 30);
      reportHitRects();
    }
    function clearAll() {
      document.getElementById('next').style.display = 'none';
      document.getElementById('choice-row').style.display = 'none';
      document.getElementById('input-row').style.display = 'none';
      document.getElementById('msg').style.display = 'inline';
      reportHitRects();
    }
    // Report the window-local rects of the currently-VISIBLE interactive controls
    // so Python keeps the pill click-through everywhere except over them. CSS px
    // == window-local px (the view fills the frameless window with 0 margins).
    function reportHitRects() {
      if (!window.bridge) return;
      var rects = [];
      var inputVisible = false;
      function add(el) {
        if (!el) return;
        var r = el.getBoundingClientRect();
        if (r.width > 0 && r.height > 0) {
          rects.push({ x: r.left, y: r.top, w: r.width, h: r.height });
        }
      }
      var next = document.getElementById('next');
      if (next && next.style.display !== 'none') add(next);
      var choiceRow = document.getElementById('choice-row');
      if (choiceRow && choiceRow.style.display !== 'none') {
        add(document.getElementById('choice-left'));
        add(document.getElementById('choice-right'));
      }
      var inputRow = document.getElementById('input-row');
      if (inputRow && inputRow.style.display !== 'none') {
        inputVisible = true;
        add(document.getElementById('token-input'));
        add(document.getElementById('save-btn'));
      }
      try { bridge.set_hit_rects(JSON.stringify({ rects: rects, input: inputVisible })); } catch (e) {}
    }
    window.setMsg = setMsg;
    window.showNext = showNext;
    window.hideNext = hideNext;
    window.setChoice = setChoice;
    window.setInput = setInput;
    window.clearAll = clearAll;

    document.getElementById('token-input').addEventListener('keydown', function(e) {
      if (e.key === 'Enter' && window.bridge) {
        bridge.save_clicked(this.value);
      }
    });

    // Report the content's natural height to Python whenever it changes (text
    // wraps as it streams, or controls toggle); Python animates the window to fit
    // so the pill grows/shrinks smoothly from the bottom. Also re-report the
    // control hit-rects each fire (they shift when the pill height changes).
    // Height is last-value debounced so identical reports don't churn.
    (function () {
      var last = -1;
      var m = document.querySelector('.measure');
      function report() {
        if (!window.bridge || !m) return;
        var h = Math.ceil(m.getBoundingClientRect().height);
        if (h !== last) {
          last = h;
          try { bridge.height_changed(h); } catch (e) {}
        }
        reportHitRects();
      }
      window.addEventListener('load', function () { setTimeout(report, 30); });
      try { var ro = new ResizeObserver(report); ro.observe(m); } catch (e) {}
    })();
  </script>
</body>
</html>
"""


# ── HTML: compact task pill (orb circle that expands into a streaming pill) ─
COMPACT_HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8" />
""" + _CHANNEL_BOOT + r"""
<style>
  html, body {
    margin: 0; padding: 0; height: 100%; width: 100%;
    background: transparent; overflow: hidden;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    -webkit-user-select: none; user-select: none;
  }
  /* PILL: right edge fixed to the window's right (top-right of screen);
     width animates 50px → 580px so it grows LEFT. Empty state = a 50×50
     circle (orb only); has-text = a stadium with streaming text. The pill is
     a flex COLUMN so the coder terminal panel can stack BELOW the orb+text
     top row when a CLI/coder sub-agent runs (body.coder). */
  .pill {
    position: absolute; right: 0; top: 0;
    width: 50px; border-radius: 25px;
    background: #ffffff; overflow: hidden;
    transition: width 0.42s cubic-bezier(.22,1,.36,1);
    display: flex; flex-direction: column;
  }
  body.has-text .pill { width: 580px; }
  /* Coder terminal open: the pill becomes a wide, tall card. Declared AFTER
     .has-text so its width/height win regardless of the streaming has-text
     toggle. Height is content-driven; Python grows the window to fit (the
     size_changed bridge reports the pill's natural height). */
  body.coder .pill { width: 580px; height: auto; border-radius: 16px; padding-bottom: 12px; }

  /* Top row = orb + the single-line streaming ticker (the original pill). */
  .toprow { position: relative; height: 50px; width: 100%; flex-shrink: 0; }
  /* Orb pinned to the pill's left cap (centre = cap centre); it rides left
     as the pill grows. */
  .stop-agent-button { position: absolute; left: 4px; top: 4px;
    width: 42px; height: 42px; background: transparent; }
  .orb-frame { position: absolute; top: 50%; left: 50%;
    width: 50px; height: 50px; transform: translate(-50%, -50%);
    border: 0; background: transparent; pointer-events: none; }
  /* Vertically centred via top/translateY — NOT display:flex. With flex, each
     streamed per-character <span> becomes a flex item and a space-only item
     collapses to zero width, eating the spaces between words. Plain inline
     flow preserves spaces. top:50% resolves against the 50px .toprow. */
  .msg {
    position: absolute; left: 56px; right: 16px; top: 50%;
    transform: translateY(-50%);
    font-size: 13px; color: #374151; line-height: 1.4;
    white-space: nowrap; overflow: hidden;
  }

  /* ── embedded coder terminal panel (shown only while a CLI/coder runs) ── */
  /* #coderPanel is just the STACK container; each coder (CLI agent) is its own
     bordered, glowing .cp-term box, one-below-the-other — so 2 concurrent coders
     show as 2 stacked terminals, 3 as 3, etc. */
  #coderPanel { display: none; margin: 8px 8px 0; box-sizing: border-box; }
  body.coder #coderPanel { display: flex; flex-direction: column; gap: 8px; }
  .cp-term { position: relative; width: 100%; box-sizing: border-box;
    border-radius: 12px; overflow: hidden; background: #f7f7f9;
    box-shadow: inset 0 0 0 1px rgba(139,92,246,0.55), inset 0 0 8px rgba(139,92,246,0.45);
    animation: cp-edgeGlow 3s ease-in-out infinite; }
  .cp-body { position: relative; z-index: 1; padding: 14px 14px 18px; color: rgba(0,0,0,0.82);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Courier New", monospace;
    font-size: 12px; line-height: 1.5; }
  /* this coder's own streaming output line (paginated; '>' prompt re-added per
     page). Shows real output, or filler while it waits on its minions. */
  .cp-out { min-height: 22px; overflow: hidden; }
  .cp-line { white-space: nowrap; overflow: hidden; margin: 2px 0; }
  .cp-mrow .cp-line { margin: 0; }
  .cp-p { color: rgba(0,0,0,0.4); }
  /* todos are indented to sit in the SAME column as the minion loaders (both
     at the cp-body content edge + 22px), matching coder_animation.html. */
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
     animates (stretch/shrink) as minions are added/removed. */
  /* per-row margin (not container gap) so a removed row can animate its spacing
     to 0 and let the rows below slide up smoothly. */
  .cp-minions { position: relative; display: flex; flex-direction: column;
    margin: 6px 0 2px 0; padding-left: 22px; }
  .cp-mrow { margin-bottom: 6px; }
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
    min-height: 20px; color: rgba(0,0,0,0.7); }
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
  /* orbiting 5-dot loader (port of the coder_animation.html .loader/.orbe);
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
</style>
</head>
<body>
  <div class="pill">
    <div class="toprow">
      <div class="stop-agent-button">
        <iframe class="orb-frame" src="__APP_ORIGIN__/telegram/telergam_animation.html"
                scrolling="no" frameborder="0"></iframe>
      </div>
      <span class="msg" id="msg"></span>
    </div>
    <div id="coderPanel"></div>
  </div>

  <script>
""" + _CHANNEL_CONNECT + r"""
    // Stream `text` letter-by-letter into .msg, paging long lines. On the
    // first MSG of a task, body gets .has-text (the pill expands leftward via
    // CSS) and streaming waits out the expansion before any letter appears;
    // subsequent MSGs start immediately. When the final message finishes and
    // nothing new arrives, the pill collapses back to the circle.
    const _CHAR_DELAY_MS = 8;      // per-letter cadence
    const _FADE_MS = 60;           // per-letter fade-in duration
    const _PAGE_HOLD_MS = 400;     // full line lingers before paging
    const _DONE_HOLD_MS = 1400;    // idle hold before collapsing to the circle
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

      const chars = Array.from(text);
      let i = 0;

      const streamChar = () => {
        if (i >= chars.length) {
          // Whole message shown → hold, then collapse back to the circle.
          _revealTimer = setTimeout(() => {
            el.textContent = '';
            document.body.classList.remove('has-text');
            _revealTimer = null;
          }, _DONE_HOLD_MS);
          return;
        }
        const span = document.createElement('span');
        span.textContent = chars[i];
        span.style.opacity = '0';
        span.style.transition = 'opacity ' + _FADE_MS + 'ms ease-out';
        el.appendChild(span);

        if (el.scrollWidth > el.clientWidth + 0.5) {
          if (el.children.length === 1) {
            requestAnimationFrame(() => { span.style.opacity = '1'; });
            i++;
            _revealTimer = setTimeout(streamChar, _CHAR_DELAY_MS);
            return;
          }
          el.removeChild(span);
          _revealTimer = setTimeout(() => {
            el.textContent = '';
            while (i < chars.length && /\s/.test(chars[i])) i++;
            streamChar();
          }, _PAGE_HOLD_MS);
          return;
        }
        requestAnimationFrame(() => { span.style.opacity = '1'; });
        i++;
        _revealTimer = setTimeout(streamChar, _CHAR_DELAY_MS);
      };

      const startDelay = wasEmpty ? 480 : 0;   // wait out the CSS expansion
      _revealTimer = setTimeout(streamChar, startDelay);
    }
    window.setMsg = setMsg;

    // ── embedded coder terminal ──────────────────────────────────────────────
    // Faithful port of the macOS pill's coder panel. Every incoming line is
    // QUEUED and streamed letter-by-letter with pagination (overflow -> hold ->
    // clear -> continue), so the full real content flows by rather than just the
    // latest fragment. The top line shows the coder's REAL output; only while a
    // minion runs AND the coder is idle does it stream playful filler phrases
    // (CP_MSGS). Each minion row shows that minion's REAL per-iteration output.
    // Python toggles the panel via coderShow()/coderHide().
    (function () {
      const CHAR_STAGGER    = 8;     // ms between letters (fallback)
      const REAL_STAGGER    = 4;     // the agent's real output streams FAST
      const REAL_STEP       = 5;     // …and emits this many chars per tick (timers
                                     // clamp to ~4ms, so batching is what makes
                                     // real output stream VERY fast). Filler = 1.
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
      `).split('\n').map((s) => s.trim()).filter(Boolean);

      const $ = (id) => document.getElementById(id);
      const escHtml = (s) => (s == null ? '' : String(s))
        .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
      const mid = (id) => 'cpm_' + String(id).replace(/[^a-zA-Z0-9_]/g, '_');

      // ── line runner: queue + paginated letter-by-letter streaming ──
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
                  const cb = runner.onIdle; runner.onIdle = null; cb();
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
                while (i < chars.length && /\s/.test(chars[i])) i++;
                tick();
              }, PAGE_HOLD);
              return;
            }
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
      const cid = (id) => 'cpc_' + String(id).replace(/[^a-zA-Z0-9_]/g, '_');
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
          // minions (it has nothing real to print meanwhile). Boxes busy streaming
          // real output — or with no running minions — are skipped, so filler never
          // stomps live output.
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
        if (window.bridge) { try { bridge.coder_active(true); } catch (e) {} }
        document.body.classList.add('coder');
        if (!wasHidden) return;   // already visible — nothing was deferred
        // Boxes (and their minions/trunks) may have been created while the panel
        // was display:none, when all geometry measured as 0. Now that it's visible,
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
        if (window.bridge) { try { bridge.coder_active(false); } catch (e) {} }
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
        const raw = (todoText == null ? '' : String(todoText)).split('\n');
        const items = [];
        for (let i = 0; i < raw.length; i++) {
          const ln = raw[i].trim();
          if (!ln) continue;
          if (/^objective\s*:/i.test(ln)) continue;
          let m = ln.match(/^#\d+\.\s*-\s*\[([ xX])\]\s*(.*)$/);
          if (!m) m = ln.match(/^-\s*\[([ xX])\]\s*(.*)$/);
          if (!m) continue;
          items.push({ done: m[1].toLowerCase() === 'x', text: m[2] });
        }
        if (!items.length) {
          el.innerHTML = ''; el.classList.add('hidden');
          if (boxTrunk(box)) requestAnimationFrame(() => layoutTrunk(box));
          return;
        }
        // The current task is the first not-yet-done one: it gets the spinning
        // loading circle. Done tasks show the breathing gradient box.
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
        // todo rows just shifted the minions down → re-anchor this box's trunk.
        if (boxTrunk(box)) requestAnimationFrame(() => layoutTrunk(box));
      };

      // ── per-coder minion tree (one trunk + per-row elbow, scoped to each box) ──
      // Each coder box owns its OWN trunk line. Its TOP anchor (just below that
      // box's `>` output line, with a small gap) is computed ONCE per box, ROUNDED,
      // and cached on the element (box._trunkTop) so it never jitters; only the
      // HEIGHT changes, tracked to that box's LAST non-removing row's centre. The
      // fixed CSS `left:16px` lines the 1px line up exactly with the elbow.
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

      // Drive a box's trunk height per-frame for `durationMs` (used during a row
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

        // (1) retract the elbow (origin left → shrinks right-to-left) and fade.
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
              applyTrunkHeight(box, trunk, 0);     // only row → shrink to nothing
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

    // Report the pill's natural size to Python so the window can grow to host
    // the coder panel (and shrink back). The window only acts on these while
    // the coder panel is active (Python gates on _coder_active); during normal
    // text streaming the fixed 580×50 canvas is unchanged. Last-value debounce
    // (the pill height is content-driven, independent of window size, so this
    // can't feedback-loop with the window resize).
    (function () {
      var lastW = -1, lastH = -1;
      var pill = document.querySelector('.pill');
      function report() {
        if (!window.bridge || !pill) return;
        var r = pill.getBoundingClientRect();
        var w = Math.ceil(r.width);
        var h = Math.ceil(r.height);
        // Ignore ±1px height jitter so a sub-pixel content fluctuation can't make
        // the window re-animate up and down (shaky bottom edge). Real content
        // changes (a minion row / todo line ≈ 24px) clear the threshold.
        if (w === lastW && Math.abs(h - lastH) < 2) return;
        lastW = w; lastH = h;
        try { bridge.size_changed(w, h); } catch (e) {}
      }
      window.addEventListener('load', function () { setTimeout(report, 30); });
      try { var ro = new ResizeObserver(report); ro.observe(pill); } catch (e) {}
    })();
  </script>
</body>
</html>
"""


# ── stdin reader thread (subprocess-side only) ───────────────────────────

def _stdin_reader(emit_js, on_close, on_focus=None) -> None:
    """Loop reading JSON commands from stdin and dispatching them.

    Runs on its own thread so it never blocks the Qt GUI loop. Each command
    is turned into a JS string and handed to `emit_js(code)` — a thread-safe
    Signal.emit that marshals the actual runJavaScript onto the Qt GUI
    thread (QtWebEngine JS calls MUST run there). CLOSE / stdin-EOF call
    `on_close()` (also marshaled) so the window tears down on the GUI thread.
    `on_focus()` (setup pill only) is fired on SHOW_INPUT so the token field
    grabs OS focus and typing/paste land without the user clicking first.
    """
    _log("stdin_reader: thread started")
    try:
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except Exception:
                _log(f"stdin_reader: skip unparseable line {line!r}")
                continue
            cmd = msg.get("cmd")
            _log(f"stdin_reader: cmd={cmd!r}")
            try:
                if cmd == "MSG":
                    esc = _js_escape(msg.get("text", ""))
                    # The parent's per-message sequence number; the page echoes
                    # it back in REVEAL so a stale reveal can be told apart.
                    try:
                        seq = int(msg.get("seq", 0) or 0)
                    except (TypeError, ValueError):
                        seq = 0
                    # Primary path: setMsg streams the text and fires
                    # reveal_done when it lands. Fallback if the page-side
                    # script isn't there: set the text directly and post
                    # reveal_done ourselves so the parent's wait_for_* doesn't
                    # sit on its reveal cap.
                    emit_js(
                        f"if(window.setMsg){{ setMsg('{esc}', {seq}); }}"
                        f" else {{"
                        f"   var m = document.getElementById('msg');"
                        f"   if (m) m.textContent = '{esc}';"
                        f"   if (window.bridge) {{ try {{ bridge.reveal_done({seq}); }} catch (e) {{}} }}"
                        f" }}"
                    )
                elif cmd == "SHOW_NEXT":
                    emit_js("if(window.showNext) showNext();")
                elif cmd == "HIDE_NEXT":
                    emit_js("if(window.hideNext) hideNext();")
                elif cmd == "SHOW_CHOICE":
                    left = _js_escape(msg.get("left", ""))
                    right = _js_escape(msg.get("right", ""))
                    emit_js(f"if(window.setChoice) setChoice('{left}', '{right}');")
                elif cmd == "SHOW_INPUT":
                    label = _js_escape(msg.get("label", "Save"))
                    emit_js(f"if(window.setInput) setInput('{label}');")
                    if on_focus is not None:
                        on_focus()
                elif cmd == "CLEAR":
                    emit_js("if(window.clearAll) clearAll();")
                # ── embedded coder terminal (compact pill only) ──────────────
                elif cmd == "CODER_START":
                    emit_js("if(window.coderShow) coderShow();")
                elif cmd == "CODER_STOP":
                    emit_js("if(window.coderHide) coderHide();")
                elif cmd == "ADD_CODER":
                    coder = _js_escape(msg.get("id", ""))
                    emit_js(f"if(window.addCoder) addCoder('{coder}');")
                elif cmd == "REMOVE_CODER":
                    coder = _js_escape(msg.get("id", ""))
                    emit_js(f"if(window.removeCoder) removeCoder('{coder}');")
                elif cmd == "PUSH_CLI_LINE":
                    coder = _js_escape(msg.get("id", ""))
                    esc = _js_escape(msg.get("text", ""))
                    emit_js(f"if(window.setCoderLine) setCoderLine('{coder}', '{esc}');")
                elif cmd == "SET_TODO":
                    # _js_escape preserves newlines as \\n, which setTodo's
                    # .split('\\n') needs — no separate multiline escape required.
                    coder = _js_escape(msg.get("id", ""))
                    esc = _js_escape(msg.get("text", ""))
                    emit_js(f"if(window.setTodo) setTodo('{coder}', '{esc}');")
                elif cmd == "ADD_MINION":
                    pid = _js_escape(msg.get("parent_id", ""))
                    mid = _js_escape(msg.get("id", ""))
                    emit_js(f"if(window.addMinion) addMinion('{pid}', '{mid}');")
                elif cmd == "SET_MINION_LINE":
                    mid = _js_escape(msg.get("id", ""))
                    line_text = _js_escape(msg.get("line", ""))
                    emit_js(f"if(window.setMinionLine) setMinionLine('{mid}', '{line_text}');")
                elif cmd == "REMOVE_MINION":
                    mid = _js_escape(msg.get("id", ""))
                    emit_js(f"if(window.removeMinion) removeMinion('{mid}');")
                elif cmd == "CLOSE":
                    _log("stdin_reader: CLOSE received, closing window")
                    on_close()
                    return
            except Exception:
                import traceback
                _log(
                    f"stdin_reader: cmd={cmd!r} dispatch raised:\n"
                    + traceback.format_exc()
                )
    except Exception:
        import traceback
        _log("stdin_reader: outer loop raised:\n" + traceback.format_exc())
    _log("stdin_reader: thread exiting (stdin EOF or pipe break)")
    # EOF / pipe break without a CLOSE means the parent process is gone (e.g.
    # the frontend UI was closed). Close the window so this subprocess exits
    # with it instead of lingering as an orphan tied to nothing.
    try:
        on_close()
    except Exception:
        pass


# ── subprocess entry point (PySide6 QWebEngineView pill) ──────────────────

def _run_subprocess_banner() -> None:
    """Subprocess body. Imports PySide6 lazily so the parent (which only uses
    StatusBanner) doesn't pay Qt's startup cost when it imports this module.

    Builds ONE QApplication and a single transparent frameless pill window
    hosting the setup or compact HTML, then parks on app.exec(). Reads JSON
    commands from stdin (via _stdin_reader) and emits JSON events on stdout.
    """
    _log(f"subprocess start (sys.executable={sys.executable!r})")
    try:
        from PySide6.QtCore import (
            Qt, QUrl, QObject, Slot, Signal, QVariantAnimation, QEasingCurve,
        )
        from PySide6.QtGui import QColor
        from PySide6.QtWidgets import QApplication, QWidget, QVBoxLayout
        from PySide6.QtWebEngineWidgets import QWebEngineView
        from PySide6.QtWebEngineCore import QWebEngineSettings
        from PySide6.QtWebChannel import QWebChannel
    except Exception:
        import traceback
        _log("banner: PySide6 import failed:\n" + traceback.format_exc())
        raise

    compact = "--compact" in sys.argv[1:]

    class Bridge(QObject):
        """Exposed to JS as window.bridge. Each slot maps a control event to a
        stdout event the parent's StatusBanner already consumes. QWebChannel
        delivers these on the GUI thread (the Bridge's thread), so height_changed
        can resize the window directly."""

        def __init__(self, win):
            super().__init__()
            self._win = win

        @Slot()
        def next_clicked(self):
            _emit("NEXT")

        @Slot(str)
        def choice_clicked(self, value=""):
            _emit("CHOICE", value=value or "left")

        @Slot(str)
        def save_clicked(self, value=""):
            _emit("SAVE", value=(value or "").strip())

        @Slot(int)
        @Slot()
        def reveal_done(self, seq=0):
            # Setup pill only — the streaming reveal of a MSG has landed; `seq`
            # is that MSG's sequence number echoed back. The parent gates
            # SHOW_NEXT / SHOW_CHOICE / SHOW_INPUT on the REVEAL for its latest
            # MSG so a control never pops in mid-sentence.
            try:
                seq = int(seq or 0)
            except (TypeError, ValueError):
                seq = 0
            _emit("REVEAL", seq=seq)

        @Slot(int)
        def height_changed(self, h=0):
            # Setup pill only — grow/shrink the window to fit the content.
            try:
                self._win._animate_height(h)
            except Exception:
                pass

        @Slot(str)
        def set_hit_rects(self, payload=""):
            # Setup pill only — JS reports the window-local rects of the visible
            # interactive controls so the pill can be click-through everywhere
            # except over them. Runs on the GUI thread (QWebChannel slot).
            try:
                self._win._on_hit_rects(payload)
            except Exception:
                pass

        @Slot(int, int)
        def size_changed(self, w=0, h=0):
            # Compact coder pill only — JS reports the pill's natural size so the
            # window can grow to host the embedded coder terminal panel (and
            # shrink back). Runs on the GUI thread (QWebChannel slot), so it can
            # resize the window directly — like height_changed above.
            try:
                self._win._on_coder_size(w, h)
            except Exception:
                pass

        @Slot(bool)
        def coder_active(self, on=False):
            # Compact coder pill only — coderShow()/coderHide() flip this so the
            # window-grow path (size_changed) knows whether the panel is open.
            # GUI-thread write paired with the GUI-thread size_changed read, so
            # the flag and the resize never race.
            try:
                self._win._set_coder_active(on)
            except Exception:
                pass

    class _PillWindow(QWidget):
        """Transparent frameless top-right pill window hosting the HTML.

        run_js / do_close are cross-thread channels: the stdin reader runs on
        a worker thread but QtWebEngine JS and window teardown must happen on
        the GUI thread, so the reader emits these Signals (auto-queued onto
        the GUI event loop)."""

        run_js = Signal(str)
        do_close = Signal()
        do_focus = Signal()

        def __init__(self, compact: bool):
            super().__init__()
            self._compact = compact
            self._ready = False
            self._target_h = COMPACT_WIN_H if compact else SETUP_MIN_H
            self._anim = None
            # True while the embedded coder terminal panel is open (compact pill
            # only). Gates _on_coder_size so the window only grows/shrinks while
            # the panel is showing; normal text streaming keeps the fixed canvas.
            self._coder_active = False
            # Coder-panel size animation (compact pill only). _coder_target_* is
            # the size we're easing toward, so repeated identical size reports
            # don't restart the animation. Separate handle from _anim (the setup
            # pill's height animator) so the two can never stomp each other.
            self._size_anim = None
            self._coder_target_w = COMPACT_WIN_W
            self._coder_target_h = COMPACT_WIN_H
            # Setup-pill click-through state. The setup pill is click-through by
            # default and becomes interactive only while a wizard control is
            # visible (see _apply_click_through / _on_hit_rects).
            self._exstyle_state = None  # cached (transparent, noactivate) → skip redundant syscalls

            flags = (Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint | Qt.Tool)
            if compact:
                # Passive indicator: never clickable / focusable; clicks pass
                # straight through to whatever app is behind the pill.
                flags |= Qt.WindowTransparentForInput | Qt.WindowDoesNotAcceptFocus
            self.setWindowFlags(flags)
            self.setAttribute(Qt.WA_TranslucentBackground, True)
            # Neither pill activates on show. The setup pill manages focus via the
            # Win32 WS_EX_NOACTIVATE ex-style (cleared only for the token-input
            # step), not a static Qt flag, so it never steals focus from Edge.
            self.setAttribute(Qt.WA_ShowWithoutActivating, True)
            if compact:
                self.setAttribute(Qt.WA_TransparentForMouseEvents, True)

            if compact:
                self.setFixedSize(COMPACT_WIN_W, COMPACT_WIN_H)
            else:
                # Width is fixed; height is dynamic (animated downward as the
                # message wraps — see _animate_height).
                self.setFixedWidth(SETUP_WIN_W)
                self.setFixedHeight(SETUP_MIN_H)
            self._move_to_top_right()

            layout = QVBoxLayout(self)
            layout.setContentsMargins(0, 0, 0, 0)

            self.view = QWebEngineView(self)
            self.view.setAttribute(Qt.WA_TranslucentBackground, True)
            if compact:
                self.view.setAttribute(Qt.WA_TransparentForMouseEvents, True)
            self.view.setStyleSheet("background: transparent;")
            self.view.page().setBackgroundColor(QColor(0, 0, 0, 0))  # transparent page
            self.view.setContextMenuPolicy(Qt.NoContextMenu)
            # The pill page loads as local (qrc:///) content but embeds the orb
            # from the app's own server (pc_button.html / telergam_animation.html),
            # so let local content load that remote URL.
            _wa = (QWebEngineSettings.WebAttribute
                   if hasattr(QWebEngineSettings, "WebAttribute") else QWebEngineSettings)
            self.view.settings().setAttribute(_wa.LocalContentCanAccessRemoteUrls, True)
            layout.addWidget(self.view)

            self.channel = QWebChannel()
            self.bridge = Bridge(self)
            self.channel.registerObject("bridge", self.bridge)
            self.view.page().setWebChannel(self.channel)

            # cross-thread marshaling (worker thread → GUI thread)
            self.run_js.connect(self._on_run_js)
            self.do_close.connect(self._on_close)
            self.do_focus.connect(self._on_focus)
            # READY (and the stdin reader) wait for the page to finish loading
            # so window.setMsg / the bridge exist before the first command.
            self.view.loadFinished.connect(self._on_load_finished)

            html = COMPACT_HTML if compact else BANNER_HTML
            # qrc:/// base URL so the qwebchannel.js <script> resolves.
            self.view.setHtml(html.replace("__APP_ORIGIN__", app_origin()), QUrl("qrc:///"))

        def _move_to_top_right(self):
            screen = QApplication.primaryScreen().availableGeometry()
            x = screen.right() - self.width() + 1 - SCREEN_MARGIN
            y = screen.top() + SCREEN_MARGIN
            self.move(x, y)

        def _animate_height(self, content_h):
            """Smoothly grow/shrink the setup pill to fit `content_h` px of
            content. Width and top-left stay fixed, so the pill expands
            downward. Runs on the GUI thread (called from the bridge)."""
            if self._compact:
                return
            target = max(SETUP_MIN_H, min(SETUP_MAX_H, int(content_h)))
            start = self.height()
            if target == self._target_h and target == start:
                return
            self._target_h = target
            if target == start:
                return
            if self._anim is not None:
                try:
                    self._anim.stop()
                except Exception:
                    pass
            anim = QVariantAnimation(self)
            anim.setDuration(180)
            anim.setStartValue(start)
            anim.setEndValue(target)
            anim.setEasingCurve(QEasingCurve.OutCubic)
            anim.valueChanged.connect(lambda v: self.setFixedHeight(int(v)))
            self._anim = anim
            anim.start()

        def _set_coder_active(self, on):
            """Flip the coder-panel-open flag (GUI thread, from bridge.coder_active).
            Turning OFF smoothly collapses the window back to the orb pill —
            _on_coder_size early-returns once inactive, so it can't fight the
            collapse with a stale grow report."""
            if not self._compact:
                return
            self._coder_active = bool(on)
            if not self._coder_active:
                self._animate_compact_size(COMPACT_WIN_W, COMPACT_WIN_H)

        def _on_coder_size(self, w, h):
            """Grow/shrink the compact window to fit the coder panel's natural
            size (GUI thread, from bridge.size_changed). No-op unless the panel
            is open. Top-right anchored — height grows downward; width is fixed at
            580 so only the height clamp matters. Mirrors macOS _on_size_changed."""
            if not self._compact or not self._coder_active:
                return
            try:
                new_w = max(COMPACT_WIN_W, min(int(w), COMPACT_CODER_WIN_W))
                new_h = max(COMPACT_WIN_H, min(int(h), COMPACT_CODER_WIN_H))
                self._animate_compact_size(new_w, new_h)
            except Exception:
                pass

        def _animate_compact_size(self, target_w, target_h):
            """Smoothly ease the compact window to (target_w, target_h), top-right
            anchored so the pill grows/shrinks DOWNWARD (the coder panel slides in
            from below instead of snapping into place). Width is effectively
            constant at 580, so only the height animates. Repeated reports for the
            same target are ignored so streaming content doesn't restart the
            animation needlessly."""
            if (target_w, target_h) == (self._coder_target_w, self._coder_target_h):
                return
            self._coder_target_w = target_w
            self._coder_target_h = target_h
            start_h = self.height()
            # Width never animates (constant 580) — apply any change immediately.
            if self.width() != target_w:
                try:
                    self.setFixedSize(target_w, start_h)
                    self._move_to_top_right()
                except Exception:
                    pass
            if target_h == start_h:
                return
            if self._size_anim is not None:
                try:
                    self._size_anim.stop()
                except Exception:
                    pass
            anim = QVariantAnimation(self)
            anim.setDuration(280)
            anim.setStartValue(start_h)
            anim.setEndValue(target_h)
            anim.setEasingCurve(QEasingCurve.OutCubic)

            def _apply(v):
                try:
                    self.setFixedSize(target_w, int(v))
                    # Re-pin top-right each frame: x from the (constant) width so
                    # the right edge stays put, y constant so the top edge is
                    # fixed and the growth reads as downward.
                    self._move_to_top_right()
                except Exception:
                    pass

            anim.valueChanged.connect(_apply)
            self._size_anim = anim
            anim.start()

        def showEvent(self, event):
            super().showEvent(event)
            # Re-pin (DPI / geometry may resolve only after show).
            self._move_to_top_right()
            if self._compact:
                self._make_click_through()
            else:
                # Setup pill: start click-through so it never blocks the agent's
                # clicks/keystrokes. It flips to interactive only while a wizard
                # control (Next / choice / token field) is visible — see
                # _on_hit_rects. No control is ever shown while the agent
                # automates Edge, so automation always passes through.
                self._apply_click_through(True)

        def _make_click_through(self):
            # Compact pill: permanently click-through + non-activating.
            self._apply_click_through(True)

        def _apply_click_through(self, transparent):
            """Set/clear WS_EX_TRANSPARENT (mouse pass-through) on the top-level
            HWND. WS_EX_LAYERED|TOOLWINDOW are always kept. WS_EX_NOACTIVATE is
            kept only WHILE click-through (text-only / agent automation) so we
            never steal focus then; when a control is visible the window may
            activate so clicks fully register and the token field can take
            keyboard focus. No-op when the ex-style state is unchanged."""
            if sys.platform != "win32":
                return
            noactivate = bool(transparent)
            state = (bool(transparent), noactivate)
            if state == self._exstyle_state:
                return
            try:
                user32 = ctypes.windll.user32
                hwnd = int(self.winId())
                ex = user32.GetWindowLongW(hwnd, GWL_EXSTYLE)
                ex |= WS_EX_LAYERED | WS_EX_TOOLWINDOW
                ex = (ex | WS_EX_TRANSPARENT) if transparent else (ex & ~WS_EX_TRANSPARENT)
                ex = (ex | WS_EX_NOACTIVATE) if noactivate else (ex & ~WS_EX_NOACTIVATE)
                user32.SetWindowLongW(hwnd, GWL_EXSTYLE, ex)
                self._exstyle_state = state
            except Exception:
                pass

        def _on_hit_rects(self, payload):
            """JS → Python (GUI thread): whether any interactive control is
            currently visible. While ANY control (Next / choice / token field) is
            showing, the pill is interactive so the controls reliably register
            clicks; otherwise (streaming text only — which is the ONLY state
            while the agent automates Edge) it is fully click-through. No control
            is ever visible during automation, so going interactive then is safe."""
            try:
                data = json.loads(payload or "{}")
            except Exception:
                data = {}
            has_controls = len(data.get("rects") or []) > 0
            self._apply_click_through(not has_controls)
            if has_controls and data.get("input"):
                # Token-input step: pull focus so typing/paste land immediately,
                # without the user having to click the field first.
                try:
                    self.activateWindow()
                    self.raise_()
                except Exception:
                    pass

        @Slot(bool)
        def _on_load_finished(self, ok):
            if self._ready:
                return
            self._ready = True
            _emit("READY")
            _log(f"banner: READY (loadFinished ok={ok})")
            on_focus = None if self._compact else self.do_focus.emit
            threading.Thread(
                target=_stdin_reader,
                args=(self.run_js.emit, self.do_close.emit, on_focus),
                daemon=True,
            ).start()

        @Slot(str)
        def _on_run_js(self, code):
            try:
                self.view.page().runJavaScript(code)
            except Exception:
                pass

        @Slot()
        def _on_focus(self):
            # Setup pill only: pull the window to the foreground so the token
            # <input> receives keystrokes / paste without a click first.
            try:
                self.activateWindow()
                self.raise_()
            except Exception:
                pass

        @Slot()
        def _on_close(self):
            _log("banner: close requested")
            # Unmap the window THIS frame so it vanishes instantly, before the
            # (slightly slower) close()/app.quit()/Chromium teardown. Both the
            # CLOSE command and the stdin-EOF path converge here on the GUI
            # thread, so this covers every close trigger.
            try:
                self.hide()
            except Exception:
                pass
            try:
                self.close()
            finally:
                app = QApplication.instance()
                if app is not None:
                    app.quit()

    try:
        app = QApplication.instance() or QApplication(sys.argv)
        win = _PillWindow(compact)
        win.show()
        rc = app.exec()
        _log(f"banner: app.exec() returned rc={rc}")
    except Exception:
        import traceback
        _log("_run_subprocess_banner crashed:\n" + traceback.format_exc())
        raise

    _emit("CLOSED")
    _log("subprocess exit (CLOSED emitted)")


# ── parent-side wrapper ──────────────────────────────────────────────────


class StatusBanner:
    """Universal pill driver, backed by a subprocess that runs the Qt
    WebEngine pill independently. Surface-agnostic — Telegram/Discord/
    WhatsApp all use the same class and wire protocol."""

    # Module path the subprocess runs in dev mode (`python -m <module>`).
    # The compiled build re-execs AutoCua.exe --banner-mode instead (no -m).
    _PROC_MODULE = "AutoCua.windows.remote_connection.banner"

    # Cap the wait-for-reveal so a JS hiccup that drops the REVEAL event can
    # never deadlock us. Realistic banner messages stream out in well under
    # this — and shorter is better, because the wait is what the user
    # experiences between the message finishing and the button showing.
    _REVEAL_WAIT_SEC = 3.0

    def __init__(self, compact: bool = False):
        self._compact = compact
        self._proc: subprocess.Popen | None = None
        self._stdout_thread: threading.Thread | None = None
        self._closed = threading.Event()
        self._ready = threading.Event()
        self._next_event = threading.Event()
        # Distinguishes a real NEXT click from a subprocess-close that also
        # has to unblock _next_event so waiters don't deadlock. Only the
        # "NEXT" stdout event flips this to True; close-cleanup leaves it
        # False so callers can tell the user dismissed the banner.
        self._next_clicked = False
        self._choice_q: Queue = Queue()
        self._input_q: Queue = Queue()
        # Set initially: no streaming reveal is pending until update() is
        # called. update() clears this; the page's reveal_done (the REVEAL
        # stdout event) re-sets it. wait_for_* block on it before showing
        # their control so the button lands only after the text has.
        self._reveal_event = threading.Event()
        self._reveal_event.set()
        # Per-message sequence number stamped on each MSG and echoed back in
        # REVEAL, so a reveal for an OLDER message (still in the stdout pipe
        # when the next update() cleared the gate) can't release the gate for
        # the new text while it is still typing.
        self._msg_seq = 0

    # ── public API ───────────────────────────────────────────────────────

    def show(self) -> None:
        if self._proc is not None or self._closed.is_set():
            return

        # In the Nuitka build, sys.executable is AutoCua.exe — a compiled C
        # binary with no `-m` module loader. Running it with `-m …banner`
        # silently re-execs the whole AutoCua app (Flask + main webview +
        # bot), giving the user a second main window instead of the pill.
        # Re-exec AutoCua.exe with --banner-mode so app.py's main() can route
        # directly to _run_subprocess_banner. In dev (`python app.py`)
        # sys.executable IS a python interpreter, so the old -m invocation
        # still works and is preferred — it avoids the cost of bootstrapping
        # app.py just to reach the banner.
        # cwd: pin the subprocess to the binary's install dir in the compiled
        # build so native DLL loaders resolve from the install folder
        # regardless of what cwd the parent inherited. In dev mode cwd=None
        # inherits the parent's (the repo root).
        cwd = None
        if _IS_COMPILED:
            exe_dir = os.path.dirname(sys.executable)
            main_exe = os.path.join(exe_dir, "AutoCua.exe")
            args = [main_exe, "--banner-mode"]
            cwd = exe_dir
        else:
            args = [sys.executable, "-m", self._PROC_MODULE]
        if self._compact:
            args.append("--compact")

        try:
            self._proc = subprocess.Popen(
                args,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                # stderr is left attached so the subprocess can write
                # diagnostics to our terminal (useful for debugging,
                # never gets parsed).
                stderr=None,
                text=True,
                bufsize=1,  # line-buffered
                cwd=cwd,
                # On Windows, hide the extra console window subprocess
                # would otherwise spawn.
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            _stderr(
                f"spawned banner subprocess pid={self._proc.pid} "
                f"compact={self._compact}"
            )
        except Exception as e:
            _stderr(f"banner subprocess spawn failed: {e!r}")
            self._proc = None
            return

        # Register so close_all_banners() (app-window-close hook / atexit) can
        # dismiss this pill the instant the user closes the app, instead of it
        # lingering until stdin-EOF fires after the parent's slow teardown.
        with _ACTIVE_BANNERS_LOCK:
            _ACTIVE_BANNERS.add(self)

        self._stdout_thread = threading.Thread(
            target=self._stdout_reader,
            daemon=True,
            name="banner-stdout-reader",
        )
        self._stdout_thread.start()

        # Block until the subprocess emits READY (banner is visible).
        # 15 s ceiling covers a cold Python interpreter start; under
        # normal conditions READY arrives in well under a second.
        if not self._ready.wait(timeout=15):
            _stderr("banner subprocess never emitted READY")

    def update(self, text: str) -> None:
        # Both modes accept MSG — the compact pill renders the thinking-stream
        # text in its msg span and grows to fit; the setup pill streams it
        # letter-by-letter, then shimmers it and posts REVEAL once it lands.
        # Redact any token-like text as a final guard (the setup-wizard prompts
        # and agent thinking-stream never contain one, so this is a no-op there).
        # A streaming reveal is about to start in the page; clear the event so
        # any following wait_for_* call blocks until it posts REVEAL. Only
        # relevant on the setup pill (the compact pill never waits on reveals
        # and its page never posts REVEAL).
        # Pill already gone (subprocess died or close() ran): nothing will ever
        # post REVEAL again, so don't clear the event — a following wait_for_*
        # would otherwise sit on the reveal cap for nothing. Mirrors Linux.
        if self._closed.is_set():
            return
        self._msg_seq += 1
        if not self._compact:
            self._reveal_event.clear()
        self._send({"cmd": "MSG", "text": _redact_secrets(text), "seq": self._msg_seq})

    def _await_reveal(self) -> None:
        """Block until the most recent update()'s reveal animation has
        landed (or the safety cap fires). No-op if no update() is pending —
        the event stays set in that case."""
        self._reveal_event.wait(self._REVEAL_WAIT_SEC)

    # ── embedded coder terminal API (compact mode; callable from any thread) ──
    # While a CLI/coder sub-agent runs, the compact orb pill expands into a
    # terminal panel below the orb. These forward to the subprocess over the
    # JSON wire protocol; the subprocess drives the JS in COMPACT_HTML. All are
    # no-ops on the setup (non-compact) banner. The parent passes raw text — the
    # subprocess does the JS escaping, so never double-escape here.

    def coder_start(self) -> None:
        """Reveal the embedded terminal panel (the pill expands wider+taller)."""
        if not self._compact:
            return
        self._send({"cmd": "CODER_START"})

    def coder_stop(self) -> None:
        """Hide the terminal panel and collapse the pill back to the orb pill."""
        if not self._compact:
            return
        self._send({"cmd": "CODER_STOP"})

    def add_coder(self, coder_id, label: str = "coder") -> None:
        """Add a terminal box for a coder (CLI agent); they stack vertically."""
        if not self._compact:
            return
        self._send({"cmd": "ADD_CODER", "id": str(coder_id)})

    def remove_coder(self, coder_id) -> None:
        """Remove a coder's terminal box (it finished)."""
        if not self._compact:
            return
        self._send({"cmd": "REMOVE_CODER", "id": str(coder_id)})

    def push_cli_line(self, coder_id, line: str) -> None:
        """Stream one of a coder's real output lines into THAT coder's own box."""
        if not self._compact:
            return
        self._send({"cmd": "PUSH_CLI_LINE", "id": str(coder_id), "text": line or ""})

    def set_todo(self, coder_id, todo_text: str) -> None:
        """(Re)render a coder's todo checklist inside its own terminal box."""
        if not self._compact:
            return
        self._send({"cmd": "SET_TODO", "id": str(coder_id), "text": todo_text or ""})

    def add_minion(self, parent_id, minion_id, label: str = "minion") -> None:
        """Add a spinner row for a minion, nested in its parent coder's box."""
        if not self._compact:
            return
        self._send({"cmd": "ADD_MINION", "parent_id": str(parent_id), "id": str(minion_id)})

    def set_minion_line(self, minion_id, line: str) -> None:
        """Stream the minion's latest real output line into its row."""
        if not self._compact:
            return
        self._send({"cmd": "SET_MINION_LINE", "id": str(minion_id), "line": line or ""})

    def remove_minion(self, minion_id) -> None:
        """Remove a minion row (it exited)."""
        if not self._compact:
            return
        self._send({"cmd": "REMOVE_MINION", "id": str(minion_id)})

    def wait_for_next(self, timeout: float | None = None) -> bool:
        if self._compact:
            return True
        if self._proc is None:
            return True
        # Banner already dismissed (subprocess gone) — don't pretend the
        # user clicked Next. Callers use the False return to short-circuit
        # the wizard instead of opening Edge / advancing steps.
        if self._closed.is_set():
            return False
        self._next_clicked = False
        # Clear the click event BEFORE the reveal wait, so a close that lands
        # during streaming (it sets _next_event) is not wiped afterwards.
        self._next_event.clear()
        self._await_reveal()
        self._send({"cmd": "SHOW_NEXT"})
        self._next_event.wait(timeout=timeout)
        self._send({"cmd": "HIDE_NEXT"})
        return self._next_clicked

    def wait_for_choice(
        self, left_label: str, right_label: str, timeout=None
    ):
        if self._compact or self._proc is None:
            return None
        # Banner already dismissed — don't drain the close sentinel and then
        # park on the queue for 10 minutes (same short-circuit as wait_for_next).
        if self._closed.is_set():
            return None
        self._drain(self._choice_q)
        self._await_reveal()
        self._send({
            "cmd": "SHOW_CHOICE",
            "left": left_label,
            "right": right_label,
        })
        try:
            value = self._choice_q.get(timeout=timeout if timeout else 600)
        except Empty:
            value = None
        self._send({"cmd": "CLEAR"})
        return value

    def wait_for_input(self, save_label: str = "Save"):
        if self._compact or self._proc is None:
            return None
        if self._closed.is_set():
            return None
        self._drain(self._input_q)
        self._await_reveal()
        self._send({"cmd": "SHOW_INPUT", "label": save_label})
        try:
            value = self._input_q.get(timeout=600)
        except Empty:
            value = None
        self._send({"cmd": "CLEAR"})
        return value

    def close(self) -> None:
        if self._closed.is_set():
            return
        self._closed.set()
        with _ACTIVE_BANNERS_LOCK:
            _ACTIVE_BANNERS.discard(self)
        # Unblock anything still parked on a Queue/Event before we tear
        # the subprocess down.
        self._next_event.set()
        self._reveal_event.set()
        try:
            self._choice_q.put_nowait(None)
        except Exception:
            pass
        try:
            self._input_q.put_nowait(None)
        except Exception:
            pass

        if self._proc is None:
            return

        # Ask the subprocess to close gracefully (it hides the window this
        # frame — see _PillWindow._on_close), then reap the process OFF this
        # thread. close() is called from pywebview's `closing` event, which
        # runs handlers synchronously and blocks the window teardown, so it
        # must return immediately; the wait/terminate/kill escalation runs on
        # a daemon reaper instead of stalling shutdown for up to 3 s.
        self._send({"cmd": "CLOSE"})
        proc = self._proc
        self._proc = None  # detach so a racing _send / double-close can't touch it

        def _reap(p):
            try:
                p.wait(timeout=3)
            except Exception:
                try:
                    p.terminate()
                    p.wait(timeout=2)
                except Exception:
                    try:
                        p.kill()
                    except Exception:
                        pass

        threading.Thread(
            target=_reap, args=(proc,), daemon=True, name="banner-reaper"
        ).start()

    # ── internals ────────────────────────────────────────────────────────

    def _send(self, msg: dict) -> None:
        """Write a JSON command to the subprocess stdin. Silent on
        broken-pipe errors so a dead subprocess doesn't crash callers."""
        if self._proc is None or self._proc.stdin is None:
            return
        try:
            self._proc.stdin.write(json.dumps(msg) + "\n")
            self._proc.stdin.flush()
        except Exception:
            pass

    def _stdout_reader(self) -> None:
        """Read JSON events from the subprocess and route to local
        Event / Queue primitives so wait_for_* unblock at the right time."""
        if self._proc is None or self._proc.stdout is None:
            return
        for line in self._proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except Exception:
                continue
            event = msg.get("event")
            if event == "READY":
                _stderr("banner subprocess READY — pill visible")
                self._ready.set()
            elif event == "NEXT":
                # Flag must be set BEFORE the Event so a waiter that wakes
                # on _next_event.wait() reads the True value, not the
                # default False left by the close-cleanup path below.
                self._next_clicked = True
                self._next_event.set()
            elif event == "CHOICE":
                self._choice_q.put(msg.get("value", "left"))
            elif event == "SAVE":
                self._input_q.put(msg.get("value", ""))
            elif event == "REVEAL":
                # Only the reveal for the LATEST update() releases the gate; a
                # stale one (its MSG was superseded while its REVEAL was still
                # in the pipe) is dropped. seq 0 = page fallback / unknown —
                # accept it rather than sit on the reveal cap.
                try:
                    seq = int(msg.get("seq", 0) or 0)
                except (TypeError, ValueError):
                    seq = 0
                if seq == 0 or seq == self._msg_seq:
                    self._reveal_event.set()
            elif event == "CLOSED":
                _stderr("banner subprocess CLOSED")
                break

        # Subprocess exited (whether via CLOSED or pipe break). Unblock
        # any pending waiters so callers don't deadlock, and drop ourselves
        # from the active registry (the proc is gone — nothing left to close).
        self._closed.set()
        with _ACTIVE_BANNERS_LOCK:
            _ACTIVE_BANNERS.discard(self)
        self._ready.set()
        self._next_event.set()
        self._reveal_event.set()
        try:
            self._choice_q.put_nowait(None)
        except Exception:
            pass
        try:
            self._input_q.put_nowait(None)
        except Exception:
            pass

    @staticmethod
    def _drain(q: Queue) -> None:
        try:
            while True:
                q.get_nowait()
        except Empty:
            pass


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
    `minion_*`. All StatusBanner methods forward over the subprocess wire
    protocol, which is threadsafe, so handle_event is safe to call from the
    agent/pipe-reader threads.
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
            # CREATE this coder's own terminal box now — it stacks below any
            # other coders' boxes (2 coders → 2 boxes, etc.). The box is created
            # even while the panel is hidden; coderShow (await_start) lays it out
            # and reveals it. Deliberately do NOT expand the panel here: a
            # cli_agent dispatch is non-blocking and the main agent keeps doing
            # on-screen OS actions until it halts at cli_await — expanding now
            # would occlude the screen it's automating.
            self._banner.add_coder(task_id)

        elif event_type == "task_line":
            task_id = args[0] if len(args) > 0 else None
            line = args[1] if len(args) > 1 else ""
            with self._lock:
                is_coder = task_id in self._coder_tasks
            # Drop our own "[telegram]"/"[banner]" diagnostics (never meant for
            # the pill) and redact any token-like text before it can render.
            if (
                is_coder
                and line is not None
                and str(line).strip()
                and not _is_internal_log_line(line)
            ):
                self._banner.push_cli_line(task_id, _redact_secrets(line))

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
            # Drop this coder's terminal box (its minions go with it).
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
            # Nest the row under its PARENT coder's terminal box. Do NOT expand
            # the panel (same reason as task_start — a minion can spawn before the
            # main agent halts at cli_await). coderShow reveals it on await_start.
            self._banner.add_minion(parent_id, minion_id)

        elif event_type == "minion_line":
            minion_id = args[0] if len(args) > 0 else None
            line = args[1] if len(args) > 1 else ""
            with self._lock:
                known = minion_id in self._minion_ids
            if (
                known
                and line is not None
                and str(line).strip()
                and not _is_internal_log_line(line)
            ):
                self._banner.set_minion_line(minion_id, _redact_secrets(line))

        elif event_type == "minion_end":
            minion_id = args[0] if len(args) > 0 else None
            with self._lock:
                self._minion_ids.discard(minion_id)
            self._banner.remove_minion(minion_id)

        # pill_web_loading_* and other events: nothing to do.


# ── module entry: run as subprocess if invoked via `python -m …banner` ──

if __name__ == "__main__":
    _run_subprocess_banner()
