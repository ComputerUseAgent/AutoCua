"""Telegram remote-connection setup driver (Linux, guided mode).

Opens Firefox on web.telegram.org, then lets the user log in manually.
Progress is paced by a small always-on-top banner that streams status text
and has a Next button. The script blocks on user clicks via
banner.wait_for_next() — the user does the actual login (phone, country,
OTP) themselves; we just get them to the right page.

The macOS driver types the URL into Safari's address bar through the
scanner. Here the URL goes on Firefox's command line instead: it is the one
launch that behaves the same whether Firefox is already running (a new
window in that instance) or not, and it needs no focus and no address-bar
element. GNOME's focus-stealing prevention can leave a window opened by a
background process behind the app ("Firefox is ready"), so the banner
tells the user what to do in that case rather than failing.
"""
import logging
import os
import shutil
import subprocess
import time

from AutoCua.linux.tree import element as E
from AutoCua.linux.remote_connection.banner import StatusBanner
from AutoCua.linux.remote_connection.telegram.service import _start_pairing

logger = logging.getLogger(__name__)

TELEGRAM_WEB_URL = "https://web.telegram.org"
BROWSER = "firefox"
BROWSER_WAIT_SEC = 20


def _firefox_in_front() -> bool:
    """True when the window in front belongs to Firefox."""
    try:
        actors = E._collect_shell_window_actors(fresh=True)
        app, _win, _actor = E.find_top_window(actors, E.candidate_toplevels())
        return app is not None and BROWSER in (E._safe_name(app) or "").lower()
    except Exception:
        return False


def _open_telegram_in_firefox(banner) -> bool:
    """Launch Firefox on web.telegram.org and wait for it to come to the front.

    Streams sub-step status to the banner so the user can see what's
    happening. Returns False only when Firefox cannot be launched at all.
    """
    banner.update("Please wait — opening Firefox…")
    exe = shutil.which(BROWSER)
    if not exe:
        logger.error("setup.py: firefox is not installed")
        return False
    # Detached (setsid -f), the way the browser skill launches browsers:
    # Firefox outlives this thread and never holds it.
    try:
        subprocess.Popen(["setsid", "-f", exe, "--new-window", TELEGRAM_WEB_URL],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception as e:
        logger.error(f"setup.py: failed to launch Firefox ({e})")
        return False

    deadline = time.time() + BROWSER_WAIT_SEC
    while time.time() < deadline:
        time.sleep(1.0)
        if _firefox_in_front():
            banner.update("Firefox is open — Telegram Web is loading.")
            return True
    # Launched, but GNOME kept the focus where it was. It is there; the
    # user brings it forward.
    banner.update("Firefox is open — if it is not in front, click it in the dock.")
    return True


def run(country_code: str = "", phone: str = "") -> bool:
    """Guided Telegram-Web pairing.

    Shows a banner, waits for the user to click Next, opens Telegram Web in
    Firefox, waits for the user to log in manually + click Next, then closes.

    country_code and phone are accepted but ignored — kept only so the
    pre-existing /api/telegram/connect callsite signature still works.
    """
    banner = StatusBanner()
    banner.show()
    try:
        banner.update("Let's get you set up with Telegram. Please click Next.")
        banner.wait_for_next()

        if not _open_telegram_in_firefox(banner):
            banner.update("Failed to open Firefox. Close this banner and try again.")
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
            return False  # no banner process; it never appeared

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
