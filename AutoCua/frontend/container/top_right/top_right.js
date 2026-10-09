// =====================================================================
// Top-right container — LIVE "tracking progress" stream. Mounts top_right.html,
// then exposes window.trackingProgress { push, start, end }. While the agent runs,
// each scratchpad entry (fed from window.streamMilestone in script.js) streams in
// as a circle-bullet line on a connecting line, conveyor-scrolling up as it fills;
// the content fades out when the run ends. Chain mechanics mirror bottom_left.js.
// =====================================================================
(function () {
    'use strict';

    var TYPE_MS = 5;            // delay between typewriter ticks
    var CHARS_PER_TICK = 2;     // chars revealed per tick (TYPE_MS/CHARS_PER_TICK ≈ per-char speed)

    var zoneEl = null, flowEl = null, treeEl = null, frontier = -1;

    /* ---------------- blazing-fast typewriter (from bottom_left) ----------------
       Entries arrive as HTML (frontend/markdown.py). Typing markup character by
       character would show raw tags mid-animation, so we type the PLAIN text and
       swap the real markup in on the final tick — the line lands formatted. */
    function typeText(el, text, onTick, markup) {
        if (!el) return;
        clearTimeout(el._typeTimer);
        el.textContent = '';
        var i = 0;
        (function tick() {
            i = Math.min(i + CHARS_PER_TICK, text.length);
            el.textContent = text.slice(0, i);
            if (i >= text.length) {
                if (markup) {
                    el.innerHTML = markup;                // already escaped server-side
                    el.classList.add('md');
                }
                if (onTick) onTick();                     // final height may differ again
                return;
            }
            if (onTick) onTick();                         // text may wrap -> height grew, re-pin scroll
            el._typeTimer = setTimeout(tick, TYPE_MS);
        })();
    }

    // Plain text behind rendered Markdown — what the typewriter actually types.
    function htmlToText(markup) {
        var d = document.createElement('div');
        d.innerHTML = String(markup == null ? '' : markup);
        return (d.textContent || '').trim();
    }

    /* ---------------- conveyor scroll ----------------
       Entries wrap to as many lines as they need, so heights vary — we measure the
       real layout and pin the BOTTOM of the stream to the bottom of the box. Once
       the content overflows, older entries slide up past the top mask and dissolve. */
    function scrollToFrontier() {
        if (!treeEl || !flowEl) return;
        var overflow = Math.max(0, treeEl.offsetHeight - flowEl.clientHeight);
        treeEl.style.transform = 'translateY(' + (-overflow) + 'px)';
    }

    /* ---------------- one scratchpad entry: dot bullet + typed text ---------------- */
    function addEntry(text, markup) {
        if (!treeEl) return;
        var item = document.createElement('div');
        item.className = 'br-item';
        var dot = document.createElement('span');
        dot.className = 'br-circle';
        var word = document.createElement('span');
        word.className = 'br-word';
        item.appendChild(dot);
        item.appendChild(word);
        treeEl.appendChild(item);

        frontier += 1;
        scrollToFrontier();                               // slide up if it overflows
        item.classList.add('line-in');                    // draw the connecting line
        setTimeout(function () {
            item.classList.add('icon-in');                // reveal the dot
            typeText(word, text, scrollToFrontier, markup);  // type it; re-pin as it wraps
        }, 320);                                          // dot/text land as the line reaches them
    }

    // Strip a leading "N. " numbering — the circle bullet replaces it.
    function stripNum(text) {
        var s = String(text == null ? '' : text).trim();
        var dot = s.indexOf('. ');
        if (dot > 0 && /^\d+$/.test(s.slice(0, dot))) return s.slice(dot + 2);
        return s;
    }

    function clearTree() {
        if (treeEl) {
            var words = treeEl.querySelectorAll('.br-word');
            for (var i = 0; i < words.length; i++) clearTimeout(words[i]._typeTimer);
            treeEl.innerHTML = '';
            treeEl.style.transition = 'none';             // snap back to top, no animation
            treeEl.style.transform = 'translateY(0)';
            void treeEl.offsetHeight;
            treeEl.style.transition = '';
        }
        frontier = -1;
    }

    // Public API — driven by script.js (streamMilestone -> push; start/end on run lifecycle).
    window.trackingProgress = {
        push: function (payload) {                        // one scratchpad entry arrived
            if (!treeEl) return;
            // payload is rendered Markdown (markdown.py, already number-stripped).
            // stripNum still runs on the PLAIN text — never on the markup, where
            // a '. ' inside the content would slice through a tag.
            var markup = String(payload == null ? '' : payload);
            var t = stripNum(htmlToText(markup));
            if (!t) return;
            // reveal (label + stream) only when there's actually something written
            if (zoneEl && frontier < 0) zoneEl.classList.add('br-active');
            addEntry(t, markup);
        },
        start: function () {                              // new run: reset; stay hidden until 1st entry
            if (zoneEl) zoneEl.classList.remove('br-active');
            clearTree();
        },
        end: function () {                               // run done/stopped: fade content out
            if (zoneEl) zoneEl.classList.remove('br-active');
        },
    };

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.top-right-zone')) return;
        fetch('container/top_right/top_right.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.top-right-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.top-right-zone');
                if (!zone) return;
                grid.appendChild(zone);
                zoneEl = zone;
                flowEl = zone.querySelector('.br-flow');
                treeEl = zone.querySelector('.br-tree');
                // re-pin when the window resizes (wrapping changes with width)
                window.addEventListener('resize', function () { scrollToFrontier(); });
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
