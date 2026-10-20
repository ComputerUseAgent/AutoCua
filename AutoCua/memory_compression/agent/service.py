"""Handoff compression agent — turns the main agent's in-run step history into
ONE plain-text handoff document (see sibling system_prompt.md).

Pure module: no threads, no list mutation. The caller (the platform agent's
main loop) owns snapshotting the history, spawning the background thread, and
splicing the result back into its memory lists."""

import json
import os
import re

# The synthetic entry's memory field wraps the handoff document in this tag so
# the NEXT compression (and a resumed session) can find it and pass it to the
# compressor as === PREVIOUS HANDOFF ===.
HANDOFF_OPEN = "<handoff>"
HANDOFF_CLOSE = "</handoff>"

_REQUEST_MARKER_PREFIX = "<updated_user_request"
_STEP_PREFIX_RE = re.compile(r"(<Step_no=\d+ />\n)(.*)", re.DOTALL)
_HANDOFF_RE = re.compile(r"<handoff>\n?(.*?)\n?</handoff>", re.DOTALL)


def load_system_prompt() -> str:
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "system_prompt.md")
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read()


def wrap_handoff(text: str) -> str:
    return f"{HANDOFF_OPEN}\n{text}\n{HANDOFF_CLOSE}"


def extract_handoff(memory_text):
    """The handoff body if memory_text carries a <handoff> block, else None."""
    if not isinstance(memory_text, str):
        return None
    m = _HANDOFF_RE.search(memory_text)
    return m.group(1) if m else None


def _entry_parts(entry):
    """Split '<Step_no=N />\\n{json}' -> (prefix, json_str). Bridge entries and
    other bare-JSON entries -> ('', entry)."""
    m = _STEP_PREFIX_RE.match(entry or "")
    return (m.group(1), m.group(2)) if m else ("", entry or "")


def _trim_for_dump(entry: str) -> str:
    """Older dump steps keep decision/memory/next_goal only (mirrors the main
    agent's sliding-window trim; the latest step stays full)."""
    prefix, json_part = _entry_parts(entry)
    try:
        data = json.loads(json_part)
        for f in ("thinking", "eval", "action"):
            data.pop(f, None)
        return prefix + json.dumps(data, indent=2, ensure_ascii=False)
    except Exception:
        return entry


def make_synthetic_entry(step_k_entry: str, handoff_text: str) -> str:
    """Step K's original JSON with ONLY its 'memory' field replaced by the
    wrapped handoff — every other field stays untouched. Unparseable step K
    falls back to a fresh minimal entry (any <Step_no=K /> prefix preserved)."""
    prefix, json_part = _entry_parts(step_k_entry)
    wrapped = wrap_handoff(handoff_text)
    try:
        data = json.loads(json_part)
        data["memory"] = wrapped
        return prefix + json.dumps(data, indent=2, ensure_ascii=False)
    except Exception:
        return prefix + json.dumps({
            "memory": wrapped,
            "next_goal": "Memory compressed to a handoff document; continue the task using it.",
        }, indent=2, ensure_ascii=False)


def build_dump(assistant_messages, tool_responses, k, task) -> str:
    """Render steps [0..k] in the compressor prompt's <input> format.

    MAIN-THREAD ONLY: snapshots the list content into one string so the worker
    thread never reads the live lists. Excludes the main agent's system prompt
    and the live user message (they are never in these lists)."""
    entries = [str(e) for e in assistant_messages[:k + 1]]
    tools = list(tool_responses[:k + 1])
    tools += [None] * (len(entries) - len(tools))

    out = [
        "Session: live (in-run rolling compression)",
        f"Task: {task}",
        "Trigger: rolling",
    ]

    # PREVIOUS HANDOFF: entry 0 is a prior synthetic entry when its memory
    # carries <handoff> — lift the doc out into its own section and leave a
    # pointer in the inline copy so it isn't summarized twice.
    if entries:
        prefix0, json0 = _entry_parts(entries[0])
        try:
            data0 = json.loads(json0)
            prev = extract_handoff(data0.get("memory"))
            if prev is not None:
                data0["memory"] = "(carried in PREVIOUS HANDOFF above)"
                entries[0] = prefix0 + json.dumps(data0, indent=2, ensure_ascii=False)
                out += ["", "=== PREVIOUS HANDOFF ===", prev]
        except Exception:
            pass

    # First-task marker — mirrors the live payload's step-1 prepend (no="1").
    out += ["", "--- USER ---",
            f'<updated_user_request no="1">\n{task}\n</updated_user_request no="1">']

    last = len(entries) - 1
    for i, step in enumerate(entries):
        out += ["", "--- ASSISTANT ---", step if i == last else _trim_for_dump(step)]
        tr = tools[i]
        if tr:
            if isinstance(tr, str) and tr.lstrip().startswith(_REQUEST_MARKER_PREFIX):
                # Run-boundary request marker: a user turn in its own right.
                out += ["", "--- USER ---", str(tr)]
            else:
                out += ["", "--- USER ---", "<tool_response>", str(tr), "</tool_response>"]
    return "\n".join(out) + "\n"


class MemoryCompressionAgent:
    """Plain-text handoff compressor. Owns its OWN LLMManager instance — never
    the main agent's (its last_usage would race with the loop's trigger reads)."""

    def __init__(self, llm_manager):
        self.llm_manager = llm_manager
        self.system_prompt = load_system_prompt()

    def build_dump(self, assistant_messages, tool_responses, k, task) -> str:
        return build_dump(assistant_messages, tool_responses, k, task)

    def compress(self, dump_text: str) -> str:
        """One blocking plain-text LLM call (no image, no schema). Raises on
        failure — the caller decides the drop/retry policy."""
        messages = [
            {"role": "system", "content": self.system_prompt},
            {"role": "user", "content": dump_text},
        ]
        return self.llm_manager.send_request(messages)
