//! The hand test for AutoCuaBridge: `sh AutoCua/web/browser/AutoCuaBridge/test.command`.
//!
//! Stages the extension (stage_bridge), writes its config.json (the port and the
//! per-launch token of the bridge served below), restarts the owner's own Chrome
//! (its default user data folder, so his Default profile) with the two launch
//! flags and no debugging port, then serves the bridge: a localhost websocket
//! the extension dials with the token, accepted from the extension's own origin
//! only. When the Test button in the extension's popup (test.html) is pressed,
//! the extension scans the page and sends the result here as a `save` request.
//! It is written under debug/iteration_N/ in the working directory, the way
//! element.rs writes its DEBUG output (tree.txt with the url, the summary and the
//! timing as header lines, annotated_screenshot.jpg) plus hits.json, and each
//! save is printed. Ctrl-C stops the bridge; Chrome stays open.
//!
//! A Chrome already running on the profile would take the launch over and ignore
//! the flags, so it is quit first (SIGTERM is Chrome's graceful exit, the session
//! is saved) and the new one is started with --restore-last-session, so the tabs
//! come back. Other extensions are off while Chrome runs with these flags.
//!
//! macOS only for now (the Chrome path and `open`), like the rest of this work.

use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine;
use pyo3::prelude::*;
use serde_json::{json, Value};
use tungstenite::Message;

use crate::bridge;
use crate::stage_bridge;
/// element.rs DEBUG_DIR: relative to the working directory, the repo root when
/// test.command runs.
const DEBUG_DIR: &str = "debug";

/// ~/.AutoCua/bridge-test.pid: the bridge serving now, so that running
/// test.command again replaces it instead of leaving a second one behind.
fn pid_file() -> PathBuf {
    let dest = stage_bridge::default_dest();
    dest.parent().unwrap_or(&dest).join("bridge-test.pid")
}

/// Stop the bridge a previous run left serving (only if that pid still runs this
/// function; the number could have been reused) and record this one.
fn replace_previous() -> io::Result<()> {
    if let Some(pid) = fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&pid| pid != std::process::id())
    {
        let args = Command::new("ps").args(["-o", "args=", "-p", &pid.to_string()]).output();
        if args.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("bridge_test")) {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
    if let Some(parent) = pid_file().parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(pid_file(), std::process::id().to_string())
}

/// One past the highest debug/iteration_N already there, so nothing is overwritten.
fn next_iteration(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            if let Some(k) = name
                .to_string_lossy()
                .strip_prefix("iteration_")
                .and_then(|s| s.parse::<usize>().ok())
            {
                n = n.max(k);
            }
        }
    }
    n + 1
}

fn ms(t: &Value, key: &str) -> i64 {
    t.get(key).and_then(Value::as_f64).unwrap_or(0.0).round() as i64
}

/// One test's files: what element.rs writes with DEBUG on, plus hits.json.
fn save(iteration: usize, req: &Value) -> io::Result<PathBuf> {
    let dir = Path::new(DEBUG_DIR).join(format!("iteration_{iteration}"));
    fs::create_dir_all(&dir)?;
    let s = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or("");
    let t = req.get("timings").cloned().unwrap_or_else(|| json!({}));
    let header = format!(
        "# {}\n# {}\n# {} ms: settle {}, read {}, picture {}, {} frame(s)\n\n",
        s("url"),
        s("summary"),
        ms(&t, "total_ms"),
        ms(&t, "settle_ms"),
        ms(&t, "scan_ms"),
        ms(&t, "shot_ms"),
        t.get("frames").and_then(Value::as_u64).unwrap_or(1)
    );
    fs::write(dir.join("tree.txt"), format!("{header}{}\n", s("tree")))?;
    if let Some(b64) = req.get("screenshot").and_then(Value::as_str) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(io::Error::other)?;
        fs::write(dir.join("annotated_screenshot.jpg"), bytes)?;
    }
    let mut hits = serde_json::Map::new();
    for h in req.get("hits").and_then(Value::as_array).into_iter().flatten() {
        if let (Some(i), Some(r)) = (h.get(0).and_then(Value::as_u64), h.get(1)) {
            hits.insert(i.to_string(), r.clone());
        }
    }
    let dpr = req.get("dpr").cloned().unwrap_or_else(|| json!(1.0));
    let hits_json = serde_json::to_string(&json!({ "dpr": dpr, "hits": hits })).map_err(io::Error::other)?;
    fs::write(dir.join("hits.json"), hits_json)?;
    Ok(dir)
}

/// Ctrl-C arrives as Python's KeyboardInterrupt; polled between reads.
fn interrupted() -> PyResult<bool> {
    Python::attach(|py| match py.check_signals() {
        Ok(()) => Ok(false),
        Err(e) if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py) => Ok(true),
        Err(e) => Err(e),
    })
}

/// Serve the bridge until Ctrl-C: one connection at a time (the test Chrome's
/// extension), each request answered with {id, result} or {id, error}. The
/// handshake is refused without this launch's token in the path or from any
/// origin but the extension's own.
fn serve(listener: TcpListener, token: &str, origin: &str, mut iteration: usize) -> PyResult<()> {
    listener.set_nonblocking(true)?;
    loop {
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if interrupted()? {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        // Refused without this launch's token or from another origin, and a plain
        // connection that is not a websocket is dropped the same way (bridge.rs).
        let Some(mut ws) = bridge::accept_extension(stream, token, origin) else { continue };
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(300)))?;
        println!("AutoCuaBridge connected.");
        loop {
            let text = match ws.read() {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) =>
                {
                    if interrupted()? {
                        return Ok(());
                    }
                    continue;
                }
                Err(_) => break,
            };
            let Ok(msg) = serde_json::from_str::<Value>(&text) else { continue };
            let ty = msg.get("type").and_then(Value::as_str).unwrap_or("");
            if ty.is_empty() || ty == "keepalive" {
                continue;
            }
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let reply = if ty == "save" {
                match save(iteration, &msg) {
                    Ok(dir) => {
                        let t = msg.get("timings").cloned().unwrap_or_else(|| json!({}));
                        println!(
                            "{}  {} ms (settle {}, read {}, picture {})  {}  {}",
                            dir.display(),
                            ms(&t, "total_ms"),
                            ms(&t, "settle_ms"),
                            ms(&t, "scan_ms"),
                            ms(&t, "shot_ms"),
                            msg.get("summary").and_then(Value::as_str).unwrap_or(""),
                            msg.get("url").and_then(Value::as_str).unwrap_or("")
                        );
                        iteration += 1;
                        json!({ "id": id, "result": { "dir": dir.display().to_string(), "iteration": iteration - 1 } })
                    }
                    Err(e) => json!({ "id": id, "error": format!("could not write debug/iteration_{iteration}: {e}") }),
                }
            } else {
                json!({ "id": id, "error": format!("unknown request: {ty}") })
            };
            if ws.send(Message::Text(reply.to_string())).is_err() {
                break;
            }
        }
        println!("AutoCuaBridge disconnected; waiting for it again.");
    }
}

/// `sh AutoCua/web/browser/AutoCuaBridge/test.command` runs this from the repo
/// root: stage the extension, write its config.json, restart Chrome with it and
/// serve the bridge until Ctrl-C. The owner's run passes nothing and gets his own
/// Chrome (Default profile). Tests pass `profile` (a user data folder for a
/// separate Chrome) and `chrome_args` (extra switches, such as a debugging port,
/// which branded Chrome ignores on the default profile).
#[pyfunction]
#[pyo3(signature = (chrome_args=None, profile=None))]
pub fn bridge_test(py: Python<'_>, chrome_args: Option<Vec<String>>, profile: Option<String>) -> PyResult<()> {
    replace_previous()?;
    let src = crate::browser_dir(py)?.join("AutoCuaBridge");
    let dest = stage_bridge::default_dest();
    let staged = stage_bridge::stage(&src, &dest).map_err(|e| {
        crate::ScannerError::new_err(format!("could not stage AutoCuaBridge into {}: {e}", dest.display()))
    })?;
    let id = stage_bridge::extension_id(&staged);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let token = bridge::token()?;
    fs::write(staged.join("config.json"), json!({ "port": port, "token": token }).to_string())?;
    let own = profile.is_none();
    let dir = profile.map(PathBuf::from).unwrap_or_else(bridge::default_user_data_dir);
    let debug_dir = fs::canonicalize(".").unwrap_or_else(|_| PathBuf::from(".")).join(DEBUG_DIR);
    let iteration = next_iteration(Path::new(DEBUG_DIR));
    println!("AutoCuaBridge staged at {} (id {id}); bridge on 127.0.0.1:{port}.", staged.display());
    let extra = chrome_args.unwrap_or_default();
    py.detach(|| {
        let quit = bridge::quit_chrome_on(&dir);
        if quit {
            println!(
                "Quit the Chrome running on {} so the launch flags take; it comes back with its tabs.",
                dir.display()
            );
        }
        bridge::launch_chrome(&staged, if own { None } else { Some(&dir) }, quit, &extra)
    })?;
    println!(
        "Chrome is up on {} with AutoCuaBridge (other extensions are off while it runs this way). \
         Browse to any page, click the AutoCuaBridge icon (puzzle piece; pin it once) and press Test.",
        if own { "your own profile".to_string() } else { format!("the profile {}", dir.display()) }
    );
    println!(
        "Each test goes to {}/iteration_N/ (tree.txt, annotated_screenshot.jpg, hits.json), starting at \
         iteration_{iteration}. Ctrl-C stops this; Chrome stays open.",
        debug_dir.display()
    );
    let origin = format!("chrome-extension://{id}");
    let served = py.detach(|| serve(listener, &token, &origin, iteration));
    let _ = fs::remove_file(pid_file());
    println!("Bridge stopped. Chrome stays open; run test.command again to start over.");
    served
}
