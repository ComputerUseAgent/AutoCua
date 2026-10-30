"""Android emulators for "mobile use, android" with device="simulation", the
default: Google's emulator, a Google APIs system image per Android version,
and an AVD (the emulator's virtual phone or tablet) that this file writes
itself, so no Java, Android Studio or sdkmanager is needed.

    emulator = EmulatorSession()
    emulator.activate("16", device="tablet")   # boots it: {"ok", "serial", ...}
    ...                                        # android_session drives it like a phone
    emulator.deactivate()                      # adb emu kill

    python -m AutoCua.android_connector.emulator [version] [phone|tablet]
                                               # the precheck, and everything a run
                                               # needs, done ahead of time (no
                                               # version: the newest one already
                                               # here, else the newest)

The emulator and the images come from Google's SDK repository, the one
Android Studio uses, the first time they are wanted, into a per-user folder
beside Electron's:

    macOS     ~/Library/Caches/AutoCua/android-sdk/
    Windows   %LOCALAPPDATA%/AutoCua/android-sdk/
    Linux     ${XDG_CACHE_HOME:-~/.cache}/AutoCua/android-sdk/

They are Google's, under Google's Android SDK terms (TERMS_URL), so nothing
of them is in this repository or the wheel, and before the first download the
person accepts the terms once, on the terminal, as Android Studio's
sdkmanager asks (ACCEPT_ENV says yes with no terminal, for CI). Each zip is
pinned by its release, byte size and SHA-1, the checksum Google's manifest
lists and sdkmanager checks. The emulator is about 420 MB, one Android
version about 2 GB (6 GB unpacked). An Android SDK that already has the
emulator and that version's image (Android Studio's: ANDROID_HOME, or its
default folder) is used as it is, with nothing fetched and nothing asked. The
AVDs always live in our folder's avd/ (ANDROID_AVD_HOME), so the user's own
are never touched.

One AVD per Android version and kind: AutoCua_Android_17_phone,
AutoCua_Android_17_tablet... prepare() makes it: the emulator and the image,
the AVD, and its FIRST BOOT, once: a normal boot to the home screen, then a
stop that saves that moment. Every run, single or parallel, then starts a
copy of it with -read-only, from that saved moment: a fresh phone, Google's
apps and nothing else, that keeps nothing (a tester's device). It is ready in
about 5 s, 12 s when several start at once (a cold boot is 15 to 25 s on
Apple Silicon), stops in a second, and any number of copies can run at once.

The emulator needs the computer's hypervisor: built into macOS, Windows
Hypervisor Platform on Windows, KVM on Linux. Google builds it for macOS
(Apple Silicon and Intel), Windows x64 and Linux x64 only.
"""

import atexit
import errno
import os
import platform
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

from AutoCua.android_connector.adb import _adb, _download, _unzip, adb_path, devices

# Google's terms for the emulator and the images, accepted once before the
# first download (the file ACCEPTED in our folder remembers it), or by this
# environment variable where nobody can answer.
TERMS_URL = "https://developer.android.com/studio/terms"
ACCEPTED = "accepted-sdk-terms"
ACCEPT_ENV = "AutoCua_ANDROID_ACCEPT_SDK_TERMS"

# Pinned emulator release, the stable channel. Recorded 2026-10-29 from
# Google's SDK manifest (repository2-3.xml): zip, bytes and SHA-1 per
# (sys.platform, CPU). A re-pin means updating the version and all four.
EMULATOR_VERSION = "37.2.12"
REPOSITORY = "https://dl.google.com/android/repository/"
EMULATOR_ARCHIVES = {
    ("darwin", "arm64"): ("emulator-darwin_aarch64-16428233.zip", 416112708,
                          "3af4fe44ce82b3d88ae5678a53735f27ad729c15"),
    ("darwin", "x86_64"): ("emulator-darwin_x64-16428233.zip", 488731099,
                           "0c08fc22f41f2e63976f5503d593f821ff11e30b"),
    ("linux", "x86_64"): ("emulator-linux_x64-16428233.zip", 349654171,
                          "cd7362ea55dfb86a418958138dc396e74165dd01"),
    ("win32", "x86_64"): ("emulator-windows_x64-16428233.zip", 455342868,
                          "488ed747e82de7e9bb5247becd1ac043c7e5e85d"),
}

# Android version -> the image's API folder and, per CPU, its zip, bytes and
# SHA-1. Google APIs images (Google's apps and services, no Play Store),
# recorded 2026-10-29 from sys-img/google_apis/sys-img2-3.xml. Every one
# runs on EMULATOR_VERSION (Android 17's needs 37.2.11 or newer).
IMAGES_URL = REPOSITORY + "sys-img/google_apis/"
IMAGES = {
    "17": ("android-37.0", {
        "arm64-v8a": ("arm64-v8a-37.0_r08.zip", 2104098168, "7a8bfff584798fe87caa4d4883f6ade4658a185b"),
        "x86_64": ("x86_64-37.0_r08.zip", 2143899425, "a266cfbcf4d6c769b166c0af4d68eff0e4f4a5e3")}),
    "16": ("android-36", {
        "arm64-v8a": ("arm64-v8a-36_r07.zip", 1872691175, "5a99183b6d924da606260e45fd41a3eb8eca6eb7"),
        "x86_64": ("x86_64-36_r07.zip", 1895447397, "c6bf44bdcd885bb902b4ba752d111a073ad7a817")}),
    "15": ("android-35", {
        "arm64-v8a": ("arm64-v8a-35_r09.zip", 1778933980, "16f5bceca236b2737008977c4aaf826e46a8de7d"),
        "x86_64": ("x86_64-35_r09.zip", 1738815903, "0103e6dab21290c4b9d16550a3ce99476f884eef")}),
    "14": ("android-34", {
        "arm64-v8a": ("arm64-v8a-34_r14.zip", 1610393229, "2fe8b46d419a3400e30f31b0152b241b50c8b99f"),
        "x86_64": ("x86_64-34_r14.zip", 1563721130, "e0f6c9a0691aa27bd597d0deb1bcfdc943ac8ca7")}),
    "13": ("android-33", {
        "arm64-v8a": ("arm64-v8a-33_r17.zip", 1761582980, "47516ccb88230c42965940a929f51053562ee351"),
        "x86_64": ("x86_64-33_r17.zip", 1707857511, "2b96f5bd5c79bfe1cc645e70b3e630b5755d9711")}),
    "12": ("android-31", {
        "arm64-v8a": ("arm64-v8a-31_r11.zip", 1415899687, "21bcb00f4ac3dc91c4040da7980b8c5f3c681781"),
        "x86_64": ("x86_64-31_r14.zip", 1470822277, "9aedd3e85cad7a479146f6858f4a94840c2a3f29")}),
    "11": ("android-30", {
        "arm64-v8a": ("arm64-v8a-30_r16.zip", 1244324730, "0e66987d6b4db2e278af83d453ce5d74a7e6ced3"),
        "x86_64": ("x86_64-30_r16.zip", 1438186618, "6ae21030eaadc041078444d3798e4b399f3e787d")}),
    # No Android 10: its image's graphics memory won't let the software
    # encoder read the screen (gralloc_ranchu "usage mismatch"), so scrcpy's
    # server sends no video. A real Android 10 phone is fine.
}
NEWEST = max(IMAGES, key=int)

# The two kinds: screen width, height and density, the way it first stands,
# and the hardware name the AVD shows (Android Studio's Pixel 9 and Pixel Tablet).
PROFILES = {
    "phone": (1080, 2400, 420, "portrait", "pixel_9"),
    "tablet": (2560, 1600, 320, "landscape", "pixel_tablet"),
}

# A cold boot took 15 to 25 s on Apple Silicon; slower machines, and a first
# boot under load, take minutes.
BOOT_WAIT = 300
# A stopping emulator saves its quick-boot state first, about 9 s.
STOP_WAIT = 60
# adb finds emulators on console ports 5554, 5556 ... 5584, each with its adb
# port right after it. The serial is emulator-<console port>.
FIRST_PORT, LAST_PORT = 5554, 5584

# What to turn on when the emulator can't use the hypervisor.
ACCEL_HINTS = {
    "win32": "Turn on Windows Hypervisor Platform (Control Panel > Programs > Turn Windows features "
             "on or off), then restart the computer.",
    "linux": "Give this user KVM: install qemu-kvm, run sudo usermod -aG kvm $USER, then log out and "
             "back in. In a virtual machine, turn on nested virtualization.",
}

def _say(line):
    print(line, flush=True)


_LOCK = threading.RLock()       # one fetch, one AVD and one first boot at a time, however many boots ask
_PORTS = set()                  # console ports this process has claimed
_PORTS_LOCK = threading.Lock()
_DECLINED = []                  # the terms declined in this process: asked once, not per task
_ACCEL = {}                     # emulator -> what its hypervisor check said, once per process


def cache_root():
    """Our own SDK folder: the emulator, the images, the AVDs and their logs."""
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "AutoCua" / "android-sdk"
    if sys.platform == "win32":
        local = (os.environ.get("LOCALAPPDATA") or "").strip()
        return (Path(local) if local else Path.home() / "AppData" / "Local") / "AutoCua" / "android-sdk"
    xdg = (os.environ.get("XDG_CACHE_HOME") or "").strip()
    return (Path(xdg) if xdg else Path.home() / ".cache") / "AutoCua" / "android-sdk"


def avd_home():
    return cache_root() / "avd"


def _host():
    """(sys.platform, CPU), as EMULATOR_ARCHIVES keys it. A Python running
    under Rosetta on Apple Silicon still gets the arm64 emulator: an Intel one
    can't use the hypervisor there."""
    cpu = platform.machine().lower()
    if sys.platform == "darwin" and cpu == "x86_64":
        arm = subprocess.run(["sysctl", "-n", "hw.optional.arm64"], capture_output=True, text=True)
        if arm.stdout.strip() == "1":
            cpu = "arm64"
    return sys.platform, {"amd64": "x86_64", "aarch64": "arm64"}.get(cpu, cpu)


def _exe(sdk):
    return Path(sdk) / "emulator" / ("emulator.exe" if sys.platform == "win32" else "emulator")


def _image_dir(sdk, api, abi):
    return Path(sdk) / "system-images" / api / "google_apis" / abi


def _revision(folder):
    """Pkg.Revision from a package's source.properties, "" if it has none."""
    try:
        text = (Path(folder) / "source.properties").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    found = re.search(r"^Pkg\.Revision=(.+)$", text, re.M)
    return found.group(1).strip() if found else ""


def _user_sdks():
    """Android SDKs that may already be here: ANDROID_HOME, ANDROID_SDK_ROOT,
    then Android Studio's default folder."""
    found = [os.environ.get("ANDROID_HOME"), os.environ.get("ANDROID_SDK_ROOT")]
    if sys.platform == "darwin":
        found.append(str(Path.home() / "Library" / "Android" / "sdk"))
    elif sys.platform == "win32":
        found.append(str(Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local")
                         / "Android" / "Sdk"))
    else:
        found.append(str(Path.home() / "Android" / "Sdk"))
    return [Path(p.strip()).expanduser() for p in found if p and p.strip()]


def _version_key(version):
    """The IMAGES key for "16", 16, "16.0" or "Android 16"."""
    text = str(version).strip().lower().removeprefix("android").strip()
    major = text.split(".")[0]
    if major not in IMAGES or text not in (major, major + ".0"):
        raise ValueError(f"android_version must be one of {', '.join(sorted(IMAGES, key=int))} - "
                         f"got {version!r}")
    return major


def _kind(device):
    kind = str(device or "phone").strip().lower()
    if kind not in PROFILES:
        raise ValueError(f'sim_device for Android must be "phone" or "tablet" - got {device!r}')
    return kind


def _on_disk(abi):
    """The Android versions whose image is here already, in any SDK."""
    roots = _user_sdks() + [cache_root()]
    return [key for key, (api, _archives) in IMAGES.items()
            if any((_image_dir(root, api, abi) / "source.properties").is_file() for root in roots)]


def _needs(root, key, abi):
    """What our own folder still lacks for Android `key`: ("the Android
    emulator", bytes) and ("Android <key>", bytes), for the terms question."""
    host = _host()
    api, archives = IMAGES[key]
    missing = []
    if not (_exe(root).is_file() and _revision(root / "emulator") == EMULATOR_VERSION):
        missing.append(("the Android emulator", EMULATOR_ARCHIVES.get(host, (None, 0))[1]))
    if not (_image_dir(root, api, abi) / "source.properties").is_file():
        missing.append((f"Android {key}", archives[abi][1]))
    return missing


def accept_terms(missing, log=None):
    """Google's terms, accepted once before the first download: the file
    ACCEPTED in our folder, or ACCEPT_ENV, says they were. Else the question
    goes to the terminal, and the yes is remembered. Raises when they are not
    accepted: no terminal to ask on (an IDE's run button, CI), no, or no
    answer; the message says the two ways to accept."""
    log = log or _say
    root = cache_root()
    if (root / ACCEPTED).is_file():
        return
    if _DECLINED:
        # Asked and declined in this process already (a parallel run's other
        # tasks): not asked again, the same answer.
        raise RuntimeError(f"Google's Android SDK terms were not accepted, so nothing was downloaded. {_DECLINED[0]}")
    if (os.environ.get(ACCEPT_ENV) or "").strip() == "1":
        root.mkdir(parents=True, exist_ok=True)
        (root / ACCEPTED).write_text(f"accepted by {ACCEPT_ENV}=1 on {time.strftime('%Y-%m-%d')}\n")
        return
    # The terminal question every download uses (utils/consent.py); this one
    # has its own words and its own memory, because it is a licence.
    from AutoCua.utils.consent import ask as _ask, can_ask as _can_ask

    what = " and ".join(f"{name} ({size / 1e6:.0f} MB)" for name, size in missing)
    how = (f"Run `python -m AutoCua.android_connector.emulator` on a terminal to accept them and "
           f"set it up, or set {ACCEPT_ENV}=1.")
    if not _can_ask():
        raise RuntimeError(f"The Android emulator needs {what} from Google, under Google's Android SDK "
                           f"terms ({TERMS_URL}), which were not accepted yet. {how}")
    log(f"🤖 The Android emulator needs {what}, downloaded from Google into {root}.")
    log(f"🤖 They are Google's, under the Android SDK terms: {TERMS_URL}")
    answer = _ask("    Accept the terms and download them now? [y/N] ")
    if answer not in ("y", "yes"):
        _DECLINED.append(how)
        raise RuntimeError(f"Google's Android SDK terms were not accepted, so nothing was downloaded. {how}")
    root.mkdir(parents=True, exist_ok=True)
    (root / ACCEPTED).write_text(f"accepted on the terminal on {time.strftime('%Y-%m-%d')}\n")


def _sdk_for(key, abi, log=None):
    """The SDK that runs Android `key`: one already here with the emulator and
    that image, else our own, fetching what it lacks once the terms are
    accepted."""
    api = IMAGES[key][0]
    for sdk in _user_sdks():
        if _exe(sdk).is_file() and (_image_dir(sdk, api, abi) / "source.properties").is_file():
            return sdk
    root = cache_root()
    missing = _needs(root, key, abi)
    if missing:
        accept_terms(missing, log)
    ensure_emulator(root)
    ensure_image(root, key, abi)
    # The emulator takes a folder as an SDK only when it has one of these.
    (root / "platform-tools").mkdir(exist_ok=True)
    return root


def _progress(what, size):
    """Prints how far a big download has got, every tenth of it."""
    step = max(size // 10, 1)
    due = [step]

    def tick(got):
        if got >= due[0]:
            print(f"🤖 {what}: {got * 100 // size}%", flush=True)
            due[0] += step
    return tick


def _need_disk(root, need, what):
    free = shutil.disk_usage(root).free
    if free < need:
        raise RuntimeError(f"Not enough free disk for {what}: it needs about {need / 1e9:.0f} GB, "
                           f"and the disk of {root} has {free / 1e9:.0f} GB free")


def _sweep(root):
    """Remove what a stopped fetch left behind: a staging folder that has not
    grown for 10 minutes (one still growing is another run's fetch)."""
    for staging in root.glob(".*.incoming.*"):
        try:
            newest = max((p.stat().st_mtime for p in staging.rglob("*")), default=staging.stat().st_mtime)
        except OSError:
            continue
        if time.time() - newest > 600:
            shutil.rmtree(staging, ignore_errors=True)


def _rename(src, dst):
    for attempt in range(10):
        try:
            os.rename(src, dst)
            return
        except OSError:
            # Windows: an antivirus still scanning the new files holds them
            # open for a moment, as in adb.py.
            if attempt == 9:
                raise
            time.sleep(0.5)


def _fetch(root, what, url, size, sha1, unpacked, target, ready):
    """Download one pinned zip into a staging folder beside target, unpack it
    there and move its folder `unpacked` into place as target, so a half-done
    fetch is never seen as the package. ready() says target is already the
    package: another run that fetched it at the same time got there first,
    and its emulator may be using it, so it stays."""
    root.mkdir(parents=True, exist_ok=True)
    _sweep(root)
    _need_disk(root, size * 4, what)
    staging = root / f".{target.name}.incoming.{os.getpid()}"
    shutil.rmtree(staging, ignore_errors=True)
    print(f"🤖 Fetching {what} from Google ({size / 1e9:.1f} GB), one time...", flush=True)
    try:
        staging.mkdir()
        zip_path = staging / url.rsplit("/", 1)[-1]
        _download(url, zip_path, size, sha1, algo="sha1", progress=_progress(what, size))
        _unzip(zip_path, staging)
        zip_path.unlink()
        if not ready():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.rmtree(target, ignore_errors=True)
            _rename(staging / unpacked, target)
    finally:
        shutil.rmtree(staging, ignore_errors=True)
    print(f"🤖 {what} ready in {target}", flush=True)


def ensure_emulator(root):
    """Our own emulator, fetched into root/emulator once (and again when the
    pin moves). Raises when Google builds none for this computer."""
    host = _host()
    archive = EMULATOR_ARCHIVES.get(host)
    if archive is None:
        raise RuntimeError(f"Google builds no Android emulator for this computer ({host[0]}, {host[1]}): "
                           'only for macOS, Windows x64 and Linux x64. Use device="hardware" with a phone.')
    folder = root / "emulator"

    def ready():
        return _exe(root).is_file() and _revision(folder) == EMULATOR_VERSION

    with _LOCK:
        if not ready():
            name, size, sha1 = archive
            _fetch(root, f"the Android emulator {EMULATOR_VERSION}", REPOSITORY + name, size, sha1,
                   "emulator", folder, ready)


def ensure_image(root, key, abi):
    """Android `key`'s Google APIs image for this CPU, fetched once into root."""
    api, archives = IMAGES[key]
    target = _image_dir(root, api, abi)

    def ready():
        return (target / "source.properties").is_file()

    with _LOCK:
        if not ready():
            name, size, sha1 = archives[abi]
            # The zip holds one folder named after the CPU, e.g. arm64-v8a/.
            _fetch(root, f"Android {key} for the emulator", IMAGES_URL + name, size, sha1, abi, target, ready)


def _check_acceleration(exe):
    """Raises, saying what to turn on, when the emulator can't use this
    computer's hypervisor (its own -accel-check, asked once per process)."""
    key = str(exe)
    if key not in _ACCEL:
        try:
            done = subprocess.run([key, "-accel-check"], capture_output=True, text=True, timeout=60,
                                  stdin=subprocess.DEVNULL)
            said = done.stdout + done.stderr
        except (OSError, subprocess.TimeoutExpired) as e:
            said = str(e)
        # "accel:\n0\nHypervisor.Framework OS X Version 27.0\naccel": 0 is fine.
        found = re.search(r"accel:\s*\n\s*(\d+)\s*\n(.*?)\n\s*accel", said, re.S)
        problem = None
        if found is None or found.group(1) != "0":
            detail = (found.group(2) if found else said).strip() or "no answer"
            problem = f"The emulator can't use this computer's hypervisor ({detail}). {ACCEL_HINTS.get(sys.platform, '')}"
        _ACCEL[key] = problem
    if _ACCEL[key]:
        raise RuntimeError(_ACCEL[key].strip())


def _ensure_avd(name, key, abi, kind):
    """Write the AVD once: its config.ini (the device, what avdmanager would
    write, without Java), then <name>.ini, which points the emulator at it."""
    home = avd_home()
    folder = home / f"{name}.avd"
    if (folder / "config.ini").is_file() and (home / f"{name}.ini").is_file():
        return
    api = IMAGES[key][0]
    width, height, density, orientation, hardware = PROFILES[kind]
    config = {
        "AvdId": name,
        "avd.ini.displayname": f"AutoCua Android {key} {kind}",
        "avd.ini.encoding": "UTF-8",
        "PlayStore.enabled": "false",
        "abi.type": abi,
        "hw.cpu.arch": "arm64" if abi == "arm64-v8a" else "x86_64",
        "hw.cpu.ncore": "4",
        "hw.ramSize": "2048",
        "hw.lcd.width": str(width),
        "hw.lcd.height": str(height),
        "hw.lcd.density": str(density),
        "hw.initialOrientation": orientation,
        "hw.device.manufacturer": "Google",
        "hw.device.name": hardware,
        "hw.keyboard": "yes",
        "hw.mainKeys": "no",
        "hw.gpu.enabled": "yes",
        "hw.gpu.mode": "auto",
        "disk.dataPartition.size": "6G",
        "image.sysdir.1": os.path.join("system-images", api, "google_apis", abi) + os.sep,
        "tag.id": "google_apis",
        "tag.display": "Google APIs",
        "skin.dynamic": "yes",
        "showDeviceFrame": "no",
    }
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "config.ini").write_text("".join(f"{k}={v}\n" for k, v in config.items()), encoding="utf-8")
    (home / f"{name}.ini").write_text(f"avd.ini.encoding=UTF-8\npath={folder}\npath.rel=avd/{name}.avd\n"
                                      f"target={api}\n", encoding="utf-8")


def _free(port):
    with socket.socket() as s:
        try:
            s.bind(("127.0.0.1", port))
            return True
        except OSError:
            return False


def _take_lock(path, wait=False):
    """An exclusive lock on the file at path, held until _drop_lock: the fd,
    or None when another process holds it (with wait, it waits for that
    process instead). The OS lets go of it when its holder dies, however it
    dies. Where files can't be locked (some network shares) it is taken
    without locking anything, as in utils/electron.py."""
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    while True:
        try:
            if os.name == "nt":
                import msvcrt
                os.lseek(fd, 0, os.SEEK_SET)
                msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fd
        except OSError as e:
            held = isinstance(e, BlockingIOError) or (os.name == "nt" and e.errno in (errno.EACCES, errno.EDEADLK))
            if not held:
                return fd                       # cannot lock here: taken as it is
            if not wait:
                os.close(fd)
                return None
            time.sleep(0.5)


def _drop_lock(fd):
    try:
        if os.name == "nt":
            import msvcrt
            os.lseek(fd, 0, os.SEEK_SET)
            msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
    except OSError:
        pass
    finally:
        os.close(fd)


def _claim_port(adb):
    """A console port no emulator uses: not in adb's list, both it and the adb
    port after it free, and its lock file in the cache taken, so two runs
    starting at the same moment never pick the same one (an emulator whose
    port is half taken still starts, on the other half, and the two would
    answer for each other). Returns (port, lock)."""
    listed = {serial for serial, _state in devices(adb)}
    folder = cache_root() / "ports"
    folder.mkdir(parents=True, exist_ok=True)
    with _PORTS_LOCK:
        for port in range(FIRST_PORT, LAST_PORT + 1, 2):
            if port in _PORTS or f"emulator-{port}" in listed or not (_free(port) and _free(port + 1)):
                continue
            lock = _take_lock(folder / f"{port}.lock")
            if lock is not None:
                _PORTS.add(port)
                return port, lock
    raise RuntimeError(f"Every emulator port ({FIRST_PORT} to {LAST_PORT}) is taken: close some emulators")


def _kill_tree(proc):
    """Kill the emulator with what it started: its process group on POSIX (it
    leads one, and runs netsimd beside qemu), its process tree on Windows
    (emulator.exe starts qemu as a process of its own)."""
    try:
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, timeout=15)
        else:
            os.killpg(proc.pid, signal.SIGKILL)
    except (OSError, subprocess.TimeoutExpired):
        pass
    try:
        proc.kill()
    except OSError:
        pass


# The run that started an emulator can die without stopping it: a closed
# terminal (SIGHUP) or an IDE's Stop (SIGTERM, or a hard kill) ends Python
# with no finally and no atexit, and the emulator, in a session of its own,
# gets none of it. It would go on running and keep its AVD locked, and the
# next run of that AVD would fail. So each emulator gets this watcher, a small
# process in a session of its own too: when the run is gone first, it stops
# the emulator as deactivate() does (emu kill, then kill), and it ends by
# itself when the emulator does.
#     python -c _WATCHER <run pid> <emulator pid> <adb> <serial>
_WATCHER = r"""
import os, subprocess, sys, time
run, pid, adb, serial = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
if sys.platform == "win32":
    import ctypes
    k = ctypes.windll.kernel32
    k.OpenProcess.restype = ctypes.c_void_p
    both = (ctypes.c_void_p * 2)(k.OpenProcess(0x100000, False, run), k.OpenProcess(0x100000, False, pid))
    if not both[0] or not both[1] or k.WaitForMultipleObjects(2, both, False, 0xFFFFFFFF) != 0:
        sys.exit()
    gone = lambda: k.WaitForSingleObject(ctypes.c_void_p(both[1]), 0) == 0
    kill = lambda: subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True)
else:
    def gone():
        try:
            os.kill(pid, 0)
            return False
        except OSError:
            return True
    while os.getppid() == run:
        if gone():
            sys.exit()
        time.sleep(1)
    kill = lambda: os.killpg(pid, 9)
try:
    subprocess.run([adb, "-s", serial, "emu", "kill"], capture_output=True, timeout=15)
except Exception:
    pass
for _ in range(60):
    if gone():
        sys.exit()
    time.sleep(1)
kill()
"""


# Written into the AVD's folder by _first_boot once its save was checked. The
# snapshot's own files can't tell: the emulator writes an 11-byte snapshot.pb
# the moment it starts with no snapshot to load, long before any save.
FIRST_BOOT_MARK = "autocua-first-boot"


def _first_boot_done(name):
    """True once the AVD has its saved first boot: our mark, and the
    quick-boot snapshot it stands for still there."""
    folder = avd_home() / f"{name}.avd"
    return (folder / FIRST_BOOT_MARK).is_file() and (folder / "snapshots" / "default_boot" / "snapshot.pb").is_file()


def _saved_first_boot(log_path, folder):
    """True when the stopped first boot really saved its snapshot: its log
    says the save took time (a boot that was cut short says "Not saving
    state" and "took 0 ms"), and the snapshot's memory and record files are
    there with their size."""
    try:
        text = Path(log_path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    took = re.findall(r"Saving snapshot 'default_boot' took (\d+) ms", text)
    snap = folder / "snapshots" / "default_boot"
    try:
        return (bool(took) and int(took[-1]) > 0 and "Not saving state" not in text
                and (snap / "snapshot.pb").stat().st_size > 100 and (snap / "ram.bin").stat().st_size > 1_000_000)
    except OSError:
        return False


def _supported():
    """Raises when Google builds no emulator for this computer, or Linux has
    no KVM for this user: before anything is asked or fetched."""
    host = _host()
    if host not in EMULATOR_ARCHIVES:
        raise RuntimeError(f"Google builds no Android emulator for this computer ({host[0]}, {host[1]}): "
                           'only for macOS, Windows x64 and Linux x64. Use device="hardware" with a phone.')
    if sys.platform == "linux" and not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise RuntimeError(f"The emulator needs KVM, and /dev/kvm is not here or not yours to use. "
                           f"{ACCEL_HINTS['linux']}")


def _first_boot(name, key, kind, log, owner):
    """A normal boot to the home screen, then a stop that saves that moment,
    checked. A failed or interrupted one leaves no half snapshot behind (the
    stub the emulator writes at start would count as a saved one otherwise,
    and every run after would cold-boot). owner: the run's session, whose
    deactivate() stops this boot too."""
    folder = avd_home() / f"{name}.avd"
    shutil.rmtree(folder / "snapshots", ignore_errors=True)
    log(f"🤖 First boot of the Android {key} {kind} emulator, once: it starts, settles on the home "
        "screen and saves that moment, which every run starts from...")
    first = EmulatorSession(_first_boot=True)
    if owner is not None:
        owner._first = first
    try:
        res = first.activate(key, kind)
        if not res.get("ok"):
            raise RuntimeError(res.get("error"))
        time.sleep(5)                       # the first boot's own setup jobs
        if owner is not None and owner._stopping.is_set():
            raise RuntimeError("stopped while the emulator was starting")
        first.deactivate()
        if not _saved_first_boot(first._log_path, folder):
            raise RuntimeError(f"The emulator did not save {name}'s first boot (its log: {first._log_path})")
        (folder / FIRST_BOOT_MARK).write_text(f"first boot saved on {time.strftime('%Y-%m-%d %H:%M')}\n")
    except BaseException:
        first.deactivate(save=False)
        shutil.rmtree(folder / "snapshots", ignore_errors=True)
        raise
    finally:
        if owner is not None:
            owner._first = None


def prepare(version=None, device=None, log=None, owner=None):
    """Everything a run of Android `version` ("11" to "17"; None: the newest
    one already here, else the newest) as a `device` ("phone", the default,
    or "tablet") needs, done ahead: the emulator and the image (after
    Google's terms, once), the AVD, and its first boot (_first_boot), which
    every run then starts from. Nothing is done twice. One process at a time
    per AVD (a lock file beside it), so parallel copies, and two runs, wait
    for the first. owner: the run's EmulatorSession, so its deactivate()
    stops a first boot going on in here.

    Returns {"name", "version", "device", "sdk"}; raises when something is
    missing on this computer, saying what to do, before anything is fetched."""
    log = log or _say
    _supported()
    abi = "arm64-v8a" if _host()[1] == "arm64" else "x86_64"
    key = _version_key(version) if version is not None else max(_on_disk(abi), key=int, default=NEWEST)
    kind = _kind(device)
    name = f"AutoCua_Android_{key}_{kind}"
    with _LOCK:
        sdk = _sdk_for(key, abi, log)
        _check_acceleration(_exe(sdk))
        avd_home().mkdir(parents=True, exist_ok=True)
        lock = _take_lock(avd_home() / f"{name}.lock", wait=True)
        try:
            _ensure_avd(name, key, abi, kind)
            if not _first_boot_done(name):
                if owner is not None and owner._stopping.is_set():
                    raise RuntimeError("stopped before the emulator started")
                _first_boot(name, key, kind, log, owner)
        finally:
            _drop_lock(lock)
    return {"name": name, "version": key, "device": kind, "sdk": str(sdk)}


class EmulatorSession:
    """One emulator, booted by activate() and stopped by deactivate(): a
    read-only copy of its AVD, started from the saved first boot, that keeps
    nothing. (_first_boot is prepare()'s own normal boot, the one that saves.)"""

    def __init__(self, _first_boot=False):
        self._first_boot = _first_boot
        self.proc = None
        self.returncode = None  # the emulator's exit code once it stopped
        self.serial = self.port = self.name = self.version = self.kind = None
        self._log = self._log_path = self._port_lock = None
        self._first = None      # prepare()'s first boot while it goes on for this run
        self._lock = threading.Lock()
        self._stopping = threading.Event()
        atexit.register(self.deactivate)

    def activate(self, version=None, device=None):
        """Boot a fresh copy of the emulator for Android `version` ("11" to
        "17"; None: the newest one already here, else the newest) as a
        `device` ("phone", the default, or "tablet"), after prepare(), and
        wait until Android has started, on the home screen.

        Returns {"ok": True, "serial", "name", "version", "device"}, or
        {"ok": False, "error"}. deactivate() from another thread stops a boot
        that is still going, or one that has not started yet (a session is
        used once)."""
        try:
            self._boot(version, device)
        except Exception as e:  # noqa: BLE001 - reported, like android_session
            self.deactivate()
            return {"ok": False, "error": str(e)}
        except BaseException:
            self.deactivate()     # Ctrl+C while it boots: no emulator is left running
            raise
        return {"ok": True, "serial": self.serial, "name": self.name, "version": self.version,
                "device": self.kind}

    def _boot(self, version, device):
        if self._first_boot:
            # prepare() made the AVD and holds its lock; this is the boot it saves.
            abi = "arm64-v8a" if _host()[1] == "arm64" else "x86_64"
            key, kind = _version_key(version), _kind(device)
            name, sdk = f"AutoCua_Android_{key}_{kind}", _sdk_for(key, abi)
        else:
            ready = prepare(version, device, owner=self)
            key, kind, name, sdk = ready["version"], ready["device"], ready["name"], Path(ready["sdk"])
        if self._stopping.is_set():
            raise RuntimeError("stopped before the emulator started")
        exe = _exe(sdk)
        adb = adb_path()
        if not adb:
            raise RuntimeError("adb is not available")
        _adb(adb, "start-server")
        logs = cache_root() / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ, ANDROID_SDK_ROOT=str(sdk), ANDROID_HOME=str(sdk), ANDROID_AVD_HOME=str(avd_home()))
        # -no-window: the emulator draws no window of its own; the run shows
        # its screen in AutoCua's window, like an iOS Simulator (the preview
        # stream of android/tree/element.rs, in ios_connector/sim_view.py).
        cmd = [str(exe), "-avd", name, "-no-window", "-no-boot-anim", "-no-audio", "-no-metrics",
               "-crash-report-mode", "never"]
        if not self._first_boot:
            cmd.append("-read-only")
        # A process group of its own: Ctrl+C in the terminal stops the run,
        # which stops the emulator with emu kill so it saves its state. The
        # signal itself would not: measured, it hangs 20 s, kills itself and
        # saves nothing.
        group = ({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if sys.platform == "win32"
                 else {"start_new_session": True})
        with self._lock:
            if self._stopping.is_set():
                raise RuntimeError("stopped before the emulator started")
            self.port, self._port_lock = _claim_port(adb)
            self.serial, self.name, self.version, self.kind = f"emulator-{self.port}", name, key, kind
            self._log_path = logs / f"emulator-{self.port}.log"
            self._log = self._log_path.open("wb")
            if not self._first_boot:
                print(f"🤖 Starting the Android {key} {kind} emulator ({self.serial})...", flush=True)
            self.proc = subprocess.Popen(cmd + ["-port", str(self.port)], env=env, stdin=subprocess.DEVNULL,
                                         stdout=self._log, stderr=subprocess.STDOUT, **group)
            # Hidden on Windows, in a session of its own on POSIX: no Ctrl+C,
            # no closed terminal reaches it.
            subprocess.Popen([sys.executable, "-c", _WATCHER, str(os.getpid()), str(self.proc.pid), adb, self.serial],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             **({"creationflags": subprocess.CREATE_NO_WINDOW | subprocess.CREATE_NEW_PROCESS_GROUP}
                                if sys.platform == "win32" else {"start_new_session": True}))
        self._wait_boot(adb)
        # Android is up; the apps that start with it may still be busy. Wait
        # for its broadcasts to settle (seconds on a first boot, none on a
        # quick boot), then begin on the home screen.
        try:
            _adb(adb, "-s", self.serial, "shell", "cmd", "activity", "wait-for-broadcast-idle", timeout=90)
        except subprocess.TimeoutExpired:
            pass
        _adb(adb, "-s", self.serial, "shell", "input", "keyevent", "KEYCODE_HOME")

    def _wait_boot(self, adb):
        deadline = time.monotonic() + BOOT_WAIT
        while True:
            proc = self.proc
            if proc is None or self._stopping.is_set():
                raise RuntimeError("stopped while the emulator was starting")
            if proc.poll() is not None:
                raise RuntimeError(self._exit_reason(proc.returncode))
            try:
                if (dict(devices(adb)).get(self.serial) == "device" and _adb(
                        adb, "-s", self.serial, "shell", "getprop", "sys.boot_completed",
                        timeout=10).stdout.strip() == "1"):
                    return
            except subprocess.TimeoutExpired:
                pass
            if time.monotonic() > deadline:
                raise RuntimeError(f"{self.name} did not start within {BOOT_WAIT} s (its log: {self._log_path})")
            time.sleep(1)

    def _exit_reason(self, code):
        """Why the emulator ended before Android started, from its log."""
        try:
            lines = self._log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            lines = []
        if any("Another emulator instance is running" in line for line in lines):
            return (f"{self.name} is running already, in another run. Wait for that run to end, "
                    "or close that emulator.")
        errors = [line.split("|", 1)[-1].strip() for line in lines if line.startswith(("FATAL", "ERROR"))]
        detail = errors[-1] if errors else (lines[-1].strip() if lines else f"exit code {code}")
        return f"The emulator stopped before Android started: {detail} (its log: {self._log_path})"

    def deactivate(self, save=True):
        """Stop the emulator: adb emu kill, which lets it save its state (a
        first boot's; a read-only copy has nothing to save), and when its
        console doesn't take that (still starting, hung) or it won't go, kill
        it with what it started. save=False kills it at once, for a first
        boot that is given up. A first boot going on for this run in
        prepare() is stopped too. Safe to call twice, and before activate()."""
        self._stopping.set()
        first = self._first
        if first is not None:
            first.deactivate(save=False)
        with self._lock:
            proc = self.proc
            try:
                if proc is not None and proc.poll() is None:
                    self._stop(proc, save)
            except BaseException:
                # A second Ctrl+C while it saves: it goes at once, so nothing
                # is left running.
                if proc is not None:
                    _kill_tree(proc)
                raise
            finally:
                if proc is not None:
                    self.returncode = proc.poll()
                self.proc = None
                if self._log is not None:
                    self._log.close()
                    self._log = None
                if self._port_lock is not None:
                    _drop_lock(self._port_lock)
                    self._port_lock = None
                if self.port is not None:
                    with _PORTS_LOCK:
                        _PORTS.discard(self.port)
                    self.port = None

    def _stop(self, proc, save):
        said = ""
        adb = adb_path()
        if save and adb:
            try:
                said = _adb(adb, "-s", self.serial, "emu", "kill", timeout=15).stdout
            except subprocess.TimeoutExpired:
                pass
        # Its console answers "OK: killing emulator, bye bye", and it then
        # saves its state (2 to 9 s measured).
        try:
            proc.wait(timeout=STOP_WAIT if "bye" in said else 1)
        except subprocess.TimeoutExpired:
            _kill_tree(proc)
            proc.wait(timeout=15)


# ───────────────────────────── the precheck ─────────────────────────────
# What a run needs, checked and fetched ahead of time, one line each:
#     python -m AutoCua.android_connector.emulator            # the newest Android here, else 17, as a phone
#     python -m AutoCua.android_connector.emulator 15 tablet
NEED_DISK = 12_000_000_000      # one Android version, with its AVD after the first boot


def _precheck(version=None, device=None):
    """Prints each item, pass or what to fix, and returns True when the
    emulator is ready to run."""
    failed = []

    def fail(what, why):
        failed.append(what)
        _say(f"🤖 {what}: {why}")

    host = _host()
    if host in EMULATOR_ARCHIVES:
        _say(f"🤖 Computer: {host[0]} {host[1]}, Google builds the emulator for it")
    else:
        fail("Computer", f"Google builds no Android emulator for {host[0]} {host[1]} (macOS, Windows x64 and "
             "Linux x64 only)")
        return False
    here = _on_disk("arm64-v8a" if host[1] == "arm64" else "x86_64")
    if version is not None:
        _say(f"🤖 Android {_version_key(version)} as a {_kind(device)}")
    elif here:
        _say(f"🤖 Android {max(here, key=int)} as a {_kind(device)}: the newest one already here")
    else:
        _say(f"🤖 Android {NEWEST} as a {_kind(device)}: the newest, none is here yet")
    root = cache_root()
    probe = root
    while not probe.exists():
        probe = probe.parent
    free = shutil.disk_usage(probe).free
    if free >= NEED_DISK:
        _say(f"🤖 Disk: {free / 1e9:.0f} GB free for {root}")
    else:
        fail("Disk", f"{free / 1e9:.0f} GB free for {root}, one Android version needs about "
             f"{NEED_DISK / 1e9:.0f} GB")
    if sys.platform == "linux":
        if os.access("/dev/kvm", os.R_OK | os.W_OK):
            _say("🤖 Hypervisor: KVM is here and yours to use")
        else:
            fail("Hypervisor", f"KVM is not here, or not yours to use. {ACCEL_HINTS['linux']}")
    elif sys.platform == "darwin":
        _say("🤖 Hypervisor: macOS has it built in")
    else:
        _say("🤖 Hypervisor: the emulator checks Windows Hypervisor Platform once it is here")
    try:
        adb = adb_path()
        if not adb:
            raise RuntimeError("adb is not available")
        _say(f"🤖 adb: {adb}")
    except Exception as e:  # noqa: BLE001 - reported, the next items are still worth checking
        fail("adb", str(e))
    from AutoCua.android_connector.stream import SERVER, ensure_server
    if ensure_server():
        _say(f"🤖 scrcpy's server: {SERVER}")
    else:
        fail("scrcpy's server", "could not be fetched")
    try:
        # A checkout builds the Rust agent here, with cargo (a minute, once);
        # a wheel has it built.
        import AutoCua.android.agent  # noqa: F401
        _say("🤖 Android agent: built")
    except Exception as e:  # noqa: BLE001 - its message says what to install
        fail("Android agent", str(e).strip().splitlines()[0])
    if failed:
        return False
    try:
        ready = prepare(version, device)
        _say(f"🤖 Emulator: Android {ready['version']} {ready['device']} is ready to run ({ready['name']})")
    except Exception as e:  # noqa: BLE001 - reported
        fail("Emulator", str(e))
    return not failed


if __name__ == "__main__":
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows
    _args = sys.argv[1:]
    if len(_args) > 2 or any(a.startswith("-") for a in _args):
        sys.exit("usage: python -m AutoCua.android_connector.emulator [version] [phone|tablet]")
    try:
        _ok = _precheck(*_args)
    except KeyboardInterrupt:
        sys.exit(130)
    sys.exit(0 if _ok else 1)
