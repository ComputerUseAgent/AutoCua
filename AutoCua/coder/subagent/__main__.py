"""
Subagent entry point
====================
The child process the coder starts for one `subagent` call (the mac coder's
minion): a read-only scout that answers one question and reports back.

Usage:
    python -m AutoCua.coder.subagent --task "where is X defined?" \
        --provider openrouter --model gemini-3.6-flash --result path/to/result.json

The API key, when the coder has one, comes in the AutoCua_CODER_API_KEY
environment variable, never on the command line; without it the provider
falls back to .env and the app's saved key. run_subagent (subagent/service.rs)
runs the loop and always writes the result JSON {"task", "summary", "status"}
to --result.

Exit codes: 0 = the subagent returned, 1 = crash, 130 = Ctrl+C.
"""

import argparse
import os
import sys
import traceback


def main():
    parser = argparse.ArgumentParser(
        description="Subagent - Read-only scout for the parent coder agent",
    )
    parser.add_argument("--task", type=str, required=True,
                        help="Question/objective for the subagent to answer")
    parser.add_argument("--provider", type=str, required=True,
                        help="LLM provider (inherited from the parent agent)")
    parser.add_argument("--model", type=str, required=True,
                        help="LLM model name (inherited from the parent agent)")
    parser.add_argument("--result", type=str, required=True,
                        help="Path to write the result JSON when complete")
    parser.add_argument("--save-conversation", action="store_true",
                        help="Write cli_subagent_conversation/<session>/ in the current folder")
    args = parser.parse_args()

    # Taken out of the environment once read, so the subagent's own shell
    # commands never see the key.
    api_key = os.environ.pop("AutoCua_CODER_API_KEY", None) or None

    try:
        from AutoCua.coder.agent_native import run_subagent

        run_subagent(args.task, args.provider, args.model, args.result,
                     api_key=api_key, save_conversation=args.save_conversation)
    except KeyboardInterrupt:
        sys.exit(130)
    except Exception:  # noqa: BLE001 - the traceback is the parent's crash detail
        traceback.print_exc()
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
