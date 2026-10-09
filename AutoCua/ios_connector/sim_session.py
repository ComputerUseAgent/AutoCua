"""iOS Simulator session — the simulator twin of session.WDASession.

Same three-call contract the launcher and any UI polls:

    activate(ios_version=None, udid=None, exclude=(), device=None)
                               -> boot a simulator (picked by iOS version and
                                  device kind - iphone / ipad / exact name,
                                  newest runtime XCODE CAN BUILD TO when
                                  omitted; naming one this Mac lacks installs
                                  it first) and
                                  start WebDriverAgent on it via one command:
                                    xcodebuild test -destination id=<sim udid>
                                  (unsigned — simulators need no team id, no
                                  pairing, no pymobiledevice3). WDA then
                                  answers on http://127.0.0.1:8100, the same
                                  URL the whole agent already talks to.
    status()                   -> disconnected / connecting / connected / error.
    deactivate()               -> stop WDA (xcodebuild child), then SHUT THE
                                  SIMULATOR DOWN and quit the Simulator app —
                                  it's not a real device, nothing should
                                  linger on screen after the run.

First run compiles WebDriverAgent for the simulator (a few minutes); after
that the cached build boots + attaches in well under a minute.
"""

import json
import os
import re
import socket
import subprocess
import threading
import time
import urllib.request
from collections import deque
from pathlib import Path

from AutoCua.ios_connector.build_paths import wda_build_root
from AutoCua.ios_connector.session import WDA_PORT, active_target

_WDA_PROJECT = Path(__file__).resolve().parent / "WebDriverAgent" / "WebDriverAgent.xcodeproj"
# OUTSIDE the repo on purpose — see build_paths: a checkout on the Desktop would
# otherwise make the Simulator raise a TCC prompt mid-run, and a run that stops
# for a dialog nobody can click is not automation. Still separate from the
# hardware build/.
_DERIVED_DATA = wda_build_root() / "build-sim"

# Simulators simctl can BOOT and simulators Xcode can BUILD TO are two different
# sets: the CoreSimulator runtime store is system-wide and outlives Xcode
# upgrades, while platform support belongs to whichever Xcode is selected.
# Picking from the first set alone is how a run gets as far as a booted
# simulator and then dies on "Unable to find a destination".
_DEST_LINE = re.compile(r"platform:iOS Simulator[^}]*?\bid:([0-9A-Fa-f-]{36})")
_XCODE_LOCK = threading.RLock()   # reentrant: _pick nests inside it
_DEST_CACHE = {"udids": None, "at": 0.0}   # xcodebuild round-trip, so cache it
# ...but only a POSITIVE answer is cached for the whole process. "Xcode can
# target nothing" is a state the user fixes from another terminal mid-session
# (installing the platform), and a UI that keeps the first no forever would
# still be failing long after the machine was fixed. Re-probing costs a second.
_DEST_EMPTY_TTL = 30.0
# A run installs a runtime ONLY when the caller named an iOS version this Mac
# does not have — asking for 26.5 by name is consent to fetch 26.5. Nothing else
# downloads mid-run: with no version named, "newest" means newest of what is
# already here, and an empty set is an error pointing at ios_setup.sh, which is
# where the one-time platform install belongs. AutoCua_IOS_AUTO_DOWNLOAD=0 blocks
# even the named case.
_AUTO_DOWNLOAD = os.environ.get("AutoCua_IOS_AUTO_DOWNLOAD", "1").strip().lower() \
    not in ("0", "false", "no", "off")
_DOWNLOAD_TIMEOUT = float(os.environ.get("AutoCua_IOS_DOWNLOAD_TIMEOUT", "5400"))


def _simctl(*args, timeout=30):
    return subprocess.run(["xcrun", "simctl", *args],
                          capture_output=True, text=True, timeout=timeout)


def _runtime_version(runtime_key):
    """'com.apple.CoreSimulator.SimRuntime.iOS-26-5' -> '26.5' (None if not iOS)."""
    tail = runtime_key.rsplit(".", 1)[-1]
    if not tail.startswith("iOS-"):
        return None
    return tail[len("iOS-"):].replace("-", ".")


def _version_tuple(v):
    return tuple(int(p) for p in str(v).split(".") if p.isdigit())


def _named_simulator(udid):
    """Look one specific simulator up by udid. Returns (device, error)."""
    try:
        out = _simctl("list", "devices", "-j")
        for runtime_key, devices in json.loads(out.stdout).get("devices", {}).items():
            for d in devices:
                if d.get("udid") == udid:
                    return {"udid": udid, "name": d.get("name", "Simulator"), "family": _device_family(d),
                            "version": _runtime_version(runtime_key) or "?"}, None
    except Exception as e:
        return None, {"error": f"simctl failed: {e}"}
    return None, {"error": f"No simulator with udid {udid}",
                  "hint": "Check `xcrun simctl list devices`."}


def _targetable_udids(refresh=False):
    """UDIDs of the simulators the SELECTED Xcode can actually BUILD to.

    `xcrun simctl` reads the system-wide CoreSimulator store, so it lists — and
    happily boots — devices whose runtime the selected Xcode has no platform
    support for. xcodebuild then refuses them, and the run dies minutes later
    on "Unable to find a destination" with a simulator left on screen. Asking
    xcodebuild up front turns that into a two-second check.

    Returns a set of udids (EMPTY means Xcode can target nothing), or None when
    the probe itself could not run — a broken or slow probe must never make an
    otherwise working Mac fail, so callers read None as "don't filter".
    """
    with _XCODE_LOCK:
        cached = _DEST_CACHE["udids"]
        if not refresh and cached:
            return cached                      # something is targetable: settled
        if (not refresh and _DEST_CACHE["at"]
                and time.time() - _DEST_CACHE["at"] < _DEST_EMPTY_TTL):
            return cached                      # empty, or a failed probe, recently — don't spin
        if not _WDA_PROJECT.exists():
            return None

        def _unknown():
            # A probe that could not enumerate is remembered for the TTL too:
            # N parallel boots must not each pay (and serialize on) a 300s
            # timeout that the first one already paid.
            _DEST_CACHE.update(udids=None, at=time.time())
            return None
        try:
            out = subprocess.run(
                ["xcodebuild", "-project", str(_WDA_PROJECT),
                 "-scheme", "WebDriverAgentRunner", "-showdestinations"],
                capture_output=True, text=True, timeout=300)
        except Exception:
            return _unknown()
        text = (out.stdout or "") + "\n" + (out.stderr or "")
        if "destinations for the" not in text:
            return _unknown()  # xcodebuild could not even enumerate — don't filter
        # Only what precedes "Ineligible destinations": entries listed THERE are
        # the ones xcodebuild is refusing, each with its own error: reason.
        head = text.split("Ineligible destinations", 1)[0]
        cut = head.find("Available destinations")
        udids = {m.group(1).upper()
                 for m in _DEST_LINE.finditer(head[cut:] if cut != -1 else "")}
        _DEST_CACHE.update(udids=udids, at=time.time())
        return udids


def _download_platform(version=None, log=print):
    """Install Xcode's iOS platform — the piece that makes simulators buildable.

    Fetches the platform support the selected Xcode is missing plus its matching
    simulator runtime; -buildVersion asks for one specific iOS instead of the
    default. Returns None on success, or an error dict.
    """
    cmd = ["xcodebuild", "-downloadPlatform", "iOS"]
    if version:
        cmd += ["-buildVersion", str(version)]
    log("📱 Installing the iOS{} simulator platform — multi-GB download, this "
        "takes a while.".format(" " + str(version) if version else ""))
    log("   {}   (AutoCua_IOS_AUTO_DOWNLOAD=0 turns this off)".format(" ".join(cmd)))
    try:
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             text=True, errors="replace")
    except Exception as e:
        return {"code": "download_failed", "error": "Could not start xcodebuild: {}".format(e)}

    tail, last_echo, pending = deque(maxlen=25), 0.0, ""
    deadline = time.time() + _DOWNLOAD_TIMEOUT
    try:
        while True:
            chunk = p.stdout.read(256)
            if not chunk:
                break
            # Progress is written with carriage returns, so read by chunk and
            # split on both — iterating lines would look frozen for minutes.
            pending += chunk
            parts = re.split(r"[\r\n]", pending)
            pending = parts.pop()
            for part in (s.strip() for s in parts):
                if not part:
                    continue
                tail.append(part)
                if time.time() - last_echo > 2.0:  # percent lines arrive in floods
                    last_echo = time.time()
                    log("   " + part[:140])
            if time.time() > deadline:
                p.kill()
                return {"code": "download_failed",
                        "error": "Platform download exceeded {}s".format(int(_DOWNLOAD_TIMEOUT)),
                        "hint": "Run it yourself: " + " ".join(cmd)}
        rc = p.wait(timeout=120)
    except Exception as e:
        try:
            p.kill()
        except Exception:
            pass
        return {"code": "download_failed", "error": "Platform download failed: {}".format(e)}

    if rc != 0:
        why = " | ".join(list(tail)[-4:]) or "exit {}".format(rc)
        return {"code": "download_failed",
                "error": "xcodebuild -downloadPlatform iOS failed: " + why[:400],
                "hint": "Run it yourself (it may ask for an admin password): " + " ".join(cmd)}
    log("📱 iOS platform installed.")
    return None


_RUNNER_SUFFIX = ".xctrunner"


def _expected_runner_bundle_id():
    """Bundle id of the WebDriverAgentRunner-Runner.app THIS project builds:
    the WebDriverAgentRunner target's PRODUCT_BUNDLE_IDENTIFIER (the pairing
    setup rewrites it from com.facebook.* to com.AutoCua.*) + ".xctrunner"."""
    base = "com.facebook.WebDriverAgentRunner"
    try:
        text = _WDA_PROJECT.joinpath("project.pbxproj").read_text(encoding="utf-8", errors="replace")
        # Each build configuration block names its Info.plist; the runner's is
        # WebDriverAgentRunner/Info.plist. Read the id from such a block.
        for m in re.finditer(r"INFOPLIST_FILE = WebDriverAgentRunner/Info\.plist;(.*?)\};", text, re.S):
            # WebDriverAgentRunner_tvOS shares that Info.plist, comes first in
            # the file, and keeps com.facebook.* after pairing: reading its id
            # kept a dead runner and uninstalled the live one on every run.
            if "SDKROOT = appletvos" in m.group(1):
                continue
            hit = re.search(r"PRODUCT_BUNDLE_IDENTIFIER = ([^;]+);", m.group(1))
            if hit:
                base = hit.group(1).strip().strip('"')
                break
    except Exception:
        pass
    return base + _RUNNER_SUFFIX


def _uninstall_stale_runners(udid, keep, log=print):
    """Remove WDA runner apps on `udid` whose bundle id is not `keep`. A
    simulator that was driven both before and after the pairing setup
    rewrote the project's bundle id ends up with TWO "WebDriverAgent" icons -
    only the one this build installs is ever used."""
    try:
        listed = _simctl("listapps", udid, timeout=30)
        as_json = subprocess.run(["plutil", "-convert", "json", "-o", "-", "--", "-"],
                                 input=listed.stdout, capture_output=True, text=True, timeout=15)
        apps = json.loads(as_json.stdout) if as_json.stdout.strip() else {}
    except Exception:
        return
    for bundle_id in list(apps):
        if bundle_id.endswith("WebDriverAgentRunner" + _RUNNER_SUFFIX) and bundle_id != keep:
            try:
                _simctl("uninstall", udid, bundle_id, timeout=60)
                log(f"📱 Removed stale WebDriverAgent runner {bundle_id}")
            except Exception:
                pass


def _device_family(dev):
    """'iphone' | 'ipad' | '' for a simctl device row, a runtime
    supportedDeviceType, or a bare name. Typed fields first - a device we
    created is named "AutoCua iPhone 17 Pro", which a name prefix misses."""
    if isinstance(dev, dict):
        ident = str(dev.get("deviceTypeIdentifier") or dev.get("identifier") or "")
        fam = str(dev.get("productFamily") or "")
        for probe in (fam, ident.rsplit(".", 1)[-1]):
            p = probe.lower()
            if p.startswith("ipad"):
                return "ipad"
            if p.startswith("iphone"):
                return "iphone"
        dev = dev.get("name", "")
    n = str(dev or "").strip().lower()
    if n.startswith("AutoCua ".lower()):
        n = n[len("AutoCua "):]
    return "ipad" if n.startswith("ipad") else ("iphone" if n.startswith("iphone") else "")


def _device_spec(device):
    """Normalise SIM_DEVICE: ("family", "iphone"|"ipad") or ("name", <exact>).
    Empty / None means iphone - today's behaviour."""
    d = str(device or "").strip()
    if not d or d.lower() in ("iphone", "ipad"):
        return "family", (d.lower() or "iphone")
    return "name", d


def _device_matches(spec, dev):
    kind, value = spec
    if kind == "name":
        name = str(dev.get("name", "") if isinstance(dev, dict) else dev or "").strip().lower()
        return name in (value.lower(), ("AutoCua " + value).lower())
    return _device_family(dev) == value


def _describe_device(spec):
    kind, value = spec
    return value if kind == "name" else {"iphone": "iPhone", "ipad": "iPad"}[value]


def _ensure_device_on_runtime(version=None, log=print, device=None, exclude=()):
    """Give a freshly installed runtime a device of the requested family if it
    arrived without one.

    A runtime with no devices is invisible to every picker downstream, so a
    successful download would still look like "no simulator found".
    """
    want = str(version).strip() if version else ""
    try:
        runtimes = json.loads(_simctl("list", "runtimes", "-j").stdout).get("runtimes", [])
        devices = json.loads(_simctl("list", "devices", "available", "-j").stdout).get("devices", {})
    except Exception:
        return
    pool = [r for r in runtimes
            if r.get("isAvailable") and ".SimRuntime.iOS-" in str(r.get("identifier", ""))
            and (not want or str(r.get("version", "")) == want
                 or str(r.get("version", "")).startswith(want + "."))]
    if not pool:
        return
    rt = max(pool, key=lambda r: _version_tuple(r.get("version", "0")))
    spec = _device_spec(device)
    if any(_device_matches(spec, d) and d.get("udid") not in exclude
           for d in devices.get(rt["identifier"], [])):
        return
    dtype = next((t for t in rt.get("supportedDeviceTypes", []) if _device_matches(spec, t)), None)
    if not dtype:
        return
    name = "AutoCua {}".format(dtype["name"])
    log("📱 Creating {} — iOS {} installed with no devices.".format(name, rt.get("version")))
    try:
        _simctl("create", name, dtype["identifier"], rt["identifier"], timeout=180)
    except Exception:
        pass


def _pick(want, exclude, device=None):
    """Choose among the simulators Xcode can build to. Returns (device, error).
    `device` narrows the pool to a family ("iphone" / "ipad") or one exact
    simulator name; a booted device only counts if it matches too."""
    spec = _device_spec(device)
    try:
        out = _simctl("list", "devices", "available", "-j")
        devices_by_runtime = json.loads(out.stdout).get("devices", {})
    except Exception as e:
        return None, {"code": "simctl_failed", "error": "simctl failed: {}".format(e),
                      "hint": "Simulation needs full Xcode (xcode-select -p should point at Xcode.app)."}

    matching = []  # (version_tuple, version_str, device) — before exclude
    runtime_devices = 0   # devices of ANY kind on the runtimes `want` selects
    for runtime_key, devices in devices_by_runtime.items():
        version = _runtime_version(runtime_key)
        if version is None:
            continue
        if want and not (version == want or version.startswith(want + ".")):
            continue
        runtime_devices += len(devices)
        for d in devices:
            if _device_matches(spec, d):
                matching.append((_version_tuple(version), version, d))

    installed = sorted({v for k in devices_by_runtime if (v := _runtime_version(k))})
    # THE filter: what simctl lists is not what xcodebuild will accept. Drop the
    # rest here so a run never picks a device it cannot build to.
    targetable = _targetable_udids()

    def _usable(pool):
        return ([c for c in pool if str(c[2].get("udid", "")).upper() in targetable]
                if targetable is not None else list(pool))

    # Keep both views: whether EXCLUDE emptied the pool or Xcode did decides
    # which of the two failures a parallel run is actually looking at.
    usable = _usable(matching)
    eligible = [c for c in usable if c[2].get("udid") not in exclude]

    if eligible:
        booted = [c for c in eligible if c[2].get("state") == "Booted"]
        if booted:
            _vt, version, d = max(booted, key=lambda c: c[0])
        else:
            newest = max(c[0] for c in eligible)
            pool = [c for c in eligible if c[0] == newest]
            _vt, version, d = pool[0]
        return {"udid": d["udid"], "name": d.get("name", "Simulator"), "version": version,
                "family": _device_family(d)}, None

    what = "{} on {}".format(_describe_device(spec), "iOS {}".format(want) if want else "any iOS runtime")
    if usable:
        # Xcode can build to something — the other tasks just took them all.
        return None, {"code": "sims_exhausted",
                      "error": "Not enough simulators for {} — {} already in use by other "
                               "tasks".format(what, len(exclude)),
                      "hint": "Add devices in Xcode → Window → Devices and Simulators, or run "
                              "fewer tasks at once."}
    if matching:
        # Runtimes ARE installed and bootable; Xcode just can't build to them.
        return None, {"code": "no_targetable", "installed": installed,
                      "error": "Xcode cannot build to any installed simulator ({})".format(what),
                      "hint": "simctl lists {} and can boot them — that store is system-wide — but "
                              "the selected Xcode has no iOS platform support, so xcodebuild refuses "
                              "every one. Run  bash ios_setup.sh  — it installs the missing platform (or do "
                              "it by hand: xcodebuild -downloadPlatform iOS), and check "
                              "`xcode-select -p` points at the Xcode you expect.".format(", ".join(installed) or "no runtimes")}
    if runtime_devices:
        # The runtime is here, it just has no device of this kind - creatable,
        # nothing to download.
        return None, {"code": "no_device",
                      "error": "No simulator found for {}".format(what),
                      "hint": "That iOS runtime is installed but has no such device. Add one in "
                              "Xcode > Window > Devices and Simulators, or check the name with "
                              "`xcrun simctl list devices available`."}
    return None, {"code": "no_runtime",
                  "error": "No simulator found for {}".format(what),
                  "hint": "Installed runtimes: {}. Check `xcrun simctl list devices available` for the "
                          "device names; SIM_DEVICE takes \"iphone\", \"ipad\" or an exact name.".format(
                              ", ".join(installed) or "none")}


def resolve_simulator(ios_version=None, exclude=(), allow_download=None, log=print, device=None):
    """Pick (udid, name, version, family) for the run.

    `device` is the SIM_DEVICE setting: "iphone" (default) / "ipad" / an exact
    simulator name. Prefers an already-Booted device OF THAT KIND; otherwise
    one on the newest runtime THE SELECTED XCODE CAN BUILD TO — which is a
    smaller set than simctl lists, see _targetable_udids. `ios_version` pins a
    runtime by prefix ("26" finds 26.5); naming one this Mac does not have
    installs it. `exclude` skips udids already claimed by other parallel tasks.
    Returns (device_dict, error_dict) — exactly one is None.
    """
    want = str(ios_version).strip() if ios_version else ""
    if allow_download is None:
        allow_download = _AUTO_DOWNLOAD

    sim, err = _pick(want, exclude, device)
    if sim is None and (err.get("code") == "no_device"
                        or (err.get("code") == "sims_exhausted" and _device_spec(device)[0] == "family")):
        # Runtime installed but no FREE device of the requested kind (none at
        # all, or every one already claimed by another parallel task): create
        # one - a family request means any device of that family will do.
        with _XCODE_LOCK:
            _ensure_device_on_runtime(want, log=log, device=device, exclude=exclude)
            _targetable_udids(refresh=True)
        sim, err2 = _pick(want, exclude, device)
        return sim, (None if sim else err2)
    # A run downloads exactly one thing: a runtime the caller NAMED and this Mac
    # does not have. Not "named but unbuildable" — that runtime is already on
    # disk and refetching it changes nothing. Not the missing platform either:
    # that is a one-time ~8 GB install and belongs to ios_setup.sh, never to a
    # surprise mid-run.
    if sim or not want or not allow_download or err.get("code") != "no_runtime":
        return sim, err
    # And a runtime only helps if Xcode can build to simulators at all. When it
    # can build to none, the platform is what's missing — say so instead of
    # spending gigabytes on a runtime that will be refused too.
    targetable = _targetable_udids()
    if targetable is not None and not targetable:
        return None, {**err, "hint": (err.get("hint", "") + "  Note: Xcode cannot build to ANY "
                      "simulator on this Mac, so installing a runtime alone will not help — run  "
                      "bash ios_setup.sh  first, it installs the missing iOS platform.").strip()}

    with _XCODE_LOCK:
        if _pick(want, exclude, device)[0] is None:  # another task may have just done it
            derr = _download_platform(want or None, log=log)
            if derr:
                return None, {**err, "hint": " ".join(
                    p for p in (derr.get("error"), derr.get("hint")) if p)}
            _ensure_device_on_runtime(want, log=log, device=device)
            _targetable_udids(refresh=True)

    sim, err = _pick(want, exclude, device)
    return sim, (None if sim else err)


def _require_targetable(sim, log=print):
    """None when Xcode can build to this exact device, else an error dict.

    Only the pinned-udid path needs this — resolve_simulator already filters.
    Pinning a udid names a device that is already installed, so there is
    nothing to download here: either Xcode can build to it or it cannot.
    """
    targetable = _targetable_udids()
    if targetable is None or str(sim.get("udid", "")).upper() in targetable:
        return None
    return {"code": "no_targetable",
            "error": "Xcode cannot build to {} (iOS {})".format(
                sim.get("name", "that simulator"), sim.get("version", "?")),
            "hint": "simctl can boot it, but the selected Xcode has no iOS platform support for "
                    "that runtime. Install it with:  xcodebuild -downloadPlatform iOS"}


class SimulatorSession:
    def __init__(self, port=None, slot=1):
        self._lock = threading.Lock()
        self._xcodebuild = None
        self._udid = None
        self._name = None
        self._log_path = None
        # Parallel tasks each own a simulator + a WDA port (8100, 8101, ...).
        self.port = int(port or WDA_PORT)
        # ...and the port of WDA's live screen stream (9100, 9101, ...), which
        # the simulator window (sim_view) shows. Left unset, every WDA would
        # try 9100 and only the first would get it.
        self.mjpeg_port = self.port + 1000
        # ...and their own build folder. Concurrent xcodebuilds sharing one
        # derived-data dir re-sign the same .app underneath each other, which
        # kills the other run's runner ("crashed with signal term before
        # establishing connection"). Slot 1 keeps the default folder, so a
        # single-task run reuses the build it already has.
        self.derived_data = (_DERIVED_DATA if int(slot) <= 1
                             else Path(f"{_DERIVED_DATA}-{int(slot)}"))

    # -- helpers (same shapes as WDASession) --
    def _wda_up(self):
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/status", timeout=3) as r:
                body = json.loads(r.read().decode("utf-8"))
                return r.status == 200 and bool(body.get("value", {}).get("state"))
        except Exception:
            return False

    def _port_owner(self, port=None):
        """Command line of whatever LISTENs on this session's WDA port (or
        `port`) ('' if free).

        Identifies WHO answers there: a pymobiledevice3 forward (paired
        iPhone), or a simulator WDA runner — whose binary path contains
        CoreSimulator/Devices/<udid>, so it names its simulator."""
        try:
            out = subprocess.run(["lsof", "-nP", f"-iTCP:{port or self.port}", "-sTCP:LISTEN", "-t"],
                                 capture_output=True, text=True, timeout=8)
            for pid in out.stdout.split():
                cmd = subprocess.run(["ps", "-p", pid, "-o", "command="],
                                     capture_output=True, text=True, timeout=5).stdout
                if cmd.strip():
                    return cmd.strip()
        except Exception:
            pass
        return ""

    def _log_tail(self):
        if not self._log_path or not os.path.exists(self._log_path):
            return ""
        try:
            with open(self._log_path, "r", errors="replace") as f:
                lines = [ln for ln in f.read().splitlines() if ln.strip()]
            # Generous window: xcodebuild's destination report alone runs to
            # dozens of lines on a Mac with several runtimes, and a short tail
            # would scroll the explanation off and misclassify the failure.
            return "\n".join(lines[-200:])
        except Exception:
            return ""

    @staticmethod
    def _classify(tail):
        t = tail or ""
        # CoreSimulator occasionally wedges (usually after killed runs). Both
        # of these mean "the simulator service is unhealthy", not "your code or
        # your task is wrong" — and both clear with the same reset.
        if ("timed out while preparing to run tests" in t
                or "server died" in t
                or "Failed to install or launch the test runner" in t):
            return {"code": "sim_wedged",
                    "error": "The simulator's test service is wedged (a known flake)",
                    "hint": "Run: xcrun simctl shutdown all && pkill -9 -f CoreSimulatorService — then retry."}
        if "matching the provided destination specifier" in t:
            # xcodebuild prints the eligible/ineligible destinations right after
            # this line, and an "error:" reason on each ineligible one — that
            # block is the only thing that actually explains the failure, so
            # pass it through instead of guessing. Keep the informative rows
            # (iOS Simulator entries, reasons) and drop the macOS/placeholder
            # noise that would otherwise eat the whole budget.
            lines = [ln.strip() for ln in t.splitlines()]
            start = next((i for i, ln in enumerate(lines)
                          if "matching the provided destination specifier" in ln), 0)
            window = lines[start:start + 40]
            keep = [ln for ln in window
                    if ("iOS Simulator" in ln or "error:" in ln or "Ineligible" in ln
                        or "Available destinations" in ln or "could not be found" in ln
                        or ln.startswith("xcodebuild:"))]
            block = "\n    ".join(keep or window)[:1200]
            return {"code": "no_destination",
                    "error": "xcodebuild refused to use this simulator as a test destination",
                    "hint": ("simctl can see the device but Xcode cannot target it — usually the "
                             "selected Xcode has no iOS platform support installed for that "
                             "runtime. Try:  xcodebuild -downloadPlatform iOS   (or Xcode → "
                             "Settings → Components), and check `xcode-select -p` points at the "
                             "Xcode you expect.\n  xcodebuild said:\n    " + block)}
        if "xcodebuild: error" in t or "BUILD FAILED" in t or "TEST FAILED" in t:
            last = next((ln for ln in reversed(t.splitlines()) if "error" in ln.lower()), t.splitlines()[-1])
            return {"code": "build_failed", "error": last[-200:]}
        last = t.splitlines()[-1][-160:] if t else "xcodebuild exited before WDA started"
        return {"code": "failed", "error": last}

    @staticmethod
    def _device_state(udid):
        try:
            out = _simctl("list", "devices", "-j")
            for devs in json.loads(out.stdout).get("devices", {}).values():
                for d in devs:
                    if d.get("udid") == udid:
                        return d.get("state", "")
        except Exception:
            pass
        return ""

    def _cancelled(self, udid):
        """deactivate() clears _udid; a boot thread that sees that must stop."""
        return self._udid != udid

    def _boot(self, udid, name):
        state = self._device_state(udid)
        # A device still winding down from a previous run can't be booted yet
        # ("Unable to boot device in current state: Shutting Down").
        waited = 0.0
        while state == "Shutting Down" and waited < 60.0 and not self._cancelled(udid):
            time.sleep(1.0)
            waited += 1.0
            state = self._device_state(udid)
        if state != "Booted":
            print(f"📱 Booting {name}...")
            try:
                r = _simctl("boot", udid, timeout=180)
                # rc 149 means "already booted", which is fine; anything else
                # non-zero is a real failure worth reporting rather than
                # discovering later as a confusing xcodebuild error.
                if r.returncode not in (0, 149):
                    why = (r.stderr or r.stdout or "").strip().splitlines()
                    return {"ok": False, "state": "error", "code": "boot_failed",
                            "error": f"simctl could not boot {name}: "
                                     f"{why[-1][:200] if why else 'unknown error'}",
                            "hint": f"Try it by hand: xcrun simctl boot {udid}"}
            except subprocess.TimeoutExpired:
                pass  # boot continues inside CoreSimulator
        try:
            # ALWAYS confirm readiness, even when the device already claimed to
            # be "Booted": that state goes stale while a simulator is still
            # coming up or going down, and xcodebuild launching into a
            # half-alive one dies with "Mach error -308 - server died".
            # No -b: that flag BOOTS a device that isn't booted, which would
            # resurrect one deactivate() just shut down under a cancelled boot.
            _simctl("bootstatus", udid, timeout=300)
        except subprocess.TimeoutExpired:
            pass  # fall through to the explicit state check below
        # No Simulator.app: the simulator runs headless, and the launcher
        # shows the run in its own window instead (sim_view).
        # Don't hand a device that isn't actually up to xcodebuild — on a fresh
        # Mac the first boot of a runtime can outlast bootstatus, and the
        # failure then surfaces as a confusing destination/launch error. Poll
        # rather than sample once: a single reading can catch a transient
        # simctl hiccup (_device_state returns "" on any error).
        deadline = time.time() + 120.0
        state = self._device_state(udid)
        while state != "Booted" and time.time() < deadline and not self._cancelled(udid):
            time.sleep(2.0)
            state = self._device_state(udid)
        if self._cancelled(udid):
            return {"ok": False, "state": "error", "code": "cancelled",
                    "error": f"{name}: boot cancelled"}
        if state != "Booted":
            return {"ok": False, "state": "error", "code": "boot_failed",
                    "error": f"{name} did not finish booting (state: {state or 'unknown'})",
                    "hint": "Open Simulator manually and boot the device once, "
                            "then retry — the first boot of a new runtime is slow."}
        return None

    # -- public --
    def activate(self, ios_version=None, udid=None, exclude=(), device=None):
        """Boot a simulator and start WDA on this session's port.

        ios_version picks the runtime (newest Xcode can build to when omitted,
        downloaded when named and missing); device picks the simulator kind
        ("iphone" - the default - / "ipad" / an exact simulator name); udid
        pins an exact device and exclude skips devices other parallel tasks
        already claimed."""
        # EVERY simulator run passes through activate() — one task, N parallel
        # tasks, each parallel child process, the desktop app — so this is the
        # one place worth fetching WebDriverAgent from. ios_setup.sh clones it
        # for a checkout; a pip install runs no shell script, so it is cloned
        # here instead, once, the first time a simulator is asked for.
        if not _WDA_PROJECT.exists():
            from AutoCua.utils.wda import ensure_wda, REPO, VERSION
            ensure_wda()
            if not _WDA_PROJECT.exists():
                # Don't send a pip user to ios_setup.sh: the wheel holds the
                # AutoCua package and nothing else, so that script is not on
                # their disk to run. Spell out the one command that is.
                return {"ok": False, "state": "error",
                        "error": "WebDriverAgent project not found",
                        "hint": f"Automatic fetch failed. Clone it by hand:  git clone --depth 1 "
                                f"--branch {VERSION} {REPO} {_WDA_PROJECT.parent}"}
        owner = self._port_owner()
        if "pymobiledevice3" in owner and "forward" in owner:
            return {"ok": False, "state": "error", "code": "hardware_active",
                    "error": f"A paired-iPhone session is using port {self.port}",
                    "hint": 'Disconnect the phone session first, or run with device="hardware".'}

        if udid:
            sim, err = _named_simulator(udid)
            if not err:
                err = _require_targetable(sim)
        else:
            sim, err = resolve_simulator(ios_version, exclude=exclude, device=device)
        if err:
            return {"ok": False, "state": "error", **err}

        with self._lock:
            if (self._udid == sim["udid"] and self._alive(self._xcodebuild)
                    and self._wda_up()):
                active_target.update({"kind": "simulation", "udid": sim["udid"]})
                return {"ok": True, "state": "connected", **sim}
            self._stop_locked()
            self._udid, self._name = sim["udid"], sim["name"]

        boot_error = self._boot(sim["udid"], f'{sim["name"]} (iOS {sim["version"]})')
        if boot_error:
            # Don't leave a half-booted simulator behind: the caller raises on
            # this without getting a session object to deactivate.
            try:
                _simctl("shutdown", sim["udid"], timeout=60)
            except Exception:
                pass
            with self._lock:
                self._udid = self._name = None
            return boot_error
        if self._cancelled(sim["udid"]):
            # deactivate() ran while we were booting (Ctrl+C in a parallel
            # run): leave nothing behind and start no runner.
            try:
                _simctl("shutdown", sim["udid"], timeout=60)
            except Exception:
                pass
            return {"ok": False, "state": "error", "code": "cancelled",
                    "error": f'{sim["name"]}: activation cancelled'}
        active_target.update({"kind": "simulation", "udid": sim["udid"]})
        # One runner per simulator: drop any left by a build with another id.
        _uninstall_stale_runners(sim["udid"], _expected_runner_bundle_id())

        # Something already serves WDA on the port. Reuse it ONLY if it is a
        # runner living inside OUR simulator (its binary path names the udid);
        # anything else would silently drive the wrong device — refuse instead.
        if self._wda_up():
            owner = self._port_owner()
            if f"CoreSimulator/Devices/{sim['udid']}" in owner:
                return {"ok": True, "state": "connected", **sim}
            return {"ok": False, "state": "error", "code": "port_busy",
                    "error": f"Port {self.port} is serving a WDA that is not on "
                             f"{sim['name']} (iOS {sim['version']})",
                    "hint": f"Kill the leftover process (`lsof -nP -iTCP:{self.port}"
                            "` shows its pid), then retry."}

        with self._lock:
            if self._cancelled(sim["udid"]):
                # Same race, later: never spawn a runner on a session that has
                # already been torn down - the child would outlive the run and
                # boot the simulator back up by itself.
                try:
                    _simctl("shutdown", sim["udid"], timeout=60)
                except Exception:
                    pass
                return {"ok": False, "state": "error", "code": "cancelled",
                        "error": f'{sim["name"]}: activation cancelled'}
            try:
                # A stable, discoverable path beside the build — a first-time
                # user should not have to go spelunking in /var/folders for the
                # one file that explains a failed run.
                self.derived_data.mkdir(parents=True, exist_ok=True)
                self._log_path = str(self.derived_data / f"wda-{self.port}.log")
                log = open(self._log_path, "w")
                # Something else on the screen-stream port (9100 is also
                # node_exporter's default) would leave the simulator window
                # blank all run, and WDA only logs it. Take a free port then.
                self.mjpeg_port = self.port + 1000
                if self._port_owner(self.mjpeg_port):
                    with socket.socket() as s:
                        s.bind(("", 0))
                        self.mjpeg_port = s.getsockname()[1]
                    print(f"📱 Port {self.port + 1000} is taken; the screen stream "
                          f"uses {self.mjpeg_port}")
                # Unsigned simulator build+run — the recipe WebDriverAgent's own
                # Scripts/build.sh uses. `test` leaves WDA serving until we kill it.
                self._xcodebuild = subprocess.Popen(
                    ["xcodebuild",
                     "-project", str(_WDA_PROJECT),
                     "-scheme", "WebDriverAgentRunner",
                     # Fully qualified on purpose: a bare "id=..." leaves the
                     # platform for xcodebuild to infer, which can fail to
                     # match ("Unable to find a destination") on machines
                     # where physical-device destinations are in play.
                     "-destination", f"platform=iOS Simulator,id={sim['udid']}",
                     "-derivedDataPath", str(self.derived_data),
                     "CODE_SIGN_IDENTITY=", "CODE_SIGNING_REQUIRED=NO",
                     "CODE_SIGNING_ALLOWED=NO",
                     "test"],
                    stdout=log, stderr=subprocess.STDOUT,
                    # WDA reads USE_PORT and binds exactly that port — this is
                    # what lets parallel tasks each own a simulator + a port.
                    # MJPEG_SERVER_PORT reaches it the same way (the scheme
                    # maps both) and gives each one its own screen stream.
                    env={**os.environ, "USE_PORT": str(self.port),
                         "MJPEG_SERVER_PORT": str(self.mjpeg_port)})
            except Exception as e:
                self._stop_locked()
                failure = str(e)
            else:
                failure = None
        if failure is not None:
            # _stop_locked cleared _udid, so a later deactivate() would leave
            # this simulator booted — shut down the one we just booted here.
            try:
                _simctl("shutdown", sim["udid"], timeout=60)
            except Exception:
                pass
            return {"ok": False, "state": "error", "error": failure}
        return {"ok": True, "state": "connecting", **sim}

    @staticmethod
    def _alive(p):
        return p is not None and p.poll() is None

    def status(self):
        with self._lock:
            if self._udid is None:
                return {"state": "disconnected"}
            if not self._alive(self._xcodebuild) and not self._wda_up():
                info = self._classify(self._log_tail())
                if self._log_path:
                    # ALWAYS append the log path — the classified hint used to
                    # replace it, which left the failures users actually hit
                    # with no way to see what xcodebuild said.
                    info["hint"] = " ".join(
                        p for p in (info.get("hint"),
                                    f"Full xcodebuild log: {self._log_path}") if p)
                return {"state": "error", "udid": self._udid, "name": self._name, **info}
            state = "connected" if self._wda_up() else "connecting"
            return {"state": state, "udid": self._udid, "name": self._name}

    def deactivate(self):
        with self._lock:
            udid = self._udid
            # NO /wda/shutdown here (the hardware session uses it to clear the
            # on-phone overlay). On a simulator it CRASHES the runner —
            # WDA tears its HTTP server down while still writing the reply to
            # that very request, and the SIGSEGV pops macOS's "WebDriverAgent
            # Runner-Runner quit unexpectedly" dialog. Killing xcodebuild
            # instead lets XCTest end the session with a plain SIGTERM, which
            # generates no crash report.
            self._stop_locked()
        # Not a real device — shut it down so nothing lingers after the run,
        # and close the Simulator app unless another simulator is still up.
        if udid:
            try:
                _simctl("shutdown", udid, timeout=60)
            except Exception:
                pass
            try:
                out = _simctl("list", "devices", "booted")
                if "(Booted)" not in out.stdout:
                    subprocess.run(["osascript", "-e", 'quit app "Simulator"'],
                                   capture_output=True, text=True, timeout=10)
            except Exception:
                pass
        return {"ok": True, "state": "disconnected"}

    def _stop_locked(self):
        p = self._xcodebuild
        if p is not None:
            try:
                p.terminate()
                try:
                    p.wait(timeout=3)
                except Exception:
                    p.kill()
            except Exception:
                pass
            self._xcodebuild = None
        # The log file is deliberately KEPT (unlike the hardware session) —
        # error hints point users at it, and xcodebuild logs are the only
        # way to debug a failed simulator build.
        self._log_path = None
        self._udid = None
        self._name = None


sim_session = SimulatorSession()
