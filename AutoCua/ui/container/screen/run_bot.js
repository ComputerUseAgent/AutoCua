// =====================================================================
// The run's screen stage, in the screen box (run_bot.css): the moving
// background (logo/simulation_background.mp4) behind the run's screenshots,
// framed a little smaller than the box, for Computer use, Web use and iOS
// runs. The run's bot is not here any more: it comes in at the bottom
// container (container/agent/chain_bot.js).
// A Computer use run is recorded: once it has ended and its video is being
// compressed, "Preparing video" shows on the background (after a short
// grace, so a video that is ready at once just shows) until the video is
// in; if none comes, the last screen is back. A reopened Computer use chat
// shows its video on the same stage. Web use and iOS runs are not recorded:
// their screen box goes once the task is done (run_reply.js).
//   window.screenStageStart()          a run started (chat_input.js)
//   window.screenStageOn()             a reopened Computer use chat's stage (chat.js)
//   window.runVideoPreparing(chatId)   the backend: compressing the run's video
//   window.runVideoFailed(chatId)      the backend / the player: no video after all
//   window.screenStageVideoIn()        the run's video is in (screen.js)
//   window.screenStageOff()            back to the plain box (resetChatUi)
// =====================================================================
(function () {
    'use strict';

    var BG = '/simulation_background.mp4';     // AutoCua/logo/, served by ui/service.py
    var GRACE = 600;                           // ms: a video ready this soon just shows
    var prepTimer = null;

    function parts() {
        var zone = document.getElementById('screenContainer');
        var bg = zone && zone.querySelector('.cu-bg');
        if (!bg) return null;
        return { zone: zone, bg: bg, video: zone.querySelector('.run-video') };
    }

    function playBg(p) {
        if (!p.bg.getAttribute('src')) p.bg.src = BG;
        p.bg.muted = true;
        var playing = p.bg.play();
        if (playing && playing.catch) playing.catch(function () { /* stays on its first frame */ });
    }

    // "Preparing video": the last screen gives way to the words on the background.
    function prepare() {
        var p = parts();
        if (!p || !p.zone.classList.contains('cu-stage')) return;
        if (p.video && p.video.style.display === 'block') return;    // the video beat it
        p.zone.classList.add('cu-prep');
    }

    window.screenStageStart = function () {
        var p = parts();
        if (!p) return;
        window.screenStageOff();
        p.zone.classList.add('cu-stage');
        playBg(p);
    };

    // A reopened Computer use chat: the stage as its run left it, the moving
    // background with the run's video on it (chat.js shows the video next).
    window.screenStageOn = function () {
        var p = parts();
        if (!p) return;
        window.screenStageOff();
        p.zone.classList.add('cu-stage', 'cu-reopened');
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
    // it): stop waiting, the last screen is back. A reopened chat's stage
    // goes with its video: the box is plain again.
    window.runVideoFailed = function (chatId) {
        var p = parts();
        clearTimeout(prepTimer);
        if (!p || (chatId && window.currentSessionId !== chatId)) return;
        if (p.zone.classList.contains('cu-reopened')) return window.screenStageOff();
        p.zone.classList.remove('cu-prep');
    };

    window.screenStageVideoIn = function () {
        var p = parts();
        clearTimeout(prepTimer);
        if (p) p.zone.classList.remove('cu-prep');
    };

    window.screenStageOff = function () {
        var p = parts();
        clearTimeout(prepTimer);
        if (!p) return;
        p.zone.classList.remove('cu-stage', 'cu-prep', 'cu-reopened');
        p.bg.pause();
    };
})();
