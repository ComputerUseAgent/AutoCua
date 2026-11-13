//! service: computer use's shell tool (macOS, Windows, Linux). One `shell`
//! call runs a command in the coder's sandbox (zsh on macOS, bash on Linux,
//! PowerShell on Windows) and shapes what came back into the reply the agent
//! reads. On Windows the sandbox is its own instance, working in the Desktop.
//!
//! The Rust copy of universal_tool/shell/service.py. Python imports it under
//! its old dotted name, AutoCua.universal_tool.shell.service (shell/__init__.py
//! points that name here), and finds the same names in the same order, with
//! the same class, methods, attribute, replies and log records. Python's own
//! rules decide every result, so they are called, not imitated: dict.get,
//! ==, /, str(), str's + and strip, truthiness and the f-strings.
//!
//! service.py did its work when it was imported; here that is two steps.
//! register() gives the module only _build. Then the loader binds what
//! service.py's import lines bound (logging, sys, Path, and Sandbox from
//! AutoCua.coder) and calls _build(), which runs the rest of service.py's
//! lines in their order: the logger, then the class. Loading the extension
//! imports none of them.
//!
//! Every global a line reads (sys, Sandbox, Path, str, logger, Exception) is
//! read from this module at that moment, as Python reads a global, so a
//! caller or a test that replaces one is obeyed. The instance keeps
//! `sandbox` in its __dict__, as a Python instance does, and its methods
//! reach it through the instance. An error raised in the except block
//! carries the error being handled as its __context__, as in Python.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyString, PyTuple};

use crate::computer::as_python_class;
use crate::ios::controller::{self, dict, fstring, only_self, text};

type Obj<'py> = Bound<'py, PyAny>;

/// The class's docstring, as Python 3.13 keeps it (indents taken out).
const CLASS_DOC: &str = "Lightweight shell for main agent - quick commands via sandboxed Desktop environment\n\
(zsh on macOS, bash on Linux, PowerShell on Windows)";

/// The line of service.py the class statement is on: the class's
/// __firstlineno__ from Python 3.13.
const CLASS_LINE: i32 = 10;

/// What the class's methods set through self, sorted: the class's
/// __static_attributes__ from Python 3.13.
const STATIC_ATTRIBUTES: [&str; 1] = ["sandbox"];

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{}.shell.service", crate::universal_tool::NATIVE)).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now, as Python reads a
/// global name (lib.rs's controller::name: a missing one is NameError,
/// naming the name).
fn g<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

/// `result.get(key)`.
fn get<'py>(result: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    result.call_method1("get", (key,))
}

// What the command printed: stdout, then stderr, each when there is any
// (output = "", then output += result["stdout"] when result.get("stdout"),
// and the same for stderr).
fn printed<'py>(result: &Obj<'py>) -> PyResult<Obj<'py>> {
    let mut output = text(result.py(), "");
    for stream in ["stdout", "stderr"] {
        if get(result, stream)?.is_truthy()? {
            output = output.add(result.get_item(stream)?)?;
        }
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// ShellService
// ---------------------------------------------------------------------------

#[pyclass(frozen, dict, weakref, subclass)]
pub struct ShellService;

#[pymethods]
impl ShellService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        ShellService
    }

    /// Initialize ShellService. macOS and Linux: the Sandbox pointing to the
    /// existing sandbox_workspace. Windows: its own Sandbox instance, CWD = Desktop.
    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        only_self("ShellService.__init__()", args, kwargs)?;
        let py = slf.py();
        if g(py, "sys")?.getattr("platform")?.eq("win32")? {
            // self.sandbox = Sandbox(workspace_name=".", session_id=None)
            let sandbox = g(py, "Sandbox")?;
            let keywords = PyDict::new(py);
            keywords.set_item("workspace_name", ".")?;
            keywords.set_item("session_id", py.None())?;
            slf.setattr("sandbox", sandbox.call((), Some(&keywords))?)?;
            // Override working_dir to Desktop directly (not a subfolder)
            // (str(Path.home() / "Desktop"): str is read first, as Python
            // reads the name it calls before the argument.)
            let str_ = g(py, "str")?;
            let home = g(py, "Path")?.getattr("home")?.call0()?;
            let working_dir = str_.call1((home.div("Desktop")?,))?;
            slf.getattr("sandbox")?.setattr("working_dir", working_dir)?;
        } else {
            slf.setattr("sandbox", g(py, "Sandbox")?.call0()?)?;
        }
        Ok(())
    }

    #[doc = "\nExecute a shell command (PowerShell on Windows) and return formatted result.\n\nArgs:\n    command: shell command to execute (a PowerShell command on Windows)\n    input_text: Optional stdin input for interactive commands\n    \nReturns:\n    dict with agent_location, shell, status, output\n"]
    #[pyo3(signature = (command, input_text = None))]
    fn run<'py>(slf: &Bound<'py, Self>, command: &Obj<'py>, input_text: Option<&Obj<'py>>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let input_text = input_text.cloned().unwrap_or_else(|| py.None().into_bound(py));
        let replied = (|| -> PyResult<Obj<'py>> {
            let result = slf.getattr("sandbox")?.getattr("run")?.call1((command, &input_text))?;

            let agent_location = slf.getattr("sandbox")?.call_method0("get_cwd")?;
            let shell = text(py, "shell");

            // Handle input_required
            if get(&result, "error")?.eq("input_required")? {
                let output = printed(&result)?;
                let last = result.call_method1("get", ("last_output", ""))?;
                let message = fstring(
                    py,
                    &[
                        &text(py, "Process waiting for input. Last output: '"),
                        &last,
                        &text(py, "'. Use input parameter."),
                    ],
                )?;
                let response = dict(
                    py,
                    &[
                        ("status", &text(py, "input_required")),
                        ("action", &shell),
                        ("agent_location", &agent_location),
                        ("shell", command),
                        ("message", &message),
                    ],
                )?;
                if output.call_method0("strip")?.is_truthy()? {
                    response.set_item("output", output.call_method0("strip")?)?;
                }
                return Ok(response.into_any());
            }

            // Handle timeout
            if get(&result, "timeout")?.is_truthy()? {
                let output = printed(&result)?;
                let message = result.call_method1("get", ("message", "Command timed out"))?;
                let response = dict(
                    py,
                    &[
                        ("status", &text(py, "timeout")),
                        ("action", &shell),
                        ("agent_location", &agent_location),
                        ("shell", command),
                        ("message", &message),
                    ],
                )?;
                if output.call_method0("strip")?.is_truthy()? {
                    response.set_item("output", output.call_method0("strip")?)?;
                }
                return Ok(response.into_any());
            }

            // Normal result
            let output = printed(&result)?;

            let returncode = get(&result, "returncode")?;
            let succeeded = get(&result, "success")?;

            let status = text(py, if succeeded.is_truthy()? { "success" } else { "failed" });
            let response = dict(
                py,
                &[("status", &status), ("action", &shell), ("agent_location", &agent_location), ("shell", command)],
            )?;

            // Always surface the exit code so the agent can tell a real error
            // from an empty result (e.g. grep exits 1 on no match).
            if !returncode.is_none() {
                response.set_item("exit_code", returncode)?;
            }

            // Show exactly what the command returned (verbatim stdout + stderr).
            // When the terminal returns absolutely nothing, show a clear marker
            // instead of a blank string: the status/exit_code already say whether
            // it succeeded, so this just makes "no output" unambiguous (like a
            // real terminal / Claude Code).
            let clean_output = output.call_method0("strip")?;
            let shown = if clean_output.is_truthy()? { clean_output } else { text(py, "(no output)") };
            response.set_item("output", shown)?;

            let error = result.call_method1("get", ("error", ""))?;
            if error.is_truthy()? {
                response.set_item("error", error)?;
            }

            Ok(response.into_any())
        })();
        match replied {
            Ok(response) => Ok(response),
            // except Exception as e:
            Err(e) => controller::except_exception(&module(py)?, e, |e| {
                controller::log(&module(py)?, "error", || {
                    let words = g(py, "str")?.call1((e.value(py),))?;
                    fstring(py, &[&text(py, "ShellService error: "), &words])
                })?;
                let agent_location = slf.getattr("sandbox")?.call_method0("get_cwd")?;
                let error = g(py, "str")?.call1((e.value(py),))?;
                let row = [
                    ("status", &text(py, "failed")),
                    ("action", &text(py, "shell")),
                    ("agent_location", &agent_location),
                    ("shell", command),
                    ("error", &error),
                ];
                Ok(dict(py, &row)?.into_any())
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// The module's lines
// ---------------------------------------------------------------------------

/// `key = value` at the top of service.py: the module's own dict takes it.
fn store<'py>(m: &Bound<'py, PyModule>, key: &str, value: impl IntoPyObject<'py>) -> PyResult<()> {
    m.dict().set_item(PyString::intern(m.py(), key), value)
}

/// Runs the lines of service.py after its import lines, in their order, and
/// binds what each one makes on the module. The loader calls it once, right
/// after it binds the imported names, and then deletes it.
#[pyfunction]
#[pyo3(pass_module, name = "_build")]
fn build(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    // logger = logging.getLogger(__name__)
    let get_logger = g(py, "logging")?.getattr("getLogger")?;
    store(m, "logger", get_logger.call1((g(py, "__name__")?,))?)?;

    // class ShellService:
    store(m, "ShellService", py.get_type::<ShellService>())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place with _build, which the
/// loader calls once it has bound the import lines' names (see the top of
/// this file), and makes the class look like the one service.py made.
/// service.py had no docstring, so the module's __doc__ stays None.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add_function(wrap_pyfunction!(build, m)?)?;
    let class = py.get_type::<ShellService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", CLASS_LINE)?;
        let names = PyTuple::new(py, STATIC_ATTRIBUTES.iter().map(|name| PyString::intern(py, name)))?;
        class.setattr("__static_attributes__", names)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
