// =====================================================================
// Run reply — the run's done message in the bottom container once it ends,
// like a chat reply under the user's message and the screen. It takes the
// place of the tool-response chain and tracking progress (agent.html)
// until the next run.
//   window.showRunReply(html, status) — html is server-rendered Markdown
//     (ui/markdown.py, already escaped). status 'success' shows it as the
//     reply; anything else (stopped / incomplete / error) as a muted line.
//     Called by the backend at run end, by the Stop button (chat_input.js)
//     and when a saved chat is reopened (chat/chat.js).
//   window.hideRunReply() — the live panels come back (resetChatUi).
// =====================================================================
(function () {
    'use strict';

    function zone() { return document.getElementById('agentContainer'); }

    window.showRunReply = function (html, status) {
        var z = zone();
        var el = z && z.querySelector('.reply-panel');
        if (!el) return;
        el.innerHTML = String(html == null ? '' : html);
        el.classList.toggle('is-ended', status !== 'success');
        z.classList.add('show-reply');
    };

    window.hideRunReply = function () {
        var z = zone();
        if (!z) return;
        z.classList.remove('show-reply');
        var el = z.querySelector('.reply-panel');
        if (el) el.innerHTML = '';
    };
})();
