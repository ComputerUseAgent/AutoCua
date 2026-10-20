// Memory ring component — injects the composer's context gauge into the
// composer's toolbar (.input-toolbar, just right of the Fast/Quality toggle,
// before the model picker) and keeps the
// hooks the old vertical bar exposed, so service.py, chat.js and chat_input.js
// drive it unchanged:
//   window.updateMemoryBar(used, cap)  — glide the ring and its centre count to
//                                         used/cap (the MAIN agent's current
//                                         context size vs the fixed 300k memory
//                                         budget). The backend calls this each
//                                         LLM call; the value goes UP as history
//                                         grows and DOWN when the runtime memory
//                                         optimization strips it, and the number
//                                         counts smoothly either way.
//   window.resetMemoryBar()            — back to 0 (a brand-new chat).
//   window.memoryCompressionStart/End() — the arc breathes while the background
//                                         handoff compression runs.
//   window.showMemoryBar/hideMemoryBar() — shown from the first send (or when a
//                                         chat with history opens); only a new
//                                         chat hides it.
// Same self-contained fetch-inject pattern as agent_mode.js / skills.js.
(function () {
    'use strict';

    // Fixed memory budget the ring fills toward; the backend passes the same cap.
    var CAP = 300000;

    // The arc's colour follows its fill: baby purple empty -> reddish orange full.
    var PURPLE = [184, 161, 247];   // #b8a1f7
    var ORANGE = [242, 85, 44];     // #f2552c

    var reduceMotion = !!(window.matchMedia &&
        window.matchMedia('(prefers-reduced-motion: reduce)').matches);

    // The latest values live here rather than only in the DOM, so calls that
    // land before the ring is mounted (a chat restoring its size on launch)
    // still show the moment it mounts.
    var used = 0, cap = CAP, visible = false, compressing = false;
    var shown = 0;          // the count currently drawn; glides toward `used`
    var raf = 0;
    var ring = null, arc = null, value = null;

    // 850 · 1.2k · 45k · 1.3M — short enough for the ring's middle.
    function format(n) {
        n = Math.max(0, Math.round(n));
        if (n < 1000) return String(n);
        var tenths = Math.round(n / 100) / 10;        // 1234 -> 1.2
        if (tenths < 10) return tenths.toFixed(1) + 'k';
        var k = Math.round(n / 1000);
        if (k < 1000) return k + 'k';
        return (Math.round(n / 100000) / 10).toFixed(1) + 'M';
    }

    function draw(n) {
        if (!ring) return;
        var p = Math.max(0, Math.min(1, n / cap));
        arc.style.strokeDashoffset = 100 * (1 - p);
        arc.style.stroke = 'rgb(' + PURPLE.map(function (v, i) {
            return Math.round(v + (ORANGE[i] - v) * p);
        }).join(',') + ')';
        // A round-capped arc of length 0 still paints a dot; hide it when empty.
        arc.style.visibility = p > 0 ? '' : 'hidden';
        value.textContent = format(n);
    }

    function paintState() {
        if (!ring) return;
        ring.classList.toggle('visible', visible);
        ring.classList.toggle('compressing', compressing);
        ring.classList.toggle('full', used >= cap);
        var label = 'Memory: ' + Math.round(used).toLocaleString() +
                    ' / ' + cap.toLocaleString() + ' tokens';
        ring.title = label;
        ring.setAttribute('aria-label', label);
    }

    var easeInOut = function (t) {
        return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
    };

    // Count from what's on screen to `used`: a short glide for a small step,
    // a longer one for a big jump. A new value mid-glide starts from wherever
    // the number is, so it never jumps.
    function glide() {
        cancelAnimationFrame(raf);
        var from = shown, to = used;
        if (reduceMotion || !ring || from === to) { shown = to; draw(shown); return; }
        var dur = 600 + 900 * Math.min(1, Math.abs(to - from) / cap * 4);
        var start = null;
        raf = requestAnimationFrame(function frame(now) {
            if (start === null) start = now;
            var t = Math.min(1, (now - start) / dur);
            shown = from + (to - from) * easeInOut(t);
            draw(shown);
            if (t < 1) raf = requestAnimationFrame(frame);
        });
    }

    function mount() {
        var toolbar = document.querySelector('.input-area-wrapper .input-toolbar');
        if (!toolbar || document.getElementById('memoryRing')) return;
        fetch('memory_bar/memory_bar.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('memoryRing')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var el = holder.querySelector('.memory-ring');
                if (!el) return;
                // Right after the Fast/Quality toggle, so Shell use (where the
                // toggle fades out) leaves no gap before the model picker.
                var toggle = toolbar.querySelector('.fast-quality-toggle');
                toolbar.insertBefore(el, toggle ? toggle.nextSibling : toolbar.firstChild);
                ring = el;
                arc = el.querySelector('.memory-ring-arc');
                value = el.querySelector('.memory-ring-value');
                paintState();
                shown = used;           // land on the latest value, no count-up
                draw(shown);
            })
            .catch(function () { /* non-fatal: the ring just won't render */ });
    }

    window.updateMemoryBar = function (u, c) {
        cap = Number(c) || CAP;
        used = Math.max(0, Number(u) || 0);
        paintState();
        glide();
    };

    // Brand-new chat: empty the ring. A compression still marked as running
    // belonged to the old chat (its 'end' is dropped once the run is retired).
    window.resetMemoryBar = function () { compressing = false; window.updateMemoryBar(0, CAP); };

    window.memoryCompressionStart = function () { compressing = true; paintState(); };
    window.memoryCompressionEnd = function () { compressing = false; paintState(); };

    window.showMemoryBar = function () { visible = true; paintState(); };
    window.hideMemoryBar = function () { visible = false; paintState(); };

    // chat_input.html is itself fetch-injected — mount on its ready event, with
    // a direct attempt in case that fired.
    document.addEventListener('chatinput:ready', mount);
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
