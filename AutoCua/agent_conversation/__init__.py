"""Permanent (resumable) chat memory for the UI path.

All conversation-memory management lives in this package: saving, loading and
optimizing a session's conversation, and the on-disk session folders + index.
The agent stays a thin one-shot loop; app.py talks ONLY to ConversationService
(load a session -> trigger the main agent -> save the run). main.py / cli.py
never import this package, so they remain direct one-shot entry points.

The service is Rust (service.rs), its part of the one Rust extension,
AutoCua.agent_native.agent_conversation (the crate is AutoCua/Cargo.toml).
This __init__ is the loader, like AutoCua/coder/__init__.py: a checkout builds
or refreshes the extension with cargo (AutoCua/utils/rust.py); a module
shipped without a target/ folder beside it is used as it is. The Rust module
is put in sys.modules as AutoCua.agent_conversation.service, its old name, so
`from AutoCua.agent_conversation.service import conversation` keeps working.
"""

import sys

from AutoCua.utils.rust import ensure_built

ensure_built()

from AutoCua.agent_native import agent_conversation as service  # noqa: E402

sys.modules[__name__ + ".service"] = service

ConversationService = service.ConversationService
conversation = service.conversation

__all__ = ["ConversationService", "conversation"]
