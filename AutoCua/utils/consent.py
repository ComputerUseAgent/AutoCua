"""One question before any download: nothing AutoCua needs is fetched without
the person's yes.

    from AutoCua.utils.consent import allow_download
    if not allow_download("adb, Google's platform-tools", "Google (dl.google.com)", 16110554):
        return False                       # nothing fetched; the line said why

The question goes to the terminal the program runs on, the desktop app's
included (it is started from one). AUTO_ENV=1 says yes to every download for
a run nobody can answer (CI); without it, and with no terminal to ask on, the
download does not happen and the line says so. Google's Android SDK terms
(android_connector/emulator.py) have their own acceptance, since those are a
licence, not just a download.

can_ask() and ask() are the terminal question itself, shared with the
Electron download's offer (utils/electron.py).
"""

import os
import sys
import threading
import time

# Yes to every download, for runs nobody can answer.
AUTO_ENV = "AutoCua_AUTO_DOWNLOAD"

# How long a question waits for an answer before it counts as no.
ASK_TIMEOUT = 120


def _size(size):
    if size is None:
        return ""
    if isinstance(size, str):
        return f", {size}"
    return f", {size / 1e9:.1f} GB" if size >= 1e9 else f", {size / 1e6:.0f} MB"


def allow_download(what, source, size=None, log=print):
    """True when `what` may be downloaded now from `source` (a maker's own
    server, named so the person can see it), with `size` in bytes or words:
    the person answered y on the terminal, or AUTO_ENV=1. False otherwise,
    after one line through `log` that says how to go on."""
    if (os.environ.get(AUTO_ENV) or "").strip() == "1":
        return True
    if not can_ask():
        log(f"{what} is needed and is not installed, and nothing is downloaded without your yes. "
            f"Run this on a terminal and answer y, or set {AUTO_ENV}=1.")
        return False
    log(f"{what} is needed and is not installed. It comes from {source}{_size(size)}.")
    if ask("    Download it now? [y/N] ") in ("y", "yes"):
        return True
    log(f"Not downloaded. Run again and answer y, or set {AUTO_ENV}=1.")
    return False


def can_ask():
    """True when a person can answer on this terminal: stdin and stdout are
    one (on Windows a real console: NUL also says isatty) and it can be seen
    (Windows: not started hidden), and on POSIX this process is the
    terminal's foreground job (a `&` job that read it would be stopped)."""
    try:
        if not (sys.stdin and sys.stdout and sys.stdin.isatty() and sys.stdout.isatty()):
            return False
        if os.name == "nt":
            import ctypes
            import msvcrt
            from ctypes import wintypes
            kernel32 = ctypes.WinDLL("kernel32")
            kernel32.GetConsoleMode.argtypes = (wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD))
            mode = wintypes.DWORD()
            if not all(kernel32.GetConsoleMode(msvcrt.get_osfhandle(stream.fileno()),
                                               ctypes.byref(mode))
                       for stream in (sys.stdin, sys.stdout)):
                return False

            class STARTUPINFOW(ctypes.Structure):
                _fields_ = [("cb", wintypes.DWORD), ("lpReserved", wintypes.LPWSTR),
                            ("lpDesktop", wintypes.LPWSTR), ("lpTitle", wintypes.LPWSTR),
                            ("dwX", wintypes.DWORD), ("dwY", wintypes.DWORD),
                            ("dwXSize", wintypes.DWORD), ("dwYSize", wintypes.DWORD),
                            ("dwXCountChars", wintypes.DWORD), ("dwYCountChars", wintypes.DWORD),
                            ("dwFillAttribute", wintypes.DWORD), ("dwFlags", wintypes.DWORD),
                            ("wShowWindow", wintypes.WORD), ("cbReserved2", wintypes.WORD),
                            ("lpReserved2", ctypes.c_void_p), ("hStdInput", wintypes.HANDLE),
                            ("hStdOutput", wintypes.HANDLE), ("hStdError", wintypes.HANDLE)]

            # Started with its window hidden (Start-Process -WindowStyle
            # Hidden, a script's Run(..., 0)): a real console nobody sees.
            info = STARTUPINFOW()
            info.cb = ctypes.sizeof(info)
            kernel32.GetStartupInfoW(ctypes.byref(info))
            if info.dwFlags & 0x1 and info.wShowWindow == 0:   # STARTF_USESHOWWINDOW, SW_HIDE
                return False
            # A venv's python.exe is a redirector that starts the real
            # interpreter with STARTF_USESTDHANDLES only, so the flags above
            # never get here. The console's own window tells instead: a classic
            # console window that is not visible. (A terminal's pseudoconsole,
            # Windows Terminal's or VS Code's, is a PseudoConsoleWindow, which
            # is always hidden.)
            user32 = ctypes.WinDLL("user32")
            kernel32.GetConsoleWindow.restype = wintypes.HWND
            user32.GetClassNameW.argtypes = (wintypes.HWND, wintypes.LPWSTR, ctypes.c_int)
            user32.IsWindowVisible.argtypes = (wintypes.HWND,)
            hwnd = kernel32.GetConsoleWindow()
            if hwnd:
                name = ctypes.create_unicode_buffer(64)
                user32.GetClassNameW(hwnd, name, len(name))
                if name.value == "ConsoleWindowClass" and not user32.IsWindowVisible(hwnd):
                    return False
            return True
        return os.tcgetpgrp(sys.stdin.fileno()) == os.getpgrp()
    except (OSError, ValueError, AttributeError):
        return False


def ask(question, timeout=ASK_TIMEOUT):
    """The answer to `question`, stripped and lowercased; "" for Enter and
    for the end of input, None when none came within `timeout` (a terminal
    nobody looks at: a detached tmux, a console hidden some way _can_ask
    cannot see). The question goes to stdout like the lines before it
    (input() would put it on stderr, which may be redirected), and keys
    pressed before it was asked (an Enter while the app loaded) are dropped."""
    fd = sys.stdin.fileno()
    try:
        if os.name == "nt":
            import ctypes
            import msvcrt
            kernel32 = ctypes.WinDLL("kernel32")
            kernel32.FlushConsoleInputBuffer.argtypes = (ctypes.c_void_p,)
            kernel32.FlushConsoleInputBuffer(msvcrt.get_osfhandle(fd))
        else:
            import termios
            termios.tcflush(fd, termios.TCIFLUSH)
    except Exception:
        pass
    sys.stdout.write(question)
    sys.stdout.flush()
    # Read on a thread so the wait can end. os.read, not sys.stdin: a reader
    # left waiting holds no lock that interpreter shutdown would need.
    answer = []
    reader = threading.Thread(target=lambda: answer.append(os.read(fd, 1024)), daemon=True)
    reader.start()
    deadline = time.monotonic() + timeout
    while reader.is_alive() and time.monotonic() < deadline:
        reader.join(0.25)          # short waits, so Ctrl+C still gets through
    if not answer:
        sys.stdout.write("\n")
        return None
    if not answer[0] and os.name == "nt":
        # A console read ends with nothing at Ctrl+Z Enter and at Ctrl+C,
        # whose CTRL_C_EVENT reaches this process just after the read
        # returned. Let it arrive here, inside the caller's try.
        time.sleep(0.5)            # a Ctrl+C interrupts this at once
    text = answer[0].decode(errors="replace")
    if not text and os.name != "nt":
        sys.stdout.write("\n")     # Ctrl+D left the cursor on the question
    return text.strip().lower()


