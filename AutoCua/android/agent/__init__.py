"""The Android agent, implemented in Rust (agent_native), the same import
surface as the iOS agent package: AgentService and its formatter, plus the
scanner the agent reads the phone with (tree/test.py drives it by hand).

This __init__ is the loader plus the re-export facade, the copy of
AutoCua/web/agent/__init__.py: the Android agent is its part of the one Rust
extension, AutoCua.agent_native.android (the crate is AutoCua/Cargo.toml). The
build itself lives in AutoCua/utils/rust.py: a checkout builds or refreshes
the extension with cargo; a pip wheel ships it prebuilt (maturin builds it),
with no Cargo.toml in it, so cargo is never touched from a wheel.
"""

from AutoCua.utils.rust import ensure_built

ensure_built()

from AutoCua.agent_native.android import (   # noqa: E402
    AgentService,
    AgentResponseFormatter,
    UIElementScanner,
)

__all__ = [
    "AgentService",
    "AgentResponseFormatter",
    "UIElementScanner",
]
