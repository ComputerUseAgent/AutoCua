// =====================================================================
// The run's bot (chain_bot.css), for Computer use, Web use and iOS runs,
// each its own (ui/bots/<bot>*.webp). It is an item of the live turn's grid,
// in the bottom container's row, made here on first need. When the run
// starts (chat_input.js, once the backend says 'started') it comes in big in
// the middle of the bottom container like the splash's hero
// (intro_animation.html): it fades in, grows from 0.85x out of a 6px blur,
// wakes and blinks. Then it moves out to the container's left and stands
// there, its top on the container's top, for the run: the place and size of
// the reply's bot (run_reply.css), so when the run ends it stays and its
// reply comes in beside it. It blinks now and then like the bot pages
// (bot_blink.js). The container makes room on its left from the moment the
// task is sent (reserve), so nothing jumps. In a fresh chat this bot already
// stands on the chat box (chat_input/mode_bot.js): then it comes over to its
// place from there itself, no entrance.
//   window.chainBot.reserve()     a run's task was sent: room on the left
//   window.chainBot.enter(kind)   the run started: 'computer', 'web', 'mobile'
//   window.chainBot.toReply()     the run ended: to its place beside the reply
//   window.chainBot.stop()        back to the plain container (resetChatUi)
// =====================================================================
(function () {
    'use strict';

    var BOTS = { computer: 'computeruse', web: 'webuse', mobile: 'mobileuse' };
    // Timing, ms from the start: the splash's hero entrance, then the move.
    var FADE = 350, GROW = 1000;               // fades in; 0.85x -> 1x, blur 6px -> 0
    var WAKE_AT = 320, WAKE = 600;             // the eyes open
    var BLINK_AT = 1120, BLINK = 300;          // and blink
    var DOCK_AT = 1500, DOCK = 1100;           // out to the container's left
    var EASE_SETTLE = 'cubic-bezier(0.16, 1, 0.3, 1)';   // the splash's "settle"
    var EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';    // and "inOutExpo"
    var EASE_OUT = 'cubic-bezier(0.33, 1, 0.68, 1)';     // and "outCubic"

    var anims = [], timer = null, flight = null;
    var hand = 0;                 // each run's own: a stopped run's bot never takes over

    function parts() {
        var turn = document.getElementById('liveTurn');
        if (!turn) return null;
        var bot = turn.querySelector(':scope > .chain-bot');
        if (!bot) {
            bot = document.createElement('div');
            bot.className = 'chain-bot';
            bot.setAttribute('aria-hidden', 'true');
            bot.innerHTML = '<div class="chain-bot-body"><img alt=""><img class="eyes" alt=""><img alt=""></div>';
            turn.appendChild(bot);
        }
        return { turn: turn, bot: bot, body: bot.querySelector('.chain-bot-body'), eyes: bot.querySelector('.eyes') };
    }

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    function clear() {
        clearTimeout(timer);
        anims.forEach(function (a) { a.cancel(); });
        anims = [];
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

    // At the container's left for the rest of the run: the chain and the
    // tracking show (they waited while it came in).
    function docked(p) {
        clear();
        p.bot.classList.add('docked');
        p.turn.classList.remove('cb-intro');
    }

    // From the middle out to the container's left by transform alone: take
    // the docked layout, then play the move from where the bot was (from, or
    // where it is now) into it.
    function dock(from) {
        var p = parts();
        if (!p) return;
        if (reduced()) return docked(p);
        from = from || p.bot.getBoundingClientRect();
        p.bot.classList.add('docked');
        var to = p.bot.getBoundingClientRect();
        if (!from.height || !to.height) return docked(p);
        var move = flight = p.bot.animate([
            { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.top - to.top) + 'px) scale(' + (from.height / to.height) + ')' },
            { transform: 'none' }
        ], { duration: DOCK, easing: EASE_EXPO });
        anims.push(move);
        move.onfinish = function () { docked(p); };
    }

    window.chainBot = {
        reserve: function () {
            var p = parts();
            if (p) p.turn.classList.add('cb-room');
        },
        enter: function (kind) {
            var p = parts();
            if (!p) return;
            // The chat box's bot is on its way here already (a second start
            // answer, a stopped run's late one): it lands as this run's bot.
            if (p.bot.classList.contains('cb-wait')) return;
            clear();
            var mine = ++hand;
            var img = BOTS[kind] || BOTS.computer;
            var files = [img, img + '-eyes', img + '-shine'];
            var imgs = p.bot.querySelectorAll('img');
            for (var i = 0; i < imgs.length && i < files.length; i++) {
                var src = 'bots/' + files[i] + '.webp';
                if (imgs[i].getAttribute('src') !== src) imgs[i].setAttribute('src', src);
            }
            p.eyes.style.removeProperty('transform');
            p.bot.classList.remove('docked');
            // It stands on the chat box already: it comes over itself. Its
            // place is laid out now, unseen until it lands there; then it is
            // here (and goes on from there if the place moved meanwhile).
            if (window.modeBot && window.modeBot.ready(kind)) {
                p.turn.classList.add('cb-room', 'cb-on');
                p.bot.classList.add('docked', 'cb-wait');
                window.modeBot.flyTo(p.bot, function (at) {
                    if (mine !== hand) return;
                    p.bot.classList.remove('cb-wait');
                    var now = p.bot.getBoundingClientRect();
                    if (Math.abs(now.left - at.left) > 2 || Math.abs(now.top - at.top) > 2) {
                        if (flight) flight.cancel();
                        dock(at);
                    }
                });
                return;
            }
            p.turn.classList.add('cb-room', 'cb-on', 'cb-intro');
            if (reduced()) return docked(p);
            // The splash's hero: fades in, grows from 0.85x out of a blur, wakes.
            anims.push(p.body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, fill: 'both' }));
            anims.push(p.body.animate([{ transform: 'scale(0.85)' }, { transform: 'scale(1)' }],
                                      { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
            anims.push(p.body.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                                      { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
            anims.push(wake(p.eyes));
            timer = setTimeout(function () { dock(); }, DOCK_AT);
        },
        // The run ended (run_reply.js): called before its reply comes in (and
        // a Web use or iOS turn's screen box goes, so the container moves up),
        // the function it gives back after (with how far the chat is about to
        // glide up to the reply's end). The bot goes from where it was to its
        // place beside the reply, out of the middle at once if it was still
        // coming in: one bot, nothing jumps.
        toReply: function () {
            var turn = document.getElementById('liveTurn');
            if (!turn || !turn.classList.contains('cb-on')) return null;
            var p = parts(), from = p.bot.getBoundingClientRect();
            return function (by) {
                var now = p.bot.getBoundingClientRect();
                // The chat box's bot is still on its way here: on to where this
                // place will be once the chat has glided up by `by` to the reply.
                if (p.bot.classList.contains('cb-wait')) {
                    if (window.modeBot) window.modeBot.retarget(now, by || 0);
                    return;
                }
                if (p.bot.classList.contains('docked') && Math.abs(now.top - from.top) < 0.5
                    && Math.abs(now.left - from.left) < 0.5) return;      // it stays put
                clearTimeout(timer);
                if (flight) flight.cancel();
                dock(from);
            };
        },
        stop: function () {
            var p = parts();
            clear();
            hand++;
            if (!p) return;
            p.turn.classList.remove('cb-room', 'cb-on', 'cb-intro');
            p.bot.classList.remove('docked', 'cb-wait');
            p.eyes.style.removeProperty('transform');
        }
    };
})();
