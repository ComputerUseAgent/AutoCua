"""The browser agent — implemented in Rust (agent_native), same import surface as
the old Python package.

This __init__ is the loader plus the re-export facade: the browser agent is its
part of the one Rust extension, AutoCua.agent_native.browser (the crate is
AutoCua/Cargo.toml). The build itself lives in AutoCua/utils/rust.py, beside
the WebDriverAgent fetch: both are the one-time setup step a checkout gets from
a shell script and a pip install has to do for itself.
"""

from AutoCua.utils.rust import ensure_built

# A checkout (AutoCua/Cargo.toml) builds or refreshes the extension with
# cargo. A pip wheel ships it prebuilt (agent_native.abi3.so, no sources), so
# it is imported as it is — cargo is never touched from a wheel.
ensure_built()

from AutoCua.agent_native.browser import (   # noqa: E402
    AgentService,
    AgentResponseFormatter,
    BrowserScanner,
    launch_chrome,
    Bridge,
    idle_bridge_start,
    idle_bridge_stop,
    ScannerError,
    CHROME_PORT,
)

__all__ = [
    "AgentService",
    "AgentResponseFormatter",
    "BrowserScanner",
    "launch_chrome",
    "Bridge",
    "idle_bridge_start",
    "idle_bridge_stop",
    "ScannerError",
    "CHROME_PORT",
]
