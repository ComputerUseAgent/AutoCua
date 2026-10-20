"""
CLI Agent Entry Point
======================
This module allows the CLI agent to be run as a subprocess.

Usage:
    python -m AutoCua.mac.agent.coder --task "your task here"

    Options:
        --task      : Required. The task for CLI agent to execute
        --provider  : LLM provider (default: openrouter)
        --model     : LLM model (default: gemini-3.6-flash)
        --result    : Path to write result JSON when complete (optional)

When called from main agent:
    - Main agent spawns this as subprocess
    - CLI agent runs with its own UI (pywebview on main thread)
    - Result is written to --result file when done

When called directly for testing:
    - Run: python -m AutoCua.mac.agent.coder --task "test task"
    - Or use main.py at project root with MODE = "shell use"
"""

import argparse
import json
from pathlib import Path

# Import debug_log for error logging (fallback if app module not available)
try:
    from app import debug_log, debug_exception
except ImportError:
    def debug_log(msg, level="INFO"):
        pass
    def debug_exception(context):
        pass


def main():
    parser = argparse.ArgumentParser(
        description="CLI Agent - Terminal-based coding assistant",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    python -m AutoCua.mac.agent.coder --task "fix the bug in test.py"
    python -m AutoCua.mac.agent.coder --task "create hello world" --provider openrouter --model gemini-3.6-flash
        """
    )
    
    parser.add_argument(
        "--task", 
        type=str, 
        required=True,
        help="Task description for the CLI agent"
    )
    parser.add_argument(
        "--provider",
        type=str,
        required=True,
        help="LLM provider (inherited from the parent agent)"
    )
    parser.add_argument(
        "--model",
        type=str,
        required=True,
        help="LLM model name (inherited from the parent agent)"
    )
    parser.add_argument(
        "--result", 
        type=str, 
        default=None,
        help="Path to write result JSON when complete (for main agent integration)"
    )
    parser.add_argument(
        "--api_key",
        type=str,
        default=None,
        help="Runtime API key for LLM provider (optional, falls back to .env)"
    )
    parser.add_argument(
        "--no_external_terminal",
        action="store_true",
        default=False,
        help="Disable spawning sub-agents (minions) in new Terminal.app windows. "
             "Default: terminals ON for cli.py / main.py terminal UX. Pass this from "
             "app.py / headless mode to keep minion subprocesses hidden."
    )
    parser.add_argument(
        "--report-usage",
        action="store_true",
        default=False,
        help="Emit per-LLM-call token usage as stdout marker events so the app's "
             "memory bar tracks this agent's context. Passed only for TOP-LEVEL "
             "Shell-use runs — never for minions or main-agent dispatches."
    )
    parser.add_argument(
        "--request-no",
        type=int,
        default=1,
        help="Which request of the ongoing Shell-use conversation this task is "
             "(1 = first). The task is injected as <user_request=N> so the model "
             "can tell one session's requests apart. Standalone runs omit it."
    )
    parser.add_argument(
        "--history",
        type=str,
        default=None,
        help="Path to a Shell-use conversation-history JSON file. When set, the "
             "agent seeds its context from it and rewrites it after every step — "
             "this is what threads one conversation across the runs of a shell "
             "chat. Omitted for main-agent dispatches and minions."
    )

    args = parser.parse_args()

    # Import here to avoid circular imports at module load
    from .service import AgentService

    # Callback to write result when CLI agent exits
    def on_complete(result: dict):
        if args.result:
            result_path = Path(args.result)
            result_path.parent.mkdir(parents=True, exist_ok=True)
            with open(result_path, 'w', encoding='utf-8') as f:
                json.dump(result, f, indent=2, ensure_ascii=False)
            try:
                print(f"Result written to: {args.result}")
            except (ValueError, OSError):
                pass  # stdout closed in compiled mode

    # Create and run CLI agent. Output streams to stdout for the parent
    # main agent's pill UI; the agent loop runs synchronously on this
    # subprocess's main thread.
    agent = AgentService(
        provider=args.provider,
        model=args.model,
        save_conversation=True,
        api_key=args.api_key,
        task=args.task,
        on_complete=on_complete if args.result else None,
        external_terminal=not args.no_external_terminal,
        report_usage=args.report_usage,
        request_no=args.request_no,
        history_file=args.history,
    )
    agent.process_request(args.task)


if __name__ == "__main__":
    main()
