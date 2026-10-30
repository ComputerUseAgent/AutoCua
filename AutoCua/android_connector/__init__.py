# Android connector package: Google's adb and the phone it talks to (adb.py),
# fetched once, and the phone helper (stream.py starts Helper.java) that
# takes its screenshots and reads its UI tree. A real phone on USB with USB
# debugging on, or one Settings connected that is on Wi-Fi with Wireless
# debugging on, or an emulator that emulator.py boots (Google's emulator and
# images, fetched once); no agent. Reading the screenshots and the tree
# lives in AutoCua/android/tree/element.rs.
