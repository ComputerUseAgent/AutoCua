// Big center container loader — injects the full-grid overlay zone into
// #mainGrid. Empty shell; notes_stage/ mounts Agent Notes into #zoneBigCenter.
// Fires 'bigcenter:ready' once the zone is in the DOM.
(function () {
    'use strict';

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid) return;
        if (document.getElementById('zoneBigCenter')) {
            document.dispatchEvent(new CustomEvent('bigcenter:ready'));
            return;
        }
        fetch('container/big_center_container/big_center_container.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('zoneBigCenter')) {
                    document.dispatchEvent(new CustomEvent('bigcenter:ready'));
                    return;
                }
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.big-center-zone');
                if (zone) {
                    grid.appendChild(zone);
                    document.dispatchEvent(new CustomEvent('bigcenter:ready'));
                }
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
