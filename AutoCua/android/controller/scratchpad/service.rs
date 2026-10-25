//! The scratchpad, the agent's notes: the copy of
//! ios/controller/scratchpad/service.py. On disk as
//! android/scratchpad/milestone/milestone.md (scratchpad/<session>/milestone/
//! for a parallel task, AutoCua_ANDROID_SESSION), which keeps clear of a
//! scratchpad/scratchpad/scratchpad.md collision with the parent folder.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::agent::android_use::service::read_text;

pub struct ScratchpadService {
    pub scratchpad_dir: PathBuf,
    pub scratchpad_file: PathBuf,
}

impl ScratchpadService {
    /// `android_dir` is AutoCua/android. The folder is made here; failing
    /// that is an error, as on iOS.
    pub fn new(android_dir: &Path) -> std::io::Result<Self> {
        let session = std::env::var("AutoCua_ANDROID_SESSION").unwrap_or_default();
        let scratchpad_dir = if session.is_empty() {
            android_dir.join("scratchpad").join("milestone")
        } else {
            android_dir.join("scratchpad").join(session).join("milestone")
        };
        std::fs::create_dir_all(&scratchpad_dir).map_err(|e| {
            eprintln!("Error creating scratchpad directory: {e}");
            e
        })?;
        let scratchpad_file = scratchpad_dir.join("milestone.md");
        Ok(ScratchpadService { scratchpad_dir, scratchpad_file })
    }

    /// Append a scratchpad entry with sequential numbering.
    pub fn append_scratchpad(&self, scratchpad_content: &str) -> bool {
        let attempt = (|| -> std::io::Result<()> {
            let mut existing_count = 0;
            if self.scratchpad_file.exists() {
                existing_count = read_text(&self.scratchpad_file)?
                    .lines()
                    .filter(|l| !l.trim().is_empty())
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
                eprintln!("Error appending scratchpad entry: {e}");
                false
            }
        }
    }

    /// Read current scratchpad content.
    pub fn read_scratchpad(&self) -> String {
        if !self.scratchpad_file.exists() {
            return String::new();
        }
        match read_text(&self.scratchpad_file) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                eprintln!("Error reading scratchpad: {e}");
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
                eprintln!("Error clearing scratchpad: {e}");
                false
            }
        }
    }
}
