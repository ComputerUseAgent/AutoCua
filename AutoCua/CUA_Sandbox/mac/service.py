"""The Mac sandbox for the app: the second display the agent works on, while
the person keeps the main screen, the pointer and the keyboard.

The second display is Mac_CUA.rs beside this file, one standalone program.
This file starts it once, in its --serve mode, and passes the agent's orders
on over its pipes: one short method per thing asked of it, as the Windows
sandbox has (CUA_Sandbox/windows/service.py). A tool of the Mac controller
that works on the second display calls `sandbox()` and one of these methods.
So far: `launch`, `click` (the left button), `typewrite`, `set_text` and
`scan`. The main screen stays the default: nothing here runs unless a run was
asked for the sandbox.

`SandboxScanner` is the agent loop's scanner in such a run: the scan and the
picture of the second display, in the form the Mac scan gives for the
person's screen. It also turns on the live view: every new picture of the
second display goes to the app's screen box, with the agent's mouse in it.

The program is built from Mac_CUA.rs with rustc when it is missing or older
than its source (about 2 seconds). It ends when this app ends, with every app
it started on the second display.

Two runs of the same program file at once get no pictures (Mac_CUA.rs says
why), so the app holds one Sandbox and every tool shares it.
"""

import atexit
import base64
import io
import json
import queue
import shutil
import subprocess
import threading
import time
from pathlib import Path

from PIL import Image

from ...mac.tree.element import ELEMENT_CONFIG, Rect, UIElementScanner

_HERE = Path(__file__).resolve().parent
_SOURCE = _HERE / "Mac_CUA.rs"
_PROGRAM = _HERE / "build" / "Mac_CUA"

# Seconds an order may take before it counts as lost. `run` waits for the
# app's first window inside the program (up to 10 s there).
_START_WAIT = 20
_RUN_WAIT = 40
_CLICK_WAIT = 15
_TYPE_WAIT = 90
_SCAN_WAIT = 30
_FIRST_PICTURE_WAIT = 5

# The live view: a picture for the app's screen box is made no wider or higher
# than _LIVE_EDGE pixels, at this JPEG quality, and two of them are at least
# _LIVE_PAUSE seconds apart. Kept small on purpose: a picture reaches the app's
# window as one script on the one stream that carries every update of the UI
# (ui/service.py, DesktopWindow), and the page runs them in order. At full
# size (270 KB of script each, 4 a second while an app opens) the tool chain
# waited behind them and the UI stuttered (seen by the owner, 2026-11-28).
_LIVE_EDGE = 960
_LIVE_QUALITY = 70
_LIVE_PAUSE = 0.4


class SandboxError(RuntimeError):
    """The second display could not do what was asked. The message says why,
    in words the model can read in a tool result."""


def _build():
    """The program, built when it is missing or older than its source."""
    if _PROGRAM.exists() and _PROGRAM.stat().st_mtime >= _SOURCE.stat().st_mtime:
        return
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / "rustc")
    if not Path(rustc).exists():
        raise SandboxError("the second display is not built and rustc was not found to build it")
    _PROGRAM.parent.mkdir(exist_ok=True)
    built = subprocess.run([rustc, "--edition", "2021", "-O", str(_SOURCE), "-o", str(_PROGRAM)],
                           capture_output=True, text=True)
    if built.returncode != 0:
        raise SandboxError(f"the second display did not build: {built.stderr.strip()[-400:]}")


class Sandbox:
    """The second display and what can be asked of it."""

    def __init__(self):
        _build()
        self.place = None             # (x, y, width, height): where it is, in the points all screens share
        self._frame = None            # its newest picture, a BMP file's bytes
        self._said = []               # the last lines the program wrote for a person (its stderr)
        self._answers = queue.Queue() # the answers to the orders, in order
        self._placed = threading.Event()
        self._order = threading.Lock()  # one order at a time: an answer belongs to the order before it
        self._show = None             # the live view: what takes each new picture, or None
        self._new = threading.Event() # a picture came that the live view has not shown
        self._program = subprocess.Popen([str(_PROGRAM), "--serve"], stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._listen, daemon=True).start()
        threading.Thread(target=self._live, daemon=True).start()
        if not self._placed.wait(_START_WAIT):
            why = " ".join(self._said[-3:]) or "it said nothing"
            self.stop()
            raise SandboxError(f"the second display did not start: {why}")

    def _read(self):
        """The program's messages: a line "<kind> <bytes>", then that many bytes."""
        out = self._program.stdout
        while True:
            head = out.readline()
            if not head:
                break
            try:
                kind, size = head.decode("ascii").split()
                data = out.read(int(size))
            except ValueError:
                break
            if kind == "frame":
                self._frame = data
                self._new.set()
            elif kind == "answer":
                self._answers.put(data.decode("utf-8", "replace"))
            elif kind == "place":
                try:
                    self.place = tuple(float(n) for n in data.decode("ascii").split())
                except ValueError:
                    self.place = None
                self._placed.set()
            elif kind == "note":
                self._said.append(data.decode("utf-8", "replace"))
        self._answers.put("error: the second display has ended")

    def _listen(self):
        """What the program says for a person, kept for an error's words."""
        for line in self._program.stderr:
            self._said.append(line.decode("utf-8", "replace").strip())
            del self._said[:-20]

    def _live(self):
        """The live view: each new picture goes to whoever watches, as the
        base64 JPEG the app's screen box takes. Only the newest is sent: one
        that came while another was on its way is skipped, so the view can
        fall behind by a picture but never queues them up."""
        shown = None
        while self.alive():
            self._new.wait(1.0)
            self._new.clear()
            frame, show = self._frame, self._show
            if show is None or frame is None or frame is shown:
                continue
            shown = frame
            try:
                picture = Image.open(io.BytesIO(frame)).convert("RGB")
                shrink = _LIVE_EDGE / max(picture.size)
                if shrink < 1.0:
                    picture = picture.resize((max(1, int(picture.width * shrink)), max(1, int(picture.height * shrink))),
                                             Image.Resampling.BILINEAR)
                out = io.BytesIO()
                picture.save(out, format="JPEG", quality=_LIVE_QUALITY)
                show(base64.b64encode(out.getvalue()).decode("ascii"))
            except Exception:
                pass  # a picture that could not be shown: the next one is tried
            time.sleep(_LIVE_PAUSE)

    def watch(self, show):
        """Turn the live view on: `show` gets every new picture of the second
        display as a base64 JPEG, the newest one right away. None turns it
        off."""
        self._show = show
        self._new.set()

    def alive(self):
        return self._program.poll() is None

    def _ask(self, order, wait):
        """One order and its answer: the words after "ok", or SandboxError."""
        with self._order:
            if not self.alive():
                raise SandboxError("the second display has ended")
            while not self._answers.empty():
                self._answers.get_nowait()
            try:
                self._program.stdin.write((order + "\n").encode("utf-8"))
                self._program.stdin.flush()
                answer = self._answers.get(timeout=wait)
            except (OSError, queue.Empty):
                raise SandboxError(f"the second display did not answer: {order.split()[0]}") from None
        if answer.startswith("ok"):
            return answer[2:].strip()
        raise SandboxError(answer.removeprefix("error:").strip())

    def launch(self, app):
        """Start an app on the second display, by its name or the path of its
        .app. Gives its process id."""
        return int(self._ask(f"run {app}", _RUN_WAIT).split()[1])

    def click(self, x, y, clicks=1):
        """`clicks` clicks of the left button at a point of the second
        display, in the points all screens share (what a scan's rect holds).
        The person's pointer does not move and nothing comes in front."""
        return self._ask(f"click {float(x)} {float(y)} {int(clicks)}", _CLICK_WAIT)

    def typewrite(self, text):
        """Type a text into the app the agent last acted on (the one it last
        clicked in, set a text in, or opened), at its caret. It rides on key
        events handed to that app: the person's keyboard is not used. A line
        end is the Return key."""
        return self._ask("type " + str(text).encode("utf-8").hex(), _TYPE_WAIT)

    def set_text(self, number, text):
        """Put a text into element `number` of the last scan, in place of what
        is in it. The element is clicked first, so the app's keyboard is in
        it afterwards."""
        return self._ask(f"set {int(number)} " + str(text).encode("utf-8").hex(), _TYPE_WAIT)

    def scan(self):
        """What there is to click or type in on the second display:
        {"windows": [...], "elements": [...]}, as Mac_CUA.rs's served_scan
        gives them. Every box is [x, y, width, height] in the points all
        screens share."""
        return json.loads(self._ask("scan", _SCAN_WAIT))

    def shot(self):
        """The newest picture of the second display, as a BMP file's bytes.
        None before the first one."""
        return self._frame

    def picture(self, wait=_FIRST_PICTURE_WAIT):
        """The newest picture of the second display as an image. The first one
        comes a moment after the display starts: it is waited for, up to
        `wait` seconds. None when there is still none then."""
        late = time.time() + wait
        while self._frame is None and self.alive() and time.time() < late:
            time.sleep(0.05)
        frame = self._frame
        return Image.open(io.BytesIO(frame)).convert("RGB") if frame else None

    def stop(self):
        """End the second display and every app on it."""
        self._show = None
        try:
            self._program.stdin.close()
            self._program.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            self._program.kill()


_one = None
_making = threading.Lock()


def sandbox():
    """The app's one Sandbox: made on first use, and again when it has ended."""
    global _one
    with _making:
        if _one is None or not _one.alive():
            _one = Sandbox()
        return _one


def stop():
    """End the second display, when there is one."""
    global _one
    with _making:
        if _one is not None:
            _one.stop()
            _one = None


atexit.register(stop)


class SandboxScanner(UIElementScanner):
    """The agent loop's scanner in a sandbox run: what can be clicked or typed
    in on the second display, and its picture, in the form the Mac scan
    (mac/tree/element.py) gives for the person's screen. The tree text and the
    numbered boxes come from that scanner's own code, unchanged.

    The elements are those the sandbox program finds in the apps it started:
    nothing of the person's session is asked, and no app is brought in front.
    An element holds no `ax_element`: a click on it goes by its place.

    The app's screen box does not get the scan's picture: it gets the second
    display live (`Sandbox.watch`), from the moment the scanner is made."""

    def __init__(self, config=None, frontend_callback=None):
        super().__init__(config or ELEMENT_CONFIG, frontend_callback=None)
        self.sandbox = sandbox()
        self.sandbox.watch(frontend_callback)

    def scan_elements(self):
        """Read the second display: its elements, then its picture."""
        found = self.sandbox.scan()
        x0, y0, wide, high = self.sandbox.place
        self.element_tree, self.menu_bar_tree, self.second_layer_tree = [], [], []
        self.elements_mapping, self.elements_to_draw, self.found_elements = {}, [], {}
        self.element_index = 0
        # The window in front, of the agent's own apps: the scan's top layer.
        top = next((window for window in found["windows"] if window["ours"] and not window["popup"]), None)
        self.top_layer_info = {"name": top["app"], "type": "app"} if top else None
        self.application_name = top["app"] if top else "Desktop"
        for element in found["elements"]:
            left, top_y, width, height = element["box"]
            rect = Rect(int(left), int(top_y), int(left + width), int(top_y + height))
            number = element["n"]
            self.element_tree.append({"index": number, "name": element["name"], "type": element["kind"],
                                      "active": "True", "visibility": "full", "children": [],
                                      "rect": rect, "visible_rect": rect})
            self.elements_mapping[str(number)] = {
                "element": None, "rect": rect, "visible_rect": rect, "name": element["name"],
                "aria_role": "", "type": element["kind"], "value": None, "visibility": "full",
                "clipped_by": None, "ax_element": None}
            self.elements_to_draw.append({"rect": rect, "index": number, "depth": 0, "visibility": "full",
                                          "source": ""})
            self.element_index = max(self.element_index, number)

        # The picture: the second display's own, with the numbered boxes drawn
        # on it by the Mac scanner's code. A picture has `scale` pixels a point.
        self._screenshot = self.sandbox.picture()
        self._annotate_prep = self._annotate_thread = None
        self._annotated_image_base64 = None
        if self._screenshot is None:
            return
        self._scale = self._screenshot.width / wide if wide else 1.0
        self._screen_size = (wide, high)
        screen = {"x": x0, "y": y0, "width": wide, "height": high, "scale": self._scale}
        self._annotate_canvas = self._make_annotation_canvas(screen)
        if self._annotate_canvas:
            self._draw_boxes(0)
            self._encode_annotation()
