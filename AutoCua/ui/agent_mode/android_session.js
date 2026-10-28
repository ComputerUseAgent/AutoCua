// Android session connector — ios_session.js's twin for the Android phone,
// attached to the Agent-mode menu. Selecting Mobile use → Android starts the
// phone helper on the newest phone Settings → Connect Device connected (adb,
// scrcpy's server and Helper.java over the cable; nothing is installed on the
// phone); switching to anything else (Computer use / iOS) stops it.
//
// While connecting, the chat box placeholder animates "Pairing." → "Pairing.."
// → "Pairing..." on loop and the green glow breathes; on success it shows
// "Device connected" for a moment and then returns to the normal text. On
// failure the placeholder explains, and the menu quietly reverts to Computer use.
//
// Backend contract:
//   POST /api/android/activate    -> starts the helper and reads one screen
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
        fetch('/api/android/activate', { method: 'POST' })
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
        if (el && savedPlaceholder !== null) el.placeholder = savedPlaceholder;
        savedPlaceholder = null;
        restoreTimer = null;
    }

    // ---- wiring to the Agent-mode menu ----
    // androidActive: Android is the selection and a helper was asked for from
    // here (switching away must stop it). pairing: that activate() is still on
    // the way, so a second click must not restart it half-way.
    var androidActive = false;
    var pairing = false;

    document.addEventListener('agentmode:changed', function (e) {
        var d = e.detail || {};
        var wantAndroid = (d.mode === 'mobile' && d.sub === 'android');

        if (wantAndroid) {
            // EVERY Android pick (re)activates, as every iOS pick does: New
            // chat stops the helper behind the picker's back, the cable can
            // drop. A live helper answers at once ("Device connected").
            if (pairing) return;
            androidActive = true;
            pairing = true;
            // A tick later: iOS's listener (after this one) ends its own
            // animation first, so the placeholder saved here is the real one.
            setTimeout(function () {
                if (!androidActive) return;          // switched away meanwhile
                startPairingAnim();
                activate({
                    stale: function () { return !androidActive; },
                    onConnected: function () {
                        pairing = false;
                        // graceful: the current breath completes, THEN the glow settles
                        endPairingAnim('Device connected', 2200, true);
                    },
                    onFail: function (why) {
                        pairing = false;
                        androidActive = false;
                        // The sentence that names the cause, in the placeholder.
                        var msg = why
                            ? ('Android: ' + (why.length > 150 ? why.slice(0, 147) + '…' : why))
                            : 'Connecting failed. Check Settings → Connect Device';
                        if (why) { try { console.error('[android] activate failed:', why); } catch (x) {} }
                        endPairingAnim(msg, why ? 9000 : 3200);
                        // quietly put the menu back on Computer use (no re-dispatch)
                        document.dispatchEvent(new CustomEvent('agentmode:set', {
                            detail: { mode: 'computer' }
                        }));
                    }
                });
            }, 0);
        } else if (androidActive) {
            androidActive = false;
            pairing = false;
            deactivate();                        // switched away -> stop the helper
            endPairingAnim(null);                // stop any mid-pairing animation
        }
    });

    window.androidSession = { activate: activate, deactivate: deactivate };
})();
