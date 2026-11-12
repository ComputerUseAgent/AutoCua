//! AgentService: the computer-use agent, one loop for every desktop. It is
//! the Rust copy of the Mac agent's computer_use/service.py.
//!
//! One request is one run of the loop: scan the screen, build the messages,
//! ask the model, run the tools it called, record the step, and again, until
//! a `done` call, a Stop, an error or the step limit. The loop is Rust. What
//! it drives is still Python and is called over the GIL: the tool registry
//! and its LLMManager, the controller, the screen scanner, the skills, the
//! transcript codec (view) and memory compression.
//!
//! How the port keeps the Python's behaviour:
//! - Every value stays a Python object from end to end, and what the Python
//!   did with it (truthiness, `x or default`, ==, in, len, str(), format(),
//!   +, .get, iteration, unpacking) is the real Python operation, called
//!   through PyO3 in the same order. So an odd value fails where and how it
//!   did, and a lone surrogate passes through unchanged.
//! - The names service.py imported are attributes of this module. The loader
//!   (computer/__init__.py) binds them when the package is imported, and
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
//! - An `except` block, a `finally` that an error passes through and the
//!   __exit__ of a `with` block whose body failed run with that error as
//!   the one being handled (crate::computer::Handling). So sys.exc_info() shows
//!   it to what they call, and an error raised there gets it as __context__.
//! - What a generator expression ran per item goes through in_generator: a
//!   StopIteration that leaves it is Python's RuntimeError (PEP 479).
//! - What differs between the hosts (the package name behind module paths
//!   and folders, the iPhone sub-agent, the training data, the Desktop
//!   sandbox) comes from crate::computer::platform(). No host is named here.
//!
//! Python imports this module as AutoCua.computer.service.

use std::ffi::CString;

use pyo3::exceptions::{
    PyAssertionError, PyAttributeError, PyException, PyFileNotFoundError, PyKeyError, PyNameError,
    PyRuntimeError, PyStopIteration, PyTypeError, PyValueError,
};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PySlice, PyString, PyTuple};

use crate::computer::{as_python_class, platform, Handling};
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

/// How long the first step waits for the phones' checks in Daisy chain mode
/// (_wait_for_phones): the app paired them already, so a check takes about a
/// second.
const PHONE_WAIT_SEC: i64 = 5;

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
        None => Err(PyRuntimeError::new_err("the computer-use service module is not registered")),
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
fn py_module_name() -> String {
    "AutoCua.computer.service".to_string()
}

/// The module's own file, as service.py's __file__ was: in this folder,
/// AutoCua/computer, beside the prompts.
fn module_file(py: Python<'_>) -> PyResult<String> {
    let file = crate::native_dir(py)?.join("computer").join("service.py");
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

/// The type name CPython puts in its own error texts: a class's own name, or
/// for a built-in type its dotted C name ("int", "datetime.date").
fn type_name(value: &Obj<'_>) -> PyResult<String> {
    let kind = value.get_type();
    let name: String = kind.getattr("__name__")?.extract()?;
    // SAFETY: a valid type object.
    let flags = unsafe { ffi::PyType_GetFlags(kind.as_type_ptr()) };
    if flags & ffi::Py_TPFLAGS_HEAPTYPE != 0 {
        return Ok(name);
    }
    let module: String = kind.getattr("__module__")?.extract()?;
    Ok(if module == "builtins" { name } else { format!("{module}.{name}") })
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

/// One line, at most `limit` characters: a slot status in <sub_agents>.
fn cut<'py>(text: &Obj<'py>, limit: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = text.py();
    // text = " ".join(str(text or "").split())
    let words = or_empty(text.clone())?.str()?.call_method0("split")?;
    let line = txt(py, " ").call_method1("join", (words,))?;
    // return text if len(text) <= limit else text[:limit] + "..."
    if line.len()?.into_pyobject(py)?.le(limit)? {
        Ok(line)
    } else {
        line.get_item(slice(&none(py), limit)?)?.add("...")
    }
}

/// Clear all contents inside AutoCua/<package>/scratchpad/ for a fresh
/// start, and the CLI subprocess output folders in the current directory.
fn cleanup_scratchpad(py: Python<'_>) -> PyResult<()> {
    // scratchpad_dir = Path(AutoCua/<package>) / "scratchpad": the system's
    // own folder, where its controller keeps the todo and the milestones.
    let host_dir = crate::native_dir(py)?.join(platform().package);
    let scratchpad_dir = g(py, "Path")?
        .call1((host_dir.to_string_lossy().into_owned(),))?
        .div("scratchpad")?;
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

    // Also clean CLI subprocess output folders at root
    for folder in ["cli_agent_result", "cli_conversation"] {
        let folder_path = g(py, "Path")?.call1((folder,))?;
        if folder_path.call_method0("exists")?.is_truthy()? && folder_path.call_method0("is_dir")?.is_truthy()? {
            g(py, "shutil")?.getattr("rmtree")?.call1((&folder_path,))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The agent
// ---------------------------------------------------------------------------

/// The constructor's arguments, each as the call gave it or its default.
struct Args<'py> {
    provider: Obj<'py>,
    model: Obj<'py>,
    save_conversation: Obj<'py>,
    frontend_callback: Obj<'py>,
    text_callback: Obj<'py>,
    web_callback: Obj<'py>,
    shell_callback: Obj<'py>,
    cli_callback: Obj<'py>,
    tool_callback: Obj<'py>,
    token_callback: Obj<'py>,
    api_key: Obj<'py>,
    stop_event: Obj<'py>,
    external_terminal: Obj<'py>,
    prior_history: Obj<'py>,
    speed: Obj<'py>,
    daisy_chain: Obj<'py>,
}

/// __init__, line by line, on the object `me`. A line that fails leaves
/// what the lines before it did (folders made and cleared, the manager, the
/// scanner and the controller built).
fn init<'py>(me: &Obj<'py>, args: &Args<'py>) -> PyResult<()> {
    let py = me.py();
    let host = platform();

    // Clean up scratchpad for a fresh start
    g(py, "_cleanup_scratchpad")?.call0()?;

    // Ensure sandbox workspace exists on Desktop
    if host.desktop_sandbox {
        let root = g(py, "Path")?.call_method0("home")?.div("Desktop")?.div("sandbox_workspace")?;
        me.setattr("sandbox_root", root)?;
        if !me.getattr("sandbox_root")?.call_method0("exists")?.is_truthy()? {
            let kwargs = PyDict::new(py);
            kwargs.set_item("parents", true)?;
            kwargs.set_item("exist_ok", true)?;
            me.getattr("sandbox_root")?.call_method("mkdir", (), Some(&kwargs))?;
        }
    }

    // Speed mode: "fast" swaps in fast_system_prompt.md and trims the output
    // schema to memory/next_goal/action.
    let fast = args.speed.str()?.call_method0("lower")?.eq("fast")?;
    me.setattr("speed", if fast { "fast" } else { "quality" })?;
    // Show the phone in its own window when ios_agent first works on it. The
    // app turns this off for the runs it starts (ui/service.py): its page
    // shows the phone in the iOS agent's box instead. Terminal and Telegram
    // runs keep it.
    if host.ios_agent {
        me.setattr("phone_window", true)?;
    }
    // Daisy chain (Computer use, Daisy chain in the app, "computer use, daisy
    // chain" in run_agent): the phone sub-agents are reachable. Off is This
    // PC: they are listed offline, refused if called, and no phone is looked
    // for.
    me.setattr("daisy_chain", args.daisy_chain.is_truthy()?)?;

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

    // The callbacks: the agent's responses to the frontend, the globe and the
    // terminal animations, the CLI agent's output, the tool-flow chain
    // (tool_callback(event, payload); events: run_start | turn{hasImage} |
    // received{tools:[{name,clicks?}]} | run_end) and the memory bar, called
    // after each LLM call with the provider's exact token usage.
    me.setattr("text_callback", &args.text_callback)?;
    me.setattr("web_callback", &args.web_callback)?;
    me.setattr("shell_callback", &args.shell_callback)?;
    me.setattr("cli_callback", &args.cli_callback)?;
    me.setattr("tool_callback", &args.tool_callback)?;
    me.setattr("token_callback", &args.token_callback)?;

    // Initialize Controller with provider and actual API model name (pass api_key for CLI agent subprocess)
    let controller_cls = g(py, "ControllerView")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("provider", &args.provider)?;
    kwargs.set_item("model", me.getattr("llm_manager")?.call_method0("get_model_name")?)?;
    kwargs.set_item("web_callback", &args.web_callback)?;
    kwargs.set_item("shell_callback", &args.shell_callback)?;
    kwargs.set_item("cli_callback", &args.cli_callback)?;
    kwargs.set_item("api_key", &args.api_key)?;
    kwargs.set_item("stop_event", &args.stop_event)?;
    kwargs.set_item("external_terminal", &args.external_terminal)?;
    me.setattr("controller", controller_cls.call((), Some(&kwargs))?)?;
    // A browser agent runs at the same speed
    let speed = me.getattr("speed")?;
    me.getattr("controller")?.setattr("speed", speed)?;
    // This PC: the phone sub-agents are refused
    let daisy_chain = me.getattr("daisy_chain")?;
    me.getattr("controller")?.setattr("daisy_chain", daisy_chain)?;

    // Initialize Domain Knowledge Service
    me.setattr("skills", g(py, "DomainKnowledgeService")?.call0()?)?;

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
    // Sub-agent ids keep counting through the whole chat, not per request
    if me.getattr("prior_history")?.is_truthy()? {
        let seeded = (|| -> PyResult<()> {
            let last = me.getattr("prior_history")?.call_method1("get", ("sub_agent_last_id",))?;
            let last = if last.is_truthy()? { last } else { int(py, 0)? };
            let last = py.get_type::<PyInt>().call1((last,))?;
            me.getattr("controller")?.setattr("sub_agent_last_id", last)
        })();
        match seeded {
            Err(e) if e.is_instance_of::<PyTypeError>(py) || e.is_instance_of::<PyValueError>(py) => {}
            other => other?,
        }
    }
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
        // current_dir = os.path.dirname(os.path.abspath(__file__)); the
        // prompts are in its folder of this system, computer/<package>/.
        let dirname = g(py, "os")?.getattr("path")?.getattr("dirname")?;
        let abspath = g(py, "os")?.getattr("path")?.getattr("abspath")?;
        let current_dir = dirname.call1((abspath.call1((module_file(py)?,))?,))?;
        let prompt_path = g(py, "os")?
            .getattr("path")?
            .getattr("join")?
            .call1((current_dir, platform().package, prompt_name))?;
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
                    // four-block way. Otherwise every replayed step shows
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
            write(f, || fstr(py, &[L("--- ASSISTANT (response - step "), V(interaction_count), L(") ---\n")]))?;
            write(f, || Ok(current_assistant_response.clone()))?;
            write(f, || Ok(txt(py, "\n")))
        })?;

        say(py, &fstr(py, &[L("Memory snapshot saved: conversation_"), V(interaction_count), L(".txt")])?)
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
            say(py, &fstr(py, &[L("Error saving raw response: "), V(&err_text(py, &e)?)])?)
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

/// The text of one of the controller's own files, stripped: "" when the file
/// is not there, and "" with a printed line when the read fails. The path is
/// the service's own (single source of truth, exactly where it writes), so
/// read can never drift from write.
fn read_own_file<'py>(me: &Obj<'py>, service: &str, path: &str, failed: &str) -> PyResult<Obj<'py>> {
    let py = me.py();
    let read = (|| -> PyResult<Obj<'py>> {
        let path_cls = g(py, "Path")?;
        let file = path_cls.call1((me.getattr("controller")?.getattr(service)?.getattr(path)?,))?;
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

/// Read the current todo list from the task tracker's todo.md file.
fn read_todo_from_file<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    read_own_file(me, "task_tracker", "todo_file", "Error reading todo file: ")
}

/// Read the current scratchpad entries from the scratchpad service's
/// milestone.md file.
fn read_scratchpad_from_file<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    read_own_file(me, "scratchpad_service", "scratchpad_file", "Error reading scratchpad file: ")
}

/// <persistent_memory>: the agent's OWN live state, rebuilt fresh and present
/// EVERY step. Read from disk each iteration, so the copy the model sees is
/// always current truth, and ONLY the live user message carries it, so no
/// stale copies pile up in the replayed history (exactly one exists per
/// request). Inside, in order:
///   - <todo_list>: the task tracker, or "none".
///   - <scratchpad>: the verified checkpoints so far, or "none".
///   - <sub_agents>: every sub-agent type's slots, as JSON
///     (sub_agent_slots). Always present.
///
/// Skills guidance is deliberately NOT in here: it is reference material
/// about the current screen, not something the agent wrote, so it rides
/// beside this block as <skills> (see skills_block).
fn persistent_memory<'py>(me: &Obj<'py>, todo_list: &Obj<'py>, scratchpad: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    // (value or '').strip() or 'none'
    let shown = |value: &Obj<'py>| -> PyResult<Obj<'py>> {
        let stripped = or_empty(value.clone())?.call_method0("strip")?;
        if stripped.is_truthy()? {
            Ok(stripped)
        } else {
            Ok(txt(py, "none"))
        }
    };
    let todo = fstr(py, &[L("<todo_list>\n"), V(&shown(todo_list)?), L("\n</todo_list>")])?;
    let notes = fstr(py, &[L("<scratchpad>\n"), V(&shown(scratchpad)?), L("\n</scratchpad>")])?;
    let pm = PyList::new(py, [todo, notes])?;
    pm.append(txt(py, "<sub_agents>\n").add(sub_agent_slots(me)?)?.add("\n</sub_agents>")?)?;
    txt(py, "<persistent_memory>\n")
        .add(txt(py, "\n\n").call_method1("join", (pm,))?)?
        .add("\n</persistent_memory>")
}

/// <sub_agents>: JSON, one line per sub-agent type, its slots as "id":
/// status. The ids run on across the types (the controller's slot_ids:
/// coder_agent 1-4, browser_agent 5-8, then the phones). A status is
/// "running: <task, cut to 100 characters>" while a run holds the slot (until
/// its report is handed over) and "empty" when it is free (a browser_agent
/// slot reads "offline: Chrome is missing from the system" while Google
/// Chrome is not installed). A phone's slot reads "connecting" while the
/// phone is being connected and "offline: <reason>" when it cannot be used,
/// This PC mode included, so the model does not tell the user to plug the
/// phone in. While the dataset recorder is on every slot reads "offline:
/// recording mode": the sub-agent tools are withheld
/// (data_extraction.OFF_TOOLS).
fn sub_agent_slots<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    // held = {(r["agent"], r["slot"]): r for r in self.controller.get_sub_agents() if not r["seen"]}
    let held = PyDict::new(py);
    for row in me.getattr("controller")?.call_method0("get_sub_agents")?.try_iter()? {
        let row = row?;
        if !row.get_item("seen")?.is_truthy()? {
            let key = PyTuple::new(py, [row.get_item("agent")?, row.get_item("slot")?])?;
            held.set_item(key, &row)?;
        }
    }
    // Never downloaded: without it no browser agent starts
    let chrome = !g(py, "find_chrome")?.call0()?.is_none();
    let lines = PyList::empty(py);
    for item in me.getattr("controller")?.call_method0("slot_ids")?.call_method0("items")?.try_iter()? {
        let [agent, ids] = unpack(&item?)?;
        let slots = PyDict::new(py);
        for n in ids.try_iter()? {
            let n = n?;
            let row = held.get_item(PyTuple::new(py, [&agent, &n])?)?;
            let status = if g(py, "data_extraction")?.getattr("ENABLED")?.is_truthy()? {
                txt(py, "offline: recording mode")
            } else if let Some(row) = row {
                let cut = g(py, "_cut")?;
                txt(py, "running: ").add(cut.call1((row.get_item("task")?,))?)?
            } else if platform().ios_agent && g(py, "PHONE_AGENTS")?.contains(&agent)? {
                phone_slot(me, &agent)?
            } else if !platform().ios_agent && agent.eq("android_agent")? {
                android_slot(me)?
            } else if agent.eq("browser_agent")? && !chrome {
                txt(py, "offline: Chrome is missing from the system")
            } else {
                txt(py, "empty")
            };
            slots.set_item(n.str()?, status)?;
        }
        let head = fstr(py, &[L("  \""), V(&agent), L("\": ")])?;
        let dumps = g(py, "json")?.getattr("dumps")?;
        lines.append(head.add(dumps.call((&slots,), Some(&keyword(py, "ensure_ascii", false)?))?)?)?;
    }
    txt(py, "{\n").add(txt(py, ",\n").call_method1("join", (lines,))?)?.add("\n}")
}

/// A phone slot no run holds: empty when the phone is ready, connecting
/// while it is being connected, offline with the reason otherwise.
fn phone_slot<'py>(me: &Obj<'py>, agent: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    if !me.getattr("daisy_chain")?.is_truthy()? {
        return Ok(txt(py, "offline: This PC mode, phones are only reachable in Daisy chain mode"));
    }
    let ctl = me.getattr("controller")?;
    let state = if agent.eq("ios_agent")? {
        ctl.call_method0("phone_state")?
    } else {
        ctl.call_method0("android_state")?
    };
    if state.eq("online")? {
        return Ok(txt(py, "empty"));
    }
    if ctl.call_method1("phone_connecting", (agent,))?.is_truthy()? {
        return Ok(txt(py, "connecting"));
    }
    let link = if agent.eq("ios_agent")? { ctl.getattr("phone")? } else { ctl.getattr("android")? };
    // reason = getattr(link, "reason", "") or (the phone's own default)
    let reason = match link.getattr("reason") {
        Ok(reason) => reason,
        Err(e) if e.is_instance_of::<PyAttributeError>(py) => txt(py, ""),
        Err(e) => return Err(e),
    };
    let reason = if reason.is_truthy()? {
        reason
    } else if agent.eq("ios_agent")? {
        txt(py, "no iPhone or iPad is connected")
    } else {
        txt(py, "no Android phone is connected")
    };
    let cut = g(py, "_cut")?;
    txt(py, "offline: ").add(cut.call1((reason,))?)
}

/// The Android phone's slot when no run holds it, on a system whose only
/// phone it is (no iPhone sub-agent): empty when the phone is ready,
/// connecting while it is being checked, offline with the reason otherwise.
/// That controller's own words: android_state() is "online", "checking" or
/// "offline", and it has no phone_connecting.
fn android_slot<'py>(me: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    if !me.getattr("daisy_chain")?.is_truthy()? {
        return Ok(txt(py, "offline: This PC mode, phones are only reachable in Daisy chain mode"));
    }
    let state = me.getattr("controller")?.call_method0("android_state")?;
    if state.eq("online")? {
        return Ok(txt(py, "empty"));
    }
    if state.eq("checking")? {
        return Ok(txt(py, "connecting"));
    }
    // reason = getattr(self.controller.android, "reason", "") or "no Android phone is connected"
    let link = me.getattr("controller")?.getattr("android")?;
    let reason = match link.getattr("reason") {
        Ok(reason) => reason,
        Err(e) if e.is_instance_of::<PyAttributeError>(py) => txt(py, ""),
        Err(e) => return Err(e),
    };
    let reason = if reason.is_truthy()? { reason } else { txt(py, "no Android phone is connected") };
    let cut = g(py, "_cut")?;
    txt(py, "offline: ").add(cut.call1((reason,))?)
}

/// <skills>: the skills .md the skills service matched for the current app
/// or domain. It is reference guidance for this screen, so it sits OUTSIDE
/// <persistent_memory>. Empty string when nothing matched, so the message
/// carries no empty tag. The matched .md already opens with its own
/// <additional_knowledge> or <domain_knowledge> tag, which is why this
/// wrapper is named <skills>: naming it the same would self-nest.
fn skills_block<'py>(domain_block: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = domain_block.py();
    let text = or_empty(domain_block.clone())?.call_method0("strip")?;
    if text.is_truthy()? {
        fstr(py, &[L("<skills>\n"), V(&text), L("\n</skills>")])
    } else {
        Ok(txt(py, ""))
    }
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
            pointer.set_item("message", "memory optimized - refer to scratchpad for the web result")?;
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

/// `from ...controller.tool import android_agent, ios_agent`, run by Python
/// itself, so the modules are found through sys.modules now and a missing
/// one fails with Python's own error. The iPhone's module is asked for only
/// on a host that has the iPhone sub-agent.
fn import_phone_tools<'py>(py: Python<'py>) -> PyResult<(Obj<'py>, Option<Obj<'py>>)> {
    let host = platform();
    let names = if host.ios_agent { "android_agent, ios_agent" } else { "android_agent" };
    let line = format!("from AutoCua.{}.controller.tool import {names}", host.package);
    let code = CString::new(line).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let scope = PyDict::new(py);
    py.run(&code, Some(&scope), None)?;
    let found = |name: &str| -> PyResult<Obj<'py>> {
        scope
            .get_item(name)?
            .ok_or_else(|| PyNameError::new_err(format!("name '{name}' is not defined")))
    };
    let android_agent = found("android_agent")?;
    let ios_agent = if host.ios_agent { Some(found("ios_agent")?) } else { None };
    Ok((android_agent, ios_agent))
}

/// Run one user request (see run_request), with the phones around it.
///
/// The moment the request starts, the paired iPhone or iPad is looked for
/// and connected in the background, so ios_agent can come online, and an
/// Android phone on USB is checked the same way for android_agent. However
/// the request ends (done, Stop, an error, Ctrl+C), the phones are let go.
///
/// While the dataset recorder is on, the sub-agents cannot be started at all
/// (data_extraction.OFF_TOOLS), so no phone is looked for either; nor in
/// This PC mode (daisy_chain off), where the phones are offline.
fn process_request<'py>(me: &Obj<'py>, task: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    let host = platform();
    if !g(py, "data_extraction")?.getattr("ENABLED")?.is_truthy()? && me.getattr("daisy_chain")?.is_truthy()? {
        let (android_agent, ios_agent) = import_phone_tools(py)?;
        // Each phone's sub-agent starts ready the moment its phone answers,
        // with the settings the sub_agent call then gives it
        let standby = PyDict::new(py);
        standby.set_item("provider", me.getattr("controller")?.getattr("provider")?)?;
        standby.set_item("model", me.getattr("controller")?.getattr("model")?)?;
        standby.set_item("api_key", me.getattr("controller")?.getattr("api_key")?)?;
        standby.set_item("speed", me.getattr("controller")?.getattr("speed")?)?;
        if let Some(ios_agent) = ios_agent {
            let start = ios_agent.getattr("PhoneLink")?.getattr("start")?;
            let link = start.call((), Some(&keyword(py, "standby", &standby)?))?;
            me.getattr("controller")?.setattr("phone", link)?;
            let window = me.getattr("phone_window")?;
            me.getattr("controller")?.getattr("phone")?.setattr("window", window)?;
        }
        let start = android_agent.getattr("AndroidLink")?.getattr("start")?;
        let link = start.call((), Some(&keyword(py, "standby", &standby)?))?;
        me.getattr("controller")?.setattr("android", link)?;
        // Daisy chain paired the phones already, so their checks take about
        // a second: the first step waits for them, so it sees them ready
        wait_for_phones(me, &int(py, PHONE_WAIT_SEC)?)?;
    }
    let outcome = run_request(me, task);
    // finally: the phones are let go, whichever way the request ended. An
    // error that ended it is the one being handled meanwhile, so an error
    // of a release gets it as its __context__ from Python itself.
    let _handling = outcome.as_ref().err().map(|e| Handling::new(py, e));
    if host.ios_agent {
        me.getattr("controller")?.call_method0("release_phone")?;
    }
    me.getattr("controller")?.call_method0("release_android")?;
    outcome
}

/// Hold the first step while a phone is still being checked, so its
/// <sub_agents> shows the phones as they are: Daisy chain paired them
/// already, so a check takes about a second. At most `limit` seconds (a
/// phone that is not there reads connecting, then offline); Stop ends it.
fn wait_for_phones<'py>(me: &Obj<'py>, limit: &Obj<'py>) -> PyResult<()> {
    let py = me.py();
    let deadline = g(py, "time")?.call_method0("monotonic")?.add(limit)?;
    while g(py, "time")?.call_method0("monotonic")?.lt(&deadline)? && !stop_requested(me)? {
        let connecting = if platform().ios_agent {
            // if not any(self.controller.phone_connecting(a) for a in PHONE_AGENTS): return
            let mut connecting = false;
            for agent in g(py, "PHONE_AGENTS")?.try_iter()? {
                let agent = agent?;
                let answer = in_generator(py, || me.getattr("controller")?.call_method1("phone_connecting", (&agent,)))?;
                if answer.is_truthy()? {
                    connecting = true;
                    break;
                }
            }
            connecting
        } else {
            // if self.controller.android_state() != "checking": return
            !me.getattr("controller")?.call_method0("android_state")?.ne("checking")?
        };
        if !connecting {
            return Ok(());
        }
        g(py, "time")?.call_method1("sleep", (0.1,))?;
    }
    Ok(())
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
    /// The sub-agent reports of this step, for its light message.
    cli_await_result: Obj<'py>,
    /// Sub-agent reports shown in the current step.
    step_reports: Obj<'py>,
    /// Reports of a step the model left unanswered, shown again.
    carried_reports: Obj<'py>,
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
    /// Training data: this run's episode, or None while recording is off.
    dataset: Obj<'py>,
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
    /// The `scan` tool's switch, read once per step.
    scan_on: Obj<'py>,
    messages: Obj<'py>,
    annotated_image_base64: Obj<'py>,
    image_sent: bool,
    element_tree_text: Obj<'py>,
    todo_list: Obj<'py>,
    scratchpad_content: Obj<'py>,
    domain_block: Obj<'py>,
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

/// The four-block view the terminal print, the frontend text stream and the
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

/// `dataset.add_step(...)`: one step of training data. The sixth and seventh
/// arguments differ between a notes-only step and one that acted, and are
/// read in their place in the argument list.
#[allow(clippy::too_many_arguments)]
fn record_step<'py>(
    me: &Obj<'py>,
    run: &Run<'py>,
    step: &Step<'py>,
    reply: &Reply<'py>,
    elements: impl FnOnce() -> PyResult<Obj<'py>>,
    clicks: impl FnOnce() -> PyResult<Obj<'py>>,
    actions: Obj<'py>,
    result: Obj<'py>,
) -> PyResult<()> {
    let py = me.py();
    let add_step = run.dataset.getattr("add_step")?;
    let screen = g(py, "get_screen")?.call0()?;
    let frame = if step.image_sent { me.getattr("scanner")?.getattr("_screenshot")? } else { none(py) };
    let app = me.getattr("scanner")?.getattr("application_name")?;
    let elements = elements()?;
    let clicks = clicks()?;
    let tool_calls = reply.raw_response.call_method1("get", ("tool_calls",))?;
    let context = dict_of(
        py,
        &[
            ("todo", &step.todo_list),
            ("scratchpad", &step.scratchpad_content),
            ("skills", &step.domain_block),
        ],
    )?;
    let args = PyTuple::new(
        py,
        [
            int(py, run.step_number)?,
            screen,
            frame,
            app,
            step.element_tree_text.clone(),
            elements,
            clicks,
            reply.track.clone(),
            reply.resp_text.clone(),
            tool_calls,
            actions,
            result,
        ],
    )?;
    add_step.call(args, Some(&keyword(py, "context", context)?))?;
    Ok(())
}

/// Process a user request in an iterative loop until completion.
///
/// Returns a structured outcome {"status", "message"} so callers (Telegram,
/// web UI) can tell a real completion from a failure instead of always
/// reporting "done":
///   - "success"    a `done` action ran
///   - "error"      a critical exception ended the loop (e.g. API failure)
///   - "incomplete" the loop ended otherwise (stopped, max steps, parse fails)
fn run_request<'py>(me: &Obj<'py>, task: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = me.py();
    let host = platform();
    let mut run = Run {
        step_number: 0,
        is_first_iteration: true,
        assistant_messages: PyList::empty(py).into_any(),
        tool_responses: PyList::empty(py).into_any(),
        cli_await_result: none(py),
        step_reports: PyList::empty(py).into_any(),
        carried_reports: PyList::empty(py).into_any(),
        pending_web_response: none(py),
        web_memory_index: none(py),
        is_web_digest: false,
        json_fail_count: 0,
        reasoning_only_count: 0,
        final_status: "incomplete",
        final_message: txt(py, "Agent stopped before completing the task"),
        is_resumed: false,
        original_task: task.clone(),
        dataset: none(py),
        closers: false,
        calls: none(py),
        note_results: none(py),
        reject_results: none(py),
        normalized: none(py),
    };

    // Runtime compression: new call invalidates any stale worker from a
    // prior process_request on this instance.
    me.getattr("_compression")?.call_method0("reset")?;

    // Every request starts with the screen scan on (the `scan` tool).
    me.getattr("controller")?.setattr("scan_on", true)?;

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
            say_text(py, &format!("Resumed memory: {} prior step(s) loaded", run.assistant_messages.len()?))?;
        }
    }
    // -----------------------------------------------------------------------

    // Print model info once at the start
    let model_name = me.getattr("llm_manager")?.call_method0("get_model_name")?;
    say(py, &fstr(py, &[L("\nProcessing with "), V(&model_name)])?)?;
    // Tool-flow chain: clear + take over the demo
    emit_flow(me, &txt(py, "run_start"), &none(py))?;

    // Training data: this run's episode, or None while recording is off.
    if host.training_data {
        let begin = g(py, "data_extraction")?.getattr("begin")?;
        let provider_name = me.getattr("llm_manager")?.call_method0("get_provider_name")?;
        let model_name = me.getattr("llm_manager")?.call_method0("get_model_name")?;
        let speed = me.getattr("speed")?;
        let screen = g(py, "get_screen")?.call0()?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("original_task", &run.original_task)?;
        kwargs.set_item("resumed", run.is_resumed)?;
        run.dataset = begin.call((task, host.package, provider_name, model_name, speed, screen), Some(&kwargs))?;
    }

    // Main agent loop
    loop {
        match step(me, task, &mut run)? {
            Flow::Next => continue,
            Flow::Stop => break,
        }
    }

    // Cleanup: Stop CLI agent subprocess if running
    me.getattr("controller")?.call_method0("stop_cli_agent")?;

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

    if run.dataset.is_truthy()? {
        run.dataset.call_method1("finish", (run.final_status, &run.final_message))?;
    }

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
        say_text(py, "\nAgent stopped by user.")?;
        me.getattr("controller")?.getattr("controller_service")?.call_method0("release_all_inputs")?;
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
        say_text(py, "\nMax step limit reached (100). Exiting agent loop.")?;
        run.finish("incomplete", "Reached the 100-step limit without finishing");
        return Ok(Flow::Stop);
    }

    // A finished sub-agent is reported in a step of its own, with no scan:
    // after an agent_wait, and as soon as one finishes while the agent was
    // not holding for it. Taken in one go here, so none is shown twice.
    run.step_reports = PyList::empty(py).into_any();
    if !run.pending_web_response.is_truthy()? {
        let taken = me.getattr("controller")?.call_method0("take_sub_agent_reports")?;
        let reports = run.carried_reports.add(taken)?;
        run.carried_reports = PyList::empty(py).into_any();
        run.cli_await_result = if reports.is_truthy()? { dict_of(py, &[("completed", &reports)])? } else { none(py) };
    }

    // The `scan` tool's switch, read once per step: off means no scan, no
    // screenshot or tree, and no screen tools in this step's request.
    let scan_on = me.getattr("controller")?.getattr("scan_on")?;

    // Tool-flow chain: announce the turn. Digest iterations send no image,
    // so the chain skips screenshot+mapping (true to the agent's behaviour).
    // scan_on and not (cli_await_result or pending_web_response)
    let has_image = if scan_on.is_truthy()? {
        boolean(py, !(run.cli_await_result.is_truthy()? || run.pending_web_response.is_truthy()?))
    } else {
        scan_on.clone()
    };
    emit_flow(me, &txt(py, "turn"), &dict_of(py, &[("hasImage", &has_image)])?)?;

    // No screen scanned this step means no skills. Set once here, so it
    // never carries over from an earlier screen.
    let mut domain_block = txt(py, "");

    let element_tree_text;
    let mut annotated_image_base64;
    let mut image_sent;
    let mut formatted_element_tree;
    let uac_detected;
    // Skip scan for light digest iterations (CLI await or web tool response)
    // and while the scan is off
    if run.cli_await_result.is_truthy()? || run.pending_web_response.is_truthy()? || !scan_on.is_truthy()? {
        say_text(py, &format!("\n{RULE}"))?;
        if run.cli_await_result.is_truthy()? {
            say_text(py, &format!("Step {}: CLI digest iteration (no scan)", run.step_number))?;
        } else if run.pending_web_response.is_truthy()? {
            say_text(py, &format!("Step {}: Web digest iteration (no scan)", run.step_number))?;
        } else {
            say_text(py, &format!("Step {}: Scan off (no scan)", run.step_number))?;
        }
        element_tree_text = txt(py, "");
        annotated_image_base64 = none(py);
        image_sent = false;
        formatted_element_tree = txt(py, "");
        uac_detected = boolean(py, false);
    } else {
        // Scan UI elements and get annotated screenshot
        say_text(py, &format!("\n{RULE}"))?;
        say_text(py, &format!("Step {}: Scanning snapshot.", run.step_number))?;
        me.getattr("scanner")?.call_method0("scan_elements")?;

        // Check Stop AFTER Scan
        if stop_requested(me)? {
            me.getattr("controller")?.getattr("controller_service")?.call_method0("release_all_inputs")?;
            run.finish("incomplete", "Stopped by user");
            return Ok(Flow::Stop);
        }

        // Already encoded at its final size/format by the scanner
        // (LLM_IMAGE_* in tree/element.py): re-compressing here would only
        // add a decode/encode round trip and destroy the labels.
        [element_tree_text, annotated_image_base64, uac_detected] =
            unpack(&me.getattr("scanner")?.call_method0("get_scan_data")?)?;

        image_sent = !annotated_image_base64.is_none();
        if image_sent {
            say_text(py, &format!("Image captured - annotated: {} chars", annotated_image_base64.len()?))?;
        } else {
            say_text(py, "NO IMAGE - annotated image is None!")?;
        }

        // Wrap element tree in proper tags
        formatted_element_tree = fstr(py, &[L("<element_tree>\n"), V(&element_tree_text), L("\n</element_tree>")])?;

        // Skills are matched against the screen we JUST scanned, so they are
        // looked up here and nowhere else. A digest iteration never reaches
        // this branch, so it can no longer match skills against the previous
        // screen's app name and an empty element tree. Brave belongs to the
        // browser agent: no skill may coach the main agent to work in its
        // window.
        if !uac_detected.is_truthy()? && {
            let name = or_empty(me.getattr("scanner")?.getattr("application_name")?)?;
            !name.call_method0("lower")?.contains("brave")?
        } {
            let get_knowledge = me.getattr("skills")?.getattr("get_knowledge")?;
            let application_name = me.getattr("scanner")?.getattr("application_name")?;
            domain_block = get_knowledge.call1((application_name, &element_tree_text))?;
        }
    }

    // Handle UAC secure desktop detection: the scan's third value. A scanner
    // on a host without UAC always answers False (tree/element.py), so this
    // branch never runs there.
    let mut user_message = none(py);
    if uac_detected.is_truthy()? {
        say_text(py, "UAC detected - asking agent to accept or decline")?;
        image_sent = false;
        formatted_element_tree = txt(py, "");
        let uac = platform().uac_prompt;
        let notes_hint = if me.getattr("_notes_tool")?.eq("memory")? {
            "`memory` with `next_goal`"
        } else if me.getattr("_claude")?.is_truthy()? {
            "`agent_memory` with `memory` and `next_goal`"
        } else {
            "`reasoning` with `memory` and `next_goal` only (no `thinking`)"
        };
        user_message = txt(
            py,
            &format!(
                "<UAC_Trigger>\n{uac} is blocking the screen. Based on your previous actions, do you want to allow this?\nCall the `hotkey` tool with value \"alt+y\" to accept, or \"alt+n\" to decline. No screenshot or element tree is provided this step - {notes_hint}, then that one hotkey call, nothing else.\n</UAC_Trigger>"
            ),
        );
    }

    // -- Live context, rebuilt fresh EVERY step -----------------------------
    // Two sibling blocks: <persistent_memory> (the agent's own ToDo and
    // scratchpad, read from disk here so every branch below shares ONE
    // current copy) and <skills> (guidance matched for this screen). Joined
    // here so an absent skills block leaves no blank gap.
    let todo_list = read_todo_from_file(me)?;
    let scratchpad_content = read_scratchpad_from_file(me)?;
    let blocks = PyList::empty(py);
    for block in [persistent_memory(me, &todo_list, &scratchpad_content)?, skills_block(&domain_block)?] {
        if block.is_truthy()? {
            blocks.append(block)?;
        }
    }
    let state_block = txt(py, "\n\n").call_method1("join", (blocks,))?;

    // Construct user message based on iteration
    if uac_detected.is_truthy()? {
        // One-shot prompt, written above: it carries no state or skills
    } else if run.is_first_iteration {
        // First iteration: user_request + persistent memory + element tree.
        // ToDo creation and timing are governed by <todo_capability> in the
        // system prompt (flexible: iteration 1 by default, may create
        // later), so no hard-coded todo rules are injected here.
        user_message = fstr(
            py,
            &[
                L("<user_request>\n"),
                V(task),
                L("\n</user_request>\n\n"),
                V(&state_block),
                L("\n\n"),
                V(&formatted_element_tree),
            ],
        )?;
    } else {
        // Subsequent iterations: request + persistent memory. On a RESUMED
        // session, mark the request as <updated_user_request> so the agent
        // knows this is a continuation of the same session (its prior steps
        // are already in the replayed history above). No <last_response>
        // block: every step's result rides the transcript as its own
        // tool-result turn, paired to the call that produced it.
        let request_tag = if run.is_resumed { "updated_user_request" } else { "user_request" };
        user_message = fstr(py, &[L("<"), L(request_tag), L(">\n"), V(task), L("\n</"), L(request_tag), L(">")])?;

        if run.cli_await_result.is_truthy()? {
            let cli_completed = run.cli_await_result.call_method1("get", ("completed", PyList::empty(py)))?;
            run.step_reports = cli_completed.clone();
            // import json as _json: Python's own json, not this module's
            let cli_json = py.import("json")?.getattr("dumps")?.call((&cli_completed,), Some(&pretty(py)?))?;
            let still_running = PyList::empty(py);
            for row in me.getattr("controller")?.call_method0("get_sub_agents")?.try_iter()? {
                let row = row?;
                if row.get_item("status")?.eq("pending")? {
                    still_running.append(row)?;
                }
            }
            let mut running_note = txt(py, "");
            if !still_running.is_empty() {
                let names = PyList::empty(py);
                for row in still_running.iter() {
                    let name = in_generator(py, || {
                        let agent = format_value(&row.get_item("agent")?, "")?;
                        let slot = format_value(&row.get_item("slot")?, "")?;
                        fstr(py, &[V(agent.as_any()), L(" id "), V(slot.as_any())])
                    })?;
                    names.append(name)?;
                }
                running_note = txt(py, "\n 4. Still running: ")
                    .add(txt(py, ", ").call_method1("join", (names,))?)?
                    .add(". If your next move needs one of their reports and nothing independent is left, call agent_wait for it in this step.")?;
            }
            user_message = fstr(
                py,
                &[
                    L("<sub_agent_report>\n"),
                    V(&cli_json),
                    L("\n</sub_agent_report>\n\n<critical> No image or element tree this step: a sub-agent finished.\n 1. Judge the report against the brief you gave it (the count, every field, the proof you asked for): a report is a claim, not proof.\n 2. Persist what came back now: bulk data into a file with shell, key facts and paths into the scratchpad.\n 3. Then pick your plan back up. Calls that need no [id] may run in this step; clicks and keys wait for the next screenshot."),
                    V(&running_note),
                    L(" </critical>\n\n<user_request>\n"),
                    V(task),
                    L("\n</user_request>\n\n"),
                    V(&state_block),
                ],
            )?;

            image_sent = false;
            annotated_image_base64 = none(py);

            // Clear flag after consumption
            run.cli_await_result = none(py);
        } else if run.pending_web_response.is_truthy()? {
            user_message = fstr(
                py,
                &[
                    L("<critical>\nNo image and element tree provided. Focus on digesting the web response below.\n1. Analyze thoroughly - extract all relevant data (numbers, dates, names, URLs, prices, etc.)\n2. Save ALL important findings to scratchpad in this step's action.\n</critical>\n\n"),
                    V(&run.pending_web_response),
                    L("\n\n<user_request>\n"),
                    V(task),
                    L("\n</user_request>\n\n"),
                    V(&state_block),
                ],
            )?;

            image_sent = false;
            annotated_image_base64 = none(py);

            // Clear flag after consumption; mark this as the web-digest step
            // so its scratchpad response can be folded into the web memory
            // below.
            run.pending_web_response = none(py);
            run.is_web_digest = true;
        } else {
            // Normal iteration: include full context
            user_message = user_message.add(fstr(
                py,
                &[L("\n\n"), V(&state_block), L("\n\n"), V(&formatted_element_tree)],
            )?)?;

            // Add image tag if image is provided
            if image_sent {
                user_message = user_message.add("\n\n<image>Annotated screenshot with bounding boxes</image>")?;
                user_message = user_message.add("\n\n<critical>Pure vision first: decide which element to interact with from the screenshot alone, then refer to <element_tree> for its [id].</critical>")?;
            }
        }
    }

    // Scan off: said on every step it lasts, report and digest steps
    // included. Live message only, so nothing of it is persisted.
    if !scan_on.is_truthy()? {
        user_message = user_message.add("\n\n<critical>Scan is off: no screenshot or element tree, and the screen tools are hidden. Judge this step on its tool results. Call scan on to see the screen and get them back.</critical>")?;
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

    // Live user message (screen + persistent memory + skills + element
    // tree). Rebuilt every step and never persisted; the screenshot attaches
    // to it. The system prompt is added exactly once (above): the resumable
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

    let step = Step {
        scan_on,
        messages: messages.into_any(),
        annotated_image_base64,
        image_sent,
        element_tree_text,
        todo_list,
        scratchpad_content,
        domain_block,
    };
    match ask(me, run, &step) {
        Ok(flow) => Ok(flow),
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            let error_msg = fstr(py, &[L("Error processing request: "), V(&err_text(py, &e)?)])?;
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
        say_text(py, "Screenshot sent to LLM (annotated)")?;
    }

    // Check Stop BEFORE LLM
    if stop_requested(me)? {
        run.finish("incomplete", "Stopped by user");
        return Ok(Flow::Stop);
    }

    // Get raw response from LLM, with the annotated image. The screen tools
    // ride only while the scan is on.
    me.getattr("llm_manager")?.call_method1("show_screen_tools", (&step.scan_on,))?;
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
        say_text(py, "\nAgent stopped by user (response discarded).")?;
        me.getattr("controller")?.getattr("controller_service")?.call_method0("release_all_inputs")?;
        run.finish("incomplete", "Stopped by user");
        return Ok(Flow::Stop);
    }

    // Runtime compression trigger: context size is fresh in last_usage.
    me.getattr("_compression")?.call_method1(
        "maybe_trigger",
        (&run.assistant_messages, &run.tool_responses, &run.original_task),
    )?;

    say_text(py, "LLM response received")?;

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
    let allowed = if step.scan_on.is_truthy()? {
        me.getattr("_allowed_tools")?
    } else {
        let difference = me.getattr("_allowed_tools")?.getattr("difference")?;
        difference.call1((g(py, "SCREEN_TOOLS")?,))?
    };
    kwargs.set_item("allowed", allowed)?;
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
            // Training data: a thinking-only step often carries the full
            // plan the next steps only follow.
            if run.dataset.is_truthy()? {
                record_step(
                    me,
                    run,
                    step,
                    &reply,
                    || me.getattr("scanner")?.call_method0("get_elements_mapping"),
                    || Ok(PyList::empty(py).into_any()),
                    PyList::empty(py).into_any(),
                    none(py),
                )?;
            }
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
        // The retry is a report step again
        run.carried_reports = run.step_reports.clone();
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
    // rendered as the readable four-block view, not the codec JSON (whose
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
    match act(me, run, step, &reply, &actions) {
        Ok(flow) => Ok(flow),
        Err(e) if is_exception(py, &e) => {
            let _handling = Handling::new(py, &e);
            say(py, &fstr(py, &[L("Error processing action: "), V(&err_text(py, &e)?)])?)?;
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
fn act<'py>(
    me: &Obj<'py>,
    run: &mut Run<'py>,
    step: &Step<'py>,
    reply: &Reply<'py>,
    actions: &Obj<'py>,
) -> PyResult<Flow> {
    let py = me.py();
    // agent_response = {"action": actions}; it always holds the key, so what
    // decides is whether there is an action to run.
    if actions.is_truthy()? {
        // Check Stop BEFORE Action: NOTHING ran this step, so discard the
        // turn instead of saving a result for a call that never executed.
        // The chat then ends on the last step that really happened.
        if stop_requested(me)? {
            discard_step(run)?;
            say_text(py, "\nAgent stopped by user (step discarded - no action ran).")?;
            run.finish("incomplete", "Stopped by user");
            return Ok(Flow::Stop);
        }

        // Execute the action
        say_text(py, "\nExecuting action...")?;

        // Pass elements mapping to controller
        let elements_mapping = me.getattr("scanner")?.call_method0("get_elements_mapping")?;
        let set_elements = me.getattr("controller")?.getattr("set_elements")?;
        set_elements.call1((&elements_mapping, me.getattr("scanner")?.getattr("application_name")?))?;

        // Send action to controller
        let action_result = me.getattr("controller")?.call_method1("route_action", (actions,))?;

        // Training data: the step, with where every click landed.
        if run.dataset.is_truthy()? {
            record_step(
                me,
                run,
                step,
                reply,
                || Ok(elements_mapping.clone()),
                || me.getattr("controller")?.getattr("controller_service")?.getattr("clicks"),
                actions.clone(),
                action_result.clone(),
            )?;
        }

        // Check if action was stopped mid-execution. Here the actions DID
        // partially run, so record what the controller actually returned:
        // that partial effect is real and the next run must see it.
        if action_result.call_method1("get", ("status",))?.eq("stopped")? {
            say_text(py, "\nAgent stopped by user (action interrupted).")?;
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
            say_text(py, "Web results captured for digest iteration")?;
        } else {
            run.pending_web_response = none(py);
        }

        // Check if task completed (done action was executed)
        if action_result.call_method1("get", ("action",))?.eq("done")? {
            let summary = action_result.call_method1("get", ("summary", "Task completed"))?;
            say(py, &fstr(py, &[L("\nTask Complete: "), V(&summary)])?)?;
            say_text(py, "Agent has finished all tasks. Exiting loop.")?;
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
        // Scan off: no scan follows, so nothing to settle for (an explicit
        // wait already paused in the controller).
        if !me.getattr("controller")?.getattr("scan_on")?.is_truthy()? {
            wait_time = float(py, 0.0);
        }

        if wait_time.is_truthy()? {
            say(py, &fstr(py, &[L("Waiting "), V(&wait_time), L("s before next scan...")])?)?;
        }
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
            say_text(py, "Action executed successfully")?;

            if action_result.call_method1("get", ("action",))?.eq("todo_created")? {
                // This was a todo creation
                say_text(py, "Todo list created")?;
            } else if action_result.call_method1("get", ("action",))?.eq("todo_updated")? {
                // This was a todo update
                say_text(py, "Todo task marked complete")?;
            }
        } else {
            let message = action_result.call_method1("get", ("message", "Unknown error"))?;
            say(py, &fstr(py, &[L("Action result: "), V(&message)])?)?;
        }
    }

    // Mark first iteration as done
    run.is_first_iteration = false;
    Ok(Flow::Next)
}

// ---------------------------------------------------------------------------
// For Python: the class and the functions under their old names
// ---------------------------------------------------------------------------

/// The computer-use agent of this host: it drives the desktop for one user
/// request at a time (process_request).
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
        text_signature = "(provider, model, save_conversation=False, frontend_callback=None, text_callback=None, web_callback=None, shell_callback=None, cli_callback=None, tool_callback=None, token_callback=None, api_key=None, stop_event=None, external_terminal=False, prior_history=None, speed='quality', daisy_chain=False, **_extra)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AgentService
    }

    /// Initialize the Agent Service. **_extra takes the keywords meant for
    /// other agents: agent_launcher forwards every keyword to an extension
    /// type.
    #[pyo3(signature = (provider, model, save_conversation=Given::Missing, frontend_callback=None,
        text_callback=None, web_callback=None, shell_callback=None, cli_callback=None,
        tool_callback=None, token_callback=None, api_key=None, stop_event=None,
        external_terminal=Given::Missing, prior_history=None, speed=Given::Missing,
        daisy_chain=Given::Missing, **_extra))]
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
        external_terminal: Given<'py>,
        prior_history: Option<Obj<'py>>,
        speed: Given<'py>,
        daisy_chain: Given<'py>,
        _extra: Option<Bound<'py, PyDict>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let or_none = |value: Option<Obj<'py>>| value.unwrap_or_else(|| none(py));
        let args = Args {
            provider,
            model,
            save_conversation: save_conversation.or(|| boolean(py, false)),
            frontend_callback: or_none(frontend_callback),
            text_callback: or_none(text_callback),
            web_callback: or_none(web_callback),
            shell_callback: or_none(shell_callback),
            cli_callback: or_none(cli_callback),
            tool_callback: or_none(tool_callback),
            token_callback: or_none(token_callback),
            api_key: or_none(api_key),
            stop_event: or_none(stop_event),
            external_terminal: external_terminal.or(|| boolean(py, false)),
            prior_history: or_none(prior_history),
            speed: speed.or(|| txt(py, "quality")),
            daisy_chain: daisy_chain.or(|| boolean(py, false)),
        };
        init(slf.as_any(), &args)
    }

    /// Run one user request, with the phones around it. Returns {"status",
    /// "message"}: "success" after a `done`, "error" when an exception ended
    /// the loop, "incomplete" otherwise.
    #[pyo3(name = "process_request")]
    fn process_request_py<'py>(slf: &Bound<'py, Self>, task: Obj<'py>) -> PyResult<Obj<'py>> {
        process_request(slf.as_any(), &task)
    }

    /// The loop itself, without the phones around it.
    #[pyo3(name = "_process_request")]
    fn run_request_py<'py>(slf: &Bound<'py, Self>, task: Obj<'py>) -> PyResult<Obj<'py>> {
        run_request(slf.as_any(), &task)
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

    /// Read the current todo list from the task tracker's todo.md file.
    #[pyo3(name = "_read_todo_from_file")]
    fn read_todo_from_file_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        read_todo_from_file(slf.as_any())
    }

    /// Read the current scratchpad entries from the scratchpad service's
    /// milestone.md file.
    #[pyo3(name = "_read_scratchpad_from_file")]
    fn read_scratchpad_from_file_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        read_scratchpad_from_file(slf.as_any())
    }

    /// <persistent_memory>: the todo list, the scratchpad and the sub-agent
    /// slots, rebuilt every step.
    #[pyo3(name = "_persistent_memory")]
    fn persistent_memory_py<'py>(slf: &Bound<'py, Self>, todo_list: Obj<'py>, scratchpad: Obj<'py>) -> PyResult<Obj<'py>> {
        persistent_memory(slf.as_any(), &todo_list, &scratchpad)
    }

    /// <sub_agents>: JSON, one line per sub-agent type, its slots as "id":
    /// status.
    #[pyo3(name = "_sub_agent_slots")]
    fn sub_agent_slots_py<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        sub_agent_slots(slf.as_any())
    }

    /// A phone slot no run holds: empty when the phone is ready, connecting
    /// while it is being connected, offline with the reason otherwise.
    /// It takes the agent (any value, None included) on a system with the
    /// iPhone sub-agent, and nothing on one without (Android is the only
    /// phone), as each system's agent had it.
    #[pyo3(name = "_phone_slot", signature = (*args, **kwargs))]
    fn phone_slot_py<'py>(
        slf: &Bound<'py, Self>,
        args: &Bound<'py, PyTuple>,
        kwargs: Option<&Bound<'py, PyDict>>,
    ) -> PyResult<Obj<'py>> {
        let mut given: Vec<Obj<'py>> = args.iter().collect();
        if let Some(kwargs) = kwargs {
            for (key, value) in kwargs.iter() {
                if platform().ios_agent && key.eq("agent")? {
                    given.push(value);
                } else {
                    let message = format!("AgentService._phone_slot() got an unexpected keyword argument {}", key.repr()?);
                    return Err(PyTypeError::new_err(message));
                }
            }
        }
        if platform().ios_agent {
            match given.as_slice() {
                [agent] => phone_slot(slf.as_any(), agent),
                [] => Err(PyTypeError::new_err("AgentService._phone_slot() missing 1 required positional argument: 'agent'")),
                _ => Err(PyTypeError::new_err(format!(
                    "AgentService._phone_slot() takes 2 positional arguments but {} were given",
                    given.len() + 1
                ))),
            }
        } else if given.is_empty() {
            android_slot(slf.as_any())
        } else {
            Err(PyTypeError::new_err(format!(
                "AgentService._phone_slot() takes 1 positional argument but {} were given",
                given.len() + 1
            )))
        }
    }

    /// Highest sub-agent id handed out so far; saved with the chat so ids
    /// keep counting.
    #[getter]
    fn sub_agent_last_id<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        slf.as_any().getattr("controller")?.getattr("sub_agent_last_id")
    }

    /// <skills>: the skills .md matched for the current app or domain, or ""
    /// when nothing matched.
    #[staticmethod]
    #[pyo3(name = "_skills_block")]
    fn skills_block_py<'py>(domain_block: Obj<'py>) -> PyResult<Obj<'py>> {
        skills_block(&domain_block)
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

    /// Hold the first step while a phone is still being checked, at most
    /// `limit` seconds; Stop ends it.
    #[pyo3(name = "_wait_for_phones", signature = (limit=Given::Missing), text_signature = "($self, limit=5)")]
    fn wait_for_phones_py<'py>(slf: &Bound<'py, Self>, limit: Given<'py>) -> PyResult<()> {
        let py = slf.py();
        let default = int(py, PHONE_WAIT_SEC)?;
        wait_for_phones(slf.as_any(), &limit.or(|| default))
    }
}

/// `<updated_user_request no="N">`, the task, and its closing tag.
#[pyfunction]
#[pyo3(name = "_request_marker")]
fn request_marker_py<'py>(n: Obj<'py>, task: Obj<'py>) -> PyResult<Obj<'py>> {
    request_marker(&n, &task)
}

/// One line, at most `limit` characters: a slot status in <sub_agents>.
#[pyfunction]
#[pyo3(name = "_cut", signature = (text, limit=Given::Missing), text_signature = "(text, limit=100)")]
fn cut_py<'py>(py: Python<'py>, text: Obj<'py>, limit: Given<'py>) -> PyResult<Obj<'py>> {
    let default = int(py, 100)?;
    cut(&text, &limit.or(|| default))
}

/// Clear all contents inside the host's scratchpad folder for a fresh start.
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
    m.add("PHONE_WAIT_SEC", PHONE_WAIT_SEC)?;
    m.add("ASYNC_TOOLS_ADDENDUM", ASYNC_TOOLS_ADDENDUM)?;
    m.add("CLAUDE_BATCH_REMINDER", CLAUDE_BATCH_REMINDER)?;

    let name = py_module_name();
    let functions = [
        pyo3::wrap_pyfunction!(request_marker_py, m)?,
        pyo3::wrap_pyfunction!(cut_py, m)?,
        pyo3::wrap_pyfunction!(cleanup_scratchpad_py, m)?,
    ];
    for function in functions {
        function.setattr("__module__", &name)?;
        m.add_function(function)?;
    }
    m.add_class::<AgentService>()?;
    // The class as Python named it. A name the object does not have, and an
    // error of sub_agent_last_id's own, then read as they did: the class
    // has no __getattr__ that would word them again.
    as_python_class(&py.get_type::<AgentService>(), &PyString::new(py, &name))?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
