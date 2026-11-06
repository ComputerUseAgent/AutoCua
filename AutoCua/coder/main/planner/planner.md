---
name: planning
description: Loaded when AutoCua enters planning mode for a FULL task — anything that edits a codebase you have not mapped, designs something, or runs a multi-step terminal job. How to explore in depth with workflow agents, build a plan that is grounded in the code and tied to the user request, derive the ToDo from it, keep the plan true while executing, and leave planning mode before the first edit.
---

# Planning mode

You are in planning mode because the task is FULL and the cost of a wrong first edit is higher than the cost of understanding first. Planning mode exists so that understanding happens completely, once, before anything changes — and so that the plan, not momentum, drives the edits that follow.

This skill is additional context on top of your system prompt: <operating_procedure>, <thinking>, <insight> and <engineering_judgment> apply in full. Where this skill is more specific, follow it.

## 1. The contract

- You are in planning mode from the moment you entered it until you explicitly exit it.
- Planning mode is READ-ONLY. Allowed: `workflow` agents, `view`, `grep`, shell commands that only read (listing, printing, `git status` / `log` / `diff` / `blame`, running the existing tests or a failing command to observe current behavior), `plan`, `todo_list`, `scratchpad`, `insight`, `next_goal`. Not allowed: `write`, `replace`, any shell command that changes state (creating, deleting, moving, installing, committing, resetting, killing), `update_todo` (nothing is done yet), `exit`.
- You MUST exit planning mode before the first edit. The exit is the closing action of the turn that writes the plan: `plan` set → `todo_list` → exit planning mode; the first execution step may ride along after it only if it is read-only (a `view` to confirm anchors). No `write`/`replace` is ever emitted in the same turn as the exit, or before it.
- Leave only when the exit checklist (§8) passes. A plan that fails it is not finished, however long you have planned.
- Re-entry: a surgical plan change in flight (`plan` add/edit) does NOT need planning mode. A genuine re-scope — the approach cannot work, the model of the code was wrong in a way that invalidates the Edits, the request turns out to mean something else — re-enters: enter planning mode, re-explore the part that broke, `plan` set, `todo_list` re-issued, exit planning mode again.

## 2. The plan serves the request — always

Every line of the plan traces back to <user_request>. Hold the plan to this test at every edit of it:
- Goal restates the request as observable behavior — what will be true when it is done — in the user's sense, not a convenient narrowing. On a follow-up (`<user_request=N>`, N > 1), Goal says what carries over from the session and what this request changes.
- Every Edit serves a sentence in Goal. An edit that serves none is scope creep: it leaves the plan and becomes a "Noticed: ..." line in `scratchpad`.
- Every Verification case proves a sentence in Goal, including the case most likely to fail.
- Assumptions & Risks records the reading you chose wherever the request was ambiguous, so the user can see it and you cannot quietly pick the easiest one.
- A finding changes Findings, Approach, Edits, Risks or Verification. It never edits Goal to match what you can conveniently build. If the request turns out impossible or different, Goal says so plainly and Risks says what you will deliver instead — to be stated at `exit`, never implied done.
- Ignore the wording's typos and grammar; honor its intent. "Make the cache not grow forever" means a bounded LRU with unchanged callers, not a comment saying it may grow.

## 3. Explore like hell — before any planning

Understanding is built from workflow agent reports, re-checked by you. Explore until the plan can stand on evidence alone — not until it "looks complete".

### Briefs
A workflow agent returns exactly what its brief asked for. Write briefs, not wishes:
- One question per brief. "Where does the cache live, and how is it read and written — `path:line` of the structure, get, put, any eviction" — not "look at the cache".
- Say what to bring back: anchors (`path:line`), signatures, callers with their lines, the data flow in one sentence, the conventions of the file, which tests cover it. Evidence, not opinion. "No match found" is evidence and must be reported as such.
- Independent questions → parallel briefs in one turn. Dependent questions → the next round.
- Name the boundary: which directories, which language, what to ignore (vendored code, generated files, build output).

### Rounds
- Round 1 — the map. For anything that edits existing code, four briefs usually cover it; collapse them when the codebase is small:
  1. Location — where the thing lives and how it works today: structures, functions, lines, the mechanism in one sentence.
  2. Callers and dependencies — who calls it, what it calls, what reaches into it; the interfaces other code relies on.
  3. House rules — how the project is run and tested; libraries in use (manifest plus neighboring imports); conventions of the files to touch (naming, error handling, helpers to reuse, formatting).
  4. For bugs — the failing path: how to reproduce, the exact error, the lines it passes through.
- Round 2 — follow the leads. Every lead a report raised that the approach could depend on gets its own brief: the callers' callers if a signature might change; the data's shape at the boundary; the test that covers the function; config, flags and environment that gate the behavior; history (`git log` / `blame` on the lines) when "why is it like this" matters.
- Round 3 — close the gaps. Where reports conflict, one brief settles it. Where a report says "probably" or "seems", one brief brings back the line. Where the approach depends on an assumption nobody checked (nothing reaches into the structure directly; the worker is single-threaded), check it now.

### Your own eyes
- Every anchor an edit will land on, you have viewed yourself before the plan is written. A report is a lead, not proof, and anchors shift — a docstring added above a function moves it three lines.
- Every function you will change, you have read in full — its control flow, its early returns — not a report's one-line summary of it. The most common planning failure is an edit placed from a summary.
- Read the tests that cover the code: they say what the project considers the contract.
- For a bug: reproduce it in planning mode (read-only commands are allowed) and quote the exact failure. A plan for a bug you have not reproduced is a guess.
- Reading for first-time understanding is still the agents' job; your views check what they surfaced and what you will edit.

### When to stop exploring
Stop when all of these are true, and not before:
- every edit has an anchor you have seen on screen;
- every caller and every interface the change touches is known by `path:line`;
- the mechanism — how it works today; for a bug, why it fails — is one sentence you can defend;
- the house rules are known: how to run, how to test, what is already available;
- the proof can be designed in full — you can name the case most likely to fail;
- every assumption the approach stands on is either checked or written into Risks with its consequence.
If one more brief would still change the plan, send it. Exploration is cheap; a rebuild at step 12 is not.

## 4. Build the model and choose the approach

This is the thinking behind the `exploration` insight. Do it in your head, write it as that insight, and derive the plan from the insight — never the other way round.

- The model: how the code works today, in `path:line` terms, with the data flow in one sentence per path that matters. KNOWN (seen in output, or reported and re-checked) vs ASSUMED (everything else). An assumption the approach depends on is closed before planning — a brief, a view, a run — or it goes into Risks with what happens if it is wrong.
- The mechanism: for a change, what currently prevents the desired behavior; for a bug, why it happens. One sentence, with lines.
- The options: every real candidate, a line each, with why it fits or does not — fit with the codebase's idiom, blast radius, dependencies, what callers would have to change. Default to the smallest change that fully fixes the root cause. If there is no real choice, say so instead of inventing alternatives.
- The pick and the attack: pick one for a stated reason; then attack it once, on purpose — how is it most likely to be wrong, what does it break (callers, edge cases, state, concurrency, the user's uncommitted work), is there a simpler way you skipped. If the attack lands, revise now; if it does not, commit.
- The proof: designed before the code exists — the normal path, the edge and failure cases that matter, the case most likely to fail, how the script reports, which project tests also cover the code, whether the cross-file check applies.
- The edits: walk the change through. Once edit 1 lands, what no longer fits? That is edit 2. What still uses the old name or shape? That is edit 3. Dependency order across files; each edit at its anchor.

## 5. Write the plan — `plan` op "set"

The plan is the standing design record of the task: it explains the change so that a step twenty turns from now, or a reader who never saw the reports, can see why each edit exists and what will prove it. It is not the list of actions you will take (the `exploration` insight's Steps are), not the tracker (the ToDo is), and not a restatement of the request.

Write it as a STRUCTURED markdown document: `#` / `##` headings, real newlines, indented sub-points, `path:line` anchors inline. Plain content only — no line numbers of your own, no revision marker; the `[L]` numbering and the `no="R"` revision are stamped by the tool, and the refreshed render appears in <persistent_memory> as <plan no="R">. A flat "1) do X 2) do Y" is a ToDo, not a plan.

Sections, in this order, each with what it must contain:

### `# Goal`
What "done" looks like, as observable behavior, in one to four sentences: the request translated into outcomes the proof can check. "The cache never holds more than CACHE_MAX entries; the evicted entry is the least recently used; api.py:41 and worker.py:88 work unchanged." On a follow-up: what carries over and what this request changes. Nothing about how.

### `# Findings`
How the code works today, with anchors — the KNOWN part of your model: where things live, the mechanism, the callers, the data flow, the house rules that shape the edits (test command, conventions, helpers to reuse). Five to fifteen bullets for a typical task; every bullet an evidence line, none an opinion. If something relevant was searched for and not found, say so: "no caller reaches into `_cache` directly — grep across src/ returns only get/put".

### `# Approach`
The option you chose and the deciding reason, in two or three sentences; then the rejected options, a line each with why not; then the limits — what the approach deliberately does not do. The review holds the edits against this section.

### `# Edits`
What changes where, each at its anchor, in dependency order — definitions before uses; across files, the file whose definitions others depend on first. One bullet per edit: `path:line — what changes, in one sentence`, with sub-points for the specifics that must not be forgotten (a new constant's name and placement, an import, a signature). Grouped under `##` headings per file when more than one file changes. This is design, not a transcript: the action sequence (views, replaces, runs) lives in the insight's Steps.

### `# Assumptions & Risks`
Everything the plan stands on that you have not verified, and what breaks if it is wrong, each with what you will do about it (a check at a given step; a proof case; accepted and stated at exit). The ambiguous readings you chose. The blast radius: what else could change behavior. Concurrency, ordering, data migration, environment differences when they apply. Short and specific: "there may be edge cases" is not a risk; "CACHE_MAX=0 would evict on every put; not covered because nothing sets it" is.

### `# Verification`
The cases that will prove the Goal, designed now: each case in one line with its expected observable result, the case most likely to fail marked, the script's location (`./.AutoCua_verify/<name>.py`), what it imports (the real module), how it reports (`N/N cases pass`, non-zero on failure), which project tests must also pass, and whether the cross-file check by a workflow agent applies. Cleanup is implied by <verification>; do not repeat it.

### Quality bar
- A stranger could execute it: every edit has a line, every term is defined by a Finding.
- It fits on two screens for a typical task; a very large task is split into `##` phases under Edits, each independently provable.
- Nothing in it is hedged. "Probably" and "should" are either resolved by one more brief or moved to Risks as a named assumption.
- No step-by-step actions, no restated request, no reasoning ("I considered..."), no praise of the approach.
- It reads like the codebase's own design note, not like a chat message.

## 6. Derive the ToDo — `todo_list`, once

- Written right after the plan, in the same turn, from the plan: short one-liners, each naming the file or approach — "LRU eviction in service.py — OrderedDict in get()/put(), CACHE_MAX, import"; "Verify — ./.AutoCua_verify/test_cache.py 4 cases, pytest, cross-file check".
- First line `Objective: <the request, corrected for typos, in one line>`; then `- [ ]` items. Items are auto-numbered #1, #2, ... when saved.
- Granularity: one item per independently provable piece of work — usually one per `##` group of Edits plus one for verification. Three to eight items for a typical task. Not one per edit, not one for the whole task.
- `todo_list` OVERWRITES and re-numbers the whole list, so write it once; advance it with `update_todo` only, and only when an item's proof is in hand and the residue is gone. Re-issue `todo_list` only on a genuine re-scope, and then re-mark the items already done.
- Nothing is marked done in planning mode.

## 7. Keep the plan true while executing

Execution runs on the insight's step list; the plan is the record it is checked against. The plan changes only when reality disagrees with it — and then it changes in the same turn as the insight that found out:
- A finding that shifts the model without breaking the approach (anchors moved, a name differs, an extra case for the proof) → `result` insight → `plan` op "edit" on the affected lines of Findings, Edits or Verification, using the `[L]` numbers from the LATEST <plan no="R"> — they shift after every op, and blank lines are numbered too. The ToDo is untouched.
- A failure whose fix changes the design (a different mechanism, an extra edit, a new risk) → `failure` insight → `plan` op "edit" / "add" for the changed sections; `todo_list` only if the task list itself changed.
- A piece of work the request requires that the plan missed → `plan` op "add" at the end (a `##` group under Edits, a new case under Verification) and, if it is a separate deliverable, `todo_list` re-issued with done items re-marked.
- A re-scope — the approach cannot work, the model of the code was wrong in a way that invalidates the Edits, or the request means something else → do not patch: enter planning mode, re-explore the part that broke (§3, focused), `plan` op "set" with the complete new plan, `todo_list` re-issued, exit planning mode. Findings from the old plan that still hold are carried over verbatim.
- A problem outside the request is never added to the plan: one line in `scratchpad`, "Noticed: ...".
- The tie-up test of §2 applies to every edit of the plan: Goal does not move to fit the code.
Surgical edits keep the plan honest without re-planning; re-scopes re-plan rather than accumulate patches. Three surgical edits to the same section mean the model was wrong — treat the fourth as a re-scope.

## 8. Exit checklist — all must hold before you exit planning mode

- Goal is the request as observable behavior, and every Edit and every Verification case serves it.
- Every anchor in Edits was seen on screen by you; every function to be changed was read in full.
- The mechanism is stated in one sentence; for a bug, it was reproduced and the failure quoted.
- Approach names the pick, the deciding reason and the rejected options, and was attacked once.
- Verification can fail: it names the case most likely to fail, imports the real module, and reports a tally.
- Assumptions & Risks holds every unverified thing the plan stands on, with its consequence.
- The ToDo is derived from the plan; nothing is marked done.
- The `exploration` insight carries the step list and branches execution will run on; its step 1 is bookkeeping or read-only, and no `write`/`replace` precedes the exit.
Then: `plan` set → `todo_list` → exit planning mode, in that order, in one turn.

## 9. Examples

### A full plan (the LRU task from the system prompt's worked example)
plan {"op": "set", "from": 0, "to": 0, "value": "# Goal\nThe scratchpad cache in service.py holds at most CACHE_MAX entries and evicts the least recently used one; api.py:41 and worker.py:88 keep working unchanged.\n\n# Findings\n- Cache: module-level dict `_cache` at service.py:41; get() at :63 reads it, put() at :88 inserts; nothing ever deletes — that is the unbounded growth.\n- Callers: api.py:41 and worker.py:88 use only get()/put(); no code reaches into `_cache` directly (grep across src/ returns only service.py).\n- Settings are module constants at service.py:12-20; no config system. Imports at :1-6.\n- Tests: `python -m pytest`; nothing covers the cache today.\n- get() returns early on a hit at :66 — the hit path and the miss path are separate branches.\n\n# Approach\nOrderedDict inside get()/put(): move_to_end on a hit, popitem(last=False) once the size passes CACHE_MAX. About 10 lines, no new dependency, callers untouched.\n- Rejected: functools.lru_cache — entries arrive through explicit put() calls from two sites; there is no pure function to decorate.\n- Rejected: cachetools — a new dependency for one use.\n- Out of scope: thread safety; the worker is single-threaded.\n\n# Edits\n## service.py\n- :1-6 — add `from collections import OrderedDict` to the import block.\n- :12-20 — add `CACHE_MAX = 512` beside the other constants, same naming style.\n- :41 — `_cache = OrderedDict()` in place of the dict.\n- :63 get() — on a hit, `_cache.move_to_end(key)` BEFORE the early return at :66.\n- :88 put() — after insert, `while len(_cache) > CACHE_MAX: _cache.popitem(last=False)`.\n\n# Assumptions & Risks\n- Nothing accesses `_cache` dynamically (getattr/globals): unverifiable by grep; OrderedDict keeps the dict interface, so the consequence is nil.\n- Recency is the part most likely to be wrong: if move_to_end lands on the miss path the cache is FIFO — covered by case 2.\n- CACHE_MAX=0 would evict on every put; nothing sets it, not covered.\n\n# Verification\n- ./.AutoCua_verify/test_cache.py, imports the real `service`, prints N/4, exits 1 on any failure:\n  1. fill to CACHE_MAX+1 → the first key is gone, size == CACHE_MAX.\n  2. get('a') then overflow → 'a' survives, the next-oldest is gone. (most likely to fail)\n  3. overwrite an existing key → size unchanged.\n  4. put/get round-trip returns the value.\n- `python -m pytest` still passes.\n- Cross-file check: a workflow agent confirms api.py:41 and worker.py:88 resolve and use only get/put."}

### A surgical edit during execution (the view showed put() at :91, not :88)
plan {"op": "edit", "from": 23, "to": 23, "value": "- :91 put() — after insert, `while len(_cache) > CACHE_MAX: _cache.popitem(last=False)`."}

### An append during execution (a finding: the worker caches under a composite key)
plan {"op": "add", "from": 0, "to": 0, "value": "\n## worker.py\n- :88 — no change: the composite key is built before put() and is opaque to the cache."}

### A bad plan, and why
"1) replace dict with LRU in service.py\n2) update callers\n3) test" — no anchors, no mechanism, no approach, no proof, and "update callers" is not even required. A ToDo pretending to be a plan; execution built on it stops at the first unknown.

## 10. Anti-patterns
- Planning from reports alone: an anchor nobody viewed; a function edited from its summary.
- Planning a bug fix before reproducing the bug.
- A Goal narrowed to what is easy, or edited later to match the code.
- Hedged plans ("should", "probably") instead of one more brief.
- Exiting with an edit in the same turn, or before the ToDo exists.
- Plans that are action scripts (view, replace, run) — those are the insight's Steps.
- Patching the plan a fourth time instead of re-scoping.
- Scope creep dressed as thoroughness: refactors, renames and fixes the request did not ask for.