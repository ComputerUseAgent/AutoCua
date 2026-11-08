//! The coder's file tools (read, grep, glob, write, replace) and its shell
//! tool on top of the Sandbox, and the <Tool_response> envelope every routed
//! tool result is rendered into (the coder and subagent loops both use it).
//!
//! The port of mac/controller/cli/service.py, the same on every OS (also on
//! Windows, where the old Python copy differed), except the shell footer of
//! the envelope, which names PowerShell's filters on Windows
//! (windows/agent/coder/view.py). It follows Python 3.13: pathlib's glob
//! patterns and walk order, Path.resolve, str.splitlines, str.strip, lengths
//! and cuts in characters, and Python's error texts. grep runs the `regex`
//! crate on each line; a pattern it cannot compile gives grep's
//! "invalid regex" result with the crate's error text.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use pyo3::prelude::*;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};

use crate::coder::sandbox::service::{has_nul, py_home_dir, py_os_error, py_repr_str, py_resolve, Sandbox};
use crate::coder::{dumps_pretty, py_splitlines, py_strip};

// ---------------------------------------------------------------------------
// The output envelope
// ---------------------------------------------------------------------------

/// One clipped tool output: the text (elision marker included, no footer),
/// the line count of the whole output, the 1-based inclusive line ranges
/// kept, and whether anything was cut.
pub struct ClipResult {
    pub text: String,
    pub total_lines: usize,
    pub ranges: Vec<(usize, usize)>,
    pub truncated: bool,
}

/// Default line budget per tool output.
const CLIP_MAX_LINES: usize = 200;
/// The character safety net (whichever bites first).
const CLIP_MAX_CHARS: usize = 15_000;
/// A single line longer than this is cut.
const CLIP_MAX_LINE_CHARS: usize = 2_000;
/// The shell keeps the last lines too: the verdict is at the end.
const SHELL_TAIL_LINES: usize = 50;

const READ_FOOTER: &str = "... [clipped at ~15k chars — continue with a ranged read starting just after the last [N] shown]";
#[cfg(not(windows))]
const SHELL_FOOTER: &str = "... [long output clipped — re-run with filters (| head, | tail, | grep) or write to a file and grep/read it]";
#[cfg(windows)]
const SHELL_FOOTER: &str = "... [long output clipped — re-run with filters (| Select-Object -First N, | Select-Object -Last N, | Select-String) or write to a file and grep/read it]";
const DEFAULT_FOOTER: &str = "... [showing a bounded slice — narrow with grep, a read start/end range, or a tighter pattern]";

/// Python's clip_output: bound one tool output.
///
/// 1. A line over 2000 characters is cut, with a " [+N chars]" marker.
/// 2. Line budget (when `max_lines` is Some and the output is longer): the
///    first `max_lines - tail_lines` lines plus the last `tail_lines` lines,
///    with an elision marker between them.
/// 3. Character budget: whole lines are dropped from the end of the head
///    until the text fits `max_chars`; the tail always stays.
pub fn clip_output(text: &str, max_lines: Option<usize>, max_chars: usize, tail_lines: usize) -> ClipResult {
    let mut lines: Vec<String> = py_splitlines(text).into_iter().map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }
    let total = lines.len();
    let mut truncated = false;

    for ln in lines.iter_mut() {
        let n = ln.chars().count();
        if n > CLIP_MAX_LINE_CHARS {
            *ln = format!("{} [+{} chars]", char_prefix(ln, CLIP_MAX_LINE_CHARS), n - CLIP_MAX_LINE_CHARS);
            truncated = true;
        }
    }

    // Python's arithmetic, in i64 so no budget can underflow.
    let total_i = i64::try_from(total).unwrap_or(i64::MAX);
    let (tail_n, mut head_seg, tail_seg): (i64, Vec<String>, Vec<String>) = match max_lines {
        Some(max) if total > max => {
            truncated = true;
            let max = i64::try_from(max).unwrap_or(i64::MAX);
            let tail_n = if tail_lines != 0 {
                i64::try_from(tail_lines).unwrap_or(i64::MAX).min(max - 1)
            } else {
                0
            };
            let head_n = max - tail_n;
            let head = py_slice(&lines, None, Some(head_n));
            let tail = if tail_n != 0 { py_slice(&lines, Some(total_i - tail_n), None) } else { Vec::new() };
            (tail_n, head, tail)
        }
        _ => (0, lines, Vec::new()),
    };

    let mut chars: usize = head_seg.iter().chain(tail_seg.iter()).map(|l| l.chars().count() + 1).sum();
    while chars > max_chars {
        let Some(dropped) = head_seg.pop() else { break };
        chars -= dropped.chars().count() + 1;
        truncated = true;
    }
    let head_n = head_seg.len();

    let (body, ranges) = if tail_n != 0 {
        let elided = total_i - head_n as i64 - tail_n;
        let mut body = head_seg;
        body.push(format!("... [{elided} lines elided]"));
        body.extend(tail_seg);
        let tail_start = usize::try_from(total_i - tail_n + 1).unwrap_or(0);
        (body, vec![(1, head_n), (tail_start, total)])
    } else {
        let ranges = if head_n > 0 { vec![(1, head_n)] } else { vec![(0, 0)] };
        (head_seg, ranges)
    };
    ClipResult { text: body.join("\n"), total_lines: total, ranges, truncated }
}

/// The lines="A-B[,C-D] of TOTAL" attribute.
fn ranges_attr(ranges: &[(usize, usize)], total: usize) -> String {
    let spans: Vec<String> = ranges
        .iter()
        .filter(|(a, b)| b >= a && *b > 0)
        .map(|(a, b)| format!("{a}-{b}"))
        .collect();
    let spans = spans.join(",");
    format!("{} of {total}", if spans.is_empty() { "0-0" } else { spans.as_str() })
}

/// One <output> block, clipped by the policy of the result's action.
fn render_block(entry_action: &str, output_text: &str, result_no: Option<usize>) -> String {
    let (max_lines, tail_lines) = match entry_action {
        "read" => (None, 0),
        "shell" => (Some(CLIP_MAX_LINES), SHELL_TAIL_LINES),
        _ => (Some(CLIP_MAX_LINES), 0),
    };
    let clip = clip_output(output_text, max_lines, CLIP_MAX_CHARS, tail_lines);
    let mut body = clip.text;
    if clip.truncated {
        let footer = match entry_action {
            "read" => READ_FOOTER,
            "shell" => SHELL_FOOTER,
            _ => DEFAULT_FOOTER,
        };
        body = if body.is_empty() { footer.to_string() } else { format!("{body}\n{footer}") };
    }
    let lines_attr = ranges_attr(&clip.ranges, clip.total_lines);
    match result_no {
        Some(n) => format!("<output result=\"{n}\" lines=\"{lines_attr}\">\n{body}\n</output result=\"{n}\">"),
        None => format!("<output lines=\"{lines_attr}\">\n{body}\n</output>"),
    }
}

/// Python's render_tool_response: one routed result as the envelope
///
/// ```text
/// <Tool_response>
/// { the result as JSON (indent 2), without its "output" }
/// <output lines="A-B of TOTAL">
/// the output, verbatim and clipped
/// </output>
/// </Tool_response>
/// ```
///
/// A "multiple" result gets a summary head ({status, action, results: N}),
/// then each result's own head followed by its numbered block,
/// <output result="k" ...> ... </output result="k">. Results without an
/// "output" (or with null) render as a head only.
pub fn render_tool_response(action_result: &Value) -> String {
    let Value::Object(map) = action_result else {
        return format!("<Tool_response>\n{}\n</Tool_response>", dumps_pretty(action_result));
    };
    let mut res = map.clone();
    let mut parts: Vec<String> = Vec::new();

    let is_multiple = res.get("action").and_then(Value::as_str) == Some("multiple")
        && matches!(res.get("results"), Some(Value::Array(_)));
    if is_multiple {
        let entries = match res.shift_remove("results") {
            Some(Value::Array(entries)) => entries,
            _ => Vec::new(),
        };
        // The count goes to the end of the head, as Python's pop + set.
        res.insert("results".to_string(), json!(entries.len()));
        parts.push(dumps_pretty(&Value::Object(res)));
        for (i, entry) in entries.into_iter().enumerate() {
            match entry {
                Value::Object(mut entry) => {
                    let out = entry.shift_remove("output");
                    let action = action_of(&entry);
                    parts.push(dumps_pretty(&Value::Object(entry)));
                    if let Some(out) = out.filter(|o| !o.is_null()) {
                        parts.push(render_block(&action, &py_str_value(&out), Some(i + 1)));
                    }
                }
                other => parts.push(dumps_pretty(&other)),
            }
        }
    } else {
        let out = res.shift_remove("output");
        let action = action_of(&res);
        parts.push(dumps_pretty(&Value::Object(res)));
        if let Some(out) = out.filter(|o| !o.is_null()) {
            parts.push(render_block(&action, &py_str_value(&out), None));
        }
    }
    format!("<Tool_response>\n{}\n</Tool_response>", parts.join("\n"))
}

/// The result's "action" as a policy key (anything not a string has none).
fn action_of(m: &Map<String, Value>) -> String {
    m.get("action").and_then(Value::as_str).unwrap_or("").to_string()
}

/// Python's str() of an output value.
fn py_str_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr_value(other),
    }
}

/// Python's repr() of a JSON value (lists and dicts as Python prints them).
fn py_repr_value(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => py_repr_str(s),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr_value).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> =
                map.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_value(v))).collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

// ---------------------------------------------------------------------------
// The tools
// ---------------------------------------------------------------------------

/// Whole-file read cap, in lines.
const READ_DEFAULT_LINE_CAP: usize = 2000;
/// read refuses files larger than this.
const READ_FILE_SIZE_LIMIT: u64 = 5 * 1024 * 1024;
/// grep skips files larger than this.
const GREP_FILE_SIZE_LIMIT: u64 = 8 * 1024 * 1024;
/// Each grep line is cut at this many characters.
const GREP_LINE_TRUNCATE: usize = 200;
/// grep stops counting here; shown as "10000+".
const GREP_TOTALS_CAP: usize = 10_000;
/// A NUL in this many first bytes means binary.
const BINARY_SNIFF_BYTES: usize = 8192;
/// glob gives up after this many walked entries.
const GLOB_WALK_CAP: usize = 20_000;
/// Names grep and glob skip anywhere under the search base (files too).
const SKIP_DIRS: &[&str] = &[
    ".git", ".hg", ".svn",
    "venv", ".venv", "env", ".env",
    "node_modules",
    "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox",
    "dist", "build", "out", "target",
    ".next", ".nuxt", ".turbo", ".cache",
    "site-packages",
    ".idea", ".vscode",
    ".DS_Store",
];

#[cfg(windows)]
const SEP: &str = "\\";
#[cfg(not(windows))]
const SEP: &str = "/";

/// The coder's tools on one sandbox.
pub struct CLIService {
    pub sandbox: Sandbox,
}

impl CLIService {
    /// The sandbox `~/Desktop/sandbox_workspace[/<session_id>]`.
    pub fn new(session_id: Option<&str>) -> io::Result<CLIService> {
        Ok(CLIService { sandbox: Sandbox::new("sandbox_workspace", session_id)? })
    }

    /// Insert `content` at `line` (1-based; lines from there move down) in a
    /// file under the working folder, creating it and its folders.
    pub fn write(&self, path: &str, line: i64, content: &str) -> Value {
        let command_str = format!("write path=\"{path}\", line={line}");
        match self.write_file(path, line, content) {
            Ok((total, last_line)) => json!({
                "status": "success",
                "action": "write",
                "command": command_str,
                "output": format!("Written {total} lines at line {line}. Last empty line: {last_line}"),
                "last_line": last_line,
            }),
            Err(out) => failed("write", &command_str, out),
        }
    }

    fn write_file(&self, path: &str, line: i64, content: &str) -> Result<(usize, usize), String> {
        let full_path = pathlib_join(&self.sandbox.working_dir, path);
        // Python's ValueError texts for a NUL: os.mkdir's, then open()'s.
        if let Some(parent) = full_path.parent() {
            if has_nul(parent) {
                return Err("mkdir: embedded null character in path".to_string());
            }
            py_mkdir_parents(parent)?;
        }
        if has_nul(&full_path) {
            return Err("embedded null byte".to_string());
        }

        let mut existing_lines: Vec<String> = Vec::new();
        if py_exists(&full_path)? {
            let raw = read_strict(&full_path)?;
            if !py_strip(&raw).is_empty() {
                existing_lines = rstrip_nl(&raw).split('\n').map(str::to_string).collect();
            }
        }

        let mut new_lines: Vec<&str> = content.split('\n').collect();
        if new_lines.last() == Some(&"") {
            new_lines.pop();
        }
        // list.insert past the end appends: the new lines go in one block.
        let insert_index = usize::try_from(line.saturating_sub(1).max(0)).unwrap_or(usize::MAX);
        let at = insert_index.min(existing_lines.len());
        existing_lines.splice(at..at, new_lines.iter().map(|s| s.to_string()));

        let final_content = existing_lines.join("\n");
        fs::write(&full_path, final_content.as_bytes()).map_err(|e| py_os_error(&e, &full_path))?;

        if !py_exists(&full_path)? {
            return Err("Write failed - file does not exist after write".to_string());
        }
        let verify = read_strict(&full_path)?;
        let total = if py_strip(&verify).is_empty() { 1 } else { rstrip_nl(&verify).split('\n').count() };
        Ok((total, total + 1))
    }

    /// Run one shell command in the sandbox. Status "success", "error",
    /// "timeout" or "input_required", with cwd, command, exit_code, the
    /// output (stdout then stderr, stripped) and error, as mac's shell().
    /// Err only for KeyboardInterrupt (the command is killed first).
    pub fn shell(&mut self, py: Python<'_>, command: &str, input_text: Option<&str>) -> PyResult<Value> {
        let result = self.sandbox.run(py, command, input_text, false)?;
        let text = |key: &str| result.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        let output = format!("{}{}", text("stdout"), text("stderr"));
        let clean_output = py_strip(&output).to_string();

        if result.get("error").and_then(Value::as_str) == Some("input_required") {
            let last_output = text("last_output");
            let mut response = Map::new();
            response.insert("status".into(), json!("input_required"));
            response.insert("action".into(), json!("shell"));
            response.insert("cwd".into(), json!(self.sandbox.get_cwd()));
            response.insert("command".into(), json!(command));
            response.insert(
                "message".into(),
                json!(format!(
                    "Process waiting for input. Last output: '{last_output}'. Use input parameter with shell command."
                )),
            );
            if !clean_output.is_empty() {
                response.insert("output".into(), json!(clean_output));
            }
            return Ok(Value::Object(response));
        }

        if result.get("timeout").is_some_and(crate::coder::truthy) {
            let message = result.get("message").cloned().unwrap_or_else(|| {
                json!("Command timed out (may need input - use input parameter if program requires user input)")
            });
            let mut response = Map::new();
            response.insert("status".into(), json!("timeout"));
            response.insert("action".into(), json!("shell"));
            response.insert("cwd".into(), json!(self.sandbox.get_cwd()));
            response.insert("command".into(), json!(command));
            response.insert("message".into(), message);
            if !clean_output.is_empty() {
                response.insert("output".into(), json!(clean_output));
            }
            return Ok(Value::Object(response));
        }

        let succeeded = result.get("success").is_some_and(crate::coder::truthy);
        let mut response = Map::new();
        response.insert("status".into(), json!(if succeeded { "success" } else { "error" }));
        response.insert("action".into(), json!("shell"));
        response.insert("cwd".into(), json!(self.sandbox.get_cwd()));
        response.insert("command".into(), json!(command));
        // The exit code always shows, so a real error and an empty result
        // (grep exits 1 on no match) can be told apart.
        if let Some(returncode) = result.get("returncode").filter(|v| !v.is_null()) {
            response.insert("exit_code".into(), returncode.clone());
        }
        let shown = if clean_output.is_empty() { "(no output)".to_string() } else { clean_output };
        response.insert("output".into(), json!(shown));
        if let Some(error) = result.get("error").filter(|v| crate::coder::truthy(v)) {
            response.insert("error".into(), error.clone());
        }
        Ok(Value::Object(response))
    }

    /// Replace the lines of `old_block`, which must be at `line` exactly,
    /// with `new_block` (any number of lines).
    pub fn replace(&self, path: &str, line: i64, old_block: &str, new_block: &str) -> Value {
        let command_str = format!("replace path=\"{path}\", line={line}");
        match self.replace_block(path, line, old_block, new_block) {
            Ok((old_count, new_count, last_line)) => json!({
                "status": "success",
                "action": "replace",
                "command": command_str,
                "output": format!(
                    "replaced {old_count} lines with {new_count} lines at line {line}. Last empty line: {last_line}"
                ),
                "last_line": last_line,
            }),
            Err(out) => failed("replace", &command_str, out),
        }
    }

    fn replace_block(
        &self,
        path: &str,
        line: i64,
        old_block: &str,
        new_block: &str,
    ) -> Result<(usize, usize, usize), String> {
        let full_path = pathlib_join(&self.sandbox.working_dir, path);
        if !py_exists(&full_path)? {
            return Err("File not found".to_string());
        }

        let raw = read_strict(&full_path)?;
        let mut file_lines: Vec<String> = rstrip_nl(&raw).split('\n').map(str::to_string).collect();

        let mut old_lines: Vec<&str> = old_block.split('\n').collect();
        if old_lines.last() == Some(&"") {
            old_lines.pop();
        }
        let old_count = old_lines.len();
        let n_lines = file_lines.len() as i64;

        if line < 1 || line > n_lines {
            return Err(format!("line {line} out of range (file has {n_lines} lines)"));
        }
        let end_line = line - 1 + old_count as i64;
        if end_line > n_lines {
            return Err(format!(
                "old_block has {old_count} lines but only {} lines remain from line {line}",
                n_lines - (line - 1)
            ));
        }

        // 1 <= line <= end_line <= the file's line count, so both fit.
        let start = usize::try_from(line - 1).unwrap_or(0);
        let end = usize::try_from(end_line).unwrap_or(start);
        let actual_lines: &[String] = file_lines.get(start..end).unwrap_or(&[]);
        let same = actual_lines.len() == old_lines.len()
            && actual_lines.iter().zip(old_lines.iter()).all(|(a, e)| a == e);
        if !same {
            for (i, (actual, expected)) in actual_lines.iter().zip(old_lines.iter()).enumerate() {
                if actual != expected {
                    let mismatch_line = line + i as i64;
                    return Err(format!("mismatch at line {mismatch_line}: found \"{actual}\" expected \"{expected}\""));
                }
            }
            return Err(format!(
                "block length mismatch: file has {} lines, old_block has {old_count} lines",
                actual_lines.len()
            ));
        }

        let mut new_lines: Vec<&str> = new_block.split('\n').collect();
        if new_lines.last() == Some(&"") {
            new_lines.pop();
        }
        file_lines.splice(start..end, new_lines.iter().map(|s| s.to_string()));
        let new_content = file_lines.join("\n");
        fs::write(&full_path, new_content.as_bytes()).map_err(|e| py_os_error(&e, &full_path))?;

        // Check that the new block now sits at that line.
        let verify = read_strict(&full_path)?;
        let verify_lines: Vec<&str> = rstrip_nl(&verify).split('\n').collect();
        let new_count = new_lines.len();
        let verify_slice = py_slice(&verify_lines, Some(line - 1), Some(line - 1 + new_count as i64));
        if verify_slice == new_lines {
            return Ok((old_count, new_count, verify_lines.len() + 1));
        }
        Err("replace verification failed after write".to_string())
    }

    /// The file with [N] line numbers: the whole file (`start` = `end` = 0,
    /// at most 2000 lines) or the range start..=end. A missing or empty file
    /// gives "[1] ", the line to write at.
    pub fn read(&self, path: &str, start: i64, end: i64) -> Value {
        let command_str = format!("read path=\"{path}\", start={start}, end={end}");
        match self.read_file(path, start, end) {
            Ok((output, last_line)) => json!({
                "status": "success",
                "action": "read",
                "command": command_str,
                "output": output,
                "last_line": last_line,
            }),
            Err(out) => failed("read", &command_str, out),
        }
    }

    fn read_file(&self, path: &str, start: i64, end: i64) -> Result<(String, i64), String> {
        let full_path = if path.is_empty() {
            PathBuf::from(&self.sandbox.working_dir)
        } else {
            self.resolve_search_base(path)?
        };

        // A missing file reads as one empty line, so the agent can write to a
        // fresh path without a separate touch step.
        if !py_exists(&full_path)? {
            return Ok(("[1] ".to_string(), 1));
        }
        if !py_is_file(&full_path)? {
            return Err(format!("path is not a file: {path}"));
        }
        let size_bytes = match fs::metadata(&full_path) {
            Ok(md) => md.len(),
            Err(e) => return Err(format!("could not stat file: {}", py_os_error(&e, &full_path))),
        };
        if size_bytes > READ_FILE_SIZE_LIMIT {
            let size_mb = size_bytes as f64 / (1024.0 * 1024.0);
            return Err(format!(
                "file too large ({size_mb:.1} MB, limit {} MB) — use grep with head_limit, or specify a narrow start/end range",
                READ_FILE_SIZE_LIMIT / (1024 * 1024)
            ));
        }

        let raw = read_lossy(&full_path).map_err(|e| py_os_error(&e, &full_path))?;
        if py_strip(&raw).is_empty() {
            return Ok(("[1] ".to_string(), 1));
        }

        let file_lines: Vec<&str> = rstrip_nl(&raw).split('\n').collect();
        let file_total = file_lines.len() as i64;
        // The file's append target, also for a slice.
        let last_line = file_total + 1;

        if start < 0 || end < 0 {
            return Err("start and end must be >= 0".to_string());
        }
        if (start == 0) ^ (end == 0) {
            return Err("pass both start and end (or neither, with 0,0 for whole-file)".to_string());
        }

        let mut footer = String::new();
        let indexed_lines: Vec<String> = if start > 0 && end > 0 {
            if start > end {
                return Err(format!("start ({start}) > end ({end})"));
            }
            if start > file_total {
                return Err(format!("start {start} exceeds file length ({file_total})"));
            }
            let clamped_end = end.min(file_total);
            let skip = usize::try_from(start - 1).unwrap_or(0);
            let take = usize::try_from(clamped_end - start + 1).unwrap_or(0);
            let mut lines: Vec<String> = file_lines
                .iter()
                .enumerate()
                .skip(skip)
                .take(take)
                .map(|(i, l)| format!("[{}] {l}", i + 1))
                .collect();
            lines.push(format!("[{last_line}] "));
            if clamped_end < end {
                footer = format!("\n... range end clamped to file length ({file_total}).");
            } else if clamped_end < file_total {
                footer = format!("\n... showing lines {start}-{clamped_end} of {file_total}.");
            }
            lines
        } else if file_lines.len() > READ_DEFAULT_LINE_CAP {
            let cap = READ_DEFAULT_LINE_CAP;
            footer = format!("\n... file has {file_total} lines (showing 1-{cap}). Use start/end to read more.");
            file_lines.iter().take(cap).enumerate().map(|(i, l)| format!("[{}] {l}", i + 1)).collect()
        } else {
            let mut lines: Vec<String> =
                file_lines.iter().enumerate().map(|(i, l)| format!("[{}] {l}", i + 1)).collect();
            lines.push(format!("[{last_line}] "));
            lines
        };

        Ok((indexed_lines.join("\n") + &footer, last_line))
    }

    /// The search base of read, grep and glob: the working folder for "",
    /// else the path with `~` expanded, relative to the working folder,
    /// resolved. No fence: any absolute path is allowed.
    fn resolve_search_base(&self, path: &str) -> Result<PathBuf, String> {
        let working_dir = Path::new(&self.sandbox.working_dir);
        if path.is_empty() {
            return py_resolve(working_dir);
        }
        let candidate = expand_user(path)?;
        let candidate = if candidate.is_absolute() { candidate } else { working_dir.join(candidate) };
        py_resolve(&candidate)
    }

    /// Regex search over the files under `path` (or that one file), one line
    /// at a time. Modes: "content" (`path:line: text`, with `context` lines
    /// around), "files_with_matches" and "count". `head_limit` bounds what is
    /// shown; totals keep counting up to 10000 for the footer.
    #[allow(clippy::too_many_arguments)]
    pub fn grep(
        &self,
        pattern: &str,
        path: &str,
        glob_filter: &str,
        output_mode: &str,
        case_insensitive: bool,
        head_limit: i64,
        context: i64,
    ) -> Value {
        let command_str = format!(
            "grep pattern=\"{pattern}\", path=\"{path}\", glob=\"{glob_filter}\", mode=\"{output_mode}\", ci={}, head={head_limit}, ctx={context}",
            if case_insensitive { "True" } else { "False" }
        );
        let regex = match RegexBuilder::new(pattern).case_insensitive(case_insensitive).build() {
            Ok(regex) => regex,
            Err(e) => return failed("grep", &command_str, format!("invalid regex: {e}")),
        };
        let mode = match output_mode {
            "content" => GrepMode::Content,
            "files_with_matches" => GrepMode::Files,
            "count" => GrepMode::Count,
            _ => {
                return failed(
                    "grep",
                    &command_str,
                    format!("invalid output_mode \"{output_mode}\" (use content / files_with_matches / count)"),
                )
            }
        };
        match self.grep_files(&regex, mode, path, glob_filter, head_limit, context) {
            Ok(output) => json!({"status": "success", "action": "grep", "command": command_str, "output": output}),
            Err(out) => failed("grep", &command_str, out),
        }
    }

    fn grep_files(
        &self,
        regex: &Regex,
        mode: GrepMode,
        path: &str,
        glob_filter: &str,
        head_limit: i64,
        context: i64,
    ) -> Result<String, String> {
        let base = self.resolve_search_base(path)?;
        if !py_exists(&base)? {
            return Err(format!("path not found: {}", if path.is_empty() { "." } else { path }));
        }
        let mut grep = Grep::new(regex, mode, head_limit, context);
        if py_is_file(&base)? {
            let skip_root = base.parent().map(Path::to_path_buf).unwrap_or_else(|| base.clone());
            grep.visit(&base, &skip_root, &base);
        } else {
            // rglob(glob_filter or "*"): the filter is a whole pathlib pattern
            // under "**", not only a file name.
            let filter = if glob_filter.is_empty() { "*" } else { glob_filter };
            let selector = match rglob_selector(filter) {
                Ok(selector) => selector,
                Err(GlobError::Value(m)) | Err(GlobError::Other(m)) => return Err(m),
            };
            select(&selector, &base, false, &mut |fp| grep.visit(&fp, &base, &base))?;
        }
        grep.finish()
    }

    /// Files matching a pathlib pattern ("**/*.py", "src/**/*.tsx") under
    /// `path`, newest first, at most `head_limit` (Python slicing).
    pub fn glob(&self, pattern: &str, path: &str, head_limit: i64) -> Value {
        let command_str = format!("glob pattern=\"{pattern}\", path=\"{path}\", head={head_limit}");
        match self.glob_files(pattern, path, head_limit) {
            Ok(output) => json!({"status": "success", "action": "glob", "command": command_str, "output": output}),
            Err(out) => failed("glob", &command_str, out),
        }
    }

    fn glob_files(&self, pattern: &str, path: &str, head_limit: i64) -> Result<String, String> {
        let base = self.resolve_search_base(path)?;
        if !py_exists(&base)? {
            return Err(format!("path not found: {}", if path.is_empty() { "." } else { path }));
        }

        let selector = match glob_selector(pattern) {
            Ok(selector) => selector,
            Err(GlobError::Value(m)) => return Err(format!("glob error: {m}")),
            Err(GlobError::Other(m)) => return Err(m),
        };
        let mut matches: Vec<PathBuf> = Vec::new();
        let mut walked = 0usize;
        let mut walk_cap_hit = false;
        let mut walk_error: Option<String> = None;
        let walk = select(&selector, &base, false, &mut |p| {
            walked += 1;
            if walked > GLOB_WALK_CAP {
                walk_cap_hit = true;
                return false;
            }
            match py_is_file(&p) {
                Ok(true) => {
                    if !in_skip_dir(&p, &base) {
                        matches.push(p);
                    }
                    true
                }
                Ok(false) => true,
                Err(e) => {
                    walk_error = Some(e);
                    false
                }
            }
        });
        if let Err(e) = walk {
            return Err(format!("glob error: {e}"));
        }
        if let Some(e) = walk_error {
            return Err(format!("glob error: {e}"));
        }

        // Newest first; a stable sort keeps the walk order for equal times.
        let mut keyed: Vec<(PathBuf, f64)> = Vec::with_capacity(matches.len());
        for p in matches {
            let mtime = py_mtime(&p).map_err(|e| py_os_error(&e, &p))?;
            keyed.push((p, mtime));
        }
        keyed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        let total_matched = keyed.len();
        let keep = py_slice_len(total_matched, head_limit);
        let rels: Vec<String> = keyed.iter().take(keep).map(|(p, _)| display_rel(p, &base)).collect();

        if rels.is_empty() {
            return Ok("no matches".to_string());
        }
        let mut output = rels.join("\n");
        if total_matched > rels.len() {
            output.push_str(&format!(
                "\n\n({total_matched} files matched — showing newest {}; raise head_limit or narrow the pattern)",
                rels.len()
            ));
        }
        if walk_cap_hit {
            output.push_str(&format!(
                "\n\n(walk cap of {GLOB_WALK_CAP} entries hit — narrow the pattern or path for complete results)"
            ));
        }
        Ok(output)
    }
}

/// A failed tool result.
fn failed(action: &str, command: &str, output: String) -> Value {
    json!({"status": "failed", "action": action, "command": command, "output": output})
}

#[derive(Clone, Copy, PartialEq)]
enum GrepMode {
    Content,
    Files,
    Count,
}

/// grep's running state over the walked files.
struct Grep<'a> {
    regex: &'a Regex,
    mode: GrepMode,
    head_limit: i64,
    context: i64,
    content_lines: Vec<String>,
    file_match_paths: Vec<String>,
    file_match_counts: Vec<(String, usize)>,
    total_matches: usize,
    matched_files: HashSet<String>,
    budget_hit: bool,
    /// An error that ends the whole grep, as Python's outer except.
    error: Option<String>,
}

impl<'a> Grep<'a> {
    fn new(regex: &'a Regex, mode: GrepMode, head_limit: i64, context: i64) -> Grep<'a> {
        Grep {
            regex,
            mode,
            head_limit,
            context,
            content_lines: Vec::new(),
            file_match_paths: Vec::new(),
            file_match_counts: Vec::new(),
            total_matches: 0,
            matched_files: HashSet::new(),
            budget_hit: false,
            error: None,
        }
    }

    fn under_limit(&self, len: usize) -> bool {
        i64::try_from(len).is_ok_and(|n| n < self.head_limit)
    }

    /// One candidate file. False stops the walk.
    fn visit(&mut self, fp: &Path, skip_root: &Path, base: &Path) -> bool {
        match py_is_file(fp) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(e) => {
                self.error = Some(e);
                return false;
            }
        }
        if in_skip_dir(fp, skip_root) {
            return true;
        }
        match fs::metadata(fp) {
            Ok(md) if md.len() > GREP_FILE_SIZE_LIMIT => return true,
            Ok(_) => {}
            Err(_) => return true,
        }
        let Ok(bytes) = fs::read(fp) else { return true };
        let sniff = bytes.get(..bytes.len().min(BINARY_SNIFF_BYTES)).unwrap_or(&[]);
        if sniff.contains(&0) {
            return true;
        }
        let text = universal_newlines(&String::from_utf8_lossy(&bytes));
        let lines = py_splitlines(&text);
        // The path shown, relative to the search base (made only on a match).
        let mut rel: Option<String> = None;

        match self.mode {
            GrepMode::Count => {
                let n = lines.iter().filter(|ln| self.regex.is_match(ln)).count();
                if n > 0 {
                    let rel = display_rel(fp, base);
                    self.total_matches += n;
                    self.matched_files.insert(rel.clone());
                    if self.under_limit(self.file_match_counts.len()) {
                        self.file_match_counts.push((rel, n));
                    } else {
                        self.budget_hit = true;
                    }
                }
                self.total_matches < GREP_TOTALS_CAP
            }
            GrepMode::Files => {
                if lines.iter().any(|ln| self.regex.is_match(ln)) {
                    let rel = display_rel(fp, base);
                    self.matched_files.insert(rel.clone());
                    if self.under_limit(self.file_match_paths.len()) {
                        self.file_match_paths.push(rel);
                    } else {
                        self.budget_hit = true;
                    }
                }
                self.matched_files.len() < GREP_TOTALS_CAP
            }
            GrepMode::Content => {
                // head_limit bounds emitted lines (context lines and "--"
                // too); a match's block is emitted whole.
                let mut file_emitted_block = false;
                for (idx, ln) in lines.iter().enumerate() {
                    if !self.regex.is_match(ln) {
                        continue;
                    }
                    let rel = rel.get_or_insert_with(|| display_rel(fp, base)).clone();
                    self.total_matches += 1;
                    self.matched_files.insert(rel.clone());

                    if !self.under_limit(self.content_lines.len()) {
                        self.budget_hit = true;
                        if self.total_matches >= GREP_TOTALS_CAP {
                            break;
                        }
                        continue;
                    }

                    if self.context > 0 {
                        if file_emitted_block {
                            self.content_lines.push("--".to_string());
                        }
                        let idx_i = idx as i64;
                        let start = usize::try_from((idx_i - self.context).max(0)).unwrap_or(0);
                        let end = idx_i.saturating_add(self.context).saturating_add(1).min(lines.len() as i64);
                        let end = usize::try_from(end).unwrap_or(0);
                        for (j, other) in lines.iter().enumerate().take(end).skip(start) {
                            let marker = if j == idx { ":" } else { "-" };
                            self.content_lines.push(format!("{rel}:{}{marker} {}", j + 1, clip_line(other)));
                        }
                        file_emitted_block = true;
                    } else {
                        self.content_lines.push(format!("{rel}:{}: {}", idx + 1, clip_line(ln)));
                    }
                }
                self.total_matches < GREP_TOTALS_CAP
            }
        }
    }

    /// The output text, with the totals footer when the budget was exceeded.
    fn finish(self) -> Result<String, String> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let totals_capped = self.total_matches >= GREP_TOTALS_CAP || self.matched_files.len() >= GREP_TOTALS_CAP;
        let fmt = |n: usize| if totals_capped { format!("{GREP_TOTALS_CAP}+") } else { n.to_string() };
        let output = match self.mode {
            GrepMode::Content => {
                let mut output =
                    if self.content_lines.is_empty() { "no matches".to_string() } else { self.content_lines.join("\n") };
                if self.budget_hit && !self.content_lines.is_empty() {
                    output.push_str(&format!(
                        "\n\n({} matches in {} files — showing first {} lines; refine pattern/path/glob or raise head_limit)",
                        fmt(self.total_matches),
                        fmt(self.matched_files.len()),
                        self.content_lines.len()
                    ));
                }
                output
            }
            GrepMode::Files => {
                let mut output = if self.file_match_paths.is_empty() {
                    "no matches".to_string()
                } else {
                    self.file_match_paths.join("\n")
                };
                if self.budget_hit && !self.file_match_paths.is_empty() {
                    output.push_str(&format!(
                        "\n\n({} files with matches — showing first {})",
                        fmt(self.matched_files.len()),
                        self.file_match_paths.len()
                    ));
                }
                output
            }
            GrepMode::Count => {
                let mut output = if self.file_match_counts.is_empty() {
                    "no matches".to_string()
                } else {
                    let rows: Vec<String> = self.file_match_counts.iter().map(|(p, n)| format!("{p}: {n}")).collect();
                    rows.join("\n")
                };
                if self.budget_hit && !self.file_match_counts.is_empty() {
                    output.push_str(&format!(
                        "\n\n({} files with matches, {} total matches — showing first {} files)",
                        fmt(self.matched_files.len()),
                        fmt(self.total_matches),
                        self.file_match_counts.len()
                    ));
                }
                output
            }
        };
        Ok(output)
    }
}

/// grep's per-line cut, with a marker.
fn clip_line(ln: &str) -> String {
    let n = ln.chars().count();
    if n > GREP_LINE_TRUNCATE {
        format!("{} [+{} chars]", char_prefix(ln, GREP_LINE_TRUNCATE), n - GREP_LINE_TRUNCATE)
    } else {
        ln.to_string()
    }
}

// ---------------------------------------------------------------------------
// Python text and path rules
// ---------------------------------------------------------------------------

/// The first `n` characters of `s` (Python's s[:n]).
fn char_prefix(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => s.get(..i).unwrap_or(s),
        None => s,
    }
}

/// s.rstrip('\n').
fn rstrip_nl(s: &str) -> &str {
    s.trim_end_matches('\n')
}

/// Universal newlines: "\r\n" and a lone "\r" read as "\n".
fn universal_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// A Python slice index: negative counts from the end, then clamped.
fn py_index(len: usize, i: i64) -> usize {
    let len_i = i64::try_from(len).unwrap_or(i64::MAX);
    let j = if i < 0 { (len_i + i).max(0) } else { i.min(len_i) };
    usize::try_from(j).unwrap_or(0)
}

/// v[start:end] with Python's rules.
fn py_slice<T: Clone>(v: &[T], start: Option<i64>, end: Option<i64>) -> Vec<T> {
    let s = start.map_or(0, |i| py_index(v.len(), i));
    let e = end.map_or(v.len(), |i| py_index(v.len(), i));
    if s >= e {
        return Vec::new();
    }
    v.get(s..e).map(<[T]>::to_vec).unwrap_or_default()
}

/// len(v[:k]).
fn py_slice_len(len: usize, k: i64) -> usize {
    py_index(len, k)
}

/// Path.read_text(encoding="utf-8"): strict, with Python's error texts.
fn read_strict(p: &Path) -> Result<String, String> {
    match crate::coder::read_text(p) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == io::ErrorKind::InvalidData => match fs::read(p) {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Err(bad) => Err(utf8_error_text(&bytes, &bad)),
                Ok(_) => Err(e.to_string()),
            },
            Err(e2) => Err(py_os_error(&e2, p)),
        },
        Err(e) => Err(py_os_error(&e, p)),
    }
}

/// Path.read_text(encoding="utf-8", errors="replace").
fn read_lossy(p: &Path) -> io::Result<String> {
    let bytes = fs::read(p)?;
    Ok(universal_newlines(&String::from_utf8_lossy(&bytes)))
}

/// str(UnicodeDecodeError) of Python's UTF-8 decoder.
fn utf8_error_text(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let pos = e.valid_up_to();
    let first = bytes.get(pos).copied().unwrap_or(0);
    let (end_incl, reason) = match e.error_len() {
        None => (bytes.len().saturating_sub(1), "unexpected end of data"),
        Some(n) => {
            let reason = if (0x80..=0xC1).contains(&first) || first >= 0xF5 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (pos + n - 1, reason)
        }
    };
    if end_incl <= pos {
        format!("'utf-8' codec can't decode byte 0x{first:02x} in position {pos}: {reason}")
    } else {
        format!("'utf-8' codec can't decode bytes in position {pos}-{end_incl}: {reason}")
    }
}

/// Path(base) / path, with pathlib's normalizing (an absolute path wins).
fn pathlib_join(base: &str, path: &str) -> PathBuf {
    Path::new(base).join(path).components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

/// The stat errors pathlib's exists() and is_file() read as "no".
fn ignored_stat_error(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(c) if c == libc::ENOENT || c == libc::ENOTDIR || c == libc::EBADF || c == libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        e.kind() == io::ErrorKind::NotFound
            || matches!(e.raw_os_error(), Some(2 | 3 | 15 | 21 | 53 | 67 | 123 | 161 | 206 | 267 | 1921))
    }
}

/// Path.exists(): other stat errors (no permission, a name too long) raise.
fn py_exists(p: &Path) -> Result<bool, String> {
    py_stat_test(p, |_| true)
}

/// Path.is_file(), with the same errors as exists().
fn py_is_file(p: &Path) -> Result<bool, String> {
    py_stat_test(p, fs::Metadata::is_file)
}

fn py_stat_test(p: &Path, test: impl Fn(&fs::Metadata) -> bool) -> Result<bool, String> {
    if has_nul(p) {
        return Ok(false);
    }
    match fs::metadata(p) {
        Ok(md) => Ok(test(&md)),
        Err(e) if ignored_stat_error(&e) => Ok(false),
        Err(e) => Err(py_os_error(&e, p)),
    }
}

/// p.stat().st_mtime as Python computes it (seconds + nanoseconds * 1e-9).
fn py_mtime(p: &Path) -> io::Result<f64> {
    let md = fs::metadata(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(md.mtime() as f64 + md.mtime_nsec() as f64 * 1e-9)
    }
    #[cfg(not(unix))]
    {
        let t = md.modified()?;
        Ok(match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as f64 + f64::from(d.subsec_nanos()) * 1e-9,
            Err(before) => -before.duration().as_secs_f64(),
        })
    }
}

/// Path.mkdir(parents=True, exist_ok=True), with the failing level's path in
/// the error text.
fn py_mkdir_parents(p: &Path) -> Result<(), String> {
    match fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match p.parent() {
            Some(parent) if parent != p && !parent.as_os_str().is_empty() => {
                py_mkdir_parents(parent)?;
                match fs::create_dir(p) {
                    Ok(()) => Ok(()),
                    Err(_) if p.is_dir() => Ok(()),
                    Err(e2) => Err(py_os_error(&e2, p)),
                }
            }
            _ => Err(py_os_error(&e, p)),
        },
        Err(_) if p.is_dir() => Ok(()),
        Err(e) => Err(py_os_error(&e, p)),
    }
}

/// Path(path).expanduser(): a first part "~" or "~user" becomes that home.
fn expand_user(path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let comps: Vec<Component> = p.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    let first = match comps.first() {
        Some(Component::Normal(first)) => first.to_string_lossy().into_owned(),
        _ => return Ok(p.to_path_buf()),
    };
    if !first.starts_with('~') {
        return Ok(p.to_path_buf());
    }
    let home = if first == "~" { py_home_dir() } else { user_home(first.get(1..).unwrap_or("")) };
    let Some(mut out) = home else {
        return Err("Could not determine home directory.".to_string());
    };
    for c in comps.iter().skip(1) {
        out.push(c.as_os_str());
    }
    Ok(out)
}

/// ~user on macOS/Linux: the password entry's home folder.
#[cfg(unix)]
fn user_home(name: &str) -> Option<PathBuf> {
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    let cname = CString::new(name).ok()?;
    let mut buf: Vec<libc::c_char> = vec![0; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter for getpwnam_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call; buf's length is passed.
    let rc = unsafe { libc::getpwnam_r(cname.as_ptr(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: on success pw_dir is a NUL-terminated string inside buf.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_bytes().to_vec();
    Some(crate::coder::sandbox::service::py_home_rstrip(OsStr::from_bytes(&dir)))
}

/// ~user on Windows: a sibling of the current user's profile folder.
#[cfg(not(unix))]
fn user_home(name: &str) -> Option<PathBuf> {
    let home = py_home_dir()?;
    let current = std::env::var("USERNAME").ok();
    if current.as_deref() == Some(name) {
        return Some(home);
    }
    let base = home.file_name()?.to_string_lossy().into_owned();
    if current.as_deref() != Some(base.as_str()) {
        return None;
    }
    Some(home.parent()?.join(name))
}

/// The parts of `p` after `base`, compared part by part as pathlib's
/// relative_to (no file access); None when `p` is not under `base`.
fn rel_parts(p: &Path, base: &Path) -> Option<Vec<String>> {
    let pc: Vec<Component> = p.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    let bc: Vec<Component> = base.components().filter(|c| !matches!(c, Component::CurDir)).collect();
    if pc.len() < bc.len() {
        return None;
    }
    let same = |a: &Component, b: &Component| {
        if cfg!(windows) {
            a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
        } else {
            a == b
        }
    };
    if !pc.iter().zip(bc.iter()).all(|(a, b)| same(a, b)) {
        return None;
    }
    Some(pc.iter().skip(bc.len()).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect())
}

/// True if any part between `base` and `p` (the file name too) is a skip name.
fn in_skip_dir(p: &Path, base: &Path) -> bool {
    rel_parts(p, base).is_some_and(|parts| parts.iter().any(|part| SKIP_DIRS.contains(&part.as_str())))
}

/// str(p.resolve().relative_to(base)), or str(p) when it is not under base.
fn display_rel(p: &Path, base: &Path) -> String {
    let resolved = py_resolve(p).ok();
    match resolved.as_deref().and_then(|r| rel_parts(r, base)) {
        Some(parts) if parts.is_empty() => ".".to_string(),
        Some(parts) => parts.join(SEP),
        None => p.to_string_lossy().into_owned(),
    }
}

// ---------------------------------------------------------------------------
// pathlib's glob (Python 3.13, recurse_symlinks=False)
// ---------------------------------------------------------------------------

enum GlobError {
    /// A ValueError ("Unacceptable pattern: ..."); glob() words it "glob error: ...".
    Value(String),
    /// Any other exception, shown as is.
    Other(String),
}

/// One step of the pattern, as pathlib's selectors.
enum Sel {
    /// The end: the path itself, if it exists.
    Exists,
    /// "" (a trailing separator) or "..": appended as text.
    Special { part: String, next: Box<Sel> },
    /// Plain parts, joined: appended as text, checked at the end.
    Literal { part: String, next: Box<Sel> },
    /// A part with * ? or [...]: the matching entries of the folder (only
    /// folders when more parts follow).
    Wildcard { matcher: Option<NameMatcher>, next: Option<Box<Sel>> },
    /// "**": the folder and everything under it, symlinked folders not
    /// entered, in pathlib's order (a stack, last folder first).
    Recursive { dir_only: bool, next: Box<Sel> },
}

fn has_magic(part: &str) -> bool {
    part.contains(['*', '?', '['])
}

/// True for a pattern with a root or a drive (pathlib refuses those).
fn has_anchor(pattern: &str) -> bool {
    if cfg!(windows) {
        let mut chars = pattern.chars();
        let first = chars.next();
        matches!(first, Some('\\') | Some('/')) || chars.next() == Some(':')
    } else {
        pattern.starts_with('/')
    }
}

/// The pattern's parts as pathlib parses them ("" and "." dropped), and
/// whether it ends with a separator.
fn split_pattern(pattern: &str) -> (Vec<String>, bool) {
    let seps: &[char] = if cfg!(windows) { &['\\', '/'] } else { &['/'] };
    let parts = pattern
        .split(seps)
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect();
    (parts, pattern.ends_with(seps))
}

/// Path.glob(pattern)'s checks and selector.
fn glob_selector(pattern: &str) -> Result<Sel, GlobError> {
    if has_anchor(pattern) {
        return Err(GlobError::Other("Non-relative patterns are unsupported".to_string()));
    }
    let (mut parts, trailing) = split_pattern(pattern);
    if parts.is_empty() {
        let class = if cfg!(windows) { "WindowsPath" } else { "PosixPath" };
        return Err(GlobError::Value(format!("Unacceptable pattern: {class}('.')")));
    }
    if trailing {
        parts.push(String::new());
    }
    Ok(build_selector(&parts))
}

/// Path.rglob(pattern), that is glob("**" / pattern).
fn rglob_selector(pattern: &str) -> Result<Sel, GlobError> {
    if has_anchor(pattern) {
        return Err(GlobError::Other("Non-relative patterns are unsupported".to_string()));
    }
    let (rest, trailing) = split_pattern(pattern);
    let mut parts = vec!["**".to_string()];
    parts.extend(rest);
    if trailing {
        parts.push(String::new());
    }
    Ok(build_selector(&parts))
}

fn build_selector(parts: &[String]) -> Sel {
    let Some((part, mut rest)) = parts.split_first() else {
        return Sel::Exists;
    };
    if part == "**" {
        while let Some((first, more)) = rest.split_first() {
            if first != "**" {
                break;
            }
            rest = more;
        }
        return Sel::Recursive { dir_only: !rest.is_empty(), next: Box::new(build_selector(rest)) };
    }
    if part.is_empty() || part == "." || part == ".." {
        return Sel::Special { part: part.clone(), next: Box::new(build_selector(rest)) };
    }
    if !has_magic(part) {
        let mut joined = part.clone();
        while let Some((first, more)) = rest.split_first() {
            if has_magic(first) {
                break;
            }
            joined.push_str(SEP);
            joined.push_str(first);
            rest = more;
        }
        return Sel::Literal { part: joined, next: Box::new(build_selector(rest)) };
    }
    let matcher = if part == "*" { None } else { Some(NameMatcher::new(part, cfg!(windows))) };
    let next = if rest.is_empty() { None } else { Some(Box::new(build_selector(rest))) };
    Sel::Wildcard { matcher, next }
}

/// add_slash: the path with a separator at its end.
fn add_slash(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    let ends_with_sep = match s.as_encoded_bytes().last() {
        None => true,
        Some(b'/') => true,
        Some(b'\\') => cfg!(windows),
        Some(_) => false,
    };
    if !ends_with_sep {
        s.push(SEP);
    }
    PathBuf::from(s)
}

/// One folder entry, as os.scandir gives it.
struct Entry {
    path: PathBuf,
    name: String,
    /// is_dir(follow_symlinks=False).
    is_dir: bool,
    is_symlink: bool,
}

impl Entry {
    /// is_dir() following a symlink (an error reads as "no").
    fn is_dir_follow(&self) -> bool {
        if self.is_symlink {
            fs::metadata(&self.path).map(|md| md.is_dir()).unwrap_or(false)
        } else {
            self.is_dir
        }
    }
}

/// The entries of one folder in the OS's order; any error lists nothing.
fn scandir(dir: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    for item in walkdir::WalkDir::new(dir).min_depth(1).max_depth(1).follow_links(false) {
        let Ok(entry) = item else { return Vec::new() };
        let file_type = entry.file_type();
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(Entry {
            path: entry.into_path(),
            name,
            is_dir: file_type.is_dir(),
            is_symlink: file_type.is_symlink(),
        });
    }
    entries
}

/// Walk one selector from `path`, giving each match to `sink` (false from
/// the sink stops everything). Err is Python's ValueError for a NUL in a
/// path the walk must touch.
fn select(sel: &Sel, path: &Path, exists: bool, sink: &mut dyn FnMut(PathBuf) -> bool) -> Result<bool, String> {
    match sel {
        Sel::Exists => {
            if exists {
                return Ok(sink(path.to_path_buf()));
            }
            if has_nul(path) {
                return Err("lstat: embedded null character in path".to_string());
            }
            if fs::symlink_metadata(path).is_ok() {
                return Ok(sink(path.to_path_buf()));
            }
            Ok(true)
        }
        Sel::Special { part, next } => {
            let mut p = add_slash(path).into_os_string();
            p.push(part);
            select(next, Path::new(&p), exists, sink)
        }
        Sel::Literal { part, next } => {
            let mut p = add_slash(path).into_os_string();
            p.push(part);
            select(next, Path::new(&p), false, sink)
        }
        Sel::Wildcard { matcher, next } => {
            if has_nul(path) {
                return Err("scandir: embedded null character in path".to_string());
            }
            for entry in scandir(path) {
                if let Some(matcher) = matcher {
                    if !matcher.matches(&entry.name) {
                        continue;
                    }
                }
                match next {
                    Some(next) => {
                        if !entry.is_dir_follow() {
                            continue;
                        }
                        if !select(next, &entry.path, true, sink)? {
                            return Ok(false);
                        }
                    }
                    None => {
                        if !sink(entry.path) {
                            return Ok(false);
                        }
                    }
                }
            }
            Ok(true)
        }
        Sel::Recursive { dir_only, next } => {
            let start = add_slash(path);
            if !select(next, &start, exists, sink)? {
                return Ok(false);
            }
            if has_nul(&start) {
                return Err("scandir: embedded null character in path".to_string());
            }
            let mut stack = vec![start];
            while let Some(dir) = stack.pop() {
                for entry in scandir(&dir) {
                    let is_dir = entry.is_dir;
                    if is_dir || !dir_only {
                        let keep_going = if *dir_only {
                            select(next, &entry.path, true, sink)?
                        } else {
                            sink(entry.path.clone())
                        };
                        if !keep_going {
                            return Ok(false);
                        }
                    }
                    if is_dir {
                        stack.push(entry.path);
                    }
                }
            }
            Ok(true)
        }
    }
}

/// One element of a translated name pattern (fnmatch._translate).
enum Tok {
    /// "*": any run of characters.
    Star,
    /// "?": one character.
    Any,
    /// "[!]" with nothing else: any character.
    AnyChar,
    Lit(char),
    /// "[...]": the set, or its complement.
    Set { negated: bool, items: Vec<(char, char)> },
}

/// A wildcard part matched against whole entry names, as pathlib does:
/// hidden names included, case-sensitive except on Windows, no {a,b}.
struct NameMatcher {
    toks: Vec<Tok>,
    /// An empty class: nothing ever matches.
    never: bool,
    ignore_case: bool,
}

impl NameMatcher {
    /// The port of fnmatch._translate(pat).
    fn new(pat: &str, ignore_case: bool) -> NameMatcher {
        let p: Vec<char> = pat.chars().collect();
        let n = p.len();
        let mut toks: Vec<Tok> = Vec::new();
        let mut never = false;
        let mut i = 0;
        while let Some(&c) = p.get(i) {
            i += 1;
            match c {
                '*' => {
                    if !matches!(toks.last(), Some(Tok::Star)) {
                        toks.push(Tok::Star);
                    }
                }
                '?' => toks.push(Tok::Any),
                '[' => {
                    let mut j = i;
                    if p.get(j) == Some(&'!') {
                        j += 1;
                    }
                    if p.get(j) == Some(&']') {
                        j += 1;
                    }
                    while j < n && p.get(j) != Some(&']') {
                        j += 1;
                    }
                    if j >= n {
                        toks.push(Tok::Lit('['));
                    } else {
                        match class_token(&p, i, j) {
                            Some(tok) => toks.push(tok),
                            None => never = true,
                        }
                        i = j + 1;
                    }
                }
                c => toks.push(Tok::Lit(c)),
            }
        }
        NameMatcher { toks, never, ignore_case }
    }

    fn one(&self, tok: &Tok, c: char) -> bool {
        match tok {
            Tok::Star | Tok::Any | Tok::AnyChar => true,
            Tok::Lit(l) => {
                *l == c || (self.ignore_case && (simple_lower(*l) == simple_lower(c) || simple_upper(*l) == simple_upper(c)))
            }
            Tok::Set { negated, items } => {
                let inside = |x: char| items.iter().any(|(lo, hi)| *lo <= x && x <= *hi);
                let hit = inside(c) || (self.ignore_case && (inside(simple_lower(c)) || inside(simple_upper(c))));
                hit != *negated
            }
        }
    }

    /// A full match of the name, with backtracking over the stars.
    fn matches(&self, name: &str) -> bool {
        if self.never {
            return false;
        }
        let s: Vec<char> = name.chars().collect();
        let (mut ti, mut si) = (0usize, 0usize);
        let mut back: Option<(usize, usize)> = None;
        loop {
            match self.toks.get(ti) {
                Some(Tok::Star) => {
                    back = Some((ti + 1, si));
                    ti += 1;
                    continue;
                }
                Some(tok) => {
                    if let Some(&c) = s.get(si) {
                        if self.one(tok, c) {
                            ti += 1;
                            si += 1;
                            continue;
                        }
                    }
                }
                None => {
                    if si == s.len() {
                        return true;
                    }
                }
            }
            // A mismatch: let the last star take one more character.
            match back {
                Some((bt, bs)) if bs < s.len() => {
                    back = Some((bt, bs + 1));
                    ti = bt;
                    si = bs + 1;
                }
                _ => return false,
            }
        }
    }
}

/// The "[...]" class of fnmatch._translate, from p[i0..j] (the text between
/// the brackets). None: an empty class, which never matches.
fn class_token(p: &[char], i0: usize, j: usize) -> Option<Tok> {
    let stuff = p.get(i0..j)?;
    // The regex class body Python builds, as (char, escaped) units: an
    // unescaped '-' between two chunks is a range.
    let mut body: Vec<(char, bool)> = Vec::new();
    if !stuff.contains(&'-') {
        body.extend(stuff.iter().map(|&c| (c, c == '\\')));
    } else {
        let mut chunks: Vec<Vec<char>> = Vec::new();
        let mut i = i0;
        let mut k = if p.get(i0) == Some(&'!') { i0 + 2 } else { i0 + 1 };
        while let Some(found) = (k..j).find(|&x| p.get(x) == Some(&'-')) {
            chunks.push(p.get(i..found)?.to_vec());
            i = found + 1;
            k = found + 3;
        }
        let chunk = p.get(i..j)?.to_vec();
        if !chunk.is_empty() {
            chunks.push(chunk);
        } else {
            chunks.last_mut()?.push('-');
        }
        // Drop empty ranges (a later bound below an earlier one).
        let mut k = chunks.len().saturating_sub(1);
        while k > 0 {
            let prev_last = chunks.get(k - 1).and_then(|c| c.last()).copied();
            let next_first = chunks.get(k).and_then(|c| c.first()).copied();
            if let (Some(a), Some(b)) = (prev_last, next_first) {
                if a > b {
                    let tail: Vec<char> = chunks.get(k).map(|c| c.iter().skip(1).copied().collect()).unwrap_or_default();
                    if let Some(prev) = chunks.get_mut(k - 1) {
                        prev.pop();
                        prev.extend(tail);
                    }
                    if k < chunks.len() {
                        chunks.remove(k);
                    }
                }
            }
            k -= 1;
        }
        for (n, chunk) in chunks.iter().enumerate() {
            if n > 0 {
                body.push(('-', false));
            }
            body.extend(chunk.iter().map(|&c| (c, c == '\\' || c == '-')));
        }
    }
    for unit in body.iter_mut() {
        if matches!(unit.0, '&' | '~' | '|') {
            unit.1 = true;
        }
    }
    match body.first() {
        None => return None,
        Some(&('!', false)) if body.len() == 1 => return Some(Tok::AnyChar),
        _ => {}
    }
    let negated = body.first() == Some(&('!', false));
    let units = if negated { body.get(1..).unwrap_or(&[]) } else { &body[..] };

    // Read the units as Python's regex parser reads a class.
    let mut items: Vec<(char, char)> = Vec::new();
    let mut i = 0;
    while let Some(&(c, _)) = units.get(i) {
        i += 1;
        if units.get(i) == Some(&('-', false)) {
            if let Some(&(hi, _)) = units.get(i + 1) {
                i += 2;
                if c <= hi {
                    items.push((c, hi));
                }
                continue;
            }
            items.push((c, c));
            items.push(('-', '-'));
            i += 1;
            continue;
        }
        items.push((c, c));
    }
    Some(Tok::Set { negated, items })
}

/// c.lower() when that is one character.
fn simple_lower(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// c.upper() when that is one character.
fn simple_upper(c: char) -> char {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}
