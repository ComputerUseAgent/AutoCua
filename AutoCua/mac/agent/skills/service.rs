//! <skills> for the computer-use agent: the .md knowledge matched to the app
//! in front, and to the site when that app is a web browser.
//!
//! Two folders are read together: the defaults shipped in
//! AutoCua/default_skills/<package>/ and the user's own skills in
//! AutoCua_data/skills/<package>/ (added from the UI or by hand). skills.json
//! in each maps an app's name ("os") or a site's address ("browser") to a
//! markdown file. A default wins over a user entry of the same key or name.
//!
//! The Rust copy of skills/service.py. Python imports it under its old
//! dotted name, AutoCua.<package>.agent.skills.service (skills/__init__.py
//! points that name here), and gets the same class, methods, attributes and
//! log records. Python's own rules decide every result, so they are called,
//! not imitated: str methods, re, json.loads, os.path, truthiness and a
//! text-mode open(). The values stay Python objects from end to end.
//!
//! The instance keeps its four attributes (current_dir, defaults, mappings,
//! browser_keywords) where a Python instance keeps them, in its __dict__, so
//! they read, write, delete, copy and pickle as before. Its methods reach
//! them, and each other, through the instance, so a subclass or a caller
//! that replaces one is obeyed as before. A name service.py read as a global
//! (os, re, json, logger, open) is read from this module each time it is
//! used.

use pyo3::exceptions::{PyAttributeError, PyTypeError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyList, PySlice, PyString, PyTuple};

use crate::mac::{as_python_class, platform, type_name, unpack2, Handling, NATIVE};
use crate::memory_compression::{format_value, global, is_exception, join_str};

/// A row of the element tree that has a value: the element's name and the
/// value.
const ROW_PATTERN: &str = r#"<element name="([^"]*)"[^>]*valuePattern\.value="([^"]*)""#;

/// The names an address bar goes by.
const ADDRESS_BAR_PATTERN: &str = r"address|search bar|\burl\b|location";

/// Any value in the element tree.
const VALUE_PATTERN: &str = r#"valuePattern\.value="([^"]*)""#;

/// A bare hostname with at least one dot, then maybe a port, path, query or
/// fragment.
const HOST_PATTERN: &str = r"^[a-zA-Z0-9\-]+(\.[a-zA-Z0-9\-]+)+([/:?#].*)?$";

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

/// `str(e)` of a caught exception.
fn error_text<'py>(py: Python<'py>, e: &PyErr) -> PyResult<Bound<'py, PyAny>> {
    Ok(e.value(py).str()?.into_any())
}

/// `os.path.<function>`, through this module's `os`.
fn os_path<'py>(py: Python<'py>, function: &str) -> PyResult<Bound<'py, PyAny>> {
    name(py, "os")?.getattr("path")?.getattr(function)
}

/// `logger.<level>`, through this module's `logger`.
fn logger<'py>(py: Python<'py>, level: &str) -> PyResult<Bound<'py, PyAny>> {
    name(py, "logger")?.getattr(level)
}

/// `open(path, 'r', encoding='utf-8')` with Python's open, the one a test
/// or a packaged app may have put in its place.
fn open_text<'py>(py: Python<'py>, path: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let open = name(py, "open")?;
    let kwargs = PyDict::new(py);
    kwargs.set_item("encoding", "utf-8")?;
    open.call((path, "r"), Some(&kwargs))
}

/// One of the two methods a `with` statement looks up on the manager's
/// type. A type without it gets the error the running Python gives.
fn with_method<'py>(manager: &Bound<'py, PyAny>, method: &str, missed: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = manager.py();
    match manager.get_type().getattr(method) {
        Ok(found) => Ok(found),
        Err(e) if e.is_instance_of::<PyAttributeError>(py) => {
            if py.version_info() >= (3, 11) {
                Err(PyTypeError::new_err(format!(
                    "'{}' object does not support the context manager protocol{missed}",
                    type_name(manager)?
                )))
            } else {
                Err(PyAttributeError::new_err(method.to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// `with manager as f: body(f)`. Some(result) when the body ran to its end,
/// None when it raised and the manager's __exit__ took the error, so the
/// code after the block runs.
fn with_block<'py, T>(
    manager: &Bound<'py, PyAny>,
    body: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<T>,
) -> PyResult<Option<T>> {
    let py = manager.py();
    let enter = with_method(manager, "__enter__", "")?;
    let exit = with_method(manager, "__exit__", " (missed __exit__ method)")?;
    let target = enter.call1((manager,))?;
    match body(&target) {
        Ok(result) => {
            exit.call1((manager, py.None(), py.None(), py.None()))?;
            Ok(Some(result))
        }
        Err(e) => {
            let _handling = Handling::new(py, &e);
            let traceback = e.traceback(py).map(|t| t.into_any()).unwrap_or_else(|| py.None().into_bound(py));
            if exit.call1((manager, e.get_type(py), e.value(py), traceback))?.is_truthy()? {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

/// `a += b`.
fn add_in_place<'py>(a: &Bound<'py, PyAny>, b: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    // SAFETY: two valid objects; a new reference or NULL with the error set.
    unsafe { Bound::from_owned_ptr_or_err(a.py(), ffi::PyNumber_InPlaceAdd(a.as_ptr(), b.as_ptr())) }
}

/// `text[start:]`.
fn slice_from<'py>(text: &Bound<'py, PyAny>, start: usize) -> PyResult<Bound<'py, PyAny>> {
    let py = text.py();
    text.get_item(py.get_type::<PySlice>().call1((start, py.None()))?)
}

/// The folder service.py was in, os.path.dirname(os.path.abspath(__file__)):
/// the skills folder of the agent, beside the extension's own folder.
fn own_folder(py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
    let dirname = os_path(py, "dirname")?;
    let abspath = os_path(py, "abspath")?;
    let file = py.import("os")?.getattr("path")?.call_method1(
        "join",
        (crate::native_dir(py)?, platform().package, "agent", "skills", "service.py"),
    )?;
    dirname.call1((abspath.call1((file,))?,))
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

/// `<domain_knowledge="{context}">\n{content}\n</domain_knowledge>`.
fn domain_knowledge<'py>(context: &Bound<'py, PyAny>, content: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = context.py();
    let text = |s: &str| PyString::new(py, s).into_any();
    joined(
        py,
        &[
            &text("<domain_knowledge=\""),
            format_value(context, "")?.as_any(),
            &text("\">\n"),
            format_value(content, "")?.as_any(),
            &text("\n</domain_knowledge>"),
        ],
    )
}

// ---------------------------------------------------------------------------
// DomainKnowledgeService
// ---------------------------------------------------------------------------

/// Service for injecting domain-specific knowledge based on browser URL or OS app
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
        // AutoCua/default_skills/<package>/ and the user's own skills in
        // AutoCua_data/skills/<package>/ (added from the UI or by hand).
        let found = (|| -> PyResult<()> {
            // from AutoCua import skills_dir, default_skills
            let import = name(py, "__import__")?;
            let wanted = ("skills_dir", "default_skills");
            let package = import.call1(("AutoCua", module(py)?.dict(), py.None(), wanted, 0))?;
            let skills_dir = package.getattr("skills_dir")?;
            let default_skills = package.getattr("default_skills")?;
            let skills = platform().package;
            slf.setattr("current_dir", skills_dir.call1((skills,))?.str()?)?;
            // {filename: text}
            slf.setattr("defaults", default_skills.call1((skills,))?)?;
            Ok(())
        })();
        match found {
            Ok(()) => {}
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                slf.setattr("current_dir", own_folder(py)?)?;
                slf.setattr("defaults", PyDict::new(py))?;
            }
            Err(e) => return Err(e),
        }
        slf.setattr("mappings", slf.call_method0("_load_mappings")?)?;
        // The words that mark an app as a web browser.
        slf.setattr("browser_keywords", PyList::new(py, platform().browser_keywords)?)?;
        Ok(())
    }

    /// skills.json of the defaults merged with the user's skills.json. The
    /// default entry wins for a key present in both: the user's file only
    /// adds. A missing or broken file on either side gives nothing.
    #[pyo3(name = "_load_mappings")]
    fn load_mappings<'py>(slf: &Bound<'py, Self>) -> PyResult<Bound<'py, PyDict>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let merged = PyDict::new(py);
        merged.set_item("browser", PyDict::new(py))?;
        merged.set_item("os", PyDict::new(py))?;
        let mut sources: Vec<(&str, Bound<'py, PyAny>)> = Vec::new();
        if slf.getattr("defaults")?.call_method1("get", ("skills.json",))?.is_truthy()? {
            sources.push(("default", slf.getattr("defaults")?.get_item("skills.json")?));
        }
        let read = (|| -> PyResult<()> {
            let json_path = os_path(py, "join")?.call1((slf.getattr("current_dir")?, "skills.json"))?;
            if os_path(py, "exists")?.call1((&json_path,))?.is_truthy()? {
                let manager = open_text(py, &json_path)?;
                with_block(&manager, |f| {
                    sources.push(("user", f.call_method0("read")?));
                    Ok(())
                })?;
            }
            Ok(())
        })();
        if let Err(e) = read {
            if !is_exception(py, &e) {
                return Err(e);
            }
            let _handling = Handling::new(py, &e);
            let error = logger(py, "error")?;
            error.call1((joined(py, &[&text("Error reading user skills.json: "), &error_text(py, &e)?])?,))?;
        }
        for (origin, source) in &sources {
            let data = match (|| name(py, "json")?.getattr("loads")?.call1((source,)))() {
                Ok(data) => data,
                Err(e) if is_exception(py, &e) => {
                    let _handling = Handling::new(py, &e);
                    let error = logger(py, "error")?;
                    let line = joined(
                        py,
                        &[&text("Error loading "), &text(origin), &text(" skills.json: "), &error_text(py, &e)?],
                    )?;
                    error.call1((line,))?;
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
            logger(py, "warning")?.call1(("skills.json not found",))?;
        }
        Ok(merged)
    }

    /// Check if the application is a web browser
    #[pyo3(name = "_is_browser")]
    fn is_browser(slf: &Bound<'_, Self>, application_name: Bound<'_, PyAny>) -> PyResult<bool> {
        let app_lower = application_name.call_method0("lower")?;
        // any(keyword in app_lower for keyword in self.browser_keywords)
        for keyword in slf.getattr("browser_keywords")?.try_iter()? {
            if app_lower.contains(keyword?)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The browser's URL from the element tree, with or without its scheme.
    ///
    /// Browsers often show the address with the scheme taken off
    /// ("github.com/..."), and the address bar's name differs from browser
    /// to browser. So every `valuePattern.value` field is read: first one
    /// whose name looks like an address, search, url or location bar, then
    /// any value that looks like a URL or a bare domain. A result that is no
    /// URL does no harm: it loads a skill only when it matches a domain in
    /// skills.json.
    #[pyo3(name = "_extract_url")]
    fn extract_url<'py>(slf: &Bound<'py, Self>, element_tree: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let found = (|| -> PyResult<Bound<'py, PyAny>> {
            let mut candidate = text("");
            let rows = name(py, "re")?.getattr("finditer")?.call1((ROW_PATTERN, &element_tree))?;
            for m in rows.try_iter()? {
                let m = m?;
                let row_name = m.call_method1("group", (1,))?;
                let value = m.call_method1("group", (2,))?;
                let search = name(py, "re")?.getattr("search")?;
                let ignore_case = name(py, "re")?.getattr("IGNORECASE")?;
                if search.call1((ADDRESS_BAR_PATTERN, row_name, ignore_case))?.is_truthy()? {
                    if slf.call_method1("_looks_like_url", (&value,))?.is_truthy()? {
                        return Ok(value);
                    }
                    // Named like an address bar, but an odd value: held.
                    if !candidate.is_truthy()? {
                        candidate = value;
                    }
                }
            }
            if candidate.is_truthy()? {
                return Ok(candidate);
            }
            // Fallback: any value that looks like a URL or a bare domain.
            let values = name(py, "re")?.getattr("finditer")?.call1((VALUE_PATTERN, &element_tree))?;
            for m in values.try_iter()? {
                let m = m?;
                if slf.call_method1("_looks_like_url", (m.call_method1("group", (1,))?,))?.is_truthy()? {
                    return m.call_method1("group", (1,));
                }
            }
            Ok(text(""))
        })();
        match found {
            Ok(url) => Ok(url),
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                let error = logger(py, "error")?;
                error.call1((joined(py, &[&text("Error extracting URL: "), &error_text(py, &e)?])?,))?;
                Ok(text(""))
            }
            Err(e) => Err(e),
        }
    }

    /// Heuristic: does this field value look like a URL or bare domain?
    #[pyo3(name = "_looks_like_url")]
    fn looks_like_url(&self, value: Bound<'_, PyAny>) -> PyResult<bool> {
        let py = value.py();
        let v = value.call_method0("strip")?;
        if !v.is_truthy()? || v.contains(" ")? {
            return Ok(false);
        }
        if v.call_method1("startswith", ("http://",))?.is_truthy()?
            || v.call_method1("startswith", ("https://",))?.is_truthy()?
        {
            return Ok(true);
        }
        // A bare hostname with at least one dot, then maybe a port, path,
        // query or fragment.
        name(py, "re")?.getattr("match")?.call1((HOST_PATTERN, v))?.is_truthy()
    }

    /// Strip protocol (https://, http://) from URL for comparison
    #[pyo3(name = "_normalize_url")]
    fn normalize_url<'py>(&self, url: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let url = url.call_method0("strip")?;
        if url.call_method1("startswith", ("https://",))?.is_truthy()? {
            return slice_from(&url, 8);
        } else if url.call_method1("startswith", ("http://",))?.is_truthy()? {
            return slice_from(&url, 7);
        }
        Ok(url)
    }

    /// Match URL against browser patterns, return .md filename or empty string
    #[pyo3(name = "_match_browser_pattern")]
    fn match_browser_pattern<'py>(slf: &Bound<'py, Self>, url: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        if !url.is_truthy()? {
            return Ok(PyString::new(py, "").into_any());
        }
        let browser_patterns = slf.getattr("mappings")?.call_method1("get", ("browser", PyDict::new(py)))?;
        let normalized_url = slf.call_method1("_normalize_url", (url,))?;
        // Just the domain of the URL.
        let domain = normalized_url.call_method1("split", ("/",))?.get_item(0)?;
        let mut best_match = PyString::new(py, "").into_any();
        let mut best_length = 0;
        for pair in browser_patterns.call_method0("items")?.try_iter()? {
            let (pattern, md_file) = unpack2(&pair?)?;
            let normalized_pattern = slf.call_method1("_normalize_url", (pattern,))?;
            // The domain ends with the pattern: subdomains match too.
            if (domain.call_method1("endswith", (&normalized_pattern,))?.is_truthy()?
                || domain.eq(&normalized_pattern)?)
                && normalized_pattern.len()? > best_length
            {
                best_match = md_file;
                best_length = normalized_pattern.len()?;
            }
        }
        Ok(best_match)
    }

    /// Match application name against OS patterns, return .md filename or empty string
    #[pyo3(name = "_match_os_pattern")]
    fn match_os_pattern<'py>(
        slf: &Bound<'py, Self>,
        application_name: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let os_patterns = slf.getattr("mappings")?.call_method1("get", ("os", PyDict::new(py)))?;
        let app_lower = application_name.call_method0("lower")?;
        // The longest matching key wins (the browser matcher's rule), so a
        // user's "Google Chrome" skill beats the shipped "chrome".
        let mut best_match = PyString::new(py, "").into_any();
        let mut best_length = 0;
        for pair in os_patterns.call_method0("items")?.try_iter()? {
            let (app_pattern, md_file) = unpack2(&pair?)?;
            if app_lower.contains(app_pattern.call_method0("lower")?)? && app_pattern.len()? > best_length {
                best_match = md_file;
                best_length = app_pattern.len()?;
            }
        }
        Ok(best_match)
    }

    /// The .md text: the shipped default of that name if there is one, else
    /// the user's copy in AutoCua_data/skills/, else "" (a missing skill is
    /// ignored, never an error). One name loads once.
    #[pyo3(name = "_load_knowledge_file")]
    fn load_knowledge_file<'py>(slf: &Bound<'py, Self>, filename: Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let loaded = (|| -> PyResult<Bound<'py, PyAny>> {
            if slf.getattr("defaults")?.contains(&filename)? {
                return slf.getattr("defaults")?.get_item(&filename)?.call_method0("strip");
            }
            let file_path = os_path(py, "join")?.call1((slf.getattr("current_dir")?, &filename))?;
            if os_path(py, "exists")?.call1((&file_path,))?.is_truthy()? {
                let manager = open_text(py, &file_path)?;
                let read = with_block(&manager, |f| f.call_method0("read")?.call_method0("strip"))?;
                if let Some(content) = read {
                    return Ok(content);
                }
            }
            let warning = logger(py, "warning")?;
            let line = joined(py, &[&text("Knowledge file not found: "), format_value(&filename, "")?.as_any()])?;
            warning.call1((line,))?;
            Ok(text(""))
        })();
        match loaded {
            Ok(content) => Ok(content),
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                let error = logger(py, "error")?;
                let line = joined(
                    py,
                    &[
                        &text("Error loading knowledge file "),
                        format_value(&filename, "")?.as_any(),
                        &text(": "),
                        &error_text(py, &e)?,
                    ],
                )?;
                error.call1((line,))?;
                Ok(text(""))
            }
            Err(e) => Err(e),
        }
    }

    /// The main method: the knowledge for what is in front.
    ///
    /// application_name: the app's name, from the scanner.
    /// element_tree: the element tree's text, from the scanner.
    ///
    /// Returns <browser_guidelines> (with a <domain_knowledge> inside when
    /// the URL matches a site) for a browser, a <domain_knowledge> of its
    /// own for another app that has a skill, or "".
    fn get_knowledge<'py>(
        slf: &Bound<'py, Self>,
        application_name: Bound<'py, PyAny>,
        element_tree: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let py = slf.py();
        let text = |s: &str| PyString::new(py, s).into_any();
        let found = (|| -> PyResult<Bound<'py, PyAny>> {
            let is_browser = slf.call_method1("_is_browser", (&application_name,))?;

            if is_browser.is_truthy()? {
                // browser.md, through the OS pattern match.
                let browser_md = slf.call_method1("_match_os_pattern", (&application_name,))?;
                let browser_content = if browser_md.is_truthy()? {
                    slf.call_method1("_load_knowledge_file", (browser_md,))?
                } else {
                    text("")
                };

                // The URL, for knowledge about the site.
                let mut domain_block = text("");
                let url = slf.call_method1("_extract_url", (&element_tree,))?;
                if url.is_truthy()? {
                    let domain_md = slf.call_method1("_match_browser_pattern", (url,))?;
                    if domain_md.is_truthy()? {
                        let context = domain_md.call_method1("replace", (".md", ""))?;
                        let domain_content = slf.call_method1("_load_knowledge_file", (domain_md,))?;
                        if domain_content.is_truthy()? {
                            domain_block = domain_knowledge(&context, &domain_content)?;
                        }
                    }
                }

                // The site's block goes inside the browser's.
                if browser_content.is_truthy()? {
                    let mut inner = browser_content;
                    if domain_block.is_truthy()? {
                        let more = joined(py, &[&text("\n"), format_value(&domain_block, "")?.as_any()])?;
                        inner = add_in_place(&inner, &more)?;
                    }
                    return joined(
                        py,
                        &[
                            &text("<browser_guidelines>\n"),
                            format_value(&inner, "")?.as_any(),
                            &text("\n</browser_guidelines>"),
                        ],
                    );
                }

                return Ok(text(""));
            }

            // Not a browser: the app's own skill, if it has one.
            let os_md = slf.call_method1("_match_os_pattern", (&application_name,))?;
            if os_md.is_truthy()? {
                let context = os_md.call_method1("replace", (".md", ""))?;
                let content = slf.call_method1("_load_knowledge_file", (os_md,))?;
                if content.is_truthy()? {
                    return domain_knowledge(&context, &content);
                }
            }

            Ok(text(""))
        })();
        match found {
            Ok(knowledge) => Ok(knowledge),
            Err(e) if is_exception(py, &e) => {
                let _handling = Handling::new(py, &e);
                let error = logger(py, "error")?;
                error.call1((joined(py, &[&text("Error getting domain knowledge: "), &error_text(py, &e)?])?,))?;
                Ok(text(""))
            }
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes service.py's place: the names service.py
/// had, its imports (os, json, re, logging) and its logger included.
///
/// `m` carries Python's name for the module (lib.rs makes it so): the
/// logger is named after it, logging.getLogger(__name__) as before, and the
/// class reports it as its __module__.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.add("os", py.import("os")?)?;
    m.add("json", py.import("json")?)?;
    m.add("re", py.import("re")?)?;
    let logging = py.import("logging")?;
    m.add("logging", &logging)?;
    m.add("logger", logging.call_method1("getLogger", (&python_name,))?)?;
    m.add_class::<DomainKnowledgeService>()?;
    as_python_class(&py.get_type::<DomainKnowledgeService>(), &python_name)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
