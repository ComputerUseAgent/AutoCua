"""Telegram remote-connection setup driver (Windows, guided mode).

Opens Microsoft Edge on web.telegram.org, then lets the user log in
manually. Progress is paced by a small always-on-top banner that streams
status text and has a Next button. The script blocks on user clicks via
banner.wait_for_next() — the user does the actual login (phone, country,
OTP) themselves; we just get them to the right page.

The URL is launched together with the browser in ONE PowerShell command —
the Windows browser skill's rule 1 — instead of opening Edge, scanning for
its address bar, clicking it, typing the URL and pressing return. That
path cost a scan plus three fixed two-second pauses and could fail on a
missing element; Start-Process needs no scan and no focus, and works
whether Edge is already running (the URL arrives as a new tab) or not.
"""
import logging
import os
import subprocess
import threading
import time

from AutoCua.windows.remote_connection.banner import StatusBanner
from AutoCua.windows.remote_connection.telegram.service import (
    _API_KEY_FILE, _set_key_in_file,
)

logger = logging.getLogger(__name__)

TELEGRAM_WEB_URL = "https://web.telegram.org"
SETTLE_SEC = 2

# Singleton guard — /api/telegram/connect spawns a fresh daemon thread on
# every POST, so a rapid double-click or polling-induced re-fire would
# otherwise launch parallel banner wizards. We let the redundant calls
# return immediately while the first one runs to completion.
_SETUP_LOCK = threading.Lock()
_SETUP_ACTIVE = False

# The browser skill's launch ladder, in one PowerShell script: `msedge` by
# name (rule 3); if the name does not resolve (portable install, custom
# path), the .exe path of a running Edge (rule 5); failing both, the URL
# alone, which opens the default browser (rule 2). Telegram Web works in
# any of them.
_LAUNCH_PS = (
    "$url = '{url}'; "
    "try {{ Start-Process msedge $url -ErrorAction Stop }} catch {{ "
    "$p = (Get-Process msedge -ErrorAction SilentlyContinue | Select-Object -First 1).Path; "
    "if ($p) {{ Start-Process $p $url }} else {{ Start-Process $url }} }}"
)


def _open_telegram_in_edge(banner) -> bool:
    """Launch Edge on web.telegram.org.

    Streams sub-step status to the banner so the user can see what's
    happening while Edge takes focus. Returns False if the launch fails.
    """
    banner.update("Please wait — opening Edge…")
    try:
        subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             _LAUNCH_PS.format(url=TELEGRAM_WEB_URL)],
            check=True, capture_output=True, timeout=20,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    except Exception as e:
        logger.error(f"setup.py: failed to open Edge ({e})")
        return False
    # A beat for the window to come forward and the page to start loading.
    time.sleep(SETTLE_SEC)
    banner.update("Edge is open — Telegram Web is loading.")
    return True


def run(country_code: str = "", phone: str = "") -> bool:
    """Guided Telegram-Web pairing.

    Shows a banner, waits for the user to click Next, opens Telegram Web,
    waits for the user to log in manually + click Next, then closes.

    country_code and phone are accepted but ignored — kept only so the
    pre-existing /api/telegram/connect callsite signature still works.

    Idempotent under concurrent calls: if a wizard is already running,
    redundant invocations return False immediately so we don't end up
    with N parallel banners in the taskbar.
    """
    global _SETUP_ACTIVE
    with _SETUP_LOCK:
        if _SETUP_ACTIVE:
            logger.info(
                "setup.run: wizard already running — ignoring duplicate Connect"
            )
            return False
        _SETUP_ACTIVE = True

    banner = StatusBanner()
    banner.show()
    try:
        banner.update("Let's get you set up with Telegram. Please click Next.")
        if not banner.wait_for_next():
            return False

        if not _open_telegram_in_edge(banner):
            banner.update("Failed to open Telegram. Close this banner and try again.")
            banner.wait_for_next(timeout=15)
            return False

        banner.update("Please log in to Telegram, then click Next")
        if not banner.wait_for_next():
            return False

        banner.update(
            "Now search for @BotFather in Telegram and open the chat. "
            "Click Next when you're there."
        )
        if not banner.wait_for_next():
            return False

        banner.update("How do you want to set up the bot?")
        choice = banner.wait_for_choice("Fresh setup", "Token already generated")

        if choice == "left":
            banner.update(
                "In @BotFather, send these one at a time:  /newbot  →  AutoCua  →  "
                "a unique bot name. BotFather will reply with your token. "
                "Click Next when you have it."
            )
            if not banner.wait_for_next():
                return False

        banner.update("Paste your BotFather token below and click Save.")
        token = banner.wait_for_input(save_label="Save")
        if not token:
            return False  # banner never appeared or user closed it

        _set_key_in_file(_API_KEY_FILE, "TELEGRAM_BOT_TOKEN", token.strip())

        banner.update("Saved. Restarting AutoCua to start the bot…")
        # Give the message time to stream out + a beat for the user to read
        # it, then hard-exit the whole process. The user's next `python
        # app.py` boot picks up the fresh TELEGRAM_BOT_TOKEN and the bot
        # comes online with the saved owner chat. os._exit skips atexit /
        # finally cleanup, which is what we want — the tk loop will be torn
        # down as the process dies.
        time.sleep(3)
        banner.close()
        os._exit(0)
    finally:
        banner.close()
        with _SETUP_LOCK:
            _SETUP_ACTIVE = False
