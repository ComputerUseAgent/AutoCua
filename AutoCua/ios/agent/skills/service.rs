//! <skills> for the iPhone agent: the .md knowledge matched to the app in
//! front.
//!
//! The app in front costs nothing to know: every scan reads WebDriverAgent's
//! page source, whose root element carries the app's name (the scanner gives
//! it as `application_name`). skills.json maps that name, or the app's bundle
//! id, to a markdown file: a default shipped in AutoCua/default_skills/ios/
//! or the user's own in AutoCua_data/skills/ios/ (added from the UI or by
//! hand). A default wins over a user entry of the same key or name.
//!
//! The Rust copy of skills/service.py. Python imports it under its old
//! dotted name, AutoCua.ios.agent.skills.service (skills/__init__.py points
//! that name here), and gets the same class, methods, attributes and log
//! records. Python's own rules decide every result, so they are called, not
//! imitated: str methods, json.loads, pathlib, os.path and truthiness. The
//! values stay Python objects from end to end.
//!
//! The instance keeps its three attributes (dir, defaults, mappings) where a
//! Python instance keeps them, in its __dict__, so they read, write, delete,
//! copy and pickle as before. Its methods reach them, and each other,
//! through the instance, so a subclass or a caller that replaces one is
//! obeyed as before. A name service.py read as a global (os, json, Path,
//! logger) is read from this module each time it is used.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyString, PyTuple};

use crate::ios::NATIVE;
use crate::computer::{as_python_class, unpack2, Handling};
use crate::memory_compression::{format_value, global, is_exception, join_str};

/// The module's docstring, as service.py had one.
const MODULE_DOC: &str = "<skills> for the iOS driver: the .md matched to the app in front.\n\n\
The app in front costs nothing to know. Every scan parses WDA's page source,\n\
whose root XCUIElementTypeApplication carries the app's name (the scanner\n\
gives it as `application_name`). skills.json maps that name, or the app's\n\
bundle id, to a markdown file: a shipped default in AutoCua/default_skills/\n\
ios/ or the user's own in AutoCua_data/skills/ios/.\n";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || {
            py.import(format!("{NATIVE}.agent.skills.service")).map(Bound::unbind)
        })
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now.
fn name<'py>(py: Python<'py>, name: &str) -> PyResult<Bound<'py, PyAny>> {
    global(&module(py)?, name)
}

// ---------------------------------------------------------------------------
// Python's rules
// ---------------------------------------------------------------------------

/// `isinstance(value, dict)`.
fn is_dict(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    value.is_instance(&value.py().get_type::<PyDict>())
}

/// `"".join(parts)`, how an f-string puts its pieces together, as an object.
fn joined<'py>(py: Python<'py>, parts: &[&Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyAny>> {
    Ok(join_str(py, parts)?.into_any())
}

/// `logger.<level>`, through this module's `logger`.
fn logger<'py>(py: Python<'py>, level: &str) -> PyResult<Bound<'py, PyAny>> {
    name(py, "logger")?.getattr(level)
}

/// `path.read_text(encoding="utf-8")`.
fn read_text<'py>(path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(path.py());
    kwargs.set_item("encoding", "utf-8")?;
    path.call_method("read_text", (), Some(&kwargs))
}

/// The folder service.py was in, Path(__file__).resolve().parent: the skills
/// folder of the agent, beside the extension's own folder.
fn own_folder(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let path = name(py, "Path")?;
    let file = py.import("os")?.getattr("path")?.call_method1(
        "join",
        (crate::native_dir(py)?, "ios", "agent", "skills", "service.py"),
    )?;
    path.call1((file,))?.call_method0("resolve")?.getattr("parent")
}

/// __init__ takes self alone. A call that gives more is turned down in the
/// words Python has for a def of that shape, so the first keyword is named
/// before the count of positional arguments.
fn only_self(args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
    let py = args.py();
    let text = |s: &str| PyString::new(py, s).into_any();
    let init = "DomainKnowledgeService.__init__()";
    if let Some((keyword, _)) = kwargs.and_then(|kwargs| kwargs.iter().next()) {
        let message = if keyword.eq("self")? {
            text(&format!("{init} got multiple values for argument 'self'"))
        } else {
            let open = text(&format!("{init} got an unexpected keyword argument '"));
            joined(py, &[&open, keyword.str()?.as_any(), &text("'")])?
        };
        return Err(PyTypeError::new_err((message.unbind(),)));
    }
    if !args.is_empty() {
        return Err(PyTypeError::new_err(format!(
            "{init} takes 1 positional argument but {} were given",
            args.len() + 1
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DomainKnowledgeService
// ---------------------------------------------------------------------------

// The skills of the app in front. A plain comment: the Python class had no
// docstring, and a doc comment here would give it one.
#[pyclass(frozen, dict, weakref, subclass)]
pub struct DomainKnowledgeService;

#[pymethods]
impl DomainKnowledgeService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        DomainKnowledgeService
    }

    /// Finds the two skills folders and loads the mappings.
    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        only_self(args, kwargs)?;
        // Two folders, read together: the defaults shipped in
        // AutoCua/default_skills/ios/ and the user's own skills in
        // AutoCua_data/skills/ios/ (added from the UI or by hand).
        let found = (|| -> PyResult<()> {
            // from AutoCua import skills_dir, default_skills
            let import = name(py, "__import__")?;
            let wanted = ("skills_dir", "default_skills");
            let package = import.call1(("AutoCua", module(py)?.dict(), py.None(), wanted, 0))?;
            let skills_dir = package.getattr("skills_dir")?;
            let default_skills = package.getattr("default_skills")?;
            // self.dir = Path(skills_dir("ios"))
            let path = name(py, "Path")?;
            slf.setattr("dir", path.call1((skills_dir.call1(("ios",))?,))?)?;
            // {filename: text}
            slf.setattr("defaults", default_skills.call1(("ios",))?)?;
            Ok(())
        })();
        match found {
            Ok(()) => {}
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                slf.setattr("dir", own_folder(py)?)?;
                slf.setattr("defaults", PyDict::new(py))?;
            }
            Err(e) => return Err(e),
        }
        slf.setattr("mappings", slf.call_method0("_load")?)?;
        Ok(())
    }

    /// skills.json of the defaults merged with the user's skills.json. The
    /// default entry wins for a key present in both: the user's file only
    /// adds.
    #[pyo3(name = "_load")]
    fn load<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let merged = PyDict::new(py);
        let mut sources: Vec<(&str, Bound<'py, PyAny>)> = Vec::new();
        if slf.getattr("defaults")?.call_method1("get", ("skills.json",))?.is_truthy()? {
            sources.push(("default", slf.getattr("defaults")?.get_item("skills.json")?));
        }
        let read = (|| -> PyResult<()> {
            let p = slf.getattr("dir")?.div("skills.json")?;
            if p.call_method0("is_file")?.is_truthy()? {
                sources.push(("user", read_text(&p)?));
            }
            Ok(())
        })();
        if let Err(e) = read {
            if !is_exception(py, &e) {
                return Err(e);
            }
            let _handling = Handling::new(py, &e);
            let error = logger(py, "error")?;
            error.call1(("Error reading user skills.json: %s", e.value(py)))?;
        }
        for (origin, source) in &sources {
            let data = match (|| name(py, "json")?.getattr("loads")?.call1((source,)))() {
                Ok(data) => data,
                Err(e) if is_exception(py, &e) => {
                    let _handling = Handling::new(py, &e);
                    let error = logger(py, "error")?;
                    error.call1(("Error loading %s skills.json: %s", *origin, e.value(py)))?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !is_dict(&data)? {
                continue;
            }
            for pair in data.call_method0("items")?.try_iter()? {
                let (bucket, mapping) = unpack2(&pair?)?;
                if !is_dict(&mapping)? {
                    continue;
                }
                let target = merged.call_method1("setdefault", (bucket, PyDict::new(py)))?;
                for pair in mapping.call_method0("items")?.try_iter()? {
                    let (key, md) = unpack2(&pair?)?;
                    // The first one, the default, wins.
                    target.call_method1("setdefault", (key, md))?;
                }
            }
        }
        if sources.is_empty() {
            let warning = logger(py, "warning")?;
            warning.call1(("skills.json not found in %s or the defaults", slf.getattr("dir")?))?;
        }
        Ok(merged)
    }

    // str(text or "").strip().lower(): how an app's name or a key is
    // compared. A plain comment: the Python function had no docstring.
    #[staticmethod]
    #[pyo3(name = "_norm")]
    fn norm<'py>(text: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = text.py();
        let value = if text.is_truthy()? { text } else { PyString::new(py, "").into_any() };
        value.str()?.call_method0("strip")?.call_method0("lower")
    }

    /// The skill's filename for the app in front, or "" (the home screen, no
    /// entry). The exact name first. Then the app's bundle id, found in the
    /// list of installed apps the launcher already keeps (nothing is asked
    /// of the device), so a renamed app still finds its skill.
    #[pyo3(name = "match")]
    fn match_app<'py>(slf: &Bound<'py, Self>, application_name: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let app = slf.call_method1("_norm", (application_name,))?;
        if !app.is_truthy()? {
            return Ok(text(""));
        }
        // apps = self.mappings.get("apps") or {}
        let apps = slf.getattr("mappings")?.call_method1("get", ("apps",))?;
        let apps = if apps.is_truthy()? { apps } else { PyDict::new(py).into_any() };
        // by_key = {self._norm(k): v for k, v in apps.items()}
        let by_key = PyDict::new(py);
        for pair in apps.call_method0("items")?.try_iter()? {
            let (k, v) = unpack2(&pair?)?;
            by_key.set_item(slf.call_method1("_norm", (k,))?, v)?;
        }
        if by_key.contains(&app)? {
            return by_key.as_any().get_item(&app);
        }
        let found = (|| -> PyResult<Option<Bound<'py, PyAny>>> {
            // from ...controller.tool.open_app import app_launcher_service
            let import = name(py, "__import__")?;
            let wanted = ("app_launcher_service",);
            let open_app = import.call1(("controller.tool.open_app", module(py)?.dict(), py.None(), wanted, 3))?;
            let app_launcher_service = open_app.getattr("app_launcher_service")?;
            for info in app_launcher_service.getattr("apps_dict")?.call_method0("values")?.try_iter()? {
                let info = info?;
                let norm = slf.getattr("_norm")?;
                if norm.call1((info.call_method1("get", ("display_name",))?,))?.eq(&app)? {
                    let get = by_key.as_any().getattr("get")?;
                    let norm = slf.getattr("_norm")?;
                    let md = get.call1((norm.call1((info.call_method1("get", ("bundle_id",))?,))?,))?;
                    if md.is_truthy()? {
                        return Ok(Some(md));
                    }
                }
            }
            Ok(None)
        })();
        match found {
            Ok(Some(md)) => Ok(md),
            Ok(None) => Ok(text("")),
            Err(e) if is_exception(py, &e) => Ok(text("")),
            Err(e) => Err(e),
        }
    }

    /// The .md text: the shipped default of that name if there is one, else
    /// the user's copy, else "" (a missing skill is ignored). One name loads
    /// once.
    #[pyo3(name = "_read")]
    fn read<'py>(slf: &Bound<'py, Self>, filename: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let loaded = (|| -> PyResult<Option<Bound<'py, PyAny>>> {
            if slf.getattr("defaults")?.contains(&filename)? {
                return slf.getattr("defaults")?.get_item(&filename)?.call_method0("strip").map(Some);
            }
            let p = slf.getattr("dir")?.div(&filename)?;
            if p.call_method0("is_file")?.is_truthy()? {
                return read_text(&p)?.call_method0("strip").map(Some);
            }
            let warning = logger(py, "warning")?;
            warning.call1(("Skill file not found: %s", p))?;
            Ok(None)
        })();
        match loaded {
            Ok(Some(content)) => Ok(content),
            Ok(None) => Ok(text("")),
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                let error = logger(py, "error")?;
                error.call1(("Error reading skill %s: %s", &filename, e.value(py)))?;
                Ok(text(""))
            }
            Err(e) => Err(e),
        }
    }

    /// <domain_knowledge="<skill>">...</domain_knowledge> for the app in
    /// front, or "" when nothing matches.
    fn get_knowledge<'py>(slf: &Bound<'py, Self>, application_name: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let md = slf.call_method1("match", (application_name,))?;
        if !md.is_truthy()? {
            return Ok(text(""));
        }
        let content = slf.call_method1("_read", (&md,))?;
        if !content.is_truthy()? {
            return Ok(text(""));
        }
        // context = os.path.splitext(md)[0]
        let context = name(py, "os")?.getattr("path")?.getattr("splitext")?.call1((&md,))?.get_item(0)?;
        // f'<domain_knowledge="{context}">\n{text}\n</domain_knowledge>'
        joined(
            py,
            &[
                &text("<domain_knowledge=\""),
                format_value(&context, "")?.as_any(),
                &text("\">\n"),
                format_value(&content, "")?.as_any(),
                &text("\n</domain_knowledge>"),
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place: the names service.py
/// had, its imports (json, logging, os, Path) and its logger included.
///
/// `m` carries Python's name for the module (lib.rs makes it so): the
/// logger is named after it, logging.getLogger(__name__) as before, and the
/// class reports it as its __module__.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    m.add("json", py.import("json")?)?;
    let logging = py.import("logging")?;
    m.add("logging", &logging)?;
    m.add("os", py.import("os")?)?;
    m.add("Path", py.import("pathlib")?.getattr("Path")?)?;
    m.add("logger", logging.call_method1("getLogger", (&python_name,))?)?;
    m.add_class::<DomainKnowledgeService>()?;
    let class = py.get_type::<DomainKnowledgeService>();
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
