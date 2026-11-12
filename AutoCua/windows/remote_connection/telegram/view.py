"""Flask Blueprint for the Windows Telegram surface.

Mirror of the macOS view.py, adapted so app.py's single
`from ...view import telegram_bp, start_bot` works on Windows. Routes:

  GET  /api/telegram/status       → {connected, bot_username?, pair_code?}
  POST /api/telegram/connect      → kicks off the guided walkthrough (Edge)
  POST /api/telegram/disconnect   → clears the token, owner chat and pairing code

All token lookups read ONLY the .env (AutoCua.env_file), never env vars: the
bot treats that file as its single source of truth.
"""
import json
import logging
import threading
import urllib.request

from flask import Blueprint, jsonify
from AutoCua.utils.errors import error_text

# Re-export start_bot so app.py's
#   from AutoCua.windows.remote_connection.telegram.view import telegram_bp, start_bot
# works from a single import line, matching app.py:921.
# _ENV_FILE comes from service.py too (AutoCua.env_file): one source of truth.
from .service import (start_bot, _ENV_FILE, _set_keys_in_file,  # noqa: F401
                      _resolve_token, _pair_code)

logger = logging.getLogger(__name__)

telegram_bp = Blueprint("telegram_windows", __name__)

_bot_username_cache: str | None = None


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
    show. If a token is present in the .env → 'connected', and the panel
    flips to the @bot_username + Disconnect view (Connect button is hidden).
    While the bot is not paired it also gets the pairing code to show (made
    on the first ask). Cached so we don't hit Telegram's API on every load."""
    global _bot_username_cache
    token = _resolve_token()
    if not token:
        _bot_username_cache = None
        return jsonify({"connected": False})
    if _bot_username_cache is None:
        _bot_username_cache = _fetch_bot_username(token) or ""
    return jsonify({
        "connected": True,
        "bot_username": _bot_username_cache,
        "pair_code": _pair_code(),   # None once paired
    })


@telegram_bp.route("/api/telegram/connect", methods=["POST"])
def telegram_connect():
    """Kick off the guided walkthrough (Edge → web.telegram.org → user logs
    in manually, paced by the floating banner). Returns immediately; the real
    work runs on a daemon thread since it blocks on user clicks."""
    try:
        from AutoCua.windows.remote_connection.telegram.setup import (
            run as run_telegram_setup,
        )
        threading.Thread(target=run_telegram_setup, daemon=True).start()
        return jsonify({"status": "started"})
    except Exception as e:
        logger.exception("telegram_connect failed")
        return jsonify({"status": "error", "message": error_text(e)}), 500


@telegram_bp.route("/api/telegram/disconnect", methods=["POST"])
def telegram_disconnect():
    """Clear the token, the owner chat and any pairing code in one write, and
    the cached @bot_username. Connect again shows a new code to pair with. The
    polling thread already running keeps polling until the next app restart
    (soft disconnect), obeying nobody. A clean shutdown of the bot loop is a
    future enhancement."""
    global _bot_username_cache
    _set_keys_in_file(_ENV_FILE, {"TELEGRAM_BOT_TOKEN": "",
                                      "TELEGRAM_OWNER_CHAT_ID": "",
                                      "TELEGRAM_PAIR_CODE": ""})
    _bot_username_cache = None
    return jsonify({"status": "disconnected"})
