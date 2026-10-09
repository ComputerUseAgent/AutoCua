// =====================================================================
// Tool-response flow — EVENT-DRIVEN. The chain is built dynamically as the
// agent runs; each logo is a painter in SHAPES, each step maps a backend
// tool/phase to {shape, label} (see OPEN + TOOL_MAP).
//
// Backend drives it from app.py via pywebview — one event per turn from the main driver:
//     window.toolFlow.onFlow('run_start')
//     window.toolFlow.onFlow('turn', '{"hasImage":true}')   // screenshot+mapping+communicate+thinking
//     window.toolFlow.onFlow('received', '{"tools":[{"name":"left_click","clicks":2},{"name":"input"}]}')
//     window.toolFlow.onFlow('run_end')
// `received` ticks "packet received" then plays this turn's tools (read from the action
// block). Image-less turns (a web SEARCH or a sub-agent turn, neither of which takes a
// screenshot) pass hasImage:false to skip screenshot+mapping.
//
// Four agents drive this: computer use, Shell use, iOS, and the browser agent. The last
// one is Rust and emits the same five events from AutoCua/web/agent/main_driver/service.rs;
// it always scans, so its turns are always hasImage:true. Three of them send a tool called
// `click` and two send `scroll`, meaning the same name has to draw different logos per
// agent — see mobileRun / webRun and toolSpec.
//
// The chain stays EMPTY on app start and only animates while an agent run is driving it.
// A built-in demo exists but is opt-in only (window.toolFlow.startTest()).
// =====================================================================
(function () {
    'use strict';

    /* ---------------- "N tools used" counter hooks ---------------- */
    window.setToolCount = function (n) {
        var el = document.getElementById('toolUsedCount');
        if (el) el.textContent = String(Math.max(0, n | 0));
    };
    window.bumpToolCount = function () {
        var el = document.getElementById('toolUsedCount');
        if (!el) return;
        el.textContent = String((parseInt(el.textContent, 10) || 0) + 1);
    };
    window.resetToolCount = function () {
        var el = document.getElementById('toolUsedCount');
        if (el) el.textContent = '0';
    };

    /* ---------------- icon geometry ---------------- */
    var ICON = 24;                              // icon size (css px); keep == CSS --tf-icon
                                                // (every shape scales off it: VSCALE, the orb's
                                                //  R/rs, the tick, the "!" badge, the conveyor pitch)
    var COLOR = 'rgba(85, 90, 100, 0.92)';
    var VSCALE = (ICON / 2) / 44;              // source coords (~±42) -> icon
    var TYPE_MS = 12;                          // blazing-fast per-char typing
    var TICK_MS = 400;                         // shape -> tick crossfade duration

    /* ---------------- shape draws (each returns a painter(ctx,cx,cy,now,alpha)) ----
       To add a new logo: write a draw, add it to SHAPES, reference it from a STEP. */
    function rrect(ctx, x, y, w, h, r) { ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(x, y, w, h, r); else ctx.rect(x, y, w, h); }

    // vector shapes are authored in source coords (~±35) and drawn via vecPaint,
    // which centres + scales them into the icon.
    function vecScreen(ctx) {
        ctx.lineWidth = 5; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        rrect(ctx, -35, -25, 70, 45, 6); ctx.stroke();
        rrect(ctx, -20, 30, 40, 6, 3); ctx.fill();
        ctx.beginPath(); ctx.rect(-6, 20, 12, 10); ctx.fill();
    }
    var TREE_NODES = [{ x: 0, y: -25 }, { x: -20, y: 0 }, { x: 20, y: 0 }, { x: -35, y: 25 }, { x: -20, y: 25 }, { x: -5, y: 25 }, { x: 5, y: 25 }, { x: 20, y: 25 }, { x: 35, y: 25 }];
    var TREE_EDGES = [[0, 1], [0, 2], [1, 3], [1, 4], [1, 5], [2, 6], [2, 7], [2, 8]];
    function vecTree(ctx) {
        ctx.lineWidth = 3; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath();
        TREE_EDGES.forEach(function (e) { ctx.moveTo(TREE_NODES[e[0]].x, TREE_NODES[e[0]].y); ctx.lineTo(TREE_NODES[e[1]].x, TREE_NODES[e[1]].y); });
        ctx.stroke();
        TREE_NODES.forEach(function (nd) { ctx.beginPath(); ctx.arc(nd.x, nd.y, 4.5, 0, Math.PI * 2); ctx.fill(); });
    }
    function vecServer(ctx) {
        ctx.lineWidth = 4; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        rrect(ctx, -25, -35, 50, 70, 4); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-25, -12); ctx.lineTo(25, -12); ctx.moveTo(-25, 12); ctx.lineTo(25, 12); ctx.stroke();
        [-23.5, 0, 23.5].forEach(function (yc) {
            ctx.beginPath(); ctx.arc(-15, yc, 2.5, 0, Math.PI * 2); ctx.fill();
            ctx.beginPath(); ctx.moveTo(-5, yc); ctx.lineTo(15, yc); ctx.stroke();
        });
    }
    function vecPaint(ctx, cx, cy, alpha, fn) {
        ctx.save();
        ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
        ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
        fn(ctx); ctx.restore();
    }
    function vecPainter(fn) { return function (ctx, cx, cy, now, alpha) { vecPaint(ctx, cx, cy, alpha, fn); }; }

    // loader — 12 outer + 8 inner counter-rotating spokes (matches the source)
    var L_OUTER = 12, L_INNER = 8, L_PER = 14;
    function genLoader() {
        var d = [], i, j, t;
        for (i = 0; i < L_OUTER; i++) { var a = (i / L_OUTER) * Math.PI * 2; for (j = 0; j < L_PER; j++) { t = j / (L_PER - 1); d.push({ type: 'o', angle: a, r: 32 + t * 10 }); } }
        for (i = 0; i < L_INNER; i++) { var a2 = (i / L_INNER) * Math.PI * 2; for (j = 0; j < L_PER; j++) { t = j / (L_PER - 1); d.push({ type: 'i', angle: a2, r: 16 + t * 6 }); } }
        return d;
    }
    function loaderPainter() {
        var loader = genLoader();
        return function (ctx, cx, cy, now, alpha) {
            ctx.fillStyle = COLOR; ctx.globalAlpha = alpha * 0.9;
            for (var i = 0; i < loader.length; i++) {
                var l = loader[i];
                var a = l.angle + (l.type === 'o' ? now * 0.0015 : -now * 0.0025);
                ctx.beginPath();
                ctx.arc(cx + Math.cos(a) * l.r * VSCALE, cy + Math.sin(a) * l.r * VSCALE, 0.6, 0, Math.PI * 2);
                ctx.fill();
            }
        };
    }

    // globe — the "searching" thinking-orb: a dotted lat/long sphere with a scan
    // meridian sweeping it (port of all-orbs.html `globe` mode / drawGlobe, with
    // the small-size preset already resolved: radii × 1.75, scanMul 4.335,
    // speed 2.665). The dot COUNT runs denser than the shipped 0.105 preset
    // (which gave 6 × 14 = 54 dots and read too sparse/light here): the rings
    // are also stepped up past what the count scale would pair with 19 lon,
    // for a tighter vertical read — 9 × 19 = 108 dots. One more ring and the
    // rows start merging near the poles at icon size.
    // The orb ships as grayscale ink; here each dot's ink drives its ALPHA over
    // the chain grey instead, so the near/far depth read survives in one colour.
    // Both ramps sit HIGHER than the orb's own ink values (which mapped to
    // 0.17–0.88 and read washed-out next to the solid-grey logos on either side):
    // G_A_* is the near/far depth ramp, G_DIM the floor for dots off the meridian.
    var ORB_RGB = '85, 90, 100';                 // == COLOR, minus its alpha
    var G_SPIN = 0.5, G_SPEED = 2.665, G_SCAN_MUL = 4.335;
    var G_LAT = 9, G_LON = 19;                   // rings × dots-per-ring
    var G_RBASE = 1.05, G_RDEPTH = 2.975, G_RBOOST = 1.0, G_RMIN = 0.3;
    var G_A_FAR = 0.62, G_A_NEAR = 1.0, G_DIM = 0.68;
    function globePainter() {
        var R = (ICON / 2) * 0.82;
        var rs = Math.pow(ICON / 300, 0.6);      // radii were tuned on a 300pt frame
        var dots = [];
        return function (ctx, cx, cy, now, alpha) {
            var t = (now / 1000) * G_SPEED;
            var yaw = t * G_SPIN, tilt = 0.4 + 0.06 * Math.sin(t * 0.35);
            var st = Math.sin(tilt), ct = Math.cos(tilt), sy = Math.sin(yaw), cyw = Math.cos(yaw);
            // the scan sweeps relative to the spin
            var scan = t * (G_SPIN + (1.7 - G_SPIN) * G_SCAN_MUL);
            dots.length = 0;
            for (var li = 0; li <= G_LAT; li++) {
                var lat = -Math.PI / 2 + (li / G_LAT) * Math.PI;
                var cosLat = Math.cos(lat), sinLat = Math.sin(lat);
                var lonCount = Math.max(1, Math.round(Math.abs(cosLat) * G_LON));
                for (var lj = 0; lj < lonCount; lj++) {
                    var lon = (lj / lonCount) * 2 * Math.PI;
                    // spin + tilt + orthographic projection (makeProj, inlined)
                    var x1 = (cosLat * Math.cos(lon)) * cyw + (cosLat * Math.sin(lon)) * sy;
                    var z1 = -(cosLat * Math.cos(lon)) * sy + (cosLat * Math.sin(lon)) * cyw;
                    var y1 = sinLat * ct - z1 * st;
                    var z = sinLat * st + z1 * ct;
                    var depth = (z + 1) / 2;
                    // the scan: a moving meridian read as a size ripple, not a shine
                    var dA = lon + yaw - scan;
                    var d = Math.atan2(Math.sin(dA), Math.cos(dA));
                    var boost = Math.exp(-(d * d) / 0.18) * Math.max(0, z);
                    dots.push({
                        x: cx + x1 * R, y: cy - y1 * R, z: z,
                        r: Math.max(G_RMIN, (G_RBASE + G_RDEPTH * depth + G_RBOOST * boost) * rs),
                        // ink → alpha: near dots read solid, far ones recede;
                        // G_DIM keeps everything off the meridian a step quieter
                        a: (G_A_FAR + (G_A_NEAR - G_A_FAR) * depth) * (G_DIM + (1 - G_DIM) * Math.min(1, boost))
                    });
                }
            }
            dots.sort(function (p, q) { return p.z - q.z; });   // painter: far → near
            ctx.globalAlpha = 1;
            for (var i = 0; i < dots.length; i++) {
                var dot = dots[i];
                ctx.fillStyle = 'rgba(' + ORB_RGB + ',' + (alpha * dot.a).toFixed(3) + ')';
                ctx.beginPath(); ctx.arc(dot.x, dot.y, dot.r, 0, Math.PI * 2); ctx.fill();
            }
        };
    }

    // composing — the "composing" thinking-orb: an undulating multi-band sash
    // (port of all-orbs.html `ribbon` mode / drawRibbon). spin = 0, so the band's
    // ORIENTATION is frozen and only the travelling wave moves — that lets the
    // whole projection basis be precomputed once. Painted in the chain grey like
    // the globe orb: ink → alpha, near dots solid, far ones receding.
    // Counts + radii come from the reference's BIG (64) preset — counts × 0.25 →
    // 3 lanes × 44 segs + 38 ghost dots, then bandMul 3.9 → 12 lanes; radii × 0.85
    // — NOT its small (20) one. The small preset resolves to 10 coarse lanes,
    // 20 segs and only 8 scaffold dots, which at icon size reads as a thin ring of
    // merged strokes rather than the round dotted sash the reference shows. The
    // small preset's faster speed (3.12 vs 2.34) is kept so the wave still travels
    // visibly during the composing beat, and R is widened 0.78 → 0.86 to fill
    // more of the box.
    var C_SPEED = 3.12, C_LANES = 12, C_SEGS = 44, C_GHOST = 38;
    var C_RBASE = 0.94, C_RDEPTH = 1.45, C_RMIN = 0.3;
    var C_A_FAR = 0.42, C_A_NEAR = 1.0;          // depth ramp for the band
    var C_GHOST_A = 0.14, C_GHOST_SPAN = 0.26;   // the faint scaffold it rides on
    function composingPainter() {
        var R = (ICON / 2) * 0.86;
        var rs = Math.pow(ICON / 300, 0.6);
        // band plane basis (u, v, n = u × v) at the frozen orientation
        var ta = 0.55, sTa = Math.sin(ta), cTa = Math.cos(ta);
        var vy = cTa, vz = sTa;                  // u = (1,0,0), v = (0,cos ta,sin ta)
        var ny = -vz, nz = vy;                   // n = u × v = (0,-vz,vy)
        // makeProj(0, 0.3, …): yaw 0 leaves x alone; tilt folds y/z
        var sT = Math.sin(0.3), cT = Math.cos(0.3);
        // the fibonacci scaffold never moves either — resolve it once
        var golden = Math.PI * (3 - Math.sqrt(5)), ghosts = [];
        for (var gi = 0; gi < C_GHOST; gi++) {
            var gy = 1 - (2 * (gi + 0.5)) / C_GHOST;
            var grad = Math.sqrt(1 - gy * gy), ga = gi * golden;
            var gx3 = grad * Math.cos(ga), gy3 = gy, gz3 = grad * Math.sin(ga);
            var gz = (gy3 * sT + gz3 * cT) * R;
            ghosts.push({
                x: gx3 * R, y: -(gy3 * cT - gz3 * sT) * R, z: gz,
                // the reference's paint() floors EVERY dot at rMin, scaffold included
                // — at icon size 0.8 * rs (0.18px) sits well under it
                r: Math.max(C_RMIN, 0.8 * rs),
                a: C_GHOST_A + C_GHOST_SPAN * ((gz / R + 1) / 2)
            });
        }
        var dots = [];
        return function (ctx, cx, cy, now, alpha) {
            var t = (now / 1000) * C_SPEED;
            dots.length = 0;
            for (var g = 0; g < ghosts.length; g++) {
                var gh = ghosts[g];
                dots.push({ x: cx + gh.x, y: cy + gh.y, z: gh.z, r: gh.r, a: gh.a });
            }
            var mid = (C_LANES - 1) / 2, half = Math.max(1, mid);
            for (var w = 0; w < C_LANES; w++) {
                var laneOff = (w - mid) * 0.075;
                var edge = Math.abs(w - mid) / half;
                for (var k = 0; k < C_SEGS; k++) {
                    var a = (k / C_SEGS) * 2 * Math.PI;
                    // the undulation: two travelling waves along the band
                    var off = laneOff
                        + 0.16 * Math.sin(a * 3 - t * 1.7 + w * 0.22)
                        + 0.07 * Math.sin(a * 5 + t * 1.1);
                    var ca = Math.cos(a), sa = Math.sin(a);
                    var x = ca, y = vy * sa + ny * off, z = vz * sa + nz * off;
                    var l = Math.sqrt(x * x + y * y + z * z) || 1;
                    x = (x / l) * R; y = (y / l) * R; z = (z / l) * R;
                    var zr = y * sT + z * cT;
                    var depth = (zr / R + 1) / 2;
                    dots.push({
                        x: cx + x, y: cy - (y * cT - z * sT), z: zr,
                        r: Math.max(C_RMIN, (C_RBASE + C_RDEPTH * depth) * (1 - 0.25 * edge) * rs),
                        // outer lanes read a touch lighter, as in the source
                        a: (C_A_FAR + (C_A_NEAR - C_A_FAR) * depth) * (1 - 0.22 * edge)
                    });
                }
            }
            dots.sort(function (p, q) { return p.z - q.z; });   // painter: far → near
            ctx.globalAlpha = 1;
            for (var i = 0; i < dots.length; i++) {
                var dot = dots[i];
                ctx.fillStyle = 'rgba(' + ORB_RGB + ',' + (alpha * dot.a).toFixed(3) + ')';
                ctx.beginPath(); ctx.arc(dot.x, dot.y, dot.r, 0, Math.PI * 2); ctx.fill();
            }
        };
    }

    // AI mascot head — rounded square + blinking white eyes (ported from
    // tool_animation.html shape 'a'). Two-tone + animated, so it needs its own
    // painter rather than the single-colour vecPainter wrapper.
    function agentPainter() {
        return function (ctx, cx, cy, now, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            // head
            ctx.globalAlpha = alpha * 0.92; ctx.fillStyle = COLOR;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-30, -30, 60, 60, 13); else ctx.rect(-30, -30, 60, 60);
            ctx.fill();
            // blink (1.8s loop) — eyes squash near the end of each cycle
            var bt = now % 1800, sy = 1;
            if (bt > 1620) sy = bt < 1710 ? 1 - ((bt - 1620) / 90) * 0.92 : 0.08 + ((bt - 1710) / 90) * 0.92;
            var eh = 14 * sy, ey = -5 - eh / 2, er = Math.min(4, eh / 2);
            ctx.globalAlpha = alpha; ctx.fillStyle = '#ffffff';
            ctx.beginPath();
            if (ctx.roundRect) { ctx.roundRect(-12, ey, 8, eh, er); ctx.roundRect(4, ey, 8, eh, er); }
            else { ctx.rect(-12, ey, 8, eh); ctx.rect(4, ey, 8, eh); }
            ctx.fill();
            ctx.restore();
        };
    }

    // Apple terminal — window + ">" prompt + Apple logo (shape 'sh'). Build the
    // glyph path once; single-colour so it rides the vecPainter wrapper.
    var APPLE_PATH = (typeof Path2D !== 'undefined')
        ? new Path2D("M12.152 6.896c-.948 0-2.415-1.078-3.96-1.04-2.04.027-3.91 1.183-4.961 3.014-2.117 3.675-.546 9.103 1.519 12.09 1.013 1.454 2.208 3.09 3.792 3.039 1.52-.065 2.09-.987 3.935-.987 1.831 0 2.35.987 3.96.948 1.637-.026 2.676-1.48 3.676-2.948 1.156-1.688 1.636-3.325 1.662-3.415-.039-.013-3.182-1.221-3.22-4.857-.026-3.04 2.48-4.494 2.597-4.559-1.429-2.09-3.623-2.324-4.39-2.376-2-.156-3.675 1.09-4.61 1.09zM15.53 3.83c.843-1.012 1.4-2.427 1.245-3.83-1.207.052-2.662.805-3.532 1.818-.78.896-1.454 2.338-1.273 3.714 1.338.104 2.715-.688 3.559-1.701z")
        : null;
    function vecApple(ctx) {
        ctx.lineWidth = 3.5; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-35, -25, 70, 50, 4); else ctx.rect(-35, -25, 70, 50);
        ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-35, -15); ctx.lineTo(35, -15); ctx.stroke();   // top bar
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(-26, -9); ctx.lineTo(-21, -5); ctx.lineTo(-26, -1); ctx.stroke(); // ">"
        if (APPLE_PATH) { ctx.save(); ctx.translate(0, 4); ctx.scale(1.3, 1.3); ctx.translate(-12, -12); ctx.fill(APPLE_PATH); ctx.restore(); }
    }

    // Terminal shell — window + ">" prompt + three output lines (shape 'cl').
    function vecShell(ctx) {
        ctx.lineWidth = 3.5; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-35, -25, 70, 50, 4); else ctx.rect(-35, -25, 70, 50);
        ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-35, -15); ctx.lineTo(35, -15); ctx.stroke();   // top bar
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(-26, -9); ctx.lineTo(-21, -5); ctx.lineTo(-26, -1); ctx.stroke(); // ">"
        [[-16, 14, -5], [-26, 20, 5], [-26, 4, 15]].forEach(function (L) {
            ctx.beginPath(); ctx.moveTo(L[0], L[2]); ctx.lineTo(L[1], L[2]); ctx.stroke();
        });
    }

    // Clock — face ring + 12 ticks + hands at 10:10 + hub (shape 'ck'). Static.
    var _hourA = (10 + 10 / 60) * 30 * Math.PI / 180, _minA = 10 * 6 * Math.PI / 180;
    var CLOCK_HOUR_END = { x: Math.sin(_hourA) * 16, y: -Math.cos(_hourA) * 16 };
    var CLOCK_MIN_END  = { x: Math.sin(_minA) * 25,  y: -Math.cos(_minA) * 25 };
    function vecClock(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3.5; ctx.beginPath(); ctx.arc(0, 0, 32, 0, Math.PI * 2); ctx.stroke();
        ctx.lineWidth = 2.5;
        for (var i = 0; i < 12; i++) {
            var a = i * 30 * Math.PI / 180, dx = Math.sin(a), dy = -Math.cos(a);
            ctx.beginPath(); ctx.moveTo(dx * 28, dy * 28); ctx.lineTo(dx * 31.5, dy * 31.5); ctx.stroke();
        }
        ctx.lineWidth = 4; ctx.beginPath(); ctx.moveTo(0, 0); ctx.lineTo(CLOCK_HOUR_END.x, CLOCK_HOUR_END.y); ctx.stroke();
        ctx.lineWidth = 3; ctx.beginPath(); ctx.moveTo(0, 0); ctx.lineTo(CLOCK_MIN_END.x, CLOCK_MIN_END.y); ctx.stroke();
        ctx.beginPath(); ctx.arc(0, 0, 2.6, 0, Math.PI * 2); ctx.fill();
    }

    // Keyboard — body + two rows of keys + spacebar (shape 'kb'). Static.
    var KEY_XS = [-27.5, -16.5, -5.5, 5.5, 16.5, 27.5], KEY_YS = [-11, -1];
    function vecKeyboard(ctx) {
        ctx.lineWidth = 3.5; ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-38, -20, 76, 40, 5); else ctx.rect(-38, -20, 76, 40);
        ctx.stroke();
        KEY_YS.forEach(function (ky) {
            KEY_XS.forEach(function (kx) {
                ctx.beginPath();
                if (ctx.roundRect) ctx.roundRect(kx - 3.5, ky - 3.5, 7, 7, 1.5); else ctx.rect(kx - 3.5, ky - 3.5, 7, 7);
                ctx.fill();
            });
        });
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-18, 7, 36, 6, 2); else ctx.rect(-18, 7, 36, 6);
        ctx.fill();
    }

    // Mouse with scrolling wheel — body + wheel pill + button split + animated
    // scroll tick (shape 'mo'). The tick oscillates so it reads as "scrolling".
    var MOUSE_SCROLL_CENTER = -15.5;
    function mousePainter() {
        return function (ctx, cx, cy, now, alpha) {
            var scrollY = MOUSE_SCROLL_CENTER + 3.0 * Math.sin(now / 200);
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';
            ctx.lineWidth = 3.5; ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-15, -26, 30, 48, [15, 15, 12, 12]); else ctx.rect(-15, -26, 30, 48);
            ctx.stroke();
            ctx.lineWidth = 2.5; ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-3, -20, 6, 9, 3); else ctx.rect(-3, -20, 6, 9);
            ctx.stroke();
            ctx.beginPath(); ctx.moveTo(0, -9); ctx.lineTo(0, 4); ctx.stroke();
            ctx.lineWidth = 2.2; ctx.beginPath(); ctx.moveTo(-1.8, scrollY); ctx.lineTo(1.8, scrollY); ctx.stroke();
            ctx.restore();
        };
    }

    // Cursor (left-pointing arrow) with a "breathing" click pulse — 1/2/3 presses
    // per 2s loop (shapes 'cu*'). Animated + pulses about its own pivot.
    // The arrow's MASS (the triangle) sits at x≈-14..5 while its thin tail runs out
    // to 20, so drawn as authored the body reads left of the chain's connector line.
    // CURSOR_RECENTER nudges the whole glyph right so the triangle straddles the line.
    var CURSOR_PTS = [[-11, -27], [-11, 16], [2, 6], [20, 4]], CURSOR_PIVOT = { x: -1, y: -4 };
    var CURSOR_RECENTER = 3;
    function clickScale(now, clicks) {
        var local = now % 2000, dip;
        if (clicks === 3) dip = Math.min(1, Math.exp(-Math.pow((local - 700) / 80, 2)) + Math.exp(-Math.pow((local - 960) / 80, 2)) + Math.exp(-Math.pow((local - 1220) / 80, 2)));
        else if (clicks === 2) dip = Math.min(1, Math.exp(-Math.pow((local - 850) / 85, 2)) + Math.exp(-Math.pow((local - 1150) / 85, 2)));
        else dip = Math.exp(-Math.pow((local - 1000) / 160, 2));
        return 1.0 - 0.15 * dip;
    }
    function cursorPainter(clicks, flip) {
        return function (ctx, cx, cy, now, alpha) {
            var s = clickScale(now, clicks);
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            if (flip) ctx.scale(-1, 1);                       // mirror -> points up-right (right click)
            ctx.translate(CURSOR_RECENTER, 0);                // after the flip, so it mirrors too
            ctx.globalAlpha = alpha * 0.92; ctx.fillStyle = COLOR; ctx.strokeStyle = COLOR;
            ctx.translate(CURSOR_PIVOT.x, CURSOR_PIVOT.y); ctx.scale(s, s); ctx.translate(-CURSOR_PIVOT.x, -CURSOR_PIVOT.y);
            ctx.lineWidth = 6; ctx.lineJoin = 'round'; ctx.lineCap = 'round';
            ctx.beginPath(); ctx.moveTo(CURSOR_PTS[0][0], CURSOR_PTS[0][1]);
            for (var i = 1; i < CURSOR_PTS.length; i++) ctx.lineTo(CURSOR_PTS[i][0], CURSOR_PTS[i][1]);
            ctx.closePath(); ctx.fill(); ctx.stroke();
            ctx.restore();
        };
    }

    // Pen taking notes — tilted pen + nib + three note lines; the top line's left
    // end sweeps in/out so it reads as "writing" (shape 'pn'). Animated.
    var PEN_LINE_REST = -26, PEN_RECENTER = 3.5;
    function drawPenShape(ctx, lineLeftX) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.translate(PEN_RECENTER, 0);    // note lines run left, pen tip right -> mass sat left
        ctx.lineWidth = 6.5; ctx.beginPath(); ctx.moveTo(22, -20); ctx.lineTo(0, 2); ctx.stroke();
        ctx.lineWidth = 3; ctx.beginPath(); ctx.moveTo(0, 2); ctx.lineTo(-5, 7); ctx.stroke();
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(lineLeftX, 10); ctx.lineTo(-7, 10); ctx.stroke();   // writing line
        ctx.beginPath(); ctx.moveTo(-27, 17); ctx.lineTo(3, 17); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-27, 24); ctx.lineTo(-11, 24); ctx.stroke();
    }
    function penPainter() {
        return function (ctx, cx, cy, now, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawPenShape(ctx, PEN_LINE_REST + 8 * Math.sin(now / 230));
            ctx.restore();
        };
    }

    // Camera — body + viewfinder bump + lens (two rings) + flash dot (shape 'cm'). Static.
    function vecCamera(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3.5;
        ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(-35, -16, 70, 38, 6); else ctx.rect(-35, -16, 70, 38); ctx.stroke();
        ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(-11, -23, 22, 8, 3); else ctx.rect(-11, -23, 22, 8); ctx.stroke();
        ctx.beginPath(); ctx.arc(0, 3, 13, 0, Math.PI * 2); ctx.stroke();
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.arc(0, 3, 7.5, 0, Math.PI * 2); ctx.stroke();
        ctx.beginPath(); ctx.arc(23, -9, 2.5, 0, Math.PI * 2); ctx.fill();
    }

    // Drag-and-drop — dashed drop-zone + a box that glides into it (shape 'dd'). Animated.
    var DRAG_START = { x: -18, y: -14 }, DRAG_TARGET = { x: 17, y: 12 }, DRAG_RECENTER = -2.5;
    function drawDragDropShape(ctx, bx, by) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.translate(DRAG_RECENTER, 0);   // drop-zone sits right of the box's travel start
        ctx.lineWidth = 2.5; ctx.setLineDash([4, 4]);
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(2, -2, 30, 28, 4); else ctx.rect(2, -2, 30, 28);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.lineWidth = 3; ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(bx - 9, by - 9, 18, 18, 3); else ctx.rect(bx - 9, by - 9, 18, 18);
        ctx.stroke();
    }
    function dragDropPainter() {
        return function (ctx, cx, cy, now, alpha) {
            var theta = (now % 2400) / 2400 * Math.PI * 2, e = (1 - Math.cos(theta)) / 2;
            var bx = DRAG_START.x + (DRAG_TARGET.x - DRAG_START.x) * e;
            var by = DRAG_START.y + (DRAG_TARGET.y - DRAG_START.y) * e;
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawDragDropShape(ctx, bx, by);
            ctx.restore();
        };
    }

    // Todo list — 3 rows (checkbox + task line). alphas[i]>0 draws a tick + strike on
    // row i. 'todo' shows it empty (created); 'todoUpd' ticks the rows off (updated).
    var TODO_ROWS = [-16, 0, 16], TODO_ENDS = [22, 26, 14], TODO_RECENTER = 6;
    function drawTodoShape(ctx, alphas) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.translate(TODO_RECENTER, 0);   // boxes left + long rows right -> mass sat left of the line
        for (var i = 0; i < TODO_ROWS.length; i++) {
            var ry = TODO_ROWS[i];
            ctx.lineWidth = 2.5; ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-31, ry - 4.5, 9, 9, 2); else ctx.rect(-31, ry - 4.5, 9, 9);
            ctx.stroke();
            ctx.lineWidth = 3; ctx.beginPath(); ctx.moveTo(-18, ry); ctx.lineTo(TODO_ENDS[i], ry); ctx.stroke();
        }
        for (var j = 0; j < TODO_ROWS.length; j++) {
            var a = alphas[j];
            if (a > 0.01) {
                var ry2 = TODO_ROWS[j];
                ctx.save(); ctx.globalAlpha *= a;
                ctx.lineWidth = 2.2; ctx.beginPath(); ctx.moveTo(-29.5, ry2); ctx.lineTo(-27, ry2 + 2.5); ctx.lineTo(-23.5, ry2 - 3); ctx.stroke();
                ctx.lineWidth = 2.5; ctx.beginPath(); ctx.moveTo(-18, ry2); ctx.lineTo(TODO_ENDS[j], ry2); ctx.stroke();
                ctx.restore();
            }
        }
    }
    function vecTodo(ctx) { drawTodoShape(ctx, [0, 0, 0]); }
    function todoUpdateAlphas(now) {
        var t = now % 1800;
        var tickIn = function (s) { if (t < s) return 0; if (t < s + 130) return (t - s) / 130; return 1; };
        var fade = 1;
        if (t > 1300 && t < 1600) fade = 1 - (t - 1300) / 300; else if (t >= 1600) fade = 0;
        return [tickIn(0) * fade, tickIn(120) * fade, tickIn(240) * fade];
    }
    function todoUpdatePainter() {
        return function (ctx, cx, cy, now, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawTodoShape(ctx, todoUpdateAlphas(now));
            ctx.restore();
        };
    }

    // Open book (scratchpad) in perspective — both pages + spine + text, with a right
    // leaf that lifts and turns around the spine (shape 'bk'). Animated.
    var BOOK_LINES = [[5, -11, 19, -10], [5, -4, 26, -3], [6, 3, 31, 4], [6, 10, 35, 11]];
    function drawBookPage(ctx, m) {
        ctx.beginPath();
        ctx.moveTo(0, -19);
        ctx.quadraticCurveTo(m * 10, -22, m * 20, -15);
        ctx.lineTo(m * 38, 16);
        ctx.quadraticCurveTo(m * 18, 21, 0, 19);
        ctx.stroke();
    }
    function drawBookShape(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round'; ctx.lineWidth = 3;
        drawBookPage(ctx, -1); drawBookPage(ctx, 1);
        ctx.lineWidth = 2.5; ctx.beginPath(); ctx.moveTo(0, -19); ctx.lineTo(0, 19); ctx.stroke();
        ctx.lineWidth = 2;
        BOOK_LINES.forEach(function (L) { ctx.beginPath(); ctx.moveTo(L[0], L[1]); ctx.lineTo(L[2], L[3]); ctx.stroke(); });
        BOOK_LINES.forEach(function (L) { ctx.beginPath(); ctx.moveTo(-L[0], L[1]); ctx.lineTo(-L[2], L[3]); ctx.stroke(); });
    }
    function drawBookLeaf(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round'; ctx.lineWidth = 3;
        drawBookPage(ctx, 1);
        ctx.lineWidth = 2;
        BOOK_LINES.forEach(function (L) { ctx.beginPath(); ctx.moveTo(L[0], L[1]); ctx.lineTo(L[2], L[3]); ctx.stroke(); });
    }
    function bookPainter() {
        return function (ctx, cx, cy, now, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawBookShape(ctx);
            var ang = (now % 1500) / 1500 * Math.PI;       // one page turn per 1.5s
            if (ang > 0.001) {
                ctx.save();
                ctx.transform(Math.cos(ang), -Math.sin(ang) * 0.75, 0, 1, 0, 0);  // foreshorten + lift
                drawBookLeaf(ctx);
                ctx.restore();
            }
            ctx.restore();
        };
    }

    // Open app box — isometric crate with splayed lid flaps (shape 'ap'). Static.
    // (the file's rising "apps" word is dropped — illegible at 16px.)
    function vecAppBox(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round'; ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(0, -14); ctx.lineTo(24, -6); ctx.lineTo(0, 2); ctx.lineTo(-24, -6); ctx.closePath(); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-24, -6); ctx.lineTo(-24, 14); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(0, 2); ctx.lineTo(0, 22); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(24, -6); ctx.lineTo(24, 14); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-24, 14); ctx.lineTo(0, 22); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(24, 14); ctx.lineTo(0, 22); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(0, -14); ctx.lineTo(-24, -6); ctx.lineTo(-39, -16); ctx.lineTo(-15, -24); ctx.closePath(); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(0, -14); ctx.lineTo(24, -6); ctx.lineTo(39, -16); ctx.lineTo(15, -24); ctx.closePath(); ctx.stroke();
    }

    // "done" tick badge — filled circle + white check
    function drawTick(ctx, cx, cy, alpha) {
        var R = ICON / 2 - 1.5;
        ctx.save(); ctx.translate(cx, cy); ctx.globalAlpha = alpha;
        ctx.fillStyle = COLOR; ctx.beginPath(); ctx.arc(0, 0, R, 0, Math.PI * 2); ctx.fill();
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.95)';
        ctx.lineWidth = 1.6; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath(); ctx.moveTo(-R * 0.42, 0); ctx.lineTo(-R * 0.12, R * 0.32); ctx.lineTo(R * 0.46, -R * 0.32); ctx.stroke();
        ctx.restore();
    }

    // Warning triangle with a "!" — shown when a run is interrupted or fails
    // (soft red so it stands out against the grey chain).
    function drawBang(ctx, cx, cy, alpha) {
        var R = ICON / 2 - 1.5;
        ctx.save(); ctx.translate(cx, cy); ctx.globalAlpha = alpha;   // the glyph is symmetric about cx
        var red = 'rgba(198, 92, 80, 0.95)';
        ctx.fillStyle = red; ctx.strokeStyle = red;
        ctx.lineJoin = 'round'; ctx.lineCap = 'round'; ctx.lineWidth = 2.2;
        // rounded triangle pointing up (round-join stroke softens the corners)
        ctx.beginPath();
        ctx.moveTo(0, -R);
        ctx.lineTo(R * 0.96, R * 0.72);
        ctx.lineTo(-R * 0.96, R * 0.72);
        ctx.closePath();
        ctx.fill(); ctx.stroke();
        // white "!" tucked into the triangle's lower body
        ctx.fillStyle = 'rgba(255, 255, 255, 0.97)';
        var ey = 0.6;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-0.85, ey - R * 0.42, 1.7, R * 0.55, 0.85); else ctx.rect(-0.85, ey - R * 0.42, 1.7, R * 0.55);
        ctx.fill();
        ctx.beginPath(); ctx.arc(0, ey + R * 0.42, 1.05, 0, Math.PI * 2); ctx.fill();
        ctx.restore();
    }
    function bangPainter() { return function (ctx, cx, cy, now, alpha) { drawBang(ctx, cx, cy, alpha); }; }

    /* ---------------- mobile (iOS) painters — ported from tool_icon.html ---------------- */
    function easeInOutCubic(x) { return x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2; }

    // phone + hand tapping the screen, with touch ripple — "tapping the screen"
    var TAP_TOUCH = { x: -14, y: -6 };   // touch point on the phone screen
    var TAP_TILT = -0.55;                // hand tilt: finger points up-left, fist to bottom-right
    var TAP_RECENTER = 11;               // phone+hand spans x≈[-36..17] — shift right so the
                                         // composition centers on the chain like the other icons
    function drawTapPhone(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3.5;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-36, -34, 42, 64, 8); else ctx.rect(-36, -34, 42, 64);
        ctx.stroke();
        ctx.lineWidth = 2.5;
        ctx.beginPath(); ctx.moveTo(-22, -27); ctx.lineTo(-8, -27); ctx.stroke();   // speaker slit
    }
    function tapHandPath(ctx) {
        // one closed outline: index finger up, two knuckle bumps, rounded fist
        ctx.beginPath();
        ctx.moveTo(-3.5, 3);
        ctx.quadraticCurveTo(-3.5, -1.5, 0, -1.5);    // rounded fingertip
        ctx.quadraticCurveTo(3.5, -1.5, 3.5, 3);
        ctx.lineTo(3.5, 13);                          // right edge of the finger
        ctx.quadraticCurveTo(6, 9.5, 8.5, 12.5);      // knuckle bump 1
        ctx.quadraticCurveTo(11.5, 9.5, 14, 13);      // knuckle bump 2
        ctx.quadraticCurveTo(17.5, 13, 17.5, 18);     // round into the fist's right side
        ctx.lineTo(17.5, 24);
        ctx.quadraticCurveTo(17.5, 30.5, 11, 30.5);   // bottom-right corner
        ctx.lineTo(3, 30.5);
        ctx.quadraticCurveTo(-3.5, 30.5, -3.5, 24);   // bottom-left corner
        ctx.closePath();                              // straight left edge back to the finger
    }
    function drawTapHand(ctx, tx, ty, lift) {
        ctx.save();
        ctx.translate(tx, ty);
        ctx.rotate(TAP_TILT);
        ctx.translate(0, lift);          // pull back along the finger axis when lifted
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        // knock out the phone lines underneath so the unfilled hand stays readable
        ctx.save();
        ctx.globalCompositeOperation = 'destination-out';
        ctx.globalAlpha = 1;
        tapHandPath(ctx);
        ctx.fill();
        ctx.lineWidth = 7;
        ctx.stroke();
        ctx.restore();
        ctx.lineWidth = 3;               // outline only — no fill
        tapHandPath(ctx);
        ctx.stroke();
        ctx.restore();
    }
    function tapPainter() {
        var CYCLE = 1700;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var press;                   // 0 = lifted, 1 = touching
            if (t < 260) press = easeInOutCubic(t / 260);
            else if (t < 700) press = 1;
            else if (t < 1000) press = 1 - easeInOutCubic((t - 700) / 300);
            else press = 0;
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.translate(TAP_RECENTER, 0);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawTapPhone(ctx);
            // ripple rings spread from the touch point while the finger is down
            if (t > 260 && t < 960) {
                var rp = (t - 260) / 700;
                ctx.save();
                ctx.lineWidth = 2.5;
                ctx.globalAlpha = alpha * 0.92 * Math.max(0, 1 - rp);
                ctx.beginPath(); ctx.arc(TAP_TOUCH.x, TAP_TOUCH.y, 6 + rp * 13, 0, Math.PI * 2); ctx.stroke();
                if (rp > 0.35) {
                    var rp2 = (rp - 0.35) / 0.65;
                    ctx.globalAlpha = alpha * 0.92 * Math.max(0, 1 - rp2);
                    ctx.beginPath(); ctx.arc(TAP_TOUCH.x, TAP_TOUCH.y, 6 + rp2 * 13, 0, Math.PI * 2); ctx.stroke();
                }
                ctx.restore();
            }
            drawTapHand(ctx, TAP_TOUCH.x, TAP_TOUCH.y, 8 * (1 - press));
            ctx.restore();
        };
    }

    // phone + finger swiping, content lines scroll with the drag — "swiping to scroll"
    var MSCROLL_X = -15;                          // finger x on the screen
    var MSCROLL_LINES = [26, 14, 22, 10, 18];     // content line lengths
    function scrollPhonePainter() {
        var CYCLE = 2200, DRAG = 22, Y_START = 10, Y_END = -12, WRAP = 50;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE, y, lift, drag;
            if (t < 180)       { y = Y_START; lift = 8 * (1 - easeInOutCubic(t / 180)); drag = 0; }                                        // press down
            else if (t < 900)  { var f = easeInOutCubic((t - 180) / 720); y = Y_START + (Y_END - Y_START) * f; lift = 0; drag = DRAG * f; } // drag up
            else if (t < 1150) { y = Y_END; lift = 8 * easeInOutCubic((t - 900) / 250); drag = DRAG; }                                     // lift off
            else if (t < 1600) { y = Y_END + (Y_START - Y_END) * easeInOutCubic((t - 1150) / 450); lift = 8; drag = DRAG; }                // move back down
            else               { y = Y_START; lift = 8; drag = DRAG; }                                                                     // rest
            var total = Math.floor(now / CYCLE) * DRAG + drag;    // scroll accumulates across cycles
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.translate(TAP_RECENTER, 0);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawTapPhone(ctx);
            ctx.save();                            // content lines, clipped to the screen, wrapping as they scroll
            ctx.beginPath(); ctx.rect(-30, -19, 31, 44); ctx.clip();
            ctx.lineWidth = 2.5; ctx.lineCap = 'round';
            for (var i = 0; i < MSCROLL_LINES.length; i++) {
                var ly = -19 + ((i * 10 + 4 - total) % WRAP + WRAP) % WRAP;
                ctx.beginPath(); ctx.moveTo(-30, ly); ctx.lineTo(-30 + MSCROLL_LINES[i], ly); ctx.stroke();
            }
            ctx.restore();
            drawTapHand(ctx, MSCROLL_X, y, lift);
            ctx.restore();
        };
    }

    // centered phone rotates to landscape, then a media sign blinks on screen — video player
    function drawRotPhone(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3.5;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-21, -32, 42, 64, 8); else ctx.rect(-21, -32, 42, 64);
        ctx.stroke();
        ctx.lineWidth = 2.5;
        ctx.beginPath(); ctx.moveTo(-7, -25); ctx.lineTo(7, -25); ctx.stroke();   // speaker slit
    }
    function drawPlaySign(ctx) {
        ctx.lineWidth = 3; ctx.lineJoin = 'round'; ctx.lineCap = 'round';
        ctx.beginPath();
        ctx.moveTo(-5.5, -9.5); ctx.lineTo(10.5, 0); ctx.lineTo(-5.5, 9.5);
        ctx.closePath();
        ctx.fill(); ctx.stroke();
    }
    function drawPauseSign(ctx) {
        ctx.beginPath();
        if (ctx.roundRect) { ctx.roundRect(-9.5, -9.5, 6.5, 19, 3); ctx.roundRect(3, -9.5, 6.5, 19, 3); }
        else { ctx.rect(-9.5, -9.5, 6.5, 19); ctx.rect(3, -9.5, 6.5, 19); }
        ctx.fill();
    }
    function drawCrossSign(ctx) {
        ctx.lineWidth = 5; ctx.lineCap = 'round';
        ctx.beginPath(); ctx.moveTo(-8, -8); ctx.lineTo(8, 8); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(8, -8); ctx.lineTo(-8, 8); ctx.stroke();
    }
    function drawTimelineSign(ctx, p) {
        var x0 = -23, x1 = 23, ty = 12, hx = x0 + (x1 - x0) * p;
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        // profile avatar (dp) in the center: circle with head + shoulders silhouette
        var R = 11, acy = -4;
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.arc(0, acy, R, 0, Math.PI * 2); ctx.stroke();
        ctx.save();
        ctx.beginPath(); ctx.arc(0, acy, R - 1.5, 0, Math.PI * 2); ctx.clip();
        ctx.beginPath(); ctx.arc(0, acy - 3.5, 3.8, 0, Math.PI * 2); ctx.fill();   // head
        ctx.beginPath(); ctx.arc(0, acy + 8.5, 7, 0, Math.PI * 2); ctx.fill();     // shoulders
        ctx.restore();
        // seek track (faint) + elapsed portion + playhead knob
        ctx.save();
        ctx.globalAlpha *= 0.35;
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(x0, ty); ctx.lineTo(x1, ty); ctx.stroke();
        ctx.restore();
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(x0, ty); ctx.lineTo(hx, ty); ctx.stroke();
        ctx.beginPath(); ctx.arc(hx, ty, 3.5, 0, Math.PI * 2); ctx.fill();
    }
    function rotatePhonePainter(drawSign, sustain) {
        var CYCLE = 3600, ROT_S = 500, ROT_E = 1100, HOLD_E = 3100, BLINK = 500;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var rot;                                  // 0 = portrait, 1 = landscape
            if (t < ROT_S) rot = 0;                                                       // portrait hold
            else if (t < ROT_E) rot = easeInOutCubic((t - ROT_S) / (ROT_E - ROT_S));      // turn to landscape
            else if (t < HOLD_E) rot = 1;                                                 // landscape hold, sign blinks
            else rot = 1 - easeInOutCubic((t - HOLD_E) / (CYCLE - HOLD_E));               // turn back
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.save();
            ctx.rotate(-rot * Math.PI / 2);
            drawRotPhone(ctx);
            ctx.restore();
            if (t >= ROT_E && t < HOLD_E) {           // sign shows while landscape
                var p = (t - ROT_E) / (HOLD_E - ROT_E);
                var vis = sustain
                    ? Math.min(1, p / 0.08) * Math.min(1, (1 - p) / 0.10)             // fade in, hold, fade out
                    : 0.5 - 0.5 * Math.cos(((t - ROT_E) / BLINK) * Math.PI * 2);      // blink pulses
                if (vis > 0.01) {
                    ctx.globalAlpha = alpha * 0.92 * Math.min(1, vis);
                    drawSign(ctx, p);
                }
            }
            ctx.restore();
        };
    }

    // vault with a spinning combination dial; once the code lands, the door swings open — "unlocking the vault"
    function drawVaultDoor(ctx, dialA) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-26, -23, 52, 46, 5); else ctx.rect(-26, -23, 52, 46);
        ctx.stroke();
        [[-20, -17], [20, -17], [-20, 17], [20, 17]].forEach(function (rv) {   // corner rivets
            ctx.beginPath(); ctx.arc(rv[0], rv[1], 1.8, 0, Math.PI * 2); ctx.fill();
        });
        ctx.lineWidth = 2.5;                 // fixed index mark above the dial
        ctx.beginPath(); ctx.moveTo(0, -17); ctx.lineTo(0, -13.5); ctx.stroke();
        ctx.lineWidth = 3;                   // dial rim
        ctx.beginPath(); ctx.arc(0, 0, 12, 0, Math.PI * 2); ctx.stroke();
        ctx.save();                          // rotating dial face
        ctx.rotate(dialA);
        ctx.lineWidth = 2;
        for (var i = 0; i < 8; i++) {
            var a = i * Math.PI / 4 + Math.PI / 8;
            ctx.beginPath();
            ctx.moveTo(Math.cos(a) * 8.5, Math.sin(a) * 8.5);
            ctx.lineTo(Math.cos(a) * 11, Math.sin(a) * 11);
            ctx.stroke();
        }
        ctx.lineWidth = 2.5;                 // indicator spoke
        ctx.beginPath(); ctx.moveTo(0, -3); ctx.lineTo(0, -11); ctx.stroke();
        ctx.restore();
        ctx.beginPath(); ctx.arc(0, 0, 2.5, 0, Math.PI * 2); ctx.fill();
    }
    function vaultPainter() {
        var CYCLE = 4000;
        function seg(t, s, e) { if (t <= s) return 0; if (t >= e) return 1; return easeInOutCubic((t - s) / (e - s)); }
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var dialA = 1.2 * Math.PI * seg(t, 0, 700)        // spin right…
                      - 0.8 * Math.PI * seg(t, 750, 1300)     // …back left…
                      + 0.5 * Math.PI * seg(t, 1350, 1750);   // …right to the last number
            var open;                                          // 0 shut, 1 fully open
            if (t < 1950) open = 0;
            else if (t < 2500) open = easeInOutCubic((t - 1950) / 550);
            else if (t < 3300) open = 1;
            else if (t < 3800) open = 1 - easeInOutCubic((t - 3300) / 500);
            else open = 0;
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';
            ctx.lineWidth = 3.5;                 // vault body
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-33, -30, 66, 60, 8); else ctx.rect(-33, -30, 66, 60);
            ctx.stroke();
            ctx.beginPath();                     // feet
            if (ctx.roundRect) { ctx.roundRect(-27, 30, 10, 5, 2); ctx.roundRect(17, 30, 10, 5, 2); }
            else { ctx.rect(-27, 30, 10, 5); ctx.rect(17, 30, 10, 5); }
            ctx.fill();
            var A = open * 1.0;                  // door swings on its left hinge
            ctx.save();
            ctx.translate(-26, 0);
            ctx.transform(Math.cos(A), -Math.sin(A) * 0.35, 0, 1, 0, 0);
            ctx.translate(26, 0);
            drawVaultDoor(ctx, dialA);
            ctx.restore();
            ctx.restore();
        };
    }


    // ---- web agent logos -------------------------------------------------
    // The browser has no pointer to borrow from the desktop agents: a page
    // click is a mouse click, not the iOS finger, and a tab is a thing of its
    // own. These seven are drawn for the browser and used only by a web run.

    // click: the left button presses on a rhythm, with a shockwave off the corner
    function mouseClickPainter() {
        var CYCLE = 1400;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var press = (t < 200) ? easeInOutCubic(t / 200)
                      : (t < 480 ? 1 : (t < 750 ? 1 - easeInOutCubic((t - 480) / 270) : 0));

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.lineWidth = 3.5;                                  // mouse body
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-16, -26, 32, 52, [16, 16, 13, 13]);
            else ctx.rect(-16, -26, 32, 52);
            ctx.stroke();

            ctx.beginPath(); ctx.moveTo(-16, -5); ctx.lineTo(16, -5); ctx.stroke();   // button split
            ctx.beginPath(); ctx.moveTo(0, -26); ctx.lineTo(0, -5); ctx.stroke();     // divider

            if (press > 0.05) {                                   // left button, pressed
                ctx.save();
                ctx.globalAlpha = alpha * 0.85 * press;
                ctx.beginPath();
                if (ctx.roundRect) ctx.roundRect(-14.5, -24.5, 13.5, 18.5, [13, 2, 2, 2]);
                else ctx.rect(-14.5, -24.5, 13.5, 18.5);
                ctx.fill();
                ctx.restore();
            }

            ctx.lineWidth = 2.5; ctx.beginPath();                 // scroll wheel
            if (ctx.roundRect) ctx.roundRect(-3, -18, 6, 9, 3); else ctx.rect(-3, -18, 6, 9);
            ctx.stroke();

            if (t > 150 && t < 700) {                             // click shockwave
                var p = (t - 150) / 550;
                ctx.save();
                ctx.lineWidth = 2.4;
                ctx.globalAlpha = alpha * 0.85 * (1 - p);
                ctx.beginPath();
                ctx.arc(-8, -20, 5 + p * 13, -Math.PI * 0.95, -Math.PI * 0.1);
                ctx.stroke();
                ctx.restore();
            }
            ctx.restore();
        };
    }

    // hold_click: the button stays down on the left, a ticking clock counts on the right
    function holdClickPainter() {
        var TICK_STEP = 420;                                      // escapement beat, ms
        return function (ctx, cx, cy, now, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            var mx = -18, my = 0;                                 // LEFT: the held mouse
            ctx.lineWidth = 3.2;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(mx - 13, my - 23, 26, 46, [13, 13, 10, 10]);
            else ctx.rect(mx - 13, my - 23, 26, 46);
            ctx.stroke();

            ctx.beginPath(); ctx.moveTo(mx - 13, my - 4); ctx.lineTo(mx + 13, my - 4); ctx.stroke();
            ctx.beginPath(); ctx.moveTo(mx, my - 23); ctx.lineTo(mx, my - 4); ctx.stroke();

            var holdPulse = 0.78 + 0.22 * Math.sin(now / 180);    // held button breathes
            ctx.save();
            ctx.globalAlpha = alpha * holdPulse;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(mx - 11.5, my - 21.5, 10.5, 16.5, [11, 2, 2, 2]);
            else ctx.rect(mx - 11.5, my - 21.5, 10.5, 16.5);
            ctx.fill();
            ctx.restore();

            ctx.lineWidth = 2.2; ctx.beginPath();                 // scroll wheel
            if (ctx.roundRect) ctx.roundRect(mx - 2.5, my - 16, 5, 8, 2.5); else ctx.rect(mx - 2.5, my - 16, 5, 8);
            ctx.stroke();

            var holdWave = (now % 1000) / 1000;                   // pressure radiating
            ctx.save();
            ctx.lineWidth = 2.2;
            ctx.globalAlpha = alpha * 0.85 * (1 - holdWave);
            ctx.beginPath();
            ctx.arc(mx - 6, my - 16, 4 + holdWave * 9, -Math.PI * 0.9, -Math.PI * 0.15);
            ctx.stroke();
            ctx.restore();

            var kx = 18, ky = 0, R = 17;                          // RIGHT: the clock
            ctx.lineWidth = 3.2;
            ctx.beginPath(); ctx.arc(kx, ky, R, 0, Math.PI * 2); ctx.stroke();

            ctx.lineWidth = 2;
            for (var i = 0; i < 12; i++) {
                var a = i * (Math.PI / 6);
                var r1 = (i % 3 === 0) ? 12 : 13.8;
                ctx.beginPath();
                ctx.moveTo(kx + Math.cos(a) * r1, ky + Math.sin(a) * r1);
                ctx.lineTo(kx + Math.cos(a) * 15.5, ky + Math.sin(a) * 15.5);
                ctx.stroke();
            }

            var hourAng = -Math.PI * 0.65;                        // hour hand, parked
            ctx.lineWidth = 3.4;
            ctx.beginPath();
            ctx.moveTo(kx, ky);
            ctx.lineTo(kx + Math.cos(hourAng) * 8.5, ky + Math.sin(hourAng) * 8.5);
            ctx.stroke();

            // second hand: snaps past the mark on each beat, then settles back
            var tickIdx = Math.floor(now / TICK_STEP);
            var tickFrac = (now % TICK_STEP) / TICK_STEP;
            var snap = tickFrac < 0.22 ? 1 + 0.16 * Math.sin((tickFrac / 0.22) * Math.PI) : 1;
            var handAng = (tickIdx - 1 + snap) * (Math.PI / 6) - Math.PI / 2;
            ctx.lineWidth = 2.4;
            ctx.beginPath();
            ctx.moveTo(kx - Math.cos(handAng) * 3, ky - Math.sin(handAng) * 3);
            ctx.lineTo(kx + Math.cos(handAng) * 12.2, ky + Math.sin(handAng) * 12.2);
            ctx.stroke();

            ctx.beginPath(); ctx.arc(kx, ky, 2.4, 0, Math.PI * 2); ctx.fill();   // hub
            ctx.restore();
        };
    }

    // new_tab: a "+" on the tab's corner presses in, and a ripple leaves it
    function newTabPainter() {
        var CYCLE = 1900;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var press = 0;
            if (t > 200 && t < 420) press = easeInOutCubic((t - 200) / 220);
            else if (t >= 420 && t < 650) press = 1 - easeInOutCubic((t - 420) / 230);
            var bScale = 1.0 - 0.14 * press;

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.lineWidth = 3.5;                                  // the tab
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-36, -20, 60, 46, 6); else ctx.rect(-36, -20, 60, 46);
            ctx.stroke();

            ctx.lineWidth = 2.8;                                  // page content
            ctx.beginPath(); ctx.moveTo(-26, -7); ctx.lineTo(8, -7);  ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-26, 3);  ctx.lineTo(14, 3);  ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-26, 13); ctx.lineTo(-2, 13); ctx.stroke();

            var bx = 24, by = -20;
            ctx.save();                                           // clear behind the glyph
            ctx.globalCompositeOperation = 'destination-out';
            ctx.globalAlpha = 1;
            ctx.beginPath(); ctx.arc(bx, by, 13, 0, Math.PI * 2); ctx.fill();
            ctx.restore();

            ctx.save();
            ctx.translate(bx, by);
            ctx.scale(bScale, bScale);
            if (press > 0.05) {                                   // soft highlight
                ctx.save();
                ctx.globalAlpha = alpha * 0.20 * press;
                ctx.beginPath(); ctx.arc(0, 0, 12, 0, Math.PI * 2); ctx.fill();
                ctx.restore();
            }
            ctx.lineWidth = 4.5;                                  // the "+"
            ctx.beginPath();
            ctx.moveTo(-9.5, 0); ctx.lineTo(9.5, 0);
            ctx.moveTo(0, -9.5); ctx.lineTo(0, 9.5);
            ctx.stroke();
            ctx.restore();

            if (t > 380 && t < 950) {                             // ripple off the "+"
                var rp = (t - 380) / 570;
                ctx.save();
                ctx.lineWidth = 2.2;
                ctx.globalAlpha = alpha * 0.85 * (1 - rp);
                ctx.beginPath(); ctx.arc(bx, by, 13 + rp * 5, 0, Math.PI * 2); ctx.stroke();
                ctx.restore();
            }
            ctx.restore();
        };
    }

    // switch_tab: two tabs, the active fill hopping from one to the other
    function switchTabPainter() {
        var CYCLE = 2600, HOLD = 800, MOVE = 500;
        var LEFT = -18.5, RIGHT = 18.5;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE, p;
            if (t < HOLD) p = 0;
            else if (t < HOLD + MOVE) p = easeInOutCubic((t - HOLD) / MOVE);
            else if (t < 2 * HOLD + MOVE) p = 1;
            else p = 1 - easeInOutCubic((t - (2 * HOLD + MOVE)) / MOVE);

            var ax = LEFT + (RIGHT - LEFT) * p;
            var s = 1 - 0.22 * Math.sin(Math.PI * p);             // squeezes while it travels

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.lineWidth = 3.2;                                  // the two tabs
            [LEFT, RIGHT].forEach(function (sx) {
                ctx.beginPath();
                if (ctx.roundRect) ctx.roundRect(sx - 15.5, -17, 31, 34, 5); else ctx.rect(sx - 15.5, -17, 31, 34);
                ctx.stroke();
            });

            var hw = 11.5 * s, hh = 13 * s;                       // the active one
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(ax - hw, -hh, hw * 2, hh * 2, 3.5); else ctx.rect(ax - hw, -hh, hw * 2, hh * 2);
            ctx.fill();
            ctx.restore();
        };
    }

    // close_tab: the cross is pressed and the whole tab collapses into it
    function closeTabPainter() {
        var CYCLE = 2400, BX = 24, BY = -20;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var press = 0;
            if (t > 300 && t < 500) press = easeInOutCubic((t - 300) / 200);
            else if (t >= 500 && t < 700) press = 1 - easeInOutCubic((t - 500) / 200);

            var k = 1, fade = 1, c;
            if (t >= 700 && t < 1200)       { c = easeInOutCubic((t - 700) / 500);  k = 1 - c; fade = 1 - c; }
            else if (t >= 1200 && t < 1500) { k = 0; fade = 0; }
            else if (t >= 1500 && t < 2000) { c = easeInOutCubic((t - 1500) / 500); k = c; fade = c; }
            if (fade < 0.02) return;

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92 * fade; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.translate(BX, BY); ctx.scale(k, k); ctx.translate(-BX, -BY);   // shrink toward the cross

            ctx.lineWidth = 3.5;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-36, -20, 60, 46, 6); else ctx.rect(-36, -20, 60, 46);
            ctx.stroke();

            ctx.lineWidth = 2.8;
            ctx.beginPath(); ctx.moveTo(-26, -7); ctx.lineTo(8, -7);  ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-26, 3);  ctx.lineTo(14, 3);  ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-26, 13); ctx.lineTo(-2, 13); ctx.stroke();

            ctx.save();                                           // clear behind the glyph
            ctx.globalCompositeOperation = 'destination-out';
            ctx.globalAlpha = 1;
            ctx.beginPath(); ctx.arc(BX, BY, 13, 0, Math.PI * 2); ctx.fill();
            ctx.restore();

            ctx.save();                                           // the cross
            ctx.translate(BX, BY);
            ctx.scale(1 - 0.16 * press, 1 - 0.16 * press);
            ctx.lineWidth = 4.5;
            ctx.beginPath();
            ctx.moveTo(-6.5, -6.5); ctx.lineTo(6.5, 6.5);
            ctx.moveTo(6.5, -6.5);  ctx.lineTo(-6.5, 6.5);
            ctx.stroke();
            ctx.restore();
            ctx.restore();
        };
    }

    // update_tab: the reload ring turns once and the page dims through it
    function updateTabPainter() {
        var CYCLE = 2000, SPIN = 1100;
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var s = t < SPIN ? easeInOutCubic(t / SPIN) : 1;
            var dim = t < SPIN ? 0.22 + 0.78 * Math.abs(1 - 2 * (t / SPIN)) : 1;

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.lineWidth = 3.5;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-32, -24, 64, 48, 6); else ctx.rect(-32, -24, 64, 48);
            ctx.stroke();

            ctx.save();                                           // page dims as it reloads
            ctx.globalAlpha *= dim;
            ctx.lineWidth = 2.8;
            ctx.beginPath(); ctx.moveTo(-23, -11); ctx.lineTo(11, -11); ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-23, 0);   ctx.lineTo(17, 0);   ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-23, 11);  ctx.lineTo(1, 11);   ctx.stroke();
            ctx.restore();

            ctx.save();                                           // clear a disc for the ring
            ctx.globalCompositeOperation = 'destination-out';
            ctx.globalAlpha = 1;
            ctx.beginPath(); ctx.arc(0, 0, 16.5, 0, Math.PI * 2); ctx.fill();
            ctx.restore();

            ctx.save();                                           // the reload ring
            ctx.rotate(s * Math.PI * 2);
            var a1 = Math.PI * 0.15, a2 = Math.PI * 1.85;
            ctx.lineWidth = 4;
            ctx.beginPath(); ctx.arc(0, 0, 11, a1, a2); ctx.stroke();
            var px = Math.cos(a2) * 11, py = Math.sin(a2) * 11;
            var dx = -Math.sin(a2), dy = Math.cos(a2);
            ctx.beginPath();
            ctx.moveTo(px + dx * 8, py + dy * 8);
            ctx.lineTo(px - dy * 5.5, py + dx * 5.5);
            ctx.lineTo(px + dy * 5.5, py - dx * 5.5);
            ctx.closePath(); ctx.fill();
            ctx.restore();
            ctx.restore();
        };
    }

    // navigate_tab: a url fills the address bar, then the page lands row by row
    function navigateTabPainter() {
        var CYCLE = 2800;
        var NAV_ROWS = [[-23, 13, 2], [-23, 19, 11], [-23, 7, 20]];
        return function (ctx, cx, cy, now, alpha) {
            var t = now % CYCLE;
            var type = t < 800 ? easeInOutCubic(t / 800) : 1;
            var go   = t < 1150 ? 0 : (t < 1750 ? easeInOutCubic((t - 1150) / 600) : 1);
            var fade = t < 2500 ? 1 : 1 - (t - 2500) / 300;

            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            ctx.lineWidth = 3.5;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-32, -24, 64, 48, 6); else ctx.rect(-32, -24, 64, 48);
            ctx.stroke();

            ctx.lineWidth = 2.5;                                  // address bar
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-24, -17, 48, 12, 6); else ctx.rect(-24, -17, 48, 12);
            ctx.stroke();

            ctx.save();                                           // the url being entered
            ctx.globalAlpha *= fade;
            ctx.lineWidth = 3;
            ctx.beginPath(); ctx.moveTo(-18, -11); ctx.lineTo(-18 + 28 * type, -11); ctx.stroke();
            ctx.restore();

            if (t > 850 && t < 1250) {                            // "go" flashes once
                ctx.save();
                ctx.globalAlpha *= Math.max(0, 1 - Math.abs((t - 1050) / 200));
                ctx.lineWidth = 3;
                ctx.beginPath(); ctx.moveTo(14, -15); ctx.lineTo(19, -11); ctx.lineTo(14, -7); ctx.stroke();
                ctx.restore();
            }

            ctx.save();                                           // the page landing
            ctx.globalAlpha *= fade;
            ctx.lineWidth = 2.8;
            for (var i = 0; i < NAV_ROWS.length; i++) {
                var g = Math.min(1, Math.max(0, (go - i * 0.18) / 0.55));
                if (g <= 0.02) continue;
                var r = NAV_ROWS[i];
                ctx.beginPath(); ctx.moveTo(r[0], r[2]); ctx.lineTo(r[0] + (r[1] - r[0]) * g, r[2]); ctx.stroke();
            }
            ctx.restore();
            ctx.restore();
        };
    }

    // shape name -> painter factory (call to get a painter instance)
    var SHAPES = {
        screen: function () { return vecPainter(vecScreen); },
        tree:   function () { return vecPainter(vecTree); },
        server: function () { return vecPainter(vecServer); },
        loader: loaderPainter,
        globe:  globePainter,
        composing: composingPainter,                // undulating sash — the "thinking" beat
        agent:  agentPainter,                       // mascot head (blinking)
        apple:  function () { return vecPainter(vecApple); },
        shell:  function () { return vecPainter(vecShell); },
        clock:    function () { return vecPainter(vecClock); },
        keyboard: function () { return vecPainter(vecKeyboard); },
        mouse:    mousePainter,                      // mouse w/ scrolling wheel
        click1:   function () { return cursorPainter(1); },        // single left click
        click2:   function () { return cursorPainter(2); },        // double click
        click3:   function () { return cursorPainter(3); },        // triple click
        clickR:   function () { return cursorPainter(1, true); },  // right click (points right)
        pen:      penPainter,                       // pen taking notes (writing)
        camera:   function () { return vecPainter(vecCamera); },
        todo:     function () { return vecPainter(vecTodo); },  // empty todo list
        todoUpd:  todoUpdatePainter,                // todo ticking off
        dragdrop: dragDropPainter,                  // box glides into drop-zone
        book:     bookPainter,                      // open book / scratchpad (page turns)
        appbox:   function () { return vecPainter(vecAppBox); },  // open app crate
        error:    bangPainter,                      // "!" badge — interrupted / failed
        vault:    vaultPainter,                     // combination dial spins, door opens
        // mobile (iOS) logos
        tap:         tapPainter,                                            // finger tapping the phone
        phoneScroll: scrollPhonePainter,                                    // finger swiping to scroll
        phonePlay:   function () { return rotatePhonePainter(drawPlaySign); },
        phonePause:  function () { return rotatePhonePainter(drawPauseSign); },
        phoneClose:  function () { return rotatePhonePainter(drawCrossSign); },
        phoneCheck:  function () { return rotatePhonePainter(drawTimelineSign, true); },
        // web (browser) logos
        mouseClick:  mouseClickPainter,              // a page click, not the iOS finger
        holdClick:   holdClickPainter,               // button held, clock counting
        newTab:      newTabPainter,
        switchTab:   switchTabPainter,
        closeTab:    closeTabPainter,
        updateTab:   updateTabPainter,               // reload
        navigateTab: navigateTabPainter,             // url typed, page lands
    };

    /* =====================================================================
       CONFIG. STEPS is the LIVE chain, built dynamically as events arrive
       (idle demo, or real agent events via window.toolFlow.onFlow). Each step:
         shape     which logo (a key in SHAPES above)
         label     text shown while the step runs
         doneLabel (optional) text shown once the step is done
         tick      (optional) morph the logo into a ✓ when done
         phase     (optional) opening phase, not counted in "N tools used"
       ===================================================================== */
    var STEPS = [];                                  // the live on-screen chain

    // Idle demo — plays + loops when no agent run is active.
    var DEMO = [
        { shape: 'screen',  label: 'screenshot taken', phase: true },
        { shape: 'tree',    label: 'mapping pixel and element', phase: true },
        { shape: 'server',    label: 'communicating with llm service', phase: true },
        { shape: 'composing', label: 'thinking', phase: true },
        { shape: 'loader',    label: 'understanding context', doneLabel: 'synthesizing finished', tick: true, phase: true },
        { shape: 'click1',  label: 'select' },
        { shape: 'pen',     label: 'typing text' },
        { shape: 'click2',  label: 'double left click' },
        { shape: 'mouse',   label: 'scrolling the screen' },
        { shape: 'keyboard',label: 'pressing hotkeys' },
        { shape: 'globe',   label: 'searching the web' },
        { shape: 'agent',   label: 'sub agent trigger' },
        { shape: 'apple',   label: 'executing applescript' },
        { shape: 'shell',   label: 'executing shell' },
        { shape: 'appbox',  label: 'opening app' },
        { shape: 'camera',  label: 'taking a snapshot' },
        { shape: 'todo',    label: 'created todo' },
        { shape: 'todoUpd', label: 'updated todo' },
        { shape: 'dragdrop',label: 'dragging and dropping files' },
        { shape: 'book',    label: 'saving to agent memory' },
    ];

    // Per-turn opening phases (real run). The wait for the packet is told in two
    // beats: the composing sash ("thinking") holds for COMPOSE_MS, then the loader
    // ("understanding context") spins until the packet lands and ticks over to
    // "synthesizing finished". If the packet beats the composing hold, the rest of
    // the opening is fast-forwarded (see startOpening / markReceived).
    var OPEN = {
        screenshot:  { shape: 'screen',    label: 'screenshot taken', phase: true },
        mapping:     { shape: 'tree',      label: 'mapping pixel and element', phase: true },
        communicate: { shape: 'server',    label: 'communicating with llm service', phase: true },
        composing:   { shape: 'composing', label: 'thinking', phase: true },
        thinking:    { shape: 'loader',    label: 'understanding context', doneLabel: 'synthesizing finished', tick: true, phase: true },
    };

    // Backend action/tool name -> { shape, label }. left_click is split by click count.
    var TOOL_MAP = {
        left_click_1: { shape: 'click1',   label: 'select' },
        left_click_2: { shape: 'click2',   label: 'double left click' },
        left_click_3: { shape: 'click3',   label: 'triple left click' },
        right_click:  { shape: 'clickR',   label: 'right click' },
        input:        { shape: 'pen',      label: 'typing text' },
        typewrite:    { shape: 'pen',      label: 'typing text' },
        hotkey:       { shape: 'keyboard', label: 'pressing hotkeys' },
        scroll:       { shape: 'mouse',    label: 'scrolling the screen' },
        drag_drop:    { shape: 'dragdrop', label: 'dragging and dropping files' },
        screenshot:   { shape: 'camera',   label: 'taking a snapshot' },
        open_app:     { shape: 'appbox',   label: 'opening app' },
        wait:         { shape: 'clock',    label: 'waiting for a while' },
        web:          { shape: 'globe',    label: 'searching the web' },
        sub_agent:    { shape: 'agent',    label: 'sub agent trigger' },
        minion:       { shape: 'agent',    label: 'sub agent trigger' },
        agent_wait:   { shape: 'clock',    label: 'waiting for sub-agents' },
        shell:        { shape: 'shell',    label: 'executing shell' },
        applescript:  { shape: 'apple',    label: 'executing applescript' },
        todo_list:    { shape: 'todo',     label: 'created todo' },
        update_todo:  { shape: 'todoUpd',  label: 'updated todo' },
        scratchpad:   { shape: 'book',     label: 'saving to agent memory' },
        done:         { shape: 'loader',   label: 'done', tick: true },  // reuse the packet tick
        vault:        { shape: 'vault',    label: 'unlocking the vault' },
        // mobile (iOS) action names — the iOS driver sends its flat action types
        click:        { shape: 'tap',      label: 'tapping the screen' },
        // web (browser) tool names — the browser driver's own set, from
        // AutoCua/web/controller/view.rs. `input`, `scroll`, `wait`,
        // `scratchpad`, `todo_list`, `update_todo` and `done` are already above
        // and resolve correctly for a web run; `click` and `scroll` collide with
        // names another agent owns and are re-pointed in toolSpec below.
        keyboard:     { shape: 'keyboard',    label: 'pressing hotkeys' },
        hold_click:   { shape: 'holdClick',   label: 'holding a click' },
        run_script:   { shape: 'shell',       label: 'running a script' },
        new_tab:      { shape: 'newTab',      label: 'opening a tab' },
        switch_tab:   { shape: 'switchTab',   label: 'switching tab' },
        close_tab:    { shape: 'closeTab',    label: 'closing a tab' },
        update_tab:   { shape: 'updateTab',   label: 'reloading the tab' },
        navigate_tab: { shape: 'navigateTab', label: 'going to a url' },
        dialog:       { shape: 'mouseClick',  label: 'answering a popup' },
    };

    // video_player carries its sub-command in `value` (iOS router: play/pause/close/streaming).
    var VIDEO_MAP = {
        play:      { shape: 'phonePlay',  label: 'playing video' },
        pause:     { shape: 'phonePause', label: 'pausing video player' },
        close:     { shape: 'phoneClose', label: 'closing video player' },
        streaming: { shape: 'phoneCheck', label: 'checking content on screen' },
    };

    /* ---------------- per-icon canvas + render ---------------- */
    function setupCanvas(canvas) {
        var ctx = canvas.getContext('2d');
        var dpr = window.devicePixelRatio || 1;
        canvas.width = ICON * dpr; canvas.height = ICON * dpr;
        canvas.style.width = ICON + 'px'; canvas.style.height = ICON + 'px';
        ctx.scale(dpr, dpr);
        return ctx;
    }
    var rafId = null;
    function frame(now) {
        rafId = null;
        // Only the visible tail is re-rendered each frame, so long runs stay cheap
        // (off-screen items are masked out and don't need repainting).
        var start = STEPS.length > RENDER_TAIL ? STEPS.length - RENDER_TAIL : 0;
        for (var i = start; i < STEPS.length; i++) renderIcon(STEPS[i], now);
        rafId = requestAnimationFrame(frame);
    }
    function startEngine() { if (!rafId) rafId = requestAnimationFrame(frame); }

    function renderIcon(step, now) {
        var ctx = step._ctx;
        if (!ctx) return;
        var cx = ICON / 2, cy = ICON / 2;
        ctx.clearRect(0, 0, ICON, ICON);
        if (step.state === 'pending') return;                 // nothing drawn (also hidden via CSS)
        if (step.tick && step.state === 'done') {
            var k = Math.min(1, (now - step._doneAt) / TICK_MS);
            if (k < 1) { step._painter(ctx, cx, cy, now, 1 - k); drawTick(ctx, cx, cy, k); }
            else drawTick(ctx, cx, cy, 1);
        } else {
            step._painter(ctx, cx, cy, now, 1);               // active, or done-without-tick
        }
        ctx.globalAlpha = 1;
    }

    /* ---------------- blazing-fast typewriter ---------------- */
    function typeText(el, text) {
        if (!el) return;
        clearTimeout(el._typeTimer);
        el.textContent = '';
        var i = 0;
        (function tick() {
            if (i > text.length) return;
            el.textContent = text.slice(0, i);
            i += 1;
            el._typeTimer = setTimeout(tick, TYPE_MS);
        })();
    }

    /* ---------------- orchestration (state machine) ---------------- */
    function activate(step) {
        if (!step || !step._item) return;
        step.state = 'active';
        step._doneAt = 0;
        frontier = STEPS.indexOf(step);
        scrollToFrontier(frontier);                           // slide the chain up if it overflows
        step._item.classList.add('line-in');                  // CSS draws the line segment
        clearTimeout(step._revealTimer);
        step._revealTimer = setTimeout(function () {
            step._item.classList.add('icon-in');              // reveal the logo
            typeText(step._word, step.label);
        }, 320);                                              // logo/text appear as the line lands
    }
    function complete(step) {
        if (!step || !step._item) return;
        step.state = 'done';
        step._doneAt = performance.now();                     // drives the tick crossfade
        // Make sure the step is visible even if setDone() is called without a
        // prior setActive() (direct backend driving) — also scroll to it.
        clearTimeout(step._revealTimer);
        frontier = Math.max(frontier, STEPS.indexOf(step));
        scrollToFrontier(frontier);
        step._item.classList.add('line-in', 'icon-in');
        if (step.doneLabel) {
            clearTimeout(step._doneTimer);
            step._doneTimer = setTimeout(function () { typeText(step._word, step.doneLabel); }, TICK_MS * 0.6);
        } else if (step._word && !step._word.textContent) {
            typeText(step._word, step.label);                 // no label yet -> show it now
        }
        // NB: the "N tools used" counter is bumped by the driver (per real tool),
        // not here — opening phases / thinking complete without counting.
    }

    /* ---------------- conveyor scroll ----------------
       The chain grows downward as steps activate. Once it fills the visible box,
       each new step slides the whole tree up one "pitch" (logo + gap) so the top
       logo rises past the top edge and dissolves under the mask. maxVisible is
       MEASURED from the box height, so a taller window simply shows more. */
    function getGap() {
        if (!treeEl) return 12;
        var cs = getComputedStyle(treeEl);
        return parseFloat(cs.rowGap || cs.gap) || 12;     // resolve the clamp() to px
    }
    function scrollToFrontier(index) {
        if (!treeEl || !flowEl) return;
        var gap = getGap();
        var pitch = ICON + gap;                            // ICON === item height
        // N logos occupy EXACTLY N * pitch: the tree's padding-top draws the first
        // connector, then every item adds one gap + one icon. The only other cost is
        // the tree's own top offset — subtracting a gap here (as this used to) threw
        // away a whole row whenever the leftover slack was smaller than one gap.
        var maxVisible = Math.max(1, Math.floor((flowEl.clientHeight - treeEl.offsetTop) / pitch));
        var overflow = Math.max(0, index - (maxVisible - 1));
        treeEl.style.transform = 'translateY(' + (-overflow * pitch) + 'px)';
    }

    /* ---------------- timing ---------------- */
    var GAP_MS = 700, HOLD_MS = 2200, FADE_MS = 450, WORK_MS = 1400;  // idle demo
    var STEP_GAP_MS = 950;     // ~1s between opening phases (real run)
    var COMPOSE_MS = 5000;     // the composing sash's own beat before the loader takes over
    var CATCHUP_MS = 300;      // opening pace once the packet has already landed
    var TOOL_GAP_MS = 340;     // fast cadence between tools (real run)
    var RENDER_TAIL = 12;      // only the last N items are re-rendered each frame

    /* ---------------- driver state ---------------- */
    var flowEl = null, treeEl = null, zoneEl = null, frontier = -1, uid = 0;
    var shimmerWord = null;                              // the newest word (loading shine)
    // The chain stays EMPTY until a real agent run drives it. The demo is opt-in
    // only (window.toolFlow.startTest()) — it never auto-runs on app start.
    var demoOn = false, demoTimer = null;
    // True only between run_start and run_end. onFlow drops everything else while
    // it's false, so a stopped run's trailing events can't leak into a new chat.
    var runActive = false;
    var thinkingStep = null, receivedEarly = false, openingDone = true;
    // openNext advances the opening sequence; markReceived calls it early (and flips
    // fastOpening) when the packet lands before the composing beat is up.
    var openNext = null, fastOpening = false;
    var openTimers = [], toolQ = [], toolTimer = null;
    var todoCreateCount = 0;   // # of create-todo (todo_list) calls this run — 2nd+ shows "overwrote todo"
    // True while the current run is a Mobile-use run — read once at run_start from
    // the agent-mode picker's DOM mirror (same source chat_input.js sends the backend).
    var mobileRun = false;
    function isMobileRun() {
        var mw = document.getElementById('agentModeWrap');
        return !!(mw && mw.dataset.mode === 'mobile');
    }
    // Same trick for the browser agent. Three agents send a tool called `click`
    // and two send `scroll`, so the chain has to know which one is driving it
    // before it can pick the right logo (see toolSpec).
    var webRun = false;
    function isWebRun() {
        var mw = document.getElementById('agentModeWrap');
        return !!(mw && mw.dataset.mode === 'web');
    }

    function clearArr(a) { for (var i = 0; i < a.length; i++) clearTimeout(a[i]); a.length = 0; }

    // The ONE way a step enters the chain: append + build DOM + play its reveal.
    function addStep(spec) {
        var step = { key: 's' + (uid++), shape: spec.shape, label: spec.label, doneLabel: spec.doneLabel, tick: spec.tick };
        STEPS.push(step);
        buildItem(step, treeEl);
        activate(step);
        // the newest item is the "current" one — move the loading shine onto it.
        if (shimmerWord) shimmerWord.classList.remove('shimmer');
        step._word.classList.add('shimmer');
        shimmerWord = step._word;
        return step;
    }

    function clearChain() {
        runActive = false;                 // the run this chain belonged to is over for us
        clearArr(openTimers); clearTimeout(toolTimer); toolTimer = null; toolQ.length = 0;
        clearTimeout(demoTimer);
        for (var i = 0; i < STEPS.length; i++) {
            var s = STEPS[i];
            clearTimeout(s._revealTimer); clearTimeout(s._doneTimer);
            if (s._word) clearTimeout(s._word._typeTimer);
            if (s._item && s._item.parentNode) s._item.parentNode.removeChild(s._item);
        }
        STEPS.length = 0; frontier = -1; thinkingStep = null; receivedEarly = false; openingDone = true; shimmerWord = null;
        openNext = null; fastOpening = false;
        todoCreateCount = 0;
        if (treeEl) {                                        // snap the conveyor back to top, no animation
            treeEl.style.transition = 'none';
            treeEl.style.transform = 'translateY(0)';
            void treeEl.offsetHeight;
            treeEl.style.transition = '';
        }
        window.resetToolCount();
        // Restore the pristine empty state: hide the "Tool response: N tools used"
        // heading until the next run drives the chain again (run_start re-adds it).
        if (zoneEl) zoneEl.classList.remove('flow-started');
    }

    /* ---------------- idle demo (loops when no run is active) ---------------- */
    function runDemo() {
        if (!demoOn) return;
        clearChain();
        if (flowEl) flowEl.style.opacity = '1';
        var i = 0;
        (function next() {
            if (!demoOn) return;
            if (i >= DEMO.length) {                          // all shown -> hold, fade, loop
                demoTimer = setTimeout(function () {
                    if (flowEl) flowEl.style.opacity = '0';
                    demoTimer = setTimeout(runDemo, FADE_MS);
                }, HOLD_MS);
                return;
            }
            var sp = DEMO[i];
            var st = addStep(sp);
            if (!sp.phase) window.bumpToolCount();
            if (sp.tick) setTimeout(function () { complete(st); }, WORK_MS * 0.55);
            i++;
            demoTimer = setTimeout(next, GAP_MS);
        })();
    }
    function stopDemo() { demoOn = false; clearTimeout(demoTimer); }

    /* ---------------- real-run driver (fed by window.toolFlow.onFlow) ----------------
       Opening phases auto-advance ~1s apart; "thinking" holds until the real `received`
       event ticks it ("packet received"); tools queue behind the opening, then play fast. */
    function startOpening(hasImage) {
        clearArr(openTimers);
        openingDone = false; thinkingStep = null; receivedEarly = false; fastOpening = false;
        var specs = [];
        if (hasImage) { specs.push(OPEN.screenshot); specs.push(OPEN.mapping); }
        specs.push(OPEN.communicate);
        specs.push(OPEN.composing);
        specs.push(OPEN.thinking);
        var i = 0;
        function play() {
            var sp = specs[i];
            if (sp === OPEN.thinking) {
                thinkingStep = addStep(sp);                  // stays active (loader spins) until received
                openingDone = true;
                openNext = null;                             // opening's done — nothing left to hurry
                if (receivedEarly) { receivedEarly = false; complete(thinkingStep); thinkingStep = null; }
                playTools();
            } else {
                addStep(sp);
            }
            i++;
            if (i < specs.length) {
                // the composing sash gets its own, longer beat — unless the packet
                // already landed, in which case everything runs at catch-up pace
                var wait = fastOpening ? CATCHUP_MS
                    : (sp === OPEN.composing ? COMPOSE_MS : STEP_GAP_MS);
                openTimers.push(setTimeout(play, wait));
            }
        }
        openNext = play;
        play();
    }
    // received{tools}: the packet arrived — tick the loader over to "synthesizing
    // finished", then play this turn's tools (read from the action block) fast.
    function markReceived(tools) {
        toolQ = (tools || []).slice();
        if (thinkingStep) { complete(thinkingStep); thinkingStep = null; playTools(); return; }
        receivedEarly = true;                                // handled when the loader appears
        // The LLM beat the opening (often well under COMPOSE_MS). Don't sit out the
        // rest of the composing hold — drop to catch-up pace so the chain tracks the
        // real speed instead of lagging seconds behind it.
        if (openNext) {
            fastOpening = true;
            clearArr(openTimers);
            openTimers.push(setTimeout(openNext, CATCHUP_MS));
        }
    }
    function playTools() {
        if (toolTimer || !openingDone || !toolQ.length) return;
        var raw = toolQ.shift();
        var sp = toolSpec(raw);
        var st = addStep(sp);
        window.bumpToolCount();                               // every schema tool counts (incl. done)
        if (sp.tick) setTimeout(function () { complete(st); }, 300);
        if (toolQ.length) toolTimer = setTimeout(function () { toolTimer = null; playTools(); }, TOOL_GAP_MS);
    }
    function toolSpec(p) {
        var name = p.name || '';
        if (name === 'left_click') { var c = p.clicks || 1; name = c >= 3 ? 'left_click_3' : (c === 2 ? 'left_click_2' : 'left_click_1'); }
        // Mobile (iOS) variants: 'scroll' is shared with desktop, so only a mobile
        // run swaps the mouse for the swiping phone; video_player picks its sign
        // from the sub-command.
        if (mobileRun && name === 'scroll') return { shape: 'phoneScroll', label: 'swiping to scroll' };
        // Browser variants. `click` is the sharpest collision: TOOL_MAP.click is
        // the iOS finger, because the iOS driver sends that flat name, and the
        // browser driver sends it too meaning a mouse click on a page. Without
        // this a browser click would draw a phone.
        if (webRun) {
            if (name === 'click') return { shape: 'mouseClick', label: 'clicking' };
            if (name === 'scroll') return { shape: 'mouse', label: 'scrolling the page' };
        }
        if (name === 'video_player') return VIDEO_MAP[p.value] || VIDEO_MAP.streaming;
        // A second (or later) create-todo in the same run overrides the first — same logo, different word.
        if (name === 'todo_list' && ++todoCreateCount > 1) return { shape: 'todo', label: 'overwrote todo' };
        return TOOL_MAP[name] || { shape: 'loader', label: (p.name || 'tool') };
    }
    // A terminal "!" drop — agent interrupted (stop) or a backend/LLM error. Not a tool,
    // so it isn't counted; it just caps the chain with a short explanation.
    function showError(text) {
        clearArr(openTimers); clearTimeout(toolTimer); toolTimer = null; toolQ.length = 0;
        thinkingStep = null; receivedEarly = false; openingDone = true; openNext = null;
        addStep({ shape: 'error', label: text || 'something went wrong' });
    }

    /* ---------------- backend bridge ---------------- */
    window.toolFlow = {
        steps: STEPS,
        // backend (app.py) calls: window.toolFlow.onFlow('<event>', '<json payload>')
        onFlow: function (event, payload) {
            if (!treeEl) return;
            // Events are RUN-SCOPED: anything that arrives once the chain has been
            // torn down (New chat / send, both via reset()) or after run_end belongs
            // to a finished run and is dropped. The driver emits its "!" cap only
            // AFTER the loop exits and cleanup runs, so hitting stop and jumping
            // straight to a new chat used to land that "!" in the fresh chain.
            if (!runActive && event !== 'run_start') return;
            var p = {};
            if (payload) { try { p = JSON.parse(payload); } catch (e) { p = {}; } }
            switch (event) {
                case 'run_start':
                    mobileRun = isMobileRun(); webRun = isWebRun(); stopDemo(); clearChain();
                    runActive = true;                             // after clearChain — it clears the flag
                    if (zoneEl) zoneEl.classList.add('flow-started');
                    break;
                case 'turn':      startOpening(!!p.hasImage); break;
                case 'received':  markReceived(p.tools); break;   // tick + play this turn's tools
                case 'error':     showError(p.text); break;       // "!" drop (stop / backend error)
                case 'run_end':   // run finished: stop the shine on the final item; keep it visible
                    if (shimmerWord) { shimmerWord.classList.remove('shimmer'); shimmerWord = null; }
                    runActive = false;
                    break;
            }
        },
        reset: clearChain,
        stopTest: stopDemo,                                  // back-compat aliases
        startTest: function () { if (!demoOn) { demoOn = true; runDemo(); } },
    };

    /* ---------------- build + mount ---------------- */
    function buildItem(step, tree) {
        var item = document.createElement('div');
        item.className = 'tool-flow-item';
        var canvas = document.createElement('canvas');
        canvas.className = 'tf-icon';
        var word = document.createElement('span');
        word.className = 'tool-flow-word';
        item.appendChild(canvas);
        item.appendChild(word);
        tree.appendChild(item);
        step._item = item;
        step._word = word;
        step._ctx = setupCanvas(canvas);
        step._painter = (SHAPES[step.shape] || SHAPES.loader)();
        step.state = 'pending';
        return item;
    }
    function mount() {
        var grid = document.getElementById('mainGrid');
        if (!grid || grid.querySelector('.bottom-left-zone')) return;
        fetch('container/bottom_left/bottom_left.html')
            .then(function (r) { return r.text(); })
            .then(function (html) {
                if (grid.querySelector('.bottom-left-zone')) return; // guard race
                var holder = document.createElement('div');
                holder.innerHTML = html.trim();
                var zone = holder.querySelector('.bottom-left-zone');
                if (!zone) return;
                grid.appendChild(zone);
                zoneEl = zone;                                // .bottom-left-zone (heading reveal toggles on it)
                flowEl = zone.querySelector('.tool-flow');
                var tree = zone.querySelector('.tool-flow-tree');
                if (tree) {
                    treeEl = tree;
                    startEngine();                            // rAF render loop (renders the live tail)
                    // keep "fill the box, then scroll" correct when the window resizes
                    window.addEventListener('resize', function () { scrollToFrontier(frontier); });
                    // NOTE: no demo here — the chain stays empty until the agent run
                    // drives it via window.toolFlow.onFlow(...).
                }
            })
            .catch(function () { /* non-fatal */ });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', mount);
    } else {
        mount();
    }
})();
