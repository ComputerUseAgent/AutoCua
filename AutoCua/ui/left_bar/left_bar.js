// Left Bar — wires the panel (left_bar.tsx, in the page from the start) once
// and reveals it; the splash overlay covers it until splashDone. Kept out of
// the monolithic script.js so the component stays self-contained in its own
// folder.
(function () {
    'use strict';

    function reveal(bar) {
        // rAF so the initial hidden state is committed before the transition.
        requestAnimationFrame(function () {
            bar.classList.add('revealed');
            var toggle = document.getElementById('leftBarToggle');
            if (toggle) toggle.classList.add('revealed');
        });
    }

    // Collapse toggle on the bar's edge: body.left-bar-collapsed shrinks the
    // bar to an icon rail (left_bar.css). left-bar-animating turns on the
    // --left-bar-w transition just for this (css/style.css), so the main
    // content slides along with the bar while a window resize still snaps.
    var TOGGLE_MS = 450;   // the --left-bar-w transition
    function wireToggle(toggle) {
        var timer = null;
        toggle.addEventListener('click', function () {
            var body = document.body;
            body.classList.add('left-bar-animating');
            var collapsed = body.classList.toggle('left-bar-collapsed');
            var label = collapsed ? 'Expand sidebar' : 'Collapse sidebar';
            toggle.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
            toggle.setAttribute('aria-label', label);
            toggle.title = label;
            clearTimeout(timer);
            timer = setTimeout(function () {
                body.classList.remove('left-bar-animating');
            }, TOGGLE_MS + 100);
        });
    }

    function revealWhenReady(bar) {
        // Reveal as soon as the bar is wired — the splash overlay still covers
        // it until splashDone, so there is no extra fade lag after the animation.
        reveal(bar);
    }

    // GitHub button: opens the repo. The click POSTs /api/open-github rather
    // than using a plain <a href>, because a link would navigate the pywebview
    // window itself instead of opening the system browser; the URL is fixed
    // server-side (service.py open_github). Every copy is wired: the footer's
    // and its copy in the Settings cover (cornerCopy).
    function wireGithubLink() {
        document.querySelectorAll('.gh-link').forEach(function (link) {
            link.addEventListener('click', function () {
                fetch('/api/open-github', { method: 'POST' })
                    .catch(function () { /* non-fatal: the browser just doesn't open */ });
            });
        });
    }

    // Settings covers the whole window and hides the bar under it
    // (left_bar.css), so the bar's header stands in the cover's top corners: a
    // copy without the ids, laid out at the same spot, logo left, plus a copy
    // of the footer's GitHub button on the right.
    function cornerCopy(bar) {
        var cover = document.getElementById('settingsOverlay');
        var header = bar.querySelector('#leftBarHeader');
        if (!cover || !header || cover.querySelector('.settings-corners')) return;
        var copy = header.cloneNode(true);
        copy.removeAttribute('id');
        copy.className = 'settings-corners';
        var gh = bar.querySelector('#ghLink');
        if (gh) copy.appendChild(gh.cloneNode(true));
        copy.querySelectorAll('[id]').forEach(function (el) { el.removeAttribute('id'); });
        cover.insertBefore(copy, cover.firstChild);
    }

    // Once, a moment after DOMContentLoaded: when the bar's fetched markup
    // used to arrive, so everything keeps its order.
    var wired = false;
    function inject() {
        var bar = document.getElementById('leftBar');
        if (!bar || wired) return;
        wired = true;
        var toggle = document.getElementById('leftBarToggle');
        if (toggle) wireToggle(toggle);
        // Let dependent components (the settings button, New chat) wire up
        // their parts of the bar.
        document.dispatchEvent(new CustomEvent('leftbar:ready', { detail: { bar: bar } }));
        cornerCopy(bar);
        wireGithubLink();
        revealWhenReady(bar);
    }

    function schedule() { setTimeout(inject, 0); }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', schedule);
    } else {
        schedule();
    }
})();
