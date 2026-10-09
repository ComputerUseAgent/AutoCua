//! Stages AutoCuaBridge (browser/AutoCuaBridge) for a Chrome launch, so Chrome
//! always runs the extension's current code.
//!
//! Chrome keeps its own copy of an extension's background code inside the
//! profile. For an extension loaded with --disable-extensions-except it never
//! reads the files again on a later launch: not when they change, and not when
//! only the manifest version changes. It installs new background code only when
//! the version AND the background file's name both change. So Chrome loads a
//! copy (~/.AutoCua/AutoCuaBridge) that is rebuilt whenever the extension's
//! files change: the background file is named after a fingerprint of the files,
//! and every rebuild gets a version Chrome has never seen (the UTC time of the
//! rebuild). A version taken from the fingerprint would break a rollback: Chrome
//! remembers only the first version it registered, skips registering when the
//! manifest matches it again, then refuses the stored worker whose file name no
//! longer matches the manifest. The extension id stays the same (Chrome derives
//! it from the staged folder's path). Tested on Chrome 154.
//!
//! The glow overlay (browser/glow: glow.js and glow.css) goes along as two
//! files the extension runs as a content script (tools.js glow): glow.js as it
//! is, and glow.css.js, which defines AutoCua_CSS the way browser.rs's
//! glow_source does. Both count in the fingerprint, so a change to the overlay
//! restages the extension too.
//!
//! Whoever launches Chrome writes config.json (port, token) into the staged
//! folder after staging (bridge.rs), so it never lands in the package.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Timelike, Utc};
use pyo3::prelude::*;
use ring::digest::{digest, Context, SHA256};

/// Beside the agent's own profiles (~/.AutoCua/chrome-<port>). Same home rule
/// as browser.rs: Windows sets USERPROFILE rather than HOME.
pub fn default_dest() -> PathBuf {
    // Tests stage a copy of their own: every Chrome that loads the shared copy dials
    // every bridge listed in its config, the person's Chrome included.
    if let Some(dir) = std::env::var_os("AutoCua_BRIDGE_DEST").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()),
    };
    home.join(".AutoCua").join("AutoCuaBridge")
}

/// The extension's files as sorted relative paths with '/' separators.
/// config.json (written per launch) and dotfiles are not part of its code.
fn files(src: &Path) -> io::Result<Vec<String>> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let rel = format!("{prefix}{name}");
            if entry.file_type()?.is_dir() {
                walk(&entry.path(), &format!("{rel}/"), out)?;
            } else if rel != "config.json" {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(src, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// The overlay's sources beside the extension (browser/glow), when they are
/// there: a test staging its own copy of the extension has none, and gets no glow.
fn glow_files(src: &Path) -> Option<(PathBuf, PathBuf)> {
    let dir = src.parent()?.join("glow");
    let (js, css) = (dir.join("glow.js"), dir.join("glow.css"));
    (js.is_file() && css.is_file()).then_some((js, css))
}

/// The cover page a parallel run shows in the browser's front tab (tools.js cover), its
/// script and the video it plays: AutoCua/logo beside the web crate, when they are there.
fn cover_files(src: &Path) -> Option<Vec<(&'static str, PathBuf)>> {
    let logo = src.parent()?.parent()?.parent()?.join("logo");
    let files = vec![
        ("agents.html", logo.join("agents.html")),
        ("agents.js", logo.join("agents.js")),
        ("simulation_background.mp4", logo.join("simulation_background.mp4")),
    ];
    files.iter().all(|(_, p)| p.is_file()).then_some(files)
}

/// SHA-256 over every file's relative path and bytes, as hex; the overlay's
/// two files and the cover page's two included when present.
pub fn fingerprint(src: &Path) -> io::Result<String> {
    let mut ctx = Context::new(&SHA256);
    for rel in files(src)? {
        ctx.update(rel.as_bytes());
        ctx.update(b"\0");
        ctx.update(&fs::read(src.join(&rel))?);
        ctx.update(b"\0");
    }
    let mut extra = Vec::new();
    if let Some((js, css)) = glow_files(src) {
        extra.push(("glow.js", js));
        extra.push(("glow.css", css));
    }
    if let Some(cover) = cover_files(src) {
        extra.extend(cover);
    }
    for (name, path) in extra {
        ctx.update(name.as_bytes());
        ctx.update(b"\0");
        ctx.update(&fs::read(path)?);
        ctx.update(b"\0");
    }
    Ok(hex(ctx.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Make `dest` hold the current extension from `src` and return it. Rebuilt
/// only when the files changed; the copy is built aside and swapped in.
pub fn stage(src: &Path, dest: &Path) -> io::Result<PathBuf> {
    let fp = fingerprint(src)?;
    if fs::read_to_string(dest.join(".fingerprint")).is_ok_and(|s| s == fp) {
        return Ok(dest.to_path_buf());
    }
    let parent = dest.parent().ok_or_else(|| io::Error::other("staging folder has no parent"))?;
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    fs::create_dir_all(parent)?;
    let pid = std::process::id();
    let tmp = parent.join(format!(".{name}.{pid}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    for rel in files(src)? {
        let to = tmp.join(&rel);
        fs::create_dir_all(to.parent().unwrap_or(&tmp))?;
        fs::copy(src.join(&rel), to)?;
    }
    if let Some((js, css)) = glow_files(src) {
        fs::copy(js, tmp.join("glow.js"))?;
        let quoted = serde_json::to_string(&fs::read_to_string(css)?).map_err(io::Error::other)?;
        fs::write(tmp.join("glow.css.js"), format!("var AutoCua_CSS = {quoted};\n"))?;
    }
    if let Some(cover) = cover_files(src) {
        for (name, path) in cover {
            fs::copy(path, tmp.join(name))?;
        }
    }

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(src.join("manifest.json"))?).map_err(io::Error::other)?;
    let worker = manifest["background"]["service_worker"]
        .as_str()
        .ok_or_else(|| io::Error::other("manifest.json has no background.service_worker"))?
        .to_string();
    let stem = Path::new(&worker).file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let stamped = match worker.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{stem}.{}.js", &fp[..12]),
        None => format!("{stem}.{}.js", &fp[..12]),
    };
    fs::rename(tmp.join(&worker), tmp.join(&stamped))?;
    let now = Utc::now();
    manifest["version_name"] = manifest["version"].clone();
    manifest["version"] = format!(
        "{}.{}.{}.{}",
        now.year(),
        now.month() * 100 + now.day(),
        now.hour() * 100 + now.minute(),
        now.second() * 1000 + now.timestamp_subsec_millis()
    )
    .into();
    manifest["background"]["service_worker"] = stamped.into();
    let text = serde_json::to_string_pretty(&manifest).map_err(io::Error::other)?;
    fs::write(tmp.join("manifest.json"), text + "\n")?;
    fs::write(tmp.join(".fingerprint"), &fp)?;

    let old = parent.join(format!(".{name}.{pid}.old"));
    if dest.exists() {
        fs::rename(dest, &old)?;
    }
    fs::rename(&tmp, dest)?;
    let _ = fs::remove_dir_all(&old);
    Ok(dest.to_path_buf())
}

/// Chrome's id for an unpacked extension: the first 32 hex digits of
/// sha256(absolute path), written with the letters a-p.
pub fn extension_id(path: &Path) -> String {
    let d = digest(&SHA256, &id_path_bytes(path));
    d.as_ref()[..16]
        .iter()
        .flat_map(|b| [b >> 4, b & 0x0f])
        .map(|n| (b'a' + n) as char)
        .collect()
}

/// The path bytes Chrome hashes for the id. macOS and Linux: the real path
/// (Chrome's MakeAbsoluteFilePath resolves links) as UTF-8.
#[cfg(not(windows))]
fn id_path_bytes(path: &Path) -> Vec<u8> {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    abs.to_string_lossy().as_bytes().to_vec()
}

/// Windows: the path made absolute the way Chrome makes it (_wfullpath: no
/// link resolution and no \\?\ prefix, which fs::canonicalize would add), its
/// drive letter upper-cased, hashed as UTF-16LE (crx_file::id_util). Checked
/// on Chrome 154 against the id Chrome recorded for the staged copy.
#[cfg(windows)]
fn id_path_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut wide: Vec<u16> = abs.as_os_str().encode_wide().collect();
    if let [drive, colon, ..] = wide.as_mut_slice() {
        if *colon == u16::from(b':') && (u16::from(b'a')..=u16::from(b'z')).contains(drive) {
            *drive -= 32;
        }
    }
    wide.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Stage AutoCuaBridge into `dest` (default ~/.AutoCua/AutoCuaBridge) and
/// return (staged folder, extension id). `src` defaults to the package copy
/// beside browser.rs; tests pass their own.
#[pyfunction]
#[pyo3(signature = (dest=None, src=None))]
pub fn stage_bridge(py: Python<'_>, dest: Option<String>, src: Option<String>) -> PyResult<(String, String)> {
    let src = match src {
        Some(s) => PathBuf::from(s),
        None => crate::browser_dir(py)?.join("AutoCuaBridge"),
    };
    let dest = dest.map(PathBuf::from).unwrap_or_else(default_dest);
    let staged = stage(&src, &dest).map_err(|e| {
        crate::ScannerError::new_err(format!("could not stage AutoCuaBridge into {}: {e}", dest.display()))
    })?;
    Ok((staged.display().to_string(), extension_id(&staged)))
}
