"""Build and run Win_CUA.rs, the second desktop for the agent.

From D:\\AutoCua:
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\test.py
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\test.py --computer-use
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\test.py --run APP --scan scan.png

Everything after test.py goes to the program as it is: the top of Win_CUA.rs
says what it takes. The program is built with rustc on the first run, and
again whenever Win_CUA.rs changes, as Win_CUA.exe beside it. Every picture it
saves goes to the debug folder at the root, D:\\AutoCua\\debug.
"""
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE / "Win_CUA.rs"
PROGRAM = HERE / "Win_CUA.exe"


def build():
    """Compile Win_CUA.rs when there is no program yet or the source is newer."""
    if PROGRAM.exists() and PROGRAM.stat().st_mtime >= SOURCE.stat().st_mtime:
        return
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / "rustc.exe")
    if not Path(rustc).exists():
        raise SystemExit("rustc not found: run setup_windows.bat, or install Rust from https://rustup.rs")
    print("Building Win_CUA.rs...", flush=True)
    if subprocess.call([rustc, "--edition", "2021", "-O", str(SOURCE), "-o", str(PROGRAM)], cwd=HERE) != 0:
        raise SystemExit("Win_CUA.rs did not build")


def main():
    if sys.platform != "win32":
        raise SystemExit("Win_CUA is for Windows only")
    build()
    try:
        return subprocess.call([str(PROGRAM), *sys.argv[1:]])
    except KeyboardInterrupt:   # Ctrl+C ended the program too, and with it every app on the second desktop
        return 130


if __name__ == "__main__":
    sys.exit(main())
