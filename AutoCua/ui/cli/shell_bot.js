// =====================================================================
// Shell use bot (shell_bot.css): the CoderUse bot, small, standing on top
// of the chat box in its middle, under the chat with its one-line terminal
// (cli_stage.js) that fills the frame. When Shell use is picked the bot comes in
// the way the run's bot does at the bottom container (container/agent/chain_bot.js,
// itself the splash's hero): big in the middle of the frame it fades in, grows
// from 0.85x out of a 6px blur, wakes and blinks; then it shrinks down onto the
// chat box. Its place is in the chat box's own column, just above the
// "Agent running…" strip, so it goes wherever the box goes (a run's slim
// box and its strip, a long draft, the left bar). While a run goes its
// eyes give way to the run's tool icons, on its face in the eyes' colour
// (the terminal card draws them: coder_card.js faceCanvas -> tool_icons.js
// createFaceIcon). The chat stops a gap above its head. Its own state and
// layer, so the Computer use stage never moves.
//   window.shellBot.enter()      Shell use picked: the entrance
//   window.shellBot.settle()     straight onto the chat box (a run starts)
//   window.shellBot.manual(on)   Manual: the prompt is yours (body.shell-manual),
//                                and the visor's eyes give way to breathing dots
//   window.shellBot.work(on)     a run: the face shows its tools, not the eyes
//   window.shellBot.stop()       Shell use left
//   window.shellBot.face()       the face canvas, for the terminal card
//   window.shellBot.minionIn(id) / minionOut(id) / minionsClear()
//                                a coder's minion: a Minion bot beside it on the chat box
//   window.shellBot.minionFace(id) / minionWork(id, on)
//                                that bot's face canvas (its tools) and its working look
// It fires 'shellbot:settled' on document once it stands on the chat box.
// =====================================================================
(function () {
    'use strict';

    // == chain_bot.js: the splash's hero entrance, then the move.
    var FADE = 350, GROW = 1000;               // fades in; 0.85x -> 1x, blur 6px -> 0
    var WAKE_AT = 320, WAKE = 600;             // the eyes open
    var BLINK_AT = 1120, BLINK = 300;          // and blink
    var SETTLE_AT = 1500, SETTLE = 1100;       // then onto the chat box
    var BIG = 0.4;                             // its height while it comes in, of the frame's
    var EASE_SETTLE = 'cubic-bezier(0.16, 1, 0.3, 1)';
    var EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';
    var EASE_OUT = 'cubic-bezier(0.33, 1, 0.68, 1)';

    var anims = [], timer = null;

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    function find() {
        var host = document.getElementById('shellBot');
        if (!host) return null;
        var bot = host.querySelector('.sb-bot');
        return { host: host, bot: bot, body: bot.querySelector('.sb-bot-body'),
                 eyes: bot.querySelector('.sb-eyes'), face: bot.querySelector('.sb-face'),
                 dots: bot.querySelector('.sb-dots'), say: host.querySelector('.sb-say') };
    }

    // The bot's place: in the chat box's column (chat_input.html), just above the
    // "Agent running…" strip, so it stands on the strip during a run and on the box
    // otherwise. Not in the frame (#mainGrid): the frame clips what leaves it.
    function parts() {
        var p = find();
        if (p) return p;
        var box = document.querySelector('#chatWrapper .input-area-wrapper');
        if (!box) return null;
        var host = document.createElement('div');
        host.id = 'shellBot';
        host.setAttribute('aria-hidden', 'true');
        // The face sits between the eyes and the shine: the visor's glare stays
        // on top of the icons, as it does on the eyes.
        host.innerHTML =
            '<div class="sb-bot"><div class="sb-bot-body">' +
                '<img src="bots/coderuse.webp" alt="">' +
                '<img class="sb-eyes" src="bots/coderuse-eyes.webp" alt="">' +
                '<canvas class="sb-face"></canvas>' +
                '<canvas class="sb-dots"></canvas>' +
                '<img src="bots/coderuse-shine.webp" alt="">' +
            '</div></div>' +
            '<div class="sb-say" role="status"></div>';
        var strip = document.getElementById('agentResponseStrip');
        box.parentNode.insertBefore(host, (strip && strip.parentNode === box.parentNode) ? strip : box);
        // The box and the strip change size (a run, a draft, Manual): the bot's place
        // moves with them (the chat's bottom and the entrance's hold follow it).
        if (window.ResizeObserver) {
            var ro = new ResizeObserver(onResize);
            ro.observe(box);
            if (strip) ro.observe(strip);
        }
        window.addEventListener('resize', onResize);
        return find();
    }

    // The chat fills the frame down to a gap (the frame's own) above the bot's head
    // (--sb-clear, cli_stage.css), so no line hides behind it. The bot's place, not
    // its moves (they are transforms on .sb-bot).
    function clearance() {
        var stage = document.getElementById('cliContainer');
        var grid = document.getElementById('mainGrid');
        var host = document.getElementById('shellBot');
        if (!stage || !grid) return;
        var px = 0;
        if (host && host.offsetHeight) {
            var gap = parseFloat(getComputedStyle(grid).paddingBottom) || 0;
            var bottom = grid.getBoundingClientRect().bottom - gap;     // the terminal's, at full height
            px = Math.max(0, bottom - (host.getBoundingClientRect().top - gap));
        }
        stage.style.setProperty('--sb-clear', Math.round(px) + 'px');
    }

    // While it comes in: big in the middle of the frame (as the Computer use bot in
    // its screen box), held there by a transform over its place on the chat box.
    function hold(p) {
        var grid = document.getElementById('mainGrid');
        p.bot.style.transform = '';
        var to = p.bot.getBoundingClientRect();
        if (!grid || !to.height) return;
        var g = grid.getBoundingClientRect(), cs = getComputedStyle(grid);
        var top = g.top + parseFloat(cs.paddingTop), bottom = g.bottom - parseFloat(cs.paddingBottom);
        var left = g.left + parseFloat(cs.paddingLeft), right = g.right - parseFloat(cs.paddingRight);
        var h = (bottom - top) * BIG, w = h * to.width / to.height;
        p.bot.style.transform = 'translate(' + ((left + right - w) / 2 - to.left) + 'px, ' +
            ((top + bottom + h) / 2 - to.bottom) + 'px) scale(' + (h / to.height) + ')';
    }

    function onResize() {
        clearance();
        var p = find();
        if (p && p.bot.style.transform) hold(p);       // still coming in: the middle moved
    }

    function clear() {
        clearTimeout(timer);
        anims.forEach(function (a) { a.cancel(); });
        anims = [];
    }
    // Stop only the animations on one element (the others play on).
    function stopOn(el) {
        anims = anims.filter(function (a) {
            if (a.effect && a.effect.target === el) { a.cancel(); return false; }
            return true;
        });
    }

    // == chain_bot.js wake(): the eyes open out of a line, then blink once. `at`: when.
    function wake(eyes, at) {
        var span = BLINK_AT + BLINK - WAKE_AT;
        function key(t, open, easing) {
            return { offset: (t - WAKE_AT) / span, opacity: 1, transform: 'scaleY(' + open + ')', easing: easing || 'linear' };
        }
        return eyes.animate([
            { offset: 0, opacity: 0, transform: 'scaleY(0.06)', easing: EASE_OUT },
            key(WAKE_AT + WAKE, 1),
            key(BLINK_AT, 1, 'ease-in'), key(BLINK_AT + 0.45 * BLINK, 0.1),
            key(BLINK_AT + 0.51 * BLINK, 0.1, 'ease-out'), key(BLINK_AT + BLINK, 1)
        ], { delay: at, duration: span, fill: 'both' });
    }

    // Move by transform alone from where the bot is now (it may be mid-move) to
    // where `change` lays it out, then keep that layout (== chain_bot.js dock()).
    function flip(p, change, duration, done) {
        var from = p.bot.getBoundingClientRect();
        stopOn(p.bot);
        change();
        var to = p.bot.getBoundingClientRect();
        if (reduced() || !from.height || !to.height) { if (done) done(); return; }
        var move = p.bot.animate([
            { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.bottom - to.bottom) + 'px) scale(' + (from.height / to.height) + ')' },
            { transform: 'none' }
        ], { duration: duration, easing: EASE_EXPO });
        anims.push(move);
        move.onfinish = function () {
            anims = anims.filter(function (a) { return a !== move; });
            if (done) done();
        };
    }

    // On the chat box: the entrance is over, the eyes blink on their own (CSS),
    // and the terminal shows (cli_stage.css drops it in with shell-intro gone).
    function landed() {
        var p = find();
        clear();
        clearance();
        if (p) p.bot.classList.add('home');      // its face may show the run now (shell_bot.css)
        document.body.classList.remove('shell-intro');
        document.dispatchEvent(new CustomEvent('shellbot:settled'));
    }

    function enter() {
        var p = parts();
        if (!p) return;
        clear();
        dotsOff(p);
        p.bot.classList.remove('settled', 'home', 'working');
        p.bot.style.transform = '';
        if (reduced()) {
            p.bot.classList.add('settled');
            landed();
            return;
        }
        document.body.classList.add('shell-intro');
        hold(p);
        // The splash's hero: fades in, grows from 0.85x out of a blur, wakes.
        anims.push(p.body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, fill: 'both' }));
        anims.push(p.body.animate([{ transform: 'scale(0.85)' }, { transform: 'scale(1)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(p.body.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(wake(p.eyes, WAKE_AT));
        timer = setTimeout(settle, SETTLE_AT);
    }

    // Onto the chat box now (the entrance's time is up, or a run is starting).
    function settle() {
        var p = find();
        if (!p) return;
        clearTimeout(timer);
        if (p.bot.classList.contains('settled')) return;
        flip(p, function () { p.bot.style.transform = ''; p.bot.classList.add('settled'); }, SETTLE, landed);
    }

    // == the owner's coderuse-dots.html: beads in the glass's own colour on the visor,
    // bigger through the middle and finer toward the rim, each roaming slowly and
    // breathing on its own rhythm, clipped to the glass, under its shine. Its visor
    // outline and frame mapped onto these images (their own pixels, 520 x 720); fewer,
    // bigger beads than its 820px bot, so they read on this small one.
    var VISOR = 'M426.2 178.7 L427.0 187.8 L427.6 197.0 L427.8 206.3 L427.7 215.7 L427.1 225.2 L426.2 234.8 L424.6 244.3 L422.6 253.9 L419.7 263.3 L415.9 272.3 L411.1 280.8 L405.4 288.7 L398.6 295.8 L390.7 301.6 L382.1 306.5 L373.0 310.4 L363.6 313.3 L354.0 315.4 L344.6 316.8 L335.2 317.6 L326.3 318.4 L317.7 319.0 L309.4 319.5 L301.4 319.9 L293.6 320.2 L285.9 320.2 L278.3 320.5 L270.9 320.8 L263.6 320.9 L256.2 320.9 L248.9 320.9 L241.5 320.7 L234.1 320.4 L226.6 320.2 L218.9 319.9 L211.1 319.7 L203.1 319.2 L194.8 318.7 L186.3 318.0 L177.4 317.2 L168.3 316.1 L158.8 314.8 L149.4 312.6 L140.4 309.3 L131.8 305.0 L123.7 299.8 L116.3 293.7 L109.9 286.6 L104.3 278.9 L99.4 270.6 L95.5 261.8 L92.5 252.7 L90.1 243.4 L88.1 234.2 L86.7 224.8 L85.9 215.5 L85.7 206.1 L85.8 196.9 L86.0 187.8 L86.7 178.7 L87.9 169.8 L89.4 160.9 L91.2 152.2 L93.3 143.6 L95.8 135.1 L98.9 126.9 L102.1 118.7 L105.9 110.8 L110.1 103.2 L114.8 95.9 L120.0 89.0 L125.6 82.5 L131.7 76.4 L138.2 70.9 L145.1 66.0 L152.3 61.6 L159.6 57.7 L167.2 54.5 L174.9 51.7 L182.6 49.3 L190.2 47.3 L197.8 45.7 L205.4 44.4 L212.9 43.4 L220.2 42.4 L227.5 41.6 L234.7 41.0 L241.9 40.4 L249.1 40.1 L256.2 39.8 L263.4 39.8 L270.6 40.1 L277.8 40.3 L285.1 40.9 L292.4 41.7 L299.8 42.6 L307.2 43.8 L314.8 45.2 L322.3 47.0 L330.0 49.1 L337.6 51.6 L345.3 54.4 L352.9 57.6 L360.3 61.5 L367.5 65.8 L374.3 70.8 L380.8 76.3 L386.8 82.5 L392.3 89.0 L397.4 96.0 L402.4 103.2 L406.7 110.8 L410.4 118.7 L413.8 126.8 L416.8 135.1 L419.3 143.6 L421.4 152.2 L423.3 160.9 L424.9 169.8 Z';
    var DCX = 256.8, DCY = 180.3, DRX = 156.7, DRY = 123.4, DU = 344.4 / 420;
    var DOT = '95,47,41', DOT_MID = '58,26,23', DOT_EDGE = '28,14,12';
    var DOTS_IN = 0.9, DOTS_OUT = 0.35;     // s: up (after the eyes go), down
    function makeDots(canvas) {
        var ctx = canvas.getContext('2d');
        var clip = new Path2D(VISOR);
        // a small sprite: the beads are drawn only a few pixels across here, and a big
        // one shrunk that far comes out square
        var SP = 16, sprite = document.createElement('canvas');
        sprite.width = sprite.height = SP;
        var g = sprite.getContext('2d'), rg = g.createRadialGradient(SP / 2, SP / 2, 0, SP / 2, SP / 2, SP / 2);
        rg.addColorStop(0.00, 'rgba(' + DOT + ',1)');
        rg.addColorStop(0.42, 'rgba(' + DOT_MID + ',0.95)');
        rg.addColorStop(0.64, 'rgba(' + DOT_EDGE + ',0.5)');
        rg.addColorStop(1.00, 'rgba(' + DOT_EDGE + ',0)');
        g.fillStyle = rg;
        g.fillRect(0, 0, SP, SP);
        function rnd(a, b) { return a + Math.random() * (b - a); }
        var P = [];
        function seed(n, rIn, rOut, r0, r1, alpha) {
            for (var i = 0; i < n; i++) {
                var a = rnd(0, 6.283), rad = Math.sqrt(rnd(rIn * rIn, rOut * rOut));
                P.push({ x: DCX + Math.cos(a) * rad * DRX, y: DCY + Math.sin(a) * rad * DRY,
                         vx: rnd(-14, 14) * DU, vy: rnd(-14, 14) * DU, r: rnd(r0, r1), a: alpha,
                         lo: rIn, hi: rOut, ph: rnd(0, 6.283), sp: rnd(0.45, 1.05) });
            }
        }
        seed(90, 0.00, 0.80, 4.5, 10, 1.00);
        seed(70, 0.66, 1.00, 2.5, 5.5, 0.70);
        function smooth(u) { u = u < 0 ? 0 : u > 1 ? 1 : u; return u * u * u * (u * (u * 6 - 15) + 10); }
        var want = false, k = 0, wait = 0, raf = 0, last = 0, t = 0, w = -1, dpr = 0, K = 1;
        function fit() {
            var r = Math.min(window.devicePixelRatio || 1, 2);
            if (canvas.clientWidth === w && r === dpr) return;
            w = canvas.clientWidth; dpr = r;
            canvas.width = Math.round(canvas.clientWidth * r);
            canvas.height = Math.round(canvas.clientHeight * r);
            K = canvas.width / 520;
        }
        function draw(a) {
            ctx.setTransform(1, 0, 0, 1, 0, 0);
            ctx.clearRect(0, 0, canvas.width, canvas.height);
            if (a <= 0.002) return;
            ctx.setTransform(K, 0, 0, K, 0, 0);
            ctx.imageSmoothingEnabled = true;
            ctx.imageSmoothingQuality = 'high';
            ctx.save();
            ctx.clip(clip);
            for (var j = 0; j < P.length; j++) {
                var q = P[j];
                var r = reduced() ? q.r * 0.8 : q.r * (0.60 + 0.40 * Math.sin(t * q.sp * 2.0 + q.ph));   // breathing
                ctx.globalAlpha = a * q.a;
                ctx.drawImage(sprite, q.x - r * 1.7, q.y - r * 1.7, r * 3.4, r * 3.4);
            }
            ctx.restore();
            ctx.globalAlpha = 1;
        }
        function frame(now) {
            var dt = last ? Math.min((now - last) / 1000, 0.05) : 0;
            last = now;
            t += dt;
            fit();
            if (wait > 0) wait -= dt;
            else k += want ? dt / DOTS_IN : -dt / DOTS_OUT;
            k = k < 0 ? 0 : k > 1 ? 1 : k;
            if (!reduced()) {
                for (var i = 0; i < P.length; i++) {            // roam: a random walk kept in its ring
                    var p = P[i];
                    p.vx += rnd(-34, 34) * DU * dt; p.vy += rnd(-34, 34) * DU * dt;
                    var nx = (p.x - DCX) / DRX, ny = (p.y - DCY) / DRY, dd = Math.hypot(nx, ny) || 1e-6;
                    if (dd > p.hi) { p.vx -= (nx / dd) * 190 * DU * dt; p.vy -= (ny / dd) * 190 * DU * dt; }
                    else if (dd < p.lo) { p.vx += (nx / dd) * 190 * DU * dt; p.vy += (ny / dd) * 190 * DU * dt; }
                    var sp = Math.hypot(p.vx, p.vy), mx = 34 * DU;
                    if (sp > mx) { p.vx *= mx / sp; p.vy *= mx / sp; }
                    p.x += p.vx * dt; p.y += p.vy * dt;
                }
            }
            draw(smooth(k));
            if (!want && k <= 0) { raf = 0; last = 0; return; }   // gone: the loop rests
            raf = requestAnimationFrame(frame);
        }
        return {
            // on: up after `delay` s; off: down
            set: function (on, delay) {
                want = !!on;
                wait = on ? (delay || 0) : 0;
                if (!raf) raf = requestAnimationFrame(frame);
            },
            off: function () {                       // gone at once
                want = false; k = 0; wait = 0;
                if (raf) cancelAnimationFrame(raf);
                raf = 0; last = 0;
                draw(0);
            }
        };
    }
    var dotsFx = null;

    // A speech bubble over its head (Manual coming on), for `ms`, then it fades.
    var SAY_MANUAL = "You're in Manual mode";
    var sayTimer = null;
    function say(p, text, ms) {
        if (!p || !p.say) return;
        clearTimeout(sayTimer);
        p.say.textContent = text;
        p.say.classList.add('on');
        sayTimer = setTimeout(function () { p.say.classList.remove('on'); }, ms);
    }
    function hush(p) {
        clearTimeout(sayTimer);
        if (p && p.say) p.say.classList.remove('on');
    }

    // Manual: the visor's eyes close and go, and the dots come up (Auto: the dots go
    // and the eyes wake again).
    function visorDots(on) {
        var p = find();
        if (!p || !p.dots) return;
        if (!!on === p.bot.classList.contains('dotted')) return;
        if (!dotsFx) dotsFx = makeDots(p.dots);
        stopOn(p.eyes);
        p.bot.classList.toggle('dotted', !!on);
        if (on) {
            dotsFx.set(true, reduced() ? 0 : 0.35);
            say(p, SAY_MANUAL, 3200);
            if (!reduced()) anims.push(p.eyes.animate([
                { opacity: 1, transform: 'scaleY(1)', easing: 'ease-in' },
                { offset: 0.75, opacity: 0.6, transform: 'scaleY(0.08)' },
                { opacity: 0, transform: 'scaleY(0.08)' }
            ], { duration: 560, fill: 'forwards' }));
        } else {
            dotsFx.set(false);
            hush(p);
            if (reduced() || !p.bot.classList.contains('settled') || p.bot.classList.contains('working')) return;
            var w = wake(p.eyes, DOTS_OUT * 1000);
            anims.push(w);
            w.onfinish = function () { stopOn(p.eyes); };   // then the CSS blink again
        }
    }
    function dotsOff(p) {                    // at once (Shell use left, or comes in again)
        if (p) p.bot.classList.remove('dotted');
        if (dotsFx) dotsFx.off();
        hush(p);
    }

    // == the coder's minions (cli_stage.js cliMinionStart / cliMinionEnd): each one a
    // small Minion bot (the owner's minion-helmet.html: ui/bots/minion*.webp, cut and laid
    // out like the CoderUse layers) standing on the chat box beside the CoderUse bot, its
    // hem on the same line: the 1st on its right, the 2nd on its left, the 3rd further
    // right, the 4th further left (shell_bot.css). Four at most; more wait for a place.
    var MINION_PLACES = 4, MINION_OUT_MS = 320;
    var minions = {};            // minion id -> { place, el }
    var minionQueue = [];        // ids waiting for a place
    function minionIn(id) {
        var p = find();
        if (!p || !id || minions[id] || minionQueue.indexOf(id) >= 0) return;
        var taken = {};
        for (var k in minions) taken[minions[k].place] = true;
        var place = 1;
        while (place <= MINION_PLACES && taken[place]) place++;
        if (place > MINION_PLACES) { minionQueue.push(id); return; }
        var m = document.createElement('div');
        m.className = 'sb-minion place-' + place;
        // its face (its minion's tools, drawn by the terminal card) between the eyes and
        // the shine, as on the CoderUse bot
        m.innerHTML =
            '<img src="bots/minion.webp" alt="">' +
            '<img class="sb-meyes" src="bots/minion-eyes.webp" alt="">' +
            '<canvas class="sb-mface"></canvas>' +
            '<img src="bots/minion-shine.webp" alt="">';
        // each blinks on its own time, never all together
        m.style.setProperty('--sb-blink-at', (1 + Math.random() * 3.5).toFixed(2) + 's');
        p.host.appendChild(m);
        minions[id] = { place: place, el: m };
    }
    function minionOut(id) {
        var w = minionQueue.indexOf(id);
        if (w >= 0) { minionQueue.splice(w, 1); return; }
        var m = minions[id];
        if (!m) return;                           // a repeated or stale end
        delete minions[id];
        m.el.classList.add('leaving');
        setTimeout(function () {
            if (m.el.parentNode) m.el.parentNode.removeChild(m.el);
            if (minionQueue.length) minionIn(minionQueue.shift());   // the next one takes the place
        }, reduced() ? 0 : MINION_OUT_MS);
    }
    // A minion's face canvas (null while it waits for a place), and its working look:
    // the eyes give way to the tools (coder_card.js minionFace).
    function minionFace(id) { return minions[id] ? minions[id].el.querySelector('.sb-mface') : null; }
    function minionWork(id, on) { if (minions[id]) minions[id].el.classList.toggle('working', !!on); }
    function minionsClear() {                    // the run is over (or Shell use left): all go
        minionQueue = [];
        for (var k in minions) { var el = minions[k].el; if (el.parentNode) el.parentNode.removeChild(el); }
        minions = {};
    }

    // Manual: the prompt is yours (Auto: the agent's); the chat stays, and the bot's
    // visor shows the dots.
    function manual(on) {
        document.body.classList.toggle('shell-manual', !!on);
        visorDots(!!on);
    }

    // A run: the eyes go and the face shows its tools (the card paints them), once
    // the bot stands on the chat box (.home); while it comes in, its eyes stay.
    // Done: the face empties and the eyes open again.
    function work(on) {
        var p = find();
        if (!p) return;
        var home = p.bot.classList.contains('home');
        var was = p.bot.classList.contains('working');
        if (on && p.bot.classList.contains('dotted')) dotsOff(p);   // the face's tools take the visor
        if (home) stopOn(p.eyes);           // a wake in play would hold the eyes over the face
        p.bot.classList.toggle('working', !!on);
        if (on || !was || !home || reduced()) return;
        var w = wake(p.eyes, 0);
        anims.push(w);
        w.onfinish = function () { stopOn(p.eyes); };   // then the CSS blink again
    }

    function stop() {
        var p = find();
        clearTimeout(timer);
        document.body.classList.remove('shell-intro', 'shell-manual');
        minionsClear();
        if (!p) return;
        clear();
        dotsOff(p);
        p.bot.classList.remove('settled', 'home', 'working');
        p.bot.style.transform = '';
        clearance();
    }

    window.shellBot = {
        enter: enter,
        settle: settle,
        manual: manual,
        work: work,
        stop: stop,
        face: function () { var p = parts(); return p ? p.face : null; },
        minionIn: minionIn,
        minionOut: minionOut,
        minionsClear: minionsClear,
        minionFace: minionFace,
        minionWork: minionWork
    };
})();
