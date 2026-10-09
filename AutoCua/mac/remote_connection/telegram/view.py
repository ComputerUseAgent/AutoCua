"""Flask Blueprint for the macOS Telegram surface.

Lives in the telegram folder so all Telegram-related code stays here — app.py
just imports `telegram_bp` and calls `app.register_blueprint(...)`. Routes:

  GET  /api/telegram/status       → {connected, bot_username?}
  POST /api/telegram/connect      → kicks off the Phase 4 guided walkthrough
  POST /api/telegram/disconnect   → clears the persisted token and owner chat

All token lookups read ONLY from api_key.txt. We deliberately do NOT consult
.env — that file is app.py's general env-loading concern; the Telegram bot
treats api_key.txt as its single source of truth.
"""
import json
import logging
import threading
import urllib.request

from flask import Blueprint, jsonify

logger = logging.getLogger(__name__)

telegram_bp = Blueprint("telegram_macos", __name__)

# Single source of truth for the key-file path — service.py resolves it in a
# compiled-build-aware way (next to the executable when frozen). Importing it
# here keeps the picker/status/disconnect routes pointed at the same file the
# bot and the regular agent read.
from .service import _API_KEY_FILE, _set_key_in_file

_bot_username_cache: str | None = None


def _read_token() -> str | None:
    """Pull TELEGRAM_BOT_TOKEN out of api_key.txt. Returns None if missing or
    empty. Does NOT consult .env or env vars on purpose."""
    if not _API_KEY_FILE.exists():
        return None
    try:
        with open(_API_KEY_FILE, "r", encoding="utf-8") as f:
            for line in f:
                stripped = line.strip()
                if stripped.startswith("TELEGRAM_BOT_TOKEN="):
                    val = stripped.partition("=")[2].strip()
                    return val or None
    except Exception:
        logger.warning("could not read %s", _API_KEY_FILE)
    return None


def _set_token(value: str) -> None:
    """Write/clear TELEGRAM_BOT_TOKEN= in api_key.txt, preserving every other
    line (incl. empty-value placeholders the AutoCua UI relies on)."""
    lines = []
    found = False
    if _API_KEY_FILE.exists():
        try:
            with open(_API_KEY_FILE, "r", encoding="utf-8") as f:
                for raw in f:
                    if raw.strip().startswith("TELEGRAM_BOT_TOKEN="):
                        lines.append(f"TELEGRAM_BOT_TOKEN={value}\n")
                        found = True
                    else:
                        lines.append(raw if raw.endswith("\n") else raw + "\n")
        except Exception:
            logger.warning("could not read %s while updating token", _API_KEY_FILE)
            return
    if not found:
        lines.append(f"TELEGRAM_BOT_TOKEN={value}\n")
    try:
        _API_KEY_FILE.parent.mkdir(parents=True, exist_ok=True)
        with open(_API_KEY_FILE, "w", encoding="utf-8") as f:
            f.writelines(lines)
    except Exception:
        logger.warning("could not write %s", _API_KEY_FILE)


def _fetch_bot_username(token: str) -> str | None:
    """One-shot call to Telegram's getMe — used by /status so the panel can
    show '@your_bot' instead of just 'connected'."""
    try:
        resp = urllib.request.urlopen(
            f"https://api.telegram.org/bot{token}/getMe", timeout=5
        )
        data = json.loads(resp.read())
        if data.get("ok"):
            return data["result"].get("username", "") or None
    except Exception:
        pass
    return None


# ── routes ──────────────────────────────────────────────────────────────────

@telegram_bp.route("/api/telegram/status", methods=["GET"])
def telegram_status():
    """Frontend uses this to decide which Remote Connection panel state to
    show. If a token is present in api_key.txt → 'connected', and the panel
    flips to the @bot_username + Disconnect view (Connect button is hidden).
    Cached so we don't hit Telegram's API on every page load."""
    global _bot_username_cache
    token = _read_token()
    if not token:
        _bot_username_cache = None
        return jsonify({"connected": False})
    if _bot_username_cache is None:
        _bot_username_cache = _fetch_bot_username(token) or ""
    return jsonify({
        "connected": True,
        "bot_username": _bot_username_cache,
    })


@telegram_bp.route("/api/telegram/connect", methods=["POST"])
def telegram_connect():
    """Kick off the Phase 4 guided walkthrough (Safari → web.telegram.org →
    user logs in manually, paced by the floating banner). Returns immediately;
    the real work runs on a daemon thread since it blocks on user clicks."""
    try:
        from AutoCua.mac.remote_connection.telegram.setup import (
            run as run_telegram_setup,
        )
        threading.Thread(target=run_telegram_setup, daemon=True).start()
        return jsonify({"status": "started"})
    except Exception as e:
        logger.exception("telegram_connect failed")
        return jsonify({"status": "error", "message": str(e)}), 500


@telegram_bp.route("/api/telegram/disconnect", methods=["POST"])
def telegram_disconnect():
    """Clear the persisted token, the owner chat and the cached @bot_username.
    The next chat to message the bot after reconnecting claims it. The polling
    thread already running keeps polling until the next app restart (soft
    disconnect) — clean shutdown of the bot loop is a future enhancement."""
    global _bot_username_cache
    _set_token("")
    _set_key_in_file(_API_KEY_FILE, "TELEGRAM_OWNER_CHAT_ID", "")
    _bot_username_cache = None
    return jsonify({"status": "disconnected"})
