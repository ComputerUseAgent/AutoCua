"""The computer-use agent of macOS: its loop and its transcript codec.

Both are Rust: the agent loop (service.rs, in place of service.py) and the
transcript codec, the compression dump and the terminal display of a step
(view.rs, in place of view.py), in the mac part of the one Rust extension,
AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
AutoCua/utils/rust.py builds it). The three prompts beside them are read at
run time.

This __init__ is the loader, like AutoCua/memory_compression/__init__.py. The
Rust modules are put in sys.modules under their old dotted names,
AutoCua.mac.agent.computer_use.service and .view, and set as attributes here,
so every import of those names keeps working unchanged.

The Rust modules read the names the .py files imported from the rest of
AutoCua (the tool registry, the controller, the scanner, the skills and so
on) as their own attributes, each time they use them. They are bound here, by
the same import lines in the same order, at the moment the old modules ran
them: when this package is imported. Loading the extension imports none of
them, so it loads on every host and in every process.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import mac as _native  # noqa: E402
except ImportError as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no mac part: it was built before this
    # code. An app that was open during an update keeps the one it loaded
    # when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no 'mac' part "
        "(the computer-use agent). Close every AutoCua window and start again.",
        name=_error.name, path=_error.path) from _error

service = _native.agent.computer_use.service
view = _native.agent.computer_use.view
_sys.modules[__name__ + ".service"] = service


def _bind_view():
    """What view.py's import line did, then the module under its old name."""
    from ....memory_compression.agent.service import wrap_handoff, extract_handoff
    view.wrap_handoff = wrap_handoff
    view.extract_handoff = extract_handoff
    _sys.modules[__name__ + ".view"] = view


def _bind_service():
    """What service.py's import lines did: each name bound on the module."""
    from ...tool_registry.service import (LLMManager, tool_calls_to_steps, drop_bundled_terminal,
                                             MAIN_TOOL_NAMES, MAIN_TOOL_NAMES_FAST, MAIN_TOOL_NAMES_CLAUDE,
                                             MAIN_ACTION_DEFAULTS, MAIN_ACTION_DEFAULTS_FAST,
                                             MAIN_ACTION_DEFAULTS_CLAUDE,
                                             MAIN_REASONING_FIELDS, MAIN_NOTES_FIELDS_FAST,
                                             CLAUDE_REASONING_FIELDS, NOTE_TOOLS,
                                             SCREEN_TOOLS)
    from ....memory_compression.controller import CompressionController
    # service.py imported view here, and view.py its own names with it.
    _bind_view()
    from .view import (AgentResponseFormatter, decode_step, encode_step,
                       decode_results, encode_results, wire_calls_from,
                       snapshot_turn, compression_dump, compression_entry,
                       _looks_native, SKIPPED_THINKING, REASONING_RESULT)
    from ...tree.element import UIElementScanner, ELEMENT_CONFIG, get_screen
    from ...controller import ControllerView
    from ...controller.view import PHONE_AGENTS
    from ..skills import DomainKnowledgeService
    from .... import data_extraction
    from ....utils.chrome import find_chrome
    for name, value in list(locals().items()):
        setattr(service, name, value)


try:
    _bind_service()
except BaseException:
    # As a failed import of service.py left it: not in sys.modules.
    _sys.modules.pop(__name__ + ".service", None)
    raise
del _bind_service, _bind_view

from .service import AgentService  # noqa: E402
from .view import AgentResponseFormatter  # noqa: E402

__all__ = ['AgentService', 'AgentResponseFormatter']
