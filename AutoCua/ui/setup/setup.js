/* =============================================================================
   AutoCua — permission setup wizard logic
   Standalone page served at /setup. Hard-gates the main app: walks the four
   macOS permissions one-by-one, auto-advancing as each is detected granted via
   background polling. Navigates to '/' once everything is granted + effective.
   ============================================================================= */
(function () {
    'use strict';

    var POLL_MS = 1500;          // background status poll cadence
    var PREP_MIN_MS = 900;       // minimum "preparing / cleanup" beat
    var STUCK_MS = 20000;        // after this on one step, surface extra help
    var SR_FALLBACK_MS = 6000;   // dev fallback: offer restart for screen recording

    // Per-permission glyphs (stroke = currentColor, inherits row state colors).
    var ICONS = {
        accessibility: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="16" rx="2"/><path d="M6 8h.01M10 8h.01M14 8h.01M18 8h.01M6 12h.01M10 12h.01M14 12h.01M18 12h.01M7 16h10"/></svg>',
        full_disk_access: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 4h11l5 5v11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z"/><path d="M9 4v5h6"/><circle cx="12" cy="14" r="2.5"/></svg>',
        screen_recording: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="2" y="4" width="20" height="13" rx="2"/><path d="M8 21h8M12 17v4"/><circle cx="12" cy="10.5" r="2.5"/></svg>',
        automation: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.6 1.6 0 0 0-1-1.5 1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.6 1.6 0 0 0 1.5-1 1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z"/></svg>'
    };

    // ---- DOM refs -----------------------------------------------------------
    var overlay = document.getElementById('setupOverlay');
    var prepEl = document.getElementById('setupPrep');
    var mainEl = document.getElementById('setupMain');
    var stepsEl = document.getElementById('setupSteps');
    var subtitleEl = document.getElementById('setupSubtitle');
    var progressLabel = document.getElementById('setupProgressLabel');
    var progressFill = document.getElementById('setupProgressFill');
    var launchBtn = document.getElementById('setupLaunchBtn');
    var toastEl = document.getElementById('appToast');

    // ---- local state --------------------------------------------------------
    var perms = [];              // latest catalog/state from the backend
    var needsRelaunch = false;
    var isCompiled = false;
    var acted = {};              // key -> true once the user clicked Grant
    var requestedAt = {};        // key -> timestamp of the click
    var rows = {};               // key -> { el, status, action, note } refs
    var pollTimer = null;
    var inFlight = false;
    var completed = false;
    var toastTimer = null;

    function showToast(msg, ms) {
        if (!toastEl) return;
        toastEl.textContent = msg;
        toastEl.classList.add('active');
        if (toastTimer) clearTimeout(toastTimer);
        toastTimer = setTimeout(function () { toastEl.classList.remove('active'); }, ms || 3200);
    }

    // ---- backend calls ------------------------------------------------------
    function getStatus() {
        return fetch('/api/permissions/status', { cache: 'no-store' }).then(function (r) { return r.json(); });
    }
    function postJSON(url, body) {
        return fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body || {})
        }).then(function (r) { return r.json(); });
    }

    function activeKey() {
        for (var i = 0; i < perms.length; i++) {
            if (!perms[i].granted) return perms[i].key;
        }
        return null;
    }

    function findPerm(key) {
        for (var i = 0; i < perms.length; i++) { if (perms[i].key === key) return perms[i]; }
        return null;
    }
    function labelFor(key) {
        var p = findPerm(key);
        return p ? p.label : key;
    }

    // Should the active step offer a Restart (instead of Grant)? True when the
    // backend reports needs_relaunch, or — dev fallback — the user acted on
    // Screen Recording a while ago and it still reads as not granted.
    function restartMode(key) {
        if (key !== 'screen_recording') return false;
        if (needsRelaunch) return true;
        if (acted[key] && requestedAt[key] && (Date.now() - requestedAt[key] > SR_FALLBACK_MS)) return true;
        return false;
    }

    function settingsDriven(key) {
        return key === 'full_disk_access' || key === 'automation';
    }

    // ---- rendering ----------------------------------------------------------
    function buildRows() {
        stepsEl.innerHTML = '';
        rows = {};
        perms.forEach(function (p) {
            var row = document.createElement('div');
            row.className = 'setup-step';
            row.setAttribute('data-key', p.key);

            var icon = document.createElement('div');
            icon.className = 'setup-step-icon';
            icon.innerHTML = ICONS[p.key] || '';

            var body = document.createElement('div');
            body.className = 'setup-step-body';
            var label = document.createElement('div');
            label.className = 'setup-step-label';
            label.textContent = p.label;
            var desc = document.createElement('div');
            desc.className = 'setup-step-desc';
            desc.textContent = p.description;
            var note = document.createElement('div');
            note.className = 'setup-step-note';
            note.hidden = true;
            body.appendChild(label);
            body.appendChild(desc);
            body.appendChild(note);

            var action = document.createElement('button');
            action.className = 'setup-step-action is-hidden';
            action.addEventListener('click', function () { onActionClick(p.key); });

            var status = document.createElement('div');
            status.className = 'setup-step-status';

            row.appendChild(icon);
            row.appendChild(body);
            row.appendChild(action);
            row.appendChild(status);
            stepsEl.appendChild(row);

            rows[p.key] = { el: row, status: status, action: action, note: note };
        });
    }

    function setStatusEl(el, kind) {
        if (kind === 'done') { el.innerHTML = '<div class="setup-status-check">✓</div>'; }
        else if (kind === 'spin') { el.innerHTML = '<div class="setup-status-spin"></div>'; }
        else { el.innerHTML = '<div class="setup-status-dot"></div>'; }
    }

    function render() {
        var active = activeKey();
        var grantedCount = 0;

        perms.forEach(function (p) {
            var r = rows[p.key];
            if (!r) return;
            var isActive = (p.key === active);
            r.el.classList.remove('is-locked', 'is-active', 'is-done');
            r.note.hidden = true;
            r.note.className = 'setup-step-note';

            if (p.granted) {
                grantedCount++;
                r.el.classList.add('is-done');
                setStatusEl(r.status, 'done');
                r.action.classList.add('is-hidden');
            } else if (isActive) {
                r.el.classList.add('is-active');
                renderActive(p, r);
            } else {
                r.el.classList.add('is-locked');
                setStatusEl(r.status, 'dot');
                r.action.classList.add('is-hidden');
            }
        });

        progressLabel.textContent = grantedCount + ' of ' + perms.length;
        progressFill.style.width = (perms.length ? (grantedCount / perms.length * 100) : 0) + '%';

        if (grantedCount === perms.length && perms.length && !needsRelaunch) {
            onAllGranted();
        }
    }

    function renderActive(p, r) {
        var restart = restartMode(p.key);
        var waiting = acted[p.key] && !restart;
        var stuck = acted[p.key] && requestedAt[p.key] && (Date.now() - requestedAt[p.key] > STUCK_MS);

        setStatusEl(r.status, waiting ? 'spin' : 'dot');

        r.action.classList.remove('is-hidden');
        if (restart) {
            r.action.textContent = 'Restart to finish';
            r.action.disabled = false;
        } else if (waiting && !stuck) {
            r.action.textContent = 'Waiting…';
            r.action.disabled = true;
        } else {
            r.action.textContent = settingsDriven(p.key) ? 'Open Settings' : 'Grant';
            if (stuck) r.action.textContent = settingsDriven(p.key) ? 'Open Settings again' : 'Try again';
            r.action.disabled = false;
        }

        if (restart) {
            r.note.hidden = false;
            r.note.className = 'setup-step-note info';
            r.note.innerHTML = labelFor(p.key) + ' needs a restart to take effect.';
        } else if (stuck) {
            r.note.hidden = false;
            r.note.className = 'setup-step-note info';
            var who = isCompiled ? 'AutoCua' : 'your Terminal / Python';
            r.note.innerHTML = 'Still waiting — open System Settings and turn on <strong>' +
                labelFor(p.key) + '</strong> for ' + who + '.';
        }
    }

    // ---- interactions -------------------------------------------------------
    function onActionClick(key) {
        if (restartMode(key)) { doRelaunch(); return; }
        requestPermission(key);
    }

    function requestPermission(key) {
        acted[key] = true;
        requestedAt[key] = Date.now();
        render(); // immediate "Waiting…" feedback
        postJSON('/api/permissions/request', { permission_key: key }).then(function (res) {
            if (res && res.reset_performed) {
                var r = rows[key];
                if (r) {
                    r.note.hidden = false;
                    r.note.className = 'setup-step-note repair';
                    r.note.innerHTML = 'Cleaned up a leftover entry from a previous install.';
                }
            }
            poll(); // catch a fast grant without waiting a full cycle
        }).catch(function () {
            showToast("Couldn't open System Settings — try again.");
        });
    }

    function doRelaunch() {
        showToast('Restarting AutoCua…', 6000);
        postJSON('/api/app/relaunch', {}).catch(function () {
            showToast("Couldn't restart — please quit and reopen AutoCua.", 6000);
        });
    }

    function onAllGranted() {
        if (completed) return;
        completed = true;
        stopPoll();
        subtitleEl.textContent = "You're all set.";
        launchBtn.hidden = false;
        launchBtn.textContent = 'Launch AutoCua';
        setTimeout(function () { window.location.href = '/'; }, 800);
    }

    // ---- polling ------------------------------------------------------------
    function applyStatus(data) {
        if (!data) return;
        if (data.platform !== 'mac') { window.location.href = '/'; return; }
        perms = data.permissions || [];
        needsRelaunch = !!data.needs_relaunch;
        isCompiled = !!data.is_compiled;
        if (Object.keys(rows).length !== perms.length) buildRows();
        render();
    }

    function poll(force) {
        if (inFlight && !force) return Promise.resolve();
        inFlight = true;
        return getStatus().then(function (data) {
            inFlight = false;
            applyStatus(data);
        }).catch(function () {
            inFlight = false;
        });
    }

    function startPoll() {
        stopPoll();
        pollTimer = setInterval(function () {
            if (document.hidden) return; // don't wake the backend in the background
            poll();
        }, POLL_MS);
    }
    function stopPoll() {
        if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
    }

    // WKWebView won't wheel-scroll an overflow region inside a fixed + transformed
    // card — drive scrollTop manually (same fix as the chat dropdowns).
    function enableWheelScroll(el) {
        if (!el) return;
        el.addEventListener('wheel', function (e) {
            var before = el.scrollTop;
            el.scrollTop += e.deltaY;
            if (el.scrollTop !== before) e.preventDefault();
        }, { passive: false });
    }

    // ---- footer controls ----------------------------------------------------
    document.getElementById('setupRecheck').addEventListener('click', function () {
        var key = activeKey();
        if (key === 'automation') { requestPermission('automation'); } // re-probe
        else { poll(true); }
    });
    document.getElementById('setupResetAll').addEventListener('click', function () {
        showToast('Resetting all permissions…', 6000);
        postJSON('/api/permissions/reset_all', {}).catch(function () {
            showToast("Couldn't reset — try the uninstaller for a clean slate.", 6000);
        });
    });
    document.getElementById('setupQuit').addEventListener('click', function () {
        postJSON('/api/app/quit', {});
    });
    launchBtn.addEventListener('click', function () { window.location.href = '/'; });

    document.addEventListener('visibilitychange', function () {
        if (!document.hidden && !completed) poll(true); // detect a Settings toggle on return
    });

    // ---- boot ---------------------------------------------------------------
    function boot() {
        requestAnimationFrame(function () { overlay.classList.add('visible'); });
        enableWheelScroll(document.querySelector('.setup-card-content'));
        var started = Date.now();
        getStatus().then(function (data) {
            if (!data || data.platform !== 'mac' || (data.all_granted && !data.needs_relaunch)) {
                window.location.href = '/';
                return;
            }
            var wait = Math.max(0, PREP_MIN_MS - (Date.now() - started));
            setTimeout(function () {
                prepEl.hidden = true;
                mainEl.hidden = false;
                applyStatus(data);
                startPoll();
            }, wait);
        }).catch(function () {
            setTimeout(function () {
                prepEl.hidden = true;
                mainEl.hidden = false;
                startPoll();
                poll(true);
            }, PREP_MIN_MS);
        });
    }

    boot();
})();
