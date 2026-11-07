"""The coder agent in Rust, one copy for macOS, Windows and Linux, laid out like AutoCua/android.

This __init__ is the loader plus the re-export facade, the copy of
AutoCua/android/agent/__init__.py: the coder is its part of the one Rust
extension, AutoCua.agent_native.coder (the crate is AutoCua/Cargo.toml). The
build itself lives in AutoCua/utils/rust.py beside the other Rust agents': a
checkout builds or refreshes the extension with cargo; a module shipped
without a target/ folder beside it is used as it is.

The build and the import happen on first use of AgentService, not when the
package is imported: `python -m AutoCua.coder` (computer use's coder child)
then reaches __main__.py, which writes the result file even when the build
fails, so a parent with no process handle never waits for one that will not
come.
"""

__all__ = ["AgentService"]


def __getattr__(name):
    if name != "AgentService":
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    from AutoCua.utils.rust import ensure_built

    ensure_built()
    from AutoCua.agent_native.coder import AgentService

    globals()["AgentService"] = AgentService
    return AgentService
