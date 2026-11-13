//! The view: the iPhone agent's router. One model step's actions (click,
//! ground_click, ground_scroll, input, scroll, open_app, wait, web, shell,
//! vault, video_player, todo_list, update_todo, scratchpad, done) go, in
//! order, to the controller service, the tools, the todo list, the
//! scratchpad and the vault, and what they did comes back as one reply.
//!
//! The Rust copy of controller/view.py. Python imports it under its old
//! dotted name, AutoCua.ios.controller.view (controller/__init__.py points
//! that name here), and gets the same class, methods, attributes, replies and
//! log records. Python's own rules decide every result, so they are called,
//! not imitated: int(), float(), str(), len(), min(), isinstance(), the str
//! and dict methods, `in`, `is`, the comparisons, the {**base, ...} merges
//! and the f-strings.
//!
//! The names view.py imported (logging, time, sys, threading,
//! controller_service, TaskTrackerService, app_launcher_service,
//! VideoPlayerService, vault_service, MAX_PIN_ATTEMPTS, DEVICE_ENTRIES,
//! TEXT_FIELDS, ScratchpadService, ShellService, WebService) and its logger
//! are bound on this module by controller/__init__.py when the module is
//! first imported, as the .py did then. Loading the extension imports none
//! of them. Each is read from this module each time it is used, so a caller
//! or a test that replaces one is obeyed.
//!
//! The instance keeps its attributes in its __dict__, in the order __init__
//! sets them, as a Python instance does, and the methods reach them and each
//! other through the instance, so a subclass or a caller that replaces one
//! is obeyed.

use pyo3::exceptions::{PyAttributeError, PyTypeError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyList, PyTuple};

use crate::computer::{as_python_class, type_name, unpack2, Handling};
use crate::ios::controller::{self, fstring, log, text, throw};
use crate::ios::NATIVE;

type Obj<'py> = Bound<'py, PyAny>;

/// What one action did to the batch: None goes on to the next action,
/// Some(reply) ends route_action with that reply.
type Step<'py> = PyResult<Option<Obj<'py>>>;

/// A dict display, `{"key": value, ...}`: a new dict with the keys in the
/// order written. A value is anything PyO3 turns into a Python object (a
/// &str is a str, a bool a bool).
macro_rules! display {
    ($py:expr, {$($key:literal: $value:expr),* $(,)?}) => {{
        let built = PyDict::new($py);
        $(built.set_item($key, $value)?;)*
        built
    }};
}

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.controller.view")).map(Bound::unbind))
        .map(|m| m.bind(py).clone())
}

/// One of this module's globals (or a builtin), read now (lib.rs's
/// controller::name: a missing one is NameError, naming the name).
fn name<'py>(py: Python<'py>, wanted: &str) -> PyResult<Obj<'py>> {
    controller::name(&module(py)?, wanted)
}

fn none(py: Python<'_>) -> Obj<'_> {
    py.None().into_bound(py)
}

fn boolean(py: Python<'_>, value: bool) -> Obj<'_> {
    PyBool::new(py, value).to_owned().into_any()
}

/// `value.get(key)`.
fn get<'py>(value: &Obj<'py>, key: &str) -> PyResult<Obj<'py>> {
    value.call_method1("get", (key,))
}

/// `value or ""`.
fn or_empty(value: Obj<'_>) -> PyResult<Obj<'_>> {
    let py = value.py();
    Ok(if value.is_truthy()? { value } else { text(py, "") })
}

/// `logger.<level>(f"...")`, the parts given in order.
fn say<'py>(py: Python<'py>, level: &str, parts: &[&Obj<'py>]) -> PyResult<()> {
    log(&module(py)?, level, || fstring(py, parts))
}

/// `str(e)`, with the str of this module's globals.
fn str_of<'py>(py: Python<'py>, e: &PyErr) -> PyResult<Obj<'py>> {
    name(py, "str")?.call1((e.value(py),))
}

/// `logger.error(f"<what>{str(e)}")`: logger.error is looked up first, then
/// str(e) is made, as Python evaluates the call.
fn log_error<'py>(py: Python<'py>, what: &str, e: &PyErr) -> PyResult<()> {
    log(&module(py)?, "error", || fstring(py, &[&text(py, what), &str_of(py, e)?]))
}

/// Runs `body` while `e` (when there is one) is the exception being
/// handled, as an except or a finally block runs. An error the body raises
/// is made into its exception before `e` stops being handled, so one PyO3
/// makes lazily (the NameError of a missing global) gets `e` as its
/// __context__ too, as any raise inside the block does in Python.
fn handling<'py, T>(py: Python<'py>, e: Option<&PyErr>, body: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let _handling = e.map(|e| Handling::new(py, e));
    let done = body();
    if let Err(raised) = &done {
        raised.value(py);
    }
    done
}

/// `try: <tried> except Exception as e: <handler>`. The handler runs while
/// `e` is the exception being handled, so one it raises carries `e` as its
/// __context__. Exception is read from this module, as Python reads it
/// there, and what does not match it (KeyboardInterrupt, SystemExit)
/// passes on untouched (lib.rs's controller::except_exception).
fn except_exception<'py, T>(
    py: Python<'py>,
    tried: PyResult<T>,
    handler: impl FnOnce(&PyErr) -> PyResult<T>,
) -> PyResult<T> {
    match tried {
        Ok(value) => Ok(value),
        Err(e) => controller::except_exception(&module(py)?, e, handler),
    }
}

/// `isinstance(value, dict)`, both names read now (lib.rs's
/// controller::isinstance).
fn is_dict(value: &Obj<'_>) -> PyResult<bool> {
    controller::isinstance(&module(value.py())?, value, "dict")
}

/// `self.stop_event and self.stop_event.is_set()`, as an `if` tests it.
fn stop_requested(slf: &Obj<'_>) -> PyResult<bool> {
    Ok(slf.getattr("stop_event")?.is_truthy()? && slf.getattr("stop_event")?.call_method0("is_set")?.is_truthy()?)
}

/// The reply to a stop.
fn stopped(py: Python<'_>) -> PyResult<Obj<'_>> {
    Ok(display!(py, {"status": "stopped", "action": "stop", "message": "Stopped by user"}).into_any())
}

/// `{**a, **b, ...}`: an empty dict, then each part merged in, as the
/// interpreter merges a `**` part (DICT_UPDATE): PyDict_Update, and a part
/// that is not a mapping refused in its words.
fn merged<'py>(py: Python<'py>, parts: &[&Obj<'py>]) -> PyResult<Bound<'py, PyDict>> {
    let built = PyDict::new(py);
    for part in parts {
        // SAFETY: two valid objects, with the GIL held.
        if unsafe { ffi::PyDict_Update(built.as_ptr(), part.as_ptr()) } < 0 {
            let e = PyErr::fetch(py);
            if !e.is_instance_of::<PyAttributeError>(py) {
                return Err(e);
            }
            // "'%.200s' object is not a mapping": at most 200 bytes of the name.
            let mut kind = type_name(part)?;
            if kind.len() > 200 {
                let mut end = 200;
                while !kind.is_char_boundary(end) {
                    end -= 1;
                }
                kind.truncate(end);
            }
            return Err(PyTypeError::new_err(format!("'{kind}' object is not a mapping")));
        }
    }
    Ok(built)
}

// ---------------------------------------------------------------------------
// The actions of route_action, one function each, in view.py's order
// ---------------------------------------------------------------------------

/// `results.append(result)`, then `if result.get("status") == "error":
/// return result`.
fn kept_or_returned<'py>(result: Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    results.append(&result)?;
    if get(&result, "status")?.eq("error")? {
        return Ok(Some(result));
    }
    Ok(None)
}

fn click<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let element_id = get(action_item, "id")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        let element_num = name(py, "int")?.call1((&element_id,))?;
        slf.getattr("controller_service")?.call_method1("click", (&element_num,))?;
        say(py, "info", &[&text(py, "✓ Clicked element "), &element_num])?;
        Ok(display!(py, {"status": "success", "action": "click", "element": &element_num}).into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "✗ Click failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "click",
            "element": &element_id,
            "message": &str_of(py, e)?,
        })
        .into_any())
    })?;
    kept_or_returned(result, results)
}

fn ground_click<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let x = get(action_item, "x")?;
    let y = get(action_item, "y")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        slf.getattr("controller_service")?.call_method1("ground_click", (&x, &y))?;
        say(py, "info", &[&text(py, "✓ Ground clicked at ("), &x, &text(py, ", "), &y, &text(py, ")")])?;
        Ok(display!(py, {"status": "success", "action": "ground_click", "x": &x, "y": &y}).into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "✗ Ground click failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "ground_click",
            "x": &x,
            "y": &y,
            "message": &str_of(py, e)?,
        })
        .into_any())
    })?;
    kept_or_returned(result, results)
}

fn ground_scroll<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let x = get(action_item, "x")?;
    let y = get(action_item, "y")?;
    let direction = get(action_item, "value")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        slf.getattr("controller_service")?.call_method1("ground_scroll", (&x, &y, &direction))?;
        let parts = [text(py, "✓ Ground scrolled "), text(py, " from ("), text(py, ", "), text(py, ")")];
        say(py, "info", &[&parts[0], &direction, &parts[1], &x, &parts[2], &y, &parts[3]])?;
        Ok(display!(py, {
            "status": "success",
            "action": "ground_scroll",
            "x": &x,
            "y": &y,
            "direction": &direction,
        })
        .into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "✗ Ground scroll failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "ground_scroll",
            "x": &x,
            "y": &y,
            "message": &str_of(py, e)?,
        })
        .into_any())
    })?;
    kept_or_returned(result, results)
}

fn input<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let element_id = get(action_item, "id")?;
    let text_value = get(action_item, "value")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        let element_num = name(py, "int")?.call1((&element_id,))?;
        slf.getattr("controller_service")?.call_method1("type_text", (&element_num, &text_value))?;
        let parts = [text(py, "✓ Typed '"), text(py, "' in element ")];
        say(py, "info", &[&parts[0], &text_value, &parts[1], &element_num])?;
        Ok(display!(py, {
            "status": "success",
            "action": "input",
            "element": &element_num,
            "text": &text_value,
        })
        .into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "✗ Input failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "input",
            "element": &element_id,
            "message": &str_of(py, e)?,
        })
        .into_any())
    })?;
    results.append(&result)?;
    if get(&result, "status")?.eq("error")? {
        let parts = [text(py, "Input insertion in element "), text(py, " failed")];
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "input",
                "results": results,
                "message": &fstring(py, &[&parts[0], &element_id, &parts[1]])?,
            })
            .into_any(),
        ));
    }
    Ok(None)
}

fn scroll<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let element_id = get(action_item, "id")?;
    let direction = get(action_item, "value")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        let element_num = name(py, "int")?.call1((&element_id,))?;
        slf.getattr("controller_service")?.call_method1("scroll", (&element_num, &direction))?;
        say(py, "info", &[&text(py, "✓ Scrolled element "), &element_num, &text(py, " "), &direction])?;
        Ok(display!(py, {
            "status": "success",
            "action": "scroll",
            "element": &element_num,
            "direction": &direction,
        })
        .into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "✗ Scroll failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "scroll",
            "element": &element_id,
            "message": &str_of(py, e)?,
        })
        .into_any())
    })?;
    results.append(&result)?;
    if get(&result, "status")?.eq("error")? {
        let parts = [text(py, "Scroll on element "), text(py, " failed")];
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "scroll",
                "results": results,
                "message": &fstring(py, &[&parts[0], &element_id, &parts[1]])?,
            })
            .into_any(),
        ));
    }
    Ok(None)
}

fn open_app<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let m = module(py)?;
    let str_ = name(py, "str")?;
    let app_name = str_.call1((or_empty(get(action_item, "value")?)?,))?.call_method0("strip")?;
    say(py, "info", &[&text(py, "Opening application: "), &app_name])?;

    // Special 'home' case: return to the home screen
    if app_name.call_method0("lower")?.eq("home")? {
        let tried = (|| name(py, "app_launcher_service")?.call_method0("go_home"))();
        let success = except_exception(py, tried, |e| {
            log_error(py, "✗ Go home failed: ", e)?;
            Ok(boolean(py, false))
        })?;
        if success.is_truthy()? {
            let result = display!(py, {"status": "success", "action": "tool", "tool": "open_app", "app": "home"});
            results.append(&result)?;
            return Ok(None);
        }
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "tool",
                "tool": "open_app",
                "app": "home",
                "message": "Failed to navigate to home screen",
            })
            .into_any(),
        ));
    }

    let tried = (|| name(py, "app_launcher_service")?.call_method1("launch_app", (&app_name,)))();
    let launch = except_exception(py, tried, |e| {
        log_error(py, "✗ App launch failed: ", e)?;
        Ok(display!(py, {
            "ok": false,
            "message": &fstring(py, &[&text(py, "App launch raised: "), e.value(py)])?,
        })
        .into_any())
    })?;
    // Report what was ACTUALLY launched (resolved display
    // name + bundle id), never just the query - and success
    // only when the launcher confirmed the foreground app.
    let result = display!(py, {
        "status": if get(&launch, "ok")?.is_truthy()? { "success" } else { "error" },
        "action": "tool",
        "tool": "open_app",
        "requested": &app_name,
        "message": &launch.call_method1("get", ("message", ""))?,
    });
    if get(&launch, "display_name")?.is_truthy()? {
        result.set_item("app", launch.get_item("display_name")?)?;
        result.set_item("bundle_id", launch.get_item("bundle_id")?)?;
    }
    if get(&launch, "ok")?.is_truthy()? {
        if get(&launch, "verified")?.is(&boolean(py, false)) {
            result.set_item("verified", false)?;
        }
        log(&m, "info", || fstring(py, &[&text(py, "Opened "), &result.call_method1("get", ("app", &app_name))?]))?;
        results.append(&result)?;
        return Ok(None);
    }
    log(&m, "error", || {
        let message = result.as_any().get_item("message")?;
        fstring(py, &[&text(py, "Failed to open "), &app_name, &text(py, ": "), &message])
    })?;
    Ok(Some(result.into_any()))
}

fn wait<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let float = name(py, "float")?;
    let wait_time = float.call1((action_item.call_method1("get", ("value", "1"))?,))?;
    say(py, "info", &[&text(py, "Waiting for "), &wait_time, &text(py, " seconds...")])?;
    let mut elapsed = 0.0_f64.into_pyobject(py)?.into_any();
    while elapsed.lt(&wait_time)? {
        if stop_requested(slf)? {
            return Ok(Some(stopped(py)?));
        }
        let sleep = name(py, "time")?.getattr("sleep")?;
        let min = name(py, "min")?;
        let left = wait_time.sub(&elapsed)?;
        sleep.call1((min.call1((0.5, left))?,))?;
        elapsed = elapsed.add(0.5)?;
    }
    log(&module(py)?, "info", || Ok(text(py, "Wait completed")))?;
    let result = display!(py, {"status": "success", "action": "tool", "tool": "wait", "duration": &wait_time});
    results.append(&result)?;
    Ok(None)
}

fn web<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let query = get(action_item, "value")?;
    say(py, "info", &[&text(py, "Performing web search: "), &query])?;

    if name(py, "WebService")?.is_none() {
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "tool",
                "tool": "web",
                "query": &query,
                "message": "Web service not available yet on iOS",
            })
            .into_any(),
        ));
    }

    if slf.getattr("web_callback")?.is_truthy()? {
        slf.getattr("web_callback")?.call1(("start",))?;
    }

    slf.setattr("_stop_loading", false)?;
    let thread = name(py, "threading")?.getattr("Thread")?;
    let target = slf.getattr("_web_loading_animation")?;
    let loading_thread = thread.call((), Some(&display!(py, {"target": &target})))?;
    loading_thread.setattr("daemon", true)?;
    loading_thread.call_method0("start")?;

    let searched = (|| -> PyResult<Obj<'py>> {
        let web_service = name(py, "WebService")?;
        let (provider, model, api_key) = (slf.getattr("provider")?, slf.getattr("model")?, slf.getattr("api_key")?);
        let kwargs = display!(py, {"stop_event": &slf.getattr("stop_event")?});
        let web_service = web_service.call((provider, model, api_key), Some(&kwargs))?;
        web_service.call_method1("search", (&query,))
    })();
    // finally: it runs whether the search raised or not. After a raise the
    // exception is the one being handled while it runs, and goes on after.
    handling(py, searched.as_ref().err(), || {
        slf.setattr("_stop_loading", true)?;
        loading_thread.call_method("join", (), Some(&display!(py, {"timeout": 1})))?;
        // Clear the in-place text only in TTY mode
        if name(py, "sys")?.getattr("stdout")?.call_method0("isatty")?.is_truthy()? {
            let write = name(py, "sys")?.getattr("stdout")?.getattr("write")?;
            write.call1((format!("\r{}\r", " ".repeat(50)),))?;
            name(py, "sys")?.getattr("stdout")?.call_method0("flush")?;
        }

        if slf.getattr("web_callback")?.is_truthy()? {
            slf.getattr("web_callback")?.call1(("end",))?;
            // Wait for CSS fade-out to complete before next action
            name(py, "time")?.getattr("sleep")?.call1((0.7,))?;
        }
        Ok(())
    })?;
    let web_result = searched?;

    // WebService reports a failed search inside its text
    let failed = name(py, "str")?.call1((&web_result,))?;
    let failed = failed.call_method1("startswith", ("{\ntool: web,\nstatus: error,",))?;
    let result = display!(py, {
        "status": if failed.is_truthy()? { "error" } else { "success" },
        "action": "tool",
        "tool": "web",
        "query": &query,
        "result": &web_result,
    });
    results.append(&result)?;
    Ok(None)
}

fn shell<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    // Runs on the host Mac this agent runs on, not the iPhone.
    let command = action_item.call_method1("get", ("value", ""))?;

    if slf.getattr("shell_service")?.is_none() {
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "shell",
                "message": "Shell service not available yet on iOS",
            })
            .into_any(),
        ));
    }

    // Signal frontend: terminal card appears
    if slf.getattr("shell_callback")?.is_truthy()? {
        slf.getattr("shell_callback")?.call1(("start", &command))?;
    }

    let result = slf.getattr("shell_service")?.call_method1("run", (&command,))?;

    // Signal frontend: show success/fail result
    if slf.getattr("shell_callback")?.is_truthy()? {
        let shell_status = result.call_method1("get", ("status", "failed"))?;
        let outer_get = result.getattr("get")?;
        let shell_output = outer_get.call1(("output", result.call_method1("get", ("error", ""))?))?;
        let callback = slf.getattr("shell_callback")?;
        callback.call1(("result", display!(py, {"status": &shell_status, "output": &or_empty(shell_output)?})))?;
        // Hold result on screen for 2 seconds
        name(py, "time")?.getattr("sleep")?.call1((2,))?;
        slf.getattr("shell_callback")?.call1(("end",))?;
        // Wait for CSS fade-out to complete before next action
        name(py, "time")?.getattr("sleep")?.call1((0.7,))?;
    }

    results.append(&result)?;
    if get(&result, "status")?.eq("error")? && get(&result, "error")?.is_truthy()? {
        return Ok(Some(result));
    }
    Ok(None)
}

/// `str(action_item.get("value") or "").strip().lower() in ("pin", "passcode")`
fn names_pin(action_item: &Obj<'_>) -> PyResult<bool> {
    let py = action_item.py();
    let str_ = name(py, "str")?;
    let word = str_.call1((or_empty(get(action_item, "value")?)?,))?;
    let word = word.call_method0("strip")?.call_method0("lower")?;
    PyTuple::new(py, ["pin", "passcode"])?.contains(word)
}

/// `(self.controller_service.elements.get(str(action_item.get("id"))) or {}).get("type")
/// not in TEXT_FIELDS`
fn off_text_field(slf: &Obj<'_>, action_item: &Obj<'_>) -> PyResult<bool> {
    let py = slf.py();
    let lookup = slf.getattr("controller_service")?.getattr("elements")?.getattr("get")?;
    let str_ = name(py, "str")?;
    let element = lookup.call1((str_.call1((get(action_item, "id")?,))?,))?;
    let element = if element.is_truthy()? { element } else { PyDict::new(py).into_any() };
    let kind = get(&element, "type")?;
    Ok(!name(py, "TEXT_FIELDS")?.contains(kind)?)
}

fn vault_pin<'py>(slf: &Obj<'py>, action_data: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let result = slf.getattr("_enter_vault_pin")?.call1((action_data,))?;
    if result.get_item("status")?.ne("success")? {
        return Ok(Some(result));
    }
    results.append(&result)?;
    Ok(None)
}

fn vault_fill<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    // The value saved under the key the agent names, typed like
    // `input` (vault/service.py fill). Batches like `input` too.
    let fill = name(py, "vault_service")?.getattr("fill")?;
    let controller_service = slf.getattr("controller_service")?;
    let (element_id, key) = (get(action_item, "id")?, get(action_item, "value")?);
    let result = fill.call1((controller_service, element_id, key))?;
    results.append(&result)?;
    if result.get_item("status")?.ne("success")? {
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "vault",
                "results": results,
                "message": &result.get_item("message")?,
            })
            .into_any(),
        ));
    }
    Ok(None)
}

fn video_player<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let command = get(action_item, "value")?;
    let tried = (|| -> PyResult<Obj<'py>> {
        let player = || slf.getattr("videoplayer_service");
        let tool_response = if command.eq("close")? {
            let success = player()?.call_method0("close")?;
            if success.is_truthy()? { "videoplayer: Result - Closed" } else { "videoplayer: Result - Failed to close" }
        } else if command.eq("streaming")? {
            let is_streaming = player()?.call_method0("check_streaming")?;
            if is_streaming.is_truthy()? { "videoplayer: Result - Streaming" } else { "videoplayer: Result - Not streaming" }
        } else if command.eq("pause")? {
            let success = player()?.call_method0("pause")?;
            if success.is_truthy()? { "videoplayer: Result - Paused" } else { "videoplayer: Result - Failed to pause" }
        } else if command.eq("play")? {
            let success = player()?.call_method0("play")?;
            if success.is_truthy()? { "videoplayer: Result - Playing" } else { "videoplayer: Result - Failed to play" }
        } else {
            return Err(throw(&module(py)?, "ValueError", || {
                fstring(py, &[&text(py, "Unknown video_player command: "), &command])
            }));
        };
        let tool_response = text(py, tool_response);

        let result = display!(py, {
            "status": "success",
            "action": "video_player",
            "command": &command,
            "tool_response": &tool_response,
        });
        say(py, "info", &[&text(py, "🎬 "), &tool_response])?;
        Ok(result.into_any())
    })();
    let result = except_exception(py, tried, |e| {
        log_error(py, "Video player action failed: ", e)?;
        Ok(display!(py, {
            "status": "error",
            "action": "video_player",
            "command": &command,
            "message": &str_of(py, e)?,
            "tool_response": &fstring(py, &[&text(py, "videoplayer: Result - Error: "), &str_of(py, e)?])?,
        })
        .into_any())
    })?;
    results.append(&result)?;
    Ok(None)
}

fn todo_list<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let todo_value = get(action_item, "value")?;
    slf.getattr("task_tracker")?.call_method1("save_todo", (&todo_value,))?;
    let result = display!(py, {"status": "success", "action": "todo_created"});
    results.append(&result)?;
    Ok(None)
}

fn update_todo<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let int = name(py, "int")?;
    let task_number = int.call1((action_item.call_method1("get", ("value", "0"))?,))?;
    let success = slf.getattr("task_tracker")?.call_method1("update_task", (&task_number,))?;
    if success.is_truthy()? {
        let result = display!(py, {"status": "success", "action": "todo_updated", "task": &task_number});
        results.append(&result)?;
    } else {
        // Reported on its own call; the rest of the batch still runs
        let parts = [text(py, "Task "), text(py, " is not in the todo list")];
        results.append(display!(py, {
            "status": "error",
            "action": "todo_update_failed",
            "message": &fstring(py, &[&parts[0], &task_number, &parts[1]])?,
        }))?;
    }
    Ok(None)
}

fn scratchpad<'py>(slf: &Obj<'py>, action_item: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    let py = slf.py();
    let scratchpad_value = get(action_item, "value")?;
    if slf.getattr("scratchpad_service")?.is_none() {
        return Ok(Some(
            display!(py, {
                "status": "error",
                "action": "scratchpad_failed",
                "message": "Scratchpad service not available yet on iOS",
            })
            .into_any(),
        ));
    }
    let success = slf.getattr("scratchpad_service")?.call_method1("append_scratchpad", (&scratchpad_value,))?;
    if success.is_truthy()? {
        let result = display!(py, {"status": "success", "action": "scratchpad_added", "scratchpad": &scratchpad_value});
        results.append(&result)?;
        return Ok(None);
    }
    Ok(Some(
        display!(py, {
            "status": "error",
            "action": "scratchpad_failed",
            "message": "Could not add scratchpad entry",
        })
        .into_any(),
    ))
}

fn done<'py>(py: Python<'py>, action_item: &Obj<'py>) -> Step<'py> {
    let summary = get(action_item, "value")?;
    say(py, "info", &[&text(py, "Task Complete: "), &summary])?;
    Ok(Some(display!(py, {"status": "success", "action": "done", "summary": &summary}).into_any()))
}

fn unknown<'py>(py: Python<'py>, action_type: &Obj<'py>, results: &Bound<'py, PyList>) -> Step<'py> {
    say(py, "warning", &[&text(py, "Unknown action type: "), action_type])?;
    results.append(display!(py, {
        "status": "error",
        "action": action_type,
        "message": &fstring(py, &[&text(py, "Unknown action type: "), action_type])?,
    }))?;
    Ok(None)
}

/// route_action's try block: every action in turn, then the reply.
fn route<'py>(slf: &Obj<'py>, action_data: &Obj<'py>) -> PyResult<Obj<'py>> {
    let py = slf.py();
    // Accept a single flat action dict by wrapping it in a list
    let action_data = if is_dict(action_data)? { PyList::new(py, [action_data])?.into_any() } else { action_data.clone() };

    let results = PyList::empty(py);

    for action_item in action_data.try_iter()? {
        let action_item = action_item?;
        // Check stop before every action
        if stop_requested(slf)? {
            return stopped(py);
        }

        if !is_dict(&action_item)? || !get(&action_item, "type")?.is_truthy()? {
            say(py, "warning", &[&text(py, "Action item missing 'type' field: "), &action_item])?;
            continue;
        }

        let action_type = get(&action_item, "type")?;
        let item = &action_item;

        let ended = if action_type.eq("click")? {
            click(slf, item, &results)?
        } else if action_type.eq("ground_click")? {
            ground_click(slf, item, &results)?
        } else if action_type.eq("ground_scroll")? {
            ground_scroll(slf, item, &results)?
        } else if action_type.eq("input")? {
            input(slf, item, &results)?
        } else if action_type.eq("scroll")? {
            scroll(slf, item, &results)?
        } else if action_type.eq("open_app")? {
            open_app(slf, item, &results)?
        } else if action_type.eq("wait")? {
            wait(slf, item, &results)?
        } else if action_type.eq("web")? {
            web(slf, item, &results)?
        } else if action_type.eq("shell")? {
            shell(slf, item, &results)?
        } else if action_type.eq("vault")? && names_pin(item)? && off_text_field(slf, item)? {
            // A PIN aimed at no text field (id 0) goes to the keypad path:
            // the lock screen and app PIN pads have no field to type into.
            // A PIN field is filled like any other key, below.
            vault_pin(slf, &action_data, &results)?
        } else if action_type.eq("vault")? {
            vault_fill(slf, item, &results)?
        } else if action_type.eq("video_player")? {
            video_player(slf, item, &results)?
        } else if action_type.eq("todo_list")? {
            todo_list(slf, item, &results)?
        } else if action_type.eq("update_todo")? {
            update_todo(slf, item, &results)?
        } else if action_type.eq("scratchpad")? {
            scratchpad(slf, item, &results)?
        } else if action_type.eq("done")? {
            done(py, item)?
        } else {
            unknown(py, &action_type, &results)?
        };
        if let Some(reply) = ended {
            return Ok(reply);
        }

        // Check if last action was stopped
        if !results.is_empty() && get(&results.as_any().get_item(-1)?, "status")?.eq("stopped")? {
            return results.as_any().get_item(-1);
        }
    }

    if name(py, "len")?.call1((&results,))?.eq(0)? {
        Ok(display!(py, {"status": "error", "message": "No valid action found"}).into_any())
    } else if name(py, "len")?.call1((&results,))?.eq(1)? {
        results.as_any().get_item(0)
    } else {
        Ok(display!(py, {"status": "success", "action": "multiple", "results": &results}).into_any())
    }
}

// ---------------------------------------------------------------------------
// ControllerView
// ---------------------------------------------------------------------------

// The router. A plain comment: the Python class had no docstring, and a doc
// comment here would give it one.
#[pyclass(frozen, dict, weakref, subclass)]
pub struct ControllerView;

#[pymethods]
impl ControllerView {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(
        signature = (*_args, **_kwargs),
        text_signature = "(provider=None, model=None, web_callback=None, shell_callback=None, api_key=None, stop_event=None)"
    )]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        ControllerView
    }

    /// Initialize the Controller View - central router for all iPhone actions
    ///
    /// Args:
    ///     provider: LLM provider name for web search
    ///     model: LLM model name for web search
    ///     web_callback: Optional callback for web search status (start/end)
    ///     shell_callback: Optional callback for shell execution status (start/result/end)
    ///     api_key: Optional runtime API key for LLM providers (passed to web search)
    ///     stop_event: Optional threading.Event for stopping actions mid-execution
    #[pyo3(signature = (provider = None, model = None, web_callback = None, shell_callback = None, api_key = None, stop_event = None))]
    fn __init__<'py>(
        slf: &Bound<'py, Self>,
        provider: Option<&Obj<'py>>,
        model: Option<&Obj<'py>>,
        web_callback: Option<&Obj<'py>>,
        shell_callback: Option<&Obj<'py>>,
        api_key: Option<&Obj<'py>>,
        stop_event: Option<&Obj<'py>>,
    ) -> PyResult<()> {
        let py = slf.py();
        let given = |value: Option<&Obj<'py>>| value.cloned().unwrap_or_else(|| none(py));
        slf.setattr("api_key", given(api_key))?;
        slf.setattr("stop_event", given(stop_event))?;
        // Module singleton: the SAME instance the scanner feeds element
        // mappings into during scan_elements.
        slf.setattr("controller_service", name(py, "controller_service")?)?;
        slf.setattr("task_tracker", name(py, "TaskTrackerService")?.call0()?)?;
        let player = name(py, "VideoPlayerService")?;
        slf.setattr("videoplayer_service", player.call1((name(py, "controller_service")?,))?)?;
        let scratchpad = if name(py, "ScratchpadService")?.is_truthy()? {
            name(py, "ScratchpadService")?.call0()?
        } else {
            none(py)
        };
        slf.setattr("scratchpad_service", scratchpad)?;
        let shell = if name(py, "ShellService")?.is_truthy()? { name(py, "ShellService")?.call0()? } else { none(py) };
        slf.setattr("shell_service", shell)?;
        slf.setattr("provider", given(provider))?;
        slf.setattr("model", given(model))?;
        slf.setattr("web_callback", given(web_callback))?;
        slf.setattr("shell_callback", given(shell_callback))?;
        slf.setattr("_stop_loading", false)?;
        slf.setattr("_pin_attempts", PyDict::new(py))?; // vault entry name -> PIN tries that may have been spent
        Ok(())
    }

    #[doc = "Display animated loading indicator for web search.\n\nSkipped when stdout is NOT a TTY (i.e. piped subprocess) \u{2014} the `\\r`\ncarriage-return overwrite trick only works in a real terminal.\n"]
    fn _web_loading_animation(slf: &Bound<'_, Self>) -> PyResult<()> {
        let py = slf.py();
        if !name(py, "sys")?.getattr("stdout")?.call_method0("isatty")?.is_truthy()? {
            return Ok(());
        }
        let dots = PyList::new(py, ["", ".", "..", "..."])?;
        let mut idx = 0_i64.into_pyobject(py)?.into_any();
        while !slf.getattr("_stop_loading")?.is_truthy()? {
            let write = name(py, "sys")?.getattr("stdout")?.getattr("write")?;
            let count = name(py, "len")?.call1((&dots,))?;
            let dot = dots.as_any().get_item(idx.rem(&count)?)?;
            write.call1((fstring(py, &[&text(py, "\r🌐 Web"), &dot, &text(py, "   ")])?,))?;
            name(py, "sys")?.getattr("stdout")?.call_method0("flush")?;
            idx = idx.add(1)?;
            name(py, "time")?.getattr("sleep")?.call1((0.5,))?;
        }
        Ok(())
    }

    #[doc = "vault {\"id\": 0, \"value\": \"pin\"}: the vault taps its PIN on the on-screen keypad.\n\nEvery refusal happens before the first tap. Results and logs never\ncarry a digit, a key id or a coordinate (see the PIN section of\nvault/service.py).\n"]
    fn _enter_vault_pin<'py>(slf: &Bound<'py, Self>, action_data: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let base = display!(py, {"action": "vault", "element": 0, "kind": "pin"});
        // {**base, "status": "error", "message": message}
        let refused = |message: Obj<'py>| -> PyResult<Obj<'py>> {
            let built = merged(py, &[base.as_any()])?;
            built.set_item("status", "error")?;
            built.set_item("message", message)?;
            Ok(built.into_any())
        };
        if name(py, "len")?.call1((action_data,))?.gt(1)? {
            // The keys come from this step's scan; another action could move the screen first.
            return refused(text(py, "A PIN vault must be the only action in its step. Re-issue it alone."));
        }
        // Keypad first, vault second: asking where no keypad is must not reveal whether a PIN is saved.
        let find_pin_keys = name(py, "vault_service")?.getattr("find_pin_keys")?;
        let keys = find_pin_keys.call1((slf.getattr("controller_service")?.getattr("elements")?,))?;
        if !keys.is_truthy()? {
            return refused(text(py, "No digit keypad (0-9) recognised on screen. Open the PIN screen first."));
        }

        let (entry, pin) = unpack2(&name(py, "vault_service")?.call_method0("get_pin")?)?;
        if !pin.is_truthy()? {
            let parts = [
                text(py, "No PIN saved in the vault for \""),
                text(py, "\". Ask the user to add \""),
                text(py, "\": {\"pin\": \"...\"} under \"ios\" in AutoCua_data/vault/credentials.json."),
            ];
            return refused(fstring(py, &[&parts[0], &entry, &parts[1], &entry, &parts[2]])?);
        }
        let tries = slf.getattr("_pin_attempts")?.call_method1("get", (&entry, 0))?;
        if tries.ge(name(py, "MAX_PIN_ATTEMPTS")?)? {
            let parts = [
                text(py, "The \""),
                text(py, "\" PIN was already tried twice in this task. Stop and ask the user, another wrong try could lock the keypad."),
            ];
            return refused(fstring(py, &[&parts[0], &entry, &parts[1]])?);
        }

        let entered = (|| -> PyResult<Obj<'py>> {
            let enter_pin = name(py, "vault_service")?.getattr("enter_pin")?;
            let controller_service = slf.getattr("controller_service")?;
            let squeezed = entry.call_method0("lower")?.call_method1("replace", (" ", ""))?;
            let kwargs = display!(py, {"device_entry": name(py, "DEVICE_ENTRIES")?.contains(&squeezed)?});
            enter_pin.call((controller_service, &keys, &pin), Some(&kwargs))
        })();
        let outcome = except_exception(py, entered, |_| {
            Ok(display!(py, {
                "status": "error",
                "pressed": true,
                "closed": false,
                "message": "PIN entry failed and the keys may have been pressed. Do not retry - check the next screenshot, and ask the user if unsure.",
            })
            .into_any())
        })?;
        // A pad that closed starts fresh; only a try that may have reached the pad counts.
        if outcome.call_method1("pop", ("closed", false))?.is_truthy()? {
            slf.getattr("_pin_attempts")?.set_item(&entry, 0)?;
        } else if outcome.call_method1("pop", ("pressed", true))?.is_truthy()? {
            let next = tries.add(1)?;
            slf.getattr("_pin_attempts")?.set_item(&entry, next)?;
        }
        outcome.call_method1("pop", ("pressed", none(py)))?;
        log(&module(py)?, "info", || {
            fstring(py, &[&text(py, "🔐 Vault PIN on keypad: "), &outcome.get_item("status")?])
        })?;
        Ok(merged(py, &[base.as_any(), &outcome])?.into_any())
    }

    #[doc = "\nRoute actions to appropriate service based on action type.\n\nFlat action format (matching the macOS agent):\n    [{\"type\": \"click\", \"id\": 4}, {\"type\": \"wait\", \"value\": \"2\"}, ...]\n\nArgs:\n    action_data (list): The action list from the LLM response.\n        A single flat action dict is also accepted and wrapped.\n\nReturns:\n    dict: Result of the action execution (single result, or\n          {\"action\": \"multiple\", \"results\": [...]} for a sequence).\n"]
    fn route_action<'py>(slf: &Bound<'py, Self>, action_data: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let routed = route(slf.as_any(), action_data);
        except_exception(py, routed, |e| {
            log_error(py, "Error routing action: ", e)?;
            Ok(display!(py, {"status": "error", "message": &str_of(py, e)?}).into_any())
        })
    }

    /// Set the elements mapping in controller service
    #[pyo3(
        signature = (elements_mapping, application_name = None),
        text_signature = "($self, elements_mapping, application_name='')"
    )]
    fn set_elements(slf: &Bound<'_, Self>, elements_mapping: &Obj<'_>, application_name: Option<&Obj<'_>>) -> PyResult<()> {
        // application_name is not used, as in view.py.
        let _ = application_name;
        slf.getattr("controller_service")?.call_method1("update_elements", (elements_mapping,))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes view.py's place with the class. view.py had
/// no docstring, so the module's __doc__ stays None, as a new module's is.
/// controller/__init__.py binds the rest when the module is first imported
/// (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", py.None())?;
    m.add_class::<ControllerView>()?;
    let class = py.get_type::<ControllerView>();
    as_python_class(&class, &python_name)?;
    // The Python class had no docstring: None, where the extension gives "".
    class.setattr("__doc__", py.None())?;
    // What Python 3.13 gave the class statement: its line in view.py and the
    // attributes its methods set through self, sorted.
    if py.version_info() >= (3, 13) {
        class.setattr("__firstlineno__", 32)?;
        let attributes = PyTuple::new(
            py,
            [
                "_pin_attempts",
                "_stop_loading",
                "api_key",
                "controller_service",
                "model",
                "provider",
                "scratchpad_service",
                "shell_callback",
                "shell_service",
                "stop_event",
                "task_tracker",
                "videoplayer_service",
                "web_callback",
            ],
        )?;
        class.setattr("__static_attributes__", attributes)?;
    }
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
