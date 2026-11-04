"""The browser agent as a sub-agent of the main agent.

`sub_agent {"agent_type": "browser_agent"}` lands here. It starts the browser
agent (AutoCua/web, the Rust Chrome driver) on a task and returns at once;
the controller then polls `outcome` until the run is over.

Not the `web` tool's fallback (tool/web/web_agent.py), which is a blocking,
headless, read-only research call. This one ACTS: it navigates, clicks and
fills forms, in the person's own Chrome, through the AutoCua extension.

It runs the way parallel `web use` does (see browser_agent_child.py): each
agent works in a background tab of its own and never in another's, so up to
MAX_RUNS run at once. Chrome comes up as it does there (in front on macOS; on
Windows and Linux the system places the new window, and nothing here pulls it
forward). Nothing here starts the browser; the child's own constructor starts
it or attaches to it. So the first one opens Chrome, and start() holds the next
until it has (_wait_for_chrome): started together, each would open Chrome over
the others. With 2 or more at work, the cover parallel web use shows is in
front, through a line of our own (_cover_send).

One text serves macOS, Windows and Linux: keep the three copies identical.

Each run is a subprocess because the web AgentService constructor deletes
CWD-relative conversation/, debug/ and raw_reasoning/ (the main agent's LIVE
folders), so the child gets a working directory of its own.
"""

import atexit
import ctypes
import json
import os
import shutil
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

from AutoCua import data_root, IS_COMPILED

_REPO_ROOT = Path(__file__).resolve().parents[4]
_WEB_SCRATCHPAD = _REPO_ROOT / "AutoCua" / "web" / "scratchpad"
_CHILD = Path(__file__).resolve().parent / "browser_agent_child.py"

# The child has its own 100-step cap; this bounds wall-clock.
TIMEOUT_SEC = int(os.environ.get("AutoCua_BROWSER_AGENT_TIMEOUT", "900"))
# At most this many at once, each in a tab of its own. The controller's
# browser_agent slots (SUB_AGENT_SLOTS) decide first; start() is the backstop.
MAX_RUNS = 4
# How long start() holds a new browser agent while the one before it is still
# opening Chrome (_wait_for_chrome): a few seconds when Chrome is up with the
# extension, more when Chrome has to start or restart.
START_WAIT_SEC = 90
# The runtime (frontend) key reaches the child through its env: the web agent's
# __main__ has no --api_key flag, and its .env loader never overrides a var
# that is already set.
_PROVIDER_ENV_KEY = {
    "openrouter": "OPENROUTER_API_KEY",
    "groq": "GROQ_API_KEY",
    "openai": "OPENAI_API_KEY",
    "anthropic": "ANTHROPIC_API_KEY",
    "google": "GOOGLE_API_KEY",
    "perplexity": "PERPLEXITY_API_KEY",
    "together": "TOGETHER_API_KEY",
    "cerebras": "CEREBRAS_API_KEY",
    "requesty": "REQUESTY_API_KEY",
    "aws": "AWS_API_KEY",
}
# A `done` value shorter than this is almost certainly an activity log, so it
# is topped up with what the agent recorded along the way.
_SHORT_REPORT_CHARS = 200

_TASK_TEMPLATE = """{task}

How to finish:
- Your final `done` call's value is the report the main agent receives, and it is all it receives. State what you did, the result with the exact values, names and URLs that matter, and anything you could not do.
"""


class BrowserAgentError(Exception):
    """The browser agent could not be started."""


# Every run started by this process, so none outlives it.
_runs = []

# The front tab while 2 or more browser agents work, each in a background tab of
# its own: the extension's agents.html ("Multiple agents running" over the looping
# simulation_background.mp4, with the number of agents), the cover parallel web use
# shows (agent_launcher.run_parallel_web_agents). Under 2 it closes. It goes through
# a line to the extension of our own, opened when first needed, dropped with it.
_cover = {"bridge": None, "shown": 0}
_cover_lock = threading.Lock()


def _cover_send(timeout: float = 10.0) -> None:
    """Put up the cover that fits the browser agents at work now, or take it down."""
    with _cover_lock:
        n = sum(r["subprocess"].poll() is None for r in list(_runs))
        want = n if n >= 2 else 0
        if want != _cover["shown"]:
            try:
                if _cover["bridge"] is None:
                    from AutoCua.web.agent import Bridge
                    _cover["bridge"] = Bridge()
                _cover["bridge"].request({"type": "cover", "count": want}, timeout)
                _cover["shown"] = want
            except Exception:
                pass   # a run never fails over its cover page
        if _cover["shown"] == 0:
            _cover["bridge"] = None   # no cover up: its line goes


def _cover_update() -> None:
    """_cover_send in a thread of its own: opening a line to the extension takes a
    second, and the main agent's step does not wait for it."""
    threading.Thread(target=_cover_send, daemon=True).start()


def stop_all() -> None:
    """Quitting the app mid-run must not leave a browser agent working: it acts
    on websites and calls the model, with nobody left to report to. The watcher
    threads are daemons and the run's own cleanup only fires if the run unwinds,
    so nothing else covers a quit.

    Runs at exit, and from the app window's closing event too, because not
    every quit runs atexit: Cmd+Q on macOS leaves through NSApplication
    terminate:, which calls C exit()."""
    for run in list(_runs):
        try:
            stop(run)
        except Exception:
            pass
    # The cover goes too, here and now: at exit no thread is left to take it down
    try:
        _cover_send(timeout=2.0)
    except Exception:
        pass


atexit.register(stop_all)


def _milestone_notes(sid: str) -> str:
    """What the browser agent wrote to its scratchpad during the run: the
    salvage when the `done` value is thin or the run never reached `done`."""
    p = _WEB_SCRATCHPAD / sid / "milestone" / "milestone.md"
    try:
        return p.read_text(encoding="utf-8").strip() if p.exists() else ""
    except OSError:
        return ""


def _tail(path: Path, lines: int = 30) -> str:
    try:
        return "\n".join(path.read_text(encoding="utf-8", errors="replace").splitlines()[-lines:])
    except OSError:
        return ""


def _wait_for_chrome(stop_event=None) -> None:
    """Hold while a browser agent is still opening Chrome and none has it yet:
    a child has Chrome once its run has begun (its run_start event in
    events.jsonl, written right after its constructor took Chrome). Returns at
    once when no browser agent is at work, or one has Chrome; else when the one
    opening it gets there, exits, START_WAIT_SEC passes, or Stop is pressed."""
    deadline = time.monotonic() + START_WAIT_SEC
    while time.monotonic() < deadline and not (stop_event is not None and stop_event.is_set()):
        live = [r for r in _runs if r["subprocess"].poll() is None]
        if not live or any((r["result_file"].parent / "events.jsonl").exists() for r in live):
            return
        time.sleep(0.1)


def start(task: str, provider: str, model: str, api_key: str = None, speed: str = None,
          stop_event=None) -> dict:
    """Start one browser agent on `task` and return its run record, the dict
    `outcome` and `stop` take. Raises BrowserAgentError when it cannot start."""
    if IS_COMPILED:
        raise BrowserAgentError("the browser agent is unavailable in the packaged build")
    # Google Chrome is never downloaded (utils/chrome.py): without it no
    # browser agent starts, and the main agent is told why
    from AutoCua.utils.chrome import MISSING as CHROME_MISSING, find_chrome
    if find_chrome() is None:
        raise BrowserAgentError(CHROME_MISSING)
    # Each one works in a tab of its own (browser_agent_child.py), up to MAX_RUNS
    if sum(r["subprocess"].poll() is None for r in _runs) >= MAX_RUNS:
        raise BrowserAgentError(f"all {MAX_RUNS} browser agents are at work: wait for one of them to "
                                "finish (agent_wait), then start this one")
    # The first one opens Chrome; this one starts once it has, and only attaches
    _wait_for_chrome(stop_event)
    if stop_event is not None and stop_event.is_set():
        raise BrowserAgentError("stopped before it started")

    sid = f"browser_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    run_dir = data_root() / "browser_agent" / sid
    run_dir.mkdir(parents=True, exist_ok=True)
    result_file = run_dir / "result.json"
    log_file = run_dir / "agent.log"

    cmd = [
        sys.executable, str(_CHILD),
        "--task", _TASK_TEMPLATE.format(task=task),
        "--provider", provider,
        "--model", model,
        "--result", str(result_file),
        "--session-id", sid,
    ]
    if speed:
        cmd += ["--speed", str(speed)]
    # Tests only: a private port and profile. Left out, the child uses the web
    # agent's own defaults, exactly as `web use` does.
    if os.environ.get("AutoCua_BROWSER_AGENT_PORT"):
        cmd += ["--browser-port", os.environ["AutoCua_BROWSER_AGENT_PORT"]]
    if os.environ.get("AutoCua_BROWSER_AGENT_PROFILE"):
        cmd += ["--browser-profile", os.environ["AutoCua_BROWSER_AGENT_PROFILE"]]
    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    # The child's cwd is outside the repo, so make the package importable.
    env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    if api_key and provider in _PROVIDER_ENV_KEY:
        env[_PROVIDER_ENV_KEY[provider]] = api_key

    # Windows: the child shares the app's console, so closing that terminal ends
    # it too, as it does on macOS and Linux. Only an app with no console gets
    # the child a hidden one, so no console window pops up.
    creationflags = 0
    if sys.platform == "win32" and not ctypes.windll.kernel32.GetConsoleWindow():
        creationflags = subprocess.CREATE_NO_WINDOW

    try:
        # stdout/stderr go to a file: the browser agent prints a lot, and a
        # PIPE nobody drains would deadlock it.
        with open(log_file, "ab") as log:
            proc = subprocess.Popen(cmd, cwd=str(run_dir), env=env,
                                    creationflags=creationflags,
                                    stdin=subprocess.DEVNULL, stdout=log,
                                    stderr=subprocess.STDOUT)
    except Exception as e:
        raise BrowserAgentError(f"the browser agent could not be started: {e}")

    run = {
        "session_id": sid,
        "subprocess": proc,
        "result_file": result_file,
        "log_file": log_file,
        "deadline": time.monotonic() + TIMEOUT_SEC,
        "stopped": False,
    }
    _runs.append(run)
    _cover_update()   # 2 or more at work: the cover in front
    return run


def stop(run: dict) -> None:
    """End a run from outside (Stop, or the end of the main agent's run). The
    browser itself stays up, as it does after any web run: only the child is
    ended, never its process tree or group, because the browser stays up."""
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
    # The run is over for good, so its notes go with it. Done here and not only
    # in `outcome`: when the app quits, nothing calls `outcome` any more.
    shutil.rmtree(_WEB_SCRATCHPAD / run["session_id"], ignore_errors=True)


def outcome(run: dict):
    """(summary, status) once the run is over, None while it is still working.

    status is `complete` when the browser agent finished its task, otherwise how
    it ended: `incomplete`, `error`, `timeout` or `stopped`. Call it until it
    answers; the run's scratchpad is cleared on the way out.
    """
    proc = run["subprocess"]
    sid = run["session_id"]
    if proc.poll() is None:
        if time.monotonic() <= run["deadline"]:
            return None
        # Notes first: `stop` clears the run's scratchpad
        notes = _milestone_notes(sid) or "nothing was recorded before the time limit"
        stop(run)
        run["stopped"] = False   # it ran out of time, nobody stopped it
        if run in _runs:
            _runs.remove(run)
        _cover_update()
        return (f"The browser agent ran out of time ({TIMEOUT_SEC // 60} min) before it "
                f"finished.\n\nRecorded before it stopped:\n{notes}", "timeout")

    notes = _milestone_notes(sid)
    shutil.rmtree(_WEB_SCRATCHPAD / sid, ignore_errors=True)
    if run in _runs:
        _runs.remove(run)
    _cover_update()   # one fewer at work: the count goes down, or the cover goes
    if run.get("stopped"):
        return ("Stopped by user", "stopped")
    try:
        result = json.loads(run["result_file"].read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return (f"The browser agent exited with code {proc.returncode} without a report.\n\n"
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
