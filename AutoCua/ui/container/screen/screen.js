// Screen container loader — injects the screen cell into #liveTurn
// (row 3), and owns its run video: window.showRunVideo /
// window.hideRunVideo.
(function () {
    'use strict';

    // The run's recording, in place of the screenshot once the run is over.
    // The backend pushes it when the video is saved and compressed
    // (_recording_done in ui/service.py), with the run's id. Dropped if the
    // user has moved to another chat by then. A run whose turn is an earlier
    // one by now (the next task was sent while it was compressed) gets it
    // there (chat_scroll.js); the live turn's, unless a newer run is live. It
    // swaps in only once the player has its first frame, so one it cannot
    // play leaves the last screenshot up.
    window.showRunVideo = function (url, chatId, runId) {
        if (chatId && window.currentSessionId !== chatId) return;
        if (window.chatScroll) {
            var turn = window.chatScroll.videoTurn(runId, url);
            if (turn !== document.getElementById('liveTurn')) {
                window.chatScroll.showVideo(turn, url);
                return;
            }
        }
        var chatInput = document.querySelector('.chat-input');
        if (chatInput && chatInput.classList.contains('agent-active')) return;
        if (window.chatScroll) window.chatScroll.setLiveScreen(true);
        var zone = document.getElementById('screenContainer');
        var video = zone && zone.querySelector('.run-video');
        if (!video) return;
        var img = zone.querySelector('.stream-image');
        // Until it plays, it shows the run's last screen (a recording opens on
        // black): the last screenshot, or with none (a reopened chat) its own
        // last frame, and it swaps in once that is there. Play starts it over.
        var poster = img && img.getAttribute('src');
        if (poster) video.poster = img.src;
        else video.removeAttribute('poster');
        function swapIn() {
            video.onloadeddata = video.onseeked = null;
            if (video.videoHeight) {   // on its turn (chat_scroll.css)
                (zone.closest('.chat-turn') || zone).style.setProperty('--screen-ar', String(video.videoWidth / video.videoHeight));
            }
            video.style.display = 'block';
            if (img) img.style.display = 'none';
            if (window.screenStageVideoIn) window.screenStageVideoIn();   // "Preparing video" goes
        }
        video.onloadedmetadata = poster ? null : function () {
            video.onloadedmetadata = null;
            if (isFinite(video.duration) && video.duration > 0) video.currentTime = video.duration;
        };
        video.onloadeddata = function () { if (!video.seeking) swapIn(); };
        video.onseeked = swapIn;
        video.onerror = function () {
            window.hideRunVideo();
            if (window.runVideoFailed) window.runVideoFailed();   // stop "Preparing video"
        };
        video.src = url;
    };

    // Take the video away (resetChatUi: a new run, New chat, another chat).
    window.hideRunVideo = function () {
        var zone = document.getElementById('screenContainer');
        var video = zone && zone.querySelector('.run-video');
        if (!video) return;
        video.onloadedmetadata = video.onloadeddata = video.onseeked = video.onerror = null;
        video.pause();
        video.removeAttribute('src');
        video.removeAttribute('poster');
        video.load();                       // lets go of the file
        video.style.display = 'none';
        var img = zone.querySelector('.stream-image');
        if (img && img.getAttribute('src')) img.style.display = 'block';
    };

    function mount() {
        var grid = document.getElementById('liveTurn');
        if (!grid || grid.querySelector('.screen-container')) return;
        fetch('container/screen/screen.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.screen-container')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.screen-container');
                if (zone) grid.appendChild(zone);
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
