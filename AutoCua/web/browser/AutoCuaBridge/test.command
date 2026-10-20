#!/bin/sh
# AutoCuaBridge hand test (bridge_test.rs in the web crate does the work). Stages the
# extension (the copy Chrome must load),
# restarts your own Chrome (Default profile) with it loaded and no debugging port,
# and serves the bridge the extension dials. A Chrome already running is quit first
# (gracefully) and comes back with its tabs; other extensions are off while Chrome
# runs this way. Then: browse to any page, click the AutoCuaBridge icon (puzzle piece;
# pin it once) and press Test. The popup shows how long the scan took; the result is
# written to debug/iteration_N/ in the repo (tree.txt, annotated_screenshot.jpg,
# hits.json), like the real scanner's DEBUG output. Ctrl-C stops the bridge; Chrome
# stays open. Run it again after editing the extension: the staged copy is refreshed
# and Chrome restarted.
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/../../../.." || exit 1
exec python3 -c 'import AutoCua.web.agent; from AutoCua.web.agent_native import bridge_test; bridge_test()'
