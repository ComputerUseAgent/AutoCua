// =====================================================================
// User container — the user's message for this run, top right of the main
// grid like a chat (user.tsx, the live turn's first zone). Exposes
// window.showUserMessage(text, via): the send path (chat_input.js) shows the
// message the moment it is sent; '' hides the box again (resetChatUi, on a
// new run and on New chat). via: where the message came from when it was
// not typed here ("telegram", a Telegram task): a small pill over the
// message, "sent via: Telegram"; none for the chat box's own.
// =====================================================================
(function () {
    'use strict';

    window.showUserMessage = function (text, via) {
        var zone = document.getElementById('userContainer');
        var textEl = document.getElementById('userMessageText');
        var viaEl = document.getElementById('userMessageVia');
        var t = String(text == null ? '' : text).trim();
        var v = String(via == null ? '' : via).trim();
        if (!zone || !textEl) return;
        textEl.textContent = t;
        textEl.scrollTop = 0;
        if (viaEl) {
            viaEl.textContent = v ? 'sent via: ' + v.charAt(0).toUpperCase() + v.slice(1) : '';
            viaEl.hidden = !v;
        }
        zone.classList.toggle('is-empty', !t);
    };
})();
