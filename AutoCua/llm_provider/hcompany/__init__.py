# AutoCua/llm_provider/hcompany/__init__.py
#
# The H Company provider (the Holo models, https://api.hcompany.ai/v1) is
# Rust (view.rs and service.rs), in the llm_provider part of the one Rust
# extension, AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
# AutoCua/utils/rust.py builds it). The models' table, MODEL_MAPPINGS, is at
# the top of view.rs: one block per model.
#
# This __init__ is its loader, the same as the other providers' (see
# llm_provider/groq/__init__.py, whose shape this provider has). Each Rust
# module is put in sys.modules under its dotted name,
# AutoCua.llm_provider.hcompany.view and
# AutoCua.llm_provider.hcompany.service, which is how the LLM manager and the
# app's model list import a provider. The service comes first: the names its
# Rust reads as module globals are bound on it, then the Rust makes the class
# and the function; then the view's Rust makes the table and its function.
# Loading the extension imports none of them.

import sys as _sys

try:
    # The extension is already loaded and fresh: llm_provider/__init__.py
    # built and imported it before this package.
    from AutoCua.agent_native import llm_provider as _native  # noqa: E402
    _view = _native.hcompany.view
    _service = _native.hcompany.service
except (ImportError, AttributeError) as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no llm_provider.hcompany part: it was built
    # before this code. An app that was open during an update keeps the one
    # it loaded when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no "
        "'llm_provider.hcompany' part (the H Company models and the provider "
        "that sends their requests). Close every AutoCua window and start "
        "again.") from _error


def _bind_service():
    """The names service.rs reads as module globals, bound on the module,
    then everything the module holds, made by the Rust."""
    import json
    import time
    import requests
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

from .service import HCompanyProvider  # noqa: E402


def _bind_view():
    """Everything the view holds, made by the Rust (it reads no imported
    name)."""
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

__all__ = ['HCompanyProvider', 'MODEL_MAPPINGS', 'get_model_info']
