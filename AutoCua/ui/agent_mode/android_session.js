// Android session connector — ios_session.js's twin for the Android phone,
// attached to the Agent-mode menu. Selecting Mobile use → Android starts the
// phone helper on the newest phone Settings → Connect Device connected (adb,
// scrcpy's server and Helper.java; nothing is installed on the phone), over
// Wi-Fi or the cable as the chat box's Wireless / Wired toggle says
// (chat_input/link_toggle.js; flipping it connects again); switching to
// anything else (Computer use / iOS) stops it.
//
// While connecting, the chat box is read only (no typing, nothing sent), its
// placeholder animates "Pairing." → "Pairing.." → "Pairing..." on loop, the
// green glow breathes and a circle turns beside the picker (agent_mode.tsx's
// #pairStatus); on success it shows "Device connected" for a moment and then
// returns to the normal text. On failure Android stays picked and the box
// stays read only until it pairs: the placeholder names the cause in a few
// words, a little smaller and on one line, and the circle becomes an arrow
// that pairs again (flipping the toggle does too). Another mode picked frees
// the box; so does New chat or a reopened chat, which set the picker silently.
//
// Backend contract:
//   POST /api/android/activate    -> {link: "wireless" | "wired"}: starts the
//                                    helper and reads one screen
//                                    through it, then answers: {ok, state:
//                                    "connected"} or {ok: false, error, hint}
//                                    (no status poll, unlike WDA)
//   POST /api/android/deactivate  -> stops the helper
//
// Loaded BEFORE ios_session.js (index.html), so its menu listener runs first:
// leaving Android puts the placeholder back before iOS saves it, and coming
// to Android starts a tick later, after iOS's listener has put it back.
(function () {
    'use strict';

    // ---- session core ----
    // gen drops an answer that comes after the pick changed (a newer pick, or
    // a switch away); a phone that connected anyway is stopped again below.
    var gen = 0;

    // The backend says WHY it could not connect (error + hint): carry it out.
    function reasonOf(d) {
        if (!d) return '';
        return [d.error, d.hint].filter(Boolean).join('. ');
    }

    function stopHelper() {
        fetch('/api/android/deactivate', { method: 'POST' }).catch(function () {});
    }

    function activate(cb) {
        cb = cb || {};
        var my = ++gen;
        fetch('/api/android/activate', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            // Wireless or Wired: the chat box's toggle (chat_input/link_toggle.js)
            body: JSON.stringify({ link: window.androidLink ? window.androidLink() : 'wireless' })
        })
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (my !== gen) { if (d.ok && cb.stale && cb.stale()) stopHelper(); return; }
                if (d.ok && d.state === 'connected') { if (cb.onConnected) cb.onConnected(); return; }
                if (cb.onFail) cb.onFail(reasonOf(d));
            })
            .catch(function (e) {
                if (my !== gen) return;
                if (cb.onFail) cb.onFail('the AutoCua backend did not answer (' + (e && e.message || 'network error') + ')');
            });
    }

    function deactivate() {
        gen++;                                  // an answer still on the way is dropped
        stopHelper();
    }

    // ---- chat placeholder animation + breathing glow ----
    function chatInput() { return document.querySelector('.chat-input'); }
    function composer() { return document.querySelector('.input-area-wrapper'); }
    var dotsTimer = null, restoreTimer = null, savedPlaceholder = null;

    // Breathing glow via the Web Animations API. The rest frame EQUALS the
    // steady Android glow (agent_mode.css), and stopping asks the browser to
    // end the animation exactly at the current cycle's boundary — so the
    // settle into the stable glow is frame-perfect, no JS-timing snap.
    var BREATHE_MS = 1800;
    var GLOW_REST = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 14px 28px -16px rgba(61,220,132,0.32), ' +
        'inset 14px 0 28px -16px rgba(61,220,132,0.19), ' +
        'inset 0 -14px 28px -16px rgba(0,199,190,0.28), ' +
        'inset -14px 0 28px -16px rgba(0,199,190,0.19)';
    var GLOW_PEAK = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 16px 32px -12px rgba(61,220,132,0.55), ' +
        'inset 16px 0 32px -12px rgba(61,220,132,0.32), ' +
        'inset 0 -16px 32px -12px rgba(0,199,190,0.48), ' +
        'inset -16px 0 32px -12px rgba(0,199,190,0.32)';
    var breatheAnim = null;

    function startBreathing() {
        var box = composer();
        if (!box || !box.animate || breatheAnim) return;
        breatheAnim = box.animate(
            [{ boxShadow: GLOW_REST }, { boxShadow: GLOW_PEAK }, { boxShadow: GLOW_REST }],
            { duration: BREATHE_MS, iterations: Infinity, easing: 'ease-in-out' }
        );
    }
    function stopBreathing(graceful) {
        if (!breatheAnim) return;
        var anim = breatheAnim;
        breatheAnim = null;
        if (!graceful) { anim.cancel(); return; }
        // Finish the in-flight cycle: cap iterations at the next whole cycle;
        // the browser ends it ON the boundary frame (== steady glow), then
        // cancel() drops the effect with zero visual delta.
        try {
            var cycles = Math.max(1, Math.ceil((anim.currentTime || 1) / BREATHE_MS));
            anim.effect.updateTiming({ iterations: cycles });
            anim.onfinish = function () { anim.cancel(); };
        } catch (e) {
            anim.cancel();
        }
    }

    function startPairingAnim(label) {
        label = label || 'Pairing';
        startBreathing();                            // green glow breathes while connecting
        var el = chatInput();
        if (!el) return;
        clearTimeout(restoreTimer); restoreTimer = null;
        if (savedPlaceholder === null) savedPlaceholder = el.placeholder;
        el.classList.remove('pair-note');           // a failure note still showing goes
        el.readOnly = true;                         // no typing while connecting,
        el.blur();                                  // and no focus, as a disabled box
        clearInterval(dotsTimer);
        var n = 1;
        el.placeholder = label + '.';
        dotsTimer = setInterval(function () {
            n = (n % 3) + 1;                     // . .. ... . .. ...
            var e2 = chatInput();
            if (e2) e2.placeholder = label + '...'.slice(0, n);
        }, 400);
    }
    function endPairingAnim(message, holdMs, gracefulGlow) {
        stopBreathing(gracefulGlow);
        clearInterval(dotsTimer); dotsTimer = null;
        var el = chatInput();
        if (!el) { savedPlaceholder = null; return; }
        el.readOnly = false;                        // the box types again
        if (message) {
            el.placeholder = message;
            restoreTimer = setTimeout(restorePlaceholder, holdMs || 2000);
        } else {
            clearTimeout(restoreTimer); restoreTimer = null;
            restorePlaceholder();
        }
    }
    function restorePlaceholder() {
        var el = chatInput();
        if (el) el.classList.remove('pair-note');
        if (el && savedPlaceholder !== null) el.placeholder = savedPlaceholder;
        savedPlaceholder = null;
        restoreTimer = null;
    }
    // A run starts while "Device connected" still shows: the normal hint comes
    // back now, else agent_mode.js keeps "Device connected" as the hint it
    // puts back after the run (chat_input.js calls this).
    function settle() {
        if (restoreTimer === null) return;
        clearTimeout(restoreTimer);
        restorePlaceholder();
    }
    // It did not pair: the reason stays in the placeholder, smaller and on one
    // line (chat_input.css), and the box stays read only till it pairs or
    // another mode is picked (endPairingAnim(null) then).
    function failPairingAnim(message) {
        stopBreathing(false);
        clearInterval(dotsTimer); dotsTimer = null;
        var el = chatInput();
        if (!el) return;
        el.classList.add('pair-note');
        el.placeholder = message;
    }
    // The circle beside the picker turns ('pairing'), offers to pair again
    // ('failed') or goes (''): agent_mode.css reads the wrap's data-pair.
    function pairState(s) {
        var wrap = document.getElementById('agentModeWrap');
        if (wrap) wrap.dataset.pair = s;
    }

    // ---- wiring to the Agent-mode menu ----
    // androidActive: Android is the selection and a helper was asked for from
    // here (switching away must stop it). pairing: that activate() is still on
    // the way, so a second click must not restart it half-way. failed: it did
    // not pair, and Android is still the selection.
    var androidActive = false;
    var pairing = false;
    var failed = false;

    // Connect the picked phone: the pairing look, then activate().
    function connect() {
        if (!androidActive) return;              // switched away meanwhile
        pairState('pairing');
        startPairingAnim();
        activate({
            stale: function () { return !androidActive; },
            onConnected: function () {
                pairing = false;
                pairState('');
                // graceful: the current breath completes, THEN the glow settles
                endPairingAnim('Device connected', 2200, true);
            },
            onFail: function (why) {
                pairing = false;
                failed = true;
                pairState('failed');
                // The cause only, the reason's first sentence (as iOS does).
                var cause = why.split(/\. | — /)[0];
                var msg = cause
                    ? ('Android: ' + (cause.length > 80 ? cause.slice(0, 79) + '…' : cause))
                    : 'Connecting failed. Check Settings → Connect Device';
                if (why) { try { console.error('[android] activate failed:', why); } catch (x) {} }
                failPairingAnim(msg);
            }
        });
    }

    // EVERY Android pick (re)activates, as every iOS pick does: New chat stops
    // the helper behind the picker's back, the cable can drop. A live helper
    // answers at once ("Device connected"). The circle's arrow pairs again
    // the same way.
    function pair() {
        if (pairing) return;
        androidActive = true;
        pairing = true;
        failed = false;
        // A tick later: iOS's listener (after this one) ends its own
        // animation first, so the placeholder saved here is the real one.
        setTimeout(connect, 0);
    }

    // Switched away: stop the helper, and the box is free again.
    function leave() {
        androidActive = false;
        pairing = false;
        failed = false;
        pairState('');
        deactivate();
        endPairingAnim(null);                    // stop any mid-pairing animation
    }

    document.addEventListener('agentmode:changed', function (e) {
        var d = e.detail || {};
        if (d.mode === 'mobile' && d.sub === 'android') pair();
        else if (androidActive) leave();
    });

    // New chat and a reopened chat set the picker SILENTLY (chat.js): off
    // Android, that is a switch away too. A tick later, once agent_mode.js
    // has painted the new selection on the wrap.
    document.addEventListener('agentmode:set', function () {
        setTimeout(function () {
            var wrap = document.getElementById('agentModeWrap');
            if (androidActive && !(wrap && wrap.dataset.mode === 'mobile' && wrap.dataset.sub === 'android')) leave();
        }, 0);
    });

    // The circle's arrow, after a pairing that did not work: pair again.
    document.addEventListener('click', function (e) {
        if (failed && androidActive && e.target.closest && e.target.closest('#pairStatus')) pair();
    });

    // The Wireless / Wired toggle flipped (link_toggle.js): the picked phone
    // connects again, the new way, also after one that did not pair. Its
    // helper is stopped first and the new one asked for only then, so the two
    // never cross on the backend, and a new way that fails leaves nothing
    // running.
    document.addEventListener('androidlink:changed', function () {
        if (!androidActive || pairing) return;
        pairing = true;
        failed = false;
        pairState('pairing');
        startPairingAnim();
        var my = ++gen;                          // an answer still on the way is dropped
        fetch('/api/android/deactivate', { method: 'POST' }).catch(function () {}).then(function () {
            if (my === gen) connect();
        });
    });

    window.androidSession = { activate: activate, deactivate: deactivate, settle: settle };
})();
