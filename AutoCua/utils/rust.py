"""Build the Rust extensions (the web agent, the Android agent) if they aren't built yet.

Each side is ONE crate rooted at its folder, AutoCua/web/ and AutoCua/android/:
Cargo.toml, target/ and the built agent_native.so all live there, and the .rs
sources sit in their own folders (agent/, controller/, tool_registry/, tree/)
mirroring the old Python layout. Each compiles with plain
`cargo build --release` — no maturin, no pyproject.toml — one cargo build for
the whole side.

WHEN THIS ACTUALLY RUNS
    Checkout  — Cargo.toml sits beside the package, so this compiles when
                the desktop app starts (ui/service.py _start_native_agents),
                on a terminal run's first "web use" / "mobile use, android",
                and on any later run where a .rs source is newer than the
                built module.
    pip wheel — both extensions ship prebuilt. maturin builds the web one
                itself (agent_native.abi3.so / .pyd) and its crate sources
                are excluded, so there is no Cargo.toml to find and cargo is
                never touched for it. The Android one is built by THIS module
                on the release machine, before maturin runs, and
                pyproject.toml's `include` packs the result (agent_native.so
                / .pyd) into the wheel — note/pypi-release.md. Its crate
                sources ship too, so a wheel pip builds from the sdist, on a
                platform without one of ours, gets the module on its first
                Android run; a shipped module is trusted as it is (no
                target/ beside it), never rebuilt from pip's own mtimes.

Lives in utils/ beside wda.py because they are the same kind of thing: the
one-time setup step a checkout gets from a shell script and a pip install has
to do for itself.
"""

import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path

_PACKAGE = Path(__file__).resolve().parent.parent
_WEB_DIR = _PACKAGE / "web"
_ANDROID_DIR = _PACKAGE / "android"

# Two different naming schemes have to line up here, and neither is portable:
# Python only imports an extension named .pyd on Windows and .so elsewhere,
# while cargo names its cdylib agent_native.dll / libagent_native.dylib /
# libagent_native.so depending on the host. Resolve both per-platform instead
# of hardcoding the macOS pair.
if sys.platform == "win32":
    _EXT_SUFFIX, _CARGO_ARTIFACT = ".pyd", "agent_native.dll"
elif sys.platform == "darwin":
    _EXT_SUFFIX, _CARGO_ARTIFACT = ".so", "libagent_native.dylib"
else:
    _EXT_SUFFIX, _CARGO_ARTIFACT = ".so", "libagent_native.so"

# The setup script for this host — what every error below points at. Rather
# than straight at rustup.rs, because the scripts install the toolchain AND
# the platform's C compiler (cargo cannot link this crate without one) and
# then pre-build the extension, so the next run starts with everything in
# place.
if sys.platform == "win32":
    _SETUP_HINT = "run:  setup_windows.bat"
elif sys.platform == "darwin":
    _SETUP_HINT = "run:  bash setup_mac.sh"
elif sys.platform.startswith("linux"):
    _SETUP_HINT = "run:  bash setup_linux.sh"
else:
    _SETUP_HINT = "install Rust from https://rustup.rs and a C compiler"

# Directories under a crate that hold no crate sources.
_NOT_SOURCE = {"target", "tests", "scratchpad", "__pycache__"}


def _rust_sources(crate_dir):
    """Every .rs the crate compiles, wherever under its folder it lives.

    Walked rather than listed folder by folder. A hand-written list goes stale
    the moment a source moves, and it fails SILENTLY — the extension simply
    stops rebuilding and every later import loads a .so that no longer matches
    the code. browser/browser.rs was exactly that: moved out of agent/, and
    with it out of the freshness check.
    """
    for entry in crate_dir.iterdir():
        if entry.name in _NOT_SOURCE:
            continue
        if entry.is_dir():
            yield from entry.rglob("*.rs")
        elif entry.suffix == ".rs":
            yield entry


def _ensure_built(crate_dir, what, others):
    """Compile one crate's extension once — a minute on first use, then never again.

    what: the crate's name in messages ("web agent"); others: the modes that
    run without it, for the error text.

    A no-op from a pip wheel: no Cargo.toml means the extension is already
    built and shipped (the web crate), and a module with no target/ beside
    it was shipped built (the Android crate, whose sources come along), so
    nothing here has anything to do.
    """
    manifest = crate_dir / "Cargo.toml"
    if not manifest.is_file():
        return
    so = crate_dir / f"agent_native{_EXT_SUFFIX}"
    dylib = crate_dir / "target" / "release" / _CARGO_ARTIFACT

    # A shipped module, trusted as it is. Only a build here leaves target/
    # behind, so a module without one came in the wheel; pip gives every
    # file its install-time mtime, in archive order, so the stamps below
    # would call a shipped module stale and ask a pip user for cargo.
    if so.exists() and not (crate_dir / "target").exists():
        return

    # Everything the crate compiles, wherever under its folder it lives, and
    # what pins its dependencies and its compiler: a `cargo update` or a
    # toolchain bump changes the module too, with no .rs touched.
    sources = [manifest, crate_dir / "Cargo.lock", _PACKAGE.parent / "rust-toolchain.toml",
               *_rust_sources(crate_dir)]
    stamps = [p.stat().st_mtime for p in sources if p.exists()]
    if so.exists() and stamps and so.stat().st_mtime >= max(stamps):
        return

    # shutil.which applies PATHEXT itself, so it already finds cargo.exe; only
    # the rustup-default fallback path needs the suffix spelled out.
    cargo_exe = "cargo.exe" if sys.platform == "win32" else "cargo"
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / cargo_exe)
    if not Path(cargo).exists():
        raise RuntimeError(
            f"cargo not found — the {what} is a Rust extension that builds "
            f"once from source. To fix it, {_SETUP_HINT}\n"
            f"{others} run without it."
        )

    # cargo links through the system C compiler, and the web crate compiles C
    # sources of its own (ring's SHA-256) — a Linux box straight from the
    # installer has neither.
    # Caught here with a readable message rather than minutes later as
    # "linker `cc` not found" deep in cargo's output. macOS always has
    # /usr/bin/cc (Apple's shim, which offers the Command Line Tools itself),
    # so in practice this fires on Linux only.
    if sys.platform != "win32" and not any(shutil.which(c) for c in ("cc", "gcc", "clang")):
        apt = " (or just:  sudo apt install build-essential)" if sys.platform.startswith("linux") else ""
        raise RuntimeError(
            f"no C compiler found — cargo needs one to link the {what}. "
            f"To fix it, {_SETUP_HINT}{apt}\n"
            f"{others} run without it."
        )

    print(f"Building the {what} (first run, ~1 min)...")
    # Pin the build to the interpreter that is importing us. pyo3-build-config
    # otherwise picks whatever "python" PATH resolves to, which in a venv that
    # was never activated is a different install than the one that will load
    # the result.
    env = dict(os.environ)
    env.setdefault("PYO3_PYTHON", sys.executable)
    # Windows: rust-toolchain.toml at the repo root pins a version with no
    # triple, and rustup completes it with its default host, MSVC unless
    # setup_windows.bat set it. The MSVC build of that version cannot link
    # this crate (no Visual Studio), so the GNU one is named outright for a
    # checkout whose rustup predates the pin. Only x86-64: there is no GNU
    # target for Windows on ARM.
    pin = _PACKAGE.parent / "rust-toolchain.toml"
    if sys.platform == "win32" and platform.machine() in ("AMD64", "x86_64") and pin.is_file():
        m = re.search(r'^\s*channel\s*=\s*"([^"]+)"', pin.read_text(encoding="utf-8"), re.M)
        if m and "-" not in m.group(1):
            env.setdefault("RUSTUP_TOOLCHAIN", f"{m.group(1)}-x86_64-pc-windows-gnu")
    # macOS: the release profile's `strip = true` leaves a library that the
    # loader of macOS 26+ refuses ("mis-aligned LINKEDIT string pool", with
    # Xcode's current ld; re-signing does not help, "debuginfo" fails the same
    # way). Unstripped it loads, 0.8 MB bigger. Other platforms are untouched.
    if sys.platform == "darwin":
        env.setdefault("CARGO_PROFILE_RELEASE_STRIP", "false")
    # When cargo was found through the ~/.cargo/bin fallback rather than PATH
    # (a shell that never sourced rustup's profile edit), rustc and the rest
    # of the toolchain live beside it and are found the same way.
    env["PATH"] = os.pathsep.join([str(Path(cargo).parent), env.get("PATH", "")])
    subprocess.check_call(
        [cargo, "build", "--release", "--manifest-path", str(manifest)],
        cwd=str(crate_dir), env=env)
    if not dylib.exists():
        raise RuntimeError(f"build succeeded but {dylib} is missing")
    # Another thread or process installed this very build while our cargo
    # waited on cargo's lock (the app's start-up build vs an Android run, or
    # computer use's android_agent child). copy2 kept the dylib's mtime on
    # it, and on Windows a .pyd that is loaded cannot be replaced.
    if so.exists() and so.stat().st_mtime >= dylib.stat().st_mtime:
        return
    # Install through a NEW inode, never by overwriting the .so in place:
    # macOS keeps the previous code-signature blob registered on the old
    # vnode, and every process that then maps the rewritten file is
    # SIGKILLed ("zsh: killed", exit 137) with nothing in the log.
    tmp = so.with_name(so.name + ".new")
    shutil.copy2(dylib, tmp)
    os.replace(tmp, so)


def ensure_web_agent_built() -> None:
    """The web agent's extension (AutoCua/web/agent_native), built once."""
    _ensure_built(_WEB_DIR, "web agent",
                  "Every other mode (computer use, shell use, mobile use)")


def ensure_android_agent_built() -> None:
    """The Android agent's extension (AutoCua/android/agent_native), built once."""
    _ensure_built(_ANDROID_DIR, "Android agent",
                  "Every other mode (computer use, shell use, web use, iOS)")
