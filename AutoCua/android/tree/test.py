#!/usr/bin/env python3

"""
Manual scan runner for tree/element.rs (Android, real phone on USB)

element.rs reads the phone the way ios/tree/element.py reads an iPhone: the
UI tree filtered to the elements that matter, under the iPhone's element
names, numbered, as tree text plus one JSON entry each, and the screenshot
with numbered boxes. This drives the very class the agent scans with,
UIElementScanner, from the Android extension module (AutoCua/android/agent
builds it on first import). The connection is the connector's: adb and the
phone (android_connector/adb.py), then the phone helper (scrcpy's server for
the screen, Helper.java for the tree, one process) forwarded to local ports
(android_connector/stream.py), opened and closed by android_session
(android_connector/session.py), as the agent does around a run.

This file lives in:  <repo>/AutoCua/android/tree/

Usage (from anywhere):
    python3 AutoCua/android/tree/test.py
    python3 AutoCua/android/tree/test.py --serial R5CT1234ABC

On the phone, once:
    Settings > Developer options > USB debugging: on
    Plug in with a data cable, unlock, tap Allow on "Allow USB debugging?"

Notes:
    - First run fetches adb (Google's platform-tools) and scrcpy's server
      (Genymobile's GitHub release) into AutoCua/android_connector/, and
      builds the Android crate with cargo into AutoCua/android/target/ (the
      first build compiles OpenH264, about a minute). All gitignored.
    - autocua-helper.jar is built from Helper.java only when the source
      changes (android_connector/build_helper.py: a JDK and Android's d8,
      fetched once into ~/.AutoCua/android-build).
    - cargo is not on PATH here; the build falls back to ~/.cargo/bin/cargo.

REPL:
    s / enter   scan -> debug/android/tree.txt + elements.json + scan.jpg
    p           plain screenshot -> debug/android/shot.jpg
    q           quit (stops the helper on the phone)
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]                  # repo root, debug/ lives here

HELP = """
  s / enter   scan: tree + elements.json + marked screenshot
  p           plain screenshot
  q           quit (stops the helper on the phone)
"""


def main() -> None:
    ap = argparse.ArgumentParser(description="Scan the Android phone's screen with element.rs")
    ap.add_argument("--serial", default=None, help="Which phone, when more than one is plugged in")
    args = ap.parse_args()
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows

    # The scanner writes debug/android/ relative to its CWD, so anchor it at
    # the repo root.
    os.chdir(ROOT)
    sys.path.insert(0, str(ROOT))
    from AutoCua.android_connector.session import android_session  # noqa: E402

    t0 = time.perf_counter()
    res = android_session.activate(args.serial)
    if not res.get("ok"):
        sys.exit(f"phone: {res.get('error')}")
    print(f"phone: {res['model']} ({res['serial']})")
    try:
        from AutoCua.android.agent import UIElementScanner  # noqa: E402  (builds the crate first time)
        scanner = UIElementScanner()
        w, h = scanner.frame_size
        print(f"ready {w}x{h}  {(time.perf_counter() - t0) * 1000:.0f} ms")
        print(HELP)
        while True:
            try:
                cmd = input("AutoCua> ").strip().lower() or "s"
            except (EOFError, KeyboardInterrupt):
                print()
                break
            if cmd in ("q", "quit", "exit"):
                break
            if cmd in ("h", "help", "?"):
                print(HELP)
                continue
            t0 = time.perf_counter()
            try:
                if cmd == "p":
                    print(scanner.shot())
                elif cmd == "s":
                    scanner.scan_elements()
                    tree, _image = scanner.get_scan_data()
                    print(tree)
                else:
                    print(f"unknown command {cmd!r} (s, p or q)")
                    continue
            except Exception as e:  # noqa: BLE001 - the REPL goes on after a failed scan
                print(f"error: {e}")
            print(f"({(time.perf_counter() - t0) * 1000:.0f} ms round trip)")
    finally:
        android_session.deactivate()


if __name__ == "__main__":
    main()
