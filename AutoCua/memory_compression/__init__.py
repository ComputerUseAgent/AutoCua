"""Memory compression / accounting for the agent.

MemoryTracker measures the MAIN agent's current context-window fullness (the
live "memory bar" gauge) — the exact size of the latest prompt sent, including
cached tokens. CompressionController is the runtime side of the handoff
compression agent (agent/service.py): shared by every platform agent, it owns
the 110k trigger, the background worker, the indicator, and the splice policy.
"""

from .memory_tracker import MemoryTracker
from .controller import CompressionController

__all__ = ["MemoryTracker", "CompressionController"]
