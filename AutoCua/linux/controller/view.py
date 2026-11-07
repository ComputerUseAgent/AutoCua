import base64
import json
import logging
import time
import sys
import os
import shlex
import shutil
import threading
import subprocess
import uuid
from pathlib import Path

from .service import ControllerService
from .task_tracker.service import TaskTrackerService
from .scratchpad.service import ScratchpadService
from .tool import open_app, ShellService, AppScriptService
from .hotkey.service import HotkeyService
from .tool.web.service import WebService
from .tool.screenshot import ScreenshotService
try:
    from app import debug_log, app_data_dir
except ImportError:
    def debug_log(message, level="INFO"):
        pass
    def app_data_dir():
        return Path(".")

# Configure logger
logger = logging.getLogger(__name__)


# Agent types the `sub_agent` tool can start on this platform, each with its
# number of slots, so at most that many of a type run at once. Every slot has
# an id of its own, numbered on across the types (slot_ids): coder_agent 1-4,
# browser_agent 5-8, then the phone, 9. android_agent can start only while the
# request's Android phone checked out (android_state), and only in Daisy chain
# mode (daisy_chain): in This PC it is offline and refused.
SUB_AGENT_SLOTS = {"coder_agent": 4, "browser_agent": 4, "android_agent": 1}
SUB_AGENT_TYPES = tuple(SUB_AGENT_SLOTS)
# The phones, after every other slot
PHONE_AGENTS = ("android_agent",)


def _read_new_lines(path: Path, live: dict, key: str) -> list:
    """The lines a sub-agent added to `path` since the last call, read from the
    byte offset kept in live[key]. A last line still being written waits for
    the next call; a file that shrank was started over, so it is read again
    from the top. A file that is not there (yet, or any more) gives nothing."""
    pos = live.get(key, 0)
    try:
        with open(path, "rb") as f:
            if os.fstat(f.fileno()).st_size < pos:
                pos = 0
            f.seek(pos)
            data = f.read()
    except OSError:
        return []
    end = data.rfind(b"\n") + 1
    live[key] = pos + end
    lines = data[:end].decode("utf-8", errors="replace").split("\n")
    return [line.strip() for line in lines if line.strip()]


class ControllerView:
    def __init__(self, provider: str = None, model: str = None, web_callback=None, shell_callback=None, cli_callback=None, api_key: str = None, stop_event=None, external_terminal: bool = False, cli_report_usage: bool = False, cli_request_no: int = None, cli_history_file=None):
        """Initialize the Controller View - central router for all actions

        Args:
            provider: LLM provider name for web search
            model: LLM model name for web search
            web_callback: Optional callback for web search status (start/end)
            shell_callback: Optional callback for shell execution status (start/result/end)
            cli_callback: Optional callback for CLI agent streaming (await_start/task_start/task_line/task_end/await_end)
            api_key: Optional runtime API key for LLM providers (passed to CLI agent)
            stop_event: Optional threading.Event for stopping actions mid-execution
            external_terminal: If True, dispatched CLI sub-agents are launched in their own visible OS terminal window (gnome-terminal or another installed terminal emulator) instead of having their stdout/stderr piped back. Used by main.py headless mode so the user can watch sub-agent progress live.
            cli_report_usage: If True, dispatched CLI agents get `--report-usage`
                (for the frontend memory bar). Only the Shell-use path (frontend
                run_shell_task) sets this. The Rust coder takes the flag but does
                not use it yet.
            cli_request_no: Which request of the ongoing Shell-use conversation the
                dispatched task is (passed to the coder as --request-no). None = omit
                (request 1). The Rust coder does not use it yet.
            cli_history_file: Path to the Shell-use conversation-history file.
                When set, dispatched CLI agents get `--history <path>`. Only the
                Shell-use path (frontend run_shell_task) passes this. The Rust
                coder does not use it yet.
        """
        self.api_key = api_key
        self.stop_event = stop_event
        self.external_terminal = external_terminal
        self.cli_report_usage = cli_report_usage
        self.cli_request_no = cli_request_no
        self.cli_history_file = cli_history_file
        self.controller_service = ControllerService(stop_event=stop_event)
        self.task_tracker = TaskTrackerService()
        self.scratchpad_service = ScratchpadService()
        self.hotkey_service = HotkeyService(stop_event=stop_event)
        self.shell_service = ShellService()
        self.app_script_service = AppScriptService()
        self.screenshot_service = ScreenshotService(self.controller_service, sandbox_workspace=str(self.shell_service.sandbox.sandbox_root))
        self.provider = provider
        self.model = model
        self.web_callback = web_callback
        self.shell_callback = shell_callback
        self.cli_callback = cli_callback
        self._stop_loading = False

        # CLI Agent tracking - multi-task support
        self._cli_tasks = []          # Active: [{"task": str, "subprocess": Popen, "result_file": Path}]
        self._cli_completed = []      # Done: [{"task": str, "summary": str, "status": str}]
        self._cli_agent_lock = threading.Lock()
        self._cli_await_active = False  # Whether an await_start has been emitted but not yet matched by await_end

        # Sub-agent registry: one row per sub-agent the main agent started, shown to
        # it every step as <sub_agents> and never written to the scratchpad. A row
        # holds its slot (its id in slot_ids, the one the main agent sees)
        # from its start until its report is handed over (take_sub_agent_reports).
        # "id" is a hidden number that counts up through the whole chat (the main
        # driver seeds sub_agent_last_id on resume): slot ids are reused, so the
        # run's box in the chat and the lists below are keyed by it.
        self._sub_agents = []         # [{"id": int, "agent": str, "slot": int, "task": str, "status": str,
                                      #   "reported": bool, "seen": bool}]
        self.sub_agent_last_id = 0
        self._browser_tasks = []      # Browser agents at work: [{"id": int, "task": str, "run": dict}]
        self._android_tasks = []      # android_agent runs at work: [{"id": int, "task": str, "run": dict}]
        self._sub_ended = set()       # Browser / Android ids whose box in the chat got its sub_end (sent once)
        self.android = None           # The request's AndroidLink (tool/android_agent.py), set by the main driver
        self._phones_online = []      # The phones in the order they came online: their ids (slot_ids)
        self.daisy_chain = False      # Daisy chain mode: the phone sub-agent is reachable; set by the main driver (off = This PC)
        self.speed = None             # The main agent's speed, so a browser agent runs the same way
        self.scan_on = True           # The `scan` tool's switch; the main driver reads it each step

    def _web_loading_animation(self):
        """Display animated loading indicator for web search.

        Skipped when stdout is NOT a TTY (i.e. piped subprocess) — the `\r`
        carriage-return overwrite trick only works in a real terminal; in a pipe
        all the partial writes accumulate and stream into the parent's reader as
        ugly text like "🌐 Web 🌐 Web. 🌐 Web..." which then floods the pill.
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
    
    def _safe_cli_emit(self, event_type: str, *args):
        """Invoke self.cli_callback safely; never let a callback exception bubble up."""
        if self.cli_callback:
            try:
                self.cli_callback(event_type, *args)
            except Exception as e:
                logger.error(f"cli_callback({event_type}) failed: {e}")

    def _spawn_cli_external_terminal(self, cli_cmd: list, task_description: str):
        """Spawn the CLI sub-agent in a new visible terminal window.

        The Linux twin of the macOS Terminal.app launcher: the first terminal
        emulator found on PATH opens a window running the same `cli_cmd` the
        piped path would run, so the user sees the sub-agent's live output.
        A trailing shell keeps the window open once the agent exits, so the
        output can still be read. Completion is still detected via the
        result-file watcher, so we don't need a Popen handle here. Returns None.
        """
        repo_root = os.getcwd()
        inner = " ".join(shlex.quote(arg) for arg in cli_cmd)
        shell_line = f"cd {shlex.quote(repo_root)} && {inner}; exec bash"
        title = f"CLI Agent: {task_description[:60]}"
        # (binary, argv). gnome-terminal and ptyxis are GNOME's; konsole and
        # xfce4-terminal the other desktops'; x-terminal-emulator is
        # Debian/Ubuntu's alternatives link to whatever the user prefers.
        launchers = [
            ("gnome-terminal", ["gnome-terminal", f"--title={title}", "--", "bash", "-c", shell_line]),
            ("ptyxis", ["ptyxis", "--new-window", "-T", title, "--", "bash", "-c", shell_line]),
            ("konsole", ["konsole", "-p", f"tabtitle={title}", "-e", "bash", "-c", shell_line]),
            ("xfce4-terminal", ["xfce4-terminal", f"--title={title}", "-e", f"bash -c {shlex.quote(shell_line)}"]),
            ("x-terminal-emulator", ["x-terminal-emulator", "-e", "bash", "-c", shell_line]),
            ("xterm", ["xterm", "-T", title, "-e", "bash", "-c", shell_line]),
        ]
        for binary, argv in launchers:
            if shutil.which(binary) is None:
                continue
            try:
                subprocess.Popen(argv, start_new_session=True)
                debug_log(f"CLI sub-agent launched in external {binary} window: {task_description[:80]}")
                return None
            except Exception as e:
                debug_log(f"Failed to spawn {binary} for CLI sub-agent: {e}", "ERROR")
        debug_log("No terminal emulator found to show the CLI sub-agent", "ERROR")
        return None

    def _read_cli_stream(self, pipe, task_id: str, stream: str):
        """Reader thread: forwards each line from a subprocess pipe to the frontend.

        Loops until the pipe closes (subprocess exit). Each line emits a 'task_line'
        event tagged with task_id and stream ("out" or "err").
        """
        if pipe is None:
            logger.warning(f"_read_cli_stream({task_id}, {stream}): pipe is None")
            return
        logger.info(f"_read_cli_stream alive ({task_id}, {stream})")
        line_count = 0
        try:
            for raw in iter(pipe.readline, b""):
                if not raw:
                    break
                try:
                    line = raw.decode("utf-8", errors="replace").rstrip("\r\n")
                except Exception:
                    line = str(raw)
                if line == "" and stream == "err":
                    # don't spam empty stderr lines
                    continue
                line_count += 1
                if line_count <= 3 or line_count % 50 == 0:
                    logger.info(f"_read_cli_stream({task_id}, {stream}) line #{line_count}: {line[:120]}")
                self._safe_cli_emit("task_line", task_id, line, stream)
        except Exception as e:
            logger.error(f"_read_cli_stream({task_id}, {stream}) error: {e}")
        finally:
            logger.info(f"_read_cli_stream done ({task_id}, {stream}, total={line_count} lines)")
            try:
                pipe.close()
            except Exception:
                pass

    def _cli_agent_complete_callback(self, result: dict, result_file: Path):
        """Callback when CLI agent finishes execution"""
        task_id = result_file.stem
        with self._cli_agent_lock:
            # Move from active to completed
            agent_id = next((t.get("id") for t in self._cli_tasks if t["result_file"] == result_file), None)
            self._cli_tasks = [t for t in self._cli_tasks if t["result_file"] != result_file]
            self._report_sub_agent(agent_id, "coder_agent",
                                   result.get("task", "Unknown task"),
                                   result.get("summary", "Completed"),
                                   result.get("status", "complete"))
            logger.info(f"CLI Agent completed: {result.get('summary', 'No summary')}")

        # Notify frontend that this task ended
        self._safe_cli_emit("task_end", task_id, result.get("status", "complete"), result.get("summary", ""))
        self._end_await_if_idle()

    def _end_await_if_idle(self):
        """Close the streaming UI once no coder is left to show. A hold that ends
        early (another sub-agent finished first) leaves it open, so the coder
        still at work keeps its card."""
        with self._cli_agent_lock:
            if not self._cli_await_active or len(self._cli_tasks) > 0:
                return
            self._cli_await_active = False
        self._safe_cli_emit("await_end")

    def _report_sub_agent(self, agent_id, agent: str, task: str, summary: str, status: str):
        """A sub-agent is done: queue its report for the main agent, under the
        slot id it knows, and flip its row. The slot stays held until the report
        is handed over. Call with _cli_agent_lock held."""
        row = next((r for r in self._sub_agents if r["id"] == agent_id), None)
        self._cli_completed.append({
            "id": row["slot"] if row else agent_id,
            "agent": agent,
            "task": task,
            "summary": summary,
            "status": status
        })
        if row is not None:
            row["status"] = status
            row["reported"] = True

    def _end_sub_box(self, agent_id: int, status: str):
        """Close a browser / Android sub-agent's box in the chat, once: Stop and the
        run's watcher can both get here."""
        with self._cli_agent_lock:
            if agent_id in self._sub_ended:
                return
            self._sub_ended.add(agent_id)
        self._safe_cli_emit("sub_end", agent_id, status)

    def _relay_sub_agent(self, agent_id: int, live: dict):
        """One pass of a browser / Android run's live view to its box in the chat: the
        tool-flow events and the newest screen its child wrote to the run's
        folder (events.jsonl and frame.jpg, see browser_agent_child.py), and
        the new lines of its notes. Any read may fail: stop() and outcome()
        delete folders from other threads."""
        if self.cli_callback is None or agent_id in self._sub_ended:
            return   # no app to show it, or its box is already closed
        for line in _read_new_lines(live["dir"] / "events.jsonl", live, "events_at"):
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if not isinstance(row, dict):
                continue
            if row.get("k") == "flow":
                self._safe_cli_emit("sub_flow", agent_id, str(row.get("ev") or ""), row.get("p") or "")
            elif row.get("k") == "frame":
                live["frame_due"] = True
        if live.get("frame_due"):
            # Only the newest screen: the child swaps frame.jpg in whole each time
            try:
                frame = base64.b64encode((live["dir"] / "frame.jpg").read_bytes()).decode("ascii")
            except OSError:
                frame = ""   # tried again on the next pass
            if frame:
                live["frame_due"] = False
                self._safe_cli_emit("sub_frame", agent_id, frame)
        try:
            fresh = live["notes"].stat().st_mtime >= live["since"]
        except OSError:
            fresh = False
        if fresh:
            for line in _read_new_lines(live["notes"], live, "notes_at"):
                self._safe_cli_emit("sub_note", agent_id, line)

    def _watch_browser_agent(self, agent_id: int, run: dict):
        """Watcher thread: shows one browser agent live in its box in the chat,
        about three times a second, then reports it"""
        from .tool import browser_agent
        live = {"dir": run["result_file"].parent, "since": 0,
                "notes": browser_agent._WEB_SCRATCHPAD / run["session_id"] / "milestone" / "milestone.md"}
        while True:
            exited = run["subprocess"].poll() is not None
            self._relay_sub_agent(agent_id, live)
            # outcome() clears the run's notes once the child is gone, so it is
            # asked only after a pass that came after the exit, or once time is up
            if exited or time.monotonic() > run["deadline"]:
                outcome = browser_agent.outcome(run)
                if outcome is not None:
                    break
            time.sleep(0.3)
        summary, status = outcome
        # Its box closes however the run ended, also when Stop or the end of
        # the request already cleared the list below
        self._end_sub_box(agent_id, status)
        with self._cli_agent_lock:
            task = next((t for t in self._browser_tasks if t["id"] == agent_id), None)
            if task is None:
                return   # the run was stopped and its list cleared: nobody is left to tell
            self._browser_tasks = [t for t in self._browser_tasks if t["id"] != agent_id]
            self._report_sub_agent(agent_id, "browser_agent", task["task"], summary, status)
            logger.info(f"Browser Agent {agent_id} finished: {status}")

    def _watch_android_agent(self, agent_id: int, run: dict):
        """Watcher thread: shows one android_agent run live in its box in the
        chat, about three times a second, then reports it. A phone that goes
        away mid-run ends the run at once."""
        from .tool import android_agent
        # Its notes are the Android agent's one shared scratchpad, wiped when
        # the child starts: a file older than this run is an earlier run's
        live = {"dir": run["result_file"].parent, "since": run["started"],
                "notes": android_agent._ANDROID_SCRATCHPAD / "milestone" / "milestone.md"}
        while True:
            # Stop reaches the phone at once, not only when the run unwinds
            # (the main agent may be parked in a model call for a while).
            if self.stop_event and self.stop_event.is_set() and not run.get("stopped"):
                android_agent.stop(run)
            self._relay_sub_agent(agent_id, live)
            outcome = android_agent.outcome(run, phone_online=(self.android_state() == "online"))
            if outcome is not None:
                break
            time.sleep(0.3)
        # The child is gone: the last of what it wrote (outcome() keeps the notes)
        self._relay_sub_agent(agent_id, live)
        summary, status = outcome
        # Its box closes however the run ended, also when Stop or the end of
        # the request already cleared the list below
        self._end_sub_box(agent_id, status)
        with self._cli_agent_lock:
            task = next((t for t in self._android_tasks if t["id"] == agent_id), None)
            if task is None:
                return   # the run was stopped and its list cleared: nobody is left to tell
            self._android_tasks = [t for t in self._android_tasks if t["id"] != agent_id]
            self._report_sub_agent(agent_id, "android_agent", task["task"], summary, status)
            logger.info(f"Android Agent {agent_id} finished: {status}")

    def android_state(self) -> str:
        """android_agent's line in <sub_agents>: online, checking or offline."""
        try:
            return self.android.state() if self.android is not None else "offline"
        except Exception:
            return "offline"

    def release_android(self):
        """End of the main agent's request: android_agent runs stop and the
        phone check is let go."""
        with self._cli_agent_lock:
            android_runs = [t["run"] for t in self._android_tasks]
            self._android_tasks.clear()
        link, self.android = self.android, None
        if android_runs:
            from .tool import android_agent
            for run in android_runs:
                android_agent.stop(run)
        if link is not None:
            link.release()

    def slot_ids(self) -> dict:
        """Every sub-agent type's slot ids, in id order. They are numbered on
        across the types: coder_agent 1-4, browser_agent 5-8, then the
        phone, 9."""
        for agent in PHONE_AGENTS:
            if self.android_state() == "online" and agent not in self._phones_online:
                self._phones_online.append(agent)
        phones = self._phones_online + [a for a in PHONE_AGENTS if a not in self._phones_online]
        ids, first = {}, 1
        for agent in [a for a in SUB_AGENT_TYPES if a not in PHONE_AGENTS] + phones:
            ids[agent] = list(range(first, first + SUB_AGENT_SLOTS[agent]))
            first += SUB_AGENT_SLOTS[agent]
        return ids

    def _free_slot(self, agent: str):
        """The lowest id of `agent` no row holds, or None when all are busy.
        Asked BEFORE a sub-agent starts, so a busy type starts nothing."""
        ids = self.slot_ids().get(agent, [])
        with self._cli_agent_lock:
            held = {r["slot"] for r in self._sub_agents if r["agent"] == agent and not r["seen"]}
        return next((n for n in ids if n not in held), None)

    def _slots_busy(self, agent: str) -> dict:
        """The sub_agent answer when every slot of `agent` is busy: nothing started."""
        ids = self.slot_ids().get(agent) or [0]
        message = (f"Not started: all {len(ids)} {agent} slots (ids {ids[0]}-{ids[-1]}) are busy. "
                   f"Wait for one of them to finish (agent_wait), then start this one." if len(ids) > 1 else
                   f"Not started: {agent} id {ids[0]} is busy. Wait for it to finish (agent_wait), "
                   f"then start this one.")
        return {"status": "error", "action": "tool", "tool": "sub_agent", "agent_type": agent,
                "message": message}

    def _register_sub_agent(self, agent: str, task: str, slot: int) -> int:
        """Add a started sub-agent to the registry in `slot` and return its hidden id"""
        with self._cli_agent_lock:
            self.sub_agent_last_id += 1
            self._sub_agents.append({"id": self.sub_agent_last_id, "agent": agent, "slot": slot,
                                     "task": task, "status": "pending", "reported": False, "seen": False})
            return self.sub_agent_last_id

    def get_sub_agents(self) -> list:
        """Rows of the sub-agent registry, for the main agent's <sub_agents> block"""
        with self._cli_agent_lock:
            return [dict(row) for row in self._sub_agents]

    def take_sub_agent_reports(self) -> list:
        """Hand over the finished reports the main agent has not seen yet, and
        forget them. Their slots are free from now on."""
        with self._cli_agent_lock:
            reports = list(self._cli_completed)
            self._cli_completed.clear()
            for row in self._sub_agents:
                if row.get("reported"):
                    row["seen"] = True
            return reports

    def stop_cli_agent(self):
        """Terminate all CLI agent subprocesses if running"""
        terminated_ids = []
        with self._cli_agent_lock:
            for task_entry in self._cli_tasks:
                proc = task_entry.get("subprocess")
                if proc and proc.poll() is None:
                    try:
                        proc.terminate()
                        logger.info(f"CLI Agent subprocess terminated: {task_entry.get('task', 'unknown')}")
                    except Exception as e:
                        logger.error(f"Error terminating CLI subprocess: {e}")
                rf = task_entry.get("result_file")
                if rf is not None:
                    terminated_ids.append(rf.stem)
            self._cli_tasks.clear()
            self._cli_completed.clear()
            # Every slot is free again: the runs go down below
            for row in self._sub_agents:
                if row["status"] == "pending":
                    row["status"] = "stopped"
                row["seen"] = True
            box_ids = [t["id"] for t in self._browser_tasks + self._android_tasks]
            browser_runs = [t["run"] for t in self._browser_tasks]
            self._browser_tasks.clear()
            android_runs = [t["run"] for t in self._android_tasks]
            self._android_tasks.clear()

        # Their boxes in the chat close at once: a child can take seconds to stop
        for agent_id in box_ids:
            self._end_sub_box(agent_id, "stopped")

        # Browser agents go down with the run; the browser itself stays up
        if browser_runs:
            from .tool import browser_agent
            for run in browser_runs:
                browser_agent.stop(run)
        # So do android_agent runs (their helper goes with the child)
        if android_runs:
            from .tool import android_agent
            for run in android_runs:
                android_agent.stop(run)

        # Notify frontend that any pills for these tasks are done (stopped)
        for task_id in terminated_ids:
            self._safe_cli_emit("task_end", task_id, "stopped", "Stopped by user")
        if self._cli_await_active:
            self._safe_cli_emit("await_end")
            self._cli_await_active = False
        
    def route_action(self, action_data):
        """
        Route actions to appropriate service based on action type
        
        Format: [{"type": "click", "id": 8}, {"type": "wait", "value": "2"}, ...]
        
        Args:
            action_data (list): The action list from LLM response
            
        Returns:
            dict: Result of the action execution
        """
        try:
            results = []
            skip_reason = None   # set by agent_wait: the rest of the turn is not run

            for action_item in action_data:
                # Check stop before every action
                if self.stop_event and self.stop_event.is_set():
                    self.controller_service.release_all_inputs()
                    return {"status": "stopped", "action": "stop", "message": "Stopped by user"}

                if skip_reason:
                    results.append({"status": "skipped", "action": action_item.get("type"),
                                    "message": skip_reason})
                    continue
                
                action_type = action_item.get("type")
                
                if not action_type:
                    logger.warning(f"Action item missing 'type' field: {action_item}")
                    continue
                
                if action_type == "left_click":
                    element_id = action_item.get("id")
                    clicks = action_item.get("clicks", 1)
                    if clicks == 3:
                        result = self.controller_service.triple_click(element_id)
                    elif clicks == 2:
                        result = self.controller_service.double_click(element_id)
                    else:
                        result = self.controller_service.click(element_id)
                    results.append(result)
                    if result.get("status") == "error":
                        return result
                
                elif action_type == "input":
                    element_id = action_item.get("id")
                    text_value = action_item.get("value")
                    result = self.controller_service.input(element_id, text_value)
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
                    direction = action_item.get("direction")
                    result = self.controller_service.scroll(element_id, direction)
                    results.append(result)
                    if result.get("status") == "error":
                        return {
                            "status": "error",
                            "action": "scroll",
                            "results": results,
                            "message": f"Scroll on element {element_id} failed"
                        }
                
                elif action_type == "typewrite":
                    text_value = action_item.get("value")
                    result = self.controller_service.typewrite(text_value)
                    results.append(result)
                    if result.get("status") == "error":
                        return result
                        
                        
                elif action_type == "right_click":
                    element_id = action_item.get("id")
                    result = self.controller_service.right_click(element_id)
                    results.append(result)
                    if result.get("status") == "error":
                        return result

                elif action_type == "hotkey":
                    combo_value = action_item.get("value")
                    result = self.hotkey_service.send(combo_value)
                    results.append(result)
                    if result.get("status") == "error":
                        return result
                        
                elif action_type == "screenshot":
                    element_id = str(action_item.get("id"))
                    logger.info(f"Taking screenshot of element: {element_id}")
                    
                    if element_id not in self.controller_service.elements_mapping:
                        return {
                            "status": "error",
                            "action": "screenshot",
                            "index": element_id,
                            "message": f"Element index {element_id} not found"
                        }
                    
                    element_info = self.controller_service.elements_mapping[element_id]
                    rect = element_info.get('visible_rect') or element_info['rect']
                    
                    result = self.screenshot_service.capture_element(rect, element_id)
                    result["index"] = element_id
                    results.append(result)
                    if result.get("status") == "error":
                        return result

                elif action_type == "open_app":
                    app_name = action_item.get("value")
                    logger.info(f"Opening application: {app_name}")
                    
                    success = open_app(app_name)
                    
                    if success:
                        logger.info(f"Successfully opened {app_name}")
                        result = {"status": "success", "action": "tool", "tool": "open_app", "app": app_name}
                        results.append(result)
                    else:
                        logger.error(f"Failed to open {app_name}")
                        return {
                            "status": "error",
                            "action": "tool",
                            "tool": "open_app",
                            "app": app_name,
                            "message": "No application found. Verify by typing the application name in the taskbar search. If still not visible then download it or find an alternative."
                        }
                
                elif action_type == "wait":
                    wait_time = float(action_item.get("value", "1"))
                    logger.info(f"Waiting for {wait_time} seconds...")
                    elapsed = 0.0
                    while elapsed < wait_time:
                        if self.stop_event and self.stop_event.is_set():
                            self.controller_service.release_all_inputs()
                            return {"status": "stopped", "action": "stop", "message": "Stopped by user"}
                        time.sleep(min(0.5, wait_time - elapsed))
                        elapsed += 0.5
                    logger.info(f"Wait completed")
                    result = {"status": "success", "action": "tool", "tool": "wait", "duration": wait_time}
                    results.append(result)
                        
                elif action_type == "scan":
                    # Set here, as the call runs: a later action of the batch
                    # failing cannot lose it, and the last scan call wins.
                    value = str(action_item.get("value", "")).strip().lower()
                    if value not in ("on", "off"):
                        results.append({"status": "error", "action": "tool", "tool": "scan",
                                        "message": 'scan takes "on" or "off".'})
                        continue
                    if self.scan_on == (value == "on"):
                        message = f"Scan is already {value}."
                    elif value == "off":
                        message = ("Scan off: from the next step no screenshot, no <element_tree> "
                                   "and no screen tools, until you call scan on.")
                    else:
                        message = "Scan on: the screenshot, the <element_tree> and every tool come back from the next step."
                    self.scan_on = value == "on"
                    results.append({"status": "success", "action": "tool", "tool": "scan",
                                    "value": value, "message": message})

                elif action_type == "web":
                    query = action_item.get("value")
                    logger.info(f"Performing web search: {query}")

                    if self.web_callback:
                        self.web_callback("start")

                    self._stop_loading = False
                    loading_thread = threading.Thread(target=self._web_loading_animation)
                    loading_thread.daemon = True
                    loading_thread.start()

                    try:
                        web_service = WebService(self.provider, self.model, self.api_key,
                                                 stop_event=self.stop_event)
                        web_result = web_service.search(query)
                    finally:
                        self._stop_loading = True
                        loading_thread.join(timeout=1)
                        # Clear the in-place text only in TTY mode — in piped subprocess
                        # this would just dump 50 spaces + carriage returns into the
                        # parent's reader buffer.
                        if sys.stdout.isatty():
                            sys.stdout.write("\r" + " " * 50 + "\r")
                            sys.stdout.flush()

                        if self.web_callback:
                            self.web_callback("end")
                            # Wait for CSS fade-out to complete before next action
                            time.sleep(0.7)

                    # The browser-agent fallback can run for minutes and honours
                    # stop by returning early — take the normal stopped path.
                    if self.stop_event and self.stop_event.is_set():
                        self.controller_service.release_all_inputs()
                        return {"status": "stopped", "action": "stop", "message": "Stopped by user"}

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

                elif action_type == "agent_wait":
                    # AWAIT MODE: hold the pipeline for ONE sub-agent, by its slot. The
                    # hold also ends when another sub-agent finishes first, so a report
                    # is never kept back; the main agent then holds again if it needs to.
                    agent_type = action_item.get("agent_type", "coder_agent")
                    try:
                        agent_id = int(action_item.get("agent_id", 0))
                    except (TypeError, ValueError):
                        agent_id = 0
                    reason = action_item.get("value", "")
                    logger.info(f"Agent wait triggered for {agent_type} {agent_id}: {reason}")

                    ids = self.slot_ids()
                    # The run in that slot now, until its report is handed over
                    row = next((r for r in self.get_sub_agents() if r["agent"] == agent_type
                                and r["slot"] == agent_id and not r["seen"]), None)
                    if agent_id not in ids.get(agent_type, []):
                        own = ids.get(agent_type)
                        if not own:
                            message = f"Unknown agent_type '{agent_type}'. Use one of: {', '.join(SUB_AGENT_TYPES)}."
                        else:
                            message = (f"{agent_type} has ids {own[0]}-{own[-1]}." if len(own) > 1 else
                                       f"{agent_type} has id {own[0]}.")
                            other = next((t for t, n in ids.items() if agent_id in n), None)
                            if other:
                                message += f" Id {agent_id} is {other}."
                        results.append({
                            "status": "error",
                            "action": "tool",
                            "tool": "agent_wait",
                            "message": message
                        })
                        skip_reason = "Not run: the agent_wait before it failed, so nothing was held for."
                    elif row is None:
                        results.append({
                            "status": "success",
                            "action": "agent_wait",
                            "agent_type": agent_type,
                            "agent_id": agent_id,
                            "reason": reason,
                            "completed": [],
                            "message": f"No {agent_type} run holds id {agent_id}: nothing to wait for."
                        })
                    else:
                        run_key = row["id"]   # the hidden id of the run in that slot
                        # Notify frontend only if a coder is in flight (its card lives in
                        # the streaming UI), so any other hold doesn't flash it.
                        if row["status"] == "pending" and agent_type == "coder_agent" and not self._cli_await_active:
                            self._cli_await_active = True
                            self._safe_cli_emit("await_start", reason)

                        # Hold until that sub-agent is done, or any report is ready
                        while True:
                            if self.stop_event and self.stop_event.is_set():
                                self.stop_cli_agent()
                                self.controller_service.release_all_inputs()
                                return {"status": "stopped", "action": "stop", "message": "Stopped by user"}
                            with self._cli_agent_lock:
                                still_running = any(r["id"] == run_key and r["status"] == "pending"
                                                    for r in self._sub_agents)
                                if not still_running or len(self._cli_completed) > 0:
                                    break
                            time.sleep(1)

                        # The reports stay queued: the main driver takes them for the
                        # report step that follows. This is a copy for the tool result.
                        with self._cli_agent_lock:
                            completed = list(self._cli_completed)

                        logger.info(f"Agent wait over: {len(completed)} report(s) ready")

                        self._end_await_if_idle()

                        result = {
                            "status": "success",
                            "action": "agent_wait",
                            "agent_type": agent_type,
                            "agent_id": agent_id,
                            "reason": reason,
                            "completed": completed
                        }
                        if still_running:
                            result["message"] = (f"{agent_type} id {agent_id} is still running; another sub-agent "
                                                 f"finished first. Call agent_wait again to keep holding for it.")
                            skip_reason = (f"Not run: the hold ended before {agent_type} id {agent_id} "
                                           f"finished. Call it again once that sub-agent is done.")
                        elif not completed:
                            result["message"] = (f"{agent_type} id {agent_id} already finished ({row['status']}) "
                                                 f"and its report was delivered earlier.")
                        results.append(result)

                elif action_type == "sub_agent" and action_item.get("agent_type", "coder_agent") not in SUB_AGENT_TYPES:
                    results.append({
                        "status": "error",
                        "action": "tool",
                        "tool": "sub_agent",
                        "message": (f"Unknown agent_type '{action_item.get('agent_type')}'. "
                                    f"Use one of: {', '.join(SUB_AGENT_TYPES)}.")
                    })

                elif action_type == "sub_agent" and action_item.get("agent_type") == "browser_agent":
                    # DISPATCH MODE: start a browser agent in the first free slot
                    # (each one works in a tab of its own, browser_agent_child.py)
                    task_description = action_item.get("value", "")
                    slot = self._free_slot("browser_agent")
                    from .tool import browser_agent
                    run = None
                    if slot is None:
                        results.append(self._slots_busy("browser_agent"))
                    else:
                        logger.info(f"Starting Browser Agent {slot} for task: {task_description}")
                        try:
                            run = browser_agent.start(task_description, self.provider, self.model,
                                                      api_key=self.api_key, speed=self.speed,
                                                      stop_event=self.stop_event)
                        except browser_agent.BrowserAgentError as e:
                            results.append({
                                "status": "error",
                                "action": "tool",
                                "tool": "sub_agent",
                                "agent_type": "browser_agent",
                                "message": str(e)
                            })
                    if run is not None:
                        agent_id = self._register_sub_agent("browser_agent", task_description, slot)
                        # Its own box in the chat, under the main agent's
                        self._safe_cli_emit("sub_start", agent_id, "browser", task_description)
                        with self._cli_agent_lock:
                            self._browser_tasks.append({"id": agent_id, "task": task_description, "run": run})
                        threading.Thread(target=self._watch_browser_agent, args=(agent_id, run),
                                         daemon=True).start()
                        results.append({
                            "status": "success",
                            "action": "tool",
                            "tool": "sub_agent",
                            "agent_type": "browser_agent",
                            "id": slot,
                            "task": task_description,
                            "message": f"Assigned browser_agent id {slot}. It runs in parallel: continue with other tasks."
                        })

                elif action_type == "sub_agent" and action_item.get("agent_type") == "android_agent":
                    # DISPATCH MODE: start the Android agent on the checked phone.
                    # Not online yet (still connecting) or no phone: say so and start
                    # nothing; its one slot busy: the same.
                    task_description = action_item.get("value", "")
                    android_state = self.android_state()
                    slot = self._free_slot("android_agent") if android_state == "online" else None
                    if android_state == "online" and slot is None:
                        results.append(self._slots_busy("android_agent"))
                    elif android_state != "online":
                        if android_state == "checking":
                            message = ("android_agent is still connecting to the phone. Carry on with other "
                                       "work: start it once its slot in <sub_agents> reads empty.")
                        else:
                            reason = ((getattr(self.android, "reason", "") or "no Android phone is connected")
                                      if self.daisy_chain else "phones are only reachable in Daisy chain mode")
                            message = f"android_agent is currently offline: {reason}."
                        results.append({
                            "status": "error",
                            "action": "tool",
                            "tool": "sub_agent",
                            "agent_type": "android_agent",
                            "message": message
                        })
                    else:
                        logger.info(f"Starting Android Agent for task: {task_description}")
                        from .tool import android_agent
                        try:
                            run = android_agent.start(task_description, self.provider, self.model,
                                                      api_key=self.api_key, speed=self.speed,
                                                      link=self.android)
                        except android_agent.AndroidAgentError as e:
                            results.append({
                                "status": "error",
                                "action": "tool",
                                "tool": "sub_agent",
                                "agent_type": "android_agent",
                                "message": str(e)
                            })
                        else:
                            agent_id = self._register_sub_agent("android_agent", task_description, slot)
                            # Its own box in the chat, under the main agent's
                            self._safe_cli_emit("sub_start", agent_id, "android", task_description)
                            with self._cli_agent_lock:
                                self._android_tasks.append({"id": agent_id, "task": task_description, "run": run})
                            threading.Thread(target=self._watch_android_agent, args=(agent_id, run),
                                             daemon=True).start()
                            results.append({
                                "status": "success",
                                "action": "tool",
                                "tool": "sub_agent",
                                "agent_type": "android_agent",
                                "id": slot,
                                "task": task_description,
                                "message": (f"Assigned android_agent id {slot} on {self.android.name}. It runs "
                                            f"in parallel: continue with other tasks.")
                            })

                elif action_type == "sub_agent":
                    # DISPATCH MODE: spawn CLI agent, in the first free coder_agent slot
                    task_description = action_item.get("value", "")
                    slot = self._free_slot("coder_agent")
                    if slot is None:
                        results.append(self._slots_busy("coder_agent"))
                        continue
                    logger.info(f"Starting CLI Agent {slot} for task: {task_description}")

                    # Unique even for four coders started in one batch
                    result_file = (app_data_dir() / "cli_agent_result"
                                   / f"result_{int(time.time() * 1000)}_{uuid.uuid4().hex[:6]}.json")
                    result_file.parent.mkdir(parents=True, exist_ok=True)
                    
                    # The Rust coder's child entry (AutoCua/coder/__main__.py)
                    cli_cmd = [
                        sys.executable, "-m", "AutoCua.coder",
                        "--task", task_description,
                        "--provider", self.provider,
                        "--model", self.model,
                        "--result", str(result_file)
                    ]
                    
                    if self.api_key:
                        cli_cmd.extend(["--api_key", self.api_key])

                    # Shell use: this coder's token usage for the memory bar (the
                    # Rust coder takes the flag but does not use it yet). Never
                    # set for main-agent dispatches (this view's flag is False there).
                    if self.cli_report_usage:
                        cli_cmd.append("--report-usage")

                    # Shell-use conversation: which request of the chat this is.
                    # Omitted -> 1.
                    if self.cli_request_no and int(self.cli_request_no) > 1:
                        cli_cmd.extend(["--request-no", str(int(self.cli_request_no))])

                    # Shell-use conversation: the chat's history file. Absent for
                    # main-agent dispatches.
                    if self.cli_history_file:
                        cli_cmd.extend(["--history", str(self.cli_history_file)])

                    # Propagate external_terminal to the CLI subprocess. In app.py UI mode
                    # main agent has external_terminal=False; pass --no_external_terminal so
                    # the coder keeps its own subagents on PIPE, not in terminal windows.
                    if not self.external_terminal:
                        cli_cmd.append("--no_external_terminal")

                    cli_env = os.environ.copy()
                    cli_env["PYTHONUNBUFFERED"] = "1"
                    cli_proc = None

                    if self.external_terminal:
                        # Headless main.py mode: open a terminal window so the user
                        # can watch the sub-agent live. Completion still flows
                        # through the result-file watcher below.
                        self._spawn_cli_external_terminal(cli_cmd, task_description)
                    else:
                        # Start subprocess. Force unbuffered stdout/stderr in the
                        # child so our reader thread sees lines as they're printed
                        # — without this, Python block-buffers when not connected
                        # to a TTY and the streaming UI feels frozen.
                        try:
                            cli_proc = subprocess.Popen(
                                cli_cmd,
                                stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE,
                                env=cli_env,
                                # OWN SESSION (setsid). Stop kills the coder by process
                                # GROUP — ui/service.py _kill_process_trees does
                                # os.killpg(os.getpgid(pid), SIGKILL) — so that the
                                # subagents the coder spawned go down with it. Without
                                # this the coder inherits OUR group, getpgid() resolves
                                # to the app's own pgid, and pressing Stop SIGKILLs the
                                # whole application instead of the agent.
                                start_new_session=True,
                            )
                            debug_log(f"CLI subprocess started (PID: {cli_proc.pid})")
                        except Exception as e:
                            debug_log(f"CLI subprocess failed to start: {e}", "ERROR")
                            # Never registered: no report would ever come
                            results.append({
                                "status": "error",
                                "action": "tool",
                                "tool": "sub_agent",
                                "agent_type": "coder_agent",
                                "message": f"Coder Agent failed to start: {e}"
                            })
                            continue

                    # Track in active list, and as a row of <sub_agents>
                    task_id = result_file.stem
                    agent_id = self._register_sub_agent("coder_agent", task_description, slot)
                    with self._cli_agent_lock:
                        self._cli_tasks.append({
                            "id": agent_id,
                            "task": task_description,
                            "subprocess": cli_proc,
                            "result_file": result_file
                        })

                    # Notify frontend a new task has started, then begin
                    # streaming its stdout/stderr line-by-line. Its sub-agent
                    # id keys its box in the chat (Computer use).
                    self._safe_cli_emit("task_start", task_id, task_description, agent_id)
                    if cli_proc is not None:
                        for pipe, stream_name in (
                            (cli_proc.stdout, "out"),
                            (cli_proc.stderr, "err"),
                        ):
                            t = threading.Thread(
                                target=self._read_cli_stream,
                                args=(pipe, task_id, stream_name),
                                daemon=True,
                            )
                            t.start()
                    
                    # Watcher thread (bind result_file via default arg to avoid closure issue)
                    def watch_cli_result(rf=result_file, proc=cli_proc, task=task_description):
                        while True:
                            if rf.exists():
                                try:
                                    with open(rf, 'r', encoding='utf-8') as f:
                                        result_data = json.load(f)
                                    rf.unlink()
                                    self._cli_agent_complete_callback(result_data, rf)
                                except Exception as e:
                                    logger.error(f"Error reading CLI agent result: {e}")
                                break
                            # The coder exited without writing its result (it
                            # crashed): report it, or done and agent_wait would
                            # hold until Stop. Stop kills it on purpose and
                            # reports it itself, so that exit is not a crash.
                            if proc is not None and proc.poll() is not None and not rf.exists():
                                with self._cli_agent_lock:
                                    tracked = any(t["result_file"] == rf for t in self._cli_tasks)
                                if tracked and not (self.stop_event and self.stop_event.is_set()):
                                    self._cli_agent_complete_callback(
                                        {"task": task, "status": "error",
                                         "summary": f"Coder Agent exited with code {proc.returncode} without a report."},
                                        rf)
                                break
                            time.sleep(2)
                    
                    watcher_thread = threading.Thread(target=watch_cli_result)
                    watcher_thread.daemon = True
                    watcher_thread.start()
                    
                    result = {
                        "status": "success",
                        "action": "tool",
                        "tool": "sub_agent",
                        "agent_type": "coder_agent",
                        "id": slot,
                        "task": task_description,
                        "message": f"Assigned coder_agent id {slot}. It runs in parallel: continue with other tasks."
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
                        
                elif action_type == "app_script":
                    app_name = action_item.get("app", "")
                    script_value = action_item.get("value", "")
                    logger.info(f"Executing app script on {app_name}: {script_value[:80]}")

                    # Signal frontend: terminal card appears with "App script" label
                    if self.shell_callback:
                        display_cmd = f"{app_name}: {script_value[:80]}" if app_name else script_value[:80]
                        self.shell_callback("start", display_cmd, "App script")

                    result = self.app_script_service.execute(app_name, script_value)

                    # Signal frontend: show success/fail result
                    if self.shell_callback:
                        as_status = result.get("status", "error")
                        as_output = result.get("output", result.get("message", ""))
                        self.shell_callback("result", {"status": as_status, "output": as_output or ""})
                        time.sleep(2)
                        self.shell_callback("end")
                        time.sleep(0.7)

                    results.append(result)
                    if result.get("status") == "error":
                        return result

                elif action_type == "done":
                    summary = action_item.get("value")
                    
                    # Block if any CLI agents are still pending
                    with self._cli_agent_lock:
                        if len(self._cli_tasks) > 0:
                            logger.info(f"Done requested but {len(self._cli_tasks)} CLI Agent(s) still pending. Waiting...")
                    
                    # Wait for all CLI agents to complete (blocking)
                    while True:
                        if self.stop_event and self.stop_event.is_set():
                            self.stop_cli_agent()
                            self.controller_service.release_all_inputs()
                            return {"status": "stopped", "action": "stop", "message": "Stopped by user"}
                        with self._cli_agent_lock:
                            if len(self._cli_tasks) == 0 and len(self._browser_tasks) == 0 \
                                    and len(self._android_tasks) == 0:
                                break
                        time.sleep(1)
                    
                    logger.info(f"Task Complete: {summary}")
                    return {"status": "success", "action": "done", "summary": summary}

                # Shell command execution (new flat format)
                elif action_type == "shell":
                    # Main agent mode: value field only, with frontend animation
                    command = action_item.get("value", "")
                    
                    # Signal frontend: terminal card appears, screenshot slides down
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
                        # Signal frontend: terminal card fades out, screenshot slides up
                        self.shell_callback("end")
                        # Wait for CSS fade-out to complete before next action
                        time.sleep(0.7)
                    
                    results.append(result)
                    if result.get("status") == "error" and result.get("error"):
                        return result
                
                # Check if last action was stopped
                if results and results[-1].get("status") == "stopped":
                    self.controller_service.release_all_inputs()
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
        self.controller_service.set_elements(elements_mapping, application_name)
