# Structure Notes

- `windows`, `mac`, `ios` should remain structurally similar. This makes it easier to adapt features across platforms.
- Do not import code from `windows` into `mac`, or from `mac` into `windows`.
- Release binaries must remain platform-specific and separate. Importing cross-platform code into them can create build and runtime problems.
- No code mixing between platform-specific implementations should be allowed.

- Tool definitions stay in each platform's `tool_registry/service.py` (web: `tool_registry/service.rs`).
- The LLM endpoints and model tables live once, in `AutoCua/llm_provider/` (Python), shared by every platform, web included. Change a provider or a model there, never per platform.
- The Python manager in `AutoCua/llm_provider/llm_manager.py` is shared by windows, mac, linux and ios. Web's manager is Rust (`web/tool_registry/service.rs`), so a change to retries, fallback or usage goes in both.

- Do not add any provider that does not support web functionality for build or tool features.
- Only add models that either have built-in web capability or can support web access through a provider integration.
- A provider with no native web-search API is acceptable only when the platform's `web` tool routes the query to the browser agent (`AutoCua/web`) on the same provider+model and returns its report — see `<platform>/controller/tool/web/web_agent.py` (identical on mac/windows/ios; Together AI is the first such provider).
