# Computer use, the iPhone and the browser at the same time (fixed 2026-10-10/11)

Commits: `22be5d5` (ios_agent, recording), `754af08` (the fixes below), `9bb114f`
(open_app). The iOS picker fix before them is `004014f`.

## Symptom

From the app, a request like "start all three tasks at the same time: a computer
task, a browser task for browser_agent, an iPhone task for ios_agent" went wrong in
two ways, run after run, while the same request from a terminal worked:

- `<sub_agents>` showed `checking connection: ios_agent` for a few steps, then the
  line dropped: the phone never came online, so the iPhone task was never done.
  Picking Mobile use → iOS in a new chat paired the phone fine (sometimes asking
  for the passcode), so the phone itself was not the problem.
- browser_agent quit the running Chrome and started it again, although that Chrome
  already had the AutoCua extension loaded ("Quit the Chrome running without the
  AutoCua extension; it comes back with its tabs.").

Eight separate causes were behind this. They are listed in the order found, with
how each was proven.

## How to read a failure now

`AutoCua_data/ios_agent/phone_link.log` (the same lines go to the terminal as
`[ios_agent]`) records every phone connect of the main agent:

```
23:40:08 checking Ashish (0000...), up to 30s; this process runs at normal priority
23:40:08 busy check took 0.1s: free
23:40:10 activate after 2.2s: {'ok': True, 'state': 'connecting', 'udid': '...',
         'timing': {'free_port': 0.1, 'port_check': 0.0, 'mount_check': 2.0, 'spawn': 0.0}}
23:40:17 online after 9.3s: Ashish · iPhone
23:41:37 released after 89.4s
```

- `timing` splits the start into its steps (`py_start` and `pmd3_start` are the bare
  interpreter and pymobiledevice3's own start, so a slow `mount_check` says whether
  the Mac or the phone was slow).
- `offline after Ns: <reason>` names the cause; a failed start also prints the
  session's own log tail (pymobiledevice3's words).
- `BACKGROUND (App Nap)` in the first line means macOS had the app at background
  priority (cause 5).

The phone's own log is the other half. Pull it with
`pymobiledevice3 syslog collect --start-time <unix time> <folder>` and read it with
`log show --archive <folder> --start ... --predicate ...`. Useful markers:
`testmanagerd` "IDE connected" (a runner session started), `lockdownd`
"handle_get_value" (the Mac's first contact), `LocalAuthenticationUIService`
(the passcode prompt), `automationmode-writer` "requires authentication" /
"Previous authentication has not expired".

## Causes and fixes

### 1. The passcode prompt ate the 20 s check
- **What happens:** iOS 26 asks for the passcode (or Face ID) to turn on
  Automation Mode when the runner starts, whenever the last approval has expired.
  The phone log showed the prompt at 21:27:46, the person typing by 21:27:50, and
  our 20 s check killing the runner at 21:27:56, seconds before it would have
  answered. Mobile use waits 90 s, so the same prompt was harmless there.
- **Where:** [AutoCua/mac/controller/tool/ios_agent.py:52](../../AutoCua/mac/controller/tool/ios_agent.py#L52), `CHECK_SEC`.
- **Fix:** 30 s (the owner's number), and the prompts say the phone may take up to
  10 steps to join the online line. Cause 6 removes the kill altogether.

### 2. The app's own Mobile use session blocked the request, silently
- **What happens:** pair the phone with Mobile use in one chat, then reopen
  another chat. Reopening resets the mode picker silently, so the session stays
  registered in the backend with no owner. The request's phone link then hit
  `activate()`'s "in use by Mobile use in the app" refusal, which nothing ever
  showed. The phone log for that run has no contact from the Mac at all.
- **Where:** [AutoCua/ios_connector/session.py:362](../../AutoCua/ios_connector/session.py#L362), `activate()`.
- **Fix:** a live Mobile use session for the same phone is shared: the link uses it
  (`shared: True`) and `deactivate(owner)` hands it back
  ([session.py:525](../../AutoCua/ios_connector/session.py#L525)). One still
  pairing is refused; one whose processes died is replaced like any dead session.
- **Proof:** live, Mobile use paired in 12 s, then a link came online in 0.5 s on the
  same session, no second runner, no passcode; after release Mobile use was still
  connected.

### 3. Every connect downloaded 15 MB from GitHub
- **What happens:** `pymobiledevice3 mounter auto-mount` ran before every
  connect "to make sure the developer image is mounted". On iOS 17+ it first
  compares its cached image's build id (`~/.pymobiledevice3/...`, `27A5228h`) with
  a constant in the package (`LATEST_DDI_BUILD_ID = "27A5194q"` in 9.32.0). They
  never match, so it downloaded the image from GitHub before touching the phone:
  20-30 s on the owner's Wi-Fi, then GitHub's hourly rate limit (a 5 s fast fail).
  The phone log showed the Mac's first contact 22-28 s after the request started.
- **Where:** [AutoCua/ios_connector/session.py:426](../../AutoCua/ios_connector/session.py#L426).
- **Fix:** `mounter list` first (one lockdown call, about a second); auto-mount only
  when nothing is mounted. Start of the runner: 1.2 s instead of 7-31 s.

### 4. Two `lsof` port checks
- **What happens:** `busy_elsewhere()` and `_free_port()` each ran `lsof` to find
  who listens on port 8100. `lsof` walks every open file of every process; 0.07 s
  on an idle Mac, up to its 8 s timeout while Chrome and its helpers start.
- **Where:** [session.py:187](../../AutoCua/ios_connector/session.py#L187) `_port_answers`,
  [session.py:196](../../AutoCua/ios_connector/session.py#L196) `_forward_pids`.
- **Fix:** one `ps -axo pid,ppid,command` scan plus a socket probe: 0.04 s, the same
  answers (a live run's forward, a leftover forward, a simulator's WDA).

### 5. App Nap (the reason the app failed and the terminal did not)
- **What happens:** the agent works in other apps, so the app's window is soon
  covered and macOS puts the app into App Nap. That drops the app **and every
  process it started** (the backend, both sub-agents, the phone's helpers) to
  background priority. While Chrome relaunches for browser_agent, background
  processes get almost no CPU. Measured: `mount_check` 24.7 s in the app, 0.9 s
  idle; under a full CPU load the same command took 2.9 s at normal priority and
  did not finish in 120 s under `taskpolicy -b`. A terminal process is not an app,
  so it never naps.
- **Where:** [AutoCua/desktop/main.js:379](../../AutoCua/desktop/main.js#L379).
- **Fix:** `powerSaveBlocker.start('prevent-app-suspension')` at app start.

### 6. A runner killed seconds before it answered
- **What happens:** the 30 s check killed the runner when it ran out, and every
  slow start above left the runner only a few seconds. A phone that would have
  answered 5 s later was offline for the whole request.
- **Where:** [ios_agent.py:59](../../AutoCua/mac/controller/tool/ios_agent.py#L59) `LATE_SEC`,
  [ios_agent.py:313](../../AutoCua/mac/controller/tool/ios_agent.py#L313).
- **Fix:** after the 30 s check the line reads offline, but the connect carries on
  quietly and `ios_agent` joins the online line the moment the phone answers (up
  to 120 s, `AutoCua_IOS_LATE_SEC`). The request ending still lets the phone go.

### 7. Chrome quit and relaunched although the extension was there
- **What happens:** `take_chrome` gave the extension in the running Chrome 2 s to
  dial the new bridge, then quit Chrome as "running without the extension".
  Measured call-in from the running Chrome: 0.2 s when the worker is warm (a run a
  minute after the last: the owner's terminal tests), 1.4-4.9 s after minutes of
  idle. Chrome stops the extension's worker by about 5 minutes idle despite its
  20 s keepalive ([background.js:231](../../AutoCua/web/browser/AutoCuaBridge/background.js#L231)),
  and a stopped worker dials nobody until something wakes it. The staged
  extension path is the same for the app and the terminal (`~/.AutoCua/AutoCuaBridge`),
  so it was not an identity mismatch.
- **Where:** [AutoCua/web/browser/bridge.rs:53](../../AutoCua/web/browser/bridge.rs#L53) `ATTACH_WAIT`,
  [bridge.rs:60](../../AutoCua/web/browser/bridge.rs#L60) `WAKE_WAIT`,
  [bridge.rs:185](../../AutoCua/web/browser/bridge.rs#L185) `wake_extension`,
  [bridge.rs:519](../../AutoCua/web/browser/bridge.rs#L519) in `take_chrome`.
- **Fix:** when nobody dials within 2 s, the agent opens the extension's new
  `wake.html` in the running Chrome (`open -g -a <Chrome.app>` on macOS, the Chrome
  binary with the URL elsewhere: a second Chrome hands a URL to the running one).
  `wake.js` messages the worker, which starts it, and closes its own tab. Then up
  to 8 s more before the old quit-and-relaunch, which stays as the last resort.
- **Proof:** after 6.5 min idle, attached in 4.9 s, same Chrome process, the wake
  tab gone. The app's runs that night attached in 0.2 s.

### 8. The rebuilt Rust module would not load
- **What happens:** `cargo build --release` with the profile's `strip = true`
  (and `strip = "debuginfo"`) gives a library that this macOS refuses:
  `dlopen ... (mis-aligned LINKEDIT string pool)`. Re-signing does not help. The
  library from 2026-10-09 loads because it predates the toolchain (Xcode's
  ld-27037). `agent_native.so` is git-ignored, so a bad build is not recoverable
  from git: keep a copy before rebuilding.
- **Where:** [AutoCua/utils/rust.py:140](../../AutoCua/utils/rust.py#L140).
- **Fix:** the loader builds with `CARGO_PROFILE_RELEASE_STRIP=false` on macOS
  (0.8 MB bigger). Other platforms are untouched.

## Also fixed in the same days

- **iOS picker: picking Mobile use → iOS again did not pair** (`004014f`): a stale
  `iosActive` flag after the silent picker reset from New chat or a reopened chat.
  Every iOS pick now activates; a `pairing` flag stops a second click from
  restarting a pairing half-way
  ([ios_session.js:232](../../AutoCua/ui/agent_mode/ios_session.js#L232)).
- **Recording did nothing in the Chromium window** (`22be5d5`, item 2 of
  [issue.md](issue.md)): the shell's window stand-in has no native handle, and
  the recorder read it at the start of every run. It now records the main screen
  only, one `screen.mp4` per run in both modes
  ([run_recorder.py:1226](../../AutoCua/utils/run_recorder.py#L1226)); the
  app-window video is gone.
- **`open_app` stretched every window to the screen** (`9bb114f`): the Mac tool
  moved the frontmost window to the main display and sized it from the menu bar to
  the Dock on every open, and after every successful AppleScript. Removed; the
  agent decides when to move or maximize
  ([open_app.py:147](../../AutoCua/mac/controller/tool/open_app.py#L147)).

## Verified

- From the app (launched with `python -m AutoCua.ui.service`, request through
  `POST /api/start-agent`): a three-task request ran all three parts to the end,
  phone online in 9.3 s while browser_agent started, Chrome attached without a
  relaunch, every process at normal priority. The owner's own three-task run
  afterwards: phone connected quickly, Chrome attached.
- Scratch harness (not in the repo): 22 groups over the phone link's rules
  (sharing, takeover, late online, port checks, window, stop).

## Still open

- One `mount_check` of 19.8 s with no Chrome relaunch (23:30, after 11 min idle),
  before the App Nap fix was proven in effect. Every slow connect that night came
  after minutes of idling and every fast one shortly after another; the
  `py_start` / `pmd3_start` marks will say whether it was the Mac or the phone.
- The wake brings Chrome to the front for a moment, even with `open -g`.
- Chrome stops the extension's worker after about five minutes idle although the
  keepalive calls an extension API every 20 s; why is not known. The wake covers it.
- The passcode prompt (cause 1) cannot be answered by the app; it now just waits.
- A terminal simulator run started while a computer-use request holds the phone is
  refused with the clear "port 8100" message. Fixing it means moving the port.
- Killing a terminal run with SIGTERM can leave its sub-agent children and
  pymobiledevice3 helpers behind (seen once, cleaned by hand).
