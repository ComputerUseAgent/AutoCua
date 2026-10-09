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
    var GH_REPO = 'AutoCua/AutoCua';
    function wireGithubPill(bar) {
        var pill = bar.querySelector('#ghStarPill');
        if (!pill) return;
        var countEl = pill.querySelector('#ghStarCount');
        function fmt(n) {
            if (n >= 1000) return (n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, '') + 'k';
            return String(n);
        }
        function refresh() {
            fetch('https://api.github.com/repos/' + GH_REPO)
                .then(function (r) { return r.json(); })
                .then(function (d) {
                    if (d && typeof d.stargazers_count === 'number' && countEl) {
                        countEl.textContent = fmt(d.stargazers_count);
                    }
                })
                .catch(function () { /* keep last value */ });
        }
        pill.addEventListener('click', function () {
            fetch('/api/open-github', { method: 'POST' })
                .catch(function () { /* non-fatal: the browser just doesn't open */ });
        });
        refresh();
        setInterval(refresh, 5 * 60 * 1000);
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
                wireGithubPill(bar);
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
