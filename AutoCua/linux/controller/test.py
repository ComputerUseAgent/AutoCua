#!/usr/bin/env python3

"""
Standalone test of the Linux controller's input paths (no LLM involved).
Run from the project root:  python3 -m AutoCua.linux.controller.test

What it does, in order:
  1. opens the RemoteDesktop portal session — GNOME shows its consent dialog
     the FIRST time; click Share/Allow. The restore token is saved afterwards,
     so later runs (and the agent) never ask again;
  2. launches GNOME Text Editor, clicks into its document, types a line by
     keystrokes (the canvas/terminal path), pastes a multi-line block through
     the portal clipboard (the insertion path), sends ctrl+a / backspace, and
     reads the buffer back over AT-SPI after every step;
  3. closes the editor without saving.

Each step prints PASS/FAIL. A session that cannot start explains why (locked
screen, cancelled dialog, no portal).
"""

import signal
import subprocess
import sys
import time

from ..tree import element as E
from ..tree.element import _STATE, iter_children, acc_states, _safe_name, Rect
from .service import ControllerService
from .hotkey.service import HotkeyService
from .tool.input_portal import get_input, PortalInputError


def _editor_app(deadline=12.0):
    t0 = time.time()
    while time.time() - t0 < deadline:
        for app in E.get_desktop_apps():
            name = _safe_name(app).lower()
            if name in ("gnome-text-editor", "org.gnome.texteditor") or ("text" in name and "editor" in name):
                return app
        time.sleep(0.3)
    return None


def _buffer(app):
    """The editor's document: an EDITABLE, MULTI_LINE text node."""
    stack = [(w, 0) for w in iter_children(app)]
    while stack:
        acc, depth = stack.pop()
        try:
            role = acc.get_role_name() or ""
        except Exception:
            continue
        states = acc_states(acc)
        if states is None or (depth > 0 and not states.contains(_STATE.SHOWING)):
            continue
        if role == "text" and states.contains(_STATE.EDITABLE) \
                and states.contains(_STATE.MULTI_LINE):
            return acc
        if depth < 15:
            for child in iter_children(acc):
                stack.append((child, depth + 1))
    return None


def _check(label, ok, detail=""):
    print(f"  {'PASS' if ok else 'FAIL'}  {label}{(' — ' + detail) if detail else ''}")
    return ok


def main():
    print("1. Portal session (accept the consent dialog if it appears)...")
    inp = get_input()
    t0 = time.time()
    try:
        inp.ensure_session()
    except PortalInputError as e:
        print(f"  FAIL  {e}")
        sys.exit(1)
    print(f"  PASS  session up in {time.time() - t0:.1f}s, "
          f"{len(inp._streams)} monitor stream(s), clipboard "
          f"{'on' if inp._clipboard else 'off'}")

    print("2. Text Editor...")
    proc = subprocess.Popen(["gnome-text-editor", "--new-window", "--ignore-session"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    failures = 0
    try:
        app = _editor_app()
        if app is None:
            print("  FAIL  gnome-text-editor did not appear on the accessibility bus")
            sys.exit(1)
        time.sleep(1.5)
        screen = E.get_screen()
        ctx = E.prepare_scan(screen)
        node = _buffer(app)
        ext = E.acc_extents(node, ctx["offset"]) if node is not None else None
        if not ext:
            print("  FAIL  could not locate the editor's document")
            sys.exit(1)
        rect = Rect(ext["x"], ext["y"], ext["x"] + ext["width"], ext["y"] + ext["height"])
        cs = ControllerService()
        cs.set_elements({"1": {"rect": rect, "visible_rect": rect, "visibility": "full",
                               "name": "document", "type": "Text", "acc_element": node}},
                        "Text Editor")
        read = lambda: E.acc_text(node, 500)

        line = "Hello, World! 123 äöü ñ €"
        r = cs.input(1, line)
        time.sleep(0.5)
        got = read()
        failures += not _check("input (click + clear + clipboard insert)",
                               r["status"] == "success" and got.strip() == line,
                               f"buffer={got!r}")

        r = cs.typewrite(" +typed")
        time.sleep(0.4)
        got = read()
        failures += not _check("typewrite (keystrokes into focus)",
                               r["status"] == "success" and got.strip() == line + " +typed",
                               f"buffer={got!r}")

        block = "def f():\n    return 1\n\nprint(f())"
        r = cs.input(1, block)
        time.sleep(0.6)
        got = read()
        failures += not _check("input multi-line (clipboard paste)",
                               r["status"] == "success" and got.strip() == block,
                               f"buffer={got!r}")

        hk = HotkeyService()
        r1, r2 = hk.send("ctrl+a"), hk.send("backspace")
        time.sleep(0.4)
        got = read()
        failures += not _check("hotkey ctrl+a, backspace",
                               r1["status"] == "success" and r2["status"] == "success"
                               and got.strip() == "", f"buffer={got!r}")

        r = cs.double_click(1)
        failures += not _check("double click", r["status"] == "success", r.get("message", ""))
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
        print("3. Editor closed.")

    print(f"\n{'ALL PASSED' if not failures else f'{failures} step(s) FAILED'}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
