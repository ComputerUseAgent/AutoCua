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
from AutoCua.mac.remote_connection.telegram.service import _start_pairing

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

        code = _start_pairing(token.strip())
        if not code:
            banner.update("Could not save the token. Click Next, then press Connect again.")
            banner.wait_for_next(timeout=15)
            return False

        # The bot obeys only the chat that sends this code. Telegram keeps a
        # message sent now for up to 24 hours, so it pairs when the bot starts
        # after the restart; the code also stays in Settings > Remote
        # Connection until it is used.
        banner.update(
            "Saved. Last step: open your bot in Telegram and send it this code: "
            f"{code}. Click Next when you have sent it."
        )
        banner.wait_for_next()
        banner.update("AutoCua will close now. Open it again, and your bot will answer. "
                      "The code is also in Settings > Remote Connection.")
        # Give the message time to stream out + a beat for the user to read
        # it, then hard-exit the whole process. The next boot starts the bot
        # with the fresh TELEGRAM_BOT_TOKEN. os._exit skips atexit / finally
        # cleanup, which is what we want.
        time.sleep(5)
        banner.close()
        os._exit(0)
    finally:
        banner.close()
