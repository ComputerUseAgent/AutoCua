//! view: the Requesty models' table (MODEL_MAPPINGS) and the lookup that
//! gives a model's entry by its short name.
//!
//! The Rust copy of llm_provider/requesty/view.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.requesty.view
//! (llm_provider/requesty/__init__.py points that name here), and finds the
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

/// One value in a model's entry: a str (Text) or a bool (Flag).
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
}

use Value::{Flag, Text};

// Model mappings for Requesty provider
// Maps user-friendly names to actual API model names

// Requesty is an OpenAI-compatible router: one key, `vendor/model` ids, the
// same chat/completions shape for every backend. All entries below are
// image+text in / text out with native (OpenAI-format) tool calling, checked
// against https://router.requesty.ai/v1/models. Any other id from that list
// can be typed as the model name and is sent verbatim.
//
// `effort` goes out as Requesty's `reasoning_effort`, which it maps onto each
// backend (an effort for OpenAI, a thinking budget for Anthropic and Vertex).
// An entry without it keeps the model's own default. `tool_choice`, where set,
// replaces "required" for that model only, as in openrouter/view.py.
// Temperature (0.2) and prompt caching (requesty.auto_cache) are set once in
// service.py.
//
// Probed 2026-11-04 with the real driver, coder and minion prompts, a step and
// its tool results sent back, and left out:
// - openai/gpt-6.1-terra, gpt-6.1-sol and gpt-6-luna call the notes tool alone
//   turn after turn here, on chat/completions and on /v1/responses alike:
//   Requesty drops the async tool flags that make them batch on OpenRouter.
// - moonshot/kimi-k3.1-pro (temperature 1 only) and meta/muse-spark-1.3
//   (tool_choice "auto" only) mostly called the notes tool alone, the coder
//   at times for three turns running.
// - muse-spark-open (fireworks/, together/) never acted, or timed out.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Requesty model, copy a block and change its
// values: Text("...") for a str, Flag(true) / Flag(false) for a bool. The
// api name is Requesty's `vendor/model` id. The app rebuilds this file by
// itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("claude-sonnet-5", &[
        ("api_name", Text("anthropic/claude-sonnet-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_tool_choice", Text("auto")),
    ]),
    // Claude 5.5 rejects forced tool use (see openrouter/view.py), so both
    // carry tool_choice "auto".
    ("claude-sonnet-5.5", &[
        ("api_name", Text("anthropic/claude-sonnet-5-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 5.5")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("tool_choice", Text("auto")),
    ]),
    ("claude-opus-5", &[
        ("api_name", Text("anthropic/claude-opus-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5")),
        ("effort", Text("medium")),
        ("coder_effort", Text("high")),
        ("coder_tool_choice", Text("auto")),
    ]),
    ("claude-opus-5.5", &[
        ("api_name", Text("anthropic/claude-opus-5-5")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 5.5")),
        ("effort", Text("medium")),
        ("coder_effort", Text("high")),
        ("tool_choice", Text("auto")),
    ]),
    ("gemini-3.6-flash", &[
        ("api_name", Text("google/gemini-3.6-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 3.6 Flash")),
    ]),
    // Requesty serves Argon through Vertex only.
    ("gemini-4-argon", &[
        ("api_name", Text("vertex/gemini-4-argon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Argon")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // Carbon only on Vertex here too; Hydrogen also has google/, but vertex/
    // keeps the three alike.
    ("gemini-4-carbon", &[
        ("api_name", Text("vertex/gemini-4-carbon")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Carbon")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("gemini-4-hydrogen", &[
        ("api_name", Text("vertex/gemini-4-hydrogen")),
        ("vision", Flag(true)),
        ("display_name", Text("Gemini 4 Hydrogen")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    ("gpt-5.4", &[
        ("api_name", Text("openai/gpt-5.4")),
        ("vision", Flag(true)),
        ("display_name", Text("GPT-5.4")),
    ]),
    ("grok-4.7", &[
        ("api_name", Text("xai/grok-4.7")),
        ("vision", Flag(true)),
        ("display_name", Text("Grok 4.7")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // Particle AI's own model. It thinks at `high` unless told otherwise.
    ("particle-1", &[
        ("api_name", Text("particle/particle-1")),
        ("vision", Flag(true)),
        ("display_name", Text("Particle-1")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // MiniMax's own route (minimaxi/): with "required" it acts in the first
    // turn; "auto" gave the notes call first.
    ("minimax-m3.1-flash", &[
        ("api_name", Text("minimaxi/minimax-m3.1-flash")),
        ("vision", Flag(true)),
        ("display_name", Text("MiniMax M3.1 Flash")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
    ]),
    // DeepSeek's own route. Its thinking mode refuses a forced tool call (400
    // "Thinking mode does not support this tool_choice") and thinking is on
    // unless asked off, so `none` turns it off and keeps "required".
    ("deepseek-v4.1-flash-1017", &[
        ("api_name", Text("deepseek/deepseek-v4.1-flash-1017")),
        ("vision", Flag(true)),
        ("display_name", Text("DeepSeek V4.1 Flash 1017")),
        ("effort", Text("none")),
        ("coder_effort", Text("none")),
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
        .get_or_try_init(py, || py.import(format!("{}.requesty.view", super::super::NATIVE)).map(Bound::unbind))
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
