//! Second Mac: a second display for the agent, inside your own Mac session.
//!
//! macOS takes it for a real monitor: it has its own menu bar, wallpaper and
//! Space, and windows live on it. No screen shows it. It is seen in the
//! pictures this program takes and in the viewer window it opens.
//!
//! Shared with your main screen: the same user, files, internet, installed
//! apps and clipboard, and the ONE pointer and the ONE keyboard focus macOS
//! has for all its displays. Its own: its picture and the windows on it. So
//! nothing on it may come in front of your apps, and the agent's input may
//! not use your pointer or your keyboard.
//!
//! The display sits corner to corner with your main screen: the two touch at
//! one point only, so your pointer does not slide onto it by accident. (For
//! its first half second it is at the right edge of your screen.) macOS
//! takes it away when this program ends, however it ends.
//!
//! Apps: a NEW copy is started hidden, its windows are moved onto the display
//! through Accessibility, then it is shown. A file is handed to it after
//! that. Seen with Calculator, TextEdit, Dictionary and Font Book: no window
//! of theirs was on your screen at any moment and none was in front. A
//! watcher looks for both, and the reports of --shot, --scan and
//! --computer-use say what it saw.
//!   - A later window that is no document (About, Settings, a dialog) comes
//!     up on your screen first and is moved within about 0.1 seconds.
//!   - The file is handed over with an Apple event, which macOS allows per
//!     app (Automation). It is never asked for: where it is not allowed, the
//!     file goes with the start, and the app's window is on your screen for
//!     under 0.1 seconds first (seen with Preview).
//!   - Each started app has an icon in your Dock while it runs, and shares
//!     the saved settings and the list of recent documents of the copy you
//!     run yourself.
//!   - Apps built on Chromium (Chrome, Edge, Electron apps such as Cursor,
//!     web apps made by Chrome) are refused for now: a new copy would use
//!     the profile of your copy.
//!   - However this program ends, every app it started ends too: a guard
//!     process ends them when the program is killed (seen with kill -9).
//!     Work in them that was not saved is dropped.
//!
//! Input, with no pointer and no keyboard of yours: every event is handed
//! straight to the one app it is for, by process id, and only to apps this
//! program started. The app believes it is active. macOS does not: nothing
//! comes in front, and your typing stays with your own app. Seen arriving,
//! each read back from the app: clicks, a double and a triple click, a drag,
//! the wheel, typed text with accents and an emoji, Escape, Cmd+A and Cmd+Up.
//!   - No right click, and nothing that opens a menu is pressed: while a
//!     menu of an app that is not in front is open, macOS sends YOUR keyboard
//!     to that app (seen). A click at a pop-up button would still open one.
//!   - No Shift+click (tried four ways). Copy and paste would use your
//!     clipboard: neither was sent in a test.
//!   - Never sent: Option+Cmd+H, Cmd+Q with one more modifier and
//!     Option+Cmd+Escape. They would act on your whole session.
//!
//! The scan: what Accessibility finds to click or type in, in the windows of
//! the started apps, numbered on a picture. It can be pressed, and a text can
//! be set, through Accessibility alone. No other app is ever asked: its
//! window on the display is only listed, under the name macOS has for it.
//!
//! The agent's mouse: macOS has one real pointer and it stays with you, so
//! the agent's own is only drawn: into the pictures this program saves, and
//! over the picture in the viewer window. It is the arrow the browser agent
//! has on its pages (AutoCua/browser/connector/glow: its cursor only, none
//! of its glow), with a faint dark tint behind it, this file's own, so that
//! it stands out on a light page too. It is where the agent last clicked,
//! ended a drag or turned the wheel, and --mouse X,Y puts it at any point
//! of the display.
//!
//! Good to know, all seen on this Mac:
//!   - The display counts as an external monitor: on power with the lid
//!     closed, the Mac stays awake while this program runs.
//!   - While the viewer window or --serve runs, macOS shows its purple screen
//!     recording sign in your menu bar.
//!   - Two runs of the same program file at once get no pictures (seen).
//!     Two copies of the file run side by side (seen): a second run at the
//!     same time needs a copy of the file of its own.
//!   - The menu bar on the display shows the menus of one of your apps,
//!     never those of an app on it.
//!   - Not run with your screen locked: Accessibility gives nothing back then.
//!
//! RAM: the apps use what they use anywhere (seen: Calculator about 50 MB,
//! TextEdit about 54 MB). This program holds about 100 MB with its viewer
//! window open, its guard about 2 MB.
//!
//! Permissions, for the app this runs in (Terminal, VS Code, AutoCua), in
//! System Settings > Privacy & Security: Screen Recording for the pictures,
//! Accessibility for moving windows, for the scan and for reading back. It
//! never asks for one: it says in words what is missing.
//!
//! It stands on parts of macOS that Apple does not document or wants
//! replaced, so a later macOS can change them: CGVirtualDisplay, event field
//! 51 with CGEventSetWindowLocation, SLPSPostEventRecordTo, the old
//! NSWorkspace start calls and CGDisplayCreateImage. Each is named where it
//! is used. Checked on macOS 27.0.1, Apple Silicon.
//!
//! Standalone: this one file, the Rust standard library and macOS's own
//! frameworks. No crates, no cargo, no Python, no AutoCua code. macOS only.
//!
//! Build, from the root of the checkout:
//!     mkdir -p AutoCua/CUA_Sandbox/mac/build
//!     rustc --edition 2021 -O AutoCua/CUA_Sandbox/mac/Mac_CUA.rs -o AutoCua/CUA_Sandbox/mac/build/Mac_CUA
//!
//! Run:
//!     AutoCua/CUA_Sandbox/mac/build/Mac_CUA
//!         Make the second display, start Calculator on it, and show it live
//!         in a window on your screen. Closing the window ends everything
//!         that runs on it. NOT RUN YET with the window in front: so far it
//!         was only opened with --back.
//!     AutoCua/CUA_Sandbox/mac/build/Mac_CUA --shot shot.png
//!         The same, but save one screenshot of it, print a report and exit.
//!     AutoCua/CUA_Sandbox/mac/build/Mac_CUA --computer-use
//!         Computer use on it with no mouse and no keyboard of yours: scan
//!         Calculator and TextEdit with Accessibility, click, type, press
//!         keys, turn the wheel, scan again. Its pictures are cu_1_scan.png
//!         and cu_2_after.png. With --back the viewer window is open behind
//!         your windows while it runs.
//!     AutoCua/CUA_Sandbox/mac/build/Mac_CUA --run APP --scan scan.png
//!         Start APP on it and save the agent's scan: what Accessibility
//!         finds to click or type in, numbered on a picture, and print the
//!         list.
//!     AutoCua/CUA_Sandbox/mac/build/Mac_CUA --serve
//!         For another program. No app and no window: every new picture of
//!         it goes out on stdout, and a line "run APP" on stdin starts APP
//!         on it. It ends when stdin closes. The messages are those of
//!         Win_CUA.rs.
//!
//!     --run APP     start APP on it instead of Calculator: its name or the
//!                   path of its .app (can repeat)
//!     --wait SEC    seconds the apps get before --shot or --scan (default 3)
//!     --size WxH    its size in points (default: the size of your main screen)
//!     --retina      two pixels a point, as on a Retina screen
//!     --display ID  use a display that is on already instead of making one
//!     --mouse X,Y   put the agent's mouse at that point of it, in points from
//!                   its top left: it is drawn into the saved pictures and in
//!                   the live window
//!     --back        open the viewer window behind your other windows: it
//!                   then takes nothing from the app in front (seen)
//!
//! Every picture given as a file name only goes, for now, to the debug folder
//! at the root of the checkout: --shot shot.png saves debug/shot.png there.

#![allow(non_snake_case)] // the file keeps the name of the Windows one beside it

use std::collections::HashSet;
use std::ffi::{c_char, c_void, CStr, CString};
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const NAME: &str = "AutoCua second display"; // its name in System Settings > Displays
const FRAME_INTERVAL: Duration = Duration::from_millis(250); // between two pictures for the viewer
/// The word this program is started with a second time, to be the guard of its apps.
const GUARD_WORD: &str = "--guard";

const USAGE: &str = "Second Mac: a second display for the agent.

Mac_CUA [--shot PNG] [--computer-use] [--run APP] [--scan PNG] [--wait SEC] [--serve]
        [--size WxH] [--retina] [--display ID] [--mouse X,Y] [--back]

  --shot PNG       save one screenshot and exit
  --computer-use   scan and act on Calculator and TextEdit there
  --run APP        app to start on it, by its name or the path of its .app (can repeat)
  --scan PNG       save the agent's scan of it and exit
  --wait SEC       seconds before --shot or --scan (default 3)
  --serve          for another program: pictures out on stdout, \"run APP\" lines in on stdin
  --size WxH       its size in points (default: the size of your main screen)
  --retina         two pixels a point
  --display ID     use a display that is on already instead of making one
  --mouse X,Y      put the agent's mouse at that point of it (points from its top left): drawn into the pictures
  --back           open the live window behind your other windows (with --computer-use: watch it there)

A picture given as a file name only is saved in the debug folder at the root.";

/// Any Objective-C or Core Foundation object.
type Id = *mut c_void;
/// What a step gives back: its result, or what went wrong, in words.
type Res<T> = Result<T, String>;

const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
const FOR_THIS_PROGRAM: u32 = 0; // kCGConfigureForAppOnly: the arrangement ends with the program
// NSWorkspaceLaunchWithoutAddingToRecents, WithoutActivation, NewInstance and AndHide
const START_HIDDEN: usize = 0x100 | 0x200 | 0x8_0000 | 0x10_0000;
const ON_SCREEN_ONLY: u32 = 1; // kCGWindowListOptionOnScreenOnly
const NO_DESKTOP: u32 = 16; // kCGWindowListExcludeDesktopElements
const SKYLIGHT: &str = "/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight";
const RTLD_LAZY: i32 = 1;
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void; // "look in everything that is loaded"

// Signals as signal.h numbers them, and SIG_IGN.
const SIGHUP: i32 = 1;
const SIGINT: i32 = 2; // Ctrl+C in the terminal
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15; // "kill"
const IGNORE: usize = 1;
const WRITE_ONLY: i32 = 1; // O_WRONLY

// What the kernel says about a process, as libproc.h and sys/proc.h number it.
const PROCESS_INFO: i32 = 3; // PROC_PIDTBSDINFO
const USE_V2: i32 = 2; // RUSAGE_INFO_V2
const ZOMBIE: u32 = 5; // SZOMB: ended, and not yet collected by its parent

// The Apple event "open these documents", as AppleEvents.h spells it.
const CORE_EVENT: u32 = 0x6165_7674; // 'aevt', kCoreEventClass
const OPEN_DOCUMENTS: u32 = 0x6F64_6F63; // 'odoc', kAEOpenDocuments
const DIRECT_OBJECT: u32 = 0x2D2D_2D2D; // '----', keyDirectObject
const SEND_AND_GO_ON: usize = 0x1 | 0x10 | 0x1000; // NSAppleEventSendNoReply, NeverInteract and DontRecord
const NO_SUCH_PROCESS: i32 = -600; // procNotFound

// Event kinds, fields and flags, as CGEventTypes.h numbers them.
const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const LEFT_DRAGGED: u32 = 6;
const LEFT_BUTTON: u32 = 0;
const PRIVATE_STATE: i32 = -1; // kCGEventSourceStatePrivate: none of the keys you hold get into its events
const WHEEL_LINES: u32 = 1; // kCGScrollEventUnitLine
const FIELD_CLICKS: u32 = 1; // kCGMouseEventClickState
const FIELD_BUTTON: u32 = 3; // kCGMouseEventButtonNumber
const FIELD_WINDOW: u32 = 51; // not in Apple's headers: the window the event is for
const SHIFT: u64 = 0x2_0000; // kCGEventFlagMaskShift
const CONTROL: u64 = 0x4_0000; // kCGEventFlagMaskControl
const OPTION: u64 = 0x8_0000; // kCGEventFlagMaskAlternate
const COMMAND: u64 = 0x10_0000; // kCGEventFlagMaskCommand
const KEYS_OWN_FLAGS: u64 = 0x20_0000 | 0x80_0000; // number pad and fn: what an arrow key carries by itself

// Virtual key codes (Events.h in HIToolbox) of keys that are the same in every keyboard
// layout. The key of a letter is looked up, see `key_for`.
const TEXT_KEY: u16 = 0; // the key typed text rides on: what counts is the text it carries
const KEY_RETURN: u16 = 36;
const KEY_TAB: u16 = 48;
const KEY_ESCAPE: u16 = 53;
const KEY_UP: u16 = 126;
// Where H and Q are on every ANSI and ISO keyboard, whatever letter the layout gives the key.
const KEY_H: u16 = 4;
const KEY_Q: u16 = 12;

// The bits of AXMenuItemCmdModifiers, which has Cmd in every shortcut unless bit 8 says no.
const MENU_SHIFT: f64 = 1.0;
const MENU_OPTION: f64 = 2.0;
const MENU_CONTROL: f64 = 4.0;
const MENU_NO_COMMAND: f64 = 8.0;

// Accessibility's value kinds (AXValueType), CFNumber's (CFNumberType), and two of its errors.
const AX_POINT: u32 = 1;
const AX_SIZE: u32 = 2;
const AX_RECT: u32 = 3;
const AX_RANGE: u32 = 4;
const NUMBER_I64: isize = 4;
const NUMBER_F64: isize = 6;
const NOT_ANSWERING: i32 = -25204; // kAXErrorCannotComplete: the app did not answer in time
const NO_SUCH_ACTION: i32 = -25206; // kAXErrorActionUnsupported

// How a CGImage keeps its pixels, as CGImage.h numbers it.
const ALPHA_MASK: u32 = 0x1F; // kCGBitmapAlphaInfoMask
const ALPHA_FIRST_PREMULTIPLIED: u32 = 2; // kCGImageAlphaPremultipliedFirst
const ALPHA_FIRST: u32 = 4; // kCGImageAlphaFirst
const ALPHA_SKIPPED_FIRST: u32 = 6; // kCGImageAlphaNoneSkipFirst
const FLOAT_COMPONENTS: u32 = 0x100; // kCGBitmapFloatComponents
const ORDER_MASK: u32 = 0x7000; // kCGBitmapByteOrderMask
const ORDER_32_LITTLE: u32 = 0x2000; // kCGBitmapByteOrder32Little
/// In memory: blue, green, red, alpha, the colours already multiplied by alpha.
const BGRA: u32 = ORDER_32_LITTLE | ALPHA_FIRST_PREMULTIPLIED;
/// The same four bytes a pixel, the fourth meaning nothing: a picture with no see-through parts.
const NO_SEE_THROUGH: u32 = ORDER_32_LITTLE | ALPHA_SKIPPED_FIRST;

// AppKit's numbers, as its headers have them.
const POLICY_REGULAR: isize = 0; // NSApplicationActivationPolicyRegular: a Dock icon and a menu bar
const POLICY_ACCESSORY: isize = 1; // NSApplicationActivationPolicyAccessory: neither of the two
const WINDOW_STYLE: usize = 1 | 2 | 4 | 8; // titled, closable, miniaturizable, resizable
const BUFFERED: usize = 2; // NSBackingStoreBuffered
const TERMINATE_CANCEL: usize = 0; // NSTerminateCancel
const EVENT_APPLICATION_DEFINED: usize = 15; // NSEventTypeApplicationDefined

// macOS's own structs, field for field as its headers lay them out.

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Size {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    origin: Point,
    size: Size,
}

/// CFRange: a run of a text, counted in UTF-16 units as Accessibility does.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Range {
    start: isize,
    length: isize,
}

/// ProcessSerialNumber: the older name macOS has for a running app. The
/// event records are addressed with it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Serial {
    high: u32,
    low: u32,
}

/// proc_bsdinfo: what the kernel keeps about one process.
#[repr(C)]
#[allow(dead_code)]
struct ProcessInfo {
    flags: u32,
    status: u32,
    exit_status: u32,
    pid: u32,
    parent: u32,
    user_and_group_ids: [u32; 6],
    reserved: u32,
    short_name: [u8; 16],
    name: [u8; 32],
    open_files: u32,
    group: u32,
    job_count: u32,
    terminal: u32,
    terminal_group: u32,
    nice: i32,
    born_seconds: u64,
    born_microseconds: u64,
}

/// rusage_info_v2: what a process has used so far. Only its RAM is read.
#[repr(C)]
#[allow(dead_code)]
struct ResourceUse {
    uuid: [u8; 16],
    user_time: u64,
    system_time: u64,
    idle_wakeups: u64,
    interrupt_wakeups: u64,
    pageins: u64,
    wired_size: u64,
    resident_size: u64,
    footprint: u64, // ri_phys_footprint
    start_time: u64,
    exit_time: u64,
    of_its_children: [u64; 6],
    disk_read: u64,
    disk_written: u64,
}

/// CFArrayCallBacks and CFDictionary's two: tables of macOS that are only ever pointed at.
#[repr(C)]
struct Table {
    _private: [u8; 0],
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Id;
    /// Never called as it stands: `msg!` gives it the type of the method first.
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra_bytes: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    /// `method` is one of the viewer's methods, as the one type the runtime takes them all in.
    fn class_addMethod(class: Id, name: Id, method: unsafe extern "C" fn(), types: *const c_char) -> bool;
}

// NSArray, NSWorkspace and NSWindow live in these two: named here so they are loaded.
#[link(name = "Foundation", kind = "framework")]
extern "C" {}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

// CALayer and CATransaction live here.
#[link(name = "QuartzCore", kind = "framework")]
extern "C" {
    #[link_name = "kCAGravityResizeAspect"]
    static KEEP_SHAPE: Id;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, encoding: u32,
                               external: u8) -> Id;
    fn CFURLCreateFromFileSystemRepresentation(alloc: *const c_void, path: *const u8, len: isize,
                                               is_folder: u8) -> Id;
    fn CFRelease(object: *const c_void);
    fn CFRetain(object: *const c_void) -> Id;
    fn CFGetTypeID(object: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayCreate(alloc: *const c_void, values: *const *const c_void, count: isize, callbacks: *const Table) -> Id;
    fn CFArrayGetCount(array: Id) -> isize;
    fn CFArrayGetValueAtIndex(array: Id, index: isize) -> Id;
    fn CFDictionaryCreate(alloc: *const c_void, keys: *const *const c_void, values: *const *const c_void,
                          count: isize, key_callbacks: *const Table, value_callbacks: *const Table) -> Id;
    fn CFDictionaryGetValue(dictionary: Id, key: *const c_void) -> Id;
    fn CFNumberGetValue(number: Id, kind: isize, value: *mut c_void) -> u8;
    fn CFBooleanGetValue(boolean: Id) -> u8;
    fn CFDataCreate(alloc: *const c_void, bytes: *const u8, len: isize) -> Id;
    fn CFDataGetLength(data: Id) -> isize;
    fn CFDataGetBytePtr(data: Id) -> *const u8;
    fn CFAttributedStringCreate(alloc: *const c_void, text: Id, attributes: Id) -> Id;
    fn CFRunLoopRunInMode(mode: Id, seconds: f64, return_after_one: u8) -> i32;
    #[link_name = "kCFRunLoopDefaultMode"]
    static RUN_LOOP_DEFAULT_MODE: Id;
    #[link_name = "kCFRunLoopCommonModes"]
    static RUN_LOOP_COMMON_MODES: Id;
    #[link_name = "kCFBooleanTrue"]
    static TRUE: Id;
    #[link_name = "kCFTypeArrayCallBacks"]
    static ARRAY_OF_OBJECTS: Table;
    #[link_name = "kCFTypeDictionaryKeyCallBacks"]
    static KEYS_ARE_OBJECTS: Table;
    #[link_name = "kCFTypeDictionaryValueCallBacks"]
    static VALUES_ARE_OBJECTS: Table;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGGetOnlineDisplayList(max: u32, ids: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> Rect;
    fn CGBeginDisplayConfiguration(config: *mut Id) -> i32;
    fn CGConfigureDisplayOrigin(config: Id, display: u32, x: i32, y: i32) -> i32;
    fn CGCompleteDisplayConfiguration(config: Id, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: Id) -> i32;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGSessionCopyCurrentDictionary() -> Id;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> Id;
    fn CGRectMakeWithDictionaryRepresentation(dictionary: Id, rect: *mut Rect) -> bool;
    #[link_name = "kCGWindowNumber"]
    static WINDOW_NUMBER: Id;
    #[link_name = "kCGWindowOwnerPID"]
    static WINDOW_OWNER_PID: Id;
    #[link_name = "kCGWindowOwnerName"]
    static WINDOW_OWNER_NAME: Id;
    #[link_name = "kCGWindowName"]
    static WINDOW_NAME: Id;
    #[link_name = "kCGWindowLayer"]
    static WINDOW_LAYER: Id;
    #[link_name = "kCGWindowBounds"]
    static WINDOW_BOUNDS: Id;

    fn CGDisplayCreateImage(display: u32) -> Id;
    fn CGImageGetWidth(image: Id) -> usize;
    fn CGImageGetHeight(image: Id) -> usize;
    fn CGImageGetBitsPerComponent(image: Id) -> usize;
    fn CGImageGetBitsPerPixel(image: Id) -> usize;
    fn CGImageGetBytesPerRow(image: Id) -> usize;
    fn CGImageGetBitmapInfo(image: Id) -> u32;
    fn CGImageGetColorSpace(image: Id) -> Id;
    fn CGImageGetDataProvider(image: Id) -> Id;
    fn CGImageCreate(width: usize, height: usize, bits_per_component: usize, bits_per_pixel: usize,
                     bytes_per_row: usize, space: Id, bitmap_info: u32, provider: Id, decode: *const f64,
                     interpolate: bool, intent: i32) -> Id;
    fn CGImageRelease(image: Id);
    fn CGDataProviderCopyData(provider: Id) -> Id;
    fn CGDataProviderCreateWithCFData(data: Id) -> Id;
    fn CGColorSpaceCreateDeviceRGB() -> Id;
    fn CGBitmapContextCreate(data: *mut c_void, width: usize, height: usize, bits_per_component: usize,
                             bytes_per_row: usize, space: Id, bitmap_info: u32) -> Id;
    fn CGBitmapContextCreateImage(context: Id) -> Id;
    fn CGContextDrawImage(context: Id, rect: Rect, image: Id);
    fn CGContextSetRGBStrokeColor(context: Id, red: f64, green: f64, blue: f64, alpha: f64);
    fn CGContextSetRGBFillColor(context: Id, red: f64, green: f64, blue: f64, alpha: f64);
    fn CGContextStrokeRectWithWidth(context: Id, rect: Rect, width: f64);
    fn CGContextSetTextPosition(context: Id, x: f64, y: f64);

    // What draws the agent's mouse. Each `transform` is a CGAffineTransform *, and null for none.
    fn CGPathCreateMutable() -> Id;
    fn CGPathMoveToPoint(path: Id, transform: *const c_void, x: f64, y: f64);
    fn CGPathAddLineToPoint(path: Id, transform: *const c_void, x: f64, y: f64);
    fn CGPathAddCurveToPoint(path: Id, transform: *const c_void, pull_1_x: f64, pull_1_y: f64, pull_2_x: f64,
                             pull_2_y: f64, x: f64, y: f64);
    fn CGPathCloseSubpath(path: Id);
    fn CGPathGetPathBoundingBox(path: Id) -> Rect;
    fn CGContextSaveGState(context: Id);
    fn CGContextRestoreGState(context: Id);
    fn CGContextTranslateCTM(context: Id, x: f64, y: f64);
    fn CGContextScaleCTM(context: Id, x: f64, y: f64);
    fn CGContextAddPath(context: Id, path: Id);
    fn CGContextSetLineWidth(context: Id, width: f64);
    fn CGContextSetLineJoin(context: Id, join: i32);
    fn CGContextSetLineCap(context: Id, cap: i32);
    fn CGContextReplacePathWithStrokedPath(context: Id);
    fn CGContextClip(context: Id);
    fn CGContextDrawPath(context: Id, mode: i32);
    fn CGColorCreate(space: Id, components: *const f64) -> Id;
    fn CGContextSetShadowWithColor(context: Id, offset: Size, blur: f64, color: Id);
    fn CGContextBeginTransparencyLayer(context: Id, info: Id);
    fn CGContextEndTransparencyLayer(context: Id);
    fn CGGradientCreateWithColorComponents(space: Id, components: *const f64, places: *const f64, count: usize) -> Id;
    fn CGContextDrawLinearGradient(context: Id, gradient: Id, start: Point, end: Point, options: u32);

    fn CGEventSourceCreate(state: i32) -> Id;
    fn CGEventCreate(source: Id) -> Id;
    fn CGEventGetLocation(event: Id) -> Point;
    fn CGEventCreateMouseEvent(source: Id, kind: u32, at: Point, button: u32) -> Id;
    fn CGEventCreateKeyboardEvent(source: Id, key: u16, down: bool) -> Id;
    /// The wheel event with a fixed list of values. The one without the 2 is variadic.
    fn CGEventCreateScrollWheelEvent2(source: Id, units: u32, wheels: u32, wheel1: i32, wheel2: i32,
                                      wheel3: i32) -> Id;
    fn CGEventSetIntegerValueField(event: Id, field: u32, value: i64);
    fn CGEventSetFlags(event: Id, flags: u64);
    fn CGEventGetFlags(event: Id) -> u64;
    fn CGEventSetLocation(event: Id, at: Point);
    fn CGEventKeyboardSetUnicodeString(event: Id, length: usize, units: *const u16);
    /// The letters a key event carries: void (CGEventRef, UniCharCount max, UniCharCount *length, UniChar *).
    fn CGEventKeyboardGetUnicodeString(event: Id, max: usize, length: *mut usize, units: *mut u16);
    fn CGEventPostToPid(pid: i32, event: Id);
}

// CoreText draws the numbers on the scan's picture.
#[link(name = "CoreText", kind = "framework")]
extern "C" {
    fn CTFontCreateWithName(name: Id, size: f64, matrix: *const c_void) -> Id;
    fn CTFontGetAscent(font: Id) -> f64;
    fn CTLineCreateWithAttributedString(text: Id) -> Id;
    fn CTLineDraw(line: Id, context: Id);
    #[link_name = "kCTFontAttributeName"]
    static FONT_OF_TEXT: Id;
    #[link_name = "kCTForegroundColorFromContextAttributeName"]
    static COLOR_OF_TEXT_FROM_CANVAS: Id;
}

// Accessibility: how the agent reads an app, and its other way to act on one.
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> Id;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementCopyAttributeValue(element: Id, attribute: Id, value: *mut Id) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(element: Id, attributes: Id, options: u32, values: *mut Id) -> i32;
    fn AXUIElementCopyParameterizedAttributeValue(element: Id, attribute: Id, parameter: Id,
                                                  value: *mut Id) -> i32;
    fn AXUIElementSetAttributeValue(element: Id, attribute: Id, value: Id) -> i32;
    fn AXUIElementIsAttributeSettable(element: Id, attribute: Id, settable: *mut u8) -> i32;
    fn AXUIElementCopyActionNames(element: Id, names: *mut Id) -> i32;
    fn AXUIElementPerformAction(element: Id, action: Id) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Id, seconds: f32) -> i32;
    fn AXValueCreate(kind: u32, value: *const c_void) -> Id;
    fn AXValueGetValue(value: Id, kind: u32, out: *mut c_void) -> u8;
    fn AXValueGetTypeID() -> usize;
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    /// Whether this program may send an Apple event to a process. With 0 as
    /// the last value it only answers and never puts a question to you.
    fn AEDeterminePermissionToAutomateTarget(target: *const c_void, event_class: u32, event_id: u32, ask: u8) -> i32;
}

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    fn CGImageDestinationCreateWithURL(url: Id, kind: Id, count: usize, options: Id) -> Id;
    fn CGImageDestinationAddImage(destination: Id, image: Id, properties: Id);
    fn CGImageDestinationFinalize(destination: Id) -> bool;
}

// libSystem, which every Mac program has loaded: libdispatch, dyld, libproc and signals.
extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> Id;
    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn proc_listallpids(buffer: *mut c_void, size: i32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, size: i32) -> i32;
    /// Its buffer is a rusage_info_t *: the address of one of the rusage_info structs.
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut *mut c_void) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    /// The handler is a function that takes the signal's number, or SIG_IGN, which is the number 1.
    /// Gives back the handler the signal had before.
    fn signal(signal: i32, handler: usize) -> usize;
    /// int open(const char *, int, ...): the third is only read when a file is made.
    fn open(path: *const c_char, flags: i32, ...) -> i32;
    fn dup2(from: i32, to: i32) -> i32;
    fn close(file: i32) -> i32;
}

// The functions of macOS that are not in Apple's headers, each with its exact
// type. They are looked up by name while running, never linked.
/// void CGEventSetWindowLocation(CGEventRef, CGPoint): where in its window an event is.
type SetWindowLocation = unsafe extern "C" fn(Id, Point);
/// OSStatus SLPSPostEventRecordTo(const ProcessSerialNumber *, const uint8_t *record)
type PostRecord = unsafe extern "C" fn(*const Serial, *const u8) -> i32;
/// OSStatus GetProcessForPID(pid_t, ProcessSerialNumber *)
type SerialOfPid = unsafe extern "C" fn(i32, *mut Serial) -> i32;
/// OSStatus GetProcessPID(const ProcessSerialNumber *, pid_t *)
type PidOfSerial = unsafe extern "C" fn(*const Serial, *mut i32) -> i32;
/// OSStatus SLPSGetKeyFocusProcess(ProcessSerialNumber *, Boolean *). The second is one byte
/// that it always writes (read in its machine code on macOS 27.0.1), so it is never given
/// null. It gets four bytes of room here.
type KeyFocusProcess = unsafe extern "C" fn(*mut Serial, *mut u32) -> i32;
/// OSStatus _SLPSGetFrontProcess(ProcessSerialNumber *)
type FrontProcess = unsafe extern "C" fn(*mut Serial) -> i32;
/// pid_t responsibility_get_pid_responsible_for_pid(pid_t): the app macOS holds responsible for a process.
type ResponsibleFor = unsafe extern "C" fn(i32) -> i32;
/// AXError _AXUIElementGetWindow(AXUIElementRef, CGWindowID *): the window an element is in,
/// under the id macOS's window list has for it.
type WindowOfElement = unsafe extern "C" fn(Id, *mut u32) -> i32;

// ---------------------------------------------------------------- small helpers

/// Send an Objective-C message: `msg!(what it gives back; object, "name:", value => its type)`.
/// objc_msgSend has to be called as a function of exactly the method's own
/// type, so each use gives it that type first.
macro_rules! msg {
    ($ret:ty; $object:expr, $name:expr $(, $value:expr => $kind:ty)*) => {{
        let call: unsafe extern "C" fn(Id, Id $(, $kind)*) -> $ret =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        call($object, selector($name) $(, $value)*)
    }};
}

/// The name of a method, as Objective-C knows it.
unsafe fn selector(name: &str) -> Id {
    let name = CString::new(name).unwrap_or_default();
    sel_registerName(name.as_ptr())
}

/// A class by its name. Null when this macOS has none of that name.
unsafe fn class(name: &str) -> Id {
    let name = CString::new(name).unwrap_or_default();
    objc_getClass(name.as_ptr())
}

/// A new object of a class, made with alloc and init. The caller releases it.
unsafe fn new_object(of: &str) -> Id {
    msg!(Id; msg!(Id; class(of), "alloc"), "init")
}

/// A Rust string as a CFString, which is an NSString too. The caller releases it.
unsafe fn cf_string(text: &str) -> Id {
    CFStringCreateWithBytes(null(), text.as_ptr(), text.len() as isize, UTF8, 0)
}

/// An NSString as a Rust string.
unsafe fn rust_string(text: Id) -> String {
    if text.is_null() {
        return String::new();
    }
    let bytes = msg!(*const c_char; text, "UTF8String");
    if bytes.is_null() { String::new() } else { CStr::from_ptr(bytes).to_string_lossy().into_owned() }
}

/// Let macOS deliver what waits for this thread, for that many seconds.
unsafe fn pump(seconds: f64) {
    CFRunLoopRunInMode(RUN_LOOP_DEFAULT_MODE, seconds.max(0.0).min(86400.0), 0);
}

/// Text in quotes, for the reports.
fn quote(text: &str) -> String {
    format!("'{text}'")
}

fn sleep(seconds: f64) {
    thread::sleep(Duration::from_secs_f64(seconds.max(0.0).min(86400.0)));
}

/// A Core Foundation object this program owns: given back to macOS when dropped.
struct Cf(Id);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

/// A private function of macOS by its name. None when this macOS has none
/// of that name. SkyLight, the framework under CoreGraphics, is loaded first.
unsafe fn private_function(name: &str) -> Option<*mut c_void> {
    let path = CString::new(SKYLIGHT).unwrap_or_default();
    dlopen(path.as_ptr(), RTLD_LAZY);
    let name = CString::new(name).unwrap_or_default();
    let found = dlsym(RTLD_DEFAULT, name.as_ptr());
    if found.is_null() { None } else { Some(found) }
}

/// The same, as an error in words when it is missing.
unsafe fn needed_function(name: &str, what_for: &str) -> Res<*mut c_void> {
    private_function(name).ok_or(format!("this macOS has no {name}: {what_for}"))
}

/// Ctrl+C in the terminal, "kill", or the terminal's window closed: this program then ends
/// its apps and lets the display go.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// What Ctrl+C, "kill" and a closed terminal window run in this program: it raises the flag.
/// After a closed terminal window (SIGHUP) nothing takes what the program prints, and a
/// print that fails breaks the thread that prints. Seen with --computer-use --back: the
/// program ended with two panics, and only the guard ended its apps. So from then on
/// what it prints goes nowhere.
unsafe extern "C" fn interrupted(signal: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
    if signal == SIGHUP {
        let nowhere = open(b"/dev/null\0".as_ptr() as *const c_char, WRITE_ONLY);
        if nowhere >= 0 {
            dup2(nowhere, 1);
            dup2(nowhere, 2);
            close(nowhere);
        }
    }
}

/// What is said when the flag ended a run. Such a run ends with code 1 in every mode.
const STOPPED: &str = "stopped by Ctrl+C or kill";

/// The live window of --computer-use --back was closed while the agent acted.
static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);

/// Why the agent has to stop before its next step, when it has to.
fn stop_asked() -> Option<&'static str> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Some("Ctrl+C or kill");
    }
    if WINDOW_CLOSED.load(Ordering::SeqCst) { Some("the live window was closed") } else { None }
}

/// Where a picture is saved. A name with a folder is used as it is. A file
/// name only goes, for now, to the debug folder at the root of the checkout:
/// the first folder above this program that holds AutoCua/CUA_Sandbox. When
/// the program is not inside a checkout, to a debug folder beside it.
fn picture_path(name: &str) -> Res<PathBuf> {
    let given = Path::new(name);
    if given.file_name().is_none() {
        return Err(format!("{} is not a file name for a picture", quote(name)));
    }
    if given.components().count() > 1 {
        return Ok(given.to_path_buf());
    }
    let exe = std::env::current_exe().map_err(|e| format!("could not find this program's folder: {e}"))?;
    let beside = exe.parent().unwrap_or(Path::new("."));
    let root = beside.ancestors().find(|folder| folder.join("AutoCua").join("CUA_Sandbox").is_dir());
    let debug = root.unwrap_or(beside).join("debug");
    std::fs::create_dir_all(&debug).map_err(|e| format!("could not make {}: {e}", debug.display()))?;
    Ok(debug.join(given))
}

// ---------------------------------------------------------------- the display

/// The second display. macOS keeps it while `object` lives, and takes it away
/// when this program ends, however it ends (Ctrl+C, a crash).
struct Display {
    object: Id,
    id: u32,
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe { msg!((); self.object, "release") }
    }
}

/// The ids of the displays macOS has now, real and not.
unsafe fn displays() -> Vec<u32> {
    let mut ids = [0u32; 32];
    let mut count = 0u32;
    CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count);
    ids[..(count as usize).min(ids.len())].to_vec()
}

/// Make the second display: `width` by `height` points, and with `retina`
/// two pixels to a point. It is there for the rest of macOS a moment later,
/// to the right of the main screen until `place_at_corner` moves it.
unsafe fn make_display(width: u32, height: u32, retina: bool) -> Res<Display> {
    for needed in ["CGVirtualDisplay", "CGVirtualDisplayDescriptor", "CGVirtualDisplaySettings",
                   "CGVirtualDisplayMode"] {
        if class(needed).is_null() {
            return Err(format!("this macOS has no {needed}: it cannot make a second display this way"));
        }
    }
    let scale = if retina { 2 } else { 1 };

    // What the monitor says about itself, as a real one does over its cable.
    let about = new_object("CGVirtualDisplayDescriptor");
    let name = cf_string(NAME);
    msg!((); about, "setName:", name => Id);
    CFRelease(name);
    msg!((); about, "setMaxPixelsWide:", width * scale => u32);
    msg!((); about, "setMaxPixelsHigh:", height * scale => u32);
    // its size on a desk, at 110 points an inch like an ordinary monitor
    let millimeters = Size { width: width as f64 * 25.4 / 110.0, height: height as f64 * 25.4 / 110.0 };
    msg!((); about, "setSizeInMillimeters:", millimeters => Size);
    msg!((); about, "setVendorID:", 0xAC0Au32 => u32);
    msg!((); about, "setProductID:", 0xC0A1u32 => u32);
    msg!((); about, "setSerialNum:", std::process::id() => u32); // two programs make two monitors
    let queue = dispatch_queue_create(b"autocua.second.display\0".as_ptr() as *const c_char, null());
    msg!((); about, "setQueue:", queue => Id);

    let object = msg!(Id; msg!(Id; class("CGVirtualDisplay"), "alloc"), "initWithDescriptor:", about => Id);
    msg!((); about, "release");
    if object.is_null() {
        return Err("macOS did not make the second display".to_string());
    }
    let display = Display { object, id: msg!(u32; object, "displayID") };

    // The one picture size it offers, 60 times a second.
    let mode = msg!(Id; msg!(Id; class("CGVirtualDisplayMode"), "alloc"), "initWithWidth:height:refreshRate:",
                    width => u32, height => u32, 60.0 => f64);
    let settings = new_object("CGVirtualDisplaySettings");
    msg!((); settings, "setHiDPI:", retina as u32 => u32);
    msg!((); settings, "setModes:", msg!(Id; class("NSArray"), "arrayWithObject:", mode => Id) => Id);
    let applied = msg!(bool; object, "applySettings:", settings => Id);
    msg!((); settings, "release");
    msg!((); mode, "release");
    if !applied {
        return Err(format!("macOS did not take {width}x{height} for the second display"));
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while !displays().contains(&display.id) {
        if Instant::now() >= deadline {
            return Err("the second display did not come on".to_string());
        }
        pump(0.05);
    }
    Ok(display)
}

/// Put the second display's top left on the main screen's bottom right. The
/// two then touch at one point only, and a pointer that runs into the edge
/// of the main screen stays on the main screen.
unsafe fn place_at_corner(display: u32) -> Res<()> {
    let main = CGDisplayBounds(CGMainDisplayID());
    let (x, y) = ((main.origin.x + main.size.width) as i32, (main.origin.y + main.size.height) as i32);
    let mut config: Id = null_mut();
    if CGBeginDisplayConfiguration(&mut config) != 0 {
        return Err("macOS did not let the second display be moved".to_string());
    }
    if CGConfigureDisplayOrigin(config, display, x, y) != 0 {
        CGCancelDisplayConfiguration(config);
        return Err("macOS did not let the second display be moved".to_string());
    }
    if CGCompleteDisplayConfiguration(config, FOR_THIS_PROGRAM) != 0 {
        return Err("macOS did not move the second display".to_string());
    }
    pump(0.3); // the move reaches the rest of macOS
    // macOS may put a display somewhere else than it was asked to ("as close as possible",
    // its header says), so the place is read back.
    let now = CGDisplayBounds(display).origin;
    if now.x != x as f64 || now.y != y as f64 {
        return Err(format!("macOS put the second display at {:.0},{:.0}, not at the corner {x},{y} it was asked for",
                           now.x, now.y));
    }
    Ok(())
}

/// A display's place among the screens and its size, in points: "1470x956 at 1470,956".
unsafe fn place_of(display: u32) -> String {
    let b = CGDisplayBounds(display);
    format!("{:.0}x{:.0} at {:.0},{:.0}", b.size.width, b.size.height, b.origin.x, b.origin.y)
}

/// The display to work on: the one --display names, or a new one of this
/// program's own. A new one is given back too: macOS keeps it while that lives.
unsafe fn open_display(args: &Args) -> Res<(u32, Option<Display>)> {
    if let Some(id) = args.display {
        if !displays().contains(&id) {
            let on: Vec<String> = displays().iter().map(|id| id.to_string()).collect();
            return Err(format!("there is no display {id}: the displays that are on are {}", on.join(", ")));
        }
        if id == CGMainDisplayID() {
            return Err(format!("display {id} is your main screen: the agent's windows would be among yours"));
        }
        return Ok((id, None));
    }
    let main = CGDisplayBounds(CGMainDisplayID());
    let (width, height) = args.size.unwrap_or((main.size.width as u32, main.size.height as u32));
    let display = make_display(width, height, args.retina)?;
    if let Err(e) = place_at_corner(display.id) {
        eprintln!("{e}: it stays beside your main screen, where your pointer can reach it");
    }
    Ok((display.id, Some(display)))
}

// ---------------------------------------------------------------- the apps

/// What a started app is told on its command line: settings for this one
/// run, none of them is saved. No windows of its last run. An empty document
/// instead of the Open panel, which would show the names of your files. And
/// every new document as a tab of the window the app has: a new window comes
/// up on your screen (seen), a tab comes up where its window is.
const APP_SETTINGS: [&str; 6] = ["-ApplePersistenceIgnoreState", "YES",
                                 "-NSShowAppCentricOpenPanelInsteadOfUntitledFile", "NO",
                                 "-AppleWindowTabbingMode", "always"];

/// Where an app given by its name is looked for, in this order.
const APP_FOLDERS: [&str; 5] = ["/System/Applications", "/System/Applications/Utilities", "/Applications",
                                "/Applications/Utilities", "~/Applications"];

const WINDOW_WAIT: f64 = 10.0; // seconds a started app gets to make its first window
const MENU_BAR: f64 = 40.0; // the room a window is left at the top of the display, for its menu bar
const WATCH_EVERY: f64 = 0.1; // seconds between two rounds of the watcher

/// One process for good: its id and the moment it was born. macOS gives the
/// id of an ended process to a later one, so a signal is only sent after the
/// two have been checked together.
#[derive(Clone, Copy, PartialEq)]
struct Process {
    pid: i32,
    born: (u64, u64),
}

/// What the kernel keeps about a process. None when there is none of that id.
unsafe fn process_info(pid: i32) -> Option<ProcessInfo> {
    let mut info: ProcessInfo = std::mem::zeroed();
    let size = std::mem::size_of::<ProcessInfo>() as i32;
    if proc_pidinfo(pid, PROCESS_INFO, 0, &mut info as *mut ProcessInfo as *mut c_void, size) != size {
        return None;
    }
    Some(info)
}

/// The process that has this id now. None when none has, or it has ended.
unsafe fn process(pid: i32) -> Option<Process> {
    let info = process_info(pid)?;
    if info.status == ZOMBIE { None } else { Some(Process { pid, born: (info.born_seconds, info.born_microseconds) }) }
}

/// Whether that very process still runs.
unsafe fn alive(known: &Process) -> bool {
    process(known.pid) == Some(*known)
}

/// A process's name, for the reports.
unsafe fn process_name(pid: i32) -> String {
    let Some(info) = process_info(pid) else { return format!("process {pid}") };
    let name = if info.name[0] != 0 { &info.name[..] } else { &info.short_name[..] };
    let end = name.iter().position(|&byte| byte == 0).unwrap_or(name.len());
    String::from_utf8_lossy(&name[..end]).into_owned()
}

/// The ids of all processes that run now.
unsafe fn all_processes() -> Vec<i32> {
    let mut pids = vec![0i32; proc_listallpids(null_mut(), 0).max(0) as usize + 256];
    let count = proc_listallpids(pids.as_mut_ptr() as *mut c_void, (pids.len() * 4) as i32);
    pids.truncate(count.max(0) as usize);
    pids
}

/// What processes started themselves, and what that started in turn: every
/// process that descends from one of `roots`, by parent id. An Electron
/// app's helpers are found this way. The helpers macOS itself starts for an
/// app (XPC services) are not: their parent is launchd.
unsafe fn descendants(roots: &[i32]) -> Vec<i32> {
    let parents: Vec<(i32, i32)> = all_processes().iter()
        .filter_map(|&pid| Some((pid, process_info(pid)?.parent as i32))).collect();
    let (mut found, mut next) = (Vec::new(), roots.to_vec());
    while let Some(parent) = next.pop() {
        for &(pid, of) in &parents {
            if of == parent && !found.contains(&pid) && !roots.contains(&pid) {
                found.push(pid);
                next.push(pid);
            }
        }
    }
    found
}

/// The helpers macOS started for apps: the processes whose parent is launchd
/// and for which macOS holds one of `apps` responsible. They are what
/// `descendants` misses. Empty when this macOS does not say.
unsafe fn helpers(apps: &[i32]) -> Vec<i32> {
    let Some(ask) = private_function("responsibility_get_pid_responsible_for_pid") else { return Vec::new() };
    let ask = std::mem::transmute::<*mut c_void, ResponsibleFor>(ask);
    all_processes().into_iter().filter(|&pid| !apps.contains(&pid) && apps.contains(&ask(pid))
        && process_info(pid).is_some_and(|info| info.parent == 1)).collect()
}

/// A process's RAM in MB: its physical footprint, the number Activity Monitor
/// shows as Memory. None when macOS does not say.
unsafe fn footprint_mb(pid: i32) -> Option<f64> {
    let mut used: ResourceUse = std::mem::zeroed();
    if proc_pid_rusage(pid, USE_V2, &mut used as *mut ResourceUse as *mut *mut c_void) != 0 {
        return None;
    }
    Some(used.footprint as f64 / 1048576.0)
}

/// End processes and what they started: each is asked to end (SIGTERM), and
/// what is still there a second later is killed (SIGKILL). An ordinary Mac
/// app goes at once when asked, with no question about unsaved work. True
/// when nothing is left.
unsafe fn end_processes(roots: &[Process]) -> bool {
    let mut all: Vec<Process> = roots.iter().copied().filter(|root| alive(root)).collect();
    let pids: Vec<i32> = all.iter().map(|root| root.pid).collect();
    all.extend(descendants(&pids).into_iter().filter_map(|pid| process(pid)));
    for how in [SIGTERM, SIGKILL] {
        for one in &all {
            if alive(one) {
                kill(one.pid, how);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while all.iter().any(|one| alive(one)) {
            if Instant::now() >= deadline {
                break;
            }
            sleep(0.01);
        }
    }
    !all.iter().any(|one| alive(one))
}

/// The pipe to the guard: every process id written into it is one the guard
/// ends when the pipe closes. macOS closes it when this program is gone.
static GUARD: Mutex<Option<ChildStdin>> = Mutex::new(None);

/// Held while a start is in flight: from before the launch call until the new process id
/// has reached the guard. The watcher takes it before it ends the apps on Ctrl+C or kill.
/// Without it an app could be running, hidden, that neither the watcher nor the guard knows.
/// It is always the first lock taken: nothing waits for it while holding APPS or GUARD.
static STARTING: Mutex<()> = Mutex::new(());

/// Start the guard: this same program once more, with the one word
/// GUARD_WORD. macOS has no job object that ends the apps with the program
/// that started them, so a second small process does it. It holds the read
/// end of a pipe and this program the write end. However this program ends
/// (Ctrl+C, kill -9, a crash), macOS closes the pipe and the guard ends the
/// apps. It is in a process group of its own, so a Ctrl+C in the terminal
/// does not reach it. Call it first thing: `start` calls it too.
fn start_guard() -> Res<()> {
    let mut guard = GUARD.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| format!("could not find this program's own file: {e}"))?;
    let mut child = Command::new(exe).arg(GUARD_WORD).stdin(Stdio::piped()).stdout(Stdio::null())
        .stderr(Stdio::null()).process_group(0).spawn()
        .map_err(|e| format!("could not start the guard that ends the apps: {e}"))?;
    *guard = child.stdin.take();
    Ok(())
}

/// Tell the guard about a process this program started.
fn tell_guard(pid: i32) {
    let told = match GUARD.lock().unwrap().as_mut() {
        Some(pipe) => writeln!(pipe, "{pid}").is_ok(),
        None => false,
    };
    if !told {
        eprintln!("process {pid} has no guard: it will not end by itself if this program is killed");
    }
}

/// The guard's whole life, when this program was started with GUARD_WORD:
/// read process ids from the pipe until it closes, then end them and what
/// they started. It uses none of macOS's frameworks, so it is no app, has no
/// Dock icon and holds about 2 MB. Seen: after kill -9 of the starter,
/// Calculator and TextEdit were gone within 40 ms, and the helpers macOS had
/// started for them went by themselves.
fn guard() -> ! {
    unsafe {
        for stay_for in [SIGHUP, SIGINT, SIGTERM] {
            signal(stay_for, IGNORE);
        }
    }
    let mut known: Vec<Process> = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if let Some(process) = line.trim().parse().ok().and_then(|pid| unsafe { process(pid) }) {
            known.push(process);
        }
    }
    unsafe { end_processes(&known) };
    std::process::exit(0)
}

/// How far a started app has come.
#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// `start` is still busy with it.
    Starting,
    /// It made no window in time: it stays hidden until it has one.
    Hidden,
    /// Its windows are on the display.
    Shown,
    /// It came in front of your apps: it was hidden again, and stays hidden.
    Withdrawn,
}

/// One app this program started.
#[derive(Clone)]
struct App {
    process: Process,
    name: String,
    stage: Stage,
}

/// The apps this program started, in the order they were started.
static APPS: Mutex<Vec<App>> = Mutex::new(Vec::new());

/// What the watcher saw that should never be seen, and how many times.
static SEEN: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// How many rounds the watcher still does 10 ms apart, see `expect_window`.
static QUICK_ROUNDS: AtomicU32 = AtomicU32::new(0);

fn started_apps() -> Vec<App> {
    APPS.lock().unwrap().clone()
}

fn set_stage(pid: i32, stage: Stage) {
    if let Some(app) = APPS.lock().unwrap().iter_mut().rev().find(|app| app.process.pid == pid) {
        app.stage = stage;
    }
}

/// The folder of an app given by its name ("Calculator", "Font Book") or by
/// the path of its .app.
fn app_path(app: &str) -> Res<PathBuf> {
    if app.contains('/') {
        let path = PathBuf::from(app);
        let is_app = path.is_dir() && path.extension().is_some_and(|ending| ending == "app");
        return if is_app { Ok(path) } else { Err(format!("there is no app at {}", quote(app))) };
    }
    let file = if app.ends_with(".app") { app.to_string() } else { format!("{app}.app") };
    let home = std::env::var("HOME").unwrap_or_default();
    APP_FOLDERS.iter().map(|folder| Path::new(&folder.replacen('~', &home, 1)).join(&file)).find(|path| path.is_dir())
        .ok_or(format!("there is no app named {}: looked in {}", quote(app), APP_FOLDERS.join(", ")))
}

/// Whether an app is built on Chromium: a browser of that family, an
/// Electron app, or a web app made by such a browser. Its folder then holds
/// the browser engine as a framework named "... Framework.framework" (seen
/// in Brave, Edge, Cursor, Claude and GitHub Desktop, and in none of Apple's
/// own apps). A web app has no such folder: its program is app_mode_loader,
/// which loads your browser and hands the start to it (seen on the disk, in
/// a web app made by Chrome). Asked of the disk, not of the app. No such app
/// was started in the tests.
fn is_chromium(app: &Path) -> bool {
    if app.join("Contents/MacOS/app_mode_loader").exists() {
        return true;
    }
    let Ok(frameworks) = std::fs::read_dir(app.join("Contents/Frameworks")) else { return false };
    frameworks.flatten().any(|entry| entry.file_name().to_string_lossy().ends_with(" Framework.framework"))
}

/// Start a NEW copy of an app hidden and in the background, with files to
/// open or none. Gives its process id. A hidden app makes its windows but
/// macOS shows none of them, on any display. These two NSWorkspace calls are
/// old ones Apple wants replaced, but they give the process back at once and
/// need no block, and their hide option does hide (seen). With a file it
/// does not hold: an app shows itself for a document, and its window is on
/// show on your screen until it is moved (seen, also with the new call
/// NSWorkspaceOpenConfiguration and hides). So `start` gives no file to the
/// start when it can hand the files over later.
unsafe fn launch_hidden(app: &Path, files: &[PathBuf]) -> Res<i32> {
    let url_of = |path: &Path| {
        let text = Cf(cf_string(&path.to_string_lossy()));
        msg!(Id; class("NSURL"), "fileURLWithPath:", text.0 => Id)
    };
    // A Cocoa app reads APP_SETTINGS as settings. An app built on Chromium
    // would take "YES", "NO" and "always" for pages or files to open: it gets none.
    // (Not reached for now: `start` refuses an app built on Chromium.)
    let settings: &[&str] = if is_chromium(app) { &[] } else { &APP_SETTINGS };
    let settings: Vec<Cf> = settings.iter().map(|word| Cf(cf_string(word))).collect();
    let words: Vec<Id> = settings.iter().map(|word| word.0).collect();
    let arguments = msg!(Id; class("NSArray"), "arrayWithObjects:count:", words.as_ptr() => *const Id,
                         words.len() => usize);
    let (options, key) = (new_object("NSMutableDictionary"), Cf(cf_string("NSWorkspaceLaunchConfigurationArguments")));
    msg!((); options, "setObject:forKey:", arguments => Id, key.0 => Id);
    let workspace = msg!(Id; class("NSWorkspace"), "sharedWorkspace");
    let mut error: Id = null_mut();
    let running = if files.is_empty() {
        msg!(Id; workspace, "launchApplicationAtURL:options:configuration:error:", url_of(app) => Id,
             START_HIDDEN => usize, options => Id, &mut error => *mut Id)
    } else {
        let urls: Vec<Id> = files.iter().map(|file| url_of(file)).collect();
        let urls = msg!(Id; class("NSArray"), "arrayWithObjects:count:", urls.as_ptr() => *const Id,
                        urls.len() => usize);
        msg!(Id; workspace, "openURLs:withApplicationAtURL:options:configuration:error:", urls => Id,
             url_of(app) => Id, START_HIDDEN => usize, options => Id, &mut error => *mut Id)
    };
    msg!((); options, "release");
    if running.is_null() {
        let why = if error.is_null() { String::new() } else { rust_string(msg!(Id; error, "localizedDescription")) };
        return Err(format!("{} did not start: {why}", app.display()));
    }
    Ok(msg!(i32; running, "processIdentifier"))
}

/// The moment now, as the kernel stamps the birth of a process: seconds and
/// microseconds of the clock.
fn clock() -> (u64, u64) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (now.as_secs(), now.subsec_micros() as u64)
}

/// Start a hidden copy and make sure it is a new process, born after the
/// call. Some apps keep to one copy, and macOS may then give back the copy
/// that runs already: yours. That one is left alone: it is not moved, and it
/// is not told to the guard, which would end it. Seen: a new copy was born
/// 6 to 10 ms after the call. No app that keeps to one copy was started in
/// the tests.
unsafe fn launch_new(app: &Path, files: &[PathBuf], name: &str) -> Res<Process> {
    let _in_flight = STARTING.lock().unwrap(); // until the guard knows the new process, see STARTING
    let before = clock();
    let pid = launch_hidden(app, files)?;
    let Some(started) = process(pid) else { return Err(format!("{name} ended as soon as it started")) };
    if started.born < before {
        return Err(format!("{name} was not started: it keeps to one copy, and that copy runs already \
                            (process {pid}). It was left alone."));
    }
    tell_guard(pid);
    Ok(started)
}

/// Whether this program may hand files to a running app with an Apple
/// event, as things are. macOS keeps that permission per app (Privacy &
/// Security > Automation). It is only read here: you are never asked.
/// Seen: allowed for TextEdit, not allowed for Preview (error -1744).
unsafe fn may_hand_files(pid: i32) -> bool {
    let to = msg!(Id; class("NSAppleEventDescriptor"), "descriptorWithProcessIdentifier:", pid => i32);
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let may = AEDeterminePermissionToAutomateTarget(msg!(*const c_void; to, "aeDesc"), CORE_EVENT,
                                                        OPEN_DOCUMENTS, 0);
        // -600: macOS does not know the process yet, a moment after its start
        if may != NO_SUCH_PROCESS || Instant::now() >= deadline {
            return may == 0;
        }
        sleep(0.02);
    }
}

/// Hand files to one running app, by its process id: the Apple event "open
/// these documents", which is what macOS itself sends an app for a file.
/// NSWorkspace cannot do it: it picks the copy of the app itself, and that
/// can be a copy you run. Call it only after `may_hand_files` said yes:
/// then no question is put to you.
unsafe fn open_in(pid: i32, files: &[PathBuf]) -> Res<()> {
    let descriptor = class("NSAppleEventDescriptor");
    let to = msg!(Id; descriptor, "descriptorWithProcessIdentifier:", pid => i32);
    let event = msg!(Id; descriptor, "appleEventWithEventClass:eventID:targetDescriptor:returnID:transactionID:",
                     CORE_EVENT => u32, OPEN_DOCUMENTS => u32, to => Id, -1 => i16, 0 => i32);
    let list = msg!(Id; descriptor, "listDescriptor");
    for file in files {
        let text = Cf(cf_string(&file.to_string_lossy()));
        let url = msg!(Id; class("NSURL"), "fileURLWithPath:", text.0 => Id);
        let one = msg!(Id; descriptor, "descriptorWithFileURL:", url => Id);
        msg!((); list, "insertDescriptor:atIndex:", one => Id, 0 => isize); // index 0 adds at the end of the list
    }
    msg!((); event, "setParamDescriptor:forKeyword:", list => Id, DIRECT_OBJECT => u32);
    let mut error: Id = null_mut();
    msg!(Id; event, "sendEventWithOptions:timeout:error:", SEND_AND_GO_ON => usize, 5.0 => f64, &mut error => *mut Id);
    if !error.is_null() {
        return Err(format!("it did not take the files: {}", rust_string(msg!(Id; error, "localizedDescription"))));
    }
    Ok(())
}

/// Show a hidden app: its windows come on show where they are. The app does
/// not come in front and your front app keeps the keyboard (seen). What
/// `unhide` gives back says nothing: it was false for an app that came out.
unsafe fn show_app(pid: i32) {
    let app = msg!(Id; class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid => i32);
    if !app.is_null() {
        msg!(bool; app, "unhide");
    }
}

/// Hide an app again, as it was when it started: for one that came in front
/// of your apps. None ever did in the tests, so this has not run.
unsafe fn hide_app(pid: i32) {
    let app = msg!(Id; class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid => i32);
    if !app.is_null() {
        msg!(bool; app, "hide");
    }
}

/// The windows an app has, as Accessibility lists them: its documents, its
/// panels and its dialogs, also while the app is hidden.
unsafe fn app_windows(pid: i32) -> Vec<Cf> {
    let app = ax_app(pid);
    ax_list(app.0, "AXWindows").into_iter().filter(|window| ax_frame(window.0).is_some()).collect()
}

/// Whether a place is, in whole or in part, on a display other than the
/// second one: on a screen of yours.
unsafe fn on_your_screens(frame: &Rect, display: u32) -> bool {
    displays().iter().any(|&other| other != display && overlap(frame, &CGDisplayBounds(other)))
}

/// Whether an app has a real window on show on a screen of yours. It never
/// should, so it is remembered for the report.
unsafe fn shows_on_your_screen(app: &App, display: u32, windows: &[Window]) -> bool {
    let shows = windows.iter().any(|w| w.pid == app.process.pid && real(w) && on_your_screens(&w.frame, display));
    if shows {
        saw(format!("a window of {} was on show on your screen", app.name));
    }
    shows
}

/// A place on the display for a window of a size: on one row under the menu
/// bar, right of what is there. With no room left there, stepped down from
/// the top left.
fn free_place(display: &Rect, size: Size, taken: &[Rect]) -> Point {
    let top = display.origin.y + MENU_BAR + 14.0;
    let rights = taken.iter().map(|other| other.origin.x + other.size.width);
    let left = rights.fold(display.origin.x + 20.0, f64::max) + 10.0;
    if left + size.width <= display.origin.x + display.size.width - 10.0 {
        return Point { x: left, y: top };
    }
    let step = 30.0 * (taken.len() % 8) as f64;
    Point { x: display.origin.x + 30.0 + step, y: top + step }
}

/// Move windows of one app onto the display, through Accessibility, each to
/// a free place beside the windows of the started apps that are on it.
unsafe fn place(display: u32, windows: &[Cf]) {
    let bounds = CGDisplayBounds(display);
    let ours: Vec<i32> = started_apps().iter().map(|app| app.process.pid).collect();
    let mut taken: Vec<Rect> = windows_on_screen().iter()
        .filter(|w| w.layer == 0 && ours.contains(&w.pid) && real(w) && overlap(&w.frame, &bounds)
            && !on_your_screens(&w.frame, display))
        .map(|w| w.frame).collect();
    for window in windows {
        let Some(frame) = ax_frame(window.0) else { continue };
        let at = free_place(&bounds, frame.size, &taken);
        let value = Cf(AXValueCreate(AX_POINT, &at as *const Point as *const c_void));
        ax_set(window.0, "AXPosition", value.0);
        taken.push(Rect { origin: at, size: frame.size });
    }
}

/// Start a NEW copy of an app straight onto the second display, with files
/// to open or none. The app is given by its name or by the path of its .app.
/// Gives its process id.
///
/// macOS cannot be told which display a new app opens on. So the app starts
/// hidden, its windows are moved onto the display through Accessibility
/// while they are hidden, and only then it is shown. The files are handed to
/// it after that, and come up as tabs of the window it has. Seen with
/// Calculator, TextEdit, Font Book and Dictionary: no window is on show on
/// your screen at any moment, and the app is never the front app.
///
/// Where that cannot be done, a window of the app is on show on your screen
/// for a moment until it is moved (seen with Preview and a picture: 0.04
/// to 0.08 seconds):
///   - the app may not be handed files (see `may_hand_files`). The files
///     then go to the start itself, and the app shows itself for them.
///   - the app makes no window by itself (a viewer). It shows itself when
///     it gets its files.
///
/// An app that makes no window in WINDOW_WAIT seconds stays hidden and keeps
/// running: the keeper brings it out when it has a window. An app with
/// several windows has them all moved: the wait ends when no new window has
/// come for a moment.
unsafe fn start(display: u32, app: &str, files: &[&str]) -> Res<i32> {
    let path = app_path(app)?;
    let name = path.file_stem().map(|name| name.to_string_lossy().into_owned()).unwrap_or(app.to_string());
    // An app built on Chromium (Chrome, Edge, Brave, and Electron apps like Cursor) is
    // refused for now, before anything is launched. Read, not tried: none was ever started.
    if is_chromium(&path) {
        return Err(format!("{name} was not started: it is built on Chromium. A new copy of such an app would \
                            run on the profile of the copy you use, or hand its start to that copy, which opens \
                            a window on your screen. Not supported until --run can give the app a profile \
                            folder of its own."));
    }
    let mut opened = Vec::new();
    for file in files {
        opened.push(Path::new(file).canonicalize().map_err(|_| format!("there is no file {}", quote(file)))?);
    }
    if AXIsProcessTrusted() == 0 {
        return Err(format!("{name} was not started: its windows are moved onto the second display through \
                            Accessibility, and that is not allowed for the app this runs in \
                            (System Settings > Privacy & Security > Accessibility)"));
    }
    start_guard()?;
    let pool = objc_autoreleasePoolPush();
    let started = start_hidden(display, &path, &name, &opened);
    objc_autoreleasePoolPop(pool);
    started
}

/// The work of `start`, with the app's folder and the files checked.
unsafe fn start_hidden(display: u32, path: &Path, name: &str, files: &[PathBuf]) -> Res<i32> {
    let mut process = launch_new(path, &[], name)?;
    // The files are handed over when the app is on the display. Where that
    // is not allowed, this copy is ended and one is started with the files.
    let hand_over = !files.is_empty() && may_hand_files(process.pid);
    if !files.is_empty() && !hand_over {
        end_processes(&[process]);
        process = launch_new(path, files, name)?;
    }
    let pid = process.pid;
    APPS.lock().unwrap().push(App { process, name: name.to_string(), stage: Stage::Starting });

    // Its own first windows: moved while it is hidden, then it is shown.
    let (element, deadline) = (ax_app(pid), Instant::now() + Duration::from_secs_f64(WINDOW_WAIT));
    let (mut count, mut since, mut answers) = (0, Instant::now(), None);
    let mut stage = Stage::Hidden;
    while Instant::now() < deadline {
        if !alive(&process) {
            return Err(format!("{name} ended right after it started"));
        }
        if answers.is_none() && ax_text(element.0, "AXRole").is_some() {
            answers = Some(Instant::now()); // it answers Accessibility: it has done starting
        }
        let windows = app_windows(pid);
        if windows.len() != count {
            (count, since) = (windows.len(), Instant::now());
        }
        // All its windows are there when no new one has come for a moment.
        // An app that got files with its start is not hidden any more: its
        // window is moved the moment it is there.
        if count > 0 && (since.elapsed().as_secs_f64() >= 0.15 || !files.is_empty() && !hand_over) {
            if let Some(app) = started_apps().iter().rev().find(|app| app.process.pid == pid) {
                shows_on_your_screen(app, display, &windows_on_screen());
            }
            place(display, &windows);
            show_app(pid);
            stage = Stage::Shown;
            break;
        }
        // With files to hand over there is no long wait for a window the app
        // may never make by itself.
        if count == 0 && hand_over && answers.is_some_and(|at: Instant| at.elapsed().as_secs_f64() >= 0.5) {
            break;
        }
        sleep(if hand_over || files.is_empty() { 0.02 } else { 0.005 });
    }
    set_stage(pid, stage);

    if !files.is_empty() {
        if hand_over && stage == Stage::Shown {
            // The files go to it once its window is on show. A document that
            // comes sooner cannot be a tab of that window: it came up as a
            // window of its own, on your screen (seen, 2 starts in 3).
            let (bounds, patience) = (CGDisplayBounds(display), Instant::now() + Duration::from_secs(2));
            while !windows_on_screen().iter().any(|w| w.pid == pid && real(w) && overlap(&w.frame, &bounds)) {
                if Instant::now() >= patience {
                    break;
                }
                sleep(0.01);
            }
            sleep(0.05);
        }
        let before: Vec<u32> = windows_on_screen().iter().filter(|w| w.pid == pid).map(|w| w.id).collect();
        if hand_over {
            if let Err(why) = open_in(pid, files) {
                end_processes(&[process]);
                return Err(format!("{name} was ended again: {why}"));
            }
        }
        // The keeper at full speed until the documents are on show: it moves
        // a window that comes up on your screen at once.
        let (sent, mut came) = (Instant::now(), None);
        while sent.elapsed().as_secs_f64() < 5.0 && !came.is_some_and(|at: Instant| at.elapsed().as_secs_f64() >= 0.3) {
            let windows = windows_on_screen();
            keep(display, &windows);
            let new_one = windows.iter().any(|w| w.pid == pid && real(w) && !before.contains(&w.id));
            if came.is_none() && (!hand_over || new_one) {
                came = Some(Instant::now());
            }
            sleep(0.01);
        }
        stage = started_apps().iter().rev().find(|app| app.process.pid == pid).map_or(stage, |app| app.stage);
    }
    if stage == Stage::Hidden {
        let locked = " (your screen is locked, and Accessibility gives nothing back while it is)";
        let why = if screen_locked() { locked } else { "" };
        eprintln!("{name} (process {pid}) has made no window{why}: it stays hidden until it has one");
    }
    Ok(pid)
}

/// One round of the keeper. A window a started app opens later has to be on
/// the second display too. A new document comes up there by itself, as a
/// tab (see APP_SETTINGS). Any other new window (About, Settings, a dialog)
/// comes up on your screen, wherever the app's other windows are (seen). So
/// any window on show of a started app that lies on a screen of yours is
/// moved onto the display. Seen: such a window was on your screen for about
/// 0.1 seconds, and for 0.01 to 0.06 after `expect_window`. And an app that
/// is still hidden because it had no window is brought out once it has one.
unsafe fn keep(display: u32, windows: &[Window]) {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap();
    for app in started_apps() {
        let pid = app.process.pid;
        if !alive(&app.process) {
            continue;
        }
        let astray = shows_on_your_screen(&app, display, windows);
        if app.stage == Stage::Hidden {
            let found = app_windows(pid);
            if !found.is_empty() {
                place(display, &found);
                show_app(pid);
                set_stage(pid, Stage::Shown);
            }
        } else if app.stage == Stage::Shown && astray {
            let astray: Vec<Cf> = app_windows(pid).into_iter()
                .filter(|window| ax_frame(window.0).is_some_and(|frame| on_your_screens(&frame, display))).collect();
            place(display, &astray);
        }
    }
}

/// Remember something the watcher saw.
fn saw(what: String) {
    let mut seen = SEEN.lock().unwrap();
    match seen.iter_mut().find(|(known, _)| *known == what) {
        Some((_, times)) => *times += 1,
        None => seen.push((what, 1)),
    }
}

/// How many times the watcher and the keeper have seen something so far.
fn seen_times() -> u32 {
    SEEN.lock().unwrap().iter().map(|(_, times)| times).sum()
}

/// One round of the watcher: is a started app the front app, or has one
/// your keyboard. Neither should ever be, and what is seen is remembered for
/// the report, as the keeper remembers a window on your screen. What lasts
/// less than the time between two rounds can be missed. An app that is in
/// front of yours is hidden again at once and stays hidden.
unsafe fn look() {
    let (front, keyboard) = (front_process(), keyboard_process());
    for app in started_apps() {
        let pid = app.process.pid;
        if !alive(&app.process) {
            continue;
        }
        if front == Some(pid) {
            saw(format!("{} was the front app", app.name));
            if app.stage != Stage::Withdrawn {
                hide_app(pid);
                set_stage(pid, Stage::Withdrawn);
                eprintln!("{} (process {pid}) came in front of your apps: it was hidden again", app.name);
            }
        }
        if keyboard == Some(pid) {
            saw(format!("{} had your keyboard", app.name));
        }
    }
}

/// Start the watcher and the keeper: one thread that looks every
/// WATCH_EVERY seconds, for as long as this program runs. It asks macOS for
/// the windows on screen once a round, and talks to an app only when one of
/// the app's windows has to be moved. It is also where Ctrl+C and "kill"
/// end the program while there is no viewer window to close: the apps end
/// first, then the display goes with the program.
fn watch(display: u32) {
    thread::spawn(move || loop {
        if INTERRUPTED.load(Ordering::SeqCst) && VIEWER.get().is_none() {
            // A start in flight first gets its app to the guard: then none is left that nobody ends.
            let _in_flight = STARTING.lock();
            unsafe { end_all() };
            remove_lines_file();
            // Not eprintln!: with the terminal gone it would break this thread, and the program would stay.
            let _ = writeln!(std::io::stderr(), "{STOPPED}: the apps on the second display were ended");
            std::process::exit(1);
        }
        if !APPS.lock().unwrap().is_empty() {
            unsafe {
                let pool = objc_autoreleasePoolPush();
                look();
                keep(display, &windows_on_screen());
                objc_autoreleasePoolPop(pool);
            }
        }
        // one quick round less, in one step: a click that asks for 100 more in between is not lost
        let quick = QUICK_ROUNDS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1)).is_ok();
        sleep(if quick { 0.01 } else { WATCH_EVERY });
    });
}

/// Tell the keeper that a new window may come now: for the next second it
/// looks every 10 ms. Call it with every click and key the agent sends: a
/// window an app opens then (About, Settings, a dialog) comes up on your
/// screen, and the sooner the keeper sees it, the shorter it is there.
fn expect_window() {
    QUICK_ROUNDS.store(100, Ordering::Relaxed);
}

/// End every app this program started, and what they started. True when
/// nothing is left. It comes before the second display is given up: macOS
/// moves the windows of a display that goes away onto one that stays, and
/// that would be your screen. Seen with a display of this program's own,
/// looked at every 8 ms: at the end no window of its apps was on show
/// anywhere else, also not when the program was killed and the guard ended
/// them.
unsafe fn end_all() -> bool {
    let apps: Vec<Process> = started_apps().iter().map(|app| app.process).collect();
    end_processes(&apps)
}

/// "still running" or "exited", for an app this program started.
unsafe fn process_state(app: &App) -> String {
    match (alive(&app.process), app.stage) {
        (false, _) => "exited".to_string(),
        (true, Stage::Shown) => "still running".to_string(),
        (true, Stage::Withdrawn) => "still running, hidden: it came in front of your apps".to_string(),
        (true, _) => "still running, hidden: it has made no window".to_string(),
    }
}

/// (name, MB, what it is) for every started app that runs, for every process
/// one of them started, and for the helpers macOS started for them.
unsafe fn app_memory() -> Vec<(String, f64, &'static str)> {
    let apps: Vec<App> = started_apps().into_iter().filter(|app| alive(&app.process)).collect();
    let pids: Vec<i32> = apps.iter().map(|app| app.process.pid).collect();
    let mut rows = Vec::new();
    for app in &apps {
        if let Some(mb) = footprint_mb(app.process.pid) {
            rows.push((app.name.clone(), mb, ""));
        }
    }
    for (pids, what) in [(descendants(&pids), "started by an app"), (helpers(&pids), "helper from macOS")] {
        for pid in pids {
            if let Some(mb) = footprint_mb(pid) {
                rows.push((process_name(pid), mb, what));
            }
        }
    }
    rows
}

// ---------------------------------------------------------------- windows

/// One window on screen: its id for macOS, the app it belongs to, its layer
/// (0 for an ordinary window, above 0 for a menu or another popup) and its place.
#[derive(Clone, Copy)]
struct Window {
    id: u32,
    pid: i32,
    layer: i32,
    frame: Rect,
}

unsafe fn number_in(dictionary: Id, key: Id) -> i64 {
    let (number, mut value) = (CFDictionaryGetValue(dictionary, key), 0i64);
    if !number.is_null() && CFGetTypeID(number) == CFNumberGetTypeID() {
        CFNumberGetValue(number, NUMBER_I64, &mut value as *mut i64 as *mut c_void);
    }
    value
}

unsafe fn text_in(dictionary: Id, key: Id) -> String {
    let text = CFDictionaryGetValue(dictionary, key);
    if !text.is_null() && CFGetTypeID(text) == CFStringGetTypeID() { rust_string(text) } else { String::new() }
}

/// Every window that is on screen now, on every display, front to back, as
/// macOS's own list has them: no app is asked. With `named`, each comes with
/// the name of its app and its title. macOS gives the titles only with
/// Screen Recording allowed.
unsafe fn listed_windows(named: bool) -> Vec<(Window, String, String)> {
    let list = Cf(CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | NO_DESKTOP, 0));
    let mut windows = Vec::new();
    if list.0.is_null() {
        return windows;
    }
    for n in 0..CFArrayGetCount(list.0) {
        let about = CFArrayGetValueAtIndex(list.0, n);
        let mut frame = Rect::default();
        let bounds = CFDictionaryGetValue(about, WINDOW_BOUNDS);
        if bounds.is_null() || !CGRectMakeWithDictionaryRepresentation(bounds, &mut frame) {
            continue;
        }
        let window = Window {
            id: number_in(about, WINDOW_NUMBER) as u32,
            pid: number_in(about, WINDOW_OWNER_PID) as i32,
            layer: number_in(about, WINDOW_LAYER) as i32,
            frame,
        };
        let (app, title) = match named {
            true => (text_in(about, WINDOW_OWNER_NAME), text_in(about, WINDOW_NAME)),
            false => Default::default(),
        };
        windows.push((window, app, title));
    }
    windows
}

/// The same without the names: what the input, the keeper and the watcher look at.
unsafe fn windows_on_screen() -> Vec<Window> {
    listed_windows(false).into_iter().map(|(window, _, _)| window).collect()
}

fn holds(frame: &Rect, x: f64, y: f64) -> bool {
    x >= frame.origin.x && x < frame.origin.x + frame.size.width && y >= frame.origin.y
        && y < frame.origin.y + frame.size.height
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.origin.x < b.origin.x + b.size.width && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height && b.origin.y < a.origin.y + a.size.height
}

/// Whether a window is one of the thin strips every app owns along the top
/// of every display: its piece of the menu bar, not a window to click in.
unsafe fn is_menu_bar_piece(window: &Window) -> bool {
    if window.layer != 0 || window.frame.size.height > 60.0 {
        return false;
    }
    displays().iter().any(|&display| {
        let screen = CGDisplayBounds(display);
        screen.origin == window.frame.origin && screen.size.width == window.frame.size.width
    })
}

/// A real window: big enough to see, and not a piece of the menu bar.
unsafe fn real(window: &Window) -> bool {
    window.layer >= 0 && window.frame.size.width >= 4.0 && window.frame.size.height >= 4.0 && !is_menu_bar_piece(window)
}

/// The real windows on show on a display, top first: the ordinary ones
/// (layer 0), each with the name of its app and its title.
unsafe fn windows_on(display: u32) -> Vec<(Window, String, String)> {
    let bounds = CGDisplayBounds(display);
    listed_windows(true).into_iter()
        .filter(|(window, _, _)| window.layer == 0 && real(window) && overlap(&window.frame, &bounds)).collect()
}

/// Whether your screen is locked. Accessibility gives nothing back while it is.
unsafe fn screen_locked() -> bool {
    let session = Cf(CGSessionCopyCurrentDictionary());
    if session.0.is_null() {
        return false;
    }
    let key = Cf(cf_string("CGSSessionScreenIsLocked"));
    let locked = CFDictionaryGetValue(session.0, key.0);
    !locked.is_null() && CFGetTypeID(locked) == CFBooleanGetTypeID() && CFBooleanGetValue(locked) != 0
}

/// The front app as NSWorkspace has it: its process id and its name.
unsafe fn front_app() -> (i32, String) {
    pump(0.05); // NSWorkspace hears of a change through this program's run loop
    let app = msg!(Id; msg!(Id; class("NSWorkspace"), "sharedWorkspace"), "frontmostApplication");
    if app.is_null() {
        return (-1, String::new());
    }
    (msg!(i32; app, "processIdentifier"), rust_string(msg!(Id; app, "localizedName")))
}

/// The process id behind a serial number. None when macOS has none for it.
unsafe fn pid_of(serial: &Serial) -> Option<i32> {
    let to_pid = std::mem::transmute::<*mut c_void, PidOfSerial>(private_function("GetProcessPID")?);
    let mut pid = 0i32;
    if to_pid(serial, &mut pid) != 0 { None } else { Some(pid) }
}

/// The app in front, as the window server itself has it at this moment.
/// NSWorkspace hears of a change only when this program's run loop runs, so
/// it can be late. None when this macOS does not say.
unsafe fn front_process() -> Option<i32> {
    let ask = std::mem::transmute::<*mut c_void, FrontProcess>(private_function("_SLPSGetFrontProcess")?);
    let mut serial = Serial::default();
    if ask(&mut serial) != 0 { None } else { pid_of(&serial) }
}

/// The app your keyboard goes to, as the window server itself has it.
/// None when this macOS does not say.
unsafe fn keyboard_process() -> Option<i32> {
    let ask = std::mem::transmute::<*mut c_void, KeyFocusProcess>(private_function("SLPSGetKeyFocusProcess")?);
    let (mut serial, mut connection) = (Serial::default(), 0u32);
    if ask(&mut serial, &mut connection) != 0 { None } else { pid_of(&serial) }
}

/// An app's name as macOS has it, for the reports.
unsafe fn app_name(pid: i32) -> String {
    let app = msg!(Id; class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid => i32);
    if app.is_null() { format!("process {pid}") } else { rust_string(msg!(Id; app, "localizedName")) }
}

/// Where the pointer is, in the points all screens share.
unsafe fn pointer() -> Point {
    let event = CGEventCreate(null_mut());
    if event.is_null() {
        return Point::default();
    }
    let at = CGEventGetLocation(event);
    CFRelease(event);
    at
}

// ---------------------------------------------------------------- the picture

/// One picture, given back to macOS when dropped.
struct Picture(Id);

impl Drop for Picture {
    fn drop(&mut self) {
        unsafe { CGImageRelease(self.0) }
    }
}

impl Picture {
    /// Its size in pixels.
    unsafe fn size(&self) -> (usize, usize) {
        (CGImageGetWidth(self.0), CGImageGetHeight(self.0))
    }
}

/// One picture of a display, as macOS draws it now.
///
/// Two processes of the SAME program file cannot both take pictures. Seen
/// with two runs of this program, on two displays and on one, and with two
/// runs of one Python: the second asks and gets none, after 5 seconds, then
/// the first gets none either, until one of the two has ended. To try again
/// does not help (tried: 4 tries each at uneven times, 8 runs of 8 got no
/// picture), and CGWindowListCreateImage of the display's rectangle did the
/// same. Two different programs do not block each other, and neither do two
/// copies of this program's file, under another name or in another folder
/// (seen: both got their picture). So a second run at the same time needs a
/// copy of the file of its own.
unsafe fn capture(display: u32) -> Res<Picture> {
    if !CGPreflightScreenCaptureAccess() {
        return Err("no picture: Screen Recording is not allowed for the app this runs in \
                    (System Settings > Privacy & Security > Screen & System Audio Recording)".to_string());
    }
    let image = CGDisplayCreateImage(display);
    if image.is_null() { Err("macOS gave no picture of the display".to_string()) } else { Ok(Picture(image)) }
}

/// Save a picture as a PNG file, with macOS's own PNG writer.
unsafe fn save_png(picture: &Picture, path: &Path) -> Res<()> {
    let failed = || format!("could not save {}", path.display());
    let bytes = path.as_os_str().as_encoded_bytes();
    let url = CFURLCreateFromFileSystemRepresentation(null(), bytes.as_ptr(), bytes.len() as isize, 0);
    if url.is_null() {
        return Err(failed());
    }
    let png = cf_string("public.png");
    let file = CGImageDestinationCreateWithURL(url, png, 1, null_mut());
    CFRelease(png);
    CFRelease(url);
    if file.is_null() {
        return Err(failed());
    }
    CGImageDestinationAddImage(file, picture.0, null_mut());
    let saved = CGImageDestinationFinalize(file);
    CFRelease(file);
    if saved { Ok(()) } else { Err(failed()) }
}

// A CGImage never changes once it is made, and macOS lets any thread keep and
// release one. So a Picture can go from the thread that takes it to the window.
unsafe impl Send for Picture {}
unsafe impl Sync for Picture {}

/// A picture's pixels when macOS keeps them the way this program wants them
/// already: four bytes a pixel, in memory blue, green, red, alpha. That is
/// how a picture of a display comes (seen on macOS 27.0: 32 bits a pixel,
/// alpha first and always 255, low byte first). Only the filling at the end
/// of every row is left out: a display 1470 pixels wide has 5888 bytes a
/// row, not 5880. None when the picture is kept another way.
unsafe fn pixels_copied(picture: &Picture) -> Option<Vec<u8>> {
    let (width, height) = picture.size();
    let (info, row) = (CGImageGetBitmapInfo(picture.0), CGImageGetBytesPerRow(picture.0));
    let alpha = info & ALPHA_MASK;
    let as_wanted = CGImageGetBitsPerPixel(picture.0) == 32 && CGImageGetBitsPerComponent(picture.0) == 8
        && info & ORDER_MASK == ORDER_32_LITTLE && info & FLOAT_COMPONENTS == 0
        && [ALPHA_FIRST_PREMULTIPLIED, ALPHA_FIRST, ALPHA_SKIPPED_FIRST].contains(&alpha);
    if !as_wanted || width == 0 || height == 0 || row < width * 4 {
        return None;
    }
    let data = Cf(CGDataProviderCopyData(CGImageGetDataProvider(picture.0)));
    let needed = row * (height - 1) + width * 4; // macOS gives some bytes more, never fewer
    if data.0.is_null() || (CFDataGetLength(data.0) as usize) < needed {
        return None;
    }
    let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(data.0), needed);
    let mut pixels = Vec::with_capacity(width * height * 4);
    for line in bytes.chunks(row) {
        pixels.extend_from_slice(&line[..width * 4]);
    }
    if alpha == ALPHA_SKIPPED_FIRST {
        // the fourth byte means nothing there: make it "solid", so that two pictures compare by their colours
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Some(pixels)
}

/// The same for a picture kept any other way (another byte order, more
/// bits a colour): macOS draws it into bytes laid out as wanted. In the
/// picture's own colours when macOS can draw in those, so that no colour
/// changes, and in plain RGB when not.
unsafe fn pixels_drawn(picture: &Picture) -> Res<Vec<u8>> {
    let (width, height) = picture.size();
    if width == 0 || height == 0 {
        return Err("the picture is empty".to_string());
    }
    let mut pixels = vec![0u8; width * height * 4];
    let plain = Cf(CGColorSpaceCreateDeviceRGB());
    for space in [CGImageGetColorSpace(picture.0), plain.0] {
        if space.is_null() {
            continue;
        }
        let canvas = Cf(CGBitmapContextCreate(pixels.as_mut_ptr() as *mut c_void, width, height, 8, width * 4,
                                              space, BGRA));
        if canvas.0.is_null() {
            continue;
        }
        let all = Rect { origin: Point::default(), size: Size { width: width as f64, height: height as f64 } };
        CGContextDrawImage(canvas.0, all, picture.0);
        return Ok(pixels);
    }
    Err("macOS did not give the picture's pixels".to_string())
}

/// A picture's pixels as bytes: rows from the top down, four bytes a pixel
/// (blue, green, red, alpha) and nothing between the rows. `picture.size()`
/// says how many. For the BMP writer, for the stamp, and to tell whether two
/// pictures differ: two pictures of a display where nothing moved give the
/// same bytes.
unsafe fn pixels_of(picture: &Picture) -> Res<Vec<u8>> {
    match pixels_copied(picture) {
        Some(pixels) => Ok(pixels),
        None => pixels_drawn(picture),
    }
}

/// Pixels (top-down BGRA) as a picture again: to save them with save_png, or
/// to show them. `like` is the picture the pixels came from, for its
/// colours. Without one they count as plain RGB.
unsafe fn picture_from(width: usize, height: usize, bgra: &[u8], like: Option<&Picture>) -> Res<Picture> {
    if width == 0 || height == 0 || bgra.len() < width * height * 4 {
        return Err("there are no pixels to make a picture of".to_string());
    }
    // a copy of the pixels, which the picture keeps
    let data = Cf(CFDataCreate(null(), bgra.as_ptr(), (width * height * 4) as isize));
    let provider = Cf(CGDataProviderCreateWithCFData(data.0));
    let plain = Cf(CGColorSpaceCreateDeviceRGB());
    let own = like.map_or(null_mut(), |picture| CGImageGetColorSpace(picture.0));
    let image = CGImageCreate(width, height, 8, 32, width * 4, if own.is_null() { plain.0 } else { own }, BGRA,
                              provider.0, null(), false, 0);
    if image.is_null() { Err("macOS made no picture of the pixels".to_string()) } else { Ok(Picture(image)) }
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

/// The agent's mouse. macOS has one real pointer and it stays with you, so
/// this one is only drawn: over the picture in the viewer, and into pictures
/// that leave the program. It is the browser agent's own arrow, the one
/// AutoCua/browser/connector/glow puts on a page: only its cursor, none of
/// its glow. A hollow arrow with a lavender outline and a soft shadow.
/// The numbers are those of glow.js (`cursorSvg`, TIP) and of glow.css
/// (.AutoCua-cursor and the svg in it).
const MOUSE_UNITS: f64 = 32.0; // the arrow is drawn in a square of 32 units
const MOUSE_BOX: f64 = 23.0; // that square is 23 points wide on a page
const MOUSE_TIP: f64 = 2.0; // the tip is 2 points in from the square's top left
const MOUSE_LINE: f64 = 2.5; // how wide the outline is, in the 32 units
/// The outline's three colours, from its top left to its bottom right:
/// #b6a3eb, #8f7bc9 and #d6ccf5. On a page they keep changing places. Here
/// they stay as the page starts them.
const MOUSE_COLOURS: [[f64; 3]; 3] = [[182.0, 163.0, 235.0], [143.0, 123.0, 201.0], [214.0, 204.0, 245.0]];
/// Its shadow: 1 point to the right and 3 down, soft over 8, black at a
/// quarter. The page says 4px for the softness: there that is a standard
/// deviation, and Core Graphics takes about twice that as its blur.
const MOUSE_SHADOW: (f64, f64, f64, f64) = (1.0, 3.0, 8.0, 0.25);
/// A dark tint behind the arrow and a little around it, so that its pale
/// outline stands out on a light page too. The browser's cursor has none:
/// it is this file's own. It is the arrow's whole shape in black, at this
/// share of full black, made this many points bigger all around, its edge
/// then made soft over this many points (a standard deviation).
const MOUSE_TINT: (f64, f64, f64) = (0.2, 3.0, 3.0);
const MOUSE_ROOM: f64 = 16.0; // points kept free around the square, for the shadow and the tint
const ROUND: i32 = 1; // kCGLineJoinRound and kCGLineCapRound
const PAST_BOTH_ENDS: u32 = 1 | 2; // kCGGradientDrawsBeforeStartLocation and AfterEndLocation
const FILL_AND_LINE: i32 = 3; // kCGPathFillStroke

/// Where the agent's mouse is: the point of the second display where it
/// last clicked, ended a drag or turned the wheel, or where --mouse put it,
/// counted from that display's top left. None before the first of those.
static POINTER: Mutex<Option<Point>> = Mutex::new(None);
/// The mouse changed and the viewer has not drawn it there yet.
static POINTER_MOVED: AtomicBool = AtomicBool::new(false);

/// Say where the agent's mouse is now, or None to take it away. From any
/// thread: the viewer draws it there within a quarter of a second.
fn set_pointer(at: Option<Point>) {
    *POINTER.lock().unwrap() = at;
    POINTER_MOVED.store(true, Ordering::SeqCst);
}

/// The agent's mouse as pixels (top-down BGRA, the colours already
/// multiplied by alpha, see-through around the arrow): a square, `scale`
/// pixels to a point, with MOUSE_ROOM points around the arrow's own square.
/// Gives the pixels and how many the square has along a side.
unsafe fn mouse_art(scale: f64) -> Res<(Vec<u8>, usize)> {
    let side = ((MOUSE_BOX + 2.0 * MOUSE_ROOM) * scale).ceil() as usize;
    let space = Cf(CGColorSpaceCreateDeviceRGB());
    // glow.js: M8 4 C5 2 3 3 3 7 L4 24 C4 28 7 28 9 25 L13 20 C14 19 15 18 17 18 L24 18 C28 18 29 15 25 13 Z
    let outline = Cf(CGPathCreateMutable());
    CGPathMoveToPoint(outline.0, null(), 8.0, 4.0);
    CGPathAddCurveToPoint(outline.0, null(), 5.0, 2.0, 3.0, 3.0, 3.0, 7.0);
    CGPathAddLineToPoint(outline.0, null(), 4.0, 24.0);
    CGPathAddCurveToPoint(outline.0, null(), 4.0, 28.0, 7.0, 28.0, 9.0, 25.0);
    CGPathAddLineToPoint(outline.0, null(), 13.0, 20.0);
    CGPathAddCurveToPoint(outline.0, null(), 14.0, 19.0, 15.0, 18.0, 17.0, 18.0);
    CGPathAddLineToPoint(outline.0, null(), 24.0, 18.0);
    CGPathAddCurveToPoint(outline.0, null(), 28.0, 18.0, 29.0, 15.0, 25.0, 13.0);
    CGPathCloseSubpath(outline.0);

    // The arrow's whole shape, the line and what the line goes around, for the tint. A
    // wider line makes it bigger all around: the line is counted in the arrow's 32 units.
    let (share, bigger, soft) = MOUSE_TINT;
    let mut shape = vec![0u8; side * side * 4];
    let made = arrow_canvas(&mut shape, side, scale, space.0)?;
    CGContextAddPath(made.0, outline.0);
    CGContextSetLineWidth(made.0, MOUSE_LINE + 2.0 * bigger * MOUSE_UNITS / MOUSE_BOX);
    CGContextSetLineJoin(made.0, ROUND);
    CGContextSetRGBFillColor(made.0, 0.0, 0.0, 0.0, 1.0);
    CGContextSetRGBStrokeColor(made.0, 0.0, 0.0, 0.0, 1.0);
    CGContextDrawPath(made.0, FILL_AND_LINE);
    drop(made); // the canvas drew straight into `shape`

    let mut pixels = vec![0u8; side * side * 4];
    let made = arrow_canvas(&mut pixels, side, scale, space.0)?;
    let canvas = made.0;
    // The shadow falls from all that is drawn until the layer ends. It is given in the
    // canvas's own pixels, which count from the bottom left: down is below zero.
    let (right, down, blur, dark) = MOUSE_SHADOW;
    let black = [0.0, 0.0, 0.0, dark];
    let shadow = Cf(CGColorCreate(space.0, black.as_ptr()));
    CGContextSetShadowWithColor(canvas, Size { width: right * scale, height: -down * scale }, blur * scale, shadow.0);
    CGContextBeginTransparencyLayer(canvas, null_mut());
    CGContextSaveGState(canvas);
    // The line itself becomes the shape that is filled, with the three colours across it.
    CGContextAddPath(canvas, outline.0);
    CGContextSetLineWidth(canvas, MOUSE_LINE);
    CGContextSetLineJoin(canvas, ROUND);
    CGContextSetLineCap(canvas, ROUND);
    CGContextReplacePathWithStrokedPath(canvas);
    CGContextClip(canvas);
    let around = CGPathGetPathBoundingBox(outline.0);
    let mut colours = [1.0f64; 12]; // red, green, blue and alpha of each: alpha stays 1
    for (n, colour) in MOUSE_COLOURS.iter().enumerate() {
        for (part, value) in colour.iter().enumerate() {
            colours[n * 4 + part] = value / 255.0;
        }
    }
    let places = [0.0, 0.5, 1.0];
    let flow = Cf(CGGradientCreateWithColorComponents(space.0, colours.as_ptr(), places.as_ptr(), places.len()));
    let far = Point { x: around.origin.x + around.size.width, y: around.origin.y + around.size.height };
    CGContextDrawLinearGradient(canvas, flow.0, around.origin, far, PAST_BOTH_ENDS);
    CGContextRestoreGState(canvas);
    CGContextEndTransparencyLayer(canvas);
    drop(made); // the canvas drew straight into `pixels`

    // The tint goes under the arrow: the bigger shape with a soft edge, in black. Black
    // adds nothing to the colours, which are multiplied by alpha: only the alpha grows.
    let covered: Vec<f32> = shape.chunks_exact(4).map(|pixel| pixel[3] as f32 / 255.0).collect();
    for (pixel, here) in pixels.chunks_exact_mut(4).zip(blurred(&covered, side, soft * scale)) {
        let (tint, alpha) = ((share * here as f64 * 255.0).round() as u32, pixel[3] as u32);
        pixel[3] = (alpha + (tint.min(255) * (255 - alpha) + 127) / 255).min(255) as u8;
    }
    Ok((pixels, side))
}

/// A canvas over `pixels`, a square of `side` pixels (top-down BGRA), that
/// counts in the arrow's own 32 units, from the arrow's top left with y
/// going down. It draws straight into the pixels.
unsafe fn arrow_canvas(pixels: &mut [u8], side: usize, scale: f64, space: Id) -> Res<Cf> {
    if pixels.len() < side * side * 4 {
        return Err("could not draw the agent's mouse".to_string());
    }
    let canvas = Cf(CGBitmapContextCreate(pixels.as_mut_ptr() as *mut c_void, side, side, 8, side * 4, space, BGRA));
    if canvas.0.is_null() {
        return Err("could not draw the agent's mouse".to_string());
    }
    CGContextTranslateCTM(canvas.0, 0.0, side as f64); // a canvas counts from its bottom left
    CGContextScaleCTM(canvas.0, scale, -scale);
    CGContextTranslateCTM(canvas.0, MOUSE_ROOM, MOUSE_ROOM);
    CGContextScaleCTM(canvas.0, MOUSE_BOX / MOUSE_UNITS, MOUSE_BOX / MOUSE_UNITS);
    Ok(canvas)
}

/// A square of values with every edge in it made soft: each value becomes
/// the mean of those around it, the near ones counting most (a Gaussian
/// blur, `spread` pixels its standard deviation). Beyond the square counts
/// as nothing.
fn blurred(values: &[f32], side: usize, spread: f64) -> Vec<f32> {
    if !(spread > 0.0) || values.len() < side * side {
        return values.to_vec();
    }
    let reach = (spread * 3.0).ceil() as isize;
    let weights: Vec<f32> = (-reach..=reach).map(|n| (-((n * n) as f64) / (2.0 * spread * spread)).exp() as f32).collect();
    let total: f32 = weights.iter().sum();
    let side = side as isize;
    let pass = |from: &[f32], along_a_row: bool| {
        let mut to = vec![0f32; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                let mut sum = 0f32;
                for (n, weight) in weights.iter().enumerate() {
                    let step = n as isize - reach;
                    let (near_x, near_y) = if along_a_row { (x + step, y) } else { (x, y + step) };
                    if near_x >= 0 && near_y >= 0 && near_x < side && near_y < side {
                        sum += from[(near_y * side + near_x) as usize] * weight;
                    }
                }
                to[(y * side + x) as usize] = sum / total;
            }
        }
        to
    };
    pass(&pass(values, true), false)
}

/// Draw the agent's mouse into pixels (top-down BGRA), its tip at the point
/// `at` of the display. `scale` is how many pixels of the picture make one
/// point: 1, or 2 for a Retina picture. Parts that fall outside the picture
/// are left out.
unsafe fn stamp_pointer(bgra: &mut [u8], width: usize, height: usize, at: Point, scale: f64) -> Res<()> {
    if bgra.len() < width * height * 4 || !(scale > 0.0 && scale <= 8.0) {
        return Err("there is no picture to draw the agent's mouse into".to_string());
    }
    let (art, side) = mouse_art(scale)?;
    // Where the art's top left is in the picture: the arrow's square is MOUSE_TIP in
    // from the tip, and the art has MOUSE_ROOM around that square.
    let left = ((at.x - MOUSE_TIP - MOUSE_ROOM) * scale).round() as i64;
    let top = ((at.y - MOUSE_TIP - MOUSE_ROOM) * scale).round() as i64;
    for row in 0..side {
        let y = top + row as i64;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for column in 0..side {
            let x = left + column as i64;
            let from = &art[(row * side + column) * 4..][..4];
            let cover = from[3] as u32; // how much of this pixel the mouse covers, of 255
            if x < 0 || x >= width as i64 || cover == 0 {
                continue;
            }
            let pixel = &mut bgra[(y as usize * width + x as usize) * 4..][..4];
            for part in 0..4 {
                // the mouse's colours are multiplied by its alpha already: it goes over what is there
                pixel[part] = (from[part] as u32 + (pixel[part] as u32 * (255 - cover) + 127) / 255).min(255) as u8;
            }
        }
    }
    Ok(())
}

/// A picture of the display with the agent's mouse drawn into it. None when
/// the mouse has no place yet: the picture is good as it is. `wide` is how
/// many points wide the display is, which says how many pixels a point has
/// in the picture.
unsafe fn mouse_into(picture: &Picture, wide: f64) -> Res<Option<Picture>> {
    let Some(at) = *POINTER.lock().unwrap() else { return Ok(None) };
    let (width, height) = picture.size();
    let mut pixels = pixels_of(picture)?;
    stamp_pointer(&mut pixels, width, height, at, width as f64 / wide.max(1.0))?;
    Ok(Some(picture_from(width, height, &pixels, Some(picture))?))
}

const LABEL_HEIGHT: f64 = 13.0; // the numbers on the scan's picture, in points

/// A picture of the display with the scan's elements numbered on it, the
/// way the agent's screenshot marks them: a magenta rectangle around each,
/// 2 pixels thick, and its number in bold in its top left corner. The
/// picture is in the display's own pixels: each place is moved by the
/// display's origin and multiplied by the pixels a point has there (1, or 2
/// on a Retina display, where the line and the numbers are twice as big too).
unsafe fn draw_boxes(picture: &Picture, bounds: &Rect, items: &[Found]) -> Res<Picture> {
    let (width, height) = picture.size();
    let scale = width as f64 / bounds.size.width.max(1.0);
    let space = Cf(CGColorSpaceCreateDeviceRGB());
    let made = Cf(CGBitmapContextCreate(null_mut(), width, height, 8, 0, space.0, NO_SEE_THROUGH));
    let canvas = made.0;
    if canvas.is_null() {
        return Err(format!("could not make a {width}x{height} picture"));
    }
    let whole = Rect { origin: Point::default(), size: Size { width: width as f64, height: height as f64 } };
    CGContextDrawImage(canvas, whole, picture.0);
    CGContextSetRGBStrokeColor(canvas, 1.0, 0.0, 1.0, 1.0);
    CGContextSetRGBFillColor(canvas, 1.0, 0.0, 1.0, 1.0); // the numbers take the canvas's fill color

    let face = Cf(cf_string("Arial-BoldMT"));
    let font = Cf(CTFontCreateWithName(face.0, LABEL_HEIGHT * scale, null()));
    let keys = [FONT_OF_TEXT as *const c_void, COLOR_OF_TEXT_FROM_CANVAS as *const c_void];
    let values = [font.0 as *const c_void, TRUE as *const c_void];
    let look = Cf(CFDictionaryCreate(null(), keys.as_ptr(), values.as_ptr(), 2, &KEYS_ARE_OBJECTS,
                                     &VALUES_ARE_OBJECTS));
    let ascent = CTFontGetAscent(font.0);

    for (n, item) in items.iter().enumerate() {
        let left = ((item.place.origin.x - bounds.origin.x) * scale).round();
        let top = ((item.place.origin.y - bounds.origin.y) * scale).round();
        let (w, h) = ((item.place.size.width * scale).round(), (item.place.size.height * scale).round());
        // The canvas counts from its bottom left corner, the screens from their top left.
        // A line is drawn around its middle: half its thickness in, and it stays inside the element.
        let line = 2.0 * scale;
        let edge = Rect { origin: Point { x: left + line / 2.0, y: height as f64 - top - h + line / 2.0 },
                          size: Size { width: (w - line).max(0.0), height: (h - line).max(0.0) } };
        CGContextStrokeRectWithWidth(canvas, edge, line);
        let text = Cf(cf_string(&format!("[{}]", n + 1)));
        let styled = Cf(CFAttributedStringCreate(null(), text.0, look.0));
        let label = Cf(CTLineCreateWithAttributedString(styled.0));
        CGContextSetTextPosition(canvas, left + 3.0 * scale, height as f64 - top - 2.0 * scale - ascent);
        CTLineDraw(label.0, canvas);
    }
    let image = CGBitmapContextCreateImage(canvas);
    if image.is_null() { Err("could not make the numbered picture".to_string()) } else { Ok(Picture(image)) }
}

/// The latest picture of the second display, shared with the viewer.
struct Frames {
    display: u32,
    stop: AtomicBool,
    latest: Mutex<Latest>,
}

#[derive(Default)]
struct Latest {
    /// The picture as macOS gave it: what the viewer's window shows.
    picture: Option<Arc<Picture>>,
    /// The same picture as top-down BGRA bytes, `width` by `height` pixels.
    pixels: Option<Arc<Vec<u8>>>,
    width: usize,
    height: usize,
    /// One more with every picture that differs from the one before it.
    count: u64,
    /// One more with every picture taken, new or not.
    taken: u64,
    /// Why the last try gave no picture. None again once one comes.
    error: Option<String>,
}

/// Runs on a thread of its own: one picture of the display every
/// FRAME_INTERVAL. A picture that shows the same as the one before is
/// dropped, so `count` goes up only when something on the display changed.
/// macOS lets any thread take pictures. The window is the main thread's.
/// Seen: a picture takes about 25 ms, nearly all of it waiting for macOS,
/// and the whole loop uses under 1% of one core.
unsafe fn capture_loop(frames: &Frames) {
    while !frames.stop.load(Ordering::SeqCst) {
        let pool = objc_autoreleasePoolPush();
        let taken = capture(frames.display).and_then(|picture| Ok((pixels_of(&picture)?, picture)));
        let before = frames.latest.lock().unwrap().pixels.clone();
        match taken {
            Ok((pixels, picture)) => {
                let new = before.map_or(true, |old| *old != pixels); // compared before the lock is taken
                let mut latest = frames.latest.lock().unwrap();
                latest.taken += 1;
                latest.error = None;
                if new {
                    (latest.width, latest.height) = picture.size();
                    latest.picture = Some(Arc::new(picture));
                    latest.pixels = Some(Arc::new(pixels));
                    latest.count += 1;
                }
            }
            Err(e) => frames.latest.lock().unwrap().error = Some(e),
        }
        objc_autoreleasePoolPop(pool);
        thread::sleep(FRAME_INTERVAL);
    }
}

// ---------------------------------------------------------------- input without your mouse or keyboard

/// What sends the agent's input. Made once, for the apps it may send to.
struct Input {
    /// The apps input may go to, each with its birth: push a process here
    /// when an app is started. Nothing is ever sent to a process that is not
    /// in this list, nor to a later one that got the id of one that ended.
    apps: Vec<Process>,
    /// The second display: the agent's own pointer is kept in its points.
    display: u32,
    /// A private event source for the mouse and the wheel: its events carry
    /// none of the keys you hold down. Key presses get a new one each.
    source: Id,
    set_window_location: SetWindowLocation,
    post_record: PostRecord,
    serial_of_pid: SerialOfPid,
    /// Per app, the window that was made key inside it.
    key_windows: Vec<(i32, u32)>,
}

impl Drop for Input {
    fn drop(&mut self) {
        unsafe { CFRelease(self.source) }
    }
}

/// Whether an app believes it is the active one. None when it does not say.
/// After `prepare` it does, while macOS still has your app in front.
unsafe fn believes_active(pid: i32) -> Option<bool> {
    if AXIsProcessTrusted() == 0 {
        return None;
    }
    ax_bool(ax_app(pid).0, "AXFrontmost")
}

/// A shortcut that would reach beyond the agent's own apps, with what it
/// does. The app would take it from its own menu bar and act on your whole
/// session, so it is never sent. Not tried, on purpose: this function is
/// only ever asked. A key counts as H or Q when it gives that letter with
/// nothing held in your keyboard layout, or when it is the key where that
/// letter is on every keyboard: a menu goes by the letter the key gives with
/// Cmd held, and in some layouts that is another one. A key whose letter is
/// not known (Return, Tab, the space bar, a key macOS gave no letter for) is
/// refused with Cmd and one more modifier: it cannot be told from those.
/// Only run on this Mac's keyboard layout, where both ways name the same keys.
/// Hide Others and Force Quit are refused with Shift or Control held too:
/// the Apple menu has a second Force Quit for that.
/// Not covered: the shortcuts an app takes for its Services menu (Shift+Cmd+Y
/// makes a Sticky, Shift+Cmd+M opens a man page in Terminal). They would
/// start another app in front of yours. No step of this file sends one.
unsafe fn reaches_your_side(keycode: u16, flags: u64) -> Option<&'static str> {
    let held = flags & (SHIFT | CONTROL | OPTION | COMMAND);
    let option_and_command = held & (OPTION | COMMAND) == OPTION | COMMAND;
    let letter = key_letter(keycode);
    let is = |name: &str, place: u16| letter.as_deref() == Some(name) || keycode == place;
    if is("H", KEY_H) && option_and_command {
        return Some("Hide Others hides every app of yours");
    }
    if is("Q", KEY_Q) && held & COMMAND != 0 && held != COMMAND {
        return Some("it logs you out or locks your screen");
    }
    if keycode == KEY_ESCAPE && option_and_command {
        return Some("it opens Force Quit on your screen");
    }
    if letter.is_none() && held & COMMAND != 0 && held != COMMAND {
        return Some("the letter of its key is not known, so it cannot be told from Hide Others or Log Out");
    }
    None
}

/// The letter a key gives with nothing held, in your keyboard layout, as a
/// capital: "A" for key 0. It is how a menu names the key of its shortcut.
/// macOS works the letters out on the main thread only (seen: asked on
/// another thread, key 0 came back with no letter). So the letters of all
/// keys are read by the first call and kept, and that first call has to come
/// from the main thread.
unsafe fn key_letter(keycode: u16) -> Option<String> {
    static LETTERS: OnceLock<Vec<Option<String>>> = OnceLock::new();
    let read = |key: u16| {
        // a new source for every key: a used one puts its last flags into what it makes
        let source = Cf(CGEventSourceCreate(PRIVATE_STATE));
        let event = Cf(CGEventCreateKeyboardEvent(source.0, key, true));
        if event.0.is_null() {
            return None;
        }
        let (mut units, mut count) = ([0u16; 4], 0usize);
        CGEventKeyboardGetUnicodeString(event.0, units.len(), &mut count, units.as_mut_ptr());
        let letter = String::from_utf16_lossy(&units[..count.min(units.len())]).to_uppercase();
        if letter.trim().is_empty() { None } else { Some(letter) }
    };
    LETTERS.get_or_init(|| (0..128).map(read).collect()).get(keycode as usize).cloned().flatten()
}

/// The key that gives a letter in your keyboard layout: key 0 for "A" on a
/// QWERTY keyboard, key 12 on an AZERTY one.
unsafe fn key_for(letter: &str) -> Res<u16> {
    (0..128).find(|&key| key_letter(key).as_deref() == Some(letter))
        .ok_or(format!("your keyboard layout has no key for {}", quote(letter)))
}

impl Input {
    unsafe fn new(display: u32, apps: &[Process]) -> Res<Input> {
        let no_click = "a click cannot reach an app that is not in front";
        let no_key_window = "a window cannot be made key inside its app";
        let set_window_location = needed_function("CGEventSetWindowLocation", no_click)?;
        let post_record = needed_function("SLPSPostEventRecordTo", no_key_window)?;
        let serial_of_pid = needed_function("GetProcessForPID", no_key_window)?;
        let source = CGEventSourceCreate(PRIVATE_STATE);
        if source.is_null() {
            return Err("macOS gave no event source".to_string());
        }
        Ok(Input {
            apps: apps.to_vec(),
            display,
            source,
            set_window_location: std::mem::transmute::<*mut c_void, SetWindowLocation>(set_window_location),
            post_record: std::mem::transmute::<*mut c_void, PostRecord>(post_record),
            serial_of_pid: std::mem::transmute::<*mut c_void, SerialOfPid>(serial_of_pid),
            key_windows: Vec::new(),
        })
    }

    /// Whether a process id is that of one of the agent's apps.
    fn knows(&self, pid: i32) -> bool {
        self.apps.iter().any(|app| app.pid == pid)
    }

    /// An error in words when a process is not one of the agent's apps, or
    /// when that app has ended: its id may be another process's by now.
    /// Everything that sends asks this first.
    unsafe fn ours(&self, pid: i32) -> Res<()> {
        match self.apps.iter().find(|app| app.pid == pid) {
            Some(app) if alive(app) => Ok(()),
            Some(_) => Err(format!("process {pid} has ended: nothing is sent to it")),
            None => Err(format!("process {pid} is not one of the agent's apps")),
        }
    }

    /// The front-most window of our apps that holds a point. Where two of
    /// them overlap, the one macOS draws on top gets the click, as it would
    /// from a mouse. A window of another app that lies over it does not
    /// count: the events go to the app by its process id, so they arrive
    /// under a window that covers it.
    unsafe fn window_at(&self, x: f64, y: f64) -> Res<Window> {
        windows_on_screen().into_iter()
            .find(|w| self.knows(w.pid) && holds(&w.frame, x, y) && !is_menu_bar_piece(w))
            .ok_or(format!("none of the apps has a window at {x:.0},{y:.0}"))
    }

    /// The front-most ordinary window an app has on screen.
    unsafe fn front_window_of(&self, pid: i32) -> Option<Window> {
        windows_on_screen().into_iter().find(|w| w.pid == pid && w.layer == 0 && !is_menu_bar_piece(w))
    }

    /// Make a window key inside its own app. Three event records go to the
    /// app. The first says "you are in focus, with this window". The other
    /// two are a mouse down and up for that window that land nowhere:
    /// without them the first real click only wakes the window and is lost.
    /// The app then believes it is active. macOS does not: no process comes
    /// in front, no window is raised, and your typing still goes to your own
    /// app.
    unsafe fn prepare(&mut self, pid: i32, window: u32) -> Res<()> {
        self.ours(pid)?;
        let mut serial = Serial::default();
        if (self.serial_of_pid)(pid, &mut serial) != 0 {
            return Err(format!("process {pid} is not an app macOS knows"));
        }
        let mut record = [0u8; 0xF8];
        record[0x04] = 0xF8; // the length of the record
        record[0x3C..0x40].copy_from_slice(&window.to_le_bytes());
        let mut focus = record;
        focus[0x08] = 0x0D; // the kind: focus
        focus[0x8A] = 0x01; // 1 gives the focus, 2 takes it away
        let mut failed = (self.post_record)(&serial, focus.as_ptr());
        record[0x3A] = 0x10;
        record[0x20..0x30].fill(0xFF); // a place that is in no window
        for mouse in [LEFT_DOWN as u8, LEFT_UP as u8] {
            record[0x08] = mouse;
            failed |= (self.post_record)(&serial, record.as_ptr());
        }
        if failed != 0 {
            return Err(format!("macOS did not take the records for window {window} (error {failed})"));
        }
        self.key_windows.retain(|&(app, _)| app != pid);
        self.key_windows.push((pid, window));
        sleep(0.05); // the app takes them in
        Ok(())
    }

    /// Before a click: prepare its window when it needs it, which is when it
    /// is not the one that was made key in its app, or when the app says it
    /// is not active any more. Menus and other popups (layer above 0) are
    /// left alone: their app is following them already.
    unsafe fn ready(&mut self, window: &Window) -> Res<()> {
        self.ours(window.pid)?;
        if window.layer != 0 {
            return Ok(());
        }
        let known = self.key_windows.contains(&(window.pid, window.id));
        if known && believes_active(window.pid) != Some(false) {
            return Ok(());
        }
        self.prepare(window.pid, window.id)
    }

    /// Before a key: make sure the app believes it is active, with its
    /// front-most window as the key one. Seen in a TextEdit that had never
    /// been clicked: typed text, Return, the arrows and Cmd+arrows arrived
    /// (its text view takes those itself), but Cmd+A selected nothing. A
    /// shortcut is taken by the menu bar, and that only listens in an app
    /// that believes it is active.
    unsafe fn ready_for_keys(&mut self, pid: i32) -> Res<()> {
        self.ours(pid)?;
        let prepared = self.key_windows.iter().any(|&(app, _)| app == pid);
        match believes_active(pid) {
            Some(true) => return Ok(()),
            None if prepared => return Ok(()),
            _ => {}
        }
        match self.front_window_of(pid) {
            Some(window) => self.prepare(pid, window.id),
            None => Ok(()), // no window on screen: the key goes out all the same
        }
    }

    /// One event of the left mouse button for a window, handed to the
    /// window's process. It says no modifier key is held. One that said
    /// Shift was tried for a Shift+click, four ways: TextEdit moved the
    /// caret and stretched nothing.
    unsafe fn mouse(&self, kind: u32, x: f64, y: f64, window: &Window, clicks: i64) -> Res<()> {
        self.ours(window.pid)?;
        let event = CGEventCreateMouseEvent(self.source, kind, Point { x, y }, LEFT_BUTTON);
        if event.is_null() {
            return Err("macOS made no mouse event".to_string());
        }
        CGEventSetIntegerValueField(event, FIELD_CLICKS, clicks);
        CGEventSetIntegerValueField(event, FIELD_BUTTON, LEFT_BUTTON as i64);
        CGEventSetIntegerValueField(event, FIELD_WINDOW, window.id as i64);
        // the point inside the window, from the window's top left
        (self.set_window_location)(event, Point { x: x - window.frame.origin.x, y: y - window.frame.origin.y });
        CGEventSetFlags(event, 0);
        CGEventPostToPid(window.pid, event);
        CFRelease(event);
        Ok(())
    }

    /// The agent acted at a point: its own pointer, the one the viewer
    /// draws, goes there, and the keeper looks often for a window that
    /// opens now.
    unsafe fn acted_at(&self, x: f64, y: f64) {
        let screen = CGDisplayBounds(self.display).origin;
        set_pointer(Some(Point { x: x - screen.x, y: y - screen.y }));
        expect_window();
    }

    /// `clicks` clicks of the left button at a point. Gives back the window
    /// they went to. There is no right click in this file. It was seen to
    /// open the menu, but while a menu of an app that is not in front is
    /// open, macOS sends YOUR keyboard to that app (seen from 60 ms after
    /// the click until the menu closed).
    unsafe fn press(&mut self, x: f64, y: f64, clicks: i64) -> Res<Window> {
        let window = self.window_at(x, y)?;
        self.ready(&window)?;
        for click in 1..=clicks {
            self.mouse(LEFT_DOWN, x, y, &window, click)?;
            sleep(0.03); // none is needed (seen with 0), a real click has one
            self.mouse(LEFT_UP, x, y, &window, click)?;
            if click < clicks {
                sleep(0.08); // well inside the double click time
            }
        }
        self.acted_at(x, y);
        Ok(window)
    }

    /// A left click at a point. Your pointer does not move.
    unsafe fn click_at(&mut self, x: f64, y: f64) -> Res<Window> {
        self.press(x, y, 1)
    }

    /// A double click at a point: selects a word in a text.
    unsafe fn double_click_at(&mut self, x: f64, y: f64) -> Res<Window> {
        self.press(x, y, 2)
    }

    /// A triple click at a point: selects a whole line of a text.
    unsafe fn triple_click_at(&mut self, x: f64, y: f64) -> Res<Window> {
        self.press(x, y, 3)
    }

    /// The app of the agent that your keyboard goes to at this moment, when
    /// one has it: an open menu takes it, and what you type then goes into
    /// the menu. None is the good answer.
    unsafe fn has_your_keyboard(&self) -> Option<i32> {
        keyboard_process().filter(|&pid| self.knows(pid))
    }

    /// When an app of the agent has your keyboard: close what took it with
    /// the Escape key, three times at most, and wait until your own app has
    /// it again. Gives back whether there was something to close. Nothing
    /// is brought in front for it. (To ask macOS for it directly was tried:
    /// SLPSReleaseKeyFocus from this program gave 0 and changed nothing.)
    /// Seen in pieces: Escape closed the menu, and your keyboard was with
    /// your app again 275 ms later. As a whole it has not run: no step of
    /// this file opens a menu.
    unsafe fn give_keyboard_back(&mut self) -> Res<bool> {
        let mut had_it = false;
        for _ in 0..3 {
            let Some(pid) = self.has_your_keyboard() else { break };
            had_it = true;
            self.press_key(pid, KEY_ESCAPE, 0, &[])?;
            let deadline = Instant::now() + Duration::from_millis(600);
            while self.has_your_keyboard() == Some(pid) && Instant::now() < deadline {
                sleep(0.01);
            }
        }
        match self.has_your_keyboard() {
            Some(pid) => Err(format!("process {pid} still has your keyboard")),
            None => Ok(had_it),
        }
    }

    /// Press at one point, move to another in small steps, let go there.
    /// Both points count for the window under the first one.
    unsafe fn drag(&mut self, from_x: f64, from_y: f64, to_x: f64, to_y: f64) -> Res<Window> {
        let window = self.window_at(from_x, from_y)?;
        self.ready(&window)?;
        self.mouse(LEFT_DOWN, from_x, from_y, &window, 1)?;
        sleep(0.05);
        let steps = 12;
        for step in 1..=steps {
            let part = step as f64 / steps as f64;
            let (x, y) = (from_x + (to_x - from_x) * part, from_y + (to_y - from_y) * part);
            self.mouse(LEFT_DRAGGED, x, y, &window, 1)?;
            sleep(0.015);
        }
        self.mouse(LEFT_UP, to_x, to_y, &window, 1)?;
        self.acted_at(to_x, to_y);
        Ok(window)
    }

    /// Turn the mouse wheel at a point by lines: down when `lines` is above
    /// zero. The window needs no prepare for it.
    unsafe fn scroll_at(&mut self, x: f64, y: f64, lines: i32) -> Res<Window> {
        let window = self.window_at(x, y)?;
        self.ours(window.pid)?;
        let event = CGEventCreateScrollWheelEvent2(self.source, WHEEL_LINES, 1, -lines, 0, 0);
        if event.is_null() {
            return Err("macOS made no wheel event".to_string());
        }
        CGEventSetLocation(event, Point { x, y });
        CGEventSetIntegerValueField(event, FIELD_WINDOW, window.id as i64);
        (self.set_window_location)(event, Point { x: x - window.frame.origin.x, y: y - window.frame.origin.y });
        CGEventSetFlags(event, 0);
        CGEventPostToPid(window.pid, event);
        CFRelease(event);
        self.acted_at(x, y);
        Ok(window)
    }

    /// One key, down and up, for an app, with a text it carries when `units`
    /// is not empty. Every press gets an event source of its own. Seen: a
    /// source keeps the flags of the last event it posted (they reach it
    /// about 14 ms after the post) and puts them into every event it makes
    /// later. After a Left arrow, Cmd+A went out as Cmd+fn+number pad+A and
    /// no menu took it. After Shift+Right, the key A was made as a capital.
    unsafe fn press_key(&self, pid: i32, keycode: u16, flags: u64, units: &[u16]) -> Res<()> {
        self.ours(pid)?;
        let source = Cf(CGEventSourceCreate(PRIVATE_STATE));
        if source.0.is_null() {
            return Err("macOS gave no event source".to_string());
        }
        // both are made before one is posted, so the down cannot get into the up
        let events = [Cf(CGEventCreateKeyboardEvent(source.0, keycode, true)),
                      Cf(CGEventCreateKeyboardEvent(source.0, keycode, false))];
        if events.iter().any(|event| event.0.is_null()) {
            return Err("macOS made no key event".to_string());
        }
        for event in &events {
            // what was asked for and what the key carries by itself, nothing else
            CGEventSetFlags(event.0, flags | CGEventGetFlags(event.0) & KEYS_OWN_FLAGS);
            if !units.is_empty() {
                CGEventKeyboardSetUnicodeString(event.0, units.len(), units.as_ptr());
            }
        }
        for event in &events {
            CGEventPostToPid(pid, event.0);
            sleep(0.003);
        }
        expect_window(); // a key can open a window: the keeper looks often now
        Ok(())
    }

    /// Press and let go one key in an app, with modifier flags: COMMAND for
    /// Cmd, SHIFT | COMMAND for both. It goes to what has the keyboard
    /// inside that app. Your own keyboard is not used and your front app
    /// does not get it. The app works out the letter from the key and the
    /// flags itself (seen: key A with SHIFT typed a capital A, Control+E
    /// went to the end of the line).
    unsafe fn key(&mut self, pid: i32, keycode: u16, flags: u64) -> Res<()> {
        if let Some(why) = reaches_your_side(keycode, flags) {
            return Err(format!("this shortcut is not sent: {why}"));
        }
        self.ready_for_keys(pid)?;
        self.press_key(pid, keycode, flags, &[])
    }

    /// Type a text into an app at its caret: any Unicode. The text rides on
    /// key events, up to 20 UTF-16 units each. A line end is the Return key
    /// and a tab the Tab key. The app takes it as typing, so its own
    /// autocorrection applies: with "Capitalise words automatically" on in
    /// System Settings, TextEdit made "turn 1. turn 2." into "Turn 1. Turn 2.".
    unsafe fn type_text(&mut self, pid: i32, text: &str) -> Res<()> {
        self.ready_for_keys(pid)?;
        let mut units: Vec<u16> = Vec::new();
        for letter in text.chars() {
            match letter {
                '\r' => {}
                '\n' | '\t' => {
                    self.typed(pid, &mut units)?;
                    self.press_key(pid, if letter == '\n' { KEY_RETURN } else { KEY_TAB }, 0, &[])?;
                }
                _ => {
                    let mut pair = [0u16; 2];
                    let encoded = letter.encode_utf16(&mut pair);
                    if units.len() + encoded.len() > 20 {
                        self.typed(pid, &mut units)?;
                    }
                    units.extend_from_slice(encoded);
                }
            }
        }
        self.typed(pid, &mut units)
    }

    /// Send the letters gathered so far as one key press, and start again.
    unsafe fn typed(&self, pid: i32, units: &mut Vec<u16>) -> Res<()> {
        if !units.is_empty() {
            self.press_key(pid, TEXT_KEY, 0, units)?;
            units.clear();
        }
        Ok(())
    }

    /// The other way to a shortcut, for an app that does not take the key:
    /// find the entry of the app's own menus that has this shortcut and
    /// press it through Accessibility. No key event is sent and no menu
    /// opens. The app has to believe it is active for this too: in a
    /// TextEdit that had never been clicked, Select All was switched off.
    /// The Apple menu is left out: its entries act on your whole session.
    /// Gives back the entry's title.
    unsafe fn menu_shortcut(&mut self, pid: i32, keycode: u16, flags: u64) -> Res<String> {
        if let Some(why) = reaches_your_side(keycode, flags) {
            return Err(format!("this shortcut is not pressed: {why}"));
        }
        self.ready_for_keys(pid)?;
        let letter = key_letter(keycode).ok_or(format!("key {keycode} has no letter a menu could name"))?;
        let mut held = 0.0;
        for (flag, bit) in [(SHIFT, MENU_SHIFT), (OPTION, MENU_OPTION), (CONTROL, MENU_CONTROL)] {
            if flags & flag != 0 {
                held += bit;
            }
        }
        if flags & COMMAND == 0 {
            held += MENU_NO_COMMAND;
        }
        let app = ax_app(pid);
        let bar = ax_get(app.0, "AXMenuBar").ok_or(format!("process {pid} shows no menu bar to Accessibility"))?;
        let has_it = |entry: Id| {
            ax_text(entry, "AXMenuItemCmdChar").as_deref() == Some(letter.as_str())
                && ax_number(entry, "AXMenuItemCmdModifiers") == Some(held)
        };
        let entry = ax_list(bar.0, "AXChildren").iter().skip(1) // the first is the Apple menu
            .find_map(|menu| ax_find(menu.0, 6, &has_it))
            .ok_or(format!("no menu of process {pid} has this shortcut"))?;
        let title = ax_text(entry.0, "AXTitle").unwrap_or_default();
        if ax_bool(entry.0, "AXEnabled") == Some(false) {
            return Err(format!("the menu entry {} is switched off now", quote(&title)));
        }
        match ax_do(entry.0, "AXPress") {
            0 => Ok(title),
            error => Err(format!("the menu entry {} did not take the press (Accessibility error {error})",
                                 quote(&title))),
        }
    }
}

// ---------------------------------------------------------------- Accessibility, and the agent's scan

/// One attribute of an element. None when it has none of that name.
unsafe fn ax_get(element: Id, name: &str) -> Option<Cf> {
    let attribute = Cf(cf_string(name));
    let mut value: Id = null_mut();
    if AXUIElementCopyAttributeValue(element, attribute.0, &mut value) != 0 || value.is_null() {
        return None;
    }
    Some(Cf(value))
}

unsafe fn ax_text(element: Id, name: &str) -> Option<String> {
    let value = ax_get(element, name)?;
    if CFGetTypeID(value.0) == CFStringGetTypeID() { Some(rust_string(value.0)) } else { None }
}

unsafe fn ax_number(element: Id, name: &str) -> Option<f64> {
    let (value, mut number) = (ax_get(element, name)?, 0f64);
    if CFGetTypeID(value.0) != CFNumberGetTypeID() {
        return None;
    }
    CFNumberGetValue(value.0, NUMBER_F64, &mut number as *mut f64 as *mut c_void);
    Some(number)
}

unsafe fn ax_bool(element: Id, name: &str) -> Option<bool> {
    let value = ax_get(element, name)?;
    if CFGetTypeID(value.0) == CFBooleanGetTypeID() { Some(CFBooleanGetValue(value.0) != 0) } else { None }
}

/// The elements an attribute lists (AXChildren, AXWindows), each one kept.
unsafe fn ax_list(element: Id, name: &str) -> Vec<Cf> {
    let mut items = Vec::new();
    if let Some(list) = ax_get(element, name) {
        if CFGetTypeID(list.0) == CFArrayGetTypeID() {
            for n in 0..CFArrayGetCount(list.0) {
                items.push(Cf(CFRetain(CFArrayGetValueAtIndex(list.0, n))));
            }
        }
    }
    items
}

/// Where an element is, in the points all screens share.
unsafe fn ax_frame(element: Id) -> Option<Rect> {
    let (place, size) = (ax_get(element, "AXPosition")?, ax_get(element, "AXSize")?);
    as_frame(place.0, size.0) // it looks first whether the app gave values of the right kind
}

/// The first element at or under `element` that the test takes, depth first.
unsafe fn ax_find(element: Id, depth: u32, test: &dyn Fn(Id) -> bool) -> Option<Cf> {
    if test(element) {
        return Some(Cf(CFRetain(element)));
    }
    if depth == 0 {
        return None;
    }
    ax_list(element, "AXChildren").iter().find_map(|child| ax_find(child.0, depth - 1, test))
}

/// Every element at or under `element` that the test takes, in reading order.
unsafe fn ax_all(element: Id, depth: u32, test: &dyn Fn(Id) -> bool, found: &mut Vec<Cf>) {
    if test(element) {
        found.push(Cf(CFRetain(element)));
    }
    if depth > 0 {
        for child in ax_list(element, "AXChildren") {
            ax_all(child.0, depth - 1, test, found);
        }
    }
}

unsafe fn ax_role_is(element: Id, role: &str) -> bool {
    ax_text(element, "AXRole").as_deref() == Some(role)
}

/// Set one attribute. Gives back Accessibility's error number, 0 when it took it.
unsafe fn ax_set(element: Id, name: &str, value: Id) -> i32 {
    let attribute = Cf(cf_string(name));
    AXUIElementSetAttributeValue(element, attribute.0, value)
}

/// Do an element's action, "AXPress" for a button. 0 when it took it.
unsafe fn ax_do(element: Id, action: &str) -> i32 {
    let action = Cf(cf_string(action));
    AXUIElementPerformAction(element, action.0)
}

/// An app as Accessibility sees it. A call on it gives up after 3 seconds
/// when the app hangs. Its windows and what is in them do not take that
/// time over: left alone, a call on one of them waits 1.5 seconds (seen).
unsafe fn ax_app(pid: i32) -> Cf {
    let app = Cf(AXUIElementCreateApplication(pid));
    AXUIElementSetMessagingTimeout(app.0, 3.0);
    app
}

/// The selected run of a text element.
unsafe fn ax_selection(text: Id) -> Option<Range> {
    let (value, mut range) = (ax_get(text, "AXSelectedTextRange")?, Range::default());
    if CFGetTypeID(value.0) != AXValueGetTypeID()
        || AXValueGetValue(value.0, AX_RANGE, &mut range as *mut Range as *mut c_void) == 0
    {
        return None;
    }
    Some(range)
}

/// Where on screen a run of a text element is drawn.
unsafe fn ax_bounds_of(text: Id, range: Range) -> Option<Rect> {
    let (attribute, parameter) = (Cf(cf_string("AXBoundsForRange")),
                                  Cf(AXValueCreate(AX_RANGE, &range as *const Range as *const c_void)));
    let mut value: Id = null_mut();
    if AXUIElementCopyParameterizedAttributeValue(text, attribute.0, parameter.0, &mut value) != 0 || value.is_null() {
        return None;
    }
    let (value, mut frame) = (Cf(value), Rect::default());
    if CFGetTypeID(value.0) != AXValueGetTypeID()
        || AXValueGetValue(value.0, AX_RECT, &mut frame as *mut Rect as *mut c_void) == 0
    {
        return None;
    }
    Some(frame)
}

const SCAN_TIMEOUT: f32 = 2.0; // seconds an app gets to answer one question of the scan
const SCAN_MOST: usize = 4000; // elements read in one window before the scan stops there
const SCAN_DEEP: u32 = 60; // levels of children the scan goes down

/// What the scan reads of every element, in one question to its app.
const READ: [&str; 14] = ["AXRole", "AXSubrole", "AXRoleDescription", "AXTitle", "AXDescription", "AXValue", "AXHelp",
                          "AXPosition", "AXSize", "AXEnabled", "AXSelected", "AXChildren", "AXVisibleRows", "AXHeader"];
// Where each of them is in READ, and so in the answer.
const R_ROLE: isize = 0;
const R_SUBROLE: isize = 1;
const R_ROLE_IN_WORDS: isize = 2;
const R_TITLE: isize = 3;
const R_DESCRIPTION: isize = 4;
const R_VALUE: isize = 5;
const R_HELP: isize = 6;
const R_POSITION: isize = 7;
const R_SIZE: isize = 8;
const R_ENABLED: isize = 9;
const R_SELECTED: isize = 10;
const R_CHILDREN: isize = 11;
const R_VISIBLE_ROWS: isize = 12;
const R_HEADER: isize = 13;

/// The roles that are things to click or type in, whatever else they say.
const CLICKABLE: [&str; 17] = ["AXButton", "AXTextField", "AXTextArea", "AXCheckBox", "AXRadioButton", "AXPopUpButton",
                               "AXMenuButton", "AXComboBox", "AXSlider", "AXLink", "AXIncrementor",
                               "AXDisclosureTriangle", "AXMenuItem", "AXMenuBarItem", "AXColorWell", "AXDateField",
                               "AXTimeField"];
/// Whole areas: not things to click. What is in them is read.
const WHOLE_AREAS: [&str; 17] = ["AXWindow", "AXSheet", "AXDrawer", "AXPopover", "AXMenu", "AXMenuBar", "AXGroup",
                                 "AXScrollArea", "AXSplitGroup", "AXLayoutArea", "AXTabGroup", "AXToolbar", "AXList",
                                 "AXTable", "AXOutline", "AXBrowser", "AXWebArea"];

/// One thing to click or type in, found by the scan.
struct Found {
    /// Its Accessibility role without the AX: "Button", "TextArea".
    kind: String,
    /// Its title, else its description, else its value, else its help, else
    /// the first text it holds (a row of a table).
    name: String,
    /// Where it is, in the points all screens share: the part of it its window shows.
    place: Rect,
    /// The app it belongs to, as the very process that was asked, and the window it is in.
    app: Process,
    window: u32,
    /// The element itself, for `press` and `set_value`.
    element: Cf,
}

/// One window on show on the display.
struct Shown {
    window: Window,
    /// The name of its app, as macOS's list of windows has it. The app is not asked for it.
    app: String,
    /// Its title. Only read for the apps the scan may ask.
    title: String,
    /// Whether it belongs to one of the apps the scan may ask.
    ours: bool,
    /// Whether it is a window its app does not list among its windows: an open
    /// menu, a sheet, the list of a combo box.
    popup: bool,
    /// How many of the scan's elements are in it.
    count: usize,
}

/// What one scan gives back.
struct Scan {
    /// The display's place among the screens, in points.
    bounds: Rect,
    /// The windows on show on it, front to back.
    windows: Vec<Shown>,
    /// What there is to click or type in. Number 1 is the first.
    items: Vec<Found>,
    /// What could not be read, in words.
    notes: Vec<String>,
    /// Whether one of the apps builds its elements only when asked: a later read may find more.
    late: bool,
    /// How long the read took.
    took: Duration,
}

/// A value of an answer as a text. None when it is something else: an
/// attribute the element does not have comes back as an error value.
unsafe fn as_text(value: Id) -> Option<String> {
    if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() { None } else { Some(rust_string(value)) }
}

/// The same, with a number as its digits: the value of a slider or a check box.
unsafe fn as_words(value: Id) -> Option<String> {
    if value.is_null() || CFGetTypeID(value) != CFNumberGetTypeID() {
        return as_text(value);
    }
    let mut number = 0f64;
    CFNumberGetValue(value, NUMBER_F64, &mut number as *mut f64 as *mut c_void);
    Some(if number.fract() == 0.0 { format!("{number:.0}") } else { format!("{number:.3}") })
}

unsafe fn as_bool(value: Id) -> Option<bool> {
    if value.is_null() || CFGetTypeID(value) != CFBooleanGetTypeID() {
        return None;
    }
    Some(CFBooleanGetValue(value) != 0)
}

/// A place and a size of an answer as one rectangle.
unsafe fn as_frame(place: Id, size: Id) -> Option<Rect> {
    let kind = AXValueGetTypeID();
    if place.is_null() || size.is_null() || CFGetTypeID(place) != kind || CFGetTypeID(size) != kind {
        return None;
    }
    let mut frame = Rect::default();
    if AXValueGetValue(place, AX_POINT, &mut frame.origin as *mut Point as *mut c_void) == 0
        || AXValueGetValue(size, AX_SIZE, &mut frame.size as *mut Size as *mut c_void) == 0
    {
        return None;
    }
    Some(frame)
}

/// The elements a value of an answer lists (AXChildren). They stay the
/// answer's own. Empty when the value is no list.
unsafe fn as_elements(value: Id) -> Vec<Id> {
    if value.is_null() || CFGetTypeID(value) != CFArrayGetTypeID() {
        return Vec::new();
    }
    let element = AXUIElementGetTypeID();
    (0..CFArrayGetCount(value)).map(|n| CFArrayGetValueAtIndex(value, n))
        .filter(|&item| !item.is_null() && CFGetTypeID(item) == element).collect()
}

/// Names as macOS takes a list of them: a CFArray of CFStrings.
unsafe fn cf_names(names: &[&str]) -> Cf {
    let texts: Vec<Cf> = names.iter().map(|name| Cf(cf_string(name))).collect();
    let raw: Vec<*const c_void> = texts.iter().map(|text| text.0 as *const c_void).collect();
    Cf(CFArrayCreate(null(), raw.as_ptr(), raw.len() as isize, &ARRAY_OF_OBJECTS))
}

/// The part of one rectangle that lies inside another. Its size is 0 when there is none.
fn cut(a: &Rect, b: &Rect) -> Rect {
    let (left, top) = (a.origin.x.max(b.origin.x), a.origin.y.max(b.origin.y));
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    let size = Size { width: (right - left).max(0.0), height: (bottom - top).max(0.0) };
    Rect { origin: Point { x: left, y: top }, size }
}

/// Whether two rectangles are the same, to the point.
fn same_place(a: &Rect, b: &Rect) -> bool {
    (a.origin.x - b.origin.x).abs() < 1.5 && (a.origin.y - b.origin.y).abs() < 1.5
        && (a.size.width - b.size.width).abs() < 1.5 && (a.size.height - b.size.height).abs() < 1.5
}

/// Whether one rectangle lies inside another, to the point.
fn inside(a: &Rect, b: &Rect) -> bool {
    a.origin.x > b.origin.x - 1.5 && a.origin.y > b.origin.y - 1.5
        && a.origin.x + a.size.width < b.origin.x + b.size.width + 1.5
        && a.origin.y + a.size.height < b.origin.y + b.size.height + 1.5
}

/// Everything in READ of one element, in one question to its app. The
/// answer has one value for each name, an error value where the element has
/// none of that name. The question gives up after SCAN_TIMEOUT, so an app
/// that hangs cannot hang the scan. The time is set on every element: an
/// element does not take it over from its app.
unsafe fn ax_read(element: Id, names: Id) -> Result<Cf, i32> {
    AXUIElementSetMessagingTimeout(element, SCAN_TIMEOUT);
    let mut values: Id = null_mut();
    let error = AXUIElementCopyMultipleAttributeValues(element, names, 0, &mut values);
    if error != 0 || values.is_null() {
        return Err(if error != 0 { error } else { -25200 });
    }
    let values = Cf(values);
    if CFArrayGetCount(values.0) == READ.len() as isize { Ok(values) } else { Err(-25200) }
}

/// The id macOS's window list has for the window an element is in. None
/// when this macOS does not say.
unsafe fn window_id(element: Id) -> Option<u32> {
    let ask = std::mem::transmute::<*mut c_void, WindowOfElement>(private_function("_AXUIElementGetWindow")?);
    let mut id = 0u32;
    if ask(element, &mut id) == 0 && id != 0 { Some(id) } else { None }
}

/// Whether an element takes AXPress.
unsafe fn takes_press(element: Id) -> bool {
    let mut names: Id = null_mut();
    if AXUIElementCopyActionNames(element, &mut names) != 0 || names.is_null() {
        return false;
    }
    let names = Cf(names);
    (0..CFArrayGetCount(names.0)).any(|n| as_text(CFArrayGetValueAtIndex(names.0, n)).as_deref() == Some("AXPress"))
}

/// Whether an element takes the keyboard of its app: whether it can be given the focus.
unsafe fn takes_keyboard(element: Id) -> bool {
    let (focused, mut settable) = (Cf(cf_string("AXFocused")), 0u8);
    AXUIElementIsAttributeSettable(element, focused.0, &mut settable) == 0 && settable != 0
}

/// What the scan calls an element: its title, else its description, else
/// its value, else its help. A window's own buttons have none of the first
/// three, and for help the full screen button has a sentence about zooming:
/// so with `own_kind` an element that is a kind of its own (it has a subrole)
/// is called what macOS calls that kind, "close button", before its help is
/// looked at. One line, at most 100 characters.
unsafe fn name_of(answer: Id, own_kind: bool) -> String {
    let at = |n: isize| CFArrayGetValueAtIndex(answer, n);
    let kind = if own_kind && as_text(at(R_SUBROLE)).is_some() { R_ROLE_IN_WORDS } else { R_HELP };
    let name = [R_TITLE, R_DESCRIPTION, R_VALUE, kind, R_HELP].iter().filter_map(|&n| as_words(at(n)))
        .find(|words| !words.trim().is_empty()).unwrap_or_default();
    let start: String = name.chars().take(400).collect();
    start.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(100).collect()
}

/// The folder of a running app, its .app, as macOS has it: the app is not
/// asked. None when macOS knows no app for the process.
unsafe fn app_folder(pid: i32) -> Option<PathBuf> {
    let app = msg!(Id; class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid => i32);
    if app.is_null() {
        return None;
    }
    let folder = msg!(Id; app, "bundleURL");
    if folder.is_null() { None } else { Some(PathBuf::from(rust_string(msg!(Id; folder, "path")))) }
}

/// Chromium builds its elements only when someone asks for them, so a first
/// read of such an app finds little. Two switches on the app ask for them.
/// AXManualAccessibility is Electron's own and changes nothing else: an
/// ordinary app refuses it (seen: Calculator and TextEdit, error -25205).
/// AXEnhancedUserInterface is the one a screen reader sets. Every app takes
/// it and it changes how AppKit animates windows, so it is only set on an
/// app that is built on Chromium. Gives back whether the app took a switch,
/// and None when it does not answer at all. No Chromium or Electron app was
/// scanned in the tests, so for those this has not run.
/// For now no app takes a switch: `start` refuses every app built on Chromium.
unsafe fn wake(pid: i32, app: Id) -> Option<bool> {
    match ax_set(app, "AXManualAccessibility", TRUE) {
        0 => Some(true),
        NOT_ANSWERING => None,
        _ => Some(app_folder(pid).is_some_and(|folder| is_chromium(&folder))
                  && ax_set(app, "AXEnhancedUserInterface", TRUE) == 0),
    }
}

/// One read of a display, while it runs.
struct Reader {
    /// READ, as macOS takes it.
    names: Cf,
    bounds: Rect,
    /// The apps that are asked.
    apps: Vec<Process>,
    windows: Vec<Shown>,
    items: Vec<Found>,
    /// The places taken: one element per place.
    places: HashSet<[i32; 4]>,
    /// The places of the rows taken: a cell or a heading inside one is left out.
    rows: Vec<Rect>,
    /// Elements read in the window at hand.
    read: usize,
    /// The apps that stopped answering.
    stuck: Vec<i32>,
    /// The elements found, above the one at hand, that have no name yet:
    /// the first text under them names them, as it names a row of a table.
    nameless: Vec<usize>,
}

impl Reader {
    /// An open menu, a sheet or the list of a combo box is a window of its
    /// own, in front of the window it belongs to, and what is in it comes up
    /// among the elements of that window. So an element that such a popup of
    /// its app lies over may be in the popup: it is when its frame is the
    /// popup's (seen: the list of a combo box), or when macOS names the popup
    /// as its window (seen: a menu). Gives back the popup, by its place in `windows`.
    unsafe fn own_window(&self, element: Id, frame: &Rect, home: usize) -> Option<usize> {
        let pid = self.windows[home].window.pid;
        let over = |shown: &Shown| shown.popup && shown.window.pid == pid && overlap(&shown.window.frame, frame);
        let in_front = &self.windows[..home];
        if !in_front.iter().any(over) {
            return None;
        }
        if let Some(same) = in_front.iter().position(|shown| over(shown) && same_place(&shown.window.frame, frame)) {
            return Some(same);
        }
        let id = window_id(element)?;
        in_front.iter().position(|shown| over(shown) && shown.window.id == id)
    }

    /// Read one element and what is under it. `home` is the window it is in,
    /// by its place in `windows`, and `clip` the part of that window where
    /// the element can be on show.
    unsafe fn walk(&mut self, element: Id, mut home: usize, mut clip: Rect, depth: u32) {
        let pid = self.windows[home].window.pid;
        if depth > SCAN_DEEP || self.read >= SCAN_MOST || self.stuck.contains(&pid) {
            return;
        }
        let Some(app) = self.apps.iter().copied().find(|app| app.pid == pid) else { return };
        let answer = match ax_read(element, self.names.0) {
            Ok(answer) => answer,
            Err(NOT_ANSWERING) => {
                self.stuck.push(pid);
                return;
            }
            Err(_) => return, // the element went away after its parent listed it
        };
        self.read += 1;
        let at = |n: isize| CFArrayGetValueAtIndex(answer.0, n);
        let role = as_text(at(R_ROLE)).unwrap_or_default();
        let frame = as_frame(at(R_POSITION), at(R_SIZE));
        if depth == 0 {
            self.windows[home].title = as_text(at(R_TITLE)).unwrap_or_default();
        }
        // What is in a popup is cut to the popup, and only what is in front of the popup covers it.
        if let (Some(frame), true) = (&frame, depth > 0) {
            if let Some(own) = self.own_window(element, frame, home) {
                home = own;
                clip = cut(&self.windows[own].window.frame, &self.bounds);
            }
        }
        let place = frame.map(|frame| cut(&frame, &clip));

        if !self.nameless.is_empty() {
            let text = name_of(answer.0, false);
            for &waiting in &self.nameless {
                if !text.is_empty() && self.items[waiting].name.is_empty() {
                    self.items[waiting].name = text.clone();
                }
            }
        }
        let mut waits_for_a_name = false;
        if let Some(place) = place {
            let (x, y) = (place.origin.x + place.size.width / 2.0, place.origin.y + place.size.height / 2.0);
            let key = [place.origin.x.round() as i32, place.origin.y.round() as i32,
                       (place.origin.x + place.size.width).round() as i32,
                       (place.origin.y + place.size.height).round() as i32];
            let on_show = place.size.width >= 4.0 && place.size.height >= 4.0
                && !self.windows[..home].iter().any(|above| holds(&above.window.frame, x, y));
            let enabled = as_bool(at(R_ENABLED)) != Some(false);
            if on_show && enabled && !WHOLE_AREAS.contains(&role.as_str()) && !self.places.contains(&key) {
                let row = ["AXRow", "AXCell"].contains(&role.as_str());
                let selectable = row && as_bool(at(R_SELECTED)).is_some();
                // In a list a row, its cell and a heading in it are one thing to click, given
                // three times at one place (seen: Font Book's side bar, Dictionary). The row stays.
                let in_a_row = ["AXCell", "AXHeading"].contains(&role.as_str())
                    && self.rows.iter().any(|taken| inside(&place, taken));
                // The two questions at the end are only put to what its role did not settle.
                if !in_a_row && (CLICKABLE.contains(&role.as_str()) || selectable || takes_press(element)
                                 || takes_keyboard(element)) {
                    if role == "AXRow" {
                        self.rows.push(place);
                    }
                    self.places.insert(key);
                    self.windows[home].count += 1;
                    // A row is "table row" for macOS, whatever it holds: it is named by what it holds.
                    let name = name_of(answer.0, !row);
                    waits_for_a_name = name.is_empty();
                    self.items.push(Found {
                        kind: role.strip_prefix("AX").unwrap_or(&role).to_string(),
                        name,
                        place,
                        app,
                        window: self.windows[home].window.id,
                        element: Cf(CFRetain(element)),
                    });
                }
            }
        }

        if let (Some(frame), Some(place)) = (frame, place) {
            // Nothing under an element that is wholly outside is on show: a row scrolled away.
            let outside = place.size.width <= 0.0 || place.size.height <= 0.0;
            if outside && frame.size.width >= 1.0 && frame.size.height >= 1.0 {
                return;
            }
            if role == "AXScrollArea" {
                clip = place; // what is scrolled out of it is not on show
            }
        }
        // A table names the rows it shows. The others, thousands in a long list, are not read.
        let mut children = as_elements(at(R_VISIBLE_ROWS));
        if children.is_empty() {
            children = as_elements(at(R_CHILDREN));
        } else {
            let header = at(R_HEADER);
            if !header.is_null() && CFGetTypeID(header) == AXUIElementGetTypeID() {
                children.push(header);
            }
        }
        if waits_for_a_name {
            self.nameless.push(self.items.len() - 1);
        }
        for child in children {
            self.walk(child, home, clip, depth + 1);
        }
        if waits_for_a_name {
            self.nameless.pop();
        }
    }
}

/// One read of a display: the windows on show on it, and what there is to
/// click or type in in the windows of the apps in `apps`.
unsafe fn read_display(bounds: Rect, apps: &[Process]) -> Scan {
    let start = Instant::now();
    // Only the very processes that were started count, and only while they run: macOS
    // gives the id of an ended process to a later one.
    let running: Vec<i32> = apps.iter().filter(|app| alive(app)).map(|app| app.pid).collect();
    // The windows on show: the real ones of any app, and the menus and other popups of ours.
    let mut windows = Vec::new();
    for (window, app, _) in listed_windows(true) {
        let ours = running.contains(&window.pid);
        if real(&window) && overlap(&window.frame, &bounds) && (window.layer == 0 || ours) {
            windows.push(Shown { window, app, title: String::new(), ours, popup: false, count: 0 });
        }
    }
    let mut reader = Reader { names: cf_names(&READ), bounds, apps: apps.to_vec(), windows, items: Vec::new(),
                              places: HashSet::new(), rows: Vec::new(), read: 0, stuck: Vec::new(),
                              nameless: Vec::new() };
    let (mut notes, mut late) = (Vec::new(), false);

    // The windows to read: each app's own list of its windows, matched with macOS's.
    let mut to_read: Vec<(usize, Cf)> = Vec::new();
    let mut answered = Vec::new();
    for pid in apps.iter().map(|app| app.pid) {
        // Only a running app is asked: a process id that is gone answers "cannot complete" at once.
        if !running.contains(&pid)
            || msg!(Id; class("NSRunningApplication"), "runningApplicationWithProcessIdentifier:", pid => i32).is_null()
        {
            notes.push(format!("process {pid} is not a running app: not read"));
            continue;
        }
        let app = ax_app(pid);
        AXUIElementSetMessagingTimeout(app.0, SCAN_TIMEOUT);
        match wake(pid, app.0) {
            Some(took_a_switch) => late |= took_a_switch,
            None => {
                reader.stuck.push(pid);
                continue;
            }
        }
        answered.push(pid);
        for window in ax_list(app.0, "AXWindows") {
            AXUIElementSetMessagingTimeout(window.0, SCAN_TIMEOUT);
            let id = window_id(window.0);
            let frame = if id.is_none() { ax_frame(window.0) } else { None };
            let is_it = |shown: &Shown| shown.window.pid == pid && match (id, &frame) {
                (Some(id), _) => shown.window.id == id,
                (None, Some(frame)) => same_place(&shown.window.frame, frame),
                (None, None) => false,
            };
            if let Some(found) = reader.windows.iter().position(is_it) {
                to_read.push((found, window));
            }
        }
    }
    to_read.sort_by_key(|(home, _)| *home); // front to back, whatever app each belongs to
    for (n, shown) in reader.windows.iter_mut().enumerate() {
        shown.popup = answered.contains(&shown.window.pid) && !to_read.iter().any(|(home, _)| *home == n);
    }
    for (home, window) in &to_read {
        reader.read = 0;
        let clip = cut(&reader.windows[*home].window.frame, &bounds);
        reader.walk(window.0, *home, clip, 0);
        if reader.read >= SCAN_MOST {
            notes.push(format!("{} has more than {SCAN_MOST} elements in one window: the rest was not read",
                               reader.windows[*home].app));
        }
    }
    for &pid in &reader.stuck {
        notes.push(format!("{} did not answer within {SCAN_TIMEOUT} seconds: not read", app_name(pid)));
    }
    Scan { bounds, windows: reader.windows, items: reader.items, notes, late, took: start.elapsed() }
}

/// The agent's scan of a display: what Accessibility finds to click or type
/// in, in the windows on show there, front to back. Only the apps in `apps`
/// are ever asked: the apps this program started. A window of another app is
/// in the list of windows under the name of its app, covers what is behind
/// it, and is not asked.
unsafe fn scan(display: u32, apps: &[Process]) -> Res<Scan> {
    if !displays().contains(&display) {
        return Err(format!("there is no display {display}"));
    }
    if AXIsProcessTrusted() == 0 {
        return Err("the scan reads the apps through Accessibility, and that is not allowed for the app this \
                    runs in (System Settings > Privacy & Security > Accessibility)".to_string());
    }
    if screen_locked() {
        return Err("your screen is locked: Accessibility gives nothing back while it is".to_string());
    }
    let bounds = CGDisplayBounds(display);
    let pool = objc_autoreleasePoolPush(); // a scan leaves nothing behind in a program that runs for hours
    let mut scan = read_display(bounds, apps);
    if scan.late {
        // Chromium (so Electron too) builds its elements only when someone
        // first asks: read until two reads in a row agree.
        for _ in 0..8 {
            sleep(1.5);
            let again = read_display(bounds, apps);
            let same = again.items.len() == scan.items.len();
            scan = again;
            if same {
                break;
            }
        }
    }
    objc_autoreleasePoolPop(pool);
    Ok(scan)
}

/// Print what a scan found: what it could not read, then the numbered list.
fn print_scan(scan: &Scan, indent: &str) {
    for note in &scan.notes {
        println!("{indent}could not read: {note}");
    }
    for (n, item) in scan.items.iter().enumerate() {
        println!("{indent}[{}] {} {}", n + 1, item.kind, quote(&item.name.chars().take(50).collect::<String>()));
    }
}

/// The kinds a press would open a menu of: `press` refuses them. They stay in the scan's list.
const OPENS_A_MENU: [&str; 3] = ["PopUpButton", "MenuButton", "MenuBarItem"];

/// Press one of the scan's elements: Accessibility's AXPress, the
/// counterpart of UI Automation's Invoke and Toggle. No pointer and no event
/// is used, and the app does not come to the front. A pop-up button, a menu
/// button and an entry of a menu bar are not pressed: the press would open
/// their menu, and while a menu of an app that is not in front is open,
/// macOS sends YOUR keyboard to that app (seen for the menu of a right
/// click; a press was not tried, so that no menu was opened). No step of
/// this file opens a menu. A click at the place of such an element would
/// open it: `click_at` does not look at what is there. What takes no press
/// is selected (a row) or given the keyboard of its app (a text): both are
/// read back, since an app can say yes and do nothing. Seen with an app
/// that was stopped: the press gave up after SCAN_TIMEOUT and did not happen
/// later either, when the app went on. Gives back what was done.
unsafe fn press(item: &Found) -> Res<&'static str> {
    if OPENS_A_MENU.contains(&item.kind.as_str()) {
        return Err(format!("{} {} is not pressed: a press opens its menu, and an open menu of an app that is not \
                            in front takes your keyboard (seen for the menu of a right click, not tried for a \
                            press)", item.kind, quote(&item.name)));
    }
    if !alive(&item.app) {
        return Err(format!("the app of {} {} has ended", item.kind, quote(&item.name)));
    }
    let element = item.element.0;
    AXUIElementSetMessagingTimeout(element, SCAN_TIMEOUT);
    expect_window(); // a press can open a window: the keeper looks often now
    let turned_on = |name: &str| ax_set(element, name, TRUE) == 0 && ax_bool(element, name) == Some(true);
    match ax_do(element, "AXPress") {
        0 => Ok("pressed"),
        NOT_ANSWERING => Err(format!("the app of {} {} did not answer the press within {SCAN_TIMEOUT} seconds: \
                                      scan again to see whether it happened", item.kind, quote(&item.name))),
        NO_SUCH_ACTION if turned_on("AXSelected") => Ok("selected"),
        NO_SUCH_ACTION if turned_on("AXFocused") => Ok("given the keyboard of its app"),
        error => Err(format!("{} {} did not take the press (Accessibility error {error})", item.kind,
                             quote(&item.name))),
    }
}

/// Put a text into one of the scan's elements: its AXValue, the counterpart
/// of UI Automation's value pattern. The whole text of a field is replaced,
/// with no typing: the app's autocorrection does not touch it.
unsafe fn set_value(item: &Found, text: &str) -> Res<()> {
    if !alive(&item.app) {
        return Err(format!("the app of {} {} has ended", item.kind, quote(&item.name)));
    }
    AXUIElementSetMessagingTimeout(item.element.0, SCAN_TIMEOUT);
    let value = Cf(cf_string(text));
    match ax_set(item.element.0, "AXValue", value.0) {
        0 => Ok(()),
        error => Err(format!("{} {} did not take the text (Accessibility error {error})", item.kind,
                             quote(&item.name))),
    }
}

// ---------------------------------------------------------------- computer use

/// The middle of a rectangle.
fn middle(frame: &Rect) -> (f64, f64) {
    (frame.origin.x + frame.size.width / 2.0, frame.origin.y + frame.size.height / 2.0)
}

/// Where a word of one of the numbered lines starts in a text, in UTF-16 units.
fn word_in_line(text: &str, line: u32, word: &str) -> Option<isize> {
    let start = text.find(&format!("line {line} "))?;
    let at = start + text[start..].find(word)?;
    Some(text[..at].encode_utf16().count() as isize)
}

/// One line of a text, counted from 1.
fn line_of(text: &str, number: usize) -> String {
    text.lines().nth(number - 1).unwrap_or("").to_string()
}

/// How long a text is for Accessibility: in UTF-16 units.
fn units(text: &str) -> isize {
    text.encode_utf16().count() as isize
}

/// The digits of what Calculator shows. It groups them as your region does: "9,123".
fn digits(shown: &str) -> String {
    shown.chars().filter(char::is_ascii_digit).collect()
}

/// A text cut to fit one line of the report.
fn short(text: &str) -> String {
    let flat = text.replace('\n', "\\n");
    let length = flat.chars().count();
    if length <= 150 {
        return flat;
    }
    format!("{}... ({length} characters)", flat.chars().take(120).collect::<String>())
}

/// The text file `lines_file` wrote in this run, when it wrote one.
static LINES_FILE: OnceLock<PathBuf> = OnceLock::new();

/// The text the agent works on in TextEdit: 300 numbered lines, so that it
/// scrolls, written into the system's temp folder. Gives back the file. Its
/// name carries this program's process id: two runs do not share one file.
fn lines_file() -> Res<String> {
    let file = std::env::temp_dir().join(format!("Mac_CUA_lines_{}.txt", std::process::id()));
    let lines: String = (1..=300).map(|n| format!("line {n} alpha beta gamma\n")).collect();
    std::fs::write(&file, lines).map_err(|e| format!("could not write {}: {e}", file.display()))?;
    let _ = LINES_FILE.set(file.clone());
    Ok(file.to_string_lossy().into_owned())
}

/// Take that text file out of the temp folder again. Call it when the run
/// ends, after TextEdit has ended. After kill -9 the file stays. So does
/// its entry in TextEdit's list of recent documents, in every case.
fn remove_lines_file() {
    if let Some(file) = LINES_FILE.get() {
        let _ = std::fs::remove_file(file);
    }
}

/// The first of the scan's elements of that kind in that app whose name is
/// `name`, or the first of that kind when `name` is empty.
fn pick<'a>(items: &'a [Found], pid: i32, kind: &str, name: &str) -> Res<&'a Found> {
    items.iter().find(|item| item.app.pid == pid && item.kind == kind && (name.is_empty() || item.name == name))
        .ok_or_else(|| format!("no {kind} {} in the scan", quote(name)))
}

/// Save the scan's picture: the display with the scan's elements numbered
/// on it, and the agent's mouse where it is. Gives back where the picture is.
unsafe fn save_numbered(display: u32, found: &Scan, name: &str) -> Res<String> {
    let path = picture_path(name)?;
    let numbered = draw_boxes(&capture(display)?, &found.bounds, &found.items)?;
    let with_mouse = mouse_into(&numbered, found.bounds.size.width)?;
    save_png(with_mouse.as_ref().unwrap_or(&numbered), &path)?;
    Ok(path.display().to_string())
}

/// The two apps the agent works on, and what it counts while it does.
struct Desk {
    input: Input,
    calculator: i32,
    textedit: i32,
    /// TextEdit's text, and the bar that says how far it is scrolled.
    text: Cf,
    scroll_bar: Cf,
    steps: u32,
    failed: u32,
    /// How many times the watcher had seen something when the agent began.
    seen_before: u32,
}

impl Desk {
    /// Print one step with what came back. The agent stops when one of its
    /// apps came in your way: when the watcher saw one in front, with your
    /// keyboard, or with a window on your screen. And on Ctrl+C or kill, and
    /// when its live window was closed.
    unsafe fn step(&mut self, what: &str, came_back: String, arrived: bool) -> Res<()> {
        self.steps += 1;
        if !arrived {
            self.failed += 1;
        }
        println!("   {what:<58} -> {}{}", short(&came_back), if arrived { "" } else { "   << DID NOT ARRIVE" });
        // A menu or another popup that a step left open has your keyboard: it is closed at once.
        if self.input.give_keyboard_back()? {
            println!("   (a popup of an app of the agent had your keyboard: closed with Escape)");
        }
        if let Some(why) = stop_asked() {
            return Err(why.to_string());
        }
        if seen_times() > self.seen_before {
            let seen: Vec<String> = SEEN.lock().unwrap().iter().map(|(what, _)| what.clone()).collect();
            return Err(format!("an app of the agent came in your way ({})", seen.join(", ")));
        }
        Ok(())
    }

    /// What Calculator shows: the last text in its windows.
    unsafe fn shows(&self) -> String {
        if self.input.ours(self.calculator).is_err() {
            return String::new(); // it has ended: nothing is asked of that process id
        }
        let (app, mut texts) = (ax_app(self.calculator), Vec::new());
        for window in ax_list(app.0, "AXWindows") {
            ax_all(window.0, 8, &|e| ax_role_is(e, "AXStaticText"), &mut texts);
        }
        texts.iter().filter_map(|text| ax_text(text.0, "AXValue")).filter(|text| !text.is_empty()).last()
            .unwrap_or_default()
    }

    unsafe fn value(&self) -> String {
        ax_text(self.text.0, "AXValue").unwrap_or_default()
    }

    unsafe fn selection(&self) -> Range {
        ax_selection(self.text.0).unwrap_or(Range { start: -1, length: 0 })
    }

    unsafe fn selected(&self) -> String {
        ax_text(self.text.0, "AXSelectedText").unwrap_or_default()
    }

    unsafe fn scrolled(&self) -> f64 {
        ax_number(self.scroll_bar.0, "AXValue").unwrap_or(-1.0)
    }

    /// Where a word of a numbered line is: its start in the text and its place on screen.
    unsafe fn word(&self, line: u32, word: &str) -> Res<(isize, Rect)> {
        let start = word_in_line(&self.value(), line, word).ok_or(format!("line {line} has no {}", quote(word)))?;
        let frame = ax_bounds_of(self.text.0, Range { start, length: units(word) })
            .ok_or("TextEdit did not say where the word is")?;
        Ok((start, frame))
    }

    /// Press one key in TextEdit and give back where the caret or the selection is then.
    unsafe fn key_then(&mut self, keycode: u16, flags: u64) -> Res<Range> {
        self.input.key(self.textedit, keycode, flags)?;
        sleep(0.3);
        Ok(self.selection())
    }

    /// The steps: every kind of input the agent has, each read back from the app.
    unsafe fn act(&mut self, items: &[Found]) -> Res<()> {
        let (calculator, textedit) = (self.calculator, self.textedit);
        let at = |range: Range| match range.length {
            0 => format!("caret at {}", range.start),
            length => format!("selected {length} from {}", range.start),
        };

        // Calculator: a key, left clicks at points, and a press through Accessibility
        self.input.key(calculator, KEY_ESCAPE, 0)?;
        sleep(0.3);
        let shows = self.shows();
        self.step("key Escape in Calculator (key events)", format!("display: {}", quote(&shows)),
                  digits(&shows) == "0")?;
        for (name, wanted) in [("7", "7"), ("8", "78")] {
            let button = pick(items, calculator, "Button", name)?;
            let (x, y) = middle(&button.place);
            let went = self.input.click_at(x, y)?;
            sleep(0.3);
            let shows = self.shows();
            self.step(&format!("left_click {} at {x:.0},{y:.0} (mouse events)", quote(name)),
                      format!("display: {}", quote(&shows)), went.id == button.window && digits(&shows) == wanted)?;
        }
        let did = press(pick(items, calculator, "Button", "9")?)?;
        sleep(0.3);
        let shows = self.shows();
        self.step("left_click '9' (Accessibility, AXPress)", format!("{did}, display: {}", quote(&shows)),
                  digits(&shows) == "789")?;

        // TextEdit: clicks in a text, which is not the app in front either
        let (start, frame) = self.word(3, "beta")?;
        let (x, y) = middle(&frame);
        self.input.click_at(x, y)?;
        sleep(0.3);
        let caret = self.selection();
        self.step(&format!("left_click on 'beta' of line 3 at {x:.0},{y:.0}"),
                  format!("caret at {} (the word is {start} to {})", caret.start, start + 4),
                  caret.length == 0 && caret.start >= start && caret.start <= start + 4)?;

        let (start, frame) = self.word(5, "beta")?;
        let (x, y) = middle(&frame);
        self.input.double_click_at(x, y)?;
        sleep(0.3);
        let selected = self.selected();
        self.step("double_click on 'beta' of line 5", format!("selected: {}", quote(&selected)),
                  selected == "beta" && self.selection() == Range { start, length: 4 })?;

        let (_, frame) = self.word(6, "beta")?;
        let (x, y) = middle(&frame);
        self.input.triple_click_at(x, y)?;
        sleep(0.3);
        let selected = self.selected();
        self.step("triple_click on line 6", format!("selected: {}", quote(&selected)),
                  selected == "line 6 alpha beta gamma\n")?;

        let ((_, from), (_, to)) = (self.word(7, "alpha")?, self.word(7, "gamma")?);
        let (x1, y1, x2, y2) = (from.origin.x + 1.0, middle(&from).1, to.origin.x + to.size.width - 1.0, middle(&to).1);
        self.input.drag(x1, y1, x2, y2)?;
        sleep(0.3);
        let selected = self.selected();
        self.step("left_click_drag from 'alpha' to 'gamma' on line 7", format!("selected: {}", quote(&selected)),
                  selected == "alpha beta gamma")?;

        // typed text, any Unicode, at the caret
        let (_, frame) = self.word(2, "gamma")?;
        let (x, y) = (frame.origin.x + frame.size.width - 0.5, middle(&frame).1);
        self.input.click_at(x, y)?;
        sleep(0.3);
        let typed = " héllo wörld ñ 😀 日本語 ok";
        self.input.type_text(textedit, typed)?;
        sleep(0.5);
        let line = line_of(&self.value(), 2);
        self.step("left_click at the end of line 2, typewrite (key events)", format!("line 2: {}", quote(&line)),
                  line == format!("line 2 alpha beta gamma{typed}"))?;

        // a shortcut with Cmd: as a key, and as a press on the app's own menu
        let (all, key_a) = (units(&self.value()), key_for("A")?);
        let selected = self.key_then(key_a, COMMAND)?;
        self.step("key Cmd+A (key events)", format!("selected {} of {all}", selected.length),
                  selected == Range { start: 0, length: all })?;
        let caret = self.key_then(KEY_UP, COMMAND)?;
        self.step("key Cmd+Up", at(caret), caret == Range::default())?;
        let pressed = self.input.menu_shortcut(textedit, key_a, COMMAND);
        sleep(0.3);
        let selected = self.selection();
        let said = pressed.clone().map(|title| quote(&title)).unwrap_or_else(|e| e);
        self.step("Cmd+A as a press on TextEdit's own menu (Accessibility)",
                  format!("{said}: selected {} of {all}", selected.length),
                  pressed.is_ok() && selected == Range { start: 0, length: all })?;
        let caret = self.key_then(KEY_UP, COMMAND)?;
        self.step("key Cmd+Up", at(caret), caret == Range::default())?;
        // The refusal itself is asked, and nothing is sent: a step must not be able to do
        // the harm it tests for. No step calls key() or menu_shortcut() with such a shortcut.
        let refused = reaches_your_side(key_for("H")?, OPTION | COMMAND);
        self.step("Option+Cmd+H (Hide Others): refused? (asked, not sent)",
                  refused.map_or("NOT refused".to_string(), |why| format!("refused: {why}")), refused.is_some())?;

        // the wheel at a point
        let (_, frame) = self.word(12, "beta")?;
        let (x, y) = middle(&frame);
        let top = self.scrolled();
        self.input.scroll_at(x, y, 5)?;
        sleep(0.5);
        let down = self.scrolled();
        self.step(&format!("scroll down 5 at {x:.0},{y:.0} (wheel events)"),
                  format!("scroll bar: {top:.4} to {down:.4}"), down > top)?;
        self.input.scroll_at(x, y, -5)?;
        sleep(0.5);
        let up = self.scrolled();
        self.step("scroll up 5 (wheel events)", format!("scroll bar: {down:.4} to {up:.4}"), up == top)?;

        // a text set through Accessibility: no typing, so the app's autocorrection does not touch it
        let exact = "Set by the agent through Accessibility: héllo wörld 😀, exactly as given.";
        set_value(pick(items, textedit, "TextArea", "")?, exact)?;
        sleep(0.3);
        let now = self.value();
        self.step("input (Accessibility sets the text)", format!("text: {}", quote(&now)), now == exact)
    }
}

/// The agent's loop on the second display, with no mouse and no keyboard of
/// yours: scan with Accessibility, act with events handed to the apps and
/// through Accessibility, scan again.
unsafe fn computer_use(display: u32) -> Res<()> {
    let apps = started_apps();
    let on_it = |name: &str| {
        apps.iter().find(|app| app.name == name && app.stage == Stage::Shown && alive(&app.process))
            .map(|app| app.process).ok_or(format!("{name} is not on the second display"))
    };
    // the two as the very processes that were started: input and the scan go by id and birth
    let both = [on_it("Calculator")?, on_it("TextEdit")?];
    let (calculator, textedit) = (both[0].pid, both[1].pid);
    sleep(1.0); // both draw in their places

    let found = scan(display, &both)?;
    let titles: Vec<String> = found.windows.iter().filter(|shown| shown.ours && !shown.popup)
        .map(|shown| quote(&shown.title)).collect();
    println!("\n1. Accessibility, asked of the {} apps this program started, sees {} windows: {}", both.len(),
             titles.len(), titles.join(", "));
    println!("2. Scan: {} elements, numbered the way the agent sees them: {}", found.items.len(),
             save_numbered(display, &found, "cu_1_scan.png")?);
    print_scan(&found, "   ");

    println!("3. Act, with no mouse and no keyboard of yours:");
    let field = pick(&found.items, textedit, "TextArea", "")?;
    let vertical = |e: Id| {
        ax_role_is(e, "AXScrollBar") && ax_text(e, "AXOrientation").as_deref() == Some("AXVerticalOrientation")
    };
    let around = ax_get(field.element.0, "AXParent").ok_or("TextEdit's text is in no scroll area")?;
    let scroll_bar = ax_find(around.0, 1, &vertical).ok_or("TextEdit shows no scroll bar")?;
    let mut desk = Desk {
        input: Input::new(display, &both)?,
        calculator,
        textedit,
        text: Cf(CFRetain(field.element.0)),
        scroll_bar,
        steps: 0,
        failed: 0,
        seen_before: seen_times(),
    };
    let acted = desk.act(&found.items);
    if let Err(e) = &acted {
        println!("   stopped: {e}");
    }
    let (steps, failed) = (desk.steps, desk.failed);
    println!("   {} of {steps} steps arrived.", steps - failed);
    if stop_asked().is_some() {
        // no second scan and no second picture: the apps are ended now
        return Err(format!("computer use: it stopped after {steps} steps"));
    }

    let again = scan(display, &both)?;
    println!("4. Scan again: {} elements: {}", again.items.len(), save_numbered(display, &again, "cu_2_after.png")?);
    match (acted, failed) {
        (Ok(()), 0) => Ok(()),
        (Ok(()), _) => Err(format!("computer use: {failed} of its {steps} steps did not arrive")),
        (Err(_), _) => Err(format!("computer use: it stopped after {steps} steps")),
    }
}

// ---------------------------------------------------------------- shot and viewer

/// Your main screen before anything started: the front app and the pointer.
struct Before {
    front: i32,
    front_name: String,
    pointer: Point,
}

unsafe fn take_shot(name: &str, wait: f64, frames: &Frames, before: &Before) -> Res<()> {
    pump(wait); // the apps get to start
    let seen = frames.latest.lock().unwrap().taken;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let latest = frames.latest.lock().unwrap();
        if latest.taken != seen || latest.error.is_some() || Instant::now() >= deadline {
            break;
        }
        drop(latest);
        thread::sleep(Duration::from_millis(50)); // a picture taken after the wait
    }
    let picture = {
        let latest = frames.latest.lock().unwrap();
        match (&latest.error, &latest.picture) {
            (None, Some(picture)) => picture.clone(),
            (error, _) => return Err(format!("no picture: {}", error.as_deref().unwrap_or("timed out"))),
        }
    };
    let path = picture_path(name)?;
    // The agent's mouse is drawn into the picture, when it has a place (--mouse).
    let with_mouse = mouse_into(&picture, CGDisplayBounds(frames.display).size.width)?;
    save_png(with_mouse.as_ref().unwrap_or(&*picture), &path)?;
    let (w, h) = picture.size();
    println!("\nScreenshot of the second display: {} ({w}x{h})", path.display());
    let mouse = *POINTER.lock().unwrap();
    if let (Some(_), Some(at)) = (&with_mouse, mouse) {
        println!("The agent's mouse is drawn in it, its tip at {:.0},{:.0}.", at.x, at.y);
    }
    report(&window_rows(frames.display), before);
    Ok(())
}

/// The agent's scan of the second display: what Accessibility finds to
/// click or type in, in the windows of the apps this program started,
/// numbered on a picture of it.
unsafe fn scan_display(display: u32, png: &str) -> Res<Scan> {
    let apps: Vec<Process> = started_apps().iter().map(|app| app.process).filter(|app| alive(app)).collect();
    let found = scan(display, &apps)?;
    println!("\nScan of the second display: {} elements, read in {:.2} s, numbered on {}", found.items.len(),
             found.took.as_secs_f64(), save_numbered(display, &found, png)?);
    print_scan(&found, "  ");
    Ok(found)
}

/// One line of the report for a window: whose it is, its title and its place.
fn window_row(owner: &str, title: &str, frame: &Rect) -> String {
    let (size, at) = (frame.size, frame.origin);
    format!("  {owner:<24} {title:<34} {:.0}x{:.0} at {:.0},{:.0}", size.width, size.height, at.x, at.y)
}

/// The windows on show on the display, top first, as macOS's own list has
/// them. A window of an app this program did not start is there under the
/// name of its app only.
unsafe fn window_rows(display: u32) -> Vec<String> {
    let ours: Vec<i32> = started_apps().iter().map(|app| app.process.pid).collect();
    windows_on(display).iter().map(|(window, owner, title)| {
        window_row(owner, if ours.contains(&window.pid) { title } else { "(another app's)" }, &window.frame)
    }).collect()
}

/// The same from a scan, with what the scan found in each.
fn scan_rows(scan: &Scan) -> Vec<String> {
    scan.windows.iter().map(|shown| {
        let title = match (shown.popup, shown.ours) {
            (true, _) => "(a popup of its app)",
            (false, true) => shown.title.as_str(),
            (false, false) => "(another app's)",
        };
        let what = if shown.ours { format!("{} elements", shown.count) } else { "not asked".to_string() };
        format!("{:<86} {what}", window_row(&shown.app, title, &shown.window.frame))
    }).collect()
}

/// What runs on the second display, its RAM, and whether your side changed.
unsafe fn report(windows: &[String], before: &Before) {
    println!("Apps started on it:");
    for app in started_apps() {
        println!("  {:<24} {}", app.name, process_state(&app));
    }
    println!("Windows on it, top first:");
    for window in windows {
        println!("{window}");
    }
    if windows.is_empty() {
        println!("  (none)");
    }
    let rows = app_memory();
    // the 0.0: with no app running the sum of nothing is -0.0 in Rust, and was printed as "-0 MB"
    let mb = rows.iter().map(|(_, mb, _)| mb).sum::<f64>() + 0.0;
    println!("RAM of the apps on it: {mb:.0} MB physical footprint");
    for (name, mb, what) in &rows {
        println!("{}", format!("  {name:<34} {mb:7.1} MB  {what}").trim_end());
    }
    report_your_side(before);
}

/// Your side: your front app, your pointer, and what the watcher saw of the
/// apps on the second display while it ran.
unsafe fn report_your_side(before: &Before) {
    let (front, name) = front_app();
    let front = if front == before.front {
        format!("unchanged ({})", before.front_name)
    } else if started_apps().iter().any(|app| app.process.pid == front) {
        format!("changed from {} to {}, AN APP OF THE SECOND DISPLAY", quote(&before.front_name), quote(&name))
    } else {
        format!("changed from {} to {}, neither is an app of the second display", quote(&before.front_name),
                quote(&name))
    };
    let moved = if pointer() == before.pointer { "not moved" } else { "moved (by you?)" };
    println!("Your front app: {front}. Pointer: {moved}.");
    let seen = SEEN.lock().unwrap();
    if seen.is_empty() {
        println!("None of its apps was seen in front or with your keyboard, and none of their windows on your screen.");
    } else {
        println!("SEEN while it ran:");
        for (what, times) in seen.iter() {
            println!("  {what} ({times} times)");
        }
    }
}

/// What the viewer window is made of, for the methods macOS calls. The
/// objects are kept as plain numbers, so that this can be a static.
struct Viewer {
    frames: Arc<Frames>,
    window: usize,
    /// The layer that fills the window and shows the display's picture.
    picture: usize,
    /// The layer above it with the agent's arrow.
    pointer: usize,
    /// The `count` of the picture on show.
    shown: AtomicU64,
    ticks: AtomicU32,
}

static VIEWER: OnceLock<Viewer> = OnceLock::new();

/// A rectangle an object has under a name ("bounds", "frame"). It is asked
/// for as a value and copied out of that: a method that gives a rectangle
/// back directly is called through another entry of the runtime on an Intel
/// Mac, and this way is the same on both.
unsafe fn rect_of(object: Id, name: &str) -> Rect {
    let name = Cf(cf_string(name));
    let value = msg!(Id; object, "valueForKey:", name.0 => Id);
    let mut rect = Rect::default();
    if !value.is_null() {
        msg!((); value, "getValue:size:", &mut rect as *mut Rect as *mut c_void => *mut c_void,
             std::mem::size_of::<Rect>() => usize);
    }
    rect
}

/// Change layers at once. Left alone, Core Animation glides every change of
/// a layer over a quarter of a second.
unsafe fn at_once(change: impl FnOnce()) {
    let transaction = class("CATransaction");
    msg!((); transaction, "begin");
    msg!((); transaction, "setDisableActions:", true => bool);
    change();
    msg!((); transaction, "commit");
}

/// Put the arrow where the agent's pointer is in the picture, as the window
/// shows it now. The picture keeps its shape inside the window, so it can
/// have black bars at two of its sides. Done again whenever the window
/// changes its size.
unsafe fn place_pointer(viewer: &Viewer) {
    let (layer, at) = (viewer.pointer as Id, *POINTER.lock().unwrap());
    let inside = rect_of(viewer.picture as Id, "bounds").size;
    let display = CGDisplayBounds(viewer.frames.display).size;
    if inside.width <= 0.0 || inside.height <= 0.0 {
        POINTER_MOVED.store(true, Ordering::SeqCst); // the window has no size yet: the next tick tries again
        return;
    }
    at_once(|| match at {
        Some(at) if display.width > 0.0 && display.height > 0.0 => {
            let scale = f64::min(inside.width / display.width, inside.height / display.height);
            let left = (inside.width - display.width * scale) / 2.0;
            let top = (inside.height - display.height * scale) / 2.0;
            // a layer counts its points from the bottom left
            let place = Point { x: left + at.x * scale, y: inside.height - (top + at.y * scale) };
            msg!((); layer, "setPosition:", place => Point);
            msg!((); layer, "setHidden:", false => bool);
        }
        _ => msg!((); layer, "setHidden:", true => bool),
    });
}

/// End the window's loop, so that show_viewer comes back to its caller. The
/// loop only looks whether it was stopped after an event, so an empty event
/// follows the stop.
unsafe fn leave() {
    let app = msg!(Id; class("NSApplication"), "sharedApplication");
    msg!((); app, "stop:", null_mut() => Id);
    let nothing = msg!(Id; class("NSEvent"),
                       "otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:",
                       EVENT_APPLICATION_DEFINED => usize, Point::default() => Point, 0 => usize, 0.0 => f64,
                       0 => isize, null_mut() => Id, 0 => i16, 0 => isize, 0 => isize);
    msg!((); app, "postEvent:atStart:", nothing => Id, true => bool);
}

/// "tick:", from the timer, four times a second on the main thread: show the
/// newest picture when it is another one than the one on show, move the
/// arrow when the agent's pointer moved, and every two seconds say in the
/// title how many windows are on the display and how much RAM its apps use.
/// Ctrl+C and "kill" close the window from here.
unsafe extern "C" fn viewer_tick(_this: Id, _method: Id, _timer: Id) {
    let Some(viewer) = VIEWER.get() else { return };
    if INTERRUPTED.load(Ordering::SeqCst) || viewer.frames.stop.load(Ordering::SeqCst) {
        leave();
        return;
    }
    let (count, picture, error) = {
        let latest = viewer.frames.latest.lock().unwrap();
        (latest.count, latest.picture.clone(), latest.error.clone())
    };
    if let (true, Some(picture)) = (count != viewer.shown.load(Ordering::Relaxed), picture) {
        at_once(|| msg!((); viewer.picture as Id, "setContents:", picture.0 => Id)); // the layer keeps the picture
        viewer.shown.store(count, Ordering::Relaxed);
    }
    if POINTER_MOVED.swap(false, Ordering::SeqCst) {
        place_pointer(viewer);
    }
    if viewer.ticks.fetch_add(1, Ordering::Relaxed) % 8 == 0 {
        let title = match error {
            Some(error) => format!("Second Mac - {error}"),
            None => {
                let count = windows_on(viewer.frames.display).len();
                let mb = app_memory().iter().map(|(_, mb, _)| mb).sum::<f64>() + 0.0; // never "-0", see `report`
                format!("Second Mac - {count} window{} on it - its apps use {mb:.0} MB RAM - view only",
                        if count == 1 { "" } else { "s" })
            }
        };
        let title = Cf(cf_string(&title));
        msg!((); viewer.window as Id, "setTitle:", title.0 => Id);
    }
}

/// "windowDidResize:": the picture fits itself, the arrow has to be placed again.
unsafe extern "C" fn viewer_resized(_this: Id, _method: Id, _note: Id) {
    if let Some(viewer) = VIEWER.get() {
        place_pointer(viewer);
    }
}

/// "windowWillClose:": closing the window ends the viewer.
unsafe extern "C" fn viewer_closed(_this: Id, _method: Id, _note: Id) {
    leave();
}

/// "applicationShouldTerminate:": Cmd+Q, and Quit in the Dock. The answer is
/// "not yet": macOS would end the program on the spot, with no cleanup.
/// The loop is left instead, and the program ends by itself.
unsafe extern "C" fn viewer_quit(_this: Id, _method: Id, _app: Id) -> usize {
    leave();
    TERMINATE_CANCEL
}

/// The object macOS tells what happens to the window and the app: one of a
/// class that is made here, while running, with the four methods above.
unsafe fn viewer_object() -> Res<Id> {
    type Told = unsafe extern "C" fn(Id, Id, Id);
    type Asked = unsafe extern "C" fn(Id, Id, Id) -> usize;
    type Method = unsafe extern "C" fn();
    let mut made = class("AutoCuaViewer");
    if made.is_null() {
        made = objc_allocateClassPair(class("NSObject"), b"AutoCuaViewer\0".as_ptr() as *const c_char, 0);
        if made.is_null() {
            return Err("could not make the viewer window".to_string());
        }
        // the letters say what a method takes and gives: v nothing, Q a number, @ an object, : a method name
        let methods: [(&str, &[u8], Method); 4] = [
            ("tick:", b"v@:@\0", std::mem::transmute::<Told, Method>(viewer_tick)),
            ("windowDidResize:", b"v@:@\0", std::mem::transmute::<Told, Method>(viewer_resized)),
            ("windowWillClose:", b"v@:@\0", std::mem::transmute::<Told, Method>(viewer_closed)),
            ("applicationShouldTerminate:", b"Q@:@\0", std::mem::transmute::<Asked, Method>(viewer_quit)),
        ];
        for (name, types, method) in methods {
            class_addMethod(made, selector(name), method, types.as_ptr() as *const c_char);
        }
        objc_registerClassPair(made);
    }
    Ok(msg!(Id; msg!(Id; made, "alloc"), "init"))
}

/// The app's menu: Cmd+M, Cmd+W and Cmd+Q come from it. The menu bar shows
/// it only under the Regular policy. Its keys work under both.
unsafe fn viewer_menu(app: Id) {
    let (bar, top, menu) = (new_object("NSMenu"), new_object("NSMenuItem"), new_object("NSMenu"));
    for (title, action, key) in [("Minimize", "performMiniaturize:", "m"), ("Close Window", "performClose:", "w"),
                                 ("Quit Second Mac", "terminate:", "q")] {
        let (title, key) = (Cf(cf_string(title)), Cf(cf_string(key)));
        let item = msg!(Id; msg!(Id; class("NSMenuItem"), "alloc"), "initWithTitle:action:keyEquivalent:",
                        title.0 => Id, selector(action) => Id, key.0 => Id);
        msg!((); menu, "addItem:", item => Id);
        msg!((); item, "release");
    }
    msg!((); top, "setSubmenu:", menu => Id);
    msg!((); bar, "addItem:", top => Id);
    msg!((); app, "setMainMenu:", bar => Id);
    for made in [menu, top, bar] {
        msg!((); made, "release");
    }
}

/// A window on your main screen that shows the second display live, view
/// only: 60% of the main screen's width, in the display's shape, in the
/// middle. With `back` it opens behind every other window and takes nothing
/// from the app in front: no Dock icon, no menu bar, never active by
/// itself. Without, it opens in front as a program you started yourself,
/// with a Dock icon and a menu.
///
/// Comes back when the window is closed, on Cmd+Q, on Ctrl+C in the
/// terminal, and when `frames.stop` is set. Has to run on the main thread:
/// macOS allows windows nowhere else.
///
/// Seen on macOS 27.0 with `back`: the window is the last of the main
/// screen's windows, macOS never has this program as the active one, and
/// your front app keeps your keyboard. The way in front has not been run yet.
unsafe fn show_viewer(frames: &Arc<Frames>, back: bool) -> Res<()> {
    let failed = || "could not make the viewer window".to_string();
    if VIEWER.get().is_some() {
        return Err("the viewer window was open once already: one a program".to_string());
    }
    let app = msg!(Id; class("NSApplication"), "sharedApplication");
    if app.is_null() {
        return Err(failed());
    }
    msg!(bool; app, "setActivationPolicy:", if back { POLICY_ACCESSORY } else { POLICY_REGULAR } => isize);
    let object = viewer_object()?;
    msg!((); app, "setDelegate:", object => Id);
    viewer_menu(app);

    let shape = CGDisplayBounds(frames.display).size;
    let width = (CGDisplayBounds(CGMainDisplayID()).size.width * 0.6).round();
    let height = (width * shape.height / shape.width.max(1.0)).round();
    let content = Rect { origin: Point::default(), size: Size { width, height } };
    let window = msg!(Id; msg!(Id; class("NSWindow"), "alloc"), "initWithContentRect:styleMask:backing:defer:",
                      content => Rect, WINDOW_STYLE => usize, BUFFERED => usize, false => bool);
    if window.is_null() {
        return Err(failed());
    }
    msg!((); window, "setReleasedWhenClosed:", false => bool); // it is let go at the end of this function
    msg!((); window, "center");
    let title = Cf(cf_string("Second Mac - starting"));
    msg!((); window, "setTitle:", title.0 => Id);
    msg!((); window, "setDelegate:", object => Id);

    // The picture: a layer that fills the window, black where the picture does not reach.
    let picture = new_object("CALayer");
    let black = msg!(Id; class("NSColor"), "blackColor");
    msg!((); picture, "setBackgroundColor:", msg!(Id; black, "CGColor") => Id);
    msg!((); picture, "setContentsGravity:", KEEP_SHAPE => Id);
    msg!((); window, "setBackgroundColor:", black => Id);
    let view = msg!(Id; window, "contentView");
    msg!((); view, "setLayer:", picture => Id); // the layer first, then "wants": AppKit then never draws into it
    msg!((); view, "setWantsLayer:", true => bool);

    // The agent's mouse: the stamp's own drawing, as sharp as the screen the window is on.
    let scale = msg!(f64; window, "backingScaleFactor").max(1.0).min(8.0);
    let room = MOUSE_ROOM + MOUSE_TIP; // how far in the tip is from the drawing's top left
    let wide = ((MOUSE_BOX + 2.0 * MOUSE_ROOM) * scale).ceil() as usize;
    let high = wide;
    let mut drawn = vec![0u8; wide * high * 4];
    stamp_pointer(&mut drawn, wide, high, Point { x: room, y: room }, scale)?;
    let arrow = picture_from(wide, high, &drawn, None)?;
    let pointer = new_object("CALayer");
    msg!((); pointer, "setContents:", arrow.0 => Id);
    let size = Size { width: wide as f64 / scale, height: high as f64 / scale };
    let bounds = Rect { origin: Point::default(), size };
    msg!((); pointer, "setBounds:", bounds => Rect);
    // what gets placed is the tip, which is `room` points in from the top left
    let tip = Point { x: (room * scale).round() / wide as f64, y: 1.0 - (room * scale).round() / high as f64 };
    msg!((); pointer, "setAnchorPoint:", tip => Point);
    msg!((); pointer, "setHidden:", true => bool);
    msg!((); picture, "addSublayer:", pointer => Id);

    let viewer = Viewer {
        frames: frames.clone(),
        window: window as usize,
        picture: picture as usize,
        pointer: pointer as usize,
        shown: AtomicU64::new(0),
        ticks: AtomicU32::new(0),
    };
    if VIEWER.set(viewer).is_err() {
        return Err(failed());
    }
    POINTER_MOVED.store(true, Ordering::SeqCst);

    // The timer runs in the common modes, so also while you drag or resize the window.
    let timer = msg!(Id; class("NSTimer"), "timerWithTimeInterval:target:selector:userInfo:repeats:",
                     FRAME_INTERVAL.as_secs_f64() => f64, object => Id, selector("tick:") => Id, null_mut() => Id,
                     true => bool);
    let main_loop = msg!(Id; class("NSRunLoop"), "mainRunLoop");
    msg!((); main_loop, "addTimer:forMode:", timer => Id, RUN_LOOP_COMMON_MODES => Id);

    viewer_tick(object, null_mut(), null_mut()); // the picture and the title, when a picture is there already
    if back {
        msg!((); window, "orderBack:", null_mut() => Id);
    } else {
        msg!((); app, "activateIgnoringOtherApps:", true => bool);
        msg!((); window, "makeKeyAndOrderFront:", null_mut() => Id);
        msg!((); window, "orderFrontRegardless"); // in front also when macOS does not make this program the active one
    }
    msg!((); app, "run"); // until leave()

    msg!((); timer, "invalidate");
    msg!((); window, "setDelegate:", null_mut() => Id);
    msg!((); app, "setDelegate:", null_mut() => Id);
    msg!((); window, "close");
    for made in [pointer, picture, window, object] {
        msg!((); made, "release");
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

/// The second display for another program, over this program's own pipes.
/// Out on stdout, as messages: "frame", every new picture as a BMP file, and
/// "note", words about a picture that could not be taken or an order that
/// failed. In on stdin, one order a line: "run <app>" starts an app on the
/// second display, by its name or the path of its .app. It ends when stdin
/// closes, so when the other program ends, and every app on the second
/// display ends with it.
unsafe fn serve(frames: &Arc<Frames>) -> Res<()> {
    let pictures = frames.clone();
    thread::spawn(move || {
        let (mut seen, mut noted) = (0u64, None::<String>);
        loop {
            let (count, pixels, width, height, error) = {
                let latest = pictures.latest.lock().unwrap();
                (latest.count, latest.pixels.clone(), latest.width, latest.height, latest.error.clone())
            };
            // the frame loop keeps trying: each reason is told once
            if error != noted {
                if let Some(e) = &error {
                    send("note", format!("no picture: {e}").as_bytes());
                }
                noted = error;
            }
            // `count` only goes up when the display changed: one where nothing moved is not sent again
            if let (true, Some(pixels)) = (count != seen, pixels) {
                seen = count;
                if !send("frame", &bmp(width as i32, height as i32, &pixels)) {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    for line in std::io::stdin().lines() {
        let Ok(line) = line else { break };
        if let Some(app) = line.strip_prefix("run ") {
            match start(frames.display, app.trim(), &[]) {
                Ok(pid) => eprintln!("started on the second display: {} (process {pid})", app.trim()),
                Err(e) => {
                    send("note", format!("could not start on the second display: {}: {e}", app.trim()).as_bytes());
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- main

struct Args {
    shot: Option<String>,
    computer_use: bool,
    run: Vec<String>,
    scan: Option<String>,
    wait: f64,
    serve: bool,
    size: Option<(u32, u32)>,
    retina: bool,
    display: Option<u32>,
    /// Where the agent's mouse is put, in points from the display's top left.
    mouse: Option<(f64, f64)>,
    back: bool,
}

fn parse_args() -> Res<Args> {
    let mut args = Args {
        shot: None,
        computer_use: false,
        run: Vec::new(),
        scan: None,
        wait: 3.0,
        serve: false,
        size: None,
        retina: false,
        display: None,
        mouse: None,
        back: false,
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
            "--size" => {
                let size = value()?;
                let parsed = size.split_once(['x', 'X']).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                match parsed {
                    Some((w, h)) if (320..=8192).contains(&w) && (240..=8192).contains(&h) => args.size = Some((w, h)),
                    _ => return Err("--size needs a size like 1920x1080, from 320x240 to 8192x8192".to_string()),
                }
            }
            "--display" => {
                let id = value()?.parse().map_err(|_| "--display needs the number of a display".to_string())?;
                args.display = Some(id);
            }
            "--mouse" => {
                let at = value()?;
                let parsed = at.split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)));
                match parsed {
                    Some((x, y)) if x >= 0.0 && y >= 0.0 => args.mouse = Some((x, y)),
                    _ => return Err("--mouse needs a point like 300,200: points from the top left of the second \
                                     display".to_string()),
                }
            }
            "--computer-use" => args.computer_use = true,
            "--serve" => args.serve = true,
            "--retina" => args.retina = true,
            "--back" => args.back = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option: {arg}")),
        }
    }
    // What cannot both be done is refused here, before anything is started.
    let asked = [("--shot", args.shot.is_some()), ("--scan", args.scan.is_some()),
                 ("--computer-use", args.computer_use), ("--serve", args.serve)];
    let modes: Vec<&str> = asked.iter().filter(|(_, given)| *given).map(|(name, _)| *name).collect();
    if modes.len() > 1 {
        return Err(format!("{} cannot go together: a run does one of them", modes.join(" and ")));
    }
    if args.computer_use && !args.run.is_empty() {
        return Err("--run cannot go with --computer-use: that one starts Calculator and TextEdit itself".to_string());
    }
    if args.display.is_some() && (args.size.is_some() || args.retina) {
        return Err("--size and --retina are for a display this program makes: with --display the display stays \
                    as it is".to_string());
    }
    if let (true, Some(mode)) = (args.back, modes.iter().find(|mode| **mode != "--computer-use")) {
        return Err(format!("--back cannot go with {mode}: there is no live window then"));
    }
    if args.mouse.is_some() && args.serve {
        return Err("--mouse cannot go with --serve: the pictures sent there do not have the mouse in them yet"
            .to_string());
    }
    Ok(args)
}

/// Start the apps on the second display and do what was asked there. An app
/// that could not be started is said and counted in `not_started`: the rest
/// is done all the same, and the run then ends with code 1.
unsafe fn go(args: &Args, frames: &Arc<Frames>, before: &Before,
             capture_thread: &mut Option<thread::JoinHandle<()>>, not_started: &mut u32) -> Res<()> {
    let display = frames.display;
    let mut apps: Vec<(String, Vec<String>)> = Vec::new();
    if args.computer_use {
        key_letter(0); // the letters of the keys are read here, on the main thread
        apps.push(("Calculator".to_string(), Vec::new()));
        apps.push(("TextEdit".to_string(), vec![lines_file()?]));
    } else if args.run.is_empty() && !args.serve {
        apps.push(("Calculator".to_string(), Vec::new()));
    } else {
        apps.extend(args.run.iter().map(|app| (app.clone(), Vec::new())));
    }
    for (app, files) in &apps {
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let hidden = |pid| started_apps().iter().any(|app| app.process.pid == pid && app.stage == Stage::Hidden);
        let said = match start(display, app, &files) {
            Ok(pid) if hidden(pid) => format!("started, hidden: {app} (process {pid}) has no window yet"),
            Ok(pid) => format!("started on the second display: {app} (process {pid})"),
            Err(e) => {
                *not_started += 1;
                format!("could not start on the second display: {app}: {e}")
            }
        };
        if args.serve {
            eprintln!("{said}"); // stdout carries the pictures then
        } else {
            println!("{said}");
        }
    }

    if let Some(png) = &args.scan {
        pump(args.wait); // the apps get to start
        let found = scan_display(display, png)?;
        report(&scan_rows(&found), before);
        return Ok(());
    }
    if args.computer_use && !args.back {
        let acted = computer_use(display);
        report_your_side(before);
        return acted;
    }

    let pictures = frames.clone();
    *capture_thread = Some(thread::spawn(move || unsafe { capture_loop(&pictures) }));
    if args.serve {
        return serve(frames);
    }
    if let Some(png) = &args.shot {
        return take_shot(png, args.wait, frames, before);
    }
    println!("The live window opens on your main screen, {}. Closing it, or Ctrl+C here, ends everything on it.",
             if args.back { "behind your other windows" } else { "in front" });
    if !args.computer_use {
        return show_viewer(frames, args.back);
    }
    // Computer use with --back: the live window is open behind your windows
    // while the agent acts, on a thread of its own, and closes when it is done.
    let watched = frames.clone();
    let agent = thread::spawn(move || unsafe {
        let pool = objc_autoreleasePoolPush();
        let acted = computer_use(watched.display);
        objc_autoreleasePoolPop(pool);
        if stop_asked().is_none() {
            sleep(1.0); // the last step gets into the window
        }
        watched.stop.store(true, Ordering::SeqCst);
        acted
    });
    let shown = show_viewer(frames, true);
    // Closed by you while the agent acts: it stops before its next step, see `stop_asked`.
    WINDOW_CLOSED.store(true, Ordering::SeqCst);
    let acted = agent.join().unwrap_or_else(|_| Err("the agent's thread broke".to_string())); // Rust printed why
    report_your_side(before);
    shown.and(acted)
}

unsafe fn run(args: &Args) -> i32 {
    let (front, front_name) = front_app();
    let before = Before { front, front_name, pointer: pointer() };
    // Ctrl+C, "kill" and a closed terminal window (SIGHUP) only raise a flag: the apps are
    // ended before the display goes.
    for stop_on in [SIGHUP, SIGINT, SIGTERM] {
        let was = signal(stop_on, interrupted as *const () as usize);
        if stop_on == SIGHUP && was == IGNORE {
            signal(SIGHUP, IGNORE); // started with nohup: it goes on when its terminal closes
        }
    }
    // The guard comes first: no app is started without it.
    let opened = match start_guard() {
        Ok(()) => open_display(args),
        Err(e) => Err(e),
    };
    let (display, made) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let said = format!("Second display: {} (your main screen: {})", place_of(display), place_of(CGMainDisplayID()));
    if args.serve {
        eprintln!("{said}");
    } else {
        println!("{said}");
    }
    if let Some((x, y)) = args.mouse {
        let size = CGDisplayBounds(display).size;
        if x >= size.width || y >= size.height {
            eprintln!("--mouse {x:.0},{y:.0} is not on the second display: it is {:.0} by {:.0} points",
                      size.width, size.height);
            return 1; // nothing was started yet: the display goes with the program
        }
        set_pointer(Some(Point { x, y }));
    }
    watch(display);
    let frames = Arc::new(Frames { display, stop: AtomicBool::new(false), latest: Mutex::new(Latest::default()) });

    let (mut capture_thread, mut not_started) = (None, 0);
    let result = go(args, &frames, &before, &mut capture_thread, &mut not_started);

    // However it went: stop the pictures, end every app on the second display, then let it go.
    frames.stop.store(true, Ordering::SeqCst);
    if let Some(capture) = capture_thread {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !capture.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
    if !end_all() {
        eprintln!("an app on the second display did not end");
    }
    remove_lines_file(); // TextEdit has ended
    drop(made);
    match result {
        // A run that Ctrl+C or kill stopped ends with 1 in every mode, as the watcher ends it.
        Ok(()) if INTERRUPTED.load(Ordering::SeqCst) => {
            let _ = writeln!(std::io::stderr(), "{STOPPED}: the apps on the second display were ended");
            1
        }
        Ok(()) if not_started > 0 => 1, // said when it happened
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn main() {
    // Started a second time by itself: this copy is the guard of the apps.
    if std::env::args().nth(1).as_deref() == Some(GUARD_WORD) {
        guard();
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    let code = unsafe {
        let pool = objc_autoreleasePoolPush();
        let code = run(&args);
        objc_autoreleasePoolPop(pool);
        code
    };
    std::process::exit(code);
}
