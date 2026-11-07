"""
Coder entry point
=================
The coder as a child process: computer use's coder_agent (and the app's
Shell use, which runs through it) starts it for one task and waits for its
result file.

Usage:
    python -m AutoCua.coder --task "fix the bug in test.py" \
        --provider openrouter --model gemini-3.6-flash --result path/to/result.json

It takes the arguments the old Python coder took (AutoCua/<os>/agent/coder),
so the controllers start it the same way. --report-usage, --request-no and
--history are accepted and ignored: the Rust coder does not use them yet.
The API key comes in --api_key or in the AutoCua_CODER_API_KEY environment
variable; without either the provider falls back to .env and the app's
saved key.

The result JSON is {"task", "summary", "status"}: status "complete" when the
coder called exit (the summary is what it reported), "partial" when it
stopped before that, "error" when it crashed. It is written through a temp
file, so the parent never reads half of it, and also on a crash and on
Ctrl+C, so a parent with no process handle (the coder in its own terminal
window) never waits for a file that will not come.

Exit codes: 0 = the coder returned, 1 = crash, 130 = Ctrl+C.
"""

import argparse
import json
import os
import sys
import traceback
from pathlib import Path


def _exit_summary(agent):
    """What the coder reported with its exit call: the `value` of the exit
    call in its last step, since the loop does not hand it back."""
    try:
        step = json.loads(agent.assistant_messages[-1])
    except (IndexError, TypeError, ValueError):
        return ""
    for call in reversed(step.get("tool_calls") or []):
        fn = call.get("function") or call
        if fn.get("name") != "exit":
            continue
        args = fn.get("arguments") or {}
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except ValueError:
                args = {}
        value = args.get("value") if isinstance(args, dict) else None
        return value if isinstance(value, str) else ""
    return ""


def _write_result(path, task, summary, status):
    if not path:
        return
    result = Path(path)
    try:
        result.parent.mkdir(parents=True, exist_ok=True)
        tmp = result.with_name(result.name + ".tmp")
        tmp.write_text(json.dumps({"task": task, "summary": summary, "status": status},
                                  indent=2, ensure_ascii=False), encoding="utf-8")
        os.replace(tmp, result)
    except OSError:
        traceback.print_exc()


def main():
    parser = argparse.ArgumentParser(description="Coder - the coding agent, as a child process")
    parser.add_argument("--task", type=str, required=True,
                        help="The task for the coder")
    parser.add_argument("--provider", type=str, required=True,
                        help="LLM provider (inherited from the parent agent)")
    parser.add_argument("--model", type=str, required=True,
                        help="LLM model name (inherited from the parent agent)")
    parser.add_argument("--result", type=str, default=None,
                        help="Path to write the result JSON when done")
    parser.add_argument("--api_key", type=str, default=None,
                        help="API key for the provider (optional)")
    parser.add_argument("--no_external_terminal", action="store_true", default=False,
                        help="Run the coder's subagents without their own terminal windows, "
                             "their output piped (the app passes it)")
    # Still passed by the app's Shell use; the Rust coder ignores them.
    parser.add_argument("--report-usage", action="store_true", default=False,
                        help=argparse.SUPPRESS)
    parser.add_argument("--request-no", type=int, default=1, help=argparse.SUPPRESS)
    parser.add_argument("--history", type=str, default=None, help=argparse.SUPPRESS)
    args = parser.parse_args()

    # Taken out of the environment once read, so the coder's own shell
    # commands never see the key.
    api_key = args.api_key or os.environ.pop("AutoCua_CODER_API_KEY", None) or None

    try:
        from AutoCua.coder import AgentService

        agent = AgentService(provider=args.provider, model=args.model, save_conversation=True,
                             api_key=api_key, external_terminal=not args.no_external_terminal)
        outcome = agent.process_request(args.task)
    except KeyboardInterrupt:
        _write_result(args.result, args.task, "Stopped before the coder finished.", "partial")
        sys.exit(130)
    except Exception as e:  # noqa: BLE001 - the parent gets the reason in the result
        traceback.print_exc()
        _write_result(args.result, args.task, f"The coder crashed: {e}", "error")
        sys.exit(1)

    if outcome == "Exit":
        _write_result(args.result, args.task, _exit_summary(agent) or "Task completed", "complete")
    else:
        _write_result(args.result, args.task,
                      f"The coder stopped before it finished: {outcome}", "partial")
    sys.exit(0)


if __name__ == "__main__":
    main()
