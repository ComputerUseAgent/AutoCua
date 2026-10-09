#!/bin/bash
#
# AutoCua — Linux one-click setup
# ================================
# Installs the distro packages AutoCua needs (the AT-SPI bindings the scanner
# and controller read the desktop through, a C compiler and curl for the web
# agent), installs uv (if missing), creates a local .venv/ on the distro's
# Python, and installs linux_requirements.txt into it. Then installs the Rust
# toolchain (if missing) and builds the web agent's native extension, so the
# first "web use" run doesn't stop on a missing cargo. The Linux twin of
# MacOS_setup.sh, step for step.
#
# How to run (either):
#   bash linux_setup.sh          # simplest, works right after clone
#   chmod +x linux_setup.sh && ./linux_setup.sh
#
# After it finishes:
#   source .venv/bin/activate
#   python main.py               # run the agent (modes: see agent_capabilities.md)
#
# Only the distro-package step needs sudo. Everything else stays in your home
# folder: ~/.local/bin (uv), ~/.cargo (Rust) and ./.venv (Python).
#

set -e

cd "$(dirname "$0")"

MIN_PYTHON="3.10"
# Exclusive upper bound. onnxruntime (the OCR runtime the scanner uses)
# publishes wheels up to CPython 3.14 today; raise this when it moves on.
MAX_PYTHON="3.15"
PYTHON_SPEC=">=$MIN_PYTHON,<$MAX_PYTHON"

# .venv/ is the name uv looks for by default, so `uv pip install`, `uv run` and
# `uv sync` all find it with no --python flag. VENV_PROMPT, passed to
# `uv venv --prompt` below, labels the activated shell "(auto-cua)" whatever
# this repo's folder is called, rather than ".venv" or the folder name.
VENV_DIR=".venv"
VENV_PROMPT="auto-cua"

# -----------------------------------------------------------------------------
# Print helpers (match the style used across the project's build scripts)
# -----------------------------------------------------------------------------
print_step()   { printf "\n============================================================\n  %s\n============================================================\n\n" "$1"; }
print_ok()     { printf "  [OK] %s\n" "$1"; }
print_info()   { printf "  [INFO] %s\n" "$1"; }
print_error()  { printf "  [ERROR] %s\n" "$1"; }

# fetch URL FILE — curl first, wget second. Ubuntu Desktop ships wget but not
# always curl; step 0 installs curl, but this must also work before it has.
# Downloaded to a file rather than piped straight into sh: in `curl ... | sh`,
# a curl that dies mid-transfer still leaves sh reading a truncated script and
# exiting 0, so the install looks like it worked.
fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -sSfL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --https-only "$1" -O "$2"
    else
        return 1
    fi
}

# Run a command as root: directly when we are root, through sudo otherwise.
as_root() {
    if [ "$(id -u)" = "0" ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        return 127
    fi
}

# -----------------------------------------------------------------------------
# Step 0 — distro packages
# -----------------------------------------------------------------------------
# What each one is for:
#   python3-gi gir1.2-atspi-2.0 gir1.2-gtk-3.0
#       AT-SPI and Gdk bindings. The scanner (AutoCua/linux/tree) and the
#       controller read and drive the desktop through them. pip cannot install
#       these — the typelibs are GObject introspection data, not Python
#       packages (linux_requirements.txt explains) — so they come from apt.
#   at-spi2-core
#       The accessibility bus itself (org.a11y.Bus) and its registry daemon.
#       Without it the bindings above import fine and every tree comes back
#       empty, which reads like a scanner bug and is not one.
#   xdg-desktop-portal xdg-desktop-portal-gnome
#       org.freedesktop.portal.RemoteDesktop — the ONLY way to move the
#       pointer or press a key on Wayland (input_portal.py explains why every
#       other route is dead). Without the GNOME backend the portal exists but
#       answers nothing, and the agent can see the screen yet never click it.
#       KDE users want xdg-desktop-portal-kde instead.
#   python3-cairo gir1.2-webkit2-4.1
#       pywebview's GTK backend, for the desktop GUI app. WebKit2 4.1 is the
#       GTK3 build — the generation element.py pins. Not needed for headless
#       `python main.py`, and the GUI does not start on Linux yet, but a fresh
#       machine that lacks them fails at `import webview` rather than at a
#       place that explains itself.
#   libglib2.0-bin libgtk-3-bin xdg-utils
#       The launcher's three helpers. open_app.py:294 starts an app with
#       `gio launch` and falls back to `gtk-launch` -- a bare Popen makes a
#       DBusActivatable app report "window is ready" without ever taking
#       focus -- and open_app.py:206 asks `xdg-settings` which browser is the
#       default. xdg-open is in the same package and is what the agent's own
#       `shell` examples use. Missing, apps launch unfocused or not at all.
#   python3-venv
#       Only for building the venv BY HAND. A stock Ubuntu python3 has no
#       ensurepip, so `python3 -m venv .venv` fails; this script uses uv,
#       which does not care, so nothing here depends on it. Other distros
#       ship ensurepip inside their python3 package.
#   build-essential
#       cc. cargo links the web agent through it, and ring (the SHA-256
#       that stages the browser extension) compiles C sources. Nothing
#       else needs it.
#   curl ca-certificates
#       the uv and rustup installers.
#
# Not fatal: a machine without sudo still gets the venv and every pip
# dependency below. Each later step re-checks what it needs and says so.
print_step "STEP 0: Distro packages"

APT_PKGS="python3-gi python3-cairo python3-gi-cairo python3-venv gir1.2-atspi-2.0 gir1.2-gtk-3.0 gir1.2-webkit2-4.1 gir1.2-gstreamer-1.0 gstreamer1.0-pipewire gstreamer1.0-plugins-base at-spi2-core xdg-desktop-portal xdg-desktop-portal-gnome libglib2.0-bin libgtk-3-bin xdg-utils build-essential curl ca-certificates"
DNF_PKGS="python3-gobject python3-cairo at-spi2-core gtk3 webkit2gtk4.1 gstreamer1 gstreamer1-plugins-base pipewire-gstreamer xdg-desktop-portal xdg-desktop-portal-gnome glib2 xdg-utils gcc gcc-c++ make curl ca-certificates"
PACMAN_PKGS="python-gobject python-cairo at-spi2-core gtk3 webkit2gtk-4.1 gstreamer gst-plugins-base gst-plugin-pipewire xdg-desktop-portal xdg-desktop-portal-gnome glib2 xdg-utils base-devel curl ca-certificates"
ZYPPER_PKGS="python3-gobject python3-cairo python3-gobject-cairo typelib-1_0-Atspi-2_0 typelib-1_0-Gtk-3_0 typelib-1_0-WebKit2-4_1 typelib-1_0-Gst-1_0 gstreamer-plugins-base gstreamer-plugin-pipewire at-spi2-core xdg-desktop-portal xdg-desktop-portal-gnome glib2-tools gtk3-tools xdg-utils gcc gcc-c++ make curl ca-certificates"

install_pkgs() {
    # $1 = human name, rest = the install command
    local name="$1"; shift
    print_info "Installing with $name (sudo will ask for your password)..."
    if as_root "$@"; then
        print_ok "Distro packages installed"
    else
        print_error "Could not install the distro packages."
        print_info  "Run this yourself, then re-run the script:"
        printf "    sudo %s\n" "$*"
    fi
}

if command -v apt-get >/dev/null 2>&1; then
    # A fresh box may have stale lists; a failed refresh (an unreachable
    # third-party repo, say) must not block the install itself.
    as_root apt-get update -qq >/dev/null 2>&1 || true
    # shellcheck disable=SC2086
    install_pkgs "apt" apt-get install -y $APT_PKGS
elif command -v dnf >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    install_pkgs "dnf" dnf install -y $DNF_PKGS
elif command -v pacman >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    install_pkgs "pacman" pacman -S --needed --noconfirm $PACMAN_PKGS
elif command -v zypper >/dev/null 2>&1; then
    # shellcheck disable=SC2086
    install_pkgs "zypper" zypper install -y $ZYPPER_PKGS
else
    print_info "Unknown package manager — install these yourself, then re-run:"
    print_info "  PyGObject with the Atspi-2.0 and Gtk-3.0 typelibs, a C compiler, curl"
fi

# The bindings are checked on the DISTRO python: that is the copy the venv
# will be built on (step 2), and the one element.py locates from inside it.
if python3 -c "import gi; gi.require_version('Atspi', '2.0'); gi.require_version('Gdk', '3.0'); from gi.repository import Atspi, Gdk" >/dev/null 2>&1; then
    print_ok "AT-SPI bindings available to $(command -v python3)"
else
    print_error "AT-SPI bindings not found for $(command -v python3)."
    print_info  "Computer use needs them: install the packages above and re-run."
fi

# -----------------------------------------------------------------------------
# Step 1 — uv
# -----------------------------------------------------------------------------
# uv resolves an existing Python or downloads one itself, and replaces pip for
# the install in step 3 (same PyPI packages, much faster, whole tree resolved
# at once). It also sidesteps python3-venv/ensurepip, which Debian and Ubuntu
# leave out of the default install — `python3 -m venv` fails on a fresh box.
print_step "STEP 1: Checking for uv"

# Make sure the standard uv install locations are visible to this shell, in
# case uv was installed by a previous run but the user's shell profile hasn't
# been re-sourced yet.
export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"

if command -v uv >/dev/null 2>&1; then
    print_ok "Found uv at $(command -v uv) ($(uv --version))"
else
    print_info "uv not found — installing it (https://astral.sh/uv)"

    if ! UV_SH="$(mktemp "${TMPDIR:-/tmp}/uv-install.XXXXXXXX")"; then
        print_error "Could not create a temp file for the uv installer."
        exit 1
    fi
    if ! fetch https://astral.sh/uv/install.sh "$UV_SH" || ! sh "$UV_SH"; then
        rm -f "$UV_SH" || true
        print_error "Failed to install uv."
        print_info  "Check your internet connection and re-run this script."
        exit 1
    fi
    rm -f "$UV_SH" || true

    # The installer drops uv in $XDG_BIN_HOME or ~/.local/bin and appends that
    # to the shell profile — which does not affect the shell we're in now.
    export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"

    if ! command -v uv >/dev/null 2>&1; then
        print_error "uv was installed but isn't on PATH in this shell."
        print_info  "Open a new terminal and re-run this script."
        exit 1
    fi

    print_ok "Installed uv ($(uv --version))"
fi

# -----------------------------------------------------------------------------
# Step 2 — .venv/
# -----------------------------------------------------------------------------
print_step "STEP 2: Preparing $VENV_DIR/"

# An existing venv is only reusable if its interpreter is still inside the
# supported range — a leftover venv built on an unsupported Python would fail
# in step 3 with an unresolvable dependency tree instead of here.
if [ -x "$VENV_DIR/bin/python" ]; then
    if MIN="$MIN_PYTHON" MAX="$MAX_PYTHON" "$VENV_DIR/bin/python" -c 'import os, sys; bound = lambda k: tuple(int(p) for p in os.environ[k].split(".")); sys.exit(0 if bound("MIN") <= sys.version_info[:2] < bound("MAX") else 1)' 2>/dev/null; then
        print_info "$VENV_DIR/ already exists — reusing"
    else
        print_info "$VENV_DIR/ uses an unsupported Python — rebuilding it"
        rm -rf "$VENV_DIR"
    fi
fi

if [ ! -x "$VENV_DIR/bin/python" ]; then
    # The venv MUST be built on the distro's Python. The AT-SPI bindings come
    # from the distro's python3-gi, and element.py locates that copy from
    # inside the venv only when the interpreter's ABI matches. A uv-downloaded
    # Python would leave the scanner and controller without AT-SPI. So
    # --no-managed-python first, and a managed Python only as a last resort,
    # with the consequence spelled out.
    if uv venv --no-managed-python --python "$PYTHON_SPEC" --prompt "$VENV_PROMPT" "$VENV_DIR" 2>/dev/null; then
        print_ok "Created $VENV_DIR/ on the Python already installed on this machine"
    else
        print_info "No distro Python $PYTHON_SPEC found — letting uv fetch one"
        print_info "NOTE: computer use needs the distro Python (AT-SPI bindings)."
        print_info "      Install python3 $PYTHON_SPEC from your distro and re-run for that."
        if ! uv venv --python "$PYTHON_SPEC" --prompt "$VENV_PROMPT" "$VENV_DIR"; then
            print_error "Could not create a virtual environment."
            exit 1
        fi
        print_ok "Created $VENV_DIR/ with a uv-managed Python"
    fi
fi

VENV_PYTHON="$VENV_DIR/bin/python"
print_info "Python: $("$VENV_PYTHON" -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])')"

# -----------------------------------------------------------------------------
# Step 3 — install
# -----------------------------------------------------------------------------
print_step "STEP 3: Installing linux_requirements.txt"

if [ ! -f "linux_requirements.txt" ]; then
    print_error "linux_requirements.txt not found in $(pwd)"
    exit 1
fi

if ! uv pip install --python "$VENV_PYTHON" -r linux_requirements.txt; then
    print_error "Dependency installation failed."
    print_info  "Scroll up for the error, then run this script again."
    exit 1
fi

# -----------------------------------------------------------------------------
# Step 4 — Rust toolchain (for the web agent)
# -----------------------------------------------------------------------------
# The web agent is the one mode that isn't pure Python: AutoCua/web is a PyO3
# cdylib that AutoCua/web/agent/__init__.py compiles with cargo on first
# import. No cargo means
#     python main.py
# dies with "cargo not found" the moment web use is selected, so the toolchain
# belongs in setup rather than in a README step — same step as the macOS and
# Windows scripts.
#
# Nothing from here on is fatal. By this point the venv and every dependency
# are in place, and computer use, shell use and mobile use all run without
# cargo — a failed toolchain install should cost the user the web agent, not
# the whole setup.
print_step "STEP 4: Rust toolchain (web agent)"

# rustup installs into ~/.cargo/bin and appends that to the shell profile,
# which does not reach the shell we're in now — same situation as the uv
# install in step 1, same fix.
export PATH="$HOME/.cargo/bin:$PATH"

WEB_AGENT_READY=1

# Run cargo rather than look for it. ~/.cargo/bin/cargo is a symlink to the
# rustup proxy, created during rustup's self-install whether or not a
# toolchain was ever downloaded. An interrupted install leaves a cargo that
# exists and fails on every invocation — a `command -v cargo` gate would skip
# the install forever, so re-running this script could never repair it.
have_cargo() { cargo --version >/dev/null 2>&1; }

# Checked before rustup, not after: cargo links through cc, and ring (the
# SHA-256 that stages the browser extension) compiles C sources, so a machine
# without a C compiler cannot build this crate no matter how well Rust
# installs. Failing here beats failing later inside a cargo error nobody can
# read.
if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
    print_error "No C compiler found — cargo needs one to link the web agent."
    print_info  "Install build-essential (step 0) and re-run this script."
    print_info  "Everything else is already installed."
    WEB_AGENT_READY=0
fi

# A rustup that is installed but has no default toolchain is repaired in place.
if [ "$WEB_AGENT_READY" = "1" ] && ! have_cargo && command -v rustup >/dev/null 2>&1; then
    print_info "rustup is installed but has no usable default toolchain — installing stable"
    rustup toolchain install stable || true
    rustup default stable || true
fi

if [ "$WEB_AGENT_READY" = "1" ] && ! have_cargo; then
    print_info "cargo not found — installing the Rust toolchain (https://rustup.rs)"

    # The assignment is guarded: `set -e` DOES abort on a bare assignment whose
    # command substitution fails, which would kill a step documented above as
    # never fatal.
    if ! RUSTUP_SH="$(mktemp "${TMPDIR:-/tmp}/rustup-init.XXXXXXXX")"; then
        print_error "Could not create a temp file for the Rust installer."
        print_info  "Install Rust yourself from https://rustup.rs, then re-run this script."
        WEB_AGENT_READY=0
    else
        # --profile minimal: rustc + cargo + std, no docs/clippy/rustfmt. -y
        # keeps it non-interactive. The profile edit it makes is deliberate —
        # the loader re-runs cargo whenever a .rs file changes, so later shells
        # need cargo too.
        if fetch https://sh.rustup.rs "$RUSTUP_SH" && sh "$RUSTUP_SH" -y --profile minimal; then
            export PATH="$HOME/.cargo/bin:$PATH"
            print_ok "Installed the Rust toolchain"
        else
            print_error "Could not install Rust."
            print_info  "Install it yourself from https://rustup.rs, then re-run this script."
            WEB_AGENT_READY=0
        fi
        rm -f "$RUSTUP_SH" || true
    fi
fi

if [ "$WEB_AGENT_READY" = "1" ]; then
    # Captured, not interpolated inline: a cargo that exits non-zero prints
    # nothing, and `print_ok "Found $(cargo --version)"` would report the empty
    # string as success.
    CARGO_VERSION="$(cargo --version 2>/dev/null || true)"
    if [ -n "$CARGO_VERSION" ]; then
        print_ok "Found $CARGO_VERSION"
        print_info "Using: $(command -v cargo)"
    else
        print_error "cargo still isn't usable in this shell."
        print_info  "Open a new terminal and re-run this script."
        WEB_AGENT_READY=0
    fi
fi

# -----------------------------------------------------------------------------
# Step 5 — build the web agent
# -----------------------------------------------------------------------------
# Built here rather than left to the first "web use" run, so a compile error
# surfaces during setup while the user is still watching, instead of minutes
# into an agent run. Importing the package is the build: the loader compiles
# whenever the .so is missing or older than any .rs source.
print_step "STEP 5: Building the web agent"

if [ "$WEB_AGENT_READY" = "1" ]; then
    print_info "The first build takes a few minutes..."
    if "$VENV_PYTHON" -c "import AutoCua.web.agent"; then
        print_ok "Web agent built"
    else
        print_error "The web agent did not build."
        print_info  "Everything else still works — computer use, shell use and"
        print_info  "mobile use don't need it. Fix the error above and re-run."
    fi
else
    print_info "Skipped — see the step above."
    print_info "Computer use, shell use and mobile use are unaffected."
fi

# -----------------------------------------------------------------------------
# Done
# -----------------------------------------------------------------------------
print_step "SETUP COMPLETE"
print_ok "Dependencies installed into $VENV_DIR/"
printf "\n"
print_info "Next steps:"
printf "    source %s/bin/activate\n" "$VENV_DIR"
[ -f "main.py" ] && printf "    python main.py                              # run the agent\n"
printf "\n"
print_info "Self-checks for computer use (optional):"
printf "    python -m AutoCua.linux.tree.test 0           # scan the focused window\n"
printf "    python -m AutoCua.linux.controller.test       # click + typing through the portal\n"
printf "\n"
print_info "The first computer-use run asks for screen-share consent (GNOME's"
print_info "RemoteDesktop dialog). Click Share once; the token is saved after that."
printf "\n"
