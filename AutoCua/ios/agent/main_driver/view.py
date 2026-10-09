import json
import re
import os
import time
from pathlib import Path

from ....memory_compression.agent.service import wrap_handoff, extract_handoff


# ---------------------------------------------------------------------------
# NATIVE TRANSCRIPT CODEC - main-driver flavor. The notes ride on a
# dedicated call: `reasoning` {thinking, memory, next_goal} in quality mode
# (the macOS driver's design), `memory` {next_goal} in fast mode; older saves
# carry them as arguments on the action calls, and the renderers read both.
# Legacy
# tool slots are re-wrapped in <tool_response> so schema-era saves replay
# byte-identically to the old builder.
#
# A step is persisted as two aligned strings: the assistant turn (its prose
# plus the tool_calls it made) and the matching results - both JSON, so the
# turn is rebuilt EXACTLY on the next request. That is the whole point of
# native tool calling: the model sees itself having called tools in the shape
# every provider was trained on, not a text transcript describing that it did.
# Anything that ISN'T our JSON - a bridge note, a legacy "<Step_no=N />\n{...}"
# step, a request marker - degrades to plain text so resumed conversations
# keep replaying.
# ---------------------------------------------------------------------------

def decode_step(raw) -> dict:
    """Persisted assistant turn -> {"content": str, "tool_calls": list,
    "meta": dict}. `meta` is the provider's own opaque per-turn metadata (see
    encode_step); it is {} for turns saved before it existed and for providers
    that emit none, so old history replays exactly as before."""
    try:
        data = json.loads(str(raw))
    except Exception:
        return {"content": str(raw or ""), "tool_calls": [], "meta": {}}
    if not isinstance(data, dict):
        return {"content": str(raw or ""), "tool_calls": [], "meta": {}}
    calls = data.get("tool_calls")
    meta = data.get("provider_meta")
    if isinstance(calls, list) and ("content" in data or calls):
        return {"content": str(data.get("content") or ""), "tool_calls": calls,
                "meta": meta if isinstance(meta, dict) else {}}
    # Legacy 4-block schema step (or a bridge note) - replay its prose.
    return {"content": str(raw or ""), "tool_calls": [], "meta": {}}


def encode_step(text: str, calls: list, meta: dict = None) -> str:
    """Assistant turn -> persisted string. `calls` are OpenAI-shaped:
    {"id", "type": "function", "function": {"name", "arguments": json}}.

    `meta` is the PROVIDER's opaque per-turn metadata, carried so the turn can
    be echoed back exactly as the model produced it: Gemini 3 thought
    signatures, OpenRouter reasoning blocks. Reasoning-model tool calling
    breaks without it. Written only when non-empty, and each provider tags +
    checks its own name, so a resumed chat can never hand one provider
    another's blob."""
    data = {"content": text or "", "tool_calls": calls or []}
    if meta:
        data["provider_meta"] = meta
    return json.dumps(data, ensure_ascii=False)


# Slot strings that must replay BARE (not re-wrapped): run-boundary request
# markers, already-wrapped legacy results, and the compression note.
_BARE_SLOT_PREFIXES = ("<updated_user_request", "<tool_response", "<history_compressed")


def decode_results(raw) -> list:
    """Persisted results -> the `role: "tool"` messages for one step. A legacy
    plain string replays as a USER turn; unless it is a request marker (or
    already wrapped), it is re-wrapped in <tool_response> - exactly how the
    old builder replayed schema-era tool slots."""
    if not raw:
        return []
    try:
        data = json.loads(str(raw))
    except Exception:
        data = None
    if isinstance(data, list) and all(isinstance(d, dict) and "tool_call_id" in d for d in data):
        return [{"role": "tool", "tool_call_id": d["tool_call_id"],
                 "content": str(d.get("content") or "")} for d in data]
    text = str(raw)
    if not text.lstrip().startswith(_BARE_SLOT_PREFIXES):
        text = f"<tool_response>\n{text}\n</tool_response>"
    return [{"role": "user", "content": text}]


def encode_results(results: list) -> str:
    """[{"tool_call_id", "content"}] -> persisted string."""
    return json.dumps(results or [], ensure_ascii=False)


def wire_calls_from(calls: list) -> list:
    """Normalized calls -> the OpenAI-shaped `tool_calls` the assistant turn
    carries on the wire. Arguments are re-serialized EXACTLY as the model sent
    them (tracking params included) so the echoed transcript never contradicts
    the schema that required them."""
    return [{
        "id": c["id"],
        "type": "function",
        "function": {"name": c["name"],
                     "arguments": json.dumps(c.get("arguments") or {}, ensure_ascii=False)},
    } for c in (calls or [])]


# What the snapshot shows for `thinking` on a step that had none. The model
# never writes this word: a route step omits the field (or passes ""), and the
# renderer fills it in so a reader can tell "nothing thought, on purpose" from
# "nothing recorded".
SKIPPED_THINKING = "skipped"

# The `reasoning` call's result on the wire. Every provider needs a result for
# every call; this is the smallest one. The snapshot and the compression dump
# hide it.
REASONING_RESULT = "ok"


def snapshot_turn(text: str, calls: list) -> str:
    """DISPLAY-ONLY rendering of one native assistant turn as the familiar
    block shape {"thinking", "memory", "next_goal", "action"}. The notes are
    pulled OUT of the call arguments (the `reasoning` call in quality mode,
    the `memory` call in fast mode, the first action call in older saves) and
    shown as their own fields; the notes call itself is NOT listed under
    action, and a `thinking` the `reasoning` call left out renders as
    "skipped". Claude's `assessment` renders in the thinking slot. The API
    payload is untouched: the model keeps receiving the native transcript.

    A param the step's MODE never carried is omitted entirely rather than
    rendered blank - fast mode has no `thinking`, and printing an empty one
    would read as a dropped block. A turn with no calls at all (a compression
    handoff, the no-tool-called repair turn) keeps all three so its prose
    still has somewhere to land."""
    actions = []
    values = {"thinking": "", "memory": "", "next_goal": ""}
    present = set()
    for tc in calls or []:
        fn = (tc or {}).get("function") or {}
        args = fn.get("arguments")
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except (ValueError, TypeError):
                args = {"raw_arguments": args}
        if not isinstance(args, dict):
            args = {}
        args = dict(args)
        name = fn.get("name") or ""
        # `agent_memory` is Claude's notes tool; `notes` is fast mode's old
        # name for `memory`, still in older saves.
        is_note = name in ("reasoning", "agent_memory", "memory", "notes")
        if name in ("reasoning", "agent_memory"):
            # The `reasoning` call carries all three by contract; an omitted
            # `thinking` is a route step and must still render (as "skipped").
            # The fast `memory` call carries `next_goal` alone, which the loop
            # below picks up, so fast turns keep omitting `thinking`.
            present.update(values)
        # Pop UNCONDITIONALLY, then keep the first non-empty value - later
        # calls carry "" and must not render as bogus action fields.
        for key in values:
            if key in args:
                present.add(key)
            step_value = str(args.pop(key, "") or "").strip()
            values[key] = values[key] or step_value
        # Claude's `assessment`, popped the same way, fills the thinking slot.
        if "assessment" in args:
            present.add("thinking")
        step_assessment = str(args.pop("assessment", "") or "").strip()
        values["thinking"] = values["thinking"] or step_assessment
        if is_note:
            continue   # its whole content is the note fields above
        actions.append({"type": fn.get("name") or "", **args})
    if not calls:
        present = set(values)
    rendered = {}
    if "thinking" in present:
        rendered["thinking"] = values["thinking"] or (text or "").strip() or SKIPPED_THINKING
    if "memory" in present:
        rendered["memory"] = values["memory"]
    if "next_goal" in present:
        rendered["next_goal"] = values["next_goal"]
    rendered["action"] = actions
    return json.dumps(rendered, indent=2, ensure_ascii=False)


# ---------------------------------------------------------------------------
# HANDOFF COMPRESSION (main-driver flavor) - the two callables
# CompressionController takes, replacing memory_compression's schema-hardcoded
# defaults, which cannot read native entries. Handles MIXED histories: legacy
# schema-era steps render like the old default builder, native steps render via
# snapshot_turn - so the compressor prompt's existing grammar needs no edit.
# ---------------------------------------------------------------------------

_STEP_PREFIX_RE = re.compile(r"(<Step_no=\d+ />\n)(.*)", re.DOTALL)

# Tool slot paired with the synthetic handoff entry: a plain string that
# replays as a bare user turn (see _BARE_SLOT_PREFIXES) - never an orphaned
# role:"tool" result, never two adjacent assistant turns.
_COMPRESSION_NOTE = ("<history_compressed>\nEarlier steps of this session were "
                     "compressed into the handoff document above. Trust it as "
                     "accurate memory and continue from the state it describes.\n"
                     "</history_compressed>")


def _clip(value, limit=300):
    s = str(value)
    return s if len(s) <= limit else s[:limit] + f"... [clipped {len(s) - limit} chars]"


def _looks_native(raw) -> bool:
    try:
        d = json.loads(str(raw))
    except Exception:
        return False
    return isinstance(d, dict) and "content" in d and isinstance(d.get("tool_calls"), list)


def _legacy_dump_body(raw_entry) -> str:
    """A schema-era entry's JSON body, with any '<Step_no=N />' prefix removed
    (the dump stamps its own renumbered prefix)."""
    m = _STEP_PREFIX_RE.match(str(raw_entry))
    return m.group(2) if m else str(raw_entry)


def _dump_step(raw_entry, full: bool) -> str:
    """One persisted assistant entry -> compressor-readable text. Native turns
    render via snapshot_turn ({thinking, memory, next_goal, action}); legacy
    turns mirror the old default builder (older steps keep only
    {memory, next_goal}; the last step stays full). Bridge notes and other
    non-JSON prose replay verbatim."""
    step = decode_step(raw_entry)
    if step["tool_calls"] or _looks_native(raw_entry):
        rendered = json.loads(snapshot_turn(step["content"], step["tool_calls"]))
        if not full:
            rendered["action"] = [{key: _clip(val) for key, val in action.items()}
                                  for action in rendered.get("action", [])]
        return json.dumps(rendered, indent=2, ensure_ascii=False)
    body = _legacy_dump_body(raw_entry)
    if full:
        return body
    try:
        data = json.loads(body)
    except Exception:
        return body
    if not isinstance(data, dict):
        return body
    for key in ("thinking", "eval", "action"):
        data.pop(key, None)
    return json.dumps(data, indent=2, ensure_ascii=False)


def _prior_handoff(entry0):
    """A previous compression's handoff doc, whichever generation wrote it:
    native synthetic entries carry it in their CONTENT; legacy ones carried it
    in the step's `memory` field."""
    step = decode_step(entry0)
    if step["tool_calls"] or _looks_native(entry0):
        return extract_handoff(step["content"])
    try:
        data = json.loads(_legacy_dump_body(entry0))
    except Exception:
        return None
    if isinstance(data, dict):
        return extract_handoff(str(data.get("memory") or ""))
    return None


def compression_dump(assistant_messages, tool_responses, k, task) -> str:
    """Main-driver flavor of memory_compression's build_dump: steps [0..k] in
    the compressor prompt's <input> format, mixed-generation aware.

    MAIN-THREAD ONLY: snapshots the list content into one string so the worker
    thread never reads the live lists (same contract as the default's)."""
    entries = [str(e) for e in assistant_messages[:k + 1]]
    tools = list(tool_responses[:k + 1])
    tools += [None] * (len(entries) - len(tools))

    out = [
        "Session: live (in-run rolling compression)",
        f"Task: {task}",
        "Trigger: rolling",
    ]

    # PREVIOUS HANDOFF: entry 0 is a prior synthetic entry when it carries a
    # <handoff> - lift the doc into its own section and skip the entry (and its
    # boilerplate note slot) so it isn't summarized twice.
    start = 0
    prev = _prior_handoff(entries[0]) if entries else None
    if prev is not None:
        out += ["", "=== PREVIOUS HANDOFF ===", prev]
        start = 1

    out += ["", "--- USER ---",
            f'<updated_user_request no="1">\n{task}\n</updated_user_request no="1">']

    step_no = 0
    last = len(entries) - 1
    for i in range(start, len(entries)):
        step_no += 1
        out += ["", "--- ASSISTANT ---", f"<Step_no={step_no} />",
                _dump_step(entries[i], full=(i == last))]
        tr = tools[i]
        if not tr:
            continue
        results = decode_results(tr)
        if results and results[0].get("role") == "tool":
            # The `reasoning` call's bare `ok` is wire plumbing, not a result.
            contents = [str(r.get("content") or "") for r in results
                        if str(r.get("content") or "") != REASONING_RESULT]
            if not contents:
                continue
            out += ["", "--- USER ---",
                    "<tool_response>\n" + "\n".join(contents) + "\n</tool_response>"]
        else:
            raw = str(tr)
            if raw.lstrip().startswith("<updated_user_request"):
                step_no = 0   # step numbering restarts after every new task
                out += ["", "--- USER ---", raw]
            elif raw.lstrip().startswith("<tool_response"):
                out += ["", "--- USER ---", raw]
            else:
                out += ["", "--- USER ---", f"<tool_response>\n{raw}\n</tool_response>"]
    return "\n".join(out) + "\n"


def compression_entry(step_k_entry, handoff_text):
    """(entry, tool_slot) replacing entries [0..k] after a handoff lands: ONE
    content-only native assistant turn carrying the wrapped handoff, paired
    with the plain-string note slot. step_k_entry is unused - signature parity
    with the controller's default."""
    return encode_step(wrap_handoff(handoff_text), []), _COMPRESSION_NOTE


class AgentResponseFormatter:
    """Formats agent JSON responses for terminal display"""

    FIELD_EMOJIS = {
        "assessment": "🧠 Assessment",
        "thinking": "🧠 Thinking",
        "next_goal": "🎯 Next Goal",
        "memory": "💾 Memory",
        "action": "⚡ Action"
    }

    @staticmethod
    def extract_tools(normalized_response: str) -> list:
        """Pull this turn's tools from the parsed action block, in execution order,
        for the frontend tool-flow chain. e.g.
            [{"name": "left_click", "clicks": 2}, {"name": "input"}, {"name": "web"}]
        """
        tools = []
        try:
            data = json.loads(normalized_response)
            actions = data.get("action", [])
            if isinstance(actions, dict):
                actions = [actions]
            for item in actions:
                if not isinstance(item, dict):
                    continue
                name = item.get("type")
                if not name:
                    continue
                tool = {"name": name}
                if "clicks" in item:
                    tool["clicks"] = item.get("clicks")
                if "direction" in item:
                    tool["direction"] = item.get("direction")
                # video_player's sub-command (play/pause/close/streaming) picks
                # the media sign in the frontend tool chain.
                if name == "video_player" and "value" in item:
                    tool["value"] = item.get("value")
                tools.append(tool)
        except Exception:
            pass
        return tools

    @staticmethod
    def format_response(normalized_response: str, include_action: bool = False) -> str:
        """Format normalized JSON response into readable terminal output with emojis.
        include_action: If True, include the action block (for terminal). If False, omit it (for frontend stream).
        """
        try:
            # Parse clean JSON string directly (no markdown wrapper)
            data = json.loads(normalized_response)

            # Build formatted output
            lines = []
            for field, emoji_label in AgentResponseFormatter.FIELD_EMOJIS.items():
                if field in data:
                    value = data[field]

                    # Skip action field unless include_action (frontend should not stream action)
                    if field == "action" and not include_action:
                        continue

                    # Convert dict/list to string for other fields
                    if isinstance(value, (dict, list)):
                        value = json.dumps(value, indent=2)
                    lines.append(f"- {emoji_label}: {value}")

            return "\n".join(lines)

        except Exception:
            # If any error, return original response
            return normalized_response
