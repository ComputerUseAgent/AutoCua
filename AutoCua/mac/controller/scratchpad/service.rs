//! The scratchpad, the computer-use agent's notes on macOS, Windows and
//! Linux: numbered lines in <system>/scratchpad/milestone/milestone.md (the
//! mac, windows or linux folder of AutoCua). The controller's scratchpad
//! action (<system>/controller/view.py) adds a line. The folder is
//! milestone/milestone.md, not scratchpad/scratchpad.md, to keep the notes
//! already on disk and to keep clear of the parent folder's name.
//!
//! ONE file for the three systems: their scratchpad/service.py were the same
//! code, only the folder differed. It is the Rust copy of that service.py.
//! scratchpad_for! below gives each system its own class and module, which
//! Python imports under the old dotted name,
//! AutoCua.<system>.controller.scratchpad.service (each system's
//! scratchpad/__init__.py points that name here), and gets the same class,
//! methods, attributes, return values and log records. Python's own rules
//! decide every result, so they are called, not imitated: os.path, open in
//! text mode (its line endings and UTF-8), str.strip and the f-string.
//!
//! It is the iPhone's scratchpad (ios/controller/scratchpad/service.rs)
//! without two things these service.py files never had: a session folder for
//! parallel tasks (os.environ is not read), and read_scratchpad.
//!
//! The instance keeps its two attributes (scratchpad_dir, scratchpad_file)
//! where a Python instance keeps them, in its __dict__, and its methods read
//! them through the instance, so a caller that points them elsewhere is
//! obeyed. A name service.py read as a global (os, logger, open, str) is read
//! from the system's module each time it is used (the helpers in lib.rs's
//! ios::controller).

use pyo3::prelude::*;
use pyo3::types::{PyString, PyType};

use crate::computer::as_python_class;
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{agent_dir, logged, make_dir, name, open_utf8, os_path};
use crate::memory_compression::{format_value, join_str};

// ---------------------------------------------------------------------------
// The methods, the same for every system: `module` is the system's module
// (its globals are read from it), `agent` its folder (mac, windows, linux).
// ---------------------------------------------------------------------------

/// ScratchpadService.__init__, after its arguments were checked.
fn init(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>, agent: &str) -> PyResult<()> {
    // Go up two levels from AutoCua/<system>/controller/scratchpad/ to reach
    // the system's folder (agent_dir finds it from service.py's __file__, as
    // before).
    // On-disk storage paths kept as "milestone/milestone.md" to preserve existing data
    // and avoid the scratchpad/scratchpad/scratchpad.md collision with the parent dir.
    slf.setattr("scratchpad_dir", agent_dir(module, agent, "scratchpad", "milestone", None)?)?;

    let join = os_path(module, "join")?;
    slf.setattr("scratchpad_file", join.call1((slf.getattr("scratchpad_dir")?, "milestone.md"))?)?;

    // Create scratchpad directory if it doesn't exist
    slf.call_method0("_ensure_scratchpad_directory")?;
    Ok(())
}

/// _ensure_scratchpad_directory: logged, then raised again.
fn ensure_directory(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>) -> PyResult<()> {
    make_dir(module, slf, "scratchpad_dir", "Error creating scratchpad directory: ")
}

/// append_scratchpad
fn append_scratchpad(
    slf: &Bound<'_, PyAny>,
    module: &Bound<'_, PyModule>,
    scratchpad_content: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    let py = slf.py();
    let appended = (|| -> PyResult<()> {
        let mut existing_count: u64 = 0;
        let exists = os_path(module, "exists")?;
        if exists.call1((slf.getattr("scratchpad_file")?,))?.is_truthy()? {
            let f = open_utf8(module, slf, "scratchpad_file", "r")?;
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

        let f = open_utf8(module, slf, "scratchpad_file", "a")?;
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
    Ok(logged(module, "Error appending scratchpad entry: ", appended)?.is_ok())
}

/// clear_scratchpad
fn clear_scratchpad(slf: &Bound<'_, PyAny>, module: &Bound<'_, PyModule>) -> PyResult<bool> {
    let cleared = (|| -> PyResult<()> {
        let exists = os_path(module, "exists")?;
        if exists.call1((slf.getattr("scratchpad_file")?,))?.is_truthy()? {
            let remove = name(module, "os")?.getattr("remove")?;
            remove.call1((slf.getattr("scratchpad_file")?,))?;
        }
        Ok(())
    })();
    Ok(logged(module, "Error clearing scratchpad: ", cleared)?.is_ok())
}

/// Fills a system's module, the one that takes its service.py's place: the
/// names service.py had, its imports (os, logging) and its logger included.
/// `add_class` adds the system's class to it and gives it back.
///
/// `m` carries Python's name for the module (lib.rs makes it so): the
/// logger is named after it, logging.getLogger(__name__) as before, and the
/// class reports it as its __module__.
fn fill<'py>(m: &Bound<'py, PyModule>, add_class: impl FnOnce() -> PyResult<Bound<'py, PyType>>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add("os", py.import("os")?)?;
    let logging = py.import("logging")?;
    m.add("logging", &logging)?;
    m.add("logger", logging.call_method1("getLogger", (&python_name,))?)?;
    let class = add_class()?;
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    // What Python 3.13 gave the class statement: its line in service.py and
    // the attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 7)?;
        class.setattr("__static_attributes__", ("scratchpad_dir", "scratchpad_file"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// One class and module per system
// ---------------------------------------------------------------------------

/// A system's ScratchpadService (its own class, so its own __module__) and
/// its module's register(), for lib.rs's mac::register_system.
macro_rules! scratchpad_for {
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
                        py.import(concat!("AutoCua.agent_native.", $agent, ".controller.scratchpad.service"))
                            .map(Bound::unbind)
                    })
                    .map(|m| m.bind(py).clone())
            }

            // The agent's notes. A plain comment: the Python class had no
            // docstring, and a doc comment here would give it one.
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
                fn __init__(
                    slf: &Bound<'_, Self>,
                    args: &Bound<'_, PyTuple>,
                    kwargs: Option<&Bound<'_, PyDict>>,
                ) -> PyResult<()> {
                    only_self("ScratchpadService.__init__()", args, kwargs)?;
                    super::init(slf.as_any(), &module(slf.py())?, $agent)
                }

                /// Create scratchpad directory if it doesn't exist
                fn _ensure_scratchpad_directory(slf: &Bound<'_, Self>) -> PyResult<()> {
                    super::ensure_directory(slf.as_any(), &module(slf.py())?)
                }

                /// Append a scratchpad entry with sequential numbering
                fn append_scratchpad(slf: &Bound<'_, Self>, scratchpad_content: &Bound<'_, PyAny>) -> PyResult<bool> {
                    super::append_scratchpad(slf.as_any(), &module(slf.py())?, scratchpad_content)
                }

                /// Clear the scratchpad file
                fn clear_scratchpad(slf: &Bound<'_, Self>) -> PyResult<bool> {
                    super::clear_scratchpad(slf.as_any(), &module(slf.py())?)
                }
            }

            /// Fills this system's module: the names its service.py had.
            pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
                super::fill(m, || {
                    m.add_class::<ScratchpadService>()?;
                    Ok(m.py().get_type::<ScratchpadService>())
                })?;
                let _ = MODULE.set(m.py(), m.clone().unbind());
                Ok(())
            }
        }
    };
}

scratchpad_for!(mac, "mac");
scratchpad_for!(windows, "windows");
scratchpad_for!(linux, "linux");
