//! The scratchpad — the agent's own numbered notes, persisted per run.
//!
//! On-disk storage stays "milestone/milestone.md" to preserve existing data
//! and avoid a scratchpad/scratchpad/scratchpad.md collision with the parent
//! directory.

use std::path::{Path, PathBuf};

pub struct ScratchpadService {
    pub scratchpad_dir: PathBuf,
    pub scratchpad_file: PathBuf,
}

impl ScratchpadService {
    /// `scratchpad_base` is AutoCua/web/scratchpad, or a per-session
    /// subdirectory of it when agents run in parallel.
    pub fn new(scratchpad_base: &Path) -> std::io::Result<Self> {
        let scratchpad_dir = scratchpad_base.join("milestone");
        std::fs::create_dir_all(&scratchpad_dir)?;
        let scratchpad_file = scratchpad_dir.join("milestone.md");
        Ok(ScratchpadService { scratchpad_dir, scratchpad_file })
    }

    /// Append a scratchpad entry with sequential numbering. Returns whether
    /// the write landed — a failure becomes a failed tool result, never a
    /// panic.
    pub fn append_scratchpad(&self, content: &str) -> bool {
        let existing_count = std::fs::read_to_string(&self.scratchpad_file)
            .map(|text| text.lines().filter(|ln| !ln.trim().is_empty()).count())
            .unwrap_or(0);
        let entry = format!("{}. {content}\n", existing_count + 1);
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.scratchpad_file)
            .and_then(|mut f| f.write_all(entry.as_bytes()))
            .is_ok()
    }

    /// Read current scratchpad content.
    pub fn read_scratchpad(&self) -> String {
        std::fs::read_to_string(&self.scratchpad_file)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// Clear the scratchpad file.
    pub fn clear_scratchpad(&self) -> bool {
        if self.scratchpad_file.exists() {
            std::fs::remove_file(&self.scratchpad_file).is_ok()
        } else {
            true
        }
    }
}
