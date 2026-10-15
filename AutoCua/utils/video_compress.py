#!/usr/bin/env python3
"""Fastest 1080p HEVC compression (macOS / Windows / Linux).

    python compress.py input.mp4            ->  input_1080p.mp4
    python compress.py input.mp4 out.mp4

Scales to 1080p and encodes with the GPU (VideoToolbox / NVENC / QuickSync / AMF / VAAPI); falls back to
libx265 on the CPU. Uses the system ffmpeg, or the one bundled in the imageio-ffmpeg pip package.
"""
import os
import shutil
import subprocess
import sys
import time


def ffmpeg():
    exe = shutil.which("ffmpeg")
    if exe:
        return exe
    try:
        import imageio_ffmpeg
    except ImportError:
        subprocess.check_call([sys.executable, "-m", "pip", "install", "-q", "imageio-ffmpeg"])
        import imageio_ffmpeg
    return imageio_ffmpeg.get_ffmpeg_exe()


# encoder -> (input options, filter chain, encoder options); tried in order, first one that works wins
Q = "26"  # CRF-style quality; VideoToolbox 55 is calibrated to match libx265 CRF 26 on screen content
SCALE = "scale=w='if(gt(iw,ih),-2,min(1080,iw))':h='if(gt(iw,ih),min(1080,ih),-2)':flags=bicubic"
VAAPI = "/dev/dri/renderD128"
ENCODERS = {
    "darwin": [("hevc_videotoolbox", [], SCALE + ",format=yuv420p", ["-q:v", "55", "-profile:v", "main"])],
    "win32": [("hevc_nvenc", [], SCALE + ",format=yuv420p", ["-preset", "p4", "-rc", "vbr", "-cq", Q, "-b:v", "0", "-spatial-aq", "1"]),
              ("hevc_qsv", [], SCALE + ",format=nv12", ["-preset", "faster", "-global_quality", Q]),
              ("hevc_amf", [], SCALE + ",format=nv12", ["-quality", "balanced", "-rc", "cqp", "-qp_i", Q, "-qp_p", Q])],
    "linux": [("hevc_nvenc", [], SCALE + ",format=yuv420p", ["-preset", "p4", "-rc", "vbr", "-cq", Q, "-b:v", "0", "-spatial-aq", "1"]),
              ("hevc_qsv", [], SCALE + ",format=nv12", ["-preset", "faster", "-global_quality", Q]),
              ("hevc_vaapi", ["-init_hw_device", "vaapi=va:" + VAAPI, "-filter_hw_device", "va"],
               SCALE + ",format=nv12,hwupload", ["-rc_mode", "CQP", "-qp", Q])],
}
CPU = ("libx265", [], SCALE + ",format=yuv420p", ["-preset", "superfast", "-crf", Q, "-x265-params", "log-level=error"])
NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0  # no console window flashing up from the app on Windows


def compress(src, dst=None):
    """Compress src into dst (default <src>_1080p.mp4). Returns dst, or None when no encoder
    worked. The app calls this after each recording (ui/service.py); main() is the CLI."""
    dst = dst or os.path.splitext(src)[0] + "_1080p.mp4"
    ff = ffmpeg()
    for enc, pre, vf, opts in ENCODERS.get(sys.platform[:5] if sys.platform.startswith("linux") else sys.platform, []) + [CPU]:
        print("encoding with %s ..." % enc, flush=True)
        t0 = time.time()
        rc = subprocess.call([ff, "-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-stats"] + pre + ["-i", src,
                              "-vf", vf, "-c:v", enc, "-g", "300"] + opts +  # keyframe every 10 s (30 fps)
                             ["-c:a", "aac", "-b:a", "160k", "-tag:v", "hvc1", "-movflags", "+faststart", "-f", "mp4", dst],
                             creationflags=NO_WINDOW)
        if rc == 0 and os.path.getsize(dst) > 0:
            a, b = os.path.getsize(src), os.path.getsize(dst)
            print("done: %s  %.1f MB -> %.1f MB (%.1fx smaller) in %.1f s" % (dst, a / 1e6, b / 1e6, a / b, time.time() - t0))
            return dst
        print("  %s failed, trying the next encoder" % enc)
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
