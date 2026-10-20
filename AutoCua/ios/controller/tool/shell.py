"""
shell.py - Host-Mac shell tool for the iPhone agent.

One `shell` call runs BOTH plain shell commands and AppleScript — AppleScript is
just an `osascript -e '...'` command, so there's no separate tool. Everything
goes through the sandboxed Desktop workspace, and a background watcher
auto-clicks any macOS permission popup (TCC) that a command triggers, so
AppleScript that drives another app is handled the same as any other command.
"""

import logging
import subprocess

from ...sandbox import Sandbox

logger = logging.getLogger(__name__)


# Scans every process for a macOS permission popup and clicks its affirmative
# button. Fingerprint: any window or sheet that has a NEGATIVE button — exact
# name "Don't Allow" or "Deny". That structural pattern matches every TCC prompt
# (Automation, Local Network, Files & Folders, Screen Recording, Microphone, …)
# regardless of the affirmative button's label. We then click the affirmative,
# trying "Allow" → "OK" → "Always Allow" so both the automation popup
# ("Allow"/"Don't Allow") AND the folder popup ("OK"/"Don't Allow") are handled.
# AppleScript list membership is exact, so "Don't Allow" never matches "Allow".
# Returns "clicked" (affirmative pressed), "present" (popup seen but not clicked),
# or "none".
_DIALOG_SCANNER_SCRIPT = '''
tell application "System Events"
    set denyNames to {"Don't Allow", "Deny"}
    set affirmNames to {"Allow", "OK", "Always Allow"}
    set sawDialog to false
    repeat with p in application processes
        try
            repeat with w in windows of p
                set containers to {w}
                try
                    set containers to containers & (sheets of w)
                end try
                repeat with c in containers
                    try
                        set bnames to name of buttons of c
                        set hasDeny to false
                        repeat with dn in denyNames
                            if bnames contains dn then set hasDeny to true
                        end repeat
                        if hasDeny then
                            set sawDialog to true
                            repeat with an in affirmNames
                                if bnames contains an then
                                    click (first button of c whose name is an)
                                    return "clicked"
                                end if
                            end repeat
                        end if
                    end try
                end repeat
            end repeat
        end try
    end repeat
    if sawDialog then
        return "present"
    end if
    return "none"
end tell
'''


def _scan_permission_dialog() -> str:
    """Scan for a macOS TCC permission popup and click its affirmative button.

    Returns "clicked" (Allow/OK pressed), "present" (popup is up but could not be
    clicked — usually because Accessibility isn't granted), or "none". Idempotent.
    """
    try:
        result = subprocess.run(
            ["osascript", "-e", _DIALOG_SCANNER_SCRIPT],
            capture_output=True, text=True, timeout=3
        )
        status = result.stdout.strip() if result.returncode == 0 else "none"
        if status not in ("clicked", "present", "none"):
            status = "none"
        if status == "clicked":
            logger.info("Auto-clicked macOS permission popup (Allow/OK)")
        return status
    except Exception as e:
        logger.debug(f"Permission dialog scan failed: {e}")
        return "none"


def _click_automation_allow_button() -> bool:
    """Back-compat wrapper: True if a permission popup's affirmative was clicked."""
    return _scan_permission_dialog() == "clicked"


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
