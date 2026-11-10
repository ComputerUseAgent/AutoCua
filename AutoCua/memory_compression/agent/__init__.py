"""The handoff compression agent, in Rust (service.rs beside this file).

The memory_compression package runs first and puts the Rust module in
sys.modules as AutoCua.memory_compression.agent.service, so the imports below
find it under its old name. system_prompt.md, also beside this file, is the
compressor's instructions, read at run time.
"""

from . import service
from .service import MemoryCompressionAgent

__all__ = ["MemoryCompressionAgent"]
