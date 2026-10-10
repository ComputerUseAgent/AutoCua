"""The iOS agent as a sub-agent of the main agent, on the person's REAL iPhone
or iPad, plugged into this Mac by cable. Never a simulator.

Two halves:

PhoneLink: the request's phone connection. The moment the main agent starts
a request, it looks for a paired phone plugged in by USB and connects it in
the background, with the same session a "mobile use, ios" hardware run uses
(ios_connector/session.py). Meanwhile <sub_agents> shows
`checking connection: ios_agent`. Once the phone answers, ios_agent joins the
`online:` line, and that is all: no window yet. No phone plugged in, or no
answer within CHECK_SEC, and ios_agent is simply not listed. The end of the
request lets the phone go.

The phone window (ios_connector/sim_view) opens only when a task is first
delegated to the phone: "iPhone connected" shimmering on the gradient, then
the live screen. It stays for the rest of the request.

The runs: `sub_agent {"agent_type": "ios_agent"}` starts the iOS agent on a
task and returns at once; the controller polls `outcome` until the run is
over. Each run is a subprocess (ios_agent_child.py), because the iOS
AgentService constructor deletes CWD-relative conversation/, debug/ and
raw_reasoning/ (the main agent's LIVE folders), so the child gets a working
directory of its own. The child only talks to the phone this process
connected: it never opens a session itself, since a second activate() would
kill this one's cable forward.

macOS only: pairing a phone needs Xcode.
"""

import atexit
import json
import os
import socket
import subprocess
import sys
import threading
import time
import urllib.request
import uuid
from pathlib import Path

from AutoCua import data_root, IS_COMPILED
from .browser_agent import _PROVIDER_ENV_KEY

_REPO_ROOT = Path(__file__).resolve().parents[4]
_IOS_SCRATCHPAD = _REPO_ROOT / "AutoCua" / "ios" / "scratchpad"
_CHILD = Path(__file__).resolve().parent / "ios_agent_child.py"

# A connect takes about 15 s, longer when iOS asks for the passcode to turn
# on Automation Mode: give it 30, then the phone counts as offline.
CHECK_SEC = 30
# How often a connected phone is looked at again (a pulled cable, New chat).
WATCH_SEC = 5
# How long the window's shimmering words stay before the phone replaces them.
SHIMMER_SEC = 4.0
# The child has its own 100-step cap; this bounds wall-clock.
TIMEOUT_SEC = int(os.environ.get("AutoCua_IOS_AGENT_TIMEOUT", "900"))
# A `done` value shorter than this is almost certainly an activity log, so it
# is topped up with what the agent recorded along the way.
_SHORT_REPORT_CHARS = 200

_TASK_TEMPLATE = """{task}

How to finish:
- Your final `done` call's value is the report the main agent receives, and it is all it receives. State what you did, the result with the exact values and names that matter, what the screen shows at the end, and anything you could not do.
"""


class IOSAgentError(Exception):
    """ios_agent could not be started."""


# Every phone link and every run of this process, so none outlives it.
_links = []
_runs = []


def stop_all() -> None:
    """Quitting mid-run must not leave the phone driven or held: the run acts
    on the phone and calls the model with nobody left to report to, and a
    held session keeps "Automation Running" on the phone.

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


def _reason(d) -> str:
    """The session's own words for why it could not connect (error + hint)."""
    if not isinstance(d, dict):
        return ""
    return " - ".join(str(p) for p in (d.get("error"), d.get("hint")) if p)


def _log(msg: str) -> None:
    """What the phone link did and why, in ios_agent/phone_link.log under the
    data folder (and on stdout). The main agent only ever sees "online" or
    "offline": without this, a failed connect leaves no trace anywhere."""
    line = f"{time.strftime('%H:%M:%S')} {msg}"
    print(f"[ios_agent] {line}", flush=True)
    try:
        path = data_root() / "ios_agent" / "phone_link.log"
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except Exception:
        pass


def _close_window(win) -> None:
    if win is None:
        return
    try:
        from AutoCua.ios_connector.sim_view import close_view
        close_view(win)
    except Exception:
        pass


def _stream_up(port: int, timeout: float) -> bool:
    """Wait until the phone's screen stream sends its first bytes, so the
    window shows the phone the moment it appears."""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=2) as s:
                s.sendall(b"GET / HTTP/1.0\r\n\r\n")
                s.settimeout(3)
                if s.recv(1024):
                    return True
        except OSError:
            pass
        time.sleep(0.5)
    return False


def _device_model() -> str:
    """"iPhone" or "iPad", from the phone itself. WDA's device info needs no
    session, so asking never disturbs one."""
    try:
        from AutoCua.ios_connector.session import WDA_PORT
        with urllib.request.urlopen(f"http://127.0.0.1:{WDA_PORT}/wda/device/info", timeout=3) as r:
            value = json.loads(r.read().decode("utf-8")).get("value") or {}
        return str(value.get("model") or "").strip()
    except Exception:
        return ""


class PhoneLink:
    """One request's phone connection: "checking", then "online" or "offline"."""

    def __init__(self):
        self._lock = threading.Lock()
        self._state = "offline"
        self.udid = None
        self.name = "iPhone"
        self.reason = "no iPhone or iPad is plugged into this Mac"
        self._deadline = 0.0
        self._released = False
        self._owns = False       # this link activated the session, so it lets it go
        self._busy = False       # the connect thread is inside activate(), which holds the session lock
        self._model = ""         # "iPhone" or "iPad", asked from the phone once it answers
        self._window = None      # the phone window (a sim_view process)
        self._window_started = False   # opened on the first delegation, once per request

    @classmethod
    def start(cls) -> "PhoneLink":
        """Look for the phone in the background. Returns at once."""
        link = cls()
        if IS_COMPILED:
            link.reason = "ios_agent is unavailable in the packaged app"
            return link
        try:
            from AutoCua.ios_connector.session import paired_devices, plugged_in_udids
            paired = paired_devices()            # newest first
            plugged = plugged_in_udids()         # None: usbmuxd did not answer, so just try
        except Exception:
            return link
        if plugged is not None:
            paired = [d for d in paired if d.get("udid") in plugged]
        if not paired:
            _log("offline at start: no paired phone is plugged in "
                 f"(paired: {[d.get('name') for d in paired_devices()]}, plugged in: {sorted(plugged or [])})")
            return link                          # nothing paired is plugged in: offline at once
        link.udid = paired[0]["udid"]
        link.name = paired[0].get("name") or "iPhone"
        link._state = "checking"
        link._deadline = time.monotonic() + CHECK_SEC
        link._t0 = time.monotonic()
        _links.append(link)
        _log(f"checking {link.name} ({link.udid}), up to {CHECK_SEC}s")
        threading.Thread(target=link._connect, daemon=True).start()
        return link

    def state(self) -> str:
        """checking, online or offline. A check past its deadline is offline,
        and so is a phone another AutoCua run took over (the watch loop
        cleans that up within WATCH_SEC)."""
        with self._lock:
            if self._state == "checking" and time.monotonic() > self._deadline:
                return "offline"
            state = self._state
        if state == "online":
            from AutoCua.ios_connector.session import wda_session
            if not wda_session.holds(self):
                return "offline"
        return state

    def release(self) -> None:
        """The request is over: close the window and let the phone go. Never
        waits on a connect that is still mounting: that thread sees the link
        released and lets the phone go itself."""
        with self._lock:
            if self._released:
                return
            self._released = True
            self._state = "offline"
            owns = self._owns and not self._busy
            if owns:
                self._owns = False
            win, self._window = self._window, None
        if self.udid:
            _log(f"released after {self._elapsed()}")
        _close_window(win)
        if owns:
            self._deactivate()
        self._forget()

    # -- the connect thread --
    def _connect(self):
        from AutoCua.ios_connector.session import wda_session
        try:
            # A terminal "mobile use" run (a phone or a simulator) may hold the
            # connection from another process; activate() would kill its
            # forward, so never take it.
            if wda_session.busy_elsewhere():
                self._fail("another AutoCua run is already using the phone connection")
                return
            with self._lock:
                if self._released:
                    return
                self._busy = True
            res = {}
            try:
                res = wda_session.activate(self.udid, owner=self)
            finally:
                with self._lock:
                    self._busy = False
                    self._owns = bool(isinstance(res, dict) and res.get("ok"))
            _log(f"activate after {self._elapsed()}: {res}")
            if not res.get("ok"):
                self._fail(_reason(res) or "the phone could not be connected")
                return
            last = None
            while True:
                with self._lock:
                    gone = self._released or time.monotonic() > self._deadline
                if gone:
                    self._fail(f"the phone did not answer within {CHECK_SEC} seconds")
                    return
                st = wda_session.status()
                if st.get("state") != last:
                    last = st.get("state")
                    _log(f"status after {self._elapsed()}: {st}")
                if st.get("state") == "connected":
                    break
                if st.get("state") in ("error", "disconnected"):
                    self._fail(_reason(st) or "the phone could not be connected")
                    return
                time.sleep(1)
            # What the phone is, for the window's label and frame: asked once
            # now, while nothing else is talking to it yet. The paired name is
            # whatever the person called the phone ("Ashish"): add what it is.
            self._model = _device_model()
            if self._model and self._model.lower() not in self.name.lower():
                self.name = f"{self.name} · {self._model}"
            with self._lock:
                late = self._released or time.monotonic() > self._deadline
                if not late:
                    self._state = "online"
            if late:
                self._fail(f"the phone did not answer within {CHECK_SEC} seconds")
                return
            _log(f"online after {self._elapsed()}: {self.name}"
                 + (" (sharing the app's Mobile use session)" if res.get("shared") else ""))
            self._watch()
        except Exception as e:
            self._fail(f"the phone check failed: {e}")

    def _elapsed(self) -> str:
        return f"{time.monotonic() - getattr(self, '_t0', time.monotonic()):.1f}s"

    def show_window(self):
        """Open the phone window, when a task is first delegated to the phone.
        Returns at once; the window comes up on its own thread. Once per
        request: later delegations find it open, and it closes when the
        request lets the phone go."""
        with self._lock:
            if self._released or self._state != "online" or self._window_started:
                return
            self._window_started = True
        threading.Thread(target=self._open_window, daemon=True).start()

    def _open_window(self):
        """"iPhone connected" shimmering on the gradient, then the live screen."""
        from AutoCua.ios_connector import sim_view
        from AutoCua.ios_connector.session import wda_session
        win = sim_view.open_view(text=f"{self._model or 'Phone'} connected", title=self.name,
                                 device="ipad" if self._model.lower() == "ipad" else "iphone")
        opened = time.monotonic()
        with self._lock:
            keep = not self._released and self._state == "online"
            if keep:
                self._window = win
        if not keep:
            _close_window(win)
            return
        port = wda_session.screen_stream()
        if not port:
            with self._lock:
                win, self._window = self._window, None
            _close_window(win)
            return
        _stream_up(port, timeout=10)
        # Let the shimmering words be seen before the phone takes their place.
        rest = SHIMMER_SEC - (time.monotonic() - opened)
        if rest > 0:
            time.sleep(rest)
        with self._lock:
            if self._released or self._window is not win:
                return
        sim_view.show_phones(win, [{"name": self.name, "port": port}])

    def _watch(self):
        """Keep the online line true: a pulled cable (or New chat in the app)
        ends the session, and ios_agent drops off the line."""
        from AutoCua.ios_connector.session import wda_session, plugged_in_udids
        while True:
            time.sleep(WATCH_SEC)
            with self._lock:
                if self._released or self._state != "online":
                    return
            # Another AutoCua run (a terminal "mobile use" run) took the phone:
            # its activate() killed this link's forward. WDA still answers on
            # the port, but for that run, so status() alone cannot tell.
            if not wda_session.holds(self):
                self._fail("another AutoCua run took over the phone")
                return
            try:
                st = wda_session.status()
            except Exception:
                continue
            if st.get("state") == "connected":
                continue
            if st.get("state") in ("error", "disconnected"):
                self._fail(_reason(st) or "the phone disconnected")
                return
            # "connecting": WDA did not answer in time. It answers /status on
            # the same queue as the agent's own requests, and a dense screen
            # can keep it busy for a while, so a slow answer means nothing on
            # its own. Off the cable is what counts.
            plugged = plugged_in_udids()
            if plugged is not None and self.udid not in plugged:
                self._fail("the phone was unplugged")
                return

    def _fail(self, reason: str):
        with self._lock:
            self._state = "offline"
            if not self._released:
                self.reason = reason or self.reason
            owns, self._owns = self._owns, False
            win, self._window = self._window, None
        _log(f"offline after {self._elapsed()}: {reason}")
        _close_window(win)
        if owns:
            self._deactivate()
        self._forget()

    def _deactivate(self):
        """Let the phone go, unless a later request took the session over."""
        try:
            from AutoCua.ios_connector.session import wda_session
            wda_session.deactivate(owner=self)
        except Exception:
            pass

    def _forget(self):
        try:
            _links.remove(self)
        except ValueError:
            pass


# ---------------------------------------------------------------- the runs --

def _milestone_notes(run: dict) -> str:
    """What the iOS agent wrote to its scratchpad during this run: the salvage
    when the `done` value is thin or the run never reached `done`."""
    p = _IOS_SCRATCHPAD / "milestone" / "milestone.md"
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
          phone: PhoneLink = None) -> dict:
    """Start one ios_agent on `task` on the request's connected phone and
    return its run record, the dict `outcome` and `stop` take. Raises
    IOSAgentError when it cannot start."""
    if IS_COMPILED:
        raise IOSAgentError("ios_agent is unavailable in the packaged app")
    if phone is None or phone.state() != "online":
        raise IOSAgentError("ios_agent is currently offline")
    # One phone, one agent: a second one would tap over the first.
    if any(r["subprocess"].poll() is None for r in _runs):
        raise IOSAgentError("an ios_agent is still at work and only one runs at a time: "
                            "hold for it with agent_wait, then start the next")

    from AutoCua.ios_connector.session import WDA_PORT

    sid = f"ios_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    run_dir = data_root() / "ios_agent" / sid
    run_dir.mkdir(parents=True, exist_ok=True)
    result_file = run_dir / "result.json"
    log_file = run_dir / "agent.log"

    cmd = [
        sys.executable, str(_CHILD),
        "--task", _TASK_TEMPLATE.format(task=task),
        "--provider", provider,
        "--model", model,
        "--result", str(result_file),
    ]
    if speed:
        cmd += ["--speed", str(speed)]
    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    # The child's cwd is outside the repo, so make the package importable.
    env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    # The phone this process connected: WDA on the cable forward's port (the
    # iOS package reads it at import, so it is set before the child starts),
    # and pymobiledevice3 pointed at this phone for the app list.
    env["AutoCua_WDA_PORT"] = str(WDA_PORT)
    env["PYMOBILEDEVICE3_UDID"] = phone.udid
    env.pop("AutoCua_IOS_SESSION", None)   # parallel-simulator scoping, never for the phone
    if api_key and provider in _PROVIDER_ENV_KEY:
        env[_PROVIDER_ENV_KEY[provider]] = api_key

    try:
        # stdout/stderr go to a file: the iOS agent prints a lot, and a PIPE
        # nobody drains would deadlock it.
        with open(log_file, "ab") as log:
            proc = subprocess.Popen(cmd, cwd=str(run_dir), env=env,
                                    stdin=subprocess.DEVNULL, stdout=log,
                                    stderr=subprocess.STDOUT)
    except Exception as e:
        raise IOSAgentError(f"ios_agent could not be started: {e}")

    run = {
        "session_id": sid,
        "subprocess": proc,
        "result_file": result_file,
        "log_file": log_file,
        "started": time.time(),
        "deadline": time.monotonic() + TIMEOUT_SEC,
        "stopped": False,
    }
    _runs.append(run)
    # The phone has work now: show it (the first time in this request)
    phone.show_window()
    return run


def stop(run: dict) -> None:
    """End a run from outside (Stop, the end of the main agent's request, or
    the phone going away). The phone session stays: it belongs to the link."""
    run["stopped"] = True
    proc = run.get("subprocess")
    if proc is not None and proc.poll() is None:
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        except Exception:
            pass


def _drop(run: dict) -> None:
    if run in _runs:
        _runs.remove(run)


def outcome(run: dict, phone_online: bool = True):
    """(summary, status) once the run is over, None while it is still working.

    status is `complete` when the iOS agent finished its task, otherwise how
    it ended: `incomplete`, `error`, `timeout` or `stopped`. Call it until it
    answers. phone_online=False (the link lost the phone) ends a run at once.
    """
    proc = run["subprocess"]
    if proc.poll() is None:
        if not phone_online and not run["stopped"]:
            # Notes first: the run is over for good once it is stopped
            notes = _milestone_notes(run) or "nothing was recorded before the phone went away"
            stop(run)
            run["stopped"] = False   # the phone went away, nobody stopped it
            _drop(run)
            return ("The phone disconnected while ios_agent was working.\n\n"
                    f"Recorded before it stopped:\n{notes}", "error")
        if time.monotonic() <= run["deadline"]:
            return None
        notes = _milestone_notes(run) or "nothing was recorded before the time limit"
        stop(run)
        run["stopped"] = False   # it ran out of time, nobody stopped it
        _drop(run)
        return (f"ios_agent ran out of time ({TIMEOUT_SEC // 60} min) before it "
                f"finished.\n\nRecorded before it stopped:\n{notes}", "timeout")

    notes = _milestone_notes(run)
    _drop(run)
    if run.get("stopped"):
        return ("Stopped by user", "stopped")
    try:
        result = json.loads(run["result_file"].read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return (f"ios_agent exited with code {proc.returncode} without a report.\n\n"
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
