# Web agent: "scan: no answer from AutoCuaBridge in 40s" (fixed 2026-10-09)

The proof is reproducible: the test next to this note (`web_scan_no_answer_from_bridge/`)
shows the bug on the old code and the fix on the new.

## Symptom

Extension mode, meaning the web agent drives the person's own Chrome through the
AutoCuaBridge extension. At random, usually just after a click that opens a new page
(a form submit, a Search button), the terminal shows:

```
Step 8: Scanning snapshot.
Error processing request: scan: no answer from AutoCuaBridge in 40s
```

The run then ends with
`Agent terminated (error / provider): could not read the page: scan: no answer from AutoCuaBridge in 40s`.

First caught on booking.com: step 7 clicked Search, and the step 8 scan hung. It had
also happened often before 2026-10-09, so it was not caused by that day's UI, Electron,
port or folder changes. The scan code dates from 2–3 Oct.

## Root cause (proven)

1. **The click doesn't wait.** It returns as soon as the press lands (`tools.js`, the
   "click" tool). Nothing waits for the navigation it starts.
2. **The scan goes out at once.** The next step scans immediately: `browser.rs`
   `scan_once`, `start()` and `bridge_scan` have no load wait.
3. **It reads the old page.** The scan's read is
   `chrome.scripting.executeScript({allFrames, injectImmediately, func: elementScanPage})`
   in `element.js` `elementScan`, and it has no time limit. It goes into the OLD page
   while the new page is on its way. (Chrome showed the new page as the tab's
   `pendingUrl`.)
4. **Chrome freezes the old page.** The new page commits, and Chrome puts the old page
   into its back/forward cache, frozen.
   - The old page's cross-site iframes were halfway through the scan's handshake: a
     cross-origin frame in `elementScanPage` returns a Promise that waits for its
     parent's "place" message. On booking.com these were ad frames (Google safeframe and
     the adtrafficquality "sodar" frame).
   - A frozen page gets no messages and runs no timers, so the read never settles.
5. **The watchdog misses it.** The freeze watchdog (`tools.js` `toolsWatchScan`) probes
   the tab's CURRENT page, which is now the new one. That page answers, so nothing ends
   the stuck scan.
6. **The run ends.** Rust gives up after 40 s (`browser.rs` `"scan" => 40.0`, `bridge.rs`
   `request()`). `scan_core` has no retry in extension mode, so the agent ends the run
   (`agent/main_driver/service.rs`, "could not read the page").

**Evidence.** All of it was measured in headless Chrome 154 with a temp profile,
driven through `agent_native.Bridge`, with no agent and no LLM:

| Case | Result |
|---|---|
| Old code | 4 of 8 hung, and 3 of 5 hung in a later run. Every hang was exactly this error, at 40.0–40.2 s. |
| Back/forward cache turned off | 0 of 12 hung |
| Old page with no cross-site iframes | 0 of 12 hung |
| History-back to the cached page after a hang | the stuck read finished 1–2 ms later |
| A fresh scan right after a hang | worked in about 0.1 s |
| The real run's screen recording | the scan went out within about 0.5 s of the Search click, as the results page committed |

## The fix (extension only, 3 files)

This is the owner's rule: **scan only a loaded page; if the page changes during the
scan, wait for the new page to load and scan that; a page not responding for 5 s gets
reloaded.**

- **`AutoCua/web/browser/AutoCuaBridge/tools.js`**, new section
  "a scan reads a loaded page":
  - `toolsScanLoaded(tabId, scan)`:
    - Waits for the page (`toolsPageLoaded`), then runs the read.
    - Races the read against `toolsPageLeaves`.
    - If the tab moves on to a new page during the read, it drops that read (never
      waits for it), waits for the new page, and reads again.
    - The whole scan has a 30 s budget (`TOOLS_SCAN_BUDGET_MS`), under Rust's 40 s.
      After that it answers with an error rather than staying silent.
  - `toolsPageLoaded(tabId, until)`: waits until no new page is on its way (no
    `tab.pendingUrl`) and the document is parsed (`readyState` is not "loading"). It
    gives up after 10 s (`TOOLS_SCAN_LOAD_MS`).
  - `toolsDocumentParsed(tabId)`: a cheap `document.readyState` probe of the main frame,
    bounded to 1 s.
  - `toolsPageLeaves(tabId)`: listens to `chrome.webNavigation.onBeforeNavigate` and
    `onCommitted`, main frame only (`frameId === 0`). An ad frame loading, or the page
    calling `history.pushState`, does not count as a page change.
  - Unchanged: a page that doesn't answer for 5 s is closed and opened again
    (`toolsWatchScan` calls `toolsReopen`). That is the force reload.
- **`AutoCua/web/browser/AutoCuaBridge/background.js`**: the "scan" request now calls
  `elementScan` through `toolsScanLoaded`. This covers both the normal read and the read
  retried after a failed screenshot.
- **`AutoCua/web/browser/AutoCuaBridge/manifest.json`**: adds the `"webNavigation"`
  permission.

**Verified.** With the same test, the fixed code went 15 of 15 OK in 0.34–0.56 s, and
every scan returned the NEW page. In the same session, the old code had 3 of 5 hang,
and the other 2 returned the stale OLD page, so the agent would have acted on old
content.

**After changing the extension, quit Chrome fully (Cmd+Q) before the next web run.**
A Chrome that is already running keeps the old extension: `bridge.rs` `take_chrome`
reuses it. The next run restages `~/.AutoCua/AutoCuaBridge` (`stage_bridge.rs`) and
starts Chrome with the new copy.

## If it happens again

1. **Is Chrome running the fixed extension?**
   - Go to chrome://extensions, find AutoCua, and click "service worker" to open its
     console.
   - Run `typeof toolsScanLoaded`. It must print `"function"`.
   - If it doesn't, quit Chrome and run again.
2. **Which action came right before the failing scan?**
   - Look at `AutoCua_data/agent_conversation/<chat>/conversation.json`, under
     `history.assistant_messages` and `tool_responses`, and at `memory_log.txt`.
   - Note that these folders are gone once the chat is deleted in the app.
3. **Known gaps this fix does not cover.** In these, the scan can still hang while the
   page answers probes, with no navigation involved:
   - `captureVisibleTab` (the screenshot in `element.js` `elementScan`) on a page that
     cannot paint. The await has no limit.
   - The read of a subframe that has received its headers but no body.
   - The Rust side still ends the whole run on one scan failure (`browser.rs`
     `scan_core`, no retry in extension mode). Retrying the scan once there would make
     any remaining hang non-fatal.
4. **Check whether the navigation case came back.** Re-run the test below against the
   current extension.

## The test (`web_scan_no_answer_from_bridge/`)

The test uses headless Chrome with a temp profile, never your own Chrome, and drives it
only through `AutoCua.web.agent_native.Bridge`. It never runs the agent or an LLM. It
opens a form page with cross-site iframes, scans it, clicks Search (which navigates
`rdelay` ms later), and then sends the scan that used to hang.

```bash
cd ~/Desktop/AutoCua/note/fixes/web_scan_no_answer_from_bridge && python3 site/server.py 18744 site.jsonl & sleep 1 && ~/Desktop/AutoCua/.venv/bin/python drive.py --label check --ext ~/Desktop/AutoCua/AutoCua/web/browser/AutoCuaBridge --site http://127.0.0.1:18744 --form-query 'nav=0&frames=6&heavy=3000&store=1&rdelay=230&rframes=4&rheavy=400&rslow=0&rstore=1' --sweep 'rdelay=200,215,225,235,250' --after-ms 0; kill %1
```

Each trial prints a line with three parts: ok or FAIL, the time in ms, and what was read.

- `FAIL ... no answer from AutoCuaBridge in 40s`: the bug.
- `ok` with a `/form` URL: a stale read of the old page, which is also wrong.
- `ok` with a `/results` URL: correct.

`--ext` takes any copy of the AutoCuaBridge folder, as long as it has its `glow/`
sibling next to it. To test the old code, extract it from git into a temp folder and
point `--ext` at that.

The test writes `staged/`, `udd/` (Chrome profiles) and `logs/` into its folder. Delete
them afterwards. It kills only the Chrome it started.

`instrument.py <copy of AutoCuaBridge>` adds a trace log to a COPY of the extension,
showing which await hangs. Its text anchors match the pre-fix code, so update them if
the files have changed.
