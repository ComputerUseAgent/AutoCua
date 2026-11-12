// The wake-up tab (wake.html): Chrome stops the extension's worker after half a
// minute with nothing to do, and a stopped worker dials nobody. A message to it
// starts it again (its connect loop then dials every bridge in config.json within
// half a second), and this tab, having done that, closes itself.
chrome.runtime.sendMessage({ type: "wake" }).catch(() => {});
chrome.tabs.getCurrent((tab) => {
  if (tab) chrome.tabs.remove(tab.id);
});
