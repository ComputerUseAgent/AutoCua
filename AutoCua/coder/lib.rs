//! Crate root: the module behind `AutoCua.coder`.
//!
//! The coder agent is ONE crate, the same shape as ../android: this file,
//! Cargo.toml, Cargo.lock and build.rs sit here, and the sources sit in their
//! own folders beside them, one service.rs each and no mod.rs: the folders
//! are declared below. coder/__init__.py builds the crate lazily (plain
//! `cargo build --release`, through AutoCua/utils/rust.py), copies the dylib
//! beside itself as agent_native.so, and re-exports the classes from it.
//!
//! It is the Rust copy of the macOS coder (AutoCua/mac/agent/coder with its
//! tools, its controller and its sandbox), one copy for macOS, Windows and
//! Linux. The mac coder's minions are the subagents here.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pyo3::prelude::*;
use serde::Serialize;
use serde_json::Value;

/// The coder's main agent: its loop (service.rs), the transcript codec and
/// the terminal display (view.rs), beside its prompt. main/planner/ holds
/// planner.md, the planning mode's skill, read at run time (no code).
pub mod main {
    pub mod service;
    pub mod view;
}
/// The coder's tools: the name, settings and description of each one the
/// model is given, and the LLMManager that hands them to the model.
pub mod tool_registry {
    pub mod service;
}
/// The coder's controller: it routes each tool call to the code that runs it
/// (service.rs), with the file tools (cli/), the todo, the scratchpad and one
/// folder per other tool, as in ../android/controller.
pub mod controller {
    pub mod service;
    pub mod cli {
        pub mod service;
    }
    pub mod scratchpad {
        pub mod service;
    }
    pub mod task_tracker {
        pub mod service;
    }
    pub mod tool {
        /// The `plan` tool: the plan document.
        pub mod plan {
            pub mod service;
        }
        /// The web search: service.rs routes it, one file per provider's
        /// search and the browser-agent fallback beside it.
        pub mod web {
            pub mod service;
            pub mod anthropic;
            pub mod chatgpt;
            pub mod google_search;
            pub mod groq_search;
            pub mod openrouter;
            pub mod perplexity_search;
            pub mod web_agent;
        }
    }
}
/// Runs the coder's shell commands: zsh, PowerShell or bash.
pub mod sandbox {
    pub mod service;
}
/// The subagent (the mac coder's minion): a read-only investigator the coder
/// starts as a child process, the coder's kind of loop with its own tools
/// and its prompt, which is system_prompt.md with the block of the mode it
/// runs in filled in at run time: workflow/workflow.md or critic/critic.md,
/// each a prompt file with no code beside it.
pub mod subagent {
    pub mod service;
}

// ---------------------------------------------------------------------------
// Where things are
// ---------------------------------------------------------------------------

/// AutoCua/coder, the directory holding this extension module, resolved
/// lazily because importlib only sets `__file__` after module init returns.
pub fn coder_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    if let Some(dir) = DIR.get() {
        return Ok(dir);
    }
    let dir = if let Ok(v) = std::env::var("AutoCua_CODER_DIR") {
        PathBuf::from(v)
    } else {
        let module = py
            .import("AutoCua.coder.agent_native")
            .or_else(|_| py.import("agent_native"))?;
        let file: String = module.getattr("__file__")?.extract()?;
        PathBuf::from(file)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    };
    Ok(DIR.get_or_init(|| dir))
}

/// AutoCua/coder/main: the coder's system_prompt.md lives in it.
pub fn main_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(coder_dir(py)?.join("main"))
}

/// AutoCua/coder/scratchpad: the todo, the plan, the scratchpad notes and the
/// subagents' results, one folder per session (git-ignored).
pub fn scratch_root(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(coder_dir(py)?.join("scratchpad"))
}

/// AutoCua_data, asked of Python: AutoCua/__init__.py alone handles the
/// compiled-vs-dev base directory and the AutoCua_DATA_DIR override.
pub fn data_root(py: Python<'_>) -> PyResult<PathBuf> {
    let module = py.import("AutoCua")?;
    let dir = module.call_method0("data_root")?;
    Ok(PathBuf::from(dir.str()?.extract::<String>()?))
}

// ---------------------------------------------------------------------------
// The terminal
// ---------------------------------------------------------------------------

/// The mac coder's safe_print: one line to the terminal, flushed, and never a
/// panic. A closed or broken stdout just drops the text, where `println!`
/// would panic and end the run.
pub fn out(text: &str) {
    let mut o = io::stdout().lock();
    let _ = writeln!(o, "{text}");
    let _ = o.flush();
}

/// out() without the newline, for the spinner's "\r" frames.
pub fn out_raw(text: &str) {
    let mut o = io::stdout().lock();
    let _ = write!(o, "{text}");
    let _ = o.flush();
}

/// `println!` that never panics: `say!("Step {n}")`, `say!()` for a blank line.
#[macro_export]
macro_rules! say {
    () => {
        $crate::out("")
    };
    ($($arg:tt)*) => {
        $crate::out(&format!($($arg)*))
    };
}

/// str(e) of a Python exception: its message, without the type name.
pub fn py_err_str(py: Python<'_>, e: &PyErr) -> String {
    e.value(py)
        .str()
        .and_then(|s| s.extract::<String>())
        .unwrap_or_else(|_| e.to_string())
}

// ---------------------------------------------------------------------------
// Python's text rules
// ---------------------------------------------------------------------------

/// Python's text-mode read (universal newlines): "\r\n" and a lone "\r" read
/// as "\n", so a CRLF checkout (Windows, autocrlf) gives the same text as an
/// LF one.
pub fn read_text(p: &Path) -> io::Result<String> {
    std::fs::read_to_string(p).map(|s| s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// The characters Python's str.strip() removes: Unicode whitespace plus
/// U+001C..U+001F, which Rust's trim() keeps.
pub fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `s.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

/// Python's `s.lstrip()`.
pub fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(py_is_space)
}

/// Python's `s.rstrip()`.
pub fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_is_space)
}

/// Python's `s.splitlines()`: breaks on \n, \r\n, \r, \x0b, \x0c, \x1c,
/// \x1d, \x1e, \x85, U+2028 and U+2029, with no empty piece after a final
/// break ("a\n" gives ["a"], "" gives []).
pub fn py_splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
                | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&s[start..i]);
        start = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                start = j + 1;
            }
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

// ---------------------------------------------------------------------------
// Python's json.dumps
// ---------------------------------------------------------------------------

/// Compact with Python's default separators: `{"a": 1, "b": 2}`.
struct PySeparators;

impl serde_json::ser::Formatter for PySeparators {
    fn begin_object_key<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
    fn begin_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W, first: bool) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            w.write_all(b", ")
        }
    }
}

/// json.dumps(value, ensure_ascii=False) with the default separators.
pub fn dumps(v: &Value) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, PySeparators);
    // Writing a Value into a Vec cannot fail, and serde_json only emits UTF-8.
    let _ = v.serialize(&mut ser);
    String::from_utf8(buf).unwrap_or_default()
}

/// json.dumps(value, indent=2, ensure_ascii=False).
pub fn dumps_pretty(v: &Value) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    let _ = v.serialize(&mut ser);
    String::from_utf8(buf).unwrap_or_default()
}

/// Post-pass for json.dumps' default ensure_ascii=True: every non-ASCII char
/// becomes \uXXXX (a surrogate pair above the BMP). Safe as a post-pass since
/// non-ASCII only ever occurs inside JSON string literals.
pub fn ensure_ascii(s: &str) -> String {
    let mut buf = String::with_capacity(s.len());
    for ch in s.chars() {
        let cp = ch as u32;
        if cp < 0x80 {
            buf.push(ch);
        } else if cp <= 0xFFFF {
            buf.push_str(&format!("\\u{cp:04x}"));
        } else {
            let v = cp - 0x10000;
            buf.push_str(&format!("\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)));
        }
    }
    buf
}

/// Python truthiness of a JSON value: None, False, 0, "", [] and {} are
/// false, everything else is true.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `str()` of a JSON value ("None", "True", "False", numbers as
/// written; a list or dict comes out as JSON, not as Python's repr).
pub fn py_str_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        other => dumps(other),
    }
}

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

#[pymodule]
fn agent_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<main::service::AgentService>()?;
    // CompressionController builds a second, text-only manager from this
    // class, and calls the two transcript functions below.
    m.add_class::<tool_registry::service::LLMManager>()?;
    m.add_function(pyo3::wrap_pyfunction!(main::view::compression_dump, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(main::view::compression_entry, m)?)?;
    // The subagent child's entry: coder/subagent/__main__.py calls it.
    m.add_function(pyo3::wrap_pyfunction!(subagent::service::run_subagent, m)?)?;
    Ok(())
}
