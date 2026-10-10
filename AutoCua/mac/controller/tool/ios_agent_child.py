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
import os         # noqa: E402


def main():
    parser = argparse.ArgumentParser(description="One ios_agent run (spawned by tool/ios_agent.py)")
    parser.add_argument("--task", type=str, required=True, help="The task to run")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    args = parser.parse_args()

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        # The real phone, never a simulator: open_app lists the installed apps
        # with pymobiledevice3 for hardware (simctl only for a simulator).
        from AutoCua.ios_connector.session import active_target
        active_target.update({"kind": "hardware", "udid": os.environ.get("PYMOBILEDEVICE3_UDID")})

        from AutoCua.ios.agent.main_driver.service import AgentService

        agent = AgentService(provider=args.provider, model=args.model, speed=args.speed)
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
