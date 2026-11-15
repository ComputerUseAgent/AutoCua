//! view: the Claude models' table (MODEL_MAPPINGS) and the request fields
//! each model takes: the sampling knobs, adaptive thinking with its effort,
//! and tool_choice.
//!
//! The Rust copy of llm_provider/anthropic/view.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.anthropic.view
//! (llm_provider/anthropic/__init__.py points that name here), and finds the
//! same names in the same order, with the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what view.py's
//! import line bound (entry_value) and calls _build(), which runs the rest of
//! view.py's lines in their order: the three tables, then each function where
//! its def was.
//!
//! Every global a line reads (MODEL_MAPPINGS, SAMPLING_PARAMS,
//! ADAPTIVE_THINKING, entry_value, dict) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces or
//! changes one is obeyed.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyIterator, PyString, PyTuple};

use crate::ios::controller;

type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One value in a model's entry: a str (Text), a bool (Flag) or a whole
/// number (Number, for a field like coder_max_tokens; no Claude entry uses
/// one yet).
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
    #[allow(dead_code)]
    Number(i64),
}

#[allow(unused_imports)] // Number: no entry uses it yet
use Value::{Flag, Number, Text};

// Model mappings for Anthropic provider
// Maps user-friendly names to actual API model names
//
// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Claude model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false), Number(16000). The app
// rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    // Haiku takes neither adaptive thinking nor `effort` — it predates both,
    // and each is a 400 here. It is the one model left that still accepts the
    // sampling knobs (SAMPLING_PARAMS, further down), which is why the two
    // features are mutually exclusive across this table.
    ("claude-haiku-4.5", &[
        ("api_name", Text("claude-haiku-4-5-20251001")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Haiku 4.5")),
        ("sampling", Flag(true)),
    ]),
    ("claude-opus-5", &[
        ("api_name", Text("claude-opus-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5")),
        // Opus 5 dropped all three sampling knobs — temperature, top_p and
        // top_k each return 400 "deprecated for this model". Steer it by
        // prompt and by effort.
        ("sampling", Flag(false)),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_tool_choice", Text("auto")),
    ]),
    ("claude-sonnet-5", &[
        ("api_name", Text("claude-sonnet-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5")),
        // Same as Opus 5: non-default temperature/top_p/top_k are rejected.
        ("sampling", Flag(false)),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_tool_choice", Text("auto")),
    ]),
    ("claude-sonnet-5.5", &[
        ("api_name", Text("claude-sonnet-5-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5.5")),
        ("sampling", Flag(false)),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        // Sonnet 5.5 dropped forced tool use: tool_choice "any" (and "tool")
        // is a 400 "not supported for this model", so this entry is sent
        // "auto" instead. The loop's no-tool-called nudge covers a turn that
        // comes back without a call.
        ("tool_choice", Text("auto")),
    ]),
    ("claude-opus-5.5", &[
        ("api_name", Text("claude-opus-5-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5.5")),
        ("sampling", Flag(false)),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        // Same as Sonnet 5.5: forced tool_choice is a 400, so "auto".
        ("tool_choice", Text("auto")),
    ]),
];

/// MODEL_MAPPINGS as the dict view.py made: one dict per model, keys in the
/// table's order.
fn model_mappings(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let table = PyDict::new(py);
    for (model, fields) in MODEL_MAPPINGS {
        let entry = PyDict::new(py);
        for (key, value) in *fields {
            let key = PyString::intern(py, key);
            match value {
                Text(text) => entry.set_item(key, PyString::new(py, text))?,
                Flag(flag) => entry.set_item(key, PyBool::new(py, *flag))?,
                Number(number) => entry.set_item(key, *number)?,
            }
        }
        table.set_item(PyString::new(py, model), entry)?;
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
        .get_or_try_init(py, || py.import(format!("{}.anthropic.view", super::super::NATIVE)).map(Bound::unbind))
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

/// `for info in MODEL_MAPPINGS.values():`, the global read now.
fn entries(py: Python<'_>) -> PyResult<Bound<'_, PyIterator>> {
    g(py, "MODEL_MAPPINGS")?.call_method0("values")?.try_iter()
}

/// `info["api_name"] == api_name and info.get(<key>)`, as an `if` takes it.
fn matches(info: &Obj<'_>, api_name: &Obj<'_>, key: &str) -> PyResult<bool> {
    Ok(info.get_item("api_name")?.rich_compare(api_name, CompareOp::Eq)?.is_truthy()? && get(info, key)?.is_truthy()?)
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

#[pyfunction]
#[doc = "Sampling knobs for the model send_request is about to call.\n\
\n\
Keyed by api_name \u{2014} what the service actually receives \u{2014} NOT the short\n\
name; the two differ for the Haiku entry above. Anything not marked\n\
sampling-capable returns {}, so neither Opus 5 / Sonnet 5 nor a\n\
hand-typed model name can be handed a parameter it would reject.\n"]
fn get_sampling_params<'py>(api_name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = api_name.py();
    for info in entries(py)? {
        if matches(&info?, &api_name, "sampling")? {
            return g(py, "dict")?.call1((g(py, "SAMPLING_PARAMS")?,));
        }
    }
    Ok(PyDict::new(py).into_any())
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(api_name, cli_agent=False)")]
#[doc = "Adaptive-thinking + effort request fields for the model being called.\n\
\n\
Keyed by api_name for the same reason as get_sampling_params: it is what\n\
send_request receives, and what the emergency fallback swaps in mid-call,\n\
so the level follows the model actually on the wire.\n\
\n\
Returns {} for anything without an `effort` entry \u{2014} Haiku, and any\n\
hand-typed name. That is the safe direction: a model that predates\n\
adaptive thinking rejects both fields, so sending nothing leaves it on\n\
its own default instead of 400-ing every attempt of every step.\n"]
fn get_thinking_params<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("get_thinking_params", 0, args, kwargs, &["api_name", "cli_agent"], 1)?;
    let mut slots = slots.into_iter();
    let api_name = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let cli_agent = slots.next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    for info in entries(py)? {
        let info = info?;
        if matches(&info, &api_name, "effort")? {
            let thinking = g(py, "dict")?.call1((g(py, "ADAPTIVE_THINKING")?,))?;
            let effort = g(py, "entry_value")?.call1((&info, "effort", &cli_agent))?;
            let config = PyDict::new(py);
            config.set_item(PyString::intern(py, "effort"), effort)?;
            let fields = PyDict::new(py);
            fields.set_item(PyString::intern(py, "thinking"), thinking)?;
            fields.set_item(PyString::intern(py, "output_config"), config)?;
            return Ok(fields.into_any());
        }
    }
    Ok(PyDict::new(py).into_any())
}

#[pyfunction]
#[doc = "tool_choice for the model being called: {\"type\": \"any\"} (every turn\n\
must call a tool) unless its entry sets `tool_choice`.\n\
\n\
Keyed by api_name like the two helpers above, so the coder's one-call\n\
fallback to Haiku 4.5 keeps \"any\" while Sonnet 5.5 gets \"auto\". The\n\
coder on Opus 5 or Sonnet 5 (also as its fallback) never gets here:\n\
their coder_tool_choice is \"auto\".\n"]
fn get_tool_choice<'py>(api_name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = api_name.py();
    let choice = PyDict::new(py);
    for info in entries(py)? {
        let info = info?;
        if matches(&info, &api_name, "tool_choice")? {
            choice.set_item(PyString::intern(py, "type"), info.get_item("tool_choice")?)?;
            return Ok(choice.into_any());
        }
    }
    choice.set_item(PyString::intern(py, "type"), PyString::intern(py, "any"))?;
    Ok(choice.into_any())
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
    // Sampling knobs for the models that still accept them — only Haiku now.
    // `temperature` is never sent: the frontier models removed it (400
    // "`temperature` is deprecated for this model"), and one knob steers better
    // than two anyway. top_p does the real work — it holds generation to the
    // confident mass, which is what keeps tool-call arguments and schema JSON
    // well-formed. top_k is only a tail guard: it bites when the distribution is
    // unusually flat and otherwise never binds.
    let sampling = PyDict::new(py);
    sampling.set_item(PyString::intern(py, "top_p"), 0.5)?;
    sampling.set_item(PyString::intern(py, "top_k"), 40)?;
    store(m, "SAMPLING_PARAMS", sampling)?;

    // Thinking, for the models that take it. ADAPTIVE means Claude decides per
    // request whether to think and for how long, rather than being handed a fixed
    // token budget; it is the only mode the 5-series accepts, and the old
    // budget form (thinking={"type": "enabled", "budget_tokens": N}) is a 400
    // there. `effort` is the dial on top: how deep that thinking may go and how
    // much the model spends overall.
    //
    // Levels, cheapest to deepest: low | medium | high | xhigh | max. Opus 5 and
    // Sonnet 5 both accept all five and both DEFAULT TO high, so omitting effort
    // is not neutral — it is the second-most-expensive setting. Each entry states
    // its level for that reason.
    //
    // `low` also keeps this loop honest about max_tokens: thinking tokens are
    // billed against the same 4000-token ceiling as the visible answer and the
    // tool-call JSON, and the loop runs tool_choice {"type": "any"}, so a turn
    // that spends its budget thinking loses the tool call it owed.
    //
    // The coder (cli_agent) reads each entry's coder_ fields: coder_effort (one
    // level deeper), and coder_tool_choice auto where the table would force a
    // call, since a forced tool call returns no thinking block. Its output
    // ceiling is out_cap's 32768.
    let adaptive = PyDict::new(py);
    adaptive.set_item(PyString::intern(py, "type"), PyString::intern(py, "adaptive"))?;
    store(m, "ADAPTIVE_THINKING", adaptive)?;

    // The table itself is MODEL_MAPPINGS at the top of this file.
    store(m, "MODEL_MAPPINGS", model_mappings(py)?)?;

    store(m, "get_model_info", wrap_pyfunction!(get_model_info, m)?)?;
    store(m, "get_sampling_params", wrap_pyfunction!(get_sampling_params, m)?)?;
    store(m, "get_thinking_params", wrap_pyfunction!(get_thinking_params, m)?)?;
    store(m, "get_tool_choice", wrap_pyfunction!(get_tool_choice, m)?)?;
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
