#!/usr/bin/env python3
"""Fastest 1080p HEVC compression (macOS / Windows / Linux).

    python compress.py input.mp4            ->  input_1080p.mp4
    python compress.py input.mp4 out.mp4

Scales to 1080p and encodes with the GPU (VideoToolbox / NVENC / QuickSync / AMF / VAAPI); falls back to
libx265 on the CPU. Uses the system ffmpeg, then the one bundled in the imageio-ffmpeg pip package, whose Linux
build has no GPU encoders: on Linux the GPU needs the distro's ffmpeg (README, "Linux, from pip"). A GPU encoder
is used only once a test clip it encodes decodes back right, so one the build lacks, one without its GPU and one
that writes a broken picture are all skipped quietly.
"""
import glob
import os
import shutil
import subprocess
import sys
import tempfile
import time


def ffmpeg():
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    try:
        import imageio_ffmpeg
    except ImportError:
        # Never installed behind the person's back: imageio-ffmpeg is a
        # dependency (pyproject.toml and every requirements file), so this is
        # a broken install, and the fix is theirs to run.
        raise RuntimeError("no ffmpeg: put one on PATH, or install imageio-ffmpeg "
                           "(pip install imageio-ffmpeg)") from None
    return imageio_ffmpeg.get_ffmpeg_exe()


def bundled_ffmpeg():
    """imageio-ffmpeg's ffmpeg, or None. It always has libx265, which a distro ffmpeg may lack (Fedora's
    ffmpeg-free, openSUSE's own), so it backs the system one up."""
    try:
        import imageio_ffmpeg
        return imageio_ffmpeg.get_ffmpeg_exe()
    except Exception:
        return None


def vaapi_devices():
    """The render nodes of the AMD and Intel GPUs, the ones VA-API encodes on, each tried in turn: the first may
    have no HEVC encoder (a Radeon RX 6400/6500 has none) while the next does. Not a fixed renderD128: nodes are
    numbered in driver load order, and on hybrid laptops renderD128 is often the NVIDIA card."""
    nodes = []
    for node in sorted(glob.glob("/sys/class/drm/renderD*")):
        try:
            with open(node + "/device/vendor") as f:
                if f.read().strip() in ("0x1002", "0x8086"):  # AMD, Intel
                    nodes.append("/dev/dri/" + os.path.basename(node))
        except OSError:
            pass
    return nodes


# encoder -> (input options, filter chain, encoder options); tried in order, first one that works wins
Q = "26"  # CRF-style quality; VideoToolbox 55 is calibrated to match libx265 CRF 26 on screen content
SCALE = "scale=w='if(gt(iw,ih),-2,min(1080,iw))':h='if(gt(iw,ih),min(1080,ih),-2)':flags=bicubic"
ENCODERS = {
    "darwin": [("hevc_videotoolbox", [], SCALE + ",format=yuv420p", ["-q:v", "55", "-profile:v", "main"])],
    "win32": [("hevc_nvenc", [], SCALE + ",format=yuv420p", ["-preset", "p4", "-rc", "vbr", "-cq", Q, "-b:v", "0", "-spatial-aq", "1"]),
              ("hevc_qsv", [], SCALE + ",format=nv12", ["-preset", "faster", "-global_quality", Q]),
              ("hevc_amf", [], SCALE + ",format=nv12", ["-quality", "balanced", "-rc", "cqp", "-qp_i", Q, "-qp_p", Q])],
    "linux": [("hevc_nvenc", [], SCALE + ",format=yuv420p", ["-preset", "p4", "-rc", "vbr", "-cq", Q, "-b:v", "0", "-spatial-aq", "1"]),
              ("hevc_qsv", [], SCALE + ",format=nv12", ["-preset", "faster", "-global_quality", Q])] +
             [("hevc_vaapi", ["-init_hw_device", "vaapi=va:" + dev, "-filter_hw_device", "va"],
               SCALE + ",format=nv12,hwupload", ["-rc_mode", "CQP", "-qp", Q]) for dev in vaapi_devices()],
}
CPU = ("libx265", [], SCALE + ",format=yuv420p", ["-preset", "superfast", "-crf", Q, "-x265-params", "log-level=error"])
NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0  # no console window flashing up from the app on Windows
TESTED = set()  # (ffmpeg, encoder, input options) whose test clip came back right: tested once a process


def command(ff, inp, dst, enc, pre, vf, opts):
    """The encode: scale, HEVC, AAC audio, hvc1 mp4. compress() runs it on the recording, usable() on a test clip."""
    return ([ff, "-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-stats"] + pre + inp +
            ["-vf", vf, "-c:v", enc, "-g", "300"] + opts +  # keyframe every 10 s (30 fps)
            ["-c:a", "aac", "-b:a", "160k", "-tag:v", "hvc1", "-movflags", "+faststart", "-f", "mp4", dst])


def usable(ff, enc, pre, vf, opts):
    """Whether ff encodes right with enc on this machine: a short flat gray clip goes through command() and is
    decoded back, all output thrown away. That skips, without ffmpeg's errors after every recording, an encoder
    the build lacks, one whose GPU is missing, and one that writes a broken picture: Mesa's VAAPI HEVC on AMD
    writes its own SPS/PPS, which disagree with the hvc1 header ffmpeg stores, so every frame decodes as noise."""
    key = (ff, enc) + tuple(pre)
    if key not in TESTED:
        quiet = dict(stderr=subprocess.DEVNULL, creationflags=NO_WINDOW, timeout=30)
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
            out = os.path.join(tmp, "test.mp4")
            try:
                if subprocess.call(command(ff, ["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=30:d=0.2"], out, enc, pre, vf, opts),
                                   stdout=subprocess.DEVNULL, **quiet) == 0:
                    y = subprocess.run([ff, "-v", "quiet", "-nostdin", "-i", out, "-f", "rawvideo", "-pix_fmt", "gray", "-"],
                                       stdout=subprocess.PIPE, **quiet).stdout
                    if y and 100 < min(y) and max(y) < 156:  # the flat gray came back
                        TESTED.add(key)
            except (OSError, subprocess.TimeoutExpired):
                pass
    return key in TESTED


def compress(src, dst=None):
    """Compress src into dst (default <src>_1080p.mp4). Returns dst, or None when no encoder
    worked. The app calls this after each recording (ui/service.py); main() is the CLI."""
    dst = dst or os.path.splitext(src)[0] + "_1080p.mp4"
    exes = [ffmpeg()]
    spare = bundled_ffmpeg()
    if spare and os.path.realpath(spare) != os.path.realpath(exes[0]):
        exes.append(spare)
    gpus = ENCODERS.get(sys.platform[:5] if sys.platform.startswith("linux") else sys.platform, [])
    plan = [(ff,) + e for e in gpus for ff in exes] + [(ff,) + CPU for ff in exes]
    for i, (ff, enc, pre, vf, opts) in enumerate(plan):
        if i < len(plan) - 1 and not usable(ff, enc, pre, vf, opts):
            continue  # the last one runs untested, so when nothing works ffmpeg's errors still show
        print("encoding with %s ..." % enc, flush=True)
        t0 = time.time()
        rc = subprocess.call(command(ff, ["-i", src], dst, enc, pre, vf, opts), creationflags=NO_WINDOW)
        if rc == 0 and os.path.getsize(dst) > 0:
            a, b = os.path.getsize(src), os.path.getsize(dst)
            print("done: %s  %.1f MB -> %.1f MB (%.1fx smaller) in %.1f s" % (dst, a / 1e6, b / 1e6, a / b, time.time() - t0),
                  flush=True)  # the app's stdout is a buffered wrapper, and it exits through os._exit
            return dst
        print("  %s failed, trying the next encoder" % enc, flush=True)
        TESTED.discard((ff, enc) + tuple(pre))  # tested again next time
        if os.path.exists(dst):
            os.remove(dst)
    return None


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if not compress(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None):
        sys.exit("no encoder worked")


if __name__ == "__main__":
    main()
