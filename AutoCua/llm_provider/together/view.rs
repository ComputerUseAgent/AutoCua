//! view: the Together AI models' table (MODEL_MAPPINGS) and the lookup that
//! gives a model's entry by its short name.
//!
//! The Rust copy of llm_provider/together/view.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.together.view
//! (llm_provider/together/__init__.py points that name here), and finds the
//! same names in the same order, with the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader calls _build() (view.py
//! imported nothing), which runs view.py's lines in their order: the table,
//! then the function where its def was.
//!
//! Every global a line reads (MODEL_MAPPINGS) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces or
//! changes one is obeyed.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyString};

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

// Model mappings for Together AI provider
// Maps user-friendly names to actual API model names

// Three models, all image+text in / text out, all with native (OpenAI-format)
// tool calling — the two things this loop needs on every step.
//
// Reasoning is deliberately left unset. Together exposes `reasoning_effort`
// (low|medium|high) — Inkling's "controllable inference effort" knob — but
// with no field on the request each model keeps Together's own default, the
// same choice groq/ makes. Temperature is handled once in service.py (0.2).

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Together model, copy a block and change its
// values: Text("...") for a str, Flag(true) / Flag(false) for a bool,
// Number(10000) for a whole number. The api name is Together's own model id.
// The app rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("inkling", &[
        ("api_name", Text("thinkingmachines/Inkling")),
        ("vision", Flag(true)),
        ("display_name", Text("Inkling")),
    ]),
    ("muse-glimmer-30b", &[
        ("api_name", Text("meta-models/Muse-Glimmer-30B")),
        ("vision", Flag(true)),
        ("display_name", Text("Muse Glimmer 30B")),
        // 131,072-token context, and Together rejects input + max_tokens
        // over it. The coder compresses its memory at 110,000, so its cap
        // stays at 10,000 here (32,768 would fail every step past 98,304).
        ("coder_max_tokens", Number(10000)),
    ]),
    ("minimax-m3", &[
        ("api_name", Text("MiniMaxAI/MiniMax-M3")),
        ("vision", Flag(true)),
        ("display_name", Text("MiniMax M3")),
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

/// MODEL_MAPPINGS as the dict view.py made: one dict per model (each entry
/// its own), keys in the table's order.
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
        .get_or_try_init(py, || py.import(format!("{}.together.view", super::super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

// ---------------------------------------------------------------------------
// The function
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

// ---------------------------------------------------------------------------
// view.py's lines, run once by the loader
// ---------------------------------------------------------------------------

/// `key = value` at the top of view.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of view.py in their order, and binds what each one makes
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
/// calls (see the top of this file). view.py had no docstring, so neither has
/// the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
