import requests
import json
import threading
import xml.etree.ElementTree as ET
from PIL import Image, ImageDraw, ImageFont
import io
import base64
import os
import time
from ..controller.service import controller_service
from ...vault.service import vault_service

# ========== FLAGS ==========
DEBUG = False        # Set to True to save files to debug folders, False for direct LLM only
FRONTEND = True      # Set to True when running from app.py to send images to frontend

# Final geometry/encoding of the annotated screenshot — mirrors Windows/macOS
# element.py. The image is encoded ONCE, here; those exact bytes are the LLM
# payload and what DEBUG writes to disk. Callers must NOT re-encode.
#
# Two independent caps, both orientation-agnostic:
#   MAX_EDGE   - the long side. Device screenshots are tall (e.g. 1170x2532),
#                so this is what binds on phones. Vision models resize anything
#                larger themselves, which would put the annotations through
#                THEIR resampler and undo the crisp-label work.
#   MAX_PIXELS - a total-area budget, since models also cap by token count
#                (~750px per image token).
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
# Providers hardcode this in their request builders (grep: image/jpeg).
LLM_IMAGE_MEDIA_TYPE = "image/jpeg"

# WDA request deadlines, (connect, read). Neither call had one before: a wedged
# WebDriverAgent hung the scan forever. That was survivable while both requests
# ran on the main thread; it is not once the screenshot moves onto a worker,
# because no join timeout can reclaim a thread parked in recv() with no
# deadline of its own. The read budget is generous — /source is linear in the
# element count (~10ms each, measured), so a dense screen legitimately takes
# seconds.
WDA_SOURCE_TIMEOUT = (5, 60)
WDA_SCREENSHOT_TIMEOUT = (5, 30)
# How long the main thread waits for the screenshot worker once the XML is in.
# Just past the worker's own read deadline, so in a normal failure the worker
# reports its own error rather than being abandoned mid-flight.
SCREENSHOT_JOIN_TIMEOUT = 35

# Attributes WDA must NOT compute — the single biggest win in the whole scan.
#
# `visible` is not read off the accessibility snapshot; WDA derives it by
# hit-testing each element against whatever is drawn on top of it, at roughly
# 8ms per element. On a 364-element home screen that alone is 82% of the
# request (3.60s -> 0.66s when excluded); `accessible` costs a further 15%.
# Every other attribute — label, name, value, enabled, index — measured free.
#
# Because /source is almost exactly linear in element count (fitted at 9.7ms
# per node with ~0.00s fixed overhead), and these two attributes are nearly
# all of that slope, dropping them collapses the request:
#     home 3.62s -> 0.39s | Settings 1.44 -> 0.22 | Messages 0.54 -> 0.11
# Visibility is recovered from geometry instead — see _is_visible.
WDA_EXCLUDED_ATTRIBUTES = "visible,accessible"

# Smallest element edge, in iOS points, still treated as real. Geometry cannot
# see occlusion, so an element hidden BEHIND something (under a raised
# keyboard, say) still looks on-screen. Measured with the keyboard up, the only
# elements that leaked through were 1x1 unlabelled artifacts — so requiring 2pt
# removes the entire observed error, and nothing a finger could hit is that
# small anyway.
MIN_ELEMENT_SIZE = 2.0

# Index-label styling, in delivered pixels (labels are drawn AFTER the downscale).
# The stroke is a rim hugging the glyphs, NOT a filled chip — the UI underneath
# stays visible, and it supplies the luma contrast magenta lacks on its own.
#
# Sized for the DELIVERED image, not the desktop trees' 13px: a phone screenshot
# is tall, so MAX_EDGE binds on the long side and the frame arrives ~1060x2300 —
# roughly 2.7 delivered pixels per iOS point. At 13px the digits landed under 5
# points tall and the model could not read them apart. The stroke scales with the
# font; a 2px rim around 32px glyphs reads as a hairline and loses its contrast.
LABEL_FONT_SIZE = 32
LABEL_STROKE = 3
LABEL_STROKE_COLOR = (0, 0, 0)

# Configuration embedded in code
config = {
    "element_types": {
        "button": {
            "type": "XCUIElementTypeButton",
            "enabled": True
        },
        "search_field": {
            "type": "XCUIElementTypeSearchField",
            "enabled": True
        },
        "text_field": {
            "type": "XCUIElementTypeTextField",
            "enabled": True
        },
        "switch": {
            "type": "XCUIElementTypeSwitch",
            "enabled": True
        },
        "application": {
            "type": "XCUIElementTypeApplication",
            "enabled": True
        },
        "icon": {
            "type": "XCUIElementTypeIcon",
            "enabled": True
        },
        "page_indicator": {
            "type": "XCUIElementTypePageIndicator", 
            "enabled": True
        },
        "scroll_view": {
            "type": "XCUIElementTypeScrollView",
            "enabled": True
        },
        "secure_text_field": {
            "type": "XCUIElementTypeSecureTextField",
            "enabled": True
        },
        "other": {
            "type": "XCUIElementTypeOther",
            "enabled": True
        },
        "slider": {
            "type": "XCUIElementTypeSlider",
            "enabled": True
        },
        "static_text": {
            "type": "XCUIElementTypeStaticText",
            "enabled": True
        }
    },
    "only_visible": True
}

# WebDriverAgent endpoint — port 8100 unless this process was given its own
# (parallel simulator tasks each drive their own simulator on their own port).
from AutoCua.ios_connector.session import wda_url as _wda_url

wda_url = _wda_url()

def _element_rect(element):
    """An element's frame as (x, y, w, h), or None if the XML is malformed."""
    try:
        return (float(element.get('x') or 0), float(element.get('y') or 0),
                float(element.get('width') or 0), float(element.get('height') or 0))
    except (TypeError, ValueError):
        return None


def _is_visible(rect, screen_w, screen_h):
    """Stand in for WDA's `visible` attribute, computed from geometry alone.

    Tests the element's CENTRE rather than any overlap, because the centre is
    exactly the point controller/service.py taps: an element whose centre falls
    off-screen cannot be actioned even if a sliver of it shows. That choice is
    also what makes this agree with WDA on partially-scrolled rows — a cell
    straddling the bottom edge is reported not-visible by both.

    Validated against WDA's own answer on five screens: Settings, Calendar,
    Messages and Contacts matched element-for-element; the home screen differed
    only by a 0x0 icon. The gap it cannot close is occlusion (see
    MIN_ELEMENT_SIZE).
    """
    x, y, w, h = rect
    if w < MIN_ELEMENT_SIZE or h < MIN_ELEMENT_SIZE:
        return False
    cx, cy = x + w / 2.0, y + h / 2.0
    return 0 <= cx <= screen_w and 0 <= cy <= screen_h


def _phase(start, end):
    """Elapsed seconds between two phase marks, floored at zero.

    Marks are pre-seeded to the scan's start, so a scan that bails early
    leaves every later mark BEHIND the one before it. Without the floor those
    unreached phases would print as negative durations; clamped, they read a
    self-explanatory 0.00.
    """
    return max(0.0, end - start)


_ANNOTATE_FONT = None  # (font, stroke_width) — re-parsing the TTF every scan is waste


def _load_annotate_font():
    """Load the index-label font once per process.

    One shared face is safe here, unlike mac/tree/element.py which hands
    each caller its own: only the main thread ever rasterises glyphs in this
    file — the screenshot worker touches no fonts — and PIL/FreeType faces are
    only unsafe under *concurrent* rendering.
    """
    global _ANNOTATE_FONT
    if _ANNOTATE_FONT is None:
        font = None
        # Bold candidates first, so the digits carry more ink per glyph at the
        # same size.
        for path in ("/System/Library/Fonts/Supplemental/Arial Bold.ttf",
                     "arialbd.ttf",
                     "/System/Library/Fonts/Helvetica.ttc",
                     "arial.ttf"):
            try:
                font = ImageFont.truetype(path, LABEL_FONT_SIZE)
                break
            except Exception:
                continue
        # PIL renders strokes through FreeType, which load_default()'s bitmap
        # font lacks - asking for one there raises.
        stroke_w = LABEL_STROKE if font is not None else 0
        if font is None:
            font = ImageFont.load_default()
        _ANNOTATE_FONT = (font, stroke_w)
    return _ANNOTATE_FONT


class UIElementScanner:
    """Scanner for iPhone UI elements using WebDriverAgent"""
    
    def __init__(self, config=None, frontend_callback=None):
        self.config = config or {}
        self.frontend_callback = frontend_callback
        self.elements_mapping = {}
        
        # Store scan data in memory
        self.element_tree_text = ""
        self.image_base64 = None
        # The app in front, from the page source's root XCUIElementTypeApplication
        # ("" on the home screen). Read for free on every scan; the skills
        # lookup keys on it, and the tree's first line shows it to the model.
        self.application_name = ""
        # Form factor for the model's `current_device` line. Family comes from
        # WDA /status (UIDevice idiom) - asked ONCE and kept for the run, since
        # the root frame is the ACTIVE APP's frame and shrinks in Split View /
        # Slide Over / iPhone-compat mode. Orientation comes from the full
        # screenshot after the worker joins, for the same reason.
        self._status_family = None   # cached "iPhone" | "iPad"
        self.device_family = ""      # "iPhone" | "iPad" | "" (unknown)
        self.orientation = ""        # "portrait" | "landscape" | ""

    def _wda_device_family(self):
        """'iPhone' | 'iPad' from WDA's session-less GET /status (value.device),
        cached on first success; '' when it can't be read."""
        if self._status_family:
            return self._status_family
        try:
            r = requests.get(f"{wda_url}/status", timeout=3)
            if r.status_code == 200:
                dev = str(((r.json() or {}).get("value") or {}).get("device") or "").lower()
                fam = {"ipad": "iPad", "iphone": "iPhone"}.get(dev, "")
                if fam:
                    self._status_family = fam
                return fam
        except Exception:
            pass
        return ""

    def _compose_tree_text(self, body):
        device_line = (f"current_device: {self.device_family} ({self.orientation})\n"
                       if self.device_family and self.orientation else "")
        return device_line + f"current_application: {self.application_name or 'home screen'}\n" + body

    def _fetch_screenshot(self, out):
        """Fetch the screenshot and run every pixel step that does NOT need the
        XML: HTTP, base64 decode, PNG decode, the LLM-payload downscale and the
        RGB conversion. Results land in `out`.

        Runs on a worker started before GET /source. How much actually overlaps
        depends on WDA, and the measured answer is "the local half, not the
        HTTP": WDA serves requests from a single XCTest queue, so this
        /screenshot sits queued behind the snapshot rather than running beside
        it. What that still buys is the ~0.20s of base64+PNG decode and resize,
        which happen off the main thread's critical path. Measured on the home
        screen: shot=3.95s of worker elapsed against wait=0.28s of main-thread
        join — i.e. the request was blocked ~3.6s, then its pixel work ran
        while the main thread was finishing. Net 0.15-0.4s per scan depending
        on screen density.

        The pixel work is safe to background only because the main thread is
        parked in recv() (no GIL, no bytecode) while it runs. Beside a CPU-BUSY
        thread PNG decode degrades 14x — it re-acquires the GIL per 64KB chunk
        and loses every 5ms switch — which is the same effect that got
        mac's glyph pre-render reverted. If /source ever drops below
        ~0.45s these two would start colliding.

        Touches no scan_elements local, so join() is the only synchronisation
        needed — it publishes both `out` and the Image to the main thread.
        """
        t0 = time.perf_counter()
        try:
            screenshot_response = requests.get(f"{wda_url}/screenshot",
                                               timeout=WDA_SCREENSHOT_TIMEOUT)
            out['status'] = screenshot_response.status_code
            if screenshot_response.status_code != 200:
                return

            # Get base64 image
            screenshot_data = screenshot_response.json()
            image_base64 = screenshot_data['value']
            # Stashed BEFORE any PIL work: this plain frame is what the
            # non-DEBUG frontend branch sends, so it has to survive a decode
            # failure too.
            out['raw_base64'] = image_base64

            # Decode base64 to image
            image_bytes = base64.b64decode(image_base64)
            image = Image.open(io.BytesIO(image_bytes))
            # Image.open is lazy. Without this the 0.16s decode is deferred to
            # the first draw call - i.e. back on the main thread and back on
            # the critical path - on any device where shrink comes out >= 1.0
            # and the resize below is skipped.
            image.load()

            # Downscale to the final payload size FIRST, then annotate (on the
            # main thread, after the join). Drawing before the resize would put
            # every box and label through the resampler too, shrinking the
            # digits and averaging their outline away. The scale factors are
            # derived from image.size, so they follow this automatically.
            src_w, src_h = image.size
            shrink = min(1.0,
                         LLM_IMAGE_MAX_EDGE / max(src_w, src_h),
                         (LLM_IMAGE_MAX_PIXELS / (src_w * src_h)) ** 0.5)
            if shrink < 1.0:
                # Floor, not round: rounding both sides up can push the product
                # back over MAX_PIXELS, so the cap would not hold.
                image = image.resize(
                    (max(1, int(src_w * shrink)), max(1, int(src_h * shrink))),
                    Image.Resampling.LANCZOS)

            # Drawing into a palette-mode image would quantise the label
            # colour, so normalise to RGB before handing it over.
            if image.mode != 'RGB':
                image = image.convert('RGB')
            out['image'] = image
        except Exception as e:
            # Re-raised by the main thread inside the existing try, so a broken
            # screenshot still prints "Error: ..." instead of vanishing on a
            # thread nobody is watching.
            out['error'] = e
        finally:
            out['elapsed'] = time.perf_counter() - t0

    def scan_elements(self):
        """Scan iPhone UI elements and capture screenshot"""
        # Wall-clock trace of one scan. The two clocks are deliberate: strftime
        # for the human-readable stamps, perf_counter for the duration because
        # it is monotonic - a clock adjustment mid-scan cannot make the elapsed
        # time come out negative or jump.
        scan_started = time.perf_counter()
        # Phase marks, pre-seeded so a scan that dies mid-flight still prints a
        # breakdown (its unreached phases read 0.00).
        t_source = t_tree = t_shot = t_annotate = t_encode = scan_started
        print(f"[scan] started at {time.strftime('%H:%M:%S')}")
        # A failed scan must not hand the caller the PREVIOUS scan's image:
        # this is only ever reassigned on success, so without the reset the
        # agent would be shown a stale frame and told it was current.
        self.image_base64 = None

        # Fire the screenshot BEFORE /source, not after it. /source is a
        # multi-second XCTest snapshot (linear in element count, ~10ms each)
        # and the screenshot needs nothing from it. WDA serves both from one
        # queue, so the requests do NOT run in parallel - what overlaps is the
        # ~0.20s of decode+resize, which lands off the critical path either
        # way. Issuing it first is what makes today's sequential timing the
        # floor rather than the target.
        shot = {}
        shot_thread = threading.Thread(target=self._fetch_screenshot,
                                       args=(shot,), daemon=True)
        shot_thread.start()
        try:
            # Get the page source from WDA
            response = requests.get(
                f"{wda_url}/source",
                params={"excluded_attributes": WDA_EXCLUDED_ATTRIBUTES},
                timeout=WDA_SOURCE_TIMEOUT)
            t_source = time.perf_counter()

            if response.status_code == 200:
                # Parse JSON and get XML
                xml_string = response.json()['value']
                root = ET.fromstring(xml_string)
                # SpringBoard (home screen, or a system alert hosting the root) is not an app.
                _app = (root.get('name') or '').strip()
                self.application_name = '' if _app.lower() == 'springboard' else _app
                
                # Get enabled element types
                enabled_types = [v['type'] for k, v in config['element_types'].items() if v['enabled']]
                
                # List to store found elements
                found_elements = []
                
                # Get the main window dimensions from the XML for reference.
                # Seeded first: with no visible window the scale-factor block
                # below used to dereference an unbound xml_width, and the
                # NameError was swallowed by the outer handler as a bare
                # "Error: name 'xml_width' is not defined" - losing the whole
                # scan. Zero routes that case into the intended fallback.
                xml_width = xml_height = 0
                # Taken from the root XCUIElementTypeApplication, NOT from a
                # window. The old [@visible='true'] window lookup cannot work
                # now that `visible` is no longer requested, and neither can
                # "the biggest window": a device reports windows in TWO
                # coordinate spaces at once — 390x844 logical points and
                # 1170x2532 raw pixels — so picking by area lands on the pixel
                # window, makes scale_x 3x too small and shrinks every
                # annotation box to a third of its element. The application
                # root is unambiguous and is the space every child's x/y is
                # expressed in.
                if root.get('width') and root.get('height'):
                    xml_width = int(float(root.get('width') or 0))
                    xml_height = int(float(root.get('height') or 0))
                # Provisional per scan; the screenshot refines orientation below.
                self.orientation = ""
                self.device_family = self._wda_device_family()
                if xml_width > 0 and xml_height > 0:
                    if not self.device_family:   # /status unreadable: shorter side (iPad >= 744pt)
                        self.device_family = "iPad" if min(xml_width, xml_height) >= 700 else "iPhone"
                    self.orientation = "landscape" if xml_width > xml_height else "portrait"

                # Screen box used for the visibility test. Falls back to the
                # same iPhone-12 logical size the scale factors below assume,
                # so both stay consistent when no window is reported.
                screen_w = float(xml_width) if xml_width > 0 else 390.0
                screen_h = float(xml_height) if xml_height > 0 else 844.0
                
                # Function to extract elements using XML tree hierarchy
                def extract_elements(element, current_depth=0, in_keyboard=False):
                    """
                    Extract elements recursively, preserving XML tree structure.
                    This correctly handles scrollviews where children may be off-screen or beyond visible bounds.
                    """
                    element_type = element.get('type', '')

                    # Keypad keys (the lock-screen passcode pad, PIN pads) come
                    # back as Key: they carry the Keyboard Key trait, so they
                    # never matched Button and the whole pad was missing from
                    # the tree. The system keyboard's letters are Key too, but
                    # they sit under a Keyboard element; those stay out (typing
                    # goes through `input`). Keys outside a keyboard are buttons.
                    if element_type == "XCUIElementTypeKey" and not in_keyboard:
                        element_type = "XCUIElementTypeButton"

                    # Track if we should increment depth for children
                    element_added = False
                    
                    # Check if this element should be included
                    if element_type in enabled_types:
                        # Filter "other" elements to only include MainTabBar or Tab items
                        should_add = True
                        if element_type == "XCUIElementTypeOther":
                            label = element.get('label', '').lower()
                            name = element.get('name', '').lower()
                            if "maintabbar" not in label and "maintabbar" not in name and "tab" not in label and "tab" not in name and "text size" not in label:
                                should_add = False
                        
                        # Check visibility if required and add element.
                        # Visibility is now derived from the element's own
                        # geometry rather than read from WDA's `visible`
                        # attribute, which we no longer ask it to compute -
                        # that one attribute was 82% of the request time.
                        rect = _element_rect(element)
                        if rect is None:
                            should_add = False

                        if should_add and (not config['only_visible']
                                           or _is_visible(rect, screen_w, screen_h)):
                            # Get element info with depth from XML tree
                            info = {
                                'type': element_type,
                                'label': element.get('label', ''),
                                'name': element.get('name', ''),
                                'value': element.get('value', ''),
                                'x': rect[0],
                                'y': rect[1],
                                'width': rect[2],
                                'height': rect[3],
                                'depth': current_depth
                            }
                            found_elements.append(info)
                            element_added = True
                    
                    # Always process children (whether element was added or not)
                    # Only increment depth if this element was actually added
                    next_depth = current_depth + 1 if element_added else current_depth
                    in_keyboard = in_keyboard or element_type == "XCUIElementTypeKeyboard"
                    for child in element:
                        extract_elements(child, next_depth, in_keyboard)
                
                # Start extraction from root using XML tree structure
                extract_elements(root, 0)
                
                # Create virtual tick marks for adjustable elements (like sliders)
                def create_virtual_ticks(element, num_ticks=5):
                    """Create virtual tick marks for adjustable elements like sliders"""
                    virtual_ticks = []
                    
                    # Check if this is an adjustable element (text size slider)
                    label = element['label'].lower()
                    if 'text size' in label or element['type'] == 'XCUIElementTypeSlider':
                        # Calculate tick positions along the width
                        x_start = element['x']
                        y_start = element['y']
                        width = element['width']
                        height = element['height']
                        
                        # Tick dimensions - small enough to fit inside
                        tick_width = 8.0
                        tick_height = min(height, 20.0)  # Don't exceed parent height
                        
                        # Divide into equal segments, keeping ticks inside bounds
                        # Account for tick width so they don't overflow
                        usable_width = width - tick_width
                        spacing = usable_width / (num_ticks - 1) if num_ticks > 1 else 0
                        
                        for i in range(num_ticks):
                            # Center ticks vertically within slider
                            tick_x = x_start + (i * spacing)
                            tick_y = y_start + (height - tick_height) / 2
                            
                            tick_info = {
                                'type': 'XCUIElementTypeOther',  # Keep as Other for consistency
                                'label': f'Text size tick {i+1}',
                                'name': f'tick_{i+1}',
                                'value': f'Position {i+1}',
                                'x': tick_x,
                                'y': tick_y,
                                'width': tick_width,
                                'height': tick_height,
                                'depth': element['depth'] + 1,  # One level deeper than parent
                                'is_virtual': True  # Mark as virtual
                            }
                            virtual_ticks.append(tick_info)
                    
                    return virtual_ticks
                
                # Insert virtual ticks after their parent elements
                expanded_elements = []
                for elem in found_elements:
                    expanded_elements.append(elem)
                    elem['is_virtual'] = False  # Mark real elements
                    
                    # Create and insert virtual ticks for adjustable elements
                    virtual_ticks = create_virtual_ticks(elem)
                    expanded_elements.extend(virtual_ticks)
                
                # Replace found_elements with expanded list
                found_elements = expanded_elements
                
                # Clear previous mappings
                self.elements_mapping = {}
                
                # Build mapping for controller
                for index, elem in enumerate(found_elements, 1):
                    label = elem['label'] or elem['name'] or 'no_label'
                    # Clean up long labels - replace newlines with spaces and truncate if too long
                    label = label.replace('\n', ' ').replace('\r', '')
                    if len(label) > 50:
                        label = label[:47] + "..."
                    
                    bounds = {
                        'x': int(elem['x']),
                        'y': int(elem['y']),
                        'width': int(elem['width']),
                        'height': int(elem['height'])
                    }
                    
                    elem_data = {
                        'type': elem['type'].split('XCUIElementType')[-1],
                        'name': label,
                        'number': index,
                        'depth': elem['depth'],
                        'bounds': bounds
                    }
                    
                    # Add value if it exists
                    if elem['value']:
                        elem_data['value'] = elem['value']
                    
                    # Store mapping for controller
                    self.elements_mapping[str(index)] = elem_data
                
                # Update controller with element mappings
                controller_service.update_elements(self.elements_mapping)
                
                # Build element tree text in memory
                element_lines = []
                for index, elem in enumerate(found_elements, 1):
                    label = elem['label'] or elem['name'] or 'no_label'
                    # Clean up long labels - replace newlines with spaces and truncate if too long
                    label = label.replace('\n', ' ').replace('\r', '')
                    if len(label) > 50:
                        label = label[:47] + "..."
                    
                    value = elem['value']
                    element_type = elem['type'].split('XCUIElementType')[-1].lower()
                    indent = "    " * elem['depth']  # 4 spaces per depth level
                    if value:
                        element_lines.append(f'{indent}[{index}]<element_name="{label}", type="{element_type}", value="{value}" />\n')
                    else:
                        element_lines.append(f'{indent}[{index}]<element_name="{label}", type="{element_type}" />\n')
                
                # Store in memory - header lines name the device and the app in front
                tree_body = ''.join(element_lines)
                self.element_tree_text = self._compose_tree_text(tree_body)
                # Send element tree to vault
                vault_service.update_element_tree(self.element_tree_text)
                t_tree = time.perf_counter()

                # ---- Join the screenshot worker ----
                # Everything above needed only the XML; everything below needs
                # both halves, because scale_x/scale_y fold the worker's img_*
                # with the XML's xml_*. That is the one ordering constraint in
                # the scan, and join() is also what publishes the worker's
                # Image and dict to this thread - no lock or queue needed.
                shot_thread.join(SCREENSHOT_JOIN_TIMEOUT)
                if shot_thread.is_alive():
                    # Bounded, never indefinite. The worker has its own read
                    # deadline, so still being alive here means a wedged
                    # socket: treat it as a failed screenshot and move on.
                    print("Failed to take screenshot: worker still running "
                          f"after {SCREENSHOT_JOIN_TIMEOUT}s")
                elif shot.get('error') is not None:
                    raise shot['error']
                elif shot.get('image') is None:
                    # WDA answers on a single queue. If it refused the request
                    # that overlapped the snapshot, retry inline - same code,
                    # this thread - so the overlap can never cost a scan its
                    # image. Costs 0.24s in the failure path only.
                    print("Screenshot failed while overlapped "
                          f"({shot.get('status')}), retrying inline")
                    shot = {}
                    self._fetch_screenshot(shot)
                    if shot.get('error') is not None:
                        raise shot['error']

                image = shot.get('image')
                # The plain, unannotated frame the production frontend sends.
                image_base64 = shot.get('raw_base64')
                t_shot = time.perf_counter()

                if image is not None:
                    # Already downscaled and RGB-converted by the worker.
                    # Get actual image dimensions
                    img_width, img_height = image.size
                    # The screenshot is the whole screen (the root frame is not,
                    # in Split View) - it decides the orientation the model sees.
                    self.orientation = "landscape" if img_width > img_height else "portrait"
                    self.element_tree_text = self._compose_tree_text(tree_body)
                    
                    # Calculate scale factors
                    if xml_width > 0:
                        scale_x = img_width / xml_width
                        scale_y = img_height / xml_height
                    else:
                        # iPhone 12 fallback scaling
                        scale_x = img_width / 390
                        scale_y = img_height / 844

                    # Create drawing context
                    draw = ImageDraw.Draw(image)

                    # Cached across scans - parsing the TTF every scan was pure
                    # waste on the post-join critical path.
                    font, stroke_w = _load_annotate_font()

                    # Draw magenta boxes on detected elements with scaled coordinates (same color as macOS tree)
                    for i, elem in enumerate(found_elements, 1):
                        # Apply scale factors to coordinates
                        x = elem['x'] * scale_x
                        y = elem['y'] * scale_y
                        width = elem['width'] * scale_x
                        height = elem['height'] * scale_y
                        
                        # Round to integers for drawing
                        x, y, width, height = int(x), int(y), int(width), int(height)
                        
                        # Skip elements that are too small or have invalid dimensions
                        if width <= 0 or height <= 0:
                            continue
                        
                        # Draw magenta rectangle (box outline) with thicker lines
                        for offset in range(3):
                            draw.rectangle(
                                [x - offset, y - offset, x + width + offset, y + height + offset],
                                outline=(255, 0, 255),
                                width=1
                            )
                        
                        # Add element number. Measure WITH the stroke - it widens
                        # the glyphs, and the centring below has to account for it.
                        text = f"[{i}]"
                        bbox = draw.textbbox((0, 0), text, font=font,
                                             stroke_width=stroke_w)
                        tw = bbox[2] - bbox[0]
                        th = bbox[3] - bbox[1]

                        # Center the text inside the box at the top (original position)
                        text_x = x + (width // 2) - (tw // 2)  # Center horizontally
                        text_y = y + 5  # Position inside the box, 5 pixels from top

                        # A label is now wider than the narrow elements it sits
                        # on, so centring can push it off-canvas at the screen
                        # edges - where the digits would be clipped, not just
                        # overhanging. Keep the whole label on the image.
                        text_x = max(0, min(text_x, img_width - tw))

                        # Thin dark rim around the glyph shapes - NOT the filled
                        # white chip this replaced, which covered the UI beneath
                        # it. Supplies the luma contrast magenta lacks on its own.
                        draw.text((text_x, text_y), text, fill=(255, 0, 255), font=font,
                                  stroke_width=stroke_w, stroke_fill=LABEL_STROKE_COLOR)

                    t_annotate = time.perf_counter()

                    # Single encode - these exact bytes are the LLM payload.
                    # 4:4:4 chroma (subsampling=0) keeps the annotation digits
                    # crisp; see the LLM_IMAGE_* constants for why JPEG
                    # replaced lossless PNG here.
                    buffered = io.BytesIO()
                    image.save(buffered, format=LLM_IMAGE_FORMAT,
                               quality=LLM_IMAGE_QUALITY,
                               subsampling=LLM_IMAGE_SUBSAMPLING)
                    annotated_image_bytes = buffered.getvalue()
                    self.image_base64 = base64.b64encode(annotated_image_bytes).decode('utf-8')
                    t_encode = time.perf_counter()

                    # Save to debug folder ONLY if DEBUG is enabled
                    if DEBUG:
                        os.makedirs("debug/element", exist_ok=True)
                        os.makedirs("debug/screenshot", exist_ok=True)
                        timestamp = int(time.time())
                        debug_element_file = f"debug/element/ui_elements_{timestamp}.txt"
                        debug_screenshot_file = f"debug/screenshot/ui_elements_screenshot_{timestamp}.jpg"

                        # Save element tree to debug folder
                        with open(debug_element_file, 'w', encoding='utf-8') as f:
                            f.write(self.element_tree_text)

                        # Save annotated screenshot to debug folder - the SAME
                        # bytes that go to the LLM, so the dump is byte-identical
                        # to the payload and costs no second encode.
                        with open(debug_screenshot_file, "wb") as f:
                            f.write(annotated_image_bytes)

                    # Send to frontend
                    if FRONTEND and self.frontend_callback:
                        if DEBUG:
                            # Send annotated screenshot for debugging
                            self.frontend_callback(self.image_base64)
                        else:
                            # Send plain screenshot for production frontend
                            self.frontend_callback(image_base64)

                else:
                    print(f"Failed to take screenshot: {shot.get('status')}")

            else:
                print(f"Failed to get source: {response.status_code}")

        except Exception as e:
            print(f"Error: {e}")
        finally:
            # Never let the worker outlive the scan. No-op on the success path,
            # which already joined; on the failure path this is what stops a
            # screenshot thread running on into the NEXT scan and racing a
            # second request at WDA.
            if shot_thread.is_alive():
                shot_thread.join(SCREENSHOT_JOIN_TIMEOUT)
            # In finally, not at the end of the try: a scan that dies on a WDA
            # timeout is exactly the one whose duration is worth seeing, and
            # the except above already says it failed.
            #
            # shot= is measured INSIDE the worker and marked (bg) because it
            # runs off the critical path and does not sum with the rest;
            # wait= is what the join actually cost this thread. Together they
            # say whether WDA overlapped the two requests or serialised them.
            print(f"[scan] finished at {time.strftime('%H:%M:%S')} - "
                  f"source={_phase(scan_started, t_source):.2f}s "
                  f"tree={_phase(t_source, t_tree):.2f}s "
                  f"shot={shot.get('elapsed', 0.0):.2f}s(bg) "
                  f"wait={_phase(t_tree, t_shot):.2f}s "
                  f"annotate={_phase(t_shot, t_annotate):.2f}s "
                  f"encode={_phase(t_annotate, t_encode):.2f}s "
                  f"total={time.perf_counter() - scan_started:.2f}s")
    
    def get_scan_data(self):
        """Get scan data for AgentService - returns element tree text and base64 image from memory"""
        return self.element_tree_text, self.image_base64

# For compatibility
ELEMENT_CONFIG = {}

if __name__ == "__main__":
    scanner = UIElementScanner()
    scanner.scan_elements()