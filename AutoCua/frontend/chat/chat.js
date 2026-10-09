// New Chat button + chat-history list — injected at the TOP of the left bar
// body (below the logo/cog header), once the left bar exists. Owns the UI for
// permanent chat memory:
//   • New chat  -> drop the active session (window.currentSessionId = null),
//     reset the screen, ready for a fresh start.
//   • History   -> GET /api/chats; click a row to reopen that session (adopts
//     its id so the next send CONTINUES it, and shows its last "done" message
//     on the full-grid notes stage via window.showAgentNotes).
//   • Delete    -> DELETE /api/chats/<id>.
// All persistence lives in the backend (AutoCua/agent_conversation); this file
// is pure UI. Same self-contained fetch-inject pattern as settings/settings.js.
(function () {
    'use strict';

    function escapeAttr(s) { return String(s == null ? '' : s); }

    // Fetch the session list and (re)render the history rows.
    function loadChats(list) {
        if (!list) return;
        fetch('/api/chats')
            .then(function (r) { return r.json(); })
            .then(function (rows) { renderChats(list, rows || []); })
            .catch(function () { /* non-fatal: list just stays as-is */ });
    }

    // Per-mode logos for the sidebar rows — the SAME marks the composer's
    // agent-mode picker uses (agent_mode.html), so a chat's row tells you at
    // a glance which agent owns it. Static path constants, no user content.
    var MODE_ICON_PATHS = {
        computer: '<rect x="3" y="4" width="18" height="12" rx="1"/><path d="M7 20h10"/><path d="M9 16v4"/><path d="M15 16v4"/>',
        mobile: '<rect x="5.5" y="2" width="13" height="20" rx="2.5"/><path d="M10.5 18h3"/>',
        shell: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 8h18"/><path d="M7 12l2.5 2.5L7 17"/><path d="M13 17h4"/>',
        web: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><ellipse cx="12" cy="12" rx="4" ry="9"/>'
    };

    function chatModeIcon(row) {
        // agent_mode is stamped at chat mint (shell-conversation work); legacy
        // rows fall back to run_pkg (ios -> mobile, anything else ->
        // computer, same inference the reopen path uses).
        var mode = row.agent_mode;
        if (!MODE_ICON_PATHS[mode]) {
            mode = (row.run_pkg === 'ios') ? 'mobile'
                 : (row.run_pkg === 'web') ? 'web' : 'computer';
        }
        var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
        svg.setAttribute('class', 'chat-mode-icon');
        svg.setAttribute('viewBox', '0 0 24 24');
        svg.setAttribute('fill', 'none');
        svg.setAttribute('stroke', 'currentColor');
        svg.setAttribute('stroke-width', '2');
        svg.setAttribute('stroke-linecap', 'round');
        svg.setAttribute('stroke-linejoin', 'round');
        svg.setAttribute('aria-hidden', 'true');
        svg.innerHTML = MODE_ICON_PATHS[mode];
        return svg;
    }

    function renderChats(list, rows) {
        list.innerHTML = '';
        rows.forEach(function (row) {
            var item = document.createElement('div');
            item.className = 'chat-history-item';
            if (row.id === window.currentSessionId) item.classList.add('active');
            item.dataset.id = row.id;
            item.title = escapeAttr(row.name);

            item.appendChild(chatModeIcon(row));   // which agent owns this chat

            var label = document.createElement('span');
            label.className = 'chat-history-name';
            label.textContent = row.name || 'New chat';
            item.appendChild(label);

            var more = document.createElement('button');
            more.type = 'button';
            more.className = 'chat-history-more';
            more.setAttribute('aria-label', 'Chat options');
            more.textContent = '⋯';   // ⋯ horizontal ellipsis
            more.addEventListener('click', function (e) {
                e.stopPropagation();       // open the menu, not the chat
                openItemMenu(row.id, more, list);
            });
            item.appendChild(more);

            item.addEventListener('click', function () { openChat(row.id, list); });
            list.appendChild(item);
        });
    }

    // Reopen a saved session: adopt its id (so the next send continues it) and
    // show ONLY its last done message in the top-left container (the reopen view).
    function openChat(id, list) {
        window.currentSessionId = id;
        Array.prototype.forEach.call(list.children, function (el) {
            el.classList.toggle('active', el.dataset.id === id);
        });
        if (window.resetChatUi) window.resetChatUi();   // clear leftover live view
        if (window.hideWelcomeHero) window.hideWelcomeHero();   // reopening != empty state
        // Announce the reopen (the notes stage shows itself on this).
        document.dispatchEvent(new CustomEvent('chat:opened', { detail: { id: id } }));
        fetch('/api/chats/' + encodeURIComponent(id))
            .then(function (r) { return r.json(); })
            .then(function (data) {
                // Stale response: the user moved on (New chat or another row)
                // while this fetch was in flight — don't repaint their new view.
                if (window.currentSessionId !== id) return;
                if (!data || data.error) return;
                // Full life of the chat: numbered request/outcome pairs
                // (exchanges.json). Legacy sessions saved before exchanges
                // existed fall back to the old single last-done-message note.
                var exchanges = Array.isArray(data.exchanges) ? data.exchanges : [];
                // A SHELL chat renders its history in the CLI stage's own notes
                // row under the terminal — the notes-stage overlay sits BEHIND
                // the pinned panel (z 5 vs 6), so it would be invisible there.
                var isShell = (data.agent_mode === 'shell');   // single read point of the backend marker
                if (isShell) {
                    if (window.cliShellHistory) window.cliShellHistory(exchanges);
                    // drop the hidden stage content 'chat:opened' just revealed
                    if (window.hideAgentNotes) window.hideAgentNotes();
                } else if (exchanges.length && window.showAgentHistory) {
                    window.showAgentHistory(exchanges);
                } else if (window.showAgentNotes) {
                    // showAgentNotes renders as HTML, so hand it the server-rendered
                    // Markdown (markdown.py) — never the raw text. A single-element
                    // array shows the last done message as note "1.".
                    var msg = data.last_done_message_html || '';
                    window.showAgentNotes(JSON.stringify(msg ? [msg] : []));
                }
                // Restore the memory bar to this chat's last context size + cap and
                // show it (the bar hides only for a brand-new chat).
                if (window.updateMemoryBar) window.updateMemoryBar(data.context_tokens || 0, data.context_cap || 300000);
                if (window.showMemoryBar) window.showMemoryBar();
                // Per-chat mode lock: the chat follows the agent that ran it.
                // shell marker -> Shell use (the terminal re-pins via the silent
                // set below); ios -> Mobile use with the device tick CLEARED
                // (the phone session is gone — the user re-picks iOS/Android to
                // re-pair); any desktop pkg -> Computer use; untagged -> free.
                var pkg = data.run_pkg || '';
                var lockMode = isShell ? 'shell'
                    : (pkg === 'ios' ? 'mobile'
                    : (pkg === 'web' ? 'web'
                    : (pkg ? 'computer' : null)));
                document.dispatchEvent(new CustomEvent('agentmode:lock', { detail: { mode: lockMode } }));
                if (lockMode) {
                    document.dispatchEvent(new CustomEvent('agentmode:set', {
                        detail: lockMode === 'mobile' ? { mode: 'mobile', sub: null } : { mode: lockMode }
                    }));
                } else if (document.body.classList.contains('cli-shell')) {
                    // Untagged chat opened while the shell terminal is pinned —
                    // its notes render in the stage BEHIND the panel and would be
                    // invisible. Hand the view back to the default mode (the CLI
                    // stage follows the silent set and slides the terminal away).
                    document.dispatchEvent(new CustomEvent('agentmode:set', { detail: { mode: 'computer' } }));
                }
            })
            .catch(function () { /* non-fatal */ });
    }

    function deleteChat(id, list) {
        fetch('/api/chats/' + encodeURIComponent(id), { method: 'DELETE' })
            .then(function (r) { return r.json(); })
            .then(function () {
                // If we deleted the open chat, fall back to the SAME fresh-start
                // state as the New-chat button — resetChatUi alone left the memory
                // bar filled, the hero hidden, and the mode picker locked to a
                // chat that no longer exists.
                if (window.currentSessionId === id) {
                    startNewChat();
                }
                loadChats(list);
            })
            .catch(function () { /* non-fatal */ });
    }

    // Download a session's saved conversation (the agent's exact optimized
    // memory) to the user's Downloads folder — a debug aid to inspect what's
    // actually stored. The backend writes the file; we toast the saved path.
    function downloadChat(id) {
        fetch('/api/chats/' + encodeURIComponent(id) + '/download', { method: 'POST' })
            .then(function (r) { return r.json(); })
            .then(function (data) {
                if (data && data.path) {
                    if (window.showToast) window.showToast('Saved in Downloads');
                } else if (window.showToast) {
                    window.showToast((data && data.error) || 'Nothing saved for this chat yet');
                }
            })
            .catch(function () { if (window.showToast) window.showToast('Download failed'); });
    }

    // Per-chat "⋯" menu (Download / Delete). Appended to <body> and fixed-
    // positioned next to the kebab so the left bar's overflow can't clip it.
    var openMenuEl = null;
    function closeItemMenu() {
        if (!openMenuEl) return;
        openMenuEl.remove();
        openMenuEl = null;
        document.removeEventListener('click', closeItemMenu, true);
        document.removeEventListener('keydown', onMenuKey, true);
    }
    function onMenuKey(e) { if (e.key === 'Escape') closeItemMenu(); }
    function openItemMenu(id, anchor, list) {
        closeItemMenu();
        var menu = document.createElement('div');
        menu.className = 'chat-item-menu';

        var dl = document.createElement('button');
        dl.type = 'button';
        dl.className = 'chat-item-menu-action';
        dl.textContent = 'Download conversation';
        dl.addEventListener('click', function (e) {
            e.stopPropagation(); closeItemMenu(); downloadChat(id);
        });

        var del = document.createElement('button');
        del.type = 'button';
        del.className = 'chat-item-menu-action danger';
        del.textContent = 'Delete';
        del.addEventListener('click', function (e) {
            e.stopPropagation(); closeItemMenu(); deleteChat(id, list);
        });

        menu.appendChild(dl);
        menu.appendChild(del);
        document.body.appendChild(menu);

        var r = anchor.getBoundingClientRect();
        var w = menu.offsetWidth || 190;
        menu.style.top = (r.bottom + 4) + 'px';
        menu.style.left = Math.max(8, Math.min(r.left, window.innerWidth - w - 8)) + 'px';

        openMenuEl = menu;
        // Defer so the click that opened the menu doesn't immediately close it.
        setTimeout(function () {
            document.addEventListener('click', closeItemMenu, true);
            document.addEventListener('keydown', onMenuKey, true);
        }, 0);
    }

    function startNewChat() {
        // Tell the backend the live view is abandoned: stops any running agent
        // and invalidates its run id so late pushes (todo/milestone watchers,
        // run-end Agent Notes) can't repaint the freshly reset UI below.
        fetch('/api/new-chat', { method: 'POST' }).catch(function () { /* non-fatal */ });
        window.currentSessionId = null;          // fresh start: no memory loaded
        // A fresh chat has no mode history — unlock the mode picker.
        document.dispatchEvent(new CustomEvent('agentmode:lock', { detail: { mode: null } }));
        // ...and start it on the DEFAULT mode: a new chat is Computer use until
        // the user picks otherwise. Silent set (no agentmode:changed), so no
        // side effects fire (e.g. ios_session pairing); the CLI stage follows
        // via its own agentmode:set listener and slides the terminal away.
        document.dispatchEvent(new CustomEvent('agentmode:set', { detail: { mode: 'computer' } }));
        // Roll a run-active composer back to idle (orbs/strip/box) — idempotent,
        // and agentComplete won't do it for us now that its push is invalidated.
        if (window.chatInputRestoreIdle) window.chatInputRestoreIdle();
        // Same reason for the top-right stream: its fade normally comes from
        // agentComplete; clear it here so a mid-run New chat can't leave old text.
        if (window.trackingProgress) window.trackingProgress.start();
        if (window.resetChatUi) window.resetChatUi();
        if (window.resetMemoryBar) window.resetMemoryBar();     // empty the memory bar
        if (window.hideMemoryBar) window.hideMemoryBar();       // hide it (only new chat hides)
        if (window.showWelcomeHero) window.showWelcomeHero();   // bring back the hero
        var input = document.querySelector('.chat-input');
        if (input) { input.disabled = false; input.focus(); }
        // Announce the fresh start (the notes stage hides itself on this).
        document.dispatchEvent(new CustomEvent('chat:new'));
        document.dispatchEvent(new CustomEvent('chats:refresh'));   // clears highlight
    }

    function mount(bar) {
        if (!bar) return;
        var body = bar.querySelector('.left-bar-body');
        if (!body || body.querySelector('.new-chat-btn')) return; // already mounted
        fetch('chat/chat.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (body.querySelector('.new-chat-btn')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var btn = holder.querySelector('.new-chat-btn');
                if (!btn) return;
                body.insertBefore(btn, body.firstChild); // top of the body
                btn.addEventListener('click', startNewChat);

                // History list lives directly under the New-chat button.
                var list = body.querySelector('.chat-history-list');
                if (!list) {
                    list = document.createElement('div');
                    list.className = 'chat-history-list';
                    body.insertBefore(list, btn.nextSibling);
                }
                loadChats(list);
            })
            .catch(function () { /* non-fatal: the button just won't render */ });
    }

    // Reload the list whenever a run ends or a new chat is started.
    document.addEventListener('chats:refresh', function () {
        var list = document.querySelector('.chat-history-list');
        if (list) loadChats(list);
    });

    // Mount once the left bar exists: immediately if already injected, otherwise
    // when left_bar.js fires 'leftbar:ready'.
    var existing = document.getElementById('leftBar');
    if (existing) mount(existing);
    document.addEventListener('leftbar:ready', function (e) {
        mount((e && e.detail && e.detail.bar) || document.getElementById('leftBar'));
    });
})();
