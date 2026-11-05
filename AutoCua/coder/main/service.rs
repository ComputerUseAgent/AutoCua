//! The coder's main agent: its loop and its prompts.

/// The OS this coder runs on, in the prompt's own words: mac, windows or linux.
pub fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        os => os,
    }
}
