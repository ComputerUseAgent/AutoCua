#!/usr/bin/env python3

"""
Manual scan runner for tree/element.rs (Android, real phone on USB)

element.rs reads the phone the way ios/tree/element.py reads an iPhone: the
UI tree filtered to the elements that matter, under the iPhone's element
names, numbered, as tree text plus one JSON entry each, and the screenshot
with numbered boxes. It has no connection of its own: the connector finds adb
and waits for the phone (android_connector/adb.py), then starts the phone
helper (scrcpy's server for the screen, Helper.java for the tree, one
process) and forwards both to local ports (android_connector/stream.py).

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
      builds element.rs with cargo into AutoCua/android/target/ (the crate of
      the whole Android side, AutoCua/android/Cargo.toml; the first build
      compiles OpenH264, under a minute). All gitignored.
    - autocua-helper.jar is built from Helper.java only when the source
      changes (android_connector/build_helper.py: a JDK and Android's d8,
      fetched once into ~/.AutoCua/android-build).
    - cargo is not on PATH here; this falls back to ~/.cargo/bin/cargo.

REPL:
    s / enter   scan -> debug/android/tree.txt + elements.json + scan.jpg
    p           plain screenshot -> debug/android/shot.jpg
    q           quit (stops the server on the phone)
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ANDROID = HERE.parent                   # AutoCua/android: the crate (Cargo.toml, target/)
ROOT = HERE.parents[2]                  # repo root, debug/ lives here
BINARY = ANDROID / "target" / "release" / ("element.exe" if os.name == "nt" else "element")

HELP = """
  s / enter   scan: tree + elements.json + marked screenshot
  p           plain screenshot
  q           quit (stops the server on the phone)
"""


def cargo_path():
    """cargo on PATH, else rustup's default install."""
    found = shutil.which("cargo")
    if found:
        return found
    cand = Path.home() / ".cargo" / "bin" / ("cargo.exe" if os.name == "nt" else "cargo")
    return str(cand) if cand.exists() else None


def build() -> None:
    """cargo build --release --bin element into AutoCua/android/target, where
    BINARY looks (a CARGO_TARGET_DIR would send it elsewhere). The crate is the
    whole Android side (lib.rs is the agent); only the binary is built here.
    cargo itself skips the work when nothing changed."""
    cargo = cargo_path()
    if not cargo:
        sys.exit("cargo not found. Install Rust from https://rustup.rs and run again.")
    if not BINARY.exists():
        print("Building element.rs (first time: under a minute)...")
    # The crate's pyo3 (the agent's) is configured against this Python.
    env = dict(os.environ)
    env.setdefault("PYO3_PYTHON", sys.executable)
    done = subprocess.run([cargo, "build", "--release", "--quiet", "--bin", "element",
                           "--manifest-path", str(ANDROID / "Cargo.toml"),
                           "--target-dir", str(ANDROID / "target")], env=env)
    if done.returncode != 0:
        sys.exit("element.rs did not build (see above)")


def main() -> None:
    ap = argparse.ArgumentParser(description="Scan the Android phone's screen with element.rs")
    ap.add_argument("--serial", default=None, help="Which phone, when more than one is plugged in")
    ap.add_argument("--out-dir", default="debug/android",
                    help="Where scans and shots go (relative to the repo root)")
    args = ap.parse_args()
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows

    # element.rs writes relative to its CWD, so anchor it at the repo root.
    os.chdir(ROOT)
    sys.path.insert(0, str(ROOT))
    from AutoCua.android_connector.adb import connect  # noqa: E402
    from AutoCua.android_connector.stream import Stream  # noqa: E402

    adb, serial = connect(args.serial)
    print(f"phone: {serial}")
    build()
    stream = Stream.start(adb, serial)
    el = None
    try:
        # UTF-8 both ways: element.rs reads UTF-8, and Windows would use its code page.
        el = subprocess.Popen([str(BINARY), "--port", str(stream.port), "--tree-port", str(stream.tree_port),
                               "--adb", adb, "--serial", serial, "--out-dir", args.out_dir],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              encoding="utf-8", errors="replace", bufsize=1)
        ready = el.stdout.readline()        # "ready WxH  N ms", or nothing if it failed
        if not ready:
            sys.exit(el.wait() or 1)        # element.rs said why on stderr
        print(ready.strip())
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
            el.stdin.write(cmd + "\n")
            el.stdin.flush()
            reply = el.stdout.readline()
            if not reply:
                print("element.rs stopped (see above)")
                break
            if reply.startswith("scan ->"):
                print((Path(args.out_dir) / "tree.txt").read_text(encoding="utf-8"))
            print(f"{reply.strip()}   ({(time.perf_counter() - t0) * 1000:.0f} ms round trip)")
    finally:
        if el is not None and el.poll() is None:
            try:
                el.stdin.write("q\n")
                el.stdin.flush()
                el.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                el.kill()
        stream.stop()


if __name__ == "__main__":
    main()
