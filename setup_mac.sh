#!/bin/bash
#
# AutoCua — macOS one-click setup
# ================================
# Installs uv (if missing), creates a local .venv/, and installs
# requirements_mac.txt into it. No manual Python install required — uv will
# fetch a Python for you if this Mac doesn't already have a suitable one.
# Then installs the Rust toolchain (if missing) and builds the web agent's
# native extension, so the first "web use" run doesn't stop on a missing cargo.
#
# How to run (any of these):
#   bash setup_mac.sh          # simplest, works right after clone
#   chmod +x setup_mac.sh && ./setup_mac.sh
#   Finder → right-click setup_mac.sh → Open With → Terminal.app
#
# After it finishes:
#   source .venv/bin/activate
#   python app.py                # launch the GUI app (macOS + Windows)
#

set -e

cd "$(dirname "$0")"

MIN_PYTHON="3.10"
# Exclusive upper bound. Some dependencies ship native wheels that lag behind
# the newest CPython (paddlepaddle, for one, stops at cp313), so a Mac whose
# newest interpreter is 3.14 must not be used for the venv.
MAX_PYTHON="3.14"
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

# GUI popup so failures are visible even when launched from Finder
gui_alert() {
    osascript -e "display dialog \"$1\" buttons {\"OK\"} default button 1 with icon caution with title \"AutoCua setup\"" >/dev/null 2>&1 || true
}

# -----------------------------------------------------------------------------
# Step 1 — uv
# -----------------------------------------------------------------------------
# NOTE: Previous versions of this script required the user to install Python
# from python.org by hand before setup could proceed. uv removes that step —
# it resolves an existing Python or downloads one itself. It also replaces pip
# for the install in step 3 (same PyPI packages, much faster, and it resolves
# the whole tree at once instead of one package at a time).
#
# The old "sync shared files to macOS flavor" step is long gone too — main.py,
# cli.py, frontend/index.html and frontend/script.js detect the OS at runtime,
# so a single checkout runs on both macOS and Windows with zero file patching.
print_step "STEP 1: Checking for uv"

# Make sure the standard uv install locations are visible to this shell, in
# case uv was installed by a previous run but the user's shell profile hasn't
# been re-sourced yet.
export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"

if command -v uv >/dev/null 2>&1; then
    print_ok "Found uv at $(command -v uv) ($(uv --version))"
else
    print_info "uv not found — installing it (https://astral.sh/uv)"

    if ! curl -LsSf https://astral.sh/uv/install.sh | sh; then
        print_error "Failed to install uv."
        print_info  "Check your internet connection and re-run this script."
        gui_alert "Could not install uv.\n\nCheck your internet connection, then run this script again."
        exit 1
    fi

    # The installer drops uv in \$XDG_BIN_HOME or ~/.local/bin and appends that
    # to the shell profile — which does not affect the shell we're in now.
    export PATH="${XDG_BIN_HOME:-$HOME/.local/bin}:$PATH"

    if ! command -v uv >/dev/null 2>&1; then
        print_error "uv was installed but isn't on PATH in this shell."
        print_info  "Open a new Terminal window and re-run this script."
        gui_alert "uv was installed, but this Terminal session can't see it yet.\n\nOpen a new Terminal window and run the script again."
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
    # Prefer a Python already on this Mac (--no-managed-python), so the venv
    # keeps using the interpreter the user already has. Only if nothing here
    # satisfies $PYTHON_SPEC do we let uv download one.
    if uv venv --no-managed-python --python "$PYTHON_SPEC" --prompt "$VENV_PROMPT" "$VENV_DIR" 2>/dev/null; then
        print_ok "Created $VENV_DIR/ using a Python already installed on this Mac"
    else
        print_info "No local Python $PYTHON_SPEC found — letting uv fetch one"
        if ! uv venv --python "$PYTHON_SPEC" --prompt "$VENV_PROMPT" "$VENV_DIR"; then
            print_error "Could not create a virtual environment."
            gui_alert "Could not create the Python environment.\n\nCheck your internet connection, then run this script again."
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
print_step "STEP 3: Installing requirements_mac.txt"

if [ ! -f "requirements_mac.txt" ]; then
    print_error "requirements_mac.txt not found in $(pwd)"
    exit 1
fi

if ! uv pip install --python "$VENV_PYTHON" -r requirements_mac.txt; then
    print_error "Dependency installation failed."
    gui_alert "Installing dependencies failed.\n\nScroll up in Terminal for the error, then run this script again."
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
# belongs in setup rather than in a README step. setup_windows.bat has done
# this since it shipped; this is the macOS half of the same step.
#
# Unlike the Windows script, nothing from here on is fatal. By this point the
# venv and every dependency are in place, and computer use, shell use and
# mobile use all run without cargo — a failed toolchain install should cost the
# user the web agent, not the whole setup.
print_step "STEP 4: Rust toolchain (web agent)"

# rustup installs into ~/.cargo/bin and appends that to the shell profile,
# which does not reach the shell we're in now — same situation as the uv
# install in step 1, same fix.
export PATH="$HOME/.cargo/bin:$PATH"

WEB_AGENT_READY=1

# Run cargo rather than look for it. ~/.cargo/bin/cargo is not a binary, it is
# a symlink to the rustup proxy, and rustup creates that symlink during its own
# self-install whether or not a toolchain was ever downloaded. An interrupted
# install, or a rustup set up with `--default-toolchain none`, leaves a cargo
# that exists and fails on every invocation — and a `command -v cargo` gate
# would skip the install forever, so re-running this script could never repair
# it. Worse, the PATH prepend above puts that broken shim in front of a working
# Homebrew cargo.
have_cargo() { cargo --version >/dev/null 2>&1; }

# Checked before rustup, not after: cargo links through Apple's cc, and ring
# (the SHA-256 that stages the browser extension) compiles C sources, so a Mac
# without the Command Line Tools cannot build this crate no matter how well
# Rust installs. Failing here beats failing later inside a cargo error nobody
# can read.
if ! xcode-select -p >/dev/null 2>&1; then
    print_info "Command Line Tools not found — cargo needs them to link."
    print_info "Opening Apple's installer. Accept it, let it finish, then re-run this script."
    xcode-select --install >/dev/null 2>&1 || true
    gui_alert "The web agent needs Apple's Command Line Tools.\n\nAccept the installer that just opened, then run this script again.\n\nEverything else is already installed."
    WEB_AGENT_READY=0
fi

# A rustup that is installed but has no default toolchain is repaired in place.
# Re-downloading rustup-init would work too, but this is the common leftover
# state and one command fixes it.
if [ "$WEB_AGENT_READY" = "1" ] && ! have_cargo && command -v rustup >/dev/null 2>&1; then
    print_info "rustup is installed but has no usable default toolchain — installing stable"
    rustup toolchain install stable || true
    rustup default stable || true
fi

if [ "$WEB_AGENT_READY" = "1" ] && ! have_cargo; then
    print_info "cargo not found — installing the Rust toolchain (https://rustup.rs)"

    # Downloaded to a file rather than piped straight into sh: in
    # `curl ... | sh`, a curl that dies mid-transfer still leaves sh reading a
    # truncated script and exiting 0, so the install looks like it worked.
    #
    # The template is spelled out instead of using `-t rustup-init`, because
    # GNU coreutils' mktemp — which Homebrew's gnubin opt-in puts ahead of
    # /usr/bin/mktemp — rejects that form for having too few X's. And the
    # assignment is guarded: `set -e` DOES abort on a bare assignment whose
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
        if curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o "$RUSTUP_SH" \
           && sh "$RUSTUP_SH" -y --profile minimal; then
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
        print_info  "Open a new Terminal window and re-run this script."
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
[ -f "main.py" ]             && printf "    python main.py               # run the CLI agent\n"
[ -f "app.py" ]              && printf "    python app.py                # launch the GUI app\n"
[ -f "mac_binary_build.py" ] && printf "    python mac_binary_build.py   # produce AutoCua.dmg\n"
printf "\n"
# iOS is optional and needs Xcode, so it lives in its own script rather than
# making every install pay for a WebDriverAgent clone it may never use.
if [ -f "setup_ios.sh" ]; then
    print_info "Want to drive an iPhone or iPad? That needs Xcode and one extra step:"
    printf "    bash setup_ios.sh            # fetches WebDriverAgent, checks the toolchain\n"
    printf "\n"
fi
