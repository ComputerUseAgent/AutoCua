//! Crate root — the module behind `AutoCua.web.agent`.
//!
//! The package's `__init__.py` builds this crate lazily (plain
//! `cargo build --release`, no maturin), copies the dylib beside itself as
//! agent_native.so, and re-exports the classes from it.

use std::path::PathBuf;
use std::sync::OnceLock;

use pyo3::prelude::*;

// One crate for the whole web side — every subdirectory's .rs files compile
// into this single cdylib (one target/, one agent_native.so, both at web/).
// Nothing here builds a second binary: the page scanner used to, and is a
// module now.
pub mod agent {
    pub mod main_driver;
}
// The browser session keeps its own directory: web/browser/browser.rs. The
// #[path] lets the file keep its name while the module stays `crate::browser`.
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

/// AutoCua/web — the directory holding this extension module, resolved
/// lazily because importlib only sets `__file__` after module init returns.
pub fn web_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    if let Some(dir) = DIR.get() {
        return Ok(dir);
    }
    let dir = if let Ok(v) = std::env::var("AutoCua_WEB_DIR") {
        PathBuf::from(v)
    } else {
        let module = py
            .import("AutoCua.web.agent_native")
            .or_else(|_| py.import("agent_native"))?;
        let file: String = module.getattr("__file__")?.extract()?;
        PathBuf::from(file)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
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

/// AutoCua/web/agent — kept as a helper because the main_driver prompts the
/// agent reads live under it.
pub fn agent_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(web_dir(py)?.join("agent"))
}

/// AutoCua/web/browser — browser.rs and the glow assets it injects (glow/).
pub fn browser_dir(py: Python<'_>) -> PyResult<PathBuf> {
    Ok(web_dir(py)?.join("browser"))
}

#[pymodule]
fn agent_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    use agent::main_driver::view;

    m.add("ScannerError", m.py().get_type::<ScannerError>())?;
    m.add("CHROME_PORT", browser::CHROME_PORT)?;
    m.add_class::<browser::BrowserScanner>()?;
    m.add_function(pyo3::wrap_pyfunction!(browser::launch_chrome, m)?)?;
    m.add_function(pyo3::wrap_pyfunction!(stage_bridge::stage_bridge, m)?)?;
    m.add_class::<bridge::PyBridge>()?;
    m.add_function(pyo3::wrap_pyfunction!(bridge_test::bridge_test, m)?)?;
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
