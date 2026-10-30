"""The phone helper: AutoCua's Helper.java, for the screenshots and the tree.

Android's own screencap took 0.7 to 2 s per shot on a 2019 phone over USB 2.0:
the phone turns the screen into a PNG, or 10 MB of raw pixels go down the
cable (a moto g77 on USB: raw screencap plus adb pull 550 ms, 379 ms with
adb's zstd). The helper instead takes each screenshot through UiAutomation
when asked and compresses it to JPEG on the phone, so only the JPEG crosses
the cable. Likewise `uiautomator dump` waits a second for a still screen
before every tree; Helper.java (built by build_helper.py) keeps the same
accessibility connection open and answers at once.

It is ONE app_process on the phone, with AutoCua's jar alone on its
classpath, answering on one local socket that is forwarded to a free local
port. It ends when its stdin closes: stop() closes it, and so does this
process ending, however it ends.

Nothing is installed on the phone: the jar is copied to /data/local/tmp and
deletes itself once loaded.

This file only starts and stops the helper; reading its screenshots and tree
is element.rs's job (AutoCua/android/tree/element.rs), handed the local port
it is forwarded to.
"""

import random
import socket
import subprocess
import time

from AutoCua.android_connector.adb import _adb
from AutoCua.android_connector.build_helper import ensure_helper

HELPER_DEVICE_PATH = "/data/local/tmp/autocua-helper.jar"

# How long a helper that was just launched gets to answer: app_process starts
# in well under a second, then it connects to Android's accessibility once
# before it takes requests.
START_WAIT = 10.0


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


def _recv(sock, n):
    """Exactly n bytes from the helper's socket."""
    data = b""
    while len(data) < n:
        chunk = sock.recv(n - len(data))
        if not chunk:
            raise ConnectionError("the helper closed the connection")
        data += chunk
    return data


def _wait_ready(port, proc):
    """Return once the helper answers ping on port. The forward takes a
    connection before the phone side listens, then drops it, so a dropped one
    is tried again; a helper whose adb client ended fails at once."""
    deadline = time.monotonic() + START_WAIT
    while True:
        if proc.poll() is not None:
            raise RuntimeError(f"the phone helper ended at start (adb shell exit {proc.returncode})")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=max(0.5, deadline - time.monotonic())) as s:
                s.sendall(b"ping\n")
                _recv(s, int.from_bytes(_recv(s, 4), "big"))
                return
        except OSError:
            pass
        if time.monotonic() >= deadline:
            raise RuntimeError(f"the phone helper did not answer within {START_WAIT:.0f} s")
        time.sleep(0.1)


class Stream:
    """The helper on one phone, and the local port its screenshots and tree come out on."""

    def __init__(self, adb, serial, tree_port, proc):
        self.adb, self.serial, self.proc = adb, serial, proc
        self.tree_port = tree_port

    @classmethod
    def start(cls, adb, serial):
        """Copy the jar to the phone, forward a free local port to it and launch
        the helper. Returns once the helper answers on that port."""
        helper = ensure_helper()
        (port,) = _free_ports(1)
        scid = f"{random.getrandbits(31):08x}"      # names the phone-side socket
        forwarded, proc = [], None
        try:
            for args in (("push", str(helper), HELPER_DEVICE_PATH),
                         ("forward", f"tcp:{port}", f"localabstract:autocua_{scid}")):
                done = _adb(adb, "-s", serial, *args)
                if done.returncode != 0:
                    raise RuntimeError(f"adb {args[0]} failed: {done.stderr.strip()}")
                if args[0] == "forward":
                    forwarded.append(args[1])
            # stdin is a pipe this side holds open: the helper ends when it
            # closes (stop(), or this process ending), and `adb shell` never
            # reads the terminal.
            proc = subprocess.Popen(
                [adb, "-s", serial, "shell", f"CLASSPATH={HELPER_DEVICE_PATH}", "app_process", "/",
                 "autocua.Helper", f"autocua_{scid}"],
                stdin=subprocess.PIPE)
            _wait_ready(port, proc)
        except BaseException:
            # Half started (a failed adb call, no answer, Ctrl+C): no helper
            # or forward is left behind.
            if proc is not None:
                cls(adb, serial, port, proc).stop()
            else:
                for local in forwarded:
                    _adb(adb, "-s", serial, "forward", "--remove", local)
            raise
        return cls(adb, serial, port, proc)

    def stop(self):
        """Stop the helper and drop the forward. Closing its stdin ends it on
        the phone (Helper.java's lifeline), which lets go of Android's
        UiAutomation before this returns; one that does not end in time is
        stopped with its adb client."""
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        _adb(self.adb, "-s", self.serial, "forward", "--remove", f"tcp:{self.tree_port}")
