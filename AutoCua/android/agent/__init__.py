"""The Android agent, implemented in Rust (agent_native), the same import
surface as the iOS agent package: AgentService and its formatter, plus the
scanner the agent reads the phone with (tree/test.py drives it by hand).

This __init__ is the loader plus the re-export facade, the copy of
AutoCua/web/agent/__init__.py. The build itself lives in AutoCua/utils/rust.py
beside the web agent's: a checkout (Cargo.toml beside the crate) builds or
refreshes the extension with cargo; a pip wheel ships it prebuilt, like the web
agent's (the release machine runs this very import before maturin packs the
wheel, and pyproject.toml's `include` carries the module), with no Cargo.toml
beside it, so cargo is never touched from a wheel.
"""

from AutoCua.utils.rust import ensure_android_agent_built

ensure_android_agent_built()

from AutoCua.android.agent_native import (   # noqa: E402
    AgentService,
    AgentResponseFormatter,
    UIElementScanner,
)

__all__ = [
    "AgentService",
    "AgentResponseFormatter",
    "UIElementScanner",
]
