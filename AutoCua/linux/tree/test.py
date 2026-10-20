#!/usr/bin/env python3

"""
Standalone test runner for element.py scanner (Linux via AT-SPI2).
Run from project root:  python3 -m AutoCua.linux.tree.test [seconds] [-v]

    seconds   countdown before the scan — focus the window you want
              (default 5; 0 scans whatever is focused now)
    -v        timing breakdown, geometry and the untracked-role backlog

Artifacts land in debug/iteration_N/: tree.txt and annotated_screenshot.png.
The whole dev loop — preflight, scan, audit — lives in element.py (cmd_scan);
this file only triggers it. To drive one stage in isolation:

    python3 element.py topmost      topmost-window detection only
    python3 element.py screenshot   screen capture only
    python3 element.py walk         element walk only, no drawing
"""

import sys
import os

# Ensure project root is on path when run directly
sys.path.insert(0, os.path.abspath(
    os.path.join(os.path.dirname(__file__), '..', '..', '..')))

import AutoCua.linux.tree.element as element

# Force debug flags on
element.DEBUG = True
element.SCREENSHOT = True


def main():
    element.cmd_scan(sys.argv[1:])


if __name__ == "__main__":
    main()
