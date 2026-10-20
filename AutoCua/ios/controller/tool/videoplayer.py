"""
videoplayer.py - Full-screen video playback control via WebDriverAgent.

DRM-protected players black out screenshots, so the agent cannot see the
video surface. This service works from the accessibility source instead.

Facts established live against Sky Go on an iPhone (26.6.1), which shape
everything below:

  * The transport buttons stay in the accessibility tree - with visible=true -
    even while the overlay is VISUALLY hidden. Tree presence proves nothing
    about what is on screen.
  * Coordinate taps at a button's own centre are eaten by the player's
    screen-covering gesture layer ("Double tap to show controls") and merely
    toggle the overlay. That layer is exactly why the old tap-at-(x,y) code
    "tapped the screen, showed the icons, and never played or paused".
  * WDA ELEMENT clicks actuate a control only when it is really shown;
    clicking the gesture-layer element is the reliable way to reveal the
    overlay. Reveal + element-click is the sequence that actually paused and
    played the content on the device.
  * The transport button's NAME tracks true playback state: the tree holds
    pauseButton while playing and playButton while paused, overlay hidden or
    not. That is the state check. The progress label ("You've watched N
    Minutes M Seconds") advancing is the ground truth for the streaming check.
"""

import requests
import time
import logging
import re
import xml.etree.ElementTree as ET

logger = logging.getLogger(__name__)

# (connect, read) seconds for the WebDriverAgent source dump — same bound the
# scanner uses at AutoCua/ios/tree/element.py:57. Without it `requests` blocks
# forever, and a wedged WDA takes the whole agent down with it.
WDA_SOURCE_TIMEOUT = (5, 60)

# After clicking a transport button: how long before reading the state back.
# The name flip (pauseButton <-> playButton) is immediate; 1s absorbs WDA lag.
CLICK_SETTLE_SECONDS = 1.0

# After revealing the overlay: the fade-in animation, measured working at 0.7s.
REVEAL_SETTLE_SECONDS = 0.7

# Seconds between the two progress-label samples that prove playback for the
# streaming check. The label ticks every second; 4s is comfortably past any
# rounding.
PROGRESS_SAMPLE_SECONDS = 4

# WDA predicate strings (NSPredicate syntax). Sky Go names first, the legacy
# Player_* names kept for the players the old code supported, plus the plain
# words. Exact == on the words, so AirPlay can never match "Play".
PLAY_PREDICATE = ('type == "XCUIElementTypeButton" AND '
                  '(name == "playButton" OR name == "Player_Play_Button" OR '
                  'label == "Play" OR name == "Play")')
PAUSE_PREDICATE = ('type == "XCUIElementTypeButton" AND '
                   '(name == "pauseButton" OR name == "Player_Pause_Button" OR '
                   'label == "Pause" OR name == "Pause")')
# The screen-covering gesture layer that toggles the overlay. Matched loosely:
# its label reads "Double tap to show controls" on Sky Go.
TOGGLE_PREDICATE = ('label CONTAINS[c] "show controls" OR '
                    'name CONTAINS[c] "show controls"')
# Tried in order: exact trusted names first, then a loose substring pass.
# "caption" is excluded from the loose pass - "Closed Captions" contains
# "close", and closing the player must never toggle subtitles instead.
CLOSE_PREDICATES = [
    ('type == "XCUIElementTypeButton" AND '
     '(name == "closeButton" OR name == "Player_Close_Button" OR '
     'label == "Close player" OR label == "Close" OR name == "Close")'),
    ('type == "XCUIElementTypeButton" AND '
     '(label CONTAINS[c] "close" OR name CONTAINS[c] "close" OR '
     'label CONTAINS[c] "dismiss") AND '
     'NOT (label CONTAINS[c] "caption" OR name CONTAINS[c] "caption")'),
]

# Button names/labels that reveal the playback state through the tree.
_PAUSE_IDENTS = {'pausebutton', 'player_pause_button', 'pause'}
_PLAY_IDENTS = {'playbutton', 'player_play_button', 'play'}


class VideoPlayerService:
    """Controls the full-screen video player through the controller session"""

    def __init__(self, controller_service):
        # Shares the WDA endpoint + session handling with the controller service
        self.controller_service = controller_service

    @property
    def wda_url(self):
        return self.controller_service.wda_url

    def get_session(self):
        return self.controller_service.get_session()

    # ------------------------------------------------------------------ WDA

    def _find(self, session, predicate):
        """First element matching a WDA predicate string, or None.

        The caller supplies the session, and the matching _click MUST reuse
        the same one: element ids live in a per-session cache, and the
        controller's get_session() mints a NEW session on every call - a
        click through a fresh session would be handed an id that session
        has never seen."""
        try:
            response = requests.post(
                f"{self.wda_url}/session/{session}/element",
                json={"using": "predicate string", "value": predicate},
                timeout=10
            )
            # Not-found is a NON-200 whose value is {"error": "no such
            # element", "message": ...} - a dict of strings. Returning any
            # string out of that dict hands back "no such element" as an
            # element id, which made every find look successful and every
            # screen look like a player. Success responses only.
            if response.status_code != 200:
                return None
            value = response.json().get('value')
            if isinstance(value, dict) and not value.get('error'):
                for element_id in value.values():
                    if isinstance(element_id, str) and element_id:
                        return element_id
            return None
        except Exception as e:
            logger.error(f"❌ Element find error: {e}")
            return None

    def _click(self, session, element_id):
        """Click an element BY ID - with the SAME session that found it.
        Never replace this with a coordinate tap: taps at the same point are
        swallowed by the player's gesture layer."""
        try:
            response = requests.post(
                f"{self.wda_url}/session/{session}/element/{element_id}/click",
                json={},
                timeout=10
            )
            if response.status_code != 200:
                return False
            value = response.json().get('value')
            return not (isinstance(value, dict) and value.get('error'))
        except Exception as e:
            logger.error(f"❌ Element click error: {e}")
            return False

    def _reveal_controls(self, allow_blind_tap=False):
        """Bring the overlay up by clicking the player's own toggle element;
        fall back to a corner-area tap for players without one.

        The raw tap is gated on the CALLER having seen player evidence (a
        transport button or progress label in the tree): if play/pause was
        invoked on a non-player screen, firing coordinate taps would press
        random UI on whatever screen is actually up."""
        session = self.get_session()
        if not session:
            logger.error("❌ Failed to get session")
            return
        toggle = self._find(session, TOGGLE_PREDICATE)
        if toggle and self._click(session, toggle):
            logger.debug("🎬 Revealed controls via toggle element")
            return
        if not allow_blind_tap:
            logger.debug("🎬 No toggle element and no player evidence - not tapping blind")
            return
        # Fallback: raw tap on the left of the screen (pre-toggle players)
        try:
            requests.post(
                f"{self.wda_url}/session/{session}/actions",
                json={
                    "actions": [{
                        "type": "pointer",
                        "id": "finger1",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type": "pointerMove", "duration": 0, "x": 100, "y": 195},
                            {"type": "pointerDown", "button": 0},
                            {"type": "pause", "duration": 10},
                            {"type": "pointerUp", "button": 0}
                        ]
                    }]
                },
                timeout=5
            )
        except Exception:
            pass

    # ---------------------------------------------------------------- state

    def _scan_state(self):
        """Read playback state from the source tree.

        Returns {'playing': True|False|None, 'progress': str|None} or None if
        the dump failed. 'playing' comes from WHICH transport button the tree
        holds - reliable regardless of overlay visibility. 'progress' is an
        opaque advancing string ("You've watched ..." or "m:ss / mm:ss")."""
        try:
            response = requests.get(f"{self.wda_url}/source", timeout=WDA_SOURCE_TIMEOUT)
            if response.status_code != 200:
                logger.error(f"❌ Failed to get source: {response.status_code}")
                return None

            root = ET.fromstring(response.json()['value'])
            state = {'playing': None, 'progress': None, 'player_ui': False}

            for elem in root.iter():
                elem_type = elem.get('type', '')
                label = elem.get('label', '') or ''
                name = elem.get('name', '') or ''
                value = elem.get('value', '') or ''

                # Container-level player markers. During pre-roll ADS the
                # player has no transport buttons and no watched-label at all
                # - playerView/advertisingOverlay (and the exact player close
                # button) are what prove a player is on screen then.
                if name in ('playerView', 'advertisingOverlay'):
                    state['player_ui'] = True
                if elem_type == 'XCUIElementTypeButton' and (
                        name == 'closeButton' or name == 'Player_Close_Button'
                        or label == 'Close player'):
                    state['player_ui'] = True

                if elem_type == 'XCUIElementTypeButton':
                    idents = {label.lower(), name.lower()}
                    if idents & _PAUSE_IDENTS:
                        state['playing'] = True     # pause offered = playing
                    elif idents & _PLAY_IDENTS:
                        state['playing'] = False    # play offered = paused

                elif elem_type == 'XCUIElementTypeStaticText':
                    if name == 'Player_Progress_Label' and '/' in label:
                        time_match = re.search(r'(\d{1,2}:\d{2}(?::\d{2})?)',
                                               label.split('/')[0])
                        if time_match:
                            state['progress'] = time_match.group(1)
                    elif not state['progress']:
                        # Sky Go player: timeLabel "You've watched 15 Minutes
                        # 51 Seconds". Anchored on "you've watched" (loose
                        # apostrophe - straight vs curly varies), because the
                        # SHOW page has its own "Watched 19 Minutes" label:
                        # matching bare "watched" made close() believe the
                        # player was still open after it had really closed.
                        # "Running time ..." (static total) never matches.
                        text = (label or value or '').strip()
                        low = text.lower()
                        if re.search(r"you\W{0,2}ve watched|you have watched", low):
                            state['progress'] = text

            logger.debug(f"🎬 Player state: playing={state['playing']}, "
                         f"progress={state['progress']!r}")
            return state

        except Exception as e:
            logger.error(f"❌ Video scan error: {e}")
            return None

    # -------------------------------------------------------------- actions

    def _set_playback(self, want_playing):
        """Shared play/pause driver, symmetric by design.

        Check the tree's transport-button state; if already right, done. Else
        element-click the wanted button and read the state back. A click that
        changed nothing means the overlay was visually hidden and the gesture
        layer ate it - reveal via the toggle element and retry ONCE."""
        verb = "play" if want_playing else "pause"
        predicate = PLAY_PREDICATE if want_playing else PAUSE_PREDICATE

        for attempt in range(2):  # full pass + one retry
            state = self._scan_state()
            if state and state['playing'] is want_playing:
                logger.info(f"✅ {verb}: confirmed")
                return True

            # Up to two clicks on the transport element, verifying between.
            # This is parity-proof against the invisible overlay state: on a
            # VISIBLE overlay click 1 actuates; on a hidden one the gesture
            # layer eats click 1 - which itself reveals the overlay - and
            # click 2 actuates. Never insert a toggle-click before this: the
            # eaten click already toggled, and toggling again just re-hides
            # the controls (measured live - that ordering fails every time).
            for click_round in range(2):
                session = self.get_session()  # one session per round,
                if not session:               # shared by find AND click
                    break
                element_id = self._find(session, predicate)  # re-find: ids go stale
                if not element_id or not self._click(session, element_id):
                    break
                logger.info(f"🎬 Clicked {verb} button (round {click_round + 1})")
                time.sleep(CLICK_SETTLE_SECONDS)
                state = self._scan_state()
                if state and state['playing'] is want_playing:
                    logger.info(f"✅ {verb}: confirmed")
                    return True

            # Transport element missing or clicks refused: reveal and go
            # around once more. Blind coordinate taps only when the tree
            # proved a player is up.
            logger.debug(f"🎬 {verb} not confirmed, revealing controls")
            player_seen = bool(state and (state['playing'] is not None
                                          or state['progress']))
            self._reveal_controls(allow_blind_tap=player_seen)
            time.sleep(REVEAL_SETTLE_SECONDS)

        logger.error(f"❌ Failed to {verb}")
        return False

    def play(self):
        """Play the video"""
        try:
            return self._set_playback(True)
        except Exception as e:
            logger.error(f"Video play error: {e}")
            return False

    def pause(self):
        """Pause the video"""
        try:
            return self._set_playback(False)
        except Exception as e:
            logger.error(f"Video pause error: {e}")
            return False

    def check_streaming(self):
        """Check if video is currently streaming/playing.

        Ground truth is the progress label advancing between two samples;
        transport-button state is the fallback for players without one."""
        try:
            state1 = self._scan_state()
            if not state1:
                return False
            if not state1['progress']:
                return state1['playing'] is True

            time.sleep(PROGRESS_SAMPLE_SECONDS)

            state2 = self._scan_state()
            if not state2 or not state2['progress']:
                return bool(state2) and state2['playing'] is True
            return state1['progress'] != state2['progress']

        except Exception as e:
            logger.error(f"Video streaming check error: {e}")
            return False

    def _player_present(self):
        """Is the full-screen player up at all? Three-valued like _is_playing:

        True  - a player marker is in the tree (transport button, progress
                label, playerView / ad overlay / player close button, or the
                overlay-toggle element)
        False - the tree scanned fine and holds no player markers
        None  - the scan failed, so no verdict either way

        Every marker vanishes once the player screen is dismissed, which is
        what makes this the close() verification."""
        state = self._scan_state()
        if state is None:
            return None
        if state['playing'] is not None or state['progress'] or state['player_ui']:
            return True
        session = self.get_session()
        if session and self._find(session, TOGGLE_PREDICATE):
            return True
        return False

    def close(self):
        """Close the video player.

        Same parity dance as play/pause: the close button stays in the tree
        while the overlay is visually hidden, so the first element click can
        be eaten by the gesture layer (HTTP 200, nothing closed) and only
        reveals the controls. Click, VERIFY the player is really gone, and
        click once more if it is not - never trust the click alone.

        Three rounds, not two: a click landing mid-fade can be swallowed
        outright (no toggle, no actuation - observed live), and with only two
        rounds one swallowed click burns the whole budget."""
        try:
            for attempt in range(3):
                present = self._player_present()
                if present is False:
                    logger.info("✅ Video player closed")
                    return True

                session = self.get_session()
                clicked = False
                for predicate in (CLOSE_PREDICATES if session else []):  # exact names before loose
                    element_id = self._find(session, predicate)
                    if element_id and self._click(session, element_id):
                        clicked = True
                        break
                if clicked:
                    logger.info(f"🎬 Clicked close button (attempt {attempt + 1})")
                    time.sleep(CLICK_SETTLE_SECONDS)
                    continue  # the loop re-verifies; an eaten click revealed
                              # the overlay, so the next round's click lands

                # No close button clickable: reveal and go around once more
                self._reveal_controls(allow_blind_tap=present is True)
                time.sleep(REVEAL_SETTLE_SECONDS)

            # Verdict after the last click settles
            if self._player_present() is False:
                logger.info("✅ Video player closed")
                return True
            logger.error("❌ Failed to close video player")
            return False
        except Exception as e:
            logger.error(f"❌ Video close error: {e}")
            return False
