//! Crate root: AutoCua.agent_native, the one Rust extension module behind
//! the three Rust agents: the web agent (web/), the Android agent (android/)
//! and the coder (coder/).
//!
//! ONE crate for all three: this file, Cargo.toml, Cargo.lock and build.rs
//! sit here in AutoCua/, and each agent's sources sit in its own folder,
//! declared below, no src/. AutoCua/utils/rust.py builds the
//! crate lazily (plain `cargo build --release`, no maturin) and copies the
//! dylib beside this file as agent_native.so. Each agent is its own part of
//! the module, AutoCua.agent_native.web, .android and .coder, which its
//! loader re-exports (web/agent/__init__.py, android/agent/__init__.py,
//! coder/__init__.py).

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

/// The web agent (AutoCua/web): the agent, the browser session, the page
/// scanner and the tools. AutoCua.agent_native.web for Python.
pub mod web {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use pyo3::prelude::*;

    // Every subdirectory's .rs files compile into the one crate (AutoCua/lib.rs).
    // Nothing here builds a second binary: the page scanner used to, and is a
    // module now.
    pub mod agent {
        pub mod main_driver;
    }
    // The browser session keeps its own directory: web/browser/browser.rs. The
    // #[path] lets the file keep its name while the module stays
    // `crate::web::browser`.
    #[path = "browser/browser.rs"]
    pub mod browser;
    // Sends the headful browser back when a page's new tab pulls it forward.
    #[path = "browser/page_tab_guard.rs"]
    pub mod page_tab_guard;
    // Stages browser/AutoCuaBridge for a Chrome launch so Chrome runs its current
    // code (extension mode; not hooked to the launcher yet).
    #[path = "browser/stage_bridge.rs"]
    pub mod stage_bridge;
    // The bridge to AutoCuaBridge: the server the extension dials, over which the
    // scanner and every tool reach the browser in extension mode.
    #[path = "browser/bridge.rs"]
    pub mod bridge;
    // The hand test for AutoCuaBridge (test.command): stages it, restarts the test
    // Chrome and serves the bridge that writes each Test press under debug/.
    #[path = "browser/bridge_test.rs"]
    pub mod bridge_test;
    pub mod controller;
    // The page scanner. It used to build as a second binary of this package and
    // run as a subprocess; it is a plain module now, reading pages over the one
    // CDP session browser.rs owns.
    pub mod tree {
        pub mod element;
    }
    // The web tool registry and its LLM manager. The providers it calls are the
    // shared Python ones in AutoCua/llm_provider.
    pub mod tool_registry {
        pub mod service;
    }

    pyo3::create_exception!(
        agent_native,
        ScannerError,
        pyo3::exceptions::PyRuntimeError,
        "The page scanner failed, or the browser stopped answering."
    );

    /// AutoCua/web, beside the extension module (AutoCua/agent_native.so),
    /// found once; AutoCua_WEB_DIR overrides it.
    pub fn web_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        if let Some(dir) = DIR.get() {
            return Ok(dir);
        }
        let dir = if let Ok(v) = std::env::var("AutoCua_WEB_DIR") {
            PathBuf::from(v)
        } else {
            crate::native_dir(py)?.join("web")
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
    /// (browser/page_tab_guard.rs). A headless browser can never come forward,
    /// and extension mode has no port for the guard to watch.
    pub fn ensure_browser(py: Python<'_>, port: u16, headless: bool) {
        if !headless {
            // What the guard's own process is started with: this Python, and the
            // folder holding the AutoCua package (web/ -> AutoCua/ -> it), so it
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
            let package_parent = web_dir(py)
                .ok()
                .and_then(|web| web.parent()?.parent().map(PathBuf::from))
                .unwrap_or_default();
            // Without the GIL: the wait for a new guard to start (half a second)
            // must not stop the process's other Python threads, the desktop
            // app's among them.
            py.detach(|| page_tab_guard::start(port, &python, &package_parent));
        }
    }

    /// The guard process (browser/page_tab_guard.rs): `python -c` calls this, on
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

    /// AutoCua_data/vault/credentials.json, whose "web" section holds the logins
    /// the `vault` tool fills (controller/vault), asked of Python like data_root.
    pub fn vault_file(py: Python<'_>) -> PyResult<PathBuf> {
        let module = py.import("AutoCua")?;
        let path = module.call_method0("vault_file")?;
        Ok(PathBuf::from(path.str()?.extract::<String>()?))
    }

    /// AutoCua/web/agent — kept as a helper because the main_driver prompts the
    /// agent reads live under it.
    pub fn agent_dir(py: Python<'_>) -> PyResult<PathBuf> {
        Ok(web_dir(py)?.join("agent"))
    }

    /// AutoCua/web/browser — browser.rs and the glow assets it injects (glow/).
    pub fn browser_dir(py: Python<'_>) -> PyResult<PathBuf> {
        Ok(web_dir(py)?.join("browser"))
    }

    /// Fills AutoCua.agent_native.web, this agent's part of the module.
    pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
        use agent::main_driver::view;

        m.add("ScannerError", m.py().get_type::<ScannerError>())?;
        m.add("CHROME_PORT", browser::CHROME_PORT)?;
        m.add_class::<browser::BrowserScanner>()?;
        m.add_function(pyo3::wrap_pyfunction!(browser::launch_chrome, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(stage_bridge::stage_bridge, m)?)?;
        m.add_class::<bridge::PyBridge>()?;
        m.add_function(pyo3::wrap_pyfunction!(bridge_test::bridge_test, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(bridge::idle_bridge_start, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(bridge::idle_bridge_stop, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(browser::is_blank_page, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(browser::blank_html, m)?)?;
        m.add_function(pyo3::wrap_pyfunction!(browser::ensure_tab, m)?)?;
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
    /// service.rs per tool folder as in ../web/controller.
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
            /// The web search: service.rs routes it, one file per provider's
            /// search and the browser-agent fallback beside it, as on iOS.
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

/// The module: one part per agent, each also put in sys.modules under its full
/// name, so `from AutoCua.agent_native.web import AgentService` works the way an
/// import from a package does.
#[pymodule]
fn agent_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let modules = py.import("sys")?.getattr("modules")?;
    let parts: [(&str, fn(&Bound<'_, PyModule>) -> PyResult<()>); 3] = [
        ("web", web::register),
        ("android", android::register),
        ("coder", coder::register),
    ];
    for (name, register) in parts {
        let part = PyModule::new(py, name)?;
        register(&part)?;
        m.add_submodule(&part)?;
        modules.set_item(format!("AutoCua.agent_native.{name}"), &part)?;
    }
    Ok(())
}
