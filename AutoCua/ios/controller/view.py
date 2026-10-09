import logging
import time
import sys
import threading

from .service import controller_service
from .task_tracker.service import TaskTrackerService
from .tool.open_app import app_launcher_service
from .tool.videoplayer import VideoPlayerService
from ...vault.service import vault_service, MAX_PIN_ATTEMPTS, DEVICE_ENTRIES

# Services still being ported from macOS one file at a time. Guarded imports
# keep the router importable while the port is in progress: a missing service
# makes its actions return a clear error instead of crashing the controller.
try:
    from .scratchpad.service import ScratchpadService
except ImportError:
    ScratchpadService = None
try:
    from .tool import ShellService
except ImportError:
    ShellService = None
try:
    from .tool.web.service import WebService
except ImportError:
    WebService = None

# Configure logger
logger = logging.getLogger(__name__)


class ControllerView:
    def __init__(self, provider: str = None, model: str = None, web_callback=None, shell_callback=None, api_key: str = None, stop_event=None):
        """Initialize the Controller View - central router for all iPhone actions

        Args:
            provider: LLM provider name for web search
            model: LLM model name for web search
            web_callback: Optional callback for web search status (start/end)
            shell_callback: Optional callback for shell execution status (start/result/end)
            api_key: Optional runtime API key for LLM providers (passed to web search)
            stop_event: Optional threading.Event for stopping actions mid-execution
        """
        self.api_key = api_key
        self.stop_event = stop_event
        # Module singleton — the SAME instance the scanner feeds element
        # mappings into during scan_elements.
        self.controller_service = controller_service
        self.task_tracker = TaskTrackerService()
        self.videoplayer_service = VideoPlayerService(controller_service)
        self.scratchpad_service = ScratchpadService() if ScratchpadService else None
        self.shell_service = ShellService() if ShellService else None
        self.provider = provider
        self.model = model
        self.web_callback = web_callback
        self.shell_callback = shell_callback
        self._stop_loading = False
        self._pin_attempts = {}         # vault entry name -> PIN tries that may have been spent

    def _web_loading_animation(self):
        """Display animated loading indicator for web search.

        Skipped when stdout is NOT a TTY (i.e. piped subprocess) — the `\\r`
        carriage-return overwrite trick only works in a real terminal.
        """
        if not sys.stdout.isatty():
            return
        dots = ["", ".", "..", "..."]
        idx = 0
        while not self._stop_loading:
            sys.stdout.write(f"\r🌐 Web{dots[idx % len(dots)]}   ")
            sys.stdout.flush()
            idx += 1
            time.sleep(0.5)

    def _enter_vault_pin(self, action_data):
        """vault {"id": 0, "value": "pin"}: the vault taps its PIN on the on-screen keypad.

        Every refusal happens before the first tap. Results and logs never
        carry a digit, a key id or a coordinate (see the PIN section of
        vault/service.py).
        """
        base = {"action": "vault", "element": 0, "kind": "pin"}
        if len(action_data) > 1:
            # The keys come from this step's scan; another action could move the screen first.
            return {**base, "status": "error",
                    "message": "A PIN vault must be the only action in its step. Re-issue it alone."}
        # Keypad first, vault second: asking where no keypad is must not reveal whether a PIN is saved.
        keys = vault_service.find_pin_keys(self.controller_service.elements)
        if not keys:
            return {**base, "status": "error",
                    "message": "No digit keypad (0-9) recognised on screen. Open the PIN screen first."}

        name, pin = vault_service.get_pin()
        if not pin:
            return {**base, "status": "error",
                    "message": f'No PIN saved in the vault for "{name}". Ask the user to add '
                               f'"{name}": {{"pin": "..."}} to AutoCua_data/vault/credentials.json.'}
        tries = self._pin_attempts.get(name, 0)
        if tries >= MAX_PIN_ATTEMPTS:
            return {**base, "status": "error",
                    "message": f'The "{name}" PIN was already tried twice in this task. Stop and ask the user, '
                               "another wrong try could lock the keypad."}

        try:
            outcome = vault_service.enter_pin(self.controller_service, keys, pin,
                                              device_entry=name.lower().replace(" ", "") in DEVICE_ENTRIES)
        except Exception:
            outcome = {"status": "error", "pressed": True, "closed": False,
                       "message": "PIN entry failed and the keys may have been pressed. Do not retry - "
                                  "check the next screenshot, and ask the user if unsure."}
        # A pad that closed starts fresh; only a try that may have reached the pad counts.
        if outcome.pop("closed", False):
            self._pin_attempts[name] = 0
        elif outcome.pop("pressed", True):
            self._pin_attempts[name] = tries + 1
        outcome.pop("pressed", None)
        logger.info(f"🔐 Vault PIN on keypad: {outcome['status']}")
        return {**base, **outcome}

    def route_action(self, action_data):
        """
        Route actions to appropriate service based on action type.

        Flat action format (matching the macOS agent):
            [{"type": "click", "id": 4}, {"type": "wait", "value": "2"}, ...]

        Args:
            action_data (list): The action list from the LLM response.
                A single flat action dict is also accepted and wrapped.

        Returns:
            dict: Result of the action execution (single result, or
                  {"action": "multiple", "results": [...]} for a sequence).
        """
        try:
            # Accept a single flat action dict by wrapping it in a list
            if isinstance(action_data, dict):
                action_data = [action_data]

            results = []

            for action_item in action_data:
                # Check stop before every action
                if self.stop_event and self.stop_event.is_set():
                    return {"status": "stopped", "action": "stop", "message": "Stopped by user"}

                if not isinstance(action_item, dict) or not action_item.get("type"):
                    logger.warning(f"Action item missing 'type' field: {action_item}")
                    continue

                action_type = action_item.get("type")

                if action_type == "click":
                    element_id = action_item.get("id")
                    try:
                        element_num = int(element_id)
                        self.controller_service.click(element_num)
                        logger.info(f"✓ Clicked element {element_num}")
                        result = {"status": "success", "action": "click", "element": element_num}
                    except Exception as e:
                        logger.error(f"✗ Click failed: {str(e)}")
                        result = {"status": "error", "action": "click", "element": element_id, "message": str(e)}
                    results.append(result)
                    if result.get("status") == "error":
                        return result

                elif action_type == "input":
                    element_id = action_item.get("id")
                    text_value = action_item.get("value")
                    try:
                        element_num = int(element_id)
                        self.controller_service.type_text(element_num, text_value)
                        logger.info(f"✓ Typed '{text_value}' in element {element_num}")
                        result = {"status": "success", "action": "input", "element": element_num, "text": text_value}
                    except Exception as e:
                        logger.error(f"✗ Input failed: {str(e)}")
                        result = {"status": "error", "action": "input", "element": element_id, "message": str(e)}
                    results.append(result)
                    if result.get("status") == "error":
                        return {
                            "status": "error",
                            "action": "input",
                            "results": results,
                            "message": f"Input insertion in element {element_id} failed"
                        }

                elif action_type == "scroll":
                    element_id = action_item.get("id")
                    direction = action_item.get("value")
                    try:
                        element_num = int(element_id)
                        self.controller_service.scroll(element_num, direction)
                        logger.info(f"✓ Scrolled element {element_num} {direction}")
                        result = {"status": "success", "action": "scroll", "element": element_num, "direction": direction}
                    except Exception as e:
                        logger.error(f"✗ Scroll failed: {str(e)}")
                        result = {"status": "error", "action": "scroll", "element": element_id, "message": str(e)}
                    results.append(result)
                    if result.get("status") == "error":
                        return {
                            "status": "error",
                            "action": "scroll",
                            "results": results,
                            "message": f"Scroll on element {element_id} failed"
                        }

                elif action_type == "open_app":
                    app_name = str(action_item.get("value") or "").strip()
                    logger.info(f"Opening application: {app_name}")

                    # Special 'home' case — return to the home screen
                    if app_name.lower() == "home":
                        try:
                            success = app_launcher_service.go_home()
                        except Exception as e:
                            logger.error(f"✗ Go home failed: {str(e)}")
                            success = False
                        if success:
                            result = {"status": "success", "action": "tool", "tool": "open_app", "app": "home"}
                            results.append(result)
                        else:
                            return {
                                "status": "error",
                                "action": "tool",
                                "tool": "open_app",
                                "app": "home",
                                "message": "Failed to navigate to home screen"
                            }
                    else:
                        try:
                            launch = app_launcher_service.launch_app(app_name)
                        except Exception as e:
                            logger.error(f"✗ App launch failed: {str(e)}")
                            launch = {"ok": False, "message": f"App launch raised: {e}"}
                        # Report what was ACTUALLY launched (resolved display
                        # name + bundle id), never just the query - and success
                        # only when the launcher confirmed the foreground app.
                        result = {
                            "status": "success" if launch.get("ok") else "error",
                            "action": "tool",
                            "tool": "open_app",
                            "requested": app_name,
                            "message": launch.get("message", ""),
                        }
                        if launch.get("display_name"):
                            result["app"] = launch["display_name"]
                            result["bundle_id"] = launch["bundle_id"]
                        if launch.get("ok"):
                            if launch.get("verified") is False:
                                result["verified"] = False
                            logger.info(f"Opened {result.get('app', app_name)}")
                            results.append(result)
                        else:
                            logger.error(f"Failed to open {app_name}: {result['message']}")
                            return result

                elif action_type == "wait":
                    wait_time = float(action_item.get("value", "1"))
                    logger.info(f"Waiting for {wait_time} seconds...")
                    elapsed = 0.0
                    while elapsed < wait_time:
                        if self.stop_event and self.stop_event.is_set():
                            return {"status": "stopped", "action": "stop", "message": "Stopped by user"}
                        time.sleep(min(0.5, wait_time - elapsed))
                        elapsed += 0.5
                    logger.info("Wait completed")
                    result = {"status": "success", "action": "tool", "tool": "wait", "duration": wait_time}
                    results.append(result)

                elif action_type == "web":
                    query = action_item.get("value")
                    logger.info(f"Performing web search: {query}")

                    if WebService is None:
                        return {
                            "status": "error",
                            "action": "tool",
                            "tool": "web",
                            "query": query,
                            "message": "Web service not available yet on iOS"
                        }

                    if self.web_callback:
                        self.web_callback("start")

                    self._stop_loading = False
                    loading_thread = threading.Thread(target=self._web_loading_animation)
                    loading_thread.daemon = True
                    loading_thread.start()

                    try:
                        web_service = WebService(self.provider, self.model, self.api_key, stop_event=self.stop_event)
                        web_result = web_service.search(query)
                    finally:
                        self._stop_loading = True
                        loading_thread.join(timeout=1)
                        # Clear the in-place text only in TTY mode
                        if sys.stdout.isatty():
                            sys.stdout.write("\r" + " " * 50 + "\r")
                            sys.stdout.flush()

                        if self.web_callback:
                            self.web_callback("end")
                            # Wait for CSS fade-out to complete before next action
                            time.sleep(0.7)

                    # WebService reports a failed search inside its text
                    failed = str(web_result).startswith("{\ntool: web,\nstatus: error,")
                    result = {
                        "status": "error" if failed else "success",
                        "action": "tool",
                        "tool": "web",
                        "query": query,
                        "result": web_result
                    }
                    results.append(result)

                elif action_type == "shell":
                    # Runs on the host Mac this agent runs on — not the iPhone.
                    command = action_item.get("value", "")

                    if self.shell_service is None:
                        return {
                            "status": "error",
                            "action": "shell",
                            "message": "Shell service not available yet on iOS"
                        }

                    # Signal frontend: terminal card appears
                    if self.shell_callback:
                        self.shell_callback("start", command)

                    result = self.shell_service.run(command)

                    # Signal frontend: show success/fail result
                    if self.shell_callback:
                        shell_status = result.get("status", "failed")
                        shell_output = result.get("output", result.get("error", ""))
                        self.shell_callback("result", {"status": shell_status, "output": shell_output or ""})
                        # Hold result on screen for 2 seconds
                        time.sleep(2)
                        self.shell_callback("end")
                        # Wait for CSS fade-out to complete before next action
                        time.sleep(0.7)

                    results.append(result)
                    if result.get("status") == "error" and result.get("error"):
                        return result

                elif (action_type == "vault"
                      and str(action_item.get("value") or "").strip().lower() in ("pin", "passcode")):
                    # Every PIN goes to the keypad path, whatever id was sent: the
                    # field path below would guess a credential from the label.
                    result = self._enter_vault_pin(action_data)
                    if result["status"] != "success":
                        return result
                    results.append(result)

                elif action_type == "vault":
                    element_number = action_item.get("id")
                    credential_kind = action_item.get("value", "")
                    try:
                        # credential_kind (username/password/phone_number) rides along
                        # for the upcoming kind-aware vault lookup; today's vault
                        # infers the field from the element tree.
                        credential_value = vault_service.get_credential_for_element(int(element_number))

                        if credential_value:
                            # Use existing type_text to fill the credential
                            self.controller_service.type_text(element_number, credential_value)
                            logger.info(f"🔐 Vault filled element {element_number} ({credential_kind})")
                            result = {
                                "status": "success",
                                "action": "vault",
                                "element": element_number,
                                "kind": credential_kind,
                                "message": "Credential filled successfully"
                            }
                            results.append(result)
                        else:
                            logger.error(f"No credential found for element {element_number}")
                            return {
                                "status": "error",
                                "action": "vault",
                                "element": element_number,
                                "kind": credential_kind,
                                "message": "No matching credential found"
                            }
                    except Exception as e:
                        logger.error(f"Vault action failed: {str(e)}")
                        return {
                            "status": "error",
                            "action": "vault",
                            "element": element_number,
                            "kind": credential_kind,
                            "message": str(e)
                        }

                elif action_type == "video_player":
                    command = action_item.get("value")
                    try:
                        if command == "close":
                            success = self.videoplayer_service.close()
                            tool_response = "videoplayer: Result - Closed" if success else "videoplayer: Result - Failed to close"
                        elif command == "streaming":
                            is_streaming = self.videoplayer_service.check_streaming()
                            tool_response = "videoplayer: Result - Streaming" if is_streaming else "videoplayer: Result - Not streaming"
                        elif command == "pause":
                            success = self.videoplayer_service.pause()
                            tool_response = "videoplayer: Result - Paused" if success else "videoplayer: Result - Failed to pause"
                        elif command == "play":
                            success = self.videoplayer_service.play()
                            tool_response = "videoplayer: Result - Playing" if success else "videoplayer: Result - Failed to play"
                        else:
                            raise ValueError(f"Unknown video_player command: {command}")

                        result = {
                            "status": "success",
                            "action": "video_player",
                            "command": command,
                            "tool_response": tool_response
                        }
                        logger.info(f"🎬 {tool_response}")
                    except Exception as e:
                        logger.error(f"Video player action failed: {str(e)}")
                        result = {
                            "status": "error",
                            "action": "video_player",
                            "command": command,
                            "message": str(e),
                            "tool_response": f"videoplayer: Result - Error: {str(e)}"
                        }
                    results.append(result)

                elif action_type == "todo_list":
                    todo_value = action_item.get("value")
                    self.task_tracker.save_todo(todo_value)
                    result = {"status": "success", "action": "todo_created"}
                    results.append(result)

                elif action_type == "update_todo":
                    task_number = int(action_item.get("value", "0"))
                    success = self.task_tracker.update_task(task_number)
                    if success:
                        result = {"status": "success", "action": "todo_updated", "task": task_number}
                        results.append(result)
                    else:
                        # Reported on its own call; the rest of the batch still runs
                        results.append({
                            "status": "error",
                            "action": "todo_update_failed",
                            "message": f"Task {task_number} is not in the todo list"
                        })

                elif action_type == "scratchpad":
                    scratchpad_value = action_item.get("value")
                    if self.scratchpad_service is None:
                        return {
                            "status": "error",
                            "action": "scratchpad_failed",
                            "message": "Scratchpad service not available yet on iOS"
                        }
                    success = self.scratchpad_service.append_scratchpad(scratchpad_value)
                    if success:
                        result = {"status": "success", "action": "scratchpad_added", "scratchpad": scratchpad_value}
                        results.append(result)
                    else:
                        return {
                            "status": "error",
                            "action": "scratchpad_failed",
                            "message": "Could not add scratchpad entry"
                        }

                elif action_type == "done":
                    summary = action_item.get("value")
                    logger.info(f"Task Complete: {summary}")
                    return {"status": "success", "action": "done", "summary": summary}

                else:
                    logger.warning(f"Unknown action type: {action_type}")
                    results.append({
                        "status": "error",
                        "action": action_type,
                        "message": f"Unknown action type: {action_type}"
                    })

                # Check if last action was stopped
                if results and results[-1].get("status") == "stopped":
                    return results[-1]

            if len(results) == 0:
                return {"status": "error", "message": "No valid action found"}
            elif len(results) == 1:
                return results[0]
            else:
                return {"status": "success", "action": "multiple", "results": results}

        except Exception as e:
            logger.error(f"Error routing action: {str(e)}")
            return {"status": "error", "message": str(e)}

    def set_elements(self, elements_mapping, application_name=""):
        """Set the elements mapping in controller service"""
        self.controller_service.update_elements(elements_mapping)
