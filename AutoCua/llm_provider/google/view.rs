//! view: the Google Gemini models' table (MODEL_MAPPINGS) and the thinking
//! level each model is called with.
//!
//! The Rust copy of llm_provider/google/view.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.google.view
//! (llm_provider/google/__init__.py points that name here), and finds the
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

// Model mappings for Google provider
// Maps user-friendly names to actual API model names

// `thinking_level` is Gemini 3's thinking control — a named level, not a token
// budget (thinking_budget is the older field and is not what these models
// want). The SDK carries it as types.ThinkingConfig(thinking_level=...), built
// in service.py so this table stays plain data; the value is coerced
// case-insensitively, so "low" here is the same as the enum's LOW.
//
// Every model is pinned to `low`, but it means a different thing on each,
// because their ladders and defaults differ:
//   gemini-4-argon          `low` accepted (probed 2026-11-04)
//   gemini-4-carbon         low/medium/high (no `minimal` - it errors,
//   gemini-4-hydrogen       probed 2026-11-14) -> floor
//   gemini-3.6-flash        minimal/low/medium/high  default medium  -> one
//                                                                      above
//                                                                      floor
//   gemini-3.8-flash        low/medium/high (no `minimal` - it errors) -> floor
// Gemini 4 Argon replaced Gemini 3.1 Pro Preview on 2026-11-04, here and in
// the openrouter and perplexity tables: probed with the real driver, coder and
// minion prompts, `low` batches the notes call with the actions.
//
// The `-vertex` entries are the SAME models reached through a different
// client — Vertex AI keeps the bare model ID rather than renaming it — so
// api_name is intentionally duplicated across each pair. Only `vertex`
// differs, which is what picks the client in service.py.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Gemini model, copy a block and change its
// values: Text("...") for a str, Flag(true) / Flag(false) for a bool. For
// the same model through Vertex AI, copy its block again with the short name
// ending in `-vertex`, the same api_name and Flag(true) for vertex. The app
// rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("gemini-4-argon", &[
        ("api_name", Text("gemini-4-argon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Argon")),
        ("vertex", Flag(false)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-4-carbon", &[
        ("api_name", Text("gemini-4-carbon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Carbon")),
        ("vertex", Flag(false)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-4-hydrogen", &[
        ("api_name", Text("gemini-4-hydrogen")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Hydrogen")),
        ("vertex", Flag(false)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-3.6-flash", &[
        ("api_name", Text("gemini-3.6-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.6 Flash")),
        ("vertex", Flag(false)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-3.8-flash", &[
        ("api_name", Text("gemini-3.8-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.8 Flash")),
        ("vertex", Flag(false)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-4-argon-vertex", &[
        ("api_name", Text("gemini-4-argon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Argon (Vertex)")),
        ("vertex", Flag(true)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-4-carbon-vertex", &[
        ("api_name", Text("gemini-4-carbon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Carbon (Vertex)")),
        ("vertex", Flag(true)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-4-hydrogen-vertex", &[
        ("api_name", Text("gemini-4-hydrogen")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Hydrogen (Vertex)")),
        ("vertex", Flag(true)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-3.6-flash-vertex", &[
        ("api_name", Text("gemini-3.6-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.6 Flash (Vertex)")),
        ("vertex", Flag(true)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
    ("gemini-3.8-flash-vertex", &[
        ("api_name", Text("gemini-3.8-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.8 Flash (Vertex)")),
        ("vertex", Flag(true)),
        ("thinking_level", Text("low")),
        ("coder_thinking_level", Text("medium")),
    ]),
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
        .get_or_try_init(py, || py.import(format!("{}.google.view", super::super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// A def's parameters bound to a call's arguments, as Python binds them:
/// the positional ones first, then the keywords; each slot is the argument
/// given, or None where the def's default applies. `selfs` is 1 for a
/// method, whose self Python counts in its messages. service.rs binds its
/// functions' and methods' arguments with it too.
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

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(api_name, cli_agent=False)")]
#[doc = "`thinking_level` for the model send_request is about to call, or None.\n\
\n\
Keyed by api_name, matching the same helper in the other providers.\n\
\n\
NOTE the difference from those providers: api_name is NOT unique here.\n\
Each model appears twice, once as itself and once as its `-vertex` twin,\n\
since Vertex AI serves the same model ID through a different client. That\n\
is safe only while both halves of a pair carry the same level, which they\n\
do \u{2014} first match wins. If a pair ever needs to differ between AI Studio\n\
and Vertex, this has to key on the short name instead.\n\
\n\
None for an unregistered model, so a hand-typed name keeps Google's own\n\
default rather than being handed a level its ladder may not include \u{2014}\n\
`minimal`, for instance, does not exist on Pro.\n"]
fn get_thinking_level<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("get_thinking_level", 0, args, kwargs, &["api_name", "cli_agent"], 1)?;
    let mut slots = slots.into_iter();
    let api_name = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let cli_agent = slots.next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    // for info in MODEL_MAPPINGS.values():
    for info in g(py, "MODEL_MAPPINGS")?.call_method0("values")?.try_iter()? {
        let info = info?;
        // if info["api_name"] == api_name and info.get("thinking_level"):
        if info.get_item("api_name")?.rich_compare(&api_name, CompareOp::Eq)?.is_truthy()?
            && info.call_method1("get", ("thinking_level",))?.is_truthy()?
        {
            return g(py, "entry_value")?.call1((&info, "thinking_level", &cli_agent));
        }
    }
    Ok(py.None().into_bound(py))
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
    store(m, "get_thinking_level", wrap_pyfunction!(get_thinking_level, m)?)?;
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
