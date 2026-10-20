"""
Minion Sub-Agent Entry Point (macOS)
====================================
Subprocess entry for the read-only scout minion.

Usage:
    python -m AutoCua.mac.agent.minions --task "your question here"

    Options:
        --task      : Required. The question/objective for the minion to answer.
        --provider  : LLM provider (default: openrouter)
        --model     : LLM model (default: gemini-3.6-flash)
        --result    : Path to write result JSON when complete (optional)

When called from the parent CLI agent (via the `minion` action):
    - The CLI agent's controller spawns this as a subprocess.
    - The minion runs in its own session-isolated scratchpad (cli_minion/{sid}/).
    - On exit, the structured summary is written to --result and surfaced to the
      parent CLI agent as a <minion_completed> tool response.

When called directly for testing:
    python -m AutoCua.mac.agent.minions --task "where is X defined?"
"""

import argparse
import json
from pathlib import Path

try:
    from app import debug_log, debug_exception
except ImportError:
    def debug_log(msg, level="INFO"):
        pass
    def debug_exception(context):
        pass


def main():
    parser = argparse.ArgumentParser(
        description="Minion Sub-Agent - Read-only scout for the parent CLI agent",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    python -m AutoCua.mac.agent.minions --task "where is _read_scratchpad_from_file defined and who calls it?"
    python -m AutoCua.mac.agent.minions --task "list every file under src/ that imports requests"
        """
    )

    parser.add_argument(
        "--task",
        type=str,
        required=True,
        help="Question/objective for the minion to answer"
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
        help="Path to write result JSON when complete (for parent agent integration)"
    )
    parser.add_argument(
        "--api_key",
        type=str,
        default=None,
        help="Runtime API key for LLM provider (optional, falls back to .env)"
    )

    args = parser.parse_args()

    from .service import AgentService

    def on_complete(result: dict):
        if args.result:
            result_path = Path(args.result)
            result_path.parent.mkdir(parents=True, exist_ok=True)
            with open(result_path, 'w', encoding='utf-8') as f:
                json.dump(result, f, indent=2, ensure_ascii=False)
            try:
                print(f"Result written to: {args.result}")
            except (ValueError, OSError):
                pass

    agent = AgentService(
        provider=args.provider,
        model=args.model,
        save_conversation=False,
        api_key=args.api_key,
        task=args.task,
        on_complete=on_complete if args.result else None,
    )
    agent.process_request(args.task)


if __name__ == "__main__":
    main()
