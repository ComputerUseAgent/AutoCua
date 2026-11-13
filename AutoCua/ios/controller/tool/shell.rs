//! shell: the iPhone agent's shell on the host Mac (never the phone). One
//! `shell` call runs a plain command or AppleScript (an `osascript -e '...'`
//! command) in the coder's sandbox (AutoCua/coder/sandbox), the one sandbox
//! in AutoCua, and shapes what came back into the reply the agent reads.
//!
//! The Rust copy of tool/shell.py. Python imports it under its old dotted
//! name, AutoCua.ios.controller.tool.shell (tool/__init__.py points that
//! name here), and gets the same class, method, attribute, replies and log
//! records. Python's own rules decide every result, so they are called, not
//! imitated: dict.get, str's + and strip, truthiness and the f-strings.
//!
//! The names shell.py imported (logging, and Sandbox from AutoCua.coder) and
//! its logger are bound on this module by tool/__init__.py when the module
//! is first imported, as the .py did then, and read from it each time they
//! are used. The instance keeps `sandbox` in its __dict__, as a Python
//! instance does, and run reaches it through the instance.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyTuple};

use crate::computer::as_python_class;
use crate::ios::controller::{self, dict, except_exception, fstring, log, only_self, text};
use crate::ios::NATIVE;

type Obj<'py> = Bound<'py, PyAny>;

/// shell.py's docstring.
const MODULE_DOC: &str = "\nshell.py - Host-Mac shell tool for the iPhone agent.\n\n\
One `shell` call runs BOTH plain shell commands and AppleScript \u{2014} AppleScript is\n\
just an `osascript -e '...'` command, so there's no separate tool. Every\n\
command runs in the coder's sandbox (AutoCua/coder/sandbox), the one sandbox\n\
in AutoCua.\n";

/// The class's docstring, as Python 3.13 keeps it (indents taken out).
const CLASS_DOC: &str = "Host-Mac shell for the iPhone agent \u{2014} quick commands (plain or osascript\n\
AppleScript) via the sandboxed Desktop environment.";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.controller.tool.shell")).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now (lib.rs's
/// controller::name: a missing one is NameError, naming the name).
fn name<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
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

    /// Initialize ShellService with Sandbox pointing to existing sandbox_workspace
    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        only_self("ShellService.__init__()", args, kwargs)?;
        slf.setattr("sandbox", name(slf.py(), "Sandbox")?.call0()?)
    }

    #[doc = "\nExecute a shell command and return formatted result.\n\nArgs:\n    command: shell command to execute (plain shell, or `osascript -e '...'`\n        for AppleScript \u{2014} both run the same way)\n    input_text: Optional stdin input for interactive commands\n\nReturns:\n    dict with agent_location, shell, status, output\n"]
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
            Err(e) => except_exception(&module(py)?, e, |e| {
                log(&module(py)?, "error", || {
                    let words = name(py, "str")?.call1((e.value(py),))?;
                    fstring(py, &[&text(py, "ShellService error: "), &words])
                })?;
                let agent_location = slf.getattr("sandbox")?.call_method0("get_cwd")?;
                let error = name(py, "str")?.call1((e.value(py),))?;
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
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes shell.py's place with its docstring and the
/// class. tool/__init__.py binds the rest when the module is first imported
/// (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_class::<ShellService>()?;
    let class = py.get_type::<ShellService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    // What Python 3.13 gave the class statement: its line in shell.py and the
    // attributes its methods set through self.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 17)?;
        class.setattr("__static_attributes__", ("sandbox",))?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
