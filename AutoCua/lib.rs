//! Crate root: AutoCua.agent_native, the one Rust extension module behind
//! the Rust agents: the browser agent (browser/), the Android agent
//! (android/), the coder (coder/), the computer-use agent (computer/) and the
//! iPhone agent (ios/agent/). The web tool (web/) and memory compression
//! (memory_compression/), which every agent shares, Python's included, and
//! the desktop app's chat memory (agent_conversation/) are parts of it too.
//!
//! ONE crate for all of them: this file, Cargo.toml, Cargo.lock and build.rs
//! sit here in AutoCua/, and each agent's sources sit in its own folder,
//! declared below, no src/. AutoCua/utils/rust.py builds the
//! crate lazily (plain `cargo build --release`, no maturin) and copies the
//! dylib beside this file as agent_native.so. Each agent is its own part of
//! the module, AutoCua.agent_native.browser, .android, .coder, .computer and
//! .ios, which its loader re-exports (browser/agent/__init__.py,
//! android/agent/__init__.py, coder/__init__.py, computer/__init__.py and
//! computer/skills/__init__.py, and the __init__.py of each folder under
//! ios/agent/); the web tool is .web (web/__init__.py).

use std::path::PathBuf;
use std::sync::OnceLock;

use pyo3::prelude::*;

/// AutoCua, the directory holding this extension module and the agents'
/// folders, resolved lazily because importlib only sets `__file__` after
/// module init returns.
pub fn native_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    if let Some(dir) = DIR.get() {
        return Ok(dir);
    }
    let module = py
        .import("AutoCua.agent_native")
        .or_else(|_| py.import("agent_native"))?;
    let file: String = module.getattr("__file__")?.extract()?;
    let dir = PathBuf::from(file)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(DIR.get_or_init(|| dir))
}

/// The browser agent (AutoCua/browser): the agent, the browser session, the
/// page scanner and the tools. AutoCua.agent_native.browser for Python.
pub mod browser {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use pyo3::prelude::*;

    // Every subdirectory's .rs files compile into the one crate (AutoCua/lib.rs).
    // Nothing here builds a second binary: the page scanner used to, and is a
    // module now.
    pub mod agent {
        pub mod main_driver;
    }
    // The browser session keeps its own directory: browser/connector/browser.rs.
    // The #[path] lets the file keep its name while the module is
    // `crate::browser::connector`.
    #[path = "connector/browser.rs"]
    pub mod connector;
    // Sends the headful browser back when a page's new tab pulls it forward.
    #[path = "connector/page_tab_guard.rs"]
    pub mod page_tab_guard;
    // Stages connector/AutoCuaBridge for a Chrome launch so Chrome runs its current
    // code (extension mode; not hooked to the launcher yet).
    #[path = "connector/stage_bridge.rs"]
    pub mod stage_bridge;
    // The bridge to AutoCuaBridge: the server the extension dials, over which the
    // scanner and every tool reach the browser in extension mode.
    #[path = "connector/bridge.rs"]
    pub mod bridge;
    // The hand test for AutoCuaBridge (test.command): stages it, restarts the test
    // Chrome and serves the bridge that writes each Test press under debug/.
    #[path = "connector/bridge_test.rs"]
    pub mod bridge_test;
    pub mod controller;
    // The page scanner. It used to build as a second binary of this package and
    // run as a subprocess; it is a plain module now, reading pages over the one
    // CDP session browser.rs owns.
    pub mod tree {
        pub mod element;
    }
    // The browser agent's tool registry and its LLM manager. The providers it
    // calls are the shared Python ones in AutoCua/llm_provider.
    pub mod tool_registry {
        pub mod service;
    }

    pyo3::create_exception!(
        agent_native,
        ScannerError,
        pyo3::exceptions::PyRuntimeError,
        "The page scanner failed, or the browser stopped answering."
    );

    /// AutoCua/browser, beside the extension module (AutoCua/agent_native.so),
    /// found once; AutoCua_BROWSER_DIR overrides it.
    pub fn browser_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        if let Some(dir) = DIR.get() {
            return Ok(dir);
        }
        let dir = if let Ok(v) = std::env::var("AutoCua_BROWSER_DIR") {
            PathBuf::from(v)
        } else {
            crate::native_dir(py)?.join("browser")
        };
        Ok(DIR.get_or_init(|| dir))
    }

    /// The Chrome user-data-dir for a named browser profile, asked of Python.
    ///
    /// Rust cannot answer this itself: `AutoCua/__init__.py` handles the
    /// compiled-vs-dev base directory and the AutoCua_DATA_DIR override, and a
    /// second definition of "where is AutoCua_data" drifting apart from that one
    /// is the exact bug that module exists to prevent.
    /// Before any launch: AutoCua/utils/chrome.py installs Google Chrome when it
    /// is missing. Best-effort, and quiet when Chrome is there: whatever it prints
    /// is the story, and find_chrome still falls back to a Chromium or Edge.
    pub fn ensure_chrome(py: Python<'_>) {
        let _ = py
            .import("AutoCua.utils.chrome")
            .and_then(|m| m.call_method0("ensure_chrome"));
    }

    /// Before a debugging-port launch of a headful browser: the guard that sends
    /// it back when one of its pages opens a tab and pulls it forward
    /// (connector/page_tab_guard.rs). A headless browser can never come forward,
    /// and extension mode has no port for the guard to watch.
    pub fn ensure_browser(py: Python<'_>, port: u16, headless: bool) {
        if !headless {
            // What the guard's own process is started with: this Python, and the
            // folder holding the AutoCua package (browser/ -> AutoCua/ -> it), so it
            // imports this extension whatever its working directory. A packaged
            // app has no Python to start it with (sys.executable is the app
            // itself, which would open a second app), so none is given.
            let compiled = py
                .import("AutoCua")
                .and_then(|m| m.getattr("IS_COMPILED"))
                .and_then(|v| v.extract::<bool>())
                .unwrap_or(false);
            let python: PathBuf = if compiled {
                PathBuf::new()
            } else {
                py.import("sys")
                    .and_then(|sys| sys.getattr("executable"))
                    .and_then(|exe| exe.extract())
                    .unwrap_or_default()
            };
            let package_parent = browser_dir(py)
                .ok()
                .and_then(|dir| dir.parent()?.parent().map(PathBuf::from))
                .unwrap_or_default();
            // Without the GIL: the wait for a new guard to start (half a second)
            // must not stop the process's other Python threads, the desktop
            // app's among them.
            py.detach(|| page_tab_guard::start(port, &python, &package_parent));
        }
    }

    /// The guard process (connector/page_tab_guard.rs): `python -c` calls this, on
    /// that process's main thread, and it returns only when another guard already
    /// watches `port`. Off macOS it returns at once.
    #[pyfunction]
    fn page_tab_guard_serve(py: Python<'_>, port: u16) {
        py.detach(|| page_tab_guard::serve(port));
    }

    pub fn browser_profile_dir(py: Python<'_>, name: Option<&str>) -> PyResult<PathBuf> {
        let module = py.import("AutoCua")?;
        let dir = module.call_method1("browser_profile_dir", (name,))?;
        Ok(PathBuf::from(dir.str()?.extract::<String>()?))
    }

    /// AutoCua_data itself, asked of Python for the same reason. The indexes a
    /// run's scrapes store (controller/run_script window.rs) go under it.
    pub fn data_root(py: Python<'_>) -> PyResult<PathBuf> {
        let module = py.import("AutoCua")?;
        let dir = module.call_method0("data_root")?;
        Ok(PathBuf::from(dir.str()?.extract::<String>()?))
    }

    /// AutoCua_data/vault/credentials.json, whose "browser" section holds the logins
    /// the `vault` tool fills (controller/vault), asked of Python like data_root.
    pub fn vault_file(py: Python<'_>) -> PyResult<PathBuf> {
        let module = py.import("AutoCua")?;
        let path = module.call_method0("vault_file")?;
        Ok(PathBuf::from(path.str()?.extract::<String>()?))
    }

    /// AutoCua/browser/agent, kept as a helper because the main_driver prompts
    /// the agent reads live under it.
    pub fn agent_dir(py: Python<'_>) -> PyResult<PathBuf> {
        Ok(browser_dir(py)?.join("agent"))
    }

    /// AutoCua/browser/connector: browser.rs and the glow assets it injects (glow/).
    pub fn connector_dir(py: Python<'_>) -> PyResult<PathBuf> {
        Ok(browser_dir(py)?.join("connector"))
    }

    /// Fills AutoCua.agent_native.browser, this agent's part of the module.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        use agent::main_driver::view;

        m.add("ScannerError", m.py().get_type::<ScannerError>())?;
        m.add("CHROME_PORT", connector::CHROME_PORT)?;
        m.add_class::<connector::BrowserScanner>()?;
        m.add_function(pyo3::wrap_pyfunction!(connector::launch_chrome, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(stage_bridge::stage_bridge, m)?)?;
        m.add_class::<bridge::PyBridge>()?;
        m.add_function(pyo3::wrap_pyfunction!(bridge_test::bridge_test, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(bridge::idle_bridge_start, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(bridge::idle_bridge_stop, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(connector::is_blank_page, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(connector::blank_html, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(connector::ensure_tab, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(page_tab_guard_serve, m)?)?;

        m.add_class::<agent::main_driver::service::AgentService>()?;
        m.add_class::<tool_registry::service::LLMManager>()?;
        m.add_function(pyo3::wrap_pyfunction!(
            tool_registry::service::_tool_calls_to_steps_debug, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(
            tool_registry::service::_main_tools_debug, m)?)?;
        m.add_class::<view::AgentResponseFormatter>()?;
        m.add_function(pyo3::wrap_pyfunction!(controller::view::_route_action_debug, m)?)?;
        // The transcript codec — exported both for CompressionController (which
        // takes compression_dump/compression_entry as callables) and for
        // differential testing against the Python originals.
        m.add_function(pyo3::wrap_pyfunction!(view::compression_dump, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::compression_entry, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::decode_step_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::encode_step_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::decode_results_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::encode_results_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::wire_calls_from_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::snapshot_turn_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::looks_native_py, m)?)?;
        Ok(())
    }
}

/// The Android agent (AutoCua/android): the agent, its tool registry and
/// controller, and the phone reader it scans with (tree/test.py drives it by
/// hand). AutoCua.agent_native.android for Python.
pub mod android {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use pyo3::prelude::*;

    pub mod agent {
        /// The driver: the agent loop (service.rs) and the transcript codec with
        /// the display formatter (view.rs), beside the three prompts.
        pub mod android_use {
            pub mod service;
            pub mod view;
        }
        /// <skills>: the .md matched to the app in front.
        pub mod skills {
            pub mod service;
        }
    }
    /// The controller: route_action (view.rs), the phone's hands, with the
    /// element actions (service.rs), the todo and the scratchpad, and one
    /// service.rs per tool folder as in ../browser/controller.
    pub mod controller {
        pub mod service;
        pub mod view;
        pub mod task_tracker {
            pub mod service;
        }
        pub mod scratchpad {
            pub mod service;
        }
        pub mod tool {
            pub mod open_app {
                pub mod service;
            }
            pub mod shell {
                pub mod service;
            }
            /// The vault: the logins saved for the app in front, and PIN pads.
            pub mod vault {
                pub mod service;
            }
            /// The system Back: a page back, or out of a full-screen video player.
            pub mod back {
                pub mod service;
            }
            /// The video player: playing or not, play and pause, from the screen.
            pub mod videoplayer {
                pub mod service;
            }
        }
    }
    /// The tool registry and its LLM manager, the copy of ios/tool_registry.
    pub mod tool_registry {
        pub mod service;
    }
    /// The phone reader: the scanner the loop reads the phone with
    /// (UIElementScanner), which tree/test.py drives by hand.
    pub mod tree {
        pub mod element;
    }

    /// AutoCua/android, beside the extension module (AutoCua/agent_native.so),
    /// found once; AutoCua_ANDROID_DIR overrides it.
    pub fn android_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        if let Some(dir) = DIR.get() {
            return Ok(dir);
        }
        let dir = if let Ok(v) = std::env::var("AutoCua_ANDROID_DIR") {
            PathBuf::from(v)
        } else {
            crate::native_dir(py)?.join("android")
        };
        Ok(DIR.get_or_init(|| dir))
    }

    /// AutoCua/android/agent: the prompts the agent reads live under it, and the
    /// scratchpad and todo folders sit beside it.
    pub fn agent_dir(py: Python<'_>) -> PyResult<PathBuf> {
        Ok(android_dir(py)?.join("agent"))
    }

    /// AutoCua_data, asked of Python: AutoCua/__init__.py alone handles the
    /// compiled-vs-dev base directory and the AutoCua_DATA_DIR override, and a
    /// second definition of "where is AutoCua_data" drifting apart from that one
    /// is the exact bug that module exists to prevent. The user's skills
    /// (skills/android) live under it.
    pub fn data_root(py: Python<'_>) -> PyResult<PathBuf> {
        let module = py.import("AutoCua")?;
        let dir = module.call_method0("data_root")?;
        Ok(PathBuf::from(dir.str()?.extract::<String>()?))
    }

    /// Fills AutoCua.agent_native.android, this agent's part of the module.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        use agent::android_use::view;

        m.add_class::<agent::android_use::service::AgentService>()?;
        m.add_class::<tree::element::UIElementScanner>()?;
        m.add_class::<tool_registry::service::LLMManager>()?;
        m.add_function(pyo3::wrap_pyfunction!(tool_registry::service::_tool_calls_to_steps_debug, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(tool_registry::service::_main_tools_debug, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(tool_registry::service::_drop_bundled_terminal_debug, m)?)?;
        m.add_class::<view::AgentResponseFormatter>()?;
        // The transcript codec, exported both for CompressionController (which
        // takes compression_dump/compression_entry as callables) and for
        // differential testing against the Python originals.
        m.add_function(pyo3::wrap_pyfunction!(view::compression_dump, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::compression_entry, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::decode_step_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::encode_step_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::decode_results_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::encode_results_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::wire_calls_from_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::snapshot_turn_py, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(view::looks_native_py, m)?)?;
        Ok(())
    }
}

/// The coder (AutoCua/coder): the Rust copy of the macOS coder
/// (AutoCua/mac/agent/coder with its tools, its controller and its
/// sandbox), one copy for macOS, Windows and Linux. The mac coder's minions
/// are the subagents here. AutoCua.agent_native.coder for Python.
pub mod coder {
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

    /// AutoCua/coder, beside the extension module (AutoCua/agent_native.so),
    /// found once; AutoCua_CODER_DIR overrides it.
    pub fn coder_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        if let Some(dir) = DIR.get() {
            return Ok(dir);
        }
        let dir = if let Ok(v) = std::env::var("AutoCua_CODER_DIR") {
            PathBuf::from(v)
        } else {
            crate::native_dir(py)?.join("coder")
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
            $crate::coder::out("")
        };
        ($($arg:tt)*) => {
            $crate::coder::out(&format!($($arg)*))
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
    // Its part of the module
    // ---------------------------------------------------------------------------

    /// Fills AutoCua.agent_native.coder, this agent's part of the module.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_class::<main::service::AgentService>()?;
        // The sandbox the shell tools of computer use and the iPhone agent
        // run their commands in (from AutoCua.coder import Sandbox).
        m.add_class::<sandbox::service::PySandbox>()?;
        // CompressionController builds a second, text-only manager from this
        // class, and calls the two transcript functions below.
        m.add_class::<tool_registry::service::LLMManager>()?;
        m.add_function(pyo3::wrap_pyfunction!(main::view::compression_dump, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(main::view::compression_entry, m)?)?;
        // The subagent child's entry: coder/subagent/__main__.py calls it.
        m.add_function(pyo3::wrap_pyfunction!(subagent::service::run_subagent, m)?)?;
        Ok(())
    }
}

/// The computer-use agent (AutoCua/computer): the loop (service.rs), its
/// transcript codec and display (view.rs), each system's prompts (mac/,
/// windows/, linux/) and the
/// skills (skills/service.rs), each the Rust copy of the Mac agent's .py
/// it replaced. Each system's own parts, the tool registry, the
/// controller, the screen reader and the Telegram bridge, stay Python in
/// its own folder (AutoCua/mac, AutoCua/windows, AutoCua/linux) and are
/// called from here.
/// AutoCua.agent_native.computer for Python.
///
/// ONE agent for every desktop: macOS, Windows and Linux run this same
/// Rust. So nothing here names an operating system. What differs from one
/// to the next is a row of the platform table below, and platform() is the
/// one place that picks the row.
pub mod computer {
    use pyo3::exceptions::{PyTypeError, PyValueError};
    use pyo3::ffi;
    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList, PyString, PyTuple, PyType};

    /// The agent loop.
    pub mod service;
    /// Its transcript codec, compression dump and terminal display of a step.
    pub mod view;
    /// <skills>: the .md matched to the app in front, or to the site a
    /// browser shows.
    pub mod skills {
        pub mod service;
    }

    /// What differs between the operating systems the agent runs on: one row
    /// per system, read by the agent's files in place of a name written out.
    pub struct Platform {
        /// The system's package under AutoCua/, "mac" in AutoCua/mac. The
        /// folder of its own parts (its controller keeps the scratchpad
        /// there), the module path of its tools (AutoCua.<package>.
        /// controller.tool), the skills name (skills_dir, default_skills)
        /// and the platform a run's training data is recorded under all
        /// come from it.
        pub package: &'static str,
        /// The words that mark an app's name as a web browser's.
        pub browser_keywords: &'static [&'static str],
        /// The iPhone sub-agent (ios_agent) runs beside the Android one: in
        /// Daisy chain mode a request looks for the paired iPhone too, shows
        /// it in a window of its own (phone_window) and lets it go at the
        /// end. False: android_agent is the only phone.
        pub ios_agent: bool,
        /// Runs are recorded as training data (AutoCua/data_extraction.py):
        /// the episode is opened with the system's screen (the scanner
        /// module's get_screen), every step is added and the run is closed.
        pub training_data: bool,
        /// The agent makes the sandbox workspace on the Desktop
        /// (~/Desktop/sandbox_workspace) when it starts, and keeps its path
        /// as sandbox_root.
        pub desktop_sandbox: bool,
        /// How the message of a step that a UAC prompt blocks names it. Only
        /// Windows has one; the Mac's scanner never reports it.
        pub uac_prompt: &'static str,
    }

    /// macOS.
    pub const MAC: Platform = Platform {
        package: "mac",
        browser_keywords: &["chrome", "firefox", "edge", "opera", "brave", "safari", "vivaldi", "browser"],
        ios_agent: true,
        training_data: true,
        desktop_sandbox: true,
        uac_prompt: "A UAC prompt",
    };

    /// Windows: android_agent is its only phone (its controller has no
    /// iPhone calls), no training data (its scanner has no get_screen) and
    /// no Desktop sandbox.
    pub const WINDOWS: Platform = Platform {
        package: "windows",
        browser_keywords: &["chrome", "firefox", "edge", "opera", "brave", "safari", "vivaldi", "browser"],
        ios_agent: false,
        training_data: false,
        desktop_sandbox: false,
        uac_prompt: "A Windows UAC prompt",
    };

    /// Linux: android_agent is its only phone (its controller has no
    /// iPhone calls) and no training data, as on Windows; the Desktop
    /// sandbox and the UAC sentence are the Mac's, and Chromium counts as
    /// a browser.
    pub const LINUX: Platform = Platform {
        package: "linux",
        browser_keywords: &["chrome", "chromium", "firefox", "edge", "opera", "brave", "safari", "vivaldi", "browser"],
        ios_agent: false,
        training_data: false,
        desktop_sandbox: true,
        uac_prompt: "A UAC prompt",
    };

    /// The row of the system this agent runs on, chosen once: the host's
    /// (Windows, Linux, or macOS on every other host), or the one
    /// AutoCua_COMPUTER_PLATFORM names ("mac", "windows" or "linux"), so a
    /// test can run a system's row on another host.
    pub fn platform() -> &'static Platform {
        static ROW: std::sync::OnceLock<&'static Platform> = std::sync::OnceLock::new();
        ROW.get_or_init(|| match std::env::var("AutoCua_COMPUTER_PLATFORM").as_deref() {
            Ok("windows") => &WINDOWS,
            Ok("linux") => &LINUX,
            Ok("mac") => &MAC,
            _ if cfg!(target_os = "windows") => &WINDOWS,
            _ if cfg!(target_os = "linux") => &LINUX,
            _ => &MAC,
        })
    }

    /// This part's name in sys.modules; its modules sit below it, named as
    /// the files are (AutoCua.agent_native.computer.view).
    pub const NATIVE: &str = "AutoCua.agent_native.computer";

    /// The type name CPython puts in its own error texts ("cannot unpack
    /// non-iterable X object"): a class's own name, or for a built-in type
    /// its dotted C name ("int", "datetime.date").
    pub fn type_name(value: &Bound<'_, PyAny>) -> PyResult<String> {
        let kind = value.get_type();
        let name: String = kind.getattr("__name__")?.extract()?;
        // SAFETY: a valid type object.
        let flags = unsafe { ffi::PyType_GetFlags(kind.as_type_ptr()) };
        if flags & ffi::Py_TPFLAGS_HEAPTYPE != 0 {
            return Ok(name);
        }
        let module: String = kind.getattr("__module__")?.extract()?;
        Ok(if module == "builtins" { name } else { format!("{module}.{name}") })
    }

    /// `a, b = value`: Python's unpacking of exactly two items, with the
    /// errors the running Python gives (3.14 added "got N" for a list, tuple
    /// or dict with too many).
    pub fn unpack2<'py>(value: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>)> {
        let py = value.py();
        let not_enough = |got: usize| PyValueError::new_err(format!("not enough values to unpack (expected 2, got {got})"));
        let mut items = match value.try_iter() {
            Ok(items) => items,
            Err(e) => {
                let kind = value.get_type();
                // SAFETY: a valid type object and a valid object.
                let has_iter = unsafe { !ffi::PyType_GetSlot(kind.as_type_ptr(), ffi::Py_tp_iter).is_null() };
                let is_sequence = unsafe { ffi::PySequence_Check(value.as_ptr()) } == 1;
                if e.is_instance_of::<PyTypeError>(py) && !has_iter && !is_sequence {
                    return Err(PyTypeError::new_err(format!(
                        "cannot unpack non-iterable {} object",
                        type_name(value)?
                    )));
                }
                return Err(e);
            }
        };
        let first = items.next().ok_or_else(|| not_enough(0))??;
        let second = items.next().ok_or_else(|| not_enough(1))??;
        match items.next() {
            None => Ok((first, second)),
            Some(Err(e)) => Err(e),
            Some(Ok(_)) => {
                let exact = value.is_exact_instance_of::<PyList>()
                    || value.is_exact_instance_of::<PyTuple>()
                    || value.is_exact_instance_of::<PyDict>();
                let message = if exact && py.version_info() >= (3, 14) {
                    format!("too many values to unpack (expected 2, got {})", value.len()?)
                } else {
                    "too many values to unpack (expected 2)".to_string()
                };
                Err(PyValueError::new_err(message))
            }
        }
    }

    /// Python's `except` block. While this guard lives, `error` is the
    /// exception being handled: sys.exc_info() shows it, and an exception
    /// the handler's own code raises gets it as its __context__, as in
    /// Python. Dropping the guard puts back what was being handled before.
    /// It holds raw pointers, so it cannot leave the thread, and so the
    /// call, that made it.
    pub struct Handling {
        /// What the running frame's own entry held before: three owned
        /// references, or nulls when it held nothing.
        before: [*mut ffi::PyObject; 3],
    }

    impl Handling {
        pub fn new(py: Python<'_>, error: &PyErr) -> Self {
            let nothing = std::ptr::null_mut::<ffi::PyObject>();
            let mut shown = [nothing; 3];
            let mut outer = [nothing; 3];
            let kind = error.get_type(py).into_ptr();
            let value = error.value(py).clone().into_ptr();
            let traceback = error.traceback(py).map_or(nothing, |t| t.into_ptr());
            // SAFETY: the GIL is held. GetExcInfo fills in new references
            // (or NULL), and SetExcInfo takes over the three it is given,
            // NULL included.
            unsafe {
                // A generator or coroutine has an entry of its own, and when
                // that is empty GetExcInfo answers with its caller's. So
                // the running entry is emptied and the question asked again:
                // the same answer means the entry was empty, and empty is
                // what the guard must leave behind.
                ffi::PyErr_GetExcInfo(&mut shown[0], &mut shown[1], &mut shown[2]);
                ffi::PyErr_SetExcInfo(nothing, nothing, nothing);
                ffi::PyErr_GetExcInfo(&mut outer[0], &mut outer[1], &mut outer[2]);
                let own = shown[1] != outer[1];
                for object in outer {
                    ffi::Py_XDECREF(object);
                }
                if !own {
                    for object in shown {
                        ffi::Py_XDECREF(object);
                    }
                    shown = [nothing; 3];
                }
                ffi::PyErr_SetExcInfo(kind, value, traceback);
            }
            Handling { before: shown }
        }
    }

    impl Drop for Handling {
        fn drop(&mut self) {
            // SAFETY: dropped where it was made, with the GIL held.
            // SetExcInfo takes over the three references kept in new().
            unsafe { ffi::PyErr_SetExcInfo(self.before[0], self.before[1], self.before[2]) };
        }
    }

    /// Makes a class of this part look like the Python class it replaced:
    /// its __module__ is the old module's name, and its C name is the bare
    /// class name. The extension gives a class a dotted C name, which
    /// CPython prints in its own errors ("'builtins.X' object has no
    /// attribute ..."); naming the class again drops the dots, so those
    /// errors read as they did.
    pub fn as_python_class(class: &Bound<'_, PyType>, module: &Bound<'_, PyString>) -> PyResult<()> {
        class.setattr("__module__", module)?;
        class.setattr("__name__", class.getattr("__name__")?)
    }

    /// A module that takes a .py's place: `path` is where it sits below the
    /// part ("skills.service"), and `fill` gives it its names. It is made
    /// under its Python name, AutoCua.computer.<path>, so its __name__ and
    /// __package__, its functions' __module__ and a logger named after it
    /// all read as the folder says. It has no __all__, as the .py had none: the extension's add
    /// calls keep one, and with it a star import would give other names
    /// than before. So it is taken out once the module is filled.
    fn python_module<'py>(
        py: Python<'py>,
        path: &str,
        fill: fn(&Bound<'py, PyModule>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyModule>> {
        let name = format!("AutoCua.computer.{path}");
        let module = PyModule::new(py, &name)?;
        if let Some((package, _)) = name.rsplit_once('.') {
            module.setattr("__package__", package)?;
        }
        fill(&module)?;
        if module.hasattr("__all__")? {
            module.delattr("__all__")?;
        }
        Ok(module)
    }

    /// Fills AutoCua.agent_native.computer with its modules, laid out as the
    /// folder is, each also put in sys.modules under its full name. The
    /// loaders (computer/__init__.py and computer/skills/__init__.py) then
    /// put them in sys.modules under their Python names.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        let modules = py.import("sys")?.getattr("modules")?;
        let driver = python_module(py, "service", service::register)?;
        let codec = python_module(py, "view", view::register)?;
        let knowledge = python_module(py, "skills.service", skills::service::register)?;
        let skills = PyModule::new(py, &format!("{NATIVE}.skills"))?;
        skills.add("service", &knowledge)?;
        m.add("service", &driver)?;
        m.add("view", &codec)?;
        m.add("skills", &skills)?;
        // The system whose row runs: the loader binds that system's own parts.
        m.add("PLATFORM", platform().package)?;
        for (name, module) in [
            ("service", &driver),
            ("view", &codec),
            ("skills", &skills),
            ("skills.service", &knowledge),
        ] {
            modules.set_item(format!("{NATIVE}.{name}"), module)?;
        }
        Ok(())
    }
}

/// The iPhone agent (AutoCua/ios/agent): the agent's own files, each the
/// Rust copy of the .py it replaced, in the same folder under the same
/// name, made the way the computer-use agent's are (mod computer, whose helpers
/// they use). The tool registry, the controller and the phone reader beside
/// it stay Python and are called from here, and so is the vault.
/// AutoCua.agent_native.ios for Python.
pub mod ios {
    use pyo3::prelude::*;

    pub mod agent {
        /// The driver: the agent loop (service.rs), and its transcript
        /// codec, compression dump and terminal display of a step (view.rs),
        /// beside the three prompts.
        pub mod iphone_use {
            pub mod service;
            pub mod view;
        }
        /// <skills>: the .md matched to the app in front.
        pub mod skills {
            pub mod service;
        }
    }

    /// The controller's services, moving from Python one file at a time,
    /// and the lines their service.py files each had inline. Every helper
    /// takes the service's own module, so a global it reads (os, open, str,
    /// logger) is that module's, read at the moment of use, as in Python.
    pub mod controller {
        use pyo3::exceptions::PyTypeError;
        use pyo3::prelude::*;
        use pyo3::types::{PyDict, PyString, PyTuple};

        use crate::computer::Handling;
        use crate::memory_compression::{format_value, global, is_exception, join_str};

        /// The scratchpad, the agent's numbered notes (milestone.md).
        pub mod scratchpad {
            pub mod service;
        }
        /// The todo list, the agent's numbered tasks (todo.md).
        pub mod task_tracker {
            pub mod service;
        }

        /// The controller's tools: open_app, shell and videoplayer.
        pub mod tool {
            /// The app launcher: installed apps, launching one, the home
            /// screen (open_app.rs, in place of open_app.py).
            pub mod open_app;
            /// The shell on the host Mac, in the coder's sandbox (shell.rs,
            /// in place of shell.py).
            pub mod shell;
            /// Full-screen video playback: play, pause, close, streaming
            /// (videoplayer.rs, in place of videoplayer.py).
            pub mod videoplayer;
        }

        /// A str, as a Python object.
        pub(crate) fn text<'py>(py: Python<'py>, s: &str) -> Bound<'py, PyAny> {
            PyString::new(py, s).into_any()
        }

        /// An f-string: its parts in order, each through format(part, ""),
        /// joined. A literal part is a str, which format gives back as it is.
        pub(crate) fn fstring<'py>(py: Python<'py>, parts: &[&Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyAny>> {
            let mut pieces = Vec::with_capacity(parts.len());
            for part in parts {
                pieces.push(format_value(part, "")?.into_any());
            }
            Ok(join_str(py, &pieces.iter().collect::<Vec<_>>())?.into_any())
        }

        /// `os.path.<function>`, looked up now, as Python looks it up at
        /// each call.
        pub(crate) fn os_path<'py>(module: &Bound<'py, PyModule>, function: &str) -> PyResult<Bound<'py, PyAny>> {
            global(module, "os")?.getattr("path")?.getattr(function)
        }

        /// The folder an __init__ made first, with Python's lookups in
        /// Python's order. ios_dir is two levels above the service's own
        /// folder (`folder`, as in "task_tracker"), found from where its
        /// service.py was, os.path.abspath(__file__). Then
        /// os.path.join(ios_dir, "scratchpad", session, leaf) for a parallel
        /// simulator task (AutoCua_IOS_SESSION), or without the session.
        pub(crate) fn session_dir<'py>(module: &Bound<'py, PyModule>, folder: &str, leaf: &str) -> PyResult<Bound<'py, PyAny>> {
            let py = module.py();
            // current_dir = os.path.dirname(os.path.abspath(__file__))
            let dirname = os_path(module, "dirname")?;
            let abspath = os_path(module, "abspath")?;
            let file = py.import("os")?.getattr("path")?.call_method1(
                "join",
                (crate::native_dir(py)?, "ios", "controller", folder, "service.py"),
            )?;
            let current_dir = dirname.call1((abspath.call1((file,))?,))?;
            // ios_dir = os.path.dirname(os.path.dirname(current_dir))
            let dirname = os_path(module, "dirname")?;
            let inner = os_path(module, "dirname")?.call1((current_dir,))?;
            let ios_dir = dirname.call1((inner,))?;
            // session = os.environ.get("AutoCua_IOS_SESSION") or ""
            let get = global(module, "os")?.getattr("environ")?.getattr("get")?;
            let mut session = get.call1(("AutoCua_IOS_SESSION",))?;
            if !session.is_truthy()? {
                session = PyString::new(py, "").into_any();
            }
            if session.is_truthy()? {
                os_path(module, "join")?.call1((&ios_dir, "scratchpad", &session, leaf))
            } else {
                os_path(module, "join")?.call1((&ios_dir, "scratchpad", leaf))
            }
        }

        /// `logger.<level>(message)`: the method is looked up before the
        /// message is made, as Python does.
        pub(crate) fn log<'py>(
            module: &Bound<'py, PyModule>,
            level: &str,
            message: impl FnOnce() -> PyResult<Bound<'py, PyAny>>,
        ) -> PyResult<()> {
            let method = global(module, "logger")?.getattr(level)?;
            method.call1((message()?,))?;
            Ok(())
        }

        /// An `except Exception as e:` block: `logger.error(f"{what}{e}")`
        /// (the exception through format, as an f-string's {e} does) while
        /// `e` is the exception being handled. What is not an Exception
        /// (KeyboardInterrupt, SystemExit) passes on untouched, as Err.
        pub(crate) fn log_failure(module: &Bound<'_, PyModule>, what: &str, e: PyErr) -> PyResult<()> {
            let py = module.py();
            if !is_exception(py, &e) {
                return Err(e);
            }
            let _handling = Handling::new(py, &e);
            log(module, "error", || fstring(py, &[&text(py, what), e.value(py)]))
        }

        /// A dict built in the order given, as a dict display builds it.
        pub(crate) fn dict<'py>(py: Python<'py>, items: &[(&str, &Bound<'py, PyAny>)]) -> PyResult<Bound<'py, PyDict>> {
            let built = PyDict::new(py);
            for (key, value) in items {
                built.set_item(key, value)?;
            }
            Ok(built)
        }

        /// An `except Exception as e:` block: `logger.error(f"{what}{str(e)}")`
        /// while `e` is the exception being handled. What is not an
        /// Exception (KeyboardInterrupt, SystemExit) passes on untouched.
        /// Ok(Err(e)) hands the handled exception back.
        pub(crate) fn logged<T>(module: &Bound<'_, PyModule>, what: &str, result: PyResult<T>) -> PyResult<Result<T, PyErr>> {
            let py = module.py();
            match result {
                Ok(value) => Ok(Ok(value)),
                Err(e) if is_exception(py, &e) => {
                    let _handling = Handling::new(py, &e);
                    log(module, "error", || {
                        let text = global(module, "str")?.call1((e.value(py),))?;
                        let what = PyString::new(py, what);
                        Ok(join_str(py, &[what.as_any(), format_value(&text, "")?.as_any()])?.into_any())
                    })?;
                    Ok(Err(e))
                }
                Err(e) => Err(e),
            }
        }

        /// `os.makedirs(self.<attr>, exist_ok=True)`, logged as `what` and
        /// raised again when it fails: the _ensure_..._directory methods.
        pub(crate) fn make_dir(module: &Bound<'_, PyModule>, slf: &Bound<'_, PyAny>, attr: &str, what: &str) -> PyResult<()> {
            let made = (|| -> PyResult<()> {
                let makedirs = global(module, "os")?.getattr("makedirs")?;
                let kwargs = PyDict::new(module.py());
                kwargs.set_item("exist_ok", true)?;
                makedirs.call((slf.getattr(attr)?,), Some(&kwargs))?;
                Ok(())
            })();
            logged(module, what, made)?
        }

        /// `open(self.<attr>, mode, encoding="utf-8")`, in Python's order:
        /// the global open first, then the attribute.
        pub(crate) fn open_utf8<'py>(
            module: &Bound<'py, PyModule>,
            slf: &Bound<'py, PyAny>,
            attr: &str,
            mode: &str,
        ) -> PyResult<Bound<'py, PyAny>> {
            let open = global(module, "open")?;
            let file = slf.getattr(attr)?;
            let kwargs = PyDict::new(module.py());
            kwargs.set_item("encoding", "utf-8")?;
            open.call((file, mode), Some(&kwargs))
        }

        /// An __init__ that takes self alone (`init` names it as Python
        /// does, "TaskTrackerService.__init__()"). A call that gives more is
        /// turned down in the words Python has for a def of that shape, so
        /// the first keyword is named before the count of positional
        /// arguments.
        pub(crate) fn only_self(init: &str, args: &Bound<'_, PyTuple>, kwargs: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
            let py = args.py();
            if let Some((keyword, _)) = kwargs.and_then(|kwargs| kwargs.iter().next()) {
                let message = if keyword.eq("self")? {
                    PyString::new(py, &format!("{init} got multiple values for argument 'self'")).into_any()
                } else {
                    let open = PyString::new(py, &format!("{init} got an unexpected keyword argument '"));
                    let close = PyString::new(py, "'");
                    join_str(py, &[open.as_any(), keyword.str()?.as_any(), close.as_any()])?.into_any()
                };
                return Err(PyTypeError::new_err((message.unbind(),)));
            }
            if !args.is_empty() {
                return Err(PyTypeError::new_err(format!(
                    "{init} takes 1 positional argument but {} were given",
                    args.len() + 1
                )));
            }
            Ok(())
        }
    }

    /// This part's name in sys.modules; its modules sit below it, named as
    /// the folders are (AutoCua.agent_native.ios.agent.iphone_use.view).
    pub const NATIVE: &str = "AutoCua.agent_native.ios";

    /// A module that takes a .py's place, as the computer part makes its own:
    /// `path` is where it sits below the part ("agent.skills.service"), and
    /// `fill` gives it its names. It is made under the name Python has
    /// always known it by, AutoCua.ios.<path>, and it has no __all__, as
    /// the .py had none.
    fn python_module<'py>(
        py: Python<'py>,
        path: &str,
        fill: fn(&Bound<'py, PyModule>) -> PyResult<()>,
    ) -> PyResult<Bound<'py, PyModule>> {
        let name = format!("AutoCua.ios.{path}");
        let module = PyModule::new(py, &name)?;
        if let Some((package, _)) = name.rsplit_once('.') {
            module.setattr("__package__", package)?;
        }
        fill(&module)?;
        if module.hasattr("__all__")? {
            module.delattr("__all__")?;
        }
        Ok(module)
    }

    /// Fills AutoCua.agent_native.ios with its modules, laid out as the
    /// folders are, each also put in sys.modules under its full name. The
    /// loaders (the __init__.py of iphone_use, skills, scratchpad,
    /// task_tracker and tool) then point the old dotted names at them.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        let modules = py.import("sys")?.getattr("modules")?;
        let driver = python_module(py, "agent.iphone_use.service", agent::iphone_use::service::register)?;
        let view = python_module(py, "agent.iphone_use.view", agent::iphone_use::view::register)?;
        let iphone_use = PyModule::new(py, &format!("{NATIVE}.agent.iphone_use"))?;
        iphone_use.add("service", &driver)?;
        iphone_use.add("view", &view)?;
        let service = python_module(py, "agent.skills.service", agent::skills::service::register)?;
        let skills = PyModule::new(py, &format!("{NATIVE}.agent.skills"))?;
        skills.add("service", &service)?;
        let agent = PyModule::new(py, &format!("{NATIVE}.agent"))?;
        agent.add("iphone_use", &iphone_use)?;
        agent.add("skills", &skills)?;
        m.add("agent", &agent)?;
        let notes = python_module(py, "controller.scratchpad.service", controller::scratchpad::service::register)?;
        let scratchpad = PyModule::new(py, &format!("{NATIVE}.controller.scratchpad"))?;
        scratchpad.add("service", &notes)?;
        let todo = python_module(py, "controller.task_tracker.service", controller::task_tracker::service::register)?;
        let task_tracker = PyModule::new(py, &format!("{NATIVE}.controller.task_tracker"))?;
        task_tracker.add("service", &todo)?;
        let launcher = python_module(py, "controller.tool.open_app", controller::tool::open_app::register)?;
        let shell = python_module(py, "controller.tool.shell", controller::tool::shell::register)?;
        let player = python_module(py, "controller.tool.videoplayer", controller::tool::videoplayer::register)?;
        let tool = PyModule::new(py, &format!("{NATIVE}.controller.tool"))?;
        tool.add("open_app", &launcher)?;
        tool.add("shell", &shell)?;
        tool.add("videoplayer", &player)?;
        let controller = PyModule::new(py, &format!("{NATIVE}.controller"))?;
        controller.add("scratchpad", &scratchpad)?;
        controller.add("task_tracker", &task_tracker)?;
        controller.add("tool", &tool)?;
        m.add("controller", &controller)?;
        for (name, module) in [
            ("agent", &agent),
            ("agent.iphone_use", &iphone_use),
            ("agent.iphone_use.service", &driver),
            ("agent.iphone_use.view", &view),
            ("agent.skills", &skills),
            ("agent.skills.service", &service),
            ("controller", &controller),
            ("controller.scratchpad", &scratchpad),
            ("controller.scratchpad.service", &notes),
            ("controller.task_tracker", &task_tracker),
            ("controller.task_tracker.service", &todo),
            ("controller.tool", &tool),
            ("controller.tool.open_app", &launcher),
            ("controller.tool.shell", &shell),
            ("controller.tool.videoplayer", &player),
        ] {
            modules.set_item(format!("{NATIVE}.{name}"), module)?;
        }
        Ok(())
    }
}

/// The web tool (AutoCua/web), one web search shared by every agent:
/// service.rs routes a query to the agent's own provider, one file per
/// provider's search and the browser-agent fallback (web_agent.rs) beside
/// it. Computer use (macOS, Windows, Linux) and the iPhone agent use it
/// from their Python controllers through AutoCua.agent_native.web (from
/// AutoCua.web import WebService); the Android agent and the coder call
/// crate::web::service::WebService from their Rust controllers.
pub mod web {
    use pyo3::prelude::*;

    pub mod service;
    pub mod web_agent;
    pub mod anthropic;
    pub mod chatgpt;
    pub mod google_search;
    pub mod groq_search;
    pub mod openrouter;
    pub mod perplexity_search;

    /// Fills AutoCua.agent_native.web: the WebService class.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_class::<service::WebService>()?;
        Ok(())
    }
}

/// Memory compression (AutoCua/memory_compression), shared by every agent:
/// the memory bar's gauge (memory_tracker.rs) and the rolling handoff
/// compression that keeps an agent's context down (controller.rs, with the
/// compressor itself in agent/service.rs). Python and the Rust agents import
/// it under its old dotted names, AutoCua.memory_compression.controller,
/// .memory_tracker and .agent.service, which memory_compression/__init__.py
/// points at the modules here. AutoCua.agent_native.memory_compression for
/// Python.
pub mod memory_compression {
    use pyo3::exceptions::{PyAttributeError, PyException, PyNameError};
    use pyo3::ffi;
    use pyo3::prelude::*;
    use pyo3::types::{PyList, PyString};

    /// The runtime side: when a compression starts, its worker thread, the
    /// indicator and the splice.
    pub mod controller;
    /// The main agent's memory bar.
    pub mod memory_tracker;
    pub mod agent {
        /// The compressor: the dump it reads, the LLM call and the entry that
        /// replaces the compressed steps.
        pub mod service;
    }

    /// Python's lookup of a global name in one of these modules: the module's
    /// own attribute first, then the builtins. So a name the Python module
    /// imported (json, threading, MemoryCompressionAgent) is read where it
    /// was, and a builtin patched for a test or by a packaged app (print,
    /// open) is the one called.
    pub fn global<'py>(module: &Bound<'py, PyModule>, name: &str) -> PyResult<Bound<'py, PyAny>> {
        let py = module.py();
        if let Some(value) = module.dict().get_item(name)? {
            return Ok(value);
        }
        match py.import("builtins")?.getattr(name) {
            Ok(value) => Ok(value),
            Err(e) if e.is_instance_of::<PyAttributeError>(py) => {
                Err(PyNameError::new_err(format!("name '{name}' is not defined")))
            }
            Err(e) => Err(e),
        }
    }

    /// `print(line)` with Python's print, so the line goes to sys.stdout
    /// wherever it points, as it did.
    pub fn print_line(module: &Bound<'_, PyModule>, line: &Bound<'_, PyString>) -> PyResult<()> {
        global(module, "print")?.call1((line,))?;
        Ok(())
    }

    /// `format(value, spec)`, which is what an f-string's {value} or
    /// {value:spec} turns into.
    pub fn format_value<'py>(value: &Bound<'py, PyAny>, spec: &str) -> PyResult<Bound<'py, PyString>> {
        let py = value.py();
        let spec = PyString::new(py, spec);
        // SAFETY: two valid objects; a new reference or NULL with the error set.
        let text = unsafe { Bound::from_owned_ptr_or_err(py, ffi::PyObject_Format(value.as_ptr(), spec.as_ptr())) }?;
        Ok(text.cast_into::<PyString>()?)
    }

    /// `"".join(parts)`: how an f-string puts its pieces together.
    pub fn join_str<'py>(py: Python<'py>, parts: &[&Bound<'py, PyAny>]) -> PyResult<Bound<'py, PyString>> {
        let text = PyString::new(py, "").call_method1("join", (PyList::new(py, parts.iter().copied())?,))?;
        Ok(text.cast_into::<PyString>()?)
    }

    /// Python's `except Exception`: true for an Exception, false for a
    /// KeyboardInterrupt, SystemExit or other BaseException, which pass on.
    pub fn is_exception(py: Python<'_>, e: &PyErr) -> bool {
        e.is_instance_of::<PyException>(py)
    }

    /// Fills AutoCua.agent_native.memory_compression with its three modules,
    /// each also put in sys.modules under its full name.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        let py = m.py();
        let modules = py.import("sys")?.getattr("modules")?;
        let base = "AutoCua.agent_native.memory_compression";
        // The service first: the controller imports two of its names.
        let service = PyModule::new(py, &format!("{base}.agent.service"))?;
        agent::service::register(&service)?;
        let agent = PyModule::new(py, &format!("{base}.agent"))?;
        agent.add("service", &service)?;
        let memory_tracker = PyModule::new(py, &format!("{base}.memory_tracker"))?;
        memory_tracker::register(&memory_tracker)?;
        let controller = PyModule::new(py, &format!("{base}.controller"))?;
        controller::register(&controller, &service)?;
        m.add("agent", &agent)?;
        m.add("controller", &controller)?;
        m.add("memory_tracker", &memory_tracker)?;
        for (name, module) in [
            ("agent", &agent),
            ("agent.service", &service),
            ("controller", &controller),
            ("memory_tracker", &memory_tracker),
        ] {
            modules.set_item(format!("{base}.{name}"), module)?;
        }
        Ok(())
    }
}

/// The chat memory (AutoCua/agent_conversation): the desktop app's chats,
/// index.json and one folder per chat, saved, resumed and rendered, and the
/// one-time move of old chats out of the install folder.
/// AutoCua.agent_native.agent_conversation for Python, which
/// agent_conversation/__init__.py also puts in sys.modules as
/// AutoCua.agent_conversation.service, the module's old name.
pub mod agent_conversation {
    use pyo3::prelude::*;

    pub mod service;

    /// Fills AutoCua.agent_native.agent_conversation.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        service::register(m)
    }
}

/// The module: one part per agent, one for the web tool, one for memory
/// compression and one for the chat memory, each also put in sys.modules
/// under its full name, so `from AutoCua.agent_native.browser import
/// AgentService` works the way an import from a package does.
#[pymodule]
fn agent_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let modules = py.import("sys")?.getattr("modules")?;
    let parts: [(&str, fn(&Bound<'_, PyModule>) -> PyResult<()>); 8] = [
        ("browser", browser::register),
        ("android", android::register),
        ("coder", coder::register),
        ("computer", computer::register),
        ("ios", ios::register),
        ("web", web::register),
        ("memory_compression", memory_compression::register),
        ("agent_conversation", agent_conversation::register),
    ];
    for (name, register) in parts {
        let part = PyModule::new(py, name)?;
        register(&part)?;
        m.add_submodule(&part)?;
        modules.set_item(format!("AutoCua.agent_native.{name}"), &part)?;
    }
    Ok(())
}
