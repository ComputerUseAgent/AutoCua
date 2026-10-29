# Third-Party Notices

AutoCua itself is released under the MIT License (see [LICENSE](LICENSE)).

The components listed below are **third-party works redistributed inside this
repository**. They are **not** covered by AutoCua's MIT license and are **not**
sublicensed by it. Each remains under its own terms, and the MIT grant in
[LICENSE](LICENSE) — including the permission to sell or sublicense — applies
only to AutoCua's own code, never to these components.

None of the components fetched at run time is downloaded without the person's
yes on the terminal (`AutoCua/utils/consent.py`; `AutoCua_AUTO_DOWNLOAD=1`
answers yes where nobody can, and Google's Android SDK terms have their own
acceptance). Each comes from its author's own server, pinned by size and
checksum where the author publishes one.

---

## 1. Interception — **not redistributed; fetched at setup time**

| | |
|---|---|
| **Author** | Francisco Lopes da Silva ("oblitum") |
| **Upstream** | https://github.com/oblitum/Interception |
| **How it reaches you** | **Not vendored in this repository.** `setup_windows.bat` downloads the author's own GitHub release directly to the user's machine. |
| **Pinned version** | `v1.0.1` — asset `Interception.zip` |
| **SHA-256** | `ad038963d6413055765128b0b931f6e765147c9916dba79e65d872b261f9af10` — verified before the installer is executed; setup aborts on mismatch |
| **License** | **Dual-licensed.** Non-commercial use: LGPL v3.0. Commercial use: requires a **separate paid commercial license** obtained from the author. Both texts ship inside the downloaded archive under `licenses/`. |

**This project does not redistribute Interception.** No Interception binary,
library, header, or installer is committed to this repository, and none is
included in the published wheel or source distribution. `setup_windows.bat`
downloads it to the user's own machine only after the user explicitly answers
**Y** to an install prompt (answering **N** skips it entirely), and the
downloaded contents are gitignored. Users obtain the driver from its author, over
TLS, from the author's own release URL, with the archive's checksum verified
before anything is executed.

**Important — this is a carve-out from AutoCua's MIT license.**

AutoCua's MIT license permits commercial use, sublicensing, and sale *of
AutoCua's own source code*. It does **not** and **cannot** grant those rights over
Interception. If you ship, sell, or otherwise commercially distribute a product
that bundles or installs Interception — including any packaged AutoCua binary
that does so — **you must obtain a commercial license from the Interception
author yourself.** AutoCua's maintainers do not provide, resell, or extend one
to you.

**What it does.** Interception is a Windows kernel-mode keyboard/mouse filter
driver. AutoCua uses it for one purpose only: Windows' user-mode `SendInput`
API cannot deliver input to the UAC secure desktop, so responding to a UAC
elevation prompt requires kernel-mode input. `setup_windows.bat` documents the
registry changes the driver makes, and `interception_toggle.ps1 -Action unbind`
removes them.

Because it is a signed kernel input filter driver, some antivirus and EDR
products classify Interception under generic "HackTool" / "keyboard filter"
heuristics. The binaries here are the author's own unmodified release, signed
`CN=Francisco Lopes da Silva, C=BR`; verify the Authenticode signature before
trusting them.

---

## 2. WebDriverAgent — **not redistributed; cloned at setup time**

| | |
|---|---|
| **Authors** | Facebook, Inc. and the Appium project |
| **Upstream** | https://github.com/appium/WebDriverAgent |
| **How it reaches you** | **Not vendored in this repository.** [`setup_ios.sh`](setup_ios.sh) clones it from the Appium project directly onto the user's machine. |
| **Pinned version** | tag `v15.1.1` (`git clone --depth 1 --branch v15.1.1`) |
| **License** | BSD 3-Clause, with some files under Apache License 2.0 — the `LICENSE` file arrives with the clone |

Unlike Interception, **BSD 3-Clause expressly permits redistribution**, so
vendoring WebDriverAgent was never a licensing problem. It is fetched instead
simply so this repository contains no third-party source we did not write —
which also keeps Apple private-framework headers (`XCTest`, `UIKitCore`,
shipped by upstream WebDriverAgent) out of our tree entirely.

`AutoCua/ios_connector/setup.py` rewrites the clone's
`WebDriverAgent.xcodeproj/project.pbxproj` locally to switch its targets to
automatic signing with the user's own Apple development team. That modification
happens on the user's machine and is never redistributed.

Neither Facebook, Meta, nor the Appium project endorses AutoCua; per the
BSD 3-Clause terms their names are not used to promote this project.

The clone also carries **CocoaAsyncSocket** (public domain / MIT), which
WebDriverAgent vendors upstream. It reaches users from Appium, not from us.

---

## 3. Python dependencies

Runtime dependencies in [`requirements_mac.txt`](requirements_mac.txt),
[`requirements_windows.txt`](requirements_windows.txt),
[`requirements_linux.txt`](requirements_linux.txt), and
[`requirements_ios.txt`](requirements_ios.txt) (optional iOS) are **not** vendored —
they are installed from PyPI at setup time and each remains under its own
license. Notable among them, `interception-python` is a Python binding that
requires the Interception driver covered in §1; the same dual-license terms
apply to the driver it talks to.

---

## 4. PP-OCRv6 models — **not redistributed; fetched on first use**

| | |
|---|---|
| **Author** | PaddlePaddle (Baidu), the PaddleOCR project |
| **Upstream** | https://huggingface.co/PaddlePaddle/PP-OCRv6_small_det_onnx (commit `28fe5895c24fd108c19eb3e8479f4ab385fbfc62`) and https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx (commit `b8f84f0b80c529de40b4fbb3544b84fa7233a513`) — the defaults; `AutoCua/linux/tree/ocr.py` names them, and the tiny / medium tiers of the same collection drop in by name |
| **How it reaches you** | **Not vendored in this repository.** `AutoCua/utils/ocr_models.py` downloads `inference.onnx` and `inference.yml` for each model from the author's own Hugging Face repository, over TLS, at the pinned commit, into `AutoCua/linux/tree/models/<model>/` the first time a Linux scan needs them (or ahead of time: `python3 -m AutoCua.utils.ocr_models`). Size and SHA-256 are verified before a file is put in place; a mismatch is discarded. That directory is gitignored. |
| **Pinned checksums** | In `AutoCua/utils/ocr_models.py` (`REGISTRY`): every file's byte size and SHA-256 at the commit above, for the tiny, small and medium tiers. The defaults' `inference.onnx`: det (9.9 MB) `d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e`, rec (21.2 MB) `5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634` |
| **License** | Apache License 2.0, as declared on each model card. |
| **Used by** | The Linux scanner's OCR fusion, run locally through `onnxruntime` (MIT). Without the runtime or the models the scanner still runs, on AT-SPI alone, and says so. |

The models remain under the Apache License 2.0 and are not covered by
AutoCua's MIT license.

---

## 5. Electron — **not redistributed; fetched on request**

| | |
|---|---|
| **Author** | The Electron project (OpenJS Foundation and Electron contributors) |
| **Upstream** | https://github.com/electron/electron (release `v44.7.0`) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** `AutoCua/desktop/package.json` names it as an npm devDependency, so a checkout's `npm install` in that folder fetches it the usual npm way. Otherwise the first `ui=True` launch asks on the terminal whether to download it, and only on a yes does `AutoCua/utils/electron.py` download the official release zip for the machine (`electron-v44.7.0-<os>-<arch>.zip`) from the project's own GitHub release, over TLS, into a per-user cache folder (`~/Library/Caches/AutoCua/electron`, `%LOCALAPPDATA%\AutoCua\electron`, `~/.cache/AutoCua/electron`), or does so straight away for `python -m AutoCua.utils.electron`. With `ELECTRON_MIRROR` set (as for npm), the same zip comes from that mirror instead, and the question names it. Size and SHA-256 are verified before anything is unpacked; a mismatch is discarded. |
| **Pinned checksums** | In `AutoCua/utils/electron.py` (`ZIPS`): the byte size and SHA-256 of the darwin, linux and win32 zips for arm64 and x64, as published in the release's `SHASUMS256.txt`. For example darwin-arm64 `e04e411b58a0a14375dd21b0ab4a378fd38930a702e4e20e322fee4849404c0b`, win32-x64 `eee30dc8fa1f5ea95490e59f44e46ea68dd24c6e93d22facf70fe5c2d4c2665c` |
| **License** | MIT (Electron). The zip bundles Chromium, Node.js and their dependencies under their own licenses, listed in the `LICENSE` and `LICENSES.chromium.html` files it contains. |
| **Used by** | The desktop app's Chromium window (`AutoCua/desktop`). Without Electron the app opens in pywebview instead. |

Electron and the components it bundles remain under their own licenses and are
not covered by AutoCua's MIT license.

---

## 6. React — **bundled into the UI's `index.js`**

| | |
|---|---|
| **Author** | Meta Platforms, Inc. and affiliates |
| **Upstream** | https://github.com/facebook/react (`react` and `react-dom` 19.3.1, from npm) |
| **How it reaches you** | **Vendored, in built form.** The UI's React + TSX files in `AutoCua/ui` are built by Vite (`npm run build` in that folder) into `AutoCua/ui/index.js`, one minified script that includes React and ReactDOM. That file is committed and ships in the wheel; the npm packages themselves (`AutoCua/ui/node_modules`) do not. The build strips the packages' header comments, so the notice is here instead. |
| **License** | MIT. Copyright (c) Meta Platforms, Inc. and affiliates. The full text is in `node_modules/react/LICENSE` and `node_modules/react-dom/LICENSE` after `npm ci` in `AutoCua/ui`, and at https://github.com/facebook/react/blob/main/LICENSE. |
| **Used by** | The desktop app's page (`AutoCua/ui/index.tsx` and the parts under it). |

React remains under its MIT license, which is reproduced below as it requires;
it is not covered by AutoCua's own MIT license.

```
MIT License

Copyright (c) Meta Platforms, Inc. and affiliates.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

## 7. Android Emulator and system images — **not redistributed; fetched on first use**

| | |
|---|---|
| **Author** | Google LLC and the Android Open Source Project |
| **Upstream** | Google's Android SDK repository, https://dl.google.com/android/repository/, the one Android Studio's SDK Manager uses: the emulator `37.2.12` (listed in `repository2-3.xml`) and the Google APIs system images (`sys-img/google_apis/sys-img2-3.xml`) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** The first `"mobile use, android"` run with `device="simulation"` (or `python -m AutoCua.android_connector.emulator`, ahead of time) has `AutoCua/android_connector/emulator.py` ask the person, on the terminal, to accept Google's terms (below), once, the way Android Studio's `sdkmanager --licenses` does, and only then download the emulator zip for the machine and the system image of the Android version it needs from Google's own server, over TLS, into a per-user folder (`~/Library/Caches/AutoCua/android-sdk`, `%LOCALAPPDATA%\AutoCua\android-sdk`, `~/.cache/AutoCua/android-sdk`). With no terminal, nothing is downloaded unless `AutoCua_ANDROID_ACCEPT_SDK_TERMS=1` says the terms are accepted. Size and SHA-1 are verified before anything is unpacked; a mismatch is discarded. An Android SDK already on the machine that has them (Android Studio's) is used instead, and nothing is downloaded or asked. |
| **Pinned checksums** | In `AutoCua/android_connector/emulator.py` (`EMULATOR_ARCHIVES`, `IMAGES`): each zip's byte size and the SHA-1 Google's manifest lists for it. For example the emulator for Apple Silicon `3af4fe44ce82b3d88ae5678a53735f27ad729c15`, the Android 17 image for arm64 `7a8bfff584798fe87caa4d4883f6ade4658a185b` |
| **License** | Google's Android Software Development Kit License Agreement (https://developer.android.com/studio/terms), the terms Android Studio's SDK Manager shows for these packages. The emulator is built on QEMU (GPL v2); the open-source parts of both are in the Android Open Source Project, and the zips carry their `NOTICE` files. |
| **Used by** | `device="simulation"` runs of `"mobile use, android"`. A `device="hardware"` run fetches none of it. |

The emulator and the images are Google's, under Google's terms and their own
licenses, and are not covered by AutoCua's MIT license.

---

## Reporting a licensing concern

If you believe something here is misattributed, under-attributed, or
redistributed in a way its license does not permit, please open an issue at
https://github.com/AutoCua/AutoCua/issues — we will correct it promptly.
