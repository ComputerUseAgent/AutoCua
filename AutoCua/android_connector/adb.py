"""The Android connection: Google's adb, and the phone it talks to.

A real phone on USB, with Developer options and USB debugging turned on, is
reached through adb (Android Debug Bridge). adb is not a pip package. It is a
native program in Google's platform-tools, one zip per OS, so the first time a
phone is wanted it is fetched from Google's own server into platform-tools/
beside this file (gitignored), the way utils/wda.py fetches WebDriverAgent for
iOS. An adb already on PATH is used as it is.

The zip is pinned three ways: the release, its byte size and its SHA-256,
checked before it is unpacked. A partial or altered download is never used.

A phone on Wi-Fi works too, with Android 11's Wireless debugging turned on
(Developer options): wifi_addresses() finds it on a connection adb already
has to it, else by the name it announces on the network, and wifi_connect()
connects it with the key it already trusts from USB ("Always allow from this
computer"), so there is no pairing code. Which phones may go over Wi-Fi is
session.py's call: only those connected once over USB in Settings.

An emulator that emulator.py started is reached the same way, by its serial
(emulator-<port>), and never picked as a phone.

This file only connects: it finds adb, starts its server and waits until the
phone is ready. Starting the phone helper (screenshots and the tree) is
stream.py's job, and reading them element.rs's (AutoCua/android/tree/element.rs).

    python3 -m AutoCua.android_connector.adb          # fetch adb, show the phone
    python3 -m AutoCua.android_connector.adb fetch    # adb only
"""

import hashlib
import os
import platform
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

from AutoCua.utils.consent import allow_download

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

# What fetches adb on a terminal, with its question: the setup scripts run
# it, and the line a run that cannot ask prints names it.
FETCH = "python -m AutoCua.android_connector.adb fetch"

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

# Wi-Fi: the phone announces Wireless debugging over mDNS as
# adb-<its USB serial>-<6 random characters>, at a port that changes each
# time the switch is turned on. adb's own mDNS starts with its server and
# hears a phone within a second or so.
WIFI_LOOK = 3.0
# adb connect itself waits for the OS's TCP timeout (75 s on macOS) when
# nothing answers.
WIFI_CONNECT_WAIT = 10
# A Wi-Fi connection adb already has is asked which phone it is: a dozing
# phone answers in under a second, one gone without a word may never.
WIFI_ASK_WAIT = 3

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
        if not allow_download("adb, Google's platform-tools",
                              "Google (dl.google.com), under Google's Android SDK terms "
                              "(developer.android.com/studio/terms)", size,
                              log=lambda text: print(f"🤖 {text}", flush=True), how=FETCH):
            return False

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


def _contexts():
    """The certificate stores to try, in order. certifi's first: Python's own
    store can be empty (python.org builds on macOS until their certificate
    script is run), as utils/chrome.py found. Then the operating system's: an
    antivirus or company proxy that re-signs HTTPS (Norton does, on Windows)
    signs with a root only the OS trusts, as utils/electron.py found."""
    try:
        import certifi
    except ImportError:
        certifi = None
    if certifi:
        yield ssl.create_default_context(cafile=certifi.where())
    yield ssl.create_default_context()


def _download(url, dest, size, digest, algo="sha256", progress=None):
    """Stream url into dest, then check it against the pinned size and digest:
    SHA-256, or for the emulator's zips (emulator.py) the SHA-1 Google's SDK
    manifest lists. progress(bytes so far) is called as it goes. Only a
    certificate one store cannot verify moves on to the next (_contexts); the
    pinned digest is checked whichever store let the file through."""
    req = urllib.request.Request(url, headers={"User-Agent": "AutoCua"})
    for context in _contexts():
        sha = hashlib.new(algo)
        try:
            with urllib.request.urlopen(req, timeout=60, context=context) as r, open(dest, "wb") as out:
                while chunk := r.read(1 << 16):
                    sha.update(chunk)
                    out.write(chunk)
                    if progress:
                        progress(out.tell())
            break
        except urllib.error.URLError as e:
            if not isinstance(e.reason, ssl.SSLCertVerificationError):
                raise
            refused = e
    else:
        raise refused
    got = dest.stat().st_size
    if got != size or sha.hexdigest() != digest:
        raise ValueError(f"{url} is not the pinned file ({got} bytes, {algo} {sha.hexdigest()})")


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
    # stdin from DEVNULL: `adb shell <command>` forwards this process's stdin
    # to the phone while the command runs, and would eat keys (or piped
    # lines) meant for whoever runs this.
    return subprocess.run([adb, *args], capture_output=True, encoding="utf-8",
                          errors="replace", timeout=timeout, stdin=subprocess.DEVNULL)


def _wireless(serial):
    """True for a Wi-Fi transport (ip:port, or an mDNS ..._adb-tls-connect._tcp
    name). A phone once paired for Wireless debugging is listed twice, once on
    USB and once on Wi-Fi."""
    return ":" in serial or "._tcp" in serial


def _emulator(serial):
    """True for an emulator (emulator-<port>). Only device="simulation" runs
    use one, by its serial; a phone is never picked from among them."""
    return serial.startswith("emulator-")


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
            rows = [r for r in rows if not _wireless(r[0]) and not _emulator(r[0])]    # USB phones only
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


def _ask_wifi():
    """Ask the network once which phones have Wireless debugging on: one mDNS
    question, sent from mDNS's own port so the phones answer to everyone
    there, adb's own listener too. adb asks only now and then by itself:
    measured on a moto g77, a phone that had dropped out of adb's list was
    still missing 37 s later, and back 0.3 s after one question."""
    name = b"".join(bytes([len(p)]) + p for p in (b"_adb-tls-connect", b"_tcp", b"local")) + b"\0"
    question = struct.pack(">6H", 0, 0, 1, 0, 0, 0) + name + struct.pack(">2H", 12, 1)    # PTR, IN
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP) as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            if hasattr(socket, "SO_REUSEPORT"):
                s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
            s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 255)
            s.bind(("", 5353))
            s.sendto(question, ("224.0.0.251", 5353))
    except OSError:
        pass        # the port taken for good, no network: adb's list as it is


def wifi_addresses(adb, serials):
    """{serial: "ip:port"} for those of `serials` whose phone is on Wi-Fi:
    on a connection adb already has (its ro.serialno says which phone it is:
    a dozing phone keeps it but may not answer the network), or announcing
    Wireless debugging on this network. While none is heard it asks the
    network (_ask_wifi, once a second) and looks again for up to WIFI_LOOK
    seconds: a server that has just started has not heard the phones yet,
    and one that has may have let a phone drop out of its list. Still none,
    and adb has no phone at all: adb is restarted once and looked at again."""
    if not serials:
        return {}
    live = {}
    for address, state in devices(adb):
        if state != "device" or not _wireless(address):
            continue
        try:
            said = _adb(adb, "-s", address, "shell", "getprop", "ro.serialno", timeout=WIFI_ASK_WAIT)
        except subprocess.TimeoutExpired:
            continue
        if said.stdout.strip() in serials:
            live.setdefault(said.stdout.strip(), address)
    deadline = time.monotonic() + WIFI_LOOK
    asked = None
    restarted = False
    while True:
        found = dict(live)
        for line in _adb(adb, "mdns", "services").stdout.splitlines():
            # "<instance>\t<service type>\t<ip>:<port>"; ":<port>" while only
            # an IPv6 address is known, which adb cannot connect to.
            parts = line.split("\t")
            if len(parts) != 3 or not parts[1].startswith("_adb-tls-connect.") or parts[2].startswith(":"):
                continue
            for serial in serials:
                # adb-<serial>-<6 letters or digits>, maybe " (2)" after it. A
                # name that goes on with another "-" is a phone whose own
                # serial only starts like this one.
                prefix = f"adb-{serial}-"
                if parts[0].startswith(prefix) and "-" not in parts[0][len(prefix):]:
                    found.setdefault(serial, parts[2].strip())
        if found:
            return found
        if time.monotonic() > deadline:
            # adb's own discovery can go deaf after the computer sleeps (moto
            # g77: a server started before a night's sleep heard nothing after
            # it, while the phone kept announcing; a fresh one found it in
            # 0.4 s). With no phone on adb, restarting it cuts nothing.
            if restarted or devices(adb):
                return found
            _adb(adb, "kill-server")
            _adb(adb, "start-server")
            restarted, asked = True, None
            deadline = time.monotonic() + WIFI_LOOK
            continue
        if asked is None or time.monotonic() - asked >= 1.0:
            _ask_wifi()
            asked = time.monotonic()
        time.sleep(0.3)


def wifi_connect(adb, serial, address):
    """Connect the phone `serial` at `address` (from wifi_addresses) and return
    the address, its adb serial from now on. RuntimeError when it does not
    connect, or another phone answers there."""
    # A connection that was there already may be another program's: only
    # one made here is dropped again on failure.
    mine = address not in dict(devices(adb))
    try:
        said = _adb(adb, "connect", address, timeout=WIFI_CONNECT_WAIT).stdout.strip()
    except subprocess.TimeoutExpired:
        said = f"no answer from {address} within {WIFI_CONNECT_WAIT} s"
    # adb connect exits 0 either way: the row's state is what counts.
    deadline = time.monotonic() + 3
    while dict(devices(adb)).get(address) != "device":
        if time.monotonic() > deadline:
            if mine:
                _adb(adb, "disconnect", address)
            hint = ("Allow Local Network for this app in System Settings > Privacy & Security > Local Network."
                    if sys.platform == "darwin" and "No route to host" in said
                    else "Plug the phone in by USB once, then try again.")
            raise RuntimeError(f"Wi-Fi: {said or f'no answer from {address}'}. {hint}")
        time.sleep(0.3)
    # The address came from the network: the phone there must be this one.
    got = _adb(adb, "-s", address, "shell", "getprop", "ro.serialno").stdout.strip()
    if got != serial:
        if mine:
            _adb(adb, "disconnect", address)
        raise RuntimeError(f"Wi-Fi: the phone at {address} is not {serial} ({got or 'no serial'})")
    return address


if __name__ == "__main__":
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows
    if sys.argv[1:] == ["fetch"]:
        # The setup scripts' step (FETCH): adb, unless one is on PATH, after
        # its yes, no phone needed. Exits 1 when it is still missing.
        sys.exit(0 if adb_path() else 1)
    adb, serial = connect()
    model = _adb(adb, "-s", serial, "shell", "getprop", "ro.product.model").stdout.strip()
    print(f"🤖 Connected: {model or serial} ({serial}), adb at {adb}")
