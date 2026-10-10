"""Paired-device registry + WDA session toggle.

Two halves, both dead simple:

REGISTRY (AutoCua_data/ios_paired/paired_devices.json)
    A device gets added when Settings finishes pairing it (the AutoCua runner
    app installed on the phone). If a device is in the list it is paired —
    clicking the Apple logo in the chat box just activates it, no checks, no
    reinstall. Settings can list and delete entries.

SESSION (the Apple-logo toggle)
    activate()  -> fresh WDA session over the cable:
                     pymobiledevice3 mounter auto-mount   (image is lost on reboot)
                     pymobiledevice3 usbmux forward 8100 8100
                     pymobiledevice3 developer dvt xcuitest --userspace <bundle>
                   then the phone answers on http://127.0.0.1:8100.
    status()    -> disconnected / connecting / connected / error (checks the
                   real WDA /status, so "connected" means actually reachable).
                   code "repair" = the runner can't launch (free signing expired).
    deactivate()-> stop both subprocesses (click the logo off).

No xcodebuild here. Pairing is the only place that builds/installs anything;
on "repair" the chat toggle (ios_session.js) re-runs that same pairing.
"""

import os
import sys
import json
import time
import signal
import shutil
import tempfile
import threading
import subprocess
import urllib.request
from pathlib import Path

WDA_BUNDLE_ID = "com.AutoCua.WebDriverAgentRunner.xctrunner"
WDA_PORT = 8100
WDA_STATUS_URL = f"http://127.0.0.1:{WDA_PORT}/status"
# WDA's live screen stream (MJPEG) on the phone. The runner always serves it;
# the Mac reads it through a second cable forward (WDASession.screen_stream).
MJPEG_PORT = 9100

# Which iOS target the current process is driving — "hardware" (paired iPhone)
# or "simulation" (sim_session). Whichever session activates last sets it;
# tools that need non-WDA device access read it (open_app's installed-apps
# scan picks pymobiledevice3 vs simctl from here).
active_target = {"kind": "hardware", "udid": None}


def wda_port() -> int:
    """WDA port for THIS process. 8100 unless AutoCua_WDA_PORT says otherwise.

    Parallel simulator tasks each get their own simulator + their own WDA, so
    the parent hands every child process its port through the environment."""
    try:
        return int(os.environ.get("AutoCua_WDA_PORT") or WDA_PORT)
    except (TypeError, ValueError):
        return WDA_PORT


def wda_url() -> str:
    """Base URL of the WDA this process talks to (http://localhost:<port>)."""
    return f"http://localhost:{wda_port()}"

# ─────────────────────────── paired-device registry ───────────────────────────
def _registry_file() -> Path:
    """AutoCua_data/ios_paired/paired_devices.json: user data, outside the
    install folder. A registry left in the old spot (AutoCua/ios_connector/)
    is moved here once, so an existing pairing survives the move."""
    from AutoCua import data_root, install_dir, _ensure
    f = _ensure(data_root() / "ios_paired") / "paired_devices.json"
    legacy = install_dir() / "AutoCua" / "ios_connector" / "paired_devices.json"
    if not f.exists() and legacy.is_file():
        try:
            f.write_bytes(legacy.read_bytes())
            legacy.unlink()
        except Exception:
            pass
    return f


# Path.read_text / write_text, never builtins.open(): compiled builds patch
# open() and silently swallow writes under AutoCua_data (see AutoCua/__init__.py).
def _read_registry() -> list:
    f = _registry_file()
    if f.exists():
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
            if isinstance(data, dict) and isinstance(data.get("devices"), list):
                return data["devices"]
        except Exception:
            pass
    return []


def _write_registry(devices: list) -> None:
    try:
        _registry_file().write_text(
            json.dumps({"devices": devices}, indent=2, ensure_ascii=False), encoding="utf-8")
    except Exception:
        pass


def paired_devices() -> list:
    """All paired devices: [{udid, name, version, paired_at}], newest first."""
    return sorted(_read_registry(), key=lambda d: d.get("paired_at", 0), reverse=True)


def add_paired(udid, name="iPhone", version="") -> list:
    """Add/refresh one paired device (idempotent on udid)."""
    if not udid:
        return paired_devices()
    devices = [d for d in _read_registry() if d.get("udid") != udid]
    devices.append({"udid": str(udid), "name": str(name or "iPhone"),
                    "version": str(version or ""), "paired_at": int(time.time())})
    _write_registry(devices)
    return paired_devices()


def remove_paired(udid) -> list:
    """Delete one device from the list (Settings' × button)."""
    _write_registry([d for d in _read_registry() if d.get("udid") != udid])
    return paired_devices()


def is_paired(udid) -> bool:
    return any(d.get("udid") == udid for d in _read_registry())


def _recv_exact(sock, n):
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise ConnectionError("usbmuxd closed the connection")
        data += chunk
    return data


def plugged_in_udids():
    """UDIDs of the iOS devices plugged into this Mac by USB, asked straight
    from usbmuxd: about a millisecond, no subprocess. A phone seen only over
    Wi-Fi does not count, the session runs over the cable.

    None when usbmuxd could not be asked (no daemon, an odd reply), so the
    caller can fall back to simply trying the phone."""
    import plistlib
    import socket
    import struct
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(2)
            s.connect("/var/run/usbmuxd")
            body = plistlib.dumps({"MessageType": "ListDevices",
                                   "ClientVersionString": "AutoCua", "ProgName": "AutoCua"})
            # usbmuxd header: total length, version 1 (plist), message 8 (plist), tag
            s.sendall(struct.pack("<IIII", 16 + len(body), 1, 8, 1) + body)
            length = struct.unpack("<IIII", _recv_exact(s, 16))[0]
            reply = plistlib.loads(_recv_exact(s, length - 16))
        devices = reply.get("DeviceList", [])
    except Exception:
        return None
    udids = set()
    for d in devices:
        props = d.get("Properties", {}) if isinstance(d, dict) else {}
        if props.get("ConnectionType") == "USB" and props.get("SerialNumber"):
            udids.add(str(props["SerialNumber"]))
    return udids


def _free_local_port(preferred):
    """`preferred` when nothing listens there, else any free port."""
    import socket
    for port in (preferred, 0):
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
                s.bind(("127.0.0.1", port))
                return s.getsockname()[1]
        except OSError:
            continue
    return preferred


# ───────────────────────────── pymobiledevice3 ────────────────────────────────
def _pmd3_candidates():
    """Every place pymobiledevice3 can legitimately live, in priority order."""
    root = Path(__file__).resolve().parents[2]   # <checkout>/AutoCua/ios_connector/
    out = []
    override = os.environ.get("AutoCua_PMD3")
    if override:
        out.append(Path(override))
    out.append(Path(sys.executable).parent / "pymobiledevice3")
    # THE VENV THE SETUP SCRIPTS INSTALL INTO. Whoever launches the app decides
    # sys.executable — a shell without the venv active, an IDE's interpreter,
    # the venv's own base python — and none of that should make the tooling
    # "missing" when it is sitting right here in the checkout.
    for venv in (".venv", "venv", "env"):
        out.append(root / venv / "bin" / "pymobiledevice3")
    out.append(Path.home() / "Desktop" / "wda_setup" / "venv" / "bin" / "pymobiledevice3")
    return out


def _pmd3_base():
    """argv prefix that runs pymobiledevice3, or None. Forgiving search:
    env override, next-to-executable, the checkout's venv, PATH, dev venv, -m."""
    for cand in _pmd3_candidates():
        try:
            if cand.exists():
                return [str(cand)]
        except OSError:
            continue
    found = shutil.which("pymobiledevice3")
    if found:
        return [found]
    try:
        import pymobiledevice3  # noqa: F401
        return [sys.executable, "-m", "pymobiledevice3"]
    except Exception:
        return None


# ─────────────────────────────── session toggle ───────────────────────────────
class WDASession:
    def __init__(self):
        self._lock = threading.Lock()
        self._forward = None
        self._xctest = None
        self._mjpeg_forward = None   # started on demand by screen_stream()
        self.mjpeg_port = None
        # Who asked for the live session last (a computer-use run's phone
        # link), so a run that ended late never stops a session the next one
        # already took over. None: the app's own toggle, the terminal.
        self._owner = None
        # The owner is only sharing the app's own Mobile use session: its
        # release hands the session back instead of stopping it.
        self._shared = False
        self._udid = None
        self._log_path = None

    # -- helpers --
    def _wda_up(self):
        try:
            with urllib.request.urlopen(WDA_STATUS_URL, timeout=3) as r:
                body = json.loads(r.read().decode("utf-8"))
                return r.status == 200 and bool(body.get("value", {}).get("state"))
        except Exception:
            return False

    @staticmethod
    def _alive(p):
        return p is not None and p.poll() is None

    def _free_port(self):
        """Kill only a LEFTOVER pymobiledevice3 forward squatting on the port."""
        try:
            out = subprocess.run(["lsof", "-nP", f"-iTCP:{WDA_PORT}", "-sTCP:LISTEN", "-t"],
                                 capture_output=True, text=True, timeout=8)
            pids = [p for p in out.stdout.split() if p.strip().isdigit()]
        except Exception:
            return
        for pid in pids:
            try:
                cmd = subprocess.run(["ps", "-p", pid, "-o", "command="],
                                     capture_output=True, text=True, timeout=5).stdout
                if "pymobiledevice3" in cmd and "forward" in cmd:
                    os.kill(int(pid), signal.SIGTERM)
            except Exception:
                pass

    def _log_tail(self):
        if not self._log_path or not os.path.exists(self._log_path):
            return ""
        try:
            with open(self._log_path, "r", errors="replace") as f:
                lines = [ln for ln in f.read().splitlines() if ln.strip()]
            return "\n".join(lines[-40:])
        except Exception:
            return ""

    @staticmethod
    def _classify(tail):
        t = tail or ""
        # Runner gone, or iOS refused to launch it. With a free Apple ID the
        # signing profile lasts 7 days and iOS then deletes it ("Profile Missing"
        # in the phone's syslog); only pairing again re-signs it. The chat
        # toggle sees "repair" and runs that pairing itself.
        if ("AppNotInstalledError" in t or "No app with bundle id" in t
                or "Failed to launch process" in t or "deviceprocesscontrolservice" in t):
            return {"code": "repair",
                    "error": "AutoCua on the iPhone needs pairing again",
                    "hint": "Settings → Connect Device → Pair device."}
        # Only pymobiledevice3's real message (printed by the mount step). Its
        # generic "Failed to start service" list also names Developer Mode, so a
        # plain substring match blamed Developer Mode for every service failure.
        if "Developer Mode is disabled" in t:
            return {"code": "devmode",
                    "error": "Enable Developer Mode on the iPhone",
                    "hint": "iPhone: Settings → Privacy & Security → Developer Mode."}
        if "Failed to start service" in t:
            return {"code": "not_ready",
                    "error": "The iPhone isn't ready for automation",
                    "hint": "Unlock it, keep the cable in, check this Mac is online, then try again."}
        last = t.splitlines()[-1][-160:] if t else "Session ended before the server started"
        return {"code": "failed", "error": last}

    # -- public --
    def activate(self, udid=None, owner=None):
        """Fresh session for `udid` (default: newest paired device). `owner`
        becomes the session's owner, also when a live one is reused."""
        base = _pmd3_base()
        if not base:
            # Name the interpreter: "not found" is almost always "found, but you
            # launched the app with a different Python", and only this line says so.
            return {"ok": False, "state": "error", "code": "no_pmd3",
                    "error": "pymobiledevice3 not found",
                    "hint": (f"running under {sys.executable} — looked beside it, in the "
                             "checkout's .venv/, and on PATH. Install it with:  bash setup_ios.sh  "
                             "or start the app with the venv's Python:  "
                             "source .venv/bin/activate && python app.py")}
        if not udid:
            devs = paired_devices()
            if not devs:
                return {"ok": False, "state": "error", "code": "not_paired",
                        "error": "No paired device",
                        "hint": "Pair your iPhone in Settings → Connect Device."}
            udid = devs[0]["udid"]
        active_target.update({"kind": "hardware", "udid": udid})
        with self._lock:
            # The app's own Mobile use session (no owner) and a computer-use
            # request's phone link (an owner). The person pairs the phone in
            # one chat and expects a computer-use request in another to use
            # it, so a live session for the same phone is SHARED: the link
            # uses it and hands it back when its request ends. One still
            # starting is never killed from under the picker (refused), and
            # one whose processes died (the registry still names the phone)
            # is replaced below like any dead session.
            if owner is not None and self._udid is not None and self._owner is None:
                if self._udid == udid and self._wda_up():
                    self._owner = owner
                    self._shared = True
                    return {"ok": True, "state": "connected", "udid": udid, "shared": True}
                if self._alive(self._xctest) and self._alive(self._forward):
                    return {"ok": False, "state": "error", "code": "in_use",
                            "error": "the phone is in use by Mobile use in the app"}
            if self._udid == udid and self._wda_up():
                self._owner = owner
                return {"ok": True, "state": "connected", "udid": udid}
            self._stop_locked()
            self._free_port()
            if self._wda_up():
                # Something else already serves WDA on the port (a leftover
                # simulator session) — refuse rather than silently drive it.
                return {"ok": False, "state": "error", "code": "port_busy",
                        "error": f"Port {WDA_PORT} is already serving another WDA "
                                 "(a simulator session?)",
                        "hint": "Close it (quit Simulator / kill xcodebuild), then retry."}
            self._udid = udid
            self._owner = owner
            env = dict(os.environ)
            env["PYMOBILEDEVICE3_UDID"] = udid
            try:
                log = tempfile.NamedTemporaryFile(prefix="AutoCua_wda_", suffix=".log", delete=False)
                self._log_path = log.name
                # The developer disk image is gone after every reboot or iOS
                # update, and xcuitest can't start without it. Mount it each
                # time: ~2s, and a no-op when it is already mounted.
                try:
                    subprocess.run(base + ["mounter", "auto-mount"], stdout=log, stderr=log,
                                   env=env, timeout=120)
                except subprocess.TimeoutExpired:
                    pass
                self._forward = subprocess.Popen(
                    base + ["usbmux", "forward", str(WDA_PORT), str(WDA_PORT)],
                    stdout=subprocess.DEVNULL, stderr=log, env=env)
                self._xctest = subprocess.Popen(
                    base + ["developer", "dvt", "xcuitest", "--userspace", WDA_BUNDLE_ID],
                    stdout=subprocess.DEVNULL, stderr=log, env=env)
            except Exception as e:
                self._stop_locked()
                return {"ok": False, "state": "error", "error": str(e)}
        return {"ok": True, "state": "connecting", "udid": udid}

    def status(self):
        with self._lock:
            if self._udid is None:
                return {"state": "disconnected"}
            if (not self._alive(self._xctest) or not self._alive(self._forward)) \
                    and not self._wda_up():
                info = self._classify(self._log_tail())
                return {"state": "error", "udid": self._udid, **info}
            state = "connected" if self._wda_up() else "connecting"
            return {"state": state, "udid": self._udid}

    def holds(self, owner):
        """True while `owner` still has its own session: it owns it, and its
        runner and cable forward are alive. A run in another process (a
        terminal "mobile use" run) that takes the phone kills this one's
        forward first, so this turns False even though WDA still answers on
        the port, now for that run. Read without the lock (cheap, called
        every step), so it can be a moment late."""
        return (owner is not None and owner is self._owner and self._udid is not None
                and self._alive(self._xctest) and self._alive(self._forward))

    def busy_elsewhere(self):
        """True when another process holds the WDA port: a terminal run's
        forward (also while its WDA is still starting and does not answer
        yet) or a simulator's WDA. activate() would kill that run's forward,
        so a caller that only wants to borrow the phone checks this first.

        A forward left behind by a process that died (its parent is gone)
        does not count: activate() clears it. This process's own session is
        activate()'s call (a link never takes the app's Mobile use session)."""
        with self._lock:
            if self._udid is not None:
                return False
            return self._port_held_by_live_process()

    @staticmethod
    def _port_held_by_live_process():
        try:
            out = subprocess.run(["lsof", "-nP", f"-iTCP:{WDA_PORT}", "-sTCP:LISTEN", "-t"],
                                 capture_output=True, text=True, timeout=8)
            pids = [p for p in out.stdout.split() if p.strip().isdigit()]
        except Exception:
            return False
        for pid in pids:
            try:
                info = subprocess.run(["ps", "-p", pid, "-o", "ppid=,command="],
                                      capture_output=True, text=True, timeout=5).stdout.strip()
            except Exception:
                return True
            if not info:
                continue                  # gone since lsof looked
            ppid, _, cmd = info.partition(" ")
            if ppid.strip() == "1" and "pymobiledevice3" in cmd and "forward" in cmd:
                continue                  # a leftover: its run died
            return True
        return False

    def screen_stream(self):
        """Local port where the live phone screen can be read (WDA's MJPEG
        stream, phone port 9100), forwarded over the cable on first call.
        None without a session or when the forward cannot start: the screen
        is only for watching, it never fails the session."""
        with self._lock:
            if self._udid is None:
                return None
            if self._alive(self._mjpeg_forward):
                return self.mjpeg_port
            base = _pmd3_base()
            if not base:
                return None
            port = _free_local_port(MJPEG_PORT)
            try:
                self._mjpeg_forward = subprocess.Popen(
                    base + ["usbmux", "forward", "--serial", self._udid, str(port), str(MJPEG_PORT)],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            except Exception:
                self._mjpeg_forward = None
                return None
            self.mjpeg_port = port
            return port

    def deactivate(self, owner=None):
        """Stop the session. With an `owner`, only if it still owns it: a
        later caller that took the session over keeps it."""
        with self._lock:
            if owner is not None and owner is not self._owner:
                return {"ok": True, "state": "kept"}
            if owner is not None and self._shared:
                # Borrowed from the app's Mobile use picker: hand it back
                # as it was, the picker still shows the phone connected.
                self._owner = None
                self._shared = False
                return {"ok": True, "state": "kept"}
            # FIRST tell the runner ON THE PHONE to exit (WDA's /wda/shutdown).
            # That's what makes the "Automation Running" overlay vanish right
            # away — killing only the Mac-side processes leaves the phone
            # session lingering ~30s until testmanagerd times it out. The
            # request never returns: WDA frees its HTTP server while still
            # replying, so the runner segfaults (a use-after-free in the
            # vendored RoutingHTTPServer). On a phone that is invisible and the
            # process was exiting anyway; the local kills below are the
            # guarantee. The SIMULATOR path deliberately does NOT do this —
            # there the same crash pops a macOS dialog (see sim_session.py).
            # An owner (a computer-use request's phone link) whose forward or
            # runner is gone lost the phone to another run: the port now
            # reaches THAT run's runner, so it must not be told to shut down.
            lost = owner is not None and not (self._alive(self._xctest) and self._alive(self._forward))
            if self._udid is not None and not lost:
                try:
                    urllib.request.urlopen(
                        f"http://127.0.0.1:{WDA_PORT}/wda/shutdown", timeout=2)
                except Exception:
                    pass
            self._stop_locked()
        return {"ok": True, "state": "disconnected"}

    def _stop_locked(self):
        # Kill INSTANTLY: xctest first (it owns the phone-side session), tiny
        # grace for a clean teardown, then SIGKILL. The whole stop stays ~1s.
        for attr in ("_xctest", "_forward", "_mjpeg_forward"):
            p = getattr(self, attr)
            if p is not None:
                try:
                    p.terminate()
                    try:
                        p.wait(timeout=1)
                    except Exception:
                        p.kill()
                except Exception:
                    pass
                setattr(self, attr, None)
        self.mjpeg_port = None
        if self._log_path:
            try:
                os.unlink(self._log_path)
            except Exception:
                pass
            self._log_path = None
        self._udid = None
        self._owner = None
        self._shared = False


wda_session = WDASession()
