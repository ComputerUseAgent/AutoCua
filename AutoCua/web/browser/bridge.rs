//! The bridge to AutoCuaBridge: the agent side of extension mode.
//!
//! A localhost websocket server the extension dials. `config.json` in the staged
//! copy of the extension (stage_bridge.rs) carries the port and a per-launch token;
//! the handshake is refused without the token in the path, and from any origin but
//! the extension's own. Over it the agent asks the extension for what it would
//! otherwise do over a debugging port: the scan (element.js) and the browser half of
//! every tool (tools.js). One request is in flight at a time, answered with
//! `{id, result}` or `{id, error}`; what the extension sends on its own in between
//! (a `notice` about a dialog it answered or a page that crashed, the Test button's
//! `save`) is kept aside for whoever reads it.
//!
//! `ScannerInner` holds one of these in extension mode (`BrowserScanner.over_bridge`),
//! and the Python-facing `Bridge` class wraps the same one, so a test or a launcher
//! can stage the extension, start Chrome with it and wait for it to dial in. The
//! hand test (bridge_test.rs) shares the launch and the handshake guard.
//!
//! The Chrome launch finds the browser the way the debugging-port mode does
//! (browser.rs find_chrome) and starts it itself, on macOS, Windows and Linux;
//! `take_chrome` is how a run gets the person's own Chrome on the line.
//!
//! While no run is on, the desktop app keeps a line of its own open
//! (`idle_bridge_start`, ui/service.py): it asks the browser nothing and only
//! answers what the extension sends by itself, the Test button's `save` above
//! all, so a scan can be saved under debug/ without the hand test. It knows a
//! run is on by the run lock (`hold_run_lock`) and steps aside for it.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::fs::File;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use serde_json::{json, Value};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::{accept_hdr, Message, WebSocket};

use crate::browser::{SResult, ScanErr};
use crate::stage_bridge;

/// How long a request waits for the extension to dial in (or back in: its worker
/// redials half a second after a close) before giving up.
const CONNECT_WAIT: Duration = Duration::from_secs(3);
/// Reads come back this often to look for Ctrl-C.
const READ_SLICE: Duration = Duration::from_millis(200);
/// Notices kept for the model, newest kept (browser.rs keeps the same number).
const NOTICE_CAP: usize = 8;
/// How long Chrome started again after it went away (`relaunch`) has to come up and
/// its extension to dial back in; a fresh launch dials in within 1 to 2 s.
const RELAUNCH_WAIT: Duration = Duration::from_secs(30);
/// How long a Chrome already running with AutoCuaBridge gets to dial a new run in
/// (`take_chrome`): its worker reads config.json again each time it dials, every
/// half second while nobody is on the line, so it is here in about a second.
const ATTACH_WAIT: Duration = Duration::from_secs(2);
/// How long the worker gets after `wake_extension` opened its wake-up page: a
/// stopped worker starts on the page's message and dials within half a second;
/// measured 1.4 to 4 s from cold. Chrome stops an idle extension worker after
/// half a minute whatever the keepalive asks, so a run minutes after the last one
/// found nobody on the line within ATTACH_WAIT and quit a Chrome that had the
/// extension all along.
const WAKE_WAIT: Duration = Duration::from_secs(8);

/// Chrome's own user data folder, where the person's Default profile lives: a launch
/// without --user-data-dir uses it. The person's Chrome is extension mode's browser.
pub fn default_user_data_dir() -> PathBuf {
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default();
        return local.join("Google").join("Chrome").join("User Data");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Google/Chrome")
    } else {
        home.join(".config/google-chrome")
    }
}

/// Whether something listens on a bridge port: an agent still on the line.
fn port_live(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok()
}

/// config.json in the staged copy lists every agent on the line, `{"bridges": [{port,
/// token}, ...]}`, and the extension dials each (background.js). `edit` adds this
/// bridge's entry or takes it out. Several agents open at once in a parallel run, so the
/// file is rewritten under a lock file and swapped in whole; entries whose port no one
/// listens on any more (a process that ended without dropping its bridge) are dropped on
/// the way, `own` excepted.
fn config_write(staged: &Path, own: u16, edit: impl FnOnce(&mut Vec<Value>)) -> io::Result<()> {
    let lock = staged.join("config.lock");
    let mut tries = 0;
    loop {
        match fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => break,
            Err(_) if tries < 200 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            // Two seconds: a lock left by a process that died holding it.
            Err(_) => {
                let _ = fs::remove_file(&lock);
            }
        }
    }
    let path = staged.join("config.json");
    let mut entries: Vec<Value> = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .map(|v| match v.get("bridges").and_then(Value::as_array) {
            Some(list) => list.clone(),
            None if v.get("port").is_some() => vec![v],
            None => Vec::new(),
        })
        .unwrap_or_default();
    entries.retain(|e| match e.get("port").and_then(Value::as_u64) {
        Some(p) if p == own as u64 => false, // this bridge's own entry: `edit` decides
        Some(p) => u16::try_from(p).is_ok_and(port_live),
        None => false,
    });
    edit(&mut entries);
    let tmp = staged.join("config.json.tmp");
    let written = fs::write(&tmp, json!({ "bridges": entries }).to_string()).and_then(|_| fs::rename(&tmp, &path));
    let _ = fs::remove_file(&lock);
    written
}

/// 24 random bytes as hex: the per-launch token the extension must present.
pub fn token() -> io::Result<String> {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 24];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| io::Error::other("no system randomness"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Start Chrome with the staged extension: the two flags that load it
/// (--disable-extensions-except with the staged copy, and the feature that would block
/// that switch turned off), so it comes up with no prompt and no debugging port.
/// Chrome's own "'AutoCua' started debugging this browser" bar shows while the agent
/// drives a tab, and its Cancel button takes the browser back from the agent
/// (tools.js). `user_data_dir` None is the person's own Chrome; `restore` brings back
/// the tabs of a Chrome just quit; `extra` adds switches (a test passes a debugging
/// port). The browser is the one the debugging-port mode drives (browser.rs
/// find_chrome: Chrome, else Chromium or Edge), started in a process group of its own
/// so that it outlives this process and a terminal closing on it, as `open -a` left it
/// to macOS before.
pub fn launch_chrome(
    staged: &Path,
    user_data_dir: Option<&Path>,
    restore: bool,
    extra: &[String],
) -> SResult<()> {
    let chrome = crate::browser::find_chrome()?;
    let mut cmd = Command::new(chrome);
    if let Some(dir) = user_data_dir {
        fs::create_dir_all(dir)
            .map_err(|e| ScanErr::s(format!("could not create {}: {e}", dir.display())))?;
        cmd.arg(format!("--user-data-dir={}", dir.display()));
        cmd.args(["--no-first-run", "--no-default-browser-check"]);
    }
    if restore {
        cmd.arg("--restore-last-session");
    }
    cmd.arg(format!("--disable-extensions-except={}", staged.display()));
    // Only the last --disable-features switch counts, so Windows keeps the rasterizer
    // the debugging-port mode asks for (browser.rs) in the same list.
    if cfg!(windows) {
        cmd.arg("--disable-features=SkiaGraphite,DisableDisableExtensionsExceptCommandLineSwitch");
    } else {
        cmd.arg("--disable-features=DisableDisableExtensionsExceptCommandLineSwitch");
    }
    cmd.args(extra).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map_err(|e| ScanErr::s(format!("could not launch Chrome: {e}")))?;
    Ok(())
}

/// Open the extension's wake-up page (wake.html) in the Chrome already running: a
/// second Chrome started with a URL hands it to the running one and exits, which
/// opens the page as a tab there. On macOS `open -g` does the same through
/// LaunchServices without bringing Chrome to the front (the main agent may be
/// working on the screen). Says whether the request went out, not whether the
/// worker woke: `take_chrome` waits for its dial.
fn wake_extension(extension_id: &str) -> bool {
    let url = format!("chrome-extension://{extension_id}/wake.html");
    let Ok(chrome) = crate::browser::find_chrome() else { return false };
    let mut cmd = if cfg!(target_os = "macos") {
        let bundle = chrome
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .map(PathBuf::from);
        let mut c = Command::new("open");
        c.arg("-g");
        if let Some(bundle) = bundle {
            c.arg("-a").arg(bundle);
        }
        c.arg(&url);
        c
    } else {
        let mut c = Command::new(chrome);
        c.arg(&url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// The browser in a process group of its own: it is the person's browser and outlives
/// the run, whatever happens to this process or its terminal.
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS.
#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0000_0200 | 0x0000_0008);
}

/// Quit the Chrome running on `dir`, if any, and wait for it to be gone. Its
/// SingletonLock is a symlink to `host-pid`, and SIGTERM is Chrome's graceful exit,
/// which saves the session; a Chrome left running would keep its old worker and take
/// a new launch over, flags ignored. The wait is on the process, not the symlink:
/// Chrome is gone 0.1 s after the signal and leaves the symlink behind (measured), and
/// a launch takes a stale lock over by itself. Says whether one was quit.
#[cfg(unix)]
pub fn quit_chrome_on(dir: &Path) -> bool {
    let Ok(target) = fs::read_link(dir.join("SingletonLock")) else { return false };
    let Some(pid) = target
        .to_string_lossy()
        .rsplit('-')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return false;
    };
    let _ = Command::new("kill").arg(pid.to_string()).status();
    let dir = dir.to_path_buf();
    for _ in 0..80 {
        if !crate::browser::profile_locked_by_live_chrome(&dir) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    true
}

/// Windows: the Chrome on `dir` is found the way a second Chrome finds it, by its
/// Chrome_MessageWindow, which is named after the user data folder, and quit through
/// Chrome's own log-off path: WM_ENDSESSION on one of its browser windows saves every
/// window as the session and ends the process (chrome::SessionEnding), the function
/// SIGTERM runs on macOS and Linux, so like SIGTERM it asks no "Leave site?" question.
/// Measured on Chrome 154: gone in 0.2 s, exit type SessionEnded, both windows and
/// every tab back with --restore-last-session, where taskkill's WM_CLOSE left Chrome
/// running and restored nothing. Says whether it is gone. A Chrome already on its way
/// out (the person closed it during take_chrome's wait, so its windows or its
/// Chrome_MessageWindow are gone) gets 3 s to finish; one with no window to take the
/// message (kept alive by a background app), or refusing it (running elevated), is
/// left running, and `take_chrome` says so.
#[cfg(windows)]
pub fn quit_chrome_on(dir: &Path) -> bool {
    let sent = win::chrome_on(dir).is_some_and(win::end_session);
    let dir = dir.to_path_buf();
    for _ in 0..if sent { 80 } else { 12 } {
        if !crate::browser::profile_locked_by_live_chrome(&dir) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::path::Path;

    type Hwnd = *mut c_void;
    const WM_ENDSESSION: u32 = 0x0016;
    const ENDSESSION_CLOSEAPP: isize = 0x1;

    #[link(name = "user32")]
    extern "system" {
        fn FindWindowExW(parent: Hwnd, after: Hwnd, class: *const u16, title: *const u16) -> Hwnd;
        fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, len: i32) -> i32;
        fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
        fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    fn same_dir(a: &str, b: &str) -> bool {
        let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
        norm(a) == norm(b)
    }

    /// The pid of the Chrome holding `dir`: its ProcessSingleton window (message-only,
    /// class Chrome_MessageWindow) carries the user data folder as its title. Other
    /// Chromium apps (Edge, WebView2, CEF) have their own folders there.
    pub fn chrome_on(dir: &Path) -> Option<u32> {
        let want = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let want = want.to_string_lossy();
        let class: Vec<u16> = "Chrome_MessageWindow".encode_utf16().chain([0]).collect();
        let message_only = -3isize as Hwnd; // HWND_MESSAGE
        let mut hwnd: Hwnd = std::ptr::null_mut();
        loop {
            hwnd = unsafe { FindWindowExW(message_only, hwnd, class.as_ptr(), std::ptr::null()) };
            if hwnd.is_null() {
                return None;
            }
            let mut text = [0u16; 1024];
            let n = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
            if same_dir(&String::from_utf16_lossy(&text[..n.max(0) as usize]), &want) {
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                return (pid != 0).then_some(pid);
            }
        }
    }

    /// WM_ENDSESSION to one browser window of `pid` (browser.rs windows_of: its
    /// frames, visible and owned by nothing). False when it has none, or Windows
    /// refused the message (a Chrome running elevated takes none from here).
    pub fn end_session(pid: u32) -> bool {
        match crate::browser::win_tabs::windows_of(pid).first() {
            Some(&(hwnd, _)) => unsafe {
                PostMessageW(hwnd as Hwnd, WM_ENDSESSION, 1, ENDSESSION_CLOSEAPP) != 0
            },
            None => false,
        }
    }
}

/// Extension mode's bridge with the browser on the line, for the agent service: the
/// extension staged into its usual place (stage_bridge::default_dest), the server
/// open, and Chrome taken (`Bridge::take_chrome`).
pub fn open_on_chrome(src: &Path, user_data_dir: Option<PathBuf>) -> SResult<Arc<Mutex<Bridge>>> {
    let dest = stage_bridge::default_dest();
    let mut bridge = Bridge::open(src, &dest)
        .map_err(|e| ScanErr::s(format!("could not open the bridge to AutoCuaBridge: {e}")))?;
    bridge.take_chrome(user_data_dir)?;
    Ok(Arc::new(Mutex::new(bridge)))
}

/// The websocket handshake with the extension, or nothing: refused without this
/// launch's token in the path or from any origin but the extension's own, and a
/// plain TCP connection that is not a websocket is dropped the same way.
pub fn accept_extension(stream: TcpStream, token: &str, origin: &str) -> Option<WebSocket<TcpStream>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let path = format!("/bridge?token={token}");
    let guard = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let ok = req.uri().path_and_query().is_some_and(|p| p.as_str() == path)
            && req.headers().get("origin").and_then(|v| v.to_str().ok()) == Some(origin);
        if ok {
            Ok(resp)
        } else {
            let mut refused = ErrorResponse::new(Some("forbidden".to_string()));
            *refused.status_mut() = StatusCode::FORBIDDEN;
            Err(refused)
        }
    };
    accept_hdr(stream, guard).ok()
}

pub struct Bridge {
    listener: TcpListener,
    port: u16,
    token: String,
    origin: String,
    staged: PathBuf,
    extension_id: String,
    conn: Option<WebSocket<TcpStream>>,
    next_id: u64,
    /// What the extension reported on its own, in the model's words.
    notices: VecDeque<String>,
    /// Popups (alerts, confirms, prompts) open right now, by tab id: what tools.js
    /// reported when one opened (`{type: "dialog", tabId, params}`, params null once
    /// it closed).
    /// The page is frozen until the model answers it (browser.rs `open_dialog`).
    dialogs: HashMap<i64, Value>,
    /// Tabs the extension closed and opened again because their page froze or crashed
    /// (tools.js toolsReopen): old tab id -> the tab that took its place.
    reopened: HashMap<i64, i64>,
    /// Requests the extension made (string ids) that nothing here answers, for
    /// whoever reads them: none today, the Test button's `save` being answered
    /// in `stash`.
    inbox: VecDeque<Value>,
    /// The run lock (`hold_run_lock`), held for as long as this bridge lives and
    /// never read: a run's has one, the app's idle line none.
    _run_lock: Option<File>,
    /// How Chrome was started from here (`launch_chrome`): its user data folder (None:
    /// the person's own), --restore-last-session, and the extra switches. What
    /// `relaunch` starts again when Chrome goes away mid-run; None when Chrome was not
    /// started from here.
    launched: Option<(Option<PathBuf>, bool, Vec<String>)>,
}

impl Drop for Bridge {
    /// Off the list the extension dials: this agent is gone.
    fn drop(&mut self) {
        let _ = config_write(&self.staged, self.port, |_| {});
    }
}

impl Bridge {
    /// Stage the extension from `src` into `dest`, open the server on a free port
    /// and write the extension's config.json. Chrome is not started here. The run
    /// lock is held for as long as the bridge lives: the app's idle line steps aside.
    pub fn open(src: &Path, dest: &Path) -> io::Result<Bridge> {
        Self::open_with(src, dest, true)
    }

    /// `open`, holding the run lock or not: the app's idle line (`idle_serve`) holds
    /// none, being what the lock tells to step aside.
    fn open_with(src: &Path, dest: &Path, run: bool) -> io::Result<Bridge> {
        let _run_lock = if run { hold_run_lock() } else { None };
        let staged = stage_bridge::stage(src, dest)?;
        let extension_id = stage_bridge::extension_id(&staged);
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = token()?;
        config_write(&staged, port, |entries| entries.push(json!({ "port": port, "token": token })))?;
        Ok(Bridge {
            listener,
            port,
            origin: format!("chrome-extension://{extension_id}"),
            token,
            staged,
            extension_id,
            conn: None,
            next_id: 0,
            notices: VecDeque::new(),
            dialogs: HashMap::new(),
            reopened: HashMap::new(),
            inbox: VecDeque::new(),
            _run_lock,
            launched: None,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn staged(&self) -> &Path {
        &self.staged
    }

    pub fn extension_id(&self) -> &str {
        &self.extension_id
    }

    /// Take a connection waiting on the listener, if there is one. A new worker
    /// (Chrome restarted the extension) replaces the connection held so far.
    fn try_accept(&mut self) -> bool {
        match self.listener.accept() {
            Ok((stream, _)) => match accept_extension(stream, &self.token, &self.origin) {
                Some(ws) => {
                    self.conn = Some(ws);
                    self.dialogs.clear();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    /// Note how Chrome was started, for `relaunch`.
    pub fn note_launch(&mut self, user_data_dir: Option<PathBuf>, restore: bool, extra: Vec<String>) {
        self.launched = Some((user_data_dir, restore, extra));
    }

    /// Chrome went away mid-run (crashed, killed or quit): it was started from here,
    /// the extension is not on the line, and no live Chrome holds its user data folder
    /// any more. A Chrome still up whose extension is only dialling back in is not gone.
    pub fn chrome_gone(&mut self) -> bool {
        let Some(dir) = self
            .launched
            .as_ref()
            .map(|(dir, _, _)| dir.clone().unwrap_or_else(default_user_data_dir))
        else {
            return false;
        };
        if self.connected() {
            return false;
        }
        !crate::browser::profile_locked_by_live_chrome(&dir)
    }

    /// Start Chrome again the way it was started, and wait for the extension to dial
    /// back in. False when Chrome was not started from here, or it never dialled in.
    pub fn relaunch(&mut self) -> SResult<bool> {
        let Some((dir, restore, extra)) = self.launched.clone() else {
            return Ok(false);
        };
        self.conn = None;
        self.dialogs.clear();
        self.reopened.clear();
        launch_chrome(&self.staged, dir.as_deref(), restore, &extra)?;
        self.wait_connected(RELAUNCH_WAIT)
    }

    /// Extension mode's browser, on the line: the person's own Chrome (`user_data_dir`
    /// None), or the folder named (a test, a profile of the agent's own). A Chrome
    /// already running with AutoCuaBridge needs no start: its worker reads config.json
    /// again each time it dials, so it is here within ATTACH_WAIT of this bridge
    /// opening. One running without it (started from the Dock, say) is quit,
    /// gracefully, and started again with the flags and its session; none running is
    /// started. The launch is noted either way, so `relaunch` brings Chrome back the
    /// same way (its session with it) if it goes away mid-run. Two runs at once on one
    /// browser are not a thing: the second would take it from the first.
    pub fn take_chrome(&mut self, user_data_dir: Option<PathBuf>) -> SResult<()> {
        let dir = user_data_dir.clone().unwrap_or_else(default_user_data_dir);
        let mut quit = false;
        if crate::browser::profile_locked_by_live_chrome(&dir) {
            if self.wait_connected(ATTACH_WAIT)? {
                self.note_launch(user_data_dir, true, Vec::new());
                return Ok(());
            }
            // Nobody dialled: the worker is stopped, or this Chrome has no
            // extension. Wake it before concluding the latter: its wake-up page,
            // opened as a tab of this Chrome, starts the worker and closes itself.
            if wake_extension(&self.extension_id) && self.wait_connected(WAKE_WAIT)? {
                self.note_launch(user_data_dir, true, Vec::new());
                return Ok(());
            }
            quit = quit_chrome_on(&dir);
            if !quit {
                return Err(ScanErr::s(format!(
                    "Chrome is running on {} without the AutoCua extension and could not be quit \
                     from here: quit Chrome and start the run again",
                    dir.display()
                )));
            }
            println!("Quit the Chrome running without the AutoCua extension; it comes back with its tabs.");
        }
        launch_chrome(&self.staged, user_data_dir.as_deref(), quit, &[])?;
        self.note_launch(user_data_dir, true, Vec::new());
        if !self.wait_connected(RELAUNCH_WAIT)? {
            return Err(ScanErr::s(
                "Chrome started, but the AutoCua extension did not dial in \
                 (check that this Chrome still accepts the extension launch flags)",
            ));
        }
        Ok(())
    }

    /// Whether the extension is on the line (taking its connection if it is waiting).
    pub fn connected(&mut self) -> bool {
        if self.conn.is_none() {
            self.try_accept();
        }
        self.conn.is_some()
    }

    /// Wait up to `timeout` for the extension to dial in. Ctrl-C ends the wait.
    pub fn wait_connected(&mut self, timeout: Duration) -> SResult<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.connected() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            crate::browser::check_py_signals()?;
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn lost(&mut self, kind: &str) -> ScanErr {
        self.conn = None;
        ScanErr::s(format!("{kind}: the connection to AutoCuaBridge was lost"))
    }

    /// One request, answered. The extension's own messages read on the way are kept
    /// (`stash`); a reply to a request given up on earlier is dropped.
    pub fn request(&mut self, mut payload: Value, timeout: Duration) -> SResult<Value> {
        let kind = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("request")
            .to_string();
        if !self.wait_connected(CONNECT_WAIT)? {
            return Err(ScanErr::s(
                "AutoCuaBridge is not connected: Chrome is not running with the extension, \
                 or the extension has not dialled in yet",
            ));
        }
        self.next_id += 1;
        let id = self.next_id;
        payload["id"] = json!(id);
        let text = payload.to_string();
        {
            let ws = self.conn.as_mut().expect("connected above");
            ws.get_ref().set_read_timeout(Some(READ_SLICE)).ok();
            if ws.send(Message::Text(text)).is_err() {
                return Err(self.lost(&kind));
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            // Checked on every pass, not only when the line is quiet: a steady stream
            // of messages from the extension must not keep a request waiting past its
            // time, or deaf to Ctrl-C (Cdp::rpc does the same).
            crate::browser::check_py_signals()?;
            if Instant::now() >= deadline {
                return Err(ScanErr::s(format!(
                    "{kind}: no answer from AutoCuaBridge in {}s",
                    timeout.as_secs_f64()
                )));
            }
            let text = match self.conn.as_mut().expect("connected above").read() {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => return Err(self.lost(&kind)),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                Err(_) => return Err(self.lost(&kind)),
            };
            let Ok(m) = serde_json::from_str::<Value>(&text) else { continue };
            if m.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = m.get("error") {
                    return Err(ScanErr::s(
                        err.as_str().map(str::to_string).unwrap_or_else(|| err.to_string()),
                    ));
                }
                return Ok(m.get("result").cloned().unwrap_or(Value::Null));
            }
            self.stash(m);
        }
    }

    /// Read what the extension sent on its own for up to `wait`, asking it nothing:
    /// notices and its own requests. Ctrl-C ends the read.
    pub fn poll(&mut self, wait: Duration) -> SResult<()> {
        self.poll_inner(wait, true)
    }

    /// `poll`, looking for Ctrl-C between reads or not: the app's idle line
    /// (`idle_serve`) runs on a thread of its own and must not touch Python.
    fn poll_inner(&mut self, wait: Duration, signals: bool) -> SResult<()> {
        if !self.connected() {
            return Ok(());
        }
        let deadline = Instant::now() + wait;
        let slice = wait.max(Duration::from_millis(1)).min(READ_SLICE);
        loop {
            let ws = self.conn.as_mut().expect("connected above");
            ws.get_ref().set_read_timeout(Some(slice)).ok();
            match ws.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(m) = serde_json::from_str::<Value>(&t) {
                        self.stash(m);
                    }
                }
                Ok(Message::Close(_)) => {
                    self.conn = None;
                    return Ok(());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => {
                    self.conn = None;
                    return Ok(());
                }
            }
            if signals {
                crate::browser::check_py_signals()?;
            }
            if Instant::now() >= deadline {
                return Ok(());
            }
        }
    }

    /// A message that is not the answer being waited for: a notice is kept for the
    /// model, a request from the extension (string id) for the inbox, anything else
    /// (a keepalive, a late answer) is dropped.
    fn stash(&mut self, m: Value) {
        match m.get("type").and_then(Value::as_str) {
            Some("notice") => {
                if let Some(text) = m.get("text").and_then(Value::as_str) {
                    self.notice(text);
                }
            }
            Some("dialog") => {
                if let Some(tab) = m.get("tabId").and_then(Value::as_i64) {
                    match m.get("params") {
                        Some(p) if !p.is_null() => {
                            self.dialogs.insert(tab, p.clone());
                        }
                        _ => {
                            self.dialogs.remove(&tab);
                        }
                    }
                }
            }
            Some("reopened") => {
                let tab = |k: &str| m.get(k).and_then(Value::as_i64);
                if let (Some(from), Some(to)) = (tab("from"), tab("to")) {
                    self.reopened.insert(from, to);
                }
            }
            Some("keepalive") | None => {}
            // The Test button (AutoCuaBridge/test.html): the extension's own scan of
            // a page, written under debug/iteration_N/ the way the hand test writes it
            // (bridge_test.rs save_next) and answered with where it went. Whichever
            // bridge is on the line does it: a run's, or the app's idle one.
            Some("save") if m.get("id").is_some_and(Value::is_string) => {
                let id = m["id"].clone();
                let outcome = crate::bridge_test::save_next(&m)
                    .map(|(dir, n)| json!({ "dir": dir.display().to_string(), "iteration": n }));
                self.reply(&id, outcome);
            }
            Some(_) => {
                if m.get("id").is_some_and(Value::is_string) {
                    self.inbox.push_back(m);
                }
            }
        }
    }

    /// The popup open on that tab, if one is, reading first whatever the
    /// extension has already sent (without waiting for more).
    pub fn dialog(&mut self, tab: i64) -> Option<Value> {
        self.read_waiting();
        self.dialogs.get(&tab).cloned()
    }

    /// That tab's popup was answered (browser.rs answer_dialog).
    pub fn forget_dialog(&mut self, tab: i64) {
        self.dialogs.remove(&tab);
    }

    /// Which of `tabs` the extension closed and opened again because their page froze
    /// or crashed (tools.js toolsReopen), each with the tab now in its place (followed
    /// to the last, if that one was replaced too), reading first whatever the extension
    /// has already sent.
    pub fn reopened(&mut self, tabs: &[i64]) -> Vec<(i64, i64)> {
        self.read_waiting();
        if self.reopened.is_empty() {
            return Vec::new();
        }
        tabs.iter()
            .filter_map(|&tab| {
                let mut now = tab;
                for _ in 0..16 {
                    match self.reopened.get(&now) {
                        Some(next) => now = *next,
                        None => break,
                    }
                }
                (now != tab).then_some((tab, now))
            })
            .collect()
    }

    /// Read what is already on the wire, never waiting: a check between steps costs
    /// microseconds (Cdp::drain does the same for a debugging port).
    fn read_waiting(&mut self) {
        let Some(ws) = self.conn.as_mut() else { return };
        if ws.get_ref().set_nonblocking(true).is_err() {
            return;
        }
        let mut seen = Vec::new();
        let mut lost = false;
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(m) = serde_json::from_str::<Value>(&t) {
                        seen.push(m);
                    }
                }
                Ok(Message::Close(_)) => {
                    lost = true;
                    break;
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    lost = true;
                    break;
                }
            }
        }
        if lost {
            self.conn = None;
        } else if let Some(ws) = self.conn.as_mut() {
            ws.get_ref().set_nonblocking(false).ok();
        }
        for m in seen {
            self.stash(m);
        }
    }

    /// Queue something for the model, newest kept, never twice (browser.rs notice()).
    pub fn notice(&mut self, text: &str) {
        if self.notices.iter().any(|n| n == text) {
            return;
        }
        while self.notices.len() >= NOTICE_CAP {
            self.notices.pop_front();
        }
        self.notices.push_back(text.to_string());
    }

    pub fn take_notices(&mut self) -> Vec<String> {
        self.notices.drain(..).collect()
    }

    pub fn take_inbox(&mut self) -> Vec<Value> {
        self.inbox.drain(..).collect()
    }

    /// Answer a request from the inbox. False when the extension is gone.
    pub fn reply(&mut self, id: &Value, outcome: Result<Value, String>) -> bool {
        let msg = match outcome {
            Ok(result) => json!({ "id": id, "result": result }),
            Err(error) => json!({ "id": id, "error": error }),
        };
        match self.conn.as_mut() {
            Some(ws) => ws.send(Message::Text(msg.to_string())).is_ok(),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The run lock, and the app's idle line.
// ---------------------------------------------------------------------------

/// The file every bridge of a run holds a shared lock on while it lives, beside
/// the staged copy (~/.AutoCua/AutoCuaBridge.lock). How the app's idle line knows
/// a run is on (`run_on`), from any process, a crashed one included: the system
/// drops its lock with it.
fn run_lock_file() -> io::Result<File> {
    let dest = stage_bridge::default_dest();
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_else(|| dest.clone());
    fs::create_dir_all(&dir)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(dir.join("AutoCuaBridge.lock"))
}

/// Hold the run lock, shared with every other run: Some for as long as the file
/// lives, None when it could not be taken (the idle line then stays on, harmless).
pub(crate) fn hold_run_lock() -> Option<File> {
    let file = run_lock_file().ok()?;
    for _ in 0..50 {
        match file.try_lock_shared() {
            Ok(()) => return Some(file),
            // The idle line's own check holds it exclusively for an instant.
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
            Err(std::fs::TryLockError::Error(_)) => return None,
        }
    }
    None
}

/// Whether a run holds the lock: an exclusive try fails while any shared hold lives.
fn run_on() -> bool {
    let Ok(file) = run_lock_file() else { return false };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// How often the idle line looks for a run, and how long each of its reads waits.
const IDLE_CHECK: Duration = Duration::from_millis(500);
const IDLE_SLICE: Duration = Duration::from_millis(200);

/// The idle line's thread, with the flag that ends it (`idle_bridge_stop`).
static IDLE: Mutex<Option<(Arc<AtomicBool>, JoinHandle<()>)>> = Mutex::new(None);

/// Sleep `wait`, in slices, unless stopped.
fn nap(stop: &AtomicBool, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The app's line to the extension while no run is on: a bridge that asks the
/// browser nothing and answers what the extension sends by itself (`stash`: the
/// Test button's save). Open whenever no run holds the lock, and dropped (off
/// config.json, its line closed) within IDLE_CHECK of one taking it. Never
/// touches Python: the app may be on its way out.
fn idle_serve(src: PathBuf, dest: PathBuf, stop: Arc<AtomicBool>) {
    let mut bridge: Option<Bridge> = None;
    let mut checked: Option<Instant> = None;
    let mut failed = false;
    while !stop.load(Ordering::Relaxed) {
        if checked.is_none_or(|t| t.elapsed() >= IDLE_CHECK) {
            checked = Some(Instant::now());
            if run_on() {
                bridge = None;
                nap(&stop, IDLE_CHECK);
                continue;
            }
            if bridge.is_none() {
                match Bridge::open_with(&src, &dest, false) {
                    Ok(b) => {
                        bridge = Some(b);
                        failed = false;
                    }
                    Err(e) => {
                        if !failed {
                            println!("The app's line to AutoCuaBridge could not open: {e}");
                            failed = true;
                        }
                        nap(&stop, Duration::from_secs(2));
                        continue;
                    }
                }
            }
        }
        match bridge.as_mut() {
            Some(b) => {
                if b.connected() {
                    let _ = b.poll_inner(IDLE_SLICE, false);
                } else {
                    nap(&stop, IDLE_SLICE);
                }
            }
            None => nap(&stop, IDLE_SLICE),
        }
    }
}

/// Keep a line to the extension open while no run is on (`idle_serve`), from the
/// desktop app's process (ui/service.py). A second call while one is on does nothing.
#[pyfunction]
#[pyo3(signature = (src=None))]
pub fn idle_bridge_start(py: Python<'_>, src: Option<String>) -> PyResult<()> {
    let src = match src {
        Some(s) => PathBuf::from(s),
        None => crate::browser_dir(py)?.join("AutoCuaBridge"),
    };
    let dest = stage_bridge::default_dest();
    let mut slot = IDLE
        .lock()
        .map_err(|_| crate::ScannerError::new_err("idle bridge state poisoned by an earlier panic"))?;
    if slot.is_some() {
        return Ok(());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("AutoCua idle bridge".into())
        .spawn(move || idle_serve(src, dest, flag))?;
    *slot = Some((stop, thread));
    Ok(())
}

/// End the idle line: its bridge is dropped (off config.json, its line closed)
/// and the thread joined. Nothing to do when none is on.
#[pyfunction]
pub fn idle_bridge_stop(py: Python<'_>) -> PyResult<()> {
    let taken = IDLE
        .lock()
        .map_err(|_| crate::ScannerError::new_err("idle bridge state poisoned by an earlier panic"))?
        .take();
    if let Some((stop, thread)) = taken {
        stop.store(true, Ordering::Relaxed);
        py.detach(move || {
            let _ = thread.join();
        });
    }
    Ok(())
}

fn lock(b: &Arc<Mutex<Bridge>>) -> SResult<std::sync::MutexGuard<'_, Bridge>> {
    b.lock()
        .map_err(|_| ScanErr::s("bridge state poisoned by an earlier panic"))
}

/// `agent_native.Bridge`: the server side of extension mode, for Python. Staging the
/// extension and opening the port happen on construction; `launch_chrome` starts a
/// Chrome that loads it; `wait_connected` waits for the extension to dial in;
/// `BrowserScanner.over_bridge(bridge)` then drives that Chrome through it.
#[pyclass(name = "Bridge")]
pub struct PyBridge {
    pub inner: Arc<Mutex<Bridge>>,
}

#[pymethods]
impl PyBridge {
    #[new]
    #[pyo3(signature = (dest=None, src=None))]
    fn new(py: Python<'_>, dest: Option<String>, src: Option<String>) -> PyResult<Self> {
        let src = match src {
            Some(s) => PathBuf::from(s),
            None => crate::browser_dir(py)?.join("AutoCuaBridge"),
        };
        let dest = dest.map(PathBuf::from).unwrap_or_else(stage_bridge::default_dest);
        let bridge = Bridge::open(&src, &dest).map_err(|e| {
            crate::ScannerError::new_err(format!("could not open the bridge to AutoCuaBridge: {e}"))
        })?;
        Ok(PyBridge { inner: Arc::new(Mutex::new(bridge)) })
    }

    #[getter]
    fn port(&self) -> PyResult<u16> {
        Ok(lock(&self.inner)?.port())
    }

    #[getter]
    fn token(&self) -> PyResult<String> {
        Ok(lock(&self.inner)?.token().to_string())
    }

    #[getter]
    fn staged(&self) -> PyResult<String> {
        Ok(lock(&self.inner)?.staged().display().to_string())
    }

    #[getter]
    fn extension_id(&self) -> PyResult<String> {
        Ok(lock(&self.inner)?.extension_id().to_string())
    }

    /// Start Chrome with the staged extension: on `profile` (a user data folder of its
    /// own) when given, else on the user's own profile. `chrome_args` adds switches.
    #[pyo3(signature = (profile=None, chrome_args=None, restore=false))]
    fn launch_chrome(
        &self,
        py: Python<'_>,
        profile: Option<String>,
        chrome_args: Option<Vec<String>>,
        restore: bool,
    ) -> PyResult<()> {
        let dir = profile.map(PathBuf::from);
        let extra = chrome_args.unwrap_or_default();
        // Kept so that a Chrome that goes away mid-run is started again the same way.
        let staged = {
            let mut b = lock(&self.inner)?;
            b.note_launch(dir.clone(), restore, extra.clone());
            b.staged().to_path_buf()
        };
        py.detach(|| launch_chrome(&staged, dir.as_deref(), restore, &extra))?;
        Ok(())
    }

    /// Extension mode's browser on the line (Bridge::take_chrome): the person's own
    /// Chrome, or the user data folder named.
    #[pyo3(signature = (profile=None))]
    fn take_chrome(&self, py: Python<'_>, profile: Option<String>) -> PyResult<()> {
        let inner = self.inner.clone();
        let dir = profile.map(PathBuf::from);
        py.detach(move || lock(&inner)?.take_chrome(dir)).map_err(PyErr::from)
    }

    /// Whether the extension is on the line.
    fn connected(&self) -> PyResult<bool> {
        Ok(lock(&self.inner)?.connected())
    }

    /// Wait up to `timeout` seconds for the extension to dial in.
    #[pyo3(signature = (timeout=30.0))]
    fn wait_connected(&self, py: Python<'_>, timeout: f64) -> PyResult<bool> {
        let inner = self.inner.clone();
        py.detach(move || lock(&inner)?.wait_connected(Duration::from_secs_f64(timeout.max(0.0))))
            .map_err(PyErr::from)
    }

    /// One request to the extension by hand: `{"type": ..., ...}` in, its result out.
    #[pyo3(signature = (payload, timeout=10.0))]
    fn request<'py>(
        &self,
        py: Python<'py>,
        payload: Bound<'py, PyAny>,
        timeout: f64,
    ) -> PyResult<Bound<'py, PyAny>> {
        let value: Value = pythonize::depythonize(&payload)?;
        let inner = self.inner.clone();
        let result = py
            .detach(move || lock(&inner)?.request(value, Duration::from_secs_f64(timeout.max(0.001))))
            .map_err(PyErr::from)?;
        pythonize::pythonize(py, &result).map_err(Into::into)
    }

    /// What the extension reported on its own since the last call.
    fn notices(&self) -> PyResult<Vec<String>> {
        Ok(lock(&self.inner)?.take_notices())
    }
}
