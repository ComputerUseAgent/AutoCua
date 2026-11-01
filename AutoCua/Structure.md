# Structure Notes

- `windows`, `mac`, `ios` should remain structurally similar. This makes it easier to adapt features across platforms.
- Do not import code from `windows` into `mac`, or from `mac` into `windows`.
- Release binaries must remain platform-specific and separate. Importing cross-platform code into them can create build and runtime problems.
- No code mixing between platform-specific implementations should be allowed.

- Tool definitions stay in each platform's `tool_registry/service.py` (web: `tool_registry/service.rs`).
- The LLM endpoints and model tables live once, in `AutoCua/llm_provider/` (Python), shared by every platform, web included. Change a provider or a model there, never per platform.
- The Python manager in `AutoCua/llm_provider/llm_manager.py` is shared by windows, mac, linux and ios. Web's manager is Rust (`web/tool_registry/service.rs`), so a change to retries, fallback or usage goes in both.

- If the model or provider does not support a native web tool, make sure the query goes to the web agent in headless mode.
- A provider with no native web-search API is acceptable only when the platform's `web` tool routes the query to the browser agent (`AutoCua/web`) on the same provider+model and returns its report — see `<platform>/controller/tool/web/web_agent.py` (identical on mac/windows/ios; Together AI is the first such provider).
- The web and Android agents are written in Rust, and the other agents in Python. They might be converted to Rust in the future.
- View files are mostly routing, and service files hold the core logic.
- Do not add any automatic download command: ask for consent in the terminal first. See the utils folder, `AutoCua/utils`.
- Kernel commands on Windows must always stay optional, since interception may affect the keyboard and mouse if a key gets stuck or a port issue occurs. (The issue has been fixed, but for user safety keep them optional, at the cost of a UAC prompt.)
