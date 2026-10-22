// Settings button component — wires the gear button (settings.tsx, in the
// left bar footer) to emit an 'open-settings' event on click. script.js owns the settings
// overlay (and its data loaders) and listens for that event. Kept self-contained
// in its own folder for maintainability.
(function () {
    'use strict';

    function wire(btn) {
        var open = function () { document.dispatchEvent(new CustomEvent('open-settings')); };
        btn.addEventListener('click', open);
        // Keyboard support (the trigger is a role="button" div).
        btn.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); open(); }
        });
    }

    var wired = false;
    function mount(bar) {
        if (!bar || wired) return;
        var btn = bar.querySelector('.settings-btn');
        if (!btn) return;
        wired = true;
        wire(btn);
    }

    // The cog is in the bar from the start; wire it when left_bar.js says the
    // bar is ready.
    document.addEventListener('leftbar:ready', function (e) {
        mount((e && e.detail && e.detail.bar) || document.getElementById('leftBar'));
    });
})();
