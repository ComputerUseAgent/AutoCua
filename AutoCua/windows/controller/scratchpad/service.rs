//! The scratchpad, the Windows agent's notes: numbered lines in
//! windows/scratchpad/milestone/milestone.md. The controller's scratchpad action
//! (windows/controller/view.py) adds a line. The folder is
//! milestone/milestone.md, not scratchpad/scratchpad.md, to keep the notes
//! already on disk and to keep clear of the parent folder's name.
//!
//! The Rust copy of scratchpad/service.py. Python imports it under its old
//! dotted name, AutoCua.windows.controller.scratchpad.service
//! (scratchpad/__init__.py points that name here), and gets the same class,
//! methods, attributes, return values and log records. Python's own rules
//! decide every result, so they are called, not imitated: os.path, open in
//! text mode (its line endings and UTF-8), str.strip and the f-string.
//!
//! It is the iPhone's scratchpad (ios/controller/scratchpad/service.rs)
//! without two things Windows' service.py never had: a session folder for
//! parallel tasks (os.environ is not read), and read_scratchpad.
//!
//! The instance keeps its two attributes (scratchpad_dir, scratchpad_file)
//! where a Python instance keeps them, in its __dict__, and its methods read
//! them through the instance, so a caller that points them elsewhere is
//! obeyed. A name service.py read as a global (os, logger, open, str) is read
//! from this module each time it is used (the helpers in lib.rs's
//! ios::controller).

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyString, PyTuple};

use crate::computer::as_python_class;
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{agent_dir, logged, make_dir, name, only_self, open_utf8, os_path};
use crate::windows::NATIVE;
use crate::memory_compression::{format_value, join_str};

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import(format!("{NATIVE}.controller.scratchpad.service")).map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

// ---------------------------------------------------------------------------
// ScratchpadService
// ---------------------------------------------------------------------------

// The agent's notes. A plain comment: the Python class had no docstring, and
// a doc comment here would give it one.
#[pyclass(frozen, dict, weakref, subclass)]
pub struct ScratchpadService;

#[pymethods]
impl ScratchpadService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        ScratchpadService
    }

    /// Initialize the Scratchpad Service
    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        only_self("ScratchpadService.__init__()", args, kwargs)?;
        let module = module(slf.py())?;
        // Walk up two levels (controller/scratchpad -> windows) to locate the package root
        // (agent_dir finds windows_dir from service.py's __file__, as before).
        // On-disk storage paths kept as "milestone/milestone.md" to preserve existing data
        // and avoid the scratchpad/scratchpad/scratchpad.md collision with the parent dir.
        slf.setattr("scratchpad_dir", agent_dir(&module, "windows", "scratchpad", "milestone", None)?)?;

        let join = os_path(&module, "join")?;
        slf.setattr("scratchpad_file", join.call1((slf.getattr("scratchpad_dir")?, "milestone.md"))?)?;

        // Create scratchpad directory if it doesn't exist
        slf.call_method0("_ensure_scratchpad_directory")?;
        Ok(())
    }

    /// Create scratchpad directory if it doesn't exist
    fn _ensure_scratchpad_directory(slf: &Bound<'_, Self>) -> PyResult<()> {
        // Logged, then raised again.
        make_dir(&module(slf.py())?, slf.as_any(), "scratchpad_dir", "Error creating scratchpad directory: ")
    }

    /// Append a scratchpad entry with sequential numbering
    fn append_scratchpad(slf: &Bound<'_, Self>, scratchpad_content: &Bound<'_, PyAny>) -> PyResult<bool> {
        let py = slf.py();
        let module = module(py)?;
        let appended = (|| -> PyResult<()> {
            let mut existing_count: u64 = 0;
            let exists = os_path(&module, "exists")?;
            if exists.call1((slf.getattr("scratchpad_file")?,))?.is_truthy()? {
                let f = open_utf8(&module, slf.as_any(), "scratchpad_file", "r")?;
                with(&f, |f| {
                    for line in f.try_iter()? {
                        if line?.call_method0("strip")?.is_truthy()? {
                            existing_count += 1;
                        }
                    }
                    Ok(())
                })?;
            }

            let next_number = (existing_count + 1).into_pyobject(py)?.into_any();

            let f = open_utf8(&module, slf.as_any(), "scratchpad_file", "a")?;
            with(&f, |f| {
                let write = f.getattr("write")?;
                let entry = join_str(
                    py,
                    &[
                        format_value(&next_number, "")?.as_any(),
                        PyString::new(py, ". ").as_any(),
                        format_value(scratchpad_content, "")?.as_any(),
                        PyString::new(py, "\n").as_any(),
                    ],
                )?;
                write.call1((entry,))?;
                Ok(())
            })?;
            Ok(())
        })();
        Ok(logged(&module, "Error appending scratchpad entry: ", appended)?.is_ok())
    }

    /// Clear the scratchpad file
    fn clear_scratchpad(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let module = module(slf.py())?;
        let cleared = (|| -> PyResult<()> {
            let exists = os_path(&module, "exists")?;
            if exists.call1((slf.getattr("scratchpad_file")?,))?.is_truthy()? {
                let remove = name(&module, "os")?.getattr("remove")?;
                remove.call1((slf.getattr("scratchpad_file")?,))?;
            }
            Ok(())
        })();
        Ok(logged(&module, "Error clearing scratchpad: ", cleared)?.is_ok())
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
    m.add_class::<ScratchpadService>()?;
    let class = py.get_type::<ScratchpadService>();
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 7)?;
        class.setattr("__static_attributes__", ("scratchpad_dir", "scratchpad_file"))?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
