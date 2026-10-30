# Third-Party Notices

AutoCua itself is released under the MIT License (see [LICENSE](LICENSE)).

The components listed below are **third-party works that reach AutoCua's
users**. A few are redistributed in this repository or in the published
wheels: React and scheduler (§6), the Rust crates, Rust standard library and
runtime code compiled into the two prebuilt extensions (§9–§11), and the
ported thinking-orbs code and the icons in the UI (§21–§23). The rest are
downloaded, installed or built on the user's own machine. They are **not**
covered by AutoCua's MIT license and are **not** sublicensed by it. Each
remains under its own terms, and the MIT grant in [LICENSE](LICENSE) —
including the permission to sell or sublicense — applies only to AutoCua's
own code, never to these components.

What AutoCua's own code downloads is fetched only after the person's yes on
the terminal (`AutoCua/utils/consent.py`; `AutoCua_AUTO_DOWNLOAD=1` answers
yes where nobody can), from its author's own server, pinned by size and
checksum where the author publishes one. Where a download comes under its
maker's terms, the question names them: Google's Android SDK terms (§8, §20)
and Google Chrome's terms (§15); Google's emulator and system images also ask
the person to accept Google's terms (§7). The exceptions, each described in its
section: cargo fetches the Rust crates from crates.io without asking, pinned by
`Cargo.lock`, when an extension is built from source (§9, §10); Apple's
developer disk image, on a Mac where Xcode has no copy, comes after the yes
from the pymobiledevice3 project's unpinned copy on GitHub, not from Apple
(§17); Google Chrome comes from Google's unpinned "latest stable" links
(§15); the setup scripts install uv, Rust and, on Windows, WinLibs GCC without
asking (§12–§14); and `setup_mac.sh` installs the `xcodeproj` gem within the
iPhone / iPad step the person said yes to (§19).

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
| **How it reaches you** | **Not vendored in this repository.** [`setup_mac.sh`](setup_mac.sh) clones it from the Appium project directly onto the user's machine, in its optional iPhone / iPad step, only after the user answers **y** (or passes `--ios`); no answer within 30 seconds counts as no. A `pip install` clones it the same way on the first iOS run, after the user's yes (`AutoCua/utils/wda.py`). |
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

Runtime dependencies are **not** vendored. They are installed from PyPI at
setup time, each under its own license: by the setup scripts from
[`requirements_mac.txt`](requirements_mac.txt),
[`requirements_windows.txt`](requirements_windows.txt),
[`requirements_linux.txt`](requirements_linux.txt) and
[`requirements_ios.txt`](requirements_ios.txt) (optional iOS, installed by
`setup_mac.sh`'s iPhone / iPad step), and by `pip install AutoCua` from the
`dependencies` and the `ios` extra in [`pyproject.toml`](pyproject.toml).
Notable among them, `interception-python` is a Python binding that requires
the Interception driver covered in §1; the same dual-license terms apply to
the driver it talks to. `imageio-ffmpeg` carries an `ffmpeg` executable under
the GPL (§16). `pymobiledevice3` (GPL-3.0) is run as a separate program, never
imported as a library or shipped; it mounts Apple's developer disk image,
Xcode's copy (§17).

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
| **Upstream** | https://github.com/facebook/react (`react` and `react-dom` 19.3.1, and `scheduler` 0.28.0, which `react-dom` depends on, from npm) |
| **How it reaches you** | **Vendored, in built form.** The UI's React + TSX files in `AutoCua/ui` are built by Vite (`npm run build` in that folder) into `AutoCua/ui/index.js`, one minified script that includes React, ReactDOM and scheduler. That file is committed and ships in the wheel; the npm packages themselves (`AutoCua/ui/node_modules`) do not. The build strips the packages' header comments, so the notice is here instead. |
| **License** | MIT. Copyright (c) Meta Platforms, Inc. and affiliates, for all three (`AutoCua/ui/package-lock.json` lists each as MIT). The full text is in `node_modules/react/LICENSE`, `node_modules/react-dom/LICENSE` and `node_modules/scheduler/LICENSE` after `npm ci` in `AutoCua/ui`, and at https://github.com/facebook/react/blob/main/LICENSE. |
| **Used by** | The desktop app's page (`AutoCua/ui/index.tsx` and the parts under it). |

React, ReactDOM and scheduler remain under their MIT license, which is
reproduced below as it requires; they are not covered by AutoCua's own MIT
license.

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

## 8. Android SDK Platform-Tools (adb) — **not redistributed; fetched at setup or on first use**

| | |
|---|---|
| **Author** | Google LLC and the Android Open Source Project |
| **Upstream** | Google's Android SDK repository, https://dl.google.com/android/repository/ (`platform-tools_r37.0.1-<os>.zip`) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** `AutoCua/android_connector/adb.py` downloads the zip for the machine from Google's own server, over TLS, into `AutoCua/android_connector/platform-tools/` (gitignored), only after the person answers **y**: in the setup scripts' Android phone step, from `python -m AutoCua.android_connector.adb fetch`, or the first time an Android phone is used from a terminal. The question names Google's Android SDK terms. With no terminal to ask on (the desktop app), nothing is downloaded and the line names that command. An `adb` already on PATH is used instead, and nothing is downloaded or asked. Size and SHA-256 are verified before anything is unpacked; a mismatch is discarded. |
| **Pinned checksums** | In `AutoCua/android_connector/adb.py` (`ARCHIVES`): each zip's byte size and SHA-256. Windows (8,044,989 bytes) `45f4d63113e895ebde0c90f194099a4676b6ac653bd28d54314a9e022bbc1a99`, macOS (16,110,554 bytes) `ee39ad5967e95c2a07f04dbcbde96b1a0c916ba376096db5d2f498b7727a5d1d`, Linux x86_64 (9,054,187 bytes) `d230f13842f60f782a8645f9c813f8f845bf36089ea7289f28c48f17979313f1` |
| **License** | Google's Android Software Development Kit License Agreement (https://developer.android.com/studio/terms), the terms Google's download page shows for these packages. adb itself is open source in the Android Open Source Project, under the Apache License 2.0. |
| **Used by** | `"mobile use, android"`: the connection to a phone over USB or Wi-Fi, and to the emulator. |

Platform-Tools is Google's, under Google's terms, and is not covered by
AutoCua's MIT license. AutoCua does not redistribute it: each person's machine
downloads it from Google.

---

## 9. Rust crates in the web agent — **redistributed; compiled into the prebuilt extension in every wheel**

| | |
|---|---|
| **Authors** | The authors named in each crate's license files and `Cargo.toml`. Among them: Brian Smith (`ring`, `untrusted`), Unicode, Inc. (the ICU4X crates below), Carl Lerche (`bytes`), Julien Cretin and Google Inc. (`data-encoding`), Bartłomiej Kamiński (`generic-array`), David Hewitt and Contributors (`pythonize`), David Tolnay (`zmij`, `serde`, `serde_json`), Radzivon Bartoshyk (`moxcms`, `pxfm`), The Fuchsia Authors (`zerocopy`), the PyO3 Project, the image-rs Developers and the rust-url developers |
| **Upstream** | https://crates.io, at the versions and SHA-256 checksums pinned in [`AutoCua/web/Cargo.lock`](AutoCua/web/Cargo.lock) |
| **How it reaches you** | **Redistributed, in compiled form only.** maturin builds the crate in `AutoCua/web` and links these crates statically into one extension module, `AutoCua/web/agent_native`. That module ships prebuilt in the Windows x64, Apple Silicon macOS and Linux x86_64 wheels. The crates' sources are not in the wheel. Elsewhere the same module is compiled on the user's machine: by maturin for a pip install from the source distribution, and by `AutoCua/utils/rust.py` (`cargo build --release`) in a checkout. A checkout builds it in the setup scripts' build step, when the desktop app starts, or on the first `"web use"` run. Either way, cargo downloads the crates from crates.io and checks them against `Cargo.lock`. It does so without AutoCua's terminal question. |
| **Pinned versions** | `AutoCua/web/Cargo.lock`. Each wheel also lists them in `*.dist-info/sboms/agent_native.cyclonedx.json`, with names, versions and SPDX expressions but no license texts or copyright lines. |
| **License** | Each crate's own, as declared in its `Cargo.toml` (table below). The module links 77 crates on Windows and Linux and 78 on macOS. All are permissive; none is under the GPL, LGPL or MPL. |
| **Used by** | The web agent (`"web use"`). |

| License (each crate's `Cargo.toml`) | Crates linked into the module |
|---|---|
| MIT OR Apache-2.0 | base64 0.22.1, block-buffer 0.10.4, cfg-if 1.0.4, chrono 0.4.45, cpufeatures 0.2.17, crypto-common 0.1.7, digest 0.10.7, equivalent 1.0.2, form_urlencoded 1.2.2, getrandom 0.2.17, hashbrown 0.17.1, http 1.5.0, httparse 1.10.1, idna 1.1.0, idna_adapter 1.2.2, image 0.25.10 (see below), indexmap 2.14.0, itoa 1.0.18, libc 0.2.189, log 0.4.33, num-traits 0.2.19, once_cell 1.21.4, percent-encoding 2.3.2, ppv-lite86 0.2.21, pyo3 0.29.3, pyo3-ffi 0.29.3, rand 0.8.7, rand_chacha 0.3.1, rand_core 0.6.4, regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11 (see below), serde 1.0.229, serde_core 1.0.229, serde_json 1.0.151, sha1 0.10.7, smallvec 1.15.2, stable_deref_trait 1.2.1, thiserror 1.0.69, tungstenite 0.24.0, typenum 1.20.1, url 2.5.8, utf-8 0.7.6, utf8_iter 1.0.4; windows-link 0.2.1 (Windows), iana-time-zone 0.1.65 (macOS, Linux), core-foundation-sys 0.8.7 (macOS) |
| Unicode-3.0 | icu_collections, icu_locale_core, icu_normalizer, icu_normalizer_data, icu_properties, icu_properties_data and icu_provider 2.2.0; litemap 0.8.2, potential_utf 0.1.5, tinystr 0.8.3, writeable 0.6.3, yoke 0.8.3, zerofrom 0.1.8, zerotrie 0.2.4, zerovec 0.11.6 |
| MIT | bytes 1.12.1, data-encoding 2.11.1, generic-array 0.14.7, pythonize 0.29.0, zmij 1.0.23 |
| Unlicense OR MIT | aho-corasick 1.1.5, byteorder 1.5.0, byteorder-lite 0.1.0, memchr 2.8.3 |
| Zlib OR Apache-2.0 OR MIT | bytemuck 1.25.2, zune-core 0.5.3, zune-jpeg 0.5.15 |
| BSD-3-Clause OR Apache-2.0 | moxcms 0.8.1, pxfm 0.1.30 |
| BSD-2-Clause OR Apache-2.0 OR MIT | zerocopy 0.8.56 |
| ISC | untrusted 0.9.0 |
| Apache-2.0 AND ISC | ring 0.17.14 (see below) |

Three of them carry more than their declared license shows:

- **ring 0.17.14** (SHA-256 and random numbers). Its own code is ISC, "Copyright 2015-2025 Brian Smith" (`LICENSE-other-bits`). Code taken from BoringSSL is under the Apache License 2.0 (`LICENSE-BoringSSL`) or ISC, as the top of each file says. `third_party/fiat` is Apache-2.0, "Copyright 2015-2020 the fiat-crypto authors". A `once_cell` polyfill is MIT / Apache-2.0. Its assembly is pregenerated from OpenSSL's Apache-2.0 perlasm scripts, for example `sha512-x86_64.pl`, "Copyright 2005-2016 The OpenSSL Project Authors". The Windows wheel's module contains ring's x86_64 SHA-256 code. ring carries no OpenSSL or SSLeay advertising-clause license and no NOTICE file.
- **image 0.25.10.** The web agent encodes its annotated screenshots with this crate's JPEG encoder. The encoder's forward DCT (`src/codecs/jpeg/transform.rs`) is a Rust translation of `jfdctint.c` from the Independent JPEG Group's libjpeg 9a. It is under the IJG terms: "This software is copyright (C) 1991-2014, Thomas G. Lane, Guido Vollbeding." Those terms ask that copies distributed as executable code only say so in their documentation, which this sentence does: **this software is based in part on the work of the Independent JPEG Group.**
- **regex-syntax 0.8.11.** Its Unicode tables (`src/unicode_tables/`) are generated from Unicode's data files. They are under the Unicode data license in `src/unicode_tables/LICENSE-UNICODE`.

The 15 Unicode-3.0 crates share one license text, "Copyright © 2020-2024 Unicode, Inc.". It adds: "Portions of ICU4X may have been adapted from ICU4C and/or ICU4J. ICU 1.8.1 to ICU 57.1 © 1995-2016 International Business Machines Corporation and others."

These crates remain under their own licenses and are not covered by AutoCua's
MIT license. The texts that go with each wheel are listed under *License texts
that go with the wheels* below.

---

## 10. Rust crates in the Android agent — **redistributed; compiled into the prebuilt extension in every wheel**

| | |
|---|---|
| **Authors** | The authors named in each crate's license files and `Cargo.toml`. Among them: Volker Ströbel (`jpeg-encoder`), Yevhenii Reizner (`roxmltree`), David Hewitt and Contributors (`pythonize`), David Tolnay (`zmij`, `serde`, `serde_json`) and the PyO3 Project |
| **Upstream** | https://crates.io, at the versions and SHA-256 checksums pinned in [`AutoCua/android/Cargo.lock`](AutoCua/android/Cargo.lock) |
| **How it reaches you** | **Redistributed, in compiled form only.** cargo builds the crate in `AutoCua/android` (`AutoCua/utils/rust.py`, `cargo build --release`; for the Linux wheel, `cargo zigbuild` against glibc 2.17, the wheel's manylinux floor) and links these crates statically into one extension module, `AutoCua/android/agent_native`. Each release machine builds it before maturin packs the wheel, and `pyproject.toml` lists it (`include`), so that module ships prebuilt in the Windows x64, Apple Silicon macOS and Linux x86_64 wheels. The crates' sources are not in the wheel. Elsewhere the same module is compiled on the user's machine by `AutoCua/utils/rust.py`: in a checkout, in the setup scripts' build step, when the desktop app starts, or on the first Android run; from a pip install of the source distribution, on the first Android run. Either way, cargo downloads the crates from crates.io and checks them against `Cargo.lock`. It does so without AutoCua's terminal question. |
| **Pinned versions** | `AutoCua/android/Cargo.lock`. The wheel's `*.dist-info/sboms/agent_native.cyclonedx.json` covers §9's module only, the one maturin builds; this module's crates are listed, with their license texts, in `THIRD_PARTY_LICENSES.txt`. |
| **License** | Each crate's own, as declared in its `Cargo.toml` (table below). The module links 26 crates on Windows and Linux and 27 on macOS. All are permissive; none is under the GPL, LGPL or MPL. |
| **Used by** | `"mobile use, android"`. |

| License (each crate's `Cargo.toml`) | Crates linked into the module |
|---|---|
| MIT OR Apache-2.0 | base64 0.22.1, chrono 0.4.45, equivalent 1.0.2, hashbrown 0.17.1, indexmap 2.14.2, itoa 1.0.18, libc 0.2.191, num-traits 0.2.19, once_cell 1.21.4, pyo3 0.29.3, pyo3-ffi 0.29.3, regex 1.13.1, regex-automata 0.4.18, regex-syntax 0.8.11, roxmltree 0.21.1, serde 1.0.229, serde_core 1.0.229, serde_json 1.0.151; windows-link 0.2.1 (Windows), iana-time-zone 0.1.65 (macOS, Linux), core-foundation-sys 0.8.7 (macOS) |
| Zlib OR Apache-2.0 OR MIT | zune-core 0.5.3, zune-jpeg 0.5.15 |
| Unlicense OR MIT | aho-corasick 1.1.5, memchr 2.8.3 |
| MIT | pythonize 0.29.0, zmij 1.0.23 |
| (MIT OR Apache-2.0) AND IJG | jpeg-encoder 0.7.1 |

**jpeg-encoder 0.7.1** encodes the Android agent's marked screenshots
(zune-jpeg decodes the phone helper's JPEGs). It compiles one IJG-derived
file, its forward DCT (`src/fdct.rs`), ported from mozjpeg:
"Copyright (C) 1991-1996, Thomas G. Lane", with libjpeg-turbo modifications
"Copyright (C) 2015, 2020, D. R. Commander". It is under the IJG terms: "This
software is copyright (C) 1991-2020, Thomas G. Lane, Guido Vollbeding". The
crate includes those terms as `LICENSE-IJG`. The wheels ship this module
built, so the IJG statement in §9 covers it too, and `THIRD_PARTY_LICENSES.txt`
carries `LICENSE-IJG`. `regex-syntax` carries the same Unicode data license as
in §9.

These crates remain under their own licenses and are not covered by AutoCua's
MIT license. The texts that go with each wheel are listed under *License texts
that go with the wheels* below.

---

## 11. Rust standard library, GCC and MinGW-w64 runtime code — **redistributed; linked into both prebuilt extensions in every wheel**

| | |
|---|---|
| **Authors** | The Rust Project Developers (`core`, `alloc`, `std` and the crates `std` is built from); the Free Software Foundation (GCC's `libgcc`, `libgcc_eh` and `crtbegin.o`); the mingw-w64 project (its C runtime start-up code, and `winpthreads`, parts of which are (C) 2010 Lockless Inc.) |
| **Upstream** | https://github.com/rust-lang/rust (the stable toolchain that builds each wheel; rustc 1.97.1 for the 0.1.5 Windows wheel); https://gcc.gnu.org; https://www.mingw-w64.org |
| **How it reaches you** | **Redistributed, in compiled form, inside §9's and §10's modules.** rustc links its standard library statically into every extension it builds. The Windows wheel's modules are built with Rust's GNU toolchain (`stable-x86_64-pc-windows-gnu`) and linked by WinLibs GCC (§13). That link also adds, statically, GCC's runtime support code (`libgcc`, its SEH unwinder `libgcc_eh`, and `crtbegin.o`), mingw-w64's C runtime start-up code and its `winpthreads` (`libpthread.a`, which rustc links into every Windows GNU module). The modules need no MinGW DLL. |
| **License** | Rust's standard library: Apache-2.0 OR MIT, except `core`'s Unicode tables (Unicode-3.0, © 1991-2024 Unicode, Inc.). The crates it is built from are listed with their licenses in the toolchain's `share/doc/rust/COPYRIGHT-library.html`. The 0.1.5 Windows module also contains `addr2line` 0.25.1 and `object` 0.37.3, which that file does not list (dual MIT / Apache-2.0 upstream). GCC's runtime libraries: GPL-3.0-or-later with the GCC Runtime Library Exception 3.1. Its §1 reads: "You have permission to propagate a work of Target Code formed by combining the Runtime Library with Independent Modules, even if such propagation would otherwise violate the terms of GPLv3, provided that all Target Code was generated by Eligible Compilation Processes. You may then convey such a combination under terms of your choice, consistent with the licensing of the Independent Modules." mingw-w64 runtime: the terms in its `COPYING.MinGW-w64-runtime.txt`. Most of the runtime is public domain, and the file lists the notices some parts require in binary copies. `winpthreads`: MIT ("Copyright (c) 2011 mingw-w64 project"), with BSD 3-Clause parts ("(C) 2010 Lockless Inc."). |
| **Used by** | Both extensions (§9, §10). |

This code remains under its own licenses and is not covered by AutoCua's MIT
license.

---

## 12. Rust toolchain (rustup, rustc, cargo) — **not redistributed; installed by the setup scripts when missing**

| | |
|---|---|
| **Author** | The Rust Project |
| **Upstream** | https://rustup.rs (`https://win.rustup.rs/x86_64`, `https://sh.rustup.rs`), stable channel, not pinned |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** When no `cargo` is found, `setup_windows.bat` (step 7) downloads `rustup-init.exe` and runs it with `-y --default-toolchain stable-x86_64-pc-windows-gnu --profile minimal`. `setup_mac.sh` and `setup_linux.sh` (step 4) download `https://sh.rustup.rs` and run it with `-y --profile minimal`. These steps do not ask first. The installer adds `~/.cargo/bin` to PATH or to the shell profile. AutoCua's Python code never installs Rust. |
| **License** | Rust, cargo and rustup: MIT OR Apache-2.0. The toolchain also contains LLVM (Apache-2.0 WITH LLVM-exception) and the other components listed in its `share/doc/rust/COPYRIGHT.html`, with their texts in `share/doc/rust/licenses/`. The Windows GNU toolchain also carries MinGW binaries (`rust-mingw`). Its `share/doc/README.txt` says these are "licensed either under the terms of the GNU General Public License, or the GNU General Public License with the GCC Runtime Library Exception" (`COPYING3`, `COPYING.RUNTIME`). |
| **Used by** | Building §9's and §10's modules in a checkout or from the source distribution. |

The Rust toolchain remains under its own licenses and is not covered by
AutoCua's MIT license. AutoCua does not redistribute it.

---

## 13. WinLibs GCC (MinGW-w64) — **not redistributed; installed by `setup_windows.bat` when missing**

| | |
|---|---|
| **Authors** | Brecht Sanders (the WinLibs build), packaging GCC, GNU Binutils and GDB (Free Software Foundation), the mingw-w64 runtime, NASM, CMake and other tools |
| **Upstream** | https://winlibs.com, through winget's `BrechtSanders.WinLibs.POSIX.UCRT` package, at whatever version winget offers (for example `gcc-16.1.0-mingw-w64ucrt-14.0.0-r4`: GCC 16.1.0, Binutils 2.47, GDB 17.2, MinGW-w64 14.0.0, NASM 3.01, CMake 4.4.1) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** When no `gcc` is on PATH, `setup_windows.bat` (step 8) runs `winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e --source winget --accept-source-agreements --accept-package-agreements --disable-interactivity` and adds the package's `bin` folder to the user PATH. The step does not ask first, and its flags accept winget's source and package agreements on the user's behalf. |
| **License** | Each component has its own. GCC, Binutils and GDB: GPL-3.0-or-later. GCC's runtime libraries: GPL-3.0-or-later with the GCC Runtime Library Exception 3.1 (§11). The mingw-w64 runtime and `winpthreads`: their own permissive terms (§11). NASM: BSD 2-Clause. CMake: BSD 3-Clause. The installed package contains no license files for GCC, mingw-w64 or winpthreads; the upstream projects publish them. |
| **Used by** | Compiling C for the Rust builds on Windows (`ring`'s C code, §9) and linking every Windows module, including the two prebuilt in the Windows wheel (§11). |

The WinLibs tools remain under their own licenses and are not covered by
AutoCua's MIT license. AutoCua does not redistribute them.

---

## 14. uv, and the Python it may download — **not redistributed; installed by the setup scripts when missing**

| | |
|---|---|
| **Authors** | Astral Software Inc. (uv, and the python-build-standalone builds of Python); the Python Software Foundation (CPython) |
| **Upstream** | https://astral.sh/uv (installer scripts `install.ps1` and `install.sh`; winget's `astral-sh.uv` as the Windows fallback); Python builds from https://github.com/astral-sh/python-build-standalone, chosen by uv. Neither is pinned. |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** Each setup script installs uv when it is missing, without asking. `setup_windows.bat` runs `irm https://astral.sh/uv/install.ps1 \| iex`, and if that fails, `winget install --id=astral-sh.uv -e --accept-source-agreements --accept-package-agreements`. `setup_mac.sh` and `setup_linux.sh` run `https://astral.sh/uv/install.sh`. uv then creates the venv and prefers a Python already on the machine (`--no-managed-python`). Only when none fits does it download a standalone CPython build, which becomes the Python AutoCua runs on. |
| **License** | uv: MIT OR Apache-2.0. Python: the PSF License Agreement (PSF-2.0), plus the licenses of the libraries it bundles, all in the `LICENSE.txt` inside the downloaded Python. |
| **Used by** | Setup (uv); AutoCua itself, when uv had to download its Python. |

uv and Python remain under their own licenses and are not covered by AutoCua's
MIT license. AutoCua does not redistribute them.

---

## 15. Google Chrome — **not redistributed; installed on request at the first web run**

| | |
|---|---|
| **Author** | Google LLC |
| **Upstream** | Google's "latest stable" download links on dl.google.com (`AutoCua/utils/chrome.py`: `MAC_DMG`, `WIN_INSTALLER`, `LINUX_DEB`, `LINUX_RPM`). They are not pinned, and AutoCua does not check their size or checksum. |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** When a web run finds no Google Chrome, `AutoCua/utils/chrome.py` (`ensure_chrome`) asks on the terminal whether to download it from Google (about 280 MB). Only on a yes, or with `AutoCua_AUTO_DOWNLOAD=1`, does it download Google's installer over TLS and install Chrome. On macOS it copies the app from the DMG into `/Applications` (or `~/Applications`). On Windows it runs Google's installer silently (`/silent /install`). On Linux x86_64 it installs Google's `.deb` or `.rpm` as root through `apt-get`, `dnf` or `zypper` (zypper with `--allow-unsigned-rpm`); Google's package adds Google's software repository to the system. The question names Google and Chrome's terms (google.com/chrome/terms). Without a yes, the web agent uses an installed Chromium or Edge. |
| **License** | Google's proprietary software, under the Google Terms of Service and the Google Chrome and ChromeOS Additional Terms of Service (https://www.google.com/chrome/terms/). Its open-source parts are listed at `chrome://credits`. |
| **Used by** | The web agent (`"web use"`): the person's own Chrome, through the AutoCuaBridge extension, or a Chrome of its own for headless and parallel runs. |

Chrome is Google's, under Google's terms, and is not covered by AutoCua's MIT
license. AutoCua does not redistribute it: each person's machine downloads it
from Google.

---

## 16. FFmpeg, inside the `imageio-ffmpeg` package — **not redistributed; installed from PyPI**

| | |
|---|---|
| **Authors** | The FFmpeg developers (FFmpeg); the imageio project (`imageio-ffmpeg`, which bundles one build of the `ffmpeg` program per platform) |
| **Upstream** | https://ffmpeg.org; https://github.com/imageio/imageio-ffmpeg (on PyPI as `imageio-ffmpeg`) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** `imageio-ffmpeg` is a dependency in `pyproject.toml` and in every requirements file, so pip or uv installs it, with its `ffmpeg` executable, from PyPI. AutoCua starts the executable as a separate program. It uses it to encode each run's recording on Linux and Windows (`AutoCua/utils/run_recorder.py`) and to compress the recording on every OS (`AutoCua/utils/video_compress.py`), where a system `ffmpeg` on PATH is preferred. |
| **License** | The `imageio-ffmpeg` package declares BSD 2-Clause for its own code, which does not cover the executable it bundles. The Windows executable in `imageio-ffmpeg` 0.6.0 (`ffmpeg-win-x86_64-v7.1.exe`, build "7.1-essentials_build-www.gyan.dev") is configured with `--enable-gpl --enable-version3` and includes `libx264` and `libx265`. Its `ffmpeg -L` says it is under the GNU General Public License "either version 3 of the License, or (at your option) any later version". `ffmpeg -L` prints the license of whichever build is installed. |
| **Used by** | Run recordings: encoding (Linux and Windows) and compression (all platforms). |

FFmpeg and the libraries built into it remain under their own licenses and are
not covered by AutoCua's MIT license. AutoCua does not ship or link the
executable; it runs it. Anyone who bundles it redistributes a
GPL-3.0-or-later program under the GPL's terms.

---

## 17. Apple's Developer Disk Image — **not redistributed; mounted from Xcode's copy, downloaded only after a yes when Xcode has none**

| | |
|---|---|
| **Author** | Apple Inc. |
| **Upstream** | Apple ships these images with Xcode, which keeps them on the Mac: for iOS 17 and later in `/Library/Developer/DeveloperDiskImages/iOS_DDI/` (Xcode 16.3 and later), for older iOS in `Xcode.app/Contents/Developer/Platforms/iPhoneOS.platform/DeviceSupport/<version>/`. Only when this Mac has no copy from Xcode does `pymobiledevice3 mounter auto-mount` fetch one, from the pymobiledevice3 project's copy on GitHub (`raw.githubusercontent.com/doronz88/DeveloperDiskImage`, about 17 MB for iOS 17 and later), not from Apple. AutoCua does not pin or check that copy. |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** Each time a wired iPhone or iPad connects without the image mounted (`pymobiledevice3 mounter list`), `AutoCua/ios_connector/ddi.py` mounts Xcode's copy with `pymobiledevice3 mounter mount-personalized` (iOS 17 and later) or `mount-developer`, or has Xcode mount it (`xcrun devicectl device info ddiServices`). Mounting an iOS 17+ image asks Apple's signing server (gs.apple.com) for the phone's ticket the first time, as Xcode does. Only without a copy from Xcode does it run `mounter auto-mount`, after the terminal question ("Download it now? [y/N]", `AutoCua_AUTO_DOWNLOAD=1` answers yes; a run with no terminal names `python -m AutoCua.ios_connector.ddi <udid>`, which asks). The pairing flow in `AutoCua/ios_connector/setup.py` never downloads it: its xcodebuild mounts Xcode's own image. |
| **License** | Apple's software, distributed by Apple as part of Xcode under the Xcode and Apple SDKs Agreement. |
| **Used by** | `"mobile use, ios"` on a real iPhone or iPad. XCTest cannot start without the image, so neither can WebDriverAgent (§2). |

The image is Apple's and is not covered by AutoCua's MIT license. AutoCua does
not redistribute it.

---

## 18. Xcode's iOS platform (Simulator runtime) — **not redistributed; downloaded from Apple by `xcodebuild` on request**

| | |
|---|---|
| **Author** | Apple Inc. |
| **Upstream** | Apple, through Xcode's own `xcodebuild -downloadPlatform iOS` (about 8.5 GB) |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** `setup_mac.sh`'s optional iPhone / iPad step (step 7, after a **y** or `--ios`) checks whether Xcode can build for a simulator. When it cannot, it asks "Download and install it now (~8.5 GB from Apple)?" and runs `xcodebuild -downloadPlatform iOS` on a yes. No answer within 30 seconds counts as no, and `--yes` answers yes. At run time, `AutoCua/ios_connector/sim_session.py` does the same after the terminal question, but only for an iOS version a run asks for and the Mac lacks. `AutoCua_IOS_AUTO_DOWNLOAD=1` answers yes, and `=0` turns the download off. |
| **License** | Apple's, under the Xcode and Apple SDKs Agreement accepted for Xcode. When that agreement has not been accepted, `setup_mac.sh` stops and tells the user to run `sudo xcodebuild -license accept`. |
| **Used by** | `"mobile use, ios"` in simulation mode. |

It is Apple's, part of Xcode, and is not covered by AutoCua's MIT license.

---

## 19. xcodeproj (Ruby gem) — **not redistributed; installed at setup time**

| | |
|---|---|
| **Authors** | The CocoaPods project (`xcodeproj`) and the authors of its dependencies |
| **Upstream** | https://github.com/CocoaPods/Xcodeproj, from RubyGems (https://rubygems.org/gems/xcodeproj), not pinned |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** In `setup_mac.sh`'s optional iPhone / iPad step (after its **y** or `--ios`), when Ruby cannot load `xcodeproj`, the script runs `gem install --user-install xcodeproj`. That installs the gem and its dependencies from RubyGems into the user's own gem folder, with no separate question. At run time, `AutoCua/ios_connector/setup.py` uses it (`require 'xcodeproj'`) to switch WebDriverAgent's project to the user's own signing team (§2). Without the gem, `setup.py` tells the user how to install it. |
| **License** | MIT (`xcodeproj`). Its dependencies are under their own licenses, listed on RubyGems. |
| **Used by** | `"mobile use, ios"`: signing WebDriverAgent. |

`xcodeproj` and its dependencies remain under their own licenses and are not
covered by AutoCua's MIT license.

---

## 20. Build tools for the Android helper: Temurin JDK 21, Android SDK Platform 35, R8 — **not redistributed; fetched only to rebuild the helper**

| | |
|---|---|
| **Authors** | Eclipse Adoptium (Temurin, a build of OpenJDK); Google LLC and the Android Open Source Project (SDK Platform 35, R8 / d8) |
| **Upstream** | Temurin `jdk-21.0.12.1+1` from https://github.com/adoptium/temurin21-binaries; `platform-35_r02.zip` from https://dl.google.com/android/repository/; `r8-9.3.31.jar` from Google's Maven repository, https://dl.google.com/android/maven2/ |
| **How it reaches you** | **Not vendored in this repository or in the wheel.** The committed `AutoCua/android_connector/autocua-helper.jar` holds only AutoCua's own classes, compiled from `Helper.java`. `AutoCua/android_connector/build_helper.py` rebuilds the jar only when `Helper.java` no longer matches the hash recorded in it, or on `python -m AutoCua.android_connector.build_helper`. Only then does it download these three, each after the person's **y** on the terminal, over TLS into `~/.AutoCua/android-build/`. It checks size and SHA-256 before using any of them. From the platform zip it keeps only `android.jar`, to compile against. For the platform zip, the question names Google's Android SDK terms. |
| **Pinned checksums** | In `AutoCua/android_connector/build_helper.py` (`JDKS`, `PLATFORM`, `R8`): the byte size and SHA-256 of the JDK for each platform, for example Windows x64 (205,073,461 bytes) `f9d6e191ab098c0d416e7d588a24420a8621cd2f4720dab2459b8b7b2d2d8b4e`; `platform-35_r02.zip` (64,273,788 bytes) `0988cacad01b38a18a47bac14a0695f246bc76c1b06c0eeb8eb0dc825ab0c8e0`; `r8-9.3.31.jar` (20,954,934 bytes) `29677b994a40668b97155a65628afd953f72571a8100bfa7296b4c5bcc79676c` |
| **License** | Temurin: GPL-2.0 with the Classpath Exception (OpenJDK), with third-party notices in the JDK's `legal/` folder. SDK Platform 35: Google's Android Software Development Kit License Agreement (https://developer.android.com/studio/terms). R8: BSD 3-Clause; the libraries it bundles have their own licenses. |
| **Used by** | Rebuilding `autocua-helper.jar`, the helper `"mobile use, android"` runs on the phone. None of them is copied into the jar or onto the phone. |

These tools remain under their own licenses and are not covered by AutoCua's
MIT license. AutoCua does not redistribute them.

---

## 21. thinking-orbs — **vendored, modified, in the UI and the web agent's page script**

| | |
|---|---|
| **Author** | Jakub Antalik |
| **Upstream** | thinking-orbs v0.3.2, https://libraries.dev/orbs (https://github.com/Jakubantalik/Libraries.dev) |
| **How it reaches you** | **Vendored, modified.** Parts of it are ported to plain JavaScript and CSS, committed, and shipped in the wheel. The desktop app's page loads: the orb engine, presets and painter (`AutoCua/ui/thinking_orb/thinking_orb.js`); the text shimmer (`AutoCua/ui/thinking_orb/shimmer.css`); and the "searching" globe and "composing" ribbon orbs (`AutoCua/ui/container/agent/agent.js`, with copies in `AutoCua/ui/cli/cli_container/tool_icons.js`). The "connecting" orb (`AutoCua/web/browser/glow/glow.js`) is injected by the web agent into the pages it drives. |
| **License** | MIT, "Copyright (c) 2026 Jakub Antalik", reproduced below |
| **Used by** | The desktop app's agent status and tool icons; the web agent's on-page indicator. |

thinking-orbs remains under its MIT license, which is reproduced below as it
requires. It is not covered by AutoCua's own MIT license.

```
MIT License

Copyright (c) 2026 Jakub Antalik

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

## 22. Lucide and Feather icons — **vendored, as glyphs in the UI**

| | |
|---|---|
| **Authors** | Lucide Contributors (Lucide); Cole Bemis (Feather, which Lucide was forked from) |
| **Upstream** | https://lucide.dev (https://github.com/lucide-icons/lucide); https://feathericons.com (https://github.com/feathericons/feather) |
| **How it reaches you** | **Vendored, as SVG path data**, committed and shipped in the wheel. Some glyphs are slightly redrawn. From Lucide: `bot`, `keyboard`, `rotate-cw`, `plug`, `sparkles` and `history`. From Feather: `zap`, `trash`, `settings` and `edit-3`. `link` is the same in both. They are in `AutoCua/ui/agent_mode/agent_mode.tsx`, `AutoCua/ui/chat_input/chat_input.tsx`, `AutoCua/ui/index.tsx`, `AutoCua/ui/settings/settings.tsx` and `AutoCua/ui/chat/chat.tsx`, which are built into `AutoCua/ui/index.js`, and in `AutoCua/ui/setup/setup.js`, `AutoCua/ui/container/dead/notes_stage/notes_stage.html` and `AutoCua/web/browser/AutoCuaBridge/sidepanel.html`. |
| **License** | Lucide: ISC, with the parts that come from Feather © Cole Bemis under MIT (reproduced below). Feather: MIT, "Copyright (c) 2013-2017 Cole Bemis", with the MIT permission notice as reproduced in §21. |
| **Used by** | The desktop app's buttons and toggles, its setup page, and the AutoCuaBridge extension's side panel. |

```
ISC License

Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as part
of Feather (MIT). All other copyright (c) for Lucide are held by Lucide
Contributors 2022.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY
SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION
OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

The icons remain under their own licenses and are not covered by AutoCua's MIT
license.

---

## 23. GitHub mark and Telegram logo — **vendored, as glyphs in the UI**

| | |
|---|---|
| **Authors** | Path data: GitHub's Octicons (`mark-github`) and Pictogrammers' Material Design Icons (`telegram`). The marks belong to GitHub, Inc. and Telegram. |
| **Upstream** | https://github.com/primer/octicons; https://pictogrammers.com/library/mdi/ |
| **How it reaches you** | **Vendored, as SVG path data**, committed and shipped in the wheel. The GitHub mark is in `AutoCua/ui/left_bar/left_bar.tsx`, built into `AutoCua/ui/index.js`. The Telegram logo is in `AutoCua/{windows,mac,linux}/remote_connection/telegram/telergam_animation.html`. |
| **License** | Octicons: MIT, "Copyright (c) 2023 GitHub Inc.", with the MIT permission notice as reproduced in §21. Material Design Icons: the Pictogrammers license (https://pictogrammers.com/docs/general/license/). Whatever the license of a drawing, the marks themselves remain their owners' trademarks, under each owner's brand guidelines. |
| **Used by** | The GitHub link in the left bar, and the Telegram connection screen. |

GitHub and its logo are trademarks of GitHub, Inc. The Telegram name and logo
are Telegram's trademarks. Neither company endorses AutoCua. The app names iOS
and Android in words; it draws neither Apple's logo nor Google's Android robot.

---

## License texts that go with the wheels

Every wheel's `.dist-info/licenses/` holds [LICENSE](LICENSE) and this file
(`license-files` in `pyproject.toml`). So the notices reproduced here travel
with the wheel's binary parts (§9–§11) and the UI code it ships (§6,
§21–§23). The licenses of those parts ask the following of copies:

| License | What a copy must carry, per the license text | Where it is |
|---|---|---|
| IJG (`image`'s JPEG encoder, §9; `jpeg-encoder`'s, §10) | The statement "this software is based in part on the work of the Independent JPEG Group" in the documentation of executable-only copies | §9 of this file, and `THIRD_PARTY_LICENSES.txt` |
| MIT (React and scheduler, §6; thinking-orbs, §21; Feather, §22; Octicons, §23) | The copyright notice and the permission notice | This file |
| ISC (Lucide, §22) | The copyright notice and the permission notice | This file (§22) |
| The crates in §9 and §10, under their own licenses: MIT and ISC (`untrusted`, `ring`), Apache-2.0 (`ring`'s BoringSSL, OpenSSL and fiat-crypto code, and the Apache option of the dual-licensed crates), Unicode-3.0 and Unicode's data-file license (the ICU4X crates, `regex-syntax`'s tables), BSD (if the BSD option of `moxcms`, `pxfm` or `zerocopy` is relied on), IJG (`jpeg-encoder`'s `LICENSE-IJG`) | MIT and ISC: the copyright notice and the permission notice. Apache-2.0: a copy of the license; none of the crates has a NOTICE file. Unicode: the copyright and permission notice, with the copies or in their documentation. BSD: the notice, conditions and disclaimer in the documentation. IJG: the statement above. | `THIRD_PARTY_LICENSES.txt` |
| Rust's standard library (§11) | The same, for its MIT / Apache-2.0 and Unicode-3.0 parts | `THIRD_PARTY_LICENSES.txt` |
| mingw-w64 runtime (Windows wheel, §11) | The notices its `COPYING.MinGW-w64-runtime.txt` lists for binary copies | `THIRD_PARTY_LICENSES.txt` |
| GCC Runtime Library Exception (Windows wheel, §11) | No notice condition: its §1 lets the combination be conveyed "under terms of your choice" | — |

`THIRD_PARTY_LICENSES.txt` sits beside this file and is listed with it in
`license-files`, so every wheel carries both. For each crate linked into §9's
and §10's modules, at its `Cargo.lock` version, it holds the crate's own
license files (`LICENSE*`, `COPYING`, `UNLICENSE`). It also holds `ring`'s
`LICENSE-BoringSSL`, `LICENSE-other-bits`, `third_party/fiat/LICENSE` and
`src/polyfill/once_cell/LICENSE-*`, `jpeg-encoder`'s `LICENSE-IJG`, and
`regex-syntax`'s `src/unicode_tables/LICENSE-UNICODE`. It holds the Rust
toolchain's `share/doc/rust/COPYRIGHT-library.html` and the licenses of
`addr2line` and `object`, and, for the Windows wheel, mingw-w64's
`COPYING.MinGW-w64-runtime.txt`. It is rebuilt from those sources whenever
either `Cargo.lock` or the toolchain changes (`python -m AutoCua.utils.third_party_licenses`).

---

## Reporting a licensing concern

If you believe something here is misattributed, under-attributed, or
redistributed in a way its license does not permit, please open an issue at
https://github.com/AutoCua/AutoCua/issues — we will correct it promptly.
