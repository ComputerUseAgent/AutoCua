import time
import logging
import ctypes
import ctypes.wintypes
import threading
from interception import Interception, KeyStroke

logger = logging.getLogger(__name__)

# Interception key flags
KEY_DOWN = 0
KEY_UP = 1

# Modifier scan codes
SCAN_LSHIFT = 0x2A
SCAN_CTRL = 0x1D
SCAN_ALT = 0x38

# Windows API
user32 = ctypes.windll.user32
MAPVK_VK_TO_VSC = 0


# --------------------------------------------------------------------------
# Interception driver: bound to the built-in keyboard only
# --------------------------------------------------------------------------
# Interception used to be installed as an UpperFilter on the whole KEYBOARD CLASS,
# so every keyboard passed through it. It has 10 keyboard slots that are never
# freed, so each reconnect of a wireless keyboard burned one; once exhausted, a
# reconnecting keyboard enumerated fine but delivered no input until reboot. That
# repeatedly killed a user's keyboard (see INTERCEPTION_DRIVER.md).
#
# windows_setup.bat now binds the driver to the BUILT-IN keyboard alone, via a
# device-level UpperFilters on that one device. It is non-removable, so it takes
# exactly one slot at boot and never another - and no other keyboard is ever
# filtered, so no keyboard can be starved. Injection rides on that binding.
#
# There is deliberately no runtime attach/detach: measured on a clean boot,
# attaching mid-session binds NO injection slot AND leaves the physical keyboard
# dead until detached. Interception only binds slots when it loads at boot.

_driver_lock = threading.RLock()
_kb_slot = None
_mouse_slot = None

VK_LSHIFT = 0xA0
MOUSE_MOVE_RELATIVE = 0x000


def ensure_attached() -> bool:
    """True when there is a live slot we can actually inject through.

    Not a registry check: the \\.\\interceptionNN device objects exist whenever the
    driver is loaded, so ctx.valid is True even when nothing is bound and every
    keystroke vanishes. The only honest test is to probe for a slot that really
    delivers, which keyboard_device() does once and caches.
    """
    with _driver_lock:
        if _kb_slot is not None:
            return True
        try:
            ctx = Interception()
            if not ctx.valid:
                return False
            found = keyboard_device(ctx, probe_only=True)
            del ctx
            return found is not None
        except Exception as e:
            logger.warning(f"Interception availability check failed: {e}")
            return False


# Kept so existing call sites read naturally; nothing is toggled at runtime.
def driver_ready() -> bool:
    return ensure_attached()


def keyboard_device(ctx, probe_only: bool = False):
    """The keyboard slot that actually delivers strokes.

    Interception hands out its 0-9 keyboard slots in device-arrival order, and the
    library hard-codes 1 - which is simply wrong here, where the built-in keyboard
    binds to slot 0. Sending to a dead slot fails silently, so probe for the real
    one. A bare Left Shift is invisible to the user but shows up in the key state.

    probe_only=True returns None instead of falling back, so callers can tell
    "no injection available" from "found a slot".
    """
    global _kb_slot
    if _kb_slot is not None:
        return _kb_slot
    for idx in range(0, 10):
        try:
            try:
                ctx.send(idx, KeyStroke(code=SCAN_LSHIFT, flags=KEY_DOWN))
                time.sleep(0.03)
                pressed = bool(user32.GetAsyncKeyState(VK_LSHIFT) & 0x8000)
            finally:
                # Never leave Shift stuck down - it would mangle everything typed after.
                ctx.send(idx, KeyStroke(code=SCAN_LSHIFT, flags=KEY_UP))
                time.sleep(0.02)
            if pressed:
                _kb_slot = idx
                logger.info(f"Interception keyboard slot detected: {idx}")
                return idx
        except Exception:
            continue
    if probe_only:
        return None
    logger.warning("No live Interception keyboard slot found; using library default")
    return ctx.keyboard


def mouse_device(ctx) -> int:
    """The mouse slot that actually delivers strokes (same arrival-order caveat)."""
    global _mouse_slot
    if _mouse_slot is not None:
        return _mouse_slot
    from interception import MouseStroke
    pt = ctypes.wintypes.POINT()
    for idx in range(10, 20):
        try:
            user32.GetCursorPos(ctypes.byref(pt))
            before = (pt.x, pt.y)
            ctx.send(idx, MouseStroke(MOUSE_MOVE_RELATIVE, 0, 0, 2, 0))
            time.sleep(0.03)
            user32.GetCursorPos(ctypes.byref(pt))
            if (pt.x, pt.y) != before:
                ctx.send(idx, MouseStroke(MOUSE_MOVE_RELATIVE, 0, 0, -2, 0))  # put it back
                _mouse_slot = idx
                logger.info(f"Interception mouse slot detected: {idx}")
                return idx
        except Exception:
            continue
    logger.warning("No live Interception mouse slot found; using library default")
    return ctx.mouse


def release_when_idle(delay: float = 0.0):
    """No-op. The driver's binding is set once at install and never toggled.

    Kept so the injection paths still read as "done injecting" without each call
    site needing to know that nothing has to be released.
    """
    return


def _char_to_scancode(char: str) -> tuple:
    """
    Resolve a character to (scancode, needs_shift, needs_ctrl, needs_alt)
    using the current keyboard layout via Windows API.
    """
    hkl = user32.GetKeyboardLayout(0)
    
    result = user32.VkKeyScanExW(ord(char), hkl)
    
    if result == -1 or result == 0xFFFF:
        logger.warning(f"Character '{char}' not mappable on current keyboard layout")
        return None
    
    vk = result & 0xFF
    shift_state = (result >> 8) & 0xFF
    
    needs_shift = bool(shift_state & 0x01)
    needs_ctrl = bool(shift_state & 0x02)
    needs_alt = bool(shift_state & 0x04)
    
    scancode = user32.MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, hkl)
    
    if scancode == 0:
        logger.warning(f"VK 0x{vk:02X} for '{char}' has no scancode mapping")
        return None
    
    return (scancode, needs_shift, needs_ctrl, needs_alt)


def release_all_inputs():
    """Emergency release all keyboard modifiers and mouse buttons via Interception driver."""
    try:
        if not driver_ready():
            # Nothing was injected through the driver, so nothing is stuck down.
            return
        ctx = Interception()
        if not ctx.valid:
            return

        kb = keyboard_device(ctx)
        mouse = mouse_device(ctx)

        # Release all keyboard modifiers
        ctx.send(kb, KeyStroke(code=SCAN_LSHIFT, flags=KEY_UP))
        ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_UP))
        ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
        
        # Release mouse buttons
        from interception import MouseStroke
        ctx.send(mouse, MouseStroke(0, 0x002, 0, 0, 0))  # LEFT_BUTTON_UP
        ctx.send(mouse, MouseStroke(0, 0x008, 0, 0, 0))  # RIGHT_BUTTON_UP
        
        time.sleep(0.05)
        del ctx
        
        logger.info("Emergency release: all inputs released via Interception")
    except Exception as e:
        logger.error(f"Emergency release failed: {e}")


def typewrite(text: str, interval: float = 0.04, post_wait: float = 0.22, stop_event=None) -> dict:
    """
    Type text into the currently focused location using Interception kernel driver.
    
    Args:
        text: The text to type
        interval: Delay between characters in seconds (default 40ms)
        post_wait: Safety pause after typing completes (default 220ms)
    """
    try:
        if not ensure_attached():
            logger.error("Interception could not be attached for typewrite")
            return {
                "status": "error",
                "action": "typewrite",
                "message": "Kernel input unavailable - Interception could not be attached"
            }

        ctx = Interception()
        if not ctx.valid:
            logger.error("Interception driver not installed!")
            return {
                "status": "error",
                "action": "typewrite",
                "message": "Interception driver not installed"
            }

        kb = keyboard_device(ctx)

        try:
            for char in text:
                # Check stop between each character
                if stop_event and stop_event.is_set():
                    logger.info("typewrite interrupted by stop_event mid-typing")
                    break
                
                mapping = _char_to_scancode(char)
                
                if mapping is None:
                    logger.warning(f"Skipping unmappable character: '{char}'")
                    continue
                
                scancode, needs_shift, needs_ctrl, needs_alt = mapping
                
                # Per-character try/finally to prevent stuck modifiers
                try:
                    if needs_ctrl:
                        ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_DOWN))
                        time.sleep(0.005)
                    if needs_alt:
                        ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_DOWN))
                        time.sleep(0.005)
                    if needs_shift:
                        ctx.send(kb, KeyStroke(code=SCAN_LSHIFT, flags=KEY_DOWN))
                        time.sleep(0.005)
                    
                    ctx.send(kb, KeyStroke(code=scancode, flags=KEY_DOWN))
                    time.sleep(0.01)
                    ctx.send(kb, KeyStroke(code=scancode, flags=KEY_UP))
                    
                finally:
                    if needs_shift:
                        ctx.send(kb, KeyStroke(code=SCAN_LSHIFT, flags=KEY_UP))
                    if needs_alt:
                        ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
                    if needs_ctrl:
                        ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_UP))
                
                time.sleep(interval)
            
        finally:
            # Blanket safety cleanup - release all modifiers unconditionally
            ctx.send(kb, KeyStroke(code=SCAN_LSHIFT, flags=KEY_UP))
            ctx.send(kb, KeyStroke(code=SCAN_CTRL, flags=KEY_UP))
            ctx.send(kb, KeyStroke(code=SCAN_ALT, flags=KEY_UP))
            time.sleep(0.05)
            del ctx
            release_when_idle()

        time.sleep(post_wait)
        
        # If stopped mid-typing, return stopped status
        if stop_event and stop_event.is_set():
            logger.info("Canvas input (Interception): stopped by user mid-typing")
            return {
                "status": "stopped",
                "action": "typewrite",
                "message": "Stopped by user"
            }
        
        logger.info(f"Canvas input (Interception): typed '{text}' ({len(text)} chars)")
        
        return {
            "status": "success",
            "action": "typewrite",
            "text": text,
            "message": "verify yourself using visual"
        }
        
    except Exception as e:
        logger.error(f"Error in canvas input (Interception): {str(e)}")
        return {
            "status": "error",
            "action": "typewrite",
            "message": str(e)
        }
