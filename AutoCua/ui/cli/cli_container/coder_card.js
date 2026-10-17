// CoderCard — a reusable live card component for ONE CLI/coder agent.
//
// Small + self-contained so the orchestrator (cli/cli_stage.js) creates one per agent and
// can stack several later. Look ported from coder_animation.html + the proven native banner
// (banner.py COMPACT_HTML). Styles: cli/cli_container/coder_card.css (scoped .coder-card).
//
// API: create(desc) -> { el, setLine, addMinion, setMinionLine, endMinion,
//                        setWeb, setDone, dispose }
(function () {
    'use strict';

    function orbLoaderHTML() {
        var s = '';
        for (var k = 0; k < 5; k++) s += '<i style="--index:' + k + '"></i>';
        return '<span class="cc-loader show">' + s + '</span>';
    }

    // NO-DROP, CONSTANT-SPEED line streamer. A faithful port of the proven CLI pill
    // (ui/script.js _pumpCliRunner) and the native remote-connection banner
    // (remote_connection/banner.py pump): every incoming line is QUEUED and played one at a
    // time, each letter revealed left-to-right at a FIXED cadence — the pace NEVER depends on
    // line length or how much is queued. Long lines PAGINATE (overflow → hold → clear →
    // continue) so the COMPLETE content flows by, never ellipsis-clipped. We're explicitly OK
    // with the UI lagging real time: smoothness beats catching up. Shared by the `>` output
    // line and every minion row so they behave identically.
    var STAGGER   = 4;     // ms between ticks — fast & constant, never length-based
    var STEP      = 5;     // chars revealed per tick (timers clamp ~4ms, so batch for real speed)
    var CHAR_FADE = 30;    // ms opacity 0→1 fade per letter
    var PAGE_HOLD = 280;   // ms a full page lingers before clearing for the next page
    var LINE_HOLD = 110;   // ms between distinct lines
    // The Shell chat's answer types in letter by letter (mode 'type' / 'typehtml'): this
    // many letters a second, faster for a long one so none takes more than TYPE_MAX_S.
    var TYPE_CPS = 170;
    var TYPE_MAX_S = 5;
    // A command you typed is still running (the app has ONE hand-typed shell, so this is
    // shared): a terminal made meanwhile (New chat, another chat and back) knows it too.
    var cmdRunning = false;

    function reduced() {
        return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
    }

    // Playful "thinking" lines the head-side line cycles through while the coder's minions
    // are off working (inherited verbatim from remote_connection/banner.py CP_MSGS — same
    // trick: a backtick template literal avoids escaping the many apostrophes/quotes;
    // trim()+filter() strips the code indentation and blank edges).
    var CP_MSGS = (`
        summoned minions…
        digging through the codebase…
        don't bother me, i'm busy still
        reticulating splines…
        consulting the rubber duck…
        untangling spaghetti…
        watching minions go brrr…
        this is fine, everything is fine…
        spinning up neurons…
        arguing with the linter…
        minions are reading… patience, human
        pondering the orb…
        grepping the unknown…
        feeding the hamsters…
        still thinking, hold tight…
        writing tiny love letters to stdout…
        asking stack overflow nicely…
        looking for the missing semicolon…
        blaming the intern…
        performing arcane git rituals…
        have you tried turning it off and on…
        just one more refactor, i promise…
        regex go brrr…
        negotiating with the type checker…
        Schrödinger's bug: works on my machine…
        pretending to understand recursion…
        this codebase has feelings too…
        arguing with prettier…
        i swear i tested this earlier…
        checking if it's a feature, not a bug…
        the code works, nobody knows why…
        reading documentation as last resort…
        blaming the cache…
        praying to the build gods…
        git blame says it was past me…
        compiling existential dread…
        tabs vs spaces war ongoing…
        training a goldfish to write tests…
        minions arguing over indentation…
        still cheaper than a senior dev…
        pushing to prod on a friday…
        one does not simply async in python…
        404: motivation not found…
        rewriting it in rust… mentally…
        petting the dog, brb…
        yelling politely at the json…
        checking if it's plugged in…
        the cake is a bug…
        explaining mondays to the AI…
        deploying vibes…
        this stack trace feels personal…
        i was promised flying cars, got jira…
        writing tests… eventually…
        minions on coffee break…
        this wasn't in the spec…
        ctrl-z is my therapist…
        running from technical debt…
        console.log debugger gang…
        the docs lied to us…
        trying to remember what i was doing…
        rebooting reality…
        yes, that's a feature now…
        speedrun: any% blame git…
        thinking too hard, please wait…
        yet another deeply nested if…
        promise resolved with disappointment…
        hot reload, cold coffee…
        aligning ducks in rows…
        one liner that took two hours…
        naming things, the hardest problem…
        off-by-one somewhere, definitely…
        loading more excuses…
        the bug is in another castle…
        your code is fine, the universe is broken…
        the linter has strong opinions…
        minion overheard saying lgtm…
        convincing the tests to pass…
        renaming the variable to fix it…
        drowning in callback hell…
        73 unread warnings, vibes only…
        yes it works, no i don't know why…
        minions found 47 todos, ignored all…
        scrolling error logs like reels…
        two minions, one task…
        sacrificing a keyboard to the demo gods…
        undoing the undo…
        reading the error message, finally…
        minions whispering to each other…
        the algorithm has thoughts…
        trying not to break prod…
        minion union meeting in progress…
        shaking the magic 8-ball…
        asking the cat for code review…
        running tests with fingers crossed…
        exorcising the legacy code…
        i promise this is the last bug…
        binary search through 200 tabs…
        feature creep is a feature now…
        minions found a TODO from 2014…
        deprecated, but still working…
        putting console.logs in production…
        redefining what "done" means…
      `).split('\n').map(function (s) { return s.trim(); }).filter(Boolean);

    // opts.multiline: true  -> a 5-line scrolling terminal (the coder `>` output). Each pumped
    //   line APPENDS a wrapping .cc-line into a translateY conveyor (.cc-scroll); the viewport
    //   (`target`) is clamped to N rows in CSS so older rows scroll up out of view, and the
    //   bottom is re-pinned as the active line wraps — like terminal scrollback.
    // opts (default)        -> the original single-line paginating ticker (minion rows): one
    //   .cc-line page, no-drop, overflow holds → clears → continues on a fresh page.
    function makeLineStreamer(target, initial, opts) {
        opts = opts || {};
        var multiline = !!opts.multiline;
        var queue = [];
        var running = false;
        var timer = null;
        // Per-streamer pace, defaulting to the module constants. The Shell-use terminal
        // overrides these to run faster: its viewport is only 3 lines and each packet
        // REPLACES it, so a step's lines have to land before the next packet arrives.
        var TICK_MS = opts.stagger || STAGGER;
        var TICK_CHARS = opts.step || STEP;
        var HOLD_MS = (opts.lineHold != null) ? opts.lineHold : LINE_HOLD;
        // Rows kept in the scrollback (multiline). The Shell use terminal keeps the
        // whole chat; the compact card only ever shows a few.
        var KEEP = opts.keepLines || 120;
        // Queue sentinel (identity-compared, never a string): "wipe the screen HERE".
        // Sits between one step's lines and the next's — see reset().
        var CLEAR = {};

        // The trailing line keeps .cc-shimmer because the shimmer means "more is coming" —
        // it's only cleared when the NEXT line starts. When a hand-typed command has
        // exited, nothing is coming, so the caller asks the stream to settle: drop the
        // shimmer once everything queued has actually finished typing (not at exit — the
        // pump is usually still catching up then).
        var settleWanted = false;
        function dropShimmer() {
            var host = multiline ? scroll : target;
            if (!host || !host.children.length) return;
            var last = host.children[host.children.length - 1];
            if (last && last.classList) last.classList.remove('cc-shimmer');
        }

        function hardClear() {
            if (multiline) {
                scroll.replaceChildren();
                scroll.style.transform = 'translateY(0)';
                target.classList.remove('cc-scrolled');
                activeLine = null;
            } else {
                target.replaceChildren();
            }
        }

        // Multiline: the append-only conveyor + the line currently shimmering. maxLines drives the
        // viewport height via a CSS var (CSS computes the px from line-height).
        var scroll = null, activeLine = null;
        if (multiline) {
            scroll = document.createElement('div');
            scroll.className = 'cc-scroll';
            target.style.setProperty('--cc-max-lines', String(opts.maxLines || 5));
            target.replaceChildren(scroll);
        }
        // Two scroll modes. Default: the conveyor is translateY'd so old lines slide up
        // out of a fixed window — smooth, but the user can't go back. Native mode (the
        // hand-typed terminal): real scrollTop, so old output stays reachable.
        var nativeScroll = false;
        // Native mode: is the view at its end right now (the test scrollToEnd makes)?
        // Asked BEFORE a line goes in, so the line's own height can't say "no".
        function atEnd() {
            return nativeScroll && target.scrollHeight - target.scrollTop - target.clientHeight <= 24;
        }
        function scrollToEnd() {
            if (!scroll) return;
            if (nativeScroll) {
                // Follow the tail ONLY if the user is already at the bottom. If they've
                // scrolled up to read something, yanking them back down would be hostile.
                var slack = target.scrollHeight - target.scrollTop - target.clientHeight;
                if (slack <= 24) target.scrollTop = target.scrollHeight;
                return;
            }
            var over = Math.max(0, scroll.scrollHeight - target.clientHeight);
            scroll.style.transform = over ? ('translateY(' + (-over) + 'px)') : 'translateY(0)';
            target.classList.toggle('cc-scrolled', over > 0);   // fade the top edge ONLY once it scrolls
        }

        // The answer being typed (mode 'type' / 'typehtml'), or null: { finish, cancel }.
        // A copy of the markup grows node by node, so an element (a paragraph, a list
        // item, a code block) opens only when the typing reaches it, and its text fills
        // in a few letters a frame. The view follows it down smoothly while you are at the
        // end; scroll up and it leaves you there. Anything pushed meanwhile finishes it at
        // once, so a new line never waits.
        var typing = null;
        // Gliding to the end (native scroll): a fifth of the way each frame, chasing the
        // end while it moves (an answer still typing in), until it gets there. Scroll up
        // while it glides and it stops there (glideLeft) instead of pulling you back. A
        // drop that lands on the end is not you: the chat got shorter under it (the Shell
        // box closing back to its three lines after a run).
        var glideOn = false, glideAt = 0, glideRaf = 0, glideLeft = false;
        function glide() {
            if (!nativeScroll || glideOn || glideLeft) return;
            glideOn = true;
            glideAt = target.scrollTop;
            glideRaf = requestAnimationFrame(function step() {
                var d = target.scrollHeight - target.clientHeight - target.scrollTop;
                if (target.scrollTop < glideAt - 2 && d > 1) { glideOn = false; glideLeft = true; return; }
                if (d <= 0.5) { glideOn = false; return; }
                target.scrollTop += Math.max(1, d * 0.2);
                glideAt = target.scrollTop;
                glideRaf = requestAnimationFrame(step);
            });
        }
        function typeInto(line, src, isHtml, follow, done) {
            var from = document.createElement('div');
            if (isHtml) from.innerHTML = src; else from.textContent = src;
            var cps = Math.max(TYPE_CPS, from.textContent.length / TYPE_MAX_S);
            var stack = [{ src: from, dst: line, i: 0 }];
            var txt = null, txtSrc = null, shown = 0, raf = 0, ended = false;
            var t0 = (window.performance && performance.now) ? performance.now() : Date.now();
            glideLeft = false;
            function reveal(n) {             // n more letters; false once all is shown
                while (n > 0 && (txt || stack.length)) {
                    if (txt) {
                        var at = txt.data.length, k = Math.min(n, txtSrc.data.length - at);
                        var hi = txtSrc.data.charCodeAt(at + k - 1);
                        if (hi >= 0xD800 && hi <= 0xDBFF && at + k < txtSrc.data.length) k++;   // keep a pair whole
                        txt.data += txtSrc.data.substr(at, k);
                        n -= k;
                        if (txt.data.length >= txtSrc.data.length) txt = txtSrc = null;
                        continue;
                    }
                    var top = stack[stack.length - 1];
                    var node = top.src.childNodes[top.i++];
                    if (!node) { stack.pop(); continue; }
                    if (node.nodeType === 3) {
                        txtSrc = node;
                        txt = document.createTextNode('');
                        top.dst.appendChild(txt);
                    } else if (node.nodeType === 1) {
                        var copy = node.cloneNode(false);
                        top.dst.appendChild(copy);
                        stack.push({ src: node, dst: copy, i: 0 });
                    }
                }
                return !!(txt || stack.length);
            }
            function end() {
                if (ended) return;
                ended = true;
                typing = null;
                done();
            }
            function frame(now) {
                var want = Math.floor((now - t0) / 1000 * cps);
                if (want > shown) {
                    var more = reveal(want - shown);
                    shown = want;
                    if (follow) glide();     // it catches up with the new end, smoothly
                    if (!more) { end(); return; }
                }
                raf = requestAnimationFrame(frame);
            }
            raf = requestAnimationFrame(frame);
            typing = {
                finish: function () {
                    reveal(Infinity);
                    if (follow) glide();
                    cancelAnimationFrame(raf);
                    end();
                },
                cancel: function () { cancelAnimationFrame(raf); ended = true; typing = null; }
            };
        }

        // Render text into a single static page (no animation) — used for the initial label so
        // the row never flashes empty; the first streamed line replaces it (single-line) / appends
        // below it and scrolls away (multiline).
        function renderStatic(text) {
            var page = document.createElement('div');
            page.className = 'cc-line';
            page.textContent = text;
            if (multiline) { scroll.replaceChildren(page); activeLine = page; scrollToEnd(); }
            else { target.replaceChildren(page); }
        }
        if (initial != null) renderStatic(String(initial));

        function pump() {
            if (running || !queue.length) return;
            running = true;
            var item = queue.shift();
            // Step boundary reached: the previous step has finished drawing, so wipe now
            // and carry straight on with the next step's first line.
            if (item === CLEAR) { hardClear(); running = false; pump(); return; }
            // A queue entry is a bare string, or {text, cls} when the caller wants the
            // rendered line tagged (the terminal marks its prompt echoes this way).
            var plain = (typeof item === 'string');
            var text = plain ? item : item.text;
            var lineCls = plain ? '' : (item.cls ? ' ' + item.cls : '');
            // {mode:'instant'|'html'} (multiline): the line lands whole at once, in its
            // place in the queue: your command's echo, the run's answer (HTML from
            // markdown.py), a reopened chat's rows.
            if (multiline && !plain && (item.mode === 'type' || item.mode === 'typehtml')) {
                if (activeLine) activeLine.classList.remove('cc-shimmer');
                var typed = document.createElement('div');
                typed.className = 'cc-line' + lineCls;
                var follow = atEnd() || glideOn;
                scroll.appendChild(typed);
                activeLine = typed;
                while (scroll.children.length > KEEP) scroll.removeChild(scroll.firstChild);
                typeInto(typed, text, item.mode === 'typehtml', follow, function () { running = false; pump(); });
                return;
            }
            if (multiline && !plain && item.mode) {
                if (activeLine) activeLine.classList.remove('cc-shimmer');
                var whole = document.createElement('div');
                whole.className = 'cc-line' + lineCls;
                if (item.mode === 'html') whole.innerHTML = text;
                else whole.textContent = text;
                var stickWhole = atEnd();       // before it goes in: a tall line must not count
                scroll.appendChild(whole);
                activeLine = whole;
                while (scroll.children.length > KEEP) scroll.removeChild(scroll.firstChild);
                if (stickWhole) target.scrollTop = target.scrollHeight; else scrollToEnd();
                running = false;
                pump();
                return;
            }
            // Array.from splits by code point so emoji/surrogate pairs stay intact.
            var chars = Array.from(text);
            if (!chars.length) { running = false; pump(); return; }

            if (multiline) {
                // The previously finished line shimmered while waiting — stop it now that a new
                // line begins (matches the single-line path: dark text streams, THEN shimmers).
                if (activeLine) activeLine.classList.remove('cc-shimmer');
                var mline = document.createElement('div');
                mline.className = 'cc-line' + lineCls;   // pre-wrap (CSS) -> real multi-row wrapping
                var stickLine = atEnd();
                scroll.appendChild(mline);
                activeLine = mline;
                while (scroll.children.length > KEEP) scroll.removeChild(scroll.firstChild);   // memory cap (off-screen rows)
                if (stickLine) target.scrollTop = target.scrollHeight; else scrollToEnd();

                var mi = 0;
                (function mtick() {
                    for (var n = 0; n < TICK_CHARS; n++) {
                        if (mi >= chars.length) {
                            // Line fully streamed — flatten spans and add the loading shimmer
                            // (shines until the next line starts / the step ends).
                            mline.textContent = mline.textContent;
                            mline.classList.add('cc-shimmer');
                            scrollToEnd();               // a short line typed in this very tick
                            timer = setTimeout(function () {
                                running = false;
                                // nothing left to play and the caller asked to settle →
                                // this really is the last line, so stop it shimmering
                                if (settleWanted && !queue.length) dropShimmer();
                                pump();
                            }, HOLD_MS);
                            return;
                        }
                        var firstOnLine = mline.childElementCount === 0;
                        let span = document.createElement('span');   // let: own binding per char (fade closure)
                        span.className = 'cc-char';
                        span.textContent = chars[mi];
                        span.style.opacity = '0';
                        span.style.transition = 'opacity ' + CHAR_FADE + 'ms ease-out';
                        mline.appendChild(span);
                        // First char of a line shows instantly (no empty flash); the rest fade in.
                        if (firstOnLine) { span.style.opacity = '1'; }
                        else { requestAnimationFrame(function () { span.style.opacity = '1'; }); }
                        mi++;
                    }
                    scrollToEnd();                       // line may have wrapped onto a new row -> re-pin bottom
                    timer = setTimeout(mtick, TICK_MS);
                })();
                return;
            }

            // ---- single-line paginating path (minion rows) — unchanged ----
            var page = null;
            function startPage() {
                page = document.createElement('div');
                page.className = 'cc-line' + lineCls;
                target.replaceChildren(page);
            }
            startPage();

            var i = 0;
            (function tick() {
                for (var n = 0; n < TICK_CHARS; n++) {     // reveal a few chars per tick for speed
                    if (i >= chars.length) {
                        // Line fully streamed — flatten the char spans to plain text and add a
                        // loading shimmer (it shines until the next line replaces it / the step ends).
                        page.textContent = page.textContent;
                        page.classList.add('cc-shimmer');
                        timer = setTimeout(function () {
                                running = false;
                                // nothing left to play and the caller asked to settle →
                                // this really is the last line, so stop it shimmering
                                if (settleWanted && !queue.length) dropShimmer();
                                pump();
                            }, HOLD_MS);
                        return;
                    }
                    var firstOnPage = page.childElementCount === 0;
                    let span = document.createElement('span');   // let: own binding per char (fade closure)
                    span.className = 'cc-char';
                    span.textContent = chars[i];
                    span.style.opacity = '0';
                    span.style.transition = 'opacity ' + CHAR_FADE + 'ms ease-out';
                    page.appendChild(span);
                    // Did this char push past the box's right edge? Measure on the page div (it has
                    // overflow:hidden so the overflow doesn't propagate). If it doesn't fit and isn't
                    // the only char on the page, retract it, hold the page, then restart it at the
                    // left edge of a fresh page.
                    if (page.scrollWidth > page.clientWidth + 1 && !firstOnPage) {
                        page.removeChild(span);
                        timer = setTimeout(function () {
                            startPage();
                            while (i < chars.length && /\s/.test(chars[i])) i++;   // drop leading space
                            tick();
                        }, PAGE_HOLD);
                        return;
                    }
                    // First char of a page shows instantly (no empty flash); the rest fade in.
                    if (firstOnPage) { span.style.opacity = '1'; }
                    else { requestAnimationFrame(function () { span.style.opacity = '1'; }); }
                    i++;
                }
                timer = setTimeout(tick, TICK_MS);
            })();
        }

        return {
            // push(text) — a plain line. push(text, cls) — the rendered .cc-line also gets
            // `cls`, so callers can mark specific lines (the terminal tags prompt echoes).
            // push(text, cls, mode) — mode 'instant' (whole at once) or 'html' (markup).
            push: function (text, cls, mode) {
                if (text == null || !String(text).trim()) return;
                if (typing) typing.finish();   // a typed answer never holds up what follows
                settleWanted = false;          // more IS coming again
                queue.push((cls || mode) ? { text: String(text), cls: String(cls || ''), mode: mode || '' } : String(text));
                if (queue.length > 600) queue.splice(0, queue.length - 600);   // memory safety only
                if (!running) pump();
            },
            // Wipe everything now, queued lines included (a reopened chat's terminal).
            clear: function () {
                if (timer) { clearTimeout(timer); timer = null; }
                if (typing) typing.cancel();
                cancelAnimationFrame(glideRaf); glideOn = false; glideLeft = false;
                queue.length = 0;
                running = false;
                settleWanted = false;
                hardClear();
            },
            // Start a fresh screen for the step that's about to be pushed, so the terminal
            // only ever shows one step at a time.
            //
            // Idle → wipe now. Mid-flight → do NOT cut the current step off: queue the wipe
            // so it lands after the last of its lines and before the new step's first. Two
            // packets in quick succession therefore show step N in full, then step N+1 —
            // rather than truncating N the moment N+1 arrives.
            reset: function () {
                if (!running && !queue.length) { hardClear(); return; }
                queue.push(CLEAR);
            },
            // "that was the last line" — stop the trailing shimmer, now if the stream is
            // already idle, otherwise as soon as the queue drains.
            settle: function () {
                settleWanted = true;
                if (!running && !queue.length) dropShimmer();
            },
            // Swap the conveyor for real scrolling (and back). Native mode drops the
            // translateY so scrollTop is free to move, and the top-fade mask goes with
            // it — a mask over scrollback just hides the line you scrolled up to read.
            setNativeScroll: function (on) {
                nativeScroll = !!on;
                if (!scroll) return;
                if (nativeScroll) {
                    scroll.style.position = 'static';
                    scroll.style.transform = 'none';
                    target.classList.remove('cc-scrolled');
                    target.scrollTop = target.scrollHeight;
                } else {
                    scroll.style.position = '';
                    scroll.style.transform = 'translateY(0)';
                    target.scrollTop = 0;
                    scrollToEnd();
                }
            },
            // Glide to the end (your message just went in).
            toEnd: function () { glideLeft = false; glide(); },
            dispose: function () {
                if (timer) { clearTimeout(timer); timer = null; }
                if (typing) typing.cancel();
                cancelAnimationFrame(glideRaf); glideOn = false;
                queue.length = 0; running = false;
            }
        };
    }

    // The agent's `action` array IS the list of tools used — and it already arrives in the
    // validated JSON we stream to the `>` line / minion rows. So we DON'T need any extra
    // backend events: we just reassemble that JSON from the streamed lines and read `action`.
    // A line buffer that JSON.parse()s after each line; calls onObject(obj) per complete
    // object, then resets. Non-JSON lines (when idle) are ignored; a runaway buffer is capped.
    function makeActionParser(onObject) {
        var buf = '';
        return function (line) {
            var s = String(line == null ? '' : line);
            if (buf === '' && s.replace(/^\s+/, '').charAt(0) !== '{') return;  // only start on a JSON object
            buf += s + '\n';
            try {
                var obj = JSON.parse(buf);
                buf = '';
                if (obj && typeof obj === 'object') onObject(obj);
            } catch (e) {
                if (buf.length > 200000) buf = '';   // safety: drop a malformed/runaway buffer
            }
        };
    }

    // Everything the agent REPLIED except `action` — thinking / memory / next_goal and
    // whatever the schema grows later, in the order the model wrote them. This is what the
    // Shell-use terminal streams: the agent's own words, not a synthesized per-action
    // narration. "null" thinking (the fast-mode skip convention) drops out.
    function proseLines(obj) {
        var out = [];
        if (!obj || typeof obj !== 'object') return out;
        Object.keys(obj).forEach(function (k) {
            if (k === 'action' || obj[k] == null) return;
            var v = obj[k];
            var text = String(typeof v === 'string' ? v : JSON.stringify(v)).trim();
            if (!text || text.toLowerCase() === 'null') return;
            out.push(text);
        });
        return out;
    }

    // The most label-worthy field of an action, for the icon's caption.
    function argOf(a) {
        return a.pattern || a.path || a.command || a.value || a.query || '';
    }
    function actionList(obj) {
        var actions = (obj && obj.action) || [];
        return Array.isArray(actions) ? actions : [actions];
    }

    // A clean, terminal-command-style narration line for ONE action — what the coder is
    // actually doing this step. We synthesize a readable line here instead of dumping the raw
    // JSON onto the `>` terminal.
    //
    // `all` = narrate EVERY action type. Without it, scratchpad/todo actions return '' and the
    // caller skips them, because the compact card surfaces those elsewhere (scratchpad → the
    // right tracker, todo → nowhere). The Shell-use terminal passes all=true: it's meant to be
    // the complete record of the step, so an action that produced no line there read as a
    // dropped packet.
    function clip(s, n) { s = String(s == null ? '' : s); return s.length > n ? s.slice(0, n - 1) + '…' : s; }
    // How many "- [ ]" / "- [x]" rows a todo_list action wrote (its value is markdown).
    function todoCount(a) {
        var m = String(argOf(a)).match(/- \[[ xX]\]/g);
        return m ? m.length : 0;
    }
    function narrationFor(a, all) {
        if (!a || typeof a !== 'object' || !a.type) return '';
        switch (a.type) {
            case 'shell':   return '$ ' + clip(a.command || a.value || '', 240);
            case 'view':    return 'view ' + (a.path || '') + ((a.start || a.end) ? ('  :' + (a.start || 0) + '-' + (a.end || 0)) : '');
            case 'grep':    return 'grep "' + clip(a.pattern || '', 120) + '"' + (a.path ? (' in ' + a.path) : '') + (a.glob ? (' (' + a.glob + ')') : '');
            case 'glob':    return 'glob ' + (a.pattern || a.path || '');
            case 'write':   return 'write ' + (a.path || '');
            case 'replace': return 'edit ' + (a.path || '');
            case 'web':     return 'web "' + clip(a.value || a.query || '', 160) + '"';
            case 'wait':    return 'wait ' + (a.value || '1') + 's';
            case 'minion':  return 'minion: ' + clip(a.value || a.query || '', 200);
            // Shell use prints the answer whole under it (cli_stage.js), so just 'done' there.
            case 'exit':    return 'done' + ((a.value && !all) ? (' — ' + clip(a.value, 240)) : '');
            // Surfaced elsewhere on the compact card, so silent there — but narrated in full
            // on the Shell-use terminal. todo_list's value is the whole markdown list, so it's
            // summarised by item count rather than dumped into a 3-line window.
            case 'scratchpad':  return all ? ('note: ' + clip(argOf(a), 200)) : '';
            case 'todo_list':   return all ? ('todo list: ' + todoCount(a) + ' items') : '';
            case 'update_todo': return all ? ('todo #' + (a.value || '?') + ' complete') : '';
            default:        return a.type + (argOf(a) ? (' ' + clip(argOf(a), 200)) : '');
        }
    }

    // opts.header: <name> — the Shell-use card: a chat with a terminal box under it.
    //   The scrollback above holds what cli_stage.js prints (your messages, the answers)
    //   and your hand-typed commands with their output; under it ONE live box: the agent
    //   head, the `>`, then the hello (a new chat), the reply's PROSE (every JSON key except
    //   `action`, a line each, the newest three in sight) while a run goes, or the prompt
    //   you type into. While a run goes the box is its tool box, as in the other modes: the
    //   tool chain and the tracking progress on its right, the minions' bots on its bottom
    //   left. Every line stays, runs follow one another in it. Asked for by cli_stage.js
    //   only; dispatched coder cards in the other modes are unchanged: no header, actions
    //   on a 5-line `> AutoCua Code`.
    function create(desc, opts) {
        opts = opts || {};
        var header = opts.header || '';
        var el = document.createElement('div');
        el.className = 'coder-card' + (header ? ' has-head' : '');
        // The terminal card holds ONLY the streamed output + todo + minion output. Nothing else
        // (the action chain and scratchpad live in their own zones beside the card).
        // With a header the body is a ROW: .cc-main holds the terminal, with an (initially
        // empty) todo column to its RIGHT — the wrapper is what lets the two sit side by
        // side without the head/brain/out/minions each becoming their own column.
        // .cc-minions frames the TRUNK (which reaches up to the `>`, or the Shell card's
        // head, and so must not be clipped); .cc-mflow is the conveyor window that clips,
        // and .cc-mtree is the column of rows that slides up inside it. See layoutTrunk().
        var MINIONS_HTML = '<div class="cc-minions"><span class="cc-trunk"></span>' +
                               '<div class="cc-mflow"><div class="cc-mtree"></div></div>' +
                           '</div>';
        var inner =
            // (the header row stays in the markup, hidden: setName writes to it)
            (header
                ? '<div class="cc-head"><span class="cc-hdot"></span><span class="cc-hname"></span></div>'
                : '') +
            // Shell use: the scrollback, then the live line, a box: the commands you typed
            // there with their output (.cc-tview, shown while you type), then its row: head,
            // `>`, the agent's words (.cc-think) and the prompt, a REAL input you can click
            // and type into, the minions' tree and their bots, then the sliding bar (shown
            // while you type, on the box's floor). The run's tool chain and tracking
            // progress join it below (create).
            // Compact card: no input, and .cc-out-name is a PINNED row (its
            // "AutoCua Code" label used to live inside the scrolling conveyor, so it scrolled
            // away once the output overflowed).
            (header
                ? '<div class="cc-term">' +
                      '<div class="cc-out-text"><div class="cc-out-view"></div></div>' +
                      '<div class="cc-out"><div class="cc-tview"></div>' +
                          '<div class="cc-prow"><canvas class="cc-mascot"></canvas><span class="cc-p">&gt;</span>' +
                              '<span class="cc-think"></span>' +
                              // contenteditable, not <input>: the blinking `_` is a ::after on
                              // this element, so it lands immediately after the typed text with
                              // nothing to measure.
                              '<span class="cc-cmd" contenteditable="plaintext-only" role="textbox" ' +
                                    'spellcheck="false" aria-label="terminal command"></span>' +
                          '</div>' +
                          // the minions' tree, hanging from the head: an L to each minion's row
                          MINIONS_HTML +
                          // and their bots, on the box's bottom left
                          '<div class="cc-mbots"></div>' +
                          '<div class="cc-progress"><span class="cc-fill"></span></div>' +
                      '</div>' +
                  '</div>'
                : '<div class="cc-out"><span class="cc-p">&gt;</span> <span class="cc-out-text">' +
                      '<div class="cc-out-name">AutoCua Code</div>' +
                      '<div class="cc-out-view"></div>' +
                  '</span></div>') +
            // the compact card's minion tree, under its `>` (the Shell card's is in its box)
            (header ? '' : MINIONS_HTML);
        el.innerHTML =
            '<div class="cc-body">' +
                (header ? '<div class="cc-main">' + inner + '</div><div class="cc-todo"></div>' : inner) +
            '</div>' +
            (header ? '' : '<div class="cc-progress"><span class="cc-fill"></span></div>');   // Shell use: in its box
        if (header) el.querySelector('.cc-hname').textContent = header;

        var outEl = el.querySelector('.cc-out');
        var pEl = el.querySelector('.cc-p');           // the `>` prompt — trunk anchors just below it
        var outText = el.querySelector('.cc-out-view');   // the scrolling viewport, not the wrapper
        var minionsEl = el.querySelector('.cc-minions');
        var trunk = el.querySelector('.cc-trunk');
        var flowEl = el.querySelector('.cc-mflow');       // conveyor window (clips)
        var treeEl = el.querySelector('.cc-mtree');       // the rows — slides inside it

        // The `>` terminal shows a clean, multi-line scrolling log of the coder's ACTIONS
        // (synthesized narration, NOT the raw JSON), auto-scrolling once it overflows.
        // Shell use: the chat's whole terminal, run FAST, every line kept and really
        // scrollable (native scroll; the view fills the card, coder_card.css). The stream
        // starts EMPTY in both variants: the name lives outside the conveyor now (header
        // row / pinned .cc-out-name), so it can't be scrolled away by the output.
        var outStream = makeLineStreamer(
            outText,
            null,
            header ? { multiline: true, maxLines: 3, step: 12, lineHold: 45, keepLines: 2000 }
                   : { multiline: true, maxLines: 5 }
        );
        // Shell use: the terminal box's lines (your typed commands and their output), kept
        // apart from the chat; really scrollable like it.
        var atChatEnd = true, viewH = 0;  // the chat view: at its end? its last height
        var tview = header ? el.querySelector('.cc-tview') : null;
        var termStream = tview ? makeLineStreamer(tview, null, { multiline: true, maxLines: 3, step: 12, lineHold: 45, keepLines: 2000 }) : null;
        if (termStream) termStream.setNativeScroll(true);
        // Shell use: the live line is the chat's last item, inside its scrolling view (after
        // the streamer's .cc-scroll), so it scrolls with the chat like any of its lines.
        var liveBox = header ? el.querySelector('.cc-out') : null;
        if (liveBox) outText.appendChild(liveBox);
        // ...and keeps its newest line in sight when it changes height (it opens; the
        // prompt row comes back after a command) — unless your wheel took it up.
        var termAtEnd = true, termH = 0, termRo = null;
        if (tview) {
            tview.addEventListener('scroll', function () {
                if (tview.scrollHeight - tview.scrollTop - tview.clientHeight <= 24) termAtEnd = true;
            }, { passive: true });
            if (window.ResizeObserver) {
                termRo = new ResizeObserver(function () {
                    if (tview.clientHeight === termH) return;
                    termH = tview.clientHeight;
                    if (termAtEnd) tview.scrollTop = tview.scrollHeight;
                });
                termRo.observe(tview);
            }
        }
        if (header) {
            outStream.setNativeScroll(true);
            // Shell use: the chat's edges fade where there is more to scroll to, as in the
            // main chat (chat_scroll.js); on a scroll, and as the chat or its view resizes.
            var fades = function () {
                outText.classList.toggle('cc-fade-top', outText.scrollTop > 1);
                outText.classList.toggle('cc-fade-bottom', outText.scrollHeight - outText.scrollTop - outText.clientHeight > 1);
            };
            // At the end of the chat, it stays there when its view changes height (the
            // live line opening into the terminal box, or closing): the newest lines
            // must not slip out of sight under it. Only your wheel takes it off the end
            // (below); a scroll that lands at the end puts it back. (A scroll event can't
            // say "left the end": the view may already be shorter again when it arrives.)
            viewH = outText.clientHeight;
            outText.addEventListener('scroll', function () {
                if (outText.scrollHeight - outText.scrollTop - outText.clientHeight <= 24) atChatEnd = true;
                fades();
            }, { passive: true });
            if (window.ResizeObserver) {
                var liveH = -1;
                var fadeRo = new ResizeObserver(function () {
                    // the view got shorter or taller, or the live line at its end grew or
                    // shrank (minion rows, the terminal opening): at the end, stay there
                    var lh = liveBox ? liveBox.offsetHeight : 0;
                    if (outText.clientHeight !== viewH || lh !== liveH) {
                        viewH = outText.clientHeight;
                        liveH = lh;
                        if (atChatEnd) outText.scrollTop = outText.scrollHeight;
                    }
                    fades();
                    rbotPlace();      // the run's bot keeps to the box's top (the lines above rewrapped)
                });
                fadeRo.observe(outText);
                if (outText.firstElementChild) fadeRo.observe(outText.firstElementChild);   // .cc-scroll
                if (liveBox) fadeRo.observe(liveBox);
            }
        }

        // ---- the run's bot (Shell use) ------------------------------------------------
        // The CoderUse bot beside the live box while a run goes, like Computer use's run bot
        // beside its tool box (container/agent/chain_bot.js; owner, 2026-10-17): outside the
        // box on its left, its top on the box's top, the place and size of an answer's bot.
        // It is in the chat's scrolling view, so it scrolls with the chat; the box makes room
        // for it (coder_card.css). In a fresh chat the bot standing on the chat box
        // (chat_input/mode_bot.js) flies up into this place itself, unseen here until it
        // lands. The run's answer comes in where the box was (the box goes under it), so this
        // bot is beside the answer already: it stays there while the answer fades in, then
        // the answer's own bot (its ::before, the same place and size) shows and this one
        // goes, in one frame: one bot all along.
        //   botIn()        the run started (or its card did): here, or on its way up
        //   botOn()        here (or on its way)? the run's answer then waits for it
        //   botToAnswer()  the run's answer just went in (with .cc-bot-wait)
        //   botEnd()       the run ended without an answer: it goes
        var HAND_MS = 400;                              // the answer's fade-in (runReplyIn) and a little
        var rbot = null, rbotState = '', rbotHand = 0, rbotTimer = null, rbotMove = null;   // state: '' | on | wait | handing
        if (header && liveBox) {
            rbot = document.createElement('div');
            rbot.className = 'cc-rbot';
            rbot.setAttribute('aria-hidden', 'true');
            rbot.innerHTML = '<img src="bots/coderuse.webp" alt="">' +
                             '<img class="cc-rbot-eyes" src="bots/coderuse-eyes.webp" alt="">' +
                             '<img src="bots/coderuse-shine.webp" alt="">';
            outText.insertBefore(rbot, liveBox);
        }
        // its top on the box's top, in the chat's own coordinates; not once it waits for the
        // answer (that took the box's old place, the box went under it)
        function rbotPlace() {
            if (rbot && (rbotState === 'on' || rbotState === 'wait')) rbot.style.top = liveBox.offsetTop + 'px';
        }
        // from where the chat box's bot landed to where its place is now (it moved on the way)
        function rbotDock(from) {
            if (rbotMove) rbotMove.cancel();
            rbotMove = null;
            var to = rbot.getBoundingClientRect();
            if (reduced() || !from.height || !to.height) return;
            if (Math.abs(to.left - from.left) <= 2 && Math.abs(to.top - from.top) <= 2) return;
            rbotMove = rbot.animate([
                { transform: 'translate(' + (from.left - to.left) + 'px, ' + (from.top - to.top) + 'px) scale(' + (from.height / to.height) + ')' },
                { transform: 'none' }
            ], { duration: 1100, easing: 'cubic-bezier(0.87, 0, 0.13, 1)' });   // == chain_bot.js dock()
        }
        function rbotOff() {
            clearTimeout(rbotTimer); rbotTimer = null;
            if (rbotMove) rbotMove.cancel();
            rbotMove = null;
            rbotHand++;                                    // a flight still on its way lands nowhere
            rbotState = '';
            if (!rbot) return;
            rbot.classList.remove('on', 'wait');
            rbot.style.removeProperty('top');
            rbot.querySelector('.cc-rbot-eyes').style.removeProperty('transform');
            el.classList.remove('cc-rbot-room', 'cc-rbot-handing');
        }
        function botIn() {
            if (!rbot || rbotState === 'on' || rbotState === 'wait') return;
            if (rbotState === 'handing') rbotOff();        // the last answer's hand-over ends now
            var mine = ++rbotHand;
            el.classList.add('cc-rbot-room');
            rbot.classList.add('on');
            rbotState = 'on';
            rbotPlace();
            if (window.modeBot && window.modeBot.ready('shell')) {
                rbotState = 'wait';
                rbot.classList.add('wait');
                window.modeBot.flyTo(rbot, function (at) {
                    if (mine !== rbotHand) return;
                    rbot.classList.remove('wait');
                    if (rbotState === 'wait') rbotState = 'on';
                    rbotDock(at);
                });
            }
        }
        function botOn() { return rbotState === 'on' || rbotState === 'wait'; }
        function botToAnswer() {
            if (!botOn()) return;
            rbotState = 'handing';                         // its place stays: the answer's bot's
            el.classList.add('cc-rbot-handing');
            el.classList.remove('cc-rbot-room');           // the box, under the answer now, back to the left
            var since = performance.now();
            (function hand() {
                // still flying up, or gliding on from where it landed: it gets there first
                // (its place is the answer's)
                if (rbot.classList.contains('wait') || (rbotMove && rbotMove.playState === 'running')) {
                    rbotTimer = setTimeout(hand, 60);
                    return;
                }
                var left = HAND_MS - (performance.now() - since);
                if (left > 0) { rbotTimer = setTimeout(hand, left); return; }
                rbotOff();                                 // the answer's own bot shows in the same frame
            })();
        }
        function botEnd() {
            if (!botOn() || !rbot) return;
            if (reduced() || rbotState === 'wait') return rbotOff();
            var mine = rbotHand;
            if (rbotMove) rbotMove.cancel();
            var out = rbotMove = rbot.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 300, easing: 'ease', fill: 'forwards' });
            out.onfinish = function () { if (mine === rbotHand) rbotOff(); };
            el.classList.remove('cc-rbot-room');
            rbotState = 'handing';                         // nothing else moves it now
        }
        // Shell use only: the agent's own words on the live line (and a new chat's hello),
        // a line per field, wrapping, three lines in sight (coder_card.css): the newest at
        // the bottom, older ones go up out of the box (native scroll, so the line sits
        // under the head and `>` and the box hugs a short hello).
        var thinkStream = header ? makeLineStreamer(el.querySelector('.cc-think'), null, { multiline: true, maxLines: 3 }) : null;
        if (thinkStream) thinkStream.setNativeScroll(true);
        // ...and its window always shows the newest: nobody scrolls it (no scrollbar, the wheel
        // goes to the chat), so whenever it or its lines change size (they wrap anew after a
        // resize or the left bar, it was hidden while you were in another mode) it goes back
        // to the end
        var thinkRo = null;
        if (thinkStream && window.ResizeObserver) {
            var thinkEl = el.querySelector('.cc-think');
            thinkRo = new ResizeObserver(function () { thinkEl.scrollTop = thinkEl.scrollHeight; });
            thinkRo.observe(thinkEl);
            thinkRo.observe(thinkEl.firstElementChild);   // its lines (.cc-scroll)
        }
        var minionStreams = {};   // minion id -> its line streamer

        // While minions are off working the coder says nothing for a long stretch, so the
        // line beside the AI head would just sit on the last step's words. Instead it cycles
        // the playful CP_MSGS pool: the old output is wiped, then a random line types in,
        // shimmers for FUN_HOLD_MS, and the next one replaces it — until the next real
        // packet's prose takes the spot back (stopped in the parser below).
        var FUN_HOLD_MS = 3000;
        var funTimer = null, lastFun = -1;
        function cycleFun() {
            if (!thinkStream) return;
            var i = Math.floor(Math.random() * CP_MSGS.length);
            if (CP_MSGS.length > 1 && i === lastFun) i = (i + 1) % CP_MSGS.length;   // no immediate repeat
            lastFun = i;
            thinkStream.reset();            // the last one goes once it has typed in full
            thinkStream.push(CP_MSGS[i]);   // types, then shimmers
            funTimer = setTimeout(cycleFun, FUN_HOLD_MS);
        }
        function startFunLines() {
            if (!thinkStream || funTimer) return;
            cycleFun();                     // (its reset drops the step's old words beside the head)
        }
        function stopFunLines() { if (funTimer) { clearTimeout(funTimer); funTimer = null; } }

        // LEFT zone — "Tool response: N tools used" + the vertical action-icon chain (extreme
        // left of the stage, mirroring the bottom-left card). The orchestrator places it.
        var chainEl = document.createElement('div');
        chainEl.className = 'cc-actions';
        chainEl.innerHTML =
            '<div class="cc-tools-label">Tool response: <span class="cc-tools-count">0</span> tools used</div>' +
            '<div class="cc-chain-flow"><div class="cc-chain"></div></div>';
        var toolsCountEl = chainEl.querySelector('.cc-tools-count');
        // UNIVERSAL tool counter: the coder card continues the SAME running total as the main
        // agent (agent.js's #toolUsedCount). Coder AND minion tools bump it in real time, so the
        // card shows main-agent + coder + minion tools, and because it's the same element, the
        // total rolls back up to the main agent when the cli stage closes. Falls back to a local
        // count when the main-agent counter isn't present (e.g. the standalone test harness).
        var localCount = 0;
        function gCountEl() { return document.getElementById('toolUsedCount'); }
        function readCount() { var g = gCountEl(); return g ? (parseInt(g.textContent, 10) || 0) : localCount; }
        function showCount() { if (toolsCountEl) toolsCountEl.textContent = String(readCount()); }
        function bumpToolCount() {
            var g = gCountEl();
            if (g) g.textContent = String((parseInt(g.textContent, 10) || 0) + 1);   // universal counter
            else localCount += 1;                                                     // harness fallback
            showCount();
        }
        showCount();   // seed the card from the main agent's current count — continue from there

        // RIGHT zone — "tracking progress" (the coder's scratchpad), mirroring the app's
        // tracking-progress stream: NO logo; each scratchpad entry streams in char-by-char as a dot-bullet line.
        var trackEl = document.createElement('div');
        trackEl.className = 'cc-track';
        trackEl.innerHTML =
            '<div class="cc-track-label">tracking progress</div>' +
            '<div class="cc-track-flow"><div class="cc-track-tree"></div></div>';

        // Shell use: both live in the live box, on its right after the agent's words, while a
        // run goes (coder_card.css), like the other modes' tool box.
        if (liveBox) { liveBox.appendChild(chainEl); liveBox.appendChild(trackEl); }

        var ICONS = window.CliToolIcons;
        // the header's blinking agent head (same mark as the chain's `agent` icon)
        var mascotEl = el.querySelector('.cc-mascot');
        // 26px canvas -> a ~20px head; while working it wears the stop orb's colour
        var mascot = (mascotEl && ICONS && ICONS.createMascot) ? ICONS.createMascot(mascotEl, 26) : null;
        var actionChain = ICONS ? ICONS.createChain(chainEl.querySelector('.cc-chain'), { orientation: 'vertical' }) : null;
        function makeTracker() {
            return ICONS ? ICONS.createTracker(trackEl.querySelector('.cc-track-tree'), trackEl.querySelector('.cc-track-flow'), trackEl) : null;
        }
        var tracker = makeTracker();
        // Shell use: the chain and the tracking keep their newest in sight when their columns
        // change size (a resize, the left bar, back in Shell use mid-run: hidden, they
        // measured nothing)
        var colRo = null;
        if (liveBox && window.ResizeObserver) {
            colRo = new ResizeObserver(function () {
                if (actionChain && actionChain.fit) actionChain.fit();
                if (tracker && tracker.fit) tracker.fit();
            });
            colRo.observe(chainEl.querySelector('.cc-chain-flow'));
            colRo.observe(trackEl.querySelector('.cc-track-flow'));
        }
        var minionChains = {};         // minion id -> its own horizontal chain
        var minionParsers = {};        // minion id -> its action parser
        var noteLog = [];              // every scratchpad note this run — read back at task end

        function pushActions(chain, obj, isCoder) {
            actionList(obj).forEach(function (a) {
                if (!a || typeof a !== 'object' || !a.type) return;
                // coder -> stream a clean, terminal-style narration line on the `>` output
                // (narrationFor returns '' for scratchpad/todo, so those are skipped here).
                // Not in Shell use: its chat shows only the agent's words, on the live line;
                // the steps play on the live box's tool chain.
                if (isCoder && !header) { var n = narrationFor(a, false); if (n) outStream.push(n); }
                // scratchpad -> the right "tracking progress" stream (coder only; not minions).
                // Also LOGGED (noteLog) so the run's notes survive the card: cli_stage.js reads
                // them via getNotes() at task end and shows them as Agent Notes under the terminal.
                if (a.type === 'scratchpad') {
                    if (isCoder) {
                        var note = argOf(a);
                        if (note != null && String(note).trim()) {   // blanks would render as empty note rows
                            noteLog.push(note);
                            if (noteLog.length > 200) noteLog.shift();   // memory cap
                        }
                        if (tracker) tracker.push(note);
                    }
                    return;
                }
                // The CODER's tools queue behind the opening beats (so the chain always reads
                // opening -> tick -> tools, even mid-catch-up); minion sub-chains have no
                // opening phase and keep adding directly.
                if (chain === actionChain) { toolQ.push({ name: a.type, arg: argOf(a) }); playTools(); return; }
                if (chain && chain.addAction({ name: a.type, arg: argOf(a) })) bumpToolCount();
            });
        }

        // Opening-phase flow, ported from the main agent's tool-flow (agent.js): the wait for
        // each packet is told in beats — "communicating with llm service", then the composing
        // sash ("thinking") holds for THINK_MS no matter what, then the loader ("synthesizing")
        // spins until the packet lands and ticks over to "synthesizing complete". This step's
        // tools then play at TOOL_GAP_MS, and the chain anticipates the next step. Driven purely
        // by JSON-arrival timing — no backend signal. If the packet beats the opening, the
        // remaining beats fast-forward at CATCHUP_MS so the chain catches the real speed instead
        // of lagging behind it — then the next opening runs at normal pace again.
        var STEP_GAP_MS = 850;     // beat before the thinking sash appears
        var THINK_MS = 5000;       // the sash's MAX hold — a packet landing sooner fast-forwards it (same as agent.js)
        var CATCHUP_MS = 300;      // opening pace once the packet has already landed
        var TOOL_GAP_MS = 340;     // fast cadence between this step's tools
        var thinkingStep = null, openTimer = null, toolsRevealed = false;
        var openNext = null, receivedEarly = false, fastOpening = false, openingDone = false;
        var toolQ = [], toolTimer = null, anticipateNext = false;
        function revealTools() { if (!toolsRevealed) { toolsRevealed = true; chainEl.classList.add('cc-tools-active'); } }
        function startOpening() {
            if (!actionChain) return;
            revealTools();
            clearTimeout(openTimer);
            thinkingStep = null; receivedEarly = false; fastOpening = false; openingDone = false;
            var specs = [
                { shape: 'server',    label: 'communicating with llm service' },
                { shape: 'composing', label: 'thinking' },
                { shape: 'loader',    label: 'synthesizing', tick: true }
            ];
            var i = 0;
            function play() {
                var sp = specs[i];
                i++;
                if (i === specs.length) {                    // the loader — spins until the packet
                    thinkingStep = actionChain.addStep(sp);
                    openingDone = true;
                    openNext = null;                         // opening's done — nothing left to hurry
                    if (receivedEarly) { receivedEarly = false; finishThinking(); }
                    playTools();
                    return;
                }
                actionChain.addStep(sp);
                var wait = fastOpening ? CATCHUP_MS
                    : (sp.shape === 'composing' ? THINK_MS : STEP_GAP_MS);
                openTimer = setTimeout(play, wait);
            }
            openNext = play;
            play();
        }
        function finishThinking() {
            if (thinkingStep) { thinkingStep.complete('synthesizing complete'); thinkingStep = null; }
        }
        function markReceived() {
            anticipateNext = false;                          // the packet's own flags set it below
            if (thinkingStep) { finishThinking(); playTools(); return; }
            if (openNext) {
                // The packet beat the opening (often well inside the thinking hold). Don't sit
                // out the rest of it — drop to catch-up pace so the chain tracks the real speed.
                receivedEarly = true; fastOpening = true;
                clearTimeout(openTimer);
                openTimer = setTimeout(openNext, CATCHUP_MS);
                return;
            }
            // no opening in flight at all (e.g. back-to-back packets) — still land the beat
            if (actionChain) actionChain.addStep({ shape: 'loader', label: 'synthesizing', tick: true }).complete('synthesizing complete');
        }
        // Drain the coder's tool queue once the opening has ticked; when it runs dry,
        // anticipate the next step (unless this packet ended in exit / a minion dispatch).
        function playTools() {
            if (!openingDone || toolTimer) return;
            if (!toolQ.length) {
                if (anticipateNext) { anticipateNext = false; startOpening(); }
                return;
            }
            var tool = toolQ.shift();
            if (actionChain && actionChain.addAction(tool)) bumpToolCount();
            toolTimer = setTimeout(function () { toolTimer = null; playTools(); }, TOOL_GAP_MS);
        }
        function hasExit(obj) { return actionList(obj).some(function (a) { return a && a.type === 'exit'; }); }
        function hasMinion(obj) { return actionList(obj).some(function (a) { return a && a.type === 'minion'; }); }

        // ---- hand-typed terminal (Shell use) ----------------------------------------
        // The `>` prompt is a real input: Enter runs the command through /api/shell-exec
        // (zsh on macOS, PowerShell on Windows) and its output lands in the same stream the
        // agent writes to. No LLM involved — this is the user driving the shell directly.
        // A hand-typed command is LIVE: the route returns as soon as it spawns and the
        // output arrives afterwards through window.shellTermLines. termBusy marks that
        // window — Enter is ignored (one prompt, one command) and .cc-busy dims the
        // prompt — until window.shellTermEnd reports the exit code.
        var termBusy = false;
        function setBusy(on) {
            termBusy = !!on;
            if (header) cmdRunning = termBusy;
            el.classList.toggle('cc-busy', termBusy);
        }

        var cmdEl = el.querySelector('.cc-cmd');
        if (cmdEl && cmdRunning) setBusy(true);   // a typed command still running from before

        // Prompt label. Card-scope (not inside the block below) so begin() can clear it
        // too — under 'use strict' a function declared in a block isn't visible outside.
        // The location stays up for the WHOLE of user mode, not just while focused: it's
        // the terminal's identity, and it stamps every executed line.
        // The header names whoever owns the terminal: "AutoCua Code" while the agent has
        // it, "AutoCua Terminal" once you click in and start driving it by hand.
        var nameEl = el.querySelector('.cc-hname');
        function setName(t) { if (nameEl) nameEl.textContent = t; }

        var curPath = '';
        function showPath(short) {
            if (!short) return;
            curPath = short;
            // the live prompt shows a long folder by its last two folders (the echo keeps it whole)
            var parts = String(short).split('/');
            var label = (header && short.length > 28 && parts.length > 2) ? '\u2026/' + parts.slice(-2).join('/') : short;
            pEl.textContent = label + ' >';
        }
        function clearPrompt() { curPath = ''; pEl.textContent = '>'; }
        function loadPath() {
            fetch('/api/shell-cwd')
                .then(function (r) { return r.json(); })
                .then(function (d) { if (!el.classList.contains('cc-running')) showPath(d.short); })
                .catch(function () { /* leave the plain `>` */ });
        }

        // Output exists → the prompt belongs on the LAST line (CSS flips the order).
        // Until then it stays on top, so clicking an empty terminal doesn't fling the
        // prompt to the bottom of a tall empty box.
        function markOut() { el.classList.add('cc-has-out'); }

        if (cmdEl) {
            // NOTE: the path is NOT loaded here. An untouched terminal shows a bare `>_`,
            // exactly like AI mode; the location appears when you click into it.
            // While the prompt is focused the `>` is replaced by the shell's actual
            // location, so `cd` is obvious; it goes back to `>` on blur to keep the
            // agent view clean.
            // WKWebView refuses to wheel-scroll an overflow area inside a transformed
            // subtree, and #cliContainer carries a transform — so the view would look
            // scrollable and simply not move. scrollTop DOES work, so drive it from the
            // wheel event ourselves (same fix as the model dropdown in script.js).
            // The whole card wheels the chat, over the live line too. Whatever under the
            // pointer scrolls on its own goes first while it can still move that way: the
            // terminal box's lines, a long message, a wide code block or table sideways.
            var canMove = function (v, dy) {
                return !!v && (dy < 0 ? v.scrollTop > 0 : v.scrollTop + v.clientHeight < v.scrollHeight - 1);
            };
            var canSide = function (v, dx) {
                return dx < 0 ? v.scrollLeft > 0 : v.scrollLeft + v.clientWidth < v.scrollWidth - 1;
            };
            var scrolls = function (v, axis) {
                var o = getComputedStyle(v)[axis];
                return o === 'auto' || o === 'scroll';
            };
            el.addEventListener('wheel', function (e) {
                var k = (e.deltaMode === 1 ? 16 : 1), dx = e.deltaX * k, dy = e.deltaY * k;
                var side = Math.abs(dx) > Math.abs(dy);
                for (var n = e.target; n && n !== el && n !== outText; n = n.parentNode) {
                    if (n.nodeType !== 1) continue;
                    if (side ? (scrolls(n, 'overflowX') && canSide(n, dx)) : (scrolls(n, 'overflowY') && canMove(n, dy))) {
                        if (side) n.scrollLeft += dx; else n.scrollTop += dy;
                        if (n === tview) termAtEnd = tview.scrollHeight - tview.scrollTop - tview.clientHeight <= 24;
                        e.preventDefault();
                        return;
                    }
                }
                if (side || !canMove(outText, dy)) return;
                outText.scrollTop += dy;
                atChatEnd = outText.scrollHeight - outText.scrollTop - outText.clientHeight <= 24;
                e.preventDefault();
            }, { passive: false });

            cmdEl.addEventListener('focus', function () {
                // In Auto the prompt is not yours: a click on it (the browser
                // focuses an editable line by itself) never switches to Manual,
                // only the chat box's Auto / Manual pill does (owner, 2026-10-17).
                var pill = document.getElementById('agentModeWrap');
                if (!pill || pill.dataset.shellInput !== 'manual') { cmdEl.blur(); return; }
                // typing gets the full height down to the bar; a sent task puts it back
                // body-level, not card-level: the hard cap is enforced on the grid row
                // that holds the card, which a class on the card itself can't reach.
                document.body.classList.add('cli-typing');
                setName('AutoCua Terminal');       // it's yours now, not the coder's
                outStream.setNativeScroll(true);   // old output stays reachable
                loadPath();
                // typing here IS Manual: the chat box's Auto / Manual pill follows
                // (agent_mode.js; a no-op when it already says Manual)
                document.dispatchEvent(new CustomEvent('shellinput:set', { detail: { mode: 'manual' } }));
            });
            // NOTE: no blur handler — the path is not a focus affordance, it belongs to
            // user mode as a whole. Only begin() takes it away.

            // In Manual, click anywhere on the card and you're at the prompt,
            // like a real terminal — except over the todo column, and except
            // while the agent owns it. On `click` (after mouseup), NOT mousedown: focusing during
            // a mousedown+setTimeout raced the browser's own focus handling
            // (flaky in the WebView) and could destroy a drag-selection mid-drag.
            // A completed drag leaves a non-empty selection, which we respect —
            // click-without-drag focuses, drag selects, like real terminals.
            function focusPrompt() {
                cmdEl.focus();
                try {   // caret at the end of anything already typed
                    var r = document.createRange();
                    r.selectNodeContents(cmdEl);
                    r.collapse(false);
                    var s = window.getSelection();
                    s.removeAllRanges();
                    s.addRange(r);
                } catch (err) {}
            }
            el._focusPrompt = focusPrompt;   // cli_stage's panel-wide click delegate uses this
            // Click vs select is decided by POINTER TRAVEL, not by "is there a
            // selection": a stale selection elsewhere on the page survives clicks
            // on user-select:none spots (icons, padding) and used to silently
            // swallow the focus — the terminal felt randomly dead. Now: a click
            // that didn't move focuses, an actual drag (or a double-click word
            // select, e.detail > 1) is left alone as a selection gesture.
            var downX = 0, downY = 0;
            el.addEventListener('mousedown', function (e) { downX = e.clientX; downY = e.clientY; });
            el.addEventListener('click', function (e) {
                if (el.classList.contains('cc-running')) return;
                // Only in Manual: a click never switches to it, the chat box's
                // Auto / Manual pill does (owner, 2026-10-17; agent_mode.js).
                var pill = document.getElementById('agentModeWrap');
                if (!pill || pill.dataset.shellInput !== 'manual') return;
                if (e.target === cmdEl || (e.target.closest && e.target.closest('.cc-todo'))) return;
                // the chat above it is there to read (select, follow a link): only the live
                // line hands you the prompt
                if (!(e.target.closest && e.target.closest('.cc-out'))) return;
                if (e.detail > 1) return;                                        // dbl/triple click = select
                if (Math.abs(e.clientX - downX) > 4 || Math.abs(e.clientY - downY) > 4) return;  // drag = select
                if (document.activeElement === cmdEl) return;
                focusPrompt();
            });

            cmdEl.addEventListener('keydown', function (e) {
                // Ctrl+C — interrupt whatever is running, exactly like a terminal. Works
                // whether or not something IS running, so it's never a dead key.
                if (e.key === 'c' && (e.ctrlKey || e.metaKey) && !window.getSelection().toString()) {
                    e.preventDefault();
                    termStream.push('^C');
                    fetch('/api/shell-kill', { method: 'POST' }).catch(function () {});
                    return;
                }
                if (e.key !== 'Enter') return;
                e.preventDefault();                  // never insert a newline
                if (termBusy) return;                // a command owns the prompt right now
                var cmd = (cmdEl.textContent || '').trim();
                if (!cmd) return;
                // Hand-running a command IS using Shell use — engage the same
                // per-use mode lock a sent agent task engages. Fires only on an
                // actual Enter with a real command: toggling the picker or just
                // clicking into the terminal never locks anything.
                document.dispatchEvent(new CustomEvent('agentmode:lock', { detail: { mode: 'shell' } }));
                cmdEl.textContent = '';
                // Echo the command stamped with WHERE it ran, so the terminal box reads like
                // a terminal — the executed line scrolls up, the live prompt stays last.
                // tagged so it stands out from output (cc-prompt-line)
                termStream.push((curPath ? curPath + ' > ' : '$ ') + cmd, 'cc-prompt-line');
                setBusy(true);
                fetch('/api/shell-exec', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    // session_id tags the command's <manual_mode> record to THIS
                    // chat, so another chat's next agent run never sees it
                    body: JSON.stringify({ command: cmd, session_id: window.currentSessionId || null })
                })
                .then(function (r) { return r.json(); })
                .then(function (d) {
                    if (document.activeElement === cmdEl) showPath(d.short);   // cd moved us
                    // `cd` and failures answer inline and are already finished; a live
                    // command answers 'started' and ends via window.shellTermEnd.
                    if (d.status === 'started') return;
                    var text = String(d.output || '').replace(/\s+$/, '');
                    if (text) text.split('\n').forEach(function (l) { termStream.push(l); });
                    else if (d.error) termStream.push(d.error);
                    termStream.settle();   // inline reply (cd / failure) — nothing more coming
                    setBusy(false);
                })
                .catch(function () {
                    termStream.push('failed to reach the shell');
                    termStream.settle();
                    setBusy(false);
                });
            });
        }

        // Stream one reply's prose into a streamer, a field per line (Shell use only).
        function streamProse(stream, obj) {
            proseLines(obj).forEach(function (t) { stream.push(t); });
        }

        function onCoderPacket(obj) {
            markReceived();                       // this JSON IS the packet -> tick "synthesizing"
            stopFunLines();                       // real output is here — the fun lines yield the spot
            if (thinkStream) streamProse(thinkStream, obj);   // Shell use: words beside the head
            pushActions(actionChain, obj, true);  // actions -> the `>` terminal (+ tool queue/count)
            // Anticipate the next step once this packet's tools finish playing — but NOT after
            // a dispatched minion: the coder is blocked until that minion returns, so the chain
            // would sit on the opening beats while nothing is being asked. It stops at
            // "dispatched minion" and endMinion() restarts the opening phase.
            anticipateNext = !hasExit(obj) && !hasMinion(obj);
            playTools();   // covers a packet with no chain tools — hand straight to the next opening
        }
        // made again for each run (begin): a run stopped mid-packet must not leave half
        // a JSON object in front of the next run's first one
        var coderActionParser = makeActionParser(onCoderPacket);

        function setLine(text) {
            // Don't push the raw line to the terminal anymore — it's JSON. The parser reads the
            // `action` array and pushActions() streams a clean narration line per action instead.
            coderActionParser(text);     // read `action` -> narration + opening phases + tools + scratchpad
        }

        // Step 1 begins: "communicating with llm service…". Skipped for the Shell-use IDLE
        // card, which may sit for minutes before a task is sent — it would spend that whole
        // time accumulating chain steps for an LLM call that hasn't happened. cli_stage.js
        // calls begin() when the idle card is promoted to a live run.
        if (!opts.idle) startOpening();

        // Anchor the single trunk so it starts just below the `>` prompt (a tiny CONSTANT gap,
        // NOT the centre of the now-tall 5-line terminal) and runs down to the LAST minion's
        // head centre. TRUNK_GAP keeps the "almost touching the >" look constant.
        // Gap between the `>` and where the trunk starts. The Shell-use card's prompt is
        // bigger and sits alone atop a wide terminal, so the trunk is cut further back to
        // leave clean air under the glyph; the compact card keeps its almost-touching look.
        // The Shell card's trunk hangs from its head instead, starting right under it.
        var TRUNK_GAP = header ? 1 : 3;
        function layoutTrunk() {
            var rows = treeEl.querySelectorAll('.cc-mrow');
            if (!rows.length) {
                trunk.style.height = '0';
                treeEl.style.transform = 'translateY(0)';
                flowEl.classList.remove('cc-mscrolled');
                return;
            }
            // CONVEYOR. In Shell use the card is height-capped (coder_card.css), so past a
            // few minions the tree no longer fits its window — slide the whole tree up by
            // the overflow so the NEWEST rows stay in view and the older ones disappear
            // under the top fade, exactly like the action chain beside the card. The
            // content-height cards (Computer/Mobile use) grow with their tree instead, so
            // there `over` is always 0 and nothing moves.
            var over = Math.max(0, treeEl.offsetHeight - flowEl.clientHeight);
            treeEl.style.transform = over ? ('translateY(' + (-over) + 'px)') : 'translateY(0)';
            flowEl.classList.toggle('cc-mscrolled', over > 0);

            var mRect = minionsEl.getBoundingClientRect();
            var aRect = (header && mascotEl ? mascotEl : pEl).getBoundingClientRect();   // the head / the `>` glyph
            var topY = (header ? aRect.bottom : aRect.top + aRect.height / 2) - mRect.top + TRUNK_GAP;
            // Anchor the bottom to the LAST minion's head centre (the row also holds a
            // sub-chain below it). offsetTop, NOT getBoundingClientRect: offsets are pure
            // layout, so they ignore the conveyor transform set above — which at this exact
            // moment is mid-transition, and whose current value depends on whether the
            // transition is even running. Reading layout and taking `over` off by hand puts
            // the trunk on the row's final resting place in one go, whatever the animation
            // is doing. The chain is .cc-minions > .cc-mflow > (.cc-mtree) > .cc-mrow >
            // .cc-mrow-head — all three offsetParents are position:relative, and the
            // untransformed .cc-mtree is transparent to it.
            // the Shell card's trunk ends at the last row that stays (a leaving one is
            // already fading away); the compact card keeps its old reach
            var stay = header ? treeEl.querySelectorAll('.cc-mrow:not(.leaving)') : rows;
            if (!stay.length) { trunk.style.height = '0'; return; }
            var lastRow = stay[stay.length - 1];
            var head = lastRow.querySelector('.cc-mrow-head');
            var botY = flowEl.offsetTop + lastRow.offsetTop - over
                     + (head ? head.offsetTop + head.offsetHeight / 2 : lastRow.offsetHeight / 2);
            trunk.style.top = topY + 'px';
            trunk.style.height = Math.max(0, botY - topY) + 'px';
        }
        // Re-run the conveyor whenever EITHER side of "does it still fit" moves:
        //   the window — the app window is resizable, and a resize would otherwise leave the
        //     tree translated for a height it no longer has;
        //   the tree — a row keeps GROWING after it's added (its tool sub-chain fills in one
        //     icon at a time, and `leaving` rows collapse over 0.32s), so the rAF layoutTrunk
        //     at insert time measures a tree that isn't its final size yet. Without this the
        //     conveyor under-scrolls and the last row still hangs out of the card.
        // layoutTrunk only ever writes the tree's TRANSFORM, which no ResizeObserver reports,
        // so this can't feed back on itself.
        var treeRO = window.ResizeObserver ? new ResizeObserver(function () { layoutTrunk(); }) : null;
        if (treeRO) { treeRO.observe(flowEl); treeRO.observe(treeEl); }

        function findRow(id) {
            var rows = treeEl.querySelectorAll('.cc-mrow');
            for (var i = 0; i < rows.length; i++) if (rows[i].dataset.id === String(id)) return rows[i];
            return null;
        }

        // Shell use: each minion's bot (the owner's minion-helmet.html art, ui/bots/minion*.webp)
        // stands on the live box's bottom left while it works, one beside another
        // (coder_card.css), four at most; more wait for a place. Its tools play on its visor,
        // one icon at a time in its eyes' navy (tool_icons.js createFaceIcon), made with its
        // first tool: the eyes give way to them. It sinks away when its minion ends.
        var MBOT_PLACES = 4, MBOT_OUT_MS = 320;
        var mbotsEl = el.querySelector('.cc-mbots');
        var mbots = {};          // minion id -> { el, fx, face }
        var mbotQueue = [];      // ids waiting for a place
        function mbotIn(id) {
            if (!mbotsEl || mbots[id] || mbotQueue.indexOf(id) >= 0) return;
            if (Object.keys(mbots).length >= MBOT_PLACES) { mbotQueue.push(id); return; }
            var m = document.createElement('div');
            m.className = 'cc-mbot';
            // its face between the eyes and the shine: the visor's glare stays on top
            m.innerHTML =
                '<img src="bots/minion.webp" alt="">' +
                '<img class="cc-mbot-eyes" src="bots/minion-eyes.webp" alt="">' +
                '<canvas class="cc-mbot-face"></canvas>' +
                '<img src="bots/minion-shine.webp" alt="">';
            mbotsEl.appendChild(m);
            mbotsEl.classList.add('on');
            mbots[id] = { el: m, fx: null, face: null };
        }
        function mbotsFold() {                       // no bot left and none waiting: the row folds
            if (mbotsEl && !Object.keys(mbots).length && !mbotQueue.length) mbotsEl.classList.remove('on');
        }
        function mbotOut(id) {
            var w = mbotQueue.indexOf(id);
            if (w >= 0) { mbotQueue.splice(w, 1); mbotsFold(); return; }
            var b = mbots[id];
            if (!b) return;                           // a repeated or stale end
            delete mbots[id];
            if (b.fx) b.fx.dispose();
            b.el.classList.add('leaving');
            mbotsFold();
            setTimeout(function () {
                if (b.el.parentNode) b.el.parentNode.removeChild(b.el);
                if (mbotQueue.length) mbotIn(mbotQueue.shift());   // the next one takes a place
            }, reduced() ? 0 : MBOT_OUT_MS);
        }
        function mbotsClear() {                      // the run is over: all go at once
            mbotQueue.length = 0;
            for (var k in mbots) { if (mbots[k].fx) mbots[k].fx.dispose(); }
            mbots = {};
            if (mbotsEl) { mbotsEl.replaceChildren(); mbotsEl.classList.remove('on'); }
        }
        // its face (null while it waits for a place): a tool the face can show takes the visor
        // and the eyes give way to it; a packet without one leaves the eyes
        function minionFace(id) {
            var b = mbots[id];
            if (!b || !ICONS || !ICONS.createFaceIcon) return null;
            if (!b.face) {
                b.fx = ICONS.createFaceIcon(b.el.querySelector('.cc-mbot-face'), { color: 'rgb(34, 44, 80)' });
                b.face = {
                    addAction: function (tool) {
                        if (!b.fx.addAction(tool)) return false;
                        b.el.classList.add('working');
                        return true;
                    }
                };
            }
            return b.face;
        }

        function addMinion(id, query) {
            startFunLines();   // a minion is off working — the head-side line cycles fun "thinking" lines
            if (header) mbotIn(id);
            var row = document.createElement('div');
            row.className = 'cc-mrow';
            row.dataset.id = String(id);
            row.innerHTML =
                '<div class="cc-mrow-head">' + orbLoaderHTML() + '<span class="mline"></span></div>' +
                '<div class="cc-msub"><div class="cc-mchain"></div></div>';
            treeEl.appendChild(row);
            // Each minion streams its live output (initial = query) AND grows its own
            // horizontal action sub-chain, parsed from the same per-minion JSON. Shell use:
            // no sub-chain under the row; its tools play on its Minion bot's face instead.
            minionStreams[id] = makeLineStreamer(row.querySelector('.mline'), query || 'minion');
            if (ICONS && !header) minionChains[id] = ICONS.createChain(row.querySelector('.cc-mchain'), { orientation: 'horizontal' });
            minionParsers[id] = makeActionParser(function (obj) {
                // Shell use: same rule as the coder — the minion's row streams its OWN
                // reply (action excluded) instead of the raw JSON it printed.
                if (header && minionStreams[id]) streamProse(minionStreams[id], obj);
                pushActions(header ? minionFace(id) : minionChains[id], obj, false);
                var mc = row.querySelector('.cc-mchain');
                if (!header && mc && mc.children.length) row.classList.add('has-tools');   // reveal the loader→sub-chain L
                // a growing sub-chain makes this row taller, pushing later rows down — re-anchor the trunk.
                if (window.requestAnimationFrame) requestAnimationFrame(layoutTrunk); else layoutTrunk();
            });
            if (window.requestAnimationFrame) requestAnimationFrame(layoutTrunk); else layoutTrunk();
        }

        function setMinionLine(id, line) {
            // Default: the raw line IS the row's text. In Shell use the parser below
            // streams the parsed prose instead, so the raw JSON never lands in the row.
            if (!header && minionStreams[id]) minionStreams[id].push(line);
            if (minionParsers[id]) minionParsers[id](line);   // read `action` -> this minion's chain
        }

        // Todo updates — dropped everywhere else (the agent owns its list). In Shell use the
        // moment the agent writes a list the terminal gives up its right quarter to show it,
        // and every later update re-renders (todo.md is rewritten, not appended).
        // payload is the JSON string service.py sends: {objective, tasks:[{text, done}]}.
        //
        // Built with the main agent's todo classes (.todo-list / .todo-item / .todo-check /
        // .todo-spinner / .todo-text, styled at the end of coder_card.css): the animated tick
        // colour and the spinner on the current task. Row states mirror renderTodo() of the
        // main agent's todo list, now parked in container/dead/bottom_right/.
        var todoEl = el.querySelector('.cc-todo');
        function setTodo(payload) {
            if (!header || !todoEl) return;
            var data;
            try { data = (typeof payload === 'string') ? JSON.parse(payload) : payload; }
            catch (e) { return; }
            var tasks = (data && data.tasks) || [];
            if (!tasks.length) return;                // nothing written yet — stay collapsed

            var firstUndone = -1;
            for (var k = 0; k < tasks.length; k++) { if (!tasks[k].done) { firstUndone = k; break; } }

            var list = document.createElement('div');
            list.className = 'todo-list';
            tasks.forEach(function (t, i) {
                var item = document.createElement('div');
                item.className = 'todo-item';
                var mark = document.createElement('span');
                var label = document.createElement('span');
                label.className = 'todo-text';
                if (t.done) { mark.className = 'todo-check done'; label.classList.add('is-done'); }
                else if (i === firstUndone) { mark.className = 'todo-spinner'; }   // in progress
                else { mark.className = 'todo-check'; }                            // pending
                label.textContent = t.text || ('task ' + (i + 1));   // textContent: never parse as HTML
                item.appendChild(mark); item.appendChild(label);
                list.appendChild(item);
            });
            todoEl.replaceChildren(list);
            el.classList.add('has-todo');             // opens the right column (CSS)
        }

        // The minion is done — DON'T wait for its streaming or show a mark. Immediately fade the
        // whole row (line + L + tool sub-chain) and collapse its height to 0 so the rows below
        // slide up; then remove it and re-anchor the trunk.
        function endMinion(id, status) {
            var row = findRow(id);
            if (!row || row.classList.contains('leaving')) return;   // idempotent (cliMinionEnd can repeat)
            if (header) mbotOut(id);                                 // its bot sinks away now

            // stop streaming into a row that's leaving
            if (minionStreams[id]) { minionStreams[id].dispose(); delete minionStreams[id]; }

            // pin current height so max-height animates content-height -> 0 (no easing dead-zone)
            row.style.maxHeight = row.offsetHeight + 'px';   // read forces reflow
            // (the trunk re-lays in the same frame: it ends at the last row that stays)
            if (window.requestAnimationFrame) {
                requestAnimationFrame(function () { row.classList.add('leaving'); row.style.maxHeight = '0px'; layoutTrunk(); });
            } else { row.classList.add('leaving'); row.style.maxHeight = '0px'; layoutTrunk(); }

            // The chain paused at "dispatched minion" while the coder waited. Once the
            // LAST live minion is back the coder resumes talking to the LLM, so pick the
            // opening phase up again. `row` is excluded explicitly rather than by the
            // .leaving class — that class is only added on the next animation frame, so
            // it isn't set yet here; .leaving rows are mid-collapse and don't count either.
            var stillRunning = Array.prototype.filter.call(
                treeEl.querySelectorAll('.cc-mrow'),
                function (r) { return r !== row && !r.classList.contains('leaving'); }
            ).length;
            if (!stillRunning) startOpening();

            var done = false;
            function finish() {
                if (done) return; done = true;
                row.removeEventListener('transitionend', onEnd);
                if (minionChains[id]) { minionChains[id].dispose(); delete minionChains[id]; }
                delete minionParsers[id];
                if (row.parentNode) row.parentNode.removeChild(row);
                if (window.requestAnimationFrame) requestAnimationFrame(layoutTrunk); else layoutTrunk();
            }
            function onEnd(e) { if (e.target === row && e.propertyName === 'max-height') finish(); }
            row.addEventListener('transitionend', onEnd);
            setTimeout(finish, 420);   // safety if transitionend never fires (off-screen / unmounted)
        }

        // Web has its own globe icon in the tool chain now — no "searching the web…" hint in
        // the terminal. Kept as a no-op so the existing pill_web_loading events stay harmless.
        function setWeb(on) {}

        function setDone(status, summary) {
            clearTimeout(openTimer);                                  // stop any pending "thinking"
            if (thinkingStep) { thinkingStep.complete('packet received'); thinkingStep = null; }
            if (summary) outStream.push(summary);   // prose summary -> straight to the terminal (not JSON, so bypass the parser)
            el.classList.add((status === 'error' || status === 'stopped') ? 'error' : 'done');
        }

        function dispose() {
            stopFunLines();
            if (treeRO) treeRO.disconnect();
            clearTimeout(openTimer); clearTimeout(toolTimer); toolTimer = null;
            toolQ.length = 0; openNext = null; thinkingStep = null;
            if (mascot) mascot.dispose();
            if (thinkStream) thinkStream.dispose();
            if (termStream) termStream.dispose();
            if (termRo) termRo.disconnect();
            if (fadeRo) fadeRo.disconnect();
            if (thinkRo) thinkRo.disconnect();
            if (colRo) colRo.disconnect();
            outStream.dispose();
            for (var k in minionStreams) if (minionStreams[k]) minionStreams[k].dispose();
            if (actionChain) actionChain.dispose();
            if (tracker) tracker.dispose();
            for (var m in minionChains) if (minionChains[m]) minionChains[m].dispose();
            mbotsClear();
            rbotOff();
        }

        return {
            el: el,
            chainEl: chainEl,
            trackEl: trackEl,
            setLine: setLine,
            // Kick off the opening phase for a card created with {idle:true}. Called when
            // cli_stage.js promotes the waiting terminal into a live run: the terminal
            // becomes the AGENT's, so hand-typing drops and the prompt goes read-only —
            // the cursor moves to the agent's output line, which is where text is actually
            // appearing. What the terminal already shows stays above the run.
            begin: function () {
                document.body.classList.remove('cli-typing');
                el.classList.add('cc-running');
                coderActionParser = makeActionParser(onCoderPacket);
                if (cmdEl) {
                    cmdEl.setAttribute('contenteditable', 'false');
                    cmdEl.textContent = '';                 // drop anything half-typed
                    if (document.activeElement === cmdEl) cmdEl.blur();
                }
                setName(header);             // back to "AutoCua Code" — the agent's again
                if (mascot && mascot.setGlow) mascot.setGlow(true);   // head lights up: working
                if (thinkStream) thinkStream.clear();                  // the hello is not the agent's words
                clearPrompt();               // bare `>`, and no stale path on echoed lines
                startOpening();
            },
            // The run is over (its answer is printed): everything still playing for it
            // stops and the prompt is yours again; the terminal keeps what it showed and
            // waits for the next run (cli_stage.js, Shell use). Its tool chain, tracking
            // progress and minions' bots go: the next run's start empty and fade in again.
            finish: function () {
                stopFunLines();
                clearTimeout(openTimer); openTimer = null;
                clearTimeout(toolTimer); toolTimer = null;
                toolQ.length = 0; openNext = null; thinkingStep = null;
                anticipateNext = receivedEarly = fastOpening = openingDone = false;
                if (actionChain) actionChain.dispose();
                if (tracker) tracker.dispose();
                tracker = makeTracker();
                chainEl.classList.remove('cc-tools-active'); toolsRevealed = false;
                trackEl.classList.remove('cc-track-active');
                [chainEl.querySelector('.cc-chain'), trackEl.querySelector('.cc-track-tree')].forEach(function (t) {
                    if (t) t.style.transform = '';      // back to the top, while hidden (no slide later)
                });
                for (var k in minionStreams) { if (minionStreams[k]) minionStreams[k].dispose(); delete minionStreams[k]; }
                for (var m in minionChains) { if (minionChains[m]) minionChains[m].dispose(); delete minionChains[m]; }
                mbotsClear();
                for (var q in minionParsers) delete minionParsers[q];
                treeEl.replaceChildren();
                layoutTrunk();
                if (thinkStream) thinkStream.clear();         // now, not after its queue
                if (todoEl) todoEl.replaceChildren();          // the run's checklist goes with it
                el.classList.remove('has-todo');
                outStream.settle();
                el.classList.remove('cc-running');
                if (cmdEl) cmdEl.setAttribute('contenteditable', 'plaintext-only');
                setName(header);
                if (mascot && mascot.setGlow) mascot.setGlow(false);
            },
            // raw text straight onto the terminal, bypassing the JSON parser — used for
            // stage-level messages (e.g. a run that failed before it produced any output)
            // and for the live output of a hand-typed command.
            note: function (t) { if (termStream) termStream.push(t); else { outStream.push(t); markOut(); } },
            // A line of the chat's own: your message, the run's answer.
            // mode: '' (types in), 'instant', or 'html' (markup from markdown.py, whole).
            print: function (text, cls, mode) { outStream.push(text, cls, mode); outStream.settle(); markOut(); },
            // the chat glides to its end (your message just went in)
            toEnd: function () { outStream.toEnd(); },
            // Shell use: a new chat's hello, typed on the live line (it shows there until the
            // chat has a line of its own; coder_card.css).
            hello: function (t) {
                if (!thinkStream) return;
                thinkStream.clear();
                thinkStream.push(String(t || ''));
                thinkStream.settle();
            },
            // An empty terminal (a reopened chat's, before its rows).
            clear: function () {
                outStream.clear();
                if (termStream && !termBusy) termStream.clear();   // a command still running keeps its box
                if (todoEl) todoEl.replaceChildren();
                el.classList.remove('cc-has-out', 'has-todo');
            },
            // Back to Auto: the prompt lets go, the agent's name comes back.
            leaveTyping: function () {
                if (cmdEl && document.activeElement === cmdEl) cmdEl.blur();
                if (cmdEl && !termBusy) cmdEl.textContent = '';   // a half-typed command goes, as at a run's start
                document.body.classList.remove('cli-typing');
                setName(header);
                clearPrompt();
            },
            // a hand-typed command exited — hand the prompt back
            termEnd: function (code) {
                var ts = termStream || outStream;
                if (code) ts.push('[exit ' + code + ']');
                ts.settle();             // the command is done — stop the trailing shimmer
                setBusy(false);
            },
            setTodo: setTodo,
            // Shell use: the run's bot beside the live box (above)
            botIn: botIn,
            botOn: botOn,
            botToAnswer: botToAnswer,
            botEnd: botEnd,
            addMinion: addMinion,
            setMinionLine: setMinionLine,
            endMinion: endMinion,
            setWeb: setWeb,
            setDone: setDone,
            // The run's scratchpad notes, in order — cli_stage.js snapshots these at task
            // end (before dispose) to build the Agent Notes panel under the terminal.
            getNotes: function () { return noteLog.slice(); },
            dispose: dispose,
        };
    }

    window.CliCoderCard = {
        create: create,
        // the typed command ended, whichever terminal (if any) is on screen (cli_stage.js)
        termIdle: function () { cmdRunning = false; }
    };
})();
