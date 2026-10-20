//! Keyboard shortcuts — the `keyboard` tool.
//!
//! One key, or a chord of up to three, delivered to whatever has focus in
//! the current tab over the CDP session the browser side owns and lends
//! out. Trusted input through Input.dispatchKeyEvent — the same pipeline a
//! physical keyboard feeds — so no JavaScript runs in the page and nothing
//! is injected into it.
//!
//! Strokes, not typing: every key here is pressed once, as a hand would
//! press it — so a bare printable key still lands its one character in a
//! focused field, and Enter still submits. Putting a VALUE into a field is
//! `input`'s job (Input.insertText) and stays there; the two tools split
//! cleanly on that line.
//!
//! The keys reach the page's renderer only. Shortcuts the browser process
//! owns — new tab, address bar, find, reload, zoom — never arrive this way;
//! the tab tools cover those, and the tool description says so.

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Value};

use crate::agent::main_driver::view::py_str_of;
use crate::browser::{Cdp, CdpFail, ScannerInner};
use crate::controller::service::{err, scan_op, ActResult};

/// A chord longer than this is not a shortcut any page defines.
pub const MAX_KEYS: usize = 3;

// CDP's modifier bits.
const ALT: u8 = 1;
const CTRL: u8 = 2;
const META: u8 = 4;
const SHIFT: u8 = 8;

/// The whole-word keys, as the model writes them: aliases, then what Chrome
/// wants — DOM `key`, DOM `code`, the Windows virtual key code, and the text
/// the key inserts (empty for one that inserts nothing).
const NAMED: &[(&[&str], &str, &str, i64, &str)] = &[
    (&["esc", "escape"], "Escape", "Escape", 27, ""),
    (&["enter", "return"], "Enter", "Enter", 13, "\r"),
    (&["tab"], "Tab", "Tab", 9, ""),
    (&["space"], " ", "Space", 32, " "),
    (&["backspace"], "Backspace", "Backspace", 8, ""),
    (&["delete", "del"], "Delete", "Delete", 46, ""),
    (&["up"], "ArrowUp", "ArrowUp", 38, ""),
    (&["down"], "ArrowDown", "ArrowDown", 40, ""),
    (&["left"], "ArrowLeft", "ArrowLeft", 37, ""),
    (&["right"], "ArrowRight", "ArrowRight", 39, ""),
    (&["home"], "Home", "Home", 36, ""),
    (&["end"], "End", "End", 35, ""),
    (&["pageup", "pgup"], "PageUp", "PageUp", 33, ""),
    (&["pagedown", "pgdn"], "PageDown", "PageDown", 34, ""),
    (&["f1"], "F1", "F1", 112, ""),
    (&["f2"], "F2", "F2", 113, ""),
    (&["f3"], "F3", "F3", 114, ""),
    (&["f4"], "F4", "F4", 115, ""),
    (&["f5"], "F5", "F5", 116, ""),
    (&["f6"], "F6", "F6", 117, ""),
    (&["f7"], "F7", "F7", 118, ""),
    (&["f8"], "F8", "F8", 119, ""),
    (&["f9"], "F9", "F9", 120, ""),
    (&["f10"], "F10", "F10", 121, ""),
    (&["f11"], "F11", "F11", 122, ""),
    (&["f12"], "F12", "F12", 123, ""),
];

/// The punctuation keys of a US layout: the characters on a key (plain, then
/// shifted), its DOM `code`, and its virtual key code. Older shortcut
/// handlers still match on keyCode ("/" is 191), so the real code goes along.
const PUNCTUATION: &[(&str, &str, i64)] = &[
    ("`~", "Backquote", 192),
    ("-_", "Minus", 189),
    ("=+", "Equal", 187),
    ("[{", "BracketLeft", 219),
    ("]}", "BracketRight", 221),
    ("\\|", "Backslash", 220),
    (";:", "Semicolon", 186),
    ("'\"", "Quote", 222),
    (",<", "Comma", 188),
    (".>", "Period", 190),
    ("/?", "Slash", 191),
];

/// The shifted digits, in digit order: ")" sits on 0, "!" on 1, and so on.
const SHIFTED_DIGITS: &str = ")!@#$%^&*(";

/// The modifiers: aliases, DOM `key`, DOM `code`, virtual key code, CDP bit.
const MODIFIERS: &[(&[&str], &str, &str, i64, u8)] = &[
    (&["shift"], "Shift", "ShiftLeft", 16, SHIFT),
    (&["ctrl", "control"], "Control", "ControlLeft", 17, CTRL),
    (&["alt", "option", "opt"], "Alt", "AltLeft", 18, ALT),
    (&["cmd", "command", "meta", "win", "super"], "Meta", "MetaLeft", 91, META),
];

/// One key as Chrome wants it.
struct Key {
    key: String,
    code: String,
    vk: i64,
    text: String,
    /// The CDP bit this key holds down — 0 for an ordinary key.
    modifier: u8,
}

/// A name from the combo to a key: a whole-word key, a modifier, or any
/// single character standing for itself.
fn resolve(name: &str) -> Option<Key> {
    if let Some((_, key, code, vk, text)) = NAMED.iter().find(|(a, ..)| a.contains(&name)) {
        return Some(Key {
            key: key.to_string(),
            code: code.to_string(),
            vk: *vk,
            text: text.to_string(),
            modifier: 0,
        });
    }
    if let Some((_, key, code, vk, bit)) = MODIFIERS.iter().find(|(a, ..)| a.contains(&name)) {
        return Some(Key {
            key: key.to_string(),
            code: code.to_string(),
            vk: *vk,
            text: String::new(),
            modifier: *bit,
        });
    }
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_whitespace() || c.is_control() {
        return None;
    }
    // `code` and the virtual key code name the physical key, which pages
    // that match on keyCode/code rather than `key` still depend on.
    let (code, vk) = if c.is_ascii_alphabetic() {
        (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as i64)
    } else if c.is_ascii_digit() {
        (format!("Digit{c}"), c as i64)
    } else if let Some(i) = SHIFTED_DIGITS.find(c) {
        (format!("Digit{i}"), '0' as i64 + i as i64)
    } else if let Some((_, code, vk)) = PUNCTUATION.iter().find(|(chars, ..)| chars.contains(c)) {
        (code.to_string(), *vk)
    } else {
        (String::new(), 0)
    };
    Some(Key { key: c.to_string(), code, vk, text: c.to_string(), modifier: 0 })
}

/// Split "ctrl+shift+p" into its key names. A "+" that BEGINS a key is the
/// plus key itself ("ctrl++" is ctrl and plus, "++-" is plus and minus);
/// anywhere else it joins keys. Whitespace is ignored.
fn split(combo: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in combo.chars().filter(|c| !c.is_whitespace()) {
        if c == '+' && !cur.is_empty() {
            keys.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        keys.push(cur);
    }
    keys
}

/// The editing shortcuts, named for Chrome's own editing channel.
///
/// On mac the browser process owns cmd+a/c/v/x/z (they are menu items), so
/// a synthetic key event alone reaches the page and does nothing. Sending
/// the command WITH the key runs it whatever the platform — the same trick
/// `input` uses to select all before typing — and the page still sees the
/// ordinary keydown.
///
/// Caret chords (cmd+left, cmd+backspace, ...) are deliberately not bridged:
/// `input` replaces a field's value whole, so the agent never edits at a
/// caret, and the table stays six lines.
fn edit_command(key: &str, held: u8) -> Option<&'static str> {
    if held & (CTRL | META) == 0 || held & ALT != 0 {
        return None;
    }
    Some(match (key, held & SHIFT != 0) {
        ("a", false) => "selectAll",
        ("c", false) => "copy",
        ("v", false) => "paste",
        ("x", false) => "cut",
        ("z", false) => "undo",
        ("z", true) | ("y", false) => "redo",
        _ => return None,
    })
}

/// One key event, carrying the modifiers held at that moment.
fn event(cdp: &mut Cdp, sess: &str, kind: &str, k: &Key, held: u8) -> Result<(), CdpFail> {
    // A held shift makes a letter its capital, as on a real keyboard.
    let shifted = held & SHIFT != 0 && k.key.len() == 1 && k.key.chars().all(|c| c.is_ascii_lowercase());
    let key = if shifted { k.key.to_ascii_uppercase() } else { k.key.clone() };
    let mut p = json!({
        "type": kind,
        "key": key,
        "code": k.code,
        "windowsVirtualKeyCode": k.vk,
        "nativeVirtualKeyCode": k.vk,
        "modifiers": held,
    });
    if kind == "keyDown" {
        // A stroke inserts its character only when no command modifier is
        // held: ctrl+a selects, it does not type an "a". Shift alone still
        // types.
        if !k.text.is_empty() && held & (ALT | CTRL | META) == 0 {
            p["text"] = json!(if shifted { key.clone() } else { k.text.clone() });
        }
        if let Some(cmd) = edit_command(&k.key, held) {
            p["commands"] = json!([cmd]);
        }
    }
    cdp.rpc("Input.dispatchKeyEvent", p, Some(sess), 5.0)?;
    Ok(())
}

/// Hold the keys in the order given, then let go in reverse — the gesture a
/// hand makes for "ctrl+shift+p". Each event carries the modifiers held at
/// that moment, which is what makes shift+tab a backwards tab rather than a
/// shift and then a tab.
fn chord(cdp: &mut Cdp, sess: &str, keys: &[Key]) -> Result<(), CdpFail> {
    let mut held: u8 = 0;
    for k in keys {
        held |= k.modifier;
        event(cdp, sess, "keyDown", k, held)?;
    }
    for k in keys.iter().rev() {
        held &= !k.modifier;
        event(cdp, sess, "keyUp", k, held)?;
    }
    Ok(())
}

/// Press `value` — one key or a chord — on the current tab.
pub fn shortcut(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    value: &Value,
) -> ActResult<Value> {
    let combo = if value.is_null() {
        String::new()
    } else {
        py_str_of(value).trim().to_lowercase()
    };
    let names = split(&combo);
    if names.is_empty() {
        return err("keyboard needs a key in `value` - e.g. \"esc\", \"enter\" or \"ctrl+a\"");
    }
    if names.len() > MAX_KEYS {
        return err(format!(
            "'{combo}' has {} keys - a shortcut is at most {MAX_KEYS}, joined with '+'",
            names.len()
        ));
    }
    let mut keys: Vec<Key> = Vec::with_capacity(names.len());
    for name in &names {
        match resolve(name) {
            Some(k) => keys.push(k),
            None => {
                return err(format!(
                    "unknown key '{name}' in '{combo}' - use a named key (esc, enter, \
                     tab, space, backspace, delete, up, down, left, right, home, end, \
                     pageup, pagedown, f1-f12), a modifier (ctrl, cmd, alt, shift) or \
                     a single character. `keyboard` presses keys; to type text use `input`."
                ));
            }
        }
    }
    let pressed = names.join("+");
    scan_op(py, scanner, move |s| {
        if s.bridged() {
            // Extension mode: the resolved keys, pressed as a chord by
            // AutoCuaBridge (tools.js) with the same events.
            let keys: Vec<Value> = keys
                .iter()
                .map(|k| json!({"key": k.key, "code": k.code, "vk": k.vk, "text": k.text, "modifier": k.modifier}))
                .collect();
            return s.bridge_call(json!({"type": "keyboard", "keys": keys})).map(|_| ());
        }
        s.with_tab(|cdp, sess| chord(cdp, sess, &keys))
    })?;
    Ok(json!({"status": "success", "tool": "keyboard", "value": pressed,
              "message": format!("pressed {pressed}")}))
}
