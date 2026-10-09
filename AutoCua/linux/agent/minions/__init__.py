# Minion sub-agent package.
# Mirrors the coder/ package structure:
#   - service.py        : full agent loop (read-only scout variant)
#   - view.py           : MinionResponseFormatter (next_goal-shape JSON validator)
#   - __main__.py       : subprocess entry — `python -m ...agent.minions`
#   - system_prompt.md  : read-only scout system prompt

from .service import AgentService
from .view import MinionResponseFormatter

__all__ = ['AgentService', 'MinionResponseFormatter']
