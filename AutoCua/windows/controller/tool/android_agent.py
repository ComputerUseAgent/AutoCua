"""The Android agent as a sub-agent of the main agent, on the person's
Android phone plugged into this computer by USB (USB debugging on), or, when
the desktop app's Wireless / Wired toggle says Wireless, one Settings
connected that is on the same Wi-Fi (Wireless debugging on).

Two halves, as tool/ios_agent.py has:

AndroidLink: the request's phone check. The moment the main agent starts a
request, it looks for an Android phone on USB (the newest one Settings →
Connect Device remembered, else the one plugged in), or, with the app's
toggle on Wireless, a remembered one on the Wi-Fi only (the connector's
WIRELESS; a terminal run keeps the cable), and checks it in the background:
the phone answers over adb, its
helper starts and reads one screen, then stops again
(android_connector/session.py's confirm). Meanwhile <sub_agents> shows
`checking connection: android_agent`; once the check passes, android_agent
joins the `online:` line. No phone, or a check that fails, and it is simply
not listed. While it is online the phone is looked at again every WATCH_SEC,
so a pulled cable (or a phone gone from the Wi-Fi) takes it off the line.

The runs: `sub_agent {"agent_type": "android_agent"}` starts the Android agent
on a task and returns at once; the controller polls `outcome` until the run is
over. Each run is a subprocess (android_agent_child.py), because the Android
AgentService constructor deletes CWD-relative conversation/, debug/ and
raw_reasoning/ (the main agent's LIVE folders), so the child gets a working
directory of its own. The child starts the phone's helper itself: a phone has
one, and this process holds none while a run works.

adb is never fetched from here: a computer that has not set up Android yet
(adb on PATH, or fetched once by Settings or an Android run) just has no
android_agent, without a download at every request.
"""

import atexit
import ctypes
import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

from AutoCua import data_root, IS_COMPILED
from .browser_agent import _PROVIDER_ENV_KEY

_REPO_ROOT = Path(__file__).resolve().parents[4]
_ANDROID_SCRATCHPAD = _REPO_ROOT / "AutoCua" / "android" / "scratchpad"
_CHILD = Path(__file__).resolve().parent / "android_agent_child.py"

# How often an online phone is looked at again (a pulled cable).
WATCH_SEC = 5
# The child has its own 100-step cap; this bounds wall-clock.
TIMEOUT_SEC = int(os.environ.get("AutoCua_ANDROID_AGENT_TIMEOUT", "900"))
# A `done` value shorter than this is almost certainly an activity log, so it
# is topped up with what the agent recorded along the way.
_SHORT_REPORT_CHARS = 200

_TASK_TEMPLATE = """{task}

How to finish:
- Your final `done` call's value is the report the main agent receives, and it is all it receives. State what you did, the result with the exact values and names that matter, what the screen shows at the end, and anything you could not do.
"""


class AndroidAgentError(Exception):
    """android_agent could not be started."""


# Every phone link and every run of this process, so none outlives it.
_links = []
_runs = []


def stop_all() -> None:
    """Quitting mid-run must not leave the phone driven: the run acts on the
    phone and calls the model with nobody left to report to.

    Runs at exit, and from the app window's closing event too, because not
    every quit runs atexit: Cmd+Q on macOS leaves through NSApplication
    terminate:, which calls C exit()."""
    for run in list(_runs):
        try:
            stop(run)
        except Exception:
            pass
    for link in list(_links):
        try:
            link.release()
        except Exception:
            pass


atexit.register(stop_all)


def _log(msg: str) -> None:
    """What the phone check did and why, in android_agent/phone_link.log under
    the data folder (and on stdout). The main agent only ever sees "online" or
    "offline": without this, a failed check leaves no trace anywhere."""
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(f"[android_agent] {line}", flush=True)
    try:
        path = data_root() / "android_agent" / "phone_link.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except Exception:
        pass


def _adb():
    """adb when this computer already has it (on PATH, or fetched once by
    Settings or an Android run), else None: never a download from here."""
    from AutoCua.android_connector.adb import OWN_ADB
    return shutil.which("adb") or (str(OWN_ADB) if OWN_ADB.is_file() else None)


def _usb_phones(adb):
    """{serial: state} for the real phones adb sees on USB (no emulators, as
    ios_agent never drives a simulator)."""
    from AutoCua.android_connector.adb import _adb as run_adb, _wireless, devices
    run_adb(adb, "start-server")
    return {serial: state for serial, state in devices(adb)
            if not _wireless(serial) and not serial.startswith("emulator-")}


class AndroidLink:
    """One request's phone check: "checking", then "online" or "offline"."""

    def __init__(self):
        self._lock = threading.Lock()
        self._state = "offline"
        self.serial = None
        self.address = None     # adb's serial for it: the USB serial, or "ip:port" on Wi-Fi
        self.name = "Android phone"
        self.reason = "no Android phone is plugged in by USB with USB debugging on"
        self._released = False
        self._t0 = time.monotonic()
        # The main agent's provider, model, api_key and speed: an
        # android_agent child is started with them when the phone answers
        self._standby_for = None
        self._standby = None

    @classmethod
    def start(cls, standby: dict = None) -> "AndroidLink":
        """Check for the phone in the background. Returns at once. standby:
        {"provider", "model", "api_key", "speed"}, as start() will get them,
        to have the child ready when the phone answers."""
        link = cls()
        link._standby_for = standby
        if IS_COMPILED:
            link.reason = "android_agent is unavailable in the packaged app"
            return link
        try:
            adb = _adb()
        except Exception:
            adb = None
        if adb is None:
            link.reason = ("Android is not set up on this computer: connect the phone once in "
                           "Settings → Connect Device → Android")
            return link                          # no adb yet: offline at once, no download
        link._state = "checking"
        _links.append(link)
        threading.Thread(target=link._check, args=(adb,), daemon=True).start()
        return link

    def state(self) -> str:
        """checking, online or offline."""
        with self._lock:
            return self._state

    def release(self) -> None:
        """The request is over. Nothing is held: the check stopped its helper,
        and a run's helper goes with its child."""
        with self._lock:
            if self._released:
                return
            self._released = True
            self._state = "offline"
        if self.serial:
            _log(f"released after {self._elapsed()}")
        self._drop_standby()
        self._forget()

    def _elapsed(self) -> str:
        return f"{time.monotonic() - self._t0:.1f}s"

    # -- the check thread --
    def _check(self, adb):
        try:
            from AutoCua.android_connector.adb import HINTS
            from AutoCua.android_connector import session as connector
            from AutoCua.android_connector.session import android_session, paired_devices, wifi_saved
            link = "wireless" if connector.WIRELESS else "wired"
            if link == "wireless":
                # The app's toggle says Wireless: the Wi-Fi only, a phone
                # Settings connected that announces Wireless debugging. The
                # cable is left alone, so with none on the Wi-Fi the agent is
                # offline, cable or not.
                serial = wifi_saved(adb)
                if serial is None:
                    self._fail("no Android phone connected in Settings is on Wi-Fi (Wireless "
                               "debugging on, the same Wi-Fi as this computer)")
                    return
            else:
                # Wired, the default (and every terminal run): the cable only.
                plugged = _usb_phones(adb)
                ready = [s for s, state in plugged.items() if state == "device"]
                saved = [d["serial"] for d in paired_devices() if d.get("serial") in plugged]
                if saved:
                    # A phone Settings connected is plugged in: that one, never another.
                    serial = next((s for s in saved if s in ready), None)
                    if serial is None:
                        state = plugged[saved[0]]
                        self._fail(HINTS.get(state, f"the phone is in the {state!r} state"))
                        return
                elif len(ready) == 1:
                    serial = ready[0]
                elif ready:
                    self._fail("more than one Android phone is plugged in: connect the one to use in "
                               "Settings → Connect Device → Android")
                    return
                elif plugged:
                    state = next(iter(plugged.values()))
                    self._fail(HINTS.get(state, f"the phone is in the {state!r} state"))
                    return
                else:
                    self._fail("no Android phone is plugged in by USB with USB debugging on")
                    return
            self.serial = serial
            _log(f"checking {serial}{' on Wi-Fi' if link == 'wireless' else ''}")
            # A phone this process already reads (the app's own Mobile use →
            # Android pick) answers as it is: confirm() leaves it running,
            # unless it reads it the other way.
            res = android_session.confirm(serial, keep=False, link=link)
            if not res.get("ok"):
                self._fail(" - ".join(str(p) for p in (res.get("error"), res.get("hint")) if p)
                           or "the phone could not be checked")
                return
            self.name = res.get("model") or self.name
            # What the watch looks for in `adb devices`: "ip:port" on Wi-Fi.
            self.address = res.get("serial") or serial
            with self._lock:
                if self._released:
                    return
                self._state = "online"
            _log(f"online after {self._elapsed()}: {self.name} ({serial}"
                 f"{'' if self.address == serial else ', Wi-Fi ' + self.address})")
            self._start_standby(android_session)
            self._watch(adb)
        except Exception as e:
            self._fail(f"the phone check failed: {e}")

    def _watch(self, adb):
        """Keep the online line true: a pulled cable takes android_agent off it."""
        from AutoCua.android_connector.adb import _adb as run_adb
        while True:
            time.sleep(WATCH_SEC)
            with self._lock:
                if self._released or self._state != "online":
                    return
            try:
                rows = run_adb(adb, "devices").stdout.splitlines()[1:]
            except Exception:
                continue
            states = dict(line.split("\t", 1) for line in rows if "\t" in line)
            if states.get(self.address, "").strip() != "device":
                self._fail("the phone was unplugged" if self.address == self.serial
                           else "the phone left the Wi-Fi")
                return

    def _fail(self, reason: str):
        with self._lock:
            self._state = "offline"
            if not self._released:
                self.reason = reason or self.reason
        _log(f"offline after {self._elapsed()}: {reason}")
        self._drop_standby()
        self._forget()

    # -- the standby child --
    def _start_standby(self, android_session):
        """Start the request's android_agent child now, built (the phone
        connected, as its scanner does) and waiting for its task
        (android_agent_child.py --standby): when the main agent sends one,
        nothing loads first (a busy Mac took 43 s to). Not while this process
        holds the phone's one helper itself (the app's Mobile use → Android
        pick): start() lets that go only when a task really comes."""
        args = self._standby_for
        if not args or android_session.status().get("state") == "connected":
            return
        key = (args.get("provider"), args.get("model"), args.get("api_key"), args.get("speed"))
        try:
            child = _spawn(self.address or self.serial, *key)
        except Exception as e:
            _log(f"no android_agent started ahead: {e}")
            return
        child["key"] = key
        pid = child["proc"].pid
        with self._lock:
            if not self._released and self._state == "online":
                self._standby, child = child, None
        if child is not None:
            _end_standby(child)          # the request ended meanwhile
        else:
            _log(f"android_agent started ahead, waiting for its task (pid {pid})")

    def take_standby(self, provider, model, api_key, speed):
        """The standby child when it was built the way start() asks and still
        runs: it is that run's now. None otherwise, and start() starts one."""
        with self._lock:
            child, self._standby = self._standby, None
        if child is None:
            return None
        if child["key"] == (provider, model, api_key, speed) and child["proc"].poll() is None:
            return child
        _end_standby(child)
        return None

    def _drop_standby(self):
        with self._lock:
            child, self._standby = self._standby, None
        if child is not None:
            _end_standby(child)

    def _forget(self):
        try:
            _links.remove(self)
        except ValueError:
            pass


# ---------------------------------------------------------------- the runs --

def _milestone_notes(run: dict) -> str:
    """What the Android agent wrote to its scratchpad during this run: the
    salvage when the `done` value is thin or the run never reached `done`."""
    p = _ANDROID_SCRATCHPAD / "milestone" / "milestone.md"
    try:
        if p.exists() and p.stat().st_mtime >= run["started"]:
            return p.read_text(encoding="utf-8").strip()
    except OSError:
        pass
    return ""


def _tail(path: Path, lines: int = 30) -> str:
    try:
        return "\n".join(path.read_text(encoding="utf-8", errors="replace").splitlines()[-lines:])
    except OSError:
        return ""


def start(task: str, provider: str, model: str, api_key: str = None, speed: str = None,
          link: AndroidLink = None) -> dict:
    """Start one android_agent on `task` on the request's checked phone and
    return its run record, the dict `outcome` and `stop` take. Raises
    AndroidAgentError when it cannot start."""
    if IS_COMPILED:
        raise AndroidAgentError("android_agent is unavailable in the packaged app")
    if link is None or link.state() != "online":
        raise AndroidAgentError("android_agent is currently offline")
    # One phone, one agent: a second one would tap over the first.
    if any(r["subprocess"].poll() is None for r in _runs):
        raise AndroidAgentError("an android_agent is still at work and only one runs at a time: "
                                "hold for it with agent_wait, then start the next")

    # The phone has one helper, and the child starts its own: a helper this
    # process still holds (the app's Mobile use → Android pick) goes first.
    try:
        from AutoCua.android_connector.session import android_session
        android_session.deactivate()
    except Exception:
        pass

    text = _TASK_TEMPLATE.format(task=task)
    # The child the link started when the phone answered, built and
    # waiting: it gets the task as one line and begins at once.
    child = link.take_standby(provider, model, api_key, speed)
    if child is not None:
        try:
            child["proc"].stdin.write((json.dumps({"task": text}) + "\n").encode("utf-8"))
            child["proc"].stdin.close()
        except (OSError, ValueError):
            _end_standby(child)
            child = None
    if child is None:
        try:
            child = _spawn(link.address or link.serial, provider, model, api_key, speed, task=text)
        except Exception as e:
            raise AndroidAgentError(f"android_agent could not be started: {e}")

    run_dir = child["run_dir"]
    run = {
        "session_id": child["session_id"],
        "subprocess": child["proc"],
        "result_file": run_dir / "result.json",
        "log_file": run_dir / "agent.log",
        "started": time.time(),
        "deadline": time.monotonic() + TIMEOUT_SEC,
        "stopped": False,
    }
    _runs.append(run)
    return run


def _spawn(serial: str, provider: str, model: str, api_key: str = None, speed: str = None,
           task: str = None) -> dict:
    """One child in a new run folder: on `task`, or, with no task, a standby
    that gets ready and reads its task from stdin. Returns {"session_id",
    "run_dir", "proc"}."""
    sid = f"android_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    run_dir = data_root() / "android_agent" / sid
    run_dir.mkdir(parents=True, exist_ok=True)

    cmd = [
        sys.executable, str(_CHILD),
        "--provider", provider,
        "--model", model,
        "--result", str(run_dir / "result.json"),
    ]
    cmd += ["--task", task] if task is not None else ["--standby"]
    if speed:
        cmd += ["--speed", str(speed)]
    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    # The child's cwd is outside the repo, so make the package importable.
    env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    # The phone this request checked, on the connection the check used and
    # the watch follows: its USB serial, or "ip:port" on Wi-Fi.
    env["AutoCua_ANDROID_SERIAL"] = serial
    # This process: the child ends itself (and the phone helper) if it dies
    # without stopping the run (android_agent_child.py _watch_parent).
    env["AutoCua_ANDROID_PARENT"] = str(os.getpid())
    env.pop("AutoCua_ANDROID_SESSION", None)
    if api_key and provider in _PROVIDER_ENV_KEY:
        env[_PROVIDER_ENV_KEY[provider]] = api_key

    # Hidden on Windows when the app has no console of its own, so no console
    # window pops up (as browser_agent.py starts its child)
    creationflags = 0
    if sys.platform == "win32" and not ctypes.windll.kernel32.GetConsoleWindow():
        creationflags = subprocess.CREATE_NO_WINDOW
    # stdout/stderr go to a file: the Android agent prints a lot, and a
    # PIPE nobody drains would deadlock it. Its own process group on
    # POSIX, so stop() ends the phone helper's adb client with it.
    with open(run_dir / "agent.log", "ab") as log:
        proc = subprocess.Popen(cmd, cwd=str(run_dir), env=env,
                                stdin=subprocess.DEVNULL if task is not None else subprocess.PIPE,
                                stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=(sys.platform != "win32"),
                                creationflags=creationflags)
    return {"session_id": sid, "run_dir": run_dir, "proc": proc}


def _end_standby(child: dict) -> None:
    """A standby that got no task: it goes with its phone helper (the whole
    tree, as stop() ends a run), and its run folder with it."""
    proc = child["proc"]
    try:
        if proc.poll() is None:
            _kill_tree(proc, signal.SIGTERM)
            proc.wait(timeout=5)
    except Exception:
        try:
            proc.kill()
        except Exception:
            pass
    shutil.rmtree(child["run_dir"], ignore_errors=True)


def _kill_tree(proc, sig) -> None:
    """Signal the child AND what it started: the phone helper runs as its
    `adb shell` client, and a killed child runs no atexit, so the client
    would stay and keep the helper (and Android's one UiAutomation) on the
    phone. taskkill /T on Windows; the child's own process group on POSIX
    (start() gives it one), never this process's own group."""
    if sys.platform == "win32":
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"],
                       capture_output=True, timeout=10)
        return
    try:
        # start() made the child its group's leader: the group is its pid, and
        # it holds what the child left behind even once the child is gone.
        if proc.pid != os.getpgrp():
            os.killpg(proc.pid, sig)
        else:
            proc.send_signal(sig)
    except ProcessLookupError:
        pass                                    # nothing of it is left


def stop(run: dict) -> None:
    """End a run from outside (Stop, the end of the main agent's request, or
    the phone going away). The child's helper goes with it: the whole tree is
    ended, the helper's adb client too."""
    run["stopped"] = True
    proc = run.get("subprocess")
    if proc is not None and proc.poll() is None:
        try:
            _kill_tree(proc, signal.SIGTERM)
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                _kill_tree(proc, signal.SIGKILL if sys.platform != "win32" else signal.SIGTERM)
            except Exception:
                proc.kill()
            proc.wait()
        except Exception:
            pass


def _drop(run: dict) -> None:
    if run in _runs:
        _runs.remove(run)


def outcome(run: dict, phone_online: bool = True):
    """(summary, status) once the run is over, None while it is still working.

    status is `complete` when the Android agent finished its task, otherwise
    how it ended: `incomplete`, `error`, `timeout` or `stopped`. Call it until
    it answers. phone_online=False (the phone went away) ends a run at once.
    """
    proc = run["subprocess"]
    if proc.poll() is None:
        if not phone_online and not run["stopped"]:
            # Notes first: the run is over for good once it is stopped
            notes = _milestone_notes(run) or "nothing was recorded before the phone went away"
            stop(run)
            run["stopped"] = False   # the phone went away, nobody stopped it
            _drop(run)
            return ("The phone disconnected while android_agent was working.\n\n"
                    f"Recorded before it stopped:\n{notes}", "error")
        if time.monotonic() <= run["deadline"]:
            return None
        notes = _milestone_notes(run) or "nothing was recorded before the time limit"
        stop(run)
        run["stopped"] = False   # it ran out of time, nobody stopped it
        _drop(run)
        return (f"android_agent ran out of time ({TIMEOUT_SEC // 60} min) before it "
                f"finished.\n\nRecorded before it stopped:\n{notes}", "timeout")

    notes = _milestone_notes(run)
    _drop(run)
    # A child that ended without its own clean-up (a crash) can leave the phone
    # helper's adb client running: what is left of its tree goes too.
    if sys.platform != "win32":
        try:
            _kill_tree(proc, signal.SIGTERM)
        except Exception:
            pass
    if run.get("stopped"):
        return ("Stopped by user", "stopped")
    try:
        result = json.loads(run["result_file"].read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return (f"android_agent exited with code {proc.returncode} without a report.\n\n"
                f"{_tail(run['log_file'])}", "error")

    status = str(result.get("status") or "error")
    message = str(result.get("message") or "").strip() or "no message"
    if status == "success":
        if len(message) < _SHORT_REPORT_CHARS and notes:
            message += "\n\nRecorded during the run:\n" + notes
        return (message, "complete")
    if notes:
        message += "\n\nRecorded before it stopped:\n" + notes
    return (message, status)
