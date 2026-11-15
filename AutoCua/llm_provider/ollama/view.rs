//! view: the Ollama models' table (MODEL_MAPPINGS), the entry a model with
//! no table entry gets, and the list the app's and Telegram's pickers offer
//! (the table, then every other model this Ollama has pulled).
//!
//! The Rust copy of llm_provider/ollama/view.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.ollama.view
//! (llm_provider/ollama/__init__.py points that name here), and finds the
//! same names in the same order, with the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader calls _build() (view.py
//! had no import line), which runs view.py's lines in their order: the table,
//! then each function where its def was.
//!
//! Every global a line reads (MODEL_MAPPINGS, get_model_info, dict, ...) is
//! read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces or changes one is obeyed. listed_models
//! imports service's pulled_models when it runs, as view.py's did.

use pyo3::exceptions::{PyImportError, PyKeyError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyString, PyTuple};

use crate::computer::unpack2;
use crate::ios::controller::{self, now};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One value in a model's entry: a str (Text), a bool (Flag), a whole number
/// (Number) or a small dict of its own (Map), whose values are these kinds
/// again.
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
    // No entry uses the two below yet; they are here for one that does
    // (a "max_tokens": Number(10000), a nested dict as Map(&[...])).
    #[allow(dead_code)]
    Number(i64),
    #[allow(dead_code)]
    Map(&'static [(&'static str, Value)]),
}

use Value::{Flag, Map, Number, Text};

// Model mappings for the Ollama provider
// Maps user-friendly names to the model names Ollama serves

// Ollama runs models on this machine (or wherever OLLAMA_HOST points), so the
// list is whatever the person has pulled: any name `ollama list` shows can be
// typed as the model and is sent verbatim. The agents need native
// (OpenAI-format) tool calling, and vision wherever they send screenshots
// (computer use, browser, mobile); `ollama show <model>` lists both under
// Capabilities, as "tools" and "vision".
//
// `effort` goes out as reasoning_effort, which Ollama maps onto the model's
// own thinking levels. A level the model does not have is ignored without an
// error (it thinks at its default), so each entry names levels the model
// lists. An entry without one, like any unlisted model, keeps the model's
// default. Sampling is the model's own (its Modelfile), so service.py sends
// none.
//
// Every agent but the coder runs with thinking off ("none"): on a model on
// this machine each step's thinking is the wait, and the driver plans in its
// notes tool anyway. The coder thinks at "medium", one level up, as it works
// through code.
//
// Probed 2026-11-08/09 on Ollama 0.40.15 (Apple M5 Pro, 64 GB) with the real
// computer-use prompt and tools (step times below: a warm step, fast /
// quality mode), then end to end: a coder task (write and run a script) and
// a browser task in headless Chrome (read a page's heading):
// - qwen3.8:27b: dense 27B, MLX, 18 GB. Thinking levels false/low/medium/
//   xhigh (default medium); `high` is not one of them and thinks at the
//   default. 17 s / 27 s a step; acted on the first step every time; both
//   tasks done (76 s, 87 s). The most reliable for computer use.
// - qwen3.6:35b-a3b-nvfp4: mixture of experts, 36B total and about 3B active
//   per token, MLX, 23 GB. Thinking on or off only, on by default ("none"
//   turns it off, any other level on). 4 s / 6 s a step, about four times
//   as fast; planned before acting on the first step; both tasks done
//   (22 s, 25 s). The fastest that does the work.
// - ornith-1.5:35b: mixture of experts on the same architecture (35.5B total,
//   about 3B active), trained further for agents; Ollama ships it as a
//   Q4_K_M GGUF only (22 GB, 27 GB loaded), so it runs on the llama.cpp
//   engine, as fast here as qwen3.6 on MLX. Thinking on or off. 4 s / 6 s a
//   step; acted on the first step in both modes; both tasks done (33 s,
//   29 s). Fast and decisive: the one for computer use.
//
// Left out, from the same probe: qwen3.5:35b-a3b-nvfp4 (as fast, but sent
// malformed tool calls and failed the coder task), gemma4:26b-nvfp4 (a
// mixture of experts as fast, but only ever wrote notes in the screen
// agents: 100 browser steps without one action), nemotron-3.5-bolt:e4b-nvfp4
// (fastest, 3 s a step, but no tool calls in fast mode, the coder's sent as
// text, and a 65536-token window), mimo-v2.6:9b-nvfp4 (failed both tasks)
// and gemma4:31b-mlx (dense, slowest at 23 s / 36 s a step, the coder's
// tool calls sent as text). In fast mode, most of them answered without any
// tool call, so quality mode suits a local model better.

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add an Ollama model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false), Number(10000), and for a
// small dict of its own Map(&[("type", Text("adaptive"))]). The app rebuilds
// this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("qwen3.8:27b", &[
        ("api_name", Text("qwen3.8:27b")),
        ("vision", Flag(true)),
        ("display_name", Text("Qwen3.8 27B (Ollama)")),
        ("effort", Text("none")),
        ("coder_effort", Text("medium")),
    ]),
    ("qwen3.6:35b-a3b-nvfp4", &[
        ("api_name", Text("qwen3.6:35b-a3b-nvfp4")),
        ("vision", Flag(true)),
        ("display_name", Text("Qwen3.6 35B-A3B (Ollama)")),
        ("effort", Text("none")),
        ("coder_effort", Text("medium")),
    ]),
    ("ornith-1.5:35b", &[
        ("api_name", Text("ornith-1.5:35b")),
        ("vision", Flag(true)),
        ("display_name", Text("Ornith 1.5 35B-A3B (Ollama)")),
        ("effort", Text("none")),
        ("coder_effort", Text("medium")),
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
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.ollama.view", super::super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `from <level dots><name> import <wanted>` inside a function, run when the
/// line runs, as Python runs it: builtins.__import__(name, this module's
/// globals, None, (wanted,), level), then the name taken from what came
/// back, or else the submodule of that name already in sys.modules, or
/// else Python's ImportError. service.rs imports its own this way too.
pub(super) fn import_from_level<'py>(
    m: &Bound<'py, PyModule>,
    name: &str,
    level: i32,
    wanted: &str,
) -> PyResult<Obj<'py>> {
    let py = m.py();
    let import = py.import("builtins")?.getattr("__import__")?;
    let fromlist = PyTuple::new(py, [wanted])?;
    let found = import.call1((name, m.dict(), py.None(), fromlist, level))?;
    if let Some(value) = found.getattr_opt(wanted)? {
        return Ok(value);
    }
    let package = found.getattr_opt("__name__")?.filter(|package| package.is_instance_of::<PyString>());
    if let Some(package) = &package {
        let full = join_str(py, &[package, &PyString::new(py, ".").into_any(), &PyString::new(py, wanted).into_any()])?;
        match py.import("sys")?.getattr("modules")?.get_item(full) {
            Ok(module) => return Ok(module),
            Err(e) if e.is_instance_of::<PyKeyError>(py) => {}
            Err(e) => return Err(e),
        }
    }
    let shown = match &package {
        Some(package) => package.repr()?.to_string(),
        None => "'<unknown module name>'".to_string(),
    };
    let wanted_shown = PyString::new(py, wanted).repr()?.to_string();
    let message = format!("cannot import name {wanted_shown} from {shown} (unknown location)");
    let error = py.get_type::<PyImportError>().call1((message,))?;
    if let Some(package) = &package {
        error.setattr("name", package)?;
    }
    if py.version_info() >= (3, 12) {
        error.setattr("name_from", wanted)?;
    }
    Err(now(py, PyErr::from_value(error)))
}

/// A {value} of an f-string: format(value, '').
fn formatted<'py>(value: &Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(format_value(value, "")?.into_any())
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
    // Any other model the person has pulled: sent verbatim, thinking at its
    // own default.
    let info = PyDict::new(py);
    info.set_item(PyString::intern(py, "api_name"), &short_name)?;
    info.set_item(PyString::intern(py, "vision"), true)?;
    info.set_item(PyString::intern(py, "display_name"), &short_name)?;
    Ok(info.into_any())
}

#[pyfunction]
#[doc = "The models the app's and Telegram's pickers offer: the table above,\n\
then every other model this Ollama has pulled that can call tools, a\n\
model without vision marked so (it suits the coder only). Asked of\n\
Ollama on each call: Telegram asks on every pick, the app when it opens.\n\
With Ollama not answering, the table alone."]
fn listed_models(py: Python<'_>) -> PyResult<Obj<'_>> {
    // listed = dict(MODEL_MAPPINGS)
    let make = g(py, "dict")?;
    let listed = make.call1((g(py, "MODEL_MAPPINGS")?,))?;
    let tried = (|| -> PyResult<()> {
        // from .service import pulled_models
        let pulled_models = import_from_level(&module(py)?, "service", 1, "pulled_models")?;
        for pair in pulled_models.call0()?.try_iter()? {
            let (name, caps) = unpack2(&pair?)?;
            if listed.contains(&name)? {
                continue;
            }
            // info = dict(get_model_info(name))
            let make = g(py, "dict")?;
            let info = make.call1((g(py, "get_model_info")?.call1((&name,))?,))?;
            if !caps.is_none() && !caps.contains("vision")? {
                info.set_item("vision", false)?;
                // f"{name} (no vision)"
                let shown = formatted(&name)?;
                let text = join_str(py, &[&shown, &PyString::new(py, " (no vision)").into_any()])?;
                info.set_item("display_name", text)?;
            }
            listed.set_item(&name, &info)?;
        }
        Ok(())
    })();
    if let Err(e) = tried {
        // except Exception: pass
        controller::except_exception(&module(py)?, e, |_| Ok(()))?;
    }
    Ok(listed)
}

// ---------------------------------------------------------------------------
// view.py's lines, run once the loader calls _build
// ---------------------------------------------------------------------------

/// `key = value` at the top of view.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of view.py, in their order, and binds what each one makes
/// on the module. The loader calls it once and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // The table itself is at the top of this file.
    store(m, "MODEL_MAPPINGS", model_mappings(py)?)?;

    store(m, "get_model_info", wrap_pyfunction!(get_model_info, m)?)?;
    store(m, "listed_models", wrap_pyfunction!(listed_models, m)?)?;
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
