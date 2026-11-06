//! The ToDo task list: the copy of mac/controller/task_tracker/service.py.
//! Lives at AutoCua/coder/scratchpad/cli_todo/<session>/todo.md, so each
//! run's list stays its own.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Python's logging.lastResort: the bare message on stderr, never a panic.
fn log(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

pub struct TaskTrackerService {
    pub todo_dir: PathBuf,
    pub todo_file: PathBuf,
}

impl TaskTrackerService {
    /// `scratch_root` is AutoCua/coder/scratchpad. The folder is made here.
    /// Python raises when it cannot be made; this prints the same error and
    /// goes on, and each later write then fails and says so.
    pub fn new(scratch_root: &Path, session_id: &str) -> TaskTrackerService {
        let todo_dir = if session_id.is_empty() {
            scratch_root.join("cli_todo")
        } else {
            scratch_root.join("cli_todo").join(session_id)
        };
        let todo_file = todo_dir.join("todo.md");
        if let Err(e) = std::fs::create_dir_all(&todo_dir) {
            log(&format!("Error creating todo directory: {e}"));
        }
        TaskTrackerService { todo_dir, todo_file }
    }

    /// Save todo list content to the markdown file with auto-numbering
    /// (overwrites the old list).
    pub fn save_todo(&self, todo_content: &str) -> bool {
        let numbered = Self::add_task_numbers(todo_content);
        match std::fs::write(&self.todo_file, numbered) {
            Ok(()) => true, // Silent save - no terminal output
            Err(e) => {
                log(&format!("Error saving todo list: {e}"));
                false
            }
        }
    }

    /// Add #1., #2., etc. numbering to each task line.
    fn add_task_numbers(todo_content: &str) -> String {
        let mut numbered: Vec<String> = Vec::new();
        let mut task_number = 1;
        for line in todo_content.split('\n') {
            // A task line starts with - [ ] or - [x]
            let stripped = crate::py_strip(line);
            if stripped.starts_with("- [ ]") || stripped.starts_with("- [x]") {
                numbered.push(format!("#{task_number}. {stripped}"));
                task_number += 1;
            } else {
                // Keep non-task lines as they are (like Objective:)
                numbered.push(line.to_string());
            }
        }
        numbered.join("\n")
    }

    /// Mark task #number complete. False when the file is missing, the task
    /// is not in it, or the file cannot be read or written.
    pub fn update_task(&self, task_number: i64) -> bool {
        if !self.todo_file.exists() {
            log("Todo file doesn't exist");
            return false;
        }
        let content = match crate::read_text(&self.todo_file) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("Error updating task: {e}"));
                return false;
            }
        };
        let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
        let prefix = format!("#{task_number}.");
        for line in lines.iter_mut() {
            // The line starts with the task number (e.g. "#1. - [ ]")
            if crate::py_strip(line).starts_with(&prefix) {
                if line.contains("- [x]") {
                    return true; // already complete
                }
                // Mark as complete: replace [ ] with [x], once
                *line = line.replacen("- [ ]", "- [x]", 1);
                return match std::fs::write(&self.todo_file, lines.join("\n")) {
                    Ok(()) => true,
                    Err(e) => {
                        log(&format!("Error updating task: {e}"));
                        false
                    }
                };
            }
        }
        log(&format!("Task #{task_number} not found in todo list"));
        false
    }
}
