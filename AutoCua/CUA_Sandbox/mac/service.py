"""The Mac sandbox for the app: the second display the agent works on, while
the person keeps the main screen, the pointer and the keyboard.

The second display is Mac_CUA.rs beside this file, one standalone program.
This file starts it once, in its --serve mode, and passes the agent's orders
on over its pipes: one short method per thing asked of it, as the Windows
sandbox has (CUA_Sandbox/windows/service.py). A tool of the Mac controller
that works on the second display calls `sandbox()` and one of these methods.
So far: `launch` and `click` (the left button). The main screen stays the
default: nothing here runs unless a run was asked for the sandbox.

The program is built from Mac_CUA.rs with rustc when it is missing or older
than its source (about 2 seconds). It ends when this app ends, with every app
it started on the second display.

Two runs of the same program file at once get no pictures (Mac_CUA.rs says
why), so the app holds one Sandbox and every tool shares it.
"""

import atexit
import queue
import shutil
import subprocess
import threading
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SOURCE = _HERE / "Mac_CUA.rs"
_PROGRAM = _HERE / "build" / "Mac_CUA"

# Seconds an order may take before it counts as lost. `run` waits for the
# app's first window inside the program (up to 10 s there).
_START_WAIT = 20
_RUN_WAIT = 40
_CLICK_WAIT = 15


class SandboxError(RuntimeError):
    """The second display could not do what was asked. The message says why,
    in words the model can read in a tool result."""


def _build():
    """The program, built when it is missing or older than its source."""
    if _PROGRAM.exists() and _PROGRAM.stat().st_mtime >= _SOURCE.stat().st_mtime:
        return
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / "rustc")
    if not Path(rustc).exists():
        raise SandboxError("the second display is not built and rustc was not found to build it")
    _PROGRAM.parent.mkdir(exist_ok=True)
    built = subprocess.run([rustc, "--edition", "2021", "-O", str(_SOURCE), "-o", str(_PROGRAM)],
                           capture_output=True, text=True)
    if built.returncode != 0:
        raise SandboxError(f"the second display did not build: {built.stderr.strip()[-400:]}")


class Sandbox:
    """The second display and what can be asked of it."""

    def __init__(self):
        _build()
        self.place = None             # (x, y, width, height): where it is, in the points all screens share
        self._frame = None            # its newest picture, a BMP file's bytes
        self._said = []               # the last lines the program wrote for a person (its stderr)
        self._answers = queue.Queue() # the answers to the orders, in order
        self._placed = threading.Event()
        self._order = threading.Lock()  # one order at a time: an answer belongs to the order before it
        self._program = subprocess.Popen([str(_PROGRAM), "--serve"], stdin=subprocess.PIPE,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._listen, daemon=True).start()
        if not self._placed.wait(_START_WAIT):
            why = " ".join(self._said[-3:]) or "it said nothing"
            self.stop()
            raise SandboxError(f"the second display did not start: {why}")

    def _read(self):
        """The program's messages: a line "<kind> <bytes>", then that many bytes."""
        out = self._program.stdout
        while True:
            head = out.readline()
            if not head:
                break
            try:
                kind, size = head.decode("ascii").split()
                data = out.read(int(size))
            except ValueError:
                break
            if kind == "frame":
                self._frame = data
            elif kind == "answer":
                self._answers.put(data.decode("utf-8", "replace"))
            elif kind == "place":
                try:
                    self.place = tuple(float(n) for n in data.decode("ascii").split())
                except ValueError:
                    self.place = None
                self._placed.set()
            elif kind == "note":
                self._said.append(data.decode("utf-8", "replace"))
        self._answers.put("error: the second display has ended")

    def _listen(self):
        """What the program says for a person, kept for an error's words."""
        for line in self._program.stderr:
            self._said.append(line.decode("utf-8", "replace").strip())
            del self._said[:-20]

    def alive(self):
        return self._program.poll() is None

    def _ask(self, order, wait):
        """One order and its answer: the words after "ok", or SandboxError."""
        with self._order:
            if not self.alive():
                raise SandboxError("the second display has ended")
            while not self._answers.empty():
                self._answers.get_nowait()
            try:
                self._program.stdin.write((order + "\n").encode("utf-8"))
                self._program.stdin.flush()
                answer = self._answers.get(timeout=wait)
            except (OSError, queue.Empty):
                raise SandboxError(f"the second display did not answer: {order.split()[0]}") from None
        if answer.startswith("ok"):
            return answer[2:].strip()
        raise SandboxError(answer.removeprefix("error:").strip())

    def launch(self, app):
        """Start an app on the second display, by its name or the path of its
        .app. Gives its process id."""
        return int(self._ask(f"run {app}", _RUN_WAIT).split()[1])

    def click(self, x, y, clicks=1):
        """`clicks` clicks of the left button at a point of the second
        display, in the points all screens share (what a scan's rect holds).
        The person's pointer does not move and nothing comes in front."""
        return self._ask(f"click {float(x)} {float(y)} {int(clicks)}", _CLICK_WAIT)

    def shot(self):
        """The newest picture of the second display, as a BMP file's bytes.
        None before the first one."""
        return self._frame

    def stop(self):
        """End the second display and every app on it."""
        try:
            self._program.stdin.close()
            self._program.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired):
            self._program.kill()


_one = None
_making = threading.Lock()


def sandbox():
    """The app's one Sandbox: made on first use, and again when it has ended."""
    global _one
    with _making:
        if _one is None or not _one.alive():
            _one = Sandbox()
        return _one


def stop():
    """End the second display, when there is one."""
    global _one
    with _making:
        if _one is not None:
            _one.stop()
            _one = None


atexit.register(stop)
