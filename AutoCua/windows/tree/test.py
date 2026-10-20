"""
Manual test harness for element.py.

Runs a 5-second countdown, then drives the same three-call sequence the agent
uses in production (scan_elements -> save_to_file -> get_scan_data) with
DEBUG force-enabled, so the element tree and annotated screenshot are written
to debug/element/ and debug/screenshot/ under the current working directory.

Run from the repo root:
    python -m AutoCua.windows.tree.test
"""

import time

from . import element
from .element import UIElementScanner, ELEMENT_CONFIG


def main():
    # Force-enable DEBUG so UIElementScanner writes to debug/element/ and
    # debug/screenshot/. We patch the module attribute rather than editing
    # element.py so production code stays untouched.
    element.DEBUG = True

    for i in range(5, 0, -1):
        print(f"Scanning in {i}...")
        time.sleep(1)

    print("Starting scan now!\n")
    start = time.perf_counter()
    scanner = UIElementScanner(ELEMENT_CONFIG)

    # scan_elements() only populates internal state. The debug artifacts are
    # produced by save_to_file() (tree .txt) and get_scan_data() (annotated
    # screenshot .png) -- matching the sequence AgentService uses in prod.
    scanner.scan_elements()
    scanner.save_to_file()
    scanner.get_scan_data()

    elapsed = time.perf_counter() - start
    print(f"\nScan complete in {elapsed:.2f}s. Check debug/element/ and debug/screenshot/ for output.")


if __name__ == "__main__":
    main()
