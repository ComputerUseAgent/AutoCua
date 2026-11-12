"""The web tool: one web search for every agent, in Rust.

Every agent's `web` action runs here: computer use on macOS, Windows and
Linux and the iPhone agent through their Python controllers
(`from AutoCua.web import WebService`), the Android agent and the coder
through their Rust controllers. WebService(provider, model, api_key=None,
vertex=False, vertex_project_id=None, vertex_location=None, stop_event=None)
.search(query) sends the query to the agent's own provider (service.rs, one
.rs per provider beside it, web.md their system prompt), or to the headless
browser agent for a provider with no web-search API (web_agent.rs), and
gives back the `{\\ntool: web,\\nstatus: ...}` text.

This __init__ is the loader, like AutoCua/computer/__init__.py. The Rust
modules are the web part of the one Rust extension, AutoCua.agent_native (the
crate is AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it).
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

try:
    from AutoCua.agent_native import web as _native  # noqa: E402
except ImportError as _error:
    if "AutoCua.agent_native" not in _sys.modules:
        raise
    # The extension is loaded but has no web part: it was built before this
    # code. An app that was open during an update keeps the one it loaded
    # when it started.
    raise ImportError(
        "AutoCua/agent_native is an older build than this code: it has no 'web' part "
        "(the web tool). Close every AutoCua window and start again.",
        name=_error.name, path=_error.path) from _error

WebService = _native.WebService

__all__ = ['WebService']
