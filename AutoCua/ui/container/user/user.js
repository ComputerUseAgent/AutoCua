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
        attach(zone);
        setOpen(zone, false);                   // a new message starts cut, however long
    };

    // ---- a long message: the chevron that opens it ----
    // A message longer than the box's 4 lines (user.css) is cut there and
    // scrolls; a round chevron at the box's bottom right opens it to its full
    // length and turns over to close it again. Every user box gets it: the
    // live one (above) and the chat's earlier turns (chat_scroll.js place()).
    // It shows only while the text is longer than the box: a ResizeObserver
    // checks again whenever the text's box changes (laid out, a new width).
    var CHEVRON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" ' +
        'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="6 9 12 15 18 9"/></svg>';
    var ro = ('ResizeObserver' in window) ? new ResizeObserver(function (entries) {
        entries.forEach(function (e) { fit(e.target.closest('.user-container')); });
    }) : null;

    function fit(box) {
        if (!box) return;
        var text = box.querySelector('.user-message-text'), btn = box.querySelector('.user-expand');
        if (!text || !btn) return;
        var long = box.classList.contains('expanded') || text.scrollHeight > text.clientHeight + 1;
        btn.hidden = !long;
        box.classList.toggle('has-expand', long);
    }

    function attach(box) {
        if (!box) return;
        if (!box.querySelector('.user-expand')) {
            var btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'user-expand';
            btn.hidden = true;
            btn.setAttribute('aria-label', 'Show the whole message');
            btn.setAttribute('aria-expanded', 'false');
            btn.innerHTML = CHEVRON;
            box.appendChild(btn);
            var text = box.querySelector('.user-message-text');
            if (ro && text) ro.observe(text);
        }
        fit(box);
    }

    function setOpen(box, open) {
        box.classList.toggle('expanded', open);
        var btn = box.querySelector('.user-expand'), text = box.querySelector('.user-message-text');
        if (btn) {
            btn.setAttribute('aria-expanded', open ? 'true' : 'false');
            btn.setAttribute('aria-label', open ? 'Show less' : 'Show the whole message');
        }
        if (!open && text) text.scrollTop = 0;  // closed again: from its first line
        fit(box);
    }

    document.addEventListener('click', function (e) {
        var btn = e.target.closest && e.target.closest('.user-expand');
        var box = btn && btn.closest('.user-container');
        if (box) setOpen(box, !box.classList.contains('expanded'));
    });

    window.userExpand = { attach: attach };
})();
