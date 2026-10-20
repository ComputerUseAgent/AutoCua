#!/usr/bin/env python3

"""
macOS UI Element Scanner — drop-in replacement for Windows element.py
Uses macOS Accessibility API via PyObjC.
Requires: System Settings > Privacy & Security > Accessibility

Exposes the same UIElementScanner class interface that AutoCua/mac/agent/service.py
and AutoCua/mac/controller/ expect:
    - UIElementScanner(config, frontend_callback=None)
    - scanner.scan_elements()
    - scanner.get_scan_data()      → (element_tree_text, annotated_image_base64, uac_detected)
    - scanner.get_elements_mapping() → dict
    - scanner.application_name     → str
    - scanner.print_summary()
    - scanner.save_to_file()
"""

import sys
import os
import re
import io
import time
import base64
import threading
from collections import namedtuple
from PIL import Image, ImageDraw, ImageFont
from Quartz import (
    CGWindowListCreateImage, CGRectMake,
    kCGWindowListOptionOnScreenOnly, kCGNullWindowID,
    CGImageGetWidth, CGImageGetHeight,
    CGImageGetBytesPerRow, CGImageGetDataProvider, CGDataProviderCopyData,
    CGImageGetAlphaInfo, CGImageGetBitmapInfo,
    CGDisplayIsBuiltin, CGGetActiveDisplayList, CGDisplayBounds,
    CGWindowListCopyWindowInfo, kCGWindowListOptionOnScreenOnly,
    kCGWindowListExcludeDesktopElements,
)
from Cocoa import (
    NSWorkspace, NSScreen, NSBitmapImageRep, NSPNGFileType,
    NSApplicationActivateIgnoringOtherApps, NSNull,
)
from ApplicationServices import (
    AXUIElementCreateSystemWide, AXUIElementCreateApplication,
    AXUIElementCopyAttributeValue, AXUIElementSetAttributeValue,
    AXUIElementCopyMultipleAttributeValues, AXUIElementSetMessagingTimeout,
    AXValueGetType, kAXValueAXErrorType,
    AXIsProcessTrusted, kAXErrorSuccess,
)

try:
    from .ocr import OCRScanner
except ImportError:  # allow `python element.py` standalone
    from ocr import OCRScanner


# ========== CONFIGURATION ==========
# Toggle switches — same semantics as Windows element.py
SCREENSHOT = True    # Set to False to only generate element tree without screenshot
DEBUG = False        # Set to True to save files to debug folders, False for direct LLM only
FRONTEND = True      # Set to True when running from app.py to send images to frontend
OCR = True           # Set to False to disable Apple Vision OCR merge (canvas/label-less text)
OCR_RECOGNITION_LEVEL = "accurate"  # "accurate" (default) or "fast" (lower latency)
# OCR the screenshot at 1x logical resolution instead of full Retina pixels.
# Measured: Vision "accurate" 1.17s -> 0.78s on a 2940px capture. Costs a
# little character accuracy on very small text; set False for max fidelity.
OCR_LOGICAL_RESOLUTION = True

# Define Rect namedtuple matching Windows format (left, top, right, bottom)
Rect = namedtuple('Rect', ['left', 'top', 'right', 'bottom'])

# Single magenta color for all elements in screenshot (matches Windows)
BOX_COLOR = (255, 0, 255)   # Bright magenta for all boxes
NUMBER_COLOR = (255, 0, 255) # Same magenta for numbers

# Final geometry/encoding of the annotated screenshot. The image is encoded
# ONCE, here; those exact bytes are the LLM payload and what DEBUG writes to
# disk. Callers must NOT re-encode.
#
# One cap: the delivered image fits inside full HD (1080p). Orientation-
# agnostic - long side <= 1920, short side <= 1080 - aspect preserved, never
# upscaled: a capture already inside the box is sent as is.
#
# Why 1080p (measured on a 13" Air, whose Retina capture is 2940x1912; the
# previous caps delivered 2252x1464):
#   - Tokens: Claude 4.7+ (Opus/Sonnet 5.x) and GPT-6 read an image this size
#     natively and bill per patch, so the old payload cost ~4300 Claude tokens
#     (~3900 GPT-6) per step; 1660x1080 costs ~2340 (~2100) - about 45% less.
#     Gemini 3 bills a flat 1066 whatever the size; older Claude models
#     downsize to <=1568 either way. Nothing gets cheaper above 1080p.
#   - Bytes: the payload rides the never-cached live message, so it is
#     uploaded in full on EVERY step; 1080p cuts an annotated step from
#     ~1.0 MB to ~0.7 MB of JPEG (~1.4 M -> ~0.95 M base64 chars).
#   - Legibility: 1080p on this display is ~1.1x logical points, so UI text
#     stays larger than life size, and the labels are drawn AFTER the resize
#     at a fixed pixel size, so they are untouched by it.
LLM_IMAGE_LONG_EDGE = 1920
LLM_IMAGE_SHORT_EDGE = 1080


def llm_image_shrink(width, height):
    """Scale factor that fits a capture inside LLM_IMAGE_LONG_EDGE x
    LLM_IMAGE_SHORT_EDGE whichever way it is oriented; 1.0 when it already
    fits (never upscale)."""
    return min(1.0,
               LLM_IMAGE_LONG_EDGE / max(width, height),
               LLM_IMAGE_SHORT_EDGE / min(width, height))

# JPEG, not PNG. The lossless PNG payload ran multiple MB of base64 per step,
# and since the screenshot rides the live (never-cached) message, that upload
# was paid in full on EVERY step — it dominated LLM latency. Image tokens bill
# by DIMENSIONS not bytes, so the model reads the same tokens either way; the
# bytes only cost upload time. The reason PNG was chosen — chroma subsampling
# storing colour at half resolution and smearing the thin magenta labels — is
# answered by SUBSAMPLING = 0 (4:4:4) below: full-resolution chroma keeps the
# annotation digits crisp at roughly a tenth of the bytes.
LLM_IMAGE_FORMAT = "JPEG"
LLM_IMAGE_QUALITY = 85
# 0 = 4:4:4 (no chroma subsampling). This is the annotation-preserving half of
# the JPEG switch; PIL's default (4:2:0) is exactly what smears magenta digits.
LLM_IMAGE_SUBSAMPLING = 0
# Providers hardcode this in their request builders (grep: image/jpeg).
LLM_IMAGE_MEDIA_TYPE = "image/jpeg"

# Index-label styling, in delivered pixels (labels are drawn AFTER the downscale).
# The stroke is a rim hugging the glyphs, NOT a filled chip — the UI underneath
# stays visible, and it supplies the luma contrast magenta lacks on its own.
LABEL_FONT_SIZE = 13
LABEL_STROKE = 2
LABEL_STROKE_COLOR = (0, 0, 0)

# The plain screenshot mirrored to the frontend is a human-facing preview, not
# an LLM payload, so it keeps its original higher-fidelity settings.
FRONTEND_IMAGE_MAX_DIMENSION = 1920
FRONTEND_IMAGE_QUALITY = 100

MAX_DEPTH = 30

BROWSER_BUNDLES = {
    "com.apple.Safari",
    "com.google.Chrome",
    "com.microsoft.edgemac",
    "company.thebrowser.Browser",   # Arc
    "com.brave.Browser",
    "com.operasoftware.Opera",
    "org.mozilla.firefox",
}

BROWSER_LOAD_TIMEOUT = 15
AX_TREE_READY_TIMEOUT = 5
AX_TREE_READY_INTERVAL = 0.25
# Grace window for an AXWebArea to appear at all. Windows that never expose
# one (Safari's new tab / Start Page, settings windows) are not web content,
# so waiting the full load budget on them is pure stall.
BROWSER_WEB_AREA_GRACE = 1.5


# ========== ELEMENT CONFIG (macOS AX roles) ==========
ELEMENT_CONFIG = {
    "AXGroup": {
        "track": True,
        "is_enabled_flag": False,
        "fallback": ["AXTitle", "AXDescription", "_title_ui_element", "AXRoleDescription"],
    },
    "AXRadioButton": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXButton": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXCheckBox": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXCell": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXValue", "AXIdentifier", "AXHelp", "_children_text"],
    },
    "AXMenuBarItem": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXLink": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXPopUpButton": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXTextField": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXDescription", "AXTitle", "AXValue", "AXRoleDescription"],
    },
    "AXTextArea": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXDescription", "AXTitle", "AXValue", "AXRoleDescription"],
    },
    "AXComboBox": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXImage": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXDescription", "AXTitle", "AXFilename", "AXRoleDescription"],
    },
    "AXIcon": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXMenuItem": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXValue", "AXRoleDescription", "_children_text"],
    },
    "AXStaticText": {
        "track": True,
        "is_enabled_flag": False,
        "fallback": ["AXValue", "AXTitle", "AXDescription", "AXRoleDescription"],
    },
    "AXMenuButton": {
        "track": True,
        "is_enabled_flag": True,
        "fallback": ["AXTitle", "AXDescription", "AXRoleDescription"],
    },
}

# Per-role attribute list for the walk's second batched read (tracked roles
# only): the state flag, the label fallbacks, and AXValue where the walk
# reads it. The `_`-prefixed pseudo-fallbacks resolve through OTHER elements,
# so they can't be batched here. AXValue is deliberately left out of
# AXTextArea's batch — its value can be an entire document buffer, so
# build_label fetches it lazily only when every other fallback fails.
#
# Two variants because only ONE state flag is ever consulted: browsers read
# AXFocused for the roles that don't carry a meaningful enabled flag, every
# other case reads AXEnabled. Fetching both cost an extra attribute on every
# tracked node, and AX time scales with attributes fetched.
_PHASE2_ATTRS = {}          # native apps (and browser roles with an enabled flag)
_PHASE2_ATTRS_FOCUS = {}    # browser + is_enabled_flag False -> AXFocused
for _role, _cfg in ELEMENT_CONFIG.items():
    _labels = []
    for _a in _cfg.get("fallback", []):
        if _a == "_title_ui_element":
            # The linked element REF is batchable even though the title text
            # on it is not. Electron UIs are mostly AXGroups whose title and
            # description are empty, so this fallback fired on thousands of
            # nodes per scan, each costing its own round-trip.
            _a = "AXTitleUIElement"
        elif _a.startswith("_"):
            continue
        if _role == "AXTextArea" and _a == "AXValue":
            continue
        if _a not in _labels:
            _labels.append(_a)
    if _role in ("AXTextField", "AXComboBox") and "AXValue" not in _labels:
        _labels.append("AXValue")
    _PHASE2_ATTRS[_role] = tuple(["AXEnabled"] + _labels)
    _PHASE2_ATTRS_FOCUS[_role] = tuple(["AXFocused"] + _labels)
del _role, _cfg, _labels, _a

# Roles whose children can hide behind AXContents instead of AXChildren
# (NSBrowser column views in the Open/Save dialog). Probing every childless
# node for AXContents cost thousands of extra round-trips per scan for a
# fallback only these containers ever need.
_CONTENTS_ROLES = frozenset({
    "AXScrollArea", "AXList", "AXOutline", "AXTable", "AXBrowser", "AXSplitGroup",
})


# ========== AX HELPERS ==========

def ax_attr(element, attr):
    """Safely read a single AX attribute."""
    try:
        err, val = AXUIElementCopyAttributeValue(element, attr, None)
        if err == kAXErrorSuccess and val is not None:
            return val
    except Exception:
        pass
    return None


_NSNULL = NSNull.null()


def _clean_batch_slot(val):
    """Normalize one AXUIElementCopyMultipleAttributeValues slot to
    value-or-None. Failed slots come back as TRUTHY sentinels — NSNull, or an
    AXValue of kAXValueAXErrorType — which would otherwise str() into labels
    as garbage, so they must be filtered explicitly."""
    if val is None or val is _NSNULL:
        return None
    try:
        if AXValueGetType(val) == kAXValueAXErrorType:
            return None
    except Exception:
        pass
    return val


def ax_attrs(element, attrs):
    """Read several AX attributes in ONE IPC round-trip.

    Returns {attr: value-or-None}. Falls back to per-attribute ax_attr reads
    when the batch call fails outright (poorly-behaved AX servers), so the
    result is behavior-identical to N single reads, just cheaper."""
    try:
        err, values = AXUIElementCopyMultipleAttributeValues(
            element, list(attrs), 0, None)
    except Exception:
        err, values = -1, None
    if err != kAXErrorSuccess or values is None or len(values) != len(attrs):
        return {a: ax_attr(element, a) for a in attrs}
    return {a: _clean_batch_slot(v) for a, v in zip(attrs, values)}


# ========== GEOMETRY ==========

def _extract_two_floats(val):
    """Pull two floats from any AXValue format."""
    try:
        return (val.pointValue().x, val.pointValue().y)
    except Exception:
        pass
    try:
        return (val.sizeValue().width, val.sizeValue().height)
    except Exception:
        pass
    s = str(val)
    m = re.search(r'x:([-\d.]+)\s+y:([-\d.]+)', s)
    if m:
        return (float(m[1]), float(m[2]))
    m = re.search(r'w:([-\d.]+)\s+h:([-\d.]+)', s)
    if m:
        return (float(m[1]), float(m[2]))
    m = re.search(r'\{([-\d.]+),\s*([-\d.]+)\}', s)
    if m:
        return (float(m[1]), float(m[2]))
    return None


def _extract_four_floats(val):
    """Pull x, y, w, h from an AXFrame value."""
    s = str(val)
    m = re.search(r'x:([-\d.]+)\s+y:([-\d.]+)\s+w:([-\d.]+)\s+h:([-\d.]+)', s)
    if m:
        return (float(m[1]), float(m[2]), float(m[3]), float(m[4]))
    m = re.search(r'\{([-\d.]+),\s*([-\d.]+)\}.*?\{([-\d.]+),\s*([-\d.]+)\}', s)
    if m:
        return (float(m[1]), float(m[2]), float(m[3]), float(m[4]))
    return None


def get_frame(element):
    """Return {x, y, width, height} or None. Works for native + Electron."""
    return _frame_from_value(element, ax_attr(element, "AXFrame"))


def _frame_from_value(element, frame_val):
    """get_frame(), but reusing an already-fetched AXFrame value. Falls back
    to a batched AXPosition+AXSize read (Electron apps lack AXFrame)."""
    if frame_val is not None:
        r = _extract_four_floats(frame_val)
        if r:
            return {"x": r[0], "y": r[1], "width": r[2], "height": r[3]}

    ps = ax_attrs(element, ("AXPosition", "AXSize"))
    pos, size = ps["AXPosition"], ps["AXSize"]
    if pos is None or size is None:
        return None
    pt = _extract_two_floats(pos)
    sz = _extract_two_floats(size)
    if pt and sz:
        return {"x": pt[0], "y": pt[1], "width": sz[0], "height": sz[1]}
    return None


def _display_for_point(x, y):
    """Return CG bounds dict of the display that contains point (x, y), or None."""
    err, display_ids, count = CGGetActiveDisplayList(10, None, None)
    if err == 0:
        for did in display_ids[:count]:
            b = CGDisplayBounds(did)
            if (b.origin.x <= x < b.origin.x + b.size.width and
                    b.origin.y <= y < b.origin.y + b.size.height):
                return {"x": b.origin.x, "y": b.origin.y,
                        "width": b.size.width, "height": b.size.height}
    return None


def get_screen():
    """Return built-in display bounds in CG coordinates (same as AX coords)."""
    err, display_ids, count = CGGetActiveDisplayList(10, None, None)
    if err == 0:
        for did in display_ids[:count]:
            if CGDisplayIsBuiltin(did):
                b = CGDisplayBounds(did)
                scale = 2.0
                for s in NSScreen.screens():
                    sid = s.deviceDescription().get("NSScreenNumber", 0)
                    if sid == did:
                        scale = s.backingScaleFactor()
                        break
                return {
                    "x": b.origin.x, "y": b.origin.y,
                    "width": b.size.width, "height": b.size.height,
                    "scale": scale,
                }
    main = NSScreen.mainScreen()
    if main:
        f = main.frame()
        return {
            "x": 0, "y": 0,
            "width": f.size.width, "height": f.size.height,
            "scale": main.backingScaleFactor(),
        }
    return {"x": 0, "y": 0, "width": 1920, "height": 1080, "scale": 2.0}


# ========== BROWSER LOAD DETECTION ==========

def _pid_to_bundle(pid):
    """Return bundle ID for a given PID, or None."""
    for app in NSWorkspace.sharedWorkspace().runningApplications():
        if app.processIdentifier() == pid:
            bid = app.bundleIdentifier()
            return str(bid) if bid else None
    return None


def _is_browser(pid):
    """Check if PID belongs to a recognized browser."""
    bid = _pid_to_bundle(pid)
    return bid in BROWSER_BUNDLES if bid else False


def _find_ax_web_area(element, depth=0, max_depth=8):
    """Recursively search for an AXWebArea element in the AX subtree."""
    if depth > max_depth:
        return None
    role = ax_attr(element, "AXRole")
    if role and str(role) == "AXWebArea":
        return element
    children = ax_attr(element, "AXChildren")
    if children:
        try:
            for child in children:
                found = _find_ax_web_area(child, depth + 1, max_depth)
                if found:
                    return found
        except Exception:
            pass
    return None


def _wait_for_ax_web_content(app_ax, window, timeout=AX_TREE_READY_TIMEOUT):
    """Wait until the browser window's web content is loaded and walkable.

    Load state comes from the AX tree itself — no screenshots or template
    matching. Two signals, in order:
      1. Chromium removes the AXWebArea from the window entirely while a
         navigation is still waiting on the network, so "no web area yet"
         can be a loading signal — but only briefly. Windows with no web
         content at all (Safari's new tab / Start Page) never grow one, so
         the probe is bounded by a short grace window instead of the full
         browser-load budget: no web area after the grace means this isn't
         a web window, and we proceed rather than stall.
      2. Once present, WebKit/Chromium expose AXLoaded (bool) and
         AXLoadingProgress (0.0-1.0) on the web area for the render phase.
         Browsers that don't publish AXLoaded (e.g. Firefox) skip straight
         to the children-count readiness check.
    """
    print("  Browser detected — waiting for page load...")
    load_deadline = time.time() + BROWSER_LOAD_TIMEOUT
    grace_deadline = time.time() + BROWSER_WEB_AREA_GRACE

    web_area = _find_ax_web_area(window)
    while not web_area and time.time() < grace_deadline:
        time.sleep(AX_TREE_READY_INTERVAL)
        web_area = _find_ax_web_area(window)

    if not web_area:
        print("  No web content in this window — scanning as-is.")
        return True

    if ax_attr(web_area, "AXLoaded") is not None:
        while time.time() < load_deadline:
            loaded = ax_attr(web_area, "AXLoaded")
            if loaded is None or loaded:
                print("  Page loaded.")
                break
            progress = ax_attr(web_area, "AXLoadingProgress")
            if progress is not None:
                print(f"  Still loading... {int(float(progress) * 100)}%")
            time.sleep(AX_TREE_READY_INTERVAL)
        else:
            print("  Page load timeout — scanning anyway.")

    deadline = time.time() + timeout
    while time.time() < deadline:
        children = ax_attr(web_area, "AXChildren")
        if children and len(children) > 0:
            print("  Web content AX tree ready.")
            return True
        time.sleep(AX_TREE_READY_INTERVAL)

    print("  Timeout waiting for web content AX tree — scanning anyway.")
    return False


# ========== LABEL BUILDER ==========

GENERIC_LABELS = frozenset({
    "", "group", "application", "image", "text", "button", "cell", "row",
    "tab", "radio button", "check box", "menu bar item", "menu extra"
})


def build_label(element, cfg, prefetch=None):
    """Try each fallback attribute, return first non-empty, non-generic string.

    `prefetch` is an ax_attrs() result covering (some of) the fallback
    attributes; attributes present there are read from it instead of paying
    another IPC call. Pseudo-attributes and attributes deliberately absent
    from the batch (AXTextArea's AXValue) still hit the live path."""
    for attr in cfg.get("fallback", []):
        if attr == "_title_ui_element":
            if prefetch is not None and "AXTitleUIElement" in prefetch:
                linked = prefetch["AXTitleUIElement"]
            else:
                linked = ax_attr(element, "AXTitleUIElement")
            if linked:
                for sub in ("AXValue", "AXTitle", "AXDescription"):
                    v = ax_attr(linked, sub)
                    if v:
                        label = str(v).replace("\n", " ").strip()
                        if label.lower() in GENERIC_LABELS:
                            continue
                        return label[:50] if len(label) > 50 else label
            continue

        if attr == "_children_text":
            children = ax_attr(element, "AXChildren")
            if children:
                try:
                    for child in children:
                        cr = ax_attr(child, "AXRole")
                        if cr and str(cr) == "AXStaticText":
                            val = ax_attr(child, "AXValue") or ax_attr(child, "AXTitle")
                            if val:
                                label = str(val).replace("\n", " ").strip()
                                if label.lower() not in GENERIC_LABELS:
                                    return label[:50] if len(label) > 50 else label
                except Exception:
                    pass
            continue

        if prefetch is not None and attr in prefetch:
            val = prefetch[attr]
        else:
            val = ax_attr(element, attr)
        if val:
            label = str(val).replace("\n", " ").strip()
            if label.lower() in GENERIC_LABELS:
                continue
            return label[:50] if len(label) > 50 else label
    return ""


# ========== TREE WALK ==========

_seen_roles = set()

CLIP_ROLES = frozenset({"AXScrollArea", "AXList", "AXOutline", "AXTable"})

# Cleaned ("AX"-stripped) roles that are structural wrappers — they never claim
# screen space for OCR overlap purposes; gaps between their children are exactly
# where OCR fills in. Only "Group" actually reaches the tree today (the rest are
# CLIP_ROLES that aren't tracked as nodes); listing them is future-proofing.
OCR_STRUCTURAL_CONTAINER_TYPES = frozenset({
    "Group", "ScrollArea", "List", "Outline", "Table",
    "WebArea", "SplitGroup", "TabGroup", "Toolbar",
})


def _overlaps(a, b):
    """Return True if rect a overlaps rect b."""
    return not (a["x"] + a["width"] <= b["x"] or a["x"] >= b["x"] + b["width"] or
                a["y"] + a["height"] <= b["y"] or a["y"] >= b["y"] + b["height"])


def _on_screen(frame, screen):
    """Return True if frame overlaps the target screen bounds."""
    return not (frame["x"] + frame["width"] <= screen["x"]
                or frame["x"] >= screen["x"] + screen["width"]
                or frame["y"] + frame["height"] <= screen["y"]
                or frame["y"] >= screen["y"] + screen["height"])


def _rect_intersect(a, b):
    """Return intersection rect of a and b, or None if no overlap."""
    x1 = max(a["x"], b["x"])
    y1 = max(a["y"], b["y"])
    x2 = min(a["x"] + a["width"], b["x"] + b["width"])
    y2 = min(a["y"] + a["height"], b["y"] + b["height"])
    if x2 <= x1 or y2 <= y1:
        return None
    return {"x": x1, "y": y1, "width": x2 - x1, "height": y2 - y1}


def _visibility_pct(frame, clip, screen):
    """Compute what % of frame is visible within clip rect and screen bounds."""
    total = frame["width"] * frame["height"]
    if total <= 0:
        return 0.0
    visible = frame
    if clip:
        visible = _rect_intersect(visible, clip)
        if not visible:
            return 0.0
    visible = _rect_intersect(visible, screen)
    if not visible:
        return 0.0
    return (visible["width"] * visible["height"]) / total * 100.0


def _point_in_rect(px, py, rect):
    """Return True if point (px, py) is inside rect."""
    return (rect["x"] <= px <= rect["x"] + rect["width"]
            and rect["y"] <= py <= rect["y"] + rect["height"])


def _ancestor_clipped_visibility(frame, ancestors, screen, window_clip=None,
                                 scroll_clip=None):
    """Bottom-up visibility check — mirrors Windows _get_clipping_ancestors.
    Returns (visibility_str, visible_rect_dict_or_None).

    `ancestors` may be a list of frame dicts (legacy callers) or
    `(frame, role)` tuples; tuple form lets us recognise scroll containers
    and skip the fixed/sticky safety-net for them.

    `scroll_clip`, when provided, is the innermost scrollable container's
    viewport rect. Elements outside it are scroll-clipped — strictly hidden,
    no safety-net.
    """
    visible = dict(frame)

    if scroll_clip is not None:
        inter = _rect_intersect(visible, scroll_clip)
        if inter is None:
            return "hidden", None
        visible = inter

    for anc in ancestors:
        if anc is None:
            continue
        if isinstance(anc, tuple):
            anc_frame, anc_role = anc
            if anc_frame is None:
                continue
        else:
            anc_frame, anc_role = anc, None
        if anc_frame["width"] < 50 or anc_frame["height"] < 50:
            continue

        inter = _rect_intersect(visible, anc_frame)
        if inter is None:
            # Scroll containers are authoritative — if the element's frame is
            # outside the viewport, it really is scrolled out. Don't bypass.
            if anc_role in CLIP_ROLES:
                return "hidden", None
            anc_on_screen = _rect_intersect(anc_frame, screen) is not None
            anc_large = anc_frame["width"] >= 100 and anc_frame["height"] >= 100
            if anc_on_screen and anc_large:
                # Safety net for CSS position:fixed / sticky elements —
                # their AX parent frames may not encompass them even though
                # the element is clearly visible within the window.
                if (window_clip
                        and _rect_intersect(frame, window_clip)
                        and _rect_intersect(frame, screen)):
                    continue
                return "hidden", None
            continue

        vis_area = visible["width"] * visible["height"]
        int_area = inter["width"] * inter["height"]
        if vis_area > 0 and int_area < vis_area * 0.95:
            visible = inter

    visible = _rect_intersect(visible, screen)
    if visible is None:
        return "hidden", None

    total = frame["width"] * frame["height"]
    if total <= 0:
        return "hidden", None
    pct = (visible["width"] * visible["height"]) / total * 100.0
    if pct >= 99.0:
        return "full", None
    elif pct > 0:
        return f"partial {int(pct)}%", visible
    else:
        return "hidden", None


_WALK_CORE_ATTRS = ("AXRole", "AXFrame", "AXChildren")


def walk(element, results, depth, screen, clip=None, parent_frame=None,
         skip_roles=None, ancestors=None, is_browser=False, window_clip=None,
         pid=0):
    """Recursively walk AX tree, collect elements matching ELEMENT_CONFIG.

    Attribute reads are batched into at most two IPC round-trips per node:
    a core batch every node needs (role/frame/children), and a per-role batch
    for tracked nodes (flags + label fallbacks + value). `pid` is the owning
    process id, threaded down since a subtree shares one process.
    """
    if depth > MAX_DEPTH:
        return
    if ancestors is None:
        ancestors = []

    core = ax_attrs(element, _WALK_CORE_ATTRS)
    role = core["AXRole"]
    if role is None:
        return
    role_str = str(role)
    _seen_roles.add(role_str)

    my_frame = _frame_from_value(element, core["AXFrame"])
    child_clip = clip
    if role_str in CLIP_ROLES and my_frame:
        if clip:
            child_clip = _rect_intersect(clip, my_frame) or clip
        else:
            child_clip = my_frame

    cfg = ELEMENT_CONFIG.get(role_str)
    if cfg and cfg.get("track") and not (skip_roles and role_str in skip_roles):
        use_focus = is_browser and not cfg.get("is_enabled_flag", True)
        if use_focus:
            pre = ax_attrs(element, _PHASE2_ATTRS_FOCUS[role_str])
            focused = pre["AXFocused"]
            skip = focused is not None and not focused
        else:
            pre = ax_attrs(element, _PHASE2_ATTRS[role_str])
            enabled = pre["AXEnabled"]
            skip = enabled is not None and not enabled

        if not skip:
            frame = my_frame

            if frame and frame["width"] > 0 and frame["height"] > 0:
                label = build_label(element, cfg, pre)
                if label:
                    vis_str, vis_rect = _ancestor_clipped_visibility(
                        frame, ancestors, screen, window_clip,
                        scroll_clip=clip)

                    if vis_str != "hidden":
                        # Capture the element's value (URL for the omnibox, typed
                        # text for inputs) for single-line value-bearing controls —
                        # mirrors the Windows scanner's ValuePattern read. AXTextArea
                        # is deliberately excluded so multi-line editors (code cells /
                        # documents) don't dump their whole contents into the tree.
                        value = None
                        if role_str in ("AXTextField", "AXComboBox"):
                            raw_val = pre.get("AXValue")
                            if raw_val is not None:
                                v = str(raw_val).replace("\n", " ").strip()
                                if v and v.lower() != label.lower():
                                    value = v[:200]  # safety cap (domain is at front)
                        results.append({
                            "type": role_str,
                            "label": label,
                            "value": value,
                            "x": frame["x"],
                            "y": frame["y"],
                            "width": frame["width"],
                            "height": frame["height"],
                            "depth": depth,
                            "visibility": vis_str,
                            "visible_rect_raw": vis_rect,
                            "ax_element": element,
                            "_window_frame": window_clip,
                            "_pid": pid,
                        })

    my_entry = (my_frame, role_str) if my_frame else None
    child_ancestors = ancestors + [my_entry] if my_entry else ancestors
    children = core["AXChildren"]
    if not children and role_str in _CONTENTS_ROLES:
        # NSBrowser column views (e.g. the Open/Save file dialog) expose their
        # column scroll areas with an EMPTY AXChildren — the actual content
        # (an AXList of file rows) is reachable only via AXContents. Fall back
        # so these lazy containers get traversed instead of dead-ending here.
        children = ax_attr(element, "AXContents")
    if children:
        try:
            for child in children:
                walk(child, results, depth + 1, screen, child_clip,
                     my_frame, skip_roles, child_ancestors, is_browser,
                     window_clip, pid)
        except Exception:
            pass


# ========== SOURCE EXTRACTORS ==========

def find_app(bundle_id):
    """Find running app by bundle ID."""
    for app in NSWorkspace.sharedWorkspace().runningApplications():
        if app.bundleIdentifier() == bundle_id:
            return app
    return None


def _rect_overlaps(ax, ay, aw, ah, screen):
    """Check if a rectangle overlaps the screen bounds."""
    return not (ax + aw <= screen["x"] or ax >= screen["x"] + screen["width"] or
                ay + ah <= screen["y"] or ay >= screen["y"] + screen["height"])


def _is_agent_glow(w):
    """True for a window of the agent's own edge glow (AutoCua/utils/agent_glow.py).

    Those are click-through, translucent strips over all four screen edges at
    screen-saver level, marked NSWindowSharingNone (kCGWindowSharingState 0)
    so they can be told apart here. They hide nothing and are no dialog, so
    the window scans below skip them. Counted as occluders, everything within
    ~58 pt of a screen edge would be marked covered and dropped.
    """
    return w.get("kCGWindowSharingState", 1) == 0 and w.get("kCGWindowLayer", 0) >= 1000


def _find_topmost_app_on_screen(screen):
    """Find the topmost app window on the built-in screen.
    Returns (app_info, window_stack)."""
    flags = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    win_list = CGWindowListCopyWindowInfo(flags, kCGNullWindowID)
    if not win_list:
        return None, []

    skip_owners = {"Window Server", "Dock", "SystemUIServer", "Control Center", "Notification Center"}

    topmost = None
    window_stack = []

    for w in win_list:
        owner = w.get("kCGWindowOwnerName", "")
        if owner in skip_owners:
            continue
        if w.get("kCGWindowLayer", -1) != 0:
            continue
        bounds = w.get("kCGWindowBounds")
        if not bounds:
            continue
        wx = bounds.get("X", 0)
        wy = bounds.get("Y", 0)
        ww = bounds.get("Width", 0)
        wh = bounds.get("Height", 0)
        if ww < 50 or wh < 50:
            continue
        if not _rect_overlaps(wx, wy, ww, wh, screen):
            continue

        pid = w.get("kCGWindowOwnerPID", 0)
        frame = {"x": wx, "y": wy, "width": ww, "height": wh}
        window_stack.append({"pid": pid, "name": owner, "frame": frame})

        if topmost is None:
            topmost = {"name": owner, "pid": pid, "frame": frame}

    return topmost, window_stack


def _build_full_occluder_stack(screen):
    """Front-to-back list of every on-screen window (all layers).

    Each entry: {pid, name, frame, window_id, layer}. Skips Window Server
    and Dock; skips off-screen and tiny windows."""
    flags = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
    wins = CGWindowListCopyWindowInfo(flags, kCGNullWindowID)
    skip_owners = {"Window Server", "Dock"}
    stack = []
    if not wins:
        return stack
    for w in wins:
        owner = w.get("kCGWindowOwnerName", "")
        if owner in skip_owners:
            continue
        if _is_agent_glow(w):
            continue
        bounds = w.get("kCGWindowBounds")
        if not bounds:
            continue
        ww = bounds.get("Width", 0)
        wh = bounds.get("Height", 0)
        if ww < 50 or wh < 50:
            continue
        wx = bounds.get("X", 0)
        wy = bounds.get("Y", 0)
        if not _rect_overlaps(wx, wy, ww, wh, screen):
            continue
        stack.append({
            "pid": w.get("kCGWindowOwnerPID", 0),
            "name": owner,
            "frame": {"x": wx, "y": wy, "width": ww, "height": wh},
            "window_id": w.get("kCGWindowNumber", 0),
            "layer": w.get("kCGWindowLayer", 0),
        })
    return stack


def _apply_window_occlusion(results, screen):
    """Recompute per-element visibility against the real on-screen window
    z-order. Drops elements whose visible area is effectively zero.

    Elements without a known owning window (e.g. menu-bar walk results that
    lack `_window_frame`) are left untouched."""
    full_stack = _build_full_occluder_stack(screen)
    if not full_stack:
        return results

    # Cache: (pid, (x, y, w, h)) -> owning index in full_stack
    owning_cache = {}

    def _owning_index(pid, win_frame):
        if win_frame is None or not pid:
            return -1
        key = (pid, win_frame["x"], win_frame["y"],
               win_frame["width"], win_frame["height"])
        if key in owning_cache:
            return owning_cache[key]
        best = -1
        for i, w in enumerate(full_stack):
            if w["pid"] != pid:
                continue
            wf = w["frame"]
            if (abs(wf["x"] - win_frame["x"]) < 20
                    and abs(wf["y"] - win_frame["y"]) < 20
                    and abs(wf["width"] - win_frame["width"]) < 20
                    and abs(wf["height"] - win_frame["height"]) < 20):
                best = i
                break
        owning_cache[key] = best
        return best

    out = []
    for e in results:
        win_frame = e.get("_window_frame")
        pid = e.get("_pid")
        idx = _owning_index(pid, win_frame)
        if idx < 0:
            # Unknown owning window (menu-bar walk, dock) — leave as-is.
            out.append(e)
            continue

        elem_rect = {"x": e["x"], "y": e["y"],
                     "width": e["width"], "height": e["height"]}
        occluders = []
        for w in full_stack[:idx]:
            if w["window_id"] and w["window_id"] == full_stack[idx].get("window_id"):
                continue
            inter = _rect_intersect(elem_rect, w["frame"])
            if inter is not None:
                occluders.append(w["frame"])

        if not occluders:
            out.append(e)
            continue

        frac = _visible_fraction_after_occluders(elem_rect, occluders)
        # Combine with walk-time clipping fraction.
        vr = e.get("visible_rect_raw")
        if vr:
            walk_frac = (vr["width"] * vr["height"]) / max(
                1, elem_rect["width"] * elem_rect["height"])
        else:
            walk_frac = 1.0
        final = walk_frac * frac

        if final < 0.01:
            continue  # drop fully-occluded
        if final >= 0.99:
            e["visibility"] = "full"
        else:
            e["visibility"] = f"partial {int(final * 100)}%"
        out.append(e)

    return out


def _visible_fraction_after_occluders(rect, occluder_rects, samples=20):
    """Return uncovered-area fraction of rect (0.0..1.0) using a grid sample.

    `occluder_rects` is a list of rect dicts that paint on top of `rect`.
    A grid point is "covered" if it lies inside ANY occluder. Uses
    samples x samples points (default 400)."""
    if rect["width"] <= 0 or rect["height"] <= 0:
        return 0.0
    if not occluder_rects:
        return 1.0
    step_x = rect["width"] / samples
    step_y = rect["height"] / samples
    covered = 0
    total = samples * samples
    for i in range(samples):
        px = rect["x"] + (i + 0.5) * step_x
        for j in range(samples):
            py = rect["y"] + (j + 0.5) * step_y
            for occ in occluder_rects:
                if (occ["x"] <= px <= occ["x"] + occ["width"]
                        and occ["y"] <= py <= occ["y"] + occ["height"]):
                    covered += 1
                    break
    return (total - covered) / total


def _scan_menu_bar(screen, top_pid):
    """Return app-menu-bar items visible on the target screen."""
    cfg = ELEMENT_CONFIG.get("AXMenuBarItem", {})
    menu_strip_bottom = screen["y"] + 40

    def _x_overlaps_screen(frame):
        return not (frame["x"] + frame["width"] <= screen["x"]
                    or frame["x"] >= screen["x"] + screen["width"])

    def _collect(ax_source):
        out = []
        mb = ax_attr(ax_source, "AXMenuBar")
        if not mb:
            return out
        children = ax_attr(mb, "AXChildren")
        if not children:
            return out
        for child in children:
            role = ax_attr(child, "AXRole")
            if not (role and str(role) == "AXMenuBarItem"):
                continue
            frame = get_frame(child)
            if not (frame and frame["width"] > 0 and frame["height"] > 0):
                continue
            if not _x_overlaps_screen(frame):
                continue
            if frame["y"] > menu_strip_bottom:
                continue
            label = build_label(child, cfg)
            if not label:
                continue
            subrole = str(ax_attr(child, "AXSubrole") or "")
            mtype = "AXStatusMenu" if subrole == "AXMenuExtra" else "AXMenuBarItem"
            out.append({
                "type": mtype, "label": label,
                "x": frame["x"], "y": frame["y"],
                "width": frame["width"], "height": frame["height"],
            })
        return out

    def _try_app(pid):
        return _collect(AXUIElementCreateApplication(pid))

    if top_pid:
        items = _try_app(top_pid)
        if items:
            return items

    finder = find_app("com.apple.finder")
    if finder:
        items = _try_app(finder.processIdentifier())
        if items:
            return items

    ws = NSWorkspace.sharedWorkspace()
    for app in ws.runningApplications():
        try:
            if app.activationPolicy() != 0:
                continue
            pid = app.processIdentifier()
            ax_app = AXUIElementCreateApplication(pid)
            # Last-resort probe of regular apps — cap so a busy one can't
            # stall the scan, and reset (timeouts persist per-connection).
            AXUIElementSetMessagingTimeout(ax_app, 0.25)
            try:
                mb = ax_attr(ax_app, "AXMenuBar")
                if not mb:
                    continue
                mf = get_frame(mb)
                if not (mf and _x_overlaps_screen(mf)
                        and mf["width"] > screen["width"] * 0.3):
                    continue
                items = _collect(ax_app)
                if items:
                    return items
            finally:
                AXUIElementSetMessagingTimeout(ax_app, 0)
        except Exception:
            pass

    return []


def _walk_finder_desktop(screen, results):
    """Walk Finder's AXDesktop window to capture desktop icons/widgets."""
    finder = find_app("com.apple.finder")
    if not finder:
        return
    finder_pid = finder.processIdentifier()
    finder_ax = AXUIElementCreateApplication(finder_pid)
    windows = ax_attr(finder_ax, "AXWindows")
    if not windows:
        return
    screen_rect = {"x": screen["x"], "y": screen["y"],
                   "width": screen["width"], "height": screen["height"]}
    try:
        for win in windows:
            role = ax_attr(win, "AXRole")
            if role and str(role) == "AXDesktop":
                walk(win, results, 0, screen, clip=screen_rect,
                     window_clip=screen_rect, pid=finder_pid)
                return
    except Exception:
        pass


def _force_focus_topmost(screen, top):
    """Activate the topmost app so macOS updates its menu bar.

    Skips activation when the frontmost process has no real menu bar
    (Spotlight, status-menu popup, etc.) — the visible menu bar still
    belongs to the last regular app, and activating would dismiss the
    transient UI.

    Returns the target_pid used for menu-bar scanning.
    """
    ws = NSWorkspace.sharedWorkspace()
    prev = ws.frontmostApplication()
    prev_pid = prev.processIdentifier() if prev else -1

    target_pid = top["pid"] if top else None

    if target_pid is None:
        finder = find_app("com.apple.finder")
        if finder:
            target_pid = finder.processIdentifier()

    if target_pid is None or target_pid == prev_pid:
        return target_pid

    # If the frontmost app has no AXMenuBar (or fewer than 2 items),
    # it is system-level transient UI (Spotlight, status menu popup).
    # The on-screen menu bar still belongs to the last regular app —
    # skip activation so the transient UI is not dismissed.
    front_ax = AXUIElementCreateApplication(prev_pid)
    front_mb = ax_attr(front_ax, "AXMenuBar")
    if not front_mb:
        return target_pid
    mb_children = ax_attr(front_mb, "AXChildren")
    if not mb_children or len(mb_children) < 2:
        return target_pid

    for app in ws.runningApplications():
        if app.processIdentifier() == target_pid:
            app.activateWithOptions_(NSApplicationActivateIgnoringOtherApps)
            # Poll for the switch instead of sleeping a flat 0.3s — activation
            # usually lands in well under 100ms, and this sits on the critical
            # path before the screenshot every time the agent changes app.
            deadline = time.time() + 0.3
            while time.time() < deadline:
                front = ws.frontmostApplication()
                if front and front.processIdentifier() == target_pid:
                    break
                time.sleep(0.02)
            break

    return target_pid


_status_probe_cache = {}      # pid -> bool: does this process own a status bar?
_status_last_full_probe = 0.0
STATUS_PROBE_TIMEOUT = 0.25   # seconds per AX message while probing
STATUS_REPROBE_INTERVAL = 30  # seconds before re-testing known-negative apps
STATUS_PROBE_THREADS = 6      # parallel probes (see _probe_status_bars)


def _probe_status_bars(pids):
    """Return {pid: owns_a_status_bar} for pids, probed in parallel.

    Each probe talks to a DIFFERENT process, so unlike walking a single
    app's tree — where the target's accessibility server serialises every
    request and threads buy nothing — these genuinely overlap: probing ~100
    apps drops from 1.8s to 0.26s. The per-message timeout still bounds any
    single unresponsive process."""
    out = {}
    if not pids:
        return out

    def probe(chunk):
        for pid in chunk:
            ax = AXUIElementCreateApplication(pid)
            AXUIElementSetMessagingTimeout(ax, STATUS_PROBE_TIMEOUT)
            try:
                out[pid] = ax_attr(ax, "AXExtrasMenuBar") is not None
            except Exception:
                out[pid] = False
            finally:
                AXUIElementSetMessagingTimeout(ax, 0)

    n = min(STATUS_PROBE_THREADS, len(pids))
    if n <= 1:
        probe(pids)
        return out
    threads = [threading.Thread(target=probe, args=(pids[i::n],))
               for i in range(n)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return out


def _status_item_pids():
    """PIDs of processes that own menu-bar status items.

    Status items cannot be found from the window list: only Control Center
    backs its icons with real windows — SystemUIServer (Siri), Spotlight and
    third-party apps own no window at all and draw through the menu-bar
    server. So every process genuinely has to be asked for AXExtrasMenuBar.

    Two things keep that cheap. A per-message timeout stops one busy process
    from dominating the scan (an unresponsive Safari helper alone used to
    cost 1.5s of the 3.1s sweep; the whole sweep is 0.27s with the timeout).
    And the answer is cached per PID, so steady-state scans only probe
    newly-launched processes. Known-negative processes are re-tested every
    STATUS_REPROBE_INTERVAL seconds, since an app can add a status item long
    after launch."""
    global _status_last_full_probe

    now = time.time()
    live = {app.processIdentifier() for app
            in NSWorkspace.sharedWorkspace().runningApplications()}

    for dead in [p for p in _status_probe_cache if p not in live]:
        del _status_probe_cache[dead]

    recheck = (now - _status_last_full_probe) >= STATUS_REPROBE_INTERVAL
    if recheck:
        _status_last_full_probe = now

    todo = [pid for pid in live
            if pid not in _status_probe_cache
            or (recheck and not _status_probe_cache[pid])]
    if todo:
        _status_probe_cache.update(_probe_status_bars(todo))

    return [pid for pid in live if _status_probe_cache.get(pid)]


def _walk_open_menus(mb, results, screen, pid):
    """Walk the app menu bar, skipping CLOSED menus.

    macOS exposes every menu's full item subtree even while the menu is
    closed (hundreds of phantom AXMenuItems). An open menu's AXMenuBarItem
    reports AXSelected=True, so only those are worth descending into. Fails
    open — items that don't publish AXSelected are still walked."""
    children = ax_attr(mb, "AXChildren")
    if not children:
        return
    try:
        for item in children:
            role = ax_attr(item, "AXRole")
            if role and str(role) == "AXMenuBarItem":
                selected = ax_attr(item, "AXSelected")
                if selected is not None and not selected:
                    continue
            walk(item, results, 1, screen, skip_roles={"AXMenuBarItem"},
                 pid=pid)
    except Exception:
        pass


def prepare_scan(screen):
    """Scan phase 1: every operation that can change on-screen pixels.

    Activation, the AXEnhancedUserInterface toggle and the browser page-load
    wait all run BEFORE the screenshot, so the capture (and the OCR thread
    that runs on it) sees the final frame while the read-only
    collect_elements() phase runs underneath OCR."""
    top, window_stack = _find_topmost_app_on_screen(screen)
    activated_pid = _force_focus_topmost(screen, top)

    ctx = {
        "top": top,
        "window_stack": window_stack,
        "activated_pid": activated_pid,
        "app_ax": None,
        "windows": None,
        "matched_windows": [],
        "is_browser": False,
    }

    if not top:
        return ctx

    app_ax = AXUIElementCreateApplication(top["pid"])
    ctx["app_ax"] = app_ax
    ctx["is_browser"] = _is_browser(top["pid"])

    # Hang guard: cap every message to this app at 1s (a busy Chromium can
    # legitimately take a few hundred ms mid-layout, so not tighter).
    # collect_elements lifts it — the timeout persists on the connection and
    # would otherwise bleed into the controller's later action calls.
    AXUIElementSetMessagingTimeout(app_ax, 1.0)

    # Enhanced-UI makes Chromium/Electron publish their full AX tree. It
    # sticks per app, so pay the set + settle sleep only when it actually
    # flips — reading it back first is one IPC call vs a fixed 0.3s.
    if not ax_attr(app_ax, "AXEnhancedUserInterface"):
        err = AXUIElementSetAttributeValue(app_ax, "AXEnhancedUserInterface", True)
        if err == kAXErrorSuccess:
            time.sleep(0.3)

    windows = ax_attr(app_ax, "AXWindows")
    ctx["windows"] = windows
    if windows:
        tolerance = 20
        top_frame = top["frame"]
        for win in windows:
            wf = get_frame(win)
            if wf and (abs(wf["x"] - top_frame["x"]) < tolerance
                       and abs(wf["y"] - top_frame["y"]) < tolerance
                       and abs(wf["width"] - top_frame["width"]) < tolerance
                       and abs(wf["height"] - top_frame["height"]) < tolerance):
                ctx["matched_windows"].append((win, wf))

        if ctx["is_browser"] and ctx["matched_windows"]:
            t0 = time.time()
            _wait_for_ax_web_content(app_ax, ctx["matched_windows"][0][0])
            if time.time() - t0 > 0.5:
                # A navigation was genuinely in flight — give the fresh page
                # one beat to finish compositing before the capture.
                time.sleep(0.2)

    return ctx


def collect_elements(screen, ctx):
    """Scan phase 2: read-only element gathering from every source.

    Returns (app_info, menu_items, elements). Touches nothing that changes
    pixels, so it is safe to run while the OCR thread chews on the
    screenshot captured between the phases."""
    results = []
    menu_items = []
    app_info = None

    top = ctx["top"]
    window_stack = ctx["window_stack"]
    activated_pid = ctx["activated_pid"]

    menu_items.extend(_scan_menu_bar(screen, activated_pid))

    if activated_pid:
        mb = ax_attr(AXUIElementCreateApplication(activated_pid), "AXMenuBar")
        if mb:
            _walk_open_menus(mb, results, screen, activated_pid)

    if top:
        app_info = {"name": top["name"], "frame": top["frame"]}
        is_browser = ctx["is_browser"]
        windows = ctx["windows"]
        matched_windows = ctx["matched_windows"]

        if windows:
            if matched_windows:
                walked_wins = set()
                for win, wf in matched_windows:
                    walk(win, results, 0, screen, clip=wf, is_browser=is_browser,
                         window_clip=wf, pid=top["pid"])
                    walked_wins.add(id(win))
                # Walk any remaining visible, on-screen windows (dialogs, sheets, panels)
                for win in windows:
                    if id(win) in walked_wins:
                        continue
                    minimized = ax_attr(win, "AXMinimized")
                    if minimized:
                        continue
                    wf = get_frame(win)
                    if wf and _on_screen(wf, screen):
                        walk(win, results, 0, screen, clip=wf, is_browser=is_browser,
                             window_clip=wf, pid=top["pid"])
            else:
                screen_clip = {"x": screen["x"], "y": screen["y"],
                               "width": screen["width"], "height": screen["height"]}
                for win in windows:
                    walk(win, results, 0, screen, clip=screen_clip, is_browser=is_browser,
                         window_clip=screen_clip, pid=top["pid"])

        if window_stack:
            # Find overlay/dialog windows from other processes that actually
            # float ABOVE the topmost app (Spotlight, system popovers, sheets).
            # Walk full window list front-to-back; stop when we reach the
            # frontmost app's first layer-0 window — anything after is behind
            # it and must be excluded.
            dialog_pids = set()
            skip_dialog_owners = {"Window Server", "Dock"}
            top_frame = top["frame"]
            flags = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
            all_wins = CGWindowListCopyWindowInfo(flags, kCGNullWindowID)
            if all_wins:
                for w in all_wins:
                    wpid = w.get("kCGWindowOwnerPID", 0)
                    layer = w.get("kCGWindowLayer", -1)
                    if wpid == top["pid"] and layer == 0:
                        break  # reached frontmost app's window; stop
                    if wpid == top["pid"]:
                        continue  # frontmost app's own higher-layer windows already walked
                    owner = w.get("kCGWindowOwnerName", "")
                    if owner in skip_dialog_owners:
                        continue
                    if _is_agent_glow(w):
                        continue
                    bounds = w.get("kCGWindowBounds")
                    if not bounds:
                        continue
                    ww = bounds.get("Width", 0)
                    wh = bounds.get("Height", 0)
                    if ww < 50 or wh < 50:
                        continue
                    wx = bounds.get("X", 0)
                    wy = bounds.get("Y", 0)
                    if not _rect_overlaps(wx, wy, ww, wh, top_frame):
                        continue
                    dialog_pids.add(wpid)

            for dpid in dialog_pids:
                dialog_ax = AXUIElementCreateApplication(dpid)
                # Unknown, possibly-unresponsive process — cap and reset.
                AXUIElementSetMessagingTimeout(dialog_ax, 0.25)
                try:
                    d_windows = ax_attr(dialog_ax, "AXWindows")
                    if d_windows:
                        for dwin in d_windows:
                            dwf = get_frame(dwin)
                            if dwf and _on_screen(dwf, screen):
                                walk(dwin, results, 0, screen, clip=dwf,
                                     window_clip=dwf, pid=dpid)
                finally:
                    AXUIElementSetMessagingTimeout(dialog_ax, 0)

        # Done with the frontmost app's connection — lift the hang guard so
        # the controller's later action calls run at the default timeout.
        if ctx["app_ax"] is not None:
            AXUIElementSetMessagingTimeout(ctx["app_ax"], 0)

    else:
        finder = find_app("com.apple.finder")
        if finder:
            app_info = {"name": "Finder", "frame": {
                "x": screen["x"], "y": screen["y"],
                "width": screen["width"], "height": screen["height"],
            }}
            finder_pid = finder.processIdentifier()
            finder_ax = AXUIElementCreateApplication(finder_pid)
            screen_clip = {"x": screen["x"], "y": screen["y"],
                           "width": screen["width"], "height": screen["height"]}
            windows = ax_attr(finder_ax, "AXWindows")
            if windows:
                for win in windows:
                    wf = get_frame(win)
                    if wf and _on_screen(wf, screen):
                        minimized = ax_attr(win, "AXMinimized")
                        if minimized:
                            continue
                        walk(win, results, 0, screen, clip=screen_clip,
                             window_clip=screen_clip, pid=finder_pid)

        # Scan for overlay dialogs on desktop (no topmost app)
        skip_dialog_owners = {"Window Server", "Dock"}
        flags = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements
        all_wins = CGWindowListCopyWindowInfo(flags, kCGNullWindowID)
        if all_wins:
            dialog_pids = set()
            for w in all_wins:
                owner = w.get("kCGWindowOwnerName", "")
                if owner in skip_dialog_owners:
                    continue
                if _is_agent_glow(w):
                    continue
                bounds = w.get("kCGWindowBounds")
                if not bounds:
                    continue
                ww = bounds.get("Width", 0)
                wh = bounds.get("Height", 0)
                if ww < 50 or wh < 50:
                    continue
                wx = bounds.get("X", 0)
                wy = bounds.get("Y", 0)
                if not _rect_overlaps(wx, wy, ww, wh, screen):
                    continue
                dialog_pids.add(w.get("kCGWindowOwnerPID", 0))

            for dpid in dialog_pids:
                dialog_ax = AXUIElementCreateApplication(dpid)
                AXUIElementSetMessagingTimeout(dialog_ax, 0.25)
                try:
                    d_windows = ax_attr(dialog_ax, "AXWindows")
                    if d_windows:
                        for dwin in d_windows:
                            dwf = get_frame(dwin)
                            if dwf and _on_screen(dwf, screen):
                                walk(dwin, results, 0, screen, clip=dwf,
                                     window_clip=dwf, pid=dpid)
                finally:
                    AXUIElementSetMessagingTimeout(dialog_ax, 0)

    # Status menu items (right side) — only the apps that actually own a
    # status-bar window (see _status_item_pids), not every running process.
    for spid in sorted(_status_item_pids()):
        try:
            ax = AXUIElementCreateApplication(spid)
            AXUIElementSetMessagingTimeout(ax, 0.25)
            try:
                bar = ax_attr(ax, "AXExtrasMenuBar")
                if not bar:
                    continue
                items = ax_attr(bar, "AXChildren")
                if not items:
                    continue
                for item in items:
                    role = ax_attr(item, "AXRole")
                    role_s = str(role) if role else ""
                    if role_s in ("AXMenuExtra", "AXMenuBarItem", "AXStatusItem"):
                        frame = get_frame(item)
                        if frame and frame["width"] > 0 and frame["height"] > 0:
                            if not _on_screen(frame, screen):
                                continue
                            cfg = ELEMENT_CONFIG.get("AXMenuBarItem", {})
                            label = build_label(item, cfg)
                            if label:
                                menu_items.append({
                                    "type": "AXStatusMenu",
                                    "label": label,
                                    "x": frame["x"], "y": frame["y"],
                                    "width": frame["width"],
                                    "height": frame["height"],
                                })
            finally:
                AXUIElementSetMessagingTimeout(ax, 0)
        except Exception:
            pass

    menu_items.sort(key=lambda m: m["x"])

    # Desktop icons
    _walk_finder_desktop(screen, results)

    # Dock
    dock = find_app("com.apple.dock")
    if dock:
        dock_pid = dock.processIdentifier()
        walk(AXUIElementCreateApplication(dock_pid), results, 0, screen,
             pid=dock_pid)

    # ----- Real on-screen occlusion pass -----
    # Recompute each element's visibility against every window that paints
    # on top of its owning window. Drop fully-covered elements so the agent
    # never receives a click index for a coordinate it can't actually hit.
    results = _apply_window_occlusion(results, screen)

    # Deduplicate
    seen = set()
    unique = []
    for e in results:
        key = (e["label"], e["type"], round(e["x"]), round(e["y"]))
        if key not in seen:
            seen.add(key)
            unique.append(e)
    results = unique

    # Strip internal helper keys before returning so they don't leak.
    for e in results:
        e.pop("_window_frame", None)
        e.pop("_pid", None)

    return app_info, menu_items, results


def extract_all(screen):
    """Gather elements from all visible sources. Returns (app_info, menu_items, elements).

    Thin wrapper for standalone use; scan_elements() calls the two phases
    directly so the screenshot capture + OCR kickoff can sit between them."""
    return collect_elements(screen, prepare_scan(screen))


# ========== SCREENSHOT ==========

def take_screenshot(screen):
    """Capture built-in display only, return (PIL Image, scale, CGImage).

    The CGImage is handed back alongside the PIL copy so OCR can feed Vision
    the original surface instead of re-encoding one.

    Decodes the CGImage's raw BGRA buffer straight into PIL — no PNG
    encode/decode round-trip and no temp file (the old shared /tmp path was
    also a race between concurrent scanner instances). Falls back to an
    in-memory PNG when the pixel layout isn't the expected 32-bit
    little-endian BGRA."""
    try:
        scale = screen.get("scale", 2.0)
        cg_img = CGWindowListCreateImage(
            CGRectMake(screen["x"], screen["y"], screen["width"], screen["height"]),
            kCGWindowListOptionOnScreenOnly, kCGNullWindowID, 0,
        )
        if not cg_img:
            return None, 2.0, None

        w = CGImageGetWidth(cg_img)
        h = CGImageGetHeight(cg_img)
        bpr = CGImageGetBytesPerRow(cg_img)
        # Screen captures come back 32-bit little-endian alpha-first
        # (BGRA in memory). Alpha is fully opaque for a screen grab, so
        # premultiplication is a no-op and the buffer maps 1:1 into PIL.
        # The bytes-per-row stride is mandatory — rows are padded.
        bitmap_info = CGImageGetBitmapInfo(cg_img)
        little_endian = (bitmap_info & 0x7000) == 0x2000  # kCGBitmapByteOrder32Little
        alpha_first = CGImageGetAlphaInfo(cg_img) in (2, 4, 6)
        if little_endian and alpha_first:
            data = CGDataProviderCopyData(CGImageGetDataProvider(cg_img))
            # bytes() copies out of the CF-owned buffer so the PIL image
            # never aliases memory CoreFoundation may reclaim.
            return Image.frombuffer("RGBA", (w, h), bytes(data), "raw",
                                    "BGRA", bpr, 1), scale, cg_img

        # Unexpected layout — decode via an in-memory PNG instead.
        bmp = NSBitmapImageRep.alloc().initWithCGImage_(cg_img)
        png = bmp.representationUsingType_properties_(NSPNGFileType, None)
        img = Image.open(io.BytesIO(bytes(png)))
        img.load()
        return img, scale, cg_img
    except Exception as e:
        print(f"Screenshot error: {e}")
    return None, 2.0, None


# ========== XML ESCAPE ==========

def _xml_escape(text):
    """Escape special characters for XML attributes."""
    if not text:
        return ""
    return (text
            .replace('&', '&amp;')
            .replace('<', '&lt;')
            .replace('>', '&gt;')
            .replace('"', '&quot;'))


# ========== ANNOTATION FONT (cached) ==========

_ANNOTATE_FONT = None  # cached (font, stroke_width) — TTF parsing is per-scan waste
_label_heights = {}    # len(label) -> rendered height, so measuring is done once
_label_tiles = {}      # label -> pre-rendered RGBA tile (see _label_tile)
_label_inks = {}       # label -> ink bbox inside its tile, for the edge clamp


def _label_tile(label, font, stroke_w):
    """Return a cached, pre-rendered RGBA tile for an index label.

    Rasterising glyphs is the single most expensive part of annotation —
    PIL renders a stroked string twice (stroke mask + fill mask), and a busy
    screen carries a thousand labels. The label set is just "[1]".."[N]", so
    it repeats on every scan: render each one once per process and blit it
    thereafter. Pasting the tile at (x, y) is pixel-identical to drawing the
    text at (x, y), because the tile is measured from the same origin."""
    tile = _label_tiles.get(label)
    if tile is None:
        # Generously sized rather than measured: textbbox() rasterises the
        # string just to size it, doubling the work of building a tile. Any
        # extra margin is fully transparent, so it costs nothing at paste
        # time and the glyphs still land exactly where draw.text would put
        # them (both draw from the same origin).
        pad = 2 * stroke_w + 4
        tile = Image.new("RGBA",
                         (len(label) * LABEL_FONT_SIZE + pad,
                          2 * LABEL_FONT_SIZE + pad),
                         (0, 0, 0, 0))
        # Thin dark rim around the glyph shapes — NOT a filled chip, so the
        # UI underneath stays visible. Supplies the luma contrast magenta
        # lacks on its own, and renders in one pass instead of 8 offsets.
        ImageDraw.Draw(tile).text(
            (0, 0), label, fill=NUMBER_COLOR, font=font,
            stroke_width=stroke_w, stroke_fill=LABEL_STROKE_COLOR)
        # Where the glyphs actually sit in that generous tile, measured once:
        # _draw_boxes keeps the ink on canvas, not the transparent margin.
        # Stored before the tile, so a cached tile always has its ink.
        _label_inks[label] = tile.getbbox() or (0, 0, 0, 0)
        _label_tiles[label] = tile
    return tile


def _build_annotate_font():
    """Build a fresh font object for the index labels.

    Each caller gets its OWN instance: PIL/FreeType faces are not safe to
    render from two threads at once, and the tile pre-warmer runs alongside
    the main annotation path."""
    font = None
    for font_path in ("/System/Library/Fonts/Supplemental/Arial Bold.ttf",
                      "/System/Library/Fonts/Supplemental/Helvetica.ttc",
                      "/System/Library/Fonts/Helvetica.ttc",
                      "/System/Library/Fonts/SFNSMono.ttf",
                      "/System/Library/Fonts/Menlo.ttc"):
        try:
            font = ImageFont.truetype(font_path, LABEL_FONT_SIZE)
            break
        except Exception:
            continue
    # PIL renders strokes through FreeType, which load_default()'s bitmap
    # font lacks — asking for one there raises. Only stroke on a real face.
    stroke_w = LABEL_STROKE if font is not None else 0
    if font is None:
        font = ImageFont.load_default()
    return font, stroke_w


# NOTE: pre-rendering label tiles on a worker thread was tried and reverted.
# PIL rasterises glyphs through FreeType while HOLDING the GIL, so the
# "spare" CPU it was meant to use came straight out of the main thread's
# walk: annotation fell 0.70s -> 0.15s but collection rose 2.34s -> 4.53s.
# Only GIL-releasing work (the image resize below, Vision OCR) may overlap
# the walk.


def _load_annotate_font():
    """Load the index-label font once per process."""
    global _ANNOTATE_FONT
    if _ANNOTATE_FONT is None:
        _ANNOTATE_FONT = _build_annotate_font()
    return _ANNOTATE_FONT


# ========== SPATIAL CONTAINMENT (for tree hierarchy) ==========

def _contains(a, b):
    """Return True if element a spatially contains element b."""
    return (a["x"] <= b["x"] and a["y"] <= b["y"]
            and a["x"] + a["width"] >= b["x"] + b["width"]
            and a["y"] + a["height"] >= b["y"] + b["height"])


# ========== SCANNER CLASS ==========

class UIElementScanner:
    """macOS drop-in replacement for the Windows UIElementScanner.
    
    Exposes the same public interface so AutoCua/mac/agent/service.py,
    AutoCua/mac/controller/service.py, and AutoCua/mac/controller/view.py
    work without modifications.
    """

    def __init__(self, config, frontend_callback=None):
        self.config = config
        self.frontend_callback = frontend_callback

        # State populated by scan_elements()
        self.element_tree = []          # Hierarchical tree structure (top layer)
        self.menu_bar_tree = []         # Menu bar items as tree nodes
        self.element_index = 0          # Global index counter
        self.application_name = "Desktop"
        self.elements_to_draw = []      # List for screenshot bounding boxes
        self.elements_mapping = {}      # Mapping of index → element info for controller
        self.app_rect = None            # Application window rectangle
        self.top_layer_info = None      # {"name": ..., "type": "app"}
        self.second_layer_info = None   # Not used on macOS (no overlay layers)
        self.second_layer_tree = []     # Empty on macOS
        self.found_elements = {}        # Dictionary to store elements by type
        self._debug_iteration = 0       # Debug iteration counter

        # OCR state
        self._screenshot = None         # Single captured PIL screenshot (pixels)
        self._scale = 2.0               # Backing scale factor of the capture
        self._screen_size = (0, 0)      # (width, height) in CG points
        self._ocr_backdrop_max_area = float("inf")  # leaves bigger than this don't suppress OCR
        self.ocr_words = []             # Raw OCR lines (CG points, filtered later)
        self.ocr_scanner = None         # OCRScanner instance
        self.ocr_thread = None          # Thread handle for parallel OCR scan
        self._annotate_prep = None      # (shrunk RGB image, shrink factor) from worker
        self._annotate_thread = None    # Thread handle for annotate-base prep
        self._annotate_canvas = None    # (image, draw, draw_scale, origin) while annotating

    def scan_elements(self):
        """Scan the active window and menu bar for configured element types."""

        # Clear previous scan state
        self.element_tree = []
        self.menu_bar_tree = []
        self.second_layer_tree = []
        self.element_index = 0
        self.application_name = "Desktop"
        self.elements_to_draw = []
        self.elements_mapping = {}
        self.app_rect = None
        self.top_layer_info = None
        self.second_layer_info = None
        self.found_elements = {}
        self._screenshot = None
        self.ocr_words = []
        self.ocr_scanner = None
        self.ocr_thread = None
        self._annotate_prep = None
        self._annotate_thread = None
        self._annotate_canvas = None

        # Check accessibility permission
        if not AXIsProcessTrusted():
            print("\n⚠️  Accessibility permission required.")
            print("Grant in: System Settings > Privacy & Security > Accessibility")
            return

        # Get screen info
        screen = get_screen()
        self._screen_size = (screen["width"], screen["height"])

        # Run the macOS AX scan
        _seen_roles.clear()
        t_start = time.time()

        # Phase 1: everything that can change on-screen pixels (activation,
        # AXEnhancedUserInterface toggle, browser page-load wait).
        ctx = prepare_scan(screen)
        t_prepare = time.time()

        # Capture the screen ONCE, now that prepare_scan has settled the
        # frame, and start OCR immediately so it overlaps the entire
        # read-only collection phase below. The same image is reused for OCR
        # overlap-filtering and final annotation, so OCR boxes, AX boxes,
        # and the displayed screenshot share one frame.
        cg_frame = None
        if SCREENSHOT or OCR:
            self._screenshot, self._scale, cg_frame = take_screenshot(screen)
        if OCR and cg_frame is not None:
            # Vision reads the captured surface directly — no PIL copy, no
            # re-encode — so this worker holds the GIL far less while the AX
            # walk runs beside it.
            self.ocr_scanner = OCRScanner(
                cg_frame, self._scale,
                (screen["x"], screen["y"]),
                recognition_level=OCR_RECOGNITION_LEVEL,
                downscale_to_points=OCR_LOGICAL_RESOLUTION,
            )
            self.ocr_thread = threading.Thread(target=self.ocr_scanner.scan)
            self.ocr_thread.start()

        # Second worker: pre-shrink + RGB-convert the annotation base image.
        # PIL's resize releases the GIL, so this overlaps the AX walk and the
        # annotate step at the end only has to draw and encode.
        if SCREENSHOT and self._screenshot is not None:
            self._annotate_thread = threading.Thread(
                target=self._prepare_annotate_base)
            self._annotate_thread.start()
        t_capture = time.time()

        # Phase 2: read-only collection + tree build, overlapped with OCR.
        # The finally guarantees the OCR thread never outlives a failed scan.
        t_collect = t_capture
        try:
            app_info, menu_items, elements = collect_elements(screen, ctx)
            elements.sort(key=lambda e: (e["y"], e["x"]))

            # Store application name
            if app_info:
                self.application_name = app_info["name"]
                self.top_layer_info = {"name": app_info["name"], "type": "app"}
                if app_info.get("frame"):
                    f = app_info["frame"]
                    self.app_rect = Rect(
                        int(f["x"]), int(f["y"]),
                        int(f["x"] + f["width"]), int(f["y"] + f["height"])
                    )
            else:
                self.top_layer_info = {"name": "Desktop", "type": "app"}

            # ----- Build menu bar tree nodes -----
            for m in menu_items:
                self.element_index += 1
                # Map AX type to clean name (strip "AX" prefix)
                clean_type = m["type"].replace("AX", "") if m["type"].startswith("AX") else m["type"]
                rect = Rect(
                    int(m["x"]), int(m["y"]),
                    int(m["x"] + m["width"]), int(m["y"] + m["height"])
                )

                node = {
                    "element": None,    # No pywinauto element on macOS
                    "name": m["label"],
                    "aria_role": "",
                    "type": clean_type,
                    "active": True,
                    "index": self.element_index,
                    "value": None,
                    "actions": None,
                    "visibility": "full",
                    "clipped_by": None,
                    "rect": rect,
                    "visible_rect": rect,
                    "children": [],
                    "browser_top_layer": None,
                    "browser_second_layer": None,
                    "source": "",
                }
                self.menu_bar_tree.append(node)

                self.elements_mapping[str(self.element_index)] = {
                    'element': None,
                    'rect': rect,
                    'visible_rect': rect,
                    'name': m["label"],
                    'aria_role': '',
                    'type': clean_type,
                    'value': None,
                    'visibility': 'full',
                    'clipped_by': None,
                }

                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": rect,
                        "index": self.element_index,
                        "depth": 0,
                        "visibility": "full",
                        "source": "",
                    })

            # ----- Build element tree from flat elements using spatial containment -----
            self.element_tree = self._build_hierarchical_tree(elements)
            t_collect = time.time()

            # Draw the AX boxes NOW, while Vision is still working. OCR text
            # is numbered after every AX element, so none of these boxes or
            # labels can change once OCR lands — and the main thread would
            # otherwise just block on the join. Only the OCR boxes and the
            # final encode have to wait.
            if SCREENSHOT and self.elements_to_draw:
                if self._annotate_thread is not None:
                    self._annotate_thread.join()
                    self._annotate_thread = None
                self._begin_annotation(screen)
        finally:
            # Join the workers (also on failure, so no thread ever leaks
            # past its screenshot's lifetime).
            if self.ocr_thread is not None:
                self.ocr_thread.join()
                self.ocr_words = self.ocr_scanner.get_lines()
                self.ocr_thread = None
            if self._annotate_thread is not None:
                self._annotate_thread.join()
                self._annotate_thread = None
        t_ocr = time.time()

        # ----- Merge OCR survivors into the tree -----
        # Nest any text the AX tree missed (canvas / label-less controls) as
        # OCR_TEXT nodes that don't overlap an already-detected element.
        drawn = len(self.elements_to_draw)
        self._filter_and_merge_ocr()

        # ----- Finish the annotated screenshot -----
        self._debug_iteration += 1
        if SCREENSHOT and self.elements_to_draw:
            # Only the OCR boxes appended above still need drawing; the AX
            # boxes went on during the OCR wait.
            self._capture_and_annotate(screen, start_index=drawn)

        # ----- Save debug tree file -----
        if DEBUG:
            self.save_to_file()
            t_end = time.time()
            print(f"  [scan timing] prepare={t_prepare - t_start:.2f}s "
                  f"capture={t_capture - t_prepare:.2f}s "
                  f"collect+build={t_collect - t_capture:.2f}s "
                  f"ocr-wait={t_ocr - t_collect:.2f}s "
                  f"annotate={t_end - t_ocr:.2f}s "
                  f"total={t_end - t_start:.2f}s")

    def _build_hierarchical_tree(self, flat_elements):
        """Convert flat element list into a hierarchical tree using spatial containment,
        assign indices, and populate elements_mapping."""

        if not flat_elements:
            return []

        # Sort by area descending — larger containers first
        by_area = sorted(flat_elements, key=lambda e: e["width"] * e["height"], reverse=True)

        # Assign each element its nearest containing parent. Scanning
        # BACKWARDS and stopping at the first hit is equivalent to the old
        # forward scan that kept the last hit — the list is sorted by area
        # descending, so the nearest containing ancestor is the last one that
        # contains it — but it exits as soon as the parent is found instead of
        # always comparing against every earlier element (which made this
        # quadratic: ~500k comparisons for a 1000-element tree).
        n = len(by_area)
        parent = [None] * n
        depth = [0] * n
        for i in range(n):
            e = by_area[i]
            for j in range(i - 1, -1, -1):
                if _contains(by_area[j], e):
                    parent[i] = j
                    depth[i] = depth[j] + 1
                    break

        # Create tree nodes (indices assigned later after position sorting)
        nodes = []
        for i, e in enumerate(by_area):
            clean_type = e["type"].replace("AX", "") if e["type"].startswith("AX") else e["type"]

            rect = Rect(
                int(e["x"]), int(e["y"]),
                int(e["x"] + e["width"]), int(e["y"] + e["height"])
            )

            vis = e.get("visibility", "full")
            vr = e.get("visible_rect_raw")
            if vr:
                visible_rect = Rect(
                    int(vr["x"]), int(vr["y"]),
                    int(vr["x"] + vr["width"]), int(vr["y"] + vr["height"])
                )
            else:
                visible_rect = rect

            node = {
                "element": None,
                "name": e["label"],
                "aria_role": "",
                "type": clean_type,
                "active": True,
                "index": None,  # assigned after position sorting
                "value": e.get("value"),
                "actions": None,
                "visibility": vis,
                "clipped_by": None,
                "rect": rect,
                "visible_rect": visible_rect,
                "children": [],
                "browser_top_layer": None,
                "browser_second_layer": None,
                "source": "",
                "_parent_idx": parent[i],
                "_depth": depth[i],
                "_orig_idx": i,
                "_raw_element": e,
            }
            nodes.append(node)

        # Build parent-child relationships
        for i, node in enumerate(nodes):
            p = node["_parent_idx"]
            if p is not None:
                nodes[p]["children"].append(node)

        # Root nodes are those with no parent
        roots = [n for n in nodes if n["_parent_idx"] is None]

        # Sort children by position (top-to-bottom, left-to-right)
        def _sort_children(node_list):
            node_list.sort(key=lambda n: (n["rect"].top, n["rect"].left))
            for n in node_list:
                if n["children"]:
                    _sort_children(n["children"])

        _sort_children(roots)

        # Assign sequential indices via tree traversal (depth-first)
        def _assign_indices(node_list):
            for n in node_list:
                self.element_index += 1
                n["index"] = self.element_index
                e = n.pop("_raw_element")

                # Populate elements_mapping for controller
                self.elements_mapping[str(self.element_index)] = {
                    'element': None,
                    'rect': n["rect"],
                    'visible_rect': n["visible_rect"],
                    'name': n["name"],
                    'aria_role': '',
                    'type': n["type"],
                    'value': None,
                    'visibility': n["visibility"],
                    'clipped_by': None,
                    'ax_element': e.get("ax_element"),
                }

                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": n["rect"],
                        "index": self.element_index,
                        "depth": n.get("_depth", 0),
                        "visibility": n["visibility"],
                        "source": "",
                    })

                # Track found elements by type
                if n["type"] not in self.found_elements:
                    self.found_elements[n["type"]] = []
                self.found_elements[n["type"]].append(n)

                if n["children"]:
                    _assign_indices(n["children"])

        _assign_indices(roots)

        # Clean up internal keys
        def _cleanup(node_list):
            for n in node_list:
                n.pop("_parent_idx", None)
                n.pop("_depth", None)
                n.pop("_orig_idx", None)
                n.pop("_raw_element", None)
                if n["children"]:
                    _cleanup(n["children"])

        _cleanup(roots)

        return roots

    # ========== OCR MERGE (ported from windows/tree/element.py) ==========

    def _collect_leaf_rects(self, tree_list, rects):
        """Recursively collect rects that actually claim screen space, for OCR
        overlap checking. Structural wrappers (Group, ScrollArea, ...) never claim
        space — the gaps between their children are exactly where OCR fills in —
        so we recurse into them without adding their own rect. Every other element
        type claims its rect, EXCEPT backdrop-sized leaves (a full-window AXImage
        for a map / canvas, a big background) which shouldn't suppress the text
        drawn on top of them."""
        for item in tree_list:
            has_children = bool(item.get("children"))
            if item["type"] in OCR_STRUCTURAL_CONTAINER_TYPES:
                if has_children:
                    self._collect_leaf_rects(item["children"], rects)
            else:
                rect = item.get("rect") or item.get("visible_rect")
                if rect:
                    area = (rect.right - rect.left) * (rect.bottom - rect.top)
                    if area <= self._ocr_backdrop_max_area:
                        rects.append(rect)
                if has_children:
                    self._collect_leaf_rects(item["children"], rects)

    def _find_deepest_container(self, tree_list, cx, cy):
        """Find the deepest element whose rect contains point (cx, cy) and return
        its children list (so the OCR node nests there). Uses center-point
        containment — forgiving of OCR boxes that poke a pixel outside their
        visual parent. Returns the top-level tree_list if nothing claims the point."""
        for item in tree_list:
            rect = item.get("rect") or item.get("visible_rect")
            if not rect:
                continue
            if rect.left <= cx <= rect.right and rect.top <= cy <= rect.bottom:
                # Inside this element — try to go deeper among its children.
                if item.get("children"):
                    deeper = self._find_deepest_container(item["children"], cx, cy)
                    if deeper is not item["children"]:
                        return deeper
                    # Children exist but none claimed the point — nest as a sibling.
                    return item["children"]
                # Leaf element that contains the point — nest as its child.
                return item.setdefault("children", [])
        # No element in this list contains the point — caller decides.
        return tree_list

    def _filter_and_merge_ocr(self):
        """Filter OCR lines against detected elements, nest survivors into the tree.

        Only leaf (space-claiming) element rects suppress OCR; structural wrappers
        don't. An OCR line whose center sits inside a leaf rect is already covered
        by the AX scan and is discarded. Surviving lines (canvas / label-less text
        the AX tree missed) are nested into the deepest matching container as
        OCR_TEXT nodes and registered for clicking (coordinate-click fallback,
        ax_element=None) + drawing."""
        if not self.ocr_words:
            return

        # Backdrop guard: a leaf element covering a large fraction of the screen
        # is a canvas/background (e.g. Apple Maps' full-window "Map" AXImage), not
        # the label for any specific word. Such leaves must NOT suppress OCR text
        # drawn over them, or we lose every label on a map / large image / canvas.
        sw, sh = self._screen_size
        self._ocr_backdrop_max_area = (0.25 * sw * sh) if (sw and sh) else float("inf")

        # Collect rects that claim screen space (top layer + menu bar).
        leaf_rects = []
        self._collect_leaf_rects(self.element_tree, leaf_rects)
        self._collect_leaf_rects(self.menu_bar_tree, leaf_rects)

        # Drop OCR lines overlapping a detected leaf; also dedupe OCR-vs-OCR.
        kept_lines = []
        seen = set()
        for line in self.ocr_words:
            cx = (line["left"] + line["right"]) // 2
            cy = (line["top"] + line["bottom"]) // 2

            overlaps_leaf = any(
                r.left <= cx <= r.right and r.top <= cy <= r.bottom
                for r in leaf_rects
            )
            if overlaps_leaf:
                continue

            key = (line["text"], round(cx / 5), round(cy / 5))
            if key in seen:
                continue
            seen.add(key)
            kept_lines.append((line, cx, cy))

        # Nest each surviving OCR line into the deepest matching container.
        for line, cx, cy in kept_lines:
            self.element_index += 1
            line_rect = Rect(line["left"], line["top"], line["right"], line["bottom"])

            node = {
                "element": None,
                "name": line["text"],
                "aria_role": "",
                "type": "OCR_TEXT",
                "active": True,
                "index": self.element_index,
                "value": None,
                "actions": None,
                "visibility": "full",
                "clipped_by": None,
                "rect": line_rect,
                "visible_rect": line_rect,
                "children": [],
                "browser_top_layer": None,
                "browser_second_layer": None,
                "source": "ocr",
            }
            target_list = self._find_deepest_container(self.element_tree, cx, cy)
            target_list.append(node)

            # Controller mapping — ax_element=None routes to the coordinate-click fallback.
            self.elements_mapping[str(self.element_index)] = {
                'element': None,
                'rect': line_rect,
                'visible_rect': line_rect,
                'name': line["text"],
                'aria_role': '',
                'type': 'OCR_TEXT',
                'value': None,
                'visibility': 'full',
                'clipped_by': None,
                'ax_element': None,
            }

            # Track found elements by type (mirrors _assign_indices bookkeeping).
            self.found_elements.setdefault("OCR_TEXT", []).append(node)

            if SCREENSHOT:
                self.elements_to_draw.append({
                    "rect": line_rect,
                    "index": self.element_index,
                    "depth": 0,
                    "visibility": "full",
                    "source": "ocr",
                })

    def _prepare_annotate_base(self):
        """Worker-thread half of annotation: the LLM-payload downscale + RGB
        conversion of the screenshot. Runs during the AX walk (PIL's resize
        releases the GIL), so _capture_and_annotate only draws + encodes.

        Downscale FIRST, annotate later. Drawing before the resize would put
        every box and label through LANCZOS as well, shrinking the label and
        averaging its outline away to nothing. The screenshot gets resampled;
        the annotations don't."""
        try:
            img = self._screenshot
            src_w, src_h = img.size
            shrink = llm_image_shrink(src_w, src_h)
            if shrink < 1.0:
                # int() floors, so neither side can exceed its cap; float error
                # can leave the side that hits the cap 1 px short, harmlessly.
                img = img.resize(
                    (max(1, int(src_w * shrink)), max(1, int(src_h * shrink))),
                    Image.Resampling.LANCZOS)
            else:
                img = img.copy()
            # Convert to RGB before annotating: drawing into a palette-mode
            # image would quantise the label colour.
            if img.mode in ('RGBA', 'LA', 'P'):
                rgb = Image.new('RGB', img.size, (255, 255, 255))
                rgb.paste(img, mask=img.split()[-1] if img.mode == 'RGBA' else None)
                img = rgb
            elif img.mode != 'RGB':
                img = img.convert('RGB')
            self._annotate_prep = (img, shrink)
        except Exception:
            self._annotate_prep = None

    def _begin_annotation(self, screen):
        """Prepare the annotation canvas and draw the AX boxes drawn so far.

        Split out of _capture_and_annotate so it can run while the OCR thread
        is still busy; _capture_and_annotate then draws only the OCR boxes and
        encodes. Safe to call more than once — it no-ops once the canvas
        exists."""
        if self._annotate_canvas is not None:
            return
        canvas = self._make_annotation_canvas(screen)
        if canvas is None:
            return
        self._annotate_canvas = canvas
        self._draw_boxes(0)

    def _capture_and_annotate(self, screen, start_index=0):
        """Annotate the screenshot captured earlier in scan_elements, store as base64.

        `start_index` skips boxes already drawn by _begin_annotation."""
        if self._annotate_canvas is None:
            # Nothing was pre-drawn (annotation disabled mid-scan, or the
            # prep failed) — build the canvas now and draw everything.
            canvas = self._make_annotation_canvas(screen)
            if canvas is None:
                self._annotated_image_base64 = None
                return
            self._annotate_canvas = canvas
            start_index = 0
        self._draw_boxes(start_index)
        self._encode_annotation()

    def _make_annotation_canvas(self, screen):
        """Return (image, draw, draw_scale, origin) for annotation, or None."""
        # Reuse the single frame captured (after prepare_scan settled) so OCR boxes,
        # AX boxes, and the displayed screenshot are all consistent.
        if self._annotate_thread is not None:
            self._annotate_thread.join()
            self._annotate_thread = None

        if self._screenshot is not None and self._annotate_prep is not None:
            # Fast path: the worker already produced the shrunk RGB base.
            screenshot, shrink = self._annotate_prep
            scale = self._scale
            self._plain_screenshot = self._screenshot.copy()
        else:
            # Fallback — called outside scan_elements (fresh capture) or the
            # prep worker failed: do the downscale + conversion inline.
            if self._screenshot is not None:
                screenshot, scale = self._screenshot.copy(), self._scale
            else:
                screenshot, scale, _ = take_screenshot(screen)
            if screenshot is None:
                self._annotated_image_base64 = None
                self._plain_screenshot = None
                return

            self._plain_screenshot = screenshot.copy()
            src_w, src_h = screenshot.size
            shrink = llm_image_shrink(src_w, src_h)
            if shrink < 1.0:
                screenshot = screenshot.resize(
                    (max(1, int(src_w * shrink)), max(1, int(src_h * shrink))),
                    Image.Resampling.LANCZOS)
            if screenshot.mode in ('RGBA', 'LA', 'P'):
                rgb = Image.new('RGB', screenshot.size, (255, 255, 255))
                rgb.paste(screenshot, mask=screenshot.split()[-1] if screenshot.mode == 'RGBA' else None)
                screenshot = rgb
            elif screenshot.mode != 'RGB':
                screenshot = screenshot.convert('RGB')

        # CG points -> delivered pixels. Two factors fold together: the Retina
        # backing scale (points -> capture pixels) and the downscale above.
        return (screenshot, ImageDraw.Draw(screenshot), scale * shrink,
                (screen["x"], screen["y"]))

    def _draw_boxes(self, start_index):
        """Draw elements_to_draw[start_index:] onto the annotation canvas."""
        screenshot, draw, draw_scale, (ox, oy) = self._annotate_canvas
        font, stroke_w = _load_annotate_font()

        # Draw each bounding box with index labels
        for item in self.elements_to_draw[start_index:]:
            rect = item["rect"]
            index = str(item["index"])
            is_ocr = item.get("source") == "ocr"

            # Convert from CG coordinates to delivered-image pixel coordinates
            box = (
                int((rect.left - ox) * draw_scale),
                int((rect.top - oy) * draw_scale),
                int((rect.right - ox) * draw_scale),
                int((rect.bottom - oy) * draw_scale),
            )

            # OCR boxes get slight padding for breathing room around tight text.
            # In delivered pixels now, so it no longer scales with the display.
            if is_ocr:
                pad = 3
                box = (box[0] - pad, box[1] - pad, box[2] + pad, box[3] + pad)

            draw.rectangle(box, outline=BOX_COLOR, width=2)

            label = f"[{index}]"
            if is_ocr:
                # Only OCR labels need measuring (they sit ABOVE their box).
                # textbbox() rasterises the string to measure it, so calling it
                # per element doubled the glyph-rendering cost of the whole
                # annotation pass. Height depends only on digit count, so one
                # measurement per label length serves every label.
                text_height = _label_heights.get(len(label))
                if text_height is None:
                    if font:
                        bbox = draw.textbbox((0, 0), label, font=font,
                                             stroke_width=stroke_w)
                        text_height = bbox[3] - bbox[1]
                    else:
                        text_height = 15
                    _label_heights[len(label)] = text_height
                # OCR: position label above the box (text usually fills the box)
                text_x = box[0]
                text_y = box[1] - text_height - 2
            else:
                # Position label at top-left inside box
                text_x = box[0] + 4
                text_y = box[1] + 3

            tile = _label_tile(label, font, stroke_w)
            # Keep the whole label on the canvas: PIL silently clips a paste
            # past an edge, so a label near the right or bottom edge (or an
            # OCR label above a box at the very top) would lose part of its
            # number. Clamps the ink, not the tile, so a label that already
            # fits never moves.
            ink_l, ink_t, ink_r, ink_b = _label_inks[label]
            text_x = max(-ink_l, min(text_x, screenshot.width - ink_r))
            text_y = max(-ink_t, min(text_y, screenshot.height - ink_b))
            screenshot.paste(tile, (text_x, text_y), tile)

    def _encode_annotation(self):
        """Encode the finished canvas as the base64 JPEG payload."""
        screenshot = self._annotate_canvas[0]

        # Single encode — these exact bytes are the LLM payload. 4:4:4 chroma
        # (subsampling=0) keeps the annotation digits crisp; see the
        # LLM_IMAGE_* constants for why JPEG replaced lossless PNG here.
        # Resize and RGB conversion both happened before annotating.
        buffered = io.BytesIO()
        screenshot.save(buffered, format=LLM_IMAGE_FORMAT,
                        quality=LLM_IMAGE_QUALITY,
                        subsampling=LLM_IMAGE_SUBSAMPLING)
        annotated_image_bytes = buffered.getvalue()
        self._annotated_image_base64 = base64.b64encode(annotated_image_bytes).decode('utf-8')

        # Save debug files — the SAME bytes that go to the LLM, so the dump is
        # byte-identical to the payload by construction and costs no re-encode.
        if DEBUG:
            debug_dir = f"debug/iteration_{self._debug_iteration}"
            os.makedirs(debug_dir, exist_ok=True)
            with open(f"{debug_dir}/annotated_screenshot.jpg", "wb") as f:
                f.write(annotated_image_bytes)

        # Send to frontend
        if FRONTEND and self.frontend_callback:
            if DEBUG:
                self.frontend_callback(self._annotated_image_base64)
            else:
                # Send plain screenshot for production frontend. This is for
                # human eyes, not the model, so it keeps its own settings and
                # does NOT follow the LLM payload caps above.
                max_dimension = FRONTEND_IMAGE_MAX_DIMENSION
                plain = self._plain_screenshot
                w, h = plain.size
                if w > max_dimension or h > max_dimension:
                    if w > h:
                        nw = max_dimension
                        nh = int(h * (max_dimension / w))
                    else:
                        nh = max_dimension
                        nw = int(w * (max_dimension / h))
                    plain = plain.resize((nw, nh), Image.Resampling.LANCZOS)

                if plain.mode in ('RGBA', 'LA', 'P'):
                    rgb_plain = Image.new('RGB', plain.size, (255, 255, 255))
                    rgb_plain.paste(plain, mask=plain.split()[-1] if plain.mode == 'RGBA' else None)
                    plain = rgb_plain
                elif plain.mode != 'RGB':
                    plain = plain.convert('RGB')

                buf = io.BytesIO()
                # optimize=True dropped: it is lossless (Huffman tables only),
                # so the preview is pixel-identical, just encoded faster.
                plain.save(buf, format="JPEG", quality=FRONTEND_IMAGE_QUALITY)
                self.frontend_callback(base64.b64encode(buf.getvalue()).decode('utf-8'))

    def get_scan_data(self):
        """Get scan data for use by AgentService.
        
        Returns:
            tuple: (element_tree_text, annotated_image_base64, uac_detected)
                   uac_detected is always False on macOS (no UAC).
        """
        element_tree_text = ""

        # Write menu bar section
        if self.menu_bar_tree:
            element_tree_text += "<menu_bar>\n"
            element_tree_text += self._get_tree_text_recursive(self.menu_bar_tree, 1)
            element_tree_text += "</menu_bar>\n\n"

        # Write top layer
        element_tree_text += "<top_layer>\n"
        if self.top_layer_info:
            layer_name = _xml_escape(self.top_layer_info["name"])
            layer_type = self.top_layer_info["type"]
            element_tree_text += f'  <application name="{layer_name}" type="{layer_type}" />\n'
        else:
            element_tree_text += '  <application name="Desktop" type="app" />\n'
        element_tree_text += self._get_tree_text_recursive(self.element_tree, 1)
        element_tree_text += "</top_layer>\n"

        # Get annotated image
        annotated_image_base64 = getattr(self, '_annotated_image_base64', None)

        return element_tree_text, annotated_image_base64, False  # uac_detected = False

    def _get_tree_text_recursive(self, tree_list, depth):
        """Generate tree text recursively — matches Windows format."""
        result = ""
        indent = "  " * depth

        for item in tree_list:
            # OCR_TEXT elements use a distinct format (text the AX tree missed)
            if item.get("source") == "ocr":
                text = _xml_escape(item['name'])
                result += f'{indent}[{item["index"]}]<Line="{text}", type="OCR_TEXT", active="True", visibility="full" />\n'
                if item.get("children"):
                    result += self._get_tree_text_recursive(item["children"], depth + 1)
                continue

            name = _xml_escape(item['name'])
            visibility = item.get('visibility', 'full')

            # Get clipped_by — only include if visibility is not full
            clipped_by = item.get('clipped_by', None)
            clipped_by_attr = ""
            if clipped_by and visibility != "full":
                clipped_by_attr = f', clipped_by="{_xml_escape(clipped_by)}"'

            # Get aria_role
            aria_role = _xml_escape(item.get('aria_role', ''))

            if item.get("value") and item["value"]:
                value = _xml_escape(item["value"])
                if aria_role:
                    result += f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n'
                else:
                    result += f'{indent}[{item["index"]}]<element name="{name}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n'
            else:
                if aria_role:
                    result += f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n'
                else:
                    result += f'{indent}[{item["index"]}]<element name="{name}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n'

            # Recurse into children
            if item.get("children"):
                result += self._get_tree_text_recursive(item["children"], depth + 1)

        return result

    def get_elements_mapping(self):
        """Get the elements mapping for controller.
        
        Returns:
            dict: mapping index (str) → element info dict
        """
        return self.elements_mapping

    def print_summary(self):
        """Print summary of found elements — silent (matches Windows behavior)."""
        pass

    def save_to_file(self):
        """Save element tree to file when DEBUG is True."""
        if DEBUG:
            debug_dir = f"debug/iteration_{self._debug_iteration}"
            os.makedirs(debug_dir, exist_ok=True)
            filename = f"{debug_dir}/tree.txt"
            with open(filename, "w", encoding="utf-8") as f:
                # Write menu bar
                if self.menu_bar_tree:
                    f.write("<menu_bar>\n")
                    self._write_tree_recursive(f, self.menu_bar_tree, 1)
                    f.write("</menu_bar>\n\n")

                # Write top layer
                f.write("<top_layer>\n")
                if self.top_layer_info:
                    layer_name = _xml_escape(self.top_layer_info["name"])
                    layer_type = self.top_layer_info["type"]
                    f.write(f'  <application name="{layer_name}" type="{layer_type}" />\n')
                else:
                    f.write('  <application name="Desktop" type="app" />\n')
                self._write_tree_recursive(f, self.element_tree, 1)
                f.write("</top_layer>\n")

    def _write_tree_recursive(self, file, tree_list, depth):
        """Write tree recursively to file — same format as get_tree_text."""
        indent = "  " * depth

        for item in tree_list:
            # OCR_TEXT elements use a distinct format (text the AX tree missed)
            if item.get("source") == "ocr":
                text = _xml_escape(item['name'])
                file.write(f'{indent}[{item["index"]}]<Line="{text}", type="OCR_TEXT", active="True", visibility="full" />\n')
                if item.get("children"):
                    self._write_tree_recursive(file, item["children"], depth + 1)
                continue

            name = _xml_escape(item['name'])
            visibility = item.get('visibility', 'full')

            clipped_by = item.get('clipped_by', None)
            clipped_by_attr = ""
            if clipped_by and visibility != "full":
                clipped_by_attr = f', clipped_by="{_xml_escape(clipped_by)}"'

            aria_role = _xml_escape(item.get('aria_role', ''))

            if item.get("value") and item["value"]:
                value = _xml_escape(item["value"])
                if aria_role:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n')
                else:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n')
            else:
                if aria_role:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n')
                else:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", type="{item["type"]}", active="{item["active"]}", visibility="{visibility}"{clipped_by_attr} />\n')

            if item.get("children"):
                self._write_tree_recursive(file, item["children"], depth + 1)


# ========== MAIN PROGRAM ==========

def main():
    print("macOS UI Element Scanner")
    print(f"DEBUG = {DEBUG}")
    print(f"SCREENSHOT = {SCREENSHOT}")

    if not AXIsProcessTrusted():
        print("\nAccessibility permission required.")
        print("Grant in: System Settings > Privacy & Security > Accessibility")
        sys.exit(1)

    # Create scanner with configuration
    scanner = UIElementScanner(ELEMENT_CONFIG)

    # Countdown
    for i in range(5, 0, -1):
        print(f"  Scanning in {i}...")
        time.sleep(1)

    print("Scanning now!\n")
    scanner.scan_elements()

    # Print results
    element_tree_text, annotated_image_base64, _ = scanner.get_scan_data()
    mapping = scanner.get_elements_mapping()

    print(f"Application: {scanner.application_name}")
    print(f"Elements found: {len(mapping)}")
    print(f"Image captured: {annotated_image_base64 is not None}")

    if DEBUG:
        scanner.save_to_file()
        print("\nElement tree text:")
        print(element_tree_text)
        print("Scan complete. Check debug/ for files.")
    else:
        print("Scan complete. Data ready for LLM.")


if __name__ == "__main__":
    main()