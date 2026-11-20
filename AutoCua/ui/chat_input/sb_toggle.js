// The picker in the strip above the chat box (chat_input.tsx: #sbPick in
// #agentResponseStrip). Two rows, of which the strip shows the one that is on,
// with the caret of the model and mode pickers. A click turns the caret over
// and the whole strip comes up a little: the other row stands with it, one
// under the other, the tick on the one that is on. Picking one brings the
// strip back down. So does a click elsewhere, or Esc. It holds still while a
// run goes.
//
// Which two rows, by the mode (the strip's data-group):
//   - Coder: Auto / Manual, who types in the terminal. agent_mode.js keeps
//     that and is its one writer: a pick asks it ('shellinput:set'), and the
//     picker shows what the composer's data-shell-input says.
//   - the other modes: AutoCua SB / Full control, where the agent works.
//     AutoCua SB is the sandbox, a desktop of its own, so your mouse,
//     keyboard and screen stay yours. Full control, the default, is your own
//     screen, as it always was. UI only for now: nothing reads it yet. It is
//     kept here; window.agentControl() gives it ('sandbox' or 'full'), and a
//     change fires 'agentcontrol:changed', for the parts that will.
(function () {
    'use strict';

    var control = 'full';

    window.agentControl = function () { return control; };

    function strip() { return document.getElementById('agentResponseStrip'); }
    function composer() { return document.querySelector('.input-area-wrapper'); }

    // The pair the strip shows now, and the side that is on in a pair.
    function group() {
        var box = composer();
        return box && box.dataset.agentMode === 'coder' ? 'input' : 'control';
    }
    function side(pair) {
        if (pair !== 'input') return control;
        var box = composer();
        return box && box.dataset.shellInput === 'manual' ? 'manual' : 'auto';
    }

    // The strip, up with both rows in it, or as it stands.
    function open(yes) {
        var bar = strip();
        if (!bar) return;
        bar.classList.toggle('open', yes);
        var rows = bar.querySelector('.sb-options');
        if (rows) rows.setAttribute('aria-expanded', yes ? 'true' : 'false');
    }

    function paint() {
        var bar = strip();
        if (!bar) return;
        var pair = group();
        if (bar.dataset.group !== pair) open(false);     // another mode's pair: as it stands
        bar.dataset.group = pair;
        bar.dataset.control = control;
        Array.prototype.forEach.call(bar.querySelectorAll('.sb-option'), function (row) {
            var on = row.dataset.value === side(row.dataset.group);
            row.classList.toggle('selected', on);
            row.setAttribute('aria-checked', on ? 'true' : 'false');
        });
    }

    function running() {
        var box = document.querySelector('.chat-input');
        return !!(box && box.classList.contains('agent-active'));
    }

    // A row picked in the open strip.
    function pick(row) {
        if (row.dataset.group === 'input') {
            var box = document.querySelector('.chat-input');
            document.dispatchEvent(new CustomEvent('shellinput:set', { detail: { mode: row.dataset.value } }));
            // Auto hands the chat box back. Manual, also when it was on already:
            // the terminal's prompt takes the focus from this row (cli_stage.js
            // answers the event).
            if (side('input') === 'auto') { if (box && !box.disabled) box.focus(); }
            else document.dispatchEvent(new CustomEvent('shellinput:changed', { detail: { mode: 'manual' } }));
        } else if (row.dataset.value !== control) {
            control = row.dataset.value;
            document.dispatchEvent(new CustomEvent('agentcontrol:changed', { detail: { control: control } }));
        }
        paint();
        // its mark springs once (chat_input.css)
        row.classList.remove('picked');
        void row.offsetWidth;
        row.classList.add('picked');
        setTimeout(function () { row.classList.remove('picked'); }, 450);
    }

    function wire() {
        var bar = strip();
        if (!bar) return;
        Array.prototype.forEach.call(bar.querySelectorAll('.sb-option'), function (row) {
            row.addEventListener('click', function () {
                if (running()) return;
                if (!bar.classList.contains('open')) { open(true); return; }   // the one row on show: up it comes
                pick(row);
                open(false);
            });
        });
        document.addEventListener('click', function (e) {
            if (!bar.contains(e.target)) open(false);
        });
        document.addEventListener('keydown', function (e) {
            if (e.key === 'Escape') open(false);
        });
        // The mode and Coder's Auto / Manual are agent_mode.js's, mirrored on the
        // composer: the picker follows them.
        var box = composer();
        if (box) {
            new MutationObserver(paint).observe(box, {
                attributes: true, attributeFilter: ['data-agent-mode', 'data-shell-input']
            });
        }
        // A run that starts brings it down: the picker holds still till the run ends.
        var input = document.querySelector('.chat-input');
        if (input) {
            new MutationObserver(function () {
                if (running()) open(false);
            }).observe(input, { attributes: true, attributeFilter: ['class'] });
        }
        paint();
    }

    // The strip is in the page from the start (chat_input.tsx); chat_input.js
    // fires this once it has wired the box.
    document.addEventListener('chatinput:ready', wire);
})();
