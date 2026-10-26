"""One Android phone for the agent: adb and the phone helper, opened before a
run and closed after it, the way ios_connector/session.py's wda_session is
for the iPhone.

activate() finds adb and waits for the phone (adb.py), then starts the helper
on it (stream.py): scrcpy's server for the screen and Helper.java for the UI
tree, one process, forwarded to two local ports. The scanner
(android/tree/element.rs, UIElementScanner) reads those two ports; it calls
activate() itself when nobody has, so it also works on its own.
deactivate() stops the helper and drops the forwards. It also runs at exit,
so a helper never outlives this process (the helper holds Android's one
UiAutomation slot: a leftover one makes the next run's helper fail with
"already registered").

agent_launcher.run_agent does both around a "mobile use, android" run.
"""

import atexit
import os
import threading

from AutoCua.android_connector.adb import _adb, connect
from AutoCua.android_connector.stream import Stream

# Picks one phone when several are plugged in (adb's serial). Unset: the one
# USB phone.
SERIAL_ENV = "AutoCua_ANDROID_SERIAL"


class AndroidSession:
    """The helper on one phone, and the local ports its screen and tree come
    out on. One per process."""

    def __init__(self):
        self._lock = threading.Lock()
        self.adb = None
        self.serial = None
        self.model = None
        self.stream = None
        atexit.register(self.deactivate)

    def activate(self, serial=None):
        """Connect the phone and start its helper.

        Returns {"ok": True, "state": "connected", "serial", "model", "adb",
        "port", "tree_port"}, or {"ok": False, "state": "error", "error"} when
        the phone cannot be reached. While the helper is running, a second
        call returns the same connection.
        """
        with self._lock:
            if self._alive():
                return self._info()
            if self.stream is not None:
                self._stop()     # the helper died: its forwards go before a new one
            try:
                adb, found = connect(serial or os.environ.get(SERIAL_ENV) or None)
                model = _adb(adb, "-s", found, "shell", "getprop", "ro.product.model").stdout.strip()
                stream = Stream.start(adb, found)
            except Exception as e:  # noqa: BLE001 - reported, not raised, like wda_session
                return {"ok": False, "state": "error", "error": str(e)}
            self.adb, self.serial, self.model, self.stream = adb, found, model or found, stream
            return self._info()

    def status(self):
        """{"state": "connected"} while the helper runs, else "disconnected"."""
        with self._lock:
            return self._info() if self._alive() else {"ok": True, "state": "disconnected"}

    def deactivate(self):
        """Stop the helper and drop its forwards. Safe to call twice."""
        with self._lock:
            if self.stream is not None:
                self._stop()

    def _alive(self):
        return self.stream is not None and self.stream.proc.poll() is None

    def _stop(self):
        try:
            self.stream.stop()
        except Exception as e:  # noqa: BLE001 - adb gone at exit, nothing to do about it
            print(f"📱 stopping the phone helper: {e}")
        self.stream = None

    def _info(self):
        return {"ok": True, "state": "connected", "serial": self.serial, "model": self.model,
                "adb": self.adb, "port": self.stream.port, "tree_port": self.stream.tree_port}


android_session = AndroidSession()
