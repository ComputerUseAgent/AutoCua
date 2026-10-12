# Every run_agent flag (desktop app, speed, headless Chrome, parallel tasks,
# iOS simulators, ...) is documented in agent_capabilities.md.
from AutoCua.agent_launcher import run_agent

run_agent(
    mode="web use",
    provider="anthropic",
    model="claude-opus-5.5",
    task="""
Use scrape mode to gather what people think about the new iPhone 18 Pro from the past month only.

Check 2 different subreddits, including posts, top comments, replies, and upvote counts. Also, gather a few relevant posts from X.com, including replies, likes, and repost counts.

Provide a concise summary of common opinions, praise, and complaints, with source links.
""",
    save_conversation=True,
    ui=True,
)
