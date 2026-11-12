//! open_app: the iPhone agent's app launcher. It reads the installed apps
//! (pymobiledevice3 over the cable, simctl for a simulator, Xcode's
//! devicectl for a phone on Wi-Fi only), picks the ONE app a name means or
//! says why it can't, launches it through WebDriverAgent and checks it came
//! to the foreground, and goes to the home screen.
//!
//! The Rust copy of tool/open_app.py. Python imports it under its old dotted
//! name, AutoCua.ios.controller.tool.open_app (tool/__init__.py points that
//! name here), and gets the same class, methods, attributes, return values
//! and log records. Python's own rules decide every result, so they are
//! called, not imitated: requests, subprocess, json, difflib, tempfile,
//! pathlib, time, the str and dict methods, the comparisons, sort and the
//! f-strings.
//!
//! The names open_app.py imported (difflib, json, os, subprocess, tempfile,
//! time, requests, logging, Path), its logger and its one launcher,
//! app_launcher_service, are bound on this module by tool/__init__.py when
//! the module is first imported, as the .py did then. Loading the extension
//! imports none of them, so requests is not loaded by agents that never use
//! the iPhone. Each is read from this module each time it is used, so a
//! caller or a test that replaces one is obeyed.
//!
//! The instance keeps its attributes (wda_url, session_id, apps_dict) in its
//! __dict__, as a Python instance does, and its methods reach them and each
//! other through the instance, so a subclass that replaces one is obeyed.

use pyo3::exceptions::{PyAttributeError, PyImportError, PyUnboundLocalError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyCFunction, PyDict, PyInt, PyList, PyTuple};

use crate::computer::{as_python_class, unpack2, Handling};
use crate::ios::agent::iphone_use::service::with;
use crate::ios::controller::{dict, fstring, log, log_failure, only_self, text};
use crate::ios::NATIVE;
use crate::memory_compression::{global, is_exception};

type Obj<'py> = Bound<'py, PyAny>;

/// open_app.py's docstring.
const MODULE_DOC: &str =
    "\nopen_app.py - iPhone App Launcher Service\nHandles app launching and navigation on iPhone via WebDriverAgent\n";

/// The class's docstring.
const CLASS_DOC: &str = "Service for launching and managing iPhone apps";

/// The module open_app.py's functions imported their helpers from.
const SESSION: &str = "AutoCua.ios_connector.session";

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.controller.tool.open_app")).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now.
fn name<'py>(py: Python<'py>, name: &str) -> PyResult<Obj<'py>> {
    global(&module(py)?, name)
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

fn int(py: Python<'_>, value: i64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

fn float(py: Python<'_>, value: f64) -> PyResult<Obj<'_>> {
    Ok(value.into_pyobject(py)?.into_any())
}

/// A str list, as a list display of literals.
fn strs<'py>(py: Python<'py>, items: &[&str]) -> PyResult<Obj<'py>> {
    Ok(PyList::new(py, items)?.into_any())
}

/// `(a, b)`.
fn pair<'py>(py: Python<'py>, a: &Obj<'py>, b: &Obj<'py>) -> PyResult<Obj<'py>> {
    Ok(PyTuple::new(py, [a, b])?.into_any())
}

/// `x.get(key)`.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// `a or b`.
fn or<'py>(a: Obj<'py>, b: impl FnOnce() -> PyResult<Obj<'py>>) -> PyResult<Obj<'py>> {
    if a.is_truthy()? {
        Ok(a)
    } else {
        b()
    }
}

/// `from AutoCua.ios_connector.session import <names>`, run when the line
/// ran: __import__ with this module's globals, then each name in turn.
fn from_session<'py>(py: Python<'py>, names: &[&str]) -> PyResult<Vec<Obj<'py>>> {
    let import = py.import("builtins")?.getattr("__import__")?;
    let fromlist = PyTuple::new(py, names)?;
    let session = import.call1((SESSION, module(py)?.dict(), py.None(), fromlist, 0))?;
    let mut found = Vec::with_capacity(names.len());
    for name in names {
        match session.getattr(*name) {
            Ok(value) => found.push(value),
            Err(e) if e.is_instance_of::<PyAttributeError>(py) => {
                let file = session
                    .getattr("__file__")
                    .and_then(|file| file.extract::<String>())
                    .unwrap_or_else(|_| "unknown location".to_string());
                return Err(PyImportError::new_err(format!("cannot import name '{name}' from '{SESSION}' ({file})")));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

/// `subprocess.run(argv, **kwargs)`, both looked up now.
fn run<'py>(py: Python<'py>, argv: &Obj<'py>, kwargs: &Bound<'py, PyDict>) -> PyResult<Obj<'py>> {
    name(py, "subprocess")?.getattr("run")?.call((argv,), Some(kwargs))
}

/// `capture_output=True, text=True, timeout=<seconds>`, after `first` when
/// given (`input=...`), in the order the calls wrote them.
fn captured<'py>(py: Python<'py>, first: Option<(&str, &Obj<'py>)>, timeout: i64) -> PyResult<Bound<'py, PyDict>> {
    let kwargs = PyDict::new(py);
    if let Some((key, value)) = first {
        kwargs.set_item(key, value)?;
    }
    kwargs.set_item("capture_output", true)?;
    kwargs.set_item("text", true)?;
    kwargs.set_item("timeout", timeout)?;
    Ok(kwargs)
}

/// `(stream or '').strip().splitlines()`.
fn tail_of<'py>(py: Python<'py>, stream: Obj<'py>) -> PyResult<Obj<'py>> {
    or(stream, || Ok(text(py, "")))?.call_method0("strip")?.call_method0("splitlines")
}

/// `f"{self.wda_url}<rest>"`, with the {value} parts of `rest` in order.
fn wda<'py>(slf: &Obj<'py>, rest: &[&Obj<'py>]) -> PyResult<Obj<'py>> {
    let py = slf.py();
    let base = slf.getattr("wda_url")?;
    let mut parts: Vec<&Obj<'py>> = vec![&base];
    parts.extend_from_slice(rest);
    fstring(py, &parts)
}

/// `requests.<verb>(url, json=..., timeout=...)`: requests.<verb> looked up
/// first, then the url made, as Python evaluates the call.
fn http<'py>(
    py: Python<'py>,
    verb: &str,
    url: impl FnOnce() -> PyResult<Obj<'py>>,
    json: Option<&Obj<'py>>,
    timeout: i64,
) -> PyResult<Obj<'py>> {
    let call = name(py, "requests")?.getattr(verb)?;
    let url = url()?;
    let kwargs = PyDict::new(py);
    if let Some(json) = json {
        kwargs.set_item("json", json)?;
    }
    kwargs.set_item("timeout", timeout)?;
    call.call((url,), Some(&kwargs))
}

/// `response.status_code == 200`.
fn ok_status(response: &Obj<'_>) -> PyResult<bool> {
    response.getattr("status_code")?.eq(200)
}

/// `except subprocess.TimeoutExpired:` then `except Exception as e:` of the
/// scans, both logging and giving False; anything else passes on.
fn scan_failed<'py>(py: Python<'py>, e: PyErr) -> PyResult<Obj<'py>> {
    let m = module(py)?;
    let _handling = Handling::new(py, &e);
    if e.matches(py, name(py, "subprocess")?.getattr("TimeoutExpired")?)? {
        log(&m, "error", || Ok(text(py, "❌ Timeout while scanning apps")))?;
        return Ok(boolean(py, false));
    }
    if is_exception(py, &e) {
        log(&m, "error", || fstring(py, &[&text(py, "❌ Error scanning apps: "), e.value(py)]))?;
        return Ok(boolean(py, false));
    }
    Err(e)
}

/// What the scans log when they found the apps.
fn log_found<'py>(slf: &Obj<'py>) -> PyResult<()> {
    let py = slf.py();
    log(&module(py)?, "info", || {
        let count = name(py, "len")?.call1((slf.getattr("apps_dict")?,))?;
        fstring(py, &[&text(py, "✅ Found "), &count, &text(py, " apps")])
    })
}

/// `logger.error` of a line whose last part is `tail[-1]` (stripped when
/// `strip`), or `fallback` when there is no line.
fn log_tail<'py>(py: Python<'py>, tail: &Obj<'py>, strip: bool, fallback: &str) -> PyResult<()> {
    log(&module(py)?, "error", || {
        let last = if tail.is_truthy()? {
            let last = tail.get_item(-1)?;
            if strip {
                last.call_method0("strip")?
            } else {
                last
            }
        } else {
            text(py, fallback)
        };
        fstring(py, &[&text(py, "❌ App scan failed: "), &last])
    })
}

fn unbound(variable: &str) -> PyErr {
    PyUnboundLocalError::new_err(format!(
        "cannot access local variable '{variable}' where it is not associated with a value"
    ))
}

// ---------------------------------------------------------------------------
// AppLauncherService
// ---------------------------------------------------------------------------

/// Service for launching and managing iPhone apps
#[pyclass(frozen, dict, weakref, subclass)]
pub struct AppLauncherService;

#[pymethods]
impl AppLauncherService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "()")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        AppLauncherService
    }

    #[pyo3(signature = (*args, **kwargs))]
    fn __init__(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
        let py = slf.py();
        only_self("AppLauncherService.__init__()", args, kwargs)?;
        let wda_url = from_session(py, &["wda_url"])?.remove(0);
        slf.setattr("wda_url", wda_url.call0()?)?;
        slf.setattr("session_id", py.None())?;
        slf.setattr("apps_dict", PyDict::new(py))?;
        Ok(())
    }

    /// Scan all installed apps using pymobiledevice3 (same CLI the WDA session
    /// uses), or devicectl for a phone on Wi-Fi only
    fn scan_apps<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let m = module(py)?;
        let scanned = (|| -> PyResult<Obj<'py>> {
            let found = from_session(py, &["_pmd3_base", "active_target", "plugged_in_udids"])?;
            let (pmd3_base, active_target, plugged_in_udids) = (&found[0], &found[1], &found[2]);

            // Simulator runs have no USB device: list apps via simctl instead.
            if get(active_target, "kind")?.eq("simulation")? {
                let simulator = slf.getattr("_scan_apps_simulator")?;
                let udid = or(get(active_target, "udid")?, || Ok(text(py, "booted")))?;
                return simulator.call1((udid,));
            }
            // A phone on Wi-Fi only (the app's Wireless toggle): pymobiledevice3
            // reaches a phone over the cable alone, Xcode's devicectl over both.
            let udid = get(active_target, "udid")?;
            let plugged = plugged_in_udids.call0()?;
            if udid.is_truthy()? && !plugged.is_none() && !plugged.contains(&udid)? {
                return slf.getattr("_scan_apps_devicectl")?.call1((&udid,));
            }
            let base = pmd3_base.call0()?;
            if !base.is_truthy()? {
                log(&m, "error", || Ok(text(py, "❌ Error: pymobiledevice3 not found.")))?;
                log(&m, "error", || Ok(text(py, "💡 Install it with: pip install pymobiledevice3")))?;
                return Ok(boolean(py, false));
            }

            log(&m, "info", || Ok(text(py, "📱 Scanning installed apps...")))?;
            let run = name(py, "subprocess")?.getattr("run")?;
            let argv = base.add(strs(py, &["apps", "list"])?)?;
            let kwargs = captured(py, None, 30)?;
            let env = PyDict::new(py);
            env.call_method1("update", (name(py, "os")?.getattr("environ")?,))?;
            env.set_item("NO_COLOR", "1")?;
            kwargs.set_item("env", env)?;
            let result = run.call((argv,), Some(&kwargs))?;
            // No phone it can reach: exit 0, nothing on stdout, the reason on stderr.
            if result.getattr("returncode")?.ne(0)? || !result.getattr("stdout")?.call_method0("strip")?.is_truthy()? {
                let tail = tail_of(py, result.getattr("stderr")?)?;
                log_tail(py, &tail, false, "pymobiledevice3 error")?;
                return Ok(boolean(py, false));
            }

            // Parse the apps
            slf.getattr("parse_apps")?.call1((result.getattr("stdout")?,))?;
            log_found(slf.as_any())?;
            Ok(boolean(py, true))
        })();
        match scanned {
            Ok(value) => Ok(value),
            Err(e) => {
                let handling = Handling::new(py, &e);
                if e.matches(py, name(py, "FileNotFoundError")?)? {
                    log(&m, "error", || Ok(text(py, "❌ Error: pymobiledevice3 not found.")))?;
                    log(&m, "error", || Ok(text(py, "💡 Install it with: pip install pymobiledevice3")))?;
                    return Ok(boolean(py, false));
                }
                drop(handling);
                scan_failed(py, e)
            }
        }
    }

    /// `simctl listapps` prints an old-style plist; plutil turns it into the
    /// same bundle-id-keyed JSON shape `pymobiledevice3 apps list` gives, so
    /// parse_apps handles both.
    fn _scan_apps_simulator<'py>(slf: &Bound<'py, Self>, udid: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let m = module(py)?;
        let scanned = (|| -> PyResult<Obj<'py>> {
            log(&m, "info", || Ok(text(py, "📱 Scanning installed apps (simulator)...")))?;
            let argv = PyList::new(py, [text(py, "xcrun"), text(py, "simctl"), text(py, "listapps"), udid.clone()])?;
            let listed = run(py, argv.as_any(), &captured(py, None, 30)?)?;
            if listed.getattr("returncode")?.ne(0)? {
                let tail = tail_of(py, listed.getattr("stderr")?)?;
                log_tail(py, &tail, false, "simctl error")?;
                return Ok(boolean(py, false));
            }
            let run_json = name(py, "subprocess")?.getattr("run")?;
            let argv = strs(py, &["plutil", "-convert", "json", "-o", "-", "--", "-"])?;
            let stdout = listed.getattr("stdout")?;
            let as_json = run_json.call((argv,), Some(&captured(py, Some(("input", &stdout)), 15)?))?;
            if as_json.getattr("returncode")?.ne(0)? {
                log(&m, "error", || Ok(text(py, "❌ App scan failed: could not parse simctl output")))?;
                return Ok(boolean(py, false));
            }
            slf.getattr("parse_apps")?.call1((as_json.getattr("stdout")?,))?;
            log_found(slf.as_any())?;
            Ok(boolean(py, true))
        })();
        match scanned {
            Ok(value) => Ok(value),
            Err(e) => scan_failed(py, e),
        }
    }

    /// `devicectl device info apps` writes its own JSON; it is turned into
    /// the same bundle-id-keyed shape `pymobiledevice3 apps list` gives, so
    /// parse_apps handles both.
    fn _scan_apps_devicectl<'py>(slf: &Bound<'py, Self>, udid: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let m = module(py)?;
        let scanned = (|| -> PyResult<Obj<'py>> {
            let devicectl = from_session(py, &["devicectl"])?.remove(0);
            log(&m, "info", || Ok(text(py, "📱 Scanning installed apps (Wi-Fi)...")))?;
            // The two names the block sets; None while it has not set them.
            let mut listed: Option<Obj<'py>> = None;
            let mut apps: Option<Obj<'py>> = None;
            let manager = name(py, "tempfile")?.getattr("TemporaryDirectory")?.call0()?;
            with(&manager, |d| {
                let out = name(py, "Path")?.call1((d,))?.div("apps.json")?;
                let run = name(py, "subprocess")?.getattr("run")?;
                let prefix = devicectl.call0()?;
                let rest = PyList::new(
                    py,
                    [
                        text(py, "device"),
                        text(py, "info"),
                        text(py, "apps"),
                        text(py, "--device"),
                        udid.clone(),
                        text(py, "--include-all-apps"),
                        text(py, "--json-output"),
                        name(py, "str")?.call1((&out,))?,
                    ],
                )?;
                let argv = prefix.add(rest)?;
                listed = Some(run.call((argv,), Some(&captured(py, None, 30)?))?);
                apps = Some(if out.call_method0("exists")?.is_truthy()? {
                    let loads = name(py, "json")?.getattr("loads")?;
                    let loaded = loads.call1((out.call_method0("read_text")?,))?;
                    let result = or(get(&loaded, "result")?, || Ok(PyDict::new(py).into_any()))?;
                    get(&result, "apps")?
                } else {
                    none(py)
                });
                Ok(())
            })?;
            let listed = listed.ok_or_else(|| unbound("listed"))?;
            let failed = listed.getattr("returncode")?.ne(0)?
                || match &apps {
                    Some(apps) => apps.is_none(),
                    None => return Err(unbound("apps")),
                };
            if failed {
                let tail = tail_of(py, listed.getattr("stderr")?)?;
                log_tail(py, &tail, true, "devicectl error")?;
                return Ok(boolean(py, false));
            }
            let apps = apps.ok_or_else(|| unbound("apps"))?;
            let parse_apps = slf.getattr("parse_apps")?;
            let dumps = name(py, "json")?.getattr("dumps")?;
            let shaped = PyDict::new(py);
            for a in apps.try_iter()? {
                let a = a?;
                if !get(&a, "bundleIdentifier")?.is_truthy()? {
                    continue;
                }
                let key = a.get_item("bundleIdentifier")?;
                let display_name = get(&a, "name")?;
                let version = or(get(&a, "version")?, || Ok(text(py, "")))?;
                let bundle_version = or(get(&a, "bundleVersion")?, || Ok(text(py, "")))?;
                let tags = if get(&a, "hidden")?.is_truthy()? { strs(py, &["hidden"])? } else { strs(py, &[])? };
                let value = dict(
                    py,
                    &[
                        ("CFBundleDisplayName", &display_name),
                        ("CFBundleShortVersionString", &version),
                        ("CFBundleVersion", &bundle_version),
                        ("SBAppTags", &tags),
                    ],
                )?;
                shaped.set_item(key, value)?;
            }
            parse_apps.call1((dumps.call1((shaped,))?,))?;
            log_found(slf.as_any())?;
            Ok(boolean(py, true))
        })();
        match scanned {
            Ok(value) => Ok(value),
            Err(e) => scan_failed(py, e),
        }
    }

    /// Parse `pymobiledevice3 apps list` JSON into searchable dictionary
    fn parse_apps(slf: &Bound<'_, Self>, apps_output: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = slf.py();
        slf.setattr("apps_dict", PyDict::new(py))?;

        let loads = name(py, "json")?.getattr("loads")?;
        for item in loads.call1((apps_output,))?.call_method0("items")?.try_iter()? {
            let (bundle_id, info) = unpack2(&item?)?;
            // Hidden bundles (SafariViewService, MessagesViewService, CarPlay
            // settings, ...) have no home-screen icon, share a display name
            // with the real app, and never show a screen when launched. The
            // model can't see them, so it can never mean them.
            if or(get(&info, "SBAppTags")?, || Ok(PyList::empty(py).into_any()))?.contains("hidden")? {
                continue;
            }
            let display_name = or(get(&info, "CFBundleDisplayName")?, || {
                or(get(&info, "CFBundleName")?, || Ok(bundle_id.clone()))
            })?;
            let version = or(get(&info, "CFBundleShortVersionString")?, || {
                let str_ = name(py, "str")?;
                str_.call1((info.call_method1("get", ("CFBundleVersion", ""))?,))
            })?;

            // Keyed by bundle id: two bundles with the same display name (a dev
            // build and prod, an app and its App Clip) must both survive so
            // resolve_app can report the name as ambiguous instead of silently
            // launching whichever the scan listed last.
            let entry = dict(py, &[("bundle_id", &bundle_id), ("display_name", &display_name), ("version", &version)])?;
            slf.getattr("apps_dict")?.set_item(&bundle_id, entry)?;
        }
        Ok(())
    }

    #[staticmethod]
    fn _norm<'py>(py: Python<'py>, text: &Bound<'py, PyAny>) -> PyResult<Obj<'py>> {
        let str_ = name(py, "str")?;
        let value = or(text.clone(), || Ok(crate::ios::controller::text(py, "")))?;
        str_.call1((value,))?.call_method0("lower")?.call_method0("strip")?.call_method1("replace", (" ", ""))
    }

    /// Match tier of one app for a name (0 = no match). Single source of
    /// truth for search_app and resolve_app. An exact bundle id ranks with an
    /// exact display name so 'com.foo.app' picks prod even when
    /// 'com.foo.app.dev' exists with the same display name.
    fn _score<'py>(slf: &Bound<'py, Self>, app_info: &Obj<'py>, search_term: &Obj<'py>) -> PyResult<Obj<'py>> {
        let term = slf.getattr("_norm")?.call1((search_term,))?;
        let name_ = slf.getattr("_norm")?.call1((app_info.get_item("display_name")?,))?;
        let bundle = app_info.get_item("bundle_id")?.call_method0("lower")?;
        if name_.eq(&term)? || bundle.eq(&term)? {
            return slf.getattr("_EXACT");
        }
        if name_.call_method1("startswith", (&term,))?.is_truthy()? {
            return slf.getattr("_PREFIX");
        }
        if name_.contains(&term)? {
            return slf.getattr("_IN_NAME");
        }
        if bundle.contains(&term)? {
            return slf.getattr("_IN_BUNDLE");
        }
        int(slf.py(), 0)
    }

    /// Rank installed apps against a name: exact display name / bundle id,
    /// then name prefix, then substring of the name, then substring of the
    /// bundle id. Returns app_info dicts best-first (ties: shorter name first).
    fn search_app<'py>(slf: &Bound<'py, Self>, search_term: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let term = slf.getattr("_norm")?.call1((search_term,))?;
        if !term.is_truthy()? {
            return Ok(PyList::empty(py).into_any());
        }
        let ranked = PyList::empty(py);
        for app_info in slf.getattr("apps_dict")?.call_method0("values")?.try_iter()? {
            let app_info = app_info?;
            let score = slf.getattr("_score")?.call1((&app_info, &term))?;
            if score.is_truthy()? {
                let len = name(py, "len")?;
                let length = len.call1((slf.getattr("_norm")?.call1((app_info.get_item("display_name")?,))?,))?;
                ranked.append(PyTuple::new(py, [score, length, app_info])?)?;
            }
        }
        // ranked.sort(key=lambda r: (-r[0], r[1]))
        let key = PyCFunction::new_closure(py, None, None, |args, _kwargs| -> PyResult<Py<PyTuple>> {
            let r = args.get_item(0)?;
            let first = r.get_item(0)?.neg()?;
            let second = r.get_item(1)?;
            Ok(PyTuple::new(args.py(), [first, second])?.unbind())
        })?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("key", key)?;
        ranked.call_method("sort", (), Some(&kwargs))?;
        let best_first = PyList::empty(py);
        for item in ranked.iter() {
            best_first.append(item.get_item(2)?)?;
        }
        Ok(best_first.into_any())
    }

    /// Pick the ONE app a name refers to, or explain why it can't.
    /// Returns (app_info, None) on a confident match, (None, message) when the
    /// name is empty, unknown, or ambiguous - the message names the closest
    /// installed apps so the caller can re-issue with an exact name instead
    /// of the launcher guessing.
    fn resolve_app<'py>(slf: &Bound<'py, Self>, search_term: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let nothing = none(py);
        let str_ = name(py, "str")?;
        let term = str_.call1((or(search_term.clone(), || Ok(text(py, "")))?,))?.call_method0("strip")?;
        if !term.is_truthy()? {
            let why = text(py, "open_app needs an app name - re-issue with the exact name shown on the home screen.");
            return pair(py, &nothing, &why);
        }
        if !slf.getattr("apps_dict")?.is_truthy()?
            && (!slf.getattr("scan_apps")?.call0()?.is_truthy()? || !slf.getattr("apps_dict")?.is_truthy()?)
        {
            // Infrastructure failure, NOT "not installed" - never steer the
            // model into the App Store for an app that is on the home screen.
            let why = text(
                py,
                "Could not read the installed app list from the device - \
                 open the app from its icon on the home screen instead.",
            );
            return pair(py, &nothing, &why);
        }
        let mut ranked = slf.getattr("search_app")?.call1((&term,))?;
        if !ranked.is_truthy()? && slf.getattr("scan_apps")?.call0()?.is_truthy()? {
            // The list is cached per process; the app may have been installed
            // since (the model was told to download it). One rescan, then decide.
            ranked = slf.getattr("search_app")?.call1((&term,))?;
        }
        if !ranked.is_truthy()? {
            let norm = term.call_method0("lower")?.call_method1("replace", (" ", ""))?;
            let by_norm = PyDict::new(py);
            for i in slf.getattr("apps_dict")?.call_method0("values")?.try_iter()? {
                let i = i?;
                let key = i.get_item("display_name")?.call_method0("lower")?.call_method1("replace", (" ", ""))?;
                by_norm.set_item(key, i.get_item("display_name")?)?;
            }
            let close_matches = name(py, "difflib")?.getattr("get_close_matches")?;
            let names = name(py, "list")?.call1((&by_norm,))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("n", 3)?;
            kwargs.set_item("cutoff", 0.6)?;
            let close = PyList::empty(py);
            for k in close_matches.call((&norm, names), Some(&kwargs))?.try_iter()? {
                close.append(by_norm.as_any().get_item(k?)?)?;
            }
            let hint = if close.is_truthy()? {
                let joined = text(py, ", ").call_method1("join", (&close,))?;
                fstring(py, &[&text(py, " Closest installed: "), &joined, &text(py, ".")])?
            } else {
                text(py, "")
            };
            let why = fstring(
                py,
                &[
                    &text(py, "No installed app matches '"),
                    &term,
                    &text(py, "'."),
                    &hint,
                    &text(
                        py,
                        " Verify the name from the home screen; if it is not installed, \
                         download it or find an alternative.",
                    ),
                ],
            )?;
            return pair(py, &nothing, &why);
        }
        let best = ranked.get_item(0)?;
        let best_score = slf.getattr("_score")?.call1((&best, &term))?;
        let peers = PyList::empty(py);
        for i in ranked.try_iter()? {
            let i = i?;
            if slf.getattr("_score")?.call1((&i, &term))?.eq(&best_score)? {
                peers.append(i)?;
            }
        }
        // A unique best at any tier is decisive (an exact name beats weaker
        // peers by construction). Several equally-good matches: the model
        // chooses, not the launcher. Same-name bundles are told apart by id.
        if name(py, "len")?.call1((&peers,))?.eq(1)? {
            return pair(py, &best, &nothing);
        }
        let first_three = peers.get_slice(0, 3);
        if best_score.eq(slf.getattr("_EXACT")?)? {
            let named = PyList::empty(py);
            for i in first_three.iter() {
                named.append(fstring(
                    py,
                    &[&i.get_item("display_name")?, &text(py, " ("), &i.get_item("bundle_id")?, &text(py, ")")],
                )?)?;
            }
            let options = text(py, ", ").call_method1("join", (named,))?;
            let count = name(py, "len")?.call1((&peers,))?;
            let why = fstring(
                py,
                &[
                    &text(py, "'"),
                    &term,
                    &text(py, "' names "),
                    &count,
                    &text(py, " installed apps: "),
                    &options,
                    &text(py, ". Re-issue open_app with the bundle id."),
                ],
            )?;
            return pair(py, &nothing, &why);
        }
        let named = PyList::empty(py);
        for i in first_three.iter() {
            named.append(i.get_item("display_name")?)?;
        }
        let options = text(py, ", ").call_method1("join", (named,))?;
        let why = fstring(
            py,
            &[
                &text(py, "'"),
                &term,
                &text(py, "' is ambiguous - it matches: "),
                &options,
                &text(py, ". Re-issue open_app with the exact app name."),
            ],
        )?;
        pair(py, &nothing, &why)
    }

    /// Get or create WDA session
    fn get_session<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let attempt = (|| -> PyResult<Option<Obj<'py>>> {
            // Try to create new session
            let post = name(py, "requests")?.getattr("post")?;
            let url = wda(slf.as_any(), &[&text(py, "/session")])?;
            let always_match = dict(py, &[("alwaysMatch", &PyDict::new(py).into_any())])?.into_any();
            let capabilities = dict(py, &[("capabilities", &always_match)])?.into_any();
            let kwargs = dict(py, &[("json", &capabilities), ("timeout", &int(py, 10)?)])?;
            let response = post.call((url,), Some(&kwargs))?;
            if ok_status(&response)? {
                let data = response.call_method0("json")?;
                if data.contains("sessionId")? {
                    slf.setattr("session_id", data.get_item("sessionId")?)?;
                } else if data.contains("value")? && data.get_item("value")?.contains("sessionId")? {
                    slf.setattr("session_id", data.get_item("value")?.get_item("sessionId")?)?;
                }
                return Ok(Some(slf.getattr("session_id")?));
            }
            Ok(None)
        })();
        match attempt {
            Ok(Some(session)) => Ok(session),
            Ok(None) => Ok(none(py)),
            Err(e) => {
                log_failure(&module(py)?, "Session error: ", e)?;
                Ok(none(py))
            }
        }
    }

    /// Resolve a name to ONE installed app, launch it, and confirm it is
    /// the foreground app. Returns a dict the controller reports verbatim:
    ///   {"ok": True,  "display_name", "bundle_id", "message"}
    ///   {"ok": False, "display_name"?, "bundle_id"?, "message"}
    /// `ok` is True ONLY when the target is confirmed in the foreground (or
    /// the foreground could not be read - then `verified` is False).
    fn launch_app<'py>(slf: &Bound<'py, Self>, app_name: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let (app_info, why) = unpack2(&slf.getattr("resolve_app")?.call1((app_name,))?)?;
        if app_info.is_none() {
            log(&module(py)?, "error", || Ok(why.clone()))?;
            return Ok(dict(py, &[("ok", &boolean(py, false)), ("message", &why)])?.into_any());
        }
        let do_launch = slf.getattr("_do_launch")?;
        let result = do_launch.call1((app_info.get_item("bundle_id")?, app_info.get_item("display_name")?))?;
        let update = result.getattr("update")?;
        let display_name = app_info.get_item("display_name")?;
        let bundle_id = app_info.get_item("bundle_id")?;
        update.call1((dict(py, &[("display_name", &display_name), ("bundle_id", &bundle_id)])?,))?;
        Ok(result)
    }

    /// Foreground app via WDA's /wda/activeAppInfo -> {"bundleId", "pid",
    /// "name"} or None when the endpoint can't be read.
    fn active_app<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let attempt = (|| -> PyResult<Option<Obj<'py>>> {
            let url = || wda(slf.as_any(), &[&text(py, "/wda/activeAppInfo")]);
            let get_ = name(py, "requests")?.getattr("get")?;
            let response = get_.call((url()?,), Some(&dict(py, &[("timeout", &int(py, 5)?)])?))?;
            if ok_status(&response)? {
                let value = or(get(&response.call_method0("json")?, "value")?, || Ok(PyDict::new(py).into_any()))?;
                if value.is_instance_of::<PyDict>() && get(&value, "bundleId")?.is_truthy()? {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        })();
        match attempt {
            Ok(found) => Ok(found.unwrap_or_else(|| none(py))),
            Err(e) => {
                log_failure(&module(py)?, "activeAppInfo failed: ", e)?;
                Ok(none(py))
            }
        }
    }

    /// POST /session/{s}/wda/apps/state -> XCUIApplicationState int, or
    /// None when it can't be read. State 4 stays 4 while a system alert
    /// (notifications / Face ID / tracking prompt) is overlaid on the app,
    /// which is exactly where activeAppInfo reports SpringBoard instead.
    fn app_state<'py>(slf: &Bound<'py, Self>, session: &Obj<'py>, bundle_id: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let attempt = (|| -> PyResult<Option<Obj<'py>>> {
            let post = name(py, "requests")?.getattr("post")?;
            let url = wda(slf.as_any(), &[&text(py, "/session/"), session, &text(py, "/wda/apps/state")])?;
            let payload = dict(py, &[("bundleId", bundle_id)])?.into_any();
            let response = post.call((url,), Some(&dict(py, &[("json", &payload), ("timeout", &int(py, 5)?)])?))?;
            if ok_status(&response)? {
                let value = get(&response.call_method0("json")?, "value")?;
                if value.is_instance_of::<PyInt>() && !value.is_instance_of::<PyBool>() {
                    return Ok(Some(value));
                }
            }
            Ok(None)
        })();
        match attempt {
            Ok(found) => Ok(found.unwrap_or_else(|| none(py))),
            Err(e) => {
                log_failure(&module(py)?, "apps/state failed: ", e)?;
                Ok(none(py))
            }
        }
    }

    /// Launch via WDA, then VERIFY. A 200 from the launch endpoint only
    /// means WDA accepted the request - it also returns 200 for background-
    /// only system bundles that never show a screen. Success = the target's
    /// process state is running-foreground (primary), or it is the active
    /// app (fallback when apps/state can't be read), within `settle_seconds`.
    #[pyo3(signature = (bundle_id, app_name, settle_seconds = None),
           text_signature = "($self, bundle_id, app_name, settle_seconds=3.0)")]
    fn _do_launch<'py>(
        slf: &Bound<'py, Self>,
        bundle_id: &Obj<'py>,
        app_name: &Obj<'py>,
        settle_seconds: Option<Obj<'py>>,
    ) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let m = module(py)?;
        let mut settle_seconds = match settle_seconds {
            Some(given) => given,
            None => float(py, 3.0)?,
        };
        let say = |level: &str, parts: &[&Obj<'py>]| log(&m, level, || fstring(py, parts));
        say("info", &[&text(py, "🚀 Launching "), app_name, &text(py, "...")])?;

        let session = slf.getattr("get_session")?.call0()?;
        if !session.is_truthy()? {
            log(&m, "error", || Ok(text(py, "Failed to create WDA session")))?;
            let message = text(py, "Could not open a WDA session to launch the app.");
            return Ok(dict(py, &[("ok", &boolean(py, false)), ("message", &message)])?.into_any());
        }

        let before = slf.getattr("active_app")?.call0()?;
        let before_bundle = if before.is_truthy()? { get(&before, "bundleId")? } else { none(py) };

        let endpoints = [
            wda(slf.as_any(), &[&text(py, "/wda/apps/launch")])?,
            wda(slf.as_any(), &[&text(py, "/session/"), &session, &text(py, "/wda/apps/launch")])?,
            wda(slf.as_any(), &[&text(py, "/wda/apps/activate")])?,
            wda(slf.as_any(), &[&text(py, "/session/"), &session, &text(py, "/wda/apps/activate")])?,
        ];
        // Don't wait for the app to go idle: a splash animation would hold the
        // request past the client timeout while the app is already on screen.
        let payload = dict(py, &[("bundleId", bundle_id), ("shouldWaitForQuiescence", &boolean(py, false))])?.into_any();
        let mut accepted = false;
        let mut timed_out = false;
        for endpoint in &endpoints {
            match http(py, "post", || Ok(endpoint.clone()), Some(&payload), 10).and_then(|response| ok_status(&response)) {
                Ok(true) => {
                    accepted = true;
                    break;
                }
                Ok(false) => {}
                Err(e) => {
                    let _handling = Handling::new(py, &e);
                    if e.matches(py, name(py, "requests")?.getattr("Timeout")?)? {
                        // The launch is still in flight inside WDA; further endpoints
                        // would only queue behind it. Let the verification poll decide.
                        timed_out = true;
                        break;
                    }
                    if !is_exception(py, &e) {
                        return Err(e);
                    }
                }
            }
        }
        if !accepted && !timed_out {
            say("error", &[&text(py, "Failed to launch "), app_name])?;
            let message = fstring(py, &[&text(py, "WDA could not launch "), app_name, &text(py, " ("), bundle_id, &text(py, ").")])?;
            return Ok(dict(py, &[("ok", &boolean(py, false)), ("message", &message)])?.into_any());
        }
        if timed_out {
            settle_seconds = name(py, "max")?.call1((&settle_seconds, 10.0))?;
        }

        let deadline = name(py, "time")?.getattr("monotonic")?.call0()?.add(&settle_seconds)?;
        // last successful readings, not last attempts
        let mut state = none(py);
        let mut fg = none(py);
        loop {
            let s = slf.getattr("app_state")?.call1((&session, bundle_id))?;
            if !s.is_none() {
                state = s;
            }
            if state.eq(slf.getattr("_FOREGROUND")?)? {
                say("info", &[&text(py, "✅ "), app_name, &text(py, " is in the foreground")])?;
                let message = fstring(py, &[app_name, &text(py, " is open in the foreground.")])?;
                let yes = boolean(py, true);
                return Ok(dict(py, &[("ok", &yes), ("verified", &yes), ("message", &message)])?.into_any());
            }
            let active = slf.getattr("active_app")?.call0()?;
            if active.is_truthy()? {
                fg = get(&active, "bundleId")?;
            }
            if state.is_none() && fg.eq(bundle_id)? {
                say("info", &[&text(py, "✅ "), app_name, &text(py, " is the active app")])?;
                let message = fstring(py, &[app_name, &text(py, " is open in the foreground.")])?;
                let yes = boolean(py, true);
                return Ok(dict(py, &[("ok", &yes), ("verified", &yes), ("message", &message)])?.into_any());
            }
            if name(py, "time")?.getattr("monotonic")?.call0()?.ge(&deadline)? {
                break;
            }
            name(py, "time")?.getattr("sleep")?.call1((0.5,))?;
        }

        if !state.is_none() {
            say("error", &[&text(py, "Launch of "), app_name, &text(py, " accepted but its state is "), &state])?;
            let described = slf.getattr("_STATE")?.call_method1("get", (&state, "in an unknown state"))?;
            let message = fstring(
                py,
                &[
                    app_name,
                    &text(py, " ("),
                    bundle_id,
                    &text(py, ") did not come to the foreground - it is "),
                    &described,
                    &text(py, ". Tap its icon on the home screen instead."),
                ],
            )?;
            return Ok(dict(py, &[("ok", &boolean(py, false)), ("message", &message)])?.into_any());
        }
        if fg.is_none() {
            say("warning", &[&text(py, "Launch of "), app_name, &text(py, " accepted but could not be verified")])?;
            let message = fstring(
                py,
                &[
                    &text(py, "WDA accepted the launch of "),
                    app_name,
                    &text(py, ", but its state could not be read - confirm on the next screenshot."),
                ],
            )?;
            let row = [("ok", &boolean(py, true)), ("verified", &boolean(py, false)), ("message", &message)];
            return Ok(dict(py, &row)?.into_any());
        }
        if fg.ne(&before_bundle)? {
            // Something came in front that isn't the target - typically a
            // system dialog over the freshly launched app. Not a failure.
            say("warning", &[&text(py, "Launch of "), app_name, &text(py, " accepted; foreground is now "), &fg])?;
            let message = fstring(
                py,
                &[
                    &text(py, "Launch of "),
                    app_name,
                    &text(py, " accepted; the foreground is now "),
                    &fg,
                    &text(py, " - a system dialog may be in front. Confirm on the next screenshot."),
                ],
            )?;
            let row = [("ok", &boolean(py, true)), ("verified", &boolean(py, false)), ("message", &message)];
            return Ok(dict(py, &row)?.into_any());
        }
        say("error", &[&text(py, "Launch of "), app_name, &text(py, " accepted but foreground is unchanged ("), &fg, &text(py, ")")])?;
        let message = fstring(
            py,
            &[
                &text(py, "Launch of "),
                app_name,
                &text(py, " ("),
                bundle_id,
                &text(py, ") was accepted but nothing came to the foreground - active app is still "),
                &fg,
                &text(py, ". Tap its icon on the home screen instead."),
            ],
        )?;
        Ok(dict(py, &[("ok", &boolean(py, false)), ("message", &message)])?.into_any())
    }

    /// Go to home screen
    fn go_home(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let attempt = (|| -> PyResult<bool> {
            let m = module(py)?;
            let home = || wda(slf.as_any(), &[&text(py, "/wda/homescreen")]);
            let response = http(py, "post", home, None, 5)?;
            if ok_status(&response)? {
                log(&m, "info", || Ok(text(py, "Navigated to home screen")))?;
                return Ok(true);
            }

            if slf.getattr("session_id")?.is_truthy()? {
                let press = || {
                    let session_id = slf.getattr("session_id")?;
                    wda(slf.as_any(), &[&text(py, "/session/"), &session_id, &text(py, "/wda/pressButton")])
                };
                let payload = dict(py, &[("name", &text(py, "home"))])?.into_any();
                let response = http(py, "post", press, Some(&payload), 5)?;
                if ok_status(&response)? {
                    log(&m, "info", || Ok(text(py, "Navigated to home screen")))?;
                    return Ok(true);
                }
            }
            Ok(false)
        })();
        match attempt {
            Ok(done) => Ok(done),
            Err(e) => {
                log_failure(&module(py)?, "Go home failed: ", e)?;
                Ok(false)
            }
        }
    }

    /// Test connection to WDA
    fn test_wda_connection(slf: &Bound<'_, Self>) -> bool {
        let py = slf.py();
        // A bare `except:` in the .py: anything at all, KeyboardInterrupt too.
        (|| -> PyResult<bool> {
            let status = || wda(slf.as_any(), &[&text(py, "/status")]);
            ok_status(&http(py, "get", status, None, 5)?)
        })()
        .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes open_app.py's place with its docstring and
/// the class, with the class's own constants. tool/__init__.py binds the
/// rest when the module is first imported (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    m.add_class::<AppLauncherService>()?;
    let class = py.get_type::<AppLauncherService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    // Match quality, best first. An exact display-name match must beat an
    // accidental substring hit: "now" is inside "NanoWebSheet" (a hidden Apple
    // system sheet), and dictionary order used to make THAT the launch target
    // instead of the app literally named "NOW".
    class.setattr("_EXACT", 4)?;
    class.setattr("_PREFIX", 3)?;
    class.setattr("_IN_NAME", 2)?;
    class.setattr("_IN_BUNDLE", 1)?;
    // XCUIApplicationState, as returned by WDA's /wda/apps/state.
    let states = PyDict::new(py);
    for (state, words) in [
        (0, "in an unknown state"),
        (1, "not running"),
        (2, "suspended in the background"),
        (3, "running in the background"),
        (4, "in the foreground"),
    ] {
        states.set_item(state, words)?;
    }
    class.setattr("_STATE", states)?;
    class.setattr("_FOREGROUND", 4)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
