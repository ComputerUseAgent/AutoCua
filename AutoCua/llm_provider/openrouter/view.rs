//! view: the OpenRouter models' table (MODEL_MAPPINGS) and the reasoning
//! field each model is sent (get_reasoning_params).
//!
//! The Rust copy of llm_provider/openrouter/view.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.openrouter.view
//! (llm_provider/openrouter/__init__.py points that name here), and finds the
//! same names in the same order, with the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what view.py's
//! import line bound (entry_value) and calls _build(), which runs the rest of
//! view.py's lines in their order: the table, then each function where its
//! def was.
//!
//! Every global a line reads (MODEL_MAPPINGS, entry_value) is read from this
//! module at that moment, as Python reads a global, so a caller or a test
//! that replaces or changes one is obeyed.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyString, PyTuple};

use crate::ios::controller;

type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One value in a model's entry: a str (Text), a bool (Flag) or a whole
/// number (Number).
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
    Number(i64),
}

use Value::{Flag, Number, Text};

// Model mappings for OpenRouter provider
// Maps user-friendly names to actual API model names

// `effort` is how hard the model thinks, carried to OpenRouter as its unified
// reasoning field: {"reasoning": {"effort": ...}}. OpenRouter translates that
// into whatever each backend actually speaks, so one key covers OpenAI,
// Anthropic, Gemini, Grok, Mistral and Qwen alike.
//
// THE LEVELS ARE NOT A SHARED LADDER. Each model publishes its own set, and a
// level outside that set is not portable: `xhigh`/`max` do not exist on
// Gemini, Grok or Mistral, `medium` does not exist on Kimi, and `minimal`
// exists ONLY on Gemini Flash and Qwen. Every value below was checked against
// that model's own published list — - so treat this table as per-model
// configuration, not as a dial you can sweep uniformly.
//
// Stating the level matters even where it matches the default, because the
// defaults disagree wildly: Kimi K3 defaults to `max`, Qwen to `xhigh`, the
// Anthropic trio to `high`, Grok 4.3 to `low`. Left unset, two models in the
// same drop-up behave nothing alike, and the expensive ones bill against the
// same max_tokens ceiling that has to hold the tool call.
//
// Reasoning cannot be switched off on Gemini, Grok 4.5 or Qwen — those are
// mandatory-reasoning models, so the lowest level they publish is the floor.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add an OpenRouter model, copy a block and change
// its values: Text("..."), Flag(true) / Flag(false) and Number(16000). The
// app rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("gemini-3.6-flash", &[
        ("api_name", Text("google/gemini-3.6-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.6 Flash")),
        // 3.6 Flash is the one Gemini that publishes `minimal`.
        ("effort", Text("minimal")),
        ("coder_effort", Text("low")),
    ]),
    ("gemini-3.8-flash", &[
        ("api_name", Text("google/gemini-3.8-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.8 Flash")),
        // 3.8 Flash publishes low/medium/high only - `minimal` returns an
        // error there - so `low` is its floor.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // The plain route, not `google/gemini-4-argon:batch`: the half-price
    // `:batch` twins are refused on chat/completions and on Responses alike
    // (404 "cannot be used with the chat/completions endpoint", probed
    // 2026-11-04). OpenRouter runs them only as batch jobs, and a live step
    // cannot wait for one.
    ("gemini-4-argon", &[
        ("api_name", Text("google/gemini-4-argon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Argon")),
        // Probed 2026-11-04 with the real driver and coder prompts: `low`
        // batches the notes call with the actions and reads its tool results
        // on the next turn. About ten seconds a call.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // Carbon and Hydrogen publish low/medium/high (no `minimal`), probed
    // 2026-11-14. Plain routes, not `:batch`, as for Argon.
    ("gemini-4-carbon", &[
        ("api_name", Text("google/gemini-4-carbon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Carbon")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("gemini-4-hydrogen", &[
        ("api_name", Text("google/gemini-4-hydrogen")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Hydrogen")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // Neon takes low/medium/high/xhigh (no `minimal`; probed on Google's
    // API 2026-11-25, xhigh here 2026-11-26) and runs at the top, `xhigh`;
    // the coder at `high`.
    ("gemini-4-neon", &[
        ("api_name", Text("google/gemini-4-neon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Neon")),
        ("effort", Text("xhigh")),
        ("coder_effort", Text("high")),
    ]),
    // OpenAI is GPT-6 Astra at `medium`, the same entry the direct openai
    // provider carries, so switching a model between the two providers is a
    // routing change and not a capability change.
    ("gpt-6-astra", &[
        ("api_name", Text("openai/gpt-6-astra")),
        // Batches only on the Responses endpoint with async tools; the
        // provider routes it there (see ../responses.py).
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6 Astra")),
        // Astra publishes low/medium/high/xhigh/max and rejects `none` and
        // `minimal`; one entry, at `medium`.
        ("effort", Text("medium")),
        ("coder_effort", Text("high")),
        ("coder_max_tokens", Number(16000)),
    ]),
    // Luna and Sol are OpenRouter only (not in the direct openai table). Both
    // behave like Astra: probed 2026-09-23 with the real driver, coder and
    // minion prompts, chat/completions gave the notes call alone turn after
    // turn at every effort, and the async Responses route batches them.
    ("gpt-6-luna", &[
        ("api_name", Text("openai/gpt-6-luna")),
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6 Luna")),
        // Accepts every level from none to max; `low` matches the table.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_max_tokens", Number(16000)),
    ]),
    ("gpt-6-sol", &[
        ("api_name", Text("openai/gpt-6-sol")),
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6 Sol")),
        // Accepts every level from none to max; `low` matches the table.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_max_tokens", Number(16000)),
    ]),
    // GPT-6.1 Terra behaves like the GPT-6 entries (probed 2026-11-04): on
    // chat/completions it gave the notes call alone three turns in a row, and
    // the async Responses route batches the whole step. The plain route: its
    // `:batch` twin is refused (see gemini-4-argon).
    ("gpt-6.1-terra", &[
        ("api_name", Text("openai/gpt-6.1-terra")),
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6.1 Terra")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_max_tokens", Number(16000)),
    ]),
    ("claude-opus-5", &[
        ("api_name", Text("anthropic/claude-opus-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5")),
        ("effort", Text("medium")),
        ("coder_effort", Text("high")),
        ("coder_tool_choice", Text("auto")),
    ]),
    ("claude-sonnet-5", &[
        ("api_name", Text("anthropic/claude-sonnet-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_tool_choice", Text("auto")),
    ]),
    // Claude 5.5: Anthropic rejects forced tool use on both ("required" is a
    // 400 "tool_choice: type tool and any are not supported for this model",
    // probed 2026-10-01), so both carry `tool_choice`, sent in place of
    // "required" for them only.
    ("claude-sonnet-5.5", &[
        ("api_name", Text("anthropic/claude-sonnet-5.5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5.5")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("tool_choice", Text("auto")),
    ]),
    ("claude-opus-5.5", &[
        ("api_name", Text("anthropic/claude-opus-5.5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5.5")),
        ("effort", Text("medium")),
        ("coder_effort", Text("high")),
        ("tool_choice", Text("auto")),
    ]),
    ("grok-4.7", &[
        ("api_name", Text("x-ai/grok-4.7")),
        ("vision", Flag(true)),
        ("display_name", Text("Grok 4.7")),
        // Replaces 4.3 and 4.5 (2026-09-22). Probed with a forced tool call:
        // minimal, low and medium are all accepted and batch; `low` matches
        // the rest of the table.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("mimo-v2.6-pro-ultraspeed", &[
        ("api_name", Text("xiaomi/mimo-v2.6-pro-ultraspeed")),
        ("vision", Flag(true)),
        ("display_name", Text("MiMo V2.6 Pro UltraSpeed")),
        // Probed 2026-09-22: minimal and low both batch a forced tool call
        // in about a second; at `medium` the same probe came back with no
        // call at all, so `low` it is, for the coder too (no level up).
        ("effort", Text("low")),
        ("coder_effort", Text("low")),
    ]),
    ("deepseek-v4.1-flash", &[
        ("api_name", Text("deepseek/deepseek-v4.1-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("DeepSeek V4.1 Flash")),
        // Probed 2026-09-22: every level is accepted and batches; the
        // backends (Together, CoreWeave) report zero reasoning tokens at
        // each, so the level is a formality here.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("deepseek-v4.1-flash-1017", &[
        ("api_name", Text("deepseek/deepseek-v4.1-flash-1017")),
        ("vision", Flag(true)),
        ("display_name", Text("DeepSeek V4.1 Flash 1017")),
        // The 2026-10-17 snapshot, served by DeepSeek itself. Probed
        // 2026-11-04: `low` batches the notes call with the actions in about
        // two seconds, in the driver and in the coder. The coder stays at
        // `low`: DeepSeek's thinking mode answers a forced tool_choice with
        // a 400 (requesty/view.py), and this entry forces one.
        ("effort", Text("low")),
        ("coder_effort", Text("low")),
    ]),
    ("minimax-m3.1-flash", &[
        ("api_name", Text("minimax/minimax-m3.1-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("MiniMax M3.1 Flash")),
        // Probed 2026-11-04: with "required" it batched the notes call and the
        // action in two runs of three, and in the third acted on the next
        // turn; "auto" always split them, so it keeps "required".
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("muse-wonder", &[
        ("api_name", Text("meta/muse-wonder")),
        ("vision", Flag(true)),
        ("display_name", Text("Muse Wonder")),
        // Meta's backend accepts only tool_choice "auto" ("required" is a 400,
        // "only auto"), so this entry carries `tool_choice`, sent in place of
        // "required" for it only. Probed 2026-11-04: `low` batches the notes
        // call with the actions, or acts on the next turn.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("tool_choice", Text("auto")),
    ]),
    // Left out after the 2026-11-04 probe: meta/muse-spark-open and
    // tencent/hy4 call the notes tool alone turn after turn, at every effort,
    // with "required" or "auto", on chat/completions and on Responses. They
    // plan the step and never send the action, so they cannot drive a screen.
    // Muse Spark 1.3, Kimi K3 and Kimi K3.1 Pro were removed the same day:
    // they mostly sent the notes call first (Kimi at about nine seconds a
    // call), and on Requesty their coder at times did not act for three turns.
    ("claude-opus-5-fast", &[
        ("api_name", Text("anthropic/claude-opus-5-fast")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5 Fast")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_tool_choice", Text("auto")),
    ]),
    // Still the newest Mistral on OpenRouter — the line has not moved since
    // 2026-04-30, so this entry is current, not stale.
    ("mistral-medium-3.5", &[
        ("api_name", Text("mistralai/mistral-medium-3-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Mistral Medium 3.5")),
        // Mistral publishes only two levels, high and none, so `none` here
        // means reasoning genuinely off — not "lowest setting" as elsewhere.
        ("effort", Text("none")),
        ("coder_effort", Text("none")),
    ]),
    // Qwen: Alibaba's backend rejects tool_choice="required" whenever thinking
    // is on (400 "does not support being set to required or object in
    // thinking mode"), and this loop sends that on every call. Flash accepts
    // it only with thinking off and then emits one call per turn; Max cannot
    // switch thinking off at all. With tool_choice="auto" both batch at `low`
    // (probed 2026-09-22), so these two entries carry `tool_choice`, which
    // the provider sends in place of "required" for them ONLY; the loop's
    // no-tool-called nudge is the backstop for a prose-only turn.
    ("qwen3.8-max-0902", &[
        ("api_name", Text("qwen/qwen3.8-max-0902")),
        ("vision", Flag(true)),
        ("display_name", Text("Qwen3.8 Max (0902)")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("tool_choice", Text("auto")),
    ]),
    ("qwen3.8-flash", &[
        ("api_name", Text("qwen/qwen3.8-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Qwen3.8 Flash")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("tool_choice", Text("auto")),
    ]),
];

/// One value of the table as the object view.py's literal made.
fn value_object(py: Python<'_>, value: Value) -> PyResult<Obj<'_>> {
    Ok(match value {
        Text(text) => PyString::new(py, text).into_any(),
        Flag(flag) => PyBool::new(py, flag).to_owned().into_any(),
        Number(number) => number.into_pyobject(py)?.into_any(),
    })
}

/// A block's fields as a new dict, keys in the block's order.
fn fields_dict<'py>(py: Python<'py>, fields: &[(&str, Value)]) -> PyResult<Bound<'py, PyDict>> {
    let made = PyDict::new(py);
    for (key, value) in fields {
        made.set_item(PyString::intern(py, key), value_object(py, *value)?)?;
    }
    Ok(made)
}

/// MODEL_MAPPINGS as the dict view.py made: one dict per model, keys in the
/// table's order.
fn model_mappings(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let table = PyDict::new(py);
    for (model, fields) in MODEL_MAPPINGS {
        table.set_item(PyString::new(py, model), fields_dict(py, fields)?)?;
    }
    Ok(table)
}

// ---------------------------------------------------------------------------
// Python's statements (the shared ones in lib.rs's ios::controller, given
// this module)
// ---------------------------------------------------------------------------

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.openrouter.view", super::super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `value.get(key)`, the method looked up when it is called.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// A def's parameters bound to a call's arguments, as Python binds them:
/// the positional ones first, then the keywords; each slot is the argument
/// given, or None where the def's default applies. `selfs` is 1 for a
/// method, whose self Python counts in its messages. service.rs binds its
/// methods' arguments with it too.
pub(super) fn bind<'py>(
    function: &str,
    selfs: usize,
    args: &Bound<'py, PyTuple>,
    kwargs: Option<&Bound<'py, PyDict>>,
    params: &[&str],
    required: usize,
) -> PyResult<Vec<Option<Obj<'py>>>> {
    let most = params.len();
    let given = args.len();
    if given > most {
        let message = if required == most {
            format!(
                "{function}() takes {} positional argument{} but {} {} given",
                most + selfs,
                if most + selfs == 1 { "" } else { "s" },
                given + selfs,
                if given + selfs == 1 { "was" } else { "were" }
            )
        } else {
            format!(
                "{function}() takes from {} to {} positional arguments but {} were given",
                required + selfs,
                most + selfs,
                given + selfs
            )
        };
        return Err(PyTypeError::new_err(message));
    }
    let mut slots: Vec<Option<Obj<'py>>> = vec![None; most];
    for (slot, value) in slots.iter_mut().zip(args.iter()) {
        *slot = Some(value);
    }
    if let Some(kwargs) = kwargs {
        for (keyword, value) in kwargs.iter() {
            let keyword = keyword.str()?.to_string();
            match params.iter().position(|param| *param == keyword) {
                Some(at) if slots[at].is_some() => {
                    return Err(PyTypeError::new_err(format!(
                        "{function}() got multiple values for argument '{keyword}'"
                    )));
                }
                Some(at) => slots[at] = Some(value),
                None => {
                    return Err(PyTypeError::new_err(format!(
                        "{function}() got an unexpected keyword argument '{keyword}'"
                    )));
                }
            }
        }
    }
    let missing: Vec<String> = params[..required]
        .iter()
        .zip(&slots)
        .filter(|(_, slot)| slot.is_none())
        .map(|(param, _)| format!("'{param}'"))
        .collect();
    if !missing.is_empty() {
        let listed = match missing.len() {
            1 => missing[0].clone(),
            2 => format!("{} and {}", missing[0], missing[1]),
            n => format!("{}, and {}", missing[..n - 1].join(", "), missing[n - 1]),
        };
        return Err(PyTypeError::new_err(format!(
            "{function}() missing {} required positional argument{}: {listed}",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        )));
    }
    Ok(slots)
}

// ---------------------------------------------------------------------------
// The functions
// ---------------------------------------------------------------------------

#[pyfunction]
#[doc = "Get full model information from short name"]
fn get_model_info<'py>(short_name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = short_name.py();
    if g(py, "MODEL_MAPPINGS")?.contains(&short_name)? {
        return g(py, "MODEL_MAPPINGS")?.get_item(&short_name);
    }
    // If not found, assume it's already a full model name
    let info = PyDict::new(py);
    info.set_item(PyString::intern(py, "api_name"), &short_name)?;
    info.set_item(PyString::intern(py, "vision"), true)?;
    info.set_item(PyString::intern(py, "display_name"), &short_name)?;
    Ok(info.into_any())
}

/// `{"reasoning": {"effort": entry_value(<entry>, "effort", cli_agent)}}`:
/// entry_value read first, then called, then the dicts made.
fn effort_of<'py>(entry: &Obj<'py>, cli_agent: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = entry.py();
    let level = g(py, "entry_value")?.call1((entry, "effort", cli_agent))?;
    let effort = PyDict::new(py);
    effort.set_item(PyString::intern(py, "effort"), level)?;
    let made = PyDict::new(py);
    made.set_item(PyString::intern(py, "reasoning"), effort)?;
    Ok(made.into_any())
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(api_name, info=None, cli_agent=False)")]
#[doc = "OpenRouter's unified reasoning field for the model being called.\n\
\n\
The resolved table entry (`info`) wins when it carries an effort: two\n\
entries may share one api_name (one model at two levels), which a scan\n\
by api_name cannot tell apart. The api_name scan remains the fallback,\n\
matching the same helper in anthropic/view.py and openai/view.py.\n\
\n\
Returns {} for an entry with no level and for any hand-typed model name.\n\
That is the safe direction on this provider: the levels are per-model, so\n\
a guess is as likely to be rejected as honoured, and sending nothing\n\
leaves the model on the default OpenRouter already publishes for it.\n\
"]
fn get_reasoning_params<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("get_reasoning_params", 0, args, kwargs, &["api_name", "info", "cli_agent"], 1)?;
    let mut slots = slots.into_iter();
    let api_name = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let info = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let cli_agent = slots.next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    // The resolved entry wins: two entries may share one api_name (one
    // model at two levels), which a scan by api_name cannot tell apart.
    if info.is_truthy()? && get(&info, "effort")?.is_truthy()? {
        return effort_of(&info, &cli_agent);
    }
    for entry in g(py, "MODEL_MAPPINGS")?.call_method0("values")?.try_iter()? {
        let entry = entry?;
        if entry.get_item("api_name")?.rich_compare(&api_name, CompareOp::Eq)?.is_truthy()?
            && get(&entry, "effort")?.is_truthy()?
        {
            return effort_of(&entry, &cli_agent);
        }
    }
    Ok(PyDict::new(py).into_any())
}

// ---------------------------------------------------------------------------
// view.py's lines, run once the loader bound its import
// ---------------------------------------------------------------------------

/// `key = value` at the top of view.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of view.py after its import line, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds entry_value, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // The table itself is at the top of this file.
    store(m, "MODEL_MAPPINGS", model_mappings(py)?)?;

    store(m, "get_model_info", wrap_pyfunction!(get_model_info, m)?)?;
    store(m, "get_reasoning_params", wrap_pyfunction!(get_reasoning_params, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes view.py's place with _build, which the loader
/// calls once it has bound the import line's name (see the top of this
/// file). view.py had no docstring, so neither has the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
