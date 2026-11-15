//! view: the OpenAI models' table (MODEL_MAPPINGS) and the reasoning level
//! each model is sent (get_reasoning_effort).
//!
//! The Rust copy of llm_provider/openai/view.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.openai.view
//! (llm_provider/openai/__init__.py points that name here), and finds the
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

// Model mappings for OpenAI provider
// Maps user-friendly names to actual API model names

// GPT-6 Astra is a REASONING model, so `reasoning_effort` is the only real handle
// on how hard the model works. The sampling knobs are gone: temperature, top_p
// and n are locked at 1 and the penalties at 0 — sending any of them returns
// 400 "Unsupported parameter: '<name>' is not supported with this model" — and
// `seed` is not accepted either, so there is no reproducibility lever to reach
// for on this family. send_request therefore sends effort and nothing else.
//
// Levels, cheapest to deepest: none | low | medium | high | xhigh | max.
// Omitting the parameter already yields `medium`; every entry states its level
// anyway so the choice is readable here instead of inherited from the API.
//
// One ceiling to respect when raising a level: max_completion_tokens counts
// REASONING tokens as well as the visible answer and the tool-call JSON. Since
// the loop runs tool_choice="required", a turn that spends its whole budget
// thinking loses the tool call it was supposed to emit.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add an OpenAI model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false) and Number(16000). The app
// rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("gpt-6-astra", &[
        ("api_name", Text("gpt-6-astra")),
        // Batches only on the Responses endpoint with async tools; the
        // provider routes it there (see ../responses.py).
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6 Astra")),
        ("json_mode", Flag(true)),
        // Astra publishes low/medium/high/xhigh/max and rejects `none` and
        // `minimal`; one entry, at `medium`. /v1/chat/completions refuses
        // function tools on Astra at every level ("Function tools with
        // reasoning_effort are not supported ... use /v1/responses or set
        // reasoning_effort to 'none'", verified 2026-09-21), so with tools
        // the provider drives it on /v1/responses (`async_tools` above).
        ("reasoning_effort", Text("medium")),
        ("coder_reasoning_effort", Text("high")),
        ("coder_max_tokens", Number(16000)),
    ]),
    // GPT-6.1 Sol and GPT-6.1 Terra (added 2026-11-14): like Astra they
    // batch only on the Responses route with async tools. Same levels as
    // OpenRouter's GPT-6 Sol and GPT-6.1 Terra entries: `low`, and `medium`
    // for the coder.
    ("gpt-6.1-sol", &[
        ("api_name", Text("gpt-6.1-sol")),
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6.1 Sol")),
        ("json_mode", Flag(true)),
        ("reasoning_effort", Text("low")),
        ("coder_reasoning_effort", Text("medium")),
        ("coder_max_tokens", Number(16000)),
    ]),
    ("gpt-6.1-terra", &[
        ("api_name", Text("gpt-6.1-terra")),
        ("async_tools", Flag(true)),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-6.1 Terra")),
        ("json_mode", Flag(true)),
        ("reasoning_effort", Text("low")),
        ("coder_reasoning_effort", Text("medium")),
        ("coder_max_tokens", Number(16000)),
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
        .get_or_try_init(py, || py.import(format!("{}.openai.view", super::super::NATIVE)).map(Bound::unbind))
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
    info.set_item(PyString::intern(py, "json_mode"), true)?; // Default to supporting JSON mode for OpenAI
    Ok(info.into_any())
}

/// `{"reasoning_effort": entry_value(<entry>, "reasoning_effort", cli_agent)}`:
/// entry_value read first, then called, then the dict made.
fn effort_of<'py>(entry: &Obj<'py>, cli_agent: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = entry.py();
    let level = g(py, "entry_value")?.call1((entry, "reasoning_effort", cli_agent))?;
    let made = PyDict::new(py);
    made.set_item(PyString::intern(py, "reasoning_effort"), level)?;
    Ok(made.into_any())
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(api_name, info=None, cli_agent=False)")]
#[doc = "`reasoning_effort` kwarg for the model send_request is about to call.\n\
\n\
The resolved table entry (`info`) wins when it carries a level: two\n\
entries may share one api_name (one model at two levels), which a scan\n\
by api_name cannot tell apart. The api_name scan remains the fallback,\n\
matching anthropic/view.py's get_sampling_params.\n\
\n\
An unregistered model returns {} rather than a guess: a hand-typed name\n\
may be a non-reasoning model, which rejects `reasoning_effort` outright.\n\
Sending nothing leaves OpenAI's own default in place.\n"]
fn get_reasoning_effort<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("get_reasoning_effort", 0, args, kwargs, &["api_name", "info", "cli_agent"], 1)?;
    let mut slots = slots.into_iter();
    let api_name = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let info = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let cli_agent = slots.next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    // The resolved entry wins: two entries may share one api_name (one
    // model at two levels), which a scan by api_name cannot tell apart.
    if info.is_truthy()? && get(&info, "reasoning_effort")?.is_truthy()? {
        return effort_of(&info, &cli_agent);
    }
    for entry in g(py, "MODEL_MAPPINGS")?.call_method0("values")?.try_iter()? {
        let entry = entry?;
        if entry.get_item("api_name")?.rich_compare(&api_name, CompareOp::Eq)?.is_truthy()?
            && get(&entry, "reasoning_effort")?.is_truthy()?
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
    store(m, "get_reasoning_effort", wrap_pyfunction!(get_reasoning_effort, m)?)?;
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
