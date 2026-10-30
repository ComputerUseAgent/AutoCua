//! The video player: the Android side of ios/controller/tool/videoplayer.py,
//! for content that is not DRM-blocked (a DRM player is black in the
//! phone's screenshots). Closing a player is `back` here.
//!
//! Everything comes from watching the screen for about 2 s, nine screenshots
//! from the helper: the screen is cut into cells, and a cell changed between
//! two looks when its SSIM fell or its difference hash moved, whichever
//! comes true. A cell that keeps changing is movement, and the biggest patch
//! of it is the video: content is playing when there is one.
//!
//! pause and play: if the screen is already in the wanted state, nothing is
//! tapped. Else the play/pause button is tapped, the way a hand would: at
//! once if it is on the screen, or after a tap on the moving video has
//! brought the player's controls up. Then the screen is watched again, and
//! the button tapped again until the state has turned.

use std::thread::sleep;
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use roxmltree::Node;

use crate::controller::service::controller_service;
use crate::tree::element::{bounds, read_tree, screenshot_rgb};

/// One look at the screen every quarter second, nine of them: 2 s.
const LOOKS: usize = 9;
const LOOK_EVERY: Duration = Duration::from_millis(250);

/// Each look is the screenshot in grey, SHRINK x SHRINK pixels to one, cut
/// into cells of CELL x CELL of those.
const SHRINK: usize = 4;
const CELL: usize = 16;

/// A cell changed between two looks: its SSIM under SSIM_MIN, or its
/// difference hash more than HASH_MAX of its 56 bits apart. A hash bit wants
/// HASH_STEP grey levels between neighbours, so a flat cell's compression
/// noise flips none.
const SSIM_MIN: f64 = 0.97;
const HASH_MAX: u32 = 4;
const HASH_STEP: f64 = 2.0;

/// A cell is moving when it changed in at least half the look pairs: a
/// video changes all the time, a fade or a clock once or twice.
const MOVING_SHARE: f64 = 0.5;

/// The fewest touching moving cells that make a video. A talk show on
/// YouTube's watch page moved 8 to 33 (moto g77); a paused one none.
const MIN_CELLS: usize = 6;

/// After the tap that shows the controls: their fade-in (0.7 s on iOS).
const REVEAL_SETTLE: Duration = Duration::from_millis(700);

/// After a tap on play/pause, before the screen is watched again: the
/// button's own animation.
const TAP_SETTLE: Duration = Duration::from_secs(1);

/// Taps on play/pause, at most, for one play or pause.
const MAX_TAPS: usize = 3;

/// The play/pause button's whole name, lowercase.
const BUTTON_NAMES: [&str; 6] = ["play", "pause", "play video", "pause video", "play/pause", "play pause"];

/// One look: the screenshot in grey, shrunk.
pub(crate) struct Grey {
    px: Vec<u8>,
    w: usize,
    h: usize,
}

impl Grey {
    pub(crate) fn of(rgb: &[u8], frame_w: usize, frame_h: usize) -> Grey {
        let (w, h) = (frame_w / SHRINK, frame_h / SHRINK);
        let mut px = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0u32;
                for dy in 0..SHRINK {
                    let row = (y * SHRINK + dy) * frame_w;
                    for dx in 0..SHRINK {
                        let i = (row + x * SHRINK + dx) * 3;
                        sum += (rgb[i] as u32 * 299 + rgb[i + 1] as u32 * 587 + rgb[i + 2] as u32 * 114) / 1000;
                    }
                }
                px[y * w + x] = (sum / (SHRINK * SHRINK) as u32) as u8;
            }
        }
        Grey { px, w, h }
    }

    fn cells(&self) -> (usize, usize) {
        (self.w / CELL, self.h / CELL)
    }

    fn at(&self, cx: usize, cy: usize, x: usize, y: usize) -> f64 {
        self.px[(cy * CELL + y) * self.w + cx * CELL + x] as f64
    }
}

/// SSIM of one cell between two looks, the cell as one window.
fn ssim(a: &Grey, b: &Grey, cx: usize, cy: usize) -> f64 {
    let n = (CELL * CELL) as f64;
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for y in 0..CELL {
        for x in 0..CELL {
            let (p, q) = (a.at(cx, cy, x, y), b.at(cx, cy, x, y));
            sa += p;
            sb += q;
            saa += p * p;
            sbb += q * q;
            sab += p * q;
        }
    }
    let (ma, mb) = (sa / n, sb / n);
    let (va, vb, cov) = (saa / n - ma * ma, sbb / n - mb * mb, sab / n - ma * mb);
    let (c1, c2) = ((0.01 * 255.0f64).powi(2), (0.03 * 255.0f64).powi(2));
    ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2))
}

/// One cell's difference hash: the cell as 8 x 8 block means, a bit for each
/// block clearly brighter than its right-hand neighbour.
fn dhash(g: &Grey, cx: usize, cy: usize) -> u64 {
    let b = CELL / 8;
    let mut means = [[0.0f64; 8]; 8];
    for (r, row) in means.iter_mut().enumerate() {
        for (c, mean) in row.iter_mut().enumerate() {
            let mut sum = 0.0;
            for y in 0..b {
                for x in 0..b {
                    sum += g.at(cx, cy, c * b + x, r * b + y);
                }
            }
            *mean = sum / (b * b) as f64;
        }
    }
    let mut bits = 0u64;
    for row in &means {
        for c in 0..7 {
            bits = (bits << 1) | (row[c] > row[c + 1] + HASH_STEP) as u64;
        }
    }
    bits
}

/// What 2 s of looks showed: per cell, in how many look pairs it changed.
pub(crate) struct Watch {
    changes: Vec<u32>,
    cols: usize,
    rows: usize,
    pairs: u32,
    frame_w: usize,
    frame_h: usize,
}

impl Watch {
    pub(crate) fn of(looks: &[Grey], frame_w: usize, frame_h: usize) -> Watch {
        let (cols, rows) = looks.first().map_or((0, 0), Grey::cells);
        let mut changes = vec![0u32; cols * rows];
        let mut pairs = 0;
        for pair in looks.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            // A turn of the screen between two looks: nothing to compare.
            if (a.w, a.h) != (b.w, b.h) || a.cells() != (cols, rows) {
                continue;
            }
            pairs += 1;
            for cy in 0..rows {
                for cx in 0..cols {
                    if ssim(a, b, cx, cy) < SSIM_MIN || (dhash(a, cx, cy) ^ dhash(b, cx, cy)).count_ones() > HASH_MAX {
                        changes[cy * cols + cx] += 1;
                    }
                }
            }
        }
        Watch { changes, cols, rows, pairs, frame_w, frame_h }
    }

    /// The biggest patch of touching moving cells, if it is big enough to be
    /// a video: its box in frame pixels, and its cells.
    pub(crate) fn video(&self) -> Option<((f64, f64, f64, f64), usize)> {
        if self.pairs == 0 {
            return None;
        }
        let need = (self.pairs as f64 * MOVING_SHARE).ceil().max(1.0) as u32;
        let moving: Vec<bool> = self.changes.iter().map(|&c| c >= need).collect();
        let mut seen = vec![false; moving.len()];
        let mut best: Option<((usize, usize, usize, usize), usize)> = None;
        for start in 0..moving.len() {
            if !moving[start] || seen[start] {
                continue;
            }
            // One patch: its cells, and its box in cells.
            let (mut count, mut x0, mut y0, mut x1, mut y1) = (0, usize::MAX, usize::MAX, 0, 0);
            let mut todo = vec![start];
            seen[start] = true;
            while let Some(i) = todo.pop() {
                let (x, y) = (i % self.cols, i / self.cols);
                count += 1;
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                let mut near = Vec::with_capacity(4);
                if x > 0 {
                    near.push(i - 1);
                }
                if x + 1 < self.cols {
                    near.push(i + 1);
                }
                if y > 0 {
                    near.push(i - self.cols);
                }
                if y + 1 < self.rows {
                    near.push(i + self.cols);
                }
                for j in near {
                    if moving[j] && !seen[j] {
                        seen[j] = true;
                        todo.push(j);
                    }
                }
            }
            if best.is_none_or(|(_, n)| count > n) {
                best = Some(((x0, y0, x1, y1), count));
            }
        }
        let ((x0, y0, x1, y1), count) = best.filter(|(_, n)| *n >= MIN_CELLS)?;
        let px = (SHRINK * CELL) as f64;
        Some(((x0 as f64 * px, y0 as f64 * px, (x1 - x0 + 1) as f64 * px, (y1 - y0 + 1) as f64 * px), count))
    }

    /// The middle of the video in screen pixels. The screen's size is the
    /// last scan's, turned to the frame's way if the phone turned since.
    fn video_middle(&self) -> Result<Option<(i64, i64)>, String> {
        let Some(((x, y, w, h), _)) = self.video() else {
            return Ok(None);
        };
        let (mut sw, mut sh) = controller_service().screen().ok_or("No scanned screen to map the point onto")?;
        if (self.frame_w > self.frame_h) != (sw > sh) {
            std::mem::swap(&mut sw, &mut sh);
        }
        let (fw, fh) = (self.frame_w as f64, self.frame_h as f64);
        Ok(Some((((x + w / 2.0) / fw * sw) as i64, ((y + h / 2.0) / fh * sh) as i64)))
    }

    pub(crate) fn playing(&self) -> bool {
        self.video().is_some()
    }
}

/// About 2 s of the screen: LOOKS screenshots from the helper, LOOK_EVERY
/// apart. Each look is a screenshot taken on the phone now, not a frame
/// already here, so it waits only for what is left of its turn.
fn watch() -> Result<Watch, String> {
    let start = Instant::now();
    let (rgb, frame_w, frame_h) = screenshot_rgb()?;
    let mut looks = vec![Grey::of(&rgb, frame_w, frame_h)];
    let mut size = (frame_w, frame_h);
    for i in 1..LOOKS {
        sleep((start + LOOK_EVERY * i as u32).saturating_duration_since(Instant::now()));
        let (rgb, w, h) = screenshot_rgb()?;
        size = (w, h);
        looks.push(Grey::of(&rgb, w, h));
    }
    Ok(Watch::of(&looks, size.0, size.1))
}

/// The names a view goes by, lowercase: its label, its text, and its view id
/// without the package ("com.app:id/exo_play_pause" is "exo_play_pause").
fn names(n: Node) -> Vec<String> {
    let id = n.attribute("resource-id").unwrap_or("");
    let id = id.rsplit_once(":id/").map_or(id, |(_, entry)| entry);
    [n.attribute("content-desc").unwrap_or(""), n.attribute("text").unwrap_or(""), id]
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The middle of the play/pause button in the app's window (the helper
/// writes it first), in screen pixels: named Play, Pause, "Play video"...,
/// or a view id that says play_pause (Media3's exo_play_pause, YouTube's).
pub(crate) fn play_pause_button(xml: &str) -> Result<Option<(i64, i64)>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("bad XML from the helper: {e}"))?;
    let Some(app) = doc.root_element().children().find(|n| n.has_tag_name("node")) else {
        return Ok(None);
    };
    for n in app.descendants().filter(|n| n.has_tag_name("node") && n.attribute("enabled") == Some("true")) {
        let names = names(n);
        let button = names.iter().any(|s| {
            BUTTON_NAMES.contains(&s.as_str()) || s.contains("play_pause") || s.contains("playpause")
        });
        let bounds = n.attribute("bounds").and_then(bounds);
        if let (true, Some((x, y, w, h))) = (button, bounds) {
            if w > 0.0 && h > 0.0 {
                return Ok(Some(((x + w / 2.0) as i64, (y + h / 2.0) as i64)));
            }
        }
    }
    Ok(None)
}

/// Turn playback on or off: true once the screen shows the wanted state.
fn set_playback(want_playing: bool) -> Result<bool, String> {
    for _ in 0..MAX_TAPS {
        let seen = watch()?;
        if seen.playing() == want_playing {
            return Ok(true);
        }
        let button = match play_pause_button(&read_tree()?)? {
            Some(point) => point,
            None => {
                // The controls are hidden: a tap on the moving video brings
                // them up. Nothing moving and no button: nowhere to tap.
                let Some((x, y)) = seen.video_middle()? else {
                    eprintln!("❌ No play/pause button on the screen, and nothing moving to tap");
                    return Ok(false);
                };
                controller_service().tap(x, y)?;
                sleep(REVEAL_SETTLE);
                match play_pause_button(&read_tree()?)? {
                    Some(point) => point,
                    None => {
                        eprintln!("❌ No play/pause button after the tap on the video");
                        return Ok(false);
                    }
                }
            }
        };
        controller_service().tap(button.0, button.1)?;
        sleep(TAP_SETTLE);
    }
    Ok(watch()?.playing() == want_playing)
}

fn verdict(what: &str, done: Result<bool, String>) -> bool {
    done.unwrap_or_else(|e| {
        eprintln!("❌ Video {what} error: {e}");
        false
    })
}

/// Is content playing on the screen: true when the video moves.
pub fn streaming(py: Python<'_>) -> bool {
    verdict("streaming check", py.detach(|| watch().map(|w| w.playing())))
}

pub fn play(py: Python<'_>) -> bool {
    verdict("play", py.detach(|| set_playback(true)))
}

pub fn pause(py: Python<'_>) -> bool {
    verdict("pause", py.detach(|| set_playback(false)))
}
