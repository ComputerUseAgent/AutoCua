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
// task is sent (reserve), so nothing jumps.
//   window.chainBot.reserve()     a run's task was sent: room on the left
//   window.chainBot.enter(kind)   the run started: 'computer', 'web', 'mobile'
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

    var anims = [], timer = null;

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
    // the docked layout, then play the move from where the bot was into it.
    function dock() {
        var p = parts();
        if (!p) return;
        if (reduced()) return docked(p);
        var from = p.bot.getBoundingClientRect();
        p.bot.classList.add('docked');
        var to = p.bot.getBoundingClientRect();
        if (!from.height || !to.height) return docked(p);
        var move = p.bot.animate([
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
            clear();
            var img = BOTS[kind] || BOTS.computer;
            var files = [img, img + '-eyes', img + '-shine'];
            var imgs = p.bot.querySelectorAll('img');
            for (var i = 0; i < imgs.length && i < files.length; i++) {
                var src = 'bots/' + files[i] + '.webp';
                if (imgs[i].getAttribute('src') !== src) imgs[i].setAttribute('src', src);
            }
            p.eyes.style.removeProperty('transform');
            p.bot.classList.remove('docked');
            p.turn.classList.add('cb-room', 'cb-on', 'cb-intro');
            if (reduced()) return docked(p);
            // The splash's hero: fades in, grows from 0.85x out of a blur, wakes.
            anims.push(p.body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, fill: 'both' }));
            anims.push(p.body.animate([{ transform: 'scale(0.85)' }, { transform: 'scale(1)' }],
                                      { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
            anims.push(p.body.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                                      { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
            anims.push(wake(p.eyes));
            timer = setTimeout(dock, DOCK_AT);
        },
        stop: function () {
            var p = parts();
            clear();
            if (!p) return;
            p.turn.classList.remove('cb-room', 'cb-on', 'cb-intro');
            p.bot.classList.remove('docked');
            p.eyes.style.removeProperty('transform');
        }
    };
})();
