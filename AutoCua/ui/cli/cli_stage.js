// CLI stage orchestrator.
//
// Owns the #cliContainer panel (slides up on cli_await; cli/cli_stage.css) and a registry
// of live coder cards keyed by task_id (the card component is cli/cli_container/coder_card.js).
// It just ROUTES the window.cli* event stream (the same events the Telegram banner consumes,
// pushed by app.py send_cli_event_to_frontend) to the right card. Split this way so adding
// more agents later is easy — give each its own card + container slot.
//
// For now: ONE card, centered in #cliContainer. (Multiple agents → multiple stacked
// containers is the next step; the registry below already supports N cards.)
//
// NOTE: script.js assigns its window.cli* inside a DOMContentLoaded handler, so we install
// ours on the macrotask AFTER DOMContentLoaded to win the override.
(function () {
    'use strict';

    var MAX_VISIBLE = 3;               // at most 3 coder cards stacked ("piped") at once
    var cards = new Map();             // task_id -> card (VISIBLE cards only)
    var queue = [];                    // FIFO of { taskId, desc } waiting for a free slot
    var minionParent = new Map();      // minion_id -> parent task_id
    var idleCard = null;               // the waiting terminal shown in Shell use
    var clearTimer = null;             // the deferred end-of-run wipe — cancellable, so a
                                       // run starting inside its 600ms window isn't destroyed

    function getContainer() { return document.getElementById('cliContainer'); }
    // 1 card -> full-size; 2 -> halves; 3 -> thirds (drives the height transition in CSS).
    function syncLayout() {
        var c = getContainer(); if (!c) return;
        c.classList.toggle('n2', cards.size === 2);
        c.classList.toggle('n3', cards.size === 3);
    }

    function injectPanel() {
        var grid = document.getElementById('mainGrid');
        if (!grid) return false;
        if (!document.getElementById('cliContainer')) {
            var panel = document.createElement('div');
            panel.id = 'cliContainer';
            panel.setAttribute('aria-hidden', 'true');
            grid.appendChild(panel);
        }
        return true;
    }

    function clearAll() {
        clearTimer = null;
        // A NEW run started before this deferred wipe fired (await_end schedules it
        // +600ms out, and the backend's await poll can lag the visible task end by a
        // second) — never clear out from under a live run: it would snapshot the live
        // card as 'stopped' and destroy it. The new run's own await_end reschedules.
        if (awaitActive) return;
        var c = getContainer();
        // A run that dies without its cliTaskEnd (backend error / stop teardown) would
        // lose its notes with the card — snapshot the first card's scratchpad now, so
        // the Agent Notes still land under the fresh idle terminal.
        if (shellPinned && !shellNotes && cards.size) {
            var first = cards.values().next().value;
            captureNotes(first, 'stopped', '');
        }
        // Shell use, already settled: cliTaskEnd unmounted the run's card and mounted
        // the idle terminal (+ Agent Notes) at task end — there is nothing left to
        // clear. Wiping anyway would tear that unit down just to rebuild it 600ms
        // later, which BLINKED the freshly faded-in notes (and the terminal).
        if (shellPinned && !cards.size && idleCard) {
            queue.length = 0;
            minionParent.clear();
            return;
        }
        cards.forEach(function (card) { if (card && card.dispose) card.dispose(); });
        if (c) { c.innerHTML = ''; c.classList.remove('n2', 'n3'); }
        cards.clear();
        queue.length = 0;
        minionParent.clear();
        idleCard = null;                  // innerHTML wiped it too
        if (shellPinned) mountIdle();     // Shell use never shows a bare panel
    }

    // Shell use gets the terminal header — blinking dot + name, the agent head
    // under it, then the `>` prompt everything streams from. The other modes keep
    // the plain `> AutoCua Code` card.
    function cardOpts() { return shellPinned ? { header: 'AutoCua Code' } : {}; }

    // One agent row: left tool-chain | center terminal | right scratchpad.
    // Shared by the live cards and the idle placeholder so they're the same shape.
    function buildUnit(card) {
        var unit = document.createElement('div');
        unit.className = 'cc-agent';
        if (card.chainEl) unit.appendChild(card.chainEl);     // "Tool response" — LEFT
        var wrap = document.createElement('div');
        wrap.className = 'cc-card-wrap';
        wrap.appendChild(card.el);                            // terminal — CENTER
        unit.appendChild(wrap);
        if (card.trackEl) unit.appendChild(card.trackEl);     // "tracking progress" — RIGHT
        return unit;
    }

    // Mount one coder agent appended BELOW any existing card, then fade/slide it
    // in. Oldest stays on top.
    function mountAgent(taskId, desc) {
        var c = getContainer();
        if (!c || !window.CliCoderCard || cards.has(taskId)) return;
        shellNotes = null;                // a new run starts — the last run's notes are done
        shellHistory = null;              // ...and so is a reopened chat's history view
        dropTranscriptHold();             // ...and the last run's wait for its transcript
        clearTimeout(clearTimer); clearTimer = null;   // and the last run's deferred wipe is moot

        // Shell use: PROMOTE the waiting terminal rather than swapping it out. It is already
        // the same card, in the same place, with the same live mascot canvas — tearing it
        // down and sliding an identical replacement up from 16px is exactly the jitter you
        // see on send. Promotion just attaches the two side zones and starts the run, so the
        // terminal simply carries on. Nothing to animate, nothing to repaint.
        if (idleCard && !cards.size) {
            var promoted = idleCard;
            var pUnit = promoted._unit;
            idleCard = null;                                   // it's a live card now
            pUnit.classList.remove('cc-idle');
            var oldNotes = pUnit.querySelector('.cc-notes');   // notes make way for the run
            if (oldNotes) pUnit.removeChild(oldNotes);
            if (promoted.chainEl) pUnit.insertBefore(promoted.chainEl, pUnit.firstChild);
            if (promoted.trackEl) pUnit.appendChild(promoted.trackEl);
            cards.set(taskId, promoted);
            syncLayout();
            if (promoted.begin) promoted.begin();              // opening phase starts NOW
            return;
        }

        unmountIdle();                    // (other modes / extra agents) idle makes way
        var card = window.CliCoderCard.create(desc, cardOpts());
        var unit = buildUnit(card);
        unit.classList.add('entering');
        card._unit = unit;
        c.appendChild(unit);
        cards.set(taskId, card);
        syncLayout();
        // strip .entering next frame so it transitions in (and after the .two height change lands)
        if (window.requestAnimationFrame) requestAnimationFrame(function () { unit.classList.remove('entering'); });
        else unit.classList.remove('entering');
    }

    // Remove a visible agent instantly (its task ended), then pull the next queued one (if any).
    function unmountAgent(taskId) {
        var card = cards.get(taskId);
        if (!card) return;
        if (card.dispose) card.dispose();
        if (card._unit && card._unit.parentNode) card._unit.parentNode.removeChild(card._unit);
        cards.delete(taskId);
        syncLayout();
        if (queue.length) { var next = queue.shift(); mountAgent(next.taskId, next.desc); }
        else if (shellPinned && !cards.size) mountIdle();   // back to the waiting terminal
    }

    // Shell use PINS the stage open: the terminal is that mode's home screen, so
    // the panel slides up the moment the mode is picked and stays after a run
    // ends (empty, waiting for the next task) instead of sliding away. Every
    // other mode keeps the original behaviour — the stage exists only for the
    // duration of a cli_await.
    var shellPinned = false;      // Shell use selected?
    var awaitActive = false;      // a cli_await is in flight?
    var heroWasVisible = false;   // was the "Made to do…" hero up before Shell use hid it?

    // ---- Agent Notes (Shell use) ------------------------------------------------
    // The main app shows the agent's scratchpad as full-grid "Agent Notes" on run end;
    // that stage sits BEHIND this panel in Shell use (z 5 vs 6) and the backend never
    // pushes it for shell runs anyway. So the CLI stage builds its own, terminal-sized:
    // the coder's scratchpad notes (snapshotted from the card at task end, before
    // dispose), rendered with the SAME global .agent-note* classes (notes_stage.css)
    // in the free row under the idle terminal — and cleared the moment the next task
    // is sent, exactly like the main stage hides on a send. The run's done message
    // (or its stop / error reason) always closes the list, notes or not, so a run
    // ends showing what reopening the chat would show. Clicking into the terminal
    // (cli-typing) collapses the notes — the prompt needs the room (coder_card.css).
    var shellNotes = null;        // { entries: [..], status, summary, summaryHtml } of the last finished run
    var shellHistory = null;      // [{task, done_message}] of a REOPENED shell chat (cliShellHistory)

    // A run's task_end lands a beat BEFORE the backend has saved it. The saved
    // transcript (cliShellTranscript: every run of the chat, this one last) is
    // the view to show, so the live notes are held for it; only if it never
    // arrives (save failed, push dropped) do the captured notes show after all.
    var transcriptWait = null;    // fallback timer while that hold is on
    function holdForTranscript() {
        clearTimeout(transcriptWait);
        transcriptWait = setTimeout(function () {
            transcriptWait = null;
            if (idleCard && idleCard._unit) renderNotes(idleCard._unit);
        }, 1500);
    }
    function dropTranscriptHold() { clearTimeout(transcriptWait); transcriptWait = null; }

    function captureNotes(card, status, summary, summaryHtml) {
        if (!shellPinned || !card) return;
        // A reopened chat owns the panel: if the user navigated to a saved
        // chat while this run was still finishing, its transcript view wins —
        // the finished run's notes belong to the chat that just ended, not to
        // the one on screen. Non-EMPTY only: reopening the running chat's OWN
        // row mid-first-run stores an empty transcript, and eating the notes
        // for that would end the run into a completely blank terminal.
        if (shellHistory && shellHistory.length) return;
        shellNotes = {
            entries: (card.getNotes ? card.getNotes() : []),
            status: String(status || 'complete'),
            summary: String(summary || ''),
            summaryHtml: String(summaryHtml || '')   // markdown.py's rendering of summary
        };
    }

    function renderNotes(unit) {
        // Two sources, one panel: a reopened chat's exchange history wins over
        // the last run's live notes (they're mutually nulled at the write sites).
        var hasHistory = !!(shellHistory && shellHistory.length);
        if ((!hasHistory && !shellNotes) || !unit || unit.querySelector('.cc-notes')) return;
        if (!hasHistory && transcriptWait) return;   // the saved transcript is on its way
        var n = shellNotes;
        // Nothing to say — keep the row clean. A run with a done message is
        // never "nothing to say", and neither is an interrupted one: it must
        // show even when it was stopped before it wrote a single note.
        if (!hasHistory && !n.entries.length && !n.summary && n.status !== 'stopped') return;

        var box = document.createElement('div');
        box.className = 'cc-notes';
        box.innerHTML =
            '<div class="agent-notes-header">' +
                '<svg class="agent-notes-pen" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
                    '<path d="M12 20h9"></path>' +
                    '<path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z"></path>' +
                '</svg>' +
                '<span class="agent-notes-title">Agent Notes</span>' +
            '</div>' +
            '<div class="agent-notes-list"></div>';
        var list = box.querySelector('.agent-notes-list');

        // `markup` is server-rendered Markdown (markdown.py) and is assigned as
        // HTML; `text` is raw and always goes in as textContent, never parsed.
        function row(num, text, markup) {
            var r = document.createElement('div');
            r.className = 'agent-note';
            var no = document.createElement('span');
            no.className = 'agent-note-num';
            no.textContent = num;
            var txt = document.createElement('span');
            txt.className = 'agent-note-text' + (markup ? ' md' : '');
            if (markup) txt.innerHTML = String(markup);
            else txt.textContent = String(text);
            r.appendChild(no); r.appendChild(txt);
            list.appendChild(r);
        }
        if (hasHistory) {
            // Reopened chat: "N. task" + how that run ended below it — the same
            // rows the main stage's showAgentHistory renders (notes_stage.js),
            // with the identical global classes and the same rendered Markdown.
            shellHistory.forEach(function (x, i) {
                x = x || {};
                row((i + 1) + '.', String(x.task || ''), x.task_html);
                var reply = document.createElement('div');
                reply.className = 'agent-note-reply' + (x.done_html ? ' md' : '');
                if (x.done_html) reply.innerHTML = String(x.done_html);
                else reply.textContent = String(x.done_message || '');
                list.appendChild(reply);
            });
        } else {
            // Live run end: the scratchpad notes, then ALWAYS how the run ended —
            // the done message (or the stop / error reason) as the closing reply,
            // the same row a reopened chat shows under its task. A run stopped
            // with no reason still says so: the card vanishes on task_end, and
            // without a marker an interrupted run would read as a finished one.
            n.entries.forEach(function (t, i) { row((i + 1) + '.', t); });
            var ending = n.summary || (n.status === 'stopped' ? 'agent interrupted' : '');
            if (ending) {
                var reply = document.createElement('div');
                // .solo: no numbered note above it, so no indent to sit under
                reply.className = 'agent-note-reply' + (n.summaryHtml ? ' md' : '') +
                                  (n.entries.length ? '' : ' solo');
                if (n.summaryHtml) reply.innerHTML = n.summaryHtml;   // markdown.py output
                else reply.textContent = ending;
                list.appendChild(reply);
            }
        }

        // The newest row — this run's done message, or a reopened chat's last
        // exchange — is the one that matters, so the list follows its end: on
        // mount, once the box has faded in, and whenever the list changes size
        // (a window resize moves the 34vh cap and re-wraps the rows). Wheeling
        // up stops the following; wheeling back down to the end resumes it.
        var follow = true;
        function showLast() { if (follow) list.scrollTop = list.scrollHeight; }

        // WKWebView refuses to wheel-scroll an overflow area inside a transformed
        // subtree, and #cliContainer carries a transform — the list would look
        // scrollable and simply not move (its scrollbar is hidden, so overflowing
        // notes would be unreachable). scrollTop DOES work, so drive it from the
        // wheel ourselves — same fix as the terminal's output view in coder_card.js.
        list.addEventListener('wheel', function (e) {
            if (list.scrollHeight <= list.clientHeight) return;
            list.scrollTop += (e.deltaMode === 1 ? e.deltaY * 16 : e.deltaY);
            e.preventDefault();
            follow = list.scrollHeight - list.scrollTop - list.clientHeight <= 2;
        }, { passive: false });

        unit.appendChild(box);
        showLast();
        // fade/slide in like a mounting agent unit (.cc-notes.in in coder_card.css)
        if (window.requestAnimationFrame) requestAnimationFrame(function () { box.classList.add('in'); showLast(); });
        else box.classList.add('in');
        box.addEventListener('transitionend', function (e) {
            if (e.target === box && e.propertyName === 'transform') showLast();
        });
        if (window.ResizeObserver) new ResizeObserver(showLast).observe(list);
    }

    // The waiting terminal: a normal coder card that's simply never fed any
    // events, so it sits there as an empty `>` prompt with its blinking cursor.
    // It keeps the mode looking like a terminal instead of a blank panel; a real
    // run swaps it out, and it comes back when the run ends.
    //
    // TERMINAL ONLY — the card's chainEl / trackEl are deliberately NOT passed to
    // buildUnit, so "Tool response" and "tracking progress" don't exist until a
    // real run mounts its own card and has something to put in them.
    function mountIdle() {
        var c = getContainer();
        if (!c || idleCard || cards.size || !window.CliCoderCard) return;
        // idle:true — don't start the opening phase; mountAgent's promotion calls begin().
        var opts = cardOpts();
        opts.idle = true;
        idleCard = window.CliCoderCard.create('', opts);
        var unit = buildUnit({ el: idleCard.el });
        unit.classList.add('cc-idle');
        idleCard._unit = unit;
        c.appendChild(unit);
        renderNotes(unit);   // the last run's Agent Notes, in the free row under the terminal
    }

    function unmountIdle() {
        if (!idleCard) return;
        if (idleCard.dispose) idleCard.dispose();
        if (idleCard._unit && idleCard._unit.parentNode) {
            idleCard._unit.parentNode.removeChild(idleCard._unit);
        }
        idleCard = null;
    }

    function syncShellStage() {
        var wrap = document.getElementById('agentModeWrap');
        shellPinned = !!wrap && wrap.dataset.mode === 'shell';
        // Shell use re-lays-out the card: full-width terminal on top, the two
        // side zones moved BELOW it (coder_card.css). Dispatched-coder runs in
        // the other modes keep the left | center | right columns.
        document.body.classList.toggle('cli-shell', shellPinned);
        if (shellPinned) {
            injectPanel();
            document.body.classList.add('cli-stage');
            mountIdle();
            // The terminal IS this mode's empty state, so the "Made to do…" hero has
            // nothing to say and would just sit behind it. Remember whether it was up,
            // so leaving the mode restores exactly what was there — a hero already
            // hidden by a sent task or a reopened chat must STAY hidden.
            var hero = document.getElementById('welcomeHero');
            heroWasVisible = !!hero && !hero.classList.contains('hidden');
            if (window.hideWelcomeHero) window.hideWelcomeHero();
        } else {
            unmountIdle();
            shellNotes = null;                              // stale across mode switches
            shellHistory = null;
            dropTranscriptHold();
            document.body.classList.remove('cli-typing');   // no prompt outside Shell use
            // left Shell use — but never yank the stage out from under a live run
            if (!awaitActive) {
                document.body.classList.remove('cli-stage');
                // no terminal in this mode: the hero is the empty state again. Skipped
                // mid-run (guarded above), and skipped when a CHAT is open (leaving
                // shell because a saved chat was reopened — the hero would bleed
                // through the transparent grid over that chat's history).
                if (heroWasVisible && !window.currentSessionId && window.showWelcomeHero) window.showWelcomeHero();
                heroWasVisible = false;
            }
        }
    }

    function installHooks() {
        injectPanel();

        // Slide the panel in / out. The chat box is untouched (no .cli-mode).
        window.cliAwaitStart = function (reason) {
            injectPanel();
            awaitActive = true;
            // a stale end-of-run wipe from the LAST run must not fire into this one
            clearTimeout(clearTimer); clearTimer = null;
            document.body.classList.add('cli-stage');
        };
        window.cliAwaitEnd = function () {
            awaitActive = false;
            if (!shellPinned) {
                document.body.classList.remove('cli-stage');
                // A New chat / mode switch DURING the live run deferred the hero
                // hand-off (the un-pin branch skips it while awaitActive) — the
                // stage is really going away now, so finish it here.
                if (heroWasVisible && !window.currentSessionId && window.showWelcomeHero) {
                    window.showWelcomeHero();
                    heroWasVisible = false;
                }
            }
            clearTimeout(clearTimer);
            clearTimer = setTimeout(clearAll, 600);   // clear cards after the slide-down finishes
        };

        // agent_mode.js paints the picker's data-mode BEFORE dispatching either
        // event, so reading it back here is always current.
        document.addEventListener('agentmode:ready', syncShellStage);
        document.addEventListener('agentmode:changed', syncShellStage);
        // agentmode:set is SILENT by contract (no agentmode:changed — ios_session
        // must not react), but the stage still has to follow it: New chat resets
        // the picker to Computer use this way, and the pinned shell terminal must
        // slide away with it. Deferred one tick — this listener registers before
        // agent_mode's fetch-injected wire(), so a same-tick read would see the
        // STALE dataset.mode; the timeout lets agent_mode's set handler paint first.
        document.addEventListener('agentmode:set', function () { setTimeout(syncShellStage, 0); });
        // Shell use: the whole PANEL is the terminal — clicking anywhere on it
        // (the empty space around/below the card included) puts you at the
        // prompt, not just the thin `>` line. Clicks on the card itself are
        // handled by the card's own click-to-focus; this covers the rest of the
        // stage. Guards: user-mode terminal only (idleCard set — never over a
        // running agent), never over the Agent Notes box (focusing flips on
        // cli-typing, which collapses the notes — they must stay readable), and
        // a completed text selection stays a selection.
        // Same click-vs-drag rule as the card's own handler: pointer travel and
        // double-clicks mean "selecting", anything else focuses — a stale page
        // selection must never swallow the click (it used to make the terminal
        // feel randomly dead on user-select:none spots).
        var stageDownX = 0, stageDownY = 0;
        document.addEventListener('mousedown', function (e) { stageDownX = e.clientX; stageDownY = e.clientY; });
        document.addEventListener('click', function (e) {
            if (!shellPinned || !idleCard || !idleCard.el) return;
            var t = e.target;
            if (!t || !t.closest || !t.closest('#cliContainer')) return;
            if (t.closest('.cc-notes')) return;
            if (idleCard.el.contains(t)) return;   // the card's own handler owns this click
            if (e.detail > 1) return;                                                    // dbl/triple = select
            if (Math.abs(e.clientX - stageDownX) > 4 || Math.abs(e.clientY - stageDownY) > 4) return;  // drag
            if (idleCard.el._focusPrompt) idleCard.el._focusPrompt();
        });
        // New chat = a NEW terminal. chat.js resets the main chat UI, but the
        // shell stage owns its own panel — without this, the old terminal's
        // scrollback, prompt path, typing state and Agent Notes all survived
        // into the "fresh" chat. Tear everything down and mount a brand-new
        // bare `>_` idle terminal. (clearAll can't be reused here: its
        // already-settled guard would skip exactly the common case, and its
        // notes-capture fallback would drag the old run's notes along.)
        document.addEventListener('chat:new', function () {
            if (!shellPinned) return;
            var c = getContainer();
            // any live run's backend UI pushes were already invalidated by
            // /api/new-chat; its cards go with the old chat, stale events for
            // dead task ids are simply dropped by the cli* handlers
            cards.forEach(function (card) { if (card && card.dispose) card.dispose(); });
            cards.clear();
            queue.length = 0;
            minionParent.clear();
            shellNotes = null;                             // fresh chat — no old notes
            shellHistory = null;                           // ...and no reopened transcript
            dropTranscriptHold();                          // ...nor a wait for the old run's one
            clearTimeout(clearTimer); clearTimer = null;   // no deferred wipe into the new terminal
            document.body.classList.remove('cli-typing');
            unmountIdle();
            if (c) { c.innerHTML = ''; c.classList.remove('n2', 'n3'); }
            mountIdle();                                   // brand-new bare `>_` terminal
            // the fresh terminal IS this mode's empty state — startNewChat just
            // showed the hero, which would bleed through the transparent panel;
            // re-hide it (same synchronous dispatch, so it never flashes) and
            // remember it belongs to this fresh chat if the user leaves Shell use.
            if (window.hideWelcomeHero) window.hideWelcomeHero();
            heroWasVisible = true;
        });
        syncShellStage();   // in case the picker mounted first

        // A coder agent starts → mount it if a slot is free, else queue it. Up to 2 show at once;
        // the rest wait FIFO. Bare "[minion] " task_starts are ignored.
        window.cliTaskStart = function (taskId, desc) {
            if (typeof desc === 'string' && desc.indexOf('[minion] ') === 0) return;
            if (cards.has(taskId)) return;                                   // already visible
            for (var i = 0; i < queue.length; i++) if (queue[i].taskId === taskId) return;  // already queued
            if (cards.size < MAX_VISIBLE) mountAgent(taskId, desc);
            else queue.push({ taskId: taskId, desc: desc });
        };
        window.cliTaskLine = function (taskId, line, stream) {
            var card = cards.get(taskId);   // queued (unmounted) agents' lines are dropped until shown
            if (card) card.setLine(line);
        };
        // Todos are handled internally by the agent and NOT rendered as a checklist. The
        // card swallows them in every mode except Shell use, where the terminal is the
        // full transcript and setTodo() streams the list into it.
        window.cliTaskTodo = function (taskId, payload) {
            var card = cards.get(taskId);
            if (card && card.setTodo) card.setTodo(payload);
        };
        window.cliTaskEnd = function (taskId, status, summary, summaryHtml) {
            if (cards.has(taskId)) {
                // Snapshot the run's scratchpad + done message BEFORE the card is disposed —
                // mountIdle (via unmountAgent) shows them as Agent Notes under the terminal.
                captureNotes(cards.get(taskId), status, summary, summaryHtml);
                if (shellPinned) holdForTranscript();                  // show the saved transcript instead
                unmountAgent(taskId);                                  // instant vanish + pull next queued
                return;
            }
            // finished before it was ever shown → just drop it from the queue
            for (var i = 0; i < queue.length; i++) if (queue[i].taskId === taskId) { queue.splice(i, 1); break; }
        };

        window.cliMinionStart = function (parentId, taskId, query) {
            var card = cards.get(parentId);
            if (!card) return;
            minionParent.set(taskId, parentId);
            card.addMinion(taskId, query);
        };
        window.cliMinionLine = function (taskId, line, stream) {
            var parent = minionParent.get(taskId);
            var card = (parent != null) ? cards.get(parent) : null;
            if (card) card.setMinionLine(taskId, line);
        };
        window.cliMinionEnd = function (taskId, status, summary) {
            var parent = minionParent.get(taskId);
            var card = (parent != null) ? cards.get(parent) : null;
            if (card) card.endMinion(taskId, status);
        };

        // Stage-level message straight onto the terminal. In Shell use the four zones
        // that normally carry errors (milestone stream et al.) are hidden, so a run that
        // fails before producing any output would otherwise read as "nothing happened".
        function currentCard() {
            if (idleCard) return idleCard;
            var first = cards.values().next();
            return first && first.value;
        }

        window.cliShellNote = function (text, chatId) {
            // A late run-end note carries its CHAT id — if the user has since
            // opened a DIFFERENT chat, that outcome belongs to the other chat's
            // terminal, not the one on screen: drop it. (A New chat needs no
            // guard here — /api/new-chat already invalidates the run's pushes.)
            if (chatId && window.currentSessionId && String(window.currentSessionId) !== String(chatId)) return;
            var card = currentCard();
            if (card && card.note) card.note(text);
        };

        // Reopened shell chat: chat.js hands us the chat's exchange transcript
        // ({task, done_message} per run) — shown as Agent Notes rows under the
        // idle terminal (the notes-stage overlay sits BEHIND the pinned panel).
        // State-based, so it works whichever order it lands in relative to the
        // silent agentmode:set that re-pins the stage.
        window.cliShellHistory = function (exchanges) {
            shellHistory = Array.isArray(exchanges) ? exchanges.map(function (x) {
                x = x || {};
                return {
                    task: String(x.task || ''),
                    done_message: String(x.done_message || ''),
                    // Pre-rendered Markdown from /api/chats/<id> (markdown.py).
                    task_html: x.task_html ? String(x.task_html) : '',
                    done_html: x.done_html ? String(x.done_html) : ''
                };
            }) : [];
            shellNotes = null;             // the reopen view replaces the last run's notes
            dropTranscriptHold();          // ...and is what a held run end was waiting for
            // Hand-typing collapses .cc-notes (coder_card.css); the user asked to
            // SEE a chat — drop typing mode so the history is visible. The focus
            // handler only re-adds it on a fresh click into the prompt.
            document.body.classList.remove('cli-typing');
            if (idleCard && idleCard.el && document.activeElement && idleCard.el.contains(document.activeElement)) {
                try { document.activeElement.blur(); } catch (e) {}
            }
            if (idleCard && idleCard._unit) {   // already pinned: re-render in place
                var old = idleCard._unit.querySelector('.cc-notes');
                if (old && old.parentNode) old.parentNode.removeChild(old);
                renderNotes(idleCard._unit);
            }
            // not pinned yet: the deferred agentmode:set sync mounts the idle
            // terminal, and mountIdle's renderNotes picks the history up.
        };

        // A finished run, saved: service.py's run_shell pushes the chat's whole
        // transcript (this run last) once save_run has appended it, and it is
        // shown exactly like a reopened chat — old runs above, the new done
        // message below. Only for the chat on screen, and only between runs: a
        // run already going brings its own transcript when it ends.
        window.cliShellTranscript = function (payload, chatId) {
            if (!shellPinned || cards.size) return;
            if (!chatId || String(window.currentSessionId || '') !== String(chatId)) return;
            var rows = payload;
            if (typeof payload === 'string') {
                try { rows = JSON.parse(payload); } catch (e) { rows = null; }
            }
            if (!Array.isArray(rows) || !rows.length) return;   // the held notes still show
            window.cliShellHistory(rows);
        };

        // Live output from a HAND-TYPED command (service.py's _ManualTerminal). Lines
        // arrive in small batches while the process runs — that's what makes `ping`
        // scroll live instead of appearing only once it dies.
        window.shellTermLines = function (payload) {
            var lines;
            try { lines = JSON.parse(payload); } catch (e) { return; }
            var card = currentCard();
            if (!card || !card.note || !lines) return;
            lines.forEach(function (l) { card.note(l); });
        };
        window.shellTermEnd = function (code) {
            var card = currentCard();
            if (card && card.termEnd) card.termEnd(code);
        };

        window.cliPillWebLoadingStart = function (taskId) { var c = cards.get(taskId); if (c) c.setWeb(true); };
        window.cliPillWebLoadingEnd = function (taskId) { var c = cards.get(taskId); if (c) c.setWeb(false); };
    }

    function schedule() { setTimeout(installHooks, 0); }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', schedule);
    } else {
        schedule();
    }
})();
