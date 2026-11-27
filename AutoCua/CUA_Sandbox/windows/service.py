"""AutoCua SB on Windows: the second desktop the computer-use agent works on
when the chat box's picker says AutoCua SB (ui/chat_input/sb_toggle.js).

The sandbox is Win_CUA.rs beside this file, run as Win_CUA.exe --device: it
makes the second desktop and answers one request at a time over its pipes
(device_loop in Win_CUA.rs). It is built with rustc into the system temp
folder, never beside the code, and started on the first AutoCua SB run. It
lives as long as this app: when the app ends, its pipe closes, and every app
on the second desktop ends with it. Stop stops the agent, not the sandbox.

What the agent has there so far:
  - its eyes: SandboxScanner, the agent loop's scanner in an AutoCua SB run
    (computer/service.rs picks it), with the elements and the picture of the
    second desktop in the form the Windows scan gives them for your screen;
  - open_app (sandbox_action, which windows/controller/view.py sends the
    screen tools to). The other screen tools answer that they do not work in
    AutoCua SB yet, and never fall back to your mouse or keyboard.
"""
import ctypes
import io
import json
import shutil
import subprocess
import tempfile
import threading
import xml.etree.ElementTree as ET
from pathlib import Path

from PIL import Image

from ...windows.tree.element import ELEMENT_CONFIG, Rect, UIElementScanner

HERE = Path(__file__).resolve().parent                # AutoCua\CUA_Sandbox\windows
ROOT = HERE.parents[2]                                # the checkout: the program runs here
SOURCE = HERE / "Win_CUA.rs"
# The built program is a cache outside the code, made again when it is
# missing or older than the source (as test.py does).
PROGRAM = Path(tempfile.gettempdir()) / "AutoCua" / "Win_CUA.exe"

# Explorer's own windows on the second desktop: not the app the agent works in.
SHELL_CLASSES = {"Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"}
# The flag the agent's open_app gives every program it starts by its path
# (windows/controller/tool/open_app.py), so Chromium-based apps show their
# elements to UI Automation.
ACCESSIBILITY_FLAG = "--force-renderer-accessibility"


class SandboxError(RuntimeError):
    """AutoCua SB could not do what was asked; the words say why."""


def build():
    """Compile Win_CUA.rs when there is no program yet or the source is newer."""
    if PROGRAM.exists() and PROGRAM.stat().st_mtime >= SOURCE.stat().st_mtime:
        return
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / "rustc.exe")
    if not Path(rustc).exists():
        raise SandboxError("AutoCua SB needs Rust to build its sandbox: run setup_windows.bat, "
                           "or install Rust from https://rustup.rs")
    PROGRAM.parent.mkdir(parents=True, exist_ok=True)
    made = subprocess.run([rustc, "--edition", "2021", "-O", str(SOURCE), "-o", str(PROGRAM)],
                          cwd=PROGRAM.parent, capture_output=True, text=True, errors="replace",
                          creationflags=subprocess.CREATE_NO_WINDOW)
    if made.returncode != 0:
        raise SandboxError("AutoCua SB's sandbox did not build:\n" + (made.stderr or made.stdout)[-2000:])


class Sandbox:
    """The running Win_CUA.exe --device, and the one request it answers at a time."""

    def __init__(self):
        build()
        # No console window for it, and its own stdin: an inherited one is the
        # app's pipe from Electron.
        self.program = subprocess.Popen([str(PROGRAM), "--device"], cwd=ROOT,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
        self.lock = threading.Lock()
        self.screen = self._reply()   # where its picture sits on the screen: x0, y0, width, height

    def alive(self):
        return self.program.poll() is None

    def _reply(self):
        """The next reply: a line "<kind> <bytes>", then that many bytes."""
        out = self.program.stdout
        while True:
            head = out.readline().split()
            if len(head) != 2:
                raise SandboxError("AutoCua SB's sandbox stopped")
            kind, data = head[0], out.read(int(head[1]))
            if kind in (b"ok", b"ready"):
                return json.loads(data)
            if kind == b"bmp":
                return data
            if kind == b"err":
                raise SandboxError(data.decode("utf-8", "replace"))
            # anything else is no reply: read on

    def ask(self, request):
        """One request (a line, see device_loop in Win_CUA.rs) and its reply."""
        with self.lock:
            try:
                self.program.stdin.write(request.encode("utf-8") + b"\n")
                self.program.stdin.flush()
            except OSError:
                raise SandboxError("AutoCua SB's sandbox stopped") from None
            return self._reply()


_one = None
_one_lock = threading.Lock()


def sandbox():
    """This app's sandbox, started on first use, and again if it ended."""
    global _one
    with _one_lock:
        if _one is None or not _one.alive():
            _one = Sandbox()
        return _one


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
        found = self.sandbox.ask("scan")
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
        self._picture = Image.open(io.BytesIO(self.sandbox.ask("shot"))).convert("RGB")

    def _grab_screen(self):
        """The picture get_scan_data draws on: the second desktop's, taken by the scan."""
        if self._picture is None:
            self._picture = Image.open(io.BytesIO(self.sandbox.ask("shot"))).convert("RGB")
        return self._picture


def store_app_program(app_id):
    """The program file of a Store app, from its Start menu id (family!app).
    None when the app is not an ordinary Windows program in a package: that
    kind only runs on your own desktop."""
    family, _, app = app_id.partition("!")
    kernel32 = ctypes.WinDLL("kernel32")
    count, length = ctypes.c_uint32(0), ctypes.c_uint32(0)
    kernel32.GetPackagesByPackageFamily(family, ctypes.byref(count), None, ctypes.byref(length), None)
    if not count.value:
        return None
    names = (ctypes.c_wchar_p * count.value)()
    text = ctypes.create_unicode_buffer(length.value)
    if kernel32.GetPackagesByPackageFamily(family, ctypes.byref(count), names, ctypes.byref(length), text):
        return None
    for full_name in names:
        size = ctypes.c_uint32(0)
        kernel32.GetPackagePathByFullName(full_name, ctypes.byref(size), None)
        folder = ctypes.create_unicode_buffer(size.value)
        if kernel32.GetPackagePathByFullName(full_name, ctypes.byref(size), folder):
            continue
        try:
            root = ET.parse(Path(folder.value) / "AppxManifest.xml").getroot()
        except (OSError, ET.ParseError):
            continue
        for entry in root.iter():
            if entry.tag.endswith("}Application") and entry.get("Id") == app:
                ordinary = entry.get("EntryPoint") == "Windows.FullTrustApplication" or any(
                    key.endswith("}RuntimeBehavior") and value in ("packagedClassicApp", "win32App")
                    for key, value in entry.attrib.items())
                program = entry.get("Executable")
                return str(Path(folder.value) / program) if ordinary and program else None
    return None


def is_console_program(path):
    """Whether a program file is a console program (cmd, PowerShell, a CLI
    tool). On the second desktop it would share the sandbox's own hidden
    console and never get a window (review of 2026-11-25)."""
    shell32 = ctypes.WinDLL("shell32")
    shell32.SHGetFileInfoW.restype = ctypes.c_size_t
    shell32.SHGetFileInfoW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
                                       ctypes.c_uint32]
    info = ctypes.create_string_buffer(1024)   # a SHFILEINFOW is 696 bytes; SHGFI_EXETYPE leaves it as it is
    kind = shell32.SHGetFileInfoW(str(path), 0, info, len(info), 0x2000)   # SHGFI_EXETYPE
    # The low word says PE or MZ; the high word is the Windows version a GUI
    # program asks for, and 0 for a console one.
    return kind != 0 and (kind >> 16) & 0xFFFF == 0 and kind & 0xFFFF in (0x4550, 0x5A4D)


def open_app(app_name):
    """open_app in AutoCua SB: the app the agent's open_app would open (found
    the same way, windows/controller/tool/open_app.py), started on the second
    desktop by its own program file. A Store app started through its Start
    menu id would be started by Windows on your desktop, so its program file
    is used too, and a Store app that has none is refused. So are, for now,
    a shortcut with no program file behind it (a web link, steam:): the
    shell hands it to whatever program handles it, which may be one already
    open on the user's desktop; and a console program, which gets no window
    there."""
    from ...windows.controller.tool import open_app as tool

    def failed(message):
        return {"status": "error", "action": "tool", "tool": "open_app", "app": app_name, "message": message}

    candidates = tool.index_windows_start_menu() + tool.index_windows_startapps()
    for word in [app_name] + app_name.split():
        path = shutil.which(word)
        if path:
            candidates.append((word, path, tool.normalize(word)))
    candidates = list({(nn, target): (name, target, nn) for name, target, nn in candidates}.values())
    _, target = tool.best_match(app_name, candidates)
    if not target:
        return failed("No application found. Verify by typing the application name in the taskbar search. "
                      "If still not visible then download it or find an alternative.")

    if target.startswith("appx:"):
        program = store_app_program(target[5:])
        if not program:
            return failed(f"'{app_name}' is a kind of Store app that only runs on the user's own desktop, "
                          "not in AutoCua SB.")
        arguments = []
    else:
        program = tool.resolve_lnk_target(target) if target.lower().endswith(".lnk") else target
        if not program or program.lower().endswith((".lnk", ".url", ".appref-ms")):
            return failed(f"'{app_name}' is a shortcut with no program file behind it (a web link, a game "
                          "launcher's link): it cannot start in AutoCua SB yet.")
        arguments = [ACCESSIBILITY_FLAG]
    if is_console_program(program):
        return failed(f"'{app_name}' is a console program: it cannot show a window in AutoCua SB yet.")
    command = subprocess.list2cmdline([program] + arguments)

    try:
        opened = sandbox().ask("open " + command)
    except SandboxError as e:
        return failed(f"'{app_name}' could not be started in AutoCua SB: {e}")
    if opened["window"]:
        return {"status": "success", "action": "tool", "tool": "open_app", "app": app_name, "mode": "launched",
                "message": f"'{app_name}' opened in AutoCua SB: window '{opened['window']}'."}
    if opened["exited"]:
        return failed(f"'{app_name}' started and closed with no window in AutoCua SB. It may have handed over "
                      "to a copy that is open on the user's own desktop: ask the user to close it there.")
    return {"status": "success", "action": "tool", "tool": "open_app", "app": app_name, "mode": "launched",
            "message": f"'{app_name}' started in AutoCua SB, with no window yet: wait, then look again."}


# The screen tools of the Windows agent (windows/tool_registry/service.rs).
SCREEN_TOOLS = {"left_click", "right_click", "input", "typewrite", "scroll", "hotkey", "screenshot", "open_app"}
# Sub-agents that work on the user's own desktop: the coder's shell and the
# browser agent's browser.
DESKTOP_SUB_AGENTS = {"coder_agent", "browser_agent"}


def handles(action):
    """Whether an AutoCua SB run must send this tool call here: the screen
    tools, and the tools that start programs, which would open on the user's
    own screen. The shell's programs do (seen: `Start-Process charmap` from
    an AutoCua SB run opened Character Map on the user's screen), and a
    program started from a thread that joined the second desktop still opens
    on the user's (measured), so those wait until they are ported."""
    kind = action.get("type")
    return (kind in SCREEN_TOOLS or kind == "shell"
            or (kind == "sub_agent" and action.get("agent_type", "coder_agent") in DESKTOP_SUB_AGENTS))


def sandbox_action(action):
    """One tool call of an AutoCua SB run that `handles` says is ours
    (windows/controller/view.py sends them here). open_app works; the others
    are not in AutoCua SB yet and say so: they never fall back to the user's
    screen, mouse or keyboard."""
    kind = action.get("type")
    if kind == "open_app":
        return open_app(action.get("value") or "")
    if kind == "shell":
        message = ("shell does not work in AutoCua SB yet: the programs it starts would open on the user's "
                   "own screen. Use open_app to open an app.")
    elif kind == "sub_agent":
        message = (f"The {action.get('agent_type', 'coder_agent')} sub-agent does not work in AutoCua SB yet: it would work "
                   "on the user's own screen.")
    else:
        message = f"{kind} does not work in AutoCua SB yet. Only open_app works there so far."
    return {"status": "error", "action": kind, "message": message}
