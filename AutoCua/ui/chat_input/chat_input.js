// Chat input component — injects the bottom text box (the output / cli /
// agent-activated strip areas + the input bar with its textarea and the stop-agent
// orb) into #chatWrapper, then wires its behaviour: auto-resize, Enter-to-send and
// the stop button. Same self-contained fetch-inject pattern as chat/chat.js and the
// container/* zones.
//
// Shared app state stays owned by script.js and is read through a tiny window
// interface so this file never holds a second copy:
//   • window.getModelSelection() -> { provider, model }  (live selection)
//   • window.stopStreaming()                              (cancel agent-text stream)
//   • window.setWorkState(state)                          (reset the work-state cell)
// Every one of those is only called at user-event time (Enter / send / stop click),
// long after script.js has defined them — so injection order doesn't matter.
//
// Two orb iframes are overlaid in the toolbar's slot and crossfaded (CSS, keyed off
// .chat-input.agent-active): the grey send orb (send_button.html) idle, the glowing
// stop orb (pc_button.html) while running. Enter and the send orb's 'sendbtn:clicked'
// message both call startAgent(); restoreIdle() is the single teardown, also exposed
// as window.chatInputRestoreIdle so script.js's agentComplete() can morph back to idle.
(function () {
    'use strict';

    // Wire the just-injected text box: auto-resize, send (Enter / orb), stop button.
    function wireChatInput(root) {
        const chatInput = (root || document).querySelector('.chat-input');
        if (!chatInput) return;

        // Orb iframes overlaid in the toolbar slot (queried by id; DOM is stable).
        const getSendFrame = () => document.getElementById('sendBtnFrame');
        const getStopFrame = () => document.getElementById('stopBtnFrame');

        // Auto-grow the textarea between its CSS min-height (~2 lines) and the 150px cap.
        // The text now runs full width, so a plain scrollHeight measure is enough — the
        // old compact/expanded mirror machinery is gone. Skipped while a run is active:
        // the box collapses to the slim bar via CSS, so don't pin an inline height.
        const adjustHeight = () => {
            if (chatInput.classList.contains('agent-active')) { chatInput.style.height = ''; return; }
            // Empty box: no inline pin — the CSS idle height rules. Pinning a
            // measured px here (e.g. from restoreIdle, while the collapse
            // transition is still settling) came out a few px tall and snapped
            // down on the next interaction — the visible top-edge jerk.
            if (!chatInput.value) { chatInput.style.height = ''; return; }
            chatInput.style.height = 'auto';
            const cs = getComputedStyle(chatInput);
            const minH = parseFloat(cs.minHeight) || 30;   // floor matches CSS min-height (one line)
            chatInput.style.height = `${Math.min(Math.max(chatInput.scrollHeight, minH), 150)}px`;
        };
        chatInput.addEventListener('input', adjustHeight);
        window.addEventListener('resize', adjustHeight);
        adjustHeight();

        // Start the agent. Shared by Enter and the send orb's 'sendbtn:clicked' message.
        // Guard mirrors the old Enter behaviour: needs a message + a live provider/model.
        function startAgent() {
            const message = chatInput.value.trim();
            // Live (provider, model) selection lives in script.js (Settings writes it).
            const sel = window.getModelSelection ? window.getModelSelection() : {};
            if (!message || !sel.provider || !sel.model) return;

            // Agent mode at send time from the picker's DOM mirror (#agentModeWrap
            // data-mode/data-sub — correct even before interaction or after reverts).
            const modeWrap = document.getElementById('agentModeWrap');
            const agentMode = (modeWrap && modeWrap.dataset.mode) || 'computer';
            const agentSub = (modeWrap && modeWrap.dataset.sub !== 'none') ? modeWrap.dataset.sub : null;
            // Mobile use with no device picked yet (e.g. a reopened mobile chat
            // before re-pairing) — the user must select iOS/Android first.
            if (agentMode === 'mobile' && !agentSub) return;
            // Web use needs no sub-option and no gate: service.py routes
            // mode 'web' to run_pkg 'web' (the browser agent) and /api/start-agent
            // below is the right endpoint for it, same as Computer and Mobile.

            // The moment a task is sent, drop the empty-state hero.
            if (window.hideWelcomeHero) window.hideWelcomeHero();
            // ...and reveal the memory bar (only shown while the agent runs).
            if (window.showMemoryBar) window.showMemoryBar();

            const agentStrip = document.getElementById('agentResponseStrip');
            const agentText = document.getElementById('agentText');
            const sendBtnFrame = getSendFrame();
            const stopBtnFrame = getStopFrame();

            if (agentStrip) {
                agentStrip.classList.add('active');
                chatInput.disabled = true;
                chatInput.classList.add('agent-active');   // CSS: box shrinks + orbs crossfade
                adjustHeight();                            // clear inline height so the collapse is clean
                agentText.textContent = 'Starting agent...';

                // Morph the orb: fade the grey send orb out, bring the glowing stop orb in.
                if (sendBtnFrame) sendBtnFrame.contentWindow.postMessage('sendbtn:hide', '*');
                if (stopBtnFrame) {
                    stopBtnFrame.classList.add('active');
                    stopBtnFrame.contentWindow.postMessage('pcbtn:show', '*');
                }

                // Hide eyes only, keep glow.
                const welcomeEl = document.getElementById('welcomeOverlay');
                if (welcomeEl) welcomeEl.classList.add('eyes-hidden');

                // Start the live tracking-progress stream (top-right), then clear the
                // previous run's visual outputs (shared with the New-chat button).
                if (window.trackingProgress) window.trackingProgress.start();
                resetChatUi();
            }

            // Shell use bypasses the main agent: the task goes STRAIGHT to the
            // coder agent (backend: run_shell_task in service.py) and the CLI
            // stage is the UI it drives — but it IS a chat like the other modes:
            // the backend mints/continues the session, seeds the coder with the
            // chat's saved conversation, drives the memory bar, and returns
            // session_id below. The composer, stop orb and completion teardown
            // are shared between both endpoints.
            const endpoint = (agentMode === 'shell') ? '/api/start-shell' : '/api/start-agent';

            fetch(endpoint, {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                // session_id threads the active chat: null/"new" => fresh start,
                // an existing id => continue that saved session (agent resumes).
                body: JSON.stringify({
                    provider: sel.provider,
                    model: sel.model,
                    task: message,
                    session_id: window.currentSessionId || null,
                    mode: agentMode,
                    os: agentMode === 'mobile' ? agentSub : null,
                    // ⚡/✨ toggle state lives only in the .quality class (read live at send)
                    speed: (flipToggle && !flipToggle.classList.contains('quality')) ? 'fast' : 'quality'
                })
            })
            .then(response => response.json())
            .then(data => {
                if (data.status === 'started') {
                    agentText.textContent = 'Agent running...';
                    chatInput.value = '';   // box is collapsed; restored on teardown
                    // Adopt the backend's session id (a brand-new chat is minted
                    // server-side) so the run-end save + future sends target it.
                    if (data.session_id) window.currentSessionId = data.session_id;
                    // First run commits the chat to this mode — lock the picker.
                    // Shell included: the lock fires only on a STARTED run (this
                    // branch), so merely toggling to Shell use never locks — using
                    // it does. New chat unlocks, same as the other modes.
                    document.dispatchEvent(new CustomEvent('agentmode:lock', { detail: { mode: agentMode } }));
                    // Show the just-created chat in the sidebar immediately.
                    document.dispatchEvent(new CustomEvent('chats:refresh'));
                } else if (data.error) {
                    agentText.textContent = `Error: ${data.error}`;
                    restoreIdle();          // roll the orb morph + box back
                }
            })
            .catch(err => {
                console.error('Failed to start agent:', err);
                agentText.textContent = 'Failed to start agent';
                restoreIdle();
            });
        }

        // Single, idempotent teardown back to the idle composer. Called from the manual
        // stop handler AND from script.js's agentComplete()/agentError() — every line is
        // safe to run twice (remove-absent-class, re-post-show are no-ops).
        function restoreIdle() {
            const sendBtnFrame = getSendFrame();
            const stopBtnFrame = getStopFrame();
            const agentStrip = document.getElementById('agentResponseStrip');

            chatInput.disabled = false;
            chatInput.classList.remove('agent-active');   // CSS: box re-expands + orbs crossfade back

            // Bring the grey send orb back (resumes its iconFlip); drop the stop orb's
            // pointer-events (it plays its own pop-vanish when agentComplete posts pcbtn:vanish).
            if (sendBtnFrame) sendBtnFrame.contentWindow.postMessage('sendbtn:show', '*');
            if (stopBtnFrame) stopBtnFrame.classList.remove('active');

            if (agentStrip) agentStrip.classList.remove('active');

            const welcomeEl = document.getElementById('welcomeOverlay');
            if (welcomeEl) welcomeEl.classList.remove('eyes-hidden');

            adjustHeight();   // restore the ~2-line idle height
        }
        // Expose so script.js's agentComplete() can morph back on natural completion.
        window.chatInputRestoreIdle = restoreIdle;

        // Clear the previous run's visual outputs (milestones, todo card, agent
        // notes). Shared by the send path AND the New-chat button (chat.js) so a
        // fresh chat and a fresh run reset identically. Idempotent / guarded.
        function resetChatUi() {
            const milestoneStream = document.getElementById('milestoneStream');
            if (milestoneStream) milestoneStream.innerHTML = '';
            if (window.updateTodoList) window.updateTodoList({ objective: '', tasks: [] });
            if (window.setWorkState) window.setWorkState('todo');
            // Drop the previous run's "Agent Notes" so the screenshot area is live again.
            if (window.hideAgentNotes) window.hideAgentNotes();
            // Clear the previous run's screenshot + tool-response chain (and any
            // web/shell overlay) so a fresh chat / new run starts on a clean canvas
            // — no stale screen or "Tool response" steps bleeding into the next chat.
            if (window.clearAgentImage) window.clearAgentImage();
            if (window.toolFlow && window.toolFlow.reset) window.toolFlow.reset();
            if (window.webSearchEnd) window.webSearchEnd();
            if (window.shellEnd) window.shellEnd();
        }
        window.resetChatUi = resetChatUi;

        // The orb's click target. The orb frames take no clicks (they are 72px for
        // the glow and overhung the text box and the model picker), so this button
        // over the visible orb presses whichever orb is showing. The orb then pops
        // and posts sendbtn:clicked / pcbtn:clicked exactly as a direct click did,
        // and is skipped while its page has made it unclickable (hidden, vanishing).
        const orbHit = (root || document).querySelector('#orbHit');
        if (orbHit) {
            orbHit.addEventListener('click', () => {
                const frame = chatInput.classList.contains('agent-active') ? getStopFrame() : getSendFrame();
                const doc = frame && frame.contentDocument;
                const orb = doc && doc.getElementById('stopAgentBtn');
                if (!orb || doc.defaultView.getComputedStyle(orb).pointerEvents === 'none') return;
                orb.click();
            });
        }

        // Fast / Quality flip toggle — read by startAgent() as the run's speed mode.
        // Click spring-slides the thumb; state lives in the .quality class. The
        // floating label shows briefly after each flip (and on hover via CSS).
        const flipToggle = (root || document).querySelector('#fastQualityToggle');
        if (flipToggle) {
            let labelTimer = null;
            flipToggle.addEventListener('click', () => {
                flipToggle.classList.toggle('quality');
                flipToggle.setAttribute('aria-checked', flipToggle.classList.contains('quality') ? 'true' : 'false');
                flipToggle.classList.add('show-label');
                clearTimeout(labelTimer);
                labelTimer = setTimeout(() => flipToggle.classList.remove('show-label'), 1200);
            });
        }

        // Recording dot — the backend calls this: on=true once a run's recorder
        // has started (Settings -> Recording), on=false once its videos are
        // stored. The run id keeps a finished recording from hiding the dot of
        // a newer one that started during its last second.
        const recIndicator = (root || document).querySelector('#recIndicator');
        let recRun = null, recTimer = null;
        window.recordingIndicator = (on, runId) => {
            if (!recIndicator) return;
            if (on) {
                recRun = runId;
                clearTimeout(recTimer);
                recIndicator.classList.remove('leaving');
                recIndicator.classList.add('on');
            } else if (runId === recRun) {
                recRun = null;
                recIndicator.classList.remove('on');
                recIndicator.classList.add('leaving');
                recTimer = setTimeout(() => recIndicator.classList.remove('leaving'), 600);
            }
        };

        // Enter (without Shift) starts the agent; Shift+Enter inserts a newline.
        chatInput.addEventListener('keydown', (e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                startAgent();
            }
        });

        // Messages from the orb iframes: the grey send orb posts 'sendbtn:clicked'
        // (same as Enter), the glowing stop orb posts 'pcbtn:clicked' (it also plays
        // its own pop-vanish on click — we just morph back to idle).
        window.addEventListener('message', (e) => {
            if (e.data === 'sendbtn:clicked') {
                startAgent();
                return;
            }
            if (e.data !== 'pcbtn:clicked') return;

            // Stop the agent-text stream immediately (timer is owned by script.js).
            if (window.stopStreaming) window.stopStreaming();

            const agentText = document.getElementById('agentText');
            if (agentText) agentText.textContent = 'Stopping agent...';

            // Force-close any active tool animations immediately.
            if (window.webSearchEnd) window.webSearchEnd();
            if (window.shellEnd) window.shellEnd();
            if (window.trackingProgress) window.trackingProgress.end();   // fade the tracking-progress stream out

            // Interrupted by the user: freeze the todo and mark any still-pending
            // tasks with a ✕ (so a spinner doesn't rotate forever). Stop-only —
            // normal completion leaves the ticks alone.
            if (window.markTodoInterrupted) window.markTodoInterrupted();

            // Cap the tool-flow chain HERE, right now. The backend retires the
            // run the instant Stop is pressed, so the agent's own ending (it may
            // still be parked in an LLM call for seconds) is discarded and never
            // arrives — waiting for it would leave the chain hanging, or worse,
            // let it land on top of the next run the user starts.
            if (window.toolFlow) {
                window.toolFlow.onFlow('error', JSON.stringify({ text: 'agent interrupted' }));
                window.toolFlow.onFlow('run_end');
            }

            fetch('/api/stop-agent', { method: 'POST' })
                .then(res => res.json())
                .then(data => {
                    console.log('Agent stop requested:', data);
                    restoreIdle();
                    chatInput.focus();
                })
                .catch(err => console.error('Error stopping agent:', err));
        });
    }

    function mount() {
        var wrap = document.getElementById('chatWrapper');
        if (!wrap || wrap.querySelector('.chat-container-content')) return;
        fetch('chat_input/chat_input.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (wrap.querySelector('.chat-container-content')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var content = holder.querySelector('.chat-container-content');
                if (!content) return;
                wrap.appendChild(content);
                // The markup is now live — wire it (runs once, post-injection).
                wireChatInput(content);
                // Announce for any future listeners (script.js no longer needs this).
                document.dispatchEvent(new CustomEvent('chatinput:ready'));
            })
            .catch(function () { /* non-fatal: the text box just won't render */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
