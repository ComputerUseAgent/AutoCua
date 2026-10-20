# AutoCua/linux/controller/tool/app_script.py
# Linux app-script tool — the counterpart of the macOS AppleScript tool.
# Uses open_app() for activation/launching; the agent writes a bash script
# that drives the app through its command line or D-Bus interface, and the
# service runs it verbatim once the app is up.

import logging
import subprocess
import time

from .open_app import _move_to_main_screen, _is_app_running, _bring_to_front, open_app

logger = logging.getLogger(__name__)

SCRIPT_TIMEOUT = 30


class AppScriptService:
    """Generic script executor for any Linux app.

    Contract: the agent supplies a complete bash script that drives `app`
    (`firefox --new-tab URL`, `nautilus --select PATH`, `gdbus call ...`).
    The runtime handles app launch and activation — the script must not
    start the app itself, it only sends it commands. Same shape as the
    macOS AppleScriptService so the controller routes it identically.
    """

    def __init__(self):
        pass

    def execute(self, app_name: str, action: str) -> dict:
        """
        Execute a complete bash script on behalf of the agent.

        Args:
            app_name: Application name (used for activation/launch only — never
                injected into the script).
            action: Complete bash script to execute verbatim.

        Returns:
            dict: {status, action, app, command, output/error}
        """
        app_name = (app_name or "").strip()
        script = (action or "").strip()

        if not app_name or not script:
            return {
                "status": "error",
                "action": "app_script",
                "message": "Both app name and script are required"
            }

        if _is_app_running(app_name):
            # Already running: present the existing window.
            _bring_to_front(app_name)
            time.sleep(0.3)
        else:
            # Not running: launch via the desktop-entry discovery path. open_app
            # waits ~1 s for the window.
            open_app(app_name)

        result = self._run(script)

        if result.get("status") == "success":
            _move_to_main_screen()

        result["app"] = app_name
        result["command"] = action
        return result

    def _run(self, script: str) -> dict:
        """Execute the script with bash and return a structured result"""
        try:
            result = subprocess.run(
                ["/bin/bash", "-c", script],
                capture_output=True,
                text=True,
                timeout=SCRIPT_TIMEOUT
            )

            if result.returncode != 0:
                error_msg = (result.stderr.strip() or result.stdout.strip()
                             or f"exit code {result.returncode}")
                first_line = script.lstrip().split('\n', 1)[0][:120]
                logger.error(f"app script error: {error_msg} | script[1]: {first_line}")
                return {
                    "status": "error",
                    "action": "app_script",
                    "message": f"{error_msg} (script started with: {first_line})"
                }

            output = result.stdout.strip()
            logger.info(f"app script success: {output[:200]}")
            return {
                "status": "success",
                "action": "app_script",
                "output": output
            }

        except subprocess.TimeoutExpired:
            logger.error(f"app script timed out ({SCRIPT_TIMEOUT}s)")
            return {
                "status": "error",
                "action": "app_script",
                "message": f"Script timed out ({SCRIPT_TIMEOUT}s)"
            }
        except Exception as e:
            logger.error(f"app script execution failed: {e}")
            return {
                "status": "error",
                "action": "app_script",
                "message": str(e)
            }
