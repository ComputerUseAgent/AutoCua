// =====================================================================
// Top-left SCREEN OVERLAY — the shell terminal SWAPS IN over the live screenshot
// while a shell command runs (the screenshot fades out, the terminal fades in; on
// finish it swaps back). Self-contained here. Driven by the backend's existing
// pywebview hooks:
//     window.shellStart() / window.shellResult() / window.shellEnd()  — shell terminal
//     window.webSearchStart() / window.webSearchEnd()                 — no-ops, see below
// A Three.js earth used to swap in here for web searches; it was removed — a web
// search now just leaves the live screenshot up. The two hooks stay defined (and
// do nothing) because service.py / script.js / chat_input.js still call them.
// DOM lives in container/top_left/top_left.html (#shellPanel/#shellTerminalContainer);
// globe_shell.css cross-fades it with the screenshot. Elements are resolved lazily
// (top_left.html is fetch-injected), so this file's load order doesn't matter.
// =====================================================================
(function () {
    'use strict';

    // Fade the live screenshot out while the shell overlay is showing, and back
    // in when it's gone (.overlay-active on the top-left zone).
    const fadeScreenshot = (hide) => {
        const z = document.getElementById('zoneTopLeft');
        if (z) z.classList.toggle('overlay-active', hide);
    };

    /* ============================================================
       WEB SEARCH  (no visual — the screenshot stays up)
       ============================================================ */
    // The globe overlay was removed; these remain so the backend's
    // evaluate_js("window.webSearchStart()") and the run-end / stop paths in
    // script.js + chat_input.js keep resolving instead of throwing.
    window.webSearchStart = () => {};
    window.webSearchEnd = () => {};

    /* ============================================================
       SHELL TERMINAL  (coder-card style — transparent `>` cmd + dot loader,
       then an L-connector down to the output that comes back)
       ============================================================ */
    // Resolved lazily (top_left.html is fetch-injected) — re-query on each event.
    let shellTerminalContainer = null;
    let shellCmdText = null;
    let shellCmdLine = null;
    let shellProgress = null;
    let shellOutLine = null;
    let shellOutText = null;

    const resolveShellEls = () => {
        shellTerminalContainer = document.getElementById('shellTerminalContainer');
        shellCmdText = document.getElementById('shellCmdText');
        shellCmdLine = document.getElementById('shellCmdLine');
        shellProgress = document.getElementById('shellProgress');
        shellOutLine = document.getElementById('shellOutLine');
        shellOutText = document.getElementById('shellOutText');
    };

    let shellCmdStream = null;     // active command stream handle ({ stop })
    let shellOutStream = null;     // active output stream handle
    let shellCmdFull = '';         // full command text (to force-complete on result)

    // Smooth char-by-char streamer — a port of coder_card.js makeLineStreamer's
    // per-letter fade: each character is appended as a span that fades opacity
    // 0→1, a new one every SH_STAGGER ms, so the leading edge is a soft fade-in
    // wave (NOT a chunky substring jump). On done the spans are flattened back to
    // plain text so the caller can apply the shimmer cleanly. Returns { stop }.
    // FAST, coder-paced: reveal SH_STEP chars per SH_STAGGER tick (browser timers
    // clamp to a few ms, so batching is how you get real speed), each char fading
    // in over SH_CHAR_FADE — same per-letter fade mechanic as coder_card.js, same
    // brisk cadence (STEP 5 / 4ms / 30ms), not the earlier sluggish 1-char/22ms.
    const SH_STEP = 5;          // chars revealed per tick
    const SH_STAGGER = 4;       // ms between ticks
    const SH_CHAR_FADE = 30;    // ms opacity 0→1 per letter
    const streamChars = (element, text, onDone) => {
        element.textContent = '';
        const chars = Array.from(String(text));   // codepoint-safe (emoji/surrogates)
        let i = 0;
        let timer = null;
        const tick = () => {
            for (let n = 0; n < SH_STEP; n++) {
                if (i >= chars.length) {
                    element.textContent = element.textContent;   // flatten spans → plain text
                    timer = null;
                    if (onDone) onDone();
                    return;
                }
                const span = document.createElement('span');
                span.className = 'sh-char';
                span.textContent = chars[i];
                span.style.opacity = '0';
                span.style.transition = 'opacity ' + SH_CHAR_FADE + 'ms ease-out';
                element.appendChild(span);
                // First char shows instantly (no empty flash); the rest fade in.
                if (i === 0) span.style.opacity = '1';
                else requestAnimationFrame(() => { span.style.opacity = '1'; });
                i++;
            }
            timer = setTimeout(tick, SH_STAGGER);
        };
        tick();
        return { stop: () => { if (timer) { clearTimeout(timer); timer = null; } } };
    };

    const resetShellTerminal = () => {
        resolveShellEls();
        if (shellCmdStream) { shellCmdStream.stop(); shellCmdStream = null; }
        if (shellOutStream) { shellOutStream.stop(); shellOutStream = null; }
        shellCmdFull = '';
        if (shellCmdText) { shellCmdText.textContent = ''; shellCmdText.classList.remove('sh-shimmer'); }
        if (shellOutText) { shellOutText.textContent = ''; shellOutText.classList.remove('sh-shimmer'); }
        if (shellOutLine) { shellOutLine.classList.remove('show', 'fail'); }
        if (shellCmdLine) { shellCmdLine.classList.remove('running'); }
        if (shellProgress) { shellProgress.classList.remove('show'); }
    };

    // Swap the shell terminal in/out over the screenshot (#shellPanel overlay).
    const setShellPanel = (on) => {
        const p = document.getElementById('shellPanel');
        if (p) p.classList.toggle('is-active', on);
    };

    window.shellStart = (command, label) => {
        resetShellTerminal();
        if (!shellTerminalContainer) return;

        fadeScreenshot(true);
        setShellPanel(true);

        // Small coder spinner sits at the HEAD (right after `>`) the whole time the
        // command runs — never floats out to the wrapped line's end. Type the
        // command char-by-char; once typed, let it SHIMMER while we await the result.
        shellCmdFull = command || 'executing…';
        if (shellCmdLine) shellCmdLine.classList.add('running');  // blinking cursor dot after `>`
        if (shellProgress) shellProgress.classList.add('show');   // coder-style loading bar while running
        if (shellCmdText) {
            shellCmdStream = streamChars(shellCmdText, shellCmdFull, () => {
                shellCmdText.classList.add('sh-shimmer');
            });
        }
    };

    window.shellResult = (status, output) => {
        resolveShellEls();
        if (!shellTerminalContainer) return;

        // Command finished running — force-complete its stream (show the full
        // command even if the result beat the typewriter), drop the shimmer + loader.
        if (shellCmdStream) { shellCmdStream.stop(); shellCmdStream = null; }
        if (shellCmdText) { shellCmdText.textContent = shellCmdFull; shellCmdText.classList.remove('sh-shimmer'); }
        if (shellCmdLine) shellCmdLine.classList.remove('running');
        if (shellProgress) shellProgress.classList.remove('show');   // loading done

        const text = output
            ? (output.length > 120 ? output.substring(0, 120) + '…' : output)
            : (status === 'success' ? 'done' : 'failed');

        // Reveal the L-connected output line and type the result in — char-by-char
        // too, but WITHOUT shimmer (the shimmer is the command's running indicator).
        if (shellOutLine) {
            shellOutLine.classList.toggle('fail', status !== 'success');
            shellOutLine.classList.add('show');
        }
        if (shellOutText) {
            shellOutStream = streamChars(shellOutText, text);
        }
    };

    window.shellEnd = () => {
        resolveShellEls();
        setShellPanel(false);
        fadeScreenshot(false);
        setTimeout(resetShellTerminal, 700);
    };
})();
