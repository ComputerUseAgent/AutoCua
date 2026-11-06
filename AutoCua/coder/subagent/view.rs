//! The subagent's response formatter: the Rust copy of
//! mac/agent/minions/view.py (MinionResponseFormatter), as plain functions.
//!
//! The subagent runs on native tool calling, so extract_json is only the
//! fallback for a model that writes its step as JSON in the text channel
//! (the loop's leaked-step salvage), and format_stream_json is the per-step
//! print. The class's other members were never called and are left out.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::{dumps_pretty, py_strip};

/// Fenced json blocks (three backticks, then `json`). Python's `\s` also
/// matches U+001C..U+001F, which the regex crate's `\s` does not, so they
/// are added to the class.
fn fenced_json_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```json[\s\x{1C}-\x{1F}]*(.*?)[\s\x{1C}-\x{1F}]*```").ok())
        .as_ref()
}

/// How many times `c` occurs in `line` (Python's `line.count(c)`).
fn count_char(line: &str, c: char) -> i64 {
    line.chars().filter(|&x| x == c).count() as i64
}

/// JSON from the usual LLM output shapes, the first that parses wins:
/// 1. the fenced json blocks, the LAST one first (reasoning models write
///    drafts before the answer);
/// 2. the whole response;
/// 3. a brace-matched object, the last start line first.
///
/// Any JSON value can come back; the caller checks for an object. None when
/// nothing parses.
pub fn extract_json(raw_response: &str) -> Option<Value> {
    // Case 1: fenced json blocks, the last one first.
    if let Some(re) = fenced_json_re() {
        let blocks: Vec<&str> = re
            .captures_iter(raw_response)
            .filter_map(|c| c.get(1))
            .map(|m| m.as_str())
            .collect();
        for block in blocks.iter().rev() {
            if let Ok(v) = serde_json::from_str::<Value>(py_strip(block)) {
                return Some(v);
            }
        }
    }

    // Case 2: the whole response is JSON.
    if let Ok(v) = serde_json::from_str::<Value>(py_strip(raw_response)) {
        return Some(v);
    }

    // Case 3: count '{' minus '}' line by line from a line that has a '{';
    // the first LATER line where the count is back to 0 ends the object, and
    // that whole line is part of it. Braces inside strings count too, and a
    // one-line object is never found here.
    let mut lines: Vec<String> = raw_response.split('\n').map(str::to_string).collect();
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains('{'))
        .map(|(i, _)| i)
        .collect();
    for &start in starts.iter().rev() {
        let mut count: i64 = 0;
        let mut end = None;
        for (i, line) in lines.iter().enumerate().skip(start) {
            count += count_char(line, '{') - count_char(line, '}');
            if count == 0 && i > start {
                end = Some(i);
                break;
            }
        }
        let Some(end) = end else {
            continue;
        };
        // The start line is cut at its first '{' IN the list, as Python
        // does, so an earlier start that reaches this line sees it cut.
        if let Some(line) = lines.get_mut(start) {
            if let Some(pos) = line.find('{') {
                *line = line.split_off(pos);
            }
        }
        let Some(span) = lines.get(start..=end) else {
            continue;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&span.join("\n")) {
            return Some(v);
        }
    }
    None
}

/// The per-step print: the JSON again with indent=2, or the input unchanged
/// when it is not JSON. The loop already writes it that way, so this prints
/// it as it is.
pub fn format_stream_json(normalized_response: &str) -> String {
    match serde_json::from_str::<Value>(normalized_response) {
        Ok(v) => dumps_pretty(&v),
        Err(_) => normalized_response.to_string(),
    }
}
