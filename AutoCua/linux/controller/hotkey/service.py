# AutoCua/linux/controller/hotkey/service.py
# Linux version — keyboard shortcuts through the RemoteDesktop portal's
# virtual keyboard (tool/input_portal.py), the only route to a native Wayland
# window. Keys are sent as X keysyms, resolved by Gdk from the names the agent
# writes ("ctrl+shift+s", "f2", "enter"); the compositor maps each keysym to
# the keycode and level of the current keyboard layout — no hardcoded table.

import logging

from ..tool.input_portal import (get_input, keysym_for, PortalInputError,
                                 MODIFIER_KEYSYMS)

logger = logging.getLogger(__name__)


class HotkeyService:
    """Service for sending keyboard shortcuts via the desktop portal."""

    def __init__(self, stop_event=None):
        self.stop_event = stop_event

    def send(self, shortcut: str) -> dict:
        """
        Send a keyboard shortcut.

        Args:
            shortcut: Keyboard shortcut (e.g., "ctrl+c", "f2", "ctrl+shift+s").
                      Max 3 keys combined with "+". A macOS-style "cmd" is
                      taken as ctrl (see MODIFIER_KEYSYMS); "super"/"win" is
                      the Super key.

        Returns:
            dict: Result of shortcut execution.
        """
        try:
            if self.stop_event and self.stop_event.is_set():
                return {"status": "stopped", "action": "hotkey",
                        "shortcut": shortcut, "message": "Stopped by user"}

            normalized = (shortcut or "").lower().replace(" ", "")
            # A trailing "+" is the plus key itself ("ctrl++", or a bare "+"),
            # not a separator.
            plus_key = normalized.endswith("+")
            if plus_key:
                normalized = normalized[:-1]
            parts = [p for p in normalized.split("+") if p] + (["+"] if plus_key else [])

            if len(parts) > 3:
                return {
                    "status": "error", "action": "hotkey",
                    "shortcut": shortcut,
                    "message": f"Maximum 3 keys allowed, got {len(parts)}"
                }

            # Split modifiers from final key
            modifiers = []
            final = None
            for p in parts:
                if p in MODIFIER_KEYSYMS:
                    resolved = keysym_for(p)
                    if resolved:
                        modifiers.append(resolved)
                else:
                    final = keysym_for(p)
                    if final is None:
                        return {
                            "status": "error", "action": "hotkey",
                            "shortcut": shortcut,
                            "message": f"Unknown key: '{p}'"
                        }

            # Solo key (e.g. just "escape", "f5")
            if final is None and not modifiers:
                return {
                    "status": "error", "action": "hotkey",
                    "shortcut": shortcut, "message": "No key found in combo"
                }

            if final is None:
                # Single modifier sent alone — treat last as the key
                final = modifiers.pop()

            # Press modifiers down, tap final key, release modifiers
            get_input().chord(modifiers, final)

            logger.info(f"Sent shortcut: {shortcut}")

            return {
                "status": "success", "action": "hotkey",
                "shortcut": shortcut
            }

        except PortalInputError as e:
            logger.error(f"Error sending shortcut {shortcut}: {e}")
            return {
                "status": "error", "action": "hotkey",
                "shortcut": shortcut, "message": str(e)
            }
        except Exception as e:
            logger.error(f"Error sending shortcut {shortcut}: {str(e)}")
            return {
                "status": "error", "action": "hotkey",
                "shortcut": shortcut, "message": str(e)
            }
