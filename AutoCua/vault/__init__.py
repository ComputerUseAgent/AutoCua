"""Vault module for managing credentials"""

# AutoCua/vault/__init__.py
#
# The vault is Rust (service.rs, in place of service.py), in the vault part
# of the one Rust extension, AutoCua.agent_native (the crate is
# AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it).
#
# This __init__ is its loader, like AutoCua/ios/tool_registry/__init__.py.
# The Rust module is put in sys.modules under its old dotted name,
# AutoCua.vault.service, so every import of that name keeps working
# unchanged (the iPhone controller and its scanner, and the Android agent's
# Rust). What service.py did when it was imported is done here, at that same
# moment and in the same order: its import lines bind their names on the
# module, then the Rust runs the rest of its lines (the logger, the
# constants, the functions, the class and the one vault_service, which loads
# the credentials). Loading the extension imports none of them.

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import vault as _native  # noqa: E402
    _service = _native.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no vault part: it was built before
    # this code. An app that was open during an update keeps the one it
    # loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'vault' part (the saved credentials the phone agents type). "
        "Close every AutoCua window and start again.") from _error


def _bind_service():
    """What service.py's lines did: its import lines bound on the module,
    then everything it made, made by the Rust in the same order."""
    import json
    import os
    import re
    import time
    import logging
    import xml.etree.ElementTree as ET
    from pathlib import Path
    from urllib.parse import quote, quote_plus

    import requests
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

from .service import vault_service  # noqa: E402

__all__ = ['vault_service']
