# AutoCua/ios/controller/tool/__init__.py
"""The iPhone controller's tools: open_app, videoplayer and shell.

All three are Rust (open_app.rs, videoplayer.rs and shell.rs, in place of
their .py files), in the ios part of the one Rust extension,
AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
AutoCua/utils/rust.py builds it).

This __init__ is their loader, like AutoCua/ios/agent/iphone_use/__init__.py.
Each Rust module is put in sys.modules under its old dotted name,
AutoCua.ios.controller.tool.<name>, so every import of those names keeps
working unchanged. What each .py did when it was imported is done here, at
that same moment and in the same order (open_app, videoplayer, shell): its
import lines bind their names on the module (the Rust reads them from there
each time), then its logger is made, and for open_app its one launcher,
app_launcher_service. Loading the extension imports none of them.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import ios as _native  # noqa: E402
    open_app = _native.controller.tool.open_app
    videoplayer = _native.controller.tool.videoplayer
    shell = _native.controller.tool.shell
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no tool part: it was built before this
    # code. An app that was open during an update keeps the one it loaded
    # when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'ios.controller.tool' part (the iPhone agent's app launcher, video "
        "player and shell). Close every AutoCua window and start again.") from _error


def _load(module, bind):
    """Put `module` in sys.modules under its old name, then do what its .py
    did when it was imported (`bind`). A failure leaves it out of
    sys.modules, as a failed import of the .py did."""
    _sys.modules[module.__name__] = module
    try:
        bind()
    except BaseException:
        _sys.modules.pop(module.__name__, None)
        raise


def _bind_open_app():
    """What open_app.py's lines did: each import bound on the module, then
    its logger, then its one launcher."""
    import difflib
    import json
    import os
    import subprocess
    import tempfile
    import time
    import requests
    import logging
    from pathlib import Path
    for name, value in list(locals().items()):
        setattr(open_app, name, value)
    open_app.logger = logging.getLogger(open_app.__name__)
    # Create global instance
    open_app.app_launcher_service = open_app.AppLauncherService()


def _bind_videoplayer():
    """What videoplayer.py's lines did: each import bound on the module,
    then its logger. Its constants are made with the module."""
    import requests
    import time
    import logging
    import re
    import xml.etree.ElementTree as ET
    for name, value in list(locals().items()):
        setattr(videoplayer, name, value)
    videoplayer.logger = logging.getLogger(videoplayer.__name__)


def _bind_shell():
    """What shell.py's lines did: each import bound on the module, then its
    logger."""
    import logging
    from AutoCua.coder import Sandbox
    for name, value in list(locals().items()):
        setattr(shell, name, value)
    shell.logger = logging.getLogger(shell.__name__)


_load(open_app, _bind_open_app)
from .open_app import app_launcher_service  # noqa: E402
_load(videoplayer, _bind_videoplayer)
from .videoplayer import VideoPlayerService  # noqa: E402
_load(shell, _bind_shell)
from .shell import ShellService  # noqa: E402
del _load, _bind_open_app, _bind_videoplayer, _bind_shell

__all__ = ['app_launcher_service', 'VideoPlayerService', 'ShellService']
