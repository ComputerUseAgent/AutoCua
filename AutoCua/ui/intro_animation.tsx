// The intro splash: #splashOverlay with the AutoCua team's entrance. This
// was ui/intro_animation.html (its own page, in an iframe here) until
// 2026-10-22: the same markup, as a React part the page (index.tsx) draws
// inside #splashOverlay, and the same script, below the component, with the
// same timeline. Its CSS is intro_animation.css beside this file (the
// timeline is written out at the top of it). The intro used to post
// 'splashDone' to its parent window; it posts to this window now, and
// script.js listens there as before, fades the overlay over 0.9 s and takes
// it out of the page itself (the toast waits for that, showToast).
//
// THE MOTION RUNS ON THE COMPOSITOR: every move is `transform` or `opacity`,
// each element has at most one animation per property, and every animation
// is created in one task up front with its own delay, so the booting app's
// busy main thread cannot shift them (see the CSS for the full note).
import { useLayoutEffect, useRef } from 'react';

// The side bots, left to right (the lead, Computer use, is its own slot in
// front of them): the name the script finds them by, and their image files
// in ui/bots/ (body, eyes, shine).
const SIDE: [string, string][] = [
    ['AutoCua', 'autocua'], ['Web use', 'webuse'], ['Mobile use', 'mobileuse'],
    ['Coder use', 'coderuse'], ['Scraper use', 'scraperuse'], ['Minion', 'minion'],
];

// "Automatic Computer use agent": per word, the letters that stay for
// "AutoCua" and the ones that fade.
const WORDS: [string, string][] = [['Auto', 'matic'], ['C', 'omputer'], ['u', 'se'], ['a', 'gent']];

function Bot({ name, file }: { name: string; file: string }) {
    return (
        <div className="bot" role="img" aria-label={name}><img src={'bots/' + file + '.webp'} alt="" /><img className="eyes" src={'bots/' + file + '-eyes.webp'} alt="" /><img src={'bots/' + file + '-shine.webp'} alt="" /></div>
    );
}

// One <i> per letter, so each can fade and move on its own.
function Letters({ text, cls }: { text: string; cls?: string }) {
    return <>{text.split('').map((ch, i) => <i className={cls} key={i}>{ch}</i>)}</>;
}

export function IntroAnimation() {
    const overlay = useRef<HTMLDivElement>(null);

    // Before paint, as the old page's script ran right after its markup.
    useLayoutEffect(() => {
        if (overlay.current) startIntro(overlay.current);
    }, []);

    return (
        <div id="splashOverlay" className="splash-overlay" ref={overlay}>
            <div className="lift">
                <div className="row">
                    <div className="side">
                        {SIDE.map(([name, file]) => <div className="slot" key={file}><Bot name={name} file={file} /></div>)}
                    </div>
                    <div className="slot lead"><Bot name="Computer use" file="computeruse" /></div>
                </div>
            </div>

            <div className="title" role="img" aria-label="AutoCua: Automatic Computer use agent">
                {WORDS.map(([keep, fade], i) => [
                    i > 0 ? <i className="gap" key={'gap' + i}>{'\u00A0'}</i> : null,
                    <span className="word" key={'word' + i}><Letters text={keep} cls="keep" /><Letters text={fade} cls="fade" /></span>,
                ])}
            </div>

            <div className="sheen" aria-hidden="true"><div className="sheen-art"><Letters text="AutoCua" /></div></div>
        </div>
    );
}

// ---------------------------------------------------------------------
// The motion: the script of the old page, as it was.
// ---------------------------------------------------------------------

interface Geometry {
    titleTop: number;
    feet: number;
    h: number;
    w: number;
    big: number;
    drop: number;
}

export function startIntro(root: HTMLElement): void {
    const reduced = !!(window.matchMedia &&
        window.matchMedia('(prefers-reduced-motion: reduce)').matches);

    // script.js fades .splash-overlay over 0.9s from this message. Posted once.
    // Its listener comes with DOMContentLoaded (the old iframe only started
    // after that); the intro starts while the page still parses, so a
    // hand-off due before then waits for it, or the message would be lost.
    let handed = false;
    function handOff(): void {
        if (handed) return;
        handed = true;
        const post = function (): void { window.postMessage('splashDone', '*'); };
        if (document.readyState === 'loading') {
            document.addEventListener('DOMContentLoaded', function () { setTimeout(post, 0); });
        } else {
            post();
        }
    }

    // ---- timing, ms from the start: Atlas's hero entrance -------------
    const FIRST = 100;
    const FADE = 350;                       // the team fades in, linear
    const GROW = 1000;                      // 0.85x -> 1x and blur 6px -> 0
    const WAKE_AT = 320, WAKE = 600;        // the lead's eyes open
    const BLINK1_AT = 1120;                 // and blink, just after waking
    const SHRINK_AT = 1400, SHRINK = 1800;  // big -> 1x, rising into place
    // Atlas's lineup fan-out, as the shrink speeds up. The side bots fade
    // in as one group, just after they start to move.
    const FAN_AT = SHRINK_AT + 700, FAN = 1000;
    const SIDE_AT = FAN_AT + 100, SIDE_FADE = 500;
    const T0 = FIRST + SHRINK_AT;           // the title, with the shrink (Atlas)
    const SLIDE_AT = T0 + 1900, SLIDE = 900;
    // "AutoCua" stays; one shimmer crosses it, the bots blink along with
    // it, left to right, then the hand-off.
    const SHEEN_AT = SLIDE_AT + SLIDE + 200, SHEEN = 1000;
    const BLINK_AT = SHEEN_AT + 100, BLINK_GAP = 100, BLINK = 300;   // seven: the same 0.6 s wave
    const END = SHEEN_AT + SHEEN + 250;
    const DECODE_MAX = 1500;                // the longest wait for the images
    const EASE_SHEEN = 'cubic-bezier(0.4, 0, 0.6, 1)';     // Atlas sheen
    const EASE_SETTLE = 'cubic-bezier(0.16, 1, 0.3, 1)';   // Atlas "settle"
    const EASE_EXPO = 'cubic-bezier(0.87, 0, 0.13, 1)';    // Atlas "inOutExpo"
    const EASE_OUT = 'cubic-bezier(0.33, 1, 0.68, 1)';     // Atlas "outCubic"
    const EASE_FAN = 'cubic-bezier(0.2, 0.8, 0.2, 1)';     // Atlas lineup

    // Whatever happens below, the app is never left under the splash
    // (go() restarts this when the intro starts).
    let watchdog = setTimeout(handOff, DECODE_MAX + END + 1000);

    // The lineup, left to right: offset (bot widths), scale, extra delay.
    // Atlas's "Build your AI team" (+-149.4 / +-265.6 of a 142px robot,
    // so 1.05 / 1.87 bot widths), drawn a little closer together, and one
    // more each end, smaller again (owner, 2026-10-17: AutoCua's bot on the
    // left, the minion on the right).
    const TEAM: [number, number, number][] = [[-2.2, 0.45, 100], [-1.64, 0.6, 50], [-0.92, 0.8, 0], [0, 1, 0],
                                              [0.92, 0.8, 0], [1.64, 0.6, 50], [2.2, 0.45, 100]];
    const LEAD = 3;

    // ---- geometry ------------------------------------------------------
    const ASPECT = 520 / 720;     // the cropped bot images, width / height
    const lift = root.querySelector('.lift') as HTMLElement;
    const row = root.querySelector('.row') as HTMLElement;
    const side = root.querySelector('.side') as HTMLElement;
    // The slots in TEAM's order, left to right (in the markup the lead
    // comes last, in front of the side group).
    const slots = ['AutoCua', 'Web use', 'Mobile use', 'Computer use', 'Coder use', 'Scraper use', 'Minion'].map(function (name) {
        return (root.querySelector('.bot[aria-label="' + name + '"]') as HTMLElement).parentNode as HTMLElement;
    });
    const title = root.querySelector('.title') as HTMLElement;
    const sheen = root.querySelector('.sheen') as HTMLElement;
    const sheenArt = root.querySelector('.sheen-art') as HTMLElement;
    let zoom: Animation | null = null, rise: Animation | null = null, sweep: Animation | null = null,
        still: Animation | null = null, slides: Animation[] | null = null;
    const fans: Animation[] = [];

    // The intro's window listeners stop themselves once script.js has taken
    // the overlay out of the page (an iframe's died with it before).
    function whileShown(fn: () => void): () => void {
        const listener = function (): void {
            if (!root.isConnected) { window.removeEventListener('resize', listener); return; }
            fn();
        };
        return listener;
    }

    function geometry(): Geometry {
        const W = window.innerWidth, H = window.innerHeight, vmin = Math.min(W, H);
        // "AutoCua" in the middle of the window...
        const line = parseFloat(getComputedStyle(title).fontSize) * 1.2;
        const titleTop = H / 2 - line / 2;
        const clear = H / 2 + line / 2 + Math.min(0.03 * vmin, 24);   // below the title
        // ...and the team standing on the bottom edge (their lower third
        // fades out, so they rise out of it), the lead's head below the title.
        const feet = H;
        const h = Math.min(0.36 * vmin, 0.23 * W, 280, feet - clear);  // the lead, at the end
        const w = h * ASPECT;
        // Atlas: 1.75x while big, pushed down a little, rising into place as
        // it shrinks. Pushed down further where it takes that for the big
        // lead's head to stay below the title. The width needs no check:
        // while big only the lead shows (at most 0.37 W wide), and the lineup
        // of seven fits (+-2.425 bot widths: g.h <= 0.23 W keeps it within
        // 0.81 W).
        const big = 1.75;
        const drop = Math.max(28, Math.round(H * 0.06), Math.ceil(clear + h * big - feet));
        return { titleTop: titleTop, feet: feet, h: h, w: w, big: big, drop: drop };
    }

    // A side bot waiting behind the lead (Atlas: a little low, at 96%,
    // see-through), and in its place in the lineup. In row units: the row
    // is laid out big.
    function behind(g: Geometry, i: number): string {
        return 'translate(0px, ' + (0.08 * g.h * g.big) + 'px) scale(' + (0.96 * TEAM[i][1]) + ')';
    }
    function placed(g: Geometry, i: number): string {
        return 'translate(' + (TEAM[i][0] * g.w * g.big) + 'px, 0px) scale(' + TEAM[i][1] + ')';
    }

    // The row is laid out at its big size with its feet on the final line,
    // so shrinking around its feet lands it exactly in place.
    function layout(): Geometry {
        const g = geometry(), B = g.big;
        row.style.height = (g.h * B) + 'px';
        row.style.top = (g.feet - g.h * B) + 'px';
        slots.forEach(function (s, i) {
            s.style.width = (g.w * B) + 'px';
            s.style.height = (g.h * B) + 'px';
            s.style.marginLeft = (-g.w * B / 2) + 'px';
            s.style.zIndex = String(5 - Math.abs(i - LEAD));   // the lead in front
            if (fans[i]) (fans[i].effect as KeyframeEffect).setKeyframes([{ transform: behind(g, i) }, { transform: placed(g, i) }]);
        });
        title.style.top = g.titleTop + 'px';
        sheen.style.top = g.titleTop + 'px';
        if (slides) remeasureSlides();
        if (sweep && still) {
            const k = sheenTravel();
            (sweep.effect as KeyframeEffect).setKeyframes([{ transform: 'translateX(' + (-k) + 'px)' }, { transform: 'translateX(' + k + 'px)' }]);
            (still.effect as KeyframeEffect).setKeyframes([{ transform: 'translateX(' + k + 'px)' }, { transform: 'translateX(' + (-k) + 'px)' }]);
        }
        if (zoom) (zoom.effect as KeyframeEffect).setKeyframes(rowFrames(B));
        if (rise) (rise.effect as KeyframeEffect).setKeyframes([{ transform: 'translateY(' + g.drop + 'px)' }, { transform: 'translateY(0px)' }]);
        return g;
    }

    // The end state, still: the lineup under "AutoCua".
    function settle(): void {
        slides = null;
        const g = layout();
        lift.style.opacity = '1';
        side.style.opacity = '1';
        row.style.transform = 'scale(' + (1 / g.big) + ')';
        slots.forEach(function (s, i) {
            s.style.transform = placed(g, i);
            (s.querySelector('.eyes') as HTMLElement).style.opacity = '1';
        });
        title.classList.add('settled');
        sheen.style.display = 'none';   // no shimmer when still
    }

    // --------------------------------------------------------------
    // The collapse. Each kept letter slides from where the full phrase
    // put it to where it sits in "AutoCua", centred on the same point,
    // which is exactly where plain layout puts it once .settled drops the
    // rest. The distances are measured at the current size, and again
    // after a resize: the system font's letters do not scale exactly with
    // the font size (it has optical sizes), so they would land off.
    // --------------------------------------------------------------
    const SLIDE_EASE = 'cubic-bezier(0.65, 0, 0.35, 1)';

    // How far each kept letter slides, in px, measured on `t`: the title
    // before it moves (its words only rise, so the letters' x is right).
    function slideDistances(t: HTMLElement): number[] {
        const letters = t.querySelectorAll('i');
        const centre = (letters[0].getBoundingClientRect().left +
                        letters[letters.length - 1].getBoundingClientRect().right) / 2;
        const rects = Array.prototype.map.call(t.querySelectorAll('.keep'), function (el: Element) { return el.getBoundingClientRect(); }) as DOMRect[];
        let x = centre - rects.reduce(function (w, r) { return w + r.width; }, 0) / 2;
        return rects.map(function (r) { const dx = x - r.left; x += r.width; return dx; });
    }
    function slideFrames(dx: number): Keyframe[] {
        return [{ transform: 'translateX(0px)' }, { transform: 'translateX(' + dx + 'px)' }];
    }

    function titleIn(): void {
        title.style.setProperty('--t0', T0 + 'ms');
        try {
            const keeps = title.querySelectorAll('.keep');
            slides = slideDistances(title).map(function (dx, i) {
                return keeps[i].animate(slideFrames(dx),
                    { delay: SLIDE_AT, duration: SLIDE, easing: SLIDE_EASE, fill: 'forwards' });
            });
            const mine = slides;
            title.classList.add('go');

            Promise.all(mine.map(function (a) { return a.finished; })).then(function () {
                slides = null;
                title.classList.add('settled');
                mine.forEach(function (a) { a.cancel(); });
            }).catch(function () {});   // cancelled: go()'s catch settles instead
        } catch (e) {
            title.classList.add('settled');   // no slide, but never a blank title
        }
    }

    // After a resize, while the letters are still to land: measure again,
    // on a still copy of the title (the live letters are mid-slide).
    function remeasureSlides(): void {
        const copy = title.cloneNode(true) as HTMLElement;
        copy.classList.remove('go');
        copy.setAttribute('aria-hidden', 'true');
        copy.style.visibility = 'hidden';
        document.body.appendChild(copy);
        slideDistances(copy).forEach(function (dx, i) { (slides![i].effect as KeyframeEffect).setKeyframes(slideFrames(dx)); });
        document.body.removeChild(copy);
    }

    // How far the band travels: from just left of "AutoCua" to just right
    // (half the word, plus the band's 0.7em half width in .sheen's mask).
    function sheenTravel(): number {
        const first = (sheenArt.firstElementChild as Element).getBoundingClientRect();
        const last = (sheenArt.lastElementChild as Element).getBoundingClientRect();
        return (last.right - first.left) / 2 + 0.7 * parseFloat(getComputedStyle(sheen).fontSize);
    }

    // The row's ONE transform animation: it grows from 0.85x (ease
    // "settle"), holds, then shrinks to 1x around its feet (ease
    // "inOutExpo"). It is laid out big, so 1x is scale(1/B).
    function rowFrames(B: number): Keyframe[] {
        const D = SHRINK_AT + SHRINK;
        return [{ offset: 0, transform: 'scale(0.85)', easing: EASE_SETTLE },
                { offset: GROW / D, transform: 'scale(1)' },
                { offset: SHRINK_AT / D, transform: 'scale(1)', easing: EASE_EXPO },
                { offset: 1, transform: 'scale(' + (1 / B) + ')' }];
    }

    // A pair of eyes' ONE animation: they wake at `at` (open out of a line,
    // ease "outCubic"), then blink at each of the `blinks` times: shut,
    // about a frame closed, open again.
    function wake(eyes: Element, at: number, blinks: number[]): void {
        const span = blinks[blinks.length - 1] + BLINK - at;
        function key(t: number, open: number, easing?: string): Keyframe {
            return { offset: (t - at) / span, opacity: 1, transform: 'scaleY(' + open + ')', easing: easing || 'linear' };
        }
        const frames: Keyframe[] = [{ offset: 0, opacity: 0, transform: 'scaleY(0.06)', easing: EASE_OUT }, key(at + WAKE, 1)];
        blinks.forEach(function (b) {
            frames.push(key(b, 1, 'ease-in'), key(b + 0.45 * BLINK, 0.1),
                        key(b + 0.51 * BLINK, 0.1, 'ease-out'), key(b + BLINK, 1));
        });
        eyes.animate(frames, { delay: at, duration: span, fill: 'both' });
    }

    // Every animation is created here, in one task, each with its own delay.
    function start(): void {
        const g = layout(), B = g.big;
        // Comes in as Atlas's hero: fades in big, grows from 0.85x out of a
        // blur (only the lead shows; the others wait behind it), then
        // shrinks to 1x around its feet and rises into place...
        lift.animate([{ opacity: 0 }, { opacity: 1 }],
                     { delay: FIRST, duration: FADE, fill: 'both' });
        zoom = row.animate(rowFrames(B), { delay: FIRST, duration: SHRINK_AT + SHRINK, fill: 'both' });
        row.animate([{ filter: 'blur(6px)' }, { filter: 'blur(0px)' }],
                    { delay: FIRST, duration: GROW, easing: EASE_SETTLE, fill: 'backwards' });
        rise = lift.animate([{ transform: 'translateY(' + g.drop + 'px)' }, { transform: 'translateY(0px)' }],
                            { delay: FIRST + SHRINK_AT, duration: SHRINK, easing: EASE_EXPO, fill: 'both' });
        // ...while the others fan out from behind it into the lineup, fading
        // in together and waking as they come.
        side.animate([{ opacity: 0 }, { opacity: 1 }],
                     { delay: FIRST + SIDE_AT, duration: SIDE_FADE, fill: 'both' });
        slots.forEach(function (s, i) {
            const eyes = s.querySelector('.eyes') as Element;
            const last = BLINK_AT + i * BLINK_GAP;   // all of them blink at the end
            if (i === LEAD) {
                // the lead wakes up and blinks while it is big
                wake(eyes, FIRST + WAKE_AT, [FIRST + BLINK1_AT, last]);
            } else {
                const at = FIRST + FAN_AT + TEAM[i][2];
                fans[i] = s.animate([{ transform: behind(g, i) }, { transform: placed(g, i) }],
                                    { delay: at, duration: FAN, easing: EASE_FAN, fill: 'both' });
                wake(eyes, at + 250, [last]);
            }
        });
        titleIn();
        // "AutoCua" stays; one shimmer crosses it.
        const k = sheenTravel();
        sweep = sheen.animate([{ transform: 'translateX(' + (-k) + 'px)' }, { transform: 'translateX(' + k + 'px)' }],
                              { delay: SHEEN_AT, duration: SHEEN, easing: EASE_SHEEN, fill: 'both' });
        still = sheenArt.animate([{ transform: 'translateX(' + k + 'px)' }, { transform: 'translateX(' + (-k) + 'px)' }],
                                 { delay: SHEEN_AT, duration: SHEEN, easing: EASE_SHEEN, fill: 'both' });
        sheen.style.visibility = 'visible';   // its band is off the word until the sweep
        setTimeout(handOff, END);
        window.addEventListener('resize', whileShown(layout));
    }

    // A layer that fails to load would draw a broken-image box: hide its
    // bot instead. Checked now, on its error event, and again in go().
    const imgs = Array.prototype.slice.call(root.querySelectorAll('.bot img')) as HTMLImageElement[];
    function hideIfBroken(im: HTMLImageElement): void {
        if (im.complete && !im.naturalWidth) (im.closest('.bot') as HTMLElement).style.visibility = 'hidden';
    }
    imgs.forEach(function (im) {
        hideIfBroken(im);
        im.addEventListener('error', function () { hideIfBroken(im); });
    });

    if (reduced) {
        setTimeout(handOff, 600);
        settle();
        window.addEventListener('resize', whileShown(settle));
        return;
    }

    // Start once every bot layer is decoded (they are small and local), so
    // no bot comes in half drawn; never wait more than DECODE_MAX for that.
    let started = false;
    function go(): void {
        if (started) return;
        started = true;
        // The intro runs from now: the watchdog counts from here too.
        clearTimeout(watchdog);
        watchdog = setTimeout(handOff, END + 1000);
        // A failed layer's decode has rejected by now, but its error event
        // may still be queued: hide its bot before the team shows.
        imgs.forEach(hideIfBroken);
        try {
            start();
        } catch (e) {
            // No motion then, but the still end state and the hand-off.
            console.error(e);
            setTimeout(handOff, 600);
            if (root.getAnimations) {
                root.getAnimations({ subtree: true }).forEach(function (a) { a.cancel(); });
            }
            settle();
            window.addEventListener('resize', whileShown(settle));
        }
    }
    Promise.all(imgs.map(function (im) {
        return im.decode ? im.decode().catch(function () {}) : null;
    })).then(go);
    setTimeout(go, DECODE_MAX);
}
