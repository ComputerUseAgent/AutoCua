# AutoCua/linux/controller/tool/open_app.py
# Linux version — open or bring-to-front any installed desktop application
# (anything with a .desktop entry: distro packages, snaps, flatpaks, the
# user's own launchers).

import os
import shlex
import shutil
import subprocess
import logging
import time
from difflib import SequenceMatcher
from pathlib import Path

logger = logging.getLogger(__name__)

# macOS / generic names a model may use for what this desktop calls
# something else. Values are queries run through the same matcher.
_ALIASES = {
    "finder": "files",
    "file manager": "files",
    "explorer": "files",
    "nautilus": "files",
    "iterm": "terminal",
    "iterm2": "terminal",
    "textedit": "text editor",
    "gedit": "text editor",
    "system preferences": "settings",
    "system settings": "settings",
    "control panel": "settings",
    "app store": "app center",
    "safari": "browser",
    "default browser": "browser",
    "web browser": "browser",
}
# Queries answered by the desktop's default web browser.
_BROWSER_QUERIES = {"browser", "web browser", "default browser"}
# How long a launcher may run before it counts as "the app is up" (see
# _launch): a failed launch exits at once, a successful one may never.
LAUNCH_SETTLE = 1.0


def _normalize(s: str) -> str:
    s = s.lower().strip()
    for ch in (".", "_", "-", "(", ")", "[", "]", "{", "}", "®", "™", "&", "'", '"'):
        s = s.replace(ch, " ")
    return " ".join(s.split())


def _app_dirs():
    """Every folder that can hold .desktop entries, most specific first, so
    a user launcher shadows the distro's. XDG_DATA_HOME / XDG_DATA_DIRS are
    the spec; snap and flatpak export theirs beside them."""
    home = Path.home()
    data_home = Path(os.environ.get("XDG_DATA_HOME") or home / ".local" / "share")
    data_dirs = os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share"
    dirs = [data_home / "applications"]
    dirs += [Path(d) / "applications" for d in data_dirs.split(":") if d]
    dirs += [
        Path("/var/lib/snapd/desktop/applications"),
        Path("/var/lib/flatpak/exports/share/applications"),
        data_home / "flatpak" / "exports" / "share" / "applications",
    ]
    seen, out = set(), []
    for d in dirs:
        key = str(d)
        if key not in seen:
            seen.add(key)
            out.append(d)
    return out


def _exec_basename(exec_line: str) -> str:
    """The program a .desktop Exec line runs, without field codes or a
    leading `env VAR=value`."""
    try:
        parts = shlex.split(exec_line)
    except ValueError:
        parts = exec_line.split()
    while parts and (parts[0] == "env" or ("=" in parts[0] and not parts[0].startswith("/"))):
        parts = parts[1:]
    return os.path.basename(parts[0]) if parts else ""


def _parse_desktop(path: Path):
    """The launchable [Desktop Entry] of one .desktop file, or None."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except Exception:
        return None
    entry = {}
    in_main = False
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("["):
            in_main = line == "[Desktop Entry]"
            continue
        if not in_main or "=" not in line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        if key in ("Name", "GenericName", "Exec", "Type", "NoDisplay", "Hidden",
                   "Keywords", "TryExec"):
            entry.setdefault(key, value.strip())
    if entry.get("Type", "Application") != "Application" or not entry.get("Exec"):
        return None
    if entry.get("NoDisplay", "").lower() == "true" or entry.get("Hidden", "").lower() == "true":
        return None
    if entry.get("TryExec") and shutil.which(entry["TryExec"]) is None:
        return None
    return entry


def _index_applications():
    """Scan the .desktop folders. Returns [(display, path, nn, ids, words)]:
    `ids` are the other normalized names the app answers to exactly or by
    substring (desktop id, its last segment, the program it runs), `words`
    its generic name and keywords (exact matches only)."""
    entries = []
    seen_ids = set()
    for root in _app_dirs():
        if not root.is_dir():
            continue
        try:
            names = sorted(os.listdir(root))
        except OSError:
            continue
        for name in names:
            if not name.endswith(".desktop"):
                continue
            desktop_id = name[:-8]
            if desktop_id in seen_ids:
                continue
            # The first file with this id wins outright (XDG's shadowing
            # rule): a user entry marked Hidden/NoDisplay hides the distro's
            # rather than letting it through.
            seen_ids.add(desktop_id)
            path = root / name
            entry = _parse_desktop(path)
            if entry is None:
                continue
            display = entry.get("Name") or desktop_id
            # Names the app answers to besides its display name. Its ids —
            # the desktop id, its last segment, the program it runs — take
            # part in substring matching; keywords and the generic name only
            # in exact matching, or "code" would find every app whose
            # keywords mention code.
            ids = {_normalize(desktop_id), _normalize(desktop_id.split(".")[-1])}
            exe = _exec_basename(entry["Exec"])
            if exe:
                ids.add(_normalize(exe))
            words = set()
            if entry.get("GenericName"):
                words.add(_normalize(entry["GenericName"]))
            for kw in (entry.get("Keywords") or "").split(";"):
                if kw.strip():
                    words.add(_normalize(kw))
            ids.discard("")
            words.discard("")
            entries.append((display, str(path), _normalize(display), ids, words))
    return entries


def _best_match(query, candidates):
    """Find best matching app from candidates list."""
    qn = _normalize(query)
    if not qn:
        return None, None

    # Exact match on the display name, then on any other name it answers to
    for display, path, nn, ids, words in candidates:
        if nn == qn:
            return display, path
    for display, path, nn, ids, words in candidates:
        if qn in ids or qn in words:
            return display, path

    # Contains (either direction) on the display name and the ids, shortest
    # display name wins
    cont = [(display, path) for display, path, nn, ids, words in candidates
            if qn in nn or nn in qn or any(qn in x for x in ids)]
    if cont:
        cont.sort(key=lambda x: len(x[0]))
        return cont[0]

    # Fuzzy — on the display name only, as the macOS matcher does, and a
    # notch stricter than its 0.6: Linux app names are short generic words
    # ("Fonts", "Files", "Disks"), so at 0.6 "notes" launched Fonts on a
    # machine with no notes app at all. Typos still resolve ("calculater" ->
    # Calculator scores 0.9); a miss is the right answer for the rest.
    scored = []
    for display, path, nn, ids, words in candidates:
        scored.append((SequenceMatcher(None, qn, nn).ratio(), display, path))
    scored.sort(reverse=True)
    if scored and scored[0][0] >= 0.75:
        return scored[0][1], scored[0][2]

    return None, None


def _default_browser(candidates):
    """The desktop's default web browser, as (display, path), or (None, None)."""
    try:
        result = subprocess.run(["xdg-settings", "get", "default-web-browser"],
                                capture_output=True, text=True, timeout=5)
        desktop_id = result.stdout.strip()
    except Exception:
        desktop_id = ""
    if desktop_id.endswith(".desktop"):
        desktop_id = desktop_id[:-8]
    if desktop_id:
        for display, path, nn, ids, words in candidates:
            if Path(path).name == desktop_id + ".desktop":
                return display, path
    return None, None


def _resolve(app_name: str):
    """(display, path) for a query, through aliases, the index and the
    default-browser rule; (None, None) when nothing matches."""
    query = _ALIASES.get(_normalize(app_name), app_name)
    candidates = _index_applications()
    if _normalize(query) in _BROWSER_QUERIES:
        display, path = _default_browser(candidates)
        if path:
            return display, path
    return _best_match(query, candidates)


def _move_to_main_screen():
    """Position the frontmost window — a no-op on Wayland.

    The macOS tool moved the launched window onto the main display. A
    Wayland client cannot move or resize another client's window (there is
    no API for it, by design), and GNOME places a newly launched window on
    the monitor with the pointer anyway. Kept so callers stay the same."""
    return None


def _is_app_running(app_name: str) -> bool:
    """True if the app has a live process or a window on the accessibility
    bus. Three signals, any one suffices, this user's processes only:
      - a process whose comm is the Exec program (/proc/<pid>/comm is that
        name cut to 15 chars) — plain binaries;
      - a process whose argv[0] is the Exec program — the same, seen through
        a symlinked path;
      - an application on the AT-SPI bus named like the desktop entry —
        apps whose Exec is a wrapper (google-chrome-stable is a bash script
        around /opt/google/chrome/chrome, comm "chrome") and snaps or
        flatpaks, whose processes run under a launcher's name."""
    display, path = _resolve(app_name)
    if not path:
        return False
    entry = _parse_desktop(Path(path))
    exe = _exec_basename(entry["Exec"]) if entry else ""
    uid = os.getuid()
    if exe:
        comm = exe[:15]
        for pid in os.listdir("/proc"):
            if not pid.isdigit():
                continue
            try:
                if os.stat(f"/proc/{pid}").st_uid != uid:
                    continue
                with open(f"/proc/{pid}/comm") as f:
                    if f.read().strip() == comm:
                        return True
                with open(f"/proc/{pid}/cmdline", "rb") as f:
                    argv0 = f.read().split(b"\0", 1)[0].decode(errors="replace")
                if os.path.basename(argv0) == exe:
                    return True
            except Exception:
                continue
    try:
        from ...tree.element import get_desktop_apps, _safe_name
        want = _normalize(display)
        for app in get_desktop_apps():
            have = _normalize(_safe_name(app))
            if have and want and (have == want or have in want or want in have):
                return True
    except Exception:
        pass
    return False


def _launch(path: str) -> bool:
    """Launch a .desktop entry the way the desktop itself does — through
    gio, which hands the app an activation token so GNOME lets its window
    take focus (a bare Popen gets "window is ready" instead). gtk-launch is
    the fallback, then the Exec line itself."""
    desktop_id = Path(path).name[:-8]
    for argv in (["gio", "launch", path], ["gtk-launch", desktop_id]):
        if shutil.which(argv[0]) is None:
            continue
        # Never wait for the launcher to finish: for an app that is not
        # DBusActivatable, gio and gtk-launch stay alive as the app's parent
        # for as long as it runs (measured: a 10s wait per launcher, then
        # the app started a third time from Exec). A launcher still running
        # after a beat has launched; one that exited non-zero has not.
        try:
            proc = subprocess.Popen(argv, start_new_session=True,
                                    stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, text=True)
        except Exception as e:
            logger.warning(f"{argv[0]} failed for {path}: {e}")
            continue
        try:
            rc = proc.wait(timeout=LAUNCH_SETTLE)
        except subprocess.TimeoutExpired:
            return True
        if rc == 0:
            return True
        err = (proc.stderr.read() if proc.stderr else "").strip()
        logger.warning(f"{argv[0]} failed for {path}: {err[:200]}")
    entry = _parse_desktop(Path(path))
    if not entry:
        return False
    try:
        parts = shlex.split(entry["Exec"])
    except ValueError:
        return False
    parts = [p for p in parts if not (p.startswith("%") and len(p) == 2)]
    if not parts:
        return False
    try:
        subprocess.Popen(parts, start_new_session=True,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return True
    except Exception as e:
        logger.error(f"Exec launch failed for {path}: {e}")
        return False


def _bring_to_front(app_name: str):
    """Bring an already-running app to the front.

    Wayland gives a client no way to raise another window, so the app is
    activated through its own desktop entry instead: a single-instance app
    (GNOME apps, browsers, Electron apps) presents its existing window with
    the launch's activation token, which is exactly what a click on its dock
    icon does. Apps that open a new window per launch get a new window."""
    display, path = _resolve(app_name)
    if not path:
        logger.warning(f"Could not bring {app_name} to front: no desktop entry")
        return
    if _launch(path):
        logger.info(f"Activated {display} via its desktop entry")
    else:
        logger.warning(f"Could not bring {app_name} to front")


def open_app(app_name: str) -> bool:
    """
    Open an application on Linux (or bring to front if already running).

    Args:
        app_name: Application name (e.g., "Google Chrome", "firefox", "files")

    Returns:
        True if launched successfully, False otherwise.
    """
    display, path = _resolve(app_name)
    if not path:
        logger.error(f"App not installed: {app_name}")
        return False

    if not _launch(path):
        logger.error(f"Failed to open {display}")
        return False
    logger.info(f"Opened app: {display} ({path})")

    # Give the app time to launch and become frontmost
    time.sleep(1.0)

    _move_to_main_screen()

    return True
