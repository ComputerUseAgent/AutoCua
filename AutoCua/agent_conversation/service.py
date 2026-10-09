"""ConversationService — permanent, resumable chat memory for the UI path.

This is the ONE place that knows how a chat is persisted, optimized and
replayed. app.py is thin glue:

    sid, prior = conversation.start_or_resume(session_id, task)   # load
    agent = AgentService(..., prior_history=prior)                # trigger
    agent.process_request(task)
    conversation.save_run(sid, agent.assistant_messages,          # save
                          agent.tool_responses, status, message, task)

On-disk layout — data lives OUTSIDE the install folder, so uninstalling the app
can never destroy the user's chats (the Windows installer uninstalls with
`[UninstallDelete] Type: filesandordirs; Name: "{app}"`, i.e. it deletes the
WHOLE install directory, and chats used to be written inside it):

    <home>/AutoCua_data/agent_conversation/     (packaged build)
    <repo>/AutoCua_data/agent_conversation/     (python app.py)
        index.json                      <- { id: {title, created_at,
                                              updated_at, last_done_message} }
        settings.json                   <- last-used provider/model (frontend)
        <session_id>/conversation.json  <- one folder per session: the
                                           optimized, resumable history snapshot

There is exactly ONE of these for the whole app — windows, mac and
ios all route their memory through this single service — so this one root
is every platform's chat store.

The saved history is the agent's FULL per-step memory: each assistant step with
its thinking / eval / decision / memory / next_goal / action intact, paired 1:1
with the agent's already-compacted tool results, and capped with a single
"terminal note" step carrying a clean done-message so a resumed run always knows
how the previous run ended — on EVERY ending (done, user stop, error, provider
kill, step limit, parse fails). Storing the FULL steps (not a trimmed copy) is
deliberate: the debug download (render_readable) is then the TRUE agent memory,
readable like main.py's conversation_N.txt. Resume token-efficiency is
unaffected — the agent's OWN message builder trims older steps to
decision/memory/next_goal when it sends them to the LLM.

It contains NO system prompt and NO screenshot bytes, so a resumed run never
double-loads the system prompt: the agent's message builder always prepends
exactly one system message and seeds these two lists underneath it. (The debug
log adds the system prompt for readability only, supplied by app.py at export.)
"""

import os
import re
import json
import time
import shutil
import logging
import threading
from pathlib import Path

logger = logging.getLogger("agent_conversation")

# Where the user's data lives — one definition for the whole app, in the package
# root, so chats / api keys / everything else can never disagree about it.
from AutoCua import data_root, install_dir, _ensure


class ConversationService:
    """Owns all conversation-memory persistence + optimization for the UI."""

    # Chat ids are minted as chat_<hex ms>, but they arrive from the CLIENT
    # (start_or_resume's request body, DELETE /api/chats/<id>) and are joined
    # into a path that delete_session() rmtree's. Now that root() sits in the
    # user's own data folder rather than inside the package, a traversal would
    # be far more destructive — so validate before building the path.
    _SAFE_ID = re.compile(r"^[A-Za-z0-9_-]{1,64}$")

    # ── storage paths ──────────────────────────────────────────────────────
    def root(self) -> Path:
        """Root folder holding index.json + one sub-folder per session.

        Lives in AutoCua_data/, OUTSIDE the install folder, so uninstalling the
        app no longer deletes the user's chats. The first call also migrates any
        chats left behind in this package's own directory by an older build."""
        d = _ensure(data_root() / "agent_conversation")
        self._migrate_legacy(d)
        return d

    def _index_file(self) -> Path:
        return self.root() / "index.json"

    def _is_safe_id(self, session_id) -> bool:
        return bool(self._SAFE_ID.match(str(session_id)))

    def _session_dir(self, session_id) -> Path:
        sid = str(session_id)
        if not self._SAFE_ID.match(sid):
            raise ValueError(f"unsafe session id: {sid!r}")
        return self.root() / sid

    def _session_file(self, session_id) -> Path:
        return self._session_dir(session_id) / "conversation.json"

    def _memory_log_file(self, session_id) -> Path:
        # The TRUE, human-readable memory log (exact payload sent to the model).
        return self._session_dir(session_id) / "memory_log.txt"

    def _exchanges_file(self, session_id) -> Path:
        # The reopen-view transcript: one {task, done_message, status} per run
        # ending, appended by save_run — separate from conversation.json (which
        # is REWRITTEN each run) so the full request/outcome history survives.
        return self._session_dir(session_id) / "exchanges.json"

    # ── one-time migration: chats out of the install folder ────────────────
    # Old layout, from this service's previous root():
    #     packaged : <exe dir>/AutoCua/agent_conversation/
    #     dev      : <repo>/AutoCua/agent_conversation/   <- THIS package's dir
    # In dev the package's own source (service.py, __init__.py, __pycache__) sits
    # in that very folder, so the migration is an ALLOWLIST, never a denylist:
    # only index.json, settings.json and chat_* directories are ever touched. No
    # file added to the package later can be moved by accident.
    _LEGACY_FILES = ("index.json", "settings.json")
    _LEGACY_DIR_PREFIX = "chat_"
    _SAFE_ENTRY = re.compile(r"^[A-Za-z0-9._-]+$")

    _migrate_lock = threading.Lock()
    _migrated_dests = set()      # keyed by destination, so tests can repoint the env var

    def _legacy_root(self) -> Path:
        """Where root() used to point before the move to AutoCua_data/."""
        return install_dir() / "AutoCua" / "agent_conversation"

    @staticmethod
    def _read_bytes(p: Path):
        """Path.read_bytes goes through io.open, which the compiled build's
        builtins.open patch does NOT replace. Reading a legacy path with the
        plain builtin open() inside AutoCua.exe could hand back the BUILD
        MACHINE's embedded copy instead of this user's file."""
        try:
            return p.read_bytes()
        except Exception:
            logger.exception("read %s", p)
            return None

    def _move_tree(self, src: Path, dst: Path) -> int:
        """Move one chat folder, crash- AND volume-safe.

        shutil.move degrades to copy+delete across volumes (the install drive is
        often not the home drive), so a crash mid-copy would leave a HALF-WRITTEN
        dst that the next run would accept as 'already migrated'. Stage into
        '<dst>.part' and rename into place: the rename is atomic within the
        destination folder, so dst either does not exist or is complete. The
        source is removed only after the rename succeeds."""
        if dst.exists():
            return 0                                      # newer data wins; never clobber
        part = dst.with_name(dst.name + ".part")
        try:
            if part.exists():
                shutil.rmtree(part, ignore_errors=True)   # stale attempt from a crash
            shutil.copytree(str(src), str(part))
            os.replace(str(part), str(dst))
            shutil.rmtree(str(src), ignore_errors=True)
            return 1
        except Exception:
            logger.exception("migrate chat folder %s", src)
            shutil.rmtree(part, ignore_errors=True)
            return 0

    def _move_file(self, src: Path, dst: Path) -> int:
        if dst.exists():
            return 0
        data = self._read_bytes(src)
        if data is None:
            return 0
        part = dst.with_name(dst.name + ".part")
        try:
            part.write_bytes(data)
            os.replace(str(part), str(dst))
            src.unlink()
            return 1
        except Exception:
            logger.exception("migrate %s", src)
            try:
                part.unlink()
            except Exception:
                pass
            return 0

    def _read_json_dict(self, p: Path) -> dict:
        raw = self._read_bytes(p)
        if not raw:
            return {}
        try:
            data = json.loads(raw.decode("utf-8"))
            return data if isinstance(data, dict) else {}
        except Exception:
            logger.exception("parse %s", p)
            return {}

    def _merge_legacy_index(self, legacy: Path, dest: Path) -> None:
        """Both index.json exist — reachable after an interrupted migration, or
        when a user ran an older build again after migrating. The NEW file always
        wins per chat id; a legacy row is adopted only when its chat folder is
        actually present now, so the sidebar never shows a ghost row pointing at
        a chat that never made it across."""
        merged = self._read_json_dict(dest)
        added = 0
        for cid, entry in self._read_json_dict(legacy).items():
            if cid in merged or not (dest.parent / str(cid)).is_dir():
                continue
            merged[cid] = entry
            added += 1
        if added:
            part = dest.with_name(dest.name + ".part")
            try:
                part.write_bytes(json.dumps(merged, indent=2).encode("utf-8"))
                os.replace(str(part), str(dest))   # atomic: never a truncated index
                logger.info("merged %d legacy chat(s) into %s", added, dest)
            except Exception:
                logger.exception("merge index.json")
                return
        try:
            legacy.unlink()
        except Exception:
            pass

    def _migrate_legacy(self, dest: Path) -> None:
        """Move pre-AutoCua_data chats into `dest` exactly once.

        WHERE IT RUNS: lazily, on the first root() call in each process. That
        covers EVERY entry point — the GUI, the --cli-mode / --minion-mode
        re-execs, a bare import of this module — with no ordering requirement on
        app.py, and it only runs when chat data is actually touched.

        IDEMPOTENT + CRASH-SAFE BY CONSTRUCTION, with NO on-disk 'done' marker:
        every item moves only when the destination does not exist, so a re-run is
        a no-op, a crash resumes on the next run, and a user who restores an old
        folder months later still gets it picked up. A marker would introduce
        state that can disagree with the filesystem; the in-process set below is
        only a fast path, never a correctness gate.

        CONCURRENCY: the lock covers threads (Flask serves the chat routes from
        worker threads). Across processes, skip-if-exists plus the broad excepts
        make a double run harmless."""
        key = str(dest)
        if key in self._migrated_dests:
            return
        with self._migrate_lock:
            if key in self._migrated_dests:
                return
            try:
                legacy = self._legacy_root()
                if not legacy.is_dir() or legacy.resolve() == dest.resolve():
                    return
                moved = 0
                for item in sorted(legacy.iterdir()):
                    name = item.name
                    if not self._SAFE_ENTRY.match(name):
                        continue
                    if item.is_dir() and name.startswith(self._LEGACY_DIR_PREFIX):
                        moved += self._move_tree(item, dest / name)
                    elif item.is_file() and name in self._LEGACY_FILES:
                        target = dest / name
                        if name == "index.json" and target.exists():
                            self._merge_legacy_index(item, target)
                        else:
                            moved += self._move_file(item, target)
                # NEVER rmtree the legacy folder: in dev it IS this package's
                # source dir. rmdir succeeds only when it is genuinely empty —
                # i.e. the packaged case, where the folder held data and nothing
                # else (AutoCua is embedded in the binary, not copied to disk).
                try:
                    legacy.rmdir()
                except OSError:
                    pass
                if moved:
                    logger.info("migrated %d chat item(s): %s -> %s", moved, legacy, dest)
            except Exception:
                logger.exception("migrate legacy conversation data")
            finally:
                # Set even on failure so we don't re-scan on every root() call.
                # The next app start retries from scratch.
                self._migrated_dests.add(key)

    # ── index.json (session -> display meta) ───────────────────────────────
    def _read_index(self) -> dict:
        f = self._index_file()
        if f.exists():
            try:
                with open(f, "r", encoding="utf-8") as fh:
                    data = json.load(fh)
                if isinstance(data, dict):
                    return data
            except Exception:
                logger.exception("read index.json")
        return {}

    def _write_index(self, index: dict) -> None:
        f = self._index_file()
        try:
            f.parent.mkdir(parents=True, exist_ok=True)
            with open(f, "w", encoding="utf-8") as fh:
                json.dump(index, fh, indent=2)
        except Exception:
            logger.exception("write index.json")

    def _touch_index(self, session_id, *, title=None, last_done_message=None,
                     context_tokens=None, context_cap=None, run_pkg=None,
                     agent_mode=None) -> dict:
        """Create-or-update one index entry. created_at + title are set ONCE
        (title never overwritten, so the original objective stays the label);
        updated_at + last_done_message + context_tokens/context_cap refresh
        whenever provided.

        context_tokens is the memory bar's LATEST context size (not a sum) and
        context_cap is the fixed memory budget the bar fills toward (MemoryTracker
        MEMORY_CAP = 300k — NOT the model context window) — together they let a
        reopened chat restore the bar to where memory was."""
        index = self._read_index()
        now = int(time.time())
        entry = index.get(str(session_id)) or {
            "title": None, "created_at": now,
            "updated_at": now, "last_done_message": "",
            "context_tokens": 0, "context_cap": 0,
        }
        if title and not entry.get("title"):
            entry["title"] = title
        if last_done_message is not None:
            entry["last_done_message"] = last_done_message
        if context_tokens is not None:
            entry["context_tokens"] = int(context_tokens or 0)
        if context_cap is not None:
            entry["context_cap"] = int(context_cap or 0)
        if run_pkg:
            # Which agent package produced this chat's memory (mac /
            # windows / ios) — lets a resume detect a mode switch.
            entry["run_pkg"] = run_pkg
        if agent_mode:
            # Which AGENT MODE owns this chat ("computer" / "mobile" / "shell").
            # Orthogonal to run_pkg: shell runs share the desktop PLATFORM_PKG,
            # so the pkg alone can't tell a shell chat from a computer one —
            # this field is what the per-chat mode lock reads. Stamped at MINT
            # time (start_or_resume) so even a chat whose first run never
            # finished is correctly tagged, then re-stamped on every save.
            entry["agent_mode"] = agent_mode
        entry["updated_at"] = now
        index[str(session_id)] = entry
        self._write_index(index)
        return entry

    # ── per-session conversation.json ──────────────────────────────────────
    def _read_session(self, session_id):
        """Return the stored optimized history dict, or None on missing/corrupt
        (so the caller can resume the SAME id with a fresh agent context)."""
        f = self._session_file(session_id)
        if f.exists():
            try:
                with open(f, "r", encoding="utf-8") as fh:
                    data = json.load(fh)
                if isinstance(data, dict) and isinstance(data.get("history"), dict):
                    return data["history"]
            except Exception:
                logger.exception("read session %s", session_id)
        return None

    def _write_session(self, session_id, history: dict) -> None:
        f = self._session_file(session_id)
        try:
            f.parent.mkdir(parents=True, exist_ok=True)
            with open(f, "w", encoding="utf-8") as fh:
                json.dump({"session_id": str(session_id), "history": history}, fh, indent=2)
        except Exception:
            logger.exception("write session %s", session_id)

    # ── per-session exchanges.json (reopen-view transcript) ────────────────
    def _read_exchanges(self, session_id) -> list:
        f = self._exchanges_file(session_id)
        if f.exists():
            try:
                with open(f, "r", encoding="utf-8") as fh:
                    data = json.load(fh)
                if isinstance(data, list):
                    return data
            except Exception:
                logger.exception("read exchanges %s", session_id)
        return []

    def _append_exchange(self, session_id, task, done_message, status) -> None:
        """Append this run's (user request, terminal message) pair. Every run
        ending records one — completion, user stop, error alike — so a reopened
        chat can replay '1. request / outcome' for its whole life."""
        items = self._read_exchanges(session_id)
        items.append({
            "task": str(task or ""),
            "done_message": str(done_message or ""),
            "status": str(status or ""),
            "at": int(time.time()),
        })
        f = self._exchanges_file(session_id)
        try:
            f.parent.mkdir(parents=True, exist_ok=True)
            with open(f, "w", encoding="utf-8") as fh:
                json.dump(items, fh, indent=2, ensure_ascii=False)
        except Exception:
            logger.exception("write exchanges %s", session_id)

    # ── helpers ────────────────────────────────────────────────────────────
    def _new_id(self) -> str:
        """Filesystem-safe persistent chat id (distinct from app.py's per-run
        guard token)."""
        return f"chat_{int(time.time() * 1000):x}"

    def _title_from_task(self, task) -> str:
        """First non-empty line of the task, trimmed to a short sidebar label."""
        text = (task or "").strip()
        if not text:
            return "New chat"
        first = text.splitlines()[0].strip()
        return first if len(first) <= 60 else first[:57].rstrip() + "..."

    def _terminal_message(self, status: str, message: str) -> str:
        """Clean 'done message' for ANY ending — so an interrupted / failed /
        provider-killed run still records a sensible conclusion in memory."""
        message = message or ""
        if status == "success":
            return f"Task completed: {message}".strip().rstrip(":")
        if status == "error":
            return f"Agent terminated (error / provider): {message}".strip().rstrip(":")
        return f"Agent stopped before completing: {message}".strip().rstrip(":")

    @staticmethod
    def _is_terminal_note(entry) -> bool:
        """True only for OUR synthetic terminal note (the resume bridge), any
        generation. Structural — exact key sets + the canonical sentence — so a
        real step (native {content, tool_calls} or 4-key main-agent JSON) that
        merely echoes the bridge phrase it saw in context can never match."""
        try:
            d = json.loads(str(entry))
        except Exception:
            return False
        if not isinstance(d, dict):
            return False
        keys = set(d.keys())
        goal = str(d.get("next_goal", ""))
        # Main-agent bridge (both generations): pre-thinking {memory, next_goal}
        # and {thinking, memory, next_goal} (thinking = "not required") — also
        # the shape shell chats saved before the coder's native-tools bridge.
        if keys in ({"memory", "next_goal"}, {"thinking", "memory", "next_goal"}):
            return goal.startswith("Previous run concluded.")
        # Shell (coder) bridge: labeled convention — memory folded into
        # next_goal ("memory: ... next_goal: ..."), no standalone memory key.
        if keys == {"thinking", "next_goal"}:
            return goal.startswith("memory: ") and "next_goal: Previous run concluded." in goal
        return False

    def _build_history(self, task, assistant_messages, tool_responses,
                       status, message, prior_task=None, agent_mode=None,
                       request_no=None, sub_agent_last_id=None) -> dict:
        """Build the saved conversation = the agent's FULL per-step memory
        (thinking / eval / decision / memory / next_goal / action all preserved,
        tool results as-is), capped with a terminal note recording how the run
        ended.

        We store the FULL steps — NOT a trimmed copy — so the debug download is
        the true, human-readable agent memory. Resume token-efficiency is
        unaffected: the agent's OWN message builder trims older steps to
        decision/memory/next_goal when it sends them to the LLM, so storing full
        here costs only a little disk and loses nothing for debugging.
        prior_task preserves the ORIGINAL objective across resumes."""
        full_assistant = list(assistant_messages or [])
        tools = list(tool_responses or [])
        # Force exact 1:1 alignment defensively.
        if len(tools) < len(full_assistant):
            tools += [None] * (len(full_assistant) - len(tools))
        else:
            tools = tools[:len(full_assistant)]
        done_message = self._terminal_message(status, message)
        # A zero-step run (stopped/failed before its first step) saves its seed
        # back unchanged — the tail is then ALREADY a terminal note with an
        # empty tool slot. Shell chats RECORD that run instead of erasing it:
        # fill the old bridge's slot with this request's numbered marker (the
        # same string a normal resume writes), then let the new terminal note
        # below cap it — the memory keeps "request N arrived → stopped/failed"
        # in chronological order and the previous conclusion survives.
        # Elsewhere (main agent, or no request number to write) stacking would
        # accrete dangling notes with nothing between them — REPLACE the tail
        # so it stays singular and carries the NEWEST ending.
        # The check is STRUCTURAL (exact key sets + the canonical sentence),
        # never a substring — a real model step that merely echoes the bridge
        # phrase it saw in context must never be eaten.
        if full_assistant and tools[-1] is None and self._is_terminal_note(full_assistant[-1]):
            if agent_mode == "shell" and request_no and str(task or "").strip():
                tools[-1] = (f"<user_request={int(request_no)}>\n{task}"
                             f"\n</user_request={int(request_no)}>")
            else:
                full_assistant.pop()
                tools.pop()
        # Cap with a terminal note so a resumed run (and the reader) always sees
        # how the previous run ended — on EVERY ending (done / stop / error). On
        # resume it becomes the most-recent step, so the agent's builder replays
        # every real step's tool result beneath it.
        if agent_mode == "shell":
            # Shell (coder) bridge — the coder's native-tool-calling
            # conventions: thinking uses its "skipped" convention, and memory
            # folds into the labeled next_goal ("memory: ... next_goal: ...")
            # because the native step format has no standalone memory field.
            note = {
                "thinking": "skipped",
                "next_goal": (f"memory: {done_message} "
                              "next_goal: Previous run concluded. "
                              "Awaiting the user's next request."),
            }
        else:
            note = {
                "thinking": "not required",   # matches the step schema's skip convention
                "memory": done_message,
                "next_goal": "Previous run concluded. Awaiting the user's next request; resume from this point.",
            }
        full_assistant.append(json.dumps(note, ensure_ascii=False, indent=2))
        tools.append(None)
        payload = {
            "version": 1,
            "task": prior_task or task,
            "assistant_messages": full_assistant,
            "tool_responses": tools,
            "final_status": status,
            "final_message": message,
            "done_message": done_message,
        }
        # Desktop main agents only: the last sub-agent id handed out, so the ids
        # in <sub_agents> keep counting on the next request of this chat.
        if sub_agent_last_id is not None:
            payload["sub_agent_last_id"] = int(sub_agent_last_id)
        return payload

    # ── public API used by app.py ──────────────────────────────────────────
    def start_or_resume(self, req_session_id, task, agent_mode=None):
        """Resolve the chat session for an incoming run.

        Returns (session_id, prior_history):
          - existing session (folder present)  -> CONTINUATION: load its saved
            optimized history (None if missing/corrupt -> fresh agent context on
            the SAME id).
          - otherwise -> fresh start: mint a new id and seed its index entry so
            the sidebar shows it immediately. prior_history is None.

        agent_mode ("computer"/"mobile"/"shell") stamps a freshly MINTED chat's
        index row so the per-chat mode lock is correct from second one — even if
        the run later dies before save_run ever re-stamps it.
        """
        # A malformed id (stale/corrupt client state) is treated as "no such
        # session" and falls through to a fresh start, exactly as it did before
        # _session_dir began rejecting unsafe ids — starting a run must never
        # fail just because the client sent a junk id.
        if (req_session_id and req_session_id != "new"
                and self._is_safe_id(req_session_id)
                and self._session_dir(req_session_id).exists()):
            sid = str(req_session_id)
            return sid, self._read_session(sid)
        sid = self._new_id()
        self._touch_index(sid, title=self._title_from_task(task), agent_mode=agent_mode)
        return sid, None

    def save_run(self, session_id, assistant_messages, tool_responses, status,
                 message, task, last_messages=None, context_tokens=None,
                 context_cap=None, run_pkg=None, agent_mode=None,
                 request_no=None, sub_agent_last_id=None):
        """Persist a finished run + refresh index meta. Writes TWO things:
          - conversation.json  — the lean resume seed (assistant/tool turns).
          - memory_log.txt      — the TRUE debug memory: the exact final payload
            sent to the model (system + interleaved history + the live user
            message that re-injects user_request/todo/scratchpad/element_tree).
        Called for EVERY ending (completion, user stop, error, provider kill)."""
        try:
            existing = self._read_session(session_id)
            prior_task = existing.get("task") if isinstance(existing, dict) else None
            # A run that never built its agent reports no counter: keep the saved one
            if sub_agent_last_id is None and isinstance(existing, dict):
                sub_agent_last_id = existing.get("sub_agent_last_id")
            try:
                sub_agent_last_id = None if sub_agent_last_id is None else int(sub_agent_last_id)
            except (TypeError, ValueError, OverflowError):
                sub_agent_last_id = None   # a damaged value must never cost the chat its save
            payload = self._build_history(
                task, assistant_messages, tool_responses, status, message, prior_task,
                agent_mode=agent_mode, request_no=request_no,
                sub_agent_last_id=sub_agent_last_id,
            )
            self._write_session(session_id, payload)
            done_message = payload["done_message"]
            # Reopen-view transcript: this run's request + how it ended.
            self._append_exchange(session_id, task, done_message, status)

            # TRUE memory log (debug): render the exact payload the model received.
            if last_messages:
                header = [
                    "=== AGENT MEMORY — exact payload sent to the model (true snapshot) ===",
                    f"Session: {session_id}",
                    f"Task: {payload.get('task', '')}",
                    f"Final status: {status}",
                    f"Done: {done_message}",
                ]
                # The latest step's response is generated AFTER last_messages was
                # built, so append it as the final assistant turn (like main.py).
                final_response = (assistant_messages or [None])[-1]
                text = self._render_messages(last_messages, header, final_response)
                self._write_text(self._memory_log_file(session_id), text)

            self._touch_index(
                session_id,
                title=self._title_from_task(prior_task or task),  # no-op if already set
                last_done_message=done_message,
                context_tokens=context_tokens,  # latest context size for the memory bar
                context_cap=context_cap,        # fixed 300k memory budget (MEMORY_CAP)
                run_pkg=run_pkg,                # which agent produced this memory
                agent_mode=agent_mode,          # which MODE owns the chat (per-chat lock)
            )
            return done_message
        except Exception:
            logger.exception("save_run %s", session_id)
            return None

    def _write_text(self, path: Path, text: str) -> None:
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(text)
        except Exception:
            logger.exception("write text %s", path)

    @staticmethod
    def _content_text(content):
        """Flatten a message's content to text. Prompt-cached turns carry content
        as a list of {type, text, ...} blocks rather than a plain string."""
        if isinstance(content, list):
            return "\n".join(b.get("text", "") for b in content if isinstance(b, dict))
        return content if isinstance(content, str) else str(content)

    @staticmethod
    def _render_tool_calls(calls) -> str:
        """Readable view of a native assistant turn's tool_calls: the tracking
        params pulled OUT of the call arguments into their own fields, then one
        action entry per call — the same shape the agents' snapshot views use.
        Without this the whole step renders as an empty assistant turn (all its
        substance rides in the calls).

        `memory` is a MAIN-DRIVER tracking param (the coder folds memory into
        next_goal and never sends one), so it is rendered only when present —
        coder turns keep their exact previous rendering."""
        thinking = memory = next_goal = ""
        actions = []
        for tc in calls or []:
            fn = (tc or {}).get("function") or {}
            args = fn.get("arguments")
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except (ValueError, TypeError):
                    args = {"raw_arguments": args}
            if not isinstance(args, dict):
                args = {}
            args = dict(args)
            step_thinking = str(args.pop("thinking", "") or "").strip()
            step_memory = str(args.pop("memory", "") or "").strip()
            step_next_goal = str(args.pop("next_goal", "") or "").strip()
            thinking = thinking or step_thinking
            memory = memory or step_memory
            next_goal = next_goal or step_next_goal
            actions.append({"type": fn.get("name") or "", **args})
        rendered = {"thinking": thinking or "skipped"}
        if memory:
            rendered["memory"] = memory
        rendered["next_goal"] = next_goal
        rendered["action"] = actions
        return json.dumps(rendered, indent=2, ensure_ascii=False)

    @staticmethod
    def _render_persisted_step(entry) -> str:
        """A persisted assistant entry, readable: a native {content, tool_calls}
        step renders like the live turns (provider_meta dropped — it is opaque
        provider blobs, unreadable by design); anything else (old-format step
        JSON, bridge note, plain prose) is already readable and passes through."""
        try:
            d = json.loads(str(entry))
        except Exception:
            return str(entry or "")
        if (isinstance(d, dict) and isinstance(d.get("tool_calls"), list)
                and ("content" in d or d.get("tool_calls"))):
            content = str(d.get("content") or "")
            block = ConversationService._render_tool_calls(d["tool_calls"])
            return (content + "\n" + block) if content else block
        return str(entry or "")

    @staticmethod
    def _render_persisted_results(raw) -> str:
        """Persisted tool results, readable: the native [{tool_call_id, content}]
        list joins to its content bodies (already <Tool_response>-wrapped);
        plain strings (request markers, legacy results) pass through."""
        try:
            data = json.loads(str(raw))
        except Exception:
            return str(raw)
        if isinstance(data, list) and all(isinstance(d, dict) and "tool_call_id" in d for d in data):
            return "\n".join(str(d.get("content") or "") for d in data)
        return str(raw)

    def _render_messages(self, messages, header_lines=None, final_response=None):
        """Render an exact messages payload to the human-readable conversation log
        (same shape as main.py's conversation_N.txt): SYSTEM PROMPT block, then
        each ASSISTANT / USER / TOOL turn in the order sent. Native (coder)
        assistant turns get their tool_calls rendered readably; main-agent turns
        carry no tool_calls and render exactly as before."""
        bar = "=" * 60
        out = list(header_lines or [])
        if header_lines:
            out += [bar, ""]
        for m in messages or []:
            role = str(m.get("role", "?")).upper()
            content = self._content_text(m.get("content", ""))
            if role == "SYSTEM":
                out += ["=== SYSTEM PROMPT ===", content, "", bar, ""]
                continue
            calls = m.get("tool_calls")
            if calls:
                block = self._render_tool_calls(calls)
                content = (content + "\n" + block) if content else block
            out += [f"--- {role} ---", content, ""]
        if final_response:
            out += ["--- ASSISTANT (final response) ---",
                    self._render_persisted_step(final_response), ""]
        return "\n".join(out) + "\n"

    def list_sessions(self):
        """All sessions, newest-first, as {id, name, updated_at, last_done_message}."""
        index = self._read_index()
        items = [{
            "id": sid,
            "name": meta.get("title") or "New chat",
            "updated_at": meta.get("updated_at", 0),
            "last_done_message": meta.get("last_done_message", ""),
            "agent_mode": meta.get("agent_mode", ""),
            "run_pkg": meta.get("run_pkg") or "",   # legacy rows: lets the sidebar infer the mode icon
        } for sid, meta in index.items()]
        items.sort(key=lambda x: x.get("updated_at", 0), reverse=True)
        return items

    def get_session(self, session_id):
        """{id, name, last_done_message, exchanges, context_tokens, context_cap}
        for the reopen view, or None if unknown. `exchanges` is the full
        request/outcome transcript ({task, done_message, status} per run) shown
        as the numbered Agent Notes on reopen; last_done_message stays for
        legacy sessions saved before exchanges.json existed. Legacy rows that
        only carried the old cumulative `tokens_used` are ignored
        (context_tokens defaults to 0, so the bar starts empty and the next run
        corrects it)."""
        meta = self._read_index().get(str(session_id))
        if not meta:
            return None
        return {
            "id": str(session_id),
            "name": meta.get("title") or "New chat",
            "context_tokens": int(meta.get("context_tokens", 0) or 0),
            "context_cap": int(meta.get("context_cap", 0) or 0),
            "last_done_message": meta.get("last_done_message", ""),
            "run_pkg": meta.get("run_pkg") or "",
            "agent_mode": meta.get("agent_mode", ""),
            "exchanges": self._read_exchanges(session_id),
        }

    def delete_session(self, session_id) -> bool:
        """Remove a session's folder and its index entry (idempotent)."""
        # Nothing with a malformed id can exist on disk, so refuse quietly
        # rather than letting _session_dir raise into the handler below and
        # log a traceback for what is simply a bad request.
        if not self._is_safe_id(session_id):
            logger.warning("delete_session: ignoring unsafe id %r", session_id)
            return False
        try:
            d = self._session_dir(session_id)
            if d.exists():
                shutil.rmtree(d, ignore_errors=True)
            index = self._read_index()
            if str(session_id) in index:
                del index[str(session_id)]
                self._write_index(index)
            return True
        except Exception:
            logger.exception("delete_session %s", session_id)
            return False

    def read_raw(self, session_id):
        """Full saved conversation.json payload ({session_id, history}) exactly as
        on disk, or None if missing/corrupt. This is the agent's real optimized
        memory — used by the debug export."""
        f = self._session_file(session_id)
        if f.exists():
            try:
                with open(f, "r", encoding="utf-8") as fh:
                    data = json.load(fh)
                if isinstance(data, dict):
                    return data
            except Exception:
                logger.exception("read_raw %s", session_id)
        return None

    def render_readable(self, session_id, system_prompt=None):
        """Render a session's saved memory as a HUMAN-READABLE conversation log —
        the same shape main.py's save_conversation writes (conversation_N.txt):
        a SYSTEM PROMPT block, then the real interleaved ASSISTANT (per-step JSON,
        full thinking/eval/decision/memory/next_goal/action) / USER
        (<tool_response>) turns. This is the true agent memory, formatted to read
        and critique — not raw JSON. Returns the text, or None if nothing saved."""
        data = self.read_raw(session_id)
        if data is None:
            return None
        hist = data.get("history") or {}
        meta = self.get_session(session_id) or {}
        asst = hist.get("assistant_messages") or []
        tools = hist.get("tool_responses") or []
        task = hist.get("task", "") or ""
        bar = "=" * 60
        out = [
            "=== AGENT MEMORY (conversation log) ===",
            f"Chat: {meta.get('name', '')}",
            f"Session: {session_id}",
            f"Steps: {len(asst)}",
            f"Final status: {hist.get('final_status', '')}",
            f"Done: {hist.get('done_message', '')}",
            f"Task: {task}",
            bar,
        ]
        if system_prompt:
            out += ["", "=== SYSTEM PROMPT ===", system_prompt.strip(), bar]
        for i, step in enumerate(asst):
            out += ["", "--- ASSISTANT ---"]
            if i == 0 and task:
                out += ["<User_Task>", task, "</User_Task>", ""]
            out.append(self._render_persisted_step(step))
            tr = tools[i] if i < len(tools) else None
            if tr:
                out += ["", "--- USER ---", "<tool_response>",
                        self._render_persisted_results(tr), "</tool_response>"]
        return "\n".join(out) + "\n"

    def export_to_downloads(self, session_id, system_prompt=None):
        """Debug aid: write a session's TRUE memory log (.txt) to the user's
        Downloads folder. Prefers memory_log.txt (the exact payload sent to the
        model); falls back to a reconstruction for chats saved before memory_log
        existed. Returns the destination path, or None if nothing's saved."""
        text = None
        log_file = self._memory_log_file(session_id)
        if log_file.exists():
            try:
                text = log_file.read_text(encoding="utf-8")
            except Exception:
                logger.exception("read memory_log %s", session_id)
                text = None
        if text is None:
            text = self.render_readable(session_id, system_prompt)  # back-compat fallback
        if text is None:
            return None
        meta = self.get_session(session_id) or {}
        slug = re.sub(r"[^A-Za-z0-9._-]+", "-", meta.get("name") or str(session_id)).strip("-")[:40]
        # System date-time in a clear, readable form so each download is easy to tell apart.
        fname = f"AutoCua-{slug or session_id}-{time.strftime('%Y-%m-%d_%H-%M-%S')}.txt"
        home = Path.home()
        for folder in (home / "Downloads", home / "Desktop", home):
            try:
                if folder.exists():
                    dest = folder / fname
                    with open(dest, "w", encoding="utf-8") as fh:
                        fh.write(text)
                    return str(dest)
            except Exception:
                logger.exception("export_to_downloads -> %s", folder)
                continue
        return None


# Module-level singleton — app.py imports this directly.
conversation = ConversationService()
