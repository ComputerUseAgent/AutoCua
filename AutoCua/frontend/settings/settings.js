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
                slot.appendChild(btn);
                wire(btn);
            })
            .catch(function () { /* non-fatal: the cog just won't render */ });
    }

    // Settings → Recording switch. The state is kept by the backend
    // (settings.json), which reads it when a run starts, so it survives a
    // restart. The section only appears on a machine that can record.
    function wireRecording() {
        var view = document.getElementById('settingsRecordingView');
        var sw = document.getElementById('recordRunsSwitch');
        if (!view || !sw) return;
        var paint = function (on) {
            sw.classList.toggle('on', on);
            sw.setAttribute('aria-checked', on ? 'true' : 'false');
        };
        fetch('/api/recording')
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (!d || !d.supported) return;
                paint(!!d.on);
                view.style.display = '';
            })
            .catch(function () { /* non-fatal: the section just stays hidden */ });
        sw.addEventListener('click', function () {
            var on = !sw.classList.contains('on');
            paint(on);
            fetch('/api/recording', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ on: on })
            }).catch(function () { paint(!on); });
        });
    }
    wireRecording();

    // Mount once the left bar exists: immediately if already injected, otherwise
    // when left_bar.js fires 'leftbar:ready'.
    var existing = document.getElementById('leftBar');
    if (existing) mount(existing);
    document.addEventListener('leftbar:ready', function (e) {
        mount((e && e.detail && e.detail.bar) || document.getElementById('leftBar'));
    });
})();
