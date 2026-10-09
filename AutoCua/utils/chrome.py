"""Google Chrome, the web agent's browser: find it, or install it.

The web agent drives Chrome in both of its modes: the person's own Chrome
through the AutoCua extension for a normal run, and a Chrome of its own on a
debugging port for a headless or parallel run. This is the one-time setup
step for it, in the same spirit as rust.py (the extension) and wda.py
(WebDriverAgent): the first web run on a machine without Chrome downloads
Google's own installer and puts Chrome where the browser lookup (browser.rs,
find_chrome) expects it. When that cannot be done, the lookup still falls
back to a Chromium or Edge that is installed, and the reason is printed.

Where Chrome lands, matching browser.rs:
  macOS    /Applications/Google Chrome.app, or ~/Applications when
           /Applications is not writable (Google's universal DMG, copied
           with ditto)
  Windows  %LOCALAPPDATA%\\Google\\Chrome\\Application\\chrome.exe (Google's
           installer run as this user, silent: a per-user install when the
           process is not an administrator's) or under Program Files
  Linux    /opt/google/chrome/chrome, Google's own package (.deb or .rpm,
           x86_64 only: Google ships no Linux build for ARM). A package
           needs root, so the person is asked for their password once (the
           desktop's own dialog, or the terminal), unless root needs none
           (a container, a cloud image's sudo).

The downloads are Google's stable direct links, which do not change per
version, so nothing here needs updating.
"""

import os
import platform
import shutil
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

MAC_DMG = "https://dl.google.com/chrome/mac/universal/stable/GGRO/googlechrome.dmg"
WIN_INSTALLER = "https://dl.google.com/chrome/install/latest/chrome_installer.exe"
LINUX_DEB = "https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb"
LINUX_RPM = "https://dl.google.com/linux/direct/google-chrome-stable_current_x86_64.rpm"
MAC_APP = "Google Chrome.app/Contents/MacOS/Google Chrome"
WIN_EXE = r"Google\Chrome\Application\chrome.exe"
# Set once this process has tried to install Chrome, so a failed or declined
# install is not tried again on its next run, nor by the children a parallel
# run starts (they inherit the environment).
TRIED = "AutoCua_CHROME_INSTALL_TRIED"
# How long the Linux password dialog waits for an answer before the run goes
# on without Chrome: a run started from Telegram may have nobody at the screen.
PROMPT_WAIT = 300
LINUX_BY_HAND = "Install it once from https://www.google.com/chrome/ (the .deb or .rpm for your system)."


def _candidates():
    """Every place Chrome may be, in the order browser.rs checks them."""
    if sys.platform == "darwin":
        return [Path("/Applications") / MAC_APP, Path.home() / "Applications" / MAC_APP]
    if sys.platform == "win32":
        roots = [os.environ.get(k) for k in ("ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA")]
        return [Path(r) / WIN_EXE for r in roots if r]
    found = [Path(p) for p in (shutil.which("google-chrome"), shutil.which("google-chrome-stable")) if p]
    return [Path("/opt/google/chrome/chrome")] + found + [Path("/usr/bin/google-chrome")]


def find_chrome():
    """Chrome's executable as a string, or None when it is not installed."""
    for p in _candidates():
        if p.is_file():
            return str(p)
    return None


def _say(text):
    """print, dropping the globe where the stream cannot encode it. On Windows
    a pipe or a log file is cp1252 (the web fallback's child writes to one),
    and the UnicodeEncodeError would stop the install before it began."""
    try:
        print(text, flush=True)
    except UnicodeEncodeError:
        print(text.replace("🌐 ", ""), flush=True)


def ensure_chrome():
    """Chrome's executable, installing Chrome first when it is missing. Never
    raises: the web agent falls back to a Chromium or Edge that is there, and
    this says why."""
    found = find_chrome()
    if found or os.environ.get(TRIED):
        return found
    os.environ[TRIED] = "1"
    _say("🌐 Google Chrome is not installed. Downloading it for the web agent "
         "(about 280 MB, once)...")
    try:
        path = install_chrome()
    except Exception as e:
        _say(f"🌐 Could not install Chrome: {e}. Install it from https://www.google.com/chrome/ "
             "(Chromium or Edge is used if one is installed).")
        return None
    if path:
        _say(f"🌐 Chrome installed: {path}")
    else:
        _say("🌐 Chrome did not install. Install it from https://www.google.com/chrome/.")
    return path


def install_chrome():
    """Download and install Chrome for this platform; returns the executable."""
    if sys.platform == "darwin":
        return _install_mac()
    if sys.platform == "win32":
        return _install_windows()
    return _install_linux()


def _download(url, dest):
    """Stream `url` to `dest`, saying how far it has got every tenth.

    Python's own certificate store can be empty (the python.org builds on macOS
    until their certificate script is run; measured here), so the bundle certifi
    carries is used when it is installed, as the app's HTTP libraries use it, and
    the system's curl takes over when Python cannot verify the server at all."""
    try:
        import certifi
        context = ssl.create_default_context(cafile=certifi.where())
    except ImportError:
        context = ssl.create_default_context()
    req = urllib.request.Request(url, headers={"User-Agent": "AutoCua"})
    try:
        with urllib.request.urlopen(req, timeout=60, context=context) as r, open(dest, "wb") as out:
            total = int(r.headers.get("Content-Length") or 0)
            done, shown = 0, 0
            while True:
                chunk = r.read(1 << 20)
                if not chunk:
                    break
                out.write(chunk)
                done += len(chunk)
                if total and done * 10 // total > shown:
                    shown = done * 10 // total
                    print(f"   {shown * 10}% of {total // (1 << 20)} MB", flush=True)
        return
    except urllib.error.URLError as e:
        if not isinstance(e.reason, ssl.SSLError) or not shutil.which("curl"):
            raise
    print("   (Python cannot verify the download server; using curl)", flush=True)
    subprocess.run(["curl", "-fsSL", "-A", "AutoCua", "-o", str(dest), url], check=True, timeout=1800)


def _install_mac():
    # One universal DMG serves Intel and Apple silicon. AutoCua_CHROME_INSTALL_DIR:
    # where the app is copied (tests point it at a scratch folder); otherwise
    # /Applications, or ~/Applications when that is not writable.
    root = os.environ.get("AutoCua_CHROME_INSTALL_DIR")
    if root:
        root = Path(root)
    elif os.access("/Applications", os.W_OK):
        root = Path("/Applications")
    else:
        root = Path.home() / "Applications"
    root.mkdir(parents=True, exist_ok=True)
    dest = root / "Google Chrome.app"
    with tempfile.TemporaryDirectory() as tmp:
        dmg = Path(tmp) / "googlechrome.dmg"
        _download(MAC_DMG, dmg)
        mount = Path(tmp) / "mnt"
        subprocess.run(["hdiutil", "attach", "-nobrowse", "-quiet", "-mountpoint", str(mount), str(dmg)],
                       check=True)
        try:
            # ditto keeps the signature and attributes the DMG's copy carries.
            subprocess.run(["ditto", str(mount / "Google Chrome.app"), str(dest)], check=True)
        finally:
            subprocess.run(["hdiutil", "detach", "-quiet", str(mount)], check=False)
    exe = dest / "Contents/MacOS/Google Chrome"
    return str(exe) if exe.is_file() else None


def _install_windows():
    # ignore_cleanup_errors: Windows cannot delete a file that is still open,
    # and the installer's helper or an antivirus scan can hold the setup file
    # for a moment after it exits. Chrome is installed by then; that must not
    # turn into "Could not install Chrome".
    with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as tmp:
        exe = Path(tmp) / "chrome_installer.exe"
        _download(WIN_INSTALLER, exe)
        # Google's installer, silent. Run as this user it installs for this
        # user, into %LOCALAPPDATA%, with no administrator prompt; run from an
        # administrator's process it installs under Program Files. It hands
        # off to a helper and can return before the files are all there, so
        # wait for them. Written from Google's documented switches, not run.
        print("   Installing...", flush=True)
        r = subprocess.run([str(exe), "/silent", "/install"], timeout=900)
        for _ in range(120 if r.returncode == 0 else 5):
            found = find_chrome()
            if found:
                return found
            time.sleep(1)
    if r.returncode:
        # Its codes are HRESULTs, which read best in hex.
        raise RuntimeError(f"the Chrome installer failed with code {r.returncode:#x}")
    return None


def _pkexec(argv):
    """argv as root through pkexec, which asks with the desktop's own password
    dialog; its exit code. 126 when the dialog was closed, or went unanswered
    for PROMPT_WAIT; 127 when there is no one to ask (no polkit agent) or the
    password was refused."""
    p = subprocess.Popen(["pkexec", *argv])
    asked = time.monotonic()
    while p.poll() is None:
        time.sleep(0.2)
        if time.monotonic() - asked < PROMPT_WAIT:
            continue
        # While the process is still pkexec, the dialog is up and this user
        # may end it. Once the password is given it becomes root's package
        # tool, and the install is waited for.
        try:
            if Path(f"/proc/{p.pid}/comm").read_text().strip() == "pkexec":
                p.kill()
                p.wait()
                return 126
        except OSError:
            pass  # answered just now: the install runs, and is waited for
    return p.returncode


def _as_root(argv):
    """Run argv as root; its exit code, or None when root cannot be had.
    Silent where no one need type a password: root itself (a container), or
    sudo set to pass (a cloud image's user). Otherwise the person is asked
    once: with the desktop's own dialog when there is a screen (the desktop
    app is started from a terminal nobody watches), else in the terminal."""
    if os.geteuid() == 0:
        return subprocess.run(argv).returncode
    sudo = shutil.which("sudo")
    if sudo and subprocess.run([sudo, "-n", "true"], capture_output=True).returncode == 0:
        return subprocess.run([sudo, "-n", *argv]).returncode
    if shutil.which("pkexec") and (os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY")):
        _say("🌐 Installing Chrome needs your password once, in the dialog on screen.")
        code = _pkexec(argv)
        if code != 127:
            return code
    if sudo and sys.stdin.isatty():
        _say("🌐 Installing Chrome needs your password once.")
        return subprocess.run([sudo, *argv]).returncode
    return None


def _install_linux():
    # Google's own package: it adds Google's repository as it installs, so
    # Chrome updates with the system, and its path is the one the sandbox
    # rules allow (a copy under the home folder is no use on Ubuntu 23.10 and
    # later, measured for Brave on 26.04). x86_64 only: Google ships no Linux
    # build for ARM, and the lookup then falls back to a Chromium.
    if platform.machine() not in ("x86_64", "AMD64"):
        raise RuntimeError(f"Google ships no Linux Chrome for {platform.machine()}; install Chromium instead")
    with tempfile.TemporaryDirectory() as tmp:
        if shutil.which("apt-get"):
            pkg = Path(tmp) / "google-chrome-stable_current_amd64.deb"
            _download(LINUX_DEB, pkg)
            argv = ["apt-get", "install", "-y", str(pkg)]
        elif shutil.which("dnf"):
            pkg = Path(tmp) / "google-chrome-stable_current_x86_64.rpm"
            _download(LINUX_RPM, pkg)
            argv = ["dnf", "install", "-y", str(pkg)]
        elif shutil.which("zypper"):
            pkg = Path(tmp) / "google-chrome-stable_current_x86_64.rpm"
            _download(LINUX_RPM, pkg)
            argv = ["zypper", "--non-interactive", "install", "--allow-unsigned-rpm", str(pkg)]
        else:
            raise RuntimeError(f"no apt-get, dnf or zypper on this system. {LINUX_BY_HAND}")
        code = _as_root(argv)
    found = find_chrome()
    if found:
        return found
    if code is None:
        raise RuntimeError(f"it needs administrator rights on this system. {LINUX_BY_HAND}")
    if code == 126:
        raise RuntimeError(f"the password dialog was closed. {LINUX_BY_HAND}")
    if code:
        raise RuntimeError(f"the package tool exited with code {code}. {LINUX_BY_HAND}")
    return None


if __name__ == "__main__":
    print(ensure_chrome() or "no Chrome")
