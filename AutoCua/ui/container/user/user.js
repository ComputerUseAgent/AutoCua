// =====================================================================
// User container — the user's message for this run, top right of the main
// grid like a chat (user.tsx, the live turn's first zone). Exposes
// window.showUserMessage(text): the send path (chat_input.js) shows the
// message the moment it is sent; '' hides the box again (resetChatUi, on a
// new run and on New chat).
// =====================================================================
(function () {
    'use strict';

    window.showUserMessage = function (text) {
        var zone = document.getElementById('userContainer');
        var textEl = document.getElementById('userMessageText');
        var t = String(text == null ? '' : text).trim();
        if (!zone || !textEl) return;
        textEl.textContent = t;
        textEl.scrollTop = 0;
        zone.classList.toggle('is-empty', !t);
    };
})();
