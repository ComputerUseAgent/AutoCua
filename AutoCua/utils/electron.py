"""Fetch Electron, which runs the app's Chromium window, if it isn't already here.

The desktop app's Chromium window is AutoCua/desktop (main.js and
package.json), an Electron app. A checkout can `npm install` Electron in that
folder, but a `pip install AutoCua` runs no installer and has no Node.js — a
wheel is unpacked, never executed — so without this every pip install opened
the pywebview window instead. Nothing from npm is needed to run the shell:
Electron is one release zip per OS and architecture, and the launcher hands it
the shell folder (`<electron> AutoCua/desktop`), which is all `npm start`
does.

Installing it is the person's call, so nothing is downloaded without a yes.
The first launch asks on the terminal (offer_electron); a yes fetches the
official release zip for this machine from Electron's GitHub releases, the
same file `npm install electron` fetches, checks its byte size and SHA-256
against the pins below before anything is unpacked, and unpacks it into a
per-user cache folder:

    macOS     ~/Library/Caches/AutoCua/electron/v<version>-darwin-<arch>/
    Windows   %LOCALAPPDATA%/AutoCua/electron/v<version>-win32-<arch>/
    Linux     ${XDG_CACHE_HOME:-~/.cache}/AutoCua/electron/v<version>-linux-<arch>/

Not AutoCua_data, which after a pip install is wherever main.py was run from
(every such folder would get its own 300-400 MB copy), and not site-packages,
which need not be writable and holds one copy per environment.
AUTOCUA_ELECTRON_CACHE moves the folder. ELECTRON_MIRROR and
ELECTRON_CUSTOM_DIR are read as npm's Electron installer reads them, to fetch
from a mirror (the question names it); the digest must still match. A
half-unpacked tree is never visible under the real name, and it is safe to
call from several threads and several processes at once.

An "n" is remembered (a `declined` file in that folder) and not asked again;
Enter is "not now". With no terminal to ask on, nothing is asked. Either way
the app opens in the pywebview window and says how to fetch Electron later,
which asks nothing (running it is the yes):

    python -m AutoCua.utils.electron [--force]

Electron is MIT-licensed; see THIRD_PARTY_NOTICES.md.
"""

import hashlib
import os
import platform
import shlex
import shutil
import stat
import subprocess
import sys
import threading
import time
import zipfile
from pathlib import Path

# Keep in step with "electron" in AutoCua/desktop/package.json and with
# THIRD_PARTY_NOTICES.md. A re-pin changes the version AND every size and
# digest below, read from the release's SHASUMS256.txt and the files.
VERSION = "44.7.0"

# "<os>-<arch>", as Electron names its zips -> (bytes, sha256, bytes
# unpacked). The last is only for telling the person what a yes costs.
ZIPS = {
    "darwin-arm64": (130394296, "e04e411b58a0a14375dd21b0ab4a378fd38930a702e4e20e322fee4849404c0b", 321398871),
    "darwin-x64": (134289782, "4fe494a7159b10a81e2098dab388719582065e99e604c4510bb117e282213566", 324909831),
    "linux-arm64": (124088755, "9cd736d1f5873839553c3fa6c8c5e57635ec4a399cc2d32e79c82a02d9cd2258", 301132304),
    "linux-x64": (122998223, "3ae7d5bdad61c664486c6ab61361dd084d064c8c08af8d13a687096e18ce555a", 296307552),
    "win32-arm64": (156427308, "ee2621a4513f92a952273d5363e153db6f0e4dd183d89f2c0f740faa13623316", 383913807),
    "win32-x64": (158293649, "eee30dc8fa1f5ea95490e59f44e46ea68dd24c6e93d22facf70fe5c2d4c2665c", 385514847),
}

# Where the executable sits in the unpacked zip (what npm's electron package
# records in its path.txt). Every zip also holds a `version` file, which is
# how a finished copy is told from what a cache cleaner left of one.
EXECUTABLE = {
    "darwin": "Electron.app/Contents/MacOS/Electron",
    "linux": "electron",
    "win32": "electron.exe",
}

# The oldest macOS this Electron runs on (LSMinimumSystemVersion in the
# app's Info.plist, and every Mach-O's minimum).
MIN_MACOS = (13, 0)

URL = "https://github.com/electron/electron/releases/download/"
ENV_CACHE = "AUTOCUA_ELECTRON_CACHE"

# In cache_root(): the person said no, so the launch does not ask again.
DECLINED = "declined"

# How long the question waits for an answer before it counts as "not now"
# and the app opens in pywebview.
ASK_TIMEOUT = 120

# In cache_root(), locked by whichever process is fetching (_FetchLock): a
# second one waits for it rather than download the same 120-160 MB again. A
# fetch's download and half-unpacked tree carry ".incoming." in their names,
# so whatever the lock holder finds under such a name was left by a fetch
# that was killed outright (no `finally` ran), and it deletes it.
LOCK_FILE = ".lock"

# One fetch per process, however many threads ask at once.
_LOCK = threading.Lock()


def target():
    """This machine's "<os>-<arch>" as Electron names its zips, or None when
    Electron has no build for it."""
    if sys.platform == "darwin":
        os_name = "darwin"
    elif sys.platform == "win32":
        os_name = "win32"
    elif sys.platform.startswith("linux"):
        os_name = "linux"
    else:
        return None
    # On macOS this is the arch of this Python (x86_64 under Rosetta), which
    # is the one wanted: Electron starts this Python again as its backend,
    # and an arm64 Electron would start a universal Python as arm64, which
    # could not load the x86_64 packages a Rosetta Python installed.
    machine = platform.machine().lower()
    arch = {"x86_64": "x64", "amd64": "x64", "arm64": "arm64", "aarch64": "arm64"}.get(machine)
    if os_name == "linux" and sys.maxsize <= 2**32:
        # A 32-bit userland on a 64-bit kernel (Raspberry Pi OS) says
        # aarch64, but has nothing a 64-bit Electron could load.
        arch = None
    key = f"{os_name}-{arch}"
    return key if key in ZIPS else None


def cache_root():
    """The folder that holds one unpacked Electron per version and target."""
    override = (os.environ.get(ENV_CACHE) or "").strip()
    if override:
        return Path(override).expanduser()
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "AutoCua" / "electron"
    if sys.platform == "win32":
        local = (os.environ.get("LOCALAPPDATA") or "").strip()
        return (Path(local) if local else Path.home() / "AppData" / "Local") / "AutoCua" / "electron"
    xdg = (os.environ.get("XDG_CACHE_HOME") or "").strip()
    return (Path(xdg) if xdg else Path.home() / ".cache") / "AutoCua" / "electron"


def install_dir(key):
    return cache_root() / f"v{VERSION}-{key}"


def _complete(folder, key):
    """The executable in `folder` when it holds a finished copy, else None."""
    exe = folder / EXECUTABLE[key.split("-")[0]]
    try:
        if (folder / "version").read_text(encoding="utf-8").strip() != VERSION:
            return None
    except OSError:
        return None
    return exe if exe.is_file() else None


def executable():
    """The unpacked Electron executable, or None when it is not here."""
    key = target()
    return _complete(install_dir(key), key) if key else None


def fetch_command(force=False):
    """The command that fetches Electron with the Python running now. A bare
    `python` is missing on a stock Mac, and may be another environment's."""
    tail = "-m AutoCua.utils.electron" + (" --force" if force else "")
    for name in ("python", "python3"):
        found = shutil.which(name)
        if found and os.path.normcase(os.path.abspath(found)) == \
                os.path.normcase(os.path.abspath(sys.executable)):
            return f"{name} {tail}"
    exe = sys.executable or "python"
    if os.name != "nt":
        return f"{shlex.quote(exe)} {tail}"
    # A quoted path only runs in PowerShell (the default shell of Windows
    # Terminal and VS Code) behind its call operator.
    quoted = subprocess.list2cmdline([exe])
    return f"& {quoted} {tail}" if quoted.startswith('"') else f"{quoted} {tail}"


def unsupported_reason():
    """Why Electron cannot run on this machine, known before anything is
    fetched, or None."""
    if target() is None:
        return f"Electron {VERSION} has no build for {sys.platform} {platform.machine()}"
    if sys.platform == "darwin":
        version = _macos_version()
        if version and version < MIN_MACOS:
            return (f"Electron {VERSION} needs macOS {'.'.join(map(str, MIN_MACOS))} "
                    f"or newer, this Mac runs {'.'.join(map(str, version))}")
    elif sys.platform == "win32":
        if sys.getwindowsversion().major < 10:
            return f"Electron {VERSION} needs Windows 10 or newer"
    elif sys.platform.startswith("linux") and "ELECTRON_DISABLE_SANDBOX" not in os.environ:
        # ELECTRON_DISABLE_SANDBOX is Electron's own switch for running
        # without the sandbox, honoured when it is set at all, even empty:
        # whoever set it has made that call. A Linux that keeps the sandbox
        # from starting is no reason: the window runs without it there
        # (sandbox_switches).
        if os.geteuid() == 0:
            return ("Electron will not run as root with its sandbox on "
                    "(ELECTRON_DISABLE_SANDBOX=1 turns the sandbox off)")
    return None


def _macos_version():
    """(major, minor), or None when unknown."""
    text = platform.mac_ver()[0]
    if text.startswith("10.16"):
        # What a Python built against an old SDK is told on every macOS from
        # 11 on. sw_vers is Apple's own and gets the real one.
        try:
            text = subprocess.run(["/usr/bin/sw_vers", "-productVersion"], capture_output=True,
                                  text=True, timeout=5).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            return None
    try:
        parts = tuple(int(p) for p in text.split(".")[:2]) + (0,)
    except ValueError:
        return None
    return None if parts[:2] == (10, 16) else parts[:2]


def sandbox_switches(exe):
    """Linux: ["--no-sandbox"] when Chromium's sandbox cannot start for the
    Electron at `exe`, after saying so and how to turn it on; [] when it can,
    and always on macOS and Windows, whose sandboxes need nothing from here.

    Chromium will not run without its sandbox, which on Linux needs a user
    namespace it can use, or its setuid helper (chrome-sandbox, next to the
    executable) owned by root. Ubuntu 24.04 and newer
    (kernel.apparmor_restrict_unprivileged_userns) let a program with no
    AppArmor profile make a user namespace but take every capability away in
    it, and a download made as the person cannot make the helper root's, so
    a fetched Electron aborts there. Chrome and VS Code get through on
    profiles Ubuntu ships, Cursor on its package's root-owned helper. A
    profile for this Electron would be a hole rather than a fix: it runs any
    app folder it is given, so every program on the machine could use it to
    get around the restriction. Without the sandbox the window is as
    protected as the pywebview one (WebKitGTK's sandbox is off there too),
    and it only shows the app's own pages, from 127.0.0.1. The switch goes
    on Electron's command line, not into ELECTRON_DISABLE_SANDBOX: main.js
    hands its environment to the backend, and on to all the agent starts."""
    if not sys.platform.startswith("linux") or "ELECTRON_DISABLE_SANDBOX" in os.environ:
        return []
    helper = Path(exe).parent / "chrome-sandbox"
    if _helper_ready(helper) or _userns_works():
        return []
    setting = _userns_setting()
    why = f" ({setting})" if setting else ""
    print(f"[ui] This Linux keeps Chromium's sandbox from starting for a downloaded "
          f"Electron{why}, so the window runs without it.", flush=True)
    if helper.is_file():
        path = shlex.quote(str(helper))
        print("[ui] To turn it on (sudo asks for your password; again after an update "
              "brings a new Electron):\n"
              f"[ui]   sudo chown root:root {path}\n"
              f"[ui]   sudo chmod 4755 {path}", flush=True)
    return ["--no-sandbox"]


def _helper_ready(helper):
    """True when the chrome-sandbox at `helper` can run Chromium's setuid
    sandbox: owned by root, setuid and executable by all (what Chromium
    checks), on a mount that honours setuid."""
    try:
        st = helper.stat()
        nosuid = os.statvfs(helper).f_flag & os.ST_NOSUID
    except OSError:
        return False
    return bool(st.st_uid == 0 and st.st_mode & stat.S_ISUID
                and st.st_mode & stat.S_IXOTH and not nosuid)


# Chromium's own test (Credentials::CanCreateProcessInNewUserNS), in a fresh
# interpreter: unshare(CLONE_NEWUSER) needs a process with one thread, and a
# child of this process runs under the AppArmor label Electron will, since no
# profile attaches to Python or to a downloaded Electron. Ubuntu's restriction
# lets the namespace be made, then refuses these writes in it.
_USERNS_PROBE = """
import ctypes, os
uid, gid = os.getuid(), os.getgid()
if ctypes.CDLL(None, use_errno=True).unshare(0x10000000):     # CLONE_NEWUSER
    raise SystemExit(1)
for name, text in (("setgroups", "deny"), ("gid_map", f"{gid} {gid} 1"),
                   ("uid_map", f"{uid} {uid} 1")):
    with open(f"/proc/self/{name}", "w") as f:
        f.write(text)
"""


def _userns_works():
    """True when a process started from here can set up a user namespace
    the way Chromium's sandbox does."""
    try:
        return subprocess.run([sys.executable, "-I", "-c", _USERNS_PROBE],
                              capture_output=True, timeout=15).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _userns_setting():
    """The kernel setting that restricts unprivileged user namespaces here,
    as "name = value", or None."""
    for path, blocked in (("/proc/sys/kernel/apparmor_restrict_unprivileged_userns", "1"),
                          ("/proc/sys/kernel/unprivileged_userns_clone", "0"),
                          ("/proc/sys/user/max_user_namespaces", "0")):
        try:
            value = Path(path).read_text(encoding="utf-8").strip()
        except OSError:
            continue
        if value == blocked:
            name = path[len("/proc/sys/"):].replace("/", ".")
            return f"{name} = {value}"
    return None


def _url(name):
    """The zip's URL: GitHub's release, or the mirror ELECTRON_MIRROR names,
    composed the way @electron/get (npm's Electron installer) composes it."""
    mirror = (os.environ.get("ELECTRON_MIRROR") or "").strip() or URL
    if not mirror.endswith("/"):
        mirror += "/"
    folder = (os.environ.get("ELECTRON_CUSTOM_DIR") or "").strip() or f"v{VERSION}"
    folder = folder.replace("{{ version }}", VERSION).replace("{{version}}", VERSION)
    return f"{mirror}{folder}/{name}"


def _source():
    """Where the zip comes from, in the words the person is asked in."""
    if not (os.environ.get("ELECTRON_MIRROR") or "").strip():
        return "Electron's GitHub releases"
    from urllib.parse import urlsplit
    url = urlsplit(_url("x"))
    return f"{url.scheme}://{url.netloc} (ELECTRON_MIRROR)"


def _about(key):
    size, _sha, unpacked = ZIPS[key]
    return (f"Electron {VERSION}: a {size / 1e6:.0f} MB download, once, from {_source()}, "
            f"{unpacked / 1e6:.0f} MB on disk in {cache_root()}")


# =============================================================================
# Asking
# =============================================================================
def _can_ask():
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


def _ask(question):
    """The answer to `question`, stripped and lowercased; "" for Enter and
    for the end of input, None when none came within ASK_TIMEOUT (a terminal
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
    deadline = time.monotonic() + ASK_TIMEOUT
    while reader.is_alive() and time.monotonic() < deadline:
        reader.join(0.25)          # short waits, so Ctrl+C still gets through
    if not answer:
        sys.stdout.write("\n")
        return None
    if not answer[0] and os.name == "nt":
        # A console read ends with nothing at Ctrl+Z Enter and at Ctrl+C,
        # whose CTRL_C_EVENT reaches this process just after the read
        # returned. Let it arrive here, inside offer_electron's try.
        time.sleep(0.5)            # a Ctrl+C interrupts this at once
    text = answer[0].decode(errors="replace")
    if not text and os.name != "nt":
        sys.stdout.write("\n")     # Ctrl+D left the cursor on the question
    return text.strip().lower()


def offer_electron():
    """The Electron executable when it is here or the person, asked on the
    terminal, says to fetch it now. None, with the reason printed, otherwise:
    it cannot run on this machine, there is no terminal to ask on, they said
    no or not now (now, or "n" on an earlier launch), or the fetch failed."""
    exe = executable()
    if exe is not None:
        return exe
    reason = unsupported_reason()
    if reason:
        print(f"[ui] The Chromium window cannot run here: {reason}.", flush=True)
        return None
    command = fetch_command()
    _tidy()
    if (cache_root() / DECLINED).is_file():
        print(f"[ui] The Chromium window is not installed; `{command}` installs it.",
              flush=True)
        return None
    about = _about(target())
    if not _can_ask():
        print(f"[ui] The Chromium window needs {about}; `{command}` installs it.", flush=True)
        return None
    print(f"[ui] The Chromium window needs {about}.", flush=True)
    try:
        answer = _ask("[ui] Download it now? y = yes, n = no and don't ask again, "
                      "Enter = not now (the app opens in the pywebview window): ")
    except KeyboardInterrupt:
        # Ctrl+C skips the question, as it skips the download below.
        answer = ""
        print(flush=True)
    if answer is None:
        print(f"[ui] No answer in {ASK_TIMEOUT} seconds, so not now; the next launch "
              "asks again.", flush=True)
        return None
    if answer in ("n", "no"):
        try:
            cache_root().mkdir(parents=True, exist_ok=True)
            (cache_root() / DECLINED).write_text(f"{VERSION}\n", encoding="utf-8")
        except OSError as e:
            print(f"[ui] Could not save that answer ({e.strerror}: {e.filename}); the next "
                  "launch asks again.", flush=True)
            return None
        print(f"[ui] Not asking again; `{command}` installs it any time.", flush=True)
        return None
    if answer not in ("y", "yes"):
        print("[ui] Not now; the next launch asks again.", flush=True)
        return None
    try:
        return ensure_electron()
    except KeyboardInterrupt:
        # Ctrl+C on a slow download: skip it and open the app anyway.
        print(f"[ui] Download stopped, nothing installed; `{command}` installs it "
              "any time.", flush=True)
        return None


# =============================================================================
# Fetching
# =============================================================================
class _Progress:
    """Megabytes so far: in place on a terminal, every quarter otherwise."""

    def __init__(self, total):
        self.total = total
        self.tty = bool(sys.stdout and sys.stdout.isatty())
        self.at = 0.0
        self.quarter = 0
        self.shown = False

    def __call__(self, got):
        if self.tty:
            now = time.monotonic()
            if now - self.at >= 0.25 or got >= self.total:
                self.at = now
                self.shown = True
                sys.stdout.write(f"\r[ui]   {got / 1e6:6.1f} / {self.total / 1e6:.1f} MB")
                sys.stdout.flush()
        elif got * 4 // self.total > self.quarter:
            self.quarter = got * 4 // self.total
            print(f"[ui]   {got / 1e6:.1f} / {self.total / 1e6:.1f} MB", flush=True)

    def end(self):
        if self.shown:
            sys.stdout.write("\n")
            sys.stdout.flush()


def _os_store_session():
    """A requests session that trusts the OS's certificates instead of
    certifi's: antivirus HTTPS scanning and company proxies re-sign with a
    root only the OS has. The digest vouches for the bytes either way."""
    import ssl
    import requests
    from requests.adapters import HTTPAdapter
    try:
        import truststore           # the macOS keychain / Windows store, when installed
        context = truststore.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    except ImportError:
        context = ssl.create_default_context()   # loads the Windows store; Linux's bundle

    class _OSStore(HTTPAdapter):
        def init_poolmanager(self, *args, **kwargs):
            kwargs["ssl_context"] = context
            return super().init_poolmanager(*args, **kwargs)

        def proxy_manager_for(self, proxy, **proxy_kwargs):
            proxy_kwargs["ssl_context"] = context
            return super().proxy_manager_for(proxy, **proxy_kwargs)

    session = requests.Session()
    session.mount("https://", _OSStore())
    return session


def _download(url, dest, size, sha):
    """Write the zip at `url` to `dest`. Raises unless it is exactly `size`
    bytes with SHA-256 `sha`."""
    import requests      # certifi's roots: a python.org Python on macOS has none of its own

    try:
        _stream(requests.Session(), url, dest, size, sha)
    except requests.exceptions.SSLError:
        _stream(_os_store_session(), url, dest, size, sha)


def _stream(session, url, dest, size, sha):
    digest = hashlib.sha256()
    got = 0
    progress = _Progress(size)
    try:
        # The read timeout bounds a network that stalls; it is per read, not
        # for the whole download.
        with session, session.get(url, stream=True, timeout=(15, 60),
                                  headers={"User-Agent": "AutoCua"}) as r:
            r.raise_for_status()
            with open(dest, "wb") as out:
                for chunk in r.iter_content(1 << 20):
                    got += len(chunk)
                    if got > size:
                        raise ValueError(f"more than the pinned {size} bytes")
                    out.write(chunk)
                    digest.update(chunk)
                    progress(got)
    finally:
        progress.end()
    if got != size or digest.hexdigest() != sha:
        raise ValueError(f"it did not match its pin ({got} bytes, sha256 "
                         f"{digest.hexdigest()[:12]}...; expected {size} bytes, {sha[:12]}...)")


def _unpack(archive, dest):
    """Unpack the zip into `dest`, symlinks and exec bits included."""
    if sys.platform == "darwin":
        # ditto keeps what zipfile drops: Electron.app's frameworks hang
        # together by symlinks (Versions/Current -> A, ...), which zipfile
        # writes as small files holding the target's name, and every binary
        # needs its exec bit. The app would not start.
        result = subprocess.run(["/usr/bin/ditto", "-x", "-k", str(archive), str(dest)],
                                capture_output=True, text=True, timeout=600)
        if result.returncode != 0:
            raise OSError(f"ditto failed: {result.stderr.strip() or result.returncode}")
        # Electron's Mac builds are not notarized, so Gatekeeper calls a
        # quarantined copy damaged and will not run it. Python's download sets
        # no quarantine, but a terminal app that quarantines what its children
        # write would; the zip's digest is what vouches for these files.
        subprocess.run(["/usr/bin/xattr", "-dr", "com.apple.quarantine", str(dest)],
                       capture_output=True, timeout=120)
        return
    with zipfile.ZipFile(archive) as zf:
        for info in zf.infolist():
            mode = info.external_attr >> 16
            if stat.S_ISLNK(mode):
                # Only the macOS zips hold links; zipfile would write this one
                # as a file. A re-pin that brings one here must handle it.
                raise OSError(f"{info.filename} is a symlink")
            written = zf.extract(info, dest)      # names are sanitised here
            # Linux: the executables' 0755 (zipfile writes every file 0644).
            if os.name == "posix" and mode & 0o777 and not info.is_dir():
                os.chmod(written, mode & 0o777)


class _FetchLock:
    """LOCK_FILE in `root`, held by one process at a time (see LOCK_FILE).
    The OS lets go of it when its holder dies, however it dies. On a
    filesystem that cannot lock (some network and VM shares) it is taken
    without locking anything: two fetches at once there each download, as
    they would with no lock at all."""

    def __init__(self, root):
        self.path = root / LOCK_FILE
        self.fd = None
        self.locked = False

    def acquire(self, wait=True):
        """True once held; False without waiting when another holds it."""
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT, 0o600)
        locked = True
        try:
            if os.name == "nt":
                import errno
                import msvcrt
                while True:
                    try:
                        os.lseek(fd, 0, os.SEEK_SET)
                        msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
                        break
                    except OSError as e:
                        if e.errno not in (errno.EACCES, errno.EDEADLK):
                            locked = False             # cannot lock here
                            break
                        if not wait:
                            os.close(fd)
                            return False
                        time.sleep(0.5)
            else:
                import fcntl
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | (0 if wait else fcntl.LOCK_NB))
                except BlockingIOError:
                    os.close(fd)
                    return False
                except OSError:                        # ENOLCK, EOPNOTSUPP: cannot lock here
                    locked = False
        except BaseException:
            os.close(fd)
            raise
        self.fd = fd
        self.locked = locked
        return True

    def release(self):
        if self.fd is None:
            return
        try:
            if os.name == "nt" and self.locked:
                import msvcrt
                os.lseek(self.fd, 0, os.SEEK_SET)
                msvcrt.locking(self.fd, msvcrt.LK_UNLCK, 1)
        except OSError:
            pass
        finally:
            os.close(self.fd)
            self.fd = None


def _sweep(root):
    """Delete what fetches that were killed left in `root`. Only with the
    fetch lock held, so no live fetch owns any of it."""
    try:
        entries = list(root.iterdir())
    except OSError:
        return
    for p in entries:
        if not (p.name.startswith(".") and ".incoming." in p.name):
            continue
        if p.is_dir() and not p.is_symlink():
            shutil.rmtree(p, ignore_errors=True)
        else:
            _discard(p)


def _tidy():
    """_sweep, when no fetch is running: a fetch killed outright leaves up to
    500 MB behind, and the person may never say yes again to sweep it."""
    root = cache_root()
    if not root.is_dir():
        return
    lock = _FetchLock(root)
    try:
        if lock.acquire(wait=False) and lock.locked:
            _sweep(root)
    except OSError:
        pass
    finally:
        lock.release()


def _drop_older_versions(root, key, tag):
    """Delete this target's copies of older Electron versions: a re-pin
    would leave 300-400 MB behind each time. Only older ones, so two installs
    of AutoCua pinned to different versions never delete each other's in
    turn. On Windows one that is running is left alone (it cannot be
    renamed); on macOS and Linux it goes too, which only matters while an
    older AutoCua's window is open during the newer one's first fetch."""
    def parse(name):
        try:
            return tuple(int(p) for p in name[1:-len(key) - 1].split("."))
        except ValueError:
            return None

    current = parse(f"v{VERSION}-{key}")
    for p in root.glob(f"v*-{key}"):
        version = parse(p.name)
        if version is None or version >= current or not p.is_dir() or p.is_symlink():
            continue
        aside = root / f".{p.name}.{tag}.old"
        try:
            os.rename(p, aside)
        except OSError:
            continue
        shutil.rmtree(aside, ignore_errors=True)


def _discard(path):
    try:
        path.unlink()
    except OSError:
        pass


def ensure_electron(force=False):
    """The Electron executable, fetched and unpacked once if it is not here,
    without asking. force fetches it again over the copy that is here, and
    also where unsupported_reason() says it cannot run (the person has set
    that up). None, with the reason printed, when it cannot be had."""
    key = target()
    if not force:
        exe = executable()
        if exe is not None:
            return exe
    reason = unsupported_reason()
    if key is None or (reason and not force):
        print(f"[ui] The Chromium window cannot run here: {reason}.", flush=True)
        return None

    with _LOCK:
        root = cache_root()
        lock = _FetchLock(root)
        try:
            root.mkdir(parents=True, exist_ok=True)
            if not lock.acquire(wait=False):
                print("[ui] Another AutoCua is fetching Electron; waiting for it...",
                      flush=True)
                lock.acquire()
        except OSError as e:
            print(f"[ui] Could not fetch Electron {VERSION} ({type(e).__name__}: {e}).",
                  flush=True)
            return None
        try:
            # The one it waited for may have fetched it.
            exe = None if force else executable()
            return exe if exe is not None else _fetch(root, key, force, lock.locked)
        finally:
            lock.release()


def _fetch(root, key, force, locked):
    """Download, check, unpack and put in place, with the fetch lock held
    (`locked`: really held, so what '.incoming.' names hold is dead)."""
    size, sha, _unpacked = ZIPS[key]
    name = f"electron-v{VERSION}-{key}.zip"
    final = install_dir(key)
    tag = f"incoming.{os.getpid()}"
    archive = root / f".{name}.{tag}"
    staging = root / f".{final.name}.{tag}"
    aside = root / f".{final.name}.{tag}.old"
    retry = fetch_command(force)
    if locked:
        _sweep(root)
    if final.exists() or final.is_symlink():
        # A copy to replace (--force), or what a cache cleaner left of one.
        # Moved aside before the download, as Windows will not move it while
        # it runs, and put back below unless a new copy took its place.
        try:
            os.rename(final, aside)
        except OSError as e:
            print(f"[ui] Could not replace {final} ({type(e).__name__}: {e}). "
                  f"Close AutoCua, then `{retry}`.", flush=True)
            return None
    print(f"[ui] Fetching the Chromium window: Electron {VERSION} for {key}, "
          f"{size / 1e6:.0f} MB from {_source()}...", flush=True)
    placed = False
    try:
        _download(_url(name), archive, size, sha)
        _unpack(archive, staging)
        if not _complete(staging, key):
            raise OSError(f"{name} did not hold a complete Electron")
        for attempt in range(10):
            try:
                os.rename(staging, final)
                break
            except OSError:
                # Windows: an antivirus still scanning the new executables
                # holds them open for a moment.
                if attempt == 9:
                    raise
                time.sleep(0.5)
        placed = executable() is not None
        if not placed:
            raise OSError(f"{final} is not complete after unpacking")
    except Exception as e:
        print(f"[ui] Could not fetch Electron {VERSION} ({type(e).__name__}: {e}). "
              f"`{retry}` tries again.", flush=True)
        return None
    finally:
        _discard(archive)
        shutil.rmtree(staging, ignore_errors=True)
        if placed:
            shutil.rmtree(aside, ignore_errors=True)
        elif aside.exists() and not final.exists():
            try:
                os.rename(aside, final)     # the copy that was here, back
            except OSError:
                pass
    _drop_older_versions(root, key, tag)
    _discard(root / DECLINED)
    print(f"[ui] Electron {VERSION} is ready in {final}", flush=True)
    return executable()


def main(argv=None):
    """`python -m AutoCua.utils.electron [--force]`: fetch Electron now (this
    command is the yes the app asks for) and print where it is."""
    # Outside the app's process stdout keeps the console's code page (cp1252
    # when piped on Windows), and a path can hold anything.
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(errors="replace")
        except (AttributeError, ValueError):
            pass
    args = sys.argv[1:] if argv is None else list(argv)
    if any(a != "--force" for a in args):
        unknown = [a for a in args if a not in ("-h", "--help", "--force")]
        if unknown:
            print(f"Unknown argument: {' '.join(unknown)}\n")
        key = target()
        what = (f"the official release zip for this machine ({_about(key)})" if key
                else f"nothing: {unsupported_reason()}")
        print(f"Usage: {fetch_command()} [--force]\n\n"
              f"Downloads Electron {VERSION}, which runs AutoCua's Chromium window:\n"
              f"{what}.\nThe zip is checked against a pinned SHA-256 before it is "
              "unpacked. Running this is the\nyes the app asks for on its first launch.\n\n"
              "  --force   download it again over the copy that is there (close AutoCua\n"
              "            first), and also where the app thinks it cannot run (you have\n"
              "            set that up)")
        return 2 if unknown else 0
    try:
        exe = ensure_electron(force="--force" in args)
    except KeyboardInterrupt:
        print("\n[ui] Stopped; nothing was installed.", flush=True)
        return 130
    if exe is None:
        return 1
    print(exe)
    return 0


if __name__ == "__main__":
    sys.exit(main())
