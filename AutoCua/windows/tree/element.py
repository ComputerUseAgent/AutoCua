import warnings
# pywinauto 0.6.9's module docstring contains an invalid \; escape
# (keyboard.py:105), which Python 3.12 raises as a SyntaxWarning the first time
# the module is byte-compiled. Upstream bug, cosmetic, and 0.6.9 is the newest
# release, so there is nothing to upgrade to.
#
# The .* wrapping is load-bearing. For a COMPILE-time warning the string this
# filter matches against is the full path of the file being compiled, minus
# .py — "C:/.../site-packages/pywinauto/keyboard" — and warnings compiles the
# pattern with re.match, which anchors at the start. A bare "pywinauto" never
# matches it, so the plain-looking version of this line silently does nothing.
warnings.filterwarnings("ignore", category=SyntaxWarning, module=r".*pywinauto.*")
from pywinauto import Desktop, Application
from pywinauto.controls.uiawrapper import UIAWrapper
from pywinauto import uia_element_info
import comtypes.client
from .ocr_detection import OCRScanner
import time
from win32api import RGB
import win32gui
import win32con
import win32api
from PIL import Image, ImageGrab, ImageDraw, ImageFont
import numpy as np
import os
import io
import base64
import threading
from collections import namedtuple
import ctypes
from ctypes import wintypes

# ========== CONFIGURATION ==========
# Toggle switch for screenshot capture
SCREENSHOT = True   # Set to False to only generate ui_elements.txt without screenshot
DEBUG = False             # Set to True to save files to debug folders, False for direct LLM only
FRONTEND = True    # Set to True when running from app.py to send annotated images to frontend

# Final geometry/encoding of the annotated screenshot. The image is encoded ONCE,
# here, at exactly these settings - the resulting bytes are what the LLM receives
# and what DEBUG writes to disk, so the debug dump is always byte-identical to the
# payload. Callers must NOT re-encode.
#
# Two independent caps, both orientation-agnostic:
#   MAX_EDGE   - the long side, whichever it is. Vision models resize anything
#                larger on their own side, which would put the annotations through
#                THEIR resampler and undo the crisp-label work below.
#   MAX_PIXELS - a total-area budget. Models also cap images by token count
#                (~750px per image token), and that bites before the edge cap on
#                squarish aspect ratios. Sized to stay comfortably under it.
# Raising these costs image tokens on every step - they bill by dimensions, so
# this is a deliberate readability-vs-cost tradeoff, not a free win.
LLM_IMAGE_MAX_EDGE = 2300
LLM_IMAGE_MAX_PIXELS = 3_300_000

# JPEG, not PNG — matching mac/tree/element.py. The lossless PNG payload ran
# multiple MB of base64 per step, and since the screenshot rides the live
# (never-cached) message, that upload was paid in full on EVERY step — it
# dominated LLM latency. Image tokens bill by DIMENSIONS not bytes, so the
# model reads the same tokens either way; the bytes only cost upload time. The
# reason PNG was chosen — chroma subsampling storing colour at half resolution
# and smearing the thin magenta labels — is answered by SUBSAMPLING = 0
# (4:4:4) below: full-resolution chroma keeps the annotation digits crisp at
# roughly a tenth of the bytes.
LLM_IMAGE_FORMAT = "JPEG"
LLM_IMAGE_QUALITY = 85
# 0 = 4:4:4 (no chroma subsampling). This is the annotation-preserving half of
# the JPEG switch; PIL's default (4:2:0) is exactly what smears magenta digits.
LLM_IMAGE_SUBSAMPLING = 0
# Providers hardcode this in their request builders (grep: image/jpeg). Importing
# it there would drag pywinauto/win32 into the provider modules.
LLM_IMAGE_MEDIA_TYPE = "image/jpeg"

# Index-label styling. Drawn AFTER the downscale, so these are literal delivered
# pixels. The stroke is a thin rim hugging the glyphs, NOT a filled chip - it
# gives contrast on light, dark, and magenta-ish UI without hiding what's under it.
LABEL_FONT_SIZE = 13
LABEL_STROKE = 2
LABEL_STROKE_COLOR = (0, 0, 0)

# The plain screenshot mirrored to the frontend is a human-facing preview, not an
# LLM payload, so it keeps its original higher-fidelity settings.
FRONTEND_IMAGE_MAX_DIMENSION = 1920
FRONTEND_IMAGE_QUALITY = 100

# Define Rect namedtuple once for reuse throughout module
Rect = namedtuple('Rect', ['left', 'top', 'right', 'bottom'])

# ========== PRIMARY MONITOR DETECTION ==========
user32 = ctypes.windll.user32

MONITOR_DEFAULTTONULL = 0
MONITORINFOF_PRIMARY = 1

class MONITORINFO(ctypes.Structure):
    _fields_ = [
        ("cbSize", wintypes.DWORD),
        ("rcMonitor", wintypes.RECT),
        ("rcWork", wintypes.RECT),
        ("dwFlags", wintypes.DWORD),
    ]

def is_on_primary_monitor(hwnd):
    """Check if window is on the primary monitor"""
    try:
        hmonitor = user32.MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL)
        if not hmonitor:
            return False
        
        mi = MONITORINFO()
        mi.cbSize = ctypes.sizeof(MONITORINFO)
        if not user32.GetMonitorInfoW(hmonitor, ctypes.byref(mi)):
            return False
        
        return (mi.dwFlags & MONITORINFOF_PRIMARY) != 0
    except:
        return False

# DWM cloaked check for UWP apps
dwmapi = ctypes.windll.dwmapi
DWMWA_CLOAKED = 14

def is_window_cloaked(hwnd):
    """Check if window is cloaked (hidden by DWM) - common for UWP apps like Settings"""
    try:
        cloaked = ctypes.c_int(0)
        result = dwmapi.DwmGetWindowAttribute(
            hwnd, 
            DWMWA_CLOAKED, 
            ctypes.byref(cloaked), 
            ctypes.sizeof(cloaked)
        )
        return result == 0 and cloaked.value != 0
    except:
        return False

def _xml_escape(text):
    """Escape special characters for XML attributes"""
    if not text:
        return ""
    return (text
            .replace('&', '&amp;')
            .replace('<', '&lt;')
            .replace('>', '&gt;')
            .replace('"', '&quot;'))

# Load raw UIA COM interface for parallel scanning
UIAutomationClient = comtypes.client.GetModule("UIAutomationCore.dll")
_uia = comtypes.client.CreateObject(
    "{ff48dba4-60ef-4201-aa87-54103eef594e}",
    interface=UIAutomationClient.IUIAutomation
)

# UIA control type id -> friendly name, matching what pywinauto's
# element_info.control_type returns. Used by the cached fast path, which
# reads raw ids instead of going through pywinauto's registry.
_UIA_ID_TO_NAME = {
    50000: "Button", 50001: "Calendar", 50002: "CheckBox", 50003: "ComboBox",
    50004: "Edit", 50005: "Hyperlink", 50006: "Image", 50007: "ListItem",
    50008: "List", 50009: "Menu", 50010: "MenuBar", 50011: "MenuItem",
    50012: "ProgressBar", 50013: "RadioButton", 50014: "ScrollBar",
    50015: "Slider", 50016: "Spinner", 50017: "StatusBar", 50018: "Tab",
    50019: "TabItem", 50020: "Text", 50021: "ToolBar", 50022: "ToolTip",
    50023: "Tree", 50024: "TreeItem", 50025: "Custom", 50026: "Group",
    50027: "Thumb", 50028: "DataGrid", 50029: "DataItem", 50030: "Document",
    50031: "SplitButton", 50032: "Window", 50033: "Pane", 50034: "Header",
    50035: "HeaderItem", 50036: "Table", 50037: "TitleBar", 50038: "Separator",
    50039: "SemanticZoom", 50040: "AppBar",
}

_TREE_SCOPE_SUBTREE = getattr(UIAutomationClient, "TreeScope_Subtree", 7)
_TREE_SCOPE_CHILDREN = getattr(UIAutomationClient, "TreeScope_Children", 2)

_scan_cache_request = None

def _get_scan_cache_request():
    """CacheRequest listing every property the scan reads. One
    BuildUpdatedCache call with this request fetches the whole subtree in a
    single cross-process round trip; all subsequent reads are in-process."""
    global _scan_cache_request
    if _scan_cache_request is None:
        cache = _uia.CreateCacheRequest()
        for prop_id in (
            30000,  # RuntimeId
            30001,  # BoundingRectangle
            30003,  # ControlType
            30005,  # Name
            30009,  # IsKeyboardFocusable
            30010,  # IsEnabled
            30011,  # AutomationId
            30012,  # ClassName
            30016,  # IsControlElement
            30022,  # IsOffscreen
            30034,  # IsScrollPatternAvailable
            30040,  # IsTextPatternAvailable
            30041,  # IsTogglePatternAvailable
            30043,  # IsValuePatternAvailable
            30045,  # Value.Value
            30086,  # Toggle.ToggleState
            30094,  # LegacyIAccessible.Description
            30101,  # AriaRole
        ):
            cache.AddProperty(prop_id)
        cache.TreeScope = _TREE_SCOPE_SUBTREE
        # Raw view - parity with pywinauto's children(), which enumerates
        # with a true condition.
        cache.TreeFilter = _uia.CreateTrueCondition()
        _scan_cache_request = cache
    return _scan_cache_request

# The agent's own edge glow (AutoCua/utils/agent_glow.py): four click-through
# layered strips over the screen edges, all of this one window class. They hide
# nothing and are no app, popup or overlay, so every window scan below skips
# them — counted as windows on macOS, they dropped every element within ~58 px
# of a screen edge and got walked as a dialog.
_AGENT_GLOW_CLASS = "LavenderScreenGlow"

# ========== LAYER DETECTION FUNCTIONS ==========
def detect_windows_overlay():
    """Check root UIA children for Windows overlays (Start, Search, Notifications, etc.)"""
    
    overlay_names = {
        "start": "Start Menu",
        "search": "Windows Search",
        "notification center": "Notification Center",
        "notifications": "Notifications",
        "action center": "Action Center",
        "quick settings": "Quick Settings",
        "windows shell experience host": "Windows Shell",
    }
    
    try:
        root = _uia.GetRootElement()
        walker = _uia.ControlViewWalker
        child = walker.GetFirstChildElement(root)
        
        while child:
            try:
                name = child.CurrentName or ""
                class_name = child.CurrentClassName or ""
                
                if class_name == "Windows.UI.Core.CoreWindow":
                    name_lower = name.lower().strip()
                    
                    if name_lower in overlay_names:
                        return (overlay_names[name_lower], "Windows.UI.Core.CoreWindow", child)
                
            except:
                pass
            
            child = walker.GetNextSiblingElement(child)
    except:
        pass
    
    return None


def detect_system_tray_overflow():
    """Check if system tray overflow window is visible"""
    
    overflow_hwnd = None
    
    def enum_callback(hwnd, _):
        nonlocal overflow_hwnd
        
        if not win32gui.IsWindowVisible(hwnd):
            return True
        
        try:
            class_name = win32gui.GetClassName(hwnd)
        except:
            return True
        
        if class_name == _AGENT_GLOW_CLASS:
            return True

        if class_name in {"NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland"}:
            try:
                left, top, right, bottom = win32gui.GetWindowRect(hwnd)
                width = right - left
                height = bottom - top
                
                if width > 50 and height > 50 and left > -30000 and top > -30000:
                    overflow_hwnd = (hwnd, "System Tray Overflow", class_name)
                    return False
            except:
                pass
        
        return True
    
    win32gui.EnumWindows(enum_callback, None)
    
    return overflow_hwnd


def detect_popup():
    """Check if a popup/context menu is visible"""
    
    popup_classes = {
        "Microsoft.UI.Content.PopupWindowSiteBridge",
        "Xaml_WindowedPopupClass",
    }
    
    popup_hwnd = None
    
    def enum_callback(hwnd, _):
        nonlocal popup_hwnd
        
        if not win32gui.IsWindowVisible(hwnd):
            return True
        
        try:
            class_name = win32gui.GetClassName(hwnd)
        except:
            return True
        
        try:
            window_text = win32gui.GetWindowText(hwnd)
        except:
            window_text = ""
        
        if class_name == _AGENT_GLOW_CLASS:
            return True

        if class_name in popup_classes:
            try:
                left, top, right, bottom = win32gui.GetWindowRect(hwnd)
                width = right - left
                height = bottom - top
                
                if width > 20 and height > 20 and left > -30000 and top > -30000:
                    popup_hwnd = (hwnd, window_text or "Context Menu", class_name)
                    return False
            except:
                pass
        
        if window_text.lower() in {"pop-uphost", "popuphost"}:
            try:
                left, top, right, bottom = win32gui.GetWindowRect(hwnd)
                width = right - left
                height = bottom - top
                
                if width > 20 and height > 20 and left > -30000 and top > -30000:
                    popup_hwnd = (hwnd, "Context Menu", class_name)
                    return False
            except:
                pass
        
        return True
    
    win32gui.EnumWindows(enum_callback, None)
    
    return popup_hwnd


def get_topmost_app():
    """Find topmost regular application window by Z-order"""
    
    screen_width = win32api.GetSystemMetrics(0)
    screen_height = win32api.GetSystemMetrics(1)
    
    skip_classes = {
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "Progman",
        "WorkerW",
        "NotifyIconOverflowWindow",
        "TopLevelWindowForOverflowXamlIsland",
        "Windows.UI.Core.CoreWindow",
        "XamlExplorerHostIslandWindow",
        "CEF-OSC-WIDGET",
        "Microsoft.UI.Content.PopupWindowSiteBridge",
        "Xaml_WindowedPopupClass",
    }
    
    skip_names = {
        "nvidia geforce overlay",
        "rzmonitoreforegroundwindow",
        "pop-uphost",
        "popuphost",
    }
    
    topmost_app = None
    desktop_hwnd = None
    
    def enum_callback(hwnd, _):
        nonlocal topmost_app, desktop_hwnd
        
        if topmost_app is not None:
            return True
        
        if not win32gui.IsWindowVisible(hwnd):
            return True
        
        try:
            class_name = win32gui.GetClassName(hwnd)
        except:
            return True
        
        if class_name == _AGENT_GLOW_CLASS:
            return True

        if class_name in {"Progman", "WorkerW"}:
            if desktop_hwnd is None:
                desktop_hwnd = hwnd
            return True
        
        if class_name in skip_classes:
            return True
        
        try:
            window_text = win32gui.GetWindowText(hwnd)
        except:
            window_text = ""
        
        if window_text.lower() in skip_names:
            return True
        
        if not window_text.strip():
            return True
        
        try:
            placement = win32gui.GetWindowPlacement(hwnd)
            if placement[1] == win32con.SW_SHOWMINIMIZED:
                return True
        except:
            return True
        
        # Skip cloaked windows (UWP apps that are hidden but have WS_VISIBLE)
        if is_window_cloaked(hwnd):
            return True
        
        try:
            left, top, right, bottom = win32gui.GetWindowRect(hwnd)
            width = right - left
            height = bottom - top
        except:
            return True
        
        if width < 100 or height < 100:
            return True
        
        # Skip windows not on primary monitor
        if not is_on_primary_monitor(hwnd):
            return True
        
        if left <= -30000 or top <= -30000:
            return True
        if right <= 0 or bottom <= 0:
            return True
        if left >= screen_width or top >= screen_height:
            return True
        
        # Skip windows mostly on secondary monitor (less than 100px visible on main)
        if left < 0 and right < 100:
            return True
        if top < 0 and bottom < 100:
            return True
        
        topmost_app = (hwnd, window_text, class_name)
        return True
    
    win32gui.EnumWindows(enum_callback, None)
    
    if topmost_app:
        return topmost_app
    elif desktop_hwnd:
        return (desktop_hwnd, "Desktop", "Progman")
    
    return None


def get_topmost_layers():
    """Get top layer and second layer if overlay is active"""
    
    result = {
        "top_layer": None,
        "second_layer": None,
        "overlay_popup": None
    }

    overlay = detect_windows_overlay()
    tray_overflow = detect_system_tray_overflow()
    popup = detect_popup()
    app = get_topmost_app()

    if overlay:
        result["top_layer"] = {"type": "overlay", "name": overlay[0], "class": overlay[1], "element": overlay[2], "hwnd": None}
        if popup:
            # A flyout open ON TOP of the overlay (e.g. the Start menu power
            # menu with Sleep/Shut down/Restart) is a separate popup window -
            # without this it would be discarded and only OCR would see it.
            hwnd, name, class_name = popup
            result["overlay_popup"] = {"type": "popup", "name": name, "class": class_name, "hwnd": hwnd}
        if app:
            result["second_layer"] = {"type": "app", "name": app[1], "class": app[2], "hwnd": app[0]}
    elif tray_overflow:
        hwnd, name, class_name = tray_overflow
        result["top_layer"] = {"type": "tray_overflow", "name": name, "class": class_name, "hwnd": hwnd}
        if app:
            result["second_layer"] = {"type": "app", "name": app[1], "class": app[2], "hwnd": app[0]}
    elif popup:
        hwnd, name, class_name = popup
        result["top_layer"] = {"type": "popup", "name": name, "class": class_name, "hwnd": hwnd}
        if app:
            result["second_layer"] = {"type": "app", "name": app[1], "class": app[2], "hwnd": app[0]}
    else:
        if app:
            result["top_layer"] = {"type": "app", "name": app[1], "class": app[2], "hwnd": app[0]}
    
    return result


# ========== TASKBAR VISIBILITY ==========
# Shell_TrayWnd stays IsWindowVisible, uncloaked and fully UIA-walkable while a
# full-screen window paints over it (the shell only drops the taskbar's
# WS_EX_TOPMOST and sinks it in the z-order) and while auto-hide has slid it off
# the screen - so the taskbar walk kept handing the model buttons that are not
# in the screenshot. detect_taskbar_visibility() decides once per scan whether
# the bar is really on screen: how much of its on-screen strip do the windows
# AHEAD of it in z-order paint over?
#
# One coordinate space throughout: GetWindowRect / GetMonitorInfo, both in the
# calling process's own DPI context. Importing pywinauto (and pyautogui) makes
# the process per-monitor DPI-aware, so inside the app these are physical
# pixels; a probe that never imports them sees virtualized ones instead (the
# Cursor / y=1019 numbers below come from such a probe on a 150% display). The
# verdict only ever compares rects from the same two calls, so it holds in
# either space. Never mix in UIA BoundingRectangles,
# DWMWA_EXTENDED_FRAME_BOUNDS or the ABM_GETTASKBARPOS rect, and never convert
# between spaces or change DPI awareness here.
#
# Read-only window-manager queries only (no messages to other windows, no UIA),
# split so the rules are testable without a desktop:
#   _collect_taskbar_window_facts() - impure snapshot
#   decide_taskbar_visibility()     - pure decision over that snapshot
_TASKBAR_COVERED_FRACTION = 0.90      # strip coverage at/above this = hidden
_TASKBAR_MIN_ONSCREEN_FRACTION = 0.5  # less of the bar's thickness on screen = slid away
_TASKBAR_INVISIBLE_ALPHA = 8          # constant layered alpha at/below this paints nothing
_TASKBAR_MIN_OVERLAP_PX = 3           # thinner overlaps are borders or rounding, not covers
# Cap for a maximized window's invisible resize border. The border is ~7px at
# 100% and scales with the display (~11px at 150%, ~21px at 300%) in the
# physical pixels the app works in, so the cap leaves room for high scaling.
_TASKBAR_MAX_FRAME_PX = 32

# Windows that can sit ahead of the taskbar in z-order yet never hide it: the
# agent's own glow; the desktop; the taskbar's own satellites (tooltips,
# thumbnails, tray overflow, input switcher, edge-swipe strips); shell flyout
# and switcher hosts whose rects end at the taskbar edge or are transparent
# outside their content (Start, Search, Input Experience and the Alt-Tab / Snap
# host are full-monitor rects); XAML context-menu popups.
_TASKBAR_NON_OCCLUDER_CLASSES = {
    _AGENT_GLOW_CLASS,
    "Progman", "WorkerW", "Shell_SecondaryTrayWnd",
    "tooltips_class32", "TaskListThumbnailWnd",
    "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland",
    "Shell_InputSwitchTopLevelWindow", "EdgeUiInputWndClass", "EdgeUiInputTopWndClass",
    "ThumbnailDeviceHelperWnd", "ForegroundStaging",
    "Windows.UI.Core.CoreWindow", "ControlCenterWindow", "WindowsDashboard",
    "XamlExplorerHostIslandWindow", "MultitaskingViewFrame",
    "Xaml_WindowedPopupClass", "Microsoft.UI.Content.PopupWindowSiteBridge",
}

# What the decision needs to know about one top-level window.
#   lwa          - (alpha, flags) from GetLayeredWindowAttributes; None when the
#                  window is not layered or the call failed (UpdateLayeredWindow
#                  window: per-pixel alpha, unreadable)
#   monitor_rect - rcMonitor of the window's own monitor, read only for
#                  maximized windows (to strip their invisible resize border)
_WindowFacts = namedtuple('_WindowFacts', [
    'class_name', 'rect', 'visible', 'cloaked', 'iconic', 'style', 'ex_style',
    'lwa', 'monitor_rect'])


def _intersect_rects(a, b):
    """Overlap of two Rects, or None when they do not overlap."""
    left, top = max(a.left, b.left), max(a.top, b.top)
    right, bottom = min(a.right, b.right), min(a.bottom, b.bottom)
    if right <= left or bottom <= top:
        return None
    return Rect(left, top, right, bottom)


def _rect_union_area(rects):
    """Exact area of the union of Rects: coordinate compression on x, merged
    y-spans per column. Only a handful of rects ever reach it."""
    xs = sorted({x for r in rects for x in (r.left, r.right)})
    area = 0
    for x0, x1 in zip(xs, xs[1:]):
        spans = sorted((r.top, r.bottom) for r in rects if r.left <= x0 and r.right >= x1)
        covered, span_top, span_bottom = 0, None, None
        for top, bottom in spans:
            if span_bottom is None or top > span_bottom:
                if span_bottom is not None:
                    covered += span_bottom - span_top
                span_top, span_bottom = top, bottom
            else:
                span_bottom = max(span_bottom, bottom)
        if span_bottom is not None:
            covered += span_bottom - span_top
        area += covered * (x1 - x0)
    return area


def _taskbar_occluder_rect(win):
    """The part of the screen a window ahead of the taskbar really paints over,
    or None when it hides nothing. Unknown opacity counts as see-through."""
    if not win.visible or win.cloaked or win.iconic:
        return None
    if win.class_name in _TASKBAR_NON_OCCLUDER_CLASSES:
        return None
    rect = win.rect
    if rect.left <= -30000 or rect.top <= -30000:  # minimized
        return None
    if rect.right - rect.left <= 2 or rect.bottom - rect.top <= 2:
        return None
    if win.ex_style & win32con.WS_EX_LAYERED:
        # Click-through layered overlays (the agent glow, a game's
        # D3DProxyWindow) are see-through wherever they are not drawn.
        if win.ex_style & win32con.WS_EX_TRANSPARENT:
            return None
        if win.lwa is None:
            return None
        alpha, flags = win.lwa
        if flags & win32con.LWA_COLORKEY:
            return None
        # Idle overlays park an invisible full-screen window (NVIDIA
        # CEF-OSC-WIDGET: alpha 0 over 99.9% of the screen).
        if flags & win32con.LWA_ALPHA and alpha <= _TASKBAR_INVISIBLE_ALPHA:
            return None
    # A maximized framed window's rect overhangs its WORK AREA by the invisible
    # resize border on every side - so it overhangs the monitor itself only on
    # the sides without a taskbar (Cursor: [-7,-7,1714,1026] on a 1707x1067
    # monitor whose bar starts at y=1019). Measure the border on those sides
    # and trim it from all four, which also removes the strip that reaches into
    # the bar. A borderless window left WS_MAXIMIZE at exactly the monitor rect
    # has no overhang and keeps its rect.
    if win.style & win32con.WS_MAXIMIZE and win.monitor_rect is not None:
        mon = win.monitor_rect
        overhang = [d for d in (mon.left - rect.left, mon.top - rect.top,
                                rect.right - mon.right, rect.bottom - mon.bottom) if d > 0]
        if overhang:
            border = min(min(overhang), _TASKBAR_MAX_FRAME_PX)
            rect = Rect(rect.left + border, rect.top + border,
                        rect.right - border, rect.bottom - border)
    return rect


def decide_taskbar_visibility(tray, screen, above):
    """Pure decision: is the taskbar on screen?

    tray   - _WindowFacts of Shell_TrayWnd, or None when there is none
    screen - Rect of the primary monitor, the area the screenshot captures
    above  - _WindowFacts of the visible windows ahead of the taskbar in
             z-order whose rects touch it
    Returns (visible, reason, covered_fraction). reason is what the element
    tree reports when the taskbar is left out.
    """
    if tray is None:
        return False, "not_running", 0.0
    if not tray.visible or tray.cloaked or tray.iconic:
        return False, "not_shown", 0.0
    rect = tray.rect
    width, height = rect.right - rect.left, rect.bottom - rect.top
    if width <= 0 or height <= 0:
        return False, "not_shown", 0.0
    # Auto-hide slides the bar past the screen edge leaving a 0-2px sliver; a
    # bar on another monitor is not in the screenshot at all. Measured across
    # the bar, so every docking edge works.
    strip = _intersect_rects(rect, screen)
    horizontal = width >= height
    thickness = height if horizontal else width
    onscreen = 0
    if strip is not None:
        onscreen = (strip.bottom - strip.top) if horizontal else (strip.right - strip.left)
    if onscreen < thickness * _TASKBAR_MIN_ONSCREEN_FRACTION:
        return False, "off_screen", 0.0
    covers = []
    for win in above:
        cover = _taskbar_occluder_rect(win)
        if cover is None:
            continue
        cover = _intersect_rects(cover, strip)
        if (cover is None or cover.right - cover.left < _TASKBAR_MIN_OVERLAP_PX
                or cover.bottom - cover.top < _TASKBAR_MIN_OVERLAP_PX):
            continue
        covers.append(cover)
    strip_area = (strip.right - strip.left) * (strip.bottom - strip.top)
    covered = _rect_union_area(covers) / strip_area if covers else 0.0
    if covered >= _TASKBAR_COVERED_FRACTION:
        return False, "covered", covered
    return True, None, covered


def _collect_taskbar_window_facts():
    """Impure half of the taskbar gate: (tray facts or None, primary monitor
    rect, facts of the visible windows ahead of the taskbar that touch it)."""
    screen = Rect(*win32api.GetMonitorInfo(
        win32api.MonitorFromPoint((0, 0), win32con.MONITOR_DEFAULTTOPRIMARY))["Monitor"])
    tray_hwnd = win32gui.FindWindow("Shell_TrayWnd", None)
    if not tray_hwnd:
        return None, screen, []
    tray_rect = Rect(*win32gui.GetWindowRect(tray_hwnd))
    tray = _WindowFacts(
        "Shell_TrayWnd", tray_rect, bool(win32gui.IsWindowVisible(tray_hwnd)),
        is_window_cloaked(tray_hwnd), bool(win32gui.IsIconic(tray_hwnd)), 0, 0, None, None)

    # EnumWindows lists top-level windows in z-order, topmost first: everything
    # before the taskbar is ahead of it. Snapshot first, filter after - ending
    # the walk early (callback returning False) makes pywin32 raise whenever
    # GetLastError happens to be non-zero.
    hwnds = []

    def enum_callback(hwnd, _):
        hwnds.append(hwnd)
        return True

    win32gui.EnumWindows(enum_callback, None)
    if tray_hwnd not in hwnds:
        # Explorer restarted between FindWindow and the walk - let the caller fail open
        raise RuntimeError("Shell_TrayWnd is not in the z-order snapshot")

    above = []
    for hwnd in hwnds[:hwnds.index(tray_hwnd)]:
        try:
            if not win32gui.IsWindowVisible(hwnd):
                continue
            rect = Rect(*win32gui.GetWindowRect(hwnd))
            # Only windows touching the bar can hide it - drops nearly every
            # window before the queries below.
            if _intersect_rects(rect, tray_rect) is None:
                continue
            style = win32gui.GetWindowLong(hwnd, win32con.GWL_STYLE)
            ex_style = win32gui.GetWindowLong(hwnd, win32con.GWL_EXSTYLE)
            lwa = None
            if ex_style & win32con.WS_EX_LAYERED:
                try:
                    _, alpha, flags = win32gui.GetLayeredWindowAttributes(hwnd)
                    lwa = (alpha, flags)
                except Exception:
                    lwa = None  # UpdateLayeredWindow window
            monitor_rect = None
            if style & win32con.WS_MAXIMIZE:
                monitor_rect = Rect(*win32api.GetMonitorInfo(
                    win32api.MonitorFromWindow(hwnd, win32con.MONITOR_DEFAULTTONEAREST))["Monitor"])
            above.append(_WindowFacts(
                win32gui.GetClassName(hwnd), rect, True, is_window_cloaked(hwnd),
                bool(win32gui.IsIconic(hwnd)), style, ex_style, lwa, monitor_rect))
        except Exception:
            continue  # window closed mid-walk
    return tray, screen, above


def detect_taskbar_visibility():
    """(visible, reason, covered_fraction) for the taskbar the scanner walks.
    Fails OPEN: any detection error reports it visible - the behaviour from
    before this gate existed."""
    try:
        return decide_taskbar_visibility(*_collect_taskbar_window_facts())
    except Exception:
        return True, "detection_failed", None

ELEMENT_CONFIG = {
    "MenuItem": {
        "track": True,
        "uia_control_type": 50011,
        "win32_classes": [],
        "keyboard_focusable": True,
        "is_enabled_check": True,
        "fallback": ["name", "automation_id"]
    },
    "Menu": {
        "track": True,
        "uia_control_type": 50009,
        "win32_classes": [],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "Button": {
        "track": True,
        "uia_control_type": 50000,
        "win32_classes": ["Button"],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id", "legacy_description", "class_name"]
    },
    "TabItem": {
        "track": True,
        "uia_control_type": 50019,
        "win32_classes": ["SysTabControl32"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "TreeItem": {
        "track": True,
        "uia_control_type": 50024,
        "win32_classes": ["SysTreeView32"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "CheckBox": {
        "track": True,
        "uia_control_type": 50002,
        "win32_classes": ["Button"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "ListItem": {
        "track": True,
        "uia_control_type": 50007,
        "win32_classes": ["ListBox", "SysListView32"],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id"]
    },
    "Document": {
        "track": True,
        "uia_control_type": 50030,
        "win32_classes": ["RichEdit", "RichEdit20W", "RichEdit20A"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "ComboBox": {
        "track": True,
        "uia_control_type": 50003,
        "win32_classes": ["ComboBox"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id", "class_name"]
    },
    "RadioButton": {
        "track": True,
        "uia_control_type": 50013,
        "win32_classes": ["Button"],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id"]
    },
    "Edit": {
        "track": True,
        "uia_control_type": 50004,
        "win32_classes": ["Edit"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "Group": {
        "track": True,
        "uia_control_type": 50026,
        "win32_classes": ["Button"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "Hyperlink": {
        "track": True,
        "uia_control_type": 50005,
        "win32_classes": ["SysLink"],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id", "class_name"]
    },
    "Pane": {
        "track": True,
        "uia_control_type": 50033,
        "win32_classes": [],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "Image": {
        "track": True,
        "uia_control_type": 50006,
        "win32_classes": ["Static"],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id", "class_name"]
    },
    "SplitButton": {
        "track": True,
        "uia_control_type": 50031,
        "win32_classes": [],
        "keyboard_focusable": True,
        "fallback": ["name", "automation_id"]
    },
    "DataItem": {
        "track": True,
        "uia_control_type": 50029,
        "win32_classes": [],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id"]
    },
    "Text": {
        "track": True,
        "uia_control_type": 50020,
        "win32_classes": ["Static"],
        "keyboard_focusable": False,
        "fallback": ["name", "automation_id"]
    }
    # Add more element types here as needed
}

# Single magenta color for all elements in screenshot
BOX_COLOR = (255, 0, 255)  # Bright magenta for all boxes
NUMBER_COLOR = (255, 0, 255)  # Same magenta for numbers

# Browser configuration for loading detection
BROWSER_CONFIG = {
    "chrome": {
        "app_name_contains": "Google Chrome"
        # Chrome uses FullDescription check ("Stop loading this page" = loading) instead of template matching
    },
    "edge": {
        "app_name_contains": "Edge"
        # Edge uses FullDescription check ("Stop (Esc)" = loading) instead of template matching
    },
    "firefox": {
        "app_name_contains": "Firefox"
        # Firefox uses button name check ("Stop" = loading) instead of template matching
    },
    "opera": {
        "app_name_contains": "Opera"
        # Opera uses button name check ("Stop" = loading) instead of template matching
    },
    "brave": {
        "app_name_contains": "Brave"
        # Brave uses FullDescription check ("Stop loading this page" = loading) - same as Chrome
    }
}

# ========== SCANNER CLASS ==========
class UIElementScanner:
    def __init__(self, config, frontend_callback=None):
        self.config = config
        self.desktop = Desktop(backend="uia")
        self.found_elements = {}  # Dictionary to store elements by type
        self.element_tree = []  # Hierarchical tree structure for active window (UIA)
        self.win32_elements = []  # Flat list for Win32 scan results
        self.raw_uia_elements = []  # Flat list for raw UIA COM scan results
        self.taskbar_tree = []  # Hierarchical tree structure for taskbar
        self.element_index = 0  # Global index counter
        self.win32_thread = None  # Thread handle for parallel Win32 scan
        self.raw_uia_thread = None  # Thread handle for parallel raw UIA scan
        self.ocr_thread = None  # Thread handle for parallel OCR scan
        self.taskbar_thread = None  # Thread handle for parallel taskbar scan
        # Per-scan taskbar gate (detect_taskbar_visibility). Visible until a scan
        # decides otherwise, so get_scan_data / save_to_file called before any
        # scan behave exactly as before the gate. Every scan re-decides it -
        # including the ones controller/tool/screenshot.py runs, so its taskbar
        # lookups (Snipping Tool) see an empty taskbar while it is off screen.
        self.taskbar_visible = True
        self.taskbar_hidden_reason = None  # None while visible, else why it was left out
        self.ocr_scanner = None  # OCR scanner instance
        self.ocr_words = []  # Raw OCR results (filtered later)
        self.disabled_button_rects = []  # Rects of disabled buttons (block their OCR shadows)
        self.application_name = "Desktop"  # Store the application name
        self.elements_to_draw = []  # List for screenshot bounding boxes
        self.elements_mapping = {}  # Mapping of index to element for controller
        self.app_rect = None  # Store application window bounding box
        self.frontend_callback = frontend_callback  # Callback to send images to frontend
        
        # Layer-based scanning storage
        self.top_layer_tree = []  # Elements from top layer
        self.second_layer_tree = []  # Elements from second layer (if exists)
        self.top_layer_info = None  # Metadata about top layer
        self.second_layer_info = None  # Metadata about second layer
        self._is_browser = False  # Cached browser detection result
        
        # Build reverse lookup: uia_control_type -> element_type name
        self.uia_type_map = {}
        for element_type, cfg in config.items():
            if cfg.get("track") and "uia_control_type" in cfg:
                self.uia_type_map[cfg["uia_control_type"]] = element_type
        
        # Build reverse lookup: win32_class -> element_type name
        self.win32_class_map = {}
        for element_type, cfg in config.items():
            if cfg.get("track") and "win32_classes" in cfg:
                for win32_class in cfg["win32_classes"]:
                    self.win32_class_map[win32_class] = element_type
        
        # Initialize storage for each element type
        for element_type in config:
            if config[element_type]["track"]:
                self.found_elements[element_type] = []
        
        # Create debug directories if DEBUG is enabled
        if DEBUG:
            os.makedirs("debug/element", exist_ok=True)
            os.makedirs("debug/screenshot", exist_ok=True)

    def _is_browser_app(self):
        """Check if the current application is a web browser"""
        browser_keywords = ["chrome", "firefox", "edge", "opera", "brave", "safari", "vivaldi", "browser"]
        app_name_lower = self.application_name.lower()
        return any(browser in app_name_lower for browser in browser_keywords)
    
    def _detect_browser_popup(self, children):
        """Check if Document children contain an open dropdown/popup (ListItems, MenuItems, Menu)"""
        popup_types = {"ListItem", "MenuItem", "Menu"}
        popup_items = [c for c in children if c["type"] in popup_types]
        # 2+ popup-type direct children = likely an open dropdown
        return len(popup_items) >= 2
    
    def _detect_browser_type(self):
        """
        Detect which browser is active based on application name.
        Returns browser key from BROWSER_CONFIG or None if not found.
        """
        if not self._is_browser_app():
            return None
        
        # Check against each browser config
        for browser_key, config in BROWSER_CONFIG.items():
            if config["app_name_contains"] in self.application_name:
                return browser_key
        
        return None

    def _scan_layer(self, layer_info, target_tree, is_top_layer=True):
        """Scan a detected layer (overlay, popup, tray_overflow, or app)"""
        layer_type = layer_info["type"]
        layer_name = layer_info["name"]
        
        if layer_type == "overlay":
            # Windows overlay (Start Menu, Search, etc.) - use raw UIA element
            raw_element = layer_info.get("element")
            if raw_element:
                try:
                    # Wrap raw UIA element for pywinauto compatibility
                    child_info = uia_element_info.UIAElementInfo(raw_element)
                    wrapped_element = UIAWrapper(child_info)

                    # Get bounding rect for app_rect
                    try:
                        rect = raw_element.CurrentBoundingRectangle
                        self.app_rect = Rect(rect.left, rect.top, rect.right, rect.bottom)
                    except:
                        pass

                    if not self._scan_window_cached(raw_element, target_tree):
                        self._scan_element_recursive(wrapped_element, target_tree, 0, skip_visibility=False)
                except Exception as e:
                    pass

        elif layer_type in ["app", "tray_overflow", "popup"]:
            # Regular window - use hwnd
            hwnd = layer_info.get("hwnd")
            if hwnd:
                try:
                    app = Application(backend="uia").connect(handle=hwnd)
                    # Resolve the WindowSpecification to a wrapper ONCE -
                    # every attribute access on the spec re-runs the search.
                    window = app.window(handle=hwnd).wrapper_object()

                    self.app_rect = window.rectangle()

                    # Prime browser accessibility tree before scanning
                    # Chrome/Chromium lazy-loads UIA tree - a quick query wakes
                    # it up. Must be children(), one FindAll: descendants(depth=1)
                    # fetches the ENTIRE subtree and then walks parents per node
                    # to compute depth - measured 27s on a large page.
                    if is_top_layer and self._is_browser:
                        try:
                            _ = window.children()
                            time.sleep(0.1)
                        except:
                            pass

                    # Start parallel scans only for top layer
                    if is_top_layer:
                        # Start Win32 scan in parallel
                        self.win32_thread = threading.Thread(target=self._scan_with_win32_parallel, args=(layer_name,))
                        self.win32_thread.start()

                        # Start raw UIA scan in parallel
                        self.raw_uia_thread = threading.Thread(target=self._scan_with_raw_uia_parallel, args=(hwnd,))
                        self.raw_uia_thread.start()

                    scanned = False
                    try:
                        scanned = self._scan_window_cached(window.element_info.element, target_tree)
                    except Exception:
                        scanned = False
                    if not scanned:
                        self._scan_element_recursive(window, target_tree, 0, skip_visibility=False)

                except Exception as e:
                    # Fallback to desktop scan if app connection fails
                    if is_top_layer and layer_type == "app" and layer_info.get("class") == "Progman":
                        self._scan_desktop()
                        target_tree.extend(self.element_tree)
    
    def _iter_tree_buttons(self, tree_list):
        """Yield Button nodes from a scanned tree, document order, including
        browser layer splits."""
        for item in tree_list:
            if item.get("type") == "Button":
                yield item
            if item.get("browser_top_layer") is not None:
                yield from self._iter_tree_buttons(item["browser_top_layer"])
                yield from self._iter_tree_buttons(item["browser_second_layer"])
            elif item.get("children"):
                yield from self._iter_tree_buttons(item["children"])

    def _is_browser_loading_from_tree(self, browser_type):
        """Answer the browser-loading question from the tree just scanned
        instead of re-sweeping the window with a descendants() FindAll.
        Reads top_layer_tree only - the same window the live sweep targeted -
        so taskbar buttons and prior scans never influence the answer.
        Same signals as _is_browser_loading: Firefox/Opera toggle the toolbar
        button name to "Stop" while loading; Chrome/Brave/Edge keep the
        Reload/Refresh name and toggle FullDescription to a Stop message."""
        if browser_type is None or browser_type not in BROWSER_CONFIG:
            return False

        buttons = self._iter_tree_buttons(self.top_layer_tree)

        if browser_type in ("opera", "firefox"):
            return any((btn.get("name") or "") == "Stop" for btn in buttons)

        target_name = "Reload" if browser_type in ("chrome", "brave") else "Refresh"
        for btn in buttons:
            if (btn.get("name") or "") != target_name:
                continue
            element = btn.get("element")
            if element is None:
                continue
            try:
                # UIA_FullDescriptionPropertyId = 30159
                raw_element = element.element_info.element
                full_desc = str(raw_element.GetCurrentPropertyValue(30159) or "")
                return "Stop" in full_desc
            except Exception:
                continue  # same as the live sweep's per-button except
        return False  # Button not found - same answer the live sweep gives

    def _is_browser_loading(self, browser_type=None):
        """Check if browser is loading using dual template comparison"""
        # Auto-detect browser type if not provided
        if browser_type is None:
            browser_type = self._detect_browser_type()
        
        if browser_type is None or browser_type not in BROWSER_CONFIG:
            return False
        
        # Special handling for Opera and Firefox - just check button name
        # "Stop" button = loading, "Reload" button = loaded
        if browser_type in ["opera", "firefox"]:
            try:
                window = self.desktop.window(active_only=True)
                buttons = window.descendants(control_type="Button")
                for btn in buttons:
                    try:
                        name = btn.element_info.name or ""
                        if name == "Stop":
                            return True  # Loading
                    except:
                        continue
                return False  # Not loading (Reload button present or neither found)
            except:
                return False
        
        # Special handling for Chrome and Brave - check FullDescription of Reload button
        # "Stop loading this page" = loading, "Reload this page" = loaded
        if browser_type in ["chrome", "brave"]:
            try:
                window = self.desktop.window(active_only=True)
                buttons = window.descendants(control_type="Button")
                for btn in buttons:
                    try:
                        name = btn.element_info.name or ""
                        if name == "Reload":
                            # Get FullDescription property (UIA_FullDescriptionPropertyId = 30159)
                            raw_element = btn.element_info.element
                            full_desc = str(raw_element.GetCurrentPropertyValue(30159) or "")
                            if "Stop" in full_desc:
                                return True  # Loading
                            return False  # Loaded
                    except:
                        continue
                return False  # Button not found
            except:
                return False
        
        # Special handling for Edge - check FullDescription of Refresh button
        # "Stop (Esc)" = loading, "Refresh (Ctrl+R)" = loaded
        if browser_type == "edge":
            try:
                window = self.desktop.window(active_only=True)
                buttons = window.descendants(control_type="Button")
                for btn in buttons:
                    try:
                        name = btn.element_info.name or ""
                        if name == "Refresh":
                            # Get FullDescription property (UIA_FullDescriptionPropertyId = 30159)
                            raw_element = btn.element_info.element
                            full_desc = str(raw_element.GetCurrentPropertyValue(30159) or "")
                            if "Stop" in full_desc:
                                return True  # Loading
                            return False  # Loaded
                    except:
                        continue
                return False  # Button not found
            except:
                return False

        return False

    def _is_partially_visible(self, rect, app_rect):
        """
        Check if element rectangle extends beyond application window bounds
        Args:
            rect: Element's rectangle
            app_rect: Application window's rectangle
        Returns: 'full' if fully within app bounds, 'partial' if extends beyond
        """
        # If no app_rect provided, fall back to screen bounds check
        if app_rect is None:
            screen_width = win32api.GetSystemMetrics(0)
            screen_height = win32api.GetSystemMetrics(1)
            # Use screen as the boundary
            if (rect.left < 0 or rect.top < 0 or 
                rect.right > screen_width or rect.bottom > screen_height):
                return 'partial'
            return 'full'
        
        # Check if element extends beyond application window bounds
        if (rect.left < app_rect.left or 
            rect.top < app_rect.top or 
            rect.right > app_rect.right or 
            rect.bottom > app_rect.bottom):
            return 'partial'
        
        # Additional check: if element is very close to app edges (might appear cut off)
        edge_threshold = 5  # pixels
        if (rect.left < app_rect.left + edge_threshold or 
            rect.top < app_rect.top + edge_threshold or 
            rect.right > app_rect.right - edge_threshold or 
            rect.bottom > app_rect.bottom - edge_threshold):
            return 'partial'
        
        return 'full'

    def _get_clipping_ancestors(self, element):
        """
        Walk up parent chain and collect clipping containers.
        Returns: list of tuples (rect, name, control_type) for ancestors that clip content
        """
        clipping_ancestors = []
        
        # Get the element's own rect for validation
        try:
            element_rect = element.rectangle()
        except:
            return clipping_ancestors
        
        # Get screen bounds for validation
        screen_width = win32api.GetSystemMetrics(0)
        screen_height = win32api.GetSystemMetrics(1)
        
        try:
            current = element.parent()
            
            while current is not None:
                try:
                    control_type = current.element_info.control_type
                    
                    # Stop at window/document level - they don't clip in the way we care about
                    if control_type in {"Window", "Document", "Desktop"}:
                        break
                    
                    ancestor_rect = current.rectangle()
                    
                    # ===== VALIDATION: Is this ancestor rect trustworthy? =====
                    
                    # Check 1: Ancestor rect must have reasonable size
                    ancestor_width = ancestor_rect.right - ancestor_rect.left
                    ancestor_height = ancestor_rect.bottom - ancestor_rect.top
                    if ancestor_width <= 10 or ancestor_height <= 10:
                        current = current.parent()
                        continue
                    
                    # Check 2: Ancestor rect must be on screen (not garbage values)
                    if (ancestor_rect.right <= 0 or ancestor_rect.bottom <= 0 or
                        ancestor_rect.left >= screen_width or ancestor_rect.top >= screen_height):
                        current = current.parent()
                        continue
                    
                    # Check 3: Ancestor rect must OVERLAP with element rect
                    # If ancestor doesn't even overlap element, its rect is garbage
                    overlap_left = max(element_rect.left, ancestor_rect.left)
                    overlap_top = max(element_rect.top, ancestor_rect.top)
                    overlap_right = min(element_rect.right, ancestor_rect.right)
                    overlap_bottom = min(element_rect.bottom, ancestor_rect.bottom)
                    
                    has_overlap = (overlap_right > overlap_left and overlap_bottom > overlap_top)
                    
                    if not has_overlap:
                        # Ancestor rect doesn't contain/overlap element - skip it
                        current = current.parent()
                        continue
                    
                    # Check 4: Ancestor must actually be SMALLER than element in at least one dimension
                    # to be a real clipper (otherwise it's just a container, not clipping)
                    is_actually_clipping = (
                        ancestor_rect.left > element_rect.left or
                        ancestor_rect.top > element_rect.top or
                        ancestor_rect.right < element_rect.right or
                        ancestor_rect.bottom < element_rect.bottom
                    )
                    
                    if not is_actually_clipping:
                        # Ancestor fully contains element - not clipping it
                        current = current.parent()
                        continue
                    
                    # ===== Now check if this is a clipping container type =====
                    
                    is_clipping = False
                    
                    # Method 1: Has ScrollPattern (definite clipper)
                    try:
                        raw_element = current.element_info.element
                        # UIA_ScrollPatternId = 10004
                        scroll_pattern = raw_element.GetCurrentPattern(10004)
                        if scroll_pattern:
                            is_clipping = True
                    except:
                        pass
                    
                    # Method 2: Is a known clipping control type
                    # Only use control type as hint if rect validation passed
                    clipping_types = {"List", "ListBox", "Menu", "ComboBox", "ScrollViewer", "Tree"}
                    if not is_clipping and control_type in clipping_types:
                        is_clipping = True
                    
                    # Method 3: Even if not a known type, if ancestor is smaller and clips element, count it
                    # This catches custom containers that clip but aren't standard types
                    if not is_clipping and is_actually_clipping:
                        # Only count as clipping if it's significantly clipping (more than 5% of element)
                        element_area = (element_rect.right - element_rect.left) * (element_rect.bottom - element_rect.top)
                        overlap_area = (overlap_right - overlap_left) * (overlap_bottom - overlap_top)
                        if element_area > 0 and overlap_area < element_area * 0.95:
                            is_clipping = True
                    
                    if is_clipping:
                        # Get a name for this container
                        try:
                            name = current.element_info.name or control_type
                        except:
                            name = control_type
                        
                        clipping_ancestors.append((ancestor_rect, name, control_type))
                    
                    # Move to next parent
                    current = current.parent()
                    
                except Exception:
                    break
                    
        except Exception:
            pass
        
        return clipping_ancestors

    def _calculate_visible_rect(self, element_rect, clipping_ancestors):
        """
        Calculate the actual visible rectangle by intersecting with all clipping ancestors.
        
        Args:
            element_rect: The element's full bounding rectangle
            clipping_ancestors: List of (rect, name, control_type) from _get_clipping_ancestors
            
        Returns:
            tuple: (visible_rect or None, clipped_by_name or None)
                   visible_rect is None if element is fully hidden
        """
        if not clipping_ancestors:
            return element_rect, None
        
        # Start with element's full rect
        visible_left = element_rect.left
        visible_top = element_rect.top
        visible_right = element_rect.right
        visible_bottom = element_rect.bottom
        
        first_clipper_name = None
        
        for ancestor_rect, ancestor_name, control_type in clipping_ancestors:
            # Calculate intersection
            new_left = max(visible_left, ancestor_rect.left)
            new_top = max(visible_top, ancestor_rect.top)
            new_right = min(visible_right, ancestor_rect.right)
            new_bottom = min(visible_bottom, ancestor_rect.bottom)
            
            # Check if this ancestor actually clips the element
            if (new_left > visible_left or new_top > visible_top or 
                new_right < visible_right or new_bottom < visible_bottom):
                # This ancestor is clipping - record it as the clipper if first one
                if first_clipper_name is None:
                    first_clipper_name = ancestor_name
            
            visible_left = new_left
            visible_top = new_top
            visible_right = new_right
            visible_bottom = new_bottom
            
            # Check if fully clipped (no visible area remains)
            if visible_right <= visible_left or visible_bottom <= visible_top:
                return None, first_clipper_name or ancestor_name
        
        # Create visible rect using module-level Rect namedtuple
        visible_rect = Rect(visible_left, visible_top, visible_right, visible_bottom)
        
        return visible_rect, first_clipper_name

    # Same known clipping control types as _get_clipping_ancestors uses.
    _CLIPPING_CONTAINER_TYPES = {"List", "ListBox", "Menu", "ComboBox", "ScrollViewer", "Tree"}
    # Ancestors above these never clip in the way we care about (the upward
    # walk in _get_clipping_ancestors stops at them).
    _CLIP_BOUNDARY_TYPES = {"Window", "Document", "Desktop"}

    def _push_clip_entry(self, clip_stack, control_type, rect, name=None, element=None, scroll=None):
        """Return clip_stack extended with this container, or unchanged if its
        rect fails the same trustworthiness checks _get_clipping_ancestors
        applies (degenerate size, off-screen garbage). Non-mutating so sibling
        subtrees can share the parent stack."""
        if rect is None:
            return clip_stack
        width = rect.right - rect.left
        height = rect.bottom - rect.top
        if width <= 10 or height <= 10:
            return clip_stack
        screen_width = win32api.GetSystemMetrics(0)
        screen_height = win32api.GetSystemMetrics(1)
        if (rect.right <= 0 or rect.bottom <= 0 or
                rect.left >= screen_width or rect.top >= screen_height):
            return clip_stack
        return clip_stack + [{
            "rect": rect,
            "control_type": control_type,
            "name": name,
            "element": element,
            "scroll": scroll,  # None = unknown, probed lazily and memoized
        }]

    def _clippers_from_stack(self, element_rect, clip_stack):
        """Select the ancestors on the top-down clip stack that actually clip
        element_rect - the same classification _get_clipping_ancestors makes,
        but as pure math over rects the recursion already fetched, instead of
        a parent() COM walk per element. Returns (rect, name, control_type)
        tuples nearest-ancestor-first for _calculate_visible_rect."""
        clippers = []
        if not clip_stack:
            return clippers

        element_area = ((element_rect.right - element_rect.left) *
                        (element_rect.bottom - element_rect.top))

        for entry in reversed(clip_stack):
            ancestor_rect = entry["rect"]

            overlap_left = max(element_rect.left, ancestor_rect.left)
            overlap_top = max(element_rect.top, ancestor_rect.top)
            overlap_right = min(element_rect.right, ancestor_rect.right)
            overlap_bottom = min(element_rect.bottom, ancestor_rect.bottom)

            if overlap_right <= overlap_left or overlap_bottom <= overlap_top:
                continue

            is_actually_clipping = (
                ancestor_rect.left > element_rect.left or
                ancestor_rect.top > element_rect.top or
                ancestor_rect.right < element_rect.right or
                ancestor_rect.bottom < element_rect.bottom
            )
            if not is_actually_clipping:
                continue

            is_clipping = entry["control_type"] in self._CLIPPING_CONTAINER_TYPES
            if not is_clipping:
                overlap_area = ((overlap_right - overlap_left) *
                                (overlap_bottom - overlap_top))
                if element_area > 0 and overlap_area < element_area * 0.95:
                    is_clipping = True
                else:
                    # Clips less than 5%: only a ScrollPattern makes it count.
                    # Probe at most once per container, not once per descendant.
                    if entry["scroll"] is None:
                        entry["scroll"] = self._probe_scroll_pattern(entry.get("element"))
                    is_clipping = bool(entry["scroll"])

            if is_clipping:
                name = entry.get("name")
                if not name:
                    try:
                        name = entry["element"].element_info.name or entry["control_type"]
                    except Exception:
                        name = entry["control_type"]
                    entry["name"] = name
                clippers.append((ancestor_rect, name, entry["control_type"]))

        return clippers

    def _probe_scroll_pattern(self, element):
        """Check whether a pywinauto element supports ScrollPattern."""
        try:
            raw_element = element.element_info.element
            # UIA_ScrollPatternId = 10004
            return bool(raw_element.GetCurrentPattern(10004))
        except Exception:
            return False

    def _compute_visibility(self, element, element_rect, clipping_ancestors=None):
        """
        Compute true visibility of an element based on ancestor clipping.

        Args:
            element: The pywinauto element (may be None when clipping_ancestors
                     is supplied - it is only used to walk the parent chain)
            element_rect: The element's bounding rectangle
            clipping_ancestors: Optional precomputed list of
                     (rect, name, control_type) clippers, nearest-first. When
                     given, the per-element parent() COM walk is skipped.

        Returns:
            tuple: (visibility_status, visible_rect, clipped_by)
                   visibility_status: "full", "partial:XX%", or "hidden"
                   visible_rect: The actual visible rectangle (or None if hidden)
                   clipped_by: Name of the clipping container (or None if full)
        """
        # Get clipping ancestors
        if clipping_ancestors is None:
            clipping_ancestors = self._get_clipping_ancestors(element)
        
        # Calculate visible rect
        visible_rect, clipped_by = self._calculate_visible_rect(element_rect, clipping_ancestors)
        
        # Also check against application window bounds (app_rect)
        if visible_rect is not None and self.app_rect is not None:
            new_left = max(visible_rect.left, self.app_rect.left)
            new_top = max(visible_rect.top, self.app_rect.top)
            new_right = min(visible_rect.right, self.app_rect.right)
            new_bottom = min(visible_rect.bottom, self.app_rect.bottom)
            
            # Check if app window is clipping this element
            if (new_left > visible_rect.left or new_top > visible_rect.top or 
                new_right < visible_rect.right or new_bottom < visible_rect.bottom):
                if clipped_by is None:
                    clipped_by = self.application_name
            
            # Update visible_rect or set to None if no visible area
            if new_right > new_left and new_bottom > new_top:
                visible_rect = Rect(new_left, new_top, new_right, new_bottom)
            else:
                visible_rect = None
        
        # Calculate element's total area
        total_width = element_rect.right - element_rect.left
        total_height = element_rect.bottom - element_rect.top
        total_area = total_width * total_height
        
        # Handle zero area elements
        if total_area <= 0:
            return "hidden", None, clipped_by
        
        # If no visible rect, element is fully hidden
        if visible_rect is None:
            return "hidden", None, clipped_by
        
        # Calculate visible area
        visible_width = visible_rect.right - visible_rect.left
        visible_height = visible_rect.bottom - visible_rect.top
        visible_area = visible_width * visible_height
        
        # Handle edge case where visible dimensions are negative/zero
        if visible_area <= 0:
            return "hidden", None, clipped_by
        
        # Calculate percentage
        percentage = (visible_area / total_area) * 100
        
        # Classify visibility
        if percentage >= 95:
            return "full", visible_rect, None
        elif percentage > 0:
            return f"partial:{int(percentage)}%", visible_rect, clipped_by
        else:
            return "hidden", None, clipped_by

    def scan_elements(self):
        """Scan the active window and taskbar for configured element types using layer-based detection"""
        
        # Clear previous scans
        self.element_tree = []
        self.taskbar_tree = []
        # Session-long scanner: a hidden verdict from a full-screen step must not
        # leak into the next step. Re-decided below, before any taskbar thread.
        self.taskbar_visible = True
        self.taskbar_hidden_reason = None
        self.win32_elements = []  # Clear Win32 parallel scan results
        self.raw_uia_elements = []  # Clear raw UIA parallel scan results
        self.ocr_words = []  # Clear OCR results
        self.disabled_button_rects = []  # Clear disabled button rects
        # The scanner instance is session-long: without this reset,
        # found_elements accumulates every prior step's elements (and their
        # COM wrappers) forever. Must happen before any scan thread starts.
        self.found_elements = {}
        for element_type in self.config:
            if self.config[element_type]["track"]:
                self.found_elements[element_type] = []
        self.element_index = 0
        self.application_name = "Desktop"  # Default application name
        self.elements_to_draw = []  # Clear screenshot elements
        self.elements_mapping = {}  # Clear elements mapping
        self.app_rect = None  # Reset application rectangle
        
        # Clear layer storage
        self.top_layer_tree = []
        self.second_layer_tree = []
        self.top_layer_info = None
        self.second_layer_info = None
        
        # Start OCR scan in parallel (full screen, thread-safe)
        self.ocr_scanner = OCRScanner()
        self.ocr_thread = threading.Thread(target=self.ocr_scanner.scan)
        self.ocr_thread.start()
        
        # Detect visible layers using visibility-based detection (no focus required)
        layers = get_topmost_layers()
        self.top_layer_info = layers["top_layer"]
        self.second_layer_info = layers["second_layer"]
        # Is the taskbar actually on screen? Decided once, before any taskbar
        # thread starts - the thread starts, the serial fallback, the worker and
        # the text output all read this verdict.
        self._update_taskbar_visibility()
        
        # Scan top layer
        if self.top_layer_info:
            self.application_name = self.top_layer_info["name"]
            self._is_browser = self._is_browser_app()  # Cache browser detection
            # Taskbar is independent of the app scan - overlap it with the
            # expensive window walk instead of appending it afterwards.
            # Started only after application_name/_is_browser/taskbar_visible
            # are set so the taskbar walk sees the same flags it saw when it
            # ran serially. Not started at all when the taskbar is off screen.
            self._start_taskbar_scan()
            self._scan_layer(self.top_layer_info, self.top_layer_tree, is_top_layer=True)
            # Flyout popup layered above an overlay (e.g. Start power menu):
            # scan it into the same top-layer tree so its items are native
            # elements, not OCR leftovers. Runs after the overlay scan so its
            # own app_rect governs its visibility math.
            if layers.get("overlay_popup") and self.top_layer_info["type"] == "overlay":
                self._scan_layer(layers["overlay_popup"], self.top_layer_tree, is_top_layer=False)
        else:
            # Fallback: scan desktop if no layer detected
            self.application_name = "Desktop"
            # Recompute - the session-long scanner otherwise keeps the
            # previous step's flag (e.g. True after a Chrome step) and would
            # apply browser-only filtering to the desktop scan
            self._is_browser = self._is_browser_app()
            self.top_layer_info = {"type": "app", "name": "Desktop", "class": "Progman", "hwnd": None}
            self._start_taskbar_scan()
            self._scan_desktop()
            self.top_layer_tree = self.element_tree.copy()
        
        # Scan second layer (if exists)
        if self.second_layer_info:
            self._scan_layer(self.second_layer_info, self.second_layer_tree, is_top_layer=False)
        
        # Combine results: element_tree = top_layer
        self.element_tree = self.top_layer_tree.copy()
        
        # Set application_name from top layer for compatibility
        if self.top_layer_info:
            self.application_name = self.top_layer_info["name"]
        
        # Check if browser is loading and wait until loaded, then rescan.
        # The initial answer comes from the tree we just scanned (the toolbar
        # Reload/Stop button is in it) - no extra descendants() sweep. Only the
        # poll loop, where the tree is stale by definition, queries live.
        browser_type = self._detect_browser_type()
        if browser_type:
            if self._is_browser_loading_from_tree(browser_type):
                browser_name = BROWSER_CONFIG[browser_type]["app_name_contains"]
                print(f"{browser_name} is loading... waiting")
                while self._is_browser_loading(browser_type):
                    time.sleep(0.25)
                print("Browser loaded. Rescanning...")

                # Taskbar thread appends to disabled_button_rects, which is
                # about to be reset - settle it, then rescan it too.
                if self.taskbar_thread is not None:
                    self.taskbar_thread.join()
                    self.taskbar_thread = None

                # Reset state for rescan
                self.taskbar_tree = []
                self.top_layer_tree = []
                self.second_layer_tree = []
                self.element_tree = []
                self.win32_elements = []
                self.raw_uia_elements = []
                self.ocr_words = []
                self.disabled_button_rects = []
                self.element_index = 0
                self.elements_to_draw = []
                self.elements_mapping = {}
                self.found_elements = {}
                for element_type in self.config:
                    if self.config[element_type]["track"]:
                        self.found_elements[element_type] = []
                
                # Restart OCR scan (previous one captured loading state)
                if self.ocr_thread is not None:
                    self.ocr_thread.join()
                self.ocr_scanner = OCRScanner()
                self.ocr_thread = threading.Thread(target=self.ocr_scanner.scan)
                self.ocr_thread.start()
                
                # Rescan using layer-based approach. The loading wait can
                # outlast a full-screen toggle (F11, video) or an auto-hide
                # slide - re-decide the taskbar first. Safe only here: the
                # first taskbar thread was joined above and none is running.
                self._update_taskbar_visibility()
                self._start_taskbar_scan()
                if self.top_layer_info:
                    self._scan_layer(self.top_layer_info, self.top_layer_tree, is_top_layer=True)
                
                if self.second_layer_info:
                    self._scan_layer(self.second_layer_info, self.second_layer_tree, is_top_layer=False)
                
                # Combine results
                self.element_tree = self.top_layer_tree.copy()
                
                # Wait for parallel threads
                if self.win32_thread is not None:
                    self.win32_thread.join()
                    self.win32_thread = None
                if self.raw_uia_thread is not None:
                    self.raw_uia_thread.join()
                    self.raw_uia_thread = None
                
                # Merge results
                self._dedupe_and_merge()
        
        # Wait for Win32 thread to complete
        if self.win32_thread is not None:
            self.win32_thread.join()
            self.win32_thread = None
        
        # Wait for raw UIA thread to complete
        if self.raw_uia_thread is not None:
            self.raw_uia_thread.join()
            self.raw_uia_thread = None
        
        # Merge and deduplicate all results (merges into element_tree)
        self._dedupe_and_merge()
            
        # Taskbar rects must be available before OCR filtering. The scan runs
        # on a parallel thread started before the main walk; the serial scan
        # remains as a fallback in case COM refused the worker thread - never
        # for a taskbar that is off screen, whose tree is empty on purpose.
        if self.taskbar_thread is not None:
            self.taskbar_thread.join()
            self.taskbar_thread = None
        if self.taskbar_visible and not self.taskbar_tree:
            try:
                taskbar = self.desktop.window(class_name="Shell_TrayWnd").wrapper_object()
                self._scan_element_recursive(taskbar, self.taskbar_tree, 0, skip_visibility=True)
            except Exception as e:
                pass
        
        # Wait for OCR thread to complete
        if self.ocr_thread is not None:
            self.ocr_thread.join()
            self.ocr_thread = None
            self.ocr_words = self.ocr_scanner.get_lines()
        
        # Filter OCR results against UIA elements, merge survivors into tree
        self._filter_and_merge_ocr()
        
        # Re-index entire tree in document order for continuous numbering
        self.element_index = 0
        self.elements_mapping = {}
        self._reindex_tree(self.element_tree)
        self._reindex_tree(self.taskbar_tree)
        
        # Rebuild elements_to_draw with updated indices
        self.elements_to_draw = []
        self._rebuild_draw_list(self.element_tree)
        self._rebuild_draw_list(self.taskbar_tree)
        
        # Sync merged results back to top_layer_tree for save_to_file()
        self.top_layer_tree = self.element_tree.copy()
        
        # Only save to file if DEBUG is enabled
        if DEBUG:
            self.save_to_file()

    def _filter_and_merge_ocr(self):
        """Filter OCR lines against UIA elements, nest survivors into the tree.
        Only leaf element rects (no children) claim screen space. Parent containers
        are structural wrappers — gaps between their children are where OCR fills in.
        Surviving OCR lines are inserted into the deepest matching container rather
        than appended flat at the end.
        """
        if not self.ocr_words:
            return
        
        # Collect only leaf element rects (elements with no children actually claim screen space)
        leaf_rects = []
        self._collect_leaf_rects(self.element_tree, leaf_rects)
        self._collect_leaf_rects(self.taskbar_tree, leaf_rects)
        leaf_rects.extend(self.disabled_button_rects)
        
        # Filter OCR lines: discard if overlapping a detected leaf element, keep otherwise
        kept_lines = []
        for line in self.ocr_words:
            cx = (line["left"] + line["right"]) // 2
            cy = (line["top"] + line["bottom"]) // 2
            
            overlaps_leaf = False
            for rect in leaf_rects:
                if rect.left <= cx <= rect.right and rect.top <= cy <= rect.bottom:
                    overlaps_leaf = True
                    break
            
            if not overlaps_leaf:
                kept_lines.append(line)
        
        # Nest each surviving OCR line into the deepest matching container in the tree
        for line in kept_lines:
            self.element_index += 1
            line_rect = Rect(line["left"], line["top"], line["right"], line["bottom"])
            
            element_info = {
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
                "source": "ocr"
            }
            
            # Find deepest container and insert there
            cx = (line["left"] + line["right"]) // 2
            cy = (line["top"] + line["bottom"]) // 2
            target_list = self._find_deepest_container(self.element_tree, cx, cy)
            target_list.append(element_info)
            
            # Add to elements mapping for controller (index updated later by reindex)
            self.elements_mapping[str(self.element_index)] = {
                'element': None,
                'rect': line_rect,
                'visible_rect': line_rect,
                'name': line["text"],
                'aria_role': '',
                'type': 'OCR_TEXT',
                'value': None,
                'visibility': 'full',
                'clipped_by': None
            }
            
            # Add to screenshot draw list
            self.elements_to_draw.append({
                "rect": line_rect,
                "index": self.element_index,
                "depth": 0,
                "visibility": "full",
                "source": "ocr"
            })
    
    def _reindex_tree(self, tree_list):
        """Walk tree in document order and assign continuous indices."""
        for item in tree_list:
            self.element_index += 1
            item["index"] = self.element_index
            
            # Rebuild elements_mapping with new index
            self.elements_mapping[str(self.element_index)] = {
                'element': item.get('element'),
                'rect': item.get('rect'),
                'visible_rect': item.get('visible_rect'),
                'name': item.get('name', ''),
                'aria_role': item.get('aria_role', ''),
                'type': item.get('type', ''),
                'value': item.get('value'),
                'visibility': item.get('visibility', 'full'),
                'clipped_by': item.get('clipped_by')
            }
            
            if item.get("browser_top_layer") is not None:
                self._reindex_tree(item["browser_top_layer"])
                self._reindex_tree(item["browser_second_layer"])
            elif item.get("children"):
                self._reindex_tree(item["children"])

    def _rebuild_draw_list(self, tree_list, depth=0):
        """Rebuild elements_to_draw from tree with updated indices."""
        for item in tree_list:
            rect = item.get("rect") or item.get("visible_rect")
            if rect:
                self.elements_to_draw.append({
                    "rect": rect,
                    "index": item["index"],
                    "depth": depth,
                    "visibility": item.get("visibility", "full"),
                    "source": item.get("source", "")
                })
            if item.get("browser_top_layer") is not None:
                self._rebuild_draw_list(item["browser_top_layer"], depth + 1)
                self._rebuild_draw_list(item["browser_second_layer"], depth + 1)
            elif item.get("children"):
                self._rebuild_draw_list(item["children"], depth + 1)

    def _find_deepest_container(self, tree_list, cx, cy):
        """Find the deepest element whose rect contains the point (cx, cy).
        Returns the children list of that element, or the top-level tree_list
        if no container claims the point. Also checks browser_second_layer.
        """
        for item in tree_list:
            rect = item.get("rect") or item.get("visible_rect")
            if not rect:
                continue
            
            if rect.left <= cx <= rect.right and rect.top <= cy <= rect.bottom:
                # Point is inside this element — try to go deeper
                
                # Check browser_second_layer first (OCR fills web page gaps)
                if item.get("browser_second_layer") is not None:
                    deeper = self._find_deepest_container(item["browser_second_layer"], cx, cy)
                    if deeper is not item["browser_second_layer"]:
                        return deeper
                    # Landed in browser_second_layer but no deeper match — nest here
                    return item["browser_second_layer"]
                
                # Check regular children
                if item.get("children"):
                    deeper = self._find_deepest_container(item["children"], cx, cy)
                    if deeper is not item["children"]:
                        return deeper
                    # Children exist but none claimed the point — nest among siblings
                    return item["children"]
                
                # Leaf element that contains the point — nest as its child
                return item.setdefault("children", [])
        
        # No element in this list contains the point — caller decides
        return tree_list
    
    _STRUCTURAL_CONTAINER_TYPES = {"Pane", "Document", "Group", "Edit"}

    def _collect_leaf_rects(self, tree_list, rects):
        """Recursively collect rects for OCR overlap checking.
        Structural types (Pane, Document, Group) never claim screen space.
        Edit is structural only when it has no value (content container like
        rich text editors). Edits with a value are real textboxes — collect rect.
        All other element types always collect their rect.
        """
        for item in tree_list:
            has_children = bool(item.get("children"))
            elem_type = item["type"]
            is_structural = elem_type in self._STRUCTURAL_CONTAINER_TYPES

            if is_structural:
                if elem_type == "Edit" and item.get("value") and not has_children:
                    rect = item.get("rect") or item.get("visible_rect")
                    if rect:
                        rects.append(rect)
                if has_children:
                    self._collect_leaf_rects(item["children"], rects)
            else:
                rect = item.get("rect") or item.get("visible_rect")
                if rect:
                    rects.append(rect)
                if has_children:
                    self._collect_leaf_rects(item["children"], rects)

    def _dedupe_and_merge(self):
        """Deduplicate elements from UIA, Win32, and raw UIA by coordinates, then merge into unified tree"""
        if not self.win32_elements and not self.raw_uia_elements:
            return  # Nothing to merge
        
        # Collect all UIA element rects for comparison
        uia_rects = []
        self._collect_rects_recursive(self.element_tree, uia_rects)
        
        # Track all merged rects (UIA + newly added)
        all_rects = list(uia_rects)
        
        # Flatten Win32 tree for deduplication check
        win32_flat = []
        self._flatten_win32_tree(self.win32_elements, win32_flat)
        
        # Check each Win32 element against existing rects
        for win32_elem in win32_flat:
            win32_rect = win32_elem["rect"]
            is_duplicate = False
            
            for existing_rect in all_rects:
                if self._rects_match(win32_rect, existing_rect, tolerance=5):
                    is_duplicate = True
                    break
            
            if not is_duplicate:
                # Assign index and add to tree
                self.element_index += 1
                win32_elem["index"] = self.element_index
                
                # Track this rect
                all_rects.append(win32_rect)
                
                # Add to elements mapping for controller
                self.elements_mapping[str(self.element_index)] = {
                    'element': win32_elem.get("element"),
                    'rect': win32_rect,
                    'visible_rect': win32_elem.get("visible_rect", win32_rect),
                    'name': win32_elem.get("name", ""),
                    'aria_role': win32_elem.get("aria_role", ""),
                    'type': win32_elem.get("type", ""),
                    'value': win32_elem.get("value"),
                    'visibility': win32_elem.get("visibility", "full"),
                    'clipped_by': win32_elem.get("clipped_by")
                }
                
                # Clean up temporary keys used for deduplication
                if "rect" in win32_elem:
                    del win32_elem["rect"]
                if "source" in win32_elem:
                    del win32_elem["source"]
                
                self.element_tree.append(win32_elem)
                
                # Add to found_elements for summary
                element_type = win32_elem["type"]
                if element_type not in self.found_elements:
                    self.found_elements[element_type] = []
                self.found_elements[element_type].append(win32_elem)
                
                # Add to screenshot draw list
                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": win32_elem["visible_rect"],
                        "index": win32_elem["index"],
                        "depth": 0,
                        "visibility": win32_elem.get("visibility", "full")
                    })
        
        # Flatten raw UIA list for deduplication check
        raw_uia_flat = []
        self._flatten_raw_uia_list(self.raw_uia_elements, raw_uia_flat)
        
        # Check each raw UIA element against all existing rects
        for raw_elem in raw_uia_flat:
            raw_rect = raw_elem["rect"]
            is_duplicate = False
            
            for existing_rect in all_rects:
                if self._rects_match(raw_rect, existing_rect, tolerance=5):
                    is_duplicate = True
                    break
            
            if not is_duplicate:
                # Assign index and add to tree
                self.element_index += 1
                raw_elem["index"] = self.element_index
                
                # Track this rect
                all_rects.append(raw_rect)
                
                # Add to elements mapping for controller
                self.elements_mapping[str(self.element_index)] = {
                    'element': raw_elem.get("element"),
                    'rect': raw_rect,
                    'visible_rect': raw_elem.get("visible_rect", raw_rect),
                    'name': raw_elem.get("name", ""),
                    'aria_role': raw_elem.get("aria_role", ""),
                    'type': raw_elem.get("type", ""),
                    'value': raw_elem.get("value"),
                    'visibility': raw_elem.get("visibility", "full"),
                    'clipped_by': raw_elem.get("clipped_by")
                }
                
                # Clean up temporary keys used for deduplication
                if "rect" in raw_elem:
                    del raw_elem["rect"]
                if "source" in raw_elem:
                    del raw_elem["source"]
                if "raw_element" in raw_elem:
                    del raw_elem["raw_element"]
                
                self.element_tree.append(raw_elem)
                
                # Add to found_elements for summary
                element_type = raw_elem["type"]
                if element_type not in self.found_elements:
                    self.found_elements[element_type] = []
                self.found_elements[element_type].append(raw_elem)
                
                # Add to screenshot draw list
                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": raw_elem["visible_rect"],
                        "index": raw_elem["index"],
                        "depth": 0,
                        "visibility": raw_elem.get("visibility", "full")
                    })
    
    def _flatten_win32_tree(self, tree_list, flat_list):
        """Recursively flatten Win32 tree into a flat list for deduplication"""
        for item in tree_list:
            flat_list.append(item)
            if item.get("children"):
                self._flatten_win32_tree(item["children"], flat_list)
                # Clear children since we're flattening (will be added as separate items)
                item["children"] = []
    
    def _flatten_raw_uia_list(self, tree_list, flat_list):
        """Flatten raw UIA list into a flat list for deduplication"""
        for item in tree_list:
            flat_list.append(item)
            if item.get("children"):
                self._flatten_raw_uia_list(item["children"], flat_list)
                # Clear children since we're flattening
                item["children"] = []
    
    def _collect_rects_recursive(self, tree_list, rects):
        """Recursively collect all element rectangles from tree for deduplication"""
        for item in tree_list:
            # Use original full rect for deduplication (not visible_rect which may be clipped)
            if item.get("rect"):
                rects.append(item["rect"])
            elif item.get("visible_rect"):
                rects.append(item["visible_rect"])
            if item.get("children"):
                self._collect_rects_recursive(item["children"], rects)
    
    def _rects_match(self, rect1, rect2, tolerance=5):
        """Check if two rectangles match within tolerance"""
        return (abs(rect1.left - rect2.left) <= tolerance and
                abs(rect1.top - rect2.top) <= tolerance and
                abs(rect1.right - rect2.right) <= tolerance and
                abs(rect1.bottom - rect2.bottom) <= tolerance)
    
    def _scan_desktop(self):
        """Helper method to scan desktop"""
        try:
            desktop_window = self.desktop.window(class_name="Progman")
            if not desktop_window.exists():
                desktop_window = self.desktop.window(class_name="WorkerW")
            # Resolve the spec to a wrapper once (attribute access on a
            # WindowSpecification re-runs the window search every time)
            desktop_window = desktop_window.wrapper_object()

            self.app_rect = desktop_window.rectangle()  # Capture desktop window bounds

            # Get hwnd for parallel scans
            try:
                desktop_hwnd = desktop_window.handle
            except:
                desktop_hwnd = None

            # Start Win32 scan in parallel
            self.win32_thread = threading.Thread(target=self._scan_with_win32_parallel, args=("Program Manager",))
            self.win32_thread.start()

            # Start raw UIA scan in parallel
            if desktop_hwnd:
                self.raw_uia_thread = threading.Thread(target=self._scan_with_raw_uia_parallel, args=(desktop_hwnd,))
                self.raw_uia_thread.start()

            scanned = False
            try:
                scanned = self._scan_window_cached(desktop_window.element_info.element, self.element_tree)
            except Exception:
                scanned = False
            if not scanned:
                self._scan_element_recursive(desktop_window, self.element_tree, 0, skip_visibility=False)
        except Exception as e:
            pass

    def _update_taskbar_visibility(self):
        """Per-scan taskbar gate. Call on the scan thread only, after layer
        detection and never while a taskbar thread is running."""
        if (self.top_layer_info or {}).get("type") in ("overlay", "tray_overflow"):
            # Start / Search / Notification Center / Quick Settings and the tray
            # overflow open with the taskbar raised above everything, even over
            # a full-screen game - a z-order still catching up must not hide it.
            self.taskbar_visible, self.taskbar_hidden_reason = True, None
            return
        visible, reason, covered = detect_taskbar_visibility()
        self.taskbar_visible = visible
        self.taskbar_hidden_reason = None if visible else reason
        if DEBUG and not visible:
            print(f"Taskbar not on screen ({reason}, covered={covered}) - skipping taskbar scan")

    def _start_taskbar_scan(self):
        """Start the parallel taskbar walk - only when the taskbar is on screen.
        A skipped walk leaves taskbar_thread None, which both joins handle."""
        self.taskbar_thread = None
        if self.taskbar_visible:
            self.taskbar_thread = threading.Thread(target=self._scan_taskbar_parallel)
            self.taskbar_thread.start()

    def _scan_taskbar_parallel(self):
        """Parallel taskbar scan - independent of the app walk. Runs with
        skip_visibility=True (no clip stack, no app_rect reads); indices are
        reassigned by _reindex_tree afterwards, so interleaving with the main
        scan's counter is harmless. Uses its own Desktop instance so COM
        objects are created on this thread."""
        if not self.taskbar_visible:
            return  # Off screen - scan_elements never starts it then; guards future callers
        try:
            taskbar = Desktop(backend="uia").window(class_name="Shell_TrayWnd").wrapper_object()
            self._scan_element_recursive(taskbar, self.taskbar_tree, 0, skip_visibility=True)
        except Exception:
            pass  # Silent fail - scan_elements falls back to a serial scan

    def _scan_with_win32_parallel(self, window_title):
        """Parallel Win32 scan - stores results in self.win32_elements for later merge"""
        try:
            warnings.filterwarnings("ignore", category=UserWarning, module="pywinauto")
            app = Application(backend="win32").connect(title=window_title)
            dialog = app.window(title=window_title)
            
            # Scan recursively just like UIA
            self._scan_win32_recursive(dialog, self.win32_elements, 0)
                    
        except Exception:
            pass  # Silent fail - UIA results will be used

    def _scan_win32_recursive(self, element, tree_list, depth):
        """Recursively scan Win32 elements - mirrors UIA approach, stores for later merge"""
        try:
            ctrl_class = element.class_name()
            ctrl_text = element.window_text()
            
            # Check if class name exists in our win32_class_map (built from ELEMENT_CONFIG)
            if ctrl_class in self.win32_class_map:
                element_type = self.win32_class_map[ctrl_class]
            elif ctrl_class in self.config:
                element_type = ctrl_class
            else:
                element_type = None
            
            should_track_element = False
            
            if element_type and element_type in self.config and self.config[element_type]["track"]:
                should_track_element = True
                
                # Check if enabled
                if should_track_element:
                    try:
                        if not element.is_enabled():
                            should_track_element = False
                    except:
                        pass
                
                # Check if visible
                if should_track_element:
                    try:
                        if not element.is_visible():
                            should_track_element = False
                    except:
                        pass
                
                # Browser keyboard focusable check
                if should_track_element and self.config[element_type].get("keyboard_focusable", False):
                    if self._is_browser:
                        try:
                            # Win32 doesn't have direct keyboard_focusable, skip these in browsers
                            should_track_element = False
                        except:
                            pass
                
                # Skip window control buttons at depth 0
                if should_track_element and depth == 0 and element_type == "Button":
                    try:
                        btn_name = ctrl_text.lower()
                        if any(word in btn_name for word in ["minimize", "minimise", "maximize", "maximise", "close"]):
                            should_track_element = False
                    except:
                        pass
                
                # Skip Text elements in Win32 - no AriaRole to check for heading
                # UIA scanner handles Text elements with AriaRole="heading" check
                if should_track_element and element_type == "Text":
                    should_track_element = False
                
                # Check element size
                if should_track_element:
                    try:
                        rect = element.rectangle()
                        width = rect.right - rect.left
                        height = rect.bottom - rect.top
                        if width < 10 or height < 10:
                            should_track_element = False
                    except:
                        should_track_element = False
                
                # Get name using fallback chain
                name = ""
                if should_track_element:
                    for prop in self.config[element_type].get("fallback", ["name"]):
                        try:
                            if prop == "name":
                                name = ctrl_text
                            elif prop == "class_name":
                                name = ctrl_class
                        except:
                            continue
                        if name and name.strip():
                            break
                    
                    # Remove Unicode control characters
                    name = ''.join(char for char in name if ord(char) >= 32 and ord(char) != 0x200E)
                    
                    if not name or name.strip() == "":
                        should_track_element = False
            
            if should_track_element:
                rect = element.rectangle()
                
                # Get value based on element type
                value = None
                if element_type == "Edit":
                    try:
                        value = element.window_text()
                    except:
                        value = ""
                elif element_type == "CheckBox":
                    try:
                        value = "Checked" if element.is_checked() else "Unchecked"
                    except:
                        value = ""
                elif element_type in ["ComboBox", "ListItem"]:
                    try:
                        value = element.window_text()
                    except:
                        value = ""
                
                # Win32 doesn't have AriaRole - leave empty
                aria_role = ""
                
                # Compute visibility (simplified for Win32 - use app_rect)
                visibility_status = "full"
                clipped_by = None
                
                if self.app_rect is not None:
                    if (rect.left < self.app_rect.left or rect.top < self.app_rect.top or
                        rect.right > self.app_rect.right or rect.bottom > self.app_rect.bottom):
                        visibility_status = "partial"
                        clipped_by = self.application_name
                
                # Add actions hint for ListItem in non-browser apps
                actions = None
                if element_type == "ListItem" and not self._is_browser:
                    actions = "{click: select, double_click: open}"
                
                # Store without index - will be assigned after merge/dedupe
                element_info = {
                    "element": element,
                    "name": name,
                    "aria_role": aria_role,
                    "type": element_type,
                    "active": element.is_enabled(),
                    "index": None,  # Assigned after dedupe
                    "value": value,
                    "actions": actions,
                    "visibility": visibility_status,
                    "clipped_by": clipped_by,
                    "visible_rect": rect,
                    "rect": rect,  # Store rect for deduplication
                    "children": [],
                    "source": "win32"  # Track source for debugging
                }
                
                tree_list.append(element_info)
                
                # Recursively scan children
                try:
                    for child in element.children():
                        self._scan_win32_recursive(child, element_info["children"], depth + 1)
                except:
                    pass
            else:
                # Even if we don't track this element, scan its children
                try:
                    for child in element.children():
                        self._scan_win32_recursive(child, tree_list, depth + 1)
                except:
                    pass
                    
        except Exception:
            pass

    def _scan_with_raw_uia_parallel(self, hwnd):
        """Parallel raw UIA COM scan - ONLY for Start Menu LauncherFrameXAMLWindow
        
        Only runs when LauncherFrameXAMLWindow sibling is detected.
        This element contains Start Menu content (Pinned, Recommended, etc.)
        that pywinauto cannot see because it's a sibling with empty name.
        """
        try:
            # Get UIA element from window handle
            foreground_element = _uia.ElementFromHandle(hwnd)
            if not foreground_element:
                return
            
            # Get parent to search for LauncherFrameXAMLWindow sibling
            walker = _uia.ControlViewWalker
            parent_element = walker.GetParentElement(foreground_element)
            
            if not parent_element:
                return  # No parent, can't find siblings
            
            # Search siblings for LauncherFrameXAMLWindow
            launcher_frame = None
            child = walker.GetFirstChildElement(parent_element)
            
            while child:
                try:
                    auto_id = child.CurrentAutomationId or ""
                    if auto_id == "LauncherFrameXAMLWindow":
                        launcher_frame = child
                        break
                except:
                    pass
                child = walker.GetNextSiblingElement(child)
            
            # If LauncherFrameXAMLWindow not found, skip raw UIA entirely
            if not launcher_frame:
                return
            
            # Found LauncherFrameXAMLWindow - scan ONLY this element
            self._scan_raw_uia_recursive(launcher_frame, self.raw_uia_elements, 0)
                
        except Exception:
            pass  # Silent fail - other scan results will be used

    def _scan_raw_uia_recursive(self, element, tree_list, depth, max_depth=30):
        """Recursively scan using raw UIA COM interface - same rules as pywinauto UIA"""
        if depth > max_depth:
            return
        
        try:
            control_type = element.CurrentControlType
            name = element.CurrentName or ""
            
            # Check if this control type is in our config
            element_type = self.uia_type_map.get(control_type)
            
            # Special handling: Group (50026) that might be Edit
            if control_type == 50026:  # Group
                try:
                    is_keyboard_focusable = element.CurrentIsKeyboardFocusable
                    is_control = element.CurrentIsControlElement
                    if is_control and is_keyboard_focusable:
                        element_type = "Edit"
                except:
                    pass
            
            should_track_element = False
            
            if element_type and element_type in self.config and self.config[element_type]["track"]:
                should_track_element = True
                
                # Check if enabled
                if should_track_element:
                    try:
                        if not element.CurrentIsEnabled:
                            should_track_element = False
                    except:
                        pass
                
                # Check if offscreen (basic visibility)
                if should_track_element:
                    try:
                        if element.CurrentIsOffscreen:
                            should_track_element = False
                    except:
                        pass
                
                # Browser keyboard focusable check
                if should_track_element and self.config[element_type].get("keyboard_focusable", False):
                    if self._is_browser:
                        try:
                            if not element.CurrentIsKeyboardFocusable:
                                should_track_element = False
                        except:
                            pass
                
                # Skip window control buttons at depth 0
                if should_track_element and depth == 0 and element_type == "Button":
                    try:
                        btn_name = (element.CurrentName or "").lower()
                        if any(word in btn_name for word in ["minimize", "minimise", "maximize", "maximise", "close"]):
                            should_track_element = False
                    except:
                        pass
                
                # Text elements: only track if AriaRole is "heading"
                if should_track_element and element_type == "Text":
                    try:
                        aria_role = element.CurrentAriaRole
                        if aria_role != "heading":
                            should_track_element = False
                    except:
                        should_track_element = False
                
                # Check element size
                if should_track_element:
                    try:
                        rect = element.CurrentBoundingRectangle
                        width = rect.right - rect.left
                        height = rect.bottom - rect.top
                        if width < 10 or height < 10:
                            should_track_element = False
                    except:
                        should_track_element = False
                
                # Get name using fallback chain
                if should_track_element:
                    name = ""
                    for prop in self.config[element_type].get("fallback", ["name"]):
                        try:
                            if prop == "name":
                                name = element.CurrentName or ""
                            elif prop == "automation_id":
                                name = element.CurrentAutomationId or ""
                            elif prop == "class_name":
                                name = element.CurrentClassName or ""
                            elif prop == "legacy_description":
                                name = element.GetCurrentPropertyValue(30094) or ""
                        except:
                            continue
                        if name and name.strip():
                            break
                    
                    # Remove Unicode control characters
                    name = ''.join(char for char in name if ord(char) >= 32 and ord(char) != 0x200E)
                    
                    if not name or name.strip() == "":
                        should_track_element = False
                
                if should_track_element:
                    rect = element.CurrentBoundingRectangle
                    
                    # Create rect tuple using module-level Rect namedtuple
                    rect_tuple = Rect(rect.left, rect.top, rect.right, rect.bottom)
                    
                    # Wrap raw element for pywinauto compatibility
                    wrapped_element = None
                    try:
                        child_info = uia_element_info.UIAElementInfo(element)
                        wrapped_element = UIAWrapper(child_info)
                    except:
                        pass
                    
                    # Compute visibility using same logic as main UIA scan
                    visibility_status = "full"
                    visible_rect = rect_tuple
                    clipped_by = None
                    
                    if wrapped_element:
                        try:
                            visibility_status, visible_rect, clipped_by = self._compute_visibility(wrapped_element, rect_tuple)
                        except:
                            # Fallback to simple app_rect check
                            if self.app_rect is not None:
                                if (rect_tuple.left < self.app_rect.left or 
                                    rect_tuple.top < self.app_rect.top or
                                    rect_tuple.right > self.app_rect.right or 
                                    rect_tuple.bottom > self.app_rect.bottom):
                                    visibility_status = "partial"
                                    clipped_by = self.application_name
                    
                    # Skip hidden elements
                    if visibility_status == "hidden":
                        should_track_element = False
                
                if should_track_element:
                    # Get value based on element type
                    value = None
                    if element_type == "Edit":
                        try:
                            value_pattern = element.GetCurrentPattern(10002)  # UIA_ValuePatternId
                            if value_pattern:
                                value = value_pattern.CurrentValue
                        except:
                            value = ""
                    elif element_type == "CheckBox":
                        try:
                            toggle_pattern = element.GetCurrentPattern(10015)  # UIA_TogglePatternId
                            if toggle_pattern:
                                state = toggle_pattern.CurrentToggleState
                                value = "Checked" if state == 1 else "Unchecked"
                        except:
                            value = ""
                    elif element_type in ["ComboBox", "ListItem"]:
                        try:
                            value_pattern = element.GetCurrentPattern(10002)
                            if value_pattern:
                                value = value_pattern.CurrentValue
                        except:
                            value = ""
                    elif element_type == "Document":
                        try:
                            value_pattern = element.GetCurrentPattern(10002)
                            if value_pattern:
                                text = value_pattern.CurrentValue
                                value = text[:100] + "..." if len(text) > 100 else text
                        except:
                            value = ""
                    elif element_type == "Group":
                        try:
                            value_pattern = element.GetCurrentPattern(10002)
                            if value_pattern:
                                value = value_pattern.CurrentValue
                        except:
                            value = ""
                    
                    # Get AriaRole
                    aria_role = ""
                    try:
                        aria_role = element.CurrentAriaRole or ""
                    except:
                        aria_role = ""
                    
                    # Add actions hint for ListItem in non-browser apps
                    actions = None
                    if element_type == "ListItem" and not self._is_browser:
                        actions = "{click: select, double_click: open}"
                    
                    element_info = {
                        "element": wrapped_element,
                        "raw_element": element,
                        "name": name,
                        "aria_role": aria_role,
                        "type": element_type,
                        "active": element.CurrentIsEnabled,
                        "index": None,  # Assigned after dedupe
                        "value": value,
                        "actions": actions,
                        "visibility": visibility_status,
                        "clipped_by": clipped_by,
                        "visible_rect": visible_rect if visible_rect else rect_tuple,
                        "rect": rect_tuple,  # For deduplication
                        "children": [],
                        "source": "raw_uia"
                    }
                    
                    tree_list.append(element_info)
            
            # Recurse children using ControlViewWalker
            walker = _uia.ControlViewWalker
            child = walker.GetFirstChildElement(element)
            
            while child:
                self._scan_raw_uia_recursive(child, tree_list, depth + 1, max_depth)
                child = walker.GetNextSiblingElement(child)
                
        except Exception:
            pass

    def _scan_window_cached(self, raw_root, target_tree):
        """Cached fast path: one cross-process BuildUpdatedCache call fetches
        the whole subtree with every property the scan needs, then the walk
        runs in-process. Returns False (target_tree untouched) so the caller
        can fall back to the live pywinauto walk."""
        try:
            cached_root = raw_root.BuildUpdatedCache(_get_scan_cache_request())
        except Exception:
            return False
        if cached_root is None:
            return False

        marker = len(target_tree)
        self._scan_cached_recursive(cached_root, target_tree, 0, [])

        if len(target_tree) == marker:
            # Nothing tracked. Distinguish "genuinely empty window" from a
            # provider that ignored the subtree cache request: no cached
            # children at all means the cache is unusable - fall back.
            try:
                kids = cached_root.GetCachedChildren()
                if kids is None or kids.Length == 0:
                    return False
            except Exception:
                return False
        return True

    def _live_child_runtime_ids(self, element):
        """RuntimeIds of an element's LIVE children. Plain FindAll goes through
        the provider's enumeration (which prunes hidden UI), unlike the cache
        serializer. Returns None when the probe itself fails."""
        try:
            arr = element.FindAll(_TREE_SCOPE_CHILDREN, _uia.CreateTrueCondition())
        except Exception:
            return None
        ids = set()
        if arr is None:
            return ids
        for i in range(arr.Length):
            try:
                ids.add(tuple(arr.GetElement(i).GetRuntimeId()))
            except Exception:
                pass
        return ids

    def _scan_cached_children(self, element, tree_list, depth, clip_stack, allowed_ids=None):
        """Iterate an element's cached children (in-process, no COM calls).
        allowed_ids, when given, restricts the walk to live-confirmed children
        (see the zero-size-container disambiguation)."""
        try:
            children = element.GetCachedChildren()
        except Exception:
            return
        if not children:
            return
        for i in range(children.Length):
            try:
                child = children.GetElement(i)
            except Exception:
                continue
            if allowed_ids is not None:
                try:
                    if tuple(child.GetCachedPropertyValue(30000)) not in allowed_ids:
                        continue
                except Exception:
                    pass
            self._scan_cached_recursive(child, tree_list, depth, clip_stack)

    def _cached_child_clip_stack(self, element, control_type, clip_stack, node_rect):
        """Cached-walk variant of _child_clip_stack. ScrollPattern presence is
        already in the cache, so entries never need a live probe."""
        if control_type in self._CLIP_BOUNDARY_TYPES:
            return []
        # The old clipping walk used ControlViewWalker, which never visits
        # raw-view-only nodes (IsControlElement=False, e.g. Chromium wrapper
        # containers with misleading rects) - keep them out of the stack.
        try:
            if not element.GetCachedPropertyValue(30016):  # IsControlElement
                return clip_stack
        except Exception:
            pass
        try:
            scroll = bool(element.GetCachedPropertyValue(30034))  # IsScrollPatternAvailable
        except Exception:
            scroll = False
        # Label parity with _get_clipping_ancestors: raw Name, or the control
        # type when the name is empty - NOT the fallback-chain-resolved name.
        try:
            name = element.CachedName or control_type
        except Exception:
            name = control_type
        return self._push_clip_entry(clip_stack, control_type, node_rect, name, None, scroll)

    def _cached_window_text(self, element, name, wrapped_element):
        """Mirror BaseWrapper.window_text() (element_info.rich_text). When the
        cache shows a ClassName or an available TextPattern, delegate to the
        real live window_text() - its own class_name/TextPattern logic then
        decides between document text and name. Only when both cached signals
        are absent (where rich_text short-circuits to name anyway) is the live
        call skipped. Chromium quirk: CachedClassName is often empty while the
        live ClassName is not, which is why availability alone must trigger
        the delegation."""
        try:
            class_name = element.CachedClassName or ""
        except Exception:
            class_name = ""
        try:
            has_text_pattern = bool(element.GetCachedPropertyValue(30040))  # IsTextPatternAvailable
        except Exception:
            has_text_pattern = False
        if (not class_name and not has_text_pattern) or wrapped_element is None:
            return name or ""
        try:
            return wrapped_element.window_text() or ""
        except Exception:
            return name or ""

    def _cached_value(self, element, control_type, name, wrapped_element=None, remapped_from_group=False):
        """Value/state mirroring the live fallback chains exactly:
        - real Edit: ValuePattern, else window_text (EditWrapper.get_value)
        - Group remapped to Edit: plain UIAWrapper has no get_value, so the
          live chain lands on window_text()
        - ComboBox/ListItem: their wrappers have no get_value either -
          window_text()
        - Group: both live attempts raise AttributeError -> always ""
        - Document: window_text() - document text on rich-text controls
          (Notepad/WordPad), name on empty-ClassName providers (Chromium)
        - CheckBox: ToggleState only when TogglePattern is supported (the
          cached read substitutes a default when unsupported, so guard it)
        """
        if control_type == "Document":
            value = self._cached_window_text(element, name, wrapped_element)
            return value[:100] + "..." if len(value) > 100 else value
        if control_type == "Edit":
            if not remapped_from_group:
                try:
                    if element.GetCachedPropertyValue(30043):  # IsValuePatternAvailable
                        return element.GetCachedPropertyValue(30045) or ""  # Value.Value
                except Exception:
                    pass
            return self._cached_window_text(element, name, wrapped_element)
        if control_type in ("ComboBox", "ListItem"):
            return self._cached_window_text(element, name, wrapped_element)
        if control_type == "Group":
            return ""
        if control_type == "CheckBox":
            try:
                if not element.GetCachedPropertyValue(30041):  # IsTogglePatternAvailable
                    return ""
                state = element.GetCachedPropertyValue(30086)  # Toggle.ToggleState
                return "Checked" if state == 1 else "Unchecked"
            except Exception:
                return ""
        return None

    def _scan_cached_recursive(self, element, tree_list, depth, clip_stack):
        """Cached-subtree mirror of _scan_element_recursive: identical
        filtering rules, but every property read is an in-process cached
        access instead of a cross-process COM round trip."""
        try:
            ct_id = element.CachedControlType
            control_type = _UIA_ID_TO_NAME.get(ct_id, "Unknown")

            # Special handling for Group elements that are actually textboxes
            remapped_from_group = False
            if control_type == "Group":
                try:
                    if element.CachedIsControlElement and element.CachedIsKeyboardFocusable:
                        control_type = "Edit"
                        remapped_from_group = True
                except Exception:
                    pass

            try:
                r = element.CachedBoundingRectangle
                node_rect = Rect(r.left, r.top, r.right, r.bottom)
            except Exception:
                node_rect = None

            # Zero-size containers need disambiguation. The cache serializer
            # includes subtrees live enumeration prunes: Chrome's collapsed
            # bookmarks bar is a 0x0 container whose children are stale
            # ghosts (IsOffscreen=False, full-size rects) - those must go.
            # But a 0x0 rect is ALSO how structural wrappers look: the XAML
            # 'Popup' node hosting the Start menu power flyout is 0x0 while
            # its menu items are real and visible. Ground truth is live
            # enumeration itself: ghosts have no live children. One live
            # FindAll(children) on these rare nodes decides, and the walk
            # then descends only into live-confirmed children.
            # (depth > 0: never drop the scan root itself.)
            allowed_child_ids = None
            if depth > 0 and node_rect is not None:
                if (node_rect.right - node_rect.left <= 0 or
                        node_rect.bottom - node_rect.top <= 0):
                    allowed_child_ids = self._live_child_runtime_ids(element)
                    if not allowed_child_ids:
                        return

            if control_type in self.config and self.config[control_type]["track"]:
                should_track_element = True

                is_enabled = bool(element.CachedIsEnabled)
                if not is_enabled:
                    if control_type == "Button" and node_rect is not None:
                        self.disabled_button_rects.append(node_rect)
                    should_track_element = False

                if should_track_element and element.CachedIsOffscreen:
                    should_track_element = False

                # Only enforce keyboard_focusable for browsers (same rule as live path)
                if should_track_element and self.config[control_type].get("keyboard_focusable", False) and not self.config[control_type].get("is_enabled_check", False):
                    if self._is_browser:
                        try:
                            if not element.CachedIsKeyboardFocusable:
                                should_track_element = False
                        except Exception:
                            pass

                if should_track_element and depth == 0 and control_type == "Button":
                    try:
                        btn_name = (element.CachedName or "").lower()
                        if any(word in btn_name for word in ["minimize", "minimise", "maximize", "maximise", "close"]):
                            should_track_element = False
                    except Exception:
                        pass

                # Text elements: only track if AriaRole is "heading"
                if should_track_element and control_type == "Text":
                    try:
                        if element.GetCachedPropertyValue(30101) != "heading":
                            should_track_element = False
                    except Exception:
                        should_track_element = False

                if should_track_element:
                    if node_rect is None:
                        should_track_element = False
                    else:
                        width = node_rect.right - node_rect.left
                        height = node_rect.bottom - node_rect.top
                        if width < 10 or height < 10:
                            should_track_element = False

                if should_track_element:
                    # Get name using fallback chain from config
                    name = ""
                    for prop in self.config[control_type].get("fallback", ["name"]):
                        try:
                            if prop == "name":
                                name = element.CachedName or ""
                            elif prop == "automation_id":
                                name = element.CachedAutomationId or ""
                            elif prop == "class_name":
                                name = element.CachedClassName or ""
                            elif prop == "legacy_description":
                                name = element.GetCachedPropertyValue(30094) or ""
                        except Exception:
                            continue
                        if name and name.strip():
                            break

                    # Remove Unicode control characters like U+200E
                    name = ''.join(char for char in name if ord(char) >= 32 and ord(char) != 0x200E)

                    if not name or name.strip() == "":
                        should_track_element = False

                if should_track_element:
                    # Fully-clipped elements (e.g. bookmark-bar overflow laid
                    # out past the window edge) are invisible to the user:
                    # exclude them from tree, mapping and screenshot, like the
                    # raw-UIA scanner always has.
                    clippers = self._clippers_from_stack(node_rect, clip_stack)
                    visibility_status, visible_rect, clipped_by = self._compute_visibility(None, node_rect, clippers)
                    if visibility_status == "hidden":
                        should_track_element = False

                if should_track_element:
                    self.element_index += 1

                    # The controller acts through a pywinauto wrapper - build
                    # it only for tracked elements (a few COM calls each);
                    # untracked nodes never pay wrapper-creation cost. Built
                    # before the value so rich-text reads can reuse it.
                    wrapped_element = None
                    try:
                        wrapped_element = UIAWrapper(uia_element_info.UIAElementInfo(element))
                    except Exception:
                        wrapped_element = None

                    value = self._cached_value(element, control_type, name, wrapped_element, remapped_from_group)

                    aria_role = ""
                    try:
                        aria_role = element.GetCachedPropertyValue(30101) or ""
                    except Exception:
                        aria_role = ""

                    self.elements_mapping[str(self.element_index)] = {
                        'element': wrapped_element,
                        'rect': node_rect,
                        'visible_rect': visible_rect,
                        'name': name,
                        'aria_role': aria_role,
                        'type': control_type,
                        'value': value,
                        'visibility': visibility_status,
                        'clipped_by': clipped_by
                    }

                    if SCREENSHOT:
                        self.elements_to_draw.append({
                            "rect": node_rect,
                            "index": self.element_index,
                            "depth": depth,
                            "visibility": visibility_status
                        })

                    # Add actions hint for ListItem in non-browser apps
                    actions = None
                    if control_type == "ListItem" and not self._is_browser:
                        actions = "{click: select, double_click: open}"

                    element_info = {
                        "element": wrapped_element,
                        "name": name,
                        "aria_role": aria_role,
                        "type": control_type,
                        "active": is_enabled,
                        "index": self.element_index,
                        "value": value,
                        "actions": actions,
                        "visibility": visibility_status,
                        "clipped_by": clipped_by,
                        "rect": node_rect,
                        "visible_rect": visible_rect,
                        "children": []
                    }

                    tree_list.append(element_info)

                    if control_type not in self.found_elements:
                        self.found_elements[control_type] = []
                    self.found_elements[control_type].append(element_info)

                    child_stack = self._cached_child_clip_stack(element, control_type, clip_stack, node_rect)
                    self._scan_cached_children(element, element_info["children"], depth + 1, child_stack, allowed_child_ids)

                    # Browser Document: split children into top/second layer when popup detected
                    if control_type == "Document" and self._is_browser and element_info["children"]:
                        if self._detect_browser_popup(element_info["children"]):
                            popup_types = {"ListItem", "MenuItem", "Menu"}
                            element_info["browser_top_layer"] = [c for c in element_info["children"] if c["type"] in popup_types]
                            element_info["browser_second_layer"] = [c for c in element_info["children"] if c["type"] not in popup_types]
                            element_info["children"] = []  # Moved into layers

                if not should_track_element:
                    child_stack = self._cached_child_clip_stack(element, control_type, clip_stack, node_rect)
                    self._scan_cached_children(element, tree_list, depth + 1, child_stack, allowed_child_ids)
            else:
                # Even if we don't track this element, scan its children
                child_stack = self._cached_child_clip_stack(element, control_type, clip_stack, node_rect)
                self._scan_cached_children(element, tree_list, depth + 1, child_stack, allowed_child_ids)

        except Exception:
            pass

    def _child_clip_stack(self, element, control_type, clip_stack, node_rect, skip_visibility):
        """Clip stack for this node's children: reset at Window/Document
        boundaries (matching where _get_clipping_ancestors' upward walk stops),
        otherwise push this node as a potential clipping container."""
        if skip_visibility:
            return clip_stack
        if control_type in self._CLIP_BOUNDARY_TYPES:
            return []
        # Control view only, matching the old ControlViewWalker parent chain
        try:
            if not element.element_info.element.CurrentIsControlElement:
                return clip_stack
        except Exception:
            pass
        if node_rect is None:
            try:
                node_rect = element.rectangle()
            except Exception:
                return clip_stack
        # Name left unset: _clippers_from_stack lazily reads element_info.name
        # (the same source _get_clipping_ancestors used), only when this node
        # actually clips a descendant.
        return self._push_clip_entry(clip_stack, control_type, node_rect, None, element)

    def _scan_element_recursive(self, element, tree_list, depth, skip_visibility=False, clip_stack=None):
        """Recursively scan elements to build tree structure"""
        if clip_stack is None:
            clip_stack = []
        try:
            node_rect = None  # rectangle() is fetched at most once per node
            control_type = element.element_info.control_type
            # Special handling for Group elements that are actually textboxes
            if control_type == "Group":
                try:
                    # Check if this Group has input field properties:
                    # IsControlElement = True AND IsKeyboardFocusable = True
                    is_control = False
                    is_keyboard_focusable = False
                    
                    # Get IsKeyboardFocusable from element wrapper
                    if hasattr(element, 'is_keyboard_focusable'):
                        is_keyboard_focusable = element.is_keyboard_focusable()
                    
                    # Get IsControlElement from raw UIA element
                    try:
                        raw_element = element.element_info.element
                        is_control = raw_element.CurrentIsControlElement
                    except:
                        is_control = element.element_info.visible and element.element_info.enabled
                    
                    if is_control and is_keyboard_focusable:
                        control_type = "Edit"
                except:
                    pass  # If properties not accessible, keep as Group
            
            # Check if this element type is in our config and is enabled
            if control_type in self.config and self.config[control_type]["track"]:
                should_track_element = True

                is_enabled = element.is_enabled()
                if not is_enabled:
                    if control_type == "Button":
                        try:
                            node_rect = element.rectangle()
                            self.disabled_button_rects.append(node_rect)
                        except:
                            pass
                    should_track_element = False
                
                if should_track_element and not element.is_visible():
                    should_track_element = False
                
                # Only enforce keyboard_focusable for browsers (Chrome, Edge, etc.)
                # Native apps handle focus differently, so we skip this check for them
                if should_track_element and self.config[control_type].get("keyboard_focusable", False) and not self.config[control_type].get("is_enabled_check", False):
                    if self._is_browser:
                        # Browser detected - enforce keyboard focusable check
                        is_keyboard_focusable = False
                        try:
                            raw_element = element.element_info.element
                            is_keyboard_focusable = raw_element.CurrentIsKeyboardFocusable
                        except:
                            try:
                                if hasattr(element, 'is_keyboard_focusable'):
                                    is_keyboard_focusable = element.is_keyboard_focusable()
                            except:
                                try:
                                    raw_element = element.element_info.element
                                    is_keyboard_focusable = bool(raw_element.GetCurrentPropertyValue(30009))
                                except:
                                    pass
                        if not is_keyboard_focusable:
                            should_track_element = False
                    # else: Native app - skip keyboard_focusable check entirely
                
                if should_track_element and depth == 0 and control_type == "Button":
                    try:
                        name = element.element_info.name.lower()
                        if any(word in name for word in ["minimize", "minimise", "maximize", "maximise", "close"]):
                            should_track_element = False
                    except:
                        pass

                # Text elements: only track if AriaRole is "heading"
                if should_track_element and control_type == "Text":
                    try:
                        raw_element = element.element_info.element
                        aria_role = raw_element.CurrentAriaRole
                        if aria_role != "heading":
                            should_track_element = False
                    except:
                        should_track_element = False
                
                if should_track_element:
                    # Check element size - filter out very small elements
                    try:
                        rect = element.rectangle()
                        node_rect = rect
                        width = rect.right - rect.left
                        height = rect.bottom - rect.top

                        # Ignore elements smaller than 10x10 pixels (they're usually not useful)
                        if width < 10 or height < 10:
                            should_track_element = False
                    except:
                        should_track_element = False
                
                if should_track_element:
                    # Get name using fallback chain from config
                    name = ""
                    for prop in self.config[control_type].get("fallback", ["name"]):
                        try:
                            if prop == "name":
                                name = element.element_info.name
                            elif prop == "automation_id":
                                name = element.automation_id()
                            elif prop == "class_name":
                                name = element.element_info.class_name
                            elif prop == "legacy_description":
                                # LegacyIAccessiblePattern.Description (UIA Property ID: 30094)
                                raw_element = element.element_info.element
                                name = raw_element.GetCurrentPropertyValue(30094) or ""
                        except:
                            continue
                        if name and name.strip():
                            break

                    # Remove Unicode control characters like U+200E (Left-to-Right Mark)
                    name = ''.join(char for char in name if ord(char) >= 32 and ord(char) != 0x200E)
                    
                    # ========== FILTER: Skip elements with empty names ==========
                    # This filter removes elements that have no name from the tree and screenshot
                    # To disable this filter, comment out the next 2 lines
                    if not name or name.strip() == "":
                        should_track_element = False
                    # ============================================================
                    
                if should_track_element:
                    # rect was already fetched by the size check above - reuse it
                    if skip_visibility:
                        visibility_status = 'full'
                        visible_rect = rect
                        clipped_by = None
                    else:
                        # Classify clippers from the rects the recursion already
                        # carried down instead of re-walking the parent chain.
                        clippers = self._clippers_from_stack(rect, clip_stack)
                        visibility_status, visible_rect, clipped_by = self._compute_visibility(element, rect, clippers)
                        # Fully-clipped elements are invisible to the user -
                        # exclude them like the raw-UIA scanner always has.
                        if visibility_status == "hidden":
                            should_track_element = False

                if should_track_element:
                    self.element_index += 1

                    # Get the value/state based on element type
                    value = None
                    if control_type == "Edit":
                        try:
                            try:
                                value = element.element_info.current_value
                            except:
                                try:
                                    value = element.get_value()
                                except:
                                    value = element.window_text()
                        except:
                            value = ""
                    elif control_type == "CheckBox":
                        try:
                            value = "Checked" if element.get_toggle_state() == 1 else "Unchecked"
                        except:
                            value = ""
                    elif control_type in ["ComboBox", "ListItem"]:
                        try:
                            try:
                                value = element.element_info.current_value
                            except:
                                try:
                                    value = element.get_value()
                                except:
                                    value = element.window_text()
                        except:
                            value = ""
                    elif control_type == "Document":
                        try:
                            text = element.window_text()
                            value = text[:100] + "..." if len(text) > 100 else text
                        except:
                            value = ""
                    elif control_type == "Group":
                        try:
                            try:
                                value = element.element_info.current_value
                            except:
                                try:
                                    value = element.get_value()
                                except:
                                    value = ""
                        except:
                            value = ""
                    
                    # Get AriaRole property (UIA Property ID: 30101)
                    aria_role = ""
                    try:
                        raw_element = element.element_info.element
                        aria_role = raw_element.CurrentAriaRole
                    except:
                        try:
                            # Fallback: Try using GetCurrentPropertyValue with AriaRole property ID
                            raw_element = element.element_info.element
                            aria_role = raw_element.GetCurrentPropertyValue(30101)
                        except:
                            aria_role = ""
                    
                    self.elements_mapping[str(self.element_index)] = {
                        'element': element,
                        'rect': rect,
                        'visible_rect': visible_rect,  # Actual visible rectangle (clipped by ancestors)
                        'name': name,
                        'aria_role': aria_role,
                        'type': control_type,
                        'value': value,
                        'visibility': visibility_status,
                        'clipped_by': clipped_by  # Name of the container clipping this element
                    }
                    
                    if SCREENSHOT:
                        self.elements_to_draw.append({
                            "rect": rect,
                            "index": self.element_index,
                            "depth": depth,
                            "visibility": visibility_status
                        })
                    
                    # Add actions hint for ListItem in non-browser apps
                    actions = None
                    if control_type == "ListItem" and not self._is_browser:
                        actions = "{click: select, double_click: open}"
                    
                    element_info = {
                        "element": element,
                        "name": name,
                        "aria_role": aria_role,
                        "type": control_type,
                        "active": is_enabled,
                        "index": self.element_index,
                        "value": value,
                        "actions": actions,
                        "visibility": visibility_status,
                        "clipped_by": clipped_by,
                        "rect": rect,  # Store original full rect for deduplication
                        "visible_rect": visible_rect,
                        "children": []
                    }
                    
                    tree_list.append(element_info)
                    
                    if control_type not in self.found_elements:
                        self.found_elements[control_type] = []
                    self.found_elements[control_type].append(element_info)
                    
                    child_stack = self._child_clip_stack(element, control_type, clip_stack, rect, skip_visibility)
                    for child in element.children():
                        self._scan_element_recursive(child, element_info["children"], depth + 1, skip_visibility, child_stack)

                    # Browser Document: split children into top/second layer when popup detected
                    if control_type == "Document" and self._is_browser and element_info["children"]:
                        if self._detect_browser_popup(element_info["children"]):
                            popup_types = {"ListItem", "MenuItem", "Menu"}
                            element_info["browser_top_layer"] = [c for c in element_info["children"] if c["type"] in popup_types]
                            element_info["browser_second_layer"] = [c for c in element_info["children"] if c["type"] not in popup_types]
                            element_info["children"] = []  # Moved into layers
                
                if not should_track_element:
                    child_stack = self._child_clip_stack(element, control_type, clip_stack, node_rect, skip_visibility)
                    for child in element.children():
                        self._scan_element_recursive(child, tree_list, depth + 1, skip_visibility, child_stack)
            else:
                # Even if we don't track this element, scan its children
                child_stack = self._child_clip_stack(element, control_type, clip_stack, None, skip_visibility)
                for child in element.children():
                    self._scan_element_recursive(child, tree_list, depth + 1, skip_visibility, child_stack)

        except:
            pass

    def get_elements_mapping(self):
        """Get the elements mapping for controller
        Returns: dict mapping index to element info
        """
        return self.elements_mapping

    def print_summary(self):
        """Print summary of found elements"""
        # Silent - no output
        pass
    
    def save_to_file(self):
        """Save found elements to file in hierarchical structure with layers"""
        if DEBUG:
            filename = f"debug/element/ui_elements_{int(time.time())}.txt"
            os.makedirs(os.path.dirname(filename), exist_ok=True)
            with open(filename, "w", encoding="utf-8") as f:
                # Write top layer
                f.write("<top_layer>\n")
                if self.top_layer_info:
                    layer_name = _xml_escape(self.top_layer_info["name"])
                    layer_type = self.top_layer_info["type"]
                    f.write(f'  <application name="{layer_name}" type="{layer_type}" />\n')
                else:
                    f.write('  <application name="Desktop" type="app" />\n')
                self._write_tree_recursive(f, self.top_layer_tree, 1)
                f.write("</top_layer>\n\n")
                
                # Write second layer (if exists)
                if self.second_layer_info and self.second_layer_tree:
                    f.write("<second_layer>\n")
                    layer_name = _xml_escape(self.second_layer_info["name"])
                    layer_type = self.second_layer_info["type"]
                    f.write(f'  <application name="{layer_name}" type="{layer_type}" />\n')
                    self._write_tree_recursive(f, self.second_layer_tree, 1)
                    f.write("</second_layer>\n\n")
                
                # Write taskbar - or the same off-screen marker get_scan_data emits
                if self.taskbar_visible:
                    f.write("<taskbar>\n")
                    self._write_tree_recursive(f, self.taskbar_tree, 1)
                    f.write("</taskbar>\n")
                else:
                    reason = _xml_escape(self.taskbar_hidden_reason or "")
                    f.write(f'<taskbar visibility="hidden" reason="{reason}" />\n')

    def _write_tree_recursive(self, file, tree_list, depth):
        """Recursively write tree structure with indentation in XML format"""
        indent = "  " * depth  # 2 spaces per level for cleaner look
        
        for item in tree_list:
            # OCR_TEXT elements use a distinct format
            if item.get("source") == "ocr":
                text = _xml_escape(item['name'])
                file.write(f'{indent}[{item["index"]}]<Line="{text}", type="OCR_TEXT", active="True", visibility="full" />\n')
                if item.get("children"):
                    self._write_tree_recursive(file, item["children"], depth + 1)
                continue

            # Escape special characters in name for XML
            name = _xml_escape(item['name'])
            
            # Get aria_role - default to empty if not present
            aria_role = _xml_escape(item.get('aria_role', ''))
            
            # Get visibility status - default to 'full' if not present (for backward compatibility)
            visibility = item.get('visibility', 'full')
            
            # Get clipped_by - only include if visibility is not full
            clipped_by = item.get('clipped_by', None)
            clipped_by_attr = ""
            if clipped_by and visibility != "full":
                clipped_by_attr = f', clipped_by="{_xml_escape(clipped_by)}"'
            
            # Build actions attribute if present
            actions_attr = ""
            if item.get("actions"):
                actions_attr = f', actions="{item["actions"]}"'
            # Write element in the new format: [index]<element name="...", AriaRole="...", type="...", active="...", visibility="...", clipped_by="..." />
            # Include AriaRole right after name
            # Include valuePattern.value attribute if it exists and is not empty
            if item.get("value") and item["value"]:
                value = _xml_escape(item["value"])
                if aria_role:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}"{actions_attr}, visibility="{visibility}"{clipped_by_attr} />\n')
                else:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", valuePattern.value="{value}", type="{item["type"]}", active="{item["active"]}"{actions_attr}, visibility="{visibility}"{clipped_by_attr} />\n')
            else:
                if aria_role:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", AriaRole="{aria_role}", type="{item["type"]}", active="{item["active"]}"{actions_attr}, visibility="{visibility}"{clipped_by_attr} />\n')
                else:
                    file.write(f'{indent}[{item["index"]}]<element name="{name}", type="{item["type"]}", active="{item["active"]}"{actions_attr}, visibility="{visibility}"{clipped_by_attr} />\n')
            
            # Recursively write children or browser layers
            if item.get("browser_top_layer") is not None:
                file.write(f'{indent}  <top_layer_browser>\n')
                self._write_tree_recursive(file, item["browser_top_layer"], depth + 2)
                file.write(f'{indent}  </top_layer_browser>\n')
                file.write(f'{indent}  <second_layer_browser>\n')
                self._write_tree_recursive(file, item["browser_second_layer"], depth + 2)
                file.write(f'{indent}  </second_layer_browser>\n')
            elif item["children"]:
                self._write_tree_recursive(file, item["children"], depth + 1)
    
    def get_scan_data(self):
        """Get scan data for use by AgentService
        Returns: tuple (element_tree_text, annotated_image_base64, uac_detected)
        """
        # Generate element tree text with layer format
        element_tree_text = ""
        
        # Write top layer
        element_tree_text += "<top_layer>\n"
        if self.top_layer_info:
            layer_name = _xml_escape(self.top_layer_info["name"])
            layer_type = self.top_layer_info["type"]
            element_tree_text += f'  <application name="{layer_name}" type="{layer_type}" />\n'
        else:
            element_tree_text += '  <application name="Desktop" type="app" />\n'
        element_tree_text += self._get_tree_text_recursive(self.top_layer_tree, 1)
        element_tree_text += "</top_layer>\n\n"
        
        # Write second layer (if exists)
        if self.second_layer_info and self.second_layer_tree:
            element_tree_text += "<second_layer>\n"
            layer_name = _xml_escape(self.second_layer_info["name"])
            layer_type = self.second_layer_info["type"]
            element_tree_text += f'  <application name="{layer_name}" type="{layer_type}" />\n'
            element_tree_text += self._get_tree_text_recursive(self.second_layer_tree, 1)
            element_tree_text += "</second_layer>\n\n"
        
        # Write taskbar. One that is not on screen (covered by a full-screen
        # window, auto-hidden, explorer gone) is announced rather than listed:
        # none of its buttons are in the screenshot, and an empty section would
        # read like a failed scan.
        if self.taskbar_visible:
            element_tree_text += "<taskbar>\n"
            element_tree_text += self._get_tree_text_recursive(self.taskbar_tree, 1)
            element_tree_text += "</taskbar>\n"
        else:
            reason = _xml_escape(self.taskbar_hidden_reason or "")
            element_tree_text += f'<taskbar visibility="hidden" reason="{reason}" />\n'

        # Capture and annotate screenshot, get as base64
        annotated_image_base64 = None
        # Not conditioned on elements_to_draw: the always-scanned taskbar used to
        # keep it non-empty. With the taskbar gated out, a full-screen app with no
        # UIA elements and no OCR text must still send its image, trip UAC
        # detection and refresh the frontend preview.
        if SCREENSHOT:
            # Capture full screen - may fail if UAC secure desktop is active
            try:
                screenshot = ImageGrab.grab()
            except OSError as e:
                # UAC secure desktop detected - screen grab failed
                print("🔒 UAC secure desktop detected - requesting agent decision")
                return None, None, True  # uac_detected = True
            
            # Keep a copy of plain screenshot for frontend when DEBUG=False
            plain_screenshot = screenshot.copy()
            
            # Downscale to the final payload size FIRST, then annotate. Drawing
            # before the resize put every box and label through LANCZOS as well:
            # an 11px label arrived at ~6px and its 1px halo averaged away to
            # nothing, which is what made the index numbers unreadable. Annotating
            # afterwards leaves them pixel-exact at their intended size - the
            # screenshot gets compressed, the annotations don't - and drawing on
            # the smaller canvas is cheaper too.
            src_w, src_h = screenshot.size
            scale = min(1.0,
                        LLM_IMAGE_MAX_EDGE / max(src_w, src_h),
                        (LLM_IMAGE_MAX_PIXELS / (src_w * src_h)) ** 0.5)
            if scale < 1.0:
                # Floor, not round: rounding both sides up can push the product
                # back over MAX_PIXELS, so the cap would not actually hold.
                screenshot = screenshot.resize(
                    (max(1, int(src_w * scale)), max(1, int(src_h * scale))),
                    Image.Resampling.LANCZOS)

            # Convert to RGB before annotating: drawing into a palette-mode image
            # would quantise the label colour, and RGB keeps the PNG predictable.
            if screenshot.mode in ('RGBA', 'LA', 'P'):
                rgb_screenshot = Image.new('RGB', screenshot.size, (255, 255, 255))
                rgb_screenshot.paste(screenshot, mask=screenshot.split()[-1] if screenshot.mode == 'RGBA' else None)
                screenshot = rgb_screenshot
            elif screenshot.mode != 'RGB':
                screenshot = screenshot.convert('RGB')

            # Now draw annotations on the resized screenshot
            draw = ImageDraw.Draw(screenshot)

            # Prefer a BOLD face: thicker strokes carry more ink per glyph at the
            # same size, so the digits stay readable at no extra footprint.
            # Regular arial stays last as a fallback.
            font = None
            try:
                for font_name in ["arialbd.ttf", "tahomabd.ttf", "verdanab.ttf", "arial.ttf"]:
                    try:
                        font = ImageFont.truetype(font_name, LABEL_FONT_SIZE)
                        break
                    except:
                        continue
            except:
                font = None

            # PIL renders text strokes through FreeType, which the built-in bitmap
            # fallback font does not have - asking for one there raises. Only stroke
            # when a real TrueType face loaded.
            stroke_w = LABEL_STROKE if font is not None else 0
            
            # Draw each bounding box with index labels
            for item in self.elements_to_draw:
                rect = item["rect"]
                index = str(item["index"])
                is_ocr = item.get("source") == "ocr"
                
                # Rects are in native screen coordinates - map them into the
                # resized image, since the drawing now happens post-downscale.
                left = round(rect.left * scale)
                top = round(rect.top * scale)
                right = round(rect.right * scale)
                bottom = round(rect.bottom * scale)

                # OCR boxes get slight padding for breathing room
                if is_ocr:
                    pad = 3
                    box = (left - pad, top - pad, right + pad, bottom + pad)
                else:
                    box = (left, top, right, bottom)
                
                draw.rectangle(box, outline=BOX_COLOR, width=2)

                label = f"[{index}]"
                if font:
                    # Measure WITH the stroke - it widens the glyphs, and the
                    # on-canvas clamp below has to account for that or a stroked
                    # label near an edge gets clipped.
                    bbox = draw.textbbox((0, 0), label, font=font,
                                         stroke_width=stroke_w)
                    text_width = bbox[2] - bbox[0]
                    text_height = bbox[3] - bbox[1]
                else:
                    text_width = len(label) * 8
                    text_height = 15
                
                if is_ocr:
                    # OCR: position label above the box
                    text_x = box[0]
                    text_y = box[1] - text_height - 2
                else:
                    # UIA: position label at top-left corner inside the box
                    text_x = left + 4
                    text_y = top + 3

                # Keep the label on canvas. The chip is opaque, so one drawn past
                # an edge vanishes entirely rather than merely being clipped.
                text_x = max(2, min(text_x, screenshot.width - text_width - 3))
                text_y = max(2, min(text_y, screenshot.height - text_height - 3))

                # ========== STYLING: magenta text with a thin dark rim ==========
                # The rim is a stroke around the glyph shapes, NOT a filled chip:
                # the UI underneath stays visible between and inside the digits.
                # It supplies the luma contrast magenta lacks on its own (magenta
                # sits at mid-brightness, so it vanishes against mid-tone UI).
                draw.text((text_x, text_y), label, fill=NUMBER_COLOR, font=font,
                          stroke_width=stroke_w, stroke_fill=LABEL_STROKE_COLOR)
                # ================================================================
            
            # Single encode - these exact bytes are the LLM payload. 4:4:4
            # chroma (subsampling=0) keeps the annotation digits crisp; see the
            # LLM_IMAGE_* constants for why JPEG replaced lossless PNG here.
            buffered_annotated = io.BytesIO()
            screenshot.save(buffered_annotated, format=LLM_IMAGE_FORMAT,
                            quality=LLM_IMAGE_QUALITY,
                            subsampling=LLM_IMAGE_SUBSAMPLING)
            annotated_image_bytes = buffered_annotated.getvalue()
            annotated_image_base64 = base64.b64encode(annotated_image_bytes).decode('utf-8')

            # Save to debug folder if DEBUG is enabled. Writes the SAME bytes that
            # go to the LLM, so the dump is byte-identical by construction rather
            # than by two encodes happening to agree - and costs no second encode.
            if DEBUG:
                timestamp = int(time.time())
                os.makedirs("debug/screenshot", exist_ok=True)
                filename_annotated = f"debug/screenshot/annotated_screenshot_{timestamp}.jpg"
                with open(filename_annotated, "wb") as f:
                    f.write(annotated_image_bytes)

            # Send image to frontend if FRONTEND is enabled and callback exists
            # DEBUG=True: annotated image, DEBUG=False: plain image (production)
            if FRONTEND and self.frontend_callback:
                if DEBUG:
                    self.frontend_callback(annotated_image_base64)
                else:
                    # Prepare plain screenshot for production frontend. This is a
                    # human-facing preview, so it keeps the original 1920/q100
                    # fidelity and does NOT follow the LLM payload settings.
                    max_dimension = FRONTEND_IMAGE_MAX_DIMENSION
                    width, height = plain_screenshot.size
                    if width > max_dimension or height > max_dimension:
                        if width > height:
                            new_width = max_dimension
                            new_height = int(height * (max_dimension / width))
                        else:
                            new_height = max_dimension
                            new_width = int(width * (max_dimension / height))
                        plain_screenshot = plain_screenshot.resize((new_width, new_height), Image.Resampling.LANCZOS)
                    
                    if plain_screenshot.mode in ('RGBA', 'LA', 'P'):
                        rgb_plain = Image.new('RGB', plain_screenshot.size, (255, 255, 255))
                        rgb_plain.paste(plain_screenshot, mask=plain_screenshot.split()[-1] if plain_screenshot.mode == 'RGBA' else None)
                        plain_screenshot = rgb_plain
                    elif plain_screenshot.mode != 'RGB':
                        plain_screenshot = plain_screenshot.convert('RGB')
                    
                    buffered_plain = io.BytesIO()
                    # optimize=True dropped: it is lossless (Huffman tables only), so
                    # the preview is pixel-identical - just encoded faster.
                    plain_screenshot.save(buffered_plain, format="JPEG",
                                          quality=FRONTEND_IMAGE_QUALITY)
                    plain_image_base64 = base64.b64encode(buffered_plain.getvalue()).decode('utf-8')
                    self.frontend_callback(plain_image_base64)
        
        return element_tree_text, annotated_image_base64, False  # uac_detected = False
    
    def _get_tree_text_recursive(self, tree_list, depth):
        """Helper method to generate tree text recursively"""
        result = ""
        indent = "  " * depth
        
        for item in tree_list:
            # OCR_TEXT elements use a distinct format
            if item.get("source") == "ocr":
                text = _xml_escape(item['name'])
                result += f'{indent}[{item["index"]}]<Line="{text}", type="OCR_TEXT", active="True", visibility="full" />\n'
                if item["children"]:
                    result += self._get_tree_text_recursive(item["children"], depth + 1)
                continue

            name = _xml_escape(item['name'])
            
            # Get aria_role - default to empty if not present
            aria_role = _xml_escape(item.get('aria_role', ''))
            
            # Get visibility status - default to 'full' if not present (for backward compatibility)
            visibility = item.get('visibility', 'full')
            
            # Get clipped_by - only include if visibility is not full
            clipped_by = item.get('clipped_by', None)
            clipped_by_attr = ""
            if clipped_by and visibility != "full":
                clipped_by_attr = f', clipped_by="{_xml_escape(clipped_by)}"'
            
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
            
            # Recursively write children or browser layers
            if item.get("browser_top_layer") is not None:
                result += f'{indent}  <top_layer_browser>\n'
                result += self._get_tree_text_recursive(item["browser_top_layer"], depth + 2)
                result += f'{indent}  </top_layer_browser>\n'
                result += f'{indent}  <second_layer_browser>\n'
                result += self._get_tree_text_recursive(item["browser_second_layer"], depth + 2)
                result += f'{indent}  </second_layer_browser>\n'
            elif item["children"]:
                result += self._get_tree_text_recursive(item["children"], depth + 1)
        
        return result

# ========== MAIN PROGRAM ==========
def main():
    print("Starting scan now!\n")

    # Create scanner with configuration
    scanner = UIElementScanner(ELEMENT_CONFIG)
    
    # Scan for elements
    scanner.scan_elements()
    
    if DEBUG:
        print("Scan complete. Check debug/element/ for element tree and debug/screenshot/ for screenshots.")
    else:
        print("Scan complete. Data sent directly to LLM without saving.")

if __name__ == "__main__":
    main()
