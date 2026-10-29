//! service.rs: core phone interaction service, the copy of
//! ios/controller/service.py. Holds ONLY the element interaction primitives
//! (click, type, scroll) plus the shared phone / element-mapping plumbing
//! they need. Everything else lives as a tool in controller/tool/ (open_app,
//! shell, ...).
//!
//! WDA's W3C pointer actions become adb's `input` commands here: a tap, a
//! swipe, key events and text, sent to the phone the scanner connected
//! (set_phone). Each one is one `adb shell` call, 150-300 ms on USB, on any
//! Android version; the same actions through the phone helper
//! (Helper.java, UiAutomation) would take 20 ms and type any text, which is
//! the next step up when it is needed.

use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::agent::android_use::view::py_str_of;

/// Python's int(x): the whole number, or int()'s own complaint.
pub fn py_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .ok_or_else(|| format!("int() argument out of range: {n}")),
        Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: '{s}'")),
        Value::Null => Err("int() argument must be a string, a bytes-like object or a real number, not 'NoneType'".to_string()),
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            if other.is_array() { "list" } else { "dict" }
        )),
    }
}

/// Python's float(x), for the numbers the model sends.
pub fn py_float(v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(n) => n.as_f64().ok_or_else(|| format!("could not convert {n} to float")),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("could not convert string to float: '{s}'")),
        Value::Null => Err("float() argument must be a string or a real number, not 'NoneType'".to_string()),
        other => Err(format!(
            "float() argument must be a string or a real number, not '{}'",
            if other.is_array() { "list" } else { "dict" }
        )),
    }
}

/// The phone the actions go to, as the scanner connected it: the adb
/// binary, the serial, and the density (dp -> px, for the gestures' sizes).
#[derive(Clone, Debug)]
pub struct Phone {
    pub adb: String,
    pub serial: String,
    pub density: f64,
}

/// Service for executing phone actions via adb. One instance for the whole
/// process (Python's module-level `controller_service`): the scanner feeds
/// it the element mapping of every scan, the controller acts on it.
pub struct ControllerService {
    elements: Mutex<Map<String, Value>>,
    phone: Mutex<Option<Phone>>,
    /// The display's size in pixels, in the orientation of the last scan: the
    /// screenshot's frame, which ground_click's 0-1000 points map onto.
    screen: Mutex<Option<(f64, f64)>>,
}

pub fn controller_service() -> &'static ControllerService {
    static S: OnceLock<ControllerService> = OnceLock::new();
    S.get_or_init(|| ControllerService {
        elements: Mutex::new(Map::new()),
        phone: Mutex::new(None),
        screen: Mutex::new(None),
    })
}

/// A box of the element mapping, in display pixels.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

fn bounds_of(elem: &Value) -> Option<Bounds> {
    let b = elem.get("bounds")?;
    let num = |k: &str| b.get(k).and_then(Value::as_f64).map(|f| f.floor() as i64);
    Some(Bounds { x: num("x")?, y: num("y")?, width: num("width")?, height: num("height")? })
}

impl ControllerService {
    /// Update element mapping from scanner.
    pub fn update_elements(&self, elements_mapping: Map<String, Value>) {
        *self.elements.lock().unwrap() = elements_mapping;
    }

    /// The mapping of the last scan, by element number.
    pub fn elements(&self) -> Map<String, Value> {
        self.elements.lock().unwrap().clone()
    }

    /// The phone the scanner connected: where every action goes.
    pub fn set_phone(&self, adb: &str, serial: &str, density: f64) {
        *self.phone.lock().unwrap() = Some(Phone {
            adb: adb.to_string(),
            serial: serial.to_string(),
            density: if density > 0.0 { density } else { 160.0 },
        });
    }

    pub fn phone(&self) -> Option<Phone> {
        self.phone.lock().unwrap().clone()
    }

    /// The display's size in pixels, in the orientation of the scan.
    pub fn set_screen(&self, width: f64, height: f64) {
        *self.screen.lock().unwrap() = Some((width, height));
    }

    pub fn screen(&self) -> Option<(f64, f64)> {
        *self.screen.lock().unwrap()
    }

    /// Pixels per dp on this phone.
    fn dp(&self) -> f64 {
        self.phone().map(|p| p.density / 160.0).unwrap_or(1.0)
    }

    /// `adb -s <serial> shell <args...>`: the command's output, or why it
    /// gave none.
    pub fn adb_shell(&self, args: &[&str]) -> Result<String, String> {
        let phone = self.phone().ok_or("No phone connected")?;
        let out = Command::new(&phone.adb)
            .arg("-s")
            .arg(&phone.serial)
            .arg("shell")
            .args(args)
            .output()
            .map_err(|e| format!("adb: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let said = stderr.trim();
            let said = if said.is_empty() { stdout.trim() } else { said };
            return Err(format!("adb shell {}: {}", args.join(" "), if said.is_empty() { "failed" } else { said }));
        }
        Ok(stdout)
    }

    fn element(&self, elem_num: i64) -> Result<Value, String> {
        let elements = self.elements.lock().unwrap();
        match elements.get(&elem_num.to_string()) {
            Some(e) => Ok(e.clone()),
            None => {
                let keys: Vec<&String> = elements.keys().collect();
                eprintln!("Element #{elem_num} not in loaded elements: {keys:?}");
                Err(format!("Element #{elem_num} not found"))
            }
        }
    }

    /// One tap, as `input tap`. The error names the point: the vault, whose
    /// points are PIN digits, never passes it on.
    pub(crate) fn tap(&self, x: i64, y: i64) -> Result<(), String> {
        self.adb_shell(&["input", "tap", &x.to_string(), &y.to_string()])
            .map(|_| ())
            .map_err(|e| format!("adb tap failed: {e}"))
    }

    /// Click on element by number.
    pub fn click(&self, element_number: &Value) -> Result<(), String> {
        let elem_num = py_int(element_number)?;
        let elem_data = self.element(elem_num)?;
        let bounds = bounds_of(&elem_data).ok_or_else(|| format!("Element #{elem_num} has no bounds"))?;

        // Calculate center point
        let mut center_x = bounds.x + bounds.width / 2;
        let center_y = bounds.y + bounds.height / 2;

        // Some apps report a Switch as its whole row (Google Calendar's "All
        // day" is 390pt wide): a tap on its centre hits the label and does
        // nothing, only the knob at the right end toggles it. A knob is ~63pt
        // wide, so a Switch over 100pt is a row: tap 50pt in from its right
        // edge. Knob-sized switches keep the centre tap. (Points are dp here.)
        let dp = self.dp();
        if elem_data.get("type").and_then(Value::as_str) == Some("Switch") && (bounds.width as f64) > 100.0 * dp {
            center_x = bounds.x + bounds.width - (50.0 * dp).round() as i64;
        }

        self.tap(center_x, center_y).map_err(|e| format!("Click failed: {e}"))
    }

    /// A 0-1000 spot on the screenshot as a display point, and the frame it
    /// was mapped onto: (x, y, (frame x, y, width, height)). The screenshot
    /// is the display, so a fraction of the image is the same fraction of
    /// the display (set_screen); before the first scan the Application
    /// element's box stands in for it, as on iOS.
    fn ground_point(&self, x: &Value, y: &Value, tool: &str) -> Result<(i64, i64, (f64, f64, f64, f64)), String> {
        let (fx, fy) = match (py_float(x), py_float(y)) {
            (Ok(a), Ok(b)) => (a, b),
            _ => {
                return Err(format!("{tool} needs x and y numbers, got ({}, {})", py_str_of(x), py_str_of(y)));
            }
        };
        if !(0.0..=1000.0).contains(&fx) || !(0.0..=1000.0).contains(&fy) {
            return Err(format!("{tool} x and y must be 0-1000, got ({fx}, {fy})"));
        }

        let frame = match *self.screen.lock().unwrap() {
            Some((w, h)) => (0.0, 0.0, w, h),
            None => {
                let elements = self.elements.lock().unwrap();
                let screen = elements
                    .values()
                    .find(|e| e.get("type").and_then(Value::as_str) == Some("Application"))
                    .and_then(bounds_of)
                    .ok_or("No scanned screen to map the point onto")?;
                (screen.x as f64, screen.y as f64, screen.width as f64, screen.height as f64)
            }
        };
        let (ox, oy, sw, sh) = frame;
        Ok(((ox + fx / 1000.0 * sw).round() as i64, (oy + fy / 1000.0 * sh).round() as i64, frame))
    }

    /// Tap a raw point: x and y on a 0-1000 scale of the screenshot.
    ///
    /// For targets with no [id] (one chip inside a wide element, a switch
    /// knob), at the display point ground_point gives.
    pub fn ground_click(&self, x: &Value, y: &Value) -> Result<(), String> {
        let (tap_x, tap_y, _frame) = self.ground_point(x, y, "ground_click")?;
        self.tap(tap_x, tap_y).map_err(|e| format!("Ground click failed: {e}"))
    }

    /// Swipe from a raw point: x and y on the 0-1000 scale of the screenshot
    /// (ground_click's), `direction` the way the finger moves (up, down, left
    /// or right, as scroll: "up" reveals what is below).
    ///
    /// For a list or a pane with no [id]. The swipe is 200 dp, or up to the
    /// display's edge; a point too close to that edge is refused.
    pub fn ground_scroll(&self, x: &Value, y: &Value, direction: &str) -> Result<(), String> {
        let (start_x, start_y, (ox, oy, sw, sh)) = self.ground_point(x, y, "ground_scroll")?;
        let (dx, dy) = match direction.to_lowercase().as_str() {
            "up" => (0, -1),
            "down" => (0, 1),
            "left" => (-1, 0),
            "right" => (1, 0),
            other => return Err(format!("Invalid direction: {other}. Use: up, down, left, or right")),
        };
        let dp = self.dp();
        let margin = (20.0 * dp).round() as i64;
        let distance = (200.0 * dp).round() as i64;
        let end_x = (start_x + dx * distance).max(ox as i64 + margin).min((ox + sw) as i64 - margin);
        let end_y = (start_y + dy * distance).max(oy as i64 + margin).min((oy + sh) as i64 - margin);
        if (end_x - start_x).abs() + (end_y - start_y).abs() < (50.0 * dp).round() as i64 {
            return Err(format!(
                "({}, {}) is too close to the screen's edge to swipe {direction} from",
                py_str_of(x),
                py_str_of(y)
            ));
        }

        self.adb_shell(&[
            "input",
            "swipe",
            &start_x.to_string(),
            &start_y.to_string(),
            &end_x.to_string(),
            &end_y.to_string(),
            "500",
        ])
        .map(|_| ())
        .map_err(|e| format!("Ground scroll failed: {e}"))
    }

    /// Type text into element by number.
    ///
    /// If the element already has text, it is deleted first: a delete key
    /// for each existing character including spaces, from the end of the
    /// text.
    pub fn type_text(&self, element_number: &Value, text: &str) -> Result<(), String> {
        let elem_num = py_int(element_number)?;
        let elem_data = self.element(elem_num)?;

        // Get current value of the element
        let current_value = elem_data.get("value").map(py_str_of).unwrap_or_default();

        // First click on the element to focus it
        self.click(element_number)?;
        std::thread::sleep(Duration::from_millis(100)); // Small delay for focus

        // If element has existing text (not blank), delete it first
        if !current_value.trim().is_empty() {
            // The cursor to the end, then one delete per character.
            let count = current_value.chars().count();
            let mut keys: Vec<String> = vec!["input".into(), "keyevent".into(), "KEYCODE_MOVE_END".into()];
            keys.extend(std::iter::repeat("KEYCODE_DEL".to_string()).take(count));
            let args: Vec<&str> = keys.iter().map(String::as_str).collect();
            if let Err(e) = self.adb_shell(&args) {
                eprintln!("Clear text error: {e}");
            }
            std::thread::sleep(Duration::from_millis(100)); // Small delay after deletion
        }

        // Now type the new text
        if text.is_empty() {
            return Ok(());
        }
        let encoded = adb_text(text).ok_or_else(|| {
            format!("Type failed: adb can only type ASCII text, and this has other characters: {text}")
        })?;
        self.adb_shell(&["input", "text", &encoded])
            .map(|_| ())
            .map_err(|e| format!("Type failed: {e}"))
    }

    /// Scroll/swipe within element bounds in the given direction: 'up',
    /// 'down', 'left' or 'right'.
    pub fn scroll(&self, element_number: &Value, direction: &str) -> Result<(), String> {
        let elem_num = py_int(element_number)?;
        let elem_data = self.element(elem_num)?;
        let bounds = bounds_of(&elem_data).ok_or_else(|| format!("Element #{elem_num} has no bounds"))?;
        let direction = direction.to_lowercase();

        // Dynamic approach: start from the edge in the SAME direction, swipe
        // in that direction. This avoids center overlays and ensures the
        // swipe starts from the correct edge.

        // Margins and swipe distances (dp, so the gesture is the same size
        // on every screen)
        let dp = self.dp();
        let margin = (20.0 * dp).round() as i64; // Safe margin from edge
        let vertical_swipe_distance = (200.0 * dp).round() as i64; // More power for up/down swipes
        let horizontal_swipe_distance = (200.0 * dp).round() as i64; // More power for left/right swipes

        // Calculate center for positioning
        let center_x = bounds.x + bounds.width / 2;
        let center_y = bounds.y + bounds.height / 2;

        let (start_x, start_y, end_x, end_y) = match direction.as_str() {
            // Start from top area, swipe upward (toward top edge)
            "up" => (center_x, bounds.y + margin + vertical_swipe_distance, center_x, bounds.y + margin),
            // Start from bottom area, swipe downward (toward bottom edge)
            "down" => (
                center_x,
                bounds.y + bounds.height - margin - vertical_swipe_distance,
                center_x,
                bounds.y + bounds.height - margin,
            ),
            // Start from left area, swipe leftward (toward left edge)
            "left" => (bounds.x + margin + horizontal_swipe_distance, center_y, bounds.x + margin, center_y),
            // Start from right area, swipe rightward (toward right edge)
            "right" => (
                bounds.x + bounds.width - margin - horizontal_swipe_distance,
                center_y,
                bounds.x + bounds.width - margin,
                center_y,
            ),
            _ => return Err(format!("Invalid direction: {direction}. Use: up, down, left, or right")),
        };

        self.adb_shell(&[
            "input",
            "swipe",
            &start_x.to_string(),
            &start_y.to_string(),
            &end_x.to_string(),
            &end_y.to_string(),
            "500",
        ])
        .map(|_| ())
        .map_err(|e| format!("Scroll failed: {e}"))
    }
}

/// `text` as `input text` takes it, quoted for the phone's shell: a space is
/// `%s`, a quote closes and reopens the quoting. None for anything beyond
/// printable ASCII, which `input text` cannot type.
fn adb_text(text: &str) -> Option<String> {
    if text.chars().any(|c| !(' '..='~').contains(&c)) {
        return None;
    }
    let body = text.replace('\'', "'\\''").replace(' ', "%s");
    Some(format!("'{body}'"))
}

/// Errors a tool reports: a message for the model (a failed RESULT), or a
/// Python error that must fly (KeyboardInterrupt).
pub enum ActErr {
    Msg(String),
    Py(pyo3::PyErr),
}

impl From<pyo3::PyErr> for ActErr {
    fn from(e: pyo3::PyErr) -> Self {
        ActErr::Py(e)
    }
}
