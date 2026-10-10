// Top-left container loader — injects the screenshot cell into #mainGrid
// (column 1, row 1). The Agent Notes hooks (window.show/hideAgentNotes) used to
// live here; they moved with the notes to notes_stage/ (inside
// container/big_center_container/).
(function () {
    'use strict';

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.screenshot-zone')) return;
        fetch('container/top_left/top_left.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.screenshot-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.screenshot-zone');
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
