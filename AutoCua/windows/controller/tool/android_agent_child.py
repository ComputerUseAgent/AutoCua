"""Android Agent Child Runner
==========================
Subprocess entry for one android_agent run started by the main agent
(tool/android_agent.py spawns it; it is run BY PATH, never imported).

It runs the Android agent (the Rust extension in AutoCua/android) on the phone
the parent checked: AutoCua_ANDROID_SERIAL names it, and the agent's screen
reader starts the phone's helper itself and stops it when this process ends.

The process_request result dict is always written to --result.

The app shows the run live, in its box in the main agent's chat: the agent's
tool-flow events and the phone's screens go to events.jsonl and frame.jpg next
to --result (the run's folder), where the main agent's controller reads them.

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

import sys
from pathlib import Path

# Run by path, so Python put THIS folder first on the import path. Its files
# (shell.py, plan.py, web/ ...) must never shadow a real module.
_HERE = Path(__file__).resolve().parent
sys.path[:] = [p for p in sys.path if Path(p or ".").resolve() != _HERE]

import argparse   # noqa: E402
import base64     # noqa: E402
import io         # noqa: E402
import json       # noqa: E402
import os         # noqa: E402
import signal     # noqa: E402
import threading  # noqa: E402
import time       # noqa: E402

# The phone's screen as the app gets it: its longest side in px, and JPEG quality
FRAME_EDGE = 900
FRAME_QUALITY = 70


class _LiveView:
    """The run's live view, written for the app (controller/view.py reads it):
    each tool-flow event as one JSON line in events.jsonl, and each screen,
    made small, as frame.jpg plus a line saying a new one is there. The same
    as ios_agent_child.py's.

    Both are the agent's callbacks, so every call catches everything: the run
    never fails over its live view. Nothing goes to stdout, which is agent.log,
    the main agent's error report."""

    def __init__(self, folder: Path):
        self.folder = folder
        self.frames = 0
        self.lock = threading.Lock()

    def flow(self, event, payload=None):
        """tool_callback: run_start, turn, received, error or run_end"""
        try:
            p = None if payload is None else json.dumps(payload, ensure_ascii=False)
            with self.lock:
                self._write({"k": "flow", "ev": str(event), "p": p})
        except Exception:
            pass

    def frame(self, image_b64):
        """frontend_callback: the phone's plain screen of a scan, as base64 JPEG"""
        try:
            from PIL import Image
            image = Image.open(io.BytesIO(base64.b64decode(image_b64)))
            image.thumbnail((FRAME_EDGE, FRAME_EDGE))
            if image.mode != "RGB":
                image = image.convert("RGB")
            with self.lock:
                # Written whole, then swapped in: the app never reads half a picture
                tmp = self.folder / "frame.tmp"
                image.save(tmp, format="JPEG", quality=FRAME_QUALITY)
                os.replace(tmp, self.folder / "frame.jpg")
                self.frames += 1
                self._write({"k": "frame", "n": self.frames})
        except Exception:
            pass

    def _write(self, row):
        with open(self.folder / "events.jsonl", "ab") as f:
            f.write((json.dumps(row) + "\n").encode("utf-8"))


def _watch_parent():
    """The process that started this run can die without stopping it: a
    closed terminal sends it SIGHUP, which this run, in a process group of
    its own (so Stop reaches the phone helper's adb client), no longer gets.
    Then this run ends itself, its whole group with it, so nothing goes on
    driving the phone with nobody to report to. POSIX: a dead parent's
    children are handed to another one, which getppid() shows."""
    parent = int(os.environ.get("AutoCua_ANDROID_PARENT") or 0)
    if not parent or sys.platform == "win32":
        return

    def watch():
        while os.getppid() == parent:
            time.sleep(1)
        os.killpg(os.getpgrp(), signal.SIGTERM)

    threading.Thread(target=watch, daemon=True).start()


def main():
    _watch_parent()
    parser = argparse.ArgumentParser(description="One android_agent run (spawned by tool/android_agent.py)")
    parser.add_argument("--task", type=str, default=None, help="The task to run")
    parser.add_argument("--standby", action="store_true",
                        help="Get ready first, then read the task as one JSON line on stdin")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    args = parser.parse_args()
    if args.task is None and not args.standby:
        parser.error("--task is required")

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        from AutoCua.android.agent import AgentService

        # Only these two: web_callback and shell_callback add sleeps, and
        # token_callback would move the main agent's memory bar
        live = _LiveView(Path(args.result).resolve().parent) if args.result else None

        def build():
            return AgentService(provider=args.provider, model=args.model, speed=args.speed,
                                tool_callback=live.flow if live else None,
                                frontend_callback=live.frame if live else None)

        agent = build()
        task = args.task
        if args.standby:
            # Started when the phone came online (tool/android_agent.py), so
            # the run begins the moment the main agent sends its task. No
            # line: the request ended without one.
            line = sys.stdin.readline()
            task = json.loads(line)["task"] if line.strip() else None
            # The helper this agent started while it waited may be gone (a
            # phone that dozed off the Wi-Fi): connect again, as a fresh child.
            from AutoCua.android_connector.session import android_session
            if task is not None and android_session.status().get("state") != "connected":
                agent = build()
        if task is None:
            result = {"status": "stopped", "message": "no task came"}
        else:
            result = agent.process_request(task)
        exit_code = 0
    except KeyboardInterrupt:
        result = {"status": "stopped", "message": "interrupted (Ctrl+C)"}
        exit_code = 130
    except Exception as e:  # noqa: BLE001 - the result file is the error channel
        result = {"status": "error", "message": f"{type(e).__name__}: {e}"}
    finally:
        if args.result:
            try:
                path = Path(args.result)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(result, indent=2, ensure_ascii=False),
                                encoding="utf-8")
            except OSError:
                pass
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
