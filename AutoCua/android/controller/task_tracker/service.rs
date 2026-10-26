//! The ToDo task list: the copy of ios/controller/task_tracker/service.py.
//! Lives at android/scratchpad/todo/todo.md (scratchpad/<session>/todo/
//! for a parallel task, AutoCua_ANDROID_SESSION), so each task's list stays
//! its own.

use std::path::{Path, PathBuf};

use crate::agent::android_use::service::read_text;

pub struct TaskTrackerService {
    pub todo_dir: PathBuf,
    pub todo_file: PathBuf,
}

impl TaskTrackerService {
    /// `android_dir` is AutoCua/android. The folder is made here; failing
    /// that is an error, as on iOS.
    pub fn new(android_dir: &Path) -> std::io::Result<Self> {
        let session = std::env::var("AutoCua_ANDROID_SESSION").unwrap_or_default();
        let todo_dir = if session.is_empty() {
            android_dir.join("scratchpad").join("todo")
        } else {
            android_dir.join("scratchpad").join(session).join("todo")
        };
        std::fs::create_dir_all(&todo_dir).map_err(|e| {
            eprintln!("Error creating todo directory: {e}");
            e
        })?;
        let todo_file = todo_dir.join("todo.md");
        Ok(TaskTrackerService { todo_dir, todo_file })
    }

    /// Save todo list content to the markdown file with auto-numbering.
    pub fn save_todo(&self, todo_content: &str) -> bool {
        let numbered = Self::add_task_numbers(todo_content);
        match std::fs::write(&self.todo_file, numbered) {
            Ok(()) => true, // Silent save - no terminal output
            Err(e) => {
                eprintln!("Error saving todo list: {e}");
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
            let stripped = line.trim();
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

    /// Mark task #number complete.
    pub fn update_task(&self, task_number: i64) -> bool {
        if !self.todo_file.exists() {
            eprintln!("Todo file doesn't exist");
            return false;
        }
        let content = match read_text(&self.todo_file) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error updating task: {e}");
                return false;
            }
        };
        let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
        let prefix = format!("#{task_number}.");
        for line in lines.iter_mut() {
            // The line starts with the task number (e.g. "#1. - [ ]")
            if line.trim().starts_with(&prefix) {
                if line.contains("- [x]") {
                    return true; // already complete
                }
                // Mark as complete: replace [ ] with [x], once
                *line = line.replacen("- [ ]", "- [x]", 1);
                return match std::fs::write(&self.todo_file, lines.join("\n")) {
                    Ok(()) => true,
                    Err(e) => {
                        eprintln!("Error updating task: {e}");
                        false
                    }
                };
            }
        }
        eprintln!("Task #{task_number} not found in todo list");
        false
    }
}
