import contextlib
import ctypes
import logging
import time
import warnings
warnings.filterwarnings("ignore", category=SyntaxWarning, module="pywinauto")
import win32api
import win32con
import pyautogui
from interception import Interception, MouseStroke
from PIL import ImageGrab
import numpy as np
from .tool.kernel_input import release_all_inputs as kernel_release

# Interception mouse constants for UIPI-protected windows
# Note: Interception driver uses different constants than win32api
INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN = 0x001
INTERCEPTION_MOUSE_LEFT_BUTTON_UP = 0x002
INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN = 0x004
INTERCEPTION_MOUSE_RIGHT_BUTTON_UP = 0x008
INTERCEPTION_MOUSE_MOVE_RELATIVE = 0x000
INTERCEPTION_MOUSE_MOVE_ABSOLUTE = 0x001
INTERCEPTION_MOUSE_VIRTUAL_DESKTOP = 0x002

# Apps where pyautogui is blocked (UIPI-protected) — routed to kernel typewrite
KERNEL_INPUT_APPS = ["Windows Security"]

# Configure pyautogui for instant movement
pyautogui.MINIMUM_DURATION = 0
pyautogui.MINIMUM_SLEEP = 0
pyautogui.PAUSE = 0
pyautogui.FAILSAFE = False

# Configure logger
logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Clipboard — the insertion path for every `input` value (see _paste_text)
# ---------------------------------------------------------------------------
# Driven through ctypes rather than pyperclip, which only ever reads and writes
# CF_UNICODETEXT. Putting the clipboard back after a paste therefore dropped
# everything else the user had copied — an image, a file copied in Explorer,
# rich text — and every value the agent pasted, passwords included, landed in
# clipboard history (Win+V) and in the cloud clipboard.
_user32 = ctypes.WinDLL("user32", use_last_error=True)
_kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

# restype matters on 64-bit: without it ctypes truncates handles to int
_user32.CreateWindowExW.restype = ctypes.c_void_p
_user32.CreateWindowExW.argtypes = [
    ctypes.c_uint32, ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_uint32,
    ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
    ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
_user32.DestroyWindow.argtypes = [ctypes.c_void_p]
_user32.OpenClipboard.argtypes = [ctypes.c_void_p]
_user32.EnumClipboardFormats.argtypes = [ctypes.c_uint]
_user32.EnumClipboardFormats.restype = ctypes.c_uint
_user32.GetClipboardData.argtypes = [ctypes.c_uint]
_user32.GetClipboardData.restype = ctypes.c_void_p
_user32.SetClipboardData.argtypes = [ctypes.c_uint, ctypes.c_void_p]
_user32.SetClipboardData.restype = ctypes.c_void_p
_user32.RegisterClipboardFormatW.argtypes = [ctypes.c_wchar_p]
_user32.RegisterClipboardFormatW.restype = ctypes.c_uint
_user32.GetClipboardSequenceNumber.restype = ctypes.c_uint32
_kernel32.GlobalAlloc.argtypes = [ctypes.c_uint, ctypes.c_size_t]
_kernel32.GlobalAlloc.restype = ctypes.c_void_p
_kernel32.GlobalLock.argtypes = [ctypes.c_void_p]
_kernel32.GlobalLock.restype = ctypes.c_void_p
_kernel32.GlobalUnlock.argtypes = [ctypes.c_void_p]
_kernel32.GlobalSize.argtypes = [ctypes.c_void_p]
_kernel32.GlobalSize.restype = ctypes.c_size_t
_kernel32.GlobalFree.argtypes = [ctypes.c_void_p]
_kernel32.GlobalFree.restype = ctypes.c_void_p

_GMEM_MOVEABLE = 0x0002

# Formats whose clipboard entry is a GDI or private handle rather than bytes.
# The handle dies with the EmptyClipboard below, so copying it out and setting
# it back would hand the next reader a dangling handle; they are skipped. What
# users actually copy survives anyway: an image is also published as CF_DIB (8)
# or CF_DIBV5 (17) and Windows synthesises CF_BITMAP back from it, and files
# copied in Explorer are CF_HDROP (15). Both are plain memory.
_HANDLE_FORMATS = {2, 3, 9, 14, 0x0080, 0x0082, 0x0083, 0x008E}

# Written next to the pasted value. Any data in the first keeps the whole entry
# out of clipboard history, out of the cloud clipboard and out of third-party
# managers that honour it; the two DWORDs say the same to Windows itself. What
# the agent pastes, a password included, never reaches Win+V.
_NO_HISTORY_FORMATS = ("ExcludeClipboardContentFromMonitorProcessing",
                       "CanIncludeInClipboardHistory",
                       "CanUploadToCloudClipboard")


class _ClipboardUnavailable(Exception):
    """The value could not be put on the clipboard, so nothing was pasted."""


@contextlib.contextmanager
def _own_clipboard(timeout=0.5):
    """Hold the clipboard open for the block, with a window of ours as its owner.

    OpenClipboard fails outright while another process holds it (an editor
    copying, a clipboard manager reading), so it is retried for `timeout`.
    The hidden window is not decoration: after EmptyClipboard an owner of NULL
    makes every SetClipboardData that follows fail.
    """
    hwnd = _user32.CreateWindowExW(0, "STATIC", None, 0, 0, 0, 0, 0,
                                   None, None, None, None)
    try:
        deadline = time.monotonic() + timeout
        while not _user32.OpenClipboard(hwnd):
            if time.monotonic() >= deadline:
                raise _ClipboardUnavailable("another program is holding the clipboard open")
            time.sleep(0.02)
        try:
            yield
        finally:
            _user32.CloseClipboard()
    finally:
        if hwnd:
            _user32.DestroyWindow(hwnd)


def _read_clipboard_format(fmt):
    """One format's bytes, or None when it holds no plain memory."""
    handle = _user32.GetClipboardData(fmt)
    if not handle:
        return None
    size = _kernel32.GlobalSize(handle)
    if not size:
        return None
    ptr = _kernel32.GlobalLock(handle)
    if not ptr:
        return None
    try:
        return ctypes.string_at(ptr, size)
    finally:
        _kernel32.GlobalUnlock(handle)


def _write_clipboard_format(fmt, data):
    """Put one format on the already open, already emptied clipboard."""
    handle = _kernel32.GlobalAlloc(_GMEM_MOVEABLE, len(data))
    if not handle:
        raise _ClipboardUnavailable("no memory for the clipboard")
    ptr = _kernel32.GlobalLock(handle)
    if not ptr:
        _kernel32.GlobalFree(handle)
        raise _ClipboardUnavailable("the clipboard memory could not be locked")
    ctypes.memmove(ptr, data, len(data))
    _kernel32.GlobalUnlock(handle)
    if not _user32.SetClipboardData(fmt, handle):
        _kernel32.GlobalFree(handle)      # still ours while SetClipboardData failed
        raise _ClipboardUnavailable(f"the clipboard refused format {fmt}")


def _save_clipboard():
    """Every format currently on the clipboard, as raw bytes (it must be open)."""
    saved, fmt = [], 0
    while True:
        fmt = _user32.EnumClipboardFormats(fmt)
        if not fmt:
            break
        if fmt in _HANDLE_FORMATS or 0x0200 <= fmt <= 0x03FF:
            continue
        try:
            data = _read_clipboard_format(fmt)
        except Exception:
            data = None
        if data:
            saved.append((fmt, data))
    return saved


def _restore_clipboard(saved):
    """Best-effort: put every saved format back, in the order it came off."""
    try:
        with _own_clipboard():
            _user32.EmptyClipboard()
            for fmt, data in saved or ():
                try:
                    _write_clipboard_format(fmt, data)
                except Exception:
                    pass
    except Exception:
        pass


class ControllerService:
    def __init__(self, stop_event=None):
        """Initialize the Controller Service"""
        self.elements_mapping = {}  # Will store {index: element_info}
        self.application_name = ""  # Current application name for typing mode detection
        self.stop_event = stop_event
    
    def release_all_inputs(self):
        """Emergency release all hardware inputs (keyboard + mouse) via both Interception and pyautogui."""
        kernel_release()
        try:
            for key in ['shift', 'ctrl', 'alt', 'shiftleft', 'shiftright', 'ctrlleft', 'ctrlright', 'altleft', 'altright']:
                pyautogui.keyUp(key)
            pyautogui.mouseUp(button='left')
            pyautogui.mouseUp(button='right')
            logger.info("Emergency release: all inputs released via pyautogui")
        except Exception as e:
            logger.error(f"pyautogui emergency release failed: {e}")
    
    def _move_mouse_smoothly(self, target_x, target_y):
        """Move mouse smoothly to target position"""
        current_x, current_y = win32api.GetCursorPos()
        
        # Ultra-fast animation with 10 steps
        steps = 10
        for i in range(steps + 1):
            progress = i / steps
            x = int(current_x + (target_x - current_x) * progress)
            y = int(current_y + (target_y - current_y) * progress)
            win32api.SetCursorPos((x, y))
            time.sleep(0.001)  # 1ms between steps

    def _get_click_coords_for_element(self, element_info):
        """
        Determine optimal click coordinates based on element visibility.
        Full elements: pixel centroid on full rect (avoids dead space).
        Partial elements: pixel centroid on visible_rect + adaptive safety clamp
        (keeps click away from clipping edges while guaranteeing it stays inside).
        
        Args:
            element_info: Dictionary containing element, rect, visible_rect, visibility
            
        Returns:
            tuple: (x, y) absolute screen coordinates for optimal click
        """
        visibility = element_info.get('visibility', 'full')
        
        if visibility.startswith('partial'):
            # Use visible portion for analysis
            rect = element_info.get('visible_rect') or element_info['rect']
            
            width = rect.right - rect.left
            height = rect.bottom - rect.top
            
            # Pixel centroid: finds content, avoids dead space
            centroid_x, centroid_y = self._find_click_point(rect)
            
            # Geometric center: safest point, farthest from all edges
            center_x = rect.left + width // 2
            center_y = rect.top + height // 2
            
            # Blend 50/50: content-aware but edge-safe
            click_x = int(0.5 * center_x + 0.5 * centroid_x)
            click_y = int(0.5 * center_y + 0.5 * centroid_y)
            
            # Adaptive safety margin: min(10px, 25% of dimension)
            margin_x = min(10, width // 4)
            margin_y = min(10, height // 4)
            
            # Calculate safe bounds (always inside visible_rect)
            safe_left = rect.left + margin_x
            safe_right = rect.right - 1 - margin_x
            safe_top = rect.top + margin_y
            safe_bottom = rect.bottom - 1 - margin_y
            
            # If element too small for margin, collapse to center
            if safe_left > safe_right:
                safe_left = safe_right = rect.left + width // 2
            if safe_top > safe_bottom:
                safe_top = safe_bottom = rect.top + height // 2
            
            # Clamp into safe zone
            click_x = max(safe_left, min(click_x, safe_right))
            click_y = max(safe_top, min(click_y, safe_bottom))
            
            return (click_x, click_y)
        else:
            # Full visibility - pixel centroid on full rect
            rect = element_info['rect']
            return self._find_click_point(rect)

    def _find_click_point(self, rect):
        """
        Analyze element pixels to find optimal click point.
        Uses mode (most common color) as background, finds content cluster centroid.
        Works for both normal and Interception-based clicks.
        
        Args:
            rect: Element rectangle with left, top, right, bottom
            
        Returns:
            tuple: (x, y) absolute screen coordinates for optimal click
        """
        try:
            width = rect.right - rect.left
            height = rect.bottom - rect.top
            
            # Too small - just use center
            if width < 5 or height < 5:
                return (rect.left + width // 2, rect.top + height // 2)
            
            # Capture element region from screen
            img = ImageGrab.grab(bbox=(rect.left, rect.top, rect.right, rect.bottom))
            pixels = np.array(img)
            
            # Convert to grayscale for faster processing
            if len(pixels.shape) == 3:
                gray = np.mean(pixels, axis=2).astype(np.uint8)
            else:
                gray = pixels
            
            # Find mode (most common color = background)
            flat = gray.flatten()
            counts = np.bincount(flat, minlength=256)
            background_color = np.argmax(counts)
            
            # Create mask of non-background pixels (with tolerance)
            tolerance = 15
            mask = np.abs(gray.astype(np.int16) - background_color) > tolerance
            
            # If mask is empty or nearly full, use center
            mask_ratio = np.sum(mask) / mask.size
            if mask_ratio < 0.01 or mask_ratio > 0.95:
                return (rect.left + width // 2, rect.top + height // 2)
            
            # Find coordinates of content pixels
            y_coords, x_coords = np.where(mask)
            
            if len(x_coords) == 0:
                return (rect.left + width // 2, rect.top + height // 2)
            
            # Calculate centroid of content
            centroid_x = int(np.mean(x_coords))
            centroid_y = int(np.mean(y_coords))
            
            # Convert to absolute screen coordinates
            abs_x = rect.left + centroid_x
            abs_y = rect.top + centroid_y
            
            return (abs_x, abs_y)
            
        except Exception as e:
            logger.warning(f"Smart click detection failed: {str(e)}, using center")
            # Fallback to center
            center_x = rect.left + (rect.right - rect.left) // 2
            center_y = rect.top + (rect.bottom - rect.top) // 2
            return (center_x, center_y)

    def _interception_mouse_click(self, target_x, target_y, click_type="left"):
        """
        Perform mouse click using Interception driver for UIPI-protected windows.
        
        Args:
            target_x: X coordinate to click
            target_y: Y coordinate to click
            click_type: "left", "right", or "double"
        
        Returns:
            bool: True if successful, False otherwise
        """
        try:
            if self.stop_event and self.stop_event.is_set():
                logger.info("Interception mouse click skipped — stop_event set")
                return False
            
            logger.info(f"🔧 Using Interception for {click_type} click at ({target_x}, {target_y})")

            from .tool.kernel_input import ensure_attached
            if not ensure_attached():
                logger.error("Interception could not be attached for mouse click")
                return False

            ctx = Interception()
            if not ctx.valid:
                logger.error("Interception driver not installed!")
                return False
            
            from .tool.kernel_input import mouse_device
            mouse = mouse_device(ctx)
            
            try:
                # Get virtual screen dimensions
                v_x = win32api.GetSystemMetrics(76)
                v_y = win32api.GetSystemMetrics(77)
                v_width = win32api.GetSystemMetrics(78)
                v_height = win32api.GetSystemMetrics(79)
                
                # Get primary monitor dimensions
                p_width = win32api.GetSystemMetrics(0)
                p_height = win32api.GetSystemMetrics(1)

                # Determine mapping strategy
                # If target is on primary monitor, map to primary monitor (0-65535 = Primary)
                # This fixes the issue where driver maps 0-65535 to primary but we calculated for virtual
                if 0 <= target_x < p_width and 0 <= target_y < p_height:
                    use_virtual_flag = False
                    abs_x = int((target_x / p_width) * 65535)
                    abs_y = int((target_y / p_height) * 65535)
                else:
                    # Target is on secondary monitor, must use virtual desktop mapping
                    use_virtual_flag = True
                    abs_x = int(((target_x - v_x) / v_width) * 65535)
                    abs_y = int(((target_y - v_y) / v_height) * 65535)

                # Step 1: Move mouse to position (absolute)
                # MouseStroke signature: (flags, button_flags, button_data, x, y)
                # Found via debug inspection: (flags: 'int', button_flags: 'int', button_data: 'int', x: 'int', y: 'int')
                
                flags = INTERCEPTION_MOUSE_MOVE_ABSOLUTE
                if use_virtual_flag:
                    flags |= INTERCEPTION_MOUSE_VIRTUAL_DESKTOP
                    
                # Arg 1: flags (Movement)
                # Arg 2: button_flags (Clicking)
                # Arg 3: button_data (Rolling)
                ctx.send(mouse, MouseStroke(flags, 0, 0, abs_x, abs_y))
                time.sleep(0.05)
                
                # Step 2: Click at current position (no movement, x=0, y=0)
                if click_type == "left":
                    # flags=0 (Relative/Keep), button_flags=LeftDown
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, 0, 0, 0))
                    time.sleep(0.05)
                    # flags=0, button_flags=LeftUp
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_UP, 0, 0, 0))
                    
                elif click_type == "right":
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_RIGHT_BUTTON_DOWN, 0, 0, 0))
                    time.sleep(0.05)
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_RIGHT_BUTTON_UP, 0, 0, 0))
                    
                elif click_type == "double":
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, 0, 0, 0))
                    time.sleep(0.05)
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_UP, 0, 0, 0))
                    time.sleep(0.1)
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_DOWN, 0, 0, 0))
                    time.sleep(0.05)
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_UP, 0, 0, 0))
                
                time.sleep(0.05)
                
            finally:
                # Only release buttons that were actually pressed
                if click_type == "left":
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_LEFT_BUTTON_UP, 0, 0, 0))
                elif click_type == "right":
                    ctx.send(mouse, MouseStroke(0, INTERCEPTION_MOUSE_RIGHT_BUTTON_UP, 0, 0, 0))
                # Remove the unconditional RIGHT_BUTTON_UP
                time.sleep(0.1)
                del ctx
                from .tool.kernel_input import release_when_idle
                release_when_idle()
            
            logger.info(f"✅ Interception {click_type} click successful")
            return True
            
        except Exception as e:
            logger.error(f"❌ Interception mouse click failed: {str(e)}")
            return False
        
    def _escape_for_type_keys(self, text):
        """
        Escape special characters for pywinauto's type_keys method.
        
        pywinauto interprets these characters as control sequences:
        - ( ) for grouping
        - ^ for Ctrl
        - + for Shift
        - % for Alt
        - ~ for Enter
        - { } for special keys
        
        This method wraps them in curly braces to type them literally.
        
        Args:
            text (str): The text to escape
            
        Returns:
            str: Escaped text safe for type_keys
        """
        special_chars = {
            '(': '{(}',
            ')': '{)}',
            '{': '{{}',
            '}': '{}}',
            '^': '{^}',
            '+': '{+}',
            '%': '{%}',
            '~': '{~}',
        }
        result = ''
        for char in text:
            result += special_chars.get(char, char)
        return result
    
    def set_elements(self, elements_mapping, application_name=""):
        """
        Set the elements mapping from scanner
        
        Args:
            elements_mapping (dict): Dictionary with index as key and element info as value
                                   element_info contains 'element' (pywinauto element) and 'rect' (position)
            application_name (str): Current application name for typing mode detection
        """
        self.elements_mapping = elements_mapping
        self.application_name = application_name
        logger.info(f"Controller received {len(self.elements_mapping)} elements for '{application_name}'")
        
    def click(self, index):
        """
        Click on element by index using pywinauto's native click (live coordinates)
        
        Args:
            index (str): The element index to click
            
        Returns:
            dict: Result of click operation
        """
        try:
            index = str(index)  # Ensure index is string
            
            if index not in self.elements_mapping:
                return {
                    "status": "error", 
                    "action": "click",
                    "index": index,
                    "message": f"Element index {index} not found"
                }
            
            element_info = self.elements_mapping[index]
            element = element_info['element']
            
            # OCR_TEXT: no pywinauto element, use coordinate-based click
            if element is None:
                click_x, click_y = self._find_click_point(element_info['rect'])
                self._move_mouse_smoothly(click_x, click_y)
                time.sleep(0.05)
                pyautogui.click(click_x, click_y)
                time.sleep(1.0)
                return {"status": "success", "action": "click", "index": index, "element_name": element_info.get('name', 'Unknown')}
            
            # Check if element is hidden (not clickable)
            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {
                    "status": "error",
                    "action": "click",
                    "index": index,
                    "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."
                }
            
            # For partial elements, try InvokePattern first (no mouse, no coordinates)
            # Falls back to coordinate click if element doesn't support invoke
            if visibility.startswith('partial'):
                try:
                    import comtypes.client
                    _UIA_module = comtypes.client.GetModule("UIAutomationCore.dll")
                    raw_element = element.element_info.element
                    invoke_raw = raw_element.GetCurrentPattern(10000)  # UIA_InvokePatternId
                    if invoke_raw:
                        invoke_iface = invoke_raw.QueryInterface(_UIA_module.IUIAutomationInvokePattern)
                        invoke_iface.Invoke()
                        print(f"✅ InvokePattern clicked element {index} (visibility={visibility})")
                        time.sleep(1.0)
                        return {
                            "status": "success",
                            "action": "click",
                            "index": index,
                            "element_name": element_info.get('name', 'Unknown'),
                            "method": "invoke"
                        }
                except Exception:
                    pass  # No invoke support, fall through to coordinate click
            
            # Try native pywinauto click first (uses live coordinates)
            try:
                # Get fresh element rectangle (live position)
                current_rect = element.rectangle()
                
                # Get optimal click point using pixel analysis on appropriate rect
                click_x, click_y = self._get_click_coords_for_element(element_info)
                
                # Convert absolute coords to relative coords within element
                # Use original rect for offset calculation (pixel analysis was done on it)
                orig_rect = element_info['rect']
                rel_x = click_x - orig_rect.left
                rel_y = click_y - orig_rect.top
                
                # Clamp to current element bounds
                elem_width = current_rect.right - current_rect.left
                elem_height = current_rect.bottom - current_rect.top
                rel_x = max(0, min(rel_x, elem_width - 1))
                rel_y = max(0, min(rel_y, elem_height - 1))
                    
                # Use pywinauto's native click with relative coords (fetches live position internally)
                element.click_input(coords=(rel_x, rel_y))
                
                logger.info(f"Clicked element {index} at relative coords ({rel_x}, {rel_y})")
                
            except Exception as click_error:
                error_str = str(click_error)
                # Check if this is a UIPI/privilege error
                if "SetCursorPos" in error_str or "Cannot create a file" in error_str:
                    logger.warning(f"Normal click blocked by UIPI, trying Interception fallback...")
                    
                    # For Interception, get fresh absolute coords
                    try:
                        current_rect = element.rectangle()
                        abs_x = current_rect.left + rel_x
                        abs_y = current_rect.top + rel_y
                    except:
                        # Fallback to original calculated coords
                        abs_x, abs_y = self._get_click_coords_for_element(element_info)
                    
                    if not self._interception_mouse_click(abs_x, abs_y, "left"):
                        return {
                            "status": "error",
                            "action": "click",
                            "index": index,
                            "message": "Click failed: UIPI blocked and Interception fallback failed"
                        }
                else:
                    raise click_error
            
            # Wait 1 second after click to let UI update
            time.sleep(1.0)
            
            return {
                "status": "success",
                "action": "click", 
                "index": index,
                "element_name": element_info.get('name', 'Unknown')
            }
            
        except Exception as e:
            logger.error(f"Error clicking element {index}: {str(e)}")
            return {
                "status": "error",
                "action": "click",
                "index": index,
                "message": str(e)
            }
    
    def _paste_text(self, text, element=None):
        """Insert `text` verbatim via the clipboard (Ctrl+V).

        The insertion path for `input` (every value) and for multi-line
        `typewrite` content. The whole string lands at once instead of ~50 ms
        per keystroke, every character arrives verbatim (no type_keys escaping,
        nothing dropped for a layout without the key), and code editors that
        auto-indent on Enter (CodeMirror/Colab, Monaco) don't compound the
        indentation already present in `text`. Typing char-by-char sends each
        newline as an Enter keypress and the editor stacks its own indentation
        on top of the value's leading spaces — a 4→8→12-space cascade. Pasting
        inserts the exact characters with no auto-indent.

        `element` (a pywinauto wrapper) targets the paste at a specific control;
        when None, Ctrl+V is sent to the focused location.

        The clipboard itself goes through ctypes rather than pyperclip, which
        fixes the same four things the macOS side did:
          * every format the user had copied (an image, a file copied in
            Explorer, rich text) is saved and put back, not only plain text;
          * the value is marked no-history, so Win+V, the cloud clipboard and
            clipboard managers skip it — a pasted password included;
          * the old clipboard goes back 0.8 s after Ctrl+V (longer for long
            text) instead of 0.3 s. An app reads the clipboard only when it
            gets round to handling the paste, and a busy web page or Office can
            take longer than a fraction of a second — which used to paste the
            OLD clipboard;
          * if something else wrote the clipboard while the paste was in
            flight (the user copied something), it is left alone.

        Raises _ClipboardUnavailable, before any key is sent, when the value
        cannot be put on the clipboard; the user's clipboard is put back first.
        """
        saved, emptied = None, False
        try:
            with _own_clipboard():
                saved = _save_clipboard()
                _user32.EmptyClipboard()
                emptied = True
                _write_clipboard_format(win32con.CF_UNICODETEXT,
                                        text.encode("utf-16-le") + b"\x00\x00")
                for name in _NO_HISTORY_FORMATS:
                    _write_clipboard_format(_user32.RegisterClipboardFormatW(name),
                                            b"\x00\x00\x00\x00")
            ours = _user32.GetClipboardSequenceNumber()
        except _ClipboardUnavailable:
            if emptied:
                _restore_clipboard(saved)
            raise
        except Exception as e:
            if emptied:
                _restore_clipboard(saved)
            raise _ClipboardUnavailable(str(e)) from e

        time.sleep(0.05)
        if element is not None:
            element.type_keys('^v', with_spaces=True)
        else:
            pyautogui.hotkey('ctrl', 'v')
        time.sleep(max(0.8, len(text) * 0.005))

        # Put the user's clipboard back, unless something else wrote to it
        # meanwhile (the user copied something during the paste).
        try:
            if _user32.GetClipboardSequenceNumber() == ours:
                _restore_clipboard(saved)
        except Exception:
            pass

    def input(self, index, value):
        """
        Input text into element by index: click it, clear it, then insert the
        value whole through the clipboard (keystrokes only when the clipboard
        itself is unavailable — see below).

        Args:
            index (str): The element index to input into
            value (str): The text to input

        Returns:
            dict: Result of input operation
        """
        try:
            index = str(index)  # Ensure index is string
            
            if index not in self.elements_mapping:
                return {
                    "status": "error",
                    "action": "input",
                    "index": index,
                    "message": f"Element index {index} not found"
                }
            
            element_info = self.elements_mapping[index]
            element = element_info['element']
            
            # Check if element is hidden (not interactable)
            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {
                    "status": "error",
                    "action": "input",
                    "index": index,
                    "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."
                }
            
            # Click to focus using pywinauto's native click (handles coordinate updates internally)
            element.click_input()
            time.sleep(0.1)
            
            # Clear existing content using element.type_keys (targets specific element)
            element.type_keys('^a', with_spaces=True)  # Ctrl+A to select all
            time.sleep(0.05)
            element.type_keys('{BACKSPACE}', with_spaces=True)  # Backspace to delete
            time.sleep(0.05)
            
            # The value is INSERTED whole through the clipboard (Ctrl+V into the
            # element) — single-line and multi-line alike, from the first
            # character. One paste lands in a fixed ~0.4 s where keystrokes cost
            # ~50 ms each, every character arrives verbatim (no type_keys
            # escaping, no dropped non-ASCII), and auto-indenting editors don't
            # compound the indentation already in `value` (see _paste_text).
            # This replaced the per-app "slow typing" list: Notepad, cmd and
            # PowerShell all take a Ctrl+V paste (verified on Windows 11 —
            # conhost and Windows Terminal both bind it). Keystrokes remain only
            # as the fallback when the clipboard itself can't be used (another
            # process holds it open — pyperclip gives up after its retries).
            value = "" if value is None else str(value)
            pasted = False
            if value:
                if self.stop_event and self.stop_event.is_set():
                    return {"status": "stopped", "action": "input", "message": "Stopped by user"}
                try:
                    self._paste_text(value, element=element)
                    pasted = True
                except _ClipboardUnavailable as e:
                    logger.info(f"clipboard unavailable ({e}); typing the value instead")
                    element.type_keys(self._escape_for_type_keys(value),
                                      with_spaces=True, with_newlines=True)

            logger.info(f"{'Pasted' if pasted else 'Typed'} '{value}' into element {index}")

            # A paste has already landed and settled (_paste_text waits 0.8 s
            # before giving the clipboard back); only keystrokes need the
            # per-character settle (20 ms/char, min 0.5 s).
            wait_time = 0 if pasted else max(0.5, len(value) * 0.02)
            time.sleep(wait_time)
            
            return {
                "status": "success",
                "action": "input",
                "index": index,
                "value": value,
                "element_name": element_info.get('name', 'Unknown'),
                "message": "verify yourself using Raw Vision"
            }
            
        except Exception as e:
            logger.error(f"Error inputting to element {index}: {str(e)}")
            return {
                "status": "error", 
                "action": "input",
                "index": index,
                "message": str(e)
            }

    def double_click(self, index):
        """
        Double-click on element by index using pywinauto's native double-click (live coordinates)
        
        Args:
            index (str): The element index to double-click
            
        Returns:
            dict: Result of double-click operation
        """
        try:
            index = str(index)  # Ensure index is string
            
            if index not in self.elements_mapping:
                return {
                    "status": "error", 
                    "action": "double_click",
                    "index": index,
                    "message": f"Element index {index} not found"
                }
            
            element_info = self.elements_mapping[index]
            element = element_info['element']
            
            # OCR_TEXT: no pywinauto element, use coordinate-based double click
            if element is None:
                click_x, click_y = self._find_click_point(element_info['rect'])
                self._move_mouse_smoothly(click_x, click_y)
                time.sleep(0.05)
                pyautogui.click(click_x, click_y, clicks=2, interval=0.05)
                time.sleep(1.0)
                return {"status": "success", "action": "double_click", "index": index, "element_name": element_info.get('name', 'Unknown')}
            
            # Check if element is hidden (not clickable)
            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {
                    "status": "error",
                    "action": "double_click",
                    "index": index,
                    "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."
                }
            
            # Try native pywinauto double-click first (uses live coordinates)
            try:
                # Get fresh element rectangle (live position)
                current_rect = element.rectangle()
                
                # Get optimal click point using pixel analysis on appropriate rect
                click_x, click_y = self._get_click_coords_for_element(element_info)
                
                # Convert absolute coords to relative coords within element
                orig_rect = element_info['rect']
                rel_x = click_x - orig_rect.left
                rel_y = click_y - orig_rect.top
                
                # Clamp to current element bounds
                elem_width = current_rect.right - current_rect.left
                elem_height = current_rect.bottom - current_rect.top
                rel_x = max(0, min(rel_x, elem_width - 1))
                rel_y = max(0, min(rel_y, elem_height - 1))
                
                # Use pywinauto's native double-click with relative coords (fetches live position internally)
                element.double_click_input(coords=(rel_x, rel_y))
                
                logger.info(f"Double-clicked element {index} at relative coords ({rel_x}, {rel_y})")
                
            except Exception as click_error:
                error_str = str(click_error)
                # Check if this is a UIPI/privilege error
                if "SetCursorPos" in error_str or "Cannot create a file" in error_str:
                    logger.warning(f"Normal double-click blocked by UIPI, trying Interception fallback...")
                    
                    # For Interception, get fresh absolute coords
                    try:
                        current_rect = element.rectangle()
                        abs_x = current_rect.left + rel_x
                        abs_y = current_rect.top + rel_y
                    except:
                        # Fallback to original calculated coords
                        abs_x, abs_y = self._get_click_coords_for_element(element_info)
                    
                    if not self._interception_mouse_click(abs_x, abs_y, "double"):
                        return {
                            "status": "error",
                            "action": "double_click",
                            "index": index,
                            "message": "Double-click failed: UIPI blocked and Interception fallback failed"
                        }
                else:
                    raise click_error
            
            # Wait 1 second after double-click to let UI update
            time.sleep(1.0)
            
            return {
                "status": "success",
                "action": "double_click", 
                "index": index,
                "element_name": element_info.get('name', 'Unknown')
            }
            
        except Exception as e:
            logger.error(f"Error double-clicking element {index}: {str(e)}")
            return {
                "status": "error",
                "action": "double_click",
                "index": index,
                "message": str(e)
            }
    
    def triple_click(self, index):
        """
        Triple-click on element by index (select entire line).
        Used primarily for OCR_TEXT elements to select the full line.
        
        Args:
            index (str): The element index to triple-click
            
        Returns:
            dict: Result of triple-click operation
        """
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {
                    "status": "error",
                    "action": "triple_click",
                    "index": index,
                    "message": f"Element index {index} not found"
                }

            element_info = self.elements_mapping[index]
            element = element_info['element']

            # OCR_TEXT: no pywinauto element, use coordinate-based triple click
            if element is None:
                click_x, click_y = self._find_click_point(element_info['rect'])
                self._move_mouse_smoothly(click_x, click_y)
                time.sleep(0.05)
                pyautogui.click(click_x, click_y, clicks=3, interval=0.05)
                time.sleep(1.0)
                return {"status": "success", "action": "triple_click", "index": index, "element_name": element_info.get('name', 'Unknown')}

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {
                    "status": "error",
                    "action": "triple_click",
                    "index": index,
                    "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."
                }

            try:
                current_rect = element.rectangle()
                click_x, click_y = self._get_click_coords_for_element(element_info)

                orig_rect = element_info['rect']
                rel_x = click_x - orig_rect.left
                rel_y = click_y - orig_rect.top

                elem_width = current_rect.right - current_rect.left
                elem_height = current_rect.bottom - current_rect.top
                rel_x = max(0, min(rel_x, elem_width - 1))
                rel_y = max(0, min(rel_y, elem_height - 1))

                abs_x = current_rect.left + rel_x
                abs_y = current_rect.top + rel_y

                self._move_mouse_smoothly(abs_x, abs_y)
                time.sleep(0.05)
                pyautogui.click(abs_x, abs_y, clicks=3, interval=0.05)

                logger.info(f"Triple-clicked element {index} at ({abs_x}, {abs_y})")

            except Exception as click_error:
                error_str = str(click_error)
                if "SetCursorPos" in error_str or "Cannot create a file" in error_str:
                    logger.warning(f"Normal triple-click blocked by UIPI, trying Interception fallback...")
                    try:
                        current_rect = element.rectangle()
                        abs_x = current_rect.left + rel_x
                        abs_y = current_rect.top + rel_y
                    except:
                        abs_x, abs_y = self._get_click_coords_for_element(element_info)

                    for i in range(3):
                        if not self._interception_mouse_click(abs_x, abs_y, "left"):
                            return {
                                "status": "error",
                                "action": "triple_click",
                                "index": index,
                                "message": "Triple-click failed: UIPI blocked and Interception fallback failed"
                            }
                        if i < 2:
                            time.sleep(0.05)
                else:
                    raise click_error

            time.sleep(1.0)

            return {
                "status": "success",
                "action": "triple_click",
                "index": index,
                "element_name": element_info.get('name', 'Unknown')
            }

        except Exception as e:
            logger.error(f"Error triple-clicking element {index}: {str(e)}")
            return {
                "status": "error",
                "action": "triple_click",
                "index": index,
                "message": str(e)
            }

    def right_click(self, index):
        """
        Right-click on element by index using pywinauto's native right-click (live coordinates)
        
        Args:
            index (str): The element index to right-click
            
        Returns:
            dict: Result of right-click operation
        """
        try:
            index = str(index)  # Ensure index is string
            
            if index not in self.elements_mapping:
                return {
                    "status": "error", 
                    "action": "right_click",
                    "index": index,
                    "message": f"Element index {index} not found"
                }
            
            element_info = self.elements_mapping[index]
            element = element_info['element']
            
            # OCR_TEXT: no pywinauto element, use coordinate-based right click
            if element is None:
                click_x, click_y = self._find_click_point(element_info['rect'])
                self._move_mouse_smoothly(click_x, click_y)
                time.sleep(0.05)
                pyautogui.rightClick(click_x, click_y)
                time.sleep(1.0)
                return {"status": "success", "action": "right_click", "index": index, "element_name": element_info.get('name', 'Unknown')}
            
            # Check if element is hidden (not clickable)
            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {
                    "status": "error",
                    "action": "right_click",
                    "index": index,
                    "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."
                }
            
            # Try native pywinauto right-click first (uses live coordinates)
            try:
                # Get fresh element rectangle (live position)
                current_rect = element.rectangle()
                
                # Get optimal click point using pixel analysis on appropriate rect
                click_x, click_y = self._get_click_coords_for_element(element_info)
                
                # Convert absolute coords to relative coords within element
                orig_rect = element_info['rect']
                rel_x = click_x - orig_rect.left
                rel_y = click_y - orig_rect.top
                
                # Clamp to current element bounds
                elem_width = current_rect.right - current_rect.left
                elem_height = current_rect.bottom - current_rect.top
                rel_x = max(0, min(rel_x, elem_width - 1))
                rel_y = max(0, min(rel_y, elem_height - 1))
                
                # Use pywinauto's native right-click with relative coords (fetches live position internally)
                element.right_click_input(coords=(rel_x, rel_y))
                
                logger.info(f"Right-clicked element {index} at relative coords ({rel_x}, {rel_y})")
                
            except Exception as click_error:
                error_str = str(click_error)
                # Check if this is a UIPI/privilege error
                if "SetCursorPos" in error_str or "Cannot create a file" in error_str:
                    logger.warning(f"Normal right-click blocked by UIPI, trying Interception fallback...")
                    
                    # For Interception, get fresh absolute coords
                    try:
                        current_rect = element.rectangle()
                        abs_x = current_rect.left + rel_x
                        abs_y = current_rect.top + rel_y
                    except:
                        # Fallback to original calculated coords
                        abs_x, abs_y = self._get_click_coords_for_element(element_info)
                    
                    if not self._interception_mouse_click(abs_x, abs_y, "right"):
                        return {
                            "status": "error",
                            "action": "right_click",
                            "index": index,
                            "message": "Right-click failed: UIPI blocked and Interception fallback failed"
                        }
                else:
                    raise click_error
            
            # Wait 1 second after right-click to let context menu appear
            time.sleep(1.0)
            
            return {
                "status": "success",
                "action": "right_click", 
                "index": index,
                "element_name": element_info.get('name', 'Unknown')
            }
            
        except Exception as e:
            logger.error(f"Error right-clicking element {index}: {str(e)}")
            return {
                "status": "error",
                "action": "right_click",
                "index": index,
                "message": str(e)
            }
            
    def scroll(self, index, direction):
        """
        Scroll an element in a specified direction
        
        Args:
            index (str): The element index to scroll
            direction (str): Direction to scroll ('up', 'down', 'left', 'right')
            
        Returns:
            dict: Result of scroll operation
        """
        try:
            index = str(index)  # Ensure index is string
            
            if index not in self.elements_mapping:
                return {
                    "status": "error",
                    "action": "scroll",
                    "index": index,
                    "message": f"Element index {index} not found"
                }
            
            element_info = self.elements_mapping[index]
            element = element_info['element']
            
            # For scroll, we allow scrolling even on hidden elements (to reveal them)
            # But we still use visible_rect if available for better positioning
            # Use visible_rect for partial elements, fallback to full rect if None
            rect = element_info.get('visible_rect') or element_info['rect']
            
            # Calculate center position of the element
            center_x = rect.left + (rect.right - rect.left) // 2
            center_y = rect.top + (rect.bottom - rect.top) // 2
            
            # Move mouse to the element
            self._move_mouse_smoothly(center_x, center_y)
            time.sleep(0.1)
            
            # Perform scroll based on direction
            scroll_amount = 3  # Number of scroll clicks
            
            if direction.lower() == "up":
                # Scroll up (positive scroll)
                for _ in range(scroll_amount):
                    pyautogui.scroll(120, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "down":
                # Scroll down (negative scroll)
                for _ in range(scroll_amount):
                    pyautogui.scroll(-120, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "left":
                # Scroll left (using horizontal scroll if supported)
                for _ in range(scroll_amount):
                    pyautogui.hscroll(-120, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "right":
                # Scroll right (using horizontal scroll if supported)
                for _ in range(scroll_amount):
                    pyautogui.hscroll(120, x=center_x, y=center_y)
                    time.sleep(0.05)
            else:
                return {
                    "status": "error",
                    "action": "scroll",
                    "index": index,
                    "message": f"Invalid scroll direction: {direction}. Use 'up', 'down', 'left', or 'right'"
                }
            
            logger.info(f"Scrolled element {index} {direction} at position ({center_x}, {center_y})")
            
            # Wait briefly for UI to update
            time.sleep(0.5)
            
            return {
                "status": "success",
                "action": "scroll",
                "index": index,
                "direction": direction,
                "element_name": element_info.get('name', 'Unknown')
            }
            
        except Exception as e:
            logger.error(f"Error scrolling element {index}: {str(e)}")
            return {
                "status": "error",
                "action": "scroll",
                "index": index,
                "message": str(e)
            }
    
    def drag(self, start_x, start_y, end_x, end_y):
        """
        Drag mouse from start position to end position.
        Used for screenshot region selection and similar drag operations.
        
        Args:
            start_x, start_y: Starting coordinates
            end_x, end_y: Ending coordinates
            
        Returns:
            dict: Result of drag operation
        """
        try:
            # Move to start position
            self._move_mouse_smoothly(start_x, start_y)
            time.sleep(0.1)
            
            # Mouse down at start
            pyautogui.mouseDown(x=start_x, y=start_y)
            time.sleep(0.05)
            
            # Drag to end position (smooth movement)
            pyautogui.moveTo(end_x, end_y, duration=0.3)
            time.sleep(0.05)
            
            # Mouse up at end
            pyautogui.mouseUp(x=end_x, y=end_y)
            time.sleep(0.1)
            
            logger.info(f"Dragged from ({start_x}, {start_y}) to ({end_x}, {end_y})")
            
            return {
                "status": "success",
                "action": "drag",
                "start": (start_x, start_y),
                "end": (end_x, end_y)
            }
            
        except Exception as e:
            logger.error(f"Error dragging: {str(e)}")
            return {
                "status": "error",
                "action": "drag",
                "message": str(e)
            }

    def typewrite(self, text):
        """
        Type text directly into currently focused location (no element targeting).
        UIPI-protected apps (e.g. Windows Security) route to kernel_input.py (kernel driver).
        Normal apps use pyautogui; falls back to kernel driver on failure.
        
        Args:
            text (str): The text to type
            
        Returns:
            dict: Result of canvas input operation
        """
        from .tool.kernel_input import typewrite as kernel_typewrite

        if any(app.lower() in self.application_name.lower() for app in KERNEL_INPUT_APPS):
            logger.info(f"App '{self.application_name}' is UIPI-protected, routing to kernel typewrite")
            return kernel_typewrite(text, stop_event=self.stop_event)

        try:
            # Multi-line: paste verbatim so auto-indenting editors don't compound
            # the indentation (see _paste_text). Replaces any active selection,
            # which is exactly what the line-edit workflow expects.
            if '\n' in text:
                if self.stop_event and self.stop_event.is_set():
                    logger.info("typewrite (paste) interrupted by stop_event")
                    return {"status": "stopped", "action": "typewrite", "message": "Stopped by user"}
                self._paste_text(text)
            else:
                # Type character by character so we can check stop_event
                for char in text:
                    if self.stop_event and self.stop_event.is_set():
                        logger.info("typewrite (pyautogui) interrupted by stop_event")
                        return {"status": "stopped", "action": "typewrite", "message": "Stopped by user"}
                    pyautogui.write(char, interval=0.04)
            time.sleep(0.22)
            
            logger.info(f"Canvas input: typed '{text}' ({len(text)} chars)")
            
            return {
                "status": "success",
                "action": "typewrite",
                "text": text,
                "message": "verify yourself using visual"
            }
            
        except Exception as e:
            logger.warning(f"pyautogui typewrite failed: {e}, falling back to kernel driver")
            return kernel_typewrite(text)
