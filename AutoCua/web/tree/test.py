#!/usr/bin/env python3

"""
Manual browser scan runner for tree/element.rs

Starts Chrome the same way the agent does — through the browser side, which
owns the browser — then scans whatever tab you point it at.

element.rs SCANS and nothing else. It has no socket, no tab and no browser of
its own: the browser side holds one CDP session and hands it over for a read.
So there is nothing here to click or type with — drive the page in the Chrome
window with your own mouse and keyboard, and use this prompt to read what you
are looking at.

This file lives in:  <repo>/AutoCua/web/tree/

Usage (from anywhere):
    python3 AutoCua/web/tree/test.py
    python3 AutoCua/web/tree/test.py --port 9222
    python3 AutoCua/web/tree/test.py --url https://www.github.com

Filtering:
    element.config.json (this folder) is picked up automatically and merged
    over the built-in defaults — edit it to tune what gets marked. The "noise"
    block controls the marks filter, "hosts" holds per-site overrides. Press c
    to reload it without restarting.

Notes:
    - First run compiles the web crate with cargo (a minute or two). The
      scanner is part of that crate now, not a separate binary.
    - cargo is not on PATH here; the import falls back to ~/.cargo/bin/cargo.

REPL:
    s / enter   scan the bound tab -> debug/scans/tree.txt + hits.json + shot.jpg
    t           list open tabs
    u <n>       read tab [n] instead
    m           toggle the numbered marks on the screenshot
    c           reload element.config.json
    q           quit (browser stays open)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]                  # repo root — debug/ + scans live here

HELP = """
  s / enter   scan the bound tab
  t           list open tabs
  u <n>       read tab [n] instead
  m           toggle marks
  c           reload element.config.json
  q           quit (browser stays open)
"""


def chrome_json(port: int, path: str, method: str = "GET"):
    """One call to Chrome's HTTP control endpoint."""
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method=method)
    with urllib.request.urlopen(req, timeout=5) as resp:
        body = resp.read().decode("utf-8", "replace")
    try:
        return json.loads(body)
    except json.JSONDecodeError:
        return body


def main() -> None:
    ap = argparse.ArgumentParser(description="Open Chrome and run the element scanner REPL")
    ap.add_argument("--port", type=int, default=9222, help="Chrome remote-debugging port")
    ap.add_argument("--url", default=None, help="Optional start URL (otherwise blank / last tab)")
    ap.add_argument("--out", default="debug/scans",
                    help="Output directory for tree/shot files (relative to the repo root)")
    ap.add_argument("--marks", action="store_true", default=True, help="(default) number the screenshot")
    ap.add_argument("--no-marks", dest="marks", action="store_false", help="plain screenshot")
    args = ap.parse_args()

    # The binary wrote its output relative to its CWD and the agent's debug/
    # wipe expects it at the repo root, so keep the same anchor.
    os.chdir(ROOT)
    sys.path.insert(0, str(ROOT))

    # Importing the package builds the crate if a source changed, then loads it.
    from AutoCua.web.agent import BrowserScanner, launch_chrome  # noqa: E402

    launch_chrome(args.port, False)
    if args.url:
        chrome_json(args.port, f"/json/new?{urllib.parse.quote(args.url, safe='')}", "PUT")
        time.sleep(1)

    sc = BrowserScanner(args.port, None, args.out, False)
    sc.set_marks(args.marks)
    sc.start()

    print()
    print("Chrome is open. Browse anywhere manually — clicking and typing are")
    print("yours to do; this only reads the page.")
    print(HELP)

    while True:
        try:
            line = input("AutoCua> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        cmd, _, arg = line.partition(" ")
        cmd, arg = (cmd or "s").lower(), arg.strip()
        try:
            if cmd in ("q", "quit", "exit"):
                break
            elif cmd in ("h", "help", "?"):
                print(HELP)
            elif cmd in ("t", "tabs"):
                print(sc.tabs())
            elif cmd in ("u", "use"):
                print("-> " + sc.bind_tab(int(arg)))
            elif cmd in ("m", "marks"):
                main.marks = not getattr(main, "marks", args.marks)
                sc.set_marks(main.marks)
                print(f"marks: {main.marks}")
            elif cmd in ("c", "config"):
                sc.reload_config()
                print("config reloaded")
            elif cmd in ("s", "scan"):
                t0 = time.time()
                summary = sc.scan_elements()
                tree, shot, tabs = sc.get_scan_data()
                print(tree)
                print(f"\n{summary} | {(time.time() - t0) * 1000:.0f} ms wall")
                print(f"tree -> {args.out}/tree.txt")
                if shot:
                    print(f"shot -> {args.out}/shot.jpg")
            else:
                print(f"? {cmd}   (h for help)")
        except Exception as e:      # a bad scan must not end the session
            print(f"! {e}")

    sc.stop()
    print(f"browser still running on port {args.port}")


if __name__ == "__main__":
    main()
