# Every run_agent flag (desktop app, speed, headless Chrome, parallel tasks,
# iOS simulators, ...) is documented in agent_capabilities.md.
from AutoCua.agent_launcher import run_agent

run_agent(
    mode="computer use",
    provider="google",
    model="gemini-3.8-flash",
    task="""
Open Google Flights.
1. .Click the “Round trip” dropdown, press the Down arrow and then Enter to select “One way.” Select 30 October and click “Done.”
2. Click the “Round trip” dropdown, press the Down arrow and then Enter to select “One way.” Select 30 October and click “Done.”
3. Click “Search.”
no  subagent
""",
    save_conversation=True,
)
