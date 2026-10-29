//! The Android main driver's tool registry: every tool it can call, and the
//! LLMManager that hands them to the model. The copy of
//! ios/tool_registry/service.py in Rust, together with the shared manager it
//! subclasses there (AutoCua/llm_provider/llm_manager.py: the tool builder,
//! the parser tool_calls_to_steps, drop_bundled_terminal, the dialects, the
//! retries and the usage), as the web agent's manager carries them. The
//! providers are the shared Python ones in AutoCua/llm_provider, the code
//! every platform runs: build_provider makes one per tool list and
//! resolve_model_info reads the model tables the UI lists.
//!
//! NATIVE TOOLS: the main driver calls these tools natively; no JSON
//! envelope. The provider returns structured calls, tool_calls_to_steps()
//! converts them into the same `[{type, ...}]` action dicts route_action
//! consumes, so the controller is untouched.
//!
//! In quality mode a `reasoning` tool carries the step's notes (thinking,
//! memory, next_goal) and every action tool carries only its own fields; in
//! fast mode a `memory` tool carries `next_goal` alone (see
//! main_notes_fields_fast). tool_choice is forced (required / {"type":
//! "any"} / mode="ANY"), so a text-only step is unrepresentable; the text
//! channel remains for free prose only.
//!
//! Exposed to Python as the `LLMManager` class with the same constructor and
//! surface the Python original has: CompressionController builds its second
//! text-mode manager from this class and reads the instance.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Map, Value};

use crate::agent::android_use::view::{py_str_of, truthy};

/// The transcript carries an optional per-turn `provider_meta` on assistant
/// messages: the provider's OWN metadata for that turn (Gemini 3 thought
/// signatures, OpenRouter reasoning blocks), needed to echo the turn back
/// exactly as the model produced it. Only these providers translate it; for
/// everyone else the key is stripped before the request is built, because
/// openai/groq/perplexity/together forward the message dicts to their API
/// verbatim and an unknown key there is a 400.
pub const META_KEY: &str = "provider_meta";
pub const META_PROVIDERS: [&str; 2] = ["openrouter", "google"];

/// The dedicated notes tools: `reasoning` (quality mode), `agent_memory`
/// (the same notes on Claude) and `memory` (fast mode). The parser and the
/// loop treat a call to any of them as the step's notes, never as an action.
pub const NOTE_TOOLS: [&str; 3] = ["reasoning", "agent_memory", "memory"];

/// The optional note field: `thinking` on the `reasoning` tool. Left out of
/// the schema's `required` list and out of the "no arguments" error text.
/// Claude's `agent_memory` has no optional field: its `memory` and
/// `next_goal` are filled every step.
pub const REASONING_OPTIONAL: &[&str] = &["thinking"];

fn fields(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(k, d)| (k.to_string(), d.to_string())).collect()
}

/// Fast mode's notes tool is named `memory` and carries ONE field:
/// `next_goal`. thinking and the quality `memory` field are dropped ENTIRELY
/// rather than left required-but-empty: a required field is an invitation to
/// fill it, and every token spent narrating a plan is a token not spent on
/// the action, which is the whole point of fast mode. next_goal alone
/// carries the verdict, the forward plan (Now / Plan / Then) and the Expect
/// guard (fast_system_prompt.md has no <memory> section: its rules are the
/// <next_goal> section).
pub fn main_notes_fields_fast() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| fields(&[
        ("next_goal", r##"Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (element_name/type/value), and any tool name + purpose + important result. Then the forward plan: "Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>." END with the predicted visible change of THIS step's action prefixed "Expect:", so the next step can verify against the new screenshot. 3-5 concise lines."##),
    ]))
}

/// The MAIN DRIVER's `reasoning` tool fields (quality mode; fast mode's
/// `memory` tool carries `next_goal` alone, see main_notes_fields_fast): the
/// macOS driver's design in phone terms. The verdict is judged on the
/// CURRENT screenshot for a UI action and on the returned result for a tool
/// action (web, shell, video_player), targets are named by name/role in
/// `next_goal` and locked to fresh [id]s in `memory`'s Targets line every
/// step, and the guard is the visible change the next screenshot must show
/// (or the tool's expected result). No S<n> codes, no "not required": a
/// route step omits `thinking` and the snapshot shows "skipped".
pub fn main_reasoning_fields() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| fields(&[
        ("thinking", r##"Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route, it is used up, or a new surface appeared; a short freeform paragraph (RECOVERY) after a guard failed (on the screenshot, or in the returned result of web, shell or video_player); 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass "") on a route step: the previous "Next:" named this action, its guard holds (a UI guard on the current screenshot, a tool guard in that tool's result), and every named target resolves to exactly one [id]."##),
        ("memory", r##"Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT screenshot for a UI guard (never on what a UI tool result claims) or on that tool's returned result for a tool guard (web, shell, video_player): "Last step worked: <what the screen or the result shows>", "Last step failed: <what it shows instead>" or "Last step unclear: <what is ambiguous, and what would settle it>". First step: "Task start." Then the key context (app/screen state; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: "Targets: id N (element_name/type/value), ..." resolved from the CURRENT <element_tree> before acting. 2-4 tight lines."##),
        ("next_goal", r##"Follow <next_goal>: "Doing: <this step> (ToDo: <task_name>). If <the visible change the next screenshot must show, or the expected returned result of web/shell/video_player> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>." Never name a successor target by [id]: ids are re-assigned every scan. On a route step, "Doing" is the next line of the ROUTE and "Next:" the line after it."##),
    ]))
}

/// CLAUDE: any Claude model, whatever the provider (anthropic direct, or a
/// Claude entry on OpenRouter; see LLMManager.claude). The main driver runs
/// on its own claude_prompt.md (in quality mode only) with its own notes
/// tool, `agent_memory`: `memory` and `next_goal`, both filled every step.
/// The Claude prompt has no <assessment> block and no thinking field, so
/// both fields are required (no REASONING_OPTIONAL here).
pub fn claude_reasoning_fields() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| fields(&[
        ("memory", "Follow <memory>. Filled every step."),
        ("next_goal", "Follow <next_goal>. Filled every step."),
    ]))
}

/// Build one canonical tool def: name + parameters + description. The
/// REGISTRY is the single source of tool documentation: each tool's rules
/// live in its `description`, which flows to every provider dialect. Tool
/// defs sit in the cached prefix, so this bills once. All params are
/// required unless named in `optional`, so the controller always sees every
/// field of an action (the only optional one today is the `reasoning` tool's
/// `thinking`, which a route step omits). `track` (per-call tracking params)
/// rides ahead of the action fields when given; the driver passes none,
/// because its notes live on the notes tool.
fn tool(name: &str, params: &[(&str, Value)], description: &str, track: &[(String, String)]) -> Value {
    tool_opt(name, params, description, track, &[])
}

fn tool_opt(
    name: &str,
    params: &[(&str, Value)],
    description: &str,
    track: &[(String, String)],
    optional: &[&str],
) -> Value {
    let mut props = Map::new();
    for (b, d) in track {
        props.insert(b.clone(), json!({"type": "string", "description": d}));
    }
    for (k, schema) in params {
        props.insert((*k).to_string(), schema.clone());
    }
    let required: Vec<Value> = props
        .keys()
        .filter(|k| !optional.contains(&k.as_str()))
        .map(|k| json!(k))
        .collect();
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

/// The notes fields as a tool's params: {"type": "string", "description"}.
fn field_params(list: &[(String, String)]) -> Vec<(&str, Value)> {
    list.iter()
        .map(|(k, d)| (k.as_str(), json!({"type": "string", "description": d})))
        .collect()
}

/// The quality notes tool: `reasoning` {thinking (optional), memory, next_goal}.
pub fn main_reasoning_tool() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| tool_opt("reasoning", &field_params(main_reasoning_fields()),
        r##"Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its guard (the visible change the next screenshot must show; for web, shell and video_player, that tool's returned result); on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails, a new surface appears, or the route said "think". A screenshot that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.
- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is only partially visible, means this step thinks.
- A step may be `reasoning` alone when you need to think before you act (it costs a fresh screen scan); the next step then does the work. Do not chain several such steps.
- A keypad PIN `vault` (id 0, value "pin") stays the only action call of its turn: `reasoning`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: Play Store front, Search tab open, the search field is empty as predicted. Targets: id 19 (element_name='Search', type='searchfield').", "next_goal": "Doing: search for Netflix (ToDo: Update Netflix). If the results list shows the Netflix row → Next: think: confirm the row and route into its page."}
  2. New surface (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, the Mail compose sheet is open as predicted, To field empty. UNDERSTAND: known: To, Subject and the body are in the tree as text fields; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. tap Send, if the compose sheet closes and the mailbox is back → think: verify the mail in Sent, then mark the ToDo.", "memory": "Last step worked: compose sheet open, guard held. Targets: id 12 (element_name='To', type='textfield'), id 14 (element_name='Subject', type='textfield'), id 20 (element_name='Message body', type='textview').", "next_goal": "Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: tap the Send button."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "Failed: the To field is still empty after input on id 74; the screenshot shows the cursor in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: tap the To field first, then input. Guard: the To field shows the address. Route holds after this.", "memory": "Last step failed: To field empty, focus was in Subject. Correction carried: tap a field before typing into it in this compose sheet. Targets: id 74 (element_name='To', type='textfield').", "next_goal": "Doing: tap the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject."}"##,
        &[], REASONING_OPTIONAL))
}

/// The fast notes tool: `memory`, carrying `next_goal` alone.
pub fn main_notes_tool_fast() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| tool("memory", &field_params(main_notes_fields_fast()),
        r##"Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.
- One field, `next_goal`, filled every step: key context, the forward plan (Now / Plan / Then) and the Expect guard the next step verifies against (see <next_goal>). There is no thinking field: the reasoning stays silent (see <silent_reasoning>).
- Every step resolves the plan's targets to fresh [id]s from the CURRENT <element_tree> and names them in `next_goal` before acting.
- A keypad PIN `vault` (id 0, value "pin") stays the only action call of its turn: `memory`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: memory {"next_goal": "..."}
- Example:
  memory {"next_goal": "Play Store Search tab open; search field id 19 (element_name='Search', type='searchfield'), search button id 21 (element_name='Search', type='button'). Now: tap id 19, type 'Netflix', tap id 21 (ToDo: Update Netflix). Plan: open its page -> tap Update. Then: tap the Netflix row. Expect: results list shows the Netflix app row."}"##,
        &[]))
}

/// Claude's `agent_memory` notes tool. `final_tool` is the agent's closing
/// tool (always the only call of its turn); `work` names its action calls.
///
/// `batched` (every agent): the opening sentences say the call never ends a
/// turn. Claude treats a notes tool as a think step - it calls it, ends the
/// turn to wait for the result and acts on the next turn - unless told the
/// actions belong in the SAME message. Measured 2026-10-05 on the driver's
/// step 1 (Sonnet 5): notes+action in one turn 0/5 before, 5/6 with this
/// wording plus the matching <blocks> rule in claude_prompt.md and the
/// per-turn reminder the driver appends to its live message. Keep the name:
/// a renamed tool (`step_notes`) was refused by Sonnet 5.5 / Opus 5 as
/// reasoning extraction.
pub fn claude_reasoning_tool(final_tool: &str, work: &str, batched: bool) -> Value {
    let opening = if batched {
        format!(
            "Your written notes for this step: a log entry, not an action. It has no result and nothing waits on \
             it, so it NEVER ends a turn: emit it first in the list and the {work} that do this step's \"Doing\" \
             in the SAME message. A message holding only this call and no action is a wasted turn. The call is \
             replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n"
        )
    } else {
        format!(
            "Your notes to yourself for this step. Call it FIRST in every step, then the {work} that do the work, \
             all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is \
             replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n"
        )
    };
    let description = format!(
        "{opening}\
         - `memory` and `next_goal` are filled every step: `memory` follows <memory>, \
         `next_goal` follows <next_goal>.\n\
         - A keypad PIN `vault` (id 0, value \"pin\") stays the only action call of its turn: `agent_memory`, then `vault`, nothing else. \
         Not used on the `{final_tool}` step: `{final_tool}` is always the only call of its turn.\n\
         - Format: agent_memory {{\"memory\": \"...\", \"next_goal\": \"...\"}}"
    );
    tool("agent_memory", &field_params(claude_reasoning_fields()), &description, &[])
}

/// The MAIN DRIVER's registry: one tool per action type. route_action and
/// the frontend's tool-flow map key on these names and fields, so they must
/// never drift.
///
/// Descriptions are the system prompt's OWN tool text, copied from
/// <tool_capability> / <os_interaction> / <task_completion> of the iOS
/// prompt in the Android prompt's words (Android phone, host computer, Play
/// Store); the prompt no longer carries a tool list, so this registry is the
/// single source of tool documentation. Built three times, each with NO
/// per-call params (`track` stays empty) and a dedicated notes tool in
/// front: quality mode the `reasoning` tool (thinking optional, memory +
/// next_goal every step), fast mode the `memory` tool (`next_goal` alone),
/// Claude the `agent_memory` tool. All share each action tool's text.
///
/// Phone-specific vs the macOS registry: `scroll` and `vault` carry `value`
/// (not macOS's `direction`), `click` has no `clicks` field, and `vault`,
/// `back` and `video_player` exist here only (`back` is how a player closes,
/// the iPhone's video_player close).
fn main_tools(track: &[(String, String)], notes: Option<&Value>) -> Vec<Value> {
    let mut tools: Vec<Value> = Vec::new();
    if let Some(t) = notes {
        tools.push(t.clone());
    }
    tools.extend(vec![
        // -- <tool_capability> ------------------------------------------------
        tool("open_app", &[("value", json!({"type": "string"}))],
             r##"Launch an installed application directly by name - faster than searching on the device. Use the special name "home" to return to the home screen.
    1. Requirement: Typically call wait 2-3 seconds after this tool to allow loading.
    2. Success means the app is confirmed in the foreground (if the result says verified: false, confirm on the next screenshot). An unknown or ambiguous name returns an error naming the closest installed apps - re-issue with that exact name.
    3. Format: open_app {"value": "app name"}
    4. Examples:
        1. open_app {"value": "disney+"}
        2. open_app {"value": "home"}"##, track),
        tool("wait", &[("value", json!({"type": "string"}))],
             r##"Pause before the next screen scan to allow UI loading. Never exceed 3 seconds at a time.
    1. Format: wait {"value": "time in seconds"}
    2. Examples:
        1. wait {"value": "3"}
        2. wait {"value": "2"}"##, track),
        tool("web", &[("value", json!({"type": "string"}))],
             r##"Delegate to a specialized AI that fetches real-time information and provides data at runtime. Use it for speed instead of browsing manually on the phone.
    1. Format: web {"value": "search query"}
    2. Examples:
        1. web {"value": "financial results of Nvidia Q4 2025"}
        2. web {"value": "latest Netflix app version on the Play Store"}"##, track),
        tool("shell", &[("value", json!({"type": "string"}))],
             r##"Run a shell command on the host computer where this agent is running - not on the Android phone. Use it to check information or perform actions on the host OS, then continue the task on the phone (e.g. photo sharing: read the photo names and details on the computer first, then share those same photos from the phone). Accepts every command of the host's shell.
    1. Format: shell {"value": "command"}
    2. Examples:
        1. shell {"value": "ls ~/Pictures/holiday | head -5"}
        2. shell {"value": "cat ~/Documents/notes.txt"}"##, track),
        tool("todo_list", &[("value", json!({"type": "string"}))],
             r##"Create the ToDo task list (iteration 1 by default; you may also create/expand it later if complexity emerges). See <todo_capability>."##, track),
        tool("update_todo", &[("value", json!({"type": "string"}))],
             r##"Tasks are auto-numbered #1, #2, #3, etc. when saved.
    1. Update a task only after it is confirmed complete via <agent_history> and the effect is visible in the latest input (image or any relevant tag); one item per call.
    2. Example: update_todo {"value": "1"}"##, track),
        tool("vault", &[("id", json!({"type": "integer"})), ("value", json!({"type": "string"}))],
             r##"Fill a field with a login the user saved for the app in front: the runtime types it for you, and the value itself never reaches you.
    1. Format: vault {"id": <element_id>, "value": "<key_from_vault>"}
    2. Examples:
        1. vault {"id": 3, "value": "username"} - the saved username into the username field
        2. vault {"id": 4, "value": "password"} - the saved password into the password field
        3. vault {"id": 0, "value": "pin"} - the saved PIN on a PIN keypad
    3. <vault> in <persistent_memory> lists the keys saved for the app in front. No <vault> means nothing is saved for it: use what <user_request> gives, or ask the user. Never guess a login.
    4. Like `input`, it clears the field first, and it batches like `input`: fill every login field and tap the button in one turn. A password goes only into a password field (securetextfield).
    5. PIN keypads - a separate button for each digit 0-9, like an app's PIN pad (not the phone's own lock screen: ask the user to unlock the phone): use id 0 with value "pin". The vault finds the digit keys and taps the whole PIN itself. Never tap PIN digits yourself. A keypad PIN vault must be the ONLY action call of its turn (only your notes call may precede it). A PIN text field you can type into is filled like any other field, with its [id].
    6. The PIN result says whether every key press registered. Retry only when it says it is safe to try once more; if the PIN was not accepted, only part of it registered, or it was already tried twice, stop and ask the user.
    7. What the vault typed reads as [vault] in later steps."##, track),
        tool("back", &[("value", json!({"type": "string"}))],
             r##"Press the phone's system Back, the same as its back button or back gesture: it takes the page back, and it closes a full-screen video player (works despite DRM screenshot restrictions). `value` is how many times to press it, usually 1.
    1. Format: back {"value": "1"}
    2. Examples:
        1. back {"value": "1"} - close the full-screen video player
        2. back {"value": "1"} - go back to the previous page
    3. On YouTube one Back only leaves full screen; a second one shrinks the video to the small player.
    4. An error result means the screen did not change after a Back."##, track),
        tool("video_player", &[("value", json!({"type": "string"}))],
             r##"Check and control video playback by watching the screen (content that is not DRM-blocked). Commands: streaming (check whether content is playing), pause, play. To close a full-screen video player, use back.
    1. Format: video_player {"value": "one of: streaming/pause/play"}
    2. Examples:
        1. video_player {"value": "streaming"}
        2. video_player {"value": "pause"}
    3. If play or pause fails, ground_click the video to bring up its controls, then call video_player again (it checks the state first)."##, track),
        tool("scratchpad", &[("value", json!({"type": "string"}))],
             r##"Record a verified checkpoint or any critical fact (file path, metric, finding). Follow <scratchpad> rules.
    1. Write `value` in Markdown - inline only (`**bold**`, backticks), never a line break.
    2. Example: scratchpad {"value": "**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**"}"##, track),

        // -- <tool_capability> #10 + <task_completion> -------------------------
        tool("done", &[("value", json!({"type": "string"}))],
             r##"End the task with an end-to-end summary of what was achieved. Dedicated final step - never combine with any other action; do cleanup and ToDo/scratchpad updates in the step before.
    1. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.
    2. Examples:
        1. done {"value": "**Netflix updated** to the latest version - login verified and version noted."}
        2. done {"value": "**Message sent to John** - delivery confirmed on screen."}
    3. Only start completion after reviewing <agent_history> to confirm every requested task is finished.
    4. Then do a final verification from the latest input (double-check the last steps match the request; if playback is DRM-blocked, verify via a video_player check).
    5. Use `done` as a dedicated final step only:
        1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.
        2. Step 2: output ONLY Format: done {"value": "<end-to-end summary in markdown format>"}
    6. Never combine `done` with any other action/tool in the same step."##, track),

        // -- <os_interaction> -------------------------------------------------
        tool("click", &[("id", json!({"type": "integer"}))],
             r##"Tap the centre of an element by its [id].
    1. Examples:
        1. click {"id": 4}
        2. click {"id": 23}"##, track),
        tool("ground_click", &[("x", json!({"type": "integer"})), ("y", json!({"type": "integer"}))],
             r##"Tap a raw point on the screenshot, for a target with no [id]: x and y on a 0-1000 scale of the image (0,0 top-left, 1000,1000 bottom-right).
    1. Example: ground_click {"x": 650, "y": 258}"##, track),
        tool("ground_scroll", &[("x", json!({"type": "integer"})), ("y", json!({"type": "integer"})), ("value", json!({"type": "string"}))],
             r##"Swipe from a raw point on the screenshot, for a list or pane with no [id]: x and y on the 0-1000 scale of the image (as ground_click), value the direction the finger moves (up/down/left/right, as scroll: "up" reveals what is below).
    1. Example: ground_scroll {"x": 500, "y": 700, "value": "up"}"##, track),
        tool("input", &[("id", json!({"type": "integer"})), ("value", json!({"type": "string"}))],
             r##"Type into an element by its [id]. Existing text in the field is auto-deleted before typing.
    1. Examples:
        1. input {"id": 3, "value": "Hi, how are you"}
        2. input {"id": 4, "value": "conjuring"}"##, track),
        tool("scroll", &[("id", json!({"type": "integer"})), ("value", json!({"type": "string"}))],
             r##"Swipe within an element's bounds - three-part action: the element [id] and the direction (value: up/down/left/right).
    1. To reveal content below the visible area, scroll "up"; to reveal content above, scroll "down".
    2. To reveal content on the right, scroll "left"; to reveal content on the left, scroll "right".
    3. Examples:
        1. scroll {"id": 3, "value": "up"}
        2. scroll {"id": 7, "value": "left"}"##, track),
    ]);
    tools
}

pub fn main_tools_quality() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(main_reasoning_tool())))
}

pub fn main_tools_fast() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(main_notes_tool_fast())))
}

/// Quality mode on any Claude model: the same tools with `agent_memory` first.
pub fn main_tools_claude() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(&claude_reasoning_tool("done", "calls", true))))
}

/// Per-action defaults for the main driver: guarantees route_action receives
/// every field of an action, even if the model omits an optional-feeling
/// one. Doubles as the field whitelist: fast mode's per-call `memory` is
/// absent here, so it never reaches the controller. `reasoning` is listed so
/// the parser accepts the call; the service pulls it out before routing.
pub fn main_action_defaults() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| {
        json!({
            "reasoning": {"thinking": "", "memory": "", "next_goal": ""},
            "click": {"id": 0},
            // null, not 0: a coordinate the model left out must fail, not tap the edge.
            "ground_click": {"x": null, "y": null},
            "ground_scroll": {"x": null, "y": null, "value": "up"},
            "input": {"id": 0, "value": ""},
            "scroll": {"id": 0, "value": "up"},
            "vault": {"id": 0, "value": ""},
            "open_app": {"value": ""},
            "wait": {"value": ""},
            "web": {"value": ""},
            "shell": {"value": ""},
            "todo_list": {"value": ""},
            "update_todo": {"value": ""},
            "back": {"value": "1"},
            "video_player": {"value": ""},
            "scratchpad": {"value": ""},
            "done": {"value": ""},
        })
    })
}

/// The defaults with another notes tool in place of `reasoning`.
fn defaults_with_notes(notes_name: &str, notes: Value) -> Value {
    let mut out = Map::new();
    out.insert(notes_name.to_string(), notes);
    if let Some(base) = main_action_defaults().as_object() {
        for (k, v) in base {
            if k != "reasoning" {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

/// Fast mode's notes tool is `memory` (next_goal alone) instead of
/// `reasoning`; tool_calls_to_steps reads the defaults map to find the
/// mode's notes tool.
pub fn main_action_defaults_fast() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| defaults_with_notes("memory", json!({"next_goal": ""})))
}

pub fn main_action_defaults_claude() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| defaults_with_notes("agent_memory", json!({"memory": "", "next_goal": ""})))
}

fn names_of(tools: &[Value]) -> HashSet<String> {
    tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Names the driver may call: anything else is answered with an error tool
/// result instead of being silently dropped, so the model can correct itself
/// on the very next turn.
pub fn main_tool_names() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| names_of(main_tools_quality()))
}

pub fn main_tool_names_fast() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| names_of(main_tools_fast()))
}

pub fn main_tool_names_claude() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| names_of(main_tools_claude()))
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

// -- tool_calls -> steps -----------------------------------------------------

/// Python `int(x)` (Err for what int() rejects).
fn int_of(v: &Value) -> Result<i64, ()> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .ok_or(()),
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Value::String(s) => s.trim().parse::<i64>().map_err(|_| ()),
        _ => Err(()),
    }
}

/// Best-effort coercion to the default's type (models sometimes send '5' for 5).
fn coerce(value: &Value, default: &Value) -> Value {
    match default {
        Value::Bool(_) => {
            if let Value::Bool(_) = value {
                value.clone()
            } else {
                let s = py_str_of(value).trim().to_lowercase();
                json!(matches!(s.as_str(), "true" | "1" | "yes"))
            }
        }
        Value::Number(n) if n.is_i64() || n.is_u64() => match int_of(value) {
            Ok(i) => json!(i),
            Err(_) => default.clone(),
        },
        Value::String(_) => {
            if value.is_string() {
                value.clone()
            } else {
                json!(py_str_of(value))
            }
        }
        _ => value.clone(),
    }
}

/// Convert normalized provider tool calls into (actions, calls, rejects,
/// track) where:
///
///   actions: the SAME `[{type, ...}]` dicts route_action always consumed,
///            with the tracking params STRIPPED (the controller never sees
///            them)
///   calls:   parallel to actions: {"id", "name", "arguments"} for each, so
///            the loop can echo the model's OWN tool calls back in the next
///            request and match each result to its call id (the native
///            transcript that makes models behave natively). Arguments are
///            kept EXACTLY as the model sent them, tracking params included,
///            so the echoed transcript never contradicts the schema that
///            required them.
///   rejects: {"id", "name", "error"} for calls naming a tool that does not
///            exist; the loop feeds these back as error tool results
///   track:   the tracking params stitched from the step's calls: the first
///            call carries them in full, later calls pass "".
///
/// `allowed` and `defaults_map` come from the mode's registry
/// (main_tool_names / main_action_defaults and their fast and Claude
/// twins), `track_params` from the mode's notes fields.
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
    let mut track: Vec<(String, String)> =
        track_params.iter().map(|(k, _)| (k.clone(), String::new())).collect();
    let empty = Map::new();
    let dm = defaults_map.as_object().unwrap_or(&empty);
    // When the registry has a dedicated notes tool carrying the tracking
    // params (`reasoning`, `agent_memory`, or `memory` in fast mode), the
    // action tools do NOT, so a malformed call's "required" list must not
    // name them. A registry without one (older saves replayed against an old
    // map) still carries them on every call. Read off the defaults map,
    // which is per registry.
    let notes_on_own_tool = NOTE_TOOLS.iter().any(|n| {
        dm.get(*n)
            .and_then(Value::as_object)
            .is_some_and(|fields| track.iter().all(|(k, _)| fields.contains_key(k)))
    });
    let list = tool_calls.as_array().cloned().unwrap_or_default();
    for (i, call) in list.iter().enumerate() {
        let name = call
            .get("name")
            .filter(|v| truthy(v))
            .map(|v| py_str_of(v).trim().to_string())
            .unwrap_or_default();
        let args = match call.get("arguments") {
            Some(Value::Object(o)) => o.clone(),
            _ => Map::new(),
        };
        let call_id = call
            .get("id")
            .filter(|v| truthy(v))
            .map(py_str_of)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("call_{i}"));
        for (key, slot) in track.iter_mut() {
            let v = args
                .get(key.as_str())
                .filter(|v| truthy(v))
                .map(|v| py_str_of(v).trim().to_string())
                .unwrap_or_default();
            if !v.is_empty() && slot.is_empty() {
                *slot = v;
            }
        }
        let defaults = dm.get(&name).and_then(Value::as_object);
        let (Some(defaults), true) = (defaults, allowed.contains(&name)) else {
            let shown = if name.is_empty() { "(unnamed)" } else { name.as_str() };
            let mut sorted_names: Vec<&String> = allowed.iter().collect();
            sorted_names.sort();
            rejects.push(json!({
                "id": call_id,
                "name": shown,
                "arguments": Value::Object(args),
                "error": format!(
                    "No tool named '{shown}' exists. Available tools: {}. Call one of those instead.",
                    sorted_names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ),
            }));
            continue;
        };
        // EMPTY arguments is a schema violation, not a set of omitted
        // optional fields: every param on every tool is `required`. Letting
        // the defaults below fill it would silently promote the model's
        // malformed turn into a REAL action: a `click` with no arguments
        // becomes id 0 and taps whatever element 0 happens to be; an `input`
        // with none becomes id 0 / value "", which focuses element 0 and
        // clears it. Reject it exactly the way an unknown tool name is
        // rejected, so the model gets an error keyed to its own call id and
        // re-issues the step instead of acting on an element it never chose.
        //
        // Narrow on purpose: ONLY a fully empty argument object. A partial
        // call still gets default-filled as before, and id 0 remains a
        // perfectly valid element: the problem is never the value 0 itself,
        // only a 0 that arrived from a default the model never sent.
        if !defaults.is_empty() && args.is_empty() {
            let optional: &[&str] = if name == "reasoning" || name == "agent_memory" {
                REASONING_OPTIONAL
            } else {
                &[]
            };
            let mut required: Vec<String> = defaults
                .keys()
                .filter(|k| !optional.contains(&k.as_str()))
                .cloned()
                .collect();
            if !notes_on_own_tool {
                required.extend(track.iter().map(|(k, _)| k.clone()));
            }
            required.sort();
            required.dedup();
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
/// call (`done` for the driver) after this turn's other calls, whose results
/// it has not seen; the controller would run them and end the run on the
/// terminal call, unverified. The terminal call is dropped from the batch
/// and answered with an error result keyed to its id, so the other calls
/// run, the replay stays complete (the caller's all_calls still holds the
/// call) and the model calls the terminal tool alone once it has verified
/// them. A turn whose only calls are terminal ones is returned untouched.
/// Returns (actions, calls, results); the caller prints under its own
/// stdout convention.
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
                "tool_call_id": c["id"].clone(),
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

fn int_or_zero(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if truthy(v) => int_of(v).unwrap_or(0),
        _ => 0,
    }
}

/// Normalize a provider usage dict to {input_tokens, output_tokens,
/// total_tokens, context_tokens}, tolerating both key styles (Anthropic-style
/// input/output and OpenAI-style prompt/completion). Empty/missing -> zeros.
///
/// context_tokens is the TRUE size of the prompt actually sent this turn: the
/// memory-bar number. A cached token still occupies the context window, so
/// the cache classes are added back: input_tokens + cache_read +
/// cache_creation. Exact for every provider: Anthropic's input_tokens
/// EXCLUDES cache, so the cache fields are added; everyone else's
/// prompt_tokens already INCLUDES cached tokens and the Anthropic cache keys
/// are absent (0), so this collapses to the full prompt count.
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
    /// same request gets the same answer, so no retry.
    Refused(String),
    /// Ctrl+C (KeyboardInterrupt) or SystemExit while the provider waited.
    /// Not a failure: no retry, and it reaches the caller as the same Python
    /// error.
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

/// One shared Python provider (AutoCua/llm_provider, made by build_provider)
/// asked for one reply: the normalized response, choices[0].message with
/// content and tool_calls, plus usage. Takes the GIL for the call; the
/// provider gives it back while it waits on the network.
pub fn send_py(provider: &Py<PyAny>, messages: &[Value], model: &str, shots: &[String]) -> Result<Value, SendErr> {
    Python::attach(|py| {
        let reply = (|| -> PyResult<Value> {
            let messages = pythonize::pythonize(py, messages)?;
            // One image goes as the string every provider has always taken;
            // several as a list of them (llm_provider screenshots()).
            let shot: Option<Bound<'_, PyAny>> = match shots.len() {
                0 => None,
                1 => Some(pyo3::types::PyString::new(py, &shots[0]).into_any()),
                _ => Some(pyo3::types::PyList::new(py, shots)?.into_any()),
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
        reply.map_err(|e| {
            if !e.is_instance_of::<PyException>(py) {
                return SendErr::Interrupted(e);
            }
            let refused = py
                .import("AutoCua.llm_provider")
                .and_then(|m| m.getattr("RefusalError"))
                .and_then(|ty| e.value(py).is_instance(&ty))
                .unwrap_or(false);
            let msg = e.value(py).to_string();
            if refused { SendErr::Refused(msg) } else { SendErr::Failed(msg) }
        })
    })
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
/// LLMManager with the Android main driver's tool lists.
#[pyclass(frozen, name = "LLMManager")]
pub struct LLMManager {
    pub provider: String,
    pub model_short_name: String,
    /// Runtime key from the frontend (priority over .env).
    pub runtime_api_key: Option<String>,
    /// Coder/minion flag. NOT a "native tools" switch: every mode but "text"
    /// is native. It stays false for the MAIN DRIVER because each provider
    /// gates its screenshot splice on `not self.cli_agent`, so flipping it
    /// would silently kill the driver's vision.
    pub cli_agent: bool,
    /// "main" | "text": picks the tool registry ("text" gets none).
    pub mode: String,
    /// "quality" | "fast": fast trims the main-agent tracking params.
    pub speed: String,
    pub native_tools: bool,
    /// Any Claude model, whatever the provider: anthropic direct, or a Claude
    /// entry on OpenRouter (api_name anthropic/claude-...). It picks the tool
    /// registry and, in the agent, claude_prompt.md plus the `agent_memory`
    /// notes tool. Set once from the primary model, so the one-call fallback
    /// never flips it.
    pub claude: bool,
    cli_fallback_model: Option<String>,
    primary_model_info: Value,
    /// The shared Python provider for this mode's tool list.
    provider_impl: Py<PyAny>,
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

impl LLMManager {
    pub fn build(
        py: Python<'_>,
        provider: &str,
        model: &str,
        api_key: Option<String>,
        cli_agent: bool,
        mode: &str,
        speed: &str,
    ) -> PyResult<Self> {
        let llm = shared_llm(py)?;
        let provider = provider.to_lowercase();
        let runtime_api_key = api_key;
        // NATIVE TOOL CALLING is the only path: the main driver gets tool
        // definitions as its output contract, no JSON envelope, no
        // response_format, nothing to parse. The single exception is
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
        // Coder / minion (cli_agent=True) run the SAME model as everyone
        // else. The only per-provider hardcoding left is the emergency
        // fallback, used for one call (see send_rust).
        let cli_fallback_model: Option<String> = if cli_agent {
            llm.call_method1("_pick_cli_fallback", (&provider, model))?.extract()?
        } else {
            None
        };
        let model_info = resolve_model_info(py, &provider, model)?;
        let api_name = model_info.get("api_name").and_then(Value::as_str).unwrap_or(model).to_string();
        let claude = provider == "anthropic" || api_name.to_lowercase().contains("claude");

        // Hand each provider its dialect's tool definitions: the registry is
        // picked by mode (the main driver gets its action tools, thinking-less
        // in fast mode; a Claude model gets the CLAUDE variant, whose notes
        // tool is `agent_memory`). mode="text" is the only caller that gets
        // no tools at all. Android has no minion and no coder registry.
        if mode == "minion" || cli_agent {
            return Err(PyValueError::new_err(format!(
                "no {} tool registry on Android",
                if mode == "minion" { "minion" } else { "coder" }
            )));
        }
        let registry: &Vec<Value> = if speed == "fast" {
            main_tools_fast()
        } else if claude {
            main_tools_claude()
        } else {
            main_tools_quality()
        };
        let tools: Option<Vec<Value>> = native_tools.then(|| dialect_tools(&provider, registry));
        let kwargs = PyDict::new(py);
        kwargs.set_item("cli_agent", cli_agent)?;
        kwargs.set_item("max_tokens", max_tokens)?;
        kwargs.set_item("api_key", runtime_api_key.as_deref())?;
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
            runtime_api_key,
            cli_agent,
            mode: mode.to_string(),
            speed: speed.to_string(),
            native_tools,
            claude,
            cli_fallback_model,
            primary_model_info: model_info.clone(),
            provider_impl,
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

    /// Point this manager at a model. Also re-points the provider's
    /// model_info, which openrouter/groq/perplexity cache at construction;
    /// otherwise a swapped model inherits the other model's registry entry.
    fn apply_model_info(&self, info: &Value) {
        let mut state = self.state.lock().unwrap();
        state.model = info.get("api_name").and_then(Value::as_str).unwrap_or_default().to_string();
        state.has_vision = info.get("vision").and_then(Value::as_bool).unwrap_or(true);
        state.display_name = info.get("display_name").and_then(Value::as_str).unwrap_or_default().to_string();
        state.model_info = info.clone();
        drop(state);
        Python::attach(|py| {
            let Ok(entry) = pythonize::pythonize(py, info) else { return };
            let p = self.provider_impl.bind(py);
            if p.hasattr("model_info").unwrap_or(false) {
                let _ = p.setattr("model_info", &entry);
            }
        });
    }

    /// Three idempotent tries against the model currently loaded. Errors
    /// carry the last provider message.
    ///
    /// Return shape: plain content string in text mode; in native-tools mode
    /// {"text": str, "tool_calls": [{"name", "arguments"}], "provider_meta":
    /// dict}, the provider having already normalized the calls.
    fn attempt(&self, messages: &[Value], shots: &[String]) -> Result<SendOutcome, SendErr> {
        let mut last_error = String::new();
        for attempt in 0..3 {
            // Providers may mutate messages in-place (e.g. wrapping the last
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
                let state = self.state.lock().unwrap();
                (state.model.clone(), state.display_name.clone())
            };
            // Pure API round-trip: clock starts the moment the provider is
            // pinged, stops the moment its response is back. Fresh from 0 on
            // EVERY attempt; the copy above is outside the clock.
            let t0 = Instant::now();
            match send_py(&self.provider_impl, &attempt_messages, &model, shots) {
                Ok(response) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let usage = normalize_usage(response.get("usage"));
                    {
                        let mut state = self.state.lock().unwrap();
                        state.last_call_seconds = elapsed;
                        state.last_usage = usage.clone();
                    }
                    if std::io::stdout().is_terminal() {
                        // \r first: overwrite any spinner residue so the line
                        // is clean. TTY-only: never leak into the UI
                        // subprocess pipe. Token counts ride on the same
                        // line: a slow call with a small input/output is NOT
                        // a generation cost, it's cold prefill, provider-side
                        // reasoning, or backend routing.
                        let raw_usage = response.get("usage").cloned().unwrap_or(json!({}));
                        let cached = match int_or_zero(raw_usage.get("cache_read_input_tokens")) {
                            0 => int_or_zero(
                                raw_usage.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")),
                            ),
                            c => c,
                        };
                        println!(
                            "\r⏱ LLM call: {elapsed:.2}s ({display_name}) | in {} (cached {cached}) | out {}",
                            int_or_zero(usage.get("input_tokens")),
                            int_or_zero(usage.get("output_tokens")),
                        );
                    }
                    let message = response
                        .get("choices")
                        .and_then(Value::as_array)
                        .and_then(|c| c.first())
                        .and_then(|c| c.get("message"))
                        .cloned();
                    let Some(message) = message else {
                        last_error = "provider response carried no choices[0].message".to_string();
                        if attempt < 2 {
                            println!("⚠️ {display_name} request failed (attempt {}/3): {last_error}", attempt + 1);
                            println!("   Retrying in 1 second with a fresh message copy...");
                            std::thread::sleep(Duration::from_secs(1));
                            continue;
                        }
                        println!("❌ {display_name} request failed after 3 attempts: {last_error}");
                        break;
                    };
                    if self.native_tools {
                        let content = message.get("content").cloned().unwrap_or(Value::Null);
                        return Ok(SendOutcome::Native(json!({
                            "text": if truthy(&content) { content } else { json!("") },
                            "tool_calls": message.get("tool_calls").filter(|v| truthy(v)).cloned().unwrap_or(json!([])),
                            // Provider metadata for THIS turn, persisted with
                            // the step and handed back to the same provider
                            // next request (see META_KEY). {} when the
                            // provider emits none.
                            META_KEY: message.get(META_KEY).filter(|v| truthy(v)).cloned().unwrap_or(json!({})),
                        })));
                    }
                    let content = message.get("content").cloned().unwrap_or(Value::Null);
                    return Ok(SendOutcome::Text(match content {
                        Value::String(s) => s,
                        // No content (a model that spent its whole budget
                        // thinking): empty, which memory compression drops,
                        // as it drops Python's None. Never the string "None".
                        Value::Null => String::new(),
                        other => py_str_of(&other),
                    }));
                }
                Err(SendErr::Interrupted(e)) => return Err(SendErr::Interrupted(e)),
                Err(SendErr::Refused(e)) => {
                    // The API refused the request itself; the same request
                    // gets the same answer, so no retry. send_rust falls back
                    // where it can.
                    println!("⛔ {display_name} refused the request: {e}");
                    return Err(SendErr::Refused(e));
                }
                Err(SendErr::Failed(e)) => {
                    last_error = e;
                    if attempt < 2 {
                        println!("⚠️ {display_name} request failed (attempt {}/3): {last_error}", attempt + 1);
                        println!("   Retrying in 1 second with a fresh message copy...");
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    println!("❌ {display_name} request failed after 3 attempts: {last_error}");
                }
            }
        }
        Err(SendErr::Failed(last_error))
    }

    /// Send request to the selected provider with idempotent retries.
    ///
    /// Coder/minion only: if the user's model fails all 3 attempts, ONE call
    /// is served by the per-provider fallback (3 attempts of its own). Win or
    /// lose, the manager is put back on the user's model afterwards, so the
    /// next iteration starts on their model again. If the fallback also
    /// fails its 3 attempts the error propagates and the agent stops.
    ///
    /// The main agent has no fallback (cli_agent=false): 3 attempts, then
    /// raise. GIL-free: callers detach around this.
    pub fn send_rust(&self, messages: &[Value], shots: &[String]) -> Result<SendOutcome, SendErr> {
        match self.attempt(messages, shots) {
            Ok(out) => return Ok(out),
            Err(e @ SendErr::Interrupted(_)) => return Err(e),
            Err(e) => {
                if !(self.cli_agent && self.cli_fallback_model.is_some()) {
                    return Err(e);
                }
            }
        }
        let fallback_model = self.cli_fallback_model.clone().unwrap();
        let fallback_info = Python::attach(|py| resolve_model_info(py, &self.provider, &fallback_model))
            .map_err(|e| SendErr::Failed(e.to_string()))?;
        let fallback_display = fallback_info
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(&fallback_model)
            .to_string();
        println!(
            "⚠️ {} failed — this step only, falling back to {fallback_display}...",
            self.state.lock().unwrap().display_name
        );
        self.apply_model_info(&fallback_info);
        let result = self.attempt(messages, shots);
        // Always revert: the fallback covers this call, not the whole run.
        self.apply_model_info(&self.primary_model_info.clone());
        match result {
            Ok(out) => {
                println!("↩️ Back on {} for the next step.", self.state.lock().unwrap().display_name);
                Ok(out)
            }
            Err(e) => {
                println!("❌ Fallback {fallback_display} also failed 3 attempts — stopping.");
                Err(e)
            }
        }
    }

    /// True when the model batches only on the Responses endpoint with async
    /// tools (GPT-6 Astra, table flag `async_tools`): the loop adds the
    /// <async_tools> prompt block and the bundled done backstop for it. Read
    /// from the primary entry, so a one-call fallback does not flip it.
    pub fn async_tools(&self) -> bool {
        truthy(self.primary_model_info.get("async_tools").unwrap_or(&Value::Null))
    }

    /// Most recent send's normalized token usage (input/output/total/context).
    pub fn last_usage_value(&self) -> Value {
        self.state.lock().unwrap().last_usage.clone()
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
            api_key,
            cli_agent,
            mode.as_deref().unwrap_or("main"),
            speed.as_deref().unwrap_or("quality"),
        )
    }

    /// Send request to the selected provider with idempotent retries: the
    /// reply dict in native-tools mode, the text in text mode.
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
        let outcome = py
            .detach(|| self.send_rust(&msgs, &shots))
            .map_err(PyErr::from)?;
        match outcome {
            SendOutcome::Native(v) => pythonize::pythonize(py, &v).map_err(Into::into),
            SendOutcome::Text(s) => Ok(pyo3::types::PyString::new(py, &s).into_any()),
        }
    }

    #[pyo3(name = "async_tools")]
    fn async_tools_py(&self) -> bool {
        self.async_tools()
    }

    /// Current model short name (preserves vertex suffix for routing).
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
    fn claude(&self) -> bool {
        self.claude
    }

    #[getter]
    fn model(&self) -> String {
        self.state.lock().unwrap().model.clone()
    }

    #[getter]
    fn display_name(&self) -> String {
        self.state.lock().unwrap().display_name.clone()
    }

    #[getter]
    fn has_vision(&self) -> bool {
        self.state.lock().unwrap().has_vision
    }

    #[getter]
    fn model_info<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pythonize::pythonize(py, &self.state.lock().unwrap().model_info).map_err(Into::into)
    }

    #[getter]
    fn last_usage<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        pythonize::pythonize(py, &self.last_usage_value()).map_err(Into::into)
    }

    #[getter]
    fn last_call_seconds(&self) -> f64 {
        self.state.lock().unwrap().last_call_seconds
    }
}

// ---------------------------------------------------------------------------
// Test hooks: the converter and the registries exposed to Python so they can
// be differentially tested against the originals.
// ---------------------------------------------------------------------------

/// The mode's (track params, names, defaults): "fast", "claude" or quality.
fn mode_tables(speed: Option<&str>) -> (&'static Vec<(String, String)>, &'static HashSet<String>, &'static Value) {
    match speed {
        Some("fast") => (main_notes_fields_fast(), main_tool_names_fast(), main_action_defaults_fast()),
        Some("claude") => (claude_reasoning_fields(), main_tool_names_claude(), main_action_defaults_claude()),
        _ => (main_reasoning_fields(), main_tool_names(), main_action_defaults()),
    }
}

#[pyfunction]
#[pyo3(signature = (tool_calls, speed=None))]
pub fn _tool_calls_to_steps_debug<'py>(
    py: Python<'py>,
    tool_calls: Bound<'py, PyAny>,
    speed: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let calls_val: Value = if tool_calls.is_none() { Value::Null } else { pythonize::depythonize(&tool_calls)? };
    let (track_params, names, defaults) = mode_tables(speed.as_deref());
    let (actions, calls, rejects, track) = tool_calls_to_steps(&calls_val, names, defaults, track_params);
    let track_map: Map<String, Value> = track.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
    pythonize::pythonize(py, &json!([actions, calls, rejects, Value::Object(track_map)])).map_err(Into::into)
}

#[pyfunction]
pub fn _drop_bundled_terminal_debug<'py>(
    py: Python<'py>,
    actions: Bound<'py, PyAny>,
    calls: Bound<'py, PyAny>,
    rejects: Bound<'py, PyAny>,
    terminal: String,
) -> PyResult<Bound<'py, PyAny>> {
    let list = |v: &Bound<'py, PyAny>| -> PyResult<Vec<Value>> {
        Ok(match pythonize::depythonize(v)? {
            Value::Array(a) => a,
            _ => Vec::new(),
        })
    };
    let (a, c, r) = drop_bundled_terminal(list(&actions)?, list(&calls)?, &list(&rejects)?, &terminal);
    pythonize::pythonize(py, &json!([a, c, r])).map_err(Into::into)
}

/// The registry a mode sends ("fast", "claude" or quality), for tests; with
/// `provider`, in that provider's dialect.
#[pyfunction]
#[pyo3(signature = (speed=None, provider=None))]
pub fn _main_tools_debug<'py>(py: Python<'py>, speed: Option<String>, provider: Option<String>) -> PyResult<Bound<'py, PyAny>> {
    let registry = match speed.as_deref() {
        Some("fast") => main_tools_fast(),
        Some("claude") => main_tools_claude(),
        _ => main_tools_quality(),
    };
    let out = match provider.as_deref() {
        Some(p) => dialect_tools(&p.to_lowercase(), registry),
        None => registry.clone(),
    };
    pythonize::pythonize(py, &Value::Array(out)).map_err(Into::into)
}
