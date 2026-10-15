/*!
 * Thinking Orb, "breathing" state (a face-on ring, slowly morphing).
 * Engine code from thinking-orbs v0.3.2 (libraries.dev/orbs), ported from
 * the React component to plain JS. Geometry, presets, painter and clock are
 * unchanged. AutoCua changes: the theme watchers run only for theme "auto",
 * there is no auto-mount (a component mounts its own canvas, e.g. the chat
 * box's agent strip in chat_input.js), and the labels are ASCII-escaped.
 *
 *   var orb = window.ThinkingOrb(canvas, { size: 20, theme: 'light' });
 *   orb.update({ paused: true });   // holds its frame, no animation loop
 *   orb.destroy();
 *
 * MIT License
 *
 * Copyright (c) 2026 Jakub Antalik
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */
(() => {
  "use strict";

  /* -- engine/core ------------------------------------------------ */

  /** Stable directions on a unit sphere (Fibonacci lattice). */
  function fibDir(i, n) {
    const golden = Math.PI * (3 - Math.sqrt(5));
    const y = 1 - 2 * (i + 0.5) / n;
    const rad = Math.sqrt(1 - y * y);
    const a = i * golden;
    return [rad * Math.cos(a), y, rad * Math.sin(a)];
  }

  /** Shared spin + tilt + orthographic projection. */
  function makeProj(yaw, tilt, cx, cy, scale) {
    const st = Math.sin(tilt);
    const ct = Math.cos(tilt);
    const sy = Math.sin(yaw);
    const cyw = Math.cos(yaw);
    return (x, y, z) => {
      const x1 = x * cyw + z * sy;
      const z1 = -x * sy + z * cyw;
      const y1 = y * ct - z1 * st;
      const z2 = y * st + z1 * ct;
      return [cx + x1 * scale, cy - y1 * scale, z2];
    };
  }

  function inkColor(w, alpha, dark, tint) {
    if (!tint) {
      const g = Math.round((dark ? 1 - w : w) * 255);
      return `rgba(${g},${g},${g},${alpha})`;
    }
    const ramp = (c) => Math.round(dark ? c * (1 - w) : c + (255 - c) * w);
    return `rgba(${ramp(tint.r)},${ramp(tint.g)},${ramp(tint.b)},${alpha})`;
  }

  /** Painter: matte grayscale dots; on dark substrates ink is mirrored so near dots read bright. */
  function paint(ctx, dots, dark, rMin = 0.3, tint) {
    for (const d of dots) {
      const alpha = d.a ?? 1;
      const w = Math.min(1, Math.max(0, d.white));
      ctx.fillStyle = inkColor(w, alpha, dark, tint);
      ctx.beginPath();
      ctx.arc(d.x, d.y, d.r, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  function paintLines(ctx, lines, dark, tint) {
    for (const l of lines) {
      const alpha = l.a ?? 1;
      const w = Math.min(1, Math.max(0, l.white));
      ctx.strokeStyle = inkColor(w, alpha, dark, tint);
      ctx.lineWidth = l.w;
      ctx.beginPath();
      ctx.moveTo(l.x1, l.y1);
      ctx.lineTo(l.x2, l.y2);
      ctx.stroke();
    }
  }

  /** Drop invisible marks, clamp radii to the floor, z-sort far to near. */
  function finalizeFrame(dots, lines, rMin = 0.3) {
    const visible = [];
    for (const d of dots) {
      if ((d.a ?? 1) < 0.02) continue;
      d.r = Math.max(rMin, d.r);
      visible.push(d);
    }
    visible.sort((a, b) => a.z - b.z);
    return { dots: visible, lines: lines.filter((l) => (l.a ?? 1) >= 0.02) };
  }

  function paintFrame(ctx, frame, dark, tint) {
    if (frame.lines.length) paintLines(ctx, frame.lines, dark, tint);
    paint(ctx, frame.dots, dark, 0.3, tint);
  }

  /** Radii were tuned for a 300pt frame; sub-linear scaling keeps small orbs legible. */
  function radiusScale(size, pow) {
    return (size / 300) ** pow;
  }

  /* -- engine/profiles -------------------------------------------- */

  const COUNT_PAIRS = [
    ["latRings", "lonDensity"],
    ["rings", "lonDensity"],
    ["lanes", "segs"]
  ];
  const COUNT_KEYS = ["orbitN", "ghostN", "nodeN", "strandN", "signals"];
  const ICON_DENSITY_KEYS = ["iconD"];
  const RADIUS_KEYS = [
    "rBase", "rDepth", "rActive", "rDot", "ghostR",
    "partR", "partRDepth", "nodeR", "nodeRDepth"
  ];

  function scaleCounts(opts, scale) {
    const out = { ...opts };
    const done = new Set();
    const rt = Math.sqrt(scale);
    for (const [a, b] of COUNT_PAIRS) {
      const va = out[a];
      const vb = out[b];
      if (va != null && vb != null && !done.has(a) && !done.has(b)) {
        out[a] = Math.max(2, Math.round(va * rt));
        out[b] = Math.max(2, Math.round(vb * rt));
        done.add(a);
        done.add(b);
      }
    }
    for (const k of COUNT_KEYS) {
      const v = out[k];
      if (v != null && v !== 0 && !done.has(k)) out[k] = Math.max(1, Math.round(v * scale));
    }
    for (const k of ICON_DENSITY_KEYS) {
      const v = out[k];
      if (v != null) out[k] = Math.max(0.02, v * scale);
    }
    return out;
  }

  function scaleRadii(opts, scale) {
    const out = { ...opts };
    for (const k of RADIUS_KEYS) {
      const v = out[k];
      if (v != null) out[k] = v * scale;
    }
    out.rSizeMul = (out.rSizeMul ?? 1) * scale;
    return out;
  }

  const BASE_PROFILES = {
    // ring shares ribbon's painter; faceOn cancels the camera tilt and moves
    // the undulation onto the radius, and there is no ghost sphere behind it
    ring: { lanes: 5, segs: 88, ghostN: 0, faceOn: 1, rBase: 1.1, rDepth: 1.7, rsPow: 0.6, rMin: 0.3 }
  };

  /* -- presets ---------------------------------------------------- */

  const STATE_TO_MODE = { breathing: "ring" };

  const PRESETS = {
    ring: {
      64: { speed: 3.24,   count: 0.25,   size: 0.956, extra: { spin: 0, bandMul: 3.627,  wobMul: 0.368 } },
      32: { speed: 3.5517, count: 0.0678, size: 1.31,  extra: { spin: 0, bandMul: 3.8265, wobMul: 0.4751 } },
      20: { speed: 3.78,   count: 0.028,  size: 1.622, extra: { spin: 0, bandMul: 3.968,  wobMul: 0.565 } }
    }
  };

  const cache = new Map();
  /** Resolve a (state, size) pair to its mode + fully-scaled draw options. */
  function resolvePreset(state, size) {
    const key = `${state}-${size}`;
    const hit = cache.get(key);
    if (hit) return hit;
    const mode = STATE_TO_MODE[state];
    const preset = PRESETS[mode][size];
    let opts = { ...BASE_PROFILES[mode] };
    if (preset.count !== 1) opts = scaleCounts(opts, preset.count);
    if (preset.size !== 1) opts = scaleRadii(opts, preset.size);
    if (preset.extra) opts = { ...opts, ...preset.extra };
    const resolved = { mode, speed: preset.speed, opts };
    cache.set(key, resolved);
    return resolved;
  }

  /* -- geometry: ribbon / ring (faceOn) --------------------------- */

  const frameRibbon = (size, t, o) => {
    const cx = size / 2;
    const cy = size / 2;
    const R = size / 2 * 0.78;
    const spin = o.spin ?? 1;
    const camTilt = 0.3;
    const pt = makeProj(t * 0.1 * spin, camTilt, cx, cy, 1);
    const rs = radiusScale(size, o.rsPow ?? 0.6);
    const dots = [];
    const ghostN = o.ghostN ?? 150;
    for (let i = 0; i < ghostN; i++) {
      const d = fibDir(i, ghostN);
      const [px, py, z] = pt(d[0] * R, d[1] * R, d[2] * R);
      const depth = (z / R + 1) / 2;
      dots.push({ x: px, y: py, z, r: 0.8 * rs, white: 0.78, a: 0.1 + 0.22 * depth });
    }
    const ya = t * 0.24 * spin;
    const ta = o.faceOn ? -camTilt : 0.55 + 0.3 * Math.sin(t * 0.18) * spin;
    const ux = Math.cos(ya);
    const uy = 0;
    const uz = Math.sin(ya);
    const vx = -uz * Math.sin(ta);
    const vy = Math.cos(ta);
    const vz = ux * Math.sin(ta);
    const nx = uy * vz - uz * vy;
    const ny = uz * vx - ux * vz;
    const nz = ux * vy - uy * vx;
    const wobAmp = 0.23 * (o.wobMul ?? 1);
    const baseR = o.faceOn ? R / (1 + 0.85 * wobAmp) : R;
    const baseLanes = o.lanes ?? 5;
    const segs = o.segs ?? 88;
    const lanes = Math.max(1, Math.round(baseLanes * (o.bandMul ?? 1)));
    for (let w = 0; w < lanes; w++) {
      const laneOff = (w - (lanes - 1) / 2) * 0.075;
      const edge = Math.abs(w - (lanes - 1) / 2) / Math.max(1, (lanes - 1) / 2);
      for (let k = 0; k < segs; k++) {
        const a = k / segs * 2 * Math.PI;
        const wob = (0.16 * Math.sin(a * 3 - t * 1.7 + w * 0.22) + 0.07 * Math.sin(a * 5 + t * 1.1)) * (o.wobMul ?? 1);
        const radial = o.faceOn ? 1 + wob : 1;
        const off = o.faceOn ? laneOff : laneOff + wob;
        const x = ux * Math.cos(a) + vx * Math.sin(a) + nx * off;
        const y = uy * Math.cos(a) + vy * Math.sin(a) + ny * off;
        const z = uz * Math.cos(a) + vz * Math.sin(a) + nz * off;
        const l = Math.sqrt(x * x + y * y + z * z);
        const rr = baseR * radial;
        const [px, py, zr] = pt(x / l * rr, y / l * rr, z / l * rr);
        const depth = (zr / R + 1) / 2;
        dots.push({
          x: px,
          y: py,
          z: zr,
          r: ((o.rBase ?? 1.1) + (o.rDepth ?? 1.7) * depth) * (1 - 0.25 * edge) * rs,
          white: 0.52 - 0.44 * depth + 0.18 * edge,
          a: 0.4 + 0.6 * depth
        });
      }
    }
    return finalizeFrame(dots, [], o.rMin);
  };

  const MODE_FRAMES = { ring: frameRibbon };

  /* -- theme / tint helpers --------------------------------------- */

  function ancestorTheme(el) {
    let node = el;
    while (node) {
      const attr = node.getAttribute("data-theme");
      if (attr === "dark") return true;
      if (attr === "light") return false;
      if (node.classList.contains("dark")) return true;
      if (node.classList.contains("light")) return false;
      node = node.parentElement;
    }
    return null;
  }

  function systemDark() {
    return typeof matchMedia === "undefined" || matchMedia("(prefers-color-scheme: dark)").matches;
  }

  function parseTint(color) {
    if (!color) return undefined;
    const hex = color.trim().match(/^#([0-9a-f]{3}|[0-9a-f]{6})$/i);
    if (hex) {
      let h = hex[1];
      if (h.length === 3) h = h.replace(/./g, (c) => c + c);
      const n = parseInt(h, 16);
      return { r: n >> 16 & 255, g: n >> 8 & 255, b: n & 255 };
    }
    const fn = color.trim().match(/^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)/i);
    if (fn) return { r: Number(fn[1]), g: Number(fn[2]), b: Number(fn[3]) };
    return undefined;
  }

  const LABELS = { breathing: "Thinking…" };

  /* -- ThinkingOrb: plain-JS port of the React component ---------- */
  // options: { state: "breathing", size: 20 | 32 | 64, theme: "dark" | "light" | "auto",
  //            speed: 1, paused: false, color: undefined }
  // returns { update(patch), destroy() }

  function ThinkingOrb(canvas, options = {}) {
    const props = { state: "breathing", size: 20, theme: "auto", speed: 1, paused: false, color: undefined, ...options };
    let dark = true;
    let reduced = false;
    let cleanup = null;

    const applyElementProps = () => {
      canvas.setAttribute("role", "img");
      if (!canvas.hasAttribute("aria-label")) canvas.setAttribute("aria-label", LABELS[props.state]);
      canvas.style.width = props.size + "px";
      canvas.style.height = props.size + "px";
      canvas.style.display = "block";
    };

    // Mirrors the component's draw effect.
    const effect = () => {
      const { state, size, speed, paused, color } = props;
      const dpr = Math.min(2, typeof devicePixelRatio !== "undefined" && devicePixelRatio || 1);
      canvas.width = Math.round(size * dpr);
      canvas.height = Math.round(size * dpr);
      const ctx = canvas.getContext("2d");
      if (!ctx) return null;
      const { mode, speed: baseSpeed, opts } = resolvePreset(state, size);
      const frameFn = MODE_FRAMES[mode];
      const tint = parseTint(color);
      const effSpeed = baseSpeed * speed;
      const frame = (tSec) => {
        ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
        ctx.clearRect(0, 0, size, size);
        paintFrame(ctx, frameFn(size, tSec, opts), dark, tint);
      };
      if (reduced) {
        frame(0.6);
        return null;
      }
      let raf = 0;
      let running = false;
      // One shared clock (performance.now) so every instance stays in phase.
      const loop = () => {
        frame(performance.now() / 1e3 * effSpeed);
        if (running) raf = requestAnimationFrame(loop);
      };
      const start = () => {
        if (running || paused) return;
        running = true;
        raf = requestAnimationFrame(loop);
      };
      const stop = () => {
        running = false;
        cancelAnimationFrame(raf);
      };
      frame(performance.now() / 1e3 * effSpeed);
      let visible = true;
      const io = typeof IntersectionObserver !== "undefined" ? new IntersectionObserver(([entry]) => {
        visible = entry.isIntersecting;
        if (visible && document.visibilityState !== "hidden") start();
        else stop();
      }) : null;
      if (io) io.observe(canvas);
      const onVis = () => {
        if (document.visibilityState === "hidden") stop();
        else if (visible) start();
      };
      document.addEventListener("visibilitychange", onVis);
      if (!io) start();
      return () => {
        stop();
        if (io) io.disconnect();
        document.removeEventListener("visibilitychange", onVis);
      };
    };

    const rerun = () => {
      if (cleanup) cleanup();
      cleanup = effect();
    };

    // Theme: "dark"/"light" pin it; "auto" = ancestor data-theme / .dark / .light, else OS scheme.
    const resolveDark = () => {
      const next = props.theme === "dark" ? true
        : props.theme === "light" ? false
        : (ancestorTheme(canvas) ?? systemDark());
      if (next !== dark) { dark = next; return true; }
      return false;
    };
    const onThemeChange = () => { if (resolveDark()) rerun(); };
    // Watched only for "auto": a pinned theme never changes, and the watcher
    // would see every class change in the page.
    const watchTheme = props.theme === "auto";
    const schemeMq = watchTheme && typeof matchMedia !== "undefined" ? matchMedia("(prefers-color-scheme: dark)") : null;
    const themeMo = watchTheme && typeof MutationObserver !== "undefined" ? new MutationObserver(onThemeChange) : null;
    if (schemeMq) schemeMq.addEventListener("change", onThemeChange);
    if (themeMo) themeMo.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "data-theme"], subtree: true });

    // Reduced motion: a static representative frame (t = 0.6), live-updating.
    const motionMq = typeof matchMedia !== "undefined" ? matchMedia("(prefers-reduced-motion: reduce)") : null;
    const onMotion = (e) => { reduced = e.matches; rerun(); };
    if (motionMq) {
      reduced = motionMq.matches;
      motionMq.addEventListener("change", onMotion);
    }

    applyElementProps();
    resolveDark();
    rerun();

    return {
      update(patch) {
        Object.assign(props, patch);
        applyElementProps();
        resolveDark();
        rerun();
      },
      destroy() {
        if (cleanup) cleanup();
        cleanup = null;
        if (schemeMq) schemeMq.removeEventListener("change", onThemeChange);
        if (themeMo) themeMo.disconnect();
        if (motionMq) motionMq.removeEventListener("change", onMotion);
      }
    };
  }

  window.ThinkingOrb = ThinkingOrb;
})();
