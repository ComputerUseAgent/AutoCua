"""Browser-agent fallback for the `web` tool.

Providers with no native web-search API (Together AI) still have to honour
the `web` action — Structure.md requires every provider to. This hands the
query to the browser agent (AutoCua/web, the Rust/CDP Chrome driver) running
on the SAME provider+model, waits for it, and returns its `done` summary as
the web result. The task text below is what turns that summary into a proper
report rather than a one-line activity log.

Runs the browser agent as a subprocess (mirrors agent_launcher's parallel
runner and the minion await loop) rather than in-process, because:
  * the web AgentService constructor deletes CWD-relative conversation/,
    debug/ and raw_reasoning/ — the mac agent's LIVE folders — so the child
    gets its own working directory;
  * the child owns its own headless Chrome on a DEDICATED port, never the
    visible one a `web use` run may have left on the default port — a visible
    browser popping up would corrupt the desktop agent's screenshots;
  * a stop request or wall-clock timeout only has to terminate a process.
"""

import json
import os
import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path

from AutoCua import data_root, IS_COMPILED

_REPO_ROOT = Path(__file__).resolve().parents[5]
_WEB_SCRATCHPAD = _REPO_ROOT / "AutoCua" / "web" / "scratchpad"

# Dedicated Chrome port: launch_chrome_impl ATTACHES to whatever is already on
# a port and then ignores --headless, and Chrome deliberately stays up after a
# `web use` run — so the default 9222 could be a visible window.
BROWSER_PORT = int(os.environ.get("AutoCua_WEB_FALLBACK_PORT", "9333"))
# Dedicated Chrome profile too: a port alone is not isolation - Chrome given
# a user-data-dir that a running instance already holds hands the launch to
# that instance (a new tab in the VISIBLE window, and the debug port never
# opens). mac catches that via the profile lock; Windows has no such lock.
BROWSER_PROFILE = os.environ.get("AutoCua_WEB_FALLBACK_PROFILE", "web_fallback")
# The child also has its own 100-step cap; this bounds wall-clock.
TIMEOUT_SEC = int(os.environ.get("AutoCua_WEB_FALLBACK_TIMEOUT", "900"))
_POLL_SEC = 0.5
# Runtime (frontend) key reaches the child through its env — the web agent's
# __main__ has no --api_key flag, and its .env loader never overrides a var
# that is already set.
_PROVIDER_ENV_KEY = {"together": "TOGETHER_API_KEY", "cerebras": "CEREBRAS_API_KEY",
                     "aws": "AWS_API_KEY"}
# A `done` value shorter than this is almost certainly an activity log, not
# the answer — top it up with what the agent recorded along the way.
_SHORT_REPORT_CHARS = 200

_TASK_TEMPLATE = """Research the following on the web and deliver the answer as a written report.

<research_request>
{query}
</research_request>

How to work:
- Open a search engine via `update_tab` (https://www.google.com — if it shows a consent/captcha wall use https://duckduckgo.com or https://www.bing.com) and visit several credible sources: official sites, primary documents, reputable news/reference sites. Cross-check key facts across at least two sources when possible.
- Record every key finding (numbers, dates, names, exact URLs) in the scratchpad as you go so nothing is lost.
- Read only: never log in, sign up, buy, download, or change anything.

How to finish — THIS IS THE DELIVERABLE:
- Your final `done` call's value MUST BE THE ANSWER ITSELF as a detailed, well-formatted Markdown report — NOT a summary of the actions you took.
- Structure: a title line, `##` headings and `###` subheadings per sub-topic, `-` bullets, and the key numbers/facts/quotes stated explicitly. Go in depth where the request needs it; keep it brief where it doesn't.
- End with a `## Sources` section listing the URLs you actually used.
- If something could not be found, say so explicitly instead of guessing.
"""


class WebAgentSearchError(Exception):
    """The browser agent did not produce a report. `note` is the short reason
    for the tool result's Note: line; the message carries the detail."""

    def __init__(self, note: str, detail: str = ""):
        super().__init__(detail or note)
        self.note = note


def _terminate(proc: subprocess.Popen) -> None:
    if proc.poll() is not None:
        return
    try:
        proc.terminate()
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    except Exception:
        pass


def _tail(path: Path, lines: int = 30) -> str:
    try:
        return "\n".join(path.read_text(encoding="utf-8", errors="replace").splitlines()[-lines:])
    except OSError:
        return ""


def _milestone_notes(sid: str) -> str:
    """What the browser agent wrote to its scratchpad during the run — the
    salvage when the `done` value is thin or the run never reached `done`."""
    p = _WEB_SCRATCHPAD / sid / "milestone" / "milestone.md"
    try:
        return p.read_text(encoding="utf-8").strip() if p.exists() else ""
    except OSError:
        return ""


def web_search(query: str, provider: str, model: str, api_key: str = None, stop_event=None) -> str:
    """Run the browser agent on `query` and return its report (Markdown).

    Raises WebAgentSearchError when there is no report to return: stopped by
    the user, timed out, the child crashed, or it finished without `done`.
    """
    if IS_COMPILED:
        raise WebAgentSearchError("browser-agent web search is unavailable in the packaged build")

    sid = f"web_{int(time.time())}_{uuid.uuid4().hex[:6]}"
    run_dir = data_root() / "web_fallback" / sid
    run_dir.mkdir(parents=True, exist_ok=True)
    result_file = run_dir / "result.json"
    log_file = run_dir / "agent.log"

    cmd = [
        sys.executable, "-m", "AutoCua.web.agent",
        "--task", _TASK_TEMPLATE.format(query=query),
        "--provider", provider,
        "--model", model,
        "--result", str(result_file),
        "--headless",
        "--session-id", sid,
        "--browser-port", str(BROWSER_PORT),
        "--browser-profile", BROWSER_PROFILE,
    ]
    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"
    # The child's cwd is outside the repo, so make the package importable.
    env["PYTHONPATH"] = str(_REPO_ROOT) + os.pathsep + env.get("PYTHONPATH", "")
    if api_key and provider in _PROVIDER_ENV_KEY:
        env[_PROVIDER_ENV_KEY[provider]] = api_key

    print(f"\n🌐 web: {provider} has no native search — delegating to the headless "
          f"browser agent ({model}); log: {log_file}")

    try:
        # stdout/stderr go to a file: the browser agent prints a lot, and a
        # PIPE nobody drains would deadlock it. Deliberately NO
        # start_new_session — the child stays in this process group so a Stop
        # of the parent (killpg on the coder) takes it down too.
        with open(log_file, "ab") as log:
            proc = subprocess.Popen(cmd, cwd=str(run_dir), env=env,
                                    stdin=subprocess.DEVNULL, stdout=log,
                                    stderr=subprocess.STDOUT)
            deadline = time.monotonic() + TIMEOUT_SEC
            while proc.poll() is None:
                if stop_event is not None and stop_event.is_set():
                    _terminate(proc)
                    raise WebAgentSearchError("Stopped by user")
                if time.monotonic() > deadline:
                    _terminate(proc)
                    raise WebAgentSearchError(
                        f"browser agent timed out after {TIMEOUT_SEC // 60} min",
                        _milestone_notes(sid) or "no findings were recorded before the timeout")
                time.sleep(_POLL_SEC)

        try:
            result = json.loads(result_file.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            raise WebAgentSearchError(
                f"browser agent exited with code {proc.returncode} without a result",
                _tail(log_file))

        status = str(result.get("status") or "error")
        message = str(result.get("message") or "").strip()
        if status == "success" and message:
            if len(message) < _SHORT_REPORT_CHARS:
                notes = _milestone_notes(sid)
                if notes:
                    message += "\n\nFindings recorded during the run:\n" + notes
            return message

        notes = _milestone_notes(sid)
        detail = message or "no message"
        if notes:
            detail += "\n\nFindings recorded before it stopped:\n" + notes
        raise WebAgentSearchError(f"browser agent did not finish ({status}): {message or 'no message'}", detail)
    finally:
        # The run's own scratchpad is spent; result.json + agent.log stay in
        # run_dir for inspection.
        shutil.rmtree(_WEB_SCRATCHPAD / sid, ignore_errors=True)


if __name__ == "__main__":
    q = input("Search: ")
    print(web_search(q, "together", "minimax-m3"))
