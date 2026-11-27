//! view: the Perplexity models' table (MODEL_MAPPINGS) and the reasoning
//! config each model is sent (get_reasoning_params).
//!
//! The Rust copy of llm_provider/perplexity/view.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.perplexity.view
//! (llm_provider/perplexity/__init__.py points that name here), and finds the
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

/// One value in a model's entry: a str (Text) or a bool (Flag).
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
}

use Value::{Flag, Text};

// Model mappings for Perplexity provider
// Maps user-friendly names to actual Perplexity Agent API model names

// Perplexity fronts other vendors' models, so api_name here is a PERPLEXITY
// route, not the vendor's own ID, and the spelling differs from the same model
// on other providers. Two traps live in this table:
//
//   xAI       Perplexity says `xai/`, OpenRouter says `x-ai/`
//   Moonshot  Perplexity says `perplexity/kimi-k3` — its OWN namespace, not
//             `moonshotai/` as on OpenRouter
//
// So a slug copied across provider maps will 404 even though it names the same
// model. Short names are kept identical to the other maps on purpose — that is
// what makes a model switchable between providers — but api_name never is.
//
// `effort` is carried as the Agent API's reasoning config,
// {"reasoning": {"effort": ...}} — the same shape OpenRouter uses. The endpoint
// accepts minimal/low/medium/high/xhigh/max, but that is the API's range, NOT
// any single model's: `minimal` does not exist on the Grok or Kimi routes,
// `xhigh` does not exist on the Gemini, Grok or Kimi ones, and `max` not on
// the Gemini or Grok ones.
// Every value below was checked against the underlying model's own published
// ladder.
//
// Not probed live: whether Perplexity clamps an out-of-range level to the
// nearest supported one (as OpenRouter does) or rejects it outright. That is
// the reason to keep these conservative: every level here exists on its own
// model either way.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Perplexity model, copy a block and change its
// values: Text("...") for a str, Flag(true) / Flag(false) for a bool. The api
// name is Perplexity's own route (see the traps above). The app rebuilds this
// file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    // No OpenAI entry. GPT-6 batches only through the `async` tool flag, and
    // Perplexity strips it (probed 2026-10-01), so here GPT-6 makes one call
    // per turn. Use the openrouter or openai provider for GPT-6.
    // Google. Gemini 4 Argon replaces Gemini 3.1 Pro Preview here, as on
    // OpenRouter. Probed 2026-11-04 with the real driver, coder and minion
    // prompts: `low` batches the notes call with the actions.
    ("gemini-4-argon", &[
        ("api_name", Text("google/gemini-4-argon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Argon")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
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
    ("gemini-4-neon", &[
        ("api_name", Text("google/gemini-4-neon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Neon")),
        ("effort", Text("xhigh")),
        ("coder_effort", Text("high")),
    ]),
    ("gemini-3.6-flash", &[
        ("api_name", Text("google/gemini-3.6-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.6 Flash")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("gemini-3.8-flash", &[
        ("api_name", Text("google/gemini-3.8-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.8 Flash")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // xAI — note the `xai/` prefix, NOT `x-ai/` as on OpenRouter.
    ("grok-4.5", &[
        ("api_name", Text("xai/grok-4.5")),
        ("vision", Flag(true)),
        ("display_name", Text("Grok 4.5")),
        // Grok 4.5 cannot switch reasoning off at all, so `low` is its floor.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // No Grok 4.3 or 4.7: on this route both answer 400 "invalid request" to
    // any request that carries a screenshot, and the same request without it
    // goes through (probed 2026-11-04), so they cannot drive a screen.
    // Moonshot — routed under Perplexity's own namespace, not `moonshotai/`.
    ("kimi-k3", &[
        ("api_name", Text("perplexity/kimi-k3")),
        ("vision", Flag(true)),
        ("display_name", Text("Kimi K3")),
        // K3 defaults to `max`, the most expensive default of anything in
        // this table. Perplexity takes every level for it, `medium` (the
        // coder's) included.
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // Z.ai GLM: also under Perplexity's own namespace, like Kimi. Only the
    // Flash model: plain glm-5.3 rejects any request with an image here.
    ("glm-5.3-flash", &[
        ("api_name", Text("perplexity/glm-5.3-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("GLM 5.3 Flash")),
        ("effort", Text("low")),
        // GLM's own ladder is low / high / max, no `medium`: the coder goes
        // up to `high`.
        ("coder_effort", Text("high")),
    ]),
    // No `sonar` entry. Perplexity's own search-grounded model is text-only,
    // which makes it unusable for the driver's screenshot step — and while it
    // was registered it was also the secondary fallback, so a failed vision
    // call could land on a model that cannot see. Every entry above takes
    // images, so that whole failure mode is gone.
];

/// One value of the table as the object view.py's literal made.
fn value_object(py: Python<'_>, value: Value) -> Obj<'_> {
    match value {
        Text(text) => PyString::new(py, text).into_any(),
        Flag(flag) => PyBool::new(py, flag).to_owned().into_any(),
    }
}

/// A block's fields as a new dict, keys in the block's order.
fn fields_dict<'py>(py: Python<'py>, fields: &[(&str, Value)]) -> PyResult<Bound<'py, PyDict>> {
    let made = PyDict::new(py);
    for (key, value) in fields {
        made.set_item(PyString::intern(py, key), value_object(py, *value))?;
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
        .get_or_try_init(py, || py.import(format!("{}.perplexity.view", super::super::NATIVE)).map(Bound::unbind))
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
#[doc = "Agent API reasoning config for the model send_request is about to call.\n\
\n\
The resolved table entry (`info`) wins when it carries an effort: two\n\
entries may share one api_name (one model at two levels), which a scan\n\
by api_name cannot tell apart. The api_name scan remains the fallback,\n\
matching the same helper in the other provider views.\n\
\n\
Returns {} for a hand-typed model name, which then keeps whatever default\n\
Perplexity applies. That is the safe direction: the levels are per-model,\n\
so a guess is as likely to be rejected as honoured.\n\
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
