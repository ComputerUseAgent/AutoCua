"""The scratchpad, the Linux agent's numbered notes (milestone.md).

The service is Rust (service.rs, in place of service.py), in the linux part of
the one Rust extension, AutoCua.agent_native (the crate is
AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it).

This __init__ is the loader, like AutoCua/ios/controller/scratchpad/__init__.py. The
Rust module is put in sys.modules under its old dotted name,
AutoCua.linux.controller.scratchpad.service, and set as an attribute here, so
every import of that name keeps working unchanged.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import linux as _native  # noqa: E402
    service = _native.controller.scratchpad.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no scratchpad part: it was built before
    # this code. An app that was open during an update keeps the one it
    # loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'linux.controller.scratchpad' part (the Linux agent's scratchpad). "
        "Close every AutoCua window and start again.") from _error

_sys.modules[__name__ + ".service"] = service

from .service import ScratchpadService  # noqa: E402

__all__ = ['ScratchpadService']
