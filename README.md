<div align="center">

<img src="AutoCua/logo/logo.png" alt="AutoCua" width="120"/>

<h1>AutoCua</h1>

<h3>Automatic Computer Use Agent.<br/>Mac, Windows, Linux, real phone, simulator, any browser.<br/>A multi-agent harness engine.</h3>

[![PyPI](https://img.shields.io/pypi/v/AutoCua?color=4c6ef5&label=pypi)](https://pypi.org/project/AutoCua/)
[![Python](https://img.shields.io/badge/python-3.10%2B-4c6ef5)](https://pypi.org/project/AutoCua/)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux%20%7C%20iOS%20%7C%20Chrome-6c757d)](https://github.com/AutoCua/AutoCua/blob/main/README.md#requirements-and-setup)
[![License](https://img.shields.io/badge/license-MIT-31a24c)](https://github.com/AutoCua/AutoCua/blob/main/LICENSE)

[![Desktop input](https://img.shields.io/badge/desktop%20input-real%20hardware%20events-e8590c)](https://github.com/AutoCua/AutoCua/blob/main/README.md#how-it-acts-on-a-screen)
[![Browser agent](https://img.shields.io/badge/browser%20agent-Rust%20%2B%20a%20Chrome%20extension-b7410e)](https://github.com/AutoCua/AutoCua/blob/main/README.md#the-browser-agent)
[![Models](https://img.shields.io/badge/models-48%20across%209%20providers-7048e8)](https://github.com/AutoCua/AutoCua/blob/main/README.md#providers-and-models)

<table><tr><td>

```bash
pip install AutoCua
```

</td></tr></table>

| | Windows | Linux | macOS |
|:--|:--:|:--:|:--:|
| **Computer use** — any desktop app, with real mouse and keyboard input | ✓ | ✓ | ✓ |
| **Browser skills** — Chrome, Firefox, Edge, Brave and Opera | ✓ | ✓ | ✓ |
| **Shell use** — a coding agent in a real terminal | ✓ | ✓ | ✓ |
| **Web use** — your own Chrome, driven through the AutoCua extension | ✓ | ✓ | ✓ |
| **Mobile use** — your iPhone, iPad or an iOS Simulator | — | — | ✓ |

<sub>Windows 10 and 11 · Linux with GNOME · macOS on Apple Silicon or Intel</sub>

**Full capabilities:** **[agent_capabilities.md](https://github.com/AutoCua/AutoCua/blob/main/agent_capabilities.md)**

</div>

---

## Web agent: fast mode

6.1 seconds for a flight search. Agent mode: web.

https://github.com/user-attachments/assets/8cc797d5-5556-40a9-bbfc-5d6aa8880fb8

## Web agent: quality mode

Under a minute for three tasks in one run: an iPhone 18 Pro into the Apple bag, a case into the
Amazon basket, and a Google Sheet of Meta's five-year financials. Agent mode: web.

https://github.com/user-attachments/assets/82bf6163-55c9-4cc4-95e7-9c6466f2a25d

## iOS agent: iPhone use

A paired iPhone, driven over the USB cable. Agent mode: mobile use, iOS.

https://github.com/user-attachments/assets/3c22eb45-d939-4742-9053-22ecc75b416f

---

## Pick a surface

One string decides what the agent drives. Everything else stays the same.

```python
from AutoCua.agent_launcher import run_agent

run_agent(mode="web use", provider="openrouter", model="gemini-3.8-flash",
          task="Open wikipedia.org and summarise today's featured article.")
```

| `mode` | What it drives | Where it runs |
|---|---|---|
| `"computer use"` | The desktop. Clicks, typing, hotkeys, apps, AppleScript | macOS, Windows or Linux, auto detected |
| `"shell use"` | A coding agent, straight into a real terminal | macOS, Windows or Linux |
| `"web use"` | Your own Chrome through the AutoCua extension; headless runs on a debugging port | Any host with Chrome, installed for you when it is missing |
| `"mobile use, ios"` | An iOS Simulator by default, or your paired iPhone or iPad | macOS host |
| `"mobile use, android"` | Not available yet | |

On Linux, computer use needs a GNOME desktop (tested on Wayland), and computer use and shell use
both need a few distro packages that pip cannot install. See [Linux, from pip](#linux-from-pip).

---

## Why it holds up on real software

**No pixel guessing.** The model never sees a coordinate. It picks a numbered element from a
view that fuses the operating system's accessibility tree with OCR, and a number it was never
shown is rejected before anything moves.

**No debugging port on your desktop.** Input goes in at the operating system's own input
layer. Mouse events posted at `kCGHIDEventTap` on macOS, pywinauto's real input path on
Windows, and a kernel driver when Windows UAC blocks everything else. An ordinary
application sees the same events it would get from your hand on the mouse.

**No debugging port on your browser either.** A normal `"web use"` run drives your own Chrome
through the AutoCua extension, which Chrome loads at launch with no store install. Chrome shows
its own "AutoCua started debugging this browser" bar while the agent works, and its Cancel
button hands the browser back to you. A debugging port is opened only for headless runs, on a
browser of the agent's own. The page scanner never calls `Runtime.enable`, and the
numbered boxes are painted into the screenshot, not into your DOM.

**No context wall.** A separate compression agent watches the transcript and splices a handoff
summary into it mid-run, so a long task finishes instead of dying at the context limit. In the
app you watch the memory ring's count fall.

**No telemetry.** AutoCua sends nothing anywhere except to the model provider you chose, and
to Telegram if you connect it yourself.

---

## Contents

- [Quick start](#quick-start)
- [How it sees a screen](#how-it-sees-a-screen)
- [How it acts on a screen](#how-it-acts-on-a-screen)
- [The agent team](#the-agent-team)
- [The browser agent](#the-browser-agent)
- [iPhone and iPad](#iphone-and-ipad)
- [Windows, UAC and the kernel driver](#windows-uac-and-the-kernel-driver)
- [Automation testing and workflows](#automation-testing-and-workflows)
- [Running tasks in parallel](#running-tasks-in-parallel)
- [The desktop app](#the-desktop-app)
- [Memory, vault, chats and skills](#memory-vault-chats-and-skills)
- [Remote control from Telegram](#remote-control-from-telegram)
- [Providers and models](#providers-and-models)
- [Requirements and setup](#requirements-and-setup)
- [Limits and safety](#limits-and-safety)
- [Project layout](#project-layout)
- [Roadmap](#roadmap)
- [Licence, credits and citation](#licence-credits-and-citation)

## Quick start

```bash
pip install AutoCua
```

On Linux, install a few distro packages first: see [Linux, from pip](#linux-from-pip).

Put the four lines from [Pick a surface](#pick-a-surface) in a `main.py`, then:

```bash
python main.py
```

That is the whole API.

### What the wheel brings, and what it cannot

Two of the native pieces arrive on their own. Two cannot: a Windows kernel driver, which `pip`
is structurally unable to install, and on Linux the desktop's accessibility and input bindings,
which come from your distro.

| | How it reaches you |
|---|---|
| **Browser agent** — Rust | Ships **prebuilt** in the Windows x64, Apple Silicon macOS and Linux x86_64 wheels. On any other platform pip builds it from the source distribution, which needs [Rust](https://rustup.rs) and a C compiler. |
| **WebDriverAgent** — iOS | **Cloned automatically** the first time you run a `"mobile use, ios"` task. Needs full Xcode and git. Nothing to do by hand. |
| **Interception** — Windows UAC | **Not installed by pip.** One manual step, once — below. |
| **AT-SPI, GTK and the desktop portal** — Linux | **Not installed by pip.** One `apt` line, once — [below](#linux-from-pip). |

### Answering UAC prompts on Windows

Windows' user-mode `SendInput` cannot deliver input to the UAC secure desktop, so an elevation
prompt can only be answered from kernel mode. That is the one thing the
[Interception](https://github.com/oblitum/Interception) driver does here.

`pip install` cannot set it up, and no packaging trick changes that: a wheel is unpacked and
never executed, pip does not run elevated, and the driver binds its input slots only when it
loads **at boot**. Without it **every mode still works** — you simply lose UAC handling, and a
task that hits an elevation prompt stops there.

To enable it, pick one:

```bat
setup_windows.bat
```

from a checkout — it asks first (answer N and nothing is downloaded or installed), then downloads
the pinned release, verifies its SHA-256, installs it, binds it to your **built-in keyboard only**
so external keyboards are never filtered, and asks before rebooting. Or install Interception
yourself from its own release page and reboot.

> It is never bundled because it is dual-licensed **LGPL-3.0 non-commercial / paid commercial**.
> Shipping it inside the wheel would make this project its distributor to every person who runs
> `pip install`, which that licence does not allow for free. You receive it from its author.
> See [THIRD_PARTY_NOTICES.md](https://github.com/AutoCua/AutoCua/blob/main/THIRD_PARTY_NOTICES.md).

### Linux, from pip

The Linux wheel is built for x86_64 and needs a GNOME desktop (tested on Ubuntu with Wayland;
X11 is not tested yet). The agent reads and drives the desktop through AT-SPI, GTK and the XDG
RemoteDesktop portal. Those bindings come from your distro, not from pip, so install them once:

```bash
sudo apt install python3-gi python3-cairo python3-gi-cairo python3-venv gir1.2-atspi-2.0 gir1.2-gtk-3.0 \
                 gir1.2-webkit2-4.1 at-spi2-core xdg-desktop-portal xdg-desktop-portal-gnome \
                 gir1.2-gstreamer-1.0 gstreamer1.0-pipewire gstreamer1.0-plugins-base \
                 libglib2.0-bin libgtk-3-bin xdg-utils
```

On KDE, swap `xdg-desktop-portal-gnome` for `xdg-desktop-portal-kde`. Fedora, Arch and openSUSE
ship the same packages under their own names; `setup_linux.sh` in the repository lists them.

Ubuntu and Debian refuse `pip install` into the system Python, so create a virtualenv first,
inside the folder that holds your `main.py` and `.env` so the agent finds that `.env`. Build it on
your **distro's own `python3`** (3.10 to 3.14): AutoCua loads the system PyGObject from inside
the venv only for the same 3.x version, so a venv on another version stops with "cannot import gi".

```bash
cd my-agent                         # the folder with your main.py and .env
python3 -m venv .venv
source .venv/bin/activate
pip install AutoCua
python main.py                      # the main.py from Quick start
```

The browser agent is prebuilt in the wheel, so you need no Rust or C compiler. On the first
computer-use run GNOME asks for screen-share consent: turn **Allow Remote Interaction** on
before you click **Share**, because GNOME leaves it off. Your choice is saved in `AutoCua_data/`.
The first scan also downloads the PP-OCRv6 models that read text the desktop does not publish.

### A few things worth knowing on the first run

- Add `ui=True` to open the desktop app instead of running the task in the terminal.
- Put your provider key in a `.env` file, or paste it into Settings in the app.
- Model names come from **[model_list.txt](https://github.com/AutoCua/AutoCua/blob/main/model_list.txt)** and are case sensitive. A name
  that is not on the list is forwarded verbatim and comes back as a 404.
- Everything the agent keeps, chats, keys, your own skills and browser profiles, lands in an
  `AutoCua_data/` folder created where you run Python from.

### From a checkout

```bash
git clone https://github.com/AutoCua/AutoCua.git
cd AutoCua

bash setup_mac.sh            # macOS
setup_windows.bat            # Windows, self-elevates, asks before installing the kernel driver

cp .env.example .env         # add your provider key
python main.py
```

`main.py` is the whole configuration surface: one `run_agent(...)` call. Edit it in place:

```python
run_agent(
    mode="computer use",
    provider="anthropic",
    model="claude-sonnet-5",           # names come from model_list.txt
    task="check the version of macOS.",
)
```

Every optional flag goes straight into that call (`ui=True`, `speed="fast"`, …). All of them,
`ui`, `save_conversation`, `speed`, `external_terminal`, `headless`, `browser_port`,
`browser_profile`, `extra_tasks`, `device`, `ios_version`, `sim_device`, `app_build`, `api_key`
and `os`, are documented in
**[agent_capabilities.md](https://github.com/AutoCua/AutoCua/blob/main/agent_capabilities.md)**.

---

## How it sees a screen

Every step the agent gets an element tree and a matching annotated image, then reasons over
both.

### macOS

1. **Find the real front window.** Window z-order comes from `CGWindowListCopyWindowInfo`, and
   the tree is walked per process from `AXUIElementCreateApplication`. The front app, any
   dialog owner, the Dock, Finder's desktop and menu-bar status-item owners are each walked
   separately, so a sheet on top of a window does not hide what owns it.
2. **Wake the app up.** `AXEnhancedUserInterface` is set when it is not already on, so apps
   publish their full tree, then the scanner waits 0.3 s for it to settle.
3. **OCR whatever the tree missed.** Apple Vision runs at the accurate level with a confidence
   floor of 0.4, on worker threads, while the accessibility walk is still running. Recognising
   at 1x logical resolution instead of 2x took a 2940 px capture from 1.17 s to 0.78 s with no
   loss in hit rate.
4. **Drop what cannot be clicked.** Every element is re-tested against the real window stack
   with a 20x20 grid of sample points. Anything under 1 percent visible is thrown away, so the
   model is never handed a number that sits behind another window.
5. **Draw the numbers.** The capture is downscaled first to fit inside full HD (1920 x 1080,
   aspect kept, never upscaled), then 13 px magenta labels with a 2 px black rim are stamped on
   from cached tiles, and the frame is encoded as a **4:4:4 JPEG at quality 85**. That chroma
   choice is deliberate. Default subsampling smears thin magenta digits into mush, and this is a
   tenth of PNG's bytes.

### Windows

The same contract, built out of what Windows offers.

One UI Automation cached round trip pulls 18 properties for the whole subtree, then four
threads run in parallel: WinRT OCR, a taskbar walk, a Win32 backend scan and a raw UIA scan of
the Start menu. The results are merged and deduped at 5 px, with OCR lines nested into the
deepest container that does not already hold a labelled element there. Chromium browsers are
launched with `--force-renderer-accessibility` so they publish their full tree, and page loads
are gated by polling the toolbar reload button until it stops saying "Stop".

Clicks aim at the pixel centroid of the element's own content rather than the middle of its
box, so a wide button with a label on the left still gets hit where the label is.

### Both platforms

A step that only digests a tool result, the turn after a `web` lookup or after the coder agent
reports back, deliberately skips the scan entirely. No screenshot, no tree, just the payload
and an instruction to pull the findings into the scratchpad. Scanning a screen that nobody
looked at costs tokens and buys nothing.

---

## How it acts on a screen

Nothing in the desktop input path opens a debugging port, sets an automation flag or injects
JavaScript. Input goes in at the operating system's own input layer, so an ordinary
application sees the same events it would get from your hand on the mouse.

### macOS

Mouse events are posted at `kCGHIDEventTap`, the same tap a physical mouse feeds, from a
private event source, after a genuine cursor warp with `CGWarpMouseCursorPosition`.

- A partially visible element tries the accessibility `AXPress` action first, then falls back
  to a synthetic click. A fully hidden element is refused with "scroll first" instead of being
  clicked blind.
- Multi-line text is put on the clipboard and pasted, then the previous clipboard is restored,
  because code editors auto-indent typed newlines and corrupt the text.
- Drags are 20 interpolated move events at 15 ms, not a teleport.
- `applescript` is a first-class tool, not a bash escape hatch. The model supplies a complete
  `tell application` block and the runtime handles launching, foregrounding and a 30 second cap.

Typing and hotkeys go through pyautogui and pynput rather than the private event source, so
they are ordinary synthetic input.

### Windows

Element clicks use `pywinauto.click_input()`, which is a real `SendInput`. When that fails
because User Interface Privilege Isolation blocked it, the click is retried through the
**Interception kernel-mode driver**. Typing switches to the driver when the front application
is Windows Security. See [Windows, UAC and the kernel driver](#windows-uac-and-the-kernel-driver).

### A watcher for permission dialogs

macOS asks for consent the first time an app does something new, and a modal dialog will
deadlock a run. AutoCua fingerprints those dialogs **structurally**, any window or sheet
carrying a "Don't Allow" or "Deny" button, and clicks the affirmative. It runs on a one second
loop around every AppleScript call and every shell command, and if a dialog is present but
cannot be clicked, the command fails after 8 seconds with a permission-specific error instead
of hanging forever.

### Browser windows on the desktop

When a browser is in front, the agent gets browser rules injected into its prompt and the
runtime changes behaviour:

- It **reuses the browser you already have open**, with your profile, cookies and sessions. The
  rules are emphatic that launching a second instance lands in a signed-out profile.
- macOS waits for a real `AXWebArea` before scanning, with a 1.5 second grace period and a 15
  second load budget. No web area means it is not a web window, so scan it as it is. Firefox
  publishes no load property, so it falls through to a children-ready check.
- Known destinations are opened in one command with the query already in the URL, rather than
  opening a site to click its search box.

### Tools the desktop agent has

| | macOS | Windows |
|---|---|---|
| Click | `left_click` (1, 2 or 3 clicks), `right_click` | same |
| Type | `input`, `typewrite` | same |
| Navigate | `scroll`, `hotkey`, `screenshot`, `drag_drop` | `scroll`, `hotkey`, `screenshot` |
| System | `open_app`, `shell` (zsh), `applescript` | `open_app`, `shell` (PowerShell) |
| Delegate | `sub_agent` (coder_agent, browser_agent), `agent_wait`, `web` | same |
| State | `todo_list`, `update_todo`, `scratchpad`, `wait`, `done` | same |

19 tools on macOS, 17 on Windows. The names are a frozen allow-list. Anything else the model
invents comes back as an error result and is never executed.

---

## The agent team

Behind one task sits a hierarchy of agents, each in its own process.

**The parent agent** owns the screen and decides what a task needs. When it needs real code
work it delegates and can carry on, or block until the result comes back.

**The coder agent** has 14 tools: `shell`, `view`, `grep`, `glob`, `write`, `replace`, `web`,
`plan`, `todo_list`, `update_todo`, `wait`, `scratchpad`, `minion` and `exit`. Its operating
procedure is EXPLORE, PLAN, EXECUTE, VERIFY, and it is written into the prompt with teeth:

- Exploration is delegated by rule. "Never read the codebase first-hand to build first-time
  understanding, send a minion."
- The plan is a structured Markdown document with real `path:line` anchors, not a restatement
  of the request.
- Every change with logic in it needs a throwaway test under `./.AutoCua_verify/` that is
  actually run, whose output is recorded, and which is deleted before exit.
- `replace` verifies the old block before writing and re-verifies after, so a stale line number
  fails with "mismatch at line X" instead of corrupting your file.

**Minions** are read-only scouts with six tools and no recursion. The restriction on tool
*names* is structural: the minion's six-name set is the allow-list at the call router, so a
hallucinated `write` comes back as an error result and never runs. Several minions fire from
one coder step, run in parallel, and the loop blocks until all of them report back with
findings anchored to exact `path:line`. Even a crashed minion returns its partial scratchpad,
so the parent never hangs. To be precise about the boundary: the tool *names* are enforced, and
the "read-only commands only" rule on its `shell` tool is held by the prompt.

Minions exist to keep the coder's context small. Every tool result is budgeted too: 200 lines
or 15,000 characters, with `shell` keeping a 50-line tail, `view` capped at 2,000 lines and
`grep` at 8 MB, so one wide search or noisy build log can never blow up the conversation.

Run either directly. They start in their own workspace, so point them at an absolute path when
the work is in an existing project:

```bash
python -m AutoCua.mac.agent.coder   --task "refactor the auth module in /Users/me/projects/api" --provider anthropic --model claude-sonnet-5
python -m AutoCua.mac.agent.minions --task "where is _validate_token defined in /Users/me/projects/api and who calls it?" --provider anthropic --model claude-sonnet-5
```

---

## The browser agent

`"web use"` is a different program from the rest of the framework. The whole browser agent,
the agent loop, the page scanner and the controller, is **one Rust crate** compiled into a
single Python extension module and imported like any other. Its LLM calls go through the
same Python providers every other mode uses, in `AutoCua/llm_provider/`.

A checkout builds it on first import with `cargo build --release` and rebuilds only when a
`.rs` file actually changed. The pip wheel ships it already compiled. One binary covers every
CPython from 3.10 up.

**Two ways into Chrome.** Every run with a window goes through the **AutoCua extension**, a
parallel run included: Chrome is started with the flags that load it, or taken as it is when it
is already running with it, and the agents drive your own browser, logged in as you are, with
no debugging port open; each agent is on a line of its own to the extension. A headless run
uses the **DevTools Protocol** on a Chrome of the agent's own. Every tool works the same way in
both. When Chrome is missing it is downloaded from Google and installed on the first web run.

### Raw CDP, hand written

No Playwright, no Selenium, no Puppeteer, no chromedriver. None of them appear anywhere in the
repository. In the debugging-port mode there is **one WebSocket** to Chrome plus hand-written
HTTP for tab management, one session per tab and a child session per cross-origin iframe,
attached in flatten mode up to six levels deep. In extension mode the same scanner runs as a
content script of the extension, in an isolated world the page cannot see, and the tools send
their input through `chrome.debugger` as the same trusted CDP events.

The property that matters:

> **The debugging-port scanner runs no JavaScript in the page, and `Runtime.enable` is never
> called in either mode.**

In the debugging-port mode the page model is two CDP calls per frame, `DOMSnapshot.captureSnapshot`
plus `Accessibility.getFullAXTree`; in extension mode it is one injection into every frame of
the tab, cross-origin frames included, reading the same facts from the DOM. Clicks and keystrokes go through CDP's trusted input path with a
rising click count. Even the numbered boxes the model sees are painted **into the JPEG** in
Rust with a hand-rolled 3x5 bitmap font, never into the DOM.

Two honest exceptions, both deliberate:

- `run_script` and `scrape` are the two doors for JavaScript in the page, and they are split by
  what they are for: `run_script` runs what the model wrote, to DO something the ordinary tools
  cannot reach; `scrape` runs a reader we wrote, which takes no selectors from the model and only
  reads. Both are raced against a 10 second in-page timer and serialised in page, `run_script` is
  parse-probed with `new Function` and capped at 30,000 characters, and both are screened by the
  same `guardrails_check.rs`: web pages only, theft and cross-site sends refused before anything
  runs, credential-shaped values taken out of the result. Both descriptions tell the model to use
  them sparingly, because automation blockers can notice them.
- A `scrape` reads the screen the page is on plus the next two below it: three viewports at
  most, whole records only, in one chunk of about 8k tokens. The reader finds the page's records
  itself, by shape and never by site: the block in the page's main content (a sidebar or a menu
  only when nothing else repeats), a custom element that repeats and carries its data in
  attributes, or the repeated block on plain HTML, with nesting or indentation read as thread
  depth; it falls back to the text in reading order on a page without records, so an article or
  a table reads too. A thread comes back as the post, then its comments with each reply nested
  under the comment it answers; a list as its items with title, url, score, counts, price,
  phone or address where the page shows them. Every chunk is stored as
  JSON under `AutoCua_data/scrapes/<run>/<n>.json` with its index number and the time it was
  read. After the call the agent is in **scraping mode** until it calls `exit_scrape_mode`: no
  scan, no picture, and a schema of exactly five tools, `index` (its own record of the chunk: a
  one-line title, the findings as a JSON array, what is still pending), `more` (the next three
  screens, scrolled to from where the last read ended; nothing is read twice), `exit_scrape_mode`,
  `scratchpad` and `update_todo`. On exit the mode's steps leave the history and the scrape
  result becomes a pointer; what stays is one line per index in `<indexes>`, and `index_read`
  brings an index back in full. The rules the model follows are in
  `AutoCua/default_skills/web/scraping.md`.
- The "agent is driving" overlay, an edge glow and the agent's own cursor, is injected into the
  page (its main world in the debugging-port mode, the extension's own world in extension
  mode), which makes it detectable by a page that looks for it. That is a product trade-off in
  favour of you being able to see what the agent is doing.

### The page model

- **Settling counts in-flight requests** rather than sleeping. 150 ms of quiet, capped at 3
  seconds.
- **Occlusion** by paint order demotes elements behind cookie banners and modals.
- **Noise filtering** collapses the wrappers and SVGs inside a button that would otherwise
  stack four boxes on the same pixels.
- `[1]` is always the page itself. Numbering follows document order and stops at 300 elements.
- **Per-host overrides** are shipped, for example YouTube turns off pointer-cursor detection
  and caps at 200 elements.
- Modal dialogs are shown to the model as a step of their own, with their text, for it to
  answer. A tab whose page froze for 5 seconds or crashed is closed and its page opened again in
  its place. The Chrome Web Store is off limits: the tools refuse to open it or act on it.
  Navigation failures, end of history and stale ids all come back as recoverable tool errors
  rather than silent successes.

### Staying logged in

A run with a window is in **your own Chrome**, so the agent is logged in wherever you are.
Headless runs use a **persistent profile keyed by name**, so cookies, localStorage and logins
survive between runs there too. As the source puts it, arriving already logged in
matters far more than any fingerprint tuning, because a login wall is where a web agent usually
stops.

**18 tools:** `new_tab`, `switch_tab`, `close_tab`, `update_tab`, `navigate_tab`, `click`,
`hold_click`, `input`, `keyboard`, `run_script`, `scrape`, `scroll`, `dialog`, `wait`,
`scratchpad`, `todo_list`, `update_todo`, `done`, plus `index_read` once a run has stored an
index. Scraping mode sends exactly five: `index`, `more`, `exit_scrape_mode`, `scratchpad`,
`update_todo`. A parallel agent pinned to one tab gets 15, with the three tab-lifecycle tools
removed from its registry and refused at the router as a backstop.

---

## iPhone and iPad

The same loop that drives your Mac drives a phone. Read the accessibility tree, annotate a
screenshot, tap, swipe and type through
**[WebDriverAgent](https://github.com/appium/WebDriverAgent)**. The model is never told which
target it is on, because both paths end at the same endpoints.

| | `device="simulation"` (default) | `device="hardware"` |
|---|---|---|
| Runs on | An iOS Simulator on your Mac | Your paired iPhone or iPad |
| Driven by | `xcrun simctl` plus one **unsigned** `xcodebuild test` | pymobiledevice3, USB forward plus XCUITest launch |
| Needs | Full Xcode. **No Apple ID, no signing, no pairing** | One-time pairing and Team ID signing |
| iOS version | `ios_version="26.5"`, or the newest your Xcode can build for | Whatever the device runs |
| Your own app build | `app_build="MyApp-Simulator.ipa"`, installed before the task starts | Not supported, the run stops with an error |
| Best for | Everyday runs, testing, pinning a specific iOS version | Real apps with your logins, camera, cellular |

```python
run_agent(
    mode="mobile use, ios",
    provider="anthropic", model="claude-sonnet-5",
    device="simulation",        # or "hardware"
    ios_version=None,           # e.g. "26.5"; None picks the newest usable runtime
    sim_device="iphone",        # "iphone", "ipad", or an exact simulator name
    app_build=None,             # simulation only: a .ipa/.app in AutoCua_data/app_build/
    task="Open Settings, turn on Dark Mode, and confirm it on the home screen.",
)
```

AutoCua boots the simulator, starts WebDriverAgent, runs the task, then shuts everything down
when the agent finishes, whether it succeeded, errored or you pressed Ctrl+C. The first run
compiles WebDriverAgent once. After that it starts in well under a minute.

**Testing your own app.** `app_build` works with `device="simulation"` only. Put a simulator
build (`.ipa` or `.app`) in `AutoCua_data/app_build/` and pass its file name. It is installed
the moment the simulator has booted, at the same time as WebDriverAgent starts, and the agent
only begins once both are done. No app, no run: a missing file, a failed install or
`device="hardware"` stops the run with a clear error before the agent starts. Details in
**[agent_capabilities.md](https://github.com/AutoCua/AutoCua/blob/main/agent_capabilities.md)**.

**Three details worth knowing.**

What `simctl` can boot is not the same as what `xcodebuild` can build for. Simulator runtimes
are system wide and outlive Xcode upgrades, so AutoCua probes `xcodebuild -showdestinations`
up front and filters to what is actually usable, instead of dying minutes into a run. If a
parallel run needs one more device than you have, it creates one.

The element scan asks WebDriverAgent to skip its `visible` and `accessible` attributes, which
it computes by hit-testing every element. On a 364-element home screen that takes the request
from **3.60 s to 0.39 s**, about 82 percent of the time. Visibility is recomputed from the
element's centre, which is the exact point the tap will land.

There is a **`video_player`** tool because DRM players black out screenshots. When vision goes
dark it drives play, pause, close and streaming checks from the accessibility tree, and proves
"streaming" by watching the progress label advance across a four second window.

**13 tools:** `open_app`, `click`, `input`, `scroll`, `wait`, `shell`, `web`, `vault`,
`video_player`, `todo_list`, `update_todo`, `scratchpad`, `done`.

---

## Windows, UAC and the kernel driver

Windows UAC runs on a **separate secure desktop**. User-mode `SendInput` cannot reach it and
screen capture fails there too. Most automation tools simply hang at an elevation prompt.

AutoCua turns that failure into the detector, then answers from kernel space.

1. **Detect.** The full-screen grab fails, so the scanner reports that UAC is up.
2. **Ask the model.** That step ships with **no screenshot and no element tree**. Just a
   one-shot prompt: *"A Windows UAC prompt is blocking the screen. Based on your previous
   actions, do you want to allow this?"* The agent answers `alt+y` or `alt+n`. **Elevation is
   never automatic.** It is a decision the model has to make in context.
3. **Inject.** Those two combinations are intercepted and sent as raw scancodes through the
   **Interception** kernel-mode input filter driver, which the secure desktop does accept.

The driver is also the fallback whenever User Interface Privilege Isolation blocks ordinary
input, for example when driving Windows Security.

**How it is installed.** `setup_windows.bat` asks first — answer N and nothing is downloaded or
installed. On Y it downloads the author's own signed v1.0.1 release, verifies it against a
**pinned SHA-256** and aborts rather than run an unverified kernel installer. It then binds the
driver to the **built-in** keyboard and mouse only, at the device
level. That detail matters. The driver has ten keyboard slots that are never freed, so the
older class-wide binding burned one on every wireless-keyboard reconnect and eventually left
keyboards dead until reboot. Device-level binding takes exactly one slot at boot and never
filters another keyboard. If the bind fails, setup strips every filter rather than reboot into
a configuration that could kill your keyboard.

Without the driver everything still works except three things: answering a UAC prompt, a click
that User Interface Privilege Isolation blocked, and typing into Windows Security.

> **Licensing.** Interception is dual licensed. LGPL v3.0 for non-commercial use, and
> **commercial use requires a separate paid licence from its author**. AutoCua's MIT licence
> does not and cannot grant it. See [THIRD_PARTY_NOTICES.md](https://github.com/AutoCua/AutoCua/blob/main/THIRD_PARTY_NOTICES.md).

**macOS has no UAC** and AutoCua does not elevate on it. The analogue is TCC, handled entirely
in user space by the permission wizard and the consent-dialog watcher. There is no `sudo`
anywhere in the agent paths.

---

## Automation testing and workflows

A task here is a sentence, not a selector. Nothing in it names an XPath, a CSS class or a
coordinate, so the redesign that breaks a selector-based suite does not break the task. The
agent finds the button again because it reads the screen the way a person does.

**What that buys you.**

| Need | How AutoCua covers it |
|---|---|
| End-to-end across surfaces | One harness covers a desktop app, a website and an iPhone. Change one string to move between them |
| A device or browser matrix | `extra_tasks=[...]` runs N tasks at once. One Chrome with a tab each, or one simulator each |
| Unattended runs | `headless=True` for Chrome. Desktop and iOS runs still need a logged-in graphical session |
| An audit trail | `save_conversation=True` writes exactly what the model saw and answered at every step |
| A result you can branch on | `run_agent` returns a status dict, and every parallel child writes its own `result.json` |
| Credentials in a test | On iOS the vault types the secret. The value never enters the model's context |
| Site or app specific rules | Skills are Markdown files loaded only when that site or app is in front |
| Long flows | Memory compression keeps a long checkout or onboarding flow inside the context window |

**A smoke test you can run today.**

```python
from AutoCua.agent_launcher import run_agent

result = run_agent(
    mode="web use", provider="anthropic", model="claude-sonnet-5", headless=True,
    save_conversation=True,
    task="""
    Go to staging.example.com, sign in as demo@example.com, add any item to the basket,
    go to checkout and stop at the payment step. Report the basket total and whether the
    payment form rendered. Do not submit a payment.
    """,
)

assert result["status"] == "success", result["message"]
```

`run_agent` returns `{"status", "message"}`, where status is `success` when the agent called
`done`, `error` when the loop died, and `incomplete` when it was stopped or ran out of steps.
That is the hook you branch on.

**What you get back to read afterwards.** With `save_conversation=True`, `conversation/`
holds the exact payload sent to the model at every step and `raw_reasoning/` holds its raw
reply, so a failed run is readable rather than guessed at. A parallel run additionally writes
`result.json` per child and exits 0 when it returned, 1 on a crash and 130 on Ctrl+C.

> Those folders are cleared at the start of every run, whether the flag is on or not. Copy out
> anything you need to keep before running again.

One design decision matters more than any of this for reliability: the agent is told that a
tool result reports that a tool ran, never that the screen changed. Every step judges the
previous step's expected outcome against the new screenshot before it does anything else.

**A cross-surface matrix in one call.**

```python
run_agent(
    mode="web use", provider="openrouter", model="gemini-3.8-flash", headless=True,
    task="Sign up for a new account on staging.example.com and confirm the welcome screen.",
    extra_tasks=[
        "Reset the password for demo@example.com and confirm the email prompt appears.",
        "Open the pricing page and check every plan card shows a price and a CTA button.",
    ],
)
```

Each task gets its own tab, its own agent and its own working directory under
`./parallel/task_N/`, with a `result.json` holding `status` and `message`. Live output is
prefixed per task, and one Ctrl+C stops all of them.

**Everyday workflow automation** is the same machinery pointed at your own machine:

```python
task = "Open Numbers, put last month's totals from ~/Downloads/report.csv into a new sheet, chart them, and save it to the Desktop as monthly.numbers"
task = "Go through my Gmail inbox, find every invoice from this month, and save the PDFs into ~/Documents/Invoices"
task = "Check the three competitor pricing pages in my bookmarks and tell me what changed since the notes in ~/Desktop/pricing.md"
```

### Be honest about what this is

AutoCua is a language model driving real software. Two runs of the same task can take
different routes. It is a strong fit for exploratory testing, smoke flows, repetitive
back-office work and anything where writing selectors costs more than the test is worth. It is
not a replacement for a deterministic unit or integration suite, and it has no assertion
framework of its own. Use it where a human tester would otherwise be clicking.

---

## Running tasks in parallel

Pass `extra_tasks` and every task, including the first, runs at the same time in its own child
process.

```python
run_agent(
    mode="web use", provider="anthropic", model="claude-sonnet-5", headless=True,
    task="find the cheapest flight to Tokyo next month",
    extra_tasks=[
        "summarise today's top Hacker News thread",
        "check my GitHub notifications",
    ],
)
```

| | `"web use"` | `"mobile use, ios"` with `device="simulation"` |
|---|---|---|
| Each task gets | Its own agent pinned to **its own tab** | Its own **simulator** and its own WebDriverAgent port |
| Shared | Your own Chrome, through the extension (headless: one browser of the agent's own on a debugging port); its front tab shows a cover page while the agents work in tabs behind it | Nothing. A phone screen cannot be split |
| Isolation | A 13-tool single-tab registry. It cannot touch another agent's tab | Own port (8100, 8101, ...), own scratchpad, own build directory |
| Ceiling | Tabs and RAM | Simulators, and it creates one when it needs another |

Backgrounded tabs are told they are still focused, so pages that gate on focus keep behaving
while another agent is in front. Output is prefixed per task, one Ctrl+C stops everything, and
results land in `./parallel/task_N/`. On iOS every simulator the run booted is shut down at the
end, whether it succeeded, failed or was interrupted. With `app_build`, every simulator installs
your app at the same time, each as soon as it has booted.

> `./parallel/` is deleted and recreated on every parallel run. Copy out anything you want to
> keep.

Parallel mode is for `run_agent`. The desktop app runs one agent at a time.

---

## The desktop app

`ui=True` opens a native window, 1140 by 700, on a Flask server at AutoCua's own port
(27321, or a free one for that run when another program holds it). After a one-time
`npm install` in `AutoCua/desktop` (Node.js 22.12 or newer) the window is Chromium, through
Electron, the same on every OS. Without it the app opens in
[pywebview](https://pywebview.flowrl.com/), with the OS title bar tinted to match the page so
the whole surface reads as one.

```
+--------------+----------------------------------------------+
|  AutoCua     |  live agent screenshot  |  tracking progress |
|              |  (what the agent sees)  |  (scratchpad notes)|
|  + New chat  +-------------------------+--------------------+
|              |  tool-response chain    |  live TODO list    |
|  chat 1      |  "N tools used"         |  (agent's plan)    |
|  chat 2      +-------------------------+--------------------+
|  chat 3      |   Agent Notes  or  Skills   (big centre)     |
|              +----------------------------------------------+
|  settings    |   composer  [fast] [mode] [model] [skills]   |
+--------------+----------------------------------------------+
```

| Element | What it does |
|---|---|
| **Live screenshot** | The screen the agent just captured, updated every step |
| **Tracking progress** | Each scratchpad entry streams in as a bullet on a connecting line |
| **Tool-response chain** | Every tool call drawn as an animated canvas icon, so you can see *why* it did something |
| **Live TODO** | The agent's own plan, tailed from its file as it edits it, frozen with crosses if you stop it |
| **Agent Notes** | The final write-up, shown when a run ends, completed or stopped |
| **Skills** | Browse, preview, add, edit and delete domain knowledge files, live, for desktop and iOS |
| **Memory ring** | Context gauge beside the Skills icon, with the live token count inside. It breathes while compression runs and its count *falls* when the handoff lands |
| **Fast / Quality** | Leaner prompt and fewer tokens per step, or full reasoning, which is the default |
| **Mode picker** | Computer use, Mobile use, Shell use |
| **Stop** | Retires the run id instantly, so a still-running model call can never repaint your next chat |

Chats are saved and resumable. Reopening one restores its history and puts the memory ring back
where it was, and you can download the exact payload the model received. On macOS the first
launch opens a **setup wizard** that walks the four permissions AutoCua needs, Accessibility,
Full Disk Access, Screen Recording and Automation, one at a time, auto-advancing as each is
granted. It reads the TCC database directly to notice a grant the in-process check cannot see,
and repairs stale grants left by a previous build.

### Two owners, one terminal prompt

In Shell use the terminal card has a single `>` prompt that either the agent or **you** can own.

| | AI mode | Manual mode |
|---|---|---|
| Header | `AutoCua Code` | `AutoCua Terminal` |
| Prompt | Bare `>`, read only | Shows the real working directory |
| Who runs it | The coder agent, spawning minions and writing files | **You.** No agent, no model |
| Interrupt | The stop control | **Ctrl+C**, wired to a real signal on the process group |

Click the card and you take the keyboard. Type `ls`, `git status`, `npm test` and it runs live,
streaming as it goes, with `cd` tracked between commands.

**The bridge is the interesting part.** Every command you run by hand is captured, command,
directory, exit code and output, and replayed into the agent's **next** run as a
`<manual_mode>` block that tells it those effects are already applied. So you can drop in,
check something yourself, fix a file, install a package, and the agent picks up already
knowing. It is capped at 12 commands and 60 output lines each so it never bloats the
conversation, and undelivered commands are re-queued if a run dies before it sees them.

---

## Memory, vault, chats and skills

**Memory compression is its own agent.** When the live context crosses 110,000 tokens, a
background thread asks a second model to write a handoff document, and the controller splices
it into the transcript **in place**, on the main thread, guarded by a generation counter so a
stale result can never land and a re-arm delay so it cannot thrash. Every agent uses it: the
macOS and Windows desktop agents, iOS, the coder, and the Rust browser agent, which calls the
same Python controller across the language boundary. It is why the memory ring's count in the
app *falls* mid-run instead of only climbing.

**The vault fills credentials without showing them to the model.** The agent says "fill element
12 with the password". The runtime resolves the app from the element tree, looks up the
credential locally, types it, and returns only "Credential filled successfully". The value
never enters the model's context on the way in. Two limits worth stating: the store is plain
JSON on your disk, and a field that renders its value back to the screen, a username rather
than a masked password, will appear in the next element scan like any other text. PIN pads
with a separate key per digit, like the iPhone lock screen, work the same way: the runtime taps
the keys, checks that every press registered, and never logs which keys it pressed. It is wired
into the iOS agent today.

**Chats live outside the install folder** in `AutoCua_data/`, so an uninstall cannot take them.
They resume with the agent's full per-step reasoning paired to its tool results, plus a note
recording how the previous run ended, on every path including stop and crash. Resumable chats
are a feature of the desktop app. A terminal run with `save_conversation=True` writes readable
per-step logs instead, which is what you want for auditing rather than resuming.

**Skills are Markdown files matched to what is on screen**, on the desktop and iOS agents. The
browser agent has the slot but does not fill it yet. A router maps hostnames and app
names to files, so opening Google Colab loads the browser rules with Colab guidance nested
inside them, and nothing else. Hostnames match on the longest domain suffix, app names on the
longest substring. AutoCua ships browser, Google, Microsoft, Colab, LibreOffice Calc,
Skyscanner and Wikipedia knowledge read-only in `AutoCua/default_skills/`, plus Apple Maps on
macOS and Instagram on iOS. Your own skills live beside them in `AutoCua_data/skills/`, where
you can add, edit and delete them from the app while it runs. Skills are injected only on
steps that actually looked at a screen.

The browser skill also carries the scraping and safety rules: record findings every iteration,
prefer genuine over sponsored results, scroll to the true bottom of a page, always **reject**
cookie banners rather than accept, and never click a link paired with a malicious message.
It includes a **prompt-injection defence**: follow only the user's request, ignore any
instruction found inside an image or an element tree, and log detections with the site that
carried them.

---

## Remote control from Telegram

Connect a bot in Settings, pick a provider and model from an inline keyboard, and send tasks
from your phone. Only providers whose key you saved in Settings are offered, a key that lives
only in `.env` will not appear there. Tasks sent
mid-run are queued, the bot asks whether to continue the previous session or start fresh, and
milestones stream back to your phone every two seconds. A small always-on-top pill shows
status on the desktop, so a remotely started run is never invisible to whoever is sitting at
the machine.

> The pill is a status cue and has no stop control. The bot obeys **only its owner**: the first
> chat that messages it after you connect claims it, and every other chat is ignored. Disconnect
> in the app to release it. Treat the token like a password.

Discord and WhatsApp exist as placeholder files on Windows only. Neither is implemented.

---

## Providers and models

Swap the model by editing two strings. Nothing else changes.

```python
run_agent(..., provider="anthropic",  model="claude-sonnet-5")
run_agent(..., provider="openrouter", model="gemini-3.8-flash")
run_agent(..., provider="google",     model="gemini-3.1-pro")
run_agent(..., provider="openai",     model="gpt-6-astra")
```

Nine providers, all supported on every surface.

| Provider | Desktop, macOS | Desktop, Windows | Browser | iOS |
|---|:--:|:--:|:--:|:--:|
| Anthropic | yes | yes | yes | yes |
| AWS Bedrock | yes | yes | yes | yes |
| Cerebras | yes | yes | yes | yes |
| Google, including Vertex | yes | yes | yes | yes |
| Groq | yes | yes | yes | yes |
| OpenAI | yes | yes | yes | yes |
| OpenRouter | yes | yes | yes | yes |
| Perplexity | yes | yes | yes | yes |
| Together AI | yes | yes | yes | yes |

48 model names are listed in **[model_list.txt](https://github.com/AutoCua/AutoCua/blob/main/model_list.txt)**. Copy them exactly. A name
that is not on the list gets no validation. It is forwarded verbatim and comes back as a 404.

Keys resolve from the runtime setting first, then the environment or `.env`, so you can
override per machine without editing files. Every call gets three attempts, and the coder and
minion agents additionally fall back to a second model.

The `web` tool uses each provider's own search: Claude web search, the OpenAI Responses web
search tool, Gemini grounding, Groq's compound model, OpenRouter's Exa plugin running on your
chosen model, and Perplexity Sonar.

> Together AI, Cerebras and AWS Bedrock have no native web search. Under any of them the `web` tool hands the query to the
> browser agent running headless on its own dedicated port and profile, capped at 15 minutes,
> and returns its report. Expect that step to take minutes rather than seconds.

---

## Requirements and setup

- **macOS** on Apple Silicon or Intel, **Windows 10 and 11**, or **Linux** (GNOME; tested on Ubuntu with Wayland, X11 not tested yet)
- **Python 3.10 or newer**
- An **API key** from any supported provider
- *For the browser agent from source:* **Rust** and a C toolchain — the setup scripts install these for you
- *For the browser agent:* **Google Chrome**, installed for you on the first web run when it is missing
- *For iOS:* macOS with **full Xcode**, not just the Command Line Tools

### macOS

```bash
bash setup_mac.sh
cp .env.example .env
python main.py
```

The script installs [uv](https://astral.sh/uv), creates a virtualenv and installs the platform
requirements. It then installs a Rust toolchain if you don't have one and precompiles the
browser agent, so `"web use"` works on the first run instead of stopping with "cargo not
found". It needs no `sudo` and does not reboot.

The Rust half needs Apple's Command Line Tools to link — if they're missing the script opens
Apple's installer, tells you to re-run, and finishes anyway: every other mode works without
them. To install the toolchain yourself instead:

```bash
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Grant Full Disk Access** so the coder and minion agents can read and write Desktop,
Documents and Downloads without permission popups. System Settings, Privacy and Security, Full
Disk Access, then add your **Terminal, VS Code or python** binary. The app opens this pane for
you on first launch. A terminal run does not, so grant it yourself.

### Windows

```bat
setup_windows.bat
copy .env.example .env
python main.py
```

Setup self-elevates, installs uv and a Rust toolchain with mingw-w64 so you do not need Visual
Studio, precompiles the browser agent, then asks Y/N before the Interception driver. On Y it
downloads it, verifies its checksum, binds it to your built-in keyboard and mouse, and asks
before rebooting. On N it skips the driver and changes nothing else.
Python 3.11 to 3.13 is what it asks uv for.

### Linux

```bash
bash setup_linux.sh
source .venv/bin/activate
cp .env.example .env
python main.py
```

The script installs the distro packages with `sudo` (the AT-SPI and GTK introspection bindings
the scanner and controller read the desktop through, a C compiler and curl), then
[uv](https://astral.sh/uv), a virtualenv on your distro's Python and the Linux requirements. It
then installs a Rust toolchain if you don't have one and precompiles the browser agent, so
`"web use"` works on the first run instead of stopping with "cargo not found". Only the package
step needs `sudo`; it does not reboot. Debian and Ubuntu use `apt`; Fedora, Arch and openSUSE
get the same packages through their own package managers. To install the toolchain yourself
instead:

```bash
sudo apt install build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The first computer-use run asks for screen-share consent through GNOME's RemoteDesktop portal
dialog, because that portal is the only way to move the pointer and type on Wayland. Turn
**Allow Remote Interaction** on, then click **Share** once; the session token is saved and you
are not asked again.

### iOS, optional

```bash
bash setup_ios.sh
```

Clones **WebDriverAgent** at pinned tag `v15.1.1` into `AutoCua/ios_connector/`. It is not
bundled here, you get it from the Appium project directly. The script checks your Xcode
toolchain and installs the device dependencies.

**Simulator**, the default, needs no Apple ID, no signing and no pairing. Xcode does need to be
able to *target* a simulator, which on a fresh Mac is a separate one-time download the script
offers to run:

```bash
xcodebuild -downloadPlatform iOS      # about 8.5 GB, once
sudo xcodebuild -runFirstLaunch       # run this yourself if Xcode still targets nothing
```

> A booted simulator in `xcrun simctl list` is **not** proof this works. Runtimes are system
> wide and `simctl` can boot devices your selected Xcode cannot build for. The real test is
> `xcodebuild -showdestinations` listing `platform:iOS Simulator` entries.

**A physical iPhone or iPad** additionally needs an Apple ID in Xcode and a one-time signing
and pairing pass, from Settings, Connect Device in the app, or standalone with
`python AutoCua/ios_connector/setup.py`. The app rewrites the Xcode targets to automatic
signing for you, opens Xcode on the Accounts pane, and streams the build live.

> A free Apple ID works, but its provisioning profiles expire after 7 days, so you re-sign
> weekly. A paid developer account lasts a year.

### Where your data lives

Everything the agent keeps is in one `AutoCua_data/` folder that no installer owns: chats, your
API keys, the persistent Chrome profiles that keep the browser agent logged in, the credential
vault and your own skills. It sits next to your checkout, or in the directory you ran Python
from for a pip install. `AutoCua_DATA_DIR` overrides both.

---

## Limits and safety

**What protects you**

- A Windows UAC prompt is handed to the model as an explicit allow-or-decline step, so
  **elevation never happens on its own**.
- The minion tool set has **no write tool in it**. Six names are the allow-list at the call
  router, so a hallucinated `write` comes back as an error result rather than an edit.
- An id the model was never shown **is refused before anything moves**, and so is a fully
  hidden element, which returns "scroll to make it visible first" instead of a blind click.
- Vault credentials are typed by the runtime, so **the value is never shown to the model**.
  Wired into the iOS agent today.
- The scraping ruleset carries a **prompt-injection defence**: follow only your request, and
  log anything that tries to give instructions from inside a page or an image.
- **Stop is checked** before every action, between characters while typing, and inside waits.
  The run id is retired the moment you click, and the coder's whole process tree is killed so
  its minions cannot survive as orphans burning API credit. A shell command already running is
  the exception. It finishes, or hits its cap.
- Shell commands are **bounded**: a 10 minute ceiling, a fixed working directory, and idle
  detection that turns "this program is waiting for input" into a structured result after 15
  seconds instead of hanging the run.

**What to know going in**

- The `shell` tool is **a shell, not a sandbox**. It runs in a working directory, and its only
  path guard is a case-insensitive substring block on `/system`, `/usr/sbin` and `/private/var`
  on macOS, and `c:\windows` on Windows. Everything else in your home folder is reachable, and
  there is **no human-in-the-loop approval prompt** before a command runs. Run tasks you would
  be comfortable running yourself.
- Minions are read-only by tool set, but their `shell` tool is held to read-only commands by
  its description and prompt rather than by enforcement.
- The browser agent's overlay, the edge glow and its cursor, is injected into the page, so a
  site that looks for it can detect it. The debugging-port **scanner** injects nothing; the
  extension's scanner runs in the extension's own world and leaves nothing in the page.
- While your Chrome runs with the extension's launch flags, your other extensions are off until
  Chrome is started again without them. A run that finds your Chrome open without the extension
  quits it gracefully and starts it again with the flags and your tabs restored.
- On macOS the consent-dialog watcher clicks Allow on permission prompts it finds while a shell
  command or AppleScript runs. It keeps a run from deadlocking, and it does mean a first-time
  permission grant can happen without you being asked.
- The Telegram bot obeys only the first chat that messages it after you connect; Disconnect releases it.
- There is **no automated test suite** in this repository.
- AutoCua drives *your* real machine, *your* real browser profile and *your* real logins. That
  is the point, and it is also the risk.

---

## Project layout

```
main.py                    the only entry point: one run_agent(mode=, provider=, model=, task=, ...) call
pyproject.toml             the AutoCua package, dependencies and the maturin build
agent_capabilities.md      every optional flag, in detail
model_list.txt             provider and model names
AutoCua_data/              YOUR data: chats, keys, skills, browser profiles, vault

AutoCua/
  agent_launcher.py        mode to AgentService dispatch, parallel fan-out, the ui flag
  ui/                      the desktop app: Flask, pywebview, chat, stages, skills, settings
  desktop/                 the app's Chromium window (Electron), used once `npm install` has run in it
  default_skills/          the shipped skills, read-only; yours go in AutoCua_data/skills/
  llm_provider/            every LLM endpoint and the model tables, one copy for all platforms
  mac/  windows/           computer use: agent, controller, tree, sandbox, tool_registry
    agent/main_driver/       the desktop loop
    agent/coder/             the coding agent
    agent/minions/           read-only scouts
    controller/tool/         shell, open_app, screenshot, applescript, kernel_input
    tree/                    accessibility scanner and OCR
  web/                     the Rust browser agent, one crate; browser/AutoCuaBridge/ is the AutoCua Chrome extension
  ios/                     the iPhone and iPad agent
  ios_connector/           WebDriverAgent transport: hardware and simulator sessions
  linux/tree/              the Linux accessibility scanner, see Roadmap
  memory_compression/      context gauge and rolling handoff compression
  agent_conversation/      resumable chat persistence
  vault/                   credential fill that never reaches the model
```

The macOS, Windows and iOS packages stay structurally identical and **never import each
other**, because release binaries are platform specific. See
[AutoCua/Structure.md](https://github.com/AutoCua/AutoCua/blob/main/AutoCua/Structure.md).

---

## Roadmap

**Linux is new in 0.1.5**, with an x86_64 wheel on PyPI. The scanner reads the desktop
through AT-SPI2, captures the screen through the XDG portal, is tested on GNOME Wayland (X11 not yet),
wakes up Electron and Chromium trees that publish nothing, and pulls whole application trees in
one D-Bus call (1,952 GNOME Shell nodes in 29 ms). Text the toolkit never publishes (an
editor's lines, a canvas, a label-less control, a Flutter app such as App Center) is filled in
by OCR, the way macOS and Windows do it with their native engines: Linux has none, so it runs
PaddlePaddle's PP-OCRv6 models locally through onnxruntime (downloaded on first use) and merges
only the text that lands outside every element the tree already names. It emits the same numbered tree and annotated screenshot the other platforms
consume.

The agent, controller and launcher branch are there too: `"computer use"` on a Linux host runs
the same main driver as macOS, and the controller clicks, types and scrolls through the XDG
RemoteDesktop portal (the only route to a native Wayland window) with AT-SPI actions as the
fallback. The desktop app (`ui=True`) and the Telegram remote connection run on Linux too.
Still missing: the domain skills macOS and Windows ship (Google and Microsoft services,
LibreOffice Calc and others); Linux has the browser skill only. Install with pip (see
[Linux, from pip](#linux-from-pip)) or set up a checkout with `bash setup_linux.sh` (see
[Requirements and setup](#requirements-and-setup)). From a checkout you can also try the scanner
and controller on their own; these two self-checks are not in the wheel:

```bash
source .venv/bin/activate
python3 -m AutoCua.linux.tree.test 5 -v          # scan: annotated screenshot + tree
python3 -m AutoCua.linux.controller.test         # click + typing through the portal
```

**Android** is not available yet. `"mobile use, android"` raises a clear error.

---

## Licence, credits and citation

Built and maintained by **Ashish Yadav**. Issues and merge requests are welcome at
[github.com/AutoCua/AutoCua](https://github.com/AutoCua/AutoCua/issues).

Licensed under the **MIT License**, see [LICENSE](https://github.com/AutoCua/AutoCua/blob/main/LICENSE). You may use, copy, modify, merge,
publish, distribute, sublicense and sell this software, including commercially. The only
condition is to retain the copyright and permission notice. Not required but appreciated:
credit the author and link back to the project.

### Third-party components

Some components are **not covered by the MIT licence above** and are **not redistributed** in
this repository. They are fetched from their authors at setup time or on first use. Full details in
[THIRD_PARTY_NOTICES.md](https://github.com/AutoCua/AutoCua/blob/main/THIRD_PARTY_NOTICES.md).

| Component | How it reaches you | Licence |
|---|---|---|
| **WebDriverAgent**, by Facebook, Inc. and the [Appium](https://github.com/appium/WebDriverAgent) project | [`setup_ios.sh`](https://github.com/AutoCua/AutoCua/blob/main/setup_ios.sh) clones it at pinned tag `v15.1.1` | BSD 3-Clause, some files Apache 2.0 |
| **Interception**, a Windows kernel input driver by [Francisco Lopes da Silva](https://github.com/oblitum/Interception) | `setup_windows.bat` downloads the author's signed `v1.0.1` release and verifies its SHA-256 | **Dual:** LGPL v3.0 non-commercial. **Commercial use needs a paid licence from the author** |
| **PP-OCRv6** text detection and recognition models, by [PaddlePaddle](https://huggingface.co/PaddlePaddle) | `AutoCua/utils/ocr_models.py` downloads the two ONNX models from the author's Hugging Face repositories at pinned commits, SHA-256 verified, the first time a Linux scan needs them | Apache 2.0 |

> If you ship or sell anything built on AutoCua that bundles or installs the Interception
> driver, you must obtain a commercial Interception licence yourself. AutoCua's MIT licence
> does not, and cannot, grant it.

### How to cite

> Ashish Yadav. *AutoCua, a multi-agent framework for computer, web, mobile and shell
> automation.* 2026. https://github.com/AutoCua/AutoCua

```bibtex
@software{AutoCua2026,
  author = {Ashish Yadav},
  title  = {AutoCua: a multi-agent framework for computer, web, mobile and shell automation},
  year   = {2026},
  url    = {https://github.com/AutoCua/AutoCua}
}
```
