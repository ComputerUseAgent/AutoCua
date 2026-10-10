"""Build the web agent's Rust extension if it isn't built yet.

The whole web side is ONE crate rooted at AutoCua/web/: Cargo.toml, target/
and the built agent_native.so all live there, and the .rs sources sit in their
own folders (agent/, browser/, controller/, tool_registry/, tree/) mirroring the
old Python layout. tree/element.rs is the page scanner; it used to build as a
second binary and run as a subprocess, and is a plain module of this crate now.
It compiles with plain `cargo build --release` — no maturin, no pyproject.toml
— one cargo build for the whole web side.

WHEN THIS ACTUALLY RUNS
    Checkout  — Cargo.toml sits beside the package, so this compiles on the
                first `web use` and on any later run where a .rs source is
                newer than the built module.
    pip wheel — the extension ships prebuilt (agent_native.abi3.so) and the
                crate sources are excluded, so there is no Cargo.toml to find
                and cargo is never touched.

Lives in utils/ beside wda.py because they are the same kind of thing: the
one-time setup step a checkout gets from a shell script and a pip install has
to do for itself.
"""

import os
import shutil
import subprocess
import sys
from pathlib import Path

_WEB_DIR = Path(__file__).resolve().parent.parent / "web"

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

_SO = _WEB_DIR / f"agent_native{_EXT_SUFFIX}"
_DYLIB = _WEB_DIR / "target" / "release" / _CARGO_ARTIFACT
_MANIFEST = _WEB_DIR / "Cargo.toml"

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

# Directories under web/ that hold no crate sources.
_NOT_SOURCE = {"target", "tests", "scratchpad", "__pycache__"}


def _rust_sources():
    """Every .rs the crate compiles, wherever under web/ it lives.

    Walked rather than listed folder by folder. A hand-written list goes stale
    the moment a source moves, and it fails SILENTLY — the extension simply
    stops rebuilding and every later import loads a .so that no longer matches
    the code. browser/browser.rs was exactly that: moved out of agent/, and
    with it out of the freshness check.
    """
    for entry in _WEB_DIR.iterdir():
        if entry.name in _NOT_SOURCE:
            continue
        if entry.is_dir():
            yield from entry.rglob("*.rs")
        elif entry.suffix == ".rs":
            yield entry


def ensure_web_agent_built() -> None:
    """Compile the extension once — a minute on first use, then never again.

    A no-op from a pip wheel: no Cargo.toml means the extension is already
    built and shipped, so nothing here has anything to do.
    """
    if not _MANIFEST.is_file():
        return

    # Everything the crate compiles — including tree/element.rs, which builds
    # as the `element` binary target of this same crate (one cargo build, one
    # target/, both artifacts).
    sources = [_MANIFEST, *_rust_sources()]
    stamps = [p.stat().st_mtime for p in sources if p.exists()]
    if _SO.exists() and stamps and _SO.stat().st_mtime >= max(stamps):
        return

    # shutil.which applies PATHEXT itself, so it already finds cargo.exe; only
    # the rustup-default fallback path needs the suffix spelled out.
    cargo_exe = "cargo.exe" if sys.platform == "win32" else "cargo"
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / cargo_exe)
    if not Path(cargo).exists():
        raise RuntimeError(
            "cargo not found — the web agent is a Rust extension that builds "
            f"once from source. To fix it, {_SETUP_HINT}\n"
            "Every other mode (computer use, shell use, mobile use) runs "
            "without it."
        )

    # cargo links through the system C compiler, and ring (the SHA-256 that
    # stages the browser extension) compiles C sources — a Linux box straight from the
    # installer has neither. Caught here with a readable message rather than
    # minutes later as "linker `cc` not found" deep in cargo's output. macOS
    # always has /usr/bin/cc (Apple's shim, which offers the Command Line
    # Tools itself), so in practice this fires on Linux only.
    if sys.platform != "win32" and not any(shutil.which(c) for c in ("cc", "gcc", "clang")):
        apt = " (or just:  sudo apt install build-essential)" if sys.platform.startswith("linux") else ""
        raise RuntimeError(
            "no C compiler found — cargo needs one to link the web agent. "
            f"To fix it, {_SETUP_HINT}{apt}\n"
            "Every other mode (computer use, shell use, mobile use) runs "
            "without it."
        )

    print("Building the web agent (first run, ~1 min)...")
    # Pin the build to the interpreter that is importing us. pyo3-build-config
    # otherwise picks whatever "python" PATH resolves to, which in a venv that
    # was never activated is a different install than the one that will load
    # the result.
    env = dict(os.environ)
    env.setdefault("PYO3_PYTHON", sys.executable)
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
        [cargo, "build", "--release", "--manifest-path", str(_MANIFEST)],
        cwd=str(_WEB_DIR), env=env)
    if not _DYLIB.exists():
        raise RuntimeError(f"build succeeded but {_DYLIB} is missing")
    # Install through a NEW inode, never by overwriting the .so in place:
    # macOS keeps the previous code-signature blob registered on the old
    # vnode, and every process that then maps the rewritten file is
    # SIGKILLed ("zsh: killed", exit 137) with nothing in the log.
    tmp = _SO.with_name(_SO.name + ".new")
    shutil.copy2(_DYLIB, tmp)
    os.replace(tmp, _SO)
