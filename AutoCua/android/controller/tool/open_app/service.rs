//! The app launcher: the copy of ios/controller/tool/open_app.py. Lists the
//! installed apps, matches a name to ONE of them, launches it and confirms
//! it came to the front; and the home screen.
//!
//! Through adb: `pm list packages` for the list, `monkey` with the LAUNCHER
//! category to launch (it resolves the app's launcher activity itself, on
//! every Android version), `dumpsys window` for the app in front, the HOME
//! key for the home screen. The phone lists packages, not the names shown
//! on the home screen, so an app's display name is the one of KNOWN_LABELS
//! when its package is there, else read off its package ("com.whatsapp" is
//! "whatsapp", "com.google.android.apps.maps" is "maps"); a name also
//! matches anywhere inside the package, as a bundle id does on iOS.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::super::super::service::controller_service;

/// One installed app. `bundle_id` keeps the iOS name: it is the package.
#[derive(Clone, Debug)]
pub struct AppInfo {
    pub bundle_id: String,
    pub display_name: String,
    pub version: String,
}

/// Apps whose package does not carry the name on their icon.
const KNOWN_LABELS: &[(&str, &str)] = &[
    ("com.android.vending", "Play Store"),
    ("com.google.android.gm", "Gmail"),
    ("com.google.android.apps.docs", "Drive"),
    ("com.google.android.googlequicksearchbox", "Google"),
    ("com.google.android.apps.messaging", "Messages"),
    ("com.google.android.dialer", "Phone"),
    ("com.android.dialer", "Phone"),
    ("com.samsung.android.dialer", "Phone"),
    ("com.samsung.android.messaging", "Messages"),
    ("com.google.android.play.games", "Play Games"),
    ("com.google.android.deskclock", "Clock"),
    ("com.google.android.apps.nbu.files", "Files"),
    ("com.google.android.apps.walletnfcrel", "Wallet"),
    ("com.google.android.apps.tachyon", "Meet"),
    ("com.google.android.videos", "Google TV"),
    ("com.google.android.apps.youtube.music", "YouTube Music"),
    ("com.android.chrome", "Chrome"),
    ("com.google.android.youtube", "YouTube"),
    ("com.google.android.apps.maps", "Maps"),
    ("com.google.android.apps.photos", "Photos"),
    ("com.google.android.calendar", "Calendar"),
    ("com.google.android.contacts", "Contacts"),
    ("com.android.contacts", "Contacts"),
    ("com.android.settings", "Settings"),
    ("com.android.camera2", "Camera"),
    ("com.netflix.mediaclient", "Netflix"),
    ("com.disney.disneyplus", "Disney+"),
    ("com.amazon.mShop.android.shopping", "Amazon"),
    ("com.amazon.avod.thirdpartyclient", "Prime Video"),
    ("com.zhiliaoapp.musically", "TikTok"),
    ("com.ss.android.ugc.trill", "TikTok"),
    ("com.facebook.katana", "Facebook"),
    ("com.facebook.orca", "Messenger"),
    ("com.twitter.android", "X"),
    ("com.microsoft.teams", "Teams"),
    ("com.google.android.apps.authenticator2", "Authenticator"),
];

/// Package segments that name nothing: skipped when a name is read off a
/// package ("com.instagram.android" is "instagram").
const GENERIC: &[&str] = &[
    "com", "org", "net", "io", "co", "android", "app", "apps", "mobile", "main", "client", "free", "pro", "lite",
    "beta", "release", "google", "samsung", "oneplus", "xiaomi", "miui", "huawei", "sec", "ui",
];

/// Match quality, best first. An exact display-name match must beat an
/// accidental substring hit.
const EXACT: i32 = 4;
const PREFIX: i32 = 3;
const IN_NAME: i32 = 2;
const IN_BUNDLE: i32 = 1;

/// Service for launching and managing the phone's apps.
pub struct AppLauncherService {
    apps: Mutex<Vec<AppInfo>>,
}

pub fn app_launcher_service() -> &'static AppLauncherService {
    static S: OnceLock<AppLauncherService> = OnceLock::new();
    S.get_or_init(|| AppLauncherService { apps: Mutex::new(Vec::new()) })
}

/// (display name, package) of every installed app, from the list the
/// launcher already cached: no device request (the skills lookup).
pub fn installed_apps() -> Vec<(String, String)> {
    app_launcher_service()
        .apps
        .lock()
        .unwrap()
        .iter()
        .map(|a| (a.display_name.clone(), a.bundle_id.clone()))
        .collect()
}

fn norm(text: &str) -> String {
    text.to_lowercase().trim().replace(' ', "")
}

/// A name for a package the phone cannot name: its most telling segment.
fn label_of(package: &str) -> String {
    if let Some((_, label)) = KNOWN_LABELS.iter().find(|(p, _)| *p == package) {
        return label.to_string();
    }
    let segments: Vec<&str> = package.split('.').collect();
    segments
        .iter()
        .rev()
        .find(|s| !GENERIC.contains(&s.to_lowercase().as_str()) && !s.is_empty())
        .or_else(|| segments.last())
        .map(|s| s.to_string())
        .unwrap_or_else(|| package.to_string())
}

impl AppLauncherService {
    pub fn apps(&self) -> Vec<AppInfo> {
        self.apps.lock().unwrap().clone()
    }

    /// Scan the installed apps: `pm list packages`. True when the list was read.
    pub fn scan_apps(&self) -> bool {
        match controller_service().adb_shell(&["pm", "list", "packages"]) {
            Ok(out) => {
                let mut apps: Vec<AppInfo> = out
                    .lines()
                    .filter_map(|l| l.trim().strip_prefix("package:"))
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .map(|p| AppInfo { bundle_id: p.to_string(), display_name: label_of(p), version: String::new() })
                    .collect();
                apps.sort_by(|a, b| a.bundle_id.cmp(&b.bundle_id));
                let found = !apps.is_empty();
                *self.apps.lock().unwrap() = apps;
                found
            }
            Err(e) => {
                eprintln!("App scan failed: {e}");
                false
            }
        }
    }

    /// Match tier of one app for a name (0 = no match). Single source of
    /// truth for search_app and resolve_app. An exact package ranks with an
    /// exact display name.
    fn score(app: &AppInfo, search_term: &str) -> i32 {
        let term = norm(search_term);
        let name = norm(&app.display_name);
        let bundle = app.bundle_id.to_lowercase();
        if name == term || bundle == term {
            EXACT
        } else if name.starts_with(&term) {
            PREFIX
        } else if name.contains(&term) {
            IN_NAME
        } else if bundle.contains(&term) {
            IN_BUNDLE
        } else {
            0
        }
    }

    /// Rank installed apps against a name: exact display name / package,
    /// then name prefix, then substring of the name, then substring of the
    /// package. Best first (ties: shorter name first).
    pub fn search_app(&self, search_term: &str) -> Vec<AppInfo> {
        let term = norm(search_term);
        if term.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<(i32, usize, AppInfo)> = self
            .apps()
            .into_iter()
            .filter_map(|app| {
                let score = Self::score(&app, &term);
                (score > 0).then(|| (score, norm(&app.display_name).chars().count(), app))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.into_iter().map(|(_, _, app)| app).collect()
    }

    /// Pick the ONE app a name refers to, or explain why it can't: Ok(app) on
    /// a confident match, Err(message) when the name is empty, unknown or
    /// ambiguous; the message names the closest installed apps so the caller
    /// can re-issue with an exact name instead of the launcher guessing.
    pub fn resolve_app(&self, search_term: &str) -> Result<AppInfo, String> {
        let term = search_term.trim().to_string();
        if term.is_empty() {
            return Err("open_app needs an app name - re-issue with the exact name shown on the home screen.".to_string());
        }
        if self.apps().is_empty() && (!self.scan_apps() || self.apps().is_empty()) {
            // Infrastructure failure, NOT "not installed": never steer the
            // model into the store for an app that is on the home screen.
            return Err("Could not read the installed app list from the device - open the app from its icon on the home screen instead.".to_string());
        }
        let mut ranked = self.search_app(&term);
        if ranked.is_empty() && self.scan_apps() {
            // The list is cached per process; the app may have been installed
            // since (the model was told to download it). One rescan, then decide.
            ranked = self.search_app(&term);
        }
        if ranked.is_empty() {
            let wanted = norm(&term);
            let mut close: Vec<(f64, String)> = self
                .apps()
                .into_iter()
                .map(|a| (similarity(&wanted, &norm(&a.display_name)), a.display_name))
                .filter(|(s, _)| *s >= 0.6)
                .collect();
            close.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            close.dedup_by(|a, b| a.1 == b.1);
            let names: Vec<String> = close.into_iter().take(3).map(|(_, n)| n).collect();
            let hint = if names.is_empty() { String::new() } else { format!(" Closest installed: {}.", names.join(", ")) };
            return Err(format!(
                "No installed app matches '{term}'.{hint} Verify the name from the home screen; if it is not installed, download it or find an alternative."
            ));
        }
        let best = ranked[0].clone();
        let best_score = Self::score(&best, &term);
        let peers: Vec<&AppInfo> = ranked.iter().filter(|a| Self::score(a, &term) == best_score).collect();
        // A unique best at any tier is decisive. Several equally-good matches:
        // the model chooses, not the launcher. Same-name packages are told
        // apart by the package.
        if peers.len() == 1 {
            return Ok(best);
        }
        if best_score == EXACT {
            let options: Vec<String> = peers.iter().take(3).map(|a| format!("{} ({})", a.display_name, a.bundle_id)).collect();
            return Err(format!(
                "'{term}' names {} installed apps: {}. Re-issue open_app with the package name.",
                peers.len(),
                options.join(", ")
            ));
        }
        let options: Vec<String> = peers.iter().take(3).map(|a| a.display_name.clone()).collect();
        Err(format!(
            "'{term}' is ambiguous - it matches: {}. Re-issue open_app with the exact app name.",
            options.join(", ")
        ))
    }

    /// Resolve a name to ONE installed app, launch it, and confirm it is the
    /// foreground app. Returns a dict the controller reports verbatim:
    ///   {"ok": true,  "display_name", "bundle_id", "message", "verified"?}
    ///   {"ok": false, "display_name"?, "bundle_id"?, "message"}
    /// `ok` is true ONLY when the target is confirmed in the foreground (or
    /// the foreground could not be read: then `verified` is false).
    pub fn launch_app(&self, app_name: &str) -> Value {
        let app = match self.resolve_app(app_name) {
            Ok(app) => app,
            Err(why) => {
                eprintln!("{why}");
                return json!({"ok": false, "message": why});
            }
        };
        let mut result = self.do_launch(&app.bundle_id, &app.display_name, Duration::from_secs(3));
        result["display_name"] = json!(app.display_name);
        result["bundle_id"] = json!(app.bundle_id);
        result
    }

    /// The package of the app in front (`dumpsys window`'s focus lines), or
    /// None when it can't be read.
    pub fn active_app(&self) -> Option<String> {
        let out = match controller_service().adb_shell(&["dumpsys", "window", "windows"]) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("dumpsys window failed: {e}");
                return None;
            }
        };
        focused_package(&out)
    }

    /// Launch, then VERIFY: success = the target is the app in front within
    /// `settle` seconds.
    fn do_launch(&self, package: &str, app_name: &str, settle: Duration) -> Value {
        let before = self.active_app();
        let launched = controller_service().adb_shell(&["monkey", "-p", package, "-c", "android.intent.category.LAUNCHER", "1"]);
        if let Err(e) = launched {
            eprintln!("Failed to launch {app_name}: {e}");
            return json!({"ok": false, "message": format!("Could not launch {app_name} ({package}).")});
        }
        let deadline = Instant::now() + settle;
        let mut fg: Option<String> = None; // the last successful reading, not the last attempt
        loop {
            if let Some(active) = self.active_app() {
                fg = Some(active);
            }
            if fg.as_deref() == Some(package) {
                return json!({"ok": true, "verified": true, "message": format!("{app_name} is open in the foreground.")});
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let Some(fg) = fg else {
            eprintln!("Launch of {app_name} accepted but could not be verified");
            return json!({"ok": true, "verified": false,
                          "message": format!("The launch of {app_name} was accepted, but the app in front could not be read - confirm on the next screenshot.")});
        };
        if before.as_deref() != Some(fg.as_str()) {
            // Something came in front that isn't the target: typically a
            // system dialog over the freshly launched app. Not a failure.
            eprintln!("Launch of {app_name} accepted; foreground is now {fg}");
            return json!({"ok": true, "verified": false,
                          "message": format!("Launch of {app_name} accepted; the foreground is now {fg} - a system dialog may be in front. Confirm on the next screenshot.")});
        }
        eprintln!("Launch of {app_name} accepted but foreground is unchanged ({fg})");
        json!({"ok": false,
               "message": format!("Launch of {app_name} ({package}) was accepted but nothing came to the foreground - active app is still {fg}. Tap its icon on the home screen instead.")})
    }

    /// Go to the home screen.
    pub fn go_home(&self) -> bool {
        match controller_service().adb_shell(&["input", "keyevent", "KEYCODE_HOME"]) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("Go home failed: {e}");
                false
            }
        }
    }
}

/// The package in `dumpsys window`'s focus lines: "mCurrentFocus=Window{...
/// u0 com.foo/com.foo.Main}" (every version), else "mFocusedApp=... u0
/// com.foo/...".
fn focused_package(dump: &str) -> Option<String> {
    for key in ["mCurrentFocus", "mFocusedApp"] {
        for line in dump.lines() {
            if !line.contains(key) {
                continue;
            }
            for token in line.split_whitespace() {
                let token = token.trim_end_matches('}');
                if let Some((package, _activity)) = token.split_once('/') {
                    if package.contains('.') && !package.contains('=') {
                        return Some(package.to_string());
                    }
                }
            }
        }
    }
    None
}

/// How alike two names are, 0 to 1 (difflib's ratio stands in for).
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f64 / a.len().max(b.len()) as f64
}
