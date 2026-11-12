"""AutoCua package root — and the single source of truth for WHERE the user's
data lives.

User data must NOT sit inside the install folder: the Windows installer
uninstalls with `[UninstallDelete] Type: filesandordirs; Name: "{app}"`, i.e. it
deletes the WHOLE install directory, so anything written next to AutoCua.exe
(chats, API keys) died with the uninstall. Everything the user creates goes in
one folder no installer owns:

    <base>/AutoCua_data/
        agent_conversation/     chats: index.json, settings.json, chat_*/
    <base>/.env                 provider keys + Telegram token + Vertex config:
                                the .env you edit by hand, which the Settings
                                panel writes too (env_file below)

    base = Path.home()   packaged build   -> C:/Users/<u>/AutoCua_data
                                          -> /Users/<u>/AutoCua_data on macOS
    base = <repo root>   `python main.py` -> <repo>/AutoCua_data
    base = cwd           pip install      -> <where you ran main.py>/AutoCua_data
                                             (created the first time anything
                                             asks for it)

This lives in the package root rather than in any one sub-package because six
modules across windows, mac and ios need the same answer, and two
definitions of "where is AutoCua_data" that drift apart would silently put user
data back inside the install folder — the exact bug this exists to fix.

RULES for the code below:
  * stdlib ONLY at import time, and no filesystem side effects then. Every
    `import AutoCua.anything` executes this module first. (python-dotenv, a
    dependency, is imported inside the .env functions, which run later.)
  * NEVER print(). AutoCua re-execs itself as --banner-mode, which speaks a
    JSON-per-line protocol on stdout, and sys.stdout can be None at import time
    in the Windows GUI-subsystem binary. Log to `logger` (stderr) instead.
  * Never use the builtin open() on a path under AutoCua_data: compiled builds
    monkey-patch builtins.open to resolve embedded resources by path suffix
    (ui/service.py setup_embedded_resources), and a matching write is
    silently swallowed into a StringIO. Use Path.read_bytes / write_bytes /
    os.replace here.
"""

import io
import os
import re
import sys
import logging
import tempfile
import threading
import time
from pathlib import Path

logger = logging.getLogger("AutoCua.paths")

DATA_DIR_NAME = "AutoCua_data"

# Absolute-path override; wins over the rules below. The --banner-mode child
# and the agents' child processes are spawned with os.environ.copy(), so they
# inherit it and parent + children can never disagree on the data root.
ENV_DATA_DIR = "AutoCua_DATA_DIR"

# The .env that holds the keys (env_file), pinned the same way by data_root(),
# so a child process always uses its parent's file. Also yours to set.
ENV_ENV_FILE = "AutoCua_ENV_FILE"

# Compiled (Nuitka) binary vs dev run — mirrors the detection in app.py.
IS_COMPILED = bool(getattr(sys, "frozen", False)) or ("__compiled__" in globals())

# <repo>/AutoCua/__init__.py -> <repo>. __file__-based, NEVER cwd-based: the
# cli / minion children are spawned as `python -m ...` with no explicit cwd.
_REPO_ROOT = Path(__file__).resolve().parent.parent


def _home_dir() -> Path:
    """The user's home. Path.home() honours USERPROFILE on Windows."""
    try:
        return Path.home()
    except Exception:
        return Path(os.path.expanduser("~"))


def _base_dir() -> Path:
    """Parent folder that holds AutoCua_data/ (not created here)."""
    if IS_COMPILED:
        return _home_dir()
    # Dev = the checkout this file lives in (main.py beside the package).
    if (_REPO_ROOT / "main.py").is_file():
        return _REPO_ROOT
    # pip install: the package sits in site-packages, so the data goes next to
    # whatever main.py the user ran — the working directory. data_root()
    # creates the folder the first time it is asked for.
    return Path.cwd()


def _ensure(p: Path) -> Path:
    """mkdir -p, best effort. Callers already guard their own reads and writes,
    so a read-only or full disk degrades to 'no data' rather than crashing the
    GUI on startup."""
    try:
        p.mkdir(parents=True, exist_ok=True)
    except Exception:
        logger.exception("mkdir %s", p)
    return p


def data_root() -> Path:
    """<home>/AutoCua_data (packaged) or <repo>/AutoCua_data (dev)."""
    # A blank/whitespace override must read as UNSET: Path("") resolves to ".",
    # and callers rmtree/overwrite paths under this root.
    override = (os.environ.get(ENV_DATA_DIR) or "").strip()
    root = Path(override).expanduser() if override else _base_dir() / DATA_DIR_NAME
    if not override:
        # Pin it for every child (cli / minion / parallel tasks are spawned
        # with os.environ.copy()), so a child started with another cwd can
        # never end up with its own AutoCua_data.
        os.environ[ENV_DATA_DIR] = str(root.resolve())
    # The .env is pinned with it, decided here once from the folder as it was
    # chosen (a child sees AutoCua_DATA_DIR set and could not tell any more).
    pinned = (os.environ.get(ENV_ENV_FILE) or "").strip()
    os.environ[ENV_ENV_FILE] = (os.path.abspath(os.path.expanduser(pinned)) if pinned
                                else str(_default_env_file(root, chosen=bool(override))))
    return _ensure(root)


def _default_env_file(root: Path, chosen: bool) -> Path:
    """The .env when nothing pinned one yet: beside the data folder, the .env
    you edit by hand (<repo>/.env in a checkout, the folder you run main.py
    from after pip install), taken from the path as it is, so a symlinked
    AutoCua_data does not move it. Inside the data folder for a folder you
    chose with AutoCua_DATA_DIR (the folder above it may not be yours to
    write), a compiled build, or a data folder in your home folder: a
    <home>/.env would be read by every other project's dotenv search up the
    folder tree."""
    root = Path(os.path.abspath(root))
    base = root.parent
    if chosen or IS_COMPILED or os.path.realpath(base) == os.path.realpath(_home_dir()):
        return root / ".env"
    return base / ".env"


BROWSER_PROFILES_DIR = "browser_profiles"
DEFAULT_BROWSER_PROFILE = "default"


def normalize_profile_name(name) -> str:
    """A browser profile name, or ValueError.

    The name becomes a path segment under `data_root()`, which is a tree this
    codebase deletes from — so this REJECTS rather than sanitizes. Quietly
    mapping "../../etc" to "etc" would be worse than an error: it would delete
    or overwrite something the caller never named.
    """
    text = str(name or "").strip().lower()
    if not text:
        return DEFAULT_BROWSER_PROFILE
    if len(text) > 64 or text.startswith("."):
        raise ValueError(f"browser profile name {text!r} must be 1-64 chars and not start with '.'")
    if any(c not in "abcdefghijklmnopqrstuvwxyz0123456789._-" for c in text):
        raise ValueError(
            f"browser profile name {text!r} may only contain letters, digits, '.', '_' and '-'"
        )
    if text in ("_tmp", ".", ".."):
        raise ValueError(f"browser profile name {text!r} is reserved")
    return text


def browser_profile_dir(name=None, browser=None) -> Path:
    """The user-data-dir for a named browser profile, created if absent.

    <data_root>/browser_profiles/<name>/<browser>

    `browser` is "chrome" (the agent's browser) unless named. The browser
    owns and litters its user-data-dir, so it is nested one level down: that
    leaves room beside it for our own files and makes "reset this profile" a
    delete of one folder rather than of the profile's identity. A normal run
    drives the person's own Chrome through the AutoCua extension and never
    comes here; this folder is for the headless and parallel runs, which have
    a browser of their own on a debugging port.

    This is what makes the agent arrive already logged in. Cookies,
    localStorage and IndexedDB live in here and survive between runs, so a site
    the agent signed into once does not have to be signed into again — which
    matters far more than any fingerprint tuning, because a login wall is where
    a browser agent usually stops.
    """
    if browser is None:
        browser = "chrome"
    root = data_root() / BROWSER_PROFILES_DIR / normalize_profile_name(name)
    _ensure(root)
    return _ensure(root / browser)


def install_dir() -> Path:
    """Where the app's own code sits — the folder the uninstaller deletes. Used
    only to find data left behind by a pre-AutoCua_data build."""
    return Path(sys.executable).resolve().parent if IS_COMPILED else _REPO_ROOT


# =============================================================================
# skills — the .md knowledge files
# =============================================================================
# Two folders, same layout (skills.json + *.md), read together by every
# DomainKnowledgeService:
#   AutoCua/default_skills/<windows|mac|ios>/   the defaults shipped with the
#                                                 package (read-only, tracked)
#   AutoCua_data/skills/<windows|mac|ios>/        the user's own skills, added
#                                                 from the UI or by hand
# Nothing is ever copied from one to the other. A name is loaded once: the
# default first, then the user folder; a file missing from both is ignored.
# NOTE: `plat` IS the package name (AutoCua/{mac,windows,linux,ios}), so no map.

DEFAULT_SKILLS_DIR = "default_skills"


def skills_platform(value=None) -> str:
    """'windows' | 'mac' | 'linux' | 'ios'. Accepts sys.platform or a package
    name. iOS is driven from a Mac, so it is never the host default - only an
    explicit "ios" selects its folder."""
    key = str(sys.platform if value is None else value).lower()
    if key == "ios":
        return "ios"
    if "darwin" in key or "mac" in key:
        return "mac"
    if "linux" in key:
        return "linux"
    return "windows"


def _skills_from_dir(src: Path) -> dict:
    out = {}
    if src.is_dir():
        for p in sorted(src.iterdir()):
            if p.is_file() and (p.suffix.lower() == ".md" or p.name == "skills.json"):
                try:
                    out[p.name] = p.read_text(encoding="utf-8")
                except Exception:
                    logger.warning("unreadable default skill %s", p)
    return out


def _skills_from_resources(prefix: str) -> dict:
    """Pull skill files out of the binary's embedded-resource blobs. Keys are
    repo-root-relative with forward slashes, e.g.
    'AutoCua/default_skills/windows/browser.md'."""
    out = {}
    try:
        import base64
        from _embedded_resources import RESOURCES   # generated by the binary build
    except Exception:
        return out
    for key, blob in RESOURCES.items():
        k = str(key).replace("\\", "/")
        i = k.find(prefix)
        name = k[i + len(prefix):] if i >= 0 else ""
        if name and "/" not in name and (name.endswith(".md") or name == "skills.json"):
            try:
                out[name] = base64.b64decode(blob).decode("utf-8")
            except Exception:
                logger.warning("undecodable embedded skill %s", key)
    return out


def default_skills_dir(platform=None) -> Path:
    """AutoCua/default_skills/<windows|mac|linux|ios> — the shipped defaults.
    Read-only; may not exist on disk in a compiled build (see default_skills)."""
    return Path(__file__).resolve().parent / DEFAULT_SKILLS_DIR / skills_platform(platform)


def default_skills(platform=None) -> dict:
    """{filename: text} of the shipped default skills for the platform
    (skills.json + *.md). In a compiled build they arrive as embedded blobs;
    otherwise they are read straight off the package directory."""
    plat = skills_platform(platform)
    return (_skills_from_resources("AutoCua/%s/%s/" % (DEFAULT_SKILLS_DIR, plat))
            or _skills_from_dir(default_skills_dir(plat)))


def skills_dir(platform=None) -> Path:
    """AutoCua_data/skills/<windows|mac|linux|ios> — the user's own skills, created
    empty on first use. The defaults are never copied here."""
    return _ensure(data_root() / "skills" / skills_platform(platform))


def vault_file() -> Path:
    """AutoCua_data/vault/credentials.json — the app's saved credentials, one
    section per device: {"ios": {<app>: {...}}, "browser": {<site>: {...}}}. Each
    agent reads only its own section.

    Created with empty sections if absent, so the folder is discoverable and the
    user has an obvious file to fill in. Lives outside the install folder like
    everything else here, so an uninstall can't take the credentials with it."""
    p = _ensure(data_root() / "vault") / "credentials.json"
    if not p.exists():
        try:
            # write_text -> io.open, so the compiled build's builtins.open patch
            # can't swallow the write into a throwaway buffer.
            p.write_text('{\n  "ios": {},\n  "browser": {}\n}\n', encoding="utf-8")
        except Exception:
            logger.exception("create %s", p)
    return p


# =============================================================================
# .env: provider keys, the Telegram bot token, and Vertex config
# =============================================================================
# ONE file holds every key: the .env you edit by hand is the file the Settings
# panel and the Telegram bot write, and every reader (llm_provider, Settings,
# Telegram) reads that same file. data_root() decides where it is once, and
# pins it in AutoCua_ENV_FILE for every child (_default_env_file): beside
# AutoCua_data (<repo>/.env in a checkout, the folder you run main.py from after
# pip install), or inside it.
#
# python-dotenv parses it, the parser load_dotenv uses, so quotes, `export` and
# comments read the same everywhere. The keys of the old key file
# (AutoCua_data/api_key/api_key.txt) move into it once (_move_api_key_txt).
_env_moved = False

# Left in AutoCua_data by a move: an old key file not newer than it was moved
# already, and is never moved again (_move_api_key_txt).
_MOVED_MARK = ".api_key_moved"

# One read-change-write of the .env at a time in this process: the Settings key
# routes (ui/service.py) and the Telegram bot, Connect and Disconnect
# (remote_connection/telegram) all hold it, so no save drops another's change.
api_key_lock = threading.RLock()


def env_file() -> Path:
    """The one .env (see above), after moving the old api_key.txt into it on
    the first call in each process. Never raises."""
    data_root()                                   # pins AutoCua_ENV_FILE
    path = Path(os.environ[ENV_ENV_FILE])
    _move_api_key_txt(path)
    return path


def read_env(path: Path = None) -> dict:
    """NAME -> value for every line of the .env (env_file() unless `path`), as
    load_dotenv reads them; '' for a NAME with no value. {} when the file is
    missing or cannot be read."""
    path = env_file() if path is None else path
    try:
        text = _env_text(path, errors="replace")
    except OSError:
        return {}
    return _env_values(text)


def _env_text(path: Path, errors: str = "strict") -> str:
    """The file's text as it is on disk. Path.read_bytes, not the builtin
    open() (RULES above), and no newline translation, so a Windows file keeps
    its \\r\\n when it is written back. utf-8-sig: a BOM that a Windows editor
    saved must not stick to the first name."""
    return path.read_bytes().decode("utf-8-sig", errors)


def _env_values(text: str) -> dict:
    from dotenv import dotenv_values
    return {k: v or "" for k, v in dotenv_values(stream=io.StringIO(text)).items()}


def set_env(updates: dict, path: Path = None) -> None:
    """Set NAME=value for every item of `updates` in ONE rewrite of the .env
    (env_file() unless `path`), keeping every other line as it is. '' clears a
    value; a name the file does not have yet is added at the end. Written with
    replace_file_text: owner-only, and a reader meets the old file or the new
    one. This process's environment follows (_env_follow), so the change counts
    at once. Raises OSError when the file cannot be written."""
    path = env_file() if path is None else path
    updates = {name: "" if value is None else str(value) for name, value in updates.items()}
    with api_key_lock:
        try:
            text = _env_text(path)
        except FileNotFoundError:
            text = ""
        new, before = _env_rewrite(text, updates)
        if new != text:
            replace_file_text(path, new)
        _env_follow(updates, before)


def _env_value(value: str) -> str:
    """`value` as python-dotenv reads it back: plain when it can be, else in
    double quotes with \\, " and line breaks escaped."""
    if re.fullmatch(r"[^\s'\"\\#]*", value):
        return value
    return '"' + (value.replace("\\", "\\\\").replace('"', '\\"')
                  .replace("\n", "\\n").replace("\r", "\\r")) + '"'


def _env_rewrite(text: str, updates: dict, header: str = "") -> tuple:
    """(new text, {name: value before}) for set_env: every line that sets a name
    of `updates` gets its new value (the blank lines python-dotenv counts in
    front of it, an `export ` and the line break stay); every other line stays
    as it is. A name with no line yet is added at the end, after `header` when
    given, unless its new value is ''."""
    from dotenv.parser import parse_stream
    nl = "\r\n" if "\r\n" in text else "\n"
    out, before = [], {}
    for binding in parse_stream(io.StringIO(text)):
        line = binding.original.string
        if binding.key is None or binding.key not in updates:
            out.append(line)
            continue
        before[binding.key] = binding.value or ""
        body = line.lstrip()
        lead = line[:len(line) - len(body)]
        export = "export " if re.match(r"export[^\S\r\n]", body) else ""
        end = line[len(line.rstrip("\r\n")):]      # \r\n, \n, a bare \r, or none
        out.append(f"{lead}{export}{binding.key}={_env_value(updates[binding.key])}{end}")
    new = "".join(out)
    add = [name for name, value in updates.items() if name not in before and value]
    if add:
        if new and not new.endswith(("\n", "\r")):
            new += nl
        new += header + "".join(f"{name}={_env_value(updates[name])}{nl}" for name in add)
    return new, before


def _env_follow(updates: dict, before: dict) -> None:
    """This process's environment after a write: a name whose value came from
    the file, or that has none, takes the new value, or goes when it is
    cleared; a value the shell set to something else stays, as load_dotenv
    never overrides one."""
    for name, value in updates.items():
        if os.environ.get(name) in (None, "", before.get(name)):
            if value:
                os.environ[name] = value
            else:
                os.environ.pop(name, None)


def _move_api_key_txt(dest: Path) -> None:
    """Move the keys of the old key file into the .env, once per process, from
    ONE file: AutoCua_data/api_key/api_key.txt, or the older copy in the
    install folder only when that one is not there (the old rule: a stale copy
    never overrides newer keys). A file not newer than the mark a move leaves
    (_MOVED_MARK) was moved before and is left alone, so one that could not be
    deleted never brings back a key cleared since. Never raises: a failure is
    logged, and the next start tries again."""
    global _env_moved
    if _env_moved:
        return
    with api_key_lock:
        if _env_moved:
            return
        _env_moved = True        # first: never again in this process, also after a failure
        mark = data_root() / _MOVED_MARK
        try:
            moved_at = mark.stat().st_mtime
        except OSError:
            moved_at = None
        for src in (data_root() / "api_key" / "api_key.txt",
                    install_dir() / "AutoCua" / "api_key" / "api_key.txt"):
            try:
                if not src.is_file():
                    continue
                if moved_at is None or src.stat().st_mtime > moved_at:
                    _move_key_file(src, dest, mark)
            except Exception:
                logger.exception("move %s into %s", src, dest)
            return                               # the first file there, only


def _one_line(value: str) -> str:
    """A noted value on one line, so the note stays one comment line."""
    return value.replace("\r", "\\r").replace("\n", "\\n")


def _move_key_file(src: Path, dest: Path, mark: Path) -> None:
    """One old key file (NAME=value lines) into the .env, in one rewrite.

    The old file's value wins over another value the .env has: it is the one
    the app used (Settings and Telegram read only the old file, and Vertex took
    it first). The .env's value it replaces goes at the end as a comment, as
    does an old line python-dotenv would read under another name or with
    another value (an `export ` in the name, ${...} in the value), so nothing
    is lost and you can pick. The mark is set and the old file deleted only
    once the .env reads back every key moved."""
    old = {}
    for line in _env_text(src, errors="replace").splitlines():
        name, eq, value = line.strip().partition("=")
        name, value = name.strip(), value.strip()
        if eq and name and value and not name.startswith("#"):
            old[name] = value
    try:
        text = _env_text(dest)
    except FileNotFoundError:
        text = ""
    have = _env_values(text)
    move, replaced, unmoved = {}, {}, {}
    for name, value in old.items():
        if have.get(name) == value:
            continue                                     # the .env has it already
        if (_env_values(f"{name}=x\n") == {name: "x"}
                and _env_values(f"X={_env_value(value)}\n").get("X") == value):
            move[name] = value
            if have.get(name):
                replaced[name] = have[name]
        else:
            unmoved[name] = value
    nl = "\r\n" if "\r\n" in text else "\n"
    new, before = _env_rewrite(text, move, header=f"# Moved here from {src}{nl}")
    for title, notes in ((f"# Values these keys had here before the move from {src} (not used):", replaced),
                         (f"# From {src}, not moved, as they would not read back the same here:", unmoved)):
        if notes:
            if new and not new.endswith(("\n", "\r")):
                new += nl
            new += title + nl + "".join(f"# {name}={_one_line(value)}{nl}" for name, value in notes.items())
    if replaced:
        logger.warning("%s: the values from %s replaced its own for %s; those are comments at its end",
                       dest, src, ", ".join(sorted(replaced)))
    if unmoved:
        logger.warning("%s: not moved from %s: %s; they are comments at its end",
                       dest, src, ", ".join(sorted(unmoved)))
    if new != text:
        replace_file_text(dest, new)
        _env_follow(move, before)
    now = _env_values(_env_text(dest)) if move else {}
    if all(now.get(name) == value for name, value in move.items()):
        mark.touch()
        try:
            src.unlink()
        except OSError as e:                    # a read-only install folder, a file in use
            logger.warning("moved the keys of %s into %s, but could not delete it (%s); "
                           "the mark keeps it from being moved again", src, dest, e.strerror or e)
            return
        try:
            src.parent.rmdir()                  # only when nothing else is left in it
        except OSError:
            pass
        logger.info("moved the keys of %s into %s", src, dest)


def replace_file_text(path: Path, text: str) -> None:
    """Write `text` to `path` in one step: a reader, a crash or a full disk
    meets the old file or the new one, not an empty or half-written one
    (except in the Windows fallback below).

    Staged in a temp file in the same folder (mode 0600) and renamed over the
    old file. Windows refuses the rename while another thread has the file
    open for a moment, so it is tried again for half a second, then written
    in place as before. A symlinked file is written at its target, so the
    link stays. Path/os calls only (see RULES above)."""
    path = Path(os.path.realpath(path))
    data = text.encode("utf-8")
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".part")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        for _ in range(20):
            try:
                os.replace(tmp, str(path))
                return
            except PermissionError:
                time.sleep(0.025)
        path.write_bytes(data)
    finally:
        if os.path.exists(tmp):
            try:
                os.unlink(tmp)
            except OSError:
                pass


# =============================================================================
# The desktop app's port
# =============================================================================
# AutoCua's own port on 127.0.0.1. Everything between the window and the
# backend goes through it: the UI's files, its /api/ calls, the desktop
# shell's event stream, and the pages the remote-connection banners embed.
# Below 32768, so outside the range macOS, Windows and Linux hand out to
# outgoing connections: the OS never gives it to anything on its own. If
# another program holds it anyway, ui/service.py serves that run on a free
# port instead and exports the one it got as AUTOCUA_APP_PORT, which every
# process it starts inherits.
APP_PORT = 27321
ENV_APP_PORT = "AUTOCUA_APP_PORT"


def app_origin() -> str:
    """http://127.0.0.1:<port> of the running desktop app's server."""
    return f"http://127.0.0.1:{os.environ.get(ENV_APP_PORT) or APP_PORT}"
