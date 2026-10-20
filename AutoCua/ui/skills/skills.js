// Skills — two pieces, same fetch-inject pattern as the rest of the app:
//   1) the little scroll ICON, appended inside .agent-mode-wrap (right of the
//      Computer/Mobile use picker), mounted on 'agentmode:ready';
//   2) the STAGE (skills_stage.html), mounted INTO #zoneBigCenter — the full-grid
//      container that shows ONE thing at a time. Clicking the icon reveals the
//      stage (and hides Agent Notes so they never overlap); clicking it again
//      makes the stage vanish from the container.
// Both tabs are live and share ONE panel (list | preview | add): COMPUTER USE
// lists the host platform's skills (/api/skills — windows or mac picked
// server-side), MOBILE USE lists the iOS ones (/api/skills?platform=ios).
// Rows preview on click (Edit/Save), the bin deletes, + opens the add form
// whose Save POSTs the .md and registers it in skills.json.
(function () {
    'use strict';

    function zoneEl()  { return document.getElementById('zoneBigCenter'); }
    function stageEl() { return document.getElementById('skillsStage'); }
    function notesEl() { return document.getElementById('agentNotes'); }

    // Skills is "open" only when the stage AND the zone are active. The stage
    // class alone can go stale (a run start hides the zone without touching the
    // stage), and then the icon's first click would "close" an invisible panel.
    function skillsOpen() {
        var s = stageEl(), z = zoneEl();
        return !!(s && s.classList.contains('active') &&
                  z && z.classList.contains('active'));
    }

    // Whether Agent Notes was on screen when Skills opened — closing Skills
    // must return to that exact state (notes back, or the plain 4-zone grid).
    var notesWereShowing = false;

    function notesShowing() {
        var zone = zoneEl(), notes = notesEl();
        return !!(zone && zone.classList.contains('active') &&
                  notes && notes.style.display !== 'none');
    }

    // Icon clicked while the stage markup was still fetching — remembered so
    // mountStage can honour the click the moment the stage lands.
    var pendingOpen = false;

    // Show Skills in the container — and hide the notes so it's the only thing.
    // Always lands on the default view: Computer use tab, fresh file list.
    function openSkills() {
        var stage = stageEl(), zone = zoneEl();
        if (!stage || !zone) { pendingOpen = true; return; }
        pendingOpen = false;
        notesWereShowing = notesShowing();
        var notes = notesEl();
        if (notes) notes.style.display = 'none';       // one thing at a time
        stage.classList.add('active');
        zone.classList.add('active');
        // Skills owns the screen: the body class stands every OTHER stage down
        // (skills.css). Without it the shell terminal — a higher layer than this
        // container — keeps drawing over the list. See hideSkillsOnly for the undo.
        document.body.classList.add('skills-open');
        resetStage();
    }

    // ── Skills panel: live list / preview / edit / delete / add ─────────
    // One panel holds three sub-views (list | preview | add) that the helpers
    // below swap. The ACTIVE TAB decides which folder service.py serves:
    // Computer -> the host platform (windows / mac), Mobile -> ios.
    var currentTab = 'computer';
    function platformQuery() { return currentTab === 'mobile' ? '?platform=ios' : ''; }

    // The composer's Computer/Mobile picker paints data-mode on its wrap
    // (agent_mode.js); open Skills on the matching tab.
    function composerIsMobile() {
        var wrap = document.getElementById('agentModeWrap') || document.querySelector('.agent-mode-wrap');
        var mode = wrap ? (wrap.dataset.mode || wrap.getAttribute('data-mode') || '') : '';
        return mode === 'mobile';
    }

    var BIN_SVG =
        '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
        'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
        '<path d="M3 6h18"/><path d="M8 6V4a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v2"/>' +
        '<path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/>' +
        '<path d="M10 11v6"/><path d="M14 11v6"/></svg>';

    function computerPanel() {
        var stage = stageEl();
        return stage ? stage.querySelector('.skills-form') : null;
    }

    // Every view change bumps viewToken so an in-flight preview fetch from a
    // PREVIOUS state (user reset the stage, went to add, clicked another file)
    // can tell it's stale and drop its response instead of yanking the view.
    var viewToken = 0;

    function showComputerView(which) {
        var panel = computerPanel();
        if (!panel) return;
        viewToken++;
        panel.querySelectorAll('.skills-view').forEach(function (v) {
            v.classList.toggle('active', v.dataset.view === which);
        });
    }

    // The user's own skills (AutoCua_data/skills/<platform>/) first — those
    // can be edited and deleted; then the defaults shipped with the app
    // (AutoCua/default_skills/<platform>/), preview only.
    var defaultNames = [];
    function renderSkillsList(names, defaults) {
        var list = document.getElementById('skillsList');
        if (!list) return;
        defaultNames = defaults || [];
        list.innerHTML = '';
        if (!names.length && !defaultNames.length) {
            var empty = document.createElement('div');
            empty.className = 'skills-empty';
            empty.textContent = 'No skills yet — click + to add one.';
            list.appendChild(empty);
            return;
        }
        function addRow(name, isDefault) {
            var row = document.createElement('div');
            row.className = 'skills-item' + (isDefault ? ' skills-item-default' : '');

            var open = document.createElement('button');
            open.type = 'button';
            open.className = 'skills-item-name';
            open.textContent = name;
            open.title = 'Preview ' + name;
            open.addEventListener('click', function () { openPreview(name); });
            row.appendChild(open);

            if (isDefault) {
                var tag = document.createElement('span');
                tag.className = 'skills-item-tag';
                tag.textContent = 'default';
                tag.title = 'Shipped with AutoCua — preview only';
                row.appendChild(tag);
            } else {
                var del = document.createElement('button');
                del.type = 'button';
                del.className = 'skills-item-del';
                del.setAttribute('aria-label', 'Delete ' + name);
                del.title = 'Delete';
                del.innerHTML = BIN_SVG;
                del.addEventListener('click', function () { deleteSkill(name); });
                row.appendChild(del);
            }
            list.appendChild(row);
        }
        names.forEach(function (name) { addRow(name, false); });
        defaultNames.forEach(function (name) { addRow(name, true); });
    }

    // Only the LATEST list request may render — two rapid deletes issue two
    // refreshes, and the first response can arrive last (stale listing).
    var listReqId = 0;
    function refreshSkillsList() {
        var id = ++listReqId;
        fetch('/api/skills' + platformQuery())
            .then(function (r) { return r.json(); })
            .then(function (d) { if (id === listReqId) renderSkillsList((d && d.skills) || [], (d && d.defaults) || []); })
            .catch(function () { if (id === listReqId) renderSkillsList([], []); });
    }

    function openPreview(name) {
        var token = ++viewToken;               // a newer click/navigation invalidates us
        function land(content) {
            if (token === viewToken) showPreview(name, content);
        }
        fetch('/api/skills/' + encodeURIComponent(name) + platformQuery())
            .then(function (r) { return r.json(); })
            .then(function (d) {
                land((d && typeof d.content === 'string')
                    ? d.content : 'Couldn’t load this skill.');
            })
            .catch(function () { land('Couldn’t load this skill.'); });
    }

    var currentPreviewName = '';           // file shown in the preview / being edited

    function showPreview(name, content) {
        currentPreviewName = name;
        setEditMode(false);                // fresh preview always starts read-only
        var nameEl = document.getElementById('skillsPreviewName');
        var bodyEl = document.getElementById('skillsPreviewBody');
        var editBtn = document.getElementById('skillsEditBtn');
        if (nameEl) nameEl.textContent = name;
        if (bodyEl) bodyEl.textContent = content;
        // A shipped default is preview only — its file lives in the package.
        if (editBtn) editBtn.hidden = defaultNames.indexOf(name) !== -1;
        showComputerView('preview');
    }

    // Edit mode: swap the read-only <pre> for its textarea twin (seeded with
    // the current text) and Edit for Save in the header. Off restores the pre.
    function setEditMode(on) {
        var pre = document.getElementById('skillsPreviewBody');
        var box = document.getElementById('skillsPreviewEdit');
        var editBtn = document.getElementById('skillsEditBtn');
        var saveBtn = document.getElementById('skillsSaveBtn');
        if (!pre || !box || !editBtn || !saveBtn) return;
        if (on) box.value = pre.textContent;
        pre.hidden = on;
        box.hidden = !on;
        editBtn.hidden = on;
        saveBtn.hidden = !on;
        saveBtn.textContent = 'Save';
        if (on) box.focus();
    }

    function saveEdit() {
        var box = document.getElementById('skillsPreviewEdit');
        var name = currentPreviewName;
        if (!box || !name) return;
        var content = box.value;
        var token = viewToken;             // if the user navigates away before the
                                           // PUT resolves, don't touch the new view
                                           // (the file itself still saved fine)
        fetch('/api/skills/' + encodeURIComponent(name) + platformQuery(), {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ content: content })
        })
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (token !== viewToken) return;
                if (d && d.status === 'saved') {
                    var pre = document.getElementById('skillsPreviewBody');
                    if (pre) pre.textContent = content;
                    setEditMode(false);
                } else {
                    flashSaveError();
                }
            })
            .catch(function () { if (token === viewToken) flashSaveError(); });
    }

    // Failed save: keep the edit (nothing is lost), tell the user on the button.
    function flashSaveError() {
        var btn = document.getElementById('skillsSaveBtn');
        if (!btn) return;
        btn.textContent = 'Couldn’t save';
        setTimeout(function () { btn.textContent = 'Save'; }, 1600);
    }

    // Idempotent server-side; refresh the list whether or not it succeeded.
    function deleteSkill(name) {
        fetch('/api/skills/' + encodeURIComponent(name) + platformQuery(), { method: 'DELETE' })
            .catch(function () {})
            .then(function () { refreshSkillsList(); });
    }

    // Select a tab: paint the segmented control, show that tab's add fields,
    // and land on a fresh list of ITS folder.
    function selectTab(which) {
        var stage = stageEl();
        if (!stage) return;
        currentTab = (which === 'mobile') ? 'mobile' : 'computer';
        stage.querySelectorAll('.skills-tab').forEach(function (t) {
            var on = (t.dataset.tab === currentTab);
            t.classList.toggle('active', on);
            t.setAttribute('aria-selected', on ? 'true' : 'false');
        });
        stage.querySelectorAll('.skills-add-fields').forEach(function (row) {
            row.hidden = (row.dataset.for !== currentTab);
        });
        setEditMode(false);
        clearAddForm();
        var list = document.getElementById('skillsList');
        if (list) list.innerHTML = '';              // no other-platform row is clickable while fetching
        showComputerView('list');
        refreshSkillsList();
    }

    function clearAddForm() {
        ['skillsAddTarget', 'skillsAddApp', 'skillsAddBundle', 'skillsAddText'].forEach(function (id) {
            var el = document.getElementById(id);
            if (el) el.value = '';
        });
        setAddError('');
    }

    function setAddError(msg) {
        var el = document.getElementById('skillsAddError');
        if (el) el.textContent = msg || '';
    }

    function val(id) {
        var el = document.getElementById(id);
        return el ? el.value.trim() : '';
    }

    // Save on the add form: POST the skill + its skills.json mapping for the
    // active tab (the server names the file after the app / domain), then
    // return to the (refreshed) list.
    var addInFlight = false;
    function saveNewSkill() {
        if (addInFlight) return;
        var body = { content: val('skillsAddText') };
        if (currentTab === 'mobile') {
            body.app = val('skillsAddApp');
            body.bundle_id = val('skillsAddBundle');
            if (!body.app) { setAddError('Enter the app name as it appears on the home screen.'); return; }
        } else {
            body.target = val('skillsAddTarget');
            if (!body.target) { setAddError('Enter a link or application name.'); return; }
        }
        if (!body.content) { setAddError('Write the skill text.'); return; }
        setAddError('');
        addInFlight = true;
        var btn = document.getElementById('skillsAddSave');
        if (btn) btn.disabled = true;
        var token = viewToken, tab = currentTab;   // a later navigation must not be yanked
        fetch('/api/skills' + platformQuery(), {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body)
        })
            .then(function (r) { return r.json().then(function (d) { return { ok: r.ok, d: d }; }); })
            .then(function (res) {
                var created = !!(res.ok && res.d && res.d.status === 'created');
                if (token !== viewToken) {          // user moved on: only refresh the list
                    if (created && tab === currentTab) refreshSkillsList();
                    return;
                }
                if (created) {
                    clearAddForm();
                    showComputerView('list');
                    refreshSkillsList();
                } else {
                    setAddError((res.d && res.d.error) || 'Couldn’t save the skill.');
                }
            })
            .catch(function () { if (token === viewToken) setAddError('Couldn’t save the skill.'); })
            .then(function () {
                addInFlight = false;
                if (btn) btn.disabled = false;
            });
    }

    // Default state every time the stage opens: the tab matching the
    // composer's Computer/Mobile picker, list view, fresh fetch.
    function resetStage() {
        var stage = stageEl();
        if (!stage) return;
        var add = stage.querySelector('.skills-add');
        if (add) add.hidden = false;
        selectTab(composerIsMobile() ? 'mobile' : 'computer');
    }

    // Drop the Skills stage but leave the container to whoever else uses it
    // (Agent Notes) — used when the notes want to take the stage back.
    function hideSkillsOnly() {
        var stage = stageEl();
        if (stage) stage.classList.remove('active');
        // Every close path funnels through here, so this is the one place the
        // "Skills owns the screen" class comes off — the shell terminal and the
        // stacking order restore themselves from CSS.
        document.body.classList.remove('skills-open');
        var notes = notesEl();
        if (notes) notes.style.display = '';           // restore the notes' own visibility
    }

    // Close button: put the screen back the way it was before Skills opened.
    // If Agent Notes was showing, leave the zone active (hideSkillsOnly already
    // restored the notes); otherwise hide the zone so the 4-zone grid returns.
    // Never ADDS active — if something else (new run, New chat) hid the zone
    // while Skills was open, closing must not resurrect stale notes.
    function closeSkills() {
        hideSkillsOnly();
        var zone = zoneEl();
        if (zone && !notesWereShowing) {
            zone.classList.remove('active');
            // Re-hide the (empty) notes so they don't ghost through the zone's
            // 0.35s fade-out; every show path runs hideSkillsOnly first, which
            // restores display before the notes are next revealed.
            var notes = notesEl();
            if (notes) notes.style.display = 'none';
        }
        notesWereShowing = false;
    }

    function wireStage() {
        var stage = stageEl();
        if (!stage) return;

        // Tabs — both live; each switches the folder the panel serves.
        var addBtn = stage.querySelector('.skills-add');
        stage.querySelectorAll('.skills-tab').forEach(function (tab) {
            tab.addEventListener('click', function () {
                if (tab.dataset.tab !== currentTab) selectTab(tab.dataset.tab);   // re-clicking the active tab keeps your edits
            });
        });

        // + shows the add form; ← in the preview goes back to the list
        // (dropping any unsaved edit — Save is the explicit keep).
        if (addBtn) addBtn.addEventListener('click', function () { setAddError(''); showComputerView('add'); });
        var addSave = stage.querySelector('#skillsAddSave');
        if (addSave) addSave.addEventListener('click', saveNewSkill);
        stage.querySelectorAll('.skills-back').forEach(function (btn) {
            btn.addEventListener('click', function () {
                setEditMode(false);
                showComputerView('list');
            });
        });

        // Close (×) — identical to clicking the composer icon again: restore
        // whatever the screen was showing before Skills took it.
        var closeBtn = stage.querySelector('#skillsCloseBtn');
        if (closeBtn) closeBtn.addEventListener('click', closeSkills);

        // Preview header: Edit swaps to the editable body, Save writes it back.
        var editBtn = stage.querySelector('#skillsEditBtn');
        var saveBtn = stage.querySelector('#skillsSaveBtn');
        if (editBtn) editBtn.addEventListener('click', function () { setEditMode(true); });
        if (saveBtn) saveBtn.addEventListener('click', saveEdit);

        // Cancel on the add form returns to the file list.
        stage.querySelectorAll('.skills-cancel').forEach(function (btn) {
            btn.addEventListener('click', function () { clearAddForm(); showComputerView('list'); });
        });
    }

    function mountStage() {
        var zone = zoneEl();
        if (!zone || document.getElementById('skillsStage')) return;
        fetch('skills/skills_stage.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('skillsStage')) return;   // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var stage = holder.querySelector('.skills-stage');
                if (!stage) return;
                zone.appendChild(stage);
                wireStage();
                if (pendingOpen) { pendingOpen = false; openSkills(); }
            })
            .catch(function () { /* non-fatal: the stage just won't render */ });
    }

    // Keep the "one thing at a time" rule: any path that shows the Agent Notes
    // first drops the Skills stage. showAgentNotes / showAgentHistory are defined
    // by notes_stage.js (loaded before this) — wrap them; the event-driven show
    // paths (reopen / stop-orb) get listeners.
    function wireCoordination() {
        ['showAgentNotes', 'showAgentHistory'].forEach(function (name) {
            var orig = window[name];
            if (typeof orig === 'function') {
                window[name] = function () { hideSkillsOnly(); return orig.apply(this, arguments); };
            }
        });
        document.addEventListener('chat:opened', hideSkillsOnly);
        // New chat: hideAgentNotes already dropped the zone (its fade-out is
        // running) before 'chat:new' fires, so restoring the notes' display here
        // would let the empty header ghost through the fade. Stage class checked
        // directly — skillsOpen() is false once the zone is off.
        document.addEventListener('chat:new', function () {
            var stage = stageEl();
            var wasOpen = !!(stage && stage.classList.contains('active'));
            hideSkillsOnly();
            if (!wasOpen) return;
            var zone = zoneEl(), notes = notesEl();
            if (notes && !(zone && zone.classList.contains('active'))) {
                notes.style.display = 'none';
            }
        });
        window.addEventListener('message', function (e) {
            if (e.data === 'pcbtn:clicked') hideSkillsOnly();
        });

        // A run STARTING takes the screen back. Same signal notes_stage.js
        // watches: .chat-input gains 'agent-active' the moment send fires.
        // Without this the `skills-open` body class would survive into the run
        // and keep the shell terminal parked off-screen for the whole task.
        var runWatchWired = false;
        function watchRunStart() {
            if (runWatchWired) return;
            var chatInput = document.querySelector('.chat-input');
            if (!chatInput) return;
            runWatchWired = true;
            var running = chatInput.classList.contains('agent-active');
            new MutationObserver(function () {
                var now = chatInput.classList.contains('agent-active');
                if (now === running) return;
                running = now;
                if (now) closeSkills();      // never ADDS active — safe alongside
            }).observe(chatInput, { attributes: true, attributeFilter: ['class'] });
        }
        // .chat-input is itself fetch-injected by chat_input.js.
        document.addEventListener('chatinput:ready', watchRunStart);
        watchRunStart();                     // in case it mounted before us
    }

    function injectIcon() {
        var wrap = document.querySelector('.agent-mode-wrap');
        if (!wrap || document.getElementById('skillsBtn')) return;
        fetch('skills/skills.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('skillsBtn')) return;     // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var btn = holder.querySelector('.skills-btn');
                if (!btn) return;
                wrap.appendChild(btn);                                // right of the trigger
                btn.addEventListener('click', function (e) {
                    e.stopPropagation();                              // don't poke the agent-mode menu
                    if (skillsOpen()) closeSkills(); else openSkills();
                });
                document.dispatchEvent(new CustomEvent('skills:ready'));
            })
            .catch(function () { /* non-fatal: the icon just won't render */ });
    }

    wireCoordination();

    // Stage mounts once the big-center shell exists (big_center_container.js fires
    // 'bigcenter:ready'); the icon waits for the Computer/Mobile use picker.
    document.addEventListener('bigcenter:ready', mountStage);
    if (zoneEl()) mountStage();
    document.addEventListener('agentmode:ready', injectIcon);
    injectIcon();   // in case agentmode:ready already fired
})();
