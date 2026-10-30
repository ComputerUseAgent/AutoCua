"""One-time setup steps the code does for itself.

A checkout gets these from a shell script — setup_mac.sh / setup_windows.bat
build the Rust extension, setup_mac.sh --ios clones WebDriverAgent. A `pip install`
runs neither: a wheel is unpacked, never executed. So each step also has to be
reachable from the first run that needs it, which is what lives here.

    rust.py        compile AutoCua/web and AutoCua/android (no-op from a
                   wheel — both ship prebuilt)
    wda.py         clone WebDriverAgent for iOS
    ocr_models.py  fetch the PP-OCRv6 models for the Linux scanner's OCR
    electron.py    fetch Electron for the desktop app's Chromium window, once
                   the person says yes to it (the only one that asks first)

All are cheap no-ops once satisfied: a timestamp check, a directory check, a
file-size check.

Two modules here are not setup steps but sit with them because they are the
same shape — something the whole app reaches for, owned by no single platform
package:

    agent_glow.py  the desktop look while an agent is driving the machine
                   (lavender cursor + breathing screen-edge glow), paired to
                   the start and stop of every run
    sim_view.py    the window that shows the phones of a run: iOS simulators
                   and Android emulators alike, and a connected phone
"""
