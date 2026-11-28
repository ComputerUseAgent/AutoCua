//! view: the H Company models' table (MODEL_MAPPINGS) and the lookup that
//! gives a model's entry by its short name.
//!
//! Python imports it as AutoCua.llm_provider.hcompany.view
//! (llm_provider/hcompany/__init__.py points that name here). It has the
//! shape of the other providers' views (llm_provider/groq/view.rs), which are
//! the Rust copies of a view.py: this provider never had one.
//!
//! Two steps, as theirs: register() gives the module _build, then the loader
//! calls _build(), which makes the table and then the function.
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

// Model mappings for the H Company provider
// Maps the names passed as model= to the ids its API takes

// H Company's Models API (https://api.hcompany.ai/v1, OpenAI-compatible chat
// completions) serves the Holo models, vision-language models made for GUI
// agents: image+text in, text out. The loop needs tool use AND vision in one
// model, because a driver step hands the model a screenshot: listed here are
// the three with both. The fourth it serves, holo3-122b-a10b, takes no tools
// (GET /v1/models: its supported_features has no "tools", and the docs say
// tools sent to it are a 400), so no agent can run on it and it is left out.
//
// What each is, from H Company's own table (hub.hcompany.ai/models-api/overview)
// and GET /v1/models, read 2026-11-27:
//   holo4-27b        paid tier. "Start here. Most capable." Context 262,144.
//   holo4-35b-a3b    paid tier. "Fastest." Context 262,144.
//   holo3-1-35b-a3b  free tier too (10 requests a minute there, and the only
//                    model a free account may call: the others answer 402
//                    insufficient_credit until credits are added). Context
//                    65,537, which is BELOW the 110,000 tokens at which the
//                    agents compress their memory
//                    (memory_compression/controller.rs): a long run on it
//                    ends at the API's context limit before a compression.
//
// max_output is the most the API lets the model write in one answer (its
// max_output_length in GET /v1/models; the docs say a request for more is a
// 400). service.rs never asks for more, whatever the caller's cap is.
//
// Thinking is on for every model, as the API has it. holo4-27b is asked for
// its top level, xhigh, by its entry's effort (the owner, 2026-11-28: "switch
// to xhigh"). The docs say that is also what the API takes when no level is
// sent, and 6 answers each way on 2026-11-28 thought about the same amount (58
// and 43 tokens on average). The other two have no effort. An entry may set
// one, and the coder its own (coder_effort), as on the other providers:
// ("effort", Text("none")) turns thinking off on every model; holo4-27b also
// takes "low" and "medium" (and answers 400 to "high"); the others ignore a
// level.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add an H Company model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false) and Number(8192). The app
// rebuilds this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("holo4-27b", &[
        ("api_name", Text("holo4-27b")),
        ("vision", Flag(true)),
        ("display_name", Text("Holo4 27B")),
        ("max_output", Number(16384)),
        ("effort", Text("xhigh")),
    ]),
    ("holo4-35b-a3b", &[
        ("api_name", Text("holo4-35b-a3b")),
        ("vision", Flag(true)),
        ("display_name", Text("Holo4 35B A3B")),
        ("max_output", Number(16384)),
    ]),
    ("holo3-1-35b-a3b", &[
        ("api_name", Text("holo3-1-35b-a3b")),
        ("vision", Flag(true)),
        ("display_name", Text("Holo3.1 35B A3B")),
        ("max_output", Number(8192)),
    ]),
];

/// One value of the table as the Python object it stands for.
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

/// MODEL_MAPPINGS as a dict: one dict per model (each entry its own), keys
/// in the table's order.
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
        .get_or_try_init(py, || py.import(format!("{}.hcompany.view", super::super::NATIVE)).map(Bound::unbind))
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
    // If not found, assume it's already a full model name. It has no
    // max_output: service.rs then keeps to the smallest one H Company serves.
    let info = PyDict::new(py);
    info.set_item(PyString::intern(py, "api_name"), &short_name)?;
    info.set_item(PyString::intern(py, "vision"), true)?;
    info.set_item(PyString::intern(py, "display_name"), &short_name)?;
    Ok(info.into_any())
}

// ---------------------------------------------------------------------------
// What the loader runs, once
// ---------------------------------------------------------------------------

/// `key = value` on the module: its own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Makes the table, then the function, and binds each on the module. The
/// loader calls it once and then deletes it.
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

/// Fills the module with _build, which the loader calls (see the top of this
/// file). Like the other providers' views, the module has no docstring.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
