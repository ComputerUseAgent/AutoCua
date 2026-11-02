// Daisy chain session connector: Computer use → Daisy chain pairs the phones
// its sub-agents use (ios_agent on a Mac, android_agent everywhere) the moment
// it is picked, before any request, over Wi-Fi or the cable as the chat box's
// Wireless / Wired toggle says (chat_input/link_toggle.js). It makes the same
// backend calls as the Mobile use picks (ios_session.js, android_session.js).
//
// The iPhone is held: it is the app's own session, which a request's
// ios_agent PhoneLink then shares at once (no WebDriverAgent start) and hands
// back when the request ends. The Android phone is only checked (connected,
// one screen read, let go): a request's own check then starts its
// android_agent child ahead (the standby child), which a helper held here
// would stop (android_agent.py _start_standby). The check at the start of
// every request stays the backup, so a phone plugged in later still comes
// online during a run; a terminal run has only that check.
//
// While pairing, the chat box is read only (nothing is sent), its placeholder
// animates "Pairing mobile over Wi-Fi." → ".." → "..." (or "over cable"), the
// Daisy chain lavender glow breathes and the circle beside the picker turns.
// Once every phone has answered, the box types again and the placeholder
// lists each one ("iPhone online · Android offline") until a run starts. An
// offline phone holds nothing up: the run goes on without it.
//
// One pairing at a time: a second Daisy chain pick while one is on the way is
// answered by it (the toggle holds still while pairing, link_toggle.js), and
// a flip during a run waits for the run to end. Leaving Daisy chain lets the
// phones go, except the one a Mobile use pick takes over (that pick connects
// it again at once: a live session answers 'connected'), and during a run (a
// chat opened from the list) only once it is over, as a sub-agent may be
// using the phone. New chat goes back to This PC (chat.js), so it lets them
// go too; a chat that brings Daisy chain back (the picker remembers it)
// pairs.
//
// Backend contract (the Mobile use picks' own):
//   POST /api/ios/activate {link}     -> {ok, state: "connecting" | "connected"}
//   GET  /api/ios/session-status      -> connecting / connected / error
//   POST /api/android/activate {link} -> {ok, state: "connected"} or {ok: false}
//   POST /api/ios/deactivate, /api/android/deactivate -> lets the phone go
//
// Loaded after android_session.js and BEFORE ios_session.js (index.html): its
// menu listener puts the placeholder back before an iOS pick saves it, and
// its own pairing starts a tick later, after a Mobile use pick it leaves has
// put the placeholder back.
(function () {
    'use strict';

    // iOS is the Mac's; the Android phone is everywhere.
    var PHONES = document.documentElement.dataset.host === 'mac' ? ['ios', 'android'] : ['android'];
    var LABEL = { ios: 'iPhone', android: 'Android' };

    function link() { return window.phoneLink ? window.phoneLink() : 'wired'; }
    // Each phone's last stop: a connect waits for it, so a stop on its way
    // never lands on the session that connect brings up.
    var stopping = {};
    function stop(p) {
        stopping[p] = fetch('/api/' + p + '/deactivate', { method: 'POST' }).catch(function () {});
        return stopping[p];
    }
    function wrap() { return document.getElementById('agentModeWrap'); }
    function picked(mode, sub) {
        var w = wrap();
        return !!w && w.dataset.mode === mode && w.dataset.sub === sub;
    }
    function running() {
        var el = chatInput();
        return !!el && el.classList.contains('agent-active');
    }
    // The backend says WHY a phone did not connect: the console gets it.
    function reasonOf(d) {
        if (!d) return '';
        return [d.error, d.hint].filter(Boolean).join('. ');
    }

    // ---- one phone: done(true) online, done(false, why, started) offline ----
    // isCurrent() false: the pairing it belonged to is over, so its answer is
    // dropped, and a phone that came up after Daisy chain was left goes
    // again, unless a Mobile use pick has it now.
    // started: this pairing's activate brought a session up that then did not
    // answer, so it is stopped. A refusal before that (locked, not on the
    // Wi-Fi, not plugged in) touched nothing, so a live session stays.
    function connectIOS(isCurrent, done) {
        var how = link();
        var wait = how === 'wireless' ? 180 : 90;   // over Wi-Fi Xcode builds and starts WebDriverAgent
        fetch('/api/ios/activate', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ link: how })      // the backend picks the newest paired iPhone it reaches
        }).then(function (r) { return r.json(); }).then(function (d) {
            if (!isCurrent()) { if (d.ok && !active && !picked('mobile', 'ios')) stop('ios'); return; }
            if (d.state === 'connected') return done(true);
            if (!d.ok) return done(false, reasonOf(d), false);
            var deadline = Date.now() + wait * 1000;
            (function poll() {
                setTimeout(function () {
                    if (!isCurrent()) return;
                    if (Date.now() > deadline) return done(false, 'timed out after ' + wait + 's', true);
                    fetch('/api/ios/session-status').then(function (r) { return r.json(); }).then(function (s) {
                        if (!isCurrent()) return;
                        if (s.state === 'connected') done(true);
                        else if (s.state === 'error' || s.state === 'disconnected') done(false, reasonOf(s), true);
                        else poll();
                    }).catch(poll);                  // transient: ask again
                }, 2000);
            })();
        }).catch(function (e) {
            if (isCurrent()) done(false, 'the AutoCua backend did not answer (' + ((e && e.message) || 'network error') + ')', false);
        });
    }

    // Checked, then let go at once (see the top): unless a Mobile use pick
    // has the phone now, which keeps its own helper.
    function connectAndroid(isCurrent, done) {
        fetch('/api/android/activate', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ link: link() })
        }).then(function (r) { return r.json(); }).then(function (d) {
            if (d.ok && !picked('mobile', 'android')) stop('android');
            if (isCurrent()) done(!!(d.ok && d.state === 'connected'), reasonOf(d));
        }).catch(function (e) {
            if (isCurrent()) done(false, 'the AutoCua backend did not answer (' + ((e && e.message) || 'network error') + ')');
        });
    }

    // ---- chat placeholder animation + breathing glow (as android_session.js) ----
    function chatInput() { return document.querySelector('.chat-input'); }
    function composer() { return document.querySelector('.input-area-wrapper'); }
    // savedPlaceholder: the box's own text, put back when the status line
    // goes. ownLine: the last line written here; a box showing anything else
    // has a newer text of its own (a model or key set meanwhile), kept then.
    var dotsTimer = null, savedPlaceholder = null, ownLine = null;
    function write(el, text) { el.placeholder = text; ownLine = text; }

    // The rest frame EQUALS the steady Daisy chain glow (agent_mode.css), so
    // the settle into it is seamless.
    var BREATHE_MS = 1800;
    var GLOW_REST = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 14px 28px -16px rgba(184,167,240,0.50), ' +
        'inset 14px 0 28px -16px rgba(184,167,240,0.30), ' +
        'inset 0 -14px 28px -16px rgba(143,123,201,0.40), ' +
        'inset -14px 0 28px -16px rgba(143,123,201,0.28)';
    var GLOW_PEAK = '0 2px 10px rgba(0,0,0,0.05), ' +
        'inset 0 16px 32px -12px rgba(184,167,240,0.78), ' +
        'inset 16px 0 32px -12px rgba(184,167,240,0.48), ' +
        'inset 0 -16px 32px -12px rgba(143,123,201,0.64), ' +
        'inset -16px 0 32px -12px rgba(143,123,201,0.44)';
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
        try {
            var cycles = Math.max(1, Math.ceil((anim.currentTime || 1) / BREATHE_MS));
            anim.effect.updateTiming({ iterations: cycles });
            anim.onfinish = function () { anim.cancel(); };
        } catch (e) {
            anim.cancel();
        }
    }

    function startPairingAnim() {
        startBreathing();
        var el = chatInput();
        if (!el) return;
        if (savedPlaceholder === null || el.placeholder !== ownLine) savedPlaceholder = el.placeholder;
        el.classList.remove('pair-note');           // a status line still showing goes
        el.readOnly = true;                         // nothing typed or sent while pairing,
        el.blur();                                  // and no focus, as a disabled box
        clearInterval(dotsTimer);
        // Says the way the toggle picked: "Pairing mobile over Wi-Fi..."
        var label = 'Pairing mobile over ' + (link() === 'wireless' ? 'Wi-Fi' : 'cable');
        var n = 1;
        if (!running()) write(el, label + '.');
        dotsTimer = setInterval(function () {
            n = (n % 3) + 1;                     // . .. ... . .. ...
            var e2 = chatInput();
            // A run's own text stands while it goes (agent_mode.js)
            if (e2 && !running()) write(e2, label + '...'.slice(0, n));
        }, 400);
    }
    // status: the line that lists each phone, kept (smaller, on one line, as
    // the Mobile use picks' notes) until a run starts; null puts the box's
    // own text back at once. During a run nothing is written: agent_mode.js
    // puts the box's text back after it (settle() handed it the real one).
    function endPairingAnim(status) {
        stopBreathing(!!status);
        clearInterval(dotsTimer); dotsTimer = null;
        var el = chatInput();
        if (!el) { savedPlaceholder = ownLine = null; return; }
        el.readOnly = false;                        // the box types again
        if (running()) { el.classList.remove('pair-note'); savedPlaceholder = ownLine = null; return; }
        if (status) {
            el.classList.add('pair-note');
            write(el, status);
        } else {
            restorePlaceholder();
        }
    }
    function restorePlaceholder() {
        var el = chatInput();
        if (el) {
            el.classList.remove('pair-note');
            if (savedPlaceholder !== null && el.placeholder === ownLine) el.placeholder = savedPlaceholder;
        }
        savedPlaceholder = ownLine = null;
    }
    // The circle beside the picker turns ('pairing') or goes (''):
    // agent_mode.css reads the wrap's data-pair.
    function pairState(s) {
        var w = wrap();
        if (w) w.dataset.pair = s;
    }

    // ---- the pairing ----
    // active: Daisy chain is the pick and its phones were asked for from here
    // (leaving it lets them go). gen: the pairing on the way; leaving or a
    // newer one drops its answers. afterRun: a flip came during a run.
    var active = false;
    var gen = 0;
    var pairing = false;
    var afterRun = false;

    // stopFirst: the phones to let go before connecting, so a stop already on
    // its way (a Mobile use pick just left, a flipped toggle's old way) never
    // lands on the new session. Any other phone still waits for its own last
    // stop from here.
    function pair(stopFirst) {
        watchRuns();
        active = true;
        if (pairing) return;                     // the pairing on its way answers this pick too
        pairing = true;
        var my = ++gen;
        function isCurrent() { return my === gen && active; }
        var state = {};
        // A tick later: a Mobile use pick being left puts the placeholder
        // back first, so the one saved here is the real one.
        setTimeout(function () {
            if (!isCurrent()) return;
            pairState('pairing');
            startPairingAnim();
            PHONES.forEach(function (p) {
                function go() {
                    if (!isCurrent()) return;
                    (p === 'ios' ? connectIOS : connectAndroid)(isCurrent, function (ok, why, started) {
                        if (!isCurrent()) return;
                        if (!ok) {
                            if (why) { try { console.error('[daisy] ' + LABEL[p] + ' offline:', why); } catch (e) {} }
                            if (p === 'ios' && started) stop('ios');   // nothing half started stays up
                        }
                        state[p] = ok;
                        if (PHONES.every(function (q) { return q in state; })) finish(state);
                    });
                }
                var before = stopFirst && stopFirst.indexOf(p) >= 0 ? stop(p) : (stopping[p] || Promise.resolve());
                before.then(go);
            });
        }, 0);
    }

    function finish(state) {
        pairing = false;
        pairState('');
        endPairingAnim(PHONES.map(function (p) {
            return LABEL[p] + (state[p] ? ' online' : ' offline');
        }).join(' · '));
    }

    // Daisy chain left: its phones go, except the one the new pick (Mobile use
    // → iOS / Android) takes over, and the box is free again.
    function leave(mode, sub) {
        if (!active) return;
        active = false;
        pairing = false;
        afterRun = false;
        gen++;
        pairState('');
        PHONES.forEach(function (p) {
            if (!(mode === 'mobile' && sub === p)) stop(p);
        });
        endPairingAnim(null);
    }

    // A run is over: what had to wait for it happens now, as the picker
    // stands: Daisy chain left, a flip, or Daisy chain shown but not paired
    // (a Telegram task's set brought it back).
    var watching = false;
    function watchRuns() {
        var el = chatInput();
        if (watching || !el) return;
        watching = true;
        var was = el.classList.contains('agent-active');
        new MutationObserver(function () {
            var now = el.classList.contains('agent-active');
            if (now === was) return;
            was = now;
            if (now) return;
            var w = wrap(), daisy = picked('computer', 'daisy');
            if (active && !daisy) { leave(w && w.dataset.mode, w && w.dataset.sub); return; }
            if (daisy && (!active || afterRun)) {
                var flipped = afterRun;
                afterRun = false;
                pair(flipped ? PHONES.slice() : null);   // a flip: the old way goes first
            }
        }).observe(el, { attributes: true, attributeFilter: ['class'] });
    }

    // ---- wiring to the Agent-mode menu ----
    var last = { mode: 'computer', sub: 'thispc' };   // the pick before this one
    function fromMobile(prev) { return prev.mode === 'mobile' && PHONES.indexOf(prev.sub) >= 0 ? [prev.sub] : null; }

    document.addEventListener('agentmode:changed', function (e) {
        var d = e.detail || {};
        var prev = last;
        last = { mode: d.mode, sub: d.sub };
        // Every Daisy chain pick pairs (a live phone answers at once). A
        // Mobile use pick being left stops its phone: stop it here too and
        // connect only once that answered.
        if (d.mode === 'computer' && d.sub === 'daisy') pair(fromMobile(prev));
        else leave(d.mode, d.sub);
    });

    // New chat and a reopened chat set the picker SILENTLY (chat.js): New
    // chat to This PC, a phone's chat to Mobile use, a Computer use chat to
    // the sub the picker remembers. Off Daisy chain, that is a switch away,
    // once a run going on is over; onto it, the phones pair. A tick later,
    // once agent_mode.js has painted the new selection.
    document.addEventListener('agentmode:set', function () {
        setTimeout(function () {
            watchRuns();
            var w = wrap();
            var mode = w && w.dataset.mode, sub = w && w.dataset.sub;
            var prev = last;
            last = { mode: mode, sub: sub };
            if (mode === 'computer' && sub === 'daisy') {
                if (!active && !running()) pair(fromMobile(prev));
                return;
            }
            if (!active || running()) return;    // during a run: the run's end leaves (watchRuns)
            leave(mode, sub);
        }, 0);
    });

    // The Wireless / Wired toggle flipped (link_toggle.js): pair again the new
    // way, the old way let go first. During a run, once it is over.
    document.addEventListener('phonelink:changed', function () {
        if (!active) return;
        if (running()) { afterRun = true; return; }
        pair(PHONES.slice());
    });

    // A run starts: the box's own text comes back now, so agent_mode.js keeps
    // that one to show after the run, not the status line or a "Pairing"
    // frame (chat_input.js calls this).
    function settle() {
        var el = chatInput();
        if (!el || savedPlaceholder === null) return;
        if (el.placeholder === ownLine) el.placeholder = savedPlaceholder;
        el.classList.remove('pair-note');
        if (dotsTimer === null) savedPlaceholder = ownLine = null;
    }

    // A phone Daisy chain is using right now (the iPhone it holds, or the
    // Android phone while it is checked): a Mobile use pick's late answer
    // must not stop it (ios_session.js, android_session.js).
    function holds(p) {
        return active && PHONES.indexOf(p) >= 0 && (p === 'ios' || pairing);
    }

    window.daisySession = { settle: settle, holds: holds };
})();
