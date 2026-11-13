# AutoCua/ios/controller/__init__.py
"""The iPhone agent's controller: the action router (view) and the WDA
service (service), with the scratchpad, the todo list and the tools beside
them.

The service and the view are Rust (service.rs and view.rs, in place of
their .py files), in the ios part of the one Rust extension,
AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
AutoCua/utils/rust.py builds it).

This __init__ is their loader, like AutoCua/ios/controller/tool/__init__.py.
Each Rust module is put in sys.modules under its old dotted name,
AutoCua.ios.controller.service and .view, so every import of those names
keeps working unchanged (the scanner, ios/tree/element.py, imports
controller_service; the vault imports TypingRefused). What each .py did when
it was imported is done here, at that same moment and in the same order:
view.py's import lines ran first, and its first one imported service.py,
which bound its own imports, made its logger and its one controller_service.
Then view.py's other imports (the todo list, the tools, the vault, and the
scratchpad, the shell and the web tool when they can be had), then its
logger. Each module becomes an attribute of this package once its lines
are done, as the import system made it one. Loading the extension imports
none of them. A module already in sys.modules under its old name is left
as it is, as the import system never ran a .py a second time.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import ios as _native  # noqa: E402
    _service = _native.controller.service
    _view = _native.controller.view
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no controller part: it was built before
    # this code. An app that was open during an update keeps the one it
    # loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'ios.controller' part (the iPhone agent's controller). "
        "Close every AutoCua window and start again.") from _error


def _load(module, bind):
    """Put `module` in sys.modules under its old name, then do what its .py
    did when it was imported (`bind`), then make it an attribute of this
    package, as the import system did once a .py had run. A failure leaves
    it out of sys.modules, as a failed import of the .py did. A name already
    in sys.modules (a reload of this package, or a retry after a later line
    failed) is left as it is: nothing is run and nothing is set, as the
    import system did not run the .py again."""
    if module.__name__ in _sys.modules:
        return
    _sys.modules[module.__name__] = module
    try:
        bind()
    except BaseException:
        _sys.modules.pop(module.__name__, None)
        raise
    globals()[module.__name__.rpartition(".")[2]] = module


def _bind_service():
    """What service.py's lines did: each import bound on the module, then
    its logger, then its one controller_service."""
    import requests
    import time
    import logging
    for name, value in list(locals().items()):
        setattr(_service, name, value)
    _service.logger = logging.getLogger(_service.__name__)
    # Create global instance
    _service.controller_service = _service.ControllerService()


def _bind_view():
    """What view.py's lines did: each import bound on the module (its first
    one loads the service), then its logger."""
    import logging
    import time
    import sys
    import threading

    _load(_service, _bind_service)
    from .service import controller_service
    from .task_tracker.service import TaskTrackerService
    from .tool.open_app import app_launcher_service
    from .tool.videoplayer import VideoPlayerService
    from ...vault.service import vault_service, MAX_PIN_ATTEMPTS, DEVICE_ENTRIES, TEXT_FIELDS

    # Guarded as view.py guarded them: a missing one makes its actions
    # return a clear error instead of breaking the controller.
    try:
        from .scratchpad.service import ScratchpadService
    except ImportError:
        ScratchpadService = None
    try:
        from .tool import ShellService
    except ImportError:
        ShellService = None
    try:
        from AutoCua.universal_tool.web import WebService
    except ImportError:
        WebService = None

    for name, value in list(locals().items()):
        setattr(_view, name, value)
    _view.logger = logging.getLogger(_view.__name__)


_load(_view, _bind_view)
del _load, _bind_service, _bind_view, _service, _view

# Controller module for action block code routes
from .view import ControllerView  # noqa: E402
from .task_tracker import TaskTrackerService  # noqa: E402
from .tool.open_app import app_launcher_service  # noqa: E402

__all__ = ['ControllerView', 'TaskTrackerService', 'app_launcher_service']
