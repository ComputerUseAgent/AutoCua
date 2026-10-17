// =====================================================================
// Run reply — the run's done message in the bottom container once it ends,
// like a chat reply under the user's message and the screen. It takes the
// place of the tool-response chain and tracking progress (agent.html)
// until the next run, and the chat glides down to its last line.
//   window.showRunReply(html, status, still) — html is server-rendered Markdown
//     (ui/markdown.py, already escaped). status 'success' shows it as the
//     reply; anything else (stopped / incomplete / error) as a muted line.
//     Called by the backend at run end, by the Stop button (chat_input.js)
//     and when a saved chat is reopened (chat/chat.js, with still: the
//     reopen places the view itself, on its last turn).
//   window.hideRunReply() — the live panels come back (resetChatUi).
// =====================================================================
(function () {
    'use strict';

    function zone() { return document.getElementById('agentContainer'); }

    window.showRunReply = function (html, status, still) {
        var z = zone();
        var el = z && z.querySelector('.reply-panel');
        if (!el) return;
        var bot = window.chainBot ? window.chainBot.toReply() : null;   // the run's bot goes beside it
        el.innerHTML = String(html == null ? '' : html);
        el.classList.toggle('is-ended', status !== 'success');
        z.classList.add('show-reply');
        // A Web use or iOS run is not recorded: once it is over its screen box
        // goes, the turn is the message and the reply (chat_scroll.js).
        var live = document.getElementById('liveTurn');
        if (live && (live.dataset.bot === 'web' || live.dataset.bot === 'mobile')) {
            if (window.screenStageOff) window.screenStageOff();
            if (window.chatScroll) window.chatScroll.setLiveScreen(false);
        }
        // The chat glides down to its last line (toEnd): by this much. A run's
        // bot still flying over to its place aims where that place will be.
        var sc = document.getElementById('chatScroll');
        var by = (!still && sc) ? Math.max(0, sc.scrollHeight - sc.clientHeight - sc.scrollTop) : 0;
        if (bot) bot(by);
        if (!still && window.chatScroll) window.chatScroll.toEnd();   // its last line in view
    };

    window.hideRunReply = function () {
        var z = zone();
        if (!z) return;
        z.classList.remove('show-reply');
        var el = z.querySelector('.reply-panel');
        if (el) el.innerHTML = '';
    };
})();
