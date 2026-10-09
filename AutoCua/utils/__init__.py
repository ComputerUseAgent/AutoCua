"""One-time setup steps the code does for itself.

A checkout gets these from a shell script — MacOS_setup.sh / windows_setup.bat
build the Rust extension, ios_setup.sh clones WebDriverAgent. A `pip install`
runs neither: a wheel is unpacked, never executed. So each step also has to be
reachable from the first run that needs it, which is what lives here.

    rust.py        compile AutoCua/web (no-op from a wheel — it ships prebuilt)
    wda.py         clone WebDriverAgent for iOS
    ocr_models.py  fetch the PP-OCRv6 models for the Linux scanner's OCR

All are cheap no-ops once satisfied: a timestamp check, a directory check, a
file-size check.

One module here is not a setup step but sits with them because it is the same
shape — something the whole app reaches for, owned by no single platform
package:

    agent_glow.py  the desktop look while an agent is driving the machine
                   (lavender cursor + breathing screen-edge glow), paired to
                   the start and stop of every run
"""
