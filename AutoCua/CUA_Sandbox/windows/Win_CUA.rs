//! AutoCua SB on Windows: a second desktop for the agent, inside your own
//! Windows session, kept by the app itself.
//!
//! Shared with your main desktop: the same user, files, internet, installed apps
//! and clipboard. Its own: its windows, its focus and its picture. Nothing on it
//! moves your mouse or takes your keyboard, and nothing on it shows on your
//! screen. The taskbar strip is cropped out of every picture.
//!
//! RAM: the desktop object itself is a few MB of desktop heap. Only the apps
//! started on it use memory, the same as opening them on your main desktop.
//!
//! This file is a part of the app's one Rust module (AutoCua.agent_native, as
//! AutoCua.agent_native.cua_sandbox; lib.rs names the file, Windows only). It
//! was a program of its own until 2026-11-27: the app compiled it into
//! Win_CUA.exe in the temp folder and started that. An antivirus (Norton's
//! Behavior Shield, "IDP.Generic") took that new, unsigned program for a
//! threat the moment it started Notepad paused on the hidden desktop, and the
//! owner asked for no .exe at all. So nothing is built or started at run time
//! now: the code runs inside the app, and a program is started on the second
//! desktop as any program starts another, never paused.
//!
//! No crates of its own: the Rust standard library, Windows' own DLLs called by
//! hand, and pyo3 for the functions Python calls
//! (CUA_Sandbox/windows/service.py and test.py):
//!     start()          make the second desktop (once), give the picture's place
//!     shot()           one picture of it
//!     scan()           its windows and what can be clicked or typed in
//!     launch(command)  start a program on it
//!     typewrite(text)  type text into the app in front there, where its cursor is
//!     hotkey(keys)     press a key or a combination there ("enter", "ctrl+s")
//!     stop()           end every app on it and let it go
//! One thread of the app lives on the second desktop and answers them one at a
//! time (device_loop): window messages, PrintWindow and UI Automation reach
//! that desktop's windows only from a thread that has joined it. The desktop
//! stays for as long as the app runs. When the app ends, however it ends,
//! Windows closes the job and every app on the second desktop ends with it.

#![allow(non_snake_case)] // the file keeps the name it has had from the start

use std::collections::HashSet;
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

const DESKTOP: &str = "AutoCuaAgent"; // the second desktop, inside window station WinSta0

/// Any Windows handle: a window, a desktop, a process, a picture.
type Handle = *mut c_void;
/// What a step gives back: its result, or what went wrong, in words.
type Res<T> = Result<T, String>;

const GENERIC_ALL: u32 = 0x1000_0000;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const TOKEN_DUPLICATE: u32 = 0x0002;
const TOKEN_QUERY: u32 = 0x0008;
const STARTF_FORCEOFFFEEDBACK: u32 = 0x0000_0080;
const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
const SW_SHOWMAXIMIZED: u16 = 3;
const WM_NULL: u32 = 0x0000;
const WM_CLOSE: u32 = 0x0010;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
const SMTO_ABORTIFHUNG: u32 = 0x0002;
const STILL_ACTIVE: u32 = 259;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_EXTENDED_LIMITS: i32 = 9;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const SPI_GETWORKAREA: u32 = 0x0030;
const GW_HWNDNEXT: u32 = 2;
const GWL_EXSTYLE: i32 = -20;
const WS_EX_LAYERED: isize = 0x0008_0000;
const LWA_ALPHA: u32 = 0x2;
const DWMWA_EXTENDED_FRAME_BOUNDS: u32 = 9;
const DWMWA_CLOAKED: u32 = 14;
const PW_RENDERFULLCONTENT: u32 = 0x2;
const SRCCOPY: u32 = 0x00CC_0020;
const DIB_RGB_COLORS: u32 = 0;
const DPI_PER_MONITOR_AWARE_V2: isize = -4;
const CLSCTX_INPROC_SERVER: u32 = 1;

// Windows' own structs, field for field as its headers lay them out.

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

/// BITMAPINFO: the header, then room for three colour masks.
#[repr(C)]
#[allow(dead_code)]
struct BitmapInfo {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pels_per_meter: i32,
    y_pels_per_meter: i32,
    clr_used: u32,
    clr_important: u32,
    colors: [u32; 3],
}

#[repr(C)]
#[allow(dead_code)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    cb_reserved2: u16,
    reserved2: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
}

#[repr(C)]
#[allow(dead_code)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

#[repr(C)]
#[allow(dead_code)]
struct JobBasicLimits {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[allow(dead_code)]
struct JobExtendedLimits {
    basic: JobBasicLimits,
    io_info: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

/// GUITHREADINFO: where a thread's windows have the keyboard focus.
#[repr(C)]
#[allow(dead_code)]
struct GuiThreadInfo {
    cb: u32,
    flags: u32,
    active: Handle,
    focus: Handle,
    capture: Handle,
    menu_owner: Handle,
    move_size: Handle,
    caret: Handle,
    caret_rect: Rect,
}

/// A GUID: the name of a COM class or interface.
#[repr(C)]
#[allow(dead_code)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

#[link(name = "user32")]
extern "system" {
    fn CreateDesktopW(name: *const u16, device: *const u16, devmode: *mut c_void, flags: u32,
                      access: u32, security: *mut c_void) -> Handle;
    fn OpenDesktopW(name: *const u16, flags: u32, inherit: i32, access: u32) -> Handle;
    fn CloseDesktop(desk: Handle) -> i32;
    fn SetThreadDesktop(desk: Handle) -> i32;
    fn GetTopWindow(hwnd: Handle) -> Handle;
    fn GetWindow(hwnd: Handle, which: u32) -> Handle;
    fn FindWindowExW(parent: Handle, after: Handle, class: *const u16, title: *const u16) -> Handle;
    fn IsWindowVisible(hwnd: Handle) -> i32;
    fn IsWindow(hwnd: Handle) -> i32;
    fn IsIconic(hwnd: Handle) -> i32;
    fn GetWindowRect(hwnd: Handle, rect: *mut Rect) -> i32;
    fn GetWindowLongPtrW(hwnd: Handle, index: i32) -> isize;
    fn GetLayeredWindowAttributes(hwnd: Handle, key: *mut u32, alpha: *mut u8, flags: *mut u32) -> i32;
    fn GetWindowThreadProcessId(hwnd: Handle, pid: *mut u32) -> u32;
    fn GetGUIThreadInfo(thread: u32, info: *mut GuiThreadInfo) -> i32;
    fn PostMessageW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    fn VkKeyScanW(character: u16) -> i16;
    fn AttachThreadInput(attach: u32, to: u32, on: i32) -> i32;
    fn GetKeyboardState(state: *mut u8) -> i32;
    fn SetKeyboardState(state: *mut u8) -> i32;
    fn GetClassNameW(hwnd: Handle, text: *mut u16, len: i32) -> i32;
    fn GetWindowTextW(hwnd: Handle, text: *mut u16, len: i32) -> i32;
    fn PrintWindow(hwnd: Handle, dc: Handle, flags: u32) -> i32;
    fn SendMessageTimeoutW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize, flags: u32, timeout_ms: u32,
                           result: *mut usize) -> isize;
    fn PaintDesktop(dc: Handle) -> i32;
    fn GetDC(hwnd: Handle) -> Handle;
    fn ReleaseDC(hwnd: Handle, dc: Handle) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, value: *mut c_void, change: u32) -> i32;
    fn SetThreadDpiAwarenessContext(context: isize) -> isize;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(dc: Handle) -> Handle;
    fn CreateCompatibleBitmap(dc: Handle, w: i32, h: i32) -> Handle;
    fn CreateDIBSection(dc: Handle, info: *const BitmapInfo, usage: u32, bits: *mut *mut c_void,
                        section: Handle, offset: u32) -> Handle;
    fn SelectObject(dc: Handle, object: Handle) -> Handle;
    fn DeleteObject(object: Handle) -> i32;
    fn DeleteDC(dc: Handle) -> i32;
    fn BitBlt(dc: Handle, x: i32, y: i32, w: i32, h: i32, from: Handle, from_x: i32, from_y: i32,
              rop: u32) -> i32;
    fn SetWindowOrgEx(dc: Handle, x: i32, y: i32, old: *mut Point) -> i32;
    fn GdiFlush() -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateProcessW(app: *const u16, command: *mut u16, process_security: *mut c_void,
                      thread_security: *mut c_void, inherit: i32, flags: u32, environment: *mut c_void,
                      folder: *const u16, startup: *mut StartupInfoW, info: *mut ProcessInformation) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateJobObjectW(security: *mut c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn QueryFullProcessImageNameW(process: Handle, flags: u32, name: *mut u16, len: *mut u32) -> i32;
    fn GetCurrentProcess() -> Handle;
    fn GetCurrentThreadId() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
}

#[link(name = "userenv")]
extern "system" {
    fn CreateEnvironmentBlock(block: *mut *mut c_void, token: Handle, inherit: i32) -> i32;
    fn DestroyEnvironmentBlock(block: *mut c_void) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: Handle, attribute: u32, value: *mut c_void, len: u32) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
    fn CoCreateInstance(class: *const Guid, outer: *mut c_void, context: u32, interface: *const Guid,
                        object: *mut *mut c_void) -> i32;
}

#[link(name = "oleaut32")]
extern "system" {
    fn SysFreeString(text: *mut u16);
    fn SysStringLen(text: *mut u16) -> u32;
}

// ---------------------------------------------------------------- small helpers

/// A Rust string as the UTF-16 text Windows takes, with its closing zero.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// `what`, then the text of the last Windows error.
fn last_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

/// Text in quotes, for the reports.
fn quote(text: &str) -> String {
    format!("'{text}'")
}

fn sleep(seconds: f64) {
    thread::sleep(Duration::from_secs_f64(seconds.max(0.0).min(86400.0)));
}

/// The last part of a path: the file's name.
fn base_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

// ---------------------------------------------------------------- the desktop

/// The second desktop, made on first use. A desktop is a Windows object
/// of its own: its own windows and focus, on the same session and user.
unsafe fn open_desktop(name: &str) -> Res<Handle> {
    let name = wide(name);
    let mut desk = OpenDesktopW(name.as_ptr(), 0, 0, GENERIC_ALL);
    if desk.is_null() {
        desk = CreateDesktopW(name.as_ptr(), null(), null_mut(), 0, GENERIC_ALL, null_mut());
    }
    if desk.is_null() {
        return Err(last_error("could not make the second desktop"));
    }
    Ok(desk)
}

/// A job that ends every process in it when the app ends, however it ends
/// (closed, Ctrl+C, a crash): nothing keeps running unseen.
unsafe fn kill_on_close_job() -> Res<Handle> {
    let job = CreateJobObjectW(null_mut(), null());
    let mut info: JobExtendedLimits = zeroed();
    info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if job.is_null()
        || SetInformationJobObject(job, JOB_EXTENDED_LIMITS, &mut info as *mut JobExtendedLimits as *mut c_void,
                                   size_of::<JobExtendedLimits>() as u32) == 0
    {
        return Err(last_error("could not make the job that ends the apps"));
    }
    Ok(job)
}

/// The environment Explorer gives a program, not the app's own. An app
/// started from an editor carries the editor's settings, and one of
/// Cursor's (ELECTRON_RUN_AS_NODE) turns any Electron app into plain Node.
/// Null when Windows gives none.
unsafe fn fresh_environment() -> *mut c_void {
    let (mut token, mut block): (Handle, *mut c_void) = (null_mut(), null_mut());
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, &mut token) == 0 {
        return null_mut();
    }
    let ok = CreateEnvironmentBlock(&mut block, token, 0);
    CloseHandle(token);
    if ok != 0 { block } else { null_mut() }
}

/// Jobs made for the programs that could not join the main one (Store apps).
/// Windows closes them when the app is gone, which ends what is in them.
static EXTRA_JOBS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Start `command` with its windows on `desktop`, and put it in `job` right
/// after, so it ends with the sandbox. It is started as any program starts
/// another: not paused first (a program started paused, put in a job and
/// then let go is what an antivirus took for a threat, see the top of this
/// file).
///
/// A Store app cannot join the job: Windows has put it in a job of its own. A
/// new job made just for it sits inside that one and ends the app the same
/// way, and only that app. Without one, Paint and ChatGPT stayed running on
/// the second desktop after the sandbox had gone (seen).
///
/// The job is not named in the start-up attributes, though that would put the
/// program in it from its first moment. Tried and measured with Paint: Windows
/// then made its own job for the Store app INSIDE the sandbox's, the user's
/// own Paint, opened later on their desktop, joined that job, and stopping the
/// sandbox closed it too. Started plainly, the order is the other way round:
/// our job for the app sits inside Windows', and the user's copy stays
/// (measured both ways round).
///
/// `show`, when given, is how its first window shows (SW_SHOWMAXIMIZED), for
/// the programs that follow it. Gives back its process id.
unsafe fn start_on_desktop(command: &str, desktop: &str, job: Handle, show: Option<u16>) -> Res<u32> {
    let mut on_desktop = wide(&format!("WinSta0\\{desktop}"));
    let mut si: StartupInfoW = zeroed();
    si.cb = size_of::<StartupInfoW>() as u32;
    si.desktop = on_desktop.as_mut_ptr();
    si.flags = STARTF_FORCEOFFFEEDBACK; // no busy cursor on your screen
    if let Some(show) = show {
        si.flags |= STARTF_USESHOWWINDOW;
        si.show_window = show;
    }
    let mut pi: ProcessInformation = zeroed();
    let mut line = wide(command);
    let env = fresh_environment();
    let flags = if env.is_null() { 0 } else { CREATE_UNICODE_ENVIRONMENT };
    let ok = CreateProcessW(null(), line.as_mut_ptr(), null_mut(), null_mut(), 0, flags, env, null(), &mut si, &mut pi);
    let error = std::io::Error::last_os_error();
    if !env.is_null() {
        DestroyEnvironmentBlock(env);
    }
    if ok == 0 {
        return Err(error.to_string());
    }
    if AssignProcessToJobObject(job, pi.process) == 0 {
        let own = kill_on_close_job().unwrap_or(null_mut());
        if !own.is_null() && AssignProcessToJobObject(own, pi.process) != 0 {
            EXTRA_JOBS.lock().unwrap().push(own as usize);
        } else {
            eprintln!("{command} could not be put in a job: it will not end with the sandbox");
            if !own.is_null() {
                CloseHandle(own);
            }
        }
    }
    CloseHandle(pi.thread);
    CloseHandle(pi.process);
    Ok(pi.process_id)
}

unsafe fn process_state(pid: u32) -> String {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if handle.is_null() {
        return "exited".to_string();
    }
    let mut code = 0u32;
    let ok = GetExitCodeProcess(handle, &mut code);
    CloseHandle(handle);
    if ok != 0 && code == STILL_ACTIVE { "still running".to_string() } else { format!("exited (code {code})") }
}

// ---------------------------------------------------------------- windows

const TASKBARS: [&str; 2] = ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
const TASKBAR_CONTENT: &str = "Windows.UI.Composition.DesktopWindowContentBridge";

/// A window: whose it is, its class and its title.
struct WindowInfo {
    pid: u32,
    class: String,
    title: String,
}

unsafe fn class_of(hwnd: Handle) -> String {
    let mut text = [0u16; 256];
    let len = GetClassNameW(hwnd, text.as_mut_ptr(), 256);
    String::from_utf16_lossy(&text[..len.clamp(0, 256) as usize])
}

unsafe fn title_of(hwnd: Handle) -> String {
    let mut text = [0u16; 256];
    let len = GetWindowTextW(hwnd, text.as_mut_ptr(), 256);
    String::from_utf16_lossy(&text[..len.clamp(0, 256) as usize])
}

/// On show: visible, not minimized, not hidden by DWM, not fully clear.
unsafe fn is_shown(hwnd: Handle) -> bool {
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return false;
    }
    // The Windows 11 taskbar draws with XAML, which only runs on the main
    // desktop: on the second desktop its window is there but empty (black).
    if TASKBARS.contains(&class_of(hwnd).as_str()) {
        let content = wide(TASKBAR_CONTENT);
        if FindWindowExW(hwnd, null_mut(), content.as_ptr(), null()).is_null() {
            return false;
        }
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, 4) == 0 && cloaked != 0 {
        return false;
    }
    if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED != 0 {
        let (mut alpha, mut flags) = (0u8, 0u32);
        if GetLayeredWindowAttributes(hwnd, null_mut(), &mut alpha, &mut flags) != 0
            && flags & LWA_ALPHA != 0
            && alpha == 0
        {
            return false;
        }
    }
    let mut r = Rect::default();
    GetWindowRect(hwnd, &mut r) != 0 && r.right > r.left && r.bottom > r.top
}

unsafe fn window_info(hwnd: Handle) -> WindowInfo {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    WindowInfo { pid, class: class_of(hwnd), title: title_of(hwnd) }
}

unsafe fn process_name(pid: u32) -> String {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if handle.is_null() {
        return format!("pid {pid}");
    }
    let mut name = [0u16; 1024];
    let mut len = name.len() as u32;
    let ok = QueryFullProcessImageNameW(handle, 0, name.as_mut_ptr(), &mut len);
    CloseHandle(handle);
    if ok == 0 {
        return format!("pid {pid}");
    }
    base_name(&String::from_utf16_lossy(&name[..(len as usize).min(name.len())]))
}

// ---------------------------------------------------------------- the picture

fn bitmap_info(width: i32, height: i32) -> BitmapInfo {
    BitmapInfo {
        size: 40,
        width,
        height: -height, // top-down rows
        planes: 1,
        bit_count: 32,
        compression: 0,
        size_image: 0,
        x_pels_per_meter: 0,
        y_pels_per_meter: 0,
        clr_used: 0,
        clr_important: 0,
        colors: [0; 3],
    }
}

/// Top-level windows of this thread's desktop on show, top of the Z order first.
unsafe fn visible_windows() -> Vec<usize> {
    let mut found = Vec::new();
    let mut hwnd = GetTopWindow(null_mut());
    for _ in 0..5000 {
        if hwnd.is_null() {
            break;
        }
        if is_shown(hwnd) {
            found.push(hwnd as usize);
        }
        hwnd = GetWindow(hwnd, GW_HWNDNEXT);
    }
    found
}

/// Whether a window's thread answers within 200 ms. One that does not (a
/// hung app) is left out of the picture and the scan: PrintWindow waits
/// for that thread to paint, for good, and UI Automation waits out its own
/// timeouts on it (a scan took 114 s with one hung window there, measured).
unsafe fn answers(hwnd: Handle) -> bool {
    let mut answer = 0usize;
    SendMessageTimeoutW(hwnd, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 200, &mut answer) != 0
}

/// Ask Windows to paint one window into `canvas` at its place on screen.
/// PrintWindow works for a window nobody can see, which is every window here.
unsafe fn paint_window(canvas: Handle, screen: Handle, hwnd: Handle) {
    if !answers(hwnd) {
        return;
    }
    let mut r = Rect::default();
    if GetWindowRect(hwnd, &mut r) == 0 {
        return;
    }
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if !(0 < w && w <= 16384 && 0 < h && h <= 16384) {
        return;
    }
    let mut f = Rect::default(); // the visible frame, without the invisible resize borders
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut f as *mut Rect as *mut c_void,
                             size_of::<Rect>() as u32) != 0
    {
        f = r;
    }
    let dc = CreateCompatibleDC(screen);
    let bmp = CreateCompatibleBitmap(screen, w, h);
    let old = SelectObject(dc, bmp);
    if PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT) != 0 || PrintWindow(hwnd, dc, 0) != 0 {
        BitBlt(canvas, f.left, f.top, f.right - f.left, f.bottom - f.top, dc, f.left - r.left, f.top - r.top,
               SRCCOPY);
    }
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
}

/// One picture of this thread's desktop, the part `frames` names: the
/// wallpaper, then every window on show from the bottom up. Gives back its
/// pixels, top row first, four bytes each (blue, green, red, unused).
unsafe fn capture(frames: &Frames) -> Res<Vec<u8>> {
    let screen = GetDC(null_mut());
    let canvas = CreateCompatibleDC(screen);
    let mut bits: *mut c_void = null_mut();
    let info = bitmap_info(frames.width, frames.height);
    let dib = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if dib.is_null() || bits.is_null() {
        DeleteDC(canvas);
        ReleaseDC(null_mut(), screen);
        return Err(format!("could not make a {}x{} picture", frames.width, frames.height));
    }
    let old = SelectObject(canvas, dib);
    SetWindowOrgEx(canvas, frames.x0, frames.y0, null_mut()); // draw in screen coordinates
    PaintDesktop(canvas);
    for &hwnd in visible_windows().iter().rev() {
        paint_window(canvas, screen, hwnd as Handle);
    }
    GdiFlush();
    let len = frames.width as usize * frames.height as usize * 4;
    let pixels = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
    SelectObject(canvas, old);
    DeleteObject(dib);
    DeleteDC(canvas);
    ReleaseDC(null_mut(), screen);
    Ok(pixels)
}

/// The part of the screen every picture shows: the primary monitor without
/// the taskbar strip, in real pixels.
struct Frames {
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
}

// ---------------------------------------------------------------- UI Automation, through COM by hand

const CLSID_CUIAUTOMATION: Guid =
    Guid { d1: 0xff48dba4, d2: 0x60ef, d3: 0x4201, d4: [0xaa, 0x87, 0x54, 0x10, 0x3e, 0xef, 0x59, 0x4e] };
const IID_IUIAUTOMATION: Guid =
    Guid { d1: 0x30cbe57d, d2: 0xd9d0, d3: 0x452a, d4: [0xab, 0x13, 0x7a, 0xc5, 0xac, 0x48, 0x25, 0xee] };

// The place of each method in its interface's table, counted from 0, as
// UIAutomationCore.dll's type library lists them. Places 0, 1 and 2 are
// every COM object's QueryInterface, AddRef and Release.
const RELEASE: usize = 2;
// IUIAutomation
const UIA_ELEMENT_FROM_HANDLE: usize = 6;
const UIA_CREATE_CACHE_REQUEST: usize = 20;
const UIA_CREATE_TRUE_CONDITION: usize = 21;
// IUIAutomationElement
const EL_FIND_ALL: usize = 6;
const EL_FIND_ALL_BUILD_CACHE: usize = 8;
const EL_CACHED_CONTROL_TYPE: usize = 53;
const EL_CACHED_NAME: usize = 55;
const EL_CACHED_IS_KEYBOARD_FOCUSABLE: usize = 59;
const EL_CACHED_IS_OFFSCREEN: usize = 70;
const EL_CACHED_BOUNDING_RECTANGLE: usize = 75;
// IUIAutomationElementArray
const ARRAY_LENGTH: usize = 3;
const ARRAY_GET_ELEMENT: usize = 4;
// IUIAutomationCacheRequest
const CACHE_ADD_PROPERTY: usize = 3;

const TREE_SCOPE_DESCENDANTS: i32 = 4;
const UIA_RECT: i32 = 30001;
const UIA_TYPE: i32 = 30003;
const UIA_NAME: i32 = 30005;
const UIA_FOCUSABLE: i32 = 30009;
const UIA_OFFSCREEN: i32 = 30022;
const CLICKABLE: [i32; 13] =
    [50000, 50002, 50003, 50004, 50005, 50007, 50011, 50013, 50015, 50019, 50024, 50029, 50031];
const WHOLE_AREAS: [i32; 3] = [50030, 50032, 50033]; // Document, Window, Pane: not things to click

fn control_type(id: i32) -> &'static str {
    match id {
        50000 => "Button",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50010 => "MenuBar",
        50011 => "MenuItem",
        50013 => "RadioButton",
        50014 => "ScrollBar",
        50015 => "Slider",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50023 => "Tree",
        50024 => "TreeItem",
        50025 => "Custom",
        50026 => "Group",
        50027 => "Thumb",
        50028 => "DataGrid",
        50029 => "DataItem",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        50037 => "TitleBar",
        _ => "Other",
    }
}

/// A COM answer: an error when Windows says the call failed.
fn check(result: i32) -> Res<()> {
    if result < 0 { Err(format!("UI Automation error 0x{:08X}", result as u32)) } else { Ok(()) }
}

/// One COM object, given back to Windows when dropped. A COM object is a
/// pointer to a pointer to its table of methods, and every method takes the
/// object itself first.
struct Com(*mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = self.method(RELEASE);
            release(self.0);
        }
    }
}

impl Com {
    /// The method at `place` in the object's table, as the function type the caller names.
    unsafe fn method<F: Copy>(&self, place: usize) -> F {
        let table = *(self.0 as *const *const *const c_void);
        std::mem::transmute_copy(&*table.add(place))
    }

    /// A method that takes nothing and gives one value back.
    unsafe fn get<T>(&self, place: usize) -> Res<T> {
        let call: unsafe extern "system" fn(*mut c_void, *mut T) -> i32 = self.method(place);
        let mut value: T = zeroed();
        check(call(self.0, &mut value))?;
        Ok(value)
    }

    /// A method that gives another object back.
    unsafe fn object(&self, place: usize) -> Res<Com> {
        let found: *mut c_void = self.get(place)?;
        if found.is_null() { Err("UI Automation gave no object back".to_string()) } else { Ok(Com(found)) }
    }

    /// A method that gives text back.
    unsafe fn text(&self, place: usize) -> Res<String> {
        let text: *mut u16 = self.get(place)?;
        if text.is_null() {
            return Ok(String::new());
        }
        let found = String::from_utf16_lossy(std::slice::from_raw_parts(text, SysStringLen(text) as usize));
        SysFreeString(text);
        Ok(found)
    }

    /// A method that takes nothing and gives nothing back.
    #[allow(dead_code)] // for the hands below
    unsafe fn call(&self, place: usize) -> Res<()> {
        let call: unsafe extern "system" fn(*mut c_void) -> i32 = self.method(place);
        check(call(self.0))
    }
}

/// UI Automation for the calling thread, which is already on the second desktop.
unsafe fn uia_client() -> Res<Com> {
    CoInitializeEx(null_mut(), 0); // COM for this thread only, multithreaded
    let mut uia = null_mut();
    check(CoCreateInstance(&CLSID_CUIAUTOMATION, null_mut(), CLSCTX_INPROC_SERVER, &IID_IUIAUTOMATION, &mut uia))?;
    if uia.is_null() {
        return Err("UI Automation did not start".to_string());
    }
    Ok(Com(uia))
}

/// The element that is a window.
unsafe fn element_from_handle(uia: &Com, hwnd: Handle) -> Res<Com> {
    let call: unsafe extern "system" fn(*mut c_void, Handle, *mut *mut c_void) -> i32 =
        uia.method(UIA_ELEMENT_FROM_HANDLE);
    let mut element = null_mut();
    check(call(uia.0, hwnd, &mut element))?;
    if element.is_null() { Err("no element for that window".to_string()) } else { Ok(Com(element)) }
}

/// Every element under `element` that meets `condition`. With `cache`, each
/// comes back with the properties the cache names already read.
unsafe fn find_all(element: &Com, scope: i32, condition: &Com, cache: Option<&Com>) -> Res<Vec<Com>> {
    let mut array: *mut c_void = null_mut();
    match cache {
        None => {
            let call: unsafe extern "system" fn(*mut c_void, i32, *mut c_void, *mut *mut c_void) -> i32 =
                element.method(EL_FIND_ALL);
            check(call(element.0, scope, condition.0, &mut array))?;
        }
        Some(cache) => {
            let call: unsafe extern "system" fn(*mut c_void, i32, *mut c_void, *mut c_void,
                                                *mut *mut c_void) -> i32 = element.method(EL_FIND_ALL_BUILD_CACHE);
            check(call(element.0, scope, condition.0, cache.0, &mut array))?;
        }
    }
    if array.is_null() {
        return Ok(Vec::new());
    }
    let array = Com(array);
    let count: i32 = array.get(ARRAY_LENGTH)?;
    let get: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32 = array.method(ARRAY_GET_ELEMENT);
    let mut found = Vec::new();
    for i in 0..count {
        let mut one = null_mut();
        check(get(array.0, i, &mut one))?;
        if !one.is_null() {
            found.push(Com(one));
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------- the scan

/// One thing to click or type in, found by the scan of the whole desktop.
struct Found {
    kind: &'static str,
    name: String,
    place: [i32; 4],
    /// The top-level window it is in.
    window: usize,
}

/// The properties a scan reads, fetched with the elements in one call per
/// window.
unsafe fn scan_cache(uia: &Com) -> Res<Com> {
    let cache = uia.object(UIA_CREATE_CACHE_REQUEST)?;
    let add: unsafe extern "system" fn(*mut c_void, i32) -> i32 = cache.method(CACHE_ADD_PROPERTY);
    for property in [UIA_RECT, UIA_TYPE, UIA_NAME, UIA_FOCUSABLE, UIA_OFFSCREEN] {
        check(add(cache.0, property))?;
    }
    Ok(cache)
}

/// What UI Automation finds to click or type in, in `windows` (top first),
/// read once. What a window covers is dropped from the ones below it, and
/// one element is kept per place. Called from a thread on the second desktop.
unsafe fn find_elements(uia: &Com, every: &Com, cache: &Com, windows: &[usize]) -> Res<Vec<Found>> {
    let (mut items, mut places, mut above) = (Vec::new(), HashSet::new(), Vec::<Rect>::new());
    for &window in windows {
        let hwnd = window as Handle;
        if !answers(hwnd) {
            continue;
        }
        let mut w = Rect::default();
        GetWindowRect(hwnd, &mut w);
        let found = element_from_handle(uia, hwnd)
            .and_then(|element| find_all(&element, TREE_SCOPE_DESCENDANTS, every, Some(cache)));
        let found = match found {
            Ok(found) => found,
            Err(e) => {
                eprintln!("  could not read {}: {e}", quote(&class_of(hwnd)));
                Vec::new()
            }
        };
        for e in &found {
            let kind: i32 = e.get(EL_CACHED_CONTROL_TYPE)?;
            if e.get::<i32>(EL_CACHED_IS_OFFSCREEN)? != 0 || WHOLE_AREAS.contains(&kind) {
                continue;
            }
            if !CLICKABLE.contains(&kind) && e.get::<i32>(EL_CACHED_IS_KEYBOARD_FOCUSABLE)? == 0 {
                continue;
            }
            let r: Rect = e.get(EL_CACHED_BOUNDING_RECTANGLE)?;
            let place = [r.left.max(w.left), r.top.max(w.top), r.right.min(w.right), r.bottom.min(w.bottom)];
            let (cx, cy) = ((place[0] + place[2]).div_euclid(2), (place[1] + place[3]).div_euclid(2));
            if place[2] - place[0] < 4
                || place[3] - place[1] < 4
                || above.iter().any(|a| a.left <= cx && cx < a.right && a.top <= cy && cy < a.bottom)
            {
                continue;
            }
            if places.insert(place) {
                // one per place
                items.push(Found { kind: control_type(kind), name: e.text(EL_CACHED_NAME)?, place, window });
            }
        }
        above.push(w);
    }
    Ok(items)
}

/// A string as JSON text, quotes included.
fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A window's frame as you see it, without the invisible resize borders.
unsafe fn frame_of(hwnd: Handle) -> Rect {
    let mut f = Rect::default();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut f as *mut Rect as *mut c_void,
                             size_of::<Rect>() as u32) != 0
    {
        GetWindowRect(hwnd, &mut f);
    }
    f
}

/// The agent's scan of the second desktop, as JSON: the windows on show,
/// top first, and what UI Automation finds to click or type in, numbered
/// from 1. Boxes are in screen pixels. `seen` holds the windows of the scan
/// before: a window new since then is read again until two reads agree,
/// since Chromium (so WebView2 and Electron) builds its element tree only
/// when someone first asks.
unsafe fn device_scan(uia: &Com, every: &Com, cache: &Com, seen: &mut HashSet<usize>) -> Res<String> {
    let windows = visible_windows();
    let mut items = find_elements(uia, every, cache, &windows)?;
    if windows.iter().any(|w| !seen.contains(w)) {
        for _ in 0..3 {
            sleep(0.5);
            let again = find_elements(uia, every, cache, &windows)?;
            let same = again.len() == items.len();
            items = again;
            if same {
                break;
            }
        }
    }
    *seen = windows.iter().copied().collect();
    let mut json = String::from("{\"windows\":[");
    for (i, &hwnd) in windows.iter().enumerate() {
        let info = window_info(hwnd as Handle);
        let f = frame_of(hwnd as Handle);
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"hwnd\":{hwnd},\"pid\":{},\"exe\":{},\"class\":{},\"title\":{},\"box\":[{},{},{},{}]}}",
            info.pid, json_text(&process_name(info.pid)), json_text(&info.class), json_text(&info.title),
            f.left, f.top, f.right, f.bottom
        ));
    }
    json.push_str("],\"elements\":[");
    for (n, item) in items.iter().enumerate() {
        let [left, top, right, bottom] = item.place;
        if n > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"n\":{},\"kind\":{},\"name\":{},\"box\":[{left},{top},{right},{bottom}],\"window\":{}}}",
            n + 1, json_text(item.kind), json_text(&item.name), item.window
        ));
    }
    json.push_str("]}");
    Ok(json)
}

/// An app's own window: titled and at least 100 by 100, as the agent's
/// open_app counts them, and not the desktop or a taskbar. While the second
/// desktop is new, Explorer's own windows come up there too, the desktop
/// ("Program Manager") and small untitled ones (seen): those are not.
unsafe fn is_app_window(hwnd: Handle) -> bool {
    let f = frame_of(hwnd);
    let shell = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
    !title_of(hwnd).trim().is_empty() && f.right - f.left >= 100 && f.bottom - f.top >= 100
        && !shell.contains(&class_of(hwnd).as_str())
}

// ---------------------------------------------------------------- the keyboard: typewrite and hotkey

/// Where typed text and pressed keys go: the app window in front on the
/// second desktop, and inside it the window that has the keyboard focus (the
/// text area with the cursor). An app that names no focus gets them at its
/// own window. Gives back (the app's window, the window to send to).
unsafe fn key_target() -> Res<(Handle, Handle)> {
    let top = visible_windows()
        .into_iter()
        .map(|hwnd| hwnd as Handle)
        .find(|&hwnd| is_app_window(hwnd))
        .ok_or("no app is open in AutoCua SB to type in: open one first")?;
    // An app that is busy for a moment is waited for: one just opened by the call before is often still loading.
    let end = Instant::now() + PATIENCE;
    while !answers(top) {
        if IsWindow(top) == 0 || Instant::now() >= end {
            return Err(format!("{} does not answer", quote(&title_of(top))));
        }
        thread::sleep(Duration::from_millis(50));
    }
    let mut info: GuiThreadInfo = zeroed();
    info.cb = size_of::<GuiThreadInfo>() as u32;
    let thread = GetWindowThreadProcessId(top, null_mut());
    let focus = if GetGUIThreadInfo(thread, &mut info) != 0 && !info.focus.is_null() { info.focus } else { top };
    Ok((top, focus))
}

/// How long a busy app is waited for at one typed character or key before the
/// tool gives up on it.
const PATIENCE: Duration = Duration::from_secs(3);

/// One message SENT to `hwnd`: the call is back when the app has taken the
/// message to its end, so the next one cannot overtake it. False when the app
/// does not take it within PATIENCE (it hangs), or the window has gone.
///
/// Typed text goes this way, one character at a time, at the app's own speed
/// (Notepad: about 450 characters a second, measured). It was first posted,
/// all at once: the tool then returned while the app still read the text,
/// and 300 characters with a "ctrl+s" in the same turn left the text
/// scrambled and unsaved on Notepad (the shortcut's held Ctrl was set while
/// letters of the text were still being read, and its "s" came after Ctrl
/// had gone). Asking the app's thread for an answer after each posted
/// character did not help either: it answers long before it has read them.
unsafe fn send_in_turn(hwnd: Handle, message: u32, key: u32, word: isize) -> bool {
    let mut answer = 0usize;
    SendMessageTimeoutW(hwnd, message, key as usize, word, SMTO_ABORTIFHUNG, PATIENCE.as_millis() as u32, &mut answer) != 0
}

/// Time for the app to read what was POSTED to `hwnd`: its thread comes round
/// to its messages `turns` times, `gap` apart. A busy app is waited for, until
/// PATIENCE is over. False when it had not had its turns by then: what was
/// posted may still be unread. A window that has gone has read it (a key
/// that closes its own window).
unsafe fn read_out(hwnd: Handle, gap: Duration, turns: usize) -> bool {
    let end = Instant::now() + PATIENCE;
    let mut had = 0;
    while had < turns {
        if IsWindow(hwnd) == 0 {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(gap);
        if answers(hwnd) {
            had += 1;
        }
    }
    true
}

const KEY_TAB: u32 = 0x09;
const KEY_ENTER: u32 = 0x0D;
const KEY_ESC: u32 = 0x1B;
const KEY_DELETE: u32 = 0x2E;
const KEY_F4: u32 = 0x73;

/// typewrite: `text` typed into the app in front, where its cursor is.
///
/// A letter is one character message, the message a keyboard's typing ends
/// up as, SENT: the app has taken it before the next goes (send_in_turn).
///
/// A line end or a Tab is the key's own press, POSTED as the hotkey tool
/// posts a key, so the app's own loop makes of it what it makes of a real
/// press: a new line or a tab in a text area, the next field or the OK button
/// in a form. Then the app gets a moment to read it (read_out), and the rest
/// goes where the cursor is by then. Measured on a classic edit box and a
/// classic form, on Notepad besides: sent like the letters, the form did not
/// move to its next field and did not press OK; posted with no wait, the line
/// end landed after the letters that follow it; the character alone is no
/// line end for Notepad (three lines came out as one).
///
/// As JSON: the app's title and how many characters went.
unsafe fn device_type(text: &str) -> Res<String> {
    let (mut top, mut focus) = key_target()?;
    // A control character is no text: a keyboard makes one only from Ctrl and a letter, and an edit box reads it as
    // that shortcut (0x16 is Ctrl+V: it would paste the user's clipboard there). The line end and Tab stay.
    let units: Vec<u16> = text
        .replace("\r\n", "\n")
        .replace('\n', "\r")
        .encode_utf16()
        .filter(|&unit| (unit >= 0x20 && unit != 0x7F) || unit as u32 == KEY_ENTER || unit as u32 == KEY_TAB)
        .collect();
    let total = units.len();
    for (at, &unit) in units.iter().enumerate() {
        let key = unit as u32;
        let a_key = key == KEY_ENTER || key == KEY_TAB;
        let taken = if a_key {
            let pressed = PostMessageW(focus, WM_KEYDOWN, key as usize, key_word(key, false, false, false)) != 0;
            PostMessageW(focus, WM_KEYUP, key as usize, key_word(key, false, false, true));
            // Not read in time: the letters after it would be read before it, so the typing stops here.
            pressed && read_out(focus, Duration::from_millis(10), 3)
        } else {
            send_in_turn(focus, WM_CHAR, key, 1)
        };
        let typed = at + usize::from(taken);
        if taken && typed == total {
            break;
        }
        if taken && a_key {
            // The press may have moved the cursor, or closed the window it was in.
            (top, focus) = key_target().map_err(|why| format!("{why}: {typed} of {total} characters were typed"))?;
        } else if !taken || IsWindow(focus) == 0 {
            let why = if IsWindow(focus) == 0 { "the place it was typed in has closed" } else { "the app stopped taking the text" };
            return Err(format!("{why}: {typed} of {total} characters were typed"));
        }
    }
    Ok(format!("{{\"window\":{},\"characters\":{total}}}", json_text(&title_of(top))))
}

/// A key combination as the hotkey tool writes it ("enter", "ctrl+s",
/// "ctrl+shift+s"): what is held, and the one key pressed.
struct Keys {
    ctrl: bool,
    shift: bool,
    alt: bool,
    /// The key's Windows code, and whether it is one of the keys a key
    /// message marks as extended (the arrows, Home, End, Delete and so on).
    key: u32,
    extended: bool,
}

/// A key's Windows code by its name, or by the one character it types on
/// this keyboard. The names are the ones the Windows tool's keyboard library
/// takes in Full control, written without spaces or underscores (read_keys
/// takes them out): "page up", "page_up" and "pageup" are one key.
unsafe fn key_code(name: &str) -> Option<(u32, bool)> {
    Some(match name {
        "enter" | "return" => (KEY_ENTER, false),
        "tab" => (KEY_TAB, false),
        "esc" | "escape" => (KEY_ESC, false),
        "backspace" => (0x08, false),
        "space" | "spacebar" => (0x20, false),
        "delete" | "del" => (KEY_DELETE, true),
        "insert" | "ins" => (0x2D, true),
        "home" => (0x24, true),
        "end" => (0x23, true),
        "pageup" | "pgup" => (0x21, true),
        "pagedown" | "pgdn" | "pgdown" => (0x22, true),
        "left" | "leftarrow" | "arrowleft" => (0x25, true),
        "up" | "uparrow" | "arrowup" => (0x26, true),
        "right" | "rightarrow" | "arrowright" => (0x27, true),
        "down" | "downarrow" | "arrowdown" => (0x28, true),
        "menu" | "apps" => (0x5D, true), // the key that opens the menu a right click opens
        "plus" => (0xBB, false),
        "minus" => (0xBD, false),
        _ => {
            if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
                return (1..=24).contains(&number).then_some((0x6F + number, false));
            }
            // A sign by its word, then any key by the one character it types.
            let name = match name {
                "comma" => ",",
                "period" | "dot" => ".",
                "slash" => "/",
                "backslash" => "\\",
                "semicolon" => ";",
                "equal" | "equals" => "=",
                other => other,
            };
            let mut letters = name.encode_utf16();
            let (Some(letter), None) = (letters.next(), letters.next()) else { return None };
            // A letter or a digit key has a code of its own, the same on every keyboard: asked of the
            // keyboard's layout, "s" is no key at all on one with no Latin letters (Russian, Greek, Arabic).
            if let Some(sign) = char::from_u32(letter as u32).filter(char::is_ascii_alphanumeric) {
                return Some((sign.to_ascii_uppercase() as u32, false));
            }
            let found = VkKeyScanW(letter);
            if found == -1 {
                return None;
            }
            ((found as u16 & 0xFF) as u32, false)
        }
    })
}

/// What a shortcut that belongs to the user's own desktop is answered.
const USERS_OWN: &str = "acts on the user's own desktop: it cannot be pressed in AutoCua SB";

/// "ctrl+shift+s" read into what is held and the key pressed: at most three
/// keys, as the tool says, and exactly one that is not held. Letter case,
/// spaces and underscores do not count ("Ctrl + S", "page_up"). Refused, in
/// words:
///   - a Windows-key shortcut, and the ones Windows itself reads from the
///     user's keyboard for the user's own desktop (Alt+Tab, Alt+Esc,
///     Ctrl+Esc, Ctrl+Shift+Esc, Ctrl+Alt+Delete). Sent to an app as messages
///     they would do nothing, and the tool would call that a success;
///   - copy, cut and paste (Ctrl+C, Ctrl+X, Ctrl+V with or without more held,
///     Ctrl+Insert, Shift+Insert, Shift+Delete). Windows has one clipboard
///     for the whole session, the user's: a copy here would replace what
///     they copied, and a paste would put their own last copy (a password,
///     say) into the agent's app, where the next picture shows it to the
///     model.
unsafe fn read_keys(keys: &str) -> Res<Keys> {
    let wanted = keys.to_lowercase().replace(' ', "");
    let names: Vec<String> =
        wanted.split('+').map(|name| if name.chars().count() > 1 { name.replace('_', "") } else { name.to_string() }).collect();
    if wanted.is_empty() || names.iter().any(|name| name.is_empty()) {
        return Err(format!("{} is not a key or a combination", quote(keys)));
    }
    if names.len() > 3 {
        return Err(format!("Maximum 3 keys allowed, got {}", names.len()));
    }
    let mut read = Keys { ctrl: false, shift: false, alt: false, key: 0, extended: false };
    for name in &names {
        match name.as_str() {
            "ctrl" | "control" | "leftctrl" | "rightctrl" => read.ctrl = true,
            "shift" | "leftshift" | "rightshift" => read.shift = true,
            "alt" | "option" | "leftalt" | "rightalt" => read.alt = true,
            "win" | "windows" | "leftwindows" | "rightwindows" | "cmd" | "command" | "super" | "meta" => {
                return Err(format!("a Windows-key shortcut {USERS_OWN}"));
            }
            _ if read.key != 0 => return Err(format!("{} names two keys to press: one, with what is held", quote(keys))),
            name => {
                (read.key, read.extended) = key_code(name).ok_or(format!("{} is not a key this tool knows", quote(name)))?;
            }
        }
    }
    if read.key == 0 {
        return Err(format!("{} holds keys but presses none", quote(keys)));
    }
    if (read.alt && (read.key == KEY_TAB || read.key == KEY_ESC))
        || (read.ctrl && read.key == KEY_ESC)
        || (read.ctrl && read.alt && read.key == KEY_DELETE)
    {
        return Err(format!("{} is a shortcut Windows itself handles: it {USERS_OWN}", quote(&wanted)));
    }
    const KEY_INSERT: u32 = 0x2D;
    let letter = |key: char| read.key == key as u32; // a letter key's code is its capital letter
    if (read.ctrl && (letter('C') || letter('X') || letter('V')))
        || ((read.ctrl || read.shift) && read.key == KEY_INSERT)
        || (read.shift && !read.ctrl && !read.alt && read.key == KEY_DELETE)
    {
        return Err(format!(
            "{} would use the clipboard, and the clipboard is the user's own (AutoCua SB shares it with their desktop): it \
             cannot be pressed in AutoCua SB. To put a text somewhere, type it with typewrite.",
            quote(&wanted)
        ));
    }
    Ok(read)
}

/// The last word of a key message: one press, the key's place on the
/// keyboard, and the marks for an extended key, for Alt being held, and for
/// the key going up.
unsafe fn key_word(key: u32, extended: bool, alt: bool, up: bool) -> isize {
    let mut word = 1u32 | (MapVirtualKeyW(key, 0) & 0xFF) << 16;
    if extended {
        word |= 1 << 24;
    }
    if alt {
        word |= 1 << 29;
    }
    if up {
        word |= 3 << 30;
    }
    word as usize as isize
}

/// hotkey: one key, or one key with Ctrl, Shift or Alt held, pressed in the
/// app in front: the key messages a keyboard's press ends up as, posted to
/// the window that has the focus there, so the app's own loop makes of them
/// what it makes of a real press (its shortcuts, its menus, the next field
/// of a form).
///
/// An app asks Windows whether Ctrl, Shift or Alt is down when it reads a key
/// (that is how Ctrl+S is told from S). For the time of the press, this
/// thread shares its keyboard state with the app's (AttachThreadInput), says
/// there that the held keys are down (SetKeyboardState), and puts both back
/// once the app has had its turns at the press (read_out). Nothing else
/// waits in the app's line then: typed text is sent, not posted. That state
/// is the second desktop's alone: your own keyboard, and the apps on your
/// desktop, never see it.
///
/// Alt+F4 is done by its meaning, the window is asked to close. As JSON: the
/// app's title and the keys as read.
unsafe fn device_key(keys: &str) -> Res<String> {
    let read = read_keys(keys)?;
    let (top, focus) = key_target()?;
    let title = title_of(top);
    if read.alt && !read.ctrl && !read.shift && read.key == KEY_F4 {
        PostMessageW(top, WM_CLOSE, 0, 0);
        thread::sleep(Duration::from_millis(200));
        return Ok(format!("{{\"window\":{},\"keys\":\"alt+f4\"}}", json_text(&title)));
    }
    const CTRL: u32 = 0x11;
    const SHIFT: u32 = 0x10;
    const ALT: u32 = 0x12;
    let held: Vec<u32> =
        [(read.ctrl, CTRL), (read.shift, SHIFT), (read.alt, ALT)].iter().filter(|(on, _)| *on).map(|&(_, key)| key).collect();
    // With Alt held and Ctrl not, Windows names the messages "system" keys.
    let system = read.alt && !read.ctrl;
    let (down, up) = if system { (WM_SYSKEYDOWN, WM_SYSKEYUP) } else { (WM_KEYDOWN, WM_KEYUP) };

    let (me, theirs) = (GetCurrentThreadId(), GetWindowThreadProcessId(focus, null_mut()));
    let mut before = [0u8; 256];
    if !held.is_empty() {
        // Without the held keys set there, the key alone would be pressed: "ctrl+s" would type an s.
        if theirs == 0 || AttachThreadInput(me, theirs, 1) == 0 {
            return Err(format!("{} could not be pressed in {}: its held keys could not be set there", quote(keys), quote(&title)));
        }
        GetKeyboardState(before.as_mut_ptr());
        let mut now = before;
        for &key in &held {
            now[key as usize] |= 0x80;
            now[(key - 0x10) as usize * 2 + 0xA0] |= 0x80; // its left-hand key: 0xA0 Shift, 0xA2 Ctrl, 0xA4 Alt
        }
        SetKeyboardState(now.as_mut_ptr());
    }
    for &key in &held {
        PostMessageW(focus, if key == ALT { WM_SYSKEYDOWN } else { down }, key as usize, key_word(key, false, key == ALT, false));
    }
    PostMessageW(focus, down, read.key as usize, key_word(read.key, read.extended, system, false));
    PostMessageW(focus, up, read.key as usize, key_word(read.key, read.extended, system, true));
    for &key in held.iter().rev() {
        // A held key goes up as a plain key: Alt let go as a "system" key would open the app's menu.
        PostMessageW(focus, WM_KEYUP, key as usize, key_word(key, false, false, true));
    }
    // The app reads the press while the held keys still read as down: a turn for each message posted, and one more.
    let read_in_time = read_out(focus, Duration::from_millis(20), held.len() * 2 + 3);
    if !held.is_empty() {
        SetKeyboardState(before.as_mut_ptr());
        AttachThreadInput(me, theirs, 0);
    }
    if !read_in_time {
        let alone = if held.is_empty() { "" } else { ", and then as the key alone, without what was held" };
        return Err(format!("{} was busy and has not read {} yet: it may arrive late{alone}. Look before going on.", quote(&title), quote(keys)));
    }
    thread::sleep(Duration::from_millis(100)); // what the key set off has begun when the tool answers
    Ok(format!("{{\"window\":{},\"keys\":{}}}", json_text(&title), json_text(&keys.to_lowercase().replace(' ', ""))))
}

// ---------------------------------------------------------------- the device: the app's agent works here

/// Start a program on the second desktop for the agent, its window
/// maximized as the agent's open_app does, and wait up to 10 seconds for a
/// new app window there. As JSON: the pid, the new window's title (null when
/// none came), and whether the program ended with no window: a one-copy app
/// can hand its start to the copy on your desktop and quit.
unsafe fn device_open(command: &str, job: Handle) -> Res<String> {
    if command.is_empty() {
        return Err("no command line to start".to_string());
    }
    let before: HashSet<usize> = visible_windows().into_iter().collect();
    let pid = start_on_desktop(command, DESKTOP, job, Some(SW_SHOWMAXIMIZED))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut window = None;
    while Instant::now() < deadline {
        let new = visible_windows().into_iter().find(|&hwnd| !before.contains(&hwnd) && is_app_window(hwnd as Handle));
        if let Some(hwnd) = new {
            thread::sleep(Duration::from_millis(300)); // its first paint
            window = Some(title_of(hwnd as Handle));
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let exited = window.is_none() && process_state(pid) != "still running";
    let window = window.map(|title| json_text(&title)).unwrap_or_else(|| "null".to_string());
    Ok(format!("{{\"pid\":{pid},\"window\":{window},\"exited\":{exited}}}"))
}

/// What Python asks of the second desktop, and what comes back.
enum Ask {
    Shot,
    Scan,
    Launch(String),
    Type(String),
    Key(String),
}

enum Answer {
    Picture(Vec<u8>),
    Json(String),
}

type Reply = Res<Answer>;

/// The second desktop while it is up: the line to its thread, the picture's
/// place on the screen ([x0, y0, width, height]), and what to close at the end.
struct Device {
    asks: Sender<(Ask, Sender<Reply>)>,
    /// Set at stop: a request still waiting in the line is answered "stopped", not begun.
    stopping: Arc<AtomicBool>,
    screen: [i32; 4],
    desk: usize,
    job: usize,
    worker: thread::JoinHandle<()>,
}

/// The app's one second desktop: none until start(), none again after stop().
static DEVICE: Mutex<Option<Device>> = Mutex::new(None);

/// How long one request may take before Python is told it did not answer. A
/// program is given 10 seconds to show a window, and a scan reads each new
/// window a few times: both end well inside this.
const ASK_TIMEOUT: Duration = Duration::from_secs(120);

const STOPPED: &str = "AutoCua SB's desktop has stopped";

/// The second desktop's own thread. It reads the picture's place while it is
/// still on your desktop (the screen without the taskbar strip, in real
/// pixels: the setting is this thread's alone, the app's other threads keep
/// theirs), joins the second desktop before it owns any window, and then
/// answers one request at a time until the line to it closes. `ready` is told
/// once, when it can answer, or why it cannot.
unsafe fn device_loop(desk: usize, job: usize, asked: Receiver<(Ask, Sender<Reply>)>, ready: Sender<Res<[i32; 4]>>,
                      stopping: Arc<AtomicBool>) {
    SetThreadDpiAwarenessContext(DPI_PER_MONITOR_AWARE_V2);
    let mut work = Rect::default();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut Rect as *mut c_void, 0);
    let frames = Frames { x0: work.left, y0: work.top, width: work.right - work.left, height: work.bottom - work.top };
    if frames.width <= 0 || frames.height <= 0 {
        let _ = ready.send(Err("could not read the size of the screen".to_string()));
        return;
    }
    if SetThreadDesktop(desk as Handle) == 0 {
        let _ = ready.send(Err(last_error("could not join the second desktop")));
        return;
    }
    let tools = (|| -> Res<(Com, Com, Com)> {
        let uia = uia_client()?;
        let every = uia.object(UIA_CREATE_TRUE_CONDITION)?;
        let cache = scan_cache(&uia)?;
        Ok((uia, every, cache))
    })();
    let (uia, every, cache) = match tools {
        Ok(tools) => tools,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if ready.send(Ok([frames.x0, frames.y0, frames.width, frames.height])).is_err() {
        return;
    }
    let mut seen = HashSet::new();
    for (ask, tell) in asked {
        if stopping.load(Ordering::SeqCst) {
            let _ = tell.send(Err(STOPPED.to_string()));
            continue;
        }
        let reply = match ask {
            Ask::Shot => capture(&frames).map(Answer::Picture),
            Ask::Scan => device_scan(&uia, &every, &cache, &mut seen).map(Answer::Json),
            Ask::Launch(command) => device_open(command.trim(), job as Handle).map(Answer::Json),
            Ask::Type(text) => device_type(&text).map(Answer::Json),
            Ask::Key(keys) => device_key(&keys).map(Answer::Json),
        };
        let _ = tell.send(reply); // nobody waits any more: the next one may
    }
}

/// End every app on a second desktop and let it go. Its thread ends with the
/// request it is on; the job and the desktop are closed once it has left. If
/// it has not after two seconds (a program it started is still coming up),
/// the apps are ended all the same and the two handles stay open for that
/// thread: Windows closes them with the app.
unsafe fn close_device(device: Device) {
    device.stopping.store(true, Ordering::SeqCst);
    drop(device.asks);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !device.worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    for extra in EXTRA_JOBS.lock().unwrap().drain(..) {
        TerminateJobObject(extra as Handle, 0);
        CloseHandle(extra as Handle);
    }
    TerminateJobObject(device.job as Handle, 0);
    if device.worker.is_finished() {
        let _ = device.worker.join();
        CloseHandle(device.job as Handle);
        CloseDesktop(device.desk as Handle);
    }
}

/// The second desktop, made on first use: the desktop, the job that ends its
/// apps, its thread, and Explorer on it (the wallpaper and the desktop's own
/// windows). Gives back the picture's place on the screen.
unsafe fn start_device() -> Res<[i32; 4]> {
    let mut device = DEVICE.lock().map_err(|_| STOPPED.to_string())?;
    if let Some(up) = device.as_ref() {
        if !up.worker.is_finished() {
            return Ok(up.screen);
        }
    }
    if let Some(gone) = device.take() {
        close_device(gone); // its thread ended: clear what it left before a new one
    }
    let desk = open_desktop(DESKTOP)?;
    let job = match kill_on_close_job() {
        Ok(job) => job,
        Err(e) => {
            CloseDesktop(desk);
            return Err(e);
        }
    };
    let (asks, asked) = channel();
    let (ready, is_ready) = channel();
    let (on_desk, in_job) = (desk as usize, job as usize); // as plain numbers they can go to another thread
    let stopping = Arc::new(AtomicBool::new(false));
    let told = stopping.clone();
    let worker = thread::spawn(move || unsafe { device_loop(on_desk, in_job, asked, ready, told) });
    let screen = match is_ready.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(screen)) => Ok(screen),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("the second desktop did not start".to_string()),
    };
    let up = Device { asks, stopping, screen: [0; 4], desk: on_desk, job: in_job, worker };
    let screen = match screen {
        Ok(screen) => screen,
        Err(e) => {
            close_device(up);
            return Err(e);
        }
    };
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    if let Err(e) = start_on_desktop(&format!("{windir}\\explorer.exe"), DESKTOP, job, None) {
        eprintln!("Explorer did not start on the second desktop: {e}");
    }
    *device = Some(Device { screen, ..up });
    Ok(screen)
}

// ---------------------------------------------------------------- what Python calls

fn failed(why: String) -> PyErr {
    PyRuntimeError::new_err(why)
}

/// One request to the second desktop's thread, and its answer. The wait is
/// outside the interpreter's lock, so the app's other threads go on.
fn ask(py: Python<'_>, what: Ask) -> PyResult<Answer> {
    let asks = DEVICE.lock().map_err(|_| failed(STOPPED.to_string()))?.as_ref().map(|up| up.asks.clone());
    let Some(asks) = asks else {
        return Err(failed("AutoCua SB's desktop is not started".to_string()));
    };
    py.detach(move || -> Reply {
        let (tell, told) = channel();
        asks.send((what, tell)).map_err(|_| STOPPED.to_string())?;
        match told.recv_timeout(ASK_TIMEOUT) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => Err(format!("AutoCua SB's desktop did not answer in {} s", ASK_TIMEOUT.as_secs())),
            Err(RecvTimeoutError::Disconnected) => Err(STOPPED.to_string()),
        }
    })
    .map_err(failed)
}

#[pyfunction]
#[doc = "Make the second desktop, once: a later call finds it up. Gives back where its\n\
picture sits on the screen: {\"x0\", \"y0\", \"width\", \"height\"}, in real pixels."]
fn start(py: Python<'_>) -> PyResult<Bound<'_, PyDict>> {
    let [x0, y0, width, height] = py.detach(|| unsafe { start_device() }).map_err(failed)?;
    let screen = PyDict::new(py);
    screen.set_item("x0", x0)?;
    screen.set_item("y0", y0)?;
    screen.set_item("width", width)?;
    screen.set_item("height", height)?;
    Ok(screen)
}

#[pyfunction]
#[doc = "One picture of the second desktop: width x height pixels (see start), top row\n\
first, four bytes each: blue, green, red, unused."]
fn shot<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
    match ask(py, Ask::Shot)? {
        Answer::Picture(pixels) => Ok(PyBytes::new(py, &pixels)),
        Answer::Json(_) => Err(failed("the second desktop answered a picture with text".to_string())),
    }
}

/// An answer that is JSON text.
fn json_answer(answer: Answer) -> PyResult<String> {
    match answer {
        Answer::Json(text) => Ok(text),
        Answer::Picture(_) => Err(failed("the second desktop answered text with a picture".to_string())),
    }
}

#[pyfunction]
#[doc = "The second desktop's windows on show, top first, and what UI Automation finds\n\
to click or type in, numbered from 1, as JSON text: {\"windows\": [...],\n\
\"elements\": [...]}. Boxes are in screen pixels."]
fn scan(py: Python<'_>) -> PyResult<String> {
    json_answer(ask(py, Ask::Scan)?)
}

#[pyfunction]
#[doc = "Start a program (a command line) on the second desktop, its window maximized,\n\
and wait up to 10 seconds for its window. As JSON text: {\"pid\", \"window\" (its\n\
title, or null), \"exited\" (it ended with no window)}."]
fn launch(py: Python<'_>, command: String) -> PyResult<String> {
    json_answer(ask(py, Ask::Launch(command))?)
}

#[pyfunction]
#[doc = "Type text into the app in front on the second desktop, where its cursor is. As\n\
JSON text: {\"window\" (the app's title), \"characters\" (how many went)}."]
fn typewrite(py: Python<'_>, text: String) -> PyResult<String> {
    json_answer(ask(py, Ask::Type(text))?)
}

#[pyfunction]
#[doc = "Press a key, or a key with Ctrl, Shift or Alt held (\"enter\", \"ctrl+s\"), in the\n\
app in front on the second desktop. As JSON text: {\"window\", \"keys\"}."]
fn hotkey(py: Python<'_>, keys: String) -> PyResult<String> {
    json_answer(ask(py, Ask::Key(keys))?)
}

#[pyfunction]
#[doc = "Whether the second desktop is up."]
fn running() -> bool {
    DEVICE.lock().map(|device| device.as_ref().is_some_and(|up| !up.worker.is_finished())).unwrap_or(false)
}

#[pyfunction]
#[doc = "End every app on the second desktop and let it go. Nothing to do when it is not up."]
fn stop(py: Python<'_>) {
    py.detach(|| {
        let gone = DEVICE.lock().ok().and_then(|mut device| device.take());
        if let Some(gone) = gone {
            unsafe { close_device(gone) };
        }
    });
}

/// Fills the cua_sandbox part of the module with the functions above.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(start, m)?)?;
    m.add_function(wrap_pyfunction!(shot, m)?)?;
    m.add_function(wrap_pyfunction!(scan, m)?)?;
    m.add_function(wrap_pyfunction!(launch, m)?)?;
    m.add_function(wrap_pyfunction!(typewrite, m)?)?;
    m.add_function(wrap_pyfunction!(hotkey, m)?)?;
    m.add_function(wrap_pyfunction!(running, m)?)?;
    m.add_function(wrap_pyfunction!(stop, m)?)?;
    Ok(())
}

// ---------------------------------------------------------------- hands: not wired to a tool yet

/// What the screen tools that come next are built from (left_click, input,
/// scroll): a click and a wheel turn sent as window
/// messages, and UI Automation's patterns (press, set a value, tick). Each
/// was measured on the second desktop while this file was a program of its
/// own (Character Map: typed, clicked and scrolled with the real mouse and
/// keyboard untouched). No tool calls them yet: open_app, typewrite and hotkey
/// are the screen tools that work in AutoCua SB so far (each has its AutoCua
/// SB branch in the tool's own file, under windows/controller).
#[allow(dead_code)]
mod hands {
    use super::*;

    const WM_MOUSEMOVE: u32 = 0x0200;
    const WM_LBUTTONDOWN: u32 = 0x0201;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_MOUSEWHEEL: u32 = 0x020A;
    const MK_LBUTTON: usize = 0x0001;
    const CWP_SKIPINVISIBLE: u32 = 0x0001;
    const CWP_SKIPTRANSPARENT: u32 = 0x0004;

    #[link(name = "user32")]
    extern "system" {
        fn WindowFromPoint(point: Point) -> Handle;
        fn ChildWindowFromPointEx(hwnd: Handle, point: Point, flags: u32) -> Handle;
        fn ScreenToClient(hwnd: Handle, point: *mut Point) -> i32;
    }

    #[link(name = "oleaut32")]
    extern "system" {
        fn SysAllocString(text: *const u16) -> *mut u16;
    }

    fn lparam_point(x: i32, y: i32) -> isize {
        (((y as u16 as u32) << 16) | (x as u16 as u32)) as isize
    }

    /// The innermost window under a screen point of this thread's desktop.
    unsafe fn deepest_window_at(x: i32, y: i32) -> Handle {
        let mut hwnd = WindowFromPoint(Point { x, y });
        while !hwnd.is_null() {
            let mut local = Point { x, y };
            ScreenToClient(hwnd, &mut local);
            let child = ChildWindowFromPointEx(hwnd, local, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT);
            if child.is_null() || child == hwnd {
                break;
            }
            hwnd = child;
        }
        hwnd
    }

    /// A left click at a screen point, sent as messages to the window under
    /// it. For things UI Automation cannot press. The real cursor never moves.
    unsafe fn click_at(x: i32, y: i32) -> Handle {
        let hwnd = deepest_window_at(x, y);
        if !hwnd.is_null() {
            let mut local = Point { x, y };
            ScreenToClient(hwnd, &mut local);
            let lp = lparam_point(local.x, local.y);
            PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp);
            PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp);
            PostMessageW(hwnd, WM_LBUTTONUP, 0, lp);
        }
        hwnd
    }

    /// Mouse-wheel scroll at a screen point, as a message (down when notches > 0).
    unsafe fn scroll_at(x: i32, y: i32, notches: i32) -> Handle {
        let hwnd = deepest_window_at(x, y);
        if !hwnd.is_null() {
            PostMessageW(hwnd, WM_MOUSEWHEEL, ((-120 * notches) as u16 as usize) << 16, lparam_point(x, y));
        }
        hwnd
    }

    const IID_VALUE_PATTERN: Guid =
        Guid { d1: 0xa94cd8b1, d2: 0x0844, d3: 0x4cd6, d4: [0x9d, 0x2d, 0x64, 0x05, 0x37, 0xab, 0x39, 0xe9] };
    const IID_INVOKE_PATTERN: Guid =
        Guid { d1: 0xfb377fbe, d2: 0x8ea6, d3: 0x46d5, d4: [0x9c, 0x73, 0x64, 0x99, 0x64, 0x2d, 0x30, 0x59] };
    const IID_TOGGLE_PATTERN: Guid =
        Guid { d1: 0x94cf8058, d2: 0x9b8d, d3: 0x4ab9, d4: [0x8b, 0xfd, 0x4c, 0xd0, 0xa3, 0x3c, 0x8c, 0x70] };

    // Places in the tables of IUIAutomation's own QueryInterface, an element's
    // GetCurrentPattern, and the value, invoke and toggle patterns.
    const QUERY_INTERFACE: usize = 0;
    const EL_GET_CURRENT_PATTERN: usize = 16;
    const VALUE_SET_VALUE: usize = 3;
    const VALUE_CURRENT_VALUE: usize = 4;
    const INVOKE_INVOKE: usize = 3;
    const TOGGLE_TOGGLE: usize = 3;
    const TOGGLE_CURRENT_STATE: usize = 4;
    const UIA_INVOKE: i32 = 10000;
    const UIA_VALUE: i32 = 10002;
    const UIA_TOGGLE: i32 = 10015;

    /// One of an element's patterns (what it can do: hold a value, be pressed,
    /// be ticked), as the interface `interface` names.
    unsafe fn pattern(element: &Com, id: i32, interface: &Guid) -> Res<Com> {
        let call: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> i32 =
            element.method(EL_GET_CURRENT_PATTERN);
        let mut unknown = null_mut();
        check(call(element.0, id, &mut unknown))?;
        if unknown.is_null() {
            return Err(format!("the element has no pattern {id}"));
        }
        let unknown = Com(unknown);
        let query: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32 =
            unknown.method(QUERY_INTERFACE);
        let mut typed = null_mut();
        check(query(unknown.0, interface, &mut typed))?;
        if typed.is_null() { Err(format!("the element has no pattern {id}")) } else { Ok(Com(typed)) }
    }

    /// Set a field's text through its value pattern.
    unsafe fn set_value(value: &Com, text: &str) -> Res<()> {
        let call: unsafe extern "system" fn(*mut c_void, *mut u16) -> i32 = value.method(VALUE_SET_VALUE);
        let text = wide(text);
        let bstr = SysAllocString(text.as_ptr());
        let result = call(value.0, bstr);
        SysFreeString(bstr);
        check(result)
    }
}
