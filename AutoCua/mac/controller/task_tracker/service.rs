//! The todo list, the computer-use agent's tasks on macOS, Windows and
//! Linux: numbered #1., #2., ... in <system>/scratchpad/todo/todo.md (the
//! mac, windows or linux folder of AutoCua). The controller's todo actions
//! (<system>/controller/view.py) write it and mark a task done, and the agent
//! shows it back to itself every step.
//!
//! ONE file for the three systems: their task_tracker/service.py were the
//! same code, only the folder differed. It is the Rust copy of that
//! service.py. task_tracker_for! below gives each system its own class and
//! module, which Python imports under the old dotted name,
//! AutoCua.<system>.controller.task_tracker.service (each system's
//! task_tracker/__init__.py points that name here), and gets the same class,
//! methods, attributes, return values and log records. Python's own rules
//! decide every result, so they are called, not imitated: os.path, open in
//! text mode, int(), the str methods (split, strip, startswith, replace,
//! join, `in`) and the f-strings.
//!
//! It is the iPhone's todo list (ios/controller/task_tracker/service.rs)
//! with these service.py files' own differences kept: no session folder for
//! parallel tasks (os.environ is not read), save_todo first asks
//! os.path.exists whether the file is new (the answer is not used, but the
//! call is made), and the module also holds json and datetime, which
//! service.py imported.
//!
//! The instance keeps its two attributes (todo_dir, todo_file) where a
//! Python instance keeps them, in its __dict__, and its methods reach them
//! and each other through the instance, so a caller or a subclass that
//! replaces one is obeyed. A name service.py read as a global (os, logger,
//! open, str, int, enumerate, ValueError, TypeError) is read from the
//! system's module each time it is used (the helpers in lib.rs's
//! ios::controller).

use pyo3::prelude::*;
use pyo3::types::{PyList, PyString, PyTuple, PyType};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{
    agent_dir, caught, fstring, log, logged, make_dir, name, open_utf8, os_path, text, unbound_local,
};

// ---------------------------------------------------------------------------
// The methods, the same for every system: `module` is the system's module
// (its globals are read from it), `agent` its folder (mac, windows, linux).
// ---------------------------------------------------------------------------

/// TaskTrackerService.__init__, after its arguments were checked.
fn init(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>, agent: &str) -> PyResult<()> {
    // Go up two levels from AutoCua/<system>/controller/task_tracker/ to reach
    // the system's folder (agent_dir finds it from service.py's __file__, as
    // before).
    slf.setattr("todo_dir", agent_dir(module, agent, "task_tracker", "todo", None)?)?;

    let join = os_path(module, "join")?;
    slf.setattr("todo_file", join.call1((slf.getattr("todo_dir")?, "todo.md"))?)?;

    // Create todo directory if it doesn't exist
    slf.call_method0("_ensure_todo_directory")?;
    Ok(())
}

/// _ensure_todo_directory: logged, then raised again.
fn ensure_directory(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    make_dir(module, slf, "todo_dir", "Error creating todo directory: ")
}

/// save_todo
fn save_todo(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>, todo_content: &Bound<'_, PyAny>) -> PyResult<bool> {
    let saved = (|| -> PyResult<()> {
        // Check if file is new
        let exists = os_path(module, "exists")?;
        // `not` asks the answer's truth, as Python does; the answer is
        // not used after that.
        let _is_new_file = !exists.call1((slf.getattr("todo_file")?,))?.is_truthy()?;

        // Auto-number the tasks
        let numbered_content = slf.call_method1("_add_task_numbers", (todo_content,))?;

        // Write to file (overwrite mode)
        let f = open_utf8(module, slf, "todo_file", "w")?;
        with(&f, |f| {
            f.getattr("write")?.call1((&numbered_content,))?;
            Ok(())
        })?;

        // Silent save - no terminal output
        Ok(())
    })();
    Ok(logged(module, "Error saving todo list: ", saved)?.is_ok())
}

/// _add_task_numbers
fn add_task_numbers<'py>(slf: &Bound<'py, PyAny>, todo_content: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = slf.py();
    let lines = todo_content.call_method1("split", ("\n",))?;
    let numbered_lines = PyList::empty(py);
    let mut task_number: u64 = 1;

    for line in lines.try_iter()? {
        let line = line?;
        // Check if line is a task (starts with - [ ] or - [x])
        let stripped = line.call_method0("strip")?;
        if stripped.call_method1("startswith", ("- [ ]",))?.is_truthy()?
            || stripped.call_method1("startswith", ("- [x]",))?.is_truthy()?
        {
            // Add number prefix
            let number = task_number.into_pyobject(py)?.into_any();
            numbered_lines.append(fstring(py, &[&text(py, "#"), &number, &text(py, ". "), &stripped])?)?;
            task_number += 1;
        } else {
            // Keep non-task lines as-is (like Objective:)
            numbered_lines.append(&line)?;
        }
    }

    PyString::new(py, "\n").call_method1("join", (numbered_lines,))
}

/// update_task
fn update_task(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>, task_number: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = slf.py();
    let updated = (|| -> PyResult<bool> {
        // Read current todo content
        let exists = os_path(module, "exists")?;
        if !exists.call1((slf.getattr("todo_file")?,))?.is_truthy()? {
            log(module, "warning", || Ok(text(py, "Todo file doesn't exist")))?;
            return Ok(false);
        }

        let f = open_utf8(module, slf, "todo_file", "r")?;
        // None when __exit__ swallowed an error: Python goes on after the
        // block with `content` never set, runs int() and its except first,
        // and fails only where it reads content, at content.split below.
        let content = with(&f, |f| f.call_method0("read"))?;

        // Convert task_number to int if it's a string
        let task_num = match name(module, "int").and_then(|int| int.call1((task_number,))) {
            Ok(task_num) => task_num,
            Err(e) => {
                // except (ValueError, TypeError): the two names are read
                // and checked while the error is being handled.
                let _handling = Handling::new(py, &e);
                let kinds = PyTuple::new(py, [name(module, "ValueError")?, name(module, "TypeError")?])?;
                if !caught(py, &e, kinds.as_any())? {
                    return Err(e);
                }
                log(module, "warning", || fstring(py, &[&text(py, "Invalid task number: "), task_number]))?;
                return Ok(false);
            }
        };

        // Find and update the line with matching #number
        let Some(content) = content else {
            return Err(unbound_local(py, "content"));
        };
        let lines = content.call_method1("split", ("\n",))?;
        let mut updated = false;

        for item in name(module, "enumerate")?.call1((&lines,))?.try_iter()? {
            let (i, line) = unpack2(&item?)?;
            // Check if line starts with the task number (e.g., "#1. - [ ]")
            let startswith = line.call_method0("strip")?.getattr("startswith")?;
            if startswith.call1((fstring(py, &[&text(py, "#"), &task_num, &text(py, ".")])?,))?.is_truthy()? {
                // Check if already marked complete
                if line.contains("- [x]")? {
                    log(module, "info", || {
                        fstring(py, &[&text(py, "Task #"), &task_num, &text(py, " already completed")])
                    })?;
                    return Ok(true);
                }

                // Mark as complete: replace [ ] with [x]
                let replaced = line.call_method1("replace", ("- [ ]", "- [x]", 1))?;
                lines.set_item(i, replaced)?;
                updated = true;
                log(module, "info", || {
                    fstring(py, &[&text(py, "Marked task #"), &task_num, &text(py, " as complete")])
                })?;
                break;
            }
        }

        if updated {
            // Write back to file
            let f = open_utf8(module, slf, "todo_file", "w")?;
            with(&f, |f| {
                let write = f.getattr("write")?;
                write.call1((PyString::new(py, "\n").call_method1("join", (&lines,))?,))?;
                Ok(())
            })?;
            Ok(true)
        } else {
            log(module, "warning", || {
                fstring(py, &[&text(py, "Task #"), &task_num, &text(py, " not found in todo list")])
            })?;
            Ok(false)
        }
    })();
    Ok(logged(module, "Error updating task: ", updated)?.unwrap_or(false))
}

/// Fills a system's module, the one that takes its service.py's place: the
/// names service.py had, its imports (os, json, logging, and datetime from
/// datetime, which it imported and never used) and its logger included, in
/// its order. `add_class` adds the system's class to it and gives it back.
///
/// `m` carries Python's name for the module (lib.rs makes it so): the
/// logger is named after it, logging.getLogger(__name__) as before, and the
/// class reports it as its __module__.
fn fill<'py>(m: &Bound<'py, PyModule>, add_class: impl FnOnce() -> PyResult<Bound<'py, PyType>>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add("os", py.import("os")?)?;
    m.add("json", py.import("json")?)?;
    let logging = py.import("logging")?;
    m.add("logging", &logging)?;
    // from datetime import datetime: the class, not the module.
    m.add("datetime", py.import("datetime")?.getattr("datetime")?)?;
    m.add("logger", logging.call_method1("getLogger", (&python_name,))?)?;
    let class = add_class()?;
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 9)?;
        class.setattr("__static_attributes__", ("todo_dir", "todo_file"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// One class and module per system
// ---------------------------------------------------------------------------

/// A system's TaskTrackerService (its own class, so its own __module__) and
/// its module's register(), for lib.rs's mac::register_system.
macro_rules! task_tracker_for {
    ($system:ident, $agent:literal) => {
        pub mod $system {
            use pyo3::prelude::*;
            use pyo3::sync::PyOnceLock;
            use pyo3::types::{PyDict, PyTuple};

            use crate::ios::controller::only_self;

            /// This system's module, as registered: its globals are read from
            /// it at run time.
            static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

            fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
                MODULE
                    .get_or_try_init(py, || {
                        py.import(concat!("AutoCua.agent_native.", $agent, ".controller.task_tracker.service"))
                            .map(Bound::unbind)
                    })
                    .map(|m| m.bind(py).clone())
            }

            // The agent's todo list. A plain comment: the Python class had no
            // docstring, and a doc comment here would give it one.
            #[pyclass(frozen, dict, weakref, subclass)]
            pub struct TaskTrackerService;

            #[pymethods]
            impl TaskTrackerService {
                /// A bare instance, as object.__new__ gave: __init__ fills it in.
                #[new]
                #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
                fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
                    TaskTrackerService
                }

                /// Initialize the Task Tracker Service
                #[pyo3(signature = (*args, **kwargs))]
                fn __init__(
                    slf: &Bound<'_, Self>,
                    args: &Bound<'_, PyTuple>,
                    kwargs: Option<&Bound<'_, PyDict>>,
                ) -> PyResult<()> {
                    only_self("TaskTrackerService.__init__()", args, kwargs)?;
                    super::init(slf.as_any(), &module(slf.py())?, $agent)
                }

                /// Create todo directory if it doesn't exist
                fn _ensure_todo_directory(slf: &Bound<'_, Self>) -> PyResult<()> {
                    super::ensure_directory(slf.as_any(), &module(slf.py())?)
                }

                /// Save todo list content to markdown file with auto-numbering
                fn save_todo(slf: &Bound<'_, Self>, todo_content: &Bound<'_, PyAny>) -> PyResult<bool> {
                    super::save_todo(slf.as_any(), &module(slf.py())?, todo_content)
                }

                /// Add #1., #2., etc. numbering to each task line
                fn _add_task_numbers<'py>(
                    slf: &Bound<'py, Self>,
                    todo_content: &Bound<'py, PyAny>,
                ) -> PyResult<Bound<'py, PyAny>> {
                    super::add_task_numbers(slf.as_any(), todo_content)
                }

                /// Update a task in the todo list by marking task #number as complete
                fn update_task(slf: &Bound<'_, Self>, task_number: &Bound<'_, PyAny>) -> PyResult<bool> {
                    super::update_task(slf.as_any(), &module(slf.py())?, task_number)
                }
            }

            /// Fills this system's module: the names its service.py had.
            pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
                super::fill(m, || {
                    m.add_class::<TaskTrackerService>()?;
                    Ok(m.py().get_type::<TaskTrackerService>())
                })?;
                let _ = MODULE.set(m.py(), m.clone().unbind());
                Ok(())
            }
        }
    };
}

task_tracker_for!(mac, "mac");
task_tracker_for!(windows, "windows");
task_tracker_for!(linux, "linux");
