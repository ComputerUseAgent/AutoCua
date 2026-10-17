// =====================================================================
// The run's bot (chain_bot.css), for Computer use, Web use and iOS runs,
// each its own (ui/bots/<bot>*.webp). It is an item of the live turn's grid,
// in the bottom container's row, made here on first need. When the run
// starts (chat_input.js, once the backend says 'started') it is simply there
// (owner, 2026-10-17: no entrance), outside the container on its left, its
// top on the container's top: the place and size of the reply's bot
// (run_reply.css). When the run ends it stays there: beside the reply, or
// beside a Computer use run's video with the reply under it. It blinks now and then
// like the bot pages (bot_blink.js). The container makes room on its left
// from the moment the task is sent (reserve), so nothing jumps. In a fresh
// chat this bot already stands on the chat box (chat_input/mode_bot.js):
// then it comes over to its place from there itself.
//   window.chainBot.reserve()     a run's task was sent: room on the left
//   window.chainBot.enter(kind)   the run started: 'computer', 'web', 'mobile'
//   window.chainBot.toReply()     the run ended: to its place beside the reply
//   window.chainBot.stop()        back to the plain container (resetChatUi)
// =====================================================================
(function () {
    'use strict';

    var BOTS = { computer: 'computeruse', web: 'webuse', mobile: 'mobileuse' };
    var DOCK = 1100;                                     // a move to its place
    var EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';    // the splash's "inOutExpo"

    var flight = null;
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
        return { turn: turn, bot: bot, eyes: bot.querySelector('.eyes') };
    }

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    // To where it is laid out now, by transform alone, from where it was.
    function dock(p, from) {
        if (flight) flight.cancel();
        flight = null;
        var to = p.bot.getBoundingClientRect();
        if (reduced() || !from.height || !to.height) return;
        flight = p.bot.animate([
            { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.top - to.top) + 'px) scale(' + (from.height / to.height) + ')' },
            { transform: 'none' }
        ], { duration: DOCK, easing: EASE_EXPO });
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
            if (flight) flight.cancel();
            var mine = ++hand;
            var img = BOTS[kind] || BOTS.computer;
            var files = [img, img + '-eyes', img + '-shine'];
            var imgs = p.bot.querySelectorAll('img');
            for (var i = 0; i < imgs.length && i < files.length; i++) {
                var src = 'bots/' + files[i] + '.webp';
                if (imgs[i].getAttribute('src') !== src) imgs[i].setAttribute('src', src);
            }
            p.eyes.style.removeProperty('transform');
            p.turn.classList.add('cb-room', 'cb-on');
            p.bot.classList.add('docked');
            // It stands on the chat box already: it comes over itself, unseen
            // here until it lands (and on from there if the place moved).
            if (window.modeBot && window.modeBot.ready(kind)) {
                p.bot.classList.add('cb-wait');
                window.modeBot.flyTo(p.bot, function (at) {
                    if (mine !== hand) return;
                    p.bot.classList.remove('cb-wait');
                    var now = p.bot.getBoundingClientRect();
                    if (Math.abs(now.left - at.left) > 2 || Math.abs(now.top - at.top) > 2) dock(p, at);
                });
            }
        },
        // The run ended (run_reply.js): called before its reply comes in (a
        // Web use or iOS screen box goes), the function it gives back after
        // (with how far the chat is about to glide up to the reply's end).
        // The bot stays, or goes from where it was to its place: one bot,
        // nothing jumps.
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
                if (Math.abs(now.top - from.top) < 0.5 && Math.abs(now.left - from.left) < 0.5) return;   // it stays put
                dock(p, from);
            };
        },
        stop: function () {
            var p = parts();
            if (flight) flight.cancel();
            flight = null;
            hand++;
            if (!p) return;
            p.turn.classList.remove('cb-room', 'cb-on');
            p.bot.classList.remove('docked', 'cb-wait');
            p.eyes.style.removeProperty('transform');
        }
    };
})();
