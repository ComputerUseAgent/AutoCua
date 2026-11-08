# Every run_agent flag (desktop app, speed, headless Chrome, parallel tasks,
# iOS simulators, ...) is documented in agent_capabilities.md.
from dotenv import load_dotenv

from AutoCua.agent_launcher import run_agent

load_dotenv()

run_agent(
    mode="coder",
    provider="aws",
    model="grok-4.7",
    task="add 2 number",
    save_conversation=True,
    external_terminal=True,
    ui=True
)

