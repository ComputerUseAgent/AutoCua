"""Memory compression and accounting for the agents, in Rust.

MemoryTracker measures the MAIN agent's current context-window fullness (the
live "memory bar" gauge): the exact size of the latest prompt sent, cached
tokens included. CompressionController is the runtime side of the handoff
compression agent (agent/service): shared by every platform agent, it owns
the 110k trigger, the background worker, the indicator and the splice policy.

This __init__ is the loader. The three modules are Rust (controller.rs,
memory_tracker.rs and agent/service.rs, each in place of the .py it
replaced), the memory_compression part of the one Rust extension,
AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
AutoCua/utils/rust.py builds it). Each is put in sys.modules under its old
dotted name, AutoCua.memory_compression.controller, .memory_tracker and
.agent.service, and set as an attribute here, so every import of those names
keeps working unchanged, Python's and the Rust agents' alike.
"""

import sys as _sys

from AutoCua.utils.rust import ensure_built as _ensure_built

# A checkout builds or refreshes the extension with cargo. A pip wheel ships
# it prebuilt (no Cargo.toml in it), so cargo is never touched from a wheel.
_ensure_built()

from AutoCua.agent_native import memory_compression as _native  # noqa: E402

controller = _native.controller
memory_tracker = _native.memory_tracker
_sys.modules[__name__ + ".controller"] = controller
_sys.modules[__name__ + ".memory_tracker"] = memory_tracker
_sys.modules[__name__ + ".agent.service"] = _native.agent.service

from .memory_tracker import MemoryTracker  # noqa: E402
from .controller import CompressionController  # noqa: E402
# The agent package, as before (importing the controller imported it).
from . import agent  # noqa: E402,F401

__all__ = ["MemoryTracker", "CompressionController"]
