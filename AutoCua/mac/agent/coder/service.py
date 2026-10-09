import os
import time
import json
import re
import sys
import io
import shutil
import uuid
from pathlib import Path
from datetime import datetime
from typing import Optional
import threading

import subprocess
from ...tool_registry.service import (LLMManager, tool_calls_to_steps, REASONING_FIELDS, drop_bundled_terminal,
                                         CLAUDE_REASONING_FIELDS, ACTION_DEFAULTS_CLAUDE,
                                         CODER_TOOL_NAMES_CLAUDE)
from ...controller.view import ControllerView
from ...controller.cli.service import render_tool_response
from ....memory_compression.controller import CompressionController
from .view import (CLIAgentResponseFormatter, snapshot_turn, decode_step,
                   encode_step, decode_results, encode_results, wire_calls_from,
                   compression_dump, compression_entry, REASONING_RESULT)

# Import debug_log and IS_COMPILED for safe logging in compiled mode
try:
    from app import debug_log, IS_COMPILED
except ImportError:
    def debug_log(msg, level="INFO"):
        pass
    IS_COMPILED = False

# =============================================================================
# FIX FOR COMPILED CLI SUBPROCESS: Ensure stdout/stderr are valid
# PyInstaller --windowed builds set sys.stdout/stderr to None at startup
# (no console). When the main agent spawns this binary as a subprocess via
# `subprocess.Popen(..., stdout=PIPE, stderr=PIPE)`, the OS-level fds 1 and
# 2 ARE valid pipe ends connected to the parent's reader threads — we just
# need a Python wrapper around them so safe_print() actually flows through.
# Falling back to io.StringIO() (the old behavior) silently swallowed every
# line, which is exactly why the streaming pill stayed empty in the .app.
# =============================================================================
def _ensure_real_stdio_or_fallback():
    for name, fd in (('stdout', 1), ('stderr', 2)):
        stream = getattr(sys, name, None)
        if stream is not None and not (hasattr(stream, 'closed') and stream.closed):
            # Already valid (dev mode) — keep the stream, but pin it to UTF-8 with
            # errors='replace'. A pipe otherwise takes the locale encoding (UTF-8
            # normally, but not under e.g. LANG=en_US.ISO-8859-1) with strict
            # errors: a char it can't encode — the → every next_goal carries, or a
            # lone surrogate from a broken emoji — makes print() raise, safe_print
            # swallows it, and the whole step never reaches the UI (whose reader
            # decodes UTF-8). This used to happen as a side effect of
            # `from app import ...` running app.py's stdio fixup; app.py is gone.
            try:
                stream.reconfigure(encoding='utf-8', errors='replace')
            except Exception:
                pass
            continue
        try:
            raw = os.fdopen(fd, 'wb', buffering=0, closefd=False)
            wrapped = io.TextIOWrapper(
                raw,
                encoding='utf-8',
                errors='replace',
                write_through=True,
                line_buffering=False,
            )
            setattr(sys, name, wrapped)
        except OSError:
            # fd is unusable (truly headless, no parent pipe) — last-resort
            # in-memory buffer so safe_print doesn't raise.
            setattr(sys, name, io.StringIO())

_ensure_real_stdio_or_fallback()

# Safe print function that won't crash on closed stdout
def safe_print(*args, **kwargs):
    """Print that won't crash if stdout is unavailable.
    Always forces flush so the parent process's pipe reader sees output
    immediately (the streaming pill UI relies on prompt line delivery)."""
    kwargs.setdefault("flush", True)
    try:
        print(*args, **kwargs)
    except (ValueError, OSError, AttributeError):
        # I/O operation on closed file or similar - just log instead
        msg = ' '.join(str(a) for a in args)
        debug_log(f"[PRINT] {msg}")

# Maximum iterations before CLI agent auto-exits (prevents infinite loops)
MAX_CLI_ITERATIONS = 50

# ── Shell-use conversation continuity ────────────────────────────────────────
# The bridge ("terminal note") is appended by agent_conversation's _build_history
# at save time, capping every saved run; a resumed run fills that bridge's empty
# tool slot with the NEW request's numbered <user_request=N> tag. That user turn
# IS the live request, sitting in its chronological place between the previous
# run's conclusion and this run's steps; the transcript head stays pinned to
# <user_request=1>, so follow-ups extend the cached prefix instead of
# rewriting it.

def _is_bridge_entry(text) -> bool:
    """True only for agent_conversation's synthetic terminal note, any
    generation. Structural — exact key sets + the canonical sentence — so a
    real step (a native {content, tool_calls} turn, or an old 4-key JSON step)
    that merely echoes the bridge phrase it saw in context can never match."""
    try:
        d = json.loads(str(text))
    except Exception:
        return False
    if not isinstance(d, dict):
        return False
    keys = set(d.keys())
    goal = str(d.get("next_goal", ""))
    # Pre-native generations: {memory, next_goal} and {thinking, memory,
    # next_goal} (thinking = "not required").
    if keys in ({"memory", "next_goal"}, {"thinking", "memory", "next_goal"}):
        return goal.startswith("Previous run concluded.")
    # Native-tools generation: labeled convention — memory folded into
    # next_goal ("memory: ... next_goal: ..."), no standalone memory key.
    if keys == {"thinking", "next_goal"}:
        return goal.startswith("memory: ") and "next_goal: Previous run concluded." in goal
    return False


# Appended to the system prompt for an `async_tools` model (GPT-6 Astra, see
# llm_provider/responses.py): its tool calls run after the turn ends, and the
# block tells it what that changes about the rules above it.
ASYNC_TOOLS_ADDENDUM = """
<async_tools>
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's <Tool_response>. The rules above are unchanged: one `reasoning` call first, then the calls that do the step, batched per <efficiency_guideline>. Because nothing comes back mid-turn:
- Never write a text message in a turn that contains tool calls, and never state that a command succeeded or failed: that verdict belongs to the next step's memory, judged on the <Tool_response>.
- `exit` stays the only call of its turn. Never add it after this turn's calls: their results have not arrived, so nothing is verified yet.
</async_tools>
"""


class AgentService:
    """CLI Agent Service - Agentic loop only"""
    
    def __init__(self, provider: str, model: str, save_conversation: bool = False,
                 api_key: str = None, stop_event=None,
                 task: str = None, on_complete: callable = None,
                 external_terminal: bool = True, report_usage: bool = False,
                 request_no: int = 1, history_file: str = None):

        self.provider = provider
        self.model = model
        self.save_conversation = save_conversation
        self.stop_event = stop_event
        self.task = task  # Task description (for tracking when called as service)
        self.on_complete = on_complete  # Callback when CLI agent exits
        # When True, sub-spawns (minions) get their own visible Terminal.app window.
        # Default True so cli.py and main.py terminal flows show every sub-agent live;
        # app.py / UI mode can pass False to keep them hidden.
        self.external_terminal = external_terminal
        # Shell use only (--report-usage): after every LLM call, emit the call's
        # normalized token usage up the stdout marker pipe so the frontend's
        # memory bar tracks this coder's live context — the SAME gauge mechanism
        # Computer use drives. Main-agent dispatches, minions and cli.py never
        # pass the flag, so they emit nothing and behave exactly as before.
        self.report_usage = report_usage
        # Which request of the CHAT this run answers: the task is injected as
        # <user_request=N>. A standalone run is request 1; the Shell-use
        # conversation layer passes the follow-up number in (--request-no) so
        # the model can tell the requests of one ongoing session apart.
        self.request_no = max(1, int(request_no or 1))
        # Shell-use CONVERSATION (--history): the chat's history file threads ONE
        # conversation across runs — seeded in at start, atomically rewritten
        # after every step (a user stop hard-kills this process, so the per-step
        # snapshots are what keep progress recoverable for the parent's save).
        # Absent = stateless run, exactly as before.
        self.history_file = Path(history_file) if history_file else None
        self.assistant_messages = []   # parallel history lists (main-driver style)
        self.tool_responses = []
        self.last_messages = None      # exact final LLM payload (→ memory_log.txt)
        self._original_task = task
        self._history_finalized = False   # final snapshot flushed before the result file?
        self._seed_request_marker = None  # THIS run's bridge-slot fill (the live request's chronological turn)
        # <manual_mode> block from the seed — commands the user hand-typed at
        # the shared terminal since the previous run. Normally embedded into
        # the bridge-slot fill (persisted); kept here ONLY when no bridge slot
        # exists (fresh chat), where it rides the opening turn instead.
        self._seed_manual_block = None
        # The parent's delivery ack, stamped into every history snapshot. Set
        # the moment delivery is DURABLE: at bridge-slot embed time (the block
        # then persists inside tool_responses), or — opening-turn case — only
        # after the first SUCCESSFUL LLM call actually showed it to the model.
        # Never at seed-load: the crash-path snapshot would ack an undelivered
        # block and the parent would drop the records.
        self._manual_delivered = False

        # Generate unique session ID for complete isolation
        self.session_id = uuid.uuid4().hex[:8]

        # Initialize LLM Manager
        self.llm = LLMManager(
            provider=provider,
            model=model,
            api_key=api_key,
            cli_agent=True
        )
        # Any Claude model (any provider) runs on claude_prompt.md, and its notes
        # call is `agent_memory`, with `assessment` in place of `thinking`.
        self._claude = self.llm.claude
        self._notes_tool = "agent_memory" if self._claude else "reasoning"

        # Initialize Controller with cli_mode + session_id for isolation, and propagate
        # external_terminal so spawned minions (via the `minion` action) inherit the
        # same "give each sub-agent its own visible Terminal.app window" behavior.
        self.controller = ControllerView(
            provider=provider, model=model,
            cli_mode=True, session_id=self.session_id,
            api_key=api_key,
            external_terminal=external_terminal,
        )

        # Rolling handoff compression — the SHARED controller with two coder-
        # flavored callables (native dump renderer + native-safe splice entry).
        # Runs for every coder run; the indicator rides _emit_usage, which is
        # gated on --report-usage, so only the top-level Shell-use coder blinks
        # the frontend while dispatched coders compress silently.
        self._compression = CompressionController(
            self.llm, LLMManager, self._emit_usage, stop_event,
            dump_builder=compression_dump, synthetic_entry=compression_entry)

        # Load system prompt
        self.system_prompt = self._load_system_prompt()
        # An `async_tools` model (GPT-6 Astra) runs its calls after the turn
        # ends; the block tells it what that changes.
        if self.llm.async_tools():
            self.system_prompt += ASYNC_TOOLS_ADDENDUM
        
        # Setup session-specific conversation directory (subfolder inside cli_conversation)
        if self.save_conversation:
            self.conversation_dir = Path("cli_conversation") / self.session_id
            self.conversation_dir.mkdir(parents=True, exist_ok=True)
            
            # Create raw_reasoning directory for storing raw LLM outputs (for debugging)
            self.raw_reasoning_dir = self.conversation_dir / "raw_reasoning"
            self.raw_reasoning_dir.mkdir(parents=True, exist_ok=True)
        
        self.interaction_count = 0
        self._pending_web_response = ""  # Store web response for next iteration
        # Last todo/plan actually sent — state is re-injected only when it
        # changes, so the append-only transcript stays cache-friendly.
        self._last_state = {}
    
    def _save_conversation_snapshot(self, messages: list, current_assistant_response: str, interaction_count: int):
        """Save TRUE agent memory - exactly what LLM receives at each step

        Dumps the actual messages list sent to the API — except assistant
        turns, which are rendered through snapshot_turn so each past step
        reads as ONE {"thinking", "memory", "next_goal", "action"} JSON (the
        reasoning call folded into the first three, the action calls listed)
        instead of hiding its tool calls.
        """
        if not self.save_conversation:
            return

        conversation_file = self.conversation_dir / f"conversation_{interaction_count}.txt"

        with open(conversation_file, 'w', encoding='utf-8') as f:
            # Header
            f.write("=== CLI AGENT MEMORY SNAPSHOT ===\n")
            f.write(f"Step: {interaction_count}\n")
            f.write(f"Time: {datetime.now()}\n")
            f.write("=" * 60 + "\n\n")

            # Dump every message exactly as sent to the API, except the
            # reasoning call's placeholder result, which says nothing.
            n = 0
            for msg in messages:
                role = msg["role"]
                content = msg["content"]
                if role == "tool" and content == REASONING_RESULT:
                    continue
                if isinstance(content, list):
                    content = content[0]["text"] if content and isinstance(content[0], dict) and "text" in content[0] else str(content)
                f.write(f"=== MESSAGE {n} (role='{role}') ===\n")
                n += 1
                if role == "assistant":
                    f.write(snapshot_turn(content, msg.get("tool_calls")))
                else:
                    f.write(content)
                f.write("\n\n" + "=" * 60 + "\n\n")

            # Current assistant response (not yet in messages list)
            entry = decode_step(current_assistant_response)
            f.write(f"=== CURRENT ASSISTANT RESPONSE (role='assistant') ===\n")
            f.write(snapshot_turn(entry["content"], entry["tool_calls"]))
            f.write("\n")
    
    def _save_raw_response(self, raw_response: str, step_number: int):
        """Save raw LLM response before any parsing/normalization (for debugging)"""
        if self.save_conversation:
            try:
                raw_file = self.raw_reasoning_dir / f"raw_response_{step_number}.txt"
                with open(raw_file, 'w', encoding='utf-8') as f:
                    f.write(raw_response)
            except Exception as e:
                safe_print(f"⚠ Error saving raw response: {str(e)}")
    
    def _load_system_prompt(self) -> str:
        """Load system prompt from file (claude_prompt.md for Claude)"""
        current_dir = os.path.dirname(os.path.abspath(__file__))
        prompt_path = os.path.join(current_dir, "claude_prompt.md" if self._claude else "system_prompt.md")

        with open(prompt_path, 'r', encoding='utf-8') as f:
            return f.read()
    
    # ── Native transcript codec ──────────────────────────────────────────
    def _trim_history_entry(self, response_json: str) -> str:
        """Sliding-window memory: drops an 'action' field if one exists. Native
        tool-calling entries carry NO action block (the call's outcome lives in
        the following <Tool_response>), so for them this is a safe no-op — it
        still trims legacy envelope entries seeded from an old history file.
        thinking/memory/next_goal are always RETAINED as the durable route
        rationale."""
        try:
            response_data = json.loads(response_json)
            if "action" in response_data:
                del response_data["action"]
            return json.dumps(response_data, indent=2, ensure_ascii=False)
        except Exception:
            return response_json

    def _tool_response_for_memory(self, action_result: dict) -> dict:
        """Build the compact tool_response preserved in memory for a step
        (main-driver policy). Only SUCCESSFUL web-tool results are compacted:
        their raw text was already extracted into the next step's <tool> block
        and gets digested into the scratchpad there, so memory keeps just the
        query + a pointer. A failed web call keeps its exact response so the
        model can see what went wrong; every other result stays verbatim."""
        import copy
        res = copy.deepcopy(action_result)

        def compact(entry):
            # One flat shape for a web call's durable result: status, action,
            # query, result. `result` starts as a pointer and is replaced by
            # the findings the model records on the digest step (see
            # _fill_web_findings).
            if (isinstance(entry, dict)
                    and entry.get("tool") == "web"
                    and entry.get("status") == "success"):
                return {
                    "status": "success",
                    "action": "web",
                    "query": entry.get("query", ""),
                    "result": ("delivered in the next step's <tool> block; the scratchpad "
                               "entries recorded there replace this line"),
                }
            return entry

        if res.get("action") == "multiple" and "results" in res:
            res["results"] = [compact(r) for r in res["results"]]
            return res
        return compact(res)

    @staticmethod
    def _fill_web_findings(data: dict, findings: str) -> None:
        """Fold the digest step's numbered scratchpad findings into the STASHED
        (compacted) result dict IN PLACE: each web entry's pointer message is
        replaced by the findings as its durable web_result. The caller then
        re-renders the whole envelope from this dict — no parsing of the stored
        string, so the fold is immune to output bodies that themselves contain
        envelope-looking text (e.g. viewing this very file). The scratchpad is
        per-run, so this is what turns web knowledge into durable conversation
        memory (main-driver policy)."""
        def fill(entry):
            if isinstance(entry, dict) and (entry.get("action") == "web" or entry.get("tool") == "web"):
                entry.pop("message", None)
                entry.pop("web_result", None)
                entry["result"] = findings
        if isinstance(data, dict) and data.get("action") == "multiple" and isinstance(data.get("results"), list):
            for r in data["results"]:
                fill(r)
        else:
            fill(data)

    @staticmethod
    def _has_web(data) -> bool:
        """True when a (compacted) result dict is, or contains, a web result."""
        if not isinstance(data, dict):
            return False
        if data.get("action") == "web" or data.get("tool") == "web":
            return True
        return (data.get("action") == "multiple"
                and any(AgentService._has_web(r) for r in data.get("results") or []))
    
    def _remove_agent_sitting_from_user_message(self, user_message: str) -> str:
        """Remove <agent_sitting> block from user message to save tokens in history"""
        try:
            # Remove <agent_sitting>...</agent_sitting> block
            cleaned = re.sub(r'<agent_sitting>.*?</agent_sitting>', '', user_message, flags=re.DOTALL)
            # Clean up extra whitespace
            cleaned = re.sub(r'\n{3,}', '\n\n', cleaned)
            return cleaned.strip()
        except Exception:
            return user_message
    
    def _emit_usage(self, usage):
        """Forward one LLM call's normalized usage (llm.last_usage — shared
        LLMManager._normalize_usage shape: input/output/total/context_tokens) up
        the stdout marker pipe as a `token_usage` event. The parent ControllerView
        re-emits it and the frontend feeds the SAME MemoryTracker/updateMemoryBar
        path Computer use uses. Gated on --report-usage: only the TOP-LEVEL
        Shell-use coder counts — minion or dispatched-coder usage never pollutes
        the gauge. Called from the agent loop's own thread only."""
        if not self.report_usage:
            return
        try:
            self.controller._safe_cli_emit("token_usage", usage or {})
        except Exception:
            pass

    def _load_seed(self, task: str) -> str:
        """Seed this run from the chat's history file (Shell-use conversation).
        Returns the chat's ORIGINAL task — the transcript's opening turn carries
        it as <user_request=1> so the first objective survives resumes; the
        CURRENT task rides in the bridge slot's <user_request=N> tag. On any
        read failure the run starts fresh on the same chat."""
        original_task = task
        if not self.history_file:
            return original_task
        data = None
        try:
            with open(self.history_file, "r", encoding="utf-8") as fh:
                data = json.load(fh)
        except Exception:
            debug_log("shell history seed unreadable — starting fresh", "ERROR")
        if not isinstance(data, dict):
            return original_task
        original_task = data.get("task") or original_task
        # Commands the user hand-typed at the shared terminal since the last
        # run, pre-rendered by the parent as one <manual_mode> block. Read it
        # BEFORE the empty-history check — a fresh chat can carry one too.
        self._seed_manual_block = str(data.get("manual_mode") or "") or None
        seeded = data.get("assistant_messages") or []
        if not seeded:
            return original_task
        self.assistant_messages = [str(m) for m in seeded]
        tools = list(data.get("tool_responses") or [])[:len(self.assistant_messages)]
        tools += [None] * (len(self.assistant_messages) - len(tools))
        self.tool_responses = tools
        # The saved run ends on a bridge entry with an empty tool slot — fill it
        # with THIS request's numbered tag. This user turn IS the live request,
        # replayed in its chronological place (right after the previous run's
        # conclusion); the opening turn stays pinned to <user_request=1>.
        if self.tool_responses[-1] is None and _is_bridge_entry(self.assistant_messages[-1]):
            fill = f"<user_request={self.request_no}>\n{task}\n</user_request={self.request_no}>"
            if self._seed_manual_block:
                # The user's hand-typed commands sit in their chronological
                # place: after the previous run concluded, before the new
                # request. Embedded here they persist with the slot, so every
                # later resume still shows them — delivery is durable from
                # this moment even if no LLM call ever lands.
                fill = f"{self._seed_manual_block}\n\n{fill}"
                self._seed_manual_block = None
                self._manual_delivered = True
            self.tool_responses[-1] = fill
            self._seed_request_marker = fill
        debug_log(f"shell conversation resumed: {len(self.assistant_messages)} prior step(s)")
        return original_task

    def _write_history_snapshot(self):
        """Atomically rewrite the chat's history file with the CURRENT
        conversation state — called after every completed step and at run end,
        so even a hard-killed run reads back complete to its last finished step."""
        if not self.history_file:
            return
        try:
            payload = {
                "task": self._original_task,
                "assistant_messages": self.assistant_messages,
                "tool_responses": self.tool_responses,
                "last_messages": self.last_messages,
                # delivery ack for the seed's <manual_mode> block (see
                # _load_seed) — the parent re-queues the records if the run
                # dies before any snapshot carries this stamp
                "manual_delivered": self._manual_delivered,
            }
            tmp = self.history_file.with_suffix(".tmp")
            with open(tmp, "w", encoding="utf-8") as fh:
                json.dump(payload, fh, ensure_ascii=False)
            os.replace(tmp, self.history_file)
        except Exception:
            debug_log("shell history snapshot failed", "ERROR")

    def _finalize_history(self):
        """Flush the FINAL snapshot BEFORE the result file is written. The parent
        treats the result file as 'run over' and reads + deletes the history file
        right after — snapshotting any later would race that read (the last
        step would be missing from the saved chat) and the late os.replace could
        resurrect the just-deleted file as an orphan."""
        self._write_history_snapshot()
        self._history_finalized = True

    def _read_todo_from_file(self) -> str:
        """Read the current todo list from cli_todo/todo.md file"""
        try:
            todo_file = Path(self.controller.task_tracker.todo_file)
            if todo_file.exists():
                with open(todo_file, 'r', encoding='utf-8') as f:
                    return f.read().strip()
            else:
                return ""
        except Exception as e:
            return ""
    
    def _read_plan_from_file(self) -> str:
        """Render the current plan as a complete <plan no="N"> block for injection:
        the revision number rides on the tag (increments on every plan change) and
        each line is [N]-numbered like `view`. Returns "" when no plan exists yet."""
        try:
            rendered = self.controller.plan_service.render_plan()
            if not rendered:
                return ""
            plan_no = self.controller.plan_service.get_plan_no()
            return f'<plan no="{plan_no}">\n{rendered}\n</plan>'
        except Exception:
            return ""

    def _read_scratchpad_from_file(self) -> str:
        """Read the coder's durable scratchpad via controller.scratchpad_service
        (mirror of the minion's helper). Returns "" when no notes exist yet."""
        try:
            return self.controller.scratchpad_service.read_scratchpad()
        except Exception:
            return ""

    def _get_agent_sitting(self) -> str:
        """Workspace + current directory as PURE JSON (same convention as the
        <Tool_response> body — one machine-readable shape everywhere)."""
        return json.dumps({
            "your_workspace": str(self.controller.cli_service.sandbox.sandbox_root),
            "current_sitting": self.controller.cli_service.sandbox.get_cwd(),
        }, indent=2, ensure_ascii=False)
    
    def _print_start_banner(self):
        """One-time 'agent started' banner with provider/model — TTY only (cli.py /
        main.py). Skipped for the piped subprocess (app.py UI) so no protocol noise
        reaches the frontend reader."""
        if not sys.stdout.isatty():
            return
        bar = "─" * 44
        safe_print(f"\n{bar}")
        safe_print("🤖 AutoCua  ·  agent started")
        safe_print(f"   provider : {self.provider}")
        safe_print(f"   model    : {self.model}")
        safe_print(f"{bar}\n")

    def _working_spinner(self, stop_flag):
        """Cycle a '⚡ Working...' indicator while the LLM call blocks, so the terminal
        doesn't look frozen. TTY only — the `\\r` overwrite trick only works in a real
        terminal; in a pipe the partial writes would flood the frontend reader."""
        dots = ["", ".", "..", "..."]
        idx = 0
        while not stop_flag.is_set():
            sys.stdout.write(f"\r⚡ Working{dots[idx % len(dots)]:<3}")
            sys.stdout.flush()
            idx += 1
            stop_flag.wait(0.4)
        # Clear the spinner line so the step prints cleanly
        sys.stdout.write("\r" + " " * 24 + "\r")
        sys.stdout.flush()

    def process_request(self, task: str) -> str:
        """Run the agent loop synchronously. Output streams to stdout for the
        parent main agent's pill UI."""
        try:
            return self._run_agent_loop(task)
        finally:
            # Stop a dangling compression indicator on ANY exit path (a worker
            # still in flight deposits into a dead instance — harmless).
            self._compression.finish_run()
            # Shell-use conversation teardown (no-op otherwise): flush the final
            # snapshot — unless the exit/max-iterations path already finalized it
            # BEFORE writing the result file (writing again would race the
            # parent's read-and-delete of the history file).
            if not self._history_finalized:
                self._write_history_snapshot()

    def _run_agent_loop(self, task: str) -> str:
        """Main agentic loop"""

        self._print_start_banner()

        step_number = 0
        last_response = None
        is_first = True
        json_fail_count = 0  # Track consecutive JSON parse failures (max 3 before exit)
        reasoning_only_count = 0  # Consecutive steps that called `reasoning` and nothing else
        web_memory_index = None    # tool slot of the web step to backfill with the digest's scratchpad
        web_pending = None         # [(call_id, compacted dict)] of the web step, for the digest fold
        is_web_digest = False      # True only on the iteration that digests a web result

        # Conversation state — TWO parallel lists (main-driver style), replacing
        # the old (assistant, tool) tuple list, so agent_conversation can persist
        # them as-is. Shell use seeds them from the chat's history file; a seeded
        # run is NOT a first iteration — the dialogue already exists.
        self.assistant_messages = []
        self.tool_responses = []
        self._history_finalized = False   # fresh run on this instance
        original_task = self._load_seed(task)
        self._original_task = original_task
        if self.assistant_messages:
            is_first = False
        self._compression.reset()

        while True:
            # Check stop
            if self.stop_event and self.stop_event.is_set():
                safe_print("Agent stopped.")
                break

            # Splice a finished background compression IN PLACE (main-loop
            # thread only); snapshot immediately so a hard kill — and the
            # parent's save_run — pick up the spliced history.
            before = len(self.assistant_messages)
            web_memory_index = self._compression.apply_pending(
                self.assistant_messages, self.tool_responses, web_memory_index)
            if len(self.assistant_messages) != before:
                self._write_history_snapshot()

            step_number += 1
            is_web_digest = False  # set True below only when this iteration digests a web result

            # Check max iterations limit
            if step_number > MAX_CLI_ITERATIONS:
                safe_print(f"CLI Agent reached max iterations ({MAX_CLI_ITERATIONS})")
                
                # Read current todo to report what's done/pending
                todo_status = self._read_todo_from_file()
                summary = f"Max iterations ({MAX_CLI_ITERATIONS}) reached. Task incomplete. Todo status:\n{todo_status}"

                # Conversation snapshot BEFORE the result file (see _finalize_history)
                self._finalize_history()

                # Notify main agent with partial status
                if self.on_complete:
                    self.on_complete({
                        "task": self.task,
                        "summary": summary,
                        "status": "partial"
                    })

                return "Max iterations reached"
            
            # Step heartbeat → debug log only (NOT the UI). The card shows ONLY the agent's
            # own validated output; this "running step N" marker is ours, not the model's.
            debug_log(f"cli agent running step {step_number}")
            
            # Get agent sitting info
            agent_sitting = self._get_agent_sitting()
            
            # Read fresh todo + plan + scratchpad
            todo_list = self._read_todo_from_file()
            plan_doc = self._read_plan_from_file()
            scratchpad_notes = self._read_scratchpad_from_file()

            # Pending web result: rides into this step as its own user turn
            # (the raw result is never stored — the digest step's scratchpad
            # entries become its durable memory, folded in further below).
            pending_web = ""
            if not is_first and self._pending_web_response:
                pending_web = self._pending_web_response
                self._pending_web_response = ""
                is_web_digest = True

            # ── NATIVE TRANSCRIPT (the agent's memory) ────────────────────
            # APPEND-ONLY: system → opening user turn → for each past step, the
            # assistant turn carrying its OWN tool_calls, then one `role:
            # "tool"` result per call keyed by tool_call_id. The model sees
            # itself having called tools in the canonical shape every provider
            # was trained on, which is what keeps adherence high without
            # forcing tool_choice. Nothing earlier is ever rewritten, so the
            # prompt-cache prefix stays valid step after step.
            messages = [{"role": "system", "content": self.system_prompt}]
            # The opening turn is BYTE-STABLE for the whole CHAT, not just this
            # run: a resumed conversation opens with the chat's FIRST request
            # (<user_request=1>), so every follow-up EXTENDS the cached prefix
            # instead of rewriting its head. The LIVE request then sits in its
            # chronological place — the user turn that fills the previous
            # bridge's tool slot (_load_seed). A fresh run (or a seed whose
            # tail wasn't a bridge, so no slot exists) opens with the live
            # request itself. The workspace root is NOT repeated:
            # <agent_sitting> in the <persistent_memory> tail already carries
            # it (alongside the live cwd, which moves and so could never live
            # in a cached message).
            if self._seed_request_marker is not None:
                opening = f"<user_request=1>\n{original_task}\n</user_request=1>"
            else:
                opening = f"<user_request={self.request_no}>\n{task}\n</user_request={self.request_no}>"
                if self._seed_manual_block:
                    # No bridge slot took the block (fresh chat / degraded
                    # resume): the user's hand-typed commands ride the opening
                    # turn instead. Stable across iterations of THIS run, but
                    # never persisted — the parent re-queues the records if
                    # this run dies before its first step. A follow-up rebuilds
                    # the opening WITHOUT the block (one-time cache refresh) —
                    # accepted: by then the model already acted on it here.
                    opening = f"{self._seed_manual_block}\n\n{opening}"
            messages.append({"role": "user", "content": opening})

            for i, raw_entry in enumerate(self.assistant_messages):
                entry = decode_step(raw_entry)
                if (i == 0 and self._seed_request_marker is None
                        and original_task and original_task != task):
                    # Degraded resume (no bridge slot to carry the live request,
                    # so the head shows it instead): keep the chat's original
                    # objective visible on the first replayed turn.
                    entry["content"] = (f"<User_Task>\n{original_task}\n</User_Task>\n\n"
                                        + (entry["content"] or ""))
                asst = {"role": "assistant", "content": entry["content"] or ""}
                if entry["tool_calls"]:
                    asst["tool_calls"] = entry["tool_calls"]
                if entry.get("meta"):
                    # The provider's own metadata for this turn (Gemini 3
                    # thought signatures, OpenRouter reasoning blocks). The
                    # provider translates it back into its dialect; every
                    # other provider never sees the key (LLMManager strips it).
                    asst["provider_meta"] = entry["meta"]
                messages.append(asst)
                for result in decode_results(self.tool_responses[i]
                                                  if i < len(self.tool_responses) else None):
                    messages.append(result)

            # <persistent_memory>: live state re-sent EVERY step as one user
            # turn at the tail, read fresh from disk each iteration. Never
            # persisted into history (assistant_messages / tool_responses never
            # contain it) — exactly one copy exists per request. agent_sitting
            # is unconditional; plan/todo appear once they exist. A one-shot
            # pending web digest rides in the same turn but OUTSIDE the wrapper
            # (it is transient payload, not persistent state).
            pm = [f"<agent_sitting>\n{agent_sitting}\n</agent_sitting>"]
            if plan_doc:
                pm.append(plan_doc)
            if todo_list:
                pm.append(f"<todo_list>\n{todo_list}\n</todo_list>")
            if scratchpad_notes:
                pm.append(f"<scratchpad>\n{scratchpad_notes}\n</scratchpad>")
            content = "<persistent_memory>\n" + "\n\n".join(pm) + "\n</persistent_memory>"
            if pending_web:
                # The raw web result is shown ONCE, here, and never stored.
                # Tell the model to digest it now: the scratchpad entries it
                # records this step become the web call's durable `result`.
                content += (
                    "\n\n<critical>\nA web result arrived below. Digest it NOW: extract every "
                    "fact you will need later (numbers, dates, names, paths, URLs) and record "
                    "each as its own `scratchpad` call in this step. Those entries become the "
                    "web call's durable `result` in <agent_history>; the raw text below is "
                    "shown once and never stored, so anything you do not record is gone.\n"
                    f"</critical>\n\n{pending_web}")
            messages.append({"role": "user", "content": content})

            # Prompt caching: mark the newest PERSISTENT turn — the last tool
            # result (or the opening user turn on step 1). Never the
            # <persistent_memory> tail: it is rebuilt and dropped every
            # request, so a breakpoint there is written once and never read
            # back. The last tool result survives into the next request's
            # prefix, so each call reads the cache the previous one wrote.
            # OpenRouter forwards the parts-array marker to backends that
            # support it; the direct Anthropic translation lifts it onto the
            # tool_result block, and aws turns it into a Bedrock cachePoint.
            if self.provider in ("openrouter", "anthropic", "aws") and len(messages) > 2:
                for cache_msg in reversed(messages):
                    if cache_msg.get("role") not in ("tool", "user"):
                        continue
                    if cache_msg.get("role") == "user" and cache_msg is messages[-1]:
                        continue  # skip the ephemeral persistent_memory tail
                    content = cache_msg.get("content")
                    if isinstance(content, str) and content:
                        cache_msg["content"] = [{
                            "type": "text",
                            "text": content,
                            "cache_control": {"type": "ephemeral"},
                        }]
                    break

            # Exact payload snapshot — the history file carries it per step and
            # save_run renders it into memory_log.txt (the TRUE debug memory).
            self.last_messages = messages

            try:
                # Call LLM — show a working spinner on a TTY so the terminal doesn't
                # look frozen during the (multi-second) LLM wait.
                if sys.stdout.isatty():
                    spinner_stop = threading.Event()
                    spinner = threading.Thread(target=self._working_spinner, args=(spinner_stop,), daemon=True)
                    spinner.start()
                    try:
                        raw_response = self.llm.send_request(messages)
                    finally:
                        spinner_stop.set()
                        spinner.join(timeout=1)
                else:
                    raw_response = self.llm.send_request(messages)

                # Memory bar (Shell use only; no-op otherwise): forward this
                # call's exact token usage — same hook point as the main driver.
                self._emit_usage(self.llm.last_usage)

                # Opening-turn <manual_mode> (fresh chat / degraded resume,
                # where no bridge slot could persist it): the call that just
                # SUCCEEDED carried it, so only now is delivery real — a crash
                # before this point leaves the stamp False and the parent
                # re-queues the records.
                if self._seed_manual_block:
                    self._manual_delivered = True

                # Check stop after LLM
                if self.stop_event and self.stop_event.is_set():
                    break

                # Rolling compression trigger — context size is fresh in
                # last_usage, and the lists hold only completed steps here
                # (this step's entry is appended further below).
                self._compression.maybe_trigger(
                    self.assistant_messages, self.tool_responses, self._original_task)

                # NATIVE TOOL CALLING, two-channel turn: the
                # model's prose arrives on the TEXT channel and its actions as
                # real tool calls — nothing to parse, no envelope. The provider
                # already returned {"text", "tool_calls"}; convert the calls to
                # the exact action dicts route_action has always consumed, and
                # keep their ids so each result can be paired back to the call
                # that produced it in the next request.
                self._save_raw_response(
                    json.dumps(raw_response, indent=2, ensure_ascii=False, default=str),
                    step_number,
                )
                actions, calls, rejects, track = tool_calls_to_steps(
                    raw_response.get("tool_calls"),
                    allowed=CODER_TOOL_NAMES_CLAUDE if self._claude else None,
                    defaults_map=ACTION_DEFAULTS_CLAUDE if self._claude else None,
                    track_params=CLAUDE_REASONING_FIELDS if self._claude else REASONING_FIELDS)
                if self._claude:
                    # Claude's `assessment` fills the `thinking` slot downstream.
                    track["thinking"] = track.pop("assessment", "")
                resp_text = (raw_response.get("text") or "").strip()

                # `reasoning` is the step's note to itself, not an action. Its
                # fields are already harvested into `track`; pull the call out
                # before routing (the controller has no handler for it) but
                # keep it in `all_calls`, which is what the assistant turn
                # replays - the note is the reasoning the model reads back
                # later. Each note call is answered with the bare
                # REASONING_RESULT: every provider needs a result per call and
                # this is the smallest one; the snapshot and the compression
                # dump hide it.
                all_calls = calls
                note_ids = [c["id"] for a, c in zip(actions, calls) if a.get("type") == self._notes_tool]
                note_results = [{"tool_call_id": i, "content": REASONING_RESULT} for i in note_ids]
                kept = [(a, c) for a, c in zip(actions, calls) if a.get("type") != self._notes_tool]
                actions = [a for a, _ in kept]
                calls = [c for _, c in kept]

                # Backstop for an `async_tools` model (Astra): an `exit` bundled
                # after this turn's calls is dropped from the batch and answered
                # with an error result (see drop_bundled_terminal); the result
                # rides with the rejects below.
                exit_results = []
                if self.llm.async_tools():
                    actions, calls, exit_results = drop_bundled_terminal(actions, calls, rejects, "exit")
                    if exit_results and sys.stdout.isatty():
                        safe_print("⚠️ `exit` bundled with other calls: ignored, it must be the only call of its turn")

                if not actions and not rejects:
                    if note_ids:
                        # A reasoning-only step: the model chose to think
                        # before acting. Accepted as a normal step - the turn
                        # is recorded and its call answered like any other.
                        # Only a RUN of them is bounded: from the third in a
                        # row, the result carries a one-line reminder to act.
                        reasoning_only_count += 1
                        if reasoning_only_count >= 3:
                            note_results = [{"tool_call_id": i, "content": render_tool_response({
                                "status": "success", "action": self._notes_tool,
                                "message": f"{reasoning_only_count} steps in a row without an action: "
                                           "call a tool that does the work in this turn."})}
                                for i in note_ids]
                        normalized = encode_step(resp_text, wire_calls_from(all_calls),
                                                 raw_response.get("provider_meta"))
                        display_payload = json.dumps(
                            {"text": resp_text, "thinking": track["thinking"],
                             "memory": track["memory"], "next_goal": track["next_goal"],
                             "action": []},
                            indent=2, ensure_ascii=False)
                        if sys.stdout.isatty():
                            safe_print(CLIAgentResponseFormatter.format_terminal(display_payload))
                        else:
                            safe_print(CLIAgentResponseFormatter.format_stream_json(display_payload))
                        self._save_conversation_snapshot(messages, normalized, step_number)
                        self.assistant_messages.append(normalized)
                        self.tool_responses.append(encode_results(note_results))
                        self._write_history_snapshot()
                        is_first = False
                        continue
                    if resp_text:
                        # REPAIR, not exit: `exit` is an explicit dedicated
                        # final call (its completion rules ride on the tool
                        # itself), so a message with NO tool calls is always a
                        # dropped-calls mistake — the model narrated its move
                        # instead of making it (live: flash wrote the whole
                        # plan and called nothing; routing that as exit ended
                        # the run with the plan as the "answer"). Keep the
                        # turn so the thinking isn't lost, answer it with a
                        # corrective user turn, and let the model continue.
                        # The strike counter still bounds a model that keeps
                        # talking without acting.
                        json_fail_count += 1
                        if sys.stdout.isatty():
                            safe_print(f"⚠️ message had no tool calls — asking the model to act ({json_fail_count}/3)")
                        if json_fail_count >= 3:
                            break
                        normalized = encode_step(resp_text, [],
                                                 raw_response.get("provider_meta"))
                        display_payload = json.dumps(
                            {"text": resp_text, "action": []}, indent=2, ensure_ascii=False)
                        if sys.stdout.isatty():
                            safe_print(CLIAgentResponseFormatter.format_terminal(display_payload))
                        else:
                            safe_print(CLIAgentResponseFormatter.format_stream_json(display_payload))
                        self._save_conversation_snapshot(messages, normalized, step_number)
                        self.assistant_messages.append(normalized)
                        # Feedback in the SAME <Tool_response> envelope every
                        # real result uses — the environment reporting "nothing
                        # ran", not an instruction breaking the loop's rhythm.
                        self.tool_responses.append(
                            "<Tool_response>\n"
                            + json.dumps({"message": "no tool called"},
                                         indent=2, ensure_ascii=False)
                            + "\n</Tool_response>"
                        )
                        self._write_history_snapshot()
                        is_first = False
                        continue
                    else:
                        # Genuinely empty turn (no text, no calls). Retry — no
                        # seed is sent any more, so a retry is a fresh sample
                        # rather than the same response again.
                        json_fail_count += 1
                        if sys.stdout.isatty():
                            safe_print(f"⚠️ empty response from the model — retrying ({json_fail_count}/3)")
                        if json_fail_count >= 3:
                            break
                        step_number -= 1
                        continue

                json_fail_count = 0
                reasoning_only_count = 0

                # The assistant turn exactly as the model produced it: prose on
                # the text channel, its tool calls on the tool channel. This is
                # what gets replayed next step, so the model always sees its own
                # calls in the canonical shape. REJECTED calls are included too
                # — the turn must mirror what the model actually emitted, and
                # every result written below (errors included) has to pair with
                # a call of the same id or the next request is malformed.
                wire_calls = wire_calls_from(all_calls + rejects)
                normalized = encode_step(resp_text, wire_calls,
                                         raw_response.get("provider_meta"))

                # Display payload — the terminal print and the frontend's
                # tool-icon chain read `action`; the transcript does not.
                display_payload = json.dumps(
                    {"text": resp_text, "thinking": track["thinking"],
                     "memory": track["memory"], "next_goal": track["next_goal"],
                     "action": actions},
                    indent=2, ensure_ascii=False
                )

                # Real terminal (cli.py / main.py): print the model's message and
                # the tools it called. Piped subprocess (app.py UI mode, stdout
                # not a TTY): emit the JSON the frontend parses to drive the
                # action-icon chain + scratchpad.
                if sys.stdout.isatty():
                    safe_print(CLIAgentResponseFormatter.format_terminal(display_payload))
                else:
                    safe_print(CLIAgentResponseFormatter.format_stream_json(display_payload))

                # Save TRUE agent memory snapshot
                self._save_conversation_snapshot(messages, normalized, step_number)

                # Record the turn with an empty result slot, backfilled after
                # routing — so an `exit` turn also lands in the conversation.
                self.assistant_messages.append(normalized)
                self.tool_responses.append(None)

                # REPAIR: a call naming a tool
                # that doesn't exist is answered with an error tool result
                # instead of being dropped, so the model sees exactly what went
                # wrong — keyed to its own call id — and corrects on the very
                # next turn. Never a blind re-send of the same request.
                reject_results = [{"tool_call_id": r["id"], "content": f"error: {r['error']}"}
                                  for r in rejects]
                reject_results += exit_results
                if rejects:
                    if sys.stdout.isatty():
                        for r in rejects:
                            safe_print(f"⚠️ unknown tool '{r['name']}' — error returned to the model")
                    if not actions:
                        # Nothing valid to route: persist just the errors (and
                        # the note's result) and let the model correct itself
                        # on the next turn.
                        self.tool_responses[-1] = encode_results(note_results + reject_results)
                        self._write_history_snapshot()
                        is_first = False
                        continue

                # Actions came straight from the native tool calls
                action_block = actions

                # Route actions through controller
                action_result = self.controller.route_action(action_block)

                # Check if exit (CLI agent termination)
                if action_result.get("action") == "exit":
                    summary = action_result.get("summary", "Task completed")

                    # Close the call/result pairing before persisting: a tool
                    # call with no matching result is a malformed transcript and
                    # every provider rejects it, so a resumed conversation must
                    # never end on a dangling `exit` call.
                    if calls or note_results:
                        self.tool_responses[-1] = encode_results(
                            note_results + reject_results
                            + [{"tool_call_id": c["id"], "content": "run ended"} for c in calls]
                        )

                    # Conversation snapshot BEFORE the result file — the exit step
                    # is already in the lists, and the parent reads + deletes the
                    # history file the moment the result file appears (see
                    # _finalize_history).
                    self._finalize_history()

                    # Notify caller (main agent) if callback provided
                    if self.on_complete:
                        self.on_complete({
                            "task": self.task,
                            "summary": summary,
                            "status": "complete"
                        })

                    return "Exit"
                
                # Extract web tool response if present (before formatting last_response)
                web_tool_response = ""
                web_results_list = []
                if action_result.get("tool") == "web" and "result" in action_result:
                    web_results_list.append(action_result["result"])
                    del action_result["result"]  # Remove from action_result to avoid duplication
                elif action_result.get("action") == "multiple" and "results" in action_result:
                    for idx, result in enumerate(action_result["results"]):
                        if result.get("tool") == "web" and "result" in result:
                            web_results_list.append(result["result"])
                            del action_result["results"][idx]["result"]
                
                # Combine all web results with newlines, wrap in <tool> tag
                if web_results_list:
                    web_tool_response = "<tool>\n" + "\n".join(web_results_list) + "\n</tool>"
                    # Remember this web step's memory slot so the NEXT (digest)
                    # iteration can backfill it with the scratchpad findings.
                    web_memory_index = len(self.tool_responses) - 1
                
                # Store web response for next iteration
                self._pending_web_response = web_tool_response
                
                # ONE canonical tool-response shape (shared with minions): a
                # strictly-JSON metadata head + raw, clipped <output> blocks —
                # real newlines/indentation, per-tool truncation with steering
                # footers (see controller/cli/service.py render_tool_response).
                # Successful web entries are compacted first to a query +
                # scratchpad pointer (the raw result rides the <tool> block
                # once, then the digest step's findings fold back in below).
                compacted_result = self._tool_response_for_memory(action_result)

                # Web-result memory: on the digest step, fold the numbered
                # scratchpad findings the model JUST wrote into the web step's
                # slot as the web call's durable `result` (the raw web result
                # itself is never stored). ONLY the web call's own result is
                # re-rendered, on its own id; every other result of that step
                # (the reasoning note, a shell output) stays exactly as it was.
                if (is_web_digest and web_memory_index is not None
                        and 0 <= web_memory_index < len(self.tool_responses)
                        and self.tool_responses[web_memory_index]
                        and web_pending):
                    entries = [
                        str(a.get("value", "")).strip()
                        for a in (action_block or [])
                        if isinstance(a, dict) and a.get("type") == "scratchpad" and a.get("value")
                    ]
                    prior = decode_results(self.tool_responses[web_memory_index])
                    if entries and prior and all(m.get("tool_call_id") for m in prior):
                        numbered = "\n".join(f"{i + 1}. {e}" for i, e in enumerate(entries))
                        refreshed = {}
                        for call_id, d in web_pending:
                            self._fill_web_findings(d, numbered)
                            refreshed[call_id] = render_tool_response(d)
                        self.tool_responses[web_memory_index] = encode_results([
                            {"tool_call_id": m["tool_call_id"],
                             "content": refreshed.get(m["tool_call_id"], m.get("content"))}
                            for m in prior])
                    web_memory_index = None
                    web_pending = None

                # Backfill this step's results as `role: "tool"` messages, one
                # per call the model made, each keyed to that call's id. When a
                # batch of calls produced one combined envelope, the first id
                # carries it and the rest point at it — the pairing must stay
                # complete or providers reject the next request.
                per = (compacted_result.get("results")
                       if compacted_result.get("action") == "multiple" else None)
                if len(calls) <= 1:
                    pairs = [(calls[0]["id"] if calls else "call_0", compacted_result)]
                elif isinstance(per, list) and len(per) == len(calls):
                    pairs = [(calls[i]["id"], per[i]) for i in range(len(calls))]
                else:
                    pairs = [(calls[0]["id"], compacted_result)]
                action_results = [{"tool_call_id": i, "content": render_tool_response(d)}
                                  for i, d in pairs]
                if len(pairs) < len(calls):
                    action_results += [
                        {"tool_call_id": c["id"],
                         "content": "(result included in the first tool result of this batch)"}
                        for c in calls[1:]
                    ]
                if web_results_list:
                    # Stash the web call's own compacted dict(s), keyed to the
                    # id each was rendered on, for the digest step's fold.
                    web_pending = [(i, d) for i, d in pairs if self._has_web(d)] or None
                self.tool_responses[-1] = encode_results(note_results + reject_results + action_results)
                self._write_history_snapshot()

                is_first = False
                
            except Exception as e:
                safe_print(f"Error: {str(e)}")
                debug_log(f"CLI Agent loop error: {str(e)}", "ERROR")
                import traceback
                debug_log(f"CLI Agent traceback: {traceback.format_exc()}", "ERROR")
                break
        
        return "Agent loop ended"
