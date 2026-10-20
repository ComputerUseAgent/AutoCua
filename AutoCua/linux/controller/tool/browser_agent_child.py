"""Browser Agent Child Runner
==========================
Subprocess entry for one browser agent started by the main agent
(tool/browser_agent.py spawns it; it is run BY PATH, never imported).

It runs the web agent exactly the way `web use` does: the agent starts Chrome
itself (or attaches to the one already up), the agent takes over the blank
tab, the window shows the tab it works in, and it has its tab tools.

One text serves macOS, Windows and Linux: keep the three copies identical.

Not AutoCua/web/agent/__main__.py: that one is the PARALLEL runner and pins
each agent to one background tab (single_tab=True), which the window never
shows. One browser agent runs at a time here, so it can own the window.

    --session-id scopes the web scratchpad to scratchpad/{sid}/, so the
      launcher can read this run's notes and clear them afterwards.
    --browser-port / --browser-profile are for tests only. Left out, the agent
      uses its default port and its default automation profile, as web use does.
    The process_request result dict is always written to --result.

The app shows the run live, in its box in the main agent's chat: the agent's
tool-flow events and its screens go to events.jsonl and frame.jpg next to
--result (the run's folder), where the main agent's controller reads them.

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
import threading  # noqa: E402

# The run's screen as the app gets it: its longest side in px, and JPEG quality
FRAME_EDGE = 900
FRAME_QUALITY = 70


class _LiveView:
    """The run's live view, written for the app (controller/view.py reads it):
    each tool-flow event as one JSON line in events.jsonl, and each screen,
    made small, as frame.jpg plus a line saying a new one is there.

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
        """frontend_callback: the screen of a scan, as base64"""
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
    parser = argparse.ArgumentParser(description="One browser agent run (spawned by tool/browser_agent.py)")
    parser.add_argument("--task", type=str, required=True, help="The task to run")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--session-id", type=str, default=None,
                        help="Scopes web/scratchpad/{sid}/ for this run")
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    parser.add_argument("--browser-port", type=int, default=None,
                        help="Tests only: a private debug port")
    parser.add_argument("--browser-profile", type=str, default=None,
                        help="Tests only: a named profile (default profile when omitted)")
    args = parser.parse_args()

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        from AutoCua.web.agent import AgentService

        # No text_callback: the agent does not guard that one
        live = _LiveView(Path(args.result).resolve().parent) if args.result else None
        agent = AgentService(
            provider=args.provider,
            model=args.model,
            speed=args.speed,
            browser_port=args.browser_port,
            session_id=args.session_id,
            browser_profile=args.browser_profile,
            tool_callback=live.flow if live else None,
            frontend_callback=live.frame if live else None,
        )
        result = agent.process_request(args.task)
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
