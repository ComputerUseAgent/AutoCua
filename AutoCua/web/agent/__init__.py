"""The web agent — implemented in Rust (agent_native), same import surface as
the old Python package.

This __init__ is the loader plus the re-export facade. The build itself lives
in AutoCua/utils/rust.py, beside the WebDriverAgent fetch: both are the
one-time setup step a checkout gets from a shell script and a pip install has
to do for itself.
"""

from AutoCua.utils.rust import ensure_web_agent_built

# A checkout (Cargo.toml beside the crate) builds or refreshes the extension
# with cargo. A pip wheel ships it prebuilt (agent_native.abi3.so, no sources),
# so it is imported as it is — cargo is never touched from a wheel.
ensure_web_agent_built()

from AutoCua.web.agent_native import (   # noqa: E402
    AgentService,
    AgentResponseFormatter,
    BrowserScanner,
    launch_chrome,
    Bridge,
    ScannerError,
    CHROME_PORT,
)

__all__ = [
    "AgentService",
    "AgentResponseFormatter",
    "BrowserScanner",
    "launch_chrome",
    "Bridge",
    "ScannerError",
    "CHROME_PORT",
]
