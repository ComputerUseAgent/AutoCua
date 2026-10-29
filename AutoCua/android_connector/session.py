"""One Android phone for the agent: adb and the phone helper, opened before a
run and closed after it, the way ios_connector/session.py's wda_session is
for the iPhone.

activate() finds adb and waits for the phone (adb.py), then starts the helper
on it (stream.py): scrcpy's server for the screen and Helper.java for the UI
tree, one process, forwarded to two local ports. The scanner
(android/tree/element.rs, UIElementScanner) reads those two ports; it calls
activate() itself when nobody has, so it also works on its own.
deactivate() stops the helper and drops the forwards. It also runs at exit,
so a helper never outlives this process (the helper holds Android's one
UiAutomation slot: a leftover one makes the next run's helper fail with
"already registered").

agent_launcher.run_agent does both around a "mobile use, android" run.

The desktop app has the iPhone's two steps too. Settings → Connect Device
checks a phone once (confirm with keep=False) and remembers it in
AutoCua_data/android_paired/paired_devices.json, the way
ios_connector/session.py remembers a paired iPhone. The chat box's Android
pick then starts the helper on the newest remembered phone that is plugged in
(confirm with keep=True).

In the desktop app (WIRELESS, which ui/service.py turns on; a terminal run is
cable only) a remembered phone works over Wi-Fi too, when it is not plugged in
and has Wireless debugging on (Android 11 and up, the same Wi-Fi as this computer):
activate() finds it by its USB serial in the name it announces and connects
it (adb.py's wifi_addresses and wifi_connect); its adb serial is then
"ip:port", and "phone" keeps its USB serial. Only remembered phones: a phone
is never reached over Wi-Fi before it was connected over USB in Settings.
USB comes first; the chat box's pick takes the newest remembered phone on
Wi-Fi only when none is plugged in. Except with the chat box's Wireless /
Wired toggle (link="wireless" / "wired" on confirm and activate): Wireless,
its default, reaches the phone over Wi-Fi only, also with its cable in, and
Wired over USB only. The app's run then connects the way its pick did.
"""

import atexit
import json
import os
import socket
import threading
import time

from AutoCua.android_connector.adb import (HINTS, NO_PERMISSIONS, _adb, _wireless, adb_path, connect, devices,
                                           wifi_addresses, wifi_connect)
from AutoCua.android_connector.stream import Stream

# Picks one phone when several are plugged in (adb's serial). Unset: the one
# USB phone.
SERIAL_ENV = "AutoCua_ANDROID_SERIAL"
# A phone over Wi-Fi is the desktop app's only (owner, 2026-10-29): its
# Wireless / Wired toggle and its computer-use runs. ui/service.py turns this
# on at start; a terminal run leaves it off and is cable only.
WIRELESS = False

# How long the app's Connect waits for a phone that is not ready (the
# terminal's run waits a minute, saying what to do on the phone).
APP_WAIT = 5.0

# How long the first tree may take: the helper connects to Android's
# accessibility first, and the forward takes a connection before it listens.
TREE_WAIT = 15.0

# How long a passing answer is asked again: "already registered" (a helper
# that was just stopped is gone in under a second; anything longer is another
# program) and "no active window" (a helper that has just connected gets
# Android's windows a moment later; anything longer is a screen that is off).
PASSING_WAIT = 3.0
PASSING = ("already registered", "no active window")


# ─────────────────────────── remembered phones ───────────────────────────
def _registry_file():
    """AutoCua_data/android_paired/paired_devices.json: user data, outside the
    install folder, as the iPhone's list is."""
    from AutoCua import _ensure, data_root
    return _ensure(data_root() / "android_paired") / "paired_devices.json"


# Path.read_text / write_text, never builtins.open(): compiled builds patch
# open() and silently swallow writes under AutoCua_data (see AutoCua/__init__.py).
def _read_registry():
    f = _registry_file()
    if f.exists():
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            if isinstance(data, dict) and isinstance(data.get("devices"), list):
                return data["devices"]
        except Exception:
            pass
    return []


def _write_registry(phones):
    try:
        _registry_file().write_text(
            json.dumps({"devices": phones}, indent=2, ensure_ascii=False), encoding="utf-8")
    except Exception:
        pass


def paired_devices():
    """Every phone connected in Settings: [{serial, name, version, paired_at}], newest first."""
    return sorted(_read_registry(), key=lambda d: d.get("paired_at", 0), reverse=True)


def add_paired(serial, name="Android", version=""):
    """Add or refresh one phone (one entry per serial)."""
    if not serial:
        return paired_devices()
    phones = [d for d in _read_registry() if d.get("serial") != serial]
    phones.append({"serial": str(serial), "name": str(name or "Android"),
                   "version": str(version or ""), "paired_at": int(time.time())})
    _write_registry(phones)
    return paired_devices()


def remove_paired(serial):
    """Forget one phone (Settings' × button)."""
    _write_registry([d for d in _read_registry() if d.get("serial") != serial])
    return paired_devices()


def phones():
    """The phones on USB, ready or not, for Settings' Device menu:
    [{"serial", "state", "model", "version", "hint"}]. A ready one ("device")
    tells its model and Android version; for the others, hint says what to
    do on the phone."""
    adb = adb_path()
    if not adb:
        raise RuntimeError("adb is not available")
    started = _adb(adb, "start-server")
    if started.returncode != 0:
        raise RuntimeError(f"adb start-server failed: {started.stderr.strip()}")
    found = []
    for serial, state in devices(adb):
        if _wireless(serial):
            continue                                # USB only for now, as connect()
        phone = {"serial": serial, "state": state, "model": "", "version": "", "hint": ""}
        if state == "device":
            props = _adb(adb, "-s", serial, "shell", "getprop ro.product.model; "
                         "getprop ro.build.version.release").stdout.splitlines() + ["", ""]
            phone["model"], phone["version"] = props[0].strip() or serial, props[1].strip()
        elif state.startswith("no permissions"):
            phone["hint"] = NO_PERMISSIONS
        else:
            phone["hint"] = HINTS.get(state, f"The phone is in the {state!r} state.")
        found.append(phone)
    return found


def wifi_saved(adb):
    """The newest remembered phone that is on Wi-Fi now (announcing Wireless
    debugging), by its USB serial; None when none is."""
    saved = [d["serial"] for d in paired_devices()]
    found = wifi_addresses(adb, saved)
    return next((s for s in saved if s in found), None)


def _link(serial):
    """How adb's `serial` reaches its phone: "wireless" or "wired"."""
    return "wireless" if _wireless(serial) else "wired"


def _reach(serial, wait, link=None):
    """(adb, adb serial, phone serial) for activate(). USB first, as connect()
    finds a phone; with none named, the newest remembered phone that is
    plugged in. A remembered phone that is not on USB is looked for on
    Wi-Fi: `serial` when it names one, else (no serial, no phone on USB at
    all) the newest one there. A Wi-Fi address named as `serial` is used only
    when the phone there is a remembered one. No other phone is ever reached
    over Wi-Fi. link, the chat box's toggle: "wireless" is Wi-Fi only (a
    remembered phone, also with its cable in), "wired" USB only."""
    adb = adb_path()
    if not adb:
        raise RuntimeError("adb is not available")
    started = _adb(adb, "start-server")
    if started.returncode != 0:
        raise RuntimeError(f"adb start-server failed: {started.stderr.strip()}")
    usb = [s for s, _ in devices(adb) if not _wireless(s)]
    saved = [d["serial"] for d in paired_devices()]
    if link == "wired" or (link is None and not WIRELESS):
        wanted = []                             # the cable only: Wired, or a terminal run
    elif link == "wireless":
        wanted = ([serial] if serial in saved else []) if serial else saved
    elif serial:
        wanted = [serial] if serial in saved and serial not in usb else []
    else:
        wanted = [] if usb else saved
    found = wifi_addresses(adb, wanted)
    for phone in wanted:                        # newest first
        if phone in found:
            return adb, wifi_connect(adb, phone, found[phone]), phone
    if link == "wireless":
        raise RuntimeError("The phone is not on Wi-Fi")
    # No phone named: the newest remembered one that is plugged in, as the
    # chat box and android_agent pick. A phone never connected in Settings
    # is still used when it is the only one on USB (connect() finds it).
    if not serial:
        serial = next((s for s in saved if s in usb), None)
    adb, ready = connect(serial, wait)
    if not _wireless(ready):
        return adb, ready, ready
    # A Wi-Fi address named directly (AutoCua_ANDROID_SERIAL; android_agent's
    # child gets the one its check verified): only a remembered phone's.
    phone = _adb(adb, "-s", ready, "shell", "getprop", "ro.serialno").stdout.strip()
    if phone not in saved:
        raise RuntimeError(f"Wi-Fi: the phone at {ready} is not one connected in Settings")
    return adb, ready, phone


def _recv(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise ConnectionError("the helper closed the connection")
        data += chunk
    return data


def _read_tree(port):
    """One tree through the helper, as element.rs reads it: the line "dump",
    then a 4-byte big-endian length and that many bytes. The forward takes a
    connection before the phone side listens, then drops it, so a dropped one
    is tried again until TREE_WAIT runs out. A helper stopped a moment ago
    (Settings' check) holds Android's one UiAutomation for under a second
    while it exits, and a helper that has just connected may not have the
    active window yet, so those two answers are asked again for a few seconds
    (the helper connects and reads again on each request)."""
    start = time.monotonic()
    while True:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=TREE_WAIT) as sock:
                sock.sendall(b"dump\n")
                answer = _recv(sock, int.from_bytes(_recv(sock, 4), "big")).decode("utf-8", "replace")
        except OSError as e:
            if time.monotonic() - start > TREE_WAIT:
                raise RuntimeError(f"no answer within {TREE_WAIT:.0f} s ({e})")
            time.sleep(0.2)
            continue
        if not answer.startswith("ERROR:"):
            return
        if not any(p in answer for p in PASSING) or time.monotonic() - start > PASSING_WAIT:
            raise RuntimeError(answer[len("ERROR:"):].strip())
        time.sleep(0.3)


class AndroidSession:
    """The helper on one phone, and the local ports its screen and tree come
    out on. One per process."""

    def __init__(self):
        self._lock = threading.Lock()
        self.adb = None
        self.serial = None      # adb's serial for it: the USB serial, or "ip:port" on Wi-Fi
        self.phone = None       # the phone's USB serial, as remembered
        self.link = None        # the chat box's toggle at its last pick: "wireless" / "wired"
        self.model = None
        self.stream = None
        atexit.register(self.deactivate)

    def activate(self, serial=None, wait=60.0, link=None):
        """Connect the phone and start its helper. serial: the phone's USB
        serial (a remembered phone that is not plugged in is looked for on
        Wi-Fi). wait: how long a phone that is not ready yet (Allow not
        tapped, still starting) gets. link: "wireless" / "wired" (_reach's),
        None for USB first.

        Returns {"ok": True, "state": "connected", "serial", "phone", "model",
        "adb", "port", "tree_port"} ("serial" is adb's, "ip:port" on Wi-Fi;
        "phone" the USB serial), or {"ok": False, "state": "error", "error"}
        when the phone cannot be reached. While the helper is running, a
        second call returns the same connection, unless it asks for the
        other link.
        """
        with self._lock:
            if self._alive() and link in (None, _link(self.serial)):
                return self._info()
            if self.stream is not None:
                self._stop()     # the helper died, or runs the other way: its forwards go first
            try:
                adb, found, phone = _reach(serial or os.environ.get(SERIAL_ENV) or None, wait, link)
                model = _adb(adb, "-s", found, "shell", "getprop", "ro.product.model").stdout.strip()
                stream = Stream.start(adb, found)
            except Exception as e:  # noqa: BLE001 - reported, not raised, like wda_session
                return {"ok": False, "state": "error", "error": str(e)}
            self.adb, self.serial, self.phone, self.model, self.stream = adb, found, phone, model or found, stream
            return self._info()

    def confirm(self, serial=None, keep=True, link=None):
        """The app's two steps. Settings' Connect (keep=False) starts the
        helper on `serial`, reads one tree through it, so the phone really
        answers, then stops it again. The chat box's Android pick (keep=True,
        no serial) does the same on the newest phone Settings connected that
        is plugged in, else the newest one on Wi-Fi, and leaves the helper
        running for the run. With its toggle's link, the newest one on Wi-Fi
        ("wireless", also with a cable in) or plugged in ("wired").

        Same answer as activate(), with "hint" on an error. A helper already
        running for the phone is left as it is (it serves one client at a
        time, and a run may be reading through it), unless the toggle asks
        for the other link.
        """
        if link:
            self.link = link
        if not serial:
            saved = paired_devices()
            if not saved:
                return {"ok": False, "state": "error", "error": "No Android phone connected yet",
                        "hint": "Connect it in Settings → Connect Device."}
            try:
                plugged = {p["serial"]: p for p in phones()}
            except Exception as e:  # noqa: BLE001 - reported, like activate
                return {"ok": False, "state": "error", "error": str(e)}
            phone = None if link == "wireless" else next((d for d in saved if d["serial"] in plugged), None)
            if phone is None:
                if link == "wired":
                    return {"ok": False, "state": "error", "error": f"{saved[0]['name']} is not plugged in",
                            "hint": "Plug it in with a USB cable and unlock it, or pick Wireless."}
                # None of them plugged in (or Wireless): the newest one on Wi-Fi.
                try:
                    on_wifi = wifi_saved(adb_path())
                except Exception as e:  # noqa: BLE001 - reported, like activate
                    return {"ok": False, "state": "error", "error": str(e)}
                phone = next((d for d in saved if d["serial"] == on_wifi), None)
                if phone is None and link == "wireless":
                    return {"ok": False, "state": "error", "error": f"{saved[0]['name']} is not on Wi-Fi",
                            "hint": "Turn on Wireless debugging on it (Developer options, Android 11 and "
                                    "up, on the same Wi-Fi as this computer), or pick Wired."}
                if phone is None:
                    return {"ok": False, "state": "error",
                            "error": f"{saved[0]['name']} is not plugged in or on Wi-Fi",
                            "hint": "Plug it in with a USB cable and unlock it, or turn on Wireless "
                                    "debugging on it (Developer options, Android 11 and up, on the "
                                    "same Wi-Fi as this computer)."}
            elif plugged[phone["serial"]]["state"] != "device":
                return {"ok": False, "state": "error", "error": f"{phone['name']} is not ready",
                        "hint": plugged[phone["serial"]]["hint"]}
            serial = phone["serial"]

        live = self.status()
        if live["state"] == "connected":
            if live["phone"] != serial:
                return {"ok": False, "state": "error", "error": f"Another phone ({live['model']}) is in use",
                        "hint": "Pick another mode in the chat box, or wait for its run to end."}
            if link in (None, _link(live["serial"])):
                return live

        res = self.activate(serial, wait=APP_WAIT, link=link)
        if not res["ok"]:
            return res
        try:
            _read_tree(res["tree_port"])
        except Exception as e:  # noqa: BLE001 - reported, like activate
            self.deactivate()
            hint = ("Wake the phone and unlock it, then try again." if "no active window" in str(e)
                    else "Close anything else that reads the phone's screen (another AutoCua, "
                         "Appium), then try again.")
            return {"ok": False, "state": "error", "error": f"The phone helper did not read the screen: {e}",
                    "hint": hint}
        if not keep:
            self.deactivate()
        return res

    def status(self):
        """{"state": "connected"} while the helper runs, else "disconnected"."""
        with self._lock:
            return self._info() if self._alive() else {"ok": True, "state": "disconnected"}

    def deactivate(self):
        """Stop the helper and drop its forwards. Safe to call twice."""
        with self._lock:
            if self.stream is not None:
                self._stop()

    def _alive(self):
        return self.stream is not None and self.stream.proc.poll() is None

    def _stop(self):
        try:
            self.stream.stop()
        except Exception as e:  # noqa: BLE001 - adb gone at exit, nothing to do about it
            print(f"📱 stopping the phone helper: {e}")
        self.stream = None

    def _info(self):
        return {"ok": True, "state": "connected", "serial": self.serial, "phone": self.phone,
                "model": self.model, "adb": self.adb, "port": self.stream.port,
                "tree_port": self.stream.tree_port}


android_session = AndroidSession()
