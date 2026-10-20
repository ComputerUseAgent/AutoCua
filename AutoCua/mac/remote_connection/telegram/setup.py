"""Telegram remote-connection setup driver (macOS, guided mode).

Opens Safari on web.telegram.org, then lets the user log in manually.
Progress is paced by a small always-on-top banner that streams status text
and has a Next button. The script blocks on user clicks via
banner.wait_for_next() — the user does the actual login (phone, country,
OTP) themselves; we just get them to the right page.

The URL is launched together with the browser in ONE command — the mac
browser skill's rule 1 — instead of opening Safari, scanning for its
address bar, clicking it, typing the URL and pressing return. That path
cost a scan plus three fixed two-second pauses and could fail on a missing
element; `open -a` needs no scan, no focus and no Automation permission,
and works whether Safari is already running (the URL arrives as a new tab)
or not.
"""
import logging
import os
import subprocess
import time

from AutoCua.mac.remote_connection.banner import StatusBanner
from AutoCua.mac.remote_connection.telegram.service import (
    _API_KEY_FILE, _set_key_in_file,
)

logger = logging.getLogger(__name__)

TELEGRAM_WEB_URL = "https://web.telegram.org"
BROWSER = "Safari"
SETTLE_SEC = 2


def _open_telegram_in_safari(banner) -> bool:
    """Launch Safari on web.telegram.org.

    Streams sub-step status to the banner so the user can see what's
    happening while Safari takes focus. Returns False if the launch fails.
    """
    banner.update("Please wait — opening Safari…")
    try:
        subprocess.run(["open", "-a", BROWSER, TELEGRAM_WEB_URL],
                       check=True, capture_output=True, timeout=15)
    except Exception as e:
        logger.error(f"setup.py: failed to open Safari ({e})")
        return False
    # A beat for the window to come forward and the page to start loading.
    time.sleep(SETTLE_SEC)
    banner.update("Safari is open — Telegram Web is loading.")
    return True


def run(country_code: str = "", phone: str = "") -> bool:
    """Guided Telegram-Web pairing.

    Shows a banner, waits for the user to click Next, opens Telegram Web,
    waits for the user to log in manually + click Next, then closes.

    country_code and phone are accepted but ignored — kept only so the
    pre-existing /api/telegram/connect callsite signature still works.
    """
    banner = StatusBanner()
    banner.show()
    try:
        banner.update("Let's get you set up with Telegram. Please click Next.")
        banner.wait_for_next()

        if not _open_telegram_in_safari(banner):
            banner.update("Failed to open Telegram. Close this banner and try again.")
            banner.wait_for_next(timeout=15)
            return False

        banner.update("Please log in to Telegram, then click Next")
        banner.wait_for_next()

        banner.update(
            "Now search for @BotFather in Telegram and open the chat. "
            "Click Next when you're there."
        )
        banner.wait_for_next()

        banner.update("How do you want to set up the bot?")
        choice = banner.wait_for_choice("Fresh setup", "Token already generated")

        if choice == "left":
            banner.update(
                "In @BotFather, send these one at a time:  /newbot  →  AutoCua  →  "
                "a unique bot name. BotFather will reply with your token. "
                "Click Next when you have it."
            )
            banner.wait_for_next()

        banner.update("Paste your BotFather token below and click Save.")
        token = banner.wait_for_input(save_label="Save")
        if not token:
            return False  # Cocoa-unavailable fallback; banner never appeared

        _set_key_in_file(_API_KEY_FILE, "TELEGRAM_BOT_TOKEN", token.strip())

        banner.update("Saved. Restarting AutoCua to start the bot…")
        # Give the message time to stream out + a beat for the user to read
        # it, then hard-exit the whole process. The user's next `python
        # app.py` boot picks up the fresh TELEGRAM_BOT_TOKEN and the bot
        # comes online with the saved owner chat. os._exit skips atexit /
        # finally cleanup, which is what we want — Cocoa will tear down
        # the banner + windows as the process dies.
        time.sleep(3)
        banner.close()
        os._exit(0)
    finally:
        banner.close()
