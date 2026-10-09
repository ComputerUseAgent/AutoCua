# Every run_agent flag (desktop app, speed, headless Chrome, parallel tasks,
# iOS simulators, ...) is documented in agent_capabilities.md.
from AutoCua.agent_launcher import run_agent

run_agent(
    mode="computer use",
    provider="google",
    model="gemini-3.8-flash",
    task="""
play a good ideo on sfari please 
""",
    save_conversation=True,
    speed="fast",
    ui=True,
)
