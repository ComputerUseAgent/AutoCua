//! The ToDo tracker — the agent's plan as a numbered markdown checklist.

use std::path::{Path, PathBuf};

pub struct TodoTrackerService {
    pub todo_dir: PathBuf,
    pub todo_file: PathBuf,
}

impl TodoTrackerService {
    /// `scratchpad_base` is AutoCua/web/scratchpad, or a per-session
    /// subdirectory of it when agents run in parallel.
    pub fn new(scratchpad_base: &Path) -> std::io::Result<Self> {
        let todo_dir = scratchpad_base.join("todo");
        std::fs::create_dir_all(&todo_dir)?;
        let todo_file = todo_dir.join("todo.md");
        Ok(TodoTrackerService { todo_dir, todo_file })
    }

    /// Add #1., #2., etc. numbering to each task line. Non-task lines (like
    /// "Objective:") are kept as-is.
    fn add_task_numbers(todo_content: &str) -> String {
        let mut task_number = 1;
        todo_content
            .split('\n')
            .map(|line| {
                let stripped = line.trim();
                if stripped.starts_with("- [ ]") || stripped.starts_with("- [x]") {
                    let numbered = format!("#{task_number}. {stripped}");
                    task_number += 1;
                    numbered
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Save todo list content to the markdown file with auto-numbering.
    /// Overwrites and re-numbers the whole list. Returns whether it landed.
    pub fn save_todo(&self, todo_content: &str) -> bool {
        let numbered = Self::add_task_numbers(todo_content);
        std::fs::write(&self.todo_file, numbered).is_ok()
    }

    /// Mark task #number complete. Missing task numbers still return true —
    /// blocking the whole run on a bookkeeping miss helps nobody.
    pub fn update_task(&self, task_number: i64) -> bool {
        let Ok(content) = std::fs::read_to_string(&self.todo_file) else {
            return false;
        };
        let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
        let prefix = format!("#{task_number}.");
        let mut updated = false;
        for line in lines.iter_mut() {
            if line.trim().starts_with(&prefix) {
                if line.contains("- [x]") {
                    return true; // already complete
                }
                *line = line.replacen("- [ ]", "- [x]", 1);
                updated = true;
                break;
            }
        }
        if updated {
            std::fs::write(&self.todo_file, lines.join("\n")).is_ok()
        } else {
            // Task not found — return true anyway to avoid blocking the flow.
            true
        }
    }
}
