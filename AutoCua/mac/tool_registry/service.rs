//! The macOS main driver's tool registry: every tool the computer-use agent
//! can call on the Mac, and the LLMManager that hands them to the model.
//!
//! The Rust copy of tool_registry/service.py. Python imports it under its
//! old dotted name, AutoCua.mac.tool_registry.service (tool_registry/
//! __init__.py points that name here), and finds the same names with the
//! same values. The manager it subclasses stays the shared Python one,
//! AutoCua/llm_provider/llm_manager.py, and so does _tool, which builds
//! every tool; data_extraction.live_tools still takes the tools out that a
//! recorded run must not teach.
//!
//! The same design as the iPhone's registry (ios/tool_registry/service.rs,
//! whose helpers this file copies): register() gives the module what never
//! changes (its docstring and its two functions); the loader binds what
//! service.py's import lines bound (data_extraction, _LLMManager,
//! NOTE_TOOLS, REASONING_OPTIONAL, _tool, drop_bundled_terminal,
//! tool_calls_to_steps) and calls _build(), which runs the rest of
//! service.py's lines in their order. Every name a line reads is read from
//! the module at that moment, as Python reads a global, so a patched one is
//! the one used.
//!
//! Python's own rules decide every result, so they are called, not
//! imitated: _tool builds each tool, the scan tool's f-string goes through
//! format(), `+` and `!=` are Python's operators, dict.update joins the
//! defaults and the metaclass makes the class. The objects Python shares
//! stay shared: MAIN_TOOLS[0] is MAIN_REASONING_TOOL, the three defaults
//! maps share their value dicts, and an action tool's description (a
//! constant of _main_tools) is one object in all three tool lists.

use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyStopIteration, PyTypeError};
use pyo3::ffi;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyCFunction, PyDict, PyIterator, PyList, PyString, PyTuple, PyType};

use crate::computer::{type_name, unpack2};
use crate::ios::controller::{fstring, name, text};

/// service.py's docstring.
const MODULE_DOC: &str = concat!(
    "The macOS main driver's tool registry: every tool it can call, and the\n",
    "LLMManager that hands them to the model. The manager itself (providers,\n",
    "retries, fallback, the tool-call parser) is the same on every platform:\n",
    "AutoCua/llm_provider/llm_manager.py. The driver imports everything\n",
    "LLM-related from here.",
);

/// LLMManager's docstring.
const CLASS_DOC: &str = "The shared LLMManager with this platform's tool lists.";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 325;

/// A str constant of a function: made once, then the same object at every
/// call, as a Python function's constants are. Not interned: Python interns
/// only the constants that look like names, and those are made with
/// PyString::intern here.
macro_rules! constant {
    ($py:expr, $text:expr) => {{
        static TEXT: PyOnceLock<Py<PyString>> = PyOnceLock::new();
        TEXT.get_or_init($py, || PyString::new($py, $text).unbind()).bind($py)
    }};
}

/// An argument given, or left to its default.
enum Given<'py> {
    Value(Bound<'py, PyAny>),
    Missing,
}

impl<'py> FromPyObject<'_, 'py> for Given<'py> {
    type Error = PyErr;

    fn extract(obj: Borrowed<'_, 'py, PyAny>) -> PyResult<Self> {
        Ok(Given::Value(obj.to_owned()))
    }
}

// ---------------------------------------------------------------------------
// Python's displays and statements, as service.py's lines use them
// ---------------------------------------------------------------------------

/// A dict display, `{"key": value, ...}`, in the order given. Its keys are
/// names, so they are the interned strs Python's constants of that kind
/// are (the shared crate::ios::controller::dict does not intern them).
fn display<'py>(py: Python<'py>, items: &[(&str, &Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
    let built = PyDict::new(py);
    for (key, value) in items {
        built.set_item(PyString::intern(py, key), value)?;
    }
    Ok(built)
}

/// A notes tool's field, `{"type": "string", "description": <text>}`.
fn note_field<'py>(py: Python<'py>, description: &str) -> PyResult<Bound<'py, PyDict>> {
    let description = PyString::new(py, description);
    display(py, &[("type", intern!(py, "string").as_any()), ("description", description.as_any())])
}

/// The keyword part of a call, `key=value`.
fn keyword<'py>(py: Python<'py>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(PyString::intern(py, key), value)?;
    Ok(kwargs)
}

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: &Bound<'py, PyAny>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// A def statement: the function, made now, bound under its name like any
/// other name of the module (a name already there keeps its place).
fn define<'py>(m: &Bound<'py, PyModule>, key: &str, function: Bound<'py, PyCFunction>) -> PyResult<()> {
    store(m, key, function.as_any())
}

/// Whether a comprehension or a generator takes iter() a second time of the
/// iterator it loops over: Python 3.13's code does, 3.12's and 3.14's loop
/// over it as it is.
fn iter_again(py: Python<'_>) -> bool {
    let version = py.version_info();
    version >= (3, 13) && version < (3, 14)
}

/// `{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}`,
/// as Python runs it: MAIN_ACTION_DEFAULTS read now, iter() of its items
/// (taken twice on Python 3.13), each pair unpacked and compared, and the
/// same value objects kept.
fn without_reasoning<'py>(m: &Bound<'py, PyModule>) -> PyResult<Bound<'py, PyDict>> {
    let py = m.py();
    let items = name(m, "MAIN_ACTION_DEFAULTS")?.call_method0(intern!(py, "items"))?;
    let mut pairs = items.try_iter()?;
    if iter_again(py) {
        pairs = pairs.try_iter()?;
    }
    let kept = PyDict::new(py);
    for pair in pairs {
        let (k, v) = unpack2(&pair?)?;
        if k.ne(intern!(py, "reasoning"))? {
            kept.set_item(k, v)?;
        }
    }
    Ok(kept)
}

/// `frozenset(t["name"] for t in <tools>)`: frozenset read first, then the
/// tools, and iter() of them taken before frozenset is called, as Python
/// makes a generator expression.
fn name_set<'py>(m: &Bound<'py, PyModule>, tools: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = m.py();
    let frozenset = name(m, "frozenset")?;
    let tools = name(m, tools)?.try_iter()?;
    let names = Bound::new(py, Names { run: Run::Made(tools.unbind()) })?;
    frozenset.call1((names,))
}

// The generator expression `(t["name"] for t in <tools>)` a name set is
// made from, run as Python runs one. It is made holding iter(<tools>); on
// Python 3.13 its first next() takes iter() of that again (that version's
// generator code does), and each next() takes one tool and gives back
// t["name"]. A StopIteration raised inside becomes RuntimeError("generator
// raised StopIteration"), and once it has ended, at the end of the tools or
// by an error, it stays ended. It has the generator's type name, so a
// frozenset patched in a test sees what it saw before. A plain comment: a
// doc comment would give the class a docstring, and a generator has none.
#[pyclass(name = "generator", module = "builtins")]
struct Names {
    run: Run,
}

/// Where a Names generator is.
enum Run {
    /// Made, not run yet: it holds iter(<tools>).
    Made(Py<PyIterator>),
    /// Running: the iterator its loop takes the tools from.
    Looping(Py<PyIterator>),
    /// Ended.
    Ended,
}

#[pymethods]
impl Names {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        // Ended, unless a tool's name comes back.
        let mut tools = match std::mem::replace(&mut self.run, Run::Ended) {
            Run::Made(tools) if iter_again(py) => tools.bind(py).try_iter().map_err(|e| in_generator(py, e))?,
            Run::Made(tools) | Run::Looping(tools) => tools.into_bound(py),
            Run::Ended => return Ok(None),
        };
        let tool = match tools.next() {
            None => return Ok(None),
            Some(tool) => tool.map_err(|e| in_generator(py, e))?,
        };
        let value = tool.get_item(intern!(py, "name")).map_err(|e| in_generator(py, e))?;
        self.run = Run::Looping(tools.unbind());
        Ok(Some(value))
    }

    fn __repr__(slf: &Bound<'_, Self>) -> String {
        format!("<generator object <genexpr> at {:p}>", slf.as_ptr())
    }
}

/// An error leaving a generator's code: a StopIteration becomes
/// RuntimeError("generator raised StopIteration"), with the StopIteration
/// as its __cause__ and its __context__. Any other error goes on as it is.
fn in_generator(py: Python<'_>, e: PyErr) -> PyErr {
    if !e.is_instance_of::<PyStopIteration>(py) {
        return e;
    }
    let error = PyRuntimeError::new_err("generator raised StopIteration");
    error.set_cause(py, Some(e.clone_ref(py)));
    // SAFETY: two live exception instances; PyException_SetContext takes the
    // new reference into_ptr gives it.
    unsafe { ffi::PyException_SetContext(error.value(py).as_ptr(), e.value(py).clone().into_ptr()) };
    error
}

// ---------------------------------------------------------------------------
// class LLMManager(_LLMManager), as a class statement makes it
// ---------------------------------------------------------------------------

/// `class LLMManager(_LLMManager):` and its body, as Python's class
/// statement runs them (builtins.__build_class__): the base read now, a
/// base that is no class replaced by what its __mro_entries__ gives, the
/// metaclass worked out from the bases (no metaclass= is given), its
/// __prepare__ asked for the namespace, the body run in that namespace, and
/// the metaclass called with the name, the bases and the namespace.
fn class_statement<'py>(m: &Bound<'py, PyModule>) -> PyResult<Bound<'py, PyAny>> {
    let py = m.py();
    let class_name = intern!(py, "LLMManager");
    let orig_bases = PyTuple::new(py, [name(m, "_LLMManager")?])?;
    let bases = update_bases(&orig_bases)?;
    // The type of the first base (type when there is none), or a more
    // derived one among the bases' types.
    let first = match bases.len() {
        0 => py.get_type::<PyType>(),
        _ => bases.get_item(0)?.get_type(),
    };
    let meta = calculate_metaclass(first, &bases)?;
    let namespace = match meta.getattr_opt(intern!(py, "__prepare__"))? {
        Some(prepare) => prepare.call1((class_name, &bases))?,
        None => PyDict::new(py).into_any(),
    };
    // SAFETY: a live object.
    if unsafe { ffi::PyMapping_Check(namespace.as_ptr()) } == 0 {
        return Err(PyTypeError::new_err(format!(
            "{}.__prepare__() must return a mapping, not {}",
            upto_200_bytes(&own_type_name(&meta)?),
            upto_200_bytes(&type_name(&namespace)?)
        )));
    }

    // The body: each name it reads is looked up in the namespace first.
    let module_name = load_name(&namespace, m, intern!(py, "__name__"))?;
    namespace.set_item(intern!(py, "__module__"), module_name)?;
    namespace.set_item(intern!(py, "__qualname__"), class_name)?;
    if py.version_info() >= (3, 13) {
        namespace.set_item(intern!(py, "__firstlineno__"), CLASS_LINE)?;
    }
    namespace.set_item(intern!(py, "__doc__"), CLASS_DOC)?;
    for key in [
        intern!(py, "MAIN_TOOLS"),
        intern!(py, "MAIN_TOOLS_FAST"),
        intern!(py, "MAIN_TOOLS_CLAUDE"),
        intern!(py, "SCREEN_TOOLS"),
    ] {
        let value = load_name(&namespace, m, key)?;
        namespace.set_item(key, value)?;
    }
    if py.version_info() >= (3, 13) {
        // No method sets an attribute through self: the class has none.
        namespace.set_item(intern!(py, "__static_attributes__"), PyTuple::empty(py))?;
    }

    if !bases.is(&orig_bases) {
        namespace.set_item("__orig_bases__", &orig_bases)?;
    }
    meta.call1((class_name, bases, namespace))
}

/// The bases as a class statement takes them: a base that is no class but
/// has __mro_entries__ is replaced by the tuple that gives (called with the
/// bases as written). The same tuple when nothing is replaced.
fn update_bases<'py>(orig_bases: &Bound<'py, PyTuple>) -> PyResult<Bound<'py, PyTuple>> {
    let py = orig_bases.py();
    let mut replaced: Option<Vec<Bound<'py, PyAny>>> = None;
    for (index, base) in orig_bases.iter().enumerate() {
        if !base.is_instance_of::<PyType>() {
            if let Some(entries) = base.getattr_opt(intern!(py, "__mro_entries__"))? {
                let entries = entries.call1((orig_bases,))?;
                let entries = entries
                    .cast::<PyTuple>()
                    .map_err(|_| PyTypeError::new_err("__mro_entries__ must return a tuple"))?;
                replaced.get_or_insert_with(|| orig_bases.iter().take(index).collect()).extend(entries.iter());
                continue;
            }
        }
        if let Some(kept) = &mut replaced {
            kept.push(base);
        }
    }
    match replaced {
        Some(kept) => PyTuple::new(py, kept),
        None => Ok(orig_bases.clone()),
    }
}

/// The metaclass a class statement uses when none is given: of `first` and
/// the bases' types, the one that is a subclass of all the others, or
/// Python's TypeError when there is none.
fn calculate_metaclass<'py>(first: Bound<'py, PyType>, bases: &Bound<'py, PyTuple>) -> PyResult<Bound<'py, PyType>> {
    // SAFETY: two live type objects.
    let subtype = |a: &Bound<'py, PyType>, b: &Bound<'py, PyType>| unsafe { ffi::PyType_IsSubtype(a.as_type_ptr(), b.as_type_ptr()) != 0 };
    let mut winner = first;
    for base in bases.iter() {
        let kind = base.get_type();
        if subtype(&winner, &kind) {
            continue;
        }
        if subtype(&kind, &winner) {
            winner = kind;
            continue;
        }
        return Err(PyTypeError::new_err(
            "metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases",
        ));
    }
    Ok(winner)
}

/// A name the class body reads (LOAD_NAME): the class namespace first (a
/// dict looked up as a dict, any other mapping by `[]`, KeyError meaning
/// not there), then the module's globals and the builtins.
fn load_name<'py>(namespace: &Bound<'py, PyAny>, m: &Bound<'py, PyModule>, key: &Bound<'py, PyString>) -> PyResult<Bound<'py, PyAny>> {
    let py = m.py();
    let found = match namespace.cast_exact::<PyDict>() {
        Ok(dict) => dict.get_item(key)?,
        Err(_) => match namespace.get_item(key) {
            Ok(value) => Some(value),
            Err(e) if e.is_instance_of::<PyKeyError>(py) => None,
            Err(e) => return Err(e),
        },
    };
    match found {
        Some(value) => Ok(value),
        None => name(m, key.to_str()?),
    }
}

/// A class's own name as CPython's error texts give it (its tp_name): the
/// name of a class made in Python, the dotted C name of a built-in type
/// (crate::computer::type_name gives this for the type of a value).
fn own_type_name(kind: &Bound<'_, PyType>) -> PyResult<String> {
    let name: String = kind.getattr("__name__")?.extract()?;
    // SAFETY: a live type object.
    let flags = unsafe { ffi::PyType_GetFlags(kind.as_type_ptr()) };
    if flags & ffi::Py_TPFLAGS_HEAPTYPE != 0 {
        return Ok(name);
    }
    let module: String = kind.getattr("__module__")?.extract()?;
    Ok(if module == "builtins" { name } else { format!("{module}.{name}") })
}

/// `%.200s` in CPython's error texts: at most 200 bytes of the text, a
/// character cut in two shown as U+FFFD.
fn upto_200_bytes(text: &str) -> String {
    let bytes = text.as_bytes();
    String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).into_owned()
}

// ---------------------------------------------------------------------------
// The module's functions
// ---------------------------------------------------------------------------

/// The MAIN DRIVER's registry - one tool per action type. route_action and
/// the frontend's tool-flow map key on these names and fields, so they must
/// never drift.
///
/// Descriptions are the system prompt's OWN tool text, copied verbatim from
/// <Tool_Capability> / <os_interaction> / <task_completion> - the prompt stays
/// the source of truth (it is user-managed and never edited from here); this
/// just puts the same words where a native tool call can read them. Built
/// twice, each with NO per-call params (`track` stays {}) and a dedicated
/// notes tool in front: quality mode the `reasoning` tool (thinking optional,
/// memory + next_goal every step), fast mode the `memory` tool (`next_goal`
/// alone).
///
/// macOS-specific vs the Windows registry: `input`/`typewrite` carry `value`
/// (not `text`), and `applescript` (app + value) and `drag_drop` exist here
/// only. `clicks` stays on right_click/screenshot even though the controller
/// ignores it, because the old strict union required it.
///
/// While the dataset recorder is on, live_tools withholds the tools a
/// recorded run must not teach (data_extraction.OFF_TOOLS).
#[pyfunction]
#[pyo3(pass_module, name = "_main_tools", signature = (track, notes = None), text_signature = "(track, notes=None)")]
fn main_tools<'py>(
    m: &Bound<'py, PyModule>,
    track: Bound<'py, PyAny>,
    notes: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = m.py();
    // notes = [notes] if notes else []
    let notes = match notes {
        Some(notes) if notes.is_truthy()? => PyList::new(py, [notes])?,
        _ => PyList::empty(py),
    };
    // return data_extraction.live_tools(notes + [...]): data_extraction and
    // its live_tools are read first, as Python evaluates the call.
    let live_tools = name(m, "data_extraction")?.getattr(intern!(py, "live_tools"))?;
    // _tool(<name>, {<params>}, <description>, track=track), in Python's
    // order: _tool read when its call comes (as a function reads a global),
    // then the params, then the description.
    let tool = |tool_name: &str,
                params: &dyn Fn() -> PyResult<Bound<'py, PyDict>>,
                description: Desc<'_, 'py>|
     -> PyResult<Bound<'py, PyAny>> {
        let make = name(m, "_tool")?;
        let params = params()?;
        let description = match description {
            Desc::Constant(constant) => constant.clone().into_any(),
            // f'...({", ".join(SCREEN_TOOLS)})...': SCREEN_TOOLS read now.
            Desc::ScreenTools(before, after) => {
                let joined = PyString::new(py, ", ").call_method1(intern!(py, "join"), (name(m, "SCREEN_TOOLS")?,))?;
                fstring(py, &[&text(py, before), &joined, &text(py, after)])?
            }
        };
        make.call((PyString::intern(py, tool_name), params, description), Some(&keyword(py, "track", &track)?))
    };
    let actions = PyList::new(py, [
        // -- <Tool_Capability> ------------------------------------------------
        tool("open_app", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Launch an installed application (.app only). No manual search required within the OS.\n",
                "    1. Requirement: Typically call wait 3 seconds immediately after this tool to allow loading.\n",
                "    3. Example: open_app {\"value\": \"spotify\"}",
            ))))?,
        tool("wait", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Pause execution to allow UI loading or to trigger a fresh screen scan.\n",
                "    2. Example: wait {\"value\": \"2\"}",
            ))))?,
        tool("scan", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any()), ("enum", PyList::new(py, [intern!(py, "on"), intern!(py, "off")])?.as_any())])?.as_any())])?), Desc::ScreenTools(concat!(
                "Switch the screen scan on or off. The scan is on by default, and every new user request starts with it on. A switch takes effect from the next step.\n",
                "    1. off: use it when the next steps only run through `shell` (zsh commands) and the other tools that need no screen (web, sub-agents, scratchpad, todo). Every scanned step sends a screenshot and an <element_tree>; with scan off neither is sent, which cuts the input tokens and saves money, and the screen tools (",
            ), concat!(
                ") are removed. Turn it off once your last screen action was judged on a screenshot.\n",
                "    2. Keep it off for as long as the following steps do not need the screen.\n",
                "    3. on: switch it on in the step before one that needs the screen (a click, a key, a visual check). From the next step the screenshot, the <element_tree> and every tool are back.\n",
                "    4. Example: scan {\"value\": \"off\"}",
            )))?,
        tool("web", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Delegate to a specialized AI to fetch real-time information and provide data at runtime. Use this for speed instead of manual browsing.\n",
                "    2. Example: web {\"value\": \"financial result of nvidia Q4 2025\"}",
            ))))?,
        tool("sub_agent", &|| Ok(display(py, &[("agent_type", display(py, &[("type", intern!(py, "string").as_any()), ("enum", PyList::new(py, [intern!(py, "coder_agent"), intern!(py, "browser_agent"), intern!(py, "ios_agent"), intern!(py, "android_agent")])?.as_any())])?.as_any()), ("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Start a sub-agent on a task. It works in parallel while you carry on. It takes the first empty slot of its type in <sub_agents>, and the result names its id. When every slot of its type is busy, nothing starts: wait for one of them to finish (agent_wait).\n",
                "    1. `agent_type`:\n",
                "      1. `coder_agent`: complex coding and multi-step shell work. Up to 4 at once (ids 1-4).\n",
                "      2. `browser_agent`: acts on websites in its own Chrome browser (navigate, click, fill forms). Up to 4 at once (ids 5-8), each in a tab of its own. Its Chrome window opens in front of your screen. Check <sub_agents>: while a browser_agent reads running, never touch that window, and bring your own app forward with open_app. It runs on the agent's own Chrome profile, not the person's browser, so the person's signed-in accounts are not there: buying and anything that needs their accounts stay with you.\n",
                "      3. `ios_agent`: works on the person's real iPhone or iPad, plugged into this Mac by cable: opens its apps, taps, types, scrolls and reads its screen. One slot: id 9 or 10, as <sub_agents> shows it. Only while it reads empty; `connecting` means the phone is still being connected.\n",
                "      4. `android_agent`: works on the person's Android phone, plugged into this Mac by USB: opens its apps, taps, types, scrolls and reads its screen. One slot: id 9 or 10, as <sub_agents> shows it. Only while it reads empty; `connecting` means the phone is still being checked.\n",
                "    2. `value`: the full instruction. It is all the sub-agent gets, so make it self-contained.\n",
                "    3. Example: sub_agent {\"agent_type\": \"coder_agent\", \"value\": \"instruction\"}",
            ))))?,
        tool("agent_wait", &|| Ok(display(py, &[("agent_type", display(py, &[("type", intern!(py, "string").as_any()), ("enum", PyList::new(py, [intern!(py, "coder_agent"), intern!(py, "browser_agent"), intern!(py, "ios_agent"), intern!(py, "android_agent")])?.as_any())])?.as_any()), ("agent_id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Hold the pipeline until one sub-agent finishes. Use it only when your next step needs its report, when every slot of the type you need is busy, or before done.\n",
                "    1. `agent_type` and `agent_id`: the sub-agent to hold for, as its slot in <sub_agents> shows them.\n",
                "    2. If another sub-agent finishes first, the hold ends with that report; call agent_wait again to keep holding.\n",
                "    3. Example: agent_wait {\"agent_type\": \"coder_agent\", \"agent_id\": 1, \"value\": \"Reason\"}",
            ))))?,
        // shell point 2, click by point with no mouse: the osascript (JXA) line
        // converts 0-1000 to the scanner's display (as tree/element.py
        // get_screen), then System Events clicks there, pressing the element at
        // that point. click() needs the null. No drag: without the mouse there
        // is none. Tested live on Chess, 2026-11-01.
        tool("shell", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Run a shell/zsh command for fast execution to achieve the goal.\n",
                "    1. Example:\n",
                "      1. shell {\"value\": \"rm -rf ~/.Trash/*\"}\n",
                "      2. shell {\"value\": \"osascript -e 'tell application \\\"Reminders\\\"\\nset dueDate to current date\\nset year of dueDate to 2026\\nset month of dueDate to 4\\nset day of dueDate to 25\\nset hours of dueDate to 6\\nset minutes of dueDate to 0\\nset seconds of dueDate to 0\\nset newReminder to make new reminder with properties {name:\\\"Catch my flight\\\", due date:dueDate, remind me date:dueDate}\\nreturn name of newReminder\\nend tell'\"}\n",
                "    2. Click by point at the OS level, for when the scan is wrong. AppleScript's System Events presses the element the app reports at that point, in any app or layer, without the mouse, so it does not depend on the scan and it is reliable.\n",
                "      1. When: the target is visible in the image but has no [id], its [id] box is not on it, or its name in <element_tree> does not match what the image shows there (e.g., a chess square named as another square). Otherwise click by [id].\n",
                "      2. Points: the CENTER of the target on the 0-1000 scale of the screenshot (0,0 top-left, 1000,1000 bottom-right). The line converts them to the screen itself: never pass image pixels or screen points.\n",
                "      3. Copy the line exactly and change only the calls at its end: click(x,y) clicks once; put delay(0.5) between several clicks (e.g., a piece, then its square, to move it).\n",
                "      4. No mouse: the cursor stays where it is, and the app does not have to be in front. The output names what was pressed last (e.g., ...buttons.byName(\"e4\")): check it is your target.\n",
                "      5. If the next screenshot shows no change, the app has nothing to press at that point (e.g., a drawing canvas): use keys or the menu instead.\n",
                "      6. The first use may ask to let AutoCua control System Events: accept it, then rerun.\n",
                "      7. Judge it on the next screenshot by the target itself (the piece on its new square, the field showing the value), never by a side sign such as the window title.\n",
                "      8. Click a piece, then its square: shell {\"value\": \"osascript -l JavaScript -e 'ObjC.import(`AppKit`);ObjC.import(`CoreGraphics`);var d=$.CGMainDisplayID();ObjC.unwrap($.NSScreen.screens).forEach(function(s){var n=ObjC.unwrap(s.deviceDescription.objectForKey(`NSScreenNumber`));if($.CGDisplayIsBuiltin(n))d=n});var r=$.CGDisplayBounds(d),se=Application(`System Events`);function click(x,y){return se.click(null,{at:[Math.round(r.origin.x+x*r.size.width/1000),Math.round(r.origin.y+y*r.size.height/1000)]})}click(426,595);delay(0.5);click(427,512)'\"}",
            ))))?,
        tool("applescript", &|| Ok(display(py, &[("app", display(py, &[("type", intern!(py, "string").as_any())])?.as_any()), ("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Run a complete AppleScript on any macOS app. Wrap in `tell application \"X\" ... end tell`. Do NOT include `activate`/`launch` - runtime handles activation. End with `return` for verification.\n",
                "    1. Safari: use `make new tab` (never `make new document`); always `set current tab to newTab`.\n",
                "    2. Example: applescript {\"app\": \"Safari\", \"value\": \"tell application \\\"Safari\\\"\\n  tell front window to set newTab to make new tab with properties {URL:\\\"https://youtube.com\\\"}\\n  tell front window to set current tab to newTab\\nend tell\"}",
            ))))?,
        tool("todo_list", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, "Create the ToDo task list (iteration 1 by default; you may also create/expand it later if complexity emerges). See <todo_capability>.")))?,
        tool("update_todo", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Tasks are auto-numbered #1, #2, #3, etc. when saved.\n",
                "    1. Update (only after confirmed complete via <agent_history> and the effect is visible in the latest input - image or any relevant tag; one item per call)\n",
                "    2. Example: update_todo {\"value\": \"1\"}",
            ))))?,
        tool("scratchpad", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Record a verified checkpoint or any critical fact (file path, metric, finding). Follow <scratchpad> rules.\n",
                "    1. Write `value` in Markdown - inline only (`**bold**`, backticks), never a line break.\n",
                "    2. Example: scratchpad {\"value\": \"**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**\"}",
            ))))?,

        // -- <os_interaction> -------------------------------------------------
        tool("left_click", &|| Ok(display(py, &[("id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("clicks", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "left mouse click. clicks=1: single click, clicks=2: double click (open files/folders), clicks=3: triple click (OCR_TEXT).\n",
                "    1. Example: left_click {\"id\": 8, \"clicks\": 2}\n",
                "    2. Sequence example: left_click {\"id\": 9, \"clicks\": 1}, left_click {\"id\": 10, \"clicks\": 1}",
            ))))?,
        tool("right_click", &|| Ok(display(py, &[("id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("clicks", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "right mouse click, open context menu/options.\n",
                "    1. Example: right_click {\"id\": 9 , \"clicks\": 1}",
            ))))?,
        tool("input", &|| Ok(display(py, &[("id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "replace an element's text with `value`: the field is cleared, then the whole string is inserted at once, exactly as given.\n",
                "    1. Always overwrites, never appends, so do not clear the field yourself first.\n",
                "    2. `enter` must be sent separately when needed (e.g., email 'From', 'To', 'Search' fields).\n",
                "      1. Scenario: input + enter + input.\n",
                "    3. If the text does not appear (a field that blocks paste, or one box per character), left_click the field and use typewrite instead.\n",
                "    4. Example: input {\"id\": 9, \"value\": \"hi, how are you\"}",
            ))))?,
        tool("typewrite", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "type into the currently focused area when no element is available.\n",
                "    1. Does not auto-delete; use backspace if needed.\n",
                "    2. Example: typewrite {\"value\": \"hi, how are you\"}",
            ))))?,
        tool("scroll", &|| Ok(display(py, &[("id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("direction", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "scroll an element in a direction (`up/down/left/right`).\n",
                "    1. Example: scroll {\"id\": 9, \"direction\": \"up\"}",
            ))))?,
        tool("hotkey", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "OS hotkeys (max 3 keys pairs). Applies to `<Front_screen>`.\n",
                "    1. Use only for OS-level shortcut combinations (e.g., `cmd+c`, `cmd+q`, `cmd+down`).\n",
                "    2. Examples:\n",
                "        1. hotkey {\"value\": \"enter\"}\n",
                "        2. hotkey {\"value\": \"cmd+shift+s\"}",
            ))))?,
        tool("screenshot", &|| Ok(display(py, &[("id", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any()), ("clicks", display(py, &[("type", intern!(py, "integer").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Capture a UI element part as an image and copy it to the clipboard for pasting elsewhere.\n",
                "    1. It takes a screenshot without annotation, so do not trigger it to capture the magenta element number.\n",
                "    2. Image is ready to paste with cmd+v. The clicks field is a dummy (always 1).\n",
                "    3. Example: screenshot {\"id\": 15, \"clicks\": 1}",
            ))))?,
        tool("drag_drop", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "click-hold on one element and release on another (drag and drop).\n",
                "    1. Format: drag_drop {\"value\": \"<from_id> to <to_id>\"}\n",
                "    2. Example: drag_drop {\"value\": \"8 to 15\"}",
            ))))?,

        // -- <task_completion> (the only place `done` is documented) -----------
        tool("done", &|| Ok(display(py, &[("value", display(py, &[("type", intern!(py, "string").as_any())])?.as_any())])?), Desc::Constant(constant!(py, concat!(
                "Use `done` as a dedicated final step only, after reviewing <agent_history> to confirm every requested task is finished and doing a final visual verification from the latest image.\n",
                "    1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.\n",
                "    2. Step 2: output ONLY Format: done {\"value\": \"<end-to-end summary in markdown format>\"}\n",
                "    3. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.\n",
                "    4. Never combine `done` with any other action/tool in the same step.\n",
                "    5. While scan is off there is no image: base the final verification on the latest tool results.",
            ))))?,
    ])?;
    live_tools.call1((notes.add(actions)?,))
}

/// A tool's description: a constant of _main_tools, or the scan tool's
/// f-string around ", ".join(SCREEN_TOOLS).
enum Desc<'a, 'py> {
    Constant(&'a Bound<'py, PyString>),
    ScreenTools(&'static str, &'static str),
}

/// Claude's `agent_memory` notes tool. `final` is the agent's closing tool (always
/// the only call of its turn); `work` names its action calls.
///
/// `batched` (every mac agent): the opening sentences say the call never ends a
/// turn. Claude treats a notes tool as a think step - it calls it, ends the
/// turn to wait for the result and acts on the next turn - unless told the
/// actions belong in the SAME message. Measured 2026-10-05 on the driver's
/// step 1 (Sonnet 5): notes+action in one turn 0/5 before, 5/6 with this
/// wording plus the matching <blocks> rule in claude_prompt.md and the per-turn
/// reminder the driver appends to its live message. Keep the name: a renamed
/// tool (`step_notes`) was refused by Sonnet 5.5 / Opus 5 as reasoning
/// extraction.
#[pyfunction]
#[pyo3(
    pass_module,
    name = "_claude_reasoning_tool",
    signature = (r#final, work = Given::Missing, batched = Given::Missing),
    text_signature = "(final, work='calls', batched=False)"
)]
fn claude_reasoning_tool<'py>(
    m: &Bound<'py, PyModule>,
    r#final: Bound<'py, PyAny>,
    work: Given<'py>,
    batched: Given<'py>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = m.py();
    let work = match work {
        Given::Value(work) => work,
        Given::Missing => intern!(py, "calls").clone().into_any(),
    };
    // if batched: (the default, False, is false with nothing to ask)
    let batched = match batched {
        Given::Value(batched) => batched.is_truthy()?,
        Given::Missing => false,
    };
    let opening = if batched {
        fstring(py, &[
            &text(py, concat!(
                "Your written notes for this step: a log entry, not an action. It has no result and nothing waits on ",
                "it, so it NEVER ends a turn: emit it first in the list and the ",
            )),
            &work,
            &text(py, concat!(
                " that do this step's \"Doing\" ",
                "in the SAME message. A message holding only this call and no action is a wasted turn. The call is ",
                "replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n",
            )),
        ])?
    } else {
        fstring(py, &[
            &text(py, "Your notes to yourself for this step. Call it FIRST in every step, then the "),
            &work,
            &text(py, concat!(
                " that do the work, ",
                "all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is ",
                "replayed in <agent_history>, so your future self can read what you knew and decided at every step.\n",
            )),
        ])?
    };
    let make = name(m, "_tool")?;
    let fields = name(m, "CLAUDE_REASONING_FIELDS")?;
    let rules = fstring(py, &[
        &text(py, concat!(
            "- `memory` and `next_goal` are filled every step: `memory` follows <memory>, `next_goal` follows <next_goal>.\n",
            "- Not used on the `",
        )),
        &r#final,
        &text(py, "` step: `"),
        &r#final,
        &text(py, concat!(
            "` is always the only call of its turn.\n",
            "- Format: agent_memory {\"memory\": \"...\", \"next_goal\": \"...\"}",
        )),
    ])?;
    make.call1((intern!(py, "agent_memory"), fields, opening.add(rules)?))
}

/// Runs the lines of service.py after its import line, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build<'py>(m: &Bound<'py, PyModule>) -> PyResult<()> {
    let py = m.py();
    // service.py's two functions come from its def statements further down,
    // so until then its module does not have them. register() gave this
    // module its own, and those are taken out here: each def below binds a
    // new one where service.py bound it.
    let globals = m.dict();
    for key in [intern!(py, "_main_tools"), intern!(py, "_claude_reasoning_tool")] {
        if globals.contains(key)? {
            globals.del_item(key)?;
        }
    }

    // ---------------------------------------------------------------------------
    // NATIVE TOOLS: the main driver calls these tools natively; no JSON envelope.
    // The provider returns structured calls, tool_calls_to_steps() converts them
    // into the same `[{type, ...}]` action dicts route_action already consumes,
    // and the controller is untouched.
    //
    // In quality mode a `reasoning` tool carries the step's notes (thinking,
    // memory, next_goal) and every action tool carries only its own fields; in
    // fast mode a `memory` tool carries `next_goal` alone (see
    // MAIN_NOTES_FIELDS_FAST). tool_choice is forced (required / {"type": "any"} /
    // mode="ANY"), so a text-only step is unrepresentable; the text channel remains
    // for free prose only.
    // ---------------------------------------------------------------------------

    // Fast mode's notes tool is named `memory` and carries ONE field: `next_goal`.
    // thinking and the quality `memory` field are dropped ENTIRELY rather than left
    // required-but-empty - a required field is an invitation to fill it, and every
    // token spent narrating a plan is a token not spent on the action, which is
    // the whole point of fast mode. next_goal alone carries the verdict, the forward
    // plan (Now / Plan / Then) and the Expect guard.
    let fields = display(py, &[("next_goal", note_field(py, "Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (name/type/valuePattern.value/active), and any tool name + purpose + important result. Then the forward plan: \"Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>.\" END with the predicted visible change of THIS step's action prefixed \"Expect:\", so the next step can verify against the new screenshot. 3-5 concise lines.")?.as_any())])?;
    store(m, "MAIN_NOTES_FIELDS_FAST", &fields)?;

    // The MAIN DRIVER's `reasoning` tool fields (quality mode; fast mode's
    // `memory` tool carries `next_goal` alone, see MAIN_NOTES_FIELDS_FAST): the
    // coder's design in screen terms. The verdict is judged on the CURRENT
    // screenshot, targets are named by name/role in `next_goal` and locked to
    // fresh [id]s in `memory`'s Targets line every step, and the guard is the
    // visible change the next screenshot must show. No S<n> codes, no "not
    // required": a route step omits `thinking` and the snapshot shows "skipped".
    let fields = display(py, &[
        ("thinking", note_field(py, "Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: \"JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ...\" (FULL) when there is no route, it is used up, or a new surface appeared; a short freeform paragraph (RECOVERY) after a guard failed on the screenshot; 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass \"\") on a route step: the previous \"Next:\" named this action, its visible-change guard holds on the current screenshot, and every named target resolves to exactly one [id].")?.as_any()),
        ("memory", note_field(py, "Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT screenshot, never on what a tool result claims: \"Last step worked: <what the screen shows>\", \"Last step failed: <what the screen shows instead>\" or \"Last step unclear: <what is ambiguous, and what would settle it>\". First step: \"Task start.\" Then the key context (app/screen state; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: \"Targets: id N (name/type/valuePattern.value/active), ...\" resolved from the CURRENT <element_tree> before acting. 2-4 tight lines.")?.as_any()),
        ("next_goal", note_field(py, "Follow <next_goal>: \"Doing: <this step> (ToDo: <task_name>). If <visible change the next screenshot must show> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>.\" Never name a successor target by [id]: ids are re-assigned every scan. On a route step, \"Doing\" is the next line of the ROUTE and \"Next:\" the line after it.")?.as_any()),
    ])?;
    store(m, "MAIN_REASONING_FIELDS", &fields)?;

    // MAIN_REASONING_TOOL = _tool("reasoning", MAIN_REASONING_FIELDS, r"""...""",
    //                             optional=REASONING_OPTIONAL)
    let make = name(m, "_tool")?;
    let fields = name(m, "MAIN_REASONING_FIELDS")?;
    let optional = name(m, "REASONING_OPTIONAL")?;
    let description = PyString::new(py, concat!(
        "Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.\n",
        "- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its visible-change guard; on each step of that route omit `thinking` (or pass \"\"), and think again only when the route is used up, a guard fails on the screenshot, a new surface appears, or the route said \"think\". A screenshot that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.\n",
        "- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is only partially visible, means this step thinks.\n",
        "- A step may be `reasoning` alone when you need to think before you act (it costs a fresh screen scan); the next step then does the work. Do not chain several such steps.\n",
        "- Not used on the `done` step: `done` is always the only call of its turn.\n",
        "- Format: reasoning {\"thinking\": \"\", \"memory\": \"...\", \"next_goal\": \"...\"}\n",
        "- Examples:\n",
        "  1. Route step (no thinking; \"Doing\" is the next route line):\n",
        "     reasoning {\"memory\": \"Last step worked: Mail front, the message list showing as predicted. Targets: id 31 (name='Search', type='TextField', active='True').\", \"next_goal\": \"Doing: search Mail for the invoice (ToDo: Find the invoice). If the message list shows only messages matching 'invoice' → Next: think: pick the message to open.\"}\n",
        "  2. New surface (FULL thinking, ends with the ROUTE):\n",
        "     reasoning {\"thinking\": \"JUDGE: PASS, the Gmail compose window is open as predicted, To field empty. UNDERSTAND: known: To, Subject and body are in the tree as text fields; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. click Send, if the compose window closes and 'Message sent' appears → think: verify the sent mail, then mark the ToDo.\", \"memory\": \"Last step worked: compose window open, guard held. Targets: id 12 (name='To', type='TextField'), id 14 (name='Subject', type='TextField'), id 20 (name='Message Body', type='TextArea').\", \"next_goal\": \"Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: click the Send button.\"}\n",
        "  3. Recovery after a failed guard:\n",
        "     reasoning {\"thinking\": \"Failed: the To field is still empty after input on id 74; the screenshot shows the cursor in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: click the To field first, then input. Guard: the To field shows the address. Route holds after this.\", \"memory\": \"Last step failed: To field empty, focus was in Subject. Correction carried: click a field before typing into it in this compose window. Targets: id 74 (name='To', type='TextField').\", \"next_goal\": \"Doing: click the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject.\"}",
    ));
    let made = make.call((intern!(py, "reasoning"), fields, description), Some(&keyword(py, "optional", optional)?))?;
    store(m, "MAIN_REASONING_TOOL", &made)?;

    // MAIN_NOTES_TOOL_FAST = _tool("memory", MAIN_NOTES_FIELDS_FAST, r"""...""")
    let make = name(m, "_tool")?;
    let fields = name(m, "MAIN_NOTES_FIELDS_FAST")?;
    let description = PyString::new(py, concat!(
        "Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.\n",
        "- One field, `next_goal`, filled every step: key context, the forward plan (Now / Plan / Then) and the Expect guard the next step verifies against (see <next_goal>). There is no thinking field: the reasoning stays silent (see <silent_reasoning>).\n",
        "- Every step resolves the plan's targets to fresh [id]s from the CURRENT <element_tree> and names them in `next_goal` before acting.\n",
        "- Not used on the `done` step: `done` is always the only call of its turn.\n",
        "- Format: memory {\"next_goal\": \"...\"}\n",
        "- Example:\n",
        "  memory {\"next_goal\": \"Spotify front, Search page open; search field id 44 (name='Search', type='TextField', active='True'). Now: search for the focus playlist (ToDo: Play focus playlist). Plan: pick the playlist -> play it. Then: open the best-matching playlist. Expect: Spotify lists focus playlists in its results.\"}",
    ));
    let made = make.call1((intern!(py, "memory"), fields, description))?;
    store(m, "MAIN_NOTES_TOOL_FAST", &made)?;

    // The tools that act on the screen. While the main driver's scan is off
    // (the `scan` tool) they are taken out of the tool list sent to the model and
    // out of the names it may call, and they come back with scan on.
    let screen_tools = PyTuple::new(py, [intern!(py, "left_click"), intern!(py, "right_click"), intern!(py, "input"), intern!(py, "typewrite"), intern!(py, "scroll"), intern!(py, "hotkey"), intern!(py, "screenshot"), intern!(py, "drag_drop"), intern!(py, "open_app"), intern!(py, "applescript")])?;
    store(m, "SCREEN_TOOLS", screen_tools.as_any())?;

    // def _main_tools(track: dict, notes: dict = None) -> list:
    define(m, "_main_tools", wrap_pyfunction!(main_tools, m)?)?;

    // MAIN_TOOLS = _main_tools({}, notes=MAIN_REASONING_TOOL)
    // MAIN_TOOLS_FAST = _main_tools({}, notes=MAIN_NOTES_TOOL_FAST)
    for (key, notes) in [("MAIN_TOOLS", "MAIN_REASONING_TOOL"), ("MAIN_TOOLS_FAST", "MAIN_NOTES_TOOL_FAST")] {
        let make = name(m, "_main_tools")?;
        let track = PyDict::new(py);
        let notes = name(m, notes)?;
        let made = make.call((track,), Some(&keyword(py, "notes", notes)?))?;
        store(m, key, &made)?;
    }

    // Per-action defaults for the main driver - guarantees route_action always
    // receives every field of an action, even one the model left out.
    let empty = PyString::new(py, "").into_any();
    let zero = 0i32.into_pyobject(py)?.into_any();
    let one = 1i32.into_pyobject(py)?.into_any();
    let defaults = display(py, &[
        ("reasoning", display(py, &[("thinking", &empty), ("memory", &empty), ("next_goal", &empty)])?.as_any()),
        ("left_click", display(py, &[("id", &zero), ("clicks", &one)])?.as_any()),
        ("right_click", display(py, &[("id", &zero), ("clicks", &one)])?.as_any()),
        ("screenshot", display(py, &[("id", &zero), ("clicks", &one)])?.as_any()),
        ("input", display(py, &[("id", &zero), ("value", &empty)])?.as_any()),
        ("typewrite", display(py, &[("value", &empty)])?.as_any()),
        ("scroll", display(py, &[("id", &zero), ("direction", intern!(py, "down").as_any())])?.as_any()),
        ("drag_drop", display(py, &[("value", &empty)])?.as_any()),
        ("applescript", display(py, &[("app", &empty), ("value", &empty)])?.as_any()),
        ("hotkey", display(py, &[("value", &empty)])?.as_any()),
        ("open_app", display(py, &[("value", &empty)])?.as_any()),
        ("wait", display(py, &[("value", &empty)])?.as_any()),
        ("scan", display(py, &[("value", &empty)])?.as_any()),
        ("web", display(py, &[("value", &empty)])?.as_any()),
        ("shell", display(py, &[("value", &empty)])?.as_any()),
        ("sub_agent", display(py, &[("agent_type", intern!(py, "coder_agent").as_any()), ("value", &empty)])?.as_any()),
        ("agent_wait", display(py, &[("agent_type", intern!(py, "coder_agent").as_any()), ("agent_id", &zero), ("value", &empty)])?.as_any()),
        ("todo_list", display(py, &[("value", &empty)])?.as_any()),
        ("update_todo", display(py, &[("value", &empty)])?.as_any()),
        ("scratchpad", display(py, &[("value", &empty)])?.as_any()),
        ("done", display(py, &[("value", &empty)])?.as_any()),
    ])?;
    store(m, "MAIN_ACTION_DEFAULTS", &defaults)?;

    // Fast mode's notes tool is `memory` (next_goal alone) instead of `reasoning`;
    // tool_calls_to_steps reads the defaults map to find the mode's notes tool.
    let fast = display(py, &[("memory", display(py, &[("next_goal", &empty)])?.as_any())])?;
    fast.update(without_reasoning(m)?.as_mapping())?;
    store(m, "MAIN_ACTION_DEFAULTS_FAST", &fast)?;

    store(m, "MAIN_TOOL_NAMES", &name_set(m, "MAIN_TOOLS")?)?;
    store(m, "MAIN_TOOL_NAMES_FAST", &name_set(m, "MAIN_TOOLS_FAST")?)?;

    // ---------------------------------------------------------------------------
    // CLAUDE: any Claude model, whatever the provider (anthropic direct, or a Claude
    // entry on OpenRouter; see LLMManager.claude). Every agent runs on
    // its own claude_prompt.md (the main driver in quality mode only) with its own
    // notes tool, `agent_memory`: `memory` and `next_goal`, both filled every step.
    // The Claude prompts have no <assessment> block and no thinking field, so
    // both fields are required (no REASONING_OPTIONAL here).
    // ---------------------------------------------------------------------------
    let fields = display(py, &[
        ("memory", note_field(py, "Follow <memory>. Filled every step.")?.as_any()),
        ("next_goal", note_field(py, "Follow <next_goal>. Filled every step.")?.as_any()),
    ])?;
    store(m, "CLAUDE_REASONING_FIELDS", &fields)?;

    // def _claude_reasoning_tool(final: str, work: str = "calls", batched: bool = False) -> dict:
    define(m, "_claude_reasoning_tool", wrap_pyfunction!(claude_reasoning_tool, m)?)?;

    store(m, "_CLAUDE_REASONING_DEFAULTS", display(py, &[("memory", &empty), ("next_goal", &empty)])?.as_any())?;

    // MAIN_TOOLS_CLAUDE = _main_tools({}, notes=_claude_reasoning_tool("done", batched=True))
    let make = name(m, "_main_tools")?;
    let track = PyDict::new(py);
    let claude = name(m, "_claude_reasoning_tool")?;
    let notes = claude.call((intern!(py, "done"),), Some(&keyword(py, "batched", true)?))?;
    let made = make.call((track,), Some(&keyword(py, "notes", notes)?))?;
    store(m, "MAIN_TOOLS_CLAUDE", &made)?;

    // MAIN_ACTION_DEFAULTS_CLAUDE = {"agent_memory": dict(_CLAUDE_REASONING_DEFAULTS),
    //                                **{k: v for k, v in MAIN_ACTION_DEFAULTS.items() if k != "reasoning"}}
    let copy = name(m, "dict")?;
    let claude_defaults = name(m, "_CLAUDE_REASONING_DEFAULTS")?;
    let claude = display(py, &[("agent_memory", &copy.call1((claude_defaults,))?)])?;
    claude.update(without_reasoning(m)?.as_mapping())?;
    store(m, "MAIN_ACTION_DEFAULTS_CLAUDE", &claude)?;
    store(m, "MAIN_TOOL_NAMES_CLAUDE", &name_set(m, "MAIN_TOOLS_CLAUDE")?)?;

    // class LLMManager(_LLMManager):
    //     """The shared LLMManager with this platform's tool lists."""
    //     MAIN_TOOLS = MAIN_TOOLS
    //     MAIN_TOOLS_FAST = MAIN_TOOLS_FAST
    //     MAIN_TOOLS_CLAUDE = MAIN_TOOLS_CLAUDE
    //     SCREEN_TOOLS = SCREEN_TOOLS
    let class = class_statement(m)?;
    store(m, "LLMManager", &class)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with what never changes:
/// the docstring, _main_tools and _claude_reasoning_tool, and _build, which
/// the loader calls once it has bound the import line's names (see the top
/// of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_function(wrap_pyfunction!(main_tools, m)?)?;
    m.add_function(wrap_pyfunction!(claude_reasoning_tool, m)?)?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    Ok(())
}
