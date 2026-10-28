// Connect Device — Settings section for phone automation.
//   • iPhone (iOS): clean pairing flow in the app's own theme, driving the
//     existing setup.py backend. Two dropdowns (Apple Account — always with a
//     "＋ Add Apple Account…" entry — and Device), one status line, one action.
//     PAIRED = the AutoCua app is installed on the phone:
//       - already installed            -> "Device paired successfully" (no build)
//       - not installed  -> Pair       -> sign/build/install via setup.py /run
//       - trust needed   -> show steps -> after trusting we RE-CHECK installed
//                                         (the app is already on the phone; no
//                                         rebuild loop) -> paired
//   • Android: the same flow with no account and nothing to install (second
//     block below). One Device dropdown, one status line, one action.
//
// Backend: POST /api/ios/setup-server (Flask) boots setup.py's server; then we
// talk to it directly (CORS): /detect, /installed, /run (SSE), /add-account, /stop.
(function () {
    'use strict';

    var els = {};
    var base = null;            // setup server URL (http://127.0.0.1:8765)
    var es = null;              // active /run EventSource
    var acctTimer = null;       // add-account poll
    var teams = [];             // usable teams from /detect
    var devices = [];           // connected devices from /detect
    var chosen = { team: null, udid: null };

    function $(id) { return document.getElementById(id); }

    // ---- tiny UI helpers ----
    function status(state, text) {
        els.status.setAttribute('data-state', state);
        els.statusText.textContent = text;
        var busy = (state === 'loading' || state === 'pairing' || state === 'waiting');
        els.spinner.hidden = !busy;
        els.check.hidden = (state !== 'paired');
    }
    function steps(html) { els.steps.hidden = !html; els.steps.innerHTML = html || ''; }
    function action(label, handler, ghost) {
        els.action.hidden = !label;
        els.action.textContent = label || '';
        els.action.disabled = false;
        els.action.classList.toggle('ghost', !!ghost);
        els.action.onclick = handler || null;
    }
    function noAction() { action(null, null); }
    function esc(s) { return String(s).replace(/[&<>"]/g, function (c) {
        return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }

    // ---- dropdowns ----
    function closeMenus() {
        els.teamSelect.classList.remove('is-open');
        els.deviceSelect.classList.remove('is-open');
    }
    function renderTeamMenu() {
        var m = els.teamMenu;
        m.innerHTML = '';
        teams.forEach(function (t) {
            var it = document.createElement('div');
            it.className = 'dc-menu-item' + (t.team_id === chosen.team ? ' selected' : '');
            it.textContent = t.label + ' — ' + t.team_id;
            it.addEventListener('click', function (e) {
                e.stopPropagation();
                chosen.team = t.team_id;
                paintSelections();
                closeMenus();
                afterSelectionChange();
            });
            m.appendChild(it);
        });
        // Always offer adding another account (mirrors Xcode).
        var add = document.createElement('div');
        add.className = 'dc-menu-item add';
        add.textContent = '＋ Add Apple Account…';
        add.addEventListener('click', function (e) {
            e.stopPropagation();
            closeMenus();
            addAccount();
        });
        m.appendChild(add);
    }
    function renderDeviceMenu() {
        var m = els.deviceMenu;
        m.innerHTML = '';
        if (!devices.length) {
            m.innerHTML = '<div class="dc-menu-empty">No iPhone found — plug it in over USB</div>';
            return;
        }
        devices.forEach(function (d) {
            var it = document.createElement('div');
            it.className = 'dc-menu-item' + (d.udid === chosen.udid ? ' selected' : '');
            it.textContent = d.name + ' · iOS ' + d.version;
            it.addEventListener('click', function (e) {
                e.stopPropagation();
                chosen.udid = d.udid;
                paintSelections();
                closeMenus();
                afterSelectionChange();
            });
            m.appendChild(it);
        });
    }
    function paintSelections() {
        var t = teams.find(function (x) { return x.team_id === chosen.team; });
        els.teamValue.textContent = t ? (t.label + ' — ' + t.team_id) : 'No account — add one';
        els.teamValue.classList.toggle('placeholder', !t);
        var d = devices.find(function (x) { return x.udid === chosen.udid; });
        els.deviceValue.textContent = d ? (d.name + ' · iOS ' + d.version) : 'No device';
        els.deviceValue.classList.toggle('placeholder', !d);
        renderTeamMenu();
        renderDeviceMenu();
    }

    // ---- flow ----
    function begin() {
        status('loading', 'Starting…');
        steps(''); noAction();
        loadPaired();                       // show the paired list immediately
        fetch('/api/ios/setup-server', { method: 'POST' })
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (!d.url) return fail(d.error || 'Could not start the pairing service.');
                base = d.url;
                refresh();
            })
            .catch(function () { fail('The desktop app backend is not responding.'); });
    }

    function refresh() {
        status('loading', 'Looking for your iPhone…');
        steps(''); noAction();
        fetch(base + '/detect').then(function (r) { return r.json(); }).then(function (d) {
            if (!d.xcode) return fail('Xcode isn’t installed on this Mac — it’s required to pair an iPhone.');
            teams = (d.teams || []).filter(function (t) { return t.signed_in !== false; });
            devices = (d.devices || []).filter(function (x) { return x.connected !== false; });

            if (chosen.team && !teams.some(function (t) { return t.team_id === chosen.team; })) chosen.team = null;
            if (!chosen.team && teams.length) chosen.team = teams[0].team_id;
            if (chosen.udid && !devices.some(function (x) { return x.udid === chosen.udid; })) chosen.udid = null;
            if (!chosen.udid && devices.length) chosen.udid = devices[0].udid;
            paintSelections();

            afterSelectionChange();
        }).catch(function () { fail('Couldn’t reach the pairing service.'); });
    }

    // Whatever changed (initial detect, dropdown pick, account added): decide
    // the state. THE PAIRED LIST (paired_devices.json) IS THE TRUTH — a device
    // in the list shows paired until it's deleted; a device not in the list is
    // pairable, full stop.
    function afterSelectionChange() {
        if (!devices.length) {
            status('idle', 'No iPhone found');
            steps('Plug your iPhone into this Mac with a USB cable, unlock it, and tap <b>Trust</b> if asked.');
            action('Try again', refresh, true);
            return;
        }
        if (!chosen.team) {
            status('idle', 'Add your Apple Account');
            steps('Pairing signs the app with <b>your own</b> Apple ID — pick “＋ Add Apple Account…” in the Apple Account menu above.');
            action('Add Apple Account', addAccount);
            return;
        }
        fetch('/api/ios/paired')
            .then(function (r) { return r.json(); })
            .then(function (d) {
                var list = d.devices || [];
                renderPaired(list);
                if (list.some(function (x) { return x.udid === chosen.udid; })) paired(false);
                else readyToPair();
            })
            .catch(function () { readyToPair(); });
    }

    function readyToPair() {
        status('idle', 'Ready to pair');
        steps('');
        action('Pair device', pair);
    }

    function pair() {
        if (!chosen.udid || !chosen.team) return refresh();
        status('pairing', 'Signing…');
        steps(''); noAction();
        closeStream();
        var qs = new URLSearchParams({
            team: chosen.team, udid: chosen.udid,
            prefix: 'com.AutoCua', mode: 'test', mount: '1'
        }).toString();
        es = new EventSource(base + '/run?' + qs);

        es.addEventListener('log', function (e) {
            var line = e.data || '';
            if (/editing project\.pbxproj|wrote project\.pbxproj/.test(line)) status('pairing', 'Signing…');
            else if (/mounting developer disk image/.test(line)) status('pairing', 'Preparing device…');
            else if (/^xcodebuild (test|build-for-testing)/.test(line)) status('pairing', 'Installing… (this can take a minute)');
        });
        es.addEventListener('serverup', function () {
            closeStream();
            fetch(base + '/stop').catch(function () {});   // pairing done; end the test run
            paired(true);
        });
        es.addEventListener('done', function (e) {
            var code = -1; try { code = JSON.parse(e.data).code; } catch (x) {}
            closeStream();
            // Success OR failure: the truth is whether the app landed on the
            // phone. (A failed LAUNCH after a good install must still pair.)
            checkInstalledThenMaybeFail(code === 0 ? null : 'Pairing finished with errors.');
        });
        es.addEventListener('needaccount', function () {
            closeStream();
            status('idle', 'That account isn’t signed into Xcode');
            steps('Pick “＋ Add Apple Account…” in the Apple Account menu, or sign the account back into Xcode.');
            action('Add Apple Account', addAccount);
        });
        es.addEventListener('needtrust', function (e) {
            var dev = 'your Apple Development certificate', phone = 'your iPhone';
            try { var j = JSON.parse(e.data); dev = j.developer || dev; phone = j.device || phone; } catch (x) {}
            closeStream();
            needTrust(dev, phone);
        });
        es.addEventListener('fatal', function (e) { closeStream(); fail(e.data || 'Pairing failed.'); });
        es.onerror = function () { /* transient; terminal events above drive the UI */ };
    }

    // The app is ALREADY INSTALLED when trust is requested — after the user
    // trusts, just verify and finish. Never rebuild here.
    function needTrust(dev, phone) {
        status('idle', 'One more step — trust AutoCua');
        steps('The app is installed on <b>' + esc(phone) + '</b>. iOS needs you to trust the developer once:' +
            '<ol>' +
            '<li>On the iPhone open <b>Settings → General → VPN &amp; Device Management</b></li>' +
            '<li>Tap <b>' + esc(dev) + '</b>, then <b>Trust</b></li>' +
            '</ol>');
        action('I’ve trusted it — finish', function () { checkInstalledThenMaybeFail('Trust it on the iPhone, then try again.'); });
    }

    function checkInstalledThenMaybeFail(failMsg) {
        status('loading', 'Checking the device…');
        noAction();
        fetch(base + '/installed?udid=' + encodeURIComponent(chosen.udid))
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (d.installed) paired(true);
                else if (failMsg) fail(failMsg);
                else readyToPair();
            })
            .catch(function () { failMsg ? fail(failMsg) : readyToPair(); });
    }

    function addAccount() {
        status('waiting', 'Waiting for sign-in in Xcode…');
        steps('Xcode is opening — sign in with your Apple ID there (it may ask for a 2-factor code). This page updates by itself.');
        noAction();
        fetch(base + '/add-account').catch(function () {});
        var known = {};
        teams.forEach(function (t) { known[t.team_id] = true; });
        var t0 = Date.now();
        if (acctTimer) clearInterval(acctTimer);
        acctTimer = setInterval(function () {
            if (Date.now() - t0 > 300000) {
                clearInterval(acctTimer); acctTimer = null;
                status('idle', 'Still waiting for the sign-in');
                steps('Finish signing in inside Xcode, then press the button.');
                action('Check again', addAccount, true);
                return;
            }
            fetch(base + '/detect').then(function (r) { return r.json(); }).then(function (d) {
                var now = (d.teams || []).filter(function (t) { return t.signed_in !== false; });
                var fresh = now.find(function (t) { return !known[t.team_id]; });
                teams = now;
                devices = (d.devices || []).filter(function (x) { return x.connected !== false; });
                if (!chosen.udid && devices.length) chosen.udid = devices[0].udid;
                if (fresh) {
                    clearInterval(acctTimer); acctTimer = null;
                    chosen.team = fresh.team_id;
                    paintSelections();
                    afterSelectionChange();     // -> installed check -> paired / ready
                } else if (!chosen.team && teams.length) {
                    // account list refreshed with a usable team (e.g. re-signed in)
                    clearInterval(acctTimer); acctTimer = null;
                    chosen.team = teams[0].team_id;
                    paintSelections();
                    afterSelectionChange();
                }
            }).catch(function () {});
        }, 3000);
    }

    // record=true only when a pairing actually completed (fresh install/trust);
    // the registry-driven display path passes false and never re-adds.
    function paired(record) {
        status('paired', 'Device paired successfully');
        steps('');
        noAction();
        if (!record) return;
        // Record it in paired_devices.json — from now on the Apple logo in the
        // chat box activates this device directly (no checks, no reinstall).
        var d = devices.find(function (x) { return x.udid === chosen.udid; }) || {};
        fetch('/api/ios/paired/add', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ udid: chosen.udid, name: d.name || 'iPhone', version: d.version || '' })
        }).then(function (r) { return r.json(); })
          .then(function (res) { renderPaired(res.devices || []); })
          .catch(function () { loadPaired(); });
    }

    // ---- paired-devices list (paired_devices.json) ----
    function loadPaired() {
        fetch('/api/ios/paired')
            .then(function (r) { return r.json(); })
            .then(function (d) { renderPaired(d.devices || []); })
            .catch(function () { renderPaired([]); });
    }
    function renderPaired(list) {
        els.paired.hidden = !list.length;
        els.pairedList.innerHTML = '';
        list.forEach(function (d) {
            var row = document.createElement('div');
            row.className = 'dc-paired-row';
            var name = document.createElement('span');
            name.className = 'dc-paired-name';
            name.textContent = d.name + (d.version ? ' · iOS ' + d.version : '');
            var udid = document.createElement('span');
            udid.className = 'dc-paired-udid';
            udid.textContent = (d.udid || '').slice(0, 8) + '…';
            udid.title = d.udid || '';
            var rm = document.createElement('button');
            rm.className = 'dc-paired-remove';
            rm.type = 'button';
            rm.title = 'Forget this device';
            rm.textContent = '✕';
            rm.addEventListener('click', function () {
                fetch('/api/ios/paired/remove', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ udid: d.udid })
                }).then(function (r) { return r.json(); })
                  .then(function (res) {
                      renderPaired(res.devices || []);
                      // forgetting the selected device: it's no longer "paired"
                      if (d.udid === chosen.udid) afterSelectionChange();
                  })
                  .catch(function () {});
            });
            row.appendChild(name);
            row.appendChild(udid);
            row.appendChild(rm);
            els.pairedList.appendChild(row);
        });
    }

    function fail(msg) {
        status('error', msg || 'Something went wrong');
        steps('');
        action('Try again', refresh, true);
    }

    function closeStream() { if (es) { es.close(); es = null; } }
    function cleanup() {
        closeStream();
        if (acctTimer) { clearInterval(acctTimer); acctTimer = null; }
    }

    // ---- iPhone row expand / collapse ----
    function toggleiOS() {
        var open = els.optioniOS.classList.toggle('is-open');
        els.optioniOS.setAttribute('aria-expanded', open ? 'true' : 'false');
        els.paneliOS.hidden = !open;
        if (open) begin();
        else cleanup();
    }

    function wire() {
        els.optioniOS = $('deviceOptioniOS');
        els.paneliOS = $('devicePaneliOS');
        els.teamSelect = $('dcTeamSelect');
        els.teamValue = $('dcTeamValue');
        els.teamMenu = $('dcTeamMenu');
        els.deviceSelect = $('dcDeviceSelect');
        els.deviceValue = $('dcDeviceValue');
        els.deviceMenu = $('dcDeviceMenu');
        els.status = $('dcStatus');
        els.spinner = $('dcSpinner');
        els.check = $('dcCheck');
        els.statusText = $('dcStatusText');
        els.steps = $('dcSteps');
        els.action = $('dcAction');
        els.paired = $('dcPaired');
        els.pairedList = $('dcPairedList');
        if (!els.optioniOS) return;

        els.optioniOS.addEventListener('click', toggleiOS);
        els.optioniOS.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleiOS(); }
        });

        els.teamSelect.addEventListener('click', function (e) {
            e.stopPropagation();
            var was = els.teamSelect.classList.contains('is-open');
            closeMenus();
            if (!was) els.teamSelect.classList.add('is-open');
        });
        els.deviceSelect.addEventListener('click', function (e) {
            e.stopPropagation();
            var was = els.deviceSelect.classList.contains('is-open');
            closeMenus();
            if (!was) els.deviceSelect.classList.add('is-open');
        });
        document.addEventListener('click', closeMenus);
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', wire);
    } else {
        wire();
    }
})();

// Android: the iPhone's flow for an Android phone. CONNECTED = Connect read
// the phone over USB (adb), started its helper, read one screen through it,
// and put the phone in paired_devices.json (android_connector):
//   - in the list                  -> "Device connected successfully"
//   - plugged in, not in the list  -> Connect -> check -> connected
//   - Allow not tapped, offline    -> what to do on the phone + Try again
// The Android pick in the chat box then starts the newest listed phone that
// is plugged in (agent_mode/android_session.js).
//
// Backend: GET /api/android/detect, POST /api/android/connect,
// GET /api/android/paired, POST /api/android/paired/remove.
(function () {
    'use strict';

    var els = {};
    var phones = [];            // the phones on USB from /detect, ready or not
    var chosen = null;          // the picked phone's serial
    var gen = 0;                // a new look (or the panel closing) drops older answers

    function $(id) { return document.getElementById(id); }

    // ---- tiny UI helpers (the iPhone block's, on the Android panel) ----
    function status(state, text) {
        els.status.setAttribute('data-state', state);
        els.statusText.textContent = text;
        els.spinner.hidden = !(state === 'loading' || state === 'pairing');
        els.check.hidden = (state !== 'paired');
    }
    function steps(html) { els.steps.hidden = !html; els.steps.innerHTML = html || ''; }
    function action(label, handler, ghost) {
        els.action.hidden = !label;
        els.action.textContent = label || '';
        els.action.disabled = false;
        els.action.classList.toggle('ghost', !!ghost);
        els.action.onclick = handler || null;
    }
    function noAction() { action(null, null); }
    function esc(s) { return String(s).replace(/[&<>"]/g, function (c) {
        return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
    function current() { return phones.find(function (p) { return p.serial === chosen; }) || null; }
    // "moto g77 · Android 16"; a phone that is not ready yet tells only its serial.
    function label(p) {
        return p.model ? p.model + (p.version ? ' · Android ' + p.version : '')
            : p.serial + ' · ' + (p.state === 'unauthorized' ? 'waiting for Allow' : p.state);
    }

    // ---- dropdown ----
    function renderDeviceMenu() {
        var m = els.deviceMenu;
        m.innerHTML = '';
        if (!phones.length) {
            m.innerHTML = '<div class="dc-menu-empty">No Android phone found. Plug it in over USB</div>';
            return;
        }
        phones.forEach(function (p) {
            var it = document.createElement('div');
            it.className = 'dc-menu-item' + (p.serial === chosen ? ' selected' : '');
            it.textContent = label(p);
            it.addEventListener('click', function (e) {
                e.stopPropagation();
                chosen = p.serial;
                paintSelection();
                els.deviceSelect.classList.remove('is-open');
                afterSelectionChange();
            });
            m.appendChild(it);
        });
    }
    function paintSelection() {
        var p = current();
        els.deviceValue.textContent = p ? label(p) : 'No device';
        els.deviceValue.classList.toggle('placeholder', !p);
        renderDeviceMenu();
    }

    // ---- flow ----
    function begin() {
        var my = ++gen;
        status('loading', 'Looking for your Android phone…');
        steps(''); noAction();
        loadPaired();                       // show the connected list immediately
        fetch('/api/android/detect').then(function (r) { return r.json(); }).then(function (d) {
            if (my !== gen) return;
            if (!d.ok) return fail(d.error || 'Could not look for the phone.');
            phones = d.devices || [];
            if (chosen && !phones.some(function (p) { return p.serial === chosen; })) chosen = null;
            if (!chosen && phones.length) {
                var ready = phones.filter(function (p) { return p.state === 'device'; })[0];
                chosen = (ready || phones[0]).serial;
            }
            paintSelection();
            afterSelectionChange();
        }).catch(function () { if (my === gen) fail('The desktop app backend is not responding.'); });
    }

    // THE CONNECTED LIST IS THE TRUTH, as the iPhone's paired list is: a
    // ready phone in the list shows connected until it is forgotten.
    function afterSelectionChange() {
        var p = current();
        if (!p) {
            status('idle', 'No Android phone found');
            steps('Plug your phone into this computer with a USB cable. Then, on the phone:' +
                '<ol>' +
                '<li><b>Settings → About phone</b>: tap <b>Build number</b> 7 times (this turns on Developer options)</li>' +
                '<li><b>Developer options</b>: turn on <b>USB debugging</b></li>' +
                '<li>Tap <b>Allow</b> when the phone asks</li>' +
                '</ol>');
            action('Try again', begin, true);
            return;
        }
        if (p.state !== 'device') {
            status('idle', p.state === 'unauthorized' ? 'Allow USB debugging on the phone' : 'The phone is not ready');
            steps(esc(p.hint || ''));
            action('Try again', begin, true);
            return;
        }
        var my = gen;
        fetch('/api/android/paired')
            .then(function (r) { return r.json(); })
            .then(function (d) {
                if (my !== gen) return;
                var list = d.devices || [];
                renderPaired(list);
                if (list.some(function (x) { return x.serial === chosen; })) connected();
                else readyToConnect();
            })
            .catch(function () { if (my === gen) readyToConnect(); });
    }

    function readyToConnect() {
        status('idle', 'Ready to connect');
        steps('');
        action('Connect device', connect);
    }

    function connect() {
        var p = current();
        if (!p || p.state !== 'device') return begin();
        var my = gen;
        status('pairing', 'Connecting…');
        steps(''); noAction();
        fetch('/api/android/connect', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ serial: p.serial, name: p.model, version: p.version })
        }).then(function (r) { return r.json(); })
          .then(function (d) {
              if (my !== gen) return;
              if (!d.ok) return fail([d.error, d.hint].filter(Boolean).join('. ') || 'Could not connect the phone.');
              renderPaired(d.devices || []);
              connected();
          })
          .catch(function () { if (my === gen) fail('The desktop app backend is not responding.'); });
    }

    function connected() {
        status('paired', 'Device connected successfully');
        steps('');
        noAction();
    }

    // ---- connected-phones list (paired_devices.json) ----
    function loadPaired() {
        fetch('/api/android/paired')
            .then(function (r) { return r.json(); })
            .then(function (d) { renderPaired(d.devices || []); })
            .catch(function () { renderPaired([]); });
    }
    function renderPaired(list) {
        els.paired.hidden = !list.length;
        els.pairedList.innerHTML = '';
        list.forEach(function (d) {
            var row = document.createElement('div');
            row.className = 'dc-paired-row';
            var name = document.createElement('span');
            name.className = 'dc-paired-name';
            name.textContent = d.name + (d.version ? ' · Android ' + d.version : '');
            var serial = document.createElement('span');
            serial.className = 'dc-paired-udid';
            serial.textContent = (d.serial || '').slice(0, 8) + ((d.serial || '').length > 8 ? '…' : '');
            serial.title = d.serial || '';
            var rm = document.createElement('button');
            rm.className = 'dc-paired-remove';
            rm.type = 'button';
            rm.title = 'Forget this device';
            rm.textContent = '✕';
            rm.addEventListener('click', function () {
                fetch('/api/android/paired/remove', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify({ serial: d.serial })
                }).then(function (r) { return r.json(); })
                  .then(function (res) {
                      renderPaired(res.devices || []);
                      // forgetting the picked phone: it is no longer connected
                      if (d.serial === chosen) afterSelectionChange();
                  })
                  .catch(function () {});
            });
            row.appendChild(name);
            row.appendChild(serial);
            row.appendChild(rm);
            els.pairedList.appendChild(row);
        });
    }

    function fail(msg) {
        status('error', msg || 'Something went wrong');
        steps('');
        action('Try again', begin, true);
    }

    // ---- Android row expand / collapse ----
    function toggleAndroid() {
        var open = els.option.classList.toggle('is-open');
        els.option.setAttribute('aria-expanded', open ? 'true' : 'false');
        els.panel.hidden = !open;
        if (open) begin();
        else gen++;                         // answers still on the way are dropped
    }

    function wire() {
        els.option = $('deviceOptionAndroid');
        els.panel = $('devicePanelAndroid');
        els.deviceSelect = $('dcAndroidDeviceSelect');
        els.deviceValue = $('dcAndroidDeviceValue');
        els.deviceMenu = $('dcAndroidDeviceMenu');
        els.status = $('dcAndroidStatus');
        els.spinner = $('dcAndroidSpinner');
        els.check = $('dcAndroidCheck');
        els.statusText = $('dcAndroidStatusText');
        els.steps = $('dcAndroidSteps');
        els.action = $('dcAndroidAction');
        els.paired = $('dcAndroidPaired');
        els.pairedList = $('dcAndroidPairedList');
        if (!els.option || !els.panel) return;

        els.option.addEventListener('click', toggleAndroid);
        els.option.addEventListener('keydown', function (e) {
            if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggleAndroid(); }
        });
        els.deviceSelect.addEventListener('click', function (e) {
            e.stopPropagation();
            els.deviceSelect.classList.toggle('is-open');
        });
        document.addEventListener('click', function () { els.deviceSelect.classList.remove('is-open'); });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', wire);
    } else {
        wire();
    }
})();
