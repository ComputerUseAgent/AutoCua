// iOS session connector — attaches the proven connect logic to the Agent-mode
// menu. Selecting Mobile use → iOS activates the paired iPhone's WDA session:
// over the cable a fresh session (no rebuild, no reinstall), over Wi-Fi Xcode
// starts it, as the chat box's Wireless / Wired toggle says
// (chat_input/link_toggle.js; flipping it connects again); switching to
// anything else (Computer use / Android) kills it instantly.
//
// While connecting, the chat box is read only (no typing, nothing sent), its
// placeholder animates "Pairing." → "Pairing.." → "Pairing..." on loop and a
// circle turns beside the picker (agent_mode.tsx's #pairStatus); on success
// it shows "Device connected" for a moment and then returns to the normal
// "Type your task..." text. On failure iOS stays picked and the box stays read
// only until it pairs: the placeholder names the cause in a few words, a
// little smaller and on one line, and the circle becomes an arrow that pairs
// again. Another mode picked frees the box; so does New chat or a reopened
// chat, which set the picker silently.
//
// Backend contract (all verified end-to-end):
//   POST /api/ios/activate        -> starts forward + XCUITest on the newest
//                                    device in paired_devices.json
//   GET  /api/ios/session-status  -> connecting / connected / error
//   POST /api/ios/deactivate      -> instant kill (phone-side /wda/shutdown
//                                    first, so the overlay drops in ~1s)
//   status code "repair"          -> the runner can't launch; pair again via the
//                                    setup server (/detect, /run), then reconnect
(function () {
    'use strict';

    // ---- session core ----
    var statusTimer = null;
    function stopPoll() { if (statusTimer) { clearInterval(statusTimer); statusTimer = null; } }

    // A saved iPhone whose runner can't launch (code "repair": the free Apple ID
    // signing lasts 7 days, then iOS deletes it) gets paired again right here,
    // with the same setup-server calls as Settings' "Pair device" button.
    var repairStream = null, repairBase = null, repairGen = 0;
    function stopRepair() {
        repairGen++;                          // anything still in flight is dropped
        if (!repairStream) return;
        repairStream.close(); repairStream = null;
        fetch(repairBase + '/stop').catch(function () {});
    }
    function repair(udid, onDone) {
        var gen = repairGen;
        function done(why) { if (gen === repairGen) onDone(why); }
        fetch('/api/ios/setup-server', { method: 'POST' })
            .then(function (r) { return r.json(); })
            .then(function (srv) {
                if (!srv.url) return done(srv.error || 'could not start the pairing service');
                repairBase = srv.url;
                return fetch(srv.url + '/detect').then(function (r) { return r.json(); }).then(function (d) {
                    if (gen !== repairGen) return;    // switched away while detecting
                    var team = (d.teams || []).filter(function (t) { return t.signed_in !== false; })[0];
                    var dev = (d.devices || []).filter(function (x) {
                        return x.udid === udid && x.connected !== false;
                    })[0];
                    if (!team) return done('no Apple Account in Xcode. Add one in Settings → Connect Device');
                    if (!dev) return done('iPhone not found. Plug it in with the cable and unlock it');
                    var es = repairStream = new EventSource(srv.url + '/run?' + new URLSearchParams({
                        team: team.team_id, udid: udid, prefix: 'com.AutoCua', mode: 'test', mount: '1'
                    }).toString());
                    function end(why) { es.close(); if (repairStream === es) repairStream = null; done(why); }
                    es.addEventListener('serverup', function () {
                        fetch(srv.url + '/stop').catch(function () {});   // pairing done; end the test run
                        fetch('/api/ios/paired/add', {
                            method: 'POST',
                            headers: { 'Content-Type': 'application/json' },
                            body: JSON.stringify({ udid: udid, name: dev.name || 'iPhone', version: dev.version || '' })
                        }).catch(function () {});
                        end(null);
                    });
                    es.addEventListener('done', function () { end('pairing again did not finish. Open Settings → Connect Device'); });
                    es.addEventListener('needaccount', function () { end('the Apple Account is not signed into Xcode. Open Settings → Connect Device'); });
                    es.addEventListener('needtrust', function () { end('trust AutoCua on the iPhone (Settings → General → VPN & Device Management)'); });
                    es.addEventListener('fatal', function (e) { end(e.data || 'pairing again failed'); });
                });
            })
            .catch(function () { done('the pairing service did not answer'); });
    }

    // The backend always says WHY it could not connect (error + hint): a missing
    // pymobiledevice3, a port already serving another WDA, a device that is not
    // paired. Swallowing that and printing one fixed sentence turns a one-read
    // diagnosis into a guessing game, so carry the reason all the way out.
    function reasonOf(d) {
        if (!d) return '';
        return [d.error, d.hint].filter(Boolean).join(' — ');
    }

    // gen drops an answer that comes after the pick changed (a newer pick, or
    // a switch away), as android_session.js does: a late one must not lock,
    // free or relabel the chat box for a pick that is over.
    var gen = 0;

    function activate(cb) {
        cb = cb || {};
        stopPoll();
        var my = ++gen;
        function fail(why) {
            if (my !== gen) return;
            stopPoll();
            fetch('/api/ios/deactivate', { method: 'POST' }).catch(function () {});
            if (cb.onFail) cb.onFail(why || '');
        }
        // Wireless or Wired: the chat box's toggle (chat_input/link_toggle.js).
        // Over Wi-Fi Xcode builds and starts WebDriverAgent, so it gets longer.
        var link = window.phoneLink ? window.phoneLink() : 'wireless';
        var wait = link === 'wireless' ? 180 : 90;
        fetch('/api/ios/activate', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ link: link })   // backend picks the newest paired device it reaches
        }).then(function (r) { return r.json(); }).then(function (d) {
            // iOS left meanwhile: a session that came up anyway goes again
            if (my !== gen) { if (d.ok && !iosActive) fetch('/api/ios/deactivate', { method: 'POST' }).catch(function () {}); return; }
            if (d.state === 'connected') { if (cb.onConnected) cb.onConnected(); return; }
            if (!d.ok) { fail(reasonOf(d)); return; }
            var deadline = Date.now() + wait * 1000;
            statusTimer = setInterval(function () {
                if (Date.now() > deadline) {
                    fail('timed out after ' + wait + 's waiting for WebDriverAgent to answer');
                    return;
                }
                fetch('/api/ios/session-status').then(function (r) { return r.json(); }).then(function (s) {
                    if (my !== gen) return;
                    if (s.state === 'connected') { stopPoll(); if (cb.onConnected) cb.onConnected(); }
                    else if (s.state === 'error' && s.code === 'repair' && !cb.repaired) {
                        stopPoll();
                        fetch('/api/ios/deactivate', { method: 'POST' }).catch(function () {});
                        if (cb.onRepair) cb.onRepair();
                        repair(s.udid, function (why) {
                            if (why) return fail(why);
                            cb.repaired = true;                 // pair again at most once per click
                            activate(cb);
                        });
                    }
                    else if (s.state === 'error' || s.state === 'disconnected') { fail(reasonOf(s)); }
                }).catch(function () { /* transient */ });
            }, 2000);
        }).catch(function (e) {
            fail('the AutoCua backend did not answer (' + (e && e.message || 'network error') + ')');
        });
    }

    function deactivate() {
        gen++;                                  // an answer still on the way is dropped
        stopPoll();
        stopRepair();
        fetch('/api/ios/deactivate', { method: 'POST' }).catch(function () {});
    }

    // ---- chat placeholder animation + breathing glow ----
    function chatInput() { return document.querySelector('.chat-input'); }
    function composer() { return document.querySelector('.input-area-wrapper'); }
    var dotsTimer = null, restoreTimer = null, savedPlaceholder = null;

    // Breathing glow via the Web Animations API. The rest frame EQUALS the
    // steady iOS glow (agent_mode.css), and stopping asks the browser to end
    // the animation exactly at the current cycle's boundary — so the settle
    // into the stable glow is frame-perfect, no JS-timing snap.
    var BREATHE_MS = 1800;
    var GLOW_REST = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 14px 28px -16px rgba(191,90,242,0.32), ' +
        'inset 14px 0 28px -16px rgba(191,90,242,0.19), ' +
        'inset 0 -14px 28px -16px rgba(94,92,230,0.30), ' +
        'inset -14px 0 28px -16px rgba(94,92,230,0.19)';
    var GLOW_PEAK = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 16px 32px -12px rgba(191,90,242,0.55), ' +
        'inset 16px 0 32px -12px rgba(191,90,242,0.32), ' +
        'inset 0 -16px 32px -12px rgba(94,92,230,0.50), ' +
        'inset -16px 0 32px -12px rgba(94,92,230,0.32)';
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
        startBreathing();                            // violet glow breathes while pairing
        var el = chatInput();
        if (!el) return;
        clearTimeout(restoreTimer); restoreTimer = null;
        if (savedPlaceholder === null) savedPlaceholder = el.placeholder;
        el.classList.remove('pair-note');           // a failure note still showing goes
        el.readOnly = true;                         // no typing while pairing,
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
    // iosActive: iOS is the selection and a session was asked for from here
    // (switching away must kill it). pairing: that activate() is still in
    // flight, so a second click must not restart it half-way. failed: it did
    // not pair, and iOS is still the selection.
    var iosActive = false;
    var pairing = false;
    var failed = false;

    // EVERY iOS pick (re)activates, not only the first. The session can die
    // behind the picker's back: New chat kills it server-side, the cable can
    // drop. Re-activating a live session is free: the backend answers
    // 'connected' at once (session.py), so the box just confirms "Device
    // connected". The circle's arrow pairs again the same way.
    function pair() {
        if (pairing) return;
        iosActive = true;
        pairing = true;
        failed = false;
        connect();
    }

    // Connect the picked iPhone: the pairing look, then activate().
    function connect() {
        pairState('pairing');
        startPairingAnim();
        activate({
            onRepair: function () {
                // free Apple ID signing lasts 7 days; say why this takes a minute
                startPairingAnim('Signing expired, re-signing AutoCua (about a minute)');
            },
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
                // A placeholder holds a few words, not a paragraph: only the
                // reason's first sentence, the cause, not what to do about it
                // (the console gets it all). Naming the cause still beats
                // sending people to a Settings pane that will not mention it.
                var cause = why.split(/\. | — /)[0];
                var msg = cause
                    ? ('iOS: ' + (cause.length > 80 ? cause.slice(0, 79) + '…' : cause))
                    : 'Pairing failed — check Settings → Connect Device';
                if (why) { try { console.error('[ios] activate failed:', why); } catch (e) {} }
                failPairingAnim(msg);
            }
        });
    }

    // Switched away: instant kill, and the box is free again.
    function leave() {
        iosActive = false;
        pairing = false;
        failed = false;
        pairState('');
        deactivate();
        endPairingAnim(null);                    // stop any mid-pairing animation
    }

    document.addEventListener('agentmode:changed', function (e) {
        var d = e.detail || {};
        if (d.mode === 'mobile' && d.sub === 'ios') pair();
        else if (iosActive) leave();
    });

    // The Wireless / Wired toggle flipped (chat_input/link_toggle.js): the
    // picked iPhone connects again, the new way, also after one that did not
    // pair. Its session is stopped first and the new one asked for only then,
    // so the two never cross on the backend (android_session.js does the same).
    document.addEventListener('phonelink:changed', function () {
        if (!iosActive || pairing) return;
        pairing = true;
        failed = false;
        stopPoll();
        stopRepair();
        var my = ++gen;                          // an answer still on the way is dropped
        pairState('pairing');
        startPairingAnim();
        fetch('/api/ios/deactivate', { method: 'POST' }).catch(function () {}).then(function () {
            if (my === gen && iosActive) connect();
        });
    });

    // New chat and a reopened chat set the picker SILENTLY (chat.js): off
    // iOS, that is a switch away too. A tick later, once agent_mode.js has
    // painted the new selection on the wrap.
    document.addEventListener('agentmode:set', function () {
        setTimeout(function () {
            var wrap = document.getElementById('agentModeWrap');
            if (iosActive && !(wrap && wrap.dataset.mode === 'mobile' && wrap.dataset.sub === 'ios')) leave();
        }, 0);
    });

    // The circle's arrow, after a pairing that did not work: pair again.
    document.addEventListener('click', function (e) {
        if (failed && iosActive && e.target.closest && e.target.closest('#pairStatus')) pair();
    });

    window.iosSession = { activate: activate, deactivate: deactivate, settle: settle };
})();
