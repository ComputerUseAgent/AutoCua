// The Test button (in Settings): scan the page under the popup with the extension's own scanner,
// show how long it took, the summary line, the annotated picture and the tree. The
// result goes over the bridge to the agent side (bridge_test.rs, run by test.command),
// which writes tree.txt, annotated_screenshot.jpg and hits.json under debug/iteration_N/.
const $ = (id) => document.getElementById(id);
const manifest = chrome.runtime.getManifest();
$("version").textContent = manifest.version_name || manifest.version;

// The views: home holds the two buttons. Settings opens in its place, with Back
// at the top; Chat opens the chat panel (sidepanel.html) in Chrome's side panel,
// on the right of the window, and the popup goes. Inline handlers are not
// allowed in an extension page, so the wiring is here.
const show = (id) => {
  for (const view of ["home", "settings"]) $(view).hidden = view !== id;
};
for (const b of document.querySelectorAll("[data-open]")) b.addEventListener("click", () => show(b.dataset.open));
for (const b of document.querySelectorAll("[data-back]")) b.addEventListener("click", () => show("home"));
$("open-chat").addEventListener("click", async () => {
  const say = (text) => { $("home").hidden = false; $("note").textContent = text; };
  if (!chrome.sidePanel) {
    say("The chat panel needs the extension reloaded once: chrome://extensions, then Reload on AutoCua.");
    return;
  }
  // Chrome opens a side panel only on a user gesture: asked for the current
  // window first, in the click itself; by the window's id when that is refused.
  try {
    try {
      await chrome.sidePanel.open({ windowId: chrome.windows.WINDOW_ID_CURRENT });
    } catch (e) {
      const win = await chrome.windows.getCurrent();
      await chrome.sidePanel.open({ windowId: win.id });
    }
    window.close();
  } catch (e) {
    say("Could not open the chat panel: " + ((e && e.message) || e));
  }
});

$("test").addEventListener("click", async () => {
  const btn = $("test");
  btn.disabled = true;
  $("status").textContent = "Scanning…";
  let reply = null;
  try {
    reply = await chrome.runtime.sendMessage({ type: "test" });
  } catch (e) {
    reply = { ok: false, error: String((e && e.message) || e) };
  }
  btn.disabled = false;
  if (!reply || !reply.ok) {
    $("status").textContent = "Failed: " + (reply ? reply.error : "no reply from the service worker");
    return;
  }
  const r = reply.result;
  const t = r.timings;
  const ms = (v) => Math.round(v);
  $("status").textContent = r.url;
  $("status").title = r.url;
  $("time").innerHTML = `${ms(t.total_ms)}<small>ms</small>`;
  const pills = [`settle ${ms(t.settle_ms)}`, `read ${ms(t.scan_ms)}`, `picture ${ms(t.shot_ms)}`, `${t.frames} frame${t.frames === 1 ? "" : "s"}`];
  $("split").replaceChildren(...pills.map((p) => Object.assign(document.createElement("span"), { textContent: p })));
  $("summary").textContent = r.summary;
  const saved = $("saved");
  if (reply.saved) {
    saved.textContent = `Saved to ${reply.saved.dir}/`;
    saved.className = "";
  } else {
    saved.textContent = `Not saved: ${reply.save_error || "unknown error"}`;
    saved.className = "bad";
  }
  if (r.screenshot) {
    $("shot").src = "data:image/jpeg;base64," + r.screenshot;
    $("shot").hidden = false;
  }
  const lines = r.tree.split("\n");
  $("treehead").textContent = `Tree (${lines.length} lines)`;
  $("tree").textContent = r.tree;
  $("result").classList.add("on");
});
