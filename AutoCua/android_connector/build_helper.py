"""Build AutoCua's half of the phone helper: Helper.java -> autocua-helper.jar.

The phone runs one helper (stream.py starts it): scrcpy's server streams the
screen, and Helper.java, inside the same process, reads the UI tree. Java
reaches a phone as dex, so BUILDING it takes a JDK (javac), Android's
android.jar to compile against, and Google's d8 to turn classes into dex.
RUNNING it takes none of them: the jar is a small file the phone loads.

The three are fetched once, pinned by size and SHA-256 like adb.py's zip, into
~/.AutoCua/android-build (outside the repo, about 300 MB unpacked). A JDK on
PATH is not used, so every build comes out of the same tools.

The jar records the SHA-256 of the exact Helper.java bytes it was built from
(line endings evened out, since a Windows checkout may differ), so
ensure_helper() rebuilds only when the source really changed: a checkout's
file times say nothing.

    python3 -m AutoCua.android_connector.build_helper     # build now
"""

import hashlib
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from pathlib import Path

from AutoCua.android_connector.adb import HERE, _download, _unzip
from AutoCua.utils.consent import allow_download

SOURCE = HERE / "Helper.java"
JAR = HERE / "autocua-helper.jar"
SOURCE_ENTRY = "autocua-source.sha256"
CACHE = Path.home() / ".AutoCua" / "android-build"

# Temurin 21 (Eclipse Adoptium). Recorded 2026-10-25 from Adoptium's API
# (size + SHA-256 per file); the newest release was two days old, so the one
# before it.
JDK_RELEASE = "jdk-21.0.12.1+1"
JDK_URL = "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/"
JDKS = {
    # (sys.platform, CPU) -> (file, bytes, sha256)
    ("darwin", "aarch64"): ("OpenJDK21U-jdk_aarch64_mac_hotspot_21.0.12.1_1.tar.gz", 200073404,
                            "3623232f33a9c3baadf304480b2535f9a3cba8a58d42ecbb438ba267315d9998"),
    ("darwin", "x64"): ("OpenJDK21U-jdk_x64_mac_hotspot_21.0.12.1_1.tar.gz", 194316575,
                        "44db0f08196daf19a47f90d13388b0c943b67663cb537f998fe29e836fa842ce"),
    ("linux", "x64"): ("OpenJDK21U-jdk_x64_linux_hotspot_21.0.12.1_1.tar.gz", 207473347,
                       "ce79869e1307ed8ee1e2baa86a412b1eb5b75d10a01006d788a6f968bcfaee94"),
    ("linux", "aarch64"): ("OpenJDK21U-jdk_aarch64_linux_hotspot_21.0.12.1_1.tar.gz", 205641175,
                           "23e37e026f12f3e706f18938ff611db3032d075b09d0879a25d06718c773e223"),
    ("win32", "x64"): ("OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip", 205073461,
                       "f9d6e191ab098c0d416e7d588a24420a8621cd2f4720dab2459b8b7b2d2d8b4e"),
}

# android.jar from the Android 15 SDK platform (API 35), and d8 from R8 9.3.31.
# Recorded 2026-10-25; both files matched the SHA-1 Google publishes for them.
PLATFORM = ("https://dl.google.com/android/repository/platform-35_r02.zip", 64273788,
            "0988cacad01b38a18a47bac14a0695f246bc76c1b06c0eeb8eb0dc825ab0c8e0")
PLATFORM_JAR = "android-35/android.jar"
R8 = ("https://dl.google.com/android/maven2/com/android/tools/r8/9.3.31/r8-9.3.31.jar", 20954934,
      "29677b994a40668b97155a65628afd953f72571a8100bfa7296b4c5bcc79676c")


def ensure_helper() -> Path:
    """The built jar, rebuilt only when Helper.java changed."""
    if JAR.is_file():
        try:
            with zipfile.ZipFile(JAR) as z:
                if z.read(SOURCE_ENTRY).decode().strip() == _source_hash(SOURCE.read_bytes()):
                    return JAR
        except (KeyError, zipfile.BadZipFile):
            pass
    build()
    return JAR


def build():
    """javac, then d8, into autocua-helper.jar (written under a private name
    and renamed into place)."""
    javac, java = _jdk()
    android_jar = _android_jar()
    r8 = _fetch(*R8, CACHE / "r8-9.3.31.jar")
    print("🤖 Building AutoCua's phone helper (Helper.java)...", flush=True)
    # Read once: the bytes that are hashed are the bytes that are compiled,
    # even if the file is saved again mid-build.
    source = SOURCE.read_bytes()
    with tempfile.TemporaryDirectory() as tmp:
        java_file, classes, dex = Path(tmp, SOURCE.name), Path(tmp, "classes"), Path(tmp, "dex")
        java_file.write_bytes(source)
        dex.mkdir()
        # Java 8 against android.jar's own java.* classes, the way Android code
        # has always been compiled; d8 then lowers it for Android 5 and up.
        _run([javac, "-source", "8", "-target", "8", "-bootclasspath", android_jar,
              "-Xlint:-options", "-nowarn", "-encoding", "UTF-8", "-d", classes, java_file])
        _run([java, "-cp", r8, "com.android.tools.r8.D8", "--release", "--min-api", "21",
              "--lib", android_jar, "--output", dex, *sorted(classes.rglob("*.class"))])
        part = JAR.with_name(f".{JAR.name}.{os.getpid()}")
        with zipfile.ZipFile(part, "w", zipfile.ZIP_DEFLATED) as z:
            z.write(dex / "classes.dex", "classes.dex")
            z.writestr(SOURCE_ENTRY, _source_hash(source))
        os.replace(part, JAR)
    print(f"🤖 Phone helper built: {JAR}")


def _source_hash(source):
    return hashlib.sha256(source.replace(b"\r\n", b"\n")).hexdigest()


def _jdk():
    """javac and java from the pinned JDK, fetched and unpacked once."""
    machine = platform.machine().lower()
    cpu = {"arm64": "aarch64", "aarch64": "aarch64", "x86_64": "x64", "amd64": "x64"}.get(machine, machine)
    if sys.platform == "win32":
        cpu = "x64"     # Windows on Arm runs the x64 JDK
    if (sys.platform, cpu) not in JDKS:
        raise RuntimeError(f"no pinned JDK for {sys.platform} {machine}")
    name, size, digest = JDKS[(sys.platform, cpu)]
    home = CACHE / JDK_RELEASE
    if not home.is_dir():
        archive = _fetch(JDK_URL + name, size, digest, CACHE / name)
        _unpack(archive, home)
        archive.unlink()
    exe = ".exe" if sys.platform == "win32" else ""
    javac = next(home.rglob(f"bin/javac{exe}"))
    return javac, javac.with_name(f"java{exe}")


def _android_jar():
    """android.jar out of the pinned SDK platform zip, kept on its own."""
    jar = CACHE / "android-35.jar"
    if not jar.is_file():
        archive = _fetch(*PLATFORM, CACHE / "platform-35_r02.zip")
        part = jar.with_name(f".{jar.name}.{os.getpid()}")
        with zipfile.ZipFile(archive) as z, open(part, "wb") as out:
            shutil.copyfileobj(z.open(PLATFORM_JAR), out)
        os.replace(part, jar)
        archive.unlink()
    return jar


def _fetch(url, size, digest, dest):
    """One pinned download into the cache, under a private name until whole."""
    if dest.is_file():
        return dest
    if not allow_download(f"{url.rsplit('/', 1)[-1]}, to build the phone helper", f"its maker ({url.split('/')[2]})",
                          size, log=lambda text: print(f"🤖 {text}", flush=True)):
        raise RuntimeError(f"{url.rsplit('/', 1)[-1]} was not downloaded")
    CACHE.mkdir(parents=True, exist_ok=True)
    print(f"🤖 Fetching {url.rsplit('/', 1)[-1]} ({size / 1e6:.0f} MB), one time...", flush=True)
    part = dest.with_name(f".{dest.name}.{os.getpid()}")
    try:
        _download(url, part, size, digest)
        os.replace(part, dest)
    finally:
        part.unlink(missing_ok=True)
    return dest


def _unpack(archive, dest):
    """Unpack under a private name and rename it into place, as adb.py does."""
    staging = dest.with_name(f".{dest.name}.{os.getpid()}")
    shutil.rmtree(staging, ignore_errors=True)
    try:
        if archive.name.endswith(".zip"):
            _unzip(archive, staging)
        else:
            with tarfile.open(archive) as t:
                # "tar", not "data": the JDK holds relative symlinks, which the
                # "data" filter of Python 3.10.12 and 3.11.4 (Ubuntu 22.04's)
                # rejects by mistake. The archive is pinned by SHA-256 anyway.
                try:
                    t.extractall(staging, filter="tar")
                except TypeError:       # a Python from before the extraction filters
                    t.extractall(staging)
        for attempt in range(10):
            try:
                os.replace(staging, dest)
                break
            except OSError:
                # Another process unpacked it first: fine. Windows: an
                # antivirus still scanning the new files holds them open.
                if dest.is_dir():
                    break
                if attempt == 9:
                    raise
                time.sleep(0.5)
    finally:
        shutil.rmtree(staging, ignore_errors=True)


def _run(cmd):
    done = subprocess.run([str(c) for c in cmd], capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    if done.returncode != 0:
        raise RuntimeError(f"{Path(str(cmd[0])).name} failed:\n{done.stdout}{done.stderr}")


if __name__ == "__main__":
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows
    build()
