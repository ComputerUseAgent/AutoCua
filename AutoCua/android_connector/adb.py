"""The Android connection: Google's adb, and the phone it talks to.

A real phone on USB, with Developer options and USB debugging turned on, is
reached through adb (Android Debug Bridge). adb is not a pip package. It is a
native program in Google's platform-tools, one zip per OS, so the first time a
phone is wanted it is fetched from Google's own server into platform-tools/
beside this file (gitignored), the way utils/wda.py fetches WebDriverAgent for
iOS. An adb already on PATH is used as it is.

The zip is pinned three ways: the release, its byte size and its SHA-256,
checked before it is unpacked. A partial or altered download is never used.

This file only connects: it finds adb, starts its server and waits until the
phone is ready. Streaming the screen is stream.py's job, and saving
screenshots element.rs's (AutoCua/android/tree/element.rs).

    python3 -m AutoCua.android_connector.adb     # fetch adb, show the phone
"""

import hashlib
import os
import platform
import shutil
import ssl
import subprocess
import sys
import threading
import time
import urllib.request
import zipfile
from pathlib import Path

# Pinned platform-tools release. Recorded 2026-10-25 from Google's SDK
# manifest (repository2-3.xml, which lists size + SHA-1) and the zips as
# downloaded. A re-pin means updating the release, sizes AND digests together.
VERSION = "37.0.1"
BASE_URL = "https://dl.google.com/android/repository/"
ARCHIVES = {
    # sys.platform -> (zip name, bytes, sha256)
    "darwin": (f"platform-tools_r{VERSION}-darwin.zip", 16110554,
               "ee39ad5967e95c2a07f04dbcbde96b1a0c916ba376096db5d2f498b7727a5d1d"),
    "linux": (f"platform-tools_r{VERSION}-linux.zip", 9054187,
              "d230f13842f60f782a8645f9c813f8f845bf36089ea7289f28c48f17979313f1"),
    "win32": (f"platform-tools_r{VERSION}-win.zip", 8044989,
              "45f4d63113e895ebde0c90f194099a4676b6ac653bd28d54314a9e022bbc1a99"),
}

HERE = Path(__file__).resolve().parent
TOOLS_DIR = HERE / "platform-tools"
OWN_ADB = TOOLS_DIR / ("adb.exe" if sys.platform == "win32" else "adb")

# What each `adb devices` state means for the person holding the phone.
HINTS = {
    None: "No phone found. Plug it in with a data USB cable and turn on "
          "Settings > Developer options > USB debugging. "
          "(Windows may also need the phone maker's USB driver.)",
    "unauthorized": "Unlock the phone and tap Allow on \"Allow USB debugging?\" "
                    "(tick \"Always allow from this computer\").",
    "authorizing": "The phone is still authorizing this computer...",
    "offline": "The phone is offline. Unplug it and plug it back in.",
}
NO_PERMISSIONS = ("This user may not open the phone over USB. Add Android udev "
                  "rules (for example: apt install android-sdk-platform-tools-common) "
                  "and plug it in again.")

# One fetch per process, however many threads ask at once.
_LOCK = threading.Lock()


def ensure_adb() -> bool:
    """True when our own adb is on disk. Fetches platform-tools once if it isn't."""
    if OWN_ADB.is_file():
        return True
    archive = ARCHIVES.get(sys.platform)
    # Google's Linux build is x86-64 only; Mac is universal, Windows runs anywhere.
    if archive is None or (sys.platform == "linux" and platform.machine() != "x86_64"):
        print("🤖 Google has no adb for this machine. Install one "
              "(for example: apt install adb) and run again.")
        return False
    name, size, digest = archive

    with _LOCK:
        # Another thread may have finished the fetch while we waited.
        if OWN_ADB.is_file():
            return True

        # Unpack under a private staging name and rename it into place at the
        # end, so a half-written folder is never seen as platform-tools/.
        staging = HERE / f".platform-tools.incoming.{os.getpid()}"
        shutil.rmtree(staging, ignore_errors=True)
        print(f"🤖 Fetching adb (platform-tools {VERSION}) from Google, one time...", flush=True)
        try:
            staging.mkdir()
            zip_path = staging / name
            _download(BASE_URL + name, zip_path, size, digest)
            _unzip(zip_path, staging)
            if not OWN_ADB.is_file():
                shutil.rmtree(TOOLS_DIR, ignore_errors=True)    # a folder with no adb in it
                for attempt in range(10):
                    try:
                        os.rename(staging / "platform-tools", TOOLS_DIR)
                        break
                    except OSError:
                        # Windows: an antivirus still scanning the new
                        # executables holds them open for a moment.
                        if attempt == 9 or OWN_ADB.is_file():
                            raise
                        time.sleep(0.5)
        except Exception as e:
            # Another process may have won the rename: that is still fine.
            if not OWN_ADB.is_file():
                print(f"🤖 Could not fetch adb: {e}")
                print(f"🤖 Fetch it by hand: unzip {BASE_URL}{name} into {HERE}")
                return False
        finally:
            shutil.rmtree(staging, ignore_errors=True)

        print(f"🤖 adb {VERSION} ready at {TOOLS_DIR}")
        return True


def _download(url, dest, size, digest):
    """Stream url into dest, then check it against the pinned size and SHA-256.

    Python's own certificate store can be empty (python.org builds on macOS
    until their certificate script is run), so certifi's bundle is used when
    it is installed, as utils/chrome.py does."""
    try:
        import certifi
        context = ssl.create_default_context(cafile=certifi.where())
    except ImportError:
        context = ssl.create_default_context()
    sha = hashlib.sha256()
    req = urllib.request.Request(url, headers={"User-Agent": "AutoCua"})
    with urllib.request.urlopen(req, timeout=60, context=context) as r, open(dest, "wb") as out:
        while chunk := r.read(1 << 16):
            sha.update(chunk)
            out.write(chunk)
    got = dest.stat().st_size
    if got != size or sha.hexdigest() != digest:
        raise ValueError(f"{url} is not the pinned file ({got} bytes, sha256 {sha.hexdigest()})")


def _unzip(zip_path, dest):
    """Unpack, keeping the run bits. zipfile drops them, and adb must be executable."""
    with zipfile.ZipFile(zip_path) as z:
        for info in z.infolist():
            path = z.extract(info, dest)
            mode = (info.external_attr >> 16) & 0o777
            if mode and os.name != "nt":
                os.chmod(path, mode)


def adb_path():
    """The adb to use: the one on PATH if there is one, else ours. None if neither."""
    return shutil.which("adb") or (str(OWN_ADB) if ensure_adb() else None)


def _adb(adb, *args, timeout=30):
    # adb writes UTF-8; Windows would otherwise decode it with its code page.
    return subprocess.run([adb, *args], capture_output=True, encoding="utf-8",
                          errors="replace", timeout=timeout)


def _wireless(serial):
    """True for a Wi-Fi transport (ip:port, or an mDNS ..._adb-tls-connect._tcp
    name). A phone once paired for Wireless debugging is listed twice, once on
    USB and once on Wi-Fi."""
    return ":" in serial or "._tcp" in serial


def devices(adb):
    """[(serial, state)] from `adb devices`. A ready phone's state is "device"."""
    rows = []
    for line in _adb(adb, "devices").stdout.splitlines()[1:]:
        if "\t" in line:
            serial, state = line.split("\t", 1)
            rows.append((serial.strip(), state.strip()))
    return rows


def connect(serial=None, wait=60.0):
    """Start adb and wait until the phone is ready. Returns (adb path, serial).

    serial picks one phone when several are plugged in. Each time the phone's
    state changes, one line says what to do about it (tap Allow, replug...).
    """
    adb = adb_path()
    if not adb:
        raise RuntimeError("adb is not available")
    started = _adb(adb, "start-server")
    if started.returncode != 0:
        raise RuntimeError(f"adb start-server failed: {started.stderr.strip()}")

    deadline = time.monotonic() + wait
    said = object()
    while True:
        rows = devices(adb)
        if serial:
            rows = [r for r in rows if r[0] == serial]
        else:
            rows = [r for r in rows if not _wireless(r[0])]    # USB only for now
        ready = [s for s, state in rows if state == "device"]
        if len(ready) > 1:
            raise RuntimeError(f"More than one phone is ready ({', '.join(ready)}); pick one with serial.")
        if ready:
            return adb, ready[0]

        state = rows[0][1] if rows else None
        if state != said:
            said = state
            if state and state.startswith("no permissions"):
                print(f"🤖 {NO_PERMISSIONS}")
            else:
                print(f"🤖 {HINTS.get(state, f'The phone is in the {state!r} state, waiting...')}")
        if time.monotonic() > deadline:
            raise RuntimeError(f"No ready phone after {wait:.0f} s (last state: {state or 'none'})")
        time.sleep(1)


if __name__ == "__main__":
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows
    adb, serial = connect()
    model = _adb(adb, "-s", serial, "shell", "getprop", "ro.product.model").stdout.strip()
    print(f"🤖 Connected: {model or serial} ({serial}), adb at {adb}")
