"""
Android Agent Child Runner
==========================
Subprocess entry for one parallel Android-emulator task.

Usage:
    python -m AutoCua.android.agent --task "..." --provider anthropic --model claude-sonnet-5 --serial emulator-5556

When called from run_parallel_emulator_agents (agent_launcher.py):
    - The parent booted this task's OWN emulator; this child starts the phone
      helper on --serial (android_session) and stops it when it ends.
    - The parent set cwd to ./parallel/task_N/, so every CWD-relative artifact
      (conversation/, raw_reasoning/, debug/) is isolated per task.
    - --session-id scopes the package-level scratchpad to
      AutoCua/android/scratchpad/{sid}/ (AutoCua_ANDROID_SESSION), so parallel
      agents never overwrite each other's milestone notes or todo list.
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
        description="One parallel Android-emulator task (spawned by run_parallel_emulator_agents)"
    )
    parser.add_argument("--task", type=str, required=True, help="The task to run")
    parser.add_argument("--provider", type=str, required=True, help="LLM provider")
    parser.add_argument("--model", type=str, required=True, help="LLM model name")
    parser.add_argument("--speed", type=str, default=None, help='"quality" or "fast"')
    parser.add_argument("--save-conversation", action="store_true",
                        help="Write conversation logs under the task cwd")
    parser.add_argument("--serial", type=str, default=None,
                        help="This task's emulator (adb serial, emulator-<port>)")
    parser.add_argument("--session-id", type=str, default=None,
                        help="Scopes AutoCua/android/scratchpad/{sid}/ for this agent")
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when complete")
    args = parser.parse_args()

    # Belt and braces for a hand-run child: run_parallel_emulator_agents puts
    # both in the child's environment already. The serial also keeps the
    # screen reader on this emulator when it starts the helper again, and the
    # agent reads the session when it is built, below.
    if args.serial:
        os.environ.setdefault("AutoCua_ANDROID_SERIAL", args.serial)
    if args.session_id:
        os.environ.setdefault("AutoCua_ANDROID_SESSION", args.session_id)

    result = {"status": "error", "message": "child crashed before the agent returned"}
    exit_code = 1
    try:
        from AutoCua.android_connector.session import android_session

        res = android_session.activate(args.serial)
        if not res.get("ok"):
            raise RuntimeError(f"Android connection failed: {res.get('error')}")

        from AutoCua.android.agent import AgentService

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
