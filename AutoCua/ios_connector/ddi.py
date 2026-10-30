"""The developer disk image (DDI) on a wired iPhone, mounted from Xcode's copy.

XCTest, so WebDriverAgent, cannot start without it, and the phone drops it at
every reboot and iOS update. iOS needs full Xcode anyway, and Xcode keeps the
image on this Mac, so it is mounted from there and nothing is downloaded:

    iOS 17+   current Xcode unpacks it into XCODE_RESTORE, whose
              BuildManifest.plist names the image and its trust cache:
                pymobiledevice3 mounter mount-personalized IMAGE TRUST_CACHE BUILD_MANIFEST
    iOS < 17  <xcode-select -p>/Platforms/iPhoneOS.platform/DeviceSupport/<x.y>/:
                pymobiledevice3 mounter mount-developer IMAGE SIGNATURE

When that leaves it unmounted (an Xcode that keeps the image as one .dmg or
has not unpacked it yet, a phone newer than the image's device list, iOS 27's
cryptex), Xcode mounts its own: `xcrun devicectl device info ddiServices`.
An iOS 17+ image also needs a ticket from Apple's signing server
(gs.apple.com) the first time a phone mounts it, as it does for Xcode.

Only when this Mac has no copy from Xcode at all does `pymobiledevice3
mounter auto-mount` fetch one: the pymobiledevice3 project's copy on GitHub,
not Apple's, and only after the person's yes (utils/consent.py). auto-mount's
own lookup is no use here: on iOS 17+ it never looks at Xcode, and it fetches
again whenever its cache's build differs from the one its release names.

    python -m AutoCua.ios_connector.ddi [udid]   # the same on a terminal, with the question
"""

import json
import os
import plistlib
import re
import subprocess
import sys
from pathlib import Path

# Where current Xcode (26, 27; most likely from 16.3) unpacks the iOS 17+
# image: `xcrun devicectl list preferredDDI` names its parent as the "hostDDI".
XCODE_RESTORE = Path("/Library/Developer/DeveloperDiskImages/iOS_DDI/Restore")

# The same image as one .dmg, which only Xcode itself (devicectl) mounts:
# Xcode 16.3+ before it first unpacks it, Xcode 16.0-16.2, Xcode 15.
XCODE_DMGS = (Path("/Library/Developer/CoreDevice/CandidateDDIs/iOS_DDI.dmg"),
              Path("/Library/Developer/DeveloperDiskImages/iOS_DDI.dmg"),
              Path.home() / "Library/Developer/DeveloperDiskImages/iOS_DDI.dmg")

# What mounts it on a terminal, with the question: a run that cannot ask (the
# desktop app's backend) names it in its line.
MOUNT = "python -m AutoCua.ios_connector.ddi"

# The download, for the question: where it comes from, and its size for iOS
# 17+ (image, build manifest and trust cache of Xcode build 27A5228h) and for
# an older iOS.
SOURCE = ("the pymobiledevice3 project's copy on GitHub "
          "(raw.githubusercontent.com/doronz88/DeveloperDiskImage), not Apple's server")
SIZE_PERSONALIZED = 16_536_648
SIZE_DEVELOPER = "about 9 MB"

# What to do when Xcode's copy is there and still did not mount.
HINT = ("unlock the iPhone, keep it on the cable and this Mac online (Apple signs the image "
        "for the phone the first time), then try again. Still failing: open Xcode once, or "
        "update it; `xcrun devicectl device info ddiServices --device {udid}` says why.")


def _env(udid):
    return dict(os.environ, PYMOBILEDEVICE3_UDID=udid) if udid else None


def _version(text):
    """(16, 4) from "16.4", "16.4.1" or "16.4 (20E247)"; None for anything else."""
    m = re.match(r"\s*(\d+(?:\.\d+)*)", str(text or ""))
    return tuple(int(n) for n in m.group(1).split(".")) if m else None


def _is_image(entry):
    """A `mounter list` entry for a developer disk image: by its type, or, in
    an entry without one, by a signature (what this check looked for before)."""
    if not isinstance(entry, dict):
        return False
    kind = entry.get("DiskImageType")
    return kind in ("Developer", "Personalized") if kind else "ImageSignature" in entry


def mounted(base, udid):
    """True when the phone has the image mounted (mounter list: one lockdown
    call, about a second). False also when it could not say."""
    try:
        out = subprocess.run(base + ["mounter", "list"], capture_output=True, text=True,
                             env=_env(udid), timeout=30).stdout
        images = json.loads(out or "[]")
    except Exception:
        return False
    return isinstance(images, list) and any(_is_image(entry) for entry in images)


def _ios(base, udid):
    """The phone's iOS, (26, 1); None when it could not say."""
    try:
        out = subprocess.run(base + ["lockdown", "info"], capture_output=True, text=True,
                             env=_env(udid), timeout=30).stdout
        return _version(json.loads(out).get("ProductVersion"))
    except Exception:
        return None


def _developer_dir():
    """The selected Xcode's Developer folder (xcode-select -p); None without
    a full Xcode (the Command Line Tools carry no disk image)."""
    try:
        out = subprocess.run(["xcode-select", "-p"], capture_output=True, text=True,
                             timeout=10).stdout.strip()
    except Exception:
        return None
    return out if out.endswith(".app/Contents/Developer") and os.path.isdir(out) else None


def xcode_image(ios):
    """The mounter subcommand and files for Xcode's copy of the image for iOS
    `ios` (None, not known: tried as 17+, as nearly every phone is); None when
    Xcode has none to hand to pymobiledevice3."""
    if ios is None or ios >= (17,):
        manifest = XCODE_RESTORE / "BuildManifest.plist"
        try:
            identities = plistlib.loads(manifest.read_bytes()).get("BuildIdentities", [])
        except Exception:
            return None
        # Every identity with a PersonalizedDMG names the same two files; the
        # manifest also lists the cryptex's (iOS 27) under other keys.
        for identity in identities:
            try:
                entries = identity["Manifest"]
                image = XCODE_RESTORE / entries["PersonalizedDMG"]["Info"]["Path"]
                trust = XCODE_RESTORE / entries["LoadableTrustCache"]["Info"]["Path"]
            except (KeyError, TypeError):
                continue
            if image.is_file() and trust.is_file():
                return ["mount-personalized", str(image), str(trust), str(manifest)]
        return None
    developer = _developer_dir()
    if not developer:
        return None
    # Xcode keeps one folder per image change (16.4 serves 16.4 to 16.7): the
    # newest of the phone's major version that is not newer than the phone.
    best = None
    try:
        for folder in (Path(developer) / "Platforms/iPhoneOS.platform/DeviceSupport").iterdir():
            v = _version(folder.name)
            image = folder / "DeveloperDiskImage.dmg"
            signature = folder / "DeveloperDiskImage.dmg.signature"
            if (v and v[0] == ios[0] and v[:2] <= ios[:2] and image.is_file()
                    and signature.is_file() and (best is None or v > best[0])):
                best = (v, ["mount-developer", str(image), str(signature)])
    except OSError:
        return None
    return best[1] if best else None


def _xcode_dmgs():
    """The .dmg copies of the iOS 17+ image on this Mac, Xcode.app's own included."""
    dmgs = list(XCODE_DMGS)
    developer = _developer_dir()
    if developer:
        dmgs.append(Path(developer).parent / "Resources/CoreDeviceDDIs/iOS_DDI.dmg")
    return [p for p in dmgs if p.is_file()]


def mount(base, udid, run, say, fetch=True):
    """Mount the image on the phone `udid`, Xcode's copy first. True when it
    is mounted after.

    `run(cmd, timeout)` runs a command the caller's way (its output into the
    session's log, or onto the pairing page) and returns its exit code, None
    when it did not finish; `say(text)` reports one line. Nothing is
    downloaded unless `fetch`, this Mac has no copy from Xcode, and the
    person says yes."""
    ios = _ios(base, udid)
    personalized = ios is None or ios >= (17,)
    image = xcode_image(ios)
    if image:
        say(f"mounting the developer disk image from Xcode's copy ({Path(image[1]).parent})")
        run(base + ["mounter"] + image, 90)
        if mounted(base, udid):
            return True
    # Xcode's copy at all: the mounter's files, or (iOS 17+) the image as a
    # .dmg, which only Xcode itself mounts.
    if image or (personalized and (XCODE_RESTORE.is_dir() or _xcode_dmgs())):
        if personalized:
            # Xcode's own way: CoreDevice unpacks its .dmg when it must and
            # mounts what the phone takes (the cryptex from iOS 27, a phone
            # newer than the image's device list).
            say("asking Xcode to mount its developer disk image (xcrun devicectl)")
            if run(["xcrun", "devicectl", "device", "info", "ddiServices", "--device", udid],
                   90) == 0 or mounted(base, udid):
                return True
        say("the developer disk image is not mounted: " + HINT.format(udid=udid))
        return False
    # No copy from Xcode on this Mac at all.
    fix = ("open Xcode once so it installs its components, or run  sudo xcodebuild -runFirstLaunch"
           if personalized else "this Xcode has no image for iOS {}.{}".format(*ios[:2]))
    if not fetch:
        say(f"skipping DDI mount (no copy from Xcode on this Mac: {fix})")
        return False
    say(f"Xcode's developer disk image is not on this Mac ({fix}).")
    from AutoCua.utils.consent import allow_download
    what = ("Apple's developer disk image for iOS 17 and later" if personalized
            else "Apple's developer disk image for iOS {}.{}".format(*ios[:2]))
    size = SIZE_PERSONALIZED if personalized else SIZE_DEVELOPER
    if not allow_download(what, SOURCE, size, log=say, how=f"{MOUNT} {udid}"):
        say(f"the developer disk image is not mounted: {fix}, or run  {MOUNT} {udid}  "
            "on a terminal and answer y.")
        return False
    say("fetching and mounting it (pymobiledevice3 mounter auto-mount)")
    run(base + ["mounter", "auto-mount"], 120)
    if mounted(base, udid):
        return True
    say("the developer disk image is not mounted: " + HINT.format(udid=udid))
    return False


if __name__ == "__main__":
    # MOUNT: the same steps on a terminal, where the download can be asked
    # about. Exits 1 when the image is not mounted.
    from AutoCua.ios_connector.session import _pmd3_base, plugged_in_udids
    base = _pmd3_base()
    if not base:
        sys.exit("pymobiledevice3 was not found. Install it with:  bash setup_mac.sh --ios")
    plugged = sorted(plugged_in_udids() or [])
    udid = sys.argv[1] if sys.argv[1:] else (plugged[0] if len(plugged) == 1 else None)
    if not udid:
        sys.exit(f"Plug in one iPhone, or name it:  {MOUNT} <udid>")

    def run(cmd, timeout):
        try:
            return subprocess.run(cmd, env=_env(udid), timeout=timeout).returncode
        except (OSError, subprocess.TimeoutExpired):
            return None

    def say(text):
        print(f"📱 {text}", flush=True)

    if mounted(base, udid):
        say("The developer disk image is already mounted.")
        sys.exit(0)
    sys.exit(0 if mount(base, udid, run, say) else 1)
