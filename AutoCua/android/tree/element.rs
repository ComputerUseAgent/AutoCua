// AutoCua - Android screen reader (scrcpy stream + UI tree, Rust)
//
// READS ONLY, and not its own connection. UIElementScanner at the end of
// this file is the class the agent loop reads the phone with, the copy of
// ios/tree/element.py's UIElementScanner (scan_elements, get_scan_data,
// application_name); tree/test.py drives the same class by hand.
//
// Both halves of a scan come from one helper on the phone, which the
// connector starts and forwards to two local ports
// (android_connector/session.py, android_session, over stream.py):
//   shots   scrcpy's server streams the screen as H.264. A thread here
//           decodes every frame as it arrives and keeps the newest one, so a
//           shot is the frame already in memory.
//   scans   AutoCua's Helper.java, in the same process, answers each request
//           with the accessibility tree in uiautomator's XML format. It keeps
//           its connection open, so there is none of `uiautomator dump`'s
//           one-second wait for a still screen. The scan then does what the
//           iPhone scanner does: keep only the element types that matter,
//           under the iPhone's names (button, statictext, textfield...),
//           number them, and hand the loop the tree text, one JSON entry per
//           element (the controller's map), and the screenshot with numbered
//           magenta boxes. The three also go to debug/android/ (tree.txt,
//           elements.json, scan.jpg) for a look after a run.
//
// The stream (scrcpy 5.x video socket, forward tunnel, no device name):
//   1 byte    dummy, sent once the server really listens
//   4 bytes   codec id, "h264"
//   12-byte headers, each followed by its payload:
//     session packet   first bit 1: width + height (u32 BE each), no payload
//     media packet     first bit 0: flags + PTS (u64 BE), then size (u32 BE)
//                      bit 62 marks a config packet (SPS/PPS)
//
// The tree (Helper.java, its own socket): send a line, get a 4-byte big-endian
// length and that many bytes of UTF-8, the XML or a line starting "ERROR:".
// The vault and the back tool use the same connection (read_tree,
// fill_focused): the helper serves one client at a time, and the scanner
// keeps its one open.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use roxmltree::{Node, NodeId};
use serde_json::{json, Map, Value};

use crate::controller::service::controller_service;

/// Where each scan's files are written, relative to the process CWD: main.py
/// and test.py run from the repo root, so they land in the root debug/
/// folder (gitignored). The last scan only; each one overwrites the three.
const DEFAULT_OUT_DIR: &str = "debug/android";

/// Plain shots keep the video's own quarter-size colour (4:2:0).
const SHOT_QUALITY: u8 = 90;
/// Scans use ios/tree/element.py's LLM_IMAGE_QUALITY and full-size colour
/// (4:4:4), so the magenta digits stay crisp.
const SCAN_QUALITY: u8 = 85;

/// How long the phone's server gets to start listening, and then to send the
/// first frame.
const START_WAIT: Duration = Duration::from_secs(10);

/// Bit 62 of a media packet's header: SPS/PPS rather than a picture.
const CONFIG_FLAG: u64 = 1 << 62;

/// Longest wait for one tree answer: a guard against a wedged helper, not a
/// performance budget.
const TREE_WAIT: Duration = Duration::from_secs(10);

/// No real screen's tree comes near this; a bigger length means the stream
/// is out of step.
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

/// The newest decoded frame as RGB, written by the decode thread.
struct Latest {
    rgb: Vec<u8>,
    width: usize,
    height: usize,
    frames: u64,
    /// Why the stream stopped, once it has.
    ended: Option<String>,
}

struct Shared {
    latest: Mutex<Latest>,
    changed: Condvar,
}

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

/// Opens the screen stream the helper serves on `port`: connects and starts
/// the decode thread (first_frame waits for its first picture).
fn open(port: u16) -> Result<Arc<Shared>, String> {
    let mut sock = connect(port)?;
    let mut codec = [0u8; 4];
    sock.read_exact(&mut codec)
        .map_err(|e| format!("stream: {e}"))?;
    if &codec != b"h264" {
        return Err(format!("expected an h264 stream, got {:?}", String::from_utf8_lossy(&codec)));
    }

    let shared = Arc::new(Shared {
        latest: Mutex::new(Latest { rgb: Vec::new(), width: 0, height: 0, frames: 0, ended: None }),
        changed: Condvar::new(),
    });
    let reader = Arc::clone(&shared);
    thread::spawn(move || {
        let why = match decode_loop(sock, &reader) {
            Ok(()) => "the phone closed the stream".to_string(),
            Err(e) => e,
        };
        reader.latest.lock().unwrap().ended = Some(why);
        reader.changed.notify_all();
    });
    Ok(shared)
}

/// Waits for the stream's first frame (at once when it has one).
fn first_frame(shared: &Shared) -> Result<(), String> {
    let latest = shared.latest.lock().unwrap();
    let (latest, _) = shared
        .changed
        .wait_timeout_while(latest, START_WAIT, |l| l.frames == 0 && l.ended.is_none())
        .unwrap();
    if let Some(why) = &latest.ended {
        return Err(why.clone());
    }
    if latest.frames == 0 {
        return Err(format!("no frame within {} s", START_WAIT.as_secs()));
    }
    Ok(())
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
/// reads the two ports it forwarded. The phone goes to the controller once
/// (set_phone), the elements and the display size after every scan.
#[pyclass(frozen)]
pub struct UIElementScanner {
    /// The app's screen box, handed each scan's plain screenshot (base64)
    /// as on iOS; none on the terminal.
    frontend_callback: Option<Py<PyAny>>,
    shared: Arc<Shared>,
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
        let port: u16 = info.get_item("port")?.extract()?;
        let tree_port: u16 = info.get_item("tree_port")?.extract()?;
        // The stream an earlier scanner opened on this same helper goes on
        // while it runs (the app's next run), also when its first frame never
        // came (a still screen sends none: this run waits again); a new
        // helper's is opened, and kept for the next scanner at once.
        let live = stream()
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(p, s)| *p == port && s.latest.lock().unwrap().ended.is_none())
            .map(|(_, s)| Arc::clone(s));
        let shared = match live {
            Some(shared) => shared,
            None => {
                let shared = py.detach(|| open(port)).map_err(PyRuntimeError::new_err)?;
                *stream().lock().unwrap() = Some((port, Arc::clone(&shared)));
                shared
            }
        };
        // Size, density and home-screen app over adb (one call), then the
        // first frame, which can take a moment: nothing in Python waits for it.
        let device = py.detach(|| device_info(adb, serial));
        py.detach(|| first_frame(&shared)).map_err(PyRuntimeError::new_err)?;
        controller_service().set_phone(&device.adb, &device.serial, device.density);
        use_helper(tree_port);
        Ok(UIElementScanner {
            frontend_callback,
            shared,
            device,
            scan: Mutex::new(ScanState::default()),
        })
    }

    /// Scan the phone's screen: the tree from the helper and the newest
    /// frame, kept here for get_scan_data, the elements handed to the
    /// controller, the files written to debug/android/.
    pub fn scan_elements(&self, py: Python<'_>) -> PyResult<()> {
        self.scan.lock().unwrap().image_base64 = None;
        let want_plain = self.frontend_callback.is_some();
        // Pure Rust from here to the pictures: the GIL is not held meanwhile.
        let cap = py
            .detach(|| {
                let mut tree = helper().lock().unwrap();
                let tree = tree.as_mut().ok_or("no phone helper connected")?;
                capture(&self.shared, &self.device, tree, want_plain)
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
        {
            let mut scan = self.scan.lock().unwrap();
            scan.element_tree_text = cap.tree_text;
            scan.image_base64 = Some(base64::engine::general_purpose::STANDARD.encode(&cap.jpeg));
            scan.application_name = cap.application_name;
        }
        println!("{}", cap.summary);

        // Send to frontend: the plain screenshot, as on iOS.
        if let (Some(callback), Some(plain)) = (&self.frontend_callback, &cap.plain) {
            callback.call1(py, (base64::engine::general_purpose::STANDARD.encode(plain),))?;
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

    /// The stream's frame size, (width, height): what the phone sends after
    /// scrcpy's max_size scaling, in the screen's current orientation.
    #[getter]
    pub fn frame_size(&self) -> (usize, usize) {
        let latest = self.shared.latest.lock().unwrap();
        (latest.width, latest.height)
    }

    /// A plain screenshot of the newest frame to debug/android/shot.jpg
    /// (tree/test.py's "p"). Returns one line saying what was written.
    pub fn shot(&self, py: Python<'_>) -> PyResult<String> {
        py.detach(|| shot(&self.shared, Path::new(DEFAULT_OUT_DIR)))
            .map_err(PyRuntimeError::new_err)
    }
}

// ============================================================ stream

/// The adb forward accepts a connection at once, even before the phone's
/// server listens. Only the dummy byte says the server is really there.
fn connect(port: u16) -> Result<TcpStream, String> {
    let deadline = Instant::now() + START_WAIT;
    loop {
        if let Ok(mut sock) = TcpStream::connect(("127.0.0.1", port)) {
            let mut dummy = [0u8; 1];
            sock.set_read_timeout(Some(START_WAIT)).ok();
            if sock.read_exact(&mut dummy).is_ok() {
                sock.set_read_timeout(None).ok();
                return Ok(sock);
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("no stream on port {port} after {} s", START_WAIT.as_secs()));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Reads packets until the stream ends, decoding every frame into `shared`.
fn decode_loop(mut sock: TcpStream, shared: &Shared) -> Result<(), String> {
    let mut decoder = Decoder::new().map_err(|e| format!("decoder: {e}"))?;
    let mut head = [0u8; 12];
    let mut packet = Vec::new();
    let mut spare = Vec::new();
    loop {
        match sock.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(format!("stream: {e}")),
        }
        if head[0] & 0x80 != 0 {
            continue; // session packet: a new size (rotation); the frames carry it too
        }
        let flags = u64::from_be_bytes(head[..8].try_into().unwrap());
        let len = u32::from_be_bytes(head[8..].try_into().unwrap()) as usize;
        packet.resize(len, 0);
        sock.read_exact(&mut packet)
            .map_err(|e| format!("stream: {e}"))?;

        // SPS/PPS go in on their own and give no picture. The first packet or
        // two may give none either, which is normal for H.264.
        let yuv = match decoder.decode(&packet) {
            Ok(Some(yuv)) if flags & CONFIG_FLAG == 0 => yuv,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("element: decode: {e}");
                continue;
            }
        };
        let (w, h) = yuv.dimensions();
        spare.resize(w * h * 3, 0);
        yuv.write_rgb8(&mut spare);

        let mut latest = shared.latest.lock().unwrap();
        std::mem::swap(&mut latest.rgb, &mut spare);
        latest.width = w;
        latest.height = h;
        latest.frames += 1;
        shared.changed.notify_all();
    }
}

/// The screen stream: one per process, with the forwarded port it came in
/// on. The app builds a scanner per run while the phone's helper stays up,
/// and scrcpy serves one video client, so the next scanner on the same
/// helper reads this one on. The video player watches it for movement.
fn stream() -> &'static Mutex<Option<(u16, Arc<Shared>)>> {
    static STREAM: OnceLock<Mutex<Option<(u16, Arc<Shared>)>>> = OnceLock::new();
    STREAM.get_or_init(|| Mutex::new(None))
}

/// The newest frame of the screen stream: RGB, width, height.
pub fn latest_frame() -> Result<(Vec<u8>, usize, usize), String> {
    let shared = stream().lock().unwrap().as_ref().map(|(_, s)| Arc::clone(s)).ok_or("no screen stream")?;
    frame(&shared)
}

/// A copy of the newest frame (a few ms), so the decode thread never waits.
fn frame(shared: &Shared) -> Result<(Vec<u8>, usize, usize), String> {
    let latest = shared.latest.lock().unwrap();
    if let Some(why) = &latest.ended {
        return Err(format!("stream ended: {why}"));
    }
    Ok((latest.rgb.clone(), latest.width, latest.height))
}

/// Saves the newest frame as shot.jpg and says what was written.
fn shot(shared: &Shared, out_dir: &Path) -> Result<String, String> {
    let t0 = Instant::now();
    let (rgb, w, h) = frame(shared)?;
    // The video only has quarter-size colour (4:2:0); keeping full-size colour
    // would add bytes and time and no detail.
    let jpeg = encode_jpeg(&rgb, w, h, SHOT_QUALITY, SamplingFactor::F_2_2)?;
    let path = out_dir.join("shot.jpg");
    write(&path, &jpeg)?;
    Ok(format!(
        "shot -> {}  {w}x{h}  {} KB  {} ms",
        path.display(),
        jpeg.len() / 1024,
        t0.elapsed().as_millis()
    ))
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
    /// The frame before the marks, for the app's screen box; encoded only
    /// when asked.
    plain: Option<Vec<u8>>,
    /// The app in front: its package, "" on the home screen.
    application_name: String,
    /// The display's size in pixels, in the frame's orientation.
    disp_w: f64,
    disp_h: f64,
    /// One line: the counts and the timings.
    summary: String,
}

/// One scan: the tree from the helper, the newest frame, then the elements,
/// the tree text, the JSON and the marked screenshot.
fn capture(shared: &Shared, dev: &Device, tree: &mut Tree, want_plain: bool) -> Result<Capture, String> {
    let t0 = Instant::now();
    let xml = tree.dump()?;
    let t_dump = Instant::now();
    // Taken right after the tree, so both show the same moment as nearly as
    // they can.
    let (mut rgb, fw, fh) = frame(shared)?;

    let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("bad XML from uiautomator: {e}"))?;
    // One root per window: the app's first, then the system's bars.
    let windows: Vec<Node> = doc.root_element().children().filter(|n| n.has_tag_name("node")).collect();
    let root = *windows.first().ok_or("uiautomator returned an empty tree")?;
    let (rx, ry, rw, rh) = bounds(attr(root, "bounds")).unwrap_or((0.0, 0.0, fw as f64, fh as f64));

    // The display box, turned to the frame's orientation (wm size reports
    // the natural one). Unknown: the root window's box stands in for it.
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

    // The video only has quarter-size colour (4:2:0); keeping full-size
    // colour in the plain shot would add bytes and time and no detail.
    let plain = if want_plain { Some(encode_jpeg(&rgb, fw, fh, SHOT_QUALITY, SamplingFactor::F_2_2)?) } else { None };
    annotate(&mut rgb, fw, fh, &elems, fw as f64 / disp_w, fh as f64 / disp_h);
    let t_draw = Instant::now();
    let jpeg = encode_jpeg(&rgb, fw, fh, SCAN_QUALITY, SamplingFactor::F_1_1)?;
    let t_jpeg = Instant::now();

    let ms = |a: Instant, b: Instant| b.duration_since(a).as_millis();
    let summary = format!(
        "[scan] {} elements{}  dump={} ms tree={} ms draw={} ms jpeg={} ms total={} ms",
        elems.len() - hidden,
        if hidden > 0 { format!(" ({hidden} hidden)") } else { String::new() },
        ms(t0, t_dump),
        ms(t_dump, t_tree),
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

/// The helper connection: one per process, as there is one scanner and one
/// helper on the phone. The scanner reads the tree through it, and the vault
/// fills a field and re-reads a PIN pad through it too, so the helper (which
/// serves one client at a time) never has a second client waiting.
fn helper() -> &'static Mutex<Option<Tree>> {
    static HELPER: OnceLock<Mutex<Option<Tree>>> = OnceLock::new();
    HELPER.get_or_init(|| Mutex::new(None))
}

/// The helper connection from here on: the one on `port`, the helper's
/// forwarded tree port (connected at the first request).
pub(crate) fn use_helper(port: u16) {
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

/// The tree half of the phone helper (Helper.java), on its forwarded port.
/// One connection, kept open between scans.
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
        let text = match self.exchange(b"dump\n") {
            Ok(text) => text,
            Err(_) => self.exchange(b"dump\n")?,
        };
        match text.strip_prefix("ERROR:") {
            Some(why) => Err(format!("helper: {}", why.trim())),
            None => Ok(text),
        }
    }

    /// One request line and its whole answer. On any I/O error the connection
    /// is dropped, so a late or half-read answer is never taken for the next
    /// one. A whole "ERROR:" answer keeps it: that stream is still in step.
    fn exchange(&mut self, request: &[u8]) -> Result<String, String> {
        if self.sock.is_none() {
            let sock = TcpStream::connect(("127.0.0.1", self.port)).map_err(|e| format!("tree: {e}"))?;
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
            Ok(body) => Ok(String::from_utf8_lossy(&body).into_owned()),
            Err(e) => {
                self.sock = None;
                // A timed-out read reads differently on every OS ("Resource
                // temporarily unavailable" on macOS); say what it means.
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
                    return Err(format!("tree: no answer within {} s", TREE_WAIT.as_secs()));
                }
                Err(format!("tree: {e}"))
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
/// and `sy` take them to the frame, which the phone may have scaled down.
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
