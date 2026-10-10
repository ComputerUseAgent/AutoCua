// Bottom-right container loader — injects the agent TODO list into #mainGrid
// (column 2, row 2). This file mounts the markup AND owns the live TODO render hook
// (window.updateTodoList), fed by app.py's todo.md watcher. The list visibility is
// driven from script.js via setWorkState() toggling .is-active on #todoState.
(function () {
    'use strict';

    // --- live todo state (per run) ---
    var ROW_PITCH = 30;          // fallback row pitch (~28px row + 2px gap); measured live when possible
    var _lastTodo = null;        // last tasks array we rendered (for interrupt re-render)
    var _interrupted = false;    // true after Stop pressed — freezes the list (✕ shown)

    // Build the rows. stopped=true => every not-done task is marked cancelled (✕).
    function renderTodo(tasks, stopped) {
        var listEl = document.getElementById('todoList');
        if (!listEl) return;

        var firstUndone = -1;
        for (var k = 0; k < tasks.length; k++) {
            if (!tasks[k].done) { firstUndone = k; break; }
        }

        // Update rows IN PLACE when the task count is unchanged (the common case:
        // a task just got checked). Rebuilding innerHTML every update would reset
        // scrollTop to 0, so the "move up" would snap-then-glide instead of gliding
        // smoothly by one row. Only a changed count (first populate / the agent
        // expanded the list) rebuilds.
        var reuse = listEl.children.length === tasks.length;
        if (!reuse) listEl.innerHTML = '';

        tasks.forEach(function (t, i) {
            var item, mark, label;
            if (reuse) {
                item = listEl.children[i];
                mark = item.children[0];
                label = item.children[1];
            } else {
                item = document.createElement('div');
                item.className = 'todo-item';
                mark = document.createElement('span');
                label = document.createElement('span');
                item.appendChild(mark);
                item.appendChild(label);
                listEl.appendChild(item);
            }

            label.className = 'todo-text';
            if (t.done) {
                mark.className = 'todo-check done';
                label.classList.add('is-done');
            } else if (stopped) {
                mark.className = 'todo-cross';            // ✕ — interrupted, was pending
                label.classList.add('is-cancelled');
            } else if (i === firstUndone) {
                mark.className = 'todo-spinner';          // rotating dial — in progress
            } else {
                mark.className = 'todo-check';            // empty box — pending
            }

            var text = t.text || ('task ' + (i + 1));
            if (label.textContent !== text) label.textContent = text;
        });

        // Scroll behaviour (skip entirely once interrupted — keep the ✕ frozen).
        if (stopped) return;
        requestAnimationFrame(function () {
            var doneCount = 0;
            for (var j = 0; j < tasks.length; j++) { if (tasks[j].done) doneCount++; }

            // Stay put until 3 tasks are checked; from the 3rd onward, move up
            // exactly one row per completion (smooth, one at a time). Clamped to
            // the scrollable range, so a list that already fits never moves.
            var rowsUp = Math.max(0, doneCount - 2);

            var pitch = ROW_PITCH;
            if (listEl.children.length >= 2) {
                var measured = listEl.children[1].offsetTop - listEl.children[0].offsetTop;
                if (measured > 0) pitch = measured;
            }

            var maxScroll = listEl.scrollHeight - listEl.clientHeight;
            var target = Math.min(Math.max(0, rowsUp * pitch), Math.max(0, maxScroll));
            listEl.scrollTo({ top: target, behavior: 'smooth' });
        });
    }

    // Render the main agent's real todo list into #todoState. Defined up front
    // (queries the DOM lazily) so it exists before the backend ever calls it,
    // even though the markup is fetch-injected below. Payload (string or object):
    //   { objective: "<goal>", tasks: [ { text: "...", done: true|false }, ... ] }
    // An empty payload is a RESET (new run / cleared): blanks the card and clears
    // the interrupt state.
    window.updateTodoList = function (payload) {
        var data = payload;
        if (typeof payload === 'string') {
            try { data = JSON.parse(payload); } catch (e) { return; }
        }
        if (!data || typeof data !== 'object') return;

        var listEl = document.getElementById('todoList');
        if (!listEl) return;   // markup not injected yet — backend retries on next change

        var tasks = Array.isArray(data.tasks) ? data.tasks : [];

        if (!tasks.length) {
            // Reset for a fresh run: blank, and clear interrupt state.
            _interrupted = false;
            _lastTodo = null;
            listEl.innerHTML = '';
            return;
        }

        // Frozen after a manual Stop until the next run sends an empty reset —
        // don't let the backend's final read overwrite the ✕ marks.
        if (_interrupted) return;

        _lastTodo = tasks.slice();
        renderTodo(tasks, false);
    };

    // Freeze the list and mark every still-pending task with a ✕ (so a spinner
    // doesn't rotate forever). Called from script.js both when the user presses
    // Stop AND on natural completion when the agent called `done` but left a task
    // unfinished (never marked it complete). No-ops if nothing is pending.
    window.markTodoInterrupted = function () {
        if (_interrupted || !_lastTodo || !_lastTodo.length) return;
        var anyPending = _lastTodo.some(function (t) { return !t.done; });
        if (!anyPending) return;   // nothing pending — leave the ticks as-is
        _interrupted = true;
        renderTodo(_lastTodo, true);
    };

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.bottom-right-zone')) return;
        fetch('container/bottom_right/bottom_right.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.bottom-right-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.bottom-right-zone');
                if (zone) grid.appendChild(zone);
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
