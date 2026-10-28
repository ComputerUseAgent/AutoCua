//! Crate root: the module behind `AutoCua.android.agent`.
//!
//! The whole Android side is ONE crate, the same shape as ../web: this file,
//! Cargo.toml, Cargo.lock and build.rs sit here, and the sources sit in their
//! own folders beside them, one file each and no mod.rs: the folders are
//! declared below. agent/__init__.py builds the crate lazily (plain
//! `cargo build --release`, through AutoCua/utils/rust.py), copies the dylib
//! beside itself as agent_native.so, and re-exports the classes from it.

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

/// AutoCua/android, the directory holding this extension module, resolved
/// lazily because importlib only sets `__file__` after module init returns.
pub fn android_dir(py: Python<'_>) -> PyResult<&'static PathBuf> {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    if let Some(dir) = DIR.get() {
        return Ok(dir);
    }
    let dir = if let Ok(v) = std::env::var("AutoCua_ANDROID_DIR") {
        PathBuf::from(v)
    } else {
        let module = py
            .import("AutoCua.android.agent_native")
            .or_else(|_| py.import("agent_native"))?;
        let file: String = module.getattr("__file__")?.extract()?;
        PathBuf::from(file)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
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

#[pymodule]
fn agent_native(m: &Bound<'_, PyModule>) -> PyResult<()> {
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
