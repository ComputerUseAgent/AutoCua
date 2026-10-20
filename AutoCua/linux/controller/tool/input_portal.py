# AutoCua/linux/controller/tool/input_portal.py
"""Pointer and keyboard injection for Linux desktops, through the XDG
RemoteDesktop portal (org.freedesktop.portal.RemoteDesktop).

WHY THE PORTAL. On Wayland a client cannot synthesize input for another
window: there is no XTest, and every unprivileged route was tried on this
machine (GNOME 50, Wayland) and found dead:
  - Atspi.generate_mouse_event / generate_keyboard_event return True and do
    NOTHING — the registry daemon injects through XWayland, which native
    Wayland windows never see. Measured: a click on gnome-calculator's "7"
    and a typed "8" left its display empty.
  - pynput / pyautogui speak X11 and reach only XWayland clients.
  - ydotool / evdev uinput need root or a udev rule on /dev/uinput.
The RemoteDesktop portal is the one sanctioned path: the compositor itself
moves a virtual pointer and keyboard on our behalf. Version 2 (this machine)
adds persistence — the first session shows the user GNOME's consent dialog
("Share your screen / allow remote interaction"), and the restore token it
hands back makes every later session silent. The token is single-use and
re-issued on every Start, so it is rewritten each time.

ABSOLUTE POSITIONING. NotifyPointerMotionAbsolute is defined relative to a
screencast STREAM, so the session also selects the monitors as ScreenCast
sources. The agent never reads the PipeWire streams (the run recorder does,
through pipewire_stream, when Settings -> Recording is on) — they exist to give
every monitor a node id and a logical-pixel origin, which is what maps the
scanner's screen coordinates (already logical, offset-corrected) onto the
right monitor. Without a stream that contains the point, motion falls back
to pinning the pointer at the top-left corner with a huge relative move and
walking to (x, y) from there.

One session per process (get_input). Every method raises PortalInputError
with a message a user can act on; the controller turns it into the error
dict the agent sees.
"""

import logging
import os
import secrets
import threading
import time
import uuid
from pathlib import Path

from ...tree.element import Gio, GLib, Gdk

logger = logging.getLogger(__name__)

PORTAL_BUS = "org.freedesktop.portal.Desktop"
PORTAL_PATH = "/org/freedesktop/portal/desktop"
IFACE_REMOTE_DESKTOP = "org.freedesktop.portal.RemoteDesktop"
IFACE_SCREENCAST = "org.freedesktop.portal.ScreenCast"
IFACE_REQUEST = "org.freedesktop.portal.Request"
IFACE_SESSION = "org.freedesktop.portal.Session"
IFACE_CLIPBOARD = "org.freedesktop.portal.Clipboard"

# org.freedesktop.portal.RemoteDesktop.AvailableDeviceTypes bits
DEVICE_KEYBOARD = 1
DEVICE_POINTER = 2
# The input devices an agent cannot work without. A session may legitimately
# come back with neither (see _build_session) — that is a denial, not a bug.
DEVICES_NEEDED = DEVICE_KEYBOARD | DEVICE_POINTER
# Said in two places (a fresh Start, and a restored session refusing Notify*),
# so it lives in one.
DENIED_MESSAGE = (
    "the desktop granted screen sharing but NO input devices, so AutoCua can "
    "see the screen and cannot move the pointer or type. In GNOME's Remote "
    "Desktop dialog, turn ON 'Allow Remote Interaction' before clicking Share. "
    "Re-run AutoCua to be asked again.")
# D-Bus errors that mean the session object itself is gone, so rebuilding is
# the right answer. Every OTHER error leaves the session alone: a refused call
# on a LIVE session used to reset it, which sent the next action back through
# ensure_session and reopened the consent dialog — once per click, forever.
SESSION_GONE_ERRORS = frozenset((
    "org.freedesktop.DBus.Error.UnknownObject",
    "org.freedesktop.DBus.Error.UnknownMethod",
    "org.freedesktop.DBus.Error.ServiceUnknown",
    "org.freedesktop.DBus.Error.NoReply",
    "org.freedesktop.DBus.Error.Disconnected",
))
# org.freedesktop.portal.ScreenCast source types / cursor modes
SOURCE_MONITOR = 1
CURSOR_EMBEDDED = 2     # the pointer is painted into the stream (the recorder shows it)
# persist_mode: 0 do not persist, 1 while the app runs, 2 until revoked
PERSIST_UNTIL_REVOKED = 2

# Linux input event codes (input-event-codes.h) — the portal speaks evdev.
BTN_LEFT = 0x110
BTN_RIGHT = 0x111
BTN_MIDDLE = 0x112
PRESSED = 1
RELEASED = 0
AXIS_VERTICAL = 0
AXIS_HORIZONTAL = 1

# How long the user has to answer the consent dialog on the first run. The
# restore token makes later sessions instant, so this only ever bites once.
CONSENT_TIMEOUT = 120
# How many times a session may be rebuilt without a single Notify* succeeding
# in between. The consent dialog is shown at most this many times per process:
# past it, something is refusing every action and re-asking only trains the
# user to click Share at a prompt that will not help. Two, so one genuine
# expiry (the portal restarted) still recovers on its own.
MAX_REBUILDS = 2
# Ordinary portal round-trips (Notify*) — the compositor answers at once.
CALL_TIMEOUT_MS = 5000

# Timing of a synthesized click. GNOME's double-click interval is 400ms by
# default, so two presses 80ms apart read as a double click.
BUTTON_HOLD = 0.06
CLICK_GAP = 0.08
KEY_HOLD = 0.03
# Keystroke typing pace (seconds per character, hold included) — the macOS
# controller's 0.04, with 0.05 for terminals.
TYPE_INTERVAL = 0.04
# How long a paste may take to be collected by the focused app.
PASTE_TIMEOUT = 3.0
# What the clipboard text is offered as; GTK, Qt and Chromium all ask for
# the first, older X clients for the rest.
CLIPBOARD_MIMES = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING",
                   "STRING", "TEXT"]

# Keysym names (Gdk.keyval_from_name) for the modifiers a shortcut can hold.
MODIFIER_KEYSYMS = {
    "ctrl": "Control_L", "control": "Control_L",
    # A macOS-trained model says cmd+c for what Linux calls ctrl+c; treating
    # cmd as ctrl gives it the shortcut it meant. The Super key is reachable
    # by name for the shell's own bindings.
    "cmd": "Control_L", "command": "Control_L",
    "shift": "Shift_L",
    "alt": "Alt_L", "option": "Alt_L", "opt": "Alt_L",
    "super": "Super_L", "win": "Super_L", "meta": "Super_L",
}
# Named non-character keys → X keysym names.
NAMED_KEYSYMS = {
    "return": "Return", "enter": "Return",
    "tab": "Tab", "space": "space",
    "backspace": "BackSpace", "delete": "Delete", "del": "Delete",
    "escape": "Escape", "esc": "Escape",
    "up": "Up", "down": "Down", "left": "Left", "right": "Right",
    "home": "Home", "end": "End",
    "pageup": "Page_Up", "pagedown": "Page_Down",
    "insert": "Insert", "menu": "Menu", "printscreen": "Print",
    "capslock": "Caps_Lock", "numlock": "Num_Lock",
}
NAMED_KEYSYMS.update({f"f{i}": f"F{i}" for i in range(1, 25)})


class PortalInputError(RuntimeError):
    """The portal could not do it — the message says what the user must do."""


def _keyval(name):
    """Gdk.keyval_from_name, with its "unknown" answer (VoidSymbol, not 0)
    turned into None so an unknown key is an error rather than a keystroke."""
    val = Gdk.keyval_from_name(name)
    return val if val and val != Gdk.KEY_VoidSymbol else None


def keysym_for(name):
    """Keysym for a key name as the agent writes it ("enter", "f5", "a",
    "ctrl"), or None. Single characters go through Unicode."""
    key = (name or "").strip().lower()
    if not key:
        return None
    if key in MODIFIER_KEYSYMS:
        return _keyval(MODIFIER_KEYSYMS[key])
    if key in NAMED_KEYSYMS:
        return _keyval(NAMED_KEYSYMS[key])
    if len(name.strip()) == 1:
        return Gdk.unicode_to_keyval(ord(name.strip())) or None
    return _keyval(name.strip())


def _screen_locked():
    """True when the desktop's screensaver / lock screen is active. Asked
    only to explain a refused session — never to decide anything."""
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        return bool(bus.call_sync(
            "org.gnome.ScreenSaver", "/org/gnome/ScreenSaver",
            "org.gnome.ScreenSaver", "GetActive", None, None,
            Gio.DBusCallFlags.NONE, 2000, None).unpack()[0])
    except Exception:
        return False


def _monitor_scales(bus):
    """{(x, y): scale} of every logical monitor, from mutter's DisplayConfig
    — the same layout the screencast streams are positioned in. Empty (so
    every stream falls back to scale 1.0) when the compositor is not
    mutter."""
    try:
        _serial, _monitors, logical, _props = bus.call_sync(
            "org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig",
            "org.gnome.Mutter.DisplayConfig", "GetCurrentState", None, None,
            Gio.DBusCallFlags.NONE, 3000, None).unpack()
    except Exception:
        return {}
    out = {}
    for lm in logical:
        try:
            x, y, scale = int(lm[0]), int(lm[1]), float(lm[2])
        except Exception:
            continue
        out[(x, y)] = scale if scale > 0 else 1.0
    return out


def _default_token_file():
    """AutoCua_data/linux/remote_desktop.token — beside the rest of the
    user's data, outside the install folder."""
    try:
        from AutoCua import data_root
        d = data_root() / "linux"
    except Exception:
        d = Path.home() / ".local" / "share" / "AutoCua"
    d.mkdir(parents=True, exist_ok=True)
    return d / "remote_desktop.token"


class PortalInput:
    """One RemoteDesktop session: pointer + keyboard for the whole screen."""

    def __init__(self, token_file=None):
        self._bus = None
        self._session = None
        self._streams = []          # [(node_id, x, y, w, h, scale)] logical px + monitor scale
        self._lock = threading.RLock()
        self._pending_session = None    # set while _build_session runs
        self._clipboard = False         # session has clipboard access (Start result)
        self._clip_server = None        # _ClipboardServer, once a paste happened
        self._token_file = Path(token_file) if token_file else _default_token_file()
        self._held_buttons = set()
        self._held_keys = set()
        self._devices = 0           # device bitmask the portal actually granted
        self._denied = None         # why input was refused; set once, never retried
        self._rebuilds = 0          # sessions built since the last Notify* that worked
        self._token_spent = False   # a stored token was presented but not yet re-issued

    # ------------------------------------------------------------------
    # Portal plumbing
    # ------------------------------------------------------------------

    def _connection(self):
        if self._bus is None:
            try:
                self._bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            except GLib.Error as e:
                raise PortalInputError(
                    f"no session bus, so no desktop portal: {e.message}") from None
        return self._bus

    def _request(self, iface, method, build_params, timeout=CONSENT_TIMEOUT):
        """Call a portal method that answers through a Request object and
        wait for its Response signal. Returns the results dict; raises
        PortalInputError when the user cancelled or the portal failed.

        Runs on a PRIVATE GLib main context so it can never quit — or be
        quit by — the scanner's screenshot loop on the default context."""
        bus = self._connection()
        token = "AutoCua" + secrets.token_hex(4)
        sender = bus.get_unique_name()[1:].replace(".", "_")
        req_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        ctx = GLib.MainContext.new()
        ctx.push_thread_default()
        loop = GLib.MainLoop(ctx)
        out = {}

        def on_response(_conn, _sender, _path, _iface, _signal, params):
            code, data = params.unpack()
            out["code"] = code
            out["results"] = data
            loop.quit()

        try:
            sub = bus.signal_subscribe(
                PORTAL_BUS, IFACE_REQUEST, "Response", req_path, None,
                Gio.DBusSignalFlags.NONE, on_response)
            try:
                try:
                    handle = bus.call_sync(
                        PORTAL_BUS, PORTAL_PATH, iface, method,
                        build_params(token), GLib.VariantType("(o)"),
                        Gio.DBusCallFlags.NONE, -1, None).unpack()[0]
                except GLib.Error as e:
                    # No portal on this desktop, or an interface it lacks:
                    # the same "cannot inject" answer as a refusal, so the
                    # controller's AT-SPI fallback still gets its turn.
                    raise PortalInputError(
                        f"{method}: the desktop portal is unavailable "
                        f"({e.message})") from None
                if handle != req_path:
                    # Pre-0.9 portals name the request themselves; listen to
                    # that path too rather than wait on a signal that never
                    # comes.
                    sub2 = bus.signal_subscribe(
                        PORTAL_BUS, IFACE_REQUEST, "Response", handle, None,
                        Gio.DBusSignalFlags.NONE, on_response)
                else:
                    sub2 = None
                fired = []

                def _deadline(*_a):
                    fired.append(1)
                    loop.quit()
                    return False

                src = GLib.timeout_source_new(int(timeout * 1000))
                src.set_callback(_deadline)
                src.attach(ctx)
                try:
                    loop.run()
                finally:
                    if not fired:
                        src.destroy()
                    if sub2 is not None:
                        bus.signal_unsubscribe(sub2)
                if fired and "code" not in out:
                    # Withdraw the request so the consent dialog does not
                    # outlive the attempt (best effort — it may have gone).
                    try:
                        bus.call_sync(PORTAL_BUS, handle, IFACE_REQUEST, "Close",
                                      None, None, Gio.DBusCallFlags.NONE,
                                      CALL_TIMEOUT_MS, None)
                    except GLib.Error:
                        pass
                    raise PortalInputError(
                        f"{method}: no answer from the desktop portal within "
                        f"{timeout:.0f}s — the consent dialog may still be "
                        f"open; accept it and retry.")
            finally:
                bus.signal_unsubscribe(sub)
        finally:
            ctx.pop_thread_default()

        code = out.get("code")
        if code == 1:
            raise PortalInputError(
                f"{method}: the remote-desktop consent dialog was cancelled. "
                f"AutoCua cannot click or type until you allow it to control "
                f"the screen (Settings > Apps > Remote Desktop permissions).")
        if code != 0:
            # GNOME inhibits every remote-access session while the screen is
            # locked (the shell's unlock-dialog mode forbids screencasts, and
            # mutter answers "Session creation inhibited") — the one refusal
            # the user can fix in a second, so name it.
            if _screen_locked():
                raise PortalInputError(
                    f"{method}: the screen is locked (screensaver active), and "
                    f"the desktop refuses remote control while it is. Unlock "
                    f"the screen and retry.")
            raise PortalInputError(f"{method}: the desktop portal refused "
                                   f"(response code {code}).")
        return out.get("results") or {}

    def _call(self, method, params):
        """A plain (non-Request) RemoteDesktop method — the Notify* family."""
        try:
            self._connection().call_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_REMOTE_DESKTOP, method, params,
                None, Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None)
            self._rebuilds = 0      # the session works; re-arm the circuit breaker
        except GLib.Error as e:
            # Only a session that is genuinely GONE may be forgotten (the user
            # revoked it, or the portal restarted). Anything else is a refused
            # call on a LIVE session — most often input devices that were never
            # granted — and resetting it there is what produced a fresh consent
            # dialog on every single click.
            msg = e.message or ""
            name = Gio.dbus_error_get_remote_error(e) or ""

            # A session that reaches us WITHOUT input devices refuses every
            # Notify* with "Session is not allowed to call Notify...". The
            # Start-time gate below cannot catch this one: a token saved by an
            # older build restores a devices=0 session silently, with no dialog
            # at all, so this is the only place it ever surfaces. Treat it as
            # the same permanent denial and burn the token that caused it.
            if "not allowed to call Notify" in msg:
                self._clear_token()
                self._denied = DENIED_MESSAGE
                logger.error(self._denied)
                self._drop_session()
                raise PortalInputError(self._denied) from None

            # Only a session that is genuinely GONE may be forgotten (the user
            # revoked it, or the portal restarted). Anything else is a refused
            # call on a LIVE session, and resetting it there is what produced a
            # fresh consent dialog on every single click.
            if name in SESSION_GONE_ERRORS or any(n in msg for n in SESSION_GONE_ERRORS):
                self._drop_session()
            raise PortalInputError(f"{method} failed: {msg}") from None

    def _drop_session(self):
        """Forget the current session AND tell the portal to close it.

        The old code cleared the handles and walked away, leaking one
        compositor RemoteDesktop+ScreenCast session per failure — the
        "Stop Screen Sharing" indicator that outlived the run."""
        with self._lock:
            old, self._session = self._session, None
            self._streams = []
            self._clipboard = False
        if old:
            self._close_session(old)

    # ------------------------------------------------------------------
    # Session
    # ------------------------------------------------------------------

    def _load_token(self):
        """The stored restore token, or None. Anything that is not a UUID is
        discarded here rather than handed to the portal: SelectDevices answers
        a malformed token with InvalidArgument ("Restore token is not a valid
        UUID string") and aborts the whole build, which no amount of retrying
        fixes until the file is removed."""
        try:
            text = self._token_file.read_text(encoding="utf-8").strip()
        except Exception:
            return None
        if not text:
            return None
        try:
            uuid.UUID(text)
        except ValueError:
            logger.warning("the stored remote-desktop token is not a UUID; "
                           "discarding it and asking for consent again")
            self._clear_token()
            return None
        return text

    def _save_token(self, token):
        """Persist a token. A FALSY token is ignored, never a delete.

        GNOME omits restore_token whenever the user leaves "Remember This
        Selection" unchecked. Treating that as "erase what you have" meant one
        un-remembered Share destroyed a perfectly good stored grant. Explicit
        invalidation goes through _clear_token."""
        if not token:
            logger.info("the portal returned no restore token (was 'Remember "
                        "This Selection' unchecked?); the consent dialog will "
                        "appear again next run")
            return
        try:
            self._token_file.write_text(token, encoding="utf-8")
            logger.info(f"remote-desktop grant stored in {self._token_file}")
        except Exception as e:
            logger.warning(f"could not store the remote-desktop token: {e}")

    def _clear_token(self):
        """Drop a token we know the portal will not honour."""
        self._token_spent = False
        try:
            if self._token_file.exists():
                self._token_file.unlink()
                logger.info("discarded the stored remote-desktop grant")
        except Exception as e:
            logger.warning(f"could not remove the remote-desktop token: {e}")

    def ensure_session(self):
        """Create and start the portal session once. Idempotent. A session
        whose setup fails part-way is closed again, so a refused or timed-out
        consent never leaves a half-built session behind."""
        with self._lock:
            if self._session:
                return
            # A denial is final for the life of the process. Re-opening the
            # dialog cannot fix a toggle the user already answered, and asking
            # again once per action is how this looked like an unfixable bug.
            if self._denied:
                raise PortalInputError(self._denied)
            if self._rebuilds >= MAX_REBUILDS:
                raise PortalInputError(
                    f"the remote-desktop session was rebuilt {self._rebuilds} "
                    f"times without a single pointer or key event getting "
                    f"through. Not asking for consent again. Check that the "
                    f"desktop portal is running (xdg-desktop-portal plus the "
                    f"backend for your desktop, e.g. xdg-desktop-portal-gnome) "
                    f"and restart AutoCua.")
            self._rebuilds += 1
            try:
                self._build_session()
            except PortalInputError:
                self._close_session(self._pending_session)
                self._burn_spent_token()
                raise
            except GLib.Error as e:
                self._close_session(self._pending_session)
                self._burn_spent_token()
                raise PortalInputError(f"desktop portal error: {e.message}") from None
            finally:
                self._pending_session = None

    def _burn_spent_token(self):
        """Drop a token that was presented but never re-issued.

        The portal DELETES its half of the grant the moment SelectDevices
        accepts the token ("Immediately delete them now as a safety measure")
        and only writes a new one inside Start. So a build that is cancelled or
        times out leaves our file holding a UUID the portal has already erased:
        every later run presents it, gets NO error, and prompts anyway. Without
        this the file is a permanent silent ratchet; with it, one clean
        re-consent."""
        if self._token_spent:
            self._clear_token()

    def _close_session(self, session):
        if not session:
            return
        try:
            self._connection().call_sync(
                PORTAL_BUS, session, IFACE_SESSION, "Close", None, None,
                Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None)
        except Exception:
            pass

    def _build_session(self):
        """The CreateSession → SelectDevices → SelectSources → Start
        sequence; ensure_session wraps it with cleanup."""
        session_token = "AutoCua_s" + secrets.token_hex(4)
        res = self._request(
            IFACE_REMOTE_DESKTOP, "CreateSession",
            lambda t: GLib.Variant("(a{sv})", ({
                "handle_token": GLib.Variant("s", t),
                "session_handle_token": GLib.Variant("s", session_token),
            },)))
        session = res.get("session_handle")
        if not session:
            raise PortalInputError("CreateSession returned no session handle.")
        self._pending_session = session

        devices = {
            "types": GLib.Variant("u", DEVICE_KEYBOARD | DEVICE_POINTER),
            "persist_mode": GLib.Variant("u", PERSIST_UNTIL_REVOKED),
        }
        restore = self._load_token()
        if restore:
            devices["restore_token"] = GLib.Variant("s", restore)
        # From here the token is SPENT: the portal deletes its half as soon as
        # SelectDevices accepts it, and only Start re-issues one.
        self._token_spent = bool(restore)

        def _devices(t):
            opts = dict(devices)
            opts["handle_token"] = GLib.Variant("s", t)
            return GLib.Variant("(oa{sv})", (session, opts))

        try:
            self._request(IFACE_REMOTE_DESKTOP, "SelectDevices", _devices)
        except PortalInputError as e:
            # A token the portal will not take aborts the build outright. Drop
            # it and ask once, cleanly, rather than failing every run until
            # somebody deletes the file by hand.
            if not restore or "token" not in str(e).lower():
                raise
            logger.warning(f"the stored grant was rejected ({e}); "
                           f"asking for consent again")
            self._clear_token()
            devices.pop("restore_token", None)
            self._request(IFACE_REMOTE_DESKTOP, "SelectDevices", _devices)

        # Monitors as screencast sources, for absolute pointer motion.
        # Every monitor, so multi-monitor coordinates all resolve; the
        # cursor stays hidden in the (never consumed) stream.
        #
        # Do NOT add persist_mode/restore_token here. xdg-desktop-portal's
        # screen-cast implementation refuses both for any session that is also
        # a RemoteDesktop one ("Remote desktop sessions cannot persist"), and
        # the except below would swallow that into a stream-less session whose
        # move() silently degrades to the corner-pin path — accuracy gone, no
        # error raised. The monitor choice already rides inside the
        # RemoteDesktop restore data.
        try:
            self._request(
                IFACE_SCREENCAST, "SelectSources",
                lambda t: GLib.Variant("(oa{sv})", (session, {
                    "handle_token": GLib.Variant("s", t),
                    "types": GLib.Variant("u", SOURCE_MONITOR),
                    "multiple": GLib.Variant("b", True),
                    "cursor_mode": GLib.Variant("u", CURSOR_EMBEDDED),
                })))
        except PortalInputError as e:
            logger.warning(f"screencast sources unavailable ({e}); "
                           f"pointer motion will use the corner-pin fallback")

        # Clipboard access rides on the same session and must be asked for
        # BEFORE Start: it is how multi-line text is inserted — our text is
        # offered as the selection and served to whatever the focused app
        # asks for after a ctrl+v (see paste_text).
        try:
            self._connection().call_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_CLIPBOARD, "RequestClipboard",
                GLib.Variant("(oa{sv})", (session, {})), None,
                Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None)
        except GLib.Error as e:
            logger.warning(f"clipboard portal unavailable ({e.message}); "
                           f"multi-line text will be typed key by key")

        res = self._request(
            IFACE_REMOTE_DESKTOP, "Start",
            lambda t: GLib.Variant("(osa{sv})", (session, "", {
                "handle_token": GLib.Variant("s", t),
            })))
        # WHICH DEVICES the user actually granted. This is the field the whole
        # thing turns on. GNOME 49+ presents the consent dialog with "Allow
        # Remote Interaction" OFF by default; clicking Share with it off still
        # answers response code 0 -- the SCREEN CAST was granted -- and returns
        # devices=0. Measured on GNOME 50.1: toggle off -> devices 0, toggle on
        # -> devices 3 (KEYBOARD|POINTER). Without this check the session looks
        # healthy, every Notify* is refused, and the old _call reset sent the
        # next action back to a new consent dialog.
        granted = int(res.get("devices", 0) or 0)
        if not granted & DEVICES_NEEDED:
            # Do NOT keep the restore token: it would silently restore this
            # same input-less session on every future run, with no dialog left
            # to fix it in.
            self._clear_token()
            self._close_session(session)
            self._pending_session = None
            self._denied = DENIED_MESSAGE
            logger.error(self._denied)
            raise PortalInputError(self._denied)
        if granted & DEVICES_NEEDED != DEVICES_NEEDED:
            missing = "keyboard" if not granted & DEVICE_KEYBOARD else "pointer"
            logger.warning(
                f"the portal granted only part of the input: no {missing}. "
                f"Actions that need it will fail; the rest still work.")
        self._devices = granted
        self._clipboard = bool(res.get("clipboard_enabled", False))
        scales = _monitor_scales(self._connection())
        streams = []
        for node_id, props in (res.get("streams") or []):
            pos = props.get("position", (0, 0))
            size = props.get("size", (0, 0))
            sx, sy = int(pos[0]), int(pos[1])
            streams.append((int(node_id), sx, sy, int(size[0]), int(size[1]),
                            scales.get((sx, sy), 1.0)))
        self._streams = streams
        self._save_token(res.get("restore_token"))
        self._token_spent = False       # Start re-issued it (or there is none)
        self._session = session
        names = "+".join(n for b, n in ((DEVICE_KEYBOARD, "keyboard"),
                                        (DEVICE_POINTER, "pointer"))
                         if granted & b)
        logger.info(f"RemoteDesktop portal session ready "
                    f"({len(streams)} monitor stream(s), {names}, clipboard "
                    f"{'on' if self._clipboard else 'off'})")

    def close(self):
        with self._lock:
            if not self._session:
                return
            try:
                self.release_all()
            except Exception:
                pass
            self._close_session(self._session)
            self._session = None
            self._streams = []

    # ------------------------------------------------------------------
    # Pointer
    # ------------------------------------------------------------------

    def _stream_for(self, x, y):
        for node_id, sx, sy, w, h, scale in self._streams:
            if sx <= x < sx + w and sy <= y < sy + h:
                return node_id, sx, sy, scale
        return None

    def move(self, x, y):
        """Put the pointer at logical screen coordinates (x, y).

        The stream's coordinate space is the monitor's FRAMEBUFFER when
        GNOME scales per monitor (mutter resolves the point as
        layout.x + stream_x / scale), so the logical offset is multiplied by
        the monitor's scale — a no-op at scale 1, a miss by half the screen
        at 2 without it."""
        self.ensure_session()
        hit = self._stream_for(x, y)
        if hit is not None:
            node_id, sx, sy, scale = hit
            self._call("NotifyPointerMotionAbsolute", GLib.Variant(
                "(oa{sv}udd)", (self._session, {}, node_id,
                                float((x - sx) * scale), float((y - sy) * scale))))
            return
        # No stream covers the point: pin the pointer to the top-left corner
        # (the compositor clamps a huge negative move there) and walk over.
        self._call("NotifyPointerMotion", GLib.Variant(
            "(oa{sv}dd)", (self._session, {}, -1.0e5, -1.0e5)))
        self._call("NotifyPointerMotion", GLib.Variant(
            "(oa{sv}dd)", (self._session, {}, float(x), float(y))))

    def button(self, button, pressed):
        self.ensure_session()
        self._call("NotifyPointerButton", GLib.Variant(
            "(oa{sv}iu)", (self._session, {}, int(button),
                           PRESSED if pressed else RELEASED)))
        if pressed:
            self._held_buttons.add(button)
        else:
            self._held_buttons.discard(button)

    def click(self, x, y, button=BTN_LEFT, count=1):
        """Move to (x, y) and click `count` times (2 = double, 3 = triple)."""
        self.move(x, y)
        time.sleep(0.05)
        for i in range(count):
            self.button(button, True)
            time.sleep(BUTTON_HOLD)
            self.button(button, False)
            if i + 1 < count:
                time.sleep(CLICK_GAP)

    def scroll(self, x, y, steps, horizontal=False):
        """Wheel notches at (x, y): positive scrolls down / right."""
        self.move(x, y)
        time.sleep(0.05)
        axis = AXIS_HORIZONTAL if horizontal else AXIS_VERTICAL
        step = 1 if steps > 0 else -1
        for _ in range(abs(int(steps))):
            self._call("NotifyPointerAxisDiscrete", GLib.Variant(
                "(oa{sv}ui)", (self._session, {}, axis, step)))
            time.sleep(0.03)

    def drag(self, x1, y1, x2, y2, steps=20, button=BTN_LEFT):
        self.move(x1, y1)
        time.sleep(0.05)
        self.button(button, True)
        time.sleep(0.05)
        for i in range(1, steps + 1):
            t = i / steps
            self.move(int(x1 + (x2 - x1) * t), int(y1 + (y2 - y1) * t))
            time.sleep(0.015)
        self.button(button, False)

    # ------------------------------------------------------------------
    # Keyboard
    # ------------------------------------------------------------------

    def key(self, keysym, pressed):
        self.ensure_session()
        self._call("NotifyKeyboardKeysym", GLib.Variant(
            "(oa{sv}iu)", (self._session, {}, int(keysym),
                           PRESSED if pressed else RELEASED)))
        if pressed:
            self._held_keys.add(keysym)
        else:
            self._held_keys.discard(keysym)

    def tap(self, keysym):
        self.key(keysym, True)
        time.sleep(KEY_HOLD)
        self.key(keysym, False)

    def chord(self, modifiers, keysym):
        """Hold `modifiers` (keysyms), tap `keysym`, release in reverse."""
        for m in modifiers:
            self.key(m, True)
            time.sleep(0.02)
        try:
            self.tap(keysym)
        finally:
            for m in reversed(modifiers):
                self.key(m, False)
                time.sleep(0.02)

    # ------------------------------------------------------------------
    # Typing
    # ------------------------------------------------------------------

    def type_text(self, text, interval=TYPE_INTERVAL, stop=None):
        """One keystroke per character — the path canvases, terminals and
        anything that watches key events need. Newline is Return, tab is
        Tab; every other character is sent as its Unicode keysym, which the
        compositor maps onto the current layout (holding Shift/AltGr for
        the level) or onto a reserved keycode when the layout lacks it, so
        accented and non-Latin text types too. Returns False if `stop` (a
        threading.Event) fired part-way."""
        self.ensure_session()
        pause = max(0.0, interval - KEY_HOLD)
        for ch in text:
            if stop is not None and stop.is_set():
                return False
            if ch == "\r":
                continue
            if ch == "\n":
                ks = _keyval("Return")
            elif ch == "\t":
                ks = _keyval("Tab")
            else:
                ks = Gdk.unicode_to_keyval(ord(ch))
            if not ks:
                continue
            self.tap(ks)
            time.sleep(pause)
        return True

    def paste_text(self, text):
        """Insert `text` at the caret verbatim: offer it as the clipboard
        selection through the portal, send ctrl+v, and serve the focused
        app's request for the data. Multi-line values go this way so
        auto-indenting editors do not compound the indentation a keystroke
        Return would trigger (the macOS controller's clipboard rule).

        The text stays on the clipboard afterwards and keeps being served
        by a background thread for the life of the process, so the user's
        own later ctrl+v works — Wayland has no way to hand a selection
        back to its previous owner. Raises PortalInputError when the
        session has no clipboard access; callers then type instead."""
        self.ensure_session()
        if not self._clipboard:
            raise PortalInputError("the portal session has no clipboard access")
        if self._clip_server is None:
            self._clip_server = _ClipboardServer(self)
            self._clip_server.start()
        server = self._clip_server
        server.offer(text.encode("utf-8"))
        try:
            self._connection().call_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_CLIPBOARD, "SetSelection",
                GLib.Variant("(oa{sv})", (self._session, {
                    "mime_types": GLib.Variant("as", CLIPBOARD_MIMES)})),
                None, Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None)
        except GLib.Error as e:
            raise PortalInputError(f"SetSelection failed: {e.message}") from None
        time.sleep(0.15)     # let the compositor take the new selection
        self.chord([_keyval("Control_L")], Gdk.unicode_to_keyval(ord("v")))
        if not server.served.wait(PASTE_TIMEOUT):
            raise PortalInputError("the focused app never collected the pasted "
                                   "text (nothing accepts a paste there?)")
        # A few extra beats: the app may fetch a second mime type, and the
        # pasted text needs a moment to land before the next action.
        time.sleep(max(0.3, len(text) * 0.001))

    def pipewire_stream(self):
        """(fd, node_id) for reading the screencast of the monitor at (0, 0),
        for the run recorder (utils/run_recorder.py). It rides on this same
        session, so recording never opens a second consent dialog. The caller
        owns the fd. None when there is no session yet, or it has no stream.

        It never opens the consent dialog itself: while that dialog is up the
        agent keeps working and could click it. With a stored grant the
        session restores silently, so it is built here; without one the
        recorder waits for the agent's first action to build it."""
        with self._lock:
            if not self._session:
                if not self._token_file.exists():
                    return None
                self.ensure_session()
            if not self._streams:
                return None
            node = next((s for s in self._streams if (s[1], s[2]) == (0, 0)),
                        self._streams[0])[0]
            reply, fds = self._connection().call_with_unix_fd_list_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_SCREENCAST, "OpenPipeWireRemote",
                GLib.Variant("(oa{sv})", (self._session, {})),
                GLib.VariantType("(h)"), Gio.DBusCallFlags.NONE,
                CALL_TIMEOUT_MS, None, None)
            return fds.get(reply.unpack()[0]), node

    def _selection_write(self, serial, data):
        """Answer one SelectionTransfer: write `data` into the fd the
        portal hands over for `serial`, then report the outcome."""
        bus = self._connection()
        ok = False
        try:
            reply, fds = bus.call_with_unix_fd_list_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_CLIPBOARD, "SelectionWrite",
                GLib.Variant("(ou)", (self._session, int(serial))),
                GLib.VariantType("(h)"), Gio.DBusCallFlags.NONE,
                CALL_TIMEOUT_MS, None, None)
            fd = fds.get(reply.unpack()[0])
            try:
                view = memoryview(data)
                while view:
                    n = os.write(fd, view)
                    view = view[n:]
                ok = True
            finally:
                os.close(fd)
        except Exception as e:
            logger.warning(f"clipboard write failed: {e}")
        try:
            bus.call_sync(
                PORTAL_BUS, PORTAL_PATH, IFACE_CLIPBOARD, "SelectionWriteDone",
                GLib.Variant("(oub)", (self._session, int(serial), ok)), None,
                Gio.DBusCallFlags.NONE, CALL_TIMEOUT_MS, None)
        except GLib.Error as e:
            logger.warning(f"SelectionWriteDone failed: {e.message}")
        return ok

    def release_all(self):
        """Let go of everything this session is still holding — the
        emergency stop. Never raises."""
        if not self._session:
            return
        for b in list(self._held_buttons):
            try:
                self.button(b, False)
            except Exception:
                pass
        for k in list(self._held_keys):
            try:
                self.key(k, False)
            except Exception:
                pass


class _ClipboardServer(threading.Thread):
    """Serves the text paste_text put on the clipboard. The compositor
    asks for the data every time something pastes — the agent's ctrl+v now,
    the user's own later — through a SelectionTransfer signal, and that
    request must be answered promptly from a thread that is iterating a
    main context; the controller thread is busy sending keys at that
    moment. Daemon thread, one per PortalInput, lives with the process."""

    def __init__(self, owner):
        super().__init__(name="AutoCua-clipboard", daemon=True)
        self.owner = owner
        self.data = b""
        self.served = threading.Event()
        self.ctx = GLib.MainContext.new()
        self.loop = GLib.MainLoop(self.ctx)
        self._ready = threading.Event()

    def offer(self, data):
        self.data = data
        self.served.clear()
        self._ready.wait(2.0)

    def run(self):
        self.ctx.push_thread_default()
        bus = self.owner._connection()

        def on_transfer(_conn, _sender, _path, _iface, _signal, params):
            session, mime, serial = params.unpack()
            if session != self.owner._session:
                return
            if self.owner._selection_write(serial, self.data):
                self.served.set()

        bus.signal_subscribe(PORTAL_BUS, IFACE_CLIPBOARD, "SelectionTransfer",
                             PORTAL_PATH, None, Gio.DBusSignalFlags.NONE,
                             on_transfer)
        self._ready.set()
        self.loop.run()


_INPUT = None
_INPUT_LOCK = threading.Lock()


def get_input(create=True):
    """The process-wide PortalInput (one portal session per process), or
    None when `create` is False and none exists yet — so an emergency
    release never opens a consent dialog."""
    global _INPUT
    with _INPUT_LOCK:
        if _INPUT is None and create:
            _INPUT = PortalInput()
        return _INPUT


def _selftest():
    """`python -m AutoCua.linux.controller.tool.input_portal` — open the
    portal session (the one-time consent dialog appears here), save the
    restore token, and prove the pointer moves: it is parked at the centre
    of the primary monitor. Nothing is clicked or typed."""
    import sys
    from ...tree.element import get_screen
    inp = get_input()
    print("Opening the RemoteDesktop portal session — accept the consent "
          "dialog if one appears (only the first time)...")
    try:
        inp.ensure_session()
    except PortalInputError as e:
        print(f"FAILED: {e}")
        sys.exit(1)
    streams = ", ".join(f"node {n} at {x},{y} {w}x{h} scale {sc}"
                        for n, x, y, w, h, sc in inp._streams) or "none"
    granted = "+".join(n for b, n in ((DEVICE_KEYBOARD, "keyboard"),
                                      (DEVICE_POINTER, "pointer"))
                       if inp._devices & b) or "NONE"
    print(f"Session ready. Devices granted: {granted}. Monitor streams: {streams}")
    print(f"Restore token saved to {inp._token_file} — later sessions are silent.")
    screen = get_screen()
    cx, cy = screen["x"] + screen["width"] // 2, screen["y"] + screen["height"] // 2
    inp.move(cx, cy)
    print(f"Pointer moved to the screen centre ({cx}, {cy}). If it did not "
          f"move, clicks will not land either.")


if __name__ == "__main__":
    _selftest()
