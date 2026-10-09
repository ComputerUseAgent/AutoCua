// Shared canvas icon engine + action-chain factory for the CLI coder card.
//
// Painters are ported from the proven main-agent tool-flow (container/bottom_left/bottom_left.js)
// and from tool_animation.html (grep/glob/view/replace — which bottom_left dropped). This module
// is kept SEPARATE from bottom_left.js so the working main chain is never touched.
//
// API (window.CliToolIcons):
//   createChain(treeEl, {orientation})        -> { addAction(tool)->bool, dispose }
//   createTracker(treeEl, flowEl, zoneEl)     -> { push(text), dispose }  (scratchpad stream)
//   createMascot(canvas, size)                -> { dispose }  (the blinking agent head, big)
// where `tool` = { name: <action type>, arg: <label param> }. Unmapped names (todo/minion/
// scratchpad/exit) are skipped by the chain — they're shown elsewhere.
(function () {
    'use strict';

    var ICON = 24;                       // logo size — same as bottom_left's chain (keep == CSS .cc-chain-icon)
    var COLOR = 'rgba(85, 90, 100, 0.92)';
    var VSCALE = (ICON / 2) / 44;        // source coords (~±44) -> ICON-px icon
    var TYPE_MS = 12;                    // per-char typing
    var TICK_MS = 400;

    function now() { return (window.performance && performance.now) ? performance.now() : Date.now(); }

    /* ============================ painters ============================ */
    // Each painter is (ctx, cx, cy, now, alpha). Static shapes are authored in source coords and
    // centred + scaled by vecPaint; animated ones drive themselves off `now`.

    function vecPaint(ctx, cx, cy, alpha, fn) {
        ctx.save();
        ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
        ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
        fn(ctx); ctx.restore();
    }
    function vecPainter(fn) { return function (ctx, cx, cy, t, alpha) { vecPaint(ctx, cx, cy, alpha, fn); }; }

    // --- shell terminal (window + ">" + 3 output lines) — from bottom_left vecShell ---
    function vecShell(ctx) {
        ctx.lineWidth = 3.5; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-35, -25, 70, 50, 4); else ctx.rect(-35, -25, 70, 50);
        ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-35, -15); ctx.lineTo(35, -15); ctx.stroke();
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(-26, -9); ctx.lineTo(-21, -5); ctx.lineTo(-26, -1); ctx.stroke();
        [[-16, 14, -5], [-26, 20, 5], [-26, 4, 15]].forEach(function (L) {
            ctx.beginPath(); ctx.moveTo(L[0], L[2]); ctx.lineTo(L[1], L[2]); ctx.stroke();
        });
    }

    // --- server rack ("communicating with llm service") — from bottom_left vecServer ---
    function vecServer(ctx) {
        ctx.lineWidth = 4; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-25, -35, 50, 70, 4); else ctx.rect(-25, -35, 50, 70);
        ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-25, -12); ctx.lineTo(25, -12); ctx.moveTo(-25, 12); ctx.lineTo(25, 12); ctx.stroke();
        [-23.5, 0, 23.5].forEach(function (yc) {
            ctx.beginPath(); ctx.arc(-15, yc, 2.5, 0, Math.PI * 2); ctx.fill();
            ctx.beginPath(); ctx.moveTo(-5, yc); ctx.lineTo(15, yc); ctx.stroke();
        });
    }

    // --- view: document + magnifying glass (ported from tool_animation drawViewFileShape) ---
    function vecView(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round'; ctx.lineWidth = 3;
        ctx.beginPath();
        ctx.moveTo(-19, -26); ctx.lineTo(7, -26); ctx.lineTo(18, -15);
        ctx.lineTo(18, 26); ctx.lineTo(-19, 26); ctx.closePath(); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(7, -26); ctx.lineTo(7, -15); ctx.lineTo(18, -15); ctx.stroke();
        ctx.lineWidth = 2.4;
        [-7, 1, 9].forEach(function (y) { ctx.beginPath(); ctx.moveTo(-12, y); ctx.lineTo(8, y); ctx.stroke(); });
        var lx = 10, ly = 14, r = 11;
        ctx.lineWidth = 3; ctx.beginPath(); ctx.arc(lx, ly, r, 0, Math.PI * 2); ctx.stroke();
        var hx = lx + r * Math.cos(Math.PI / 4), hy = ly + r * Math.sin(Math.PI / 4);
        ctx.lineWidth = 4.2; ctx.beginPath(); ctx.moveTo(hx, hy); ctx.lineTo(hx + 9, hy + 9); ctx.stroke();
    }

    // --- grep: gutter + text lines + a highlighted match row (ported from drawGrepShape) ---
    function vecGrep(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(-32, -26); ctx.lineTo(-32, 26); ctx.stroke();   // gutter
        var lines = [[18, -20], [28, -10], [6, 0], [30, 10], [12, 20]];             // [x2, y]
        // faint highlight behind a "match" row (y=-10)
        ctx.save(); ctx.globalAlpha *= 0.4; ctx.lineWidth = 8;
        ctx.beginPath(); ctx.moveTo(-26, -10); ctx.lineTo(28, -10); ctx.stroke(); ctx.restore();
        ctx.lineWidth = 3;
        lines.forEach(function (L) { ctx.beginPath(); ctx.moveTo(-26, L[1]); ctx.lineTo(L[0], L[1]); ctx.stroke(); });
    }

    // --- glob: asterisk over 3 file cards (ported from drawGlobShape) ---
    function vecGlob(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 4;
        var r = 14, scx = 0, scy = -12;
        [Math.PI / 6, Math.PI / 2, 5 * Math.PI / 6].forEach(function (ang) {
            ctx.beginPath();
            ctx.moveTo(scx - Math.cos(ang) * r, scy - Math.sin(ang) * r);
            ctx.lineTo(scx + Math.cos(ang) * r, scy + Math.sin(ang) * r);
            ctx.stroke();
        });
        ctx.lineWidth = 2.6;
        var cards = [[-22, 16], [0, 16], [22, 16]], w = 15, h = 19, fold = 5;
        cards.forEach(function (cd) {
            var x = cd[0], y = cd[1];
            ctx.beginPath();
            ctx.moveTo(x - w / 2, y - h / 2); ctx.lineTo(x + w / 2 - fold, y - h / 2);
            ctx.lineTo(x + w / 2, y - h / 2 + fold); ctx.lineTo(x + w / 2, y + h / 2);
            ctx.lineTo(x - w / 2, y + h / 2); ctx.closePath(); ctx.stroke();
        });
    }

    // --- replace: wide code window + clipped lines (ported from drawReplaceShape, rest state) ---
    function vecReplace(ctx) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round'; ctx.lineWidth = 3;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-38, -20, 76, 40, 5); else ctx.rect(-38, -20, 76, 40);
        ctx.stroke();
        ctx.save();
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(-34, -15, 68, 30, 3); else ctx.rect(-34, -15, 68, 30);
        ctx.clip();
        ctx.lineWidth = 2.8;
        [[-21, 18], [-13, 28], [-5, 8], [3, 24], [11, 14], [19, 30]].forEach(function (L) {
            ctx.beginPath(); ctx.moveTo(-30, L[0]); ctx.lineTo(L[1], L[0]); ctx.stroke();
        });
        ctx.restore();
    }

    // --- web globe — bottom_left's "searching" orb: a dotted lat/long sphere with a
    //     scan meridian sweeping it (same constants; see bottom_left.js for the tuning
    //     notes). Each dot's ink drives its ALPHA over the chain grey, so the near/far
    //     depth read survives in one colour. ---
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

    // --- composing — bottom_left's "thinking" orb: an undulating multi-band dotted sash
    //     over a faint fibonacci scaffold (same constants; see bottom_left.js for the
    //     tuning notes). The opening phase shows it for the thinking beat. ---
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

    // --- flowchart with flowing dashed links — "planning next steps" (tool_icon.html planPainter).
    //     Animated rather than a vecPainter: lineDashOffset is driven off the frame clock. ---
    function planPainter() {
        return function (ctx, cx, cy, t, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            ctx.lineCap = 'round'; ctx.lineJoin = 'round';

            // flowing dashed lines linking the nodes
            ctx.lineWidth = 2.2;
            ctx.setLineDash([4, 4]);
            ctx.lineDashOffset = -t / 40;
            ctx.beginPath();
            ctx.moveTo(0, -10); ctx.lineTo(0, 0);
            ctx.lineTo(-23, 0); ctx.lineTo(-23, 10);
            ctx.stroke();
            ctx.beginPath();
            ctx.moveTo(0, 0); ctx.lineTo(23, 0); ctx.lineTo(23, 10);
            ctx.stroke();
            ctx.setLineDash([]);

            // node boxes: one on top, two below
            ctx.lineWidth = 3;
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-18, -26, 36, 16, 4); else ctx.rect(-18, -26, 36, 16);
            ctx.stroke();
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-36, 10, 26, 16, 4); else ctx.rect(-36, 10, 26, 16);
            ctx.stroke();
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(10, 10, 26, 16, 4); else ctx.rect(10, 10, 26, 16);
            ctx.stroke();

            // abstract task text inside each box
            ctx.lineWidth = 2.2;
            ctx.beginPath(); ctx.moveTo(-9, -18); ctx.lineTo(9, -18); ctx.stroke();
            ctx.beginPath(); ctx.moveTo(-29, 18); ctx.lineTo(-17, 18); ctx.stroke();
            ctx.beginPath(); ctx.moveTo(17, 18); ctx.lineTo(29, 18); ctx.stroke();

            ctx.restore();
        };
    }

    // The stop orb's look (tool_icon.html .stop) — NOT a hard gradient: a lavender body
    // with heavily BLURRED pink/blue accent blobs under a whitish sheen, so it reads
    // soft and pastel. This recreates that on the head with radial gradients (a real
    // canvas blur filter is patchy in WKWebView): hazy pink top-right, hazy blue
    // bottom-left, pale sheen over the middle — clipped to the head, gently breathing.
    function paintWorkingCoat(ctx, t, alpha) {
        ctx.save();
        ctx.clip();                                     // the head path — the haze stays inside
        var breath = (Math.sin(t / 700) + 1) / 2;       // 0..1 slow pulse (like orbPulse)
        var drift = Math.sin(t / 1100) * 4;             // the blobs wander a little
        // wide, low-density gradients with a gaussian-ish falloff — no solid cores, just
        // a wash of colour, like the orb's blur(8px) blobs seen through its white body
        ctx.globalAlpha = alpha * (0.5 + 0.2 * breath);
        var pink = ctx.createRadialGradient(14, -10 + drift, 0, 14, -10 + drift, 36);
        pink.addColorStop(0, 'rgba(255, 0, 115, 0.5)');
        pink.addColorStop(0.45, 'rgba(255, 0, 115, 0.26)');
        pink.addColorStop(1, 'rgba(255, 0, 115, 0)');
        ctx.fillStyle = pink; ctx.fillRect(-30, -30, 60, 60);
        var blue = ctx.createRadialGradient(-12, 14 - drift, 0, -12, 14 - drift, 34);
        blue.addColorStop(0, 'rgba(0, 187, 255, 0.5)');
        blue.addColorStop(0.45, 'rgba(0, 187, 255, 0.26)');
        blue.addColorStop(1, 'rgba(0, 187, 255, 0)');
        ctx.fillStyle = blue; ctx.fillRect(-30, -30, 60, 60);
        // the orb's milky body: a broad white veil over everything, colour only ghosting
        // through it — this is what makes it read "blurred" instead of painted-on
        var sheen = ctx.createRadialGradient(0, -2, 0, 0, -2, 34);
        sheen.addColorStop(0, 'rgba(255, 255, 255, 0.62)');
        sheen.addColorStop(0.6, 'rgba(255, 255, 255, 0.4)');
        sheen.addColorStop(1, 'rgba(255, 255, 255, 0.18)');
        ctx.globalAlpha = alpha;
        ctx.fillStyle = sheen; ctx.fillRect(-30, -30, 60, 60);
        ctx.restore();
    }

    // --- agent mascot head (blinking) — from bottom_left agentPainter ---
    // opts.glow: () => bool. When it returns true the head wears the stop orb's soft
    // colours — used to show the agent is WORKING. A live getter, not a flag, so the
    // same painter instance can be switched at runtime. The chain's small `agent`
    // icon passes nothing and stays grey.
    function agentPainter(opts) {
        opts = opts || {};
        return function (ctx, cx, cy, t, alpha) {
            var working = !!(opts.glow && opts.glow());
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92;
            ctx.fillStyle = working ? '#9292d8' : COLOR;   // the orb's lavender base
            ctx.beginPath();
            if (ctx.roundRect) ctx.roundRect(-30, -30, 60, 60, 13); else ctx.rect(-30, -30, 60, 60);
            ctx.fill();
            if (working) paintWorkingCoat(ctx, t, alpha);

            var bt = t % 1800, sy = 1;
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

    // --- minion mascot: three AI heads, a parent branching to two children — ported from
    //     tool_animaation.html (AINODE / generateMinionData / drawSmallHead) ---
    var MINION_NODE = { left: [-24, 0], h1: [24, -20], h2: [24, 20], half: 12, l1: [-7, -4, 7, -16], l2: [-7, 4, 7, 16] };
    function drawMiniHead(ctx, cx, cy, half, t, alpha) {
        var k = half / 30;
        ctx.globalAlpha = alpha * 0.92; ctx.fillStyle = COLOR;
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(cx - half, cy - half, 2 * half, 2 * half, 13 * k); else ctx.rect(cx - half, cy - half, 2 * half, 2 * half);
        ctx.fill();
        var bt = t % 1800, sy = 1;
        if (bt > 1620) sy = bt < 1710 ? 1 - ((bt - 1620) / 90) * 0.92 : 0.08 + ((bt - 1710) / 90) * 0.92;
        var eyeH = 14 * k * sy, eyeY = cy + (-5 * k) - eyeH / 2, eyeR = Math.min(4 * k, eyeH / 2);
        ctx.globalAlpha = alpha; ctx.fillStyle = '#ffffff';
        ctx.beginPath();
        if (ctx.roundRect) { ctx.roundRect(cx - 12 * k, eyeY, 8 * k, eyeH, eyeR); ctx.roundRect(cx + 4 * k, eyeY, 8 * k, eyeH, eyeR); }
        else { ctx.rect(cx - 12 * k, eyeY, 8 * k, eyeH); ctx.rect(cx + 4 * k, eyeY, 8 * k, eyeH); }
        ctx.fill();
    }
    function minionPainter() {
        return function (ctx, cx, cy, t, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.lineCap = 'round'; ctx.lineWidth = 3;
            ctx.beginPath(); ctx.moveTo(MINION_NODE.l1[0], MINION_NODE.l1[1]); ctx.lineTo(MINION_NODE.l1[2], MINION_NODE.l1[3]); ctx.stroke();
            ctx.beginPath(); ctx.moveTo(MINION_NODE.l2[0], MINION_NODE.l2[1]); ctx.lineTo(MINION_NODE.l2[2], MINION_NODE.l2[3]); ctx.stroke();
            drawMiniHead(ctx, MINION_NODE.left[0], MINION_NODE.left[1], MINION_NODE.half, t, alpha);
            drawMiniHead(ctx, MINION_NODE.h1[0], MINION_NODE.h1[1], MINION_NODE.half, t, alpha);
            drawMiniHead(ctx, MINION_NODE.h2[0], MINION_NODE.h2[1], MINION_NODE.half, t, alpha);
            ctx.restore();
        };
    }

    // --- pen writing (write/insert) — from bottom_left penPainter ---
    var PEN_LINE_REST = -26;
    function drawPenShape(ctx, lineLeftX) {
        ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.lineWidth = 6.5; ctx.beginPath(); ctx.moveTo(22, -20); ctx.lineTo(0, 2); ctx.stroke();
        ctx.lineWidth = 3; ctx.beginPath(); ctx.moveTo(0, 2); ctx.lineTo(-5, 7); ctx.stroke();
        ctx.lineWidth = 3;
        ctx.beginPath(); ctx.moveTo(lineLeftX, 10); ctx.lineTo(-7, 10); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-27, 17); ctx.lineTo(3, 17); ctx.stroke();
        ctx.beginPath(); ctx.moveTo(-27, 24); ctx.lineTo(-11, 24); ctx.stroke();
    }
    function penPainter() {
        return function (ctx, cx, cy, t, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawPenShape(ctx, PEN_LINE_REST + 8 * Math.sin(t / 230));
            ctx.restore();
        };
    }

    // --- open book / scratchpad (page turns) — from bottom_left bookPainter ---
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
        return function (ctx, cx, cy, t, alpha) {
            ctx.save();
            ctx.translate(cx, cy); ctx.scale(VSCALE, VSCALE);
            ctx.globalAlpha = alpha * 0.92; ctx.strokeStyle = COLOR; ctx.fillStyle = COLOR;
            drawBookShape(ctx);
            var ang = (t % 1500) / 1500 * Math.PI;
            if (ang > 0.001) {
                ctx.save();
                ctx.transform(Math.cos(ang), -Math.sin(ang) * 0.75, 0, 1, 0, 0);
                drawBookLeaf(ctx);
                ctx.restore();
            }
            ctx.restore();
        };
    }

    // --- spinner loader (fallback / wait) — from bottom_left loaderPainter ---
    var L_OUTER = 12, L_INNER = 8, L_PER = 14;
    function genLoader() {
        var d = [], i, j, t;
        for (i = 0; i < L_OUTER; i++) { var a = (i / L_OUTER) * Math.PI * 2; for (j = 0; j < L_PER; j++) { t = j / (L_PER - 1); d.push({ type: 'o', angle: a, r: 32 + t * 10 }); } }
        for (i = 0; i < L_INNER; i++) { var a2 = (i / L_INNER) * Math.PI * 2; for (j = 0; j < L_PER; j++) { t = j / (L_PER - 1); d.push({ type: 'i', angle: a2, r: 16 + t * 6 }); } }
        return d;
    }
    function loaderPainter() {
        var loader = genLoader();
        return function (ctx, cx, cy, t, alpha) {
            ctx.fillStyle = COLOR; ctx.globalAlpha = alpha * 0.9;
            for (var i = 0; i < loader.length; i++) {
                var l = loader[i];
                var a = l.angle + (l.type === 'o' ? t * 0.0015 : -t * 0.0025);
                ctx.beginPath();
                ctx.arc(cx + Math.cos(a) * l.r * VSCALE, cy + Math.sin(a) * l.r * VSCALE, 0.6, 0, Math.PI * 2);
                ctx.fill();
            }
        };
    }

    var SHAPES = {
        shell:   function () { return vecPainter(vecShell); },
        server:  function () { return vecPainter(vecServer); },
        view:    function () { return vecPainter(vecView); },
        grep:    function () { return vecPainter(vecGrep); },
        glob:    function () { return vecPainter(vecGlob); },
        replace: function () { return vecPainter(vecReplace); },
        write:   penPainter,
        web:     globePainter,
        book:    bookPainter,
        plan:    planPainter,
        agent:   agentPainter,
        minion:  minionPainter,
        loader:  loaderPainter,
        composing: composingPainter,             // undulating sash — the "thinking" beat
    };

    /* ===================== action name -> {shape, FIXED label} =====================
       Fixed, generic captions like the main agent — NOT the real command / args. */
    var CODER_TOOL_MAP = {
        shell:   { shape: 'shell',   label: 'executed shell' },
        view:    { shape: 'view',    label: 'viewed a file' },
        grep:    { shape: 'grep',    label: 'used grep' },
        glob:    { shape: 'glob',    label: 'used glob' },
        write:   { shape: 'write',   label: 'wrote a file' },
        replace: { shape: 'replace', label: 'edited a file' },
        web:     { shape: 'web',     label: 'searching the web' },
        wait:    { shape: 'loader',  label: 'waiting' },
        minion:  { shape: 'minion',  label: 'dispatched minion' },
        plan:    { shape: 'plan',    label: 'planning next steps' },
    };
    // todo_list / update_todo / scratchpad / exit are intentionally absent — they are surfaced
    // elsewhere in the card (right tracker / not shown) and so are skipped. `minion` IS shown
    // here now (the three-head logo) since the user wants the dispatch represented in the chain.

    // "done" ✓ badge — filled circle + white check (from bottom_left drawTick)
    function drawTick(ctx, cx, cy, alpha) {
        var R = ICON / 2 - 1.5;
        ctx.save(); ctx.translate(cx, cy); ctx.globalAlpha = alpha;
        ctx.fillStyle = COLOR; ctx.beginPath(); ctx.arc(0, 0, R, 0, Math.PI * 2); ctx.fill();
        ctx.strokeStyle = 'rgba(255, 255, 255, 0.95)';
        ctx.lineWidth = 1.6; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
        ctx.beginPath(); ctx.moveTo(-R * 0.42, 0); ctx.lineTo(-R * 0.12, R * 0.32); ctx.lineTo(R * 0.46, -R * 0.32); ctx.stroke();
        ctx.restore();
    }

    /* ============================ engine ============================ */
    function setupCanvas(canvas) {
        var ctx = canvas.getContext('2d');
        var dpr = window.devicePixelRatio || 1;
        canvas.width = ICON * dpr; canvas.height = ICON * dpr;
        canvas.style.width = ICON + 'px'; canvas.style.height = ICON + 'px';
        ctx.scale(dpr, dpr);
        return ctx;
    }

    // one shared rAF loop across every chain + scratch indicator on the page.
    // Each step renders inside try/catch so one bad painter can never blank the whole chain.
    var liveSteps = [];
    var rafId = null;
    function frame(t) {
        rafId = null;
        for (var i = 0; i < liveSteps.length; i++) {
            var s = liveSteps[i];
            try {
                if (s._render) { s._render(t); continue; }
                var ctx = s._ctx; if (!ctx) continue;
                ctx.clearRect(0, 0, ICON, ICON);
                if (s.tick && s.state === 'done') {
                    // crossfade the painter into a ✓ tick (e.g. "thinking" -> "packet received")
                    var k = Math.min(1, (t - s._doneAt) / TICK_MS);
                    if (k < 1) { s._painter(ctx, ICON / 2, ICON / 2, t, 1 - k); drawTick(ctx, ICON / 2, ICON / 2, k); }
                    else { drawTick(ctx, ICON / 2, ICON / 2, 1); }
                } else {
                    s._painter(ctx, ICON / 2, ICON / 2, t, 1);
                }
                ctx.globalAlpha = 1;
            } catch (e) { /* keep the loop alive */ }
        }
        if (liveSteps.length) rafId = requestAnimationFrame(frame);
    }
    function startEngine() { if (!rafId) rafId = requestAnimationFrame(frame); }
    function dropStep(step) { var i = liveSteps.indexOf(step); if (i >= 0) liveSteps.splice(i, 1); }

    // The same blinking agent head as the chain's `agent` icon, but standalone and
    // at any size — the Shell-use terminal shows it under its name. Rides the
    // shared rAF loop through the _render escape hatch, so no second loop starts.
    // agentPainter draws a 60-unit head already scaled by VSCALE, so pre-scale by
    // k about the centre to land it at exactly `size` px.
    // `size` is the CANVAS box; the head is drawn at HEAD_FRAC of it (kept from the
    // old glow-bleed sizing so the head's on-screen size doesn't change).
    var HEAD_FRAC = 0.78;
    function createMascot(canvas, size) {
        size = size || 44;
        var ctx = canvas.getContext('2d');
        var dpr = window.devicePixelRatio || 1;
        canvas.width = size * dpr; canvas.height = size * dpr;
        canvas.style.width = size + 'px'; canvas.style.height = size + 'px';
        ctx.scale(dpr, dpr);
        var glowOn = false;
        var paint = agentPainter({ glow: function () { return glowOn; } });
        var k = (size * HEAD_FRAC / 60) / VSCALE;
        var step = {
            _render: function (t) {
                ctx.clearRect(0, 0, size, size);
                ctx.save();
                ctx.translate(size / 2, size / 2);
                ctx.scale(k, k);
                ctx.translate(-size / 2, -size / 2);
                paint(ctx, size / 2, size / 2, t, 1);
                ctx.restore();
            }
        };
        liveSteps.push(step);
        startEngine();
        return {
            setGlow: function (on) { glowOn = !!on; },
            dispose: function () { dropStep(step); }
        };
    }

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

    /* ============================ chain factory ============================ */
    function createChain(treeEl, opts) {
        opts = opts || {};
        var horizontal = (opts.orientation === 'horizontal');
        var steps = [];
        var shimmerWord = null;

        function scrollToEnd() {
            var box = treeEl.parentNode; if (!box) return;
            try {
                if (horizontal) {
                    var overX = Math.max(0, treeEl.scrollWidth - box.clientWidth);
                    treeEl.style.transform = overX ? 'translateX(' + (-overX) + 'px)' : 'translateX(0)';
                    box.classList.toggle('cc-conveyor', overX > 0);   // fade the left edge only while sliding
                } else {
                    var overY = Math.max(0, treeEl.scrollHeight - box.clientHeight);
                    treeEl.style.transform = overY ? 'translateY(' + (-overY) + 'px)' : 'translateY(0)';
                }
            } catch (e) {}
        }

        function removeStep(step) {
            dropStep(step);
            if (step._word) clearTimeout(step._word._typeTimer);
            clearTimeout(step._revealTimer);
            if (step._item && step._item.parentNode) step._item.parentNode.removeChild(step._item);
        }

        // Low-level: add one link to the chain. spec = { shape, label, tick }. Returns a handle
        // whose complete(doneLabel) marks it done — morphing a `tick` step into a ✓ and (optionally)
        // retyping the label (used for the "thinking" -> "packet received" opening phase).
        function addStep(spec) {
            var step = { shape: spec.shape, label: spec.label || '', tick: !!spec.tick };
            var item = document.createElement('div');
            item.className = 'cc-chain-item';
            var canvas = document.createElement('canvas'); canvas.className = 'cc-chain-icon';
            var word = document.createElement('span'); word.className = 'cc-chain-word';
            item.appendChild(canvas); item.appendChild(word);
            treeEl.appendChild(item);

            step._item = item; step._word = word;
            step._ctx = setupCanvas(canvas);
            step._painter = (SHAPES[step.shape] || SHAPES.loader)();
            step.state = 'active'; step._doneAt = 0;
            steps.push(step);
            liveSteps.push(step); startEngine();

            // reveal: connector + icon (CSS), then type the label
            if (window.requestAnimationFrame) requestAnimationFrame(function () { item.classList.add('in'); });
            else item.classList.add('in');
            step._revealTimer = setTimeout(function () { typeText(word, step.label); }, 240);

            if (shimmerWord) shimmerWord.classList.remove('shimmer');
            word.classList.add('shimmer'); shimmerWord = word;

            scrollToEnd();
            while (steps.length > 40) removeStep(steps.shift());   // memory cap

            return {
                complete: function (doneLabel) {
                    step.state = 'done'; step._doneAt = now();
                    if (doneLabel) { clearTimeout(step._revealTimer); typeText(word, doneLabel); }
                    if (shimmerWord === word) { word.classList.remove('shimmer'); shimmerWord = null; }
                }
            };
        }

        // Map a real tool -> a chain link. Returns true if it was added (for the tool counter);
        // unmapped names (todo/minion/scratchpad/exit) are skipped — shown elsewhere.
        function addAction(tool) {
            if (!tool || !tool.name) return false;
            var map = CODER_TOOL_MAP[tool.name];
            if (!map) return false;
            addStep({ shape: map.shape, label: map.label });   // fixed caption (ignore the real arg)
            return true;
        }

        function dispose() {
            for (var i = 0; i < steps.length; i++) removeStep(steps[i]);
            steps.length = 0; shimmerWord = null;
        }

        return { addStep: addStep, addAction: addAction, dispose: dispose };
    }

    /* ====== "tracking progress" — scratchpad entries streamed char-by-char (top_right style:
       a dot bullet on a connecting line + wrapping typed text, conveyor-scrolled; NO logo). ====== */
    function createTracker(treeEl, flowEl, zoneEl) {
        var TYPE = 5, STEP = 2;            // ~2 chars every 5ms
        var items = [];
        var seeded = false;

        function scrollToEnd() {
            if (!treeEl || !flowEl) return;
            var over = Math.max(0, treeEl.offsetHeight - flowEl.clientHeight);
            treeEl.style.transform = over ? 'translateY(' + (-over) + 'px)' : 'translateY(0)';
        }
        function typeText(el, text) {
            if (!el) return;
            clearTimeout(el._t); el.textContent = '';
            var i = 0;
            (function tick() {
                i = Math.min(i + STEP, text.length);
                el.textContent = text.slice(0, i);
                scrollToEnd();                 // text wraps -> height grew, re-pin the bottom
                if (i >= text.length) return;
                el._t = setTimeout(tick, TYPE);
            })();
        }
        function push(text) {
            text = String(text == null ? '' : text).trim();
            if (!text || !treeEl) return;
            if (zoneEl && !seeded) { zoneEl.classList.add('cc-track-active'); seeded = true; }  // reveal on 1st entry
            var item = document.createElement('div');
            item.className = 'cc-track-item';
            var dot = document.createElement('span'); dot.className = 'cc-track-dot';
            var word = document.createElement('span'); word.className = 'cc-track-word';
            item.appendChild(dot); item.appendChild(word);
            treeEl.appendChild(item);
            items.push(item);
            scrollToEnd();
            item.classList.add('line-in');
            setTimeout(function () { item.classList.add('icon-in'); typeText(word, text); }, 320);
            while (items.length > 60) {        // memory cap
                var old = items.shift();
                var ow = old.querySelector('.cc-track-word'); if (ow) clearTimeout(ow._t);
                if (old.parentNode) old.parentNode.removeChild(old);
            }
        }
        function dispose() {
            for (var i = 0; i < items.length; i++) { var w = items[i].querySelector('.cc-track-word'); if (w) clearTimeout(w._t); }
            items.length = 0;
            if (treeEl) treeEl.innerHTML = '';
        }
        return { push: push, dispose: dispose };
    }

    window.CliToolIcons = { createChain: createChain, createTracker: createTracker, createMascot: createMascot };
})();
