"""Test site for the scan-after-click repro.

/form     the search form (page a). Its Search button navigates after `nav` ms to
          /results with the r* parameters. `frames` cross-site iframes (the other
          host), `heavy` extra elements, `store=0` sends Cache-Control: no-store.
/results  the results page (page b): answered after `delay` ms (headers and all),
          or with `stream=1` headers at once and the body spread over `delay` ms.
          `frames` cross-site iframes, `heavy` result cards, `slow` subresources
          that take `slowms` ms each.
/frame    a cross-site iframe document.
/log      POST: the pages' pagehide/pageshow beacons, appended to the log file.
/slow     a subresource answered after `ms` ms.

Usage: server.py <port> <logfile>. Pages put the other host in their iframes:
127.0.0.1 pages embed localhost frames and the other way round.
"""
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

PORT = int(sys.argv[1])
LOG = sys.argv[2]
lock = threading.Lock()


def log(entry):
    entry["srv_t"] = int(time.time() * 1000)
    with lock, open(LOG, "a") as f:
        f.write(json.dumps(entry) + "\n")


BEACON = """
<script>
(function(){
  function beacon(type, e){
    try { navigator.sendBeacon('/log', JSON.stringify({type: type, persisted: !!(e && e.persisted), page: location.pathname, t: Date.now(), vis: document.visibilityState})); } catch (err) {}
  }
  addEventListener('pagehide', function(e){ beacon('pagehide', e); });
  addEventListener('pageshow', function(e){ beacon('pageshow', e); });
  addEventListener('freeze', function(e){ beacon('freeze', e); });
  addEventListener('resume', function(e){ beacon('resume', e); });
  document.addEventListener('visibilitychange', function(){ beacon('visibility:' + document.visibilityState); });
})();
</script>
"""


def other_host(host):
    h = (host or "").split(":")[0]
    return "localhost" if h == "127.0.0.1" else "127.0.0.1"


def q(qs, k, d):
    v = qs.get(k)
    return v[0] if v else d


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def send_html(self, body, store="1", status=200):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        if store == "0":
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            entry = json.loads(raw or b"{}")
        except Exception:
            entry = {"raw": raw.decode(errors="replace")}
        entry["host"] = self.headers.get("Host")
        log(entry)
        self.send_response(204)
        self.end_headers()

    def do_GET(self):
        u = urlparse(self.path)
        qs = parse_qs(u.query)
        host = self.headers.get("Host", f"127.0.0.1:{PORT}")
        other = f"http://{other_host(host)}:{PORT}"
        if u.path == "/form":
            return self.form(qs, other)
        if u.path == "/results":
            return self.results(qs, other)
        if u.path == "/frame":
            return self.frame(qs)
        if u.path == "/slow":
            ms = int(q(qs, "ms", "1000"))
            time.sleep(ms / 1000)
            body = b"/* slow */"
            self.send_response(200)
            self.send_header("Content-Type", q(qs, "type", "text/css"))
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_html("<!doctype html><title>x</title><p>not here", status=404)

    def form(self, qs, other):
        nav = int(q(qs, "nav", "300"))
        frames = int(q(qs, "frames", "3"))
        heavy = int(q(qs, "heavy", "300"))
        store = q(qs, "store", "1")
        # what the results page gets: every r* parameter, without its r
        rq = {k[1:]: v[0] for k, v in qs.items() if k.startswith("r") and len(k) > 1}
        target = "/results?" + urlencode(rq)
        kids = "".join(
            f'<iframe src="{other}/frame?n={i}" width="300" height="120" title="ad {i}"></iframe>' for i in range(frames)
        )
        extra = "".join(f'<li><a href="#x{i}">Offer number {i}</a> <button type="button">Save {i}</button></li>' for i in range(heavy))
        log({"type": "serve", "page": "form", "nav": nav, "target": target})
        body = f"""<!doctype html><html><head><meta charset="utf-8"><title>Find your next stay</title>
<style>body{{font:16px sans-serif;margin:20px}} #search{{position:absolute;left:40px;top:120px;width:200px;height:50px;font-size:20px}}</style>
{BEACON}</head><body>
<h1>Find your next stay</h1>
<form id="f" onsubmit="return false"><input name="ss" value="Manchester" aria-label="Destination">
<button id="search" type="submit">Search</button></form>
<div style="height:200px"></div>
<div>{kids}</div>
<ul>{extra}</ul>
<script>
document.getElementById('search').addEventListener('click', function(){{
  try {{ navigator.sendBeacon('/log', JSON.stringify({{type:'click', t: Date.now()}})); }} catch (e) {{}}
  setTimeout(function(){{ location.href = {json.dumps(target)}; }}, {nav});
}});
</script>
</body></html>"""
        self.send_html(body, store=store)

    def results(self, qs, other):
        delay = int(q(qs, "delay", "2000"))
        stream = q(qs, "stream", "0") == "1"
        frames = int(q(qs, "frames", "4"))
        heavy = int(q(qs, "heavy", "400"))
        slow = int(q(qs, "slow", "3"))
        slowms = int(q(qs, "slowms", "8000"))
        store = q(qs, "store", "1")
        log({"type": "serve", "page": "results-start", "delay": delay, "stream": stream})
        head = f"""<!doctype html><html><head><meta charset="utf-8"><title>Manchester: 600 properties found</title>
{BEACON}
{''.join(f'<link rel="stylesheet" href="/slow?ms={slowms}&n={i}">' for i in range(0))}
</head><body><h1>Manchester: 600 properties found</h1>"""
        cards = []
        for i in range(heavy):
            cards.append(
                f'<div class="card" role="listitem"><h3><a href="#p{i}">Hotel number {i}</a></h3>'
                f'<span>Price £{100 + i}</span> <button type="button">See availability</button></div>'
            )
        kids = "".join(
            f'<iframe src="{other}/frame?n={i}&r=1" width="320" height="140" title="frame {i}"></iframe>' for i in range(frames)
        )
        slows = "".join(f'<img src="/slow?ms={slowms}&n={i}&type=image/png" width="10" height="10" alt="">' for i in range(slow))
        tail = "</body></html>"
        if not stream:
            time.sleep(delay / 1000)
            log({"type": "serve", "page": "results-headers"})
            return self.send_html(head + kids + "".join(cards) + slows + tail, store=store)
        # streamed: headers now, the body in pieces over `delay` ms
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        if store == "0":
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        log({"type": "serve", "page": "results-headers", "stream": True})
        pieces = [head, kids[: len(kids) // 2]] + ["".join(cards[i:i + 50]) for i in range(0, len(cards), 50)] + [kids[len(kids) // 2:], slows, tail]
        per = delay / 1000 / max(len(pieces), 1)
        try:
            for p in pieces:
                self.wfile.write(p.encode())
                self.wfile.flush()
                time.sleep(per)
        except Exception as e:
            log({"type": "serve", "page": "results-stream-broken", "err": str(e)})
        self.close_connection = True

    def frame(self, qs):
        n = q(qs, "n", "0")
        body = f"""<!doctype html><html><head><meta charset="utf-8"><title>frame {n}</title></head>
<body style="margin:0;font:13px sans-serif;background:#eef"><p>Sponsored {n}</p><a href="#a">Learn more</a> <button>Close ad</button>
{''.join(f'<span>chip {k}</span> ' for k in range(30))}</body></html>"""
        self.send_html(body)


ThreadingHTTPServer.daemon_threads = True
srv = ThreadingHTTPServer(("127.0.0.1", PORT), H)
print(f"serving on {PORT}", flush=True)
srv.serve_forever()
