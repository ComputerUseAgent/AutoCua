"""Telegram → AgentService bridge with a guided provider/model picker.

Runs as a standalone process (not mounted into Flask). On the first message
the bot asks you to pick a provider (limited to providers with a non-empty
key in the .env, and Ollama while it runs, which needs none), then a model
(from the same MODEL_MAPPINGS the AutoCua frontend uses; Ollama's also lists
the models pulled there). Subsequent messages are dispatched as tasks to the
agent with that provider/model. Picked provider/model persist for the whole
chat session until you `/reset`.

The token is TELEGRAM_BOT_TOKEN in the .env (AutoCua.env_file), and only
there: env vars are not read.

Setup:
  1. @BotFather → /newbot → copy token.
  2. Paste it into the .env as TELEGRAM_BOT_TOKEN=…
  3. Make sure at least one provider key (e.g. OPENROUTER_API_KEY=…) is set.
  4. python -m AutoCua.linux.remote_connection.telegram.service
  5. Send your bot the pairing code main() prints (the app shows it in
     Settings > Remote Connection). Only that chat is obeyed.
"""
import asyncio
import concurrent.futures
import datetime
import importlib
import json
import logging
import secrets
import sys
import threading
import time
from pathlib import Path

from telegram import Update, InlineKeyboardButton, InlineKeyboardMarkup
from telegram.constants import ChatType
from telegram.error import BadRequest, NetworkError
from telegram.request import HTTPXRequest
from telegram.ext import (
    Application,
    CommandHandler,
    MessageHandler,
    CallbackQueryHandler,
    filters,
)

logger = logging.getLogger(__name__)

# The Telegram surface reads ONE file, the .env (AutoCua.env_file), and never
# env vars: the token and the providers it offers are what that file holds.
# It is the file the Settings panel writes and the agent's llm_provider
# reads; AutoCua/__init__.py resolves it for the whole app, so they cannot
# drift onto different files.
from AutoCua import env_file, api_key_lock, read_env, set_env

_ENV_FILE = env_file()

# Agent writes per-step "milestone" lines here. We tail this file during a
# task and forward each new line back to the user's Telegram chat so they
# see the agent's progress in real time.
SCRATCHPAD_PATH = (
    Path(__file__).resolve().parents[2] / "scratchpad" / "milestone" / "milestone.md"
)
SCRATCHPAD_POLL_SEC = 2.0
MAX_TG_MSG_LEN = 4000  # Telegram caps at 4096; leave headroom for safety

# Provider id → API-key name in the KV files. Same mapping the Windows side
# uses ([windows/remote_connection/telegram/service.py:44-51]).
PROVIDER_KEY_MAP = {
    "openrouter": "OPENROUTER_API_KEY",
    "groq":       "GROQ_API_KEY",
    "openai":     "OPENAI_API_KEY",
    "anthropic":  "ANTHROPIC_API_KEY",
    "google":     "GOOGLE_API_KEY",
    "perplexity": "PERPLEXITY_API_KEY",
    "together":   "TOGETHER_API_KEY",
    "cerebras":   "CEREBRAS_API_KEY",
    "requesty":   "REQUESTY_API_KEY",
    "litellm":    "LITELLM_API_KEY",
    "aws":        "AWS_API_KEY",
}

# Providers that run without a key (a model on this machine), offered with
# none saved. Kept out of PROVIDER_KEY_MAP, whose values are key names.
KEYLESS_PROVIDERS = ("ollama",)


# ── file helpers ─────────────────────────────────────────────────────────────

def _read_all_keys(path: Path) -> dict:
    """The .env's names that have a value (AutoCua.read_env: quotes, export
    and comments read as load_dotenv reads them)."""
    try:
        return {k: v for k, v in read_env(path).items() if v}
    except Exception:
        return {}


def _resolve_token() -> str | None:
    """TELEGRAM_BOT_TOKEN from the .env only. Env vars are intentionally
    ignored, see the header comment."""
    return _read_all_keys(_ENV_FILE).get("TELEGRAM_BOT_TOKEN")


def _get_available_providers() -> list:
    """Providers with a non-empty key in the .env, then the keyless ones that
    answer right now (Ollama while it runs), so a person with neither still
    gets the add-a-key guidance."""
    keys = _read_all_keys(_ENV_FILE)
    return [
        {"id": pid, "key": keys[kname]}
        for pid, kname in PROVIDER_KEY_MAP.items()
        if keys.get(kname)
    ] + [{"id": pid, "key": None} for pid in KEYLESS_PROVIDERS if _keyless_answers(pid)]


def _keyless_answers(provider_id: str) -> bool:
    """True when the keyless provider's server answers (its service's
    reachable(): a quick local call)."""
    try:
        return bool(importlib.import_module(
            f"AutoCua.llm_provider.{provider_id}.service").reachable())
    except Exception:
        return False


# One key-file write at a time: pairing, Connect, Disconnect and a new code
# each read the file, change it and write it back. The same lock as the
# Settings key routes (ui/service.py), so neither drops the other's change.
_key_lock = api_key_lock


def _set_keys_in_file(path: Path, updates: dict) -> None:
    """Set KEY=value for every item of `updates` in ONE rewrite of the .env,
    keeping every other line as it is (AutoCua.set_env). The new file is
    written next to the old one and renamed over it, so a reader, a crash or
    a full disk meets the old file or the new one. A failure is logged; the
    callers read the file back to see what was saved."""
    with _key_lock:
        try:
            set_env(updates, path)
        except Exception:
            logger.warning("failed to write %s", path)


def _resolve_owner_chat_id() -> int | None:
    """Owner chat_id = the private chat that paired the bot with its code
    (_pairing_step). Stored in the .env as TELEGRAM_OWNER_CHAT_ID=…, so it
    survives restarts. Missing, unreadable or not a private chat (a group or
    channel id is negative) means no owner, and with no owner nobody is obeyed."""
    val = _read_all_keys(_ENV_FILE).get("TELEGRAM_OWNER_CHAT_ID")
    try:
        owner = int(val)
    except (TypeError, ValueError):
        return None
    return owner if owner > 0 else None


def _is_owner_chat(chat_id: int) -> bool:
    """Only the owner's chat is obeyed. Nothing here claims the bot: only the
    pairing code shown on the computer does (_pairing_step). Disconnect in the
    app clears the owner and the token."""
    if not _resolve_token():
        return False  # disconnected: the bot keeps polling until restart, obeying nobody
    owner = _resolve_owner_chat_id()
    if owner is None:
        return False  # not paired, or the owner line is lost: obey nobody
    if chat_id != owner:
        logger.info("ignored an update from a chat that is not the owner")
    return chat_id == owner


# ── pairing ──────────────────────────────────────────────────────────────────
# The bot obeys a chat only after that chat sends the one-time code that
# Connect (setup.py) and Settings > Remote Connection show on the computer.
# 12 characters from 30 (about 59 bits): guessing is hopeless even with no
# time limit, so a wrong try does not burn the code (a stranger cannot use
# it up). Pairing deletes it.
_PAIR_ALPHABET = "23456789ABCDEFGHJKMNPQRSTVWXYZ"   # no 0 O 1 I L U: easy to read and type


def _new_pair_code() -> str:
    """XXXX-XXXX-XXXX: fits a t.me ?start= link (A-Z, 0-9 and '-', 64 at most)."""
    raw = "".join(secrets.choice(_PAIR_ALPHABET) for _ in range(12))
    return f"{raw[:4]}-{raw[4:8]}-{raw[8:]}"


def _clean_code(s) -> str:
    return "".join(c for c in (s or "").upper() if c.isascii() and c.isalnum())


def _valid_pair_code(code) -> bool:
    """12 characters from _PAIR_ALPHABET, as _new_pair_code makes them. Anything
    else (a hand edit, a cut-off line) never pairs and is replaced."""
    c = _clean_code(code)
    return len(c) == 12 and all(ch in _PAIR_ALPHABET for ch in c)


def _codes_match(text, code) -> bool:
    """Constant time. Case, spaces and dashes do not count, so a typed code
    works as well as the link. A malformed saved code never matches."""
    return _valid_pair_code(code) and secrets.compare_digest(_clean_code(text), _clean_code(code))


def _pair_code() -> str | None:
    """The code that pairs the bot while it has no owner: the saved one, or a
    new one saved now (a key file from before pairing existed, or an owner
    line lost later). None when the bot is paired or no code could be saved."""
    with _key_lock:
        if not _resolve_token() or _resolve_owner_chat_id() is not None:
            return None
        code = _read_all_keys(_ENV_FILE).get("TELEGRAM_PAIR_CODE")
        if not _valid_pair_code(code):
            code = _new_pair_code()
            _set_keys_in_file(_ENV_FILE, {"TELEGRAM_PAIR_CODE": code})
            if _read_all_keys(_ENV_FILE).get("TELEGRAM_PAIR_CODE") != code:
                return None
        return code


def _start_pairing(token: str) -> str | None:
    """Connect: save the token, forget the old owner and make a new code, all
    in one write. Returns the code to show, or None if it could not be saved."""
    code = _new_pair_code()
    with _key_lock:
        _set_keys_in_file(_ENV_FILE, {"TELEGRAM_BOT_TOKEN": token,
                                          "TELEGRAM_OWNER_CHAT_ID": "",
                                          "TELEGRAM_PAIR_CODE": code})
        keys = _read_all_keys(_ENV_FILE)
    if keys.get("TELEGRAM_BOT_TOKEN") == token and keys.get("TELEGRAM_PAIR_CODE") == code:
        return code
    return None


def _pair_if_code(chat_id: int, text) -> bool | None:
    """chat_id becomes the owner if `text` is the saved code: the owner is
    saved and the code deleted in one write. True paired, False not the code
    (or no code), None the file could not be written."""
    with _key_lock:
        code = _read_all_keys(_ENV_FILE).get("TELEGRAM_PAIR_CODE")
        if not code or not _codes_match(text, code):
            return False
        _set_keys_in_file(_ENV_FILE, {"TELEGRAM_OWNER_CHAT_ID": str(chat_id),
                                          "TELEGRAM_PAIR_CODE": ""})
        if _resolve_owner_chat_id() != chat_id:
            return None
    return True


def _get_models_for_provider(provider_id: str) -> list:
    """Read MODEL_MAPPINGS from AutoCua/llm_provider/<id>/view.py (or what
    its listed_models() returns, where it has one: Ollama's pulled models)
    and return non-hidden entries as [{id, display_name}, …]. An id too long
    for a button's 64-byte callback_data ("model:<id>") is left out, as
    Telegram would refuse the whole keyboard."""
    try:
        mod = importlib.import_module(
            f"AutoCua.llm_provider.{provider_id}.view"
        )
        listed = getattr(mod, "listed_models", None)
        mappings = listed() if callable(listed) else getattr(mod, "MODEL_MAPPINGS", {})
        return [
            {"id": mid, "display_name": info.get("display_name", mid)}
            for mid, info in mappings.items()
            if not info.get("hidden", False)
            and len(f"model:{mid}".encode("utf-8")) <= 64
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
#   "history":          dict | None,  # the app chat to go on with: {"session_id",
#                                     # "task", "done_message"}; the memory itself
#                                     # is that saved chat
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
    if not _is_owner_chat(chat_id):
        return  # disconnected since the task was queued: nobody is obeyed
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
            "OPENROUTER_API_KEY=…) to the .env and try again."
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


# When the bot came online. A message older than this waited on Telegram's
# servers while the app was closed: it can still pair, but it is never
# answered with the "not paired" line.
_online_since = None
# Chats already told that the bot is not paired, so a flood gets one reply.
_told_not_paired: set = set()


async def _pairing_step(update, text) -> bool:
    """While the bot has no owner: pair it to the private chat that sends the
    code, and obey nobody else (a stranger, a group, a channel, an edited
    message). True when the bot has no owner, so the update ends here; False
    when it is paired and the handler goes on as before."""
    if _resolve_owner_chat_id() is not None:
        return False
    msg, chat = update.message, update.effective_chat
    # The saved token must be the one this bot polls with: after Disconnect and
    # Connect with another bot, the old polling thread pairs nobody.
    if (_resolve_token() != update.get_bot().token or msg is None or chat is None
            or chat.type != ChatType.PRIVATE):
        return True
    paired = _pair_if_code(chat.id, text)
    if paired:
        _told_not_paired.clear()
        logger.info("paired with the chat that sent the code")
        _chat_state[chat.id] = {"phase": "pick_provider"}
        await msg.reply_text("✅ Paired. You can now control AutoCua from this Telegram account.")
        if not await _show_provider_picker(msg):
            _chat_state[chat.id] = {"phase": "idle"}
    elif paired is None:
        await msg.reply_text("❌ Could not save the pairing on the computer. Please try again.")
    elif chat.id not in _told_not_paired and (_online_since is None or msg.date >= _online_since):
        _told_not_paired.add(chat.id)
        await msg.reply_text(
            "This bot is not paired yet. On your computer, open AutoCua, go to "
            "Settings > Remote Connection, and send me the code shown there."
        )
    return True


async def _post_init(application) -> None:
    """Fires once after the bot finishes initialising (before polling starts).
    Used to message the saved owner: 'AutoCua online at …' + a fresh provider
    picker, so the user doesn't have to send anything to get going. With no
    owner nobody is greeted, and a waiting update is never taken as the owner."""
    global _online_since
    # Whole seconds, like a message's date.
    _online_since = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    owner_id = _resolve_owner_chat_id()
    if not owner_id:
        _stderr("not paired yet: send your bot the code shown in "
                "Settings > Remote Connection")
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
                text="⚠️ No provider API keys found. Add at least one to the .env and /reset.",
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
    # No owner yet: only the code pairs the bot ("/start CODE" is what a
    # t.me/<bot>?start=CODE link sends). Then only the owner's chat is obeyed.
    if await _pairing_step(update, " ".join(ctx.args or [])):
        return
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
    if await _pairing_step(update, update.message.text if update.message else ""):
        return
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
    # The chat to go on with is the app's; deleted there, there is nothing
    # to continue, so the task starts fresh and the question is not asked.
    history = state.get("history")
    if history:
        from AutoCua.agent_conversation.service import conversation
        if not conversation.get_session(history.get("session_id")):
            state["history"] = None
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

def _one_line(exc) -> str:
    """`NetworkError: httpx.ConnectError` — what happened, on one line. A
    stack through httpx and anyio says nothing more and fills the screen."""
    return f"{exc.__class__.__name__}: {exc}".strip().rstrip(":").strip()


async def _send_with_retry(bot, chat_id, text, tries: int = 3):
    """bot.send_message that rides out a momentary network failure.

    Telegram's API drops the odd connection on a laptop's Wi-Fi — a TLS
    connect error, a read that never comes — and each is over within a
    second or two. A reply the agent worked for must not be lost to one,
    so a NetworkError is retried with backoff (1.5 s, 3 s). Anything else
    is raised at once."""
    from telegram.error import NetworkError
    delay = 1.5
    for attempt in range(1, tries + 1):
        try:
            return await bot.send_message(chat_id=chat_id, text=text)
        except NetworkError as e:
            if attempt == tries:
                raise
            logger.warning("send_message to chat %s: %s — retrying in %.1fs (%d/%d)",
                           chat_id, _one_line(e), delay, attempt, tries)
            await asyncio.sleep(delay)
            delay *= 2


def _send_chat(bot, chat_id, text, loop, wait: bool = False, timeout: float = 20.0):
    """Schedule a bot.send_message on the asyncio loop from a worker thread.
    A failure is one warning line (after the retries in _send_with_retry),
    never a traceback, and never kills the calling thread.

    When wait=True, block the calling thread until the send actually
    completes (or `timeout` seconds elapse — long enough for the retries).
    Used for terminal messages like "✅ Done." that must land in the chat
    BEFORE the next message is scheduled — without it, the "Done" send and
    the "Running queued task" send race inside the asyncio loop as two
    parallel HTTP POSTs and Telegram can deliver them out of order."""
    try:
        fut = asyncio.run_coroutine_threadsafe(
            _send_with_retry(bot, chat_id, text), loop
        )
    except Exception as e:
        logger.warning("Failed to schedule send_message to chat %s: %s", chat_id, _one_line(e))
        return

    def _report(f):
        try:
            f.result()
        except Exception as e:
            logger.warning("send_message to chat %s failed: %s", chat_id, _one_line(e))

    fut.add_done_callback(_report)
    if wait:
        try:
            fut.result(timeout=timeout)
        except concurrent.futures.TimeoutError:
            logger.warning("send_message to chat %s not confirmed within %ss — still sending",
                           chat_id, timeout)
        except Exception:
            pass    # _report has said what happened


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
    Mac user can see a Telegram task is running, and the app shows the task as a
    message sent from its chat box, "sent via: Telegram" over it, and runs it
    like one (ui/service.py start_run_from_telegram). Restores phase to
    'ready'. prior_history names the chat to go on with (continue-session flow:
    its session_id); the finished run's chat and done line are kept in
    _chat_state["history"] for the next task. run_state is the chat's state
    dict at spawn time — the finally block only writes back if it is still
    the live dict (identity check), so a /reset mid-run can't be undone by
    this run finishing later."""
    # Compact "Telegram task in progress" indicator. Best-effort — never let
    # UI fluff block the actual task.
    from AutoCua.linux.remote_connection.banner import StatusBanner, CoderBannerManager
    task_banner = StatusBanner(compact=True)
    try:
        task_banner.show()
    except Exception:
        logger.warning("could not show task banner", exc_info=True)
    # Expands the orb pill into an embedded terminal panel (streamed lines +
    # todo checklist + minion spinner rows) while the main agent is halted in
    # cli_await, then collapses back. Drives the same task_banner. Best-effort.
    coder_mgr = CoderBannerManager(task_banner)

    # Reset the milestone scratchpad to empty before starting the monitor.
    # AgentService.__init__ wipes the entire scratchpad/ directory in
    # _cleanup_scratchpad() — so if we snapshotted the file's current size
    # here and the agent then deleted + rewrote it, the monitor's last_pos
    # would point mid-way into the fresh content and we'd stream a
    # fragment (e.g. "ome." instead of "Verified: …Chrome.") to the chat.
    # Deleting the file ourselves up front and starting from byte 0 keeps
    # the monitor aligned with whatever the agent writes next. Best-effort
    # — a failure here just degrades us back to the old (buggy) behavior.
    try:
        if SCRATCHPAD_PATH.exists():
            SCRATCHPAD_PATH.unlink()
    except Exception:
        logger.warning("could not reset milestone scratchpad", exc_info=True)
    start_pos = 0
    outcome = {}            # the app's ending: status, message, the chat's session_id
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
        # Pipe each step's formatted response (thinking + next_goal +
        # memory, with action stripped) into the compact banner.
        # The agent already calls text_callback at
        # computer_use/service.py:704-705 with exactly this content — same
        # path the frontend's streamAgentText uses in app.py. update()
        # serialises onto the banner's UI thread, so the agent loop never
        # blocks on it.
        def _banner_update(text: str) -> None:
            try:
                task_banner.update(text)
            except Exception:
                logger.warning("banner.update failed", exc_info=True)

        # The run goes through the app, as a message sent from its chat box
        # does: the page shows the task with "sent via: Telegram" over it, the
        # run's screens and reply, and keeps the chat. The bot only adds its
        # banner and the reply to the phone. The chat goes on from the last
        # run's (continue) or starts new (fresh): prior_history's session_id.
        from AutoCua.ui import service as app_service
        finished = threading.Event()

        def _done(run_outcome, session_id):
            outcome.update(run_outcome or {})
            outcome["session_id"] = session_id
            finished.set()

        code, started = app_service.start_run_from_telegram(
            task, provider, model,
            session_id=(prior_history or {}).get("session_id"),
            text_callback=_banner_update,
            cli_callback=coder_mgr.handle_event,
            on_done=_done,
        )
        if code != 200 or started.get("status") != "started":
            raise RuntimeError(started.get("error") or f"the app did not start the run ({code})")
        finished.wait()
        result = {"status": outcome.get("status") or "error", "message": outcome.get("message") or ""}
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
        # What the next task needs of this run: the app keeps the memory (the
        # saved chat); the bot keeps which chat to go on with, and how the run
        # ended for the continue / start fresh question. None when the run
        # never started: the previous chat stays.
        new_history = None
        if outcome.get("session_id"):
            if run_status == "success":
                done_message = f"Task completed: {run_message}"
            elif run_status == "error":
                done_message = f"Agent terminated (error / provider): {run_message}"
            else:
                done_message = f"Agent stopped before completing: {run_message}"
            new_history = {"session_id": outcome["session_id"], "task": task,
                           "done_message": done_message.strip().rstrip(":")}
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
    if isinstance(err, NetworkError):
        # A dropped connection mid-handler. Nothing to debug in the stack.
        logger.warning("Telegram unreachable in a handler (%s)", _one_line(err))
        return
    logger.error("Unhandled exception in telegram handler", exc_info=err)


def _on_polling_error(exc) -> None:
    """The updater's error callback. Left to itself the library prints a
    full traceback for every dropped connection while polling, then
    retries anyway; the retry is what matters, so a dropped connection is
    one line here. Anything that is not a network error keeps its trace."""
    if isinstance(exc, NetworkError):
        logger.warning("Telegram unreachable while polling (%s) — retrying", _one_line(exc))
    else:
        logger.error("Exception while polling for updates", exc_info=exc)


def _build_telegram_app(token: str):
    """Build a python-telegram-bot Application with all our handlers wired.

    `post_init` is the hook python-telegram-bot calls once after the bot
    finishes initialising but before polling starts — perfect spot to send
    the "AutoCua online" announcement + provider picker to the saved owner.
    """
    # IPv4 only. Measured on this laptop, eight connections to
    # api.telegram.org each way: over IPv4 all eight answered in 0.04-0.08 s;
    # over IPv6 one failed outright and the rest took 0.13-1.3 s. httpx tries
    # IPv6 first (Happy Eyeballs), and that is where the day's TimedOut,
    # ConnectError and ReadError all came from. Binding the client's local
    # address to 0.0.0.0 keeps it on IPv4. The timeouts are wider than the
    # library's 5 s defaults for Wi-Fi behind a VPN; get_updates long-polls
    # for 10 s, so its read timeout must sit above that.
    def _ipv4_request(**timeouts):
        import httpx
        return HTTPXRequest(
            httpx_kwargs={"transport": httpx.AsyncHTTPTransport(local_address="0.0.0.0")},
            **timeouts)

    app = (
        Application.builder()
        .token(token)
        .post_init(_post_init)
        .request(_ipv4_request(connect_timeout=15, read_timeout=20,
                               write_timeout=20, pool_timeout=5))
        .get_updates_request(_ipv4_request(connect_timeout=15, read_timeout=30,
                                           write_timeout=20, pool_timeout=5))
        .build()
    )
    app.add_error_handler(_on_error)
    app.add_handler(CommandHandler("start", start_cmd))
    app.add_handler(CommandHandler("reset", reset_cmd))
    app.add_handler(CallbackQueryHandler(callback_handler))
    # New messages only: an edit, a channel post, or a business or guest
    # message (whose chat id can equal the owner's) never reaches text_handler.
    app.add_handler(MessageHandler(filters.UpdateType.MESSAGE & filters.TEXT & ~filters.COMMAND,
                                   text_handler))
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
    _post_init runs before start_polling, as it does in run_polling().
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
    # bootstrap_retries=-1: the library retries its own deleteWebhook on a
    # network hiccup, so a started app is never torn down and rebuilt.
    await tg_app.updater.start_polling(allowed_updates=Update.ALL_TYPES,
                                       bootstrap_retries=-1,
                                       error_callback=_on_polling_error)
    _stderr("polling loop is live")
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
            f"BOT NOT STARTED: TELEGRAM_BOT_TOKEN not found in {_ENV_FILE}. "
            "Connect the bot in Settings, or paste your @BotFather token there."
        )
        return
    _stderr(f"starting bot (token ends in …{token[-6:]})")

    def _runner():
        import sys, traceback
        from telegram.error import NetworkError, RetryAfter
        delay = 5
        while True:
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
                return
            except (NetworkError, RetryAfter) as e:
                # Telegram not reachable right now — a stalled link, a VPN
                # reconnecting, a captive network. One read timeout on the
                # very first getMe used to kill the bot for the whole
                # session; the API is usually back within seconds. Retry
                # with backoff, and the bot comes online when the network
                # does. A 429 flood limit (RetryAfter) counts as unreachable
                # too. A bad token is neither and still crashes loudly below.
                # Type and message only, the token scrubbed and the text capped:
                # a proxy block page can echo the request URL into the error.
                _stderr(f"Telegram unreachable ({type(e).__name__}: "
                        f"{str(e).replace(token, '<token>')[:160]}) — retrying in {delay}s")
                time.sleep(delay)
                delay = min(delay * 2, 60)
            except Exception as e:
                _stderr(f"BOT CRASHED: {e!r}")
                traceback.print_exc(file=sys.stderr)
                return

    _BOT_THREAD = threading.Thread(target=_runner, daemon=True, name="telegram-bot")
    _BOT_THREAD.start()


def main():
    """Standalone entry — for testing without launching the full AutoCua app."""
    token = _resolve_token()
    if not token:
        raise SystemExit(
            f"TELEGRAM_BOT_TOKEN not found in {_ENV_FILE}\n"
            "(create the bot via @BotFather first, then add the token to that file)."
        )
    tg_app = _build_telegram_app(token)
    code = _pair_code()
    if code:
        print(f"Not paired yet. Send this code to your bot in Telegram: {code}", flush=True)
    logger.info("Telegram bot polling started (main thread)")
    tg_app.run_polling(allowed_updates=Update.ALL_TYPES)


if __name__ == "__main__":
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    main()
