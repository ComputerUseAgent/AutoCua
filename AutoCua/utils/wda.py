"""Clone WebDriverAgent if it isn't already here.

`ios_setup.sh` does this for a checkout. A `pip install AutoCua` runs no shell
script — a wheel is unpacked, never executed — so the same clone has to happen
the first time an iOS run asks for it.

WebDriverAgent is BSD-3-Clause, so it could legally be vendored; it is cloned
instead so this tree holds no third-party source we did not write, and because
it is an Xcode project that must be built against the user's own signing
identity anyway. See THIRD_PARTY_NOTICES.md.

Call it from wherever WebDriverAgent is about to be needed — it is a cheap
directory check once the clone exists, and it is safe to call from several
threads and several processes at once (a parallel run boots N simulators in N
threads, and each one asks for the project).
"""

import os
import shutil
import subprocess
import sys
import threading
from pathlib import Path

# Pinned tag — keep in step with ios_setup.sh and THIRD_PARTY_NOTICES.md.
VERSION = "v15.1.1"
REPO = "https://github.com/appium/WebDriverAgent.git"

# Where ios_setup.sh has always put it, so a checkout that already ran the
# script keeps using the copy it has already signed and built.
WDA_DIR = Path(__file__).resolve().parent.parent / "ios_connector" / "WebDriverAgent"
XCODEPROJ = WDA_DIR / "WebDriverAgent.xcodeproj"

# One fetch per process, however many threads ask at once.
_LOCK = threading.Lock()


def ensure_wda() -> bool:
    """True when WebDriverAgent is on disk. Clones it once if it isn't."""
    if XCODEPROJ.is_dir():
        return True
    if sys.platform != "darwin":
        return False
    if not shutil.which("git"):
        print("📱 git not found — run:  xcode-select --install")
        return False

    with _LOCK:
        # Another thread may have finished the clone while we waited.
        if XCODEPROJ.is_dir():
            return True

        # Clone to a private staging name and rename it into place at the end.
        # Two PROCESSES can be here at once (each parallel task is its own
        # child process), and a half-written tree must never be visible under
        # the real name — sim_session/setup.py test for the .xcodeproj and
        # would happily hand a partial checkout to xcodebuild.
        staging = WDA_DIR.parent / f".WebDriverAgent.incoming.{os.getpid()}"
        shutil.rmtree(staging, ignore_errors=True)

        print(f"📱 Fetching WebDriverAgent {VERSION} — one time, a few seconds...", flush=True)
        # GIT_TERMINAL_PROMPT=0 and the askpass override matter more than they
        # look: this runs unattended inside an agent, and a proxy or a stale
        # credential helper would otherwise stop on a username prompt that
        # nobody is there to answer. Fail instead, and bound the wait — a
        # network that hangs must not hang the run with it.
        env = dict(os.environ,
                   GIT_TERMINAL_PROMPT="0",
                   GIT_ASKPASS="echo",
                   GCM_INTERACTIVE="never")
        try:
            result = subprocess.run(
                ["git", "clone", "--depth", "1", "--branch", VERSION, REPO, str(staging)],
                check=False, env=env, timeout=600,
            )
            failed = result.returncode != 0
        except subprocess.TimeoutExpired:
            print("📱 Fetching WebDriverAgent timed out after 10 minutes.")
            failed = True
        except OSError as e:                      # unwritable install dir, no git
            print(f"📱 Could not write into {WDA_DIR.parent}: {e}")
            failed = True

        if failed or not (staging / "WebDriverAgent.xcodeproj").is_dir():
            shutil.rmtree(staging, ignore_errors=True)
            print("📱 Could not fetch WebDriverAgent.")
            print(f"📱 Fetch it by hand with:  git clone --depth 1 --branch {VERSION} {REPO} {WDA_DIR}")
            return False

        if WDA_DIR.exists():
            # Someone else won the race — keep their copy, drop ours.
            if XCODEPROJ.is_dir():
                shutil.rmtree(staging, ignore_errors=True)
                return True
            # A previous failed attempt left a directory that is neither a
            # usable project nor something any check would call "present".
            shutil.rmtree(WDA_DIR, ignore_errors=True)

        try:
            os.rename(staging, WDA_DIR)
        except OSError:
            shutil.rmtree(staging, ignore_errors=True)
            return XCODEPROJ.is_dir()             # lost the rename race: still fine

        print(f"📱 WebDriverAgent {VERSION} ready at {WDA_DIR}")
        return True
