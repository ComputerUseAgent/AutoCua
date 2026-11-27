"""AutoCua SB on Windows: the second desktop the computer-use agent works on
when the chat box's picker says AutoCua SB (ui/chat_input/sb_toggle.js).

The second desktop is kept by the app itself. Win_CUA.rs beside this file is
a part of the app's Rust module (AutoCua.agent_native.cua_sandbox): it makes
the second desktop and answers one request at a time (device_loop there). No
program is built or started for it: until 2026-11-27 one was, Win_CUA.exe in
the temp folder, and an antivirus took it for a threat. The desktop is made on
the first AutoCua SB run and lives as long as this app: when the app ends,
Windows ends every app on the second desktop with it. Stop stops the agent,
not the sandbox.

This file is the sandbox only: the second desktop (Sandbox) and the agent's
eyes on it (SandboxScanner, the agent loop's scanner in an AutoCua SB run:
computer/service.rs picks it). No tool is written here. Each tool of the
agent keeps its work where it lives, in windows/controller, and has a branch
there for AutoCua SB that asks this sandbox to do the same thing on the second
desktop: typewrite (controller/service.py), hotkey (controller/hotkey/
service.py) and open_app (controller/tool/open_app.py) so far. The tools with
no branch yet are refused in controller/view.py; none falls back to your
mouse or keyboard.
"""
import json
import threading

from PIL import Image

from ...windows.tree.element import ELEMENT_CONFIG, Rect, UIElementScanner

try:
    # The extension is already loaded and fresh: computer/__init__.py, which
    # imports this file, built and imported it first.
    from ...agent_native import cua_sandbox as _desktop
except ImportError as _error:
    # The extension is loaded but has no cua_sandbox part: it was built before
    # this code. An app that was open during an update keeps the one it loaded
    # when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'cua_sandbox' part (AutoCua SB's second desktop). Close every AutoCua "
        "window and start again.") from _error

# Explorer's own windows on the second desktop: not the app the agent works in.
SHELL_CLASSES = {"Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"}


class SandboxError(RuntimeError):
    """AutoCua SB could not do what was asked; the words say why."""


def _ask(function, *args):
    """One call to the second desktop (cua_sandbox in Win_CUA.rs), its failure in AutoCua SB's own error."""
    try:
        return function(*args)
    except RuntimeError as e:
        raise SandboxError(str(e)) from None


class Sandbox:
    """The second desktop: made on first use, and asked one thing at a time."""

    def __init__(self):
        self.screen = _ask(_desktop.start)   # where its picture sits on the screen: x0, y0, width, height

    def alive(self):
        return _desktop.running()

    def scan(self):
        """Its windows on show, top first, and what can be clicked or typed in (device_scan in Win_CUA.rs)."""
        return json.loads(_ask(_desktop.scan))

    def shot(self):
        """Its picture, as an RGB image."""
        size = (self.screen["width"], self.screen["height"])
        return Image.frombytes("RGB", size, _ask(_desktop.shot), "raw", "BGRX")

    def launch(self, command):
        """Start a command line there: {"pid", "window", "exited"} (device_open in Win_CUA.rs)."""
        return json.loads(_ask(_desktop.launch, command))

    def typewrite(self, text):
        """Type text into the app in front there, where its cursor is: {"window", "characters"} (device_type)."""
        return json.loads(_ask(_desktop.typewrite, text))

    def hotkey(self, keys):
        """Press a key or a combination in the app in front there: {"window", "keys"} (device_key)."""
        return json.loads(_ask(_desktop.hotkey, keys))


_one = None
_one_lock = threading.Lock()


def sandbox():
    """This app's sandbox, started on first use, and again if it ended."""
    global _one
    with _one_lock:
        if _one is None or not _one.alive():
            _one = Sandbox()
        return _one


def stop():
    """End every app on the second desktop and let it go. The app never calls
    this: its sandbox lives until the app itself ends. test.py beside this
    file does, when its window closes."""
    global _one
    with _one_lock:
        _one = None
        _desktop.stop()


def _app_window(window):
    """An app's own window, as Win_CUA.rs's is_app_window counts them."""
    left, top, right, bottom = window["box"]
    return bool(window["title"].strip() and right - left >= 100 and bottom - top >= 100
                and window["class"] not in SHELL_CLASSES)


class SandboxScanner(UIElementScanner):
    """The agent loop's scanner in an AutoCua SB run: what can be clicked or
    typed in on the second desktop, and its picture, in the same form the
    Windows scan (windows/tree/element.py) gives for your screen. The tree
    text, the numbered boxes, the JPEG for the model and the preview for the
    app's screen box all come from get_scan_data there, unchanged."""

    def __init__(self, config=None, frontend_callback=None):
        super().__init__(config or ELEMENT_CONFIG, frontend_callback=frontend_callback)
        self.sandbox = sandbox()
        self._picture = None

    def scan_elements(self):
        """Read the second desktop: its windows and elements, then its picture."""
        found = self.sandbox.scan()
        x0, y0 = self.sandbox.screen["x0"], self.sandbox.screen["y0"]
        self.top_layer_tree, self.second_layer_tree, self.taskbar_tree = [], [], []
        self.top_layer_info = self.second_layer_info = None
        self.elements_mapping, self.elements_to_draw = {}, []
        self.element_index = 0
        # It has no taskbar of its own: the tree says so instead of listing one.
        self.taskbar_visible = False
        self.taskbar_hidden_reason = "AutoCua SB: the agent's own desktop has no taskbar"

        windows = {window["hwnd"]: window for window in found["windows"]}
        top = next((window for window in found["windows"] if _app_window(window)), None)
        self.top_layer_info = {"name": top["title"], "type": "app", "hwnd": top["hwnd"]} if top else None
        self.application_name = top["title"] if top else "Desktop"
        for element in found["elements"]:
            left, top_y, right, bottom = element["box"]
            rect = Rect(left, top_y, right, bottom)
            item = {"index": element["n"], "name": element["name"], "type": element["kind"],
                    "active": "True", "visibility": "full", "children": [], "element": None,
                    "rect": rect, "visible_rect": rect}
            in_top = top is not None and element["window"] == top["hwnd"]
            if in_top or top is None:
                self.top_layer_tree.append(item)
            else:
                if self.second_layer_info is None:
                    other = windows.get(element["window"], {})
                    shell = other.get("class") in SHELL_CLASSES
                    self.second_layer_info = {"name": "Desktop" if shell else other.get("title") or "Desktop",
                                              "type": "app"}
                self.second_layer_tree.append(item)
            self.elements_mapping[str(element["n"])] = {
                "element": None, "rect": rect, "visible_rect": rect, "name": element["name"],
                "aria_role": "", "type": element["kind"], "value": None, "visibility": "full",
                "clipped_by": None}
            # The picture starts at (x0, y0) of the screen: the boxes are drawn on it.
            self.elements_to_draw.append({"rect": Rect(left - x0, top_y - y0, right - x0, bottom - y0),
                                          "index": element["n"], "depth": 0, "visibility": "full",
                                          "source": ""})
            self.element_index = max(self.element_index, element["n"])
        self._picture = self.sandbox.shot()

    def _grab_screen(self):
        """The picture get_scan_data draws on: the second desktop's, taken by the scan."""
        if self._picture is None:
            self._picture = self.sandbox.shot()
        return self._picture
