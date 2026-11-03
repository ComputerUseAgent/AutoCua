"""Where WebDriverAgent must be, and what to say when it is not there.

WebDriverAgent (WDA) is the Appium project's Xcode app that drives an iPhone, an
iPad or a simulator. AutoCua never downloads it: the person puts it in
AutoCua_data/WebDriverAgent with one git clone (wda_command), and every place
that needs it asks wda_ready() first. While it is missing, iOS automation is
paused, and the terminal and the app show MISSING with that command.

It lives in AutoCua_data, beside the person's other data, and not in the code:
pairing signs it for the person's own Apple team (setup.py rewrites its
project.pbxproj). Its build products live elsewhere, in Application Support
(ios_connector/build_paths.py).

WebDriverAgent is BSD-3-Clause; this tree holds none of its source. See
THIRD_PARTY_NOTICES.md.
"""

from pathlib import Path

# Pinned tag: keep in step with setup_mac.sh and THIRD_PARTY_NOTICES.md.
VERSION = "v15.1.1"
REPO = "https://github.com/appium/WebDriverAgent.git"

# The one short line the terminal, the app and the agent show while it is
# missing. Short and with no ". " inside: the chat box shows its first sentence
# only, and <sub_agents> cuts a reason at 100 characters.
MISSING = "iOS automation paused: WebDriverAgent is missing"


def wda_dir() -> Path:
    """AutoCua_data/WebDriverAgent, where the person puts it."""
    from AutoCua import data_root
    return data_root() / "WebDriverAgent"


def wda_project() -> Path:
    """Its Xcode project, the path xcodebuild and the signer are given."""
    return wda_dir() / "WebDriverAgent.xcodeproj"


def wda_ready() -> bool:
    """True when WebDriverAgent is in place. Its project file must be there, so
    a clone cut short does not count. Never downloads anything."""
    return (wda_project() / "project.pbxproj").is_file()


def wda_command() -> str:
    """The one command that puts it in place, with the full path quoted."""
    return f'git clone --depth 1 --branch {VERSION} {REPO} "{wda_dir()}"'


def wda_message() -> str:
    """MISSING and the command, for the terminal."""
    return f"{MISSING}. Put it in place with:\n  {wda_command()}"


if __name__ == "__main__":
    print("WebDriverAgent ready at " + str(wda_dir()) if wda_ready() else wda_message())
