// =====================================================================
// Computer use stage, in the screen box (run_bot.css). When a Computer use
// run starts (chat_input.js, once the backend says 'started'), the moving
// background fades in and the Computer use bot comes in like the splash's
// hero (intro_animation.html): it fades in, grows from 0.85x out of a 6px
// blur, wakes and blinks. Then it shrinks into the bottom-left corner and
// the screenshots show behind it.
// When the run has ended and its video is being compressed, the bot comes
// back to the middle over "Preparing video" (after a short grace, so a video
// that is ready at once just shows). It leaves when the video is in; if no
// video comes, it goes back to its corner. A reopened Computer use chat
// shows its video on the same stage, without the bot.
//   window.startScreenBot()            a Computer use run started
//   window.screenStageOn()             a reopened chat's stage (chat.js)
//   window.runVideoPreparing(chatId)   the backend: compressing the run's video
//   window.runVideoFailed(chatId)      the backend / the player: no video after all
//   window.screenBotLeave()            the run's video is in (screen.js)
//   window.stopScreenBot()             back to the plain box (resetChatUi)
// =====================================================================
(function () {
    'use strict';

    var BG = '/simulation_background.mp4';     // AutoCua/logo/, served by ui/service.py
    // Timing, ms from the start: the splash's hero entrance, then the dock.
    var FADE = 350, GROW = 1000;               // fades in; 0.85x -> 1x, blur 6px -> 0
    var WAKE_AT = 320, WAKE = 600;             // the eyes open
    var BLINK_AT = 1120, BLINK = 300;          // and blink
    var DOCK_AT = 1500, DOCK = 1100;           // shrink into the corner
    var GRACE = 600, MOVE = 900;               // video: wait, then back to the middle
    // == .cu-bot.docked: the sweater's hem (row 1159 of the images' 1224) on
    // the screenshot's bottom edge, 7% above the box's.
    var DOCK_H = 0.46, DOCK_LEFT = 0.015, DOCK_BOTTOM = 0.07 - DOCK_H * 65 / 1224;
    var EASE_SETTLE = 'cubic-bezier(0.16, 1, 0.3, 1)';   // the splash's "settle"
    var EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';    // and "inOutExpo"
    var EASE_OUT = 'cubic-bezier(0.33, 1, 0.68, 1)';     // and "outCubic"

    var anims = [], timer = null, prepTimer = null;

    function parts() {
        var zone = document.getElementById('screenContainer');
        var bot = zone && zone.querySelector('.cu-bot');
        if (!bot) return null;
        return { zone: zone, bot: bot, body: bot.querySelector('.cu-bot-body'),
                 eyes: bot.querySelector('.eyes'), bg: zone.querySelector('.cu-bg'),
                 video: zone.querySelector('.run-video') };
    }

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    function playBg(p) {
        if (!p.bg.getAttribute('src')) p.bg.src = BG;
        p.bg.muted = true;
        var playing = p.bg.play();
        if (playing && playing.catch) playing.catch(function () { /* stays on its first frame */ });
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

    // In the corner: the bot takes its own small layout there, the screenshot
    // shows, and the box takes the screenshot's shape (held while the bot came in).
    function docked(p) {
        clear();
        p.bot.classList.remove('preparing');
        p.bot.classList.add('docked');
        p.zone.classList.remove('cu-intro');
        var img = p.zone.querySelector('.stream-image');
        if (img && img.naturalHeight) {
            p.zone.style.setProperty('--screen-ar', String(img.naturalWidth / img.naturalHeight));
        }
    }

    // Shrink from the middle into the corner by transform alone, from where
    // the bot really is (it may be mid-flight to the middle) to where the
    // docked layout puts it, then swap to that layout.
    function dock(duration) {
        var p = parts();
        if (!p) return;
        if (reduced()) return docked(p);
        var live = p.bot.getBoundingClientRect();
        clearTimeout(timer);
        // Only the bot's own move stops; the eyes' wake keeps them open on the way.
        anims = anims.filter(function (a) {
            if (a.effect && a.effect.target === p.bot) { a.cancel(); return false; }
            return true;
        });
        var lay = p.bot.getBoundingClientRect();
        var box = p.zone.getBoundingClientRect();
        var w = p.zone.clientWidth, h = p.zone.clientHeight;
        if (!lay.height || !h) return docked(p);
        var left = box.left + p.zone.clientLeft + DOCK_LEFT * w;
        var bottom = box.top + p.zone.clientTop + (1 - DOCK_BOTTOM) * h;
        function at(l, b, height) {
            return 'translate(' + (l - lay.left) + 'px, ' + (b - lay.bottom) + 'px) scale(' + (height / lay.height) + ')';
        }
        var move = p.bot.animate([
            { transform: at(live.left, live.bottom, live.height) },
            { transform: at(left, bottom, DOCK_H * h) }
        ], { duration: duration || DOCK, easing: EASE_EXPO, fill: 'forwards' });
        anims.push(move);
        move.onfinish = function () { docked(p); };
    }

    // Back to the middle for "Preparing video": take the big layout there and
    // grow into it from wherever the bot is now (corner, or still coming in).
    function prepare() {
        var p = parts();
        if (!p || !p.zone.classList.contains('cu-stage') || p.bot.classList.contains('leaving')) return;
        if (p.video && p.video.style.display === 'block') return;    // the video beat it
        var from = p.bot.getBoundingClientRect();
        clear();
        p.zone.classList.remove('cu-intro');
        p.zone.classList.add('cu-prep');
        p.bot.classList.remove('docked');
        p.bot.classList.add('preparing');
        var to = p.bot.getBoundingClientRect();
        if (reduced() || !from.height || !to.height) return;
        anims.push(p.bot.animate([
            { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.bottom - to.bottom) + 'px) scale(' + (from.height / to.height) + ')' },
            { transform: 'none' }
        ], { duration: MOVE, easing: EASE_EXPO }));
    }


    window.startScreenBot = function () {
        var p = parts();
        if (!p) return;
        clear();
        clearTimeout(prepTimer);
        p.bot.classList.remove('docked', 'preparing', 'leaving');
        p.zone.classList.remove('cu-prep');
        p.zone.classList.add('cu-stage', 'cu-intro');
        playBg(p);
        if (reduced()) {
            docked(p);
            return;
        }
        // The splash's hero: fades in, grows from 0.85x out of a blur, wakes.
        anims.push(p.body.animate([{ opacity: 0 }, { opacity: 1 }], { duration: FADE, fill: 'both' }));
        anims.push(p.body.animate([{ transform: 'scale(0.85)' }, { transform: 'scale(1)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(p.body.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                                  { duration: GROW, easing: EASE_SETTLE, fill: 'both' }));
        anims.push(wake(p.eyes));
        timer = setTimeout(function () { dock(); }, DOCK_AT);
    };

    // A reopened Computer use chat: the stage as its run left it, the moving
    // background with the run's video on it (chat.js shows the video next)
    // and the bot already gone.
    window.screenStageOn = function () {
        var p = parts();
        if (!p) return;
        window.stopScreenBot();
        p.bot.classList.add('leaving');
        p.zone.classList.add('cu-stage');
        playBg(p);
    };

    // The backend is compressing the run's video (it pushes this for the
    // latest run only): wait a moment for it, then show "Preparing video".
    window.runVideoPreparing = function (chatId) {
        var p = parts();
        if (!p || !p.zone.classList.contains('cu-stage')) return;
        if (chatId && window.currentSessionId !== chatId) return;
        var input = document.querySelector('.chat-input');
        if (input && input.classList.contains('agent-active')) return;   // a newer run is live
        clearTimeout(prepTimer);
        prepTimer = setTimeout(prepare, GRACE);
    };

    // No video after all (nothing was captured, or the player cannot read
    // it): stop waiting, the bot goes back to its corner over the last screen.
    // Once the bot has gone (a reopened chat's stage), the stage goes with
    // the video: the box is plain again.
    window.runVideoFailed = function (chatId) {
        var p = parts();
        clearTimeout(prepTimer);
        if (!p || (chatId && window.currentSessionId !== chatId)) return;
        if (p.bot.classList.contains('leaving')) return window.stopScreenBot();
        if (!p.zone.classList.contains('cu-prep')) return;
        p.zone.classList.remove('cu-prep');
        dock(MOVE);
    };

    window.screenBotLeave = function () {
        var p = parts();
        if (!p || !p.zone.classList.contains('cu-stage')) return;
        clearTimeout(prepTimer);
        p.bot.classList.add('leaving');
        p.zone.classList.remove('cu-prep', 'cu-intro');
    };

    window.stopScreenBot = function () {
        var p = parts();
        clearTimeout(prepTimer);
        if (!p) return;
        clear();
        p.zone.classList.remove('cu-stage', 'cu-intro', 'cu-prep');
        p.bot.classList.remove('docked', 'preparing', 'leaving');
        if (p.bg) p.bg.pause();
    };
})();
