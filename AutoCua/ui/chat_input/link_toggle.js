// The Wireless / Wired toggle (chat_input.tsx's #linkToggle, shown in Mobile
// use and Computer use): how the phone connects, Android or iPhone. Wired,
// the default, is USB only; Wireless, picked on purpose, reaches it over
// Wi-Fi, also with its cable in. Either way only a phone connected once in
// Settings → Connect Device, and nothing else in the app reaches a phone
// over Wi-Fi.
//
// Mobile use: android_session.js and ios_session.js ask window.phoneLink()
// when their pick connects, and connect again when the toggle flips
// ('phonelink:changed'). The backend side is the link of
// android_connector/session.py (confirm, activate) and
// ios_connector/session.py (activate: Xcode over Wi-Fi). The toggle holds
// still while a phone pairs (the picker's wrap says data-pair="pairing") and
// while a run goes (chat_input.css fades it and takes its clicks). After a
// pairing that did not work it flips: that pairs again.
//
// Computer use: the phone sub-agents (android_agent, ios_agent) look for the
// phone at the start of every request, over the Wi-Fi only with Wireless and
// the cable only with Wired. Only in Daisy chain: This PC looks for no
// phone, so the toggle hides there (chat_input.css). Every flip, and the page's load, tells the app
// (POST /api/phone-link), and the flip stays live while a run goes: it counts
// from the next request on.
(function () {
    'use strict';

    // 'wireless' or 'wired': the toggle's .wired class.
    window.phoneLink = function () {
        var toggle = document.getElementById('linkToggle');
        return toggle && toggle.classList.contains('wired') ? 'wired' : 'wireless';
    };

    // The app's side of the toggle, for the phone sub-agents (ui/service.py
    // api_phone_link). Best effort: a flip the app did not hear of is sent
    // again by the next one or the next load.
    function tell() {
        fetch('/api/phone-link', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ link: window.phoneLink() })
        }).catch(function () { /* non-fatal */ });
    }

    function wire() {
        var toggle = document.getElementById('linkToggle');
        if (!toggle) return;
        tell();                                  // the page starts on Wired: so does the app
        // Fast / Quality's behaviour: state in the class, the floating label
        // briefly after each flip.
        var labelTimer = null;
        toggle.addEventListener('click', function () {
            var wrap = document.getElementById('agentModeWrap');
            if (wrap && wrap.dataset.pair === 'pairing') return;   // a phone is pairing
            toggle.classList.toggle('wired');
            toggle.setAttribute('aria-checked', toggle.classList.contains('wired') ? 'true' : 'false');
            toggle.classList.add('show-label');
            clearTimeout(labelTimer);
            labelTimer = setTimeout(function () { toggle.classList.remove('show-label'); }, 1200);
            tell();
            document.dispatchEvent(new CustomEvent('phonelink:changed', {
                detail: { link: window.phoneLink() }
            }));
        });
    }

    // The toggle is in the page from the start (chat_input.tsx); chat_input.js
    // fires this once it has wired the box.
    document.addEventListener('chatinput:ready', wire);
})();
