// Top-left container loader — injects the screenshot cell into #mainGrid
// (column 1, row 1), and owns its run video: window.showRunVideo /
// window.hideRunVideo.
(function () {
    'use strict';

    // The run's recording, in place of the screenshot once the run is over.
    // The backend pushes it when the video is saved and compressed
    // (_recording_done in ui/service.py). Dropped if a newer run is live or the
    // user has moved to another chat by then. It swaps in only once the player
    // has its first frame, so one it cannot play leaves the last screenshot up.
    window.showRunVideo = function (url, chatId) {
        var chatInput = document.querySelector('.chat-input');
        if (chatInput && chatInput.classList.contains('agent-active')) return;
        if (chatId && window.currentSessionId !== chatId) return;
        var zone = document.getElementById('zoneTopLeft');
        var video = zone && zone.querySelector('.run-video');
        if (!video) return;
        var img = zone.querySelector('.stream-image');
        // Until it plays, it shows the run's last screen (a recording opens on black).
        if (img && img.getAttribute('src')) video.poster = img.src;
        else video.removeAttribute('poster');
        video.onloadeddata = function () {
            if (video.videoHeight) {
                zone.style.setProperty('--screen-ar', String(video.videoWidth / video.videoHeight));
            }
            video.style.display = 'block';
            if (img) img.style.display = 'none';
        };
        video.onerror = function () { window.hideRunVideo(); };
        video.src = url;
    };

    // Take the video away (resetChatUi: a new run, New chat, another chat).
    window.hideRunVideo = function () {
        var zone = document.getElementById('zoneTopLeft');
        var video = zone && zone.querySelector('.run-video');
        if (!video) return;
        video.onloadeddata = video.onerror = null;
        video.pause();
        video.removeAttribute('src');
        video.removeAttribute('poster');
        video.load();                       // lets go of the file
        video.style.display = 'none';
        var img = zone.querySelector('.stream-image');
        if (img && img.getAttribute('src')) img.style.display = 'block';
    };

    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.screenshot-zone')) return;
        fetch('container/top_left/top_left.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.screenshot-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.screenshot-zone');
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
