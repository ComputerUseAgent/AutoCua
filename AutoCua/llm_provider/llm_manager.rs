//! llm_manager: the one manager every agent sends its requests through
//! (LLMManager), the provider builder every platform and the Rust agents use
//! (build_provider, resolve_model_info), and the tool-call parser
//! (tool_calls_to_steps).
//!
//! The Rust copy of llm_provider/llm_manager.py. Python imports it under its
//! old dotted name, AutoCua.llm_provider.llm_manager (llm_provider/
//! __init__.py points that name here), and finds the same names in the same
//! order, with the same values, return values, errors and printed lines.
//! Python's own rules decide every result, so they are called, not imitated:
//! the builtins, the str and dict methods, the operators, the comparisons and
//! the f-strings.
//!
//! llm_manager.py did its work when it was imported; here that is two steps.
//! register() gives the module only _build. Then the loader binds what
//! llm_manager.py's import lines bound (copy, importlib, os, sys, time,
//! Optional, load_dotenv, _ensure_tls_works, RefusalError) and calls
//! _build(), which runs the rest of llm_manager.py's lines in their order:
//! the provider table, its two lookups, load_dotenv(), the constants, each
//! function where its def was, and the class.
//!
//! Every global a line reads (copy, time, sys, print, _key, build_provider,
//! RefusalError, int, str, sorted, ...) is read from this module at that
//! moment, as Python reads a global, so a caller or a test that replaces one
//! is obeyed. The manager keeps its attributes in its __dict__, and its
//! methods reach them and each other through the instance, so a subclass
//! (each platform's tool registry) or a patched attribute is obeyed. An error
//! is raised the way Python's raise statement raises it, so the error being
//! handled at that moment becomes its __context__.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::{PyRuntimeError, PyStopIteration, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pyclass::CompareOp;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyIterator, PyList, PyString, PyTuple};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::controller::{self, caught, import_from, now, raise};
use crate::memory_compression::{format_value, join_str};

type Obj<'py> = Bound<'py, PyAny>;

/// LLMManager's docstring.
const CLASS_DOC: &str = "Manager to route requests to the correct LLM provider.\n\
\n\
Shared by every platform: each platform's tool_registry/service.py\n\
subclasses it and sets the tool lists below (iOS only the main driver's),\n\
and the agents use that subclass.";

/// The line of llm_manager.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 500;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 19] = [
    "_cli_fallback_model",
    "_primary_model_info",
    "_tool_cuts",
    "claude",
    "cli_agent",
    "display_name",
    "has_vision",
    "last_call_seconds",
    "last_usage",
    "max_tokens",
    "mode",
    "model",
    "model_info",
    "model_short_name",
    "native_tools",
    "provider",
    "provider_instance",
    "runtime_api_key",
    "speed",
];

/// The class attributes that are None until a platform's subclass sets them.
const TOOL_LISTS: [&str; 7] = [
    "MAIN_TOOLS",
    "MAIN_TOOLS_FAST",
    "MAIN_TOOLS_CLAUDE",
    "CODER_TOOLS",
    "CODER_TOOLS_CLAUDE",
    "MINION_TOOLS",
    "MINION_TOOLS_CLAUDE",
];

/// _PROVIDER_CLASSES: each provider and the class its service.py holds.
const PROVIDER_CLASSES: [(&str, &str); 12] = [
    ("openrouter", "OpenRouterProvider"),
    ("groq", "GroqProvider"),
    ("openai", "OpenAIProvider"),
    ("anthropic", "AnthropicProvider"),
    ("google", "GoogleProvider"),
    ("perplexity", "PerplexityProvider"),
    ("together", "TogetherProvider"),
    ("cerebras", "CerebrasProvider"),
    ("requesty", "RequestyProvider"),
    ("aws", "AWSProvider"),
    ("hcompany", "HCompanyProvider"),
    ("ollama", "OllamaProvider"),
];

/// _TRACK_PARAMS["thinking"].
const TRACK_THINKING: &str = "Follow the <thinking> rules: \"THINK: ... PLAN: ... ACT: ...\" (FULL) at decision points, a short freeform paragraph (RECOVERY) on failures, 1\u{2013}2 judgment lines on plan-driven steps \u{2014} or exactly \"skipped\" when the SKIP TEST passes. Never empty. Fill on the FIRST tool call of the step; pass \"\" on every additional call in the same step.";

/// _TRACK_PARAMS["next_goal"].
const TRACK_NEXT_GOAL: &str = "Follow the <next_goal> rules \u{2014} labeled format: \"memory: <S<n> verdict + key context> next_goal: Doing: ... If ... \u{2192} Next: ...\". Fill on the FIRST tool call of the step; pass \"\" on every additional call in the same step.";

/// _CLI_FALLBACK_CANDIDATES: each provider's fallback models, in order.
const CLI_FALLBACK_CANDIDATES: [(&str, &[&str]); 10] = [
    // Single entry: groq now registers one model, so a user already on it has
    // nothing to fall back TO and correctly resolves to None. The tuple still
    // earns its place for a hand-typed groq model, which falls back here.
    ("groq", &["qwen3.8-27b"]),
    // Single entry: the openai table holds one model, so a user already on
    // it has nothing to fall back TO and correctly resolves to None.
    ("openai", &["gpt-6-astra"]),
    ("openrouter", &["gemini-3.6-flash", "gpt-6-astra"]),
    ("anthropic", &["claude-haiku-4.5", "claude-sonnet-5"]),
    ("google", &["gemini-3.6-flash", "gemini-4-argon"]),
    ("perplexity", &["gemini-3.6-flash", "kimi-k3"]),
    ("together", &["minimax-m3", "inkling"]),
    // Single entry, like groq: one registered model, so nothing to fall back TO.
    ("cerebras", &["qwen-3.8-27b"]),
    // Non-Claude on purpose, for the Claude entries too: a fallback keeps the
    // primary's tool registry, and the Claude one (`agent_memory`) suits any
    // model, while a Claude model handed the default `reasoning` tool has
    // refused it (seen on the browser agent's Claude path).
    ("aws", &["kimi-k3", "grok-4.7"]),
    // None: the models are the ones the person has pulled, and no other is
    // sure to be there.
    ("ollama", &[]),
];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.llm_manager", super::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// Python's statements (the shared ones in lib.rs's ios::controller, given
// this module)
// ---------------------------------------------------------------------------

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name. A missing one is Python's NameError, naming the name.
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `isinstance(value, <kind>)`: isinstance and the class read from this
/// module now, then the truth of the answer.
fn isinstance(value: &Obj<'_>, kind: &str) -> PyResult<bool> {
    controller::isinstance(&module(value.py())?, value, kind)
}

/// `raise <kind>(<message>)`: the global `kind` read first, then the message
/// made, then the error made and raised, in Python's order.
fn throw<'py>(py: Python<'py>, kind: &str, message: impl FnOnce() -> PyResult<Obj<'py>>) -> PyErr {
    match module(py) {
        Ok(m) => controller::throw(&m, kind, message),
        Err(e) => e,
    }
}

/// An `except Exception:` block: `body` runs while `e` is being handled,
/// with Exception read from this module as Python reads it there. What does
/// not match passes on untouched.
fn except_exception<T>(py: Python<'_>, e: PyErr, body: impl FnOnce(&PyErr) -> PyResult<T>) -> PyResult<T> {
    controller::except_exception(&module(py)?, e, body)
}

/// `print(line)`, with the print read from this module now.
fn print(py: Python<'_>, line: &Obj<'_>) -> PyResult<()> {
    g(py, "print")?.call1((line,))?;
    Ok(())
}

/// A str constant.
fn lit<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

/// `a or b`: a when it is true, else b, made only then.
fn or<'py>(a: Obj<'py>, b: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    if a.is_truthy()? {
        Ok(a)
    } else {
        b()
    }
}

/// `{}`, a new empty dict.
fn empty_dict(py: Python<'_>) -> Obj<'_> {
    PyDict::new(py).into_any()
}

/// `value.get(key)`, the method looked up when it is called.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// Whether a comprehension or a generator takes iter() a second time of the
/// iterator it loops over: Python 3.13's code does, 3.12's and 3.14's loop
/// over it as it is.
fn iter_again(py: Python<'_>) -> bool {
    let version = py.version_info();
    version >= (3, 13) && version < (3, 14)
}

/// What a list or dict comprehension loops over: iter() of `source`, then on
/// Python 3.13 iter() of that again.
fn comprehension<'py>(source: &Obj<'py>) -> PyResult<Bound<'py, PyIterator>> {
    let items = source.try_iter()?;
    if iter_again(source.py()) {
        return items.try_iter();
    }
    Ok(items)
}

/// An f-string, made as Python makes one: each {value} is formatted the
/// moment it is reached (format(value, spec)), and the pieces are joined at
/// the end.
struct Fmt<'py> {
    py: Python<'py>,
    pieces: Vec<Obj<'py>>,
}

impl<'py> Fmt<'py> {
    fn new(py: Python<'py>) -> Self {
        Fmt { py, pieces: Vec::new() }
    }

    /// A literal part.
    fn s(mut self, part: &str) -> Self {
        self.pieces.push(lit(self.py, part));
        self
    }

    /// A {value} part.
    fn v(self, value: &Obj<'py>) -> PyResult<Self> {
        self.spec(value, "")
    }

    /// A {value:spec} part.
    fn spec(mut self, value: &Obj<'py>, spec: &str) -> PyResult<Self> {
        self.pieces.push(format_value(value, spec)?.into_any());
        Ok(self)
    }

    fn done(self) -> PyResult<Obj<'py>> {
        let pieces: Vec<&Obj<'py>> = self.pieces.iter().collect();
        Ok(join_str(self.py, &pieces)?.into_any())
    }
}

/// A str tuple of names, as a constant tuple of llm_manager.py holds them.
fn names<'py>(py: Python<'py>, items: &[&str]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items.iter().map(|item| PyString::intern(py, item)))
}

/// A def's parameters bound to a call's arguments, as Python binds them:
/// the positional ones first, then the keywords; each slot is the argument
/// given, or None where the def's default applies. `selfs` is 1 for a
/// method, whose self Python counts in its messages.
fn bind<'py>(
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
// The generator expressions
// ---------------------------------------------------------------------------

/// What a generator expression's body does with one item: Some(value) to
/// give it, None to go on to the next item.
type Body = Box<dyn FnMut(&Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> + Send>;

/// A body, typed so the closure takes any item.
fn body<F>(step: F) -> Body
where
    F: FnMut(&Bound<'_, PyAny>) -> PyResult<Option<Py<PyAny>>> + Send + 'static,
{
    Box::new(step)
}

// The generator expressions handed to any(), next() and tuple(), run as
// Python runs one (as vault/service.rs runs its own). Made holding iter() of
// what it loops over, on Python 3.13 its first step takes iter() of that
// again, as that version's generator code does; then it runs its body one
// value per next(). A StopIteration from its own body comes out as
// RuntimeError (PEP 479), and once it has ended or failed it gives nothing
// more. A plain comment: a doc comment would give the class a docstring, and
// a generator has none.
#[pyclass(frozen, name = "generator", module = "builtins")]
struct Genexpr {
    /// The function that made it, as its __qualname__ names the generator.
    qualname: &'static str,
    /// iter() of what it loops over, made when the generator was made.
    given: Py<PyIterator>,
    /// iter() of `given` on Python 3.13, taken when the generator first runs.
    items: PyOnceLock<Py<PyIterator>>,
    /// The body; held while a step runs.
    body: Mutex<Body>,
    /// The generator has ended or failed.
    done: AtomicBool,
}

impl Genexpr {
    /// The generator made: iter() of `source` taken now.
    fn made<'py>(source: &Obj<'py>, qualname: &'static str, body: Body) -> PyResult<Obj<'py>> {
        let given = source.try_iter()?;
        let generator = Genexpr {
            qualname,
            given: given.unbind(),
            items: PyOnceLock::new(),
            body: Mutex::new(body),
            done: AtomicBool::new(false),
        };
        Ok(Bound::new(source.py(), generator)?.into_any())
    }
}

#[pymethods]
impl Genexpr {
    fn __iter__<'py>(slf: &Bound<'py, Self>) -> Bound<'py, Self> {
        slf.clone()
    }

    fn __next__<'py>(slf: &Bound<'py, Self>) -> PyResult<Option<Obj<'py>>> {
        let py = slf.py();
        let me = slf.get();
        let Ok(mut step) = me.body.try_lock() else {
            return Err(now(py, PyValueError::new_err("generator already executing")));
        };
        if me.done.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let ran = (|| -> PyResult<Option<Obj<'py>>> {
            let items = if iter_again(py) {
                let again = || me.given.bind(py).as_any().try_iter().map(Bound::unbind);
                me.items.get_or_try_init(py, again)?.bind(py).clone()
            } else {
                me.given.bind(py).clone()
            };
            for item in items {
                if let Some(value) = (*step)(&item?)? {
                    return Ok(Some(value.into_bound(py)));
                }
            }
            Ok(None)
        })();
        drop(step);
        match ran {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => {
                me.done.store(true, Ordering::SeqCst);
                Ok(None)
            }
            Err(e) => {
                me.done.store(true, Ordering::SeqCst);
                if !e.is_instance_of::<PyStopIteration>(py) {
                    return Err(e);
                }
                let error = py.get_type::<PyRuntimeError>().call1(("generator raised StopIteration",))?;
                error.setattr("__cause__", e.value(py))?;
                error.setattr("__context__", e.value(py))?;
                Err(PyErr::from_value(error))
            }
        }
    }

    fn __repr__(slf: &Bound<'_, Self>) -> String {
        format!("<generator object {} at {:p}>", slf.get().qualname, slf.as_ptr())
    }
}

// ---------------------------------------------------------------------------
// The provider table's two lookups
// ---------------------------------------------------------------------------

// def _provider_class(provider: str): (no docstring)
#[pyfunction]
#[pyo3(name = "_provider_class")]
fn provider_class<'py>(provider: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = provider.py();
    // return getattr(importlib.import_module(f".{provider}.service", __package__),
    //                _PROVIDER_CLASSES[provider])
    let getattr = g(py, "getattr")?;
    let import_module = g(py, "importlib")?.getattr("import_module")?;
    let path = Fmt::new(py).s(".").v(&provider)?.s(".service").done()?;
    let found = import_module.call1((path, g(py, "__package__")?))?;
    let class_name = g(py, "_PROVIDER_CLASSES")?.get_item(&provider)?;
    getattr.call1((found, class_name))
}

// def _model_info_getter(provider: str): (no docstring)
#[pyfunction]
#[pyo3(name = "_model_info_getter")]
fn model_info_getter<'py>(provider: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = provider.py();
    // return importlib.import_module(f".{provider}.view", __package__).get_model_info
    let import_module = g(py, "importlib")?.getattr("import_module")?;
    let path = Fmt::new(py).s(".").v(&provider)?.s(".view").done()?;
    import_module.call1((path, g(py, "__package__")?))?.getattr("get_model_info")
}

// ---------------------------------------------------------------------------
// The tool definitions
// ---------------------------------------------------------------------------

#[pyfunction]
#[pyo3(name = "_tool", signature = (*args, **kwargs), text_signature = "(name, params, description='', track=None, optional=())")]
#[doc = "Build one canonical tool def — name + parameters + description. The\n\
REGISTRY is the single source of tool documentation: each tool's rules\n\
live in its `description`, which flows to every provider dialect. The\n\
system prompt keeps only the step protocol and operating procedure. Tool\n\
defs sit in the cached prefix, so this bills once. All params are required\n\
unless named in `optional`, so the controller always sees every field of\n\
an action (the only optional one today is the coder's `thinking`, which a\n\
route step omits). `track` (the minion's and the main driver's per-call\n\
tracking params) rides ahead of the action fields when given; the coder\n\
passes none, because its notes live on the `reasoning` tool."]
fn tool<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let slots = bind("_tool", 0, args, kwargs, &["name", "params", "description", "track", "optional"], 2)?;
    let [name, params, description, track, optional]: [Option<Obj<'py>>; 5] =
        slots.try_into().map_err(|_| PyTypeError::new_err("_tool() arguments"))?;
    let name = name.unwrap_or_else(|| none(py));
    let params = params.unwrap_or_else(|| none(py));
    let description = description.unwrap_or_else(|| lit(py, ""));
    let track = track.unwrap_or_else(|| none(py));
    let optional = optional.unwrap_or_else(|| PyTuple::empty(py).into_any());
    // props = {b: {"type": "string", "description": d} for b, d in (track or {}).items()}
    let props = PyDict::new(py);
    for pair in comprehension(&or(track, || Ok(empty_dict(py)))?.call_method0("items")?)? {
        let (b, d) = unpack2(&pair?)?;
        let field = PyDict::new(py);
        field.set_item("type", "string")?;
        field.set_item("description", d)?;
        props.set_item(b, field)?;
    }
    // props.update(params)
    props.call_method1("update", (params,))?;
    // tool = {"name": name, "parameters": {"type": "object", "properties": props,
    //         "required": [k for k in props if k not in optional]}}
    let required = PyList::empty(py);
    for k in comprehension(props.as_any())? {
        let k = k?;
        if !optional.contains(&k)? {
            required.append(k)?;
        }
    }
    let parameters = PyDict::new(py);
    parameters.set_item("type", "object")?;
    parameters.set_item("properties", &props)?;
    parameters.set_item("required", required)?;
    let tool = PyDict::new(py);
    tool.set_item("name", name)?;
    tool.set_item("parameters", parameters)?;
    // if description: tool["description"] = description
    if description.is_truthy()? {
        tool.set_item("description", description)?;
    }
    Ok(tool.into_any())
}

#[pyfunction]
#[doc = "OpenAI/OpenRouter/Groq/Together chat-completions function format."]
fn coder_tools_openai<'py>(registry: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = registry.py();
    // [{"type": "function", "function": t} for t in registry]
    let out = PyList::empty(py);
    for t in comprehension(&registry)? {
        let one = PyDict::new(py);
        one.set_item("type", "function")?;
        one.set_item("function", t?)?;
        out.append(one)?;
    }
    Ok(out.into_any())
}

#[pyfunction]
#[pyo3(name = "_with_description")]
#[doc = "Carry the registry description into a dialect dict when present (the\n\
coder `exit` tool has none — _tool() only sets the key when non-empty)."]
fn with_description<'py>(tool: Obj<'py>, source: Obj<'py>) -> PyResult<Obj<'py>> {
    // if source.get("description"): tool["description"] = source["description"]
    if get(&source, "description")?.is_truthy()? {
        tool.set_item("description", source.get_item("description")?)?;
    }
    Ok(tool)
}

/// The dialects' comprehensions: `[_with_description({<key>: t["name"],
/// <schema>: t["parameters"]}, t) for t in registry]`, with
/// `"type": "function"` first for Perplexity.
fn described<'py>(registry: &Obj<'py>, function_type: bool, schema: &str) -> PyResult<Obj<'py>> {
    let py = registry.py();
    let out = PyList::empty(py);
    for t in comprehension(registry)? {
        let t = t?;
        let with_description = g(py, "_with_description")?;
        let one = PyDict::new(py);
        if function_type {
            one.set_item("type", "function")?;
        }
        one.set_item("name", t.get_item("name")?)?;
        one.set_item(schema, t.get_item("parameters")?)?;
        out.append(with_description.call1((one, t))?)?;
    }
    Ok(out.into_any())
}

#[pyfunction]
#[doc = "Anthropic Messages API tools format."]
fn coder_tools_anthropic<'py>(registry: Obj<'py>) -> PyResult<Obj<'py>> {
    described(&registry, false, "input_schema")
}

#[pyfunction]
#[doc = "Gemini function declarations (dicts accepted by google-genai)."]
fn coder_tools_gemini<'py>(registry: Obj<'py>) -> PyResult<Obj<'py>> {
    described(&registry, false, "parameters")
}

#[pyfunction]
#[doc = "Perplexity agent API (Responses-style flat function tools)."]
fn coder_tools_perplexity<'py>(registry: Obj<'py>) -> PyResult<Obj<'py>> {
    described(&registry, true, "parameters")
}

#[pyfunction]
#[pyo3(name = "_coerce")]
#[doc = "Best-effort coercion to the default's type (models sometimes send '5' for 5)."]
fn coerce<'py>(value: Obj<'py>, default: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = value.py();
    if isinstance(&default, "bool")? {
        if isinstance(&value, "bool")? {
            return Ok(value);
        }
        // return str(value).strip().lower() in ("true", "1", "yes")
        let word = g(py, "str")?.call1((value,))?.call_method0("strip")?.call_method0("lower")?;
        let yes = names(py, &["true", "1", "yes"])?.contains(word)?;
        return Ok(yes.into_pyobject(py)?.to_owned().into_any());
    }
    if isinstance(&default, "int")? && !isinstance(&default, "bool")? {
        // try: return int(value)
        // except (TypeError, ValueError): return default
        return match g(py, "int").and_then(|int| int.call1((&value,))) {
            Ok(number) => Ok(number),
            Err(e) => {
                let _handling = Handling::new(py, &e);
                let kinds = (|| -> PyResult<Obj<'py>> {
                    let type_error = g(py, "TypeError")?;
                    let value_error = g(py, "ValueError")?;
                    Ok(PyTuple::new(py, [type_error, value_error])?.into_any())
                })()
                .map_err(|raised| now(py, raised))?;
                if caught(py, &e, &kinds)? {
                    Ok(default)
                } else {
                    Err(e)
                }
            }
        };
    }
    if isinstance(&default, "str")? {
        // return value if isinstance(value, str) else str(value)
        if isinstance(&value, "str")? {
            return Ok(value);
        }
        return g(py, "str")?.call1((value,));
    }
    Ok(value)
}

#[pyfunction]
#[pyo3(name = "_dialect_tool_name")]
#[doc = "A tool's name in any provider dialect: at the top (Anthropic, Gemini,\n\
Perplexity) or under `function` (chat completions)."]
fn dialect_tool_name<'py>(tool: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = tool.py();
    // return tool.get("name") or (tool.get("function") or {}).get("name") or ""
    let top = get(&tool, "name")?;
    if top.is_truthy()? {
        return Ok(top);
    }
    let inner = get(&or(get(&tool, "function")?, || Ok(empty_dict(py)))?, "name")?;
    or(inner, || Ok(lit(py, "")))
}

// ---------------------------------------------------------------------------
// The tool-call parser
// ---------------------------------------------------------------------------

/// A bool, as Python's `in` and `not` give one.
fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

/// `a == b` (or another comparison): Python's rich comparison, its result as
/// it is.
fn compare<'py>(a: &Obj<'py>, b: impl IntoPyObject<'py>, op: CompareOp) -> PyResult<Obj<'py>> {
    a.rich_compare(b, op)
}

#[pyfunction]
#[pyo3(signature = (tool_calls, allowed, defaults_map, track_params = None))]
#[doc = "Convert normalized provider tool calls into\n\
(actions, calls, rejects, track) where:\n\
\n\
\x20 actions — the SAME `[{type, ...}]` dicts route_action always consumed,\n\
\x20           with the tracking params STRIPPED (the controller never sees\n\
\x20           them)\n\
\x20 calls   — parallel to actions: {\"id\", \"name\", \"arguments\"} for each,\n\
\x20           so the loop can echo the model's OWN tool calls back in the\n\
\x20           next request and match each result to its call id (the native\n\
\x20           transcript that makes models behave natively). Arguments are\n\
\x20           kept EXACTLY as the model sent them — tracking params\n\
\x20           included — so the echoed transcript never contradicts the\n\
\x20           schema that required them.\n\
\x20 rejects — {\"id\", \"name\", \"error\"} for calls naming a tool that does not\n\
\x20           exist; the loop feeds these back as error tool results\n\
\x20 track   — the tracking params stitched from the step's calls: the first\n\
\x20           call carries them in full, later calls pass \"\".\n\
\n\
`allowed` and `defaults_map` come from the caller's tool registry, which\n\
re-exports this function, so the agents' call sites stay as they were."]
fn tool_calls_to_steps<'py>(
    tool_calls: Obj<'py>,
    allowed: Obj<'py>,
    defaults_map: Obj<'py>,
    track_params: Option<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    let py = tool_calls.py();
    let actions = PyList::empty(py);
    let calls = PyList::empty(py);
    let rejects = PyList::empty(py);
    // track = {b: "" for b in (track_params or _TRACK_PARAMS)}
    let source = or(track_params.unwrap_or_else(|| none(py)), || g(py, "_TRACK_PARAMS"))?;
    let track = PyDict::new(py);
    for b in comprehension(&source)? {
        track.set_item(b?, "")?;
    }
    let names = allowed;
    let dm = defaults_map;
    // When the registry has a dedicated notes tool carrying the tracking
    // params (`reasoning`: coder, minion, main driver in quality mode; `memory`:
    // main driver in fast mode), the action tools do NOT, so a malformed call's
    // "required" list must not name them. A registry without one (older saves
    // replayed against an old map) still carries them on every call. Read off
    // the defaults map, which is per registry.
    // notes_on_own_tool = any(n in dm and set(track) <= set(dm[n]) for n in NOTE_TOOLS)
    let any = g(py, "any")?;
    let held_dm = dm.clone().unbind();
    let held_track = track.clone().into_any().unbind();
    let step = body(move |n| {
        let py = n.py();
        let dm = held_dm.bind(py);
        if !dm.contains(n)? {
            return Ok(Some(boolean(py, false).unbind()));
        }
        let mine = g(py, "set")?.call1((held_track.bind(py),))?;
        let theirs = g(py, "set")?.call1((dm.get_item(n)?,))?;
        Ok(Some(compare(&mine, theirs, CompareOp::Le)?.unbind()))
    });
    let notes = Genexpr::made(&g(py, "NOTE_TOOLS")?, "tool_calls_to_steps.<locals>.<genexpr>", step)?;
    let notes_on_own_tool = any.call1((notes,))?;
    let enumerate = g(py, "enumerate")?;
    let numbered = enumerate.call1((or(tool_calls, || Ok(PyList::empty(py).into_any()))?,))?;
    for pair in numbered.try_iter()? {
        let (i, call) = unpack2(&pair?)?;
        let call_or_empty = || or(call.clone(), || Ok(empty_dict(py)));
        // name = str((call or {}).get("name") or "").strip()
        let str_ = g(py, "str")?;
        let raw = or(get(&call_or_empty()?, "name")?, || Ok(lit(py, "")))?;
        let name = str_.call1((raw,))?.call_method0("strip")?;
        // args = (call or {}).get("arguments")
        let mut args = get(&call_or_empty()?, "arguments")?;
        if !isinstance(&args, "dict")? {
            args = empty_dict(py);
        }
        // call_id = str((call or {}).get("id") or "") or f"call_{i}"
        let str_ = g(py, "str")?;
        let raw = or(get(&call_or_empty()?, "id")?, || Ok(lit(py, "")))?;
        let call_id = or(str_.call1((raw,))?, || Fmt::new(py).s("call_").v(&i)?.done())?;
        for b in track.as_any().try_iter()? {
            let b = b?;
            // v = str(args.get(b) or "").strip()
            let str_ = g(py, "str")?;
            let raw = or(args.call_method1("get", (&b,))?, || Ok(lit(py, "")))?;
            let v = str_.call1((raw,))?.call_method0("strip")?;
            // if v and not track[b]: track[b] = v
            if v.is_truthy()? && !track.as_any().get_item(&b)?.is_truthy()? {
                track.as_any().set_item(&b, v)?;
            }
        }
        // defaults = dm.get(name)
        let defaults = dm.call_method1("get", (&name,))?;
        if defaults.is_none() || !names.contains(&name)? {
            let shown = or(name.clone(), || Ok(lit(py, "(unnamed)")))?;
            let reject = PyDict::new(py);
            reject.set_item("id", &call_id)?;
            reject.set_item("name", shown)?;
            reject.set_item("arguments", &args)?;
            // f"No tool named '{name or '(unnamed)'}' exists. "
            // f"Available tools: {', '.join(sorted(names))}. "
            // f"Call one of those instead."
            let shown = or(name.clone(), || Ok(lit(py, "(unnamed)")))?;
            let message = Fmt::new(py).s("No tool named '").v(&shown)?.s("' exists. Available tools: ");
            let join = lit(py, ", ").getattr("join")?;
            let sorted = g(py, "sorted")?;
            let listed = join.call1((sorted.call1((&names,))?,))?;
            let error = message.v(&listed)?.s(". Call one of those instead.").done()?;
            reject.set_item("error", error)?;
            rejects.append(reject)?;
            continue;
        }
        // EMPTY arguments is a schema violation, not a set of omitted optional
        // fields — every param on every tool is `required`. Letting the defaults
        // below fill it would silently promote the model's malformed turn into a
        // REAL action: a `left_click` with no arguments becomes id 0 / clicks 1
        // and clicks whatever element 0 happens to be; an `input` with none
        // becomes id 0 / value "", which focuses element 0 and clears it.
        // Observed in the wild on the browser agent's driver (gpt-5.6-luna emitted
        // `"arguments": {}` on one step, then a well-formed call on the next).
        // Reject it exactly the way an unknown tool name is rejected, so the
        // model gets an error keyed to its own call id and re-issues the step
        // instead of acting on an element it never chose.
        //
        // Narrow on purpose: ONLY a fully empty argument object. A partial call
        // still gets default-filled as before, and id 0 remains a perfectly
        // valid element — the problem is never the value 0 itself, only a 0 that
        // arrived from a default the model never sent.
        if defaults.is_truthy()? && !args.is_truthy()? {
            let reject = PyDict::new(py);
            reject.set_item("id", &call_id)?;
            reject.set_item("name", &name)?;
            reject.set_item("arguments", &args)?;
            // f"'{name}' was called with no arguments at all. Every "
            // f"field is required: "
            // f"{', '.join(sorted((set(defaults) - (set(REASONING_OPTIONAL) if name in ('reasoning', 'agent_memory') else set())) | (set() if notes_on_own_tool else set(track))))}. "
            // f"Re-issue the call with all of them filled in."
            let message = Fmt::new(py).s("'").v(&name)?.s("' was called with no arguments at all. Every field is required: ");
            let join = lit(py, ", ").getattr("join")?;
            let sorted = g(py, "sorted")?;
            let every = g(py, "set")?.call1((&defaults,))?;
            // (`names` here is the local; self::names makes the tuple constant.)
            let optional = if self::names(py, &["reasoning", "agent_memory"])?.contains(&name)? {
                let set = g(py, "set")?;
                set.call1((g(py, "REASONING_OPTIONAL")?,))?
            } else {
                g(py, "set")?.call0()?
            };
            let wanted = every.sub(optional)?;
            let notes = if notes_on_own_tool.is_truthy()? {
                g(py, "set")?.call0()?
            } else {
                g(py, "set")?.call1((&track,))?
            };
            let listed = join.call1((sorted.call1((wanted.bitor(notes)?,))?,))?;
            let error = message.v(&listed)?.s(". Re-issue the call with all of them filled in.").done()?;
            reject.set_item("error", error)?;
            rejects.append(reject)?;
            continue;
        }
        // action = {"type": name}
        let action = PyDict::new(py);
        action.set_item("type", &name)?;
        for pair in defaults.call_method0("items")?.try_iter()? {
            let (key, default) = unpack2(&pair?)?;
            // action[key] = _coerce(args[key], default) if key in args else default
            let value = if args.contains(&key)? {
                let coerce = g(py, "_coerce")?;
                coerce.call1((args.get_item(&key)?, default))?
            } else {
                default
            };
            action.set_item(key, value)?;
        }
        actions.append(action)?;
        let echoed = PyDict::new(py);
        echoed.set_item("id", &call_id)?;
        echoed.set_item("name", &name)?;
        echoed.set_item("arguments", &args)?;
        calls.append(echoed)?;
    }
    Ok(PyTuple::new(py, [actions.into_any(), calls.into_any(), rejects.into_any(), track.into_any()])?.into_any())
}

#[pyfunction]
#[pyo3(name = "_pick_cli_fallback")]
#[doc = "First registered secondary for `provider` that isn't `model` itself."]
fn pick_cli_fallback<'py>(provider: Obj<'py>, model: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = provider.py();
    const QUALNAME: &str = "_pick_cli_fallback.<locals>.<genexpr>";
    // candidates = _CLI_FALLBACK_CANDIDATES.get(provider, ())
    let table = g(py, "_CLI_FALLBACK_CANDIDATES")?;
    let mut candidates = table.call_method1("get", (&provider, PyTuple::empty(py)))?;
    // Vertex and AI-Studio are different clients, decided at provider
    // construction — a vertex model may only fall back to another vertex one.
    if compare(&provider, "google", CompareOp::Eq)?.is_truthy()?
        && g(py, "str")?.call1((&model,))?.call_method1("endswith", ("-vertex",))?.is_truthy()?
    {
        // candidates = tuple(f"{c}-vertex" for c in candidates)
        let tuple = g(py, "tuple")?;
        let step = body(|c| Ok(Some(Fmt::new(c.py()).v(c)?.s("-vertex").done()?.unbind())));
        candidates = tuple.call1((Genexpr::made(&candidates, QUALNAME, step)?,))?;
    }
    // return next((c for c in candidates if c != model), None)
    let next = g(py, "next")?;
    let held = model.clone().unbind();
    let step = body(move |c| {
        if compare(c, held.bind(c.py()), CompareOp::Ne)?.is_truthy()? {
            Ok(Some(c.clone().unbind()))
        } else {
            Ok(None)
        }
    });
    next.call1((Genexpr::made(&candidates, QUALNAME, step)?, py.None()))
}

#[pyfunction]
#[doc = "Backstop for an `async_tools` model (GPT-6 Astra, see responses.py).\n\
\n\
With every tool async, nothing stops the model from bundling its terminal\n\
call (`done` for the driver, `exit` for the coder and the minion) after\n\
this turn's other calls, whose results it has not seen; the controller\n\
would run them and end the run on the terminal call, unverified. The\n\
terminal call is dropped from the batch and answered with an error result\n\
keyed to its id, so the other calls run, the replay stays complete (the\n\
caller's all_calls still holds the call) and the model calls the terminal\n\
tool alone once it has verified them. A turn whose only calls are terminal\n\
ones is returned untouched. Returns (actions, calls, results); the caller\n\
prints under its own stdout convention."]
fn drop_bundled_terminal<'py>(actions: Obj<'py>, calls: Obj<'py>, rejects: Obj<'py>, terminal: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = actions.py();
    const QUALNAME: &str = "drop_bundled_terminal.<locals>.<genexpr>";
    // `a.get("type") <op> terminal` for each action, as a generator.
    let typed = |op: CompareOp| -> PyResult<Obj<'py>> {
        let held = terminal.clone().unbind();
        let step = body(move |a| Ok(Some(compare(&get(a, "type")?, held.bind(a.py()), op)?.unbind())));
        Genexpr::made(&actions, QUALNAME, step)
    };
    // bundled = any(a.get("type") == terminal for a in actions)
    let any = g(py, "any")?;
    let bundled = any.call1((typed(CompareOp::Eq)?,))?;
    // others = any(a.get("type") != terminal for a in actions) or bool(rejects)
    let any = g(py, "any")?;
    let others = or(any.call1((typed(CompareOp::Ne)?,))?, || g(py, "bool")?.call1((&rejects,)))?;
    if !(bundled.is_truthy()? && others.is_truthy()?) {
        return Ok(PyTuple::new(py, [actions, calls, PyList::empty(py).into_any()])?.into_any());
    }
    // results = [{"tool_call_id": c["id"], "content": f"error: {terminal} ignored, ..."}
    //            for a, c in zip(actions, calls) if a.get("type") == terminal]
    let results = PyList::empty(py);
    let zip = g(py, "zip")?;
    for pair in comprehension(&zip.call1((&actions, &calls))?)? {
        let (a, c) = unpack2(&pair?)?;
        if compare(&get(&a, "type")?, &terminal, CompareOp::Eq)?.is_truthy()? {
            let result = PyDict::new(py);
            result.set_item("tool_call_id", c.get_item("id")?)?;
            let content = Fmt::new(py)
                .s("error: ")
                .v(&terminal)?
                .s(" ignored, it must be the only call of its turn. This turn's other calls ran; verify them on the next input, then call ")
                .v(&terminal)?
                .s(" alone.")
                .done()?;
            result.set_item("content", content)?;
            results.append(result)?;
        }
    }
    // kept = [(a, c) for a, c in zip(actions, calls) if a.get("type") != terminal]
    let kept = PyList::empty(py);
    let zip = g(py, "zip")?;
    for pair in comprehension(&zip.call1((&actions, &calls))?)? {
        let (a, c) = unpack2(&pair?)?;
        if compare(&get(&a, "type")?, &terminal, CompareOp::Ne)?.is_truthy()? {
            kept.append(PyTuple::new(py, [a, c])?)?;
        }
    }
    // return [a for a, _ in kept], [c for _, c in kept], results
    let first = PyList::empty(py);
    for pair in comprehension(kept.as_any())? {
        first.append(unpack2(&pair?)?.0)?;
    }
    let second = PyList::empty(py);
    for pair in comprehension(kept.as_any())? {
        second.append(unpack2(&pair?)?.1)?;
    }
    Ok(PyTuple::new(py, [first.into_any(), second.into_any(), results.into_any()])?.into_any())
}

#[pyfunction]
#[doc = "Provider-appropriate get_model_info; unknown providers pass through.\n\
\n\
Every provider view's get_model_info already falls through to a\n\
passthrough dict for unregistered names, so any model string the user\n\
types is usable — nothing here validates against an allowlist.\n\
"]
fn resolve_model_info<'py>(provider: Obj<'py>, short_name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = provider.py();
    if !g(py, "_PROVIDER_CLASSES")?.contains(&provider)? {
        let info = PyDict::new(py);
        info.set_item("api_name", &short_name)?;
        info.set_item("vision", true)?;
        info.set_item("display_name", &short_name)?;
        return Ok(info.into_any());
    }
    // return _model_info_getter(provider)(short_name)
    g(py, "_model_info_getter")?.call1((provider,))?.call1((short_name,))
}

#[pyfunction]
#[doc = "`registry` (the canonical tool list) in `provider`'s own tool format."]
fn dialect_tools<'py>(provider: Obj<'py>, registry: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = provider.py();
    for (name, dialect) in [
        ("anthropic", "coder_tools_anthropic"),
        ("google", "coder_tools_gemini"),
        ("perplexity", "coder_tools_perplexity"),
    ] {
        if compare(&provider, name, CompareOp::Eq)?.is_truthy()? {
            return g(py, dialect)?.call1((registry,));
        }
    }
    // aws takes this format too and makes Converse toolSpecs of it itself,
    // because it is also what the browser agent's Rust manager sends any provider
    // it has no dialect for.
    g(py, "coder_tools_openai")?.call1((registry,))
}

#[pyfunction]
#[pyo3(name = "_key")]
#[doc = "The API key for a provider, in order: the one the caller passed (the\n\
app's Settings for a run from the app, api_key= for a script), the\n\
environment and .env, then the .env the app's Settings writes\n\
(AutoCua.env_file), read again now, so a key saved after this process\n\
loaded its environment counts too. None when none of them has it."]
fn key<'py>(api_key: Obj<'py>, name: Obj<'py>) -> PyResult<Obj<'py>> {
    let py = api_key.py();
    if api_key.is_truthy()? {
        return Ok(api_key);
    }
    // value = os.getenv(name)
    let value = g(py, "os")?.getattr("getenv")?.call1((&name,))?;
    if value.is_truthy()? {
        return Ok(value);
    }
    // try:
    //     from AutoCua import read_env
    //     return read_env().get(name) or None
    // except Exception:
    //     return None
    let saved = (|| -> PyResult<Obj<'py>> {
        let read_env = import_from(&module(py)?, "AutoCua", &["read_env"])?.remove(0);
        or(read_env.call0()?.call_method1("get", (&name,))?, || Ok(none(py)))
    })();
    match saved {
        Ok(value) => Ok(value),
        Err(e) => except_exception(py, e, |_| Ok(none(py))),
    }
}

/// The keyword part of a call, in the order given.
fn keywords<'py>(py: Python<'py>, items: &[(&str, &Obj<'py>)]) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    for (key, value) in items {
        kwargs.set_item(*key, value)?;
    }
    Ok(kwargs)
}

/// One branch of build_provider that needs a key: `api_key = _key(api_key,
/// '<env>')`, the ValueError when there is none, then `make` calls
/// `_provider_class("<provider>")` with it.
fn keyed<'py>(
    py: Python<'py>,
    provider: &str,
    env: &str,
    missing: &str,
    api_key: &Obj<'py>,
    make: impl FnOnce(Obj<'py>, Obj<'py>) -> PyResult<Obj<'py>>,
) -> PyResult<Obj<'py>> {
    // Priority: Runtime key > .env fallback
    let api_key = g(py, "_key")?.call1((api_key, env))?;
    if !api_key.is_truthy()? {
        return Err(throw(py, "ValueError", || Ok(lit(py, missing))));
    }
    let class = g(py, "_provider_class")?.call1((provider,))?;
    make(class, api_key)
}

#[pyfunction]
#[pyo3(
    signature = (*args, **kwargs),
    text_signature = "(provider, model_short_name, model_info, tools=None, cli_agent=False, max_tokens=None, api_key=None)"
)]
#[doc = "The provider object for one manager: `tools` already in the provider's\n\
own format (None for mode=\"text\"), and the API key (_key: the runtime key\n\
from the frontend first, then .env and the environment, then the one\n\
saved in the app's Settings). Every platform's LLMManager builds its\n\
provider here, and so does the browser agent's Rust manager. Runs the Windows\n\
TLS probe of this provider's own host first (a no-op on macOS and Linux)."]
fn build_provider<'py>(args: &Bound<'py, PyTuple>, kwargs: Option<&Bound<'py, PyDict>>) -> PyResult<Obj<'py>> {
    let py = args.py();
    let params = ["provider", "model_short_name", "model_info", "tools", "cli_agent", "max_tokens", "api_key"];
    let slots = bind("build_provider", 0, args, kwargs, &params, 3)?;
    let mut slots = slots.into_iter();
    let mut next = || slots.next().flatten();
    let provider = next().unwrap_or_else(|| none(py));
    let model_short_name = next().unwrap_or_else(|| none(py));
    let model_info = next().unwrap_or_else(|| none(py));
    let tools = next().unwrap_or_else(|| none(py));
    let cli_agent = next().unwrap_or_else(|| boolean(py, false));
    let max_tokens = next().unwrap_or_else(|| none(py));
    let api_key = next().unwrap_or_else(|| none(py));

    // _ensure_tls_works(provider, model_short_name)
    g(py, "_ensure_tls_works")?.call1((&provider, &model_short_name))?;
    let is = |name: &str| -> PyResult<bool> { compare(&provider, name, CompareOp::Eq)?.is_truthy() };
    // `<class>(api_key, cli_agent, model_info, tools=tools, max_tokens=max_tokens)`
    let positional = |class: Obj<'py>, key: Obj<'py>| -> PyResult<Obj<'py>> {
        let kwargs = keywords(py, &[("tools", &tools), ("max_tokens", &max_tokens)])?;
        class.call((key, &cli_agent, &model_info), Some(&kwargs))
    };
    // `<class>(api_key, cli_agent, tools=tools, max_tokens=max_tokens, model_info=model_info)`
    let by_name = |class: Obj<'py>, key: Obj<'py>| -> PyResult<Obj<'py>> {
        let kwargs = keywords(py, &[("tools", &tools), ("max_tokens", &max_tokens), ("model_info", &model_info)])?;
        class.call((key, &cli_agent), Some(&kwargs))
    };
    if is("openrouter")? {
        let missing = "OpenRouter API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "openrouter", "OPENROUTER_API_KEY", missing, &api_key, positional);
    }
    if is("groq")? {
        let missing = "Groq API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "groq", "GROQ_API_KEY", missing, &api_key, positional);
    }
    if is("openai")? {
        let missing = "OpenAI API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "openai", "OPENAI_API_KEY", missing, &api_key, by_name);
    }
    if is("anthropic")? {
        let missing = "Anthropic API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "anthropic", "ANTHROPIC_API_KEY", missing, &api_key, by_name);
    }
    if is("google")? {
        // Check if this is a Vertex model
        let model_meta = g(py, "_model_info_getter")?.call1(("google",))?.call1((&model_short_name,))?;
        let is_vertex = model_meta.call_method1("get", ("vertex", false))?;

        if is_vertex.is_truthy()? {
            // Vertex config from the .env the Settings panel writes
            // (AutoCua.env_file), read again now like _key does.
            let mut vertex_project_id = none(py);
            let mut vertex_location = none(py);
            let read = (|| -> PyResult<()> {
                let read_env = import_from(&module(py)?, "AutoCua", &["read_env"])?.remove(0);
                let saved = read_env.call0()?;
                vertex_project_id = or(get(&saved, "VERTEX_PROJECT_ID")?, || Ok(none(py)))?;
                vertex_location = or(get(&saved, "VERTEX_LOCATION")?, || Ok(none(py)))?;
                Ok(())
            })();
            if let Err(e) = read {
                except_exception(py, e, |_| Ok(()))?;
            }
            let class = g(py, "_provider_class")?.call1(("google",))?;
            let nothing = none(py);
            let kwargs = keywords(
                py,
                &[
                    ("api_key", &nothing),
                    ("cli_agent", &cli_agent),
                    ("model", &model_short_name),
                    ("vertex_project_id", &vertex_project_id),
                    ("vertex_location", &vertex_location),
                    ("tools", &tools),
                    ("model_info", &model_info),
                    ("max_tokens", &max_tokens),
                ],
            )?;
            return class.call((), Some(&kwargs));
        }
        // AI Studio — needs API key
        let missing = "Google API key not provided, not in .env, and not saved in the app's Settings";
        let studio = |class: Obj<'py>, key: Obj<'py>| -> PyResult<Obj<'py>> {
            let kwargs = keywords(
                py,
                &[("model", &model_short_name), ("tools", &tools), ("model_info", &model_info), ("max_tokens", &max_tokens)],
            )?;
            class.call((key, &cli_agent), Some(&kwargs))
        };
        return keyed(py, "google", "GOOGLE_API_KEY", missing, &api_key, studio);
    }
    if is("perplexity")? {
        let missing = "Perplexity API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "perplexity", "PERPLEXITY_API_KEY", missing, &api_key, positional);
    }
    if is("together")? {
        let missing = "Together API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "together", "TOGETHER_API_KEY", missing, &api_key, positional);
    }
    if is("cerebras")? {
        let missing = "Cerebras API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "cerebras", "CEREBRAS_API_KEY", missing, &api_key, positional);
    }
    if is("requesty")? {
        let missing = "Requesty API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "requesty", "REQUESTY_API_KEY", missing, &api_key, positional);
    }
    if is("aws")? {
        let missing = "AWS API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "aws", "AWS_API_KEY", missing, &api_key, positional);
    }
    if is("hcompany")? {
        let missing = "H Company API key not provided, not in .env, and not saved in the app's Settings";
        return keyed(py, "hcompany", "HCOMPANY_API_KEY", missing, &api_key, positional);
    }
    if is("ollama")? {
        // A model on this machine (or the Ollama at OLLAMA_HOST) needs no
        // key. OLLAMA_API_KEY is for an Ollama behind one, and goes out
        // only when set.
        let class = g(py, "_provider_class")?.call1(("ollama",))?;
        let key = g(py, "_key")?.call1((&api_key, "OLLAMA_API_KEY"))?;
        return positional(class, key);
    }
    Err(throw(py, "ValueError", || Fmt::new(py).s("Unsupported provider: ").v(&provider)?.done()))
}

// ---------------------------------------------------------------------------
// LLMManager
// ---------------------------------------------------------------------------

/// `self.<name>`, through the instance, as Python reads it.
fn attr<'py>(slf: &Obj<'py>, name: &str) -> PyResult<Obj<'py>> {
    slf.getattr(name)
}

/// `int(u.get(<key>, 0) or 0)` for one usage count. int is read before
/// u.get() runs, as Python loads the global before it evaluates the argument.
fn count<'py>(py: Python<'py>, u: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    let int = g(py, "int")?;
    let value = u.call_method1("get", (key, 0))?;
    int.call1((or(value, || Ok(0i32.into_pyobject(py)?.into_any()))?,))
}

// Manager to route requests to the correct LLM provider (its docstring is
// CLASS_DOC, set in register()).
#[pyclass(frozen, dict, weakref, subclass, name = "LLMManager")]
pub struct LLMManager;

#[pymethods]
impl LLMManager {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(provider, model, api_key=None, cli_agent=False, mode='main', speed='quality')"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        LLMManager
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let params = ["provider", "model", "api_key", "cli_agent", "mode", "speed"];
        let slots = bind("LLMManager.__init__", 1, args, kwargs, &params, 2)?;
        let mut slots = slots.into_iter();
        let mut next = || slots.next().flatten();
        let provider = next().unwrap_or_else(|| none(py));
        let model = next().unwrap_or_else(|| none(py));
        let api_key = next().unwrap_or_else(|| none(py));
        let cli_agent = next().unwrap_or_else(|| boolean(py, false));
        let mode = next().unwrap_or_else(|| PyString::intern(py, "main").into_any());
        let speed = next().unwrap_or_else(|| PyString::intern(py, "quality").into_any());

        me.setattr("provider", provider.call_method0("lower")?)?;
        me.setattr("model_short_name", &model)?;
        me.setattr("runtime_api_key", &api_key)?; // Runtime key from frontend (priority)
        // Coder/minion flag. NOT a "native tools" switch — every mode but
        // "text" is native now. It stays False for the MAIN DRIVER because
        // each provider gates its screenshot splice on `not self.cli_agent`,
        // so flipping it would silently kill the driver's vision.
        me.setattr("cli_agent", &cli_agent)?;
        me.setattr("mode", &mode)?; // "main" | "minion" | "text" — picks the tool registry
        me.setattr("speed", &speed)?; // "quality" | "fast" — fast trims the main-agent tracking params
        // Output cap handed to the provider: 10k for the handoff agent, None
        // for every other mode (each provider keeps its own default).
        let max_tokens = if compare(&mode, "text", CompareOp::Eq)?.is_truthy()? {
            g(py, "HANDOFF_MAX_TOKENS")?
        } else {
            none(py)
        };
        me.setattr("max_tokens", max_tokens)?;

        // Coder / minion (cli_agent=True) run the SAME model as everyone else —
        // whatever the UI, cli.py or --model handed in. The only per-provider
        // hardcoding left is the emergency fallback, used for one call (see
        // send_request).
        let fallback = if cli_agent.is_truthy()? {
            let pick = g(py, "_pick_cli_fallback")?;
            pick.call1((attr(me, "provider")?, &model))?
        } else {
            none(py)
        };
        me.setattr("_cli_fallback_model", fallback)?;

        // Get model info based on provider
        let model_info = me.call_method1("_resolve_model_info", (&model,))?;

        me.setattr("model", model_info.get_item("api_name")?)?;
        me.setattr("has_vision", model_info.get_item("vision")?)?;
        me.setattr("display_name", model_info.get_item("display_name")?)?;
        me.setattr("model_info", &model_info)?; // Full model info, forwarded to the provider
        me.setattr("_primary_model_info", &model_info)?; // restored after every fallback call

        // NATIVE TOOL CALLING is the only path: the main driver, the coder and
        // the minion all get tool definitions as their output contract — no
        // JSON envelope, no response_format, nothing to parse. The single
        // exception is mode="text" (the memory-compression handoff), which
        // wants plain prose and so gets neither tools nor a schema.
        me.setattr("native_tools", compare(&mode, "text", CompareOp::Ne)?)?;

        // Most recent send_request's normalized token usage (input/output/total).
        // Captured as a side effect so callers (e.g. the memory bar) can read it
        // without changing send_request's return shape.
        me.setattr("last_usage", PyDict::new(py))?;
        // Most recent successful API round-trip in seconds (ping → response),
        // measured fresh per attempt — read this in code for the exact number.
        me.setattr("last_call_seconds", PyFloat::new(py, 0.0))?;
        // Any Claude model, whatever the provider: anthropic direct, or a Claude
        // entry on OpenRouter (api_name anthropic/claude-...). It
        // picks the tool registry below and, in every agent, claude_prompt.md
        // plus the `agent_memory` notes tool. Set once from the primary model,
        // so the one-call fallback never flips it.
        let anthropic = compare(&attr(me, "provider")?, "anthropic", CompareOp::Eq)?;
        let claude = or(anthropic, || {
            let lowered = attr(me, "model")?.call_method0("lower")?;
            Ok(boolean(py, lowered.contains("claude")?))
        })?;
        me.setattr("claude", claude)?;
        me.setattr("provider_instance", me.call_method0("_initialize_provider")?)?;
        Ok(())
    }

    #[doc = "resolve_model_info for this manager's provider."]
    fn _resolve_model_info<'py>(slf: &Bound<'py, Self>, short_name: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let resolve = g(py, "resolve_model_info")?;
        resolve.call1((attr(slf.as_any(), "provider")?, short_name))
    }

    #[doc = "Point this manager at a model.\n\
\n\
Also re-points provider_instance.model_info, which openrouter/groq/\n\
perplexity cache at construction — otherwise a swapped model inherits\n\
the other model's registry entry.\n\
"]
    fn _apply_model_info<'py>(slf: &Bound<'py, Self>, info: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        me.setattr("model", info.get_item("api_name")?)?;
        me.setattr("has_vision", info.get_item("vision")?)?;
        me.setattr("display_name", info.get_item("display_name")?)?;
        me.setattr("model_info", &info)?;
        // provider_instance = getattr(self, "provider_instance", None)
        let provider_instance = g(py, "getattr")?.call1((me, "provider_instance", py.None()))?;
        if !provider_instance.is_none() && g(py, "hasattr")?.call1((&provider_instance, "model_info"))?.is_truthy()? {
            provider_instance.setattr("model_info", &info)?;
        }
        Ok(())
    }

    #[doc = "Initialize the appropriate provider based on selection"]
    fn _initialize_provider<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        // Hand each provider its dialect's tool definitions — the registry is
        // picked by mode (minion gets the read-only subset; the coder gets its
        // own; the main driver gets its action tools, thinking-less in fast
        // mode). mode="text" is the only caller that gets no tools at all.
        // A Claude model (self.claude, any provider) gets the CLAUDE variants,
        // whose notes tool is `agent_memory` (memory + next_goal, no thinking field).
        let claude = attr(me, "claude")?;
        let registry = if compare(&attr(me, "mode")?, "minion", CompareOp::Eq)?.is_truthy()? {
            attr(me, if claude.is_truthy()? { "MINION_TOOLS_CLAUDE" } else { "MINION_TOOLS" })?
        } else if attr(me, "cli_agent")?.is_truthy()? {
            attr(me, if claude.is_truthy()? { "CODER_TOOLS_CLAUDE" } else { "CODER_TOOLS" })?
        } else if compare(&attr(me, "speed")?, "fast", CompareOp::Eq)?.is_truthy()? {
            attr(me, "MAIN_TOOLS_FAST")?
        } else {
            attr(me, if claude.is_truthy()? { "MAIN_TOOLS_CLAUDE" } else { "MAIN_TOOLS" })?
        };
        let build = g(py, "build_provider")?;
        let provider = attr(me, "provider")?;
        let short_name = attr(me, "model_short_name")?;
        let model_info = attr(me, "model_info")?;
        let tools = if attr(me, "native_tools")?.is_truthy()? {
            let dialect = g(py, "dialect_tools")?;
            dialect.call1((attr(me, "provider")?, &registry))?
        } else {
            none(py)
        };
        let kwargs = keywords(
            py,
            &[
                ("cli_agent", &attr(me, "cli_agent")?),
                ("max_tokens", &attr(me, "max_tokens")?),
                ("api_key", &attr(me, "runtime_api_key")?),
            ],
        )?;
        build.call((provider, short_name, model_info, tools), Some(&kwargs))
    }

    #[doc = "Normalize a provider usage dict to {input_tokens, output_tokens,\n\
total_tokens, context_tokens}, tolerating both key styles (Anthropic-style\n\
input/output and OpenAI-style prompt/completion). Empty/missing -> zeros.\n\
\n\
context_tokens is the TRUE size of the prompt actually sent this turn — the\n\
memory-bar number. A cached token still occupies the context window, so we\n\
add the cache classes back: input_tokens + cache_read + cache_creation.\n\
This is exact for every provider:\n\
\x20 - Anthropic: input_tokens EXCLUDES cache, so the cache fields are added.\n\
\x20 - OpenAI/Google/Perplexity/OpenRouter/Groq: prompt_tokens already INCLUDES\n\
\x20   cached tokens and the Anthropic cache keys are absent (0), so this\n\
\x20   collapses to the full prompt count — no double-count.\n\
"]
    fn _normalize_usage<'py>(slf: &Bound<'py, Self>, u: Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        // u = u or {}
        let u = or(u, || Ok(empty_dict(py)))?;
        // `u.get(<key>, u.get(<fallback>, 0))`: the outer get looked up first.
        let either = |key: &str, fallback: &str| -> PyResult<Obj<'py>> {
            let outer = u.getattr("get")?;
            let inner = u.getattr("get")?.call1((fallback, 0))?;
            outer.call1((key, inner))
        };
        // inp = int(u.get("input_tokens", u.get("prompt_tokens", 0)) or 0)
        let int = g(py, "int")?;
        let inp = int.call1((or(either("input_tokens", "prompt_tokens")?, || Ok(0i32.into_pyobject(py)?.into_any()))?,))?;
        // out = int(u.get("output_tokens", u.get("completion_tokens", 0)) or 0)
        let int = g(py, "int")?;
        let out = int.call1((or(either("output_tokens", "completion_tokens")?, || Ok(0i32.into_pyobject(py)?.into_any()))?,))?;
        // tot = int(u.get("total_tokens", 0) or 0) or (inp + out)
        let tot = or(count(py, &u, "total_tokens")?, || inp.add(&out))?;
        // cache_read = int(u.get("cache_read_input_tokens", 0) or 0)
        let cache_read = count(py, &u, "cache_read_input_tokens")?;
        // cache_create = int(u.get("cache_creation_input_tokens", 0) or 0)
        let cache_create = count(py, &u, "cache_creation_input_tokens")?;
        let context_tokens = inp.add(&cache_read)?.add(&cache_create)?;
        let usage = PyDict::new(py);
        usage.set_item("input_tokens", inp)?;
        usage.set_item("output_tokens", out)?;
        usage.set_item("total_tokens", tot)?;
        usage.set_item("context_tokens", context_tokens)?;
        Ok(usage.into_any())
    }

    #[pyo3(signature = (messages, annotated_screenshot_base64 = None))]
    #[doc = "Three idempotent tries against the model currently loaded. Raises the\n\
last error if all three fail.\n\
\n\
Return shape: plain content string normally; in native-tools mode\n\
(coder) a dict {\"text\": str, \"tool_calls\": [{\"name\", \"arguments\"}],\n\
\"provider_meta\": dict} — the provider already normalized the calls."]
    fn _attempt<'py>(slf: &Bound<'py, Self>, messages: Obj<'py>, annotated_screenshot_base64: Option<Obj<'py>>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let shot = annotated_screenshot_base64.unwrap_or_else(|| none(py));
        let mut last_error = none(py);
        for attempt in g(py, "range")?.call1((3,))?.try_iter()? {
            let attempt = attempt?;
            // Providers may mutate messages in-place (e.g. wrapping the last user
            // message into multimodal content blocks); deep-copy per attempt so
            // those mutations cannot compound across retries.
            let attempt_messages = g(py, "copy")?.getattr("deepcopy")?.call1((&messages,))?;
            let provider = attr(me, "provider")?;
            if !g(py, "_META_PROVIDERS")?.contains(provider)? {
                for m in attempt_messages.try_iter()? {
                    let m = m?;
                    if isinstance(&m, "dict")? {
                        let pop = m.getattr("pop")?;
                        pop.call1((g(py, "_META_KEY")?, py.None()))?;
                    }
                }
            }
            let tried = (|| -> PyResult<Obj<'py>> {
                // Pure API round-trip: clock starts the moment the provider is
                // pinged, stops the moment its response is back. Fresh from 0
                // on EVERY attempt; the deep-copy above is outside the clock.
                let t0 = g(py, "time")?.getattr("perf_counter")?.call0()?;
                let send = attr(me, "provider_instance")?.getattr("send_request")?;
                let response = send.call1((&attempt_messages, attr(me, "model")?, &shot))?;
                let elapsed = g(py, "time")?.getattr("perf_counter")?.call0()?.sub(&t0)?;
                me.setattr("last_call_seconds", elapsed)?;
                let normalize = attr(me, "_normalize_usage")?;
                me.setattr("last_usage", normalize.call1((get(&response, "usage")?,))?)?;
                if g(py, "sys")?.getattr("stdout")?.call_method0("isatty")?.is_truthy()? {
                    // \r first: overwrite any spinner residue so the line is
                    // clean. TTY-only — never leak into the UI subprocess pipe.
                    // Token counts ride on the same line: a slow call with a
                    // small input/output is NOT a generation cost — it's cold
                    // prefill, provider-side reasoning, or backend routing.
                    let raw_usage = or(get(&response, "usage")?, || Ok(empty_dict(py)))?;
                    let int = g(py, "int")?;
                    // _cached = int(
                    //     _raw_usage.get("cache_read_input_tokens", 0)
                    //     or ((_raw_usage.get("prompt_tokens_details") or {}).get("cached_tokens", 0))
                    //     or 0
                    // )
                    let read = raw_usage.call_method1("get", ("cache_read_input_tokens", 0))?;
                    let cached = if read.is_truthy()? {
                        read
                    } else {
                        let details = or(get(&raw_usage, "prompt_tokens_details")?, || Ok(empty_dict(py)))?;
                        or(details.call_method1("get", ("cached_tokens", 0))?, || Ok(0i32.into_pyobject(py)?.into_any()))?
                    };
                    let cached = int.call1((cached,))?;
                    let print = g(py, "print")?;
                    let line = Fmt::new(py)
                        .s("\r\u{23f1} LLM call: ")
                        .spec(&attr(me, "last_call_seconds")?, ".2f")?
                        .s("s (")
                        .v(&attr(me, "display_name")?)?
                        .s(") | in ")
                        .v(&attr(me, "last_usage")?.get_item("input_tokens")?)?
                        .s(" (cached ")
                        .v(&cached)?
                        .s(") | out ")
                        .v(&attr(me, "last_usage")?.get_item("output_tokens")?)?
                        .done()?;
                    print.call1((line,))?;
                }
                let message = response.get_item("choices")?.get_item(0)?.get_item("message")?;
                if attr(me, "native_tools")?.is_truthy()? {
                    let reply = PyDict::new(py);
                    reply.set_item("text", or(get(&message, "content")?, || Ok(lit(py, "")))?)?;
                    reply.set_item("tool_calls", or(get(&message, "tool_calls")?, || Ok(PyList::empty(py).into_any()))?)?;
                    // Provider metadata for THIS turn, persisted with the
                    // step and handed back to the same provider next
                    // request (see _META_KEY above). {} when the provider
                    // emits none, which is the pre-existing behavior.
                    let meta_key = g(py, "_META_KEY")?;
                    let meta = or(message.call_method1("get", (g(py, "_META_KEY")?,))?, || Ok(empty_dict(py)))?;
                    reply.set_item(meta_key, meta)?;
                    return Ok(reply.into_any());
                }
                message.get_item("content")
            })();
            let e = match tried {
                Ok(result) => return Ok(result),
                Err(e) => e,
            };
            let _handling = Handling::new(py, &e);
            let refusal = g(py, "RefusalError").map_err(|raised| now(py, raised))?;
            if caught(py, &e, &refusal)? {
                // The API refused the request itself; the same request gets
                // the same answer, so no retry. send_request falls back where
                // it can.
                (|| -> PyResult<()> {
                    let print = g(py, "print")?;
                    let line = Fmt::new(py)
                        .s("\u{26d4} ")
                        .v(&attr(me, "display_name")?)?
                        .s(" refused the request: ")
                        .v(e.value(py).as_any())?
                        .done()?;
                    print.call1((line,))?;
                    Ok(())
                })()
                .map_err(|raised| now(py, raised))?;
                return Err(e);
            }
            let exception = g(py, "Exception").map_err(|raised| now(py, raised))?;
            if !caught(py, &e, &exception)? {
                return Err(e);
            }
            last_error = e.value(py).clone().into_any();
            (|| -> PyResult<()> {
                if compare(&attempt, 2, CompareOp::Lt)?.is_truthy()? {
                    let print = g(py, "print")?;
                    let line = Fmt::new(py)
                        .s("\u{26a0}\u{fe0f} ")
                        .v(&attr(me, "display_name")?)?
                        .s(" request failed (attempt ")
                        .v(&attempt.add(1)?)?
                        .s("/3): ")
                        .v(e.value(py).as_any())?
                        .done()?;
                    print.call1((line,))?;
                    print_line(py, "   Retrying in 1 second with a fresh message copy...")?;
                    g(py, "time")?.getattr("sleep")?.call1((1,))?;
                    return Ok(());
                }
                let print = g(py, "print")?;
                let line = Fmt::new(py)
                    .s("\u{274c} ")
                    .v(&attr(me, "display_name")?)?
                    .s(" request failed after 3 attempts: ")
                    .v(e.value(py).as_any())?
                    .done()?;
                print.call1((line,))?;
                Ok(())
            })()
            .map_err(|raised| now(py, raised))?;
        }
        Err(raise(py, &last_error))
    }

    #[pyo3(signature = (messages, annotated_screenshot_base64 = None))]
    #[doc = "Send request to the selected provider with idempotent retries.\n\
\n\
Coder/minion only: if the user's model fails all 3 attempts, ONE call is\n\
served by the per-provider fallback (3 attempts of its own). Win or lose,\n\
the manager is put back on the user's model afterwards, so the next\n\
iteration starts on their model again. If the fallback also fails its 3\n\
attempts the error propagates and the agent stops.\n\
\n\
The main agent has no fallback (cli_agent=False): 3 attempts, then raise.\n\
"]
    fn send_request<'py>(slf: &Bound<'py, Self>, messages: Obj<'py>, annotated_screenshot_base64: Option<Obj<'py>>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let me = slf.as_any();
        let shot = annotated_screenshot_base64.unwrap_or_else(|| none(py));
        // try: return self._attempt(messages, annotated_screenshot_base64)
        // except Exception:
        //     if not (self.cli_agent and self._cli_fallback_model): raise
        match me.call_method1("_attempt", (&messages, &shot)) {
            Ok(result) => return Ok(result),
            Err(e) => except_exception(py, e, |e| {
                let go_on = attr(me, "cli_agent")?.is_truthy()? && attr(me, "_cli_fallback_model")?.is_truthy()?;
                if !go_on {
                    return Err(e.clone_ref(py));
                }
                Ok(())
            })?,
        }

        let resolve = attr(me, "_resolve_model_info")?;
        let fallback_info = resolve.call1((attr(me, "_cli_fallback_model")?,))?;
        let print = g(py, "print")?;
        let line = Fmt::new(py)
            .s("\u{26a0}\u{fe0f} ")
            .v(&attr(me, "display_name")?)?
            .s(" failed \u{2014} this step only, falling back to ")
            .v(&fallback_info.get_item("display_name")?)?
            .s("...")
            .done()?;
        print.call1((line,))?;
        let apply = attr(me, "_apply_model_info")?;
        apply.call1((&fallback_info,))?;
        // try: result = self._attempt(messages, annotated_screenshot_base64)
        // except Exception: print(...); raise
        let outcome = match me.call_method1("_attempt", (&messages, &shot)) {
            Ok(result) => Ok(result),
            Err(e) => except_exception(py, e, |e| {
                let print = g(py, "print")?;
                let line = Fmt::new(py)
                    .s("\u{274c} Fallback ")
                    .v(&attr(me, "display_name")?)?
                    .s(" also failed 3 attempts \u{2014} stopping.")
                    .done()?;
                print.call1((line,))?;
                Err(e.clone_ref(py))
            }),
        };
        // finally: Always revert: the fallback covers this call, not the whole run.
        let revert = || -> PyResult<()> {
            let apply = attr(me, "_apply_model_info")?;
            apply.call1((attr(me, "_primary_model_info")?,))?;
            Ok(())
        };
        let result = match outcome {
            Ok(result) => {
                revert()?;
                result
            }
            Err(e) => {
                // The finally block runs while the error is being handled.
                let _handling = Handling::new(py, &e);
                revert().map_err(|raised| now(py, raised))?;
                return Err(e);
            }
        };
        let print = g(py, "print")?;
        let line = Fmt::new(py)
            .s("\u{21a9}\u{fe0f} Back on ")
            .v(&attr(me, "display_name")?)?
            .s(" for the next step.")
            .done()?;
        print.call1((line,))?;
        Ok(result)
    }

    #[doc = "True when the model batches only on the Responses endpoint with\n\
async tools (GPT-6 Astra, table flag `async_tools`): the loops add\n\
the <async_tools> prompt block and the bundled done/exit backstop\n\
for it. Read from the primary entry, so a one-call fallback does not flip it."]
    fn async_tools<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let bool_ = g(py, "bool")?;
        bool_.call1((get(&attr(slf.as_any(), "_primary_model_info")?, "async_tools")?,))
    }

    #[doc = "Send every tool, or every tool but `hidden`. The provider holds its\n\
tools in its own dialect and reads them on every request, so each short\n\
list is cut from the full one once per provider instance and swapped\n\
onto the provider. One toggle per manager: the driver hides its screen\n\
tools, the coder its notes tool."]
    fn _show_tools<'py>(slf: &Bound<'py, Self>, on: Obj<'py>, hidden: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let me = slf.as_any();
        let inst = attr(me, "provider_instance")?;
        // cache = getattr(self, "_tool_cuts", None)
        let mut cache = g(py, "getattr")?.call1((me, "_tool_cuts", py.None()))?;
        if cache.is_none() || !cache.get_item(0)?.is(&inst) {
            // cache = self._tool_cuts = (inst, inst.tools, {})
            let tools = inst.getattr("tools")?;
            let fresh = PyTuple::new(py, [inst.clone(), tools, empty_dict(py)])?.into_any();
            cache = fresh.clone();
            me.setattr("_tool_cuts", fresh)?;
        }
        let full = cache.get_item(1)?;
        let cuts = cache.get_item(2)?;
        if full.is_none() {
            return Ok(()); // mode="text": no tools to hide
        }
        let key = g(py, "frozenset")?.call1((hidden,))?;
        if !cuts.contains(&key)? {
            // cuts[key] = [t for t in full if _dialect_tool_name(t) not in key]
            let cut = PyList::empty(py);
            for t in comprehension(&full)? {
                let t = t?;
                let tool_name = g(py, "_dialect_tool_name")?.call1((&t,))?;
                if !key.contains(tool_name)? {
                    cut.append(t)?;
                }
            }
            cuts.set_item(&key, cut)?;
        }
        // inst.tools = full if on else cuts[key]
        let chosen = if on.is_truthy()? { full } else { cuts.get_item(&key)? };
        inst.setattr("tools", chosen)?;
        Ok(())
    }

    #[doc = "Scan toggle (main driver): send every tool, or every tool but\n\
SCREEN_TOOLS while the scan is off."]
    fn show_screen_tools<'py>(slf: &Bound<'py, Self>, on: Obj<'py>) -> PyResult<()> {
        let me = slf.as_any();
        let show = attr(me, "_show_tools")?;
        show.call1((on, attr(me, "SCREEN_TOOLS")?))?;
        Ok(())
    }

    #[doc = "Notes pause (coder): send every tool, or every tool but the notes\n\
tool (`reasoning` / `agent_memory`) while the coder has it paused after\n\
a run of notes-only turns, so the model can only call the tools that\n\
do the work."]
    fn show_notes_tool<'py>(slf: &Bound<'py, Self>, on: Obj<'py>) -> PyResult<()> {
        let py = slf.py();
        let show = attr(slf.as_any(), "_show_tools")?;
        show.call1((on, g(py, "NOTE_TOOLS")?))?;
        Ok(())
    }

    #[doc = "Get the current model short name (preserves vertex suffix for downstream routing)"]
    fn get_model_name<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        attr(slf.as_any(), "model_short_name")
    }

    #[doc = "Get the current provider name"]
    fn get_provider_name<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        attr(slf.as_any(), "provider")
    }
}

/// `print("<line>")`: a constant line through the print read now.
fn print_line(py: Python<'_>, line: &str) -> PyResult<()> {
    print(py, &lit(py, line))
}

// ---------------------------------------------------------------------------
// llm_manager.py's lines, run once the loader bound its imports
// ---------------------------------------------------------------------------

/// `key = value` at the top of llm_manager.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of llm_manager.py after its import lines, in their order,
/// and binds what each one makes on the module. The loader calls it once,
/// right after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // Each provider (and its SDK) loads when it is first used, not all at import:
    // the SDKs are thousands of modules, and an iOS agent's process loaded every
    // one of them before its first step (seconds, minutes on a busy Mac).
    let classes = PyDict::new(py);
    for (provider, class) in PROVIDER_CLASSES {
        classes.set_item(PyString::intern(py, provider), PyString::intern(py, class))?;
    }
    store(m, "_PROVIDER_CLASSES", classes)?;
    store(m, "_provider_class", wrap_pyfunction!(provider_class, m)?)?;
    store(m, "_model_info_getter", wrap_pyfunction!(model_info_getter, m)?)?;

    // Load environment variables
    g(py, "load_dotenv")?.call0()?;

    // One manager for every platform (windows, mac, linux, ios): each platform's
    // AutoCua/<os>/tool_registry/service.py holds its tool lists and subclasses
    // LLMManager below to hand them in. The browser agent's Rust manager builds its
    // providers here too (build_provider, resolve_model_info).
    //
    // Every agent here speaks NATIVE TOOL CALLING — the tool registries ARE
    // the output contract. There is no JSON-envelope response schema anymore: the
    // main driver's AGENT_OUTPUT_SCHEMA / FAST_AGENT_SCHEMA are gone the same way
    // CLI_AGENT_SCHEMA and MINION_SCHEMA were, and no provider is handed a
    // response_format. The only schema-less, tool-less path left is mode="text"
    // (the memory-compression handoff), which wants plain prose.

    // The coder/minion transcript carries an optional per-turn `provider_meta` on
    // assistant messages — the provider's OWN metadata for that turn (Gemini 3
    // thought signatures, OpenRouter reasoning blocks), needed to echo the turn
    // back exactly as the model produced it. Only these providers translate it;
    // for everyone else the key is stripped before the request is built, because
    // openai/groq/perplexity/together forward the message dicts to their API
    // verbatim and an unknown key there is a 400.
    store(m, "_META_KEY", PyString::intern(py, "provider_meta"))?;
    store(m, "_META_PROVIDERS", names(py, &["openrouter", "google"])?)?;

    // Output cap for the memory-compression handoff agent (mode="text"), the
    // same on every provider. It writes one long structured document, and the
    // 4000-token defaults on anthropic/openai/groq cut it off silently.
    store(m, "HANDOFF_MAX_TOKENS", 10000)?;

    // LEGACY default for tool_calls_to_steps (no registry carries these any more:
    // the coder and the minion moved their notes onto the `reasoning` tool). Kept
    // so a caller passing no track_params still harvests the old two params.
    let track = PyDict::new(py);
    track.set_item(PyString::intern(py, "thinking"), TRACK_THINKING)?;
    track.set_item(PyString::intern(py, "next_goal"), TRACK_NEXT_GOAL)?;
    store(m, "_TRACK_PARAMS", track)?;

    // The dedicated notes tools: `reasoning` (quality mode, coder, minion),
    // `agent_memory` (the same notes on Claude) and `memory` (fast mode). The
    // parser and the loop treat a call to any of them as the step's notes,
    // never as an action.
    store(m, "NOTE_TOOLS", names(py, &["reasoning", "agent_memory", "memory"])?)?;

    // The optional note field: `thinking` on the `reasoning` tools (see each tool
    // registry). Left out of the schema's `required` list and out of the "no
    // arguments" error text. Claude's `agent_memory` has no optional field: its
    // `memory` and `next_goal` are filled every step.
    store(m, "REASONING_OPTIONAL", names(py, &["thinking"])?)?;

    store(m, "_tool", wrap_pyfunction!(tool, m)?)?;
    store(m, "coder_tools_openai", wrap_pyfunction!(coder_tools_openai, m)?)?;
    store(m, "_with_description", wrap_pyfunction!(with_description, m)?)?;
    store(m, "coder_tools_anthropic", wrap_pyfunction!(coder_tools_anthropic, m)?)?;
    store(m, "coder_tools_gemini", wrap_pyfunction!(coder_tools_gemini, m)?)?;
    store(m, "coder_tools_perplexity", wrap_pyfunction!(coder_tools_perplexity, m)?)?;
    store(m, "_coerce", wrap_pyfunction!(coerce, m)?)?;
    store(m, "_dialect_tool_name", wrap_pyfunction!(dialect_tool_name, m)?)?;
    store(m, "tool_calls_to_steps", wrap_pyfunction!(tool_calls_to_steps, m)?)?;

    // Coder / minion emergency fallbacks. The PRIMARY model is always the one the
    // user picked (UI drop-up, cli.py's MODEL, --model) — these cover a SINGLE
    // failing call and are then dropped. Ordered: the first candidate that isn't
    // the user's own model wins, so the fallback is never the model that just
    // failed. Every name below exists in the matching provider view.py
    // MODEL_MAPPINGS (the old map's gpt-5.2 / gpt-5.1 /
    // claude-sonnet-4.5 did not, which is why the openai + anthropic fallbacks
    // could never succeed). (Each entry's own comment is on CLI_FALLBACK_CANDIDATES.)
    let candidates = PyDict::new(py);
    for (provider, models) in CLI_FALLBACK_CANDIDATES {
        candidates.set_item(PyString::intern(py, provider), PyTuple::new(py, models.iter().map(|model| PyString::new(py, model)))?)?;
    }
    store(m, "_CLI_FALLBACK_CANDIDATES", candidates)?;

    store(m, "_pick_cli_fallback", wrap_pyfunction!(pick_cli_fallback, m)?)?;
    store(m, "drop_bundled_terminal", wrap_pyfunction!(drop_bundled_terminal, m)?)?;
    store(m, "resolve_model_info", wrap_pyfunction!(resolve_model_info, m)?)?;
    store(m, "dialect_tools", wrap_pyfunction!(dialect_tools, m)?)?;
    store(m, "_key", wrap_pyfunction!(key, m)?)?;
    store(m, "build_provider", wrap_pyfunction!(build_provider, m)?)?;

    // class LLMManager:
    store(m, "LLMManager", py.get_type::<LLMManager>())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes llm_manager.py's place with _build, which the
/// loader calls once it has bound the import lines' names (see the top of
/// this file), and makes the class look like the one llm_manager.py made:
/// its docstring and its class attributes.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<LLMManager>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    for name in TOOL_LISTS {
        class.setattr(name, py.None())?;
    }
    // The tools the scan toggle takes away (show_screen_tools).
    class.setattr("SCREEN_TOOLS", PyTuple::empty(py))?;
    // What Python 3.13 gave the class statement: its line in llm_manager.py
    // and the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        class.setattr("__static_attributes__", names(py, &STATIC_ATTRIBUTES)?)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
