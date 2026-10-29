"""The phone helper: scrcpy's server for the screen, AutoCua's for the tree.

Android's own screencap took 0.7 to 2 s per shot on a 2019 phone over USB 2.0:
the phone turns the screen into a PNG, or 10 MB of raw pixels go down the
cable. scrcpy's server instead keeps running on the phone and sends the screen
as H.264 video, only when it changes, so a screenshot is the newest frame,
already on this side. Likewise `uiautomator dump` waits a second for a still
screen before every tree; Helper.java (built by build_helper.py) keeps the
same accessibility connection open and answers at once.

Both run as ONE helper: one app_process on the phone, with scrcpy's jar and
AutoCua's jar on its classpath (scrcpy's first, it takes that entry as its own
file). Helper.java starts the tree reader on a thread and hands the main
thread to scrcpy, so the video ending ends the whole helper.

The scrcpy server is Genymobile's (Apache-2.0). It is fetched once from their
GitHub release into scrcpy-server-v<version> beside this file (gitignored),
pinned by size and SHA-256 like adb.py's zip, and must match VERSION exactly:
the server refuses a client of another version. Nothing is installed on the
phone. Both jars are copied to /data/local/tmp and delete themselves once
loaded.

This file only starts and stops the helper. Reading the video and the tree is
element.rs's job (AutoCua/android/tree/element.rs), handed the two local ports
they are forwarded to.
"""

import os
import random
import re
import socket
import subprocess
import threading

from AutoCua.android_connector.adb import HERE, _adb, _download
from AutoCua.android_connector.build_helper import ensure_helper
from AutoCua.utils.consent import allow_download

# Pinned scrcpy release. Recorded 2026-10-25 from the GitHub release's asset
# list (size + sha256 digest) and the file as downloaded.
VERSION = "5.0.1"
URL = f"https://github.com/Genymobile/scrcpy/releases/download/v{VERSION}/scrcpy-server-v{VERSION}"
SIZE = 733930
SHA256 = "764eb6f79811d5211fe9df341120882ba9994c7a61b897d7bf3fb662e53bc536"

SERVER = HERE / f"scrcpy-server-v{VERSION}"
DEVICE_PATH = "/data/local/tmp/scrcpy-server.jar"
HELPER_DEVICE_PATH = "/data/local/tmp/autocua-helper.jar"

# Video bit rate. scrcpy's default is 8 Mbps; 16 keeps small text sharper and
# is still a fraction of what USB 2.0 carries.
BIT_RATE = 16_000_000

# The frame the model gets, capped as in ios/tree/element.py: the long side
# at most 2300 px and the whole frame at most 3.3 MP. scrcpy scales on the
# phone (its max_size caps the long side), so this costs nothing here.
LLM_IMAGE_MAX_EDGE = 2300
LLM_IMAGE_MAX_PIXELS = 3_300_000

# One fetch per process, however many threads ask at once.
_LOCK = threading.Lock()


def ensure_server() -> bool:
    """True when scrcpy's server is on disk. Fetches it once if it isn't."""
    if SERVER.is_file():
        return True
    with _LOCK:
        if SERVER.is_file():
            return True
        if not allow_download(f"scrcpy's server {VERSION}", "Genymobile's scrcpy release on GitHub", SIZE,
                              log=lambda text: print(f"🤖 {text}", flush=True)):
            return False
        # Written under a private name and renamed into place, so a partial
        # download is never seen as the server.
        staging = HERE / f".scrcpy-server.incoming.{os.getpid()}"
        print(f"🤖 Fetching scrcpy's server {VERSION} from GitHub, one time...", flush=True)
        try:
            _download(URL, staging, SIZE, SHA256)
            os.replace(staging, SERVER)
        except Exception as e:
            staging.unlink(missing_ok=True)
            print(f"🤖 Could not fetch scrcpy's server: {e}")
            print(f"🤖 Fetch it by hand: {URL} -> {SERVER}")
            return False
        print(f"🤖 scrcpy's server {VERSION} ready at {SERVER}")
        return True


def _free_ports(n):
    """n different free local ports (held open together while picked, so the
    system cannot hand out the same one twice)."""
    socks = [socket.socket() for _ in range(n)]
    try:
        for s in socks:
            s.bind(("127.0.0.1", 0))
        return [s.getsockname()[1] for s in socks]
    finally:
        for s in socks:
            s.close()


def _max_size(adb, serial):
    """scrcpy's max_size for this screen, from `wm size` (an override size is
    listed last and wins). The edge cap alone if the size cannot be read."""
    sizes = re.findall(r"(\d+)x(\d+)", _adb(adb, "-s", serial, "shell", "wm size").stdout)
    if not sizes:
        return LLM_IMAGE_MAX_EDGE
    w, h = (int(v) for v in sizes[-1])
    shrink = min(1.0, (LLM_IMAGE_MAX_PIXELS / (w * h)) ** 0.5)
    return min(LLM_IMAGE_MAX_EDGE, int(max(w, h) * shrink))


class Stream:
    """The helper on one phone, and the local ports its video and tree come out on."""

    def __init__(self, adb, serial, port, tree_port, proc):
        self.adb, self.serial, self.proc = adb, serial, proc
        self.port, self.tree_port = port, tree_port

    @classmethod
    def start(cls, adb, serial):
        """Copy both jars to the phone, forward two free local ports to them and
        launch the helper. It is listening a moment later; element.rs waits for that."""
        if not ensure_server():
            raise RuntimeError("scrcpy's server is not available")
        helper = ensure_helper()
        max_size = _max_size(adb, serial)
        port, tree_port = _free_ports(2)
        scid = f"{random.getrandbits(31):08x}"      # names the phone-side sockets
        forwarded = []
        try:
            for args in (("push", str(SERVER), DEVICE_PATH),
                         ("push", str(helper), HELPER_DEVICE_PATH),
                         ("forward", f"tcp:{port}", f"localabstract:scrcpy_{scid}"),
                         ("forward", f"tcp:{tree_port}", f"localabstract:autocua_{scid}")):
                done = _adb(adb, "-s", serial, *args)
                if done.returncode != 0:
                    raise RuntimeError(f"adb {args[0]} failed: {done.stderr.strip()}")
                if args[0] == "forward":
                    forwarded.append(args[1])
            # stdin from DEVNULL: `adb shell` would otherwise read the terminal
            # and take keys meant for whoever runs this.
            proc = subprocess.Popen(
                [adb, "-s", serial, "shell", f"CLASSPATH={DEVICE_PATH}:{HELPER_DEVICE_PATH}", "app_process", "/",
                 "autocua.Helper", f"autocua_{scid}",
                 VERSION, f"scid={scid}", "log_level=warn",
                 "audio=false", "control=false", "tunnel_forward=true", "send_device_meta=false",
                 "video_codec=h264", f"video_bit_rate={BIT_RATE}", f"max_size={max_size}"],
                stdin=subprocess.DEVNULL)
        except BaseException:
            # Half started (a failed adb call, Ctrl+C): no forward is left behind.
            for local in forwarded:
                _adb(adb, "-s", serial, "forward", "--remove", local)
            raise
        return cls(adb, serial, port, tree_port, proc)

    def stop(self):
        """Stop the helper and drop the forwards. element.rs closing its socket
        is not enough: with no control socket scrcpy only notices at its next frame."""
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        for port in (self.port, self.tree_port):
            _adb(self.adb, "-s", self.serial, "forward", "--remove", f"tcp:{port}")
