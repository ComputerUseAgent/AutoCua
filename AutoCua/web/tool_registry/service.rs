//! LLMManager — routes requests to the correct LLM provider.
//!
//! The web driver speaks NATIVE TOOL CALLING — the tool registry below IS the
//! output contract. There is no JSON-envelope response schema: the prompt's
//! four blocks ride as tracking params on the calls themselves. The only
//! schema-less, tool-less path is mode="text" (the memory-compression
//! handoff), which wants plain prose.
//!
//! Exposed to Python as the `LLMManager` class with the same constructor and
//! surface the Python original had — CompressionController builds its second
//! text-mode manager from this class.
//!
//! The providers are the shared Python ones in AutoCua/llm_provider, the
//! code every platform runs: build_provider makes one per tool list and
//! resolve_model_info reads the model tables the UI lists. This file keeps the
//! web tool registry, its tool dialects, the parser, the retries and the usage.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Map, Value};

use crate::browser::truthy;
use crate::agent::main_driver::view::py_str_of;

/// The transcript carries an optional per-turn `provider_meta` on assistant
/// messages — the provider's OWN metadata for that turn. Only these providers
/// translate it; for everyone else the key is stripped before the request is
/// built (an unknown key there is a 400).
pub const META_KEY: &str = "provider_meta";
pub const META_PROVIDERS: [&str; 2] = ["openrouter", "google"];

// ---------------------------------------------------------------------------
// The step's notes ride on a dedicated tool, called FIRST in every step,
// while the action tools carry only their own fields. Quality mode: the
// `reasoning` tool, {thinking, memory, next_goal} (thinking optional: a route
// step omits it). Fast mode: the `memory` tool, {next_goal} alone, whose one
// string carries the memory and the decision in labeled sections.
// ---------------------------------------------------------------------------

/// The `reasoning` tool's fields (quality mode), in page terms: the verdict
/// is judged on the CURRENT page, targets are named by name/role in
/// `next_goal` and locked to fresh [id]s in `memory`'s Targets line every
/// step, and the guard is the visible change the next page must show. No
/// S<n> codes, no "not required": a route step omits `thinking` and the
/// snapshot shows "skipped".
pub fn main_reasoning_fields() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| vec![
        ("thinking".to_string(), r#"Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: "JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ..." (FULL) when there is no route, it is used up, or a new page appeared; a short freeform paragraph (RECOVERY) after a guard failed on the page; 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass "") on a route step: the previous "Next:" named this action, its visible-change guard holds on the current page, and every named target resolves to exactly one [id]."#.to_string()),
        ("memory".to_string(), r#"Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT page (<element_tree> + screenshot), never on what a tool result claims: "Last step worked: <what the page shows>", "Last step failed: <what the page shows instead>" or "Last step unclear: <what is ambiguous, and what would settle it>". First step: "Task start." Then the key context (site/page state and the tab; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: "Targets: id N (tag/role/visible text), ..." resolved from the CURRENT <element_tree> before acting. 2-4 tight lines."#.to_string()),
        ("next_goal".to_string(), r#"Follow <next_goal>: "Doing: <this step> (ToDo: <task_name>). If <visible change the next page must show> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>." Never name a successor target by [id]: ids are re-assigned every scan. On a route step, "Doing" is the next line of the ROUTE and "Next:" the line after it."#.to_string()),
    ])
}

/// The notes tools' optional field: `thinking` on `reasoning`, `assessment` on
/// Claude's `agent_memory`. Left out of the schema's `required` list and out of
/// the missing-field reject, so a route step can omit it (a required string
/// invites filling).
pub const REASONING_OPTIONAL: &[&str] = &["thinking", "assessment"];

/// Claude's `agent_memory` fields (quality mode, any Claude model): the
/// `reasoning` notes with `assessment` in place of `thinking`, as on the
/// desktop agents (CLAUDE_REASONING_FIELDS in <os>/tool_registry/service.py).
pub fn claude_reasoning_fields() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| vec![
        ("assessment".to_string(), r#"Follow <assessment>. Omit it (or pass "") on a route step."#.to_string()),
        ("memory".to_string(), "Follow <memory>. Filled every step.".to_string()),
        ("next_goal".to_string(), "Follow <next_goal>. Filled every step.".to_string()),
    ])
}

/// The fast `memory` tool's single field. Its one string carries the verdict,
/// the context and the targets ("Memory:"), an optional one-line decision
/// ("Reasoning:"), this step's move and the guard ("Doing: ... If ... → Next:
/// ..."), the guard being what the next tree must show: fast mode sends
/// no screenshot, the tree is the whole page.
pub fn main_memory_fields_fast() -> &'static Vec<(String, String)> {
    static P: OnceLock<Vec<(String, String)>> = OnceLock::new();
    P.get_or_init(|| vec![
        ("next_goal".to_string(), r#"Your one note for this step, in labeled sections (see <next_goal>): "Memory: <verdict on the previous step judged on the CURRENT input (worked / failed / unclear; first step: Task start), key context, Targets: id N (tag/role/visible text) resolved from the CURRENT <element_tree>>. Reasoning: <one line, only when a decision was made>. Doing: <this step> (ToDo: <task_name>). If <the change the next input must show> → Next: <action on a target named by NAME/ROLE | think: <decision>>." Judge on the CURRENT <element_tree> alone (fast mode sends no screenshot) and name what in the tree each verdict rests on. Never name a successor target by [id]: ids are re-assigned every scan. 3-5 tight lines."#.to_string()),
    ])
}

/// Build one canonical tool def — name + parameters + description. All
/// params are required unless named in `optional`, so the controller always
/// sees every field of an action (the only optional one today is the
/// `reasoning` tool's `thinking`, which a route step omits). `track` (fast
/// mode's per-call `memory`, or the `reasoning` tool's own three fields)
/// rides ahead of the action fields.
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

/// The MAIN DRIVER's registry — one tool per action type. route_action and
/// the frontend's tool-flow map key on these names and fields, so they must
/// never drift. Each description is the system prompt's former
/// <tool_capability> entry VERBATIM — the prompt no longer carries a tool
/// list, so this registry is the single source of tool documentation. The
/// only additions are schema-required params the prompt text never named
/// (click's `times`, input's `enter` false case), appended as extra rules.
/// Built twice, each with NO per-call params (`track` stays empty) and a
/// dedicated notes tool in front: quality mode `main_reasoning_tool()`, fast
/// mode `main_memory_tool_fast()`.
fn main_tools(track: &[(String, String)], notes: Option<&Value>) -> Vec<Value> {
    let mut tools: Vec<Value> = Vec::new();
    if let Some(t) = notes {
        tools.push(t.clone());
    }
    tools.extend(vec![
        tool("new_tab", &[("value", json!({"type": "string"}))],
             r#"Open a new browser tab. ALWAYS recommended when the current tab already holds task-relevant content - never hijack an occupied tab; check <all_tabs> first.
  1. Format: new_tab {"value": "<url_or_empty>"} - keep value "" to open a blank tab when the destination url is unknown.
  2. Examples:
    1. new_tab {"value": "https://www.amazon.com"}
    2. new_tab {"value": ""} - a blank tab, for when the destination url is not known yet
  3. A page landing in a new tab is a NEW SURFACE: survey before routing deep (see <operating_rhythm>)."#, track),

        tool("switch_tab", &[("id", json!({"type": "integer"}))],
             r#"Switch to a tab that is already open, making it current so the next scan and every following action land on it.
  1. Format: switch_tab {"id": <n_from_all_tabs>}
  2. Example: switch_tab {"id": 2}
  3. `id` is the [n] from <all_tabs>, NOT an [id] from <element_tree>. They are separate numberings that both look like small integers.
  4. Prefer this over re-opening a page you already have: switching keeps that tab's session, scroll position and half-filled forms, while a fresh new_tab throws all of it away.
  5. The tab you land on is a NEW SURFACE: survey before routing deep (see <operating_rhythm>)."#, track),

        tool("close_tab", &[("id", json!({"type": "integer"}))],
             r#"Close a tab that is open, removing it from <all_tabs>. Everything it held - session, scroll position, half-filled forms - is gone for good.
  1. Format: close_tab {"id": <n_from_all_tabs>}
  2. Example: close_tab {"id": 3}
  3. `id` is the [n] from <all_tabs>, NOT an [id] from <element_tree>.
  4. Use it to tidy up a tab whose job is finished. NEVER close a tab that still holds task-relevant state; if in doubt, leave it open - an extra tab costs nothing, a closed one cannot be reopened.
  5. Closing the CURRENT tab moves you to a neighbouring tab. The LAST remaining tab cannot be closed - the action FAILS and says so; navigate it with `update_tab` instead.
  6. Closing RE-NUMBERS <all_tabs>: read the fresh list in the next input before any other tab action."#, track),

        tool("update_tab", &[("value", json!({"type": "string"}))],
             r#"Navigate the CURRENT tab to a url, replacing whatever it is showing. Same tab, new page - nothing is opened and nothing is closed.
  1. Format: update_tab {"value": "<url>"}
  2. Example: update_tab {"value": "https://www.wikipedia.org"}
  3. Use it when the current page has served its purpose and the tab can be reused: undoing a wrong turn, following a url you can construct directly, or leaving a redirect you did not want.
  4. NOT for a page you still need - that is `new_tab`, which leaves this one open. And never type a url into the browser's address bar or a new-tab search box: those are browser chrome, not page elements, so they carry no [id] and typing there is not a page interaction. `update_tab` is how you navigate.
  5. The page that loads is a NEW SURFACE: survey before routing deep (see <operating_rhythm>)."#, track),

        tool("navigate_tab", &[("value", json!({"type": "string", "enum": ["back", "forward", "reload"]}))],
             r#"Move the CURRENT tab without naming a destination - step through its history, or re-request the page it is on. Same tab throughout.
  1. Format: navigate_tab {"value": "<back|forward|reload>"}
  2. Examples:
    1. navigate_tab {"value": "back"} - return to the previous page, e.g. from a product page to the results you opened it from
    2. navigate_tab {"value": "reload"} - re-request the same url for a fresh copy of the page
  3. "back" undoes a wrong turn while keeping the page you came from intact - the results list, its scroll position and its filters are all still there, which re-searching would throw away. Prefer it over rebuilding a page you already had.
  4. "forward" only exists after a "back": it returns to the page you left.
  5. "reload" is for a page that is stuck or stale: a spinner that never resolved, a list that did not update after your action, a transient error page. It is NOT a verification step - it discards unsaved form input and closes any open dialog, so read the page first and reload only when a fresh load is genuinely what you need.
  6. If there is no page to go to, the action FAILS and says so - the tab is at the start or the end of its history. Do not retry the same move.
  7. Whatever loads is a NEW SURFACE: survey before routing deep (see <operating_rhythm>)."#, track),

        tool("click", &[("id", json!({"type": "integer"})), ("times", json!({"type": "integer"}))],
             r#"Single click on an interactable element (default click, nothing more).
  1. Format: click {"id": <id_from_element_tree>, "times": <1_or_2>}
  2. Positive examples:
    1. click {"id": 19, "times": 1} - a normal single click
    2. click {"id": 7, "times": 2} - a DOUBLE click on the same element
  3. Negative examples (NEVER emit these):
    1. click {"id": 0, "times": 1} - WRONG: 0 is not a real element id; always use an [id] from the current <element_tree>
    2. click {"id": 19, "times": 3} - WRONG: `times` above 2 is meaningless; use 1, or 2 for a double click
  4. Clicking a `collapsed` element expands it; its children arrive in the NEXT <element_tree>.
  5. `times` 1 is the normal case and what nearly every web control wants. Use 2 only where a double click is genuinely the gesture: a file-manager style row that opens on double click, selecting a word inside a text field. Values above 2 are clamped to 2.
  6. `times` 2 is ONE double-click gesture, not two clicks. If you actually want two separate clicks, emit two `click` calls with "times": 1 - a toggle pressed twice ends up back where it started."#, track),

        tool("hold_click", &[("id", json!({"type": "integer"})), ("time", json!({"type": "integer"}))],
             r#"Press and hold the element for a duration, then release.
  1. Format: hold_click {"id": <id_from_element_tree>, "time": <seconds>} - `time` is the hold duration in seconds.
  2. Example: hold_click {"id": 19, "time": 2}
  3. Use for press-and-hold controls ("hold to confirm" buttons, human-verification holds); for a normal click use `click`."#, track),

        tool("input", &[("id", json!({"type": "integer"})), ("value", json!({"type": "string"})), ("enter", json!({"type": "boolean"}))],
             r#"Clear the element, then type the value into it.
  1. Format: input {"id": <id_from_element_tree>, "value": "<text_to_type>", "enter": <true_or_false>}
  2. Positive examples:
    1. input {"id": 21, "value": "iphone 16", "enter": false} - fill the field, do not submit
    2. input {"id": 21, "value": "iphone 16", "enter": true} - fill AND submit, in one action
  3. Negative examples (NEVER emit these):
    1. input {"id": 0, "value": "iphone 16", "enter": false} - WRONG: 0 is not a real element id; always use an [id] from the current <element_tree>
    2. input {"id": 21, "value": "", "enter": false} - WRONG: an empty value types nothing; it only clears the field
  4. "enter": true presses Enter after typing - use it to submit searches/forms in the same action. Typing and its submit belong together here - never fill a field and stop before the submit that completes it. Other keys (esc, tab, shortcuts) are the `keyboard` tool's; text is ONLY typed with `input`.
  5. Use false when the field is one of several: an early Enter submits a half-filled form."#, track),

        tool("keyboard", &[("value", json!({"type": "string"}))],
             r#"Press a keyboard shortcut on the current page: one key, or up to 3 keys held together, joined with "+". Key STROKES only - this is NOT a way to type: a word is rejected, and text always goes into a field through `input`.
  1. Format: keyboard {"value": "<key>"} or {"value": "<key+key>"} or {"value": "<key+key+key>"}
  2. Positive examples:
    1. keyboard {"value": "esc"} - dismiss an open modal, dropdown or overlay
    2. keyboard {"value": "shift+tab"} - move focus to the previous control
    3. keyboard {"value": "cmd+a"} on mac, keyboard {"value": "ctrl+a"} on windows/linux - select everything in the focused field
    4. keyboard {"value": "ctrl+shift+z"} - a 3-key shortcut
  3. Negative examples (NEVER emit these):
    1. keyboard {"value": "hello world"} - WRONG: that is text, not a shortcut; `input` types text
    2. keyboard {"value": "ctrl+shift+alt+t"} - WRONG: 4 keys; a shortcut is at most 3
    3. keyboard {"value": "cmd+t"} - WRONG: a browser shortcut, not a page one (see rule 6); open tabs with `new_tab`
  4. Keys: esc, enter, tab, space, backspace, delete, up, down, left, right, home, end, pageup, pagedown, f1-f12; modifiers ctrl, cmd, alt, shift; any single character stands for itself ("/", "?", "k"). The keys are held together in the order written - modifiers first, the key last. A "+" that begins a key is the plus key itself: "+" alone is plus, "++-" is plus and minus.
  5. The modifier follows current_os in <knowledge_base>: cmd on mac, ctrl on windows and linux.
  6. The keys reach the PAGE only, never the browser: shortcuts owned by the browser itself (new/close/switch tab, address bar, find, reload, zoom) do nothing here even though the call succeeds - the tab tools do those jobs.
  7. Keys go to whatever has focus. When the shortcut belongs to a field or widget, `click` it first in the same turn, then send the keys. When you are typing anyway, submit with `input`'s "enter": true rather than a separate enter.
  8. The page may change afterwards (a dialog closes, focus moves): read the next <element_tree> before acting on what you now see."#, track),

        tool("run_script", &[
                ("id", json!({"type": "integer"})),
                ("type", json!({"type": "string", "enum": ["action", "scrape"]})),
                ("value", json!({"type": "string"})),
             ],
             r#"Run JavaScript inside a tab's page. Two types, named in `type`:
  "action" DOES something the ordinary tools cannot reach: put the cursor in a field, take away what covers a control, open a collapsed section, set a plain form value. It may also take a small look at how the page is built, to plan a scrape.
  "scrape" READS the data the task needs out of the page - records, exact numbers, full text, table rows - as JSON, with your own selectors and fields, three screens at a time.
  USE SPARINGLY: sites that watch for automation can notice page JavaScript and block the session. For actions the ordinary tools come first - `click`, `input`, `keyboard`, `scroll`, and <element_tree> to see what is there. Do each job in ONE call.
  1. Format: run_script {"id": <n_from_all_tabs>, "type": "<action|scrape>", "value": "<javascript>"}
  2. type "action", examples:
    1. run_script {"id": 1, "type": "action", "value": "const box = document.querySelector('#t-name-box'); box.focus(); box.select(); return box.value"} - put the cursor in a field and select what is already in it, so `keyboard` or `input` types over it instead of after it
    2. run_script {"id": 1, "type": "action", "value": "document.querySelector('.sticky-header, .overlay-backdrop')?.remove(); return 'cleared'"} - take away the banner or overlay sitting on top of the control you need to click
  3. type "scrape", examples:
    1. run_script {"id": 1, "type": "scrape", "value": "return [...document.querySelectorAll('shreddit-comment')].map(el => ({author: el.getAttribute('author'), upvotes: Number(el.getAttribute('score')), depth: Number(el.getAttribute('depth')), text: document.getElementById(el.getAttribute('thingid') + '-comment-rtjson-content')?.innerText.trim().slice(0, 400)}))"} - the comments of a Reddit thread on these screens: the exact count from the score attribute (the screen rounds it to "8.2K"), the depth (0 is a top-level comment), and each comment's own words from its own text element (its innerText repeats every reply under it)
    2. run_script {"id": 1, "type": "scrape", "value": "const num = c => Number((c?.innerText || '').replace(/\\[.*?\\]/g, '').replace(/\\D/g, '')) || null; return [...document.querySelectorAll('table.wikitable tr')].filter(tr => tr.querySelector('td')).map(tr => ({city: tr.children[0].innerText.replace(/\\[.*?\\]/g, '').trim(), country: tr.children[1]?.innerText.trim(), un_2025: num(tr.children[2])}))"} - the rows of a Wikipedia table on these screens: header rows left out, cells by position, footnote marks ("[14]") stripped, numbers as numbers
  4. How a scrape reads:
    1. It reads the screen the page is on and the two below it. Your script runs once on each of the 3 screens, and there `document.querySelectorAll` gives only the elements whose top edge is on that screen; the arrays it returns are joined. Start every read from `document.querySelectorAll(<one record>)`: a lookup that starts anywhere else is not held to the screens.
    2. The page scrolls through the 3 screens and stays at the third. A clean screenshot of each screen (no ids, no overlay) comes with the next input, for that step only: check the records against them.
    3. A read that brings records opens scrape mode: the next step shows no page, only your records and the screenshots, and its tools are `more` (the next 3 screens with the same script, with their screenshots), `scratchpad`, `update_todo` and `exit_scrape_mode`. Make the scrape the LAST call of its batch.
    4. One read hands back at most 10000 tokens (about 30000 characters). Over that nothing comes back, the page returns to where the read began, and the screenshots still come, so you can narrow the script: fewer fields, shorter text, a narrower selector. A read that matches nothing opens no mode; its screenshots come too.
  5. How to scrape: load the scraping skill first (`skills`, its [id] in <skills>). The method, the site notes and worked examples are there; a scrape that opens scrape mode loads it for you.
  6. Negative examples (NEVER emit these):
    1. run_script {"id": 1, "type": "scrape", "value": "document.body.innerText"} - WRONG: the whole page as one blob, past the limit and stripped of its structure; read the records the task needs, field by field
    2. run_script {"id": 1, "type": "scrape", "value": "document.querySelectorAll('shreddit-comment')"} - WRONG: DOM nodes are not data; map each record to the fields the task needs
    3. run_script {"id": 1, "type": "action", "value": "el.dispatchEvent(new KeyboardEvent('keydown', {key: 'a'}))"} - WRONG: events made by script are untrusted and apps ignore them; press real keys with `keyboard`, type with `input`, click with `click`
  7. `id` is the [n] from <all_tabs>, NOT an [id] from <element_tree>, and it must be the tab you are working in - `switch_tab` to it first to work in another one.
  8. Return plain data - strings, numbers, arrays, objects. A single expression gives its value; code with statements produces nothing unless it says `return`, and `await` works there. A thrown error comes back as a failed action carrying its message. A script still running after 10 seconds is stopped. An action's result is cut at 30000 characters: return less.
  9. What an action changes is REAL: DOM edits, form values and storage stay changed and the next <element_tree> shows them. But a value set by script is one the page was never told about, so an app may ignore it - confirm it took, and leave anything a person would do by hand to `click`, `keyboard` and `input`.
  10. Web pages only, and credentials never travel: a script that reads cookies, stored logins, the clipboard or a password or payment field is refused before it runs, so is one that sends page data to another site, and anything credential-shaped is taken out of what comes back. A refusal quotes the exact construct - rewrite the step without it rather than looking for a way around."#, track),

        tool("scroll", &[("id", json!({"type": "integer"})), ("direction", json!({"type": "string", "enum": ["up", "down", "left", "right"]}))],
             r#"Bring off-screen content into view. Changes only WHAT IS VISIBLE - nothing is activated, opened or submitted.
  1. Format: scroll {"id": <id_from_element_tree>, "direction": "<up|down|left|right>"}
  2. Examples:
    1. scroll {"id": 1, "direction": "down"} - the whole page down one screenful
    2. scroll {"id": 14, "direction": "down"} - scroll the list/panel/dropdown that [14] sits inside, leaving the page where it is
  3. `id` picks WHICH surface moves, because the scroll is delivered AT that element and whatever scrolls around it takes it. [1] is the page itself. For a list, dropdown, sidebar, panel, table or carousel, pass the [id] of ANY element you can see INSIDE it and that region scrolls while the page stays put - the container never needs an [id] of its own.
  4. One scroll moves about three quarters of the visible surface, so consecutive scrolls overlap and nothing is skipped.
  5. The result says whether anything actually moved. "NO EFFECT - nothing moved" means that surface is at its end in that direction: do not repeat it - change direction, change region, or scroll [1].
  6. Every [id] is re-assigned afterwards: read the new <element_tree> before acting on what you now see."#, track),

        tool("wait", &[("value", json!({"type": "string"}))],
             r#"Pause execution to allow page loading or to trigger a fresh page scan.
  1. Format: wait {"value": "<seconds>"}
  2. Example: wait {"value": "2"}
  3. Rarely needed: the scanner already waits for the page to stop fetching before every scan. Use it for something that is NOT network work - an animation settling, a countdown on the page - not for ordinary page loads."#, track),

        tool("scratchpad", &[("value", json!({"type": "string"}))],
             r#"Your durable note store - the record of MILESTONES ACHIEVED plus any key fact you need later. Follow <scratchpad>.
  1. Format: scratchpad {"value": "<one_line_verified_note>"}
  2. Examples:
    1. Smaller milestone: scratchpad {"value": "**Milestone:** signed in to amazon.com - account menu shows the user name"}
    2. Smaller milestone: scratchpad {"value": "**Milestone:** filters applied - 128GB + Prime delivery + 4 stars and up"}
    3. Greater milestone: scratchpad {"value": "**Done:** order placed on amazon.com - confirmation **#114-2698**"}
    4. Key fact: scratchpad {"value": "Product page: https://www.amazon.com/dp/B0DGHYDZR9 - iPhone 16 128GB"}
    5. Answer: scratchpad {"value": "**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**"}
  3. Write a milestone the moment one lands, at EVERY size. A smaller milestone (signed in, filters applied, the right product page reached, one form section filled, a cookie wall cleared) is recorded exactly like a greater one (order placed, booking confirmed, the answer to <user_request> found). The small ones are what tell a later step how far the route already got - without them a re-route restarts from zero.
  4. Only write after visual confirmation on the CURRENT page - never assume an action landed.
  5. One fact per call, one line. If several things are confirmed in the same step, emit one separate `scratchpad` call for each - never batch them into one entry.
  6. The live file is rendered in your input as <scratchpad> once any entries exist - check it before recording, so entries never duplicate.
  7. Use for: milestones (small and large), metrics/numbers/final answers, important findings, exact urls of pages that matter.
  8. Write `value` in Markdown - inline only (`**bold**`, backticks, links), never a line break."#, track),

        tool("todo_list", &[("value", json!({"type": "string"}))],
             r#"Create or re-capture the ToDo. Follow <todo_capability>.
  1. Format: todo_list {"value": "Objective: <goal>\n- [ ] <task_1>\n- [ ] <task_2>"}
  2. Example: todo_list {"value": "Objective: buy an iPhone 16 on amazon\n- [ ] open amazon.com\n- [ ] search for iphone 16\n- [ ] place the order"}"#, track),

        tool("update_todo", &[("value", json!({"type": "string"}))],
             r#"Mark ONE task complete - only after its effect is visually confirmed in the current input.
  1. Format: update_todo {"value": "<task_number>"}
  2. Example: update_todo {"value": "1"}"#, track),

        tool("skills", &[("type", json!({"type": "string", "enum": ["load", "remove"]})), ("id", json!({"type": "integer"}))],
             r#"Load a skill into <skills>, or remove it. <skills> lists the skill files by [id] with their size; a loaded skill shows its content there every step until you remove it.
  1. Format: skills {"type": "<load|remove>", "id": <id_from_skills>}
  2. Examples:
    1. skills {"type": "load", "id": 1} - the scraping skill, before an in-depth read with `run_script` type "scrape"
    2. skills {"type": "remove", "id": 1} - once the reading is done, to free the context
  3. A `run_script` scrape loads the scraping skill itself when it opens scrape mode; remove it when you are done with it.
  4. <domain_knowledge> is not a skill: it comes with the current tab's site and goes with it on its own, has no id, and this tool does not touch it."#, track),

        tool("done", &[("value", json!({"type": "string"}))],
             r#"Ends the loop - the completion tool. Follow <task_completion>: a dedicated final step, never combined with any other action.
  1. Format: done {"value": "<end_to_end_summary>"}
  2. Example: done {"value": "Opened amazon.com, searched for the iPhone 16 128GB and recorded its price: $799"}"#, track),
    ]);
    tools
}

/// The tools of scrape mode, offered on the steps after a run_script of type
/// "scrape" brought records back, until `exit_scrape_mode` (controller/
/// run_script): `more` reads the next three screens, `exit_scrape_mode`
/// leaves. Defined here so every tool this crate offers a model is still in
/// this one file.
pub fn scraping_tools() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| {
        vec![
            tool("more", &[("value", json!({"type": "string"}))],
                 r#"The next 3 screens of the page being read, with the same script: the page scrolls on from where the last read ended, and a clean screenshot of each screen comes with the next input.
  1. Format: more {"value": "<what you are looking for next on this page>"}
  2. Call it while what the task needs goes on below. Not once the read said the page ends there.
  3. Over 10000 tokens nothing comes back and the page goes back to where this `more` began: `exit_scrape_mode`, then read again with a narrower script."#, &[]),
            tool("exit_scrape_mode", &[("value", json!({"type": "string"}))],
                 r#"Leave scrape mode: the look comes off the page, the next step scans it as normal and the page tools are back.
  1. Format: exit_scrape_mode {"value": "<what the data gave and which parts of the task it settles>"}
  2. Call it in the same turn as the `scratchpad` notes that keep what the task needs from the data. After it, the steps of the mode leave your history; your run_script call and its data stay, and so does the scratchpad."#, &[]),
        ]
    })
}

/// The whole schema of scrape mode (`LLMManager::set_scrape_step`): the two
/// tools above plus scratchpad, update_todo and skills, and nothing else - no
/// notes tool, no page tool.
pub fn scrape_mode_tools() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| {
        let keep: Vec<Value> = main_tools_quality()
            .iter()
            .filter(|t| matches!(t.get("name").and_then(Value::as_str), Some("scratchpad" | "update_todo" | "skills")))
            .cloned()
            .collect();
        scraping_tools().iter().cloned().chain(keep).collect()
    })
}

/// The names scraping mode accepts (tool_calls_to_steps).
pub fn scrape_mode_names() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| {
        scrape_mode_tools()
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    })
}

/// The `dialog` tool: offered only on a step whose page is held by a JavaScript
/// alert, confirm or prompt, which the input shows as `<dialog>` in place of
/// `<element_tree>` (controller/dialog, `LLMManager::set_dialog_step`).
pub fn dialog_tool() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| {
        tool("dialog", &[("id", json!({"type": "integer"})), ("value", json!({"type": "string"}))],
             r#"Answer the popup shown in <dialog>: a JavaScript alert, confirm or prompt the page opened. The page is frozen until it is answered, so this is the one page action that works while <dialog> is shown.
  1. Format: dialog {"id": <button_from_dialog>, "value": "<text_for_a_prompt_or_empty>"}
  2. Examples:
    1. dialog {"id": 1, "value": ""} - press [1] OK on an alert or a confirm
    2. dialog {"id": 2, "value": ""} - press [2] Cancel
    3. dialog {"id": 1, "value": "2 adults"} - type "2 adults" into a prompt's text box, then press OK
  3. `id` is a button [n] from <dialog>, NOT an [id] from <element_tree>. An alert has only [1] OK.
  4. Read the question first: OK goes ahead with what the page asks, often something that cannot be undone (delete, leave, pay); Cancel does not.
  5. `value` is typed only into a prompt, and only with OK; give "" otherwise."#, &[])
    })
}

/// The quality notes tool: `reasoning` {thinking (optional), memory, next_goal}.
pub fn main_reasoning_tool() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| tool_opt("reasoning", &[],
             r#"Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its visible-change guard; on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails on the page, a new page appears, or the route said "think". A page that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.
- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is not rendered on the screenshot, means this step thinks.
- A step may be `reasoning` alone when you need to think before you act (it costs a fresh scan); the next step then does the work. Do not chain several such steps.
- Not used on the `done` step: `done` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: amazon.com home page loaded in the current tab as predicted, the search box is empty. Targets: id 4 (searchbox 'Search Amazon').", "next_goal": "Doing: search 'iphone 16 128gb' and submit (ToDo: Find iPhone 16). If the results page lists iPhone 16 products → Next: think: pick the iPhone 16 128GB result."}
  2. New surface (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, the Gmail compose dialog is open as predicted, the To field is empty. UNDERSTAND: known: To, Subject and the body are in the tree as textboxes inside the compose dialog; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch with enter false and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. click Send, if the compose dialog closes and 'Message sent' appears → think: verify the mail in Sent, then mark the ToDo.", "memory": "Last step worked: compose dialog open, guard held. Targets: id 12 (textbox 'To'), id 14 (textbox 'Subject'), id 20 (textbox 'Message Body').", "next_goal": "Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: click the Send button."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "Failed: the To field is still empty after input on id 74; the screenshot shows the caret in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: click the To field first, then input. Guard: the To field shows the address. Route holds after this.", "memory": "Last step failed: To field empty, focus was in Subject. Correction carried: click a field before typing into it in this compose dialog. Targets: id 74 (textbox 'To').", "next_goal": "Doing: click the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject."}"#,
             main_reasoning_fields(), REASONING_OPTIONAL))
}

/// The fast notes tool: `memory`, carrying `next_goal` alone.
pub fn main_memory_tool_fast() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| tool("memory", &[],
             r#"Your one note for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.
- One field, `next_goal`, filled every step, in labeled sections (see <next_goal>): "Memory:" (the verdict on the previous step judged on the CURRENT input, the key context, the Targets line resolved from the CURRENT <element_tree>), "Reasoning:" (one line, only when a decision was made), "Doing:" (this step, with its ToDo task), "If <change> → Next: <named action | think: <decision>>".
- There is no thinking field: the reasoning stays silent (see <silent_reasoning>). No screenshot is sent in this mode: judge on the <element_tree> alone and name what in the tree each verdict rests on.
- A step may be `memory` alone when you must think before you act (it costs a fresh scan); do not chain several such steps. Not used on the `done` step: `done` is always the only call of its turn.
- Format: memory {"next_goal": "Memory: ... Targets: ... Doing: ... (ToDo: ...). If ... → Next: ..."}
- Example:
  memory {"next_goal": "Memory: worked: amazon.com home page loaded as predicted, the tree shows searchbox 'Search Amazon' with no value; current tab 1. Targets: id 4 (searchbox 'Search Amazon'). Doing: search 'iphone 16 128gb' and submit (ToDo: Find iPhone 16). If the results page lists iPhone 16 products → Next: think: pick the iPhone 16 128GB result."}"#,
             main_memory_fields_fast()))
}

/// Claude's notes tool (quality mode): `agent_memory` {assessment (optional),
/// memory, next_goal}, word for word the desktop agents' tool.
pub fn claude_reasoning_tool() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| tool_opt("agent_memory", &[],
             r#"Your written notes for this step: a log entry, not an action. It has no result and nothing waits on it, so it NEVER ends a turn: emit it first in the list and the calls that do this step's "Doing" in the SAME message. A message holding only this call and no action is a wasted turn. The call is replayed in <agent_history>, so your future self can read what you knew and decided at every step.
- `memory` and `next_goal` are filled every step. `assessment` follows <assessment>: omit it (or pass "") on a route step.
- Not used on the `done` step: `done` is always the only call of its turn.
- Format: agent_memory {"assessment": "", "memory": "...", "next_goal": "..."}"#,
             claude_reasoning_fields(), REASONING_OPTIONAL))
}

pub fn main_tools_quality() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(main_reasoning_tool())))
}

pub fn main_tools_fast() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(main_memory_tool_fast())))
}

/// Quality mode on any Claude model: the same tools with `agent_memory` first.
pub fn main_tools_claude() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| main_tools(&[], Some(claude_reasoning_tool())))
}

/// Tools that don't exist for a single-tab (parallel) agent: its whole run
/// happens in one dedicated tab of a shared browser, so the tab lifecycle is
/// off the table. update_tab/navigate_tab stay — they act on the current tab.
const SINGLE_TAB_EXCLUDED: [&str; 3] = ["new_tab", "switch_tab", "close_tab"];

fn without_tab_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter(|t| {
            t.get("name")
                .and_then(Value::as_str)
                .map(|n| !SINGLE_TAB_EXCLUDED.contains(&n))
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

pub fn main_tools_quality_single_tab() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| without_tab_tools(main_tools_quality()))
}

pub fn main_tools_fast_single_tab() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| without_tab_tools(main_tools_fast()))
}

pub fn main_tools_claude_single_tab() -> &'static Vec<Value> {
    static T: OnceLock<Vec<Value>> = OnceLock::new();
    T.get_or_init(|| without_tab_tools(main_tools_claude()))
}

/// Per-action defaults — guarantees route_action always receives every field
/// of an action, even one the model left out. The notes tools (`reasoning`
/// in quality mode, `memory` in fast) are listed so the parser accepts the
/// call; the service pulls it out before routing, and the mode's name set
/// decides which of the two is offered.
pub fn main_action_defaults() -> &'static Value {
    static D: OnceLock<Value> = OnceLock::new();
    D.get_or_init(|| {
        json!({
            "reasoning": {"thinking": "", "memory": "", "next_goal": ""},
            "agent_memory": {"assessment": "", "memory": "", "next_goal": ""},
            "memory": {"next_goal": ""},
            "new_tab": {"value": ""},
            "switch_tab": {"id": 0},
            "close_tab": {"id": 0},
            "update_tab": {"value": ""},
            "navigate_tab": {"value": "reload"},
            "click": {"id": 0, "times": 1},
            "hold_click": {"id": 0, "time": 1},
            "input": {"id": 0, "value": "", "enter": false},
            "keyboard": {"value": ""},
            "dialog": {"id": 0, "value": ""},
            "run_script": {"id": 0, "type": "action", "value": ""},
            "more": {"value": ""},
            "exit_scrape_mode": {"value": ""},
            "scroll": {"id": 0, "direction": "down"},
            "wait": {"value": ""},
            "scratchpad": {"value": ""},
            "todo_list": {"value": ""},
            "update_todo": {"value": ""},
            "skills": {"type": "load", "id": 0},
            "done": {"value": ""},
        })
    })
}

pub fn main_tool_names() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| {
        main_tools_quality()
            .iter()
            .chain(std::iter::once(dialog_tool()))
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    })
}

/// Fast mode has no `reasoning` tool, so a call to it is an unknown tool
/// there, exactly as before.
pub fn main_tool_names_fast() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| {
        main_tools_fast()
            .iter()
            .chain(std::iter::once(dialog_tool()))
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    })
}

/// Claude's quality registry: `agent_memory` in place of `reasoning`, so a call
/// to `reasoning` is an unknown tool there, as on the desktop agents.
pub fn main_tool_names_claude() -> &'static HashSet<String> {
    static N: OnceLock<HashSet<String>> = OnceLock::new();
    N.get_or_init(|| {
        main_tools_claude()
            .iter()
            .chain(std::iter::once(dialog_tool()))
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    })
}

// -- dialect emitters --------------------------------------------------------

/// OpenAI/OpenRouter/Groq/Together chat-completions function format.
fn tools_openai(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| json!({"type": "function", "function": t}))
        .collect()
}

/// Chat-completions format with structured-outputs `strict` mode: the
/// provider then ENFORCES the schema at decode time (every `required` field
/// present, no extra keys) instead of treating it as advisory — without it a
/// model can omit `id` and the call still comes back. OpenAI-only: strict
/// demands `additionalProperties: false`, which Gemini's declaration parser
/// rejects, and Groq/OpenRouter route to models with uneven strict support;
/// for every non-strict provider, tool_calls_to_steps' missing-field reject
/// is the enforcement layer.
fn tools_openai_strict(registry: &[Value]) -> Vec<Value> {
    registry
        .iter()
        .map(|t| {
            let mut f = t.as_object().cloned().unwrap_or_default();
            if let Some(Value::Object(params)) = f.get_mut("parameters") {
                params.insert("additionalProperties".into(), json!(false));
                // Strict mode demands EVERY property in `required`. An optional
                // field (the `reasoning` tool's `thinking`) is expressed the
                // way strict allows instead: nullable and required, so the
                // model sends null on a route step. The parser reads null as
                // "not given", exactly like an omitted field.
                let required: Vec<String> = params
                    .get("required")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                let mut all_required: Vec<Value> = required.iter().map(|k| json!(k)).collect();
                if let Some(Value::Object(props)) = params.get_mut("properties") {
                    for (k, schema) in props.iter_mut() {
                        if required.contains(k) {
                            continue;
                        }
                        if let Value::Object(s) = schema {
                            if let Some(Value::String(ty)) = s.get("type").cloned() {
                                s.insert("type".into(), json!([ty, "null"]));
                            }
                        }
                        all_required.push(json!(k));
                    }
                }
                params.insert("required".into(), Value::Array(all_required));
            }
            f.insert("strict".into(), json!(true));
            json!({"type": "function", "function": Value::Object(f)})
        })
        .collect()
}

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
            m.insert(
                "input_schema".into(),
                t.get("parameters").cloned().unwrap_or(Value::Null),
            );
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
            m.insert(
                "parameters".into(),
                t.get("parameters").cloned().unwrap_or(Value::Null),
            );
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
            m.insert(
                "parameters".into(),
                t.get("parameters").cloned().unwrap_or(Value::Null),
            );
            with_description(m, t)
        })
        .collect()
}

// -- tool_calls -> steps -----------------------------------------------------

/// Python `int(x)` (falls back to Err for unparseable values).
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
/// track): actions for route_action (tracking params STRIPPED), calls echoed
/// back next request keyed by id, rejects for unknown/empty calls, track =
/// the tracking params stitched from the step's calls (the `reasoning` call
/// in quality mode, the first action call in fast mode). `names` is the
/// registry this mode sends (main_tool_names / main_tool_names_fast).
#[allow(clippy::type_complexity)]
pub fn tool_calls_to_steps(
    tool_calls: &Value,
    track_params: &[(String, String)],
    names: &HashSet<String>,
) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<(String, String)>) {
    let mut actions: Vec<Value> = Vec::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut rejects: Vec<Value> = Vec::new();
    let mut track: Vec<(String, String)> =
        track_params.iter().map(|(k, _)| (k.clone(), String::new())).collect();
    let defaults_map = main_action_defaults();

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
        let defaults = defaults_map.get(&name).and_then(Value::as_object);
        let known_name = defaults.is_some() && names.contains(&name);
        if !known_name {
            let shown = if name.is_empty() { "(unnamed)" } else { name.as_str() };
            let mut sorted_names: Vec<&String> = names.iter().collect();
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
        }
        let defaults = defaults.unwrap();
        // A MISSING action field is a schema violation, not an omitted
        // optional — every param on every tool is `required`. Letting the
        // defaults fill it would silently promote a malformed turn into a
        // real page action: an `input` missing `id` becomes id 0, which
        // targets nothing and then reports a misleading stale-id error —
        // observed in the wild as a run burning 8 straight steps
        // re-resolving a fresh id while never actually sending one. Reject
        // instead, naming the missing fields, so the model fixes the CALL.
        // (Tracking params are exempt: they are legitimately "" after the
        // first call of a step.)
        // The notes tools' `thinking` / `assessment` is the one optional
        // field: a route step leaves it out on purpose.
        let optional: &[&str] = if name == "reasoning" || name == "agent_memory" { REASONING_OPTIONAL } else { &[] };
        let missing: Vec<String> = defaults
            .keys()
            .filter(|k| !args.contains_key(k.as_str()) && !optional.contains(&k.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let mut fields: Vec<String> = defaults
                .keys()
                .filter(|k| !optional.contains(&k.as_str()))
                .cloned()
                .collect();
            fields.sort();
            rejects.push(json!({
                "id": call_id,
                "name": name,
                "arguments": Value::Object(args),
                "error": format!(
                    "'{name}' was called without its required field(s): {}. Every field must be present: {}. Re-issue the call with ALL of them filled in — never leave one out.",
                    missing.join(", "),
                    fields.join(", ")
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
            // A tool's own field called `type` (run_script's) cannot sit under
            // the action's `type`, which names the tool every router reads:
            // it travels as `script_type`. The replayed call keeps it as sent.
            let slot = if key == "type" { "script_type".to_string() } else { key.clone() };
            action.insert(slot, value);
        }
        actions.push(Value::Object(action));
        // Echo back only the fields this tool actually HAS — models
        // sometimes add junk keys, and `calls` is replayed verbatim into the
        // next request, so a kept extra teaches the model to repeat it.
        let known: HashSet<&str> = defaults
            .keys()
            .map(String::as_str)
            .chain(track.iter().map(|(k, _)| k.as_str()))
            .collect();
        let kept: Map<String, Value> = args
            .iter()
            .filter(|(k, _)| known.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        calls.push(json!({"id": call_id, "name": name, "arguments": Value::Object(kept)}));
    }
    (actions, calls, rejects, track)
}

// -- usage normalization -----------------------------------------------------

fn int_or_zero(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if truthy(v) => int_of(v).unwrap_or(0),
        _ => 0,
    }
}

/// Normalize a provider usage dict to {input_tokens, output_tokens,
/// total_tokens, context_tokens}, tolerating both key styles.
/// context_tokens is the TRUE size of the prompt actually sent this turn —
/// cached tokens still occupy the context window, so the cache classes are
/// added back (exact for every provider; non-Anthropic sets them to 0).
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
    /// Ctrl+C (KeyboardInterrupt) or SystemExit while the provider waited. Not a
    /// failure: no retry, and it reaches the caller as the same Python error.
    Interrupted(PyErr),
}

impl From<SendErr> for PyErr {
    fn from(e: SendErr) -> PyErr {
        match e {
            SendErr::Failed(msg) => PyException::new_err(msg),
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
            // several (the page's screenshot and the screens a scrape read
            // covered) as a list of them (llm_provider screenshots()).
            let shot: Option<Bound<'_, PyAny>> = match shots.len() {
                0 => None,
                1 => Some(pyo3::types::PyString::new(py, &shots[0]).into_any()),
                _ => Some(pyo3::types::PyList::new(py, shots)?.into_any()),
            };
            let out = provider.bind(py).call_method1("send_request", (messages, model, shot))?;
            match pythonize::depythonize(&out) {
                Ok(reply) => Ok(reply),
                // A number no serde_json number holds (a tool argument past 64
                // bits): read the JSON text instead, where it becomes a float,
                // as it did when the providers were Rust.
                Err(_) => {
                    let text: String = py.import("json")?.call_method1("dumps", (&out,))?.extract()?;
                    serde_json::from_str(&text).map_err(|e| PyException::new_err(e.to_string()))
                }
            }
        })();
        reply.map_err(|e| {
            if e.is_instance_of::<PyException>(py) {
                SendErr::Failed(e.value(py).to_string())
            } else {
                SendErr::Interrupted(e)
            }
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

/// Manager to route requests to the correct LLM provider.
#[pyclass(frozen, name = "LLMManager")]
pub struct LLMManager {
    pub provider: String,
    pub model_short_name: String,
    pub runtime_api_key: Option<String>,
    /// Sub-agent flag — NOT a "native tools" switch. Stays false for the
    /// MAIN DRIVER because each provider gates its screenshot splice on it.
    pub cli_agent: bool,
    pub mode: String,
    pub speed: String,
    pub native_tools: bool,
    /// Any Claude model, whatever the provider (anthropic direct, or a Claude
    /// entry on OpenRouter): quality mode runs it on `agent_memory` and
    /// claude_prompt.md, as the desktop agents do (LLMManager.claude there).
    /// Set once from the primary model.
    pub claude: bool,
    cli_fallback_model: Option<String>,
    primary_model_info: Value,
    /// The shared Python provider for this mode's tool list.
    provider_impl: Py<PyAny>,
    /// The same provider with the `dialog` tool in its list (dialog_tool), for a step
    /// whose page is held by a JavaScript alert, confirm or prompt.
    dialog_impl: Py<PyAny>,
    /// Whether the requests offer `dialog` (`set_dialog_step`).
    dialog_step: AtomicBool,
    /// Scrape mode's own list - more, exit_scrape_mode, scratchpad,
    /// update_todo and nothing else - on the steps after a run_script scrape
    /// brought records back, until `exit_scrape_mode` (`set_scrape_step`).
    scrape_impl: Py<PyAny>,
    scrape_step: AtomicBool,
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
        single_tab: bool,
    ) -> PyResult<Self> {
        let llm = shared_llm(py)?;
        let provider = provider.to_lowercase();
        let runtime_api_key = api_key;
        let native_tools = mode != "text";
        let cli_fallback_model: Option<String> = if cli_agent {
            llm.call_method1("_pick_cli_fallback", (&provider, model))?.extract()?
        } else {
            None
        };
        let model_info = resolve_model_info(py, &provider, model)?;
        // The memory-compression handoff (mode="text") gets the same output cap
        // as on every platform; every other mode keeps the provider's default.
        let max_tokens: Option<i64> = if native_tools {
            None
        } else {
            Some(llm.getattr("HANDOFF_MAX_TOKENS")?.extract()?)
        };

        let api_name = model_info.get("api_name").and_then(Value::as_str).unwrap_or(model).to_lowercase();
        let claude = provider == "anthropic" || api_name.contains("claude");

        // Hand each provider its dialect's tool definitions — the driver gets
        // its action tools, thinking-less in fast mode, `agent_memory` in place
        // of `reasoning` on a Claude model in quality mode, tab-less in
        // single-tab (parallel) mode; mode="text" gets none.
        let registry: &Vec<Value> = match (speed == "fast", single_tab) {
            (true, true) => main_tools_fast_single_tab(),
            (true, false) => main_tools_fast(),
            (false, true) if claude => main_tools_claude_single_tab(),
            (false, false) if claude => main_tools_claude(),
            (false, true) => main_tools_quality_single_tab(),
            (false, false) => main_tools_quality(),
        };
        let native = native_tools;
        // One provider per tool list: the mode's own, and the same list with `dialog`
        // added, which a step whose page is held by a popup uses (`set_dialog_step`).
        // The tools go in the provider's own dialect; OpenAI direct takes the strict one.
        let make = |registry: &[Value], max_tokens: Option<i64>, info: &Value| -> PyResult<Py<PyAny>> {
            let tools = native.then(|| match provider.as_str() {
                "openai" => tools_openai_strict(registry),
                "anthropic" => tools_anthropic(registry),
                "google" => tools_gemini(registry),
                "perplexity" => tools_perplexity(registry),
                _ => tools_openai(registry),
            });
            let kwargs = PyDict::new(py);
            kwargs.set_item("cli_agent", cli_agent)?;
            kwargs.set_item("max_tokens", max_tokens)?;
            kwargs.set_item("api_key", runtime_api_key.as_deref())?;
            let args = (
                provider.as_str(),
                model,
                pythonize::pythonize(py, info)?,
                pythonize::pythonize(py, &tools)?,
            );
            Ok(llm.call_method("build_provider", args, Some(&kwargs))?.unbind())
        };
        let provider_impl = make(registry, max_tokens, &model_info)?;
        let with_dialog: Vec<Value> =
            registry.iter().chain(std::iter::once(dialog_tool())).cloned().collect();
        let dialog_impl = make(&with_dialog, max_tokens, &model_info)?;
        // Scraping mode: the chunk in view is the whole input, so the schema
        // holds the tools of the job and the two bookkeeping tools only.
        let scrape_impl = make(scrape_mode_tools(), max_tokens, &model_info)?;

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
            dialog_impl,
            dialog_step: AtomicBool::new(false),
            scrape_impl,
            scrape_step: AtomicBool::new(false),
            state: Mutex::new(CoreState {
                model: model_info
                    .get("api_name")
                    .and_then(Value::as_str)
                    .unwrap_or(model)
                    .to_string(),
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

    fn apply_model_info(&self, info: &Value) {
        let mut state = self.state.lock().unwrap();
        state.model = info
            .get("api_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state.has_vision = info.get("vision").and_then(Value::as_bool).unwrap_or(true);
        state.display_name = info
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state.model_info = info.clone();
        drop(state);
        // The providers that keep their table entry (openrouter, groq, openai,
        // perplexity, together, cerebras) read the effort from it: point them
        // at the new one, as the Python manager's _apply_model_info does.
        Python::attach(|py| {
            let Ok(entry) = pythonize::pythonize(py, info) else { return };
            for p in [
                &self.provider_impl,
                &self.dialog_impl,
                &self.scrape_impl,
            ] {
                let p = p.bind(py);
                if p.hasattr("model_info").unwrap_or(false) {
                    let _ = p.setattr("model_info", &entry);
                }
            }
        });
    }

    /// Three idempotent tries against the model currently loaded. Errors
    /// carry the last provider message.
    fn attempt(&self, messages: &[Value], shots: &[String]) -> Result<SendOutcome, SendErr> {
        let mut last_error = String::new();
        for attempt in 0..3 {
            // Deep-copy per attempt so provider mutations cannot compound.
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
            let t0 = Instant::now();
            match send_py(self.step_provider(), &attempt_messages, &model, shots) {
                Ok(response) => {
                    let elapsed = t0.elapsed().as_secs_f64();
                    let usage = normalize_usage(response.get("usage"));
                    {
                        let mut state = self.state.lock().unwrap();
                        state.last_call_seconds = elapsed;
                        state.last_usage = usage.clone();
                    }
                    if std::io::stdout().is_terminal() {
                        // \r first: overwrite any spinner residue. TTY-only —
                        // never leak into the UI subprocess pipe.
                        let raw_usage = response.get("usage").cloned().unwrap_or(json!({}));
                        let cached = match int_or_zero(raw_usage.get("cache_read_input_tokens")) {
                            0 => int_or_zero(
                                raw_usage
                                    .get("prompt_tokens_details")
                                    .and_then(|d| d.get("cached_tokens")),
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
                            "tool_calls": message.get("tool_calls").cloned().unwrap_or(json!([])),
                            META_KEY: message.get(META_KEY).cloned().unwrap_or(json!({})),
                        })));
                    }
                    let content = message.get("content").cloned().unwrap_or(Value::Null);
                    return Ok(SendOutcome::Text(match content {
                        Value::String(s) => s,
                        // No content (a model that spent its whole budget thinking):
                        // empty, which memory compression drops, as it drops Python's
                        // None. Never the string "None".
                        Value::Null => String::new(),
                        other => py_str_of(&other),
                    }));
                }
                Err(SendErr::Interrupted(e)) => return Err(SendErr::Interrupted(e)),
                Err(SendErr::Failed(e)) => {
                    last_error = e;
                    if attempt < 2 {
                        println!(
                            "⚠️ {display_name} request failed (attempt {}/3): {last_error}",
                            attempt + 1
                        );
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

    /// Send with idempotent retries; sub-agents only get one fallback call.
    /// GIL-free — callers detach around this.
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
        let display = self.state.lock().unwrap().display_name.clone();
        println!(
            "⚠️ {display} failed 3 attempts — this step only, falling back to {}...",
            fallback_info.get("display_name").and_then(Value::as_str).unwrap_or(&fallback_model)
        );
        self.apply_model_info(&fallback_info);
        let result = self.attempt(messages, shots);
        // Always revert: the fallback covers this call, not the whole run.
        self.apply_model_info(&self.primary_model_info.clone());
        match result {
            Ok(out) => {
                println!(
                    "↩️ Back on {} for the next step.",
                    self.state.lock().unwrap().display_name
                );
                Ok(out)
            }
            Err(e) => {
                println!(
                    "❌ Fallback {} also failed 3 attempts — stopping.",
                    fallback_info.get("display_name").and_then(Value::as_str).unwrap_or(&fallback_model)
                );
                Err(e)
            }
        }
    }

    /// Whether the loaded model's table entry asks for the Responses
    /// endpoint with async tool definitions (`"async_tools": true`, GPT-6
    /// Astra): the driver appends the matching prompt addendum and guards a
    /// bundled `done`.
    /// Offer the `dialog` tool on the requests from now on, or stop: the agent loop
    /// sets it on every step, true while a popup holds the page.
    pub fn set_dialog_step(&self, on: bool) {
        self.dialog_step.store(on, Ordering::Relaxed);
    }

    /// Scrape mode on the requests from now on, or off: the agent loop sets it
    /// on every step, true from a run_script scrape until `exit_scrape_mode`.
    pub fn set_scrape_step(&self, on: bool) {
        self.scrape_step.store(on, Ordering::Relaxed);
    }

    /// The provider holding this step's tool list.
    fn step_provider(&self) -> &Py<PyAny> {
        if self.scrape_step.load(Ordering::Relaxed) {
            return &self.scrape_impl;
        }
        if self.dialog_step.load(Ordering::Relaxed) {
            &self.dialog_impl
        } else {
            &self.provider_impl
        }
    }

    pub fn async_tools(&self) -> bool {
        self.primary_model_info.get("async_tools").and_then(Value::as_bool).unwrap_or(false)
    }

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
            false,
        )
    }

    /// Send request to the selected provider with idempotent retries.
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

    /// Debugging: set the two step switches as the agent loop would and
    /// name the tools the next request carries, read off the provider line
    /// `step_provider` picks (its `tools`, in whichever dialect it holds).
    #[pyo3(signature = (scrape=false, dialog=false))]
    fn _step_tool_names(&self, py: Python<'_>, scrape: bool, dialog: bool) -> PyResult<Vec<String>> {
        self.set_scrape_step(scrape);
        self.set_dialog_step(dialog);
        let tools = self.step_provider().bind(py).getattr("tools")?;
        if tools.is_none() {
            return Ok(Vec::new());
        }
        let list: Value = pythonize::depythonize(&tools)?;
        Ok(list
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| {
                        t.get("name")
                            .or_else(|| t.get("function").and_then(|f| f.get("name")))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default())
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
// Test hook — the converter exposed to Python so it can be differentially
// tested against the original (and reused by any future Python caller).
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(signature = (tool_calls, speed=None))]
pub fn _tool_calls_to_steps_debug<'py>(
    py: Python<'py>,
    tool_calls: Bound<'py, PyAny>,
    speed: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let calls_val: Value = if tool_calls.is_none() {
        Value::Null
    } else {
        pythonize::depythonize(&tool_calls)?
    };
    let fast = speed.as_deref() == Some("fast");
    let track_params = if fast { main_memory_fields_fast() } else { main_reasoning_fields() };
    let names = if fast { main_tool_names_fast() } else { main_tool_names() };
    let (actions, calls, rejects, track) = tool_calls_to_steps(&calls_val, track_params, names);
    let track_map: Map<String, Value> =
        track.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
    pythonize::pythonize(
        py,
        &json!([actions, calls, rejects, Value::Object(track_map)]),
    )
    .map_err(Into::into)
}

/// The registry a speed mode sends (quality unless "fast"; "scrape" for the
/// scraping mode's own list), for tests; with `strict`, as the OpenAI strict
/// dialect rewrites it.
#[pyfunction]
#[pyo3(signature = (speed=None, strict=false))]
pub fn _main_tools_debug<'py>(py: Python<'py>, speed: Option<String>, strict: bool) -> PyResult<Bound<'py, PyAny>> {
    let registry = match speed.as_deref() {
        Some("fast") => main_tools_fast(),
        Some("scrape") => scrape_mode_tools(),
        _ => main_tools_quality(),
    };
    let out = if strict { tools_openai_strict(registry) } else { registry.clone() };
    pythonize::pythonize(py, &Value::Array(out)).map_err(Into::into)
}
