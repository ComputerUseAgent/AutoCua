#!/usr/bin/env python3
"""
Linux UI Element Scanner for AutoCua.

Reads the on-screen UI through AT-SPI2 (the desktop accessibility bus) via
PyGObject, and captures the screen through the XDG desktop portal, so it works
on both Wayland (GNOME) and X11.

Tree reading normally goes through the fast bulk reader below — whole app
trees in one D-Bus call each via org.a11y.atspi.Cache, extents/text as
pipelined async waves — with the per-node synchronous walk as the
always-available fallback (and the reference for what the fast path must
produce). See FASTSCAN below.

The agent drives one scanner per platform behind a shared interface, which
this class implements:
    - UIElementScanner(config, frontend_callback=None)
    - scanner.scan_elements()
    - scanner.get_scan_data()        → (element_tree_text, annotated_image_base64, uac_detected)
    - scanner.get_elements_mapping() → dict
    - scanner.application_name       → str
    - scanner.print_summary()
    - scanner.save_to_file()

Standalone usage — the full scan, or one pipeline step at a time:
    python3 element.py              full scan (5s countdown → screenshot → annotate)
    python3 element.py topmost      step 1: topmost-application detection only
    python3 element.py screenshot   step 2: screen capture only (saves element_screenshot.png)
    python3 element.py walk         step 3: element walk only (prints what was found)

Requires: python3-gi (Atspi 2.0, Gdk 3.0) and Pillow — see
requirements_linux.txt. There is no OCR pass: AT-SPI already exposes text
that would otherwise have to be recognised from pixels.
"""

import os
import io
import re
import sys
import time
import contextlib
import base64
import secrets
import threading
from collections import namedtuple, Counter
from urllib.parse import urlparse, unquote

from PIL import Image, ImageDraw, ImageFont

# The OCR scanner (PP-OCRv6 via onnxruntime, see ocr.py). ocr.py imports only
# the standard library and PIL, so this cannot fail on a machine without the
# runtime — availability is asked per scan (ocr_unavailable_reason) instead.
try:
    from .ocr import OCRScanner, ocr_unavailable_reason
except ImportError:
    from ocr import OCRScanner, ocr_unavailable_reason


def _add_system_gi_to_path():
    """Put the system PyGObject on sys.path. Returns True if one was found.

    gi cannot come from pip: it binds to the AT-SPI and GTK typelibs, which
    are introspection data under /usr/lib/<triplet>/girepository-1.0/ rather
    than Python, so the pip half alone still cannot import Atspi. That makes
    gi a system package, and a venv built without --system-site-packages
    cannot see it. Locating it here keeps that off the user's plate — nobody
    should have to hand-symlink a package to launch the app.

    Only a directory whose compiled _gi extension carries THIS interpreter's
    ABI tag is accepted, so a 3.12 build can never be loaded into 3.14. The
    path is APPENDED, so anything already installed in the venv still wins.
    """
    import glob
    tag = f"{sys.version_info.major}{sys.version_info.minor}"
    ver = f"{sys.version_info.major}.{sys.version_info.minor}"
    for base in (f"/usr/lib/python3/dist-packages",          # Debian, Ubuntu
                 f"/usr/lib/python{ver}/site-packages",      # Arch, Fedora
                 f"/usr/lib64/python{ver}/site-packages"):   # Fedora (64-bit)
        if base not in sys.path and glob.glob(
                os.path.join(base, "gi", f"_gi.cpython-{tag}-*.so")):
            sys.path.append(base)
            return True
    return False


# The two ways this can fail are both environment, not code, and a bare
# traceback makes them look like element.py is broken: gi is a system package
# (see above), and gi.require_version raises ValueError — not ImportError —
# when the typelibs are missing. Name the one apt install that fixes either.
_GI_HINT = ("PyGObject and its typelibs are system packages — the typelibs are "
            "introspection data, not Python, so pip cannot supply them:\n"
            "  sudo apt install python3-gi gir1.2-atspi-2.0 gir1.2-gtk-3.0")
try:
    import gi
except ImportError as _exc:
    if not _add_system_gi_to_path():
        raise ImportError(f"cannot import gi ({_exc})\n{_GI_HINT}") from None
    import gi

try:
    gi.require_version('Atspi', '2.0')
    gi.require_version('Gdk', '3.0')
except ValueError as _exc:
    raise ImportError(f"{_exc}\n{_GI_HINT}") from None
from gi.repository import Atspi, Gdk, Gio, GLib


# ========== CONFIGURATION ==========
# Toggle switches — what a scan produces besides the element tree.
SCREENSHOT = True    # Set to False to only generate element tree without screenshot
DEBUG = False        # Set to True to save files to debug folders, False for direct LLM only
FRONTEND = True      # Set to True when running from app.py to send images to frontend
# Fuse OCR text into the tree WHERE AT-SPI PUBLISHED NOTHING — canvas text,
# label-less controls, a renderer that never woke. Linux has no native OCR, so
# this is PP-OCRv6 through onnxruntime (ocr.py); when that runtime or its
# models are missing the scan says so once and carries on without it.
OCR = True
# Read the text of the boxes that survive the tree filter. False keeps the
# boxes (as unread text) and skips the recognizer — faster, blinder.
OCR_RECOGNIZE = True

# Define Rect namedtuple matching Windows format (left, top, right, bottom)
Rect = namedtuple('Rect', ['left', 'top', 'right', 'bottom'])

# One colour for every element, so the boxes read as a single overlay.
BOX_COLOR = (255, 0, 255)     # Bright magenta for all boxes
NUMBER_COLOR = (255, 0, 255)  # Same magenta for numbers
# Outline width in DELIVERED pixels (boxes are drawn after the downscale, so
# this is literal). A dense tree nests boxes only a few pixels apart, and at
# 2px adjacent borders merge into magenta slabs that bury the UI underneath —
# the box is meant to delimit an element, not fill it.
BOX_WIDTH = 1

# Final geometry/encoding of the annotated screenshot.
# The image is encoded ONCE, here, as lossless PNG: those bytes are what DEBUG
# writes to disk and, with DEBUG on, what the frontend preview shows (with it
# off, the preview is the plain, unannotated capture). On the way to the model
# the Linux tool registry (tool_registry/service.py, as_jpeg_base64) converts
# it to the same JPEG q85 4:4:4 the other platforms send, so the DIMENSIONS
# chosen here are what the model sees and what it is billed for; the PNG bytes
# themselves never reach the wire.
#
# One cap, matching mac/tree/element.py: the delivered image fits inside full
# HD (1080p). Orientation-agnostic - long side <= 1920, short side <= 1080 -
# aspect preserved, never upscaled: a single 1080p monitor's capture is sent
# as is, anything bigger (4K, HiDPI laptops) is scaled down to fit. NOTE: on
# Linux the capture is the bounding box of EVERY monitor (get_screen), so with
# several monitors the box applies to the whole desktop composite - two
# side-by-side 1080p monitors arrive as 1920x540, two stacked as 1080x1215 -
# and UI text in the delivered image is correspondingly smaller.
#
# Why 1080p: Claude 4.7+ and GPT-6 read an image up to ~2500 px natively and
# bill per patch, so the cost follows the delivered pixel count. A 16:9 1440p
# or 4K capture used to go out as 2300x1293 (~3900 Claude tokens per step) and
# now goes as 1920x1080 (~2690) - about 31% less. 16:10 and 3:2 HiDPI panels
# save ~45% (the mac tree's 13" Air: ~4300 -> ~2340). A single 1080p monitor
# was already sent as is, so it is unchanged. Gemini 3 bills a flat ~1066
# whatever the size; older Claude models downsize to <=1568 either way. The
# JPEG rides the never-cached live message on EVERY step and shrinks with the
# pixel count too. Labels are drawn AFTER the resize at a fixed pixel size, so
# they are untouched.
LLM_IMAGE_LONG_EDGE = 1920
LLM_IMAGE_SHORT_EDGE = 1080
LLM_IMAGE_FORMAT = "PNG"          # lossless — annotations are thin, saturated detail
LLM_IMAGE_COMPRESS_LEVEL = 1      # PNG is lossless at every level; 1 encodes fastest
LLM_IMAGE_MEDIA_TYPE = "image/png"


def llm_image_shrink(width, height):
    """Scale factor that fits a capture inside LLM_IMAGE_LONG_EDGE x
    LLM_IMAGE_SHORT_EDGE whichever way it is oriented; 1.0 when it already
    fits (never upscale)."""
    return min(1.0,
               LLM_IMAGE_LONG_EDGE / max(width, height),
               LLM_IMAGE_SHORT_EDGE / min(width, height))


# Index-label styling, in delivered pixels (labels are drawn AFTER the downscale).
LABEL_FONT_SIZE = 13
LABEL_STROKE = 2
LABEL_STROKE_COLOR = (0, 0, 0)

# The plain screenshot mirrored to the frontend is a human-facing preview.
FRONTEND_IMAGE_MAX_DIMENSION = 1920
FRONTEND_IMAGE_QUALITY = 100

MAX_DEPTH = 30
MAX_NODES = 20_000        # hard cap on AT-SPI nodes visited per scan (runaway guard)
MAX_CHILDREN = 500        # per-node child cap

# Electron/Chromium apps publish their AT-SPI tree only when a screen reader is
# "present", which org.a11y.Status.ScreenReaderEnabled announces. The tree then
# populates lazily, so after flipping the flag we poll until it stops growing.
A11Y_TREE_READY_TIMEOUT = 6.0
A11Y_TREE_READY_INTERVAL = 0.4
# The fast probe is one bulk GetItems (~15ms) instead of hundreds of sync
# child reads, so it can afford to look much more often — which is what
# makes a small static window clear the four-equal-counts bar in ~0.4s
# instead of ~1.2s.
A11Y_TREE_READY_INTERVAL_FAST = 0.12
MENU_STRIP_BOTTOM = 40    # shell items above this y are "menu bar" (top bar)

# Runtime Electron/Chromium tree activation (see electron_nudge): when the
# active window's tree is empty, pulse the screen-reader flag until the app
# starts building its tree, then revert. Universal — no launch flags or
# per-app settings required. Set False to disable the pulse; then sparse
# Electron apps need --force-renderer-accessibility (or, for VS Code family,
# the "editor.accessibilitySupport": "on" setting).
ELECTRON_NUDGE = True
# How long to wait for a Chromium tree after the get_attributes() probe.
# Measured 0.54s on Chrome 152 from a cold start; 2.0s is headroom, not a
# budget that gets spent — the wait exits as soon as the tree appears, and an
# app that ignores the probe is not a Chromium app and never had a tree to
# wait for.
NUDGE_TIMEOUT = 2.0

# ---- Web browsers ----
# A browser window is scanned the way the macOS scanner scans one (see its
# BROWSER_BUNDLES / _wait_for_ax_web_content / AXFocused rule):
#   1. the scan BLOCKS until the page has loaded (wait_for_browser_load), so
#      the tree and the screenshot describe the page the user is looking at
#      and not the one on its way out;
#   2. in the walk, a non-interactive role (is_enabled_flag False: label,
#      static, heading, filler) is kept ONLY while it holds keyboard focus.
#      Web pages publish every text run as a `static`, most of them repeating
#      the link or button around them; the controls are what the agent acts
#      on, and OCR reads the prose. Measured on a GitLab project page: 320
#      elements before, of which ~100 were statics duplicating their parent.
#
# A browser is recognised by its PROCESS, not its toolkit: Electron apps
# (Cursor, VS Code, Slack) report toolkit "Chromium" exactly like Chrome, and
# their trees are ordinary application UI that must keep its labels. comm is
# the kernel's 15-character name (/proc/<pid>/comm), so "chromium-browser"
# arrives as "chromium-browse"; both spellings are listed.
BROWSER_PROCESS_NAMES = frozenset({
    "chrome", "google-chrome", "google-chrome-stable", "chromium",
    "chromium-browser", "chromium-browse", "thorium", "thorium-browser",
    "firefox", "firefox-bin", "firefox-esr", "librewolf", "waterfox",
    "brave", "brave-browser", "brave-browser-stable",
    "msedge", "microsoft-edge", "microsoft-edge-stable",
    "opera", "vivaldi", "vivaldi-bin", "epiphany",
})
# Fallback on the AT-SPI application name, for a pid /proc will not show.
BROWSER_APP_NAMES = ("google chrome", "chromium", "firefox", "brave",
                     "microsoft edge", "opera", "vivaldi", "librewolf")
# Roles a browser publishes for the page itself (Chromium and Gecko both use
# "document web"; older Gecko and print previews use "document frame"), and
# the role Gecko wraps each tab's page in.
BROWSER_DOCUMENT_ROLES = frozenset({"document web", "document frame"})
BROWSER_FRAME_ROLE = "internal frame"
# Load-wait budgets, mirroring the macOS scanner's. Measured on Chrome 152
# and Firefox (snap, Gecko) against a page held on the network for 8s and a
# page whose image was held for 8s:
#   - while the navigation waits on the NETWORK, Chromium keeps an EMPTY
#     document (no name, 0 children, window titled "Untitled"); Gecko has no
#     document node at all — only the tab's SHOWING `internal frame`, with
#     no children, for the whole wait;
#   - once the page arrives and until its load event, BOTH set the AT-SPI
#     BUSY state on the document (Gecko adds STALE), and clear it exactly
#     when loading ends.
# The network phase gets the same budget as the render phase: it lasts as
# long as the site takes to answer. The macOS scanner's short grace for a
# missing web area is kept only for a window with neither a document nor a
# frame, which is a window with no web content at all.
BROWSER_LOAD_TIMEOUT = 15.0   # a page on its way: pending navigation or BUSY
BROWSER_DOC_GRACE = 1.5       # no document and no frame — not a web window
BROWSER_LOAD_INTERVAL = 0.25
BROWSER_SETTLE = 0.2          # one beat for compositing after a real wait

# Stop the walk at a node that is not SHOWING, skipping its whole subtree —
# ~70% of a real Electron tree, and the single biggest cost in a scan. See
# walk() for why this cannot lose an element. Escape hatch for a toolkit that
# reports SHOWING wrongly; leave it on.
#
# NOTE: Atspi.Accessible.set_cache_mask was tried here and removed. It makes
# a REPEAT read of the same node's role/state/name free, but a walk touches
# each node exactly once, so there is no repeat to serve — measured 6.28s
# without it vs 6.28s with it on a 2900-node tree. Caching across scans would
# help even less: SHOWING is what the prune below depends on, and a stale
# SHOWING would silently amputate the tree.
PRUNE_HIDDEN = True

# Bulk fast path (the FAST BULK READER section): read whole app trees
# through the toolkit bridges' org.a11y.atspi.Cache in one round-trip each,
# and fetch extents/text as pipelined async calls, instead of 2-6 sync D-Bus
# round-trips per node. Same semantics, ~10x the speed; every piece falls
# back to the sync walk on its own when a toolkit doesn't cooperate. False
# forces the sync walk everywhere (the A/B switch for debugging the fast
# path itself).
FASTSCAN = True


# ========== ELEMENT CONFIG (AT-SPI role names) ==========
# ONE table, keyed by Atspi.Accessible.get_role_name(). Everything the scanner
# needs to know about a role lives in its entry here, so adding support for a
# new widget is adding one block below and nothing else. The lookup sets under
# the table are DERIVED from it — never edit them by hand.
#
# Keys, all optional except `track`:
#
#   track             False keeps the role out of the tree. Untracked roles are
#                     still walked THROUGH: a container is not an element, but
#                     its children are.
#   type              The name printed in the tree text: the REAL AT-SPI role,
#                     CamelCased. Do not substitute another platform's
#                     vocabulary — an "entry" is an Entry, not a TextField, and
#                     a "filler" is a Filler, not a Group. Each platform's
#                     scanner speaks its own toolkit's names (macOS AXButton,
#                     iOS XCUIElementTypeStaticText), so a role here can be
#                     looked up in the AT-SPI docs as written. Omit on
#                     untracked roles; clean_type() then falls back to the same
#                     CamelCase transform.
#   is_enabled_flag   True skips the element when it reports neither ENABLED
#                     nor SENSITIVE. Both are checked because GTK4 sets only
#                     SENSITIVE on interactive widgets (measured on GTK 4.22:
#                     every gnome-control-center row is
#                     SENSITIVE|SHOWING|VISIBLE|FOCUSABLE and never ENABLED),
#                     so testing ENABLED alone dropped every button and toggle
#                     button in the app and left only their inner labels.
#                     Non-interactive roles use False: an enabled bit on a
#                     layout box is toolkit noise, not signal. In a WEB
#                     BROWSER's window a False role is kept only while it
#                     holds keyboard focus (see BROWSER_PROCESS_NAMES).
#   fallback          Label search order, first non-empty non-generic wins.
#                     "name" and "description" are AT-SPI attributes; "_text"
#                     reads the Text interface contents and costs a D-Bus
#                     round-trip, so it goes last and only on roles that carry
#                     real text.
#   value             True captures the control's current text, so the agent
#                     can read a URL bar or a filled-in field.
#   clips_children    True bounds descendants' visible area to this node's
#                     viewport — how scrolled-out content is detected.
#   shell             True collects this role from the GNOME Shell chrome walk
#                     (top bar and dash/dock).
#   default_label     The label used when every fallback source is empty or
#                     generic — for the one control that IS the thing rather
#                     than a named thing. An editor's text pane has no name,
#                     no description and, until something is typed, no text,
#                     yet it is the target the agent must click to type.
#                     Measured on gnome-text-editor 50: the pane is an
#                     anonymous EDITABLE|MULTI_LINE `text` node whose only
#                     relation is controlled-by (its two scroll bars), so no
#                     source in the chain could name it and both walks dropped
#                     it as label-less — the app reached the tree as its
#                     header-bar buttons only. The macOS scanner gets this
#                     case free from AXRoleDescription ("text area"); AT-SPI's
#                     equivalent, the role name, is "text", which
#                     GENERIC_LABELS rightly rejects.
ELEMENT_CONFIG = {

    # ---------- buttons and other activatable controls ----------
    "push button": {
        "track": True,
        "type": "PushButton",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "shell": True,
    },
    "button": {
        "track": True,
        "type": "Button",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "shell": True,
    },
    "toggle button": {
        "track": True,
        "type": "ToggleButton",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "shell": True,
    },
    "check box": {
        "track": True,
        "type": "CheckBox",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "shell": True,
    },
    "radio button": {
        "track": True,
        "type": "RadioButton",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
    },
    "link": {
        "track": True,
        "type": "Link",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
    },
    "page tab": {
        "track": True,
        "type": "PageTab",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
    },

    # ---------- menus ----------
    "menu item": {
        "track": True,
        "type": "MenuItem",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "shell": True,
    },
    "check menu item": {
        "track": True,
        "type": "CheckMenuItem",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
    },
    "radio menu item": {
        "track": True,
        "type": "RadioMenuItem",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
    },
    "menu": {
        # A pure container, so "name" alone: it has no description of its own
        # and implements no Text interface.
        "track": True,
        "type": "Menu",
        "is_enabled_flag": True,
        "fallback": ["name"],
    },

    # ---------- text entry and value-bearing controls ----------
    "entry": {
        "track": True,
        "type": "Entry",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "value": True,
    },
    "password text": {
        # No "_text": never read the contents of a password field.
        "track": True,
        "type": "PasswordText",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
    },
    "text": {
        # Multi-line. Deliberately NOT a value role — an editor would dump a
        # whole document into the tree. The walk additionally tracks it only
        # when EDITABLE, so Chromium's read-only text runs stay out.
        #
        # "_text" last, as the macOS chain reads AXValue: a pane with content
        # is labelled by its first line (the 50-char label cap applies), the
        # way the Windows scanner labels a Document. An empty pane has
        # nothing to read, hence default_label — see the key's note above.
        "track": True,
        "type": "Text",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "default_label": "Text Area",
    },
    "combo box": {
        "track": True,
        "type": "ComboBox",
        "is_enabled_flag": True,
        "fallback": ["name", "description", "_text"],
        "value": True,
    },
    "spin button": {
        "track": True,
        "type": "SpinButton",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
        "value": True,
    },
    "slider": {
        "track": True,
        "type": "Slider",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
    },

    # ---------- collection items ----------
    "list item": {
        "track": True,
        "type": "ListItem",
        "is_enabled_flag": True,
        "fallback": ["name", "_text", "description"],
    },
    "tree item": {
        "track": True,
        "type": "TreeItem",
        "is_enabled_flag": True,
        "fallback": ["name", "_text", "description"],
    },
    "table cell": {
        "track": True,
        "type": "TableCell",
        "is_enabled_flag": True,
        "fallback": ["name", "_text", "description"],
    },

    # ---------- graphics ----------
    "icon": {
        "track": True,
        "type": "Icon",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
        "shell": True,
    },
    "image": {
        "track": True,
        "type": "Image",
        "is_enabled_flag": True,
        "fallback": ["name", "description"],
    },

    # ---------- static text ----------
    "label": {
        "track": True,
        "type": "Label",
        "is_enabled_flag": False,
        "fallback": ["name", "_text"],
        "shell": True,
    },
    "static": {
        "track": True,
        "type": "Static",
        "is_enabled_flag": False,
        "fallback": ["name", "_text"],
    },
    "heading": {
        "track": True,
        "type": "Heading",
        "is_enabled_flag": False,
        "fallback": ["name", "_text"],
    },

    # ---------- containers ----------
    "filler": {
        # ATK's "object that fills up space" — pure layout, and in GTK3 every
        # GtkBox reports it (measured: 67 of accerciser's 567 nodes, none
        # named). Tracked anyway for one reason: GNOME's Desktop Icons
        # extension builds each desktop tile from a GtkBox, so the tiles ARE
        # fillers, and without this the desktop reaches the tree only as its
        # 38x19 text labels and never as the 128x111 target you actually
        # click. Unnamed ones are real layout and build_label drops them.
        #
        # "name" alone — a filler has no description (measured: none of the
        # 67, nor the desktop tiles), and "_text" would be worse than useless:
        # a container implements no Text interface, so it costs a failed
        # D-Bus round-trip to return "" on essentially every filler.
        "track": True,
        "type": "Filler",
        "is_enabled_flag": False,
        "fallback": ["name"],
    },
    # ---------- toplevels ----------
    # Never tracked by the walk. Each walked window is emitted afterwards by
    # _finish_window, sized to what the user SEES of it (the frame minus any
    # client-side shadow), as the element that boxes everything inside it.
    # These entries only name that box's type.
    "frame": {"track": False, "type": "Window"},
    "dialog": {"track": False, "type": "Dialog"},
    "window": {"track": False, "type": "Window"},

    "scroll pane": {
        # Not an element, but its viewport is what makes scrolled-out
        # children count as hidden.
        "track": False,
        "clips_children": True,
    },
    "viewport": {
        "track": False,
        "clips_children": True,
    },
}


# --- Derived lookups. Generated from ELEMENT_CONFIG; do not edit by hand. ---

# AT-SPI role name -> the type printed in the tree text.
TYPE_MAP = {role: cfg["type"]
            for role, cfg in ELEMENT_CONFIG.items() if "type" in cfg}

# Roles that clip their children's visible area to their own viewport.
CLIP_ROLES = frozenset(role for role, cfg in ELEMENT_CONFIG.items()
                       if cfg.get("clips_children"))

# Value-bearing single-line controls whose current text is captured.
VALUE_ROLES = frozenset(role for role, cfg in ELEMENT_CONFIG.items()
                        if cfg.get("value"))

# Extra roles collected ONLY while the overview is up. The search entry
# publishes its placeholder through a "text" node; outside the overview that
# role would pull in shell text runs nobody can click.
OVERVIEW_EXTRA_ROLES = frozenset({"text"})

# Element types that are pure layout: they never claim screen space for OCR
# suppression, so text in the gaps between their children still comes
# through. Every other tracked type is a real widget and hides the OCR text
# under it (its own label is already in the tree).
# The window box (see _finish_window) is layout too: it holds everything.
OCR_STRUCTURAL_CONTAINER_TYPES = frozenset({"Filler", "Window", "Dialog"})
# An OCR box with at least this fraction of its area under element rects is
# text the tree already names, even when its centre lands in a gap.
OCR_COVERED_DROP = 0.6
_OCR_WARNED = False


def _covered_fraction(line, rects):
    """Fraction of an OCR line's area lying under `rects` (summed, not
    unioned — element leaves seldom overlap, so this errs high, which only
    ever drops a box the tree very probably has)."""
    w = line["right"] - line["left"]
    h = line["bottom"] - line["top"]
    if w <= 0 or h <= 0:
        return 1.0
    covered = 0
    for r in rects:
        iw = min(line["right"], r.right) - max(line["left"], r.left)
        if iw <= 0:
            continue
        ih = min(line["bottom"], r.bottom) - max(line["top"], r.top)
        if ih <= 0:
            continue
        covered += iw * ih
        if covered >= w * h:
            break
    return min(1.0, covered / (w * h))


def _norm_label(text):
    """Label text reduced to what OCR could plausibly reproduce: lower-case
    alphanumerics only."""
    return "".join(ch for ch in (text or "").lower() if ch.isalnum())


def _repeats_touching_label(line, leaf_labels):
    """True if the read text equals the label of an element whose rect
    touches the line's box (grown by 4px, for captions drawn just beside
    their control)."""
    t = _norm_label(line.get("text"))
    if len(t) < 2:
        return False
    l, tp = line["left"] - 4, line["top"] - 4
    r, b = line["right"] + 4, line["bottom"] + 4
    for rect, label in leaf_labels:
        if label != t:
            continue
        if rect.left <= r and rect.right >= l and rect.top <= b and rect.bottom >= tp:
            return True
    return False

# Roles collected from the GNOME Shell chrome walk (top bar, dash/dock).
SHELL_TRACK_ROLES = frozenset(role for role, cfg in ELEMENT_CONFIG.items()
                              if cfg.get("shell"))


def clean_type(role_str):
    return TYPE_MAP.get(role_str, role_str.title().replace(" ", ""))


GENERIC_LABELS = frozenset({
    "", "group", "application", "image", "icon", "text", "button", "cell",
    "row", "tab", "label", "panel", "frame", "radio button", "check box",
    "menu item", "list item",
})

# Roles collected from the GNOME Shell chrome walk (top bar, dash/dock).
SHELL_TRACK_ROLES = frozenset({
    "label", "push button", "button", "toggle button", "menu item",
    "check box", "icon",
})

_IS_WAYLAND = (os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland"
               or bool(os.environ.get("WAYLAND_DISPLAY")))


# ========== AT-SPI HELPERS ==========

# Which coordinate space to ask every element for.
#
# On Wayland, WINDOW — not SCREEN. A Wayland client cannot know its own screen
# position, and toolkits disagree about what to do with SCREEN as a result:
# Chromium answers window-relative coordinates anyway (its window reports
# (0,0), so the two spaces coincide), while GTK4 answers (0, 0) for EVERY
# node. Measured on gnome-control-center (GTK 4.22): the Appearance pane is
# SCREEN=(0,0,735,640) — wrong — and WINDOW=(245,0,735,640) — right. Reading
# SCREEN there collapses an entire window's widgets onto its top-left corner,
# which is how a correctly-targeted Settings scan produced elements that all
# shared one pixel.
#
# WINDOW is the space the Wayland offset correction already exists to fix up:
# resolve_window_offset finds the window's true origin from gnome-shell's
# actors, and every element is shifted by it. So window-relative input is
# exactly what that machinery wants, and Chromium is unaffected because its
# two spaces are identical.
#
# On X11 keep SCREEN: coordinates there are already true screen coordinates
# and the offset is (0, 0), so asking for WINDOW would strip the origin back
# off and send every click to the wrong place.
_ATSPI_COORD = (Atspi.CoordType.WINDOW if _IS_WAYLAND
                else Atspi.CoordType.SCREEN)

# How long a collected actor list stays trustworthy. Window geometry only
# changes when something animates, so a list this fresh can be reused instead
# of paying ~0.18s of D-Bus to fetch an identical one.
_ACTOR_STALE_AFTER = 0.5

# Largest plausible width/height for a real widget. Guards against a toolkit
# returning uninitialised memory (measured: 891410224x32707 on GTK4).
_MAX_SANE_EXTENT = 32000

_STATE = Atspi.StateType

def acc_states(acc):
    try:
        return acc.get_state_set()
    except Exception:
        return None


def acc_extents(acc, offset=(0, 0)):
    """Return {x, y, width, height} in (offset-corrected) screen coords, or None."""
    try:
        ext = acc.get_extents(_ATSPI_COORD)
    except Exception:
        return None
    if ext.width <= 0 or ext.height <= 0:
        return None
    # A toolkit can hand back uninitialised memory here — measured on GTK4:
    # a panel reporting 891410224x32707. Anything larger than the desktop is
    # not a real widget, and letting it through makes it an occluder that
    # swallows the screen.
    if ext.width > _MAX_SANE_EXTENT or ext.height > _MAX_SANE_EXTENT:
        return None
    return {"x": ext.x + offset[0], "y": ext.y + offset[1],
            "width": ext.width, "height": ext.height}


def acc_text(acc, limit=120):
    """Read the Text interface contents (single line, capped), or "".

    Calls the Atspi.Text interface methods UNBOUND: acc.get_text() resolves to
    the deprecated Accessible.get_text (an interface getter that takes no
    offsets), so bound calls with offsets fail."""
    try:
        n = Atspi.Text.get_character_count(acc)
        if n and n > 0:
            s = Atspi.Text.get_text(acc, 0, min(n, limit))
            return (s or "").replace("\n", " ").strip()
    except Exception:
        pass
    return ""


# Characters that carry no text: the object-replacement char (U+FFFC) that
# Chromium and Gecko put in a container's name and Text for each embedded
# child — a GitLab sidebar published every list item as "￼" or "￼￼", which
# reached the tree as an element whose name was nothing — the replacement
# char (U+FFFD), and the zero-width family.
_JUNK_CHARS = frozenset("\ufffc\ufffd\u200b\u200c\u200d\u2060\ufeff")
# List markers a browser prefixes to a list item's name/text ("• Link A" in
# Gecko; "• ￼" in Chromium, which is nothing but the marker once the
# embedded-object char is gone).
_LIST_BULLETS = "\u2022\u25e6\u25aa\u25ab\u2023\u2043\u25cf\u25cb\u25a0\u25a1"


def _strip_junk(s):
    """Drop the characters that only look like text: embedded-object and
    replacement chars, zero-width chars, and Unicode private-use icon glyphs
    (Chromium exposes icon fonts as PUA codepoints)."""
    return "".join(ch for ch in s if ch not in _JUNK_CHARS
                   and not ('\ue000' <= ch <= '\uf8ff')
                   and not ('\U000f0000' <= ch <= '\U0010fffd'))


def _clean_label(s):
    """Collapse whitespace, drop junk characters and a leading list marker.
    Returns "" for a label that was nothing but those."""
    s = " ".join(_strip_junk(s).split())
    if s and s[0] in _LIST_BULLETS:
        s = s[1:].lstrip()
    return s


def _clean_value(s):
    """A control's current text, minus the junk characters — a combo box
    whose value is an embedded-object char has no value."""
    return " ".join(_strip_junk(s).split())


def iter_children(acc):
    try:
        count = min(acc.get_child_count(), MAX_CHILDREN)
    except Exception:
        return
    for i in range(count):
        try:
            child = acc.get_child_at_index(i)
        except Exception:
            continue
        if child is not None:
            yield child


def build_label(acc, cfg, name=None, description=None):
    """Try each fallback source, return first non-empty, non-generic string."""
    for attr in cfg.get("fallback", []):
        if attr == "name":
            val = name if name is not None else _safe_name(acc)
        elif attr == "description":
            val = description if description is not None else _safe_desc(acc)
        elif attr == "_text":
            val = acc_text(acc)
        else:
            val = ""
        if val:
            label = _clean_label(str(val))
            if not label or label.lower() in GENERIC_LABELS:
                continue
            return label[:50] if len(label) > 50 else label
    return cfg.get("default_label", "")


# How much bigger than the node its parent may be and still be treated as
# "the same control, drawn properly". Measured in the GNOME overview: the
# search entry's text is 291x19 inside a 338x37 box — 2.3x, and that box is
# what the user sees and clicks (it holds the magnifier icon too). An app-grid
# tile's parent, by contrast, is the whole 1980x813 grid — 126x — and
# promoting to it would replace every icon with one screen-sized target.
_PROMOTE_MAX_AREA_RATIO = 6.0


def _promotable(inner, outer):
    """True if `outer` is a tight box around `inner` — the drawn control."""
    if not inner or not outer:
        return False
    ia = inner["width"] * inner["height"]
    oa = outer["width"] * outer["height"]
    if ia <= 0 or oa < ia:
        return False
    return oa <= ia * _PROMOTE_MAX_AREA_RATIO and _contains(outer, inner)


def _relation_label(acc):
    """The name a control points at through LABELLED_BY / DESCRIBED_BY.

    AT-SPI's standard way for an unnamed control to name itself. GNOME's
    overview search entry is exactly this: an anonymous, EDITABLE `text` node
    whose only identification is
        rels=[('described-by', 'Type to search', 'label')]
    with no name, description, text or attributes of its own. Nothing in the
    scanner read relations, so the field was invisible."""
    try:
        for rel in (acc.get_relation_set() or []):
            rt = rel.get_relation_type()
            if rt not in (Atspi.RelationType.LABELLED_BY,
                          Atspi.RelationType.DESCRIBED_BY):
                continue
            for i in range(rel.get_n_targets()):
                name = _safe_name(rel.get_target(i))
                if name:
                    return name
    except Exception:
        pass
    return ""


def _safe_name(acc):
    try:
        return acc.get_name() or ""
    except Exception:
        return ""


def _safe_desc(acc):
    try:
        return acc.get_description() or ""
    except Exception:
        return ""


# ========== GEOMETRY ==========

def _rect_intersect(a, b):
    """Return intersection rect of a and b, or None if no overlap."""
    x1 = max(a["x"], b["x"])
    y1 = max(a["y"], b["y"])
    x2 = min(a["x"] + a["width"], b["x"] + b["width"])
    y2 = min(a["y"] + a["height"], b["y"] + b["height"])
    if x2 <= x1 or y2 <= y1:
        return None
    return {"x": x1, "y": y1, "width": x2 - x1, "height": y2 - y1}


def _visibility(frame, clip, screen):
    """Return (visibility_str, visible_rect_or_None) for frame within the
    innermost scroll clip and the screen bounds — "full", "partial N%", or
    "hidden". AT-SPI's SHOWING state already excludes most scrolled-out
    elements; this catches the partially-clipped remainder."""
    visible = dict(frame)
    if clip is not None:
        visible = _rect_intersect(visible, clip)
        if visible is None:
            return "hidden", None
    visible = _rect_intersect(visible, screen)
    if visible is None:
        return "hidden", None
    total = frame["width"] * frame["height"]
    if total <= 0:
        return "hidden", None
    pct = (visible["width"] * visible["height"]) / total * 100.0
    if pct >= 99.0:
        return "full", None
    if pct > 0:
        return f"partial {int(pct)}%", visible
    return "hidden", None


def _contains(a, b):
    """Return True if element a spatially contains element b."""
    return (a["x"] <= b["x"] and a["y"] <= b["y"]
            and a["x"] + a["width"] >= b["x"] + b["width"]
            and a["y"] + a["height"] >= b["y"] + b["height"])


def get_screen():
    """Bounding box of all monitors in logical coords: {x, y, width, height}."""
    try:
        display = Gdk.Display.get_default()
        if display and display.get_n_monitors() > 0:
            x0 = y0 = 10 ** 9
            x1 = y1 = -10 ** 9
            for i in range(display.get_n_monitors()):
                g = display.get_monitor(i).get_geometry()
                x0, y0 = min(x0, g.x), min(y0, g.y)
                x1, y1 = max(x1, g.x + g.width), max(y1, g.y + g.height)
            return {"x": x0, "y": y0, "width": x1 - x0, "height": y1 - y0}
    except Exception:
        pass
    return {"x": 0, "y": 0, "width": 1920, "height": 1080}


# ========== DESKTOP / ACTIVE WINDOW ==========

# Desktop Icons publishes one toplevel per monitor, and the title depends on
# which implementation is installed: the original extension used a readable
# "Desktop Icons" prefix, while DING / desktop-icons-ng encodes the monitor
# origin instead — e.g. "@!0,0;BDHF". Matching only the readable form left the
# whole desktop invisible to the scanner on a stock Ubuntu GNOME session.
_DING_WINDOW_RE = re.compile(r"^@!-?\d+,-?\d+;")


def is_desktop_window(name):
    """True if `name` is a Desktop Icons toplevel, either naming scheme."""
    name = name or ""
    return name.startswith("Desktop Icons") or bool(_DING_WINDOW_RE.match(name))


def get_desktop_apps():
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        try:
            app = desktop.get_child_at_index(i)
        except Exception:
            continue
        if app is not None:
            yield app


def find_app(name):
    for app in get_desktop_apps():
        if _safe_name(app) == name:
            return app
    return None


_NO_WALK_TOOLKITS = {}      # pid -> reason, cached for the process lifetime


def unwalkable_toolkit(app):
    """A reason string if this application's accessibility tree must NOT be
    walked, else None.

    Flutter (App Center / snap-store on Ubuntu) publishes a GTK toplevel and
    reports its toolkit as "gtk", but its accessibility bridge is its own,
    and walking it CRASHES THE APP — measured 2026-09-09: App Center died
    during a scan, twice. Reading the toplevel's states and extents is safe
    (every scan reads them for every app on the bus); enumerating children,
    Cache.GetItems, GetText and the rest are not. Such a window is scanned
    by OCR alone.

    Flutter cannot be told apart through AT-SPI (toolkit "gtk 3.24.50" like
    any GTK3 app), so the process is asked: /proc/<pid>/maps lists
    libflutter_linux_gtk.so. Same-uid processes are readable under Yama
    ptrace_scope 1 (measured). Cached per pid — one read per process."""
    try:
        pid = app.get_process_id()
    except Exception:
        return None
    if not pid or pid <= 0:
        return None
    if pid in _NO_WALK_TOOLKITS:
        return _NO_WALK_TOOLKITS[pid]
    reason = None
    try:
        with open(f"/proc/{pid}/maps") as f:
            if any("libflutter" in line for line in f):
                reason = "Flutter"
    except Exception:
        pass
    _NO_WALK_TOOLKITS[pid] = reason
    return reason


_BROWSER_PIDS = {}          # pid -> bool, cached for the process lifetime


def _browser_process_name(name):
    """True if a process name (comm or exe basename) is a known browser's.
    comm is truncated to 15 characters, so a truncated name matches any
    listed browser it is a prefix of."""
    name = (name or "").lower()
    if not name:
        return False
    if name in BROWSER_PROCESS_NAMES:
        return True
    return len(name) == 15 and any(b.startswith(name)
                                   for b in BROWSER_PROCESS_NAMES)


def is_browser_app(app):
    """True if this AT-SPI application is a web browser — see the
    BROWSER_PROCESS_NAMES note for why the process, not the toolkit, decides.
    /proc/<pid>/comm and the exe link are both asked, since comm can be a
    wrapper's name; the application name on the bus is the fallback for a
    process /proc will not show. Cached per pid — one read per process."""
    try:
        pid = app.get_process_id()
    except Exception:
        pid = 0
    if pid and pid in _BROWSER_PIDS:
        return _BROWSER_PIDS[pid]
    hit = False
    if pid and pid > 0:
        for src in (f"/proc/{pid}/comm", f"/proc/{pid}/exe"):
            try:
                if src.endswith("/exe"):
                    name = os.path.basename(os.readlink(src))
                else:
                    with open(src) as f:
                        name = f.read().strip()
            except Exception:
                continue
            if _browser_process_name(name):
                hit = True
                break
    if not hit:
        app_name = _safe_name(app).lower()
        hit = any(b in app_name for b in BROWSER_APP_NAMES)
    if pid:
        _BROWSER_PIDS[pid] = hit
    return hit


def candidate_toplevels():
    """Every scannable AT-SPI toplevel as (app, win, frame, states). Skips
    gnome-shell (its own chrome is scanned separately), the desktop, and
    windows with no readable geometry."""
    out = []
    for app in get_desktop_apps():
        if _safe_name(app) == "gnome-shell":
            continue
        for win in iter_children(app):
            states = acc_states(win)
            if states is None:
                continue
            if is_desktop_window(_safe_name(win)):
                continue
            ext = acc_extents(win)
            if not ext or ext["width"] <= 1 or ext["height"] <= 1:
                continue
            out.append((app, win, ext, states))
    return out


def _exact_owner(actor, cands, taken=(), narrate=False):
    """The (app, win) whose frame has EXACTLY this clone's size, or None.

    `taken` holds the windows already paired with other clones; they are
    skipped, so two clones of one size can never both resolve to the same
    window. Two maximized windows are exactly that case — measured: Cursor and
    Chrome, both 1980x1099 — and before this both clones resolved to whichever
    app the bus listed first, so Chrome on top was reported as "cursor" and
    then wholly occluded by its own twin.

    Among several candidates of the size, the one holding keyboard focus
    (ACTIVE) wins: the user just raised the window they clicked, so focus and
    topmost coincide. ACTIVE is a poor PRIMARY signal (see find_top_window)
    but it is the only tie-breaker Wayland leaves. With no focused candidate
    the pick is genuinely arbitrary, and `narrate` says so."""
    exact = [c for c in cands
             if not any(c[1] is t for t in taken)
             and c[2]["width"] == actor["width"]
             and c[2]["height"] == actor["height"]]
    if not exact:
        return None
    focused = [c for c in exact if c[3].contains(_STATE.ACTIVE)]
    pick = (focused or exact)[0]
    if narrate and len(exact) > 1:
        names = ", ".join(repr(_safe_name(a) or _safe_name(w) or "?")
                          for a, w, _f, _s in exact)
        if len(focused) == 1:
            why = "it has keyboard focus"
        elif focused:
            why = "several claim keyboard focus — ambiguous"
        else:
            why = "none has keyboard focus — genuinely ambiguous"
        print(f"  {len(exact)} on-screen windows share "
              f"{actor['width']}x{actor['height']} ({names}); this clone "
              f"is paired with {_safe_name(pick[0])!r}: {why}.")
    return pick[0], pick[1]


def _margin_owner(actor, cands, taken=()):
    """The (app, win) whose frame fits inside this clone with a CSD shadow
    margin (0..160 px on each axis), or None. Only ever asked AFTER every
    clone has had its exact-size chance (get_window_stack): asked first, a
    CSD clone was handed the exact size of a smaller window that happened to
    be earlier on the bus, and its own window went unowned."""
    for app, win, fr, _st in cands:
        if any(win is t for t in taken):
            continue
        mw, mh = actor["width"] - fr["width"], actor["height"] - fr["height"]
        if 0 <= mw <= 160 and 0 <= mh <= 160:
            return app, win
    return None


def _actor_owner(actor, cands, taken=(), narrate=False):
    """(app, win) for one clone considered on its own: exact size, else CSD
    margin. get_window_stack does NOT use this — it runs the exact pass over
    every clone before any margin pass. This is for one-off questions such
    as prepare_scan's blind-top audit."""
    return (_exact_owner(actor, cands, taken, narrate)
            or _margin_owner(actor, cands, taken))


def is_desktop_window_actor(actor, actors):
    """True if this clone is the desktop's own. The desktop is a fullscreen
    toplevel, so it is identified by covering the largest actor area seen —
    it owns no scannable window and must never be reported as a blind one."""
    if not actors:
        return False
    biggest = max(a["width"] * a["height"] for a in actors)
    return actor["width"] * actor["height"] >= biggest


def get_window_stack(actors=None, cands=None, narrate=False):
    """Front-to-back list of on-screen windows: [(app, win, frame, actor)],
    TOPMOST LAST — the Linux twin of the macOS scanner's window stack.

    macOS gets z-order for free from CGWindowListCopyWindowInfo, and Windows
    from EnumWindows. Wayland gives a client nothing: AT-SPI has no global
    z-order and every toplevel claims (0, 0). The one live source of truth is
    gnome-shell's own accessibility tree, which publishes a "Wayland window"
    clone per client window in Clutter paint order — so the last LIVE actor is
    the window on top of the screen. Matching each actor back to the AT-SPI
    toplevel of its size recovers the whole stack.

    The pairing is one-to-one and runs TOP-DOWN: the clone on top claims its
    window first (keyboard focus breaks a size tie, see _actor_owner) and a
    lower clone of the same size gets what is left. Bottom-up first-match
    handed both clones of two maximized windows to the same toplevel.

    Actors whose clone is not SHOWING are dropped: GNOME keeps a
    MetaWindowActor at last-known geometry for a MINIMIZED window, so a
    minimized Chrome otherwise reads as an on-screen window covering
    everything beneath it."""
    if actors is None:
        actors = _collect_shell_window_actors()
    # Only windows the toolkit itself calls on-screen may own an actor. Without
    # this, a minimized window stays eligible and the margin branch hands it
    # somebody else's actor — measured: the desktop's 1600x900 clone matched a
    # minimized 1533x871 Chrome (67/29 margin, inside the CSD band).
    cands = [c for c in (candidate_toplevels() if cands is None else cands)
             if c[3].contains(_STATE.SHOWING)]
    live = live_actors(actors)
    owner_of = {}
    taken = []
    # Pass 1 — exact sizes, top-down, over EVERY clone before any margin
    # match. The clone on top claims its window first (keyboard focus breaks
    # a size tie, see _exact_owner) and a lower clone of the same size gets
    # what is left. Running margins per clone instead handed a CSD clone
    # (1030x690 around a 980x640 frame) the exact size of a smaller window
    # that was earlier on the bus (1000x660), and the CSD window went
    # unowned; once that window has claimed its own exact clone here, the
    # margin pass below finds the CSD window free.
    for i in range(len(live) - 1, -1, -1):
        owner = _exact_owner(live[i], cands, taken, narrate=narrate)
        if owner is not None:
            owner_of[i] = owner
            taken.append(owner[1])
    # Pass 2 — CSD shadow margins for the clones still unowned. Never for a
    # desktop-sized clone: the desktop's own toplevel is not a candidate, so
    # by margin it can only ever steal a near-fullscreen window — measured:
    # the 2048x1130 desktop clone matched a 1981x1101 Chrome, 67/29 inside
    # the CSD band. A desktop-sized clone with an EXACT owner is a real
    # fullscreen window (F11 Chrome, a presentation) and was paired above.
    for i in range(len(live) - 1, -1, -1):
        if i in owner_of or is_desktop_window_actor(live[i], live):
            continue
        owner = _margin_owner(live[i], cands, taken)
        if owner is not None:
            owner_of[i] = owner
            taken.append(owner[1])
    if narrate:
        # A clone with no owner is a window off the accessibility bus. When
        # an OWNED clone has its exact size, the pairing may have gone to
        # the wrong one of the two — the only signal there is.
        for i, a in enumerate(live):
            if i in owner_of or is_desktop_window_actor(a, live):
                continue
            twin = [j for j in owner_of
                    if live[j]["width"] == a["width"]
                    and live[j]["height"] == a["height"]]
            if twin:
                print(f"  WARNING: a {a['width']}x{a['height']} window on "
                      f"screen is not on the accessibility bus, and "
                      f"{_safe_name(owner_of[twin[0]][0])!r} has that exact "
                      f"size — the two may be swapped. An app started before "
                      f"accessibility was on has to be restarted.")
    # Bottom-to-top, the documented contract (topmost LAST).
    return [(app, win, acc_extents(win), live[i])
            for i, (app, win) in sorted(owner_of.items())]


def find_top_window(actors=None, cands=None, narrate=False):
    """Return (app, window, actor) for the window ON TOP OF THE SCREEN.

    Z-order first, exactly like the macOS and Windows scanners: the owner of
    the topmost live shell actor. The AT-SPI ACTIVE state is only a fallback,
    because it answers a different question — which window has keyboard focus,
    which a modal dialog or a just-raised window need not have, and which a
    toolkit can leave set on a window that lost focus long ago.

    `actor` is the shell clone the stack paired with the window, so callers
    can resolve its origin and its place in the stack WITHOUT matching by size
    again — which cannot tell two windows of one size apart. It is None on the
    fallback paths, where no clone is known.

    Falls back to ACTIVE, then to the largest SHOWING toplevel, so a scan
    still describes something useful when gnome-shell publishes no actors at
    all (X11, or a non-GNOME compositor)."""
    if _IS_WAYLAND and actors is None:
        actors = _collect_shell_window_actors()
    if _IS_WAYLAND and actors:
        try:
            stack = get_window_stack(actors, cands, narrate=narrate)
            if stack:
                app, win, _fr, actor = stack[-1]
                return app, win, actor
            # The compositor answered and said NOTHING is on top: every app
            # window is minimized and the user is looking at the desktop.
            # Returning (None, None) is that answer — prepare_scan turns it
            # into the Desktop scan.
            #
            # Falling through to the heuristics below would be actively wrong
            # here, and was: with everything minimized, Chrome still publishes
            # phantom 1371x88 / 1371x458 frames that report SHOWING while the
            # real window is ICONIFIED, and the largest-SHOWING contest handed
            # one of them the scan — "Application: Google Chrome", 6 elements,
            # over a screenshot of a bare desktop.
            return None, None, None
        except Exception as e:
            print(f"  Z-order lookup failed ({e}) — falling back to ACTIVE.")

    # No z-order source at all (X11, or a compositor that publishes no window
    # actors). Only here are the state heuristics the best available answer.

    best = (None, None)
    best_area = 0
    for app, win, ext, states in (candidate_toplevels() if cands is None
                                 else cands):
        if states.contains(_STATE.ACTIVE):
            return app, win, None
        # The desktop is excluded from the largest-window fallback on
        # purpose: it is a fullscreen toplevel, so it would win that
        # contest on every session where no app happens to be ACTIVE.
        if states.contains(_STATE.SHOWING):
            if ext["width"] * ext["height"] > best_area:
                best_area = ext["width"] * ext["height"]
                best = (app, win)
    return best[0], best[1], None


def find_active_window(actors=None, cands=None, narrate=False):
    """(app, window) for the window on top — find_top_window without the
    actor, for callers that only need to know WHAT is in front."""
    app, win, _actor = find_top_window(actors, cands, narrate=narrate)
    return app, win


def _paired_actor(window, actors, cands):
    """The clone get_window_stack pairs with `window` in THIS actor list, or
    None. For when the actors are re-read mid-scan: the pairing has to be
    redone against the new clones, not carried over by size."""
    for _app, win, _fr, actor in get_window_stack(actors, cands):
        if win is window:
            return actor
    return None


# ========== WAYLAND WINDOW-OFFSET CORRECTION ==========
# On Wayland, AT-SPI reports toplevel windows at (0, 0) — clients can't know
# their own global position. GNOME Shell's own accessibility tree, however,
# exposes a "Wayland window" actor per client window with REAL screen
# geometry. Matching the window's size against those actors recovers the true
# origin. Actors can include the CSD shadow margin, so an equal-margin match
# (actor centred on the client area) is accepted too.

def _drop_agent_glow(actors):
    """Take the agent's own edge glow out of a shell actor list.

    While a run is live, AutoCua/utils/agent_glow.py paints four strips over
    the screen edges. They are click-through override-redirect X11 windows, and
    measured on GNOME 50 Wayland gnome-shell does not publish them here at all
    — the list is identical with the glow on and off. This runs anyway, because
    the cost of being wrong is silent and large: a strip in this list is a
    window covering a screen edge, so find_top_window would pair the scan with
    it and _apply_stack_occlusion would mark every element under the band
    covered. That is exactly what happened on macOS before _is_agent_glow was
    added there.

    Matching is by geometry, which is all an actor exposes (they carry no name,
    description or attributes — see resolve_window_offset). The rects come from
    the glow itself, so nothing else can match by accident.
    """
    try:
        from AutoCua.utils import agent_glow
        rects = agent_glow.glow_rects()
    except Exception:
        return actors
    if not rects:
        return actors
    def is_glow(a):
        return any(abs(a["x"] - x) <= 2 and abs(a["y"] - y) <= 2
                   and abs(a["width"] - w) <= 2 and abs(a["height"] - h) <= 2
                   for x, y, w, h in rects)
    return [a for a in actors if not is_glow(a)]


def _collect_shell_window_actors(fresh=False):
    return _drop_agent_glow(_shell_window_actors(fresh))


def _shell_window_actors(fresh=False):
    fast = _get_fast()
    if fast is not None:
        try:
            return _fast_shell_window_actors(fast, fresh)
        except Exception as e:
            print(f"  Fast actor scan unavailable ({e}) — walking gnome-shell.")

    actors = []
    shell = find_app("gnome-shell")
    if shell is None:
        return actors

    def _walk(acc, depth):
        if depth > 6:
            return
        # Role first: a window clone is always a "panel", so asking the role
        # (one D-Bus read) rules out most of the tree without also paying for
        # the name. Reading the name of every node on the way down doubled the
        # cost of this walk to find a handful of clones.
        try:
            role_str = acc.get_role_name() or ""
        except Exception:
            return
        if role_str == "panel" and _safe_name(acc) == "Wayland window":
            ext = acc_extents(acc)
            if ext:
                st = acc_states(acc)
                ext["showing"] = bool(
                    st is not None and st.contains(_STATE.SHOWING)
                    and st.contains(_STATE.VISIBLE))
                actors.append(ext)
            return  # window clones have no useful children
        for child in iter_children(acc):
            _walk(child, depth + 1)

    try:
        _walk(shell, 0)
    except Exception:
        pass
    return actors


def live_actors(actors):
    """Only the clones that are actually painted. GNOME keeps a
    MetaWindowActor at last-known geometry for a minimized window; it reports
    no SHOWING/VISIBLE bits while a real one reports both. Older callers that
    predate the bit are treated as live."""
    return [a for a in actors if a.get("showing", True)]


def resolve_window_offset(win_frame, actors, allow_topmost=False,
                          prefer=None):
    """Return ((dx, dy), how, actor) mapping a Wayland window's local coords to
    screen coords, or (None, None, None) when the origin cannot be resolved.
    `how` is "exact", "margin", "topmost" or "x11", for callers that want to
    report how much to trust the answer.

    `actor` is the shell clone the match came from, or None on the "x11" path.
    Callers must NOT try to recover it afterwards by comparing the corrected
    frame against the actor list: a "margin" match deliberately insets the
    frame inside its actor (measured: actor 269,55 1030x690 -> frame 294,80
    980x640), so the two rects differ by the shadow margin and no geometric
    re-match can find it. Returning it here is the only reliable way for the
    occlusion pass to know which actor is the window's own.

    Three strategies, most trustworthy first:

    "exact"/"margin" match the window against an actor BY SIZE. That is
    ambiguous when two windows share exact dimensions (two half-screen tiles),
    since both resolve to the same actor — Wayland gives clients nothing to
    disambiguate with. The actors carry no identity either: measured, they
    expose an empty name beyond "Wayland window", empty description, no
    attributes, no relations, and a single generic child. Size is all there is.
    `prefer` is the clone the z-order stack already paired with this window
    (get_window_stack breaks the size tie on keyboard focus); when given it is
    tried first, because two same-size windows need not sit at the same place
    and "the topmost clone of that size" is then the wrong origin for the
    lower one.

    "topmost" exists because size matching fails outright in a case that is not
    rare: AT-SPI can report a STALE window size. Cursor was observed reporting
    1280x800 while its actor said 1853x926, so nothing matched and the caller
    fell back to a (0, 0) offset — which silently makes every coordinate
    window-relative and every click miss. The window's ORIGIN was correct and
    stable at (330, 40) throughout. Actors are listed in Clutter paint order,
    so the last is the topmost window, and the window this is asked to resolve
    is the ACTIVE one — which is the topmost window. Measured over repeated
    samples, the topmost actor agreed with size matching on every scan where
    size matching succeeded (10/10), and supplied the correct origin on the
    scans where it did not. Only enable it for the active window; a guess is
    right there and would be arbitrary for a background one.
    """
    if not _IS_WAYLAND:
        return (0, 0), "x11", None
    if win_frame is None:
        return None, None, None
    w, h = win_frame["width"], win_frame["height"]
    # Painted clones only: a minimized window's stale actor keeps its old
    # size, and matching against it hands back an origin for a window that is
    # not on screen.
    live = live_actors(actors)
    if prefer is not None and prefer.get("showing", True):
        if prefer["width"] == w and prefer["height"] == h:
            return ((prefer["x"] - win_frame["x"],
                     prefer["y"] - win_frame["y"]), "exact", prefer)
        mw, mh = prefer["width"] - w, prefer["height"] - h
        if 0 <= mw <= 160 and 0 <= mh <= 160:
            return ((prefer["x"] + mw // 2 - win_frame["x"],
                     prefer["y"] + mh // 2 - win_frame["y"]), "margin", prefer)
        # A stale frame (see "topmost" below): fall through to size matching,
        # and from there to the topmost guess — which for the window on top IS
        # this clone.
    # Reversed: shell actors are listed bottom-to-top, and the window being
    # resolved is usually the active one, i.e. the topmost match.
    for a in reversed(live):  # exact size match
        if a["width"] == w and a["height"] == h:
            return (a["x"] - win_frame["x"], a["y"] - win_frame["y"]), "exact", a
    for a in reversed(live):  # actor carries a CSD/shadow margin — centre it
        mw, mh = a["width"] - w, a["height"] - h
        if 0 <= mw <= 160 and 0 <= mh <= 160:
            return (a["x"] + mw // 2 - win_frame["x"],
                    a["y"] + mh // 2 - win_frame["y"]), "margin", a
    # The topmost guess retries against the UNFILTERED list: a genuinely
    # on-screen window whose clone reports no SHOWING (fullscreen unredirect,
    # mid restore animation) must not fall through to a (0,0) offset, which
    # would silently make every coordinate window-relative.
    pool = live or actors
    if allow_topmost and pool:
        a = pool[-1]
        return (a["x"] - win_frame["x"], a["y"] - win_frame["y"]), "topmost", a
    return None, None, None


def window_offset(win_frame, actors):
    """Return (dx, dy) by size matching alone, or None. Background windows use
    this: a wrong origin there is worse than none, so they never guess."""
    return resolve_window_offset(win_frame, actors)[0]


def shell_overview_active():
    """True when GNOME Shell's overview / app grid is on screen.

    The shell is the only authority here, and it has to be asked: overview
    content does NOT carry the SHOWING state. Measured with the app grid
    open, every icon (`Clocks`, `Calculator`, `Terminal`...) reports
    showing=False while holding correct on-screen extents — and the set of
    non-SHOWING-but-on-screen shell nodes is byte-identical whether the
    overview is up or not. So geometry alone cannot tell the two apart, and
    relaxing the SHOWING filter unconditionally would paint the whole app
    grid over an ordinary desktop. This property is what makes the
    relaxation safe (see scan_shell_chrome's `overview` argument)."""
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        return bool(bus.call_sync(
            "org.gnome.Shell", "/org/gnome/Shell",
            "org.freedesktop.DBus.Properties", "Get",
            GLib.Variant("(ss)", ("org.gnome.Shell", "OverviewActive")),
            None, Gio.DBusCallFlags.NONE, 1000, None).unpack()[0])
    except Exception:
        return False   # not GNOME, or the shell is not answering


# ========== ELECTRON / CHROMIUM TREE ACTIVATION ==========

def enable_screen_reader_flag():
    """Set org.a11y.Status.IsEnabled so toolkits publish their AT-SPI trees,
    and verify by read-back. Returns True only if the flag is actually on.

    This is the master switch for the GTK/Qt/Gecko bridges, and it is
    harmless: measured, it does NOT launch Orca. ScreenReaderEnabled is
    deliberately never touched — nothing reads it (see electron_nudge), and
    clearing it makes at-spi2-core recompute IsEnabled to FALSE, switching the
    whole desktop's accessibility off. Every app launched after that comes up
    blind, which is how a Firefox started between two scans ends up absent
    from the AT-SPI desktop entirely.

    Gecko reads this flag at STARTUP ONLY, so it must be set as early as
    possible — before the user focuses or launches the app to be scanned. A
    Firefox already running while it was off cannot be rescued at runtime; it
    has to be restarted."""
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        bus.call_sync(
            "org.a11y.Bus", "/org/a11y/bus",
            "org.freedesktop.DBus.Properties", "Set",
            GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled",
                                   GLib.Variant("b", True))),
            None, Gio.DBusCallFlags.NONE, 2000, None)
        got = bus.call_sync(
            "org.a11y.Bus", "/org/a11y/bus",
            "org.freedesktop.DBus.Properties", "Get",
            GLib.Variant("(ss)", ("org.a11y.Status", "IsEnabled")),
            None, Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        if not got:
            print("  Accessibility flag did not stick — trees will be empty.")
        return bool(got)
    except Exception as e:
        print(f"  Could not enable accessibility flag: {e}")
        return False


# Turn accessibility on as soon as this module is imported — the earliest
# moment any entry point can offer.
#
# It cannot wait for prepare_scan(). Gecko reads org.a11y.Status.IsEnabled
# ONLY at startup, so a browser launched between import and the scan (during
# test.py's focus countdown, or while an agent is deciding what to do) is
# readable only if the flag was already set when it started. Doing it here
# keeps that guarantee without every caller having to remember, and keeps
# test.py a plain trigger.
#
# Safe to run at import: the write is idempotent, costs one D-Bus round trip,
# and measured does NOT launch Orca (that is ScreenReaderEnabled, which this
# never touches — see enable_screen_reader_flag). prepare_scan() calls it
# again per scan, which also recovers if something else cleared it meanwhile.
enable_screen_reader_flag()


def electron_nudge(window):
    """Runtime tree activation for Electron/Chromium apps. No launch flags, no
    per-app settings, no screen reader.

    Chromium creates a real AtkObject only for the application node and its
    toplevel frames until AXMode::kNativeAPIs is set
    (AXPlatformNodeAuraLinux::CreateAtkObject); every child below that is a
    null object, published as the literal path /org/a11y/atspi/null, which
    libatspi hands back as None. That is why an un-activated Chrome frame
    reports get_child_count() == 1 while get_child_at_index(0) is None, and
    why the whole app measures 4 nodes.

    Reading an EXTENDED property — atk_object_get_attributes, i.e.
    get_attributes() — routes through Chromium's
    BrowserAccessibilityStateImpl::OnPropertiesUsedInBrowserUI(), which turns
    kNativeAPIs on process-wide and the real tree appears. Role, state,
    extents, name and text do NOT trip it, so an ordinary walk never wakes
    Chrome, and neither does the bulk Cache.GetItems path (its item struct
    carries no attributes).

    Measured on Google Chrome 152, fresh instance, twice independently:
        control, no probe   -> 1 node, still 1 after 4.9s
        get_attributes()    -> 1 -> 218 nodes in 0.54s

    This replaces an earlier pulse of org.a11y.Status.ScreenReaderEnabled.
    That property is read by nothing on the machine — `strings | grep -c` over
    chrome, Electron (cursor), Firefox's libxul, libatk-bridge and libatspi
    all return 0 — so the pulse cost a fixed NUDGE_TIMEOUT + 1s and achieved
    nothing, and its revert switched the desktop's accessibility off globally
    (see enable_screen_reader_flag). Gecko is a different mechanism again: it
    reads org.a11y.Status.IsEnabled at STARTUP ONLY, which is why
    enable_screen_reader_flag() must run early and stay on.

    Returns True when it activated a tree (the caller then waits for that
    tree to settle), False when there was nothing to do."""
    if not ELECTRON_NUDGE:
        return False
    # >4 nodes means a real tree already (a bare Electron frame is 2-3
    # nodes; even a minimal GTK dialog exceeds this) — nothing to activate.
    if _quick_node_count(window, cap=6) > 4:
        return False

    # Only Chromium has this gate, and only Chromium is worth waiting for.
    # Asking the toolkit costs one D-Bus read and keeps a sparse GTK/Qt/Java
    # window — or a minimized one — from spending the whole timeout on a
    # probe that cannot help it.
    try:
        parent = window.get_parent()
    except Exception:
        parent = None
    toolkit = ""
    for src in (parent, window):
        if src is None:
            continue
        try:
            toolkit = (src.get_toolkit_name() or "").lower()
        except Exception:
            toolkit = ""
        if toolkit:
            break
    if toolkit and "chromium" not in toolkit:
        return False

    # The frame first, then the application node: on some builds only one of
    # the two carries the attribute set that trips the gate.
    targets = [window]
    if parent is not None:
        targets.append(parent)

    probed = False
    for target in targets:
        try:
            target.get_attributes()
            probed = True
        except Exception:
            continue
    if not probed:
        return False

    deadline = time.time() + NUDGE_TIMEOUT
    while time.time() < deadline:
        if _quick_node_count(window, cap=40) > 4:
            print("  Chromium tree activated.")
            return True
        time.sleep(0.1)
    return True


def _quick_node_count(acc, cap=300):
    """Cheaply estimate subtree size, stopping at `cap` nodes.

    The depth guard is MAX_DEPTH, not something tighter, because `cap` is what
    bounds the cost — the guard only has to stop a cyclic tree. A tighter one
    silently answers a different question: this is a depth-first stack, so an
    8-deep guard walked one long chain and reported 17 nodes for a 2900-node
    Electron window, which left every caller believing the tree was empty.
    """
    count = 0
    stack = [(acc, 0)]
    while stack and count < cap:
        node, depth = stack.pop()
        count += 1
        if depth >= MAX_DEPTH:
            continue
        for child in iter_children(node):
            stack.append((child, depth + 1))
    return count


def wait_for_tree_ready(window):
    """Poll until the window's AT-SPI subtree stops growing (Chromium builds
    it lazily after the screen-reader flag flips).

    Small trees are legitimate (a plain GTK dialog is ~7 nodes), so "stable"
    means four consecutive equal counts — enough for a freshly flagged
    Electron tree to start growing, without stalling the full timeout on
    every small static window."""
    fast = _get_fast()
    if fast is not None:
        try:
            bus, path = _bus_path_of(window)
            if path is not None:
                deadline = time.time() + A11Y_TREE_READY_TIMEOUT
                prev2 = prev = last = -1
                while time.time() < deadline:
                    count = _fast_node_count(fast, bus, path)
                    if count >= 300 or count == last == prev == prev2:
                        return
                    if last >= 0 and count > last:
                        print(f"  Waiting for accessibility tree... "
                              f"({count} nodes)")
                    prev2, prev, last = prev, last, count
                    time.sleep(A11Y_TREE_READY_INTERVAL_FAST)
                return
        except Exception:
            pass   # no Cache interface on this toolkit — poll per-node

    deadline = time.time() + A11Y_TREE_READY_TIMEOUT
    prev2 = prev = last = -1
    while time.time() < deadline:
        count = _quick_node_count(window)
        if count >= 300 or count == last == prev == prev2:
            return
        if last >= 0 and count > last:
            print(f"  Waiting for accessibility tree... ({count} nodes)")
        prev2, prev, last = prev, last, count
        time.sleep(A11Y_TREE_READY_INTERVAL)


# ========== BROWSER PAGE-LOAD WAIT ==========

def _sync_browser_documents(window, max_depth=10, cap=800):
    """[{kind, name, nchild, busy}] for the web documents (kind "doc") and
    Gecko tab frames (kind "frame") under `window`, by a per-node walk — the
    fallback when the fast bus is unavailable. Descends only through SHOWING
    nodes (a background tab's document is not what the user sees) and never
    into a document (its iframes are its own affair)."""
    out = []
    stack = [(window, 0)]
    n = 0
    while stack and n < cap:
        acc, depth = stack.pop()
        n += 1
        try:
            role = acc.get_role_name() or ""
        except Exception:
            continue
        states = acc_states(acc)
        if depth > 0 and (states is None
                          or not states.contains(_STATE.SHOWING)):
            continue
        if role in BROWSER_DOCUMENT_ROLES or role == BROWSER_FRAME_ROLE:
            try:
                nchild = acc.get_child_count()
            except Exception:
                nchild = 0
            if role in BROWSER_DOCUMENT_ROLES:
                out.append({"kind": "doc", "name": _safe_name(acc),
                            "nchild": nchild,
                            "busy": bool(states is not None
                                         and states.contains(_STATE.BUSY))})
                continue
            out.append({"kind": "frame", "name": "", "nchild": nchild,
                        "busy": False})
        if depth < max_depth:
            for child in iter_children(acc):
                stack.append((child, depth + 1))
    return out


def _fast_browser_documents(fast, bus, win_path, max_depth=10, cap=800):
    """The fast twin of _sync_browser_documents: a breadth-first descent,
    one pipelined wave per level (role, state and children of every node on
    the level). Chrome keeps ~160 nodes above its document — ~0.4s of
    synchronous reads, a few tens of ms here — and this runs on every poll
    of the load wait.

    Not GetItems: neither browser keeps its page in org.a11y.atspi.Cache
    (measured: Chrome's cache holds 4 items; Gecko caches the frame and
    leaves the document a hole), so the bulk cache cannot answer this."""
    frontier = [win_path]
    out = []
    seen = 0
    for depth in range(max_depth + 1):
        if not frontier or seen >= cap:
            break
        calls = []
        for p in frontier:
            calls.append(((p, "ro"), bus, p, _IFACE_ACCESSIBLE, "GetRole",
                          None))
            calls.append(((p, "st"), bus, p, _IFACE_ACCESSIBLE, "GetState",
                          None))
            calls.append(((p, "ch"), bus, p, _IFACE_ACCESSIBLE, "GetChildren",
                          None))
        got = fast.wave(calls, wave_timeout=3.0)
        nxt = []
        docs = []
        for p in frontier:
            seen += 1
            r = got.get((p, "ro"))
            s = got.get((p, "st"))
            c = got.get((p, "ch"))
            role = _role_name(r[0]) if r else ""
            states = _pack_states(s[0]) if s else 0
            kids = [kp for _b, kp in c[0]] if c else []
            if depth > 0 and not (states >> _BIT_SHOWING) & 1:
                continue
            if role in BROWSER_DOCUMENT_ROLES:
                docs.append((p, len(kids), bool((states >> _BIT_BUSY) & 1)))
                continue
            if role == BROWSER_FRAME_ROLE:
                out.append({"kind": "frame", "name": "", "nchild": len(kids),
                            "busy": False})
            nxt.extend(kids)
        if docs:
            names = fast.wave(
                (p, bus, p, _IFACE_PROPS, "Get",
                 GLib.Variant("(ss)", (_IFACE_ACCESSIBLE, "Name")))
                for p, _n, _b in docs)
            for p, nchild, busy in docs:
                r = names.get(p)
                out.append({"kind": "doc",
                            "name": (r[0] if r and isinstance(r[0], str)
                                     else ""),
                            "nchild": nchild, "busy": busy})
        frontier = nxt[:max(0, cap - seen)]
    return out


def _browser_documents(window):
    """The web documents under a browser window — fast path first, the
    per-node walk when the bus or the window's path is unavailable."""
    fast = _get_fast()
    if fast is not None:
        try:
            bus, path = _bus_path_of(window)
            if path is not None:
                return _fast_browser_documents(fast, bus, path)
        except Exception:
            pass
    return _sync_browser_documents(window)


def _browser_load_state(entries, window_name=""):
    """'ready', 'busy' (page arrived, still loading), 'pending' (navigation
    waiting on the network: Chromium's empty document, or a Gecko tab frame
    with no document in it yet) or 'none' (neither a document nor a frame
    under the window — no web content).

    Chromium's about:blank is an empty, unnamed document too, and would
    read as pending until the budget ran out; its window is titled with the
    literal URL ("about:blank - Google Chrome", measured), which is not
    localized, so that title is the one exception."""
    docs = [d for d in entries if d["kind"] == "doc"]
    if docs:
        if any(d["busy"] for d in docs):
            return "busy"
        if all(d["nchild"] == 0 and not d["name"] for d in docs) \
                and not window_name.lower().startswith("about:blank"):
            return "pending"
        return "ready"
    if any(d["kind"] == "frame" for d in entries):
        return "pending"
    return "none"


def wait_for_browser_load(window):
    """Block until the browser window's page has loaded — the Linux twin of
    the macOS scanner's _wait_for_ax_web_content. Returns the seconds the
    scan was held: 0.0 when the page was already loaded at the first look,
    otherwise how long it waited (settle included), whether the page then
    loaded or the budget ran out. The scan goes ahead either way, since a
    late tree still beats none — but a non-zero return tells the caller the
    screen was mid-change when the scan began, so any frame captured before
    this returned shows the page on its way out, not the one the tree will
    describe (see UIElementScanner._recapture).

    The page's own AT-SPI document is the only thing consulted — no
    toolbar strings (Chrome's "Stop loading this page" tooltip and
    Firefox's "Stop" button are English-only), no screenshots. The
    BROWSER_* constants say what each state looks like and how long it may
    last; the budgets run from one start. A window with neither a document
    nor a tab frame has no web content (a bare popup) and gets
    BROWSER_DOC_GRACE; a page on its way — pending on the network, or
    arrived and BUSY — gets BROWSER_LOAD_TIMEOUT. about:blank is the one
    page that looks pending forever, and pays the full budget."""
    t0 = time.time()
    budgets = {"none": BROWSER_DOC_GRACE,
               "pending": BROWSER_LOAD_TIMEOUT,
               "busy": BROWSER_LOAD_TIMEOUT}
    waiting = {"none": "  Browser: no web document yet — waiting...",
               "pending": "  Browser: navigation pending (page not arrived) "
                          "— waiting...",
               "busy": "  Browser: page is loading — waiting..."}
    announced = None
    state = "none"
    while True:
        state = _browser_load_state(_browser_documents(window),
                                    _safe_name(window))
        if state == "ready" or time.time() - t0 >= budgets[state]:
            break
        if state != announced:
            print(waiting[state])
            announced = state
        time.sleep(BROWSER_LOAD_INTERVAL)
    waited = time.time() - t0
    if state == "ready":
        if announced is None:
            print("  Browser: page already loaded.")
            return 0.0
        print(f"  Browser: page loaded after {waited:.1f}s.")
        # A navigation was genuinely in flight — one beat for the fresh
        # page to finish compositing before the capture.
        time.sleep(BROWSER_SETTLE)
        return time.time() - t0
    print({"none": "  Browser: no web content in this window — scanning "
                   "as-is.",
           "pending": f"  Browser: page has not arrived after {waited:.1f}s "
                      f"— scanning anyway.",
           "busy": f"  Browser: page still loading after {waited:.1f}s — "
                   f"scanning anyway."}[state])
    return waited


# ========== FAST BULK READER (org.a11y.atspi.Cache) ==========
# The scan's fast path. The portable way to read an AT-SPI tree is one
# synchronous D-Bus round-trip per fact — get_role_name(), get_state_set(),
# get_extents(), each ~1.3ms — and a walk touches thousands of nodes, which
# is why this scanner once cost 5-6s while the Windows/macOS scanners, whose
# accessibility APIs fetch in bulk, finish in a fraction of that. AT-SPI has
# the same machinery, just not where the convenience API points: every
# toolkit bridge (atk-bridge for GTK and Electron/Chromium, cally for
# gnome-shell, Qt's adaptor, modern Gecko) serves
# org.a11y.atspi.Cache.GetItems, which returns the ENTIRE application tree —
# name, role, description, state set, parent link, child count, interfaces —
# in ONE round-trip. Measured here: gnome-shell, 1952 nodes in 0.029s; an
# Electron window, 631 nodes in 0.011s. The walk then becomes Python over
# local data, and the only per-node traffic left is what the cache cannot
# carry — extents and text — fetched as PIPELINED async calls: 0.12ms/node
# measured, against 1.31ms/node synchronous.
#
# FRESHNESS. The bridges update their cache on every tree/state/name change
# (it is what keeps a screen reader current), and that was verified rather
# than assumed: diffing GetItems against live GetState + Name for every node
# of a running Electron app and of gnome-shell found zero differences. What
# the cache can miss is whole LAZY SUBTREES — nodes whose parent advertises
# more children than the cache holds (measured: 15 holes hiding 114 nodes on
# one Electron window). _resolve_holes() fills those with pipelined
# GetChildren waves before the walk, skipping holes under non-SHOWING nodes
# when the caller prunes those subtrees anyway.
#
# CONTRACT. Everything here mirrors the sync walk's semantics — same config
# tables, same prune rules, same label fallback chains, same output dicts —
# so every piece can fall back to the sync path on its own (an exception
# here is never fatal to a scan) and FASTSCAN=False A/B runs stay
# comparable. Two deliberate divergences: `acc_element` in results is a
# (bus_name, path) tuple rather than an Atspi.Accessible, since raw refs are
# what this reader holds (nothing consumes the field today; a controller can
# drive org.a11y.atspi.Action straight from the ref); and when one node
# needs text for both its label and its value, one 200-char read serves both
# instead of the sync path's separate 120- and 200-char reads.
#
# Role names come from Atspi.role_get_name's table rather than per-node
# GetRoleName calls. Verified against live GetRoleName across every role
# code in use on this desktop: one cosmetic mismatch ("statusbar" vs
# "status bar"), a role ELEMENT_CONFIG does not track under either spelling.

_APP_ROOT = "/org/a11y/atspi/accessible/root"

_IFACE_ACCESSIBLE = "org.a11y.atspi.Accessible"
_IFACE_COMPONENT = "org.a11y.atspi.Component"
_IFACE_TEXT = "org.a11y.atspi.Text"
_IFACE_CACHE = "org.a11y.atspi.Cache"
_IFACE_PROPS = "org.freedesktop.DBus.Properties"

# Same space as the sync walk — see _ATSPI_COORD. The fast path must agree
# with acc_extents or the two produce different geometry for the same node.
_COORD_SCREEN = GLib.Variant("(u)", (int(_ATSPI_COORD),))

# State bit positions inside the cache's packed two-word state set.
_BIT_SHOWING = int(Atspi.StateType.SHOWING)
_BIT_VISIBLE = int(Atspi.StateType.VISIBLE)
_BIT_ENABLED = int(Atspi.StateType.ENABLED)
_BIT_SENSITIVE = int(Atspi.StateType.SENSITIVE)
_BIT_EDITABLE = int(Atspi.StateType.EDITABLE)
_BIT_FOCUSED = int(Atspi.StateType.FOCUSED)
_BIT_BUSY = int(Atspi.StateType.BUSY)

_role_names = {}


def _role_name(code):
    """AT-SPI role code → the role-name string ELEMENT_CONFIG is keyed by."""
    name = _role_names.get(code)
    if name is None:
        try:
            name = Atspi.role_get_name(Atspi.Role(code)) or ""
        except Exception:
            name = ""
        _role_names[code] = name
    return name


class _Node:
    """One accessible, as cached facts. `states` packs both u32 words."""
    __slots__ = ("path", "name", "desc", "role", "states", "idx", "nchild",
                 "ifaces", "children")

    def __init__(self, path, name="", desc="", role="", states=0, idx=0,
                 nchild=0, ifaces=()):
        self.path = path
        self.name = name
        self.desc = desc
        self.role = role
        self.states = states
        self.idx = idx
        self.nchild = nchild
        self.ifaces = ifaces
        self.children = []

    def has_state(self, bit):
        return (self.states >> bit) & 1


class _FastA11y:
    """Raw connection to the accessibility bus. Raises if it cannot
    connect — the caller treats that as 'no fast path' and walks sync."""

    def __init__(self):
        session = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        addr = session.call_sync(
            "org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress",
            None, None, Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        self.conn = Gio.DBusConnection.new_for_address_sync(
            addr,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
            | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None, None)

    # ---- pipelined batch ----

    def wave(self, calls, per_call_timeout=3000, wave_timeout=10.0):
        """Fire every call at once, collect replies as they land.

        `calls`: iterable of (key, bus, path, iface, method, args_variant).
        Returns {key: unpacked_tuple_or_None}; a failed or timed-out call is
        None — the same "that fact is unavailable" the sync helpers express
        by swallowing an exception into a default.

        Runs on a PRIVATE GLib.MainContext, pushed thread-default for the
        duration: Gio routes each call's completion to the context that was
        thread-default when call() was made, so replies land here and
        nowhere else. The isolation matters — the portal screenshot runs a
        loop on the global default context, and a stray source firing there
        must not be able to wake or quit a wave.
        """
        calls = list(calls)
        out = {}
        if not calls:
            return out
        ctx = GLib.MainContext.new()
        ctx.push_thread_default()
        state = {"pending": 0}

        def _mk(key):
            def cb(conn, res, _ud=None):
                try:
                    out[key] = conn.call_finish(res).unpack()
                except Exception:
                    out[key] = None
                state["pending"] -= 1
            return cb

        try:
            for key, bus, path, iface, method, params in calls:
                state["pending"] += 1
                self.conn.call(bus, path, iface, method, params, None,
                               Gio.DBusCallFlags.NONE, per_call_timeout,
                               None, _mk(key))
            timed_out = []
            src = GLib.timeout_source_new(int(wave_timeout * 1000))
            src.set_callback(lambda *a: timed_out.append(1) and False)
            src.attach(ctx)
            try:
                deadline = time.monotonic() + wave_timeout
                while state["pending"] > 0 and not timed_out \
                        and time.monotonic() < deadline:
                    ctx.iteration(True)
            finally:
                src.destroy()
        finally:
            ctx.pop_thread_default()
        return out

    def extents_wave(self, bus, nodes):
        """{path: (x, y, w, h)} for every node that has the Component
        interface and reports positive size — None entries dropped."""
        got = self.wave(
            (n.path, bus, n.path, _IFACE_COMPONENT, "GetExtents",
             _COORD_SCREEN)
            for n in nodes if _IFACE_COMPONENT in n.ifaces)
        out = {}
        for path, r in got.items():
            if not r:
                continue
            x, y, w, h = r[0]
            if 0 < w <= _MAX_SANE_EXTENT and 0 < h <= _MAX_SANE_EXTENT:
                out[path] = (x, y, w, h)
        return out

    # ---- one-shot reads ----

    def get_items(self, bus):
        """The whole app tree in one round-trip. Raises if the toolkit does
        not serve the Cache interface — the caller falls back to sync."""
        v = self.conn.call_sync(
            bus, "/org/a11y/atspi/cache", _IFACE_CACHE, "GetItems",
            None, None, Gio.DBusCallFlags.NONE, 10000, None)
        return v.unpack()[0]

    def registry_apps(self):
        """[(bus_name, root_path)] of every application on the bus."""
        v = self.conn.call_sync(
            "org.a11y.atspi.Registry", _APP_ROOT,
            _IFACE_ACCESSIBLE, "GetChildren",
            None, None, Gio.DBusCallFlags.NONE, 2000, None)
        return [(b, p) for b, p in v.unpack()[0]]

    def find_app_bus(self, name):
        """Bus name of the application whose root is named `name`."""
        apps = self.registry_apps()
        got = self.wave(
            (bus, bus, path, _IFACE_PROPS, "Get",
             GLib.Variant("(ss)", (_IFACE_ACCESSIBLE, "Name")))
            for bus, path in apps)
        for bus, _path in apps:
            r = got.get(bus)
            if r and r[0] == name:
                return bus
        return None


_FAST = None
_FAST_FAILED = False


def _get_fast(retry=False):
    """The process-wide _FastA11y, or None when the fast path is off or the
    bus is unreachable — which just means 'walk synchronously', never an
    error. scan_elements passes retry=True once per scan so a transient
    connection failure does not pin the process to the slow walk forever."""
    global _FAST, _FAST_FAILED
    if not FASTSCAN:
        return None
    if _FAST is not None:
        return _FAST
    if _FAST_FAILED and not retry:
        return None
    try:
        _FAST = _FastA11y()
        _FAST_FAILED = False
    except Exception:
        _FAST = None
        _FAST_FAILED = True
    return _FAST


def _bus_path_of(acc):
    """(bus_name, path_or_None) of an Atspi.Accessible.

    PyGObject exposes libatspi's public struct fields, so acc.path and
    acc.app.bus_name read directly (verified on this PyGObject). The
    fallback matches the accessible's process id against the bus
    connections' pids — struct fields are an implementation detail worth
    not trusting unconditionally. The fallback cannot recover the object
    PATH, so it returns (bus, None) and the caller re-matches the window
    inside the cached tree by index/name.
    """
    try:
        bus = acc.app.bus_name
        path = acc.path
        if bus and path:
            return bus, path
    except Exception:
        pass
    fast = _get_fast()
    if fast is None:
        raise RuntimeError("no fast a11y connection")
    pid = acc.get_process_id()
    for bus, _root in fast.registry_apps():
        try:
            got = fast.conn.call_sync(
                "org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", "GetConnectionUnixProcessID",
                GLib.Variant("(s)", (bus,)), None,
                Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
        except Exception:
            continue
        if got == pid:
            return bus, None
    raise RuntimeError(f"no a11y bus connection for pid {pid}")


def _pack_states(pair):
    try:
        lo, hi = (list(pair) + [0, 0])[:2]
        return (int(lo) & 0xFFFFFFFF) | ((int(hi) & 0xFFFFFFFF) << 32)
    except Exception:
        return 0


def _build_app_tree(fast, bus, items=None, resolve_hidden=False):
    """{path: _Node} for one application, cache holes resolved.

    `resolve_hidden=False` leaves holes under non-SHOWING nodes alone —
    correct for callers with walk() semantics, which prune those subtrees
    anyway. gnome-shell's chrome scan does not prune, so it passes True.
    """
    if items is None:
        items = fast.get_items(bus)
    nodes = {}
    parent_of = {}
    explicit = {}   # path -> ordered child paths (the a(so) cache layout)
    for it in items:
        # Two cache-item layouts exist in the wild: index + child count
        # ("(so)(so)(so)iiassusau", every bridge on this machine) and an
        # explicit child list ("(so)(so)(so)a(so)assusau"). The 4th field
        # tells them apart.
        obj, _app, parent = it[0], it[1], it[2]
        if isinstance(it[3], int):
            idx, nchild, ifaces, name, role, desc, states = it[3:10]
        else:
            refs, ifaces, name, role, desc, states = it[3:9]
            explicit[obj[1]] = [p for _b, p in refs]
            idx, nchild = 0, len(refs)
        nodes[obj[1]] = _Node(
            obj[1], name=name or "", desc=desc or "", role=_role_name(role),
            states=_pack_states(states), idx=idx, nchild=nchild,
            ifaces=tuple(ifaces or ()))
        parent_of[obj[1]] = parent[1]

    if explicit:
        for path, refs in explicit.items():
            kids = [nodes[p] for p in refs if p in nodes][:MAX_CHILDREN]
            for i, c in enumerate(kids):
                c.idx = i
            nodes[path].children = kids
    else:
        linked = {}
        for path, node in nodes.items():
            pp = parent_of.get(path)
            if pp is not None and pp != path and pp in nodes:
                linked.setdefault(pp, []).append(node)
        for pp, kids in linked.items():
            kids.sort(key=lambda n: n.idx)
            nodes[pp].children = kids[:MAX_CHILDREN]

    _resolve_holes(fast, bus, nodes, resolve_hidden)
    return nodes


def _resolve_holes(fast, bus, nodes, resolve_hidden, max_rounds=30):
    """Fetch the subtrees the cache lazily skipped, in pipelined waves.

    A hole is a node advertising more children than the cache delivered.
    Non-SHOWING holes stay unfetched unless resolve_hidden — the walk
    prunes those subtrees, so fetching them would be pure cost (closed
    menus are the big case: every phantom item, uncached AND unrendered).
    """
    root = nodes.get(_APP_ROOT)

    def _wants_descent(node):
        # The root and its windows always resolve: walk() exempts depth 0
        # from the SHOWING prune, so a not-yet-SHOWING window still scans.
        # Checked live against root.children, which _adopt can rebuild.
        return (resolve_hidden or node.has_state(_BIT_SHOWING)
                or node is root
                or (root is not None
                    and any(node is c for c in root.children)))

    frontier = []   # freshly created stubs awaiting their facts

    def _adopt(parent, refs):
        kids = []
        for i, p in enumerate(refs[:MAX_CHILDREN]):
            child = nodes.get(p)
            if child is None:
                child = _Node(p, idx=i)
                nodes[p] = child
                frontier.append(child)
            else:
                child.idx = i
            kids.append(child)
        parent.children = kids
        parent.nchild = len(kids)

    holes = [n for n in nodes.values()
             if n.nchild > len(n.children) and _wants_descent(n)]
    if not holes:
        return
    got = fast.wave((n.path, bus, n.path, _IFACE_ACCESSIBLE, "GetChildren",
                     None) for n in holes)
    for n in holes:
        r = got.get(n.path)
        if r:
            _adopt(n, [p for _b, p in r[0]])

    rounds = 0
    while frontier and rounds < max_rounds and len(nodes) < MAX_NODES:
        rounds += 1
        batch, frontier = frontier, []
        calls = []
        for n in batch:
            calls.append(((n.path, "st"), bus, n.path, _IFACE_ACCESSIBLE,
                          "GetState", None))
            calls.append(((n.path, "ro"), bus, n.path, _IFACE_ACCESSIBLE,
                          "GetRole", None))
            calls.append(((n.path, "if"), bus, n.path, _IFACE_ACCESSIBLE,
                          "GetInterfaces", None))
            calls.append(((n.path, "na"), bus, n.path, _IFACE_PROPS, "Get",
                          GLib.Variant("(ss)", (_IFACE_ACCESSIBLE, "Name"))))
            calls.append(((n.path, "de"), bus, n.path, _IFACE_PROPS, "Get",
                          GLib.Variant("(ss)",
                                       (_IFACE_ACCESSIBLE, "Description"))))
            calls.append(((n.path, "ch"), bus, n.path, _IFACE_ACCESSIBLE,
                          "GetChildren", None))
        got = fast.wave(calls)
        for n in batch:
            r = got.get((n.path, "st"))
            n.states = _pack_states(r[0]) if r else 0
            r = got.get((n.path, "ro"))
            n.role = _role_name(r[0]) if r else ""
            r = got.get((n.path, "if"))
            n.ifaces = tuple(r[0]) if r else ()
            r = got.get((n.path, "na"))
            n.name = (r[0] or "") if r else ""
            r = got.get((n.path, "de"))
            n.desc = (r[0] or "") if r else ""
            r = got.get((n.path, "ch"))
            if r and _wants_descent(n):
                _adopt(n, [p for _b, p in r[0]])
            elif r:
                n.nchild = len(r[0])


def _fast_node_count(fast, bus, win_path, cap=300):
    """Node count under one window — the fast _quick_node_count. One
    GetItems, no hole resolution: wait_for_tree_ready polls this, so cheap
    beats complete (holes only under-count, and growth is what it wants
    to see)."""
    kids = {}
    present = set()
    for it in fast.get_items(bus):
        present.add(it[0][1])
        if isinstance(it[3], int):
            kids.setdefault(it[2][1], []).append(it[0][1])
        else:
            kids[it[0][1]] = [p for _b, p in it[3]]
    if win_path not in present:
        return 0
    count = 0
    stack = [(win_path, 0)]
    seen = set()
    while stack and count < cap:
        path, depth = stack.pop()
        if path in seen:
            continue
        seen.add(path)
        count += 1
        if depth >= MAX_DEPTH:
            continue
        stack.extend((p, depth + 1) for p in kids.get(path, ()))
    return count


def _label_pass(cfg, node, text):
    """build_label over cached facts. `text` None means "not fetched yet":
    if the fallback chain reaches _text before resolving, the answer is
    (None, needs_text=True) and the caller queues a fetch."""
    for attr in cfg.get("fallback", []):
        if attr == "name":
            val = node.name
        elif attr == "description":
            val = node.desc
        elif attr == "_text":
            if _IFACE_TEXT not in node.ifaces:
                val = ""          # sync acc_text on a non-Text node → ""
            elif text is None:
                return None, True
            else:
                val = text
        else:
            val = ""
        if val:
            label = _clean_label(str(val))
            if not label or label.lower() in GENERIC_LABELS:
                continue
            return (label[:50] if len(label) > 50 else label), False
    return cfg.get("default_label", ""), False


def _walk_plan(node, depth, budget, visit):
    """The traversal skeleton of walk(): same depth guard, budget, child
    cap, role backlog and SHOWING/menu prune. It runs twice per scan with
    identical decisions — pass 1 collects fetch targets, pass 2 emits
    results with the fetched extents/text in hand — which works because no
    prune here depends on anything fetched between the passes."""
    if depth > MAX_DEPTH:
        return
    if budget is not None:
        if budget[0] <= 0:
            return
        budget[0] -= 1
    _seen_roles.add(node.role)
    showing = node.has_state(_BIT_SHOWING)
    if not showing and (node.role == "menu"
                        or (PRUNE_HIDDEN and depth > 0)):
        return
    visit(node, depth, showing)
    for child in node.children[:MAX_CHILDREN]:
        _walk_plan(child, depth + 1, budget, visit)


def _candidate_filter(node, showing, is_browser=False):
    """The state half of walk()'s track test (geometry applies after the
    extents wave)."""
    cfg = ELEMENT_CONFIG.get(node.role)
    if not (cfg and cfg.get("track") and showing
            and node.has_state(_BIT_VISIBLE)):
        return None
    if cfg.get("is_enabled_flag") and not (node.has_state(_BIT_ENABLED)
                                           or node.has_state(_BIT_SENSITIVE)):
        return None
    if node.role == "text" and not node.has_state(_BIT_EDITABLE):
        return None
    # Web content keeps its text only while focused — see walk().
    if is_browser and not cfg.get("is_enabled_flag") \
            and not node.has_state(_BIT_FOCUSED):
        return None
    return cfg


class _TwoPassWalk:
    """Scout → fetch → emit over a set of planned (window, offset) walks.

    Pass 1 records which nodes need extents (tracked candidates and clip
    roles) and which need text (label chains that reach _text, and value
    reads). One extents wave and at most two text waves later, pass 2 runs
    the identical traversal and emits walk()-shaped result dicts.
    """

    def __init__(self, fast, bus, screen, source="", is_browser=False):
        self.fast = fast
        self.bus = bus
        self.screen = screen
        self.source = source
        self.is_browser = is_browser
        self.need_extents = []
        self.candidates = {}     # path -> (node, cfg)
        self.frames = {}
        self.labels = {}
        self.texts = {}

    def _scout(self, node, _depth, showing):
        cfg = _candidate_filter(node, showing, self.is_browser)
        if cfg is not None:
            self.candidates[node.path] = (node, cfg)
            self.need_extents.append(node)
        elif node.role in CLIP_ROLES:
            self.need_extents.append(node)

    def scout(self, plan, budget_size):
        budget = [budget_size]
        for win, _off in plan:
            _walk_plan(win, 0, budget, self._scout)

    def fetch(self):
        self.frames = self.fast.extents_wave(self.bus, self.need_extents)
        text_need = {}
        for path, (node, cfg) in self.candidates.items():
            rf = self.frames.get(path)
            if rf is None or rf[2] < 3 or rf[3] < 3:
                continue     # fails walk()'s geometry test under any offset
            label, needs_text = _label_pass(cfg, node, None)
            self.labels[path] = None if needs_text else label
            if needs_text:
                text_need[path] = 120
            if node.role in VALUE_ROLES and _IFACE_TEXT in node.ifaces:
                text_need[path] = 200    # value read; serves the label too
        self.texts = _fetch_texts(self.fast, self.bus, text_need)
        for path, label in list(self.labels.items()):
            if label is None:
                node, cfg = self.candidates[path]
                self.labels[path], _ = _label_pass(
                    cfg, node, self.texts.get(path, ""))

    def emit(self, plan, budget_size, results, tag_win_seq=False):
        budget = [budget_size]
        for seq, (win, off) in enumerate(plan):
            clip_stack = [None]

            def _visit(node, depth, _showing, _off=off, _seq=seq,
                       _stack=clip_stack):
                # Clip bookkeeping mirrors walk()'s recursion: entries above
                # this depth belong to subtrees already left behind.
                del _stack[depth + 1:]
                clip = _stack[-1]
                rf = self.frames.get(node.path)
                my_frame = None
                if rf is not None:
                    my_frame = {"x": rf[0] + _off[0], "y": rf[1] + _off[1],
                                "width": rf[2], "height": rf[3]}
                child_clip = clip
                if node.role in CLIP_ROLES and my_frame:
                    child_clip = (_rect_intersect(clip, my_frame) or clip) \
                        if clip else my_frame
                _stack.append(child_clip)

                label = self.labels.get(node.path)
                if not label or my_frame is None \
                        or my_frame["width"] < 3 or my_frame["height"] < 3:
                    return
                vis_str, vis_rect = _visibility(my_frame, child_clip,
                                                self.screen)
                if vis_str == "hidden":
                    return
                value = None
                if node.role in VALUE_ROLES:
                    v = _clean_value(self.texts.get(node.path, ""))
                    if v and v.lower() != label.lower():
                        value = v[:200]
                entry = {
                    "type": node.role,
                    "label": label,
                    "value": value,
                    "x": my_frame["x"], "y": my_frame["y"],
                    "width": my_frame["width"], "height": my_frame["height"],
                    "depth": depth,
                    "visibility": vis_str,
                    "visible_rect_raw": vis_rect,
                    "acc_element": (self.bus, node.path),
                    "source": self.source,
                }
                if tag_win_seq:
                    entry["_win_seq"] = _seq
                results.append(entry)

            _walk_plan(win, 0, budget, _visit)


def _fetch_texts(fast, bus, need):
    """{path: single-line text} for the requested nodes — a CharacterCount
    wave, then a GetText wave, replicating acc_text's shaping."""
    if not need:
        return {}
    counts = fast.wave(
        (path, bus, path, _IFACE_PROPS, "Get",
         GLib.Variant("(ss)", (_IFACE_TEXT, "CharacterCount")))
        for path in need)
    fetch = {}
    for path, limit in need.items():
        r = counts.get(path)
        n = r[0] if r and isinstance(r[0], int) else 0
        if n > 0:
            fetch[path] = min(n, limit)
    got = fast.wave(
        (path, bus, path, _IFACE_TEXT, "GetText",
         GLib.Variant("(ii)", (0, end)))
        for path, end in fetch.items())
    out = {}
    for path in need:
        r = got.get(path)
        s = r[0] if r else ""
        out[path] = (s or "").replace("\n", " ").strip()
    return out


# One gnome-shell tree per scan. The window-actor pass and the chrome scan
# both need gnome-shell's cached tree with every hole resolved, and each
# ran its own GetItems + hole waves — measured 0.22-0.26 s per build and
# three builds per scan (actors, the stale re-read, chrome), so a third of
# prepare_scan + collect_elements was the same 1952-item decode repeated.
# The snapshot carries no geometry — every consumer fetches extents fresh —
# so sharing it moves no coordinates. scan_elements() clears it; the two
# prepare_scan paths that exist to re-read after time has passed (the
# staleness check, the animation resettle) ask for a fresh build.
_SHELL_TREE = {}   # bus -> nodes, for the scan in progress


def _shell_tree(fast, fresh=False):
    """(bus, nodes) for gnome-shell, built once per scan; (None, None)
    when gnome-shell is not on the bus."""
    bus = fast.find_app_bus("gnome-shell")
    if bus is None:
        return None, None
    nodes = None if fresh else _SHELL_TREE.get(bus)
    if nodes is None:
        nodes = _build_app_tree(fast, bus, resolve_hidden=True)
        _SHELL_TREE.clear()
        _SHELL_TREE[bus] = nodes
    return bus, nodes


def _fast_shell_window_actors(fast, fresh=False):
    """Fast twin of _collect_shell_window_actors: the 'Wayland window'
    clone geometries. Pre-order DFS of the cached tree matches the sync
    walk's traversal, so the list keeps Clutter paint order — which
    resolve_window_offset's topmost fallback depends on."""
    bus, nodes = _shell_tree(fast, fresh)
    if nodes is None:
        return []
    root = nodes.get(_APP_ROOT)
    if root is None:
        return []

    clones = []

    def _walk(node, depth):
        if depth > 6:
            return
        if node.role == "panel" and node.name == "Wayland window":
            clones.append(node)
            return   # window clones have no useful children
        for child in node.children:
            _walk(child, depth + 1)

    _walk(root, 0)
    ext = fast.extents_wave(bus, clones)
    return [{"x": e[0], "y": e[1], "width": e[2], "height": e[3],
             "showing": n.has_state(_BIT_SHOWING) and n.has_state(_BIT_VISIBLE)}
            for n, e in ((n, ext.get(n.path)) for n in clones) if e]


# AT-SPI relation type numbers (Atspi.RelationType) for the two that name a
# control. Hard-coded because the cache speaks raw D-Bus, not the GI enum.
_REL_LABELLED_BY = int(Atspi.RelationType.LABELLED_BY)
_REL_DESCRIBED_BY = int(Atspi.RelationType.DESCRIBED_BY)


def _fast_relation_labels(fast, bus, nodes):
    """{path: name} for anonymous nodes that name themselves via a relation.

    Two waves — GetRelationSet, then Name on whatever it points at — instead
    of the sync walk, which costs 6.8s on gnome-shell versus 0.28s here.
    Only anonymous nodes are asked, so this is a handful of calls."""
    if not nodes:
        return {}
    rels = fast.wave((n.path, bus, n.path, _IFACE_ACCESSIBLE, "GetRelationSet", None)
                     for n in nodes)
    want = {}                      # path -> (bus, target_path)
    for path, r in rels.items():
        if not r:
            continue
        for entry in (r[0] or []):
            try:
                rtype, targets = entry[0], entry[1]
            except Exception:
                continue
            if rtype not in (_REL_LABELLED_BY, _REL_DESCRIBED_BY):
                continue
            for tgt in targets:
                want[path] = (tgt[0], tgt[1])
                break
            if path in want:
                break
    if not want:
        return {}
    names = fast.wave(
        (path, tb, tp, _IFACE_PROPS, "Get",
         GLib.Variant("(ss)", (_IFACE_ACCESSIBLE, "Name")))
        for path, (tb, tp) in want.items())
    out = {}
    for path, r in names.items():
        if r and isinstance(r[0], str) and r[0]:
            out[path] = r[0]
    return out


def _fast_scan_shell_chrome(fast, screen, overview=False):
    """Fast twin of scan_shell_chrome — same tracked roles, strip split,
    phantom-tooltip drop and generic-name filter, over cached facts.

    resolve_hidden=True because the sync chrome walk descends through
    non-SHOWING containers (only the tracked node itself must be SHOWING),
    so cache holes under them must be filled too.
    """
    bus, nodes = _shell_tree(fast)
    if nodes is None:
        return [], []
    root = nodes.get(_APP_ROOT)
    if root is None:
        return [], []

    picked = []   # (node, depth)

    max_depth = 16 if overview else 12

    parent_of = {}

    def _walk(node, depth, parent=None):
        if depth > max_depth:
            return
        parent_of[node.path] = parent
        tracked = node.role in SHELL_TRACK_ROLES or (
            overview and node.role in OVERVIEW_EXTRA_ROLES)
        name = node.name if (tracked or node.role == "panel") else ""
        if name == "Wayland window":
            return   # client window clones — walked separately with offsets
        named_ok = name and name.lower() not in GENERIC_LABELS
        anon_ok = overview and not name and node.role in (
            "button", "toggle button", "push button", "text")
        if tracked and (overview or node.has_state(_BIT_SHOWING)) \
                and (named_ok or anon_ok):
            picked.append((node, depth))
        for child in node.children:
            _walk(child, depth + 1, node)

    _walk(root, 0, None)
    ext = fast.extents_wave(bus, [n for n, _d in picked])
    anon = [n for n, _d in picked if not n.name] if overview else []
    rel_names = _fast_relation_labels(fast, bus, anon)
    # Extents of the parents of relation-named nodes, so a bare inner text can
    # be promoted to the box drawn around it (see _promotable).
    promo = {}
    if rel_names:
        parents = {n.path: parent_of.get(n.path) for n in anon
                   if n.path in rel_names and parent_of.get(n.path) is not None}
        pext = fast.extents_wave(bus, list({p.path: p for p in parents.values()}.values()))
        for path, par in parents.items():
            e = pext.get(par.path)
            if e:
                promo[path] = {"x": e[0], "y": e[1], "width": e[2], "height": e[3]}
    texts = _fetch_texts(fast, bus, {n.path: 50 for n in anon
                                     if n.role == "text"
                                     and n.path not in rel_names}) \
        if overview else {}

    menu_items, elements = [], []
    for node, depth in picked:
        e = ext.get(node.path)
        if e is not None and node.path in promo:
            inner = {"x": e[0], "y": e[1], "width": e[2], "height": e[3]}
            box = promo[node.path]
            if _promotable(inner, box):
                e = (box["x"], box["y"], box["width"], box["height"])
        if e is not None and overview and _rect_intersect(
                {"x": e[0], "y": e[1], "width": e[2], "height": e[3]},
                screen) is None:
            e = None            # off-screen grid page
        in_strip = e is not None and \
            e[1] + e[3] <= screen["y"] + MENU_STRIP_BOTTOM
        # Labels outside the top bar are dock/dash hover-tooltips — SHOWING
        # even when not displayed. The icons themselves are separate nodes.
        # In the overview they are the app names, and the only geometry there.
        if node.role == "label" and not in_strip and not overview:
            e = None
        if not e:
            continue
        item = {
            "_showing": node.has_state(_BIT_SHOWING),
            "type": node.role,
            "label": (node.name or rel_names.get(node.path)
                      or texts.get(node.path, ""))[:50],
            "value": None,
            "x": e[0], "y": e[1], "width": e[2], "height": e[3],
            "depth": depth,
            "visibility": "full",
            "visible_rect_raw": None,
            "acc_element": (bus, node.path),
            "source": "shell",
        }
        (menu_items if in_strip else elements).append(item)
    menu_items.sort(key=lambda m: m["x"])
    return menu_items, elements


def _fast_scan_desktop_icons(fast, screen, results, actors):
    """Fast twin of scan_desktop_icons: find Desktop Icons windows across
    the bus, occlusion-test them, walk the visible ones."""
    apps = fast.registry_apps()
    wins = fast.wave((bus, bus, path, _IFACE_ACCESSIBLE, "GetChildren",
                      None) for bus, path in apps)
    name_calls = []
    for bus, _path in apps:
        r = wins.get(bus)
        if not r:
            continue
        for _b, wpath in r[0]:
            name_calls.append(((bus, wpath), bus, wpath, _IFACE_PROPS,
                               "Get",
                               GLib.Variant("(ss)",
                                            (_IFACE_ACCESSIBLE, "Name"))))
    names = fast.wave(name_calls)
    targets = [key for key, r in names.items()
               if r and is_desktop_window(r[0])]
    if not targets:
        return

    exts = fast.wave(
        ((bus, path), bus, path, _IFACE_COMPONENT, "GetExtents",
         _COORD_SCREEN)
        for bus, path in targets)
    trees = {}
    for bus, path in targets:
        r = exts.get((bus, path))
        wf = None
        if r and r[0][2] > 0 and r[0][3] > 0:
            x, y, w, h = r[0]
            wf = {"x": x, "y": y, "width": w, "height": h}
        off = window_offset(wf, actors) if wf else None
        # Fullscreen desktop windows normally match an actor exactly; the
        # primary really does sit at the origin, so (0,0) is sound there.
        off = off or (0, 0)
        desk_frame = ({"x": wf["x"] + off[0], "y": wf["y"] + off[1],
                       "width": wf["width"], "height": wf["height"]}
                      if wf else None)
        occluders = _desktop_occluders(actors, desk_frame)
        # Whole-monitor test first — a maximised window covers the desktop
        # completely, which skips the walk outright.
        if desk_frame and occluders and _visible_fraction_after_occluders(
                desk_frame, occluders) < 0.01:
            continue

        if bus not in trees:
            trees[bus] = _build_app_tree(fast, bus)
        win = trees[bus].get(path)
        if win is None:
            continue
        mark = len(results)
        walker = _TwoPassWalk(fast, bus, screen, source="desktop")
        plan = [(win, off)]
        walker.scout(plan, 2000)
        walker.fetch()
        walker.emit(plan, 2000, results)
        _trim_desktop_occluded(results, mark, occluders)


# ========== TREE WALK ==========

_seen_roles = set()


def walk(acc, results, depth, screen, offset=(0, 0), clip=None, budget=None,
         source="", is_browser=False):
    """Recursively walk the AT-SPI tree, collecting ELEMENT_CONFIG matches.

    `offset` is the Wayland window-origin correction applied to every extent.
    `clip` is the innermost scroll-pane viewport rect (already corrected).
    `budget` is a single-element list holding the remaining node budget.
    `is_browser` applies the web-content rule: roles without an enabled
    flag are kept only while focused (see BROWSER_PROCESS_NAMES).
    """
    if depth > MAX_DEPTH:
        return
    if budget is not None:
        if budget[0] <= 0:
            return
        budget[0] -= 1

    try:
        role_str = acc.get_role_name() or ""
    except Exception:
        return
    _seen_roles.add(role_str)
    states = acc_states(acc)
    showing = states is not None and states.contains(_STATE.SHOWING)

    # A node that is not being rendered prunes its whole subtree. AT-SPI
    # propagates SHOWING downwards, so nothing under an unrendered ancestor is
    # rendered either — probed across a 2900-node Electron tree, the number of
    # SHOWING nodes sitting under a non-SHOWING ancestor was zero. Since a
    # non-SHOWING node is never tracked anyway (see the track test below),
    # descending only bought a D-Bus round-trip per node on the way to
    # discarding it: 70% of that tree. Closed menus are the extreme case, as
    # they publish every phantom item they contain.
    #
    # depth 0 is exempt so a caller handing us a window that has not been
    # marked SHOWING yet gets the old behaviour rather than silently nothing —
    # except for a closed menu, which was already pruned here before.
    if not showing and (role_str == "menu"
                        or (PRUNE_HIDDEN and depth > 0)):
        return

    my_frame = None
    cfg = ELEMENT_CONFIG.get(role_str)
    if cfg or role_str in CLIP_ROLES:
        my_frame = acc_extents(acc, offset)

    child_clip = clip
    if role_str in CLIP_ROLES and my_frame:
        child_clip = (_rect_intersect(clip, my_frame) or clip) if clip else my_frame

    if cfg and cfg.get("track") and my_frame and showing \
            and states.contains(_STATE.VISIBLE) \
            and my_frame["width"] >= 3 and my_frame["height"] >= 3:
        skip = False
        if cfg.get("is_enabled_flag") \
                and not states.contains(_STATE.ENABLED) \
                and not states.contains(_STATE.SENSITIVE):
            skip = True
        # Multi-line "text" is only tracked when editable — otherwise Chromium
        # spams read-only text runs, and GTK TextViews are covered by "_text".
        if role_str == "text" and not states.contains(_STATE.EDITABLE):
            skip = True
        # A browser's web content: a role with no enabled flag (label,
        # static, heading, filler) is text, and text is kept only while it
        # holds keyboard focus — the macOS scanner's AXFocused rule. The
        # controls stay; OCR reads the prose.
        if is_browser and not cfg.get("is_enabled_flag") \
                and not states.contains(_STATE.FOCUSED):
            skip = True

        if not skip:
            label = build_label(acc, cfg)
            if label:
                vis_str, vis_rect = _visibility(my_frame, child_clip, screen)
                if vis_str != "hidden":
                    value = None
                    if role_str in VALUE_ROLES:
                        v = _clean_value(acc_text(acc, 200))
                        if v and v.lower() != label.lower():
                            value = v[:200]
                    results.append({
                        "type": role_str,
                        "label": label,
                        "value": value,
                        "x": my_frame["x"], "y": my_frame["y"],
                        "width": my_frame["width"], "height": my_frame["height"],
                        "depth": depth,
                        "visibility": vis_str,
                        "visible_rect_raw": vis_rect,
                        "acc_element": acc,
                        "source": source,
                    })

    for child in iter_children(acc):
        walk(child, results, depth + 1, screen, offset, child_clip, budget,
             source, is_browser)


# ========== GNOME SHELL CHROME (top bar / dock) ==========

def scan_shell_chrome(screen, overview=False):
    """Collect labelled items from GNOME Shell's own UI. Items in the top
    strip become menu-bar entries — the top bar is where GNOME puts what other
    desktops put in a menu bar; the rest (dash/dock icons, desktop widgets) are
    regular elements.

    `overview` relaxes two rules that would otherwise hide the whole app grid,
    and must only ever be set from shell_overview_active():
      - SHOWING is not required. The shell never sets it on overview content.
      - Labels outside the top strip are kept. Normally they are dash
        hover-tooltips and dropped; in the overview they are the app names,
        and they carry the only usable geometry (the icon buttons above them
        report no extents at all).
    The depth limit is lifted too: grid icons sit at depth 12-14, past the
    limit that suffices for the top bar and dash."""
    fast = _get_fast()
    if fast is not None:
        try:
            return _fast_scan_shell_chrome(fast, screen, overview)
        except Exception as e:
            print(f"  Fast chrome scan unavailable ({e}) — walking "
                  f"gnome-shell.")

    menu_items = []
    elements = []
    shell = find_app("gnome-shell")
    if shell is None:
        return menu_items, elements

    max_depth = 16 if overview else 12

    def _walk(acc, depth):
        if depth > max_depth:
            return
        try:
            role_str = acc.get_role_name() or ""
        except Exception:
            return
        # The name is only ever needed to label a tracked node or to spot a
        # "Wayland window" clone, and clones are always panels — so an
        # anonymous container costs one D-Bus read here instead of two.
        tracked = role_str in SHELL_TRACK_ROLES or (
            overview and role_str in OVERVIEW_EXTRA_ROLES)
        name = _safe_name(acc) if (tracked or role_str == "panel") else ""
        if name == "Wayland window":
            return  # client window clones — walked separately with offsets
        if tracked:
            states = acc_states(acc)
            visible = states is not None and (
                overview or states.contains(_STATE.SHOWING))
            if visible:
                ext = acc_extents(acc)
                if overview and ext is not None \
                        and _rect_intersect(ext, screen) is None:
                    ext = None      # off-screen grid page
                in_strip = ext is not None and \
                    ext["y"] + ext["height"] <= screen["y"] + MENU_STRIP_BOTTOM
                # Labels outside the top bar are dock/dash hover-tooltips —
                # SHOWING even when not displayed. The icons themselves are
                # separate nodes, so drop the phantom labels. In the overview
                # they are the app names and the only thing with geometry.
                if role_str == "label" and not in_strip and not overview:
                    ext = None
                if ext and overview and not name:
                    # An anonymous control names itself through a relation.
                    name = _relation_label(acc)
                    if name:
                        # The named node is often just the inner text; the box
                        # around it is what is drawn and clicked.
                        try:
                            pext = acc_extents(acc.get_parent())
                        except Exception:
                            pext = None
                        if _promotable(ext, pext):
                            ext = pext
                    elif role_str == "text":
                        name = acc_text(acc)
                # Unnamed nodes are kept in the overview: the app-grid tiles
                # and window thumbnails are anonymous 113x113 / 298x166
                # buttons whose name lives in a separate label INSIDE them,
                # adopted later by _adopt_overview_labels.
                keep = bool(ext) and (
                    (name and name.lower() not in GENERIC_LABELS)
                    or (overview and not name and role_str in
                        ("button", "toggle button", "push button")))
                if keep:
                    live = states.contains(_STATE.SHOWING)
                    item = {
                        "_showing": live,
                        "type": role_str,
                        "label": name[:50],
                        "value": None,
                        "x": ext["x"], "y": ext["y"],
                        "width": ext["width"], "height": ext["height"],
                        "depth": depth,
                        "visibility": "full",
                        "visible_rect_raw": None,
                        "acc_element": acc,
                        "source": "shell",
                    }
                    if in_strip:
                        menu_items.append(item)
                    else:
                        elements.append(item)
        for child in iter_children(acc):
            _walk(child, depth + 1)

    try:
        _walk(shell, 0)
    except Exception:
        pass
    menu_items.sort(key=lambda m: m["x"])
    return menu_items, elements


def _rects_match(a, b, tol=8):
    """True if two rects describe the same window, within a few pixels."""
    return (abs(a["x"] - b["x"]) <= tol and abs(a["y"] - b["y"]) <= tol
            and abs(a["width"] - b["width"]) <= tol
            and abs(a["height"] - b["height"]) <= tol)


def _desktop_occluders(actors, desk_frame):
    """The window rects that paint over a desktop window.

    That is every visible toplevel except the desktop's own clone. Exactly ONE
    match is dropped, not all of them: a genuinely fullscreen app has the same
    geometry as the desktop, and removing every match would let it hide inside
    the exclusion and occlude nothing.
    """
    out, skipped = [], False
    # Painted clones only: a minimized window keeps its actor at last-known
    # geometry, and counting it as an occluder deletes every desktop icon
    # underneath it.
    for a in live_actors(actors):
        if not skipped and desk_frame and _rects_match(a, desk_frame):
            skipped = True
            continue
        out.append(a)
    return out


def scan_desktop_icons(screen, results, actors):
    """Walk the Desktop Icons (DING) windows — one toplevel per monitor.

    The desktop is the BOTTOM surface: every ordinary window paints over it.
    AT-SPI still reports its icons as SHOWING when a maximised browser covers
    them, so without an occlusion test the agent is handed a numbered box for
    a folder it cannot click and the click lands in the browser instead.
    gnome-shell publishes the real geometry of every visible toplevel as the
    same actors used for the origin correction, so that is the occluder set.

    On X11 `actors` is empty (the origin correction is not needed there), so
    no occlusion is applied and every desktop icon is reported as before.
    """
    fast = _get_fast()
    if fast is not None:
        mark = len(results)
        try:
            _fast_scan_desktop_icons(fast, screen, results, actors)
            return
        except Exception as e:
            del results[mark:]   # scrub a half-finished fast attempt
            print(f"  Fast desktop scan unavailable ({e}) — using the "
                  f"slow walk.")

    for app in get_desktop_apps():
        for win in iter_children(app):
            if not is_desktop_window(_safe_name(win)):
                continue
            wf = acc_extents(win)
            off = window_offset(wf, actors) if wf else None
            # Fullscreen desktop windows normally match an actor exactly;
            # the primary really does sit at the origin, so (0,0) is a
            # sound fallback there.
            off = off or (0, 0)
            desk_frame = acc_extents(win, off) if wf else None
            occluders = _desktop_occluders(actors, desk_frame)

            # Whole-monitor test first. A maximised window covers the desktop
            # completely, which is the common case, so this usually skips the
            # DING traversal outright rather than walking it to throw it away.
            if desk_frame and occluders and _visible_fraction_after_occluders(
                    desk_frame, occluders) < 0.01:
                continue

            mark = len(results)
            walk(win, results, 0, screen, off, budget=[2000], source="desktop")
            _trim_desktop_occluded(results, mark, occluders)


def _trim_desktop_occluded(results, mark, occluders):
    """Drop or downgrade the desktop-walk results at results[mark:] that sit
    behind ordinary windows. Shared by the sync and fast desktop scans."""
    if not occluders:
        return
    kept = []
    for e in results[mark:]:
        rect = {"x": e["x"], "y": e["y"],
                "width": e["width"], "height": e["height"]}
        hits = [o for o in occluders if _rect_intersect(rect, o)]
        if not hits:
            kept.append(e)
            continue
        frac = _visible_fraction_after_occluders(rect, hits)
        if frac < 0.01:
            continue  # behind a window — not clickable
        if frac < 0.99:
            e["visibility"] = f"partial {int(frac * 100)}%"
        kept.append(e)
    results[mark:] = kept


def _visible_fraction_after_occluders(rect, occluders, samples=12):
    """Uncovered-area fraction of rect (0.0..1.0), by sampling a 12x12 grid of
    points and asking how many land inside an occluder."""
    if rect["width"] <= 0 or rect["height"] <= 0:
        return 0.0
    if not occluders:
        return 1.0
    step_x = rect["width"] / samples
    step_y = rect["height"] / samples
    covered = 0
    for i in range(samples):
        px = rect["x"] + (i + 0.5) * step_x
        for j in range(samples):
            py = rect["y"] + (j + 0.5) * step_y
            for occ in occluders:
                if (occ["x"] <= px <= occ["x"] + occ["width"]
                        and occ["y"] <= py <= occ["y"] + occ["height"]):
                    covered += 1
                    break
    return (samples * samples - covered) / (samples * samples)


def _apply_sibling_occlusion(results, win_frames):
    """Recompute visibility of elements covered by later-walked windows of
    the same app. AT-SPI exposes
    no global z-order, but within one app the sibling windows walked after
    the active one are its dialogs/popovers, which paint on top. Elements
    from other sources (shell, desktop) carry no _win_seq and pass through."""
    if len(win_frames) < 2:
        for e in results:
            e.pop("_win_seq", None)
        return results
    out = []
    for e in results:
        seq = e.pop("_win_seq", None)
        occluders = win_frames[seq + 1:] if seq is not None else []
        if not occluders:
            out.append(e)
            continue
        rect = {"x": e["x"], "y": e["y"],
                "width": e["width"], "height": e["height"]}
        occluders = [o for o in occluders if _rect_intersect(rect, o)]
        if not occluders:
            out.append(e)
            continue
        frac = _visible_fraction_after_occluders(rect, occluders)
        vr = e.get("visible_rect_raw")
        walk_frac = (vr["width"] * vr["height"]) / max(
            1, rect["width"] * rect["height"]) if vr else 1.0
        final = walk_frac * frac
        if final < 0.01:
            continue  # fully behind a dialog — not clickable
        if final < 0.99:
            e["visibility"] = f"partial {int(final * 100)}%"
        out.append(e)
    return out


# ========== THE WINDOW ITSELF: ITS VISIBLE RECT, ITS CLIP, ITS BOX ==========

def _showing_children(acc):
    """The SHOWING children of an accessible, per node — the sync twin of
    filtering _Node.children on _BIT_SHOWING."""
    out = []
    for child in iter_children(acc):
        st = acc_states(child)
        if st is not None and st.contains(_STATE.SHOWING):
            out.append(child)
    return out


def _window_role(acc):
    try:
        return acc.get_role_name() or "frame"
    except Exception:
        return "frame"


# The most a client-side shadow can inset a window inside its own frame —
# the bound resolve_window_offset already trusts for a "margin" actor match.
_SHADOW_MAX_INSET = 160


def _window_client_rect(frame, node, children_of, rects_of, elements,
                        max_depth=4):
    """The rect the user SEES of a window whose AT-SPI frame is `frame`.

    A Wayland client publishes its whole buffer as its frame, and a
    client-side-decorated Chromium window paints its drop shadow inside that
    buffer. Measured on Chrome: the frame is 1037x883 while the window on
    screen is a 1005x841 panel inset 16/10/16/32 px within it. Nothing
    clipped to the window before this, so a footer link cut off by the
    window's bottom edge came out "full", and two lines of the terminal
    standing under the shadow reached the tree as the window's own OCR text.

    The visible window is found the way Chromium lays it out: frame > panel
    > panel, each the frame's own size, then the client panel inset on all
    four sides. Descend through SHOWING children that match the frame
    (transparent wrappers) and take the largest child inset on every side
    by 1..160 px that covers at least half the frame. GTK frames already
    exclude their shadow (their actor is the larger one — the "margin"
    match), so their content fills the frame and nothing qualifies.

    One guard: every element walked from the window must still touch the
    candidate. A libadwaita dialog sheet is an inset child of the same
    shape, but the window's own controls lie outside it; a shadow margin
    holds nothing. Falls back to the frame."""
    fw, fh = frame["width"], frame["height"]
    if fw <= 0 or fh <= 0:
        return frame
    best = None
    level = [node]
    for _ in range(max_depth):
        kids = [c for n in level for c in children_of(n)]
        if not kids:
            break
        level = []
        for kid, r in zip(kids, rects_of(kids)):
            if not r or r["width"] <= 0 or r["height"] <= 0:
                continue
            if _rects_match(r, frame, tol=2):
                level.append(kid)          # a wrapper: look inside it
                continue
            insets = (r["x"] - frame["x"], r["y"] - frame["y"],
                      frame["x"] + fw - (r["x"] + r["width"]),
                      frame["y"] + fh - (r["y"] + r["height"]))
            if min(insets) < 1 or max(insets) > _SHADOW_MAX_INSET:
                continue
            if r["width"] * r["height"] < 0.5 * fw * fh:
                continue
            if best is None \
                    or r["width"] * r["height"] > best["width"] * best["height"]:
                best = r
        if not level:
            break
    if best is None:
        return frame
    for e in elements:
        rect = {"x": e["x"], "y": e["y"],
                "width": e["width"], "height": e["height"]}
        if _rect_intersect(rect, best) is None:
            return frame
    return {"x": best["x"], "y": best["y"],
            "width": best["width"], "height": best["height"]}


def _clip_base(frame, ctx):
    """The active window's frame — unless its origin came from the topmost
    guess. That path exists because AT-SPI reported a size no actor had (a
    stale 1280x800 for a Cursor painted at 1853x926), and clipping to a
    stale, smaller frame would delete real elements. The paired actor is
    the painted truth there."""
    actor = ctx.get("actor")
    if ctx.get("offset_how") == "topmost" and actor:
        return {"x": actor["x"], "y": actor["y"],
                "width": actor["width"], "height": actor["height"]}
    return frame


# A "margin" origin centres the frame inside its actor (resolve_window_offset)
# and is a pixel off when the shadow is uneven — measured: Text Editor's
# "Open" button at x=642 in a window placed at 643. A cut that thin is
# placement error, not a cut, and must not read as "partial 98%".
_EDGE_SLACK = 2


def _clip_to_window(elements, client, screen):
    """Re-derive each element's visibility inside the window's visible rect.
    Walk-time visibility knows only scroll viewports and the screen; the
    window's own edge is applied here, once that rect is known. An element
    with nothing inside the window is dropped; one cut by its edge becomes
    "partial N%" with the remainder as its click target (the controller
    clicks the centre of visible_rect)."""
    kept = []
    for e in elements:
        rect = {"x": e["x"], "y": e["y"],
                "width": e["width"], "height": e["height"]}
        vis = e.get("visible_rect_raw") or rect
        clip = _rect_intersect(vis, client)
        if clip is None:
            continue
        if max(clip["x"] - vis["x"], clip["y"] - vis["y"],
               vis["x"] + vis["width"] - clip["x"] - clip["width"],
               vis["y"] + vis["height"] - clip["y"] - clip["height"]) \
                <= _EDGE_SLACK:
            clip = vis                    # a sliver: placement error, no cut
        vis_str, vis_rect = _visibility(rect, clip, screen)
        if vis_str == "hidden":
            continue
        e["visibility"] = vis_str
        e["visible_rect_raw"] = vis_rect
        kept.append(e)
    return kept


def _window_element(title, role, client, acc, seq, source=""):
    """The walked window itself, as the element that boxes everything in it
    — the macOS and Windows trees open each window this way, and the agent
    reads the title and the nesting from it. Sized to the VISIBLE rect."""
    return {
        "type": role or "frame",
        "label": title,
        "value": None,
        "x": client["x"], "y": client["y"],
        "width": client["width"], "height": client["height"],
        "depth": 0,
        "visibility": "full",
        "visible_rect_raw": None,
        "acc_element": acc,
        "source": source,
        "_win_seq": seq,
    }


def _finish_window(elements, frame, title, role, node, acc, seq, screen,
                   children_of, rects_of):
    """After a window's walk: find its visible rect, clip its elements to
    it, and put the window in front of them as their box. Returns
    (visible_rect, elements); the rect is what this window occludes with
    from here on (see _apply_sibling_occlusion) and, for the active window,
    what OCR is confined to."""
    client = _window_client_rect(frame, node, children_of, rects_of, elements)
    kept = _clip_to_window(elements, client, screen)
    kept.insert(0, _window_element(title, role, client, acc, seq))
    return client, kept


# ========== SCAN PHASES ==========

def prepare_scan(screen):
    """Phase 1: everything that can change on-screen pixels or tree contents
    (screen-reader flag flip, lazy-tree wait) runs BEFORE the screenshot."""
    enable_screen_reader_flag()
    # Z-order first, so the window ON TOP is the one scanned.
    t_actors = time.time()
    actors = _collect_shell_window_actors() if _IS_WAYLAND else []
    # One desktop enumeration, shared by the z-order stack and the blind-top
    # audit below. Each costs ~0.15s of synchronous D-Bus, so doing it per
    # caller doubled prepare_scan for no new information.
    cands = candidate_toplevels()
    # `actor` is the clone the z-order stack paired with the window. It is
    # threaded through to the offset and occlusion passes rather than
    # re-derived by size there — two maximized windows share a size, and
    # re-matching picked the wrong twin (see _actor_owner).
    app, window, actor = find_top_window(actors, cands, narrate=True)
    ctx = {"app": app, "window": window, "offset": (0, 0), "actors": actors,
           "offset_how": None, "blind_top": None, "desktop_only": False,
           "overview": shell_overview_active(), "is_browser": False,
           "browser_waited": 0.0}
    if window is None:
        # Nothing but the desktop is on screen — THIS is the Desktop case, and
        # the only one in which desktop icons are foreground UI. The overview
        # is the exception: it is fullscreen shell UI painted OVER the
        # desktop, so the icons beneath it are not visible and not clickable,
        # and emitting them put boxes on things the user could not see.
        ctx["desktop_only"] = not ctx["overview"]
        return ctx

    # An actor on top that owns no AT-SPI toplevel is a window that is on
    # screen but off the accessibility bus — the scan is about to describe
    # something BEHIND it. Say so rather than reporting a confident answer.
    live = live_actors(actors)
    _showing = [c for c in cands if c[3].contains(_STATE.SHOWING)]
    if live and not is_desktop_window_actor(live[-1], actors) \
            and _actor_owner(live[-1], _showing) is None:
        top = live[-1]
        ctx["blind_top"] = dict(top)
        print(f"  WARNING: the topmost window ({top['width']}x{top['height']} "
              f"at {top['x']},{top['y']}) belongs to no AT-SPI application, so "
              f"it cannot be scanned. Reporting {_safe_name(app)!r}, which is "
              f"behind it. If this is a browser or a dialog, restart it now "
              f"that accessibility is on.")

    # A toolkit whose tree must not be touched (Flutter, see
    # unwalkable_toolkit): no nudge, no readiness poll — both enumerate
    # children — and no walk later. OCR carries the window.
    ctx["no_walk"] = unwalkable_toolkit(app)
    ctx["is_browser"] = not ctx["no_walk"] and is_browser_app(app)
    if ctx["no_walk"]:
        print(f"  {_safe_name(app)!r} is a {ctx['no_walk']} app: its "
              f"accessibility tree is not walked (walking it crashes the "
              f"app) — OCR carries the window.")
    else:
        nudged = electron_nudge(window)
        # A web browser: hold the scan until its page has loaded. After the
        # nudge — Chromium publishes no document to watch until its tree is
        # activated — and before the readiness poll, which then sees the
        # loaded page's tree settle rather than the previous page's.
        if ctx["is_browser"]:
            ctx["browser_waited"] = wait_for_browser_load(window)
        # The readiness poll is for a tree still being built: one the nudge
        # just activated, or a browser page that just loaded. Everywhere
        # else it was a fixed tax — four equal counts 0.12 s apart, 0.37 s
        # on every scan — and on an Electron app it could not even see
        # growth: Chromium's cache serves ~2 items, so the fast count is
        # tiny and constant whatever the tree is doing.
        if nudged or ctx["is_browser"]:
            wait_for_tree_ready(window)
    # Re-read the actors only if enough time has passed for the geometry to
    # have moved — activating a lazy tree can take a second and windows
    # animate. When the tree was already up, the list above is still current
    # and re-collecting it is ~0.18s of pure duplicate D-Bus work.
    if _IS_WAYLAND and time.time() - t_actors > _ACTOR_STALE_AFTER:
        ctx["actors"] = _collect_shell_window_actors(fresh=True)
        # Fresh clone dicts: the pairing must be redone against them, since
        # identity is what the occlusion pass keys on.
        actor = _paired_actor(window, ctx["actors"], cands)
    actors = ctx["actors"]
    win_frame = acc_extents(window)
    if win_frame:
        off, how, act = resolve_window_offset(win_frame, ctx["actors"],
                                              prefer=actor)
        if off is None:
            # The window may be mid restore/move animation, leaving the shell
            # actor's size out of step with the frame — resettle and retry.
            time.sleep(0.7)
            win_frame = acc_extents(window) or win_frame
            ctx["actors"] = _collect_shell_window_actors(fresh=True)
            actor = _paired_actor(window, ctx["actors"], cands)
            off, how, act = resolve_window_offset(win_frame, ctx["actors"],
                                                  prefer=actor)
        if off is None:
            # Still nothing after resettling, so this is not an animation: the
            # window is reporting a size no actor has. Fall back to the
            # topmost actor, which IS this window — see resolve_window_offset.
            off, how, act = resolve_window_offset(
                win_frame, ctx["actors"], allow_topmost=True, prefer=actor)
        if off is None:
            # No actors at all (no gnome-shell). A window-relative scan of the
            # active window still beats no scan.
            print("  Window origin unresolved — coordinates may be window-relative.")
            off, how, act = (0, 0), "none", None
        ctx["offset"] = off
        ctx["offset_how"] = how
        ctx["actor"] = act
    return ctx


def _collect_app_windows_sync(app, window, ctx, screen, results):
    """The per-node walk of the active window and its sibling windows —
    the fallback when the bulk fast path is unavailable. Appends result
    dicts tagged with _win_seq; returns win_frames in walk order."""
    offset = ctx["offset"]
    is_browser = ctx.get("is_browser", False)
    win_frame = acc_extents(window, offset) or dict(screen)
    budget = [MAX_NODES]
    win_frames = []              # walk order ≈ stacking order (dialogs above)
    app_name = _safe_name(app)

    def _finish(mark, frame, win, off, seq):
        """Box and clip the window just walked into results[mark:]; returns
        its visible rect, which is what win_frames carries from here."""
        elems = results[mark:]
        for e in elems:
            e["_win_seq"] = seq
        client, kept = _finish_window(
            elems, frame, _safe_name(win) or app_name, _window_role(win),
            win, win, seq, screen, _showing_children,
            lambda kids, _o=off: [acc_extents(k, _o) for k in kids])
        results[mark:] = kept
        return client

    mark = len(results)
    walk(window, results, 0, screen, offset, clip=None, budget=budget,
         is_browser=is_browser)
    win_frames.append(_finish(mark, _clip_base(win_frame, ctx), window,
                              offset, 0))

    # Other on-screen windows of the same app (dialogs, popups) — Wayland
    # gives no position for them, so only walk when a shell actor matches.
    for win in iter_children(app):
        if win is window:
            continue
        states = acc_states(win)
        if states is None or not states.contains(_STATE.SHOWING):
            continue
        wf = acc_extents(win)
        if not wf:
            continue
        off = window_offset(wf, ctx["actors"])
        if off is None:
            continue  # unresolved origin — wrong coords are worse than none
        placed = acc_extents(win, off) or wf
        if _rects_match(placed, win_frame, tol=2):
            # A same-size sibling (two maximized Chrome windows) resolves by
            # size to the active window's OWN clone. It is really behind the
            # window in front and cannot be placed; walked here it would sit
            # on top of it and _apply_sibling_occlusion would delete every
            # element of the window the user is looking at.
            continue
        mark = len(results)
        walk(win, results, 0, screen, off, clip=None, budget=budget,
             is_browser=is_browser)
        win_frames.append(_finish(mark, acc_extents(win, off) or wf, win,
                                  off, len(win_frames)))
    return win_frames


def _collect_app_windows_fast(fast, window, ctx, screen, results):
    """Same contract as _collect_app_windows_sync, through the bulk cache:
    walk the active window, then every sibling window with a resolvable
    origin, appending walk()-shaped dicts tagged with _win_seq. Returns
    win_frames in walk order (= stacking order)."""
    bus, active_path = _bus_path_of(window)
    nodes = _build_app_tree(fast, bus)
    root = nodes.get(_APP_ROOT)
    active = nodes.get(active_path) if active_path else None
    if active is None and root is not None:
        # _bus_path_of could not read the path fields — re-match the window
        # among the root's children by index, name-checked, then by name.
        try:
            idx = window.get_index_in_parent()
        except Exception:
            idx = -1
        active_name = _safe_name(window)
        wins = root.children
        if 0 <= idx < len(wins) \
                and (not active_name or wins[idx].name == active_name):
            active = wins[idx]
        else:
            named = [w for w in wins if w.name == active_name]
            if len(named) == 1:
                active = named[0]
    if active is None:
        raise RuntimeError("active window not found in cached tree")
    windows = root.children if root is not None else [active]

    # Window frames first: sibling walks need offsets before planning.
    win_ext = fast.extents_wave(bus, windows)

    def _frame(w, offset=(0, 0)):
        rf = win_ext.get(w.path)
        if rf is None:
            return None
        return {"x": rf[0] + offset[0], "y": rf[1] + offset[1],
                "width": rf[2], "height": rf[3]}

    plan = [(active, ctx["offset"])]
    raw_frames = [None]  # per-plan-entry uncorrected frame (sibling fallback)
    active_frame = _frame(active, ctx["offset"])
    for w in windows:
        if w is active or not w.has_state(_BIT_SHOWING):
            continue
        wf = _frame(w)
        if not wf:
            continue
        off = window_offset(wf, ctx["actors"])
        if off is None:
            continue  # unresolved origin — wrong coords are worse than none
        placed = _frame(w, off)
        if active_frame and placed and _rects_match(placed, active_frame, tol=2):
            # A same-size sibling lands on the active window's own clone —
            # it is behind the window in front and cannot be placed. See
            # the sync walk for why walking it here deletes the front.
            continue
        plan.append((w, off))
        raw_frames.append(wf)

    walker = _TwoPassWalk(fast, bus, screen,
                          is_browser=ctx.get("is_browser", False))
    walker.scout(plan, MAX_NODES)
    walker.fetch()
    mark = len(results)
    walker.emit(plan, MAX_NODES, results, tag_win_seq=True)
    walked = results[mark:]
    del results[mark:]

    def _showing_kids(node):
        return [c for c in node.children if c.has_state(_BIT_SHOWING)]

    # Box and clip each window (see _finish_window); win_frames carries the
    # visible rects. One extents wave per wrapper level, a few nodes each.
    app_name = root.name if root is not None else ""
    win_frames = []
    for i, (win, off) in enumerate(plan):
        fallback = dict(screen) if i == 0 else raw_frames[i]
        frame = _frame(win, off) or fallback
        if i == 0:
            frame = _clip_base(frame, ctx)

        def _rects_of(kids, _off=off):
            ext = fast.extents_wave(bus, kids)
            return [{"x": r[0] + _off[0], "y": r[1] + _off[1],
                     "width": r[2], "height": r[3]} if r else None
                    for r in (ext.get(k.path) for k in kids)]

        client, kept = _finish_window(
            [e for e in walked if e.get("_win_seq") == i], frame,
            win.name or app_name, win.role, win, (bus, win.path), i, screen,
            _showing_kids, _rects_of)
        results.extend(kept)
        win_frames.append(client)
    return win_frames


def _adopt_overview_labels(items):
    """Give each anonymous overview tile the label that sits inside it.

    GNOME publishes an app-grid entry as an unnamed 113x113 `button` with a
    separate `label` node inside it, and a window thumbnail as an unnamed
    298x166 `button` the same way. Neither carries a name of its own, so the
    walk used to emit only the 89x19 label — a text-sized click target under
    an icon nobody could see, which is what the annotated screenshot showed.

    Each label is adopted by the SMALLEST tile containing it, so a label is
    not claimed by an outer container when a tighter one exists, and the
    adopted label is then dropped: it is the same target as the tile."""
    tiles = [e for e in items if not e.get("label")]
    labelled = [e for e in items if e.get("label")]
    # No early return when there are no anonymous tiles: the caption-dedup
    # below still has work to do once relations have named them.

    # Captions only. A tile may contain other real widgets; those are not
    # swallowed just because they sit inside it.
    CAPTION = ("label", "text", "static", "heading")

    consumed = set()
    for tile in tiles:
        inside = [i for i, lab in enumerate(labelled)
                  if i not in consumed and lab["type"] in CAPTION
                  and _contains(tile, lab)]
        if not inside:
            continue
        # Name from the tightest caption; consume ALL of them. GNOME publishes
        # the same app name twice at one spot — a `label` and a `text` of
        # different widths — so adopting only the smallest left the other
        # behind as a second box on the same icon.
        best = min(inside, key=lambda i: labelled[i]["width"] * labelled[i]["height"])
        tile["label"] = labelled[best]["label"]
        consumed.update(inside)

    drop = {id(labelled[i]) for i in consumed}

    # A caption repeating the label of a tile that contains it is that tile,
    # not a second target. This runs whether or not the tile was anonymous:
    # once relations name a tile ("Clocks" via described-by), it is no longer
    # a tile above and its caption would otherwise survive as a second — and
    # third, since GNOME publishes both a `label` and a `text` — box on one
    # icon.
    CAPTION = ("label", "text", "static", "heading")
    for e in items:
        if id(e) in drop or e["type"] not in CAPTION or not e.get("label"):
            continue
        for other in items:
            if other is e or other.get("label") != e["label"]:
                continue
            if other["type"] not in CAPTION and _contains(other, e):
                drop.add(id(e))
                break

    return [e for e in items
            if e.get("label") and id(e) not in drop]


def _apply_stack_occlusion(results, ctx, screen):
    """Recompute visibility against the REAL cross-application window stack.

    The Linux twin of the macOS scanner's _apply_window_occlusion. Everything
    painted above the scanned window — another app's window, and the dock or
    desktop showing through beside it — is a genuine occluder, but until now
    only same-app siblings were considered, so elements buried under a
    maximized window were still reported clickable.

    Only the window actually scanned is exempt: its own elements are already
    clipped and occluded by _apply_sibling_occlusion, and it is by definition
    not behind itself. Elements are DOWNGRADED to "partial N%" and only dropped
    when essentially nothing is left, because the occluders here are actor
    geometry rather than per-element truth."""
    actors = live_actors(ctx.get("actors") or [])
    window = ctx.get("window")
    win_frame = acc_extents(window, ctx.get("offset", (0, 0))) \
        if window is not None else None
    if win_frame is None:
        # The Desktop case: nothing is on top, so nothing occludes anything.
        for e in results:
            e.pop("_bg", None)
        return results

    # Everything painted above the scanned window. Its own actor comes from
    # resolve_window_offset — NOT from re-matching win_frame, which silently
    # fails for every "margin" match (the frame is inset inside its actor by
    # the CSD shadow) and then treats every actor, including the ones BELOW,
    # as an occluder. Measured before this was threaded through: all 29
    # gnome-control-center elements deleted by the window behind it.
    above = []
    mine = -1
    my_actor = ctx.get("actor")
    if actors:
        if my_actor is not None:
            # Identity first. Two clones can share a rect to the pixel (two
            # maximized windows), and a rect match then finds whichever comes
            # first in paint order — measured: the LOWER twin, so the scanned
            # window's own upper twin counted as "above" it and every one of
            # its elements was deleted. Only fall back to geometry when the
            # dict is not from this list, and then take the topmost match.
            for i, a in enumerate(actors):
                if a is my_actor:
                    mine = i
                    break
            if mine < 0:
                for i in range(len(actors) - 1, -1, -1):
                    if _rects_match(actors[i], my_actor, tol=2):
                        mine = i
                        break
        if mine < 0:
            # No actor was matched at all ("topmost"/"none" offsets). Z-order
            # is unknown, so claiming to know what is above it would delete
            # real elements — occlude nothing.
            for e in results:
                e.pop("_bg", None)
            return results
        biggest = max(a["width"] * a["height"] for a in actors)
        above = [a for i, a in enumerate(actors)
                 if i > mine and a["width"] * a["height"] < biggest]

    out = []
    for e in results:
        rect = {"x": e["x"], "y": e["y"],
                "width": e["width"], "height": e["height"]}
        # Background chrome — the dash/dock and desktop icons — is also behind
        # the window being scanned, so that window occludes it too. GNOME
        # auto-hides the dash under a maximized window while gnome-shell's
        # AT-SPI tree still reports its buttons as SHOWING, which is how
        # "Trash" and "App Center" ended up in a maximized Chrome's tree.
        occ = above + [win_frame] if e.pop("_bg", False) else above
        hits = [a for a in occ if _rect_intersect(rect, a)]
        if not hits:
            out.append(e)
            continue
        frac = _visible_fraction_after_occluders(rect, hits)
        if frac < 0.02:
            continue                      # wholly buried — not clickable
        if frac < 0.99 and e.get("visibility") == "full":
            e["visibility"] = f"partial {int(frac * 100)}%"
        out.append(e)
    return out


# When one widget is published under two roles at the same rect, this is the
# order we keep — most actionable first. A thing you can click beats a thing
# that merely describes it.
_ROLE_PRECEDENCE = [
    "entry", "password text", "combo box", "spin button", "slider",
    "toggle button", "check box", "radio button", "push button", "button",
    "menu item", "check menu item", "radio menu item", "page tab", "link",
    "list item", "tree item", "table cell", "menu",
    "icon", "image", "heading", "label", "static", "filler", "text",
]
_ROLE_RANK = {role: i for i, role in enumerate(_ROLE_PRECEDENCE)}


def _collapse_coincident(results, tol=2):
    """Collapse elements that are the SAME widget published twice.

    GTK4 routinely exposes one control under two roles at one rect — measured
    in Nautilus: 'Main Menu' as both a `button` and a `toggle button` at
    421,236 34x34, likewise 'View Options' and 'Current Folder Menu'. Both
    reach the tree, so the annotated screenshot draws two numbered boxes on
    one widget and the agent is offered the same click twice.

    Only an exact coincidence is collapsed — same label AND the same rect to
    within `tol` px. A child that merely sits inside its parent is left alone:
    that is real hierarchy, and _build_hierarchical_tree nests it."""
    kept = []
    for e in results:
        label = e.get("label", "")
        for i, k in enumerate(kept):
            if k.get("label", "") != label:
                continue
            if (abs(k["x"] - e["x"]) <= tol and abs(k["y"] - e["y"]) <= tol
                    and abs(k["width"] - e["width"]) <= tol
                    and abs(k["height"] - e["height"]) <= tol):
                # Same widget. Keep whichever role is more actionable.
                if _ROLE_RANK.get(e["type"], 99) < _ROLE_RANK.get(k["type"], 99):
                    kept[i] = e
                break
        else:
            kept.append(e)
    return kept


# The roles _collapse_duplicates reasons about. A caption is text that names
# what it sits in; a graphic is the picture inside a control; a wrapper is a
# collection entry that exists to hold a control; a control is what gets
# clicked.
_CAPTION_ROLES = frozenset({"label", "static", "heading"})
_GRAPHIC_ROLES = frozenset({"icon", "image"})
_WRAPPER_ROLES = frozenset({"list item", "table cell", "tree item"})
_CONTROL_ROLES = frozenset({
    "link", "push button", "button", "toggle button", "check box",
    "radio button", "entry", "password text", "combo box", "spin button",
    "menu item", "check menu item", "radio menu item", "page tab",
})


def _collapse_duplicates(results):
    """Drop the elements that repeat, inside or around another element, that
    element's label. A widget is routinely published as two or three nested
    nodes with one name — measured on a GitLab page in Chrome: every file
    row was `table cell` > `link` > `static`, all "README.md", three
    numbered boxes for one click; a GTK button is `push button` > `label`,
    both "Cancel"; a desktop tile is `filler` > `icon` + `label`, all
    "Trash". The agent is offered each target once:

      - a CAPTION inside anything with its label is that thing's own text:
        dropped (the overview's caption rule, applied everywhere);
      - a GRAPHIC inside a non-graphic with its label is its picture: dropped;
      - a WRAPPER around a CONTROL with its label adds nothing the control
        does not (its centre need not even be on the control): dropped.

    Only elements from one source are compared — a dash icon and a link in
    the window in front never name each other. Same-rect twins never reach
    here (_collapse_coincident keeps one), so every pair below is a real
    nesting and the outer is strictly larger."""
    groups = {}
    for i, e in enumerate(results):
        key = (e.get("label") or "").strip().lower()
        if key:
            groups.setdefault((key, e.get("source") or ""), []).append(i)
    drop = set()
    for idxs in groups.values():
        if len(idxs) < 2:
            continue
        for i in idxs:
            outer = results[i]
            for j in idxs:
                if i == j:
                    continue
                inner = results[j]
                if not _contains(outer, inner) or _contains(inner, outer):
                    continue
                ot, it = outer["type"], inner["type"]
                if it in _CAPTION_ROLES:
                    drop.add(j)
                elif it in _GRAPHIC_ROLES and ot not in _GRAPHIC_ROLES:
                    drop.add(j)
                elif ot in _WRAPPER_ROLES and it in _CONTROL_ROLES:
                    drop.add(i)
    if not drop:
        return results
    return [e for k, e in enumerate(results) if k not in drop]


def collect_elements(screen, ctx):
    """Phase 2: read-only element gathering from every source.
    Returns (app_info, menu_items, elements)."""
    results = []
    app_info = None

    app, window = ctx["app"], ctx["window"]
    offset = ctx["offset"]
    fast = _get_fast()

    # Without the fast path, the shell walk is the longest single pole in a
    # scan and it talks to gnome-shell, a DIFFERENT process from the client
    # window — so unlike two walks of one app's tree, where the target's
    # accessibility server serialises every request and a thread buys
    # nothing, these genuinely overlap. Each AT-SPI call releases the GIL
    # while it waits on D-Bus. Measured 5.55s sequential vs 3.49s
    # overlapped, with byte-identical output across repeated runs.
    #
    # WITH the fast path both finish in tens of milliseconds, and its D-Bus
    # waves ride thread-default main contexts — so everything runs on this
    # thread, sequentially, where no two contexts can fight.
    shell_thread = None
    shell_out = {}
    if fast is None:
        def _shell_worker():
            try:
                shell_out["result"] = scan_shell_chrome(
                    screen, ctx.get("overview", False))
            except Exception:
                shell_out["result"] = ([], [])

        shell_thread = threading.Thread(target=_shell_worker)
        shell_thread.start()

    if window is not None:
        name = _safe_name(app) or _safe_name(window) or "Desktop"
        win_frame = acc_extents(window, offset) or dict(screen)
        app_info = {"name": name, "frame": win_frame}

        win_frames = None
        if ctx.get("no_walk"):
            # The frame is all this app may be asked for (see prepare_scan):
            # the window box only, and no descent into its children.
            results.append(_window_element(
                _safe_name(window) or name, _window_role(window),
                win_frame, window, 0))
            win_frames = [win_frame]
        elif fast is not None:
            mark = len(results)
            try:
                win_frames = _collect_app_windows_fast(
                    fast, window, ctx, screen, results)
            except Exception as e:
                del results[mark:]   # scrub a half-finished fast attempt
                print(f"  Fast window walk unavailable ({e}) — using the "
                      f"slow walk.")
        if win_frames is None:
            win_frames = _collect_app_windows_sync(
                app, window, ctx, screen, results)

        # What the user sees of the active window, not its frame: OCR is
        # confined to it, so text under the shadow margin stays out.
        app_info["frame"] = win_frames[0]
        results = _apply_sibling_occlusion(results, win_frames)

    # GNOME Shell chrome: the top bar and the dash/dock.
    if shell_thread is not None:
        shell_thread.join()
        menu_items, shell_elements = shell_out.get("result", ([], []))
    else:
        try:
            menu_items, shell_elements = scan_shell_chrome(
                screen, ctx.get("overview", False))
        except Exception:
            menu_items, shell_elements = [], []
    # In the overview, the shell walk returns two different things mixed
    # together, and the SHOWING bit is exactly what separates them: the dash
    # and top bar are genuinely displayed and carry it, while the app grid
    # does not (that is why it has to be asked for — see
    # shell_overview_active). So SHOWING means "real chrome, task bar";
    # its absence means "overview content, the surface in front".
    if ctx.get("overview"):
        for e in shell_elements:
            e.pop("_showing", None)
        for e in menu_items:
            e.pop("_showing", None)
        # Tracking the "text" role (for the search entry) makes the top bar
        # publish the clock twice — a `label` and the `text` inside it, same
        # string. Drop the inner copy.
        menu_items = [m for m in menu_items
                      if not any(o is not m and o["label"] == m["label"]
                                 and _contains(o, m) for o in menu_items)]
        # Split the dash from the overview content by GEOMETRY. An earlier
        # version split on the SHOWING bit, which looked right against an
        # overview opened over D-Bus and is wrong against a real one: when the
        # user opens the grid, its icons DO report SHOWING, so everything read
        # as dash and <top_layer> came out empty.
        #
        # The dash is the strip flush against the left screen edge; the grid,
        # the thumbnails and the search entry are all clear of it.
        dash_right = max([e["x"] + e["width"] for e in shell_elements
                          if e["x"] <= 2] or [0])
        live = [e for e in shell_elements if e["x"] + e["width"] <= dash_right]
        grid = [e for e in shell_elements if e["x"] + e["width"] > dash_right]
        # A non-SHOWING label that repeats a dash icon's name AT THAT ICON'S
        # HEIGHT is the icon's hover tooltip, not an app-grid entry. Name
        # alone is not enough — "Settings" and "Cursor" are legitimately in
        # both the dash and the grid — and neither is overlap, because the
        # tooltip is drawn beside the icon, not over it. Name plus vertical
        # band identifies it: the grid rows sit at completely different y.
        def _is_dash_tooltip(g):
            for l in live:
                if l["label"] != g["label"]:
                    continue
                if (g["y"] < l["y"] + l["height"]
                        and l["y"] < g["y"] + g["height"]):
                    return True
            return False

        grid = [g for g in grid if not _is_dash_tooltip(g)]
        # Fold each app-grid label into the tile that contains it, so the
        # click target is the 113x113 icon and not its 89x19 caption.
        grid = _adopt_overview_labels(grid)
        live = _adopt_overview_labels(live)
        for e in grid:
            e["source"] = "overview"
        shell_elements = live + grid
    else:
        for e in shell_elements:
            e.pop("_showing", None)

    # The dash/dock is background chrome: it sits behind the scanned window
    # and is auto-hidden when that window covers it. Tagged so the occlusion
    # pass below can drop it; the top bar (menu_items) is not tagged, because
    # GNOME keeps it above every window, exactly like the macOS menu bar.
    for e in shell_elements:
        if e.get("source") != "overview":
            e["_bg"] = True
    results.extend(shell_elements)

    # Desktop icons belong to the DESKTOP, not to the application in front.
    # They are scanned only when the desktop is the front surface — i.e. no
    # application window is on screen at all (`desktop_only`, decided in
    # prepare_scan). An app being in front means the agent is working in that
    # app; wallpaper icons beside it are not part of its UI and must not be
    # offered as targets.
    #
    # An occlusion-based version of this was tried (scan always, let the
    # window trim what it covers, macOS-style). It is rejected: a window that
    # does not span the screen leaves the icons "visible", so they came back
    # into an app scan. Visibility is not the test — ownership is.
    # They are still tagged `_bg` so the occlusion pass trims any that a
    # window does cover in the desktop case itself.
    #
    # In the sync world this must come AFTER the shell join, not overlapped:
    # it enumerates the AT-SPI desktop root, which is what the shell walk is
    # already traversing, and the two serialise on it badly — measured alone
    # 0.2s; alongside the shell walk it took 8.6s and dragged the shell walk
    # from 3.3s to 10.8s. The fast desktop scan reads bulk caches instead and
    # takes ~30ms wherever it runs.
    if ctx.get("desktop_only"):
        mark = len(results)
        scan_desktop_icons(screen, results, ctx["actors"])
        for e in results[mark:]:
            e["_bg"] = True

    # Cross-application occlusion — the macOS scanner's _apply_window_occlusion,
    # which Linux never had. Everything above only ever occluded windows of ONE
    # app, so a maximized window left the dock and every element beneath it
    # reported as fully visible: measured 215 of 216 elements "full" under a
    # dialog covering half the screen.
    results = _apply_stack_occlusion(results, ctx, screen)

    # Deduplicate exact repeats.
    seen = set()
    unique = []
    for e in results:
        key = (e["label"], e["type"], round(e["x"]), round(e["y"]))
        if key not in seen:
            seen.add(key)
            unique.append(e)

    unique = _collapse_coincident(unique)
    unique = _collapse_duplicates(unique)
    return app_info, menu_items, unique


def extract_all(screen):
    """Gather elements from all visible sources (standalone helper)."""
    return collect_elements(screen, prepare_scan(screen))


# ========== SCREENSHOT ==========

def _screenshot_portal():
    """Wayland-safe capture via the org.freedesktop.portal.Screenshot D-Bus
    API. Returns a PIL image, or None."""
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    token = "elemscan" + secrets.token_hex(4)
    sender = bus.get_unique_name()[1:].replace(".", "_")
    req_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
    result = {}

    # A private main context, pushed thread-default for the duration — the
    # way _FastA11y.wave isolates its replies. The Response signal is
    # dispatched to whatever context was thread-default when it was
    # subscribed, and this capture now runs on a worker thread beside the
    # walk's own private contexts: nothing here touches the global default
    # context, so no other loop can wake or quit this one, or vice versa.
    ctx = GLib.MainContext.new()
    ctx.push_thread_default()
    loop = GLib.MainLoop.new(ctx, False)

    def on_response(conn, sender_name, path, iface, signal, params):
        code, data = params.unpack()
        result["code"] = code
        result["uri"] = data.get("uri")
        loop.quit()

    try:
        sub = bus.signal_subscribe(
            "org.freedesktop.portal.Desktop",
            "org.freedesktop.portal.Request", "Response", req_path,
            None, Gio.DBusSignalFlags.NONE, on_response)
        try:
            bus.call_sync(
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                "org.freedesktop.portal.Screenshot", "Screenshot",
                GLib.Variant("(sa{sv})", ("", {
                    "handle_token": GLib.Variant("s", token),
                    "interactive": GLib.Variant("b", False),
                })),
                GLib.VariantType("(o)"), Gio.DBusCallFlags.NONE, -1, None)
            # The deadline source lives on this context only, so a stale
            # timeout can never fire into a later scan's loop; it is still
            # removed when it did not fire, to leave the context clean.
            fired = []

            def _deadline(*_args):
                fired.append(1)
                loop.quit()
                return False

            src = GLib.timeout_source_new_seconds(15)
            src.set_callback(_deadline)
            src.attach(ctx)
            loop.run()
            if not fired:
                src.destroy()
        finally:
            bus.signal_unsubscribe(sub)
    finally:
        ctx.pop_thread_default()

    if result.get("code") != 0 or not result.get("uri"):
        return None
    path = unquote(urlparse(result["uri"]).path)
    try:
        img = Image.open(path)
        img.load()
        return img
    finally:
        # The portal writes into ~/Pictures — don't litter it.
        try:
            os.remove(path)
        except OSError:
            pass


def take_screenshot(screen):
    """Capture the screen. Returns (PIL Image, scale) — scale maps logical
    (AT-SPI) coords to captured pixels (HiDPI factor)."""
    img = None
    try:
        img = _screenshot_portal()
    except Exception as e:
        print(f"  Portal screenshot failed: {e}")
    if img is None:
        try:
            from PIL import ImageGrab   # X11 fallback
            img = ImageGrab.grab()
        except Exception as e:
            print(f"Screenshot error: {e}")
            return None, 1.0
    scale = img.width / screen["width"] if screen["width"] else 1.0
    return img, scale


# ========== XML ESCAPE ==========

def _xml_escape(text):
    if not text:
        return ""
    return (text
            .replace('&', '&amp;')
            .replace('<', '&lt;')
            .replace('>', '&gt;')
            .replace('"', '&quot;'))


# ========== ANNOTATION FONT (cached) ==========

_ANNOTATE_FONT = None  # cached (font, stroke_width)
_label_tiles = {}      # label -> pre-rendered RGBA tile


def _build_annotate_font():
    font = None
    for font_path in (
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"):
        try:
            font = ImageFont.truetype(font_path, LABEL_FONT_SIZE)
            break
        except Exception:
            continue
    # PIL strokes render through FreeType, which load_default() lacks.
    stroke_w = LABEL_STROKE if font is not None else 0
    if font is None:
        font = ImageFont.load_default()
    return font, stroke_w


def _load_annotate_font():
    global _ANNOTATE_FONT
    if _ANNOTATE_FONT is None:
        _ANNOTATE_FONT = _build_annotate_font()
    return _ANNOTATE_FONT


def _label_tile(label, font, stroke_w):
    """Cached pre-rendered RGBA tile for an index label — glyph rasterisation
    is the most expensive part of annotation and the label set repeats."""
    tile = _label_tiles.get(label)
    if tile is None:
        pad = 2 * stroke_w + 4
        tile = Image.new("RGBA",
                         (len(label) * LABEL_FONT_SIZE + pad,
                          2 * LABEL_FONT_SIZE + pad),
                         (0, 0, 0, 0))
        ImageDraw.Draw(tile).text(
            (0, 0), label, fill=NUMBER_COLOR, font=font,
            stroke_width=stroke_w, stroke_fill=LABEL_STROKE_COLOR)
        # Crop to the ink. The canvas above is allocated generously (twice
        # the font size), and anything that positions a label by the tile's
        # size — an OCR label sits ABOVE its box — was off by the slack:
        # measured 35 px up, so every OCR number landed on the line above and
        # read as that line's index.
        ink = tile.getbbox()
        if ink:
            tile = tile.crop(ink)
        _label_tiles[label] = tile
    return tile


# ========== SCANNER CLASS ==========

class UIElementScanner:
    """The scanner the AutoCua agent drives on Linux.

    Implements the interface the agent and controller expect from every
    platform's scanner, so neither needs to know which one it is holding."""

    def __init__(self, config, frontend_callback=None):
        self.config = config
        self.frontend_callback = frontend_callback

        # State populated by scan_elements()
        self.element_tree = []          # Hierarchical tree structure (top layer)
        self._overview = False          # GNOME Shell overview / app grid up
        self.task_bar_tree = []         # Dash/dock items (GNOME's task bar)
        self.desktop_tree = []          # Desktop icons, when not covered
        self.menu_bar_tree = []         # Top-bar items as tree nodes
        self.element_index = 0          # Global index counter
        self.application_name = "Desktop"
        self.elements_to_draw = []      # List for screenshot bounding boxes
        self.elements_mapping = {}      # Mapping of index → element info for controller
        self.app_rect = None            # Application window rectangle
        self.top_layer_info = None      # {"name": ..., "type": "app"}
        self.second_layer_info = None   # Not used on Linux (no overlay layers)
        self.second_layer_tree = []     # Empty on Linux
        self.found_elements = {}        # Dictionary to store elements by type
        self._debug_iteration = 0       # Debug iteration counter

        self._screenshot = None         # Captured PIL screenshot (pixels)
        self._scale = 1.0               # logical-coords → capture-pixels factor
        self._annotated_image_base64 = None
        self._plain_screenshot = None
        self._annotate_prep = None      # (shrunk RGB image, shrink factor)
        self._annotate_thread = None
        # OCR state — the scanner runs in its own thread beside the walk
        self.ocr_scanner = None
        self.ocr_thread = None
        self.ocr_stats = None
        self._screen_size = (0, 0)
        self._ocr_backdrop_max_area = float("inf")

    def scan_elements(self):
        """Scan the active window, shell chrome and desktop for configured
        element types, then annotate the screenshot."""
        # Clear previous scan state
        self.element_tree = []
        self.task_bar_tree = []
        self.desktop_tree = []
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
        self._annotated_image_base64 = None
        self._plain_screenshot = None
        self._annotate_prep = None
        self._annotate_thread = None
        # OCR state — the scanner runs in its own thread beside the walk
        self.ocr_scanner = None
        self.ocr_thread = None
        self.ocr_stats = None
        self._screen_size = (0, 0)
        self._ocr_backdrop_max_area = float("inf")

        screen = get_screen()
        self._screen_size = (screen["width"], screen["height"])
        _seen_roles.clear()
        t_start = time.time()

        # One reconnect attempt per scan, so a transient a11y-bus hiccup
        # does not pin the whole process to the slow walk forever.
        _get_fast(retry=True)
        _SHELL_TREE.clear()

        # Capture FIRST, on a worker. The portal round-trip is ~0.6 s of
        # pure waiting, and nothing in prepare_scan changes what is on
        # screen — the a11y flag is a bus property, the nudge and the
        # readiness poll only touch the tree — so the frame lands while
        # the z-order and readiness work runs. The worker chains straight
        # into OCR detection and the annotation downscale, so both start
        # the moment pixels exist rather than after prepare_scan. One frame
        # feeds everything, so OCR boxes, element boxes and the displayed
        # screenshot agree.
        #
        # The one exception is a browser with a page on its way: there
        # prepare_scan holds the scan until the page has loaded, and the
        # screen changes under this frame. _recapture handles that below.
        self._capture_thread = None
        if SCREENSHOT or OCR:
            self._capture_thread = threading.Thread(
                target=self._capture_worker, args=(screen,))
            self._capture_thread.start()

        # Phase 1: anything that changes tree contents.
        ctx = prepare_scan(screen)
        t_prepare = time.time()
        if ctx.get("browser_waited") and self._capture_thread is not None:
            self._recapture(screen, ctx["browser_waited"])
        t_capture = time.time()

        walk_ok = False
        try:
            self._overview = ctx.get("overview", False)
            app_info, menu_items, elements = collect_elements(screen, ctx)
            elements.sort(key=lambda e: (e["y"], e["x"]))

            if app_info:
                self.application_name = app_info["name"]
                self.top_layer_info = {"name": app_info["name"], "type": "app"}
                f = app_info.get("frame")
                if f:
                    self.app_rect = Rect(
                        int(f["x"]), int(f["y"]),
                        int(f["x"] + f["width"]), int(f["y"] + f["height"]))
            elif self._overview:
                # Fullscreen shell UI, not the desktop and not an app. Naming
                # it "Desktop" pointed the agent at a surface it could not see.
                self.application_name = "GNOME Shell Overview"
                self.top_layer_info = {"name": "GNOME Shell Overview",
                                       "type": "app"}
            else:
                self.top_layer_info = {"name": "Desktop", "type": "app"}

            # ----- Build menu bar tree nodes -----
            for m in menu_items:
                self.element_index += 1
                ctype = clean_type(m["type"])
                rect = Rect(int(m["x"]), int(m["y"]),
                            int(m["x"] + m["width"]), int(m["y"] + m["height"]))
                node = {
                    "element": None,
                    "name": m["label"],
                    "aria_role": "",
                    "type": ctype,
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
                    'type': ctype,
                    'value': None,
                    'visibility': 'full',
                    'clipped_by': None,
                    'acc_element': m.get("acc_element"),
                }
                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": rect,
                        "index": self.element_index,
                        "depth": 0,
                        "visibility": "full",
                        "source": "",
                    })

            # ----- Build element trees using spatial containment -----
            # Shell chrome is NOT part of the application in front. The dash
            # (Firefox, App Center, Trash...) is GNOME's task bar and the
            # wallpaper icons are the desktop; merging either into <top_layer>
            # told the agent they were part of the app it was looking at.
            # collect_elements already tags them, so this is a partition, not
            # a second scan — and they stay in one list until here so dedup
            # and occlusion see the whole screen.
            front = [e for e in elements
                     if e.get("source") in ("", "overview", None)]
            task_bar = [e for e in elements if e.get("source") == "shell"]
            desktop = [e for e in elements if e.get("source") == "desktop"]
            # Built in the order get_scan_data emits them, because
            # _build_hierarchical_tree stamps self.element_index as it goes —
            # build out of order and the indices run 1,2 then 194.. then 3..,
            # which is unreadable next to the annotated screenshot. Keep this
            # order matching all_trees / get_scan_data.
            self.task_bar_tree = self._build_hierarchical_tree(task_bar)
            self.desktop_tree = self._build_hierarchical_tree(desktop)
            self.element_tree = self._build_hierarchical_tree(front)
            walk_ok = True
        finally:
            # The capture worker owns the frame, the annotation prep and
            # the start of the OCR reader; join it here (also on failure,
            # so no thread ever outlives its screenshot). The reader itself
            # is joined by _filter_and_merge_ocr, which first tells it what
            # to skip — unless the walk failed, when nothing will filter
            # the boxes and the reader is told to stop instead.
            if self._capture_thread is not None:
                self._capture_thread.join()
                self._capture_thread = None
            if not walk_ok:
                if self.ocr_scanner is not None:
                    self.ocr_scanner.set_keep(lambda _line: False)
                self._join_ocr()
        t_collect = time.time()

        # ----- Fuse OCR where the tree had nothing -----
        # Detection already ran beside the walk; this filters its boxes
        # against every element rect, reads the survivors and nests them as
        # OCR_TEXT nodes — the same merge the macOS and Windows scanners do.
        self._filter_and_merge_ocr()
        t_ocr = time.time()

        # ----- Annotated screenshot -----
        self._debug_iteration += 1
        if SCREENSHOT and self.elements_to_draw:
            self._capture_and_annotate(screen)
        t_end = time.time()

        if DEBUG:
            self.save_to_file()
            print(f"  [scan timing] prepare={t_prepare - t_start:.2f}s "
                  f"capture={t_capture - t_prepare:.2f}s "
                  f"collect+build={t_collect - t_capture:.2f}s "
                  f"ocr={t_ocr - t_collect:.2f}s "
                  f"annotate={t_end - t_ocr:.2f}s "
                  f"total={time.time() - t_start:.2f}s")

    def _build_hierarchical_tree(self, flat_elements):
        """Convert flat element list into a hierarchical tree using spatial
        containment, assign indices, and populate elements_mapping."""
        if not flat_elements:
            return []

        # Sort by area descending — larger containers first
        by_area = sorted(flat_elements,
                         key=lambda e: e["width"] * e["height"], reverse=True)

        # Nearest containing parent: scan backwards, stop at first hit (the
        # list is area-descending, so the first backward hit is the nearest
        # containing ancestor).
        # A cut-off element nests by what is left of it: a link the
        # window's bottom edge cuts is still the window's, though its full
        # rect pokes out of the box.
        n = len(by_area)
        inner = [e.get("visible_rect_raw") or e for e in by_area]
        parent = [None] * n
        depth = [0] * n
        for i in range(n):
            e = inner[i]
            for j in range(i - 1, -1, -1):
                if _contains(by_area[j], e):
                    parent[i] = j
                    depth[i] = depth[j] + 1
                    break

        nodes = []
        for i, e in enumerate(by_area):
            rect = Rect(int(e["x"]), int(e["y"]),
                        int(e["x"] + e["width"]), int(e["y"] + e["height"]))
            vr = e.get("visible_rect_raw")
            visible_rect = Rect(
                int(vr["x"]), int(vr["y"]),
                int(vr["x"] + vr["width"]), int(vr["y"] + vr["height"])
            ) if vr else rect

            nodes.append({
                "element": None,
                "name": e["label"],
                "aria_role": "",
                "type": clean_type(e["type"]),
                "active": True,
                "index": None,
                "value": e.get("value"),
                "actions": None,
                "visibility": e.get("visibility", "full"),
                "clipped_by": None,
                "rect": rect,
                "visible_rect": visible_rect,
                "children": [],
                "browser_top_layer": None,
                "browser_second_layer": None,
                "source": e.get("source", ""),
                "_parent_idx": parent[i],
                "_depth": depth[i],
                "_raw_element": e,
            })

        for i, node in enumerate(nodes):
            p = node["_parent_idx"]
            if p is not None:
                nodes[p]["children"].append(node)
        roots = [nd for nd in nodes if nd["_parent_idx"] is None]

        # Sort children by position (top-to-bottom, left-to-right)
        def _sort_children(node_list):
            node_list.sort(key=lambda nd: (nd["rect"].top, nd["rect"].left))
            for nd in node_list:
                if nd["children"]:
                    _sort_children(nd["children"])

        _sort_children(roots)

        # Assign sequential indices depth-first
        def _assign_indices(node_list):
            for nd in node_list:
                self.element_index += 1
                nd["index"] = self.element_index
                e = nd.pop("_raw_element")

                self.elements_mapping[str(self.element_index)] = {
                    'element': None,
                    'rect': nd["rect"],
                    'visible_rect': nd["visible_rect"],
                    'name': nd["name"],
                    'aria_role': '',
                    'type': nd["type"],
                    # Deliberate divergence: the other platforms' scanners
                    # hardcode None here and surface the value only in the
                    # tree text. The controller can use it directly.
                    'value': nd["value"],
                    'visibility': nd["visibility"],
                    'clipped_by': None,
                    'acc_element': e.get("acc_element"),
                }
                if SCREENSHOT:
                    self.elements_to_draw.append({
                        "rect": nd["rect"],
                        "index": self.element_index,
                        "depth": nd.get("_depth", 0),
                        "visibility": nd["visibility"],
                        "source": nd.get("source", ""),
                    })
                self.found_elements.setdefault(nd["type"], []).append(nd)
                if nd["children"]:
                    _assign_indices(nd["children"])

        _assign_indices(roots)

        def _cleanup(node_list):
            for nd in node_list:
                nd.pop("_parent_idx", None)
                nd.pop("_depth", None)
                nd.pop("_raw_element", None)
                if nd["children"]:
                    _cleanup(nd["children"])

        _cleanup(roots)
        return roots

    # ========== OCR MERGE (the macOS / Windows scanners' merge, on AT-SPI) ==========

    def _start_ocr(self, screen):
        """Kick off text detection on the captured frame, in a thread. Says
        once per process why it cannot, then stays quiet."""
        global _OCR_WARNED
        reason = ocr_unavailable_reason()
        if reason:
            if not _OCR_WARNED:
                print(f"  OCR off: {reason}")
                _OCR_WARNED = True
            return
        self.ocr_scanner = OCRScanner(
            self._screenshot, self._scale, (screen["x"], screen["y"]),
            read=OCR_RECOGNIZE)
        self.ocr_thread = threading.Thread(target=self.ocr_scanner.scan)
        self.ocr_thread.start()

    def _capture_worker(self, screen):
        """Worker half of the capture: take the frame, hand it to the OCR
        reader (its own thread), then do the annotation downscale here.
        Runs beside prepare_scan and the walk, which need nothing from it
        until _filter_and_merge_ocr."""
        try:
            self._screenshot, self._scale = take_screenshot(screen)
        except Exception as e:
            print(f"Screenshot error: {e}")
            self._screenshot, self._scale = None, 1.0
        if self._screenshot is None:
            return
        if OCR:
            self._start_ocr(screen)
        if SCREENSHOT:
            self._prepare_annotate_base()

    def _join_ocr(self):
        if self.ocr_thread is not None:
            self.ocr_thread.join()
            self.ocr_thread = None

    def _recapture(self, screen, waited):
        """The early frame is stale: take it again, now that the page is up.

        The capture worker starts at t=0 on the promise that nothing in
        prepare_scan changes the screen. A browser navigation breaks that
        promise: wait_for_browser_load holds the scan until the page has
        loaded, and the pixels the worker caught before that show the page on
        its way out — Chrome's "Loading…" tab over a blank viewport — while
        the tree walked afterwards describes the page that arrived. Measured
        on Chrome: a tree full of the new page's links drawn as boxes over
        empty grey. One frame must feed everything, so the stale frame and
        whatever was started on it (OCR detection, the annotation downscale)
        are dropped and the worker runs again, here, in line. The extra
        portal round-trip is paid only on a scan that actually waited."""
        self._capture_thread.join()
        self._capture_thread = None
        if self.ocr_scanner is not None:
            self.ocr_scanner.set_keep(lambda _line: False)   # read nothing
        self._join_ocr()
        self.ocr_scanner = None
        self.ocr_stats = None
        self._screenshot = None
        self._annotate_prep = None
        print(f"  Browser: the frame captured before the {waited:.1f}s wait "
              f"is stale — capturing again.")
        self._capture_worker(screen)

    def _collect_leaf_rects(self, tree_list, rects):
        """Rects that actually claim screen space, for OCR suppression.
        Structural wrappers never claim space — the gaps between their
        children are exactly where OCR fills in — so they are recursed
        into without adding their own rect. Every other element claims its
        rect, EXCEPT backdrop-sized leaves (a canvas, a full-window image, a
        document body) which must not hide the text drawn on top of them."""
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
        """The children list of the deepest element whose rect contains
        (cx, cy), so an OCR node nests where it visually belongs. Centre-point
        containment, forgiving of a box that pokes a pixel outside its parent.
        Returns tree_list itself when nothing claims the point."""
        for item in tree_list:
            rect = item.get("rect") or item.get("visible_rect")
            if not rect:
                continue
            if rect.left <= cx <= rect.right and rect.top <= cy <= rect.bottom:
                if item.get("children"):
                    deeper = self._find_deepest_container(item["children"], cx, cy)
                    if deeper is not item["children"]:
                        return deeper
                    return item["children"]
                return item.setdefault("children", [])
        return tree_list

    def _filter_and_merge_ocr(self):
        """Keep the OCR boxes the tree does not cover, read them, nest them.

        Only leaf (space-claiming) element rects suppress OCR; structural
        wrappers do not. A box whose centre sits inside a leaf rect is text
        the tree already names and is dropped. The survivors — canvas text,
        label-less controls, a renderer that never published — are the ONLY
        boxes the recognizer reads, then they are nested into the deepest
        matching container as OCR_TEXT nodes and registered for clicking
        (acc_element=None: coordinate click) and drawing."""
        scanner = self.ocr_scanner
        if scanner is None:
            return
        # The frame arrived ~0.6 s into the scan and the detector took it
        # from there, so the walk is often done before the boxes are.
        scanner.det_done.wait()
        lines = scanner.get_lines()
        stats = dict(scanner.stats)
        stats.update(outside=0, merged=0)
        self.ocr_stats = stats
        if not lines:
            self._join_ocr()
            return

        # Backdrop guard: a leaf covering a large fraction of the screen is a
        # canvas or background, not the label of any word on it.
        sw, sh = self._screen_size
        self._ocr_backdrop_max_area = (0.25 * sw * sh) if (sw and sh) \
            else float("inf")

        leaf_rects = []
        for tree in self.all_trees:
            self._collect_leaf_rects(tree, leaf_rects)
        leaf_labels = [(n["rect"], _norm_label(n["name"]))
                       for tree in self.all_trees for n in _iter_tree(tree)
                       if n.get("name") and n.get("rect")]

        # Two geometric tests, either one drops the box. Centre-in-leaf is
        # the macOS test; the coverage test catches what it misses: a line
        # that runs across several small elements (a row of menu items, a
        # toolbar) has its centre in a gap between them and would have been
        # merged as a duplicate of their labels. Coverage is the fraction of
        # the box's area lying under leaf rects, summed per rect — leaves
        # rarely overlap each other, so the sum is close to the union.
        # Only the window in front is the agent's surface. Text visible
        # beside or behind it — another app's edge, the desktop's icon
        # captions, the top bar — is not part of it and is not offered.
        # Measured before this: an App Center scan carried 40 lines of the
        # Cursor window standing behind it and every wallpaper caption. The
        # Desktop and overview scans have no frame and keep everything.
        frame = self.app_rect
        kept = []
        seen = set()
        stats.update(dropped_outside=0, dropped_centre=0,
                     dropped_covered=0, dropped_label=0)
        for line in lines:
            cx = (line["left"] + line["right"]) // 2
            cy = (line["top"] + line["bottom"]) // 2
            if frame is not None and not (
                    frame.left <= cx <= frame.right
                    and frame.top <= cy <= frame.bottom):
                stats["dropped_outside"] += 1
                continue
            if any(r.left <= cx <= r.right and r.top <= cy <= r.bottom
                   for r in leaf_rects):
                stats["dropped_centre"] += 1
                continue
            if _covered_fraction(line, leaf_rects) >= OCR_COVERED_DROP:
                stats["dropped_covered"] += 1
                continue
            key = (round(cx / 5), round(cy / 5))
            if key in seen:
                continue
            seen.add(key)
            kept.append(line)
        stats["outside"] = len(kept)

        if OCR_RECOGNIZE:
            # The reader has been working through the boxes since detection
            # ended. Hand it the verdict so it skips what the tree covers,
            # wait for it, then read whatever it had not reached.
            keep_ids = {id(line) for line in kept}
            scanner.set_keep(lambda line: id(line) in keep_ids)
            self._join_ocr()
            kept = scanner.recognize(kept)
            stats["rec_ms"] = scanner.stats["rec_ms"]
            stats["recognized"] = scanner.stats["recognized"]
            # Third test, on the words: a read that repeats the label of an
            # element it touches is that element's own caption seen through
            # a gap (an icon button's text beside it, a tab title with the
            # tab rect a pixel short) — the tree already has it.
            before = len(kept)
            kept = [l for l in kept
                    if not _repeats_touching_label(l, leaf_labels)]
            stats["dropped_label"] = before - len(kept)
            stats["recognized"] = len(kept)
        else:
            self._join_ocr()
            for line in kept:
                line["text"] = "(unread text)"

        for line in kept:
            cx = (line["left"] + line["right"]) // 2
            cy = (line["top"] + line["bottom"]) // 2
            self.element_index += 1
            line_rect = Rect(line["left"], line["top"],
                             line["right"], line["bottom"])
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
            self._find_deepest_container(self.element_tree, cx, cy).append(node)
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
                'acc_element': None,
            }
            self.found_elements.setdefault("OCR_TEXT", []).append(node)
            if SCREENSHOT:
                self.elements_to_draw.append({
                    "rect": line_rect,
                    "index": self.element_index,
                    "depth": 0,
                    "visibility": "full",
                    "source": "ocr",
                })
        stats["merged"] = len(kept)

    # ========== ANNOTATION ==========

    def _prepare_annotate_base(self):
        """Worker-thread half of annotation: LLM-payload downscale + RGB
        conversion. Downscale FIRST, annotate later — drawing before the
        resize would put the labels through the resampler too."""
        try:
            img = self._screenshot
            src_w, src_h = img.size
            shrink = llm_image_shrink(src_w, src_h)
            if shrink < 1.0:
                img = img.resize(
                    (max(1, int(src_w * shrink)), max(1, int(src_h * shrink))),
                    Image.Resampling.LANCZOS)
            else:
                img = img.copy()
            if img.mode in ('RGBA', 'LA', 'P'):
                rgb = Image.new('RGB', img.size, (255, 255, 255))
                rgb.paste(img, mask=img.split()[-1] if img.mode == 'RGBA' else None)
                img = rgb
            elif img.mode != 'RGB':
                img = img.convert('RGB')
            self._annotate_prep = (img, shrink)
        except Exception:
            self._annotate_prep = None

    def _capture_and_annotate(self, screen):
        """Annotate the captured screenshot and store it as base64."""
        if self._screenshot is None or self._annotate_prep is None:
            self._annotated_image_base64 = None
            return
        screenshot, shrink = self._annotate_prep
        self._plain_screenshot = self._screenshot.copy()
        draw = ImageDraw.Draw(screenshot)
        # Logical coords -> delivered pixels: HiDPI capture scale × downscale.
        draw_scale = self._scale * shrink
        ox, oy = screen["x"], screen["y"]
        font, stroke_w = _load_annotate_font()

        for item in self.elements_to_draw:
            rect = item["rect"]
            box = (
                int((rect.left - ox) * draw_scale),
                int((rect.top - oy) * draw_scale),
                int((rect.right - ox) * draw_scale),
                int((rect.bottom - oy) * draw_scale),
            )
            if item.get("source") == "ocr":
                # Text fills an OCR box edge to edge: give it a little room.
                pad = 3
                box = (box[0] - pad, box[1] - pad, box[2] + pad, box[3] + pad)
            draw.rectangle(box, outline=BOX_COLOR, width=BOX_WIDTH)
            # The index goes INSIDE the box, top-left, for every kind of box
            # — one convention to read. (macOS floats OCR labels above their
            # box; Ashish wants them inside, like the rest.)
            label = f"[{item['index']}]"
            tile = _label_tile(label, font, stroke_w)
            # Keep the whole label on the canvas: PIL silently clips a paste
            # past an edge, so a box near the right or bottom edge would lose
            # the end of its number. The tile is cropped to its ink, so its
            # size is exactly what has to fit.
            tx = max(0, min(box[0] + 4, screenshot.width - tile.width))
            ty = max(0, min(box[1] + 3, screenshot.height - tile.height))
            screenshot.paste(tile, (tx, ty), tile)

        self._encode_annotation(screenshot)

    def _encode_annotation(self, screenshot):
        """Single lossless encode. On the way to the model the tool registry
        (as_jpeg_base64) re-encodes it as JPEG at the same dimensions."""
        buffered = io.BytesIO()
        screenshot.save(buffered, format=LLM_IMAGE_FORMAT,
                        compress_level=LLM_IMAGE_COMPRESS_LEVEL)
        annotated_image_bytes = buffered.getvalue()
        self._annotated_image_base64 = base64.b64encode(
            annotated_image_bytes).decode('utf-8')

        if DEBUG:
            debug_dir = f"debug/iteration_{self._debug_iteration}"
            os.makedirs(debug_dir, exist_ok=True)
            with open(f"{debug_dir}/annotated_screenshot.png", "wb") as f:
                f.write(annotated_image_bytes)

        if FRONTEND and self.frontend_callback:
            if DEBUG:
                self.frontend_callback(self._annotated_image_base64)
            else:
                # Plain screenshot for production frontend (human preview).
                plain = self._plain_screenshot
                w, h = plain.size
                md = FRONTEND_IMAGE_MAX_DIMENSION
                if w > md or h > md:
                    if w > h:
                        plain = plain.resize((md, int(h * md / w)),
                                             Image.Resampling.LANCZOS)
                    else:
                        plain = plain.resize((int(w * md / h), md),
                                             Image.Resampling.LANCZOS)
                if plain.mode in ('RGBA', 'LA', 'P'):
                    rgb = Image.new('RGB', plain.size, (255, 255, 255))
                    rgb.paste(plain, mask=plain.split()[-1]
                              if plain.mode == 'RGBA' else None)
                    plain = rgb
                elif plain.mode != 'RGB':
                    plain = plain.convert('RGB')
                buf = io.BytesIO()
                plain.save(buf, format="JPEG", quality=FRONTEND_IMAGE_QUALITY)
                self.frontend_callback(
                    base64.b64encode(buf.getvalue()).decode('utf-8'))

    # ========== OUTPUT ==========

    @property
    def all_trees(self):
        """Every section's tree roots, in the order get_scan_data emits them.

        Callers that need the whole scan — audit_scan, anything counting
        nodes — should walk this rather than naming the trees, so a
        new section does not silently fall out of their reckoning."""
        return [self.menu_bar_tree, self.task_bar_tree,
                self.desktop_tree, self.element_tree]

    def get_scan_data(self):
        """Get scan data for use by AgentService.

        Returns:
            tuple: (element_tree_text, annotated_image_base64, uac_detected)
                   uac_detected is always False on Linux (no UAC).
        """
        element_tree_text = ""
        if self.menu_bar_tree:
            element_tree_text += "<menu_bar>\n"
            element_tree_text += self._get_tree_text_recursive(self.menu_bar_tree, 1)
            element_tree_text += "</menu_bar>\n\n"

        if self.task_bar_tree:
            element_tree_text += "<task_bar>\n"
            element_tree_text += self._get_tree_text_recursive(
                self.task_bar_tree, 1)
            element_tree_text += "</task_bar>\n\n"

        if self.desktop_tree:
            element_tree_text += "<desktop>\n"
            element_tree_text += self._get_tree_text_recursive(
                self.desktop_tree, 1)
            element_tree_text += "</desktop>\n\n"

        element_tree_text += "<top_layer>\n"
        if self.top_layer_info:
            layer_name = _xml_escape(self.top_layer_info["name"])
            element_tree_text += (
                f'  <application name="{layer_name}" '
                f'type="{self.top_layer_info["type"]}" />\n')
        else:
            element_tree_text += '  <application name="Desktop" type="app" />\n'
        element_tree_text += self._get_tree_text_recursive(self.element_tree, 1)
        element_tree_text += "</top_layer>\n"

        return element_tree_text, self._annotated_image_base64, False

    def _get_tree_text_recursive(self, tree_list, depth):
        """Generate tree text recursively — matches the Windows format."""
        result = ""
        indent = "  " * depth
        for item in tree_list:
            name = _xml_escape(item['name'])
            visibility = item.get('visibility', 'full')
            # OCR_TEXT — text the tree missed — uses the other platforms'
            # distinct Line form, so the agent can tell it from a control.
            if item.get("source") == "ocr":
                result += (f'{indent}[{item["index"]}]<Line="{name}", '
                           f'type="OCR_TEXT", active="True", '
                           f'visibility="full" />\n')
                if item.get("children"):
                    result += self._get_tree_text_recursive(
                        item["children"], depth + 1)
                continue
            clipped_by = item.get('clipped_by', None)
            clipped_by_attr = ""
            if clipped_by and visibility != "full":
                clipped_by_attr = f', clipped_by="{_xml_escape(clipped_by)}"'

            if item.get("value"):
                value = _xml_escape(item["value"])
                result += (f'{indent}[{item["index"]}]<element name="{name}", '
                           f'valuePattern.value="{value}", type="{item["type"]}", '
                           f'active="{item["active"]}", visibility="{visibility}"'
                           f'{clipped_by_attr} />\n')
            else:
                result += (f'{indent}[{item["index"]}]<element name="{name}", '
                           f'type="{item["type"]}", active="{item["active"]}", '
                           f'visibility="{visibility}"{clipped_by_attr} />\n')
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
        if not DEBUG:
            return
        debug_dir = f"debug/iteration_{self._debug_iteration}"
        os.makedirs(debug_dir, exist_ok=True)
        with open(f"{debug_dir}/tree.txt", "w", encoding="utf-8") as f:
            text, _, _ = self.get_scan_data()
            f.write(text)


# ========== MAIN PROGRAM (full scan + step-by-step debug commands) ==========

# ========== DEV LOOP ==========
# preflight -> scan -> audit. AutoCua/linux/tree/test.py is a plain trigger
# for cmd_scan below (like the macOS test.py is for its element.py); every
# check lives here, next to the code it checks.

def preflight(verbose=False):
    """Check the accessibility stack. Returns False when a scan cannot
    meaningfully run.

    Each check below has already cost the port a debugging session: an a11y
    bus with nothing published on it, a Wayland session with no gnome-shell
    actors to resolve window origins against, a PIL fallback font that cannot
    stroke the index labels. Nothing here prints on a healthy machine;
    `verbose` prints every reading."""
    detail = print if verbose else (lambda *_a, **_k: None)
    ok = True

    session = "Wayland" if _IS_WAYLAND else "X11"
    desktop = os.environ.get("XDG_CURRENT_DESKTOP", "?")
    detail(f"Session     : {session}  ({desktop})")

    # An AT-SPI desktop with almost nothing on it means the bus is alive but
    # toolkits are not publishing — usually org.a11y.Status.IsEnabled was
    # never set, which is exactly what enable_screen_reader_flag() fixes at
    # import and scan time. Two is the floor: gnome-shell plus one client.
    try:
        apps = [_safe_name(a) or "?" for a in get_desktop_apps()]
    except Exception as e:
        print(f"AT-SPI      : UNREACHABLE — {e}")
        print("              Install gir1.2-atspi-2.0 and check that the "
              "a11y bus is running.")
        return False

    detail(f"AT-SPI      : {len(apps)} applications on the bus")
    if len(apps) < 2:
        print(f"AT-SPI      : only {len(apps)} application(s) on the bus — "
              f"accessibility is probably off:")
        print("              gsettings set org.gnome.desktop.interface "
              "toolkit-accessibility true")
        ok = False

    # Wayland clients cannot know their own global position, so the origin is
    # recovered by size-matching gnome-shell's "Wayland window" actors. No
    # gnome-shell on the bus means that match can never succeed and every
    # coordinate stays window-relative.
    if _IS_WAYLAND:
        actors = _collect_shell_window_actors()
        detail(f"Shell actors: {len(actors)}  (Wayland window-origin sources)")
        if not actors:
            print("Shell actors: none — gnome-shell publishes no window "
                  "actors, so coordinates will be")
            print("              window-relative and clicks will miss.")
            ok = False

    # OCR needs onnxruntime + numpy and the two PP-OCRv6 models (fetched on
    # first use). Not fatal — the scan runs on AT-SPI alone — but a tree with
    # no OCR fill-in is a different, blinder result, so say so every time.
    if OCR:
        reason = ocr_unavailable_reason()
        if reason:
            print(f"OCR         : {reason}")
        else:
            detail("OCR         : PP-OCRv6 ready")

    # load_default() hands back a bitmap font with no FreeType face, and PIL
    # renders strokes through FreeType — so the index labels lose their dark
    # rim and magenta digits become unreadable on light UI.
    _, stroke_w = _load_annotate_font()
    detail(f"Label font  : {'TrueType' if stroke_w else 'PIL bitmap fallback'}")
    if not stroke_w:
        print("Label font  : PIL bitmap fallback — labels will be unstroked "
              "and hard to read.")
        print("              Install fonts-dejavu-core.")

    return ok


def _iter_tree(nodes):
    """Depth-first walk of a hierarchical element tree."""
    for node in nodes:
        yield node
        yield from _iter_tree(node.get("children") or [])


def audit_scan(scanner, screen, verbose=False):
    """Check a finished scan for the failures that do not raise.

    Returns a list of human-readable problems; an empty list means the scan
    looks structurally sound (it does NOT mean the labels are good).
    `verbose` also prints the readings behind the checks."""
    detail = print if verbose else (lambda *_a, **_k: None)
    problems = []
    nodes = [n for tree in scanner.all_trees for n in _iter_tree(tree)]
    mapping = scanner.get_elements_mapping()

    # --- Wayland window origin -------------------------------------------
    # The highest-value check. AT-SPI reports client windows at (0, 0) on
    # Wayland; when the shell-actor size match fails, prepare_scan() falls
    # back to a (0, 0) offset and carries on. The tree still renders
    # perfectly — every rect is just silently window-relative. Re-running the
    # probe here is cheap and tells the dev which of the two worlds they are
    # looking at.
    if _IS_WAYLAND:
        actors = _collect_shell_window_actors()
        _, win, actor = find_top_window(actors)
        frame = acc_extents(win) if win else None
        if frame:
            # Ask exactly what prepare_scan asks, topmost fallback included,
            # or this cries wolf on every scan the fallback quietly rescued.
            offset, how, _act = resolve_window_offset(
                frame, actors, allow_topmost=True, prefer=actor)
            if offset is None:
                problems.append(
                    "Wayland window origin unresolved — gnome-shell published "
                    "no window actors at all, so every coordinate is "
                    "window-relative and every click will miss.")
            elif how == "topmost":
                # Almost certainly right, still worth surfacing: AT-SPI
                # reported a size no actor has — the signature of a stale
                # frame.
                detail(f"Win origin  : +{offset[0]},+{offset[1]} "
                       f"(inferred from the topmost actor — AT-SPI reported "
                       f"a size no actor matched)")
            else:
                detail(f"Win origin  : +{offset[0]},+{offset[1]} ({how})")
            # Size is the only identity a Wayland clone has, so two windows
            # of one size (two maximized windows, two half-screen tiles) are
            # paired by keyboard focus instead. Usually right, worth knowing:
            # if the wrong app is reported, this is where to look first.
            twins = [a for a in live_actors(actors)
                     if a["width"] == frame["width"]
                     and a["height"] == frame["height"]]
            if len(twins) > 1:
                detail(f"Win pairing : {len(twins)} on-screen windows share "
                       f"{frame['width']}x{frame['height']} — the z-order "
                       f"pairing broke the tie on keyboard focus")

    # --- Structural consistency ------------------------------------------
    # elements_mapping is what the controller clicks through, so it must
    # hold exactly one entry per node the tree numbered.
    if len(mapping) != len(nodes):
        problems.append(
            f"elements_mapping has {len(mapping)} entries but the tree holds "
            f"{len(nodes)} nodes — an index was dropped or reused.")

    degenerate = [n for n in nodes
                  if n["rect"].right <= n["rect"].left
                  or n["rect"].bottom <= n["rect"].top]
    if degenerate:
        problems.append(
            f"{len(degenerate)} element(s) have a zero or inverted rect — "
            f"e.g. {degenerate[0]['name']!r} ({degenerate[0]['type']}).")

    # walk() drops anything the screen does not intersect, but the shell
    # and desktop passes bypass that filter.
    off_screen = [n for n in nodes
                  if n["rect"].right <= screen["x"]
                  or n["rect"].left >= screen["x"] + screen["width"]
                  or n["rect"].bottom <= screen["y"]
                  or n["rect"].top >= screen["y"] + screen["height"]]
    if off_screen:
        problems.append(
            f"{len(off_screen)} element(s) lie entirely outside the screen "
            f"bounds {screen['width']}x{screen['height']} — a bad offset "
            f"correction, or a monitor get_screen() did not account for.")

    # --- Coverage ---------------------------------------------------------
    # Counted on the surface in front, NOT the whole mapping: the top bar and
    # the dash alone contribute a dozen elements, which let a scan whose
    # <top_layer> was completely empty pass as healthy (measured: 12
    # elements, all shell chrome, over a Cursor window every one of whose
    # elements a mis-paired occluder had deleted). Chromium trees are woken
    # at runtime by electron_nudge(); Gecko reads org.a11y.Status.IsEnabled
    # at startup only, so a Firefox started while it was off needs a restart.
    front_nodes = list(_iter_tree(scanner.element_tree))
    front = len(front_nodes)
    native = sum(1 for n in front_nodes if n.get("source") != "ocr")
    surface = (scanner.top_layer_info or {}).get("name", "Desktop")
    if surface != "Desktop" and front == 0:
        ocr_note = ("" if scanner.ocr_stats else
                    " OCR was off, so nothing could fill in for it — see the "
                    "OCR line above.")
        problems.append(
            f"the surface in front ({surface!r}) contributed 0 elements — "
            f"either it never published an AT-SPI tree, or everything it "
            f"published was deleted afterwards (an occluder paired with the "
            f"wrong window, see _apply_stack_occlusion). The scanner output "
            f"above carries the pairing narration.{ocr_note}")
    elif surface != "Desktop" and native == 0:
        # The app is blind to AT-SPI (App Center / Flutter, a renderer that
        # never woke) and OCR is carrying the whole window. Correct, and
        # worth knowing: every target in front is a coordinate click.
        detail(f"Coverage    : {surface!r} published no AT-SPI tree — all "
               f"{front} of its elements are OCR text")
    elif surface != "Desktop" and front < 10:
        # A small dialog (polkit prompt, "Save changes?") legitimately has
        # a handful of widgets, so this is a reading, not a verdict.
        detail(f"Coverage    : only {front} element(s) from {surface!r} — "
               f"fine for a small dialog; for a full window it means the "
               f"app never published its tree (restart it; for "
               f"Electron/Chromium check ELECTRON_NUDGE)")
    elif surface == "Desktop" and len(mapping) < 10:
        problems.append(
            f"only {len(mapping)} element(s) found on the desktop — the "
            f"shell chrome or the desktop icons did not come through.")

    # Only a capture failure is worth reporting: with nothing to draw the
    # annotation is skipped, and the coverage check above covers that.
    if SCREENSHOT and scanner.elements_to_draw \
            and not scanner._annotated_image_base64:
        problems.append(
            "no annotated screenshot was produced — the XDG portal capture "
            "was denied or timed out (see take_screenshot).")

    return problems


def untracked_roles():
    """AT-SPI roles the last walk met but ELEMENT_CONFIG does not track — the
    porting to-do list, since _seen_roles is everything walk() laid eyes on."""
    return sorted(r for r in _seen_roles if r and r not in ELEMENT_CONFIG)


def _countdown(seconds, verb):
    for i in range(seconds, 0, -1):
        print(f"  {verb} in {i}... (focus the window you want)")
        time.sleep(1)
    if seconds > 0:
        print()


def cmd_topmost():
    """Step 1: topmost-application detection, in isolation."""
    _countdown(3, "Detecting")
    print("All AT-SPI applications and their windows:")
    for app in get_desktop_apps():
        name = _safe_name(app) or "?"
        lines = []
        for win in iter_children(app):
            states = acc_states(win)
            if states is None:
                continue
            flags = "".join((
                "A" if states.contains(_STATE.ACTIVE) else "-",
                "S" if states.contains(_STATE.SHOWING) else "-",
            ))
            ext = acc_extents(win)
            geo = (f"{ext['width']}x{ext['height']}@({ext['x']},{ext['y']})"
                   if ext else "no-extents")
            lines.append(f"    [{flags}] {win.get_role_name()} "
                         f"{(_safe_name(win) or '')[:45]!r} {geo}")
        print(f"  {name}" + ("" if lines else "  (no windows)"))
        for ln in lines:
            print(ln)
    print("  (flags: A=active, S=showing)\n")

    actors = _collect_shell_window_actors() if _IS_WAYLAND else []
    cands = candidate_toplevels()
    if actors:
        # The z-order stack is the actual decision input on Wayland; show it,
        # so a wrong answer can be traced to a wrong pairing.
        print("Z-order stack from gnome-shell (bottom to top):")
        for app_, win_, _fr, actor in get_window_stack(actors, cands):
            print(f"    {(_safe_name(app_) or '?')!r:22} "
                  f"{actor['width']}x{actor['height']}"
                  f"@({actor['x']},{actor['y']})  "
                  f"{(_safe_name(win_) or '')[:40]!r}")
        print()

    app, win, actor = find_top_window(actors, cands, narrate=True)
    if win is None:
        print("RESULT: no active or visible window -> would scan Desktop only")
        return

    states = acc_states(win)
    if actor is not None:
        via = "z-order: topmost shell actor"
    elif states and states.contains(_STATE.ACTIVE):
        via = "fallback: ACTIVE state (no z-order source)"
    else:
        via = "fallback: largest visible window (no z-order source)"
    ext = acc_extents(win)
    print(f"RESULT: topmost app = {_safe_name(app)!r}")
    print(f"        window      = {_safe_name(win)!r}  (found via {via})")
    print(f"        raw extents = {ext}")
    if _IS_WAYLAND and ext:
        off = resolve_window_offset(ext, actors, prefer=actor)[0]
        print(f"        shell actors seen = {len(actors)}")
        if off is None:
            print("        origin correction = UNRESOLVED (no actor matched)")
        else:
            print(f"        origin correction = +{off}  "
                  f"-> true position ({ext['x'] + off[0]}, {ext['y'] + off[1]})")


def cmd_screenshot():
    """Step 2: screen capture, in isolation."""
    screen = get_screen()
    print(f"screen (logical coords): {screen}")
    t0 = time.time()
    img, scale = take_screenshot(screen)
    if img is None:
        print("RESULT: FAILED — no capture method worked")
        return
    img.save("element_screenshot.png")
    print(f"RESULT: captured {img.width}x{img.height} pixels "
          f"in {time.time() - t0:.2f}s")
    print(f"        scale (logical->pixels) = {scale:.3f}")
    print("        saved -> element_screenshot.png  (open it to verify)")


def cmd_walk():
    """Step 3: element walk of the topmost window, no drawing."""
    _countdown(3, "Scanning")
    screen = get_screen()
    ctx = prepare_scan(screen)
    if ctx["window"] is None:
        print("No topmost window found — scanning shell/desktop only.")
    else:
        print(f"Walking window of app (offset correction {ctx['offset']})...")

    t0 = time.time()
    app_info, menu_items, elements = collect_elements(screen, ctx)
    print(f"\nRESULT: {len(elements)} elements + {len(menu_items)} menu-bar "
          f"items in {time.time() - t0:.2f}s")
    print(f"        application = "
          f"{app_info['name'] if app_info else 'Desktop'!r}")
    counts = Counter(clean_type(e["type"]) for e in elements)
    print("        by type:", ", ".join(
        f"{t}={n}" for t, n in counts.most_common()))

    print("\nMenu bar items:")
    for m in menu_items:
        print(f"  [{clean_type(m['type'])}] {m['label']!r} "
              f"at ({m['x']},{m['y']}) {m['width']}x{m['height']}")
    print("\nFirst 30 elements (corrected screen coords):")
    for e in sorted(elements, key=lambda e: (e["y"], e["x"]))[:30]:
        print(f"  [{clean_type(e['type'])}] {e['label'][:35]!r} "
              f"at ({int(e['x'])},{int(e['y'])}) "
              f"{int(e['width'])}x{int(e['height'])} {e['visibility']}"
              + (f"  value={e['value']!r}" if e.get("value") else ""))


def _parse_scan_args(argv):
    """(countdown_seconds, verbose) from a bare number and/or -v, any order."""
    seconds, verbose = 5, False
    for arg in argv:
        if arg in ("-v", "--verbose"):
            verbose = True
        else:
            try:
                seconds = max(0, int(arg))
            except ValueError:
                print("Usage: element.py scan [countdown_seconds] [-v]")
                sys.exit(1)
    return seconds, verbose


def cmd_scan(argv=()):
    """The development loop: preflight, countdown, one timed scan, audit.

    Reported BY EXCEPTION. A healthy run prints two lines — which window the
    scanner decided it was looking at, and how long deciding took — and
    nothing else; any extra output means something needs attention, and the
    scanner's own narration (tree-ready polling, the Electron nudge, portal
    failures, the window pairing) is replayed under it, because that is
    usually where the cause shows up. `-v` prints everything regardless.

    This matters because the Linux port fails quietly: an unresolved Wayland
    origin, an Electron app that never published its tree, a role nobody
    added to ELEMENT_CONFIG — none of those raise, they just produce a tree
    that reads fine and points at the wrong pixels. The audit names the
    cause so this file does not have to be bisected by hand."""
    seconds, verbose = _parse_scan_args(argv)
    detail = print if verbose else (lambda *_a, **_k: None)

    if not preflight(verbose):
        print("Preflight failed — fix the above, then re-run.")
        sys.exit(1)

    # Countdown — switch to the window you want to scan. 0 skips it and scans
    # the terminal, the fastest loop when the walk itself is being debugged.
    _countdown(seconds, "Scanning")

    screen = get_screen()
    t0 = time.time()
    scanner = UIElementScanner(ELEMENT_CONFIG)
    chatter = io.StringIO()
    with contextlib.redirect_stdout(sys.stdout if verbose else chatter):
        scanner.scan_elements()
    scan_time = time.time() - t0

    tree_text, image_b64, _ = scanner.get_scan_data()
    mapping = scanner.get_elements_mapping()

    print(f"\nApplication : {scanner.application_name}")
    print(f"Scan time   : {scan_time:.2f}s")

    detail(f"Elements    : {len(mapping)} "
           f"({len(scanner.menu_bar_tree)} in the top bar)")
    detail(f"Screen      : {screen['width']}x{screen['height']} logical")
    if scanner._screenshot is not None:
        detail(f"Capture     : {scanner._screenshot.width}x"
               f"{scanner._screenshot.height} px  (scale {scanner._scale:.2f})")
    detail(f"Image       : {'yes' if image_b64 else 'no'}")
    detail(f"Tree text   : {len(tree_text)} chars")
    st = scanner.ocr_stats
    if st:
        detail(f"OCR         : {st['detected']} boxes detected, "
               f"{st['outside']} outside the tree, {st['merged']} merged  "
               f"(det {st['det_ms']:.0f} ms, rec {st['rec_ms']:.0f} ms)")
        detail(f"OCR filter  : dropped {st.get('dropped_outside', 0)} outside "
               f"the front window, {st.get('dropped_centre', 0)} centred on "
               f"an element, {st.get('dropped_covered', 0)} mostly covered "
               f"by elements, {st.get('dropped_label', 0)} repeating a "
               f"touching label")
    elif OCR:
        detail(f"OCR         : off — "
               f"{ocr_unavailable_reason() or 'no screenshot to read'}")

    problems = audit_scan(scanner, screen, verbose)

    counts = Counter(n["type"] for n in _iter_tree(scanner.element_tree))
    if counts:
        detail("\nBy type     : " + ", ".join(
            f"{t}={n}" for t, n in counts.most_common()))
    backlog = untracked_roles()
    if backlog:
        detail(f"\nRoles seen but NOT tracked ({len(backlog)}) — candidates "
               f"for ELEMENT_CONFIG:")
        detail("  " + ", ".join(backlog))

    if problems:
        held = chatter.getvalue().strip()
        if held:
            print("\nScanner output:")
            print(held)
        print(f"\n{len(problems)} problem(s) found:")
        for p in problems:
            print(f"  - {p}")

    # Debug artifacts go to a RELATIVE "debug/iteration_N", next to wherever
    # this was launched from — print the absolute path.
    detail(f"\nDebug saved to: "
           f"{os.path.abspath(f'debug/iteration_{scanner._debug_iteration}')}/")


def main(argv=None):
    """`element.py [scan [seconds] [-v] | topmost | screenshot | walk]`.
    Bare scan options are accepted without the word: `element.py 0 -v`."""
    argv = sys.argv[1:] if argv is None else list(argv)
    commands = {"scan": cmd_scan, "topmost": cmd_topmost,
                "screenshot": cmd_screenshot, "walk": cmd_walk}
    cmd = argv[0].lstrip("-").lower() if argv else "scan"
    if cmd in commands:
        fn, rest = commands[cmd], argv[1:]
    elif cmd in ("v", "verbose") or cmd.isdigit():
        fn, rest = cmd_scan, argv
    else:
        print(f"Unknown command {cmd!r}. "
              f"Usage: python3 element.py [{'|'.join(commands)}] "
              f"[countdown_seconds] [-v]")
        sys.exit(1)
    fn(rest) if fn is cmd_scan else fn()


if __name__ == "__main__":
    main()
