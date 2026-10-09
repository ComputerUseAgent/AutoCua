import copy
import json
import logging
import re
from pathlib import Path
from typing import NamedTuple, Optional

from ...sandbox import Sandbox

logger = logging.getLogger(__name__)


# ═════════════════════════════════════════════════════════════════════════════
# Shared tool-output rendering — the CODER and MINION loops both run every
# routed action_result through render_tool_response(). It lives HERE, next to
# the tool implementations, so the envelope format and the tools evolve
# together. The envelope:
#
#   <Tool_response>
#   { ...the action_result as strictly-valid JSON, minus its "output"... }
#   <output lines="A-B of TOTAL">
#   ...the output VERBATIM: real newlines, real indentation, view's [N]
#   numbers, grep's path:line prefixes...
#   </output>
#   </Tool_response>
#
# Rationale: json.dumps used to swallow the whole output into ONE escaped
# string ("\n" literals, no indentation) — unreadable for the model and a
# hallucination risk. Metadata stays JSON (machine-parseable, keeps the JSON
# discipline the model must follow in its own actions); content is raw.
# Outputs are clipped per tool (clip_output) with a steering footer when
# truncated, so a single huge command can never explode agent memory again.
# ═════════════════════════════════════════════════════════════════════════════

class ClipResult(NamedTuple):
    text: str          # clipped text (elision marker included) — NO footer
    total_lines: int
    ranges: tuple      # ((1, 200),) or ((1, 150), (1401, 1450)) — 1-based, inclusive
    truncated: bool


_CLIP_MAX_LINES = 200         # default line budget per tool output
_CLIP_MAX_CHARS = 15_000      # char safety net (whichever bites first)
_CLIP_MAX_LINE_CHARS = 2_000  # single-line hard cut (minified files, base64…)


def clip_output(text, max_lines: Optional[int] = _CLIP_MAX_LINES,
                max_chars: int = _CLIP_MAX_CHARS, tail_lines: int = 0) -> ClipResult:
    """Bound one tool output for agent memory (Claude-Code-style).

    1. Any single line over _CLIP_MAX_LINE_CHARS is hard-cut with a
       " [+N chars]" marker — one minified line must never cost megabytes.
    2. Line budget: keep the first (max_lines - tail_lines) lines plus the
       LAST tail_lines lines (tail_lines > 0 = shell mode: tracebacks and
       verdicts live at the end), with an elision marker between segments.
       max_lines=None disables the line budget (view manages its own ranges).
    3. Char budget: drop whole lines from the END of the head segment until
       under max_chars — the tail segment always survives.
    Clipping is always at line boundaries."""
    lines = str(text).splitlines() or [""]
    total = len(lines)
    truncated = False

    clipped = []
    for ln in lines:
        if len(ln) > _CLIP_MAX_LINE_CHARS:
            clipped.append(ln[:_CLIP_MAX_LINE_CHARS] + f" [+{len(ln) - _CLIP_MAX_LINE_CHARS} chars]")
            truncated = True
        else:
            clipped.append(ln)
    lines = clipped

    if max_lines is not None and total > max_lines:
        truncated = True
        tail_n = min(tail_lines, max_lines - 1) if tail_lines else 0
        head_n = max_lines - tail_n
        head_seg = lines[:head_n]
        tail_seg = lines[total - tail_n:] if tail_n else []
    else:
        tail_n = 0
        head_seg = list(lines)
        tail_seg = []

    def _chars() -> int:
        return sum(len(l) + 1 for l in head_seg) + sum(len(l) + 1 for l in tail_seg)

    while head_seg and _chars() > max_chars:
        head_seg.pop()
        truncated = True
    head_n = len(head_seg)

    if tail_n:
        elided = total - head_n - tail_n
        body = head_seg + [f"... [{elided} lines elided]"] + tail_seg
        ranges = ((1, head_n), (total - tail_n + 1, total))
    else:
        body = head_seg
        ranges = ((1, head_n),) if head_n else ((0, 0),)
    return ClipResult("\n".join(body), total, ranges, truncated)


# Per-tool clip policy: `view` is the deliberate range-driven reader (its own
# 2000-line/start-end mechanics and real [N] numbers) so it keeps every line
# but stays under the char guard; `shell` keeps a TAIL because build/test
# output puts the verdict at the end; everything else takes the plain head cap.
_CLIP_POLICY = {
    "view": {"max_lines": None, "tail_lines": 0},
    "shell": {"max_lines": _CLIP_MAX_LINES, "tail_lines": 50},
}
_DEFAULT_POLICY = {"max_lines": _CLIP_MAX_LINES, "tail_lines": 0}

_CLIP_FOOTERS = {
    "view": "... [clipped at ~15k chars — continue with a ranged view starting just after the last [N] shown]",
    "shell": "... [long output clipped — re-run with filters (| head, | tail, | grep) or write to a file and grep/view it]",
}
_DEFAULT_FOOTER = "... [showing a bounded slice — narrow with grep, a view start/end range, or a tighter pattern]"


def _ranges_attr(ranges, total) -> str:
    spans = ",".join(f"{a}-{b}" for a, b in ranges if b >= a and b > 0)
    return f'{spans or "0-0"} of {total}'


def _render_block(entry_action, output_text, result_no=None) -> str:
    policy = _CLIP_POLICY.get(entry_action, _DEFAULT_POLICY)
    clip = clip_output(output_text, max_lines=policy["max_lines"], tail_lines=policy["tail_lines"])
    body = clip.text
    if clip.truncated:
        footer = _CLIP_FOOTERS.get(entry_action, _DEFAULT_FOOTER)
        body = f"{body}\n{footer}" if body else footer
    lines_attr = _ranges_attr(clip.ranges, clip.total_lines)
    if result_no is not None:
        # Numbered open AND close (same convention as <user_request=N>):
        # unambiguous pairing, and a raw output line that happens to contain a
        # plain "</output>" can never be mistaken for this block's end.
        return (f'<output result="{result_no}" lines="{lines_attr}">\n{body}\n'
                f'</output result="{result_no}">')
    return f'<output lines="{lines_attr}">\n{body}\n</output>'


def render_tool_response(action_result) -> str:
    """Render one routed action_result as the <Tool_response> envelope (see the
    format note above). Entries with no `output` key (web-compacted pointers,
    plan_updated, scratchpad_added, ...) render as head only.

    A `multiple` result is INTERLEAVED for locality: a minimal summary head
    ({status, action: "multiple", results: N}), then per result its own JSON
    head immediately followed by its own numbered block —
    <output result="k" ...> ... </output result="k"> — so every output sits
    directly under the command that produced it."""
    if not isinstance(action_result, dict):
        return f"<Tool_response>\n{json.dumps(action_result, indent=2, ensure_ascii=False)}\n</Tool_response>"

    res = copy.deepcopy(action_result)
    parts = []

    if res.get("action") == "multiple" and isinstance(res.get("results"), list):
        entries = res.pop("results")
        res["results"] = len(entries)          # summary head carries the count
        parts.append(json.dumps(res, indent=2, ensure_ascii=False))
        for i, entry in enumerate(entries, 1):
            if isinstance(entry, dict):
                out_text = entry.pop("output", None)
                parts.append(json.dumps(entry, indent=2, ensure_ascii=False))
                if out_text is not None:
                    parts.append(_render_block(entry.get("action", ""), out_text, i))
            else:
                parts.append(json.dumps(entry, indent=2, ensure_ascii=False))
    else:
        out_text = res.pop("output", None) if "output" in res else None
        parts.append(json.dumps(res, indent=2, ensure_ascii=False))
        if out_text is not None:
            parts.append(_render_block(res.get("action", ""), out_text))

    return "<Tool_response>\n" + "\n".join(parts) + "\n</Tool_response>"


class CLIService:
    """Service for CLI agent actions - executes commands via sandbox"""
    
    def __init__(self, session_id: str = None):
        """Initialize CLI Service with sandbox connection
        
        Args:
            session_id: Optional unique session ID for isolated sandbox workspace
        """
        self.sandbox = Sandbox(session_id=session_id)
    
    def write(self, path: str, line: int, content: str) -> dict:
        """
        Write content into a file at a specific line.
        If file doesn't exist, creates it. Existing lines from the insertion
        point onward are shifted down.
        
        Args:
            path: File path (relative to sandbox)
            line: Line number to insert at (1-indexed)
            content: Content to write (can be multi-line with \n)
            
        Returns:
            dict: Formatted response for agent with last_line
        """
        command_str = f'write path="{path}", line={line}'
        
        try:
            full_path = Path(self.sandbox.working_dir) / path
            
            # Create parent directory if needed
            full_path.parent.mkdir(parents=True, exist_ok=True)
            
            # Read existing content if file exists
            existing_lines = []
            if full_path.exists():
                raw_content = full_path.read_text(encoding='utf-8')
                if raw_content.strip():
                    existing_lines = raw_content.rstrip('\n').split('\n')
            
            # Split new content into lines
            new_lines = content.split('\n')
            # Remove trailing empty string from split if content ends with \n
            if new_lines and new_lines[-1] == '':
                new_lines.pop()
            
            # Insert new lines at the specified position
            insert_index = max(0, line - 1)  # Convert to 0-indexed
            for i, new_line in enumerate(new_lines):
                existing_lines.insert(insert_index + i, new_line)
            
            # Write back
            final_content = '\n'.join(existing_lines)
            with open(full_path, 'w', encoding='utf-8') as f:
                f.write(final_content)
            
            # Verify and get final line count
            if full_path.exists():
                verify_content = full_path.read_text(encoding='utf-8')
                final_lines = verify_content.rstrip('\n').split('\n') if verify_content.strip() else ['']
                last_line = len(final_lines) + 1  # +1 for the extra blank line (matches view behavior)
                
                return {
                    "status": "success",
                    "action": "write",
                    "command": command_str,
                    "output": f"Written {len(final_lines)} lines at line {line}. Last empty line: {last_line}",
                    "last_line": last_line
                }
            else:
                return {
                    "status": "failed",
                    "action": "write",
                    "command": command_str,
                    "output": "Write failed - file does not exist after write"
                }
                
        except Exception as e:
            return {
                "status": "failed",
                "action": "write",
                "command": command_str,
                "output": str(e)
            }
    
    def shell(self, command: str, input_text: str = None) -> dict:
        """
        Execute a shell command in the sandbox
        
        Args:
            command: shell command to execute
            input_text: Optional input to pipe to stdin (for interactive programs)
            
        Returns:
            dict: Result with cwd, command, output (only if present)
            Status can be: "success", "error", "timeout", "input_required"
        """
        try:
            result = self.sandbox.run(command, input_text)
            
            # Handle input_required error specially
            if result.get("error") == "input_required":
                last_output = result.get("last_output", "")
                output = ""
                if result.get("stdout"):
                    output += result["stdout"]
                if result.get("stderr"):
                    output += result["stderr"]
                
                response = {
                    "status": "input_required",
                    "action": "shell",
                    "cwd": self.sandbox.get_cwd(),
                    "command": command,
                    "message": f"Process waiting for input. Last output: '{last_output}'. Use input parameter with shell command."
                }
                
                if output.strip():
                    response["output"] = output.strip()
                
                return response
            
            # Handle timeout specially
            if result.get("timeout"):
                last_output = result.get("last_output", "")
                output = ""
                if result.get("stdout"):
                    output += result["stdout"]
                if result.get("stderr"):
                    output += result["stderr"]
                
                response = {
                    "status": "timeout",
                    "action": "shell",
                    "cwd": self.sandbox.get_cwd(),
                    "command": command,
                    "message": result.get("message", "Command timed out (may need input - use input parameter if program requires user input)")
                }
                
                if output.strip():
                    response["output"] = output.strip()
                
                return response
            
            output = ""
            if result.get("stdout"):
                output += result["stdout"]
            if result.get("stderr"):
                output += result["stderr"]

            returncode = result.get("returncode")
            succeeded = result.get("success")

            response = {
                "status": "success" if succeeded else "error",
                "action": "shell",
                "cwd": self.sandbox.get_cwd(),
                "command": command
            }

            # Always surface the exit code so the agent can tell a real error
            # from an empty result (e.g. grep exits 1 on no match).
            if returncode is not None:
                response["exit_code"] = returncode

            # Show exactly what the command returned (verbatim stdout + stderr).
            # When the terminal returns absolutely nothing, show a clear marker
            # instead of a blank string — the status/exit_code already say whether
            # it succeeded, so this just makes "no output" unambiguous (like a
            # real terminal / Claude Code).
            clean_output = output.strip()
            response["output"] = clean_output if clean_output else "(no output)"

            # Only include error if there's actual content
            error = result.get("error", "")
            if error:
                response["error"] = error

            return response
            
        except Exception as e:
            logger.error(f"Shell execution error: {str(e)}")
            return {
                "status": "error",
                "action": "shell",
                "cwd": self.sandbox.get_cwd(),
                "command": command,
                "error": str(e)
            }
    
    def replace(self, path: str, line: int, old_block: str, new_block: str) -> dict:
        """
        Replace a block of lines in a file starting at a specific line.
        
        Reads N lines from `line` downward (where N = lines in old_block),
        verifies exact match, then swaps in new_block (any number of lines).
        
        Args:
            path: File path (relative to sandbox)
            line: Starting line number (1-indexed)
            old_block: Expected block at that position (multi-line with \\n)
            new_block: New block to replace with (multi-line with \\n)
            
        Returns:
            dict: Formatted response for agent with last_line
        """
        command_str = f'replace path="{path}", line={line}'
        
        try:
            full_path = Path(self.sandbox.working_dir) / path

            if not full_path.exists():
                return {
                    "status": "failed",
                    "action": "replace",
                    "command": command_str,
                    "output": "File not found"
                }

            raw_content = full_path.read_text(encoding='utf-8')
            file_lines = raw_content.rstrip('\n').split('\n')
            
            # Split old_block into lines
            old_lines = old_block.split('\n')
            if old_lines and old_lines[-1] == '':
                old_lines.pop()
            old_count = len(old_lines)
            
            # Range check
            if line < 1 or line > len(file_lines):
                return {
                    "status": "failed",
                    "action": "replace",
                    "command": command_str,
                    "output": f"line {line} out of range (file has {len(file_lines)} lines)"
                }
            
            end_line = line - 1 + old_count
            if end_line > len(file_lines):
                return {
                    "status": "failed",
                    "action": "replace",
                    "command": command_str,
                    "output": f"old_block has {old_count} lines but only {len(file_lines) - (line - 1)} lines remain from line {line}"
                }
            
            # Extract actual block and compare
            actual_lines = file_lines[line - 1 : end_line]
            
            if actual_lines != old_lines:
                for i, (actual, expected) in enumerate(zip(actual_lines, old_lines)):
                    if actual != expected:
                        mismatch_line = line + i
                        return {
                            "status": "failed",
                            "action": "replace",
                            "command": command_str,
                            "output": f'mismatch at line {mismatch_line}: found "{actual}" expected "{expected}"'
                        }
                return {
                    "status": "failed",
                    "action": "replace",
                    "command": command_str,
                    "output": f"block length mismatch: file has {len(actual_lines)} lines, old_block has {old_count} lines"
                }
            
            # REPLACE: Swap old block with new block
            new_lines = new_block.split('\n')
            if new_lines and new_lines[-1] == '':
                new_lines.pop()
            
            file_lines[line - 1 : end_line] = new_lines
            new_content = '\n'.join(file_lines)
            
            try:
                with open(full_path, 'w', encoding='utf-8') as f:
                    f.write(new_content)
            except Exception as write_error:
                return {
                    "status": "failed",
                    "action": "replace",
                    "command": command_str,
                    "output": str(write_error)
                }
            
            # POST-CHECK: Verify new_block is at that position
            verify_content = full_path.read_text(encoding='utf-8')
            verify_lines = verify_content.rstrip('\n').split('\n')
            new_count = len(new_lines)
            
            verify_slice = verify_lines[line - 1 : line - 1 + new_count]
            if verify_slice == new_lines:
                last_line = len(verify_lines) + 1
                return {
                    "status": "success",
                    "action": "replace",
                    "command": command_str,
                    "output": f"replaced {old_count} lines with {new_count} lines at line {line}. Last empty line: {last_line}",
                    "last_line": last_line
                }
            
            return {
                "status": "failed",
                "action": "replace",
                "command": command_str,
                "output": "replace verification failed after write"
            }
            
        except Exception as e:
            return {
                "status": "failed",
                "action": "replace",
                "command": command_str,
                "output": str(e)
            }
    
    # Whole-file `view` cap — files larger than this in lines are clipped with
    # a footer telling the agent how to fetch the rest via start/end.
    _VIEW_DEFAULT_LINE_CAP = 2000
    # Hard refusal threshold: don't even try to read files this large.
    _VIEW_FILE_SIZE_LIMIT = 5 * 1024 * 1024  # 5 MB

    def view(self, path: str, start: int = 0, end: int = 0) -> dict:
        """
        View file contents with line numbers, optionally restricted to a range.

        - `start == 0 and end == 0`: whole-file mode. If the file has more than
          _VIEW_DEFAULT_LINE_CAP lines, only the first cap-many lines are
          returned plus a footer instructing the agent to use start/end for
          the rest.
        - `start > 0 and end >= start`: returns lines [start..end] (inclusive,
          1-indexed), with the original file line numbers preserved in the
          output (e.g. `[400]`, not `[1]`) so write/replace can use them
          directly.

        Files larger than _VIEW_FILE_SIZE_LIMIT are refused — the agent should
        use grep with head_limit instead.

        Path resolution matches grep/glob: relative paths anchor at the sandbox
        cwd, absolute paths and `~` are honored.
        """
        command_str = f'view path="{path}", start={start}, end={end}'

        try:
            full_path = self._resolve_search_base(path) if path else Path(self.sandbox.working_dir)

            # Non-existent file: keep the original "empty stub" behavior so the
            # agent can `write` to a fresh path without a separate touch step.
            if not full_path.exists():
                return {
                    "status": "success",
                    "action": "view",
                    "command": command_str,
                    "output": "[1] ",
                    "last_line": 1,
                }

            if not full_path.is_file():
                return {
                    "status": "failed",
                    "action": "view",
                    "command": command_str,
                    "output": f"path is not a file: {path}",
                }

            # Hard size guard — refuse before any read.
            try:
                size_bytes = full_path.stat().st_size
            except OSError as e:
                return {
                    "status": "failed",
                    "action": "view",
                    "command": command_str,
                    "output": f"could not stat file: {e}",
                }
            if size_bytes > self._VIEW_FILE_SIZE_LIMIT:
                size_mb = size_bytes / (1024 * 1024)
                return {
                    "status": "failed",
                    "action": "view",
                    "command": command_str,
                    "output": (
                        f"file too large ({size_mb:.1f} MB, limit "
                        f"{self._VIEW_FILE_SIZE_LIMIT // (1024 * 1024)} MB) — "
                        f"use grep with head_limit, or specify a narrow start/end range"
                    ),
                }

            # Read + tokenize.
            raw_content = full_path.read_text(encoding="utf-8", errors="replace")

            if not raw_content.strip():
                return {
                    "status": "success",
                    "action": "view",
                    "command": command_str,
                    "output": "[1] ",
                    "last_line": 1,
                }

            # File body lines (no trailing extra). The "+1 blank line at the
            # end" marker is appended at the END of the output so the agent
            # sees a valid append target line number — preserving the existing
            # write contract.
            file_lines = raw_content.rstrip("\n").split("\n")
            file_total = len(file_lines)
            # last_line points at the file's true append target, regardless of
            # whether the agent viewed a slice or the whole file.
            last_line = file_total + 1

            # Validate range parameters before slicing.
            if start < 0 or end < 0:
                return {
                    "status": "failed",
                    "action": "view",
                    "command": command_str,
                    "output": "start and end must be >= 0",
                }
            if (start == 0) ^ (end == 0):
                return {
                    "status": "failed",
                    "action": "view",
                    "command": command_str,
                    "output": "pass both start and end (or neither, with 0,0 for whole-file)",
                }

            ranged = start > 0 and end > 0
            footer = ""

            if ranged:
                if start > end:
                    return {
                        "status": "failed",
                        "action": "view",
                        "command": command_str,
                        "output": f"start ({start}) > end ({end})",
                    }
                if start > file_total:
                    return {
                        "status": "failed",
                        "action": "view",
                        "command": command_str,
                        "output": f"start {start} exceeds file length ({file_total})",
                    }
                # Clamp end to file length silently — partial overlap is fine.
                clamped_end = min(end, file_total)
                slice_lines = file_lines[start - 1 : clamped_end]
                # Preserve real line numbers in the output.
                indexed_lines = [
                    f"[{start + i}] {line}" for i, line in enumerate(slice_lines)
                ]
                indexed_lines.append(f"[{last_line}] ")  # append target marker
                if clamped_end < end:
                    footer = f"\n... range end clamped to file length ({file_total})."
                elif clamped_end < file_total:
                    footer = f"\n... showing lines {start}-{clamped_end} of {file_total}."
            else:
                # Whole-file mode with soft cap.
                if file_total > self._VIEW_DEFAULT_LINE_CAP:
                    cap = self._VIEW_DEFAULT_LINE_CAP
                    slice_lines = file_lines[:cap]
                    indexed_lines = [
                        f"[{i + 1}] {line}" for i, line in enumerate(slice_lines)
                    ]
                    footer = (
                        f"\n... file has {file_total} lines (showing 1-{cap}). "
                        f"Use start/end to view more."
                    )
                else:
                    indexed_lines = [
                        f"[{i + 1}] {line}" for i, line in enumerate(file_lines)
                    ]
                    indexed_lines.append(f"[{last_line}] ")  # append target marker

            output = "\n".join(indexed_lines) + footer

            return {
                "status": "success",
                "action": "view",
                "command": command_str,
                "output": output,
                "last_line": last_line,
            }

        except Exception as e:
            return {
                "status": "failed",
                "action": "view",
                "command": command_str,
                "output": str(e),
            }

    # Skip files larger than this when grepping (8 MB) — minified bundles, lockfiles
    _GREP_FILE_SIZE_LIMIT = 8 * 1024 * 1024
    # Cap each match line so a single minified file can't drown the result list
    _GREP_LINE_TRUNCATE = 200
    # Totals keep counting after the emit budget fills (for the "N matches in M
    # files" footer) — but never beyond this, so grep can't walk forever just
    # to report a precise count. Rendered as "10000+" when hit.
    _GREP_TOTALS_CAP = 10_000
    # First-block sniff for binary detection
    _BINARY_SNIFF_BYTES = 8192
    # Directory names that are virtually never what the agent wants to search.
    # Match by basename anywhere in the path — applies to grep AND glob.
    _SKIP_DIRS = frozenset({
        ".git", ".hg", ".svn",
        "venv", ".venv", "env", ".env",
        "node_modules",
        "__pycache__", ".mypy_cache", ".pytest_cache", ".ruff_cache", ".tox",
        "dist", "build", "out", "target",
        ".next", ".nuxt", ".turbo", ".cache",
        "site-packages",
        ".idea", ".vscode",
        ".DS_Store",
    })

    @staticmethod
    def _looks_binary(data: bytes) -> bool:
        """Cheap binary sniff: a NUL byte in the first chunk strongly implies binary."""
        return b"\x00" in data

    def _clip_line(self, ln: str) -> str:
        """Clamp one grep emit line, with an explicit marker (was a silent cut)."""
        if len(ln) > self._GREP_LINE_TRUNCATE:
            return ln[: self._GREP_LINE_TRUNCATE] + f" [+{len(ln) - self._GREP_LINE_TRUNCATE} chars]"
        return ln

    @classmethod
    def _is_in_skip_dir(cls, path: Path, base: Path) -> bool:
        """True if any path segment between base and path is in _SKIP_DIRS."""
        try:
            rel = path.relative_to(base)
        except ValueError:
            return False
        return any(part in cls._SKIP_DIRS for part in rel.parts)

    # Defensive cap on total entries glob walks before bailing — guards against
    # a runaway pattern (e.g. broad "**/*" on a huge tree) iterating forever
    # before head_limit cuts in. Skip-dirs already remove most noise.
    _GLOB_WALK_CAP = 20_000

    def _resolve_search_base(self, path: str) -> Path:
        """Resolve the search base for grep/glob.

        - Empty path → sandbox working_dir (the agent's home base).
        - Absolute path → that path, expanded (`~` honored).
        - Relative path → resolved against sandbox working_dir.

        The agent owns its target directory. We don't fence it — we only stop
        runaway output (skip-dirs, head_limit, walk cap, file-size cap, line
        truncation) regardless of where it points.
        """
        if not path:
            return Path(self.sandbox.working_dir).resolve()
        candidate = Path(path).expanduser()
        if not candidate.is_absolute():
            candidate = Path(self.sandbox.working_dir) / candidate
        return candidate.resolve()

    def grep(
        self,
        pattern: str,
        path: str = "",
        glob_filter: str = "",
        output_mode: str = "content",
        case_insensitive: bool = False,
        head_limit: int = 50,
        context: int = 0,
    ) -> dict:
        """
        Regex search across files. Three output modes:
          - "content" (default): "path:line: matching_text", with optional ±context lines.
          - "files_with_matches": one path per line, files that contain at least one match.
          - "count": "path: N" per file, only files with N >= 1.

        Skips binary files (NUL byte sniff), files > 8 MB, and undecodable bytes.
        Each emitted line is truncated to 200 chars to keep context bounded.
        """
        command_str = (
            f'grep pattern="{pattern}", path="{path}", glob="{glob_filter}", '
            f'mode="{output_mode}", ci={case_insensitive}, head={head_limit}, ctx={context}'
        )

        # Compile regex up front so bad patterns fail cleanly, not mid-walk.
        try:
            flags = re.IGNORECASE if case_insensitive else 0
            regex = re.compile(pattern, flags)
        except re.error as e:
            return {
                "status": "failed",
                "action": "grep",
                "command": command_str,
                "output": f"invalid regex: {e}",
            }

        if output_mode not in ("content", "files_with_matches", "count"):
            return {
                "status": "failed",
                "action": "grep",
                "command": command_str,
                "output": f'invalid output_mode "{output_mode}" (use content / files_with_matches / count)',
            }

        try:
            base = self._resolve_search_base(path)
            if not base.exists():
                return {
                    "status": "failed",
                    "action": "grep",
                    "command": command_str,
                    "output": f"path not found: {path or '.'}",
                }

            # Resolve search root: file → just that file; dir → walk recursively.
            if base.is_file():
                candidates = [base]
                skip_root = base.parent
            else:
                # rglob with "*" walks every entry; the glob_filter pattern (when given)
                # is applied as an additional fnmatch filter on the file name.
                candidates = base.rglob(glob_filter or "*")
                skip_root = base

            content_lines: list[str] = []
            file_match_paths: list[str] = []
            file_match_counts: list[tuple[str, int]] = []
            # Totals keep counting AFTER the emit budget fills (without
            # emitting), so the footer can report the true scope of the match
            # set — capped at _GREP_TOTALS_CAP so grep never walks forever
            # just to produce a precise number.
            total_matches = 0
            matched_files: set[str] = set()
            budget_hit = False

            for fp in candidates:
                if not fp.is_file():
                    continue
                # Skip noise directories (venv, .git, node_modules, __pycache__, etc.)
                # — checked relative to the agent-specified base, not the sandbox.
                if self._is_in_skip_dir(fp, skip_root):
                    continue
                try:
                    if fp.stat().st_size > self._GREP_FILE_SIZE_LIMIT:
                        continue
                except OSError:
                    continue

                # Binary sniff on the first chunk before reading the whole file.
                try:
                    with open(fp, "rb") as bf:
                        head_bytes = bf.read(self._BINARY_SNIFF_BYTES)
                    if self._looks_binary(head_bytes):
                        continue
                except OSError:
                    continue

                try:
                    text = fp.read_text(encoding="utf-8", errors="replace")
                except OSError:
                    continue

                lines = text.splitlines()
                # Emit paths relative to the agent's specified search base
                # (not the sandbox) so output is readable and doesn't leak the
                # full host layout. Falls back to absolute path if for some
                # reason relativization fails (symlinks, etc.).
                try:
                    rel = str(fp.resolve().relative_to(base))
                except ValueError:
                    rel = str(fp)

                if output_mode == "count":
                    n = sum(1 for ln in lines if regex.search(ln))
                    if n > 0:
                        total_matches += n
                        matched_files.add(rel)
                        if len(file_match_counts) < head_limit:
                            file_match_counts.append((rel, n))
                        else:
                            budget_hit = True
                    if total_matches >= self._GREP_TOTALS_CAP:
                        break
                    continue

                if output_mode == "files_with_matches":
                    if any(regex.search(ln) for ln in lines):
                        matched_files.add(rel)
                        if len(file_match_paths) < head_limit:
                            file_match_paths.append(rel)
                        else:
                            budget_hit = True
                    if len(matched_files) >= self._GREP_TOTALS_CAP:
                        break
                    continue

                # content mode — head_limit bounds EMITTED LINES (context lines
                # and "--" separators included), not matches: context=2 with
                # head=50 used to emit ~250 lines. A match's context block is
                # emitted atomically, so the overshoot is bounded by one block
                # (≤ 2*context + 1 lines).
                file_emitted_block = False
                for idx, ln in enumerate(lines):
                    if not regex.search(ln):
                        continue
                    total_matches += 1
                    matched_files.add(rel)

                    if len(content_lines) >= head_limit:
                        budget_hit = True
                        if total_matches >= self._GREP_TOTALS_CAP:
                            break
                        continue          # budget spent: keep counting, stop emitting

                    if context > 0:
                        if file_emitted_block:
                            content_lines.append("--")
                        start = max(0, idx - context)
                        end = min(len(lines), idx + context + 1)
                        for j in range(start, end):
                            marker = ":" if j == idx else "-"
                            content_lines.append(f"{rel}:{j + 1}{marker} {self._clip_line(lines[j])}")
                        file_emitted_block = True
                    else:
                        content_lines.append(f"{rel}:{idx + 1}: {self._clip_line(ln)}")

                if total_matches >= self._GREP_TOTALS_CAP:
                    break

            # Totals footer — only when the emit budget was actually exceeded.
            totals_capped = (total_matches >= self._GREP_TOTALS_CAP
                             or len(matched_files) >= self._GREP_TOTALS_CAP)

            def _fmt(n: int) -> str:
                return f"{self._GREP_TOTALS_CAP}+" if totals_capped else str(n)

            if output_mode == "content":
                output = "\n".join(content_lines) if content_lines else "no matches"
                if budget_hit and content_lines:
                    output += (
                        f"\n\n({_fmt(total_matches)} matches in {_fmt(len(matched_files))} files "
                        f"— showing first {len(content_lines)} lines; refine pattern/path/glob or raise head_limit)"
                    )
            elif output_mode == "files_with_matches":
                output = "\n".join(file_match_paths) if file_match_paths else "no matches"
                if budget_hit and file_match_paths:
                    output += f"\n\n({_fmt(len(matched_files))} files with matches — showing first {len(file_match_paths)})"
            else:  # count
                output = (
                    "\n".join(f"{p}: {n}" for p, n in file_match_counts)
                    if file_match_counts
                    else "no matches"
                )
                if budget_hit and file_match_counts:
                    output += (
                        f"\n\n({_fmt(len(matched_files))} files with matches, {_fmt(total_matches)} total matches "
                        f"— showing first {len(file_match_counts)} files)"
                    )

            return {
                "status": "success",
                "action": "grep",
                "command": command_str,
                "output": output,
            }

        except Exception as e:
            return {
                "status": "failed",
                "action": "grep",
                "command": command_str,
                "output": str(e),
            }

    def glob(self, pattern: str, path: str = "", head_limit: int = 100) -> dict:
        """
        File pattern matching (e.g. "**/*.py", "src/**/*.tsx"). Returns matched paths
        sorted by mtime descending so recently-edited files float to the top.
        """
        command_str = f'glob pattern="{pattern}", path="{path}", head={head_limit}'

        try:
            base = self._resolve_search_base(path)
            if not base.exists():
                return {
                    "status": "failed",
                    "action": "glob",
                    "command": command_str,
                    "output": f"path not found: {path or '.'}",
                }

            # Walk with an early bail at _GLOB_WALK_CAP — defends against an
            # over-broad pattern iterating millions of entries before mtime sort.
            try:
                matches: list[Path] = []
                walked = 0
                walk_cap_hit = False
                for p in base.glob(pattern):
                    walked += 1
                    if walked > self._GLOB_WALK_CAP:
                        walk_cap_hit = True
                        break
                    if p.is_file() and not self._is_in_skip_dir(p, base):
                        matches.append(p)
            except (ValueError, OSError) as e:
                return {
                    "status": "failed",
                    "action": "glob",
                    "command": command_str,
                    "output": f"glob error: {e}",
                }

            # Newest first — most likely the file the agent wants to look at.
            matches.sort(key=lambda p: p.stat().st_mtime, reverse=True)
            total_matched = len(matches)
            matches = matches[:head_limit]

            # Emit paths relative to the agent-specified base for readability.
            rels: list[str] = []
            for p in matches:
                try:
                    rels.append(str(p.resolve().relative_to(base)))
                except ValueError:
                    rels.append(str(p))

            if rels:
                output = "\n".join(rels)
                if total_matched > len(rels):
                    output += f"\n\n({total_matched} files matched — showing newest {len(rels)}; raise head_limit or narrow the pattern)"
                if walk_cap_hit:
                    output += f"\n\n(walk cap of {self._GLOB_WALK_CAP} entries hit — narrow the pattern or path for complete results)"
            else:
                output = "no matches"
            return {
                "status": "success",
                "action": "glob",
                "command": command_str,
                "output": output,
            }

        except Exception as e:
            return {
                "status": "failed",
                "action": "glob",
                "command": command_str,
                "output": str(e),
            }