"""The coder agent in Rust, one copy for macOS, Windows and Linux, laid out like AutoCua/android.

This __init__ is the loader plus the re-export facade, the copy of
AutoCua/android/agent/__init__.py. The build itself lives in
AutoCua/utils/rust.py beside the other Rust agents': a checkout (Cargo.toml
beside the crate) builds or refreshes the extension with cargo; a module
shipped without a target/ folder beside it is used as it is.
"""

from AutoCua.utils.rust import ensure_coder_built

ensure_coder_built()

from AutoCua.coder.agent_native import AgentService   # noqa: E402

__all__ = ["AgentService"]
