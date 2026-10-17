// =====================================================================
// The mode's bot when a mode is picked (mode_bot.css): Computer use, Web use
// and Mobile use, each its own (ui/bots/<bot>*.webp); Shell use has its own
// (cli/shell_bot.js). It comes in big in the middle of the frame like the
// splash's hero (intro_animation.html): it fades in, grows from 0.85x out
// of a 6px blur, wakes and blinks; the welcome words wait meanwhile. Then,
// in a fresh chat (nothing sent yet), it shrinks onto the chat box and
// stands there like Shell use's bot, its waist on the box's top edge,
// blinking now and then (bot_blink.js). In a chat with turns it fades away
// instead. When that chat's run starts it goes over to the bottom container
// itself and is the run's bot there (chain_bot.js). Any other task sent,
// New chat, opening a saved chat or picking Shell use takes it away.
// Its place is in the chat box's column, like Shell use's bot.
//   window.modeBot.enter(kind)   a mode was picked: 'computer', 'web', 'mobile'
//   window.modeBot.hide()        away (a task sent, a chat opened, Shell use)
//   window.modeBot.ready(kind)   a fresh chat's bot, as kind's?
//   window.modeBot.flyTo(to, done)  then over to to's place (chain_bot.js)
//   window.modeBot.retarget(rect, by)  the run ended on the way: on to its place's new spot
//   window.modeBot.drop()        its run does not start: away (not mid-flight)
// =====================================================================
(function () {
    'use strict';

    // Each mode's bot and the rows under its sweater's hem, of the images' 1224
    // (its waist stands on the chat box's top edge; == the screen stage's numbers).
    var BOTS = {
        computer: { img: 'computeruse', hem: 65 },
        web: { img: 'webuse', hem: 44 },
        mobile: { img: 'mobileuse', hem: 70 }
    };
    // == shell_bot.js: the splash's hero entrance, then onto the chat box.
    var FADE = 350, GROW = 1000;               // fades in; 0.85x -> 1x, blur 6px -> 0
    var WAKE_AT = 320, WAKE = 600;             // the eyes open
    var BLINK_AT = 1120, BLINK = 300;          // and blink
    var SETTLE_AT = 1500, SETTLE = 1100;       // then onto the chat box
    var LEAVE = 500;                           // or away: it fades out
    var FLY = 1100;                            // over to the run's bot's place (== chain_bot.js DOCK)
    var BIG = 0.4;                             // its height while it comes in, of the frame's
    var EASE_SETTLE = 'cubic-bezier(0.16, 1, 0.3, 1)';
    var EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';
    var EASE_OUT = 'cubic-bezier(0.33, 1, 0.68, 1)';

    var anims = [], timer = null, leaving = false, glide = null, flight = null;
    var stay = false;                          // its chat is fresh: it stays (on the chat box)
    var handing = null;                        // while it flies over: the run's bot's take-over
    var aim = null, flew = 0;                  // where it flies to, and since when

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    // A fresh chat: nothing sent and nothing opened yet.
    function fresh() {
        var msg = document.getElementById('userContainer');
        return !window.currentSessionId && !document.querySelector('#chatPast > .chat-turn')
            && (!msg || msg.classList.contains('is-empty'));
    }

    function find() {
        var host = document.getElementById('modeBot');
        if (!host) return null;
        var bot = host.querySelector('.mb-bot');
        return { host: host, bot: bot, body: bot.querySelector('.mb-bot-body'), eyes: bot.querySelector('.mb-eyes') };
    }

    // Its place: in the chat box's column (chat_input.html), just above the
    // "Agent running..." strip, as Shell use's bot. Not in the frame: the
    // frame clips what leaves it.
    function parts() {
        var p = find();
        if (p) return p;
        var box = document.querySelector('#chatWrapper .input-area-wrapper');
        if (!box) return null;
        var host = document.createElement('div');
        host.id = 'modeBot';
        host.setAttribute('aria-hidden', 'true');
        host.innerHTML = '<div class="mb-bot"><div class="mb-bot-body">' +
            '<img alt=""><img class="mb-eyes" alt=""><img alt="">' +
            '</div></div>';
        var strip = document.getElementById('agentResponseStrip');
        box.parentNode.insertBefore(host, (strip && strip.parentNode === box.parentNode) ? strip : box);
        window.addEventListener('resize', function () {
            var q = find();
            if (q && q.bot.style.transform && !q.bot.classList.contains('settled')) hold(q);   // the middle moved
        });
        return find();
    }

    function clear() {
        clearTimeout(timer);
        anims.forEach(function (a) { a.cancel(); });
        anims = [];
    }

    // While it comes in: big in the middle of the frame, held there by a
    // transform over its place on the chat box (== shell_bot.js hold()).
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

    // The eyes' one animation (the splash's wake()): open out of a line, then blink.
    function wake(eyes) {
        var span = BLINK_AT + BLINK - WAKE_AT;
        function key(t, open, easing) {
            return { offset: (t - WAKE_AT) / span, opacity: 1, transform: 'scaleY(' + open + ')', easing: easing || 'linear' };
        }
        return eyes.animate([
            { offset: 0, opacity: 0, transform: 'scaleY(0.06)', easing: EASE_OUT },
            key(WAKE_AT + WAKE, 1),
            key(BLINK_AT, 1, 'ease-in'), key(BLINK_AT + 0.45 * BLINK, 0.1),
            key(BLINK_AT + 0.51 * BLINK, 0.1, 'ease-out'), key(BLINK_AT + BLINK, 1)
        ], { delay: WAKE_AT, duration: span, fill: 'both' });
    }

    // On the chat box: the entrance is over, the welcome words come back.
    function landed(p) {
        clear();
        p.bot.style.transform = '';
        p.bot.classList.add('settled', 'home');
        document.body.classList.remove('mode-intro', 'mode-front');
    }

    // Onto the chat box by transform alone, from the middle (== shell_bot.js settle()).
    function settle() {
        var p = find();
        if (!p) return;
        var from = p.bot.getBoundingClientRect();
        p.bot.style.transform = '';
        p.bot.classList.add('settled');
        var to = p.bot.getBoundingClientRect();
        if (reduced() || !from.height || !to.height) return landed(p);
        var move = glide = p.bot.animate([
            { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.bottom - to.bottom) + 'px) scale(' + (from.height / to.height) + ')' },
            { transform: 'none' }
        ], { duration: SETTLE, easing: EASE_EXPO });
        anims.push(move);
        move.onfinish = function () { if (!leaving) landed(p); };
    }

    // A fresh chat's run started (chain_bot.js): is it this chat's bot (coming
    // in, on its way onto the chat box, or standing there) as that run's bot?
    function ready(kind) {
        var p = find();
        return !!p && p.host.classList.contains('on') && stay
            && !leaving && !handing && p.host.dataset.kind === kind;
    }

    // Then it goes over to the run's bot's place itself (to: that bot, laid
    // out there unseen) with the move the run's bot makes to the container's
    // left, from wherever it is, in front of the frame on the way (its own
    // entrance's fade, growth and wake go on meanwhile). There the run's bot
    // takes over: done(where it is). Cut short (a mode picked, New chat, a
    // chat opened), the run's bot takes over from wherever it is.
    function flyTo(to, done) {
        var p = find();
        var from = p.bot.getBoundingClientRect();
        clearTimeout(timer);
        if (glide) glide.cancel();
        handing = done;
        flew = performance.now();
        p.bot.style.transform = '';
        // Its eyes stay open on the way, and no blink starts (bot_blink.js).
        p.bot.classList.add('settled', 'home', 'flying');
        document.body.classList.remove('mode-intro');
        document.body.classList.add('mode-front');
        fly(p, from, to.getBoundingClientRect(), FLY, EASE_EXPO);
    }

    // The run ended on the way (chain_bot.js toReply): its place (rect) may
    // have moved (a screen box gone), and the chat is about to glide up by
    // `by` to the reply's end. It goes on from where it is to where its place
    // will be, in the time left, no stop.
    function retarget(rect, by) {
        var p = find();
        if (!handing || !p || !flight) return;
        var at = { left: rect.left, bottom: rect.bottom - by, height: rect.height };
        if (aim && Math.abs(at.left - aim.left) < 1 && Math.abs(at.bottom - aim.bottom) < 1
            && Math.abs(at.height - aim.height) < 1) return;
        var from = p.bot.getBoundingClientRect();
        flight.onfinish = null;
        flight.cancel();
        fly(p, from, at, Math.max(400, FLY - (performance.now() - flew)), EASE_OUT);
    }

    // The move, by transform alone over its place on the chat box: from one
    // rect to another, then the run's bot takes over (release).
    function fly(p, from, at, duration, easing) {
        aim = at;
        var place = p.bot.getBoundingClientRect();
        if (reduced() || !place.height || !at.height) return release();
        function over(r) {
            return 'translate(' + (r.left - place.left) + 'px, ' + (r.bottom - place.bottom) + 'px) scale(' + (r.height / place.height) + ')';
        }
        var move = flight = p.bot.animate([{ transform: over(from) }, { transform: over(at) }],
                                          { duration: duration, easing: easing, fill: 'forwards' });
        anims.push(move);
        move.onfinish = release;
    }

    function release() {
        var p = find(), done = handing;
        handing = null;
        if (!p || !done) return;
        var at = p.bot.getBoundingClientRect();
        off(p);
        done(at);
    }

    // The run it was kept for does not start (Stop came first, a start
    // error): it goes. Already on its way over, it lands as that run's bot.
    function drop() {
        if (!handing) leave();
    }

    // Away: it fades out where it is (once), then is gone. Flying over, the
    // run's bot takes over from where it is instead.
    function leave() {
        var p = find();
        clearTimeout(timer);
        if (handing) return release();
        if (!p || !p.host.classList.contains('on') || leaving) return;
        leaving = true;
        document.body.classList.remove('mode-intro');
        if (reduced()) return off(p);
        var out = p.bot.animate([{ opacity: 1 }, { opacity: 0 }], { duration: LEAVE, easing: 'ease', fill: 'forwards' });
        anims.push(out);
        out.onfinish = function () { off(p); };
    }

    function off(p) {
        clear();
        leaving = false;
        p.host.classList.remove('on');
        p.bot.classList.remove('settled', 'home', 'flying');
        p.bot.style.transform = '';
        p.eyes.style.removeProperty('transform');
        document.body.classList.remove('mode-intro', 'mode-front');
    }

    function use(p, kind) {
        var b = BOTS[kind];
        var files = [b.img, b.img + '-eyes', b.img + '-shine'];
        var imgs = p.bot.querySelectorAll('img');
        for (var i = 0; i < imgs.length && i < files.length; i++) {
            var src = 'bots/' + files[i] + '.webp';
            if (imgs[i].getAttribute('src') !== src) imgs[i].setAttribute('src', src);
        }
        p.host.style.setProperty('--mb-hem', String(b.hem));
        p.host.dataset.kind = kind;
    }

    function enter(kind) {
        if (handing) release();
        if (!BOTS[kind]) return leave();
        var p = parts();
        if (!p) return;
        clear();
        leaving = false;
        use(p, kind);
        p.bot.classList.remove('settled', 'home');
        p.eyes.style.removeProperty('transform');
        p.host.classList.add('on');
        stay = fresh();
        if (reduced()) {
            if (stay) landed(p);
            else off(p);
            return;
        }
        document.body.classList.add('mode-intro', 'mode-front');
        hold(p);
        // The splash's hero: fades in, grows from 0.85x out of a blur, wakes.
        anims.push(p.body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, fill: 'both' }));
        anims.push(p.body.animate([{ transform: 'scale(0.85)' }, { transform: 'scale(1)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(p.body.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(wake(p.eyes));
        // Fresh at the pick: onto the chat box (a task sent meanwhile keeps it
        // for its run, chat_input.js; a chat opened or New chat took it away).
        timer = setTimeout(function () {
            if (stay) settle();
            else leave();
        }, SETTLE_AT);
    }

    // A mode picked in the menu (agent_mode.js; every pick, not the silent
    // sets below). On the next frame: leaving Shell use takes its stage away
    // in its own listener first (cli_stage.js), and the bot's place shows only then.
    document.addEventListener('agentmode:changed', function (e) {
        var mode = (e.detail || {}).mode;
        if (!BOTS[mode]) return leave();
        requestAnimationFrame(function () { enter(mode); });
    });
    // The menu set without a pick (a reopened chat's lock, a failed iPhone
    // pairing going back to Computer use): the bot in a fresh chat becomes
    // that mode's (also while it comes in), or goes. A frame later: New chat
    // sets it too, and says so (chat:new) right after.
    document.addEventListener('agentmode:set', function (e) {
        var mode = (e.detail || {}).mode;
        requestAnimationFrame(function () {
            var p = find();
            if (!p || !p.host.classList.contains('on') || leaving || handing) return;
            if (BOTS[mode] && fresh()) use(p, mode);
            else leave();
        });
    });
    // A saved chat opened: no longer a fresh chat. After New chat: no bot.
    document.addEventListener('chat:opened', leave);
    document.addEventListener('chat:new', leave);

    window.modeBot = { enter: enter, hide: leave, ready: ready, flyTo: flyTo, retarget: retarget, drop: drop };
})();
