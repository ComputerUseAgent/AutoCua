//! The back tool: the phone's system Back, the same as its back button or
//! back gesture (`input keyevent KEYCODE_BACK`). Every app takes it: it takes
//! the page back, and it closes a full-screen video player, one with no close
//! button too (YouTube), with nothing to find on a screen a DRM player keeps
//! black. Each Back must change the app's screen before the next is pressed.

use std::thread::sleep;
use std::time::Duration;

use pyo3::prelude::*;
use serde_json::Value;

use crate::controller::service::{controller_service, py_int};
use crate::tree::element::read_tree;

/// The most Backs one call presses: enough to leave YouTube's full screen and
/// its page, not enough to leave every app.
const MAX_PRESSES: i64 = 5;

/// After a Back, the screen is read every half second, up to 3 s, for it to
/// change.
const CHANGE_CHECK: Duration = Duration::from_millis(500);
const CHANGE_CHECKS: u32 = 6;

/// The app's screen as its views: each one's class and view id, in order.
/// Their text is left out, so a clock or an ad's countdown ticking is no
/// change.
fn app_views() -> Result<Vec<(String, String)>, String> {
    let xml = read_tree()?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("bad XML from the helper: {e}"))?;
    // The helper writes the app's window first, then the system's bars.
    let app = doc.root_element().children().find(|n| n.has_tag_name("node")).ok_or("the helper returned an empty tree")?;
    Ok(app
        .descendants()
        .filter(|n| n.has_tag_name("node"))
        .map(|n| (n.attribute("class").unwrap_or("").to_string(), n.attribute("resource-id").unwrap_or("").to_string()))
        .collect())
}

/// One Back: true when the app's screen then changed.
fn press() -> Result<bool, String> {
    let before = app_views().map_err(|e| format!("Back was not pressed: the screen could not be read ({e})"))?;
    controller_service().adb_shell(&["input", "keyevent", "KEYCODE_BACK"]).map_err(|e| format!("Back failed: {e}"))?;
    for _ in 0..CHANGE_CHECKS {
        sleep(CHANGE_CHECK);
        let now = app_views().map_err(|e| format!("Back was pressed, but the screen could not be read ({e})"))?;
        if now != before {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Press Back `value` times (1 when it is not a whole number, at most
/// MAX_PRESSES), each once the last one changed the screen: the presses
/// made, or why it stopped.
pub fn back(py: Python<'_>, value: &Value) -> Result<i64, String> {
    let times = py_int(value).unwrap_or(1).clamp(1, MAX_PRESSES);
    py.detach(|| {
        for n in 1..=times {
            if !press()? {
                return Err(if times == 1 {
                    "Back was pressed, but the screen did not change".to_string()
                } else {
                    format!("Back {n} of {times} was pressed, but the screen did not change")
                });
            }
        }
        Ok(times)
    })
}
