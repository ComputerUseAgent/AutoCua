"""
Minion Sub-Agent Service — read-only scout sub-agent loop (Linux).

Mirror of cli/service.py with three intentional differences:
  1. Loads minions/system_prompt.md (read-only scout prompt, next_goal blocks).
  2. Uses MINION_TOOLS via LLMManager(mode="minion") — native tool calling on the
     read-only subset; no write/replace/web/wait/todo.
  3. user_message injects <scratchpad> (per the minion prompt's <input> contract)
     instead of <todo_list> (which the CLI agent uses).

Everything else — the agent-loop shape, history threading, prompt caching, JSON
extraction, action routing through ControllerView, exit semantics — mirrors the
CLI agent so future maintenance can be done by reference.
"""

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

from ...tool_registry.service import (LLMManager, tool_calls_to_steps, MINION_TOOL_NAMES, drop_bundled_terminal,
                                         MINION_REASONING_FIELDS, CLAUDE_REASONING_FIELDS, ACTION_DEFAULTS_CLAUDE,
                                         MINION_TOOL_NAMES_CLAUDE)
from ...controller.view import ControllerView
from ...controller.cli.service import render_tool_response
from ..coder.view import (snapshot_turn, decode_step, encode_step,
                          decode_results, encode_results, wire_calls_from,
                          REASONING_RESULT)
from .view import MinionResponseFormatter

try:
    from app import debug_log, IS_COMPILED
except ImportError:
    def debug_log(msg, level="INFO"):
        pass
    IS_COMPILED = False


def _ensure_real_stdio_or_fallback():
    for name, fd in (('stdout', 1), ('stderr', 2)):
        stream = getattr(sys, name, None)
        if stream is not None and not (hasattr(stream, 'closed') and stream.closed):
            # Same as the coder: pin the pipe to UTF-8 with errors='replace' so a
            # char the locale encoding can't take never drops a whole step.
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
            setattr(sys, name, io.StringIO())

_ensure_real_stdio_or_fallback()


def safe_print(*args, **kwargs):
    """Print that won't crash if stdout is unavailable."""
    kwargs.setdefault("flush", True)
    try:
        print(*args, **kwargs)
    except (ValueError, OSError, AttributeError):
        msg = ' '.join(str(a) for a in args)
        debug_log(f"[PRINT] {msg}")


# Minion runs are bounded — its job is location-finding, not multi-step coding.
MAX_MINION_ITERATIONS = 100


# Appended to the system prompt for an `async_tools` model (GPT-6 Astra, see
# llm_provider/responses.py): its tool calls run after the turn ends, and the
# block tells it what that changes about the rules above it.
ASYNC_TOOLS_ADDENDUM = """
<async_tools>
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's <Tool_response>. The rules above are unchanged: one `reasoning` call first, then the calls that do the step, batched per <efficiency_guideline>. Because nothing comes back mid-turn:
- Never write a text message in a turn that contains tool calls, and never state that a probe succeeded or failed: that verdict belongs to the next step's memory, judged on the <Tool_response>.
- `exit` stays the only call of its turn. Never add it after this turn's calls: their results have not arrived, so nothing is verified yet.
</async_tools>
"""


class AgentService:
    """Minion Agent Service - read-only scout sub-agent loop."""

    def __init__(self, provider: str, model: str, save_conversation: bool = False,
                 api_key: str = None, stop_event=None,
                 task: str = None, on_complete: callable = None):

        self.provider = provider
        self.model = model
        self.save_conversation = save_conversation
        self.stop_event = stop_event
        self.task = task
        self.on_complete = on_complete

        self.session_id = uuid.uuid4().hex[:8]

        self.llm = LLMManager(
            provider=provider,
            model=model,
            api_key=api_key,
            cli_agent=True,
            mode="minion",
        )
        # Any Claude model (any provider) runs on claude_prompt.md, and its notes
        # call is `agent_memory`, with `assessment` in place of `thinking`.
        self._claude = self.llm.claude
        self._notes_tool = "agent_memory" if self._claude else "reasoning"

        self.controller = ControllerView(
            provider=provider, model=model,
            cli_mode=True, session_id=self.session_id,
            api_key=api_key,
            minion_mode=True,
        )

        self.formatter = MinionResponseFormatter

        self.system_prompt = self._load_system_prompt()
        # An `async_tools` model (GPT-6 Astra) runs its calls after the turn
        # ends; the block tells it what that changes.
        if self.llm.async_tools():
            self.system_prompt += ASYNC_TOOLS_ADDENDUM

        if self.save_conversation:
            self.conversation_dir = Path("cli_minion_conversation") / self.session_id
            self.conversation_dir.mkdir(parents=True, exist_ok=True)
            self.raw_reasoning_dir = self.conversation_dir / "raw_reasoning"
            self.raw_reasoning_dir.mkdir(parents=True, exist_ok=True)

        self.interaction_count = 0

    def _save_conversation_snapshot(self, messages: list, current_assistant_response: str, interaction_count: int):
        """Save TRUE agent memory — exactly what LLM receives at each step."""
        if not self.save_conversation:
            return

        conversation_file = self.conversation_dir / f"conversation_{interaction_count}.txt"

        with open(conversation_file, 'w', encoding='utf-8') as f:
            f.write("=== MINION MEMORY SNAPSHOT ===\n")
            f.write(f"Step: {interaction_count}\n")
            f.write(f"Time: {datetime.now()}\n")
            f.write("=" * 60 + "\n\n")

            # Every message as sent to the API, except the reasoning call's
            # placeholder result, which says nothing.
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

            entry = decode_step(current_assistant_response)
            f.write(f"=== CURRENT ASSISTANT RESPONSE (role='assistant') ===\n")
            f.write(snapshot_turn(entry["content"], entry["tool_calls"]))
            f.write("\n")

    def _save_raw_response(self, raw_response: str, step_number: int):
        """Save raw LLM response before any parsing/normalization (for debugging)."""
        if self.save_conversation:
            try:
                raw_file = self.raw_reasoning_dir / f"raw_response_{step_number}.txt"
                with open(raw_file, 'w', encoding='utf-8') as f:
                    f.write(raw_response)
            except Exception as e:
                safe_print(f"⚠ Error saving raw response: {str(e)}")

    def _load_system_prompt(self) -> str:
        """Load minion system prompt from sibling system_prompt.md
        (claude_prompt.md for Claude)."""
        current_dir = os.path.dirname(os.path.abspath(__file__))
        prompt_path = os.path.join(current_dir, "claude_prompt.md" if self._claude else "system_prompt.md")

        with open(prompt_path, 'r', encoding='utf-8') as f:
            return f.read()

    def _remove_agent_sitting_from_user_message(self, user_message: str) -> str:
        try:
            cleaned = re.sub(r'<agent_sitting>.*?</agent_sitting>', '', user_message, flags=re.DOTALL)
            cleaned = re.sub(r'\n{3,}', '\n\n', cleaned)
            return cleaned.strip()
        except Exception:
            return user_message

    def _read_scratchpad_from_file(self) -> str:
        """Read the minion's scratchpad (cli_minion/{session_id}/milestone.md)."""
        try:
            return self.controller.scratchpad_service.read_scratchpad()
        except Exception:
            return ""

    def _get_agent_sitting(self) -> str:
        """Workspace + current directory as PURE JSON (same convention as the
        <Tool_response> body and the coder's helper — one machine-readable
        shape everywhere, instead of loose `key: value` prose)."""
        return json.dumps({
            "your_workspace": str(self.controller.cli_service.sandbox.sandbox_root),
            "current_sitting": self.controller.cli_service.sandbox.get_cwd(),
        }, indent=2, ensure_ascii=False)

    def process_request(self, task: str) -> str:
        """Run the minion loop synchronously."""
        return self._run_agent_loop(task)

    def _run_agent_loop(self, task: str) -> str:
        """Main agentic loop — mirrors cli/service.py with scratchpad-in-user-message."""

        step_number = 0
        last_response = None
        is_first = True
        history = []
        json_fail_count = 0
        reasoning_only_count = 0  # Consecutive steps that called `reasoning` and nothing else

        while True:
            if self.stop_event and self.stop_event.is_set():
                safe_print("Minion stopped.")
                break

            step_number += 1

            if step_number > MAX_MINION_ITERATIONS:
                safe_print(f"Minion reached max iterations ({MAX_MINION_ITERATIONS})")

                scratchpad_status = self._read_scratchpad_from_file()
                summary = f"Max iterations ({MAX_MINION_ITERATIONS}) reached. Findings so far:\n{scratchpad_status}"

                if self.on_complete:
                    self.on_complete({
                        "task": self.task,
                        "summary": summary,
                        "status": "partial"
                    })

                return "Max iterations reached"

            # Step heartbeat → debug log only (NOT the UI). The row shows ONLY the minion's
            # own validated output; this "running step N" marker is ours, not the model's.
            debug_log(f"minion running step {step_number}")

            agent_sitting = self._get_agent_sitting()

            # Read fresh scratchpad — minion's prompt expects <scratchpad> in <input>,
            # NOT <todo_list> like the CLI agent.
            scratchpad_content = self._read_scratchpad_from_file()

            # <persistent_memory>: same wrapper contract as the coder — live
            # state, rebuilt fresh every step, never stored in history.
            # sitting always; scratchpad once entries exist (absent = none yet).
            pm = [f"<agent_sitting>\n{agent_sitting}\n</agent_sitting>"]
            if scratchpad_content:
                pm.append(f"<scratchpad>\n{scratchpad_content}\n</scratchpad>")
            # The request is NOT repeated here: it is stated once in the
            # opening turn below and never restated, so a long run carries
            # exactly one copy instead of one per step.
            user_message = "<persistent_memory>\n" + "\n\n".join(pm) + "\n</persistent_memory>"

            # ── NATIVE TRANSCRIPT (the minion's memory) ───────────────────
            # Identical in shape to the coder's: system → byte-stable opening
            # user turn carrying the request ONCE → for each past step, the
            # assistant turn carrying its OWN tool_calls followed by one
            # `role: "tool"` result per call keyed by tool_call_id → the live
            # state tail. The results are REAL tool messages, not
            # <Tool_response> text pasted into a user turn: native tool
            # calling is only native if the results come back on the tool
            # channel too, which is what the providers are trained on and what
            # makes generation faster than the JSON-envelope schema this
            # replaced. Append-only, so the cached prefix survives each step.
            # The request used to ride BOTH glued onto the front of assistant
            # turn 1 (as <User_Task> — the user's ask stuffed into an assistant
            # message) AND in the tail every step.
            messages = [{"role": "system", "content": self.system_prompt}]
            messages.append({
                "role": "user",
                "content": f"<user_request>\n{task}\n</user_request>",
            })

            for raw_entry, raw_results in history:
                entry = decode_step(raw_entry)
                asst = {"role": "assistant", "content": entry["content"] or ""}
                if entry["tool_calls"]:
                    asst["tool_calls"] = entry["tool_calls"]
                if entry.get("meta"):
                    # Provider metadata for this turn (Gemini 3 thought
                    # signatures, OpenRouter reasoning blocks) — same contract
                    # as the coder; see coder/view.py encode_step.
                    asst["provider_meta"] = entry["meta"]
                messages.append(asst)
                for result in decode_results(raw_results):
                    messages.append(result)

            messages.append({"role": "user", "content": user_message})

            # Prompt caching: mark the newest PERSISTENT turn — the last tool
            # result (or the opening user turn on step 1). Never the live tail:
            # it is rebuilt and dropped every request, so a breakpoint there is
            # written once and never read back. Same walk as the coder; the old
            # fixed messages[-2] index landed on whatever happened to sit there
            # (at step 1, the opening turn — rewriting it into a parts array and
            # invalidating the prefix on the very next step).
            if self.provider in ("openrouter", "anthropic", "aws") and len(messages) > 2:
                for cache_msg in reversed(messages):
                    if cache_msg.get("role") not in ("tool", "user"):
                        continue
                    if cache_msg.get("role") == "user" and cache_msg is messages[-1]:
                        continue  # skip the ephemeral live tail
                    content = cache_msg.get("content")
                    if isinstance(content, str) and content:
                        cache_msg["content"] = [{
                            "type": "text",
                            "text": content,
                            "cache_control": {"type": "ephemeral"},
                        }]
                    break

            try:
                raw_response = self.llm.send_request(messages)

                if self.stop_event and self.stop_event.is_set():
                    break

                # NATIVE TOOL CALLING: send_request returns the whole message
                # — {"text", "tool_calls"}. Convert the calls into the classic
                # envelope (thinking + labeled next_goal + action) so the
                # minion's text history, display and routing stay unchanged.
                # The MINION_TOOL_NAMES filter answers a hallucinated
                # coder-only call (write/replace/minion/...) with an error
                # result instead of ever routing it — the read-only guarantee
                # the old strict schema enforced structurally.
                self._save_raw_response(
                    json.dumps(raw_response, indent=2, ensure_ascii=False, default=str)
                    if isinstance(raw_response, dict) else str(raw_response),
                    step_number,
                )
                message = raw_response if isinstance(raw_response, dict) else {"content": str(raw_response)}
                actions, calls, rejects, track = tool_calls_to_steps(
                    message.get("tool_calls"),
                    allowed=MINION_TOOL_NAMES_CLAUDE if self._claude else MINION_TOOL_NAMES,
                    defaults_map=ACTION_DEFAULTS_CLAUDE if self._claude else None,
                    track_params=CLAUDE_REASONING_FIELDS if self._claude else MINION_REASONING_FIELDS)
                if self._claude:
                    # Claude's `assessment` fills the `thinking` slot downstream.
                    track["thinking"] = track.pop("assessment", "")
                resp_text = (message.get("text") or "").strip()
                step_meta = message.get("provider_meta") or {}

                # `reasoning` is the step's note to itself, not a probe (same
                # design as the coder). Its fields are already harvested into
                # `track`; pull the call out before routing but keep it in
                # `all_calls`, which is what the assistant turn replays. Each
                # note call is answered with the bare REASONING_RESULT (every
                # provider needs a result per call); the snapshot hides it.
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

                if not actions and not rejects and resp_text:
                    # LEAKED-ENVELOPE SALVAGE: flash-class models sometimes
                    # write the whole step as JSON TEXT instead of calling
                    # tools. If the text parses to a dict with an action
                    # array, adopt it (filtered to the read-only set) instead
                    # of discarding a perfectly usable step.
                    leaked = None
                    try:
                        leaked = json.loads(resp_text)
                    except (ValueError, TypeError):
                        extract = getattr(self.formatter, "_extract_json", None)
                        if extract:
                            leaked = extract(resp_text)
                    if isinstance(leaked, dict) and isinstance(leaked.get("action"), list):
                        salvaged = [a for a in leaked["action"]
                                    if isinstance(a, dict) and a.get("type") in MINION_TOOL_NAMES]
                        if salvaged:
                            actions = salvaged
                            track = {
                                "thinking": str(leaked.get("thinking") or "").strip() or track["thinking"],
                                "memory": str(leaked.get("memory") or "").strip() or track["memory"],
                                "next_goal": str(leaked.get("next_goal") or "").strip() or track["next_goal"],
                            }
                            resp_text = ""
                            if sys.stdout.isatty():
                                safe_print("⚠️ minion wrote its step as text — salvaged the action")

                # The assistant turn exactly as the model produced it, echoed
                # back next step in the canonical shape. REJECTED calls ride
                # along too: every result written below (errors included) must
                # pair with a call of the same id or the request is malformed.
                wire_calls = wire_calls_from(all_calls + rejects)
                reject_results = [{"tool_call_id": r["id"], "content": f"error: {r['error']}"}
                                  for r in rejects]
                reject_results += exit_results

                if not actions:
                    if rejects:
                        # Disallowed/unknown tool: keep the turn, answer each
                        # bad call with an error result keyed to ITS id, and
                        # let the minion correct itself on the next step.
                        if sys.stdout.isatty():
                            safe_print(f"⚠️ minion called disallowed tool '{rejects[0]['name']}' — error returned")
                        history.append((encode_step(resp_text, wire_calls, step_meta),
                                        encode_results(note_results + reject_results)))
                        is_first = False
                        continue
                    if note_ids:
                        # A reasoning-only step: the minion chose to think
                        # before probing. Accepted as a normal step - the turn
                        # is recorded and its call answered like any other.
                        # Only a RUN of them is bounded: from the third in a
                        # row, the result carries a one-line reminder to act.
                        reasoning_only_count += 1
                        if reasoning_only_count >= 3:
                            note_results = [{"tool_call_id": i, "content": render_tool_response({
                                "status": "success", "action": self._notes_tool,
                                "message": f"{reasoning_only_count} steps in a row without a probe: "
                                           "call a tool that does the work in this turn."})}
                                for i in note_ids]
                        safe_print(self.formatter.format_stream_json(json.dumps(
                            {"thinking": track["thinking"] or resp_text,
                             "memory": track["memory"], "next_goal": track["next_goal"],
                             "action": []},
                            indent=2, ensure_ascii=False)))
                        self._save_conversation_snapshot(
                            messages, encode_step(resp_text, wire_calls, step_meta), step_number)
                        history.append((encode_step(resp_text, wire_calls, step_meta),
                                        encode_results(note_results)))
                        is_first = False
                        continue
                    # No calls at all: talk-only or empty turn. Keep a
                    # talk-only turn and answer "no tool called"; retry an
                    # empty one — both bounded by the 3-strike counter.
                    json_fail_count += 1
                    if sys.stdout.isatty():
                        safe_print(f"⚠️ minion produced no tool call — asking it to act ({json_fail_count}/3)")
                    if json_fail_count >= 3:
                        break
                    if resp_text:
                        # No calls means no call id to key a tool result to, so
                        # the nudge rides a plain user turn (decode_results
                        # replays a bare string as exactly that).
                        history.append((
                            encode_step(resp_text, [], step_meta),
                            "<Tool_response>\n"
                            + json.dumps({"message": "no tool called"},
                                         indent=2, ensure_ascii=False)
                            + "\n</Tool_response>"))
                        is_first = False
                    else:
                        step_number -= 1
                    continue

                json_fail_count = 0
                reasoning_only_count = 0

                # Display payload — the minion row's frontend reads `action`
                # for its sub-chain; the transcript above does not.
                normalized = json.dumps(
                    {"thinking": track["thinking"] or resp_text,
                     "memory": track["memory"], "next_goal": track["next_goal"],
                     "action": actions},
                    indent=2, ensure_ascii=False)

                # Stream the validated, complete response (action included) to the minion row.
                # The frontend parses the `action` array out of THIS JSON to drive the row's
                # own action sub-chain — no extra backend events needed.
                safe_print(self.formatter.format_stream_json(normalized))

                self._save_conversation_snapshot(
                    messages, encode_step(resp_text, wire_calls, step_meta), step_number)

                action_block = actions

                action_result = self.controller.route_action(action_block)

                if action_result.get("action") == "exit":
                    summary = action_result.get("summary", "Findings ready.")

                    # Close the call/result pairing before returning: a tool
                    # call with no matching result is a malformed transcript.
                    if calls or note_results:
                        history.append((encode_step(resp_text, wire_calls, step_meta),
                                        encode_results(note_results + reject_results + [
                                            {"tool_call_id": c["id"], "content": "run ended"}
                                            for c in calls])))

                    if self.on_complete:
                        self.on_complete({
                            "task": self.task,
                            "summary": summary,
                            "status": "complete"
                        })

                    return "Exit"

                # ONE canonical tool-response shape, identical to the coder's:
                # a strictly-JSON metadata head plus the raw output in its own
                # <output lines="A-B of TOTAL"> block. This replaces the old
                # per-tool hand-formatted <shell>/<view>/<grep>/<glob> tags,
                # which had three problems: the minion's envelope looked
                # nothing like the coder's, every tool needed its own branch
                # (and its own `multiple` branch again), and the output was
                # pasted in UNCLIPPED — one wide grep could bury the whole
                # context. render_tool_response applies the per-tool line/char
                # budgets with a steering footer when it truncates.
                last_response = render_tool_response(action_result)

                # One `role: "tool"` result per call the model made, each keyed
                # to that call's id. When a batch produced one combined
                # envelope, the first id carries it and the rest point at it —
                # the pairing must stay complete or providers reject the turn.
                if len(calls) <= 1:
                    action_results = [{
                        "tool_call_id": calls[0]["id"] if calls else "call_0",
                        "content": last_response,
                    }]
                else:
                    per = (action_result.get("results")
                           if action_result.get("action") == "multiple" else None)
                    if isinstance(per, list) and len(per) == len(calls):
                        action_results = [{"tool_call_id": calls[i]["id"],
                                           "content": render_tool_response(per[i])}
                                          for i in range(len(calls))]
                    else:
                        action_results = [{"tool_call_id": calls[0]["id"], "content": last_response}] + [
                            {"tool_call_id": c["id"],
                             "content": "(result included in the first tool result of this batch)"}
                            for c in calls[1:]
                        ]

                history.append((encode_step(resp_text, wire_calls, step_meta),
                                encode_results(note_results + reject_results + action_results)))

                is_first = False

            except Exception as e:
                safe_print(f"Error: {str(e)}")
                debug_log(f"Minion loop error: {str(e)}", "ERROR")
                import traceback
                debug_log(f"Minion traceback: {traceback.format_exc()}", "ERROR")
                break

        # The loop ended WITHOUT an exit call (3 strikes / stop / error).
        # ALWAYS report back — the parent agent is blocked waiting on this
        # minion's result; a silent death would hang it forever. Deliver
        # whatever the scratchpad already holds as a partial report.
        scratchpad_status = self._read_scratchpad_from_file()
        summary = ("Minion ended without a final report. Findings so far:\n"
                   f"{scratchpad_status or 'none'}")
        if self.on_complete:
            self.on_complete({
                "task": self.task,
                "summary": summary,
                "status": "partial"
            })

        return "Agent loop ended"
