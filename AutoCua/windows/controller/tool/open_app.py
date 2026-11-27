import os
import sys
import time
import ctypes
import subprocess
import shutil
import json
import xml.etree.ElementTree as ET
from pathlib import Path
from difflib import SequenceMatcher

import psutil
import win32gui
import win32con
import win32api
import win32process

# Windows shell/system window classes that never represent an app the user
# asked to open (same skip set as tree/element.py's get_topmost_app).
_SKIP_WINDOW_CLASSES = {
    "Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd",
    "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland",
    "Windows.UI.Core.CoreWindow", "XamlExplorerHostIslandWindow",
    "Microsoft.UI.Content.PopupWindowSiteBridge", "Xaml_WindowedPopupClass",
    "#32768", "tooltips_class32",
}
_SKIP_WINDOW_TITLES = {
    "program manager", "windows input experience",
}
# Given to every program started by its path, so Chromium-based apps show
# their elements to UI Automation.
ACCESSIBILITY_FLAG = "--force-renderer-accessibility"

def _is_window_cloaked(hwnd) -> bool:
    """DWM-cloaked windows (hidden UWP apps, other virtual desktops).
    Inline copy of tree/element.py's is_window_cloaked — importing that
    module would pull UIA COM initialization into this lightweight tool."""
    try:
        DWMWA_CLOAKED = 14
        cloaked = ctypes.c_int(0)
        ctypes.windll.dwmapi.DwmGetWindowAttribute(
            hwnd, DWMWA_CLOAKED, ctypes.byref(cloaked), ctypes.sizeof(cloaked)
        )
        return cloaked.value != 0
    except Exception:
        return False

def normalize(s: str) -> str:
    s = s.lower().strip()
    for ch in ("'", '"', ".", "_", "-", "(", ")", "[", "]", "{", "}", "®", "™", "&"):
        s = s.replace(ch, " ")
    return " ".join(s.split())

def best_match(query, candidates):
    """candidates: list of (display_name, target, norm_name)
       target is either a filesystem path or 'appx:<AppID>'"""
    qn = normalize(query)

    # exact
    for name, target, nn in candidates:
        if nn == qn:
            return name, target

    # contains
    cont = [(name, target) for name, target, nn in candidates if qn in nn or nn in qn]
    if cont:
        cont.sort(key=lambda x: len(x[0]))
        return cont[0]

    # fuzzy
    scored = []
    for name, target, nn in candidates:
        scored.append((SequenceMatcher(None, qn, nn).ratio(), name, target))
    scored.sort(reverse=True)
    if scored and scored[0][0] >= 0.6:
        return scored[0][1], scored[0][2]

    return None, None

def index_windows_start_menu():
    entries = []
    start_dirs = []
    if "ProgramData" in os.environ:
        start_dirs.append(Path(os.environ["ProgramData"]) / r"Microsoft\Windows\Start Menu\Programs")
    if "AppData" in os.environ:
        start_dirs.append(Path(os.environ["AppData"]) / r"Microsoft\Windows\Start Menu\Programs")

    exts = {".lnk", ".url", ".appref-ms"}
    seen = set()
    for root in start_dirs:
        if not root.exists():
            continue
        for p in root.rglob("*"):
            if p.is_file() and p.suffix.lower() in exts:
                name = p.stem
                nn = normalize(name)
                key = (nn, str(p))
                if key in seen:
                    continue
                seen.add(key)
                entries.append((name, str(p), nn))
    return entries

def index_windows_startapps():
    """Use PowerShell Get-StartApps to include UWP/Store apps (e.g., Spotify)."""
    try:
        cmd = [
            "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
            "Get-StartApps | Select-Object Name,AppID | ConvertTo-Json -Depth 2 -Compress"
        ]
        cp = subprocess.run(cmd, capture_output=True, text=True, timeout=8, creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
        if cp.returncode != 0 or not cp.stdout.strip():
            return []
        data = json.loads(cp.stdout)
        if isinstance(data, dict):
            data = [data]
        entries = []
        for item in data:
            name = str(item.get("Name", "")).strip()
            appid = str(item.get("AppID", "")).strip()
            if name and appid:
                entries.append((name, f"appx:{appid}", normalize(name)))
        return entries
    except Exception:
        return []

def _ps_quote(s: str) -> str:
    # PowerShell single-quote escape
    return "'" + s.replace("'", "''") + "'"

def resolve_lnk_target(lnk_path: str) -> str | None:
    """Resolve a .lnk shortcut to its target exe path via WScript.Shell."""
    try:
        cmd = [
            "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
            f"(New-Object -ComObject WScript.Shell).CreateShortcut({_ps_quote(lnk_path)}).TargetPath"
        ]
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=8, creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
        if r.returncode == 0 and r.stdout.strip():
            return r.stdout.strip()
    except Exception:
        pass
    return None

def launch_windows_target(name: str, target: str) -> bool:
    """Launch (maximized where possible) with accessibility flags."""
    # Universal accessibility flag for better UI automation
    accessibility_flag = ACCESSIBILITY_FLAG

    try:
        if target.startswith("appx:"):
            appid = target[5:]
            # UWP / Store app - can't add custom flags
            cmd = [
                "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
                "Start-Process " + _ps_quote("shell:AppsFolder\\" + appid) + " -WindowStyle Maximized"
            ]
            r = subprocess.run(cmd, capture_output=True, text=True, creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
            return r.returncode == 0
        else:
            # For .lnk shortcuts, we need to resolve the actual exe first
            if target.endswith('.lnk'):
                exe_path = resolve_lnk_target(target)
                if exe_path:
                    # Launch with accessibility flag
                    cmd = [
                        "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
                        f"Start-Process -FilePath {_ps_quote(exe_path)} -ArgumentList {_ps_quote(accessibility_flag)} -WindowStyle Maximized"
                    ]
                else:
                    # Fallback to launching the shortcut directly (no flag)
                    cmd = [
                        "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
                        f"Start-Process -FilePath {_ps_quote(target)} -WindowStyle Maximized"
                    ]
            else:
                # Direct exe/executable path - add accessibility flag
                cmd = [
                    "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
                    f"Start-Process -FilePath {_ps_quote(target)} -ArgumentList {_ps_quote(accessibility_flag)} -WindowStyle Maximized"
                ]
            
            r = subprocess.run(cmd, capture_output=True, text=True, creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
            return r.returncode == 0
    except Exception:
        return False

def _enum_app_windows() -> list:
    """Enumerate candidate app windows as (hwnd, title, class_name, pid),
    in Z-order (first = most recently used). Minimized windows are kept —
    they are the main reason this scan exists — so the cloak/size/position
    checks only apply to non-minimized windows (minimized ones park at
    -32000 and minimized UWP frames are DWM-cloaked)."""
    windows = []

    def cb(hwnd, _):
        try:
            if not win32gui.IsWindowVisible(hwnd):
                return True
            title = win32gui.GetWindowText(hwnd).strip()
            if not title or title.lower() in _SKIP_WINDOW_TITLES:
                return True
            class_name = win32gui.GetClassName(hwnd)
            if class_name in _SKIP_WINDOW_CLASSES:
                return True
            if not win32gui.IsIconic(hwnd):
                if _is_window_cloaked(hwnd):
                    return True
                left, top, right, bottom = win32gui.GetWindowRect(hwnd)
                if right - left < 100 or bottom - top < 100:
                    return True
                if left <= -30000 or top <= -30000:
                    return True
            _, pid = win32process.GetWindowThreadProcessId(hwnd)
            windows.append((hwnd, title, class_name, pid))
        except Exception:
            pass
        return True

    try:
        win32gui.EnumWindows(cb, None)
    except Exception:
        pass
    return windows

def find_running_window(name: str, target: str) -> int | None:
    """Find a window of an already-running instance of the matched app.
    Returns the hwnd (first in Z-order = most recently used) or None.

    exe/.lnk targets match by process exe name — title-independent, so
    "report.docx - Word" still matches. appx/UWP targets match by process
    name and window title instead: their windows are hosted by
    ApplicationFrameHost.exe, so exe matching is useless."""
    try:
        windows = _enum_app_windows()
        if not windows:
            return None

        exe_name = None
        if not target.startswith("appx:"):
            if target.lower().endswith(".lnk"):
                resolved = resolve_lnk_target(target)
                exe_name = os.path.basename(resolved).lower() if resolved else None
            else:
                exe_name = os.path.basename(target).lower()

        if exe_name:
            for hwnd, title, class_name, pid in windows:
                try:
                    if psutil.Process(pid).name().lower() == exe_name:
                        return hwnd
                except Exception:
                    continue
            return None

        qn = normalize(name)
        if not qn:
            return None

        # Store apps that are really Win32 (Spotify, WhatsApp) keep the app
        # name as their process name — check that first, since their window
        # titles often don't contain the app name (e.g. Spotify shows the
        # playing track).
        compact = qn.replace(" ", "")
        for hwnd, title, class_name, pid in windows:
            try:
                proc = psutil.Process(pid).name().lower()
            except Exception:
                continue
            stem = proc[:-4] if proc.endswith(".exe") else proc
            if normalize(stem).replace(" ", "") == compact:
                return hwnd

        # Title matching. Exact title == app name is trusted for any window
        # class; containment and fuzzy are restricted to UWP frame windows —
        # a Chrome tab titled "calculator - Google Search" must not be
        # mistaken for the Calculator app. Fuzzy threshold is stricter than
        # best_match's 0.6: a false positive here focuses the wrong app.
        contains = []
        for hwnd, title, class_name, pid in windows:
            tn = normalize(title)
            if not tn:
                continue
            if tn == qn:
                return hwnd
            if class_name == "ApplicationFrameWindow" and (qn in tn or tn in qn):
                contains.append(hwnd)
        if contains:
            return contains[0]

        scored = []
        for hwnd, title, class_name, pid in windows:
            if class_name != "ApplicationFrameWindow":
                continue
            ratio = SequenceMatcher(None, qn, normalize(title)).ratio()
            if ratio >= 0.75:
                scored.append((ratio, hwnd))
        if scored:
            scored.sort(key=lambda x: -x[0])
            return scored[0][1]
    except Exception:
        pass
    return None

def focus_window(hwnd) -> bool:
    """Restore (if minimized) and bring an existing window to the foreground.
    Restore only — no forced maximize: an already-running window has a
    deliberate size, unlike the fresh-launch path which maximizes.
    SetForegroundWindow is restricted when the caller isn't the foreground
    process, so fall through the known workarounds, verifying after each."""
    def reached_foreground() -> bool:
        time.sleep(0.15)
        return win32gui.GetForegroundWindow() == hwnd

    try:
        if win32gui.IsIconic(hwnd):
            win32gui.ShowWindow(hwnd, win32con.SW_RESTORE)

        try:
            win32gui.SetForegroundWindow(hwnd)
        except Exception:
            pass
        if reached_foreground():
            return True

        # An ALT keypress lifts the foreground-lock restriction
        VK_MENU = 0x12
        try:
            win32api.keybd_event(VK_MENU, 0, 0, 0)
            win32gui.SetForegroundWindow(hwnd)
        except Exception:
            pass
        finally:
            win32api.keybd_event(VK_MENU, 0, win32con.KEYEVENTF_KEYUP, 0)
        if reached_foreground():
            return True

        # Attach to the current foreground thread's input queue
        try:
            fg_hwnd = win32gui.GetForegroundWindow()
            fg_thread = win32process.GetWindowThreadProcessId(fg_hwnd)[0] if fg_hwnd else 0
            cur_thread = win32api.GetCurrentThreadId()
            attached = False
            if fg_thread and fg_thread != cur_thread:
                attached = bool(ctypes.windll.user32.AttachThreadInput(cur_thread, fg_thread, True))
            try:
                win32gui.BringWindowToTop(hwnd)
                win32gui.SetForegroundWindow(hwnd)
            finally:
                if attached:
                    ctypes.windll.user32.AttachThreadInput(cur_thread, fg_thread, False)
        except Exception:
            pass
        if reached_foreground():
            return True

        # Last resort — emulates Alt-Tab switching
        try:
            ctypes.windll.user32.SwitchToThisWindow(hwnd, 1)
        except Exception:
            pass
        if reached_foreground():
            return True

        # Lenient pass: another window of the same app took focus (UWP
        # frames and multi-window apps do this) — the app is still in front.
        fg_now = win32gui.GetForegroundWindow()
        if fg_now and fg_now != hwnd:
            same_pid = (win32process.GetWindowThreadProcessId(fg_now)[1]
                        == win32process.GetWindowThreadProcessId(hwnd)[1])
            if same_pid:
                return True
    except Exception:
        pass
    return False

def find_app(app_name: str):
    """The app a name means: (display name, target) of the best match, or
    (None, None). The target is a file path or 'appx:<AppID>'."""
    # Build candidate list from Start Menu + StartApps
    candidates = index_windows_start_menu() + index_windows_startapps()

    # Add PATH executables as lightweight candidates
    exe = shutil.which(app_name)
    if exe:
        candidates.append((app_name, exe, normalize(app_name)))
    for token in app_name.split():
        exe = shutil.which(token)
        if exe:
            candidates.append((token, exe, normalize(token)))

    # De-duplicate by (norm_name, target)
    dedup = {}
    for name, target, nn in candidates:
        dedup[(nn, target)] = (name, target, nn)
    candidates = list(dedup.values())

    return best_match(app_name, candidates)

def open_on_windows(app_name: str) -> str | None:
    """Open an app, or focus it if it's already running.
    Returns "focused" (existing window brought to the foreground),
    "launched" (new instance started), or None (no matching app / launch
    failed). Truthy on success, so boolean callers keep working."""
    name, target = find_app(app_name)
    if not target:
        return None

    # Already running? Bring the existing window to the front instead of
    # spawning a duplicate instance.
    try:
        hwnd = find_running_window(name, target)
        if hwnd and focus_window(hwnd):
            return "focused"
    except Exception:
        pass

    return "launched" if launch_windows_target(name, target) else None

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
    tool). Started from the app it takes the app's own console, which has no
    window, so it would never get one on the second desktop (review of
    2026-11-25)."""
    shell32 = ctypes.WinDLL("shell32")
    shell32.SHGetFileInfoW.restype = ctypes.c_size_t
    shell32.SHGetFileInfoW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_void_p, ctypes.c_uint32,
                                       ctypes.c_uint32]
    info = ctypes.create_string_buffer(1024)   # a SHFILEINFOW is 696 bytes; SHGFI_EXETYPE leaves it as it is
    kind = shell32.SHGetFileInfoW(str(path), 0, info, len(info), 0x2000)   # SHGFI_EXETYPE
    # The low word says PE or MZ; the high word is the Windows version a GUI
    # program asks for, and 0 for a console one.
    return kind != 0 and (kind >> 16) & 0xFFFF == 0 and kind & 0xFFFF in (0x4550, 0x5A4D)

def open_in_sandbox(app_name: str) -> dict:
    """open_app in AutoCua SB: the same app, found the same way (find_app),
    started on the agent's own desktop (CUA_Sandbox/windows) by its program
    file. A Store app started through its Start menu id would be started by
    Windows on your desktop, so its program file is used too, and a Store app
    that has none is refused. So are, for now, a shortcut with no program
    file behind it (a web link, steam:): the shell hands it to whatever
    program handles it, which may be one already open on the user's desktop;
    and a console program, which gets no window there. Gives the tool's
    result."""
    from ....CUA_Sandbox.windows.service import sandbox, SandboxError

    def failed(message):
        return {"status": "error", "action": "tool", "tool": "open_app", "app": app_name, "message": message}

    _, target = find_app(app_name)
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
        program = resolve_lnk_target(target) if target.lower().endswith(".lnk") else target
        if not program or program.lower().endswith((".lnk", ".url", ".appref-ms")):
            return failed(f"'{app_name}' is a shortcut with no program file behind it (a web link, a game "
                          "launcher's link): it cannot start in AutoCua SB yet.")
        arguments = [ACCESSIBILITY_FLAG]
    if is_console_program(program):
        return failed(f"'{app_name}' is a console program: it cannot show a window in AutoCua SB yet.")

    try:
        opened = sandbox().launch(subprocess.list2cmdline([program] + arguments))
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