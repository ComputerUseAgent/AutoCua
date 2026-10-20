"""
OCR Detection — Linux PP-OCRv6 scanner (ONNX Runtime).

Mirrors the macOS (Apple Vision, mac/tree/ocr.py) and Windows (WinRT,
windows/tree/ocr_detection.py) scanners: it captures nothing itself — it OCRs
the PRE-CAPTURED screenshot, so it shares the exact frame the AT-SPI scan and
the annotation use — and hands back a raw line list. Filtering and merging
against the element tree happen in the caller (element.py).

Linux has no native OCR, so this runs PaddlePaddle's PP-OCRv6 (Apache-2.0)
through onnxruntime. Two models, used very unequally:

    detection    PP-OCRv6_small_det   10 MB   DB text detector. Once per scan
                 over the whole frame, ~220 ms on 4 CPU cores at 2048x1120
                 (tiny: 120 ms, medium: 2 s). 13px UI text needs the full
                 resolution, so the frame is only shrunk past DET_MAX_SIDE.
    recognition  PP-OCRv6_small_rec   21 MB   CTC text reader, ~9 ms a line
                 (tiny: 2 ms, medium: 37 ms; small reads like medium).
                 NOT run over every detection: element.py first drops the
                 boxes that sit on an element the tree already named and asks
                 for text only where the tree had nothing (recognize()), so
                 the cost scales with what AT-SPI missed, not with the screen.

The tiers share the file layout and drop in by name (DET_MODEL / REC_MODEL).
Models live in models/<name>/ beside this file — inference.onnx plus
inference.yml, which carries the recognizer's character dictionary. They are
NOT in git (tens of MB): AutoCua/utils/ocr_models.py fetches a missing model
from Hugging Face on first use, pinned and checksummed, the way utils/wda.py
fetches WebDriverAgent for iOS.

Post-processing is numpy only (no OpenCV, no pyclipper): the DB probability
map is thresholded, split into connected components by run-length union-find,
and each component's bounding box is grown by DB's unclip distance. UI text is
axis-aligned, so axis-aligned boxes lose nothing.

Results are in LOGICAL (AT-SPI) pixels with a top-left origin, offset by the
screen origin, so they live in the same coordinate space as element rects.
"""

import os
import re
import sys
import glob
import math
import time
import threading

from PIL import Image

# The model fetcher lives with the other one-time setup steps. This file also
# runs standalone (python3 element.py ...), where the package is not on the
# path — put the project root there before giving up.
try:
    from AutoCua.utils import ocr_models as _models
except ImportError:
    sys.path.append(os.path.abspath(os.path.join(
        os.path.dirname(__file__), "..", "..", "..")))
    from AutoCua.utils import ocr_models as _models

# ---------------------------------------------------------------- settings

# Model tiers, measured on a 4-core VM at 2047x1128 (HANDOFF §3.17):
#   detector    tiny 118 ms / small 219 ms / medium 1970 ms per frame
#   recognizer  tiny 2.1 ms / small 8.8 ms / medium 36.5 ms per line, on a
#               realistic mix of UI labels; small and medium read alike and
#               both fix tiny's misreads ("Sep9", "[20z]", stray glyphs)
# so both are "small": medium's detector is 10x the cost for slightly
# cleaner boxes, and medium's recognizer 4x the cost for the same reads —
# which matters on a window OCR carries alone (App Center: ~200 lines).
# All three tiers share the file layout; swap by name.
DET_MODEL = "PP-OCRv6_small_det_onnx"
REC_MODEL = "PP-OCRv6_small_rec_onnx"

MODELS_DIR = str(_models.MODELS_DIR)     # AutoCua/linux/tree/models, gitignored

# Detection. The frame is fed at native resolution up to this long side —
# screen text is small and the tiny detector is cheap enough (see above).
DET_MAX_SIDE = 2048
# DBPostProcess parameters. Each tier ships its own in inference.yml
# (tiny 0.2/0.4/1.4, small and medium 0.45/0.45/1.4 — measured); these are
# the fallbacks when the yml does not carry a value.
DET_THRESH = 0.2          # probability-map binarisation
DET_BOX_THRESH = 0.4      # mean probability a box must reach
DET_UNCLIP_RATIO = 1.4    # how far to grow the (shrunk) text core
DET_MIN_SIDE_PX = 3       # capture pixels; anything smaller is a speck

# Recognition (PP-OCR rec: 48px-high strips, batched by aspect ratio).
REC_HEIGHT = 48
REC_BASE_WIDTH = 320
REC_BATCH = 8             # measured: 8 beats 16 by 10% (less right-padding per batch)
REC_PAD_PX = 2            # capture pixels of margin around a crop

# Drop low-confidence reads and stray single-character / blank lines that add
# noise without being actionable (same floors as the macOS scanner).
MIN_CONFIDENCE = 0.5
MIN_TEXT_LEN = 2

_IMAGENET_MEAN = (0.485, 0.456, 0.406)
_IMAGENET_STD = (0.229, 0.224, 0.225)

# ---------------------------------------------------------------- runtime

_np = None
_ort = None
_UNAVAILABLE = None          # str reason once something is known to be missing
_sessions = {}
_alphabets = {}
_det_params = {}
_lock = threading.Lock()


def _add_venv_site_packages():
    """Put the project venv's site-packages on sys.path. True if one was found.

    The mirror image of element.py's _add_system_gi_to_path: that one lets
    the venv see the distro's PyGObject, this one lets the distro's python
    see the venv's onnxruntime. `python3 -m AutoCua.linux.tree.test` is the
    documented way to run the scanner and it uses the system interpreter,
    while pip puts onnxruntime into the project's .venv — nobody should have
    to remember which interpreter has which half.

    Only a venv of the SAME python version is accepted (the compiled
    extension carries the interpreter's ABI tag), and the path is APPENDED,
    so nothing already importable is shadowed."""
    root = os.path.abspath(os.path.join(os.path.dirname(__file__),
                                        "..", "..", ".."))
    ver = f"python{sys.version_info.major}.{sys.version_info.minor}"
    tag = f"cpython-{sys.version_info.major}{sys.version_info.minor}"
    for venv in (".venv", "venv", "env"):
        sp = os.path.join(root, venv, "lib", ver, "site-packages")
        if sp in sys.path:
            continue
        if glob.glob(os.path.join(sp, "onnxruntime", "capi", f"*{tag}-*.so")):
            sys.path.append(sp)
            return True
    return False


def ocr_unavailable_reason():
    """None when OCR can run, else one line saying why — the missing pip
    package or the model that could not be fetched. Cheap after the first
    call; element.py asks once per scan and prints the answer once."""
    global _np, _ort, _UNAVAILABLE
    if _UNAVAILABLE is not None:
        return _UNAVAILABLE
    if _np is None or _ort is None:
        try:
            try:
                import numpy
                import onnxruntime
            except ImportError:
                if not _add_venv_site_packages():
                    raise
                import numpy
                import onnxruntime
        except ImportError as e:
            _UNAVAILABLE = (f"{e.name or e} is not installed — scanning "
                            f"without OCR (pip install onnxruntime numpy)")
            return _UNAVAILABLE
        _np, _ort = numpy, onnxruntime
    if not _models.ensure_ocr_models((DET_MODEL, REC_MODEL)):
        _UNAVAILABLE = ("the PP-OCRv6 models are missing and could not be "
                        "fetched (see above) — scanning without OCR; "
                        "python3 -m AutoCua.utils.ocr_models fetches them")
        return _UNAVAILABLE
    return None


def _session(name):
    """One onnxruntime session per model per process. Sessions are
    thread-safe for run(), so the detection thread and the main thread's
    recognition share them freely."""
    with _lock:
        s = _sessions.get(name)
        if s is None:
            opts = _ort.SessionOptions()
            # 6 = this machine's physical cores; measured 12% faster than 4
            # and slower again at 12 (SMT siblings fight over the same units).
            opts.intra_op_num_threads = max(1, min(6, os.cpu_count() or 1))
            opts.log_severity_level = 3
            s = _ort.InferenceSession(
                os.path.join(MODELS_DIR, name, "inference.onnx"), opts,
                providers=["CPUExecutionProvider"])
            _sessions[name] = s
        return s


def _det_postprocess(name):
    """(thresh, box_thresh, unclip_ratio) for a detector, from the
    PostProcess block of its inference.yml, falling back to the constants."""
    with _lock:
        p = _det_params.get(name)
        if p is not None:
            return p
        vals = {"thresh": DET_THRESH, "box_thresh": DET_BOX_THRESH,
                "unclip_ratio": DET_UNCLIP_RATIO}
        try:
            text = open(os.path.join(MODELS_DIR, name, "inference.yml"),
                        encoding="utf-8").read()
            block = text[text.index("PostProcess:"):]
            for key in vals:
                m = re.search(rf"^\s+{key}: ([0-9.]+)\s*$", block, re.M)
                if m:
                    vals[key] = float(m.group(1))
        except Exception:
            pass
        p = (vals["thresh"], vals["box_thresh"], vals["unclip_ratio"])
        _det_params[name] = p
        return p


def _alphabet(name):
    """CTC alphabet of a recognizer: blank, the yml's character_dict, space.
    Read straight from the model's inference.yml — a two-quote-style YAML
    list, parsed here rather than pulling PyYAML in for one file."""
    with _lock:
        a = _alphabets.get(name)
        if a is not None:
            return a
        text = open(os.path.join(MODELS_DIR, name, "inference.yml"),
                    encoding="utf-8").read()
        chars = []
        for line in text[text.index("character_dict:"):].splitlines()[1:]:
            m = re.match(r"^  - (.*)$", line)
            if not m:
                if chars:
                    break
                continue
            v = m.group(1)
            if len(v) >= 2 and v[0] == v[-1] == "'":
                v = v[1:-1].replace("''", "'")
            elif len(v) >= 2 and v[0] == v[-1] == '"':
                v = bytes(v[1:-1], "utf-8").decode("unicode_escape")
            chars.append(v)
        a = ["<blank>"] + chars + [" "]
        _alphabets[name] = a
        return a


# ---------------------------------------------------------------- detection

def _det_input(rgb):
    """(tensor, (sx, sy)) — the frame as the detector wants it: BGR, ImageNet
    normalised, CHW, sides a multiple of 32, no larger than DET_MAX_SIDE.
    sx/sy map capture pixels to map pixels."""
    np = _np
    w, h = rgb.size
    r = min(1.0, DET_MAX_SIDE / max(w, h))
    nw = max(32, int(round(w * r / 32)) * 32)
    nh = max(32, int(round(h * r / 32)) * 32)
    im = rgb if (nw, nh) == (w, h) else rgb.resize(
        (nw, nh), Image.Resampling.BILINEAR)
    a = np.asarray(im, dtype=np.float32)[:, :, ::-1] / 255.0   # RGB -> BGR
    a = (a - np.asarray(_IMAGENET_MEAN, np.float32)) \
        / np.asarray(_IMAGENET_STD, np.float32)
    return np.ascontiguousarray(a.transpose(2, 0, 1)[None]), (nw / w, nh / h)


def _components(binary):
    """Bounding boxes of the 8-connected regions of a boolean map, as an
    (n, 5) int array of x0, y0, x1, y1 (exclusive), pixel count.

    Runs of set pixels per row are found in numpy; runs on adjacent rows that
    overlap horizontally are joined by union-find. The Python loop is over
    runs, not pixels — a dense 2048x1120 map with 250 text lines is ~10k runs
    and ~12 ms."""
    np = _np
    h, w = binary.shape
    padded = np.zeros((h, w + 2), dtype=np.int8)
    padded[:, 1:-1] = binary
    d = np.diff(padded, axis=1)
    rows, starts = np.nonzero(d == 1)
    _, ends = np.nonzero(d == -1)          # exclusive
    n = len(rows)
    if n == 0:
        return np.zeros((0, 5), dtype=np.int64)

    parent = np.arange(n)

    def find(i):
        while parent[i] != i:
            parent[i] = parent[parent[i]]
            i = parent[i]
        return i

    row_first = np.searchsorted(rows, np.arange(h + 1))
    for r in range(1, h):
        a0, a1 = row_first[r - 1], row_first[r]
        b0, b1 = row_first[r], row_first[r + 1]
        if a0 == a1 or b0 == b1:
            continue
        ps, pe = starts[a0:a1], ends[a0:a1]
        for i in range(b0, b1):
            # 8-connectivity: touching corners count, hence the +-1.
            lo = np.searchsorted(pe, starts[i] - 1, side="right")
            hi = np.searchsorted(ps, ends[i] + 1, side="left")
            for j in range(a0 + lo, a0 + hi):
                ri, rj = find(i), find(j)
                if ri != rj:
                    parent[ri] = rj

    labels = np.fromiter((find(i) for i in range(n)), dtype=np.int64, count=n)
    _uniq, inv = np.unique(labels, return_inverse=True)
    k = int(inv.max()) + 1
    x0 = np.full(k, w + 1, dtype=np.int64)
    x1 = np.zeros(k, dtype=np.int64)
    y0 = np.full(k, h + 1, dtype=np.int64)
    y1 = np.zeros(k, dtype=np.int64)
    area = np.zeros(k, dtype=np.int64)
    np.minimum.at(x0, inv, starts)
    np.maximum.at(x1, inv, ends)
    np.minimum.at(y0, inv, rows)
    np.maximum.at(y1, inv, rows + 1)
    np.add.at(area, inv, ends - starts)
    return np.stack([x0, y0, x1, y1, area], axis=1)


def _det_boxes(prob, map_w, map_h, params=None):
    """DB post-processing on the probability map -> [(x0, y0, x1, y1, score)]
    in MAP pixels. Each text core (the map is trained on text shrunk to ~40%
    of its height) is grown by DB's unclip distance, area*ratio/perimeter,
    which for a w x h rectangle is the margin that restores the glyph box."""
    thresh, box_thresh, unclip = params or (DET_THRESH, DET_BOX_THRESH,
                                            DET_UNCLIP_RATIO)
    out = []
    for x0, y0, x1, y1, _area in _components(prob > thresh):
        w, h = int(x1 - x0), int(y1 - y0)
        if w < 2 or h < 2:
            continue
        score = float(prob[y0:y1, x0:x1].mean())
        if score < box_thresh:
            continue
        grow = (w * h * unclip) / (2.0 * (w + h))
        out.append((max(0.0, x0 - grow), max(0.0, y0 - grow),
                    min(float(map_w), x1 + grow), min(float(map_h), y1 + grow),
                    score))
    return out


# ---------------------------------------------------------------- recognition

def _rec_batch(session, alphabet, crops):
    """Read a batch of RGB line crops. Returns [(text, confidence)].

    PP-OCR rec preprocessing: every strip is resized to 48px high keeping its
    aspect ratio, the batch is padded on the right to the widest strip (never
    narrower than 320), pixels are BGR scaled to [-1, 1]. The output is a
    per-timestep softmax over the alphabet; CTC decoding collapses repeats
    and drops blanks, and the confidence is the mean of the kept steps."""
    np = _np
    ratios = [c.width / max(1, c.height) for c in crops]
    img_w = int(math.ceil(REC_HEIGHT * max(REC_BASE_WIDTH / REC_HEIGHT,
                                           max(ratios))))
    batch = np.zeros((len(crops), 3, REC_HEIGHT, img_w), dtype=np.float32)
    for i, c in enumerate(crops):
        rw = min(img_w, max(1, int(math.ceil(REC_HEIGHT * ratios[i]))))
        strip = c.resize((rw, REC_HEIGHT), Image.Resampling.BICUBIC)
        a = np.asarray(strip, dtype=np.float32)[:, :, ::-1] / 255.0
        batch[i, :, :, :rw] = ((a - 0.5) / 0.5).transpose(2, 0, 1)
    probs = session.run(None, {session.get_inputs()[0].name: batch})[0]
    results = []
    for p in probs:
        idx = p.argmax(axis=1)
        conf = p.max(axis=1)
        keep = np.ones(len(idx), dtype=bool)
        keep[1:] = idx[1:] != idx[:-1]
        keep &= idx != 0
        text = "".join(alphabet[i] for i in idx[keep])
        results.append((text.strip(),
                        float(conf[keep].mean()) if keep.any() else 0.0))
    return results


# ---------------------------------------------------------------- scanner

class OCRScanner:
    """Run PP-OCRv6 over a pre-captured screen image.

    scan() detects text boxes and stores them in self.lines as
        [{text, left, top, right, bottom, score, confidence}, ...]
    in LOGICAL pixels, text empty until recognize() has read them.
    recognize(lines) reads the given lines in place and returns the ones
    that pass the confidence and length floors.

    Zero-arg scan() so it can be a threading.Thread target — onnxruntime
    releases the GIL while it computes, so detection genuinely overlaps the
    AT-SPI walk on the main thread."""

    def __init__(self, image, scale, screen_origin, read=True):
        self._image = image                  # PIL screenshot (capture PIXELS)
        self._scale = scale or 1.0           # capture px per logical px
        self._ox, self._oy = screen_origin   # screen["x"], screen["y"]
        self._rgb = None
        self._read = read                    # recognise after detecting
        self._keep = None                    # the tree's verdict, see set_keep
        self._keep_lock = threading.Lock()
        self.det_done = threading.Event()    # set once self.lines is final
        self.lines = []
        self.stats = {"detected": 0, "recognized": 0,
                      "det_ms": 0.0, "rec_ms": 0.0}

    def scan(self):
        """Thread target. Detect, publish the boxes (det_done), then read
        them straight away until the tree says which ones matter
        (set_keep). Recognition used to wait for the whole walk and then
        run on the main thread — 0.8 to 2.8 s of serial time; now it
        overlaps the walk, and a box read before the verdict arrives costs
        a little CPU on a thread the walk was not using."""
        try:
            if self._image is not None:
                self.lines = self._detect()
        except Exception as e:
            print(f"OCR error: {e}")
            self.lines = []
        finally:
            self.det_done.set()
        if self._read and self.lines:
            try:
                self._read_lines(self.lines)
            except Exception as e:
                print(f"OCR error: {e}")

    def set_keep(self, keep):
        """`keep(line) -> bool`: the tree's verdict on each detected box.
        Boxes the reader has not reached yet are skipped when it says no;
        boxes it already read are simply not returned by recognize()."""
        with self._keep_lock:
            self._keep = keep

    def _crop(self, line):
        w, h = self._rgb.size
        px0, py0, px1, py1 = line["_px"]
        return self._rgb.crop((
            max(0, int(px0) - REC_PAD_PX), max(0, int(py0) - REC_PAD_PX),
            min(w, int(math.ceil(px1)) + REC_PAD_PX),
            min(h, int(math.ceil(py1)) + REC_PAD_PX)))

    @staticmethod
    def _ratio(line):
        px0, py0, px1, py1 = line["_px"]
        return ((px1 - px0 + 2 * REC_PAD_PX)
                / max(1.0, py1 - py0 + 2 * REC_PAD_PX))

    def _read_lines(self, lines):
        """Read the unread lines of `lines` in place, batched by aspect
        ratio so the right-padding wasted on each batch stays small, and
        drop pending boxes the verdict rejects as soon as it is in."""
        pending = sorted((l for l in lines if l.get("confidence") is None),
                         key=self._ratio)
        if not pending:
            return
        t0 = time.time()
        session = _session(REC_MODEL)
        alphabet = _alphabet(REC_MODEL)
        while pending:
            with self._keep_lock:
                keep = self._keep
            if keep is not None:
                pending = [l for l in pending if keep(l)]
                if not pending:
                    break
            batch, pending = pending[:REC_BATCH], pending[REC_BATCH:]
            try:
                reads = _rec_batch(session, alphabet,
                                   [self._crop(l) for l in batch])
            except Exception as e:
                print(f"OCR error: {e}")
                break
            for line, (text, conf) in zip(batch, reads):
                line["text"] = text
                line["confidence"] = conf
        self.stats["rec_ms"] += (time.time() - t0) * 1000.0

    def _detect(self):
        np = _np
        t0 = time.time()
        self._rgb = self._image.convert("RGB")
        session = _session(DET_MODEL)
        x, (sx, sy) = _det_input(self._rgb)
        prob = session.run(None, {session.get_inputs()[0].name: x})[0][0, 0]
        map_h, map_w = prob.shape
        s = self._scale
        out = []
        params = _det_postprocess(DET_MODEL)
        for x0, y0, x1, y1, score in _det_boxes(prob, map_w, map_h, params):
            # map px -> capture px -> logical px (+ screen origin)
            px0, px1 = x0 / sx, x1 / sx
            py0, py1 = y0 / sy, y1 / sy
            if px1 - px0 < DET_MIN_SIDE_PX or py1 - py0 < DET_MIN_SIDE_PX:
                continue
            out.append({
                "text": "",
                "left": int(round(self._ox + px0 / s)),
                "top": int(round(self._oy + py0 / s)),
                "right": int(round(self._ox + px1 / s)),
                "bottom": int(round(self._oy + py1 / s)),
                "score": score,
                "confidence": None,
                "_px": (px0, py0, px1, py1),
            })
        out.sort(key=lambda l: (l["top"], l["left"]))
        self.stats["detected"] = len(out)
        self.stats["det_ms"] = (time.time() - t0) * 1000.0
        return out

    def recognize(self, lines):
        """The lines of `lines` that pass MIN_CONFIDENCE / MIN_TEXT_LEN,
        after reading whatever the worker had not reached (normally
        nothing: the verdict reaches it before it runs dry)."""
        if not lines or self._rgb is None:
            return []
        self._read_lines(lines)
        kept = [l for l in lines
                if l.get("confidence") is not None
                and l["confidence"] >= MIN_CONFIDENCE
                and len(l["text"]) >= MIN_TEXT_LEN]
        kept.sort(key=lambda l: (l["top"], l["left"]))
        self.stats["recognized"] = len(kept)
        return kept

    def get_lines(self):
        """Raw detection list in logical pixels (text filled in only for
        lines that went through recognize())."""
        return self.lines
