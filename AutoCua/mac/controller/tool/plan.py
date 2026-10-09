import os
import logging

# Configure logger
logger = logging.getLogger(__name__)


class PlanService:
    """Session-scoped plan document store for the CLI agent.

    The plan is the agent's detailed, codebase-anchored route (written after
    exploration); the ToDo only tracks it. Stored as plan.md with the same
    isolation scheme as TaskTrackerService: cli_mode + session_id ->
    scratchpad/cli_plan/<session_id>/plan.md.

    Three ops (mirroring the `plan` tool in the coder system prompt):
      set  — overwrite the complete plan
      add  — append value at the end
      edit — replace lines from..to (inclusive, 1-indexed) with value

    Edit ranges are validated against the current file and fail loudly on
    drift, like `replace` does — line numbers come from the latest <plan>
    block rendered by render_plan().
    """

    def __init__(self, cli_mode: bool = False, session_id: str = None):
        """Initialize the Plan Service

        Args:
            cli_mode: If True, uses cli_plan folder for isolation from main agent
            session_id: Optional unique session ID for isolated plan folders (cli_mode only)
        """
        current_dir = os.path.dirname(os.path.abspath(__file__))
        # Go up two levels from AutoCua/mac/controller/tool/ to reach mac/
        mac_dir = os.path.dirname(os.path.dirname(current_dir))

        if cli_mode:
            if session_id:
                self.plan_dir = os.path.join(mac_dir, "scratchpad", "cli_plan", session_id)
            else:
                self.plan_dir = os.path.join(mac_dir, "scratchpad", "cli_plan")
        else:
            self.plan_dir = os.path.join(mac_dir, "scratchpad", "plan")

        self.plan_file = os.path.join(self.plan_dir, "plan.md")
        # Revision counter — bumped on every successful op so the agent can
        # see which plan revision it is acting on (the no="N" attr on the
        # <plan no="N"> block in its input).
        self.plan_no_file = os.path.join(self.plan_dir, "plan_no.txt")

        # Create plan directory if it doesn't exist
        self._ensure_plan_directory()

    def _ensure_plan_directory(self):
        """Create plan directory if it doesn't exist"""
        try:
            os.makedirs(self.plan_dir, exist_ok=True)
        except Exception as e:
            logger.error(f"Error creating plan directory: {str(e)}")
            raise

    def apply_op(self, op, value, from_line=0, to_line=0):
        """Apply a plan op. Returns (success, detail) — detail is the
        confirmation line on success or the reason on failure. Every
        successful op bumps the plan revision number."""
        if op == "set":
            result = self.set_plan(value)
        elif op == "add":
            result = self.add_plan(value)
        elif op == "edit":
            result = self.edit_plan(from_line, to_line, value)
        else:
            return (False, f'unknown op "{op}" — use "set", "add" or "edit"')
        if result[0]:
            self._bump_plan_no()
        return result

    def get_plan_no(self):
        """Current plan revision number (0 when no plan has been written yet)"""
        try:
            if not os.path.exists(self.plan_no_file):
                return 0
            with open(self.plan_no_file, "r", encoding="utf-8") as f:
                return int(f.read().strip())
        except Exception:
            return 0

    def _bump_plan_no(self):
        try:
            next_no = self.get_plan_no() + 1
            with open(self.plan_no_file, "w", encoding="utf-8") as f:
                f.write(str(next_no))
        except Exception as e:
            logger.error(f"Error bumping plan revision: {str(e)}")

    def _read_lines(self):
        """Current plan as a list of lines, or None when no plan exists yet"""
        try:
            if not os.path.exists(self.plan_file):
                return None
            with open(self.plan_file, "r", encoding="utf-8") as f:
                content = f.read()
            if not content.strip():
                return None
            return content.rstrip("\n").split("\n")
        except Exception as e:
            logger.error(f"Error reading plan: {str(e)}")
            return None

    def _write_lines(self, lines):
        with open(self.plan_file, "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")

    def set_plan(self, value):
        """Overwrite the complete plan"""
        try:
            if not value or not value.strip():
                return (False, "empty plan — provide the plan text in value")
            lines = value.rstrip("\n").split("\n")
            self._write_lines(lines)
            return (True, f"plan set — {len(lines)} lines")
        except Exception as e:
            logger.error(f"Error setting plan: {str(e)}")
            return (False, f"could not write plan: {str(e)}")

    def add_plan(self, value):
        """Append value at the end of the plan"""
        try:
            if not value or not value.strip():
                return (False, "empty value — nothing to append")
            existing = self._read_lines() or []
            new_lines = value.rstrip("\n").split("\n")
            lines = existing + new_lines
            self._write_lines(lines)
            return (True, f"{len(new_lines)} line(s) appended — plan now {len(lines)} lines")
        except Exception as e:
            logger.error(f"Error appending to plan: {str(e)}")
            return (False, f"could not append to plan: {str(e)}")

    def edit_plan(self, from_line, to_line, value):
        """Replace plan lines from_line..to_line (inclusive, 1-indexed) with value.

        value may contain more or fewer lines than the range it replaces;
        an empty value deletes the range."""
        try:
            lines = self._read_lines()
            if lines is None:
                return (False, 'no plan exists yet — write one with op "set" first')
            total = len(lines)
            try:
                from_line, to_line = int(from_line), int(to_line)
            except (ValueError, TypeError):
                return (False, "from/to must be integers")
            if from_line < 1 or to_line < from_line:
                return (False, f"invalid range {from_line}..{to_line} — need 1 <= from <= to")
            if to_line > total:
                return (False, f"range {from_line}..{to_line} exceeds plan length ({total} lines) — re-check the latest <plan> line numbers")
            new_lines = value.rstrip("\n").split("\n") if value and value.strip() else []
            lines = lines[:from_line - 1] + new_lines + lines[to_line:]
            if not lines:
                return (False, 'edit would empty the plan — use op "set" to rewrite it instead')
            self._write_lines(lines)
            return (True, f"lines {from_line}-{to_line} replaced with {len(new_lines)} line(s) — plan now {len(lines)} lines")
        except Exception as e:
            logger.error(f"Error editing plan: {str(e)}")
            return (False, f"could not edit plan: {str(e)}")

    def read_plan(self):
        """Raw plan text ('' when no plan exists yet)"""
        lines = self._read_lines()
        return "\n".join(lines) if lines else ""

    def render_plan(self):
        """Numbered rendering for the <plan> input block: one `[N] text` per
        line, matching the `view` tool's numbering style so the agent can use
        the numbers directly in `plan` edit ranges. '' when no plan yet."""
        lines = self._read_lines()
        if lines is None:
            return ""
        return "\n".join(f"[{i + 1}] {line}" for i, line in enumerate(lines))
