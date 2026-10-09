import json
import re
import time
from pathlib import Path

from ....memory_compression.agent.service import extract_handoff


# ═════════════════════════════════════════════════════════════════════════════
# NATIVE TRANSCRIPT CODEC — shared by the coder and the minion.
#
# A step is persisted as two aligned strings (the shape agent_conversation and
# the Telegram bridge already read): the assistant turn — its prose plus the
# tool_calls it made — and the matching results. Both are JSON so the turn can
# be rebuilt EXACTLY on the next request, which is the whole point of native
# tool calling: the model sees itself having called tools in the canonical
# shape every provider was trained on, not a text transcript describing that it
# did. Anything that ISN'T our JSON (a bridge note, a legacy envelope step, a
# request marker) degrades to plain text so resumed conversations keep
# replaying.
# ═════════════════════════════════════════════════════════════════════════════

def decode_step(raw) -> dict:
    """Persisted assistant turn -> {"content": str, "tool_calls": list,
    "meta": dict}. `meta` is the provider's own opaque per-turn metadata (see
    encode_step); it is {} for every turn saved before it existed and for
    providers that emit none, so old history replays exactly as before."""
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
    # Legacy 4-block envelope step (or a bridge note) — replay its prose.
    return {"content": str(raw or ""), "tool_calls": [], "meta": {}}


def encode_step(text: str, calls: list, meta: dict = None) -> str:
    """Assistant turn -> persisted string. `calls` are OpenAI-shaped:
    {"id", "type": "function", "function": {"name", "arguments": json}}.

    `meta` is the PROVIDER's opaque per-turn metadata, carried so the turn can
    be echoed back exactly as the model produced it: Gemini 3 thought
    signatures, OpenRouter reasoning blocks. Reasoning-model tool calling
    breaks without it — Gemini 3 binds a function RESULT to the call through
    the signature on that call, so a rebuilt-from-scratch turn makes the model
    read its tool output as empty. Written only when non-empty, and each
    provider tags + checks its own name, so nothing changes for providers that
    emit none and a resumed chat can never hand one provider another's blob."""
    data = {"content": text or "", "tool_calls": calls or []}
    if meta:
        data["provider_meta"] = meta
    return json.dumps(data, ensure_ascii=False)


def decode_results(raw) -> list:
    """Persisted results -> the `role: "tool"` messages for one step. A legacy
    plain string replays as a user turn so old saves still work."""
    if not raw:
        return []
    try:
        data = json.loads(str(raw))
    except Exception:
        data = None
    if isinstance(data, list) and all(isinstance(d, dict) and "tool_call_id" in d for d in data):
        return [{"role": "tool", "tool_call_id": d["tool_call_id"],
                 "content": str(d.get("content") or "")} for d in data]
    return [{"role": "user", "content": str(raw)}]


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


# The compressed-memory entry: after a handoff lands, steps [0..k] are replaced
# by ONE assistant turn whose content is {"memory_compression": "<document>"}
# and nothing else - no thinking, no memory, no next_goal.
_COMPRESSION_KEY = "memory_compression"


# What the snapshot shows for `thinking` on a step that had none. The model
# never writes this word: a route step omits the field (or passes ""), and the
# renderer fills it in so a reader can tell "nothing thought, on purpose" from
# "nothing recorded".
SKIPPED_THINKING = "skipped"


def snapshot_turn(text: str, calls: list) -> str:
    """DISPLAY-ONLY rendering of one assistant turn as ONE JSON:
    {"thinking", "memory", "next_goal", "action": [{type, ...}]}. The
    `reasoning` call is folded into the first three fields and is NOT listed
    under action; the action calls are listed in order. Older saves that
    carried the notes as arguments on every call render the same way (the
    notes are pulled out of the arguments). `thinking` renders as "skipped"
    when the step omitted it (a route step) and wrote no message: the word is
    written HERE, never by the model, whose own text is shown whenever there
    is any. Claude's `assessment` renders in the thinking slot. A
    compressed-memory entry renders as itself. The API payload is untouched:
    the model keeps receiving the native transcript."""
    if not calls:
        try:
            data = json.loads(str(text or ""))
        except Exception:
            data = None
        if isinstance(data, dict) and _COMPRESSION_KEY in data:
            return json.dumps(data, indent=2, ensure_ascii=False)
    actions = []
    thinking = memory = next_goal = ""
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
        # Pop UNCONDITIONALLY, then keep the first non-empty value. Folding the
        # pop into `thinking = thinking or ...` short-circuits once thinking is
        # set, so every call after the first kept its (empty) tracking params
        # and rendered them as bogus action fields.
        step_thinking = str(args.pop("thinking", "") or "").strip()
        step_assessment = str(args.pop("assessment", "") or "").strip()
        step_memory = str(args.pop("memory", "") or "").strip()
        step_next_goal = str(args.pop("next_goal", "") or "").strip()
        thinking = thinking or step_thinking or step_assessment
        memory = memory or step_memory
        next_goal = next_goal or step_next_goal
        if (fn.get("name") or "") in ("reasoning", "agent_memory"):
            continue   # its whole content is the three fields above
        actions.append({"type": fn.get("name") or "", **args})
    return json.dumps(
        {"thinking": thinking or (text or "").strip() or SKIPPED_THINKING,
         "memory": memory, "next_goal": next_goal, "action": actions},
        indent=2, ensure_ascii=False)


# ═════════════════════════════════════════════════════════════════════════════
# HANDOFF COMPRESSION (coder flavor) — the two callables CompressionController
# takes to serve the coder's NATIVE history: compression_dump renders steps
# [0..k] of the parallel lists into the compressor prompt's <input> format, and
# compression_entry builds the splice that replaces them. They live HERE
# because this module owns the native codec; memory_compression stays
# platform-neutral and must never import a platform coder module.
# ═════════════════════════════════════════════════════════════════════════════

# The seed/bridge fill writes `<user_request=N>` markers into tool slots; the
# compressor prompt's grammar knows them as <updated_user_request no="N">.
_USER_REQ_RE = re.compile(r"<user_request=(\d+)>\n?(.*?)\n?</user_request=\1>", re.DOTALL)

# The `reasoning` call's result on the wire. Every provider needs a result for
# every call, so it cannot be dropped; this is the smallest one. It carries no
# information, so the snapshot file and the compression dump hide it.
REASONING_RESULT = "ok"

# Tool slot paired with the synthetic handoff entry: a plain string, so
# decode_results replays it as a USER turn — the splice never orphans a
# role:"tool" result and never leaves two assistant turns adjacent.
_COMPRESSION_NOTE = ("<history_compressed>\nEarlier steps of this session were "
                     "compressed into the memory_compression block above. Trust it "
                     "as accurate memory and continue from the state it describes.\n"
                     "</history_compressed>")


def compressed_memory(content):
    """The handoff document inside a compressed-memory entry's content, else
    None. Reads the {"memory_compression": ...} shape, and the older
    <handoff> tag for chats saved before it."""
    try:
        data = json.loads(str(content))
    except Exception:
        data = None
    if isinstance(data, dict) and isinstance(data.get(_COMPRESSION_KEY), str):
        return data[_COMPRESSION_KEY]
    return extract_handoff(content)


def _clip(value, limit=300):
    s = str(value)
    return s if len(s) <= limit else s[:limit] + f"... [clipped {len(s) - limit} chars]"


def _looks_native(raw) -> bool:
    try:
        d = json.loads(str(raw))
    except Exception:
        return False
    return isinstance(d, dict) and "content" in d and isinstance(d.get("tool_calls"), list)


def _dump_step(raw_entry, full: bool) -> str:
    """One persisted assistant entry -> compressor-readable text. Native turns
    render via the snapshot_turn convention ({thinking, memory, next_goal, action});
    older steps clip long action args (write bodies etc.) so verbatim commands
    and paths survive while file bodies don't. Bridge notes / legacy prose
    replay verbatim (the compressor prompt knows the concluding-turn shapes)."""
    step = decode_step(raw_entry)
    if not step["tool_calls"] and not _looks_native(raw_entry):
        return step["content"]
    rendered = json.loads(snapshot_turn(step["content"], step["tool_calls"]))
    if not full:
        rendered["action"] = [{key: _clip(val) for key, val in action.items()}
                              for action in rendered.get("action", [])]
    return json.dumps(rendered, indent=2, ensure_ascii=False)


def compression_dump(assistant_messages, tool_responses, k, task) -> str:
    """Coder flavor of memory_compression's build_dump: steps [0..k] of the
    native parallel lists in the compressor prompt's <input> format.

    MAIN-THREAD ONLY: snapshots the list content into one string so the worker
    thread never reads the live lists (same contract as the main agent's)."""
    entries = [str(e) for e in assistant_messages[:k + 1]]
    tools = list(tool_responses[:k + 1])
    tools += [None] * (len(entries) - len(tools))

    out = [
        "Session: live (in-run rolling compression, coder/shell session)",
        f"Task: {task}",
        "Trigger: rolling",
    ]

    # PREVIOUS HANDOFF: entry 0 is a prior compressed-memory entry when its
    # decoded content carries the document — lift it into its own section and
    # skip the entry (and its boilerplate note slot) so it isn't summarized
    # twice.
    start = 0
    prev = compressed_memory(decode_step(entries[0])["content"]) if entries else None
    if prev is not None:
        out += ["", "=== PREVIOUS HANDOFF ===", prev]
        start = 1

    # First-task marker — request 1's tag never sits in a tool slot (it rides
    # the live opening turn), so the dump prepends it like the main agent's.
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
            # The reasoning call's placeholder result says nothing: skip it.
            body = [str(r.get("content") or "") for r in results
                    if str(r.get("content") or "") != REASONING_RESULT]
            if body:
                out += ["", "--- USER ---", "\n".join(body)]
        else:
            raw = str(tr)
            m = _USER_REQ_RE.search(raw)
            if m:
                step_no = 0   # step numbering restarts after every new task
                req = (f'<updated_user_request no="{m.group(1)}">\n{m.group(2)}'
                       f'\n</updated_user_request no="{m.group(1)}">')
                # A bridge slot can carry more than the request — e.g. the
                # <manual_mode> block of commands the user hand-typed between
                # runs. Keep it, or the handoff summary loses what the user
                # did at the terminal.
                extra = (raw[:m.start()] + raw[m.end():]).strip()
                out += ["", "--- USER ---", f"{extra}\n\n{req}" if extra else req]
            else:
                out += ["", "--- USER ---", raw]
    return "\n".join(out) + "\n"


def compression_entry(step_k_entry, handoff_text):
    """(entry, tool_slot) replacing entries [0..k] after a handoff lands: ONE
    content-only native assistant turn whose content is
    {"memory_compression": <the whole handoff document>} and nothing else,
    paired with the plain-string note slot. step_k_entry is unused — signature
    parity with the controller's main-agent default."""
    content = json.dumps({_COMPRESSION_KEY: handoff_text}, indent=2, ensure_ascii=False)
    return encode_step(content, []), _COMPRESSION_NOTE


class CLIAgentResponseFormatter:
    """Validates and normalizes CLI agent JSON responses before they enter agent memory"""
    
    # Required fields for CLI agent schema
    REQUIRED_FIELDS = ["thinking", "memory", "next_goal", "action"]
    
    @staticmethod
    def _extract_json(raw_response: str) -> dict | None:
        """
        Extract JSON from various LLM output formats.
        Returns parsed dict if successful, None otherwise.
        """
        # Case 1: Find ```json blocks (use LAST one for reasoning models)
        json_blocks = list(re.finditer(r'```json\s*(.*?)\s*```', raw_response, re.DOTALL))
        if json_blocks:
            for match in reversed(json_blocks):
                try:
                    return json.loads(match.group(1).strip())
                except json.JSONDecodeError:
                    continue
        
        # Case 2: Raw JSON (entire response)
        try:
            return json.loads(raw_response.strip())
        except json.JSONDecodeError:
            pass
        
        # Case 3: Find JSON object using brace matching (last occurrence)
        try:
            lines = raw_response.split('\n')
            potential_starts = [i for i, line in enumerate(lines) if '{' in line]
            
            for json_start in reversed(potential_starts):
                brace_count = 0
                json_end = -1
                
                for i in range(json_start, len(lines)):
                    brace_count += lines[i].count('{') - lines[i].count('}')
                    if brace_count == 0 and i > json_start:
                        json_end = i
                        break
                
                if json_end != -1:
                    first_line = lines[json_start]
                    brace_pos = first_line.find('{')
                    lines[json_start] = first_line[brace_pos:]
                    
                    json_str = '\n'.join(lines[json_start:json_end + 1])
                    try:
                        return json.loads(json_str)
                    except json.JSONDecodeError:
                        continue
        except:
            pass
        
        return None
    
    @staticmethod
    def _validate_schema(json_data: dict) -> tuple:
        """
        Validate that all required fields exist and are properly typed.
        Returns (is_valid, missing_fields)
        """
        missing_fields = []
        
        for field in CLIAgentResponseFormatter.REQUIRED_FIELDS:
            if field not in json_data:
                missing_fields.append(field)
            elif field == "action" and not isinstance(json_data[field], list):
                missing_fields.append(f"{field} (must be array)")
            elif field != "action" and not isinstance(json_data[field], str):
                missing_fields.append(f"{field} (must be string)")
        
        return (len(missing_fields) == 0, missing_fields)
    
    @staticmethod
    def normalize_response(raw_response: str) -> tuple:
        """
        Normalize and validate LLM response.
        
        Returns:
            tuple: (success, normalized_json, raw_response)
            - success: True if valid and complete, False otherwise
            - normalized_json: Formatted JSON string or None if failed
            - raw_response: Original response (for retry on failure)
        """
        # Step 1: Extract JSON
        json_data = CLIAgentResponseFormatter._extract_json(raw_response)

        if json_data is None:
            # Raw response already saved by service._save_raw_response before this call
            return (False, None, raw_response)

        # Step 2: Validate schema (strict - all fields must exist)
        is_valid, missing_fields = CLIAgentResponseFormatter._validate_schema(json_data)
        
        if not is_valid:
            # Raw response already saved by service._save_raw_response before this call
            return (False, None, raw_response)
        
        # Step 3: Normalize and format (no markdown wrapper - avoids backtick collision)
        normalized_json = json.dumps(json_data, indent=2, ensure_ascii=False)
        return (True, normalized_json, raw_response)
    
    @staticmethod
    def get_action_block(normalized_response: str) -> list:
        """
        Extract action block from validated response.
        Call this only after normalize_response returns success.
        
        Returns:
            list: Action array from the response
        """
        try:
            json_data = json.loads(normalized_response)
            return json_data.get("action", [])
        except:
            pass
        return []

    @staticmethod
    def format_stream_json(normalized_response: str) -> str:
        """Per-step payload for the streaming UI (app.py, piped subprocess).

        Since the coder moved to two-channel output its turn is `text`
        (the model's own prose, straight off the text channel) plus `action`
        (the tools it called). The frontend reads `action` for the tool-icon
        chain and the prose for the Shell-use terminal, so `text` is ALSO
        emitted as `thinking` — the key the existing UI already renders — and
        the frontend needs no change. On parse error, pass the input through.
        For a real terminal (cli.py / main.py) use format_terminal instead.
        """
        try:
            json_data = json.loads(normalized_response)
        except Exception:
            return normalized_response
        if isinstance(json_data, dict):
            text = json_data.get("text")
            if text and not json_data.get("thinking"):
                json_data = {**json_data, "thinking": text}
        return json.dumps(json_data, indent=2, ensure_ascii=False)

    @staticmethod
    def format_terminal(normalized_response: str) -> str:
        """Human-readable rendering of one turn for a real terminal (cli.py /
        main.py): the model's message, then the tools it called. Also renders
        legacy envelope steps (thinking / memory / next_goal) so replayed or
        saved runs still print. On parse error, pass the input through.
        """
        try:
            data = json.loads(normalized_response)
        except Exception:
            return normalized_response

        parts = []
        text = data.get("text")
        if text and str(text).strip():
            parts.append(f"💬 {str(text).strip()}")
        # Legacy envelope fields — only present on older/replayed steps.
        thinking = data.get("thinking")
        if thinking and str(thinking).strip().lower() not in ("null", "") and thinking != text:
            parts.append(f"🧠 thinking:\n{thinking}")
        if data.get("memory"):
            parts.append(f"📝 memory: {data.get('memory')}")
        if data.get("next_goal"):
            parts.append(f"🎯 next_goal: {data.get('next_goal')}")
        action = data.get("action") or []
        if action:
            parts.append("⚙️  action:\n" + json.dumps(action, indent=2, ensure_ascii=False))
        return "\n".join(parts) if parts else normalized_response