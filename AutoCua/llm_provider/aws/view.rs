//! view: the Amazon Bedrock models' table (MODEL_MAPPINGS), the region and
//! output cap a call falls back on, and the request fields each model takes
//! (Claude's effort and thinking, Kimi/Grok's reasoning level).
//!
//! The Rust copy of llm_provider/aws/view.py. Python imports it under its old
//! dotted name, AutoCua.llm_provider.aws.view (llm_provider/aws/__init__.py
//! points that name here), and finds the same names in the same order, with
//! the same values and return values.
//!
//! view.py did its work when it was imported; here that is two steps.
//! register() gives the module _build. Then the loader binds what view.py's
//! import line bound (entry_value) and calls _build(), which runs the rest of
//! view.py's lines in their order: the two constants and the table, then
//! each function where its def was.
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

/// One value in a model's entry: a str (Text), a bool (Flag), a whole number
/// (Number) or a small dict of its own (Map, for a field like
/// coder_thinking), whose values are these kinds again.
#[derive(Clone, Copy)]
enum Value {
    Text(&'static str),
    Flag(bool),
    Number(i64),
    Map(&'static [(&'static str, Value)]),
}

use Value::{Flag, Map, Number, Text};

// Model mappings for the AWS provider (Amazon Bedrock)
// Maps user-friendly names to actual API model names

// Every api_name is a `global.` cross-Region inference profile: the call goes
// to Bedrock's Converse API through one regional endpoint (AWS_BEDROCK_REGION
// in .env, DEFAULT_REGION when unset) and AWS serves it from whichever region
// has capacity.
//
// Request settings, probed against Bedrock in eu-west-2 on 2026-10-05:
//   effort            Claude: output_config.effort. Adaptive thinking is NOT
//                     sent: with it, Opus 4.7 answered a tool_choice "auto"
//                     request with an empty turn 3 times in 3; without it,
//                     every call worked. The coder (cli_agent) does get
//                     it, through each Claude entry's coder_thinking,
//                     coder_effort and coder_tool_choice auto (a forced
//                     call skips thinking): 14 live coder runs on
//                     2026-11-07 (Opus 4.7, Opus 4.6, Sonnet 4.6, multi-
//                     step file and bug-fix tasks) thought and finished
//                     with no empty turn, and Converse took 32768 output
//                     tokens with thinking, non-streamed.
//   reasoning_config  Kimi K3 and Grok 4.7: their reasoning level. Kimi's
//                     default is `max` and Grok's is `high`, so leaving it out
//                     is the expensive choice.
//   cache             Claude: cachePoint blocks after the system prompt and on
//                     the turn the agent loop marks with cache_control. Kimi K3
//                     rejects cachePoint with a 400 (it caches on its own) and
//                     Grok 4.7 with a 403.
//   max_tokens        Output cap: 10000 on every model, far under what Bedrock
//                     allows (Opus 4.7 and 4.6: 128K, Sonnet 4.6: 64K, per
//                     the AWS model cards on 2026-10-06; Kimi K3 and Grok 4.7
//                     state no lower limit). Kimi K3 and Grok 4.7 always
//                     reason, and their reasoning tokens count against it,
//                     as on openrouter/. The coder gets out_cap's 32768
//                     instead.
// No sampling knob is sent to any of them: Kimi K3 and Grok 4.7 reject
// temperature, and Opus 4.7 rejects top_k.

/// The region every call goes through when AWS_BEDROCK_REGION is unset.
const DEFAULT_REGION: &str = "eu-west-2";

// Output cap for a hand-typed model ID, which has no table entry.
const DEFAULT_MAX_TOKENS: i64 = 10000;

// One block per model, in the table's order, and in each block the entry's
// keys in their order. To add a Bedrock model, copy a block and change its
// values: Text("..."), Flag(true) / Flag(false), Number(10000), and for a
// small dict of its own Map(&[("type", Text("adaptive"))]). The app rebuilds
// this file by itself on its next start.
const MODEL_MAPPINGS: &[(&str, &[(&str, Value)])] = &[
    ("kimi-k3", &[
        ("api_name", Text("global.moonshotai.kimi-k3")),
        ("vision", Flag(true)),
        ("display_name", Text("Kimi K3")),
        ("reasoning_config", Text("low")),
        ("coder_reasoning_config", Text("medium")),
        ("max_tokens", Number(10000)),
    ]),
    ("grok-4.7", &[
        ("api_name", Text("global.xai.grok-4.7")),
        ("vision", Flag(true)),
        ("display_name", Text("Grok 4.7")),
        ("reasoning_config", Text("low")),
        ("coder_reasoning_config", Text("medium")),
        ("max_tokens", Number(10000)),
    ]),
    ("claude-opus-4.7", &[
        ("api_name", Text("global.anthropic.claude-opus-4-7")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 4.7")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_thinking", Map(&[("type", Text("adaptive"))])),
        ("coder_tool_choice", Text("auto")),
        ("cache", Flag(true)),
        ("max_tokens", Number(10000)),
    ]),
    ("claude-opus-4.6", &[
        ("api_name", Text("global.anthropic.claude-opus-4-6-v1")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Opus 4.6")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_thinking", Map(&[("type", Text("adaptive"))])),
        ("coder_tool_choice", Text("auto")),
        ("cache", Flag(true)),
        ("max_tokens", Number(10000)),
    ]),
    ("claude-sonnet-4.6", &[
        ("api_name", Text("global.anthropic.claude-sonnet-4-6")),
        ("vision", Flag(true)),
        ("display_name", Text("Claude Sonnet 4.6")),
        ("effort", Text("low")),
        ("coder_effort", Text("medium")),
        ("coder_thinking", Map(&[("type", Text("adaptive"))])),
        ("coder_tool_choice", Text("auto")),
        ("cache", Flag(true)),
        ("max_tokens", Number(10000)),
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
        .get_or_try_init(py, || py.import(format!("{}.aws.view", super::super::NATIVE)).map(Bound::unbind))
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
    // If not found, assume it's already a Bedrock model or profile ID
    let info = PyDict::new(py);
    info.set_item(PyString::intern(py, "api_name"), &short_name)?;
    info.set_item(PyString::intern(py, "vision"), true)?;
    info.set_item(PyString::intern(py, "display_name"), &short_name)?;
    Ok(info.into_any())
}

#[pyfunction]
#[doc = "The table entry whose api_name is `api_name`, or {} for a hand-typed ID."]
fn find_entry<'py>(api_name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = api_name.py();
    for info in g(py, "MODEL_MAPPINGS")?.call_method0("values")?.try_iter()? {
        let info = info?;
        if info.get_item("api_name")?.rich_compare(&api_name, CompareOp::Eq)?.is_truthy()? {
            return Ok(info);
        }
    }
    Ok(PyDict::new(py).into_any())
}

#[pyfunction]
#[pyo3(signature = (*args, **kwargs), text_signature = "(info, cli_agent=False)")]
#[doc = "additionalModelRequestFields for the entry being called: Claude's\n\
effort or Kimi/Grok's reasoning level. {} for a hand-typed ID, which then\n\
keeps the model's own default rather than a field it may reject. The\n\
coder reads the entry's coder_ fields (coder_thinking on Claude)."]
fn get_request_fields<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("get_request_fields", 0, args, kwargs, &["info", "cli_agent"], 1)?;
    let mut slots = slots.into_iter();
    let info = slots.next().flatten().unwrap_or_else(|| py.None().into_bound(py));
    let cli_agent = slots.next().flatten().unwrap_or_else(|| PyBool::new(py, false).to_owned().into_any());
    if get(&info, "effort")?.is_truthy()? {
        // fields = {"output_config": {"effort": entry_value(info, "effort", cli_agent)}}
        let effort = g(py, "entry_value")?.call1((&info, "effort", &cli_agent))?;
        let config = PyDict::new(py);
        config.set_item(PyString::intern(py, "effort"), effort)?;
        let fields = PyDict::new(py);
        fields.set_item(PyString::intern(py, "output_config"), config)?;
        let thinking = g(py, "entry_value")?.call1((&info, "thinking", &cli_agent))?;
        if thinking.is_truthy()? {
            fields.set_item("thinking", thinking)?;
        }
        return Ok(fields.into_any());
    }
    if get(&info, "reasoning_config")?.is_truthy()? {
        let level = g(py, "entry_value")?.call1((&info, "reasoning_config", &cli_agent))?;
        let fields = PyDict::new(py);
        fields.set_item(PyString::intern(py, "reasoning_config"), level)?;
        return Ok(fields.into_any());
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
    // The two constants and the table itself are at the top of this file.
    store(m, "DEFAULT_REGION", PyString::new(py, DEFAULT_REGION))?;
    store(m, "DEFAULT_MAX_TOKENS", DEFAULT_MAX_TOKENS)?;
    store(m, "MODEL_MAPPINGS", model_mappings(py)?)?;

    store(m, "get_model_info", wrap_pyfunction!(get_model_info, m)?)?;
    store(m, "find_entry", wrap_pyfunction!(find_entry, m)?)?;
    store(m, "get_request_fields", wrap_pyfunction!(get_request_fields, m)?)?;
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
