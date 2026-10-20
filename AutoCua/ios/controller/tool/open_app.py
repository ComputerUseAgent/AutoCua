#!/usr/bin/env python3

"""
open_app.py - iPhone App Launcher Service
Handles app launching and navigation on iPhone via WebDriverAgent
"""

import difflib
import json
import os
import subprocess
import time
import requests
import logging

logger = logging.getLogger(__name__)


class AppLauncherService:
    """Service for launching and managing iPhone apps"""
    
    def __init__(self):
        from AutoCua.ios_connector.session import wda_url
        self.wda_url = wda_url()
        self.session_id = None
        self.apps_dict = {}
        
    def scan_apps(self):
        """Scan all installed apps using pymobiledevice3 (same CLI the WDA session uses)"""
        try:
            from AutoCua.ios_connector.session import _pmd3_base, active_target

            # Simulator runs have no USB device — list apps via simctl instead.
            if active_target.get("kind") == "simulation":
                return self._scan_apps_simulator(active_target.get("udid") or "booted")
            base = _pmd3_base()
            if not base:
                logger.error("❌ Error: pymobiledevice3 not found.")
                logger.error("💡 Install it with: pip install pymobiledevice3")
                return False

            logger.info("📱 Scanning installed apps...")
            result = subprocess.run(
                base + ['apps', 'list'],
                capture_output=True,
                text=True,
                timeout=30,
                env={**os.environ, "NO_COLOR": "1"},
            )
            if result.returncode != 0:
                tail = (result.stderr or '').strip().splitlines()
                logger.error(f"❌ App scan failed: {tail[-1] if tail else 'pymobiledevice3 error'}")
                return False

            # Parse the apps
            self.parse_apps(result.stdout)
            logger.info(f"✅ Found {len(self.apps_dict)} apps")
            return True

        except FileNotFoundError:
            logger.error("❌ Error: pymobiledevice3 not found.")
            logger.error("💡 Install it with: pip install pymobiledevice3")
            return False
        except subprocess.TimeoutExpired:
            logger.error("❌ Timeout while scanning apps")
            return False
        except Exception as e:
            logger.error(f"❌ Error scanning apps: {e}")
            return False

    def _scan_apps_simulator(self, udid):
        """`simctl listapps` prints an old-style plist; plutil turns it into the
        same bundle-id-keyed JSON shape `pymobiledevice3 apps list` gives, so
        parse_apps handles both."""
        try:
            logger.info("📱 Scanning installed apps (simulator)...")
            listed = subprocess.run(["xcrun", "simctl", "listapps", udid],
                                    capture_output=True, text=True, timeout=30)
            if listed.returncode != 0:
                tail = (listed.stderr or '').strip().splitlines()
                logger.error(f"❌ App scan failed: {tail[-1] if tail else 'simctl error'}")
                return False
            as_json = subprocess.run(["plutil", "-convert", "json", "-o", "-", "--", "-"],
                                     input=listed.stdout,
                                     capture_output=True, text=True, timeout=15)
            if as_json.returncode != 0:
                logger.error("❌ App scan failed: could not parse simctl output")
                return False
            self.parse_apps(as_json.stdout)
            logger.info(f"✅ Found {len(self.apps_dict)} apps")
            return True
        except subprocess.TimeoutExpired:
            logger.error("❌ Timeout while scanning apps")
            return False
        except Exception as e:
            logger.error(f"❌ Error scanning apps: {e}")
            return False

    def parse_apps(self, apps_output):
        """Parse `pymobiledevice3 apps list` JSON into searchable dictionary"""
        self.apps_dict = {}

        for bundle_id, info in json.loads(apps_output).items():
            # Hidden bundles (SafariViewService, MessagesViewService, CarPlay
            # settings, ...) have no home-screen icon, share a display name
            # with the real app, and never show a screen when launched. The
            # model can't see them, so it can never mean them.
            if "hidden" in (info.get('SBAppTags') or []):
                continue
            display_name = info.get('CFBundleDisplayName') or info.get('CFBundleName') or bundle_id
            version = info.get('CFBundleShortVersionString') or str(info.get('CFBundleVersion', ''))

            # Keyed by bundle id: two bundles with the same display name (a dev
            # build and prod, an app and its App Clip) must both survive so
            # resolve_app can report the name as ambiguous instead of silently
            # launching whichever the scan listed last.
            self.apps_dict[bundle_id] = {
                'bundle_id': bundle_id,
                'display_name': display_name,
                'version': version
            }
    
    # Match quality, best first. An exact display-name match must beat an
    # accidental substring hit: "now" is inside "NanoWebSheet" (a hidden Apple
    # system sheet), and dictionary order used to make THAT the launch target
    # instead of the app literally named "NOW".
    _EXACT, _PREFIX, _IN_NAME, _IN_BUNDLE = 4, 3, 2, 1

    @staticmethod
    def _norm(text):
        return str(text or '').lower().strip().replace(' ', '')

    def _score(self, app_info, search_term):
        """Match tier of one app for a name (0 = no match). Single source of
        truth for search_app and resolve_app. An exact bundle id ranks with an
        exact display name so 'com.foo.app' picks prod even when
        'com.foo.app.dev' exists with the same display name."""
        term = self._norm(search_term)
        name = self._norm(app_info['display_name'])
        bundle = app_info['bundle_id'].lower()
        if name == term or bundle == term:
            return self._EXACT
        if name.startswith(term):
            return self._PREFIX
        if term in name:
            return self._IN_NAME
        if term in bundle:
            return self._IN_BUNDLE
        return 0

    def search_app(self, search_term):
        """Rank installed apps against a name: exact display name / bundle id,
        then name prefix, then substring of the name, then substring of the
        bundle id. Returns app_info dicts best-first (ties: shorter name first)."""
        term = self._norm(search_term)
        if not term:
            return []
        ranked = []
        for app_info in self.apps_dict.values():
            score = self._score(app_info, term)
            if score:
                ranked.append((score, len(self._norm(app_info['display_name'])), app_info))
        ranked.sort(key=lambda r: (-r[0], r[1]))
        return [info for _, _, info in ranked]

    def resolve_app(self, search_term):
        """Pick the ONE app a name refers to, or explain why it can't.
        Returns (app_info, None) on a confident match, (None, message) when the
        name is empty, unknown, or ambiguous - the message names the closest
        installed apps so the caller can re-issue with an exact name instead
        of the launcher guessing."""
        term = str(search_term or '').strip()
        if not term:
            return None, "open_app needs an app name - re-issue with the exact name shown on the home screen."
        if not self.apps_dict and (not self.scan_apps() or not self.apps_dict):
            # Infrastructure failure, NOT "not installed" - never steer the
            # model into the App Store for an app that is on the home screen.
            return None, ("Could not read the installed app list from the device - "
                          "open the app from its icon on the home screen instead.")
        ranked = self.search_app(term)
        if not ranked and self.scan_apps():
            # The list is cached per process; the app may have been installed
            # since (the model was told to download it). One rescan, then decide.
            ranked = self.search_app(term)
        if not ranked:
            norm = term.lower().replace(' ', '')
            by_norm = {i['display_name'].lower().replace(' ', ''): i['display_name']
                       for i in self.apps_dict.values()}
            close = [by_norm[k] for k in difflib.get_close_matches(norm, list(by_norm), n=3, cutoff=0.6)]
            hint = f" Closest installed: {', '.join(close)}." if close else ""
            return None, (f"No installed app matches '{term}'.{hint} Verify the name from the "
                          f"home screen; if it is not installed, download it or find an alternative.")
        best = ranked[0]
        best_score = self._score(best, term)
        peers = [i for i in ranked if self._score(i, term) == best_score]
        # A unique best at any tier is decisive (an exact name beats weaker
        # peers by construction). Several equally-good matches: the model
        # chooses, not the launcher. Same-name bundles are told apart by id.
        if len(peers) == 1:
            return best, None
        if best_score == self._EXACT:
            options = ', '.join(f"{i['display_name']} ({i['bundle_id']})" for i in peers[:3])
            return None, (f"'{term}' names {len(peers)} installed apps: {options}. "
                          f"Re-issue open_app with the bundle id.")
        options = ', '.join(i['display_name'] for i in peers[:3])
        return None, (f"'{term}' is ambiguous - it matches: {options}. "
                      f"Re-issue open_app with the exact app name.")

    def get_session(self):
        """Get or create WDA session"""
        try:
            # Try to create new session
            response = requests.post(
                f"{self.wda_url}/session",
                json={"capabilities": {"alwaysMatch": {}}},
                timeout=10
            )
            if response.status_code == 200:
                data = response.json()
                if 'sessionId' in data:
                    self.session_id = data['sessionId']
                elif 'value' in data and 'sessionId' in data['value']:
                    self.session_id = data['value']['sessionId']
                return self.session_id
        except Exception as e:
            logger.error(f"Session error: {e}")
        return None
    
    def launch_app(self, app_name):
        """Resolve a name to ONE installed app, launch it, and confirm it is
        the foreground app. Returns a dict the controller reports verbatim:
          {"ok": True,  "display_name", "bundle_id", "message"}
          {"ok": False, "display_name"?, "bundle_id"?, "message"}
        `ok` is True ONLY when the target is confirmed in the foreground (or
        the foreground could not be read - then `verified` is False)."""
        app_info, why = self.resolve_app(app_name)
        if app_info is None:
            logger.error(why)
            return {"ok": False, "message": why}
        result = self._do_launch(app_info['bundle_id'], app_info['display_name'])
        result.update({"display_name": app_info['display_name'],
                       "bundle_id": app_info['bundle_id']})
        return result

    def active_app(self):
        """Foreground app via WDA's /wda/activeAppInfo -> {"bundleId", "pid",
        "name"} or None when the endpoint can't be read."""
        try:
            response = requests.get(f"{self.wda_url}/wda/activeAppInfo", timeout=5)
            if response.status_code == 200:
                value = response.json().get("value") or {}
                if isinstance(value, dict) and value.get("bundleId"):
                    return value
        except Exception as e:
            logger.error(f"activeAppInfo failed: {e}")
        return None

    # XCUIApplicationState, as returned by WDA's /wda/apps/state.
    _STATE = {0: "in an unknown state", 1: "not running", 2: "suspended in the background",
              3: "running in the background", 4: "in the foreground"}
    _FOREGROUND = 4

    def app_state(self, session, bundle_id):
        """POST /session/{s}/wda/apps/state -> XCUIApplicationState int, or
        None when it can't be read. State 4 stays 4 while a system alert
        (notifications / Face ID / tracking prompt) is overlaid on the app,
        which is exactly where activeAppInfo reports SpringBoard instead."""
        try:
            response = requests.post(f"{self.wda_url}/session/{session}/wda/apps/state",
                                     json={"bundleId": bundle_id}, timeout=5)
            if response.status_code == 200:
                value = response.json().get("value")
                if isinstance(value, int) and not isinstance(value, bool):
                    return value
        except Exception as e:
            logger.error(f"apps/state failed: {e}")
        return None

    def _do_launch(self, bundle_id, app_name, settle_seconds=3.0):
        """Launch via WDA, then VERIFY. A 200 from the launch endpoint only
        means WDA accepted the request - it also returns 200 for background-
        only system bundles that never show a screen. Success = the target's
        process state is running-foreground (primary), or it is the active
        app (fallback when apps/state can't be read), within `settle_seconds`."""
        logger.info(f"🚀 Launching {app_name}...")

        session = self.get_session()
        if not session:
            logger.error("Failed to create WDA session")
            return {"ok": False, "message": "Could not open a WDA session to launch the app."}

        before = self.active_app()
        before_bundle = before.get("bundleId") if before else None

        endpoints = [
            f"{self.wda_url}/wda/apps/launch",
            f"{self.wda_url}/session/{session}/wda/apps/launch",
            f"{self.wda_url}/wda/apps/activate",
            f"{self.wda_url}/session/{session}/wda/apps/activate",
        ]
        # Don't wait for the app to go idle: a splash animation would hold the
        # request past the client timeout while the app is already on screen.
        payload = {"bundleId": bundle_id, "shouldWaitForQuiescence": False}
        accepted = timed_out = False
        for endpoint in endpoints:
            try:
                response = requests.post(endpoint, json=payload, timeout=10)
                if response.status_code == 200:
                    accepted = True
                    break
            except requests.Timeout:
                # The launch is still in flight inside WDA; further endpoints
                # would only queue behind it. Let the verification poll decide.
                timed_out = True
                break
            except Exception:
                continue
        if not accepted and not timed_out:
            logger.error(f"Failed to launch {app_name}")
            return {"ok": False, "message": f"WDA could not launch {app_name} ({bundle_id})."}
        if timed_out:
            settle_seconds = max(settle_seconds, 10.0)

        deadline = time.monotonic() + settle_seconds
        state = fg = None          # last successful readings, not last attempts
        while True:
            s = self.app_state(session, bundle_id)
            if s is not None:
                state = s
            if state == self._FOREGROUND:
                logger.info(f"✅ {app_name} is in the foreground")
                return {"ok": True, "verified": True, "message": f"{app_name} is open in the foreground."}
            active = self.active_app()
            if active:
                fg = active.get("bundleId")
            if state is None and fg == bundle_id:
                logger.info(f"✅ {app_name} is the active app")
                return {"ok": True, "verified": True, "message": f"{app_name} is open in the foreground."}
            if time.monotonic() >= deadline:
                break
            time.sleep(0.5)

        if state is not None:
            logger.error(f"Launch of {app_name} accepted but its state is {state}")
            return {"ok": False,
                    "message": f"{app_name} ({bundle_id}) did not come to the foreground - it is "
                               f"{self._STATE.get(state, 'in an unknown state')}. Tap its icon on the home screen instead."}
        if fg is None:
            logger.warning(f"Launch of {app_name} accepted but could not be verified")
            return {"ok": True, "verified": False,
                    "message": f"WDA accepted the launch of {app_name}, but its state could not be read - confirm on the next screenshot."}
        if fg != before_bundle:
            # Something came in front that isn't the target - typically a
            # system dialog over the freshly launched app. Not a failure.
            logger.warning(f"Launch of {app_name} accepted; foreground is now {fg}")
            return {"ok": True, "verified": False,
                    "message": f"Launch of {app_name} accepted; the foreground is now {fg} - a system dialog may be in front. Confirm on the next screenshot."}
        logger.error(f"Launch of {app_name} accepted but foreground is unchanged ({fg})")
        return {"ok": False,
                "message": f"Launch of {app_name} ({bundle_id}) was accepted but nothing came to the foreground - active app is still {fg}. Tap its icon on the home screen instead."}
    
    def go_home(self):
        """Go to home screen"""
        try:
            response = requests.post(f"{self.wda_url}/wda/homescreen", timeout=5)
            if response.status_code == 200:
                logger.info("Navigated to home screen")
                return True
                
            if self.session_id:
                response = requests.post(
                    f"{self.wda_url}/session/{self.session_id}/wda/pressButton",
                    json={"name": "home"},
                    timeout=5
                )
                if response.status_code == 200:
                    logger.info("Navigated to home screen")
                    return True
        except Exception as e:
            logger.error(f"Go home failed: {e}")
            
        return False
    
    def test_wda_connection(self):
        """Test connection to WDA"""
        try:
            response = requests.get(f"{self.wda_url}/status", timeout=5)
            if response.status_code == 200:
                return True
            return False
        except:
            return False


# Create global instance
app_launcher_service = AppLauncherService()