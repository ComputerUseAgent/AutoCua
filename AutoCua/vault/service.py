import json
import os
import re
import time
import logging
import xml.etree.ElementTree as ET
from pathlib import Path
from urllib.parse import quote, quote_plus

import requests

logger = logging.getLogger(__name__)


# ─────────────────────────────── PIN keypads ──────────────────────────────────
# vault {"id": 0, "value": "pin"}: some PIN screens have nothing to type into.
# The iPhone lock screen and app PIN pads (Sky Go's "Sky Device PIN") are ten
# separate keys, so the vault taps the PIN itself and the model never sees it.
#
# Facts established live on an iPhone (iOS 26.6.2):
#   * Lock-screen keys are XCUIElementTypeKey "1".."0" (the scanner lists them
#     as buttons). The "Passcode field" value counts presses: "0 of 6 values
#     entered" -> "4 of 6". That count confirms every press registered.
#   * Sky Go's keys are statictext "1".."0" covering each key cell. Its entered
#     digits are not in the tree, so the only check is whether the pad closed.
#   * A key can appear twice (a button wrapping a same-named label).
#
# Anything that maps to a digit is as secret as the PIN: a key's element id,
# its coordinates, which press failed. None of it is logged or returned.

# One digit, optionally with its phone letters ("2 ABC", "2, A B C", "0 +").
# Phone letters are uppercase, so "1 of 3" and "5 min" do not match.
PIN_KEY_LABEL = re.compile(r"\s*([0-9])(?:[\s,]+[A-Z+][A-Z+ ]{0,8})?\s*")

# The native passcode field's value, e.g. "4 of 6 values entered".
PIN_ENTRY_COUNT = re.compile(r"(\d+) of (\d+) values entered")

# Finger down this long per key. Keys go one request each, back to back: each
# request takes ~0.4s on the device, which is also what keeps a repeated digit
# (0000) from reading as a double tap.
PIN_PRESS_MS = 80

# After the last key, before reading the screen back.
PIN_SETTLE_SECONDS = 1.0

# Tries per PIN entry per task. A pad that closes resets it; pads lock after a few wrong tries.
MAX_PIN_ATTEMPTS = 2

# Vault entries for the device itself (lock screen). Only get_pin reads these,
# and only while the native passcode field is on screen.
DEVICE_ENTRIES = ("iphone", "ipad")

# (connect, read) seconds for the WDA source dump - same bound as the scanner.
WDA_SOURCE_TIMEOUT = (5, 60)


def _pin_key(name):
    m = PIN_KEY_LABEL.fullmatch(name or "")
    return m.group(1) if m else None


def _centre(b):
    return b["x"] + b["width"] / 2.0, b["y"] + b["height"] / 2.0


def _inside(point, b):
    x, y = point
    return b["x"] <= x <= b["x"] + b["width"] and b["y"] <= y <= b["y"] + b["height"]


def _read_pin_screen(controller_service):
    """(entry count or None, keypad on screen) from a fresh WDA source dump."""
    response = requests.get(f"{controller_service.wda_url}/source",
                            params={"excluded_attributes": "visible,accessible"},
                            timeout=WDA_SOURCE_TIMEOUT)
    root = ET.fromstring(response.json()["value"])
    count, digits = None, set()
    for node in root.iter():
        m = PIN_ENTRY_COUNT.search(node.get("value") or "")
        if m and count is None:
            count = (int(m.group(1)), int(m.group(2)))
        digit = _pin_key(node.get("label") or node.get("name"))
        if digit is not None:
            digits.add(digit)
    return count, len(digits) == 10


def _tap_pin_keys(controller_service, session, points):
    """Tap each key with its own request, one after another on one session.

    Not one chained request: on Sky Go a four-tap chain landed two taps, a late
    third and no fourth. One tap per request is the shape controller_service.click
    uses, and measured ~0.42s per key, steady, on the device."""
    for x, y in points:
        response = requests.post(
            f"{controller_service.wda_url}/session/{session}/actions",
            json={"actions": [{"type": "pointer", "id": "finger1",
                               "parameters": {"pointerType": "touch"}, "actions": [
                                   {"type": "pointerMove", "duration": 0, "x": int(x), "y": int(y)},
                                   {"type": "pointerDown", "button": 0},
                                   {"type": "pause", "duration": PIN_PRESS_MS},
                                   {"type": "pointerUp", "button": 0}]}]},
            timeout=30)
        if response.status_code != 200:
            # Never echo the response: WDA can quote the coordinates back.
            raise RuntimeError("tap request rejected")


# ─────────────────────────────── Field fills ──────────────────────────────────
# vault {"id": 5, "value": "password"}: the value saved under the key the agent
# names, in the "ios" entry of the app in front (its exact name, case and
# spaces ignored), typed into [5] like `input`. The same rules as the web vault
# (web/controller/vault): the agent sees key names only (`block`), a password
# goes only into a password field, and what was typed reads as [vault] from
# then on (`hide`). Right before the keys go out, after the focus tap, the app
# scanned must still be in front: an earlier call in the step can switch apps.

# The fields a saved value may go into (the controller mapping's type).
TEXT_FIELDS = ("TextField", "SecureTextField", "TextView", "SearchField")

# The app WDA reports in front while a system screen (an alert) is up.
SPRINGBOARD = "com.apple.springboard"

# How long a tap gets to land focus in a field before the fill gives up.
FOCUS_WAIT_SECONDS = 1.5

# What a value the vault typed reads as, back on screen.
HIDDEN = "[vault]"

# The tree cuts a long label to its first 47 characters and "..."
# (ios/tree/element.py), so a longer value is hidden by those too.
CLIP_KEEP = 47

# The values the vault typed this chat, longest first.
_filled = []


def _norm(name):
    return str(name or "").lower().replace(" ", "")


def _text_of(value):
    """A saved value as the text to type: a non-empty string, or a whole
    number (a pin saved as 1234). Anything else is not typed."""
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str) and value:
        return value
    return None


def _usable_keys(creds):
    """The keys of an entry that hold something to type, as the file writes them."""
    return [k for k, v in creds.items() if _text_of(v) is not None]


def new_chat():
    """A fresh chat has typed nothing yet. A continuation keeps the list: the
    phone may still show what was typed."""
    _filled.clear()


def _remember(secret):
    """Hide `secret` from here on, in the forms a screen shows it back in: as
    typed, with its whitespace collapsed, and url-encoded. Added before a
    single key goes out, since a fill that fails halfway may have typed some."""
    for form in (secret, " ".join(secret.split()), quote(secret, safe=""), quote_plus(secret)):
        if form and form not in _filled:
            _filled.append(form)
    _filled.sort(key=len, reverse=True)


def hide(text):
    """`text` with every value the vault typed this chat shown as [vault]."""
    for secret in _filled:
        text = _hide_one(text, secret)
    return text


def hide_json(value):
    """Every string in a tool result, through `hide`."""
    if not _filled:
        return value
    if isinstance(value, str):
        return hide(value)
    if isinstance(value, list):
        return [hide_json(v) for v in value]
    if isinstance(value, dict):
        return {k: hide_json(v) for k, v in value.items()}
    return value


def _hide_one(text, secret):
    """`secret`, and any run of at least CLIP_KEEP of its first characters, as
    [vault]. A match that runs on into a longer word or number is that word,
    not the secret ("admin" in "administration"), and is left alone."""
    head = secret[:CLIP_KEEP]
    if not head:
        return text
    out, i = [], 0
    while True:
        at = text.find(head, i)
        if at < 0:
            break
        end = at + len(head)
        while end - at < len(secret) and end < len(text) and text[end] == secret[end - at]:
            end += 1
        before = text[at - 1] if at else ""
        after = text[end] if end < len(text) else ""
        joins = (before.isalnum() and secret[0].isalnum()) or (after.isalnum() and text[end - 1].isalnum())
        out.append(text[i:at])
        if joins:
            # Part of a longer word: keep its first character, look on.
            out.append(text[at])
            i = at + 1
        else:
            out.append(HIDDEN)
            i = end
    out.append(text[i:])
    return "".join(out)


class VaultService:
    """Service for managing and retrieving credentials"""

    def __init__(self):
        self.element_tree_text = ""
        self.credentials = {}
        # The bundle id of the app in front as the last scan read the screen
        # (begin_scan), for `_fill_check`; None after a scan that failed.
        self._next_bundle = None
        self._scan_bundle = None
        self._load_credentials()
    
    def _load_credentials(self, keep_on_error=False):
        """Load credentials from JSON file.

        keep_on_error: a reload while the app runs (get_pin) must not wipe what
        normal fills use when it catches a half-saved or mistyped file.
        """
        try:
            # AutoCua_data/vault/credentials.json — outside the install folder,
            # so uninstalling AutoCua can't delete the user's credentials.
            try:
                from AutoCua import vault_file
                credentials_path = str(vault_file())
            except Exception:
                current_dir = os.path.dirname(os.path.abspath(__file__))
                credentials_path = os.path.join(current_dir, 'credentials.json')

            if os.path.exists(credentials_path):
                with open(credentials_path, 'r', encoding='utf-8') as f:
                    loaded = json.load(f)
                if not isinstance(loaded, dict):
                    raise ValueError("credentials.json must hold an object")
                # One file for every device, a section each ("ios", "web", ...):
                # iOS reads only its own, so a site key can never match an app.
                section = loaded.get("ios", {})
                if not isinstance(section, dict):
                    raise ValueError('credentials.json\'s "ios" must hold an object')
                self.credentials = section
                logger.info(f"Loaded credentials for {len(self.credentials)} applications")
            else:
                logger.debug("credentials.json not found")
                self.credentials = {}
        except Exception as e:
            logger.error(f"Error loading credentials: {str(e)}")
            if not keep_on_error:
                self.credentials = {}
    
    def begin_scan(self):
        """A scan starts: note the app in front by bundle id, read just before
        the screen is, so the app and the tree belong together. Until the scan
        succeeds (update_element_tree) the vault types into nothing."""
        self._scan_bundle = None
        self._next_bundle = None
        if self.credentials:
            from ..ios.controller.tool.open_app import app_launcher_service
            self._next_bundle = (app_launcher_service.active_app() or {}).get("bundleId")

    def update_element_tree(self, element_tree_text):
        """Update the stored element tree, from the scan begin_scan started"""
        self.element_tree_text = element_tree_text
        self._scan_bundle = self._next_bundle
        logger.debug("Vault element tree updated")
    
    def _scanned_app(self):
        """(app name, device) of the screen last scanned: the root [1]
        application's label ("" on the home and lock screen) and the
        current_device header ("iPhone" when it is missing)."""
        app_name, device = "", "iPhone"
        for line in self.element_tree_text.splitlines():
            if line.startswith("current_device:"):
                device = (line.split(":", 1)[1].split() or [device])[0]
            elif line.lstrip().startswith("[1]<") and 'type="application"' in line:
                app_name = ((self._parse_element_line(line) or {}).get("label") or "").strip()
                break
        return app_name, device

    def _app_entry(self, app_name):
        """(entry name, its keys) saved for `app_name`: its exact name, case
        and spaces ignored. Never a device entry: those hold the lock-screen
        PIN, which only get_pin reads."""
        wanted = _norm(app_name)
        if not wanted or wanted in DEVICE_ENTRIES:
            return None
        for key, creds in self.credentials.items():
            if _norm(key) == wanted and isinstance(creds, dict):
                return key, creds
        return None

    def block(self):
        """<vault>: the key names saved for the app just scanned, or the
        device's PIN on the home and lock screen. Empty when nothing is saved."""
        self._load_credentials(keep_on_error=True)
        app_name, _ = self._scanned_app()
        found = self._app_entry(app_name)
        if found:
            name, keys = found[0], _usable_keys(found[1])
        elif not app_name:
            name, pin = self.get_pin()
            keys = ["pin"] if pin else []
        else:
            return ""
        if not keys:
            return ""
        return f'<vault app="{name}">\n{", ".join(keys)}\n</vault>'

    def fill(self, controller_service, element_id, key):
        """vault {"id", "value": <key>}: type the value saved under `key` for
        the app in front into [id], like `input`. The result names the key,
        never the value. A PIN keypad is enter_pin's, not this."""
        key = str(key or "").strip()
        result = {"action": "vault", "id": element_id, "value": key}

        def refuse(message):
            return {**result, "status": "error", "message": message}

        try:
            idx = int(element_id)
        except (TypeError, ValueError):
            return refuse(f"'{element_id}' is not a valid element id")
        elem = controller_service.elements.get(str(idx))
        if not elem:
            return refuse(f"no element [{idx}] in the current element_tree. Ids are re-assigned on "
                          "every scan - re-read the latest tree and use an id from it.")
        if not key:
            return refuse('vault\'s `value` is the name of a key in <vault>, like "username" or "password"')
        self._load_credentials(keep_on_error=True)
        app_name, _ = self._scanned_app()
        found = self._app_entry(app_name)
        if not found:
            return refuse(f"nothing is saved in the vault for {app_name or 'the home screen'}: "
                          "use what the task gives, or ask the user")
        entry, creds = found
        name = next((k for k in creds if k.strip().lower() == key.lower()), None)
        if name is None:
            return refuse(f'no "{key}" is saved for {entry}: <vault> has {", ".join(_usable_keys(creds))}')
        secret = _text_of(creds[name])
        if secret is None:
            return refuse(f'the "{name}" saved for {entry} is empty or not text: ask the user to fix it '
                          'under "ios" in AutoCua_data/vault/credentials.json')
        password = "password" in name.lower()
        kind = elem.get("type") or "element"
        if kind not in TEXT_FIELDS:
            return refuse(f"[{idx}] is a {kind.lower()}, not a text field")
        if password and kind != "SecureTextField":
            return refuse(f"[{idx}] is a {kind.lower()}, and a saved password goes only into a password field")

        _remember(secret)
        from ..ios.controller.service import TypingRefused
        try:
            controller_service.type_text(idx, secret,
                                         check=lambda session: self._fill_check(controller_service, session, password))
        except TypingRefused as e:
            return refuse(f"nothing typed: {e}")
        except Exception:
            # Never the error's own text: a WDA reply can quote what was typed.
            return refuse("the typing failed on the phone, and part of the value may be in the field: "
                          "check the next screenshot")
        logger.info(f"🔐 Vault filled element {idx} ({name})")
        label = str(elem.get("name") or "").strip()
        target = f"[{idx}] ({label})" if label and label != "no_label" else f"[{idx}]"
        return {**result, "status": "success", "message": f"filled {target} with the saved {name} for {entry}"}

    def _fill_check(self, controller_service, session, password):
        """type_text's check, after the focus tap and before a single key: the
        field that has focus takes this value, and the app the scan read is
        still in front with no system screen up (asked last, right before the
        keys). "" when all hold. A phone that cannot answer gets nothing."""
        # A tap that starts an animation (Settings' search bar slides up) can
        # leave WDA without an answer for a moment: ask again until FOCUS_WAIT.
        base = f"{controller_service.wda_url}/session/{session}"
        deadline = time.monotonic() + FOCUS_WAIT_SECONDS
        while True:
            try:
                ref = requests.get(f"{base}/element/active", timeout=10).json().get("value") or {}
                uid = ref.get("ELEMENT") or ref.get("element-6066-11e4-a52e-4f735466cecf")
                kind = requests.get(f"{base}/element/{uid}/attribute/type", timeout=10).json().get("value") if uid else ""
            except Exception:
                kind = ""
            # An error reply is a dict: no answer yet, never text for a message.
            kind = kind.replace("XCUIElementType", "") if isinstance(kind, str) else ""
            if kind or time.monotonic() >= deadline:
                break
            time.sleep(0.2)
        if not kind:
            return "no field took focus after the tap"
        if password and kind != "SecureTextField":
            return f"focus is on a {kind.lower()}, and a saved password goes only into a password field"
        if kind not in TEXT_FIELDS:
            return f"focus is on a {kind.lower()}, not a text field"
        from ..ios.controller.tool.open_app import app_launcher_service
        live = (app_launcher_service.active_app() or {}).get("bundleId")
        if not live or live == SPRINGBOARD or live != self._scan_bundle:
            return "the app that was scanned is no longer in front, or a system screen is up"
        return ""

    def get_pin(self):
        """(entry_name, pin) for the screen in front, for vault {"id": 0, "value": "pin"}.

        Careful, because a PIN pad locks after a few wrong tries:
          * re-reads credentials.json, so a PIN added while the app runs is used
          * matches the app name EXACTLY (case and spaces ignored), like `fill`
          * reads only "pin", never "code", and only if it is all digits
        Screens with no app in front (lock screen, home screen) use the entry
        named after the device: "iPhone" or "iPad". pin is None when nothing
        usable is saved; entry_name still says where to add it.
        """
        self._load_credentials(keep_on_error=True)
        app_name, device = self._scanned_app()
        name = app_name or device
        wanted = name.lower().replace(" ", "")
        for app_key, app_creds in self.credentials.items():
            if app_key.lower().replace(" ", "") == wanted and isinstance(app_creds, dict):
                pin = str(app_creds.get("pin") or "")
                return name, (pin if re.fullmatch(r"[0-9]+", pin) else None)
        return name, None

    def find_pin_keys(self, elements):
        """{digit: (x, y)} for a full on-screen digit keypad, or None.

        `elements` is the controller's mapping from the latest scan. All ten
        digits must be there, each resolving to exactly one key - checking only
        the PIN's own digits would happily tap a "1" badge four times for 1111.
        """
        candidates = {}
        for elem in (elements or {}).values():
            digit = _pin_key(elem.get("name"))
            if digit is not None and elem.get("bounds"):
                candidates.setdefault(digit, []).append(elem["bounds"])

        keys = {}
        for digit in "0123456789":
            kept = []
            # Outermost first: a label whose centre sits inside a kept key is
            # that same key, not a second one.
            for b in sorted(candidates.get(digit, []), key=lambda b: -(b["width"] * b["height"])):
                if not any(_inside(_centre(b), k) for k in kept):
                    kept.append(b)
            if len(kept) != 1:
                return None
            keys[digit] = _centre(kept[0])
        return keys

    def enter_pin(self, controller_service, keys, pin, device_entry=False):
        """Tap `pin` on the keypad from find_pin_keys, then confirm it registered.

        Returns {"status", "message", "pressed", "closed"}: `pressed` says a
        wrong-PIN try may have been spent, `closed` that the PIN screen went
        away. Messages never contain a digit, a key id or a coordinate.
        """
        def result(status, message, pressed, closed=False):
            return {"status": status, "message": message, "pressed": pressed, "closed": closed}

        # Re-read the screen right before tapping: the keys come from the scan
        # taken before the model thought, and the pad may be gone by now.
        try:
            before, keypad_open = _read_pin_screen(controller_service)
        except Exception:
            return result("error", "Could not read the screen, nothing was pressed. It is safe to try once more.", False)
        if not keypad_open:
            return result("error", "The PIN keypad is no longer on screen, nothing was pressed. "
                                   "Open the PIN screen again first.", False)
        if device_entry and before is None:
            # The device passcode only ever goes into the native passcode field.
            return result("error", "No iPhone passcode field on screen, so the device PIN was not used. "
                                   "Open the lock-screen passcode first.", False)
        if before is not None and (before[0] != 0 or len(pin) != before[1]):
            return result("error", "The passcode field is not empty, or the saved PIN does not fit this "
                                   "passcode, so nothing was pressed. Ask the user.", False)

        session = controller_service.get_session()
        if not session:
            return result("error", "No WDA session, nothing was pressed. It is safe to try once more.", False)
        try:
            _tap_pin_keys(controller_service, session, [keys[d] for d in pin])
        except Exception:
            return result("error", "The key presses may have gone through. Do not retry - "
                                   "check the next screenshot, and ask the user if unsure.", True)
        time.sleep(PIN_SETTLE_SECONDS)

        try:
            after, keypad_open = _read_pin_screen(controller_service)
        except Exception:
            return result("success", "PIN keys pressed, but the screen could not be read back. "
                                     "Check the next screenshot before doing anything else.", True)

        if before is not None:
            if after is None:
                return result("success", "The passcode screen closed. Confirm on the next screenshot "
                                         "that the device unlocked.", True, closed=True)
            entered, total = after
            if entered == total:
                return result("success", f"PIN submitted, every key press registered ({entered} of {total}). "
                                         "Confirm on the next screenshot that it was accepted.", True)
            if entered == 0:
                # The field was empty and the PIN fills it exactly, so a reset
                # to 0 means iOS rejected the code (a try was spent).
                return result("error", "The PIN was not accepted. Do not retry - "
                                       "ask the user to check the PIN saved in the vault.", True)
            return result("error", f"Only part of the PIN registered ({entered} of {total} values entered). "
                                   "Do not retry - ask the user.", True)

        if not keypad_open:
            return result("success", "PIN entered and the keypad closed.", True, closed=True)
        # No entry count to read, and the pad is still up after the whole PIN:
        # it was not accepted, or a press did not land. Never call that success.
        return result("error", "The keypad is still open after the whole PIN: it was not accepted or a press "
                               "did not register. Do not retry - if the boxes are full and an OK/Continue button "
                               "shows, tap it; otherwise ask the user.", True)

    def _parse_element_line(self, line):
        """Parse element line to extract attributes"""
        try:
            # Two tree dialects reach here:
            #   iOS  : [1]<element_name="Instagram", type="application" />
            #   older: [1]<type="button", label="Back", value="" />
            # `element_name` is what ios/tree/element.py actually emits today, so
            # read the label from either key rather than only `label=`.
            attrs = {}
            
            # Extract type
            if 'type="' in line:
                start = line.find('type="') + 6
                end = line.find('"', start)
                attrs['type'] = line[start:end]
            
            # Extract label (element_name= wins; it is the iOS spelling)
            for key in ('element_name="', 'label="'):
                if key in line:
                    start = line.find(key) + len(key)
                    end = line.find('"', start)
                    attrs['label'] = line[start:end]
                    break
            
            # Extract value
            if 'value="' in line:
                start = line.find('value="') + 7
                end = line.find('"', start)
                attrs['value'] = line[start:end]
            
            return attrs
        except Exception as e:
            logger.error(f"Error parsing element line: {str(e)}")
            return None


# Create global instance
vault_service = VaultService()