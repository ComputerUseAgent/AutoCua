#!/bin/bash
#
# AutoCua — iOS setup (WebDriverAgent)
# ====================================
# Fetches WebDriverAgent and verifies everything the iOS connector needs to
# sign, build and install it onto a real device.
#
# WebDriverAgent is third-party software (Facebook, Inc. / the Appium project,
# BSD 3-Clause). It is deliberately NOT vendored in this repo — this script
# clones it from the Appium project at a pinned tag, so you get it from its
# authors rather than from us. See THIRD_PARTY_NOTICES.md.
#
# iOS support is OPTIONAL. Run setup_mac.sh first (it creates .venv/ and
# installs the desktop Python dependencies); only run this one if you want
# AutoCua to drive an iPhone or iPad.
#
# Python packages for iOS are standalone in requirements_ios.txt (full app stack
# + device libs — not shared with requirements_mac.txt). This script installs
# them into .venv/. To install them yourself instead:
#   source .venv/bin/activate
#   uv pip install --python .venv/bin/python -r requirements_ios.txt
#   # or:  pip install -r requirements_ios.txt
#
# How to run (any of these):
#   bash setup_ios.sh            # simplest
#   bash setup_ios.sh --force    # discard the existing clone and re-fetch
#   bash setup_ios.sh --yes      # don't ask before installing Xcode's iOS platform
#   chmod +x setup_ios.sh && ./setup_ios.sh
#
# After it finishes, the iOS connector UI does the signing — either from the
# desktop app (Settings > Connect Device > iPhone) or standalone:
#   source .venv/bin/activate
#   python AutoCua/ios_connector/setup.py
#

set -e

cd "$(dirname "$0")"

# Pinned on purpose. WebDriverAgent ships breaking changes across majors (v15.0.0
# alone carried several), and AutoCua/ios_connector/setup.py targets a specific
# scheme and target list — WebDriverAgentLib / WebDriverAgentRunner /
# IntegrationApp. Tracking upstream's default branch would hand new users a
# version this connector was never tested against. Bump this only after testing.
WDA_VERSION="v15.1.1"
WDA_REPO="https://github.com/appium/WebDriverAgent.git"
WDA_DIR="AutoCua/ios_connector/WebDriverAgent"
VENV_DIR=".venv"
IOS_REQUIREMENTS="requirements_ios.txt"

FORCE=0
ASSUME_YES=0
for _arg in "$@"; do
    case "$_arg" in
        --force)   FORCE=1 ;;
        --yes|-y)  ASSUME_YES=1 ;;
    esac
done

# -----------------------------------------------------------------------------
# Print helpers (match the style used across the project's build scripts)
# -----------------------------------------------------------------------------
print_step()   { printf "\n============================================================\n  %s\n============================================================\n\n" "$1"; }
print_ok()     { printf "  [OK] %s\n" "$1"; }
print_info()   { printf "  [INFO] %s\n" "$1"; }
print_warn()   { printf "  [!] %s\n" "$1"; }
print_error()  { printf "  [ERROR] %s\n" "$1"; }

# GUI popup so failures are visible even when launched from Finder
gui_alert() {
    osascript -e "display dialog \"$1\" buttons {\"OK\"} default button 1 with icon caution with title \"AutoCua iOS setup\"" >/dev/null 2>&1 || true
}

# Soft requirements are collected rather than aborting immediately, so one run
# reports everything that needs fixing instead of one thing per run.
MISSING_TOOLING=0

# -----------------------------------------------------------------------------
# Step 1 — host checks (hard blockers)
# -----------------------------------------------------------------------------
print_step "STEP 1: Checking this Mac"

if [ "$(uname -s)" != "Darwin" ]; then
    print_error "iOS setup only works on macOS — signing an iOS app requires Xcode."
    exit 1
fi
print_ok "macOS $(sw_vers -productVersion)"

if ! command -v git >/dev/null 2>&1; then
    print_error "git not found."
    print_info  "Install the Command Line Tools, then re-run:  xcode-select --install"
    gui_alert "git was not found.\n\nRun:  xcode-select --install\n\nThen run this script again."
    exit 1
fi
print_ok "git $(git --version | awk '{print $3}')"

# xcodebuild must come from a full Xcode.app. The Command Line Tools alone
# provide a stub that cannot build or sign an iOS app, and it fails much later
# with a confusing error if we let it through here.
if ! XCODE_VER="$(xcodebuild -version 2>&1 | head -1)"; then
    print_error "xcodebuild failed: $XCODE_VER"
    if printf '%s' "$XCODE_VER" | grep -qi "license"; then
        print_info "Accept the Xcode license first:  sudo xcodebuild -license accept"
    else
        print_info "Install Xcode from the App Store, then run:"
        print_info "  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
    fi
    gui_alert "Xcode is not usable yet.\n\n$XCODE_VER\n\nSee the Terminal output for the fix."
    exit 1
fi

XCODE_PATH="$(xcode-select -p 2>/dev/null || true)"
case "$XCODE_PATH" in
    *CommandLineTools*)
        print_error "xcode-select points at the Command Line Tools, not a full Xcode."
        print_info  "Install Xcode from the App Store, then run:"
        print_info  "  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        gui_alert "AutoCua needs full Xcode, not just the Command Line Tools.\n\nInstall Xcode, then run:\nsudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        exit 1
        ;;
esac
print_ok "$XCODE_VER  ($XCODE_PATH)"

# -----------------------------------------------------------------------------
# Step 2 — fetch WebDriverAgent
# -----------------------------------------------------------------------------
print_step "STEP 2: Fetching WebDriverAgent $WDA_VERSION"

print_info "Source:  $WDA_REPO"
print_info "License: BSD 3-Clause (Facebook, Inc. / Appium) — not covered by"
print_info "         AutoCua's MIT license. See THIRD_PARTY_NOTICES.md."

if [ -d "$WDA_DIR" ] && [ $FORCE -eq 1 ]; then
    print_info "--force given — removing the existing clone"
    rm -rf "$WDA_DIR"
fi

if [ -d "$WDA_DIR/WebDriverAgent.xcodeproj" ]; then
    if [ -d "$WDA_DIR/.git" ]; then
        WDA_ACTUAL="$(git -C "$WDA_DIR" describe --tags --always 2>/dev/null || echo unknown)"
        print_ok "Already present at $WDA_DIR (version: $WDA_ACTUAL) — reusing"
        if [ "$WDA_ACTUAL" != "$WDA_VERSION" ]; then
            print_warn "That is not the pinned $WDA_VERSION."
            print_info  "Re-fetch the tested version with:  bash setup_ios.sh --force"
        fi
    else
        # A hand-placed copy. Report it as unverified rather than claiming the
        # pinned version — we have no way to know what it actually is.
        WDA_ACTUAL="unverified (existing copy, not a git clone)"
        print_ok "Already present at $WDA_DIR (not a git clone) — reusing"
        print_info "Replace it with the pinned clone using:  bash setup_ios.sh --force"
    fi
    print_info "Note: re-fetching resets project.pbxproj, so signing must be redone."
else
    # --depth 1 on a single tag: ~4MB of working tree instead of the full history.
    if ! git clone --depth 1 --branch "$WDA_VERSION" "$WDA_REPO" "$WDA_DIR"; then
        print_error "Could not clone WebDriverAgent."
        print_info  "Check your internet connection, or clone it manually:"
        print_info  "  git clone --depth 1 --branch $WDA_VERSION $WDA_REPO $WDA_DIR"
        gui_alert "Could not download WebDriverAgent.\n\nCheck your internet connection, then run this script again."
        exit 1
    fi

    if [ ! -d "$WDA_DIR/WebDriverAgent.xcodeproj" ]; then
        print_error "Clone finished but $WDA_DIR/WebDriverAgent.xcodeproj is missing."
        print_info  "Upstream layout may have changed for tag $WDA_VERSION."
        exit 1
    fi
    WDA_ACTUAL="$WDA_VERSION"
    print_ok "Cloned WebDriverAgent $WDA_VERSION into $WDA_DIR"
fi

# -----------------------------------------------------------------------------
# Step 3 — signing toolchain
# -----------------------------------------------------------------------------
# ios_connector/setup.py rewrites WebDriverAgent.xcodeproj/project.pbxproj
# through Ruby's xcodeproj gem to switch the targets to automatic signing. No
# gem, no signing — so this is genuinely required, not a nicety.
print_step "STEP 3: Checking the signing toolchain"

if ! command -v ruby >/dev/null 2>&1; then
    print_error "ruby not found — it normally ships with macOS."
    MISSING_TOOLING=1
elif ruby -e "require 'xcodeproj'" >/dev/null 2>&1; then
    print_ok "ruby $(ruby -e 'print RUBY_VERSION') with the xcodeproj gem"
else
    print_info "The Ruby 'xcodeproj' gem is missing — installing it now."
    # --user-install, deliberately NOT sudo: this lands in ~/.gem, which is
    # already on Ruby's search path, instead of writing into the system Ruby
    # that macOS owns and replaces on update. Every dependency is pure Ruby,
    # so there is nothing to compile and nothing to elevate for.
    printf "\n"
    if gem install --user-install xcodeproj; then
        printf "\n"
        if ruby -e "require 'xcodeproj'" >/dev/null 2>&1; then
            print_ok "xcodeproj installed in $(ruby -e 'require "rubygems"; print Gem.user_dir')"
        else
            print_warn "The gem installed but ruby still cannot load it."
            print_info  "Something is overriding the gem path — compare:"
            printf "    gem env | grep -A3 'GEM PATHS'\n"
            MISSING_TOOLING=1
        fi
    else
        printf "\n"
        print_warn "Could not install the xcodeproj gem — signing cannot run without it."
        print_info  "Try it by hand:  gem install --user-install xcodeproj"
        MISSING_TOOLING=1
    fi
fi

# -----------------------------------------------------------------------------
# Step 4 — device tooling (Python packages from requirements_ios.txt)
# -----------------------------------------------------------------------------
print_step "STEP 4: Checking device tooling"

if [ ! -f "$IOS_REQUIREMENTS" ]; then
    print_error "$IOS_REQUIREMENTS not found in $(pwd)"
    print_info  "That file lists the optional iOS Python packages for this script."
    MISSING_TOOLING=1
elif [ -x "$VENV_DIR/bin/python" ]; then
    if "$VENV_DIR/bin/python" -c "import pymobiledevice3" >/dev/null 2>&1; then
        print_ok "pymobiledevice3 available in $VENV_DIR/"
    else
        print_info "Installing iOS Python packages from $IOS_REQUIREMENTS …"
        if command -v uv >/dev/null 2>&1; then
            if uv pip install --python "$VENV_DIR/bin/python" -r "$IOS_REQUIREMENTS"; then
                print_ok "Installed packages from $IOS_REQUIREMENTS"
            else
                print_warn "uv pip install -r $IOS_REQUIREMENTS failed."
                MISSING_TOOLING=1
            fi
        elif "$VENV_DIR/bin/python" -m pip install -r "$IOS_REQUIREMENTS"; then
            print_ok "Installed packages from $IOS_REQUIREMENTS (via pip)"
        else
            print_warn "Could not install $IOS_REQUIREMENTS."
            print_info  "Install manually with:"
            print_info  "  uv pip install --python $VENV_DIR/bin/python -r $IOS_REQUIREMENTS"
            print_info  "  # or:  $VENV_DIR/bin/python -m pip install -r $IOS_REQUIREMENTS"
            MISSING_TOOLING=1
        fi
        if [ $MISSING_TOOLING -eq 0 ] \
            && ! "$VENV_DIR/bin/python" -c "import pymobiledevice3" >/dev/null 2>&1; then
            print_warn "Install finished but pymobiledevice3 still does not import."
            MISSING_TOOLING=1
        fi
    fi
else
    print_warn "$VENV_DIR/ not found — run setup_mac.sh first."
    print_info  "  bash setup_mac.sh"
    print_info  "Then either re-run this script, or install iOS packages alone with:"
    print_info  "  uv pip install --python $VENV_DIR/bin/python -r $IOS_REQUIREMENTS"
    MISSING_TOOLING=1
fi

# -----------------------------------------------------------------------------
# Simulator preflight — can Xcode actually TARGET a simulator?
#
# `xcrun simctl` reads the system-wide CoreSimulator runtime store, so it can
# list and boot devices that the SELECTED Xcode has no platform support for.
# xcodebuild then refuses those devices ("Unable to find a destination...")
# halfway through an agent run. One second here beats that every time.
# -----------------------------------------------------------------------------
print_step "CHECKING SIMULATOR SUPPORT"

# Whether Xcode can BUILD to a simulator, which is a different question from
# whether simctl can boot one — and the only one that matters here, because
# simulation mode compiles WebDriverAgent onto the device.
sim_buildable() {
    xcodebuild -project "$WDA_DIR/WebDriverAgent.xcodeproj" \
        -scheme WebDriverAgentRunner -showdestinations 2>/dev/null \
        | grep -q "platform:iOS Simulator, arch"
}

if sim_buildable; then
    print_ok "Xcode can build for the iOS Simulator"
else
    print_warn "Xcode cannot target any iOS Simulator on this Mac."
    print_info "\`xcrun simctl\` may still list and boot simulators — that store is"
    print_info "system-wide — but the selected Xcode has no iOS platform support,"
    print_info "so builds fail with \"Unable to find a destination\"."
    printf "\n"
    print_info "The missing piece is Xcode's iOS platform: an ~8.5 GB download from"
    print_info "Apple, once. This is the right moment for it — the agent will never"
    print_info "start a download of this size mid-run."
    printf "\n"

    # Setup is where a big one-time install belongs, so offer it here. Anything
    # non-interactive (CI, a piped run) declines and reports instead of hanging.
    DO_INSTALL=0
    if [ "$ASSUME_YES" -eq 1 ]; then
        DO_INSTALL=1
    elif [ -t 0 ]; then
        printf "  Download and install it now? [Y/n] "
        read -r REPLY_INSTALL || REPLY_INSTALL=""
        case "$REPLY_INSTALL" in
            ""|y|Y|yes|YES|Yes) DO_INSTALL=1 ;;
        esac
    fi

    if [ $DO_INSTALL -eq 1 ]; then
        print_info "Running: xcodebuild -downloadPlatform iOS"
        printf "\n"
        if xcodebuild -downloadPlatform iOS; then
            printf "\n"
            if sim_buildable; then
                print_ok "iOS platform installed — Xcode can build for the iOS Simulator"
            else
                print_warn "The download finished but Xcode still targets no simulator."
                print_info "Check that xcode-select points at the Xcode you expect:"
                printf "    xcode-select -p\n"
                printf "    sudo xcodebuild -runFirstLaunch\n"
                MISSING_TOOLING=1
            fi
        else
            printf "\n"
            print_warn "xcodebuild -downloadPlatform iOS did not complete."
            print_info "Re-run this script to try again, or install it from"
            print_info "Xcode > Settings > Components."
            MISSING_TOOLING=1
        fi
    else
        print_info "Skipped. Install it later with either of:"
        printf "    bash setup_ios.sh --yes               # re-run and install without asking\n"
        printf "    xcodebuild -downloadPlatform iOS      # or Xcode > Settings > Components\n"
        printf "    xcode-select -p                       # confirm the right Xcode is selected\n"
        printf "\n"
        print_info "Simulation mode (the default) needs this; hardware-only use does not."
        MISSING_TOOLING=1
    fi
fi

# -----------------------------------------------------------------------------
# Done
# -----------------------------------------------------------------------------
if [ $MISSING_TOOLING -ne 0 ]; then
    print_step "ACTION REQUIRED"
    print_warn "WebDriverAgent is in place, but the items flagged above must be"
    print_warn "fixed before an iPhone can be driven."
    printf "\n"
    print_info "Fix them, then re-run this script — it reuses the existing clone,"
    print_info "so the second run is instant."
    printf "\n"
    exit 1
fi

print_step "iOS SETUP COMPLETE"
print_ok "WebDriverAgent ready at $WDA_DIR  (version: $WDA_ACTUAL)"
printf "\n"
print_info "Next steps — open the sign & run UI, either way works:"
printf "    source %s/bin/activate\n" "$VENV_DIR"
printf "    python app.py                              # then Settings > Connect Device > iPhone\n"
printf "    python AutoCua/ios_connector/setup.py     # or standalone, on port 8765\n"
printf "\n"
print_info "To sign and install onto the device you will need:"
printf "    - an Apple ID added in Xcode (Settings > Accounts)\n"
printf "    - your iPhone connected by USB, unlocked, and trusting this Mac\n"
printf "\n"
print_info "A free Apple ID works; its provisioning profiles expire after 7 days,"
print_info "so re-signing is needed weekly. A paid developer account lasts a year."
printf "\n"
