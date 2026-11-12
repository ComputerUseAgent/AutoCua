//! videoplayer: full-screen video playback control on the iPhone (play,
//! pause, close, is it streaming) through WebDriverAgent. DRM players black
//! out screenshots, so it works from the accessibility source: which
//! transport button the tree holds says playing or paused, and the progress
//! label advancing says streaming. Element clicks, never coordinate taps,
//! actuate the controls; see the module docstring below for what was learned
//! live on the device.
//!
//! The Rust copy of tool/videoplayer.py. Python imports it under its old
//! dotted name, AutoCua.ios.controller.tool.videoplayer (tool/__init__.py
//! points that name here), and gets the same class, methods, constants,
//! return values and log records. Python's own rules decide every result, so
//! they are called, not imitated: requests, ElementTree, re, time, the set
//! and dict operations, `is` and the f-strings.
//!
//! The names videoplayer.py imported (requests, time, logging, re, ET) and
//! its logger are bound on this module by tool/__init__.py when the module
//! is first imported, as the .py did then; the constants are made with the
//! module. Every one is read from the module each time it is used, so a
//! caller or a test that replaces one is obeyed. The instance keeps
//! controller_service in its __dict__, and the methods reach it and each
//! other through the instance, so a subclass that replaces one is obeyed.

use pyo3::basic::CompareOp;
use pyo3::exceptions::PyAttributeError;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyList, PySet, PyString, PyTuple};

use crate::computer::as_python_class;
use crate::ios::controller::{dict, fstring, log, log_failure, text};
use crate::ios::NATIVE;
use crate::memory_compression::{global, is_exception};

type Obj<'py> = Bound<'py, PyAny>;

/// videoplayer.py's docstring.
const MODULE_DOC: &str = "\nvideoplayer.py - Full-screen video playback control via WebDriverAgent.\n\nDRM-protected players black out screenshots, so the agent cannot see the\nvideo surface. This service works from the accessibility source instead.\n\nFacts established live against Sky Go on an iPhone (26.6.1), which shape\neverything below:\n\n  * The transport buttons stay in the accessibility tree - with visible=true -\n    even while the overlay is VISUALLY hidden. Tree presence proves nothing\n    about what is on screen.\n  * Coordinate taps at a button's own centre are eaten by the player's\n    screen-covering gesture layer (\"Double tap to show controls\") and merely\n    toggle the overlay. That layer is exactly why the old tap-at-(x,y) code\n    \"tapped the screen, showed the icons, and never played or paused\".\n  * WDA ELEMENT clicks actuate a control only when it is really shown;\n    clicking the gesture-layer element is the reliable way to reveal the\n    overlay. Reveal + element-click is the sequence that actually paused and\n    played the content on the device.\n  * The transport button's NAME tracks true playback state: the tree holds\n    pauseButton while playing and playButton while paused, overlay hidden or\n    not. That is the state check. The progress label (\"You've watched N\n    Minutes M Seconds\") advancing is the ground truth for the streaming check.\n";

/// The class's docstring.
const CLASS_DOC: &str = "Controls the full-screen video player through the controller session";

// WDA predicate strings (NSPredicate syntax). Sky Go names first, the legacy
// Player_* names kept for the players the old code supported, plus the plain
// words. Exact == on the words, so AirPlay can never match "Play".
const PLAY_PREDICATE: &str = "type == \"XCUIElementTypeButton\" AND \
(name == \"playButton\" OR name == \"Player_Play_Button\" OR label == \"Play\" OR name == \"Play\")";
const PAUSE_PREDICATE: &str = "type == \"XCUIElementTypeButton\" AND \
(name == \"pauseButton\" OR name == \"Player_Pause_Button\" OR label == \"Pause\" OR name == \"Pause\")";
// The screen-covering gesture layer that toggles the overlay. Matched loosely:
// its label reads "Double tap to show controls" on Sky Go.
const TOGGLE_PREDICATE: &str = "label CONTAINS[c] \"show controls\" OR name CONTAINS[c] \"show controls\"";
// Tried in order: exact trusted names first, then a loose substring pass.
// "caption" is excluded from the loose pass - "Closed Captions" contains
// "close", and closing the player must never toggle subtitles instead.
const CLOSE_EXACT: &str = "type == \"XCUIElementTypeButton\" AND \
(name == \"closeButton\" OR name == \"Player_Close_Button\" OR \
label == \"Close player\" OR label == \"Close\" OR name == \"Close\")";
const CLOSE_LOOSE: &str = "type == \"XCUIElementTypeButton\" AND \
(label CONTAINS[c] \"close\" OR name CONTAINS[c] \"close\" OR \
label CONTAINS[c] \"dismiss\") AND \
NOT (label CONTAINS[c] \"caption\" OR name CONTAINS[c] \"caption\")";

/// "m:ss" or "h:mm:ss" at the start of a Player_Progress_Label.
const PROGRESS_PATTERN: &str = r"(\d{1,2}:\d{2}(?::\d{2})?)";
/// Sky Go's watched label; see _scan_state.
const WATCHED_PATTERN: &str = r"you\W{0,2}ve watched|you have watched";

/// The fallback tap of _reveal_controls: a touch on the left of the screen.
const TAP_ACTIONS: &str = r#"{"actions": [{"type": "pointer", "id": "finger1", "parameters": {"pointerType": "touch"}, "actions": [{"type": "pointerMove", "duration": 0, "x": 100, "y": 195}, {"type": "pointerDown", "button": 0}, {"type": "pause", "duration": 10}, {"type": "pointerUp", "button": 0}]}]}"#;

/// This module, as registered: its globals are read from it at run time.
static MODULE: PyOnceLock<Py<PyModule>> = PyOnceLock::new();

fn module(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    MODULE
        .get_or_try_init(py, || py.import(format!("{NATIVE}.controller.tool.videoplayer")).map(Bound::unbind))
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

/// `logger.<level>(f"...")`, the parts given in order.
fn say<'py>(py: Python<'py>, level: &str, parts: &[&Obj<'py>]) -> PyResult<()> {
    log(&module(py)?, level, || fstring(py, parts))
}

/// `time.sleep(<constant>)`, both read now.
fn sleep(py: Python<'_>, constant: &str) -> PyResult<()> {
    let sleep = name(py, "time")?.getattr("sleep")?;
    sleep.call1((name(py, constant)?,))?;
    Ok(())
}

/// `requests.post(url, json=..., timeout=...)`: requests.post looked up
/// first, then the url made, as Python evaluates the call.
fn post<'py>(
    py: Python<'py>,
    url: impl FnOnce() -> PyResult<Obj<'py>>,
    json: &Obj<'py>,
    timeout: i64,
) -> PyResult<Obj<'py>> {
    let post = name(py, "requests")?.getattr("post")?;
    let url = url()?;
    let timeout = timeout.into_pyobject(py)?.into_any();
    post.call((url,), Some(&dict(py, &[("json", json), ("timeout", &timeout)])?))
}

// ---------------------------------------------------------------------------
// VideoPlayerService
// ---------------------------------------------------------------------------

#[pyclass(frozen, dict, weakref, subclass)]
pub struct VideoPlayerService;

#[pymethods]
impl VideoPlayerService {
    /// A bare instance, as object.__new__ gave: __init__ fills it in.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs), text_signature = "(controller_service)")]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        VideoPlayerService
    }

    #[pyo3(signature = (controller_service))]
    fn __init__(slf: &Bound<'_, Self>, controller_service: &Obj<'_>) -> PyResult<()> {
        // Shares the WDA endpoint + session handling with the controller service
        slf.setattr("controller_service", controller_service)
    }

    #[getter]
    fn wda_url<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        slf.getattr("controller_service")?.getattr("wda_url")
    }

    // A property with no setter or deleter refuses both, in Python's words.
    // A plain comment: the old property had no docstring.
    #[setter]
    fn set_wda_url(slf: &Bound<'_, Self>, value: Option<&Obj<'_>>) -> PyResult<()> {
        let what = if value.is_some() { "setter" } else { "deleter" };
        let kind = slf.get_type().name()?;
        Err(PyAttributeError::new_err(format!("property 'wda_url' of '{kind}' object has no {what}")))
    }

    fn get_session<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        slf.getattr("controller_service")?.getattr("get_session")?.call0()
    }

    // ------------------------------------------------------------------ WDA

    #[doc = "First element matching a WDA predicate string, or None.\n\nThe caller supplies the session, and the matching _click MUST reuse\nthe same one: element ids live in a per-session cache, and the\ncontroller's get_session() mints a NEW session on every call - a\nclick through a fresh session would be handed an id that session\nhas never seen."]
    fn _find<'py>(slf: &Bound<'py, Self>, session: &Obj<'py>, predicate: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let found = (|| -> PyResult<Obj<'py>> {
            let url = || fstring(py, &[&slf.getattr("wda_url")?, &text(py, "/session/"), session, &text(py, "/element")]);
            let body = dict(py, &[("using", &text(py, "predicate string")), ("value", predicate)])?.into_any();
            let response = post(py, url, &body, 10)?;
            // Not-found is a NON-200 whose value is {"error": "no such
            // element", "message": ...} - a dict of strings. Returning any
            // string out of that dict hands back "no such element" as an
            // element id, which made every find look successful and every
            // screen look like a player. Success responses only.
            if response.getattr("status_code")?.ne(200)? {
                return Ok(none(py));
            }
            let value = response.call_method0("json")?.call_method1("get", ("value",))?;
            if value.is_instance_of::<PyDict>() && !value.call_method1("get", ("error",))?.is_truthy()? {
                for element_id in value.call_method0("values")?.try_iter()? {
                    let element_id = element_id?;
                    if element_id.is_instance_of::<PyString>() && element_id.is_truthy()? {
                        return Ok(element_id);
                    }
                }
            }
            Ok(none(py))
        })();
        match found {
            Ok(element_id) => Ok(element_id),
            Err(e) => {
                log_failure(&module(py)?, "❌ Element find error: ", e)?;
                Ok(none(py))
            }
        }
    }

    #[doc = "Click an element BY ID - with the SAME session that found it.\nNever replace this with a coordinate tap: taps at the same point are\nswallowed by the player's gesture layer."]
    fn _click(slf: &Bound<'_, Self>, session: &Obj<'_>, element_id: &Obj<'_>) -> PyResult<bool> {
        let py = slf.py();
        let clicked = (|| -> PyResult<bool> {
            let url = || {
                let base = slf.getattr("wda_url")?;
                let middle = [text(py, "/session/"), text(py, "/element/"), text(py, "/click")];
                fstring(py, &[&base, &middle[0], session, &middle[1], element_id, &middle[2]])
            };
            let response = post(py, url, &PyDict::new(py).into_any(), 10)?;
            if response.getattr("status_code")?.ne(200)? {
                return Ok(false);
            }
            let value = response.call_method0("json")?.call_method1("get", ("value",))?;
            Ok(!(value.is_instance_of::<PyDict>() && value.call_method1("get", ("error",))?.is_truthy()?))
        })();
        match clicked {
            Ok(done) => Ok(done),
            Err(e) => {
                log_failure(&module(py)?, "❌ Element click error: ", e)?;
                Ok(false)
            }
        }
    }

    #[doc = "Bring the overlay up by clicking the player's own toggle element;\nfall back to a corner-area tap for players without one.\n\nThe raw tap is gated on the CALLER having seen player evidence (a\ntransport button or progress label in the tree): if play/pause was\ninvoked on a non-player screen, firing coordinate taps would press\nrandom UI on whatever screen is actually up."]
    #[pyo3(signature = (allow_blind_tap = None), text_signature = "($self, allow_blind_tap=False)")]
    fn _reveal_controls(slf: &Bound<'_, Self>, allow_blind_tap: Option<&Obj<'_>>) -> PyResult<()> {
        let py = slf.py();
        let m = module(py)?;
        let session = slf.getattr("get_session")?.call0()?;
        if !session.is_truthy()? {
            log(&m, "error", || Ok(text(py, "❌ Failed to get session")))?;
            return Ok(());
        }
        let find = slf.getattr("_find")?;
        let toggle = find.call1((&session, name(py, "TOGGLE_PREDICATE")?))?;
        if toggle.is_truthy()? && slf.getattr("_click")?.call1((&session, &toggle))?.is_truthy()? {
            log(&m, "debug", || Ok(text(py, "🎬 Revealed controls via toggle element")))?;
            return Ok(());
        }
        // The default, False, and an explicit None are both falsy.
        let blind = match allow_blind_tap {
            Some(allowed) => allowed.is_truthy()?,
            None => false,
        };
        if !blind {
            log(&m, "debug", || Ok(text(py, "🎬 No toggle element and no player evidence - not tapping blind")))?;
            return Ok(());
        }
        // Fallback: raw tap on the left of the screen (pre-toggle players)
        let tapped = (|| -> PyResult<()> {
            let url = || fstring(py, &[&slf.getattr("wda_url")?, &text(py, "/session/"), &session, &text(py, "/actions")]);
            let actions = py.import("json")?.call_method1("loads", (TAP_ACTIONS,))?;
            post(py, url, &actions, 5)?;
            Ok(())
        })();
        match tapped {
            Err(e) if !is_exception(py, &e) => Err(e),
            _ => Ok(()),
        }
    }

    // ---------------------------------------------------------------- state

    #[doc = "Read playback state from the source tree.\n\nReturns {'playing': True|False|None, 'progress': str|None} or None if\nthe dump failed. 'playing' comes from WHICH transport button the tree\nholds - reliable regardless of overlay visibility. 'progress' is an\nopaque advancing string (\"You've watched ...\" or \"m:ss / mm:ss\")."]
    fn _scan_state<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let m = module(py)?;
        let scanned = (|| -> PyResult<Obj<'py>> {
            let get = name(py, "requests")?.getattr("get")?;
            let url = fstring(py, &[&slf.getattr("wda_url")?, &text(py, "/source")])?;
            let response = get.call((url,), Some(&dict(py, &[("timeout", &name(py, "WDA_SOURCE_TIMEOUT")?)])?))?;
            if response.getattr("status_code")?.ne(200)? {
                let status = response.getattr("status_code")?;
                log(&m, "error", || fstring(py, &[&text(py, "❌ Failed to get source: "), &status]))?;
                return Ok(none(py));
            }

            let fromstring = name(py, "ET")?.getattr("fromstring")?;
            let root = fromstring.call1((response.call_method0("json")?.get_item("value")?,))?;
            let nothing = none(py);
            let state = dict(py, &[("playing", &nothing), ("progress", &nothing), ("player_ui", &boolean(py, false))])?;
            let player_names = PyTuple::new(py, ["playerView", "advertisingOverlay"])?;
            let empty = text(py, "");
            let or_empty = |value: Obj<'py>| -> PyResult<Obj<'py>> {
                Ok(if value.is_truthy()? { value } else { text(py, "") })
            };

            for elem in root.call_method0("iter")?.try_iter()? {
                let elem = elem?;
                let elem_type = elem.call_method1("get", ("type", &empty))?;
                let label = or_empty(elem.call_method1("get", ("label", &empty))?)?;
                let name_ = or_empty(elem.call_method1("get", ("name", &empty))?)?;
                let value = or_empty(elem.call_method1("get", ("value", &empty))?)?;

                // Container-level player markers. During pre-roll ADS the
                // player has no transport buttons and no watched-label at all
                // - playerView/advertisingOverlay (and the exact player close
                // button) are what prove a player is on screen then.
                if player_names.contains(&name_)? {
                    state.set_item("player_ui", true)?;
                }
                if elem_type.eq("XCUIElementTypeButton")?
                    && (name_.eq("closeButton")? || name_.eq("Player_Close_Button")? || label.eq("Close player")?)
                {
                    state.set_item("player_ui", true)?;
                }

                if elem_type.eq("XCUIElementTypeButton")? {
                    let idents = PySet::new(py, [label.call_method0("lower")?, name_.call_method0("lower")?])?;
                    if idents.as_any().bitand(name(py, "_PAUSE_IDENTS")?)?.is_truthy()? {
                        state.set_item("playing", true)?; // pause offered = playing
                    } else if idents.as_any().bitand(name(py, "_PLAY_IDENTS")?)?.is_truthy()? {
                        state.set_item("playing", false)?; // play offered = paused
                    }
                } else if elem_type.eq("XCUIElementTypeStaticText")? {
                    if name_.eq("Player_Progress_Label")? && label.contains("/")? {
                        let search = name(py, "re")?.getattr("search")?;
                        let head = label.call_method1("split", ("/",))?.get_item(0)?;
                        let time_match = search.call1((PROGRESS_PATTERN, head))?;
                        if time_match.is_truthy()? {
                            state.set_item("progress", time_match.call_method1("group", (1,))?)?;
                        }
                    } else if !state.as_any().get_item("progress")?.is_truthy()? {
                        // Sky Go player: timeLabel "You've watched 15 Minutes
                        // 51 Seconds". Anchored on "you've watched" (loose
                        // apostrophe - straight vs curly varies), because the
                        // SHOW page has its own "Watched 19 Minutes" label:
                        // matching bare "watched" made close() believe the
                        // player was still open after it had really closed.
                        // "Running time ..." (static total) never matches.
                        let shown = if label.is_truthy()? { label.clone() } else { or_empty(value.clone())? };
                        let text_ = shown.call_method0("strip")?;
                        let low = text_.call_method0("lower")?;
                        let search = name(py, "re")?.getattr("search")?;
                        if search.call1((WATCHED_PATTERN, low))?.is_truthy()? {
                            state.set_item("progress", text_)?;
                        }
                    }
                }
            }

            log(&m, "debug", || {
                let playing = state.as_any().get_item("playing")?;
                let progress = state.as_any().get_item("progress")?.repr()?.into_any();
                fstring(py, &[&text(py, "🎬 Player state: playing="), &playing, &text(py, ", progress="), &progress])
            })?;
            Ok(state.into_any())
        })();
        match scanned {
            Ok(state) => Ok(state),
            Err(e) => {
                log_failure(&m, "❌ Video scan error: ", e)?;
                Ok(none(py))
            }
        }
    }

    // -------------------------------------------------------------- actions

    #[doc = "Shared play/pause driver, symmetric by design.\n\nCheck the tree's transport-button state; if already right, done. Else\nelement-click the wanted button and read the state back. A click that\nchanged nothing means the overlay was visually hidden and the gesture\nlayer ate it - reveal via the toggle element and retry ONCE."]
    fn _set_playback<'py>(slf: &Bound<'py, Self>, want_playing: &Obj<'py>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let verb = text(py, if want_playing.is_truthy()? { "play" } else { "pause" });
        let predicate = name(py, if want_playing.is_truthy()? { "PLAY_PREDICATE" } else { "PAUSE_PREDICATE" })?;
        let confirmed = |state: &Obj<'py>| -> PyResult<bool> {
            Ok(state.is_truthy()? && state.get_item("playing")?.is(want_playing))
        };

        for _attempt in 0..2 {
            // full pass + one retry
            let mut state = slf.getattr("_scan_state")?.call0()?;
            if confirmed(&state)? {
                say(py, "info", &[&text(py, "✅ "), &verb, &text(py, ": confirmed")])?;
                return Ok(boolean(py, true));
            }

            // Up to two clicks on the transport element, verifying between.
            // This is parity-proof against the invisible overlay state: on a
            // VISIBLE overlay click 1 actuates; on a hidden one the gesture
            // layer eats click 1 - which itself reveals the overlay - and
            // click 2 actuates. Never insert a toggle-click before this: the
            // eaten click already toggled, and toggling again just re-hides
            // the controls (measured live - that ordering fails every time).
            for click_round in 0..2_i64 {
                let session = slf.getattr("get_session")?.call0()?; // one session per round,
                if !session.is_truthy()? {
                    // shared by find AND click
                    break;
                }
                let element_id = slf.getattr("_find")?.call1((&session, &predicate))?; // re-find: ids go stale
                if !element_id.is_truthy()? || !slf.getattr("_click")?.call1((&session, &element_id))?.is_truthy()? {
                    break;
                }
                let round = (click_round + 1).into_pyobject(py)?.into_any();
                say(py, "info", &[&text(py, "🎬 Clicked "), &verb, &text(py, " button (round "), &round, &text(py, ")")])?;
                sleep(py, "CLICK_SETTLE_SECONDS")?;
                state = slf.getattr("_scan_state")?.call0()?;
                if confirmed(&state)? {
                    say(py, "info", &[&text(py, "✅ "), &verb, &text(py, ": confirmed")])?;
                    return Ok(boolean(py, true));
                }
            }

            // Transport element missing or clicks refused: reveal and go
            // around once more. Blind coordinate taps only when the tree
            // proved a player is up.
            say(py, "debug", &[&text(py, "🎬 "), &verb, &text(py, " not confirmed, revealing controls")])?;
            let player_seen = state.is_truthy()?
                && (!state.get_item("playing")?.is_none() || state.get_item("progress")?.is_truthy()?);
            let reveal = slf.getattr("_reveal_controls")?;
            reveal.call((), Some(&dict(py, &[("allow_blind_tap", &boolean(py, player_seen))])?))?;
            sleep(py, "REVEAL_SETTLE_SECONDS")?;
        }

        say(py, "error", &[&text(py, "❌ Failed to "), &verb])?;
        Ok(boolean(py, false))
    }

    /// Play the video
    fn play<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        match slf.getattr("_set_playback").and_then(|set| set.call1((true,))) {
            Ok(done) => Ok(done),
            Err(e) => {
                log_failure(&module(py)?, "Video play error: ", e)?;
                Ok(boolean(py, false))
            }
        }
    }

    /// Pause the video
    fn pause<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        match slf.getattr("_set_playback").and_then(|set| set.call1((false,))) {
            Ok(done) => Ok(done),
            Err(e) => {
                log_failure(&module(py)?, "Video pause error: ", e)?;
                Ok(boolean(py, false))
            }
        }
    }

    #[doc = "Check if video is currently streaming/playing.\n\nGround truth is the progress label advancing between two samples;\ntransport-button state is the fallback for players without one."]
    fn check_streaming<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let yes = boolean(py, true);
        let checked = (|| -> PyResult<Obj<'py>> {
            let state1 = slf.getattr("_scan_state")?.call0()?;
            if !state1.is_truthy()? {
                return Ok(boolean(py, false));
            }
            if !state1.get_item("progress")?.is_truthy()? {
                return Ok(boolean(py, state1.get_item("playing")?.is(&yes)));
            }

            sleep(py, "PROGRESS_SAMPLE_SECONDS")?;

            let state2 = slf.getattr("_scan_state")?.call0()?;
            if !state2.is_truthy()? || !state2.get_item("progress")?.is_truthy()? {
                // bool(state2) and state2['playing'] is True
                if !state2.is_truthy()? {
                    return Ok(boolean(py, false));
                }
                return Ok(boolean(py, state2.get_item("playing")?.is(&yes)));
            }
            state1.get_item("progress")?.rich_compare(state2.get_item("progress")?, CompareOp::Ne)
        })();
        match checked {
            Ok(streaming) => Ok(streaming),
            Err(e) => {
                log_failure(&module(py)?, "Video streaming check error: ", e)?;
                Ok(boolean(py, false))
            }
        }
    }

    #[doc = "Is the full-screen player up at all? Three-valued like _is_playing:\n\nTrue  - a player marker is in the tree (transport button, progress\n        label, playerView / ad overlay / player close button, or the\n        overlay-toggle element)\nFalse - the tree scanned fine and holds no player markers\nNone  - the scan failed, so no verdict either way\n\nEvery marker vanishes once the player screen is dismissed, which is\nwhat makes this the close() verification."]
    fn _player_present<'py>(slf: &Bound<'py, Self>) -> PyResult<Obj<'py>> {
        let py = slf.py();
        let state = slf.getattr("_scan_state")?.call0()?;
        if state.is_none() {
            return Ok(none(py));
        }
        if !state.get_item("playing")?.is_none()
            || state.get_item("progress")?.is_truthy()?
            || state.get_item("player_ui")?.is_truthy()?
        {
            return Ok(boolean(py, true));
        }
        let session = slf.getattr("get_session")?.call0()?;
        if session.is_truthy()? {
            let find = slf.getattr("_find")?;
            if find.call1((&session, name(py, "TOGGLE_PREDICATE")?))?.is_truthy()? {
                return Ok(boolean(py, true));
            }
        }
        Ok(boolean(py, false))
    }

    #[doc = "Close the video player.\n\nSame parity dance as play/pause: the close button stays in the tree\nwhile the overlay is visually hidden, so the first element click can\nbe eaten by the gesture layer (HTTP 200, nothing closed) and only\nreveals the controls. Click, VERIFY the player is really gone, and\nclick once more if it is not - never trust the click alone.\n\nThree rounds, not two: a click landing mid-fade can be swallowed\noutright (no toggle, no actuation - observed live), and with only two\nrounds one swallowed click burns the whole budget."]
    fn close(slf: &Bound<'_, Self>) -> PyResult<bool> {
        let py = slf.py();
        let m = module(py)?;
        let no = boolean(py, false);
        let yes = boolean(py, true);
        let closed = (|| -> PyResult<bool> {
            for attempt in 0..3_i64 {
                let present = slf.getattr("_player_present")?.call0()?;
                if present.is(&no) {
                    log(&m, "info", || Ok(text(py, "✅ Video player closed")))?;
                    return Ok(true);
                }

                let session = slf.getattr("get_session")?.call0()?;
                let mut clicked = false;
                // exact names before loose
                let predicates = if session.is_truthy()? { name(py, "CLOSE_PREDICATES")? } else { PyList::empty(py).into_any() };
                for predicate in predicates.try_iter()? {
                    let predicate = predicate?;
                    let element_id = slf.getattr("_find")?.call1((&session, &predicate))?;
                    if element_id.is_truthy()? && slf.getattr("_click")?.call1((&session, &element_id))?.is_truthy()? {
                        clicked = true;
                        break;
                    }
                }
                if clicked {
                    let round = (attempt + 1).into_pyobject(py)?.into_any();
                    say(py, "info", &[&text(py, "🎬 Clicked close button (attempt "), &round, &text(py, ")")])?;
                    sleep(py, "CLICK_SETTLE_SECONDS")?;
                    // the loop re-verifies; an eaten click revealed the
                    // overlay, so the next round's click lands
                    continue;
                }

                // No close button clickable: reveal and go around once more
                let reveal = slf.getattr("_reveal_controls")?;
                reveal.call((), Some(&dict(py, &[("allow_blind_tap", &boolean(py, present.is(&yes)))])?))?;
                sleep(py, "REVEAL_SETTLE_SECONDS")?;
            }

            // Verdict after the last click settles
            if slf.getattr("_player_present")?.call0()?.is(&no) {
                log(&m, "info", || Ok(text(py, "✅ Video player closed")))?;
                return Ok(true);
            }
            log(&m, "error", || Ok(text(py, "❌ Failed to close video player")))?;
            Ok(false)
        })();
        match closed {
            Ok(done) => Ok(done),
            Err(e) => {
                log_failure(&m, "❌ Video close error: ", e)?;
                Ok(false)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Its part of the module
// ---------------------------------------------------------------------------

/// Fills the module that takes videoplayer.py's place: its docstring, its
/// constants (made here, as the .py made them at import) and the class.
/// tool/__init__.py binds the imports and the logger when the module is
/// first imported (see the top of this file).
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let python_name = m.name()?;
    m.setattr("__doc__", MODULE_DOC)?;
    // (connect, read) seconds for the WebDriverAgent source dump: same bound the
    // scanner uses at AutoCua/ios/tree/element.py. Without it `requests` blocks
    // forever, and a wedged WDA takes the whole agent down with it.
    m.add("WDA_SOURCE_TIMEOUT", PyTuple::new(py, [5, 60])?)?;
    // After clicking a transport button: how long before reading the state back.
    // The name flip (pauseButton <-> playButton) is immediate; 1s absorbs WDA lag.
    m.add("CLICK_SETTLE_SECONDS", 1.0)?;
    // After revealing the overlay: the fade-in animation, measured working at 0.7s.
    m.add("REVEAL_SETTLE_SECONDS", 0.7)?;
    // Seconds between the two progress-label samples that prove playback for the
    // streaming check. The label ticks every second; 4s is comfortably past any
    // rounding.
    m.add("PROGRESS_SAMPLE_SECONDS", 4)?;
    m.add("PLAY_PREDICATE", PLAY_PREDICATE)?;
    m.add("PAUSE_PREDICATE", PAUSE_PREDICATE)?;
    m.add("TOGGLE_PREDICATE", TOGGLE_PREDICATE)?;
    m.add("CLOSE_PREDICATES", PyList::new(py, [CLOSE_EXACT, CLOSE_LOOSE])?)?;
    // Button names/labels that reveal the playback state through the tree.
    m.add("_PAUSE_IDENTS", PySet::new(py, ["pausebutton", "player_pause_button", "pause"])?)?;
    m.add("_PLAY_IDENTS", PySet::new(py, ["playbutton", "player_play_button", "play"])?)?;
    m.add_class::<VideoPlayerService>()?;
    let class = py.get_type::<VideoPlayerService>();
    as_python_class(&class, &python_name)?;
    class.setattr("__doc__", CLASS_DOC)?;
    let _ = MODULE.set(py, m.clone().unbind());
    Ok(())
}
