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

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

import sys
from pathlib import Path

# Run by path, so Python put THIS folder first on the import path. Its files
# (shell.py, plan.py, web/ ...) must never shadow a real module.
_HERE = Path(__file__).resolve().parent
sys.path[:] = [p for p in sys.path if Path(p or ".").resolve() != _HERE]

import argparse   # noqa: E402
import json       # noqa: E402


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

        agent = AgentService(
            provider=args.provider,
            model=args.model,
            speed=args.speed,
            browser_port=args.browser_port,
            session_id=args.session_id,
            browser_profile=args.browser_profile,
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
