#!/usr/bin/env python3

# WebDriverAgent one-shot signer + runner.
#
# Layout expected (put this file and the html/ folder next to a fresh WDA clone):
#
#   WebDriverAgent/                <- you clone this here, untouched
#   html/index.html                <- the UI
#   setup.py                       <- run this
#
# What it does when you hit Connect in the browser:
#   1. validates team id / device / project
#   2. rewrites WebDriverAgent.xcodeproj/project.pbxproj (Automatic signing, team, bundle ids)
#      -> exactly the three targets you set by hand: Lib, Runner, IntegrationApp
#   3. mounts the developer disk image (pymobiledevice3, best effort)
#   4. runs xcodebuild with -allowProvisioningUpdates  (this IS the "Test" click)
#   5. streams every line back to the page, live
#
# Not handled here (one-time, already done, lives outside the project file):
#   Apple ID signed into Xcode, phone trusted, Developer Mode on.
#
# Free-account reality: the profile expires every 7 days. The fix is just re-running
# this. No UI, ever. `test` mode stays running once the WDA server is up -- that is
# success; use Stop to end it. `build only` mode exits cleanly on a good build.

import os
import re
import sys
import pty
import shutil
import json
import time
import plistlib
import select
import signal
import tempfile
import threading
import subprocess
import webbrowser
from pathlib import Path
from urllib.parse import urlparse, parse_qs
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# ===================== EDIT THESE IF YOU NEED TO =====================
PORT           = 8765
WDA_DIR_NAME   = "WebDriverAgent"           # the fresh clone folder, sibling to this file
XCODEPROJ_NAME = "WebDriverAgent.xcodeproj"
SCHEME         = "WebDriverAgentRunner"
BUNDLE_PREFIX  = "com.AutoCua"              # default; also editable in the UI
TARGET_NAMES   = ["WebDriverAgentLib", "WebDriverAgentRunner", "IntegrationApp"]
DERIVED_NAME   = "build"                    # derived data name under the shared build root
DEFAULT_MODE   = "test"                     # "test" or "build-for-testing"
# =====================================================================

SCRIPT_DIR = Path(__file__).resolve().parent
WDA_DIR    = SCRIPT_DIR / WDA_DIR_NAME
PROJECT    = WDA_DIR / XCODEPROJ_NAME


def ensure_project():
    """Clone WebDriverAgent if it isn't here yet, and report whether it is.

    ios_setup.sh does this for a checkout; a `pip install AutoCua` runs no
    shell script, so the fetch has to happen on first use instead. This file
    runs as a SCRIPT (setup_server spawns `python setup.py` with cwd=this
    folder), so AutoCua is not necessarily importable — a wheel install finds
    it in site-packages, a checkout needs its root on sys.path first.
    """
    if PROJECT.exists():
        return True
    try:
        try:
            from AutoCua.utils.wda import ensure_wda
        except ImportError:
            sys.path.insert(0, str(SCRIPT_DIR.parent.parent))
            from AutoCua.utils.wda import ensure_wda
        ensure_wda()
    except Exception as e:
        print(f"  (could not fetch WebDriverAgent: {e})")
    return PROJECT.exists()
try:                                            # package import (app.py embeds this UI)
    from AutoCua.ios_connector.build_paths import wda_build_root
except ImportError:                             # standalone: python .../setup.py
    from build_paths import wda_build_root
# Not SCRIPT_DIR: build products in a TCC-guarded folder (~/Desktop and friends)
# make macOS prompt mid-build. See build_paths.
DERIVED    = wda_build_root() / DERIVED_NAME

# The UI can live either right next to setup.py (index.html) or in an html/ folder.
# Prefer whichever exists so the layout is forgiving.
def _find_index_html():
    for candidate in (SCRIPT_DIR / "index.html", SCRIPT_DIR / "html" / "index.html"):
        if candidate.exists():
            return candidate
    # default target if neither exists yet -- keeps the error message sensible
    return SCRIPT_DIR / "index.html"

INDEX_HTML = _find_index_html()


def pymobiledevice3_path():
    """Resolve the pymobiledevice3 CLI. Prefer the copy sitting next to the
    running Python interpreter -- that way `venv/bin/python setup.py` finds it
    without the venv having to be `activate`d -- then fall back to PATH.
    Returns the full path as a string, or None if it isn't installed."""
    cand = Path(sys.executable).parent / "pymobiledevice3"
    if cand.exists():
        return str(cand)
    return shutil.which("pymobiledevice3")


SERVER_RE = re.compile(r"ServerURLHere->(\S+?)<-ServerURLHere")

# the running child (xcodebuild / ruby / mounter), so /stop can kill it
CURRENT_PROC = None
PROC_LOCK = threading.Lock()

# Ruby is the reliable per-target editor. It reads its config from the environment
# so there's nothing to escape. Passed vars: PROJECT_PATH, TEAM_ID, WDA_TARGETS_JSON.
RUBY_SCRIPT = r"""
require 'json'
begin
  require 'xcodeproj'
rescue LoadError
  STDERR.puts "MISSING_GEM"
  exit 3
end

project_path = ENV.fetch('PROJECT_PATH')
team         = ENV.fetch('TEAM_ID')
targets      = JSON.parse(ENV.fetch('WDA_TARGETS_JSON'))   # { "TargetName" => "bundle.id" }

project = Xcodeproj::Project.open(project_path)
attrs   = (project.root_object.attributes['TargetAttributes'] ||= {})
done    = 0

project.targets.each do |t|
  next unless targets.key?(t.name)
  bundle = targets[t.name]
  t.build_configurations.each do |c|
    c.build_settings['CODE_SIGN_STYLE']           = 'Automatic'
    c.build_settings['DEVELOPMENT_TEAM']          = team
    c.build_settings['PRODUCT_BUNDLE_IDENTIFIER'] = bundle
    # remove any manual profile refs that would fight automatic signing
    c.build_settings.delete('PROVISIONING_PROFILE_SPECIFIER')
    c.build_settings.delete('PROVISIONING_PROFILE')
  end
  a = (attrs[t.uuid] ||= {})
  a['ProvisioningStyle'] = 'Automatic'
  a['DevelopmentTeam']   = team
  STDOUT.puts "  signed  #{t.name}  ->  #{bundle}"
  STDOUT.flush
  done += 1
end

project.save
STDOUT.puts "  wrote project.pbxproj  (#{done} targets)"
if done < targets.size
  missing = targets.keys.reject { |k| project.targets.map(&:name).include?(k) }
  STDERR.puts "  note: targets not found in project: #{missing.join(', ')}"
end
"""


# UI-scripts Xcode to Settings > Accounts and presses "+" so the add-account
# sheet is already open. Every step is a best-effort `try`: worst case the user
# lands on a plain Xcode and the web page's manual steps take over. The pane
# itself is pre-selected via the IDELastViewedSettingsPane pref (see
# route_add_account), the toolbar click below is only a backup.
XCODE_ACCOUNTS_SCRIPT = r'''
tell application "Xcode" to activate
tell application "System Events"
    -- wait for Xcode to come up (a cold launch can take a while)
    repeat 40 times
        if (exists process "Xcode") then
            if frontmost of process "Xcode" then exit repeat
        end if
        delay 0.5
    end repeat
    if not (exists process "Xcode") then return "NO_XCODE"
    tell process "Xcode"
        set frontmost to true
        delay 0.4
        try
            click menu item "Settings…" of menu "Xcode" of menu bar 1
        on error
            try
                click menu item "Preferences…" of menu "Xcode" of menu bar 1
            on error
                -- keystrokes go to whatever app has focus, so only as a last
                -- resort and only if Xcode really is frontmost
                if frontmost then keystroke "," using command down
            end try
        end try
        delay 1.2
        try
            click button "Accounts" of toolbar 1 of window 1
        end try
        delay 0.6
        try
            click (first button of window 1 whose description contains "add")
        on error
            try
                click (first button of window 1 whose description contains "Add")
            end try
        end try
    end tell
end tell
return "OK"
'''


def sh(cmd, inp=None):
    """Blocking one-shot for short checks. Returns (returncode, combined_output)."""
    try:
        p = subprocess.run(cmd, input=inp, capture_output=True, text=True)
        return p.returncode, (p.stdout or "") + (p.stderr or "")
    except FileNotFoundError:
        return 127, ""


def _xcode_prefs():
    """com.apple.dt.Xcode preferences as a dict.
    Primary: `defaults export` -- it goes through cfprefsd, so it's fresh
    immediately after Xcode writes (the UI polls this while the user signs in;
    reading the plist file directly can lag behind).
    Fallback: parse the plist file directly.
    Returns (prefs_dict, ok). ok=False means the state is unknown (unreadable
    prefs) -- callers must treat that as 'don't know', never as 'signed out'.
    NOTE: must not use sh() -- that is text-mode and appends stderr to stdout,
    which would corrupt the plist bytes."""
    try:
        p = subprocess.run(["defaults", "export", "com.apple.dt.Xcode", "-"],
                           capture_output=True, timeout=10)
        if p.returncode == 0 and p.stdout:
            return plistlib.loads(p.stdout), True
    except Exception:
        pass
    plist = Path.home() / "Library/Preferences/com.apple.dt.Xcode.plist"
    if not plist.exists():
        # Xcode has never been configured on this machine: definitively no accounts
        return {}, True
    try:
        with open(plist, "rb") as f:
            return plistlib.load(f), True
    except Exception:
        return {}, False


_TEAM_ID_RE = re.compile(r"^[A-Z0-9]{10}$")


def _walk(node):
    """Yield every dict nested anywhere inside node (dicts/lists, any depth)."""
    if isinstance(node, dict):
        yield node
        for v in node.values():
            yield from _walk(v)
    elif isinstance(node, list):
        for v in node:
            yield from _walk(v)


def detect_xcode_accounts():
    """Apple Accounts signed into Xcode (Settings > Accounts) and their teams.
    This is the source of truth for 'can -allowProvisioningUpdates create/renew
    free provisioning profiles' -- a keychain cert alone cannot.
    Returns {"ok": bool, "accounts": [emails], "teams": {team_id: name}}.
    Parsing is deliberately defensive: the field names inside these keys are
    undocumented, so walk every nested dict and accept anything that carries a
    10-char [A-Z0-9] team id."""
    prefs, ok = _xcode_prefs()

    # accounts: DVTDeveloperAccountManagerAppleIDLists ->
    #   {"IDE.Identifiers.Prod": [ ...account dicts or strings... ]}
    accounts = []

    def _collect_emails(node):
        if isinstance(node, dict):
            for v in node.values():
                _collect_emails(v)
        elif isinstance(node, list):
            for v in node:
                _collect_emails(v)
        elif isinstance(node, str) and "@" in node and node not in accounts:
            accounts.append(node)

    _collect_emails(prefs.get("DVTDeveloperAccountManagerAppleIDLists"))

    # teams: IDEProvisioningTeamByIdentifier ->
    #   {account_identifier: [ {teamID/teamName/teamType/...}, ... ]}
    teams = {}
    for d in _walk(prefs.get("IDEProvisioningTeamByIdentifier")):
        tid = None
        # pass 1: a key that looks like a team-id field ('teamID', 'TeamId', ...)
        for k, v in d.items():
            if isinstance(v, str) and _TEAM_ID_RE.fullmatch(v) \
                    and "teamid" in k.lower().replace("_", ""):
                tid = v
                break
        # pass 2: any 10-char [A-Z0-9] value -- safe because we're scoped to
        # the IDEProvisioningTeamByIdentifier subtree only
        if tid is None:
            for v in d.values():
                if isinstance(v, str) and _TEAM_ID_RE.fullmatch(v):
                    tid = v
                    break
        if tid is None:
            continue
        name = ""
        for k, v in d.items():
            if isinstance(v, str) and v and "name" in k.lower():
                name = v
                break
        teams.setdefault(tid, name or tid)

    return {"ok": ok, "accounts": accounts, "teams": teams}


def _detect_cert_teams():
    """Team id == the OU field of your Apple Development certificate.
    Cert-derived only: certificates outlive the Xcode account (removing an
    account in Xcode does NOT remove its keychain certs), so this is the
    secondary source -- see detect_teams() for the merge."""
    found = {}
    for cn in ("Apple Development", "iPhone Developer",
               "Apple Distribution", "iPhone Distribution"):
        rc, out = sh(["security", "find-certificate", "-a", "-c", cn, "-p"])
        if rc != 0 or not out:
            continue
        for block in re.findall(
                r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----", out, re.S):
            rc2, subj = sh(["openssl", "x509", "-noout", "-subject"], inp=block)
            if rc2 != 0:
                continue
            mou = re.search(r"OU\s*=\s*([A-Z0-9]{10})", subj)
            mcn = re.search(r"CN\s*=\s*([^,/]+)", subj)
            if mou:
                found.setdefault(mou.group(1), (mcn.group(1).strip() if mcn else cn))
    return [{"team_id": k, "label": v} for k, v in found.items()]


def signing_identities(team=None):
    """Codesigning identities in the login keychain, newest expiry first.

    Each entry: {"sha1", "name", "team", "revoked", "expires"}. `team` filters
    by the certificate's OU, which IS the team id (the parenthesised part of the
    common name is not — it identifies the person).
    """
    rc, listing = sh(["security", "find-identity", "-v", "-p", "codesigning"])
    revoked, known = set(), {}
    for line in (listing or "").splitlines():
        m = re.search(r"\)\s+([0-9A-Fa-f]{40})\s+\"(.+?)\"", line)
        if not m:
            continue
        known[m.group(1).upper()] = m.group(2)
        if "CSSMERR" in line:                     # revoked / untrusted / expired
            revoked.add(m.group(1).upper())

    out = []
    for cn_prefix in ("Apple Development", "iPhone Developer",
                      "Apple Distribution", "iPhone Distribution"):
        rc, blob = sh(["security", "find-certificate", "-a", "-Z", "-c", cn_prefix, "-p"])
        if rc != 0 or not blob:
            continue
        for chunk in re.split(r"(?=SHA-1 hash:)", blob):
            mh = re.search(r"SHA-1 hash:\s*([0-9A-Fa-f]{40})", chunk)
            pem = re.search(r"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----",
                            chunk, re.S)
            if not (mh and pem):
                continue
            sha1 = mh.group(1).upper()
            if sha1 not in known:                 # cert without a private key
                continue
            rc2, subj = sh(["openssl", "x509", "-noout", "-subject", "-enddate"],
                           inp=pem.group(0))
            if rc2 != 0:
                continue
            mou = re.search(r"OU\s*=\s*([A-Z0-9]{10})", subj)
            end = re.search(r"notAfter=(.*)", subj)
            if team and (not mou or mou.group(1) != team):
                continue
            out.append({"sha1": sha1, "name": known[sha1],
                        "team": mou.group(1) if mou else None,
                        "revoked": sha1 in revoked,
                        "expires": (end.group(1).strip() if end else "")})

    def _when(e):
        try:
            return time.mktime(time.strptime(e["expires"], "%b %d %H:%M:%S %Y %Z"))
        except Exception:
            return 0.0
    out.sort(key=lambda e: (e["revoked"], -_when(e)))
    return out


def resolve_signing_identity(team):
    """(sha1, note) for `team` — sha1 is None when nothing usable was found.

    WHY THIS EXISTS: codesign matches identities BY NAME. Apple reissues a
    certificate under the SAME common name, and the old one stays in the
    keychain, so a perfectly ordinary Mac ends up with two identical names.
    Every `codesign --sign "<name>"` then dies with

        <name>: ambiguous (matches "<name>" and "<name>" in login.keychain-db)

    WebDriverAgent's own "Embed app icon into Runner.app" scheme post-action
    signs by name (Xcode does not export CODE_SIGN_* to post-actions, so it
    reads the name back off the signed bundle), which turns that keychain state
    into a build failure with no hint of the real cause. Resolving the name to a
    single SHA-1 up front and handing it to the script removes the ambiguity.
    """
    ids = signing_identities(team)
    if not ids:
        return None, ""
    usable = [i for i in ids if not i["revoked"]]
    if not usable:
        return None, ("every certificate for team %s is revoked or expired — "
                      "open Xcode > Settings > Accounts and let it issue a new one" % team)
    note = ""
    if len(ids) > 1:
        dupes = [i for i in ids if i["name"] == usable[0]["name"]]
        if len(dupes) > 1:
            note = ("%d certificates share the name %r (%d unusable) — pinning the valid "
                    "one so codesign cannot call it ambiguous"
                    % (len(dupes), usable[0]["name"], len(dupes) - len(usable)))
    return usable[0]["sha1"], note


def detect_teams(xc=None):
    """Merged team list for the UI. Teams from Xcode signed-in accounts are
    primary (signed_in=True); keychain-cert-only leftovers are flagged
    signed_in=False -- they can still sign with an existing valid profile but
    CANNOT create/renew free provisioning profiles (which expire every 7 days).
    signed_in=None means 'unknown' (Xcode prefs unreadable -- don't claim
    either way)."""
    if xc is None:
        xc = detect_xcode_accounts()
    cert_labels = {t["team_id"]: t["label"] for t in _detect_cert_teams()}
    out = []
    for tid, name in xc["teams"].items():
        out.append({"team_id": tid,
                    "label": name or cert_labels.get(tid, tid),
                    "signed_in": True, "source": "xcode"})
    for tid, label in cert_labels.items():
        if tid not in xc["teams"]:
            out.append({"team_id": tid, "label": label,
                        "signed_in": (False if xc["ok"] else None),
                        "source": "keychain"})
    out.sort(key=lambda t: t["signed_in"] is not True)   # signed-in first
    return out


def _devices_via_devicectl():
    """Physical iOS devices via CoreDevice (`xcrun devicectl`). This knows the *real*
    connection state and reports the hardware ECID udid -- exactly what
    `xcodebuild -destination id=` expects -- so what we list is what will actually build.
    Returns a list (possibly empty) of dicts with a `connected` flag, or None if
    devicectl isn't usable so the caller can fall back to xctrace."""
    tmp = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    tmp.close()
    try:
        rc, _ = sh(["xcrun", "devicectl", "list", "devices", "--json-output", tmp.name])
        if rc != 0:
            return None
        try:
            with open(tmp.name) as f:
                data = json.load(f)
        except (OSError, ValueError):
            return None
    finally:
        try:
            os.unlink(tmp.name)
        except OSError:
            pass

    devices = []
    for dev in data.get("result", {}).get("devices", []):
        hw = dev.get("hardwareProperties", {})
        dp = dev.get("deviceProperties", {})
        cp = dev.get("connectionProperties", {})
        if hw.get("platform") != "iOS":
            continue
        udid = hw.get("udid")
        if not udid:
            continue
        # transportType is "wired"/"wireless"/"localNetwork" when the phone is actually
        # reachable, and "None" (or missing) when it's only a remembered pairing.
        transport = cp.get("transportType")
        connected = bool(transport) and transport != "None"
        devices.append({
            "name": (dp.get("name") or "iOS device").strip(),
            "version": dp.get("osVersionNumber") or "",
            "udid": udid,
            "connected": connected,
            "source": "devicectl",
        })
    # connected first, so the UI's auto-select lands on a live device
    devices.sort(key=lambda d: not d["connected"])
    return devices


def _devices_via_xctrace():
    """Fallback for machines without devicectl. xctrace can't report connection state,
    so mark everything connected=True (optimistic); a run-time destination error will
    correct the user if the pick isn't actually reachable."""
    rc, out = sh(["xcrun", "xctrace", "list", "devices"])
    devices = []
    if rc != 0:
        return devices
    in_sims = False
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("== Simulators"):
            in_sims = True
            continue
        if s.startswith("=="):
            in_sims = False
            continue
        if in_sims or not s:
            continue
        # "Name (16.5) (00008120-000...)"  -> physical device (two paren groups)
        m = re.match(r"^(.*?)\s+\(([\d.]+)\)\s+\(([0-9A-Fa-f-]{8,})\)\s*$", s)
        if m:
            devices.append({
                "name": m.group(1).strip(),
                "version": m.group(2),
                "udid": m.group(3),
                "connected": True,
                "source": "xctrace",
            })
    return devices


def detect_devices():
    """Physical iOS devices for the picker. Prefer devicectl (knows real connection state
    and the hardware udid xcodebuild wants); fall back to xctrace if it isn't available."""
    devs = _devices_via_devicectl()
    if devs is not None:
        return devs
    return _devices_via_xctrace()


def _device_name(udid):
    """Friendly name for a udid (for the 'trust on <device>' message). Best effort."""
    for d in detect_devices():
        if d.get("udid") == udid:
            return d.get("name") or "your iPhone"
    return "your iPhone"


def _developer_label(team):
    """The certificate label iOS shows under Settings > VPN & Device Management, e.g.
    'Apple Development: you@example.com'. Must come from the certificate CN
    (not the merged team list, whose labels can be Xcode team names) because
    it has to match the string iOS displays verbatim."""
    for t in _detect_cert_teams():
        if t.get("team_id") == team:
            return t.get("label") or "your Apple Development certificate"
    return "your Apple Development certificate"


def installed_bundle_ids(udid=None):
    """Bundle ids of the apps installed on the device (pymobiledevice3
    `apps list`). Empty list on any failure — callers treat that as 'unknown',
    not 'not installed'."""
    pmd = pymobiledevice3_path()
    if not pmd:
        return []
    cmd = [pmd, "apps", "list"]
    if udid:
        cmd += ["--udid", udid]
    rc, out = sh(cmd)
    if rc != 0:
        return []
    try:
        start = out.find("{")
        if start < 0:
            return []
        return list(json.loads(out[start:]).keys())
    except Exception:
        return []


def kill_current():
    with PROC_LOCK:
        p = CURRENT_PROC
    if p and p.poll() is None:
        try:
            os.killpg(os.getpgid(p.pid), signal.SIGTERM)
        except Exception:
            pass
        time.sleep(0.5)
        if p.poll() is None:
            try:
                os.killpg(os.getpgid(p.pid), signal.SIGKILL)
            except Exception:
                pass


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # keep the console clean; the browser shows everything

    def end_headers(self):
        # The desktop app drives these endpoints from its own clean UI on a
        # different local port (fetch + EventSource), so allow cross-origin.
        # All routes are GET, so no preflight is involved.
        self.send_header("Access-Control-Allow-Origin", "*")
        super().end_headers()

    # ---- tiny response helpers ----
    def _json(self, obj, code=200):
        body = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _sse_open(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

    def _sse(self, event, data=""):
        """Write one SSE event. Returns False if the client is gone."""
        try:
            chunk = ""
            if event:
                chunk += f"event: {event}\n"
            for ln in str(data).split("\n"):
                chunk += f"data: {ln}\n"
            chunk += "\n"
            self.wfile.write(chunk.encode("utf-8"))
            self.wfile.flush()
            return True
        except (BrokenPipeError, ConnectionResetError, ValueError, OSError):
            return False

    # ---- stream a subprocess through a pty so output arrives line-by-line ----
    def _stream_cmd(self, cmd, cwd=None, env=None, detect_serverup=False, line_hook=None):
        global CURRENT_PROC
        master, slave = pty.openpty()
        try:
            proc = subprocess.Popen(
                cmd, cwd=cwd, env=env,
                stdin=slave, stdout=slave, stderr=slave,
                preexec_fn=os.setsid, close_fds=True,
            )
        except FileNotFoundError:
            os.close(master)
            os.close(slave)
            self._sse("log", f"!! not found: {cmd[0]}")
            return 127
        os.close(slave)
        with PROC_LOCK:
            CURRENT_PROC = proc

        buf = b""
        alive = True
        try:
            while True:
                try:
                    r, _, _ = select.select([master], [], [], 0.2)
                except (OSError, ValueError):
                    break
                if master in r:
                    try:
                        data = os.read(master, 4096)
                    except OSError:
                        data = b""
                    if not data:
                        break
                    buf += data
                    while b"\n" in buf:
                        raw, buf = buf.split(b"\n", 1)
                        line = raw.decode("utf-8", "replace").rstrip("\r")
                        if not self._sse("log", line):
                            alive = False
                            break
                        if detect_serverup:
                            m = SERVER_RE.search(line)
                            if m:
                                self._sse("serverup", json.dumps({"url": m.group(1)}))
                        if line_hook:
                            try:
                                line_hook(line)
                            except Exception:
                                pass
                    if not alive:
                        break
                elif proc.poll() is not None:
                    try:
                        data = os.read(master, 4096)
                    except OSError:
                        data = b""
                    if data:
                        buf += data
                        continue
                    break
        finally:
            if buf and alive:
                tail = buf.decode("utf-8", "replace").rstrip("\r\n")
                if tail:
                    self._sse("log", tail)
            try:
                os.close(master)
            except OSError:
                pass
            if not alive:
                kill_current()
            rc = proc.poll()
            if rc is None:
                try:
                    proc.wait(timeout=5)
                except Exception:
                    pass
                rc = proc.poll()
            with PROC_LOCK:
                CURRENT_PROC = None
        return rc if rc is not None else -1

    # ---- routes ----
    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        if path in ("/", "/index.html"):
            return self.route_index()
        if path == "/detect":
            return self.route_detect()
        if path == "/add-account":
            return self.route_add_account()
        if path == "/run":
            return self.route_run(parse_qs(parsed.query))
        if path == "/installed":
            # Is the AutoCua runner already on the device? (pairing = installed)
            q = parse_qs(parsed.query)
            udid = (q.get("udid", [""])[0] or "").strip() or None
            bundle = (q.get("bundle", [""])[0] or "").strip() \
                or (BUNDLE_PREFIX + ".WebDriverAgentRunner.xctrunner")
            ids = installed_bundle_ids(udid)
            return self._json({"installed": bundle in ids, "bundle": bundle,
                               "checked": bool(ids)})
        if path == "/stop":
            kill_current()
            return self._json({"stopped": True})
        self.send_error(404)

    def route_index(self):
        try:
            body = INDEX_HTML.read_bytes()
        except OSError:
            body = b"<h1>index.html not found</h1><p>Put index.html next to setup.py (or inside an html/ folder).</p>"
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def route_detect(self):
        # The UI opens by calling /detect, so this is where a pip install gets
        # its clone — the "project" chip below then reports the truth either way.
        ensure_project()
        rc_xc, out_xc = sh(["xcodebuild", "-version"])
        xcode = ""
        if rc_xc == 0 and out_xc.strip():
            xcode = out_xc.strip().splitlines()[0]
        rc_rb, _ = sh(["ruby", "-e", "require 'xcodeproj'"])
        xc = detect_xcode_accounts()
        self._json({
            "project": str(PROJECT),
            "project_ok": PROJECT.exists(),
            "xcode": xcode,
            "ruby_gem_ok": rc_rb == 0,
            "pymobiledevice3": bool(pymobiledevice3_path()),
            "teams": detect_teams(xc),
            "accounts": xc["accounts"],
            "accounts_ok": xc["ok"],
            "devices": detect_devices(),
            "default_prefix": BUNDLE_PREFIX,
            "targets": TARGET_NAMES,
            "default_mode": DEFAULT_MODE,
        })

    def route_add_account(self):
        """Open Xcode directly on Settings > Accounts with the add-account
        sheet up (best effort). There is no CLI/API/URL-scheme for this, but:
          1. the IDELastViewedSettingsPane pref steers the settings window to
             the Accounts pane before it opens,
          2. UI scripting (System Events) opens the window and presses "+".
        UI scripting needs a one-time Accessibility/Automation permission for
        the terminal running this tool; without it we still launch Xcode and
        the page falls back to manual steps. Typing the Apple ID password/2FA
        is the only part Apple insists the user does themselves."""
        sh(["defaults", "write", "com.apple.dt.Xcode",
            "IDELastViewedSettingsPane", "-string", "IDEKit.IDESettingsPane.Accounts"])
        rc, out = sh(["open", "-a", "Xcode"])
        if rc != 0:
            return self._json({"launched": False, "automated": False, "hint": "",
                               "detail": (out.strip() or "could not launch Xcode -- is it installed?")})
        try:
            p = subprocess.run(["osascript", "-"], input=XCODE_ACCOUNTS_SCRIPT,
                               capture_output=True, text=True, timeout=60)
            err = (p.stderr or "").strip() or (p.stdout or "").strip()
            automated = (p.returncode == 0 and "OK" in (p.stdout or ""))
        except Exception as e:
            err = str(e)
            automated = False
        hint = ""
        if not automated:
            low = err.lower()
            if ("assistive access" in low or "not authorized" in low
                    or "-25211" in low or "-1719" in low):
                hint = ("Tip: allow your terminal under System Settings > Privacy & Security > "
                        "Accessibility (and Automation), and this page will open itself next time.")
        return self._json({"launched": True, "automated": automated, "hint": hint,
                           "detail": ("opened Xcode Settings > Accounts" if automated else err)})

    def route_run(self, q):
        self._sse_open()

        team   = (q.get("team",   [""])[0] or "").strip()
        udid   = (q.get("udid",   [""])[0] or "").strip()
        prefix = (q.get("prefix", [BUNDLE_PREFIX])[0] or BUNDLE_PREFIX).strip()
        mode   = (q.get("mode",   [DEFAULT_MODE])[0] or DEFAULT_MODE).strip()
        mount  = (q.get("mount",  ["1"])[0] == "1")
        force  = (q.get("force",  ["0"])[0] == "1")

        self._sse("log", "starting")

        # --- preflight ---
        # Authoritative check — /detect may never have run.
        if not ensure_project():
            return self._sse("fatal", f"project not found: {PROJECT}")
        if not re.fullmatch(r"[A-Z0-9]{10}", team or ""):
            return self._sse("fatal", f"team id must be 10 chars A-Z0-9 (got '{team}')")
        if not udid:
            return self._sse("fatal", "no device UDID -- pick one or type it in")
        rc, _ = sh(["ruby", "-e", "require 'xcodeproj'"])
        if rc != 0:
            return self._sse("fatal", "ruby 'xcodeproj' gem missing.  run:  bash ios_setup.sh  "
                              "(or: gem install --user-install xcodeproj)")

        # --- preflight: is this team backed by an Apple Account signed into Xcode? ---
        # A keychain cert alone can sign with an EXISTING profile, but
        # -allowProvisioningUpdates cannot create/renew free profiles (7-day
        # expiry) without an account signed into Xcode. Emitted before any
        # pbxproj write, so refusing here has no side effects.
        xc = detect_xcode_accounts()
        if team not in xc["teams"]:
            if not xc["ok"]:
                self._sse("log", "warn: could not read Xcode account state -- continuing")
            elif xc["accounts"] and not xc["teams"]:
                self._sse("log", "warn: an Apple Account is signed in but Xcode hasn't cached its teams yet -- continuing")
            elif force:
                self._sse("log", f"warn: forced run -- team {team} has no signed-in Xcode account; "
                                 "profile renewal will fail if the current profile is expired")
            else:
                return self._sse("needaccount", json.dumps({
                    "team": team,
                    "accounts": xc["accounts"],
                }))

        # --- stage 1: signing ---
        targets_map = {n: f"{prefix}.{n}" for n in TARGET_NAMES}
        self._sse("log", "editing project.pbxproj (xcodeproj gem)")
        env = os.environ.copy()
        env["PROJECT_PATH"]      = str(PROJECT)
        env["TEAM_ID"]           = team
        env["WDA_TARGETS_JSON"]  = json.dumps(targets_map)
        rb = tempfile.NamedTemporaryFile("w", suffix=".rb", delete=False)
        rb.write(RUBY_SCRIPT)
        rb.close()
        code = self._stream_cmd(["ruby", rb.name], env=env)
        try:
            os.unlink(rb.name)
        except OSError:
            pass
        if code != 0:
            return self._sse("done", json.dumps({"code": code, "stage": "signing"}))

        # --- stage 2: mount developer disk image (best effort) ---
        if mount:
            pmd3 = pymobiledevice3_path()
            if pmd3:
                self._sse("log", "mounting developer disk image (pymobiledevice3)")
                self._stream_cmd([pmd3, "mounter", "auto-mount"])
            else:
                self._sse("log", "skipping DDI mount (pymobiledevice3 not installed)")

        # --- stage 3: xcodebuild ---
        action = "test" if mode == "test" else "build-for-testing"
        cmd = [
            "xcodebuild",
            "-project", str(PROJECT),
            "-scheme", SCHEME,
            "-destination", f"id={udid}",
            "-allowProvisioningUpdates",
            "-derivedDataPath", str(DERIVED),
            action,
        ]
        self._sse("log", "xcodebuild " + action)
        self._sse("log", "  " + " ".join(cmd))
        if mode == "test":
            self._sse("log", "  test mode stays running once the server is up -- that is success. Stop to end.")

        # Watch xcodebuild output for the two "first run" snags and translate them into
        # plain-English, actionable guidance instead of a raw dump:
        #   (a) the picked device isn't a valid destination (wrong / offline udid)
        #   (b) the app installed fine but the developer isn't trusted on the phone yet
        dest_err  = {"hit": False, "avail": []}
        trust_err = {"hit": False}
        stale_id  = {"hit": False}
        trust_re = re.compile(
            r"explicitly trusted|VPN & Device Management|Developer App certificate|"
            r"untrusted developer|has not been trusted|trust the developer",
            re.I)

        def _watch(line):
            if "Unable to find a destination matching" in line:
                dest_err["hit"] = True
            elif dest_err["hit"]:
                m = re.search(
                    r"platform:iOS,\s*arch:\w+,\s*id:([0-9A-Fa-f-]{8,}),\s*name:(.+?)\s*}",
                    line)
                if m:
                    dest_err["avail"].append(f"{m.group(2).strip()} ({m.group(1)})")
            if trust_re.search(line):
                trust_err["hit"] = True
            if "0xe8008018" in line:        # "The identity used to sign the executable is no longer valid."
                stale_id["hit"] = True

        # Hand the post-action an UNAMBIGUOUS identity. WebDriverAgent's icon
        # script falls back to signing by name when Xcode does not export one,
        # and a keychain holding a reissued certificate has two of those names
        # — see resolve_signing_identity. Xcode's own signing is untouched:
        # this variable only fills in the blank the post-action reads.
        benv = dict(os.environ)
        sha1, note = resolve_signing_identity(team)
        if note:
            self._sse("log", "  " + note)
        if sha1:
            benv["EXPANDED_CODE_SIGN_IDENTITY"] = sha1

        code = self._stream_cmd(cmd, env=benv, detect_serverup=(mode == "test"),
                                line_hook=_watch)

        # -allowProvisioningUpdates can issue a NEW certificate during this very
        # build, and Apple then revokes the old one. The identity pinned above is
        # then stale, the icon post-action re-signs with a revoked certificate and
        # the phone refuses the install (0xe8008018). Resolve again, build once more.
        if stale_id["hit"]:
            sha1_now, _ = resolve_signing_identity(team)
            if sha1_now and sha1_now != sha1:
                self._sse("log", "  signing certificate changed during the build -- building again with the new one")
                benv["EXPANDED_CODE_SIGN_IDENTITY"] = sha1_now
                code = self._stream_cmd(cmd, env=benv, detect_serverup=(mode == "test"),
                                        line_hook=_watch)

        if dest_err["hit"]:
            names = ", ".join(dest_err["avail"]) or "no iOS device is currently connected"
            return self._sse(
                "fatal",
                f"device not connected/ready (udid {udid}). "
                f"Xcode can currently build to: {names}. "
                f"Plug the phone in with a cable, unlock it, enable Developer Mode "
                f"(Settings > Privacy & Security > Developer Mode), then pick it and hit Connect again."
            )

        if trust_err["hit"]:
            # The app is now installed on the phone, so the "Trust" option is available in
            # Settings. Apple requires the user to tap it by hand -- there's no API to do
            # it remotely. We surface exactly what to tap; the retry ("I've trusted it")
            # then launches. On every later run the cert stays trusted, so this is silent.
            return self._sse("needtrust", json.dumps({
                "device": _device_name(udid),
                "developer": _developer_label(team),
                "udid": udid,
            }))

        self._sse("done", json.dumps({"code": code, "stage": "xcodebuild", "mode": mode}))


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    print(f"WDA setup   ->  http://localhost:{PORT}")
    print(f"project     ->  {PROJECT}   exists={PROJECT.exists()}")
    if not PROJECT.exists():
        # WebDriverAgent is not vendored in this repo — ios_setup.sh clones it
        # from the Appium project at a pinned tag. See THIRD_PARTY_NOTICES.md.
        # A pip install has no shell script to run, so the first /detect or
        # build below fetches it instead.
        print(f"  (no WebDriverAgent clone at {WDA_DIR} — fetching it on first use)")
        print("   or run:  bash ios_setup.sh   # fetches it and checks the toolchain")
    srv = Server(("127.0.0.1", PORT), Handler)
    # When the desktop app embeds this UI in an iframe (AutoCua_EMBED=1) we don't
    # want a stray browser tab — the app loads http://localhost:PORT itself.
    if not os.environ.get("AutoCua_EMBED"):
        try:
            webbrowser.open(f"http://localhost:{PORT}")
        except Exception:
            pass
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nshutting down")
    finally:
        kill_current()
        srv.shutdown()


if __name__ == "__main__":
    main()
