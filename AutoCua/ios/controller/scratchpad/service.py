import os
import logging

# Configure logger
logger = logging.getLogger(__name__)

class ScratchpadService:
    def __init__(self):
        """Initialize the Scratchpad Service"""
        current_dir = os.path.dirname(os.path.abspath(__file__))
        # Go up two levels from AutoCua/ios/controller/scratchpad/ to reach ios/
        ios_dir = os.path.dirname(os.path.dirname(current_dir))

        # On-disk storage kept as "milestone/milestone.md" to avoid the
        # scratchpad/scratchpad/scratchpad.md collision with the parent dir.
        # Parallel simulator tasks each get their own scratchpad/<session>/
        # subtree (AutoCua_IOS_SESSION), so they never overwrite each other.
        session = os.environ.get("AutoCua_IOS_SESSION") or ""
        self.scratchpad_dir = os.path.join(ios_dir, "scratchpad", session, "milestone") \
            if session else os.path.join(ios_dir, "scratchpad", "milestone")
        self.scratchpad_file = os.path.join(self.scratchpad_dir, "milestone.md")

        # Create scratchpad directory if it doesn't exist
        self._ensure_scratchpad_directory()

    def _ensure_scratchpad_directory(self):
        """Create scratchpad directory if it doesn't exist"""
        try:
            os.makedirs(self.scratchpad_dir, exist_ok=True)
        except Exception as e:
            logger.error(f"Error creating scratchpad directory: {str(e)}")
            raise

    def append_scratchpad(self, scratchpad_content):
        """Append a scratchpad entry with sequential numbering"""
        try:
            existing_count = 0
            if os.path.exists(self.scratchpad_file):
                with open(self.scratchpad_file, "r", encoding="utf-8") as f:
                    for line in f:
                        if line.strip():
                            existing_count += 1

            next_number = existing_count + 1

            with open(self.scratchpad_file, "a", encoding="utf-8") as f:
                f.write(f"{next_number}. {scratchpad_content}\n")

            return True

        except Exception as e:
            logger.error(f"Error appending scratchpad entry: {str(e)}")
            return False

    def read_scratchpad(self):
        """Read current scratchpad content"""
        try:
            if os.path.exists(self.scratchpad_file):
                with open(self.scratchpad_file, "r", encoding="utf-8") as f:
                    return f.read().strip()
            return ""
        except Exception as e:
            logger.error(f"Error reading scratchpad: {str(e)}")
            return ""

    def clear_scratchpad(self):
        """Clear the scratchpad file"""
        try:
            if os.path.exists(self.scratchpad_file):
                os.remove(self.scratchpad_file)
            return True
        except Exception as e:
            logger.error(f"Error clearing scratchpad: {str(e)}")
            return False
