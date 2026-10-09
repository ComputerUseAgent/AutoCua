"""Launcher for the existing WebDriverAgent setup UI (ios_connector/setup.py).

setup.py is the complete, self-contained sign → build → install → run tool with
its own index.html (Add Apple Account, team/device select, trust panel, live
build console). We don't reimplement any of that — the desktop app just runs it
as a local server on port 8765 and shows its index.html in an iframe (Settings →
Connect Device → iPhone). AutoCua_EMBED=1 suppresses setup.py's browser-open.
"""

import os
import sys
import shutil
import threading
import subprocess
import urllib.request
from pathlib import Path

SETUP_PORT = 8765
SETUP_URL = f"http://127.0.0.1:{SETUP_PORT}"
_DIR = Path(__file__).resolve().parent          # AutoCua/ios_connector
_SETUP_PY = _DIR / "setup.py"

_lock = threading.Lock()
_proc = None


def _python():
    """An interpreter that can run setup.py. sys.executable in dev; in a compiled
    build it isn't Python, so fall back to a real python3 on PATH."""
    exe = sys.executable or ""
    if exe and ("python" in Path(exe).name.lower()):
        return exe
    return shutil.which("python3") or shutil.which("python") or exe


def is_up(timeout=1.5):
    try:
        with urllib.request.urlopen(SETUP_URL + "/", timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False


def ensure_running(wait=12):
    """Start the setup server if it isn't already up; return {ok, url, ready}."""
    global _proc
    if is_up():
        return {"ok": True, "url": SETUP_URL, "ready": True}
    with _lock:
        if is_up():
            return {"ok": True, "url": SETUP_URL, "ready": True}
        py = _python()
        if not py or not _SETUP_PY.exists():
            return {"ok": False, "url": SETUP_URL, "ready": False,
                    "error": "setup.py or a Python interpreter was not found"}
        env = dict(os.environ)
        env["AutoCua_EMBED"] = "1"                # no browser tab
        try:
            _proc = subprocess.Popen(
                [py, str(_SETUP_PY)], cwd=str(_DIR), env=env,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except Exception as e:
            return {"ok": False, "url": SETUP_URL, "ready": False, "error": str(e)}
    # poll until the server answers
    import time
    for _ in range(int(wait / 0.5)):
        if is_up():
            return {"ok": True, "url": SETUP_URL, "ready": True}
        time.sleep(0.5)
    return {"ok": True, "url": SETUP_URL, "ready": False}   # started, still warming up
