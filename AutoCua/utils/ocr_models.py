"""Fetch the PP-OCRv6 models if they aren't already here.

The Linux scanner's OCR (AutoCua/linux/tree/ocr.py) runs PaddlePaddle's
PP-OCRv6 through onnxruntime: one text detector and one text recognizer, ONNX
files of 10 and 21 MB for the tier in use, 1.7 to 77 MB across the tiers.
They are Apache-2.0 and could legally be vendored; they are not, because a
weight file pushed to the repository sits in its history for good, and a few
re-exports would fill the project's storage. So they live under
AutoCua/linux/tree/models/ (gitignored) and are fetched from the author's
own Hugging Face repositories the first time a scan needs them — exactly what
wda.py does for WebDriverAgent on iOS. See THIRD_PARTY_NOTICES.md.

Every file is pinned three ways: the upstream commit it is fetched from, its
byte size, and its SHA-256, checked before the file is renamed into place. A
partial or altered download is never visible under the real name.

Call ensure_ocr_models() from wherever the models are about to be needed — it
is a cheap size check once they exist, and it is safe to call from several
threads and several processes at once. To fetch ahead of time:

    python3 -m AutoCua.utils.ocr_models            # the default tier
    python3 -m AutoCua.utils.ocr_models medium     # tiny / small / medium / all
"""

import os
import sys
import hashlib
import threading
import urllib.request
from pathlib import Path
from typing import NamedTuple


class Model(NamedTuple):
    commit: str      # Hugging Face commit the files are fetched from
    files: dict      # file name -> (bytes, sha256)


# Pinned upstream state of every model this project knows how to run. Keep
# in step with THIRD_PARTY_NOTICES.md. Recorded 2026-09-09 from the files as
# downloaded; a re-pin means updating commit, size AND digest together.
REGISTRY = {
    "PP-OCRv6_tiny_det_onnx": Model(
        commit="2ba1506c0380b8f0b03dd142459aac66d4421f6c",
        files={
            "inference.onnx": (1780590, "193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8"),
            "inference.yml": (883, "3ac018be6f97499a08faa3bbdeb33640968d9307f6736d152902747a9f259593"),
        }),
    "PP-OCRv6_tiny_rec_onnx": Model(
        commit="2612ab37152ae0a677521bae4e1e3d4fb4cf7c30",
        files={
            "inference.onnx": (4462639, "9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6"),
            "inference.yml": (55571, "66170210bad538e83fff3c4a3867e547d6bf20b50d64b20347c4b913f3034ea1"),
        }),
    "PP-OCRv6_small_det_onnx": Model(
        commit="28fe5895c24fd108c19eb3e8479f4ab385fbfc62",
        files={
            "inference.onnx": (9880512, "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e"),
            "inference.yml": (885, "193f435274bf9f0b5f71a929bbfbcf148282df7e633b34e7c373e8f44741b516"),
        }),
    "PP-OCRv6_small_rec_onnx": Model(
        commit="b8f84f0b80c529de40b4fbb3544b84fa7233a513",
        files={
            "inference.onnx": (21159378, "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"),
            "inference.yml": (150579, "ab078671bb49f06228eadccd34f1bb501e157f7a047095ffb943ba81512c77d1"),
        }),
    "PP-OCRv6_medium_det_onnx": Model(
        commit="61323801669c338b7891481ec7bac61ce31b576a",
        files={
            "inference.onnx": (62032837, "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"),
            "inference.yml": (886, "7298d5ead546584af2504d03355f881ac7a7bc0eb1e282d3e159277c1d0af871"),
        }),
    "PP-OCRv6_medium_rec_onnx": Model(
        commit="50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
        files={
            "inference.onnx": (76554979, "9c09abf0957f7968c7586464b7397b84ad2387a0497a351af40e9acc71b673ba"),
            "inference.yml": (150580, "991b700facf5b50a7de193468207d5f4255b538dde0d312ae3b7c7a9b6873129"),
        }),
}

# The detector + recognizer pair of each tier. ocr.py names the pair it runs
# (DET_MODEL / REC_MODEL); "small" is what it ships with — see HANDOFF §3.17
# for the measurements behind that.
TIERS = {
    "tiny": ("PP-OCRv6_tiny_det_onnx", "PP-OCRv6_tiny_rec_onnx"),
    "small": ("PP-OCRv6_small_det_onnx", "PP-OCRv6_small_rec_onnx"),
    "medium": ("PP-OCRv6_medium_det_onnx", "PP-OCRv6_medium_rec_onnx"),
}
DEFAULT_TIER = "small"

# Where ocr.py looks, beside itself. Gitignored.
MODELS_DIR = Path(__file__).resolve().parent.parent / "linux" / "tree" / "models"

# resolve/<commit> rather than resolve/main: the file at a commit never
# changes, so the digest below stays meaningful.
URL = "https://huggingface.co/PaddlePaddle/{name}/resolve/{commit}/{file}"

# One fetch per process, however many threads ask at once.
_LOCK = threading.Lock()


def _present(name: str) -> bool:
    """True when every pinned file of `name` is on disk at its pinned size.
    Size only — a digest of 21 MB on every scan is not worth it, and the
    digest was checked when the file was written."""
    model = REGISTRY[name]
    d = MODELS_DIR / name
    for fname, (size, _sha) in model.files.items():
        p = d / fname
        try:
            if p.stat().st_size != size:
                return False
        except OSError:
            return False
    return True


def _describe(name: str) -> str:
    tier = name.split("_")[1]
    kind = "text detector" if "_det_" in name else "text recognizer"
    mb = REGISTRY[name].files["inference.onnx"][0] / 1e6
    return f"PP-OCRv6 {tier} {kind} ({mb:.1f} MB)"


def _fetch_file(name: str, fname: str, size: int, sha: str) -> bool:
    """Download one pinned file into place. False, with the reason printed,
    on any failure — nothing half-written is ever left under the real name."""
    model = REGISTRY[name]
    d = MODELS_DIR / name
    d.mkdir(parents=True, exist_ok=True)
    final = d / fname
    staging = d / f".{fname}.incoming.{os.getpid()}"
    url = URL.format(name=name, commit=model.commit, file=fname)
    digest = hashlib.sha256()
    got = 0
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "AutoCua"})
        # The per-read timeout bounds a network that stalls; a healthy one
        # moves 21 MB in a few seconds.
        with urllib.request.urlopen(req, timeout=60) as r, open(staging, "wb") as out:
            while True:
                chunk = r.read(1 << 20)
                if not chunk:
                    break
                out.write(chunk)
                digest.update(chunk)
                got += len(chunk)
    except Exception as e:
        _discard(staging)
        print(f"🔤 Could not fetch {fname} for {name}: {e}")
        return False
    if got != size or digest.hexdigest() != sha:
        _discard(staging)
        print(f"🔤 {name}/{fname} did not match its pin "
              f"({got} bytes, sha256 {digest.hexdigest()[:12]}…; expected "
              f"{size} bytes, {sha[:12]}…) — discarded.")
        return False
    try:
        os.replace(staging, final)
    except OSError as e:
        _discard(staging)
        print(f"🔤 Could not write {final}: {e}")
        return False
    return True


def _discard(path: Path) -> None:
    try:
        path.unlink()
    except OSError:
        pass


def ensure_ocr_models(names=TIERS[DEFAULT_TIER]) -> bool:
    """True when every model in `names` is on disk. Fetches the missing ones
    once, verified against their pins. Prints why when it cannot."""
    unknown = [n for n in names if n not in REGISTRY]
    if unknown:
        print(f"🔤 Unknown OCR model(s) {unknown} — known: {sorted(REGISTRY)}")
        return False
    if all(_present(n) for n in names):
        return True

    with _LOCK:
        for name in names:
            # Another thread may have finished while we waited on the lock,
            # and another PROCESS may have written the files meanwhile —
            # _present re-checks the disk, so both cases are one size read.
            if _present(name):
                continue
            print(f"🔤 Fetching {_describe(name)} — one time, a few seconds...",
                  flush=True)
            for fname, (size, sha) in REGISTRY[name].files.items():
                p = MODELS_DIR / name / fname
                if p.is_file() and p.stat().st_size == size:
                    continue
                if not _fetch_file(name, fname, size, sha):
                    print(f"🔤 Fetch it by hand: python3 -m AutoCua.utils.ocr_models "
                          f"— or download {URL.format(name=name, commit=REGISTRY[name].commit, file=fname)} "
                          f"to {p}")
                    return False
            print(f"🔤 {name} ready at {MODELS_DIR / name}")
    return all(_present(n) for n in names)


def main(argv=None) -> int:
    """`python3 -m AutoCua.utils.ocr_models [tiny|small|medium|all]`."""
    argv = sys.argv[1:] if argv is None else list(argv)
    tier = argv[0].lower() if argv else DEFAULT_TIER
    if tier == "all":
        names = tuple(REGISTRY)
    elif tier in TIERS:
        names = TIERS[tier]
    else:
        print(f"Usage: python3 -m AutoCua.utils.ocr_models "
              f"[{'|'.join(TIERS)}|all]   (default: {DEFAULT_TIER})")
        return 2
    ok = ensure_ocr_models(names)
    for n in names:
        print(f"  {'ready  ' if _present(n) else 'MISSING'}  {n}  ({MODELS_DIR / n})")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
