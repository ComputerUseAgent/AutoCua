// Memory bar component — injects the far-right vertical context gauge into <body>
// (fixed, background, pointer-transparent), and exposes the backend hooks:
//   window.updateMemoryBar(used, cap)  — set the fill height to used/cap (the
//                                         MAIN agent's current context size vs the
//                                         fixed 300k memory budget). The backend
//                                         calls this each LLM call; the value goes
//                                         UP as history grows and DOWN when the
//                                         runtime memory optimization strips it.
//   window.resetMemoryBar()            — back to 0 (a brand-new chat).
// Same self-contained fetch-inject pattern as left_bar.js / chat.js.
(function () {
    'use strict';

    // Fixed memory budget the bar fills toward; the backend passes the same cap.
    // 300k = headroom for the future memory-compression system.
    var CAP = 300000;

    function inject() {
        if (document.getElementById('memoryBar')) return; // already mounted
        fetch('memory_bar/memory_bar.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('memoryBar')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var bar = holder.querySelector('.memory-bar');
                if (bar) document.body.appendChild(bar);
            })
            .catch(function () { /* non-fatal: the bar just won't render */ });
    }

    // Set the fill to used/cap (clamped 0..100%); turn red at/over the cap.
    window.updateMemoryBar = function (used, cap) {
        cap = cap || CAP;
        var pct = Math.max(0, Math.min(100, (Number(used) || 0) / cap * 100));
        var fill = document.querySelector('.memory-bar .memory-bar-fill');
        if (fill) fill.style.height = pct + '%';
        var bar = document.getElementById('memoryBar');
        if (bar) bar.classList.toggle('full', pct >= 100);
    };

    // Brand-new chat: empty the bar.
    window.resetMemoryBar = function () { window.updateMemoryBar(0, CAP); };

    // Memory compression indicator — the Memory logo blinks red while the
    // background handoff compression runs (backend signals start/end).
    window.memoryCompressionStart = function () {
        var bar = document.getElementById('memoryBar');
        if (bar) bar.classList.add('compressing');
    };
    window.memoryCompressionEnd = function () {
        var bar = document.getElementById('memoryBar');
        if (bar) bar.classList.remove('compressing');
    };

    // Visibility — the bar is hidden by default and only shown while the agent
    // is running (shown on send, hidden when the run ends).
    window.showMemoryBar = function () {
        var bar = document.getElementById('memoryBar');
        if (bar) bar.classList.add('visible');
    };
    window.hideMemoryBar = function () {
        var bar = document.getElementById('memoryBar');
        if (bar) bar.classList.remove('visible');
    };

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', inject);
    } else {
        inject();
    }
})();
