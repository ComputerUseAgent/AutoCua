//! Second Windows: a second desktop for the agent, inside your own Windows session.
//!
//! Shared with your main desktop: the same user, files, internet, installed apps
//! and clipboard. Its own: its windows, its focus and its picture. Nothing on it
//! moves your mouse or takes your keyboard, and nothing on it shows on your
//! screen except through the viewer window this program opens. The taskbar strip
//! is cropped out of every picture.
//!
//! RAM: the desktop object itself is a few MB of desktop heap. Only the apps
//! started on it use memory, the same as opening them on your main desktop.
//!
//! Standalone: this one file, the Rust standard library and Windows' own DLLs.
//! No crates, no cargo, no Python, no AutoCua code. Windows only.
//!
//! Run it through test.py beside this file, from D:\AutoCua:
//!     venv\Scripts\python.exe AutoCua\CUA_Sandbox\windows\test.py [OPTIONS]
//! test.py builds this file with rustc alone (no cargo) into the system temp
//! folder, %TEMP%\AutoCua\Win_CUA.exe, never beside the code, and again
//! whenever the file changes. It runs it from D:\AutoCua with the OPTIONS.
//! With none, test.py shows the second desktop in a page of its own, through
//! --serve below.
//!
//! The program's modes:
//!     (no option)
//!         Make the second desktop, start Explorer (wallpaper, icons) and
//!         Character Map on it, and show it live in a window on your screen.
//!         Closing the window ends everything that runs on it.
//!     --shot shot.png
//!         The same, but save one screenshot of it, print a report and exit.
//!     --computer-use
//!         Computer use on it with no mouse and no keyboard: scan Character Map
//!         with UI Automation, type, click, scroll, scan again. Its pictures
//!         are cu_1_scan.png and cu_2_after.png.
//!     --run APP --scan scan.png
//!         Start APP on it and save the agent's scan: what UI Automation finds
//!         to click or type in, numbered on a picture, and print the list.
//!     --serve
//!         For another program, the page of test.py for one. No Character Map
//!         and no window: every new picture of it goes out on stdout, and a
//!         line "run CMD" on stdin starts CMD on it. It ends when stdin closes.
//!     --device
//!         For the app's agent, in an AutoCua SB run (service.py beside this
//!         file starts it). No Character Map and no window: one request a
//!         line on stdin, one reply each on stdout. It ends when stdin closes,
//!         and every app on the second desktop with it. See device_loop.
//!
//!     --no-shell   no Explorer on it (no wallpaper or icons, least RAM)
//!     --run CMD    start CMD on it instead of Character Map (can repeat)
//!     --wait SEC   seconds the apps get to start before --shot or --scan (default 6)
//!
//! Every picture it saves goes, for now, to the debug folder of the folder it
//! runs in, under the file name given. test.py runs it from D:\AutoCua, so
//! --shot shot.png saves D:\AutoCua\debug\shot.png.

#![allow(non_snake_case)] // the file keeps the name of the Python script it came from

use std::collections::HashSet;
use std::ffi::c_void;
use std::io::Write;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const DESKTOP: &str = "AutoCuaAgent"; // the second desktop, inside window station WinSta0
const FRAME_INTERVAL: Duration = Duration::from_millis(250); // between two pictures for the viewer

const USAGE: &str = "Second Windows: a second desktop for the agent.

Win_CUA.exe [--shot PNG] [--computer-use] [--no-shell] [--run CMD] [--scan PNG] [--wait SEC] [--serve] [--device]

  --shot PNG       save one screenshot and exit
  --computer-use   scan and act on Character Map there
  --no-shell       no Explorer on it
  --run CMD        program to start on it (can repeat)
  --scan PNG       save the agent's scan of it and exit
  --wait SEC       seconds before --shot or --scan (default 6)
  --serve          for test.py's page: pictures out on stdout, \"run CMD\" lines in on stdin
  --device         for the app's agent (AutoCua SB): one request a line on stdin, one reply each on stdout

Every picture is saved in the debug folder at the root, under the file name given.";

/// Any Windows handle: a window, a desktop, a process, a picture.
type Handle = *mut c_void;
/// What a step gives back: its result, or what went wrong, in words.
type Res<T> = Result<T, String>;
type WndProc = unsafe extern "system" fn(Handle, u32, usize, isize) -> isize;
type EnumProc = unsafe extern "system" fn(Handle, isize) -> i32;

const GENERIC_ALL: u32 = 0x1000_0000;
const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
const TOKEN_DUPLICATE: u32 = 0x0002;
const TOKEN_QUERY: u32 = 0x0008;
const STARTF_FORCEOFFFEEDBACK: u32 = 0x0000_0080;
const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
const SW_SHOWMAXIMIZED: u16 = 3;
const WM_NULL: u32 = 0x0000;
const SMTO_ABORTIFHUNG: u32 = 0x0002;
const STILL_ACTIVE: u32 = 259;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;
const JOB_PROCESS_ID_LIST: i32 = 3;
const JOB_EXTENDED_LIMITS: i32 = 9;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_VM_READ: u32 = 0x0010;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;
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
const HALFTONE: i32 = 4;
const BLACK_BRUSH: i32 = 4;
const NULL_BRUSH: i32 = 5;
const TRANSPARENT: i32 = 1;
const LABEL_COLOR: u32 = 0x00FF_00FF; // magenta, as a COLORREF (0x00BBGGRR)
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WM_DESTROY: u32 = 0x0002;
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_TIMER: u32 = 0x0113;
const WM_CHAR: u32 = 0x0102;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_MOUSEWHEEL: u32 = 0x020A;
const EM_SETSEL: u32 = 0x00B1;
const MK_LBUTTON: usize = 0x0001;
const CWP_SKIPINVISIBLE: u32 = 0x0001;
const CWP_SKIPTRANSPARENT: u32 = 0x0004;
const IDC_ARROW: usize = 32512;
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

#[repr(C)]
#[allow(dead_code)]
struct JobProcessIds {
    assigned: u32,
    in_list: u32,
    list: [usize; 512],
}

#[repr(C)]
#[allow(dead_code)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[repr(C)]
#[allow(dead_code)]
struct WndClassExW {
    size: u32,
    style: u32,
    wnd_proc: WndProc,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu_name: *const u16,
    class_name: *const u16,
    icon_small: Handle,
}

#[repr(C)]
#[allow(dead_code)]
struct PaintStruct {
    dc: Handle,
    erase: i32,
    paint: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

#[repr(C)]
#[allow(dead_code)]
struct Msg {
    hwnd: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    point: Point,
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
    fn FindWindowW(class: *const u16, title: *const u16) -> Handle;
    fn FindWindowExW(parent: Handle, after: Handle, class: *const u16, title: *const u16) -> Handle;
    fn EnumWindows(each: EnumProc, param: isize) -> i32;
    fn EnumChildWindows(hwnd: Handle, each: EnumProc, param: isize) -> i32;
    fn IsWindowVisible(hwnd: Handle) -> i32;
    fn IsIconic(hwnd: Handle) -> i32;
    fn GetWindowRect(hwnd: Handle, rect: *mut Rect) -> i32;
    fn GetWindowLongPtrW(hwnd: Handle, index: i32) -> isize;
    fn GetLayeredWindowAttributes(hwnd: Handle, key: *mut u32, alpha: *mut u8, flags: *mut u32) -> i32;
    fn GetWindowThreadProcessId(hwnd: Handle, pid: *mut u32) -> u32;
    fn GetClassNameW(hwnd: Handle, text: *mut u16, len: i32) -> i32;
    fn GetWindowTextW(hwnd: Handle, text: *mut u16, len: i32) -> i32;
    fn GetForegroundWindow() -> Handle;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn WindowFromPoint(point: Point) -> Handle;
    fn ChildWindowFromPointEx(hwnd: Handle, point: Point, flags: u32) -> Handle;
    fn ScreenToClient(hwnd: Handle, point: *mut Point) -> i32;
    fn ClientToScreen(hwnd: Handle, point: *mut Point) -> i32;
    fn PostMessageW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> i32;
    fn PrintWindow(hwnd: Handle, dc: Handle, flags: u32) -> i32;
    fn SendMessageTimeoutW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize, flags: u32, timeout_ms: u32,
                           result: *mut usize) -> isize;
    fn PaintDesktop(dc: Handle) -> i32;
    fn GetDC(hwnd: Handle) -> Handle;
    fn ReleaseDC(hwnd: Handle, dc: Handle) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, value: *mut c_void, change: u32) -> i32;
    fn SetProcessDpiAwarenessContext(context: isize) -> i32;
    fn RegisterClassExW(class: *const WndClassExW) -> u16;
    fn CreateWindowExW(ex_style: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32,
                       w: i32, h: i32, parent: Handle, menu: Handle, instance: Handle,
                       param: *mut c_void) -> Handle;
    fn DefWindowProcW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn GetMessageW(msg: *mut Msg, hwnd: Handle, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn BeginPaint(hwnd: Handle, paint: *mut PaintStruct) -> Handle;
    fn EndPaint(hwnd: Handle, paint: *const PaintStruct) -> i32;
    fn GetClientRect(hwnd: Handle, rect: *mut Rect) -> i32;
    fn InvalidateRect(hwnd: Handle, rect: *const Rect, erase: i32) -> i32;
    fn SetTimer(hwnd: Handle, id: usize, ms: u32, func: *mut c_void) -> usize;
    fn LoadCursorW(instance: Handle, name: *const u16) -> Handle;
    fn SetWindowTextW(hwnd: Handle, text: *const u16) -> i32;
    fn FillRect(dc: Handle, rect: *const Rect, brush: Handle) -> i32;
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
    fn StretchDIBits(dc: Handle, x: i32, y: i32, w: i32, h: i32, from_x: i32, from_y: i32, from_w: i32,
                     from_h: i32, bits: *const c_void, info: *const BitmapInfo, usage: u32, rop: u32) -> i32;
    fn SetStretchBltMode(dc: Handle, mode: i32) -> i32;
    fn SetBrushOrgEx(dc: Handle, x: i32, y: i32, old: *mut Point) -> i32;
    fn SetWindowOrgEx(dc: Handle, x: i32, y: i32, old: *mut Point) -> i32;
    fn GetStockObject(index: i32) -> Handle;
    fn GdiFlush() -> i32;
    fn CreatePen(style: i32, width: i32, color: u32) -> Handle;
    fn CreateFontW(height: i32, width: i32, escapement: i32, orientation: i32, weight: i32, italic: u32,
                   underline: u32, strike_out: u32, char_set: u32, out_precision: u32, clip_precision: u32,
                   quality: u32, pitch_and_family: u32, face: *const u16) -> Handle;
    fn Rectangle(dc: Handle, left: i32, top: i32, right: i32, bottom: i32) -> i32;
    fn SetBkMode(dc: Handle, mode: i32) -> i32;
    fn SetTextColor(dc: Handle, color: u32) -> u32;
    fn TextOutW(dc: Handle, x: i32, y: i32, text: *const u16, len: i32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateProcessW(app: *const u16, command: *mut u16, process_security: *mut c_void,
                      thread_security: *mut c_void, inherit: i32, flags: u32, environment: *mut c_void,
                      folder: *const u16, startup: *mut StartupInfoW, info: *mut ProcessInformation) -> i32;
    fn ResumeThread(thread: Handle) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateJobObjectW(security: *mut c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32) -> i32;
    fn QueryInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32,
                                 returned: *mut u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn TerminateJobObject(job: Handle, code: u32) -> i32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
    fn QueryFullProcessImageNameW(process: Handle, flags: u32, name: *mut u16, len: *mut u32) -> i32;
    fn K32GetProcessMemoryInfo(process: Handle, counters: *mut ProcessMemoryCountersEx, len: u32) -> i32;
    fn GetModuleHandleW(name: *const u16) -> Handle;
    fn GetCurrentProcess() -> Handle;
    fn GetLastError() -> u32;
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
    fn SysAllocString(text: *const u16) -> *mut u16;
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

/// Where a picture is saved, for now: the debug folder of the folder this
/// program runs in (test.py runs it from the root, so D:\AutoCua\debug),
/// under the file name of `name`. A folder in `name` is dropped, and the
/// debug folder is made when it is not there yet.
fn debug_path(name: &str) -> Res<PathBuf> {
    let debug = std::env::current_dir()
        .map_err(|e| format!("could not find the folder this program runs in: {e}"))?
        .join("debug");
    std::fs::create_dir_all(&debug).map_err(|e| format!("could not make {}: {e}", debug.display()))?;
    let file = base_name(name);
    if file.is_empty() {
        return Err(format!("{} is not a file name for a picture", quote(name)));
    }
    Ok(debug.join(file))
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

/// A job that ends every process in it when this program ends, however it
/// ends (window closed, Ctrl+C, a crash): nothing keeps running unseen.
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

/// The environment Explorer gives a program, not this program's own. A
/// program started from an editor carries the editor's settings, and one of
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
/// Windows closes them when this program is gone, which ends what is in them.
static EXTRA_JOBS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Start `command` with its windows on `desktop`. It starts suspended and
/// joins the job first, so whatever it starts in turn is in the job too.
/// A Store app cannot join: Windows has put it in a job of its own by then.
/// A new job made just for it can sit inside that one, and ends the app the
/// same way. Without it, Paint and ChatGPT stayed running on the second
/// desktop after this program had gone (seen). `show`, when given, is how
/// its first window shows (SW_SHOWMAXIMIZED), for the programs that follow
/// it. Gives back its process id.
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
    let flags = CREATE_SUSPENDED | if env.is_null() { 0 } else { CREATE_UNICODE_ENVIRONMENT };
    let ok = CreateProcessW(null(), line.as_mut_ptr(), null_mut(), null_mut(), 0, flags, env, null(),
                            &mut si, &mut pi);
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
            eprintln!("{command} could not be put in a job: it will not end with this program");
            if !own.is_null() {
                CloseHandle(own);
            }
        }
    }
    ResumeThread(pi.thread);
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

/// The program's file name out of a command line.
fn exe_name(command: &str) -> String {
    let first = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    base_name(first)
}

// ---------------------------------------------------------------- windows

const TASKBARS: [&str; 2] = ["Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
const TASKBAR_CONTENT: &str = "Windows.UI.Composition.DesktopWindowContentBridge";

/// A window for the reports: whose it is, its class and its title.
#[derive(Clone)]
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

/// The windows on show on your main desktop (this thread's desktop).
unsafe fn main_desktop_windows() -> Vec<(usize, WindowInfo)> {
    unsafe extern "system" fn collect(hwnd: Handle, found: isize) -> i32 {
        // `found` is the list below, which outlives the EnumWindows call.
        if is_shown(hwnd) {
            (*(found as *mut Vec<(usize, WindowInfo)>)).push((hwnd as usize, window_info(hwnd)));
        }
        1
    }
    let mut found: Vec<(usize, WindowInfo)> = Vec::new();
    EnumWindows(collect, &mut found as *mut Vec<(usize, WindowInfo)> as isize);
    found
}

unsafe fn child_windows(hwnd: Handle) -> Vec<usize> {
    unsafe extern "system" fn collect(child: Handle, found: isize) -> i32 {
        (*(found as *mut Vec<usize>)).push(child as usize);
        1
    }
    let mut found: Vec<usize> = Vec::new();
    EnumChildWindows(hwnd, collect, &mut found as *mut Vec<usize> as isize);
    found
}

/// (name, private MB) for every process running in the job, and in the jobs
/// made for the programs that could not join it.
unsafe fn job_memory(job: Handle) -> Vec<(String, f64)> {
    let mut jobs = vec![job as usize];
    jobs.extend(EXTRA_JOBS.lock().unwrap().iter());
    let mut rows = Vec::new();
    for job in jobs {
        let mut ids: JobProcessIds = zeroed();
        if QueryInformationJobObject(job as Handle, JOB_PROCESS_ID_LIST,
                                     &mut ids as *mut JobProcessIds as *mut c_void,
                                     size_of::<JobProcessIds>() as u32, null_mut()) == 0
        {
            continue;
        }
        for &pid in &ids.list[..(ids.in_list as usize).min(ids.list.len())] {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid as u32);
            if handle.is_null() {
                continue;
            }
            let mut mem: ProcessMemoryCountersEx = zeroed();
            mem.cb = size_of::<ProcessMemoryCountersEx>() as u32;
            if K32GetProcessMemoryInfo(handle, &mut mem, mem.cb) != 0 {
                rows.push((process_name(pid as u32), mem.private_usage as f64 / 1048576.0));
            }
            CloseHandle(handle);
        }
    }
    rows
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

/// Ask Windows to paint one window into `canvas` at its place on screen.
/// PrintWindow works for a window nobody can see, which is every window here.
/// Whether a window's thread answers within 200 ms. One that does not (a
/// hung app) is left out of the picture and the scan: PrintWindow waits
/// for that thread to paint, for good, and UI Automation waits out its own
/// timeouts on it (a scan took 114 s with one hung window there, measured).
unsafe fn answers(hwnd: Handle) -> bool {
    let mut answer = 0usize;
    SendMessageTimeoutW(hwnd, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 200, &mut answer) != 0
}

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

/// Numbered magenta boxes, the way the agent's screenshot marks elements.
unsafe fn draw_boxes(canvas: Handle, boxes: &[(usize, [i32; 4])]) {
    let face = wide("Arial");
    let pen = CreatePen(0, 2, LABEL_COLOR);
    let font = CreateFontW(-22, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 4, 0, face.as_ptr());
    let old_pen = SelectObject(canvas, pen);
    let old_brush = SelectObject(canvas, GetStockObject(NULL_BRUSH));
    let old_font = SelectObject(canvas, font);
    SetBkMode(canvas, TRANSPARENT);
    SetTextColor(canvas, LABEL_COLOR);
    for &(index, [left, top, right, bottom]) in boxes {
        Rectangle(canvas, left, top, right, bottom);
        let label: Vec<u16> = format!("[{index}]").encode_utf16().collect();
        TextOutW(canvas, left + 3, top + 2, label.as_ptr(), label.len() as i32);
    }
    SelectObject(canvas, old_font);
    SelectObject(canvas, old_brush);
    SelectObject(canvas, old_pen);
    DeleteObject(pen);
    DeleteObject(font);
}

/// One picture of this thread's desktop, the part `frames` names: the
/// wallpaper, then every window on show from the bottom up, then `boxes` as
/// (number, [left, top, right, bottom]). Gives back (top-down BGRA bytes,
/// windows).
unsafe fn capture(frames: &Frames, boxes: &[(usize, [i32; 4])]) -> Res<(Vec<u8>, Vec<usize>)> {
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
    let windows = visible_windows();
    for &hwnd in windows.iter().rev() {
        paint_window(canvas, screen, hwnd as Handle);
    }
    if !boxes.is_empty() {
        draw_boxes(canvas, boxes);
    }
    GdiFlush();
    let len = frames.width as usize * frames.height as usize * 4;
    let pixels = std::slice::from_raw_parts(bits as *const u8, len).to_vec();
    SelectObject(canvas, old);
    DeleteObject(dib);
    DeleteDC(canvas);
    ReleaseDC(null_mut(), screen);
    Ok((pixels, windows))
}

/// The latest picture of the second desktop, shared with the viewer.
struct Frames {
    x0: i32,
    y0: i32,
    width: i32,
    height: i32,
    stop: AtomicBool,
    latest: Mutex<Latest>,
}

#[derive(Default)]
struct Latest {
    pixels: Option<Arc<Vec<u8>>>,
    windows: Vec<WindowInfo>,
    count: u64,
    error: Option<String>,
}

/// Runs on a thread of its own that moves onto the second desktop: window
/// messages, and so PrintWindow, only reach windows on the caller's desktop.
unsafe fn capture_loop(desk: usize, frames: &Frames) {
    if SetThreadDesktop(desk as Handle) == 0 {
        let code = GetLastError();
        frames.latest.lock().unwrap().error = Some(format!("could not join the second desktop (error {code})"));
        return;
    }
    while !frames.stop.load(Ordering::SeqCst) {
        match capture(frames, &[]) {
            Ok((pixels, windows)) => {
                let infos: Vec<WindowInfo> = windows.iter().map(|&hwnd| window_info(hwnd as Handle)).collect();
                let mut latest = frames.latest.lock().unwrap();
                latest.pixels = Some(Arc::new(pixels));
                latest.windows = infos;
                latest.count += 1;
            }
            Err(e) => {
                frames.latest.lock().unwrap().error = Some(e);
                return;
            }
        }
        thread::sleep(FRAME_INTERVAL);
    }
}

/// `n` bits at a time into bytes, lowest bit first, as deflate packs them.
struct Bits {
    out: Vec<u8>,
    hold: u64,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: u32, n: u32) {
        self.hold |= (value as u64) << self.count;
        self.count += n;
        while self.count >= 8 {
            self.out.push(self.hold as u8);
            self.hold >>= 8;
            self.count -= 8;
        }
    }

    /// A code of `n` bits, which goes in highest bit first.
    fn code(&mut self, code: u32, n: u32) {
        self.put(code.reverse_bits() >> (32 - n), n);
    }

    /// A byte (0 to 255), the end mark (256) or a length (257 to 285), in
    /// deflate's fixed code.
    fn symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + (symbol - 144), 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xC0 + (symbol - 280), 8),
        }
    }
}

/// `data` packed as a zlib stream (RFC 1950 and 1951), this file's own
/// packer in place of a library: one block in the fixed code, and a run of
/// bytes seen before is written as "that many bytes, from that far back".
/// A table keeps where each three bytes were last seen.
fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: vec![0x78, 0x01], hold: 0, count: 0 };
    bits.put(1, 1); // the last block
    bits.put(1, 2); // in the fixed code
    let key = |i: usize| (data[i] as usize) << 8 ^ (data[i + 1] as usize) << 4 ^ data[i + 2] as usize;
    let mut last = vec![usize::MAX; 1 << 16];
    let mut i = 0;
    while i < data.len() {
        let (mut len, mut back) = (0, 0);
        if i + 3 <= data.len() {
            let seen = std::mem::replace(&mut last[key(i)], i);
            if seen != usize::MAX && i - seen <= 32768 {
                let most = (data.len() - i).min(258);
                while len < most && data[seen + len] == data[i + len] {
                    len += 1;
                }
                back = i - seen;
            }
        }
        if len < 3 {
            bits.symbol(data[i] as u32);
            i += 1;
            continue;
        }
        let l = (len - 3) as u32;
        if len == 258 {
            bits.symbol(285);
        } else if l < 8 {
            bits.symbol(257 + l);
        } else {
            let extra = 29 - l.leading_zeros();
            bits.symbol(261 + 4 * extra + ((l >> extra) & 3));
            bits.put(l & ((1 << extra) - 1), extra);
        }
        let d = (back - 1) as u32;
        if d < 4 {
            bits.code(d, 5);
        } else {
            let extra = 30 - d.leading_zeros();
            bits.code(2 * (extra + 1) + ((d >> extra) & 1), 5);
            bits.put(d & ((1 << extra) - 1), extra);
        }
        for j in i + 1..(i + len).min(data.len().saturating_sub(2)) {
            last[key(j)] = j;
        }
        i += len;
    }
    bits.symbol(256); // the end of the block
    bits.put(0, 7); // fill the last byte
    let mut out = bits.out;
    let (mut a, mut b) = (1u32, 0u32); // Adler-32 of the data, zlib's check
    for part in data.chunks(5552) {
        for &byte in part {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

/// CRC-32, the check at the end of every PNG chunk.
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, entry) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *entry = c;
    }
    let mut crc = !0u32;
    for &byte in data {
        crc = table[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// Top-down BGRA pixels to a PNG file.
fn write_png(path: &Path, width: i32, height: i32, bgra: &[u8]) -> Res<()> {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    if w == 0 || h == 0 || bgra.len() < w * h * 4 {
        return Err(format!("no picture to save in {}", path.display()));
    }
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for row in bgra.chunks_exact(w * 4).take(h) {
        raw.push(0); // this row is stored as it is
        for pixel in row.chunks_exact(4) {
            raw.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    let mut header = Vec::new();
    header.extend_from_slice(&(w as u32).to_be_bytes());
    header.extend_from_slice(&(h as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits a colour, RGB
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [(b"IHDR", header), (b"IDAT", zlib(&raw)), (b"IEND", Vec::new())] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = png.len();
        png.extend_from_slice(kind);
        png.extend_from_slice(&data);
        let check = crc32(&png[start..]);
        png.extend_from_slice(&check.to_be_bytes());
    }
    std::fs::write(path, png).map_err(|e| format!("could not save {}: {e}", path.display()))
}

/// Top-down BGRA pixels as a BMP file, 24 bits a pixel and not packed: the
/// picture a web page takes that costs no time to make.
fn bmp(width: i32, height: i32, bgra: &[u8]) -> Vec<u8> {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    if w == 0 || h == 0 || bgra.len() < w * h * 4 {
        return Vec::new();
    }
    let row = (w * 3 + 3) & !3; // every row is filled up to a multiple of four bytes
    let mut file = Vec::with_capacity(54 + row * h);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&((54 + row * h) as u32).to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&54u32.to_le_bytes()); // where the pixels start
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&(w as i32).to_le_bytes());
    file.extend_from_slice(&(h as i32).to_le_bytes()); // rows from the bottom up
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    for line in bgra.chunks_exact(w * 4).take(h).rev() {
        for pixel in line.chunks_exact(4) {
            file.extend_from_slice(&pixel[..3]);
        }
        file.resize(file.len() + row - w * 3, 0);
    }
    file
}

// ---------------------------------------------------------------- input without mouse or keyboard

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

/// Type into a window as character messages. The real keyboard is never used.
unsafe fn type_text(hwnd: Handle, text: &str) {
    for unit in text.encode_utf16() {
        PostMessageW(hwnd, WM_CHAR, unit as usize, 1);
    }
}

/// Mouse-wheel scroll at a screen point, as a message (down when notches > 0).
unsafe fn scroll_at(x: i32, y: i32, notches: i32) -> Handle {
    let hwnd = deepest_window_at(x, y);
    if !hwnd.is_null() {
        PostMessageW(hwnd, WM_MOUSEWHEEL, ((-120 * notches) as u16 as usize) << 16, lparam_point(x, y));
    }
    hwnd
}

// ---------------------------------------------------------------- UI Automation, through COM by hand

const CLSID_CUIAUTOMATION: Guid =
    Guid { d1: 0xff48dba4, d2: 0x60ef, d3: 0x4201, d4: [0xaa, 0x87, 0x54, 0x10, 0x3e, 0xef, 0x59, 0x4e] };
const IID_IUIAUTOMATION: Guid =
    Guid { d1: 0x30cbe57d, d2: 0xd9d0, d3: 0x452a, d4: [0xab, 0x13, 0x7a, 0xc5, 0xac, 0x48, 0x25, 0xee] };
const IID_VALUE_PATTERN: Guid =
    Guid { d1: 0xa94cd8b1, d2: 0x0844, d3: 0x4cd6, d4: [0x9d, 0x2d, 0x64, 0x05, 0x37, 0xab, 0x39, 0xe9] };
const IID_INVOKE_PATTERN: Guid =
    Guid { d1: 0xfb377fbe, d2: 0x8ea6, d3: 0x46d5, d4: [0x9c, 0x73, 0x64, 0x99, 0x64, 0x2d, 0x30, 0x59] };
const IID_TOGGLE_PATTERN: Guid =
    Guid { d1: 0x94cf8058, d2: 0x9b8d, d3: 0x4ab9, d4: [0x8b, 0xfd, 0x4c, 0xd0, 0xa3, 0x3c, 0x8c, 0x70] };

// The place of each method in its interface's table, counted from 0, as
// UIAutomationCore.dll's type library lists them. Places 0, 1 and 2 are
// every COM object's QueryInterface, AddRef and Release.
const QUERY_INTERFACE: usize = 0;
const RELEASE: usize = 2;
// IUIAutomation
const UIA_GET_ROOT_ELEMENT: usize = 5;
const UIA_ELEMENT_FROM_HANDLE: usize = 6;
const UIA_CREATE_CACHE_REQUEST: usize = 20;
const UIA_CREATE_TRUE_CONDITION: usize = 21;
// IUIAutomationElement
const EL_FIND_ALL: usize = 6;
const EL_FIND_ALL_BUILD_CACHE: usize = 8;
const EL_GET_CURRENT_PATTERN: usize = 16;
const EL_CURRENT_CONTROL_TYPE: usize = 21;
const EL_CURRENT_NAME: usize = 23;
const EL_CURRENT_NATIVE_WINDOW_HANDLE: usize = 36;
const EL_CURRENT_IS_OFFSCREEN: usize = 38;
const EL_CURRENT_BOUNDING_RECTANGLE: usize = 43;
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
// IUIAutomationValuePattern, IUIAutomationInvokePattern, IUIAutomationTogglePattern
const VALUE_SET_VALUE: usize = 3;
const VALUE_CURRENT_VALUE: usize = 4;
const INVOKE_INVOKE: usize = 3;
const TOGGLE_TOGGLE: usize = 3;
const TOGGLE_CURRENT_STATE: usize = 4;

const TREE_SCOPE_CHILDREN: i32 = 2;
const TREE_SCOPE_DESCENDANTS: i32 = 4;
const UIA_INVOKE: i32 = 10000;
const UIA_VALUE: i32 = 10002;
const UIA_TOGGLE: i32 = 10015;
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

// ---------------------------------------------------------------- computer use

/// Share of pixels that differ inside `rect` between two pictures.
fn changed(before: &[u8], after: &[u8], frames: &Frames, rect: [i32; 4]) -> f64 {
    let (w, h) = (frames.width.max(0), frames.height.max(0));
    let left = (rect[0] - frames.x0).clamp(0, w) as usize;
    let right = ((rect[2] - frames.x0).clamp(0, w) as usize).max(left);
    let (top, bottom) = ((rect[1] - frames.y0).clamp(0, h), (rect[3] - frames.y0).clamp(0, h));
    let (mut diff, mut total) = (0usize, 0usize);
    for y in (top..bottom).step_by(4) {
        let row = y as usize * w as usize;
        let a = &before[(row + left) * 4..(row + right) * 4];
        let b = &after[(row + left) * 4..(row + right) * 4];
        total += a.len();
        diff += 16 * (0..a.len()).step_by(16).filter(|&i| a[i..i + 3] != b[i..i + 3]).count();
    }
    diff as f64 / total.max(1) as f64
}

/// One element of the Character Map scan.
struct Item {
    element: Com,
    kind: &'static str,
    name: String,
    rect: [i32; 4],
}

/// The first element of that kind whose name holds `words`.
fn pick<'a>(items: &'a [Item], kind: &str, words: &str) -> Res<&'a Item> {
    items
        .iter()
        .find(|item| item.kind == kind && item.name.to_lowercase().contains(&words.to_lowercase()))
        .ok_or_else(|| format!("no {kind} {} in the scan", quote(words)))
}

/// The agent's loop on the second desktop, with no mouse and no keyboard:
/// scan with UI Automation, act on the elements directly, scan again.
unsafe fn computer_use(desk: usize, frames: &Frames) -> Res<()> {
    if SetThreadDesktop(desk as Handle) == 0 {
        println!("could not join the second desktop, error {}", GetLastError());
        return Ok(());
    }
    let uia = uia_client()?;
    let every = uia.object(UIA_CREATE_TRUE_CONDITION)?;

    let tops = find_all(&uia.object(UIA_GET_ROOT_ELEMENT)?, TREE_SCOPE_CHILDREN, &every, None)?;
    let mut names = Vec::new();
    for top in &tops {
        names.push(quote(&top.text(EL_CURRENT_NAME)?));
    }
    println!("\n1. UI Automation, from the second desktop, sees {} windows: {}", names.len(), names.join(", "));

    let title = wide("Character Map");
    let mut hwnd = null_mut();
    for _ in 0..100 {
        hwnd = FindWindowW(null(), title.as_ptr());
        if !hwnd.is_null() {
            break;
        }
        sleep(0.1);
    }
    if hwnd.is_null() {
        println!("Character Map did not open on the second desktop");
        return Ok(());
    }
    sleep(1.0);
    let window = element_from_handle(&uia, hwnd)?;

    let scan = |png: &str| -> Res<(Vec<Item>, String)> {
        let mut items = Vec::new();
        for element in find_all(&window, TREE_SCOPE_DESCENDANTS, &every, None)? {
            let r: Rect = element.get(EL_CURRENT_BOUNDING_RECTANGLE)?;
            if r.right > r.left && r.bottom > r.top && element.get::<i32>(EL_CURRENT_IS_OFFSCREEN)? == 0 {
                let kind = control_type(element.get(EL_CURRENT_CONTROL_TYPE)?);
                let name = element.text(EL_CURRENT_NAME)?;
                items.push(Item { element, kind, name, rect: [r.left, r.top, r.right, r.bottom] });
            }
        }
        let boxes: Vec<(usize, [i32; 4])> = items.iter().enumerate().map(|(n, item)| (n + 1, item.rect)).collect();
        let (pixels, _) = capture(frames, &boxes)?;
        let path = debug_path(png)?;
        write_png(&path, frames.width, frames.height, &pixels)?;
        Ok((items, path.display().to_string()))
    };

    let (items, path) = scan("cu_1_scan.png")?;
    println!("2. Scan: {} elements, numbered the way the agent sees them: {path}", items.len());
    for (n, item) in items.iter().enumerate() {
        println!("   [{}] {} {}", n + 1, item.kind, quote(&item.name));
    }

    println!("3. Act, with no mouse and no keyboard:");
    let edit = pick(&items, "Edit", "")?;
    let value = pattern(&edit.element, UIA_VALUE, &IID_VALUE_PATTERN)?;
    let edit_hwnd: Handle = edit.element.get(EL_CURRENT_NATIVE_WINDOW_HANDLE)?;

    type_text(edit_hwnd, "Hi");
    sleep(0.4);
    println!("   typewrite 'Hi' (character messages)          -> field: {}", quote(&value.text(VALUE_CURRENT_VALUE)?));

    let text = "Hello from the second desktop ";
    set_value(&value, text)?;
    sleep(0.3);
    println!("   input (UI Automation sets the field)         -> field: {}", quote(&value.text(VALUE_CURRENT_VALUE)?));

    let grid = child_windows(hwnd).into_iter().find(|&child| class_of(child as Handle).to_lowercase().contains("grid"));
    if let Some(grid) = grid {
        let grid = grid as Handle;
        let (mut r, mut origin) = (Rect::default(), Point::default());
        GetClientRect(grid, &mut r);
        ClientToScreen(grid, &mut origin);
        let (cell_w, cell_h) = (r.right as f64 / 20.0, r.bottom as f64 / 10.0);
        let (x, y) = (origin.x + (12.5 * cell_w) as i32, origin.y + (1.5 * cell_h) as i32); // the "A" cell
        let target = click_at(x, y);
        sleep(0.4);
        let end = text.encode_utf16().count();
        PostMessageW(edit_hwnd, EM_SETSEL, end, end as isize); // the caret to the end
        let select = pick(&items, "Button", "Select")?;
        pattern(&select.element, UIA_INVOKE, &IID_INVOKE_PATTERN)?.call(INVOKE_INVOKE)?;
        sleep(0.4);
        println!("   left_click at a point, cell 'A' (messages)   -> went to {}", quote(&class_of(target)));
        println!("   left_click 'Select' (UI Automation Invoke)   -> field: {}", quote(&value.text(VALUE_CURRENT_VALUE)?));

        let grid_rect = [origin.x, origin.y, origin.x + r.right, origin.y + r.bottom];
        let (before, _) = capture(frames, &[])?;
        scroll_at(x, y, 3);
        sleep(0.6);
        let (after, _) = capture(frames, &[])?;
        let mut share = changed(&before, &after, frames, grid_rect);
        println!("   scroll down 3 (wheel messages)               -> {:.0}% of the grid changed", share * 100.0);
        if share < 0.01 {
            // this grid ignores the wheel: press its scroll bar's button instead
            let down = pick(&items, "Button", "Line down")?;
            let press = pattern(&down.element, UIA_INVOKE, &IID_INVOKE_PATTERN)?;
            for _ in 0..3 {
                press.call(INVOKE_INVOKE)?;
                sleep(0.1);
            }
            sleep(0.5);
            let (after, _) = capture(frames, &[])?;
            share = changed(&before, &after, frames, grid_rect);
            println!("   scroll down 3 ('Line down', UI Automation)   -> {:.0}% of the grid changed", share * 100.0);
        }
    } else {
        println!("   (no character grid found, point click and scroll skipped)");
    }

    let advanced = pick(&items, "CheckBox", "Advanced")?;
    let toggle = pattern(&advanced.element, UIA_TOGGLE, &IID_TOGGLE_PATTERN)?;
    toggle.call(TOGGLE_TOGGLE)?;
    sleep(0.8);
    let state: i32 = toggle.get(TOGGLE_CURRENT_STATE)?;
    println!("   left_click 'Advanced view' (UI Automation)   -> {}", if state != 0 { "on" } else { "off" });

    let (items, path) = scan("cu_2_after.png")?;
    println!("4. Scan again: {} elements: {path}", items.len());
    Ok(())
}

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

/// The agent's scan of the second desktop: what UI Automation finds to
/// click or type in, in every window on show, numbered on a picture of it.
/// Gives back the windows on show, top first.
unsafe fn scan_desktop(desk: usize, frames: &Frames, png: &str) -> Res<Vec<WindowInfo>> {
    if SetThreadDesktop(desk as Handle) == 0 {
        return Err(last_error("could not join the second desktop"));
    }
    let uia = uia_client()?;
    let every = uia.object(UIA_CREATE_TRUE_CONDITION)?;
    let cache = scan_cache(&uia)?;
    let windows = visible_windows();
    let read = || find_elements(&uia, &every, &cache, &windows);

    // Chromium (so WebView2 and Electron) builds its element tree only when
    // someone first asks: read until two reads in a row agree.
    let mut items = read()?;
    let mut took = Duration::ZERO;
    for _ in 0..8 {
        sleep(1.5);
        let start = Instant::now();
        let again = read()?;
        took = start.elapsed();
        let same = again.len() == items.len();
        items = again;
        if same {
            break;
        }
    }
    let boxes: Vec<(usize, [i32; 4])> = items.iter().enumerate().map(|(n, item)| (n + 1, item.place)).collect();
    let (pixels, _) = capture(frames, &boxes)?;
    let path = debug_path(png)?;
    write_png(&path, frames.width, frames.height, &pixels)?;
    println!("\nScan of the second desktop: {} elements, read in {:.2} s, numbered on {}",
             items.len(), took.as_secs_f64(), path.display());
    for (n, item) in items.iter().enumerate() {
        println!("  [{}] {} {}", n + 1, item.kind, quote(&item.name.chars().take(50).collect::<String>()));
    }
    Ok(windows.iter().map(|&hwnd| window_info(hwnd as Handle)).collect())
}

// ---------------------------------------------------------------- shot and viewer

/// Your main desktop before anything started: its windows, the front one and the mouse.
struct Before {
    windows: Vec<(usize, WindowInfo)>,
    front: usize,
    front_title: String,
    cursor: (i32, i32),
}

unsafe fn take_shot(path: &str, wait: f64, frames: &Frames, job: Handle, before: &Before,
                    started: &[(String, u32)]) -> Res<()> {
    sleep(wait);
    let seen = frames.latest.lock().unwrap().count;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let latest = frames.latest.lock().unwrap();
        if latest.count != seen || latest.error.is_some() || Instant::now() >= deadline {
            break;
        }
        drop(latest);
        thread::sleep(Duration::from_millis(50)); // a picture taken after the wait
    }
    let (pixels, windows) = {
        let latest = frames.latest.lock().unwrap();
        match (&latest.error, &latest.pixels) {
            (None, Some(pixels)) => (pixels.clone(), latest.windows.clone()),
            (error, _) => return Err(format!("no picture: {}", error.as_deref().unwrap_or("timed out"))),
        }
    };
    let path = debug_path(path)?;
    write_png(&path, frames.width, frames.height, &pixels)?;
    println!("\nScreenshot of the second desktop: {} ({}x{})", path.display(), frames.width, frames.height);
    report(&windows, job, before, started);
    Ok(())
}

/// What runs on the second desktop, its RAM, and whether your desktop changed.
unsafe fn report(windows: &[WindowInfo], job: Handle, before: &Before, started: &[(String, u32)]) {
    println!("Programs started on it:");
    for (command, pid) in started {
        println!("  {:<24} {}", exe_name(command), process_state(*pid));
    }
    println!("Windows on it, top first:");
    for window in windows {
        println!("  {:<24} {:<34} {}", process_name(window.pid), window.class, window.title);
    }
    if windows.is_empty() {
        println!("  (none)");
    }
    let rows = job_memory(job);
    println!("RAM of the apps on it: {:.0} MB private", rows.iter().map(|(_, mb)| mb).sum::<f64>());
    for (name, mb) in &rows {
        println!("  {name:<24} {mb:7.1} MB");
    }
    report_main_desktop(before);
}

unsafe fn report_main_desktop(before: &Before) {
    let now = main_desktop_windows();
    let mut point = Point::default();
    GetCursorPos(&mut point);
    let new: Vec<&WindowInfo> = now
        .iter()
        .filter(|(hwnd, _)| !before.windows.iter().any(|(old, _)| old == hwnd))
        .map(|(_, info)| info)
        .collect();
    if new.is_empty() {
        println!("Your main desktop: no new windows.");
    } else {
        println!("New windows on your main desktop while it ran:");
        for window in new {
            println!("  {:<24} {:<34} {}", process_name(window.pid), window.class, window.title);
        }
    }
    let fg = GetForegroundWindow();
    let front = if fg as usize == before.front {
        "unchanged".to_string()
    } else {
        format!("changed from {} to {}", quote(&before.front_title), quote(&title_of(fg)))
    };
    let mouse = if (point.x, point.y) == before.cursor { "not moved" } else { "moved (by you?)" };
    println!("Your front window: {front}. Mouse: {mouse}.");
}

/// What the viewer window shows: the pictures, and the job whose RAM its title tells.
static VIEWER: OnceLock<(Arc<Frames>, usize)> = OnceLock::new();
static TICKS: AtomicU32 = AtomicU32::new(0);

unsafe fn paint(hwnd: Handle, frames: &Frames) {
    let mut ps: PaintStruct = zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut rc = Rect::default();
    GetClientRect(hwnd, &mut rc);
    let (cw, ch) = (rc.right, rc.bottom);
    let black = GetStockObject(BLACK_BRUSH);
    let pixels = frames.latest.lock().unwrap().pixels.clone();
    match pixels {
        Some(pixels) if cw > 0 && ch > 0 => {
            let scale = f64::min(cw as f64 / frames.width as f64, ch as f64 / frames.height as f64);
            let (dw, dh) = (((frames.width as f64 * scale) as i32).max(1), ((frames.height as f64 * scale) as i32).max(1));
            let (dx, dy) = ((cw - dw) / 2, (ch - dh) / 2);
            for (left, top, right, bottom) in
                [(0, 0, dx, ch), (dx + dw, 0, cw, ch), (dx, 0, dx + dw, dy), (dx, dy + dh, dx + dw, ch)]
            {
                FillRect(dc, &Rect { left, top, right, bottom }, black);
            }
            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, 0, 0, null_mut());
            let info = bitmap_info(frames.width, frames.height);
            StretchDIBits(dc, dx, dy, dw, dh, 0, 0, frames.width, frames.height, pixels.as_ptr() as *const c_void,
                          &info, DIB_RGB_COLORS, SRCCOPY);
        }
        _ => {
            FillRect(dc, &rc, black);
        }
    }
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn viewer_proc(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> isize {
    let Some((frames, job)) = VIEWER.get() else {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    };
    match msg {
        WM_PAINT => {
            paint(hwnd, frames);
            0
        }
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            InvalidateRect(hwnd, null(), 0);
            if TICKS.fetch_add(1, Ordering::Relaxed) % 8 == 0 {
                let mb: f64 = job_memory(*job as Handle).iter().map(|(_, mb)| mb).sum();
                let count = frames.latest.lock().unwrap().windows.len();
                let title = wide(&format!(
                    "Second Windows - {count} windows on it - its apps use {mb:.0} MB RAM - view only"
                ));
                SetWindowTextW(hwnd, title.as_ptr());
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// A window on your screen that shows the second desktop live, view only.
unsafe fn show_viewer(frames: &Arc<Frames>, job: Handle) -> Res<()> {
    let _ = VIEWER.set((frames.clone(), job as usize));
    let instance = GetModuleHandleW(null());
    let class = wide("AutoCuaSecondWindows");
    let wc = WndClassExW {
        size: size_of::<WndClassExW>() as u32,
        style: 0,
        wnd_proc: viewer_proc,
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: null_mut(),
        cursor: LoadCursorW(null_mut(), IDC_ARROW as *const u16),
        background: null_mut(),
        menu_name: null(),
        class_name: class.as_ptr(),
        icon_small: null_mut(),
    };
    if RegisterClassExW(&wc) == 0 {
        return Err(last_error("could not make the viewer window"));
    }
    let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    let w = (sw as f64 * 0.6) as i32;
    let h = (w as f64 * frames.height as f64 / frames.width.max(1) as f64) as i32 + 40;
    let title = wide("Second Windows - starting");
    let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW | WS_VISIBLE, (sw - w) / 2,
                               (sh - h) / 2, w, h, null_mut(), null_mut(), instance, null_mut());
    if hwnd.is_null() {
        return Err(last_error("could not make the viewer window"));
    }
    SetTimer(hwnd, 1, FRAME_INTERVAL.as_millis() as u32, null_mut());
    let mut msg: Msg = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

// ---------------------------------------------------------------- serving another program

/// One message to the program that reads this one's stdout: a line
/// "<kind> <bytes>", then that many bytes. False when nobody reads any more.
fn send(kind: &str, data: &[u8]) -> bool {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{kind} {}", data.len()).is_ok() && out.write_all(data).is_ok() && out.flush().is_ok()
}

/// The second desktop for another program, the page of test.py for one,
/// over this program's own pipes. Out on stdout, as messages: "frame", every
/// new picture as a BMP file, and "note", words about an order that failed.
/// In on stdin, one order a line: "run <command line>" starts a program on
/// the second desktop. It ends when stdin closes, so when the other program
/// ends, and every app on the second desktop ends with it.
unsafe fn serve(frames: &Arc<Frames>, job: Handle) -> Res<()> {
    let frames = frames.clone();
    thread::spawn(move || {
        let (mut seen, mut shown) = (0u64, None::<Arc<Vec<u8>>>);
        loop {
            let (count, pixels, error) = {
                let latest = frames.latest.lock().unwrap();
                (latest.count, latest.pixels.clone(), latest.error.clone())
            };
            if let Some(e) = error {
                send("note", format!("no picture: {e}").as_bytes());
                return;
            }
            if let (true, Some(pixels)) = (count != seen, pixels) {
                seen = count;
                // a desktop where nothing moved is not sent again
                if shown.as_ref().map_or(true, |old| **old != *pixels) {
                    if !send("frame", &bmp(frames.width, frames.height, &pixels)) {
                        return;
                    }
                    shown = Some(pixels);
                }
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { break };
        if let Some(command) = line.strip_prefix("run ") {
            if let Err(e) = start_on_desktop(command.trim(), DESKTOP, job, None) {
                send("note", format!("could not start on the second desktop: {command}: {e}").as_bytes());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- the device: the app's agent works here

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

/// Start a program on the second desktop for the agent, its window
/// maximized as the agent's open_app does, and wait up to 10 seconds for a
/// new app window there. As JSON: the pid, the new window's title (null when
/// none came), and whether the program ended with no window: a one-copy app
/// can hand its start to the copy on your desktop and quit.
unsafe fn device_open(command: &str, job: Handle) -> Res<String> {
    if command.is_empty() {
        return Err("open needs a command line".to_string());
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

/// The device, on a thread of its own that joins the second desktop before
/// anything else: window messages, PrintWindow and UI Automation reach that
/// desktop's windows only from there. The app (service.py beside this file)
/// sends one request a line on stdin, and gets exactly one reply for each, in
/// the order asked, as `send` writes them: a line "<kind> <bytes>", then the
/// bytes. First, unasked, "ready" with the picture's place on the screen.
///   ping           -> ok {}
///   shot           -> bmp: the picture of the second desktop (the primary
///                     monitor's work area, the taskbar strip cut off)
///   scan           -> ok {"windows": [...], "elements": [...]} (device_scan)
///   open CMDLINE   -> ok {"pid", "window", "exited"} (device_open)
/// What fails answers err, with why in words. Nothing else may go to stdout.
unsafe fn device_loop(desk: usize, frames: &Frames, job: usize, requests: Receiver<String>) -> Res<()> {
    if SetThreadDesktop(desk as Handle) == 0 {
        let error = last_error("could not join the second desktop");
        send("err", error.as_bytes());
        return Err(error);
    }
    let uia = uia_client()?;
    let every = uia.object(UIA_CREATE_TRUE_CONDITION)?;
    let cache = scan_cache(&uia)?;
    let ready = format!("{{\"x0\":{},\"y0\":{},\"width\":{},\"height\":{}}}",
                        frames.x0, frames.y0, frames.width, frames.height);
    if !send("ready", ready.as_bytes()) {
        return Ok(());
    }
    let mut seen = HashSet::new();
    for line in requests {
        let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let reply = match verb.trim() {
            "" => continue,
            "ping" => Ok(("ok", b"{}".to_vec())),
            "shot" => capture(frames, &[]).map(|(pixels, _)| ("bmp", bmp(frames.width, frames.height, &pixels))),
            "scan" => device_scan(&uia, &every, &cache, &mut seen).map(|json| ("ok", json.into_bytes())),
            "open" => device_open(rest.trim(), job as Handle).map(|json| ("ok", json.into_bytes())),
            other => Err(format!("unknown request: {other}")),
        };
        let sent = match reply {
            Ok((kind, data)) => send(kind, &data),
            Err(e) => send("err", e.as_bytes()),
        };
        if !sent {
            break; // nobody reads any more
        }
    }
    Ok(())
}

/// The device for the app's agent: device_loop on its thread, till stdin
/// closes. This thread reads stdin and hands each line over, so the end of
/// stdin (the app gone, however it went) is seen even while a request is
/// still being answered: then it waits two seconds at most, and returns.
/// run() then ends every app on the second desktop, a hung one included,
/// and the program ends with them.
unsafe fn device(frames: &Arc<Frames>, desk: usize, job: Handle) -> Res<()> {
    let (frames, job) = (frames.clone(), job as usize);
    let (hand_over, requests) = channel::<String>();
    let worker = thread::spawn(move || unsafe { device_loop(desk, &frames, job, requests) });
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { break };
        if hand_over.send(line).is_err() {
            break; // the device thread has ended
        }
    }
    drop(hand_over);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !worker.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    if !worker.is_finished() {
        return Ok(()); // still busy with a request nobody waits for any more
    }
    worker.join().unwrap_or_else(|_| Err("the device thread stopped".to_string()))
}

// ---------------------------------------------------------------- main

struct Args {
    shot: Option<String>,
    computer_use: bool,
    no_shell: bool,
    run: Vec<String>,
    scan: Option<String>,
    wait: f64,
    serve: bool,
    device: bool,
}

fn parse_args() -> Res<Args> {
    let mut args = Args {
        shot: None,
        computer_use: false,
        no_shell: false,
        run: Vec::new(),
        scan: None,
        wait: 6.0,
        serve: false,
        device: false,
    };
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        // "--name value" and "--name=value" both work
        let (name, mut inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_string(), Some(value.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = || inline.take().or_else(|| rest.next()).ok_or(format!("{name} needs a value"));
        match name.as_str() {
            "--shot" => args.shot = Some(value()?),
            "--scan" => args.scan = Some(value()?),
            "--run" => args.run.push(value()?),
            "--wait" => args.wait = value()?.parse().map_err(|_| "--wait needs a number of seconds".to_string())?,
            "--computer-use" => args.computer_use = true,
            "--no-shell" => args.no_shell = true,
            "--serve" => args.serve = true,
            "--device" => args.device = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    Ok(args)
}

/// Start the apps on the second desktop and do what was asked there.
unsafe fn go(args: &Args, frames: &Arc<Frames>, desk: Handle, job: Handle, before: &Before,
             capture_thread: &mut Option<thread::JoinHandle<()>>) -> Res<()> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let mut commands = if args.no_shell { Vec::new() } else { vec![format!("{windir}\\explorer.exe")] };
    if args.computer_use || (args.run.is_empty() && !args.serve && !args.device) {
        commands.push(format!("{windir}\\System32\\charmap.exe"));
    } else {
        commands.extend(args.run.iter().cloned());
    }
    let mut started = Vec::new();
    for command in commands {
        let said = match start_on_desktop(&command, DESKTOP, job, None) {
            Ok(pid) => {
                started.push((command.clone(), pid));
                format!("started on the second desktop: {command} (pid {pid})")
            }
            Err(e) => format!("could not start on the second desktop: {command}: {e}"),
        };
        if args.serve || args.device {
            eprintln!("{said}"); // stdout carries the pictures or the replies then
        } else {
            println!("{said}");
        }
    }

    let desk = desk as usize; // as a plain number it can go to another thread
    if args.device {
        return device(frames, desk, job);
    }
    if args.computer_use || args.scan.is_some() {
        if args.scan.is_some() {
            sleep(args.wait); // the apps get to start
        }
        // on a thread of its own, which moves onto the second desktop
        let (frames, scan) = (frames.clone(), args.scan.clone());
        let worker = thread::spawn(move || unsafe {
            match scan {
                Some(png) => scan_desktop(desk, &frames, &png).map(Some),
                None => computer_use(desk, &frames).map(|_| None),
            }
        });
        match worker.join() {
            Ok(Ok(Some(windows))) => report(&windows, job, before, &started),
            Ok(Ok(None)) => report_main_desktop(before),
            Ok(Err(e)) => {
                eprintln!("{e}");
                report_main_desktop(before);
            }
            Err(_) => report_main_desktop(before), // it panicked, and Rust printed why
        }
        return Ok(());
    }

    let for_thread = frames.clone();
    *capture_thread = Some(thread::spawn(move || unsafe { capture_loop(desk, &for_thread) }));
    if args.serve {
        return serve(frames, job);
    }
    match &args.shot {
        Some(path) => take_shot(path, args.wait, frames, job, before, &started),
        None => show_viewer(frames, job),
    }
}

unsafe fn run(args: &Args) -> i32 {
    SetProcessDpiAwarenessContext(DPI_PER_MONITOR_AWARE_V2); // real pixels, not scaled ones
    let mut work = Rect::default(); // the screen without the taskbar strip: every picture is cropped to it
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut Rect as *mut c_void, 0);
    let frames = Arc::new(Frames {
        x0: work.left,
        y0: work.top,
        width: work.right - work.left,
        height: work.bottom - work.top,
        stop: AtomicBool::new(false),
        latest: Mutex::new(Latest::default()),
    });

    let mut cursor = Point::default();
    GetCursorPos(&mut cursor);
    let front = GetForegroundWindow();
    let before = Before {
        windows: main_desktop_windows(),
        front: front as usize,
        front_title: title_of(front),
        cursor: (cursor.x, cursor.y),
    };
    let desk = match open_desktop(DESKTOP) {
        Ok(desk) => desk,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let job = match kill_on_close_job() {
        Ok(job) => job,
        Err(e) => {
            eprintln!("{e}");
            CloseDesktop(desk);
            return 1;
        }
    };

    let mut capture_thread = None;
    let result = go(args, &frames, desk, job, &before, &mut capture_thread);

    // However it went: stop the pictures, end every app on the second desktop, let it go.
    frames.stop.store(true, Ordering::SeqCst);
    if let Some(capture) = capture_thread {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !capture.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
    for extra in EXTRA_JOBS.lock().unwrap().drain(..) {
        TerminateJobObject(extra as Handle, 0);
        CloseHandle(extra as Handle);
    }
    TerminateJobObject(job, 0);
    CloseHandle(job);
    CloseDesktop(desk);
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    std::process::exit(unsafe { run(&args) });
}
