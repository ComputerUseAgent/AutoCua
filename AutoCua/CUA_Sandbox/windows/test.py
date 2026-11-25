"""Build Win_CUA.rs, the second desktop for the agent, and show it live.

From D:\\AutoCua:
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py
        A window with the second desktop in it, live, and a field under it.
        Type an app's name there and press Enter: the agent's own open_app
        tool for Windows (AutoCua\\windows\\controller\\tool\\open_app.py)
        opens that app on the second desktop, and a line under the field says
        how it went and how long it took. Closing the window ends everything
        that runs on the second desktop.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py --computer-use
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py --run APP --scan scan.png
        With options, they go to the program as they are: the top of
        Win_CUA.rs says what it takes.

The program is built with rustc on the first run, and again whenever
Win_CUA.rs changes, into the system temp folder (%TEMP%\\AutoCua\\Win_CUA.exe),
never beside the code. It runs from the root, D:\\AutoCua, so every picture it
saves goes to the debug folder there, D:\\AutoCua\\debug.

The window is pywebview, showing the one page in this file (PAGE). The page
talks to a small web server in this file, on this PC only, which gets the
pictures from the program (Win_CUA.exe --serve). The pictures go to the page
down a WebSocket: as a plain web reply, security software that watches web
traffic (seen on this PC) cut the end off every picture.
"""
import base64
import hashlib
import secrets
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent                # AutoCua\CUA_Sandbox\windows
ROOT = HERE.parents[2]                                # D:\AutoCua: the program runs here
SOURCE = HERE / "Win_CUA.rs"
# The built program is a cache outside the code, made again when it is
# missing or older than the source.
PROGRAM = Path(tempfile.gettempdir()) / "AutoCua" / "Win_CUA.exe"
OPEN_APP = HERE.parents[1] / "windows" / "controller" / "tool" / "open_app.py"
# Python with no console window, for the half of this file that runs on the second desktop.
PYTHONW = Path(sys.executable).with_name("pythonw.exe")

PAGE = """<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Second Windows</title>
<style>
  html, body { height: 100%; margin: 0; background: #111; color: #ddd; font: 14px "Segoe UI", sans-serif; }
  body { display: flex; flex-direction: column; }
  #screen { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; background: #000; }
  #screen img { max-width: 100%; max-height: 100%; }
  form { display: flex; gap: 8px; padding: 10px 10px 6px; }
  input { flex: 1; padding: 8px 10px; font: inherit; color: #eee; background: #222; border: 1px solid #444; border-radius: 6px; }
  button { padding: 8px 18px; font: inherit; color: #eee; background: #2d6cdf; border: 0; border-radius: 6px; }
  pre { margin: 0; padding: 0 12px 10px; height: 4.6em; overflow: auto; color: #9c9; font: 12px Consolas, monospace; white-space: pre-wrap; }
</style>
</head>
<body>
<div id="screen"><img id="desktop" alt="the second desktop"></div>
<form id="open">
  <input id="app" placeholder="Type an app's name and press Enter" autocomplete="off" autofocus>
  <button>Open</button>
</form>
<pre id="log"></pre>
<script>
  const desktop = document.getElementById("desktop");
  const app = document.getElementById("app");
  const log = document.getElementById("log");

  // The picture: every new one comes down a WebSocket the moment the desktop changes.
  function live() {
    const socket = new WebSocket(location.href.replace(/^http/, "ws") + "live");
    socket.onmessage = event => {
      const old = desktop.src;
      desktop.src = URL.createObjectURL(new Blob([event.data], { type: "image/bmp" }));
      if (old) URL.revokeObjectURL(old);
    };
    socket.onclose = () => setTimeout(live, 1000);
  }

  // The lines under the field: what was asked and how it went.
  async function lines() {
    try {
      const text = await (await fetch("log")).text();
      if (text !== log.textContent) {
        log.textContent = text;
        log.scrollTop = log.scrollHeight;
      }
    } catch (e) {}
    setTimeout(lines, 400);
  }

  document.getElementById("open").addEventListener("submit", event => {
    event.preventDefault();
    const name = app.value.trim();
    if (name) fetch("open", { method: "POST", body: name });
    app.select();
  });

  live();
  lines();
</script>
</body>
</html>
"""


def build():
    """Compile Win_CUA.rs when there is no program yet or the source is newer."""
    if PROGRAM.exists() and PROGRAM.stat().st_mtime >= SOURCE.stat().st_mtime:
        return
    rustc = shutil.which("rustc") or str(Path.home() / ".cargo" / "bin" / "rustc.exe")
    if not Path(rustc).exists():
        raise SystemExit("rustc not found: run setup_windows.bat, or install Rust from https://rustup.rs")
    print("Building Win_CUA.rs...", flush=True)
    PROGRAM.parent.mkdir(parents=True, exist_ok=True)
    # run in the build folder: whatever rustc writes on the way lands there too
    if subprocess.call([rustc, "--edition", "2021", "-O", str(SOURCE), "-o", str(PROGRAM)], cwd=PROGRAM.parent) != 0:
        raise SystemExit("Win_CUA.rs did not build")


class Desktop:
    """The running program (Win_CUA.exe --serve) and what it sent last: the
    picture of the second desktop, and the lines the page shows."""

    def __init__(self):
        self.program = subprocess.Popen([str(PROGRAM), "--serve"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        cwd=ROOT)
        self.fresh = threading.Condition()   # told of every new picture
        self.frame, self.number = b"", 0
        self.lock = threading.Lock()
        self.lines = []
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        """Every message of the program: a line "<kind> <bytes>", then that many bytes."""
        out = self.program.stdout
        while True:
            head = out.readline().split()
            if len(head) != 2:
                break
            data = out.read(int(head[1]))
            if head[0] == b"frame":
                with self.fresh:
                    self.frame, self.number = data, self.number + 1
                    self.fresh.notify_all()
            else:
                self.say(data.decode("utf-8", "replace"))

    def say(self, text):
        with self.lock:
            self.lines.append(text)
            del self.lines[:-50]

    def run(self, command):
        """Start a command line on the second desktop."""
        try:
            with self.lock:
                self.program.stdin.write(b"run " + command.encode("utf-8") + b"\n")
                self.program.stdin.flush()
        except OSError:
            self.say("the program is not running any more")

    def close(self):
        """The program ends when its stdin closes, and every app on the second desktop with it."""
        try:
            self.program.stdin.close()
            self.program.wait(timeout=10)
        except Exception:
            self.program.kill()


def page_server(desktop, token):
    """The web server behind the page, on this PC only. `token` is a random
    word in every address, so no other page or program can use it."""

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"   # the page keeps its connections open

        def log_message(self, *args):   # no line in the terminal for every request
            pass

        def reply(self, code, kind="text/plain; charset=utf-8", body=b""):
            self.send_response(code)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def live(self):
            """The pictures, each sent to the page as one WebSocket message the
            moment it comes from the program."""
            key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            self.send_response(101, "Switching Protocols")
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
            self.end_headers()
            self.close_connection = True
            print("The window is showing the second desktop.", flush=True)
            sent = 0
            try:
                while True:
                    with desktop.fresh:
                        desktop.fresh.wait_for(lambda: desktop.number != sent, timeout=5)
                        frame, number = desktop.frame, desktop.number
                    if number == sent:
                        self.wfile.write(b"\x89\x00")   # nothing new: a ping, which fails once the page is gone
                    else:
                        self.wfile.write(b"\x82\x7f" + struct.pack(">Q", len(frame)) + frame)
                        sent = number
            except OSError:
                pass   # the page closed

        def do_GET(self):
            if self.path == f"/{token}/":
                self.reply(200, "text/html; charset=utf-8", PAGE.encode("utf-8"))
            elif self.path == f"/{token}/live" and self.headers.get("Sec-WebSocket-Key"):
                self.live()
            elif self.path == f"/{token}/log":
                with desktop.lock:
                    text = "\n".join(desktop.lines)
                self.reply(200, body=text.encode("utf-8"))
            else:
                self.reply(404)

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode("utf-8", "replace")
            if self.path == f"/{token}/run" and body.strip():
                # From this file's other half: a Store app's program file, for the sandbox's program to start.
                desktop.run(" ".join(body.splitlines()).strip())
                self.reply(204)
                return
            text = " ".join(body.split())[:300]
            if self.path == f"/{token}/open" and text:
                # This file's other half, started on the second desktop, opens the app there.
                tell = f"http://127.0.0.1:{self.server.server_address[1]}/{token}/said"
                desktop.say(f"opening {text!r} ...")
                desktop.run(subprocess.list2cmdline(
                    [str(PYTHONW), str(Path(__file__).resolve()), "--open", text, "--tell", tell]))
                self.reply(204)
            elif self.path == f"/{token}/said" and text:
                desktop.say(text)
                self.reply(204)
            else:
                self.reply(404)

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)


def show():
    """The window: the second desktop live, and the field that opens an app on it."""
    import webview   # pywebview

    desktop = Desktop()
    token = secrets.token_urlsafe(8)
    server = page_server(desktop, token)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/{token}/"
    print(f"The page: {url}", flush=True)
    try:
        webview.create_window("Second Windows", url, width=1280, height=880)
        webview.start()
    finally:
        desktop.close()
    return 0


def store_app_program(app_id):
    """The program file of a Store app, from its Start menu id (family!app).
    None when the app is not an ordinary Windows program in a package: that
    kind only runs on the main desktop."""
    import ctypes
    import xml.etree.ElementTree as ET

    family, _, app = app_id.partition("!")
    kernel32 = ctypes.WinDLL("kernel32")
    count, length = ctypes.c_uint32(0), ctypes.c_uint32(0)
    kernel32.GetPackagesByPackageFamily(family, ctypes.byref(count), None, ctypes.byref(length), None)
    if not count.value:
        return None
    names = (ctypes.c_wchar_p * count.value)()
    text = ctypes.create_unicode_buffer(length.value)
    if kernel32.GetPackagesByPackageFamily(family, ctypes.byref(count), names, ctypes.byref(length), text):
        return None
    for full_name in names:
        size = ctypes.c_uint32(0)
        kernel32.GetPackagePathByFullName(full_name, ctypes.byref(size), None)
        folder = ctypes.create_unicode_buffer(size.value)
        if kernel32.GetPackagePathByFullName(full_name, ctypes.byref(size), folder):
            continue
        try:
            root = ET.parse(Path(folder.value) / "AppxManifest.xml").getroot()
        except (OSError, ET.ParseError):
            continue
        for entry in root.iter():
            if entry.tag.endswith("}Application") and entry.get("Id") == app:
                ordinary = entry.get("EntryPoint") == "Windows.FullTrustApplication" or any(
                    key.endswith("}RuntimeBehavior") and value in ("packagedClassicApp", "win32App")
                    for key, value in entry.attrib.items())
                program = entry.get("Executable")
                return str(Path(folder.value) / program) if ordinary and program else None
    return None


def open_there(name, tell):
    """Runs on the second desktop, started there by the page: the agent's own
    open_app tool for Windows opens `name`, and the page is told how it went.

    One step of the tool is replaced here. A Store app it starts by its Start
    menu id, and then it is Windows that starts the app, on the main desktop
    (seen: Notepad and ChatGPT came up on the owner's screen). Here the app's
    own program file is started instead, by the sandbox's program: that puts
    it on the second desktop, and ends it when the window closes."""
    import importlib.util
    import urllib.request

    # straight to this PC, whatever proxy Windows is set to use
    direct = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    began = time.monotonic()
    on_main = []
    try:
        spec = importlib.util.spec_from_file_location("open_app", OPEN_APP)
        tool = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(tool)
        tool_launch = tool.launch_windows_target

        def launch(app, target):
            program = store_app_program(target[5:]) if target.startswith("appx:") else None
            if program:
                command = subprocess.list2cmdline([program])
                direct.open(tell.rsplit("/", 1)[0] + "/run", data=command.encode("utf-8"), timeout=5).close()
                return True
            if target.startswith("appx:"):
                on_main.append(app)
            return tool_launch(app, target)

        tool.launch_windows_target = launch
        result = tool.open_on_windows(name) or "no app of that name, or it did not start"
        if on_main and result == "launched":
            result = "launched on your main desktop (this kind of Store app cannot run on the second one)"
    except Exception as e:
        result = f"failed: {e!r}"
    text = f"open_app {name!r}: {result} ({time.monotonic() - began:.1f} s)"
    direct.open(tell, data=text.encode("utf-8"), timeout=5).close()
    return 0


def main():
    if sys.platform != "win32":
        raise SystemExit("Win_CUA is for Windows only")
    args = sys.argv[1:]
    if len(args) == 4 and args[0] == "--open" and args[2] == "--tell":
        return open_there(args[1], args[3])
    build()
    if not args:
        return show()
    try:
        return subprocess.call([str(PROGRAM), *args], cwd=ROOT)
    except KeyboardInterrupt:   # Ctrl+C ended the program too, and with it every app on the second desktop
        return 130


if __name__ == "__main__":
    sys.exit(main())
