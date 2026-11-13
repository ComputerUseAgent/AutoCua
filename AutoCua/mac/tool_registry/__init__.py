"""The macOS main driver's tool registry: every tool it can call, and the
LLMManager that hands them to the model.

The registry is Rust (service.rs, in place of service.py), in the mac part
of the one Rust extension, AutoCua.agent_native (the crate is
AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it). The manager it
subclasses stays the shared Python one, AutoCua/llm_provider/llm_manager.py,
the same on every platform.

This __init__ is its loader, like AutoCua/ios/tool_registry/__init__.py. The
Rust module is put in sys.modules under its old dotted name,
AutoCua.mac.tool_registry.service, so every import of that name keeps
working unchanged. What service.py did when it was imported is done here, at
that same moment and in the same order: its import lines bind their names on
the module, then the Rust makes the field tables, the tools (through that
same _tool), the defaults, the name sets and the LLMManager subclass.
Loading the extension imports none of them.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import mac as _native  # noqa: E402
    _service = _native.tool_registry.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no tool registry part: it was built
    # before this code. An app that was open during an update keeps the one
    # it loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'mac.tool_registry' part (the Mac agent's tool registry). "
        "Close every AutoCua window and start again.") from _error


def _bind_service():
    """What service.py's lines did: its import lines bound on the module,
    then everything it made, made by the Rust in the same order."""
    from AutoCua import data_extraction
    from AutoCua.llm_provider.llm_manager import (LLMManager as _LLMManager, NOTE_TOOLS, REASONING_OPTIONAL,
                                                   _tool, drop_bundled_terminal, tool_calls_to_steps)
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
