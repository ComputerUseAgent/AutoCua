"""
Web Agent Child Runner
======================
Subprocess entry for one parallel web-agent task.

Usage:
    python -m AutoCua.web.agent --task "..." --provider openai --model gpt-6-astra

When called from run_parallel_web_agents (agent_launcher.py):
    - The parent pre-launched Chrome; this child attaches to the same port.
    - The parent set cwd to ./parallel/task_N/, so every CWD-relative artifact
      (conversation/, raw_reasoning/, debug/scans) is isolated per task.
    - --session-id scopes the shared web/scratchpad to scratchpad/{sid}/.
    - single_tab=True pins this agent to ONE dedicated tab of the shared
      browser: no tab tools, other agents' tabs are invisible.
    - The process_request result dict is always written to --result.

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

import argparse
import json
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(
        description="One parallel web-agent task (spawned by run_parallel_web_agents)"
    )
    parser.add_argument("--task", type=str, required=True, help="The task to run")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--headless", action="store_true", help="Chrome without a window")
    parser.add_argument("--save-conversation", action="store_true",
                        help="Write conversation logs under the task cwd")
    parser.add_argument("--browser-port", type=int, default=None,
                        help="Shared Chrome debug port (attaches, never relaunches)")
    parser.add_argument("--session-id", type=str, default=None,
                        help="Scopes web/scratchpad/{sid}/ for this agent")
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    parser.add_argument("--browser-profile", type=str, default=None,
                        help="Named Chrome profile dir (default profile when omitted)")
    args = parser.parse_args()

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        from AutoCua.web.agent import AgentService

        agent = AgentService(
            provider=args.provider,
            model=args.model,
            save_conversation=args.save_conversation,
            speed=args.speed,
            headless=args.headless,
            browser_port=args.browser_port,
            session_id=args.session_id,
            single_tab=True,
            browser_profile=args.browser_profile,
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
