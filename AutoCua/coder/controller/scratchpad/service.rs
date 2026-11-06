//! The scratchpad, the agent's notes: the copy of
//! mac/controller/scratchpad/service.py. On disk under
//! AutoCua/coder/scratchpad as cli_milestone/<session>/milestone.md for the
//! coder and cli_subagent/<session>/milestone.md for a subagent (mac's
//! cli_minion), one entry per line, `N. <text>`. The file keeps mac's name,
//! which stays clear of a scratchpad/scratchpad/scratchpad.md collision.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Python's logging.lastResort: the bare message on stderr, never a panic.
fn log(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

pub struct ScratchpadService {
    pub scratchpad_dir: PathBuf,
    pub scratchpad_file: PathBuf,
}

impl ScratchpadService {
    /// `scratch_root` is AutoCua/coder/scratchpad. `subagent_mode` gives a
    /// subagent its own folder, so it never writes into the coder's notes.
    /// The folder is made here. Python raises when it cannot be made; this
    /// prints the same error and goes on, and each later write then fails
    /// and says so.
    pub fn new(scratch_root: &Path, session_id: &str, subagent_mode: bool) -> ScratchpadService {
        let subdir = if subagent_mode { "cli_subagent" } else { "cli_milestone" };
        let scratchpad_dir = if session_id.is_empty() {
            scratch_root.join(subdir)
        } else {
            scratch_root.join(subdir).join(session_id)
        };
        let scratchpad_file = scratchpad_dir.join("milestone.md");
        if let Err(e) = std::fs::create_dir_all(&scratchpad_dir) {
            log(&format!("Error creating scratchpad directory: {e}"));
        }
        ScratchpadService { scratchpad_dir, scratchpad_file }
    }

    /// Append a scratchpad entry with sequential numbering: the number is
    /// one more than the count of non-blank lines already in the file.
    pub fn append_scratchpad(&self, scratchpad_content: &str) -> bool {
        let attempt = (|| -> std::io::Result<()> {
            let mut existing_count = 0;
            if self.scratchpad_file.exists() {
                existing_count = crate::read_text(&self.scratchpad_file)?
                    .split('\n')
                    .filter(|line| !crate::py_strip(line).is_empty())
                    .count();
            }
            let next_number = existing_count + 1;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.scratchpad_file)?;
            f.write_all(format!("{next_number}. {scratchpad_content}\n").as_bytes())
        })();
        match attempt {
            Ok(()) => true,
            Err(e) => {
                log(&format!("Error appending scratchpad entry: {e}"));
                false
            }
        }
    }

    /// Read current scratchpad content, stripped; "" when there is none.
    pub fn read_scratchpad(&self) -> String {
        if !self.scratchpad_file.exists() {
            return String::new();
        }
        match crate::read_text(&self.scratchpad_file) {
            Ok(text) => crate::py_strip(&text).to_string(),
            Err(e) => {
                log(&format!("Error reading scratchpad: {e}"));
                String::new()
            }
        }
    }

    /// Clear the scratchpad file.
    pub fn clear_scratchpad(&self) -> bool {
        if !self.scratchpad_file.exists() {
            return true;
        }
        match std::fs::remove_file(&self.scratchpad_file) {
            Ok(()) => true,
            Err(e) => {
                log(&format!("Error clearing scratchpad: {e}"));
                false
            }
        }
    }
}
