"""Browser Agent Child Runner
==========================
Subprocess entry for one browser agent started by the main agent
(../browser_agent/service.py spawns it; it is run BY PATH, never imported).

It runs the browser agent the way parallel `browser` runs do
(AutoCua/browser/agent/__main__.py): the agent starts Chrome itself (or attaches
to the one already up) and is pinned to one background tab of its own
(single_tab=True), so up to four browser agents run at once and never work in
each other's tabs. The first one opens Chrome: browser_agent.start holds the
next ones until it has (its run_start event in events.jsonl).

One file serves macOS, Windows and Linux (AutoCua/universal_tool).

    --session-id scopes the web scratchpad to scratchpad/{sid}/, so the
      launcher can read this run's notes and clear them afterwards.
    --browser-port / --browser-profile are for tests only. Left out, the agent
      uses its default port and its default automation profile, as web use does.
    The process_request result dict is always written to --result.

The app shows the run live, in its box in the main agent's chat: the agent's
tool-flow events and its screens go to events.jsonl and frame.jpg next to
--result (the run's folder), where the main agent's controller reads them.

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

# This file only starts the runner. The runner itself is Rust (service.rs
# beside this file), in the universal_tool part of the one Rust extension,
# AutoCua.agent_native (the crate is AutoCua/Cargo.toml, and
# AutoCua/utils/rust.py builds it). Its module,
# AutoCua.universal_tool.browser_agent_child.service, has this file's old
# names: FRAME_EDGE, FRAME_QUALITY, _LiveView and main.

import sys
from pathlib import Path

# Run by path, so Python put THIS folder first on the import path. Its files
# (service.py ...) must never shadow a real module.
_HERE = Path(__file__).resolve().parent
sys.path[:] = [p for p in sys.path if Path(p or ".").resolve() != _HERE]

if __name__ == "__main__":
    # A checkout builds or refreshes the extension with cargo first. A pip
    # wheel ships it prebuilt (no Cargo.toml in it), so cargo is never
    # touched from a wheel.
    from AutoCua.utils.rust import ensure_built  # noqa: E402
    ensure_built()
    from AutoCua.agent_native import universal_tool  # noqa: E402
    universal_tool.browser_agent_child.service.main()
