// AutoCua - Android screen reader (the phone helper's screenshots + UI tree, Rust)
//
// READS ONLY, and not its own connection. UIElementScanner at the end of
// this file is the class the agent loop reads the phone with, the copy of
// ios/tree/element.py's UIElementScanner (scan_elements, get_scan_data,
// application_name); tree/test.py drives the same class by hand.
//
// Both halves of a scan come from one helper on the phone, AutoCua's
// Helper.java, which the connector starts and forwards to one local port
// (android_connector/session.py, android_session, over stream.py):
//   scans   the helper answers "dump" with the accessibility tree in
//           uiautomator's XML format. It keeps its connection open, so there
//           is none of `uiautomator dump`'s one-second wait for a still
//           screen. The scan then does what the iPhone scanner does: keep
//           only the element types that matter, under the iPhone's names
//           (button, statictext, textfield...), number them, and hand the
//           loop the tree text, one JSON entry per element (the controller's
//           map), and the screenshot with numbered magenta boxes. The three
//           also go to debug/android/ (tree.txt, elements.json, scan.jpg)
//           for a look after a run.
//   shots   right after the tree, "shot" asks for a screenshot. The helper
//           takes it through the same UiAutomation, scales it to the model's
//           caps and compresses it to JPEG on the phone. Decoded here, it
//           gets the marks; as it came, it is the app's plain screenshot.
//
// The helper (Helper.java, one socket): send a line ("dump", "shot MAX_EDGE
// MAX_PIXELS QUALITY", "fill ..." or "ping"), get a 4-byte big-endian length
// and that many bytes: the XML (UTF-8), the JPEG (it starts FF D8), "OK", or
// a line starting "ERROR:". The scanner, the vault, the back tool and the
// video player share one connection (helper()). The preview opens its own,
// and the helper answers each connection on a thread of its own.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use roxmltree::{Node, NodeId};
use serde_json::{json, Map, Value};
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::JpegDecoder;

use crate::controller::service::controller_service;

// ========== FLAGS ==========
/// Set to true to save files to debug folders, false for direct LLM only:
/// ios/tree/element.py's DEBUG. With it every scan of a run is kept, the
/// tree as debug/element/ui_elements_<time>.txt and the marked screenshot
/// (the bytes the model gets) as debug/screenshot/ui_elements_screenshot_<time>.jpg,
/// and the app's screen box gets that marked screenshot instead of the plain one.
const DEBUG: bool = false;

/// Where each scan's files are written, relative to the process CWD: main.py
/// and test.py run from the repo root, so they land in the root debug/
/// folder (gitignored). The last scan only; each one overwrites the three.
const DEFAULT_OUT_DIR: &str = "debug/android";

/// The helper's JPEG quality for the model's screenshot (Bitmap.compress,
/// 4:2:0). Decoded here for the marks; as it came, it is the app's plain
/// screenshot.
const SHOT_QUALITY: u8 = 90;
/// Scans use ios/tree/element.py's LLM_IMAGE_QUALITY and full-size colour
/// (4:4:4), so the magenta digits stay crisp.
const SCAN_QUALITY: u8 = 85;

/// The model's image caps, ios/tree/element.py's LLM_IMAGE_MAX_EDGE and
/// LLM_IMAGE_MAX_PIXELS. The helper applies them on the phone with
/// element.py's rule, each side floored.
const LLM_IMAGE_MAX_EDGE: u32 = 2300;
const LLM_IMAGE_MAX_PIXELS: u32 = 3_300_000;

/// Longest wait for one answer from the helper (a tree or a screenshot): a
/// guard against a wedged helper, not a performance budget.
const TREE_WAIT: Duration = Duration::from_secs(10);

/// No real tree or screenshot comes near this; a bigger length means the
/// connection is out of step.
const MAX_TREE_BYTES: usize = 64 << 20;

/// ios/tree/element.py's MIN_ELEMENT_SIZE (2 points). Android counts in dp,
/// the same unit, turned into pixels with the screen's density.
const MIN_ELEMENT_DP: f64 = 2.0;

/// An element partly under a tappable one drawn over it still shows when an
/// uncovered part at least this big each way is left; a thinner sliver (boxes
/// a few pixels apart) is nothing to tap.
const MIN_SHOWN_DP: f64 = 8.0;

/// Names longer than this are cut to 47 characters plus "...", as on iOS.
const MAX_NAME_CHARS: usize = 50;

/// Virtual tap points along a slider, as on iOS: 8 x 20 dp, evenly spaced.
const SLIDER_TICKS: usize = 5;
const TICK_W_DP: f64 = 8.0;
const TICK_H_DP: f64 = 20.0;

/// Index labels. iOS draws "[n]" in 32 px Arial Bold; with no font files
/// here, a 5x7 bitmap font at 4x gives the same 28 px digit height. The dark
/// rim hugs the glyphs (no filled chip), so the UI underneath stays visible.
const GLYPH_SCALE: i64 = 4;
const LABEL_RIM: i64 = 3;
const MAGENTA: [u8; 3] = [255, 0, 255];
const RIM: [u8; 3] = [0, 0, 0];

/// What a scan needs to know about the phone, read once at start.
struct Device {
    adb: String,
    serial: String,
    /// Display size in pixels, in the screen's natural orientation (0 if
    /// unknown: the tree's own root box is used then).
    width: f64,
    height: f64,
    /// dpi; 160 means one pixel per dp.
    density: f64,
    /// The home-screen app's package, "" if unknown.
    launcher: String,
}

/// One element kept by a scan, in screen pixels.
struct Elem {
    /// The iPhone name of its kind: "Button", "StaticText", "TextField"...
    kind: &'static str,
    /// Whitespace already collapsed; cut to MAX_NAME_CHARS only when written.
    name: String,
    value: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    depth: usize,
    /// The node it came from (none for slider ticks).
    node: Option<NodeId>,
    /// Hidden: wholly under a tappable element drawn over it, this one (with
    /// its own name, for when it is not listed itself).
    under: Option<(NodeId, String)>,
}

impl Elem {
    /// Hidden elements get no number: they are listed in the tree for the
    /// record, and left out of the JSON and the marks.
    fn numbered(&self) -> bool {
        self.under.is_none()
    }
}

/// A box as (x, y, width, height), in screen pixels.
type Rect = (f64, f64, f64, f64);

/// One raw screenshot over adb (`screencap` without -p, so no PNG to make or
/// read): a header of little-endian u32s, width, height, pixel format and on
/// newer Androids the colour space (12 or 16 bytes), then 4 bytes a pixel.
/// As RGB, width, height. The stand-in for a helper that took no screenshot
/// (see grab).
fn screencap(adb: &str, serial: &str) -> Result<(Vec<u8>, usize, usize), String> {
    let out = Command::new(adb)
        // One argument: the phone's shell runs it (sh -c), so its stderr is
        // dropped there. exec-out sends stderr down the same stream, and on
        // Android 14+ with two displays (foldables) screencap warns there
        // before the picture, which would break the header.
        .args(["-s", serial, "exec-out", "screencap 2>/dev/null"])
        .output()
        .map_err(|e| format!("screencap: {e}"))?;
    let b = out.stdout;
    if b.len() < 12 {
        return Err(format!("screencap: {} bytes", b.len()));
    }
    let word = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap()) as usize;
    let (w, h, format) = (word(0), word(4), word(8));
    // RGBA_8888 (1), RGBX_8888 (2), BGRA_8888 (5): the formats a phone gives.
    if !matches!(format, 1 | 2 | 5) || w == 0 || h == 0 {
        return Err(format!("screencap: {w}x{h}, pixel format {format}"));
    }
    let head = b
        .len()
        .checked_sub(w * h * 4)
        .filter(|n| *n == 12 || *n == 16)
        .ok_or_else(|| format!("screencap: {} bytes for {w}x{h}", b.len()))?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in b[head..].chunks_exact(4) {
        if format == 5 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        } else {
            rgb.extend_from_slice(&px[..3]);
        }
    }
    Ok((rgb, w, h))
}

/// An RGB picture scaled to `dw` x `dh`, bilinear (a screencap to the
/// model's caps).
fn scale(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 3];
    let (fx, fy) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
    for y in 0..dh {
        let sy = ((y as f64 + 0.5) * fy - 0.5).max(0.0);
        let y0 = (sy as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = sy - y0 as f64;
        for x in 0..dw {
            let sx = ((x as f64 + 0.5) * fx - 0.5).max(0.0);
            let x0 = (sx as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let wx = sx - x0 as f64;
            for c in 0..3 {
                let p = |xx: usize, yy: usize| src[(yy * sw + xx) * 3 + c] as f64;
                let top = p(x0, y0) * (1.0 - wx) + p(x1, y0) * wx;
                let bottom = p(x0, y1) * (1.0 - wx) + p(x1, y1) * wx;
                out[(y * dw + x) * 3 + c] = (top * (1.0 - wy) + bottom * wy).round() as u8;
            }
        }
    }
    out
}

// ============================================================ the scanner

/// What the loop reads of the last scan. The tree text stays from the last
/// scan that worked; the picture is dropped the moment a new scan starts, so
/// a failed scan never hands the loop the previous screen as if it were now
/// (ios/tree/element.py does the same).
#[derive(Default)]
struct ScanState {
    element_tree_text: String,
    image_base64: Option<String>,
    /// The app in front: its package, "" on the home screen (as "" is the
    /// iPhone's SpringBoard).
    application_name: String,
}

/// The scanner the agent loop reads the phone with: the Rust copy of
/// ios/tree/element.py's UIElementScanner, with the same calls
/// (scan_elements, get_scan_data, application_name), and a Python class too
/// so tree/test.py can drive it by hand.
///
/// The connection is the connector's: android_connector/session.py's
/// android_session starts the helper (agent_launcher does it before the run
/// and stops it after; new() asks it as well, in case nobody has) and this
/// reads the port it forwarded. The phone goes to the controller once
/// (set_phone), the elements and the display size after every scan.
#[pyclass(frozen)]
pub struct UIElementScanner {
    /// The app's screen box, handed each scan's plain screenshot (base64)
    /// as on iOS; none on the terminal.
    frontend_callback: Option<Py<PyAny>>,
    device: Device,
    scan: Mutex<ScanState>,
}

#[pymethods]
impl UIElementScanner {
    #[new]
    #[pyo3(signature = (frontend_callback=None))]
    pub fn new(py: Python<'_>, frontend_callback: Option<Py<PyAny>>) -> PyResult<Self> {
        let session = py.import("AutoCua.android_connector.session")?.getattr("android_session")?;
        let info = session.call_method0("activate")?;
        if !info.get_item("ok")?.extract::<bool>()? {
            let why: String = info.get_item("error")?.extract()?;
            return Err(PyRuntimeError::new_err(format!("📱 Android connection failed: {why}")));
        }
        let adb: String = info.get_item("adb")?.extract()?;
        let serial: String = info.get_item("serial")?.extract()?;
        let tree_port: u16 = info.get_item("tree_port")?.extract()?;
        // activate() returned once the helper answered on this port
        // (stream.py's _wait_ready).
        use_helper(tree_port);
        // The run's window (utils/sim_view.py) shows this screen from
        // the preview, when the launcher asked for one.
        if let Some(port) = std::env::var(PREVIEW_PORT_ENV).ok().and_then(|p| p.trim().parse::<u16>().ok()) {
            serve_preview(port);
        }
        // Size, density and home-screen app over adb (one call).
        let device = py.detach(|| device_info(adb, serial));
        controller_service().set_phone(&device.adb, &device.serial, device.density);
        Ok(UIElementScanner {
            frontend_callback,
            device,
            scan: Mutex::new(ScanState::default()),
        })
    }

    /// Scan the phone's screen: the tree and a screenshot from the helper,
    /// kept here for get_scan_data, the elements handed to the controller,
    /// the files written to debug/android/.
    pub fn scan_elements(&self, py: Python<'_>) -> PyResult<()> {
        self.scan.lock().unwrap().image_base64 = None;
        // Pure Rust from here to the pictures: the GIL is not held meanwhile.
        let cap = py
            .detach(|| {
                let mut tree = helper().lock().unwrap();
                let tree = tree.as_mut().ok_or("no phone helper connected")?;
                capture(&self.device, tree)
            })
            .map_err(PyRuntimeError::new_err)?;

        let controller = controller_service();
        controller.update_elements(cap.elements.clone());
        controller.set_screen(cap.disp_w, cap.disp_h);
        // The same bytes the model gets, for a look after a run. Never a
        // reason to fail the scan.
        if let Err(e) = write_scan(Path::new(DEFAULT_OUT_DIR), &cap) {
            eprintln!("element: {e}");
        }
        // Save to debug folder ONLY if DEBUG is enabled: every scan, as on iOS.
        if DEBUG {
            if let Err(e) = write_debug(Path::new("debug"), &cap) {
                eprintln!("element: {e}");
            }
        }
        {
            let mut scan = self.scan.lock().unwrap();
            scan.element_tree_text = cap.tree_text;
            scan.image_base64 = Some(base64::engine::general_purpose::STANDARD.encode(&cap.jpeg));
            scan.application_name = cap.application_name;
        }
        println!("{}", cap.summary);

        // Send to frontend: the plain screenshot, as on iOS (the marked one
        // with DEBUG, for debugging).
        let shown = if DEBUG { &cap.jpeg } else { &cap.plain };
        if let Some(callback) = &self.frontend_callback {
            callback.call1(py, (base64::engine::general_purpose::STANDARD.encode(shown),))?;
        }
        Ok(())
    }

    /// Get scan data for AgentService: the element tree text and the
    /// annotated screenshot as base64 (None when the last scan failed).
    pub fn get_scan_data(&self, _py: Python<'_>) -> PyResult<(String, Option<String>)> {
        let scan = self.scan.lock().unwrap();
        Ok((scan.element_tree_text.clone(), scan.image_base64.clone()))
    }

    /// The app in front at the last scan: its package, "" on the home screen.
    #[getter]
    pub fn application_name(&self) -> String {
        self.scan.lock().unwrap().application_name.clone()
    }

    /// A screenshot from the helper to debug/android/shot.jpg, the JPEG as it
    /// came (tree/test.py's "p"). Returns one line saying what was written.
    pub fn shot(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| shot(Path::new(DEFAULT_OUT_DIR))).map_err(PyRuntimeError::new_err)
    }
}

// ============================================================ shots

/// One screenshot: the JPEG as it came (the app's plain screenshot) and its
/// pixels (the scan draws its marks on them).
struct Shot {
    jpeg: Vec<u8>,
    rgb: Vec<u8>,
    w: usize,
    h: usize,
}

/// A JPEG as RGB, width, height.
fn decode_jpeg(jpeg: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    let mut decoder = JpegDecoder::new(ZCursor::new(jpeg));
    let rgb = decoder.decode().map_err(|e| format!("jpeg: {e}"))?;
    let (w, h) = decoder.dimensions().ok_or("jpeg: no size")?;
    if rgb.len() != w * h * 3 {
        return Err(format!("jpeg: {} bytes for {w}x{h}", rgb.len()));
    }
    Ok((rgb, w, h))
}

/// The model's caps on a w x h picture, by the same rule the helper applies
/// on the phone.
fn capped(w: usize, h: usize) -> (usize, usize) {
    let s = (LLM_IMAGE_MAX_EDGE as f64 / w.max(h) as f64)
        .min((LLM_IMAGE_MAX_PIXELS as f64 / (w * h) as f64).sqrt());
    if s >= 1.0 {
        return (w, h);
    }
    (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1))
}

/// One screenshot for a scan or a look: the helper's JPEG, decoded. When the
/// helper took none (its UiAutomation screenshot failed, or a jar from
/// before "shot" answered with a tree), one raw screencap over adb stands
/// in: the same picture, capped the same way, but slower (10 MB down the
/// cable).
fn grab(tree: &mut Tree) -> Result<Shot, String> {
    let why = match tree.shot(LLM_IMAGE_MAX_EDGE, LLM_IMAGE_MAX_PIXELS, SHOT_QUALITY)? {
        ShotAnswer::Jpeg(jpeg) => {
            let (rgb, w, h) = decode_jpeg(&jpeg)?;
            return Ok(Shot { jpeg, rgb, w, h });
        }
        ShotAnswer::Refused(why) => why,
    };
    // Said once: a helper that cannot take screenshots says no every time.
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!("[scan] the helper took no screenshot ({why}): screencap over adb instead, slower");
    }
    let phone = controller_service().phone().ok_or("no phone connected")?;
    let (rgb, w, h) = screencap(&phone.adb, &phone.serial)?;
    let (cw, ch) = capped(w, h);
    let rgb = if (cw, ch) == (w, h) { rgb } else { scale(&rgb, w, h, cw, ch) };
    let jpeg = encode_jpeg(&rgb, cw, ch, SHOT_QUALITY, SamplingFactor::F_2_2)?;
    Ok(Shot { jpeg, rgb, w: cw, h: ch })
}

/// A fresh screenshot as RGB, width, height (the video player's looks).
pub fn screenshot_rgb() -> Result<(Vec<u8>, usize, usize), String> {
    let mut tree = helper().lock().unwrap();
    let shot = grab(tree.as_mut().ok_or("no phone helper connected")?)?;
    Ok((shot.rgb, shot.w, shot.h))
}

/// Saves a screenshot from the helper as shot.jpg, the JPEG as it came, and
/// says what was written.
fn shot(out_dir: &Path) -> Result<String, String> {
    let t0 = Instant::now();
    let Shot { jpeg, w, h, .. } = {
        let mut tree = helper().lock().unwrap();
        grab(tree.as_mut().ok_or("no phone helper connected")?)?
    };
    let path = out_dir.join("shot.jpg");
    write(&path, &jpeg)?;
    Ok(format!(
        "shot -> {}  {w}x{h}  {} KB  {} ms",
        path.display(),
        jpeg.len() / 1024,
        t0.elapsed().as_millis()
    ))
}

// ============================================================ preview

/// The launcher's port for the preview of this process's screen
/// (agent_launcher sets it for an emulator run, one port per task).
const PREVIEW_PORT_ENV: &str = "AutoCua_ANDROID_PREVIEW_PORT";
/// The preview: a JPEG every PREVIEW_INTERVAL at most (3 a second), its long
/// side PREVIEW_EDGE px at quality PREVIEW_QUALITY, scaled and compressed on
/// the phone. A picture to watch, not the one the model reads.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(333);
const PREVIEW_EDGE: u32 = 1280;
const PREVIEW_QUALITY: u8 = 70;
/// No screenshot for this long: the helper is gone; clients are let go, so
/// the window shows the phone as down.
const PREVIEW_GONE: Duration = Duration::from_secs(5);

/// The helper's forwarded port, for the preview's own connection (0 until
/// use_helper names one). A new helper's port replaces it, and the preview
/// follows.
static HELPER_PORT: AtomicU16 = AtomicU16::new(0);

/// The newest preview JPEG, one for every client.
struct Preview {
    jpeg: Arc<Vec<u8>>,
    /// Counts up with each new JPEG; a client sends the ones it has not.
    seq: u64,
    /// Clients connected now: with none, nothing is fetched.
    clients: usize,
    /// A fetch thread is running.
    fetching: bool,
    /// Nothing came from the helper for PREVIEW_GONE.
    gone: bool,
}

fn preview() -> &'static (Mutex<Preview>, Condvar) {
    static PREVIEW: OnceLock<(Mutex<Preview>, Condvar)> = OnceLock::new();
    PREVIEW.get_or_init(|| {
        let empty = Preview { jpeg: Arc::new(Vec::new()), seq: 0, clients: 0, fetching: false, gone: false };
        (Mutex::new(empty), Condvar::new())
    })
}

/// Serve the screen as MJPEG on 127.0.0.1:`port`, the way WebDriverAgent
/// serves a simulator's (port 9100), so the run's window shows an emulator
/// like an iOS Simulator. One server per port (the app builds a scanner per
/// run, and a later run may bring another port). A client gets the newest
/// JPEG at once, then every new one until it hangs up: while anyone is
/// connected, one thread asks the helper for a JPEG every PREVIEW_INTERVAL,
/// on a connection of its own, for all the clients at once; with nobody
/// watching, nothing is fetched.
fn serve_preview(port: u16) {
    static SERVED: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    let mut served = SERVED.lock().unwrap();
    if served.contains(&port) {
        return;
    }
    let listener = match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[preview] port {port}: {e}");
            return;
        }
    };
    served.push(port);
    thread::spawn(move || {
        for client in listener.incoming().flatten() {
            thread::spawn(move || preview_client(client));
        }
    });
}

/// The preview's fetch thread, started by its first client: a "shot" every
/// PREVIEW_INTERVAL on a connection of its own (never the helper() mutex, so
/// a scan never waits behind a preview frame), until the last client has
/// gone. It follows HELPER_PORT, so a restarted helper is picked up and no
/// dead one is kept.
fn preview_fetch() {
    let (slot, changed) = preview();
    let mut tree: Option<Tree> = None;
    let mut last_ok = Instant::now();
    loop {
        {
            let mut p = slot.lock().unwrap();
            if p.clients == 0 {
                p.fetching = false;
                return;
            }
        }
        let t0 = Instant::now();
        let port = HELPER_PORT.load(Ordering::Relaxed);
        if tree.as_ref().map_or(true, |t| t.port != port) {
            tree = (port != 0).then(|| Tree { port, sock: None });
        }
        let answer = tree.as_mut().map(|t| t.shot(PREVIEW_EDGE, 0, PREVIEW_QUALITY));
        {
            let mut p = slot.lock().unwrap();
            if let Some(Ok(ShotAnswer::Jpeg(jpeg))) = answer {
                p.jpeg = Arc::new(jpeg);
                p.seq += 1;
                p.gone = false;
                last_ok = Instant::now();
            } else if last_ok.elapsed() >= PREVIEW_GONE {
                p.gone = true;
            }
        }
        changed.notify_all();
        thread::sleep(PREVIEW_INTERVAL.saturating_sub(t0.elapsed()));
    }
}

fn preview_client(mut client: TcpStream) {
    // The request line and headers, dropped: any path gets the stream.
    let _ = client.set_read_timeout(Some(Duration::from_secs(2)));
    let mut sink = [0u8; 1024];
    let _ = client.read(&mut sink);
    let (slot, changed) = preview();
    // The header goes at once, so the window shows the phone as up. After
    // the helper went, it waits for the first JPEG of a new one instead:
    // the window shows the phone as down until then, not up and down at
    // every try.
    let (mut headed, mut sent) = {
        let mut p = slot.lock().unwrap();
        p.clients += 1;
        if !p.fetching {
            p.fetching = true;
            thread::spawn(preview_fetch);
        }
        (!p.gone, if p.gone { p.seq } else { 0 })
    };
    let head = b"HTTP/1.0 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\
                 Cache-Control: no-cache\r\nConnection: close\r\n\r\n";
    if headed && client.write_all(head).is_err() {
        slot.lock().unwrap().clients -= 1;
        return;
    }
    // Short, for the peek below: a live peer that sent nothing more shows as
    // a timeout, a closed one as 0 bytes.
    let _ = client.set_read_timeout(Some(Duration::from_millis(100)));
    loop {
        let next = {
            let p = slot.lock().unwrap();
            let (p, _) = changed
                .wait_timeout_while(p, Duration::from_secs(1), |p| p.seq == sent && !(headed && p.gone))
                .unwrap();
            if headed && p.gone {
                break;
            }
            if p.seq == sent { None } else { Some((Arc::clone(&p.jpeg), p.seq)) }
        };
        let Some((jpeg, seq)) = next else {
            // Nothing new. A client still without its header has never been
            // written to, so only a peek says whether it is still there;
            // otherwise its thread would outlive it and keep the fetch going.
            if !headed && matches!(client.peek(&mut [0u8; 1]), Ok(0)) {
                break;
            }
            continue;
        };
        if !headed {
            if client.write_all(head).is_err() {
                break;
            }
            headed = true;
        }
        let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
        if client.write_all(part.as_bytes()).is_err()
            || client.write_all(&jpeg).is_err()
            || client.write_all(b"\r\n").is_err()
            || client.flush().is_err()
        {
            break;
        }
        sent = seq;
    }
    slot.lock().unwrap().clients -= 1;
}

// ============================================================ device

/// Display size, density and home-screen app, from one adb call. A value it
/// cannot read stays unknown; a scan then falls back (see Device).
fn device_info(adb: String, serial: String) -> Device {
    let mut dev = Device { adb, serial, width: 0.0, height: 0.0, density: 160.0, launcher: String::new() };
    let out = Command::new(&dev.adb)
        .args(["-s", &dev.serial, "shell",
               "wm size; wm density; cmd package resolve-activity --brief \
                -a android.intent.action.MAIN -c android.intent.category.HOME"])
        .output();
    let Ok(out) = out else { return dev };
    // "Physical size: 1080x2400", "Override size: ..." (the override wins,
    // and is listed after), "Physical density: 420", and the launcher's
    // "package/.Activity" on the last line of the resolve-activity answer.
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if let Some(size) = line.split("size:").nth(1) {
            if let Some((w, h)) = size.trim().split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                    dev.width = w;
                    dev.height = h;
                }
            }
        } else if let Some(dpi) = line.split("density:").nth(1) {
            if let Ok(dpi) = dpi.trim().parse() {
                dev.density = dpi;
            }
        } else if let Some((package, _)) = line.split_once('/') {
            if !package.contains(' ') && !package.is_empty() {
                dev.launcher = package.to_string();
            }
        }
    }
    dev
}

// ============================================================ scan

/// Everything one scan produces, in memory: the loop sends the tree text and
/// the marked picture to the model, the controller gets the elements by
/// number, and the debug files are written from the same bytes.
struct Capture {
    tree_text: String,
    /// elements.json's text, and the same entries as the controller's map.
    json: String,
    elements: Map<String, Value>,
    jpeg: Vec<u8>,
    /// The screenshot before the marks, the helper's JPEG as it came, for
    /// the app's screen box.
    plain: Vec<u8>,
    /// The app in front: its package, "" on the home screen.
    application_name: String,
    /// The display's size in pixels, in the screenshot's orientation.
    disp_w: f64,
    disp_h: f64,
    /// One line: the counts and the timings.
    summary: String,
}

/// One scan: the tree, then a screenshot, both from the helper, then the
/// elements, the tree text, the JSON and the marked screenshot.
fn capture(dev: &Device, tree: &mut Tree) -> Result<Capture, String> {
    let t0 = Instant::now();
    let xml = tree.dump()?;
    let t_dump = Instant::now();
    // Asked for right after the tree, so both show the same moment as nearly
    // as they can.
    let Shot { jpeg: plain, mut rgb, w: fw, h: fh } = grab(tree)?;
    let t_shot = Instant::now();

    let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("bad XML from uiautomator: {e}"))?;
    // One root per window: the app's first, then the system's bars.
    let windows: Vec<Node> = doc.root_element().children().filter(|n| n.has_tag_name("node")).collect();
    let root = *windows.first().ok_or("uiautomator returned an empty tree")?;
    let (rx, ry, rw, rh) = bounds(attr(root, "bounds")).unwrap_or((0.0, 0.0, fw as f64, fh as f64));

    // The display box, turned to the screenshot's orientation (wm size
    // reports the natural one). Unknown: the root window's box stands in for it.
    let (mut disp_w, mut disp_h) = if dev.width > 0.0 { (dev.width, dev.height) } else { (rx + rw, ry + rh) };
    if (fw > fh) != (disp_w > disp_h) {
        std::mem::swap(&mut disp_w, &mut disp_h);
    }
    let (disp_w, disp_h) = (disp_w.max(1.0), disp_h.max(1.0));

    let package = attr(root, "package").to_string();
    let mut walk = Walk {
        launcher: &dev.launcher,
        dp: dev.density / 160.0,
        disp_w,
        disp_h,
        paths: draw_paths(&windows),
        taps: tappable(&windows),
        out: Vec::new(),
    };
    // The app itself comes first, as the iPhone's Application element does,
    // and everything else nests under it.
    let (rx, ry, rw, rh) = walk.clip((rx, ry, rw, rh));
    walk.out.push(Elem {
        kind: "Application",
        name: package.clone(),
        value: String::new(),
        x: rx,
        y: ry,
        w: rw,
        h: rh,
        depth: 0,
        node: Some(root.id()),
        under: None,
    });
    for child in root.children().filter(|n| n.has_tag_name("node")) {
        walk.node(child, 1, "");
    }
    // The system's bars are listed under the app too (Back / Home / Recent
    // apps, the status bar's clock); a phone with gestures has no buttons.
    for window in &windows[1..] {
        walk.node(*window, 1, "");
    }
    let elems = walk.out;
    let hidden = elems.iter().filter(|e| e.under.is_some()).count();

    let tablet = disp_w.min(disp_h) / (dev.density / 160.0) >= 600.0;
    let home = !dev.launcher.is_empty() && package == dev.launcher;
    let app = if home { "home screen" } else { package.as_str() };
    let tree = tree_text(&elems, tablet, fw > fh, app);
    let json = elements_json(&elems);
    let elements = elements_map(&elems);
    let t_tree = Instant::now();

    annotate(&mut rgb, fw, fh, &elems, fw as f64 / disp_w, fh as f64 / disp_h);
    let t_draw = Instant::now();
    let jpeg = encode_jpeg(&rgb, fw, fh, SCAN_QUALITY, SamplingFactor::F_1_1)?;
    let t_jpeg = Instant::now();

    let ms = |a: Instant, b: Instant| b.duration_since(a).as_millis();
    let summary = format!(
        "[scan] {} elements{}  dump={} ms shot={} ms tree={} ms draw={} ms jpeg={} ms total={} ms",
        elems.len() - hidden,
        if hidden > 0 { format!(" ({hidden} hidden)") } else { String::new() },
        ms(t0, t_dump),
        ms(t_dump, t_shot),
        ms(t_shot, t_tree),
        ms(t_tree, t_draw),
        ms(t_draw, t_jpeg),
        t0.elapsed().as_millis()
    );
    Ok(Capture {
        tree_text: tree,
        json,
        elements,
        jpeg,
        plain,
        application_name: if home { String::new() } else { package },
        disp_w,
        disp_h,
        summary,
    })
}

/// The scan's three files: tree.txt, elements.json and scan.jpg.
fn write_scan(out_dir: &Path, cap: &Capture) -> Result<(), String> {
    write(&out_dir.join("tree.txt"), cap.tree_text.as_bytes())?;
    write(&out_dir.join("elements.json"), cap.json.as_bytes())?;
    write(&out_dir.join("scan.jpg"), &cap.jpeg)
}

/// One scan's DEBUG files under `dir`, named by the time in seconds as
/// ios/tree/element.py names them: element/ui_elements_<time>.txt (the tree
/// text) and screenshot/ui_elements_screenshot_<time>.jpg (the marked
/// screenshot, byte for byte what the model gets).
fn write_debug(dir: &Path, cap: &Capture) -> Result<(), String> {
    let time = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    write(&dir.join("element").join(format!("ui_elements_{time}.txt")), cap.tree_text.as_bytes())?;
    write(&dir.join("screenshot").join(format!("ui_elements_screenshot_{time}.jpg")), &cap.jpeg)
}

/// The helper connection the agent reads through: one per process. The
/// scanner reads the tree and the screenshots through it, the vault fills a
/// field and re-reads a PIN pad, and the back tool and the video player look
/// through it too. The preview has its own connection (the helper answers
/// each one on its own thread), so it never waits behind a scan.
fn helper() -> &'static Mutex<Option<Tree>> {
    static HELPER: OnceLock<Mutex<Option<Tree>>> = OnceLock::new();
    HELPER.get_or_init(|| Mutex::new(None))
}

/// The helper connection from here on: the one on `port`, the helper's
/// forwarded port (connected at the first request); the preview follows it
/// too.
pub(crate) fn use_helper(port: u16) {
    HELPER_PORT.store(port, Ordering::Relaxed);
    *helper().lock().unwrap() = Some(Tree { port, sock: None });
}

/// The screen's tree as it is now, in uiautomator's XML format (the vault's
/// read of a PIN pad before and after it taps, the back tool's check that
/// the screen changed).
pub fn read_tree() -> Result<String, String> {
    let mut tree = helper().lock().unwrap();
    tree.as_mut().ok_or("no phone helper connected")?.dump()
}

/// Why fill_focused set nothing.
pub enum FillError {
    /// The helper looked and said no: `why` is no_focus, not_password,
    /// not_text or wrong_app, `class` the focused view's class when it named
    /// one. Nothing was set.
    Refused { why: String, class: String },
    /// The request failed, or the field did not take the text: what the field
    /// holds now is unknown.
    Failed,
}

/// Set the text of the field that has input focus, through the helper
/// (Helper.java's fill): `password` when the value is a password (the field
/// must then be a password field), `package` the app the scan read (the
/// field must be in it). The value travels only over the helper's socket:
/// never in a process list, a log or an error.
pub fn fill_focused(password: bool, package: &str, text: &str) -> Result<(), FillError> {
    // Both in base64, so neither can break the request line.
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s.as_bytes());
    let request = format!("fill {} {} {}\n", if password { 1 } else { 0 }, b64(package), b64(text));
    let mut tree = helper().lock().unwrap();
    let tree = tree.as_mut().ok_or(FillError::Failed)?;
    // One try: the text may have been set before a reply went missing.
    let answer = tree.exchange(request.as_bytes()).map_err(|_| FillError::Failed)?;
    let answer = String::from_utf8_lossy(&answer);
    if answer == "OK" {
        return Ok(());
    }
    let mut words = answer.strip_prefix("ERROR:").unwrap_or("").split_whitespace();
    match words.next() {
        Some(why @ ("no_focus" | "not_password" | "not_text" | "wrong_app")) => Err(FillError::Refused {
            why: why.to_string(),
            class: words.next().unwrap_or("").to_string(),
        }),
        _ => Err(FillError::Failed),
    }
}

/// What the helper said to a "shot".
enum ShotAnswer {
    /// The JPEG it compressed on the phone.
    Jpeg(Vec<u8>),
    /// Why it took none: its "ERROR:" line, or an answer that is no JPEG.
    Refused(String),
}

/// The phone helper (Helper.java) on its forwarded port: trees, screenshots
/// and fills. One connection, kept open between requests.
struct Tree {
    port: u16,
    sock: Option<TcpStream>,
}

impl Tree {
    /// The phone's accessibility tree, in uiautomator's XML format. Only the
    /// views that matter to accessibility, as `uiautomator dump --compressed`
    /// gives: layout boxes with no text, which a scan drops anyway, are left out.
    fn dump(&mut self) -> Result<String, String> {
        // A connection the helper has dropped shows up as an I/O error: one
        // fresh try.
        let body = match self.exchange(b"dump\n") {
            Ok(body) => body,
            Err(_) => self.exchange(b"dump\n")?,
        };
        let text = String::from_utf8_lossy(&body).into_owned();
        match text.strip_prefix("ERROR:") {
            Some(why) => Err(format!("helper: {}", why.trim())),
            None => Ok(text),
        }
    }

    /// One screenshot, scaled on the phone so its long side is at most
    /// `max_edge` and its area at most `max_pixels` (0: no cap), as a JPEG of
    /// `quality`. Err only when the connection failed.
    fn shot(&mut self, max_edge: u32, max_pixels: u32, quality: u8) -> Result<ShotAnswer, String> {
        let request = format!("shot {max_edge} {max_pixels} {quality}\n");
        let body = match self.exchange(request.as_bytes()) {
            Ok(body) => body,
            Err(_) => self.exchange(request.as_bytes())?,
        };
        if body.starts_with(&[0xFF, 0xD8]) {
            return Ok(ShotAnswer::Jpeg(body));
        }
        let text = String::from_utf8_lossy(&body);
        Ok(ShotAnswer::Refused(match text.strip_prefix("ERROR:") {
            Some(why) => why.trim().to_string(),
            // A tree: an autocua-helper.jar from before "shot" answered any
            // line with one.
            None => format!("not a JPEG ({} bytes): an autocua-helper.jar from before shot?", body.len()),
        }))
    }

    /// One request line and its whole answer, as bytes. On any I/O error the
    /// connection is dropped, so a late or half-read answer is never taken
    /// for the next one. A whole "ERROR:" answer keeps it: that connection is
    /// still in step.
    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, String> {
        if self.sock.is_none() {
            let sock = TcpStream::connect(("127.0.0.1", self.port)).map_err(|e| format!("helper: {e}"))?;
            sock.set_read_timeout(Some(TREE_WAIT)).ok();
            self.sock = Some(sock);
        }
        let sock = self.sock.as_mut().unwrap();
        let answer = (|| -> io::Result<Vec<u8>> {
            sock.write_all(request)?;
            let mut len = [0u8; 4];
            sock.read_exact(&mut len)?;
            let len = u32::from_be_bytes(len) as usize;
            if len > MAX_TREE_BYTES {
                return Err(io::Error::new(ErrorKind::InvalidData, format!("an answer of {len} bytes")));
            }
            let mut body = vec![0u8; len];
            sock.read_exact(&mut body)?;
            Ok(body)
        })();
        match answer {
            Ok(body) => Ok(body),
            Err(e) => {
                self.sock = None;
                // A timed-out read reads differently on every OS ("Resource
                // temporarily unavailable" on macOS); say what it means.
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
                    return Err(format!("helper: no answer within {} s", TREE_WAIT.as_secs()));
                }
                Err(format!("helper: {e}"))
            }
        }
    }
}

/// The walk over the tree: which nodes to keep, under which name, how deep.
struct Walk<'a> {
    launcher: &'a str,
    /// Pixels per dp.
    dp: f64,
    disp_w: f64,
    disp_h: f64,
    /// Every node's place in the drawing, and the nodes that take taps.
    paths: HashMap<NodeId, DrawPath>,
    taps: Vec<(NodeId, Rect)>,
    out: Vec<Elem>,
}

impl Walk<'_> {
    /// Keeps `n` if it is an element worth showing, then walks its children
    /// one level deeper if it was kept, as ios/tree/element.py does: depth
    /// counts kept ancestors only. `absorb` is the shown name of the nearest
    /// kept element above: text already shown in it is not listed again.
    fn node(&mut self, n: Node, depth: usize, absorb: &str) {
        let mut next_depth = depth;
        let mut next_absorb = absorb.to_string();
        if let (Some(kind), Some(b)) = (self.kind_of(n), bounds(attr(n, "bounds"))) {
            let (x, y, w, h) = self.clip(b);
            let (name, value) = name_and_value(n, kind, absorb);
            let absorbed = kind == "StaticText" && !absorb.is_empty() && absorb.contains(name.as_str());
            // Switched off by the app (greyed out, as YouTube's "Previous
            // video" on a first video): left out, only what works is listed.
            // A missing flag counts as on, as on Android.
            let enabled = attr(n, "enabled") != "false";
            if !absorbed && enabled && self.visible(x, y, w, h) {
                // The part a tap can reach; with none, it is listed as hidden.
                // A slider's ticks still spread over its whole bar.
                let bar = (x, y, w, h);
                let (reach, under) = self.uncovered(n, bar);
                let (x, y, w, h) = reach.unwrap_or(bar);
                if matches!(kind, "Button" | "Icon" | "Switch" | "StaticText") {
                    // Only the part that gets written: text cut off at 47
                    // characters was never shown, so it is listed below.
                    next_absorb = shown(&name);
                }
                let hidden = under.is_some();
                self.out.push(Elem { kind, name, value, x, y, w, h, depth, node: Some(n.id()), under });
                if kind == "Slider" && !hidden {
                    self.ticks(n, bar, depth + 1);
                }
                next_depth = depth + 1;
            }
        }
        for child in n.children().filter(|c| c.has_tag_name("node")) {
            self.node(child, next_depth, &next_absorb);
        }
    }

    /// The iPhone name for what this node is, or None for a node that is only
    /// layout. Android classes map onto the same kinds ios/tree/element.py
    /// keeps, so the model reads one vocabulary on both phones.
    fn kind_of(&self, n: Node) -> Option<&'static str> {
        let class = attr(n, "class");
        let short = class.rsplit('.').next().unwrap_or(class);
        if is_input(class) {
            let search = short.contains("Search") || attr(n, "resource-id").to_lowercase().contains("search");
            return Some(if flag(n, "password") {
                "SecureTextField"
            } else if search {
                "SearchField"
            } else {
                "TextField"
            });
        }
        if flag(n, "checkable") {
            return Some("Switch");
        }
        // A slider the user can move. A star rating that only shows a score
        // is not focusable, and is not a control (iOS keeps no such Slider).
        if (short.contains("SeekBar") || short.contains("RatingBar")) && flag(n, "focusable") {
            return Some("Slider");
        }
        if short == "NumberPicker" {
            return Some("PickerWheel");
        }
        // Custom views report their framework class (a ViewGroup), so the
        // resource id is the better sign.
        let id = attr(n, "resource-id").to_lowercase().replace('_', "");
        if short.contains("PageIndicator") || id.contains("pageindicator") {
            return Some("PageIndicator");
        }
        if flag(n, "scrollable") {
            let list = ["RecyclerView", "ListView", "GridView"].iter().any(|l| short.contains(l));
            return Some(if list { "CollectionView" } else { "ScrollView" });
        }
        // Material's tabs and bottom navigation make the SELECTED item
        // unclickable; it is still a tab, as its clickable siblings show.
        let tab = flag(n, "selected")
            && n.parent().is_some_and(|p| {
                p.children().any(|s| s != n && attr(s, "class") == class && flag(s, "clickable"))
            });
        if flag(n, "clickable") || tab {
            // An app on the home screen, as the iPhone's Icon. Its own package,
            // not the window's: a widget's views carry the widget's package.
            let icon = !self.launcher.is_empty() && attr(n, "package") == self.launcher && short.contains("TextView");
            return Some(if icon { "Icon" } else { "Button" });
        }
        // Text the user can read. A description alone counts too (Compose
        // and web views carry text that way), but only on a leaf: on a box
        // around other views it only names the group, which iOS drops too
        // (its Other), and pictures are left out as iOS keeps no Image.
        let leaf = !n.children().any(|c| c.has_tag_name("node"));
        let desc_only = leaf && !attr(n, "content-desc").trim().is_empty() && !short.contains("Image");
        if !attr(n, "text").trim().is_empty() || desc_only {
            return Some("StaticText");
        }
        None
    }

    /// The box cut to the screen, as uiautomator reports boxes: what shows of
    /// a row half scrolled off, not the whole row.
    fn clip(&self, (x, y, w, h): (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
        let (x0, y0) = (x.max(0.0), y.max(0.0));
        let (x1, y1) = ((x + w).min(self.disp_w), (y + h).min(self.disp_h));
        (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }

    /// ios/tree/element.py's _is_visible: the centre (the point a tap goes
    /// to) is on the screen, and both sides are at least 2 dp.
    fn visible(&self, x: f64, y: f64, w: f64, h: f64) -> bool {
        let min = MIN_ELEMENT_DP * self.dp;
        if w < min || h < min {
            return false;
        }
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        (0.0..=self.disp_w).contains(&cx) && (0.0..=self.disp_h).contains(&cy)
    }

    /// Five virtual tap points along a slider's whole bar, as on iOS. One
    /// under a tappable element drawn over the slider is hidden, like any
    /// element.
    fn ticks(&mut self, slider: Node, (x, y, w, h): Rect, depth: usize) {
        let tw = TICK_W_DP * self.dp;
        let th = h.min(TICK_H_DP * self.dp);
        let step = (w - tw) / (SLIDER_TICKS - 1) as f64;
        for i in 0..SLIDER_TICKS {
            let (tx, ty) = (x + i as f64 * step, y + (h - th) / 2.0);
            let under = self.cover_at(slider, tx + tw / 2.0, ty + th / 2.0);
            self.out.push(Elem {
                kind: "Other",
                name: format!("Slider tick {}", i + 1),
                value: format!("Position {}", i + 1),
                x: tx,
                y: ty,
                w: tw,
                h: th,
                depth,
                node: None,
                under,
            });
        }
    }

    /// The part of a box a tap can reach: what is not under a tappable
    /// element (clickable or long-clickable) drawn over it, which would take
    /// the tap. While the centre is clear (the point a tap goes to, and the
    /// one WDA's `visible` hit-tests on iOS), the whole box; else the biggest
    /// part left, or, with none worth tapping, no box and the element on top
    /// at the centre.
    fn uncovered(&self, n: Node, b: Rect) -> (Option<Rect>, Option<(NodeId, String)>) {
        let Some(top) = self.cover_at(n, b.0 + b.2 / 2.0, b.1 + b.3 / 2.0) else {
            return (Some(b), None);
        };
        let path = &self.paths[&n.id()];
        let mut parts = vec![b];
        for (id, r) in &self.taps {
            if overlap(*r, b) && drawn_over(&self.paths[id], path) {
                parts = parts.into_iter().flat_map(|p| cut(p, *r)).collect();
            }
        }
        let min = MIN_SHOWN_DP * self.dp;
        match parts.into_iter().filter(|p| p.2 >= min && p.3 >= min).max_by(|p, q| (p.2 * p.3).total_cmp(&(q.2 * q.3))) {
            Some(part) => (Some(part), None),
            None => (None, Some(top)),
        }
    }

    /// The tappable element a tap at (x, y) lands on instead of node `n`: the
    /// top one drawn over `n` there, with its name (for when it is not listed
    /// itself). None when the point is clear.
    fn cover_at(&self, n: Node, x: f64, y: f64) -> Option<(NodeId, String)> {
        let path = self.paths.get(&n.id())?;
        let mut top: Option<NodeId> = None;
        for (id, r) in &self.taps {
            if inside(*r, x, y)
                && drawn_over(&self.paths[id], path)
                && top.is_none_or(|was| drawn_over(&self.paths[id], &self.paths[&was]))
            {
                top = Some(*id);
            }
        }
        let top = top?;
        Some((top, n.document().get_node(top).map(|t| name_and_value(t, "Button", "").0).unwrap_or_default()))
    }
}

/// Where a node is drawn: for each level from its window down, its place
/// among its siblings and its drawing order there (Helper.java's
/// drawing-order: counted from 1 for views, from 0 in Compose, and 0 for
/// every node of a web page). The windows are a level too: the app's first,
/// then the others bottom to top, as Helper.java sends them.
type DrawPath = Vec<(usize, i64)>;

fn draw_paths(windows: &[Node]) -> HashMap<NodeId, DrawPath> {
    fn add(n: Node, path: &mut DrawPath, out: &mut HashMap<NodeId, DrawPath>) {
        out.insert(n.id(), path.clone());
        for (i, c) in n.children().filter(|c| c.has_tag_name("node")).enumerate() {
            path.push((i, attr(c, "drawing-order").parse().unwrap_or(0)));
            add(c, path, out);
            path.pop();
        }
    }
    let mut out = HashMap::new();
    for (k, w) in windows.iter().enumerate() {
        add(*w, &mut vec![(k, k as i64 + 1)], &mut out);
    }
    out
}

/// Whether the node at `top` is drawn over the one at `under`: where their
/// paths part, `top`'s side comes later in the drawing. One inside the
/// other, or the same order there (nothing to tell by, as on a web page), is no.
fn drawn_over(top: &[(usize, i64)], under: &[(usize, i64)]) -> bool {
    match top.iter().zip(under).find(|(t, u)| t.0 != u.0) {
        Some((t, u)) => t.1 > u.1,
        None => false,
    }
}

/// The nodes that take a tap (clickable or long-clickable), with their boxes:
/// a tap on one goes to it, not to what it is drawn over.
fn tappable(windows: &[Node]) -> Vec<(NodeId, Rect)> {
    windows
        .iter()
        .flat_map(|w| w.descendants())
        .filter(|n| n.has_tag_name("node") && (flag(*n, "clickable") || flag(*n, "long-clickable")))
        .filter_map(|n| bounds(attr(n, "bounds")).map(|b| (n.id(), b)))
        .collect()
}

fn overlap(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

fn inside(r: Rect, x: f64, y: f64) -> bool {
    r.0 <= x && x < r.0 + r.2 && r.1 <= y && y < r.1 + r.3
}

/// The parts of `a` outside `b`: up to four boxes, above, below, left and
/// right of it.
fn cut(a: Rect, b: Rect) -> Vec<Rect> {
    if !overlap(a, b) {
        return vec![a];
    }
    let (a_right, a_bottom, b_right, b_bottom) = (a.0 + a.2, a.1 + a.3, b.0 + b.2, b.1 + b.3);
    let (top, bottom) = (a.1.max(b.1), a_bottom.min(b_bottom));
    let mut parts = Vec::new();
    if b.1 > a.1 {
        parts.push((a.0, a.1, a.2, b.1 - a.1));
    }
    if b_bottom < a_bottom {
        parts.push((a.0, b_bottom, a.2, a_bottom - b_bottom));
    }
    if b.0 > a.0 {
        parts.push((a.0, top, b.0 - a.0, bottom - top));
    }
    if b_right < a_right {
        parts.push((b_right, top, a_right - b_right, bottom - top));
    }
    parts
}

/// A node's name and value, the way iOS reports the same kind of element.
/// Names fall back as on iOS (label, then identifier, then "no_label"); a
/// control with no text of its own takes the text its children show. `row`
/// is the shown name of the nearest kept element above (a settings row).
fn name_and_value(n: Node, kind: &str, row: &str) -> (String, String) {
    let text = clean(attr(n, "text"));
    let desc = clean(attr(n, "content-desc"));
    let id = clean(attr(n, "resource-id").rsplit('/').next().unwrap_or(""));
    let first = |options: &[&str]| -> String {
        options.iter().find(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| "no_label".into())
    };
    let inner = || if text.is_empty() && desc.is_empty() { inner_text(n) } else { String::new() };
    match kind {
        // What is typed (or the hint, which Android reports while it is empty)
        // is the value, as on iOS; the name is what the field is called.
        "TextField" | "SearchField" | "SecureTextField" => (first(&[&desc, &id]), text),
        // Below Android 11 a switch puts its state word in its text ("Wi-Fi
        // ON", or just "OFF"), and Quick Settings tiles set it to "On"/"Off":
        // a name that flips with the switch. The state is the value; the name
        // is what it switches, as on iOS.
        "Switch" => {
            let label = if is_toggle(n) { strip_state(&text) } else { text.clone() };
            let inner = if label.is_empty() && desc.is_empty() { inner_text(n) } else { String::new() };
            (first(&[&desc, &label, &inner, row, &id]), if flag(n, "checked") { "1" } else { "0" }.into())
        }
        "PickerWheel" => {
            let shown = n
                .descendants()
                .find(|d| is_input(attr(*d, "class")))
                .map(|d| clean(attr(d, "text")))
                .unwrap_or_default();
            (first(&[&desc, &id]), shown)
        }
        // Android's tree has no slider position (it is RangeInfo, which is not
        // dumped), so a slider's text is its label, when an app sets one.
        "Slider" => (first(&[&desc, &text, &id]), String::new()),
        // A selected tab reports value 1, as on iOS.
        "Button" | "Icon" => (first(&[&text, &desc, &inner(), &id]), if flag(n, "selected") { "1" } else { "" }.into()),
        "PageIndicator" => (first(&[&desc, &text, &id]), String::new()),
        "ScrollView" | "CollectionView" => (first(&[&desc, &id]), String::new()),
        _ => (first(&[&text, &desc]), String::new()),
    }
}

/// The text a control shows through its children (a settings row, a tab
/// whose icon carries the description), skipping children that are controls
/// of their own.
fn inner_text(n: Node) -> String {
    fn collect(n: Node, parts: &mut Vec<String>) {
        for c in n.children().filter(|c| c.has_tag_name("node")) {
            if is_control(c) {
                continue;
            }
            let text = clean(attr(c, "text"));
            let shown = if text.is_empty() { clean(attr(c, "content-desc")) } else { text };
            if !shown.is_empty() && !parts.contains(&shown) {
                parts.push(shown);
            }
            collect(c, parts);
        }
    }
    let mut parts = Vec::new();
    collect(n, &mut parts);
    parts.join(", ")
}

fn is_control(n: Node) -> bool {
    flag(n, "clickable") || flag(n, "checkable") || flag(n, "scrollable") || is_input(attr(n, "class"))
}

pub(crate) fn is_input(class: &str) -> bool {
    let short = class.rsplit('.').next().unwrap_or(class);
    short.contains("EditText") || short.contains("AutoCompleteTextView")
}

/// A switch or toggle button: the kinds that write their state into their text.
fn is_toggle(n: Node) -> bool {
    let class = attr(n, "class");
    let short = class.rsplit('.').next().unwrap_or(class);
    short.contains("Switch") || short.contains("ToggleButton")
}

/// A toggle's text without its state word: "Wi-Fi ON" is "Wi-Fi", "OFF" is "".
fn strip_state(text: &str) -> String {
    let lower = text.to_lowercase();
    for word in ["on", "off"] {
        if lower == word {
            return String::new();
        }
        if lower.ends_with(&format!(" {word}")) {
            return text[..text.len() - word.len() - 1].trim_end().to_string();
        }
    }
    text.to_string()
}

fn attr<'a>(n: Node<'a, '_>, key: &str) -> &'a str {
    n.attribute(key).unwrap_or("")
}

fn flag(n: Node, key: &str) -> bool {
    attr(n, key) == "true"
}

/// "[x1,y1][x2,y2]" as (x, y, width, height).
pub(crate) fn bounds(s: &str) -> Option<(f64, f64, f64, f64)> {
    let mut nums = s.split(['[', ']', ',']).filter(|p| !p.is_empty()).map(|p| p.parse::<f64>());
    let (x1, y1, x2, y2) = (nums.next()?.ok()?, nums.next()?.ok()?, nums.next()?.ok()?, nums.next()?.ok()?);
    Some((x1, y1, x2 - x1, y2 - y1))
}

/// One line per value: newlines, tabs and runs of spaces become one space, and
/// double quotes become single ones so the tree lines stay readable.
fn clean(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace('"', "'")
}

/// The name as written out: cut to 47 characters plus "..." past 50, as on iOS.
fn short_name(name: &str) -> String {
    if name.chars().count() > MAX_NAME_CHARS {
        format!("{}...", shown(name))
    } else {
        name.to_string()
    }
}

/// The part of a name that is written out (short_name without its "...").
fn shown(name: &str) -> String {
    if name.chars().count() > MAX_NAME_CHARS {
        name.chars().take(MAX_NAME_CHARS - 3).collect()
    } else {
        name.to_string()
    }
}

// ============================================================ output

/// The tree as ios/tree/element.py writes it: two header lines, then one
/// line per element, four spaces per depth level. A hidden element has no
/// number and names the element over it, by number when that one has one.
fn tree_text(elems: &[Elem], tablet: bool, landscape: bool, app: &str) -> String {
    let mut out = format!(
        "current_device: {} ({})\ncurrent_application: {app}\n",
        if tablet { "Android tablet" } else { "Android phone" },
        if landscape { "landscape" } else { "portrait" }
    );
    // Every listed element by its node, with its number if it has one.
    let mut count = 0;
    let listed: HashMap<NodeId, (Option<usize>, &str)> = elems
        .iter()
        .filter_map(|e| {
            let number = e.numbered().then(|| {
                count += 1;
                count
            });
            e.node.map(|id| (id, (number, e.name.as_str())))
        })
        .collect();
    let mut number = 0;
    for e in elems {
        let indent = "    ".repeat(e.depth);
        let kind = e.kind.to_lowercase();
        let name = short_name(&e.name);
        let value = if e.value.is_empty() { String::new() } else { format!(", value=\"{}\"", e.value) };
        match &e.under {
            None => {
                number += 1;
                out.push_str(&format!("{indent}[{number}]<element_name=\"{name}\", type=\"{kind}\"{value} />\n"));
            }
            Some((id, own)) => {
                let over = match listed.get(id) {
                    Some((Some(n), name)) => format!("[{n}] {}", short_name(name)),
                    Some((None, name)) => short_name(name),
                    None => short_name(own),
                };
                out.push_str(&format!("{indent}<element_name=\"{name}\", type=\"{kind}\"{value}, hidden_under=\"{over}\" />\n"));
            }
        }
    }
    out
}

/// One JSON entry per element, keyed by its number: ios/tree/element.py's
/// elements_mapping (type, name, number, depth, bounds, and value if any).
/// Hidden elements have no number, and no entry.
fn elements_json(elems: &[Elem]) -> String {
    let elems: Vec<&Elem> = elems.iter().filter(|e| e.numbered()).collect();
    let mut out = String::from("{\n");
    for (i, e) in elems.iter().enumerate() {
        let n = i + 1;
        let value = if e.value.is_empty() { String::new() } else { format!(", \"value\": {}", json_str(&e.value)) };
        out.push_str(&format!(
            "  \"{n}\": {{\"type\": \"{}\", \"name\": {}, \"number\": {n}, \"depth\": {}, \"bounds\": {{\"x\": {}, \"y\": {}, \"width\": {}, \"height\": {}}}{value}}}{}\n",
            e.kind,
            json_str(&short_name(&e.name)),
            e.depth,
            e.x as i64,
            e.y as i64,
            e.w as i64,
            e.h as i64,
            if n < elems.len() { "," } else { "" }
        ));
    }
    out.push_str("}\n");
    out
}

/// The same entries as elements_json, as the map the controller reads the
/// element numbers from (ios/controller/service.py's elements_mapping).
fn elements_map(elems: &[Elem]) -> Map<String, Value> {
    let mut out = Map::new();
    for (i, e) in elems.iter().filter(|e| e.numbered()).enumerate() {
        let n = i + 1;
        let mut entry = json!({
            "type": e.kind,
            "name": short_name(&e.name),
            "number": n,
            "depth": e.depth,
            "bounds": {"x": e.x as i64, "y": e.y as i64, "width": e.w as i64, "height": e.h as i64},
        });
        if !e.value.is_empty() {
            entry["value"] = Value::String(e.value.clone());
        }
        out.insert(n.to_string(), entry);
    }
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ============================================================ marks

/// 5x7 bitmap glyphs for the index labels, so no font file is needed:
/// '0'..'9', then '[' and ']'. One row per byte, the low 5 bits.
const GLYPHS: [[u8; 7]; 12] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
    [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
    [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
];

/// Magenta boxes and "[n]" labels, drawn the way ios/tree/element.py draws
/// them: a 3 px outline growing outward, the label centred at the top inside
/// the box and kept on the image. Element boxes are in screen pixels; `sx`
/// and `sy` take them to the screenshot, which the helper may have scaled down.
fn annotate(rgb: &mut [u8], fw: usize, fh: usize, elems: &[Elem], sx: f64, sy: f64) {
    let mut canvas = Canvas { rgb, w: fw as i64, h: fh as i64 };
    for (i, e) in elems.iter().filter(|e| e.numbered()).enumerate() {
        let (x, y) = ((e.x * sx) as i64, (e.y * sy) as i64);
        let (w, h) = ((e.w * sx) as i64, (e.h * sy) as i64);
        if w <= 0 || h <= 0 {
            continue;
        }
        for o in 0..3 {
            canvas.outline(x - o, y - o, x + w + o, y + h + o, MAGENTA);
        }
        let label = format!("[{}]", i + 1);
        let n = label.len() as i64;
        let tw = n * 5 * GLYPH_SCALE + (n - 1) * GLYPH_SCALE;
        let tx = (x + w / 2 - tw / 2).min(canvas.w - tw).max(0);
        canvas.text(tx, y + 5, &label);
    }
}

struct Canvas<'a> {
    rgb: &'a mut [u8],
    w: i64,
    h: i64,
}

impl Canvas<'_> {
    fn fill(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, c: [u8; 3]) {
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                let i = ((y * self.w + x) * 3) as usize;
                self.rgb[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    /// A 1 px rectangle outline, corners included.
    fn outline(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, c: [u8; 3]) {
        self.fill(x0, y0, x1 + 1, y0 + 1, c);
        self.fill(x0, y1, x1 + 1, y1 + 1, c);
        self.fill(x0, y0, x0 + 1, y1 + 1, c);
        self.fill(x1, y0, x1 + 1, y1 + 1, c);
    }

    /// The rim first, under every glyph cell, then the magenta glyphs over it.
    fn text(&mut self, x: i64, y: i64, s: &str) {
        let s_px = GLYPH_SCALE;
        for pass in 0..2 {
            for (ci, ch) in s.chars().enumerate() {
                let glyph = match ch {
                    '0'..='9' => GLYPHS[ch as usize - '0' as usize],
                    '[' => GLYPHS[10],
                    ']' => GLYPHS[11],
                    _ => continue,
                };
                let gx = x + ci as i64 * 6 * s_px;
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (0b10000 >> col) == 0 {
                            continue;
                        }
                        let (cx, cy) = (gx + col * s_px, y + row as i64 * s_px);
                        if pass == 0 {
                            self.fill(cx - LABEL_RIM, cy - LABEL_RIM, cx + s_px + LABEL_RIM, cy + s_px + LABEL_RIM, RIM);
                        } else {
                            self.fill(cx, cy, cx + s_px, cy + s_px, MAGENTA);
                        }
                    }
                }
            }
        }
    }
}

// ============================================================ files

fn encode_jpeg(rgb: &[u8], w: usize, h: usize, quality: u8, sampling: SamplingFactor) -> Result<Vec<u8>, String> {
    let mut jpeg = Vec::with_capacity(rgb.len() / 8);
    let mut encoder = Encoder::new(&mut jpeg, quality);
    encoder.set_sampling_factor(sampling);
    encoder
        .encode(rgb, w as u16, h as u16, ColorType::Rgb)
        .map_err(|e| format!("jpeg: {e}"))?;
    Ok(jpeg)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    }
    fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}
