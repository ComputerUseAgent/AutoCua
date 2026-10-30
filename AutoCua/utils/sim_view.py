"""Our own window for the phone runs: iOS simulators, in place of
Simulator.app, and Android emulators, which draw no window of their own.

Simulator.app opens one window per booted simulator, and a parallel run ends
up with them stacked on top of each other. Instead the simulators run headless
and this one window shows them all: a single iPhone or iPad sits in the
middle, two stand side by side, more line up in a row (rows, when there are
many). It opens the moment the run starts ("Preparing simulation" over the
looping background video) and the devices appear once they are ready.

Each screen is WebDriverAgent's MJPEG stream on that simulator's own port
(SimulatorSession.mjpeg_port). The window only ever reads those picture
streams and never touches WDA's control port, which matters: a new WDA
session kills the agent's.

It runs as its own process, because pywebview needs a main thread and the
launcher's is busy running the agent. The launcher talks to it over stdin:

    window = open_view()                  # right away: "Preparing simulation"
    show_phones(window, [{"name": "iPhone 17 Pro", "port": 9100, "task": 1}, ...])
    close_view(window)                    # or the launcher exits: stdin closes

A real, cable-connected iPhone or iPad uses the same window with its own
words: open_view(text="iPhone connected", title=<device name>).
"""

import html
import http.server
import json
import os
import re
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

_PAGE = Path(__file__).resolve().parent / "sim_view.html"
_VIDEO = Path(__file__).resolve().parents[1] / "logo" / "simulation_background.mp4"
# The folder holding the AutoCua package (a checkout's root, or site-packages
# for a pip install), so `-m AutoCua...` imports whatever the cwd is.
_PACKAGE_PARENT = Path(__file__).resolve().parents[2]
# The waiting words and the window title. The launcher sets them for a real
# phone (open_view); a simulator run leaves them alone.
_TEXT = os.environ.get("AutoCua_VIEW_TEXT") or "Preparing simulation"
_TITLE = os.environ.get("AutoCua_VIEW_TITLE") or "iOS Simulator"
# "iphone" or "ipad": one real phone, so a small window just around it
# (sim_view.html's compact layout). Empty for a simulator run.
_DEVICE = (os.environ.get("AutoCua_VIEW_DEVICE") or "").strip().lower()
# The compact window: the device (width / height of the screen plus its bezel,
# portrait) and the page's compact padding around it: room at the top for the
# window buttons, some gradient on each side, the name underneath. Keep in
# step with sim_view.html (PAD_X, PAD_Y + LABEL). macOS takes the title bar's
# height off the window it was asked for, so that is added back.
_COMPACT_SHAPE = {"iphone": 0.476, "ipad": 0.76}
_COMPACT_PAD_X, _COMPACT_PAD_Y = 140, 86
_TITLE_BAR = 28
# The simulator window: the desktop app's own size (ui/service.py: 1140 of
# width, the 900 of content plus the 240 left bar, by 700), so the two look
# like one set, capped to the screen.
_APP_WINDOW = (1140, 700)
# The video's own colour: the window's background, so it never flashes black
# or white before the page is up.
_BACKGROUND = "#b7c3f4"


def open_view(text=None, title=None, device=None):
    """Open the window ("Preparing simulation") and return its process.

    text and title replace "Preparing simulation" and "iOS Simulator" (a real
    phone: "iPhone connected" and its name). device ("iphone" or "ipad")
    makes it the small window of one real phone, sized just around it.

    Returns None if it could not start: the window is only for watching, and
    a run must never fail because of it.
    """
    env = os.environ.copy()
    env["PYTHONPATH"] = str(_PACKAGE_PARENT) + os.pathsep + env.get("PYTHONPATH", "")
    for key in ("AutoCua_VIEW_TEXT", "AutoCua_VIEW_TITLE", "AutoCua_VIEW_DEVICE"):
        env.pop(key, None)
    if text:
        env["AutoCua_VIEW_TEXT"] = str(text)
    if title:
        env["AutoCua_VIEW_TITLE"] = str(title)
    if device:
        env["AutoCua_VIEW_DEVICE"] = str(device)
    try:
        return subprocess.Popen(
            [sys.executable, "-m", "AutoCua.utils.sim_view"],
            env=env, stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            # Its own session, so Ctrl+C stops the run and not the window;
            # the run's cleanup closes it (close_view).
            start_new_session=True)
    except Exception as e:
        print(f"📱 Could not open the simulator window: {e}")
        return None


def show_phones(proc, phones):
    """Replace "Preparing simulation" with these devices.

    phones: [{"name", "port" (MJPEG), "task" (optional number)}, ...]
    """
    if proc is None:
        return
    try:
        proc.stdin.write((json.dumps(phones) + "\n").encode("utf-8"))
        proc.stdin.flush()
    except Exception:
        pass  # the user closed the window: nothing to show them in


def close_view(proc):
    if proc is None:
        return
    try:
        proc.stdin.close()  # the window's cue to close
        proc.wait(timeout=5)
    except Exception:
        try:
            proc.kill()
            proc.wait(timeout=5)
        except Exception:
            pass


# -- the window process --

class _Files(http.server.BaseHTTPRequestHandler):
    """The page and its background video. WebKit won't play a file:// video
    in a page loaded from a string, and won't play one from a server that
    can't answer byte ranges, hence this."""

    def log_message(self, *args):
        pass

    def do_GET(self):
        try:
            if self.path == "/":
                page = (_PAGE.read_text(encoding="utf-8")
                        .replace("<h1>Preparing simulation</h1>", f"<h1>{html.escape(_TEXT)}</h1>")
                        .replace("<title>iOS Simulator</title>", f"<title>{html.escape(_TITLE)}</title>"))
                if _DEVICE:
                    page = page.replace("<body>", '<body class="compact">', 1)
                body = page.encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif self.path == "/background.mp4" and _VIDEO.exists():
                self._video()
            else:
                self.send_error(404)
        except (ConnectionError, OSError):
            pass  # WebKit hangs up on video requests it no longer needs

    def _video(self):
        size = _VIDEO.stat().st_size
        start, end = 0, size - 1
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", self.headers.get("Range", "").strip())
        if m and (m[1] or m[2]):
            if m[1]:
                start = int(m[1])
                end = min(int(m[2]), size - 1) if m[2] else size - 1
            else:
                start = max(size - int(m[2]), 0)  # "bytes=-N": the last N
            if start > end:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.end_headers()
                return
            self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        else:
            self.send_response(200)
        self.send_header("Content-Type", "video/mp4")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        self.end_headers()
        with open(_VIDEO, "rb") as f:
            f.seek(start)
            left = end - start + 1
            while left > 0:
                chunk = f.read(min(1 << 16, left))
                if not chunk:
                    break
                self.wfile.write(chunk)
                left -= len(chunk)


def _logical_screen(screens):
    """The screen in the points windows are sized in. Windows reports it in
    physical pixels, and pywebview then scales a window's size by the
    display's DPI once more, so a size taken from that screen came out 1.5x
    too big there (2560 px wide at 150 %: the window filled the screen).
    macOS and GTK report points already."""
    try:
        width, height = screens[0].width, screens[0].height
    except Exception:
        return 1440, 900
    if sys.platform == "win32":
        try:
            import ctypes
            scale = ctypes.windll.user32.GetDpiForSystem() / 96
            width, height = int(width / scale), int(height / scale)
        except Exception:
            pass
    return width, height


def _window_size(screens):
    """The desktop app's window size (_APP_WINDOW), kept inside the screen.
    One real phone (_DEVICE): portrait, just around the device, about half
    the screen tall."""
    screen_w, screen_h = _logical_screen(screens)
    if _DEVICE:
        device_h = min(int(screen_h * 0.56), 640)
        shape = _COMPACT_SHAPE.get(_DEVICE, _COMPACT_SHAPE["iphone"])
        return (int(device_h * shape) + _COMPACT_PAD_X,
                device_h + _COMPACT_PAD_Y + _TITLE_BAR)
    width, height = _APP_WINDOW
    fit = min(screen_w * 0.85 / width, screen_h * 0.85 / height, 1.0)
    return int(width * fit), int(height * fit)


def _watch_stream(window, index, port):
    """Tell the page when simulator `index`'s screen stream is up, and when it
    has gone (WDA died, or restarted).

    Holds its own connection to the stream, since that's the only place a
    drop shows: the page's <img> gets no event when a live stream stops.
    It reads every frame (dropping them): a client that stops reading would
    make WDA queue frames for it."""
    up = False
    while True:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=3) as s:
                s.sendall(b"GET / HTTP/1.0\r\n\r\n")
                s.settimeout(None)  # a static screen can go quiet for a while
                if s.recv(65536):
                    up = True
                    window.evaluate_js(f"streamUp({index})")
                    while s.recv(65536):
                        pass
        except OSError:
            pass
        except Exception:
            return  # the window is gone
        if up:
            up = False
            try:
                window.evaluate_js(f"streamDown({index})")
            except Exception:
                return
        time.sleep(1.5)


def _listen(window):
    """The launcher's messages: a line of devices to show, then EOF (the run
    is over, or the launcher died) to close."""
    for line in sys.stdin.buffer:
        try:
            phones = json.loads(line)
            window.evaluate_js(f"showPhones({json.dumps(phones)})")
        except Exception:
            continue
        for i, phone in enumerate(phones):
            threading.Thread(target=_watch_stream, args=(window, i, phone["port"]),
                             daemon=True).start()
    window.destroy()


def _see_through_titlebar(window):
    """Run the page, video and all, up under a transparent title bar; the
    window buttons stay. pywebview has no option for it, so set it on the
    NSWindow (on the main thread, as AppKit wants)."""
    try:
        import AppKit
        from PyObjCTools import AppHelper
    except Exception:
        return

    def apply():
        ns = window.native
        if ns is None:
            return
        ns.setStyleMask_(ns.styleMask() | AppKit.NSWindowStyleMaskFullSizeContentView)
        ns.setTitlebarAppearsTransparent_(True)
        ns.setTitleVisibility_(AppKit.NSWindowTitleHidden)
        # pywebview paints the title bar's container with the window colour,
        # which would still cover the top of the page: clear it.
        for view in ns.contentView().superview().subviews():
            if "Titlebar" in view.className() and view.respondsToSelector_("setBackgroundColor:"):
                view.setBackgroundColor_(AppKit.NSColor.clearColor())

    AppHelper.callAfter(apply)


# Windows and Linux have no see-through title bar to offer, so there the window
# has none at all (frameless) and the page runs to its very top, as it does on
# the Mac; the window's three buttons are drawn by the page where the system
# puts them (sim_view.html, .winbtns), and the strip they sit in drags the
# window (pywebview's drag region). Windows gets its sizing frame back too
# (_sizing_frame), and Linux its moving and sizing (_gtk_frame).
_FRAMELESS = sys.platform != "darwin"
# Linux, where pywebview runs on GTK.
_GTK = sys.platform.startswith("linux")


class _Buttons:
    """The page's window buttons, as pywebview's js_api: the page calls
    pywebview.api.minimize(), .maximize() (a toggle) and .close()."""

    def __init__(self):
        # Private on purpose: pywebview offers every public attribute of a
        # js_api object to the page, and would walk the whole native window.
        self._window = None
        self._maximized = False

    def minimize(self):
        self._window.minimize()

    def maximize(self):
        if _GTK:
            # pywebview's restore() on GTK only brings back a minimized
            # window, never a maximized one: GTK's own state decides.
            from gi.repository import GLib
            GLib.idle_add(_toggle_maximized, self._window.native)
            return
        if self._maximized:
            self._window.restore()
        else:
            self._window.maximize()
        self._maximized = not self._maximized

    def close(self):
        self._window.destroy()


# The subclass procedure of _sizing_frame, kept so the callback outlives it.
_KEEP = []


def _sizing_frame(window):
    """Windows: pywebview's frameless window is a bare one (FormBorderStyle
    None), which cannot be resized from its edges. Windows' own sizing frame
    gives that back: the frame stays (WS_THICKFRAME: edges and corners
    resize, Aero Snap works), only the caption goes, and WM_NCCALCSIZE puts
    the client area at the very top, so the page runs under where the bar
    was. Maximized, the frame hangs off the screen, and the default inset
    stays so the page starts at the screen's edge. Called once the window is
    shown, when its handle exists; nothing happens elsewhere."""
    if sys.platform != "win32":
        return
    try:
        import ctypes
        from ctypes import wintypes
        user32, comctl = ctypes.windll.user32, ctypes.windll.comctl32
        LRESULT = ctypes.c_ssize_t
        SUBCLASSPROC = ctypes.WINFUNCTYPE(LRESULT, wintypes.HWND, wintypes.UINT, wintypes.WPARAM,
                                          wintypes.LPARAM, ctypes.c_size_t, ctypes.c_size_t)
        comctl.DefSubclassProc.restype = LRESULT
        comctl.DefSubclassProc.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
        user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
        user32.SetWindowLongPtrW.restype = ctypes.c_ssize_t
        user32.SetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int, ctypes.c_ssize_t]
        WM_NCDESTROY, WM_NCCALCSIZE = 0x82, 0x83
        WS_CAPTION, WS_THICKFRAME = 0x00C00000, 0x00040000
        WS_MINIMIZEBOX, WS_MAXIMIZEBOX = 0x00020000, 0x00010000

        @SUBCLASSPROC
        def proc(hwnd, msg, wparam, lparam, _uid, _ref):
            if msg == WM_NCDESTROY:
                # The window is going: step out of its message chain first,
                # or Windows calls back into a procedure that is being torn
                # down with the interpreter (an access violation at exit).
                comctl.RemoveWindowSubclass(hwnd, proc, 1)
                return comctl.DefSubclassProc(hwnd, msg, wparam, lparam)
            if msg == WM_NCCALCSIZE and wparam:
                # rgrc[0]: the window's rect on the way in, the client's on the way out.
                rect = ctypes.cast(lparam, ctypes.POINTER(wintypes.RECT)).contents
                top = rect.top
                comctl.DefSubclassProc(hwnd, msg, wparam, lparam)       # the frame's inset, every side
                if not user32.IsZoomed(hwnd):
                    rect.top = top                                        # none at the top
                return 0
            return comctl.DefSubclassProc(hwnd, msg, wparam, lparam)

        def apply():
            hwnd = int(window.native.Handle.ToInt32())
            _KEEP.append(proc)
            # Refused from any thread but the window's own (hence Invoke):
            # then the frame stays off too, or its top would show as a bar.
            if not comctl.SetWindowSubclass(hwnd, proc, 1, 0):
                return
            style = user32.GetWindowLongPtrW(hwnd, -16)                       # GWL_STYLE
            user32.SetWindowLongPtrW(hwnd, -16, (style & ~WS_CAPTION) | WS_THICKFRAME
                                     | WS_MINIMIZEBOX | WS_MAXIMIZEBOX)
            # SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE
            user32.SetWindowPos(hwnd, 0, 0, 0, 0, 0, 0x0020 | 0x0002 | 0x0001 | 0x0004 | 0x0010)

        import System                      # pythonnet, loaded by pywebview's WinForms backend
        form = window.native
        if form.InvokeRequired:
            form.Invoke(System.Action(apply))
        else:
            apply()
    except Exception:
        pass


def _toggle_maximized(native):
    """GTK, on its main thread: maximize the window, or give it back its size.
    False, so as a GLib.idle_add callback it runs once."""
    if native.is_maximized():
        native.unmaximize()
    else:
        native.maximize()
    return False


def _gtk_frame(window):
    """Linux: what a title bar and its edges would do for the frameless
    window, asked of the compositor the way GTK's own title bar asks
    (begin_move_drag, begin_resize_drag). pywebview moves a frameless window
    with gtk_window_move, which Wayland ignores (a client cannot place its
    windows, as banner.py measured), and an undecorated window has no edges
    to size it by.

    A press anywhere but on the page's window buttons drags the window once
    it has moved past GTK's drag threshold (Windows' easy drag does the
    same); a double click on the top strip maximizes and restores; the outer
    EDGE pixels size it, with the matching cursor. None of those presses
    reach the page. Called once the window is shown; nothing happens
    elsewhere."""
    if not _GTK:
        return
    try:
        from gi.repository import Gdk, GLib, Gtk, WebKit2
        native = window.native
        EDGE, CORNER = 6, 16            # the sizing band, and how far along it a corner reaches
        # The page's window buttons (sim_view.html .winbtns: three of 46 x
        # 32) stay the page's; the strip (.titlebar) is 30 high.
        BUTTONS_W, BUTTONS_H, STRIP = 3 * 46, 32, 30
        E = Gdk.WindowEdge
        SIDES = ((E.NORTH_WEST, E.NORTH, E.NORTH_EAST),
                 (E.WEST, None, E.EAST),
                 (E.SOUTH_WEST, E.SOUTH, E.SOUTH_EAST))
        CURSORS = {E.NORTH_WEST: "nw-resize", E.NORTH: "n-resize", E.NORTH_EAST: "ne-resize",
                   E.WEST: "w-resize", E.EAST: "e-resize",
                   E.SOUTH_WEST: "sw-resize", E.SOUTH: "s-resize", E.SOUTH_EAST: "se-resize"}
        press = {}          # the press a drag starts from, until it moves or ends
        showing = [None]    # the edge whose cursor is up

        def on_buttons(view, event):
            return event.x >= view.get_allocated_width() - BUTTONS_W and event.y < BUTTONS_H

        def edge_at(view, event):
            if native.is_maximized() or on_buttons(view, event):
                return None
            w, h = view.get_allocated_width(), view.get_allocated_height()
            if EDGE <= event.x < w - EDGE and EDGE <= event.y < h - EDGE:
                return None
            row = 0 if event.y < CORNER else 2 if event.y >= h - CORNER else 1
            col = 0 if event.x < CORNER else 2 if event.x >= w - CORNER else 1
            return SIDES[row][col]

        def on_press(view, event):
            if event.button != 1 or on_buttons(view, event):
                return False
            press.clear()
            if event.type == Gdk.EventType._2BUTTON_PRESS:
                if event.y < STRIP:
                    _toggle_maximized(native)
            elif event.type == Gdk.EventType.BUTTON_PRESS:
                side = edge_at(view, event)
                if side is not None:
                    native.begin_resize_drag(side, event.button, int(event.x_root), int(event.y_root),
                                             event.time)
                else:
                    press.update(button=event.button, x=event.x_root, y=event.y_root)
            return True

        def on_motion(view, event):
            if press and event.state & Gdk.ModifierType.BUTTON1_MASK:
                # Past the threshold, so a click that wobbles is still a click.
                if max(abs(event.x_root - press["x"]), abs(event.y_root - press["y"])) \
                        > Gtk.Settings.get_default().props.gtk_dnd_drag_threshold:
                    native.begin_move_drag(press["button"], int(press["x"]), int(press["y"]), event.time)
                    press.clear()
                return True
            press.clear()       # no button down: its release went elsewhere
            side = edge_at(view, event)
            if side != showing[0] and view.get_window() is not None:
                showing[0] = side
                view.get_window().set_cursor(
                    Gdk.Cursor.new_from_name(view.get_display(), CURSORS[side]) if side is not None else None)
            # Kept from the page on an edge, or WebKit would put its own cursor back.
            return side is not None

        def on_release(view, event):
            held = bool(press)
            press.clear()
            return held

        def apply():
            view = native.get_child()
            while view is not None and not isinstance(view, WebKit2.WebView):
                view = view.get_child() if isinstance(view, Gtk.Bin) else None
            if view is not None:
                # Ahead of WebKit's own handling (it runs last), so returning
                # True keeps the event from the page.
                view.connect("button-press-event", on_press)
                view.connect("button-release-event", on_release)
                view.connect("motion-notify-event", on_motion)
            return False

        GLib.idle_add(apply)        # GTK only from its main thread; this one is pywebview's event thread
    except Exception:
        pass


def _main():
    if _GTK:
        # pywebview's GTK backend runs `import gi`, and gi is a system package
        # a venv cannot see: without it pywebview finds no GUI at all and this
        # process ended before any window showed. The Linux scanner puts it on
        # sys.path when imported (element._add_system_gi_to_path), as
        # ui/service.py does. A scanner that then fails (no Atspi typelib) has
        # still added gi, which is all the window needs.
        try:
            import gi  # noqa: F401
        except ImportError:
            try:
                import importlib
                importlib.import_module("AutoCua.linux.tree.element")
            except Exception:
                pass
    import webview

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Files)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()

    width, height = _window_size(webview.screens)
    buttons = _Buttons()
    # No easy drag on GTK: it moves the window itself (which Wayland ignores),
    # and would miss the release of a press _gtk_frame handed to the
    # compositor, then move the window at the next mouse move (X11).
    window = webview.create_window(
        _TITLE, url=f"http://127.0.0.1:{server.server_address[1]}/",
        width=width, height=height, min_size=(200, 300) if _DEVICE else (520, 380),
        background_color=_BACKGROUND, frameless=_FRAMELESS, easy_drag=not _GTK, js_api=buttons)
    buttons._window = window

    # WKWebView won't autoplay a video without a user gesture, muted or not,
    # and pywebview has no setting for it; script run from here counts as one.
    # frameless() shows the page's window buttons where there is no title bar.
    window.events.loaded += lambda: window.evaluate_js(
        "startBackground()" + ("; frameless()" if _FRAMELESS else ""))
    window.events.before_show += lambda: _see_through_titlebar(window)
    window.events.shown += lambda: _sizing_frame(window)
    window.events.shown += lambda: _gtk_frame(window)

    # Daemon threads, not webview.start(func): pywebview runs func on a
    # normal thread, and Python would wait on it after the user closes the
    # window, leaving a frozen app in the Dock until the run ends.
    threading.Thread(target=_listen, args=(window,), daemon=True).start()
    webview.start()
    # The window is gone, and so is everything this process was for. Closed
    # by its button (or the Mac's red one) rather than by the launcher, the
    # _listen thread is still blocked on stdin, and a thread stuck in a read
    # while the interpreter shuts down crashes it on Windows (an access
    # violation at exit). Nothing here needs flushing: out at once.
    os._exit(0)


if __name__ == "__main__":
    _main()
