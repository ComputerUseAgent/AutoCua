//! Put the agent's browser back where it was when a PAGE pulls it forward.
//!
//! The agent itself never brings its browser forward: it opens its tabs in
//! the background and shows the one it drives by moving the tab strip alone,
//! never by activating the window (browser.rs: open_tab_impl, show_tab), so
//! the browser stays wherever the person puts it. A page still can: a
//! target=_blank link, window.open or a pop-up opens a tab, and Chrome shows
//! that tab by activating its window. No CDP call stops that, and on macOS it
//! took the screen and the keyboard from the app in front (measured, Brave
//! 154).
//!
//! So this hands the front straight back in exactly that case and no other:
//! the agent's browser becoming the active app just after a tab appeared in
//! it. Chrome announced the tab 2.4 to 12.5 ms before the browser became
//! active (measured, 39 of 39). Any new tab counts, not only one Chrome marks
//! as opened by a page (openerId): a click the page or the browser treats as
//! a modified one (Cmd+click, Shift+click, a middle click, or a script firing
//! such a click itself) has the BROWSER open the tab, with no opener, and
//! pulls the browser forward just the same (measured). A Google ad link that
//! opened its page in a new tab went unanswered in a real run while only
//! tabs with an opener counted. The tabs the agent opens itself never activate the
//! browser, so they meet no activation to hand back. The browser coming
//! forward any other way, the person clicking it, the Dock, Cmd-Tab or a
//! launch, is left alone.
//!
//! macOS only. On Windows the foreground lock already refuses a page's tab
//! and flashes the taskbar button instead; on a Wayland desktop the compositor
//! shows a "... is ready" banner (measured by the Windows and Linux sessions).
//!
//! `start` is called (lib.rs, ensure_browser) every time a web run is about to
//! use a headful browser; a headless one can never come forward. It returns
//! at once when a guard already watches the port, and never fails: without
//! one, only a page opening a tab brings the browser forward.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What `start` last started a guard with, for `restart`.
static STARTED_WITH: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);

/// Make sure a guard is watching the browser on `port`. `python` and
/// `package_parent` (the folder holding the AutoCua package) start the
/// guard's own process.
pub fn start(port: u16, python: &Path, package_parent: &Path) {
    if let Ok(mut with) = STARTED_WITH.lock() {
        *with = Some((python.to_path_buf(), package_parent.to_path_buf()));
    }
    #[cfg(target_os = "macos")]
    macos::start(port, python, package_parent);
    #[cfg(not(target_os = "macos"))]
    let _ = port;
}

/// `start` again, the way this process last did, after the scanner relaunched
/// a browser that died (browser.rs, revive). The guard outlives its browser
/// by a minute only, and a relaunch waits for the agent's next step, which a
/// slow model call can put further off than that.
pub fn restart(port: u16) {
    let with = STARTED_WITH.lock().ok().and_then(|with| with.clone());
    if let Some((python, package_parent)) = with {
        start(port, &python, &package_parent);
    }
}

/// The guard process's whole life (page_tab_guard_serve in lib.rs): returns
/// only when another guard already watches `port`. Elsewhere, nothing.
pub fn serve(port: u16) {
    #[cfg(target_os = "macos")]
    macos::serve(port);
    #[cfg(not(target_os = "macos"))]
    let _ = port;
}

/// macOS. One process, two ears:
///   - a thread on the browser's DevTools socket is told of every new tab
///     (Target.setDiscoverTargets) and notes when the last one appeared;
///   - the main run loop hears the browser become the active app
///     (NSWorkspaceDidActivateApplicationNotification) and, when a tab has
///     just appeared, activates the app that had the front.
///
/// A process of its own, on its own main run loop: macOS activates another
/// app at once only when asked from a main thread (from any other thread each
/// request waits a second, measured), and the main thread of the agent's
/// process runs the agent. A library cannot be a process, so it is a plain
/// `python -c` that calls straight back into this extension
/// (page_tab_guard_serve); the packaged app has no Python to run it with, so
/// it runs unguarded. One per debug port, by a lock file. It outlives the run
/// that started it, as the browser does, and exits a minute after the browser
/// has gone. A browser relaunched within that minute is heard again on the
/// next check; the scanner's revive also calls `restart` for a later one.
#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::ffi::{c_char, c_void};
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU16, AtomicU64, Ordering};
    use std::sync::{mpsc, OnceLock};
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    type Id = *mut c_void;
    type Sel = *const c_void;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> Id;
        fn sel_registerName(name: *const c_char) -> Sel;
        fn objc_msgSend();
        fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
        fn objc_registerClassPair(class: Id);
        fn class_addMethod(class: Id, name: Sel, imp: *const c_void, types: *const c_char) -> i8;
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopRun();
        fn CFAbsoluteTimeGetCurrent() -> f64;
        fn CFRunLoopTimerCreate(allocator: *const c_void, fire: f64, interval: f64, flags: usize,
                                order: isize, callout: extern "C" fn(*mut c_void, *mut c_void),
                                context: *mut c_void) -> *mut c_void;
        fn CFRunLoopAddTimer(run_loop: *mut c_void, timer: *mut c_void, mode: *const c_void);
    }

    extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        fn sysctl(name: *mut i32, namelen: u32, old: *mut c_void, oldlen: *mut usize,
                  new: *mut c_void, newlen: usize) -> i32;
        fn flock(fd: i32, operation: i32) -> i32;
        fn setsid() -> i32;
    }

    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    const LOCK_UN: i32 = 8;
    const RTLD_NOW: i32 = 2;
    const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;
    /// How long a guard outlives its browser before it exits.
    const GONE_GRACE: Duration = Duration::from_secs(60);
    /// How recent a new tab must be to explain the browser becoming
    /// active. The measured gap was 2.4 to 12.5 ms; this leaves room for a
    /// slow first hand-back (about 40 ms, measured).
    const TAB_WINDOW_MS: u64 = 150;

    static FLAG: OnceLock<String> = OnceLock::new();
    static PORT: AtomicU16 = AtomicU16::new(0);
    /// NSWorkspaceApplicationKey, looked up in AppKit at start.
    static APP_KEY: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
    /// When a tab last appeared in the agent's browser, in ms on `now_ms`'s
    /// clock; 0 for never.
    static PAGE_TAB_AT: AtomicU64 = AtomicU64::new(0);
    /// Whether the socket thread is up.
    static LISTENING: AtomicBool = AtomicBool::new(false);

    thread_local! {
        /// The apps the front can go back to, by pid, most recent first.
        static RECENT: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
        static GONE_SINCE: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    /// Milliseconds since this process first asked, never 0.
    fn now_ms() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
    }

    fn lock_path(port: u16) -> PathBuf {
        let folder = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join("Library/Caches/AutoCua");
        let _ = std::fs::create_dir_all(&folder);
        folder.join(format!("page_tab_guard-{port}.lock"))
    }

    /// Whether a guard already holds the lock for `port`.
    fn guarded(port: u16) -> bool {
        let Ok(probe) = OpenOptions::new().write(true).create(true).open(lock_path(port)) else {
            return true;
        };
        unsafe {
            if flock(probe.as_raw_fd(), LOCK_EX | LOCK_NB) != 0 {
                return true;
            }
            flock(probe.as_raw_fd(), LOCK_UN);
        }
        false
    }

    /// The open lock file once this process is the one guard for `port`,
    /// else None. Held, never closed, for as long as the process lives.
    fn hold_lock(port: u16) -> Option<File> {
        let lock = OpenOptions::new().write(true).create(true).open(lock_path(port)).ok()?;
        (unsafe { flock(lock.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0).then_some(lock)
    }

    /// Returns at once when a guard is already up. A new one is waited for
    /// (about half a second, once) until it hears the apps change and knows
    /// the one in front, so both hold before the browser launches. Its tab
    /// socket connects on the first check after the browser answers, long
    /// before a page of the run can open a tab.
    pub fn start(port: u16, python: &Path, package_parent: &Path) {
        // No Python to start it with: a packaged app, where sys.executable
        // is the app itself. The browser runs unguarded there.
        if python.as_os_str().is_empty() || guarded(port) {
            return;
        }
        let mut path = package_parent.as_os_str().to_os_string();
        if let Some(rest) = std::env::var_os("PYTHONPATH") {
            path.push(":");
            path.push(rest);
        }
        let mut cmd = Command::new(python);
        cmd.arg("-c")
            .arg(format!(
                "from AutoCua.web.agent_native import page_tab_guard_serve; page_tab_guard_serve({port})"
            ))
            .env("PYTHONPATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Its own session: it outlives the run that started it, as the
        // browser does, and a Ctrl+C in the terminal does not reach it.
        unsafe {
            cmd.pre_exec(|| {
                setsid();
                Ok(())
            });
        }
        let Ok(mut child) = cmd.spawn() else {
            return;
        };
        // "ready" once it is set up; nothing (end of file) if another guard won.
        if let Some(out) = child.stdout.take() {
            let (ready, listening) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = BufReader::new(out).read_line(&mut String::new());
                let _ = ready.send(());
            });
            let _ = listening.recv_timeout(Duration::from_secs(3));
        }
        // Collected when it exits, so it never lingers as a zombie of this process.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }

    pub fn serve(port: u16) {
        let Some(_lock) = hold_lock(port) else {
            return; // another guard already watches this port
        };
        unsafe { guard(port) };
    }

    unsafe fn class(name: &[u8]) -> Id {
        objc_getClass(name.as_ptr().cast())
    }

    unsafe fn sel(name: &[u8]) -> Sel {
        sel_registerName(name.as_ptr().cast())
    }

    /// objc_msgSend as a function of the method's own signature, which is
    /// how it must be called (on arm64 especially).
    unsafe fn msg<F: Copy>() -> F {
        let send = objc_msgSend as *const c_void;
        std::mem::transmute_copy(&send)
    }

    unsafe fn get(object: Id, name: &[u8]) -> Id {
        let send: unsafe extern "C" fn(Id, Sel) -> Id = msg();
        send(object, sel(name))
    }

    unsafe fn get_with(object: Id, name: &[u8], arg: Id) -> Id {
        let send: unsafe extern "C" fn(Id, Sel, Id) -> Id = msg();
        send(object, sel(name), arg)
    }

    unsafe fn pid_of(app: Id) -> i32 {
        let send: unsafe extern "C" fn(Id, Sel) -> i32 = msg();
        send(app, sel(b"processIdentifier\0"))
    }

    /// The running app with `pid`, or null once it has quit.
    unsafe fn app_for(pid: i32) -> Id {
        let send: unsafe extern "C" fn(Id, Sel, i32) -> Id = msg();
        send(class(b"NSRunningApplication\0"), sel(b"runningApplicationWithProcessIdentifier:\0"), pid)
    }

    unsafe fn activate(app: Id) -> bool {
        let terminated: unsafe extern "C" fn(Id, Sel) -> i8 = msg();
        if app.is_null() || terminated(app, sel(b"isTerminated\0")) != 0 {
            return false;
        }
        let send: unsafe extern "C" fn(Id, Sel, usize) -> i8 = msg();
        send(app, sel(b"activateWithOptions:\0"), ACTIVATE_IGNORING_OTHER_APPS) != 0
    }

    /// A process's command line, read from the kernel (sysctl
    /// KERN_PROCARGS2). About half a millisecond, where running ps took
    /// about thirty, and the browser keeps the front until this answers.
    fn command_line(pid: i32) -> String {
        let mut mib = [1, 49, pid]; // CTL_KERN, KERN_PROCARGS2
        let mut size = 0usize;
        unsafe {
            if sysctl(mib.as_mut_ptr(), 3, null_mut(), &mut size, null_mut(), 0) != 0 {
                return String::new();
            }
            let mut buf = vec![0u8; size];
            if sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, null_mut(), 0) != 0 {
                return String::new();
            }
            buf.truncate(size);
            String::from_utf8_lossy(&buf).replace('\0', " ")
        }
    }

    /// The agent's browser is the one started with this debug port and one
    /// of the agent's profiles: that tells it from a browser the person runs
    /// themselves.
    fn is_agent(pid: i32) -> bool {
        let cmd = command_line(pid) + " ";
        FLAG.get().is_some_and(|flag| cmd.contains(flag.as_str())) && cmd.contains("browser_profiles")
    }

    fn port_open(port: u16) -> bool {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
    }

    /// Start the socket thread unless it is up: at start, and from `check`
    /// once a relaunched browser answers again.
    fn listen(port: u16) {
        if LISTENING.swap(true, Ordering::SeqCst) {
            return;
        }
        let spawned = std::thread::Builder::new().name("page-tabs".into()).spawn(move || {
            let _ = std::panic::catch_unwind(|| watch_tabs(port));
            LISTENING.store(false, Ordering::SeqCst);
        });
        if spawned.is_err() {
            LISTENING.store(false, Ordering::SeqCst);
        }
    }

    /// Note every tab that appears, until the browser goes away.
    fn watch_tabs(port: u16) -> Option<()> {
        let version = crate::browser::chrome_http(port, "/json/version", "GET").ok()?;
        let url = version.get("webSocketDebuggerUrl")?.as_str()?.to_string();
        let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
        let (mut ws, _) = tungstenite::client(url.as_str(), stream).ok()?;
        let discover = json!({"id": 1, "method": "Target.setDiscoverTargets",
                              "params": {"discover": true}});
        ws.send(tungstenite::Message::Text(discover.to_string().into())).ok()?;
        // The tabs already open are announced first, before the reply: none
        // of them is news.
        let mut caught_up = false;
        loop {
            let tungstenite::Message::Text(text) = ws.read().ok()? else {
                continue;
            };
            let Ok(message) = serde_json::from_str::<Value>(text.as_ref()) else {
                continue;
            };
            if !caught_up {
                caught_up = message.get("id").and_then(Value::as_u64) == Some(1);
                continue;
            }
            // Any new tab or window, with an opener or without (see the
            // module's note on modified clicks).
            let new_tab = message.get("method").and_then(Value::as_str)
                == Some("Target.targetCreated")
                && message.pointer("/params/targetInfo/type").and_then(Value::as_str)
                    == Some("page");
            if new_tab {
                PAGE_TAB_AT.store(now_ms(), Ordering::SeqCst);
            }
        }
    }

    /// Whether a tab appeared in the agent's browser just before it became
    /// active. Chrome announced the tab first in every pull measured (51 of
    /// 51), so only a tab already noted counts. A person clicking or tapping
    /// a page's link in the browser activates it before the tab exists, so
    /// that is never taken for a pull.
    fn page_opened_tab() -> bool {
        let at = PAGE_TAB_AT.load(Ordering::SeqCst);
        at != 0 && now_ms().saturating_sub(at) <= TAB_WINDOW_MS
    }

    extern "C" fn activated(_this: Id, _cmd: Sel, note: Id) {
        // A panic must not cross into the Objective-C runtime: it would end
        // the process.
        let _ = std::panic::catch_unwind(|| unsafe {
            let pool = objc_autoreleasePoolPush();
            on_activated(note);
            objc_autoreleasePoolPop(pool);
        });
    }

    unsafe fn on_activated(note: Id) {
        let key = APP_KEY.load(Ordering::Relaxed);
        let info = get(note, b"userInfo\0");
        if key.is_null() || info.is_null() {
            return;
        }
        let app = get_with(info, b"objectForKey:\0", key);
        if app.is_null() {
            return;
        }
        let pid = pid_of(app);
        if !is_agent(pid) {
            RECENT.with(|recent| {
                let mut recent = recent.borrow_mut();
                recent.retain(|p| *p != pid);
                recent.insert(0, pid);
                recent.truncate(5);
            });
            return;
        }
        // The agent's browser has the front. Left there unless a new tab
        // pulled it forward: then straight back to the app that had it, or
        // the next one along if that one has quit since.
        if !page_opened_tab() {
            return;
        }
        let recent = RECENT.with(|recent| recent.borrow().clone());
        for back in recent {
            if activate(app_for(back)) {
                return;
            }
        }
    }

    /// Every second: listen again once a relaunched browser answers, and
    /// leave a minute after the browser has gone for good.
    extern "C" fn check(_timer: *mut c_void, _info: *mut c_void) {
        let _ = std::panic::catch_unwind(|| {
            let port = PORT.load(Ordering::Relaxed);
            let open = port_open(port);
            if open {
                listen(port);
            }
            GONE_SINCE.with(|gone| match gone.get() {
                _ if open => gone.set(None),
                None => gone.set(Some(Instant::now())),
                Some(since) if since.elapsed() > GONE_GRACE => std::process::exit(0),
                Some(_) => {}
            });
        });
    }

    unsafe fn guard(port: u16) {
        let appkit = dlopen(b"/System/Library/Frameworks/AppKit.framework/AppKit\0".as_ptr().cast(), RTLD_NOW);
        if appkit.is_null() {
            return;
        }
        let note_name = dlsym(appkit, b"NSWorkspaceDidActivateApplicationNotification\0".as_ptr().cast())
            as *const Id;
        let app_key = dlsym(appkit, b"NSWorkspaceApplicationKey\0".as_ptr().cast()) as *const Id;
        if note_name.is_null() || app_key.is_null() {
            return;
        }
        APP_KEY.store(*app_key, Ordering::Relaxed);
        PORT.store(port, Ordering::Relaxed);
        let _ = FLAG.set(format!("--remote-debugging-port={port} "));
        let pool = objc_autoreleasePoolPush();

        // The observer: an NSObject whose activated: is the function above.
        let mut observer_class =
            objc_allocateClassPair(class(b"NSObject\0"), b"AutoCuaPageTabGuard\0".as_ptr().cast(), 0);
        if observer_class.is_null() {
            observer_class = class(b"AutoCuaPageTabGuard\0");
        } else {
            class_addMethod(observer_class, sel(b"activated:\0"), activated as *const c_void,
                            b"v@:@\0".as_ptr().cast());
            objc_registerClassPair(observer_class);
        }
        let observer = get(get(observer_class, b"alloc\0"), b"init\0");
        let workspace = get(class(b"NSWorkspace\0"), b"sharedWorkspace\0");
        let add: unsafe extern "C" fn(Id, Sel, Id, Sel, Id, Id) = msg();
        add(get(workspace, b"notificationCenter\0"), sel(b"addObserver:selector:name:object:\0"),
            observer, sel(b"activated:\0"), *note_name, null_mut());

        // Where the front goes back to until the person switches apps.
        let front = get(workspace, b"frontmostApplication\0");
        if !front.is_null() && !is_agent(pid_of(front)) {
            let pid = pid_of(front);
            RECENT.with(|recent| recent.borrow_mut().push(pid));
        }
        if port_open(port) {
            listen(port);
        }
        // Every second, `check`. It also keeps the run loop running: with
        // nothing to wait on, it would return at once.
        let timer = CFRunLoopTimerCreate(null(), CFAbsoluteTimeGetCurrent() + 1.0, 1.0, 0, 0, check,
                                         null_mut());
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopDefaultMode);
        objc_autoreleasePoolPop(pool);
        println!("ready"); // start() waits for this
        CFRunLoopRun();
    }
}
