# AutoCua/linux/controller/service.py
# Linux version — every pointer action goes through the XDG RemoteDesktop
# portal (tool/input_portal.py). On Wayland that is the only unprivileged way
# to reach a native window: the AT-SPI event generator, pynput and pyautogui
# all inject through XWayland, which native windows never see (measured on
# GNOME 50 — see input_portal.py). An element the scanner resolved to an
# AT-SPI ref can ALSO be activated through its Action interface, which needs
# neither consent nor a pointer: that is the path for partially visible
# elements (the macOS scanner's AXPress rule) and the fallback when the
# portal session is unavailable.

import logging
import time

from ..tree.element import (enable_screen_reader_flag, _get_fast, _bus_path_of,
                            Gio, GLib)
from .tool.input_portal import (get_input, keysym_for, PortalInputError,
                                BTN_LEFT, BTN_RIGHT)

logger = logging.getLogger(__name__)

# Wheel notches per scroll action. One notch scrolls ~3 lines in GTK and a
# few dozen pixels in Chromium — five is a modest, visible nudge, the same
# intent as the macOS scanner's 3x5 line events.
SCROLL_NOTCHES = 5

# AT-SPI action names that mean "activate this control", in preference
# order. GTK publishes "click" / "press" / "activate"; Gecko "jump" on links
# and "click" elsewhere; Chromium answers GetName(i) with "doDefault".
_ACTIVATE_ACTIONS = ("click", "press", "activate", "dodefault", "jump",
                     "clickancestor")

_A11Y_ACTION_IFACE = "org.a11y.atspi.Action"

# Below this many characters the keyboard finishes sooner than a paste, so a
# short value is typed. Measured in GNOME Text Editor on a GNOME 50 Wayland
# desktop: a paste costs a flat ~0.75 s at any length (one clipboard hand-off
# plus fixed settles), keystrokes 0.221 s + 43.7 ms per character. The two
# curves meet at 12.1 characters — 12 chars measured dead even, 14 already
# favours the paste.
KEYSTROKE_FASTER_BELOW = 12

# Apps that need slow character-by-character typing (the macOS controller's
# Terminal / iTerm2 rule, for the terminals GNOME and friends ship).
SLOW_TYPING_APPS = [
    "terminal",
    "ptyxis",
    "konsole",
    "xterm",
    "alacritty",
    "kitty",
    "tilix",
]


class ControllerService:
    def __init__(self, stop_event=None):
        """Initialize the Controller Service"""
        self.elements_mapping = {}
        self.application_name = ""
        self.stop_event = stop_event

        # The accessibility bus is where every element comes from and what
        # the Action interface rides on — make sure the desktop publishes.
        if enable_screen_reader_flag():
            logger.info("Accessibility bus OK")
        else:
            logger.warning(
                "Accessibility is not enabled on this desktop — element trees will "
                "be empty and nothing can be clicked by element. Check that "
                "at-spi2-core is installed and org.a11y.Bus is running.")

    def release_all_inputs(self):
        """Emergency release all hardware inputs (keyboard + mouse) the portal
        session is still holding. Never opens a session just to do it."""
        inp = get_input(create=False)
        if inp is None:
            return
        try:
            inp.release_all()
            logger.info("Emergency release: all portal inputs released")
        except Exception as e:
            logger.error(f"Emergency release failed: {e}")

    # ------------------------------------------------------------------
    # AT-SPI Action — activate an element without a pointer
    # ------------------------------------------------------------------

    def _atspi_activate(self, element_info):
        """Activate an element through its AT-SPI Action interface.

        Returns (ok, why). The scanner keeps a ref on every element it
        publishes — an Atspi.Accessible from the per-node walk or a
        (bus, path) pair from the bulk reader — and both resolve to one raw
        D-Bus address here; OCR elements carry None and cannot be activated
        this way. GetActions lists what the widget offers; the first name in
        _ACTIVATE_ACTIONS that it has is performed."""
        ref = element_info.get("acc_element")
        if ref is None:
            return False, "no accessibility ref (OCR element)"
        fast = _get_fast()
        if fast is None:
            return False, "accessibility bus unavailable"
        try:
            bus, path = ref if isinstance(ref, tuple) else _bus_path_of(ref)
        except Exception as e:
            return False, f"no bus path ({e})"
        if not bus or not path:
            return False, "no object path"
        try:
            actions = fast.conn.call_sync(
                bus, path, _A11Y_ACTION_IFACE, "GetActions", None, None,
                Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        except GLib.Error as e:
            return False, f"no Action interface ({e.message})"
        names = [str(a[0]).lower() for a in (actions or [])]
        # Chromium (Chrome, Electron) lists its actions with EMPTY names —
        # measured: GetActions -> [('', '', ''), ('', '', '')] while
        # GetName(0) answers "doDefault" — and gnome-shell's tiles carry no
        # names at all. Fill the blanks through GetName; a list that stays
        # anonymous is taken as "action 0 is the default", which is what ATK
        # promises for the first action.
        for i, n in enumerate(names):
            if n:
                continue
            try:
                names[i] = str(fast.conn.call_sync(
                    bus, path, _A11Y_ACTION_IFACE, "GetName",
                    GLib.Variant("(i)", (i,)), None,
                    Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]).lower()
            except GLib.Error:
                pass
        index, wanted = None, None
        for candidate in _ACTIVATE_ACTIONS:
            if candidate in names:
                index, wanted = names.index(candidate), candidate
                break
        if index is None and names and not any(names):
            index, wanted = 0, "default action"
        if index is None:
            return False, f"no activate action (has: {', '.join(n for n in names if n) or 'none'})"
        try:
            ok = fast.conn.call_sync(
                bus, path, _A11Y_ACTION_IFACE, "DoAction",
                GLib.Variant("(i)", (index,)), None,
                Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]
        except GLib.Error as e:
            return False, f"DoAction failed ({e.message})"
        return bool(ok), f"AT-SPI action '{wanted}'"

    # ------------------------------------------------------------------
    # Click coordinate calculation
    # ------------------------------------------------------------------

    def _get_click_coords_for_element(self, element_info):
        """The element's geometric center. That is the click point.

        Pure arithmetic on the rect the scanner already resolved — logical
        screen coordinates, offset-corrected — which is also what the
        annotated screenshot shows the model, so the click lands where the
        model was told the element is. Partially visible elements click the
        center of `visible_rect`, so the point is on the part actually on
        screen rather than the middle of a clipped box.
        """
        visibility = element_info.get('visibility', 'full')
        if visibility.startswith('partial'):
            rect = element_info.get('visible_rect') or element_info['rect']
        else:
            rect = element_info['rect']

        return (rect.left + (rect.right - rect.left) // 2,
                rect.top + (rect.bottom - rect.top) // 2)

    def _lookup(self, index, action):
        """(element_info, None) or (None, error_dict): the index and
        visibility checks every element action starts with."""
        if index not in self.elements_mapping:
            return None, {"status": "error", "action": action, "index": index,
                          "message": f"Element index {index} not found"}
        element_info = self.elements_mapping[index]
        if element_info.get('visibility', 'full') == 'hidden':
            clipped_by = element_info.get('clipped_by', 'unknown container')
            return None, {"status": "error", "action": action, "index": index,
                          "message": f"Element is hidden (clipped by '{clipped_by}'). "
                                     f"Scroll to make it visible first."}
        return element_info, None

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def set_elements(self, elements_mapping, application_name=""):
        """Set the elements mapping from scanner"""
        self.elements_mapping = elements_mapping
        self.application_name = application_name
        logger.info(f"Controller received {len(self.elements_mapping)} elements for '{application_name}'")

    def _click_element(self, index, count, action):
        """Left-click an element `count` times: pointer through the portal,
        AT-SPI Action for the partially visible (the macOS AXPress rule) and
        as the fallback when the portal cannot be used."""
        try:
            index = str(index)
            element_info, err = self._lookup(index, action)
            if err:
                return err
            name = element_info.get('name', 'Unknown')

            if count == 1 and element_info.get('visibility', 'full').startswith('partial'):
                ok, why = self._atspi_activate(element_info)
                if ok:
                    time.sleep(1.0)
                    logger.info(f"Activated element {index} (partial) via {why}")
                    return {"status": "success", "action": action, "index": index,
                            "element_name": name}
                logger.info(f"AT-SPI activation unavailable for element {index} ({why}), "
                            f"falling back to a pointer click")

            click_x, click_y = self._get_click_coords_for_element(element_info)
            try:
                get_input().click(click_x, click_y, button=BTN_LEFT, count=count)
            except PortalInputError as e:
                # No portal session (consent refused, no portal): an accessible
                # control can still be activated without a pointer.
                why = "not attempted (multi-click)"
                if count == 1:
                    ok, why = self._atspi_activate(element_info)
                    if ok:
                        time.sleep(1.0)
                        logger.info(f"Activated element {index} via {why} (portal: {e})")
                        return {"status": "success", "action": action, "index": index,
                                "element_name": name}
                logger.error(f"Pointer click failed for element {index}: {e}")
                return {"status": "error", "action": action, "index": index,
                        "message": f"{e} AT-SPI fallback: {why}."}
            time.sleep(1.0)

            logger.info(f"{action} element {index} at ({click_x}, {click_y})")
            return {"status": "success", "action": action, "index": index,
                    "element_name": name}

        except Exception as e:
            logger.error(f"Error in {action} on element {index}: {str(e)}")
            return {"status": "error", "action": action, "index": index, "message": str(e)}

    def click(self, index):
        """Click on element by index"""
        return self._click_element(index, 1, "click")

    def double_click(self, index):
        """Double-click on element by index"""
        return self._click_element(index, 2, "double_click")

    def triple_click(self, index):
        """Triple-click on element by index (select entire line)."""
        return self._click_element(index, 3, "triple_click")

    def right_click(self, index):
        """Right-click on element by index"""
        try:
            index = str(index)
            element_info, err = self._lookup(index, "right_click")
            if err:
                return err

            click_x, click_y = self._get_click_coords_for_element(element_info)
            try:
                get_input().click(click_x, click_y, button=BTN_RIGHT, count=1)
            except PortalInputError as e:
                logger.error(f"Right click failed for element {index}: {e}")
                return {"status": "error", "action": "right_click", "index": index,
                        "message": str(e)}
            time.sleep(1.0)

            logger.info(f"Right-clicked element {index} at ({click_x}, {click_y})")
            return {"status": "success", "action": "right_click", "index": index,
                    "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error right-clicking element {index}: {str(e)}")
            return {"status": "error", "action": "right_click", "index": index, "message": str(e)}

    def scroll(self, index, direction):
        """Scroll an element in a specified direction"""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "scroll", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]
            rect = element_info.get('visible_rect') or element_info['rect']

            center_x = rect.left + (rect.right - rect.left) // 2
            center_y = rect.top + (rect.bottom - rect.top) // 2

            d = (direction or "").lower()
            if d not in ("up", "down", "left", "right"):
                return {"status": "error", "action": "scroll", "index": index,
                        "message": f"Invalid scroll direction: {direction}. Use 'up', 'down', 'left', or 'right'"}

            # Discrete wheel notches at the element's centre: the pointer moves
            # there first so the widget under it receives the wheel.
            notches = SCROLL_NOTCHES if d in ("down", "right") else -SCROLL_NOTCHES
            try:
                get_input().scroll(center_x, center_y, notches,
                                   horizontal=d in ("left", "right"))
            except PortalInputError as e:
                logger.error(f"Scroll failed for element {index}: {e}")
                return {"status": "error", "action": "scroll", "index": index,
                        "message": str(e)}

            logger.info(f"Scrolled element {index} {direction} at position ({center_x}, {center_y})")
            time.sleep(0.5)

            return {"status": "success", "action": "scroll", "index": index,
                    "direction": direction, "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error scrolling element {index}: {str(e)}")
            return {"status": "error", "action": "scroll", "index": index, "message": str(e)}

    # ------------------------------------------------------------------
    # Typing — the macOS controller's two paths, on the portal keyboard
    # ------------------------------------------------------------------
    #   1. keystrokes: one key event per character (tool/input_portal.py
    #      type_text) — what terminals and key-watching widgets need, and
    #      the only route left when a paste cannot land;
    #   2. insertion: the value goes through the portal clipboard and one
    #      ctrl+v (paste_text). The whole string lands at once — a field
    #      fills in well under a second instead of 40 ms per character —
    #      and auto-indenting editors (CodeMirror, Monaco) don't compound
    #      the indentation each Return would trigger.
    # BOTH typing tools insert. `input` targets an element it just clicked
    # and cleared; `typewrite` goes to whatever holds focus — and that is
    # the busier path, because the model's habit is left_click on a field
    # followed by typewrite rather than input (measured over a real run).
    # Leaving typewrite on keystrokes would have left the slow path in
    # charge of most typing.
    # Keystrokes are kept where they win or where a paste cannot land: a
    # SHORT ascii value (KEYSTROKE_FASTER_BELOW), a TERMINAL (ctrl+v is not
    # paste there — the SLOW_TYPING_APPS list), a session with no clipboard,
    # and a widget that never collects the paste. The last two fall back on
    # their own, so nothing regresses.

    def _type_value(self, value, action, insert=False):
        """Type or paste `value` into whatever has focus. Returns (result,
        pasted): result is None on success or the dict to return (stopped /
        error); pasted says the clipboard did the work, keystrokes otherwise.
        `insert` offers the clipboard for single-line text too (both typing
        tools pass it), subject to the length and terminal rules below; a
        multi-line value always takes the clipboard."""
        inp = get_input()
        is_slow_app = any(app in self.application_name.lower()
                          for app in SLOW_TYPING_APPS)
        # A multi-line value ALWAYS inserts: typing its Returns would make an
        # editor auto-indent and a terminal execute each line. Otherwise insert
        # only where it beats the keyboard — never in a terminal, and not for a
        # short value the keyboard finishes sooner. Non-ASCII always inserts
        # whatever its length: the keystroke path silently drops characters the
        # active layout has no key for (measured: ä ñ ö ü on a GB layout).
        paste_wins = insert and not is_slow_app and (
            len(value) >= KEYSTROKE_FASTER_BELOW or not value.isascii())
        if value and ('\n' in value or paste_wins):
            if self.stop_event and self.stop_event.is_set():
                return {"status": "stopped", "action": action, "message": "Stopped by user"}, False
            try:
                inp.paste_text(value)
                return None, True
            except PortalInputError as e:
                logger.info(f"paste unavailable ({e}); typing the value instead")
        interval = 0.05 if is_slow_app else 0.04
        if not inp.type_text(value, interval=interval, stop=self.stop_event):
            return {"status": "stopped", "action": action, "message": "Stopped by user"}, False
        return None, False

    def input(self, index, value):
        """Input text into element by index: click it, clear it, then insert
        the value whole through the clipboard (keystrokes only where a paste
        cannot land - see _type_value)."""
        try:
            index = str(index)
            element_info, err = self._lookup(index, "input")
            if err:
                return err
            value = "" if value is None else str(value)

            click_x, click_y = self._get_click_coords_for_element(element_info)
            inp = get_input()
            inp.click(click_x, click_y, button=BTN_LEFT, count=1)
            time.sleep(0.1)

            # Select all and delete existing content (ctrl+a on Linux)
            inp.chord([keysym_for("ctrl")], keysym_for("a"))
            time.sleep(0.05)
            inp.tap(keysym_for("backspace"))
            time.sleep(0.05)

            halted, pasted = self._type_value(value, "input", insert=True)
            if halted:
                return halted

            logger.info(f"{'Pasted' if pasted else 'Typed'} '{value}' into element {index}")

            # A paste has already landed whole (paste_text waits for the app
            # to collect it); only keystrokes need the per-character settle.
            wait_time = 0.5 if pasted else max(0.5, len(value) * 0.02)
            time.sleep(wait_time)

            return {
                "status": "success",
                "action": "input",
                "index": index,
                "value": value,
                "element_name": element_info.get('name', 'Unknown'),
                "message": "verify yourself using Raw Vision"
            }

        except PortalInputError as e:
            logger.error(f"Error inputting to element {index}: {e}")
            return {"status": "error", "action": "input", "index": index, "message": str(e)}
        except Exception as e:
            logger.error(f"Error inputting to element {index}: {str(e)}")
            return {"status": "error", "action": "input", "index": index, "message": str(e)}

    def typewrite(self, text):
        """Insert text at whatever holds focus (no element targeting): the
        value goes in whole through the clipboard, falling back to
        keystrokes in a terminal or wherever a paste cannot land (see
        _type_value)."""
        try:
            text = "" if text is None else str(text)
            halted, _ = self._type_value(text, "typewrite", insert=True)
            if halted:
                logger.info("typewrite interrupted by stop_event")
                return halted
            time.sleep(0.22)

            logger.info(f"Canvas input: typed '{text}' ({len(text)} chars)")
            return {"status": "success", "action": "typewrite", "text": text,
                    "message": "verify yourself using visual"}

        except PortalInputError as e:
            logger.error(f"typewrite failed: {e}")
            return {"status": "error", "action": "typewrite", "message": str(e)}
        except Exception as e:
            logger.error(f"typewrite failed: {e}")
            return {"status": "error", "action": "typewrite", "message": str(e)}
