# AutoCua/mac/controller/service.py
# macOS version — all mouse interactions via Quartz with proven approach:
#   - kCGEventSourceStatePrivate (not CombinedSessionState)
#   - CGWarpMouseCursorPosition for cursor placement
#   - CGEventSetLocation on all down/up events
# Keyboard/scroll still use pyautogui (works fine on macOS)

import logging
import time
import objc
import pyautogui
import Quartz
from Quartz import (
    CGEventCreateMouseEvent, CGEventPost, CGEventSourceCreate,
    CGEventSetIntegerValueField, CGEventSetLocation,
    CGWarpMouseCursorPosition, CGAssociateMouseAndMouseCursorPosition,
    CGPointMake,
    kCGEventMouseMoved, kCGEventLeftMouseDown, kCGEventLeftMouseUp,
    kCGEventRightMouseDown, kCGEventRightMouseUp,
    kCGEventLeftMouseDragged,
    kCGMouseButtonLeft, kCGMouseButtonRight,
    kCGMouseEventClickState,
    kCGHIDEventTap, kCGEventSourceStatePrivate,
)
from ApplicationServices import AXIsProcessTrusted, AXUIElementPerformAction, AXUIElementCopyActionNames
from Cocoa import (NSWorkspace, NSApplicationActivateIgnoringOtherApps, NSData,
                   NSPasteboard, NSPasteboardItem, NSPasteboardTypeString)
from ..tree.element import get_screen, _find_topmost_app_on_screen, find_app, ax_attr, get_frame, Rect

# pyautogui only used for keyboard + scroll (not mouse clicks)
pyautogui.MINIMUM_DURATION = 0
pyautogui.MINIMUM_SLEEP = 0
pyautogui.PAUSE = 0
pyautogui.FAILSAFE = False

logger = logging.getLogger(__name__)

# Clipboard-history apps (Maccy, Raycast, Alfred, Paste...) skip pasteboard
# entries carrying these markers (the nspasteboard.org convention), so values
# the agent pastes, passwords included, never land in the user's history.
_PASTE_MARKERS = ("org.nspasteboard.TransientType", "org.nspasteboard.ConcealedType")

# Text inputs keep the plain center: their whole box is the hit target, and
# moving the point onto one of their text lines would move the caret.
_EDITABLE_TYPES = frozenset({"TextField", "TextArea", "ComboBox"})
_GAP_MAX_DEPTH = 3       # child levels followed to reach real content
_GAP_MAX_CHILDREN = 50   # children read per level


class _ClipboardUnavailable(Exception):
    """The value could not be put on the pasteboard, so nothing was pasted."""


def _restore_pasteboard(pb, saved):
    """Best-effort: put back every item and type captured from `pb`."""
    try:
        pb.clearContents()
        items = []
        for pairs in saved:
            item = NSPasteboardItem.alloc().init()
            for kind, data in pairs:
                if data is not None:
                    item.setData_forType_(data, kind)
            items.append(item)
        if items:
            pb.writeObjects_(items)
    except Exception:
        pass


class ControllerService:
    def __init__(self, stop_event=None):
        """Initialize the Controller Service"""
        self.elements_mapping = {}
        self.application_name = ""
        self.stop_event = stop_event
        # Where this batch's pointer actions landed, read by data_extraction.
        self.clicks = []

        trusted = AXIsProcessTrusted()
        if not trusted:
            logger.warning(
                "Accessibility permission NOT granted — mouse clicks will be silently dropped by macOS. "
                "Go to System Settings > Privacy & Security > Accessibility and add this app."
            )
        else:
            logger.info("Accessibility permission OK")

    def release_all_inputs(self):
        """Emergency release all hardware inputs (keyboard + mouse) via pyautogui."""
        try:
            for key in ['shift', 'ctrl', 'alt', 'shiftleft', 'shiftright',
                        'ctrlleft', 'ctrlright', 'altleft', 'altright']:
                pyautogui.keyUp(key)
            pyautogui.mouseUp(button='left')
            pyautogui.mouseUp(button='right')
            logger.info("Emergency release: all inputs released via pyautogui")
        except Exception as e:
            logger.error(f"pyautogui emergency release failed: {e}")

    # ------------------------------------------------------------------
    # Quartz primitives — single source per interaction
    # The move and click MUST share the same event source or macOS
    # drops the click silently.
    # ------------------------------------------------------------------

    def _quartz_source(self):
        """Create a Private event source — required for clicks to register."""
        return CGEventSourceCreate(kCGEventSourceStatePrivate)

    def _force_focus_target_app(self):
        """Re-activate the topmost app on the built-in display before clicking.

        After extract_all() scans the AX tree it restores focus to the calling
        process (this agent).  macOS treats the first click on an inactive
        window as an activation click rather than an element click, so we must
        bring the target app back to front before posting Quartz events.
        Mirrors the force_focus_main() step from element/click.py.
        """
        try:
            screen = get_screen()
            top, _ = _find_topmost_app_on_screen(screen)
            target_pid = top["pid"] if top else None

            if target_pid is None:
                finder = find_app("com.apple.finder")
                if finder:
                    target_pid = finder.processIdentifier()

            if target_pid is None:
                return

            ws = NSWorkspace.sharedWorkspace()
            for app in ws.runningApplications():
                if app.processIdentifier() == target_pid:
                    app.activateWithOptions_(NSApplicationActivateIgnoringOtherApps)
                    time.sleep(0.5)
                    return
        except Exception as e:
            logger.warning(f"force_focus_target_app failed: {e}")

    def _warp_move_click(self, x, y, click_count=1):
        """
        Full click sequence with ONE event source (proven working approach):
        force-focus target app → warp cursor → move event → click down/up.
        """
        self._force_focus_target_app()

        point = CGPointMake(float(x), float(y))
        source = self._quartz_source()

        # Warp cursor physically
        CGWarpMouseCursorPosition(point)
        CGAssociateMouseAndMouseCursorPosition(True)
        time.sleep(0.5)

        # Move event (same source)
        move = CGEventCreateMouseEvent(
            source, kCGEventMouseMoved, point, kCGMouseButtonLeft
        )
        if move is None:
            logger.error("Quartz: failed to create move event")
            return
        CGEventPost(kCGHIDEventTap, move)
        time.sleep(0.3)

        # Click down/up (same source)
        for i in range(1, click_count + 1):
            down = CGEventCreateMouseEvent(
                source, kCGEventLeftMouseDown, point, kCGMouseButtonLeft
            )
            up = CGEventCreateMouseEvent(
                source, kCGEventLeftMouseUp, point, kCGMouseButtonLeft
            )
            if down is None or up is None:
                logger.error(f"Quartz: failed to create click events (pass {i})")
                return

            CGEventSetIntegerValueField(down, kCGMouseEventClickState, i)
            CGEventSetIntegerValueField(up, kCGMouseEventClickState, i)
            CGEventSetLocation(down, point)
            CGEventSetLocation(up, point)

            CGEventPost(kCGHIDEventTap, down)
            time.sleep(0.08)
            CGEventPost(kCGHIDEventTap, up)
            if i < click_count:
                time.sleep(0.06)

    def _warp_move_right_click(self, x, y):
        """
        Full right-click sequence with ONE event source.
        """
        self._force_focus_target_app()

        point = CGPointMake(float(x), float(y))
        source = self._quartz_source()

        CGWarpMouseCursorPosition(point)
        CGAssociateMouseAndMouseCursorPosition(True)
        time.sleep(0.5)

        move = CGEventCreateMouseEvent(
            source, kCGEventMouseMoved, point, kCGMouseButtonLeft
        )
        if move is None:
            logger.error("Quartz: failed to create move event")
            return
        CGEventPost(kCGHIDEventTap, move)
        time.sleep(0.3)

        down = CGEventCreateMouseEvent(
            source, kCGEventRightMouseDown, point, kCGMouseButtonRight
        )
        up = CGEventCreateMouseEvent(
            source, kCGEventRightMouseUp, point, kCGMouseButtonRight
        )
        if down is None or up is None:
            logger.error("Quartz: failed to create right-click events")
            return

        CGEventSetIntegerValueField(down, kCGMouseEventClickState, 1)
        CGEventSetIntegerValueField(up, kCGMouseEventClickState, 1)
        CGEventSetLocation(down, point)
        CGEventSetLocation(up, point)

        CGEventPost(kCGHIDEventTap, down)
        time.sleep(0.08)
        CGEventPost(kCGHIDEventTap, up)

    def _warp_cursor_only(self, x, y):
        """
        Just warp + move (for scroll, drag start). No click.
        Returns the source so drag can reuse it.
        """
        point = CGPointMake(float(x), float(y))
        source = self._quartz_source()

        CGWarpMouseCursorPosition(point)
        CGAssociateMouseAndMouseCursorPosition(True)
        time.sleep(0.5)

        move = CGEventCreateMouseEvent(
            source, kCGEventMouseMoved, point, kCGMouseButtonLeft
        )
        if move:
            CGEventPost(kCGHIDEventTap, move)
        time.sleep(0.3)

        return source

    def _quartz_mouse_down(self, source, x, y):
        """Mouse down using provided source."""
        point = CGPointMake(float(x), float(y))
        down = CGEventCreateMouseEvent(
            source, kCGEventLeftMouseDown, point, kCGMouseButtonLeft
        )
        if down is None:
            logger.error("Quartz: failed to create mouseDown event")
            return
        CGEventSetIntegerValueField(down, kCGMouseEventClickState, 1)
        CGEventSetLocation(down, point)
        CGEventPost(kCGHIDEventTap, down)

    def _quartz_mouse_up(self, source, x, y):
        """Mouse up using provided source."""
        point = CGPointMake(float(x), float(y))
        up = CGEventCreateMouseEvent(
            source, kCGEventLeftMouseUp, point, kCGMouseButtonLeft
        )
        if up is None:
            logger.error("Quartz: failed to create mouseUp event")
            return
        CGEventSetIntegerValueField(up, kCGMouseEventClickState, 1)
        CGEventSetLocation(up, point)
        CGEventPost(kCGHIDEventTap, up)

    # ------------------------------------------------------------------
    # Click coordinate calculation
    # ------------------------------------------------------------------

    def _get_click_coords_for_element(self, element_info):
        """The element's geometric center. That is the click point.

        Deliberately simple, and deliberately pure arithmetic on the rect the
        scanner already resolved - no screen sampling. The previous version
        grabbed the element's pixels and clicked the "ink centroid" instead.
        That cost a ~250ms screencapture on EVERY click and mixed coordinate
        spaces: rects are CG points, but ImageGrab returns the crop at backing
        resolution (2x on Retina), so a pixel centroid was added to a
        point-space origin and the click landed up to a full element-size
        down-and-right - outside the element, on whatever sits below it.

        The center is also what the annotated screenshot shows the model, so
        the click now lands where the model was told the element is. Same rule
        every mainstream automation stack uses.

        Partially visible elements click the center of `visible_rect`, so the
        point is on the part actually on screen rather than the middle of a
        clipped box.

        One exception: when the center is on none of the element's children,
        it is a gap between them. A Google result link is the site header row
        plus the title below it; its frame is their union, and the center sits
        right of the header, above the title - dead page space the click falls
        through (Chrome's own AX hit-test still reports the link there, so it
        cannot catch this). The click then goes to the largest child instead,
        the title, which is still inside the box the model was shown.
        """
        visibility = element_info.get('visibility', 'full')
        if visibility.startswith('partial'):
            rect = element_info.get('visible_rect') or element_info['rect']
        else:
            rect = element_info['rect']

        if element_info.get('type') in _EDITABLE_TYPES:
            return self._rect_center(rect)
        return self._point_on_content(element_info.get('ax_element'), rect)

    @staticmethod
    def _rect_center(rect):
        return (rect.left + (rect.right - rect.left) // 2,
                rect.top + (rect.bottom - rect.top) // 2)

    def _point_on_content(self, ax_el, rect, depth=0):
        """The center of `rect`, unless it falls in a gap between the children
        of `ax_el`; then the same rule applied to the largest child, clipped
        to `rect`. No children, or the center on one of them: the center."""
        cx, cy = self._rect_center(rect)
        if ax_el is None or depth >= _GAP_MAX_DEPTH:
            return cx, cy

        parts = []
        for child in list(ax_attr(ax_el, "AXChildren") or [])[:_GAP_MAX_CHILDREN]:
            f = get_frame(child)
            if not f:
                continue
            part = Rect(max(rect.left, int(f["x"])), max(rect.top, int(f["y"])),
                        min(rect.right, int(f["x"] + f["width"])),
                        min(rect.bottom, int(f["y"] + f["height"])))
            if part.right > part.left and part.bottom > part.top:
                parts.append((child, part))

        if not parts or any(p.left <= cx < p.right and p.top <= cy < p.bottom
                            for _, p in parts):
            return cx, cy

        child, part = max(parts, key=lambda cp: (cp[1].right - cp[1].left)
                                                * (cp[1].bottom - cp[1].top))
        return self._point_on_content(child, part, depth + 1)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def set_elements(self, elements_mapping, application_name=""):
        """Set the elements mapping from scanner"""
        self.elements_mapping = elements_mapping
        self.application_name = application_name
        self.clicks = []   # a new batch starts with every set_elements
        logger.info(f"Controller received {len(self.elements_mapping)} elements for '{application_name}'")

    def _note_click(self, index, x, y, via="mouse"):
        """Keep the point a pointer action used on element `index`. `via` is
        "axpress" when a partial element was pressed through AX: no mouse
        moved, and the point is where the mouse would have clicked."""
        self.clicks.append({"index": str(index), "point": [int(x), int(y)], "via": via})

    def click(self, index):
        """Click on element by index"""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "click", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {"status": "error", "action": "click", "index": index,
                        "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            if visibility.startswith('partial'):
                ax_el = element_info.get('ax_element')
                if ax_el:
                    try:
                        err, actions = AXUIElementCopyActionNames(ax_el, None)
                        if err == 0 and actions and "AXPress" in actions:
                            err = AXUIElementPerformAction(ax_el, "AXPress")
                            if err == 0:
                                # Guarded: the press already happened, so a failure
                                # here must never reach the mouse fallback below.
                                try:
                                    self._note_click(index, *self._get_click_coords_for_element(element_info),
                                                     via="axpress")
                                except Exception:
                                    pass
                                time.sleep(1.0)
                                logger.info(f"AXPress element {index} (partial)")
                                return {"status": "success", "action": "click", "index": index,
                                        "element_name": element_info.get('name', 'Unknown')}
                            else:
                                logger.warning(f"AXPress failed (err={err}) for element {index}, falling back to mouse click")
                        else:
                            logger.info(f"AXPress not available for element {index}, falling back to mouse click")
                    except Exception as e:
                        logger.warning(f"AXPress exception for element {index}: {e}, falling back to mouse click")

            click_x, click_y = self._get_click_coords_for_element(element_info)
            self._warp_move_click(click_x, click_y, click_count=1)
            self._note_click(index, click_x, click_y)
            time.sleep(1.0)

            logger.info(f"Clicked element {index} at ({click_x}, {click_y})")
            return {"status": "success", "action": "click", "index": index,
                    "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error clicking element {index}: {str(e)}")
            return {"status": "error", "action": "click", "index": index, "message": str(e)}

    def _paste_text(self, text):
        """Insert `text` verbatim at the focused caret via the clipboard (Cmd+V).

        The insertion path for `input` (every value) and for multi-line
        `typewrite` content, same as Windows. The whole string lands at once
        instead of ~40 ms per keystroke, every character arrives verbatim
        (pyautogui silently drops characters it has no key for, such as é or
        emoji), and code editors that auto-indent on Return (CodeMirror/Colab,
        Monaco) don't compound the indentation already present
        in `text`. Typing char-by-char would send each '\n' as a Return keypress,
        the editor would auto-indent the new line, and the value's own leading
        spaces would stack on top — producing a 4→8→12-space cascade. Pasting
        inserts the exact characters with no auto-indent.

        It talks to NSPasteboard directly rather than through pbcopy/pbpaste:
          * every item and type on the user's clipboard (an image, a copied
            Finder file, rich text) is saved first and put back afterwards,
            not just its plain text;
          * the value is marked transient + concealed, so clipboard-history
            apps don't keep it;
          * the text goes over as a string, so no locale can garble it and a
            value that happens to start with {\\rtf stays plain text.

        The old clipboard goes back 0.8 s after Cmd+V (longer for long text).
        An app reads the pasteboard only when it gets round to handling the
        paste, and a busy web page or Office can take longer than a fraction
        of a second, which would paste the OLD clipboard. If something else
        writes the clipboard in that window, it is left alone.

        Raises _ClipboardUnavailable, before any key is sent, if the value
        can't be put on the pasteboard; the user's clipboard is put back first.
        """
        with objc.autorelease_pool():
            pb = NSPasteboard.generalPasteboard()
            saved, cleared = None, False
            try:
                saved = [[(kind, item.dataForType_(kind)) for kind in item.types()]
                         for item in (pb.pasteboardItems() or [])]
                value = NSPasteboardItem.alloc().init()
                value.setString_forType_(text, NSPasteboardTypeString)
                for marker in _PASTE_MARKERS:
                    value.setData_forType_(NSData.data(), marker)
                pb.clearContents()
                cleared = True
                if not pb.writeObjects_([value]):
                    raise RuntimeError("the pasteboard refused the value")
                ours = pb.changeCount()
            except Exception as e:
                if cleared:                 # the user's clipboard is already gone
                    _restore_pasteboard(pb, saved)
                raise _ClipboardUnavailable(str(e)) from e

            time.sleep(0.05)
            pyautogui.hotkey('command', 'v')
            time.sleep(max(0.8, len(text) * 0.005))

            # Put the user's clipboard back, unless something else wrote to it
            # meanwhile (the user copied something during the paste).
            try:
                if pb.changeCount() == ours:
                    _restore_pasteboard(pb, saved)
            except Exception:
                pass

    def input(self, index, value):
        """Input text into element by index"""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "input", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {"status": "error", "action": "input", "index": index,
                        "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            click_x, click_y = self._get_click_coords_for_element(element_info)
            self._warp_move_click(click_x, click_y, click_count=1)
            self._note_click(index, click_x, click_y)
            time.sleep(0.1)

            # Select all and delete existing content (Cmd+A on macOS)
            pyautogui.hotkey('command', 'a')
            time.sleep(0.05)
            pyautogui.press('backspace')
            time.sleep(0.05)

            # The value is INSERTED whole through the clipboard (Cmd+V into the
            # focused element), single-line and multi-line alike, same as
            # Windows. One paste replaces ~40 ms per keystroke, every character
            # arrives verbatim, and auto-indenting editors don't compound the
            # indentation already in `value` (see _paste_text). This replaced
            # the per-app "slow typing" list: Terminal and iTerm2 both bind
            # Cmd+V. Keystrokes remain only as the fallback when the clipboard
            # itself can't be used, and only for single-line values: typed,
            # every newline would be a Return, which is what pasting avoids.
            value = "" if value is None else str(value)
            pasted = False
            if value:
                if self.stop_event and self.stop_event.is_set():
                    return {"status": "stopped", "action": "input", "message": "Stopped by user"}
                try:
                    self._paste_text(value)
                    pasted = True
                except _ClipboardUnavailable as e:
                    if '\n' in value:
                        raise
                    logger.info(f"clipboard unavailable ({e}); typing the value instead")
                    for char in value:
                        if self.stop_event and self.stop_event.is_set():
                            return {"status": "stopped", "action": "input", "message": "Stopped by user"}
                        pyautogui.write(char, interval=0.04)

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
            return {"status": "error", "action": "input", "index": index, "message": str(e)}

    def double_click(self, index):
        """Double-click on element by index"""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "double_click", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {"status": "error", "action": "double_click", "index": index,
                        "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            click_x, click_y = self._get_click_coords_for_element(element_info)
            self._warp_move_click(click_x, click_y, click_count=2)
            self._note_click(index, click_x, click_y)
            time.sleep(1.0)

            logger.info(f"Double-clicked element {index} at ({click_x}, {click_y})")
            return {"status": "success", "action": "double_click", "index": index,
                    "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error double-clicking element {index}: {str(e)}")
            return {"status": "error", "action": "double_click", "index": index, "message": str(e)}

    def triple_click(self, index):
        """Triple-click on element by index (select entire line)."""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "triple_click", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {"status": "error", "action": "triple_click", "index": index,
                        "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            click_x, click_y = self._get_click_coords_for_element(element_info)
            self._warp_move_click(click_x, click_y, click_count=3)
            self._note_click(index, click_x, click_y)
            time.sleep(1.0)

            logger.info(f"Triple-clicked element {index} at ({click_x}, {click_y})")
            return {"status": "success", "action": "triple_click", "index": index,
                    "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error triple-clicking element {index}: {str(e)}")
            return {"status": "error", "action": "triple_click", "index": index, "message": str(e)}

    def right_click(self, index):
        """Right-click on element by index"""
        try:
            index = str(index)

            if index not in self.elements_mapping:
                return {"status": "error", "action": "right_click", "index": index,
                        "message": f"Element index {index} not found"}

            element_info = self.elements_mapping[index]

            visibility = element_info.get('visibility', 'full')
            if visibility == 'hidden':
                clipped_by = element_info.get('clipped_by', 'unknown container')
                return {"status": "error", "action": "right_click", "index": index,
                        "message": f"Element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            click_x, click_y = self._get_click_coords_for_element(element_info)
            self._warp_move_right_click(click_x, click_y)
            self._note_click(index, click_x, click_y)
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

            # Warp cursor to scroll target
            self._warp_cursor_only(center_x, center_y)

            # macOS scrolls in LINE units — pyautogui posts one scroll event per unit,
            # so this is a small, gentle nudge. This is NOT the Windows "120 == one wheel
            # notch" convention: a value like 120 here becomes ~120 line events and macOS
            # momentum scrolling then flings the view straight to the end. Keep it small.
            # ~3 steps x 5 lines = a modest scroll; tune scroll_clicks to taste.
            scroll_amount = 3
            scroll_clicks = 5

            if direction.lower() == "up":
                for _ in range(scroll_amount):
                    pyautogui.scroll(scroll_clicks, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "down":
                for _ in range(scroll_amount):
                    pyautogui.scroll(-scroll_clicks, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "left":
                for _ in range(scroll_amount):
                    pyautogui.hscroll(-scroll_clicks, x=center_x, y=center_y)
                    time.sleep(0.05)
            elif direction.lower() == "right":
                for _ in range(scroll_amount):
                    pyautogui.hscroll(scroll_clicks, x=center_x, y=center_y)
                    time.sleep(0.05)
            else:
                return {"status": "error", "action": "scroll", "index": index,
                        "message": f"Invalid scroll direction: {direction}. Use 'up', 'down', 'left', or 'right'"}

            self._note_click(index, center_x, center_y)
            logger.info(f"Scrolled element {index} {direction} at position ({center_x}, {center_y})")
            time.sleep(0.5)

            return {"status": "success", "action": "scroll", "index": index,
                    "direction": direction, "element_name": element_info.get('name', 'Unknown')}

        except Exception as e:
            logger.error(f"Error scrolling element {index}: {str(e)}")
            return {"status": "error", "action": "scroll", "index": index, "message": str(e)}

    def drag(self, start_x, start_y, end_x, end_y):
        """Drag mouse from start position to end position via Quartz events."""
        try:
            # Warp to start position — returns source for reuse
            source = self._warp_cursor_only(start_x, start_y)

            # Mouse down at start (same source)
            self._quartz_mouse_down(source, start_x, start_y)
            time.sleep(0.05)

            # Drag in steps (same source)
            steps = 20
            for i in range(1, steps + 1):
                progress = i / steps
                ix = int(start_x + (end_x - start_x) * progress)
                iy = int(start_y + (end_y - start_y) * progress)
                point = CGPointMake(float(ix), float(iy))
                drag_ev = CGEventCreateMouseEvent(
                    source, kCGEventLeftMouseDragged, point, kCGMouseButtonLeft
                )
                if drag_ev:
                    CGEventSetLocation(drag_ev, point)
                    CGEventPost(kCGHIDEventTap, drag_ev)
                time.sleep(0.015)

            # Mouse up at end (same source)
            self._quartz_mouse_up(source, end_x, end_y)
            time.sleep(0.1)

            logger.info(f"Dragged from ({start_x}, {start_y}) to ({end_x}, {end_y})")
            return {"status": "success", "action": "drag",
                    "start": (start_x, start_y), "end": (end_x, end_y)}

        except Exception as e:
            logger.error(f"Error dragging: {str(e)}")
            return {"status": "error", "action": "drag", "message": str(e)}

    def drag_drop(self, from_index, to_index):
        """Drag from one element to another by index (drag and drop)."""
        try:
            from_index = str(from_index)
            to_index = str(to_index)

            if from_index not in self.elements_mapping:
                return {"status": "error", "action": "drag_drop", "from_index": from_index,
                        "message": f"Source element index {from_index} not found"}

            if to_index not in self.elements_mapping:
                return {"status": "error", "action": "drag_drop", "to_index": to_index,
                        "message": f"Target element index {to_index} not found"}

            from_info = self.elements_mapping[from_index]
            to_info = self.elements_mapping[to_index]

            for label, idx, info in [("Source", from_index, from_info), ("Target", to_index, to_info)]:
                if info.get('visibility', 'full') == 'hidden':
                    clipped_by = info.get('clipped_by', 'unknown container')
                    return {"status": "error", "action": "drag_drop", "index": idx,
                            "message": f"{label} element is hidden (clipped by '{clipped_by}'). Scroll to make it visible first."}

            from_x, from_y = self._get_click_coords_for_element(from_info)
            to_x, to_y = self._get_click_coords_for_element(to_info)

            result = self.drag(from_x, from_y, to_x, to_y)

            if result.get("status") == "success":
                self._note_click(from_index, from_x, from_y)
                self._note_click(to_index, to_x, to_y)
                logger.info(f"Drag-dropped from element {from_index} to element {to_index}")
                return {"status": "success", "action": "drag_drop",
                        "from_index": from_index, "to_index": to_index,
                        "from_element": from_info.get('name', 'Unknown'),
                        "to_element": to_info.get('name', 'Unknown')}
            return result

        except Exception as e:
            logger.error(f"Error drag-dropping from {from_index} to {to_index}: {str(e)}")
            return {"status": "error", "action": "drag_drop", "message": str(e)}

    def typewrite(self, text):
        """Type text directly into currently focused location (no element targeting)."""
        try:
            # Multi-line: paste verbatim so auto-indenting editors don't compound
            # the indentation (see _paste_text). Replaces any active selection,
            # which is exactly what the line-edit workflow expects.
            if '\n' in text:
                if self.stop_event and self.stop_event.is_set():
                    logger.info("typewrite interrupted by stop_event")
                    return {"status": "stopped", "action": "typewrite",
                            "message": "Stopped by user"}
                self._paste_text(text)
            else:
                for char in text:
                    if self.stop_event and self.stop_event.is_set():
                        logger.info("typewrite interrupted by stop_event")
                        return {"status": "stopped", "action": "typewrite",
                                "message": "Stopped by user"}
                    pyautogui.write(char, interval=0.04)
            time.sleep(0.22)

            logger.info(f"Canvas input: typed '{text}' ({len(text)} chars)")
            return {"status": "success", "action": "typewrite", "text": text,
                    "message": "verify yourself using visual"}

        except Exception as e:
            logger.error(f"typewrite failed: {e}")
            return {"status": "error", "action": "typewrite", "message": str(e)}