//! The coder's tools: the name, settings and description of each one the model is given.
//!
//! The Rust copy of the coder and minion parts of mac/tool_registry/service.py
//! (the mac minion is the subagent here), together with the shared manager it
//! subclasses there (AutoCua/llm_provider/llm_manager.py: the tool builder,
//! the parser tool_calls_to_steps, drop_bundled_terminal, the dialects, the
//! retries, the fallback model and the usage), in the shape of
//! ../android/tool_registry/service.rs. The providers stay the shared Python
//! ones in AutoCua/llm_provider: build_provider makes one for the tool list
//! and resolve_model_info reads the model tables.
//!
//! NATIVE TOOLS: the coder and the subagent call these tools natively, with
//! no JSON envelope. The provider returns structured calls, and
//! tool_calls_to_steps() converts them into the `[{type, ...}]` action dicts
//! route_action consumes. The per-step notes live on two tools of their own,
//! batched with the action calls and the same for the coder and the
//! subagent: `insight` (only when something new came up) and `next_goal`
//! (every probe turn). The action tools carry only their own fields. tool_choice is
//! forced (required / {"type": "any"} / mode="ANY"), so a text-only step is
//! unrepresentable; the text channel remains for free prose only.
//!
//! One prompt and one tool list for every model: there is no Claude variant.
//!
//! Exposed to Python as the `LLMManager` class with the constructor and the
//! surface the Python original has: CompressionController builds its second,
//! text-mode manager from this class and reads the instance.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use serde_json::{json, Map, Value};

use crate::coder::{py_strip, truthy};

/// The transcript carries an optional per-turn `provider_meta` on assistant
/// messages: the provider's OWN metadata for that turn (Gemini 3 thought
/// signatures, OpenRouter reasoning blocks), needed to echo the turn back
/// exactly as the model produced it. Only these providers translate it; for
/// everyone else the key is stripped before the request is built, because
/// openai/groq/perplexity/together forward the message dicts to their API
/// verbatim and an unknown key there is a 400.
pub const META_KEY: &str = "provider_meta";
pub const META_PROVIDERS: [&str; 2] = ["openrouter", "google"];

/// The notes tools, the coder's and the subagent's: `insight` and
/// `next_goal`. Each loop answers a call to one of them `ok` itself and never
/// routes it, and the controller adds no result for one. (The shared Python
/// constant also names other agents' notes tools, which no list here has.)
pub const NOTE_TOOLS: &[&str] = &["insight", "next_goal"];

/// The legacy track params tool_calls_to_steps harvests when it is given
/// none. No registry carries them any more: both loops pass none, because
/// each reads its notes off its own `insight` and `next_goal` calls. Kept so
/// the parser behaves as the shared one.
const LEGACY_TRACK_KEYS: [&str; 2] = ["thinking", "next_goal"];

/// How the mac descriptions name the shell the `shell` tool runs.
const MAC_SHELL: &str = "zsh/bash";

/// The shell the `shell` tool runs on this OS, as its description names it:
/// the mac text's own words on macOS, PowerShell on Windows, bash on Linux.
fn shell_name() -> &'static str {
    if cfg!(windows) {
        "PowerShell"
    } else if cfg!(target_os = "macos") {
        MAC_SHELL
    } else {
        "bash"
    }
}

/// A mac description with the shell named for this OS.
fn on_this_os(description: &str) -> String {
    description.replacen(MAC_SHELL, shell_name(), 1)
}

/// Build one canonical tool def: name + parameters + description. The
/// REGISTRY is the single source of tool documentation: each tool's rules
/// live in its `description`, which flows to every provider dialect. Tool
/// defs sit in the cached prefix, so this bills once. Every param is
/// required, so the controller always sees every field of an action. The
/// coder and the subagent pass no per-call track params: their notes live on
/// tools of their own (NOTE_TOOLS).
fn tool(name: &str, params: &[(&str, Value)], description: &str) -> Value {
    let mut props = Map::new();
    for (k, schema) in params {
        props.insert((*k).to_string(), schema.clone());
    }
    let required: Vec<Value> = props.keys().map(|k| json!(k)).collect();
    let mut out = Map::new();
    out.insert("name".into(), json!(name));
    out.insert(
        "parameters".into(),
        json!({"type": "object", "properties": Value::Object(props), "required": required}),
    );
    if !description.is_empty() {
        out.insert("description".into(), json!(description));
    }
    Value::Object(out)
}

/// The notes tools, the same two for the coder and the subagent, first in
/// each list: `insight` when something new came up and `next_goal` every
/// probe turn. They follow the prompts' <insight> and <next_goal>; the loops
/// answer them (NOTE_TOOLS). `goal_format` is the agent's own `next_goal`
/// format, the one its prompt gives.
fn notes_tools(goal_format: &str) -> Vec<Value> {
    vec![
        tool("insight", &[
            ("kind", json!({"type": "string", "enum": ["request", "exploration", "result", "failure", "review"]})),
            ("content", json!({"type": "string"})),
        ],
            r#"Follow <insight>. Write down what your thinking concluded when something new came up. `kind` is what arrived: request, exploration, result, failure or review. `content` is markdown in that kind's format."#),

        tool("next_goal", &[("value", json!({"type": "string"}))],
            &format!(r#"Follow <next_goal>. The step you are taking now, written as the outcome it leaves behind: "{goal_format}""#)),
    ]
}

/// The coder's `next_goal` format (main/system_prompt.md, <next_goal>).
const CODER_GOAL_FORMAT: &str = "Goal: <observable outcome of this step> (insight step <k>; ToDo #<n> <task_name>).";
/// The subagent's (subagent/system_prompt.md, <next_goal>): no ToDo there.
const SUBAGENT_GOAL_FORMAT: &str = "Goal: <evidence this step brings in> (insight step <k>).";

/// One mode of the coder, turned on and off with its own switch tool:
/// {"value": "on"} or {"value": "off"}. Every mode is off when a run starts.
/// The coder's loop answers a switch call itself and never routes it
/// (main/service.rs), and the switch shapes the next request only, because
/// the model can only call the tools a request gives it. While a mode is
/// on, the tools it hides leave the tool list (LLMManager::hide_tools) and
/// the names a call may use, and its block sits last in the live
/// <persistent_memory>, after the block of every mode above it in MODES:
///
/// ```text
/// <{tool}>
/// {on_message}
///
/// <skill name="{skill_name}">
/// {the skill file, read fresh every step}
/// </skill>
/// </{tool}>
/// ```
pub struct Mode {
    /// The switch tool's name, also the tag of the mode's block.
    pub tool: &'static str,
    /// The skill's name, in its <skill name="..."> tag.
    pub skill_name: &'static str,
    /// The skill file, relative to AutoCua/coder, with `/` between its parts on every OS.
    pub skill_file: &'static str,
    /// The tools taken away while the mode is on. Never its own switch: the
    /// model could not turn the mode off again.
    pub hides: &'static [&'static str],
    /// The switch tool's description.
    pub description: &'static str,
    /// The answer to "on", and the first line of the mode's block.
    pub on_message: &'static str,
    /// The answer to "off".
    pub off_message: &'static str,
}

/// The coder's modes, in the order their switches are given to the model
/// (right before `plan`) and their blocks are shown.
///
/// To add a mode: add its entry here and put its skill file at its
/// `skill_file`. Nothing else changes: the tool list, the defaults, the
/// names a call may use, the hidden tools, the answers and the block all
/// come from this table.
pub const MODES: &[Mode] = &[
    Mode {
        tool: "planning_mode",
        skill_name: "planning",
        skill_file: "main/planner/planner.md",
        hides: &["write", "replace", "update_todo", "exit"],
        description: r#"Follow <operating_procedure>, PLANNING MODE. `value` "on" enters planning mode: the planning skill is loaded into <persistent_memory>, and `write`, `replace`, `update_todo` and `exit` are taken away until you turn it off. `value` "off" leaves planning mode and gives them back."#,
        on_message: "Planning mode active. Editing of code is prohibited. Flip planning mode off to continue editing.",
        off_message: "Planning mode off. Editing of code is allowed again.",
    },
];

/// A mode's switch tool: one required field, `value`, "on" or "off".
fn mode_tool(mode: &Mode) -> Value {
    tool(mode.tool, &[("value", json!({"type": "string", "enum": ["on", "off"]}))], mode.description)
}

/// The coder's tools, in the order the model is given them: the notes tools
/// (notes_tools) first, then one tool per action type. route_action keys on
/// these names and fields, so they must never drift. The action tools'
/// descriptions are the mac coder's own text, with the shell named for this
/// OS (on_this_os), except `plan` (the owner's text for planning mode) and
/// `subagent` (its two modes, workflow and critic). The modes' switches
/// (MODES) sit right before `plan`, in table order.
pub fn coder_tools() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| [notes_tools(CODER_GOAL_FORMAT), vec![
        tool("shell", &[
            ("command", json!({"type": "string"})),
            ("input", json!({"type": "string"})),
        ],
            &on_this_os(r#"Any native zsh/bash command.
- Always include `input` parameter. Use `""` when no input needed. Use actual values when program requires user input (input(), read, prompts, etc.)
- If a result returns `error: permission_dialog`, a macOS permission popup is blocking the command and couldn't be auto-clicked. Do NOT blindly retry — report it to the user and ask them to grant AutoCua **Accessibility + Full Disk Access** (System Settings > Privacy & Security), then retry once they confirm.
- Format: shell {"command": "your_command", "input": ""}
- Example:
  1. shell {"command": "find . -type f", "input": ""}
  2. shell {"command": "python calc.py", "input": "5\n10\n"}"#)),

        tool("read", &[
            ("path", json!({"type": "string"})),
            ("start", json!({"type": "integer"})),
            ("end", json!({"type": "integer"})),
        ],
            r#"Read a file's contents with line numbers. Supports an optional line range — pair this with `grep` to read just the section you need rather than dumping whole files into context.
- All fields required. For whole-file reads pass `start: 0, end: 0`. For a range, pass actual line numbers (1-indexed, inclusive).
- `path` accepts both relative (sandbox cwd) and absolute paths — same as `grep`/`glob`.
- Whole-file mode caps at 2000 lines. If the file is larger, you'll get the first 2000 plus a footer showing the total line count — re-call with `start`/`end` to read other sections.
- Files larger than 5 MB are refused. Use `grep` with `head_limit` instead.
- Output line numbers reflect the file's real line numbers (e.g. `[400] line text` when you read starting at 400), so `write`/`replace` can use them directly without offset arithmetic.
- Format: read {"path": "file_path", "start": 0, "end": 0}
- Examples:
  1. Whole file (small):
     read {"path": "src/auth.py", "start": 0, "end": 0}
  2. Section after a grep hit at line 412:
     read {"path": "src/auth.py", "start": 400, "end": 440}
  3. Project file via absolute path:
     read {"path": "/Users/you/projects/app/src/main.py", "start": 0, "end": 0}
  4. Pair pattern — grep first, then read a narrow range:
     Step 1: grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 10, "context": 0}
     (grep returns `AutoCua/mac/agent/cli/service.py:233: ...`)
     Step 2: read {"path": "AutoCua/mac/agent/cli/service.py", "start": 220, "end": 260}"#),

        tool("grep", &[
            ("pattern", json!({"type": "string"})),
            ("path", json!({"type": "string"})),
            ("glob", json!({"type": "string"})),
            ("output_mode", json!({"type": "string", "enum": ["content", "files_with_matches", "count"]})),
            ("case_insensitive", json!({"type": "boolean"})),
            ("head_limit", json!({"type": "integer"})),
            ("context", json!({"type": "integer"})),
        ],
            r#"Search file contents using regex (Python `re` syntax). Prefer this over `shell grep ...` — it's faster, structured (`path:line: text`), and capped to keep context small.
- All fields are required. Use empty/zero defaults for ones you don't need: `path: ""` (sandbox cwd), `glob: ""` (every text file), `case_insensitive: false`, `context: 0`.
- `path` accepts both **relative** (resolved against sandbox cwd) and **absolute** paths. If the user's task is in a project elsewhere on disk (e.g. `/Users/you/projects/app`), pass that absolute path — `grep` will search under it. Always pick a specific directory; never pass `/` or `~` to crawl your whole disk.
- Returned `path:line` references are **relative to the `path` you specified**, so they're readable and don't leak full host layout. Noise dirs (`venv`, `.git`, `node_modules`, `__pycache__`, `dist`, `build`, `site-packages`, etc.) are auto-skipped.
- Three `output_mode`s — pick the one matching your intent:
  - `content` — `path:line: matching_text`. Use when you want to read the actual matches.
  - `files_with_matches` — one path per line. Use to find which files to `read` next.
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
     grep {"pattern": "raise ValueError", "path": "src", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 20, "context": 2}"#),

        tool("glob", &[
            ("pattern", json!({"type": "string"})),
            ("path", json!({"type": "string"})),
            ("head_limit", json!({"type": "integer"})),
        ],
            r#"Find files by name pattern. Results are sorted newest-first (by modification time) so recently-edited files surface first.
- All fields required. Use `path: ""` for sandbox cwd; raise `head_limit` when you need to see everything.
- Like `grep`, `path` accepts both relative (sandbox-cwd-anchored) and absolute paths. To list files in a project elsewhere on disk, pass that project's absolute path. Returned paths are relative to the `path` you specified. Noise dirs (`venv`, `.git`, `node_modules`, etc.) are skipped.
- Format: glob {"pattern": "**/*.ext", "path": "base_dir", "head_limit": 100}
- Examples:
  1. All Python files: glob {"pattern": "**/*.py", "path": "", "head_limit": 200}
  2. Recently-changed YAML in configs/: glob {"pattern": "**/*.yaml", "path": "configs", "head_limit": 20}
  3. Top-level test files: glob {"pattern": "test_*.py", "path": "", "head_limit": 50}"#),

        tool("write", &[
            ("path", json!({"type": "string"})),
            ("line", json!({"type": "integer"})),
            ("content", json!({"type": "string"})),
        ],
            r#"Write code, text, or any content into a file.
- Indentation in `content` must match the target file's style.
- Never write an entire large code in one go; build incrementally — one `write` call per step, one file at a time. Break large code across subsequent iterations.
- Always `read` the file first to get current line numbers before writing.
- `line`: The insertion point. New content starts here; existing lines from this point onward shift down.
  - Empty file: use `line: 1`.
  - Append at end: use the last line number shown by `read`.
  - Insert in the middle: use the exact line number where new content should begin.
- Format: write {"path": "file_path", "line": N, "content": "..."}
- Examples:
  1. write {"path": "scr/script.py", "line": 1, "content": "def add(a, b):\n    return a + b\n"}
  2. write {"path": "src/script.py", "line": 11, "content": "def subtract(a, b):\n    return a - b\n"}
  3. write {"path": "src/script.py", "line": 3, "content": "    print('calculating...')\n"}"#),

        tool("replace", &[
            ("path", json!({"type": "string"})),
            ("line", json!({"type": "integer"})),
            ("old_block", json!({"type": "string"})),
            ("new_block", json!({"type": "string"})),
        ],
            r#"Replace a block of code starting at a specific line.
- Always `read` the file first to get fresh line numbers before replacing.
- `line`: starting line number of the block you want to replace.
- `old_block`: the exact block of code currently in the file (multi-line, must match precisely).
- `new_block`: the replacement block (can be more or fewer lines than old_block).
- Multiple `replace`s in one action are supported and safe — the controller validates `old_block` against the actual file content before writing, so any line drift fails loudly with a `mismatch at line X` error rather than corrupting the file. When batching same-file replaces, order them **bottom-up** (highest line first) so earlier replaces don't shift the line numbers below them. Replaces in different files are always safe to batch.
- Format: replace {"path": "file_path", "line": 5, "old_block": "line5\nline6\nline7", "new_block": "new_line5\nnew_line6"}
- Example:
  1. replace {"path": "src/app.py", "line": 10, "old_block": "def add(a, b):\n    return a + b", "new_block": "def add(a, b):\n    result = a + b\n    print(result)\n    return result"}"#),

        tool("web", &[("value", json!({"type": "string"}))],
            r#"Perform a web search across multiple sites automatically.
- The result arrives ONCE, in the next step's <tool> block, and is NOT kept in your history — on that step, DIGEST it: write each finding you need later as its own `scratchpad` entry. Those entries are folded back into the web step's memory as its durable result; anything you don't record is gone.
- Format: web {"value": "query"}
- Example: web {"value": "fetch the latest available LangChain package version for Groq to install"}"#),

    // The modes' switches, in table order.
    ], MODES.iter().map(mode_tool).collect(), vec![

        tool("plan", &[
            ("op", json!({"type": "string", "enum": ["set", "add", "edit"]})),
            ("from", json!({"type": "integer"})),
            ("to", json!({"type": "integer"})),
            ("value", json!({"type": "string"})),
        ],
            r##"Your plan document — the standing design record of the task, written in planning mode AFTER exploration, derived from the `exploration` insight. The plan explains; the ToDo tracks; the insight's Steps are the actions. Three ops (all fields required; use 0 for `from`/`to` when unused):
- op "set": write/overwrite the COMPLETE plan. Use once post-exploration (right before `todo_list` and the exit from planning mode), or on a genuine full re-scope, which re-enters planning mode.
- op "add": append `value` at the end of the plan — for work the request requires that the plan missed, found during execution.
- op "edit": overwrite plan lines `from`..`to` (inclusive) with `value` — `value` may contain more or fewer lines than the range it replaces. For findings that shift the plan without breaking it (moved anchors, a changed detail, an extra proof case), in the same turn as the insight that found them.
- Edit ranges always use the `[L]` line numbers from the LATEST <plan no="R"> in input — they shift after every op, and blank lines are numbered too.
- Write `value` as PLAIN content — never write your own line numbers or a revision marker; the `[L]` numbering and the `no="R"` revision are stamped automatically. The tool response is a bare `plan updated`; the refreshed, renumbered render is always present in the current step's <persistent_memory> as <plan no="R">.
- CONTENT FORMAT: write a real structured document, NOT a flat numbered list. Use `#` / `##` markdown headings for the sections, in this order: Goal (the request as observable behavior) · Findings (how the code works today, with anchors) · Approach (the pick, its deciding reason, rejected options a line each) · Edits (what changes where, each at its anchor, in dependency order; `##` per file) · Assumptions & Risks (what is unverified and what breaks if it is wrong) · Verification (the cases that will prove the Goal, the one most likely to fail marked). Real newlines (`\n`) between lines, indentation for sub-points, concrete `path:line` anchors inline. A bare "1) do X\n2) do Y" is wrong — that's a ToDo, not a plan. Goal is never edited to fit what was built.
- Format: plan {"op": "set", "from": 0, "to": 0, "value": "..."}
- Examples:
1. Full plan (note the headings, newlines, indentation, and inline anchors):
plan {"op": "set", "from": 0, "to": 0, "value": "# Goal\nThe scratchpad cache in service.py holds at most CACHE_MAX entries and evicts the least recently used one; api.py:41 and worker.py:88 keep working unchanged.\n\n# Findings\n- Cache: module-level dict `_cache` at service.py:41; get() at :63 reads it, put() at :88 inserts; nothing ever deletes — that is the unbounded growth.\n- Callers: api.py:41 and worker.py:88 use only get()/put(); no code reaches into `_cache` directly (grep across src/ returns only service.py).\n- Settings are module constants at service.py:12-20; no config system. Imports at :1-6.\n- Tests: `python -m pytest`; nothing covers the cache today.\n- get() returns early on a hit at :66 — the hit path and the miss path are separate branches.\n\n# Approach\nOrderedDict inside get()/put(): move_to_end on a hit, popitem(last=False) once the size passes CACHE_MAX. About 10 lines, no new dependency, callers untouched.\n- Rejected: functools.lru_cache — entries arrive through explicit put() calls from two sites; there is no pure function to decorate.\n- Rejected: cachetools — a new dependency for one use.\n- Out of scope: thread safety; the worker is single-threaded.\n\n# Edits\n## service.py\n- :1-6 — add `from collections import OrderedDict` to the import block.\n- :12-20 — add `CACHE_MAX = 512` beside the other constants, same naming style.\n- :41 — `_cache = OrderedDict()` in place of the dict.\n- :63 get() — on a hit, `_cache.move_to_end(key)` BEFORE the early return at :66.\n- :88 put() — after insert, `while len(_cache) > CACHE_MAX: _cache.popitem(last=False)`.\n\n# Assumptions & Risks\n- Nothing accesses `_cache` dynamically (getattr/globals): unverifiable by grep; OrderedDict keeps the dict interface, so the consequence is nil.\n- Recency is the part most likely to be wrong: if move_to_end lands on the miss path the cache is FIFO — covered by case 2.\n- CACHE_MAX=0 would evict on every put; nothing sets it, not covered.\n\n# Verification\n- ./.AutoCua_verify/test_cache.py, imports the real `service`, prints N/4, exits 1 on any failure:\n  1. fill to CACHE_MAX+1 → the first key is gone, size == CACHE_MAX.\n  2. get('a') then overflow → 'a' survives, the next-oldest is gone. (most likely to fail)\n  3. overwrite an existing key → size unchanged.\n  4. put/get round-trip returns the value.\n- `python -m pytest` still passes.\n- Cross-file check: a workflow agent confirms api.py:41 and worker.py:88 resolve and use only get/put."}
2. Append a section: plan {"op": "add", "from": 0, "to": 0, "value": "\n## worker.py\n- :88 — no change: the composite key is built before put() and is opaque to the cache."}
3. Surgical edit (the read showed put() at :91, not :88 — line [23] of the render above): plan {"op": "edit", "from": 23, "to": 23, "value": "- :91 put() — after insert, `while len(_cache) > CACHE_MAX: _cache.popitem(last=False)`."}"##),

        tool("todo_list", &[("value", json!({"type": "string"}))],
            r#"Create the tracking to-do list, derived from the plan.
- The ToDo is your TRACKER, not your plan. The plan (`plan` tool) holds the detail; each ToDo task is a short one-liner derived from it.
  - Iteration 1: if the task needs exploration, skip both plan and ToDo (or write a one-line skeleton) and dispatch subagents first.
  - Right after subagents report: think → write the plan (`plan` op set) from `<user_request>` + subagent findings (ignore typos) → then write the ToDo from that plan.
- `todo_list` OVERWRITES and re-numbers the whole list. So write it ONCE (right after the plan), before completing any items; after that, advance it with `update_todo` only. Small plan revisions (add/edit) usually need NO ToDo change — re-issue `todo_list` only if the task list itself genuinely re-scopes, and then re-mark items already done.
- Tasks are auto-numbered as #1, #2, #3, etc. when saved.
- Format: todo_list {"value": "Objective: <corrected_user_request>\n- [ ] <task naming file/approach>\n- [ ] <task 2>"}"#),

        tool("update_todo", &[("value", json!({"type": "string"}))],
            r#"Only update once cross verfied thoroughly. Mark a ToDo item complete by providing its #number.
- Update only after the task is confirmed complete; mark one item per call.
- Provide only the task number to mark complete.
- Format: update_todo {"value": "task number #x"}
- Example: update_todo {"value": "2"}"#),

        tool("wait", &[("value", json!({"type": "string"}))],
            r#"Pause the pipeline for x seconds.
- Format: wait {"value": "2"}
- Example: wait {"value": "2"}"#),

        tool("scratchpad", &[("value", json!({"type": "string"}))],
            r#"Your durable note store — verified checkpoints AND any key fact you may need later. Write an entry immediately after something is visually confirmed. If multiple facts are confirmed in one step, emit one separate `scratchpad` call per fact.
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
     scratchpad {"value": "**Verified:** parser handles empty input — **6/6** cases pass"}, scratchpad {"value": "**Saved:** report exported to `/Users/me/Desktop/q3_report.pdf`"}"#),

        tool("subagent", &[
            ("mode", json!({"type": "string", "enum": ["workflow", "critic"]})),
            ("value", json!({"type": "string"})),
        ],
            r#"Start a subagent: a helper with its own fresh context that does one bounded job and returns ONE summary. It cannot edit files. Pick its `mode`:
- "workflow" (the workflow agents of your prompt): maps the code for you. It finds the connections between files, drills through several code paths at once, follows the patterns that link them, and reports which file holds what at which lines: the callers, the dependencies, what crosses each connection. You get `path:line` anchors to go to directly, instead of filling your own context with reads, so your context stays lean for editing, for building the logic and for the plan.
- "critic": an unbiased judge of your edit. It has no stake in your change: it reads what you changed, judges whether the edit is correct and whether the code is sound, and tries hard to prove where you went wrong: a wrong assumption, an edge case the code mishandles, logic that only looks right, a test that could not fail. Its summary helps you judge whether the edit is correct; use it to fix the logic, strengthen the verification scenario and test again, until the change is robust and correct. Whether the edit is wired into the rest of the code (callers, imports, signatures) is a workflow check.
- `value`: a self-contained brief a fresh agent can act on. For workflow: one narrow question and exactly what to bring back (anchors, signatures, callers). For critic: what you changed and where, what it must do, and the proof you already have. Name files by absolute path: the subagent starts in its own folder, not in your current_sitting. Say what you need, never which tools to use: the subagent picks its own.
- Several subagents in one turn run in parallel; the turn waits until all of them report.
- Format: subagent {"mode": "workflow" | "critic", "value": "<brief>"}
- Examples:
  1. subagent {"mode": "workflow", "value": "Where does the scratchpad cache live and how is it read and written? Bring back path:line of the structure, get, put and any eviction."}
  2. subagent {"mode": "workflow", "value": "Every caller of _read_scratchpad_from_file, with path:line and what each one passes in."}
  3. subagent {"mode": "critic", "value": "I made the cache in /Users/you/projects/app/service.py:41-95 an LRU bounded by CACHE_MAX: get() refreshes recency, put() evicts the oldest. Prove it wrong if you can: recency on a hit, eviction order, the CACHE_MAX boundary, anything /Users/you/projects/app/.AutoCua_verify/test_cache.py misses. Bring back each flaw with path:line and the input that breaks it."}
  4. Parallel: subagent {"mode": "workflow", "value": "Q1..."}, subagent {"mode": "workflow", "value": "Q2..."}
- Anti-pattern: "use grep to find X" or "use the shell to list Y". Ask for what you need; the subagent picks what to run."#),

        tool("exit", &[("value", json!({"type": "string"}))],
            r#"End the run and deliver your final summary to the user. Pass the end-to-end summary as the value.
- Only start completion after reviewing `<agent_history>` to confirm every requested task is finished.
- Then do a final verification against actual outputs — the concrete <Tool_response> evidence — double-checking the last steps match the request.
- Use `exit` as a dedicated final step only:
  - Step 1 (no `exit`): confirm <verification> passed with concrete proof and ALL throwaway check files (`./.AutoCua_verify/`) are deleted; finish/cleanup + update ToDos/scratchpad.
  - Step 2: call `exit` — the only call in its turn.
- Write `value` in Markdown — headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.
- Format: exit {"value": "<end-to-end summary in markdown format>"}"#),
    ]].concat())
}

/// The subagent's tools: the notes tools (notes_tools), then the read-only
/// subset (shell, read, grep, glob, scratchpad, exit; no write, replace, web,
/// plan, todo, wait or subagent). The action tools' descriptions are the mac
/// minion prompt's own words.
pub fn subagent_tools() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| [notes_tools(SUBAGENT_GOAL_FORMAT), vec![
        tool("shell", &[
            ("command", json!({"type": "string"})),
            ("input", json!({"type": "string"})),
        ],
            &on_this_os(r#"Native zsh/bash - **READ-ONLY commands only** (e.g. `ls`, `find`, `file`, `stat`, `wc -l`, `head`, `tail` without redirection, `which`). Never run anything that writes, deletes, moves, or otherwise mutates state. Always include `input: ""`.
- If a result returns `error: permission_dialog`, a macOS permission popup blocked the command and couldn't be auto-clicked. Don't retry blindly - note it in your final report (the parent agent needs Accessibility + Full Disk Access granted) and continue with what you can.
- Format: shell {"command": "your_command", "input": ""}
- Allowed examples:
  1. shell {"command": "find . -type d -maxdepth 3 -not -path '*/.*'", "input": ""}
  2. shell {"command": "ls -la src/", "input": ""}
  3. shell {"command": "wc -l AutoCua/mac/agent/service.py", "input": ""}
- **Forbidden** (do NOT emit - these mutate state):
  1. shell {"command": "rm ...", "input": ""}
  2. shell {"command": "mv a b", "input": ""}
  3. shell {"command": "echo hi > a.txt", "input": ""}
  4. shell {"command": "sed -i '' 's/x/y/' file", "input": ""}
  5. shell {"command": "touch new.txt", "input": ""}"#)),

        tool("read", &[
            ("path", json!({"type": "string"})),
            ("start", json!({"type": "integer"})),
            ("end", json!({"type": "integer"})),
        ],
            r#"Read a file's contents with line numbers. Supports an optional line range - pair this with `grep` to read just the section you need rather than dumping whole files into context.
- All fields required. For whole-file reads pass `start: 0, end: 0`. For a range, pass actual line numbers (1-indexed, inclusive).
- `path` accepts both relative (sandbox cwd) and absolute paths - same as `grep`/`glob`.
- Whole-file mode caps at 2000 lines. If the file is larger, you'll get the first 2000 plus a footer showing the total line count - re-call with `start`/`end` to read other sections.
- Files larger than 5 MB are refused. Use `grep` with `head_limit` instead.
- Output line numbers reflect the file's real line numbers (e.g. `[400] line text` when you read starting at 400) - quote them exactly in your final report.
- Format: read {"path": "file_path", "start": 0, "end": 0}
- Examples:
  1. Whole file (small):
     read {"path": "src/auth.py", "start": 0, "end": 0}
  2. Section after a grep hit at line 412:
     read {"path": "src/auth.py", "start": 400, "end": 440}
  3. Project file via absolute path:
     read {"path": "/Users/you/projects/app/src/main.py", "start": 0, "end": 0}
  4. Pair pattern - grep first, then read a narrow range:
     Step 1: grep {"pattern": "process_request\\(", "path": "", "glob": "*.py", "output_mode": "content", "case_insensitive": false, "head_limit": 10, "context": 0}
     (grep returns `AutoCua/mac/agent/cli/service.py:233: ...`)
     Step 2: read {"path": "AutoCua/mac/agent/cli/service.py", "start": 220, "end": 260}"#),

        tool("grep", &[
            ("pattern", json!({"type": "string"})),
            ("path", json!({"type": "string"})),
            ("glob", json!({"type": "string"})),
            ("output_mode", json!({"type": "string", "enum": ["content", "files_with_matches", "count"]})),
            ("case_insensitive", json!({"type": "boolean"})),
            ("head_limit", json!({"type": "integer"})),
            ("context", json!({"type": "integer"})),
        ],
            r#"Search file contents using regex (Python `re` syntax). Prefer this over `shell grep ...` - it's faster, structured (`path:line: text`), and capped to keep context small.
- All fields are required. Use empty/zero defaults for ones you don't need: `path: ""` (sandbox cwd), `glob: ""` (every text file), `case_insensitive: false`, `context: 0`.
- `path` accepts both **relative** (resolved against sandbox cwd) and **absolute** paths. Always pick a specific directory; never pass `/` or `~` to crawl your whole disk.
- Returned `path:line` references are **relative to the `path` you specified**. Noise dirs (`venv`, `.git`, `node_modules`, `__pycache__`, `dist`, `build`, `site-packages`, etc.) are auto-skipped.
- Binary files, files larger than 8 MB, and lines longer than 200 chars are auto-skipped/truncated.
- Three output_modes:
  - `content` - `path:line: matching_text` (default; use when you want to read matches)
  - `files_with_matches` - list of paths only (use to find which files to read next)
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
- Tactics for coverage: anchor on the definition first (`def `/`class `/`function `/`=` shapes) then widen to bare usages; if a symbol might be imported under an alias, also grep `import.*<name>` and `as <alias>`; if a first pattern returns nothing, broaden (drop the `(`, make it case-insensitive, widen the path/glob) rather than concluding it's absent."#),

        tool("glob", &[
            ("pattern", json!({"type": "string"})),
            ("path", json!({"type": "string"})),
            ("head_limit", json!({"type": "integer"})),
        ],
            r#"Find files by name pattern. Results are sorted newest-first (by modification time) so recently-edited files surface first.
- All fields required. Use `path: ""` for sandbox cwd; raise `head_limit` when you need to see everything.
- Like `grep`, `path` accepts both relative and absolute paths. Returned paths are relative to the `path` you specified. Noise dirs (`venv`, `.git`, `node_modules`, etc.) are skipped.
- Format: glob {"pattern": "**/*.ext", "path": "base_dir", "head_limit": 100}
- Examples:
  1. All Python files: glob {"pattern": "**/*.py", "path": "", "head_limit": 200}
  2. Recently-changed YAML in configs/: glob {"pattern": "**/*.yaml", "path": "configs", "head_limit": 20}
  3. Top-level test files: glob {"pattern": "test_*.py", "path": "", "head_limit": 50}"#),

        tool("scratchpad", &[("value", json!({"type": "string"}))],
            r#"Your durable note store while exploring. Every verified finding goes here immediately so the final exit report can be assembled from it without re-reading files.
- Purpose: persist `path:line` findings + key facts across iterations.
- Only write entries after the finding is confirmed by an actual `read`/`grep` result. Never assume.
- Use one entry per finding (don't pack multiple facts into one line).
- Use for:
  - confirmed `path:line` definitions, callers, and connections
  - file/folder layout summaries
  - exact code snippets you want to quote in the report (also note the file's language/extension so the fence tag - e.g. ```python - is ready at exit time)
  - open questions you still need to answer before exit
- Write `value` in Markdown — inline only (`**bold**`, backticks around `path:line` and symbols), never a line break.
- Format: scratchpad {"value": "<one-line verified note in markdown format>"}
- Examples:
  1. scratchpad {"value": "`AutoCua/mac/agent/service.py:254` — `_read_scratchpad_from_file` definition"}
  2. scratchpad {"value": "`AutoCua/mac/controller/view.py:620` — `action_type == \"scratchpad\"` routing branch"}
  3. scratchpad {"value": "**Still to verify:** any other callers of `_read_scratchpad_from_file` outside `agent/service.py`"}"#),

        tool("exit", &[("value", json!({"type": "string"}))],
            r#"Deliver your final findings report to the parent CLI agent and end the loop. **This is the only way to terminate.** The `value` is the summary, combined from your scratchpad findings: follow <handoff> and the mode's summary format.
- Write `value` in Markdown — headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the report needs them.
- Format: exit {"value": "<structured report in markdown format>"}
- Must be a standalone action - no other tool calls in the same step."#),
    ]].concat())
}

/// Per-action defaults, shared by the coder and the subagent (the
/// subagent's restriction is its name set, not its defaults): route_action
/// always receives every field of an action, even if the model omits an
/// optional-feeling one. Doubles as the field whitelist: an argument that is
/// not a key here is dropped from the action (and kept in its call). The key
/// order is the routed action's key order, which is printed and persisted.
/// The notes tools (NOTE_TOOLS: `insight` and `next_goal`): the loops record
/// them and never route them. The modes' switches (MODES, added last, one
/// field `value` each) are never routed either: the coder's loop answers
/// them.
pub fn action_defaults() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| {
        let mut defaults = json!({
            "insight": {"kind": "", "content": ""},
            "next_goal": {"value": ""},
            "shell": {"command": "", "input": ""},
            "read": {"path": "", "start": 0, "end": 0},
            "grep": {"pattern": "", "path": "", "glob": "", "output_mode": "content",
                     "case_insensitive": false, "head_limit": 50, "context": 0},
            "glob": {"pattern": "", "path": "", "head_limit": 100},
            "write": {"path": "", "line": 1, "content": ""},
            "replace": {"path": "", "line": 1, "old_block": "", "new_block": ""},
            "plan": {"op": "set", "from": 0, "to": 0, "value": ""},
            "web": {"value": ""},
            "todo_list": {"value": ""},
            "update_todo": {"value": ""},
            "wait": {"value": ""},
            "scratchpad": {"value": ""},
            "subagent": {"mode": "", "value": ""},
            "exit": {"value": ""},
        });
        for mode in MODES {
            defaults[mode.tool] = json!({"value": ""});
        }
        defaults
    })
}

fn names_of(tools: &[Value]) -> HashSet<String> {
    tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Names the coder may call: anything else is answered with an error tool
/// result instead of being silently dropped, so the model can correct itself
/// on the very next turn.
pub fn coder_tool_names() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| names_of(coder_tools()))
}

/// The subagent's allowed action names: a coder-only call (write, replace,
/// subagent, ...) is answered with an error result, never routed. This is
/// what enforces the read-only guarantee (its `shell` is only told not to
/// write, in its description).
pub fn subagent_tool_names() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| names_of(subagent_tools()))
}

// -- dialect emitters --------------------------------------------------------

/// OpenAI/OpenRouter/Groq/Together chat-completions function format. aws
/// takes this format too and makes Converse toolSpecs of it itself.
fn tools_openai(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| json!({"type": "function", "function": t}))
        .collect()
}

/// Carry the registry description into a dialect dict when present.
fn with_description(mut tool: Map<String, Value>, source: &Value) -> Value {
    if let Some(desc) = source.get("description").filter(|d| truthy(d)) {
        tool.insert("description".into(), desc.clone());
    }
    Value::Object(tool)
}

/// Anthropic Messages API tools format.
fn tools_anthropic(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("name".into(), t.get("name").cloned().unwrap_or(Value::Null));
            m.insert("input_schema".into(), t.get("parameters").cloned().unwrap_or(Value::Null));
            with_description(m, t)
        })
        .collect()
}

/// Gemini function declarations.
fn tools_gemini(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("name".into(), t.get("name").cloned().unwrap_or(Value::Null));
            m.insert("parameters".into(), t.get("parameters").cloned().unwrap_or(Value::Null));
            with_description(m, t)
        })
        .collect()
}

/// Perplexity agent API (Responses-style flat function tools).
fn tools_perplexity(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| {
            let mut m = Map::new();
            m.insert("type".into(), json!("function"));
            m.insert("name".into(), t.get("name").cloned().unwrap_or(Value::Null));
            m.insert("parameters".into(), t.get("parameters").cloned().unwrap_or(Value::Null));
            with_description(m, t)
        })
        .collect()
}

/// `registry` (the canonical tool list) in `provider`'s own tool format.
pub fn dialect_tools(provider: &str, registry: &[Value]) -> Vec<Value> {
    match provider {
        "anthropic" => tools_anthropic(registry),
        "google" => tools_gemini(registry),
        "perplexity" => tools_perplexity(registry),
        _ => tools_openai(registry),
    }
}

// -- Python's str() and int() on JSON values ---------------------------------

/// Python `str(value)` of a JSON-decoded value. Unlike crate::coder::py_str_of, a
/// list or dict comes out the way Python writes it (`['ls', '-la']`,
/// `{'a': 1}`) and a float as Python's repr (`1e-07`), so a coerced argument
/// reads exactly as in the Python coder.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python `repr()` of a JSON-decoded value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() => py_float_repr(f),
            _ => n.to_string(),
        },
        Value::String(s) => py_str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Python `repr()` of a str: single quotes unless the text holds a single
/// quote and no double quote; a backslash escape for the quote, the
/// backslash, tab, newline and carriage return; hex escapes for the other
/// control characters and the non-printable ones.
fn py_str_repr(s: &str) -> String {
    const BS: char = '\\';
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        let cp = c as u32;
        if c == quote || c == BS {
            out.push(BS);
            out.push(c);
        } else if c == '\t' {
            out.push(BS);
            out.push('t');
        } else if c == '\n' {
            out.push(BS);
            out.push('n');
        } else if c == '\r' {
            out.push(BS);
            out.push('r');
        } else if cp < 0x20 || cp == 0x7f {
            out.push_str(&format!("{BS}x{cp:02x}"));
        } else if cp < 0x7f || py_printable(c) {
            out.push(c);
        } else if cp <= 0xff {
            out.push_str(&format!("{BS}x{cp:02x}"));
        } else if cp <= 0xffff {
            out.push_str(&format!("{BS}u{cp:04x}"));
        } else {
            out.push_str(&format!("{BS}U{cp:08x}"));
        }
    }
    out.push(quote);
    out
}

/// Python's str.isprintable() for a non-ASCII character: false for the
/// assigned characters of the categories Cc, Cf, Cs, Co, Zl, Zp and Zs
/// (Unicode 15.1, Python 3.13). Unassigned code points count as printable
/// here.
fn py_printable(c: char) -> bool {
    !matches!(
        c as u32,
        0x0080..=0x00a0
            | 0x00ad
            | 0x0600..=0x0605
            | 0x061c
            | 0x06dd
            | 0x070f
            | 0x0890..=0x0891
            | 0x08e2
            | 0x1680
            | 0x180e
            | 0x2000..=0x200f
            | 0x2028..=0x202f
            | 0x205f..=0x2064
            | 0x2066..=0x206f
            | 0x3000
            | 0xd800..=0xf8ff
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
            | 0xf0000..=0xffffd
            | 0x100000..=0x10fffd
    )
}

/// Python's repr() of a float: the shortest digits that read back the same,
/// in fixed notation for exponents -4 to 15 (with ".0" when whole), else as
/// d.ddde+XX.
fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return (if f > 0.0 { "inf" } else { "-inf" }).to_string();
    }
    if f == 0.0 {
        return (if f.is_sign_negative() { "-0.0" } else { "0.0" }).to_string();
    }
    // Rust's {:e} gives the same shortest digits: "1.5e300", "-1e-7".
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1;
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else {
            let point = point as usize;
            if point >= digits.len() {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                // `digits` is ASCII, so any byte index is a char boundary.
                format!("{}.{}", &digits[..point], &digits[point..])
            }
        };
        format!("{sign}{body}")
    } else {
        let body = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let exp_sign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{body}e{exp_sign}{:02}", exp.unsigned_abs())
    }
}

/// Python `int(value)` of a JSON-decoded value, or None where int() raises:
/// a bool is 0 or 1, a float is cut toward zero, a string is read as a
/// decimal (surrounding whitespace, a sign and single underscores between
/// digits allowed; ASCII digits only). Past 64 bits it saturates, where
/// Python keeps a big int.
fn py_int(v: &Value) -> Option<Value> {
    match v {
        Value::Bool(b) => Some(json!(i64::from(*b))),
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(Value::Number(n.clone())),
        Value::Number(n) => {
            let t = n.as_f64()?.trunc();
            Some(if t >= i64::MIN as f64 && t < i64::MAX as f64 {
                json!(t as i64)
            } else if t >= 0.0 && t < u64::MAX as f64 {
                json!(t as u64)
            } else {
                json!(t as i64)
            })
        }
        Value::String(s) => py_int_of_str(s),
        _ => None,
    }
}

fn py_int_of_str(s: &str) -> Option<Value> {
    let text = py_strip(s);
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut n: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let d = c.to_digit(10)?;
        n = n.saturating_mul(10).saturating_add(i128::from(d));
    }
    let n = if negative { -n } else { n };
    Some(if let Ok(i) = i64::try_from(n) {
        json!(i)
    } else if let Ok(u) = u64::try_from(n) {
        json!(u)
    } else if n < 0 {
        json!(i64::MIN)
    } else {
        json!(u64::MAX)
    })
}

// -- tool_calls -> steps -----------------------------------------------------

/// Best-effort coercion to the default's type (models sometimes send '5' for
/// 5): a bool default takes a bool, else str(value).strip().lower() in
/// ("true", "1", "yes"); an int default takes int(value), or the default
/// where int() refuses; a str default takes a str, else str(value).
fn coerce(value: &Value, default: &Value) -> Value {
    match default {
        Value::Bool(_) => match value {
            Value::Bool(_) => value.clone(),
            other => {
                let s = py_strip(&py_str(other)).to_lowercase();
                Value::Bool(matches!(s.as_str(), "true" | "1" | "yes"))
            }
        },
        Value::Number(n) if n.is_i64() || n.is_u64() => py_int(value).unwrap_or_else(|| default.clone()),
        Value::String(_) => match value {
            Value::String(_) => value.clone(),
            other => Value::String(py_str(other)),
        },
        _ => value.clone(),
    }
}

/// Convert normalized provider tool calls into (actions, calls, rejects,
/// track) where:
///
///   actions: the `[{type, ...}]` dicts route_action consumes, every field
///            of the tool's defaults filled (coerced when sent, the default
///            when not), anything else dropped
///   calls:   parallel to actions: {"id", "name", "arguments"} for each, so
///            the loop can echo the model's OWN tool calls back in the next
///            request and match each result to its call id. Arguments are
///            kept EXACTLY as the model sent them, so the echoed transcript
///            never contradicts the schema that required them.
///   rejects: {"id", "name", "arguments", "error"} for a call naming a tool
///            that does not exist (or is not allowed now), or carrying no
///            arguments at all; the loop feeds these back as error tool
///            results
///   track:   the track params stitched from the step's calls (rejected ones
///            too): for each key, the first non-empty value, stripped.
///
/// `allowed` is the loop's name set (coder_tool_names or
/// subagent_tool_names), `defaults_map` its defaults (action_defaults; an
/// empty map means action_defaults, as the Python wrapper does) and
/// `track_params` its notes fields (empty means the legacy keys, which is
/// what both loops pass: each reads its notes off its own `insight` and
/// `next_goal` calls).
#[allow(clippy::type_complexity)]
pub fn tool_calls_to_steps(
    tool_calls: &Value,
    allowed: &HashSet<String>,
    defaults_map: &Value,
    track_params: &[(String, String)],
) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<(String, String)>) {
    let mut actions: Vec<Value> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut rejects: Vec<Value> = Vec::new();
    let mut track: Vec<(String, String)> = if track_params.is_empty() {
        LEGACY_TRACK_KEYS.iter().map(|k| (k.to_string(), String::new())).collect()
    } else {
        track_params.iter().map(|(k, _)| (k.clone(), String::new())).collect()
    };
    let empty = Map::new();
    let dm = match defaults_map.as_object().filter(|o| !o.is_empty()) {
        Some(o) => o,
        None => action_defaults().as_object().unwrap_or(&empty),
    };
    let list = tool_calls.as_array().cloned().unwrap_or_default();
    for (i, call) in list.iter().enumerate() {
        let name = call
            .get("name")
            .filter(|v| truthy(v))
            .map(|v| py_strip(&py_str(v)).to_string())
            .unwrap_or_default();
        let args = match call.get("arguments") {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let call_id = call
            .get("id")
            .filter(|v| truthy(v))
            .map(py_str)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("call_{i}"));
        for (key, slot) in track.iter_mut() {
            let v = args
                .get(key.as_str())
                .filter(|v| truthy(v))
                .map(|v| py_strip(&py_str(v)).to_string())
                .unwrap_or_default();
            if !v.is_empty() && slot.is_empty() {
                *slot = v;
            }
        }
        let defaults = dm.get(&name).and_then(Value::as_object);
        let (Some(defaults), true) = (defaults, allowed.contains(&name)) else {
            let shown = if name.is_empty() { "(unnamed)" } else { name.as_str() };
            let mut sorted_names: Vec<&str> = allowed.iter().map(String::as_str).collect();
            sorted_names.sort_unstable();
            rejects.push(json!({
                "id": call_id,
                "name": shown,
                "arguments": Value::Object(args),
                "error": format!(
                    "No tool named '{shown}' exists. Available tools: {}. Call one of those instead.",
                    sorted_names.join(", ")
                ),
            }));
            continue;
        };
        // EMPTY arguments is a schema violation, not a set of omitted
        // optional fields: every param on every tool is `required`. Letting
        // the defaults below fill it would silently promote the model's
        // malformed turn into a REAL action (a `write` with no arguments
        // writes "" to the path ""). Reject it exactly the way an unknown
        // tool name is rejected, so the model gets an error keyed to its own
        // call id and re-issues the step. Narrow on purpose: ONLY a fully
        // empty argument object; a partial call still gets default-filled.
        // The error names the tool's own fields: the notes live on their own
        // tools, so no action tool carries the track params.
        if !defaults.is_empty() && args.is_empty() {
            let mut required: Vec<String> = defaults.keys().cloned().collect();
            required.sort_unstable();
            rejects.push(json!({
                "id": call_id,
                "name": name,
                "arguments": Value::Object(args),
                "error": format!(
                    "'{name}' was called with no arguments at all. Every field is required: {}. Re-issue the call with all of them filled in.",
                    required.join(", ")
                ),
            }));
            continue;
        }
        let mut action = Map::new();
        action.insert("type".into(), json!(name));
        for (key, default) in defaults {
            let value = match args.get(key) {
                Some(v) => coerce(v, default),
                None => default.clone(),
            };
            action.insert(key.clone(), value);
        }
        actions.push(Value::Object(action));
        calls.push(json!({"id": call_id, "name": name, "arguments": Value::Object(args)}));
    }
    (actions, calls, rejects, track)
}

/// Backstop for an `async_tools` model (GPT-6 Astra, see responses.py).
///
/// With every tool async, nothing stops the model from bundling its terminal
/// call (`exit` for the coder and the subagent) after this turn's other
/// calls, whose results it has not seen; the controller would run them and
/// end the run on the terminal call, unverified. The terminal call is dropped
/// from the batch and answered with an error result keyed to its id, so the
/// other calls run, the replay stays complete (the caller's all_calls still
/// holds the call) and the model calls the terminal tool alone once it has
/// verified them. A turn whose only calls are terminal ones is returned
/// untouched. Returns (actions, calls, results); the caller prints under its
/// own stdout convention.
pub fn drop_bundled_terminal(
    actions: Vec<Value>,
    calls: Vec<Value>,
    rejects: &[Value],
    terminal: &str,
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let is_terminal = |a: &Value| a.get("type").and_then(Value::as_str) == Some(terminal);
    let bundled = actions.iter().any(is_terminal);
    let others = actions.iter().any(|a| !is_terminal(a)) || !rejects.is_empty();
    if !(bundled && others) {
        return (actions, calls, Vec::new());
    }
    let mut results = Vec::new();
    let mut kept_actions = Vec::new();
    let mut kept_calls = Vec::new();
    for (a, c) in actions.into_iter().zip(calls) {
        if is_terminal(&a) {
            results.push(json!({
                "tool_call_id": c.get("id").cloned().unwrap_or(Value::Null),
                "content": format!(
                    "error: {terminal} ignored, it must be the only call of its turn. This turn's other calls ran; verify them on the next input, then call {terminal} alone."
                ),
            }));
        } else {
            kept_actions.push(a);
            kept_calls.push(c);
        }
    }
    (kept_actions, kept_calls, results)
}

// -- usage normalization -----------------------------------------------------

/// Python `int(v or 0)`: 0 for a missing or falsy value, and for one int()
/// refuses (where Python would raise).
fn int_or_zero(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if truthy(v) => py_int(v)
            .and_then(|n| n.as_i64().or_else(|| n.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX))))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Normalize a provider usage dict to {input_tokens, output_tokens,
/// total_tokens, context_tokens}, tolerating both key styles (Anthropic-style
/// input/output and OpenAI-style prompt/completion). Empty/missing -> zeros.
/// A present `input_tokens` wins over `prompt_tokens` even when it is null.
///
/// context_tokens is the TRUE size of the prompt actually sent this turn: the
/// number memory compression triggers on. A cached token still occupies the
/// context window, so the cache classes are added back: input_tokens +
/// cache_read + cache_creation. Exact for every provider: Anthropic's
/// input_tokens EXCLUDES cache, so the cache fields are added; everyone
/// else's prompt_tokens already INCLUDES cached tokens and the Anthropic
/// cache keys are absent (0), so this collapses to the full prompt count.
pub fn normalize_usage(u: Option<&Value>) -> Value {
    let u = u.cloned().unwrap_or(json!({}));
    let inp = if u.get("input_tokens").is_some() {
        int_or_zero(u.get("input_tokens"))
    } else {
        int_or_zero(u.get("prompt_tokens"))
    };
    let out = if u.get("output_tokens").is_some() {
        int_or_zero(u.get("output_tokens"))
    } else {
        int_or_zero(u.get("completion_tokens"))
    };
    let tot = match int_or_zero(u.get("total_tokens")) {
        0 => inp + out,
        t => t,
    };
    let cache_read = int_or_zero(u.get("cache_read_input_tokens"));
    let cache_create = int_or_zero(u.get("cache_creation_input_tokens"));
    json!({
        "input_tokens": inp,
        "output_tokens": out,
        "total_tokens": tot,
        "context_tokens": inp + cache_read + cache_create,
    })
}

// -- the manager -------------------------------------------------------------

/// Why a send gave no reply.
pub enum SendErr {
    /// The provider failed: logged and retried, as Python's `except Exception`.
    Failed(String),
    /// The API refused the request itself (llm_provider.RefusalError): the
    /// same request gets the same answer, so no retry. The fallback model
    /// still gets its one call.
    Refused(String),
    /// Ctrl+C (KeyboardInterrupt) or SystemExit while the provider waited.
    /// Not a failure: no retry, no fallback, and it reaches the caller as the
    /// same Python error.
    Interrupted(PyErr),
}

impl From<SendErr> for PyErr {
    fn from(e: SendErr) -> PyErr {
        match e {
            SendErr::Failed(msg) | SendErr::Refused(msg) => PyException::new_err(msg),
            SendErr::Interrupted(err) => err,
        }
    }
}

/// A Python error as a SendErr: an Exception is a failure (a RefusalError a
/// refusal) carrying str(e); anything else (KeyboardInterrupt, SystemExit)
/// passes through untouched.
fn send_err(py: Python<'_>, e: PyErr) -> SendErr {
    if !e.is_instance_of::<PyException>(py) {
        return SendErr::Interrupted(e);
    }
    let refused = py
        .import("AutoCua.llm_provider")
        .and_then(|m| m.getattr("RefusalError"))
        .and_then(|ty| e.value(py).is_instance(&ty))
        .unwrap_or(false);
    let msg = crate::coder::py_err_str(py, &e);
    if refused {
        SendErr::Refused(msg)
    } else {
        SendErr::Failed(msg)
    }
}

/// One shared Python provider (AutoCua/llm_provider, made by build_provider)
/// asked for one reply: the normalized response, choices[0].message with
/// content and tool_calls, plus usage. Takes the GIL for the call; the
/// provider gives it back while it waits on the network.
fn send_py(provider: &Py<PyAny>, messages: &[Value], model: &str, shots: &[String]) -> Result<Value, SendErr> {
    Python::attach(|py| {
        let reply = (|| -> PyResult<Value> {
            let messages = pythonize::pythonize(py, messages)?;
            // One image goes as the string every provider has always taken;
            // several as a list of them. The coder sends none.
            let shot: Option<Bound<'_, PyAny>> = match shots {
                [] => None,
                [one] => Some(PyString::new(py, one).into_any()),
                many => Some(PyList::new(py, many)?.into_any()),
            };
            let out = provider.bind(py).call_method1("send_request", (messages, model, shot))?;
            match pythonize::depythonize(&out) {
                Ok(reply) => Ok(reply),
                // A number no serde_json number holds (a tool argument past 64
                // bits): read the JSON text instead, where it becomes a float.
                Err(_) => {
                    let text: String = py.import("json")?.call_method1("dumps", (&out,))?.extract()?;
                    serde_json::from_str(&text).map_err(|e| PyException::new_err(e.to_string()))
                }
            }
        })();
        reply.map_err(|e| send_err(py, e))
    })
}

/// The 1 second pause before a retry (Python's time.sleep(1)), slept in short
/// pieces so Ctrl+C gets through. The caller holds no GIL while it sleeps.
fn retry_pause() -> Result<(), SendErr> {
    let end = Instant::now() + Duration::from_secs(1);
    loop {
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        std::thread::sleep((end - now).min(Duration::from_millis(50)));
        Python::attach(|py| py.check_signals()).map_err(SendErr::Interrupted)?;
    }
}

/// Python's type name of a JSON value, for the error texts Python would give.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

pub enum SendOutcome {
    /// Native-tools mode: {"text", "tool_calls", "provider_meta"}.
    Native(Value),
    /// mode="text": plain prose.
    Text(String),
}

struct CoreState {
    model: String,
    has_vision: bool,
    display_name: String,
    model_info: Value,
    last_usage: Value,
    last_call_seconds: f64,
}

/// Manager to route requests to the correct LLM provider: the shared
/// LLMManager with the coder's and the subagent's tool lists.
#[pyclass(frozen, name = "LLMManager")]
pub struct LLMManager {
    pub provider: String,
    pub model_short_name: String,
    /// Runtime key from the caller (priority over .env and the saved key).
    pub runtime_api_key: Option<String>,
    /// Coder/subagent flag: it turns on the one-call fallback model. NOT a
    /// "native tools" switch: every mode but "text" is native. Each provider
    /// also gates its screenshot splice on `not cli_agent` (the coder sends
    /// no screenshot).
    pub cli_agent: bool,
    /// "main" (the coder's tools) | "subagent" (the subagent's) | "text"
    /// (no tools: the memory-compression handoff).
    pub mode: String,
    /// "quality" | "fast": kept as given; the coder's tools do not use it.
    pub speed: String,
    pub native_tools: bool,
    cli_fallback_model: Option<String>,
    primary_model_info: Value,
    /// The shared Python provider holding this mode's tool list.
    provider_impl: Py<PyAny>,
    /// The provider's full tool list, read off it on the first hide_tools
    /// call; None when the provider has no tools.
    full_tools: OnceLock<Option<Py<PyAny>>>,
    state: Mutex<CoreState>,
}

/// AutoCua/llm_provider/llm_manager.py, the manager module every platform shares.
fn shared_llm(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import("AutoCua.llm_provider.llm_manager")
}

/// `short_name`'s entry in the shared model tables (AutoCua/llm_provider/<p>/view.py),
/// or a passthrough entry for a name they do not register.
fn resolve_model_info(py: Python<'_>, provider: &str, short_name: &str) -> PyResult<Value> {
    let entry = shared_llm(py)?.call_method1("resolve_model_info", (provider, short_name))?;
    Ok(pythonize::depythonize(&entry)?)
}

/// A provider tool's name in any dialect (the shared _dialect_tool_name): at
/// the top (Anthropic, Gemini, Perplexity) or under `function` (chat
/// completions); "" when neither has one.
fn dialect_tool_name(t: &Bound<'_, PyAny>) -> String {
    fn get<'py>(obj: &Bound<'py, PyAny>, key: &str) -> Option<Bound<'py, PyAny>> {
        obj.call_method1("get", (key,))
            .ok()
            .filter(|v| v.is_truthy().unwrap_or(false))
    }
    get(t, "name")
        .or_else(|| get(t, "function").and_then(|f| get(&f, "name")))
        .and_then(|n| n.extract::<String>().ok())
        .unwrap_or_default()
}

impl LLMManager {
    /// The manager for one agent. `mode` picks the tool list: "main" the
    /// coder's, "subagent" the subagent's, "text" none (plain prose, output
    /// capped at HANDOFF_MAX_TOKENS). `cli_agent` turns on the one-call
    /// fallback model. Raises what build_provider raises (no API key, an
    /// unsupported provider).
    pub fn build(
        py: Python<'_>,
        provider: &str,
        model: &str,
        api_key: Option<&str>,
        cli_agent: bool,
        mode: &str,
        speed: &str,
    ) -> PyResult<Self> {
        let llm = shared_llm(py)?;
        let provider = provider.to_lowercase();
        // NATIVE TOOL CALLING is the only path: the coder and the subagent
        // get tool definitions as their output contract, no JSON envelope,
        // no response_format, nothing to parse. The single exception is
        // mode="text" (the memory-compression handoff), which wants plain
        // prose and so gets neither tools nor a schema.
        let native_tools = mode != "text";
        // Output cap handed to the provider: 10k for the handoff agent, None
        // for every other mode (each provider keeps its own default).
        let max_tokens: Option<i64> = if native_tools {
            None
        } else {
            Some(llm.getattr("HANDOFF_MAX_TOKENS")?.extract()?)
        };
        // The coder and the subagent (cli_agent) run the model they were
        // given. The only per-provider hardcoding is the emergency fallback,
        // used for one call (see send_rust).
        let cli_fallback_model: Option<String> = if cli_agent {
            llm.call_method1("_pick_cli_fallback", (&provider, model))?.extract()?
        } else {
            None
        };
        let model_info = resolve_model_info(py, &provider, model)?;
        let api_name = model_info.get("api_name").and_then(Value::as_str).unwrap_or(model).to_string();

        // Hand the provider its dialect's tool definitions, picked by mode.
        let registry: Option<&Vec<Value>> = if !native_tools {
            None
        } else if mode == "subagent" {
            Some(subagent_tools())
        } else {
            Some(coder_tools())
        };
        let tools: Option<Vec<Value>> = registry.map(|r| dialect_tools(&provider, r));
        let kwargs = PyDict::new(py);
        kwargs.set_item("cli_agent", cli_agent)?;
        kwargs.set_item("max_tokens", max_tokens)?;
        kwargs.set_item("api_key", api_key)?;
        let provider_impl = llm
            .call_method(
                "build_provider",
                (
                    provider.as_str(),
                    model,
                    pythonize::pythonize(py, &model_info)?,
                    pythonize::pythonize(py, &tools)?,
                ),
                Some(&kwargs),
            )?
            .unbind();

        Ok(LLMManager {
            provider,
            model_short_name: model.to_string(),
            runtime_api_key: api_key.map(str::to_string),
            cli_agent,
            mode: mode.to_string(),
            speed: speed.to_string(),
            native_tools,
            cli_fallback_model,
            primary_model_info: model_info.clone(),
            provider_impl,
            full_tools: OnceLock::new(),
            state: Mutex::new(CoreState {
                model: api_name,
                has_vision: model_info.get("vision").and_then(Value::as_bool).unwrap_or(true),
                display_name: model_info
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(model)
                    .to_string(),
                model_info,
                last_usage: json!({}),
                last_call_seconds: 0.0,
            }),
        })
    }

    fn state(&self) -> MutexGuard<'_, CoreState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Point this manager at a model. Also re-points the provider's
    /// model_info, which openrouter/groq/perplexity cache at construction;
    /// otherwise a swapped model inherits the other model's registry entry.
    fn apply_model_info(&self, info: &Value) {
        {
            let mut state = self.state();
            state.model = info.get("api_name").and_then(Value::as_str).unwrap_or_default().to_string();
            state.has_vision = info.get("vision").and_then(Value::as_bool).unwrap_or(true);
            state.display_name = info.get("display_name").and_then(Value::as_str).unwrap_or_default().to_string();
            state.model_info = info.clone();
        }
        Python::attach(|py| {
            let Ok(entry) = pythonize::pythonize(py, info) else { return };
            let p = self.provider_impl.bind(py);
            if p.hasattr("model_info").unwrap_or(false) {
                let _ = p.setattr("model_info", &entry);
            }
        });
    }

    /// `response['choices'][0]['message']` read as the Python manager reads
    /// it: the reply, or the text of the error Python would raise there (a
    /// failed attempt, retried).
    fn read_message(&self, response: &Value) -> Result<SendOutcome, String> {
        let message = match response.get("choices") {
            None => return Err("'choices'".to_string()),
            Some(Value::Array(choices)) => match choices.first() {
                None => return Err("list index out of range".to_string()),
                Some(Value::Object(choice)) => match choice.get("message") {
                    Some(m) => m,
                    None => return Err("'message'".to_string()),
                },
                Some(other) => return Err(format!("'{}' object is not subscriptable", py_type(other))),
            },
            Some(other) => return Err(format!("'{}' object is not subscriptable", py_type(other))),
        };
        let Some(message) = message.as_object() else {
            return Err(if self.native_tools {
                format!("'{}' object has no attribute 'get'", py_type(message))
            } else {
                format!("'{}' object is not subscriptable", py_type(message))
            });
        };
        if self.native_tools {
            let pick = |key: &str, empty: Value| message.get(key).filter(|v| truthy(v)).cloned().unwrap_or(empty);
            return Ok(SendOutcome::Native(json!({
                "text": pick("content", json!("")),
                "tool_calls": pick("tool_calls", json!([])),
                // Provider metadata for THIS turn, persisted with the step and
                // handed back to the same provider next request (see
                // META_KEY). {} when the provider emits none.
                META_KEY: pick(META_KEY, json!({})),
            })));
        }
        match message.get("content") {
            None => Err("'content'".to_string()),
            Some(Value::String(s)) => Ok(SendOutcome::Text(s.clone())),
            // No content (a model that spent its whole budget thinking):
            // empty, which memory compression drops, as it drops Python's
            // None. Never the string "None".
            Some(Value::Null) => Ok(SendOutcome::Text(String::new())),
            Some(other) => Ok(SendOutcome::Text(py_str(other))),
        }
    }

    /// Three idempotent tries against the model currently loaded. Errors
    /// carry the last provider message.
    ///
    /// Return shape: plain content string in text mode; in native-tools mode
    /// {"text": str, "tool_calls": [{"id", "name", "arguments"}],
    /// "provider_meta": dict}, the provider having already normalized the
    /// calls.
    fn attempt(&self, messages: &[Value], shots: &[String]) -> Result<SendOutcome, SendErr> {
        let mut last_error = String::new();
        for attempt in 0..3 {
            // Providers may mutate messages in place (e.g. wrapping the last
            // user message into multimodal content blocks); copy per attempt
            // so those mutations cannot compound across retries.
            let mut attempt_messages: Vec<Value> = messages.to_vec();
            if !META_PROVIDERS.contains(&self.provider.as_str()) {
                for m in attempt_messages.iter_mut() {
                    if let Some(obj) = m.as_object_mut() {
                        obj.shift_remove(META_KEY);
                    }
                }
            }
            let (model, display_name) = {
                let state = self.state();
                (state.model.clone(), state.display_name.clone())
            };
            // Pure API round-trip: the clock starts the moment the provider
            // is pinged and stops the moment its response is back, fresh on
            // EVERY attempt; the copy above is outside the clock.
            let t0 = Instant::now();
            let failure = match send_py(&self.provider_impl, &attempt_messages, &model, shots) {
                Ok(response) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    self.state().last_call_seconds = elapsed;
                    let raw_usage = response.get("usage").filter(|u| truthy(u));
                    if !response.is_object() || raw_usage.is_some_and(|u| !u.is_object()) {
                        // Python's response.get / usage.get raise here.
                        let odd = raw_usage.unwrap_or(&response);
                        format!("'{}' object has no attribute 'get'", py_type(odd))
                    } else {
                        // Set BEFORE the message is read, as Python does.
                        let usage = normalize_usage(raw_usage);
                        self.state().last_usage = usage.clone();
                        if std::io::stdout().is_terminal() {
                            // \r first: overwrite any spinner residue so the
                            // line is clean. TTY only: never in a pipe. Token
                            // counts ride on the same line: a slow call with a
                            // small input/output is NOT a generation cost, it
                            // is cold prefill, provider-side reasoning, or
                            // backend routing.
                            let read = raw_usage.and_then(|u| u.get("cache_read_input_tokens")).filter(|v| truthy(v));
                            let details = raw_usage
                                .and_then(|u| u.get("prompt_tokens_details"))
                                .filter(|d| truthy(d))
                                .and_then(|d| d.get("cached_tokens"))
                                .filter(|v| truthy(v));
                            let cached = int_or_zero(read.or(details));
                            crate::say!(
                                "\r⏱ LLM call: {elapsed:.2}s ({display_name}) | in {} (cached {cached}) | out {}",
                                int_or_zero(usage.get("input_tokens")),
                                int_or_zero(usage.get("output_tokens")),
                            );
                        }
                        match self.read_message(&response) {
                            Ok(out) => return Ok(out),
                            Err(e) => e,
                        }
                    }
                }
                Err(SendErr::Failed(e)) => e,
                Err(SendErr::Refused(e)) => {
                    // The API refused the request itself; the same request
                    // gets the same answer, so no retry. send_rust falls back
                    // where it can.
                    crate::say!("⛔ {display_name} refused the request: {e}");
                    return Err(SendErr::Refused(e));
                }
                Err(e @ SendErr::Interrupted(_)) => return Err(e),
            };
            last_error = failure;
            if attempt < 2 {
                crate::say!("⚠️ {display_name} request failed (attempt {}/3): {last_error}", attempt + 1);
                crate::say!("   Retrying in 1 second with a fresh message copy...");
                retry_pause()?;
                continue;
            }
            crate::say!("❌ {display_name} request failed after 3 attempts: {last_error}");
        }
        Err(SendErr::Failed(last_error))
    }

    /// Send a request to the provider with idempotent retries.
    ///
    /// Coder/subagent only (cli_agent): if the model fails all 3 attempts,
    /// ONE call is served by the per-provider fallback (3 attempts of its
    /// own). Win or lose, the manager is put back on the given model
    /// afterwards, so the next step starts on it again. If the fallback also
    /// fails its 3 attempts the error propagates and the agent stops. Without
    /// cli_agent: 3 attempts, then the error.
    ///
    /// GIL-free: callers detach around this; it takes the GIL only for the
    /// provider call and the Ctrl+C checks.
    pub fn send_rust(&self, messages: &[Value], shots: &[String]) -> Result<SendOutcome, SendErr> {
        let first_error = match self.attempt(messages, shots) {
            Ok(out) => return Ok(out),
            Err(e @ SendErr::Interrupted(_)) => return Err(e),
            Err(e) => e,
        };
        let Some(fallback_model) = self
            .cli_fallback_model
            .as_deref()
            .filter(|m| self.cli_agent && !m.is_empty())
        else {
            return Err(first_error);
        };
        let fallback_info = Python::attach(|py| {
            resolve_model_info(py, &self.provider, fallback_model).map_err(|e| send_err(py, e))
        })?;
        let fallback_display = fallback_info
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model)
            .to_string();
        let display_name = self.state().display_name.clone();
        crate::say!("⚠️ {display_name} failed — this step only, falling back to {fallback_display}...");
        self.apply_model_info(&fallback_info);
        let result = self.attempt(messages, shots);
        // Always revert: the fallback covers this call, not the whole run.
        self.apply_model_info(&self.primary_model_info);
        match result {
            Ok(out) => {
                let display_name = self.state().display_name.clone();
                crate::say!("↩️ Back on {display_name} for the next step.");
                Ok(out)
            }
            Err(e @ SendErr::Interrupted(_)) => Err(e),
            Err(e) => {
                crate::say!("❌ Fallback {fallback_display} also failed 3 attempts — stopping.");
                Err(e)
            }
        }
    }

    /// Send every tool but the ones named in `names`, or every tool when it
    /// is empty: the coder hides the tools of the modes that are on (MODES).
    /// The provider holds its tools in its own dialect (it may have reworked
    /// the schemas it was given) and reads them on every request, so the cut
    /// list is set on it; the full list is read off it once. Needs the GIL:
    /// call it before detaching for send_rust.
    pub fn hide_tools(&self, py: Python<'_>, names: &[&str]) -> PyResult<()> {
        let provider = self.provider_impl.bind(py);
        if self.full_tools.get().is_none() {
            let full = provider.getattr("tools")?;
            let _ = self.full_tools.set((!full.is_none()).then(|| full.unbind()));
        }
        let Some(Some(full)) = self.full_tools.get() else {
            return Ok(());
        };
        if names.is_empty() {
            return provider.setattr("tools", full.bind(py));
        }
        let cut = PyList::empty(py);
        for t in full.bind(py).try_iter()? {
            let t = t?;
            if !names.contains(&dialect_tool_name(&t).as_str()) {
                cut.append(&t)?;
            }
        }
        provider.setattr("tools", cut)
    }

    /// True when the model batches only on the Responses endpoint with async
    /// tools (GPT-6 Astra, table flag `async_tools`): the loops add the
    /// <async_tools> prompt block and the bundled exit backstop for it. Read
    /// from the primary entry, so a one-call fallback does not flip it.
    pub fn async_tools(&self) -> bool {
        truthy(self.primary_model_info.get("async_tools").unwrap_or(&Value::Null))
    }

    /// Most recent send's normalized token usage (input/output/total/context);
    /// {} before the first reply.
    pub fn last_usage_value(&self) -> Value {
        self.state().last_usage.clone()
    }
}

#[pymethods]
impl LLMManager {
    #[new]
    #[pyo3(signature = (provider, model, api_key=None, cli_agent=false, mode=None, speed=None))]
    fn new(
        py: Python<'_>,
        provider: String,
        model: String,
        api_key: Option<Bound<'_, PyAny>>,
        cli_agent: bool,
        mode: Option<String>,
        speed: Option<String>,
    ) -> PyResult<Self> {
        let api_key = match &api_key {
            Some(v) if !v.is_none() => Some(v.str()?.extract::<String>()?),
            _ => None,
        };
        LLMManager::build(
            py,
            &provider,
            &model,
            api_key.as_deref(),
            cli_agent,
            mode.as_deref().unwrap_or("main"),
            speed.as_deref().unwrap_or("quality"),
        )
    }

    /// Send a request to the provider with idempotent retries: the reply dict
    /// in native-tools mode, the text in text mode. The GIL is released while
    /// it waits.
    #[pyo3(signature = (messages, annotated_screenshot_base64=None))]
    fn send_request<'py>(
        &self,
        py: Python<'py>,
        messages: Bound<'py, PyAny>,
        annotated_screenshot_base64: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let msgs: Vec<Value> = match pythonize::depythonize(&messages)? {
            Value::Array(a) => a,
            other => vec![other],
        };
        let shots: Vec<String> = annotated_screenshot_base64.into_iter().collect();
        let outcome = py.detach(|| self.send_rust(&msgs, &shots)).map_err(PyErr::from)?;
        match outcome {
            SendOutcome::Native(v) => pythonize::pythonize(py, &v).map_err(Into::into),
            SendOutcome::Text(s) => Ok(PyString::new(py, &s).into_any()),
        }
    }

    #[pyo3(name = "async_tools")]
    fn async_tools_py(&self) -> bool {
        self.async_tools()
    }

    /// Current model short name (keeps a vertex suffix for routing).
    fn get_model_name(&self) -> &str {
        &self.model_short_name
    }

    fn get_provider_name(&self) -> &str {
        &self.provider
    }

    #[getter]
    fn provider(&self) -> &str {
        &self.provider
    }

    #[getter]
    fn model_short_name(&self) -> &str {
        &self.model_short_name
    }

    #[getter]
    fn runtime_api_key(&self) -> Option<&str> {
        self.runtime_api_key.as_deref()
    }

    #[getter]
    fn cli_agent(&self) -> bool {
        self.cli_agent
    }

    #[getter]
    fn mode(&self) -> &str {
        &self.mode
    }

    #[getter]
    fn speed(&self) -> &str {
        &self.speed
    }

    #[getter]
    fn native_tools(&self) -> bool {
        self.native_tools
    }

    #[getter]
    fn model(&self) -> String {
        self.state().model.clone()
    }

    #[getter]
    fn display_name(&self) -> String {
        self.state().display_name.clone()
    }

    #[getter]
    fn has_vision(&self) -> bool {
        self.state().has_vision
    }

    #[getter]
    fn model_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let info = self.state().model_info.clone();
        pythonize::pythonize(py, &info).map_err(Into::into)
    }

    #[getter]
    fn last_usage<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pythonize::pythonize(py, &self.last_usage_value()).map_err(Into::into)
    }

    #[getter]
    fn last_call_seconds(&self) -> f64 {
        self.state().last_call_seconds
    }
}
