@echo off
setlocal enabledelayedexpansion

:: ============================================
::  AutoCua - Windows Dev Setup
:: ============================================
::  Installs uv, creates venv\ and installs requirements_windows.txt into it.
::  It needs no Administrator rights, installs no driver and never reboots.
::  Platform-shared files
::  (main.py, cli.py, ui/index.html, ui/script.js) detect the OS at
::  runtime, so no file patching is needed - one checkout runs on both macOS
::  and Windows as-is.
::
::  uv (https://astral.sh/uv) finds the Python already on this machine and
::  replaces pip for the install below - same PyPI packages, much faster, and
::  it resolves the whole tree at once instead of one package at a time. It
::  never downloads a Python (UV_PYTHON_DOWNLOADS=never): with no Python 3.11
::  to 3.13 installed, python.org's download page opens and the script stops,
::  so you install one and run it again. setup_mac.sh works the same way.

:: --- 1. No elevation ---
:: Nothing below needs Administrator rights: uv, Rust, mingw-w64 and the venv
:: all install for this user only. Only the Interception driver needed them,
:: and this script no longer installs it (see the end).

:: --- 2. Anchor to script location ---
cd /d "%~dp0"

:: uv never downloads a Python here, in any command (see step 5).
set "UV_PYTHON_DOWNLOADS=never"

echo.
echo ============================================
echo   AutoCua - Windows Setup
echo ============================================
echo.

:: --- 3. Sanity-check repo layout ---
:: Required: main.py, requirements_windows.txt.
:: The Interception driver is neither vendored nor installed here - the end of
:: this script says how to add it yourself (see THIRD_PARTY_NOTICES.md).
:: Optional (proprietary): AutoCua\windows - skipped if absent.
set "MISSING="
if not exist "main.py" set "MISSING=main.py"
if not exist "requirements_windows.txt" set "MISSING=requirements_windows.txt"

if defined MISSING (
    echo [ERROR] Required file not found: %MISSING%
    echo.
    echo Are you running this from the repo root? Expected layout:
    echo   ^<repo^>\main.py
    echo   ^<repo^>\requirements_windows.txt
    echo.
    pause
    exit /b 1
)

:: Optional proprietary pieces - informational only, not fatal.
if not exist "AutoCua\windows"     echo [i] AutoCua\windows not found - proprietary module absent.

:: --- 4. uv ---
echo [*] Checking for uv...

:: The official installer drops uv in %USERPROFILE%\.local\bin (older builds
:: used .cargo\bin) and appends that to the *user* PATH in the registry, which
:: does nothing for the cmd session we are already inside. winget uses its own
:: Links folder. Put all three on PATH up front so a uv installed by a previous
:: run - or by the installer we are about to invoke - is visible immediately.
set "PATH=%USERPROFILE%\.local\bin;%USERPROFILE%\.cargo\bin;%LOCALAPPDATA%\Microsoft\WinGet\Links;%PATH%"

set "UV_EXE="
for /f "usebackq delims=" %%p in (`where uv 2^>nul`) do if not defined UV_EXE set "UV_EXE=%%p"
if defined UV_EXE goto :uv_ready

echo [i] uv not found - installing it from https://astral.sh/uv
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference='Stop';" ^
  "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12;" ^
  "irm https://astral.sh/uv/install.ps1 | iex"

for /f "usebackq delims=" %%p in (`where uv 2^>nul`) do if not defined UV_EXE set "UV_EXE=%%p"
if defined UV_EXE goto :uv_ready

:: Locked-down machines (execution policy, constrained language mode, AV) can
:: block the piped installer. winget ships with Windows 10/11 and is a
:: completely separate delivery path, so try it before giving up.
echo.
echo [i] Script installer did not work - trying winget...
winget install --id=astral-sh.uv -e --accept-source-agreements --accept-package-agreements

for /f "usebackq delims=" %%p in (`where uv 2^>nul`) do if not defined UV_EXE set "UV_EXE=%%p"
if defined UV_EXE goto :uv_ready

echo.
echo ============================================
echo   [ERROR] Could not install uv
echo ============================================
echo.
echo   uv is what fetches Python and the dependencies, so setup cannot
echo   continue without it.
echo.
echo   Check your internet connection and re-run this script, or install
echo   uv yourself and re-run:
echo       winget install --id=astral-sh.uv -e
echo.
echo   If uv IS installed already, close this window, open a NEW terminal
echo   (so it picks up the updated PATH) and re-run setup_windows.bat
echo.
pause
exit /b 1

:uv_ready
set "UVLINE="
for /f "usebackq delims=" %%v in (`uv --version 2^>^&1`) do set "UVLINE=%%v"
echo [OK] Found !UVLINE!
echo [OK] Using: !UV_EXE!

:: Make uv trust the Windows certificate store.
::
:: Unlike pip, uv validates TLS against its own bundled root certificates, so on
:: any machine that inspects HTTPS - antivirus with "HTTPS scanning" (Kaspersky,
:: ESET, Avast, BitDefender), or a corporate proxy (Zscaler, Netskope) - every
:: download dies with "invalid peer certificate: UnknownIssuer" even though the
:: browser and pip are perfectly happy. The intercepting root lives in the
:: Windows store, so pointing uv at that store fixes it. This covers the Python
:: download in step 5 as well as the package install in step 6.
::
:: The setting was renamed (UV_NATIVE_TLS -> UV_SYSTEM_CERTS); probe --help so an
:: older uv that a user already has installed still gets the right one, and no
:: deprecation warning is printed on either.
uv pip install --help 2>nul | findstr /c:"--system-certs" >nul
if !errorlevel! equ 0 (
    set "UV_SYSTEM_CERTS=1"
) else (
    set "UV_NATIVE_TLS=1"
)

:: --- 5. venv ---
:: Exclusive upper bound, same reasoning as setup_mac.sh: pinned deps ship
:: native wheels that lag the newest CPython, and requirements_windows.txt has
:: a hard floor too (numpy 2.4 needs >= 3.11). 3.13 is what AutoCua is
:: developed and shipped on, and it is what uv picks from this range.
set "PYSPEC=>=3.11,<3.14"

echo.
if not exist "venv\Scripts\python.exe" goto :venv_create

:: A leftover venv is only reusable if its interpreter is still in range.
:: Otherwise the install in step 6 dies on an unresolvable dependency tree,
:: which is a much worse error than just rebuilding the venv here.
"venv\Scripts\python.exe" -c "import sys; sys.exit(0 if (3,11) <= sys.version_info[:2] < (3,14) else 1)" >nul 2>&1
if !errorlevel! equ 0 (
    echo [OK] Reusing existing venv\
    goto :venv_ready
)
echo [i] Existing venv\ uses an unsupported Python - rebuilding it
rmdir /s /q venv

:venv_create
echo [*] Creating venv\ ...
:: Only a Python already installed on this machine (--no-managed-python). With
:: none in PYSPEC, python.org's download page opens and the script stops: the
:: person installs Python and runs it again. Nothing here downloads a Python.
:: --seed puts pip inside the venv - uv itself never needs it, but the Nuitka
:: build and any manual "venv\Scripts\python.exe -m pip install ..." do.
:: --prompt auto-cua labels the activated shell "(auto-cua)" whatever this
:: folder is called.
uv venv --seed --no-managed-python --python "!PYSPEC!" --prompt auto-cua venv >nul 2>&1

if exist "venv\Scripts\python.exe" (
    echo [OK] venv created using a Python already on this machine
    goto :venv_ready
)
echo.
echo ============================================
echo   [ERROR] No Python 3.11 to 3.13 found
echo ============================================
echo.
echo   AutoCua needs Python 3.11, 3.12 or 3.13. This script does not download
echo   one. The download page is opening in your browser:
echo       https://www.python.org/downloads/windows/
echo.
echo   Install Python 3.13 ^(64-bit^), tick "Add python.exe to PATH", then run
echo   setup_windows.bat again.
echo.
start "" "https://www.python.org/downloads/windows/"
pause
exit /b 1

:venv_ready
set "PYLINE="
for /f "usebackq delims=" %%v in (`"venv\Scripts\python.exe" --version 2^>^&1`) do set "PYLINE=%%v"
echo [i] !PYLINE!

:: --- 6. Install dependencies ---
echo.
echo [*] Installing requirements from requirements_windows.txt ...
uv pip install --python "venv\Scripts\python.exe" -r requirements_windows.txt
if !errorlevel! neq 0 (
    echo.
    echo [ERROR] Dependency installation failed. Fix the error above and re-run.
    echo.
    echo         If the error mentions a certificate ^("UnknownIssuer", "self-signed
    echo         certificate"^), something on this machine is inspecting HTTPS -
    echo         usually antivirus HTTPS scanning or a corporate proxy. Turn that
    echo         off for the install, or add its root certificate to the Windows
    echo         certificate store, then re-run.
    pause
    exit /b 1
)
echo [OK] Requirements installed

:: --- 7. Rust toolchain (for the web agent) ---
:: The web agent is a Rust extension module (AutoCua\web, PyO3 cdylib) that
:: AutoCua\web\agent\__init__.py compiles on first import. No cargo means
::   python main.py  dies with "cargo not found" the moment web use is selected,
:: so the toolchain belongs in setup rather than in a README step.
::
:: x86_64-pc-windows-gnu, NOT the usual msvc: the msvc target can only link
:: through Visual Studio Build Tools, a 3-7 GB install with its own installer,
:: its own prompts and sometimes its own reboot. The gnu target links against
:: mingw-w64, which step 8 installs unattended in ~250 MB. The extension is
:: abi3 and links only against python3.dll, so the C ABI it is built with makes
:: no difference to CPython.
echo.
echo ============================================
echo   Rust toolchain (web agent)
echo ============================================
echo.

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "CARGO_EXE="
for /f "usebackq delims=" %%p in (`where cargo 2^>nul`) do if not defined CARGO_EXE set "CARGO_EXE=%%p"

if defined CARGO_EXE goto :rust_ready

echo [i] cargo not found - installing the Rust toolchain
echo     Target: x86_64-pc-windows-gnu ^(no Visual Studio needed^)
echo.
set "RUSTUP_EXE=%TEMP%\rustup-init.exe"
powershell -NoProfile -Command ^
  "$ErrorActionPreference='Stop';" ^
  "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12;" ^
  "Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile '%RUSTUP_EXE%' -UseBasicParsing"
if %errorlevel% neq 0 (
    echo [ERROR] Could not download rustup-init.exe
    echo         Check your internet connection and re-run, or install Rust
    echo         yourself from https://rustup.rs and re-run this script.
    pause
    exit /b 1
)

:: --profile minimal: rustc + cargo + std, no docs/clippy/rustfmt. PATH is left
:: modifiable so cargo is available in later shells, same as the uv install
:: above; the prepend on the line before covers THIS session, which the
:: registry write does not reach.
:: --default-host too: rust-toolchain.toml at the repo root names a version
:: without a triple, and rustup completes it with the default host, which
:: rustup-init otherwise sets to MSVC on Windows.
"%RUSTUP_EXE%" -y --default-toolchain stable-x86_64-pc-windows-gnu --default-host x86_64-pc-windows-gnu --profile minimal
if %errorlevel% neq 0 (
    echo [ERROR] rustup-init failed. Fix the error above and re-run.
    pause
    exit /b 1
)
del /f /q "%RUSTUP_EXE%" >nul 2>&1

for /f "usebackq delims=" %%p in (`where cargo 2^>nul`) do if not defined CARGO_EXE set "CARGO_EXE=%%p"
if not defined CARGO_EXE (
    echo [ERROR] Rust installed but cargo is not on PATH in this session.
    echo         Close this window, open a NEW terminal and re-run setup_windows.bat
    pause
    exit /b 1
)

:rust_ready
:: A rustup from before this script set the host (or installed by hand) has
:: MSVC as its default host, so the repo's rust-toolchain.toml would fetch the
:: MSVC build of the pinned version, which needs Visual Studio's linker.
rustup set default-host x86_64-pc-windows-gnu >nul 2>&1
:: The first cargo call installs the pinned toolchain (rust-toolchain.toml),
:: about 150 MB, once.
echo [i] Checking the Rust version the repo pins ^(rust-toolchain.toml^)...
set "CARGOLINE="
for /f "usebackq delims=" %%v in (`cargo --version 2^>^&1`) do set "CARGOLINE=%%v"
echo [OK] Found !CARGOLINE!
echo [OK] Using: !CARGO_EXE!

:: --- 8. C toolchain (mingw-w64) ---
:: cargo alone cannot build this crate. Two things need a real C toolchain:
::   * ring (the SHA-256 that stages the browser extension) compiles C sources
::   * the windows-gnu target needs binutils' dlltool + as to generate the
::     import libraries chrono links against
:: rustup ships linker stubs only - its own GCC-WARNING.txt states the bundled
:: gcc "cannot be used for compiling C files", and it fails with "cannot
:: execute cc1". WinLibs is a plain unpacked mingw-w64 GCC: no MSYS2 shell, no
:: second package manager, ~250 MB, and it installs unattended.
echo.
echo ============================================
echo   C toolchain (mingw-w64)
echo ============================================
echo.

set "MINGW_BIN="
for /f "usebackq delims=" %%p in (`where gcc 2^>nul`) do if not defined MINGW_BIN set "MINGW_BIN=already-on-path"
if defined MINGW_BIN (
    echo [OK] gcc already on PATH - skipping
    goto :mingw_ready
)

echo [i] gcc not found - installing mingw-w64 ^(WinLibs^)
echo.
:: --source winget is required: without it, a failing msstore source turns this
:: into an ambiguous-package error instead of an install.
winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e --source winget ^
  --accept-source-agreements --accept-package-agreements --disable-interactivity

:mingw_ready
:: winget unpacks WinLibs under its Packages folder and does NOT put it on PATH,
:: so find the bin/ it created. The folder name carries a source suffix, hence
:: the wildcard rather than a fixed path.
if not defined MINGW_BIN goto :mingw_find
if "%MINGW_BIN%"=="already-on-path" goto :mingw_done

:mingw_find
set "MINGW_BIN="
for /d %%D in ("%LOCALAPPDATA%\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.*") do (
    if exist "%%~fD\mingw64\bin\gcc.exe" set "MINGW_BIN=%%~fD\mingw64\bin"
)

if not defined MINGW_BIN (
    echo.
    echo [^^!] Could not find a mingw-w64 gcc after installing.
    echo     The web agent will not build; every other mode still works.
    echo     Install it yourself and re-run:
    echo       winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e --source winget
    goto :web_build
)

:: This session sees it via the prepend; later ones need the registry entry,
:: because AutoCua\web\agent\__init__.py re-runs cargo whenever a .rs file
:: changes and cargo must still find gcc then.
set "PATH=%MINGW_BIN%;%PATH%"
powershell -NoProfile -Command ^
  "$b=$env:MINGW_BIN; $p=[Environment]::GetEnvironmentVariable('Path','User');" ^
  "if ($p -notlike ('*'+$b+'*')) { [Environment]::SetEnvironmentVariable('Path',$b+';'+$p,'User') }"
echo [OK] mingw-w64 at %MINGW_BIN%

:mingw_done
set "GCCLINE="
for /f "usebackq delims=" %%v in (`gcc --version 2^>^&1`) do if not defined GCCLINE set "GCCLINE=%%v"
echo [OK] Found !GCCLINE!

:web_build
:: Build the extension now instead of leaving it to the first "web use" run.
:: Doing it here means a compile error surfaces during setup, where the user is
:: already watching, rather than mid-task minutes into an agent run.
echo.
echo ============================================
echo   Building the web agent and the Android agent
echo ============================================
echo.
echo [*] First build takes a few minutes...
"venv\Scripts\python.exe" -c "import AutoCua.web.agent"
if !errorlevel! neq 0 (
    echo.
    echo [^^!] The web agent did not build. Everything else still works -
    echo     computer use, shell use and mobile use do not need it.
    echo     Re-run this script after fixing the error above to retry.
) else (
    echo [OK] Web agent built
)
:: The Android agent (AutoCua\android, Rust like the web agent) is built the
:: same way, so the first "mobile use, android" run does not stop for a build.
:: The emulator and the Android images are NOT fetched here (2.5 GB, Google's
:: terms): the first emulator run does that, or
:: python -m AutoCua.android_connector.emulator ahead of time.
echo [*] Building the Android agent, about a minute...
"venv\Scripts\python.exe" -c "import AutoCua.android.agent"
if !errorlevel! neq 0 (
    echo.
    echo [^^!] The Android agent did not build. Everything else still works -
    echo     only "mobile use, android" needs it.
    echo     Re-run this script after fixing the error above to retry.
) else (
    echo [OK] Android agent built
)

:: --- Android phone tool: adb ---
::
:: A real phone needs Google's adb (platform-tools) to talk to it. It is not
:: vendored in this repo: it is fetched
:: from Google's dl.google.com, version-pinned and SHA-256 verified, and only
:: after the user answers y. An adb already on PATH is used as it is. No (or
:: no answer) skips it; only phones need it.
echo.
echo ============================================
echo   Android phone tool ^(adb^)
echo ============================================
echo.
echo [i] For an Android phone. adb comes from Google, only after your y, and is
echo     never shipped in this repo, under Google's Android SDK terms
echo     ^(developer.android.com/studio/terms^).
"venv\Scripts\python.exe" -m AutoCua.android_connector.adb fetch
if !errorlevel! neq 0 (
    echo [i] Skipped - only an Android phone needs it. Re-run this script,
    echo     or run: venv\Scripts\python.exe -m AutoCua.android_connector.adb fetch
) else (
    echo [OK] adb is ready
)

:: --- 9. The Interception driver: a message, never an install ---
::
:: Interception is THIRD-PARTY software by Francisco Lopes da Silva (oblitum),
:: dual-licensed LGPL-3.0 (non-commercial) / paid commercial license. AutoCua
:: neither ships nor installs it: whoever wants UAC handling installs it
:: themselves, from its author (README.md, "Answering UAC prompts on Windows").
echo.
echo ============================================
echo   Friendly reminder
echo ============================================
echo.
echo [i] In a few cases, like Windows UAC prompts, AutoCua needs the
echo     Interception driver for full control. This setup does not install
echo     it, and everything else works without it. If you want it, see
echo     README.md, "Answering UAC prompts on Windows".
echo.
echo ============================================
echo   Setup complete
echo ============================================
echo.

:setup_end
pause
endlocal
