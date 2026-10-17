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
// Shell use: ONE card for the whole chat, the CoderUse bot on the chat box below it
// (cli/shell_bot.js): a chat with a one-line terminal under it. The line greets ("Hi!
// How can I help you!"), your messages show as the normal chat's bubbles, the line
// carries the agent's words while a run goes, and the answer lands in the chat's reply
// box; it all stays: at a run's end the card goes back to waiting instead of being
// replaced. The run's tool chain plays on the bot's face. A reopened shell chat fills
// it with its messages and answers. The chat box's Auto / Manual pill (agent_mode.js)
// hands the line's prompt to you (Manual) or to the agent.
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
        // second) — never clear out from under a live run: it would destroy its card.
        // The new run's own await_end reschedules.
        if (awaitActive) return;
        var c = getContainer();
        // Shell use: the terminal is the chat's, never wiped. A run that died without
        // its cliTaskEnd (backend error / stop teardown) just goes back to waiting.
        if (shellPinned) {
            Array.from(cards.keys()).forEach(function (id) { demote(id, 'stopped', ''); });
            queue.length = 0;
            minionParent.clear();
            if (!idleCard) mountIdle();
            return;
        }
        cards.forEach(function (card) { if (card && card.dispose) card.dispose(); });
        if (c) { c.innerHTML = ''; c.classList.remove('n2', 'n3'); }
        cards.clear();
        queue.length = 0;
        minionParent.clear();
        idleCard = null;                  // innerHTML wiped it too
        runChat = null;
    }

    // Shell use gets the terminal header — blinking dot + name, then the `>` prompt
    // everything streams from — and its tool chain plays on the bot's face. The other
    // modes keep the plain `> AutoCua Code` card.
    function cardOpts() {
        if (!shellPinned) return {};
        return {
            header: 'AutoCua Code',
            faceCanvas: window.shellBot ? window.shellBot.face() : null,
            // its minions' tools play on their own Minion bots' faces
            minionFace: function (id) { return window.shellBot ? window.shellBot.minionFace(id) : null; },
            minionWork: function (id, on) { if (window.shellBot) window.shellBot.minionWork(id, on); }
        };
    }

    // One agent row: left tool-chain | center terminal | right scratchpad. In Shell
    // use the terminal alone (its chain is on the bot's face). Shared by the live
    // cards and the idle placeholder so they're the same shape.
    function buildUnit(card) {
        var sides = !shellPinned;
        var unit = document.createElement('div');
        unit.className = 'cc-agent';
        if (sides && card.chainEl) unit.appendChild(card.chainEl);     // "Tool response" — LEFT
        var wrap = document.createElement('div');
        wrap.className = 'cc-card-wrap';
        wrap.appendChild(card.el);                                     // terminal — CENTER
        unit.appendChild(wrap);
        if (sides && card.trackEl) unit.appendChild(card.trackEl);     // "tracking progress" — RIGHT
        return unit;
    }

    // Mount one coder agent appended BELOW any existing card, then fade/slide it
    // in. Oldest stays on top.
    function mountAgent(taskId, desc) {
        var c = getContainer();
        if (!c || !window.CliCoderCard || cards.has(taskId)) return;
        clearTimeout(clearTimer); clearTimer = null;   // the last run's deferred wipe is moot
        if (runChat == null) runChat = window.currentSessionId || null;

        // Shell use: PROMOTE the waiting terminal rather than swapping it out. It is already
        // the same card, in the same place, with the chat's lines in it — tearing it down
        // and sliding an identical replacement up from 16px is exactly the jitter you see
        // on send. Promotion just starts the run, so the terminal simply carries on, and the
        // bot gets to work (its face shows the run's tools). Nothing to animate, nothing to
        // repaint.
        if (idleCard && !cards.size) {
            var promoted = idleCard;
            var pUnit = promoted._unit;
            idleCard = null;                                   // it's a live card now
            pUnit.classList.remove('cc-idle');
            cards.set(taskId, promoted);
            syncLayout();
            if (promoted.begin) promoted.begin();              // opening phase starts NOW
            if (window.shellBot) { window.shellBot.settle(); window.shellBot.work(true); }
            return;
        }

        unmountIdle();                    // (other modes / extra agents) idle makes way
        // Shell use beside another live card (no idle one to promote): a fresh Shell card,
        // the agent's from the start (begin below), like a promoted one
        var opts = cardOpts();
        if (shellPinned) opts.idle = true;
        var card = window.CliCoderCard.create(desc, opts);
        var unit = buildUnit(card);
        unit.classList.add('entering');
        card._unit = unit;
        c.appendChild(unit);
        cards.set(taskId, card);
        syncLayout();
        if (shellPinned && card.begin) {
            card.begin();
            if (window.shellBot && isShellCard(card)) { window.shellBot.settle(); window.shellBot.work(true); }
        }
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
        if (!cards.size) runChat = null;                    // no run left to belong to a chat
        if (queue.length) { var next = queue.shift(); mountAgent(next.taskId, next.desc); }
        else if (shellPinned && !cards.size) mountIdle();   // back to the waiting terminal
    }

    // Shell use: the run is over, its terminal goes back to waiting (the reverse of the
    // promotion in mountAgent) with everything it showed still in it. Late events for
    // the old task id find no card and are dropped.
    // A card that is not the Shell terminal (a Computer use run's coder, still
    // going when a shell chat was opened), or one whose chat was left for another
    // shell chat mid-run (pendingHistory), is not kept: a fresh terminal takes
    // its place, with that chat's rows.
    function isShellCard(card) { return !!(card && card.el && card.el.classList.contains('has-head')); }
    function demote(taskId) {
        var card = cards.get(taskId);
        if (!card) return;
        cards.delete(taskId);
        syncLayout();
        // only this card's minions stop routing (another card's run may still be going)
        minionParent.forEach(function (parent, m) { if (parent === taskId) minionParent.delete(m); });
        if (card.finish) card.finish();
        if (isShellCard(card) && !idleCard && !pendingHistory) {
            if (card._unit) card._unit.classList.add('cc-idle');
            idleCard = card;
        } else {
            if (card.dispose) card.dispose();
            if (card._unit && card._unit.parentNode) card._unit.parentNode.removeChild(card._unit);
        }
        // the Shell run is over (not merely another card beside it, e.g. a Computer use
        // coder's: that would cost the live Shell run its answer, its face and its bots)
        if (isShellCard(card)) {
            runEnded = true;
            runChat = null;
            if (window.shellBot) { window.shellBot.work(false); window.shellBot.minionsClear(); }
        }
        if (shellPinned && !idleCard) mountIdle();
    }

    // Shell use PINS the stage open: the terminal is that mode's home screen, so
    // the panel slides up the moment the mode is picked and stays after a run
    // ends (empty, waiting for the next task) instead of sliding away. Every
    // other mode keeps the original behaviour — the stage exists only for the
    // duration of a cli_await.
    var shellPinned = false;      // Shell use selected?
    var awaitActive = false;      // a cli_await is in flight?
    var heroWasVisible = false;   // was the "Made to do…" hero up before Shell use hid it?

    // ---- the Shell use terminal's own lines ----------------------------------------
    var GREETING = 'Hi! How can I help you!';
    var pendingHistory = null;    // a reopened chat's rows, waiting for its terminal (cliShellHistory)
    var pendingGreet = null;      // a fresh terminal whose greeting waits for the bot to land
    var runEnded = true;          // the latest run's ending (answer / stop) is already printed
    var runChat = null;           // the chat a running command was sent in

    // Your message as the chat's bubble: its text in .cc-user-text, the part that
    // scrolls once it is longer than four lines (coder_card.css).
    function esc(t) {
        return String(t).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; });
    }
    function printUser(card, text) {
        card.print('<div class="cc-user-text">' + esc(text) + '</div>', 'cc-user', 'html');
    }

    // A fresh chat says hello on its live line, once the bot has come in (it types where
    // you can see it).
    function greet(card) {
        if (!card || !card.hello) return;
        if (document.body.classList.contains('shell-intro')) { pendingGreet = card; return; }
        pendingGreet = null;
        card.hello(GREETING);
    }

    // A reopened chat: each run's request as your message and its done message as the
    // answer under it (Markdown already rendered by markdown.py), all at once. None: hello.
    function fillHistory(card, rows) {
        if (!card || !card.clear) return;
        pendingGreet = null;
        card.clear();
        if (!rows || !rows.length) { greet(card); return; }
        rows.forEach(function (x) {
            x = x || {};
            if (x.task) printUser(card, x.task);
            var answer = x.answer_html || x.done_html;
            var cls = 'cc-answer' + ((x.status || 'success') === 'success' ? '' : ' is-ended');
            if (answer) card.print(String(answer), cls + ' md', 'html');
            else if (x.done_message) card.print(String(x.done_message), cls, 'instant');
        });
    }

    // The run's answer under your message: its done message, or why it ended, typed in
    // letter by letter (its markdown kept) while the chat follows it down.
    function printAnswer(card, status, summary, summaryHtml) {
        if (!card || !card.print) return;
        var ended = status !== 'complete';
        var cls = 'cc-answer cc-fresh' + (ended ? ' is-ended' : '');
        if (summaryHtml) card.print(String(summaryHtml), cls + ' md', 'typehtml');
        else if (summary) card.print(String(summary), cls, 'type');
        else if (status === 'stopped') card.print('Stopped.', 'cc-answer cc-fresh is-ended', 'type');
        else if (status === 'complete') card.print('Done.', cls, 'type');
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
        if (shellPinned) {
            if (pendingHistory) { fillHistory(idleCard, pendingHistory); pendingHistory = null; }
            else greet(idleCard);
        }
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
        var was = shellPinned;
        shellPinned = !!wrap && wrap.dataset.mode === 'shell';
        // Shell use re-lays-out the stage: the chat with its one-line terminal filling
        // the frame and the CoderUse bot on the chat box (shell_bot.css, cli_stage.css,
        // coder_card.css).
        // Dispatched-coder runs in the other modes keep the left | center | right columns.
        document.body.classList.toggle('cli-shell', shellPinned);
        if (shellPinned) {
            injectPanel();
            document.body.classList.add('cli-stage');
            // picked just now: the bot comes in, and the terminal waits for it
            if (!was && window.shellBot) {
                window.shellBot.enter();
                // back while its run still goes: the face shows that run's tools again
                if (Array.from(cards.values()).some(function (k) { return k.el && k.el.classList.contains('has-face'); })) {
                    window.shellBot.work(true);
                }
                // ...and its minions still running stand beside it again (after it lands)
                cards.forEach(function (k) {
                    if (!isShellCard(k) || runEnded) return;
                    [].forEach.call(k.el.querySelectorAll('.cc-mrow:not(.leaving)'), function (r) { window.shellBot.minionIn(r.dataset.id); });
                });
            }
            mountIdle();
            // The terminal IS this mode's empty state, so the "Made to do…" hero has
            // nothing to say and would just sit behind it. Remember whether it was up,
            // so leaving the mode restores exactly what was there — a hero already
            // hidden by a sent task or a reopened chat must STAY hidden.
            var hero = document.getElementById('welcomeHero');
            heroWasVisible = !!hero && !hero.classList.contains('hidden');
            if (window.hideWelcomeHero) window.hideWelcomeHero();
        } else {
            if (was && window.shellBot) window.shellBot.stop();
            pendingGreet = null;
            pendingHistory = null;        // the chat they were for is not the one open now
            unmountIdle();
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
            // Shell use: a run's own await_end always comes after its task_end (which
            // demotes its card; the backend's teardown sends both, in that order). One
            // that finds a card still live is a stopped run's, arriving late after you
            // sent the next message: that run goes on.
            if (shellPinned && cards.size) return;
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
        // Shell use, in Manual: the whole PANEL is the terminal — clicking
        // anywhere on it (the empty space around/below the card included) puts
        // you at the prompt, not just the thin `>` line. Clicks on the card
        // itself are handled by the card's own click-to-focus; this covers the
        // rest of the stage. In Auto a click does nothing: only the chat box's
        // pill turns it to Manual.
        // Guards: user-mode terminal only (idleCard set — never over a running
        // agent), and a completed text selection stays a selection.
        // Same click-vs-drag rule as the card's own handler: pointer travel and
        // double-clicks mean "selecting", anything else focuses — a stale page
        // selection must never swallow the click (it used to make the terminal
        // feel randomly dead on user-select:none spots).
        var stageDownX = 0, stageDownY = 0;
        document.addEventListener('mousedown', function (e) { stageDownX = e.clientX; stageDownY = e.clientY; });
        document.addEventListener('click', function (e) {
            if (!shellPinned || !idleCard || !idleCard.el) return;
            // Only in Manual: the pill switches to it, never a click (owner, 2026-10-17).
            var pill = document.getElementById('agentModeWrap');
            if (!pill || pill.dataset.shellInput !== 'manual') return;
            var t = e.target;
            if (!t || !t.closest || !t.closest('#cliContainer')) return;
            if (idleCard.el.contains(t)) return;   // the card's own handler owns this click
            if (e.detail > 1) return;                                                    // dbl/triple = select
            if (Math.abs(e.clientX - stageDownX) > 4 || Math.abs(e.clientY - stageDownY) > 4) return;  // drag
            if (idleCard.el._focusPrompt) idleCard.el._focusPrompt();
        });
        // New chat = a NEW terminal. chat.js resets the main chat UI, but the
        // shell stage owns its own panel — without this, the old terminal's
        // scrollback, prompt path and typing state all survived into the "fresh"
        // chat. Tear everything down and mount a brand-new idle terminal, which
        // greets. (clearAll can't be reused here: it never wipes the terminal in
        // Shell use.)
        document.addEventListener('chat:new', function () {
            pendingHistory = null;                         // fresh chat — nothing to fill in
            runChat = null;
            if (!shellPinned) {
                // a Shell run's card still on the stage belongs to the old chat (a
                // Computer use coder's is left alone)
                Array.from(cards.keys()).forEach(function (id) { if (isShellCard(cards.get(id))) unmountAgent(id); });
                runEnded = true;
                return;
            }
            var c = getContainer();
            // any live run's backend UI pushes were already invalidated by
            // /api/new-chat; its cards go with the old chat, stale events for
            // dead task ids are simply dropped by the cli* handlers
            cards.forEach(function (card) { if (card && card.dispose) card.dispose(); });
            cards.clear();
            queue.length = 0;
            minionParent.clear();
            runEnded = true;
            clearTimeout(clearTimer); clearTimer = null;   // no deferred wipe into the new terminal
            if (window.shellBot) { window.shellBot.work(false); window.shellBot.minionsClear(); }
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
                // Shell use: the run's answer under its lines, then the terminal waits for
                // the next run with all of it still there (it is the chat's).
                if (shellPinned) {
                    var ended = cards.get(taskId);
                    // only the run your message on screen started answers here (another
                    // chat's run, or a Computer use coder, just goes back to waiting)
                    if (isShellCard(ended) && !runEnded) printAnswer(ended, status, summary, summaryHtml);
                    demote(taskId);
                    return;
                }
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
            // Shell use: a Minion bot comes to stand beside the CoderUse bot
            if (shellPinned && isShellCard(card) && !runEnded && window.shellBot) window.shellBot.minionIn(taskId);
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
            if (window.shellBot) window.shellBot.minionOut(taskId);   // its bot goes (a stale end: nothing)
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
            // Shell use: a run that ended without an answer of its own (it never got
            // going, or the send itself failed) says why, once, as its answer, without
            // the note's leading mark. One that printed its ending already is done.
            if (shellPinned) {
                if (runEnded) return;
                runEnded = true;
                var ended = currentCard();
                if (ended && ended.print) ended.print(String(text || '').replace(/^\W+/, ''), 'cc-answer cc-fresh is-ended', 'type');
                return;
            }
            var card = currentCard();
            if (card && card.note) card.note(text);
        };

        // Reopened shell chat: chat.js hands us the chat's exchanges ({task,
        // done_message, status, done_html} per run). The terminal shows them: each
        // request as a command line, its answer under it. State-based, so it works
        // whichever order it lands in relative to the silent agentmode:set that
        // re-pins the stage (mountIdle fills a terminal made after it). A run going
        // in this terminal is left as it is.
        window.cliShellHistory = function (exchanges) {
            var rows = Array.isArray(exchanges) ? exchanges : [];
            if (cards.size) {
                // a run is going: its own chat keeps it as it is; another chat's rows
                // wait for its end, then take a fresh terminal (demote)
                var other = String(window.currentSessionId || '') !== String(runChat || '');
                pendingHistory = other ? rows : null;
                return;
            }
            runEnded = true;
            // the user asked to SEE a chat: the prompt lets go (the pill says Auto)
            if (idleCard && idleCard.leaveTyping) idleCard.leaveTyping();
            if (idleCard) { pendingHistory = null; fillHistory(idleCard, rows); }
            else pendingHistory = rows;
        };

        // Shell use: your message, the chat's next line the moment you send it
        // (chat_input.js); its run follows under it.
        window.cliShellSend = function (text) {
            if (!shellPinned) return;
            runEnded = false;
            runChat = window.currentSessionId || null;
            pendingGreet = null;                       // you spoke first: no hello under it
            var card = currentCard();
            if (!card) return;
            if (card.leaveTyping) card.leaveTyping();
            if (card.print && String(text || '').trim()) printUser(card, text);
            if (card.toEnd) card.toEnd();              // the chat glides down to it
        };

        // Shell use: the run just started (chat_input.js). A brand-new chat only gets its
        // id now, from the backend: the run is that chat's.
        window.cliShellStarted = function (id) {
            if (runChat == null && !runEnded && id) runChat = id;
        };

        // Shell use: Stop (chat_input.js). The run says so and the terminal is yours
        // again at once; the run's own late end finds no card.
        window.cliShellStop = function () {
            if (!shellPinned) return;
            var ids = Array.from(cards.keys());
            if (!ids.length) {
                if (runEnded) return;
                runEnded = true;
                var waiting = currentCard();
                if (waiting && waiting.print) waiting.print('Stopped.', 'cc-answer cc-fresh is-ended', 'type');
                return;
            }
            ids.forEach(function (id) {
                var card = cards.get(id);
                if (isShellCard(card)) card.print('Stopped.', 'cc-answer cc-fresh is-ended', 'type');
                demote(id);
            });
        };

        // The chat box's Auto / Manual pill (agent_mode.js). Manual: the live line's
        // prompt is yours (you type the commands). Auto: the agent's again.
        document.addEventListener('shellinput:changed', function (e) {
            var manual = ((e.detail || {}).mode === 'manual');
            if (window.shellBot) window.shellBot.manual(manual);
            if (!shellPinned || !idleCard) return;
            if (!manual) { if (idleCard.leaveTyping) idleCard.leaveTyping(); return; }
            var cmd = idleCard.el && idleCard.el.querySelector('.cc-cmd');
            if (idleCard.el && idleCard.el._focusPrompt && document.activeElement !== cmd) idleCard.el._focusPrompt();
        });

        // The bot has come in: a fresh terminal says hello now.
        document.addEventListener('shellbot:settled', function () {
            var card = pendingGreet;
            pendingGreet = null;
            if (card && card === idleCard && !card.el.classList.contains('cc-has-out')) greet(card);
        });

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
            if (window.CliCoderCard && window.CliCoderCard.termIdle) window.CliCoderCard.termIdle();
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
