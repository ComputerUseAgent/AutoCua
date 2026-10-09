"""<skills> for the iOS driver: the .md matched to the app in front.

The app in front costs nothing to know - every scan parses WDA's page source,
whose root XCUIElementTypeApplication carries the app's name (the scanner
exposes it as `application_name`). skills.json maps that name, or the app's
bundle id, to a markdown file — a shipped default in AutoCua/default_skills/
ios/ or the user's own in AutoCua_data/skills/ios/ (this package holds only
the code).
"""

import json
import logging
import os
from pathlib import Path

logger = logging.getLogger(__name__)


class DomainKnowledgeService:

    def __init__(self):
        # Two folders, read together: the defaults shipped in
        # AutoCua/default_skills/ios/ and the user's own skills in
        # AutoCua_data/skills/ios/ (added from the UI or by hand).
        try:
            from AutoCua import skills_dir, default_skills
            self.dir = Path(skills_dir("ios"))
            self.defaults = default_skills("ios")         # {filename: text}
        except Exception:
            self.dir = Path(__file__).resolve().parent
            self.defaults = {}
        self.mappings = self._load()

    def _load(self) -> dict:
        """skills.json of the defaults merged with the user's skills.json: the
        default entry wins for a key present in both, the user file only adds."""
        merged = {}
        sources = []
        if self.defaults.get("skills.json"):
            sources.append(("default", self.defaults["skills.json"]))
        try:
            p = self.dir / "skills.json"
            if p.is_file():
                sources.append(("user", p.read_text(encoding="utf-8")))
        except Exception as e:
            logger.error("Error reading user skills.json: %s", e)
        for origin, text in sources:
            try:
                data = json.loads(text)
            except Exception as e:
                logger.error("Error loading %s skills.json: %s", origin, e)
                continue
            if not isinstance(data, dict):
                continue
            for bucket, mapping in data.items():
                if not isinstance(mapping, dict):
                    continue
                target = merged.setdefault(bucket, {})
                for key, md in mapping.items():
                    target.setdefault(key, md)          # first (default) wins
        if not sources:
            logger.warning("skills.json not found in %s or the defaults", self.dir)
        return merged

    @staticmethod
    def _norm(text) -> str:
        return str(text or "").strip().lower()

    def match(self, application_name: str) -> str:
        """The skill filename for the app in front, or "" (home screen, no
        entry). Exact name match first; then the app's bundle id, resolved
        from the installed-app list the launcher already caches (no device
        request) - so a renamed app still finds its skill."""
        name = self._norm(application_name)
        if not name:
            return ""
        apps = self.mappings.get("apps") or {}
        by_key = {self._norm(k): v for k, v in apps.items()}
        if name in by_key:
            return by_key[name]
        try:
            from ...controller.tool.open_app import app_launcher_service
            for info in app_launcher_service.apps_dict.values():
                if self._norm(info.get("display_name")) == name:
                    md = by_key.get(self._norm(info.get("bundle_id")))
                    if md:
                        return md
        except Exception:
            pass
        return ""

    def _read(self, filename: str) -> str:
        """The shipped default of that name if there is one, else the user's
        copy, else "" (a missing skill is ignored). One name loads once."""
        try:
            if filename in self.defaults:
                return self.defaults[filename].strip()
            p = self.dir / filename
            if p.is_file():
                return p.read_text(encoding="utf-8").strip()
            logger.warning("Skill file not found: %s", p)
        except Exception as e:
            logger.error("Error reading skill %s: %s", filename, e)
        return ""

    def get_knowledge(self, application_name: str) -> str:
        """<domain_knowledge="<skill>">...</domain_knowledge> for the app in
        front, or "" when nothing matches."""
        md = self.match(application_name)
        if not md:
            return ""
        text = self._read(md)
        if not text:
            return ""
        context = os.path.splitext(md)[0]
        return f'<domain_knowledge="{context}">\n{text}\n</domain_knowledge>'
