//! The ToDo tracker — the agent's plan as a numbered markdown checklist.

use std::path::{Path, PathBuf};

pub struct TodoTrackerService {
    pub todo_dir: PathBuf,
    pub todo_file: PathBuf,
}

impl TodoTrackerService {
    /// `scratchpad_base` is AutoCua/browser/scratchpad, or a per-session
    /// subdirectory of it when agents run in parallel.
    pub fn new(scratchpad_base: &Path) -> std::io::Result<Self> {
        let todo_dir = scratchpad_base.join("todo");
        std::fs::create_dir_all(&todo_dir)?;
        let todo_file = todo_dir.join("todo.md");
        Ok(TodoTrackerService { todo_dir, todo_file })
    }

    /// A task line as "- [ ] ..." / "- [x] ...", or None for a line that is no
    /// task (the "Objective:" line, a blank one). A "#<n>. " in front, as the
    /// model sees the list in <todo_list> and may copy it back, is taken off,
    /// and "- [X]" counts as "- [x]": otherwise such a line would keep its old
    /// number next to the new ones, and update_todo would tick the wrong line.
    fn task_body(line: &str) -> Option<String> {
        let text = Self::without_number(line.trim());
        if text.starts_with("- [ ]") || text.starts_with("- [x]") {
            Some(text.to_string())
        } else {
            text.strip_prefix("- [X]").map(|rest| format!("- [x]{rest}"))
        }
    }

    /// `text` without the "#<n>. " that <todo_list> shows in front of a task.
    fn without_number(text: &str) -> &str {
        if let Some(rest) = text.strip_prefix('#') {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 {
                if let Some(after) = rest[digits..].strip_prefix('.') {
                    return after.trim_start();
                }
            }
        }
        text
    }

    /// Add #1., #2., etc. numbering to each task line. Non-task lines (like
    /// "Objective:") are kept as-is.
    fn add_task_numbers(todo_content: &str) -> String {
        let mut task_number = 1;
        todo_content
            .split('\n')
            .map(|line| match Self::task_body(line) {
                Some(body) => {
                    let numbered = format!("#{task_number}. {body}");
                    task_number += 1;
                    numbered
                }
                None => line.to_string(),
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
            // A task line only (a note written as "#1. ..." is not task #1), and
            // its own box decides: a "- [x]" in the line's text is not a tick.
            let Some(body) = Self::task_body(line).filter(|_| line.trim().starts_with(&prefix)) else {
                continue;
            };
            if body.starts_with("- [x]") {
                return true; // already complete
            }
            *line = line.replacen("- [ ]", "- [x]", 1);
            updated = true;
            break;
        }
        if updated {
            std::fs::write(&self.todo_file, lines.join("\n")).is_ok()
        } else {
            // Task not found — return true anyway to avoid blocking the flow.
            true
        }
    }

    /// Re-word task #number (`rewrite_todo`): what comes after its colon
    /// becomes the new proof, the line is open again, and `reason` stays
    /// beside it as "(why: ...)" so the lesson is in front of the agent every
    /// step. An empty `value` removes the line. The other lines keep their
    /// numbers: the model may tick or re-word more lines in the same turn by
    /// the numbers it saw, so numbers change only when `todo_list` writes the
    /// whole list again. Unlike update_task, which lets a missing line pass,
    /// a missing line, a changed part, a value on more than one line or a
    /// value with no words is an error the model is told about.
    pub fn rewrite_task(&self, task_number: i64, value: &str, reason: &str) -> Result<Rewritten, String> {
        let content = std::fs::read_to_string(&self.todo_file)
            .map_err(|_| "no todo list exists yet - call `todo_list` first".to_string())?;
        if value.trim().contains('\n') {
            return Err("`value` is one line - to add or split lines, write the list again with `todo_list`".to_string());
        }
        let prefix = format!("#{task_number}.");
        let mut lines: Vec<String> = content.split('\n').map(str::to_string).collect();
        // A task line only: a note the model wrote as "#1. ..." is not task #1.
        let Some(at) = lines
            .iter()
            .position(|line| line.trim().starts_with(&prefix) && Self::task_body(line).is_some())
        else {
            return Err(format!("there is no task #{task_number} in the todo list"));
        };
        // The new text without a "#n. " or "- [ ]" the model may have put in
        // front of it, or a "(why: ...)" it copied from the old line.
        let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat = Self::without_number(&flat).to_string();
        let mut text = match Self::task_body(&flat) {
            Some(body) => body[5..].trim().to_string(),
            None => flat,
        };
        text = Self::without_why(&text);
        let outcome = if value.trim().is_empty() {
            lines.remove(at);
            Rewritten::Removed
        } else if text.is_empty() {
            // Only a box or a number ("- [ ]", "#3."): no new line in it. Only a
            // truly empty value removes a line.
            return Err("`value` has no words - give the new line, or an empty value to remove the line".to_string());
        } else {
            // The part before ": " keeps the request's words: only what proves
            // it may change. ": " and not ":" alone, so a time, a ratio or a
            // url in the part ("before 23:00") stays in the part. A value with
            // no ": " is the new proof alone.
            let old = Self::task_body(&lines[at]).map(|body| Self::without_why(body[5..].trim())).unwrap_or_default();
            if let Some((old_part, _)) = old.split_once(": ") {
                match text.split_once(": ") {
                    None if text.to_lowercase().starts_with(&old_part.trim().to_lowercase()) => {}
                    None => text = format!("{}: {}", old_part.trim(), text),
                    Some((new_part, _)) if Self::same_words(new_part, old_part) => {}
                    Some(_) => {
                        return Err(format!(
                            "the part before the colon stays \"{}\" - re-word only what proves it",
                            old_part.trim()
                        ));
                    }
                }
            }
            lines[at] = format!("#{task_number}. - [ ] {text} (why: {reason})");
            Rewritten::Reworded
        };
        std::fs::write(&self.todo_file, lines.join("\n")).map_err(|e| format!("could not write the todo list: {e}"))?;
        Ok(outcome)
    }

    /// `text` without a "(why: ...)" that closes at its very end; one inside
    /// the text, with words after it, stays.
    fn without_why(text: &str) -> String {
        if let Some(open) = text.rfind(" (why: ") {
            let mut depth = 0;
            for (i, c) in text[open + 1..].char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    if open + 1 + i + 1 == text.len() {
                        return text[..open].trim_end().to_string();
                    }
                    break;
                }
            }
        }
        text.to_string()
    }

    /// The same words, whatever the case and spacing.
    fn same_words(a: &str, b: &str) -> bool {
        let words = |s: &str| s.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>();
        words(a) == words(b)
    }
}

/// What `rewrite_task` did to the line.
pub enum Rewritten {
    Reworded,
    Removed,
}
