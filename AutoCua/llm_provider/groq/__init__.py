# AutoCua/llm_provider/groq/__init__.py
#
# The Groq provider is Rust (view.rs and service.rs, in place of view.py and
# service.py), in the llm_provider part of the one Rust extension,
# AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
# AutoCua/utils/rust.py builds it). The models' table, MODEL_MAPPINGS, is at
# the top of view.rs: one block per model.
#
# This __init__ is its loader, like AutoCua/vault/__init__.py. Each Rust
# module is put in sys.modules under its old dotted name,
# AutoCua.llm_provider.groq.view and AutoCua.llm_provider.groq.service, so
# every import of those names keeps working unchanged (the LLM manager, the
# app's model list). What each .py did when it was imported is done here, in
# the order the old imports ran it (this file imported service.py first, and
# service.py does not import view.py): service's import lines bind their
# names on the module and the Rust runs the rest of service.py's lines (the
# class and the function); then view.py, which imported nothing, has the Rust
# run its lines (the table and the function). Loading the extension imports
# none of them.

import sys as _sys

try:
    # The extension is already loaded and fresh: llm_provider/__init__.py
    # built and imported it before this package.
    from AutoCua.agent_native import llm_provider as _native  # noqa: E402
    _view = _native.groq.view
    _service = _native.groq.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no llm_provider.groq part: it was built
    # before this code. An app that was open during an update keeps the one
    # it loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'llm_provider.groq' part (the Groq models and the provider that "
        "sends their requests). Close every AutoCua window and start "
        "again.") from _error


def _bind_service():
    """What service.py's lines did: its import lines bound on the module,
    then everything it made, made by the Rust in the same order."""
    import json
    import requests
    import base64
    from typing import Dict, Any, Optional
    from .. import entry_value, http_timeout, out_cap, screenshots
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

from .service import GroqProvider  # noqa: E402


def _bind_view():
    """What view.py's lines did (it had no import line): everything it made,
    made by the Rust in the same order."""
    _view._build()
    del _view._build


# The same for view.
if _view.__name__ not in _sys.modules:
    _sys.modules[_view.__name__] = _view
    try:
        _bind_view()
    except BaseException:
        _sys.modules.pop(_view.__name__, None)
        raise
    view = _view
del _bind_service, _service, _bind_view, _view, _native, _sys

from .view import MODEL_MAPPINGS, get_model_info  # noqa: E402

__all__ = ['GroqProvider', 'MODEL_MAPPINGS', 'get_model_info']
