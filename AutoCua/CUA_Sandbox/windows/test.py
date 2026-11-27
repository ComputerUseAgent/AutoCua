"""Show the agent's second desktop (AutoCua SB) live, and call the agent's
tools on it.

From D:\\AutoCua:
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py
        A window with the second desktop in it, live, and a tool call under
        it: pick the tool (open_app, typewrite, hotkey), type what it takes
        (an app's name, the text, the keys) and press Enter. A line under the
        field says how it went and how long it took. Closing the window ends
        everything that runs on the second desktop.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py --shot shot.png
        Save one picture of it and exit.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\windows\\test.py --open notepad --type "hello" --key ctrl+a --scan scan.png
        Make tool calls there first, in the order given (--open, --type and
        --key can repeat), then save the agent's scan of it: the picture with
        the numbered boxes, as the model gets it, and the list of what can be
        clicked or typed in on the terminal.
    --wait SEC   seconds the apps get before --shot or --scan (default 3)

No tool is written here. This file starts the sandbox and makes each call
through the agent's own controller (windows/controller/view.py), set to
AutoCua SB as an AutoCua SB run sets it: the same tools, called the same way.

Nothing is built and no program is started for it. The second desktop is a
part of the app's own Rust module (Win_CUA.rs beside this file, loaded as
AutoCua.agent_native.cua_sandbox), reached through service.py, as the agent
reaches it. Every picture it saves goes to the debug folder at the root,
D:\\AutoCua\\debug, under the file name given.

The window is pywebview, showing the one page in this file (PAGE). The page
talks to a small web server in this file, on this PC only. The pictures go to
the page down a WebSocket: as a plain web reply, security software that
watches web traffic (seen on this PC) cut the end off every picture.
"""
import base64
import hashlib
import io
import secrets
import struct
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent                # AutoCua\CUA_Sandbox\windows
ROOT = HERE.parents[2]                                # D:\AutoCua
DEBUG = ROOT / "debug"                                # where every saved picture goes
FRAME_INTERVAL = 0.25                                 # seconds between two pictures for the page
# The agent's tools that work in AutoCua SB so far, and the name each gives
# what it takes (windows/tool_registry/service.rs).
TOOLS = {"open_app": "value", "typewrite": "text", "hotkey": "value"}

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
  input, select { padding: 8px 10px; font: inherit; color: #eee; background: #222; border: 1px solid #444; border-radius: 6px; }
  input { flex: 1; }
  button { padding: 8px 18px; font: inherit; color: #eee; background: #2d6cdf; border: 0; border-radius: 6px; }
  pre { margin: 0; padding: 0 12px 10px; height: 4.6em; overflow: auto; color: #9c9; font: 12px Consolas, monospace; white-space: pre-wrap; }
</style>
</head>
<body>
<div id="screen"><img id="desktop" alt="the second desktop"></div>
<form id="call">
  <select id="tool">
    <option value="open_app" data-takes="an app's name">open_app</option>
    <option value="typewrite" data-takes="the text to type">typewrite</option>
    <option value="hotkey" data-takes="the keys, like ctrl+s">hotkey</option>
  </select>
  <input id="value" autocomplete="off" autofocus>
  <button>Call</button>
</form>
<pre id="log"></pre>
<script>
  const desktop = document.getElementById("desktop");
  const tool = document.getElementById("tool");
  const value = document.getElementById("value");
  const log = document.getElementById("log");

  // The picture: every new one comes down a WebSocket.
  function live() {
    const socket = new WebSocket(location.href.replace(/^http/, "ws") + "live");
    socket.onmessage = event => {
      const old = desktop.src;
      desktop.src = URL.createObjectURL(new Blob([event.data], { type: "image/jpeg" }));
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

  // The field says what the picked tool takes.
  function takes() {
    value.placeholder = "Type " + tool.selectedOptions[0].dataset.takes + " and press Enter";
    value.focus();
  }
  tool.addEventListener("change", takes);

  document.getElementById("call").addEventListener("submit", event => {
    event.preventDefault();
    if (value.value.trim()) fetch("call/" + tool.value, { method: "POST", body: value.value });
    value.select();
  });

  takes();
  live();
  lines();
</script>
</body>
</html>
"""


def sandbox_service():
    """AutoCua SB's service (service.py beside this file), with the app's Rust
    module built first, as the app does on its own start."""
    sys.path.insert(0, str(ROOT))
    from AutoCua.utils.rust import ensure_built
    ensure_built()
    from AutoCua.CUA_Sandbox.windows import service
    return service


def agent_controller():
    """The agent's own controller (windows/controller/view.py), set to AutoCua
    SB as an AutoCua SB run sets it: its tools are the ones called here."""
    from AutoCua.windows.controller.view import ControllerView
    controller = ControllerView()
    controller.control = "sandbox"
    return controller


def call(controller, tool, value):
    """One tool call, made as the agent makes it. Gives a line on how it went."""
    began = time.monotonic()
    result = controller.route_action([{"type": tool, TOOLS[tool]: value}])
    return f"{tool} {value!r}: {result.get('message') or result.get('status')} ({time.monotonic() - began:.1f} s)"


class Desktop:
    """The second desktop for the page: its latest picture, taken every
    FRAME_INTERVAL, and the lines the page shows."""

    def __init__(self, service):
        self.service = service
        self.sandbox = service.sandbox()
        self.controller = agent_controller()
        self.fresh = threading.Condition()   # told of every new picture
        self.frame, self.number = b"", 0
        self.lock = threading.Lock()
        self.lines = []
        self.closed = False
        threading.Thread(target=self.watch, daemon=True).start()

    def watch(self):
        """A picture of the second desktop every FRAME_INTERVAL, as a JPEG the page can show."""
        while not self.closed:
            try:
                packed = io.BytesIO()
                self.sandbox.shot().save(packed, format="JPEG", quality=85)
            except Exception as e:
                self.say(f"no picture: {e}")
                return
            with self.fresh:
                self.frame, self.number = packed.getvalue(), self.number + 1
                self.fresh.notify_all()
            time.sleep(FRAME_INTERVAL)

    def say(self, text):
        with self.lock:
            self.lines.append(text)
            del self.lines[:-50]

    def call(self, tool, value):
        """A tool call from the page, and a line on how it went."""
        self.say(f"{tool} {value!r} ...")
        self.say(call(self.controller, tool, value))

    def close(self):
        """Every app on the second desktop ends with it."""
        self.closed = True
        self.service.stop()


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
            moment it is taken."""
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
                        # The length in its shortest form: a browser closes the connection on any other.
                        size = len(frame)
                        if size < 126:
                            length = bytes([size])
                        elif size < 65536:
                            length = b"\x7e" + struct.pack(">H", size)
                        else:
                            length = b"\x7f" + struct.pack(">Q", size)
                        self.wfile.write(b"\x82" + length + frame)
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
            tool = self.path.rpartition("/")[2]
            # Text to type goes as it is; a name or keys with their spaces tidied.
            text = body[:300] if tool == "typewrite" else " ".join(body.split())[:300]
            if self.path == f"/{token}/call/{tool}" and tool in TOOLS and text.strip():
                # On a thread of its own: the page gets its answer at once, and the line when the call is done.
                threading.Thread(target=desktop.call, args=(tool, text), daemon=True).start()
                self.reply(204)
            else:
                self.reply(404)

    return ThreadingHTTPServer(("127.0.0.1", 0), Handler)


def show(service):
    """The window: the second desktop live, and the tool call under it."""
    import webview   # pywebview

    desktop = Desktop(service)
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


def debug_file(name):
    """Where a picture is saved: the debug folder at the root, under the file
    name given (a folder in it is dropped)."""
    DEBUG.mkdir(exist_ok=True)
    return DEBUG / Path(name).name


def once(service, calls, shot, scan, wait):
    """Make the tool calls in order, a second apart, give the apps `wait`
    seconds, then save a picture or the agent's scan."""
    try:
        sandbox = service.sandbox()
        controller = agent_controller()
        for tool, value in calls:
            print(call(controller, tool, value), flush=True)
            time.sleep(1)
        time.sleep(wait)
        if shot:
            path = debug_file(shot)
            sandbox.shot().save(path)
            print(f"picture of the second desktop: {path}")
        if scan:
            scanner = service.SandboxScanner()
            scanner.scan_elements()
            tree, picture, _ = scanner.get_scan_data()
            path = debug_file(scan)
            with io.BytesIO(base64.b64decode(picture)) as packed:
                from PIL import Image
                Image.open(packed).save(path)
            print(tree)
            print(f"the agent's scan, {len(scanner.get_elements_mapping())} elements numbered on {path}")
    finally:
        service.stop()
    return 0


def main():
    if sys.platform != "win32":
        raise SystemExit("this second desktop is for Windows only")
    sys.stdout.reconfigure(errors="replace")    # cp1252 when piped on Windows: a scanned name or a typed text can hold any character
    args = sys.argv[1:]
    tools = {"--open": "open_app", "--type": "typewrite", "--key": "hotkey"}
    calls, shot, scan, wait = [], None, None, 3.0
    while args:
        option = args.pop(0)
        if option in (*tools, "--shot", "--scan", "--wait") and args:
            value = args.pop(0)
            if option in tools:
                calls.append((tools[option], value))
            elif option == "--shot":
                shot = value
            elif option == "--scan":
                scan = value
            else:
                wait = float(value)
        else:
            raise SystemExit(f"unknown option, or one with no value: {option}\n\n{__doc__}")
    service = sandbox_service()
    if not (calls or shot or scan):
        return show(service)
    return once(service, calls, shot, scan, wait)


if __name__ == "__main__":
    sys.exit(main())
