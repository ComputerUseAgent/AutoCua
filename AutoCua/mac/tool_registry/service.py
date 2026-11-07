"""The macOS main driver's tool registry: every tool it can call, and the
LLMManager that hands them to the model. The manager itself (providers,
retries, fallback, the tool-call parser) is the same on every platform:
AutoCua/llm_provider/llm_manager.py. The driver imports everything
LLM-related from here."""

from AutoCua import data_extraction
from AutoCua.llm_provider.llm_manager import (LLMManager as _LLMManager, NOTE_TOOLS, REASONING_OPTIONAL,
                                               _tool, drop_bundled_terminal, tool_calls_to_steps)


# ---------------------------------------------------------------------------
# NATIVE TOOLS: the main driver calls these tools natively; no JSON envelope.
# The provider returns structured calls, tool_calls_to_steps() converts them
# into the same `[{type, ...}]` action dicts route_action already consumes,
# and the controller is untouched.
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
# plan (Now / Plan / Then) and the Expect guard.
MAIN_NOTES_FIELDS_FAST = {
    "next_goal": {
        "type": "string",
        "description": 'Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (name/type/valuePattern.value/active), and any tool name + purpose + important result. Then the forward plan: "Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>." END with the predicted visible change of THIS step\'s action prefixed "Expect:", so the next step can verify against the new screenshot. 3-5 concise lines.',
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
                "hotkey", "screenshot", "drag_drop", "open_app", "applescript")


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

    macOS-specific vs the Windows registry: `input`/`typewrite` carry `value`
    (not `text`), and `applescript` (app + value) and `drag_drop` exist here
    only. `clicks` stays on right_click/screenshot even though the controller
    ignores it, because the old strict union required it.

    While the dataset recorder is on, live_tools withholds the tools a
    recorded run must not teach (data_extraction.OFF_TOOLS)."""
    notes = [notes] if notes else []
    return data_extraction.live_tools(notes + [
        # -- <Tool_Capability> ------------------------------------------------
        _tool("open_app", {"value": {"type": "string"}},
              'Launch an installed application (.app only). No manual search required within the OS.\n'
              '    1. Requirement: Typically call wait 3 seconds immediately after this tool to allow loading.\n'
              '    3. Example: open_app {"value": "spotify"}', track=track),
        _tool("wait", {"value": {"type": "string"}},
              'Pause execution to allow UI loading or to trigger a fresh screen scan.\n'
              '    2. Example: wait {"value": "2"}', track=track),
        _tool("scan", {"value": {"type": "string", "enum": ["on", "off"]}},
              'Switch the screen scan on or off. The scan is on by default, and every new user request starts with it on. A switch takes effect from the next step.\n'
              f'    1. off: use it when the next steps only run through `shell` (zsh commands) and the other tools that need no screen (web, sub-agents, scratchpad, todo). Every scanned step sends a screenshot and an <element_tree>; with scan off neither is sent, which cuts the input tokens and saves money, and the screen tools ({", ".join(SCREEN_TOOLS)}) are removed. Turn it off once your last screen action was judged on a screenshot.\n'
              '    2. Keep it off for as long as the following steps do not need the screen.\n'
              '    3. on: switch it on in the step before one that needs the screen (a click, a key, a visual check). From the next step the screenshot, the <element_tree> and every tool are back.\n'
              '    4. Example: scan {"value": "off"}', track=track),
        _tool("web", {"value": {"type": "string"}},
              'Delegate to a specialized AI to fetch real-time information and provide data at runtime. Use this for speed instead of manual browsing.\n'
              '    2. Example: web {"value": "financial result of nvidia Q4 2025"}', track=track),
        _tool("sub_agent", {"agent_type": {"type": "string", "enum": ["coder_agent", "browser_agent", "ios_agent", "android_agent"]},
                            "value": {"type": "string"}},
              'Start a sub-agent on a task. It works in parallel while you carry on. It takes the first empty slot of its type in <sub_agents>, and the result names its id. When every slot of its type is busy, nothing starts: wait for one of them to finish (agent_wait).\n'
              '    1. `agent_type`:\n'
              '      1. `coder_agent`: complex coding and multi-step shell work. Up to 4 at once (ids 1-4).\n'
              '      2. `browser_agent`: acts on websites in its own Chrome browser (navigate, click, fill forms). Up to 4 at once (ids 5-8), each in a tab of its own. Its Chrome window opens in front of your screen. Check <sub_agents>: while a browser_agent reads running, never touch that window, and bring your own app forward with open_app. It runs on the agent\'s own Chrome profile, not the person\'s browser, so the person\'s signed-in accounts are not there: buying and anything that needs their accounts stay with you.\n'
              '      3. `ios_agent`: works on the person\'s real iPhone or iPad, plugged into this Mac by cable: opens its apps, taps, types, scrolls and reads its screen. One slot: id 9 or 10, as <sub_agents> shows it. Only while it reads empty; `connecting` means the phone is still being connected.\n'
              '      4. `android_agent`: works on the person\'s Android phone, plugged into this Mac by USB: opens its apps, taps, types, scrolls and reads its screen. One slot: id 9 or 10, as <sub_agents> shows it. Only while it reads empty; `connecting` means the phone is still being checked.\n'
              '    2. `value`: the full instruction. It is all the sub-agent gets, so make it self-contained.\n'
              '    3. Example: sub_agent {"agent_type": "coder_agent", "value": "instruction"}', track=track),
        _tool("agent_wait", {"agent_type": {"type": "string", "enum": ["coder_agent", "browser_agent", "ios_agent", "android_agent"]},
                             "agent_id": {"type": "integer"},
                             "value": {"type": "string"}},
              'Hold the pipeline until one sub-agent finishes. Use it only when your next step needs its report, when every slot of the type you need is busy, or before done.\n'
              '    1. `agent_type` and `agent_id`: the sub-agent to hold for, as its slot in <sub_agents> shows them.\n'
              '    2. If another sub-agent finishes first, the hold ends with that report; call agent_wait again to keep holding.\n'
              '    3. Example: agent_wait {"agent_type": "coder_agent", "agent_id": 1, "value": "Reason"}', track=track),
        # shell point 2, click by point with no mouse: the osascript (JXA) line
        # converts 0-1000 to the scanner's display (as tree/element.py
        # get_screen), then System Events clicks there, pressing the element at
        # that point. click() needs the null. No drag: without the mouse there
        # is none. Tested live on Chess, 2026-11-01.
        _tool("shell", {"value": {"type": "string"}},
              'Run a shell/zsh command for fast execution to achieve the goal.\n'
              '    1. Example:\n'
              '      1. shell {"value": "rm -rf ~/.Trash/*"}\n'
              '      2. shell {"value": "osascript -e \'tell application \\"Reminders\\"\\nset dueDate to current date\\nset year of dueDate to 2026\\nset month of dueDate to 4\\nset day of dueDate to 25\\nset hours of dueDate to 6\\nset minutes of dueDate to 0\\nset seconds of dueDate to 0\\nset newReminder to make new reminder with properties {name:\\"Catch my flight\\", due date:dueDate, remind me date:dueDate}\\nreturn name of newReminder\\nend tell\'"}\n'
              '    2. Click by point at the OS level, for when the scan is wrong. AppleScript\'s System Events presses the element the app reports at that point, in any app or layer, without the mouse, so it does not depend on the scan and it is reliable.\n'
              '      1. When: the target is visible in the image but has no [id], its [id] box is not on it, or its name in <element_tree> does not match what the image shows there (e.g., a chess square named as another square). Otherwise click by [id].\n'
              '      2. Points: the CENTER of the target on the 0-1000 scale of the screenshot (0,0 top-left, 1000,1000 bottom-right). The line converts them to the screen itself: never pass image pixels or screen points.\n'
              '      3. Copy the line exactly and change only the calls at its end: click(x,y) clicks once; put delay(0.5) between several clicks (e.g., a piece, then its square, to move it).\n'
              '      4. No mouse: the cursor stays where it is, and the app does not have to be in front. The output names what was pressed last (e.g., ...buttons.byName("e4")): check it is your target.\n'
              '      5. If the next screenshot shows no change, the app has nothing to press at that point (e.g., a drawing canvas): use keys or the menu instead.\n'
              '      6. The first use may ask to let AutoCua control System Events: accept it, then rerun.\n'
              '      7. Judge it on the next screenshot by the target itself (the piece on its new square, the field showing the value), never by a side sign such as the window title.\n'
              '      8. Click a piece, then its square: shell {"value": "osascript -l JavaScript -e \'ObjC.import(`AppKit`);ObjC.import(`CoreGraphics`);var d=$.CGMainDisplayID();ObjC.unwrap($.NSScreen.screens).forEach(function(s){var n=ObjC.unwrap(s.deviceDescription.objectForKey(`NSScreenNumber`));if($.CGDisplayIsBuiltin(n))d=n});var r=$.CGDisplayBounds(d),se=Application(`System Events`);function click(x,y){return se.click(null,{at:[Math.round(r.origin.x+x*r.size.width/1000),Math.round(r.origin.y+y*r.size.height/1000)]})}click(426,595);delay(0.5);click(427,512)\'"}', track=track),
        _tool("applescript", {"app": {"type": "string"}, "value": {"type": "string"}},
              'Run a complete AppleScript on any macOS app. Wrap in `tell application "X" ... end tell`. Do NOT include `activate`/`launch` - runtime handles activation. End with `return` for verification.\n'
              '    1. Safari: use `make new tab` (never `make new document`); always `set current tab to newTab`.\n'
              '    2. Example: applescript {"app": "Safari", "value": "tell application \\"Safari\\"\\n  tell front window to set newTab to make new tab with properties {URL:\\"https://youtube.com\\"}\\n  tell front window to set current tab to newTab\\nend tell"}', track=track),
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
              'replace an element\'s text with `value`: the field is cleared, then the whole string is inserted at once, exactly as given.\n'
              '    1. Always overwrites, never appends, so do not clear the field yourself first.\n'
              '    2. `enter` must be sent separately when needed (e.g., email \'From\', \'To\', \'Search\' fields).\n'
              '      1. Scenario: input + enter + input.\n'
              '    3. If the text does not appear (a field that blocks paste, or one box per character), left_click the field and use typewrite instead.\n'
              '    4. Example: input {"id": 9, "value": "hi, how are you"}', track=track),
        _tool("typewrite", {"value": {"type": "string"}},
              'type into the currently focused area when no element is available.\n'
              '    1. Does not auto-delete; use backspace if needed.\n'
              '    2. Example: typewrite {"value": "hi, how are you"}', track=track),
        _tool("scroll", {"id": {"type": "integer"}, "direction": {"type": "string"}},
              'scroll an element in a direction (`up/down/left/right`).\n'
              '    1. Example: scroll {"id": 9, "direction": "up"}', track=track),
        _tool("hotkey", {"value": {"type": "string"}},
              'OS hotkeys (max 3 keys pairs). Applies to `<Front_screen>`.\n'
              '    1. Use only for OS-level shortcut combinations (e.g., `cmd+c`, `cmd+q`, `cmd+down`).\n'
              '    2. Examples:\n'
              '        1. hotkey {"value": "enter"}\n'
              '        2. hotkey {"value": "cmd+shift+s"}', track=track),
        _tool("screenshot", {"id": {"type": "integer"}, "clicks": {"type": "integer"}},
              'Capture a UI element part as an image and copy it to the clipboard for pasting elsewhere.\n'
              '    1. It takes a screenshot without annotation, so do not trigger it to capture the magenta element number.\n'
              '    2. Image is ready to paste with cmd+v. The clicks field is a dummy (always 1).\n'
              '    3. Example: screenshot {"id": 15, "clicks": 1}', track=track),
        _tool("drag_drop", {"value": {"type": "string"}},
              'click-hold on one element and release on another (drag and drop).\n'
              '    1. Format: drag_drop {"value": "<from_id> to <to_id>"}\n'
              '    2. Example: drag_drop {"value": "8 to 15"}', track=track),

        # -- <task_completion> (the only place `done` is documented) -----------
        _tool("done", {"value": {"type": "string"}},
              'Use `done` as a dedicated final step only, after reviewing <agent_history> to confirm every requested task is finished and doing a final visual verification from the latest image.\n'
              '    1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.\n'
              '    2. Step 2: output ONLY Format: done {"value": "<end-to-end summary in markdown format>"}\n'
              '    3. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.\n'
              '    4. Never combine `done` with any other action/tool in the same step.\n'
              '    5. While scan is off there is no image: base the final verification on the latest tool results.', track=track),
    ])


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
    "drag_drop": {"value": ""},
    "applescript": {"app": "", "value": ""},
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
# notes tool, `agent_memory`: `memory` and `next_goal`, both filled every step.
# The Claude prompts have no <assessment> block and no thinking field, so
# both fields are required (no REASONING_OPTIONAL here).
# ---------------------------------------------------------------------------
CLAUDE_REASONING_FIELDS = {
    "memory": {"type": "string", "description": "Follow <memory>. Filled every step."},
    "next_goal": {"type": "string", "description": "Follow <next_goal>. Filled every step."},
}


def _claude_reasoning_tool(final: str, work: str = "calls", batched: bool = False) -> dict:
    """Claude's `agent_memory` notes tool. `final` is the agent's closing tool (always
    the only call of its turn); `work` names its action calls.

    `batched` (every mac agent): the opening sentences say the call never ends a
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
                 "- `memory` and `next_goal` are filled every step: `memory` follows <memory>, "
                 "`next_goal` follows <next_goal>.\n"
                 f"- Not used on the `{final}` step: `{final}` is always the only call of its turn.\n"
                 '- Format: agent_memory {"memory": "...", "next_goal": "..."}')


_CLAUDE_REASONING_DEFAULTS = {"memory": "", "next_goal": ""}
MAIN_TOOLS_CLAUDE = _main_tools({}, notes=_claude_reasoning_tool("done", batched=True))
MAIN_ACTION_DEFAULTS_CLAUDE = {"agent_memory": dict(_CLAUDE_REASONING_DEFAULTS),
                               **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}
MAIN_TOOL_NAMES_CLAUDE = frozenset(t["name"] for t in MAIN_TOOLS_CLAUDE)


class LLMManager(_LLMManager):
    """The shared LLMManager with this platform's tool lists."""
    MAIN_TOOLS = MAIN_TOOLS
    MAIN_TOOLS_FAST = MAIN_TOOLS_FAST
    MAIN_TOOLS_CLAUDE = MAIN_TOOLS_CLAUDE
    SCREEN_TOOLS = SCREEN_TOOLS
