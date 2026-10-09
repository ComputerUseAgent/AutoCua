"""
service.py - Core iPhone interaction service via WebDriverAgent.

Holds ONLY the element interaction primitives (click, type, scroll) plus the
shared WDA session/element-mapping plumbing they need. Everything else lives
as a tool in controller/tool/ (open_app, videoplayer, web, ...).
"""

import requests
import time
import logging

logger = logging.getLogger(__name__)

class ControllerService:
    """Service for executing iPhone actions via WebDriverAgent"""

    def __init__(self):
        from AutoCua.ios_connector.session import wda_url
        self.wda_url = wda_url()
        self.elements = {}
        self.session_id = None

    def update_elements(self, elements_mapping):
        """Update element mapping from scanner"""
        self.elements = elements_mapping
        logger.debug(f"Updated elements: {len(self.elements)} elements")

    def get_session(self):
        """Get or create WDA session"""
        try:
            # Create new session
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

    def click(self, element_number):
        """Click on element by number"""
        elem_num = int(element_number)

        if str(elem_num) not in self.elements:
            logger.error(f"Element #{elem_num} not in loaded elements: {list(self.elements.keys())}")
            raise ValueError(f"Element #{elem_num} not found")

        elem_data = self.elements[str(elem_num)]
        bounds = elem_data['bounds']

        # Calculate center point
        center_x = bounds['x'] + bounds['width'] // 2
        center_y = bounds['y'] + bounds['height'] // 2

        logger.debug(f"Clicking at ({center_x}, {center_y})")

        # Get or create session
        session = self.get_session()
        if not session:
            raise Exception("Failed to create WDA session")

        # Use W3C Actions API for tapping
        try:
            response = requests.post(
                f"{self.wda_url}/session/{session}/actions",
                json={
                    "actions": [{
                        "type": "pointer",
                        "id": "finger1",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type": "pointerMove", "duration": 0, "x": center_x, "y": center_y},
                            {"type": "pointerDown", "button": 0},
                            {"type": "pause", "duration": 100},
                            {"type": "pointerUp", "button": 0}
                        ]
                    }]
                },
                timeout=5
            )

            if response.status_code == 200:
                logger.info(f"✓ Click successful on element {elem_num} at ({center_x}, {center_y})")
                return True
            else:
                logger.error(f"✗ WDA tap failed: {response.status_code} - {response.text}")
                raise Exception(f"WDA tap failed: {response.text}")

        except Exception as e:
            logger.error(f"✗ Click exception: {str(e)}")
            raise Exception(f"Click failed: {e}")

    def type_text(self, element_number, text):
        """Type text into element by number

        If element already has text, it will be deleted first before typing new text.
        Sends delete/backspace key for each existing character including spaces.
        """
        elem_num = int(element_number)

        if str(elem_num) not in self.elements:
            raise ValueError(f"Element #{elem_num} not found")

        # Get current value of the element
        current_value = self.elements[str(elem_num)].get('value', '')

        # First click on the element to focus it
        self.click(element_number)
        time.sleep(0.1)  # Small delay for focus

        # Get session
        session = self.get_session()
        if not session:
            raise Exception("Failed to create WDA session")

        # If element has existing text (not blank), delete it first
        if current_value and current_value.strip():
            # Send delete/backspace key for each character including spaces
            delete_keys = '\u0008' * len(current_value)

            try:
                requests.post(
                    f"{self.wda_url}/session/{session}/wda/keys",
                    json={'value': [delete_keys]},
                    timeout=10
                )
                logger.debug(f"Cleared {len(current_value)} characters from element")
            except Exception as e:
                logger.warning(f"Clear text error: {e}")

            time.sleep(0.1)  # Small delay after deletion

        # Now type the new text
        try:
            response = requests.post(
                f"{self.wda_url}/session/{session}/wda/keys",
                json={'value': [text]},
                timeout=10
            )

            if response.status_code == 200:
                return True
            else:
                raise Exception(f"WDA type failed: {response.text}")

        except Exception as e:
            raise Exception(f"Type failed: {e}")

    def scroll(self, element_number, direction):
        """Scroll/swipe within element bounds in specified direction

        Args:
            element_number: The element to scroll within
            direction: 'up', 'down', 'left', or 'right'
        """
        elem_num = int(element_number)

        if str(elem_num) not in self.elements:
            logger.error(f"Element #{elem_num} not in loaded elements: {list(self.elements.keys())}")
            raise ValueError(f"Element #{elem_num} not found")

        elem_data = self.elements[str(elem_num)]
        bounds = elem_data['bounds']
        direction = direction.lower()

        # Dynamic approach: Start from edge in SAME direction, swipe in that direction
        # This avoids center overlays and ensures the swipe starts from the correct edge

        # Margins and swipe distances
        margin = 20  # Safe margin from edge
        vertical_swipe_distance = 200  # More power for up/down swipes
        horizontal_swipe_distance = 200  # More power for left/right swipes

        # Calculate center for positioning
        center_x = bounds['x'] + bounds['width'] // 2
        center_y = bounds['y'] + bounds['height'] // 2

        # Calculate start and end points based on direction
        # Start from edge in the SAME direction and swipe in that direction
        if direction == 'up':
            # Start from top area, swipe upward (toward top edge)
            start_x = center_x
            start_y = bounds['y'] + margin + vertical_swipe_distance
            end_x = center_x
            end_y = bounds['y'] + margin
        elif direction == 'down':
            # Start from bottom area, swipe downward (toward bottom edge)
            start_x = center_x
            start_y = bounds['y'] + bounds['height'] - margin - vertical_swipe_distance
            end_x = center_x
            end_y = bounds['y'] + bounds['height'] - margin
        elif direction == 'left':
            # Start from left area, swipe leftward (toward left edge) - MORE POWER
            start_x = bounds['x'] + margin + horizontal_swipe_distance
            start_y = center_y
            end_x = bounds['x'] + margin
            end_y = center_y
        elif direction == 'right':
            # Start from right area, swipe rightward (toward right edge) - MORE POWER
            start_x = bounds['x'] + bounds['width'] - margin - horizontal_swipe_distance
            start_y = center_y
            end_x = bounds['x'] + bounds['width'] - margin
            end_y = center_y
        else:
            raise ValueError(f"Invalid direction: {direction}. Use: up, down, left, or right")

        logger.debug(f"Scrolling {direction} from ({start_x}, {start_y}) to ({end_x}, {end_y})")

        # Get or create session
        session = self.get_session()
        if not session:
            raise Exception("Failed to create WDA session")

        # Use W3C Actions API for swipe gesture
        try:
            response = requests.post(
                f"{self.wda_url}/session/{session}/actions",
                json={
                    "actions": [{
                        "type": "pointer",
                        "id": "finger1",
                        "parameters": {"pointerType": "touch"},
                        "actions": [
                            {"type": "pointerMove", "duration": 0, "x": start_x, "y": start_y},
                            {"type": "pointerDown", "button": 0},
                            {"type": "pause", "duration": 100},
                            {"type": "pointerMove", "duration": 500, "x": end_x, "y": end_y},
                            {"type": "pointerUp", "button": 0}
                        ]
                    }]
                },
                timeout=5
            )

            if response.status_code == 200:
                return True
            else:
                raise Exception(f"WDA scroll failed: {response.text}")

        except Exception as e:
            raise Exception(f"Scroll failed: {e}")

# Create global instance
controller_service = ControllerService()
