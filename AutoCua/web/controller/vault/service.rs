//! The `vault` tool and the <vault> block inside <persistent_memory>.
//!
//! The user's logins live in the "web" section of AutoCua_data/vault/
//! credentials.json (one file, a section per device), keyed by site:
//! "web": {"github.com": {"email": "...", "password": "...", "pin": "1234"}}. A site
//! covers itself and its subdomains ("google.com" covers accounts.google.com),
//! and the longest site that covers the tab wins. The file is read fresh each
//! time, so an entry saved during a run is used at once.
//!
//! The model only ever sees key NAMES: <vault url="github.com"> lists them
//! while the current tab is on a saved site (no block at all otherwise), and
//! `vault {"id": 5, "value": "password"}` types the saved value into [5] the
//! way `input` types. The value is never in a result, and every value the
//! vault typed this run is shown as [vault] wherever the page could show it
//! back: the element tree and the tool results (`hide`).
//!
//! Checked at fill time, on the tab as it is NOW (a click earlier in the same
//! batch may have left the page the last scan saw):
//! - the site comes from the tab's live url, https only (http on localhost),
//!   and the page is checked again right before the value goes out: the
//!   vault's own click may have followed a link, or a redirect landed;
//! - the field that takes focus must be in the page itself, never in a frame:
//!   a frame on github.com can be another site's page;
//! - a password goes only into a password field;
//! - a page that cannot answer those checks gets nothing.

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;
use serde_json::{json, Map, Value};

use crate::agent::main_driver::view::py_str_of;
use crate::browser::{truthy, ScannerInner};
use crate::controller::input::service::type_into_rect;
use crate::controller::service::{
    err, label_for, rect_for, reject_viewport, resolve_element_id, scan_op, with_cursor, ActErr, ActResult,
};

/// What a value the vault typed reads as, once it is back in a page.
const HIDDEN: &str = "[vault]";

/// The element tree clips a field's value to 39 characters and an ellipsis
/// (tree/element.rs, element.js: clip(.., 40)), so a longer value is hidden
/// by its first 39 characters too.
const CLIP_KEEP: usize = 39;

/// The values the vault typed this run, longest first.
static FILLED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A run starts with nothing to hide.
pub fn new_run() {
    if let Ok(mut filled) = FILLED.lock() {
        filled.clear();
    }
}

/// The site of `url` when the vault may fill there: its host, lowercased.
/// None for anything but https (http on localhost): chrome:, file:, data:...
fn site_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");
    match u.scheme() {
        "https" => Some(host),
        "http" if local => Some(host),
        _ => None,
    }
}

/// A "web" key as a bare host: "https://www.GitHub.com/login" -> "github.com".
fn site_key(key: &str) -> String {
    let k = key.trim().to_ascii_lowercase();
    let k = k.trim_start_matches("https://").trim_start_matches("http://");
    let host = k.split('/').next().unwrap_or("");
    let host = match host.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    host.trim_end_matches('.').trim_start_matches("www.").to_string()
}

/// `host` is `site` or one of its subdomains. A whole label must match:
/// "github.com" does not cover "notgithub.com" or "github.com.evil.io".
fn covers(host: &str, site: &str) -> bool {
    !site.is_empty() && (host == site || host.ends_with(&format!(".{site}")))
}

/// The "web" section of credentials.json, read now (none: empty). Its errors
/// never quote the file.
fn entries(py: Python<'_>) -> Result<Map<String, Value>, String> {
    const FILE: &str = "AutoCua_data/vault/credentials.json";
    let path = crate::vault_file(py).map_err(|_| "the vault file could not be found".to_string())?;
    let text = std::fs::read_to_string(&path).map_err(|_| format!("{FILE} could not be read"))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(mut all)) => match all.remove("web") {
            Some(Value::Object(web)) => Ok(web),
            None => Ok(Map::new()),
            Some(_) => Err(format!("\"web\" in {FILE} must hold an object: {{\"site\": {{\"key\": \"value\"}}}}")),
        },
        Ok(_) => Err(format!("{FILE} must hold an object")),
        Err(e) => Err(format!("{FILE} is not valid JSON ({e})")),
    }
}

/// The entry for `host`: the longest site that covers it, with that site.
fn entry_for<'a>(all: &'a Map<String, Value>, host: &str) -> Option<(String, &'a Map<String, Value>)> {
    all.iter()
        .filter_map(|(k, v)| Some((site_key(k), v.as_object()?)))
        .filter(|(site, _)| covers(host, site))
        .max_by_key(|(site, _)| site.len())
}

/// A saved value as the text to type: a non-empty string, or a whole number
/// (a pin saved as 1234). Anything else is not typed.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) if n.is_u64() || n.is_i64() => Some(n.to_string()),
        _ => None,
    }
}

/// The keys of an entry that hold something to type, as the file writes them.
fn usable_keys(creds: &Map<String, Value>) -> Vec<&str> {
    creds.iter().filter(|(_, v)| text_of(v).is_some()).map(|(k, _)| k.as_str()).collect()
}

/// <vault>: the key names saved for the site of `url` (the last scan's, the
/// page the model is shown). Empty when nothing is saved for it.
pub fn block(py: Python<'_>, url: &str) -> String {
    let Some(host) = site_of(url) else {
        return String::new();
    };
    let Ok(all) = entries(py) else {
        return String::new();
    };
    let Some((site, creds)) = entry_for(&all, &host) else {
        return String::new();
    };
    let keys = usable_keys(creds);
    if keys.is_empty() {
        return String::new();
    }
    format!("<vault url=\"{site}\">\n{}\n</vault>", keys.join(", "))
}

/// The vault's focus test, in FOCUS_HOLDER's shape (input/service.rs): "" when
/// the field that has focus may take the value, else what has it and why not.
/// It runs as the value is about to go out, so it checks the page's own site
/// too, by site_of's rule.
fn focus_check(site: &str, password: bool) -> String {
    let site = serde_json::to_string(site).unwrap_or_default();
    format!(
        "(function () {{\
            var site = {site}, h = location.hostname.toLowerCase().replace(/\\.$/, '');\
            var local = h === 'localhost' || h === '127.0.0.1' || h === '[::1]';\
            var secure = location.protocol === 'https:' || (local && location.protocol === 'http:');\
            if (!secure || !(h === site || h.endsWith('.' + site)))\
                return 'a page on ' + (h || location.protocol) + ', and the vault types only on ' + site;\
            var a = document.activeElement;\
            while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;\
            if (!a) return 'nothing';\
            var t = a.tagName, ty = (a.getAttribute('type') || '').toLowerCase();\
            var what = t.toLowerCase() + (ty ? '[type=' + ty + ']' : '');\
            if (t === 'IFRAME' || t === 'FRAME') return what + ', a page inside the page that can be another site, and the vault never types there';\
            if ({password}) return t === 'INPUT' && a.type === 'password' ? '' : what + ', and a saved password goes only into a password field';\
            if (t === 'INPUT' && !/^(button|submit|reset|checkbox|radio|file|image|range|color|hidden)$/.test(ty)) return '';\
            if (t === 'TEXTAREA' || a.isContentEditable === true) return '';\
            var role = a.getAttribute('role');\
            return what + (!ty && role ? '[role=' + role + ']' : '');\
        }})()"
    )
}

/// Hide `secret` from here on, in the forms a page shows it back in: as
/// typed, with its whitespace collapsed (the tree's clip, an email field's
/// trim) and url-encoded (a later url's ?email=you%40example.com). Added
/// before a single key goes out, since a fill that fails halfway may still
/// have typed some of it.
fn remember(secret: &str) {
    let flat = secret.split_whitespace().collect::<Vec<_>>().join(" ");
    let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    let spaced = encoded.replace('+', "%20");
    if let Ok(mut filled) = FILLED.lock() {
        for form in [secret.to_string(), flat, encoded, spaced] {
            if !form.is_empty() && !filled.contains(&form) {
                filled.push(form);
            }
        }
        // Longest first: a value inside another value goes with it.
        filled.sort_by_key(|s| std::cmp::Reverse(s.chars().count()));
    }
}

/// The `vault` tool: type the value saved under `raw_key` for the tab's site
/// into [raw_id]. The result names the key, never the value.
pub fn fill(
    py: Python<'_>,
    scanner: &Arc<Mutex<ScannerInner>>,
    raw_id: &Value,
    raw_key: &Value,
    elements: &Map<String, Value>,
) -> ActResult<Value> {
    let idx = resolve_element_id(elements, raw_id)?;
    reject_viewport(idx, "vault")?;
    let key = if truthy(raw_key) { py_str_of(raw_key).trim().to_string() } else { String::new() };
    if key.is_empty() {
        return err("vault's `value` is the name of a key in <vault>, like \"email\" or \"password\"");
    }
    let url = scan_op(py, scanner, |s| {
        Ok(s.current_target_id().ok().map(|id| s.tab_url_now(&id)).unwrap_or_default())
    })?;
    let Some(host) = site_of(&url) else {
        return err("the vault fills only on https pages, and this tab is not on one");
    };
    let all = entries(py).map_err(ActErr::Msg)?;
    let Some((site, creds)) = entry_for(&all, &host) else {
        return err(format!(
            "nothing is saved in the vault for {host}: use what the task gives, or ask the user"
        ));
    };
    let Some((name, saved)) = creds.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(&key)) else {
        return err(format!(
            "no \"{key}\" is saved for {site}: <vault> has {}",
            usable_keys(creds).join(", ")
        ));
    };
    let Some(secret) = text_of(saved) else {
        return err(format!(
            "the \"{name}\" saved for {site} is empty or not text: ask the user to fix it under \"web\" in AutoCua_data/vault/credentials.json"
        ));
    };
    let rect = rect_for(elements, idx)?;
    remember(&secret);
    let check = focus_check(&site, name.to_ascii_lowercase().contains("password"));
    with_cursor(py, scanner, &rect, move |s| {
        if s.bridged() {
            // Extension mode: input's typing (tools.js), with this focus test
            // in place of its own.
            return s
                .bridge_call(json!({"type": "vault_input", "rect": rect, "text": secret, "focus": check}))
                .map(|_| ());
        }
        s.with_tab(|cdp, sess| type_into_rect(cdp, sess, &rect, &secret, false, &check))
    })
    .map_err(|e| match e {
        ActErr::Msg(m) if m.contains("unknown request") => ActErr::Msg(
            "the AutoCua extension running in Chrome is an older copy without the vault: \
             restart Chrome so it loads the new one, then try again"
                .into(),
        ),
        e => e,
    })?;
    let label = label_for(elements, idx);
    let target = if label.is_empty() { format!("[{idx}]") } else { format!("[{idx}] {label}") };
    Ok(json!({"status": "success", "tool": "vault", "id": idx, "value": name,
              "message": format!("filled {target} with the saved {name} for {site}")}))
}

/// `text` with every value the vault typed this run shown as [vault].
pub fn hide(text: &str) -> String {
    let Ok(filled) = FILLED.lock() else {
        return text.to_string();
    };
    let mut out = text.to_string();
    for secret in filled.iter() {
        out = hide_one(&out, secret);
    }
    out
}

/// Every string in a tool result, through `hide`.
pub fn hide_json(v: &mut Value) {
    if FILLED.lock().map(|f| f.is_empty()).unwrap_or(true) {
        return;
    }
    match v {
        Value::String(s) => *s = hide(s),
        Value::Array(items) => items.iter_mut().for_each(hide_json),
        Value::Object(map) => map.values_mut().for_each(hide_json),
        _ => {}
    }
}

/// `secret`, and any run of at least CLIP_KEEP of its first characters, as
/// [vault]. A match that runs on into a longer word or number is that word,
/// not the secret ("admin" in "administration"), and is left alone.
fn hide_one(text: &str, secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    let need = chars.len().min(CLIP_KEEP);
    if need == 0 {
        return text.to_string();
    }
    let head: String = chars[..need].iter().collect();
    let joins = |next: Option<char>, edge: char| next.is_some_and(char::is_alphanumeric) && edge.is_alphanumeric();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&head) {
        let before = rest[..at].chars().next_back().or_else(|| out.chars().next_back());
        let mut end = at;
        let mut last = chars[0];
        for (c, want) in rest[at..].chars().zip(chars.iter()) {
            if c != *want {
                break;
            }
            end += c.len_utf8();
            last = c;
        }
        out.push_str(&rest[..at]);
        if joins(before, chars[0]) || joins(rest[end..].chars().next(), last) {
            // Part of a longer word: keep its first character, look on.
            let step = at + chars[0].len_utf8();
            out.push_str(&rest[at..step]);
            rest = &rest[step..];
        } else {
            out.push_str(HIDDEN);
            rest = &rest[end..];
        }
    }
    out.push_str(rest);
    out
}
