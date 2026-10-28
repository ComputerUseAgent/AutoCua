// =====================================================================
// The sub-agents' boxes (sub_agents.css). In Computer use every sub-agent
// the run starts gets its own box in the live turn, under the run's tool
// box, one under the other in the order they start: the browser agent's and
// the iOS agent's (their screen, tool chain and tracking progress, fed by the
// backend's sub_* events, ui/service.py) and a coder's (its box card, made by
// cli/cli_stage.js). Each has its agent's bot outside its left at its top. A
// box opens by growing from nothing as it fades in; when its agent is done
// (or stopped) it closes the same way and the boxes under it slide up. A
// box opening or closing at the chat's end takes the chat along
// (chat_scroll.js follow). The live turn is never archived with them.
//   window.subAgents.start(id, kind, task)      a browser ('browser'), iOS ('ios') or Android ('android') agent started
//   window.subAgents.flow(id, event, payload)    its tool chain's events (agent.js createToolFlow)
//   window.subAgents.frame(id, b64)              its screen, a JPEG (base64)
//   window.subAgents.note(id, html)              a scratchpad line for its tracking progress
//   window.subAgents.end(id, status)             it is done or stopped: its box closes
//   window.subAgents.mountCoder(taskId, card, agentId)   a coder's box card, in a box
//   window.subAgents.endCoder(taskId)            the coder is done: its box closes, then card.dispose()
//   window.subAgents.closeAll()                  every box closes at once (Stop)
//   window.subAgents.clear()                     every box goes at once, no animation (a send,
//                                                New chat, a reopen, the run's reply)
// Unknown ids are ignored.
// =====================================================================
(function () {
    'use strict';

    var BOTS = { browser: 'webuse', ios: 'mobileuse', android: 'mobileuse', coder: 'coderuse' };   // ui/bots/<bot>*.webp
    var FLOWS = { browser: 'web', ios: 'mobile', android: 'mobile' };    // their chain's click and scroll logos
    var MOVE_MS = 350;            // a box opens or closes (sub_agents.css .sub-slot)
    var TYPE_MS = 5;              // the tracking progress's typing (tracking_progress.js)
    var CHARS_PER_TICK = 2;

    var boxes = new Map();        // 'b' + id (browser, iOS, Android) or 'c' + taskId (coder) -> box
    var closing = [];             // boxes on their way out

    function div(cls) {
        var d = document.createElement('div');
        d.className = cls;
        return d;
    }
    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }
    function follow() {
        if (window.chatScroll && window.chatScroll.follow) window.chatScroll.follow();
    }
    // A box that grows once it is open (a coder's minion rows and bots): the
    // chat goes along with it too, when it is at its end.
    var sizes = window.ResizeObserver ? new ResizeObserver(function () { follow(); }) : null;

    // The live turn's stack, made on first need.
    function stack(make) {
        var live = document.getElementById('liveTurn');
        if (!live) return null;
        var s = live.querySelector(':scope > .sub-stack');
        if (!s && make) {
            s = div('sub-stack');
            live.appendChild(s);
        }
        return s;
    }

    // A box for one agent, its bot on its left; not on the page yet (open).
    function make(key, kind) {
        var slot = div('sub-slot');
        var el = div('sub-box');
        el.dataset.kind = kind;
        var bot = div('sub-bot');
        bot.setAttribute('aria-hidden', 'true');
        bot.innerHTML = '<img alt=""><img class="eyes" alt=""><img alt="">';
        var imgs = bot.querySelectorAll('img');
        ['', '-eyes', '-shine'].forEach(function (part, i) {
            imgs[i].src = 'bots/' + BOTS[kind] + part + '.webp';
        });
        el.appendChild(bot);
        slot.appendChild(el);
        var b = { key: key, kind: kind, slot: slot, el: el, timers: [], gone: false };
        boxes.set(key, b);
        return b;
    }

    // When the slot's own height transition is over, or a little after it
    // should have been at the latest: fn, once. A new move replaces the last.
    function settle(b, fn) {
        if (b.onMoved) b.slot.removeEventListener('transitionend', b.onMoved);
        clearTimeout(b.moveTimer);
        b.onMoved = function (e) {
            if (e && (e.target !== b.slot || e.propertyName !== 'height')) return;
            b.slot.removeEventListener('transitionend', b.onMoved);
            clearTimeout(b.moveTimer);
            b.onMoved = null;
            fn();
        };
        b.slot.addEventListener('transitionend', b.onMoved);
        b.moveTimer = setTimeout(b.onMoved, MOVE_MS + 150);
    }

    // At the stack's end: it grows from nothing to its height (the gap
    // above it included) and fades in, then is as tall as it is again.
    function open(b) {
        var s = stack(true);
        if (!s) return;
        if (sizes) sizes.observe(b.slot);
        if (reduced()) {
            s.appendChild(b.slot);
            follow();
            return;
        }
        b.slot.style.height = '0px';
        b.slot.style.opacity = '0';
        s.appendChild(b.slot);
        var h = b.el.getBoundingClientRect().height + (parseFloat(getComputedStyle(b.el).marginTop) || 0);
        b.slot.style.height = h + 'px';
        b.slot.style.opacity = '1';
        settle(b, function () {
            b.slot.style.height = '';
            b.slot.style.opacity = '';
        });
        follow();
    }

    // Its height pinned where it is (it may still be opening), then to
    // nothing as it fades; gone once there.
    function close(b) {
        if (!b || b.gone || closing.indexOf(b) >= 0) return;
        if (boxes.get(b.key) === b) boxes.delete(b.key);
        closing.push(b);
        var h = b.slot.isConnected ? b.slot.getBoundingClientRect().height : 0;
        if (reduced() || h < 0.5) return drop(b);
        b.slot.style.height = h + 'px';
        b.slot.style.opacity = getComputedStyle(b.slot).opacity;
        void b.slot.offsetHeight;                 // from here
        b.slot.style.height = '0px';
        b.slot.style.opacity = '0';
        settle(b, function () { drop(b); });
        follow();
    }

    // Off the page, its timers stopped, its chain and card disposed: once.
    function drop(b) {
        if (b.gone) return;
        b.gone = true;
        if (sizes) sizes.unobserve(b.slot);
        if (b.onMoved) b.slot.removeEventListener('transitionend', b.onMoved);
        clearTimeout(b.moveTimer);
        b.timers.forEach(clearTimeout);
        if (b.tree) b.tree.querySelectorAll('.br-word').forEach(function (w) { clearTimeout(w._typeTimer); });
        if (b.slot.parentNode) b.slot.parentNode.removeChild(b.slot);
        unmirror(b);
        if (boxes.get(b.key) === b) boxes.delete(b.key);
        var i = closing.indexOf(b);
        if (i >= 0) closing.splice(i, 1);
        // Another component's: one that throws must not keep the rest here.
        try {
            if (b.flow) b.flow.dispose();
            if (b.card && b.card.dispose) b.card.dispose();
        } catch (e) {
            console.error('sub-agent box dispose:', e);
        }
    }

    // ── The tracking progress: a small copy of tracking_progress.js ──────
    // Each scratchpad line types in on a dot on the line, the column sliding
    // up as it fills. Lines come as rendered Markdown (ui/markdown.py): the
    // plain text types, then the markup is swapped in.
    function typeText(el, text, onTick, markup) {
        clearTimeout(el._typeTimer);
        el.textContent = '';
        var i = 0;
        (function tick() {
            i = Math.min(i + CHARS_PER_TICK, text.length);
            el.textContent = text.slice(0, i);
            if (i >= text.length) {
                if (markup) {
                    el.innerHTML = markup;                // already escaped server-side
                    el.classList.add('md');
                }
                onTick();
                return;
            }
            onTick();                                     // it may wrap: keep its end in view
            el._typeTimer = setTimeout(tick, TYPE_MS);
        })();
    }
    function htmlToText(markup) {
        var d = document.createElement('div');
        d.innerHTML = markup;
        return (d.textContent || '').trim();
    }
    // A leading "N. " numbering goes: the dot replaces it.
    function stripNum(s) {
        var dot = s.indexOf('. ');
        if (dot > 0 && /^\d+$/.test(s.slice(0, dot))) return s.slice(dot + 2);
        return s;
    }
    function pinTrack(b) {
        if (!b.tree || !b.trackFlow) return;
        var over = Math.max(0, b.tree.offsetHeight - b.trackFlow.clientHeight);
        b.tree.style.transform = 'translateY(' + (-over) + 'px)';
    }
    function addNote(b, text, markup) {
        var item = div('br-item');
        var dot = document.createElement('span');
        dot.className = 'br-circle';
        var word = document.createElement('span');
        word.className = 'br-word';
        item.appendChild(dot);
        item.appendChild(word);
        b.tree.appendChild(item);
        pinTrack(b);
        item.classList.add('line-in');                    // the line comes down
        b.timers.push(setTimeout(function () {
            item.classList.add('icon-in');                // the dot, then its words
            typeText(word, text, function () { pinTrack(b); }, markup);
        }, 320));
    }
    window.addEventListener('resize', function () { boxes.forEach(pinTrack); });

    // ── Its screen: the run's screen stage, its size and its look; the
    //    browser's window or the phone fits inside it (sub_agents.css)
    function showFrame(b) {
        b.screen.classList.add('has-frame');
    }
    // Its moving background is the run's own (its screen box's video,
    // container/screen/run_bot.js), drawn into its canvas at each new frame: a
    // second copy of the video playing made the whole page stutter (about 17
    // frames a second in Electron, 2026-10-18).
    function mirror(b) {
        var video = document.querySelector('#screenContainer .cu-bg');
        if (!video) return;
        var ctx = b.bg.getContext('2d');
        var rvfc = typeof video.requestVideoFrameCallback === 'function';
        b.bgVideo = video;
        b.bgRvfc = rvfc;
        (function draw() {
            b.bgCb = 0;
            if (b.gone) return;
            if (video.readyState >= 2) ctx.drawImage(video, 0, 0, b.bg.width, b.bg.height);
            b.bgCb = rvfc ? video.requestVideoFrameCallback(draw) : requestAnimationFrame(draw);
        })();
    }
    function unmirror(b) {
        if (!b.bgCb) return;
        if (b.bgRvfc) b.bgVideo.cancelVideoFrameCallback(b.bgCb);
        else cancelAnimationFrame(b.bgCb);
        b.bgCb = 0;
    }

    window.subAgents = {
        start: function (id, kind, task) {
            var key = 'b' + id;
            if (!FLOWS[kind] || boxes.has(key) || !stack(true)) return;
            var b = make(key, kind);
            if (task) b.el.setAttribute('aria-label', String(task));
            var parts = document.createElement('div');
            // Its screen is the run's screen stage: its screenshot on the moving
            // background (sub_agents.css)
            parts.innerHTML =
                '<div class="sub-screen"><canvas class="sub-bg" width="448" height="252" aria-hidden="true"></canvas>' +
                '<div class="sub-shot"><img class="sub-frame" alt=""></div></div>' +
                '<div class="bl-panel tool-panel"><div class="tool-flow" aria-hidden="true"><div class="tool-flow-tree"></div></div></div>' +
                '<div class="bl-panel tracking-panel"><div class="scratchpad-label">tracking progress</div>' +
                '<div class="br-flow" aria-hidden="true"><div class="br-tree"></div></div></div>';
            while (parts.firstChild) b.el.appendChild(parts.firstChild);
            b.screen = b.el.querySelector('.sub-screen');
            b.img = b.screen.querySelector('.sub-frame');
            b.img.onload = function () { showFrame(b); };
            b.bg = b.screen.querySelector('.sub-bg');
            if (b.bg) mirror(b);
            b.track = b.el.querySelector('.tracking-panel');
            b.trackFlow = b.track.querySelector('.br-flow');
            b.tree = b.track.querySelector('.br-tree');
            open(b);
            // Its own chain, in the shapes of its agent's tools (agent.js).
            if (window.createToolFlow) b.flow = window.createToolFlow(b.el.querySelector('.tool-panel'), { kind: FLOWS[kind] });
        },
        flow: function (id, event, payload) {
            var b = boxes.get('b' + id);
            if (b && b.flow) b.flow.onFlow(event, payload);
        },
        frame: function (id, b64) {
            var b = boxes.get('b' + id);
            if (!b || !b.img || !b64) return;
            // Sniffed like the run's own screen (script.js updateAgentImage).
            var mime = b64.startsWith('iVBORw0KGgo') ? 'image/png' : 'image/jpeg';
            b.img.src = 'data:' + mime + ';base64,' + b64;
        },
        note: function (id, html) {
            var b = boxes.get('b' + id);
            if (!b || !b.tree) return;
            var markup = String(html == null ? '' : html);
            var text = stripNum(htmlToText(markup));
            if (!text) return;
            b.track.classList.add('br-active');           // the label and the lines show from the first one
            addNote(b, text, markup);
        },
        end: function (id) {
            close(boxes.get('b' + id));
        },
        mountCoder: function (taskId, card, agentId) {
            var key = 'c' + taskId;
            if (!card || !card.el || boxes.has(key) || !stack(true)) return;
            var b = make(key, 'coder');
            b.card = card;
            if (agentId != null) b.el.dataset.agent = String(agentId);
            b.el.appendChild(card.el);
            open(b);
        },
        endCoder: function (taskId) {
            close(boxes.get('c' + taskId));
        },
        closeAll: function () {
            Array.from(boxes.values()).forEach(close);
        },
        clear: function () {
            Array.from(boxes.values()).concat(closing).forEach(drop);
            var s = stack(false);
            if (s) s.parentNode.removeChild(s);
        }
    };
})();
