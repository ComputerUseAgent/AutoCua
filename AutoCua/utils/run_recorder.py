#!/usr/bin/env python3

"""Record one agent run as two videos, kept separate so they can be cut
together later:

    AutoCua_data/recordings/<time>_<mode>/screen.mp4   what the agent drives
    AutoCua_data/recordings/<time>_<mode>/app.mp4      the AutoCua window

screen.mp4
  * Computer use: the built-in display, the one the mac agent's get_screen()
    picks (mac/tree/element.py). A CG capture contains neither the pointer
    nor the edge glow (the glow is a sharing-none window), so both are drawn
    in: the lavender arrow that agent_glow shows on Windows, shrinking on a
    click, and the breathing lavender edge glow.
  * Web use: the Chrome window the agent drives, found by the pid in its
    profile's SingletonLock. The app raises itself over Chrome at the start
    of a web run, so the display would mostly show the app. The page already
    carries the web agent's own cursor and glow, so nothing is drawn.
app.mp4: the AutoCua window, grabbed by its own window id, so it stays in
  the video while the agent's apps cover it.

Both are full resolution (Retina on a Mac), 30 fps, H.264, with all four
corners rounded on black. Each video has a capture thread that keeps the
newest frame and a writer thread that stamps it and writes it by the wall
clock: a late capture repeats the last frame, so the video plays at real
speed. Both start at the same instant (a late Chrome is padded with black),
so they line up.

    rec = run_recorder.start(ui_window, "computer", on_done=notify)
    rec.stop(tail=2.0)   # keeps recording `tail` seconds, then finalizes

macOS: capture is CGWindowListCreateImage, the call the agent already makes
for its screenshots, so no new permission is needed; OpenCV encodes.
Linux: nothing is drawn in, because agent_glow's glow and lavender cursor are
real on screen there. screen.mp4 in computer use is the PipeWire screencast
of the agent's own RemoteDesktop portal session (no second consent dialog);
in web use it is Chrome's own screencast over its DevTools port, since on
Wayland one app cannot read another's window. app.mp4 is WebKit's snapshot of
the app's own web view (the page, without the title bar). imageio-ffmpeg
encodes: OpenCV's Linux wheels cannot write H.264.
Windows: nothing is drawn in either, because agent_glow's lavender arrow is
the real system cursor there and its glow is real windows. screen.mp4 in
computer use is the primary display, the one the Windows agent screenshots,
read through DXGI Desktop Duplication with the pointer composited in from
the shape the compositor reports; GDI BitBlt stands in while duplication is
unavailable. In web use it is the Chrome window, found by the pid listening
on the DevTools port. That window and app.mp4 are PrintWindow with
PW_RENDERFULLCONTENT: DWM renders the window's own pixels, WebView2's and
Chrome's GPU content included, while other windows cover it. imageio-ffmpeg
encodes, as on Linux: OpenCV's Windows wheels reach H.264 only through Media
Foundation, at a fixed bit per pixel per frame (about 900 MB a minute of a
2560x1600 screen).
start() and stop() never raise: a recording must not be able to fail a run.
finalize_all() finishes whatever is live when the app quits, so the files are
always playable.
"""

import atexit
import math
import os
import sys
import threading
import time
from datetime import datetime

from AutoCua import _ensure, browser_profile_dir, data_root

FPS = 30
CORNER = 0.022        # corner radius, as a fraction of the short side
CURSOR_PT = 32        # arrow size in points (agent_glow draws a 32-unit box)
CHROME_PORT = 9222    # the web agent's DevTools port; the UI's web agent always uses it

_live = set()         # recorders not finalized yet
_live_lock = threading.Lock()
_exiting = False      # set by finalize_all: no UI calls during teardown


def supported() -> bool:
    """True when this machine can record: macOS with OpenCV, or Linux and
    Windows with imageio-ffmpeg and the ffmpeg it bundles."""
    try:
        import numpy  # noqa: F401
        if sys.platform == "darwin":
            import cv2  # noqa: F401
            return True
        if sys.platform.startswith("linux") or sys.platform == "win32":
            import imageio_ffmpeg
            imageio_ffmpeg.get_ffmpeg_exe()     # raises when the binary is missing
            return True
    except Exception:
        pass
    return False


def start(ui_window, kind, on_done=None):
    """Start recording a run. kind is "computer" or "web". on_done(folder) is
    called once it is finished, with None when nothing was captured. Returns
    the recorder, or None when recording is unsupported or could not start."""
    if not supported():
        return None
    try:
        rec = _Recorder(ui_window, kind, on_done)
        rec.begin()
        return rec
    except Exception as e:
        print(f"[recorder] could not start: {e}")
        return None


# ---------------------------------------------------------------------------
# macOS capture
# ---------------------------------------------------------------------------

def _cg_to_bgr(cg_img):
    """CGImage -> BGR numpy array, or None. Captures come back 32-bit
    little-endian alpha-first (BGRA in memory), with padded rows."""
    import cv2
    import numpy as np
    from Quartz import (CGImageGetWidth, CGImageGetHeight, CGImageGetBytesPerRow,
                        CGImageGetBitmapInfo, CGImageGetAlphaInfo,
                        CGImageGetDataProvider, CGDataProviderCopyData)
    if cg_img is None:
        return None
    w, h = CGImageGetWidth(cg_img), CGImageGetHeight(cg_img)
    if w < 2 or h < 2:
        return None
    little_endian = (CGImageGetBitmapInfo(cg_img) & 0x7000) == 0x2000
    alpha_first = CGImageGetAlphaInfo(cg_img) in (2, 4, 6)
    if not (little_endian and alpha_first):
        return None
    bpr = CGImageGetBytesPerRow(cg_img)
    # Read the CFData in place: bytes() on it leaks the whole buffer every
    # frame under PyObjC. The buffer can run past h * bpr, hence count.
    data = CGDataProviderCopyData(CGImageGetDataProvider(cg_img))
    bgra = np.frombuffer(data, dtype=np.uint8, count=h * bpr).reshape(h, bpr // 4, 4)[:, :w]
    return cv2.cvtColor(bgra, cv2.COLOR_BGRA2BGR)


def _display_rect():
    """Bounds of the display the mac agent drives: the built-in one, else the
    main display (mirrors get_screen in mac/tree/element.py)."""
    from Quartz import (CGGetActiveDisplayList, CGDisplayIsBuiltin,
                        CGDisplayBounds, CGMainDisplayID)
    err, ids, count = CGGetActiveDisplayList(10, None, None)
    if err == 0:
        for did in ids[:count]:
            if CGDisplayIsBuiltin(did):
                return CGDisplayBounds(did)
    return CGDisplayBounds(CGMainDisplayID())


def _grab_display(rect):
    from Quartz import (CGWindowListCreateImage, kCGWindowListOptionOnScreenOnly,
                        kCGNullWindowID, kCGWindowImageDefault)
    return _cg_to_bgr(CGWindowListCreateImage(
        rect, kCGWindowListOptionOnScreenOnly, kCGNullWindowID, kCGWindowImageDefault))


def _grab_window(wid):
    """One window's own pixels, even when other windows cover it. None while
    it is minimized, hidden or on another Space."""
    if not wid:
        return None
    from Quartz import (CGWindowListCreateImage, CGRectNull,
                        kCGWindowListOptionIncludingWindow,
                        kCGWindowImageBoundsIgnoreFraming)
    return _cg_to_bgr(CGWindowListCreateImage(
        CGRectNull, kCGWindowListOptionIncludingWindow, wid,
        kCGWindowImageBoundsIgnoreFraming))


def _chrome_window_id():
    """The web agent's Chrome window: the largest normal window owned by the
    pid in the default profile's SingletonLock ("<host>-<pid>"). The UI always
    runs the web agent on the default profile. None until Chrome is up."""
    try:
        target = os.readlink(browser_profile_dir(None) / "SingletonLock")
        pid = int(target.rsplit("-", 1)[1])
    except Exception:
        return None
    from Quartz import (CGWindowListCopyWindowInfo, kCGWindowListOptionOnScreenOnly,
                        kCGWindowListExcludeDesktopElements, kCGNullWindowID)
    best, best_area = None, 0
    infos = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID) or []
    for w in infos:
        if w.get("kCGWindowOwnerPID") != pid or w.get("kCGWindowLayer", 0) != 0:
            continue
        b = w.get("kCGWindowBounds") or {}
        area = b.get("Width", 0) * b.get("Height", 0)
        if area > best_area:
            best, best_area = w.get("kCGWindowNumber"), area
    return best


def _keep_ui_painting(ui_window, on):
    """WebKit stops repainting a window that macOS reports as fully covered,
    and in computer use the agent's apps cover the AutoCua window for much of
    the run. Occlusion detection is switched off while recording, so app.mp4
    stays live, and back on afterwards. Private WebKit API, so it is only
    called when the web view answers to it. macOS only."""
    if sys.platform != "darwin":
        return
    try:
        from PyObjCTools import AppHelper
        from webview.platforms.cocoa import BrowserView
        view = BrowserView.instances[ui_window.uid].webview
        if view.respondsToSelector_(b"_setWindowOcclusionDetectionEnabled:"):
            AppHelper.callAfter(view._setWindowOcclusionDetectionEnabled_, not on)
    except Exception:
        pass


# ---------------------------------------------------------------------------
# What is drawn on screen.mp4 in computer use
# ---------------------------------------------------------------------------

class _Pointer:
    """agent_glow's lavender arrow at the pointer, cycling its gradient and
    shrinking toward the tip on a click, as it does on Windows. The agent's
    clicks are a down and an up microseconds apart, which no frame would ever
    catch as "held", so a click listener stamps each press and the arrow
    plays the whole press for PRESS_SECONDS after it."""
    PRESS_SECONDS = 0.18   # long enough at 30 fps to reach the full shrink

    def __init__(self, rect, scale):
        import numpy as np
        from AutoCua.utils import agent_glow as g
        self.g, self.rect, self.scale = g, rect, scale
        size = int(round(CURSOR_PT * scale))
        self.hot = g.hotspot_for(size)
        self.sprites = []                       # [zoom level][phase] -> (premultiplied BGR, alpha)
        for zoom in g.CLICK_STEPS:
            row = []
            for i in range(g.FRAMES):
                rgba = np.asarray(g.render_frame(size, i / g.FRAMES, ss=4, zoom=zoom)
                                  .convert("RGBA"), np.float32) / 255.0
                a = rgba[..., 3:4]
                row.append((rgba[..., 2::-1] * a * 255.0, a))
            self.sprites.append(row)
        self.level, self.pressed_at, self.held = 0, -1.0, False
        self.listener = None
        try:
            from pynput import mouse

            def on_click(x, y, button, pressed):
                self.held = pressed
                if pressed:
                    self.pressed_at = time.monotonic()
            self.listener = mouse.Listener(on_click=on_click)
            self.listener.daemon = True
            self.listener.start()
        except Exception:
            self.listener = None                # no click feedback, arrow still drawn

    def close(self):
        if self.listener is not None:
            try:
                self.listener.stop()
            except Exception:
                pass

    def draw(self, frame, now, t0):
        from Quartz import CGEventCreate, CGEventGetLocation
        g = self.g
        pressing = self.held or now - self.pressed_at < self.PRESS_SECONDS
        target = len(g.CLICK_STEPS) - 1 if pressing else 0
        self.level += (target > self.level) - (target < self.level)   # one step a frame, eased
        phase = int((now - t0) / g.CYCLE_SECONDS * g.FRAMES) % g.FRAMES
        prem, a = self.sprites[self.level][phase]

        loc = CGEventGetLocation(CGEventCreate(None))
        x = int((loc.x - self.rect.origin.x) * self.scale) - self.hot[0]
        y = int((loc.y - self.rect.origin.y) * self.scale) - self.hot[1]
        h, w = frame.shape[:2]
        s = prem.shape[0]
        x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + s, w), min(y + s, h)
        if x0 >= x1 or y0 >= y1:
            return
        sx, sy = x0 - x, y0 - y
        region = frame[y0:y1, x0:x1].astype("float32")
        p = prem[sy:sy + y1 - y0, sx:sx + x1 - x0]
        al = a[sy:sy + y1 - y0, sx:sx + x1 - x0]
        frame[y0:y1, x0:x1] = (region * (1.0 - al) + p).clip(0, 255).astype(frame.dtype)


class _Glow:
    """The mac edge glow as agent_glow builds it (same thickness, opacity and
    colours), breathing the way MacScreenGlow does: the whole field's alpha
    follows BREATH_LOW..BREATH_HIGH on a cosine. Only the four edge strips are
    blended; the middle of the screen is never touched."""

    def __init__(self, w, h, scale):
        import numpy as np
        from AutoCua.utils import agent_glow as g
        self.g = g
        short = min(w, h) / scale
        t = max(4, int(round(max(8.0, min(short * g.MAC_GLOW_FRACTION, short / 2.0)) * scale)))
        field = np.asarray(g.build_edge_glow(w, h, t, g.MAC_GLOW_OPACITY).convert("RGBA"))
        a = field[..., 3:4].astype(np.float32) / 255.0
        prem = (field[..., 2::-1].astype(np.float32) * a).astype(np.uint8)
        alpha = np.repeat(field[..., 3:4], 3, axis=2)
        self.strips = []
        for ys, xs in ((slice(0, t), slice(0, w)), (slice(h - t, h), slice(0, w)),
                       (slice(t, h - t), slice(0, t)), (slice(t, h - t), slice(w - t, w))):
            self.strips.append((ys, xs, prem[ys, xs].copy(), alpha[ys, xs].copy()))

    def draw(self, frame, now, t0, fade):
        import cv2
        g = self.g
        k = 0.5 - 0.5 * math.cos(2 * math.pi * (now - t0) / g.BREATH_SECONDS)
        b = (g.BREATH_LOW + (g.BREATH_HIGH - g.BREATH_LOW) * k) * fade
        if b <= 0.0:
            return
        for ys, xs, prem, alpha in self.strips:
            v = frame[ys, xs]
            inv = cv2.bitwise_not(cv2.convertScaleAbs(alpha, alpha=b))
            cv2.multiply(v, inv, dst=v, scale=1 / 255.0)
            cv2.add(v, cv2.convertScaleAbs(prem, alpha=b), dst=v)


def _corner_masks(w, h):
    """Anti-aliased quarter-circle masks for the four corners (black outside)."""
    import numpy as np
    r = max(6, int(round(min(w, h) * CORNER)))
    yy, xx = np.mgrid[0:r, 0:r].astype(np.float32) + 0.5
    tl = np.clip(r - np.hypot(r - xx, r - yy) + 0.5, 0.0, 1.0)[..., None]
    return r, (tl, tl[:, ::-1], tl[::-1, :], tl[::-1, ::-1])


def _round_corners(frame, masks):
    r, (tl, tr, bl, br) = masks
    for ys, xs, m in ((slice(0, r), slice(0, r), tl), (slice(0, r), slice(-r, None), tr),
                      (slice(-r, None), slice(0, r), bl), (slice(-r, None), slice(-r, None), br)):
        frame[ys, xs] = (frame[ys, xs] * m).astype(frame.dtype)


# ---------------------------------------------------------------------------
# One video
# ---------------------------------------------------------------------------

class _Stream:
    """A capture thread keeps the newest frame; a writer thread copies it,
    lets `decorate` draw on it, rounds the corners and writes it FPS times a
    second by the wall clock. The writer opens at the first frame's size; a
    later frame of another size (a resized window) is letterboxed into it."""

    def __init__(self, path, grab, t0, done, decorate=None):
        self.path, self.grab, self.t0, self.done = path, grab, t0, done
        self.decorate = decorate
        self.part = path.with_name(path.stem + ".part.mp4")
        self.latest = None
        self.lock = threading.Lock()
        self.writer = None
        self.saved = False
        self.threads = [threading.Thread(target=self._capture, name=f"rec-grab-{path.stem}", daemon=True),
                        threading.Thread(target=self._write, name=f"rec-write-{path.stem}", daemon=True)]

    def start(self):
        for t in self.threads:
            t.start()

    def join(self, timeout):
        end = time.monotonic() + timeout
        for t in self.threads:
            t.join(max(0.1, end - time.monotonic()))

    def _capture(self):
        while not self.done.is_set():
            began = time.monotonic()
            try:
                img = self.grab()
            except Exception:
                img = None
            if img is not None:
                with self.lock:
                    self.latest = img
            time.sleep(max(0.0, 1.0 / FPS - (time.monotonic() - began)))

    def _open(self, img):
        import numpy as np
        if sys.platform == "darwin":
            import cv2
        h, w = img.shape[:2]
        self.size = (w - w % 2, h - h % 2)               # H.264 wants even sides
        if sys.platform != "darwin":
            self.writer = _FfmpegWriter(self.part, self.size)
        for code in (("avc1", "mp4v") if self.writer is None else ()):   # mp4v if this ffmpeg lacks H.264
            vw = cv2.VideoWriter(str(self.part), cv2.CAP_FFMPEG,
                                 cv2.VideoWriter_fourcc(*code), FPS, self.size)
            if vw.isOpened():
                self.writer = vw
                break
            vw.release()
        if self.writer is None:
            raise RuntimeError("no mp4 encoder available")
        self.masks = _corner_masks(*self.size)
        # Started late (Chrome comes up after the run does): pad with black so
        # both videos begin at the same instant and line up in an editor.
        # Counted as written, since writing the padding itself takes time.
        # Capped at 2 s of writing: an encoder slower than real time would
        # otherwise chase the clock forever; the main loop catches up the rest.
        black = np.zeros((self.size[1], self.size[0], 3), np.uint8)
        n, give_up = 0, time.monotonic() + 2.0
        while (n < int((time.monotonic() - self.t0) * FPS)
               and not self.done.is_set() and time.monotonic() < give_up):
            self.writer.write(black)
            n += 1
        return n

    def _fit(self, img):
        import numpy as np
        w, h = self.size
        ih, iw = img.shape[:2]
        if (iw - iw % 2, ih - ih % 2) == (w, h):
            # Always a copy: the writer draws on it, and the same capture can
            # be written twice (a view would get the glow and arrow twice).
            return img[:h, :w].copy()
        s = min(w / iw, h / ih)
        nw, nh = max(2, int(iw * s)), max(2, int(ih * s))
        out = np.zeros((h, w, 3), np.uint8)
        y, x = (h - nh) // 2, (w - nw) // 2
        out[y:y + nh, x:x + nw] = _resize(img, nw, nh)
        return out

    def _write(self):
        written = None
        try:
            while not self.done.is_set():
                with self.lock:
                    img = self.latest
                now = time.monotonic()
                if img is not None:
                    if self.writer is None:
                        written = self._open(img)
                    frame = self._fit(img)
                    if self.decorate:
                        self.decorate(frame, now)
                    _round_corners(frame, self.masks)
                    due = int((time.monotonic() - self.t0) * FPS) + 1
                    # A late frame repeats to keep real speed; after a long
                    # stall (sleep/wake) the gap is skipped instead.
                    for _ in range(min(max(due - written, 0), FPS * 5)):
                        self.writer.write(frame)
                    written = max(written, due)
                    wake = self.t0 + written / FPS
                else:
                    wake = now + 1.0 / FPS
                time.sleep(max(0.0, wake - time.monotonic()))
        except Exception as e:
            print(f"[recorder] {self.path.name} stopped: {e}")
        finally:
            if self.writer is not None:
                try:
                    self.writer.release()
                    os.replace(self.part, self.path)
                    self.saved = True
                except Exception as e:
                    print(f"[recorder] could not save {self.path.name}: {e}")


# ---------------------------------------------------------------------------
# Linux
# ---------------------------------------------------------------------------

def _bgrx_to_bgr(data, w, h, stride=None):
    """32-bit BGRx / little-endian ARGB32 pixels (a GStreamer BGRx buffer, a
    cairo ImageSurface) -> a BGR numpy array of its own."""
    import numpy as np
    stride = stride or len(data) // h
    px = np.frombuffer(data, dtype=np.uint8, count=h * stride).reshape(h, stride // 4, 4)
    return px[:, :w, :3].copy()


def _resize(img, w, h):
    """cv2.resize where OpenCV is installed (macOS), Pillow elsewhere."""
    try:
        import cv2
        return cv2.resize(img, (w, h), interpolation=cv2.INTER_AREA)
    except ImportError:
        import numpy as np
        from PIL import Image
        rgb = Image.fromarray(img[..., ::-1].copy()).resize((w, h), Image.BILINEAR)
        return np.asarray(rgb)[..., ::-1].copy()


class _FfmpegWriter:
    """cv2.VideoWriter's write/release over imageio-ffmpeg, for Linux and
    Windows: OpenCV's Linux wheels cannot write H.264, and its Windows wheels
    only through Media Foundation at a fixed bitrate, while imageio-ffmpeg's
    bundled ffmpeg has libx264, whose size follows the content (a still
    2560x1600 screen is ~5 MB a minute). It runs as a child process fed
    through a pipe; release() waits for it to finish the mp4."""

    def __init__(self, path, size):
        import imageio_ffmpeg
        self.gen = imageio_ffmpeg.write_frames(
            str(path), size, fps=FPS, codec="libx264", pix_fmt_in="bgr24",
            macro_block_size=2, quality=None,
            output_params=["-preset", "superfast", "-crf", "20"],
            ffmpeg_log_level="error")
        self.gen.send(None)

    def write(self, frame):
        self.gen.send(frame)

    def release(self):
        self.gen.close()


class _PipeWireScreen:
    """screen.mp4 in Linux computer use. The agent's RemoteDesktop portal
    session already runs a screencast of each monitor (input_portal.py), so
    the recorder reads that PipeWire stream through GStreamer instead of
    asking the desktop again. The recorder never opens the consent dialog
    (see PortalInput.pipewire_stream): until the session exists it asks again
    once a second, and the writer pads the start with black. Frames only come
    when the screen changes; the writer repeats the last one in between."""

    def __init__(self):
        self.Gst = self.pipe = self.sink = self.fd = None
        self.failed = self.warned = self.closed = self.got = False
        self.opened = self.next_try = 0.0
        self.pending = None                # prime()'s frame, until grab() hands it on
        self.guard = threading.Lock()      # close() vs a pipeline still being opened

    def prime(self, wait=1.0):
        """Open the stream now and wait up to `wait` s for its first frame, so
        the video opens on a picture, as it does on macOS and Windows. With a
        stored grant the session restores silently in ~0.1 s; the first run
        also pays the controller import the agent needs anyway (~1.3 s). The
        frame is kept for the capture thread: the next one only comes when the
        screen changes. False when there is no session yet (no stored grant:
        the agent's first action builds it and the video starts on black)."""
        end = time.monotonic() + wait
        while time.monotonic() < end and not self.failed:
            img = self.grab()
            if img is not None:
                self.pending = img
                return True
            if self.pipe is None:          # no session yet
                return False
        return False

    def _open(self):
        """True once the pipeline runs; False while there is no session yet."""
        import gi
        gi.require_version("Gst", "1.0")
        from gi.repository import Gst
        from AutoCua.linux.controller.tool.input_portal import get_input
        Gst.init(None)
        stream = get_input().pipewire_stream()
        if not stream:
            return False
        fd, node = stream                  # ours to close: pipewiresrc works on its own copy
        with self.guard:
            if self.closed:                # the recording ended while the session was built
                os.close(fd)
                return False
            try:
                pipe = Gst.parse_launch(
                    f"pipewiresrc fd={fd} path={node} do-timestamp=true ! videoconvert ! "
                    f"video/x-raw,format=BGRx ! appsink name=sink max-buffers=1 drop=true sync=false")
                pipe.set_state(Gst.State.PLAYING)
            except Exception:
                os.close(fd)
                raise
            self.Gst, self.pipe, self.fd = Gst, pipe, fd
            self.sink = pipe.get_by_name("sink")
            self.opened = time.monotonic()
        return True

    def grab(self):
        if self.failed or self.closed:
            return None
        if self.pipe is None:
            now = time.monotonic()
            if now < self.next_try:
                return None
            self.next_try = now + 1.0
            try:
                if not self._open():
                    return None
            except Exception as e:
                print(f"[recorder] no Linux screen stream: {e}")
                self.failed = True
                return None
        sample = self.sink.emit("try-pull-sample", 100_000_000)   # 100 ms, in ns
        if sample is None:
            if self.pending is not None:
                img, self.pending = self.pending, None
                return img
            # A still screen sends nothing, so only a stream that never sent
            # a frame is worth a warning.
            if not self.got and not self.warned and time.monotonic() - self.opened > 3.0:
                self.warned = True
                msg = self.pipe.get_bus().pop_filtered(self.Gst.MessageType.ERROR)
                why = msg.parse_error()[0].message if msg else "no error reported"
                print(f"[recorder] no frame from the screen stream after 3 s ({why})")
            return None
        self.got, self.pending = True, None
        caps = sample.get_caps().get_structure(0)
        buf = sample.get_buffer()
        return _bgrx_to_bgr(buf.extract_dup(0, buf.get_size()),
                            caps.get_value("width"), caps.get_value("height"))

    def close(self):
        with self.guard:
            self.closed = True
            if self.pipe is not None:
                self.pipe.set_state(self.Gst.State.NULL)
                self.pipe = None
            if self.fd is not None:
                try:
                    os.close(self.fd)
                except OSError:
                    pass
                self.fd = None


class _Screencast:
    """screen.mp4 in Linux web use. Chrome is a native Wayland client there
    and one client cannot read another's window, so Chrome streams the page
    itself over its DevTools port (Page.startScreencast), the port the web
    agent drives it on. Every page is attached; only the tab on show sends
    frames, and the agent shows its tab through the browser's tabs API
    (browser.rs show_tab), so the video follows the agent's tab
    switches. The page already
    carries the web agent's own cursor and glow. Never polls /json/list,
    which stalls under load."""
    PORT = CHROME_PORT

    def __init__(self):
        self.jpeg, self.seq = None, 0
        self.img, self.img_seq = None, -1
        self.lock = threading.Lock()
        self.stopped = threading.Event()
        threading.Thread(target=self._run, name="rec-screencast", daemon=True).start()

    def _run(self):
        import inspect
        import json
        import urllib.request
        try:
            from websockets.sync.client import connect
        except Exception as e:
            print(f"[recorder] no websocket client for Chrome's screencast: {e}")
            return
        # Chrome is on this machine: never go through an http(s)_proxy from
        # the environment (websockets 15+ would, and so would urllib).
        local = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        no_proxy = {"proxy": None} if "proxy" in inspect.signature(connect).parameters else {}
        while not self.stopped.is_set():
            try:
                # Chrome comes up after the recording starts, and can restart.
                with local.open(f"http://127.0.0.1:{self.PORT}/json/version", timeout=2) as r:
                    url = json.load(r)["webSocketDebuggerUrl"]
                with connect(url, max_size=None, compression=None, open_timeout=5,
                             **no_proxy) as ws:
                    self._pump(ws, json)
            except Exception:
                self.stopped.wait(1.0)

    def _pump(self, ws, json):
        n, hidden = 0, set()

        def send(method, params, session=None):
            nonlocal n
            n += 1
            msg = {"id": n, "method": method, "params": params}
            if session:
                msg["sessionId"] = session
            ws.send(json.dumps(msg))

        send("Target.setAutoAttach",
             {"autoAttach": True, "waitForDebuggerOnStart": False, "flatten": True})
        while not self.stopped.is_set():
            try:
                m = json.loads(ws.recv(timeout=0.5))
            except TimeoutError:
                continue
            method, p, sid = m.get("method"), m.get("params") or {}, m.get("sessionId")
            if method == "Target.attachedToTarget":
                if (p.get("targetInfo") or {}).get("type") == "page":
                    send("Page.startScreencast", {"format": "jpeg", "quality": 90},
                         p.get("sessionId"))
            elif method == "Page.screencastFrame":
                send("Page.screencastFrameAck", {"sessionId": p.get("sessionId")}, sid)
                if sid not in hidden:
                    with self.lock:
                        self.jpeg, self.seq = p.get("data"), self.seq + 1
            elif method == "Page.screencastVisibilityChanged":
                (hidden.discard if p.get("visible") else hidden.add)(sid)

    def grab(self):
        with self.lock:
            data, seq = self.jpeg, self.seq
        if data is None:
            return None
        if seq != self.img_seq:            # decode only a new frame
            import base64
            import io
            import numpy as np
            from PIL import Image
            rgb = np.asarray(Image.open(io.BytesIO(base64.b64decode(data))).convert("RGB"))
            self.img, self.img_seq = rgb[..., ::-1].copy(), seq
        return self.img

    def close(self):
        self.stopped.set()


class _WebViewSnapshot:
    """app.mp4 on Linux. pywebview's GTK backend shows the page in a
    WebKit2GTK web view, and WebKit hands our own process a snapshot of it,
    covered or not, with no portal. GTK and WebKit belong to the main thread,
    so each snapshot is asked for there and picked up here. The page only:
    the GNOME title bar is not part of it."""

    @staticmethod
    def usable():
        try:
            import gi
            import webview
            gi.require_foreign("cairo")    # python3-gi-cairo: the snapshot comes back as a cairo surface
            return webview.renderer == "gtkwebkit2"   # not when KDE got pywebview's Qt backend
        except Exception as e:
            print(f"[recorder] no app video on this desktop: {e}")
            return False

    def __init__(self, ui_window):
        self.ui_window = ui_window
        self.failed = False

    def grab(self):
        if self.failed:
            return None
        from gi.repository import GLib
        box, ready = {}, threading.Event()

        def done(view, result):
            try:
                box["surface"] = view.get_snapshot_finish(result)
            except Exception as e:
                box["error"] = e
            ready.set()

        def ask():
            try:
                from gi.repository import WebKit2
                from webview.platforms.gtk import BrowserView
                view = BrowserView.instances[self.ui_window.uid].webview
                view.get_snapshot(WebKit2.SnapshotRegion.VISIBLE,
                                  WebKit2.SnapshotOptions.NONE, None, done)
            except Exception as e:
                box["error"] = e
                ready.set()
            return False                   # run once

        GLib.idle_add(ask)
        if not ready.wait(0.5) or "surface" not in box:
            if "error" in box:             # a real failure, not a slow frame: stop asking
                self.failed = True
                print(f"[recorder] no snapshot of the app window: {box['error']}")
            return None
        surface = box["surface"]
        surface.flush()
        return _bgrx_to_bgr(surface.get_data(), surface.get_width(),
                            surface.get_height(), surface.get_stride())

    def close(self):
        pass


# ---------------------------------------------------------------------------
# Windows
# ---------------------------------------------------------------------------

if sys.platform == "win32":
    import ctypes
    from ctypes import wintypes

    # Private handles: argtypes set on the shared ctypes.windll would change
    # them for every other module calling the same functions.
    _user32 = ctypes.WinDLL("user32", use_last_error=True)
    _gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
    _dwmapi = ctypes.WinDLL("dwmapi")

    PW_RENDERFULLCONTENT = 2
    DWMWA_EXTENDED_FRAME_BOUNDS = 9
    SRCCOPY, CAPTUREBLT = 0x00CC0020, 0x40000000
    CURSOR_SHOWING, DI_NORMAL = 0x1, 0x3
    DXGI_ERROR_UNSUPPORTED = 0x887A0004
    DXGI_ERROR_WAIT_TIMEOUT = 0x887A0027

    class _BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG),
                    ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
                    ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
                    ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
                    ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
                    ("biClrImportant", wintypes.DWORD)]

    class _CURSORINFO(ctypes.Structure):
        _fields_ = [("cbSize", wintypes.DWORD), ("flags", wintypes.DWORD),
                    ("hCursor", wintypes.HANDLE), ("ptScreenPos", wintypes.POINT)]

    class _ICONINFO(ctypes.Structure):
        _fields_ = [("fIcon", wintypes.BOOL), ("xHotspot", wintypes.DWORD),
                    ("yHotspot", wintypes.DWORD), ("hbmMask", wintypes.HBITMAP),
                    ("hbmColor", wintypes.HBITMAP)]

    class _GUID(ctypes.Structure):
        _fields_ = [("Data1", ctypes.c_uint32), ("Data2", ctypes.c_uint16),
                    ("Data3", ctypes.c_uint16), ("Data4", ctypes.c_ubyte * 8)]

    def _guid(text):
        p = text.split("-")
        return _GUID(int(p[0], 16), int(p[1], 16), int(p[2], 16),
                     (ctypes.c_ubyte * 8)(*bytes.fromhex(p[3] + p[4])))

    _IID_IDXGIFactory1 = _guid("770aae78-f26f-4dba-a829-253c83d1b387")
    _IID_IDXGIOutput1 = _guid("00cddea8-939b-4b83-a340-a685226666cc")
    _IID_ID3D11Texture2D = _guid("6f15aaf2-d208-4e89-9ab4-489535d34f9c")

    class _DXGI_OUTPUT_DESC(ctypes.Structure):
        _fields_ = [("DeviceName", wintypes.WCHAR * 32), ("DesktopCoordinates", wintypes.RECT),
                    ("AttachedToDesktop", wintypes.BOOL), ("Rotation", ctypes.c_uint),
                    ("Monitor", wintypes.HMONITOR)]

    class _DXGI_OUTDUPL_DESC(ctypes.Structure):      # its DXGI_MODE_DESC flattened in
        _fields_ = [("Width", ctypes.c_uint), ("Height", ctypes.c_uint),
                    ("RefreshNumerator", ctypes.c_uint), ("RefreshDenominator", ctypes.c_uint),
                    ("Format", ctypes.c_uint), ("ScanlineOrdering", ctypes.c_uint),
                    ("Scaling", ctypes.c_uint), ("Rotation", ctypes.c_uint),
                    ("DesktopImageInSystemMemory", wintypes.BOOL)]

    class _D3D11_TEXTURE2D_DESC(ctypes.Structure):   # its DXGI_SAMPLE_DESC flattened in
        _fields_ = [("Width", ctypes.c_uint), ("Height", ctypes.c_uint),
                    ("MipLevels", ctypes.c_uint), ("ArraySize", ctypes.c_uint),
                    ("Format", ctypes.c_uint), ("SampleCount", ctypes.c_uint),
                    ("SampleQuality", ctypes.c_uint), ("Usage", ctypes.c_uint),
                    ("BindFlags", ctypes.c_uint), ("CPUAccessFlags", ctypes.c_uint),
                    ("MiscFlags", ctypes.c_uint)]

    class _DXGI_OUTDUPL_FRAME_INFO(ctypes.Structure):  # its pointer position flattened in
        _fields_ = [("LastPresentTime", ctypes.c_longlong),
                    ("LastMouseUpdateTime", ctypes.c_longlong),
                    ("AccumulatedFrames", ctypes.c_uint),
                    ("RectsCoalesced", wintypes.BOOL),
                    ("ProtectedContentMaskedOut", wintypes.BOOL),
                    ("PointerPosition", wintypes.POINT),
                    ("PointerVisible", wintypes.BOOL),
                    ("TotalMetadataBufferSize", ctypes.c_uint),
                    ("PointerShapeBufferSize", ctypes.c_uint)]

    class _DXGI_OUTDUPL_POINTER_SHAPE_INFO(ctypes.Structure):
        _fields_ = [("Type", ctypes.c_uint), ("Width", ctypes.c_uint),
                    ("Height", ctypes.c_uint), ("Pitch", ctypes.c_uint),
                    ("HotSpot", wintypes.POINT)]

    class _D3D11_MAPPED_SUBRESOURCE(ctypes.Structure):
        _fields_ = [("pData", ctypes.c_void_p), ("RowPitch", ctypes.c_uint),
                    ("DepthPitch", ctypes.c_uint)]

    _ENUMPROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    # restype matters on 64-bit: without it ctypes truncates handles to int
    _user32.GetDC.restype = wintypes.HDC
    _user32.GetDC.argtypes = [wintypes.HWND]
    _user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
    _user32.PrintWindow.argtypes = [wintypes.HWND, wintypes.HDC, wintypes.UINT]
    _user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
    _user32.IsWindowVisible.argtypes = [wintypes.HWND]
    _user32.IsIconic.argtypes = [wintypes.HWND]
    _user32.GetWindowTextLengthW.argtypes = [wintypes.HWND]
    _user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    _user32.EnumWindows.argtypes = [_ENUMPROC, wintypes.LPARAM]
    _user32.GetCursorInfo.argtypes = [ctypes.POINTER(_CURSORINFO)]
    _user32.GetIconInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(_ICONINFO)]
    _user32.DrawIconEx.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int, wintypes.HANDLE,
                                   ctypes.c_int, ctypes.c_int, wintypes.UINT, wintypes.HBRUSH,
                                   wintypes.UINT]
    _gdi32.CreateCompatibleDC.restype = wintypes.HDC
    _gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
    _gdi32.CreateDIBSection.restype = wintypes.HBITMAP
    _gdi32.CreateDIBSection.argtypes = [wintypes.HDC, ctypes.POINTER(_BITMAPINFOHEADER),
                                        wintypes.UINT, ctypes.POINTER(ctypes.c_void_p),
                                        wintypes.HANDLE, wintypes.DWORD]
    _gdi32.SelectObject.restype = wintypes.HGDIOBJ
    _gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
    _gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
    _gdi32.DeleteDC.argtypes = [wintypes.HDC]
    _gdi32.BitBlt.argtypes = [wintypes.HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                              ctypes.c_int, wintypes.HDC, ctypes.c_int, ctypes.c_int,
                              wintypes.DWORD]
    _dwmapi.DwmGetWindowAttribute.argtypes = [wintypes.HWND, wintypes.DWORD,
                                              ctypes.c_void_p, wintypes.DWORD]

    def _com(ptr, index, restype, *argtypes):
        """Method `index` of a COM object's vtable, called with the object first."""
        table = ctypes.cast(ptr, ctypes.POINTER(ctypes.c_void_p)).contents.value
        fn = ctypes.cast(table, ctypes.POINTER(ctypes.c_void_p))[index]
        return ctypes.WINFUNCTYPE(restype, ctypes.c_void_p, *argtypes)(fn)

    def _release(ptr):
        if ptr:
            _com(ptr, 2, ctypes.c_ulong)(ptr)


def _physical_pixels():
    """Make the calling capture thread per-monitor DPI aware, so window rects,
    the screen size and the cursor all come in physical pixels, the units DWM
    and DXGI always use. The thread's setting only: the process's stays as it
    is (agent_glow says why it must not move mid-run)."""
    try:
        _user32.SetThreadDpiAwarenessContext(ctypes.c_void_p(-4))   # PER_MONITOR_AWARE_V2
    except Exception:
        pass                    # before Windows 10 1607: the process's setting stands


def _bgr(bgra, out=None):
    """BGRA -> BGR, into `out` when given: cv2 where OpenCV is installed (a
    4 MP frame in ~2 ms), numpy elsewhere (~10 ms)."""
    try:
        import cv2
        return cv2.cvtColor(bgra, cv2.COLOR_BGRA2BGR, dst=out)
    except ImportError:
        import numpy as np
        if out is None:
            return np.ascontiguousarray(bgra[..., :3])
        out[...] = bgra[..., :3]
        return out


def _ui_hwnd(ui_window):
    """The AutoCua window's handle (pywebview's WinForms form), or 0."""
    try:
        return int(ui_window.native.Handle.ToInt64())
    except Exception:
        return 0


def _chrome_hwnd():
    """The web agent's Chrome window: the largest titled window of the process
    listening on its DevTools port (popups and tooltips have no title). The
    pid lookup takes ~1 ms. None until Chrome is up."""
    try:
        import psutil
        pid = next((c.pid for c in psutil.net_connections(kind="tcp")
                    if c.status == psutil.CONN_LISTEN and c.laddr
                    and c.laddr.port == CHROME_PORT), None)
    except Exception:
        return None
    if not pid:
        return None
    found = []

    def visit(hwnd, _):
        owner = wintypes.DWORD()
        _user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if (owner.value == pid and _user32.IsWindowVisible(hwnd)
                and _user32.GetWindowTextLengthW(hwnd) > 0):
            r = wintypes.RECT()
            _user32.GetWindowRect(hwnd, ctypes.byref(r))
            found.append(((r.right - r.left) * (r.bottom - r.top), hwnd))
        return True
    _user32.EnumWindows(_ENUMPROC(visit), 0)
    return max(found)[1] if found else None


class _Dib:
    """A 32-bit top-down DIB selected into a memory DC, its pixels (BGRA) as a
    numpy view that dies with it in free()."""

    def __init__(self, w, h):
        import numpy as np
        self.size = (w, h)
        screen = _user32.GetDC(None)
        self.dc = _gdi32.CreateCompatibleDC(screen)
        _user32.ReleaseDC(None, screen)
        head = _BITMAPINFOHEADER(ctypes.sizeof(_BITMAPINFOHEADER), w, -h, 1, 32)   # -h: top-down
        bits = ctypes.c_void_p()
        self.bmp = _gdi32.CreateDIBSection(self.dc, ctypes.byref(head), 0,
                                           ctypes.byref(bits), None, 0)
        if not self.bmp:
            _gdi32.DeleteDC(self.dc)
            raise ctypes.WinError(ctypes.get_last_error())
        self.old = _gdi32.SelectObject(self.dc, self.bmp)
        self.px = np.ctypeslib.as_array(ctypes.cast(bits, ctypes.POINTER(ctypes.c_uint8)),
                                        shape=(h, w, 4))

    def free(self):
        self.px = None
        _gdi32.SelectObject(self.dc, self.old)
        _gdi32.DeleteObject(self.bmp)
        _gdi32.DeleteDC(self.dc)


class _WindowGrab:
    """One window's own pixels, even while other windows cover it:
    PrintWindow with PW_RENDERFULLCONTENT has DWM render the window, GPU
    content included (plain PrintWindow leaves a WebView2 page white), in
    ~10 ms. Cropped to the frame DWM draws, without the invisible resize
    borders around it. None while the window is minimized, hidden or gone,
    so the video holds its last frame. find() names the window."""

    def __init__(self, find):
        self.find = find
        self.dib = None
        self.lock = threading.Lock()
        self.closed = self.aware = False

    def grab(self):
        with self.lock:
            if self.closed:
                return None
            if not self.aware:
                _physical_pixels()
                self.aware = True
            hwnd = self.find()
            if not hwnd or not _user32.IsWindowVisible(hwnd) or _user32.IsIconic(hwnd):
                return None
            r, f = wintypes.RECT(), wintypes.RECT()
            if not _user32.GetWindowRect(hwnd, ctypes.byref(r)):
                return None
            if _dwmapi.DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS,
                                             ctypes.byref(f), ctypes.sizeof(f)):
                f = r
            w, h = r.right - r.left, r.bottom - r.top
            if w < 2 or h < 2:
                return None
            if self.dib is None or self.dib.size != (w, h):
                if self.dib is not None:
                    self.dib.free()
                    self.dib = None
                self.dib = _Dib(w, h)
            if not _user32.PrintWindow(hwnd, self.dib.dc, PW_RENDERFULLCONTENT):
                return None
            x0, y0 = max(f.left - r.left, 0), max(f.top - r.top, 0)
            x1, y1 = min(f.right - r.left, w), min(f.bottom - r.top, h)
            if x1 - x0 < 2 or y1 - y0 < 2:
                return None
            return _bgr(self.dib.px[y0:y1, x0:x1])

    def close(self):
        # Never under a grab: that one could still be inside PrintWindow.
        if self.lock.acquire(timeout=1.0):
            try:
                self.closed = True
                if self.dib is not None:
                    self.dib.free()
                    self.dib = None
            finally:
                self.lock.release()


class _Desktop:
    """screen.mp4 in Windows computer use: the primary display, the one the
    Windows agent screenshots. DXGI Desktop Duplication hands over the frame
    the compositor already has, in a few ms where a GDI BitBlt of a 4 MP
    screen takes ~30. Its image has no pointer: the shape and the position
    come separately and are composited in here as the compositor would
    (agent_glow's lavender arrow while the run's look is on). A process can
    duplicate an output only once, so while the last recording's tail still
    holds it, or while there is none to have (UAC's secure desktop, some
    remote sessions), a GDI BitBlt with the cursor drawn by DrawIconEx stands
    in and duplication is tried again every RETRY seconds. None from grab()
    means nothing changed: the writer repeats the last frame."""
    RETRY = 2.0

    def __init__(self):
        self.dup = self.device = self.ctx = self.staging = None
        self.w = self.h = 0
        self.desktop = None             # the newest screen, BGR, without the pointer
        self.shape = None               # (type, BGRA pixels, AND mask, XOR mask)
        self.pos, self.visible = (0, 0), False
        self.shape_buf = ctypes.create_string_buffer(4096)
        self.dib = None                 # the GDI stand-in's buffer
        self.lock = threading.Lock()
        self.closed = self.aware = self.warned = False
        self.dxgi = True                # False for good once this output cannot be duplicated
        self.retry_at = 0.0
        self._try_open()

    def _try_open(self):
        try:
            self._open()
        except Exception as e:
            self._teardown()
            self.retry_at = time.monotonic() + self.RETRY
            if not self.warned:
                self.warned = True
                print(f"[recorder] screen through GDI while duplication is unavailable: {e}")

    def _open(self):
        """The duplication of the output at the desktop's origin (the primary
        display), on a D3D device made on that output's own adapter: on a
        hybrid-GPU laptop a device on the default adapter cannot duplicate
        the other adapter's output."""
        factory, adapter, output = ctypes.c_void_p(), None, None
        try:
            if ctypes.WinDLL("dxgi").CreateDXGIFactory1(
                    ctypes.byref(_IID_IDXGIFactory1), ctypes.byref(factory)) < 0:
                raise OSError("CreateDXGIFactory1 failed")
            enum = (ctypes.c_long, ctypes.c_uint, ctypes.POINTER(ctypes.c_void_p))
            i = 0
            while output is None:
                ad = ctypes.c_void_p()
                if _com(factory, 7, *enum)(factory, i, ctypes.byref(ad)) < 0:    # EnumAdapters
                    raise OSError("no display output at the desktop's origin")
                j = 0
                while output is None:
                    out = ctypes.c_void_p()
                    if _com(ad, 7, *enum)(ad, j, ctypes.byref(out)) < 0:          # EnumOutputs
                        break
                    desc = _DXGI_OUTPUT_DESC()
                    _com(out, 7, ctypes.c_long, ctypes.POINTER(_DXGI_OUTPUT_DESC))(
                        out, ctypes.byref(desc))
                    if (desc.DesktopCoordinates.left, desc.DesktopCoordinates.top) == (0, 0):
                        o1 = ctypes.c_void_p()
                        if _com(out, 0, ctypes.c_long, ctypes.POINTER(_GUID),
                                ctypes.POINTER(ctypes.c_void_p))(
                                out, ctypes.byref(_IID_IDXGIOutput1), ctypes.byref(o1)) >= 0:
                            output = o1
                    _release(out)
                    j += 1
                if output is None:
                    _release(ad)
                else:
                    adapter = ad
                i += 1
            device, ctx = ctypes.c_void_p(), ctypes.c_void_p()
            if ctypes.WinDLL("d3d11").D3D11CreateDevice(
                    adapter, 0, None, 0, None, 0, 7,        # driver type UNKNOWN, SDK version 7
                    ctypes.byref(device), None, ctypes.byref(ctx)) < 0:
                raise OSError("D3D11CreateDevice failed")
            self.device, self.ctx = device, ctx
            dup = ctypes.c_void_p()
            hr = _com(output, 22, ctypes.c_long, ctypes.c_void_p,
                      ctypes.POINTER(ctypes.c_void_p))(output, device, ctypes.byref(dup))
            if hr < 0:
                if hr & 0xFFFFFFFF == DXGI_ERROR_UNSUPPORTED:
                    self.dxgi = False
                raise OSError(f"DuplicateOutput failed (0x{hr & 0xFFFFFFFF:08X})")
            self.dup = dup
        finally:
            _release(output)
            _release(adapter)
            _release(factory)
        # The mode's own size, not DesktopCoordinates: those are shrunk for a
        # DPI-unaware caller, and a staging texture of that size copies nothing.
        dd = _DXGI_OUTDUPL_DESC()
        _com(self.dup, 7, None, ctypes.POINTER(_DXGI_OUTDUPL_DESC))(self.dup, ctypes.byref(dd))
        if dd.Rotation not in (0, 1):   # a portrait display comes unrotated: GDI has it right
            self.dxgi = False
            raise OSError("the display is rotated")
        self.w, self.h = dd.Width, dd.Height
        # B8G8R8A8, one mip level, a staging texture the CPU reads
        td = _D3D11_TEXTURE2D_DESC(self.w, self.h, 1, 1, 87, 1, 0, 3, 0, 0x20000, 0)
        staging = ctypes.c_void_p()
        if _com(self.device, 5, ctypes.c_long, ctypes.POINTER(_D3D11_TEXTURE2D_DESC),
                ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p))(
                self.device, ctypes.byref(td), None, ctypes.byref(staging)) < 0:
            raise OSError("CreateTexture2D failed")
        self.staging = staging
        # Bound once: building a WINFUNCTYPE costs more than a capture takes.
        self._acquire = _com(self.dup, 8, ctypes.c_long, ctypes.c_uint,
                             ctypes.POINTER(_DXGI_OUTDUPL_FRAME_INFO),
                             ctypes.POINTER(ctypes.c_void_p))
        self._shape_of = _com(self.dup, 11, ctypes.c_long, ctypes.c_uint, ctypes.c_void_p,
                              ctypes.POINTER(ctypes.c_uint),
                              ctypes.POINTER(_DXGI_OUTDUPL_POINTER_SHAPE_INFO))
        self._release_frame = _com(self.dup, 14, ctypes.c_long)
        self._copy = _com(self.ctx, 47, None, ctypes.c_void_p, ctypes.c_void_p)
        self._map = _com(self.ctx, 14, ctypes.c_long, ctypes.c_void_p, ctypes.c_uint,
                         ctypes.c_uint, ctypes.c_uint, ctypes.POINTER(_D3D11_MAPPED_SUBRESOURCE))
        self._unmap = _com(self.ctx, 15, None, ctypes.c_void_p, ctypes.c_uint)

    def _teardown(self):
        for name in ("staging", "dup", "ctx", "device"):
            _release(getattr(self, name))
            setattr(self, name, None)

    def _refresh(self):
        """Take the compositor's news: a new desktop image, the pointer moving
        or changing shape. True when there was any. Raises once the
        duplication has died (a mode change, UAC's secure desktop)."""
        import numpy as np
        info, res = _DXGI_OUTDUPL_FRAME_INFO(), ctypes.c_void_p()
        hr = self._acquire(self.dup, 0, ctypes.byref(info), ctypes.byref(res))
        if hr & 0xFFFFFFFF == DXGI_ERROR_WAIT_TIMEOUT:
            return False
        if hr < 0:
            raise OSError(f"AcquireNextFrame failed (0x{hr & 0xFFFFFFFF:08X})")
        try:
            news = self._pointer_news(info)
            if info.LastPresentTime == 0 and self.desktop is not None:
                return news                 # the pointer alone
            tex = ctypes.c_void_p()
            if _com(res, 0, ctypes.c_long, ctypes.POINTER(_GUID), ctypes.POINTER(ctypes.c_void_p))(
                    res, ctypes.byref(_IID_ID3D11Texture2D), ctypes.byref(tex)) < 0:
                return news
            try:
                self._copy(self.ctx, self.staging, tex)
                m = _D3D11_MAPPED_SUBRESOURCE()
                if self._map(self.ctx, self.staging, 0, 1, 0, ctypes.byref(m)) < 0:   # MAP_READ
                    return news
                try:
                    rows = np.ctypeslib.as_array(
                        ctypes.cast(m.pData, ctypes.POINTER(ctypes.c_uint8)),
                        shape=(self.h, m.RowPitch))
                    if self.desktop is None or self.desktop.shape[:2] != (self.h, self.w):
                        self.desktop = np.empty((self.h, self.w, 3), np.uint8)
                    _bgr(rows[:, :self.w * 4].reshape(self.h, self.w, 4), self.desktop)
                finally:
                    self._unmap(self.ctx, self.staging, 0)
            finally:
                _release(tex)
            return True
        finally:
            self._release_frame(self.dup)
            _release(res)

    def _pointer_news(self, info):
        """Apply one frame's pointer news; before ReleaseFrame, which drops it."""
        import numpy as np
        news = False
        if info.LastMouseUpdateTime:
            pos, visible = (info.PointerPosition.x, info.PointerPosition.y), bool(info.PointerVisible)
            news = visible != self.visible or (visible and pos != self.pos)
            self.pos, self.visible = pos, visible
        size = info.PointerShapeBufferSize      # only sent when the shape changes
        if size:
            if len(self.shape_buf) < size:
                self.shape_buf = ctypes.create_string_buffer(size)
            need, si = ctypes.c_uint(), _DXGI_OUTDUPL_POINTER_SHAPE_INFO()
            if self._shape_of(self.dup, size, self.shape_buf, ctypes.byref(need),
                              ctypes.byref(si)) >= 0:
                w, h, pitch = si.Width, si.Height, si.Pitch
                buf = np.frombuffer(self.shape_buf.raw[:size], np.uint8)
                if si.Type == 1:                # monochrome: the AND mask stacked over the XOR mask
                    h //= 2
                    bits = np.unpackbits(buf[:pitch * h * 2].reshape(h * 2, pitch), axis=1)
                    bits = bits[:, :w].astype(bool)
                    self.shape = (1, None, bits[:h], bits[h:])
                else:                           # 2 colour, 4 masked colour: BGRA
                    px = buf[:pitch * h].reshape(h, pitch)[:, :w * 4].reshape(h, w, 4).copy()
                    self.shape = (si.Type, px, None, None)
                news = True
        return news

    def _draw_pointer(self, frame):
        """Composite the pointer the way the compositor does, per shape type."""
        import numpy as np
        kind, px, and_mask, xor_mask = self.shape
        ph, pw = and_mask.shape if px is None else px.shape[:2]
        x, y = self.pos                 # the shape's top-left, in output pixels
        fh, fw = frame.shape[:2]
        x0, y0, x1, y1 = max(0, x), max(0, y), min(fw, x + pw), min(fh, y + ph)
        if x1 <= x0 or y1 <= y0:
            return
        dst = frame[y0:y1, x0:x1]
        sy, sx = slice(y0 - y, y1 - y), slice(x0 - x, x1 - x)
        if kind == 2:                   # colour, straight alpha
            src = px[sy, sx]
            a = src[..., 3:4].astype(np.uint16)
            dst[:] = (src[..., :3] * a + dst * (255 - a) + 127) // 255
        elif kind == 4:                 # masked colour: alpha set XORs the screen, else replaces it
            src = px[sy, sx]
            dst[:] = np.where(src[..., 3:4] != 0, dst ^ src[..., :3], src[..., :3])
        else:                           # monochrome: (screen AND and-mask) XOR xor-mask
            keep = and_mask[sy, sx][..., None]
            flip = xor_mask[sy, sx][..., None]
            dst[:] = np.where(keep, dst, 0) ^ (flip * np.uint8(255))

    def _gdi(self):
        """The stand-in: a BitBlt of the primary display (CAPTUREBLT brings the
        layered glow along) with the cursor drawn onto it by DrawIconEx, which
        gets the monochrome ones (the I-beam) right."""
        w, h = _user32.GetSystemMetrics(0), _user32.GetSystemMetrics(1)
        if w < 2 or h < 2:
            return None
        if self.dib is None or self.dib.size != (w, h):
            if self.dib is not None:
                self.dib.free()
                self.dib = None
            self.dib = _Dib(w, h)
        screen = _user32.GetDC(None)
        try:
            if not _gdi32.BitBlt(self.dib.dc, 0, 0, w, h, screen, 0, 0, SRCCOPY | CAPTUREBLT):
                return None
        finally:
            _user32.ReleaseDC(None, screen)
        ci = _CURSORINFO(ctypes.sizeof(_CURSORINFO))
        if _user32.GetCursorInfo(ctypes.byref(ci)) and ci.flags & CURSOR_SHOWING and ci.hCursor:
            ii = _ICONINFO()
            if _user32.GetIconInfo(ci.hCursor, ctypes.byref(ii)):
                _user32.DrawIconEx(self.dib.dc, ci.ptScreenPos.x - ii.xHotspot,
                                   ci.ptScreenPos.y - ii.yHotspot, ci.hCursor,
                                   0, 0, 0, None, DI_NORMAL)
                for bm in (ii.hbmColor, ii.hbmMask):    # GetIconInfo's bitmaps are ours to free
                    if bm:
                        _gdi32.DeleteObject(bm)
        return _bgr(self.dib.px)

    def grab(self):
        with self.lock:
            if self.closed:
                return None
            if not self.aware:
                _physical_pixels()      # for the GDI stand-in; DXGI is in physical pixels anyway
                self.aware = True
            if self.dup is None and self.dxgi and time.monotonic() >= self.retry_at:
                self._try_open()
            if self.dup is None:
                return self._gdi()
            try:
                if not self._refresh():
                    return None
            except Exception:
                self._teardown()        # a dead duplication: rebuilt on the next call
                self.retry_at = 0.0
                return None
            frame = self.desktop.copy()
            if self.visible and self.shape is not None:
                self._draw_pointer(frame)
            return frame

    def close(self):
        # Never under a grab (see _WindowGrab.close).
        if self.lock.acquire(timeout=1.0):
            try:
                self.closed = True
                self._teardown()
                if self.dib is not None:
                    self.dib.free()
                    self.dib = None
            finally:
                self.lock.release()


# ---------------------------------------------------------------------------
# The recorder
# ---------------------------------------------------------------------------

class _Recorder:
    def __init__(self, ui_window, kind, on_done):
        self.kind = "web" if kind == "web" else "computer"
        self.ui_window = ui_window
        self.on_done = on_done
        stamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
        self.path = _ensure(data_root() / "recordings" / f"{stamp}_{self.kind}")
        self._deadline = None           # monotonic time to stop at; None = running
        self._stop_called = None        # when stop() first came: the glow fades from here
        self._lock = threading.Lock()
        self._thread = None

    def begin(self):
        self._t0 = time.monotonic()
        self._done = threading.Event()
        self._pointer = None
        self._sources = []              # Linux and Windows frame sources, closed in _run
        if sys.platform == "win32":
            self._streams = self._windows_streams()
            return self._start()
        if sys.platform != "darwin":
            self._streams = self._linux_streams()
            return self._start()
        ui_id = int(self.ui_window.native.windowNumber()) if self.ui_window else 0

        if self.kind == "computer":
            rect = _display_rect()
            first = _grab_display(rect)
            if first is None:
                raise RuntimeError("the display could not be captured")
            scale = first.shape[1] / rect.size.width
            self._pointer = _Pointer(rect, scale)
            glow = _Glow(first.shape[1] - first.shape[1] % 2, first.shape[0] - first.shape[0] % 2, scale)
            self._t0 = time.monotonic()   # after the setup above, so the video opens on a picture, not black

            def decorate(frame, now):
                glow.draw(frame, now, self._t0, self._glow_fade(now))
                self._pointer.draw(frame, now, self._t0)
            screen = _Stream(self.path / "screen.mp4", lambda: _grab_display(rect),
                             self._t0, self._done, decorate)
        else:
            chrome = {"id": None, "checked": 0.0}

            def grab_chrome():
                # Chrome starts after the recording does and may open another
                # window later, so it is looked up again every second.
                now = time.monotonic()
                if chrome["id"] is None or now - chrome["checked"] > 1.0:
                    chrome["checked"] = now
                    chrome["id"] = _chrome_window_id() or chrome["id"]
                return _grab_window(chrome["id"])
            screen = _Stream(self.path / "screen.mp4", grab_chrome, self._t0, self._done)

        app = _Stream(self.path / "app.mp4", lambda: _grab_window(ui_id), self._t0, self._done)
        self._streams = (screen, app)
        self._start()

    def _linux_streams(self):
        """Linux: nothing is drawn in (see the module docstring)."""
        src = _PipeWireScreen() if self.kind == "computer" else _Screencast()
        self._sources.append(src)
        if self.kind == "computer" and src.prime():
            self._t0 = time.monotonic()   # after the setup above, so the video opens on a picture, not black
        streams = [_Stream(self.path / "screen.mp4", src.grab, self._t0, self._done)]
        if self.ui_window and _WebViewSnapshot.usable():
            app = _WebViewSnapshot(self.ui_window)
            streams.append(_Stream(self.path / "app.mp4", app.grab, self._t0, self._done))
        return tuple(streams)

    def _windows_streams(self):
        """Windows: nothing is drawn in (see the module docstring)."""
        if self.kind == "computer":
            src = _Desktop()
            self._t0 = time.monotonic()   # after the setup above, so the video opens on a picture, not black
        else:
            chrome = {"hwnd": None, "checked": 0.0}

            def find_chrome():
                # Chrome starts after the recording does and may open another
                # window later, so it is looked up again: every second once
                # found, five times a second until then.
                now = time.monotonic()
                if now - chrome["checked"] > (1.0 if chrome["hwnd"] else 0.2):
                    chrome["checked"] = now
                    chrome["hwnd"] = _chrome_hwnd() or chrome["hwnd"]
                return chrome["hwnd"]
            src = _WindowGrab(find_chrome)
        self._sources.append(src)
        streams = [_Stream(self.path / "screen.mp4", src.grab, self._t0, self._done)]
        hwnd = _ui_hwnd(self.ui_window)
        if hwnd:
            app = _WindowGrab(lambda: hwnd)
            self._sources.append(app)
            streams.append(_Stream(self.path / "app.mp4", app.grab, self._t0, self._done))
        return tuple(streams)

    def _start(self):
        # Two recordings can overlap for a moment (the next run starts during
        # the last one's tail), so the painting switch is flipped under the
        # lock: on here, and off only by the last recording to finish.
        with _live_lock:
            _live.add(self)
            if self.ui_window:
                _keep_ui_painting(self.ui_window, True)
        for s in self._streams:
            s.start()
        self._thread = threading.Thread(target=self._run, name="run-recorder", daemon=True)
        self._thread.start()

    def _glow_fade(self, now):
        # The real glow goes off when the run ends; the video's fades out over
        # 0.4 s once stop() comes, rather than glowing through the tail.
        with self._lock:
            since = self._stop_called
        return 1.0 if since is None else max(0.0, 1.0 - (now - since) / 0.4)

    def stop(self, tail=0.0, wait=False):
        """Keep recording `tail` more seconds, then finalize. Safe to call more
        than once: the earliest deadline wins."""
        with self._lock:
            now = time.monotonic()
            if self._stop_called is None:
                self._stop_called = now
            end = now + max(0.0, tail)
            if self._deadline is None or end < self._deadline:
                self._deadline = end
        if wait and self._thread is not None:
            self._thread.join(timeout=max(5.0, tail + 5.0))

    def _run(self):
        while True:
            with self._lock:
                deadline = self._deadline
            if deadline is not None and time.monotonic() >= deadline:
                break
            time.sleep(0.05)
        self._done.set()
        for s in self._streams:
            s.join(timeout=5.0)
        if self._pointer is not None:
            self._pointer.close()
        for src in self._sources:
            src.close()
        self._finish()

    def _finish(self):
        saved = any(s.saved for s in self._streams)
        if saved:
            print(f"[recorder] saved {self.path}")
        else:
            try:
                self.path.rmdir()       # nothing was ever captured: no empty folder
            except OSError:
                pass
        with _live_lock:
            _live.discard(self)
            if self.ui_window and not _live and not _exiting:
                _keep_ui_painting(self.ui_window, False)
        if self.on_done and not _exiting:
            try:
                self.on_done(self.path if saved else None)
            except Exception:
                pass


@atexit.register
def finalize_all():
    """The app is closing mid-run: the run thread is a daemon and never
    reaches its finally, so finish every live recording here. An mp4 is only
    playable once its writer is released. On macOS also wired to the window's
    closing event (frontend/service.py), because Cmd+Q ends in NSApplication's
    terminate:, which calls C exit() and skips atexit. Windows needs no more
    than atexit: closing the window returns from webview.start() and the
    interpreter exits normally, and its closing event runs on the UI thread,
    where waiting here would hold up PrintWindow on that very window."""
    global _exiting
    _exiting = True
    with _live_lock:
        live = list(_live)
    for rec in live:
        rec.stop(wait=True)


if __name__ == "__main__":
    # `python -m AutoCua.utils.run_recorder`: what this machine can record.
    print("recording supported:", supported())
    if sys.platform == "win32":
        desktop = _Desktop()
        print("  screen in computer use:", "DXGI desktop duplication" if desktop.dup
              else "GDI BitBlt (no desktop duplication here)")
        desktop.close()
    if sys.platform.startswith("linux"):
        from AutoCua.linux.tree import element  # noqa: F401  puts the distro's gi on sys.path

        def _gst():
            import gi
            gi.require_version("Gst", "1.0")
            from gi.repository import Gst
            Gst.init(None)
            assert Gst.ElementFactory.find("pipewiresrc"), "no pipewiresrc (gstreamer1.0-pipewire)"

        def _cairo():
            import gi
            gi.require_foreign("cairo")

        def _ffmpeg():
            import imageio_ffmpeg
            imageio_ffmpeg.get_ffmpeg_exe()

        def _ws():
            from websockets.sync.client import connect  # noqa: F401

        for name, check in (("imageio-ffmpeg, the encoder", _ffmpeg),
                            ("GStreamer + pipewiresrc, the screen in computer use", _gst),
                            ("gi-cairo, the app window", _cairo),
                            ("websockets, the screen in web use", _ws)):
            try:
                check()
                print(f"  ok       {name}")
            except Exception as e:
                print(f"  MISSING  {name}: {e}")
