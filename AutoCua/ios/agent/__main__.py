"""
iOS Agent Child Runner
======================
Subprocess entry for one parallel iOS-simulator task.

Usage:
    python -m AutoCua.ios.agent --task "..." --provider anthropic --model claude-sonnet-5

When called from run_parallel_sim_agents (agent_launcher.py):
    - The parent booted this task's OWN simulator and started its OWN
      WebDriverAgent; this child only attaches to --wda-port.
    - The parent set cwd to ./parallel/task_N/, so every CWD-relative artifact
      (conversation/, raw_reasoning/, debug/) is isolated per task.
    - --session-id scopes the package-level scratchpad to
      AutoCua/ios/scratchpad/{sid}/, so parallel agents never overwrite each
      other's milestone notes or todo list.
    - The process_request result dict is always written to --result.

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

import argparse
import json
import os
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(
        description="One parallel iOS-simulator task (spawned by run_parallel_sim_agents)"
    )
    parser.add_argument("--task", type=str, required=True, help="The task to run")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--save-conversation", action="store_true",
                        help="Write conversation logs under the task cwd")
    parser.add_argument("--wda-port", type=int, default=None,
                        help="WebDriverAgent port for this task's simulator")
    parser.add_argument("--sim-udid", type=str, default=None,
                        help="This task's simulator (used for simctl app scans)")
    parser.add_argument("--session-id", type=str, default=None,
                        help="Scopes AutoCua/ios/scratchpad/{sid}/ for this agent")
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    args = parser.parse_args()

    # Belt and braces for a hand-run child. run_parallel_sim_agents already
    # puts both of these in the child's environment, which is the only way that
    # works for the port: `python -m AutoCua.ios.agent` imports the package —
    # and with it ios/tree/element.py, which resolves the WDA URL at import —
    # BEFORE this function runs. Setting the port here only takes effect if the
    # agent modules somehow have not been imported yet.
    if args.wda_port:
        os.environ.setdefault("AutoCua_WDA_PORT", str(args.wda_port))
    if args.session_id:
        os.environ.setdefault("AutoCua_IOS_SESSION", args.session_id)

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        from AutoCua.ios_connector.session import active_target
        # The parent owns the simulator's lifecycle; this child only needs to
        # know it IS a simulator (open_app scans apps with simctl, not
        # pymobiledevice3) and which one.
        active_target.update({"kind": "simulation", "udid": args.sim_udid})

        from AutoCua.ios.agent.main_driver.service import AgentService

        agent = AgentService(
            provider=args.provider,
            model=args.model,
            save_conversation=args.save_conversation,
            speed=args.speed or "quality",
        )
        result = agent.process_request(args.task)
        exit_code = 0
    except KeyboardInterrupt:
        result = {"status": "stopped", "message": "interrupted (Ctrl+C)"}
        exit_code = 130
    except Exception as e:  # noqa: BLE001 — the result file is the error channel
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
