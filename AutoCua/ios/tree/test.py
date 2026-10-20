"""
Standalone test runner for element.py scanner (iOS via WebDriverAgent).

File location : AutoCua/ios/tree/test.py

Run from project root (the AutoCua repo folder):

    python -m AutoCua.ios.tree.test

Or run the file directly from anywhere:

    python /path/to/AutoCua/AutoCua/ios/tree/test.py

Requires WebDriverAgent reachable at http://localhost:8100.
If WDA runs on the phone over USB, forward the port first:

    pymobiledevice3 usbmux forward 8100 8100
"""

import sys
import os
import time

# Ensure project root is on path when run directly
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..'))

import requests

import AutoCua.ios.tree.element as element
from AutoCua.ios.tree.element import UIElementScanner, ELEMENT_CONFIG, wda_url

# Force debug flags on
element.DEBUG = True


def main():
    print("=== Element Scanner Test (iOS) ===")

    # Check WebDriverAgent is reachable (iPhone connected + WDA running)
    try:
        requests.get(f"{wda_url}/status", timeout=5)
    except Exception as e:
        print(f"\nWebDriverAgent not reachable at {wda_url}")
        print("Make sure the iPhone is connected and WDA is running.")
        print(f"({e})")
        sys.exit(1)

    # Countdown — switch the iPhone to the screen you want to scan
    for i in range(5, 0, -1):
        print(f"  Scanning in {i}...")
        time.sleep(1)

    print("\nScanning now...\n")

    scanner = UIElementScanner(ELEMENT_CONFIG)
    scanner.scan_elements()

    _, image_b64 = scanner.get_scan_data()

    print(f"Elements    : {len(scanner.elements_mapping)}")
    print(f"Image       : {'yes' if image_b64 else 'no'}")


if __name__ == "__main__":
    main()
