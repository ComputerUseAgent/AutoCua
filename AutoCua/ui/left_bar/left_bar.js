// Left Bar loader — fetches the panel markup, injects it into the top document
// once, and reveals it when the splash animation finishes (or immediately if
// the splash is already gone). Kept out of the monolithic script.js so the
// component stays self-contained in its own folder.
(function () {
    'use strict';

    function reveal(bar) {
        // rAF so the initial hidden state is committed before the transition.
        requestAnimationFrame(function () { bar.classList.add('revealed'); });
    }

    function revealWhenReady(bar) {
        // Reveal as soon as the bar is injected — the splash overlay still covers
        // it until splashDone, so there is no extra fade lag after the animation.
        reveal(bar);
    }

    // GitHub star pill: live stargazer count (refreshed every 5 minutes), and
    // click-through to the repo. The click POSTs /api/open-github rather than
    // using a plain <a href>, because a link would navigate the pywebview
    // window itself instead of opening the system browser; the URL is fixed
    // server-side (service.py open_github) — keep the two in sync.
    // Every pill is wired: the header's and its copy in the Settings cover
    // (cornerCopy).
    var GH_REPO = 'AutoCua/AutoCua';
    function wireGithubPill() {
        var pills = document.querySelectorAll('.gh-star-pill');
        if (!pills.length) return;
        function fmt(n) {
            if (n >= 1000) return (n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, '') + 'k';
            return String(n);
        }
        function refresh() {
            fetch('https://api.github.com/repos/' + GH_REPO)
                .then(function (r) { return r.json(); })
                .then(function (d) {
                    if (d && typeof d.stargazers_count === 'number') {
                        var text = fmt(d.stargazers_count);
                        document.querySelectorAll('.gh-star-count').forEach(function (el) { el.textContent = text; });
                    }
                })
                .catch(function () { /* keep last value */ });
        }
        pills.forEach(function (pill) {
            pill.addEventListener('click', function () {
                fetch('/api/open-github', { method: 'POST' })
                    .catch(function () { /* non-fatal: the browser just doesn't open */ });
            });
        });
        refresh();
        setInterval(refresh, 5 * 60 * 1000);
    }

    // Settings covers the whole window and hides the bar under it
    // (left_bar.css), so the bar's header stands in the cover's top corners: a
    // copy without the ids, laid out at the same spot, logo left, pill right.
    function cornerCopy(bar) {
        var cover = document.getElementById('settingsOverlay');
        var header = bar.querySelector('#leftBarHeader');
        if (!cover || !header || cover.querySelector('.settings-corners')) return;
        var copy = header.cloneNode(true);
        copy.removeAttribute('id');
        copy.className = 'settings-corners';
        copy.querySelectorAll('[id]').forEach(function (el) { el.removeAttribute('id'); });
        cover.insertBefore(copy, cover.firstChild);
    }

    function inject() {
        fetch('left_bar/left_bar.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (document.getElementById('leftBar')) return; // guard double-inject
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var bar = holder.querySelector('.left-bar');
                if (!bar) return;
                document.body.appendChild(bar);
                // Let dependent components (e.g. the settings button) mount into
                // the bar now that it exists in the DOM.
                document.dispatchEvent(new CustomEvent('leftbar:ready', { detail: { bar: bar } }));
                cornerCopy(bar);
                wireGithubPill();
                revealWhenReady(bar);
            })
            .catch(function () { /* non-fatal: the bar simply won't render */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', inject);
    } else {
        inject();
    }
})();
