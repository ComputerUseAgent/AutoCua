import os
import json
import re
import time
import shutil
from pathlib import Path
from datetime import datetime
from typing import Optional
from ...tool_registry.service import (LLMManager, tool_calls_to_steps, drop_bundled_terminal,
                                         MAIN_TOOL_NAMES, MAIN_TOOL_NAMES_FAST, MAIN_TOOL_NAMES_CLAUDE,
                                         MAIN_ACTION_DEFAULTS, MAIN_ACTION_DEFAULTS_FAST,
                                         MAIN_ACTION_DEFAULTS_CLAUDE,
                                         MAIN_REASONING_FIELDS, MAIN_NOTES_FIELDS_FAST,
                                         CLAUDE_REASONING_FIELDS, NOTE_TOOLS)
from ....memory_compression.controller import CompressionController
from .view import (AgentResponseFormatter, decode_step, encode_step,
                   decode_results, encode_results, wire_calls_from,
                   snapshot_turn, compression_dump, compression_entry,
                   _looks_native, SKIPPED_THINKING, REASONING_RESULT)
from ...tree.element import UIElementScanner, ELEMENT_CONFIG
from ..skills import DomainKnowledgeService
from ...controller import ControllerView

# Run-boundary request markers: on resume, the bridge entry that ended the prior
# run ("Previous run concluded.", saved by agent_conversation._build_history with
# an empty tool slot) gets its slot filled with the NEW run's request, so the
# persisted memory attributes every run's steps to the request that drove them.
# The sentence rides in the bridge's next_goal (prefix match — it continues).
_BRIDGE_SIGNATURE = '"next_goal": "Previous run concluded.'
_REQUEST_MARKER_PREFIX = "<updated_user_request"

def _request_marker(n: int, task: str) -> str:
    return f'<updated_user_request no="{n}">\n{task}\n</updated_user_request no="{n}">'

def _cleanup_scratchpad():
    """Clear all contents inside AutoCua/ios/scratchpad/ for a fresh start.

    Parallel simulator tasks set AutoCua_IOS_SESSION and own only their own
    scratchpad/<session>/ subtree — wiping the whole folder there would delete
    a sibling task's notes mid-run."""
    # Clear scratchpad contents
    scratchpad_dir = Path(__file__).parent.parent.parent / "scratchpad"
    session = os.environ.get("AutoCua_IOS_SESSION") or ""
    if session:
        scratchpad_dir = scratchpad_dir / session
    if scratchpad_dir.exists():
        for item in scratchpad_dir.iterdir():
            if item.is_dir():
                shutil.rmtree(item)
            else:
                item.unlink()
    else:
        scratchpad_dir.mkdir(parents=True, exist_ok=True)

    # Also clear the todo folder (on iOS it lives at ios/todo, not under
    # scratchpad/ like macOS, so it needs its own wipe for the same fresh start)
    todo_dir = Path(__file__).parent.parent.parent / "todo"
    if todo_dir.exists() and todo_dir.is_dir():
        shutil.rmtree(todo_dir)


# Appended to the system prompt for an `async_tools` model (GPT-6 Astra, see
# llm_provider/responses.py): its tool calls run after the turn ends, and the
# block tells it what that changes about the rules above it.
ASYNC_TOOLS_ADDENDUM = """
<async_tools>
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's input. The rules above are unchanged: one notes call first, then every action whose target is on the current screen, stopping where the screen must change. Because nothing comes back mid-turn:
- Never write a text message in a turn that contains tool calls, and never state that an action succeeded or failed: that verdict belongs to the next step's memory, judged on the next screenshot.
- `done` stays the only call of its turn. Never add it after this turn's actions: their results have not arrived, so nothing is verified yet.
</async_tools>
"""

# Appended to the live user message for a Claude model (quality mode). Claude
# treats a notes tool as a think step: it calls `agent_memory`, ends its turn
# to wait for the result, and acts on the NEXT turn - one call per turn and a
# wasted screen scan per action. Saying in the live message that the notes
# never end a turn is what puts the notes and the actions in one message.
# Measured 2026-10-05 on step 1 of "open calculator and compute 23*47"
# (Sonnet 5 via OpenRouter): notes+action together 0/5 before, 6/6 with this
# reminder plus the matching <blocks> rule in claude_prompt.md and the
# agent_memory description (tool_registry, `batched=True`). Anthropic's own
# guidance: a sentence at the end of the current request moves batching far
# more than the same text in the system prompt, so it rides here, every step,
# on the live message only (never persisted, so the cached prefix is unchanged).
CLAUDE_BATCH_REMINDER = (
    "\n\n<critical>This turn must carry the `agent_memory` call AND the action calls that do its "
    "\"Doing\" in this one response. `agent_memory` alone is not a step: it has no result, "
    "so never end the turn after it.</critical>")


class AgentService:
    """Service for iOS automation agent"""

    # cli_callback is accepted for GUI parity with the desktop agents (the web UI
    # always passes it); iOS has no CLI agent, so it is ignored.
    def __init__(self, provider: str, model: str, save_conversation: bool = False, frontend_callback=None, text_callback=None, web_callback=None, shell_callback=None, cli_callback=None, tool_callback=None, token_callback=None, api_key: str = None, stop_event=None, prior_history: Optional[dict] = None, speed: str = "quality"):
        """Initialize the Agent Service"""
        # Clean up scratchpad for a fresh start
        _cleanup_scratchpad()

        # Ensure sandbox workspace exists on Desktop
        self.sandbox_root = Path.home() / "Desktop" / "sandbox_workspace"
        if not self.sandbox_root.exists():
            self.sandbox_root.mkdir(parents=True, exist_ok=True)

        # Speed mode: "fast" swaps in fast_system_prompt.md and trims the
        # tracking params the driver's tools carry.
        self.speed = "fast" if str(speed).lower() == "fast" else "quality"

        # Initialize LLM Manager with optional runtime API key.
        # The driver speaks NATIVE TOOL CALLING - its action tools ARE the
        # output contract (no response_format schema, nothing to parse). It
        # stays cli_agent=False, which is what keeps every provider's
        # screenshot splice (gated on `not cli_agent`) working.
        self.llm_manager = LLMManager(provider, model, api_key, speed=self.speed)
        # Where this mode's notes live: on a dedicated notes tool, called
        # first in every step, while the action tools carry only their own
        # fields. Quality: `reasoning` (thinking optional, memory + next_goal
        # every step). Fast: `memory` (`next_goal` alone; fast_system_prompt.md
        # folds the forward plan and the Expect guard into next_goal). Quality
        # on Claude (any Claude model, any provider): `agent_memory`, the same
        # notes with `assessment` in place of `thinking`, and its own prompt,
        # claude_prompt.md.
        fast = self.speed == "fast"
        self._claude = not fast and self.llm_manager.claude
        if fast:
            self._notes_tool = "memory"
            self._track_params = MAIN_NOTES_FIELDS_FAST
            self._allowed_tools = MAIN_TOOL_NAMES_FAST
            self._action_defaults = MAIN_ACTION_DEFAULTS_FAST
        elif self._claude:
            self._notes_tool = "agent_memory"
            self._track_params = CLAUDE_REASONING_FIELDS
            self._allowed_tools = MAIN_TOOL_NAMES_CLAUDE
            self._action_defaults = MAIN_ACTION_DEFAULTS_CLAUDE
        else:
            self._notes_tool = "reasoning"
            self._track_params = MAIN_REASONING_FIELDS
            self._allowed_tools = MAIN_TOOL_NAMES
            self._action_defaults = MAIN_ACTION_DEFAULTS
        assert self._notes_tool in NOTE_TOOLS

        # Store stop event
        self.stop_event = stop_event

        # Initialize UI Element Scanner with optional frontend callback for image streaming
        self.scanner = UIElementScanner(ELEMENT_CONFIG, frontend_callback=frontend_callback)
        self.skills = DomainKnowledgeService()

        # Store text callback for streaming agent responses to frontend
        self.text_callback = text_callback

        # Store web callback for globe animation
        self.web_callback = web_callback

        # Store shell callback for terminal animation
        self.shell_callback = shell_callback

        # Store tool-flow callback for the bottom "Tool response" chain animation.
        # Signature: tool_callback(event: str, payload: dict|None)
        #   events: run_start | turn{hasImage} | received{tools:[{name,clicks?}]} | run_end
        # The per-turn tools come straight from the parsed action block (this driver),
        # so the controller needs no per-action plumbing.
        self.tool_callback = tool_callback

        # Memory bar: called after each LLM call with the provider's exact token
        # usage (llm_manager.last_usage). The agent only forwards — accumulation +
        # persistence live in app.py / memory_compression.
        self.token_callback = token_callback

        # Initialize Controller — forward the GUI callbacks and stop_event so the
        # web/shell containers animate, web search gets its provider/model, and
        # mid-action stop works (macOS parity; iOS has no cli/external_terminal).
        self.controller = ControllerView(
            provider=provider,
            model=self.llm_manager.get_model_name(),
            web_callback=web_callback,
            shell_callback=shell_callback,
            api_key=api_key,
            stop_event=stop_event,
        )

        # Save conversation flag
        self.save_conversation = save_conversation

        # Load system prompt
        self.system_prompt = self._load_system_prompt()
        # An `async_tools` model (GPT-6 Astra) runs its calls after the turn
        # ends; the block tells it what that changes.
        if self.llm_manager.async_tools():
            self.system_prompt += ASYNC_TOOLS_ADDENDUM

        # Clear previous conversation folder to start fresh
        conversation_folder = Path("conversation")
        if conversation_folder.exists():
            shutil.rmtree(conversation_folder)

        # Clear previous debug folder to start fresh
        debug_folder = Path("debug")
        if debug_folder.exists():
            shutil.rmtree(debug_folder)

        # Clear previous raw_reasoning folder to start fresh
        raw_reasoning_folder = Path("raw_reasoning")
        if raw_reasoning_folder.exists():
            shutil.rmtree(raw_reasoning_folder)

        # Create conversation directory and initialize fresh conversation file
        if self.save_conversation:
            self.conversation_dir = Path("conversation")
            self.conversation_dir.mkdir(exist_ok=True)
            self.conversation_file = self.conversation_dir / "conversation.txt"

            # Always start fresh - each program run is a new session
            self._initialize_conversation_file()

            # Create raw_reasoning directory for storing raw LLM outputs
            self.raw_reasoning_dir = Path("raw_reasoning")
            self.raw_reasoning_dir.mkdir(exist_ok=True)

        # Start fresh each session
        self.interaction_count = 0

        # Resumable chat memory (UI path only). prior_history is an optimized
        # snapshot from a PRIOR run of this session, built + loaded by
        # AutoCua.agent_conversation.service (ALL memory management lives there,
        # not here). When present, process_request seeds the conversation lists
        # from it so the agent "remembers" earlier turns. Scratchpad/todo are
        # still wiped above — continuity comes ONLY from this memory, not files.
        # The final lists are exposed on self so the conversation service can
        # persist them after the run.
        self.prior_history = prior_history if isinstance(prior_history, dict) else None
        self.assistant_messages = []
        self.tool_responses = []
        # The exact final messages payload sent to the model (system + interleaved
        # history + the live user message that re-injects user_request/todo/
        # scratchpad/element_tree each step). Captured for the debug memory log so
        # the download is the TRUE conversation, not a reconstruction.
        self.last_messages = None

        # ── Runtime memory compression (rolling handoff at 110k tokens) ──────
        # The compression agent is a SEPARATE agent: ALL orchestration (trigger,
        # worker thread, indicator, splice policy) lives in its controller,
        # shared by every platform. This loop only calls the four hooks:
        # reset / maybe_trigger / apply_pending / finish_run. LLMManager is
        # passed as a class — the controller lazily builds its own 2nd
        # text-mode manager from it.
        self._compression = CompressionController(
            self.llm_manager, LLMManager, self.token_callback, self.stop_event,
            dump_builder=compression_dump, synthetic_entry=compression_entry)

    def _load_system_prompt(self) -> str:
        """Load the system prompt matching the speed mode (quality/fast), and
        Claude's own prompt for quality mode on any Claude model"""
        prompt_name = ("fast_system_prompt.md" if self.speed == "fast"
                       else "claude_prompt.md" if self._claude
                       else "system_prompt.md")
        try:
            current_dir = os.path.dirname(os.path.abspath(__file__))
            prompt_path = os.path.join(current_dir, prompt_name)

            with open(prompt_path, 'r', encoding='utf-8') as file:
                return file.read()
        except FileNotFoundError:
            raise FileNotFoundError(f"{prompt_name} file not found in the agent directory")
        except Exception as e:
            raise Exception(f"Error loading system prompt: {str(e)}")

    def _initialize_conversation_file(self):
        """Initialize the conversation file with header information"""
        try:
            with open(self.conversation_file, 'w', encoding='utf-8') as f:
                f.write("=== CONVERSATION LOG ===\n")
                f.write(f"Session Started: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n")
                f.write(f"Provider: {self.llm_manager.get_provider_name()}\n")
                f.write(f"Model: {self.llm_manager.get_model_name()}\n")
                f.write("=" * 60 + "\n\n")

                f.write("=== SYSTEM PROMPT ===\n")
                f.write(self.system_prompt)
                f.write("\n\n" + "=" * 60 + "\n\n")
        except Exception as e:
            print(f"Error initializing conversation file: {str(e)}")

    def _save_conversation_snapshot(self, messages: list, current_assistant_response: str, image_sent: bool, interaction_count: int):
        """Save a numbered conversation file rendering the EXACT payload sent this
        step (system + interleaved assistant/user turns + current user message) plus
        this step's freshly generated response — a faithful peek into agent memory."""
        try:
            conversation_file = self.conversation_dir / f"conversation_{interaction_count}.txt"

            def _text(content):
                # Cached messages carry content as a list of {type,text,...} blocks.
                if isinstance(content, list):
                    return "\n".join(b.get("text", "") for b in content if isinstance(b, dict))
                return content if isinstance(content, str) else str(content)

            with open(conversation_file, 'w', encoding='utf-8') as f:
                # Header
                f.write("=== CONVERSATION LOG ===\n")
                f.write(f"Session Started: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}\n")
                f.write(f"Provider: {self.llm_manager.get_provider_name()}\n")
                f.write(f"Model: {self.llm_manager.get_model_name()}\n")
                f.write(f"Current Interaction: #{interaction_count}\n")
                f.write("=" * 60 + "\n\n")

                # Render every message in the exact order it was sent to the model.
                for m in messages:
                    role = str(m.get("role", "?")).upper()
                    if role == "SYSTEM":
                        f.write("=== SYSTEM PROMPT ===\n")
                        f.write(_text(m.get("content", "")))
                        f.write("\n\n" + "=" * 60 + "\n\n")
                    else:
                        content = m.get("content", "")
                        if role == "TOOL" and _text(content) == REASONING_RESULT:
                            continue   # the `reasoning` call's `ok`: wire plumbing, not memory
                        f.write(f"--- {role} ---\n")
                        # A native assistant turn carries its substance in the
                        # tool_calls (the `reasoning` call, or the notes on
                        # the action calls in fast mode), so render it the
                        # readable block way - otherwise every replayed step
                        # shows up blank.
                        if m.get("tool_calls"):
                            f.write(snapshot_turn(_text(m.get("content", "")), m["tool_calls"]))
                        else:
                            f.write(_text(m.get("content", "")))
                        f.write("\n\n")

                if image_sent:
                    f.write("[Screenshot sent with the latest user message]\n\n")

                # This step's freshly generated response (the reply, not part of the request).
                f.write(f"--- ASSISTANT (response · step {interaction_count}) ---\n")
                f.write(current_assistant_response)
                f.write("\n")

            print(f"✓ Memory snapshot saved: conversation_{interaction_count}.txt")
        except Exception as e:
            print(f"Error saving conversation snapshot: {str(e)}")


    def _save_raw_response(self, raw_response: str, step_number: int):
        """Save raw LLM response before any parsing/normalization"""
        if self.save_conversation:
            try:
                raw_file = self.raw_reasoning_dir / f"raw_response_{step_number}.txt"
                with open(raw_file, 'w', encoding='utf-8') as f:
                    f.write(raw_response)
            except Exception as e:
                print(f"⚠ Error saving raw response: {str(e)}")

    def _save_conversation(self, messages: list, current_assistant_response: str, image_sent: bool, interaction_count: int):
        """Save conversation snapshot to file - simple and direct"""
        if self.save_conversation:
            self._save_conversation_snapshot(messages, current_assistant_response, image_sent, interaction_count)

    @staticmethod
    def _persistent_memory(todo_list: str, scratchpad_content: str) -> str:
        """<persistent_memory>: the agent's own ToDo + scratchpad, rebuilt
        fresh every step - the tag the system prompt's <input> describes."""
        scratch = (f"<scratchpad>\n{scratchpad_content}\n</scratchpad>"
                   if scratchpad_content else "<scratchpad>none</scratchpad>")
        return f"<persistent_memory>\n<todo_list>\n{todo_list}\n</todo_list>\n\n{scratch}\n</persistent_memory>"

    def _read_todo_from_file(self) -> str:
        """Read the current todo list from the task tracker's todo.md file"""
        try:
            # Read from the task tracker's own file path (single source of truth —
            # exactly where TaskTrackerService writes), so read can never drift from write.
            todo_file = Path(self.controller.task_tracker.todo_file)
            if todo_file.exists():
                with open(todo_file, 'r', encoding='utf-8') as f:
                    return f.read().strip()
            else:
                return ""
        except Exception as e:
            print(f"⚠ Error reading todo file: {str(e)}")
            return ""

    def _read_scratchpad_from_file(self) -> str:
        """Read the current scratchpad entries from scratchpad/milestone/milestone.md file"""
        try:
            # iOS has no scratchpad service on the controller yet — read the same
            # milestone file layout as macOS directly (the future scratchpad
            # handler must write here).
            scratchpad_file = Path(__file__).parent.parent.parent / "scratchpad" / "milestone" / "milestone.md"
            if scratchpad_file.exists():
                with open(scratchpad_file, 'r', encoding='utf-8') as f:
                    return f.read().strip()
            else:
                return ""
        except Exception as e:
            print(f"⚠ Error reading scratchpad file: {str(e)}")
            return ""

    def _remove_action_from_response(self, response_json: str) -> str:
        """Remove 'action' field from assistant response to save tokens in history"""
        try:
            response_data = json.loads(response_json)
            if "action" in response_data:
                del response_data["action"]
            return json.dumps(response_data, indent=2, ensure_ascii=False)
        except Exception:
            # Handle <Step_no=X /> prefix that makes json.loads fail
            match = re.match(r'(<Step_no=\d+ />\n)(.*)', response_json, re.DOTALL)
            if match:
                prefix, json_part = match.group(1), match.group(2)
                try:
                    response_data = json.loads(json_part)
                    if "action" in response_data:
                        del response_data["action"]
                    return prefix + json.dumps(response_data, indent=2, ensure_ascii=False)
                except Exception:
                    return response_json
            return response_json

    def _tool_response_for_memory(self, action_result: dict) -> dict:
        """Build the compact tool_response preserved in agent memory for a step.

        Only web-tool results are compacted: their raw text is already removed and
        digested into the scratchpad, so memory keeps just the query + a pointer.
        Web is compacted ONLY when it succeeded — a failed web call keeps its exact
        response so the agent can see what went wrong. Every other result (shell
        output, click/input results, etc.) is preserved verbatim.
        """
        import copy
        res = copy.deepcopy(action_result)

        def compact(entry):
            if (isinstance(entry, dict)
                    and entry.get("tool") == "web"
                    and entry.get("status") == "success"):
                return {
                    "action": "tool",
                    "tool": "web",
                    "query": entry.get("query", ""),
                    "message": "memory optimized — refer to scratchpad for the web result",
                }
            return entry

        if res.get("action") == "multiple" and "results" in res:
            res["results"] = [compact(r) for r in res["results"]]
            return res
        return compact(res)

    def _backfill_web_findings(self, tool_response_str: str, findings: str) -> str:
        """Replace a web step's 'refer to scratchpad' placeholder with the actual
        numbered findings the agent distilled into the scratchpad on the digest
        step. The scratchpad is wiped on the next user request, so this keeps the
        distilled web info in durable conversation memory. Preserves the
        surrounding shape (a single web result or a 'multiple' results list)."""
        try:
            data = json.loads(tool_response_str)
        except Exception:
            return tool_response_str

        def fill(entry):
            if isinstance(entry, dict) and entry.get("tool") == "web":
                entry.pop("message", None)
                entry["web_result"] = findings
            return entry

        if isinstance(data, dict) and data.get("action") == "multiple" and "results" in data:
            data["results"] = [fill(r) for r in data["results"]]
        else:
            data = fill(data)
        return json.dumps(data, indent=2, ensure_ascii=False)

    def _trim_history_entry(self, entry: str) -> str:
        """Trim an OLDER history step for context: drop 'action' only.
        'thinking' is RETAINED in history (a skipped one costs nothing; FULL
        thinking is the durable route rationale the prompt tells the agent to
        consult when re-routing). Handles the leading
        '<Step_no=N />' marker.
        """
        m = re.match(r'(<Step_no=\d+ />\n)(.*)', entry, re.DOTALL)
        prefix, json_part = (m.group(1), m.group(2)) if m else ("", entry)
        try:
            data = json.loads(json_part)
            data.pop("action", None)
            return prefix + json.dumps(data, indent=2, ensure_ascii=False)
        except Exception:
            return entry

    def _emit_flow(self, event, payload=None):
        """Push a tool-flow event to the frontend bottom chain (best-effort)."""
        if not self.tool_callback:
            return
        try:
            self.tool_callback(event, payload)
        except Exception:
            pass

    def process_request(self, task: str) -> dict:
        """Process a user request in an iterative loop until completion.

        Returns a structured outcome {"status", "message"} so callers (Telegram,
        web UI) can tell a real completion from a failure instead of always
        reporting "done":
          - "success"    a `done` action ran
          - "error"      a critical exception ended the loop (e.g. API failure)
          - "incomplete" the loop ended otherwise (stopped, max steps, parse fails)
        """
        # Initialize tracking variables
        step_number = 0
        last_response = None
        is_first_iteration = True
        assistant_messages = []  # Track all assistant responses for memory
        tool_responses = []      # Per-step tool result, aligned 1:1 with assistant_messages; replayed as user turns
        pending_web_response = None  # Stores web tool response for light digest iteration
        web_memory_index = None  # tool_responses slot of the web step to backfill with the digest's scratchpad
        is_web_digest = False    # True only on the iteration that digests a web result into the scratchpad
        json_fail_count = 0  # Track consecutive JSON parse failures (max 3 before exit)
        reasoning_only_count = 0  # Consecutive `reasoning`-only steps (a reminder rides on the third)
        # Final outcome reported to the caller. Defaults to "incomplete" so any
        # loop exit that doesn't explicitly set success/error is reported as
        # not-finished rather than silently looking like a completion.
        final_status = "incomplete"
        final_message = "Agent stopped before completing the task"

        # Runtime compression: new call invalidates any stale worker from a
        # prior process_request on this instance.
        self._compression.reset()

        # ---- Resumable memory seed (UI continuation) -------------------------
        # If a prior optimized snapshot was supplied (continuing a saved chat),
        # replay it into the two memory lists so the existing message-build loop
        # re-emits the earlier conversation. Entries are ALREADY trimmed and
        # tool_responses already compacted, so no re-processing. The saved memory
        # ends with a terminal-note step, so on resume that note is the most-recent
        # entry — which makes the loop replay EVERY real step's tool_response.
        is_resumed = False
        original_task = task
        if self.prior_history:
            seeded = self.prior_history.get("assistant_messages") or []
            if seeded:
                assistant_messages = list(seeded)
                tool_responses = list(self.prior_history.get("tool_responses") or [])[:len(assistant_messages)]
                tool_responses += [None] * (len(assistant_messages) - len(tool_responses))
                is_resumed = True
                is_first_iteration = False  # continuation, not a fresh dialogue
                last_response = self.prior_history.get("done_message") or "Resuming previous session."
                # The chat's ORIGINAL objective (frozen to run 1 by _build_history);
                # the no="1" request prepend must carry this, not the newest request.
                original_task = self.prior_history.get("task") or task
                # Label this run boundary: fill the terminal bridge's empty tool
                # slot with a numbered request marker so this run's task is durably
                # attributed to the steps that follow it (the filled slot is part
                # of the persisted lists, so it survives every save/seed cycle).
                if tool_responses[-1] is None and _BRIDGE_SIGNATURE in assistant_messages[-1]:
                    # Next marker number: compression can swallow bridge entries,
                    # so count BOTH remaining bridges and existing marker numbers
                    # and continue past the highest.
                    n_bridges = sum(1 for m in assistant_messages if _BRIDGE_SIGNATURE in str(m))
                    marker_nos = [int(n) for tr in tool_responses if isinstance(tr, str)
                                  for n in re.findall(r'<updated_user_request no="(\d+)">', tr)]
                    tool_responses[-1] = _request_marker(max([n_bridges] + marker_nos) + 1, task)
                print(f"🧠 Resumed memory: {len(assistant_messages)} prior step(s) loaded")
        # ----------------------------------------------------------------------

        # Print model info once at the start
        print(f"\n🔄 Processing with {self.llm_manager.get_model_name()}")
        self._emit_flow("run_start")   # tool-flow chain: clear + take over the demo

        # Main agent loop
        while True:
            # Check for stop signal
            if self.stop_event and self.stop_event.is_set():
                print("\n🛑 Agent stopped by user.")
                final_status, final_message = "incomplete", "Stopped by user"
                # Don't send callback to frontend to avoid re-opening the strip
                break

            # ── Apply a finished background compression: the controller splices
            # the lists IN PLACE here on the main thread (its worker only
            # deposits) and returns the shifted web_memory_index. ────────────
            web_memory_index = self._compression.apply_pending(
                assistant_messages, tool_responses, web_memory_index)

            step_number += 1
            is_web_digest = False  # set True below only when this iteration digests a web result

            # Max step limit - prevent infinite loops
            if step_number > 100:
                print("\n🛑 Max step limit reached (100). Exiting agent loop.")
                final_status, final_message = "incomplete", "Reached the 100-step limit without finishing"
                break

            # Tool-flow chain: announce the turn. Digest iterations send no image,
            # so the chain skips screenshot+mapping (true to the agent's behaviour).
            self._emit_flow("turn", {"hasImage": not pending_web_response})

            # Skip scan for light digest iterations (web tool response)
            if pending_web_response:
                print(f"\n{'='*60}")
                print(f"🌐 Step {step_number}: Web digest iteration (no scan)")
                element_tree_text = ""
                annotated_image_base64 = None
                image_sent = False
                formatted_element_tree = ""
                skills_block = ""
            else:
                # Scan UI elements and get annotated screenshot
                print(f"\n{'='*60}")
                print(f"🔍 Step {step_number}: Scanning snapshot.")
                self.scanner.scan_elements()

                # Check Stop AFTER Scan
                if self.stop_event and self.stop_event.is_set():
                    final_status, final_message = "incomplete", "Stopped by user"
                    break

                # Already encoded at its final size/format by the scanner
                # (LLM_IMAGE_* in tree/element.py) - re-compressing here would
                # only add a decode/encode round trip and destroy the labels.
                element_tree_text, annotated_image_base64 = self.scanner.get_scan_data()

                image_sent = annotated_image_base64 is not None

                if image_sent:
                    print(f"✅ Image captured - annotated: {len(annotated_image_base64)} chars")
                else:
                    print("❌ NO IMAGE - annotated image is None!")

                # Wrap element tree in proper tags
                formatted_element_tree = f"<element_tree>\n{element_tree_text}\n</element_tree>"
                # Skills are matched against the screen we JUST scanned: the
                # app in front is the root of the page source the scanner parsed.
                skills_text = self.skills.get_knowledge(self.scanner.application_name)
                skills_block = f"<skills>\n{skills_text}\n</skills>" if skills_text else ""
                if skills_block:
                    print(f"📘 skills: loaded for {self.scanner.application_name}")

            # Construct user message based on iteration
            if is_first_iteration:
                # First iteration - user_request + element tree only.
                # ToDo creation and timing are governed by <todo_capability> in the
                # system prompt (flexible: iteration 1 by default, may create later),
                # so no hard-coded todo rules are injected here.
                user_message = f"""<user_request>
{task}
</user_request>
"""
                if skills_block:
                    user_message += f"\n{skills_block}\n"
                user_message += f"\n{formatted_element_tree}"
                if image_sent:
                    user_message += "\n\n<image>Annotated screenshot with bounding boxes</image>"
            else:
                # Fetch fresh todo from file system
                todo_list = self._read_todo_from_file()

                # Fetch fresh scratchpad entries from file system
                scratchpad_content = self._read_scratchpad_from_file()

                # Subsequent iterations - request + todo_list. <last_response>
                # is GONE: under native tool calling each result arrives in its
                # canonical place, as a role:"tool" turn keyed to the call that
                # produced it, so re-injecting it here would duplicate it.
                # On a RESUMED session, mark the request as <updated_user_request>
                # so the agent knows this is a continuation of the same session
                # (its prior steps are already in the replayed history above).
                request_tag = "updated_user_request" if is_resumed else "user_request"
                user_message = f"""<{request_tag}>
{task}
</{request_tag}>"""

                if pending_web_response:
                    user_message = f"""<critical>
No image and element tree provided. Focus on digesting the web response below.
1. Analyze thoroughly - extract all relevant data (numbers, dates, names, URLs, prices, etc.)
2. Save ALL important findings to scratchpad in this step's action.
</critical>

{pending_web_response}

<user_request>
{task}
</user_request>

{self._persistent_memory(todo_list, scratchpad_content)}"""

                    image_sent = False
                    annotated_image_base64 = None

                    # Clear flag after consumption; mark this as the web-digest step so its
                    # scratchpad response can be folded into the web memory below.
                    pending_web_response = None
                    is_web_digest = True
                else:
                    # Normal iteration - include full context
                    user_message += f"""

{self._persistent_memory(todo_list, scratchpad_content)}"""

                    if skills_block:
                        user_message += f"""

{skills_block}"""

                    user_message += f"""

{formatted_element_tree}"""

                   # Add image tag if image is provided
                    if image_sent:
                        user_message += "\n\n<image>Annotated screenshot with bounding boxes</image>"
                        user_message += "\n\n<critical>Pure vision first: decide which element to interact with from the screenshot alone, then refer to <element_tree> for its [id].</critical>"

            # Claude: the notes call and the action calls belong in ONE
            # response (see CLAUDE_BATCH_REMINDER). Last in the live message.
            if self._claude:
                user_message += CLAUDE_BATCH_REMINDER

            # Build the API messages as a real NATIVE dialogue:
            #   system -> opening user turn (the objective) -> for each past step:
            #   assistant(prose + its tool_calls) -> one role:"tool" result per
            #   call -> finally the live user message (screen + state).
            #
            # APPEND-ONLY: nothing already in the transcript is ever rewritten, so
            # the prompt-cache prefix stays valid across steps. The old per-step
            # "trim the action out of older steps" pass is gone - it invalidated
            # that prefix on every single request.
            #
            # EVERY step replays its results, including the most recent: a saved
            # tool call with no matching result is a malformed transcript that
            # every provider rejects. That is also why <last_response> is gone -
            # the result now arrives in its canonical place, keyed to its call.
            #
            # Legacy (schema-era) entries degrade to content-only assistant turns
            # and keep the old sliding-window trim + <tool_response> user-turn
            # replay, so resumed chats look exactly as they did before.
            messages = [
                {"role": "system", "content": self.system_prompt}
            ]

            # Opening turn: the chat's ORIGINAL objective in the same numbered
            # tag the run-boundary markers use, so the transcript head is stable
            # for the whole chat and follow-ups extend the cached prefix.
            if assistant_messages:
                messages.append({"role": "user",
                                 "content": _request_marker(1, original_task)})

            n_hist = len(assistant_messages)
            for i, step_msg in enumerate(assistant_messages):
                is_recent = (i == n_hist - 1)
                step = decode_step(step_msg)
                native = bool(step["tool_calls"]) or _looks_native(step_msg)
                if native:
                    asst = {"role": "assistant", "content": step["content"] or ""}
                    if step["tool_calls"]:
                        asst["tool_calls"] = step["tool_calls"]
                    if step.get("meta"):
                        # The provider's OWN metadata for this turn (Gemini 3
                        # thought signatures, OpenRouter reasoning blocks) - the
                        # provider translates it back into its dialect; every
                        # other provider never sees the key.
                        asst["provider_meta"] = step["meta"]
                    messages.append(asst)
                else:
                    # Legacy step: keep the old trim for older entries.
                    content = step_msg if is_recent else self._trim_history_entry(step_msg)
                    messages.append({"role": "assistant", "content": content})

                tr = tool_responses[i] if i < len(tool_responses) else None
                if tr:
                    # decode_results emits role:"tool" turns for native results,
                    # a bare user turn for a run-boundary marker, and a
                    # <tool_response>-wrapped user turn for legacy slots.
                    messages.extend(decode_results(tr))

            # Live user message (screen + todo + scratchpad + element tree).
            # Rebuilt every step and never persisted; the screenshot attaches to
            # it. The system prompt is added exactly once (above) - the resumable
            # seed never carries a system prompt, so a continued session can't
            # double it.
            messages.append({"role": "user", "content": user_message})

            # Capture the exact payload for the debug memory log (the last
            # iteration's value is retained — i.e. the agent's final true memory).
            self.last_messages = messages

            # Prompt caching (OpenRouter/Anthropic/AWS): mark the newest PERSISTENT
            # turn - the last tool result, or the opening user turn on step 1.
            # Never the live user message: it is rebuilt and dropped every
            # request, so a breakpoint there is written once and never read
            # back. Never an assistant turn either - Anthropic's translation
            # flattens those and would silently drop the breakpoint. The last
            # tool result survives into the next request's prefix, so each call
            # reads the cache the previous one wrote.
            if self.llm_manager.get_provider_name() in ("openrouter", "anthropic", "aws") and len(messages) > 2:
                for cache_msg in reversed(messages):
                    if cache_msg.get("role") not in ("tool", "user"):
                        continue
                    if cache_msg is messages[-1]:
                        continue  # skip the ephemeral live user message
                    content = cache_msg.get("content")
                    if isinstance(content, str) and content:
                        cache_msg["content"] = [
                            {
                                "type": "text",
                                "text": content,
                                "cache_control": {"type": "ephemeral"}
                            }
                        ]
                    break

            try:
                # Make API request through LLM Manager
                if image_sent:
                    print("📸 Screenshot sent to LLM (annotated)")

                # Check Stop BEFORE LLM
                if self.stop_event and self.stop_event.is_set():
                    final_status, final_message = "incomplete", "Stopped by user"
                    break

                # Get raw response from LLM - pass annotated image
                raw_response = self.llm_manager.send_request(messages, annotated_image_base64)

                # Memory bar: forward this call's exact token usage (input+output).
                if self.token_callback:
                    try:
                        self.token_callback(self.llm_manager.last_usage)
                    except Exception:
                        pass

                # CRITICAL Check Stop AFTER LLM (discards result if stopped while waiting)
                if self.stop_event and self.stop_event.is_set():
                    print("\n🛑 Agent stopped by user (response discarded).")
                    final_status, final_message = "incomplete", "Stopped by user"
                    break

                # Runtime compression trigger: context size is fresh in last_usage.
                self._compression.maybe_trigger(assistant_messages, tool_responses, original_task)

                print("✓ LLM response received")

                # Save raw response before any parsing (for debugging). Under
                # native tool calling this is a dict, not a string.
                self._save_raw_response(
                    json.dumps(raw_response, indent=2, ensure_ascii=False)
                    if isinstance(raw_response, dict) else str(raw_response),
                    step_number)

                # NATIVE TOOL CALLING: nothing to parse. Convert the model's own
                # calls into the same [{type, ...}] action dicts route_action has
                # always consumed, keeping their ids so each result pairs back to
                # the call that produced it in the next request.
                actions, calls, rejects, track = tool_calls_to_steps(
                    raw_response.get("tool_calls"), allowed=self._allowed_tools,
                    defaults_map=self._action_defaults, track_params=self._track_params)
                resp_text = (raw_response.get("text") or "").strip()

                # The notes call (`reasoning` in quality mode, `memory` in fast)
                # is the step's notes, not an action: pull it out before
                # routing (the controller has no handler for it) but keep it in
                # `all_calls`, which is what the assistant turn replays - and
                # what Gemini's thought signature binds to. Each note call is
                # answered with the bare REASONING_RESULT; the snapshot and the
                # compression dump hide it.
                all_calls = calls
                note_ids = [c["id"] for a, c in zip(actions, calls) if a.get("type") == self._notes_tool]
                note_results = [{"tool_call_id": i, "content": REASONING_RESULT} for i in note_ids]
                kept = [(a, c) for a, c in zip(actions, calls) if a.get("type") != self._notes_tool]
                actions = [a for a, _ in kept]
                calls = [c for _, c in kept]

                # Backstop for an `async_tools` model (Astra): a `done` bundled
                # after this turn's actions is dropped from the batch and
                # answered with an error result (see drop_bundled_terminal);
                # the result rides with the rejects below.
                done_results = []
                if self.llm_manager.async_tools():
                    actions, calls, done_results = drop_bundled_terminal(actions, calls, rejects, "done")
                    if done_results:
                        print("`done` bundled with other actions: ignored, it must be the only call of its turn")

                if not actions and not rejects:
                    if note_ids:
                        # A reasoning-only step: the model chose to think
                        # before acting. Accepted as a normal step (it costs a
                        # fresh scan, so only a RUN of them is bounded: from
                        # the third in a row the result carries a reminder).
                        reasoning_only_count += 1
                        # An `async_tools` model that never acts is the route
                        # having dropped the async flags (one call per turn
                        # again): the reminder cannot help it, so the run stops
                        # at six instead of scanning to the step cap.
                        if self.llm_manager.async_tools() and reasoning_only_count >= 6:
                            final_status = "incomplete"
                            final_message = (
                                f"{reasoning_only_count} steps in a row without an action on an "
                                "async-tools model: the route dropped the async tool flags (see the "
                                "raw responses), so the model can only emit one call per turn.")
                            break
                        if reasoning_only_count >= 3:
                            note_results = [{"tool_call_id": i, "content": json.dumps({
                                "status": "success", "action": self._notes_tool,
                                "message": f"{reasoning_only_count} steps in a row without an action: "
                                           "call a tool that does the work in this turn."},
                                indent=2, ensure_ascii=False)} for i in note_ids]
                        normalized = encode_step(resp_text, wire_calls_from(all_calls),
                                                 raw_response.get("provider_meta"))
                        # Only the fields this mode carries (fast has no thinking).
                        display = {}
                        if "assessment" in track:
                            display["assessment"] = track["assessment"] or resp_text or SKIPPED_THINKING
                        if "thinking" in track:
                            display["thinking"] = track["thinking"] or resp_text or SKIPPED_THINKING
                        if "memory" in track:
                            display["memory"] = track["memory"]
                        if "next_goal" in track:
                            display["next_goal"] = track["next_goal"]
                        display["action"] = []
                        display_payload = json.dumps(display, indent=2, ensure_ascii=False)
                        self._save_conversation(messages, display_payload, image_sent, step_number)
                        assistant_messages.append(normalized)
                        tool_responses.append(encode_results(note_results))
                        print(AgentResponseFormatter.format_response(display_payload, include_action=True))
                        if self.text_callback:
                            self.text_callback(AgentResponseFormatter.format_response(display_payload, include_action=False))
                        self._emit_flow("received", {"tools": []})
                        is_first_iteration = False
                        continue
                    # REPAIR, not exit: `done` is an explicit dedicated final
                    # call, so a message with NO tool calls is always a
                    # dropped-calls mistake - the model narrated its move
                    # instead of making it. Keep the turn so the reasoning isn't
                    # lost, answer it with a corrective result turn, and let the
                    # model continue. The strike counter still bounds a model
                    # that keeps talking without acting.
                    json_fail_count += 1
                    if resp_text:
                        print(f"message had no tool calls - asking the model to act ({json_fail_count}/3)")
                        if json_fail_count >= 3:
                            final_status, final_message = "incomplete", "Model kept responding without calling any tool (3 consecutive turns)"
                            break
                        normalized = encode_step(resp_text, [], raw_response.get("provider_meta"))
                        # snapshot shows the prose the model wrote, not the codec JSON
                        self._save_conversation(messages, resp_text, image_sent, step_number)
                        assistant_messages.append(normalized)
                        # Feedback in the SAME envelope every real result uses -
                        # the environment reporting "nothing ran", not an
                        # instruction breaking the loop's rhythm.
                        tool_responses.append(
                            "<tool_response>\n"
                            + json.dumps({"message": "no tool called"}, indent=2, ensure_ascii=False)
                            + "\n</tool_response>")
                        is_first_iteration = False
                        continue
                    # Genuinely empty turn (no text, no calls) - retry the step.
                    print(f"empty response from the model - retrying ({json_fail_count}/3)")
                    if json_fail_count >= 3:
                        final_status, final_message = "incomplete", "Could not get a valid model response (3 consecutive failures)"
                        break
                    step_number -= 1
                    continue

                # Reset the consecutive counters on a real, actionable turn
                json_fail_count = 0
                reasoning_only_count = 0

                # The assistant turn EXACTLY as the model produced it: prose on
                # the text channel, its tool calls on the tool channel - this is
                # what gets replayed next step. REJECTED calls are included too:
                # the turn must mirror what the model emitted, and every result
                # written below (errors included) has to pair with a call of the
                # same id or the next request is malformed.
                normalized = encode_step(resp_text, wire_calls_from(all_calls + rejects),
                                         raw_response.get("provider_meta"))

                # Display payload - the terminal print, the frontend text stream
                # and the tool-icon chain read this block view; the transcript
                # sent to the model does not.
                # Only the params this mode actually carries: quality has all
                # three (on the `reasoning` call), fast has `next_goal` alone.
                # Rendering one the mode dropped would print an empty block in
                # the terminal and the UI stream.
                display = {}
                if "assessment" in track:
                    display["assessment"] = track["assessment"] or resp_text or SKIPPED_THINKING
                if "thinking" in track:
                    display["thinking"] = track["thinking"] or resp_text or SKIPPED_THINKING
                if "memory" in track:
                    display["memory"] = track["memory"]
                if "next_goal" in track:
                    display["next_goal"] = track["next_goal"]
                display["action"] = actions
                display_payload = json.dumps(display, indent=2, ensure_ascii=False)

                # Save a faithful snapshot of the EXACT payload sent this step plus
                # this step's response - a true peek into agent memory. The
                # response is rendered as the readable block view, not the codec
                # JSON (whose call arguments are JSON escaped inside JSON).
                self._save_conversation(messages, display_payload, image_sent, step_number)

                # Record the turn with an empty result slot, backfilled after
                # routing - so a `done` turn also lands in the conversation.
                assistant_messages.append(normalized)
                tool_responses.append(None)

                # A call naming a tool that doesn't exist is answered with an
                # error result instead of being dropped, so the model sees what
                # went wrong - keyed to its own call id - and corrects next turn.
                reject_results = [{"tool_call_id": r["id"], "content": f"error: {r['error']}"}
                                  for r in rejects]
                reject_results += done_results

                # -- Closing this step's call/result pairing ------------------
                # A saved tool call with no matching result is a malformed
                # transcript every provider rejects on resume, so the slot must
                # never stay None once the turn is recorded. There are exactly
                # three honest ways to close it, and each break path below picks
                # the one that matches what actually happened:
                #   _pair_results  - the actions RAN: record what they returned.
                #   _discard_step  - nothing ran: drop the turn entirely rather
                #                    than invent a result for a call that never
                #                    executed (same contract as the post-LLM
                #                    stop check, which discards the response).
                #   _close_pairing - the run died mid-flight: record the error.
                def _pair_results(action_result):
                    """Record what the calls actually returned, keyed per call:
                    one result each when the controller returned a matching
                    batch, otherwise the first call carries the whole envelope
                    and the rest point at it. Web results are compacted to a
                    scratchpad pointer by _tool_response_for_memory."""
                    compacted = self._tool_response_for_memory(action_result)
                    batch = compacted.get("results") if isinstance(compacted, dict) else None
                    if calls and isinstance(batch, list) and len(batch) == len(calls):
                        paired = [
                            {"tool_call_id": c["id"],
                             "content": json.dumps(batch[idx], indent=2, ensure_ascii=False)}
                            for idx, c in enumerate(calls)
                        ]
                    elif calls:
                        envelope = json.dumps(compacted, indent=2, ensure_ascii=False)
                        paired = [{"tool_call_id": calls[0]["id"], "content": envelope}]
                        paired += [
                            {"tool_call_id": c["id"],
                             "content": "(result included in the first tool result of this batch)"}
                            for c in calls[1:]
                        ]
                    else:
                        paired = []
                    if tool_responses:
                        tool_responses[-1] = encode_results(note_results + reject_results + paired)

                def _discard_step():
                    """Drop this step's turn and its empty slot - nothing ran, so
                    there is nothing to remember. Keeps the two lists 1:1 and
                    leaves the saved chat ending on the last step that really
                    happened. Returns True when it dropped something."""
                    if (assistant_messages and tool_responses
                            and tool_responses[-1] is None
                            and assistant_messages[-1] is normalized):
                        assistant_messages.pop()
                        tool_responses.pop()
                        return True
                    return False

                def _close_pairing(note: str):
                    """Last resort for a run dying mid-step (exceptions): record
                    the note against every call so nothing dangles."""
                    if tool_responses and tool_responses[-1] is None and (calls or reject_results or note_results):
                        tool_responses[-1] = encode_results(
                            note_results + reject_results
                            + [{"tool_call_id": c["id"], "content": note} for c in calls])

                # Format the response with emojis for console output (terminal: include action block)
                formatted_response = AgentResponseFormatter.format_response(display_payload, include_action=True)
                print(formatted_response)

                # Send to frontend if callback exists (omit action from stream)
                if self.text_callback:
                    self.text_callback(AgentResponseFormatter.format_response(display_payload, include_action=False))

                # Tool-flow chain: the packet arrived — tick "packet received" and hand
                # the frontend this turn's tools, read straight from the action block.
                self._emit_flow("received", {"tools": AgentResponseFormatter.extract_tools(display_payload)})

                if rejects:
                    for r in rejects:
                        print(f"unknown tool '{r['name']}' - error returned to the model")
                    if not actions:
                        # Nothing valid to route: persist just the errors and let
                        # the model correct itself on the next turn.
                        tool_responses[-1] = encode_results(note_results + reject_results)
                        is_first_iteration = False
                        continue

                # The actions came straight from the model's native tool calls
                try:
                    agent_response = {"action": actions}
                    if agent_response:

                        # Execute actions if present
                        if "action" in agent_response and agent_response["action"]:
                            # Check Stop BEFORE Action - NOTHING ran this step,
                            # so discard the turn instead of saving a result for
                            # a call that never executed. The chat then ends on
                            # the last step that really happened.
                            if self.stop_event and self.stop_event.is_set():
                                _discard_step()
                                print("\n🛑 Agent stopped by user (step discarded - no action ran).")
                                final_status, final_message = "incomplete", "Stopped by user"
                                break

                            # Execute the action
                            print("\n⚡ Executing action...")

                            # (The iOS scanner already pushed the elements mapping to
                            # controller_service during scan_elements, so no
                            # set_elements handoff is needed here.)

                            # Send action to controller
                            action_result = self.controller.route_action(agent_response["action"])

                            # Check if action was stopped mid-execution. Here the
                            # actions DID partially run, so record what the
                            # controller actually returned - that partial effect
                            # is real and the next run must see it.
                            if action_result.get("status") == "stopped":
                                print("\n🛑 Agent stopped by user (action interrupted).")
                                _pair_results(action_result)
                                final_status, final_message = "incomplete", "Stopped by user"
                                break

                            # Check if web tool was used — store for light digest iteration.
                            # A failed search stays in its own result, so its error is
                            # kept in memory verbatim instead of going to a digest.
                            web_results_list = []
                            if (action_result.get("tool") == "web" and "result" in action_result
                                    and action_result.get("status") == "success"):
                                web_results_list.append(action_result["result"])
                                del action_result["result"]
                            elif action_result.get("action") == "multiple" and "results" in action_result:
                                for idx, result in enumerate(action_result["results"]):
                                    if (result.get("tool") == "web" and "result" in result
                                            and result.get("status") == "success"):
                                        web_results_list.append(result["result"])
                                        del action_result["results"][idx]["result"]

                            if web_results_list:
                                pending_web_response = "<tool>\n" + "\n".join(web_results_list) + "\n</tool>"
                                print(f"🌐 Web results captured for digest iteration")
                            else:
                                pending_web_response = None

                            # Check if task completed (done action was executed)
                            if action_result.get("action") == "done":
                                print(f"\n🎉 Task Complete: {action_result.get('summary', 'Task completed')}")
                                print("✅ Agent has finished all tasks. Exiting loop.")
                                # `done` ran, so record the real outcome (the
                                # summary) rather than a filler - this is the
                                # final step the next resume reads.
                                _pair_results(action_result)
                                final_status = "success"
                                final_message = action_result.get("summary", "Task completed")
                                break

                            # Store the action result as last_response
                            last_response = json.dumps(action_result, indent=2)

                            # Preserve this step's results as role:"tool" turns
                            # keyed to the calls that produced them.
                            _pair_results(action_result)

                            # Web-result memory: on the digest step, fold the numbered
                            # scratchpad findings the agent JUST wrote into the original web
                            # step's tool_response — replacing the 'refer to scratchpad'
                            # pointer. The scratchpad is wiped on the next user request, so
                            # this keeps the distilled web info in durable conversation memory.
                            if (is_web_digest and web_memory_index is not None
                                    and 0 <= web_memory_index < len(tool_responses)
                                    and tool_responses[web_memory_index]):
                                entries = [
                                    str(a.get("value", "")).strip()
                                    for a in (agent_response.get("action") or [])
                                    if isinstance(a, dict) and a.get("type") == "scratchpad" and a.get("value")
                                ]
                                if entries:
                                    numbered = "\n".join(f"{i + 1}. {e}" for i, e in enumerate(entries))
                                    # The web step's slot is a native result list.
                                    # Fold the findings into EVERY entry (the web
                                    # call may sit at any index - results are
                                    # per-call now, and reject errors ride ahead
                                    # of them; _backfill_web_findings is a no-op
                                    # on anything that isn't a web result) and
                                    # re-encode onto the SAME call ids, so the
                                    # pairing that step wrote stays intact.
                                    slot = tool_responses[web_memory_index]
                                    try:
                                        stored = json.loads(str(slot))
                                    except Exception:
                                        stored = None
                                    if (isinstance(stored, list) and stored
                                            and all(isinstance(d, dict) and "tool_call_id" in d
                                                    for d in stored)):
                                        for entry in stored:
                                            entry["content"] = self._backfill_web_findings(
                                                str(entry.get("content") or ""), numbered)
                                        tool_responses[web_memory_index] = encode_results(stored)
                                    else:
                                        # Legacy (schema-era) slot: plain JSON string.
                                        tool_responses[web_memory_index] = self._backfill_web_findings(
                                            str(slot), numbered)
                                web_memory_index = None

                            # A web call this step points the pointer at THIS step,
                            # for the next digest to fold into. Set AFTER the fold
                            # above, which still needed the previous web step.
                            if web_results_list:
                                web_memory_index = len(tool_responses) - 1

                            # Wait before next scan (default 1 second, unless wait action was used)
                            wait_time = 1.0  # Default wait
                            if action_result.get("tool") == "wait":
                                # If wait was explicitly called, use that duration
                                wait_time = action_result.get("duration", 1.0)
                            elif action_result.get("action") == "multiple":
                                # Check if wait was in multiple actions
                                for result in action_result.get("results", []):
                                    if result.get("tool") == "wait":
                                        wait_time = result.get("duration", 1.0)
                                        break

                            print(f"⏳ Waiting {wait_time}s before next scan...")
                            elapsed = 0.0
                            while elapsed < wait_time:
                                if self.stop_event and self.stop_event.is_set():
                                    break
                                time.sleep(min(0.5, wait_time - elapsed))
                                elapsed += 0.5

                            # Print action result
                            if action_result.get("status") == "success":
                                print(f"✓ Action executed successfully")

                                # Check if this was a todo creation
                                if action_result.get("action") == "todo_created":
                                    print("📋 Todo list created")

                                # Check if this was a todo update
                                elif action_result.get("action") == "todo_updated":
                                    print("✓ Todo task marked complete")
                            else:
                                print(f"⚠ Action result: {action_result.get('message', 'Unknown error')}")

                        # Mark first iteration as done
                        is_first_iteration = False

                except Exception as e:
                    print(f"⚠ Error processing action: {str(e)}")
                    # The step died mid-flight: record the error against every
                    # call so the saved chat never carries a dangling tool call.
                    _close_pairing(f"error: {str(e)}")
                    # Even on error, continue the loop
                    last_response = json.dumps({"status": "error", "message": str(e)})
                    is_first_iteration = False

            except Exception as e:
                error_msg = f"❌ Error processing request: {str(e)}"
                print(error_msg)
                # Same pairing guarantee for a failure ANYWHERE in the step. The
                # NameError guard covers an exception raised before the closers
                # were defined (e.g. the LLM call itself failing).
                try:
                    _close_pairing(f"error: {str(e)}")
                except NameError:
                    pass
                # On critical error, record it and break the loop so the caller
                # can report the failure instead of a false "done".
                final_status, final_message = "error", str(e)
                break

        # tool-flow chain: if the run didn't finish cleanly, cap it with a "!" drop.
        if final_status == "error":
            self._emit_flow("error", {"text": "llm service not responding"})
        elif final_status == "incomplete" and final_message and "stop" in final_message.lower():
            self._emit_flow("error", {"text": "agent interrupted"})
        self._emit_flow("run_end")   # tool-flow chain: run finished

        # Expose the final conversation lists so the conversation service (NOT the
        # agent) can optimize + persist them after the run. Memory management is
        # kept entirely out of the agent. Return shape is unchanged.
        # Compression still pending at run end (in flight or deposited but never
        # applied) — the result is discarded; make sure the indicator stops.
        self._compression.finish_run()

        self.assistant_messages = assistant_messages
        self.tool_responses = tool_responses

        return {"status": final_status, "message": final_message}
