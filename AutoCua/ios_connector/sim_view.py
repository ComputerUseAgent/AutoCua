"""Our own window for iOS Simulator runs, in place of Simulator.app.

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
"""

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


def open_view():
    """Open the window ("Preparing simulation") and return its process.

    Returns None if it could not start: the window is only for watching, and
    a run must never fail because of it.
    """
    env = os.environ.copy()
    env["PYTHONPATH"] = str(_PACKAGE_PARENT) + os.pathsep + env.get("PYTHONPATH", "")
    try:
        return subprocess.Popen(
            [sys.executable, "-m", "AutoCua.ios_connector.sim_view"],
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
                body = _PAGE.read_bytes()
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


def _window_size(screens):
    """Landscape, about three quarters of the screen's width."""
    try:
        screen_w, screen_h = screens[0].width, screens[0].height
    except Exception:
        screen_w, screen_h = 1440, 900
    width = int(screen_w * 0.72)
    height = int(width / 1.5)
    if height > screen_h * 0.85:
        height = int(screen_h * 0.85)
        width = int(height * 1.5)
    return width, height


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


def _main():
    import webview

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _Files)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()

    width, height = _window_size(webview.screens)
    window = webview.create_window(
        "iOS Simulator", url=f"http://127.0.0.1:{server.server_address[1]}/",
        width=width, height=height, min_size=(520, 380),
        # The video's own colours, so the window never flashes black or white.
        background_color="#b7c3f4")

    # WKWebView won't autoplay a video without a user gesture, muted or not,
    # and pywebview has no setting for it; script run from here counts as one.
    window.events.loaded += lambda: window.evaluate_js("startBackground()")
    window.events.before_show += lambda: _see_through_titlebar(window)

    # Daemon threads, not webview.start(func): pywebview runs func on a
    # normal thread, and Python would wait on it after the user closes the
    # window, leaving a frozen app in the Dock until the run ends.
    threading.Thread(target=_listen, args=(window,), daemon=True).start()
    webview.start()


if __name__ == "__main__":
    _main()
