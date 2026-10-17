// =====================================================================
// The chat's scroll (chat_scroll.css). #chatScroll holds the chat's earlier
// turns (#chatPast) and then the live one (#liveTurn, whose zones the
// user, screen and agent containers' components fill and drive).
//   • Earlier turns are built here: from the live turn when the next task
//     is sent (archiveLive, chat_input.js) and from a reopened chat's saved
//     exchanges (addPast, chat/chat.js). Same classes as the live zones, so
//     they look the same; no ids, so the live hooks never reach them.
//   • Scrolling: a sent task's turn comes in below the last reply and the
//     chat glides up to it; a reopened chat opens on its last turn (toLive).
//     A run's reply glides the chat down to its last line (toEnd,
//     run_reply.js). The top and bottom edges fade while there is more chat
//     that way.
//   • A run's video can come after its turn became an earlier one (the next
//     task was sent while it was being compressed): videoTurn finds its turn
//     by run id or recording folder, showVideo puts it there (screen.js).
//   • Each turn knows its mode's bot (data-bot: 'computer', 'web', 'mobile'),
//     the one beside its reply (run_reply.css).
//   window.chatScroll = { archiveLive, addPast, clear, toLive, toEnd, setLive,
//                         setLiveScreen, setLiveBot, videoTurn, showVideo }
// =====================================================================
(function () {
    'use strict';

    var BG = '/simulation_background.mp4';      // the screen stage's background (run_bot.js)

    function byId(id) { return document.getElementById(id); }
    function div(cls) {
        var d = document.createElement('div');
        d.className = cls;
        return d;
    }
    // The recording folder in a run video's url (/api/recordings/<folder>/<file>),
    // the same name a saved exchange keeps as `recording`.
    function recOf(url) {
        var m = /\/api\/recordings\/([^/]+)\//.exec(url || '');
        return m ? m[1] : '';
    }
    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    // Where the chat stands when the live turn's top is one gap inside the
    // frame's top line (its message there).
    function liveTop(sc, live) {
        return live.offsetTop - (parseFloat(getComputedStyle(sc).paddingTop) || 0);
    }

    // ── The edge fades: on while the chat goes on past that edge ──────────
    // Also notes whether the view is at the chat's end or on the live turn,
    // so a window resize can keep it there (the turns' screens resize with
    // the frame).
    var fadeQueued = false, atLive = false, atEnd = false;
    function fades() {
        fadeQueued = false;
        var sc = byId('chatScroll'), live = byId('liveTurn');
        if (!sc) return;
        var rest = sc.scrollHeight - sc.clientHeight - sc.scrollTop;
        sc.classList.toggle('fade-top', sc.scrollTop > 1);
        sc.classList.toggle('fade-bottom', rest > 1);
        atEnd = rest <= 2;
        if (live) atLive = Math.abs(sc.scrollTop - Math.min(liveTop(sc, live), sc.scrollHeight - sc.clientHeight)) <= 2;
    }
    function queueFades() {
        if (fadeQueued) return;
        fadeQueued = true;
        requestAnimationFrame(fades);
    }

    // ── Each turn's screen fits its own message ──────────────────────────
    // The screen's height counts the turn's message box (chat_scroll.css
    // --turn-msg-h): a longer message, a smaller screen, so the live turn
    // still fits the frame; earlier turns look as they did live.
    var msgRO = null;
    function fitMsg(box) {
        var turn = box.parentElement;
        if (!turn || !turn.classList.contains('chat-turn')) return;
        if (box.classList.contains('is-empty')) turn.style.removeProperty('--turn-msg-h');
        else turn.style.setProperty('--turn-msg-h', box.offsetHeight + 'px');
    }
    function watchMsg(box) {
        if (!box) return;
        fitMsg(box);
        if (!('ResizeObserver' in window)) return;
        if (!msgRO) {
            msgRO = new ResizeObserver(function (entries) {
                entries.forEach(function (e) { fitMsg(e.target); });
            });
        }
        msgRO.observe(box);
    }

    // ── Media of earlier turns: loaded and played only near the view ──────
    var io = null;
    function wake(zone, near) {
        zone.dataset.near = near ? '1' : '';
        var video = zone.querySelector('.run-video');
        if (near && video && video.dataset.src && video.getAttribute('src') !== video.dataset.src) {
            video.src = video.dataset.src;
        }
        var bg = zone.querySelector('.cu-bg');
        if (!bg) return;
        if (near) {
            if (!bg.getAttribute('src')) bg.src = BG;
            var playing = bg.play();
            if (playing && playing.catch) playing.catch(function () { /* stays on its first frame */ });
        } else {
            bg.pause();
        }
    }
    function watch(zone) {
        if (!('IntersectionObserver' in window)) return wake(zone, true);
        if (!io) {
            io = new IntersectionObserver(function (entries) {
                entries.forEach(function (e) { wake(e.target, e.isIntersecting); });
            }, { root: byId('chatScroll'), rootMargin: '50% 0px' });
        }
        io.observe(zone);
    }
    function release(root) {
        if (msgRO) root.querySelectorAll('.user-container').forEach(function (b) { msgRO.unobserve(b); });
        root.querySelectorAll('video').forEach(function (v) {
            v.onloadedmetadata = v.onloadeddata = v.onseeked = v.onerror = null;
            v.pause();
            v.removeAttribute('src');
            v.load();                       // lets go of the file and the decoder
        });
    }

    // ── Building an earlier turn ──────────────────────────────────────────
    // The screen box of a turn, made on first need (a late video can come to
    // a turn that had nothing to show).
    function screenZone(turn) {
        var zone = turn.querySelector('.screen-container');
        if (zone) return zone;
        zone = div('container-zone screen-container' + (turn.dataset.stage ? ' cu-stage' : ''));
        if (turn.dataset.stage) {
            var bg = document.createElement('video');
            bg.className = 'cu-bg';
            bg.muted = true;
            bg.loop = true;
            bg.playsInline = true;
            bg.preload = 'none';
            bg.setAttribute('aria-hidden', 'true');
            zone.appendChild(bg);
        }
        zone.appendChild(div('screenshot-frame'));
        turn.insertBefore(zone, turn.querySelector('.agent-container'));
        turn.classList.remove('no-screen');
        watch(zone);
        return zone;
    }

    // Its run's video in a turn's screen box. Until it plays it shows the run's
    // last screen: the cover picture (the last screenshot, or the frame the
    // live player showed), or with none its own last frame. A video that
    // cannot play leaves the last screenshot up.
    function showVideo(turn, url, cover) {
        var zone = screenZone(turn);
        var frame = zone.querySelector('.screenshot-frame');
        var img = frame.querySelector('.stream-image');
        var video = frame.querySelector('.run-video');
        if (!video) {
            video = document.createElement('video');
            video.className = 'run-video';
            video.controls = true;
            video.playsInline = true;
            video.preload = 'metadata';
            frame.appendChild(video);
        }
        cover = cover || (img && img.getAttribute('src')) || '';
        if (cover) video.poster = cover;
        else video.removeAttribute('poster');
        function swapIn() {
            video.onloadeddata = video.onseeked = null;
            if (video.videoHeight) {
                zone.style.setProperty('--screen-ar', String(video.videoWidth / video.videoHeight));
            }
            video.style.display = '';
            if (img) img.style.display = 'none';
        }
        video.style.display = cover ? '' : 'none';
        if (cover && img) img.style.display = 'none';
        video.onloadedmetadata = cover ? null : function () {
            video.onloadedmetadata = null;
            if (isFinite(video.duration) && video.duration > 0) video.currentTime = video.duration;
        };
        video.onloadeddata = function () { if (!video.seeking) swapIn(); };
        video.onseeked = swapIn;
        video.onerror = function () {
            video.onloadedmetadata = video.onloadeddata = video.onseeked = video.onerror = null;
            delete video.dataset.src;          // not fetched again near the view; a late video sets it
            video.removeAttribute('src');
            video.style.display = 'none';
            if (img) img.style.display = '';
        };
        video.dataset.src = url;
        if (zone.dataset.near) video.src = url;     // else when it comes near the view
        if (!turn.dataset.rec) turn.dataset.rec = recOf(url);
    }

    // t = { text, stage, ar, image, video, cover, reply (html), ended, run, rec, msgH, bot }
    function buildTurn(t) {
        var turn = div('chat-turn');
        if (t.stage) turn.dataset.stage = '1';
        if (t.bot) turn.dataset.bot = t.bot;
        if (t.run) turn.dataset.run = t.run;
        if (t.rec) turn.dataset.rec = t.rec;
        if (t.msgH) turn.style.setProperty('--turn-msg-h', t.msgH);   // until it is measured in place

        var msg = div('container-zone user-container');
        var text = div('user-message-text');
        text.textContent = t.text || '';
        msg.appendChild(text);
        turn.appendChild(msg);

        if (t.reply != null) {
            var bottom = div('container-zone agent-container show-reply');
            var reply = div('bl-panel reply-panel md' + (t.ended ? ' is-ended' : ''));
            reply.innerHTML = String(t.reply);
            bottom.appendChild(reply);
            turn.appendChild(bottom);
        }

        if (t.image || t.video) {
            var zone = screenZone(turn);
            if (t.ar) zone.style.setProperty('--screen-ar', t.ar);
            if (t.image) {
                var img = document.createElement('img');
                img.className = 'stream-image';
                img.alt = 'Agent screen';
                img.src = t.image;
                zone.querySelector('.screenshot-frame').appendChild(img);
            }
            if (t.video) showVideo(turn, t.video, t.cover);
        } else {
            turn.classList.add('no-screen');
        }
        return turn;
    }
    // In the chat: its message box measured for its screen from now on.
    function place(turn) {
        byId('chatPast').appendChild(turn);
        watchMsg(turn.querySelector('.user-container'));
    }

    // The frame the live player shows, as a picture, so its copy shows it at
    // once instead of loading black.
    function frameOf(video) {
        try {
            if (!video || video.readyState < 2 || !video.videoWidth) return '';
            var c = document.createElement('canvas');
            c.width = video.videoWidth;
            c.height = video.videoHeight;
            c.getContext('2d').drawImage(video, 0, 0);
            return c.toDataURL('image/jpeg', 0.85);
        } catch (e) {
            return '';
        }
    }

    // The next task was sent: the live turn becomes an earlier one, as it is
    // now (message, video or last screenshot, reply), before its zones are
    // cleared for the new task. Nothing to keep (no task on screen yet, or a
    // send that never started): null. Returns where it was, for toLive.
    function archiveLive() {
        var live = byId('liveTurn'), past = byId('chatPast');
        var msgZone = byId('userContainer'), msg = byId('userMessageText');
        if (!live || !past || !msg || !msgZone || msgZone.classList.contains('is-empty')) return null;
        var screen = byId('screenContainer'), bottom = byId('agentContainer');
        var replyEl = (bottom && bottom.classList.contains('show-reply')) ? bottom.querySelector('.reply-panel') : null;
        var video = screen && screen.querySelector('.run-video');
        var img = screen && screen.querySelector('.stream-image');
        var vsrc = (video && video.getAttribute('src')) || '';
        var isrc = (img && img.getAttribute('src')) || '';
        if (!replyEl && !vsrc && !isrc) return null;
        var top = live.getBoundingClientRect().top;
        var noScreen = live.classList.contains('no-screen');
        var turn = buildTurn({
            text: msg.textContent,
            stage: !!(screen && screen.classList.contains('cu-stage')),
            ar: screen ? screen.style.getPropertyValue('--screen-ar') : '',
            image: noScreen ? '' : isrc,
            video: noScreen ? '' : vsrc,
            cover: vsrc ? (video.getAttribute('poster') || frameOf(video)) : '',
            reply: replyEl ? replyEl.innerHTML : null,
            ended: !!(replyEl && replyEl.classList.contains('is-ended')),
            run: live.dataset.run || '',
            rec: live.dataset.rec || recOf(vsrc),
            msgH: live.style.getPropertyValue('--turn-msg-h'),
            bot: live.dataset.bot || ''
        });
        place(turn);
        return { turn: turn, top: top };
    }

    // A chat's mode -> the bot beside its replies (run_reply.css): its own
    // (only iOS chats are saved as 'mobile'), or none (AutoCua's, a chat from
    // before modes were saved).
    function botOf(mode) {
        return (mode === 'computer' || mode === 'web' || mode === 'mobile') ? mode : '';
    }

    // A reopened chat's turns before its last one (its exchanges rows). A
    // Computer use chat's videos on the stage's background; Web use and iOS
    // turns keep no screen (not even an older Web use recording).
    function addPast(rows, mode) {
        var past = byId('chatPast');
        if (!past) return;
        (rows || []).forEach(function (x) {
            if (!x) return;
            place(buildTurn({
                text: x.task || '',
                stage: mode === 'computer',
                bot: botOf(mode),
                video: (mode === 'web' || mode === 'mobile') ? '' : (x.video_url || ''),
                reply: x.done_html || '',
                ended: (x.status || 'success') !== 'success',
                rec: x.recording || recOf(x.video_url)
            }));
        });
        queueFades();
    }

    // New chat, or another chat opened: no earlier turns.
    function clear() {
        var past = byId('chatPast');
        if (io) {
            io.disconnect();
            io = null;
        }
        if (past) {
            release(past);
            past.innerHTML = '';
        }
        var sc = byId('chatScroll');
        if (sc) sc.scrollTop = 0;
        queueFades();
    }

    // Which run (run id, from the backend's 'started') or saved recording the
    // live turn shows; a late video finds its turn by them.
    function setLive(run, rec) {
        var live = byId('liveTurn');
        if (!live) return;
        if (run) live.dataset.run = run;
        else delete live.dataset.run;
        if (rec) live.dataset.rec = rec;
        else delete live.dataset.rec;
    }

    // The live turn without its screen box (a reopened turn with nothing to
    // show), or with it again (a new run, a screenshot, its video).
    function setLiveScreen(on) {
        var live = byId('liveTurn');
        if (live) live.classList.toggle('no-screen', !on);
    }

    // The bot beside the live turn's reply: a sent task's (chat_input.js:
    // 'computer', 'web', 'mobile' for iOS) or a reopened chat's (chat.js, its
    // saved mode).
    function setLiveBot(bot) {
        var live = byId('liveTurn');
        if (!live) return;
        bot = botOf(bot);
        if (bot) live.dataset.bot = bot;
        else delete live.dataset.bot;
    }

    // The turn a run's video belongs to: the one with that run id, else the
    // one with that recording (a reopened chat's turns know only that). A
    // call with no run id (not a backend push) for a live turn that knows
    // neither: the live turn. null: no turn of this chat view (a pushed run
    // that saved no turn here yet).
    function videoTurn(runId, url) {
        var live = byId('liveTurn');
        var turns = [].slice.call(document.querySelectorAll('#chatPast > .chat-turn'));
        if (live) turns.push(live);
        function find(key, value) {
            if (!value) return null;
            for (var i = turns.length - 1; i >= 0; i--) {
                if (turns[i].dataset[key] === value) return turns[i];
            }
            return null;
        }
        var turn = find('run', runId) || find('rec', recOf(url));
        if (turn || runId || !live || live.dataset.run || live.dataset.rec) return turn;
        return live;
    }

    // Bring the live turn into view: its top one gap inside the frame's top
    // line. After archiveLive, the new earlier turn first takes the old live
    // turn's place on screen, then the chat glides up to the new one.
    function toLive(prev, instant) {
        var sc = byId('chatScroll'), live = byId('liveTurn');
        if (!sc || !live) return;
        if (prev && prev.turn && prev.turn.isConnected) {
            sc.scrollTop += prev.turn.getBoundingClientRect().top - prev.top;
        }
        var target = liveTop(sc, live);
        if (instant || reduced()) sc.scrollTop = target;
        else sc.scrollTo({ top: target, behavior: 'smooth' });
        atLive = true;
        queueFades();
    }

    // Down to the chat's last line, the live turn's end: a run's reply came
    // in (run_reply.js), the chat glides down to where it ends.
    function toEnd(instant) {
        var sc = byId('chatScroll');
        if (!sc) return;
        var target = sc.scrollHeight - sc.clientHeight;
        if (instant || reduced()) sc.scrollTop = target;
        else sc.scrollTo({ top: target, behavior: 'smooth' });
        queueFades();
    }

    function wire() {
        var sc = byId('chatScroll');
        if (!sc) return;
        sc.addEventListener('scroll', queueFades, { passive: true });
        if ('ResizeObserver' in window) {
            var ro = new ResizeObserver(queueFades);
            ro.observe(sc);
            ['chatPast', 'liveTurn'].forEach(function (id) { if (byId(id)) ro.observe(byId(id)); });
        }
        // A window resize: the turns' screens resize with the frame, so the view
        // stays at the chat's end, or on the live turn, if it was there. Their
        // message boxes are measured first (their text size follows the
        // window), so it lands where they leave it.
        if ('ResizeObserver' in window) {
            var size = '';
            new ResizeObserver(function () {
                var now = sc.clientWidth + 'x' + sc.clientHeight;
                if (now !== size && size && (atEnd || atLive)) {
                    sc.querySelectorAll('.chat-turn > .user-container').forEach(fitMsg);
                    if (atEnd) toEnd(true);
                    else toLive(null, true);
                }
                size = now;
            }).observe(sc);
        }
        // The live turn's message box, once its component has mounted it.
        var live = byId('liveTurn');
        if (live) {
            var seen = function () {
                var box = byId('userContainer');
                if (box && !box.dataset.fit) { box.dataset.fit = '1'; watchMsg(box); }
            };
            seen();
            new MutationObserver(seen).observe(live, { childList: true });
        }
        queueFades();
    }

    window.chatScroll = {
        archiveLive: archiveLive,
        addPast: addPast,
        clear: clear,
        toLive: toLive,
        toEnd: toEnd,
        setLive: setLive,
        setLiveScreen: setLiveScreen,
        setLiveBot: setLiveBot,
        videoTurn: videoTurn,
        showVideo: function (turn, url) { if (turn) showVideo(turn, url); }
    };

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', wire);
    } else {
        wire();
    }
})();
