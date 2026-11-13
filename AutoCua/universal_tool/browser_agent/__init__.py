"""The browser_agent sub-agent tool of computer use (macOS, Windows, Linux):
service.py. Its child process is ../browser_agent_child/service.py."""

# AutoCua/universal_tool/browser_agent/__init__.py
#
# The tool is Rust (service.rs, in place of service.py), in the
# universal_tool part of the one Rust extension, AutoCua.agent_native (the
# crate is AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it).
#
# This __init__ is its loader, like AutoCua/vault/__init__.py. The Rust
# module is put in sys.modules under its old dotted name,
# AutoCua.universal_tool.browser_agent.service, so every import of that name
# keeps working unchanged (the Mac, Windows and Linux controllers, the Mac's
# ios_agent tool, the android_agent tool, and the app's close hook). What
# service.py did when it was imported is done here, at that same moment and
# in the same order: its import lines bind their names on the module, then
# the Rust runs the rest of its lines (the paths, the constants, the class,
# the runs and the cover, the functions, and atexit.register(stop_all)).
# Loading the extension imports none of them.

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import universal_tool as _native  # noqa: E402
    _service = _native.browser_agent.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no universal_tool part: it was built
    # before this code. An app that was open during an update keeps the one
    # it loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'universal_tool' part (the browser_agent sub-agent tool of computer "
        "use). Close every AutoCua window and start again.") from _error


def _bind_service():
    """What service.py's lines did: its import lines bound on the module,
    then everything it made, made by the Rust in the same order."""
    import atexit
    import ctypes
    import json
    import os
    import shutil
    import subprocess
    import sys
    import threading
    import time
    import uuid
    from pathlib import Path

    from AutoCua import data_root, IS_COMPILED
    for name, value in list(locals().items()):
        setattr(_service, name, value)
    _service._build()
    del _service._build


# As the import system did: a name already in sys.modules (a reload of this
# package) is left as it is, and a failure leaves it out.
if _service.__name__ not in _sys.modules:
    _sys.modules[_service.__name__] = _service
    try:
        _bind_service()
    except BaseException:
        _sys.modules.pop(_service.__name__, None)
        raise
    service = _service
del _bind_service, _service
