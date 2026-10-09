"""
OCR Detection — macOS Apple Vision OCR scanner.

Mirrors the Windows OCRScanner (windows/tree/ocr_detection.py): captures
nothing itself — it OCRs a PRE-CAPTURED screen image so it shares the exact
frame the AX scan + annotation use — and returns a raw line list. Filtering /
merging against the AX element tree happens in the caller (element.py).

Talks to Vision directly rather than through ocrmac, because ocrmac's entry
point PNG-encodes the image (pil2buf -> Image.save(format="PNG")) just for
Vision to decode it again. This scanner takes the CGImage the screenshot
already produced and hands it straight to VNImageRequestHandler. That matters
for more than the encode time: this runs on a worker thread beside the AX
walk, and every byte of Python/PIL work it does holds the GIL against that
walk. Measured on a full-screen capture, OCR cost inside a live scan dropped
from 0.71s to well under half that.

Designed to run as a parallel thread alongside the AX walk. The returned line
boxes are already converted to CG/AX POINTS (top-left origin) so they live in
the same coordinate space as AX element rects.
"""

import Vision
from Quartz import (
    CGImageGetWidth, CGImageGetHeight, CGColorSpaceCreateDeviceRGB,
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGContextDrawImage,
    CGContextSetInterpolationQuality, CGRectMake,
    kCGImageAlphaPremultipliedFirst, kCGBitmapByteOrder32Little,
    kCGInterpolationHigh,
)

# Drop low-confidence detections and stray single-character / blank lines that
# add noise without being actionable.
MIN_CONFIDENCE = 0.4
MIN_TEXT_LEN = 2

# VNRequestTextRecognitionLevel
_LEVEL_ACCURATE = 0
_LEVEL_FAST = 1


def _downscale_cg_image(cg_image, width, height):
    """Resample a CGImage through a bitmap context (Core Graphics, GIL-free)."""
    ctx = CGBitmapContextCreate(
        None, width, height, 8, 0, CGColorSpaceCreateDeviceRGB(),
        kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little)
    if ctx is None:
        return None
    CGContextSetInterpolationQuality(ctx, kCGInterpolationHigh)
    CGContextDrawImage(ctx, CGRectMake(0, 0, width, height), cg_image)
    return CGBitmapContextCreateImage(ctx)


class OCRScanner:
    """Run Apple Vision OCR over a pre-captured screen CGImage.

    Stores a raw line list in CG/AX POINTS:
        [{text, left, top, right, bottom, confidence}, ...]
    Thread-safe for parallel execution — each run creates its own Vision
    request and handler, with no shared global state.
    """

    def __init__(self, cg_image, scale, screen_origin, recognition_level="accurate",
                 downscale_to_points=False):
        self._cg_image = cg_image               # full-screen CGImage (PIXELS)
        self._scale = scale                     # backing scale factor (retina)
        self._ox, self._oy = screen_origin      # screen["x"], screen["y"] in CG points
        self._level = recognition_level
        self._downscale = downscale_to_points   # recognise at 1x logical resolution
        self.lines = []

    def scan(self):
        """OCR the image and store results in self.lines. Zero-arg so it can be
        used directly as a threading.Thread target."""
        if self._cg_image is None:
            self.lines = []
            return
        try:
            self.lines = self._recognize()
        except Exception as e:
            print(f"OCR error: {e}")
            self.lines = []

    def _recognize(self):
        image = self._cg_image
        px_w = CGImageGetWidth(image)
        px_h = CGImageGetHeight(image)
        if not px_w or not px_h:
            return []

        # Vision's cost scales with pixel count, and macOS UI text is designed
        # to be legible at 1x, so recognising the logical-resolution image is
        # markedly faster for the same words. Resampling happens in Core
        # Graphics, which does not hold the GIL.
        scale = self._scale or 1.0
        if self._downscale and scale > 1.0:
            small = _downscale_cg_image(image, max(1, int(round(px_w / scale))),
                                        max(1, int(round(px_h / scale))))
            if small is not None:
                image = small

        request = Vision.VNRecognizeTextRequest.alloc().init()
        request.setRecognitionLevel_(
            _LEVEL_FAST if self._level == "fast" else _LEVEL_ACCURATE)
        request.setUsesLanguageCorrection_(True)

        handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(
            image, None)
        ok, err = handler.performRequests_error_([request], None)
        if not ok:
            print(f"OCR error: {err}")
            return []

        # Vision reports NORMALISED rects (0..1) with a BOTTOM-LEFT origin.
        # Normalised means resolution-independent, so they map onto the screen's
        # point geometry directly and the downscale above needs no correction.
        pts_w = px_w / scale
        pts_h = px_h / scale
        ox, oy = self._ox, self._oy

        out = []
        for obs in (request.results() or []):
            candidates = obs.topCandidates_(1)
            if not candidates:
                continue
            best = candidates[0]
            conf = float(best.confidence())
            if conf < MIN_CONFIDENCE:
                continue
            text = (best.string() or "").strip()
            if len(text) < MIN_TEXT_LEN:
                continue

            box = obs.boundingBox()
            bx, by = box.origin.x, box.origin.y
            bw, bh = box.size.width, box.size.height

            left = int(round(ox + bx * pts_w))
            right = int(round(ox + (bx + bw) * pts_w))
            # Flip the vertical axis: Vision measures up from the bottom.
            top = int(round(oy + (1.0 - (by + bh)) * pts_h))
            bottom = int(round(oy + (1.0 - by) * pts_h))
            if right <= left or bottom <= top:
                continue

            out.append({
                "text": text,
                "left": left,
                "top": top,
                "right": right,
                "bottom": bottom,
                "confidence": conf,
            })
        return out

    def get_lines(self):
        """Return raw line list: [{text, left, top, right, bottom, confidence}, ...]
        in CG/AX points."""
        return self.lines
