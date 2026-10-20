#!/usr/bin/env python3

"""The desktop look that says an agent is driving this machine.

Two things at once, for as long as a run is live:
  * the mouse pointer becomes the animated lavender arrow, which shrinks
    toward its tip while a button is held — that shrink is the click feedback;
  * a soft lavender glow breathes around all four screen edges.
Both go back to normal the moment the run ends. On macOS it is the glow
alone, see below.

    agent_glow.start()   # agent run begins
    agent_glow.stop()    # run ends, by completion, error or Stop

start() is idempotent and never raises: a desktop cue must not be able to
fail an agent run. stop() is safe to call when nothing is running, and an
atexit hook calls it too, so a normal exit always hands the desktop back.

ONE FILE, THREE PLATFORMS
start() and stop() switch on the platform. Windows gets the cursor and the
glow (_run_windows), macOS gets the glow alone (_start_mac), and Linux gets
the glow and the cursor, minus the click feedback (_run_linux).
macOS keeps its own cursor because it has to: SetSystemCursor replaces the
cursor for the whole logged-in session, and macOS has no equivalent, since
NSCursor only applies to your own app's windows. A Wayland client cannot
set another client's cursor at all either. The edge glow is just a
transparent click-through always-on-top window, so it ports directly.
Linux does the glow as four override-redirect X11 windows (Xwayland under a
Wayland session) and the pointer as an Xcursor theme it installs for the run
and removes afterwards. A theme animates itself frame by frame in the display
server, which is why the arrow breathes there but does not shrink on click:
nothing tells a cursor theme that a button is down.
On macOS every AppKit window belongs to the main thread. In the desktop app
that thread runs pywebview's event loop, so start() hands the glow to it.
Terminal mode runs no loop there (the agent itself holds the main thread),
so start() runs this module's CLI as a child process with a loop of its own,
and stop() ends it. The child also leaves by itself if the agent dies
without calling stop().

The geometry below is generated rather than traced from the demo's SVG,
because that path is not symmetric. Everything is defined as one half and
mirrored, so symmetry is exact by construction.

This module is also runnable, which is how you recover a cursor left behind
by a hard kill (Task Manager), since that skips every cleanup path:

    python -m AutoCua.utils.agent_glow --restore
    python -m AutoCua.utils.agent_glow            # try it, Ctrl+C to stop
    python -m AutoCua.utils.agent_glow --preview out.png
"""

import argparse
import atexit
import ctypes
import io
import math
import os
import shutil
import signal
import struct
import subprocess
import sys
import threading
import time

from PIL import Image, ImageChops, ImageDraw, ImageFilter

IS_WIN = sys.platform == "win32"
IS_MAC = sys.platform == "darwin"
IS_LINUX = sys.platform.startswith("linux")

# Nuitka binary vs a normal Python run, the same test as AutoCua/__init__.py.
# The binary has no `python -m` to start the macOS terminal-mode glow child.
_COMPILED = bool(getattr(sys, "frozen", False)) or "__compiled__" in globals()

# ----------------------------------------------------------------------------
# shape
#
# The arrow is generated from geometry rather than copied from the demo's SVG
# path, because that path is not symmetric: its right wing is 18% longer than
# its bottom wing, its notch sits 2.6 degrees off the axis, and its tip curve
# is not a symmetric fillet, which is what made the tip bulge. Everything below
# is defined as one half and mirrored, so symmetry is exact by construction.
# ----------------------------------------------------------------------------

VIEWBOX = 32.0
AXIS_DEG = 57.3         # direction the arrow points (its axis of symmetry)
HALF_ANGLE_DEG = 29.4   # half the spread between the two wings
WING = 30.0             # tip -> wing tip, identical on both sides
NOTCH = 18.9            # tip -> the concave notch between the wings
R_TIP = 3.4             # corner fillet radii
R_WING = 2.4
R_NOTCH = 4.6
STROKE = 2.5
MARGIN = 2.8            # room for the stroke and the shadow

PALETTE = [(184, 167, 240), (143, 123, 201), (212, 203, 250)]  # b8a7f0 8f7bc9 d4cbfa

# (dx, dy, blur, alpha) in 32-unit space: a soft halo all round, plus a light drop
SHADOW_DROP = 0.58      # offset along the axis, so the shadow stays symmetric too
SHADOW_LAYERS = [(0.0, 0.0, 0.85, 0.16), (SHADOW_DROP, 1.25, 0.20)]
SHADOW_RGB = (44, 30, 78)

FRAMES = 24
CYCLE_SECONDS = 2.0

# click feedback: the demo scales the cursor to 0.75 about its top-left, which is
# the tip, so it shrinks toward the point and the hotspot never moves.
GLOW_FRACTION = 0.06       # border thickness, as a fraction of the short screen edge
GLOW_OPACITY = 0.32        # peak opacity right at the screen edge
BREATH_SECONDS = 5.0       # one full in-and-out
BREATH_LOW, BREATH_HIGH = 0.45, 1.0

# macOS runs the band thicker and a good deal stronger than the other two. At
# the shared pair above the mac glow is on screen and correct in every other
# way, but on a bright desktop it is too faint to read as a cue, which is the
# only thing it is there for. Windows and Linux keep the values they were
# tuned with. Checked live on a Retina display.
MAC_GLOW_FRACTION = 0.075
MAC_GLOW_OPACITY = 0.62

# What this platform's glow uses, and what the CLI flags below default to.
DEF_GLOW_FRACTION = MAC_GLOW_FRACTION if IS_MAC else GLOW_FRACTION
DEF_GLOW_OPACITY = MAC_GLOW_OPACITY if IS_MAC else GLOW_OPACITY

CLICK_STEPS = [1.0, 0.93, 0.84, 0.75]   # eased, not a hard snap
CLICK_STEP_MS = 30
POLL_SECONDS = 0.016


def _fillet(prev, v, nxt, r, steps=28):
    """A true circular fillet at vertex v, symmetric about the corner bisector."""
    ux, uy = prev[0] - v[0], prev[1] - v[1]
    wx, wy = nxt[0] - v[0], nxt[1] - v[1]
    lu, lw = math.hypot(ux, uy), math.hypot(wx, wy)
    ux, uy, wx, wy = ux / lu, uy / lu, wx / lw, wy / lw
    half = math.acos(max(-1.0, min(1.0, ux * wx + uy * wy))) / 2.0
    d = min(r / math.tan(half), lu * 0.5, lw * 0.5)
    r_eff = d * math.tan(half)
    bx, by = ux + wx, uy + wy
    lb = math.hypot(bx, by)
    cx = v[0] + bx / lb * (r_eff / math.sin(half))
    cy = v[1] + by / lb * (r_eff / math.sin(half))
    a1 = math.atan2(v[1] + uy * d - cy, v[0] + ux * d - cx)
    a2 = math.atan2(v[1] + wy * d - cy, v[0] + wx * d - cx)
    da = (a2 - a1 + math.pi) % (2 * math.pi) - math.pi
    return [(cx + r_eff * math.cos(a1 + da * i / steps),
             cy + r_eff * math.sin(a1 + da * i / steps)) for i in range(steps + 1)]


def build_shape():
    """Closed outline in the 32-unit box, plus the hotspot at the tip of the ink."""
    th = math.radians(HALF_ANGLE_DEG)
    tip_v = (0.0, 0.0)
    wing_a = (WING * math.cos(th),  WING * math.sin(th))
    notch_v = (NOTCH, 0.0)
    wing_b = (WING * math.cos(th), -WING * math.sin(th))   # the mirror of wing_a
    verts = [tip_v, wing_a, notch_v, wing_b]
    radii = [R_TIP, R_WING, R_NOTCH, R_WING]

    pts = []
    for i, (v, r) in enumerate(zip(verts, radii)):
        pts += _fillet(verts[i - 1], v, verts[(i + 1) % 4], r)
    pts.append(pts[0])

    inset = R_TIP / math.sin(th) - R_TIP      # where the tip arc sits behind the apex

    a = math.radians(AXIS_DEG)
    ca, sa = math.cos(a), math.sin(a)
    rot = lambda p: (p[0] * ca - p[1] * sa, p[0] * sa + p[1] * ca)
    pts = [rot(p) for p in pts]
    tip = rot((inset, 0.0))

    xs, ys = [p[0] for p in pts], [p[1] for p in pts]
    s = (VIEWBOX - 2 * MARGIN) / max(max(xs) - min(xs), max(ys) - min(ys))
    ox = (VIEWBOX - (max(xs) - min(xs)) * s) / 2 - min(xs) * s
    oy = (VIEWBOX - (max(ys) - min(ys)) * s) / 2 - min(ys) * s
    fit = lambda p: (p[0] * s + ox, p[1] * s + oy)

    tip = fit(tip)
    return [fit(p) for p in pts], (tip[0] - ca * STROKE / 2, tip[1] - sa * STROKE / 2)


OUTLINE, TIP = build_shape()

# The gradient runs along the axis of symmetry, not on a fixed 45 degree
# diagonal. On a diagonal the two tails land on different parts of the ramp, so
# one gets the pale end of the palette, loses contrast against the background and
# reads as the shorter tail. Projected on the axis, mirrored points get the same
# colour and both tails match at every moment of the cycle.
_AXIS = (math.cos(math.radians(AXIS_DEG)), math.sin(math.radians(AXIS_DEG)))
_PROJ = [x * _AXIS[0] + y * _AXIS[1] for x, y in OUTLINE]
PROJ_MIN, PROJ_SPAN = min(_PROJ), max(_PROJ) - min(_PROJ)


def hotspot_for(size):
    k = size / VIEWBOX
    return (max(0, round(TIP[0] * k)), max(0, round(TIP[1] * k)))


# ----------------------------------------------------------------------------
# rendering
# ----------------------------------------------------------------------------

def _resample(pts, step):
    """Even out the spacing along the polyline so the stroke has no gaps."""
    out, carry = [pts[0]], 0.0
    for (x0, y0), (x1, y1) in zip(pts, pts[1:]):
        dx, dy = x1 - x0, y1 - y0
        seg = math.hypot(dx, dy)
        if seg == 0:
            continue
        d = step - carry
        while d < seg:
            out.append((x0 + dx * d / seg, y0 + dy * d / seg))
            d += step
        carry = (carry + seg) % step
    out.append(pts[-1])
    return out


_MASK_CACHE = {}


def stroke_mask(size, ss):
    """
    The stroked outline as an 8-bit mask.

    Pillow's line(joint="curve") leaves hairline gaps at the joints and varies
    the width slightly, so the stroke is built the way a vector renderer defines
    one: a disc of the stroke diameter stamped along the path. Same for every
    frame, so it is cached.
    """
    key = (size, ss)
    if key in _MASK_CACHE:
        return _MASK_CACHE[key]

    canvas = size * ss
    scale = canvas / VIEWBOX
    r = STROKE * scale / 2.0
    pts = _resample([(x * scale, y * scale) for x, y in OUTLINE], max(0.4, r / 12.0))

    mask = Image.new("L", (canvas, canvas), 0)
    d = ImageDraw.Draw(mask)
    for x, y in pts:
        d.ellipse((x - r, y - r, x + r, y + r), fill=255)

    _MASK_CACHE[key] = mask
    return mask


def _cycle(u):
    """Walk the 3-colour palette in a loop; u is 0..1."""
    u %= 1.0
    seg = u * 3.0
    i = int(seg)
    f = seg - i
    a, b = PALETTE[i % 3], PALETTE[(i + 1) % 3]
    return tuple(round(a[k] + (b[k] - a[k]) * f) for k in range(3))


def _gradient(phase, n=64):
    """The diagonal 3-stop gradient, at low res; it gets scaled up after."""
    s0, s1, s2 = _cycle(phase), _cycle(phase + 1 / 3), _cycle(phase + 2 / 3)
    ca, sa = _AXIS
    px = []
    for y in range(n):
        for x in range(n):
            p = (x / (n - 1) * VIEWBOX) * ca + (y / (n - 1) * VIEWBOX) * sa
            t = min(1.0, max(0.0, (p - PROJ_MIN) / PROJ_SPAN))
            a, b, f = (s0, s1, t * 2) if t < 0.5 else (s1, s2, (t - 0.5) * 2)
            px.append(tuple(round(a[k] + (b[k] - a[k]) * f) for k in range(3)))
    img = Image.new("RGB", (n, n))
    img.putdata(px)
    return img


_BASE_CACHE = {}


def _base_frame(size, ss, phase):
    """The full-size cursor at supersampled resolution. Cached per frame."""
    key = (size, ss, round(phase, 6))
    if key in _BASE_CACHE:
        return _BASE_CACHE[key]

    canvas = size * ss
    scale = canvas / VIEWBOX
    mask = stroke_mask(size, ss)

    layer = _gradient(phase).resize((canvas, canvas), Image.BILINEAR).convert("RGBA")
    layer.putalpha(mask)

    for layer_spec in SHADOW_LAYERS:
        if len(layer_spec) == 4:
            dx, dy, blur, alpha = layer_spec
        else:                                   # distance along the axis
            along, blur, alpha = layer_spec
            dx, dy = along * _AXIS[0], along * _AXIS[1]
        haze = mask.filter(ImageFilter.GaussianBlur(blur * scale))
        haze = haze.point(lambda a, k=alpha: int(a * k))
        shifted = Image.new("L", (canvas, canvas), 0)
        shifted.paste(haze, (round(dx * scale), round(dy * scale)))
        shadow = Image.new("RGBA", (canvas, canvas), SHADOW_RGB + (0,))
        shadow.putalpha(shifted)
        layer = Image.alpha_composite(shadow, layer)

    _BASE_CACHE[key] = layer
    return layer


def render_frame(size, phase, ss=8, zoom=1.0):
    """
    One frame of the cursor as an RGBA image of size x size.

    zoom < 1 is the click state. Scaling the finished image (stroke, gradient and
    shadow together) about the tip is exactly what the demo's CSS transform does,
    and it keeps the hotspot on the same pixel.
    """
    base = _base_frame(size, ss, phase)
    canvas = size * ss

    if zoom < 1.0:
        small = base.resize((max(1, round(canvas * zoom)),) * 2, Image.LANCZOS)
        out = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
        tx, ty = TIP[0] * canvas / VIEWBOX, TIP[1] * canvas / VIEWBOX
        out.paste(small, (round(tx * (1 - zoom)), round(ty * (1 - zoom))))
        base = out

    return base.resize((size, size), Image.LANCZOS)


def _ramp(t):
    """Deep lavender at the corner fading to the palest stop at the outer edge."""
    a, b, f = (PALETTE[1], PALETTE[0], t * 2) if t < 0.5 else (PALETTE[0], PALETTE[2], (t - 0.5) * 2)
    return tuple(round(a[k] + (b[k] - a[k]) * f) for k in range(3))


def build_edge_glow(w, h, thickness, opacity=GLOW_OPACITY, n=224):
    """
    A glow hugging all four screen edges, as one full-screen RGBA field.

    Each edge is treated as its own light and the four are screen-blended
    (1 - product of the inverses) rather than picked with a min(), so the
    corners pick up two edges and stay a little warmer without a seam.
    The field is built small and scaled up; the falloff is smooth so nothing
    is lost, and it is generated whole so the four strips cut out of it line
    up exactly.
    """
    gw = n
    gh = max(8, round(n * h / w))
    alphas, cols = [], []
    for j in range(gh):
        for i in range(gw):
            x = i / (gw - 1) * (w - 1)
            y = j / (gh - 1) * (h - 1)
            dists = (x, y, (w - 1) - x, (h - 1) - y)
            inv = 1.0
            for d in dists:
                u = 1.0 - min(1.0, d / thickness)
                inv *= 1.0 - u * u          # quadratic: bright at the edge, no plateau
            alphas.append(int(round(255 * opacity * (1.0 - inv))))
            cols.append(_ramp(min(1.0, min(dists) / thickness)))

    a = Image.new("L", (gw, gh)); a.putdata(alphas)
    c = Image.new("RGB", (gw, gh)); c.putdata(cols)
    a = a.resize((w, h), Image.BICUBIC)
    c = c.resize((w, h), Image.BICUBIC)
    return Image.merge("RGBA", (*c.split(), a))


def edge_strips(w, h, t):
    """The four boxes that tile the border exactly, leaving the middle empty."""
    return [(0, 0, w, t), (0, h - t, w, h), (0, t, t, h - t), (w - t, t, w, h - t)]


def mock_screen(fraction=GLOW_FRACTION, opacity=GLOW_OPACITY, w=1280, h=800,
                bg=(26, 26, 32), breath=1.0):
    """A fake desktop, so the glow can be judged without running it."""
    t = max(16, min(int(min(w, h) * fraction), min(w, h) // 2))
    field = build_edge_glow(w, h, t, opacity * breath)
    screen = Image.alpha_composite(Image.new("RGBA", (w, h), bg + (255,)), field)
    cur = render_frame(96, 0.0, ss=3)
    screen.paste(cur, (w // 2 - 48, h // 2 - 48), cur)
    return screen.convert("RGB")


def _premultiplied_bgra(img):
    """UpdateLayeredWindow with ULW_ALPHA wants premultiplied alpha, in BGRA order."""
    r, g, b, a = img.split()
    r, g, b = (ImageChops.multiply(ch, a) for ch in (r, g, b))
    return Image.merge("RGBA", (b, g, r, a)).tobytes()


# ----------------------------------------------------------------------------
# Windows plumbing
# ----------------------------------------------------------------------------

if IS_WIN:
    from ctypes import wintypes

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [
            ("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG),
            ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
            ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
            ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
            ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
            ("biClrImportant", wintypes.DWORD),
        ]

    class BITMAPINFO(ctypes.Structure):
        _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wintypes.DWORD * 3)]

    class ICONINFO(ctypes.Structure):
        _fields_ = [
            ("fIcon", wintypes.BOOL), ("xHotspot", wintypes.DWORD),
            ("yHotspot", wintypes.DWORD), ("hbmMask", wintypes.HBITMAP),
            ("hbmColor", wintypes.HBITMAP),
        ]

    gdi32.CreateDIBSection.argtypes = [
        wintypes.HDC, ctypes.POINTER(BITMAPINFO), wintypes.UINT,
        ctypes.POINTER(ctypes.c_void_p), wintypes.HANDLE, wintypes.DWORD]
    gdi32.CreateDIBSection.restype = wintypes.HBITMAP
    gdi32.CreateBitmap.argtypes = [ctypes.c_int, ctypes.c_int, wintypes.UINT,
                                   wintypes.UINT, ctypes.c_void_p]
    gdi32.CreateBitmap.restype = wintypes.HBITMAP
    gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]

    user32.CreateIconIndirect.argtypes = [ctypes.POINTER(ICONINFO)]
    user32.CreateIconIndirect.restype = wintypes.HICON
    user32.CopyIcon.argtypes = [wintypes.HICON]
    user32.CopyIcon.restype = wintypes.HICON
    user32.DestroyIcon.argtypes = [wintypes.HICON]
    user32.SetSystemCursor.argtypes = [wintypes.HICON, wintypes.DWORD]
    user32.SetSystemCursor.restype = wintypes.BOOL
    user32.SystemParametersInfoW.argtypes = [wintypes.UINT, wintypes.UINT,
                                             ctypes.c_void_p, wintypes.UINT]

    SPI_SETCURSORS = 0x0057
    SM_CXCURSOR = 13

    OCR = {
        "normal": 32512, "ibeam": 32513, "wait": 32514, "cross": 32515,
        "up": 32516, "sizenwse": 32642, "sizenesw": 32643, "sizewe": 32644,
        "sizens": 32645, "sizeall": 32646, "no": 32648, "hand": 32649,
        "appstarting": 32650, "help": 32651,
    }
    DEFAULT_TARGETS = ["normal", "appstarting", "hand", "help"]

    LRESULT = ctypes.c_ssize_t
    WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wintypes.HWND, wintypes.UINT,
                                 wintypes.WPARAM, wintypes.LPARAM)

    class WNDCLASS(ctypes.Structure):
        _fields_ = [
            ("style", wintypes.UINT), ("lpfnWndProc", WNDPROC),
            ("cbClsExtra", ctypes.c_int), ("cbWndExtra", ctypes.c_int),
            ("hInstance", wintypes.HINSTANCE), ("hIcon", wintypes.HICON),
            ("hCursor", wintypes.HANDLE), ("hbrBackground", wintypes.HBRUSH),
            ("lpszMenuName", wintypes.LPCWSTR), ("lpszClassName", wintypes.LPCWSTR),
        ]

    class BLENDFUNCTION(ctypes.Structure):
        _fields_ = [("BlendOp", ctypes.c_ubyte), ("BlendFlags", ctypes.c_ubyte),
                    ("SourceConstantAlpha", ctypes.c_ubyte), ("AlphaFormat", ctypes.c_ubyte)]

    WS_POPUP = 0x80000000
    WS_EX_LAYERED = 0x00080000
    WS_EX_TRANSPARENT = 0x00000020      # clicks pass straight through
    WS_EX_TOPMOST = 0x00000008
    WS_EX_TOOLWINDOW = 0x00000080       # keeps it out of Alt+Tab
    WS_EX_NOACTIVATE = 0x08000000       # never steals focus
    SW_SHOWNA = 8
    ULW_ALPHA = 0x02
    AC_SRC_OVER, AC_SRC_ALPHA = 0x00, 0x01
    PM_REMOVE = 0x0001

    # restype matters on 64-bit: without it ctypes truncates handles to int
    user32.CreateWindowExW.restype = wintypes.HWND
    user32.CreateWindowExW.argtypes = [
        wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD,
        ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
        wintypes.HWND, wintypes.HMENU, wintypes.HINSTANCE, wintypes.LPVOID]
    user32.DefWindowProcW.restype = LRESULT
    user32.DefWindowProcW.argtypes = [wintypes.HWND, wintypes.UINT,
                                      wintypes.WPARAM, wintypes.LPARAM]
    user32.GetDC.restype = wintypes.HDC
    user32.GetDC.argtypes = [wintypes.HWND]
    user32.ReleaseDC.argtypes = [wintypes.HWND, wintypes.HDC]
    user32.RegisterClassW.argtypes = [ctypes.POINTER(WNDCLASS)]
    user32.UnregisterClassW.argtypes = [wintypes.LPCWSTR, wintypes.HINSTANCE]
    user32.DestroyWindow.argtypes = [wintypes.HWND]
    user32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
    user32.UpdateLayeredWindow.restype = wintypes.BOOL
    user32.UpdateLayeredWindow.argtypes = [
        wintypes.HWND, wintypes.HDC, ctypes.POINTER(wintypes.POINT),
        ctypes.POINTER(wintypes.SIZE), wintypes.HDC, ctypes.POINTER(wintypes.POINT),
        wintypes.DWORD, ctypes.POINTER(BLENDFUNCTION), wintypes.DWORD]
    user32.PeekMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND,
                                    wintypes.UINT, wintypes.UINT, wintypes.UINT]
    gdi32.CreateCompatibleDC.restype = wintypes.HDC
    gdi32.CreateCompatibleDC.argtypes = [wintypes.HDC]
    gdi32.SelectObject.restype = wintypes.HGDIOBJ
    gdi32.SelectObject.argtypes = [wintypes.HDC, wintypes.HGDIOBJ]
    gdi32.DeleteDC.argtypes = [wintypes.HDC]
    kernel32.GetModuleHandleW.restype = wintypes.HINSTANCE
    kernel32.GetModuleHandleW.argtypes = [wintypes.LPCWSTR]


def make_hcursor(img, hotspot):
    """PIL RGBA image -> HCURSOR."""
    w, h = img.size
    # BGRA, the order CreateDIBSection wants — produced by a channel swap
    # rather than a per-pixel Python loop over getdata(). Identical bytes, one
    # C call instead of ~1k iterations per cursor with 96 cursors built per
    # run, and getdata() is deprecated and goes away in Pillow 14.
    r, g, b, a = img.split()
    bgra = Image.merge("RGBA", (b, g, r, a)).tobytes()

    bmi = BITMAPINFO()
    bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bmi.bmiHeader.biWidth = w
    bmi.bmiHeader.biHeight = -h          # negative = top-down rows
    bmi.bmiHeader.biPlanes = 1
    bmi.bmiHeader.biBitCount = 32
    bmi.bmiHeader.biCompression = 0      # BI_RGB

    bits = ctypes.c_void_p()
    hbm_colour = gdi32.CreateDIBSection(None, ctypes.byref(bmi), 0,
                                        ctypes.byref(bits), None, 0)
    if not hbm_colour:
        raise ctypes.WinError(ctypes.get_last_error())
    ctypes.memmove(bits, bytes(bgra), len(bgra))

    row = ((w + 15) // 16) * 2                      # 1bpp rows are WORD aligned
    blank = ctypes.create_string_buffer(row * h)    # all zeros = use the alpha
    hbm_mask = gdi32.CreateBitmap(w, h, 1, 1, blank)

    info = ICONINFO(False, int(hotspot[0]), int(hotspot[1]), hbm_mask, hbm_colour)
    hcur = user32.CreateIconIndirect(ctypes.byref(info))

    gdi32.DeleteObject(hbm_colour)
    gdi32.DeleteObject(hbm_mask)
    if not hcur:
        raise ctypes.WinError(ctypes.get_last_error())
    return hcur


GLOW_CLASS = "LavenderScreenGlow"


class ScreenGlow:
    """
    A lavender glow hugging all four screen edges.

    Four click-through layered windows tile the border and leave the middle of
    the screen with no overlay over it at all, which keeps full-screen apps out
    from under a full-screen layer. The strips are cut from one generated field,
    so they meet without a seam.

    Pixels upload once. Breathing then only changes the blend function's constant
    alpha, which moves no pixels, so this idles at effectively no cost.
    """

    def __init__(self, fraction=GLOW_FRACTION, opacity=GLOW_OPACITY):
        self.sw = user32.GetSystemMetrics(0)
        self.sh = user32.GetSystemMetrics(1)
        short = min(self.sw, self.sh)
        self.t = max(16, min(int(short * fraction), short // 2))
        self.hinst = kernel32.GetModuleHandleW(None)

        # the window procedure object must outlive the windows or ctypes frees it
        self._proc = WNDPROC(lambda h, m, w, l: user32.DefWindowProcW(h, m, w, l))
        cls = WNDCLASS()
        cls.lpfnWndProc = self._proc
        cls.hInstance = self.hinst
        cls.lpszClassName = GLOW_CLASS
        user32.RegisterClassW(ctypes.byref(cls))

        field = build_edge_glow(self.sw, self.sh, self.t, opacity)
        self.screen_dc = user32.GetDC(None)
        self.windows = []
        for box in edge_strips(self.sw, self.sh, self.t):
            self.windows.append(self._spawn(field.crop(box), box[0], box[1]))
        del field

        self.level = -1

    def _spawn(self, img, x, y):
        w, h = img.size
        hwnd = user32.CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
            WS_EX_NOACTIVATE, GLOW_CLASS, None, WS_POPUP,
            x, y, w, h, None, None, self.hinst, None)
        if not hwnd:
            raise ctypes.WinError(ctypes.get_last_error())

        bmi = BITMAPINFO()
        bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.bmiHeader.biWidth = w
        bmi.bmiHeader.biHeight = -h
        bmi.bmiHeader.biPlanes = 1
        bmi.bmiHeader.biBitCount = 32
        bmi.bmiHeader.biCompression = 0

        bits = ctypes.c_void_p()
        hbm = gdi32.CreateDIBSection(self.screen_dc, ctypes.byref(bmi), 0,
                                     ctypes.byref(bits), None, 0)
        data = _premultiplied_bgra(img)
        ctypes.memmove(bits, data, len(data))

        hdc = gdi32.CreateCompatibleDC(self.screen_dc)
        gdi32.SelectObject(hdc, hbm)

        pt = wintypes.POINT(x, y)
        sz = wintypes.SIZE(w, h)
        src = wintypes.POINT(0, 0)
        blend = BLENDFUNCTION(AC_SRC_OVER, 0, 255, AC_SRC_ALPHA)
        user32.UpdateLayeredWindow(hwnd, self.screen_dc, ctypes.byref(pt), ctypes.byref(sz),
                                   hdc, ctypes.byref(src), 0, ctypes.byref(blend), ULW_ALPHA)
        user32.ShowWindow(hwnd, SW_SHOWNA)
        return hwnd, hdc, hbm

    def breathe(self, k):
        """k is 0..1. Only the blend alpha changes, so no pixels move."""
        a = max(0, min(255, int(round(k * 255))))
        if a == self.level:
            return
        self.level = a
        blend = BLENDFUNCTION(AC_SRC_OVER, 0, a, AC_SRC_ALPHA)
        for hwnd, _, _ in self.windows:
            user32.UpdateLayeredWindow(hwnd, None, None, None, None, None, 0,
                                       ctypes.byref(blend), ULW_ALPHA)

    def close(self):
        for hwnd, hdc, hbm in self.windows:
            user32.DestroyWindow(hwnd)
            gdi32.DeleteDC(hdc)
            gdi32.DeleteObject(hbm)
        self.windows.clear()
        user32.ReleaseDC(None, self.screen_dc)
        user32.UnregisterClassW(GLOW_CLASS, self.hinst)


def pump_messages():
    msg = wintypes.MSG()
    while user32.PeekMessageW(ctypes.byref(msg), None, 0, 0, PM_REMOVE):
        user32.TranslateMessage(ctypes.byref(msg))
        user32.DispatchMessageW(ctypes.byref(msg))


def restore_defaults():
    """Reload every cursor from the user's saved scheme."""
    user32.SystemParametersInfoW(SPI_SETCURSORS, 0, None, 2)   # SPIF_SENDCHANGE


# ----------------------------------------------------------------------------
# macOS plumbing
# ----------------------------------------------------------------------------
# The glow alone: macOS has no public system-wide cursor API, so the pointer is
# left as it is. Everything that touches AppKit here runs on the main thread.

_MAC_OK = False
if IS_MAC:
    try:
        from AppKit import (NSApp, NSApplication, NSColor, NSImage, NSImageView,
                            NSScreen, NSWindow)
        from Foundation import NSData, NSMakeRect, NSMakeSize, NSTimer
        from PyObjCTools import AppHelper
        _MAC_OK = True
    except Exception:
        pass            # no PyObjC: start() just reports False

    NSWindowStyleMaskBorderless = 0
    NSBackingStoreBuffered = 2
    NSImageScaleAxesIndependently = 1
    NSScreenSaverWindowLevel = 1000                 # over the menu bar and the Dock
    # Sharing "none" is how the mac tree (AutoCua/mac/tree/element.py,
    # _is_agent_glow) tells the glow apart from real windows: it must never
    # count as something covering the elements under the screen edges.
    NSWindowSharingNone = 0
    # CLI only. Accessory keeps the glow child out of the Dock. Prohibited
    # would too, but a framework Python (python.org, Homebrew) runs as a
    # bundled Python.app, and a Prohibited bundled app shows no windows at all.
    # Accessory has its own catch, handled in main(): app.run() would take
    # focus from the app the agent is driving. All measured.
    NSApplicationActivationPolicyAccessory = 1
    COLLECTION = (1 << 0) | (1 << 4) | (1 << 6) | (1 << 8)
    # canJoinAllSpaces | stationary | ignoresCycle | fullScreenAuxiliary


class MacScreenGlow:
    """
    The same edge glow on macOS, on every display.

    Four borderless click-through windows tile the border of each screen and
    leave the middle with nothing over it. The strips are cut from one field,
    so they meet without a seam, and they are rendered at backing-store
    resolution so they stay crisp on Retina. Breathing only sets alphaValue,
    which the compositor handles, so no pixels are redrawn.
    """

    def __init__(self, fraction=MAC_GLOW_FRACTION, opacity=MAC_GLOW_OPACITY):
        self.windows = []
        for screen in NSScreen.screens():
            self._cover(screen, fraction, opacity)

    def _cover(self, screen, fraction, opacity):
        frame = screen.frame()
        sx, sy = frame.origin.x, frame.origin.y
        sw, sh = frame.size.width, frame.size.height
        scale = float(screen.backingScaleFactor())

        short = min(sw, sh)
        wpx, hpx = int(round(sw * scale)), int(round(sh * scale))
        tpx = max(4, int(round(max(8.0, min(short * fraction, short / 2.0)) * scale)))

        # derive the point sizes back from the pixel sizes, so the strips the
        # windows ask for are exactly the strips that were cut from the field
        t = tpx / scale
        sw, sh = wpx / scale, hpx / scale

        field = build_edge_glow(wpx, hpx, tpx, opacity)
        # Cocoa's origin is bottom-left, so the PIL "top" crop goes at the top
        # of the screen rect, not at y = 0. Same order as edge_strips().
        rects = [
            (sx, sy + sh - t, sw, t),                     # top
            (sx, sy, sw, t),                              # bottom
            (sx, sy + t, t, sh - 2 * t),                  # left
            (sx + sw - t, sy + t, t, sh - 2 * t),         # right
        ]
        for crop, rect in zip(edge_strips(wpx, hpx, tpx), rects):
            self.windows.append(self._spawn(field.crop(crop), rect))

    def _spawn(self, img, rect):
        x, y, w, h = rect
        win = NSWindow.alloc().initWithContentRect_styleMask_backing_defer_(
            NSMakeRect(x, y, w, h), NSWindowStyleMaskBorderless,
            NSBackingStoreBuffered, False)
        win.setOpaque_(False)
        win.setBackgroundColor_(NSColor.clearColor())
        win.setIgnoresMouseEvents_(True)          # clicks pass straight through
        win.setHasShadow_(False)
        win.setLevel_(NSScreenSaverWindowLevel)
        win.setCollectionBehavior_(COLLECTION)
        win.setSharingType_(NSWindowSharingNone)  # the mac tree's marker, see above

        buf = io.BytesIO()
        img.save(buf, "PNG")
        raw = buf.getvalue()
        image = NSImage.alloc().initWithData_(NSData.dataWithBytes_length_(raw, len(raw)))
        image.setSize_(NSMakeSize(w, h))          # points; the bitmap stays at device pixels

        view = NSImageView.alloc().initWithFrame_(NSMakeRect(0, 0, w, h))
        view.setImage_(image)
        view.setImageScaling_(NSImageScaleAxesIndependently)
        win.setContentView_(view)
        win.orderFrontRegardless()
        return win

    def breathe(self, k):
        for win in self.windows:
            win.setAlphaValue_(k)

    def close(self):
        for win in self.windows:
            win.orderOut_(None)
        self.windows = []


_MAC = {}   # the live glow and its breathing timer; main thread only


def _mac_loop_running():
    """True when an NSApp event loop is running, i.e. the desktop app's."""
    try:
        return bool(NSApp.isRunning())
    except Exception:       # NSApp is nil: nothing has made an application
        return False


def _mac_on(fraction=MAC_GLOW_FRACTION, opacity=MAC_GLOW_OPACITY):
    """Main thread: put the glow up and start it breathing."""
    _mac_off()
    try:
        glow = MacScreenGlow(fraction, opacity)
    except Exception:
        return
    started = time.monotonic()

    def tick(_timer):
        k = 0.5 - 0.5 * math.cos(2 * math.pi * (time.monotonic() - started) / BREATH_SECONDS)
        glow.breathe(BREATH_LOW + (BREATH_HIGH - BREATH_LOW) * k)

    tick(None)
    _MAC["glow"] = glow
    _MAC["timer"] = NSTimer.scheduledTimerWithTimeInterval_repeats_block_(1 / 30.0, True, tick)


def _mac_off():
    """Main thread: stop the breathing and take the windows down."""
    timer, glow = _MAC.pop("timer", None), _MAC.pop("glow", None)
    if timer is not None:
        timer.invalidate()
    if glow is not None:
        glow.close()


# ----------------------------------------------------------------------------
# Linux plumbing — X11, and Xwayland under a Wayland session
#
# No toolkit. libX11, libXrender and libXfixes sit on every Linux desktop and
# ctypes reaches them from any interpreter, which sidesteps the problem
# PyGObject has here: it is a system package built for one Python version and
# a venv cannot see it. Under Wayland this talks to Xwayland, which
# composites override-redirect windows like any tooltip or menu — measured on
# GNOME 50 Wayland: the four strips paint over every window, and gnome-shell
# does not publish them in the window list the scanner reads, so element
# detection is untouched. The scanner filters them out by geometry anyway
# (glow_rects, below), so a shell that does publish them cannot start
# swallowing the elements along the screen edges.
#
# The cursor is a THEME here, not a per-frame swap: an Xcursor file carries
# its own frames and delays and the display server animates it. That is why
# Linux has no shrink-on-click feedback — nothing tells a theme that a button
# is down, and there is no equivalent of SetSystemCursor to swap per frame.
# The theme is installed when a run starts and removed when it ends.
# ----------------------------------------------------------------------------

# Layout confirmed against Adwaita's own cursor files: 16-byte header, version
# 0x00010000, a table of contents of (type, nominal size, offset), then 36-byte
# image chunks. Frames are grouped by nominal size, offsets ascend, and the
# pixels are premultiplied ARGB.
_XCUR_MAGIC = b"Xcur"
_XCUR_IMAGE = 0xfffd0002
CURSOR_SIZES = (24, 32, 48, 64, 96)          # the set Adwaita ships
THEME_NAME = "Lavender"
ARROW_NAMES = ["default", "left_ptr", "arrow", "top_left_arrow"]


class _XVisualInfo(ctypes.Structure):
    _fields_ = [("visual", ctypes.c_void_p), ("visualid", ctypes.c_ulong),
                ("screen", ctypes.c_int), ("depth", ctypes.c_int),
                ("cls", ctypes.c_int), ("red_mask", ctypes.c_ulong),
                ("green_mask", ctypes.c_ulong), ("blue_mask", ctypes.c_ulong),
                ("colormap_size", ctypes.c_int), ("bits_per_rgb", ctypes.c_int)]


class _XSetWindowAttributes(ctypes.Structure):
    _fields_ = [("background_pixmap", ctypes.c_ulong), ("background_pixel", ctypes.c_ulong),
                ("border_pixmap", ctypes.c_ulong), ("border_pixel", ctypes.c_ulong),
                ("bit_gravity", ctypes.c_int), ("win_gravity", ctypes.c_int),
                ("backing_store", ctypes.c_int), ("backing_planes", ctypes.c_ulong),
                ("backing_pixel", ctypes.c_ulong), ("save_under", ctypes.c_int),
                ("event_mask", ctypes.c_long), ("do_not_propagate_mask", ctypes.c_long),
                ("override_redirect", ctypes.c_int), ("colormap", ctypes.c_ulong),
                ("cursor", ctypes.c_ulong)]


class _XRenderColor(ctypes.Structure):
    _fields_ = [("red", ctypes.c_ushort), ("green", ctypes.c_ushort),
                ("blue", ctypes.c_ushort), ("alpha", ctypes.c_ushort)]


_CW_BACK_PIXEL, _CW_BORDER_PIXEL = 1 << 1, 1 << 3
_CW_OVERRIDE_REDIRECT, _CW_COLORMAP = 1 << 9, 1 << 13
_INPUT_OUTPUT, _TRUE_COLOR, _ZPIXMAP = 1, 4, 2
_SHAPE_INPUT, _PICT_OP_SRC, _PICT_STD_ARGB32 = 2, 1, 0


def _load_x11():
    """The three client libraries, with the signatures we rely on pinned down."""
    X = ctypes.CDLL("libX11.so.6")
    R = ctypes.CDLL("libXrender.so.1")
    F = ctypes.CDLL("libXfixes.so.3")
    P, U, I = ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int

    X.XOpenDisplay.restype = P
    X.XOpenDisplay.argtypes = [ctypes.c_char_p]
    X.XDefaultScreen.argtypes = [P]
    X.XRootWindow.restype = U
    X.XRootWindow.argtypes = [P, I]
    X.XDisplayWidth.argtypes = X.XDisplayHeight.argtypes = [P, I]
    X.XMatchVisualInfo.argtypes = [P, I, I, I, ctypes.POINTER(_XVisualInfo)]
    X.XCreateColormap.restype = U
    X.XCreateColormap.argtypes = [P, U, P, I]
    X.XCreateWindow.restype = U
    X.XCreateWindow.argtypes = [P, U, I, I, ctypes.c_uint, ctypes.c_uint, ctypes.c_uint,
                                I, ctypes.c_uint, P, U, ctypes.POINTER(_XSetWindowAttributes)]
    X.XCreatePixmap.restype = U
    X.XCreatePixmap.argtypes = [P, U, ctypes.c_uint, ctypes.c_uint, ctypes.c_uint]
    X.XCreateGC.restype = P
    X.XCreateGC.argtypes = [P, U, U, P]
    X.XFreeGC.argtypes = [P, P]
    X.XCreateImage.restype = P
    X.XCreateImage.argtypes = [P, P, ctypes.c_uint, I, I, ctypes.c_char_p,
                               ctypes.c_uint, ctypes.c_uint, I, I]
    X.XPutImage.argtypes = [P, U, P, P, I, I, I, I, ctypes.c_uint, ctypes.c_uint]
    X.XFree.argtypes = [P]
    X.XMapRaised.argtypes = X.XDestroyWindow.argtypes = [P, U]
    X.XFreePixmap.argtypes = [P, U]
    X.XFlush.argtypes = [P]
    X.XSync.argtypes = [P, I]
    X.XPending.argtypes = [P]
    X.XNextEvent.argtypes = [P, P]
    X.XCloseDisplay.argtypes = [P]
    X.XInternAtom.restype = U
    X.XInternAtom.argtypes = [P, ctypes.c_char_p, I]
    X.XGetSelectionOwner.restype = U
    X.XGetSelectionOwner.argtypes = [P, U]

    R.XRenderFindStandardFormat.restype = P
    R.XRenderFindStandardFormat.argtypes = [P, I]
    R.XRenderCreatePicture.restype = U
    R.XRenderCreatePicture.argtypes = [P, U, P, U, P]
    R.XRenderCreateSolidFill.restype = U
    R.XRenderCreateSolidFill.argtypes = [P, ctypes.POINTER(_XRenderColor)]
    R.XRenderComposite.argtypes = [P, I, U, U, U, I, I, I, I, I, I,
                                   ctypes.c_uint, ctypes.c_uint]
    R.XRenderFreePicture.argtypes = [P, U]

    F.XFixesCreateRegion.restype = U
    F.XFixesCreateRegion.argtypes = [P, P, I]
    F.XFixesSetWindowShapeRegion.argtypes = [P, U, I, I, I, U]
    F.XFixesDestroyRegion.argtypes = [P, U]
    return X, R, F


def _glow_blocker():
    """Why the glow cannot run on this machine, or None."""
    if not os.environ.get("DISPLAY"):
        if os.environ.get("XDG_SESSION_TYPE") == "wayland":
            return ("Wayland session with no Xwayland (DISPLAY is unset). "
                    "Nothing can place an overlay here.")
        return "DISPLAY is not set; is this a graphical session?"
    try:
        _load_x11()
    except OSError as exc:
        return f"X11 client libraries not found ({exc})"
    return None


class LinuxScreenGlow:
    """Four click-through override-redirect windows tiling the screen border.

    Each strip's pixels go to the server once, into a Pixmap. Breathing is one
    XRenderComposite per strip per frame against a solid-alpha mask, so the
    server does the work and the client never touches pixels again.
    """

    def __init__(self, fraction=GLOW_FRACTION, opacity=GLOW_OPACITY):
        self.X, self.R, self.F = _load_x11()
        X = self.X
        self.dpy = X.XOpenDisplay(None)
        if not self.dpy:
            raise RuntimeError(f"cannot open display {os.environ.get('DISPLAY')!r}")
        self.scr = X.XDefaultScreen(self.dpy)
        self.root = X.XRootWindow(self.dpy, self.scr)
        self.sw = X.XDisplayWidth(self.dpy, self.scr)
        self.sh = X.XDisplayHeight(self.dpy, self.scr)

        wayland = os.environ.get("XDG_SESSION_TYPE") == "wayland"
        atom = X.XInternAtom(self.dpy, f"_NET_WM_CM_S{self.scr}".encode(), 0)
        if not wayland and not X.XGetSelectionOwner(self.dpy, atom):
            X.XCloseDisplay(self.dpy)
            raise RuntimeError("no compositing manager is running, so a translucent "
                               "window would show as a black bar")

        self.vi = _XVisualInfo()
        if not X.XMatchVisualInfo(self.dpy, self.scr, 32, _TRUE_COLOR, ctypes.byref(self.vi)):
            X.XCloseDisplay(self.dpy)
            raise RuntimeError("no 32-bit ARGB visual on this display")
        self.cmap = X.XCreateColormap(self.dpy, self.root, self.vi.visual, 0)
        self.fmt = self.R.XRenderFindStandardFormat(self.dpy, _PICT_STD_ARGB32)

        short = min(self.sw, self.sh)
        self.t = max(8, min(int(short * fraction), short // 2))
        field = build_edge_glow(self.sw, self.sh, self.t, opacity)
        self.strips, self.rects = [], []
        for box in edge_strips(self.sw, self.sh, self.t):
            self.strips.append(self._spawn(field.crop(box), box[0], box[1]))
            self.rects.append((box[0], box[1], box[2] - box[0], box[3] - box[1]))
        del field
        X.XFlush(self.dpy)
        self.level = -1

    def _spawn(self, img, x, y):
        X, R, F = self.X, self.R, self.F
        w, h = img.size
        attrs = _XSetWindowAttributes()
        attrs.override_redirect = 1              # the window manager never touches it
        attrs.colormap = self.cmap
        attrs.border_pixel = 0
        attrs.background_pixel = 0
        win = X.XCreateWindow(self.dpy, self.root, x, y, w, h, 0, 32, _INPUT_OUTPUT,
                              self.vi.visual,
                              _CW_OVERRIDE_REDIRECT | _CW_COLORMAP | _CW_BORDER_PIXEL |
                              _CW_BACK_PIXEL, ctypes.byref(attrs))

        region = F.XFixesCreateRegion(self.dpy, None, 0)      # empty = clicks pass through
        F.XFixesSetWindowShapeRegion(self.dpy, win, _SHAPE_INPUT, 0, 0, region)
        F.XFixesDestroyRegion(self.dpy, region)

        pix = X.XCreatePixmap(self.dpy, win, w, h, 32)
        gc = X.XCreateGC(self.dpy, pix, 0, None)
        data = _premultiplied_bgra(img)          # ARGB32 little-endian is B,G,R,A in memory
        buf = ctypes.create_string_buffer(data, len(data))
        ximg = X.XCreateImage(self.dpy, self.vi.visual, 32, _ZPIXMAP, 0, buf, w, h, 32, 0)
        X.XPutImage(self.dpy, pix, gc, ximg, 0, 0, 0, 0, w, h)
        X.XFree(ximg)                 # frees the struct only; buf is ours
        X.XFreeGC(self.dpy, gc)

        src = R.XRenderCreatePicture(self.dpy, pix, self.fmt, 0, None)
        dst = R.XRenderCreatePicture(self.dpy, win, self.fmt, 0, None)
        X.XMapRaised(self.dpy, win)
        return (win, pix, src, dst, w, h)

    def breathe(self, k):
        a = max(0, min(255, int(round(k * 255))))
        if a == self.level:
            return
        self.level = a
        colour = _XRenderColor(0, 0, 0, a * 257)              # 0..65535
        mask = self.R.XRenderCreateSolidFill(self.dpy, ctypes.byref(colour))
        for win, pix, src, dst, w, h in self.strips:
            self.R.XRenderComposite(self.dpy, _PICT_OP_SRC, src, mask, dst,
                                    0, 0, 0, 0, 0, 0, w, h)
        self.R.XRenderFreePicture(self.dpy, mask)
        self.X.XFlush(self.dpy)

    def pump(self):
        """Drain the event queue so it cannot grow; we never act on events."""
        ev = (ctypes.c_long * 24)()
        while self.X.XPending(self.dpy):
            self.X.XNextEvent(self.dpy, ev)

    def close(self):
        for win, pix, src, dst, w, h in self.strips:
            self.R.XRenderFreePicture(self.dpy, src)
            self.R.XRenderFreePicture(self.dpy, dst)
            self.X.XFreePixmap(self.dpy, pix)
            self.X.XDestroyWindow(self.dpy, win)
        self.strips, self.rects = [], []
        self.X.XSync(self.dpy, 0)
        self.X.XCloseDisplay(self.dpy)


def _gsettings(*args):
    try:
        out = subprocess.run(["gsettings", *args], capture_output=True, text=True, timeout=5)
        return out.stdout.strip().strip("'") if out.returncode == 0 else None
    except (FileNotFoundError, subprocess.SubprocessError):
        return None


def _pack_xcursor(entries):
    """entries: (nominal, width, height, xhot, yhot, delay_ms, pixel_bytes)."""
    header = struct.pack("<4sIII", _XCUR_MAGIC, 16, 0x00010000, len(entries))
    pos = 16 + 12 * len(entries)
    toc, chunks = b"", b""
    for nominal, w, h, xhot, yhot, delay, px in entries:
        toc += struct.pack("<III", _XCUR_IMAGE, nominal, pos)
        chunk = struct.pack("<9I", 36, _XCUR_IMAGE, nominal, 1, w, h, xhot, yhot, delay) + px
        chunks += chunk
        pos += len(chunk)
    return header + toc + chunks


def build_cursor_theme():
    """Every size, every frame, in one animated Xcursor file."""
    delay = max(1, round(CYCLE_SECONDS * 1000 / FRAMES))
    entries = []
    for size in CURSOR_SIZES:                      # grouped by size, as themes do
        ss = max(1, round(256 / size))
        xhot, yhot = hotspot_for(size)
        for i in range(FRAMES):
            px = _premultiplied_bgra(render_frame(size, i / FRAMES, ss=ss))
            entries.append((size, size, size, xhot, yhot, delay, px))
    return _pack_xcursor(entries)


def _theme_dir():
    return os.path.join(os.path.expanduser("~"), ".icons", THEME_NAME)


def install_cursor_theme(inherit=None):
    """Write ~/.icons/Lavender and make it the session's cursor theme.

    The theme it replaces is remembered inside the theme folder, so
    remove_cursor_theme() can put it back even from another process — which
    is how a run killed with SIGKILL is cleaned up on the next start.
    """
    root = _theme_dir()
    cursors = os.path.join(root, "cursors")
    os.makedirs(cursors, exist_ok=True)

    current = _gsettings("get", "org.gnome.desktop.interface", "cursor-theme")
    if current == THEME_NAME:                      # never inherit from ourselves
        current = None
    base = inherit or current or "Adwaita"

    data = build_cursor_theme()
    target = os.path.join(cursors, ARROW_NAMES[0])
    with open(target, "wb") as fh:
        fh.write(data)
    for alias in ARROW_NAMES[1:]:                  # themes symlink the aliases
        link = os.path.join(cursors, alias)
        if os.path.lexists(link):
            os.remove(link)
        os.symlink(ARROW_NAMES[0], link)

    with open(os.path.join(root, "index.theme"), "w") as fh:
        fh.write("[Icon Theme]\n"
                 f"Name={THEME_NAME}\n"
                 "Comment=AutoCua agent pointer\n"
                 f"Inherits={base}\n")

    previous = _gsettings("get", "org.gnome.desktop.interface", "cursor-theme")
    if previous and previous != THEME_NAME:
        with open(os.path.join(root, ".previous-theme"), "w") as fh:
            fh.write(previous)
    _gsettings("set", "org.gnome.desktop.interface", "cursor-theme", THEME_NAME)
    return root, len(data), base


def remove_cursor_theme():
    """Put the user's cursor theme back and delete ours. Idempotent."""
    root = _theme_dir()
    prev_file = os.path.join(root, ".previous-theme")
    previous = None
    if os.path.exists(prev_file):
        try:
            previous = open(prev_file).read().strip()
        except OSError:
            previous = None
    if _gsettings("get", "org.gnome.desktop.interface", "cursor-theme") == THEME_NAME:
        if previous:
            _gsettings("set", "org.gnome.desktop.interface", "cursor-theme", previous)
        else:
            # reset restores whatever this distro ships as the default: Yaru on
            # Ubuntu, Adwaita on stock GNOME. Guessing one of them would be
            # wrong half the time.
            _gsettings("reset", "org.gnome.desktop.interface", "cursor-theme")
            previous = _gsettings("get", "org.gnome.desktop.interface", "cursor-theme")
    if os.path.isdir(root):
        shutil.rmtree(root, ignore_errors=True)
    return previous


def _clean_stale_cursor_theme():
    """A run killed with SIGKILL leaves the theme applied; undo that first."""
    if os.path.isdir(_theme_dir()) or _gsettings(
            "get", "org.gnome.desktop.interface", "cursor-theme") == THEME_NAME:
        remove_cursor_theme()


def _spawn_linux_watchdog():
    """A child that puts the cursor theme back if this process dies without
    calling stop(): a closed terminal (SIGHUP skips atexit), a kill -9, a
    crash. The X11 glow windows go with the process on their own; the cursor
    theme is a gsettings change and would outlive it — measured: the lavender
    pointer stayed after the agent's terminal was closed. The child gets its
    own session, so the terminal's SIGHUP does not reach it either, which is
    the point. Mirrors _spawn_mac_child."""
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    try:
        return subprocess.Popen(
            [sys.executable, "-m", "AutoCua.utils.agent_glow", "--parent", str(os.getpid())],
            cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True)
    except Exception:
        return None


def _run_linux(stop_ev, glow_on, cursor_on):
    """The Linux look, start to finish, on its own thread.

    The finally is the whole contract: whatever happens in here, the cursor
    theme goes back and the glow windows are destroyed. Building the cursor
    costs about a second, and it happens here rather than in start(), so an
    agent run never waits for it.
    """
    global _LINUX_RECTS
    glow = None
    cursor_installed = False
    watchdog = None
    try:
        if glow_on and _glow_blocker() is None:
            try:
                glow = LinuxScreenGlow()
                _LINUX_RECTS = tuple(glow.rects)   # the scanner reads this
            except Exception:
                glow = None     # a glow we cannot draw must not cost the cursor
        if cursor_on:
            try:
                _clean_stale_cursor_theme()
                install_cursor_theme()
                cursor_installed = True
            except Exception:
                cursor_installed = False
            if cursor_installed and not _COMPILED:
                watchdog = _spawn_linux_watchdog()

        while not stop_ev.is_set():
            if glow is None:
                stop_ev.wait(0.25)
                continue
            glow.pump()
            k = 0.5 - 0.5 * math.cos(2 * math.pi * time.perf_counter() / BREATH_SECONDS)
            glow.breathe(BREATH_LOW + (BREATH_HIGH - BREATH_LOW) * k)
            stop_ev.wait(1 / 30)
    except Exception:
        pass
    finally:
        _LINUX_RECTS = ()
        if glow is not None:
            try:
                glow.close()
            except Exception:
                pass
        if cursor_installed:
            try:
                remove_cursor_theme()
            except Exception:
                pass
        if watchdog is not None:            # theme is back; its job is done
            try:
                watchdog.terminate()
                watchdog.wait(timeout=2)
            except Exception:
                pass


# ----------------------------------------------------------------------------
# library API — what the agent calls
# ----------------------------------------------------------------------------
# main() below owns a foreground process and ends on Ctrl+C. An agent run has
# no foreground to give it, so start() runs the same loop on a daemon thread
# and stop() takes the desktop back.
#
# Everything Windows needs lives on THAT ONE thread: a window belongs to the
# thread that created it and only that thread may pump its messages, so the
# thread builds the glow itself. start() touches no Win32 at all and returns
# as soon as the thread is running. Building the cursors costs about a fifth
# of a second, and it happens on the thread, so an agent run never waits.

_LOCK = threading.Lock()
_STOP = None        # threading.Event of the running session, None when off
_THREAD = None
_MAC_ON = False     # macOS: glow handed to the main thread and not taken back
_MAC_PROC = None    # macOS terminal mode: the child process drawing the glow
_LINUX_RECTS = ()   # Linux: the live glow's strips, for the scanner to skip


def is_active():
    """True while the agent look is on."""
    with _LOCK:
        return _MAC_ON or (_THREAD is not None and _THREAD.is_alive())


def glow_rects():
    """The screen rectangles the glow covers right now, as (x, y, w, h).

    Empty whenever nothing is showing. The Linux scanner subtracts these from
    the window list it reads out of gnome-shell, so a strip can never be taken
    for a window covering a screen edge. That mistake cost macOS every element
    within a band of the edge until _is_agent_glow was added there; measured on
    GNOME 50 Wayland the strips do not reach that list at all, and this keeps
    it that way whatever a future shell decides to publish.

    Same-process only, which is all the scanner needs: the Linux glow runs on a
    thread of the process that calls start(), and that is the process the
    scanner runs in (both the launcher and the desktop app build the agent
    there). Windows and macOS identify their own glow windows natively.
    """
    return _LINUX_RECTS


def start(glow=True, cursor=True):
    """Turn the agent-active desktop look on. True if it took.

    Both halves by default, for exactly as long as the run lasts: the pointer
    becomes the animated lavender arrow AND the screen edges glow. macOS gets
    the glow alone and ignores cursor.

    Idempotent, and deliberately silent about failure: returns False on a
    platform with no implementation yet, or if it could not be applied.
    """
    global _STOP, _THREAD
    try:
        with _LOCK:
            if IS_MAC:
                return _start_mac(glow)
            if _THREAD is not None and _THREAD.is_alive():
                return True
            if not (IS_WIN or IS_LINUX):
                return False        # see ONE FILE, THREE PLATFORMS above
            stop_ev = threading.Event()
            thread = threading.Thread(target=_run_windows if IS_WIN else _run_linux,
                                      args=(stop_ev, glow, cursor),
                                      name="agent-glow", daemon=True)
            _STOP, _THREAD = stop_ev, thread
            thread.start()
            return True
    except Exception:
        return False


def stop(timeout=2.0):
    """Put the desktop back. Safe when nothing is running."""
    global _STOP, _THREAD
    if IS_MAC:
        _stop_mac()
        return
    try:
        with _LOCK:
            stop_ev, thread = _STOP, _THREAD
            _STOP = _THREAD = None
        if stop_ev is None:
            return
        stop_ev.set()
        if thread is not None and thread.is_alive():
            thread.join(timeout)
        # Belt and braces: if the thread never reached its own cleanup, the
        # session's cursors would still be ours. Reloading the user's scheme
        # is cheap and idempotent, so do it either way.
        if IS_WIN:
            try:
                restore_defaults()
            except Exception:
                pass
        elif IS_LINUX:
            # Same reasoning: if the thread never reached its own cleanup the
            # session would still be wearing our cursor theme. A no-op once
            # the thread has done it.
            try:
                _clean_stale_cursor_theme()
            except Exception:
                pass
    except Exception:
        pass


atexit.register(stop)   # a normal exit always hands the desktop back


def _run_windows(stop_ev, glow_on, cursor_on):
    """The Windows look, start to finish, on its own thread.

    The finally is the whole contract: whatever happens in here, the cursors
    go back and the glow windows are destroyed.
    """
    glow = None
    handles = []
    try:
        # Deliberately NOT SetProcessDPIAware() here, though the standalone CLI
        # below does call it. That flag is process-wide and permanent, and
        # turning it on mid-run would move the whole app's coordinate space —
        # the scanner's screenshots and every pyautogui / pywinauto click are
        # in whatever units it decides. Nothing else in the app sets it, so a
        # cosmetic cursor thread must not be the thing that does. The only cost
        # is that on a scaled display the cursor and glow are sized in
        # virtualised pixels, which Windows then scales for us.
        if glow_on:
            try:
                glow = ScreenGlow()
            except Exception:
                glow = None         # a glow we cannot draw must not cost the cursor

        targets = [OCR[k] for k in DEFAULT_TARGETS]
        n, zooms = FRAMES, CLICK_STEPS
        if cursor_on:
            size = user32.GetSystemMetrics(SM_CXCURSOR) or 32
            hotspot = hotspot_for(size)
            # handles[level][phase]; level 0 rests, the last level is pressed
            handles = [[make_hcursor(render_frame(size, i / n, zoom=z), hotspot)
                        for i in range(n)] for z in zooms]

        def apply(h):
            for ocr in targets:
                copy = user32.CopyIcon(h)
                if copy and not user32.SetSystemCursor(copy, ocr):
                    return False
            return True

        if handles and not apply(handles[0][0]):
            handles = []            # Windows refused it; leave the cursor alone

        def button_down():          # VK_LBUTTON / VK_RBUTTON, no hook needed
            return bool(user32.GetAsyncKeyState(1) & 0x8000) or \
                   bool(user32.GetAsyncKeyState(2) & 0x8000)

        interval = CYCLE_SECONDS / FRAMES
        top = len(zooms) - 1
        level, applied, next_step = 0, None, 0.0
        started = last_check = time.perf_counter()

        while not stop_ev.is_set():
            pump_messages()
            now = time.perf_counter()

            if glow is not None:
                if now - last_check > 2.0:   # docked, undocked, resolution changed
                    last_check = now
                    if (user32.GetSystemMetrics(0),
                            user32.GetSystemMetrics(1)) != (glow.sw, glow.sh):
                        glow.close()
                        glow = ScreenGlow()
                k = 0.5 - 0.5 * math.cos(2 * math.pi * now / BREATH_SECONDS)
                glow.breathe(BREATH_LOW + (BREATH_HIGH - BREATH_LOW) * k)

            if handles:
                target = top if button_down() else 0
                if level != target and now >= next_step:
                    level += 1 if target > level else -1
                    next_step = now + CLICK_STEP_MS / 1000.0
                phase = int((now - started) / interval) % n
                if (phase, level) != applied:
                    apply(handles[level][phase])
                    applied = (phase, level)

            stop_ev.wait(POLL_SECONDS)   # wait, not sleep, so stop() is instant
    except Exception:
        pass
    finally:
        try:
            restore_defaults()
        except Exception:
            pass
        for row in handles:
            for h in row:
                try:
                    user32.DestroyIcon(h)
                except Exception:
                    pass
        if glow is not None:
            try:
                glow.close()
            except Exception:
                pass


def _start_mac(glow):
    """The macOS half of start(), called with _LOCK held.

    AppKit windows must live on a main thread that runs an event loop. In the
    desktop app that is pywebview's, so the glow is handed to it. In terminal
    mode the agent holds the main thread and nothing runs a loop, so the glow
    goes to a child process instead: this module's CLI, which runs its own.
    """
    global _MAC_ON, _MAC_PROC
    if _MAC_ON:
        return True
    if not (_MAC_OK and glow):
        return False
    if _mac_loop_running():
        AppHelper.callAfter(_mac_on)
    elif _COMPILED:
        return False            # no `python -m` in the binary to start the child
    else:
        _MAC_PROC = _spawn_mac_child()
    _MAC_ON = True
    return True


def _spawn_mac_child():
    """Terminal mode: run the glow CLI below as a child, tied to this process.

    Its own session, so a Ctrl+C in the terminal reaches only the agent, whose
    finally then calls stop(). --parent makes it leave by itself if this
    process dies without doing that (a kill -9, say).
    """
    # <root>/AutoCua/utils/agent_glow.py -> <root>, so the child imports this
    # same copy of the package whatever directory the agent was started from.
    root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    return subprocess.Popen(
        [sys.executable, "-m", "AutoCua.utils.agent_glow", "--parent", str(os.getpid())],
        cwd=root, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL, start_new_session=True)


def _stop_mac():
    """The macOS half of stop(): take the glow down wherever it was put up."""
    global _MAC_ON, _MAC_PROC
    try:
        with _LOCK:
            was_on, proc = _MAC_ON, _MAC_PROC
            _MAC_ON, _MAC_PROC = False, None
        if proc is not None:
            proc.terminate()                # its windows go with the process
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
        elif was_on:
            AppHelper.callAfter(_mac_off)
    except Exception:
        pass


def main():
    ap = argparse.ArgumentParser(
        description="The lavender pointer and the breathing screen-edge glow.")
    ap.add_argument("--all", action="store_true",
                    help="replace every standard cursor, not just the arrow ones")
    ap.add_argument("--static", action="store_true", help="skip the colour animation")
    ap.add_argument("--no-click", action="store_true",
                    help="skip the shrink-on-click feedback")
    ap.add_argument("--no-glow", action="store_true",
                    help="cursor only, no screen glow")
    ap.add_argument("--no-cursor", action="store_true",
                    help="screen glow only, leave the cursor alone")
    ap.add_argument("--glow-opacity", type=float, default=DEF_GLOW_OPACITY,
                    help=f"peak edge opacity 0-1 (default {DEF_GLOW_OPACITY})")
    ap.add_argument("--glow-size", type=float, default=DEF_GLOW_FRACTION,
                    help=f"border thickness as a fraction of the short screen edge (default {DEF_GLOW_FRACTION})")
    ap.add_argument("--preview-screen", metavar="FILE.png",
                    help="save a mock screen showing the glow, change nothing")
    ap.add_argument("--restore", action="store_true",
                    help="just put the default cursors back and exit")
    ap.add_argument("--size", type=int, default=0, help="cursor size in px (default: system)")
    ap.add_argument("--preview", metavar="FILE.png",
                    help="save a big PNG of the cursor instead of using it")
    # set by start() for the macOS terminal-mode child: exit once that pid is gone
    ap.add_argument("--parent", type=int, default=0, help=argparse.SUPPRESS)
    args = ap.parse_args()

    if args.preview:
        render_frame(320, 0.0, ss=2).save(args.preview)
        print(f"Saved {args.preview}")
        return

    if args.preview_screen:
        mock_screen(args.glow_size, args.glow_opacity).save(args.preview_screen)
        print(f"Saved {args.preview_screen}")
        return

    if IS_MAC and _MAC_OK:
        # The glow alone, since macOS never gets the cursor. This process owns
        # the main thread, so it runs the event loop itself.
        if args.restore or args.no_glow:
            print("Nothing to do on macOS: the cursor is never changed.")
            return
        app = NSApplication.sharedApplication()
        app.setActivationPolicy_(NSApplicationActivationPolicyAccessory)
        _mac_on(args.glow_size, args.glow_opacity)

        def quit_now(*_):
            print("\nGlow off.", flush=True)
            os._exit(0)                     # the windows go with the process

        # Python's default Ctrl+C never gets out of the event loop, so it gets
        # its own handler, which Python runs on the next breath tick.
        signal.signal(signal.SIGINT, quit_now)

        if args.parent:
            # Started by start() for a terminal-mode run. Once that process is
            # gone, however it went, this one is reparented and goes too.
            def watch(_timer):
                if os.getppid() != args.parent:
                    quit_now()

            NSTimer.scheduledTimerWithTimeInterval_repeats_block_(1.0, True, watch)

        print("Glowing. Press Ctrl+C to stop.", flush=True)
        # A console run loop rather than app.run(): run() finishes launching
        # the app, and a launched Accessory app takes focus. This loop still
        # fires the breathing and watchdog timers and draws the windows.
        AppHelper.runConsoleEventLoop(installInterrupt=False)
        return

    if IS_LINUX:
        if args.parent:
            # Started by _run_linux: outlive the agent, and when it is gone —
            # however it went — put the cursor theme back. On a normal end
            # stop() restores the theme itself and then ends this process,
            # so nothing is done twice.
            while os.getppid() == args.parent:
                time.sleep(1.0)
            remove_cursor_theme()
            return
        # Also the recovery path: --restore puts the cursor theme back after a
        # run whose watchdog was killed too.
        if args.restore:
            previous = remove_cursor_theme()
            print(f"Cursor theme set back to {previous or 'the system default'}.")
            print("Apps already running keep the old pointer until they restart.")
            return
        if args.no_glow and args.no_cursor:
            sys.exit("Nothing to do: --no-cursor and --no-glow together.")
        why = _glow_blocker()
        if why and not args.no_glow:
            print(f"Glow skipped: {why}")
        stop_ev = threading.Event()
        thread = threading.Thread(
            target=_run_linux, args=(stop_ev, not args.no_glow, not args.no_cursor),
            name="agent-glow", daemon=True)
        thread.start()
        print("Running. Press Ctrl+C to put everything back.", flush=True)
        try:
            while thread.is_alive():
                time.sleep(0.2)
        except KeyboardInterrupt:
            pass
        stop_ev.set()
        thread.join(5)
        print("\nBack to normal.")
        return

    if not IS_WIN:
        sys.exit("This one only runs on Windows, macOS and Linux.")

    if args.restore:
        restore_defaults()
        print("Default cursors restored.")
        return

    user32.SetProcessDPIAware()

    glow = None
    if not args.no_glow:
        glow = ScreenGlow(args.glow_size, args.glow_opacity)
        print(f"Edge glow: {glow.t}px border on a {glow.sw}x{glow.sh} screen.")
    if args.no_cursor:
        if glow is None:
            sys.exit("Nothing to do: --no-cursor and --no-glow together.")
        atexit.register(glow.close)
        print("Breathing. Press Ctrl+C to stop.")
        try:
            while True:
                pump_messages()
                t = time.perf_counter()
                k = 0.5 - 0.5 * math.cos(2 * math.pi * t / BREATH_SECONDS)
                glow.breathe(BREATH_LOW + (BREATH_HIGH - BREATH_LOW) * k)
                time.sleep(POLL_SECONDS)
        except KeyboardInterrupt:
            pass
        finally:
            glow.close()
            print("\nGlow off.")
        return

    size = args.size or user32.GetSystemMetrics(SM_CXCURSOR) or 32
    targets = [OCR[k] for k in (OCR if args.all else DEFAULT_TARGETS)]
    hotspot = hotspot_for(size)

    n = 1 if args.static else FRAMES
    zooms = [1.0] if args.no_click else CLICK_STEPS
    print(f"Drawing {n * len(zooms)} frame(s) at {size}x{size}...")
    # handles[level][phase]; level 0 is resting, the last level is fully pressed
    handles = [[make_hcursor(render_frame(size, i / n, zoom=z), hotspot) for i in range(n)]
               for z in zooms]

    def cleanup():
        restore_defaults()
        for row in handles:
            for h in row:
                user32.DestroyIcon(h)
        handles.clear()
        if glow is not None:
            glow.close()

    atexit.register(cleanup)

    handler_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.DWORD)

    def on_ctrl(_):          # console X button, logoff, shutdown
        cleanup()
        return False

    keep_alive = handler_type(on_ctrl)
    kernel32.SetConsoleCtrlHandler(keep_alive, True)

    def apply(h):
        for ocr in targets:
            copy = user32.CopyIcon(h)
            if copy and not user32.SetSystemCursor(copy, ocr):
                return False
        return True

    if not apply(handles[0][0]):
        print("Windows refused the cursor change:", ctypes.WinError(ctypes.get_last_error()))
        return

    def button_down():       # VK_LBUTTON / VK_RBUTTON, works without a hook
        return bool(user32.GetAsyncKeyState(1) & 0x8000) or \
               bool(user32.GetAsyncKeyState(2) & 0x8000)

    print("Lavender cursor is on. Press Ctrl+C to put the normal one back.")
    interval = CYCLE_SECONDS / FRAMES
    top = len(zooms) - 1
    level, applied, next_step = 0, None, 0.0
    start = last_check = time.perf_counter()
    try:
        while True:
            pump_messages()
            now = time.perf_counter()

            if glow is not None:
                if now - last_check > 2.0:      # docked, undocked, resolution changed
                    last_check = now
                    if (user32.GetSystemMetrics(0), user32.GetSystemMetrics(1)) != (glow.sw, glow.sh):
                        glow.close()
                        glow = ScreenGlow(args.glow_size, args.glow_opacity)
                k = 0.5 - 0.5 * math.cos(2 * math.pi * now / BREATH_SECONDS)
                glow.breathe(BREATH_LOW + (BREATH_HIGH - BREATH_LOW) * k)

            target = top if button_down() else 0
            if level != target and now >= next_step:
                level += 1 if target > level else -1
                next_step = now + CLICK_STEP_MS / 1000.0

            phase = 0 if args.static else int((now - start) / interval) % n
            if (phase, level) != applied:
                apply(handles[level][phase])
                applied = (phase, level)

            time.sleep(POLL_SECONDS)
    except KeyboardInterrupt:
        pass
    finally:
        cleanup()
        print("\nBack to normal.")


if __name__ == "__main__":
    main()