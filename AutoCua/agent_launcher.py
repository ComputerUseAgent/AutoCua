# Picks the right AgentService for the requested mode:
#   mode = "computer use"     -> desktop agent for the host OS (mac / windows / linux)
#   mode = "shell use"        -> CLI/coder agent for the host OS, straight to the
#                                terminal (skips the main agent layer)
#   mode = "mobile use, ios"  -> iOS agent (iPhone or iPad). By default it runs on an iOS
#                                SIMULATOR (device="simulation", ios_version
#                                picks the runtime); pass device="hardware" to
#                                drive the paired iPhone/iPad instead. Either way the
#                                WDA connection is opened before the run and
#                                closed after it (ios_connector).
#   mode = "mobile use, android" -> Android agent (AutoCua/android, Rust like the
#                                web agent). By default it runs on an Android
#                                EMULATOR (device="simulation", android_version
#                                picks the Android, sim_device "phone"/"tablet";
#                                android_connector/emulator.py); pass
#                                device="hardware" to drive the phone on USB.
#                                Either way adb and the phone helper are started
#                                before the run and stopped after it
#                                (android_connector/session.py).
#                                AutoCua_ANDROID_SERIAL picks one phone when
#                                several are plugged in.
#   mode = "web use"          -> browser agent driving CDP-controlled Chrome
#                                (AutoCua/web). Host OS independent.
import atexit
import inspect
import json
import os
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

# Repo root (the folder holding main.py and the AutoCua package) — parallel
# children run with cwd inside ./parallel/task_N/, so they need this on
# PYTHONPATH to import AutoCua.
_REPO_ROOT = Path(__file__).resolve().parents[1]


def _parse_mode(mode, os=None):
    """Split a mode string into (kind, device), e.g. "mobile use, ios" -> ("mobile", "ios")."""
    words = str(mode).strip().lower().replace(",", " ").replace("_", " ").split()
    words = [w for w in words if w != "use"]
    kind = words[0] if words else ""
    device = words[1] if len(words) > 1 else str(os or "").strip().lower()
    return kind, device


def resolve_agent_service(mode, os=None):
    """Return the AgentService class for the given mode.

    mode: "computer use" / "shell use" / "web use" (host OS auto-detected or
          irrelevant, nothing else needed) or "mobile use, ios" /
          "mobile use, android" — the device os rides along in the same string.
          Underscores/commas/case don't matter.
    os:   optional separate device os ("ios"/"android") for callers that
          don't embed it in mode; the embedded one wins if both are given.
    """
    kind, os = _parse_mode(mode, os)

    if kind == "computer":
        host = platform.system()
        if host == "Darwin":
            from AutoCua.mac.agent.computer_use.service import AgentService
        elif host == "Windows":
            from AutoCua.windows.agent.computer_use.service import AgentService
        elif host == "Linux":
            from AutoCua.linux.agent.computer_use.service import AgentService
        else:
            raise RuntimeError(f"Unsupported OS for computer use: {host}")
        return AgentService

    if kind == "shell":
        host = platform.system()
        if host == "Darwin":
            from AutoCua.mac.agent.coder import AgentService
        elif host == "Windows":
            from AutoCua.windows.agent.coder import AgentService
        elif host == "Linux":
            from AutoCua.linux.agent.coder import AgentService
        else:
            raise RuntimeError(f"Unsupported OS for shell use: {host}")
        return AgentService

    if kind == "web":
        # Browser agent — no host-OS branch: it drives Chrome over CDP, which
        # is the same on every platform. The implementation is the Rust
        # extension in AutoCua/web/agent (agent_native), compiled lazily by
        # that package's __init__ on first import.
        from AutoCua.web.agent import AgentService
        return AgentService

    if kind == "mobile":
        device = str(os or "").strip().lower()
        if device == "ios":
            from AutoCua.ios.agent.iphone_use.service import AgentService
            return AgentService
        if device == "android":
            # The Rust extension in AutoCua/android (agent_native), compiled
            # lazily by that package's agent/__init__ on first import, like
            # the web agent's.
            from AutoCua.android.agent import AgentService
            return AgentService
        raise ValueError('mobile use needs a device — "mobile use, ios" or "mobile use, android"')

    raise ValueError(
        f'Unknown mode "{mode}" — use "computer use", "shell use", "web use", '
        '"mobile use, ios" or "mobile use, android"'
    )


def _supported_kwargs(AgentService, kwargs):
    """Keep only the kwargs this AgentService actually takes.

    One call site serves every mode, but the services differ: the mobile agent
    has no external_terminal, the shell-use (coder) agent has no speed. Anything
    the target doesn't declare is dropped instead of raising TypeError.
    """
    params = inspect.signature(AgentService.__init__).parameters
    if any(p.kind is inspect.Parameter.VAR_KEYWORD for p in params.values()):
        return kwargs
    return {k: v for k, v in kwargs.items() if k in params}


def _connect_iphone(poll_every=2.0, deadline=90.0):
    """Open the iPhone WDA session and wait until it answers (the app's mode-dial flow).

    Returns the live wda_session; raises RuntimeError if the phone can't be reached.
    """
    from AutoCua.ios_connector.session import wda_session

    res = wda_session.activate()
    if not res.get("ok"):
        detail = " — ".join(
            p for p in (res.get("error"), res.get("hint")) if p) or res.get("code") or res
        raise RuntimeError(f"📱 iPhone connection failed: {detail}")
    if res.get("state") == "connected":
        print(f"📱 iPhone already connected ({res.get('udid', '')})")
        return wda_session

    print("📱 Connecting iPhone...")
    end = time.time() + deadline
    while time.time() < end:
        time.sleep(poll_every)
        try:
            st = wda_session.status()
        except Exception:
            continue  # transient probe hiccup, keep polling (matches the UI)
        state = st.get("state")
        if state == "connected":
            print(f"📱 Paired device connected ({st.get('udid', '')})")
            return wda_session
        if state in ("error", "disconnected"):
            wda_session.deactivate()  # reap half-started processes, like the UI's fail path
            detail = st.get("hint") or st.get("error") or st.get("code") or state
            raise RuntimeError(f"📱 iPhone connection failed: {detail}")

    wda_session.deactivate()
    raise RuntimeError(f"📱 iPhone connection timed out after {int(deadline)}s")


def _connect_android(serial=None):
    """Start the Android phone's helper (adb, then Helper.java: screenshots and the tree)
    and wait until the phone is ready. The counterpart of _connect_iphone.
    serial: the emulator's, for a device="simulation" run.

    Returns the live android_session; raises RuntimeError if the phone can't
    be reached (adb.py already said what to do on the phone meanwhile).
    """
    from AutoCua.android_connector.session import android_session

    print("📱 Connecting Android phone...")
    res = android_session.activate(serial)
    if not res.get("ok"):
        raise RuntimeError(f"📱 Android connection failed: {res.get('error')}")
    print(f"📱 Android phone connected ({res.get('model')}, {res.get('serial')})")
    return android_session


# The port the Android agent serves its screen on as MJPEG (android/tree/
# element.rs), for the run's window: set before the agent is built.
PREVIEW_ENV = "AutoCua_ANDROID_PREVIEW_PORT"


def _pin_android_serial(serial):
    """Set AutoCua_ANDROID_SERIAL to serial (None unsets it); returns what it
    was. A simulation run pins its emulator there, so the screen reader, when
    it starts the phone helper again mid-run, stays on that emulator and never
    takes a phone that is plugged in too. The run puts the old value back."""
    from AutoCua.android_connector.session import SERIAL_ENV

    before = os.environ.get(SERIAL_ENV)
    if serial is None:
        os.environ.pop(SERIAL_ENV, None)
    else:
        os.environ[SERIAL_ENV] = serial
    return before


def _connect_emulator(android_version=None, sim_device=None, build=None):
    """Boot an Android emulator (sim_device: "phone" / "tablet") for the run,
    fetching Google's emulator and that Android the first time, and install
    build (from _android_app_build) once Android has started. The counterpart
    of _connect_simulator.

    Returns the live EmulatorSession; raises RuntimeError if it can't start.
    """
    from AutoCua.android_connector.emulator import EmulatorSession

    emulator = EmulatorSession()
    res = emulator.activate(android_version, device=sim_device)
    if not res.get("ok"):
        raise RuntimeError(f"📱 Emulator failed to start: {res.get('error')}")
    print(f"📱 Emulator ready — Android {res['version']} {res['device']} ({res['serial']})")
    if build:
        try:
            _install_apk(res["serial"], build)
        except BaseException:
            emulator.deactivate()
            raise
    return emulator


def _connect_simulator(ios_version=None, poll_every=2.0, deadline=600.0, sim_device=None,
                       build=None):
    """Boot an iOS Simulator (sim_device: "iphone" / "ipad" / exact name) and
    start WDA on it, then wait until it answers.

    Mirrors _connect_iphone (same activate/status/deactivate contract). The
    deadline is generous because the FIRST run compiles WebDriverAgent for the
    simulator (a few minutes); after that it attaches in seconds.

    build (from _unpack_app_build) is installed the moment the simulator has
    booted, while WDA starts; this returns only once both are done.
    """
    from AutoCua.ios_connector.sim_session import sim_session

    res = sim_session.activate(ios_version, device=sim_device)
    if not res.get("ok"):
        detail = " — ".join(
            p for p in (res.get("error"), res.get("hint")) if p) or res.get("code") or res
        raise RuntimeError(f"📱 Simulator connection failed: {detail}")
    label = f"{res.get('name', 'Simulator')} (iOS {res.get('version', '?')})"
    # activate() returns once the simulator has booted — start the app install
    # now so it runs alongside the WDA start below instead of after it.
    install = _install_in_background(res["udid"], build) if build else None

    def ready():
        # WDA answers; the run still waits for the install to finish.
        if install:
            try:
                install()
            except BaseException:
                sim_session.deactivate()
                raise
        return sim_session

    if res.get("state") == "connected":
        print(f"📱 Simulator already connected — {label}")
        return ready()

    print(f"📱 Starting WebDriverAgent on {label} — first run builds it, a few minutes...")
    end = time.time() + deadline
    try:
        while time.time() < end:
            time.sleep(poll_every)
            try:
                st = sim_session.status()
            except Exception:
                continue  # transient probe hiccup, keep polling
            state = st.get("state")
            if state == "connected":
                print(f"📱 Simulator connected — {label}")
                return ready()
            if state in ("error", "disconnected"):
                sim_session.deactivate()
                detail = " — ".join(
                    p for p in (st.get("error"), st.get("hint")) if p) or st.get("code") or state
                raise RuntimeError(f"📱 Simulator connection failed: {detail}")
    except KeyboardInterrupt:
        # Ctrl+C mid-connect — don't leave the xcodebuild/WDA child running.
        sim_session.deactivate()
        raise

    sim_session.deactivate()
    raise RuntimeError(f"📱 Simulator connection timed out after {int(deadline)}s")


def _unpack_app_build(app_build):
    """Get AutoCua_data/app_build/<app_build> (.app or .ipa) ready to install.

    Returns (file name, .app path, bundle id). An .ipa is unpacked ONCE here,
    before any simulator boots, so every simulator installs from the same copy
    (removed when the process exits). Raises if the build is missing or is not
    an app — the agent can't run without it.
    """
    from AutoCua import data_root

    path = data_root() / "app_build" / app_build
    if not path.exists():
        raise RuntimeError(f"📱 app_build not found: {path} — the agent can't run without it")
    app = path
    if path.suffix.lower() == ".ipa":
        # simctl only installs a .app; an .ipa is a zip of Payload/<Name>.app.
        # ditto (not zipfile) keeps the executable bit on the app binary.
        tmp = tempfile.mkdtemp(prefix="AutoCua_app_build_")
        atexit.register(shutil.rmtree, tmp, ignore_errors=True)
        subprocess.run(["ditto", "-x", "-k", str(path), tmp], check=True)
        app = next((Path(tmp) / "Payload").glob("*.app"), None)
        if app is None:
            raise RuntimeError(f"📱 no .app inside {path.name}")
    try:
        with open(app / "Info.plist", "rb") as f:
            bundle_id = plistlib.load(f)["CFBundleIdentifier"]
    except (OSError, KeyError, plistlib.InvalidFileException):
        raise RuntimeError(f"📱 {path.name} has no readable bundle id — not an app build")
    return path.name, app, bundle_id


def _install_app_build(udid, build, deadline=120.0):
    """Install an unpacked build on one simulator and wait until the simulator
    lists it as installed. Raises if it never is."""
    name, app, bundle_id = build
    print(f"📱 Installing {name} on simulator {udid}...")
    res = subprocess.run(["xcrun", "simctl", "install", udid, str(app)],
                         capture_output=True, text=True)
    if res.returncode != 0:
        raise RuntimeError(f"📱 Installing {name} failed: {(res.stderr or res.stdout).strip()}")
    # Don't take simctl's word for it: the run only starts once the simulator
    # itself lists the app as installed.
    end = time.time() + deadline
    while subprocess.run(["xcrun", "simctl", "get_app_container", udid, bundle_id],
                         capture_output=True).returncode != 0:
        if time.time() > end:
            raise RuntimeError(f"📱 {name} ({bundle_id}) is not installed on simulator "
                               f"{udid} — the agent can't run without it")
        time.sleep(1.0)
    print(f"📱 {name} installed on simulator {udid}")


def _install_in_background(udid, build):
    """Run _install_app_build on its own thread. Returns wait(), which blocks
    until the install is done and re-raises whatever made it fail."""
    failure = []

    def work():
        try:
            _install_app_build(udid, build)
        except BaseException as e:
            failure.append(e)

    thread = threading.Thread(target=work, daemon=True)
    thread.start()

    def wait():
        thread.join()
        if failure:
            raise failure[0]

    return wait


def _android_app_build(app_build):
    """AutoCua_data/app_build/<app_build>, an .apk, ready to install on an
    emulator: (file name, path). Raises if it is missing or not an .apk — the
    agent can't run without it."""
    from AutoCua import data_root

    path = data_root() / "app_build" / app_build
    if not path.is_file():
        raise RuntimeError(f"📱 app_build not found: {path} — the agent can't run without it")
    if path.suffix.lower() != ".apk":
        raise RuntimeError(f"📱 app_build for Android must be an .apk — got {path.name}")
    return path.name, path


def _install_apk(serial, build):
    """Install the .apk on one emulator. adb install waits until Android has
    it, and says Success; anything else raises."""
    from AutoCua.android_connector.adb import _adb, adb_path

    name, path = build
    print(f"📱 Installing {name} on {serial}...")
    res = _adb(adb_path(), "-s", serial, "install", "-r", str(path), timeout=600)
    if res.returncode != 0 or "Success" not in res.stdout:
        raise RuntimeError(f"📱 Installing {name} failed: {(res.stderr or res.stdout).strip()}")
    print(f"📱 {name} installed on {serial}")


def _device_target(device):
    """Normalise the device flag to "simulation" (default) or "hardware"."""
    target = str(device or "simulation").strip().lower()
    if target == "simulator":
        target = "simulation"
    if target not in ("simulation", "hardware"):
        raise ValueError(f'device must be "simulation" or "hardware" — got "{device}"')
    return target


def run_agent(mode, provider, model, task, os=None,
              device=None, ios_version=None, sim_device=None, app_build=None,
              android_version=None,
              save_conversation=False, external_terminal=False,
              extra_tasks=None, ui=False, **agent_kwargs):
    """Create the AgentService for mode/os, run the task, return its response dict.

    ui: True opens the desktop app (AutoCua/ui) instead of running
    `task` in the terminal. Every other argument is ignored — the app has its
    own mode/model/task controls — and the call returns once its window closes.
    It opens in the Chromium shell (AutoCua/desktop) when Electron is here
    (a checkout's `npm install`, or AutoCua/utils/electron.py's download,
    which the first launch asks about on the terminal), else in the
    pywebview window; AUTOCUA_UI=webview forces the pywebview one.

    device ("mobile use, ios" and "mobile use, android"): "simulation" (the
    default) runs on an iOS Simulator — ios_version picks the runtime (e.g.
    "26.5"; omitted = newest installed) and sim_device picks the simulator:
    "iphone" (default), "ipad", or an exact name such as "iPad Pro 11-inch
    (M5)" (an already-booted simulator is reused only if it is that kind).
    "hardware" drives the paired device - iPhone or iPad, whichever is paired
    - with whatever iOS it runs (ios_version and sim_device are ignored).
    On Android "simulation" runs on a fresh copy of an emulator
    (android_connector/emulator.py, Google's emulator and images, fetched once
    after Google's terms are accepted on the terminal; nothing survives a
    run): android_version picks the Android ("11" to "17"; omitted = newest
    one already here, else the newest) and sim_device "phone" (default) or
    "tablet". "hardware" drives the phone on USB, with whatever Android it
    runs. All of them are ignored by every other mode.

    app_build ("mobile use, ios" and "mobile use, android", simulation only):
    file name of a .app/.ipa (iOS) or an .apk (Android) in
    AutoCua_data/app_build/, installed on the simulator or emulator once it is
    ready. The run does not start unless the file exists and the app is
    installed.

    extra_tasks ("web use", and "mobile use, ios" or "mobile use, android"
    with device="simulation"): optional list of additional task strings
    (main.py's task_2, task_3, ...). Any non-empty entries make ALL tasks —
    including `task` — run in parallel: on the web each as its own agent
    sharing one Chrome, pinned to its own single tab; on iOS each on its own
    simulator; on Android each on its own copy of the emulator.

    For "mobile use, ios" and "mobile use, android" the device connection is
    opened first and always closed when the agent terminates (success, error,
    or Ctrl+C), and so are a simulator or an emulator the run started.
    """
    if ui:
        from AutoCua.ui.service import main as launch_ui
        launch_ui()
        return {"status": "success", "message": "desktop app closed"}

    kind, mobile_os = _parse_mode(mode, os)

    build = None
    if (kind, mobile_os) == ("mobile", "ios"):
        # setup_mac.sh --ios clones WebDriverAgent for a checkout; a pip install runs
        # no shell script, so fetch it the first time iOS is used. ABOVE the
        # extra_tasks branch below, which returns without ever reaching the
        # single-task path — simulation and hardware, one task or ten, all
        # arrive here first. The connectors ask again at their own choke
        # points; this call is the one that reports it before N simulators
        # spend a minute booting for a run that cannot work.
        from AutoCua.utils.wda import ensure_wda
        ensure_wda()
        if app_build:
            # No build, no run — checked before minutes of simulator booting.
            if _device_target(device) == "hardware":
                raise ValueError('app_build installs on the simulator only — use '
                                 'device="simulation" or set app_build=None')
            build = _unpack_app_build(app_build)
    elif (kind, mobile_os) == ("mobile", "android") and app_build:
        # The same before an emulator boots.
        if _device_target(device) == "hardware":
            raise ValueError('app_build installs on the emulator only — use '
                             'device="simulation" or set app_build=None')
        build = _android_app_build(app_build)

    extra = [str(t).strip() for t in (extra_tasks or []) if t and str(t).strip()]
    if extra:
        if kind == "web":
            return run_parallel_web_agents(
                [task] + extra,
                provider=provider,
                model=model,
                save_conversation=save_conversation,
                speed=agent_kwargs.get("speed"),
                headless=bool(agent_kwargs.get("headless")),
                browser_port=agent_kwargs.get("browser_port"),
                browser_profile=agent_kwargs.get("browser_profile"),
            )
        if (kind, mobile_os) == ("mobile", "ios"):
            if _device_target(device) == "hardware":
                raise ValueError(
                    'extra tasks (task_2, task_3, ...) need one device per task — '
                    'you only have one paired device. Use device="simulation" (the '
                    'default), which gives each task its own simulator.'
                )
            return run_parallel_sim_agents(
                [task] + extra,
                provider=provider,
                model=model,
                save_conversation=save_conversation,
                speed=agent_kwargs.get("speed"),
                ios_version=ios_version,
                sim_device=sim_device,
                build=build,
            )
        if (kind, mobile_os) == ("mobile", "android"):
            if _device_target(device) == "hardware":
                raise ValueError(
                    'extra tasks (task_2, task_3, ...) need one device per task — '
                    'a run drives one phone. Use device="simulation" (the '
                    'default), which gives each task its own emulator.'
                )
            return run_parallel_emulator_agents(
                [task] + extra,
                provider=provider,
                model=model,
                save_conversation=save_conversation,
                speed=agent_kwargs.get("speed"),
                android_version=android_version,
                sim_device=sim_device,
                build=build,
            )
        raise ValueError(
            f'extra tasks (task_2, task_3, ...) are only supported in "web use", '
            f'"mobile use, ios" and "mobile use, android" (device="simulation") — got "{mode}"'
        )

    AgentService = resolve_agent_service(mode, os)

    kwargs = dict(provider=provider, model=model,
                  save_conversation=save_conversation,
                  external_terminal=external_terminal, **agent_kwargs)

    phone = None
    sim_window = None
    emulator = pinned = preview = None
    if (kind, mobile_os) == ("mobile", "ios"):
        target = _device_target(device)
        if target == "hardware":
            if sim_device:
                print('📱 sim_device is ignored with device="hardware" — the paired '
                      'device is whatever it is (iPhone or iPad).')
            if ios_version:
                print('📱 ios_version is ignored with device="hardware" — '
                      'the phone runs whatever iOS it has')
            phone = _connect_iphone()
        else:
            # The simulator runs headless and shows in our own window instead
            # of Simulator.app. It opens right away ("Preparing simulation")
            # and gets the phone once it is ready, in the try below.
            from AutoCua.ios_connector.sim_view import open_view, close_view
            sim_window = open_view()
            try:
                phone = _connect_simulator(ios_version, sim_device=sim_device, build=build)
            except BaseException:
                close_view(sim_window)
                raise
    elif (kind, mobile_os) == ("mobile", "android"):
        # The phone helper is up before the agent is built, and stopped in
        # the finally below like the iOS connection; so is the emulator.
        if _device_target(device) == "hardware":
            if sim_device:
                print('📱 sim_device is ignored with device="hardware" — the phone '
                      'is whatever it is.')
            if android_version:
                print('📱 android_version is ignored with device="hardware" — '
                      'the phone runs whatever Android it has')
            phone = _connect_android()
        else:
            # The emulator draws no window of its own: the iOS Simulator's
            # window shows its screen, from the agent's preview stream, once
            # the agent is built below.
            from AutoCua.ios_connector.sim_view import open_view, close_view
            from AutoCua.android_connector.stream import _free_ports
            sim_window = open_view(title="Android Emulator")
            try:
                emulator = _connect_emulator(android_version, sim_device=sim_device, build=build)
                pinned = _pin_android_serial(emulator.serial)
                try:
                    phone = _connect_android(emulator.serial)
                except BaseException:
                    _pin_android_serial(pinned)
                    emulator.deactivate()
                    raise
            except BaseException:
                close_view(sim_window)
                raise
            preview = _free_ports(1)[0]
            os.environ[PREVIEW_ENV] = str(preview)
    agent_glow = None
    try:
        if sim_window is not None and emulator is None:
            from AutoCua.ios_connector.sim_view import show_phones
            show_phones(sim_window, [{"name": phone.status().get("name") or "Simulator",
                                      "port": phone.mjpeg_port}])
        # Terminal mode's half of the desktop cue the UI sets in
        # ui/service.py — the lavender cursor and edge glow last exactly
        # as long as the run, including Ctrl+C and a failed construction.
        # Computer use only: it is the one mode that drives this desktop.
        # The import is guarded because it is the one part that can raise:
        # start() swallows its own failures, but an ImportError here would
        # abort the run, and a cosmetic cue must never cost the user a task.
        if kind == "computer":
            try:
                from AutoCua.utils import agent_glow
            except Exception:
                agent_glow = None
        if agent_glow is not None:
            agent_glow.start()
        agent = AgentService(**_supported_kwargs(AgentService, kwargs))
        if sim_window is not None and emulator is not None:
            # The agent's scanner is up, and with it the preview stream.
            from AutoCua.ios_connector.sim_view import show_phones
            show_phones(sim_window, [{"name": f"Android {emulator.version} {emulator.kind}", "port": preview}])
        return agent.process_request(task)
    finally:
        if agent_glow is not None:
            try:
                agent_glow.stop()
            except Exception:
                pass
        if sim_window is not None:
            from AutoCua.ios_connector.sim_view import close_view
            close_view(sim_window)
        if phone is not None:
            phone.deactivate()
            print(f"📱 {'Android' if mobile_os == 'android' else 'iOS'} connection closed")
        if emulator is not None:
            os.environ.pop(PREVIEW_ENV, None)
            _pin_android_serial(pinned)
            emulator.deactivate()
            print("📱 Emulator closed")


def run_parallel_sim_agents(tasks, provider, model, save_conversation=False,
                            speed=None, ios_version=None, connect_deadline=900.0,
                            sim_device=None, build=None):
    """Run N iOS tasks in parallel — one SIMULATOR per task, one agent each.

    build (from _unpack_app_build): installed on each simulator the moment
    THAT simulator has booted, alongside its WDA start — never one after another.

    The simulator counterpart of run_parallel_web_agents. Where web shares one
    Chrome and gives each agent a tab, a phone screen can't be shared: every
    task gets its own simulator, its own WebDriverAgent (ports 8100, 8101, ...)
    and its own child process in ./parallel/task_N/ (so its conversation/,
    raw_reasoning/ and debug/ never touch another task's). The parent boots the
    simulators and starts every WDA before any agent runs, exactly like the
    web path pre-launches Chrome, and always shuts them down afterwards.

    Returns {"status", "message", "results": [{"task", "status", "message"}, ...]}.
    """
    from AutoCua.ios_connector.session import WDA_PORT
    from AutoCua.ios_connector.sim_session import SimulatorSession

    n = len(tasks)
    print(f"Running {n} iOS tasks in parallel — one simulator each")

    sessions = []  # (index, SimulatorSession, sim_info)
    boot_threads = []
    sim_window = None
    try:
        # Every simulator in one window, side by side, instead of
        # Simulator.app's windows stacked on top of each other. It opens now
        # ("Preparing simulation") and gets the phones once they are ready.
        from AutoCua.ios_connector.sim_view import open_view, show_phones
        sim_window = open_view()

        # 1. Claim a distinct simulator + port per task. Resolving is quick
        #    (a simctl listing), so it runs in order and each pick excludes the
        #    udids already taken - that is what keeps the claims distinct.
        from AutoCua.ios_connector.sim_session import resolve_simulator
        claimed = []
        for i in range(1, n + 1):
            port = WDA_PORT + i - 1
            sim, err = resolve_simulator(ios_version, exclude=tuple(claimed), device=sim_device)
            if err:
                detail = " — ".join(p for p in (err.get("error"), err.get("hint")) if p)
                raise RuntimeError(f"📱 task {i}: {detail}")
            claimed.append(sim["udid"])
            # Track it BEFORE activating: activate() boots a simulator and can
            # still fail afterwards, and only sessions in this list get shut
            # down by the finally below.
            sessions.append((i, SimulatorSession(port=port, slot=i), sim))
            print(f"[task {i}] {sim['name']} (iOS {sim['version']}) on port {port}")

        # 1b. Boot them ALL AT ONCE. activate() blocks until its simulator has
        #     finished booting (tens of seconds each, minutes on a cold
        #     runtime), so doing it in sequence made a 3-task run wait for
        #     three boots back to back. Each session has its own udid, port
        #     and derived-data folder, so the boots and xcodebuilds don't
        #     touch each other; the pinned-udid path skips re-resolution.
        print(f"📱 Booting {n} simulators in parallel...")
        outcomes = {}
        installs = {}   # index -> wait() of that simulator's app install

        def _activate(index, session, sim):
            try:
                outcomes[index] = session.activate(udid=sim["udid"])
                # This simulator is booted and its WDA is starting: install the
                # app right now, without waiting for the other simulators.
                if build and outcomes[index].get("ok"):
                    installs[index] = _install_in_background(sim["udid"], build)
            except BaseException as e:            # never lose a thread's failure
                outcomes[index] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

        boot_threads[:] = [threading.Thread(target=_activate, args=(i, session, sim), daemon=True)
                           for i, session, sim in sessions]
        for t in boot_threads:
            t.start()
        for t in boot_threads:
            t.join()   # Ctrl+C lands here; the finally cancels the boots
        for i, _session, _sim in sessions:
            res = outcomes.get(i) or {"ok": False, "error": "activate() returned nothing"}
            if not res.get("ok"):
                detail = " — ".join(p for p in (res.get("error"), res.get("hint")) if p) or res.get("code")
                raise RuntimeError(f"📱 task {i}: {detail}")

        # 2. Wait for all of them to answer (first run builds WDA — minutes).
        print("📱 Starting WebDriverAgent on each simulator...")
        pending = [(i, s) for i, s, _r in sessions]
        end = time.time() + connect_deadline
        while pending and time.time() < end:
            time.sleep(2.0)
            still = []
            for i, session in pending:
                try:
                    st = session.status()
                except Exception:
                    still.append((i, session))
                    continue
                if st.get("state") == "connected":
                    print(f"[task {i}] simulator ready")
                elif st.get("state") in ("error", "disconnected"):
                    detail = " — ".join(p for p in (st.get("error"), st.get("hint")) if p)
                    raise RuntimeError(f"📱 task {i} failed to start: {detail}")
                else:
                    still.append((i, session))
            pending = still
        if pending:
            raise RuntimeError(
                f"📱 {len(pending)} simulator(s) not ready after {int(connect_deadline)}s"
            )

        # The installs have been running since each boot; no agent starts
        # until every simulator has the app.
        for wait in installs.values():
            wait()

        show_phones(sim_window, [{"name": sim["name"], "port": session.mjpeg_port, "task": i}
                                 for i, session, sim in sessions])

        # 3. One child process per task, each pinned to its own simulator.
        run_root = Path.cwd() / "parallel"
        shutil.rmtree(run_root, ignore_errors=True)

        base_env = os.environ.copy()
        base_env["PYTHONUNBUFFERED"] = "1"
        base_env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + base_env.get("PYTHONPATH", "")

        procs = []  # (index, task_text, Popen, result_file)
        for (i, session, sim), task_text in zip(sessions, tasks):
            task_dir = run_root / f"task_{i}"
            task_dir.mkdir(parents=True, exist_ok=True)
            result_file = task_dir / "result.json"
            cmd = [sys.executable, "-m", "AutoCua.ios.agent",
                   "--task", str(task_text),
                   "--provider", provider,
                   "--model", model,
                   "--session-id", f"task_{i}",
                   "--wda-port", str(session.port),
                   "--sim-udid", str(sim["udid"]),
                   "--result", str(result_file)]
            if speed:
                cmd += ["--speed", str(speed)]
            if save_conversation:
                cmd += ["--save-conversation"]
            # The port MUST be in the child's environment before the process
            # starts: `python -m AutoCua.ios.agent` imports the package (and
            # ios/tree/element.py, which resolves the WDA URL at import) before
            # __main__ runs, so a value set inside main() would arrive too late
            # and the scanner would read task 1's screen. One env per child.
            env = dict(base_env)
            env["AutoCua_WDA_PORT"] = str(session.port)
            env["AutoCua_IOS_SESSION"] = f"task_{i}"
            # Deliberately NO start_new_session: children stay in this
            # terminal's process group, so one Ctrl+C interrupts every agent.
            proc = subprocess.Popen(cmd, cwd=str(task_dir), env=env, text=True,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            procs.append((i, str(task_text), proc, result_file))
            head = " ".join(str(task_text).split())[:70]
            print(f"[task {i}] started: {head}")

        def pump(index, stream):
            for line in stream:
                print(f"[task {index}] {line}", end="", flush=True)

        pumps = []
        for i, _task_text, proc, _rf in procs:
            t = threading.Thread(target=pump, args=(i, proc.stdout), daemon=True)
            t.start()
            pumps.append(t)

        try:
            for _i, _task_text, proc, _rf in procs:
                proc.wait()
        except KeyboardInterrupt:
            # The terminal already delivered SIGINT to the whole group; give
            # each child a grace period to write its result file, then escalate.
            print("\nStopping all tasks...")
            for _i, _task_text, proc, _rf in procs:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.terminate()
                    try:
                        proc.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()

        for t in pumps:
            t.join(timeout=2)

        results = []
        ok = 0
        for i, task_text, proc, result_file in procs:
            try:
                result = json.loads(result_file.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                result = {"status": "error",
                          "message": f"child exited with code {proc.returncode} "
                                     f"without writing a result"}
            if result.get("status") == "success":
                ok += 1
            results.append({"task": task_text, **result})

        print("=" * 60)
        print(f"PARALLEL RUN SUMMARY — {ok}/{n} succeeded")
        for i, r in enumerate(results, start=1):
            message = " ".join(str(r.get("message", "")).split())
            print(f"[task {i}] {r.get('status', '?'):<10} {message[:80]}")
        print(f"Artifacts: ./parallel/task_N/  (result.json, conversation/, "
              f"debug/, raw_reasoning/)")
        print("=" * 60)

        return {
            "status": "success" if ok == n else "error",
            "message": f"{ok}/{n} tasks succeeded",
            "results": results,
        }
    finally:
        # Every simulator this run booted gets shut down — success, error or
        # Ctrl+C — the same guarantee the single-task path gives. deactivate()
        # also CANCELS a boot still running in its thread (activate re-checks
        # ownership and starts no runner); wait for those threads, then
        # deactivate once more so a runner spawned in the last instant before
        # the cancel is reaped too.
        if sim_window is not None:
            from AutoCua.ios_connector.sim_view import close_view
            close_view(sim_window)

        def _close_all():
            for i, session, _sim in sessions:
                try:
                    session.deactivate()
                except Exception:
                    pass
        _close_all()
        for t in boot_threads:
            if t.is_alive():
                t.join(timeout=15)
        if any(t.is_alive() for t in boot_threads) or boot_threads:
            _close_all()
        if sessions:
            print("📱 simulators closed")


def run_parallel_emulator_agents(tasks, provider, model, save_conversation=False,
                                 speed=None, android_version=None, sim_device=None, build=None):
    """Run N Android tasks in parallel — one EMULATOR per task, one agent each.

    The emulator counterpart of run_parallel_sim_agents. Every task gets its
    own fresh copy of the emulator (android_connector/emulator.py: each starts
    from the saved first boot and keeps nothing), all booted at once, and its
    own child process in ./parallel/task_N/ (so its
    conversation/, raw_reasoning/ and debug/ never touch another task's),
    pinned to its copy by AutoCua_ANDROID_SERIAL. build (from
    _android_app_build) is installed on each copy the moment it has started.
    Every copy is stopped afterwards.

    Returns {"status", "message", "results": [{"task", "status", "message"}, ...]}.
    """
    from AutoCua.android_connector.emulator import EmulatorSession, prepare
    # Importing the package builds the Rust extension ONCE here in the parent,
    # before N children import it at the same time.
    import AutoCua.android.agent  # noqa: F401

    n = len(tasks)
    print(f"Running {n} Android tasks in parallel — one emulator each")

    # Everything the copies need, once, here in the main thread: the terms
    # question (asked once, not per task), the downloads and the first boot,
    # where Ctrl+C lands directly. The copies then only boot.
    prepare(android_version, sim_device)

    sessions = [(i, EmulatorSession()) for i in range(1, n + 1)]
    sim_window = None
    try:
        # Every copy in one window, side by side, as the iOS simulators: the
        # emulators draw none of their own. It opens now ("Preparing
        # simulation") and gets the screens once the agents stream them.
        from AutoCua.ios_connector.sim_view import open_view, show_phones
        from AutoCua.android_connector.stream import _free_ports
        sim_window = open_view(title="Android Emulator")

        # 1. Boot them ALL AT ONCE; each installs the app as soon as it is up.
        outcomes = {}

        def _boot(index, session):
            try:
                outcomes[index] = session.activate(android_version, device=sim_device)
                if build and outcomes[index].get("ok"):
                    _install_apk(session.serial, build)
            except BaseException as e:            # never lose a thread's failure
                outcomes[index] = {"ok": False, "error": f"{type(e).__name__}: {e}"}

        threads = [threading.Thread(target=_boot, args=(i, session), daemon=True)
                   for i, session in sessions]
        for t in threads:
            t.start()
        for t in threads:
            # Ctrl+C lands here, and the finally stops the emulators. In short
            # joins: on Windows before Python 3.14 a plain join() never sees it.
            while t.is_alive():
                t.join(0.5)
        for i, _session in sessions:
            res = outcomes.get(i) or {"ok": False, "error": "activate() returned nothing"}
            if not res.get("ok"):
                raise RuntimeError(f"📱 task {i}: {res.get('error')}")
            print(f"[task {i}] Android {res['version']} {res['device']} on {res['serial']}")

        # 2. One child process per task, each pinned to its own emulator.
        run_root = Path.cwd() / "parallel"
        shutil.rmtree(run_root, ignore_errors=True)

        base_env = os.environ.copy()
        base_env["PYTHONUNBUFFERED"] = "1"
        base_env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + base_env.get("PYTHONPATH", "")
        # The agent's lines carry emoji: UTF-8 both ways, never Windows' cp1252,
        # which can't print them in the child or read them here.
        base_env["PYTHONIOENCODING"] = "utf-8"

        previews = _free_ports(n)   # one preview stream per copy, for the window
        procs = []  # (index, task_text, Popen, result_file)
        for (i, session), task_text, preview in zip(sessions, tasks, previews):
            task_dir = run_root / f"task_{i}"
            task_dir.mkdir(parents=True, exist_ok=True)
            result_file = task_dir / "result.json"
            cmd = [sys.executable, "-m", "AutoCua.android.agent",
                   "--task", str(task_text),
                   "--provider", provider,
                   "--model", model,
                   "--session-id", f"task_{i}",
                   "--serial", session.serial,
                   "--result", str(result_file)]
            if speed:
                cmd += ["--speed", str(speed)]
            if save_conversation:
                cmd += ["--save-conversation"]
            # The serial pins the child's phone helper to its own emulator;
            # the session scopes its scratchpad to scratchpad/task_N/; the
            # preview port is where its agent streams the screen for the window.
            env = dict(base_env)
            env["AutoCua_ANDROID_SERIAL"] = session.serial
            env["AutoCua_ANDROID_SESSION"] = f"task_{i}"
            env[PREVIEW_ENV] = str(preview)
            # Deliberately NO start_new_session: children stay in this
            # terminal's process group, so one Ctrl+C interrupts every agent.
            proc = subprocess.Popen(cmd, cwd=str(task_dir), env=env, encoding="utf-8", errors="replace",
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            procs.append((i, str(task_text), proc, result_file))
            head = " ".join(str(task_text).split())[:70]
            print(f"[task {i}] started: {head}")

        # The window connects to each stream as its agent comes up.
        show_phones(sim_window, [{"name": f"Android {session.version} {session.kind}", "port": preview, "task": i}
                                 for (i, session), preview in zip(sessions, previews)])

        def pump(index, stream):
            for line in stream:
                print(f"[task {index}] {line}", end="", flush=True)

        pumps = []
        for i, _task_text, proc, _rf in procs:
            t = threading.Thread(target=pump, args=(i, proc.stdout), daemon=True)
            t.start()
            pumps.append(t)

        try:
            for _i, _task_text, proc, _rf in procs:
                proc.wait()
        except KeyboardInterrupt:
            # The terminal already delivered SIGINT to the whole group; give
            # each child a grace period to write its result file, then escalate.
            print("\nStopping all tasks...")
            for _i, _task_text, proc, _rf in procs:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.terminate()
                    try:
                        proc.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()

        for t in pumps:
            t.join(timeout=2)

        results = []
        ok = 0
        for i, task_text, proc, result_file in procs:
            try:
                result = json.loads(result_file.read_text(encoding="utf-8"))
            except (OSError, ValueError):
                result = {"status": "error",
                          "message": f"child exited with code {proc.returncode} "
                                     f"without writing a result"}
            if result.get("status") == "success":
                ok += 1
            results.append({"task": task_text, **result})

        print("=" * 60)
        print(f"PARALLEL RUN SUMMARY — {ok}/{n} succeeded")
        for i, r in enumerate(results, start=1):
            message = " ".join(str(r.get("message", "")).split())
            print(f"[task {i}] {r.get('status', '?'):<10} {message[:80]}")
        print(f"Artifacts: ./parallel/task_N/  (result.json, conversation/, "
              f"debug/, raw_reasoning/)")
        print("=" * 60)

        return {
            "status": "success" if ok == n else "error",
            "message": f"{ok}/{n} tasks succeeded",
            "results": results,
        }
    finally:
        # Every copy this run booted is stopped — success, error or Ctrl+C.
        # deactivate() also stops a copy whose boot is still going in its
        # thread, and a second Ctrl+C here still reaches every copy.
        if sim_window is not None:
            from AutoCua.ios_connector.sim_view import close_view
            close_view(sim_window)
        for _i, session in sessions:
            try:
                session.deactivate()
            except BaseException:
                pass
        print("📱 emulators closed")


def run_parallel_web_agents(tasks, provider, model, save_conversation=False,
                            speed=None, headless=False, browser_port=None,
                            browser_profile=None):
    """Run N web tasks in parallel — one child process per task, one shared Chrome.

    Each child gets its own working directory ./parallel/task_N/ (so its
    conversation/, raw_reasoning/ and debug/ never touch another task's), its
    own scratchpad session, and ONE dedicated browser tab. Live output is
    prefixed [task N]; a summary prints when every task has finished.

    Returns {"status", "message", "results": [{"task", "status", "message"}, ...]}.
    """
    # Importing the package builds the Rust extension ONCE here in the parent,
    # so N children never race the compiler — they find a fresh .so.
    from AutoCua.web.agent import launch_chrome, Bridge, CHROME_PORT
    from AutoCua import browser_profile_dir
    from AutoCua.utils.chrome import ensure_chrome

    port = int(browser_port or CHROME_PORT)
    n = len(tasks)
    # Chrome itself is installed first when it is missing.
    ensure_chrome()
    bridge = None
    if headless:
        # Headless is the one case on a debugging port: one browser of the agent's own
        # for everyone, launched (or attached) before any child starts, so every child's
        # constructor takes the clean attach path.
        print(f"Running {n} web tasks in parallel — one headless browser, port {port}")
        launch_chrome(port, True, str(browser_profile_dir(browser_profile)))
    else:
        # Headful is extension mode, a parallel run too (the owner, 2026-10-03): the
        # person's own Chrome (or the profile named), taken once here, with the cover
        # page in front (the video, "Multiple agents running") for the whole run. Each
        # child then dials its own line to the extension (bridge.rs config.json lists
        # them all) and works in a background tab of its own; none switches the front.
        print(f"Running {n} web tasks in parallel — your Chrome, through the AutoCua extension")
        print(f"Note: {n} agents share one browser window, each working in a "
              f"background tab of its own. Click a tab to watch that agent.")
        bridge = Bridge()
        bridge.take_chrome(str(browser_profile_dir(browser_profile)) if browser_profile else None)
        try:
            bridge.request({"type": "cover", "count": n})
        except Exception as e:  # noqa: BLE001 - a run never fails over its cover page
            print(f"(no cover page: {e})")

    run_root = Path.cwd() / "parallel"
    shutil.rmtree(run_root, ignore_errors=True)

    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    # The agent's lines carry emoji: UTF-8 both ways, never Windows' cp1252,
    # which can't print them in the child or read them here.
    env["PYTHONIOENCODING"] = "utf-8"

    procs = []  # (index, task_text, Popen, result_file)
    for i, task_text in enumerate(tasks, start=1):
        task_dir = run_root / f"task_{i}"
        task_dir.mkdir(parents=True, exist_ok=True)
        result_file = task_dir / "result.json"
        cmd = [sys.executable, "-m", "AutoCua.web.agent",
               "--task", str(task_text),
               "--provider", provider,
               "--model", model,
               "--session-id", f"task_{i}",
               "--result", str(result_file)]
        if speed:
            cmd += ["--speed", str(speed)]
        if headless:
            cmd += ["--headless", "--browser-port", str(port)]
        if browser_profile:
            cmd += ["--browser-profile", str(browser_profile)]
        if save_conversation:
            cmd += ["--save-conversation"]
        # Deliberately NO start_new_session: children stay in this terminal's
        # process group, so one Ctrl+C interrupts every agent at once.
        proc = subprocess.Popen(cmd, cwd=str(task_dir), env=env, encoding="utf-8", errors="replace",
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        procs.append((i, str(task_text), proc, result_file))
        head = " ".join(str(task_text).split())[:70]
        print(f"[task {i}] started: {head}")

    def pump(index, stream):
        for line in stream:
            print(f"[task {index}] {line}", end="", flush=True)

    pumps = []
    for i, _task_text, proc, _rf in procs:
        t = threading.Thread(target=pump, args=(i, proc.stdout), daemon=True)
        t.start()
        pumps.append(t)

    try:
        for _i, _task_text, proc, _rf in procs:
            proc.wait()
    except KeyboardInterrupt:
        # The terminal already delivered SIGINT to the whole group; give each
        # child a grace period to write its result file, then escalate.
        print("\nStopping all tasks...")
        for _i, _task_text, proc, _rf in procs:
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.terminate()
                try:
                    proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
    finally:
        # The run is over: the cover page goes, and this bridge with it.
        if bridge is not None:
            try:
                bridge.request({"type": "cover", "count": 0})
            except Exception:  # noqa: BLE001
                pass
            bridge = None

    for t in pumps:
        t.join(timeout=2)

    results = []
    ok = 0
    for i, task_text, proc, result_file in procs:
        try:
            result = json.loads(result_file.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            result = {"status": "error",
                      "message": f"child exited with code {proc.returncode} "
                                 f"without writing a result"}
        if result.get("status") == "success":
            ok += 1
        results.append({"task": task_text, **result})

    print("=" * 60)
    print(f"PARALLEL RUN SUMMARY — {ok}/{n} succeeded")
    for i, r in enumerate(results, start=1):
        message = " ".join(str(r.get("message", "")).split())
        print(f"[task {i}] {r.get('status', '?'):<10} {message[:80]}")
    print(f"Artifacts: ./parallel/task_N/  (result.json, conversation/, "
          f"debug/, raw_reasoning/)")
    print("=" * 60)

    return {
        "status": "success" if ok == n else "error",
        "message": f"{ok}/{n} tasks succeeded",
        "results": results,
    }
