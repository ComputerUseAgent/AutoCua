"""
shell.py - Host-Mac shell tool for the iPhone agent.

One `shell` call runs BOTH plain shell commands and AppleScript — AppleScript is
just an `osascript -e '...'` command, so there's no separate tool. Every
command runs in the coder's sandbox (AutoCua/coder/sandbox), the one sandbox
in AutoCua.
"""

import logging

from AutoCua.coder import Sandbox

logger = logging.getLogger(__name__)


class ShellService:
    """Host-Mac shell for the iPhone agent — quick commands (plain or osascript
    AppleScript) via the sandboxed Desktop environment."""

    def __init__(self):
        """Initialize ShellService with Sandbox pointing to existing sandbox_workspace"""
        self.sandbox = Sandbox()

    def run(self, command: str, input_text: str = None) -> dict:
        """
        Execute a shell command and return formatted result.

        Args:
            command: shell command to execute (plain shell, or `osascript -e '...'`
                for AppleScript — both run the same way)
            input_text: Optional stdin input for interactive commands

        Returns:
            dict with agent_location, shell, status, output
        """
        try:
            result = self.sandbox.run(command, input_text)

            agent_location = self.sandbox.get_cwd()

            # Handle input_required
            if result.get("error") == "input_required":
                output = ""
                if result.get("stdout"):
                    output += result["stdout"]
                if result.get("stderr"):
                    output += result["stderr"]

                response = {
                    "status": "input_required",
                    "action": "shell",
                    "agent_location": agent_location,
                    "shell": command,
                    "message": f"Process waiting for input. Last output: '{result.get('last_output', '')}'. Use input parameter."
                }
                if output.strip():
                    response["output"] = output.strip()
                return response

            # Handle timeout
            if result.get("timeout"):
                output = ""
                if result.get("stdout"):
                    output += result["stdout"]
                if result.get("stderr"):
                    output += result["stderr"]

                response = {
                    "status": "timeout",
                    "action": "shell",
                    "agent_location": agent_location,
                    "shell": command,
                    "message": result.get("message", "Command timed out")
                }
                if output.strip():
                    response["output"] = output.strip()
                return response

            # Normal result
            output = ""
            if result.get("stdout"):
                output += result["stdout"]
            if result.get("stderr"):
                output += result["stderr"]

            returncode = result.get("returncode")
            succeeded = result.get("success")

            response = {
                "status": "success" if succeeded else "failed",
                "action": "shell",
                "agent_location": agent_location,
                "shell": command,
            }

            # Always surface the exit code so the agent can tell a real error
            # from an empty result (e.g. grep exits 1 on no match).
            if returncode is not None:
                response["exit_code"] = returncode

            # Show exactly what the command returned (verbatim stdout + stderr).
            # When the terminal returns absolutely nothing, show a clear marker
            # instead of a blank string — the status/exit_code already say whether
            # it succeeded, so this just makes "no output" unambiguous (like a
            # real terminal / Claude Code).
            clean_output = output.strip()
            response["output"] = clean_output if clean_output else "(no output)"

            error = result.get("error", "")
            if error:
                response["error"] = error

            return response

        except Exception as e:
            logger.error(f"ShellService error: {str(e)}")
            return {
                "status": "failed",
                "action": "shell",
                "agent_location": self.sandbox.get_cwd(),
                "shell": command,
                "error": str(e)
            }
