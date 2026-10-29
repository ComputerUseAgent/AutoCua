// The Wireless / Wired toggle (chat_input.tsx's #linkToggle, shown in Mobile
// use): how the Android phone connects. Wireless, the default, reaches it
// over Wi-Fi only, also with its cable in; Wired over USB only. Either way
// only a phone connected once in Settings → Connect Device.
//
// android_session.js asks window.androidLink() when the Android pick
// connects, and connects again when the toggle flips ('androidlink:changed').
// The backend side is android_connector/session.py's link (confirm, activate).
// The toggle holds still while a phone pairs (the picker's wrap says
// data-pair="pairing") and while a run goes (chat_input.css fades it and takes
// its clicks). After a pairing that did not work it flips: that pairs again.
(function () {
    'use strict';

    // 'wireless' or 'wired': the toggle's .wired class.
    window.androidLink = function () {
        var toggle = document.getElementById('linkToggle');
        return toggle && toggle.classList.contains('wired') ? 'wired' : 'wireless';
    };

    function wire() {
        var toggle = document.getElementById('linkToggle');
        if (!toggle) return;
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
            document.dispatchEvent(new CustomEvent('androidlink:changed', {
                detail: { link: window.androidLink() }
            }));
        });
    }

    // The toggle is in the page from the start (chat_input.tsx); chat_input.js
    // fires this once it has wired the box.
    document.addEventListener('chatinput:ready', wire);
})();
