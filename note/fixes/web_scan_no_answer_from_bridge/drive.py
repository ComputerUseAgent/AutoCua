"""Drive a scratch Chrome through agent_native.Bridge only, the way the web agent does
in fast mode: open the form, scan, click Search, settle, tabs, glow, then the scan
that failed in the incident, timed. No agent, no LLM, never the person's Chrome.

python drive.py --label L --ext EXT --site http://127.0.0.1:PORT [--trials N]
       [--form-query 'nav=300&frames=3&rdelay=3000...'] [--headless 1] [--gap-ms 0]
       [--url URL] (a real page instead of the form: --url-scan-only)
"""
import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time

# The checkout this note lives in: note/fixes/<this folder>/drive.py
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "..")))
from AutoCua.web.agent_native import Bridge  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--label", required=True)
ap.add_argument("--ext", required=True)
ap.add_argument("--site", default="")
ap.add_argument("--form-query", default="")
ap.add_argument("--trials", type=int, default=1)
ap.add_argument("--headless", type=int, default=1)
ap.add_argument("--gap-ms", type=int, default=0)
ap.add_argument("--after-ms", type=int, default=20000, help="how long to keep watching a stuck scan after the 40 s")
ap.add_argument("--scan-timeout", type=float, default=40.0)
ap.add_argument("--extra", default="", help="extra Chrome switches, space separated")
ap.add_argument("--keep", type=int, default=0, help="leave Chrome running (pids printed)")
ap.add_argument("--sweep", default="", help="key=v1,v2,...: one trial per value, overriding that form-query key")
ap.add_argument("--on-fail", default="", help="back: after a hang, step the tab back in history and trace again")
args = ap.parse_args()

ROOT = os.path.dirname(os.path.abspath(__file__))
LOGS = os.path.join(ROOT, "logs")
os.makedirs(LOGS, exist_ok=True)
STAGED = os.path.join(ROOT, "staged", args.label)
UDD = os.path.join(ROOT, "udd", args.label)
OUT = os.path.join(LOGS, args.label)
os.makedirs(OUT, exist_ok=True)


def ms():
    return int(time.time() * 1000)


def say(*a):
    line = " ".join(str(x) for x in a)
    print(line, flush=True)
    with open(os.path.join(OUT, "run.txt"), "a") as f:
        f.write(line + "\n")


def req(b, payload, timeout):
    t0 = ms()
    try:
        r = b.request(payload, timeout)
        return True, r, t0, ms()
    except Exception as e:  # ScannerError and friends
        return False, f"{type(e).__name__}: {e}", t0, ms()


def chrome_pids():
    """Every process of the Chrome on MY user data folder: its main process carries the
    switch, its helpers share its process group (launch_chrome starts it in a group of
    its own)."""
    out = subprocess.run(["ps", "-axo", "pid=,pgid=,command="], capture_output=True, text=True).stdout
    rows = []
    for line in out.splitlines():
        parts = line.split(None, 2)
        if len(parts) == 3:
            rows.append((int(parts[0]), int(parts[1]), parts[2]))
    mains = {pgid for pid, pgid, cmd in rows if f"--user-data-dir={UDD}" in cmd and pid == pgid}
    return [pid for pid, pgid, cmd in rows if pgid in mains or f"--user-data-dir={UDD}" in cmd]


def kill_chrome():
    pids = chrome_pids()
    for p in pids:
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    for _ in range(40):
        if not chrome_pids():
            break
        time.sleep(0.25)
    for p in chrome_pids():
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass
    return pids


def trace(b, tab, name):
    ok, r, t0, t1 = req(b, {"type": "trace", "tabId": tab}, 15)
    path = os.path.join(OUT, f"{name}.json")
    with open(path, "w") as f:
        json.dump(r if ok else {"error": r}, f, indent=1, default=str)
    return r if ok else None


def summarize_trace(tr, since):
    if not tr:
        return
    for e in tr.get("trace", []):
        if e["t"] < since:
            continue
        x = e.get("x")
        xs = json.dumps(x, default=str) if x is not None else ""
        if len(xs) > 300:
            xs = xs[:300] + "..."
        say(f"    +{e['t'] - since:6d} ms  {e['label']:28s} {xs}")


def find_button(tree, hits, dpr, word="Search"):
    m = re.search(r"\[(\d+)\] <button[^>]*>" + re.escape(word) + r"</button>", tree)
    if not m:
        return None, None
    idx = int(m.group(1))
    for i, rect in hits:
        if i == idx:
            return idx, [v / (dpr or 1) for v in rect]
    return idx, None


# ------------------------------------------------------------------ launch
b = Bridge(dest=STAGED, src=args.ext)
say(f"[{args.label}] bridge port {b.port}, staged {b.staged}, extension id {b.extension_id}")
switches = ["--window-size=1280,900"]
if args.headless:
    switches.insert(0, "--headless=new")
if args.extra:
    switches += args.extra.split()
b.launch_chrome(profile=UDD, chrome_args=switches)
t_launch = ms()
up = b.wait_connected(30)
say(f"[{args.label}] Chrome launched ({' '.join(switches)}), extension dialled in: {up} after {ms() - t_launch} ms; pids {chrome_pids()}")
if not up:
    say("extension never dialled in")
    kill_chrome()
    sys.exit(2)

try:
    ok, tabs, *_ = req(b, {"type": "tabs"}, 10)
    tab = tabs[0]["id"]
    say(f"tabs: {[(t['id'], t.get('url'), t.get('active')) for t in tabs]}")
    ok, r, t0, t1 = req(b, {"type": "activate", "tabId": tab, "wait_load": False}, 35)
    say(f"activate {tab}: {ok} {r} ({t1 - t0} ms)")
    ok, r, t0, t1 = req(b, {"type": "glow"}, 10)
    say(f"glow: {ok} {r} ({t1 - t0} ms)")

    from urllib.parse import parse_qsl, urlencode
    plan = []
    if args.sweep:
        key, vals = args.sweep.split("=", 1)
        for v in vals.split(","):
            qd = dict(parse_qsl(args.form_query))
            qd[key] = v
            plan.append(urlencode(qd))
    else:
        plan = [args.form_query] * args.trials
    results = []
    for trial, fq in enumerate(plan, 1):
        req(b, {"type": "trace_clear", "tabId": tab}, 10)
        url = f"{args.site}/form?{fq}"
        ok, r, t0, t1 = req(b, {"type": "navigate", "tabId": tab, "url": url}, 35)
        say(f"\n=== trial {trial}: navigate {url}: {ok} {r} ({t1 - t0} ms)")
        req(b, {"type": "glow"}, 10)
        ok, sc, t0, t1 = req(b, {"type": "scan", "tabId": tab, "overlay": {}, "marks": True, "screenshot": True}, args.scan_timeout)
        if not ok:
            say(f"first scan failed: {sc} ({t1 - t0} ms)")
            tr = trace(b, tab, f"trial{trial}-firstscan")
            summarize_trace(tr, t0)
            continue
        say(f"first scan ok in {t1 - t0} ms: {sc['summary']} url={sc['url']} shot={'yes' if sc.get('screenshot') else 'no'}")
        idx, rect = find_button(sc["tree"], sc["hits"], sc.get("dpr", 1))
        if rect is None:
            say("no Search button in the tree:\n" + sc["tree"][:1500])
            break
        # the step that failed: click [Search], settle, then the next step's scan
        t_click = ms()
        ok, r, t0, t1 = req(b, {"type": "click", "tabId": tab, "rect": rect, "times": 1}, 10)
        say(f"click [{idx}] rect={[round(v) for v in rect]}: {ok} {r} ({t1 - t0} ms)")
        ok, r, t0, t1 = req(b, {"type": "settle", "tabId": tab}, 3)
        say(f"settle: {ok} {r} ({t1 - t0} ms)")
        ok, r, t0, t1 = req(b, {"type": "tabs"}, 10)
        say(f"tabs: {ok} {[(t['id'], t.get('url'), t.get('pendingUrl'), t.get('status')) for t in r] if ok else r} ({t1 - t0} ms)")
        ok, r, t0, t1 = req(b, {"type": "glow"}, 10)
        say(f"glow: {ok} {r} ({t1 - t0} ms)")
        if args.gap_ms:
            time.sleep(args.gap_ms / 1000)
        ok, sc, t0, t1 = req(b, {"type": "scan", "tabId": tab, "overlay": {}, "marks": True, "screenshot": True}, args.scan_timeout)
        if ok:
            say(f"SCAN AFTER CLICK ok in {t1 - t0} ms (started {t0 - t_click} ms after the click): {sc['summary']} url={sc['url']} shot={'yes' if sc.get('screenshot') else 'no'}")
            results.append((fq, "ok", t1 - t0, t0 - t_click, sc["url"]))
            tr = trace(b, tab, f"trial{trial}-ok")
            summarize_trace(tr, t_click)
        else:
            say(f"SCAN AFTER CLICK FAILED in {t1 - t0} ms (started {t0 - t_click} ms after the click): {sc}")
            results.append((fq, "FAIL", t1 - t0, t0 - t_click, sc))
            tr = trace(b, tab, f"trial{trial}-fail")
            summarize_trace(tr, t_click)
            if tr:
                say(f"  tab now: {json.dumps(tr.get('tab'), default=str)[:400]}")
                say(f"  frames now: {json.dumps(tr.get('frames'), default=str)[:1500]}")
                say(f"  frame states now: {json.dumps(tr.get('states'), default=str)[:1500]}")
                say(f"  top probe: {tr.get('topProbe')}")
            if args.on_fail == "rescan":
                ok2, sc2, r0, r1 = req(b, {"type": "scan", "tabId": tab, "overlay": {}, "marks": True, "screenshot": True}, args.scan_timeout)
                say(f"  a new scan of the same tab right after the hang: {'ok' if ok2 else 'FAILED'} in {r1 - r0} ms: "
                    f"{(sc2['summary'] + ' url=' + sc2['url']) if ok2 else sc2}")
            if args.on_fail == "back":
                tb = ms()
                okb, rb, b0, b1 = req(b, {"type": "history", "tabId": tab, "delta": -1}, 35)
                say(f"  history back after the hang: {okb} {rb} ({b1 - b0} ms)")
                time.sleep(3)
                tr3 = trace(b, tab, f"trial{trial}-after-back")
                if tr3:
                    say("  after history back:")
                    summarize_trace({"trace": [e for e in tr3["trace"] if e["t"] >= tb]}, t_click)
                    say(f"  frame states after back: {json.dumps(tr3.get('states'), default=str)[:2500]}")
                okb, rb, b0, b1 = req(b, {"type": "history", "tabId": tab, "delta": 1}, 35)
                say(f"  history forward again: {okb} {rb} ({b1 - b0} ms)")
            # keep watching: does the stuck scan ever end?
            if args.after_ms:
                time.sleep(args.after_ms / 1000)
                tr2 = trace(b, tab, f"trial{trial}-fail-later")
                if tr2:
                    say(f"  {args.after_ms} ms later:")
                    summarize_trace({"trace": [e for e in tr2["trace"] if e["t"] > t1]}, t_click)
    say("\n=== summary")
    for r in results:
        say("  ", *r)
finally:
    if args.keep:
        say(f"leaving Chrome up: pids {chrome_pids()}")
    else:
        killed = kill_chrome()
        say(f"[{args.label}] stopped my Chrome pids {killed}; still there: {chrome_pids()}")
