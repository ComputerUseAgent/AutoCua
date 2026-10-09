"""The Linux agents' tool registry: every tool the main driver, the coder and
the minion can call, and the LLMManager that hands them to the model. The
manager itself (providers, retries, fallback, the tool-call parser) is the
same on every platform: AutoCua/llm_provider/llm_manager.py. The agents
import everything LLM-related from here."""

import base64
import io
from typing import Optional

from AutoCua.llm_provider.llm_manager import (LLMManager as _LLMManager, NOTE_TOOLS, REASONING_OPTIONAL,
                                               _tool, drop_bundled_terminal,
                                               tool_calls_to_steps as _tool_calls_to_steps)


# The annotated screenshot goes to the model as JPEG on every platform: the
# macOS scanner encodes it that way (its tree/element.py, LLM_IMAGE_FORMAT),
# the web agent too, and every provider request builder in AutoCua/llm_provider
# labels the image "image/jpeg" - one format the whole stack has proven
# against every provider. The Linux scanner keeps its lossless PNG for the
# frontend preview and the debug dumps; this is the ONE place, on the way to
# the model (LLMManager.send_request below), where that PNG becomes the same JPEG
# the other platforms send. The request builders stay identical to macOS.
#
# Quality 85 with no chroma subsampling (4:4:4) are the macOS scanner's own
# settings: full-resolution chroma is what keeps the thin magenta index
# labels crisp - PIL's default 4:2:0 smears them. Input that is already JPEG
# passes through untouched, so a scanner that emits JPEG costs nothing here.
JPEG_QUALITY = 85
JPEG_SUBSAMPLING = 0


def as_jpeg_base64(image_base64):
    """The screenshot as base64 JPEG. Returns the input unchanged when it is
    empty, already JPEG, or cannot be decoded (the provider then reports
    whatever the API says about it, as before)."""
    if not image_base64:
        return image_base64
    try:
        raw = base64.b64decode(image_base64)
        if raw[:3] == b"\xff\xd8\xff":
            return image_base64
        from PIL import Image   # lazy: coder/minion processes never send images
        img = Image.open(io.BytesIO(raw))
        if img.mode in ("RGBA", "LA", "P"):
            rgb = Image.new("RGB", img.size, (255, 255, 255))
            rgb.paste(img, mask=img.split()[-1] if img.mode == "RGBA" else None)
            img = rgb
        elif img.mode != "RGB":
            img = img.convert("RGB")
        out = io.BytesIO()
        img.save(out, format="JPEG", quality=JPEG_QUALITY,
                 subsampling=JPEG_SUBSAMPLING)
        return base64.b64encode(out.getvalue()).decode("utf-8")
    except Exception as e:
        print(f"Screenshot JPEG conversion skipped ({e}); sending it as captured")
        return image_base64


# ---------------------------------------------------------------------------
# NATIVE TOOLS — coder (cli_agent, mode="main") and minion (read-only subset
# via MINION_TOOL_NAMES) call these tools natively; no JSON envelope. The
# provider returns structured calls, tool_calls_to_steps() converts them into
# the same `[{type, ...}]` action dicts route_action already consumes — the
# controller is untouched. The MAIN DRIVER's own registry lives further down
# (_main_tools) and works exactly the same way.
#
# The CODER's and the MINION's per-step notes (`thinking`, `memory`,
# `next_goal`) live on ONE dedicated tool, `reasoning` (REASONING_FIELDS and
# MINION_REASONING_FIELDS below), called first in every step; the action
# tools carry only their own fields. tool_choice is forced
# (required / {"type": "any"} / mode="ANY"), so a text-only step is
# unrepresentable; the text channel remains for free prose only.
# ---------------------------------------------------------------------------

# Fast mode's notes tool is named `memory` and carries ONE field: `next_goal`.
# thinking and the quality `memory` field are dropped ENTIRELY rather than left
# required-but-empty - a required field is an invitation to fill it, and every
# token spent narrating a plan is a token not spent on the action, which is
# the whole point of fast mode. next_goal alone carries the verdict, the forward
# plan (Now / Plan / Then) and the Expect guard.
MAIN_NOTES_FIELDS_FAST = {
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (name/type/valuePattern.value/active), and any tool name + purpose + important result. Then the forward plan: "Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>." END with the predicted visible change of THIS step\'s action prefixed "Expect:", so the next step can verify against the new screenshot. 3-5 concise lines.',
    },
}


# The CODER's `reasoning` tool fields: the step's notes to itself, replayed in
# history so the model can read back why it did what it did. `memory` and
# `next_goal` are filled every step; `thinking` comes in bursts: a FULL
# thinking ends with a ROUTE of the next 3-5 steps, and the steps on that
# route omit it (the rules live in the coder prompt's <thinking>). It is the
# one OPTIONAL field (REASONING_OPTIONAL): a required string invites filling,
# and the snapshot renders a missing/empty one as "skipped". No step
# numbers and no verdict codes: nothing in the harness parses them, and the
# model cannot count steps reliably, so the verdict is a plain sentence.
REASONING_FIELDS = {
    "thinking": {
        "type": "string",
        "description": 'Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route or it is used up, a short freeform paragraph (RECOVERY) after a failed guard, 1-3 judgment lines (BRIEF) on a surprise. Omit it (or pass "") on a route step: the previous "Next:" named this step and its guard held.',
    },
    "memory": {
        "type": "string",
        "description": 'Follow <memory>. Open with the verdict on the previous step, judged on the actual <Tool_response> (exit code, stderr, output), never assumed: "Last step worked: <what the output proved>" or "Last step failed: <what the output shows>". First step: "Task start." Then the key context the next step needs: important results, paths, errors, where you are in the plan. 2-4 tight lines.',
    },
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal>: "Doing: <this step> (ToDo: #x <task>). If <concrete success signal> → Next: <planned action | think: <decision to make>>." Name the pending ToDo item; make the "If" something the next <Tool_response> can prove.',
    },
}

# The MAIN DRIVER's `reasoning` tool fields (quality mode; fast mode's
# `memory` tool carries `next_goal` alone, see MAIN_NOTES_FIELDS_FAST): the
# coder's design in screen terms. The verdict is judged on the CURRENT
# screenshot, targets are named by name/role in `next_goal` and locked to
# fresh [id]s in `memory`'s Targets line every step, and the guard is the
# visible change the next screenshot must show. No S<n> codes, no "not
# required": a route step omits `thinking` and the snapshot shows "skipped".
MAIN_REASONING_FIELDS = {
    "thinking": {
        "type": "string",
        "description": 'Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route, it is used up, or a new surface appeared; a short freeform paragraph (RECOVERY) after a guard failed on the screenshot; 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass "") on a route step: the previous "Next:" named this action, its visible-change guard holds on the current screenshot, and every named target resolves to exactly one [id].',
    },
    "memory": {
        "type": "string",
        "description": 'Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT screenshot, never on what a tool result claims: "Last step worked: <what the screen shows>", "Last step failed: <what the screen shows instead>" or "Last step unclear: <what is ambiguous, and what would settle it>". First step: "Task start." Then the key context (app/screen state; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: "Targets: id N (name/type/valuePattern.value/active), ..." resolved from the CURRENT <element_tree> before acting. 2-4 tight lines.',
    },
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal>: "Doing: <this step> (ToDo: <task_name>). If <visible change the next screenshot must show> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>." Never name a successor target by [id]: ids are re-assigned every scan. On a route step, "Doing" is the next line of the ROUTE and "Next:" the line after it.',
    },
}



# The MINION's `reasoning` tool fields: same design as the coder's (route
# cadence, optional `thinking`), in the minion's own terms (probes, path:line
# finds) and with its Expect guard at the end of `memory`, which is where the
# minion prompt keeps it.
MINION_REASONING_FIELDS = {
    "thinking": {
        "type": "string",
        "description": 'Your reasoning, written for your future self, not for the parent. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route or it is used up, a short freeform paragraph (RECOVERY) after a failed Expect, 1-3 judgment lines (BRIEF) on a surprise. Omit it (or pass "") on a route step: the previous "Next:" named this probe and its Expect held.',
    },
    "memory": {
        "type": "string",
        "description": 'Follow <memory>. Open with the verdict on the previous probe, judged on the actual <Tool_response>, never assumed: "Last probe worked: <what it found or confirmed>" or "Last probe failed: <what the output shows>". First step: "Task start." Then the confirmed path:line finds and open questions. END with the predicted result of THIS step\'s probe prefixed "Expect:", so the next step can judge against it. Keep it tight.',
    },
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal>: "This step: <what I will do now>. Next: <follow-up | think>." Usually one probe or a tight pair (grep, then view); if the last probe failed, state the recovery. "Next: think" when the outcome decides the route.',
    },
}


CODER_TOOLS = [
    _tool("reasoning", REASONING_FIELDS,
          r"""Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 3-5 steps; on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails, or the route said "think".
- A step may be `reasoning` alone when you need to think before you act; the next step then does the work. Do not chain several such steps.
- Not used on the `exit` step: `exit` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: replace applied at service.py:233, no mismatch. Verification script ready at ./.AutoCua_verify/test_cache.py.", "next_goal": "Doing: run ./.AutoCua_verify/test_cache.py (ToDo: #3 verify). If output shows 6/6 cases pass → Next: think: judge proof, clean up ./.AutoCua_verify/, mark #3 done."}
  2. Planning moment (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, three reports, all anchored, and they agree. UNDERSTAND: known: the cache is a plain dict at service.py:233, put() inserts and nothing evicts; callers api.py:41 and worker.py:88 use only get/put. Assumed: no direct access to the dict elsewhere. DECIDE: OrderedDict inside get/put with a CACHE_MAX constant, no new dependency, callers untouched; the part most likely to be wrong is recency on a hit, so the proof tests exactly that. ROUTE: 1. plan set + todo_list, if both accepted. 2. view service.py:220-250, if the dict and put() sit at the reported anchors. 3. replace put()/get(), add CACHE_MAX, bottom-up, if all apply with no mismatch. 4. write + run ./.AutoCua_verify/test_cache.py, if output shows 4/4 cases pass → think: judge the proof.", "memory": "Last step worked: 3 minion reports with anchors, consistent. Cache = dict at service.py:233, callers api.py:41 and worker.py:88 use only get/put. Approach: OrderedDict LRU inside get/put. Unverified: no direct dict access elsewhere.", "next_goal": "Doing: write the plan (`plan` set) and derive the ToDo (ToDo: none yet — plan). If both are accepted → Next: view service.py:220-250 to confirm the anchors."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "pytest exited 1 with ImportError at test_cache.py:3; the import path is stale after the move. Narrowest fix: correct the import and re-run. Guard: pytest exits 0.", "memory": "Last step failed: pytest exited 1, ImportError in test_cache.py line 3. The code change at service.py:233 is in place.", "next_goal": "Doing: fix the stale import path in test_cache.py (ToDo: #2 fix caching). If pytest exits 0 → Next: re-run the verification script."}""",
          optional=REASONING_OPTIONAL),

    _tool("shell", {"command": {"type": "string"},
                    "input": {"type": "string"}},
          r"""Any native bash/sh command.
- Always include `input` parameter. Use `""` when no input needed. Use actual values when program requires user input (input(), read, prompts, etc.)
- If a result returns `error: permission_dialog`, a system authentication prompt (polkit / sudo) or a permission dialog is blocking the command and couldn't be dismissed automatically. Do NOT blindly retry — report it to the user and ask them to authenticate or grant AutoCua the access it needs, then retry once they confirm.
- Format: shell {"command": "your_command", "input": ""}
- Example:
  1. shell {"command": "find . -type f", "input": ""}
  2. shell {"command": "python calc.py", "input": "5\n10\n"}"""),

    _tool("view", {"path": {"type": "string"},
                   "start": {"type": "integer"},
                   "end": {"type": "integer"}},
          r"""View a file's contents with line numbers. Supports an optional line range — pair this with `grep` to read just the section you need rather than dumping whole files into context.
- All fields required. For whole-file reads pass `start: 0, end: 0`. For a range, pass actual line numbers (1-indexed, inclusive).
- `path` accepts both relative (sandbox cwd) and absolute paths — same as `grep`/`glob`.
- Whole-file mode caps at 2000 lines. If the file is larger, you'll get the first 2000 plus a footer showing the total line count — re-call with `start`/`end` to read other sections.
- Files larger than 5 MB are refused. Use `grep` with `head_limit` instead.
- Output line numbers reflect the file's real line numbers (e.g. `[400] line text` when you view starting at 400), so `write`/`replace` can use them directly without offset arithmetic.
- Format: view {"path": "file_path", "start": 0, "end": 0}
- Examples:
  1. Whole file (small):
     view {"path": "src/auth.py", "start": 0, "end": 0}
  2. Section after a grep hit at line 412:
     view {"path": "src/auth.py", "start": 400, "end": 440}
  3. Project file via absolute path:
     view {"path": "/home/you/projects/app/src/main.py", "start": 0, "end": 0}
  4. Pair pattern — grep first, then view a narrow range:
     Step 1: grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 10, "context": 0}
     (grep returns `AutoCua/linux/agent/cli/service.py:233: ...`)
     Step 2: view {"path": "AutoCua/linux/agent/cli/service.py", "start": 220, "end": 260}"""),

    _tool("grep", {"pattern": {"type": "string"},
                   "path": {"type": "string"},
                   "glob": {"type": "string"},
                   "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                   "case_insensitive": {"type": "boolean"},
                   "head_limit": {"type": "integer"},
                   "context": {"type": "integer"}},
          r"""Search file contents using regex (Python `re` syntax). Prefer this over `shell grep ...` — it's faster, structured (`path:line: text`), and capped to keep context small.
- All fields are required. Use empty/zero defaults for ones you don't need: `path: ""` (sandbox cwd), `glob: ""` (every text file), `case_insensitive: false`, `context: 0`.
- `path` accepts both **relative** (resolved against sandbox cwd) and **absolute** paths. If the user's task is in a project elsewhere on disk (e.g. `/home/you/projects/app`), pass that absolute path — `grep` will search under it. Always pick a specific directory; never pass `/` or `~` to crawl your whole disk.
- Returned `path:line` references are **relative to the `path` you specified**, so they're readable and don't leak full host layout. Noise dirs (`venv`, `.git`, `node_modules`, `__pycache__`, `dist`, `build`, `site-packages`, etc.) are auto-skipped.
- Three `output_mode`s — pick the one matching your intent:
  - `content` — `path:line: matching_text`. Use when you want to read the actual matches.
  - `files_with_matches` — one path per line. Use to find which files to `view` next.
  - `count` — `path: N` per file (only files with N ≥ 1). Use for distribution / sanity checks.
- Binary files, files larger than 8 MB, and lines longer than 200 chars are auto-skipped/truncated to keep output bounded.
- Format: grep {"pattern": "regex", "path": "dir_or_file", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 50, "context": 0}
- Examples:
  1. Find callers of `process_request`:
     grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 30, "context": 0}
  2. Files importing `requests`:
     grep {"pattern": "^import requests|^from requests", "path": "", "glob": "*.py", "output_mode": "files_with_matches", "case_insensitive": false, "head_limit": 100, "context": 0}
  3. Count TODOs case-insensitively:
     grep {"pattern": "TODO|FIXME", "path": "", "glob": "", "output_mode": "count", "case_insensitive": true, "head_limit": 50, "context": 0}
  4. Match with surrounding lines:
     grep {"pattern": "raise ValueError", "path": "src", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 20, "context": 2}"""),

    _tool("glob", {"pattern": {"type": "string"},
                   "path": {"type": "string"},
                   "head_limit": {"type": "integer"}},
          r"""Find files by name pattern. Results are sorted newest-first (by modification time) so recently-edited files surface first.
- All fields required. Use `path: ""` for sandbox cwd; raise `head_limit` when you need to see everything.
- Like `grep`, `path` accepts both relative (sandbox-cwd-anchored) and absolute paths. To list files in a project elsewhere on disk, pass that project's absolute path. Returned paths are relative to the `path` you specified. Noise dirs (`venv`, `.git`, `node_modules`, etc.) are skipped.
- Format: glob {"pattern": "**/*.ext", "path": "base_dir", "head_limit": 100}
- Examples:
  1. All Python files: glob {"pattern": "**/*.py", "path": "", "head_limit": 200}
  2. Recently-changed YAML in configs/: glob {"pattern": "**/*.yaml", "path": "configs", "head_limit": 20}
  3. Top-level test files: glob {"pattern": "test_*.py", "path": "", "head_limit": 50}"""),

    _tool("write", {"path": {"type": "string"},
                    "line": {"type": "integer"},
                    "content": {"type": "string"}},
          r"""Write code, text, or any content into a file.
- Indentation in `content` must match the target file's style.
- Never write an entire large code in one go; build incrementally — one `write` call per step, one file at a time. Break large code across subsequent iterations.
- Always `view` the file first to get current line numbers before writing.
- `line`: The insertion point. New content starts here; existing lines from this point onward shift down.
  - Empty file: use `line: 1`.
  - Append at end: use the last line number shown by `view`.
  - Insert in the middle: use the exact line number where new content should begin.
- Format: write {"path": "file_path", "line": N, "content": "..."}
- Examples:
  1. write {"path": "scr/script.py", "line": 1, "content": "def add(a, b):\n    return a + b\n"}
  2. write {"path": "src/script.py", "line": 11, "content": "def subtract(a, b):\n    return a - b\n"}
  3. write {"path": "src/script.py", "line": 3, "content": "    print('calculating...')\n"}"""),

    _tool("replace", {"path": {"type": "string"},
                      "line": {"type": "integer"},
                      "old_block": {"type": "string"},
                      "new_block": {"type": "string"}},
          r"""Replace a block of code starting at a specific line.
- Always `view` the file first to get fresh line numbers before replacing.
- `line`: starting line number of the block you want to replace.
- `old_block`: the exact block of code currently in the file (multi-line, must match precisely).
- `new_block`: the replacement block (can be more or fewer lines than old_block).
- Multiple `replace`s in one action are supported and safe — the controller validates `old_block` against the actual file content before writing, so any line drift fails loudly with a `mismatch at line X` error rather than corrupting the file. When batching same-file replaces, order them **bottom-up** (highest line first) so earlier replaces don't shift the line numbers below them. Replaces in different files are always safe to batch.
- Format: replace {"path": "file_path", "line": 5, "old_block": "line5\nline6\nline7", "new_block": "new_line5\nnew_line6"}
- Example:
  1. replace {"path": "src/app.py", "line": 10, "old_block": "def add(a, b):\n    return a + b", "new_block": "def add(a, b):\n    result = a + b\n    print(result)\n    return result"}"""),

    _tool("web", {"value": {"type": "string"}},
          r"""Perform a web search across multiple sites automatically.
- The result arrives ONCE, in the next step's <tool> block, and is NOT kept in your history — on that step, DIGEST it: write each finding you need later as its own `scratchpad` entry. Those entries are folded back into the web step's memory as its durable result; anything you don't record is gone.
- Format: web {"value": "query"}
- Example: web {"value": "fetch the latest available LangChain package version for Groq to install"}"""),

    _tool("plan", {"op": {"type": "string", "enum": ["set", "add", "edit"]},
                   "from": {"type": "integer"},
                   "to": {"type": "integer"},
                   "value": {"type": "string"}},
          r"""Your plan document — the detailed, grounded route written AFTER exploration. The plan explains; the ToDo tracks. Three ops (all fields required; use 0 for `from`/`to` when unused):
- op "set": write/overwrite the COMPLETE plan. Use once post-exploration, or on a genuine full re-scope.
- op "add": append `value` at the end of the plan.
- op "edit": overwrite plan lines `from`..`to` (inclusive) with `value` — `value` may contain more or fewer lines than the range it replaces.
- Edit ranges always use the `[N]` line numbers from the LATEST <plan no="N"> in input — they shift after every op.
- Write `value` as PLAIN content — never write your own line numbers or a revision marker; the `[N]` numbering and the `no="N"` revision are stamped automatically. The tool response is a bare `plan updated`; the refreshed, renumbered render is always present in the current step's <persistent_memory> as <plan no="N">.
- CONTENT FORMAT: write a real structured document, NOT a flat numbered list. Use `#` / `##` markdown headings for sections (e.g. Goal, Findings, Steps, Verification), real newlines (`\n`) between lines, and indentation for sub-points. Put concrete `path:line` anchors inline. A bare "1) do X\n2) do Y" is wrong — that's a ToDo, not a plan.
- Format: plan {"op": "set", "from": 0, "to": 0, "value": "..."}
- Examples:
  1. Full plan (note the headings, newlines, indentation, and inline anchors):
     plan {"op": "set", "from": 0, "to": 0, "value": "# Goal\nSwitch the scratchpad cache to an LRU so it stops growing unbounded.\n\n# Findings\n- Cache write lives at service.py:233 (plain dict).\n- Callers: api.py:41, worker.py:88.\n\n# Steps\n## 1. Replace the cache impl\n- service.py:233 — swap dict for functools.lru_cache-backed store.\n## 2. Update callers\n- api.py:41 — adjust call to new signature.\n- worker.py:88 — same.\n\n# Verification\n- ./.AutoCua_verify/test_cache.py — 6 cases incl. empty input + eviction."}
  2. Append a section: plan {"op": "add", "from": 0, "to": 0, "value": "\n# Follow-up\n- Migrate config flag — settings.py:12."}
  3. Surgical edit (replace the two lines under Update callers): plan {"op": "edit", "from": 12, "to": 13, "value": "- api.py:41 — adjust call to new signature.\n- worker.py:88 — already uses the new signature; no change needed."}"""),

    _tool("todo_list", {"value": {"type": "string"}},
          r"""Create the tracking to-do list, derived from the plan.
- The ToDo is your TRACKER, not your plan. The plan (`plan` tool) holds the detail; each ToDo task is a short one-liner derived from it.
  - Iteration 1: if the task needs exploration, skip both plan and ToDo (or write a one-line skeleton) and dispatch minions first.
  - Right after minions report: think → write the plan (`plan` op set) from `<user_request>` + minion findings (ignore typos) → then write the ToDo from that plan.
- `todo_list` OVERWRITES and re-numbers the whole list. So write it ONCE (right after the plan), before completing any items; after that, advance it with `update_todo` only. Small plan revisions (add/edit) usually need NO ToDo change — re-issue `todo_list` only if the task list itself genuinely re-scopes, and then re-mark items already done.
- Tasks are auto-numbered as #1, #2, #3, etc. when saved.
- Format: todo_list {"value": "Objective: <corrected_user_request>\n- [ ] <task naming file/approach>\n- [ ] <task 2>"}"""),

    _tool("update_todo", {"value": {"type": "string"}},
          r"""Only update once cross verfied thoroughly. Mark a ToDo item complete by providing its #number.
- Update only after the task is confirmed complete; mark one item per call.
- Provide only the task number to mark complete.
- Format: update_todo {"value": "task number #x"}
- Example: update_todo {"value": "2"}"""),

    _tool("wait", {"value": {"type": "string"}},
          r"""Pause the pipeline for x seconds.
- Format: wait {"value": "2"}
- Example: wait {"value": "2"}"""),

    _tool("scratchpad", {"value": {"type": "string"}},
          r"""Your durable note store — verified checkpoints AND any key fact you may need later. Write an entry immediately after something is visually confirmed. If multiple facts are confirmed in one step, emit one separate `scratchpad` call per fact.
- Purpose: store verified facts for later steps (reduces re-reading `<agent_history>`).
- Only write entries after visual confirmation (never assume success).
- The live file is rendered in your input as `<scratchpad>` once any entries exist — check it before recording, so entries never duplicate.
- Use for:
  - major task completions (not tiny micro-steps)
  - metrics / numbers / final answers
  - important `web` findings to reuse later
  - exact file save paths + filenames (especially "Save As" / PDF exports)
- `value` is ONE line — a single verified note. Don't batch several facts into one entry.
- Write `value` in Markdown — inline only (`**bold**`, backticks, links), never a line break.
- Format: scratchpad {"value": "<one-line verified note in markdown format>"}
- Examples:
  1. scratchpad {"value": "**Done:** fixed all indentation errors in `app.py`"}
  2. scratchpad {"value": "**Key metric:** Disney+ revenue (Q3 2025) = **$2.1 Billion**"}
  3. Two facts confirmed in one step — two separate calls:
     scratchpad {"value": "**Verified:** parser handles empty input — **6/6** cases pass"}, scratchpad {"value": "**Saved:** report exported to `/home/me/Desktop/q3_report.pdf`"}"""),

    _tool("minion", {"value": {"type": "string"}},
          r"""Read-only scout. **Don't explore the codebase yourself — send a minion.** It explores the filesystem, traces cross-file connections, and returns ONE structured summary anchored to `path:line`. You never see the intermediate reads — your context stays clean for editing.
- **Rule**: minion handles exploration + connection-tracing. You handle editing (`write`/`replace`).
- **When to send one (any of these → minion, not your own reading):** you need to understand code before editing it; you'd otherwise grep/glob/view more than ~2 times; you're tracing a symbol / caller / dependency across files; or you're mapping an unfamiliar directory. Your own `grep`/`view` are for quick re-checks of something a minion already surfaced — not first-time exploration.
- **Phrase the value as a question or objective — NEVER as instructions about which tools to use.** The minion is self-capable and picks its own tools internally. Do NOT write things like "use grep…" / "use shell…" / "use glob…" / "use view…" — just say what you want to know. The minion will figure out how to find it.
- Format: minion {"value": "<self-contained question a fresh agent can act on>"}
- Multiple minions in one action run in parallel; your loop pauses until all return as `<minion_completed>` blocks.
- **Trust the summary.** Don't re-read files yourself unless the summary is explicitly incomplete. The minion cannot edit — once you have its report, apply the change.
- Good examples (state what you want, not how to get it):
  1. minion {"value": "find every caller of _read_scratchpad_from_file — exact path:line for each."}
  2. minion {"value": "list all imports of ScratchpadService under AutoCua/linux/ with line numbers + direct usages."}
  3. minion {"value": "give me a list of all files and directories under /home/me/Downloads with a one-line summary of each."}
  4. Parallel: minion {"value": "Q1..."}, minion {"value": "Q2..."}, minion {"value": "Q3..."}
- Anti-pattern (do NOT write): `"Please use the shell or glob tool to list all files in X"` — you ASK what you need; the minion picks what to RUN. Correct version: `"give me a list of all files in X"`."""),

    _tool("exit", {"value": {"type": "string"}},
          r"""End the run and deliver your final summary to the user. Pass the end-to-end summary as the value.
- Only start completion after reviewing `<agent_history>` to confirm every requested task is finished.
- Then do a final verification against actual outputs — the concrete <Tool_response> evidence — double-checking the last steps match the request.
- Use `exit` as a dedicated final step only:
  - Step 1 (no `exit`): confirm <verification> passed with concrete proof and ALL throwaway check files (`./.AutoCua_verify/`) are deleted; finish/cleanup + update ToDos/scratchpad.
  - Step 2: call `exit` — the only call in its turn.
- Write `value` in Markdown — headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.
- Format: exit {"value": "<end-to-end summary in markdown format>"}"""),
]

# MINION TOOLS — the minion's read-only subset (shell/view/grep/glob/
# scratchpad/exit; NO write/replace/web/plan/todo/wait/minion). Descriptions
# moved VERBATIM from the minion prompt's <Tool_Capability>. Its notes live on
# its own `reasoning` tool (MINION_REASONING_FIELDS), same design as the coder.
MINION_TOOLS = [
    _tool("reasoning", MINION_REASONING_FIELDS,
          r"""Your notes to yourself for this step. Call it FIRST in every step, then the probe calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 3-5 probes, each with its Expect; on each probe of that route omit `thinking` (or pass ""), and think again only when the route is used up, an Expect fails, or the route said "think". Output that matches its Expect is confirmation, not new information: it goes into `memory` and `scratchpad`, never into a new thinking or a new ROUTE. The usual shape is one FULL, then three or four probes in a row without `thinking`.
- A step may be `reasoning` alone when you need to think before you probe; the next step then does the work. Do not chain several such steps.
- Not used on the `exit` step: `exit` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Two route steps in a row (no thinking; "This step" is the next route line, the Expect is that line's prediction):
     reasoning {"memory": "Last probe worked: grep found _read_scratchpad at service.py:254 (definition confirmed). Expect: view 240-270 shows the def and its return.", "next_goal": "This step: view service.py 240-270 to confirm the function. Next: view minions/service.py around its hit."}
     reasoning {"memory": "Last probe worked: view shows the def at :254 and its return at :271; recorded. Expect: minions/service.py around its hit shows a call, not a comment.", "next_goal": "This step: view minions/service.py around its hit. Next: grep the bare name in *.md."}
  2. Planning moment (FULL, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, one definition at service.py:254 as predicted. UNDERSTAND: known: the def line. Assumed: the hits in minions/service.py and controller/view.py are calls; both unviewed. DECIDE: view each hit in a 20-line range, definition first; then one grep for the bare name in *.md to catch prompt references. ROUTE: 1. view service.py:245-275, expect the def and its return. 2. view minions/service.py around its hit, expect a call, not a comment. 3. grep the bare name in *.md, expect none → think: coverage review.", "memory": "Last probe worked: one `def` at service.py:254; hits in minions/service.py and controller/view.py unviewed. Expect: view 245-275 shows the def and its return.", "next_goal": "This step: view service.py 245-275. Next: view minions/service.py around its hit."}
  3. Recovery after a failed Expect:
     reasoning {"thinking": "Empty: grep 'process_request(' under src/ returned nothing though the request says the caller exists. Wrong belief: that the call uses the name with parentheses directly; an alias or wrapper may hide it. Narrowest different probe: case-insensitive files_with_matches for process_request across the repo. Expect 1-3 files. Route holds after this.", "memory": "Last probe failed: grep 'process_request(' returned empty in src/. Expect: case-insensitive grep lists 1-3 files.", "next_goal": "This step: case-insensitive grep 'process_request' across the repo. Next: think."}""",
          optional=REASONING_OPTIONAL),

    _tool("shell", {"command": {"type": "string"},
                    "input": {"type": "string"}},
          r"""Native bash/sh - **READ-ONLY commands only** (e.g. `ls`, `find`, `file`, `stat`, `wc -l`, `head`, `tail` without redirection, `which`). Never run anything that writes, deletes, moves, or otherwise mutates state. Always include `input: ""`.
- If a result returns `error: permission_dialog`, a system authentication prompt (polkit / sudo) or a permission dialog blocked the command and couldn't be dismissed automatically. Don't retry blindly - note it in your final report (the parent agent needs to authenticate or be granted the access) and continue with what you can.
- Format: shell {"command": "your_command", "input": ""}
- Allowed examples:
  1. shell {"command": "find . -type d -maxdepth 3 -not -path '*/.*'", "input": ""}
  2. shell {"command": "ls -la src/", "input": ""}
  3. shell {"command": "wc -l AutoCua/linux/agent/service.py", "input": ""}
- **Forbidden** (do NOT emit - these mutate state):
  1. shell {"command": "rm ...", "input": ""}
  2. shell {"command": "mv a b", "input": ""}
  3. shell {"command": "echo hi > a.txt", "input": ""}
  4. shell {"command": "sed -i 's/x/y/' file", "input": ""}
  5. shell {"command": "touch new.txt", "input": ""}"""),

    _tool("view", {"path": {"type": "string"},
                   "start": {"type": "integer"},
                   "end": {"type": "integer"}},
          r"""View a file's contents with line numbers. Supports an optional line range - pair this with `grep` to read just the section you need rather than dumping whole files into context.
- All fields required. For whole-file reads pass `start: 0, end: 0`. For a range, pass actual line numbers (1-indexed, inclusive).
- `path` accepts both relative (sandbox cwd) and absolute paths - same as `grep`/`glob`.
- Whole-file mode caps at 2000 lines. If the file is larger, you'll get the first 2000 plus a footer showing the total line count - re-call with `start`/`end` to read other sections.
- Files larger than 5 MB are refused. Use `grep` with `head_limit` instead.
- Output line numbers reflect the file's real line numbers (e.g. `[400] line text` when you view starting at 400) - quote them exactly in your final report.
- Format: view {"path": "file_path", "start": 0, "end": 0}
- Examples:
  1. Whole file (small):
     view {"path": "src/auth.py", "start": 0, "end": 0}
  2. Section after a grep hit at line 412:
     view {"path": "src/auth.py", "start": 400, "end": 440}
  3. Project file via absolute path:
     view {"path": "/home/you/projects/app/src/main.py", "start": 0, "end": 0}
  4. Pair pattern - grep first, then view a narrow range:
     Step 1: grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 10, "context": 0}
     (grep returns `AutoCua/linux/agent/cli/service.py:233: ...`)
     Step 2: view {"path": "AutoCua/linux/agent/cli/service.py", "start": 220, "end": 260}"""),

    _tool("grep", {"pattern": {"type": "string"},
                   "path": {"type": "string"},
                   "glob": {"type": "string"},
                   "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                   "case_insensitive": {"type": "boolean"},
                   "head_limit": {"type": "integer"},
                   "context": {"type": "integer"}},
          r"""Search file contents using regex (Python `re` syntax). Prefer this over `shell grep ...` - it's faster, structured (`path:line: text`), and capped to keep context small.
- All fields are required. Use empty/zero defaults for ones you don't need: `path: ""` (sandbox cwd), `glob: ""` (every text file), `case_insensitive: false`, `context: 0`.
- `path` accepts both **relative** (resolved against sandbox cwd) and **absolute** paths. Always pick a specific directory; never pass `/` or `~` to crawl your whole disk.
- Returned `path:line` references are **relative to the `path` you specified**. Noise dirs (`venv`, `.git`, `node_modules`, `__pycache__`, `dist`, `build`, `site-packages`, etc.) are auto-skipped.
- Binary files, files larger than 8 MB, and lines longer than 200 chars are auto-skipped/truncated.
- Three output_modes:
  - `content` - `path:line: matching_text` (default; use when you want to read matches)
  - `files_with_matches` - list of paths only (use to find which files to view next)
  - `count` - `path: N` per file (use for distribution / sanity check)
- Format: grep {"pattern": "regex", "path": "dir_or_file", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 50, "context": 0}
- Examples:
  1. Find callers of `process_request`:
     grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 30, "context": 0}
  2. Files importing `requests`:
     grep {"pattern": "^import requests|^from requests", "path": "", "glob": "*.py", "output_mode": "files_with_matches", "case_insensitive": false, "head_limit": 100, "context": 0}
  3. Count TODOs case-insensitively:
     grep {"pattern": "TODO|FIXME", "path": "", "glob": "", "output_mode": "count", "case_insensitive": true, "head_limit": 50, "context": 0}
  4. Match with surrounding lines:
     grep {"pattern": "raise ValueError", "path": "src", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 20, "context": 2}
- Tactics for coverage: anchor on the definition first (`def `/`class `/`function `/`=` shapes) then widen to bare usages; if a symbol might be imported under an alias, also grep `import.*<name>` and `as <alias>`; if a first pattern returns nothing, broaden (drop the `(`, make it case-insensitive, widen the path/glob) rather than concluding it's absent."""),

    _tool("glob", {"pattern": {"type": "string"},
                   "path": {"type": "string"},
                   "head_limit": {"type": "integer"}},
          r"""Find files by name pattern. Results are sorted newest-first (by modification time) so recently-edited files surface first.
- All fields required. Use `path: ""` for sandbox cwd; raise `head_limit` when you need to see everything.
- Like `grep`, `path` accepts both relative and absolute paths. Returned paths are relative to the `path` you specified. Noise dirs (`venv`, `.git`, `node_modules`, etc.) are skipped.
- Format: glob {"pattern": "**/*.ext", "path": "base_dir", "head_limit": 100}
- Examples:
  1. All Python files: glob {"pattern": "**/*.py", "path": "", "head_limit": 200}
  2. Recently-changed YAML in configs/: glob {"pattern": "**/*.yaml", "path": "configs", "head_limit": 20}
  3. Top-level test files: glob {"pattern": "test_*.py", "path": "", "head_limit": 50}"""),

    _tool("scratchpad", {"value": {"type": "string"}},
          r"""Your durable note store while exploring. Every verified finding goes here immediately so the final exit report can be assembled from it without re-reading files.
- Purpose: persist `path:line` findings + key facts across iterations.
- Only write entries after the finding is confirmed by an actual `view`/`grep` result. Never assume.
- Use one entry per finding (don't pack multiple facts into one line).
- Use for:
  - confirmed `path:line` definitions, callers, and connections
  - file/folder layout summaries
  - exact code snippets you want to quote in the report (also note the file's language/extension so the fence tag - e.g. ```python - is ready at exit time)
  - open questions you still need to answer before exit
- Write `value` in Markdown — inline only (`**bold**`, backticks around `path:line` and symbols), never a line break.
- Format: scratchpad {"value": "<one-line verified note in markdown format>"}
- Examples:
  1. scratchpad {"value": "`AutoCua/linux/agent/service.py:254` — `_read_scratchpad_from_file` definition"}
  2. scratchpad {"value": "`AutoCua/linux/controller/view.py:620` — `action_type == \"scratchpad\"` routing branch"}
  3. scratchpad {"value": "**Still to verify:** any other callers of `_read_scratchpad_from_file` outside `agent/service.py`"}"""),

    _tool("exit", {"value": {"type": "string"}},
          r"""Deliver your final findings report to the parent CLI agent and end the loop. **This is the only way to terminate.** The `value` must follow the `<exit_format>` template.
- Write `value` in Markdown — headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the report needs them.
- Format: exit {"value": "<structured report in markdown format>"}
- Must be a standalone action - no other tool calls in the same step."""),
]

# The minion's allowed action names — passed to tool_calls_to_steps so a
# hallucinated coder-only call (write/replace/minion/...) is answered with an
# error result, never routed. This is what enforces the read-only guarantee
# now that the registry above, not a strict action union, is the contract.
MINION_TOOL_NAMES = frozenset(t["name"] for t in MINION_TOOLS)


# Per-action defaults: guarantees route_action always receives every field of
# an action, even if the model omits an optional-feeling one. `reasoning` is the
# coder's note to itself: the loop records it and never routes it.
_ACTION_DEFAULTS = {
    "reasoning": {"thinking": "", "memory": "", "next_goal": ""},
    "shell": {"command": "", "input": ""},
    "view": {"path": "", "start": 0, "end": 0},
    "grep": {"pattern": "", "path": "", "glob": "", "output_mode": "content",
             "case_insensitive": False, "head_limit": 50, "context": 0},
    "glob": {"pattern": "", "path": "", "head_limit": 100},
    "write": {"path": "", "line": 1, "content": ""},
    "replace": {"path": "", "line": 1, "old_block": "", "new_block": ""},
    "plan": {"op": "set", "from": 0, "to": 0, "value": ""},
    "web": {"value": ""},
    "todo_list": {"value": ""},
    "update_todo": {"value": ""},
    "wait": {"value": ""},
    "scratchpad": {"value": ""},
    "minion": {"value": ""},
    "exit": {"value": ""},
}


# Names the coder may call — anything else is answered with an error tool
# result instead of being silently dropped, so the model can correct itself
# on the very next turn.
CODER_TOOL_NAMES = frozenset(t["name"] for t in CODER_TOOLS)


MAIN_REASONING_TOOL = _tool("reasoning", MAIN_REASONING_FIELDS,
                            r"""Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its visible-change guard; on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails on the screenshot, a new surface appears, or the route said "think". A screenshot that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.
- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is only partially visible, means this step thinks.
- A step may be `reasoning` alone when you need to think before you act (it costs a fresh screen scan); the next step then does the work. Do not chain several such steps.
- Not used on the `done` step: `done` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: Mail front, the message list showing as predicted. Targets: id 31 (name='Search', type='TextField', active='True').", "next_goal": "Doing: search Mail for the invoice (ToDo: Find the invoice). If the message list shows only messages matching 'invoice' → Next: think: pick the message to open."}
  2. New surface (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, the Gmail compose window is open as predicted, To field empty. UNDERSTAND: known: To, Subject and body are in the tree as text fields; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. click Send, if the compose window closes and 'Message sent' appears → think: verify the sent mail, then mark the ToDo.", "memory": "Last step worked: compose window open, guard held. Targets: id 12 (name='To', type='TextField'), id 14 (name='Subject', type='TextField'), id 20 (name='Message Body', type='TextArea').", "next_goal": "Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: click the Send button."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "Failed: the To field is still empty after input on id 74; the screenshot shows the cursor in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: click the To field first, then input. Guard: the To field shows the address. Route holds after this.", "memory": "Last step failed: To field empty, focus was in Subject. Correction carried: click a field before typing into it in this compose window. Targets: id 74 (name='To', type='TextField').", "next_goal": "Doing: click the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject."}""",
                            optional=REASONING_OPTIONAL)

MAIN_NOTES_TOOL_FAST = _tool("memory", MAIN_NOTES_FIELDS_FAST,
                             r"""Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.
- One field, `next_goal`, filled every step: key context, the forward plan (Now / Plan / Then) and the Expect guard the next step verifies against (see <next_goal>). There is no thinking field: the reasoning stays silent (see <silent_reasoning>).
- Every step resolves the plan's targets to fresh [id]s from the CURRENT <element_tree> and names them in `next_goal` before acting.
- Not used on the `done` step: `done` is always the only call of its turn.
- Format: memory {"next_goal": "..."}
- Example:
  memory {"next_goal": "Spotify front, Search page open; search field id 44 (name='Search', type='TextField', active='True'). Now: search for the focus playlist (ToDo: Play focus playlist). Plan: pick the playlist -> play it. Then: open the best-matching playlist. Expect: Spotify lists focus playlists in its results."}""")


# The tools that act on the screen. While the main driver's scan is off
# (the `scan` tool) they are taken out of the tool list sent to the model and
# out of the names it may call, and they come back with scan on.
SCREEN_TOOLS = ("left_click", "right_click", "input", "typewrite", "scroll",
                "hotkey", "screenshot", "open_app", "app_script")


def _main_tools(track: dict, notes: dict = None) -> list:
    """The MAIN DRIVER's registry - one tool per action type. route_action and
    the frontend's tool-flow map key on these names and fields, so they must
    never drift.

    Descriptions are the system prompt's OWN tool text, copied verbatim from
    <Tool_Capability> / <os_interaction> / <task_completion> - the prompt stays
    the source of truth (it is user-managed and never edited from here); this
    just puts the same words where a native tool call can read them. Built
    twice, each with NO per-call params (`track` stays {}) and a dedicated
    notes tool in front: quality mode the `reasoning` tool (thinking optional,
    memory + next_goal every step), fast mode the `memory` tool (`next_goal`
    alone).

    Linux-specific vs the Windows registry: `input`/`typewrite` carry `value`
    (not `text`) as on macOS, and `app_script` (app + value) - the Linux
    counterpart of the macOS `applescript` tool - exists here only. The macOS
    `drag_drop` tool is deliberately not offered on Linux. `clicks` stays on
    right_click/screenshot even though the controller ignores it, because the
    old strict union required it."""
    notes = [notes] if notes else []
    return notes + [
        # -- <Tool_Capability> ------------------------------------------------
        _tool("open_app", {"value": {"type": "string"}},
              'Launch an installed application (anything with a desktop entry). No manual search required within the OS.\n'
              '    1. Requirement: Typically call wait 3 seconds immediately after this tool to allow loading.\n'
              '    3. Example: open_app {"value": "spotify"}', track=track),
        _tool("wait", {"value": {"type": "string"}},
              'Pause execution to allow UI loading or to trigger a fresh screen scan.\n'
              '    2. Example: wait {"value": "2"}', track=track),
        _tool("scan", {"value": {"type": "string", "enum": ["on", "off"]}},
              'Switch the screen scan on or off. The scan is on by default, and every new user request starts with it on. A switch takes effect from the next step.\n'
              f'    1. off: use it when the next steps only run through `shell` (bash commands) and the other tools that need no screen (web, sub-agents, scratchpad, todo). Every scanned step sends a screenshot and an <element_tree>; with scan off neither is sent, which cuts the input tokens and saves money, and the screen tools ({", ".join(SCREEN_TOOLS)}) are removed. Turn it off once your last screen action was judged on a screenshot.\n'
              '    2. Keep it off for as long as the following steps do not need the screen.\n'
              '    3. on: switch it on in the step before one that needs the screen (a click, a key, a visual check). From the next step the screenshot, the <element_tree> and every tool are back.\n'
              '    4. Example: scan {"value": "off"}', track=track),
        _tool("web", {"value": {"type": "string"}},
              'Delegate to a specialized AI to fetch real-time information and provide data at runtime. Use this for speed instead of manual browsing.\n'
              '    2. Example: web {"value": "financial result of nvidia Q4 2025"}', track=track),
        _tool("sub_agent", {"agent_type": {"type": "string", "enum": ["coder_agent", "browser_agent"]},
                            "value": {"type": "string"}},
              'Start a sub-agent on a task. It works in parallel while you carry on, and gets a row with its id in <sub_agents>.\n'
              '    1. `agent_type`:\n'
              '      1. `coder_agent`: complex coding and multi-step shell work.\n'
              '      2. `browser_agent`: acts on websites in its own Brave browser (navigate, open tabs, click, fill forms). One at a time. Its window can open in front of your app: leave it to the browser agent and bring your own app forward with Alt + Tab. It runs on the agent\'s own Brave profile, not the person\'s browser, so the person\'s signed-in accounts are not there: buying and anything that needs their accounts stay with you.\n'
              '    2. `value`: the full instruction. It is all the sub-agent gets, so make it self-contained.\n'
              '    3. Example: sub_agent {"agent_type": "coder_agent", "value": "instruction"}', track=track),
        _tool("agent_wait", {"agent_type": {"type": "string", "enum": ["coder_agent", "browser_agent"]},
                             "agent_id": {"type": "integer"},
                             "value": {"type": "string"}},
              'Hold the pipeline until one sub-agent finishes. Use it only when your next step needs its report, before you start another browser_agent (one at a time), or before done.\n'
              '    1. `agent_type` and `agent_id`: the sub-agent to hold for, as its row in <sub_agents> shows them.\n'
              '    2. If another sub-agent finishes first, the hold ends with that report; call agent_wait again to keep holding.\n'
              '    3. Example: agent_wait {"agent_type": "coder_agent", "agent_id": 1, "value": "Reason"}', track=track),
        _tool("shell", {"value": {"type": "string"}},
              'Run a shell (bash) command for fast execution to achieve the goal.\n'
              '    1. Example:\n'
              '      1. shell {"value": "gio trash --empty"}\n'
              '      2. shell {"value": "xdg-open \\"$HOME/Documents/report.pdf\\""}', track=track),
        _tool("app_script", {"app": {"type": "string"}, "value": {"type": "string"}},
              'Drive a Linux app from a bash script through its command line or D-Bus interface (the Linux counterpart of AppleScript). The runtime launches/focuses `app` first, so the script must NOT start the app itself - it only sends it commands. End with a command whose output verifies the result.\n'
              '    1. Browsers: hand URLs to the browser command (`firefox --new-tab URL`, `google-chrome URL`). GNOME apps: their own CLI (`nautilus --select <path>`) or `gdbus call --session --dest <bus name> ...`.\n'
              '    2. Example: app_script {"app": "Firefox", "value": "firefox --new-tab \\"https://youtube.com\\" && echo opened"}', track=track),
        _tool("todo_list", {"value": {"type": "string"}},
              'Create the ToDo task list (iteration 1 by default; you may also create/expand it later if complexity emerges). See <todo_capability>.', track=track),
        _tool("update_todo", {"value": {"type": "string"}},
              'Tasks are auto-numbered #1, #2, #3, etc. when saved.\n'
              '    1. Update (only after confirmed complete via <agent_history> and the effect is visible in the latest input - image or any relevant tag; one item per call)\n'
              '    2. Example: update_todo {"value": "1"}', track=track),
        _tool("scratchpad", {"value": {"type": "string"}},
              'Record a verified checkpoint or any critical fact (file path, metric, finding). Follow <scratchpad> rules.\n'
              '    1. Write `value` in Markdown - inline only (`**bold**`, backticks), never a line break.\n'
              '    2. Example: scratchpad {"value": "**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**"}', track=track),

        # -- <os_interaction> -------------------------------------------------
        _tool("left_click", {"id": {"type": "integer"}, "clicks": {"type": "integer"}},
              'left mouse click. clicks=1: single click, clicks=2: double click (open files/folders), clicks=3: triple click (OCR_TEXT).\n'
              '    1. Example: left_click {"id": 8, "clicks": 2}\n'
              '    2. Sequence example: left_click {"id": 9, "clicks": 1}, left_click {"id": 10, "clicks": 1}', track=track),
        _tool("right_click", {"id": {"type": "integer"}, "clicks": {"type": "integer"}},
              'right mouse click, open context menu/options.\n'
              '    1. Example: right_click {"id": 9 , "clicks": 1}', track=track),
        _tool("input", {"id": {"type": "integer"}, "value": {"type": "string"}},
              'Insert text into an element (the whole string lands at once, exactly as given).\n'
              '    1. Auto-deletes existing text first.\n'
              '    2. `enter` must be sent separately when needed (e.g., email \'From\', \'To\', \'Search\' fields).\n'
              '      1. Scenario: input + enter + input.\n'
              '    3. If the text does not appear (a field that refuses inserted text, or one box per character), left_click the field and use typewrite instead.\n'
              '    4. Example: input {"id": 9, "value": "hi, how are you"}', track=track),
        _tool("typewrite", {"value": {"type": "string"}},
              'type into the currently focused area when no element is available.\n'
              '    1. Does not auto-delete; use backspace if needed.\n'
              '    2. Multi-line or long text is inserted whole, exactly as given, indentation kept. Do not use the shell or the clipboard to enter text.\n'
              '    3. Example: typewrite {"value": "hi, how are you"}', track=track),
        _tool("scroll", {"id": {"type": "integer"}, "direction": {"type": "string"}},
              'scroll an element in a direction (`up/down/left/right`).\n'
              '    1. Example: scroll {"id": 9, "direction": "up"}', track=track),
        _tool("hotkey", {"value": {"type": "string"}},
              'OS hotkeys (max 3 keys pairs). Applies to `<Front_screen>`.\n'
              '    1. Use only for OS-level shortcut combinations (e.g., `ctrl+c`, `ctrl+q`, `super+down`).\n'
              '    2. Examples:\n'
              '        1. hotkey {"value": "enter"}\n'
              '        2. hotkey {"value": "ctrl+shift+s"}', track=track),
        _tool("screenshot", {"id": {"type": "integer"}, "clicks": {"type": "integer"}},
              'Capture a UI element part as an image and copy it to the clipboard for pasting elsewhere.\n'
              '    1. It takes a screenshot without annotation, so do not trigger it to capture the magenta element number.\n'
              '    2. Image is ready to paste with ctrl+v. The clicks field is a dummy (always 1).\n'
              '    3. Example: screenshot {"id": 15, "clicks": 1}', track=track),

        # -- <task_completion> (the only place `done` is documented) -----------
        _tool("done", {"value": {"type": "string"}},
              'Use `done` as a dedicated final step only, after reviewing <agent_history> to confirm every requested task is finished and doing a final visual verification from the latest image.\n'
              '    1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.\n'
              '    2. Step 2: output ONLY Format: done {"value": "<end-to-end summary in markdown format>"}\n'
              '    3. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.\n'
              '    4. Never combine `done` with any other action/tool in the same step.\n'
              '    5. While scan is off there is no image: base the final verification on the latest tool results.', track=track),
    ]


MAIN_TOOLS = _main_tools({}, notes=MAIN_REASONING_TOOL)
MAIN_TOOLS_FAST = _main_tools({}, notes=MAIN_NOTES_TOOL_FAST)

# Per-action defaults for the main driver - guarantees route_action always
# receives every field of an action, even one the model left out.
MAIN_ACTION_DEFAULTS = {
    "reasoning": {"thinking": "", "memory": "", "next_goal": ""},
    "left_click": {"id": 0, "clicks": 1},
    "right_click": {"id": 0, "clicks": 1},
    "screenshot": {"id": 0, "clicks": 1},
    "input": {"id": 0, "value": ""},
    "typewrite": {"value": ""},
    "scroll": {"id": 0, "direction": "down"},
    "app_script": {"app": "", "value": ""},
    "hotkey": {"value": ""},
    "open_app": {"value": ""},
    "wait": {"value": ""},
    "scan": {"value": ""},
    "web": {"value": ""},
    "shell": {"value": ""},
    "sub_agent": {"agent_type": "coder_agent", "value": ""},
    "agent_wait": {"agent_type": "coder_agent", "agent_id": 0, "value": ""},
    "todo_list": {"value": ""},
    "update_todo": {"value": ""},
    "scratchpad": {"value": ""},
    "done": {"value": ""},
}

# Fast mode's notes tool is `memory` (next_goal alone) instead of `reasoning`;
# tool_calls_to_steps reads the defaults map to find the mode's notes tool.
MAIN_ACTION_DEFAULTS_FAST = {"memory": {"next_goal": ""},
                             **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}

MAIN_TOOL_NAMES = frozenset(t["name"] for t in MAIN_TOOLS)
MAIN_TOOL_NAMES_FAST = frozenset(t["name"] for t in MAIN_TOOLS_FAST)


# ---------------------------------------------------------------------------
# CLAUDE: any Claude model, whatever the provider (anthropic direct, or a Claude
# entry on OpenRouter; see LLMManager.claude). Every agent runs on
# its own claude_prompt.md (the main driver in quality mode only) with its own
# notes tool, `agent_memory`: the `reasoning` tool's notes with `assessment` in
# place of `thinking` (memory + next_goal every step, assessment when
# <assessment> calls for it).
# ---------------------------------------------------------------------------
CLAUDE_REASONING_FIELDS = {
    "assessment": {"type": "string", "description": 'Follow <assessment>. Omit it (or pass "") on a route step.'},
    "memory": {"type": "string", "description": "Follow <memory>. Filled every step."},
    "next_goal": {"type": "string", "description": "Follow <next_goal>. Filled every step."},
}


def _claude_reasoning_tool(final: str, work: str = "calls", batched: bool = False) -> dict:
    """Claude's `agent_memory` notes tool. `final` is the agent's closing tool (always
    the only call of its turn); `work` names its action calls.

    `batched` (every agent): the opening sentences say the call never ends a
    turn. Claude treats a notes tool as a think step - it calls it, ends the
    turn to wait for the result and acts on the next turn - unless told the
    actions belong in the SAME message. Measured 2026-10-05 on the driver's
    step 1 (Sonnet 5): notes+action in one turn 0/5 before, 5/6 with this
    wording plus the matching <blocks> rule in claude_prompt.md and the per-turn
    reminder the driver appends to its live message. Keep the name: a renamed
    tool (`step_notes`) was refused by Sonnet 5.5 / Opus 5 as reasoning
    extraction."""
    if batched:
        opening = (f"Your written notes for this step: a log entry, not an action. It has no result and nothing waits on "
                   f"it, so it NEVER ends a turn: emit it first in the list and the {work} that do this step's \"Doing\" "
                   "in the SAME message. A message holding only this call and no action is a wasted turn. The call is "
                   "replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n")
    else:
        opening = (f"Your notes to yourself for this step. Call it FIRST in every step, then the {work} that do the work, "
                   "all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is "
                   "replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n")
    return _tool("agent_memory", CLAUDE_REASONING_FIELDS,
                 opening +
                 "- `memory` and `next_goal` are filled every step. `assessment` follows <assessment>: omit it "
                 '(or pass "") on a route step.\n'
                 f"- Not used on the `{final}` step: `{final}` is always the only call of its turn.\n"
                 '- Format: agent_memory {"assessment": "", "memory": "...", "next_goal": "..."}',
                 optional=REASONING_OPTIONAL)


_CLAUDE_REASONING_DEFAULTS = {"assessment": "", "memory": "", "next_goal": ""}
CODER_TOOLS_CLAUDE = ([_claude_reasoning_tool("exit", batched=True)]
                      + [t for t in CODER_TOOLS if t["name"] != "reasoning"])
MINION_TOOLS_CLAUDE = ([_claude_reasoning_tool("exit", "probe calls", batched=True)]
                       + [t for t in MINION_TOOLS if t["name"] != "reasoning"])
MAIN_TOOLS_CLAUDE = _main_tools({}, notes=_claude_reasoning_tool("done", batched=True))
# Coder and minion share _ACTION_DEFAULTS; the main driver has its own.
ACTION_DEFAULTS_CLAUDE = {"agent_memory": dict(_CLAUDE_REASONING_DEFAULTS),
                          **{k: v for k, v in _ACTION_DEFAULTS.items() if k != "reasoning"}}
MAIN_ACTION_DEFAULTS_CLAUDE = {"agent_memory": dict(_CLAUDE_REASONING_DEFAULTS),
                               **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}
CODER_TOOL_NAMES_CLAUDE = frozenset(t["name"] for t in CODER_TOOLS_CLAUDE)
MINION_TOOL_NAMES_CLAUDE = frozenset(t["name"] for t in MINION_TOOLS_CLAUDE)
MAIN_TOOL_NAMES_CLAUDE = frozenset(t["name"] for t in MAIN_TOOLS_CLAUDE)


def tool_calls_to_steps(tool_calls: list, allowed=None, defaults_map=None, track_params=None) -> tuple:
    """The shared tool_calls_to_steps with the coder's tables as the defaults:
    the coder and the minion pass None for them unless they run on Claude."""
    return _tool_calls_to_steps(tool_calls, CODER_TOOL_NAMES if allowed is None else allowed,
                                defaults_map or _ACTION_DEFAULTS, track_params)


class LLMManager(_LLMManager):
    """The shared LLMManager with this platform's tool lists."""
    MAIN_TOOLS = MAIN_TOOLS
    MAIN_TOOLS_FAST = MAIN_TOOLS_FAST
    MAIN_TOOLS_CLAUDE = MAIN_TOOLS_CLAUDE
    CODER_TOOLS = CODER_TOOLS
    CODER_TOOLS_CLAUDE = CODER_TOOLS_CLAUDE
    MINION_TOOLS = MINION_TOOLS
    MINION_TOOLS_CLAUDE = MINION_TOOLS_CLAUDE
    SCREEN_TOOLS = SCREEN_TOOLS

    def send_request(self, messages: list, annotated_screenshot_base64: Optional[str] = None):
        """The screenshot goes to the model as JPEG (as_jpeg_base64 above)."""
        return super().send_request(messages, as_jpeg_base64(annotated_screenshot_base64))
