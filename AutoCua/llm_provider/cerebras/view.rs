//! view: the Cerebras models' table (MODEL_MAPPINGS) and get_model_info,
//! the entry a model name stands for.
//!
//! The Rust copy of llm_provider/cerebras/view.py. Python imports it under
//! its old dotted name, AutoCua.llm_provider.cerebras.view
//! (llm_provider/cerebras/__init__.py points that name here), and finds the
//! same names in the same order, with the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader calls _build(), which
//! runs view.py's lines in their order (view.py imported nothing): the
//! table, then get_model_info where its def was.
//!
//! Every global a line reads (MODEL_MAPPINGS) is read from this module at
//! that moment, as Python reads a global, so a caller or a test that
//! replaces or changes it is obeyed.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyString};

use crate::ios::controller;

type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One value in a model's entry: a str (Text), a bool (Flag), a whole number
/// (Number) or a small dict of its own (Map), whose values are these kinds
/// again. Today's table only uses Text and Flag; the other two are here for
/// a model that needs one.
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
    Number(i64),
    Map(&'static [(&'static str, Value)]),
}

use Value::{Flag, Map, Number, Text};

// Model mappings for Cerebras provider
// Maps user-friendly names to actual API model names

// One model: Qwen 3.8 27B, image+text in / text out with native (OpenAI-format)
// tool calling — the two things this loop needs on every step.
//
// Sampling and reasoning are set once in service.py, not per model:
// temperature 1, top_p 0.95, seed 42, max_completion_tokens 32768, and
// reasoning_effort "high" for the coder and its minions, "low" for every other
// agent.
// effort: the reasoning level every agent sends; coder_effort: the coder's.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Cerebras model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false), Number(10000), and for a
// small dict of its own Map(&[("type", Text("adaptive"))]). The app rebuilds
// this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("qwen-3.8-27b", &[
        ("api_name", Text("qwen-3.8-27b")),
        ("vision", Flag(true)),
        ("display_name", Text("Qwen 3.8 27B")),
        ("effort", Text("low")),
        ("coder_effort", Text("high")),
    ]),
];

/// One value of the table as the object view.py's literal made: a Map is a
/// new dict every time, so each entry has its own.
fn value_object(py: Python<'_>, value: Value) -> PyResult<Obj<'_>> {
    Ok(match value {
        Text(text) => PyString::new(py, text).into_any(),
        Flag(flag) => PyBool::new(py, flag).to_owned().into_any(),
        Number(number) => number.into_pyobject(py)?.into_any(),
        Map(fields) => fields_dict(py, fields)?.into_any(),
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
static MODULE: pyo3::sync::PyOnceLock<Py<PyModule>> = pyo3::sync::PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.cerebras.view", super::super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
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
    // If not found, assume it's already a full model name, with the same
    // levels as the entry above (the provider always sends one).
    let info = PyDict::new(py);
    info.set_item(PyString::intern(py, "api_name"), &short_name)?;
    info.set_item(PyString::intern(py, "vision"), true)?;
    info.set_item(PyString::intern(py, "display_name"), &short_name)?;
    info.set_item(PyString::intern(py, "effort"), PyString::intern(py, "low"))?;
    info.set_item(PyString::intern(py, "coder_effort"), PyString::intern(py, "high"))?;
    Ok(info.into_any())
}

// ---------------------------------------------------------------------------
// view.py's lines, run by the loader
// ---------------------------------------------------------------------------

/// `key = value` at the top of view.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of view.py in their order and binds what each one makes
/// on the module. The loader calls it once and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // The table itself is at the top of this file.
    store(m, "MODEL_MAPPINGS", model_mappings(py)?)?;

    store(m, "get_model_info", wrap_pyfunction!(get_model_info, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes view.py's place with _build, which the loader
/// calls (see the top of this file). view.py had no docstring, so neither
/// has the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
