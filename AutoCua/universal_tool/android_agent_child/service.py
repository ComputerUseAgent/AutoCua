"""Android Agent Child Runner
==========================
Subprocess entry for one android_agent run started by the main agent
(../android_agent/service.py spawns it; it is run BY PATH, never imported).

It runs the Android agent (the Rust extension in AutoCua/android) on the phone
the parent checked: AutoCua_ANDROID_SERIAL names it, and the agent's screen
reader starts the phone's helper itself and stops it when this process ends.

The process_request result dict is always written to --result.

The app shows the run live, in its box in the main agent's chat: the agent's
tool-flow events and the phone's screens go to events.jsonl and frame.jpg next
to --result (the run's folder), where the main agent's controller reads them.

Exit codes: 0 = agent returned (any status), 1 = crash, 130 = Ctrl+C.
"""

# The runner itself is Rust (service.rs beside this file), in the
# universal_tool part of the one Rust extension, AutoCua.agent_native (the
# crate is AutoCua/Cargo.toml, and AutoCua/utils/rust.py builds it). Its
# module, AutoCua.universal_tool.android_agent_child.service, holds what this
# file held: FRAME_EDGE, FRAME_QUALITY, _LiveView, _watch_parent and main.
# This file only starts it.

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
    universal_tool.android_agent_child.service.main()
