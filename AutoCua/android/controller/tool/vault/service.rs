//! The vault: the copy of AutoCua/vault/service.py (the iPhone's) for the
//! Android agent. The user's logins live in the "android" section of
//! AutoCua_data/vault/credentials.json (one file, a section per device),
//! keyed by the app's package, the name the Android tree gives the app in
//! front: "android": {"com.bskyb.skygo": {"username": "...", "password":
//! "...", "pin": "1234"}}. The file is read fresh each time, so an entry
//! saved during a run is used at once. Unlike iOS, the phone's own
//! lock-screen PIN is not entered: the home screen and System UI also show
//! widgets and notifications, which can draw ten digit keys and a press count
//! of their own, and Android's real lock screen shows no count to check
//! against (see the controller's enter_vault_pin).
//!
//! The model only ever sees key NAMES (`block`), and `vault {"id": 5,
//! "value": "password"}` puts the saved value into [5] the way `input` types
//! (`fill`). The value is never in a result, and every value the vault typed
//! this chat shows as [vault] wherever the phone shows it back: the loop runs
//! the shared hide (AutoCua/vault/service.py) over the tree and the tool
//! results, and `fill` adds the value to it before it goes out. The value
//! reaches the phone only through the helper (Helper.java's fill), which
//! sets it on the focused field after its own checks: never through a
//! process list or an adb error.
//!
//! PIN keypads, vault {"id": 0, "value": "pin"}: some PIN screens have
//! nothing to type into (an app's PIN pad of ten keys), so the vault taps the
//! PIN itself and the model never sees it. On a moto g77 (Android 16) Sky
//! Go's "Sky Device PIN" keys are buttons "1".."0", and a statictext counts
//! the presses, "Device PIN fields, 0 of 4 values entered", as the iPhone's
//! passcode field does. Anything that maps to a digit is as secret as the
//! PIN: a key's element id, its coordinates, which press failed. None of it
//! is logged or returned.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use pyo3::prelude::*;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::agent::android_use::view::{py_str_of, truthy};
use crate::controller::service::{controller_service, py_int};
use crate::tree::element::{fill_focused, is_input, read_tree, FillError};

/// One digit, optionally with its phone letters ("2 ABC", "2, A B C", "0 +").
/// Phone letters are uppercase, so "1 of 3" and "5 min" do not match.
fn pin_key_label() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*([0-9])(?:[\s,]+[A-Z+][A-Z+ ]{0,8})?\s*$").unwrap())
}

/// The PIN pad's count of presses: "4 of 6 values entered" (the iPhone's
/// passcode field), "Device PIN fields, 0 of 4 values entered" (Sky Go).
fn pin_entry_count() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([0-9]+) of ([0-9]+) values entered").unwrap())
}

/// After the last key, before reading the screen back.
const PIN_SETTLE: Duration = Duration::from_secs(1);

/// Tries per PIN entry per task. A pad that closes resets it; pads lock
/// after a few wrong tries.
pub const MAX_PIN_ATTEMPTS: u32 = 2;

/// Vault entries for the device itself (iOS's lock-screen PIN). Never an
/// app's entry; on Android the controller refuses them before any tap.
pub const DEVICE_ENTRIES: [&str; 1] = ["android"];

/// The fields a saved value may go into (the controller mapping's type).
pub const TEXT_FIELDS: [&str; 4] = ["TextField", "SecureTextField", "TextView", "SearchField"];

/// System UI: the lock screen and the notification shade, no app in front
/// (the iPhone's SpringBoard).
const SYSTEM_UI: &str = "com.android.systemui";

/// The digit a key's label names, if it is a PIN key.
fn pin_key(name: &str) -> Option<char> {
    pin_key_label().captures(name).and_then(|c| c.get(1)).and_then(|m| m.as_str().chars().next())
}

/// A box of the controller's mapping: x, y, width, height.
#[derive(Clone, Copy)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn bounds_of(b: &Value) -> Option<Bounds> {
    let num = |k: &str| b.get(k).and_then(Value::as_f64);
    Some(Bounds { x: num("x")?, y: num("y")?, width: num("width")?, height: num("height")? })
}

fn centre(b: &Bounds) -> (f64, f64) {
    (b.x + b.width / 2.0, b.y + b.height / 2.0)
}

fn inside(point: (f64, f64), b: &Bounds) -> bool {
    let (x, y) = point;
    b.x <= x && x <= b.x + b.width && b.y <= y && y <= b.y + b.height
}

/// A fresh read of the screen around a PIN entry.
struct PinScreen {
    /// The pad's count of presses (entered, total), when it shows one.
    count: Option<(u64, u64)>,
    /// All ten digit keys are on screen.
    keypad_open: bool,
    /// The package of the window in front (the helper writes it first).
    front: String,
}

/// The screen as it is now, read for the PIN: its count, its keys, and the
/// app in front.
fn read_pin_screen(py: Python<'_>) -> Result<PinScreen, String> {
    let xml = py.detach(read_tree)?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| e.to_string())?;
    let mut count = None;
    let mut digits = HashSet::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("node")) {
        for label in [node.attribute("text"), node.attribute("content-desc")].into_iter().flatten() {
            if count.is_none() {
                if let Some(c) = pin_entry_count().captures(label) {
                    if let (Ok(entered), Ok(total)) = (c[1].parse(), c[2].parse()) {
                        count = Some((entered, total));
                    }
                }
            }
            if let Some(digit) = pin_key(label) {
                digits.insert(digit);
            }
        }
    }
    let front = doc
        .root_element()
        .children()
        .find(|n| n.has_tag_name("node"))
        .and_then(|n| n.attribute("package"))
        .unwrap_or("")
        .to_string();
    Ok(PinScreen { count, keypad_open: digits.len() == 10, front })
}

/// Tap each key, one after another: one `input tap` each, ~0.2 s apart,
/// which also keeps a repeated digit (0000) from reading as a double tap.
/// The error says nothing: the points are the PIN.
fn tap_pin_keys(py: Python<'_>, points: &[(f64, f64)]) -> Result<(), ()> {
    py.detach(|| {
        for (x, y) in points {
            controller_service().tap(*x as i64, *y as i64).map_err(|_| ())?;
        }
        Ok(())
    })
}

/// A saved value as the text to type: a non-empty string, or a whole number
/// (a pin saved as 1234). Anything else is not typed.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The keys of an entry that hold something to type, as the file writes them.
fn usable_keys(creds: &Map<String, Value>) -> Vec<String> {
    creds.iter().filter(|(_, v)| text_of(v).is_some()).map(|(k, _)| k.clone()).collect()
}

fn norm(name: &str) -> String {
    name.to_lowercase().replace(' ', "")
}

/// Hide `secret` from here on, in the forms a screen shows it back in (the
/// shared list the loop's hide reads). Added before the value goes out,
/// since a fill that fails halfway may have set it.
fn remember(py: Python<'_>, secret: &str) -> PyResult<()> {
    let shared = py.import("AutoCua.vault.service")?;
    shared.call_method1("_remember", (secret,))?;
    // The tree writes a double quote as ' (tree/element.rs clean): that
    // form too, with the variants the shared list makes of it.
    if secret.contains('"') {
        shared.call_method1("_remember", (secret.replace('"', "'"),))?;
    }
    Ok(())
}

/// The focused view's class in the tree's own type words (tree/element.rs's
/// is_input rule): an EditText or AutoCompleteTextView is a textfield,
/// anything else its own class.
fn kind_word(class: &str) -> String {
    let simple = class.rsplit('.').next().unwrap_or(class);
    if is_input(class) {
        "textfield".to_string()
    } else if simple.is_empty() {
        "element".to_string()
    } else {
        simple.to_lowercase()
    }
}

/// What enter_pin did: `pressed` says a wrong-PIN try may have been spent,
/// `closed` that the PIN screen went away.
pub struct PinOutcome {
    pub status: &'static str,
    pub message: String,
    pub pressed: bool,
    pub closed: bool,
}

fn outcome(status: &'static str, message: impl Into<String>, pressed: bool, closed: bool) -> PinOutcome {
    PinOutcome { status, message: message.into(), pressed, closed }
}

/// Service for managing and retrieving credentials.
pub struct VaultService {
    /// The tree text of the last scan, which names the app in front.
    element_tree_text: Mutex<String>,
    /// The "android" section of credentials.json, as last read.
    credentials: Mutex<Map<String, Value>>,
    /// The last scan succeeded: until then the vault types into nothing
    /// (begin_scan, update_element_tree).
    scan_ok: AtomicBool,
}

pub fn vault_service() -> &'static VaultService {
    static S: OnceLock<VaultService> = OnceLock::new();
    S.get_or_init(|| VaultService {
        element_tree_text: Mutex::new(String::new()),
        credentials: Mutex::new(Map::new()),
        scan_ok: AtomicBool::new(false),
    })
}

impl VaultService {
    /// Load credentials from the JSON file: the "android" section.
    ///
    /// keep_on_error: a reload while the app runs must not wipe what normal
    /// fills use when it catches a half-saved or mistyped file.
    fn load_credentials(&self, py: Python<'_>, keep_on_error: bool) {
        let loaded = (|| -> PyResult<Map<String, Value>> {
            // AutoCua_data/vault/credentials.json: outside the install
            // folder, so uninstalling AutoCua can't delete the user's
            // credentials.
            let path: String = py.import("AutoCua")?.call_method0("vault_file")?.str()?.extract()?;
            if !Path::new(&path).exists() {
                return Ok(Map::new());
            }
            let utf8 = pyo3::types::PyDict::new(py);
            utf8.set_item("encoding", "utf-8")?;
            let text = py.import("pathlib")?.getattr("Path")?.call1((path,))?.call_method("read_text", (), Some(&utf8))?;
            let loaded: Value = pythonize::depythonize(&py.import("json")?.call_method1("loads", (text,))?)?;
            let Value::Object(mut loaded) = loaded else {
                return Err(pyo3::exceptions::PyValueError::new_err("credentials.json must hold an object"));
            };
            // One file for every device, a section each ("ios", "web",
            // "android"): Android reads only its own, so another device's
            // key can never match an app here.
            match loaded.remove("android").unwrap_or_else(|| json!({})) {
                Value::Object(section) => Ok(section),
                _ => Err(pyo3::exceptions::PyValueError::new_err(
                    "credentials.json's \"android\" must hold an object",
                )),
            }
        })();
        match loaded {
            Ok(section) => *self.credentials.lock().unwrap() = section,
            Err(e) => {
                eprintln!("Error loading credentials: {}", e.value(py));
                if !keep_on_error {
                    self.credentials.lock().unwrap().clear();
                }
            }
        }
    }

    /// A scan starts: until it succeeds (update_element_tree) the vault types
    /// into nothing, as on iOS, since the tree it holds may no longer be the
    /// screen.
    pub fn begin_scan(&self) {
        self.scan_ok.store(false, Ordering::SeqCst);
    }

    /// The tree a scan just read (successfully), which names the app in front.
    pub fn update_element_tree(&self, element_tree_text: &str) {
        *self.element_tree_text.lock().unwrap() = element_tree_text.to_string();
        self.scan_ok.store(true, Ordering::SeqCst);
    }

    /// (app name, device) of the screen last scanned: the app's package from
    /// the current_application header (the [1] line cuts a long name to 47
    /// characters), "" on the home screen and in System UI (the lock screen
    /// and the notification shade), and the current_device header ("Android"
    /// when it is missing).
    fn scanned_app(&self) -> (String, String) {
        let tree = self.element_tree_text.lock().unwrap();
        let (mut app_name, mut device, mut header) = (String::new(), "Android".to_string(), None);
        for line in tree.lines() {
            if let Some(rest) = line.strip_prefix("current_device:") {
                if let Some(word) = rest.split_whitespace().next() {
                    device = word.to_string();
                }
            } else if let Some(rest) = line.strip_prefix("current_application:") {
                header = Some(rest.trim().to_string());
            } else if line.trim_start().starts_with("[1]<") && line.contains("type=\"application\"") {
                app_name = parse_element_line(line).remove("label").unwrap_or_default().trim().to_string();
                break;
            }
        }
        if let Some(header) = header {
            app_name = header;
        }
        if app_name == "home screen" || app_name == SYSTEM_UI {
            app_name.clear();
        }
        (app_name, device)
    }

    /// (entry name, its keys) saved for `app_name`: its exact name, case and
    /// spaces ignored. Never a device entry: those hold the lock-screen PIN,
    /// which only get_pin reads.
    fn app_entry(&self, app_name: &str) -> Option<(String, Map<String, Value>)> {
        let wanted = norm(app_name);
        if wanted.is_empty() || DEVICE_ENTRIES.contains(&wanted.as_str()) {
            return None;
        }
        let creds = self.credentials.lock().unwrap();
        creds.iter().find_map(|(key, entry)| match entry {
            Value::Object(entry) if norm(key) == wanted => Some((key.clone(), entry.clone())),
            _ => None,
        })
    }

    /// <vault>: the key names saved for the app just scanned, or the device's
    /// PIN where no app is in front. Empty when nothing is saved.
    pub fn block(&self, py: Python<'_>) -> String {
        self.load_credentials(py, true);
        let (app_name, _) = self.scanned_app();
        let (name, keys) = if let Some((name, creds)) = self.app_entry(&app_name) {
            (name, usable_keys(&creds))
        } else {
            // No app in front: iOS offers the phone's own PIN here. Not on
            // Android yet (see enter_vault_pin): nothing to offer.
            return String::new();
        };
        if keys.is_empty() {
            return String::new();
        }
        format!("<vault app=\"{name}\">\n{}\n</vault>", keys.join(", "))
    }

    /// vault {"id", "value": <key>}: put the value saved under `key` for the
    /// app in front into [id], like `input`. The result names the key, never
    /// the value. A PIN keypad is enter_pin's, not this.
    pub fn fill(&self, py: Python<'_>, element_id: &Value, key: &Value) -> PyResult<Value> {
        let key = if truthy(key) { py_str_of(key).trim().to_string() } else { String::new() };
        let result = json!({"action": "vault", "id": element_id, "value": key});
        let refuse = |message: String| {
            let mut out = result.clone();
            out["status"] = json!("error");
            out["message"] = json!(message);
            out
        };

        let Ok(idx) = py_int(element_id) else {
            return Ok(refuse(format!("'{}' is not a valid element id", py_str_of(element_id))));
        };
        let Some(elem) = controller_service().elements().get(&idx.to_string()).filter(|e| truthy(e)).cloned() else {
            return Ok(refuse(format!(
                "no element [{idx}] in the current element_tree. Ids are re-assigned on every scan - re-read the latest tree and use an id from it."
            )));
        };
        if key.is_empty() {
            return Ok(refuse(
                "vault's `value` is the name of a key in <vault>, like \"username\" or \"password\"".to_string(),
            ));
        }
        self.load_credentials(py, true);
        let (app_name, _) = self.scanned_app();
        let Some((entry, creds)) = self.app_entry(&app_name) else {
            let place = if app_name.is_empty() { "the home screen" } else { app_name.as_str() };
            return Ok(refuse(format!(
                "nothing is saved in the vault for {place}: use what the task gives, or ask the user"
            )));
        };
        let wanted = key.to_lowercase();
        let Some(name) = creds.keys().find(|k| k.trim().to_lowercase() == wanted).cloned() else {
            return Ok(refuse(format!(
                "no \"{key}\" is saved for {entry}: <vault> has {}",
                usable_keys(&creds).join(", ")
            )));
        };
        let Some(secret) = text_of(&creds[&name]) else {
            return Ok(refuse(format!(
                "the \"{name}\" saved for {entry} is empty or not text: ask the user to fix it under \"android\" in AutoCua_data/vault/credentials.json"
            )));
        };
        let password = name.to_lowercase().contains("password");
        let kind = elem.get("type").filter(|t| truthy(t)).map(py_str_of).unwrap_or_else(|| "element".to_string());
        if !TEXT_FIELDS.contains(&kind.as_str()) {
            return Ok(refuse(format!("[{idx}] is a {}, not a text field", kind.to_lowercase())));
        }
        if password && kind != "SecureTextField" {
            return Ok(refuse(format!(
                "[{idx}] is a {}, and a saved password goes only into a password field",
                kind.to_lowercase()
            )));
        }

        // The last scan failed: the tree and the boxes may no longer be the
        // screen, so nothing is tapped or typed (iOS refuses in its check).
        if !self.scan_ok.load(Ordering::SeqCst) {
            return Ok(refuse(
                "nothing typed: the app that was scanned is no longer in front, or a system screen is up".to_string(),
            ));
        }

        remember(py, &secret)?;
        // The focus tap, as `input` makes it, then the helper sets the text
        // once the focused field passes its checks (the iPhone's
        // type_text with the vault's check).
        let failed = "the typing failed on the phone, and part of the value may be in the field: check the next screenshot";
        if controller_service().click(&json!(idx)).is_err() {
            return Ok(refuse(failed.to_string()));
        }
        py.detach(|| std::thread::sleep(Duration::from_millis(100))); // Small delay for focus
        match py.detach(|| fill_focused(password, &app_name, &secret)) {
            Ok(()) => {}
            Err(FillError::Refused { why, class }) => {
                let problem = match why.as_str() {
                    "no_focus" => "no field took focus after the tap".to_string(),
                    "not_password" => format!(
                        "focus is on a {}, and a saved password goes only into a password field",
                        kind_word(&class)
                    ),
                    // A field the app made read-only still reads as a
                    // textfield in the tree.
                    "not_text" => match kind_word(&class).as_str() {
                        "textfield" => "focus is on a read-only textfield".to_string(),
                        other => format!("focus is on a {other}, not a text field"),
                    },
                    _ => "the app that was scanned is no longer in front, or a system screen is up".to_string(),
                };
                return Ok(refuse(format!("nothing typed: {problem}")));
            }
            // Never the error's own text: it could carry what was typed.
            Err(FillError::Failed) => return Ok(refuse(failed.to_string())),
        }
        let label = elem.get("name").filter(|n| truthy(n)).map(py_str_of).unwrap_or_default().trim().to_string();
        let target = if !label.is_empty() && label != "no_label" { format!("[{idx}] ({label})") } else { format!("[{idx}]") };
        let mut out = result;
        out["status"] = json!("success");
        out["message"] = json!(format!("filled {target} with the saved {name} for {entry}"));
        Ok(out)
    }

    /// (entry_name, pin) for the screen in front, for vault {"id": 0, "value": "pin"}.
    ///
    /// Careful, because a PIN pad locks after a few wrong tries:
    ///   * re-reads credentials.json, so a PIN added while the app runs is used
    ///   * matches the app's package EXACTLY (case and spaces ignored), like `fill`
    ///   * reads only "pin", never "code", and only if it is all digits
    /// Screens with no app in front (home screen, lock screen) name the entry
    /// after the device, "Android", which the controller refuses (the phone's
    /// own PIN is not entered on Android). pin is None when nothing usable is
    /// saved; entry_name still says where to add it.
    pub fn get_pin(&self, py: Python<'_>) -> (String, Option<String>) {
        self.load_credentials(py, true);
        let (app_name, device) = self.scanned_app();
        let name = if app_name.is_empty() { device } else { app_name };
        let wanted = norm(&name);
        let creds = self.credentials.lock().unwrap();
        for (app_key, app_creds) in creds.iter() {
            if let (true, Value::Object(app_creds)) = (norm(app_key) == wanted, app_creds) {
                let pin = match app_creds.get("pin") {
                    Some(v) if truthy(v) => py_str_of(v),
                    _ => String::new(),
                };
                let digits = !pin.is_empty() && pin.chars().all(|c| c.is_ascii_digit());
                return (name, digits.then_some(pin));
            }
        }
        (name, None)
    }

    /// {digit: (x, y)} for a full on-screen digit keypad, or None.
    ///
    /// `elements` is the controller's mapping from the latest scan. All ten
    /// digits must be there, each resolving to exactly one key: checking only
    /// the PIN's own digits would happily tap a "1" badge four times for 1111.
    pub fn find_pin_keys(&self, elements: &Map<String, Value>) -> Option<HashMap<char, (f64, f64)>> {
        let mut candidates: HashMap<char, Vec<Bounds>> = HashMap::new();
        for elem in elements.values() {
            let digit = elem.get("name").and_then(Value::as_str).and_then(pin_key);
            let bounds = elem.get("bounds").filter(|b| truthy(b)).and_then(bounds_of);
            if let (Some(digit), Some(bounds)) = (digit, bounds) {
                candidates.entry(digit).or_default().push(bounds);
            }
        }

        let mut keys = HashMap::new();
        for digit in "0123456789".chars() {
            let mut boxes = candidates.get(&digit).cloned().unwrap_or_default();
            // Outermost first: a label whose centre sits inside a kept key is
            // that same key, not a second one.
            boxes.sort_by(|a, b| (b.width * b.height).partial_cmp(&(a.width * a.height)).unwrap_or(std::cmp::Ordering::Equal));
            let mut kept: Vec<Bounds> = Vec::new();
            for b in boxes {
                if !kept.iter().any(|k| inside(centre(&b), k)) {
                    kept.push(b);
                }
            }
            if kept.len() != 1 {
                return None;
            }
            keys.insert(digit, centre(&kept[0]));
        }
        Some(keys)
    }

    /// Tap `pin` on the keypad from find_pin_keys, then confirm it registered.
    /// Messages never contain a digit, a key id or a coordinate.
    pub fn enter_pin(&self, py: Python<'_>, keys: &HashMap<char, (f64, f64)>, pin: &str, device_entry: bool) -> PinOutcome {
        // Re-read the screen right before tapping: the keys come from the
        // scan taken before the model thought, and the pad may be gone by now.
        let Ok(screen) = read_pin_screen(py) else {
            return outcome("error", "Could not read the screen, nothing was pressed. It is safe to try once more.", false, false);
        };
        let before = screen.count;
        if !screen.keypad_open {
            return outcome(
                "error",
                "The PIN keypad is no longer on screen, nothing was pressed. Open the PIN screen again first.",
                false,
                false,
            );
        }
        if device_entry && before.is_none() {
            // The phone's own PIN only ever goes into a PIN field that counts
            // its presses.
            return outcome(
                "error",
                "No Android lock-screen PIN field on screen, so the device PIN was not used. Open the lock-screen PIN first.",
                false,
                false,
            );
        }
        if let Some((entered, total)) = before {
            if entered != 0 || pin.chars().count() as u64 != total {
                return outcome(
                    "error",
                    "The passcode field is not empty, or the saved PIN does not fit this passcode, so nothing was pressed. Ask the user.",
                    false,
                    false,
                );
            }
        }

        let points: Vec<(f64, f64)> = pin.chars().filter_map(|d| keys.get(&d).copied()).collect();
        if points.len() != pin.chars().count() || tap_pin_keys(py, &points).is_err() {
            return outcome(
                "error",
                "The key presses may have gone through. Do not retry - check the next screenshot, and ask the user if unsure.",
                true,
                false,
            );
        }
        py.detach(|| std::thread::sleep(PIN_SETTLE));

        let Ok(after) = read_pin_screen(py) else {
            return outcome(
                "success",
                "PIN keys pressed, but the screen could not be read back. Check the next screenshot before doing anything else.",
                true,
                false,
            );
        };
        // The pad gone while the same app stays in front is not proof it
        // closed: an app's "wrong PIN" dialog is a window of its own, and
        // Android then reports only that window, never the pad behind it.
        // So it is not called closed, and the try counts.
        let (app, _) = self.scanned_app();
        let covered = !device_entry && !app.is_empty() && after.front == app;
        let unknown = || {
            outcome(
                "success",
                "PIN keys pressed. The PIN pad is gone but the app is still in front, so it is not known whether the PIN was accepted. Check the next screenshot: if it says the PIN was wrong, do not retry - ask the user.",
                true,
                false,
            )
        };

        if before.is_some() {
            let Some((entered, total)) = after.count else {
                if covered {
                    return unknown();
                }
                // The count is gone with the pad. The lock screen's wording
                // only for the phone's own PIN: an app's pad (Sky Go counts
                // its presses here) just closed.
                if !device_entry {
                    return outcome("success", "PIN entered and the keypad closed.", true, true);
                }
                return outcome(
                    "success",
                    "The passcode screen closed. Confirm on the next screenshot that the device unlocked.",
                    true,
                    true,
                );
            };
            if entered == total {
                return outcome(
                    "success",
                    format!("PIN submitted, every key press registered ({entered} of {total}). Confirm on the next screenshot that it was accepted."),
                    true,
                    false,
                );
            }
            if entered == 0 {
                // The field was empty and the PIN fills it exactly, so a reset
                // to 0 means the PIN was rejected (a try was spent).
                return outcome(
                    "error",
                    "The PIN was not accepted. Do not retry - ask the user to check the PIN saved in the vault.",
                    true,
                    false,
                );
            }
            return outcome(
                "error",
                format!("Only part of the PIN registered ({entered} of {total} values entered). Do not retry - ask the user."),
                true,
                false,
            );
        }

        if !after.keypad_open {
            if covered {
                return unknown();
            }
            return outcome("success", "PIN entered and the keypad closed.", true, true);
        }
        // No entry count to read, and the pad is still up after the whole PIN:
        // it was not accepted, or a press did not land. Never call that success.
        outcome(
            "error",
            "The keypad is still open after the whole PIN: it was not accepted or a press did not register. Do not retry - if the boxes are full and an OK/Continue button shows, tap it; otherwise ask the user.",
            true,
            false,
        )
    }
}

/// An element line's attributes: type, label (element_name), value.
fn parse_element_line(line: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    let field = |key: &str| -> Option<String> {
        let start = line.find(key)? + key.len();
        let end = line[start..].find('"').map(|e| start + e).unwrap_or(line.len());
        Some(line[start..end].to_string())
    };
    if let Some(t) = field("type=\"") {
        attrs.insert("type".to_string(), t);
    }
    // element_name= wins; label= is the older spelling.
    if let Some(label) = field("element_name=\"").or_else(|| field("label=\"")) {
        attrs.insert("label".to_string(), label);
    }
    if let Some(v) = field("value=\"") {
        attrs.insert("value".to_string(), v);
    }
    attrs
}
