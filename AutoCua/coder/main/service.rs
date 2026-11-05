//! The coder's main agent: its loop and its prompts.

use std::path::Path;

/// The word in system_prompt.md that current_os() replaces.
const OS_PLACEHOLDER: &str = "{current_os}";

/// The OS this coder runs on, in the prompt's own words: mac, windows or linux.
pub fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        os => os,
    }
}

/// system_prompt.md from `main_dir` (this folder, AutoCua/coder/main) with the
/// OS filled in. One prompt serves every OS, so the OS is filled in here; a
/// prompt without the placeholder comes back exactly as written.
pub fn load_system_prompt(main_dir: &Path) -> std::io::Result<String> {
    let prompt = std::fs::read_to_string(main_dir.join("system_prompt.md"))?;
    Ok(prompt.replace(OS_PLACEHOLDER, current_os()))
}
