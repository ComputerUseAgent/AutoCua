// =====================================================================
// User container — the user's message for this run, top right of the main
// grid like a chat. Mounts user.html into #liveTurn (row 1) and exposes
// window.showUserMessage(text): the send path (chat_input.js) shows the
// message the moment it is sent; '' hides the box again (resetChatUi, on a
// new run and on New chat).
// =====================================================================
(function () {
    'use strict';

    var pending = null;   // a message shown before the box was mounted

    window.showUserMessage = function (text) {
        var zone = document.getElementById('userContainer');
        var textEl = document.getElementById('userMessageText');
        var t = String(text == null ? '' : text).trim();
        if (!zone || !textEl) { pending = t; return; }
        textEl.textContent = t;
        textEl.scrollTop = 0;
        zone.classList.toggle('is-empty', !t);
    };

    function mount() {
        var grid = document.getElementById('liveTurn');
        if (!grid || grid.querySelector('.user-container')) return;
        fetch('container/user/user.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.user-container')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.user-container');
                if (!zone) return;
                grid.appendChild(zone);
                if (pending) window.showUserMessage(pending);
                pending = null;
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
