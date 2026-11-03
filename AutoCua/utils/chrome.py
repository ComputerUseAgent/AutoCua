"""Google Chrome, the web agent's browser: find it, and say when it is missing.

The web agent drives Google Chrome in both of its modes: the person's own Chrome
through the AutoCua extension for a normal run, and a Chrome of its own on a
debugging port for a headless or parallel run. AutoCua never downloads or
installs it: the person installs Chrome themselves (google.com/chrome). While
it is missing, the terminal says MISSING (once per process tree) and the app
shows POPUP when it starts.

Where Chrome is looked for, as browser.rs (find_chrome) looks first:
  macOS    /Applications/Google Chrome.app, or ~/Applications
  Windows  Google\\Chrome\\Application\\chrome.exe under %ProgramFiles%,
           %ProgramFiles(x86)% or %LOCALAPPDATA%
  Linux    /opt/google/chrome/chrome, google-chrome or google-chrome-stable
           on PATH, or /usr/bin/google-chrome
"""

import os
import shutil
import sys
from pathlib import Path

MAC_APP = "Google Chrome.app/Contents/MacOS/Google Chrome"
WIN_EXE = r"Google\Chrome\Application\chrome.exe"
# The terminal's line and the app's popup while Chrome is missing
MISSING = "Chrome is missing from the system: install it for the web agent to work"
POPUP = "Chrome is not installed on this system. Please install it."
# Set once this process has said Chrome is missing, so the line is not said
# again by its next run, nor by the children a run starts (they inherit the
# environment).
SAID = "AutoCua_CHROME_MISSING_SAID"


def _candidates():
    """Every place Chrome may be, in the order browser.rs checks them."""
    if sys.platform == "darwin":
        return [Path("/Applications") / MAC_APP, Path.home() / "Applications" / MAC_APP]
    if sys.platform == "win32":
        roots = [os.environ.get(k) for k in ("ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA")]
        return [Path(r) / WIN_EXE for r in roots if r]
    found = [Path(p) for p in (shutil.which("google-chrome"), shutil.which("google-chrome-stable")) if p]
    return [Path("/opt/google/chrome/chrome")] + found + [Path("/usr/bin/google-chrome")]


def find_chrome():
    """Chrome's executable as a string, or None when it is not installed."""
    for p in _candidates():
        if p.is_file():
            return str(p)
    return None


def _say(text):
    """print, dropping the globe where the stream cannot encode it. On Windows
    a pipe or a log file is cp1252 (a browser agent's child writes to one)."""
    try:
        print(text, flush=True)
    except UnicodeEncodeError:
        print(text.replace("🌐 ", ""), flush=True)


def ensure_chrome():
    """Chrome's executable, or None when it is not installed. Never downloads
    or installs anything and never raises: the web agent calls it by name
    before every launch (web/lib.rs). When Chrome is missing it says so, once."""
    found = find_chrome()
    if found is None and not os.environ.get(SAID):
        os.environ[SAID] = "1"
        _say(f"🌐 {MISSING} (https://www.google.com/chrome/).")
    return found


if __name__ == "__main__":
    print(ensure_chrome() or "no Chrome")
