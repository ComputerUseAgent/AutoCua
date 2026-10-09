"""Where WebDriverAgent's build products live — deliberately OUTSIDE the repo.

macOS guards ~/Desktop, ~/Documents and ~/Downloads behind TCC, so a checkout
in any of them makes the Simulator ask

    "SimulatorTrampoline.xpc" would like to access files in your Desktop folder

the first time it launches the runner out of the build folder. That prompt
cannot be scripted away — the dialog is drawn by a system process, synthetic
clicks are ignored on it, and the permission store is SIP-protected — and a
run that stops for a dialog is not automation. So the fix is to never touch a
guarded folder at all: Application Support needs no permission from anyone.

AutoCua_BUILD_DIR overrides the location (useful for a scratch disk or CI).
"""

import os
from pathlib import Path


def wda_build_root():
    """Root for every WebDriverAgent build product. Callers create what they need."""
    override = os.environ.get("AutoCua_BUILD_DIR", "").strip()
    if override:
        return Path(override).expanduser()
    return Path.home() / "Library" / "Application Support" / "AutoCua" / "wda-build"
