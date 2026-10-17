// Settings button component — injects the gear button into the left bar footer
// and emits an 'open-settings' event on click. script.js owns the settings
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

    function mount(bar) {
        if (!bar) return;
        var slot = bar.querySelector('#leftBarFooter') || bar.querySelector('.left-bar-footer');
        if (!slot || slot.querySelector('.settings-btn')) return; // already mounted
        fetch('settings/settings.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (slot.querySelector('.settings-btn')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var btn = holder.querySelector('.settings-btn');
                if (!btn) return;
                // First in the footer: the cog stays bottom-left, the GitHub
                // star pill (left_bar.html) sits to its right.
                slot.insertBefore(btn, slot.firstChild);
                wire(btn);
            })
            .catch(function () { /* non-fatal: the cog just won't render */ });
    }

    // Mount once the left bar exists: immediately if already injected, otherwise
    // when left_bar.js fires 'leftbar:ready'.
    var existing = document.getElementById('leftBar');
    if (existing) mount(existing);
    document.addEventListener('leftbar:ready', function (e) {
        mount((e && e.detail && e.detail.bar) || document.getElementById('leftBar'));
    });
})();
