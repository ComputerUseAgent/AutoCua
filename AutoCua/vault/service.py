import json
import os
import re
import time
import logging
import xml.etree.ElementTree as ET
from pathlib import Path

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


class VaultService:
    """Service for managing and retrieving credentials"""
    
    def __init__(self):
        self.element_tree_text = ""
        self.credentials = {}
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
                self.credentials = loaded
                logger.info(f"Loaded credentials for {len(self.credentials)} applications")
            else:
                logger.debug("credentials.json not found")
                self.credentials = {}
        except Exception as e:
            logger.error(f"Error loading credentials: {str(e)}")
            if not keep_on_error:
                self.credentials = {}
    
    def update_element_tree(self, element_tree_text):
        """Update the stored element tree"""
        self.element_tree_text = element_tree_text
        logger.debug("Vault element tree updated")
    
    def get_credential_for_element(self, element_number):
        """Get credential for a specific element based on app context and field type"""
        try:
            # Parse element tree to find app name and target element
            lines = self.element_tree_text.strip().split('\n')
            
            app_name = None
            target_element = None
            
            for line in lines:
                # Parse element info
                if f'[{element_number}]' in line:
                    target_element = self._parse_element_line(line)
                elif '[1]' in line and 'type="application"' in line:
                    # Element 1 is usually the application
                    app_info = self._parse_element_line(line)
                    if app_info:
                        app_name = app_info.get('label', '')
            
            if not app_name or not target_element:
                logger.error(f"Could not find app name or target element {element_number}")
                return None
            
            # Find matching credential
            credential = self._find_credential(app_name, target_element)
            return credential
            
        except Exception as e:
            logger.error(f"Error getting credential: {str(e)}")
            return None
    
    def get_pin(self):
        """(entry_name, pin) for the screen in front, for vault {"id": 0, "value": "pin"}.

        Stricter than the field lookup above, because a PIN pad locks after a
        few wrong tries:
          * re-reads credentials.json, so a PIN added while the app runs is used
          * matches the app name EXACTLY (case and spaces ignored) - no fuzzy
            or prefix match, which could hand one app's PIN to another
          * reads only "pin", never "code", and only if it is all digits
        Screens with no app in front (lock screen, home screen) use the entry
        named after the device: "iPhone" or "iPad". pin is None when nothing
        usable is saved; entry_name still says where to add it.
        """
        self._load_credentials(keep_on_error=True)
        app_name, device = "", "iPhone"
        for line in self.element_tree_text.splitlines():
            if line.startswith("current_device:"):
                device = (line.split(":", 1)[1].split() or [device])[0]
            elif line.lstrip().startswith("[1]<") and 'type="application"' in line:
                app_name = ((self._parse_element_line(line) or {}).get("label") or "").strip()
                break

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
    
    def _find_credential(self, app_name, element_info):
        """Find matching credential using fuzzy matching"""
        # Clean up app name for matching
        app_name_clean = app_name.lower().replace(' ', '').strip()
        element_label = element_info.get('label', '').lower()
        element_type = element_info.get('type', '').lower()

        # No app in front (lock screen, home screen: the root label is " "): an
        # empty name is "in" every key and would match the first entry.
        if not app_name_clean:
            logger.warning("No app in front, so no credential is matched")
            return None

        # Search through credentials
        for app_key, app_creds in self.credentials.items():
            app_key_clean = app_key.lower().replace(' ', '').strip()

            # The device passcode is never fuzzy-matched ("phone" is in
            # "iphone"); only get_pin reads it, on the passcode screen.
            if app_key_clean in DEVICE_ENTRIES:
                continue

            # Fuzzy match app name
            if (app_key_clean in app_name_clean or 
                app_name_clean in app_key_clean or
                self._fuzzy_match(app_key_clean, app_name_clean)):
                
                # Found matching app, now find field
                if 'email' in element_label or 'username' in element_label:
                    return app_creds.get('email') or app_creds.get('username')
                elif 'password' in element_label or element_type == 'securetextfield':
                    return app_creds.get('password')
                elif 'phone' in element_label:
                    return app_creds.get('phone')
                elif 'code' in element_label or 'pin' in element_label:
                    return app_creds.get('code') or app_creds.get('pin')
        
        logger.warning(f"No credential found for app: {app_name}, field: {element_label}")
        return None
    
    def _fuzzy_match(self, str1, str2):
        """Simple fuzzy matching for app names"""
        # Check if significant parts match
        if len(str1) < 3 or len(str2) < 3:
            return False
        
        # Check if one contains significant part of other
        if len(str1) > 4 and str1[:4] in str2:
            return True
        if len(str2) > 4 and str2[:4] in str1:
            return True
        
        return False


# Create global instance
vault_service = VaultService()