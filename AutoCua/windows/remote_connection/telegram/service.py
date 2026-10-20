"""Telegram → AgentService bridge with a guided provider/model picker.

Runs as a standalone process (not mounted into Flask). On the first message
the bot asks you to pick a provider (limited to providers with a non-empty
key in api_key.txt / .env), then a model (from the same MODEL_MAPPINGS the
AutoCua frontend uses). Subsequent messages are dispatched as tasks to the
agent with that provider/model. Picked provider/model persist for the whole
chat session until you `/reset`.

Token lookup order (first non-empty wins):
  1. TELEGRAM_BOT_TOKEN env var
  2. .env at the project root
  3. AutoCua/api_key/api_key.txt

Setup:
  1. @BotFather → /newbot → copy token.
  2. Paste it into .env OR api_key.txt as TELEGRAM_BOT_TOKEN=…
  3. Make sure at least one provider key (e.g. OPENROUTER_API_KEY=…) is set.
  4. python -m AutoCua.windows.remote_connection.telegram.service
  5. On phone: open Telegram, find your bot, send any message.
"""
import asyncio
import datetime
import importlib
import json
import logging
import sys
import threading
from pathlib import Path

from telegram import Update, InlineKeyboardButton, InlineKeyboardMarkup
from telegram.error import BadRequest
from telegram.ext import (
    Application,
    CommandHandler,
    MessageHandler,
    CallbackQueryHandler,
    filters,
)
from telegram.request import HTTPXRequest

logger = logging.getLogger(__name__)

# The Telegram surface treats api_key.txt as its single source of truth — we
# deliberately do NOT consult .env or env vars here. .env is app.py's general
# env-loading concern; keeping the bot self-contained against api_key.txt
# avoids two-files-of-record confusion.
#
# api_key.txt now lives in AutoCua_data/api_key/, OUTSIDE the install folder, so
# uninstalling AutoCua no longer wipes the bot token along with every API key.
# AutoCua/__init__.py owns that resolution for the whole app — the Settings
# panel, the agent's llm_provider and this bot all call the same function, so
# they cannot drift onto different files (the old __file__-walk here had to
# special-case the compiled build for exactly that reason).
from AutoCua import api_key_file

_API_KEY_FILE = api_key_file()

# Agent writes per-step "milestone" lines here. We tail this file during a
# task and forward each new line back to the user's Telegram chat so they
# see the agent's progress in real time.
SCRATCHPAD_PATH = (
    Path(__file__).resolve().parents[2] / "scratchpad" / "milestone" / "milestone.md"
)
SCRATCHPAD_POLL_SEC = 2.0
MAX_TG_MSG_LEN = 4000  # Telegram caps at 4096; leave headroom for safety

# Provider id → API-key name in the KV files. Same mapping the macOS side
# uses ([mac/remote_connection/telegram/service.py:78-85]).
PROVIDER_KEY_MAP = {
    "openrouter": "OPENROUTER_API_KEY",
    "groq":       "GROQ_API_KEY",
    "openai":     "OPENAI_API_KEY",
    "anthropic":  "ANTHROPIC_API_KEY",
    "google":     "GOOGLE_API_KEY",
    "perplexity": "PERPLEXITY_API_KEY",
    "together":   "TOGETHER_API_KEY",
    "cerebras":   "CEREBRAS_API_KEY",
    "aws":        "AWS_API_KEY",
}


# ── file helpers ─────────────────────────────────────────────────────────────

def _read_all_keys(path: Path) -> dict:
    """Parse a simple KEY=VALUE file (one per line) into a dict. Skips empty
    values and lines starting with '#'."""
    out = {}
    if not path.exists():
        return out
    try:
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, _, v = line.partition("=")
                k, v = k.strip(), v.strip()
                if v:
                    out[k] = v
    except Exception:
        pass
    return out


def _resolve_token() -> str | None:
    """Read TELEGRAM_BOT_TOKEN from api_key.txt only. .env and env vars are
    intentionally ignored — see header comment."""
    return _read_all_keys(_API_KEY_FILE).get("TELEGRAM_BOT_TOKEN")


def _get_available_providers() -> list:
    """Providers with a non-empty key in api_key.txt only."""
    keys = _read_all_keys(_API_KEY_FILE)
    return [
        {"id": pid, "key": keys[kname]}
        for pid, kname in PROVIDER_KEY_MAP.items()
        if keys.get(kname)
    ]


def _set_key_in_file(path: Path, key: str, value: str) -> None:
    """Write/update KEY=value in a KV file, preserving every other line.

    Unlike a naive read-all-and-write-back-with-_read_all_keys, this keeps
    empty-value placeholder lines (e.g. GROQ_API_KEY=) intact — the AutoCua
    UI relies on those for its provider list rendering.
    """
    lines = []
    found = False
    if path.exists():
        try:
            with open(path, "r", encoding="utf-8") as f:
                for raw in f:
                    stripped = raw.strip()
                    if stripped.startswith(f"{key}="):
                        lines.append(f"{key}={value}\n")
                        found = True
                    else:
                        lines.append(raw if raw.endswith("\n") else raw + "\n")
        except Exception:
            logger.warning("failed to read %s while updating %s", path, key)
            return
    if not found:
        lines.append(f"{key}={value}\n")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with open(path, "w", encoding="utf-8") as f:
            f.writelines(lines)
    except Exception:
        logger.warning("failed to write %s", path)


def _resolve_owner_chat_id() -> int | None:
    """Owner chat_id = the chat that claimed the bot (see _is_owner_chat).
    Stored in api_key.txt as TELEGRAM_OWNER_CHAT_ID=…, so it survives restarts."""
    val = _read_all_keys(_API_KEY_FILE).get("TELEGRAM_OWNER_CHAT_ID")
    if not val:
        return None
    try:
        return int(val)
    except ValueError:
        return None


def _save_owner_chat_id(chat_id: int) -> None:
    """Persist the owner chat_id so we can message them on the next boot."""
    _set_key_in_file(_API_KEY_FILE, "TELEGRAM_OWNER_CHAT_ID", str(chat_id))


# The claim is held here only if api_key.txt could not be written.
_owner_chat_unsaved: int | None = None


def _is_owner_chat(chat_id: int) -> bool:
    """Only the owner's chat is obeyed. The first chat to talk to the bot
    claims it; every other chat is ignored after that. Disconnect in the app
    clears the owner (and the token), so the bot can be claimed again."""
    global _owner_chat_unsaved
    if not _resolve_token():
        return False  # disconnected: the bot keeps polling until restart, obeying nobody
    owner = _resolve_owner_chat_id() or _owner_chat_unsaved
    if owner is not None:
        if chat_id != owner:
            logger.info("ignored an update from a chat that is not the owner")
        return chat_id == owner
    _save_owner_chat_id(chat_id)
    if _resolve_owner_chat_id() != chat_id:
        _owner_chat_unsaved = chat_id
    return True


def _get_models_for_provider(provider_id: str) -> list:
    """Read MODEL_MAPPINGS from AutoCua/llm_provider/<id>/view.py
    and return non-hidden entries as [{id, display_name}, …]."""
    try:
        mod = importlib.import_module(
            f"AutoCua.llm_provider.{provider_id}.view"
        )
        mappings = getattr(mod, "MODEL_MAPPINGS", {})
        return [
            {"id": mid, "display_name": info.get("display_name", mid)}
            for mid, info in mappings.items()
            if not info.get("hidden", False)
        ]
    except Exception:
        return []


# ── per-chat state ───────────────────────────────────────────────────────────

# chat_id → {
#   "phase":            "idle" | "pick_provider" | "pick_model" | "ready" | "running",
#   "provider":         str | None,
#   "model":            str | None,
#   "model_display":    str | None,
#   "queue":            list[str],  # tasks waiting to run, FIFO
#   "pending":          dict[str, str],  # pending_id → task awaiting Yes/No
#   "pending_counter":  int,         # monotonic id source for pending + pending_continue
#   "history":          dict | None,  # prior-run agent memory, RAM only (the
#                                     # AgentService prior_history contract shape)
#   "pending_continue": dict[str, str],  # pending_id → task awaiting Continue/Fresh
# }
_chat_state: dict = {}

# Guards mutations that read+modify state across threads (queue drain races
# between _run_agent's finally and the callback handler tapping "Yes").
_state_lock = threading.Lock()


def _state(chat_id: int) -> dict:
    return _chat_state.setdefault(chat_id, {"phase": "idle"})


def _maybe_run_next_queued(chat_id: int, bot, loop) -> None:
    """If this chat is ready and has a queued task, pop the next one and
    start it. Threadsafe — called from both _run_agent's finally (worker
    thread) and the q+ callback (asyncio loop)."""
    with _state_lock:
        state = _chat_state.get(chat_id)
        if not state:
            return
        if state.get("phase") != "ready":
            return
        queue = state.get("queue") or []
        if not queue:
            return
        provider = state.get("provider")
        model = state.get("model")
        if not provider or not model:
            return
        next_task = queue.pop(0)
        display = state.get("model_display") or model
        # Tasks queued mid-run are follow-ups: auto-continue the session that
        # just finished (its memory was captured before this drain ran).
        history = state.get("history")
        state["phase"] = "running"

    _send_chat(
        bot,
        chat_id,
        f"📝 Running queued task: {next_task[:200]}  ({provider} · {display})",
        loop,
    )
    threading.Thread(
        target=_run_agent,
        args=(next_task, provider, model, chat_id, bot, loop, history, state),
        daemon=True,
        name=f"telegram-agent-{chat_id}-queued",
    ).start()


def _build_prior_history(task, assistant_messages, tool_responses,
                         status, message, prior_task=None,
                         sub_agent_last_id=None):
    """In-RAM twin of agent_conversation's _build_history — that one persists
    to disk and surfaces in the desktop chat sidebar, which the Telegram
    surface deliberately avoids. Packages a finished run's memory lists into
    the prior_history dict AgentService expects, capped with the synthetic
    terminal bridge step + None tool slot. The bridge JSON must contain
    main_driver's _BRIDGE_SIGNATURE ('"next_goal": "Previous run concluded.')
    verbatim, or the <updated_user_request> run-marker logic silently stops
    firing on resume. Returns None when the run produced no
    steps — the caller keeps the previous memory then. prior_task freezes the
    ORIGINAL run-1 objective across continuations."""
    full_assistant = list(assistant_messages or [])
    if not full_assistant:
        return None
    tools = list(tool_responses or [])
    if len(tools) < len(full_assistant):
        tools += [None] * (len(full_assistant) - len(tools))
    else:
        tools = tools[:len(full_assistant)]
    message = message or ""
    if status == "success":
        done_message = f"Task completed: {message}".strip().rstrip(":")
    elif status == "error":
        done_message = f"Agent terminated (error / provider): {message}".strip().rstrip(":")
    else:
        done_message = f"Agent stopped before completing: {message}".strip().rstrip(":")
    full_assistant.append(json.dumps({
        "memory": done_message,
        "next_goal": "Previous run concluded. Awaiting the user's next request; resume from this point.",
    }, ensure_ascii=False, indent=2))
    tools.append(None)
    history = {
        "version": 1,
        "task": prior_task or task,
        "assistant_messages": full_assistant,
        "tool_responses": tools,
        "done_message": done_message,
    }
    # The last sub-agent id handed out, so the ids keep counting on the next task
    if sub_agent_last_id is not None:
        history["sub_agent_last_id"] = int(sub_agent_last_id)
    return history


# ── Telegram handlers ────────────────────────────────────────────────────────

def _build_online_text(providers: list) -> str:
    now_str = datetime.datetime.now().strftime("%H:%M:%S")
    if providers:
        provider_line = ", ".join(p["id"] for p in providers)
        return f"🟢 AutoCua online at {now_str}\nProviders: {provider_line}"
    return f"🟢 AutoCua online at {now_str}\nProviders: (none configured)"


async def _show_provider_picker(message):
    providers = _get_available_providers()
    # Always lead with the "AutoCua online" status line so the user gets the
    # same greeting they'd see at app boot, even when they message the bot
    # first instead of waiting for the unsolicited startup announcement.
    await message.reply_text(_build_online_text(providers))
    if not providers:
        await message.reply_text(
            "⚠️ No provider API keys found. Add at least one (e.g. "
            "OPENROUTER_API_KEY=…) to api_key.txt or .env and try again."
        )
        return False
    buttons = [
        [InlineKeyboardButton(p["id"], callback_data=f"provider:{p['id']}")]
        for p in providers
    ]
    await message.reply_text(
        "👋 Pick a provider:", reply_markup=InlineKeyboardMarkup(buttons)
    )
    return True


async def _discover_owner_from_updates(bot) -> int | None:
    """Peek at the latest pending update on Telegram's servers and use its
    chat_id as the owner. Lets the bot self-bootstrap on the very first run
    after the chat-saving code was deployed, without requiring the user to
    /start again. Safe to call before start_polling — uses offset=-1 which
    Telegram supports as 'just the most recent update', and doesn't consume
    updates from the polling updater's offset cursor."""
    try:
        updates = await bot.get_updates(offset=-1, limit=1, timeout=2)
    except Exception:
        logger.warning("owner discovery: get_updates failed", exc_info=True)
        return None
    for upd in updates:
        chat = getattr(upd, "effective_chat", None)
        if chat and chat.id:
            return int(chat.id)
    return None


async def _post_init(application) -> None:
    """Fires once after the bot finishes initialising (before polling starts).
    Used to message the saved owner: 'AutoCua online at …' + a fresh provider
    picker — so the user doesn't have to send anything to get going."""
    owner_id = _resolve_owner_chat_id()
    if not owner_id:
        # Not saved yet — try to auto-discover from Telegram's pending updates.
        # Works if the user has ever messaged the bot, even before the
        # chat-saving code was deployed. Persist the result so we don't need
        # to re-discover on every boot.
        owner_id = await _discover_owner_from_updates(application.bot)
        if owner_id:
            try:
                _save_owner_chat_id(owner_id)
                logger.info(
                    "owner discovery: saved chat_id=%s from getUpdates",
                    owner_id,
                )
            except Exception:
                logger.warning("owner discovery: could not persist chat_id", exc_info=True)
    if not owner_id:
        # No owner anywhere — they've never interacted with the bot. Stay
        # silent; they'll register themselves with /start.
        return
    bot = application.bot
    providers = _get_available_providers()
    try:
        await bot.send_message(chat_id=owner_id, text=_build_online_text(providers))
    except Exception:
        logger.exception("startup announcement: failed to send hello")
        return  # if we can't even greet, don't bother with the picker

    if not providers:
        try:
            await bot.send_message(
                chat_id=owner_id,
                text="⚠️ No provider API keys found. Add at least one to api_key.txt and /reset.",
            )
        except Exception:
            pass
        return

    buttons = [
        [InlineKeyboardButton(p["id"], callback_data=f"provider:{p['id']}")]
        for p in providers
    ]
    try:
        await bot.send_message(
            chat_id=owner_id,
            text="👋 Pick a provider:",
            reply_markup=InlineKeyboardMarkup(buttons),
        )
        # Park the owner's chat in pick_provider so the next button tap routes
        # cleanly through the existing callback flow.
        _chat_state[owner_id] = {"phase": "pick_provider"}
    except Exception:
        logger.exception("startup announcement: failed to send provider picker")


async def start_cmd(update, ctx):
    chat_id = update.effective_chat.id
    # Only the owner's chat is obeyed; the first chat to talk to the bot
    # claims it (and is remembered so future boots can auto-greet it).
    if not _is_owner_chat(chat_id):
        return
    _chat_state[chat_id] = {"phase": "pick_provider"}
    ok = await _show_provider_picker(update.message)
    if not ok:
        _chat_state[chat_id] = {"phase": "idle"}


async def reset_cmd(update, ctx):
    if not _is_owner_chat(update.effective_chat.id):
        return
    # Wipe state for this chat — including any queued tasks, pending prompts,
    # and the retained agent memory ("history"). We do NOT clear the persisted
    # owner chat_id; /reset is "start over the conversation", not "forget I
    # exist". (/start replaces the dict the same way, so it wipes memory too.)
    _chat_state[update.effective_chat.id] = {"phase": "idle"}
    await update.message.reply_text(
        "🔄 Reset. Previous session memory cleared. "
        "Send any message to pick a provider again."
    )


async def text_handler(update, ctx):
    chat_id = update.effective_chat.id
    if not _is_owner_chat(chat_id):
        return
    state = _state(chat_id)
    phase = state.get("phase", "idle")

    if phase in ("idle", "pick_provider"):
        state["phase"] = "pick_provider"
        ok = await _show_provider_picker(update.message)
        if not ok:
            state["phase"] = "idle"
        return

    if phase == "pick_model":
        await update.message.reply_text(
            "Pick a model from the buttons above first."
        )
        return

    if phase == "running":
        # Busy — offer to queue this task. Each pending prompt gets a unique
        # id so multiple "queue this?" prompts can coexist if the user spams.
        task = (update.message.text or "").strip()
        if not task:
            return
        state.setdefault("pending", {})
        state["pending_counter"] = state.get("pending_counter", 0) + 1
        pending_id = str(state["pending_counter"])
        state["pending"][pending_id] = task
        buttons = [[
            InlineKeyboardButton("✅ Yes, queue it", callback_data=f"q+:{pending_id}"),
            InlineKeyboardButton("❌ No",            callback_data=f"q-:{pending_id}"),
        ]]
        await update.message.reply_text(
            f"⏳ Currently busy performing a task.\n"
            f"Do you want to queue: \"{task[:200]}\" ?",
            reply_markup=InlineKeyboardMarkup(buttons),
        )
        return

    # phase == "ready"
    task = (update.message.text or "").strip()
    if not task:
        return
    if state.get("history"):
        # Memory from a previous session exists — ask before running. Phase
        # stays "ready"; the task is parked until the user answers. Multiple
        # unanswered prompts can coexist (same pattern as "pending"); the
        # loser of an answer race gets queued by the c+/c- handler.
        state.setdefault("pending_continue", {})
        state["pending_counter"] = state.get("pending_counter", 0) + 1
        pending_id = str(state["pending_counter"])
        state["pending_continue"][pending_id] = task
        done_hint = (state["history"].get("done_message") or "")[:120]
        buttons = [
            [InlineKeyboardButton("🧠 Continue previous session", callback_data=f"c+:{pending_id}")],
            [InlineKeyboardButton("🆕 Start fresh",               callback_data=f"c-:{pending_id}")],
        ]
        await update.message.reply_text(
            f"I still remember the previous session ({done_hint}).\n"
            f"Run \"{task[:200]}\" as a continuation, or start fresh?",
            reply_markup=InlineKeyboardMarkup(buttons),
        )
        return
    state["phase"] = "running"
    provider = state["provider"]
    model = state["model"]
    display = state.get("model_display", model)
    await update.message.reply_text(
        f"📝 Running: {task}  ({provider} · {display})"
    )
    bot = ctx.bot
    loop = asyncio.get_running_loop()
    threading.Thread(
        target=_run_agent,
        args=(task, provider, model, chat_id, bot, loop, None, state),
        daemon=True,
    ).start()


async def callback_handler(update, ctx):
    query = update.callback_query
    await query.answer()
    chat_id = query.message.chat_id
    if not _is_owner_chat(chat_id):
        return
    state = _state(chat_id)
    data = query.data or ""

    if data.startswith("provider:"):
        provider_id = data.split(":", 1)[1]
        state["provider"] = provider_id
        state["phase"] = "pick_model"
        models = _get_models_for_provider(provider_id)
        if not models:
            state["phase"] = "pick_provider"
            await query.edit_message_text(
                f"⚠️ No models found for {provider_id}. Pick another provider."
            )
            return
        buttons = [
            [InlineKeyboardButton(m["display_name"], callback_data=f"model:{m['id']}")]
            for m in models
        ]
        await query.edit_message_text(
            f"Pick a model for {provider_id}:",
            reply_markup=InlineKeyboardMarkup(buttons),
        )
        return

    if data.startswith("model:"):
        model_id = data.split(":", 1)[1]
        provider_id = state.get("provider")
        if not provider_id:
            state["phase"] = "idle"
            await query.edit_message_text("Session expired. Send any message to start over.")
            return
        models = _get_models_for_provider(provider_id)
        display = next(
            (m["display_name"] for m in models if m["id"] == model_id), model_id
        )
        state["model"] = model_id
        state["model_display"] = display
        state["phase"] = "ready"
        await query.edit_message_text(
            f"✅ Provider: {provider_id} / Model: {display}\n"
            f"Send me a task whenever you're ready."
        )
        return

    if data.startswith("q+:"):
        # User wants to queue the pending task.
        pending_id = data.split(":", 1)[1]
        task = (state.get("pending") or {}).pop(pending_id, None)
        if not task:
            await query.edit_message_text("(That prompt has already been handled.)")
            return
        state.setdefault("queue", []).append(task)
        qlen = len(state["queue"])
        await query.edit_message_text(
            f"📥 Queued (position {qlen}): \"{task[:200]}\"\n"
            f"Will run when the current task finishes."
        )
        # Edge case: agent finished in the milliseconds between the prompt
        # being sent and the user tapping Yes. Drain the queue now so the
        # queued task isn't stranded.
        _maybe_run_next_queued(chat_id, ctx.bot, asyncio.get_running_loop())
        return

    if data.startswith("q-:"):
        # User declines to queue. Drop the pending task.
        pending_id = data.split(":", 1)[1]
        (state.get("pending") or {}).pop(pending_id, None)
        await query.edit_message_text(
            "👍 OK, won't queue it. I'll let you know once the current task is done."
        )
        return

    if data.startswith("c+:") or data.startswith("c-:"):
        # Continue-vs-fresh decision for a task sent while idle with memory.
        # After /reset the fresh state dict has no pending_continue, so the
        # pop returns None and the stale tap self-heals like q+.
        pending_id = data.split(":", 1)[1]
        task = (state.get("pending_continue") or {}).pop(pending_id, None)
        if not task:
            await query.edit_message_text("(That prompt has already been handled.)")
            return
        continue_prev = data.startswith("c+:")
        # Unlike q+, check-and-transition goes under the lock: the prompt→tap
        # window is seconds long, so a concurrently finishing agent's finally
        # can drain a queued task in between — check-then-set without the
        # lock could double-run two agents.
        with _state_lock:
            if not continue_prev:
                state["history"] = None  # Start fresh → destroy old context
            provider = state.get("provider")
            model = state.get("model")
            history = state.get("history")  # None on fresh
            expired = not provider or not model
            queued = False
            if not expired:
                if state.get("phase") == "ready":
                    state["phase"] = "running"
                else:
                    # Race: something started running between the prompt and
                    # this tap (second prompt answered first, or a queued task
                    # drained). Fall back to queueing — same recovery as q+.
                    state.setdefault("queue", []).append(task)
                    queued = True
        if expired:
            state["phase"] = "idle"
            await query.edit_message_text("Session expired. Send any message to start over.")
            return
        if queued:
            try:
                await query.edit_message_text(
                    f"⏳ Another task started in the meantime — queued: \"{task[:200]}\""
                )
            except Exception:
                logger.warning("could not edit continue/fresh prompt", exc_info=True)
            # Cover the agent-finished-in-the-milliseconds gap, like q+.
            _maybe_run_next_queued(chat_id, ctx.bot, asyncio.get_running_loop())
            return
        display = state.get("model_display") or model
        label = "continuing previous session" if continue_prev else "fresh session"
        # The phase is already flipped to "running", so a failed edit must NOT
        # abort the handler — the thread below is the only thing that will
        # ever flip it back. A Telegram hiccup (timeout, flood control) here
        # would otherwise brick the chat in a permanent "busy" state.
        try:
            await query.edit_message_text(
                f"📝 Running ({label}): {task[:200]}  ({provider} · {display})"
            )
        except Exception:
            logger.warning("could not edit continue/fresh prompt", exc_info=True)
        threading.Thread(
            target=_run_agent,
            args=(task, provider, model, chat_id, ctx.bot,
                  asyncio.get_running_loop(), history, state),
            daemon=True,
        ).start()
        return


# ── scratchpad streaming ─────────────────────────────────────────────────────

def _send_chat(bot, chat_id, text, loop, wait: bool = False, timeout: float = 5.0):
    """Schedule a bot.send_message on the asyncio loop from a worker thread.
    Silently ignores failures so a transient send error never kills the
    monitor thread.

    When wait=True, block the calling thread until the send actually
    completes (or `timeout` seconds elapse). Used for terminal messages
    like "✅ Done." that must land in the chat BEFORE the next message
    is scheduled — without it, the "Done" send and the "Running queued
    task" send race inside the asyncio loop as two parallel HTTP POSTs
    and Telegram can deliver them out of order."""
    try:
        fut = asyncio.run_coroutine_threadsafe(
            bot.send_message(chat_id=chat_id, text=text), loop
        )
        if wait:
            try:
                fut.result(timeout=timeout)
            except Exception:
                logger.warning(
                    "send_message to chat %s did not confirm within %ss",
                    chat_id, timeout, exc_info=True,
                )
    except Exception:
        logger.warning("Failed to schedule send_message to chat %s", chat_id)


def _monitor_scratchpad(chat_id, bot, loop, stop_event, start_pos):
    """Tail SCRATCHPAD_PATH and forward each new non-empty line to the chat.

    Polls every SCRATCHPAD_POLL_SEC seconds. start_pos is the byte offset
    the file was at when the task began — we only forward content written
    AFTER that, so old milestones from previous tasks aren't replayed.
    Exits when stop_event is set, after one final sweep to flush any tail.
    """
    last_pos = start_pos

    def _read_and_forward():
        nonlocal last_pos
        if not SCRATCHPAD_PATH.exists():
            # File was deleted (e.g. AgentService.__init__ wiping the
            # scratchpad). Reset so the next poll re-reads the whole new
            # file from the top instead of seeking past its end.
            last_pos = 0
            return
        try:
            # Defensive: if the file shrank below last_pos it was truncated
            # or rotated; restart from byte 0 so we don't slice into the
            # middle of fresh content and stream a fragment.
            try:
                current_size = SCRATCHPAD_PATH.stat().st_size
                if current_size < last_pos:
                    last_pos = 0
            except Exception:
                pass
            with open(SCRATCHPAD_PATH, "r", encoding="utf-8", errors="replace") as f:
                f.seek(last_pos)
                new_content = f.read()
                if not new_content:
                    return
                last_pos = f.tell()
        except Exception as exc:
            logger.warning("Scratchpad read error: %s", exc)
            return
        for raw in new_content.splitlines():
            line = raw.strip()
            if not line:
                continue
            # Chunk excessively long lines so we stay under Telegram's 4096 cap.
            for i in range(0, len(line), MAX_TG_MSG_LEN):
                _send_chat(bot, chat_id, line[i : i + MAX_TG_MSG_LEN], loop)

    while not stop_event.is_set():
        _read_and_forward()
        stop_event.wait(SCRATCHPAD_POLL_SEC)

    # Final sweep — catches any line written between the last poll and the
    # stop_event being set (e.g. the agent's very last milestone).
    _read_and_forward()


# ── agent runner (worker thread) ─────────────────────────────────────────────

def _run_agent(task, provider, model, chat_id, bot, loop, prior_history=None,
               run_state=None):
    """Run the agent and ping the chat when done. Streams scratchpad milestones
    back to the chat live while the agent works. Pops a compact pill so the
    Windows user can see a Telegram task is running, and overlays the main app
    window with a "Currently occupied by Telegram" state. Restores phase to
    'ready'. prior_history seeds the agent with the previous run's memory
    (continue-session flow); the finished run's memory is captured back into
    _chat_state["history"] for the next task. run_state is the chat's state
    dict at spawn time — the finally block only writes back if it is still
    the live dict (identity check), so a /reset mid-run can't be undone by
    this run finishing later."""
    # Compact "Telegram task in progress" indicator + "occupied" overlay on
    # the main window. Both are best-effort — never let UI fluff block the
    # actual task.
    from AutoCua.windows.remote_connection.banner import StatusBanner, CoderBannerManager
    task_banner = StatusBanner(compact=True)
    try:
        task_banner.show()
    except Exception:
        logger.warning("could not show task banner", exc_info=True)
    # Expands the orb pill into an embedded terminal panel (streamed lines +
    # todo checklist + minion spinner rows) while the main agent is halted in
    # cli_await, then collapses back. Drives the same task_banner. Best-effort.
    coder_mgr = CoderBannerManager(task_banner)
    # Fade the AutoCua UI away and show the Telegram "occupied" overlay (orb +
    # message) on the main window instead of minimising it. We talk to pywebview
    # directly via its global `windows` list rather than importing from app.py —
    # `python app.py` makes app.py the __main__ module, so `from app import …`
    # would re-import a *second* copy of app.py whose webview_window is still
    # None, and the call would silently no-op. evaluate_js from this worker
    # thread is the same path app.py's send_*_to_frontend helpers already use.
    try:
        import webview as _webview
        if _webview.windows:
            _webview.windows[0].evaluate_js(
                "window.telegramOccupiedShow && window.telegramOccupiedShow()")
    except Exception:
        logger.warning("could not show Telegram occupied overlay", exc_info=True)

    # Reset the milestone scratchpad to empty before starting the monitor.
    # AgentService.__init__ wipes the entire scratchpad/ directory in
    # _cleanup_scratchpad() — so if we snapshotted the file's current size
    # here and the agent then deleted + rewrote it, the monitor's last_pos
    # would point mid-way into the fresh content and we'd stream a
    # fragment (e.g. "ome." instead of "Verified: …Edge.") to the chat.
    # Deleting the file ourselves up front and starting from byte 0 keeps
    # the monitor aligned with whatever the agent writes next. Best-effort
    # — a failure here just degrades us back to the old (buggy) behavior.
    try:
        if SCRATCHPAD_PATH.exists():
            SCRATCHPAD_PATH.unlink()
    except Exception:
        logger.warning("could not reset milestone scratchpad", exc_info=True)
    start_pos = 0
    agent = None
    run_status = "error"
    run_message = ""
    stop_event = threading.Event()
    monitor = threading.Thread(
        target=_monitor_scratchpad,
        args=(chat_id, bot, loop, stop_event, start_pos),
        daemon=True,
        name=f"telegram-scratchpad-{chat_id}",
    )
    monitor.start()

    try:
        # Imported lazily — pulls in tree/element → skimage etc., which we
        # don't want to load until a task actually runs.
        from AutoCua.windows.agent.main_driver.service import AgentService

        # Look up the runtime API key for the chosen provider so
        # LLMManager doesn't fall back to an os.getenv() that the user
        # never set. Mirrors app.py's get_provider_api_key path —
        # Telegram users edit api_key.txt (or use the AutoCua Settings
        # panel), they don't export env vars, so without passing
        # `api_key=` here the agent dies before its first scan with
        # "X API key not provided and not found in .env file".
        # _get_available_providers already gated the picker to non-
        # empty keys, so the lookup is guaranteed to return a value.
        provider_key_name = PROVIDER_KEY_MAP.get(provider)
        provider_keys = _read_all_keys(_API_KEY_FILE)
        provider_api_key = (
            provider_keys.get(provider_key_name) if provider_key_name else None
        )

        # Pipe each step's formatted response (thinking + next_goal +
        # memory, with action stripped) into the compact banner.
        # The agent already calls text_callback at
        # main_driver/service.py with exactly this content — same path the
        # frontend's streamAgentText uses in app.py. update() forwards via
        # the MSG stdin command to the banner subprocess; the call returns
        # quickly so the agent loop never blocks on it.
        def _banner_update(text: str) -> None:
            try:
                task_banner.update(text)
            except Exception:
                logger.warning("banner.update failed", exc_info=True)

        agent = AgentService(
            provider=provider,
            model=model,
            save_conversation=False,
            api_key=provider_api_key,
            text_callback=_banner_update,
            cli_callback=coder_mgr.handle_event,
            prior_history=prior_history,
        )
        # process_request returns {"status", "message"}: "success" only when a
        # `done` action ran, "error" on a critical failure (e.g. the API key is
        # wrong and every attempt 401s), "incomplete" otherwise. Without this we
        # always sent "✅ Done." even when the agent never actually ran.
        result = agent.process_request(task)
        # Stop the monitor BEFORE the done message so the final scratchpad
        # sweep happens first — keeps the chat in correct chronological order.
        stop_event.set()
        monitor.join(timeout=SCRATCHPAD_POLL_SEC + 2)

        run_status = result.get("status") if isinstance(result, dict) else "success"
        run_message = (result.get("message") if isinstance(result, dict) else "") or ""
        status = run_status
        message = run_message
        if len(message) > 400:  # keep the phone message readable
            message = message[:400] + "…"

        # wait=True: block until the terminal message is on Telegram's servers
        # before the finally-block fires _maybe_run_next_queued, which would
        # otherwise schedule "📝 Running queued task: …" as a second,
        # concurrent HTTP POST that can race past it in delivery.
        if status == "success":
            _send_chat(bot, chat_id, "✅ Done.", loop, wait=True)
        elif status == "error":
            _send_chat(bot, chat_id, f"❌ Error: {message}", loop, wait=True)
        else:  # incomplete
            _send_chat(bot, chat_id, f"⚠️ Stopped without completing: {message}", loop, wait=True)
    except Exception as e:
        logger.exception("agent error")
        run_message = str(e)
        stop_event.set()
        monitor.join(timeout=SCRATCHPAD_POLL_SEC + 2)
        _send_chat(bot, chat_id, f"❌ Error: {e}", loop, wait=True)
    finally:
        if not stop_event.is_set():
            stop_event.set()
        try:
            task_banner.close()
        except Exception:
            pass
        try:
            coder_mgr.close_all()
        except Exception:
            pass
        # Restore the main UI — hide the "occupied" overlay. If a queued task
        # follows, its _run_agent start re-shows it almost immediately (at worst
        # a brief flicker between back-to-back tasks).
        try:
            import webview as _webview
            if _webview.windows:
                _webview.windows[0].evaluate_js(
                    "window.telegramOccupiedHide && window.telegramOccupiedHide()")
        except Exception:
            pass
        # Capture this run's memory for the next task — on EVERY ending
        # (success / error / incomplete / exception), matching the desktop
        # save_run semantics. Built outside the lock (pure function).
        new_history = None
        try:
            if agent is not None:
                new_history = _build_prior_history(
                    task, agent.assistant_messages, agent.tool_responses,
                    run_status, run_message,
                    prior_task=(prior_history or {}).get("task"),
                    sub_agent_last_id=getattr(agent, "sub_agent_last_id", None),
                )
        except Exception:
            logger.warning("could not capture agent memory", exc_info=True)
        with _state_lock:
            state = _chat_state.get(chat_id)
            # "state is run_state" gates on THIS run still owning the chat:
            # /reset and /start replace the whole dict, and the user may have
            # already started a NEW run on the fresh dict while this zombie
            # was finishing — its phase=="running" belongs to that run, so
            # writing here would resurrect wiped memory into the new session
            # and prematurely flip the new run's phase. new_history is None
            # when the run produced no steps (ctor failure, crash before
            # step 1) — keep the previous memory instead of wiping it.
            if state is not None and state is run_state and state.get("phase") == "running":
                if new_history is not None:
                    state["history"] = new_history
                state["phase"] = "ready"
        # Drain one queued task if any — keeps phase='running' if it spawns.
        _maybe_run_next_queued(chat_id, bot, loop)


# ── entry points ─────────────────────────────────────────────────────────────

async def _on_error(update, context):
    err = context.error
    # Benign: user tapped the same inline button twice, so the edit produces
    # identical content. Telegram rejects it; swallow quietly.
    if isinstance(err, BadRequest) and "Message is not modified" in str(err):
        return
    logger.error("Unhandled exception in telegram handler", exc_info=err)


# ── TLS verification for the Telegram connection ──────────────────────────
# Some machines run HTTPS interception (antivirus / corporate proxy) whose
# root cert isn't in certifi, or has malformed Basic Constraints that fail
# OpenSSL's strict validation. There, verification genuinely cannot succeed
# and the bot dies with "CERTIFICATE_VERIFY_FAILED: unable to get local issuer
# certificate". We probe once at startup, prefer secure verification, and fall
# back to verify=False (with a loud warning) ONLY when nothing else works — so
# machines without interception stay fully secure.
_SSL_VERIFY_UNSET = object()
_SSL_VERIFY = _SSL_VERIFY_UNSET


def _telegram_ssl_verify():
    """Return the httpx `verify` setting for talking to api.telegram.org.

    Probes (cached): certifi bundle → OS trust store → verify=False."""
    global _SSL_VERIFY
    if _SSL_VERIFY is not _SSL_VERIFY_UNSET:
        return _SSL_VERIFY
    import httpx
    probe = "https://api.telegram.org"
    candidates = []
    try:
        import certifi
        candidates.append(certifi.where())
    except Exception:
        pass
    try:
        import ssl
        candidates.append(ssl.create_default_context())
    except Exception:
        pass
    for verify in candidates:
        try:
            httpx.get(probe, verify=verify, timeout=6)
            _SSL_VERIFY = verify
            return verify
        except Exception:
            continue
    _stderr(
        "WARNING: can't verify TLS to Telegram — your machine appears to run "
        "HTTPS interception (antivirus / corporate proxy) with a certificate "
        "Python rejects. Connecting WITHOUT certificate verification so the "
        "bot works. To restore secure verification, turn off HTTPS/SSL "
        "scanning for api.telegram.org in your antivirus or proxy."
    )
    _SSL_VERIFY = False
    return False


def _build_telegram_app(token: str):
    """Build a python-telegram-bot Application with all our handlers wired.

    `post_init` is the hook python-telegram-bot calls once after the bot
    finishes initialising but before polling starts — perfect spot to send
    the "AutoCua online" announcement + provider picker to the saved owner.
    """
    # Resolve TLS verification once (handles antivirus / proxy TLS
    # interception). The bot and the long-poll updater each need their OWN
    # request object; the updater's read_timeout must exceed the poll timeout
    # (default 10 s) so long polling doesn't time out mid-wait.
    verify = _telegram_ssl_verify()
    app = (
        Application.builder()
        .token(token)
        .request(HTTPXRequest(httpx_kwargs={"verify": verify}))
        .get_updates_request(
            HTTPXRequest(read_timeout=20, httpx_kwargs={"verify": verify})
        )
        .post_init(_post_init)
        .build()
    )
    app.add_error_handler(_on_error)
    app.add_handler(CommandHandler("start", start_cmd))
    app.add_handler(CommandHandler("reset", reset_cmd))
    app.add_handler(CallbackQueryHandler(callback_handler))
    app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, text_handler))
    return app


_BOT_THREAD: threading.Thread | None = None


def _stderr(msg: str) -> None:
    """Loud print to the terminal where python app.py is running — bypasses
    whatever logging config is in effect so the user actually sees it."""
    import sys
    print(f"[telegram] {msg}", file=sys.stderr, flush=True)


async def _run_bot_until_stopped(tg_app):
    """Manual lifecycle replacement for Application.run_polling().

    run_polling() messes with signals and assumes it owns the main thread;
    we want to drive it from a worker thread so we do it step by step.

    Order matches what run_polling() does internally:
      initialize → start → post_init → start_polling.
    We call _post_init BEFORE start_polling so its bot.get_updates(offset=-1)
    auto-discovery doesn't race with the updater's own polling loop.
    """
    await tg_app.initialize()
    await tg_app.start()
    # Application.post_init() is only invoked by run_polling(), not by the
    # manual initialize+start path above. Call our startup announcement
    # explicitly so the saved owner gets the "AutoCua online" message.
    try:
        await _post_init(tg_app)
    except Exception:
        logger.exception("post_init failed")
    await tg_app.updater.start_polling(allowed_updates=Update.ALL_TYPES)
    _stderr("polling loop is live — send your bot a message")
    # Park here forever (daemon thread; killed on app exit).
    await asyncio.Event().wait()


def start_bot() -> None:
    """Start the Telegram bot polling on a daemon thread.

    Idempotent — safe to call multiple times from app.py boot. Prints loudly
    to stderr at each milestone so the user can see what's happening.
    """
    global _BOT_THREAD
    if _BOT_THREAD is not None and _BOT_THREAD.is_alive():
        _stderr("start_bot() called but the bot is already running — skipping.")
        return
    token = _resolve_token()
    if not token:
        _stderr(
            "BOT NOT STARTED — TELEGRAM_BOT_TOKEN not found in env, .env, or "
            "api_key.txt. Paste your @BotFather token into one of those files."
        )
        return
    # Never include any part of the token in user-facing output: in the compiled
    # build a stray bot-start in a sub-agent child gets its stderr streamed onto
    # the on-screen pill, so even a 6-char suffix would leak.
    _stderr("starting bot")

    def _runner():
        import sys, traceback
        try:
            # Each thread needs its own asyncio event loop. Without this, the
            # call to asyncio.Event() inside _run_bot_until_stopped fails.
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            tg_app = _build_telegram_app(token)
            try:
                loop.run_until_complete(_run_bot_until_stopped(tg_app))
            finally:
                loop.close()
        except Exception as e:
            _stderr(f"BOT CRASHED: {e!r}")
            traceback.print_exc(file=sys.stderr)

    _BOT_THREAD = threading.Thread(target=_runner, daemon=True, name="telegram-bot")
    _BOT_THREAD.start()


def main():
    """Standalone entry — for testing without launching the full AutoCua app."""
    token = _resolve_token()
    if not token:
        raise SystemExit(
            f"TELEGRAM_BOT_TOKEN not found in {_API_KEY_FILE}\n"
            "(create the bot via @BotFather first, then add the token to that file)."
        )
    tg_app = _build_telegram_app(token)
    logger.info("Telegram bot polling started (main thread)")
    tg_app.run_polling(allowed_updates=Update.ALL_TYPES)


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    main()
