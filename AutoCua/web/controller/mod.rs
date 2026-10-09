//! Controller module for action block code routes — the Rust analog of
//! `controller/__init__.py`. One service per tool folder; view.rs routes an
//! action to the right one.
//!
//! These files live beside the manifest-less controller directory and are
//! compiled INTO the agent_native crate (see the module declarations in
//! web/lib.rs), so the whole web side builds as ONE crate with one target/
//! and one agent_native.so.
//!
//! A tool that touches the page does the CDP work itself, borrowing the
//! session the browser side owns — see `ScannerInner::with_tab`. The scanner
//! binary is not in that path: it reads pages, it does not drive them.

pub mod service;
pub mod view;

pub mod click {
    pub mod service;
}
pub mod dialog {
    pub mod service;
}
pub mod input {
    pub mod service;
}
pub mod keyboard {
    pub mod service;
}
pub mod run_script {
    pub mod service;
}
pub mod skills {
    pub mod service;
}
pub mod scroll {
    pub mod service;
}
pub mod tab {
    pub mod service;
}
pub mod wait {
    pub mod service;
}
pub mod done {
    pub mod service;
}
pub mod scratchpad {
    pub mod service;
}
pub mod todo_tracker {
    pub mod service;
}
