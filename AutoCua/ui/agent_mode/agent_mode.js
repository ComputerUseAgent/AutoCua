// Agent mode — injects the composer's bottom-left mode picker (transparent
// trigger showing the selected mode's logo + name, menu floating above with
// Computer use / Mobile use / Shell use / Web use). Always exactly one mode
// selected (default Computer use); picking another swaps trigger label + logo.
// Same fetch-inject pattern as the other components.
//
// Each change dispatches 'agentmode:changed' (detail {mode, sub});
// agent_mode/ios_session.js listens and drives the iPhone's WDA session
// (Mobile use → iOS pairs it; anything else disconnects). 'agentmode:set'
// (detail {mode, sub?}) sets the selection SILENTLY — used by ios_session.js
// to revert the menu when pairing fails.
(function () {
    'use strict';

    function wire(wrap) {
        var btn = wrap.querySelector('#agentBtn');
        var menu = wrap.querySelector('#agentMenu');
        var chev = wrap.querySelector('#agentChev');
        var curLabel = wrap.querySelector('#agentCurLabel');
        var opts = wrap.querySelectorAll('.agent-opt');
        var subopts = wrap.querySelectorAll('.agent-subopt');

        // Two-level selection: the mode, plus each mode's chosen sub-option
        // (remembered per mode). Only Computer use has a default (This PC);
        // Mobile use starts with NO platform picked.
        var state = { mode: 'computer', subs: { computer: 'thispc', mobile: null, shell: null, web: null } };
        var MODE_LABEL = {
            computer: 'Computer use', mobile: 'Mobile use',
            shell: 'Shell use', web: 'Web use'
        };
        var SUB_LABEL = { thispc: 'This PC', daisy: 'Daisy chain', ios: 'iOS', android: 'Android' };

        // Modes with NO sub-menu at all. They're selectable on their own, so
        // they must skip the "pick a sub-option first" gate below that Mobile
        // use relies on. Their sub stays null, which means no sub logo on the
        // trigger and no " on X" suffix in the running placeholder.
        var NO_SUB_MODES = { shell: true, web: true };

        // Per-chat mode lock: once a chat has run in one mode it stays there —
        // the OTHER rows grey out with an "Open a new chat" hover hint.
        // Driven by 'agentmode:lock' (detail {mode:'computer'|'mobile'|'shell'|'web'|null});
        // null unlocks (fresh chat that never ran).
        var lockedMode = null;
        var wrapRows = wrap.querySelectorAll('.agent-opt-wrap');

        function paint() {
            opts.forEach(function (o) {
                var on = (o.dataset.mode === state.mode);
                o.classList.toggle('selected', on);
                o.setAttribute('aria-checked', on ? 'true' : 'false');
            });
            subopts.forEach(function (s) {
                var mode = s.closest('.agent-opt-wrap').dataset.mode;
                var on = (state.subs[mode] === s.dataset.sub);
                s.classList.toggle('selected', on);
                s.setAttribute('aria-checked', on ? 'true' : 'false');
            });
            wrap.dataset.mode = state.mode;                       // mode logo (CSS)
            wrap.dataset.sub = state.subs[state.mode] || 'none';  // sub logo; This PC/none -> nothing
            // Mirror the mode onto the composer so SIBLING controls can react in
            // pure CSS (the picker sits after them, so ~ / + can't reach back).
            // Used by chat_input.css to drop the Fast/Quality toggle in Shell use.
            if (wrap.parentElement) wrap.parentElement.dataset.agentMode = state.mode;
            curLabel.textContent = MODE_LABEL[state.mode];
            Array.prototype.forEach.call(wrapRows, function (w) {
                w.classList.toggle('mode-locked', !!lockedMode && w.dataset.mode !== lockedMode);
            });
        }

        function commit() {
            // Leaving Mobile use drops its platform tick: iOS/Android are only
            // ticked while they ARE the selection (pairing closed = no tick).
            if (state.mode !== 'mobile') state.subs.mobile = null;
            paint();
            // Announced for the future wiring (ios_session etc.); nothing
            // listens yet — connection stays unattached by design.
            document.dispatchEvent(new CustomEvent('agentmode:changed', {
                detail: { mode: state.mode, sub: state.subs[state.mode] }
            }));
            setTimeout(function () { setOpen(false); }, 180);
        }

        var open = false;
        function setOpen(v) {
            open = v;
            btn.setAttribute('aria-expanded', v ? 'true' : 'false');
            menu.classList.toggle('open', v);
            chev.style.transform = v ? 'rotate(180deg)' : 'rotate(0deg)';
        }

        btn.addEventListener('click', function () { setOpen(!open); });

        // Main row: select that mode — but ONLY if it has a sub-option picked.
        // Mobile use has no default platform, so clicking it before choosing
        // iOS/Android selects nothing: the list just closes and the previous
        // selection (Computer use) stands. Computer use always has This PC.
        opts.forEach(function (opt) {
            opt.addEventListener('click', function () {
                var mode = opt.dataset.mode;
                if (lockedMode && mode !== lockedMode) return;   // chat is locked to the other mode
                if (!NO_SUB_MODES[mode] && !state.subs[mode]) {
                    setTimeout(function () { setOpen(false); }, 120);
                    return;               // no platform picked -> no selection change
                }
                state.mode = mode;
                commit();
            });
        });

        // Sub row: select that sub-option AND its parent mode.
        subopts.forEach(function (s) {
            s.addEventListener('click', function (e) {
                e.stopPropagation();
                var mode = s.closest('.agent-opt-wrap').dataset.mode;
                if (lockedMode && mode !== lockedMode) return;   // chat is locked to the other mode
                state.mode = mode;
                state.subs[mode] = s.dataset.sub;
                commit();
            });
        });

        document.addEventListener('click', function (e) {
            if (open && !e.target.closest('.agent-mode-wrap')) setOpen(false);
        });

        // While a run is ACTIVE the collapsed bar shouldn't say "Type your
        // task..." — it shows what the agent is running as instead, e.g.
        // "Agent mode: Computer use on This PC" / "Agent mode: Mobile use on
        // iOS". Restored to the idle text when the run ends. (agent-active is
        // observed passively, same pattern as the other components.)
        var chatInput = document.querySelector('.chat-input');
        if (chatInput) {
            var idlePlaceholder = null;
            var running = chatInput.classList.contains('agent-active');
            new MutationObserver(function () {
                var now = chatInput.classList.contains('agent-active');
                if (now === running) return;
                running = now;
                if (now) {
                    idlePlaceholder = chatInput.placeholder;
                    var sub = state.subs[state.mode];
                    chatInput.placeholder = 'Agent mode: ' + MODE_LABEL[state.mode] +
                        (sub ? ' on ' + SUB_LABEL[sub] : '');
                } else if (idlePlaceholder !== null) {
                    chatInput.placeholder = idlePlaceholder;
                    idlePlaceholder = null;
                }
            }).observe(chatInput, { attributes: true, attributeFilter: ['class'] });
        }

        // Per-chat mode lock (chat.js on reopen/new-chat, chat_input.js on run
        // start). Locking to a mode greys the other row; null unlocks.
        document.addEventListener('agentmode:lock', function (e) {
            var d = e.detail || {};
            // Any real mode locks (the label map is the source of truth — shell
            // included); anything else (null / garbage) unlocks.
            lockedMode = MODE_LABEL[d.mode] ? d.mode : null;
            paint();
        });

        // Programmatic, SILENT selection (no agentmode:changed) — e.g. pairing
        // failed and ios_session.js puts the menu back on Computer use.
        document.addEventListener('agentmode:set', function (e) {
            var d = e.detail || {};
            if (d.mode && MODE_LABEL[d.mode]) {
                state.mode = d.mode;
                if (d.sub !== undefined) state.subs[d.mode] = d.sub;
            }
            if (state.mode !== 'mobile') state.subs.mobile = null;   // same rule as commit()
            paint();
        });

        paint();
    }

    function mount() {
        var wrapper = document.querySelector('.input-area-wrapper');
        if (!wrapper || document.getElementById('agentModeWrap')) return;
        fetch('agent_mode/agent_mode.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('agentModeWrap')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var wrap = holder.querySelector('.agent-mode-wrap');
                if (!wrap) return;
                wrapper.appendChild(wrap);
                wire(wrap);
                document.dispatchEvent(new CustomEvent('agentmode:ready'));
            })
            .catch(function () { /* non-fatal: the control just won't render */ });
    }

    // chat_input.html is itself fetch-injected — mount on its ready event,
    // with a direct attempt in case that already fired.
    document.addEventListener('chatinput:ready', mount);
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
