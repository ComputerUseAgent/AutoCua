//! The iPhone agent's tool registry: every tool the main driver can call,
//! and the LLMManager that hands them to the model.
//!
//! The Rust copy of tool_registry/service.py. Python imports it under its
//! old dotted name, AutoCua.ios.tool_registry.service (tool_registry/
//! __init__.py points that name here), and finds the same names with the
//! same values. The manager it subclasses stays the shared Python one,
//! AutoCua/llm_provider/llm_manager.py, and so does _tool, which builds
//! every tool.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module what never changes: its docstring and its
//! two functions, _main_tools and _claude_reasoning_tool. Then the loader
//! binds what service.py's import line bound (_LLMManager, NOTE_TOOLS,
//! REASONING_OPTIONAL, _tool, drop_bundled_terminal, tool_calls_to_steps)
//! and calls _build(), which runs the rest of service.py's lines in their
//! order, its two def statements included, so the module ends up with its
//! names in service.py's order. Every name a line reads (_tool, dict,
//! frozenset, a table made a line before) is read from the module at that
//! moment, as Python reads a global, so a patched one is the one used.
//!
//! Python's own rules decide every result, so they are called, not
//! imitated: _tool builds each tool, f-string values go through format(),
//! `+` and `!=` are Python's operators, dict.update joins the defaults and
//! the metaclass makes the class. The objects Python shares stay shared:
//! MAIN_TOOLS[0] is MAIN_REASONING_TOOL, the three defaults maps share
//! their value dicts, and an action tool's description (a constant of
//! _main_tools) is one object in all three tool lists.

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
    "The iOS main driver's tool registry: every tool it can call, and the\n",
    "LLMManager that hands them to the model. The manager itself (providers,\n",
    "retries, the tool-call parser) is the same on every platform:\n",
    "AutoCua/llm_provider/llm_manager.py. The driver imports everything\n",
    "LLM-related from here.",
);

/// LLMManager's docstring.
const CLASS_DOC: &str = "The shared LLMManager with the iOS main driver's tool lists.";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 299;

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
    for key in [intern!(py, "MAIN_TOOLS"), intern!(py, "MAIN_TOOLS_FAST"), intern!(py, "MAIN_TOOLS_CLAUDE")] {
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
/// <tool_capability> / <os_interaction> / <task_completion> in
/// agent/iphone_use/system_prompt.md - the prompt stays the source of truth
/// (it is user-managed and never edited from here); this just puts the same
/// words where a native tool call can read them. Built twice, each with NO
/// per-call params (`track` stays {}) and a dedicated notes tool in front:
/// quality mode the `reasoning` tool (thinking optional, memory + next_goal
/// every step), fast mode the `memory` tool (`next_goal` alone). Both modes
/// share each action tool's description text.
///
/// iOS-specific vs the macOS registry: `scroll` and `vault` carry `value`
/// (not macOS's `direction`), `click` has no `clicks` field, and `vault` /
/// `video_player` exist here only.
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
    // _tool(<name>, {<field>: {"type": <kind>}, ...}, <description>,
    // track=track), with _tool read when its call comes, as a function reads
    // a global.
    let tool = |tool_name: &str, fields: &[(&str, &str)], description: &Bound<'py, PyString>| -> PyResult<Bound<'py, PyAny>> {
        let make = name(m, "_tool")?;
        let params = PyDict::new(py);
        for (field, kind) in fields {
            params.set_item(PyString::intern(py, field), display(py, &[("type", PyString::intern(py, kind).as_any())])?)?;
        }
        make.call((PyString::intern(py, tool_name), params, description), Some(&keyword(py, "track", &track)?))
    };
    let actions = PyList::new(py, [
        // -- <tool_capability> ------------------------------------------------
        tool("open_app", &[("value", "string")], constant!(py, concat!(
            "Launch an installed application directly by name - faster than searching on the device. Use the special name \"home\" to return to the home screen.\n",
            "    1. Requirement: Typically call wait 2-3 seconds after this tool to allow loading.\n",
            "    2. Success means the app is confirmed in the foreground (if the result says verified: false, confirm on the next screenshot). An unknown or ambiguous name returns an error naming the closest installed apps - re-issue with that exact name.\n",
            "    3. Format: open_app {\"value\": \"app name\"}\n",
            "    4. Examples:\n",
            "        1. open_app {\"value\": \"disney+\"}\n",
            "        2. open_app {\"value\": \"home\"}",
        )))?,
        tool("wait", &[("value", "string")], constant!(py, concat!(
            "Pause before the next screen scan to allow UI loading. Never exceed 3 seconds at a time.\n",
            "    1. Format: wait {\"value\": \"time in seconds\"}\n",
            "    2. Examples:\n",
            "        1. wait {\"value\": \"3\"}\n",
            "        2. wait {\"value\": \"2\"}",
        )))?,
        tool("web", &[("value", "string")], constant!(py, concat!(
            "Delegate to a specialized AI that fetches real-time information and provides data at runtime. Use it for speed instead of browsing manually on the phone.\n",
            "    1. Format: web {\"value\": \"search query\"}\n",
            "    2. Examples:\n",
            "        1. web {\"value\": \"financial results of Nvidia Q4 2025\"}\n",
            "        2. web {\"value\": \"latest Netflix app version on the App Store\"}",
        )))?,
        tool("shell", &[("value", "string")], constant!(py, concat!(
            "Run a shell/zsh command on the host Mac where this agent is running - not on the iPhone. Use it to check information or perform actions on the host OS, then continue the task on the phone (e.g. photo sharing: read the photo names and details on the Mac first, then share those same photos from the phone). Accepts every shell command, including AppleScript via osascript.\n",
            "    1. Format: shell {\"value\": \"command\"}\n",
            "    2. Examples:\n",
            "        1. shell {\"value\": \"ls ~/Pictures/holiday | head -5\"}\n",
            "        2. shell {\"value\": \"osascript -e 'tell application \\\"Finder\\\" to get name of every file of desktop'\"}",
        )))?,
        tool("todo_list", &[("value", "string")], constant!(py, "Create the ToDo task list (iteration 1 by default; you may also create/expand it later if complexity emerges). See <todo_capability>."))?,
        tool("update_todo", &[("value", "string")], constant!(py, concat!(
            "Tasks are auto-numbered #1, #2, #3, etc. when saved.\n",
            "    1. Update a task only after it is confirmed complete via <agent_history> and the effect is visible in the latest input (image or any relevant tag); one item per call.\n",
            "    2. Example: update_todo {\"value\": \"1\"}",
        )))?,
        tool("vault", &[("id", "integer"), ("value", "string")], constant!(py, concat!(
            "Fill a field with a login the user saved for the app in front: the runtime types it for you, and the value itself never reaches you.\n",
            "    1. Format: vault {\"id\": <element_id>, \"value\": \"<key_from_vault>\"}\n",
            "    2. Examples:\n",
            "        1. vault {\"id\": 3, \"value\": \"username\"} - the saved username into the username field\n",
            "        2. vault {\"id\": 4, \"value\": \"password\"} - the saved password into the password field\n",
            "        3. vault {\"id\": 0, \"value\": \"pin\"} - the saved PIN on a PIN keypad\n",
            "    3. <vault> in <persistent_memory> lists the keys saved for the app in front. No <vault> means nothing is saved for it: use what <user_request> gives, or ask the user. Never guess a login.\n",
            "    4. Like `input`, it clears the field first, and it batches like `input`: fill every login field and tap the button in one turn. A password goes only into a password field (securetextfield).\n",
            "    5. PIN keypads - a separate button for each digit 0-9, like the lock-screen passcode or an app PIN pad: use id 0 with value \"pin\". The vault finds the digit keys and taps the whole PIN itself. Never tap PIN digits yourself. A keypad PIN vault must be the ONLY action call of its turn (only your notes call may precede it). A PIN text field you can type into is filled like any other field, with its [id].\n",
            "    6. The PIN result says whether every key press registered. Retry only when it says it is safe to try once more; if the PIN was not accepted, only part of it registered, or it was already tried twice, stop and ask the user.\n",
            "    7. What the vault typed reads as [vault] in later steps.",
        )))?,
        tool("video_player", &[("value", "string")], constant!(py, concat!(
            "Track and control full-screen video playback through the control center (works despite DRM screenshot restrictions). Commands: close, streaming (check whether content is playing), pause, play.\n",
            "    1. Format: video_player {\"value\": \"one of: close/streaming/pause/play\"}\n",
            "    2. Examples:\n",
            "        1. video_player {\"value\": \"streaming\"}\n",
            "        2. video_player {\"value\": \"pause\"}",
        )))?,
        tool("scratchpad", &[("value", "string")], constant!(py, concat!(
            "Record a verified checkpoint or any critical fact (file path, metric, finding). Follow <scratchpad> rules.\n",
            "    1. Write `value` in Markdown - inline only (`**bold**`, backticks), never a line break.\n",
            "    2. Example: scratchpad {\"value\": \"**Key metric:** Disney+ revenue (Q3 2025) = **$2.1B**\"}",
        )))?,

        // -- <tool_capability> #10 + <task_completion> -------------------------
        tool("done", &[("value", "string")], constant!(py, concat!(
            "End the task with an end-to-end summary of what was achieved. Dedicated final step - never combine with any other action; do cleanup and ToDo/scratchpad updates in the step before.\n",
            "    1. Write `value` in Markdown - headings, `-` bullets, `**bold**`, backticks and fenced code blocks as the summary needs them.\n",
            "    2. Examples:\n",
            "        1. done {\"value\": \"**Netflix updated** to the latest version - login verified and version noted.\"}\n",
            "        2. done {\"value\": \"**Message sent to John** - delivery confirmed on screen.\"}\n",
            "    3. Only start completion after reviewing <agent_history> to confirm every requested task is finished.\n",
            "    4. Then do a final verification from the latest input (double-check the last steps match the request; if playback is DRM-blocked, verify via a video_player check).\n",
            "    5. Use `done` as a dedicated final step only:\n",
            "        1. Step 1 (no `done`): finish/cleanup + update ToDos/scratchpad.\n",
            "        2. Step 2: output ONLY Format: done {\"value\": \"<end-to-end summary in markdown format>\"}\n",
            "    6. Never combine `done` with any other action/tool in the same step.",
        )))?,

        // -- <os_interaction> -------------------------------------------------
        tool("click", &[("id", "integer")], constant!(py, concat!(
            "Tap the centre of an element by its [id].\n",
            "    1. Examples:\n",
            "        1. click {\"id\": 4}\n",
            "        2. click {\"id\": 23}",
        )))?,
        tool("ground_click", &[("x", "integer"), ("y", "integer")], constant!(py, concat!(
            "Tap a raw point on the screenshot, for a target with no [id]: x and y on a 0-1000 scale of the image (0,0 top-left, 1000,1000 bottom-right).\n",
            "    1. Example: ground_click {\"x\": 650, \"y\": 258}",
        )))?,
        tool("ground_scroll", &[("x", "integer"), ("y", "integer"), ("value", "string")], constant!(py, concat!(
            "Swipe from a raw point on the screenshot, for a list or pane with no [id]: x and y on the 0-1000 scale of the image (as ground_click), value the direction the finger moves (up/down/left/right, as scroll: \"up\" reveals what is below).\n",
            "    1. Example: ground_scroll {\"x\": 500, \"y\": 700, \"value\": \"up\"}",
        )))?,
        tool("input", &[("id", "integer"), ("value", "string")], constant!(py, concat!(
            "Type into an element by its [id]. Existing text in the field is auto-deleted before typing.\n",
            "    1. Examples:\n",
            "        1. input {\"id\": 3, \"value\": \"Hi, how are you\"}\n",
            "        2. input {\"id\": 4, \"value\": \"conjuring\"}",
        )))?,
        tool("scroll", &[("id", "integer"), ("value", "string")], constant!(py, concat!(
            "Swipe within an element's bounds - three-part action: the element [id] and the direction (value: up/down/left/right).\n",
            "    1. To reveal content below the visible area, scroll \"up\"; to reveal content above, scroll \"down\".\n",
            "    2. To reveal content on the right, scroll \"left\"; to reveal content on the left, scroll \"right\".\n",
            "    3. Examples:\n",
            "        1. scroll {\"id\": 3, \"value\": \"up\"}\n",
            "        2. scroll {\"id\": 7, \"value\": \"left\"}",
        )))?,
    ])?;
    // return notes + [...]
    notes.add(actions)
}

/// Claude's `agent_memory` notes tool. `final` is the agent's closing tool (always
/// the only call of its turn); `work` names its action calls.
///
/// `batched` (every agent): the opening sentences say the call never ends a
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
            "- `memory` and `next_goal` are filled every step: `memory` follows <memory>, ",
            "`next_goal` follows <next_goal>.\n",
            "- A keypad PIN `vault` (id 0, value \"pin\") stays the only action call of its turn: `agent_memory`, then `vault`, nothing else. ",
            "Not used on the `",
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
    // NATIVE TOOLS - the main driver calls these tools natively; no JSON envelope.
    // The provider returns structured calls, tool_calls_to_steps() converts them
    // into the same `[{type, ...}]` action dicts route_action already consumes -
    // the controller is untouched.
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
    // plan (Now / Plan / Then) and the Expect guard (fast_system_prompt.md has no
    // <memory> section: its rules are the <next_goal> section).
    let fields = display(py, &[("next_goal", note_field(py, "Follow <next_goal> - if the last action FAILED verification, open with one short clause naming the failure (skip it entirely when it passed). Then the context that matters next: current app/screen state, key ids used with their (element_name/type/value), and any tool name + purpose + important result. Then the forward plan: \"Now: <immediate step> (ToDo: <task_name>). Plan: <next 2-3 steps>. Then: <very next step>.\" END with the predicted visible change of THIS step's action prefixed \"Expect:\", so the next step can verify against the new screenshot. 3-5 concise lines.")?.as_any())])?;
    store(m, "MAIN_NOTES_FIELDS_FAST", &fields)?;

    // The MAIN DRIVER's `reasoning` tool fields (quality mode; fast mode's
    // `memory` tool carries `next_goal` alone, see MAIN_NOTES_FIELDS_FAST): the
    // macOS driver's design in iPhone/iPad terms. The verdict is judged on the
    // CURRENT screenshot for a UI action and on the returned result for a tool
    // action (web, shell, video_player), targets are named by name/role in
    // `next_goal` and locked to fresh [id]s in `memory`'s Targets line every
    // step, and the guard is the visible change the next screenshot must show
    // (or the tool's expected result). No S<n> codes, no "not required": a route
    // step omits `thinking` and the snapshot shows "skipped".
    let fields = display(py, &[
        ("thinking", note_field(py, "Your reasoning, written for your future self, not for the user. Not every step: think in bursts. Follow <thinking>: \"JUDGE: ... UNDERSTAND: ... DECIDE: ... ROUTE: 1. ... 2. ... 3. ...\" (FULL) when there is no route, it is used up, or a new surface appeared; a short freeform paragraph (RECOVERY) after a guard failed (on the screenshot, or in the returned result of web, shell or video_player); 1-3 judgment lines (BRIEF) on a surprise that leaves the route standing. Omit it (or pass \"\") on a route step: the previous \"Next:\" named this action, its guard holds (a UI guard on the current screenshot, a tool guard in that tool's result), and every named target resolves to exactly one [id].")?.as_any()),
        ("memory", note_field(py, "Follow <memory>. Open with the verdict on the previous step, judged on the CURRENT screenshot for a UI guard (never on what a UI tool result claims) or on that tool's returned result for a tool guard (web, shell, video_player): \"Last step worked: <what the screen or the result shows>\", \"Last step failed: <what it shows instead>\" or \"Last step unclear: <what is ambiguous, and what would settle it>\". First step: \"Task start.\" Then the key context (app/screen state; a tool used, its purpose and important result) and, on any step that touches UI, the Targets line: \"Targets: id N (element_name/type/value), ...\" resolved from the CURRENT <element_tree> before acting. 2-4 tight lines.")?.as_any()),
        ("next_goal", note_field(py, "Follow <next_goal>: \"Doing: <this step> (ToDo: <task_name>). If <the visible change the next screenshot must show, or the expected returned result of web/shell/video_player> → Next: <action on a target named by NAME/ROLE | think: <decision to make>>.\" Never name a successor target by [id]: ids are re-assigned every scan. On a route step, \"Doing\" is the next line of the ROUTE and \"Next:\" the line after it.")?.as_any()),
    ])?;
    store(m, "MAIN_REASONING_FIELDS", &fields)?;

    // MAIN_REASONING_TOOL = _tool("reasoning", MAIN_REASONING_FIELDS, r"""...""",
    //                             optional=REASONING_OPTIONAL)
    let make = name(m, "_tool")?;
    let fields = name(m, "MAIN_REASONING_FIELDS")?;
    let optional = name(m, "REASONING_OPTIONAL")?;
    let description = PyString::new(py, r#"Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read the reasoning behind every step.
- `memory` and `next_goal` are filled every step. `thinking` is not: you think in bursts (see <thinking>). A FULL thinking ends with a ROUTE of the next 2-5 steps, each an action on a target named by NAME/ROLE with its guard (the visible change the next screenshot must show; for web, shell and video_player, that tool's returned result); on each step of that route omit `thinking` (or pass ""), and think again only when the route is used up, a guard fails, a new surface appears, or the route said "think". A screenshot that shows exactly what the guard predicted is confirmation, not new information: it goes into `memory`, never into a new thinking or a new ROUTE.
- Every step, thinking or not, resolves the route's named targets to fresh [id]s from the CURRENT <element_tree> and locks them in `memory`'s Targets line before acting. A target that resolves to 0 or 2+ ids, or is only partially visible, means this step thinks.
- A step may be `reasoning` alone when you need to think before you act (it costs a fresh screen scan); the next step then does the work. Do not chain several such steps.
- A keypad PIN `vault` (id 0, value "pin") stays the only action call of its turn: `reasoning`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: reasoning {"thinking": "", "memory": "...", "next_goal": "..."}
- Examples:
  1. Route step (no thinking; "Doing" is the next route line):
     reasoning {"memory": "Last step worked: App Store front, Search tab open, the search field is empty as predicted. Targets: id 19 (element_name='Search', type='searchfield').", "next_goal": "Doing: search for Netflix (ToDo: Update Netflix). If the results list shows the Netflix row → Next: think: confirm the row and route into its page."}
  2. New surface (FULL thinking, ends with the ROUTE):
     reasoning {"thinking": "JUDGE: PASS, the Mail compose sheet is open as predicted, To field empty. UNDERSTAND: known: To, Subject and the body are in the tree as text fields; assumed: focus is in To, the tree does not say. DECIDE: fill the three fields in one batch and guard on the body showing the text, since a wrong focus would drop the typing into the wrong field; Send is the next surface change, so the route stops after it. ROUTE: 1. input To, Subject and body in one batch, if the body shows the flight text. 2. tap Send, if the compose sheet closes and the mailbox is back → think: verify the mail in Sent, then mark the ToDo.", "memory": "Last step worked: compose sheet open, guard held. Targets: id 12 (element_name='To', type='textfield'), id 14 (element_name='Subject', type='textfield'), id 20 (element_name='Message body', type='textview').", "next_goal": "Doing: fill To, Subject and body (ToDo: Send flight email). If the body shows the flight text → Next: tap the Send button."}
  3. Recovery after a failed guard:
     reasoning {"thinking": "Failed: the To field is still empty after input on id 74; the screenshot shows the cursor in the Subject field. Wrong belief: that compose focuses To on open; it focused Subject. Narrowest fix: tap the To field first, then input. Guard: the To field shows the address. Route holds after this.", "memory": "Last step failed: To field empty, focus was in Subject. Correction carried: tap a field before typing into it in this compose sheet. Targets: id 74 (element_name='To', type='textfield').", "next_goal": "Doing: tap the To field and input abc@gmail.com (ToDo: Send flight email). If the To field shows abc@gmail.com → Next: input the subject."}"#);
    let made = make.call((intern!(py, "reasoning"), fields, description), Some(&keyword(py, "optional", optional)?))?;
    store(m, "MAIN_REASONING_TOOL", &made)?;

    // MAIN_NOTES_TOOL_FAST = _tool("memory", MAIN_NOTES_FIELDS_FAST, r"""...""")
    let make = name(m, "_tool")?;
    let fields = name(m, "MAIN_NOTES_FIELDS_FAST")?;
    let description = PyString::new(py, r#"Your notes to yourself for this step. Call it FIRST in every step, then the calls that do the work, all in the same turn. The harness does nothing with it (its result is just `ok`), but the call is replayed in <agent_history>, so your future self can read what you knew and planned at every step.
- One field, `next_goal`, filled every step: key context, the forward plan (Now / Plan / Then) and the Expect guard the next step verifies against (see <next_goal>). There is no thinking field: the reasoning stays silent (see <silent_reasoning>).
- Every step resolves the plan's targets to fresh [id]s from the CURRENT <element_tree> and names them in `next_goal` before acting.
- A keypad PIN `vault` (id 0, value "pin") stays the only action call of its turn: `memory`, then `vault`, nothing else. Not used on the `done` step: `done` is always the only call of its turn.
- Format: memory {"next_goal": "..."}
- Example:
  memory {"next_goal": "App Store Search tab open; search field id 19 (element_name='Search', type='searchfield'), search button id 21 (element_name='Search', type='button'). Now: tap id 19, type 'Netflix', tap id 21 (ToDo: Update Netflix). Plan: open its page -> tap Update. Then: tap the Netflix row. Expect: results list shows the Netflix app row."}"#);
    let made = make.call1((intern!(py, "memory"), fields, description))?;
    store(m, "MAIN_NOTES_TOOL_FAST", &made)?;

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

    // Per-action defaults for the main driver - guarantees route_action receives
    // every field of an action, even if the model omits an optional-feeling one.
    // Doubles as the field whitelist: fast mode's per-call `memory` is absent
    // here, so it never reaches the controller. `reasoning` is listed so the
    // parser accepts the call; the service pulls it out before routing.
    let empty = PyString::new(py, "").into_any();
    let zero = 0i32.into_pyobject(py)?.into_any();
    let none = py.None().into_bound(py);
    let up = intern!(py, "up").as_any();
    let value_only = || display(py, &[("value", &empty)]);
    let defaults = display(py, &[
        ("reasoning", display(py, &[("thinking", &empty), ("memory", &empty), ("next_goal", &empty)])?.as_any()),
        ("click", display(py, &[("id", &zero)])?.as_any()),
        // None, not 0: a coordinate the model left out must fail, not tap the edge.
        ("ground_click", display(py, &[("x", &none), ("y", &none)])?.as_any()),
        ("ground_scroll", display(py, &[("x", &none), ("y", &none), ("value", up)])?.as_any()),
        ("input", display(py, &[("id", &zero), ("value", &empty)])?.as_any()),
        ("scroll", display(py, &[("id", &zero), ("value", up)])?.as_any()),
        ("vault", display(py, &[("id", &zero), ("value", &empty)])?.as_any()),
        ("open_app", value_only()?.as_any()),
        ("wait", value_only()?.as_any()),
        ("web", value_only()?.as_any()),
        ("shell", value_only()?.as_any()),
        ("todo_list", value_only()?.as_any()),
        ("update_todo", value_only()?.as_any()),
        ("video_player", value_only()?.as_any()),
        ("scratchpad", value_only()?.as_any()),
        ("done", value_only()?.as_any()),
    ])?;
    store(m, "MAIN_ACTION_DEFAULTS", &defaults)?;

    // Fast mode's notes tool is `memory` (next_goal alone) instead of `reasoning`;
    // tool_calls_to_steps reads the defaults map to find the mode's notes tool.
    let fast = display(py, &[("memory", display(py, &[("next_goal", &empty)])?.as_any())])?;
    fast.update(without_reasoning(m)?.as_mapping())?;
    store(m, "MAIN_ACTION_DEFAULTS_FAST", &fast)?;

    // Names the driver may call - anything else is answered with an error tool
    // result instead of being silently dropped, so the model can correct itself
    // on the very next turn.
    store(m, "MAIN_TOOL_NAMES", &name_set(m, "MAIN_TOOLS")?)?;
    store(m, "MAIN_TOOL_NAMES_FAST", &name_set(m, "MAIN_TOOLS_FAST")?)?;

    // ---------------------------------------------------------------------------
    // CLAUDE: any Claude model, whatever the provider (anthropic direct, or a Claude
    // entry on OpenRouter; see LLMManager.claude). The main driver runs
    // on its own claude_prompt.md (in quality mode only) with its own notes tool,
    // `agent_memory`: `memory` and `next_goal`, both filled every step. The Claude
    // prompt has no <assessment> block and no thinking field, so both fields are
    // required (no REASONING_OPTIONAL here).
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
    //     """The shared LLMManager with the iOS main driver's tool lists."""
    //     MAIN_TOOLS = MAIN_TOOLS
    //     MAIN_TOOLS_FAST = MAIN_TOOLS_FAST
    //     MAIN_TOOLS_CLAUDE = MAIN_TOOLS_CLAUDE
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
