import logging
import time
import keyboard
from interception import Interception, KeyStroke

# Configure logger
logger = logging.getLogger(__name__)

# Interception driver constants for UAC handling
KEY_DOWN = 0
KEY_UP = 1
SCAN_ALT = 0x38
SCAN_Y = 0x15
SCAN_N = 0x31
SCAN_CTRL = 0x1D

class HotkeyService:
    """Service for sending keyboard shortcuts using the keyboard library.
    UAC shortcuts (alt+y, alt+n) are routed through Interception kernel driver
    to bypass the secure desktop where normal input is blocked."""
    
    def __init__(self, stop_event=None):
        self.stop_event = stop_event
    
    def _uac_accept(self) -> dict:
        """Accept UAC prompt by sending Alt+Y via Interception kernel driver"""
        try:
            if self.stop_event and self.stop_event.is_set():
                return {"status": "stopped", "action": "hotkey", "shortcut": "alt+y", "message": "Stopped by user"}
            
            logger.info("UAC - sending Alt+Y via Interception driver")

            from ..tool.kernel_input import ensure_attached
            if not ensure_attached():
                return {
                    "status": "error",
                    "action": "hotkey",
                    "shortcut": "alt+y",
                    "message": "Kernel input unavailable - Interception could not be attached"
                }

            ctx = Interception()
            if not ctx.valid:
                return {
                    "status": "error",
                    "action": "hotkey",
                    "shortcut": "alt+y",
                    "message": "Interception driver not installed"
                }
            
            from ..tool.kernel_input import keyboard_device
            kb = keyboard_device(ctx)
            
            try:
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_DOWN))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_Y, flags=KEY_DOWN))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_Y, flags=KEY_UP))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
                time.sleep(0.05)
            finally:
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
                ctx.send(kb, KeyStroke(code=SCAN_Y, flags=KEY_UP))
                ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_UP))
                time.sleep(0.1)
                del ctx
                from ..tool.kernel_input import release_when_idle
                release_when_idle()
            
            logger.info("UAC accepted via Interception driver")
            return {"status": "success", "action": "hotkey", "shortcut": "alt+y", "message": "UAC prompt accepted via Interception driver"}
            
        except Exception as e:
            logger.error(f"Error in UAC accept: {str(e)}")
            return {"status": "error", "action": "hotkey", "shortcut": "alt+y", "message": str(e)}
    
    def _uac_decline(self) -> dict:
        """Decline UAC prompt by sending Alt+N via Interception kernel driver"""
        try:
            if self.stop_event and self.stop_event.is_set():
                return {"status": "stopped", "action": "hotkey", "shortcut": "alt+n", "message": "Stopped by user"}
            
            logger.info("UAC - sending Alt+N via Interception driver")

            from ..tool.kernel_input import ensure_attached
            if not ensure_attached():
                return {
                    "status": "error",
                    "action": "hotkey",
                    "shortcut": "alt+n",
                    "message": "Kernel input unavailable - Interception could not be attached"
                }

            ctx = Interception()
            if not ctx.valid:
                return {
                    "status": "error",
                    "action": "hotkey",
                    "shortcut": "alt+n",
                    "message": "Interception driver not installed"
                }
            
            from ..tool.kernel_input import keyboard_device
            kb = keyboard_device(ctx)
            
            try:
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_DOWN))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_N, flags=KEY_DOWN))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_N, flags=KEY_UP))
                time.sleep(0.05)
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
                time.sleep(0.05)
            finally:
                ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
                ctx.send(kb, KeyStroke(code=SCAN_N, flags=KEY_UP))
                ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_UP))
                time.sleep(0.1)
                del ctx
                from ..tool.kernel_input import release_when_idle
                release_when_idle()
            
            logger.info("UAC declined via Interception driver")
            return {"status": "success", "action": "hotkey", "shortcut": "alt+n", "message": "UAC prompt declined via Interception driver"}
            
        except Exception as e:
            logger.error(f"Error in UAC decline: {str(e)}")
            return {"status": "error", "action": "hotkey", "shortcut": "alt+n", "message": str(e)}
    
    def send(self, shortcut: str) -> dict:
        """
        Send a keyboard shortcut.
        
        UAC shortcuts (alt+y, alt+n) route to Interception kernel driver.
        All others use the keyboard library.
        
        Args:
            shortcut (str): Keyboard shortcut (e.g., "ctrl+c", "f2", "ctrl+shift+s")
                           Max 3 keys combined
        
        Returns:
            dict: Result of shortcut execution
        """
        try:
            # Check for UAC shortcuts — route to kernel driver
            normalized = shortcut.lower().replace(" ", "")
            if normalized == "alt+y":
                return self._uac_accept()
            if normalized == "alt+n":
                return self._uac_decline()
            
            # Normal shortcuts — keyboard library
            keys = normalized.split("+")
            if len(keys) > 3:
                return {
                    "status": "error",
                    "action": "hotkey",
                    "shortcut": shortcut,
                    "message": f"Maximum 3 keys allowed, got {len(keys)}"
                }
            
            keyboard.send(shortcut)
            
            logger.info(f"Sent shortcut: {shortcut}")
            
            return {
                "status": "success",
                "action": "hotkey",
                "shortcut": shortcut
            }
            
        except Exception as e:
            logger.error(f"Error sending shortcut {shortcut}: {str(e)}")
            return {
                "status": "error",
                "action": "hotkey",
                "shortcut": shortcut,
                "message": str(e)
            }