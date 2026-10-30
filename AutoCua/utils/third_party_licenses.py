"""Regenerate THIRD_PARTY_LICENSES.txt: the licence texts the wheels' prebuilt Rust code must carry.

Every wheel ships two Rust extension modules already compiled: the web agent
(AutoCua/web/agent_native) and the Android agent (AutoCua/android/agent_native).
Linking copies other people's code into them: the crates pinned in each
Cargo.lock, the Rust standard library (compiler-builtins and its bundled libm
included) and
the runtime code the linkers add: MinGW-w64's and GCC's on Windows, Zig's on
Linux. Their licences allow binary copies only together with their licence
texts and notices. This script gathers those into THIRD_PARTY_LICENSES.txt,
which pyproject.toml's license-files puts in every wheel's
.dist-info/licenses/, beside LICENSE and THIRD_PARTY_NOTICES.md.

WHAT IT READS (nothing is downloaded, built or changed)
    cargo      `cargo tree --edges normal,no-proc-macro --target <triple>`
               (--offline --locked) for each crate and each wheel target: the
               crates actually linked. Build- and dev-dependencies and
               proc-macros run at compile time only and do not ship.
               `cargo metadata` gives each crate's folder in the registry.
    registry   each linked crate's LICENSE*, LICENCE*, COPYING*, COPYRIGHT*,
               UNLICENSE and NOTICE* files, and the same names deeper in the
               crate where compiled code sits beside them (ring's BoringSSL,
               fiat and once_cell texts, regex-syntax's Unicode tables). A crate with
               no licence file gets the standard text of its declared licence
               from the toolchain. Sources derived from the Independent JPEG
               Group's code are found by that phrase and their header comment,
               which holds the IJG terms, is copied. The copyright lines of
               the compiled sources are listed too: the Rust files, the
               bundled C, C++ and assembly, and the perlasm scripts (.pl)
               ring's assembly is generated from. A crate's build script and
               other non-library targets are left out of that scan.
    toolchain  share/doc/rust/COPYRIGHT-library.html, the standard library's
               own notices, and the licence texts its in-tree entries name
               (share/doc/rust/licenses/). The std rlibs of each wheel target
               record the crates.io crates std was built from; the ones that
               file leaves out (addr2line, object, ...) get their notices from
               the toolchain's COPYRIGHT.html, as does libm, the math library
               compiler-builtins bundles. The .rmeta beside each rlib names
               the crates in its dependency closure; a crates.io crate only
               libtest or libproc_macro depend on (getopts,
               rustc-literal-escaper) is not in a cdylib and is marked so.
    this file  the texts at the bottom that no toolchain on disk carries, each
               with its source URL and SHA-256: MINGW_RUNTIME and WINPTHREADS,
               MinGW-w64's terms for the Windows modules; COMPILER_BUILTINS,
               the LICENSE.txt of std's in-tree compiler-builtins crate, which
               COPYRIGHT-library.html leaves out; and ZIG, the licence of the
               Zig toolchain whose linker builds the Linux wheel (maturin
               --zig) and may add Zig's compiler-rt to the modules.

Identical texts are printed once, listing every crate@version that uses them.
Copies that differ only in their copyright lines share one text, with each
copy's own lines listed above it. Everything is sorted, so regenerating with
no change to a Cargo.lock or the toolchain leaves the file as it was.

    python -m AutoCua.utils.third_party_licenses           # regenerate the file
    python -m AutoCua.utils.third_party_licenses --check   # exit 1 if it is out of date

Rerun it before building wheels whenever AutoCua/web/Cargo.lock,
AutoCua/android/Cargo.lock or the Rust toolchain changes, and commit the
result. rust-toolchain.toml at the repo root pins the toolchain (channel
RUST_RELEASE below), so every release machine links the std the file
describes; cargo and rustc run from the repo, so they are that toolchain.
It needs the crates in the local cargo registry (any build of the two crates
leaves them there; otherwise `cargo fetch` in each crate folder) and, for the
exact list of std's crates, each wheel target's std (`rustup target add
<triple>`); without one it says so and uses the others.
"""

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
import textwrap
from html.parser import HTMLParser
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]      # AutoCua/utils/ -> the repo
OUTPUT = REPO / "THIRD_PARTY_LICENSES.txt"

# The extension modules the wheels ship prebuilt: crate folder, what it is.
MODULES = (
    ("AutoCua/web", "the web agent"),
    ("AutoCua/android", "the Android agent"),
)

# One wheel per target (note/pypi-release.md): Windows x64 built with Rust's
# GNU toolchain, Apple Silicon macOS, and manylinux x86_64 (maturin --zig).
TARGETS = (
    ("x86_64-pc-windows-gnu", "Windows x64"),
    ("aarch64-apple-darwin", "macOS arm64"),
    ("x86_64-unknown-linux-gnu", "Linux x86_64"),
)

WIDTH = 79

# A licence file, by name: LICENSE, LICENSE-MIT, LICENCE.txt, license-apache-2.0,
# COPYING, COPYRIGHT, UNLICENSE, NOTICE... but not code that happens to be
# called that (src/license.rs, notice.py).
_LICENCE_NAME = re.compile(r"(?i)^(licen[cs]e|copying|copyright|unlicense|notice)([-._ ].*)?$")
_NOT_LICENCE = {".rs", ".c", ".h", ".py", ".toml", ".json", ".yml", ".yaml", ".sh", ".js",
                ".html", ".xml", ".pl", ".lock"}

# Folders of a crate that are never compiled into its library.
_NOT_COMPILED = {"tests", "test", "benches", "bench", "examples", "fuzz", "docs", "doc",
                 "target", ".git", ".github"}
_RUST = {".rs"}
_C = {".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".inl", ".s", ".asm"}
# ring's assembly is generated from these perlasm scripts (crypto/**/asm/*.pl,
# via crypto/perlasm/*.pl), so their copyright lines are the assembly's.
_PERLASM = {".pl"}
# cargo metadata's target kinds that are compiled into the crate's library;
# the sources of its other targets (the build script, bins) do not ship.
_LIBRARY_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}

# The standard library's own crates (library/ in rust-lang/rust). Every other
# rlib in a target's std was built from a crates.io crate.
_STD_IN_TREE = {
    "alloc", "compiler_builtins", "core", "panic_abort", "panic_unwind", "proc_macro",
    "profiler_builtins", "rustc_std_workspace_alloc", "rustc_std_workspace_core",
    "rustc_std_workspace_std", "std", "std_detect", "sysroot", "test", "unwind",
    "windows_link",
}
# The in-tree rlibs a cdylib never links: libtest serves `cargo test` harnesses
# and libproc_macro proc-macro crates. A crates.io crate that only they depend
# on (getopts in libtest, rustc-literal-escaper in libproc_macro) is in the
# sysroot but in no module.
_STD_NOT_LINKED = {"test", "proc_macro"}

# compiler-builtins bundles the libm crate (library/compiler-builtins/libm) as
# its math module and compiles it into every target's rlib; its exports differ
# per target (`nm` on the rlibs): on x86_64-unknown-linux-gnu the f32/f64
# routines (cbrt, floor, fmod, sqrt, ...) as weak symbols, which the Linux
# module then takes from it rather than from glibc's libm. The rlib records no
# version for it, so this names the one rust-lang/rust RUST_RELEASE bundles;
# COPYRIGHT.html has its LICENSE.txt. The wheel named takes math routines from
# it; the text is included whichever target is built.
STD_LIBM = ("libm", "0.2.16")
LIBM_WHEEL = "x86_64-unknown-linux-gnu"


# -- reading ------------------------------------------------------------------

def _normalize(text):
    """One spelling for every copy of a text: LF line ends, no trailing blanks,
    tabs as spaces, no blank lines at either end. Whitespace at a line's end
    carries nothing, and dropping it is what keeps the output stable."""
    text = text.replace("\r\n", "\n").replace("\r", "\n").replace("\f", "\n")
    return "\n".join(line.expandtabs(8).rstrip() for line in text.split("\n")).strip("\n")


def _read(path):
    data = Path(path).read_bytes()
    try:
        text = data.decode("utf-8-sig")
    except UnicodeDecodeError:
        text = data.decode("cp1252", errors="replace")
    return _normalize(text)


def _natural(text):
    """Sort key that puts 0.9.9 before 0.10.0."""
    return [(0, int(part), "") if part.isdigit() else (1, 0, part.lower())
            for part in re.findall(r"\d+|\D+", text)]


def _crate_order(crate):
    return _natural(crate[0]), _natural(crate[1])


def _fill(text, indent=""):
    return textwrap.fill(" ".join(text.split()), WIDTH, initial_indent=indent,
                         subsequent_indent=indent, break_on_hyphens=False)


def _spdx_ids(expression):
    return sorted(set(re.findall(r"[A-Za-z0-9.+-]+", expression or "")) - {"AND", "OR", "WITH"})


# Placeholders of an unfilled template: "Copyright [yyyy] [name of copyright
# owner]" in Apache-2.0's appendix, "<year> <copyright holders>" in SPDX texts.
_TEMPLATE = re.compile(r"\[yyyy\]|\[name of copyright owner\]|<year>|<owner>|<copyright holders>",
                       re.I)
_YEAR = re.compile(r"\b(1[89]|20)\d\d\b")


def _is_copyright(line):
    """Whether a line is a copyright notice ("Copyright (c) 2015 Andrew Gallant",
    "Portions Copyright (c) 2016, Google Inc."), as opposed to licence prose
    that merely starts with the word ("copyright license to reproduce...",
    "COPYRIGHT AND PERMISSION NOTICE")."""
    s = re.sub(r"(?i)^portions\s+", "", line.strip())
    if _TEMPLATE.search(s):
        return False
    if re.match(r"(?i)copyright\b", s):
        if _YEAR.search(s) or re.search(r"(?i)\(c\)|©", s):
            return True
        # "Copyright Mozilla Foundation": a holder without a year.
        return bool(re.match(
            r"Copyright\s+(?!(?i:and|notice|holders?|owners?|statement|licen[cs]e)\b)[A-Z]", s))
    return bool(re.match(r"(?i)(©|\(c\))\s*(1[89]|20)\d\d", s))


def _crate_files(src):
    """Every file of a crate outside the folders that are never compiled, sorted."""
    found = []
    for root, dirs, files in os.walk(src):
        dirs[:] = sorted(d for d in dirs if d.lower() not in _NOT_COMPILED)
        found.extend(Path(root) / name for name in files)
    return sorted(found, key=lambda p: p.relative_to(src).as_posix())


def _comment_with(path, phrase):
    """The comment of a source file that contains `phrase` (a /* */ block or a
    run of // lines), without its comment markers; "" if no comment does."""
    text = _read(path)
    for block in re.findall(r"/\*(.*?)\*/", text, re.S):
        if phrase in block:
            lines = [re.sub(r"^\s*\*( |$)", "", line) for line in block.split("\n")]
            return _normalize(textwrap.dedent("\n".join(lines)))
    run = []
    for line in text.split("\n") + [""]:
        if line.strip().startswith("//"):
            run.append(re.sub(r"^\s*//[/!]? ?", "", line))
        elif any(phrase in part for part in run):
            return _normalize(textwrap.dedent("\n".join(run)))
        else:
            run = []
    return ""


def scan_crate(src, package):
    """What one crate contributes: its licence files, its IJG-derived sources
    with their header comment, and the copyright lines of its compiled
    sources, Rust, C, C++, assembly and perlasm, with how many files of each
    kind there are (Rust, C-family, perlasm).

    Licence files are taken at the top of the crate and, deeper down, only in
    a folder that holds compiled code (ring's third_party/fiat, regex-syntax's
    src/unicode_tables), not in, say, pyo3's
    pyo3-runtime, a Python package that is never compiled in. The sources of
    the crate's non-library targets (its build script above all) are not
    compiled into it and are left out of the copyright scan.
    """
    files = _crate_files(src)
    rel = {f: f.relative_to(src).as_posix() for f in files}
    code_dirs = {"."}
    for f in files:
        if f.suffix.lower() in _RUST | _C:
            code_dirs.update(p.as_posix() for p in Path(rel[f]).parents)

    licences = [(rel[f], _read(f)) for f in files
                if _LICENCE_NAME.match(f.name) and f.suffix.lower() not in _NOT_LICENCE
                and Path(rel[f]).parent.as_posix() in code_dirs]
    # A licence file that Cargo.toml names under another name (license-file).
    named = src / package["license_file"] if package.get("license_file") else None
    if named and named.is_file():
        path = os.path.relpath(named, src).replace(os.sep, "/")
        if path not in dict(licences):
            licences.append((path, _read(named)))

    not_library = {Path(target["src_path"]) for target in package.get("targets", [])
                   if not _LIBRARY_KINDS & set(target.get("kind", []))}
    derived, holders, counts = [], set(), {"rust": 0, "c": 0, "perlasm": 0}
    for f in files:
        suffix = f.suffix.lower()
        if suffix not in _RUST | _C | _PERLASM or f in not_library:
            continue
        counts["rust" if suffix in _RUST else "c" if suffix in _C else "perlasm"] += 1
        if suffix in _RUST | _C and b"Independent JPEG Group" in f.read_bytes():
            derived.append((rel[f], _comment_with(f, "Independent JPEG Group")))
        for line in _read(f).split("\n"):
            # Strip comment markers: /* * // //! # ; and Doxygen's \copy.
            line = re.sub(r"^[\s/*#;!@-]*(\\copy\b)?\s*", "", line)
            line = re.sub(r"\s*\*/\s*$", "", line)
            if _is_copyright(line):
                holders.add(" ".join(line.split()))
    return licences, derived, counts, sorted(holders, key=lambda s: (s.lower(), s))


# -- cargo and the toolchain ---------------------------------------------------

def _tool(name):
    """cargo or rustc: from PATH, else rustup's default ~/.cargo/bin, the way
    AutoCua/utils/rust.py finds cargo."""
    exe = f"{name}.exe" if sys.platform == "win32" else name
    found = shutil.which(name) or str(Path.home() / ".cargo" / "bin" / exe)
    if not Path(found).exists():
        sys.exit(f"{name} not found: install Rust (the setup scripts do) and rerun.")
    return found


def _run(cmd, cwd):
    done = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace")
    if done.returncode:
        hint = ("\n(--offline: if a crate is missing from the registry, run `cargo fetch` in "
                "that folder once, then rerun)" if "--offline" in cmd else "")
        sys.exit(f"`{Path(cmd[0]).stem} {' '.join(cmd[1:3])}` failed in {cwd}:\n"
                 f"{done.stderr.strip()}{hint}")
    return done.stdout


def linked_crates(cargo, crate_dir):
    """{target: {(name, version)}} linked into one module, and the cargo
    metadata package of each.

    `--edges normal,no-proc-macro` follows only what ends up in the module:
    build-dependencies (pyo3-build-config, cc, nasm-rs, ...) and proc-macros
    (serde_derive, ...) run while compiling and leave nothing behind, and
    dev-dependencies serve the tests. `--target` resolves the cfg()
    dependencies the way that target's build does: windows-link on Windows,
    core-foundation-sys on macOS, safe_arch on x86 only.
    """
    linked, packages = {}, {}
    for triple, _ in TARGETS:
        meta = json.loads(_run([cargo, "metadata", "--offline", "--locked", "--format-version", "1",
                                "--filter-platform", triple], crate_dir))
        members = set(meta["workspace_members"])
        for package in meta["packages"]:
            if package["id"] not in members:
                packages.setdefault((package["name"], package["version"]), package)
        tree = _run([cargo, "tree", "--offline", "--locked", "--edges", "normal,no-proc-macro",
                     "--target", triple, "--prefix", "none", "--format", "{p}"], crate_dir)
        found = (re.match(r"(\S+) v(\S+)", line) for line in tree.splitlines())
        linked[triple] = {(m.group(1), m.group(2)) for m in found
                          if m and (m.group(1), m.group(2)) in packages}
    return linked, packages


def _between(doc, start, end):
    i = doc.find(start)
    j = doc.find(end, i + 1)
    return doc[i:j] if i >= 0 and j > i else ""


def _flat(fragment):
    return " ".join(html.unescape(re.sub(r"<[^>]+>", " ", fragment or "")).split())


def html_packages(doc):
    """The out-of-tree crates of one of the toolchain's COPYRIGHT*.html files:
    {(name, version): (licence, [(file, text)])}."""
    packages = {}
    for block in doc.split("<h3>")[1:]:
        title, _, body = block.partition("</h3>")
        # "<package emoji> wasip2-1.0.3+wasi-0.2.9": the version starts at the first -N.N.N.
        named = re.fullmatch(r"\s*(?:\U0001F4E6\s*)?(.+?)-(\d+\.\d+\.\d+\S*)\s*", html.unescape(title))
        if not named:
            continue
        licence = re.search(r"<b>License:</b>(.*?)</p>", body, re.S)
        notices = [(html.unescape(name).strip(), _normalize(html.unescape(text)))
                   for name, text in re.findall(
                       r"<summary><code>(.*?)</code></summary>\s*<pre>(.*?)</pre>", body, re.S)]
        packages[named.group(1), named.group(2)] = (_flat(licence.group(1) if licence else ""),
                                                    notices)
    return packages


class _HtmlText(HTMLParser):
    """COPYRIGHT-library.html's prose as plain text: paragraphs apart, the
    boxes of its in-tree entries as lines, nested boxes indented."""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.lines, self._words, self._depth = [], [], 0

    def _gap(self):
        if self.lines and self.lines[-1]:
            self.lines.append("")

    def _flush(self):
        text = " ".join("".join(self._words).split())
        self._words = []
        if not text:
            return
        if self._depth == 0:
            self._gap()
        indent = "    " * max(self._depth - 1, 0)
        self.lines.append(textwrap.fill(
            text, WIDTH - 2, initial_indent=indent,
            subsequent_indent=indent + ("    " if self._depth else ""), break_on_hyphens=False))

    def handle_starttag(self, tag, attrs):
        if tag in ("div", "p", "h1", "h2", "h3", "li", "br"):
            self._flush()
        if tag == "div":
            self._gap()
            self._depth += 1

    def handle_endtag(self, tag):
        if tag in ("div", "p", "h1", "h2", "h3", "li"):
            self._flush()
        if tag == "div":
            self._depth -= 1

    def handle_data(self, data):
        self._words.append(data)

    def text(self):
        self._flush()
        return _normalize("\n".join(self.lines))


def _norm_crate(name):
    return name.replace("-", "_").lower()


def _rlib_name(path):
    """libaddr2line-ad921020260523f4.rlib (or .rmeta) -> addr2line."""
    return re.sub(r"^lib|-[0-9a-f]+$", "", path.stem)


def std_crates(sysroot):
    """The crates.io crates in the std rlibs of the wheel targets that a
    module links, {rlib crate name: {(package, version)}} (empty where the
    rlib records no source path, and so no version); the targets whose std is
    not installed; and the crates only libtest or libproc_macro depend on,
    {rlib crate name: ({(package, version)}, {"test", ...})}, kept apart.

    Such an rlib keeps its source path, "/rust/deps/addr2line-0.25.1/src/..."
    (with a backslash after /rust/deps on Windows), and with it the version.
    The .rmeta beside each rlib names every crate in its dependency closure
    (libstd's names hashbrown, gimli, ...; libtest's getopts; libproc_macro's
    rustc_literal_escaper), which tells whose dependency a crate is. Without
    .rmeta files nothing is set apart, so every crate counts as linked.
    """
    found, absent, apart = {}, [], {}
    for triple, _ in TARGETS:
        lib = sysroot / "lib" / "rustlib" / triple / "lib"
        rlibs = sorted(lib.glob("*.rlib")) if lib.is_dir() else []
        if not rlibs:
            absent.append(triple)
            continue
        metas = {_rlib_name(meta): meta.read_bytes() for meta in sorted(lib.glob("*.rmeta"))}
        for rlib in rlibs:
            crate = _rlib_name(rlib)
            if crate in _STD_IN_TREE:
                continue
            name = re.escape(crate).replace("_", "[-_]").encode()
            pattern = re.compile(rb"/rust/deps[\\/](" + name
                                 + rb")-(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?)[\\/]")
            versions = {(package.decode(), version.decode())
                        for package, version in pattern.findall(rlib.read_bytes())}
            word = re.compile(rb"(?<![A-Za-z_])" + name + rb"(?![A-Za-z_])")
            users = {other for other, meta in metas.items() if other != crate and word.search(meta)}
            if users and users <= _STD_NOT_LINKED:
                kept, only = apart.setdefault(crate, (set(), set()))
                kept |= versions
                only |= users
            else:
                found.setdefault(crate, set()).update(versions)
    apart = {crate: value for crate, value in apart.items() if crate not in found}
    return found, absent, apart


def libm_targets(sysroot):
    """The wheel targets whose compiler_builtins rlib holds code from the libm
    it bundles: its symbols name the module compiler_builtins::math::libm_math."""
    return [triple for triple, _ in TARGETS
            if any(b"libm_math" in rlib.read_bytes() for rlib in
                   (sysroot / "lib" / "rustlib" / triple / "lib").glob("libcompiler_builtins-*.rlib"))]


# -- collecting the texts --------------------------------------------------------

class Texts:
    """Every licence text, each kept once however many copies use it.

    Copies are grouped by their wording with the copyright lines taken out and
    whitespace evened out, so the MIT texts of a dozen crates that differ only
    in "Copyright (c) <year> <who>" become one text with a dozen owners. The
    unfilled "Copyright [yyyy] [name of copyright owner]" of Apache-2.0's
    appendix counts as such a line too. A copy is (sort key, owner, file,
    text, its copyright lines, the text without them).
    """

    def __init__(self):
        self.groups = {}
        self.numbers = {}

    def add(self, rank, owner, file, text):
        """File one copy and return its text's key. `rank` orders the owners:
        0 the modules' crates, 1 and 2 std's crates, 3 std's own code."""
        text = _normalize(text)
        holders, body, blank = [], [], True
        for line in text.split("\n"):
            if _is_copyright(line):
                holders.append(line.strip())
                continue
            if re.match(r"(?i)\s*copyright\b", line) and _TEMPLATE.search(line):
                continue    # "Copyright [yyyy] [name of copyright owner]", a blank form
            if line or not blank:
                body.append(line)
                blank = not line
        body = "\n".join(body).strip("\n")
        key = " ".join(body.split()) or " ".join(text.split())
        sort_key = (rank, _natural(owner), _natural(file))
        self.groups.setdefault(key, []).append((sort_key, owner, file, text, holders, body))
        return key

    def number(self):
        order = sorted(self.groups, key=lambda key: min(copy[0] for copy in self.groups[key]))
        self.numbers = {key: n for n, key in enumerate(order, 1)}

    def refs(self, keys):
        return "[" + ", ".join(str(n) for n in sorted({self.numbers[key] for key in keys})) + "]"

    def render(self):
        out = []
        for key, number in sorted(self.numbers.items(), key=lambda item: item[1]):
            copies = sorted(self.groups[key])
            shared = len({tuple(copy[4]) for copy in copies}) == 1
            out.append(f"--- [{number}] " + "-" * (WIDTH - len(str(number)) - 7))
            for _, owner, file, _, holders, _ in copies:
                out.append(_hang(f"{owner}: {file}", "  "))
                if not shared:
                    out.extend(f"    {line}" for line in holders)
            out += ["", copies[0][3] if shared else copies[0][5], ""]
        return out


def _standard_texts(texts, owner, package, standard_dir, problems, notes):
    """For a crate with no licence file of its own (a few small crates ship
    none, nor do their repositories): the toolchain's standard text of
    each licence its Cargo.toml declares, credited to the authors it names."""
    ids = _spdx_ids(package.get("license"))
    authors = ", ".join(package.get("authors") or []) or "none named"
    keys = []
    for licence in ids:
        standard = standard_dir / f"{licence}.txt"
        if standard.is_file():
            keys.append(texts.add(0, owner, f"no licence file; {licence} per its Cargo.toml, "
                                  f"by {authors}: the toolchain's standard text", _read(standard)))
        else:
            problems.append(f"{owner}: no licence file, and the toolchain has no standard "
                            f"{licence} text")
    if not ids:
        problems.append(f"{owner}: no licence file and no licence in Cargo.toml")
    elif keys:
        notes.append(f"{owner} has no licence file; used the standard {' / '.join(ids)} text")
    return keys


# -- the file -----------------------------------------------------------------

def _heading(number, title):
    title = f"{number}. {title}"
    return [title, "=" * len(title), ""]


def _table(rows, indent="  "):
    """Rows of (a, b, c) with the first two columns aligned."""
    wide_a = max(len(row[0]) for row in rows)
    wide_b = max(len(row[1]) for row in rows)
    return [f"{indent}{a:<{wide_a}}  {b:<{wide_b}}  {c}".rstrip() for a, b, c in rows]


def _hang(line, indent):
    """One long line wrapped to WIDTH, its continuation lines indented."""
    lead = line[:len(line) - len(line.lstrip())]
    return textwrap.fill(line.strip(), WIDTH, initial_indent=lead, subsequent_indent=lead + indent,
                         break_on_hyphens=False, break_long_words=False)


def generate():
    """THIRD_PARTY_LICENSES.txt's text, the problems that keep it from being
    written (a crate without any licence text, a std crate without notices),
    notes worth a line, and the modules and texts for the summary."""
    cargo, rustc = _tool("cargo"), _tool("rustc")
    rustc_version = _run([rustc, "-V"], REPO).strip()
    sysroot = Path(_run([rustc, "--print", "sysroot"], REPO).strip())
    doc_dir = sysroot / "share" / "doc" / "rust"
    standard_dir = doc_dir / "licenses"
    problems, notes = [], []
    texts = Texts()
    label = dict(TARGETS)
    triples = [triple for triple, _ in TARGETS]

    # -- the crates, each read once whichever modules link it
    modules = []                    # (folder, what, {target: crates}, packages)
    crates, used_by = {}, {}        # (name, version) -> package / [what, ...]
    for folder, what in MODULES:
        linked, packages = linked_crates(cargo, REPO / folder)
        modules.append((folder, what, linked, packages))
        for crate in set().union(*linked.values()):
            crates[crate] = packages[crate]
            used_by.setdefault(crate, []).append(what)

    crate_keys, ijg, bundled = {}, [], []
    for crate in sorted(crates, key=_crate_order):
        package, owner = crates[crate], f"{crate[0]} {crate[1]}"
        src = Path(package["manifest_path"]).parent
        licences, derived, counts, holders = scan_crate(src, package)
        keys = crate_keys[crate] = []
        ijg_terms = []
        for path, text in licences:
            keys.append(texts.add(0, owner, path, text))
            if "Independent JPEG Group" in text:
                ijg_terms.append((path, keys[-1]))
        if not any("/" not in path for path, _ in licences):
            keys += _standard_texts(texts, owner, package, standard_dir, problems, notes)
        ijg_sources = []
        for path, comment in derived:
            if comment:
                keys.append(texts.add(0, owner, f"{path} (its comment naming the IJG)", comment))
                ijg_sources.append((path, keys[-1]))
            else:
                ijg_sources.append((path, None))
                notes.append(f"{owner}: {path} names the IJG outside a comment; read it by hand")
        if ijg_sources or ijg_terms:
            ijg.append((crate, ijg_sources + ijg_terms))
        if holders:
            bundled.append((crate, counts, holders))

    # -- the standard library
    library_html = (doc_dir / "COPYRIGHT-library.html").read_text(encoding="utf-8")
    listed = html_packages(library_html)
    in_tree_html = _between(library_html, '<h2 id="short-version"',
                            '<h2 id="out-of-tree-dependencies"')
    prose = _HtmlText()
    prose.feed(in_tree_html)
    prose.close()
    in_tree_keys = []
    for licence in _spdx_ids(" ".join(
            _flat(found) for found in re.findall(r"<b>License:</b>(.*?)</p>", in_tree_html, re.S))):
        standard = standard_dir / f"{licence}.txt"
        if standard.is_file():
            in_tree_keys.append((licence, texts.add(
                3, "std, in-tree code", f"share/doc/rust/licenses/{licence}.txt", _read(standard))))
        else:
            problems.append(f"std's in-tree code names {licence}; the toolchain has no text for it")

    # compiler-builtins, in-tree but with a LICENSE.txt of its own that the
    # COPYRIGHT files leave out; the copy below is from the pinned release.
    builtins_key = texts.add(3, "std, in-tree code",
                             f"library/compiler-builtins/LICENSE.txt (rust-lang/rust {RUST_RELEASE})",
                             COMPILER_BUILTINS)
    release = re.match(r"rustc (\S+)", rustc_version)
    if release and release.group(1) != RUST_RELEASE:
        notes.append(f"the toolchain is {rustc_version} but COMPILER_BUILTINS, RUST_RELEASE and "
                     f"STD_LIBM describe {RUST_RELEASE}: refresh them (URL and SHA-256 beside them)")

    std, absent, apart = std_crates(sysroot)
    if absent:
        notes.append(f"no std installed for {', '.join(absent)} (rustup target add ...); "
                     f"took std's crates from the other targets")
    in_std = {(_norm_crate(p), v) for versions in std.values() for p, v in versions}
    unversioned = {crate for crate, versions in std.items() if not versions}
    set_apart = {}                  # (normalised name, version or None) -> "; only in ..."
    for crate, (versions, only) in apart.items():
        libs = " and ".join(f"lib{lib}" for lib in sorted(only))
        for key in {(_norm_crate(p), v) for p, v in versions} or {(crate, None)}:
            set_apart[key] = f"; only in std's {libs}, which a cdylib never links"
    std_listed = []
    for (name, version), (licence, notices) in sorted(listed.items(), key=lambda i: _crate_order(i[0])):
        keys = [texts.add(1, f"std: {name} {version}", file, text) for file, text in notices]
        normalised = _norm_crate(name)
        if (normalised, version) in in_std or normalised in unversioned:
            mark = "; in std"
        else:
            mark = set_apart.get((normalised, version)) or set_apart.get((normalised, None), "")
        std_listed.append((name, version, licence, keys, mark))

    listed_versions = {(_norm_crate(n), v) for n, v in listed}
    listed_names = {_norm_crate(n) for n, _ in listed}
    unlisted = {(p, v) for versions in std.values() for p, v in versions
                if (_norm_crate(p), v) not in listed_versions}
    unlisted |= {(crate, None) for crate in unversioned if _norm_crate(crate) not in listed_names}
    std_unlisted = []
    full = html_packages((doc_dir / "COPYRIGHT.html").read_text(encoding="utf-8"))
    for name, version in sorted(unlisted, key=lambda i: (_natural(i[0]), _natural(i[1] or ""))):
        matches = sorted((key for key in full if _norm_crate(key[0]) == _norm_crate(name)
                          and version in (None, key[1])), key=_crate_order)
        if not matches:
            problems.append(f"std's {name} {version or ''} is in neither COPYRIGHT-library.html "
                            f"nor COPYRIGHT.html")
        for n, v in matches:
            licence, notices = full[n, v]
            if not notices:
                problems.append(f"std's {n} {v}: COPYRIGHT.html has no notices for it")
            keys = [texts.add(2, f"std: {n} {v}", f"{file} (from COPYRIGHT.html)", text)
                    for file, text in notices]
            std_unlisted.append((n, v, licence, keys, "; the rlib records no version, so each "
                                 "version COPYRIGHT.html lists" if version is None else ""))
    # libm, bundled in compiler_builtins: no rlib of its own, so found by name.
    with_libm = libm_targets(sysroot)
    if STD_LIBM in full:
        licence, notices = full[STD_LIBM]
        keys = [texts.add(2, f"std: {STD_LIBM[0]} {STD_LIBM[1]}", f"{file} (from COPYRIGHT.html)",
                          text) for file, text in notices]
        where = ("every wheel target" if len(with_libm) == len(TARGETS)
                 else ", ".join(with_libm) if with_libm else "the wheel targets")
        std_unlisted.append((*STD_LIBM, licence, keys,
                             f"; compiler_builtins' bundled math library, compiled into its rlib "
                             f"for {where}; the {label[LIBM_WHEEL]} wheel ({LIBM_WHEEL}) takes math "
                             f"routines (cbrt, floor, fmod, sqrt, ...) from it"))
        if not notices:
            problems.append(f"std's {STD_LIBM[0]} {STD_LIBM[1]}: COPYRIGHT.html has no notices for it")
    else:
        problems.append(f"COPYRIGHT.html does not list {STD_LIBM[0]} {STD_LIBM[1]}: set STD_LIBM to "
                        f"the version library/compiler-builtins/libm/Cargo.toml names")
    if len(with_libm) < len(TARGETS):
        notes.append("no libm code found in compiler_builtins for "
                     + ", ".join(t for t, _ in TARGETS if t not in with_libm))

    texts.number()

    def num(key):
        return texts.numbers[key]

    # -- writing
    out = [
        "THIRD-PARTY LICENCES: AutoCua's prebuilt Rust extensions",
        "=" * 57,
        "",
        _fill("AutoCua's wheels ship two Rust extension modules prebuilt: "
              "AutoCua/web/agent_native (the web agent) and AutoCua/android/agent_native "
              "(the Android agent), each compiled for the wheel's target: "
              + ", ".join(f"{t} ({label[t]})" for t in triples) + ". "
              "Compiling them links in third-party code: the crates pinned in "
              "AutoCua/web/Cargo.lock and AutoCua/android/Cargo.lock, the Rust standard library "
              "(with compiler-builtins and the libm it bundles) and the runtime code the "
              "linkers add: MinGW-w64's and GCC's on Windows, Zig's on Linux. This file holds "
              "the licence texts and notices that code's licences ask binary copies to carry. "
              "THIRD_PARTY_NOTICES.md describes each component; AutoCua's own MIT licence "
              "(LICENSE) covers none of it."),
        "",
        _fill(f"Generated by AutoCua/utils/third_party_licenses.py from the two Cargo.lock files, the "
              f"cargo registry and the Rust toolchain, {rustc_version}: the release that "
              f"rust-toolchain.toml at the repository root pins, so every machine that builds "
              f"a wheel links the standard library described here. Do not edit it by hand: "
              f"after a change to either Cargo.lock or to the pinned toolchain, run"),
        "",
        "    python -m AutoCua.utils.third_party_licenses",
        "",
        _fill("and commit the result. `python -m AutoCua.utils.third_party_licenses --check` exits 1 "
              "when this file is out of date."),
        "",
        "Contents",
        "  1. Independent JPEG Group",
        "  2. Crates linked into each module",
        "  3. Rust standard library",
        "  4. Runtime code the linkers add (Windows x64 and Linux x86_64 wheels)",
        "  5. Copyright lines in compiled sources",
        "  6. Licence texts, numbered; the sections above cite them as [n]",
        "",
        "",
    ]

    out += _heading(1, "INDEPENDENT JPEG GROUP")
    if ijg:
        out += ["This software is based in part on the work of the Independent JPEG Group.", "",
                _fill("The IJG's terms ask copies distributed as executable code only to say so "
                      "in their documentation, which the sentence above does. These crates hold "
                      "code derived from the IJG's libjpeg; the texts cited carry the IJG terms:"),
                ""]
        for crate, refs in ijg:
            out.append(_hang(f"  {crate[0]} {crate[1]}, in {' and '.join(used_by[crate])}: "
                             + ", ".join(f"{path} [{num(key)}]" if key else path
                                         for path, key in refs), "    "))
    else:
        out.append("No linked crate holds code derived from the Independent JPEG Group's.")
    out += ["", ""]

    out += _heading(2, "CRATES LINKED INTO EACH MODULE")
    out += [_fill("Normal dependencies only, per target, as `cargo tree --edges "
                  "normal,no-proc-macro --target <triple>` resolves them: build-dependencies, "
                  "dev-dependencies and proc-macros run at compile time and are not linked. "
                  "Each line gives the licence the crate's Cargo.toml declares and its texts "
                  "in section 6."), ""]
    for folder, what, linked, packages in modules:
        title = f"{folder}/agent_native, {what} ({folder}/Cargo.lock)"
        out += [title, "-" * len(title),
                _fill("Crates linked: " + "; ".join(
                    f"{len(linked[t])} for {t} ({label[t]})" for t in triples) + "."), ""]
        every = sorted(set().union(*linked.values()), key=_crate_order)
        out += _table([(f"{name} {version}",
                        packages[name, version].get("license")
                        or f"see {packages[name, version].get('license_file')}",
                        texts.refs(crate_keys[name, version])) for name, version in every])
        partial = [(crate, [label[t] for t in triples if crate in linked[t]]) for crate in every]
        partial = [(crate, only) for crate, only in partial if len(only) < len(triples)]
        if partial:
            out += ["", "  Linked for some targets only:"]
            out += [f"    {crate[0]} {crate[1]}: {', '.join(only)}" for crate, only in partial]
        out += ["", ""]

    out += _heading(3, "RUST STANDARD LIBRARY")
    out += [_fill(f"rustc links its standard library into every module. Its notices, from "
                  f"the toolchain's share/doc/rust/COPYRIGHT-library.html ({rustc_version}):"), "",
            textwrap.indent(prose.text(), "  "), ""]
    if in_tree_keys:
        out += [_fill("The licences these in-tree entries name, as the toolchain's "
                      "share/doc/rust/licenses/ has them: " + ", ".join(
                          f"{licence} [{num(key)}]" for licence, key in in_tree_keys) + "."), ""]
    out += [_fill(f"compiler-builtins (library/compiler-builtins), the in-tree crate whose "
                  f"routines rustc links into every module on every target for the operations "
                  f"it lowers to calls (128-bit integer division, float conversions, ...), "
                  f"has a LICENSE.txt of its own that COPYRIGHT-library.html leaves out: "
                  f"MIT AND Apache-2.0 WITH LLVM-exception, its work derived from LLVM's "
                  f"compiler-rt under MIT and its bundled libm under MIT. The text is "
                  f"[{num(builtins_key)}], as rust-lang/rust {RUST_RELEASE} has it:"),
            f"  Source: {COMPILER_BUILTINS_URL}", ""]
    out += [_fill("Its out-of-tree dependencies, as COPYRIGHT-library.html lists them. \"In "
                  "std\" marks the ones in the std rlibs of the wheel targets that a module "
                  "links; the rest serve other targets, the build or std's own tests, or (as "
                  "marked) only libtest and libproc_macro, which test harnesses and proc-macro "
                  "crates link and a cdylib never does:"), ""]
    for name, version, licence, keys, mark in std_listed:
        refs = texts.refs(keys) if keys else "(no notices listed)"
        out.append(_hang(f"  {name} {version} {refs}: {licence}{mark}", "    "))
    out.append("")
    if std_unlisted:
        out += [_fill("In the std rlibs of the wheel targets but missing from "
                      "COPYRIGHT-library.html; their notices come from the toolchain's "
                      "share/doc/rust/COPYRIGHT.html:"), ""]
        for name, version, licence, keys, mark in std_unlisted:
            out.append(_hang(f"  {name} {version} {texts.refs(keys)}: {licence}{mark}", "    "))
        out.append("")
    out.append("")

    out += _heading(4, "RUNTIME CODE THE LINKERS ADD (WINDOWS X64 AND LINUX X86_64 WHEELS)")
    out += [_fill("Linking a module can copy runtime code from the linker's own toolchain "
                  "into it, beyond the crates and the standard library. The macOS arm64 "
                  "wheel's modules link Apple's system libraries dynamically; the other two "
                  "wheels' carry what follows."), ""]
    title = "Windows x64 wheel (x86_64-pc-windows-gnu, MinGW-w64 GCC)"
    out += [title, "-" * len(title),
            _fill("Both modules are built with Rust's GNU toolchain and linked by MinGW-w64 "
                  "GCC, which adds runtime code to them statically:"), ""]
    for item in (
            "MinGW-w64's C runtime start-up code (dllcrt2.o) and parts of its runtime "
            "libraries (libmingw32, libmingwex, the CRT import libraries). Their terms: "
            "MinGW-w64's COPYING.MinGW-w64-runtime.txt, below.",
            "winpthreads (libpthread.a), which rustc links into every Windows GNU module. Its "
            "terms: winpthreads' COPYING, below.",
            "GCC's runtime libraries: libgcc, its unwinder libgcc_eh and crtbegin.o/crtend.o. "
            "They are under GPL-3.0-or-later with the "
            "GCC Runtime Library Exception 3.1, which lets the combined work be conveyed under "
            "terms of the distributor's choice and has no notice condition, so no text is "
            "needed for them."):
        out.append(textwrap.fill(" ".join(item.split()), WIDTH, initial_indent="- ",
                                 subsequent_indent="  ", break_on_hyphens=False))
    out.append("")
    for title, url, text in (("COPYING.MinGW-w64-runtime.txt", MINGW_RUNTIME_URL, MINGW_RUNTIME),
                             ("winpthreads: COPYING", WINPTHREADS_URL, WINPTHREADS)):
        out += [f"--- {title} " + "-" * (WIDTH - len(title) - 5), f"Source: {url}", "",
                _normalize(text), ""]
    title = f"Linux x86_64 wheel (maturin --zig, ziglang {ZIG_VERSION})"
    out += [title, "-" * len(title),
            _fill(f"Both modules are linked by Zig's `zig cc` (`maturin build --release --zig` "
                  f"with the ziglang {ZIG_VERSION} package; note/pypi-release.md), which may add "
                  f"Zig's own compiler-rt (its implementations of the routines compilers call "
                  f"for the operations they lower to calls) to them statically. Its terms are "
                  f"Zig's LICENSE (MIT, Zig contributors), below. Neither module compiles C++, "
                  f"so LLVM's libc++ (Apache-2.0 WITH LLVM-exception, no notice condition) is "
                  f"not linked in."),
            ""]
    title = "Zig: LICENSE"
    out += [f"--- {title} " + "-" * (WIDTH - len(title) - 5), f"Source: {ZIG_URL}", "",
            _normalize(ZIG), "", ""]

    out += _heading(5, "COPYRIGHT LINES IN COMPILED SOURCES")
    out += [_fill("The copyright lines in the compiled sources of the linked crates: their "
                  "Rust files, the C, C++ and assembly some of them bundle from other projects, "
                  "and the perlasm scripts (crypto/**/*.pl) ring's assembly is generated from. "
                  "Each line is given once per crate, as written; the crates' licence texts "
                  "are in section 6. Each list covers every architecture in the crate, of "
                  "which a target compiles only its own."), ""]
    for crate, counts, holders in bundled:
        kinds = [f"{n} {noun}{'' if n == 1 else 's'}" for n, noun in (
            (counts["rust"], "Rust file"), (counts["c"], "C, C++ and assembly file"),
            (counts["perlasm"], "perlasm script")) if n]
        out.append(f"{crate[0]} {crate[1]} ({'; '.join(kinds)}):")
        out += [f"  {line}" for line in holders] + [""]
    out.append("")

    out += _heading(6, "LICENCE TEXTS")
    out += [_fill("Each text is printed once, after the copies it stands for (owner: file). "
                  "Where those copies differ only in their copyright lines, each copy's lines "
                  "are listed under it and left out of the text. Owners marked \"std:\" are the "
                  "Rust standard library's dependencies, their files as the toolchain's "
                  "COPYRIGHT-library.html holds them (or COPYRIGHT.html, where it says so)."), ""]
    out += texts.render()

    text = "\n".join(line.rstrip() for line in "\n".join(out).split("\n")).rstrip("\n") + "\n"
    return text, problems, notes, modules, texts


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--check", action="store_true",
                        help="exit 1 if THIRD_PARTY_LICENSES.txt is out of date; write nothing")
    args = parser.parse_args()

    text, problems, notes, modules, texts = generate()
    for note in notes:
        print(f"note: {note}")
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        sys.exit(f"{OUTPUT.name} not written: fix the {len(problems)} error(s) above.")
    for folder, _, linked, _ in modules:
        print(f"{folder}: " + ", ".join(f"{len(linked[t])} crates for {t}" for t, _ in TARGETS))
    print(f"{len(texts.numbers)} distinct licence texts")

    size = len(text.encode("utf-8"))
    current = OUTPUT.read_text(encoding="utf-8").replace("\r\n", "\n") if OUTPUT.exists() else None
    if current == text:
        print(f"{OUTPUT.name} is up to date ({size} bytes)")
    elif args.check:
        sys.exit(f"{OUTPUT.name} is out of date: run  python -m AutoCua.utils.third_party_licenses")
    else:
        OUTPUT.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {OUTPUT.name} ({size} bytes)")


# -- MinGW-w64's terms ----------------------------------------------------------
#
# The Windows modules carry MinGW-w64 runtime code (see section 4 of the
# output), but neither Rust's toolchain nor WinLibs' GCC ships these texts, so
# they are copied here from mingw-w64's own repository at v14.0.0, the
# MinGW-w64 release in the WinLibs GCC that links the Windows wheel. Trailing
# blanks are dropped, as for every text. The SHA-256 is of the file as served;
# SourceForge's and the GitHub mirror's copies are identical. Refresh both when
# the Windows toolchain moves to a MinGW-w64 release whose texts differ.

# sha256 1db8da07b436c68833c0673ffee3d9fcb2526047f3820b81661865dfedc79a1f
MINGW_RUNTIME_URL = ("https://sourceforge.net/p/mingw-w64/mingw-w64/ci/v14.0.0/tree/"
                     "COPYING.MinGW-w64-runtime/COPYING.MinGW-w64-runtime.txt?format=raw")
MINGW_RUNTIME = """\
MinGW-w64 runtime licensing
***************************

This program or library was built using MinGW-w64 and statically
linked against the MinGW-w64 runtime. Some parts of the runtime
are under licenses which require that the copyright and license
notices are included when distributing the code in binary form.
These notices are listed below.


========================
Overall copyright notice
========================

Copyright (c) 2009, 2010, 2011, 2012, 2013 by the mingw-w64 project

This license has been certified as open source. It has also been designated
as GPL compatible by the Free Software Foundation (FSF).

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

   1. Redistributions in source code must retain the accompanying copyright
      notice, this list of conditions, and the following disclaimer.
   2. Redistributions in binary form must reproduce the accompanying
      copyright notice, this list of conditions, and the following disclaimer
      in the documentation and/or other materials provided with the
      distribution.
   3. Names of the copyright holders must not be used to endorse or promote
      products derived from this software without prior written permission
      from the copyright holders.
   4. The right to distribute this software or to use it for any purpose does
      not give you the right to use Servicemarks (sm) or Trademarks (tm) of
      the copyright holders.  Use of them is covered by separate agreement
      with the copyright holders.
   5. If any files are modified, you must cause the modified files to carry
      prominent notices stating that you changed the files and the date of
      any change.

Disclaimer

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS ``AS IS'' AND ANY EXPRESSED
OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO
EVENT SHALL THE COPYRIGHT HOLDERS BE LIABLE FOR ANY DIRECT, INDIRECT,
INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA,
OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE,
EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

========================================
getopt, getopt_long, and getop_long_only
========================================

Copyright (c) 2002 Todd C. Miller <Todd.Miller@courtesan.com>

Permission to use, copy, modify, and distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

Sponsored in part by the Defense Advanced Research Projects
Agency (DARPA) and Air Force Research Laboratory, Air Force
Materiel Command, USAF, under agreement number F39502-99-1-0512.

        *       *       *       *       *       *       *

Copyright (c) 2000 The NetBSD Foundation, Inc.
All rights reserved.

This code is derived from software contributed to The NetBSD Foundation
by Dieter Baron and Thomas Klausner.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:
 1. Redistributions of source code must retain the above copyright
    notice, this list of conditions and the following disclaimer.
 2. Redistributions in binary form must reproduce the above copyright
    notice, this list of conditions and the following disclaimer in the
    documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE NETBSD FOUNDATION, INC. AND CONTRIBUTORS
``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED
TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE FOUNDATION OR CONTRIBUTORS
BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.


===============================================================
gdtoa: Converting between IEEE floating point numbers and ASCII
===============================================================

The author of this software is David M. Gay.

Copyright (C) 1997, 1998, 1999, 2000, 2001 by Lucent Technologies
All Rights Reserved

Permission to use, copy, modify, and distribute this software and
its documentation for any purpose and without fee is hereby
granted, provided that the above copyright notice appear in all
copies and that both that the copyright notice and this
permission notice and warranty disclaimer appear in supporting
documentation, and that the name of Lucent or any of its entities
not be used in advertising or publicity pertaining to
distribution of the software without specific, written prior
permission.

LUCENT DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE,
INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS.
IN NO EVENT SHALL LUCENT OR ANY OF ITS ENTITIES BE LIABLE FOR ANY
SPECIAL, INDIRECT OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER
IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.

        *       *       *       *       *       *       *

The author of this software is David M. Gay.

Copyright (C) 2005 by David M. Gay
All Rights Reserved

Permission to use, copy, modify, and distribute this software and its
documentation for any purpose and without fee is hereby granted,
provided that the above copyright notice appear in all copies and that
both that the copyright notice and this permission notice and warranty
disclaimer appear in supporting documentation, and that the name of
the author or any of his current or former employers not be used in
advertising or publicity pertaining to distribution of the software
without specific, written prior permission.

THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE,
INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS.  IN
NO EVENT SHALL THE AUTHOR OR ANY OF HIS CURRENT OR FORMER EMPLOYERS BE
LIABLE FOR ANY SPECIAL, INDIRECT OR CONSEQUENTIAL DAMAGES OR ANY
DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS
SOFTWARE.

        *       *       *       *       *       *       *

The author of this software is David M. Gay.

Copyright (C) 2004 by David M. Gay.
All Rights Reserved
Based on material in the rest of /netlib/fp/gdota.tar.gz,
which is copyright (C) 1998, 2000 by Lucent Technologies.

Permission to use, copy, modify, and distribute this software and
its documentation for any purpose and without fee is hereby
granted, provided that the above copyright notice appear in all
copies and that both that the copyright notice and this
permission notice and warranty disclaimer appear in supporting
documentation, and that the name of Lucent or any of its entities
not be used in advertising or publicity pertaining to
distribution of the software without specific, written prior
permission.

LUCENT DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE,
INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS.
IN NO EVENT SHALL LUCENT OR ANY OF ITS ENTITIES BE LIABLE FOR ANY
SPECIAL, INDIRECT OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER
IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.


=========================
Parts of the math library
=========================

Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.

Developed at SunSoft, a Sun Microsystems, Inc. business.
Permission to use, copy, modify, and distribute this
software is freely granted, provided that this notice
is preserved.

        *       *       *       *       *       *       *

Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.

Developed at SunPro, a Sun Microsystems, Inc. business.
Permission to use, copy, modify, and distribute this
software is freely granted, provided that this notice
is preserved.

        *       *       *       *       *       *       *

FIXME: Cephes math lib
Copyright (C) 1984-1998 Stephen L. Moshier

It sounds vague, but as to be found at
<http://lists.debian.org/debian-legal/2004/12/msg00295.html>, it gives an
impression that the author could be willing to give an explicit
permission to distribute those files e.g. under a BSD style license. So
probably there is no problem here, although it could be good to get a
permission from the author and then add a license into the Cephes files
in MinGW runtime. At least on follow-up it is marked that debian sees the
version a-like BSD one. As MinGW.org (where those cephes parts are coming
from) distributes them now over 6 years, it should be fine.

=================================================
Some string, memory and time conversion functions
=================================================

Copyright © 2005-2020 Rich Felker, et al.

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

===================================
Headers and IDLs imported from Wine
===================================

Some header and IDL files were imported from the Wine project. These files
are prominent maked in source. Their copyright belongs to contributors and
they are distributed under LGPL license.

Disclaimer

This library is free software; you can redistribute it and/or
modify it under the terms of the GNU Lesser General Public
License as published by the Free Software Foundation; either
version 2.1 of the License, or (at your option) any later version.

This library is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
Lesser General Public License for more details.
"""

# sha256 63263614cdd29f2f93cba85e992f041b31f9fc7b4033692f31269489a8a1b177
WINPTHREADS_URL = ("https://sourceforge.net/p/mingw-w64/mingw-w64/ci/v14.0.0/tree/"
                   "mingw-w64-libraries/winpthreads/COPYING?format=raw")
WINPTHREADS = """\
Copyright (c) 2011 mingw-w64 project

Permission is hereby granted, free of charge, to any person obtaining a
copy of this software and associated documentation files (the "Software"),
to deal in the Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, sublicense,
and/or sell copies of the Software, and to permit persons to whom the
Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.


/*
 * Parts of this library are derived by:
 *
 * Posix Threads library for Microsoft Windows
 *
 * Use at own risk, there is no implied warranty to this code.
 * It uses undocumented features of Microsoft Windows that can change
 * at any time in the future.
 *
 * (C) 2010 Lockless Inc.
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without modification,
 * are permitted provided that the following conditions are met:
 *
 *
 *  * Redistributions of source code must retain the above copyright notice,
 *    this list of conditions and the following disclaimer.
 *  * Redistributions in binary form must reproduce the above copyright notice,
 *    this list of conditions and the following disclaimer in the documentation
 *    and/or other materials provided with the distribution.
 *  * Neither the name of Lockless Inc. nor the names of its contributors may be
 *    used to endorse or promote products derived from this software without
 *    specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AN
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
 * WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT,
 * INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING,
 * BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
 * LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE
 * OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED
 * OF THE POSSIBILITY OF SUCH DAMAGE.
 */
"""


# -- compiler-builtins' terms -----------------------------------------------------
#
# Every module links std's in-tree compiler-builtins crate, whose LICENSE.txt
# (MIT AND Apache-2.0 WITH LLVM-exception: its compiler-rt-derived work under
# MIT, its bundled libm under MIT) neither COPYRIGHT-library.html nor the
# installed toolchain carries, so it is copied here from rust-lang/rust at the
# release rust-toolchain.toml pins. The SHA-256 is of the file as served.
# Refresh RUST_RELEASE, the text and the hash when the pin moves (the script
# prints a note when the toolchain and RUST_RELEASE differ) and STD_LIBM with
# them if library/compiler-builtins/libm/Cargo.toml's version changed.

RUST_RELEASE = "1.97.1"

# sha256 ab6eec6caf0fa5775e411c7a8bc6a45c4ef2956b0980b157ab74fc5cd62a928b
COMPILER_BUILTINS_URL = (f"https://raw.githubusercontent.com/rust-lang/rust/{RUST_RELEASE}/"
                         "library/compiler-builtins/LICENSE.txt")
COMPILER_BUILTINS = """\
The compiler-builtins crate is available for use under both the MIT license
and the Apache-2.0 license with the LLVM exception (MIT AND Apache-2.0 WITH
LLVM-exception).

The libm crate is available for use under the MIT license.

As a contributor, you agree that your code may be used under any of the
following: the MIT license, the Apache-2.0 license, or the Apache-2.0 license
with the LLVM exception. In other words, original (non-derivative) work is
licensed under MIT OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception. This is
the default license for all other source in this repository.

Text of the relevant licenses is provided below:

------------------------------------------------------------------------------
MIT License

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
------------------------------------------------------------------------------
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

    TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

    1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work
      (an example is provided in the Appendix below).

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

    2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

    3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

    4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

    5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

    6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

    7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

    8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

    9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

    END OF TERMS AND CONDITIONS

    APPENDIX: How to apply the Apache License to your work.

      To apply the Apache License to your work, attach the following
      boilerplate notice, with the fields enclosed by brackets "[]"
      replaced with your own identifying information. (Don't include
      the brackets!)  The text should be enclosed in the appropriate
      comment syntax for the file format. We also recommend that a
      file or class name and description of purpose be included on the
      same "printed page" as the copyright notice for easier
      identification within third-party archives.

    Licensed under the Apache License, Version 2.0 (the "License");
    you may not use this file except in compliance with the License.
    You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

    Unless required by applicable law or agreed to in writing, software
    distributed under the License is distributed on an "AS IS" BASIS,
    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
    See the License for the specific language governing permissions and
    limitations under the License.


---- LLVM Exceptions to the Apache 2.0 License ----

As an exception, if, as a result of your compiling your source code, portions
of this Software are embedded into an Object form of such source code, you
may redistribute such embedded portions in such Object form without complying
with the conditions of Sections 4(a), 4(b) and 4(d) of the License.

In addition, if you combine or link compiled forms of this Software with
software that is licensed under the GPLv2 ("Combined Software") and if a
court of competent jurisdiction determines that the patent provision (Section
3), the indemnity provision (Section 9) or other Section of the License
conflicts with the conditions of the GPLv2, you may retroactively and
prospectively choose to deem waived or otherwise exclude such Section(s) of
the License, but only in their entirety and only with respect to the Combined
Software.
------------------------------------------------------------------------------

Portions of this software are derived from third-party works licensed under
terms compatible with the above Apache-2.0 WITH LLVM-exception AND MIT
license:

* compiler-builtins is derived from LLVM's compiler-rt (https://llvm.org/).
  Work derived from compiler-rt prior to 2019-01-19 is usable under the MIT
  license, with the following copyright:

      Copyright (c) 2009-2016 by the contributors listed in CREDITS.TXT

  The relevant CREDITS.TXT is located at
  https://github.com/llvm/llvm-project/blob/main/compiler-rt/CREDITS.TXT.

* Work derived from compiler-rt after 2019-01-19 is usable under the
  Apache-2.0 license with the LLVM exception.

* The bundled `math` module is from the libm crate, usable under the MIT
  license. For further details and copyrights, see see libm/LICENSE.txt at
  https://github.com/rust-lang/compiler-builtins.

Additionally, some source files may contain comments with specific copyrights
or licenses.
"""


# -- Zig's terms ------------------------------------------------------------------
#
# The Linux wheel is built with `maturin build --release --zig` (the ziglang
# package pinned in note/pypi-release.md), so Zig's `zig cc` links its modules
# and may add Zig's own compiler-rt to them (neither module compiles C++, so
# LLVM's libc++ stays out). Zig's LICENSE is copied here from the
# tagged release. The SHA-256 is of the file as served. Refresh ZIG_VERSION,
# the text and the hash when the release note moves to another ziglang.

ZIG_VERSION = "0.15.2"

# sha256 5c537d6853e005298a285d508cff9ac7192cea23576c840d485b2b586a7ff177
ZIG_URL = f"https://raw.githubusercontent.com/ziglang/zig/{ZIG_VERSION}/LICENSE"
ZIG = """\
The MIT License (Expat)

Copyright (c) Zig contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
"""


if __name__ == "__main__":
    main()
