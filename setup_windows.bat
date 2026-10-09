@echo off
setlocal enabledelayedexpansion

:: ============================================
::  AutoCua - Windows Dev Setup
:: ============================================
::  Installs uv, creates venv\, installs requirements_windows.txt into it, and -
::  only if you answer Y - installs the Interception kernel driver, then asks
::  before rebooting. Platform-shared files
::  (main.py, cli.py, frontend/index.html, frontend/script.js) detect the OS at
::  runtime, so no file patching is needed - one checkout runs on both macOS
::  and Windows as-is.
::
::  uv (https://astral.sh/uv) replaces the old "go install Python 3.13 from
::  python.org yourself, remember to tick Add to PATH, then come back and
::  re-run me" dead end: it uses a Python already on this machine, or downloads
::  one itself. It also replaces pip for the install below - same PyPI
::  packages, much faster, and it resolves the whole tree at once instead of
::  one package at a time. setup_mac.sh is uv-based for the same reasons;
::  this keeps the two platforms on one toolchain.

:: --- 1. Admin self-elevation ---
:: Relaunch through "cmd /k" instead of handing this .bat to Start-Process
:: directly. Shell-executing a .bat runs it under "cmd /c", so the console dies
:: the moment the script exits - including on every error path - and takes the
:: message you needed to read with it. All you see in the parent shell is
:: "Requesting Administrator privileges..." and a window that blinks out of
:: existence. /k keeps that window up however the run ends.
:: [char]34 supplies the quotes so a repo path containing spaces survives.
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [*] Requesting Administrator privileges...
    echo     A new window will open and STAY open - read any errors there.
    powershell -NoProfile -Command "$q=[char]34; Start-Process cmd.exe -Verb RunAs -ArgumentList '/k', ($q + '%~f0' + $q)"
    exit /b
)

:: --- 2. Anchor to script location ---
cd /d "%~dp0"

echo.
echo ============================================
echo   AutoCua - Windows Setup
echo ============================================
echo.

:: --- 3. Sanity-check repo layout ---
:: Required: main.py, requirements_windows.txt.
:: The Interception driver is NOT vendored in this repo - it is downloaded from
:: the author's own GitHub release in step 9 (see THIRD_PARTY_NOTICES.md).
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
:: Prefer a Python already installed on this machine (--no-managed-python) so
:: the venv keeps using the interpreter the user already has; only if nothing
:: local satisfies PYSPEC do we let uv download one. --seed puts pip inside the
:: venv - uv itself never needs it, but the Nuitka build and any manual
:: "venv\Scripts\python.exe -m pip install ..." do. --prompt auto-cua labels
:: the activated shell "(auto-cua)" whatever this folder is called.
uv venv --seed --no-managed-python --python "!PYSPEC!" --prompt auto-cua venv >nul 2>&1

if exist "venv\Scripts\python.exe" (
    echo [OK] venv created using a Python already on this machine
) else (
    echo [i] No suitable Python found locally - letting uv fetch one
    uv venv --seed --python "!PYSPEC!" --prompt auto-cua venv
    if not exist "venv\Scripts\python.exe" (
        echo.
        echo [ERROR] Could not create the virtual environment.
        echo         Check your internet connection and re-run this script.
        pause
        exit /b 1
    )
    echo [OK] venv created with a uv-managed Python
)

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
"%RUSTUP_EXE%" -y --default-toolchain stable-x86_64-pc-windows-gnu --profile minimal
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
echo   Building the web agent
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

:: --- 9. Fetch + install the Interception driver ---
::
:: Interception is THIRD-PARTY software by Francisco Lopes da Silva (oblitum),
:: dual-licensed LGPL-3.0 (non-commercial) / paid commercial license. It is
:: deliberately NOT vendored in this repo: we fetch it straight from the author's
:: own GitHub release so the user receives it from its author, not from us.
:: The release is version-pinned and SHA-256 verified below - if the hash does
:: not match, we abort rather than run an unverified kernel driver installer.
:: See THIRD_PARTY_NOTICES.md and README.md ("Answering UAC prompts on Windows").
::
:: Opt-in: nothing below runs unless the user explicitly answers Y.
echo.
echo ============================================
echo   Interception Driver (optional)
echo ============================================
echo.
echo [i] AutoCua uses a kernel-mode input driver to answer Windows UAC
echo     prompts - user-mode SendInput cannot reach the UAC secure desktop.
echo     Everything except UAC handling works without this driver.
echo.
echo     If you answer Y, this script will:
echo       - download Interception v1.0.1 from its author:
echo           https://github.com/oblitum/Interception
echo       - verify its SHA-256 checksum, then install the driver
echo       - bind it to your built-in keyboard only
echo       - ask before rebooting (the driver activates at boot)
echo     License: LGPL-3.0 non-commercial / paid commercial license.
echo     Shipping a commercial product with it requires your own license.
echo.
echo     If you answer N, nothing is downloaded or installed. You can add
echo     it later by running this script again and answering Y.
echo.
choice /C YN /N /M "Install the Interception driver? [Y/N]: "
:: Only an explicit Y (errorlevel 1) installs. N is 2; Ctrl+C or a choice.exe
:: failure lands outside 1 and skips too - never install by accident.
if errorlevel 2 goto :icept_skip
if not errorlevel 1 goto :icept_skip

set "ICEPT_DIR=%~dp0Interception"
set "ICEPT_VER=v1.0.1"
set "ICEPT_URL=https://github.com/oblitum/Interception/releases/download/%ICEPT_VER%/Interception.zip"
set "ICEPT_SHA=ad038963d6413055765128b0b931f6e765147c9916dba79e65d872b261f9af10"
set "ICEPT_ZIP=%TEMP%\Interception-%ICEPT_VER%.zip"
set "INSTALLER=%ICEPT_DIR%\command line installer\install-interception.exe"

if exist "%INSTALLER%" (
    echo [i] Interception already present at %ICEPT_DIR% - skipping download.
    goto :icept_run
)

echo [*] Downloading %ICEPT_URL%
powershell -NoProfile -Command ^
  "$ErrorActionPreference='Stop';" ^
  "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12;" ^
  "Invoke-WebRequest -Uri '%ICEPT_URL%' -OutFile '%ICEPT_ZIP%' -UseBasicParsing"
if %errorlevel% neq 0 (
    echo [ERROR] Download failed. Check your internet connection or download
    echo         Interception %ICEPT_VER% manually from the URL above and extract
    echo         it to: %ICEPT_DIR%
    pause
    exit /b 1
)

echo [*] Verifying SHA-256...
set "ICEPT_GOT="
for /f "usebackq delims=" %%H in (`powershell -NoProfile -Command "(Get-FileHash -Algorithm SHA256 -LiteralPath '%ICEPT_ZIP%').Hash.ToLower()"`) do set "ICEPT_GOT=%%H"

if /i not "%ICEPT_GOT%"=="%ICEPT_SHA%" (
    echo [ERROR] SHA-256 MISMATCH - refusing to run this installer.
    echo         expected: %ICEPT_SHA%
    echo         actual:   %ICEPT_GOT%
    echo         The download was corrupted or tampered with. Nothing installed.
    del /f /q "%ICEPT_ZIP%" >nul 2>&1
    pause
    exit /b 1
)
echo [OK] Checksum verified

echo [*] Extracting to %ICEPT_DIR%
powershell -NoProfile -Command ^
  "$ErrorActionPreference='Stop';" ^
  "Expand-Archive -LiteralPath '%ICEPT_ZIP%' -DestinationPath '%~dp0.' -Force"
if %errorlevel% neq 0 (
    echo [ERROR] Extraction failed.
    pause
    exit /b 1
)
del /f /q "%ICEPT_ZIP%" >nul 2>&1

if not exist "%INSTALLER%" (
    echo [ERROR] Expected installer not found after extraction:
    echo         %INSTALLER%
    pause
    exit /b 1
)
echo [OK] Interception %ICEPT_VER% fetched from its author

:icept_run
echo [*] Running: "%INSTALLER%" /install
"%INSTALLER%" /install
set INSTALL_RC=%errorlevel%

echo.
if %INSTALL_RC% equ 0 (
    echo [OK] Interception driver installed
) else (
    echo [^^!] Installer returned code: %INSTALL_RC%
    echo     The driver may not be fully registered. Reboot and check anyway.
)

:: --- 10. Bind the driver to the BUILT-IN keyboard only ---
:: The installer registers Interception as a filter on the whole keyboard CLASS,
:: so every keyboard passes through it. It has 10 keyboard slots that are never
:: freed, so each reconnect of a wireless keyboard burns one; once they run out a
:: reconnecting keyboard enumerates fine but delivers NO input until reboot. That
:: repeatedly killed a wireless keyboard (see README.md, "How it is installed").
::
:: Re-binding it to the built-in keyboard/touchpad alone fixes that permanently:
:: those are non-removable, so they take one slot each at boot and never another,
:: and no external keyboard is ever filtered. AutoCua injects through that binding.
echo.
echo ============================================
echo   Protecting external keyboards
echo ============================================
echo.

set "TOGGLE=%~dp0AutoCua\windows\controller\tool\interception_toggle.ps1"

if not exist "%TOGGLE%" (
    echo [ERROR] Missing %TOGGLE%
    echo         Without it the driver would stay bound to every keyboard, which
    echo         is the configuration that breaks external keyboards.
    pause
    exit /b 1
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%TOGGLE%" -Action bind
if %errorlevel% neq 0 (
    echo.
    echo ============================================
    echo   [ERROR] Binding failed - NOT rebooting
    echo ============================================
    echo.
    echo   The driver installer re-added a filter on the whole keyboard class.
    echo   Rebooting now would load it against EVERY keyboard - the exact
    echo   configuration that kills external keyboards.
    echo.
    echo   Removing every filter so your keyboards stay safe. AutoCua will
    echo   report kernel input as unavailable until this is re-run successfully.
    echo.
    powershell -NoProfile -ExecutionPolicy Bypass -File "%TOGGLE%" -Action unbind
    echo.
    echo   Re-run this script as Administrator to try again.
    echo.
    pause
    exit /b 1
)
echo [OK] Driver bound to the built-in keyboard only

echo.
echo ============================================
echo   Setup complete
echo ============================================
echo.
echo [i] Interception is bound to your BUILT-IN keyboard only, so external
echo     keyboards are never filtered and cannot be broken by it. AutoCua
echo     injects input through the built-in device's binding.
echo.
echo [^^!] A REBOOT is required: the driver only binds its slots when it loads
echo     at boot. Until you reboot, kernel input may be unavailable.
echo.
choice /C YN /N /M "Reboot now? Save your work first. [Y/N]: "
if errorlevel 2 goto :reboot_later
if not errorlevel 1 goto :reboot_later
echo [^^!] The system will reboot in 30 seconds.
echo [^^!] To cancel: open cmd and run:  shutdown /a
echo.
shutdown /r /t 30 /c "AutoCua setup complete - rebooting to activate the Interception driver."
goto :setup_end

:reboot_later
echo [i] Not rebooting. Reboot when convenient - UAC handling stays
echo     unavailable until you do.
goto :setup_end

:icept_skip
echo.
echo [i] Skipped the Interception driver - nothing was downloaded or installed.
echo     Everything except UAC handling works. To add it later, run this
echo     script again and answer Y.
echo.
echo ============================================
echo   Setup complete
echo ============================================
echo.

:setup_end
endlocal
