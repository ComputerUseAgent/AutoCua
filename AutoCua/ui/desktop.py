"""Opens the desktop app in the Chromium shell (desktop/ at the repo root, an
Electron app) when it is installed. run_agent(ui=True) calls launch_desktop()
first and falls back to the pywebview window when it returns False.

Stdlib only, and it never imports AutoCua.ui.service: the shell starts the
backend as a process of its own (`python -m AutoCua.ui.service --desktop`), so
the window never shares a process, a thread or a lock with the agent."""

import os
import subprocess
import sys
from pathlib import Path

# <repo>/AutoCua/ui/desktop.py -> <repo>/desktop
SHELL_DIR = Path(__file__).resolve().parents[2] / "desktop"


def electron_binary():
    """The Electron executable that `npm install` put in desktop/node_modules
    (the electron package records where in path.txt), or None."""
    package = SHELL_DIR / "node_modules" / "electron"
    try:
        relative = (package / "path.txt").read_text(encoding="utf-8").strip()
    except OSError:
        return None
    exe = package / "dist" / relative
    return exe if exe.is_file() else None


def launch_desktop():
    """Run the shell and block until it quits. Returns False, with nothing
    started, when it is not installed or AUTOCUA_UI=webview asks for the
    pywebview window."""
    if os.environ.get("AUTOCUA_UI", "").strip().lower() == "webview":
        return False
    exe = electron_binary()
    if exe is None:
        print("[ui] The Chromium window is not installed (cd desktop && npm install); "
              "opening the WebKit one.", flush=True)
        return False
    env = dict(os.environ, AUTOCUA_PYTHON=sys.executable)
    # Set for everything VS Code (an Electron app) spawns; inherited, it makes
    # Electron start as plain Node, with no window and no `app`.
    env.pop("ELECTRON_RUN_AS_NODE", None)
    subprocess.run([str(exe), str(SHELL_DIR)], env=env, check=False)
    return True
