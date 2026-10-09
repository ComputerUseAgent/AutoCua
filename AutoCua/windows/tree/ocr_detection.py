"""
OCR Detection - Windows Native OCR Scanner
Captures full screen, runs Windows built-in OCR, returns raw word list.
Designed to run as a parallel thread alongside UIA/Win32 scans.
Filtering/merging happens in the caller (element.py).
"""

import asyncio
import math
from PIL import ImageGrab

from winrt.windows.media.ocr import OcrEngine
from winrt.windows.globalization import Language
from winrt.windows.graphics.imaging import SoftwareBitmap, BitmapPixelFormat, BitmapAlphaMode


async def _pil_to_software_bitmap(pil_image):
    """Convert PIL Image to WinRT SoftwareBitmap for OCR input"""
    if pil_image.mode != "RGBA":
        pil_image = pil_image.convert("RGBA")

    width, height = pil_image.size
    # The channel swap must happen in C: a per-pixel Python loop takes ~0.5-1s
    # per full-screen frame and holds the GIL against the UIA scan running on
    # the main thread. PIL's raw encoder packs RGBA pixels into BGRA order.
    bgra = pil_image.tobytes("raw", "BGRA")

    bitmap = SoftwareBitmap(BitmapPixelFormat.BGRA8, width, height, BitmapAlphaMode.PREMULTIPLIED)
    bitmap.copy_from_buffer(bgra)
    return bitmap


async def _run_ocr(pil_image):
    """Run Windows OCR on a PIL image, return list of line dicts"""
    engine = OcrEngine.try_create_from_user_profile_languages()
    if engine is None:
        engine = OcrEngine.try_create_from_language(Language("en-US"))
    if engine is None:
        return []

    bitmap = await _pil_to_software_bitmap(pil_image)
    result = await engine.recognize_async(bitmap)

    # Windows OCR deskews the image before reading it. When it decides the text
    # is rotated it reports text_angle (degrees, clockwise, around the image
    # centre) and every word rect lives in that deskewed image, not in the
    # screenshot - so the boxes land further off the farther they sit from the
    # centre. Busy scenes trigger it on perfectly horizontal UI text: live Forza
    # frames read 2-13 deg and put every menu box 30-90px away, not even
    # touching its text. Rotating each word's CENTRE back by the angle puts it
    # on the screenshot again (0.2-2.6px residual). Its measured width/height
    # are kept as-is: rotating the corners too inflates every box (mean IoU
    # 0.83 vs 0.91 for the centre on the same frames).
    width, height = pil_image.size
    angle = result.text_angle
    if angle:
        cos_a, sin_a = math.cos(math.radians(angle)), math.sin(math.radians(angle))
        center_x, center_y = width / 2, height / 2

    lines = []
    for line in result.lines:
        if not line.words:
            continue
        # Compute line bounding box from constituent words
        word_boxes = []
        for w in line.words:
            rect = w.bounding_rect
            left, top = rect.x, rect.y
            if angle:
                dx = rect.x + rect.width / 2 - center_x
                dy = rect.y + rect.height / 2 - center_y
                left = center_x + dx * cos_a - dy * sin_a - rect.width / 2
                top = center_y + dx * sin_a + dy * cos_a - rect.height / 2
            word_boxes.append((left, top, left + rect.width, top + rect.height))
        min_left = max(0, min(int(b[0]) for b in word_boxes))
        min_top = max(0, min(int(b[1]) for b in word_boxes))
        max_right = min(width, max(int(b[2]) for b in word_boxes))
        max_bottom = min(height, max(int(b[3]) for b in word_boxes))
        if max_right <= min_left or max_bottom <= min_top:
            continue  # rotated entirely off the screenshot
        lines.append({
            "text": line.text,
            "left": min_left,
            "top": min_top,
            "right": max_right,
            "bottom": max_bottom,
        })
    return lines


class OCRScanner:
    """
    Lightweight OCR scanner. Captures full screen, runs Windows OCR,
    stores raw line list. Thread-safe for parallel execution.
    """

    def __init__(self):
        self.lines = []

    def scan(self):
        """Capture full screen and run OCR. Stores results in self.lines."""
        try:
            screenshot = ImageGrab.grab()
        except OSError:
            self.lines = []
            return

        self.lines = asyncio.run(_run_ocr(screenshot))

    def get_lines(self):
        """Return raw line list: [{text, left, top, right, bottom}, ...]"""
        return self.lines