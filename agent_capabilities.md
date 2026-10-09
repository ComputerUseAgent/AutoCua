# Agent Capabilities — every `run_agent` setting

Reference for anyone (human or AI agent) editing [main.py](main.py). Everything
`main.py` does is one call to `run_agent(...)`, and this file covers every
argument that call takes.

**Pass every flag straight into the call:** `save_conversation=True`,
`headless=True`, `speed="fast"`. There is no need to define a variable first
and pass that in. The only values worth a variable of their own are the task
strings of a parallel run (Part 2), and `mode` if you switch it often.

## The four required arguments

```python
from AutoCua.agent_launcher import run_agent

run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open Google Chrome and click the second search result",
)
```

| Argument | What it is |
|---|---|
| `mode` | Which agent runs: `"computer use"` drives this desktop (Windows, macOS or Linux, detected automatically), `"shell use"` runs the coding agent straight in your terminal, `"web use"` drives Chrome, `"mobile use, ios"` drives an iPhone/iPad or an iOS Simulator. Case, underscores and commas don't matter; hyphens do: `"computer-use"` raises `ValueError`. |
| `provider` | `"openrouter"`, `"groq"`, `"openai"`, `"anthropic"`, `"google"`, `"perplexity"`, `"together"`, `"cerebras"` or `"aws"`. The key comes from `<PROVIDER>_API_KEY` in the environment or `.env` (e.g. `OPENROUTER_API_KEY`, or `AWS_API_KEY` for a Bedrock API key), or from `api_key=` (Part 9). Google's `-vertex` models take no key: they use `VERTEX_PROJECT_ID` / `VERTEX_LOCATION` and your Google Cloud credentials. |
| `model` | A model name from [model_list.txt](model_list.txt), copied exactly. It isn't checked up front: an unknown name is sent to the provider as-is and fails at the first request. |
| `task` | What the agent should do. |

All four are required even with `ui=True`, where the app ignores them (Part 7).

## Optional flags

| Flag | Applies to | Default | Part |
|---|---|---|---|
| `ui` | every mode | `False` (run `task` in the terminal) | 7 |
| `save_conversation` | every mode | `False` | 4 |
| `speed` | `"computer use"`, `"web use"`, `"mobile use, ios"` | `"quality"` | 3 |
| `external_terminal` | `"computer use"`, `"shell use"` | `False` | 8 |
| `headless` | `"web use"` | `False` (visible Chrome) | 1 |
| `browser_port` | `"web use"` | `9222` | 1 |
| `browser_profile` | `"web use"`, single task | the `"default"` profile | 1 |
| `extra_tasks` | `"web use"`; `"mobile use, ios"` with `device="simulation"` | none (one task) | 2 |
| `device` | `"mobile use, ios"` | `"simulation"` | 6 |
| `ios_version` | `"mobile use, ios"` + simulation | newest installed runtime | 6 |
| `sim_device` | `"mobile use, ios"` + simulation | `"iphone"` | 6 |
| `app_build` | `"mobile use, ios"` + simulation | `None` (install nothing) | 6 |
| `api_key` | every mode, single task | `None` (key from `.env`) | 9 |
| `os` | `mode="mobile use"` written without a device | `None` | 9 |

> A flag the chosen mode doesn't use is **silently ignored**, never an error, so
> it is safe to leave in when you switch `mode`; it just does nothing there. The
> exceptions raise `ValueError`: `extra_tasks` in any other mode, and
> `extra_tasks` or `app_build` together with `device="hardware"`. With `ui=True`
> every other argument is ignored (Part 7).

---

# Part 1 — Web use: headless, port and profile

## Headless (`headless`)

`"web use"` drives a real Chrome via CDP. **Headless** means Chrome runs
**without a visible window**: the agent still loads pages, clicks, types and
takes screenshots exactly the same way, you just don't see a browser on screen.

| | Headful (default) | Headless |
|---|---|---|
| Chrome window | Visible on screen | Hidden, no window at all |
| You can watch the agent | ✅ Yes | ❌ No (check `conversation/` + logs) |
| Screen clutter / tab flicker | Yes, esp. with parallel tasks | None |
| Agent behaviour | Same | Same |

**Default: `headless=False`**, a visible window. The web `AgentService` and
`launch_chrome` both default to it, so if you don't pass `headless` you get a
headful browser.

```python
run_agent(
    mode="web use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="find the cheapest flight from London to Paris next Friday",
    headless=True,          # "web use" only: Chrome without a window
)
```

Pass a real `True` / `False`. Remove the argument (or pass `False`) to see the
browser again.

⚠️ **The flag only applies when Chrome is launched.** Chrome deliberately stays
up after a run, and the next run **attaches** to whatever is already on the
port, headless or not, ignoring the flag (you'll see "Attached to Chrome
already on port 9222"). So if you switch between headless and headful, quit
the old Chrome first. macOS / Linux:

```
pkill -f "remote-debugging-port=9222"
```

Windows (PowerShell):

```powershell
Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -match 'remote-debugging-port=9222' } | ForEach-Object { Stop-Process -Id $_.ProcessId }
```

When to use which:

- **Headful (default)**: first runs, debugging, demos where you want to watch
  the agent work.
- **Headless**: long unattended runs, servers/CI, and **parallel tasks** (see
  Part 2), where N agents share one window, each in a background tab of its own.

## DevTools port (`browser_port`)

The port the agent launches Chrome on, or attaches to when a Chrome is already
listening there. **Default: `9222`.** Pass a whole number:

```python
run_agent(
    mode="web use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="check the weather in Tokyo",
    browser_port=9223,      # another tool already holds 9222
)
```

Use it when something else already owns `9222`. Avoid `9333`: that is the
headless browser behind the `web` tool on Together, Cerebras and AWS (Part 5). A
parallel run (Part 2) launches its one shared Chrome on this port too.

## Chrome profile (`browser_profile`)

Names a Chrome profile that **persists between runs**, so logins, cookies and
site settings survive: it lives in
`AutoCua_data/browser_profiles/<name>/chrome/`. **Default: the `"default"`
profile**, which persists the same way.

```python
run_agent(
    mode="web use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="check my unread messages on LinkedIn",
    browser_profile="work",   # logins kept apart from the default profile
)
```

- Names use letters (lowercased), digits, `.`, `_` and `-`, at most 64
  characters, not starting with `.`; `_tmp` is reserved. An invalid name does
  not raise: Chrome silently gets `~/.AutoCua/chrome-<port>` instead.
- It is chosen when Chrome is **launched**. Attaching to a Chrome already on
  the port keeps whatever profile that Chrome has open.
- Single task only: parallel runs always use `"default"`.

---

# Part 2 — Running multiple tasks in parallel

Works in two modes, with the same `extra_tasks` flag:

| | `"web use"` | `"mobile use, ios"` + `device="simulation"` |
|---|---|---|
| What each task gets | Its own web agent, pinned to **its own tab** | Its own **iOS Simulator** + its own WebDriverAgent |
| Shared resource | **Your own Chrome**, through the AutoCua extension (headless: one browser of the agent's own) | Nothing shared, since a phone screen can't be split |
| Ports | None with a window (each agent has a line of its own to the extension); one DevTools port headless | One WDA port per task (8100, 8101, 8102, …) |
| Limit | Practical (tabs + RAM) | Practical (RAM): a simulator is created when every installed one is taken |

Not supported with `device="hardware"`: there is only one paired device, so
extras raise a `ValueError` telling you to use simulation.

## How it works

When you pass extra tasks, **all** tasks, including the main `task`, run in
parallel, each as its own child process.

- Live terminal output is prefixed per task: `[task 1] ...`, `[task 2] ...`
- On the web, the browser's front tab shows a cover page for the whole run (the AutoCua
  video and "Multiple agents running", with the number of agents). The agents work in
  tabs behind it and never switch the front; click a tab to watch one.
- One `Ctrl+C` stops every task at once.
- A summary prints when every task has finished.
- On iOS the parent boots every simulator and starts every WebDriverAgent
  **before** any agent runs, and shuts them all down at the end: success,
  error, or `Ctrl+C`.

## How to write it in main.py

Write each task as its own string, then pass the first as `task` and the rest
as a list in `extra_tasks`. There is no `task_2=` argument: the extras only
reach the agent through `extra_tasks`. `None` and empty strings in the list
are skipped, and it must be a list (a bare string would be split into single
characters).

```python
task = """
find the price of the iPhone 17 on apple.com
"""
task_2 = """
search for the current weather in London and note the temperature
"""
task_3 = """
open github.com and find the trending Python repositories today
"""

run_agent(
    mode="web use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task=task,                      # task 1, runs in parallel with the rest
    extra_tasks=[task_2, task_3],   # tasks 2..N: add more strings for more tasks
    headless=True,                  # recommended for parallel runs (Part 1)
)
```

On iOS, give it the simulator flags instead of `headless`:

```python
run_agent(
    mode="mobile use, ios",
    provider="openrouter",
    model="gemini-3.8-flash",
    task=task,
    extra_tasks=[task_2, task_3],
    device="simulation",            # the default, and what parallel needs ("hardware" raises)
    sim_device="iphone",            # optional: "iphone", "ipad" or an exact simulator name
    app_build="MyApp-Simulator.ipa",  # optional: installed on every simulator at once (Part 6)
)
```

Only some flags reach the parallel tasks. Web: `save_conversation`, `speed`,
`headless` and `browser_port`. iOS: `save_conversation`, `speed`,
`ios_version`, `sim_device` and `app_build`. Everything else
(`external_terminal`, `api_key`, `browser_profile`) is dropped, and each task
reads its API key from `.env`.

## Where results and logs go

Each task runs inside its own working directory under `./parallel/`:

```
parallel/
├── task_1/
│   ├── result.json        # {"status": "success"|"incomplete"|"error"|"stopped", "message": ...}
│   ├── conversation/      # if save_conversation=True
│   ├── raw_reasoning/
│   └── debug/
├── task_2/
└── task_3/
```

⚠️ `./parallel/` is **deleted and recreated on every parallel run**, so copy
out anything you want to keep before running again.

`run_agent` returns a combined dict:

```python
{"status": "success"|"error", "message": "...",
 "results": [{"task": "...", "status": "...", "message": "..."}, ...]}
```

## Tips

- **Headless (web):** with a visible window, N agents share one browser window,
  each in a background tab of its own; the window keeps showing whatever tab
  was in front, so click a tab to watch that agent. Pass `headless=True`
  (Part 1) for unattended runs.
- **One task = simpler path:** if every entry in `extra_tasks` is `None` or
  empty, the normal single-agent path runs, with no `parallel/` folder and no
  child processes.
- **Independent tasks only:** the agents don't talk to each other. Write each
  task so it stands alone; don't make task 3 depend on task 2's result.
- **iOS: how many tasks can I run?** As many as the Mac has RAM for. With
  `sim_device="iphone"` (the default) or `"ipad"`, each task claims a free
  simulator of that kind and one is created when all are taken. An exact
  simulator name can only be claimed once, so asking for more tasks than there
  are simulators by that name stops with "Not enough simulators".
- **iOS: the first parallel run is slower.** Each task builds WebDriverAgent into
  its own folder (`build-sim`, `build-sim-2`, …) because concurrent builds
  sharing one folder re-sign the same app underneath each other and kill one
  another's test runner. Task 1 reuses the build you already have, so only the
  extra tasks pay, and only once (~270 MB each, all gitignored).
- **iOS: watch them work.** Every simulator shows in one "iOS Simulator"
  window, side by side, instead of Simulator.app windows stacked on top of
  each other. They are all shut down when the run ends.
- **iOS: your own app on every simulator.** Pass `app_build` (Part 6). Each
  simulator starts installing it the moment it has booted, all at the same
  time, never one after another, and no agent starts until every simulator
  has it.

## Under the hood

Implemented in [AutoCua/agent_launcher.py](AutoCua/agent_launcher.py):
`run_agent(..., extra_tasks=[...])` filters out empty entries and, if any
remain, hands `[task] + extras` to either `run_parallel_web_agents()`, with one
`python -m AutoCua.web.agent` child per task against the shared Chrome port,
or `run_parallel_sim_agents()`, with one simulator + one WebDriverAgent
(`WDA_PORT` 8100, 8101, …) + one `python -m AutoCua.ios.agent` child per task.
Each iOS child gets its port via `AutoCua_WDA_PORT` and its own
`AutoCua/ios/scratchpad/task_N/` via `AutoCua_IOS_SESSION`, so parallel agents
never share a screen, a port, or a notes folder. You can also call either
function directly from your own script.

---

# Part 3 — Speed mode (`speed`)

## What it is

`speed` picks between two ways of running the main agent loop:

| | `"quality"` (default) | `"fast"` |
|---|---|---|
| System prompt | `system_prompt.md` (full) | `fast_system_prompt.md` (leaner) |
| Per-step notes | `thinking` + `memory` + `next_goal` (the `reasoning` tool) | `next_goal` only (the `memory` tool) |
| Reasoning per step | More deliberate | Lighter: fewer tokens, quicker steps |
| Best for | Complex / multi-step / unfamiliar tasks | Simple, well-defined tasks where latency matters |

Both modes have the same action tools. `fast` swaps the notes the model writes
each step for `next_goal` alone and uses a shorter prompt, so each step is
cheaper and quicker. In `"web use"` it goes further: the model gets the
element tree without the screenshot, and the on-page cursor no longer glides
between actions.

## Which modes support it

| Mode | `speed` supported? |
|---|---|
| `"computer use"` (Windows, macOS, Linux) | ✅ `"quality"` / `"fast"` |
| `"web use"` | ✅ `"quality"` / `"fast"` |
| `"mobile use, ios"` | ✅ `"quality"` / `"fast"` |
| `"shell use"` | ❌ Ignored |

The coding agents that `"computer use"` hands work to don't take `speed`
either; it applies to the main agent only.

## Default

**`speed="quality"`.** Only `"fast"` (any case) selects fast mode; any other
value means quality. If you don't pass `speed` at all, you get quality mode.

```python
run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open Notepad and type hello",
    speed="fast",           # "quality" (the default) or "fast"
)
```

## When to use which

- **`"quality"` (default)**: first runs, anything multi-step, tasks where the
  agent needs to plan or recover from surprises. Start here.
- **`"fast"`**: short, well-defined tasks you've seen the agent do reliably
  ("open X and click Y", "read the number on this screen") where you want
  lower latency and fewer tokens per step.

## Under the hood

Each platform's `AgentService.__init__` (e.g.
[AutoCua/windows/agent/main_driver/service.py](AutoCua/windows/agent/main_driver/service.py))
normalises `speed`, loads `fast_system_prompt.md` or `system_prompt.md` from
the same folder, and passes the mode to `LLMManager`, which sends the
`MAIN_TOOLS_FAST` registry (notes tool `memory`) or `MAIN_TOOLS` (notes tool
`reasoning`). The web agent does the same in Rust
([AutoCua/web/agent/main_driver/service.rs](AutoCua/web/agent/main_driver/service.rs)),
and falls back to quality only if its `fast_system_prompt.md` is missing.

---

# Part 4 — Conversation saving (`save_conversation`)

## What it is

`save_conversation` makes the agent write a readable log of **exactly what it
sent to the LLM at every step**: the system prompt, the interleaved
assistant/user turns, and the response it got back. It's the tool for
debugging "why did the agent do that?" and for reviewing a run after the fact.

Works in **every mode**.

## Default

**`save_conversation=False`: no conversation log.** Two things still touch the
disk either way: every `"computer use"`, `"web use"` and `"mobile use, ios"` run
clears `./conversation/`, `./raw_reasoning/` and `./debug/` when it starts, so
don't keep your own files there; and the coding agents that `"computer use"`
hands work to always log under `./cli_conversation/`.

## What it writes (when `True`)

Files land in the **current working directory**, wherever you ran
`python main.py` from:

```
conversation/
├── conversation.txt        # session header, started fresh each run
├── conversation_1.txt      # step 1: full payload sent + response received
├── conversation_2.txt      # step 2 ...
└── ...
raw_reasoning/              # raw LLM outputs per step (cleared each run)
```

Each `conversation_N.txt` is a "memory snapshot", a faithful peek at what the
agent could see at step N. You'll also see
`Memory snapshot saved: conversation_N.txt` in the terminal after each step.

⚠️ Both folders are **reset at the start of every run**, so copy anything you
want to keep before running again.

- **`"shell use"`** writes `./cli_conversation/<session id>/conversation_N.txt`
  with a `raw_reasoning/` folder inside it instead, one folder per run, and
  never clears old ones.
- **Parallel runs** (Part 2) write each task's copy under
  `./parallel/task_N/`.

## How to turn it on in main.py

```python
run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open Notepad and type hello",
    save_conversation=True,   # writes conversation/ + raw_reasoning/
)
```

Remove the argument (or pass `False`) to stop writing logs.

## When to use which

- **`False` (default)**: normal runs and demos; skips the per-step file
  writes.
- **`True`**: debugging a task that goes wrong, tuning prompts, or when you
  need an audit trail of what the agent saw and decided at each step.

## Under the hood

Each `AgentService.__init__` (e.g.
[AutoCua/windows/agent/main_driver/service.py](AutoCua/windows/agent/main_driver/service.py))
resets `conversation/` and `raw_reasoning/` and, when the flag is on, calls
`_save_conversation_snapshot()` after every LLM step to write
`conversation_N.txt`. The web agent does the same in Rust
(`save_conversation_snapshot` in
[AutoCua/web/agent/main_driver/service.rs](AutoCua/web/agent/main_driver/service.rs)).

---

# Part 5 — Together AI provider (`provider="together"`)

## What it is

Together AI is an OpenAI-compatible provider with native tool calling and
image input. Available in every mode, on every platform. Set
`TOGETHER_API_KEY` in `.env` for a terminal run; a key saved in the app's
Settings → API Keys is used only by the app (`ui=True`).

| `model=` | Together model id |
|---|---|
| `inkling` | `thinkingmachines/Inkling` |
| `muse-glimmer-30b` | `meta-models/Muse-Glimmer-30B` |
| `minimax-m3` | `MiniMaxAI/MiniMax-M3` |

## How the `web` tool works on Together

Together has no native web search. When the desktop or iOS agent calls its `web` tool
with `provider="together"`, the query is handed to the **browser agent**
(`AutoCua/web`) running **headless on the same model**, and its final
`done` report comes back as the web result. Expect that step to take a few
minutes rather than seconds.

- Runs in its own headless Chrome on port **9333** (`AutoCua_WEB_FALLBACK_PORT`)
  with its own profile **web_fallback** (`AutoCua_WEB_FALLBACK_PROFILE`), never
  the visible one `"web use"` uses, so it can't disturb the desktop agent's
  screen.
- Wall-clock cap **15 min** (`AutoCua_WEB_FALLBACK_TIMEOUT`, seconds); the
  Stop button / Ctrl+C interrupts it.
- Each run's `result.json` + `agent.log` are kept under
  `AutoCua_data/web_fallback/<run-id>/` for inspection.

Implementation: `AutoCua/{mac,windows,linux,ios}/controller/tool/web/web_agent.py` (identical on all four).

## Cerebras (`provider="cerebras"`)

Cerebras works the same way: an OpenAI-compatible provider with native tool calling and image
input on every platform, and no native web search, so its `web` tool also goes to the headless
browser agent described above. Set `CEREBRAS_API_KEY` in `.env` for a terminal run; a key
saved in the app's Settings → API Keys is used only by the app (`ui=True`).

| `model=` | Cerebras model id |
|---|---|
| `qwen-3.8-27b` | `qwen-3.8-27b` |

Every request sends `temperature` 1, `top_p` 0.95, `seed` 42 and `max_completion_tokens` 32768 (10000 for the memory-compression handoff).
`reasoning_effort` is `high` for the coder and its minions and `low` for every other agent.

## AWS Bedrock (`provider="aws"`)

Amazon Bedrock through its Converse API, with native tool calling and image input on every
platform. It has no native web search, so its `web` tool also goes to the headless browser
agent described above. Set `AWS_API_KEY` (a Bedrock API key) in `.env` for a terminal run; a
key saved in the app's Settings → API Keys is used only by the app (`ui=True`).

Every model is a `global.` cross-Region inference profile: the call goes in through one
regional endpoint, `AWS_BEDROCK_REGION` in `.env` (default `eu-west-2`), and AWS runs it in
whichever region has capacity.

| `model=` | Bedrock model id |
|---|---|
| `kimi-k3` | `global.moonshotai.kimi-k3` |
| `grok-4.7` | `global.xai.grok-4.7` |
| `claude-opus-4.7` | `global.anthropic.claude-opus-4-7` |
| `claude-opus-4.6` | `global.anthropic.claude-opus-4-6-v1` |
| `claude-sonnet-4.6` | `global.anthropic.claude-sonnet-4-6` |

Claude gets `effort` `low` and prompt caching, with a 4000-token output cap. Kimi K3 and Grok 4.7
get reasoning `low`, with a 10000-token cap. The memory-compression handoff gets 10000 on all five.
No sampling parameter is sent.

---

# Part 6 — iOS device target (`"mobile use, ios"`)

## What it is

`device` picks WHAT the iOS agent drives:

| | `"simulation"` (default) | `"hardware"` |
|---|---|---|
| Runs on | An iOS Simulator on this Mac | The paired iPhone or iPad (the most recently paired one) |
| iOS version | `ios_version` picks the runtime (e.g. `"26.5"`) | Whatever the device runs; `ios_version` is ignored |
| Needs | Full Xcode plus its iOS simulator platform, a separate one-time download of about 8.5 GB (`bash setup_ios.sh` offers it, or `xcodebuild -downloadPlatform iOS`): no signing, no Apple account, no pairing | One-time pairing in Settings → Connect Device (Team ID signing, WDA install) |
| First start | Builds WebDriverAgent for the simulator once, a few minutes; later runs boot + attach in under a minute | Seconds (WDA is pre-installed at pairing) |
| Agent behaviour | Same: identical WDA endpoints, tree, taps, screenshots | Same |

`"simulator"` is accepted as another spelling of `"simulation"`; any other
value raises `ValueError`.

## Default

**`device="simulation"`.** Omitting it boots the best installed simulator.
Hardware is the explicit opt-in.

## Choosing the simulator (`ios_version`, `sim_device`)

Both apply to `device="simulation"` only; with `"hardware"` they are ignored
with a printed notice.

- **`ios_version`** picks the runtime, matched by prefix: `"26"` takes any 26.x,
  `"26.5"` takes 26.5. **Default:** an already-booted simulator if one fits,
  otherwise the newest runtime your Xcode can build for. Naming a version
  that isn't installed **downloads it** first (`xcodebuild -downloadPlatform`,
  several GB, up to `AutoCua_IOS_DOWNLOAD_TIMEOUT` = 5400 s); set
  `AutoCua_IOS_AUTO_DOWNLOAD=0` to make that an error instead.
- **`sim_device`** picks the simulator: `"iphone"` (the default) or `"ipad"`
  take any free simulator of that kind and create one if there is none, or
  pass an exact simulator name such as `"iPad Pro 11-inch (M5)"` (any case).
  A partial name is treated as an exact one and fails.

## How to use it in main.py

```python
run_agent(
    mode="mobile use, ios",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open Settings and turn on Dark Mode",
    device="simulation",    # or "hardware" for the paired iPhone/iPad
    ios_version="26.5",     # simulation only; leave out for the newest installed
    sim_device="iphone",    # simulation only: "iphone", "ipad" or an exact simulator name
)
```

These flags are ignored by every other mode, so they're safe to leave in place
when you switch `mode`.

`device="simulation"` is also what unlocks **parallel tasks** on iOS, since
each task gets its own simulator. See Part 2.

## Installing your own app (`app_build`), simulation only

> ⚠️ **`app_build` works with `device="simulation"` ONLY.** It installs onto an
> iOS Simulator. With `device="hardware"` the run stops straight away with a
> `ValueError`; it never touches your paired device.

Want the agent to test **your** app? Drop the build into
`AutoCua_data/app_build/` and name the file in `app_build`. It is installed on
the simulator before the agent starts, so the agent finds it on the home screen
like any other app.

**1. Put the build in the folder:**

```
AutoCua_data/
└── app_build/
    └── MyApp-Simulator.ipa      # or MyApp.app
```

`AutoCua_data/` sits next to `main.py` when you run `python main.py` from the
repo (in your home folder for the packaged app, in the working directory for a
pip install).

**2. Name it in `main.py`:**

```python
run_agent(
    mode="mobile use, ios",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open MyApp and sign up with a test account",
    device="simulation",                 # required: app_build is simulation only
    app_build="MyApp-Simulator.ipa",     # exact file name in AutoCua_data/app_build/
)
```

Leaving `app_build` out (the default, `None`) installs nothing.

| | |
|---|---|
| Accepted files | `.ipa` or `.app` |
| Must be | A **simulator** build (built for `iphonesimulator`). An App Store / device `.ipa` is built for a real iPhone and won't run on the simulator. |
| When it installs | The moment the simulator has booted, **at the same time** as WebDriverAgent starts, not after it |
| Agent starts | Only once WebDriverAgent is ready **and** the app is installed |
| Parallel tasks (Part 2) | Every simulator installs it at once, each as soon as it has booted |
| After the run | The simulator shuts down; the app stays on it, and the next run installs over it |

### No app, no run

The agent never starts on a simulator that doesn't have the app. Each of these
stops the run with a clear message (and shuts the simulator down if it had
already booted):

- the file isn't in `AutoCua_data/app_build/`, caught **before** any simulator
  boots, so you don't wait minutes to find out
- the `.ipa` has no `.app` inside, or the app has no bundle id
- `xcrun simctl install` fails
- the simulator still doesn't list the app 2 minutes after installing
- `app_build` is set together with `device="hardware"`

### How to get a simulator build

In Xcode, pick an iOS Simulator as the run destination and build (⌘B); the
`.app` is under *Products* (right-click → Show in Finder). From the command
line:

```
xcodebuild -scheme MyApp -sdk iphonesimulator -configuration Debug -derivedDataPath build
# → build/Build/Products/Debug-iphonesimulator/MyApp.app
```

Copy that `.app` into `AutoCua_data/app_build/`, or an `.ipa`, which is just
the `.app` zipped inside a `Payload/` folder.

## When to use which

- **`"simulation"` (default)**: day-to-day runs, testing tasks, no phone on
  the desk, or when you need a specific iOS version.
- **`"hardware"`**: anything that needs the real device: real apps with your
  logins, camera, cellular, notifications, App Store apps not in the simulator.
  With several devices paired, the most recently paired one is used.

## Under the hood

The launcher branches in [AutoCua/agent_launcher.py](AutoCua/agent_launcher.py):
hardware keeps the existing `wda_session` (pymobiledevice3 USB forward +
xcuitest launch of the pre-installed WDA). Simulation uses
[AutoCua/ios_connector/sim_session.py](AutoCua/ios_connector/sim_session.py):
`xcrun simctl` resolves/boots the device, then one unsigned
`xcodebuild test -destination "platform=iOS Simulator,id=<udid>"` serves WDA on the same
`localhost:8100` the agent already talks to, so the whole agent stack is
unchanged. App scanning switches from `pymobiledevice3 apps list` to
`xcrun simctl listapps` automatically. The simulator runs headless: the run
is shown in our own window ([sim_view.py](AutoCua/ios_connector/sim_view.py)),
one device in the middle or several side by side, each screen being that
WDA's live MJPEG stream (port 9100, 9101, ...). On teardown the window
closes, WDA is stopped and the simulator is **shut down**: it's not a real
device, so nothing is left running after the agent terminates.

`app_build` lives in [AutoCua/agent_launcher.py](AutoCua/agent_launcher.py): `_unpack_app_build()` finds the build and
unpacks an `.ipa` once, before any simulator boots; as soon as
`sim_session.activate()` returns (simulator booted, WDA starting),
`_install_in_background()` runs `xcrun simctl install` on its own thread and
polls `xcrun simctl get_app_container` until the app is listed. The launcher
waits for that thread before the agent starts and re-raises any failure. For
parallel tasks each simulator's boot thread starts its own install.

---

# Part 7 — UI mode (`ui`)

## What it is

`main.py` is the only entry point. By default it runs `task` in the terminal;
with `ui=True` it opens the **desktop app** instead (the native window with
the chat, stages, skills and settings) and returns when that window closes.

| | `ui=False` (default) | `ui=True` |
|---|---|---|
| What runs | `task` from `main.py`, in the terminal | The desktop app |
| Mode / model / task | From `main.py` | Chosen inside the app |
| Other flags in `run_agent(...)` | Honoured | Ignored |
| Needs | An API key | An API key (Settings → API Keys works too) |

## Default

**`ui=False`: terminal.** Nothing opens; the agent prints its steps to the
terminal exactly as described in the parts above.

## How to turn it on in main.py

```python
run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="",                # still required, but the app has its own task box
    ui=True,                # open the desktop app instead of running the task
)
```

`mode`, `provider`, `model` and `task` are still required, but the app ignores
them, along with every other flag (`speed`, `save_conversation`,
`external_terminal`, …). The app has its own mode, model, task and speed
controls, but nothing for `save_conversation`, `external_terminal`,
`headless`, parallel tasks or the iOS simulator options: those work only in a
terminal run.

## Under the hood

`run_agent` checks `ui` before anything else. When it is `True` it imports
`AutoCua.ui.service` and calls its `main()`: the order-sensitive process
bootstrap runs at that import, Flask starts on port 5000 in a daemon thread
(`127.0.0.1` on every OS, never the LAN, and it refuses requests from other
hostnames or websites; any other process already on port 5000 is stopped first), the pywebview window
opens on the main thread, and the call returns
once the window is closed. The import only happens in UI mode, so a terminal
run never loads Flask or pywebview.

---

# Part 8 — Coding agents in their own windows (`external_terminal`)

## What it is

`"computer use"` hands coding work to a **coding agent**, which can split it
further across **minions**. By default they run hidden in the background and
their output is not shown in the `main.py` terminal. With
`external_terminal=True` each one opens in **its own terminal window**, so you
can watch it work:

| Platform | Window |
|---|---|
| Windows | A new console window |
| macOS | A Terminal.app window |
| Linux | The first of `gnome-terminal`, `ptyxis`, `konsole`, `xfce4-terminal`, `x-terminal-emulator`, `xterm` that is installed |

In `"shell use"` the coding agent already runs in your terminal, so the flag
only opens its minions in their own windows.

## Default

**`external_terminal=False`**: coding agents and minions run in the
background.

```python
run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="build a small website about cats and open it in the browser",
    external_terminal=True,   # watch each coding agent and minion in its own window
)
```

- Ignored by `"web use"`, `"mobile use, ios"`, parallel runs and `ui=True`.
- A run that never hands work to a coding agent opens no windows.
- Linux needs one of the terminals listed above installed: with none of them,
  the run waits on a coding agent that never started.
- On macOS and Linux the windows stay open after the coding agent or minion
  finishes, so its output can still be read; close them yourself. On Windows
  each console window closes as soon as its agent exits.

---

# Part 9 — `api_key` and `os`

## `api_key`

Gives the provider's key for this run only, instead of reading
`<PROVIDER>_API_KEY` from the environment or `.env`:

```python
run_agent(
    mode="computer use",
    provider="openrouter",
    model="gemini-3.8-flash",
    task="open Notepad and type hello",
    api_key="sk-or-...",      # overrides OPENROUTER_API_KEY for this run
)
```

- **Default: `None`**, so the key comes from `.env`. Prefer `.env`: a key in
  `main.py` is easy to commit by accident, and this one is also passed on the
  command line of every coding agent and minion the run starts, where other
  programs on the machine can see it.
- Single task only: parallel runs drop it, and each task reads `.env`.
- Google models served through Vertex use their own credentials and ignore it.

## `os`

Only for `mode="mobile use"` written **without** the device:
`mode="mobile use", os="ios"` is the same as `mode="mobile use, ios"`. A
device written in `mode` wins, and `os` never picks the host OS for
`"computer use"` or `"shell use"`, which is always detected automatically.
**Default: `None`.** Writing `mode="mobile use, ios"` is simpler.
