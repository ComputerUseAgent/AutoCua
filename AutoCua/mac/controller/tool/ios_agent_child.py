"""iOS Agent Child Runner
======================
Subprocess entry for one ios_agent run started by the main agent
(tool/ios_agent.py spawns it; it is run BY PATH, never imported).

It runs the iOS agent on the REAL phone the parent already connected, never a
simulator: WDA answers on the cable forward's port (AutoCua_WDA_PORT, set by
the parent before this imports the iOS package) and pymobiledevice3 points at
the phone (PYMOBILEDEVICE3_UDID). It never opens or closes the phone session
itself: a second session would kill the parent's.

Not AutoCua/ios/agent/__main__.py: that one is the parallel SIMULATOR runner
and marks the device as a simulator.

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
import faulthandler  # noqa: E402
import io         # noqa: E402
import json       # noqa: E402
import os         # noqa: E402
import threading  # noqa: E402

# The phone's screen as the app gets it: its longest side in px, and JPEG quality
FRAME_EDGE = 900
FRAME_QUALITY = 70


class _LiveView:
    """The run's live view, written for the app (controller/view.py reads it):
    each tool-flow event as one JSON line in events.jsonl, and each screen,
    made small, as frame.jpg plus a line saying a new one is there. The same
    as browser_agent_child.py's.

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
        """frontend_callback: the phone's full-size screen of a scan, as base64 PNG"""
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


def main():
    parser = argparse.ArgumentParser(description="One ios_agent run (spawned by tool/ios_agent.py)")
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
        # The run begins in 2 to 3 s. If the start hangs, every thread's stack
        # goes to this log after 15 s, so it says where it waited (the parent
        # starts the child again at 20 s, ios_agent.py START_SEC).
        faulthandler.dump_traceback_later(15)
        # The real phone, never a simulator: open_app lists the installed apps
        # with pymobiledevice3 for hardware (simctl only for a simulator).
        from AutoCua.ios_connector.session import active_target
        active_target.update({"kind": "hardware", "udid": os.environ.get("PYMOBILEDEVICE3_UDID")})

        from AutoCua.ios.agent.iphone_use.service import AgentService

        # Only these two: web_callback and shell_callback add sleeps, and
        # token_callback would move the main agent's memory bar
        live = _LiveView(Path(args.result).resolve().parent) if args.result else None
        agent = AgentService(provider=args.provider, model=args.model, speed=args.speed,
                             tool_callback=live.flow if live else None,
                             frontend_callback=live.frame if live else None)
        faulthandler.cancel_dump_traceback_later()
        task = args.task
        if args.standby:
            # Started when the phone came online (tool/ios_agent.py), so the
            # run begins the moment the main agent sends its task. No line:
            # the request ended without one.
            line = sys.stdin.readline()
            task = json.loads(line)["task"] if line.strip() else None
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
