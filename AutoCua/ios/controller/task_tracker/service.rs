//! The todo list, the iPhone agent's tasks: numbered #1., #2., ... in
//! ios/scratchpad/todo/todo.md (ios/scratchpad/<session>/todo/ for a
//! parallel simulator task, AutoCua_IOS_SESSION, so the lists stay apart).
//! The controller's todo actions write it and mark a task done, and the
//! agent shows it back to itself every step.
//!
//! The Rust copy of task_tracker/service.py. Python imports it under its old
//! dotted name, AutoCua.ios.controller.task_tracker.service
//! (task_tracker/__init__.py points that name here), and gets the same
//! class, methods, attributes, return values and log records. Python's own
//! rules decide every result, so they are called, not imitated: os.path,
//! open in text mode, int(), the str methods (split, strip, startswith,
//! replace, join, `in`) and the f-strings.
//!
//! The instance keeps its two attributes (todo_dir, todo_file) where a
//! Python instance keeps them, in its __dict__, and its methods reach them
//! and each other through the instance, so a caller or a subclass that
//! replaces one is obeyed. A name service.py read as a global (os, logger,
//! open, str, int, enumerate, ValueError, TypeError) is read from this
//! module each time it is used (the helpers in lib.rs's ios::controller).

use pyo3::exceptions::PyUnboundLocalError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{fstring, log, logged, make_dir, only_self, open_utf8, os_path, session_dir, text};
use crate::ios::NATIVE;
use crate::memory_compression::global;

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import(format!("{NATIVE}.controller.task_tracker.service")).map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// TaskTrackerService
// ---------------------------------------------------------------------------

// The agent's todo list. A plain comment: the Python class had no docstring,
// and a doc comment here would give it one.
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
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        only_self("TaskTrackerService.__init__()", args, kwargs)?;
        let module = module(slf.py())?;
        // Parallel simulator tasks each get their own scratchpad/<session>/
        // subtree (AutoCua_IOS_SESSION), so their todo lists stay separate.
        slf.setattr("todo_dir", session_dir(&module, "task_tracker", "todo")?)?;
        let join = os_path(&module, "join")?;
        slf.setattr("todo_file", join.call1((slf.getattr("todo_dir")?, "todo.md"))?)?;

        // Create todo directory if it doesn't exist
        slf.call_method0("_ensure_todo_directory")?;
        Ok(())
    }

    /// Create todo directory if it doesn't exist
    fn _ensure_todo_directory(slf: &Bound<'_, Self>) -> PyResult<()> {
        // Logged, then raised again.
        make_dir(&module(slf.py())?, slf.as_any(), "todo_dir", "Error creating todo directory: ")
    }

    /// Save todo list content to markdown file with auto-numbering
    fn save_todo(slf: &Bound<'_, Self>, todo_content: &Bound<'_, PyAny>) -> PyResult<bool> {
        let module = module(slf.py())?;
        let saved = (|| -> PyResult<()> {
            // Auto-number the tasks
            let numbered_content = slf.call_method1("_add_task_numbers", (todo_content,))?;

            // Write to file (overwrite mode)
            let f = open_utf8(&module, slf.as_any(), "todo_file", "w")?;
            with(&f, |f| {
                f.getattr("write")?.call1((&numbered_content,))?;
                Ok(())
            })?;

            // Silent save - no terminal output
            Ok(())
        })();
        Ok(logged(&module, "Error saving todo list: ", saved)?.is_ok())
    }

    /// Add #1., #2., etc. numbering to each task line
    fn _add_task_numbers<'py>(slf: &Bound<'py, Self>, todo_content: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
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

    /// Update a task in the todo list by marking task #number as complete
    fn update_task(slf: &Bound<'_, Self>, task_number: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let module = module(py)?;
        let updated = (|| -> PyResult<bool> {
            // Read current todo content
            let exists = os_path(&module, "exists")?;
            if !exists.call1((slf.getattr("todo_file")?,))?.is_truthy()? {
                log(&module, "warning", || Ok(text(py, "Todo file doesn't exist")))?;
                return Ok(false);
            }

            let f = open_utf8(&module, slf.as_any(), "todo_file", "r")?;
            // None when __exit__ swallowed an error: Python goes on after the
            // block with `content` never set.
            let Some(content) = with(&f, |f| f.call_method0("read"))? else {
                return Err(PyUnboundLocalError::new_err(
                    "cannot access local variable 'content' where it is not associated with a value",
                ));
            };

            // Convert task_number to int if it's a string
            let task_num = match global(&module, "int")?.call1((task_number,)) {
                Ok(task_num) => task_num,
                Err(e) => {
                    let caught = (global(&module, "ValueError")?, global(&module, "TypeError")?);
                    if !e.matches(py, caught)? {
                        return Err(e);
                    }
                    let _handling = Handling::new(py, &e);
                    log(&module, "warning", || fstring(py, &[&text(py, "Invalid task number: "), task_number]))?;
                    return Ok(false);
                }
            };

            // Find and update the line with matching #number
            let lines = content.call_method1("split", ("\n",))?;
            let mut updated = false;

            for item in global(&module, "enumerate")?.call1((&lines,))?.try_iter()? {
                let (i, line) = unpack2(&item?)?;
                // Check if line starts with the task number (e.g., "#1. - [ ]")
                let startswith = line.call_method0("strip")?.getattr("startswith")?;
                if startswith.call1((fstring(py, &[&text(py, "#"), &task_num, &text(py, ".")])?,))?.is_truthy()? {
                    // Check if already marked complete
                    if line.contains("- [x]")? {
                        log(&module, "info", || {
                            fstring(py, &[&text(py, "Task #"), &task_num, &text(py, " already completed")])
                        })?;
                        return Ok(true);
                    }

                    // Mark as complete: replace [ ] with [x]
                    let replaced = line.call_method1("replace", ("- [ ]", "- [x]", 1))?;
                    lines.set_item(i, replaced)?;
                    updated = true;
                    log(&module, "info", || {
                        fstring(py, &[&text(py, "Marked task #"), &task_num, &text(py, " as complete")])
                    })?;
                    break;
                }
            }

            if updated {
                // Write back to file
                let f = open_utf8(&module, slf.as_any(), "todo_file", "w")?;
                with(&f, |f| {
                    let write = f.getattr("write")?;
                    write.call1((PyString::new(py, "\n").call_method1("join", (&lines,))?,))?;
                    Ok(())
                })?;
                Ok(true)
            } else {
                log(&module, "warning", || {
                    fstring(py, &[&text(py, "Task #"), &task_num, &text(py, " not found in todo list")])
                })?;
                Ok(false)
            }
        })();
        Ok(logged(&module, "Error updating task: ", updated)?.unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place: the names service.py
/// had, its imports (os, logging) and its logger included.
///
/// `m` carries Python's name for the module (lib.rs makes it so): the
/// logger is named after it, logging.getLogger(__name__) as before, and the
/// class reports it as its __module__.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add("os", py.import("os")?)?;
    let logging = py.import("logging")?;
    m.add("logging", &logging)?;
    m.add("logger", logging.call_method1("getLogger", (&python_name,))?)?;
    m.add_class::<TaskTrackerService>()?;
    let class = py.get_type::<TaskTrackerService>();
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
