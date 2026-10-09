// Notes stage — mounts Agent Notes into #zoneBigCenter (container/big_center_container/)
// and drives that zone's visibility. Also OWNS the Agent Notes hooks (moved here from
// container/top_left): window.showAgentNotes renders the numbered scratchpad
// into the stage and reveals it; window.hideAgentNotes hides + clears it.
//
// SHOW on:  • run end — frontend/service.py pushes window.showAgentNotes(...)
//           • stop-button click (the stop orb iframe posts 'pcbtn:clicked' to the
//             app window — passive listener; chat_input.js keeps handling the stop.
//             The stage appears immediately, the notes land when the backend pushes)
//           • saved chat reopened ('chat:opened' from chat/chat.js, whose fetch
//             then calls showAgentNotes with that chat's last done message)
// HIDE on:  • a new task sent (.chat-input gains 'agent-active' — observed
//             passively via a MutationObserver; resetChatUi also calls hideAgentNotes)
//           • New chat ('chat:new' from chat/chat.js)
(function () {
    'use strict';

    function stage() { return document.getElementById('zoneBigCenter'); }
    function show() { var z = stage(); if (z) z.classList.add('active'); }
    function hide() { var z = stage(); if (z) z.classList.remove('active'); }

    // Show the agent's scratchpad notes (numbered) on the stage. Defined up front
    // (lazy DOM lookup) so it exists before the backend calls it, even though the
    // markup is fetch-injected below. payload = JSON array of strings.
    window.showAgentNotes = function (payload) {
        var entries = payload;
        if (typeof payload === 'string') {
            try { entries = JSON.parse(payload); } catch (e) { entries = []; }
        }
        if (!Array.isArray(entries)) entries = [];
        var listEl = document.getElementById('agentNotesList');
        if (!listEl) return;

        listEl.innerHTML = '';
        if (!entries.length) {
            var empty = document.createElement('div');
            empty.className = 'agent-note-empty';
            empty.textContent = 'No notes';
            listEl.appendChild(empty);
        } else {
            entries.forEach(function (t, i) {
                var row = document.createElement('div');
                row.className = 'agent-note';
                var num = document.createElement('span');
                num.className = 'agent-note-num';
                num.textContent = (i + 1) + '.';
                var txt = document.createElement('span');
                txt.className = 'agent-note-text md';
                // HTML, not text: frontend/markdown.py already rendered the
                // note's Markdown and escaped everything the model wrote.
                txt.innerHTML = String(t);
                row.appendChild(num);
                row.appendChild(txt);
                listEl.appendChild(row);
            });
        }
        show();
    };

    // Reopen view: the chat's WHOLE life as numbered exchanges — "N. <user
    // request>" with how that run ended (done / stopped / error message)
    // indented below it. Fed by chat.js from /api/chats/<id>'s `exchanges`
    // (exchanges.json, appended per run ending).
    //
    // ALSO the live run-end fallback: when a run ends with an empty scratchpad
    // there are no notes to show, so service.py pushes this transcript instead
    // of leaving the stage on "No notes" — and because save_run() appends
    // before pushing, the run that just finished is the last row. That path
    // arrives as a JSON string (evaluate_js), chat.js passes a real array.
    window.showAgentHistory = function (payload) {
        var exchanges = payload;
        if (typeof payload === 'string') {
            try { exchanges = JSON.parse(payload); } catch (e) { exchanges = []; }
        }
        if (!Array.isArray(exchanges)) exchanges = [];
        var listEl = document.getElementById('agentNotesList');
        if (!listEl) return;

        listEl.innerHTML = '';
        if (!exchanges.length) {
            var empty = document.createElement('div');
            empty.className = 'agent-note-empty';
            empty.textContent = 'No notes';
            listEl.appendChild(empty);
        } else {
            exchanges.forEach(function (x, i) {
                x = x || {};
                var row = document.createElement('div');
                row.className = 'agent-note';
                var num = document.createElement('span');
                num.className = 'agent-note-num';
                num.textContent = (i + 1) + '.';
                var txt = document.createElement('span');
                txt.className = 'agent-note-text md';
                // *_html come pre-rendered from /api/chats/<id> (markdown.py).
                // Older payloads without them fall back to plain text.
                if (x.task_html) txt.innerHTML = String(x.task_html);
                else txt.textContent = String(x.task || '');
                row.appendChild(num);
                row.appendChild(txt);
                listEl.appendChild(row);

                var reply = document.createElement('div');
                reply.className = 'agent-note-reply md';
                if (x.done_html) reply.innerHTML = String(x.done_html);
                else reply.textContent = String(x.done_message || '');
                listEl.appendChild(reply);
            });
        }
        show();
    };

    // Hide the stage and drop its content (called on a new run start / New chat),
    // so a later stop-click can't flash the previous session's notes.
    window.hideAgentNotes = function () {
        hide();
        var listEl = document.getElementById('agentNotesList');
        if (listEl) listEl.innerHTML = '';
    };

    // --- show triggers beyond showAgentNotes itself ---
    window.addEventListener('message', function (e) {
        if (e.data === 'pcbtn:clicked') show();
    });
    document.addEventListener('chat:opened', function () { show(); });

    // --- hide triggers ---
    document.addEventListener('chat:new', function () { hide(); });

    // A new task sent: agent-active appears on .chat-input the moment the send
    // fires (chat_input.js startAgent) — the live 4-zone view takes over again.
    var runWatchWired = false;
    function watchRunState() {
        if (runWatchWired) return;
        var chatInput = document.querySelector('.chat-input');
        if (!chatInput) return;
        runWatchWired = true;

        var running = chatInput.classList.contains('agent-active');
        var observer = new MutationObserver(function () {
            var now = chatInput.classList.contains('agent-active');
            if (now === running) return;
            running = now;
            if (now) hide();   // run starting; stop-teardown (false) leaves the stage alone
        });
        observer.observe(chatInput, { attributes: true, attributeFilter: ['class'] });
    }
    // .chat-input is itself fetch-injected by chat_input.js.
    document.addEventListener('chatinput:ready', watchRunState);

    function mountNotes() {
        var zone = stage();
        if (!zone || zone.querySelector('.agent-notes')) return;
        fetch('notes_stage/notes_stage.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (!zone || zone.querySelector('.agent-notes')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var notes = holder.querySelector('.agent-notes');
                if (notes) zone.appendChild(notes);
            })
            .catch(function () { /* non-fatal */ });
        watchRunState();   // in case chat_input mounted before this script ran
    }

    // Mount once the big-center shell is in the DOM (big_center_container.js
    // fires 'bigcenter:ready'; also try immediately if it already is).
    document.addEventListener('bigcenter:ready', mountNotes);
    if (stage()) mountNotes();
})();
