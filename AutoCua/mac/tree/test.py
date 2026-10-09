"""
Standalone test runner for element.py scanner.
Run from project root:  python -m AutoCua.mac.tree.test
"""

import sys
import os
import time

# Ensure project root is on path when run directly
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..'))

import AutoCua.mac.tree.element as element
from AutoCua.mac.tree.element import UIElementScanner, ELEMENT_CONFIG, AXIsProcessTrusted

# Force debug flags on
element.DEBUG = True
element.SCREENSHOT = True


def main():
    print("=== Element Scanner Test ===")

    if not AXIsProcessTrusted():
        print("\nAccessibility permission required.")
        print("Grant in: System Settings > Privacy & Security > Accessibility")
        sys.exit(1)

    # Countdown — switch to the window you want to scan
    for i in range(5, 0, -1):
        print(f"  Scanning in {i}...")
        time.sleep(1)

    print("\nScanning now...\n")
    t0 = time.time()

    scanner = UIElementScanner(ELEMENT_CONFIG)
    scanner.scan_elements()
    scan_time = time.time() - t0

    tree_text, image_b64, _ = scanner.get_scan_data()
    mapping = scanner.get_elements_mapping()
    total_time = time.time() - t0

    print("Done scanning.")
    print(f"Application : {scanner.application_name}")
    print(f"Elements    : {len(mapping)}")
    print(f"Image       : {'yes' if image_b64 else 'no'}")
    print(f"Scan time   : {scan_time:.2f}s (scan_elements)")
    print(f"Total time  : {total_time:.2f}s (incl. tree + screenshot build)")
    print(f"\nDebug saved to: debug/iteration_{scanner._debug_iteration}/")


if __name__ == "__main__":
    main()
