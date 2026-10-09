# Known issues: the Chromium (Electron) window

Found by a code review on 2026-10-09. **None of these is fixed yet.** All of
them are in the Chromium window (`AutoCua/desktop` and the desktop-shell part of
`AutoCua/ui/service.py`), not the pywebview window.

Line numbers were refreshed on 2026-10-09 (after 458885d) and move as the files change; search for the
quoted code if a line no longer matches.

---

## Serious

### 1. Closing the window skips cleanup
- **What happens:** a coding agent that is still running keeps running in the
  background and keeps calling the model. On Windows the mouse cursor stays as
  the agent's cursor. On Windows/Linux a recording is left unfinished
  (`screen.part.mp4`, never renamed to `screen.mp4`).
- **Where:**
  - [AutoCua/ui/service.py:3932](../../AutoCua/ui/service.py#L3932): `os._exit(0)` in `_serve_desktop_shell` skips every `atexit` handler.
  - The handlers it skips:
    - `_kill_shell_trees_on_exit` at [AutoCua/ui/service.py:2380](../../AutoCua/ui/service.py#L2380)
    - `atexit.register(stop)` at [AutoCua/utils/agent_glow.py:1350](../../AutoCua/utils/agent_glow.py#L1350)
    - `finalize_all` at [AutoCua/utils/run_recorder.py:1513](../../AutoCua/utils/run_recorder.py#L1513)
  - [AutoCua/desktop/main.js:101](../../AutoCua/desktop/main.js#L101): `setTimeout(() => child.kill('SIGKILL'), 5000)` kills the backend after 5 s.
- **Fix:** before `os._exit(0)`, run `atexit._run_exitfuncs()` inside a
  try/except; `atexit` is already imported. Raise the kill timeout in main.js
  from 5000 to 15000, so the cleanup has time to finish.

### 2. Recording doesn't work on Mac
- **What happens:** with Settings → Recording on, nothing is recorded in the
  Chromium window. The log only says `[recorder] could not start`.
- **Where:**
  - [AutoCua/utils/run_recorder.py:1371](../../AutoCua/utils/run_recorder.py#L1371): `int(self.ui_window.native.windowNumber())` fails, because the Chromium window has no native handle.
  - [AutoCua/ui/service.py:1070](../../AutoCua/ui/service.py#L1070): `native = None` in `DesktopWindow`.
  - [AutoCua/ui/service.py:2109](../../AutoCua/ui/service.py#L2109): `run_recorder.start(webview_window, ...)` passes that window.
- **Fix:** in desktop mode, pass `None` as the window at service.py:2109, so the
  screen video still records. Recording the app's own window needs Electron's
  window id (`win.getMediaSourceId()` in main.js).

### 3. No fallback when the Chromium window can't start
- **What happens:** if Electron fails to start, nothing opens and
  `run_agent(ui=True)` still returns "desktop app closed". Common causes:
  - Ubuntu 23.10+ blocks its sandbox by default;
  - missing system libraries;
  - an old macOS.
- **Where:** [AutoCua/ui/service.py:3911](../../AutoCua/ui/service.py#L3911):
  `subprocess.run([str(exe), str(DESKTOP_SHELL_DIR)], env=env, check=False)`
  ignores the exit code, then `return True`. An `OSError` from the launch isn't
  caught either.
- **Fix:** check the exit code and catch `OSError`. If Electron failed (non-zero,
  or exited within a few seconds), return False so main() opens the pywebview
  window. On Linux, detect the unusable `chrome-sandbox` up front and print how
  to fix it.

---

## Medium

### 4. A crash leaves the window behind
- **What happens:** if the Python process that opened the app dies (killed or
  crashed), Electron and its backend keep running. Because of the
  single-instance lock, every later launch just brings that old window forward.
- **Where:**
  - [AutoCua/desktop/main.js:363](../../AutoCua/desktop/main.js#L363): `app.requestSingleInstanceLock()`.
  - [AutoCua/ui/service.py:3907-3911](../../AutoCua/ui/service.py#L3907): the launcher passes no parent process id.
- **Fix:** pass the launcher's pid in the env (e.g. `AUTOCUA_DESKTOP_PARENT`),
  and have main.js check it every few seconds and quit when that process is
  gone.

### 5. If Python can't start, quitting hangs forever
- **What happens:** with a wrong Python path (ENOENT/EACCES), Node emits `error`
  but never `exit`. So `backend` stays set, and `before-quit` waits forever in
  `stopBackend()`.
- **Where:**
  - [AutoCua/desktop/main.js:86](../../AutoCua/desktop/main.js#L86): `child.on('error', reject);`
  - `stopBackend()` at [AutoCua/desktop/main.js:97](../../AutoCua/desktop/main.js#L97).
  - `before-quit` at [AutoCua/desktop/main.js:394](../../AutoCua/desktop/main.js#L394).
- **Fix:** in the `error` handler, set `backend = null` when the child never
  started (`child.pid === undefined`), then reject.

### 6. Mac: the first click after a restart can be lost
- **What happens:** the hidden backend makes itself the active app when it
  starts. After the app restarts itself (the setup wizard's restart, Reset
  everything), the window is only reloaded, so focus never comes back to it and
  the first click or keypress goes nowhere.
- **Where:**
  - [AutoCua/ui/service.py:3942](../../AutoCua/ui/service.py#L3942): `ns_app.run()` in `_serve_desktop_shell`.
  - The relaunch at [AutoCua/desktop/main.js:207](../../AutoCua/desktop/main.js#L207): `if (win) load(url);`
- **Fix:** when the backend becomes active, hand activation straight back to the
  Electron window, or have main.js focus the window after a relaunch.

### 7. Windows: an extra black console window
- **What happens:** started without a terminal (a shortcut, `npm start`),
  Windows opens a console window for the backend, and it stays open.
- **Where:** [AutoCua/desktop/main.js:70](../../AutoCua/desktop/main.js#L70): the
  `spawn(...)` options have no `windowsHide`.
- **Fix:** add `windowsHide: true` to the spawn options.

### 8. Electron's default menu shows up
- **What happens:**
  - On Windows and Linux, a File/Edit/View/Window/Help bar appears that the app never had.
  - On every OS, Ctrl/Cmd+R reloads the app in the middle of a run and Ctrl/Cmd+W closes it.
  - Developer tools can be opened.
- **Where:**
  - No menu is set: [AutoCua/desktop/main.js:372](../../AutoCua/desktop/main.js#L372) (`app.whenReady()`).
  - The import line is at [AutoCua/desktop/main.js:25](../../AutoCua/desktop/main.js#L25).
- **Fix:** `Menu.setApplicationMenu(null)` on Windows/Linux. On macOS, keep only
  the app menu and the Edit menu: copy/paste shortcuts need Edit.

### 9. Memory leak in the event stream
- **What happens:** a `HEAD` request to `/api/desktop/events` subscribes a queue
  that is never removed. From then on that queue keeps every event, screenshots
  included, for the life of the app.
- **Where:** [AutoCua/ui/service.py:1141](../../AutoCua/ui/service.py#L1141):
  `stream = win.subscribe()` runs in the view body, but the unsubscribe is in the
  generator's `finally`, which never runs for `HEAD`.
- **Fix:** move `win.subscribe()` to the first line of the inner `events()`
  generator.

---

## Small

### 10. The last log lines are lost on quit
- **What happens:** the backend's last prints (up to 8 KB) never reach the
  terminal.
- **Where:**
  - [AutoCua/ui/service.py:3649](../../AutoCua/ui/service.py#L3649) and [AutoCua/ui/service.py:3657](../../AutoCua/ui/service.py#L3657): stdout/stderr are re-wrapped in `io.TextIOWrapper` with a buffer.
  - [AutoCua/ui/service.py:3932](../../AutoCua/ui/service.py#L3932): `os._exit(0)` never flushes it.
- **Fix:** pass `line_buffering=IS_DESKTOP_SHELL` to both wrappers, or flush
  stdout/stderr before `os._exit(0)`.

### 11. `node main.js` starts endless node processes
- **What happens:** run with plain `node` instead of Electron, the guard re-runs
  `process.execPath`, which is node again, so the guard fires again forever.
- **Where:** [AutoCua/desktop/main.js:17](../../AutoCua/desktop/main.js#L17):
  `if (typeof require('electron') === 'string') { ... spawnSync(process.execPath, ...) }`.
- **Fix:** re-run the Electron binary that `require('electron')` returns (its
  path), not `process.execPath`, and only if that file exists.

### 12. Started from VS Code, Ctrl+C leaves the app running
- **What happens:** with `ELECTRON_RUN_AS_NODE` set (VS Code), the process that
  re-runs Electron blocks in `spawnSync` and doesn't pass Ctrl+C on. Electron and
  its backend keep running.
- **Where:** [AutoCua/desktop/main.js:17-23](../../AutoCua/desktop/main.js#L17).
- **Fix:** use async `spawn` and forward SIGINT/SIGTERM to the child.

### 13. `--desktop` in your own script's arguments hangs the app
- **What happens:** if the script that calls `run_agent(ui=True)` is run with
  `--desktop`, the app acts as the hidden backend: no window, it just waits.
- **Where:** [AutoCua/ui/service.py:104](../../AutoCua/ui/service.py#L104):
  `IS_DESKTOP_SHELL = "--desktop" in sys.argv` reads the caller's arguments at
  import.
- **Fix:** `IS_DESKTOP_SHELL = __name__ == "__main__" and "--desktop" in sys.argv`.

### 14. `npm install` on old Node.js fails with a confusing error
- **What happens:** Electron 44 declares Node.js 22.12+. On older Node, npm only
  warns, then fails deep inside Electron's install script with
  `ERR_REQUIRE_ESM`.
- **Where:** [AutoCua/desktop/package.json:8-14](../../AutoCua/desktop/package.json#L8):
  no `engines` and no version check.
- **Fix:**
  - Add `"engines": {"node": ">=22.12.0"}`.
  - Add a clear check in `postinstall` that tests `process.features.require_module`.
  - Don't use `engine-strict`: Node 20.19+ also works.

---

## Also open (from the Electron assessment)

- **No lock on the app's local API.**
  - Any program on the machine can call it, including [`/api/shell-exec`](../../AutoCua/ui/service.py#L3047).
  - In the Chromium window it can also read [`/api/desktop/events`](../../AutoCua/ui/service.py#L1133), which carries every screenshot.
  - The only guard checks Host/Origin: [AutoCua/ui/service.py:274](../../AutoCua/ui/service.py#L274).
  - Fix: a token that changes on every launch.
- **Heavy spots in the page itself** (the same in every window):
  - Agent text is typed one letter at a time, with a layout per letter: [AutoCua/ui/script.js:914](../../AutoCua/ui/script.js#L914).
  - An animation loop never stops: [AutoCua/ui/container/bottom_left/bottom_left.js:1401](../../AutoCua/ui/container/bottom_left/bottom_left.js#L1401).
  - 17 CSS animations run while idle.
  - A 0.6–1.5 MB screenshot is pushed into the page every agent step.

---

## Fixed since this list was written

- **Title bar not tinted in the Chromium window**: fixed 2026-10-09 in 88d17c1. The
  window draws its own off-white bar (`AutoCua/desktop/main.js`, `createWindow`).
- **Web scan: "no answer from AutoCuaBridge in 40s"**: fixed 2026-10-09 in 458885d.
  See [web_scan_no_answer_from_bridge.md](web_scan_no_answer_from_bridge.md).
