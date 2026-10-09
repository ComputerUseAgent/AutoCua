"""Permanent (resumable) chat memory for the UI path.

All conversation-memory management lives in this package — saving, loading and
optimizing a session's conversation, and the on-disk session folders + index.
The agent stays a thin one-shot loop; app.py talks ONLY to ConversationService
(load a session -> trigger the main agent -> save the run). main.py / cli.py
never import this package, so they remain direct one-shot entry points.
"""

from .service import ConversationService, conversation

__all__ = ["ConversationService", "conversation"]
