"""The iOS main driver's tool registry: every tool it can call, and the
LLMManager that hands them to the model. The manager itself (providers,
retries, the tool-call parser) is the same on every platform:
AutoCua/llm_provider/llm_manager.py. The driver imports everything
LLM-related from here."""

from AutoCua.llm_provider.llm_manager import (LLMManager as _LLMManager, NOTE_TOOLS, REASONING_OPTIONAL,
                                               _tool, drop_bundled_terminal, tool_calls_to_steps)


# ---------------------------------------------------------------------------
# NATIVE TOOLS - the main driver calls these tools natively; no JSON envelope.
# The provider returns structured calls, tool_calls_to_steps() converts them
# into the same `[{type, ...}]` action dicts route_action already consumes -
# the controller is untouched.
#
# In quality mode a `reasoning` tool carries the step's notes (thinking,
# memory, next_goal) and every action tool carries only its own fields; in
# fast mode a `memory` tool carries `next_goal` alone (see
# MAIN_NOTES_FIELDS_FAST). tool_choice is forced (required / {"type": "any"} /
# mode="ANY"), so a text-only step is unrepresentable; the text channel remains
# for free prose only.
# ---------------------------------------------------------------------------

# Fast mode's notes tool is named `memory` and carries ONE field: `next_goal`.
# thinking and the quality `memory` field are dropped ENTIRELY rather than left
# required-but-empty - a required field is an invitation to fill it, and every
# token spent narrating a plan is a token not spent on the action, which is
# the whole point of fast mode. next_goal alone carries the verdict, the forward
# plan (Now / Plan / Then) and the Expect guard (fast_system_prompt.md has no
# <memory> section: its rules are the <next_goal> section).
MAIN_NOTES_FIELDS_FAST = {
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (element_name/type/value), and any tool name + purpose + important result. Then the forward plan: "Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>." END with the predicted visible change of THIS step\'s action prefixed "Expect:", so the next step can verify against the new screenshot. 3-5 concise lines.',
    },
}

# The MAIN DRIVER's `reasoning` tool fields (quality mode; fast mode's
# `memory` tool carries `next_goal` alone, see MAIN_NOTES_FIELDS_FAST): the
# macOS driver's design in iPhone/iPad terms. The verdict is judged on the
# CURRENT screenshot for a UI action and on the returned result for a tool
# action (web, shell, video_player), targets are named by name/role in
# `next_goal` and locked to fresh [id]s in `memory`'s Targets line every
# step, and the guard is the visible change the next screenshot must show
# (or the tool's expected result). No S<n> codes, no "not required": a route
# step omits `thinking` and the snapshot shows "skipped".
MAIN_REASONING_FIELDS = {
    "thinking": {
        "type": "string",
        "description": 'Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route, it is used up, or a new surface appeared; a short freeform paragraph (RECOVERY) after a guard failed (on the screenshot, or in the returned result of web, shell or video_player); 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass "") on a route step: the previous "Next:" named this action, its guard holds (a UI guard on the current screenshot, a tool guard in that tool\'s result), and every named target resolves to exactly one [id].',
    },
    "memory": {
        "type": "string",
        "description": 'Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT screenshot for a UI guard (never on what a UI tool result claims) or on that tool\'s returned result for a tool guard (web, shell, video_player): "Last step worked: <what the screen or the result shows>", "Last step failed: <what it shows instead>" or "Last step unclear: <what is ambiguous, and what would settle it>". First step: "Task start." Then the key context (app/screen state; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: "Targets: id N (element_name/type/value), ..." resolved from the CURRENT <element_tree> before acting. 2-4 tight lines.',
    },
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal>: "Doing: <this step> (ToDo: <task_name>). If <the visible change the next screenshot must show, or the expected returned result of web/shell/video_player> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>." Never name a successor target by [id]: ids are re-assigned every scan. On a route step, "Doing" is the next line of the ROUTE and "Next:" the line after it.',
    },
}


MAIN_REASONING_TOOL = _tool("reasoning", MAIN_REASONING_FIELDS,
                            r"""Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its guard (the visible change the next screenshot must show; for web, shell and video_player, that tool's returned result); on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails, a new surface appears, or the route said "think". A screenshot that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.
- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is only partially visible, means this step thinks.
- A step may be `reasoning` alone when you need to think before you act (it costs a fresh screen scan); the next step then does the work. Do not chain several such steps.
- `vault` stays the only action call of its turn: `reasoning`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: App Store front, Search tab open, the search field is empty as predicted. Targets: id 19 (element_name='Search', type='searchfield').", "next_goal": "Doing: search for Netflix (ToDo: Update Netflix). If the results list shows the Netflix row → Next: think: confirm the row and route into its page."}
  2. New surface (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, the Mail compose sheet is open as predicted, To field empty. UNDERSTAND: known: To, Subject and the body are in the tree as text fields; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. tap Send, if the compose sheet closes and the mailbox is back → think: verify the mail in Sent, then mark the ToDo.", "memory": "Last step worked: compose sheet open, guard held. Targets: id 12 (element_name='To', type='textfield'), id 14 (element_name='Subject', type='textfield'), id 20 (element_name='Message body', type='textview').", "next_goal": "Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: tap the Send button."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "Failed: the To field is still empty after input on id 74; the screenshot shows the cursor in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: tap the To field first, then input. Guard: the To field shows the address. Route holds after this.", "memory": "Last step failed: To field empty, focus was in Subject. Correction carried: tap a field before typing into it in this compose sheet. Targets: id 74 (element_name='To', type='textfield').", "next_goal": "Doing: tap the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject."}""",
                            optional=REASONING_OPTIONAL)

MAIN_NOTES_TOOL_FAST = _tool("memory", MAIN_NOTES_FIELDS_FAST,
                             r"""Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.
- One field, `next_goal`, filled every step: key context, the forward plan (Now / Plan / Then) and the Expect guard the next step verifies against (see <next_goal>). There is no thinking field: the reasoning stays silent (see <silent_reasoning>).
- Every step resolves the plan's targets to fresh [id]s from the CURRENT <element_tree> and names them in `next_goal` before acting.
- `vault` stays the only action call of its turn: `memory`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: memory {"next_goal": "..."}
- Example:
  memory {"next_goal": "App Store Search tab open; search field id 19 (element_name='Search', type='searchfield'), search button id 21 (element_name='Search', type='button'). Now: tap id 19, type 'Netflix', tap id 21 (ToDo: Update Netflix). Plan: open its page -> tap Update. Then: tap the Netflix row. Expect: results list shows the Netflix app row."}""")


def _main_tools(track: dict, notes: dict = None) -> list:
    """The MAIN DRIVER's registry - one tool per action type. route_action and
    the frontend's tool-flow map key on these names and fields, so they must
    never drift.

    Descriptions are the system prompt's OWN tool text, copied verbatim from
    <tool_capability> / <os_interaction> / <task_completion> in
    agent/main_driver/system_prompt.md - the prompt stays the source of truth
    (it is user-managed and never edited from here); this just puts the same
    words where a native tool call can read them. Built twice, each with NO
    per-call params (`track` stays {}) and a dedicated notes tool in front:
    quality mode the `reasoning` tool (thinking optional, memory + next_goal
    every step), fast mode the `memory` tool (`next_goal` alone). Both modes
    share each action tool's description text.

    iOS-specific vs the macOS registry: `scroll` and `vault` carry `value`
    (not macOS's `direction`), `click` has no `clicks` field, and `vault` /
    `video_player` exist here only."""
    notes = [notes] if notes else []
    return notes + [
        # -- <tool_capability> ------------------------------------------------
        _tool("open_app", {"value": {"type": "string"}},
              'Launch an installed application directly by name - faster than searching on the device. Use the special name "home" to return to the home screen.\n'
              '    1. Requirement: Typically call wait 2-3 seconds after this tool to allow loading.\n'
              '    2. Success means the app is confirmed in the foreground (if the result says verified: false, confirm on the next screenshot). An unknown or ambiguous name returns an error naming the closest installed apps - re-issue with that exact name.\n'
              '    3. Format: open_app {"value": "app name"}\n'
              '    4. Examples:\n'
              '        1. open_app {"value": "disney+"}\n'
              '        2. open_app {"value": "home"}', track=track),
        _tool("wait", {"value": {"type": "string"}},
              'Pause before the next screen scan to allow UI loading. Never exceed 3 seconds at a time.\n'
              '    1. Format: wait {"value": "time in seconds"}\n'
              '    2. Examples:\n'
              '        1. wait {"value": "3"}\n'
              '        2. wait {"value": "2"}', track=track),
        _tool("web", {"value": {"type": "string"}},
              'Delegate to a specialized AI that fetches real-time information and provides data at runtime. Use it for speed instead of browsing manually on the phone.\n'
              '    1. Format: web {"value": "search query"}\n'
              '    2. Examples:\n'
              '        1. web {"value": "financial results of Nvidia Q4 2025"}\n'
              '        2. web {"value": "latest Netflix app version on the App Store"}', track=track),
        _tool("shell", {"value": {"type": "string"}},
              'Run a shell/zsh command on the host Mac where this agent is running - not on the iPhone. Use it to check information or perform actions on the host OS, then continue the task on the phone (e.g. photo sharing: read the photo names and details on the Mac first, then share those same photos from the phone). Accepts every shell command, including AppleScript via osascript.\n'
              '    1. Format: shell {"value": "command"}\n'
              '    2. Examples:\n'
              '        1. shell {"value": "ls ~/Pictures/holiday | head -5"}\n'
              '        2. shell {"value": "osascript -e \'tell application \\"Finder\\" to get name of every file of desktop\'"}', track=track),
        _tool("todo_list", {"value": {"type": "string"}},
              'Create the ToDo task list (iteration 1 by default; you may also create/expand it later if complexity emerges). See <todo_capability>.', track=track),
        _tool("update_todo", {"value": {"type": "string"}},
              'Tasks are auto-numbered #1, #2, #3, etc. when saved.\n'
              '    1. Update a task only after it is confirmed complete via <agent_history> and the effect is visible in the latest input (image or any relevant tag); one item per call.\n'
              '    2. Example: update_todo {"value": "1"}', track=track),
        _tool("vault", {"id": {"type": "integer"}, "value": {"type": "string"}},
              'Fill a secure credential straight from the vault - three-part action like scroll: the element [id] and the credential kind (value: username/password/phone_number/pin). The runtime types or taps it for you; secrets never appear in your context.\n'
              '    1. Critical: vault must be the ONLY action call of its turn (only your notes call may precede it), and it fills one element per step. This holds on EVERY step, including route steps where `thinking` is omitted.\n'
              '    2. Fill every required credential field (repeat vault across steps) before planning the next move.\n'
              '    3. PIN keypads - a separate button for each digit 0-9, like the lock-screen passcode or an app PIN pad: use id 0 with value "pin". The vault finds the digit keys and taps the whole PIN itself. Never tap PIN digits yourself.\n'
              '    4. The PIN result says whether every key press registered. Retry only when it says it is safe to try once more; if the PIN was not accepted, only part of it registered, or it was already tried twice, stop and ask the user.\n'
              '    5. Format: vault {"id": <element_id>, "value": "<credential_kind>"}\n'
              '    6. Examples:\n'
              '        1. vault {"id": 3, "value": "username"}\n'
              '        2. vault {"id": 4, "value": "password"}\n'
              '        3. vault {"id": 0, "value": "pin"}', track=track),
        _tool("video_player", {"value": {"type": "string"}},
              'Track and control full-screen video playback through the control center (works despite DRM screenshot restrictions). Commands: close, streaming (check whether content is playing), pause, play.\n'
              '    1. Format: video_player {"value": "one of: close/streaming/pause/play"}\n'
              '    2. Examples:\n'
              '        1. video_player {"value": "streaming"}\n'
              '        2. video_player {"value": "pause"}', track=track),
        _tool("scratchpad", {"value": {"type": "string"}},
              'Record a verified checkpoint or any critical fact (file path, metric, finding). Follow <scratchpad> rules.\n'
              '    1. Write `value` in Markdown - inline only (`**bold**`, backticks), never a line break.\n'
              '    2. Example: scratchpad {"value": "**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**"}', track=track),

        # -- <tool_capability> #10 + <task_completion> -------------------------
        _tool("done", {"value": {"type": "string"}},
              'End the task with an end-to-end summary of what was achieved. Dedicated final step - never combine with any other action; do cleanup and ToDo/scratchpad updates in the step before.\n'
              '    1. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.\n'
              '    2. Examples:\n'
              '        1. done {"value": "**Netflix updated** to the latest version - login verified and version noted."}\n'
              '        2. done {"value": "**Message sent to John** - delivery confirmed on screen."}\n'
              '    3. Only start completion after reviewing <agent_history> to confirm every requested task is finished.\n'
              '    4. Then do a final verification from the latest input (double-check the last steps match the request; if playback is DRM-blocked, verify via a video_player check).\n'
              '    5. Use `done` as a dedicated final step only:\n'
              '        1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.\n'
              '        2. Step 2: output ONLY Format: done {"value": "<end-to-end summary in markdown format>"}\n'
              '    6. Never combine `done` with any other action/tool in the same step.', track=track),

        # -- <os_interaction> -------------------------------------------------
        _tool("click", {"id": {"type": "integer"}},
              'Tap the centre of an element by its [id].\n'
              '    1. Examples:\n'
              '        1. click {"id": 4}\n'
              '        2. click {"id": 23}', track=track),
        _tool("input", {"id": {"type": "integer"}, "value": {"type": "string"}},
              'Type into an element by its [id]. Existing text in the field is auto-deleted before typing.\n'
              '    1. Examples:\n'
              '        1. input {"id": 3, "value": "Hi, how are you"}\n'
              '        2. input {"id": 4, "value": "conjuring"}', track=track),
        _tool("scroll", {"id": {"type": "integer"}, "value": {"type": "string"}},
              'Swipe within an element\'s bounds - three-part action: the element [id] and the direction (value: up/down/left/right).\n'
              '    1. To reveal content below the visible area, scroll "up"; to reveal content above, scroll "down".\n'
              '    2. To reveal content on the right, scroll "left"; to reveal content on the left, scroll "right".\n'
              '    3. Examples:\n'
              '        1. scroll {"id": 3, "value": "up"}\n'
              '        2. scroll {"id": 7, "value": "left"}', track=track),
    ]


MAIN_TOOLS = _main_tools({}, notes=MAIN_REASONING_TOOL)
MAIN_TOOLS_FAST = _main_tools({}, notes=MAIN_NOTES_TOOL_FAST)

# Per-action defaults for the main driver - guarantees route_action receives
# every field of an action, even if the model omits an optional-feeling one.
# Doubles as the field whitelist: fast mode's per-call `memory` is absent
# here, so it never reaches the controller. `reasoning` is listed so the
# parser accepts the call; the service pulls it out before routing.
MAIN_ACTION_DEFAULTS = {
    "reasoning": {"thinking": "", "memory": "", "next_goal": ""},
    "click": {"id": 0},
    "input": {"id": 0, "value": ""},
    "scroll": {"id": 0, "value": "up"},
    "vault": {"id": 0, "value": ""},
    "open_app": {"value": ""},
    "wait": {"value": ""},
    "web": {"value": ""},
    "shell": {"value": ""},
    "todo_list": {"value": ""},
    "update_todo": {"value": ""},
    "video_player": {"value": ""},
    "scratchpad": {"value": ""},
    "done": {"value": ""},
}

# Fast mode's notes tool is `memory` (next_goal alone) instead of `reasoning`;
# tool_calls_to_steps reads the defaults map to find the mode's notes tool.
MAIN_ACTION_DEFAULTS_FAST = {"memory": {"next_goal": ""},
                             **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}

# Names the driver may call - anything else is answered with an error tool
# result instead of being silently dropped, so the model can correct itself
# on the very next turn.
MAIN_TOOL_NAMES = frozenset(t["name"] for t in MAIN_TOOLS)
MAIN_TOOL_NAMES_FAST = frozenset(t["name"] for t in MAIN_TOOLS_FAST)


# ---------------------------------------------------------------------------
# CLAUDE: any Claude model, whatever the provider (anthropic direct, or a Claude
# entry on OpenRouter; see LLMManager.claude). The main driver runs
# on its own claude_prompt.md (in quality mode only) with its own notes tool,
# `agent_memory`: the `reasoning` tool's notes with `assessment` in place of
# `thinking` (memory + next_goal every step, assessment when <assessment>
# calls for it).
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
                 "- `vault` stays the only action call of its turn: `agent_memory`, then `vault`, nothing else. "
                 f"Not used on the `{final}` step: `{final}` is always the only call of its turn.\n"
                 '- Format: agent_memory {"assessment": "", "memory": "...", "next_goal": "..."}',
                 optional=REASONING_OPTIONAL)


_CLAUDE_REASONING_DEFAULTS = {"assessment": "", "memory": "", "next_goal": ""}
MAIN_TOOLS_CLAUDE = _main_tools({}, notes=_claude_reasoning_tool("done", batched=True))
MAIN_ACTION_DEFAULTS_CLAUDE = {"agent_memory": dict(_CLAUDE_REASONING_DEFAULTS),
                               **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}
MAIN_TOOL_NAMES_CLAUDE = frozenset(t["name"] for t in MAIN_TOOLS_CLAUDE)


class LLMManager(_LLMManager):
    """The shared LLMManager with the iOS main driver's tool lists."""
    MAIN_TOOLS = MAIN_TOOLS
    MAIN_TOOLS_FAST = MAIN_TOOLS_FAST
    MAIN_TOOLS_CLAUDE = MAIN_TOOLS_CLAUDE
