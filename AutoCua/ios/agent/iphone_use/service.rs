//! AgentService: the iPhone agent, the Rust copy of iphone_use/service.py.
//!
//! One request is one run of the loop: scan the phone's screen, build the
//! messages, ask the model, run the tools it called, record the step, and
//! again, until a `done` call, a Stop, an error or the step limit. The loop
//! is Rust. What it drives is still Python and is called over the GIL: the
//! tool registry and its LLMManager, the controller, the phone reader, the
//! skills, the vault, the transcript codec (view) and memory compression.
//!
//! It is the computer-use agent's loop (mac/agent/computer_use/service.rs)
//! with the iPhone agent's own differences. It has no sub-agents, no scan
//! switch, no UAC and no training data. It has the vault (a secret the vault
//! typed reads as [vault], and the <vault> block lists the saved logins), a
//! scratchpad of its own (a session's own folder, the todo folder) and its
//! own words in the messages it builds and prints.
//!
//! How the port keeps the Python's behaviour:
//! - Every value stays a Python object from end to end, and what the Python
//!   did with it (truthiness, `x or default`, ==, in, len, str(), format(),
//!   +, .get, iteration, unpacking) is the real Python operation, called
//!   through PyO3 in the same order. So an odd value fails where and how it
//!   did, and a lone surrogate passes through unchanged.
//! - The names service.py imported are attributes of this module. The loader
//!   (iphone_use/__init__.py) binds them when the package is imported, and
//!   they are read here when they are used, the way Python read its globals.
//!   print and open are Python's builtins, found the same way. So are json,
//!   re, time and datetime: a wait is Python's time.sleep, and the GIL is
//!   free while it lasts.
//! - The object's attributes live in its own __dict__. They are set in the
//!   order __init__ set them and read with a plain attribute lookup, as
//!   `self.x` was. Nothing sits behind a Rust lock, so a callback can call
//!   back into the agent, from this thread or another.
//! - A step's three closers (_pair_results, _discard_step, _close_pairing)
//!   were closures over the function's variables. Here they are functions
//!   over the same variables, kept in Run, with one flag for "a step got as
//!   far as defining them" (Python's NameError guard).
//! - An `except` block and the __exit__ of a `with` block whose body failed
//!   run with that error as the one being handled (crate::mac::Handling). So
//!   sys.exc_info() shows it to what they call, and an error raised there
//!   gets it as __context__.
//! - What a generator expression ran per item goes through in_generator: a
//!   StopIteration that leaves it is Python's RuntimeError (PEP 479).
//!
//! Python imports this module under its old dotted name,
//! AutoCua.ios.agent.iphone_use.service.

use pyo3::exceptions::{
    PyAssertionError, PyAttributeError, PyException, PyFileNotFoundError, PyKeyError, PyNameError,
    PyRuntimeError, PyStopIteration, PyTypeError, PyValueError,
};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PySlice, PyString, PyTuple};

use crate::mac::{as_python_class, type_name, Handling};
use crate::memory_compression::{format_value, global, is_exception};

// Run-boundary request markers: on resume, the bridge entry that ended the
// prior run ("Previous run concluded.", saved by agent_conversation with an
// empty tool slot) gets its slot filled with the NEW run's request, so the
// persisted memory attributes every run's steps to the request that drove
// them. The sentence rides in the bridge's next_goal (prefix match: it
// continues).
const BRIDGE_SIGNATURE: &str = "\"next_goal\": \"Previous run concluded.";
const REQUEST_MARKER_PREFIX: &str = "<updated_user_request";

/// The number in a request marker, as Python's re reads it.
const MARKER_NO_PATTERN: &str = r#"<updated_user_request no="(\d+)">"#;

/// The `<Step_no=N />` line a schema-era entry starts with, and its JSON.
const STEP_PREFIX_PATTERN: &str = r"(<Step_no=\d+ />\n)(.*)";

/// Appended to the system prompt for an `async_tools` model (GPT-6 Astra, see
/// llm_provider/responses.py): its tool calls run after the turn ends, and
/// the block tells it what that changes about the rules above it.
const ASYNC_TOOLS_ADDENDUM: &str = "\n<async_tools>\n\
Your tool calls run after the turn ends, in the order you emit them, and no result reaches you until the next step's input. The rules above are unchanged: one notes call first, then every action whose target is on the current screen, stopping where the screen must change. Because nothing comes back mid-turn:\n\
- Never write a text message in a turn that contains tool calls, and never state that an action succeeded or failed: that verdict belongs to the next step's memory, judged on the next screenshot.\n\
- `done` stays the only call of its turn. Never add it after this turn's actions: their results have not arrived, so nothing is verified yet.\n\
</async_tools>\n";

/// Appended to the live user message for a Claude model (quality mode).
/// Claude treats a notes tool as a think step: it calls `agent_memory`, ends
/// its turn to wait for the result, and acts on the NEXT turn, one call per
/// turn and a wasted screen scan per action. Saying in the live message that
/// the notes never end a turn is what puts the notes and the actions in one
/// message. Measured 2026-10-05 on step 1 of "open calculator and compute
/// 23*47" (Sonnet 5 via OpenRouter): notes and action together 0/5 before,
/// 6/6 with this reminder plus the matching <blocks> rule in claude_prompt.md
/// and the agent_memory description (tool_registry, `batched=True`).
/// Anthropic's own guidance: a sentence at the end of the current request
/// moves batching far more than the same text in the system prompt, so it
/// rides here, every step, on the live message only (never persisted, so the
/// cached prefix is unchanged).
const CLAUDE_BATCH_REMINDER: &str = "\n\n<critical>This turn must carry the `agent_memory` call AND the action calls that do its \
\"Doing\" in this one response. `agent_memory` alone is not a step: it has no result, \
so never end the turn after it.</critical>";

/// The rule line a snapshot file draws under its header and its system prompt.
const RULE: &str = "============================================================";

type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The module and its globals
// ---------------------------------------------------------------------------

/// This module, as registered: the names service.py imported and its own
/// constants and functions are read from it at run time, as Python read its
/// globals.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    match MODULE.get(py) {
        Some(module) => Ok(module.bind(py).clone()),
        None => Err(PyRuntimeError::new_err("the iPhone service module is not registered")),
    }
}

/// A global of this module, or a builtin, by name: Python's own lookup. A
/// name that is gone is Python's NameError, raised here and now: an error
/// made in Rust is only raised when it is first looked at, and by then an
/// `except` block that asked for the name is over. Raised now, it gets the
/// error being handled as its __context__, as it did.
fn g<'py>(py: Python<'py>, name: &str) -> PyResult<Obj<'py>> {
    global(&module(py)?, name).inspect_err(|e| {
        e.value(py);
    })
}

/// Python's name for this module. Its functions and its class report it as
/// their __module__.
const PY_MODULE: &str = "AutoCua.ios.agent.iphone_use.service";

/// service.py's __file__, as it was: in this file's folder, under the folder
/// that holds the extension.
fn module_file(py: Python<'_>) -> PyResult<String> {
    let file = crate::native_dir(py)?
        .join("ios")
        .join("agent")
        .join("iphone_use")
        .join("service.py");
    Ok(file.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Python's operations
// ---------------------------------------------------------------------------

fn txt<'py>(py: Python<'py>, value: &str) -> Obj<'py> {
    PyString::new(py, value).into_any()
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

fn int(py: Python<'_>, value: i64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

fn float(py: Python<'_>, value: f64) -> Obj<'_> {
    PyFloat::new(py, value).into_any()
}

fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

/// `isinstance(value, dict)`.
fn is_dict(value: &Obj<'_>) -> PyResult<bool> {
    value.is_instance(value.py().get_type::<PyDict>().as_any())
}

/// `isinstance(value, list)`.
fn is_list(value: &Obj<'_>) -> PyResult<bool> {
    value.is_instance(value.py().get_type::<PyList>().as_any())
}

/// `isinstance(value, str)`.
fn is_str(value: &Obj<'_>) -> PyResult<bool> {
    value.is_instance(value.py().get_type::<PyString>().as_any())
}

/// `value or ""`.
fn or_empty<'py>(value: Obj<'py>) -> PyResult<Obj<'py>> {
    if value.is_truthy()? {
        Ok(value)
    } else {
        Ok(txt(value.py(), ""))
    }
}

/// `a += b`: the in-place add, which extends a list and makes a new str.
fn iadd<'py>(a: &Obj<'py>, b: &Obj<'py>) -> PyResult<Obj<'py>> {
    // SAFETY: two valid objects; a new reference or NULL with the error set.
    unsafe { Bound::from_owned_ptr_or_err(a.py(), ffi::PyNumber_InPlaceAdd(a.as_ptr(), b.as_ptr())) }
}

/// `slice(start, stop)`, what `seq[start:stop]` indexes with.
fn slice<'py>(start: &Obj<'py>, stop: &Obj<'py>) -> PyResult<Obj<'py>> {
    start.py().get_type::<PySlice>().call1((start, stop))
}

/// `str(e)` of a caught error.
fn err_text<'py>(py: Python<'py>, e: &PyErr) -> PyResult<Obj<'py>> {
    Ok(e.value(py).str()?.into_any())
}

/// `raise new` while `old` is being handled, for an error this file makes
/// itself: Python keeps the handled error as the new one's __context__. An
/// error raised by Python code gets it from the interpreter (see Handling).
/// One that Rust hands back is not raised that way, so it is linked here.
fn raised_while_handling(py: Python<'_>, new: PyErr, old: &PyErr) -> PyErr {
    let _ = new.value(py).setattr("__context__", old.value(py));
    new
}

/// What a generator expression ran for one item: its `if` and the value it
/// gave. Python does not let a StopIteration out of a generator: the caller
/// gets RuntimeError('generator raised StopIteration'), with the
/// StopIteration as its __cause__ and its __context__ (PEP 479). Making the
/// iterator and what the caller does with the values are not in here.
fn in_generator<T>(py: Python<'_>, item: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    match item() {
        Err(e) if e.is_instance_of::<PyStopIteration>(py) => {
            let error = py.get_type::<PyRuntimeError>().call1(("generator raised StopIteration",))?;
            error.setattr("__cause__", e.value(py))?;
            error.setattr("__context__", e.value(py))?;
            Err(PyErr::from_value(error))
        }
        other => other,
    }
}

/// `print(line)` with Python's print, so the line goes to sys.stdout wherever
/// it points.
fn say<'py>(py: Python<'py>, line: &Obj<'py>) -> PyResult<()> {
    g(py, "print")?.call1((line,))?;
    Ok(())
}

fn say_text(py: Python<'_>, line: &str) -> PyResult<()> {
    say(py, &txt(py, line))
}

/// One piece of an f-string: text as written, or a value to format.
enum Part<'a, 'py> {
    L(&'a str),
    V(&'a Bound<'py, PyAny>),
}
use Part::{L, V};

/// An f-string: each value formatted as format(value, "") in its turn, then
/// all the pieces joined.
fn fstr<'py>(py: Python<'py>, parts: &[Part<'_, 'py>]) -> PyResult<Obj<'py>> {
    let pieces = PyList::empty(py);
    for part in parts {
        match part {
            L(text) => pieces.append(*text)?,
            V(value) => pieces.append(format_value(value, "")?)?,
        }
    }
    txt(py, "").call_method1("join", (pieces,))
}

/// A dict written out in the source: its keys in order.
fn dict_of<'py>(py: Python<'py>, items: &[(&str, &Obj<'py>)]) -> PyResult<Obj<'py>> {
    let dict = PyDict::new(py);
    for (key, value) in items {
        dict.set_item(*key, *value)?;
    }
    Ok(dict.into_any())
}

/// One keyword argument.
fn keyword<'py>(py: Python<'py>, name: &str, value: impl IntoPyObject<'py>) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(name, value)?;
    Ok(kwargs)
}

/// indent=2, ensure_ascii=False: how the transcript's JSON is written.
fn pretty(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("indent", 2)?;
    kwargs.set_item("ensure_ascii", false)?;
    Ok(kwargs)
}

/// `json.dumps(value, indent=2, ensure_ascii=False)` with this module's
/// json. The value is worked out after json.dumps is found, as in Python.
fn dumps_pretty<'py>(py: Python<'py>, value: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    let dumps = g(py, "json")?.getattr("dumps")?;
    let value = value()?;
    dumps.call((value,), Some(&pretty(py)?))
}

/// `a, b, ... = value`: Python's unpacking of exactly N items, with the
/// errors the running Python gives (3.14 added "got N" for a list, tuple or
/// dict with too many).
fn unpack<'py, const N: usize>(value: &Obj<'py>) -> PyResult<[Obj<'py>; N]> {
    let py = value.py();
    let count = N;
    let mut items = match value.try_iter() {
        Ok(items) => items,
        Err(e) => {
            let kind = value.get_type();
            // SAFETY: a valid type object and a valid object.
            let has_iter = unsafe { !ffi::PyType_GetSlot(kind.as_type_ptr(), ffi::Py_tp_iter).is_null() };
            let is_sequence = unsafe { ffi::PySequence_Check(value.as_ptr()) } == 1;
            if e.is_instance_of::<PyTypeError>(py) && !has_iter && !is_sequence {
                return Err(PyTypeError::new_err(format!(
                    "cannot unpack non-iterable {} object",
                    type_name(value)?
                )));
            }
            return Err(e);
        }
    };
    let mut found = Vec::with_capacity(count);
    while found.len() < count {
        match items.next() {
            Some(item) => found.push(item?),
            None => {
                return Err(PyValueError::new_err(format!(
                    "not enough values to unpack (expected {count}, got {})",
                    found.len()
                )))
            }
        }
    }
    match items.next() {
        // `found` holds exactly N items here, so the conversion cannot fail.
        None => found.try_into().map_err(|_| PyValueError::new_err("unpack")),
        Some(Err(e)) => Err(e),
        Some(Ok(_)) => {
            let exact = value.is_exact_instance_of::<PyList>()
                || value.is_exact_instance_of::<PyTuple>()
                || value.is_exact_instance_of::<PyDict>();
            let message = if exact && py.version_info() >= (3, 14) {
                format!("too many values to unpack (expected {count}, got {})", value.len()?)
            } else {
                format!("too many values to unpack (expected {count})")
            };
            Err(PyValueError::new_err(message))
        }
    }
}

/// A special method the way a statement finds it: on the type, never on the
/// instance, and bound to the object.
fn special<'py>(value: &Obj<'py>, name: &str) -> PyResult<Option<Obj<'py>>> {
    let py = value.py();
    let kind = value.get_type();
    for base in kind.getattr("__mro__")?.try_iter()? {
        let found = match base?.getattr("__dict__")?.get_item(name) {
            Ok(found) => found,
            Err(e) if e.is_instance_of::<PyKeyError>(py) => continue,
            Err(e) => return Err(e),
        };
        return match found.get_type().getattr("__get__") {
            Ok(get) => Ok(Some(get.call1((&found, value, &kind))?)),
            Err(e) if e.is_instance_of::<PyAttributeError>(py) => Ok(Some(found)),
            Err(e) => Err(e),
        };
    }
    Ok(None)
}

/// `with manager as value: body(value)`. None when the body failed and
/// __exit__ swallowed the error: Python then goes on after the block.
fn with<'py, T>(manager: &Obj<'py>, body: impl FnOnce(&Obj<'py>) -> PyResult<T>) -> PyResult<Option<T>> {
    let py = manager.py();
    let Some(enter) = special(manager, "__enter__")? else {
        return Err(PyTypeError::new_err(format!(
            "'{}' object does not support the context manager protocol",
            type_name(manager)?
        )));
    };
    let Some(exit) = special(manager, "__exit__")? else {
        return Err(PyTypeError::new_err(format!(
            "'{}' object does not support the context manager protocol (missed __exit__ method)",
            type_name(manager)?
        )));
    };
    let value = enter.call0()?;
    match body(&value) {
        Ok(result) => {
            exit.call1((py.None(), py.None(), py.None()))?;
            Ok(Some(result))
        }
        Err(e) => {
            // __exit__ runs while the body's error is being handled.
            let _handling = Handling::new(py, &e);
            let traceback = e.traceback(py).map(|t| t.into_any()).unwrap_or_else(|| none(py));
            if exit.call1((e.get_type(py), e.value(py), traceback))?.is_truthy()? {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// `open(path, mode, encoding='utf-8')` with Python's open, found now. The
/// path is worked out after open is found, as in Python.
fn open_text<'py>(
    py: Python<'py>,
    path: impl FnOnce() -> PyResult<Obj<'py>>,
    mode: &str,
) -> PyResult<Obj<'py>> {
    let open = g(py, "open")?;
    let path = path()?;
    open.call((path, mode), Some(&keyword(py, "encoding", "utf-8")?))
}

/// `f.write(text)`. The text is worked out after f.write is found.
fn write<'py>(f: &Obj<'py>, text: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<()> {
    let write = f.getattr("write")?;
    let text = text()?;
    write.call1((text,))?;
    Ok(())
}

/// An argument as the call gave it, or Missing when the call left it out and
/// the Python default applies. A plain Option cannot tell a None that was
/// passed from an argument that was not.
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

impl<'py> Given<'py> {
    fn or(self, default: impl FnOnce() -> Obj<'py>) -> Obj<'py> {
        match self {
            Given::Value(value) => value,
            Given::Missing => default(),
        }
    }
}

// ---------------------------------------------------------------------------
// The module's functions
// ---------------------------------------------------------------------------

/// `<updated_user_request no="N">`, the task, and its closing tag.
fn request_marker<'py>(n: &Obj<'py>, task: &Obj<'py>) -> PyResult<Obj<'py>> {
    fstr(
        n.py(),
        &[
            L("<updated_user_request no=\""),
            V(n),
            L("\">\n"),
            V(task),
            L("\n</updated_user_request no=\""),
            V(n),
            L("\">"),
        ],
    )
}

/// Clear all contents inside AutoCua/ios/scratchpad/ for a fresh start, and
/// the todo folder beside it.
///
/// A parallel simulator task sets AutoCua_IOS_SESSION and owns only its own
/// scratchpad/<session>/ folder: emptying the whole scratchpad there would
/// delete another task's notes while it runs.
fn cleanup_scratchpad(py: Python<'_>) -> PyResult<()> {
    // scratchpad_dir = Path(__file__).parent.parent.parent / "scratchpad"
    let mut scratchpad_dir = g(py, "Path")?
        .call1((module_file(py)?,))?
        .getattr("parent")?
        .getattr("parent")?
        .getattr("parent")?
        .div("scratchpad")?;
    // session = os.environ.get("AutoCua_IOS_SESSION") or ""
    let session = or_empty(g(py, "os")?.getattr("environ")?.call_method1("get", ("AutoCua_IOS_SESSION",))?)?;
    if session.is_truthy()? {
        scratchpad_dir = scratchpad_dir.div(&session)?;
    }
    if scratchpad_dir.call_method0("exists")?.is_truthy()? {
        for item in scratchpad_dir.call_method0("iterdir")?.try_iter()? {
            let item = item?;
            if item.call_method0("is_dir")?.is_truthy()? {
                g(py, "shutil")?.getattr("rmtree")?.call1((&item,))?;
            } else {
                item.call_method0("unlink")?;
            }
        }
    } else {
        let kwargs = PyDict::new(py);
        kwargs.set_item("parents", true)?;
        kwargs.set_item("exist_ok", true)?;
        scratchpad_dir.call_method("mkdir", (), Some(&kwargs))?;
    }

    // Also clear the todo folder. On iOS it is ios/todo, not under
    // scratchpad/, so it needs its own wipe for the same fresh start.
    let todo_dir = g(py, "Path")?
        .call1((module_file(py)?,))?
        .getattr("parent")?
        .getattr("parent")?
        .getattr("parent")?
        .div("todo")?;
    if todo_dir.call_method0("exists")?.is_truthy()? && todo_dir.call_method0("is_dir")?.is_truthy()? {
        g(py, "shutil")?.getattr("rmtree")?.call1((&todo_dir,))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The agent
// ---------------------------------------------------------------------------

/// The constructor's arguments, each as the call gave it or its default.
/// cli_callback is not among them: the app always passes it, and the iPhone
/// agent, which has no CLI agent, does not use it.
struct Args<'py> {
    provider: Obj<'py>,
    model: Obj<'py>,
    save_conversation: Obj<'py>,
    frontend_callback: Obj<'py>,
    text_callback: Obj<'py>,
    web_callback: Obj<'py>,
    shell_callback: Obj<'py>,
    tool_callback: Obj<'py>,
    token_callback: Obj<'py>,
    api_key: Obj<'py>,
    stop_event: Obj<'py>,
    prior_history: Obj<'py>,
    speed: Obj<'py>,
}

/// __init__, line by line, on the object `me`. A line that fails leaves
/// what the lines before it did (folders made and cleared, the manager, the
/// scanner and the controller built).
fn init<'py>(me: &Obj<'py>, args: &Args<'py>) -> PyResult<()> {
    let py = me.py();

    // Clean up scratchpad for a fresh start
    g(py, "_cleanup_scratchpad")?.call0()?;

    // Ensure sandbox workspace exists on Desktop
    let root = g(py, "Path")?.call_method0("home")?.div("Desktop")?.div("sandbox_workspace")?;
    me.setattr("sandbox_root", root)?;
    if !me.getattr("sandbox_root")?.call_method0("exists")?.is_truthy()? {
        let kwargs = PyDict::new(py);
        kwargs.set_item("parents", true)?;
        kwargs.set_item("exist_ok", true)?;
        me.getattr("sandbox_root")?.call_method("mkdir", (), Some(&kwargs))?;
    }

    // Speed mode: "fast" swaps in fast_system_prompt.md and trims the
    // tracking params the driver's tools carry.
    let fast = args.speed.str()?.call_method0("lower")?.eq("fast")?;
    me.setattr("speed", if fast { "fast" } else { "quality" })?;

    // Initialize LLM Manager with optional runtime API key. The driver speaks
    // NATIVE TOOL CALLING: its action tools ARE the output contract (no
    // response_format schema, nothing to parse). It stays cli_agent=False,
    // which is what keeps every provider's screenshot splice (gated on `not
    // cli_agent`) working.
    let manager_cls = g(py, "LLMManager")?;
    let kwargs = keyword(py, "speed", me.getattr("speed")?)?;
    let manager = manager_cls.call((&args.provider, &args.model, &args.api_key), Some(&kwargs))?;
    me.setattr("llm_manager", manager)?;
    // Where this mode's notes live: on a dedicated notes tool, called first
    // in every step, while the action tools carry only their own fields.
    // Quality: `reasoning` (thinking optional, memory + next_goal every
    // step). Fast: `memory` (`next_goal` alone). Quality on Claude (any
    // Claude model, any provider): `agent_memory` (memory + next_goal every
    // step, no thinking field) and its own prompt, claude_prompt.md.
    let fast = me.getattr("speed")?.eq("fast")?;
    // self._claude = not fast and self.llm_manager.claude
    let claude = if fast { boolean(py, false) } else { me.getattr("llm_manager")?.getattr("claude")? };
    me.setattr("_claude", claude)?;
    if fast {
        me.setattr("_notes_tool", "memory")?;
        me.setattr("_track_params", g(py, "MAIN_NOTES_FIELDS_FAST")?)?;
        me.setattr("_allowed_tools", g(py, "MAIN_TOOL_NAMES_FAST")?)?;
        me.setattr("_action_defaults", g(py, "MAIN_ACTION_DEFAULTS_FAST")?)?;
    } else if me.getattr("_claude")?.is_truthy()? {
        me.setattr("_notes_tool", "agent_memory")?;
        me.setattr("_track_params", g(py, "CLAUDE_REASONING_FIELDS")?)?;
        me.setattr("_allowed_tools", g(py, "MAIN_TOOL_NAMES_CLAUDE")?)?;
        me.setattr("_action_defaults", g(py, "MAIN_ACTION_DEFAULTS_CLAUDE")?)?;
    } else {
        me.setattr("_notes_tool", "reasoning")?;
        me.setattr("_track_params", g(py, "MAIN_REASONING_FIELDS")?)?;
        me.setattr("_allowed_tools", g(py, "MAIN_TOOL_NAMES")?)?;
        me.setattr("_action_defaults", g(py, "MAIN_ACTION_DEFAULTS")?)?;
    }
    // assert self._notes_tool in NOTE_TOOLS (python -O drops an assert)
    if !py.import("sys")?.getattr("flags")?.getattr("optimize")?.is_truthy()? {
        let notes_tool = me.getattr("_notes_tool")?;
        if !g(py, "NOTE_TOOLS")?.contains(notes_tool)? {
            return Err(PyAssertionError::new_err(()));
        }
    }

    // Store stop event
    me.setattr("stop_event", &args.stop_event)?;

    // Initialize UI Element Scanner with optional frontend callback for image streaming
    let scanner_cls = g(py, "UIElementScanner")?;
    let config = g(py, "ELEMENT_CONFIG")?;
    let kwargs = keyword(py, "frontend_callback", &args.frontend_callback)?;
    me.setattr("scanner", scanner_cls.call((config,), Some(&kwargs))?)?;
    me.setattr("skills", g(py, "DomainKnowledgeService")?.call0()?)?;

    // The callbacks: the agent's responses to the frontend, the globe and the
    // terminal animations, the tool-flow chain (tool_callback(event,
    // payload); events: run_start | turn{hasImage} |
    // received{tools:[{name,clicks?}]} | run_end) and the memory bar, called
    // after each LLM call with the provider's exact token usage.
    me.setattr("text_callback", &args.text_callback)?;
    me.setattr("web_callback", &args.web_callback)?;
    me.setattr("shell_callback", &args.shell_callback)?;
    me.setattr("tool_callback", &args.tool_callback)?;
    me.setattr("token_callback", &args.token_callback)?;

    // Initialize Controller. The GUI callbacks and the stop event go with
    // it, so the web and shell containers animate, web search gets its
    // provider and model, and a Stop in the middle of an action works.
    let controller_cls = g(py, "ControllerView")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("provider", &args.provider)?;
    kwargs.set_item("model", me.getattr("llm_manager")?.call_method0("get_model_name")?)?;
    kwargs.set_item("web_callback", &args.web_callback)?;
    kwargs.set_item("shell_callback", &args.shell_callback)?;
    kwargs.set_item("api_key", &args.api_key)?;
    kwargs.set_item("stop_event", &args.stop_event)?;
    me.setattr("controller", controller_cls.call((), Some(&kwargs))?)?;

    // Save conversation flag
    me.setattr("save_conversation", &args.save_conversation)?;

    // Load system prompt
    me.setattr("system_prompt", load_system_prompt(me)?)?;
    // An `async_tools` model (GPT-6 Astra) runs its calls after the turn
    // ends; the block tells it what that changes.
    if me.getattr("llm_manager")?.call_method0("async_tools")?.is_truthy()? {
        let prompt = me.getattr("system_prompt")?;
        let addendum = g(py, "ASYNC_TOOLS_ADDENDUM")?;
        me.setattr("system_prompt", iadd(&prompt, &addendum)?)?;
    }

    // Clear previous debug folder to start fresh
    let debug_folder = g(py, "Path")?.call1(("debug",))?;
    if debug_folder.call_method0("exists")?.is_truthy()? {
        g(py, "shutil")?.getattr("rmtree")?.call1((&debug_folder,))?;
    }

    // Create conversation directory and initialize fresh conversation file.
    // Only a run that saves its conversation clears the previous
    // conversation and raw_reasoning folders; one that does not leaves the
    // last saved run where it is.
    if me.getattr("save_conversation")?.is_truthy()? {
        let folders = [
            g(py, "Path")?.call1(("conversation",))?,
            g(py, "Path")?.call1(("raw_reasoning",))?,
        ];
        for folder in folders {
            if folder.call_method0("exists")?.is_truthy()? {
                g(py, "shutil")?.getattr("rmtree")?.call1((&folder,))?;
            }
        }
        me.setattr("conversation_dir", g(py, "Path")?.call1(("conversation",))?)?;
        me.getattr("conversation_dir")?.call_method("mkdir", (), Some(&keyword(py, "exist_ok", true)?))?;
        me.setattr("conversation_file", me.getattr("conversation_dir")?.div("conversation.txt")?)?;

        // Always start fresh - each program run is a new session
        initialize_conversation_file(me)?;

        // Create raw_reasoning directory for storing raw LLM outputs
        me.setattr("raw_reasoning_dir", g(py, "Path")?.call1(("raw_reasoning",))?)?;
        me.getattr("raw_reasoning_dir")?.call_method("mkdir", (), Some(&keyword(py, "exist_ok", true)?))?;
    }

    // Start fresh each session
    me.setattr("interaction_count", 0)?;

    // Resumable chat memory (UI path only). prior_history is an optimized
    // snapshot from a PRIOR run of this session, built and loaded by
    // AutoCua.agent_conversation.service (ALL memory management lives there,
    // not here). When present, process_request seeds the conversation lists
    // from it so the agent "remembers" earlier turns. Scratchpad and todo are
    // still wiped above: continuity comes ONLY from this memory, not files.
    // The final lists are exposed on the object so the conversation service
    // can persist them after the run.
    let history = if is_dict(&args.prior_history)? { args.prior_history.clone() } else { none(py) };
    me.setattr("prior_history", history)?;
    me.setattr("assistant_messages", PyList::empty(py))?;
    me.setattr("tool_responses", PyList::empty(py))?;
    // The exact final messages payload sent to the model (system + interleaved
    // history + the live user message that re-injects user_request, todo,
    // scratchpad and element_tree each step). Captured for the debug memory
    // log so the download is the TRUE conversation, not a reconstruction.
    me.setattr("last_messages", py.None())?;

    // Runtime memory compression (rolling handoff at 110k tokens). The
    // compression agent is a SEPARATE agent: ALL orchestration (trigger,
    // worker thread, indicator, splice policy) lives in its controller,
    // shared by every platform. This loop only calls the four hooks: reset,
    // maybe_trigger, apply_pending and finish_run. LLMManager is passed as a
    // class: the controller builds its own second text-mode manager from it.
    // The dump and entry hooks are the driver's OWN (view): the
    // memory_compression defaults parse the legacy 4-block JSON and cannot
    // read native turns. The driver's pair handles MIXED histories, legacy
    // steps from schema-era saves plus native ones from this run.
    let compression_cls = g(py, "CompressionController")?;
    let manager = me.getattr("llm_manager")?;
    let manager_cls = g(py, "LLMManager")?;
    let token_callback = me.getattr("token_callback")?;
    let stop_event = me.getattr("stop_event")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("dump_builder", g(py, "compression_dump")?)?;
    kwargs.set_item("synthetic_entry", g(py, "compression_entry")?)?;
    let compression = compression_cls.call((manager, manager_cls, token_callback, stop_event), Some(&kwargs))?;
    me.setattr("_compression", compression)?;
    Ok(())
}

/// Load the system prompt matching the speed mode (quality/fast), and
/// Claude's own prompt for quality mode on any Claude model.
fn load_system_prompt<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    let prompt_name = if me.getattr("speed")?.eq("fast")? {
        "fast_system_prompt.md"
    } else if me.getattr("_claude")?.is_truthy()? {
        "claude_prompt.md"
    } else {
        "system_prompt.md"
    };
    let read = (|| -> PyResult<Option<Obj<'py>>> {
        // current_dir = os.path.dirname(os.path.abspath(__file__))
        let dirname = g(py, "os")?.getattr("path")?.getattr("dirname")?;
        let abspath = g(py, "os")?.getattr("path")?.getattr("abspath")?;
        let current_dir = dirname.call1((abspath.call1((module_file(py)?,))?,))?;
        let prompt_path = g(py, "os")?.getattr("path")?.getattr("join")?.call1((current_dir, prompt_name))?;
        let manager = open_text(py, || Ok(prompt_path), "r")?;
        with(&manager, |file| file.call_method0("read"))
    })();
    match read {
        Ok(Some(prompt)) => Ok(prompt),
        Ok(None) => Ok(none(py)),
        Err(e) if e.is_instance_of::<PyFileNotFoundError>(py) => {
            let _handling = Handling::new(py, &e);
            let message = fstr(py, &[L(prompt_name), L(" file not found in the agent directory")])?;
            let error = py.get_type::<PyFileNotFoundError>().call1((message,))?;
            Err(raised_while_handling(py, PyErr::from_value(error), &e))
        }
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            let message = fstr(py, &[L("Error loading system prompt: "), V(&err_text(py, &e)?)])?;
            let error = py.get_type::<PyException>().call1((message,))?;
            Err(raised_while_handling(py, PyErr::from_value(error), &e))
        }
        Err(e) => Err(e),
    }
}

/// The four lines every conversation file starts with.
fn write_log_head<'py>(me: &Obj<'py>, f: &Obj<'py>) -> PyResult<()> {
    let py = me.py();
    write(f, || Ok(txt(py, "=== CONVERSATION LOG ===\n")))?;
    write(f, || {
        let now = g(py, "datetime")?
            .call_method0("now")?
            .call_method1("strftime", ("%Y-%m-%d %H:%M:%S",))?;
        fstr(py, &[L("Session Started: "), V(&now), L("\n")])
    })?;
    write(f, || {
        let provider = me.getattr("llm_manager")?.call_method0("get_provider_name")?;
        fstr(py, &[L("Provider: "), V(&provider), L("\n")])
    })?;
    write(f, || {
        let model = me.getattr("llm_manager")?.call_method0("get_model_name")?;
        fstr(py, &[L("Model: "), V(&model), L("\n")])
    })
}

/// Initialize the conversation file with header information.
fn initialize_conversation_file(me: &Obj<'_>) -> PyResult<()> {
    let py = me.py();
    let written = (|| -> PyResult<()> {
        let manager = open_text(py, || me.getattr("conversation_file"), "w")?;
        with(&manager, |f| {
            write_log_head(me, f)?;
            write(f, || Ok(txt(py, &format!("{RULE}\n\n"))))?;

            write(f, || Ok(txt(py, "=== SYSTEM PROMPT ===\n")))?;
            write(f, || me.getattr("system_prompt"))?;
            write(f, || Ok(txt(py, &format!("\n\n{RULE}\n\n"))))
        })?;
        Ok(())
    })();
    match written {
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L("Error initializing conversation file: "), V(&err_text(py, &e)?)])?)
        }
        other => other,
    }
}

/// A message's content as text. Cached messages carry content as a list of
/// {type,text,...} blocks.
fn message_text<'py>(content: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = content.py();
    if is_list(content)? {
        // "\n".join(b.get("text", "") for b in content if isinstance(b, dict))
        let pieces = PyList::empty(py);
        for block in content.try_iter()? {
            let block = block?;
            let text = in_generator(py, || {
                if is_dict(&block)? {
                    block.call_method1("get", ("text", "")).map(Some)
                } else {
                    Ok(None)
                }
            })?;
            if let Some(text) = text {
                pieces.append(text)?;
            }
        }
        return txt(py, "\n").call_method1("join", (pieces,));
    }
    if is_str(content)? {
        Ok(content.clone())
    } else {
        Ok(content.str()?.into_any())
    }
}

/// Save a numbered conversation file rendering the EXACT payload sent this
/// step (system + interleaved assistant/user turns + current user message)
/// plus this step's freshly generated response: a faithful peek into agent
/// memory.
fn save_conversation_snapshot<'py>(
    me: &Obj<'py>,
    messages: &Obj<'py>,
    current_assistant_response: &Obj<'py>,
    image_sent: &Obj<'py>,
    interaction_count: &Obj<'py>,
) -> PyResult<()> {
    let py = me.py();
    let saved = (|| -> PyResult<()> {
        let name = fstr(py, &[L("conversation_"), V(interaction_count), L(".txt")])?;
        let conversation_file = me.getattr("conversation_dir")?.div(name)?;

        let manager = open_text(py, || Ok(conversation_file), "w")?;
        with(&manager, |f| {
            // Header
            write_log_head(me, f)?;
            write(f, || fstr(py, &[L("Current Interaction: #"), V(interaction_count), L("\n")]))?;
            write(f, || Ok(txt(py, &format!("{RULE}\n\n"))))?;

            // Render every message in the exact order it was sent to the model.
            for message in messages.try_iter()? {
                let m = message?;
                let role = m.call_method1("get", ("role", "?"))?.str()?.call_method0("upper")?;
                if role.eq("SYSTEM")? {
                    write(f, || Ok(txt(py, "=== SYSTEM PROMPT ===\n")))?;
                    write(f, || message_text(&m.call_method1("get", ("content", ""))?))?;
                    write(f, || Ok(txt(py, &format!("\n\n{RULE}\n\n"))))?;
                } else {
                    let content = m.call_method1("get", ("content", ""))?;
                    // The `reasoning` call's `ok`: wire plumbing, not memory
                    if role.eq("TOOL")? && message_text(&content)?.eq(g(py, "REASONING_RESULT")?)? {
                        continue;
                    }
                    write(f, || fstr(py, &[L("--- "), V(&role), L(" ---\n")]))?;
                    // A native assistant turn carries its substance in the
                    // tool_calls (the `reasoning` call, or the notes on the
                    // action calls in fast mode), so render it the readable
                    // block way. Otherwise every replayed step shows
                    // up blank.
                    if m.call_method1("get", ("tool_calls",))?.is_truthy()? {
                        write(f, || {
                            let snapshot_turn = g(py, "snapshot_turn")?;
                            let said = message_text(&m.call_method1("get", ("content", ""))?)?;
                            let calls = m.get_item("tool_calls")?;
                            snapshot_turn.call1((said, calls))
                        })?;
                    } else {
                        write(f, || message_text(&m.call_method1("get", ("content", ""))?))?;
                    }
                    write(f, || Ok(txt(py, "\n\n")))?;
                }
            }

            if image_sent.is_truthy()? {
                write(f, || Ok(txt(py, "[Screenshot sent with the latest user message]\n\n")))?;
            }

            // This step's freshly generated response (the reply, not part of the request).
            write(f, || fstr(py, &[L("--- ASSISTANT (response · step "), V(interaction_count), L(") ---\n")]))?;
            write(f, || Ok(current_assistant_response.clone()))?;
            write(f, || Ok(txt(py, "\n")))
        })?;

        say(py, &fstr(py, &[L("✓ Memory snapshot saved: conversation_"), V(interaction_count), L(".txt")])?)
    })();
    match saved {
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L("Error saving conversation snapshot: "), V(&err_text(py, &e)?)])?)
        }
        other => other,
    }
}

/// Save raw LLM response before any parsing/normalization.
fn save_raw_response<'py>(me: &Obj<'py>, raw_response: &Obj<'py>, step_number: &Obj<'py>) -> PyResult<()> {
    let py = me.py();
    if !me.getattr("save_conversation")?.is_truthy()? {
        return Ok(());
    }
    let saved = (|| -> PyResult<()> {
        let name = fstr(py, &[L("raw_response_"), V(step_number), L(".txt")])?;
        let raw_file = me.getattr("raw_reasoning_dir")?.div(name)?;
        let manager = open_text(py, || Ok(raw_file), "w")?;
        with(&manager, |f| write(f, || Ok(raw_response.clone())))?;
        Ok(())
    })();
    match saved {
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L("⚠ Error saving raw response: "), V(&err_text(py, &e)?)])?)
        }
        other => other,
    }
}

/// Save conversation snapshot to file, when the run saves its conversation.
fn save_conversation<'py>(
    me: &Obj<'py>,
    messages: &Obj<'py>,
    current_assistant_response: &Obj<'py>,
    image_sent: &Obj<'py>,
    interaction_count: &Obj<'py>,
) -> PyResult<()> {
    if me.getattr("save_conversation")?.is_truthy()? {
        save_conversation_snapshot(me, messages, current_assistant_response, image_sent, interaction_count)?;
    }
    Ok(())
}

/// The text of one of the agent's state files, stripped: "" when the file is
/// not there, and "" with a printed line when the read fails. The path is
/// worked out inside the try, as in Python.
fn read_own_file<'py>(
    py: Python<'py>,
    path: impl FnOnce() -> PyResult<Obj<'py>>,
    failed: &str,
) -> PyResult<Obj<'py>> {
    let read = (|| -> PyResult<Obj<'py>> {
        let file = path()?;
        if file.call_method0("exists")?.is_truthy()? {
            let manager = open_text(py, || Ok(file), "r")?;
            let text = with(&manager, |f| f.call_method0("read")?.call_method0("strip"))?;
            Ok(text.unwrap_or_else(|| none(py)))
        } else {
            Ok(txt(py, ""))
        }
    })();
    match read {
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L(failed), V(&err_text(py, &e)?)])?)?;
            Ok(txt(py, ""))
        }
        other => other,
    }
}

/// Read the current todo list from the task tracker's todo.md file. The path
/// is the task tracker's own (single source of truth, exactly where it
/// writes), so read can never drift from write.
fn read_todo_from_file<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    read_own_file(
        py,
        || {
            let path_cls = g(py, "Path")?;
            path_cls.call1((me.getattr("controller")?.getattr("task_tracker")?.getattr("todo_file")?,))
        },
        "⚠ Error reading todo file: ",
    )
}

/// Read the current scratchpad entries from scratchpad/milestone/
/// milestone.md. The controller has no scratchpad service on iOS yet, so the
/// file is read where the macOS layout keeps it (the scratchpad handler to
/// come must write there).
fn read_scratchpad_from_file(py: Python<'_>) -> PyResult<Obj<'_>> {
    read_own_file(
        py,
        || {
            // Path(__file__).parent.parent.parent / "scratchpad" / "milestone" / "milestone.md"
            g(py, "Path")?
                .call1((module_file(py)?,))?
                .getattr("parent")?
                .getattr("parent")?
                .getattr("parent")?
                .div("scratchpad")?
                .div("milestone")?
                .div("milestone.md")
        },
        "⚠ Error reading scratchpad file: ",
    )
}

/// <persistent_memory>: the agent's own todo list and scratchpad, then
/// <vault> (the login keys saved for the app in front, when there are some),
/// rebuilt fresh every step. It is the tag the system prompt's <input>
/// describes.
fn persistent_memory<'py>(
    todo_list: &Obj<'py>,
    scratchpad_content: &Obj<'py>,
    vault_block: &Obj<'py>,
) -> PyResult<Obj<'py>> {
    let py = todo_list.py();
    let scratch = if scratchpad_content.is_truthy()? {
        fstr(py, &[L("<scratchpad>\n"), V(scratchpad_content), L("\n</scratchpad>")])?
    } else {
        txt(py, "<scratchpad>none</scratchpad>")
    };
    let tail = if vault_block.is_truthy()? { fstr(py, &[L("\n\n"), V(vault_block)])? } else { txt(py, "") };
    fstr(
        py,
        &[
            L("<persistent_memory>\n<todo_list>\n"),
            V(todo_list),
            L("\n</todo_list>\n\n"),
            V(&scratch),
            V(&tail),
            L("\n</persistent_memory>"),
        ],
    )
}

/// Build the compact tool_response preserved in agent memory for a step.
///
/// Only web-tool results are compacted: their raw text is already removed
/// and digested into the scratchpad, so memory keeps just the query and a
/// pointer. Web is compacted ONLY when it succeeded: a failed web call keeps
/// its exact response so the agent can see what went wrong. Every other
/// result (shell output, click and input results, and so on) is preserved
/// verbatim.
fn tool_response_for_memory<'py>(action_result: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = action_result.py();
    let res = py.import("copy")?.getattr("deepcopy")?.call1((action_result,))?;

    let compact = |entry: Obj<'py>| -> PyResult<Obj<'py>> {
        if is_dict(&entry)?
            && entry.call_method1("get", ("tool",))?.eq("web")?
            && entry.call_method1("get", ("status",))?.eq("success")?
        {
            let pointer = PyDict::new(py);
            pointer.set_item("action", "tool")?;
            pointer.set_item("tool", "web")?;
            pointer.set_item("query", entry.call_method1("get", ("query", ""))?)?;
            pointer.set_item("message", "memory optimized — refer to scratchpad for the web result")?;
            return Ok(pointer.into_any());
        }
        Ok(entry)
    };

    if res.call_method1("get", ("action",))?.eq("multiple")? && res.contains("results")? {
        let results = PyList::empty(py);
        for entry in res.get_item("results")?.try_iter()? {
            results.append(compact(entry?)?)?;
        }
        res.set_item("results", results)?;
        return Ok(res);
    }
    compact(res)
}

/// Replace a web step's 'refer to scratchpad' placeholder with the actual
/// numbered findings the agent distilled into the scratchpad on the digest
/// step. The scratchpad is wiped on the next user request, so this keeps the
/// distilled web info in durable conversation memory. Preserves the
/// surrounding shape (a single web result or a 'multiple' results list).
fn backfill_web_findings<'py>(tool_response_str: &Obj<'py>, findings: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = tool_response_str.py();
    let loaded = (|| g(py, "json")?.getattr("loads")?.call1((tool_response_str,)))();
    let data = match loaded {
        Ok(data) => data,
        Err(e) if is_exception(py, &e) => return Ok(tool_response_str.clone()),
        Err(e) => return Err(e),
    };

    let fill = |entry: Obj<'py>| -> PyResult<Obj<'py>> {
        if is_dict(&entry)? && entry.call_method1("get", ("tool",))?.eq("web")? {
            entry.call_method1("pop", ("message", py.None()))?;
            entry.set_item("web_result", findings)?;
        }
        Ok(entry)
    };

    let data = if is_dict(&data)? && data.call_method1("get", ("action",))?.eq("multiple")? && data.contains("results")? {
        let results = PyList::empty(py);
        for entry in data.get_item("results")?.try_iter()? {
            results.append(fill(entry?)?)?;
        }
        data.set_item("results", results)?;
        data
    } else {
        fill(data)?
    };
    dumps_pretty(py, || Ok(data))
}

/// Trim an OLDER history step for context: drop 'action' only. 'thinking' is
/// RETAINED in history (a skipped one costs nothing; FULL thinking is the
/// durable route rationale the prompt tells the agent to consult when
/// re-routing). Handles the leading '<Step_no=N />' marker.
///
/// LEGACY ONLY: native turns are append-only and never rewritten (rewriting
/// them would invalidate the prompt-cache prefix on every request), so this
/// now runs solely on schema-era entries from resumed chats.
fn trim_history_entry<'py>(entry: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = entry.py();
    let matcher = g(py, "re")?.getattr("match")?;
    let dotall = g(py, "re")?.getattr("DOTALL")?;
    let m = matcher.call1((STEP_PREFIX_PATTERN, entry, dotall))?;
    let (prefix, json_part) = if m.is_truthy()? {
        (m.call_method1("group", (1,))?, m.call_method1("group", (2,))?)
    } else {
        (txt(py, ""), entry.clone())
    };
    let trimmed = (|| -> PyResult<Obj<'py>> {
        let data = g(py, "json")?.getattr("loads")?.call1((&json_part,))?;
        data.call_method1("pop", ("action", py.None()))?;
        prefix.add(dumps_pretty(py, || Ok(data))?)
    })();
    match trimmed {
        Err(e) if is_exception(py, &e) => Ok(entry.clone()),
        other => other,
    }
}

/// Push a tool-flow event to the frontend bottom chain (best-effort).
fn emit_flow<'py>(me: &Obj<'py>, event: &Obj<'py>, payload: &Obj<'py>) -> PyResult<()> {
    let py = me.py();
    if !me.getattr("tool_callback")?.is_truthy()? {
        return Ok(());
    }
    let sent = (|| -> PyResult<()> {
        me.getattr("tool_callback")?.call1((event, payload))?;
        Ok(())
    })();
    match sent {
        Err(e) if is_exception(py, &e) => Ok(()),
        other => other,
    }
}

/// `self.stop_event and self.stop_event.is_set()`, as a truth value.
fn stop_requested(me: &Obj<'_>) -> PyResult<bool> {
    if !me.getattr("stop_event")?.is_truthy()? {
        return Ok(false);
    }
    me.getattr("stop_event")?.call_method0("is_set")?.is_truthy()
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// How a step ends: the loop goes on (Python's continue, or the end of the
/// loop body) or stops (break).
enum Flow {
    Next,
    Stop,
}

/// The variables of one request that live across its steps.
struct Run<'py> {
    step_number: i64,
    is_first_iteration: bool,
    /// Every assistant response, for memory. A real Python list:
    /// CompressionController splices it in place.
    assistant_messages: Obj<'py>,
    /// Per-step tool result, aligned 1:1 with assistant_messages; replayed
    /// as the turns after each step.
    tool_responses: Obj<'py>,
    /// The web tool's response, for the light digest step.
    pending_web_response: Obj<'py>,
    /// The tool_responses slot of the web step to backfill with the digest's
    /// scratchpad.
    web_memory_index: Obj<'py>,
    /// True only on the step that digests a web result into the scratchpad.
    is_web_digest: bool,
    /// Consecutive turns that produced no usable tool call (max 3).
    json_fail_count: i64,
    /// Consecutive notes-only steps (a reminder rides on the third).
    reasoning_only_count: i64,
    /// The outcome reported to the caller. "incomplete" until a step says
    /// otherwise, so a loop exit that sets nothing never looks like a
    /// completion.
    final_status: &'static str,
    final_message: Obj<'py>,
    is_resumed: bool,
    /// The chat's ORIGINAL objective (frozen to run 1 by agent_conversation).
    original_task: Obj<'py>,
    /// A step got as far as defining its closers. Until then the outer
    /// handler has nothing to close (Python's NameError guard).
    closers: bool,
    /// What the closers read: the step's calls, the notes call's results,
    /// the rejected calls' results and the turn as it was recorded. A later
    /// step that fails early leaves here what the step before it left.
    calls: Obj<'py>,
    note_results: Obj<'py>,
    reject_results: Obj<'py>,
    normalized: Obj<'py>,
}

impl<'py> Run<'py> {
    fn finish(&mut self, status: &'static str, message: &str) {
        self.final_status = status;
        self.final_message = txt(self.final_message.py(), message);
    }
}

/// What one step gathered before it asked the model.
struct Step<'py> {
    messages: Obj<'py>,
    annotated_image_base64: Obj<'py>,
    image_sent: bool,
}

/// What the model answered, once its calls were sorted.
struct Reply<'py> {
    raw_response: Obj<'py>,
    track: Obj<'py>,
    resp_text: Obj<'py>,
}

// -- Closing a step's call/result pairing ------------------------------------
// A saved tool call with no matching result is a malformed transcript every
// provider rejects on resume, so the slot must never stay None once the turn
// is recorded. There are exactly three honest ways to close it, and each
// break path picks the one that matches what actually happened:
//   pair_results  - the actions RAN: record what they returned.
//   discard_step  - nothing ran: drop the turn entirely rather than invent a
//                   result for a call that never executed (same contract as
//                   the post-LLM stop check, which discards the response).
//   close_pairing - the run died mid-flight: record the error.

/// Record what the calls actually returned, keyed per call: one result each
/// when the controller returned a matching batch, otherwise the first call
/// carries the whole envelope and the rest point at it. Web results are
/// compacted to a scratchpad pointer by tool_response_for_memory.
fn pair_results<'py>(run: &Run<'py>, action_result: &Obj<'py>) -> PyResult<()> {
    let py = action_result.py();
    let compacted = tool_response_for_memory(action_result)?;
    let batch = if is_dict(&compacted)? { compacted.call_method1("get", ("results",))? } else { none(py) };
    let calls = &run.calls;
    let paired = PyList::empty(py);
    if calls.is_truthy()? && is_list(&batch)? && batch.len()? == calls.len()? {
        for (idx, call) in calls.try_iter()?.enumerate() {
            let entry = PyDict::new(py);
            entry.set_item("tool_call_id", call?.get_item("id")?)?;
            entry.set_item("content", dumps_pretty(py, || batch.get_item(idx))?)?;
            paired.append(entry)?;
        }
    } else if calls.is_truthy()? {
        let envelope = dumps_pretty(py, || Ok(compacted))?;
        let first = PyDict::new(py);
        first.set_item("tool_call_id", calls.get_item(0)?.get_item("id")?)?;
        first.set_item("content", envelope)?;
        paired.append(first)?;
        for call in calls.get_item(slice(&int(py, 1)?, &none(py))?)?.try_iter()? {
            let entry = PyDict::new(py);
            entry.set_item("tool_call_id", call?.get_item("id")?)?;
            entry.set_item("content", "(result included in the first tool result of this batch)")?;
            paired.append(entry)?;
        }
    }
    if run.tool_responses.is_truthy()? {
        let encode_results = g(py, "encode_results")?;
        let results = run.note_results.add(&run.reject_results)?.add(&paired)?;
        run.tool_responses.set_item(-1, encode_results.call1((results,))?)?;
    }
    Ok(())
}

/// Drop this step's turn and its empty slot: nothing ran, so there is
/// nothing to remember. Keeps the two lists 1:1 and leaves the saved chat
/// ending on the last step that really happened. True when it dropped
/// something.
fn discard_step(run: &Run<'_>) -> PyResult<bool> {
    if run.assistant_messages.is_truthy()?
        && run.tool_responses.is_truthy()?
        && run.tool_responses.get_item(-1)?.is_none()
        && run.assistant_messages.get_item(-1)?.is(&run.normalized)
    {
        run.assistant_messages.call_method0("pop")?;
        run.tool_responses.call_method0("pop")?;
        return Ok(true);
    }
    Ok(false)
}

/// Last resort for a run dying mid-step (exceptions): record the note
/// against every call so nothing dangles.
fn close_pairing<'py>(run: &Run<'py>, note: &Obj<'py>) -> PyResult<()> {
    let py = note.py();
    if run.tool_responses.is_truthy()?
        && run.tool_responses.get_item(-1)?.is_none()
        && (run.calls.is_truthy()? || run.reject_results.is_truthy()? || run.note_results.is_truthy()?)
    {
        let encode_results = g(py, "encode_results")?;
        let results = run.note_results.add(&run.reject_results)?;
        let noted = PyList::empty(py);
        for call in run.calls.try_iter()? {
            let entry = PyDict::new(py);
            entry.set_item("tool_call_id", call?.get_item("id")?)?;
            entry.set_item("content", note)?;
            noted.append(entry)?;
        }
        run.tool_responses.set_item(-1, encode_results.call1((results.add(noted)?,))?)?;
    }
    Ok(())
}

/// The block view the terminal print, the frontend text stream and the
/// tool-icon chain read; the transcript sent to the model does not. Only
/// the params this mode carries: quality has all three (memory + next_goal
/// on Claude), fast has next_goal alone. Rendering one the mode dropped
/// would print an empty block in the terminal and the UI stream.
fn display_payload<'py>(py: Python<'py>, reply: &Reply<'py>, action: &Obj<'py>) -> PyResult<Obj<'py>> {
    let track = &reply.track;
    let display = PyDict::new(py);
    if track.contains("thinking")? {
        // track["thinking"] or resp_text or SKIPPED_THINKING
        let thinking = track.get_item("thinking")?;
        let thinking = if thinking.is_truthy()? {
            thinking
        } else if reply.resp_text.is_truthy()? {
            reply.resp_text.clone()
        } else {
            g(py, "SKIPPED_THINKING")?
        };
        display.set_item("thinking", thinking)?;
    }
    if track.contains("memory")? {
        display.set_item("memory", track.get_item("memory")?)?;
    }
    if track.contains("next_goal")? {
        display.set_item("next_goal", track.get_item("next_goal")?)?;
    }
    display.set_item("action", action)?;
    dumps_pretty(py, || Ok(display.into_any()))
}

/// `AgentResponseFormatter.format_response(display_payload, include_action=...)`.
fn format_response<'py>(py: Python<'py>, display_payload: &Obj<'py>, include_action: bool) -> PyResult<Obj<'py>> {
    let format_response = g(py, "AgentResponseFormatter")?.getattr("format_response")?;
    format_response.call((display_payload,), Some(&keyword(py, "include_action", include_action)?))
}

/// Process a user request in an iterative loop until completion.
///
/// Returns a structured outcome {"status", "message"} so callers (Telegram,
/// web UI) can tell a real completion from a failure instead of always
/// reporting "done":
///   - "success"    a `done` action ran
///   - "error"      a critical exception ended the loop (e.g. API failure)
///   - "incomplete" the loop ended otherwise (stopped, max steps, parse fails)
fn process_request<'py>(me: &Obj<'py>, task: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    let mut run = Run {
        step_number: 0,
        is_first_iteration: true,
        assistant_messages: PyList::empty(py).into_any(),
        tool_responses: PyList::empty(py).into_any(),
        pending_web_response: none(py),
        web_memory_index: none(py),
        is_web_digest: false,
        json_fail_count: 0,
        reasoning_only_count: 0,
        final_status: "incomplete",
        final_message: txt(py, "Agent stopped before completing the task"),
        is_resumed: false,
        original_task: task.clone(),
        closers: false,
        calls: none(py),
        note_results: none(py),
        reject_results: none(py),
        normalized: none(py),
    };

    // Runtime compression: new call invalidates any stale worker from a
    // prior process_request on this instance.
    me.getattr("_compression")?.call_method0("reset")?;

    // ---- Resumable memory seed (UI continuation) --------------------------
    // If a prior optimized snapshot was supplied (continuing a saved chat),
    // replay it into the two memory lists so the message-build loop re-emits
    // the earlier conversation. Entries are ALREADY trimmed and
    // tool_responses already compacted, so no re-processing. The saved
    // memory ends with a terminal-note step, so on resume that note is the
    // most-recent entry, which makes the loop replay EVERY real step's
    // tool_response.
    if me.getattr("prior_history")?.is_truthy()? {
        let seeded = me.getattr("prior_history")?.call_method1("get", ("assistant_messages",))?;
        let seeded = if seeded.is_truthy()? { seeded } else { PyList::empty(py).into_any() };
        if seeded.is_truthy()? {
            run.assistant_messages = py.get_type::<PyList>().call1((&seeded,))?;
            // tool_responses = list(... or [])[:len(assistant_messages)]
            let saved = me.getattr("prior_history")?.call_method1("get", ("tool_responses",))?;
            let saved = if saved.is_truthy()? { saved } else { PyList::empty(py).into_any() };
            let saved = py.get_type::<PyList>().call1((saved,))?;
            let steps = run.assistant_messages.len()?;
            run.tool_responses = saved.get_item(slice(&none(py), &int(py, steps as i64)?)?)?;
            // tool_responses += [None] * (len(assistant_messages) - len(tool_responses))
            let missing = run.assistant_messages.len()?.saturating_sub(run.tool_responses.len()?);
            let padding = PyList::new(py, (0..missing).map(|_| py.None()))?;
            run.tool_responses = iadd(&run.tool_responses, padding.as_any())?;
            run.is_resumed = true;
            // Continuation, not a fresh dialogue
            run.is_first_iteration = false;
            // last_response = self.prior_history.get("done_message") or "Resuming previous session."
            me.getattr("prior_history")?.call_method1("get", ("done_message",))?.is_truthy()?;
            // The chat's ORIGINAL objective (frozen to run 1 by
            // _build_history); the no="1" request prepend must carry this,
            // not the newest request.
            let first_task = me.getattr("prior_history")?.call_method1("get", ("task",))?;
            run.original_task = if first_task.is_truthy()? { first_task } else { task.clone() };
            // Label this run boundary: fill the terminal bridge's empty tool
            // slot with a numbered request marker so this run's task is
            // durably attributed to the steps that follow it (the filled
            // slot is part of the persisted lists, so it survives every
            // save/seed cycle).
            if run.tool_responses.get_item(-1)?.is_none() && {
                let signature = g(py, "_BRIDGE_SIGNATURE")?;
                run.assistant_messages.get_item(-1)?.contains(signature)?
            } {
                // Next marker number: compression can swallow bridge
                // entries, so count BOTH remaining bridges and existing
                // marker numbers and continue past the highest.
                let mut n_bridges: i64 = 0;
                for entry in run.assistant_messages.try_iter()? {
                    let entry = entry?;
                    let is_bridge = in_generator(py, || {
                        let signature = g(py, "_BRIDGE_SIGNATURE")?;
                        entry.str()?.contains(signature)
                    })?;
                    if is_bridge {
                        n_bridges += 1;
                    }
                }
                let marker_nos = PyList::empty(py);
                for slot in run.tool_responses.try_iter()? {
                    let slot = slot?;
                    if is_str(&slot)? {
                        let findall = g(py, "re")?.getattr("findall")?;
                        for number in findall.call1((MARKER_NO_PATTERN, &slot))?.try_iter()? {
                            marker_nos.append(py.get_type::<PyInt>().call1((number?,))?)?;
                        }
                    }
                }
                let request_marker = g(py, "_request_marker")?;
                let numbers = PyList::new(py, [n_bridges])?.as_any().add(marker_nos)?;
                let next = g(py, "max")?.call1((numbers,))?.add(1)?;
                run.tool_responses.set_item(-1, request_marker.call1((next, task))?)?;
            }
            say_text(py, &format!("🧠 Resumed memory: {} prior step(s) loaded", run.assistant_messages.len()?))?;
        }
    }
    // A fresh chat has typed nothing from the vault yet. A continuation keeps
    // what it typed hidden, since the phone may still show it.
    if !run.is_resumed {
        g(py, "vault")?.call_method0("new_chat")?;
    }
    // -----------------------------------------------------------------------

    // Print model info once at the start
    let model_name = me.getattr("llm_manager")?.call_method0("get_model_name")?;
    say(py, &fstr(py, &[L("\n🔄 Processing with "), V(&model_name)])?)?;
    // Tool-flow chain: clear + take over the demo
    emit_flow(me, &txt(py, "run_start"), &none(py))?;

    // Main agent loop
    loop {
        match step(me, task, &mut run)? {
            Flow::Next => continue,
            Flow::Stop => break,
        }
    }

    // Tool-flow chain: if the run didn't finish cleanly, cap it with a "!" drop.
    if run.final_status == "error" {
        let payload = dict_of(py, &[("text", &txt(py, "llm service not responding"))])?;
        emit_flow(me, &txt(py, "error"), &payload)?;
    } else if run.final_status == "incomplete"
        && run.final_message.is_truthy()?
        && run.final_message.call_method0("lower")?.contains("stop")?
    {
        let payload = dict_of(py, &[("text", &txt(py, "agent interrupted"))])?;
        emit_flow(me, &txt(py, "error"), &payload)?;
    }
    // Tool-flow chain: run finished
    emit_flow(me, &txt(py, "run_end"), &none(py))?;

    // Compression still pending at run end (in flight or deposited but never
    // applied): the result is discarded; make sure the indicator stops.
    me.getattr("_compression")?.call_method0("finish_run")?;

    // Expose the final conversation lists so the conversation service (NOT
    // the agent) can optimize and persist them after the run. Memory
    // management is kept entirely out of the agent.
    me.setattr("assistant_messages", &run.assistant_messages)?;
    me.setattr("tool_responses", &run.tool_responses)?;

    let outcome = PyDict::new(py);
    outcome.set_item("status", run.final_status)?;
    outcome.set_item("message", &run.final_message)?;
    Ok(outcome.into_any())
}

/// One pass of the loop. Everything up to the model call sits outside any
/// try in the Python: an error there leaves process_request. The rest is the
/// outer try (ask), whose errors end the run with status "error".
fn step<'py>(me: &Obj<'py>, task: &Obj<'py>, run: &mut Run<'py>) -> PyResult<Flow> {
    let py = me.py();

    // Check for stop signal
    if stop_requested(me)? {
        say_text(py, "\n🛑 Agent stopped by user.")?;
        // Don't send callback to frontend to avoid re-opening the strip
        run.finish("incomplete", "Stopped by user");
        return Ok(Flow::Stop);
    }

    // Apply a finished background compression: the controller splices the
    // lists IN PLACE here on the main thread (its worker only deposits) and
    // returns the shifted web_memory_index.
    run.web_memory_index = me.getattr("_compression")?.call_method1(
        "apply_pending",
        (&run.assistant_messages, &run.tool_responses, &run.web_memory_index),
    )?;

    run.step_number += 1;
    // Set true below only when this step digests a web result
    run.is_web_digest = false;

    // Max step limit: prevent infinite loops
    if run.step_number > 100 {
        say_text(py, "\n🛑 Max step limit reached (100). Exiting agent loop.")?;
        run.finish("incomplete", "Reached the 100-step limit without finishing");
        return Ok(Flow::Stop);
    }

    // Tool-flow chain: announce the turn. Digest iterations send no image,
    // so the chain skips screenshot+mapping (true to the agent's behaviour).
    let has_image = boolean(py, !run.pending_web_response.is_truthy()?);
    emit_flow(me, &txt(py, "turn"), &dict_of(py, &[("hasImage", &has_image)])?)?;

    let mut annotated_image_base64;
    let mut image_sent;
    let formatted_element_tree;
    let skills_block;
    let vault_block;
    // Skip scan for light digest iterations (web tool response)
    if run.pending_web_response.is_truthy()? {
        say_text(py, &format!("\n{RULE}"))?;
        say_text(py, &format!("🌐 Step {}: Web digest iteration (no scan)", run.step_number))?;
        annotated_image_base64 = none(py);
        image_sent = false;
        formatted_element_tree = txt(py, "");
        skills_block = txt(py, "");
        vault_block = txt(py, "");
    } else {
        // Scan UI elements and get annotated screenshot
        say_text(py, &format!("\n{RULE}"))?;
        say_text(py, &format!("🔍 Step {}: Scanning snapshot.", run.step_number))?;
        me.getattr("scanner")?.call_method0("scan_elements")?;

        // Check Stop AFTER Scan
        if stop_requested(me)? {
            run.finish("incomplete", "Stopped by user");
            return Ok(Flow::Stop);
        }

        // Already encoded at its final size/format by the scanner
        // (LLM_IMAGE_* in tree/element.py): re-compressing here would only
        // add a decode/encode round trip and destroy the labels.
        let [element_tree_text, image] = unpack(&me.getattr("scanner")?.call_method0("get_scan_data")?)?;
        annotated_image_base64 = image;

        image_sent = !annotated_image_base64.is_none();
        if image_sent {
            say_text(py, &format!("✅ Image captured - annotated: {} chars", annotated_image_base64.len()?))?;
        } else {
            say_text(py, "❌ NO IMAGE - annotated image is None!")?;
        }

        // Wrap element tree in proper tags. A value the vault typed shows in
        // the field it went into (and anywhere the app echoes it): it reads
        // as [vault].
        let shown_tree = g(py, "vault")?.call_method1("hide", (&element_tree_text,))?;
        formatted_element_tree = fstr(py, &[L("<element_tree>\n"), V(&shown_tree), L("\n</element_tree>")])?;
        // <vault>: the login keys saved for the app just scanned.
        vault_block = g(py, "vault")?.getattr("vault_service")?.call_method0("block")?;
        // Skills are matched against the screen we JUST scanned: the app in
        // front is the root of the page source the scanner parsed.
        let get_knowledge = me.getattr("skills")?.getattr("get_knowledge")?;
        let skills_text = get_knowledge.call1((me.getattr("scanner")?.getattr("application_name")?,))?;
        skills_block = if skills_text.is_truthy()? {
            fstr(py, &[L("<skills>\n"), V(&skills_text), L("\n</skills>")])?
        } else {
            txt(py, "")
        };
        if skills_block.is_truthy()? {
            let print = g(py, "print")?;
            let application_name = me.getattr("scanner")?.getattr("application_name")?;
            print.call1((fstr(py, &[L("📘 skills: loaded for "), V(&application_name)])?,))?;
        }
    }

    // Construct user message based on iteration
    let mut user_message;
    if run.is_first_iteration {
        // First iteration: user_request + element tree only. ToDo creation
        // and timing are governed by <todo_capability> in the system prompt
        // (flexible: iteration 1 by default, may create later), so no
        // hard-coded todo rules are injected here.
        user_message = fstr(py, &[L("<user_request>\n"), V(task), L("\n</user_request>\n")])?;
        if skills_block.is_truthy()? {
            user_message = user_message.add(fstr(py, &[L("\n"), V(&skills_block), L("\n")])?)?;
        }
        if vault_block.is_truthy()? {
            user_message = user_message.add(fstr(py, &[L("\n"), V(&vault_block), L("\n")])?)?;
        }
        user_message = user_message.add(fstr(py, &[L("\n"), V(&formatted_element_tree)])?)?;
        if image_sent {
            user_message = user_message.add("\n\n<image>Annotated screenshot with bounding boxes</image>")?;
        }
    } else {
        // Fetch fresh todo and scratchpad entries from the file system
        let todo_list = read_todo_from_file(me)?;
        let scratchpad_content = read_scratchpad_from_file(py)?;

        // Subsequent iterations: request + todo_list. No <last_response>
        // block: under native tool calling each result arrives in its own
        // place, as a role:"tool" turn keyed to the call that produced it.
        // On a RESUMED session, mark the request as <updated_user_request>
        // so the agent knows this is a continuation of the same session (its
        // prior steps are already in the replayed history above).
        let request_tag = if run.is_resumed { "updated_user_request" } else { "user_request" };
        user_message = fstr(py, &[L("<"), L(request_tag), L(">\n"), V(task), L("\n</"), L(request_tag), L(">")])?;

        if run.pending_web_response.is_truthy()? {
            // The f-string built <persistent_memory> last, after it had
            // formatted the web response and the task.
            let head = fstr(
                py,
                &[
                    L("<critical>\nNo image and element tree provided. Focus on digesting the web response below.\n1. Analyze thoroughly - extract all relevant data (numbers, dates, names, URLs, prices, etc.)\n2. Save ALL important findings to scratchpad in this step's action.\n</critical>\n\n"),
                    V(&run.pending_web_response),
                    L("\n\n<user_request>\n"),
                    V(task),
                    L("\n</user_request>\n\n"),
                ],
            )?;
            user_message = head.add(persistent_memory(&todo_list, &scratchpad_content, &txt(py, ""))?)?;

            image_sent = false;
            annotated_image_base64 = none(py);

            // Clear flag after consumption; mark this as the web-digest step
            // so its scratchpad response can be folded into the web memory
            // below.
            run.pending_web_response = none(py);
            run.is_web_digest = true;
        } else {
            // Normal iteration: include full context
            let memory = persistent_memory(&todo_list, &scratchpad_content, &vault_block)?;
            user_message = user_message.add(fstr(py, &[L("\n\n"), V(&memory)])?)?;

            if skills_block.is_truthy()? {
                user_message = user_message.add(fstr(py, &[L("\n\n"), V(&skills_block)])?)?;
            }

            user_message = user_message.add(fstr(py, &[L("\n\n"), V(&formatted_element_tree)])?)?;

            // Add image tag if image is provided
            if image_sent {
                user_message = user_message.add("\n\n<image>Annotated screenshot with bounding boxes</image>")?;
                user_message = user_message.add("\n\n<critical>Pure vision first: decide which element to interact with from the screenshot alone, then refer to <element_tree> for its [id].</critical>")?;
            }
        }
    }

    // Claude: the notes call and the action calls belong in ONE response
    // (see CLAUDE_BATCH_REMINDER). Last in the live message.
    if me.getattr("_claude")?.is_truthy()? {
        user_message = iadd(&user_message, &g(py, "CLAUDE_BATCH_REMINDER")?)?;
    }

    // Build the API messages as a real NATIVE dialogue:
    //   system -> opening user turn (the objective) -> for each past step:
    //   assistant(prose + its tool_calls) -> one role:"tool" result per
    //   call -> finally the live user message (screen + state).
    //
    // APPEND-ONLY: nothing already in the transcript is ever rewritten, so
    // the prompt-cache prefix stays valid across steps.
    //
    // EVERY step replays its results, including the most recent: a saved
    // tool call with no matching result is a malformed transcript that
    // every provider rejects.
    //
    // Legacy (schema-era) entries degrade to content-only assistant turns
    // and keep the old sliding-window trim + <tool_response> user-turn
    // replay, so resumed chats look exactly as they did before.
    let messages = PyList::empty(py);
    messages.append(dict_of(py, &[("role", &txt(py, "system")), ("content", &me.getattr("system_prompt")?)])?)?;

    // Opening turn: the chat's ORIGINAL objective in the same numbered tag
    // the run-boundary markers use, so the transcript head is stable for the
    // whole chat and follow-ups extend the cached prefix.
    if run.assistant_messages.is_truthy()? {
        let marker = g(py, "_request_marker")?.call1((1, &run.original_task))?;
        messages.append(dict_of(py, &[("role", &txt(py, "user")), ("content", &marker)])?)?;
    }

    let n_hist = run.assistant_messages.len()? as i64;
    for (i, step_msg) in run.assistant_messages.try_iter()?.enumerate() {
        let step_msg = step_msg?;
        let is_recent = i as i64 == n_hist - 1;
        let step = g(py, "decode_step")?.call1((&step_msg,))?;
        // native = bool(step["tool_calls"]) or _looks_native(step_msg)
        let native = step.get_item("tool_calls")?.is_truthy()?
            || g(py, "_looks_native")?.call1((&step_msg,))?.is_truthy()?;
        if native {
            let asst = PyDict::new(py);
            asst.set_item("role", "assistant")?;
            asst.set_item("content", or_empty(step.get_item("content")?)?)?;
            if step.get_item("tool_calls")?.is_truthy()? {
                asst.set_item("tool_calls", step.get_item("tool_calls")?)?;
            }
            if step.call_method1("get", ("meta",))?.is_truthy()? {
                // The provider's OWN metadata for this turn (Gemini 3
                // thought signatures, OpenRouter reasoning blocks): the
                // provider translates it back into its dialect; every other
                // provider never sees the key.
                asst.set_item("provider_meta", step.get_item("meta")?)?;
            }
            messages.append(asst)?;
        } else {
            // Legacy step: keep the old trim for older entries.
            let content = if is_recent { step_msg.clone() } else { trim_history_entry(&step_msg)? };
            messages.append(dict_of(py, &[("role", &txt(py, "assistant")), ("content", &content)])?)?;
        }

        let tr = if i < run.tool_responses.len()? { run.tool_responses.get_item(i)? } else { none(py) };
        if tr.is_truthy()? {
            // decode_results emits role:"tool" turns for native results, a
            // bare user turn for a run-boundary marker, and a
            // <tool_response>-wrapped user turn for legacy slots.
            let extend = messages.getattr("extend")?;
            extend.call1((g(py, "decode_results")?.call1((&tr,))?,))?;
        }
    }

    // Live user message (screen + todo + scratchpad + element tree).
    // Rebuilt every step and never persisted; the screenshot attaches to
    // it. The system prompt is added exactly once (above): the resumable
    // seed never carries a system prompt, so a continued session can't
    // double it.
    messages.append(dict_of(py, &[("role", &txt(py, "user")), ("content", &user_message)])?)?;

    // Capture the exact payload for the debug memory log (the last
    // iteration's value is retained: the agent's final true memory).
    me.setattr("last_messages", &messages)?;

    // Prompt caching (OpenRouter/Anthropic/AWS): mark the newest PERSISTENT
    // turn, the last tool result, or the opening user turn on step 1. Never
    // the live user message: it is rebuilt and dropped every request, so a
    // breakpoint there is written once and never read back. Never an
    // assistant turn either: Anthropic's translation flattens those and
    // would silently drop the breakpoint. The last tool result survives into
    // the next request's prefix, so each call reads the cache the previous
    // one wrote.
    let provider_name = me.getattr("llm_manager")?.call_method0("get_provider_name")?;
    if PyTuple::new(py, ["openrouter", "anthropic", "aws"])?.contains(provider_name)? && messages.len() > 2 {
        for cache_msg in g(py, "reversed")?.call1((&messages,))?.try_iter()? {
            let cache_msg = cache_msg?;
            let role = cache_msg.call_method1("get", ("role",))?;
            if !PyTuple::new(py, ["tool", "user"])?.contains(role)? {
                continue;
            }
            // Skip the ephemeral live user message
            if cache_msg.is(&messages.as_any().get_item(-1)?) {
                continue;
            }
            let content = cache_msg.call_method1("get", ("content",))?;
            if is_str(&content)? && content.is_truthy()? {
                let block = PyDict::new(py);
                block.set_item("type", "text")?;
                block.set_item("text", &content)?;
                block.set_item("cache_control", dict_of(py, &[("type", &txt(py, "ephemeral"))])?)?;
                cache_msg.set_item("content", PyList::new(py, [block])?)?;
            }
            break;
        }
    }

    let step = Step { messages: messages.into_any(), annotated_image_base64, image_sent };
    match ask(me, run, &step) {
        Ok(flow) => Ok(flow),
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            let error_msg = fstr(py, &[L("❌ Error processing request: "), V(&err_text(py, &e)?)])?;
            say(py, &error_msg)?;
            // Same pairing guarantee for a failure ANYWHERE in the step. The
            // closers are there only once a step got as far as defining
            // them (not when the first LLM call itself fails). A NameError
            // from inside them is let go, as Python's guard let it go.
            if run.closers {
                let closed = (|| close_pairing(run, &fstr(py, &[L("error: "), V(&err_text(py, &e)?)])?))();
                match closed {
                    Err(e) if e.is_instance_of::<PyNameError>(py) => {}
                    other => other?,
                }
            }
            // On critical error, record it and break the loop so the caller
            // can report the failure instead of a false "done".
            run.final_status = "error";
            run.final_message = err_text(py, &e)?;
            Ok(Flow::Stop)
        }
        Err(e) => Err(e),
    }
}

/// The outer try of a step: ask the model, sort its calls, record the turn
/// and run the actions. An Exception out of here ends the run with status
/// "error" (see step).
fn ask<'py>(me: &Obj<'py>, run: &mut Run<'py>, step: &Step<'py>) -> PyResult<Flow> {
    let py = me.py();
    let image_sent = boolean(py, step.image_sent);
    let step_number = int(py, run.step_number)?;

    // Make API request through LLM Manager
    if step.image_sent {
        say_text(py, "📸 Screenshot sent to LLM (annotated)")?;
    }

    // Check Stop BEFORE LLM
    if stop_requested(me)? {
        run.finish("incomplete", "Stopped by user");
        return Ok(Flow::Stop);
    }

    // Get raw response from LLM, with the annotated image
    let raw_response = me
        .getattr("llm_manager")?
        .call_method1("send_request", (&step.messages, &step.annotated_image_base64))?;

    // Memory bar: forward this call's exact token usage (input+output).
    if me.getattr("token_callback")?.is_truthy()? {
        let sent = (|| -> PyResult<()> {
            let token_callback = me.getattr("token_callback")?;
            token_callback.call1((me.getattr("llm_manager")?.getattr("last_usage")?,))?;
            Ok(())
        })();
        match sent {
            Err(e) if is_exception(py, &e) => {}
            other => other?,
        }
    }

    // CRITICAL Check Stop AFTER LLM (discards result if stopped while waiting)
    if stop_requested(me)? {
        say_text(py, "\n🛑 Agent stopped by user (response discarded).")?;
        run.finish("incomplete", "Stopped by user");
        return Ok(Flow::Stop);
    }

    // Runtime compression trigger: context size is fresh in last_usage.
    me.getattr("_compression")?.call_method1(
        "maybe_trigger",
        (&run.assistant_messages, &run.tool_responses, &run.original_task),
    )?;

    say_text(py, "✓ LLM response received")?;

    // Save raw response before any parsing (for debugging). Under native
    // tool calling this is a dict, not a string.
    let raw_text = if is_dict(&raw_response)? {
        dumps_pretty(py, || Ok(raw_response.clone()))?
    } else {
        raw_response.str()?.into_any()
    };
    save_raw_response(me, &raw_text, &step_number)?;

    // NATIVE TOOL CALLING: nothing to parse. Convert the model's own calls
    // into the same [{type, ...}] action dicts route_action has always
    // consumed, keeping their ids so each result pairs back to the call that
    // produced it in the next request.
    let tool_calls_to_steps = g(py, "tool_calls_to_steps")?;
    let tool_calls = raw_response.call_method1("get", ("tool_calls",))?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("allowed", me.getattr("_allowed_tools")?)?;
    kwargs.set_item("defaults_map", me.getattr("_action_defaults")?)?;
    kwargs.set_item("track_params", me.getattr("_track_params")?)?;
    let [mut actions, calls, rejects, track] = unpack(&tool_calls_to_steps.call((tool_calls,), Some(&kwargs))?)?;
    run.calls = calls;
    let resp_text = or_empty(raw_response.call_method1("get", ("text",))?)?.call_method0("strip")?;

    // The notes call (`reasoning` in quality mode, `memory` in fast) is the
    // step's notes, not an action: pull it out before routing (the
    // controller has no handler for it) but keep it in `all_calls`, which is
    // what the assistant turn replays, and what Gemini's thought signature
    // binds to. Each note call is answered with the bare REASONING_RESULT;
    // the snapshot and the compression dump hide it.
    let all_calls = run.calls.clone();
    let note_ids = PyList::empty(py);
    for pair in g(py, "zip")?.call1((&actions, &run.calls))?.try_iter()? {
        let [action, call] = unpack(&pair?)?;
        if action.call_method1("get", ("type",))?.eq(me.getattr("_notes_tool")?)? {
            note_ids.append(call.get_item("id")?)?;
        }
    }
    let note_results = PyList::empty(py);
    for id in note_ids.iter() {
        let entry = PyDict::new(py);
        entry.set_item("tool_call_id", id)?;
        entry.set_item("content", g(py, "REASONING_RESULT")?)?;
        note_results.append(entry)?;
    }
    run.note_results = note_results.into_any();
    let kept_actions = PyList::empty(py);
    let kept_calls = PyList::empty(py);
    for pair in g(py, "zip")?.call1((&actions, &run.calls))?.try_iter()? {
        let [action, call] = unpack(&pair?)?;
        if action.call_method1("get", ("type",))?.ne(me.getattr("_notes_tool")?)? {
            kept_actions.append(action)?;
            kept_calls.append(call)?;
        }
    }
    actions = kept_actions.into_any();
    run.calls = kept_calls.into_any();

    // Backstop for an `async_tools` model (Astra): a `done` bundled after
    // this turn's actions is dropped from the batch and answered with an
    // error result (see drop_bundled_terminal); the result rides with the
    // rejects below.
    let mut done_results = PyList::empty(py).into_any();
    if me.getattr("llm_manager")?.call_method0("async_tools")?.is_truthy()? {
        let drop_bundled_terminal = g(py, "drop_bundled_terminal")?;
        let dropped = drop_bundled_terminal.call1((&actions, &run.calls, &rejects, "done"))?;
        [actions, run.calls, done_results] = unpack(&dropped)?;
        if done_results.is_truthy()? {
            say_text(py, "`done` bundled with other actions: ignored, it must be the only call of its turn")?;
        }
    }

    let reply = Reply { raw_response, track, resp_text };

    if !actions.is_truthy()? && !rejects.is_truthy()? {
        if !note_ids.is_empty() {
            // A reasoning-only step: the model chose to think before acting.
            // Accepted as a normal step (it costs a fresh scan, so only a
            // RUN of them is bounded: from the third in a row the result
            // carries a reminder).
            run.reasoning_only_count += 1;
            // An `async_tools` model that never acts is the route having
            // dropped the async flags (one call per turn again): the
            // reminder cannot help it, so the run stops at six instead of
            // scanning to the step cap.
            if me.getattr("llm_manager")?.call_method0("async_tools")?.is_truthy()? && run.reasoning_only_count >= 6 {
                run.final_status = "incomplete";
                run.final_message = txt(
                    py,
                    &format!(
                        "{} steps in a row without an action on an async-tools model: the route dropped the async tool flags (see the raw responses), so the model can only emit one call per turn.",
                        run.reasoning_only_count
                    ),
                );
                return Ok(Flow::Stop);
            }
            if run.reasoning_only_count >= 3 {
                let reminded = PyList::empty(py);
                for id in note_ids.iter() {
                    let entry = PyDict::new(py);
                    entry.set_item("tool_call_id", id)?;
                    let content = dumps_pretty(py, || {
                        let reminder = PyDict::new(py);
                        reminder.set_item("status", "success")?;
                        reminder.set_item("action", me.getattr("_notes_tool")?)?;
                        reminder.set_item(
                            "message",
                            format!(
                                "{} steps in a row without an action: call a tool that does the work in this turn.",
                                run.reasoning_only_count
                            ),
                        )?;
                        Ok(reminder.into_any())
                    })?;
                    entry.set_item("content", content)?;
                    reminded.append(entry)?;
                }
                run.note_results = reminded.into_any();
            }
            let encode_step = g(py, "encode_step")?;
            let wire_calls = g(py, "wire_calls_from")?.call1((&all_calls,))?;
            let provider_meta = reply.raw_response.call_method1("get", ("provider_meta",))?;
            run.normalized = encode_step.call1((&reply.resp_text, wire_calls, provider_meta))?;
            // Only the fields this mode carries (fast and Claude have no thinking).
            let display_payload = display_payload(py, &reply, PyList::empty(py).as_any())?;
            save_conversation(me, &step.messages, &display_payload, &image_sent, &step_number)?;
            run.assistant_messages.call_method1("append", (&run.normalized,))?;
            let append = run.tool_responses.getattr("append")?;
            append.call1((g(py, "encode_results")?.call1((&run.note_results,))?,))?;
            let print = g(py, "print")?;
            print.call1((format_response(py, &display_payload, true)?,))?;
            if me.getattr("text_callback")?.is_truthy()? {
                let text_callback = me.getattr("text_callback")?;
                text_callback.call1((format_response(py, &display_payload, false)?,))?;
            }
            emit_flow(me, &txt(py, "received"), &dict_of(py, &[("tools", PyList::empty(py).as_any())])?)?;
            run.is_first_iteration = false;
            return Ok(Flow::Next);
        }
        // REPAIR, not exit: `done` is an explicit dedicated final call, so a
        // message with NO tool calls is always a dropped-calls mistake: the
        // model narrated its move instead of making it. Keep the turn so the
        // reasoning isn't lost, answer it with a corrective result turn, and
        // let the model continue. The strike counter still bounds a model
        // that keeps talking without acting.
        run.json_fail_count += 1;
        if reply.resp_text.is_truthy()? {
            say_text(
                py,
                &format!("message had no tool calls - asking the model to act ({}/3)", run.json_fail_count),
            )?;
            if run.json_fail_count >= 3 {
                run.finish("incomplete", "Model kept responding without calling any tool (3 consecutive turns)");
                return Ok(Flow::Stop);
            }
            let encode_step = g(py, "encode_step")?;
            let provider_meta = reply.raw_response.call_method1("get", ("provider_meta",))?;
            run.normalized = encode_step.call1((&reply.resp_text, PyList::empty(py), provider_meta))?;
            // Snapshot shows the prose the model wrote, not the codec JSON
            save_conversation(me, &step.messages, &reply.resp_text, &image_sent, &step_number)?;
            run.assistant_messages.call_method1("append", (&run.normalized,))?;
            // Feedback in the SAME envelope every real result uses: the
            // environment reporting "nothing ran", not an instruction
            // breaking the loop's rhythm.
            let append = run.tool_responses.getattr("append")?;
            let nothing = dumps_pretty(py, || dict_of(py, &[("message", &txt(py, "no tool called"))]))?;
            append.call1((txt(py, "<tool_response>\n").add(nothing)?.add("\n</tool_response>")?,))?;
            run.is_first_iteration = false;
            return Ok(Flow::Next);
        }
        // Genuinely empty turn (no text, no calls): retry the step.
        say_text(py, &format!("empty response from the model - retrying ({}/3)", run.json_fail_count))?;
        if run.json_fail_count >= 3 {
            run.finish("incomplete", "Could not get a valid model response (3 consecutive failures)");
            return Ok(Flow::Stop);
        }
        run.step_number -= 1;
        return Ok(Flow::Next);
    }

    // Reset the consecutive counters on a real, actionable turn
    run.json_fail_count = 0;
    run.reasoning_only_count = 0;

    // The assistant turn EXACTLY as the model produced it: prose on the text
    // channel, its tool calls on the tool channel. This is what gets
    // replayed next step. REJECTED calls are included too: the turn must
    // mirror what the model emitted, and every result written below (errors
    // included) has to pair with a call of the same id or the next request
    // is malformed.
    let encode_step = g(py, "encode_step")?;
    let wire_calls = g(py, "wire_calls_from")?.call1((all_calls.add(&rejects)?,))?;
    let provider_meta = reply.raw_response.call_method1("get", ("provider_meta",))?;
    run.normalized = encode_step.call1((&reply.resp_text, wire_calls, provider_meta))?;

    let display_payload = display_payload(py, &reply, &actions)?;

    // Save a faithful snapshot of the EXACT payload sent this step plus this
    // step's response: a true peek into agent memory. The response is
    // rendered as the readable block view, not the codec JSON (whose
    // call arguments are JSON escaped inside JSON).
    save_conversation(me, &step.messages, &display_payload, &image_sent, &step_number)?;

    // Record the turn with an empty result slot, backfilled after routing,
    // so a `done` turn also lands in the conversation.
    run.assistant_messages.call_method1("append", (&run.normalized,))?;
    run.tool_responses.call_method1("append", (py.None(),))?;

    // A call naming a tool that doesn't exist is answered with an error
    // result instead of being dropped, so the model sees what went wrong,
    // keyed to its own call id, and corrects next turn.
    let reject_results = PyList::empty(py);
    for reject in rejects.try_iter()? {
        let reject = reject?;
        let entry = PyDict::new(py);
        entry.set_item("tool_call_id", reject.get_item("id")?)?;
        entry.set_item("content", fstr(py, &[L("error: "), V(&reject.get_item("error")?)])?)?;
        reject_results.append(entry)?;
    }
    run.reject_results = reject_results.into_any();
    run.reject_results = iadd(&run.reject_results, &done_results)?;

    // The closers are defined from here on.
    run.closers = true;

    // Format the response for console output (terminal: include action block)
    let formatted_response = format_response(py, &display_payload, true)?;
    say(py, &formatted_response)?;

    // Send to frontend if callback exists (omit action from stream)
    if me.getattr("text_callback")?.is_truthy()? {
        let text_callback = me.getattr("text_callback")?;
        text_callback.call1((format_response(py, &display_payload, false)?,))?;
    }

    // Tool-flow chain: the packet arrived. Tick "packet received" and hand
    // the frontend this turn's tools, read straight from the action block.
    let tools = g(py, "AgentResponseFormatter")?.getattr("extract_tools")?.call1((&display_payload,))?;
    emit_flow(me, &txt(py, "received"), &dict_of(py, &[("tools", &tools)])?)?;

    if rejects.is_truthy()? {
        for reject in rejects.try_iter()? {
            let name = reject?.get_item("name")?;
            say(py, &fstr(py, &[L("unknown tool '"), V(&name), L("' - error returned to the model")])?)?;
        }
        if !actions.is_truthy()? {
            // Nothing valid to route: persist just the errors and let the
            // model correct itself on the next turn.
            let encode_results = g(py, "encode_results")?;
            let results = run.note_results.add(&run.reject_results)?;
            run.tool_responses.set_item(-1, encode_results.call1((results,))?)?;
            run.is_first_iteration = false;
            return Ok(Flow::Next);
        }
    }

    // The actions came straight from the model's native tool calls
    match act(me, run, &actions) {
        Ok(flow) => Ok(flow),
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L("⚠ Error processing action: "), V(&err_text(py, &e)?)])?)?;
            // The step died mid-flight: record the error against every call
            // so the saved chat never carries a dangling tool call.
            close_pairing(run, &fstr(py, &[L("error: "), V(&err_text(py, &e)?)])?)?;
            // Even on error, continue the loop.
            // last_response = json.dumps({"status": "error", "message": str(e)})
            let dumps = g(py, "json")?.getattr("dumps")?;
            dumps.call1((dict_of(py, &[("status", &txt(py, "error")), ("message", &err_text(py, &e)?)])?,))?;
            run.is_first_iteration = false;
            Ok(Flow::Next)
        }
        Err(e) => Err(e),
    }
}

/// The inner try of a step: run the actions and record what they returned.
/// An Exception out of here is recorded against the step's calls and the
/// loop goes on (see ask).
fn act<'py>(me: &Obj<'py>, run: &mut Run<'py>, actions: &Obj<'py>) -> PyResult<Flow> {
    let py = me.py();
    // agent_response = {"action": actions}; it always holds the key, so what
    // decides is whether there is an action to run.
    if actions.is_truthy()? {
        // Check Stop BEFORE Action: NOTHING ran this step, so discard the
        // turn instead of saving a result for a call that never executed.
        // The chat then ends on the last step that really happened.
        if stop_requested(me)? {
            discard_step(run)?;
            say_text(py, "\n🛑 Agent stopped by user (step discarded - no action ran).")?;
            run.finish("incomplete", "Stopped by user");
            return Ok(Flow::Stop);
        }

        // Execute the action
        say_text(py, "\n⚡ Executing action...")?;

        // The phone reader already handed the elements mapping to the
        // controller's service during scan_elements, so nothing is passed
        // here.

        // Send action to controller. A value the vault typed can come back
        // in a result: it reads as [vault]. Not in `done`: that is the
        // model's own answer to the user.
        let mut action_result = me.getattr("controller")?.call_method1("route_action", (actions,))?;
        if action_result.call_method1("get", ("action",))?.ne("done")? {
            action_result = g(py, "vault")?.call_method1("hide_json", (&action_result,))?;
        }

        // Check if action was stopped mid-execution. Here the actions DID
        // partially run, so record what the controller actually returned:
        // that partial effect is real and the next run must see it.
        if action_result.call_method1("get", ("status",))?.eq("stopped")? {
            say_text(py, "\n🛑 Agent stopped by user (action interrupted).")?;
            pair_results(run, &action_result)?;
            run.finish("incomplete", "Stopped by user");
            return Ok(Flow::Stop);
        }

        // Check if web tool was used: store for light digest iteration. A
        // failed search stays in its own result, so its error is kept in
        // memory verbatim instead of going to a digest.
        let web_results_list = PyList::empty(py);
        if action_result.call_method1("get", ("tool",))?.eq("web")?
            && action_result.contains("result")?
            && action_result.call_method1("get", ("status",))?.eq("success")?
        {
            web_results_list.append(action_result.get_item("result")?)?;
            action_result.del_item("result")?;
        } else if action_result.call_method1("get", ("action",))?.eq("multiple")? && action_result.contains("results")? {
            for (idx, result) in action_result.get_item("results")?.try_iter()?.enumerate() {
                let result = result?;
                if result.call_method1("get", ("tool",))?.eq("web")?
                    && result.contains("result")?
                    && result.call_method1("get", ("status",))?.eq("success")?
                {
                    web_results_list.append(result.get_item("result")?)?;
                    action_result.get_item("results")?.get_item(idx)?.del_item("result")?;
                }
            }
        }

        if !web_results_list.is_empty() {
            run.pending_web_response = txt(py, "<tool>\n")
                .add(txt(py, "\n").call_method1("join", (&web_results_list,))?)?
                .add("\n</tool>")?;
            say_text(py, "🌐 Web results captured for digest iteration")?;
        } else {
            run.pending_web_response = none(py);
        }

        // Check if task completed (done action was executed)
        if action_result.call_method1("get", ("action",))?.eq("done")? {
            let summary = action_result.call_method1("get", ("summary", "Task completed"))?;
            say(py, &fstr(py, &[L("\n🎉 Task Complete: "), V(&summary)])?)?;
            say_text(py, "✅ Agent has finished all tasks. Exiting loop.")?;
            // `done` ran, so record the real outcome (the summary) rather
            // than a filler: this is the final step the next resume reads.
            pair_results(run, &action_result)?;
            run.final_status = "success";
            run.final_message = action_result.call_method1("get", ("summary", "Task completed"))?;
            return Ok(Flow::Stop);
        }

        // last_response = json.dumps(action_result, indent=2): nothing reads
        // it, but a result that is not JSON fails here, before it is paired.
        let dumps = g(py, "json")?.getattr("dumps")?;
        dumps.call((&action_result,), Some(&keyword(py, "indent", 2)?))?;

        // Preserve this step's results as role:"tool" turns keyed to the
        // calls that produced them.
        pair_results(run, &action_result)?;

        // Web-result memory: on the digest step, fold the numbered
        // scratchpad findings the agent JUST wrote into the original web
        // step's tool_response, replacing the 'refer to scratchpad' pointer.
        // The scratchpad is wiped on the next user request, so this keeps
        // the distilled web info in durable conversation memory.
        if run.is_web_digest
            && !run.web_memory_index.is_none()
            && int(py, 0)?.le(&run.web_memory_index)?
            && run.web_memory_index.lt(run.tool_responses.len()?)?
            && run.tool_responses.get_item(&run.web_memory_index)?.is_truthy()?
        {
            let entries = PyList::empty(py);
            // for a in (agent_response.get("action") or [])
            let listed = if actions.is_truthy()? { actions.clone() } else { PyList::empty(py).into_any() };
            for action in listed.try_iter()? {
                let action = action?;
                if is_dict(&action)?
                    && action.call_method1("get", ("type",))?.eq("scratchpad")?
                    && action.call_method1("get", ("value",))?.is_truthy()?
                {
                    entries.append(action.call_method1("get", ("value", ""))?.str()?.call_method0("strip")?)?;
                }
            }
            if !entries.is_empty() {
                let lines = PyList::empty(py);
                for (i, entry) in entries.iter().enumerate() {
                    lines.append(in_generator(py, || fstr(py, &[L(&format!("{}. ", i + 1)), V(&entry)]))?)?;
                }
                let numbered = txt(py, "\n").call_method1("join", (lines,))?;
                // The web step's slot is a native result list. Fold the
                // findings into EVERY entry (the web call may sit at any
                // index: results are per-call now, and reject errors ride
                // ahead of them; backfill_web_findings changes nothing on
                // anything that isn't a web result) and re-encode onto the
                // SAME call ids, so the pairing that step wrote stays intact.
                let slot = run.tool_responses.get_item(&run.web_memory_index)?;
                let stored = match (|| g(py, "json")?.getattr("loads")?.call1((slot.str()?,)))() {
                    Ok(stored) => stored,
                    Err(e) if is_exception(py, &e) => none(py),
                    Err(e) => return Err(e),
                };
                let mut native = is_list(&stored)? && stored.is_truthy()?;
                if native {
                    for entry in stored.try_iter()? {
                        let entry = entry?;
                        if !in_generator(py, || Ok(is_dict(&entry)? && entry.contains("tool_call_id")?))? {
                            native = false;
                            break;
                        }
                    }
                }
                if native {
                    for entry in stored.try_iter()? {
                        let entry = entry?;
                        let content = or_empty(entry.call_method1("get", ("content",))?)?.str()?;
                        entry.set_item("content", backfill_web_findings(content.as_any(), &numbered)?)?;
                    }
                    let encoded = g(py, "encode_results")?.call1((&stored,))?;
                    run.tool_responses.set_item(&run.web_memory_index, encoded)?;
                } else {
                    // Legacy (schema-era) slot: plain JSON string.
                    let filled = backfill_web_findings(slot.str()?.as_any(), &numbered)?;
                    run.tool_responses.set_item(&run.web_memory_index, filled)?;
                }
            }
            run.web_memory_index = none(py);
        }

        // A web call this step points the pointer at THIS step, for the
        // next digest to fold into. Set AFTER the fold above, which still
        // needed the previous web step.
        if !web_results_list.is_empty() {
            run.web_memory_index = int(py, run.tool_responses.len()? as i64 - 1)?;
        }

        // Wait before next scan (default 1 second, unless wait action was used)
        let mut wait_time = float(py, 1.0);
        if action_result.call_method1("get", ("tool",))?.eq("wait")? {
            // If wait was explicitly called, use that duration
            wait_time = action_result.call_method1("get", ("duration", 1.0))?;
        } else if action_result.call_method1("get", ("action",))?.eq("multiple")? {
            // Check if wait was in multiple actions
            for result in action_result.call_method1("get", ("results", PyList::empty(py)))?.try_iter()? {
                let result = result?;
                if result.call_method1("get", ("tool",))?.eq("wait")? {
                    wait_time = result.call_method1("get", ("duration", 1.0))?;
                    break;
                }
            }
        }

        say(py, &fstr(py, &[L("⏳ Waiting "), V(&wait_time), L("s before next scan...")])?)?;
        let mut elapsed = 0.0_f64;
        while float(py, elapsed).lt(&wait_time)? {
            if stop_requested(me)? {
                break;
            }
            // time.sleep(min(0.5, wait_time - elapsed))
            let sleep = g(py, "time")?.getattr("sleep")?;
            let least = g(py, "min")?;
            sleep.call1((least.call1((0.5, wait_time.sub(float(py, elapsed))?))?,))?;
            elapsed += 0.5;
        }

        // Print action result
        if action_result.call_method1("get", ("status",))?.eq("success")? {
            say_text(py, "✓ Action executed successfully")?;

            if action_result.call_method1("get", ("action",))?.eq("todo_created")? {
                // This was a todo creation
                say_text(py, "📋 Todo list created")?;
            } else if action_result.call_method1("get", ("action",))?.eq("todo_updated")? {
                // This was a todo update
                say_text(py, "✓ Todo task marked complete")?;
            }
        } else {
            let message = action_result.call_method1("get", ("message", "Unknown error"))?;
            say(py, &fstr(py, &[L("⚠ Action result: "), V(&message)])?)?;
        }
    }

    // Mark first iteration as done
    run.is_first_iteration = false;
    Ok(Flow::Next)
}

// ---------------------------------------------------------------------------
// For Python: the class and the functions under their old names
// ---------------------------------------------------------------------------

/// The iPhone agent: it drives the phone for one user request at a time
/// (process_request).
///
/// The struct holds nothing. What the Python object kept on itself is in the
/// object's own __dict__, under the same names.
#[pyclass(frozen, dict, weakref, subclass)]
pub struct AgentService;

#[pymethods]
impl AgentService {
    /// The object, empty, as object.__new__ made it. __init__ fills it.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(provider, model, save_conversation=False, frontend_callback=None, text_callback=None, web_callback=None, shell_callback=None, cli_callback=None, tool_callback=None, token_callback=None, api_key=None, stop_event=None, prior_history=None, speed='quality', **_extra)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AgentService
    }

    /// Initialize the Agent Service. cli_callback is taken because the app
    /// always passes it: the iPhone agent has no CLI agent, so it is not
    /// used. **_extra takes the keywords meant for other agents:
    /// agent_launcher forwards every keyword to an extension type.
    #[pyo3(signature = (provider, model, save_conversation=Given::Missing, frontend_callback=None,
        text_callback=None, web_callback=None, shell_callback=None, cli_callback=None,
        tool_callback=None, token_callback=None, api_key=None, stop_event=None,
        prior_history=None, speed=Given::Missing, **_extra))]
    #[allow(clippy::too_many_arguments)]
    fn __init__<'py>(
        slf: &Bound<'py, Self>,
        provider: Obj<'py>,
        model: Obj<'py>,
        save_conversation: Given<'py>,
        frontend_callback: Option<Obj<'py>>,
        text_callback: Option<Obj<'py>>,
        web_callback: Option<Obj<'py>>,
        shell_callback: Option<Obj<'py>>,
        cli_callback: Option<Obj<'py>>,
        tool_callback: Option<Obj<'py>>,
        token_callback: Option<Obj<'py>>,
        api_key: Option<Obj<'py>>,
        stop_event: Option<Obj<'py>>,
        prior_history: Option<Obj<'py>>,
        speed: Given<'py>,
        _extra: Option<Bound<'py, PyDict>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let _ = cli_callback;
        let or_none = |value: Option<Obj<'py>>| value.unwrap_or_else(|| none(py));
        let args = Args {
            provider,
            model,
            save_conversation: save_conversation.or(|| boolean(py, false)),
            frontend_callback: or_none(frontend_callback),
            text_callback: or_none(text_callback),
            web_callback: or_none(web_callback),
            shell_callback: or_none(shell_callback),
            tool_callback: or_none(tool_callback),
            token_callback: or_none(token_callback),
            api_key: or_none(api_key),
            stop_event: or_none(stop_event),
            prior_history: or_none(prior_history),
            speed: speed.or(|| txt(py, "quality")),
        };
        init(slf.as_any(), &args)
    }

    /// Run one user request in the loop until it ends. Returns {"status",
    /// "message"}: "success" after a `done`, "error" when an exception ended
    /// the loop, "incomplete" otherwise.
    #[pyo3(name = "process_request")]
    fn process_request_py<'py>(slf: &Bound<'py, Self>, task: Obj<'py>) -> PyResult<Obj<'py>> {
        process_request(slf.as_any(), &task)
    }

    /// Load the system prompt matching the speed mode (quality/fast), and
    /// Claude's own prompt for quality mode on any Claude model.
    #[pyo3(name = "_load_system_prompt")]
    fn load_system_prompt_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        load_system_prompt(slf.as_any())
    }

    /// Initialize the conversation file with header information.
    #[pyo3(name = "_initialize_conversation_file")]
    fn initialize_conversation_file_py(slf: &Bound<'_, Self>) -> PyResult<()> {
        initialize_conversation_file(slf.as_any())
    }

    /// Save a numbered conversation file: the exact payload sent this step
    /// and this step's response.
    #[pyo3(name = "_save_conversation_snapshot")]
    fn save_conversation_snapshot_py<'py>(
        slf: &Bound<'py, Self>,
        messages: Obj<'py>,
        current_assistant_response: Obj<'py>,
        image_sent: Obj<'py>,
        interaction_count: Obj<'py>,
    ) -> PyResult<()> {
        save_conversation_snapshot(slf.as_any(), &messages, &current_assistant_response, &image_sent, &interaction_count)
    }

    /// Save raw LLM response before any parsing/normalization.
    #[pyo3(name = "_save_raw_response")]
    fn save_raw_response_py<'py>(slf: &Bound<'py, Self>, raw_response: Obj<'py>, step_number: Obj<'py>) -> PyResult<()> {
        save_raw_response(slf.as_any(), &raw_response, &step_number)
    }

    /// Save conversation snapshot to file, when the run saves its conversation.
    #[pyo3(name = "_save_conversation")]
    fn save_conversation_py<'py>(
        slf: &Bound<'py, Self>,
        messages: Obj<'py>,
        current_assistant_response: Obj<'py>,
        image_sent: Obj<'py>,
        interaction_count: Obj<'py>,
    ) -> PyResult<()> {
        save_conversation(slf.as_any(), &messages, &current_assistant_response, &image_sent, &interaction_count)
    }

    /// <persistent_memory>: the todo list and the scratchpad, then <vault>
    /// when there is one, rebuilt every step.
    #[staticmethod]
    #[pyo3(
        name = "_persistent_memory",
        signature = (todo_list, scratchpad_content, vault_block = Given::Missing),
        text_signature = "(todo_list, scratchpad_content, vault_block='')"
    )]
    fn persistent_memory_py<'py>(
        todo_list: Obj<'py>,
        scratchpad_content: Obj<'py>,
        vault_block: Given<'py>,
    ) -> PyResult<Obj<'py>> {
        let py = todo_list.py();
        persistent_memory(&todo_list, &scratchpad_content, &vault_block.or(|| txt(py, "")))
    }

    /// Read the current todo list from the task tracker's todo.md file.
    #[pyo3(name = "_read_todo_from_file")]
    fn read_todo_from_file_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        read_todo_from_file(slf.as_any())
    }

    /// Read the current scratchpad entries from scratchpad/milestone/
    /// milestone.md.
    #[pyo3(name = "_read_scratchpad_from_file")]
    fn read_scratchpad_from_file_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        read_scratchpad_from_file(slf.py())
    }

    /// Build the compact tool_response preserved in agent memory for a step.
    #[pyo3(name = "_tool_response_for_memory")]
    fn tool_response_for_memory_py<'py>(&self, action_result: Obj<'py>) -> PyResult<Obj<'py>> {
        tool_response_for_memory(&action_result)
    }

    /// Replace a web step's 'refer to scratchpad' placeholder with the
    /// findings the agent wrote into the scratchpad on the digest step.
    #[pyo3(name = "_backfill_web_findings")]
    fn backfill_web_findings_py<'py>(&self, tool_response_str: Obj<'py>, findings: Obj<'py>) -> PyResult<Obj<'py>> {
        backfill_web_findings(&tool_response_str, &findings)
    }

    /// Trim an OLDER schema-era history step for context: drop 'action' only.
    #[pyo3(name = "_trim_history_entry")]
    fn trim_history_entry_py<'py>(&self, entry: Obj<'py>) -> PyResult<Obj<'py>> {
        trim_history_entry(&entry)
    }

    /// Push a tool-flow event to the frontend bottom chain (best-effort).
    #[pyo3(name = "_emit_flow", signature = (event, payload=None))]
    fn emit_flow_py<'py>(slf: &Bound<'py, Self>, event: Obj<'py>, payload: Option<Obj<'py>>) -> PyResult<()> {
        let payload = payload.unwrap_or_else(|| none(slf.py()));
        emit_flow(slf.as_any(), &event, &payload)
    }
}

/// `<updated_user_request no="N">`, the task, and its closing tag.
#[pyfunction]
#[pyo3(name = "_request_marker")]
fn request_marker_py<'py>(n: Obj<'py>, task: Obj<'py>) -> PyResult<Obj<'py>> {
    request_marker(&n, &task)
}

/// Clear all contents inside the agent's scratchpad folder for a fresh start.
#[pyfunction]
#[pyo3(name = "_cleanup_scratchpad")]
fn cleanup_scratchpad_py(py: Python<'_>) -> PyResult<()> {
    cleanup_scratchpad(py)
}

/// Fills this module, the loop's part of the extension: the names
/// service.py had of its own, and the standard library names it imported.
/// The names it imported from the rest of AutoCua are bound later, by the
/// package's loader, so loading the extension imports none of them.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    for name in ["os", "json", "re", "time", "shutil"] {
        m.add(name, py.import(name)?)?;
    }
    m.add("Path", py.import("pathlib")?.getattr("Path")?)?;
    m.add("datetime", py.import("datetime")?.getattr("datetime")?)?;
    m.add("Optional", py.import("typing")?.getattr("Optional")?)?;
    m.add("_BRIDGE_SIGNATURE", BRIDGE_SIGNATURE)?;
    m.add("_REQUEST_MARKER_PREFIX", REQUEST_MARKER_PREFIX)?;
    m.add("ASYNC_TOOLS_ADDENDUM", ASYNC_TOOLS_ADDENDUM)?;
    m.add("CLAUDE_BATCH_REMINDER", CLAUDE_BATCH_REMINDER)?;

    let functions = [
        pyo3::wrap_pyfunction!(request_marker_py, m)?,
        pyo3::wrap_pyfunction!(cleanup_scratchpad_py, m)?,
    ];
    for function in functions {
        function.setattr("__module__", PY_MODULE)?;
        m.add_function(function)?;
    }
    m.add_class::<AgentService>()?;
    // The class as Python named it. A name the object does not have then
    // reads as it did: the class has no __getattr__ that would word it
    // again.
    as_python_class(&py.get_type::<AgentService>(), &PyString::new(py, PY_MODULE))?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
