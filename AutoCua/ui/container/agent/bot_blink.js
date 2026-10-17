// =====================================================================
// The small bots' blink: the bot beside the AI's final message
// (run_reply.css; Shell use's answers, coder_card.css) and the run's bot
// beside the bottom container while it runs (chain_bot.css). They blink the
// way the owner's bot pages do (logo/*.html, autocua-helmet.html): a 0.78 s
// blink (their data-blink) that closes on a smootherstep, rests shut for a
// beat and opens; the next one 3 to 5.4 s later, and about one time in four
// a second blink 0.2 s after it. Each bot on its own time. A reply bot's eyes are a layer of its
// box's ::before: their height goes on the box as --reply-eyes. The run's
// bot's eyes are an image: they squeeze by transform. Between blinks
// nothing changes.
// =====================================================================
(function () {
    'use strict';

    var SEL = '.agent-container.show-reply, .coder-card.has-head .cc-out-text .cc-line.cc-answer, ' +
              '.chat-turn.live.cb-on > .chain-bot.docked .eyes';
    var BLINK = 0.78;                 // s, the pages' data-blink
    var bots = new WeakMap();         // box or eyes -> { next, at, dbl, seen }
    var blinking = [];                // ones mid-blink (or waiting for a second one)
    var raf = 0, round = 0;

    function clock() { return performance.now() / 1000; }
    function rnd(a, b) { return a + Math.random() * (b - a); }
    function cl(u) { return u < 0 ? 0 : u > 1 ? 1 : u; }
    function smooth(u) { u = cl(u); return u * u * u * (u * (u * 6 - 15) + 10); }
    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    // The eyes' height at u (0..1) of a blink, the pages' curve: close, rest
    // shut for a beat (a sharp turn at the bottom reads as a snap), open.
    function eyes(u) {
        var k = u < 0.42 ? smooth(u / 0.42) : u < 0.52 ? 1 : 1 - smooth((u - 0.52) / 0.48);
        return 1 - 0.90 * k;
    }

    function show(el, v) {
        if (el.tagName === 'IMG') el.style.transform = 'scaleY(' + v + ')';
        else el.style.setProperty('--reply-eyes', v);
    }
    function rest(el) {
        if (el.tagName === 'IMG') el.style.removeProperty('transform');
        else el.style.removeProperty('--reply-eyes');
    }

    function frame() {
        raf = 0;
        var t = clock();
        blinking = blinking.filter(function (el) {
            var b = bots.get(el);
            if (!el.isConnected) { b.at = -1; return false; }
            var u = (t - b.at) / BLINK;
            if (u < 1) {
                if (u >= 0) show(el, eyes(u).toFixed(3));
                return true;
            }
            rest(el);
            if (b.dbl) { b.dbl = false; b.at = t + 0.20; return true; }    // the second blink
            b.at = -1;
            b.next = t + rnd(3.0, 5.4);
            b.dbl = Math.random() < 0.28;
            return false;
        });
        if (blinking.length) raf = requestAnimationFrame(frame);
    }

    // A tenth of a second apart: a bot that just showed (or showed again) gets
    // its first blink a moment later, each its own; the ones due blink.
    function check() {
        round++;
        if (reduced() || document.hidden) return;
        var t = clock();
        var found = document.querySelectorAll(SEL);
        for (var i = 0; i < found.length; i++) {
            var el = found[i], b = bots.get(el);
            if (!b || (b.seen !== round - 1 && b.at < 0)) {
                b = { next: t + rnd(1.5, 4.5), at: -1, dbl: false };
                bots.set(el, b);
            }
            b.seen = round;
            if (b.at < 0 && t >= b.next) {
                b.at = t;
                blinking.push(el);
            }
        }
        if (blinking.length && !raf) raf = requestAnimationFrame(frame);
    }
    setInterval(check, 100);
})();
