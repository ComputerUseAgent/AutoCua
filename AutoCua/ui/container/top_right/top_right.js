// =====================================================================
// Top-right container — EMPTY for now, kept for the user's message. Mounts
// top_right.html (the empty zone with its thin grey line) into #mainGrid
// (column 2, row 1). Its tracking-progress stream moved to the bottom
// container (container/bottom_left/tracking_progress.js).
// =====================================================================
(function () {
    'use strict';

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.top-right-zone')) return;
        fetch('container/top_right/top_right.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.top-right-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.top-right-zone');
                if (zone) grid.appendChild(zone);
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
