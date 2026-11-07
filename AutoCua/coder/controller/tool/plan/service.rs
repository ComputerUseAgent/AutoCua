//! The `plan` tool: the copy of mac/controller/tool/plan.py, the session's
//! plan document. The plan is the agent's detailed, codebase-anchored route
//! (written after exploration); the ToDo only tracks it. It lives under
//! AutoCua/coder/scratchpad as cli_plan/<session>/plan.md, with the same
//! isolation as the todo, beside plan_no.txt, its revision number.
//!
//! Three ops, as the `plan` tool describes them to the model:
//!   set:  overwrite the complete plan
//!   add:  append value at the end
//!   edit: replace lines from..to (inclusive, 1-indexed) with value
//!
//! Edit ranges are checked against the current file and fail loudly on
//! drift, like `replace` does: the line numbers come from the latest <plan>
//! block that render_plan() draws.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Python's logging.lastResort: the bare message on stderr, never a panic.
fn log(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// Python's `not value or not value.strip()`.
fn is_blank(value: &str) -> bool {
    crate::coder::py_strip(value).is_empty()
}

/// Python's `value.rstrip("\n").split("\n")`.
fn split_lines(value: &str) -> Vec<String> {
    value.trim_end_matches('\n').split('\n').map(str::to_string).collect()
}

pub struct PlanService {
    pub plan_dir: PathBuf,
    pub plan_file: PathBuf,
    /// The revision counter: bumped on every successful op, so the agent
    /// can see which plan revision it acts on (the no="N" of the
    /// <plan no="N"> block in its input).
    pub plan_no_file: PathBuf,
}

impl PlanService {
    /// `scratch_root` is AutoCua/coder/scratchpad. The folder is made here.
    /// Python raises when it cannot be made; this prints the same error and
    /// goes on, and each later write then fails and says so.
    pub fn new(scratch_root: &Path, session_id: &str) -> PlanService {
        let plan_dir = if session_id.is_empty() {
            scratch_root.join("cli_plan")
        } else {
            scratch_root.join("cli_plan").join(session_id)
        };
        let plan_file = plan_dir.join("plan.md");
        let plan_no_file = plan_dir.join("plan_no.txt");
        if let Err(e) = std::fs::create_dir_all(&plan_dir) {
            log(&format!("Error creating plan directory: {e}"));
        }
        PlanService { plan_dir, plan_file, plan_no_file }
    }

    /// Apply a plan op. Returns (success, detail): the confirmation line on
    /// success or the reason on failure. Every successful op bumps the plan
    /// revision number.
    pub fn apply_op(&self, op: &str, value: &str, from_line: i64, to_line: i64) -> (bool, String) {
        let result = match op {
            "set" => self.set_plan(value),
            "add" => self.add_plan(value),
            "edit" => self.edit_plan(from_line, to_line, value),
            _ => return (false, format!("unknown op \"{op}\" — use \"set\", \"add\" or \"edit\"")),
        };
        if result.0 {
            self.bump_plan_no();
        }
        result
    }

    /// Current plan revision number (0 when no plan has been written yet,
    /// or the file cannot be read as a number).
    pub fn get_plan_no(&self) -> i64 {
        if !self.plan_no_file.exists() {
            return 0;
        }
        match crate::coder::read_text(&self.plan_no_file) {
            Ok(text) => crate::coder::py_strip(&text).parse().unwrap_or(0),
            Err(_) => 0,
        }
    }

    fn bump_plan_no(&self) {
        let next_no = self.get_plan_no().saturating_add(1);
        if let Err(e) = std::fs::write(&self.plan_no_file, next_no.to_string()) {
            log(&format!("Error bumping plan revision: {e}"));
        }
    }

    /// Current plan as its lines, or None when no plan exists yet (no file,
    /// or only blank text in it).
    fn read_lines(&self) -> Option<Vec<String>> {
        if !self.plan_file.exists() {
            return None;
        }
        match crate::coder::read_text(&self.plan_file) {
            Ok(content) if is_blank(&content) => None,
            Ok(content) => Some(split_lines(&content)),
            Err(e) => {
                log(&format!("Error reading plan: {e}"));
                None
            }
        }
    }

    fn write_lines(&self, lines: &[String]) -> std::io::Result<()> {
        std::fs::write(&self.plan_file, lines.join("\n") + "\n")
    }

    /// Overwrite the complete plan.
    pub fn set_plan(&self, value: &str) -> (bool, String) {
        if is_blank(value) {
            return (false, "empty plan — provide the plan text in value".to_string());
        }
        let lines = split_lines(value);
        match self.write_lines(&lines) {
            Ok(()) => (true, format!("plan set — {} lines", lines.len())),
            Err(e) => {
                log(&format!("Error setting plan: {e}"));
                (false, format!("could not write plan: {e}"))
            }
        }
    }

    /// Append value at the end of the plan.
    pub fn add_plan(&self, value: &str) -> (bool, String) {
        if is_blank(value) {
            return (false, "empty value — nothing to append".to_string());
        }
        let new_lines = split_lines(value);
        let mut lines = self.read_lines().unwrap_or_default();
        lines.extend(new_lines.iter().cloned());
        match self.write_lines(&lines) {
            Ok(()) => (true, format!("{} line(s) appended — plan now {} lines", new_lines.len(), lines.len())),
            Err(e) => {
                log(&format!("Error appending to plan: {e}"));
                (false, format!("could not append to plan: {e}"))
            }
        }
    }

    /// Replace plan lines from_line..to_line (inclusive, 1-indexed) with
    /// value. The value may hold more or fewer lines than the range it
    /// replaces; a blank value deletes the range.
    pub fn edit_plan(&self, from_line: i64, to_line: i64, value: &str) -> (bool, String) {
        let Some(lines) = self.read_lines() else {
            return (false, "no plan exists yet — write one with op \"set\" first".to_string());
        };
        let total = lines.len();
        if from_line < 1 || to_line < from_line {
            return (false, format!("invalid range {from_line}..{to_line} — need 1 <= from <= to"));
        }
        if to_line > i64::try_from(total).unwrap_or(i64::MAX) {
            return (
                false,
                format!(
                    "range {from_line}..{to_line} exceeds plan length ({total} lines) — re-check the latest <plan> line numbers"
                ),
            );
        }
        let new_lines = if is_blank(value) { Vec::new() } else { split_lines(value) };
        // 1 <= from <= to <= total here, so both cuts fall inside the plan.
        let head = usize::try_from(from_line - 1).unwrap_or(0);
        let tail = usize::try_from(to_line).unwrap_or(total);
        let mut edited: Vec<String> = lines.get(..head).unwrap_or_default().to_vec();
        edited.extend(new_lines.iter().cloned());
        edited.extend(lines.get(tail..).unwrap_or_default().iter().cloned());
        if edited.is_empty() {
            return (false, "edit would empty the plan — use op \"set\" to rewrite it instead".to_string());
        }
        match self.write_lines(&edited) {
            Ok(()) => (
                true,
                format!(
                    "lines {from_line}-{to_line} replaced with {} line(s) — plan now {} lines",
                    new_lines.len(),
                    edited.len()
                ),
            ),
            Err(e) => {
                log(&format!("Error editing plan: {e}"));
                (false, format!("could not edit plan: {e}"))
            }
        }
    }

    /// Raw plan text ("" when no plan exists yet).
    pub fn read_plan(&self) -> String {
        self.read_lines().map(|lines| lines.join("\n")).unwrap_or_default()
    }

    /// Numbered rendering for the <plan> input block: one `[N] text` per
    /// line, the `view` tool's numbering style, so the agent can use the
    /// numbers directly in `plan` edit ranges. "" when no plan yet.
    pub fn render_plan(&self) -> String {
        let Some(lines) = self.read_lines() else {
            return String::new();
        };
        lines
            .iter()
            .enumerate()
            .map(|(i, line)| format!("[{}] {line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
