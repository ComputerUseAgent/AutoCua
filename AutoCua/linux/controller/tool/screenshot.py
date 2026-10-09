# AutoCua/linux/controller/tool/screenshot.py
# Linux — capture the screen through the XDG Screenshot portal (the scanner's
# own capture path, Wayland-safe), crop the element region, save to disk.

import logging
import os
from datetime import datetime
from pathlib import Path

from ...tree.element import get_screen, take_screenshot

logger = logging.getLogger(__name__)


class ScreenshotService:
    """Capture and crop element screenshots on Linux."""

    def __init__(self, controller_service, sandbox_workspace: str = None):
        self.controller_service = controller_service
        self.sandbox_workspace = sandbox_workspace

    def capture_element(self, rect, index="element") -> dict:
        """
        Capture the screen, crop to element rect, save to the sandbox
        workspace (or the Desktop).

        Args:
            rect: Element rect with .left, .top, .right, .bottom (logical px)
            index: Element index for filename

        Returns:
            dict with status and saved file path
        """
        try:
            screen = get_screen()
            img, scale = take_screenshot(screen)
            if img is None:
                return {
                    "status": "error",
                    "action": "screenshot",
                    "message": "Failed to capture the screen (portal capture denied or unavailable)"
                }

            # Rect is in logical pixels, in the same space the scanner drew its
            # boxes: offset by the screen origin (non-zero when a monitor sits
            # left of or above the primary), then scaled to captured pixels
            # (HiDPI). Same arithmetic as the scanner's _capture_and_annotate.
            ox, oy = screen["x"], screen["y"]
            crop_box = (
                int((rect.left - ox) * scale),
                int((rect.top - oy) * scale),
                int((rect.right - ox) * scale),
                int((rect.bottom - oy) * scale),
            )
            cropped = img.crop(crop_box)

            # Save to sandbox workspace (or Desktop / home as fallback)
            timestamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
            filename = f"screenshot_{index}_{timestamp}.png"
            if self.sandbox_workspace and os.path.isdir(self.sandbox_workspace):
                save_dir = Path(self.sandbox_workspace)
            elif (Path.home() / "Desktop").is_dir():
                save_dir = Path.home() / "Desktop"
            else:
                save_dir = Path.home()
            save_path = save_dir / filename

            cropped.save(str(save_path), "PNG")
            logger.info(f"Screenshot saved: {save_path}")

            return {
                "status": "success",
                "action": "screenshot",
                "message": f"Image saved at: {save_path}",
                "path": str(save_path),
            }

        except Exception as e:
            logger.error(f"Screenshot capture failed: {e}")
            return {
                "status": "error",
                "action": "screenshot",
                "message": f"Screenshot failed: {str(e)}"
            }
