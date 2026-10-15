# Mirror of static_files.kara: a static file server in the shape of nginx's
# or Caddy's file handler, over real files and a real loopback socket.
import os, socket

PORT = 47745
ROOT = "www"
BOUNDARY = "KARA_BYTERANGES"

CRC = []
for n in range(256):
    c = n
    for _ in range(8):
        c = (0xEDB88320 ^ (c >> 1)) if c & 1 else c >> 1
    CRC.append(c)

def crc32(b):
    c = 0xFFFFFFFF
    for x in b:
        c = CRC[(c ^ x) & 0xFF] ^ (c >> 8)
    return c ^ 0xFFFFFFFF

def hex8(v):
    return f"{v:08x}"

REASONS = {200: "OK", 204: "No Content", 206: "Partial Content", 301: "Moved Permanently",
           304: "Not Modified", 400: "Bad Request", 403: "Forbidden", 404: "Not Found",
           405: "Method Not Allowed", 416: "Range Not Satisfiable", 505: "HTTP Version Not Supported"}

def mime(path):
    dot = path.rfind(".")
    ext = path[dot + 1:] if dot >= 0 and dot > path.rfind("/") else ""
    return {"html": "text/html; charset=utf-8", "css": "text/css; charset=utf-8",
            "js": "text/javascript; charset=utf-8", "json": "application/json",
            "txt": "text/plain; charset=utf-8", "log": "text/plain; charset=utf-8",
            "svg": "image/svg+xml"}.get(ext, "application/octet-stream")

def cache_control(path):
    t = mime(path)
    if t.startswith("text/html"):
        return "no-cache"
    if t.startswith("text/css") or t.startswith("text/javascript") or t == "image/svg+xml":
        return "public, max-age=3600"
    return "public, max-age=60"

def etag_of(body):
    return f'"{len(body):x}-{hex8(crc32(body))}"'

def unhex(c):
    if 48 <= c <= 57: return c - 48
    if 97 <= c <= 102: return c - 87
    if 65 <= c <= 70: return c - 55
    return -1

def pct_decode(seg):
    b = seg.encode()
    out = bytearray()
    i = 0
    while i < len(b):
        if b[i] == 37:
            if i + 2 >= len(b):
                return None
            hi = unhex(b[i + 1]); lo = unhex(b[i + 2])
            if hi < 0 or lo < 0:
                return None
            out.append(hi * 16 + lo)
            i += 3
        else:
            out.append(b[i]); i += 1
    if 0 in out or 47 in out:
        return None
    try:
        return out.decode()
    except UnicodeDecodeError:
        return None

def pct_encode(name):
    out = []
    for x in name.encode():
        ch = chr(x)
        if ch.isascii() and (ch.isalnum() or ch in "-._~"):
            out.append(ch)
        else:
            out.append(f"%{x:02X}")
    return "".join(out)

def html_escape(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")

class Resp:
    def __init__(self, status):
        self.status = status
        self.headers = []
        self.body = b""
        self.length = None  # Content-Length to send; None = omit

def error(status):
    r = Resp(status)
    r.headers.append(("Content-Type", "text/plain; charset=utf-8"))
    r.body = f"{status} {REASONS[status]}\n".encode()
    r.length = len(r.body)
    return r

def etag_list_matches(header, tag):
    # weak comparison, as If-None-Match requires
    h = header.strip()
    if h == "*":
        return True
    want = tag[2:] if tag.startswith("W/") else tag
    for part in h.split(","):
        p = part.strip()
        if p.startswith("W/"):
            p = p[2:]
        if p == want:
            return True
    return False

def parse_ranges(header, size):
    # None = ignore the header (serve 200); [] = unsatisfiable; else (first, last) list
    if not header.startswith("bytes="):
        return None
    specs = header[6:].split(",")
    out = []
    for raw in specs:
        s = raw.strip()
        dash = s.find("-")
        if dash < 0:
            return None
        a = s[:dash]; b = s[dash + 1:]
        if a == "":
            if not b.isdigit():
                return None
            n = int(b)
            if n == 0 or size == 0:
                continue
            out.append((max(0, size - n), size - 1))
        else:
            if not a.isdigit() or (b != "" and not b.isdigit()):
                return None
            first = int(a)
            last = size - 1
            if b != "":
                last = int(b)
                if last < first:
                    return None
            if first >= size:
                continue
            out.append((first, min(last, size - 1)))
    return out

def serve_file(path, req):
    body = open(path, "rb").read()
    tag = etag_of(body)
    t = mime(path)
    inm = req["headers"].get("if-none-match")
    if inm is not None and etag_list_matches(inm, tag):
        r = Resp(304)
        r.headers.append(("ETag", tag))
        r.headers.append(("Cache-Control", cache_control(path)))
        return r
    rng = req["headers"].get("range")
    if rng is not None and req["method"] == "GET":
        if_range = req["headers"].get("if-range")
        if if_range is not None and if_range.strip() != tag:
            rng = None
    ranges = parse_ranges(rng, len(body)) if (rng is not None and req["method"] == "GET") else None
    if ranges is not None and len(ranges) == 0:
        r = Resp(416)
        r.headers.append(("Content-Type", "text/plain; charset=utf-8"))
        r.headers.append(("Content-Range", f"bytes */{len(body)}"))
        r.body = b"416 Range Not Satisfiable\n"
        r.length = len(r.body)
        return r
    if ranges is not None and len(ranges) == 1:
        a, b = ranges[0]
        r = Resp(206)
        r.headers.append(("Content-Type", t))
        r.body = body[a:b + 1]
        r.length = len(r.body)
        r.headers.append(("ETag", tag))
        r.headers.append(("Cache-Control", cache_control(path)))
        r.headers.append(("Accept-Ranges", "bytes"))
        r.headers.append(("Content-Range", f"bytes {a}-{b}/{len(body)}"))
        return r
    if ranges is not None:
        parts = bytearray()
        for a, b in ranges:
            parts += f"--{BOUNDARY}\r\nContent-Type: {t}\r\nContent-Range: bytes {a}-{b}/{len(body)}\r\n\r\n".encode()
            parts += body[a:b + 1]
            parts += b"\r\n"
        parts += f"--{BOUNDARY}--\r\n".encode()
        r = Resp(206)
        r.headers.append(("Content-Type", f"multipart/byteranges; boundary={BOUNDARY}"))
        r.body = bytes(parts)
        r.length = len(r.body)
        r.headers.append(("ETag", tag))
        r.headers.append(("Cache-Control", cache_control(path)))
        r.headers.append(("Accept-Ranges", "bytes"))
        return r
    r = Resp(200)
    r.headers.append(("Content-Type", t))
    r.body = body
    r.length = len(body)
    r.headers.append(("ETag", tag))
    r.headers.append(("Cache-Control", cache_control(path)))
    r.headers.append(("Accept-Ranges", "bytes"))
    return r

def listing(dirpath, url):
    names = sorted(os.listdir(dirpath))
    lines = ["<!doctype html>", f"<title>Index of {html_escape(url)}</title>",
             f"<h1>Index of {html_escape(url)}</h1>", "<ul>"]
    if url != "/":
        lines.append('<li><a href="../">../</a></li>')
    for n in names:
        if n.startswith("."):
            continue
        slash = "/" if os.path.isdir(dirpath + "/" + n) else ""
        lines.append(f'<li><a href="{pct_encode(n)}{slash}">{html_escape(n)}{slash}</a></li>')
    lines.append("</ul>")
    r = Resp(200)
    r.headers.append(("Content-Type", "text/html; charset=utf-8"))
    r.body = ("\n".join(lines) + "\n").encode()
    r.length = len(r.body)
    r.headers.append(("Cache-Control", "no-cache"))
    return r

def route(req):
    m = req["method"]
    target = req["target"]
    if m == "OPTIONS":
        r = Resp(204)
        r.headers.append(("Allow", "GET, HEAD, OPTIONS"))
        return r
    if m != "GET" and m != "HEAD":
        r = error(405)
        r.headers.append(("Allow", "GET, HEAD, OPTIONS"))
        return r
    if not target.startswith("/"):
        return error(400)
    path = target
    query = ""
    for cut in ("#", "?"):
        k = path.find(cut)
        if k >= 0:
            if cut == "?":
                query = path[k:]
            path = path[:k]
    segs = []
    for raw in path.split("/")[1:]:
        if raw == "":
            continue
        s = pct_decode(raw)
        if s is None:
            return error(400)
        if s == ".":
            continue
        if s == "..":
            if not segs:
                return error(403)
            segs.pop()
            continue
        segs.append(s)
    for s in segs:
        if s.startswith("."):
            return error(404)
    fs_path = ROOT + "".join("/" + s for s in segs)
    if not os.path.exists(fs_path):
        return error(404)
    if os.path.isdir(fs_path):
        if not path.endswith("/"):
            r = Resp(301)
            loc = path + "/" + query
            r.headers.append(("Content-Type", "text/plain; charset=utf-8"))
            r.body = f"Moved to {loc}\n".encode()
            r.length = len(r.body)
            r.headers.append(("Location", loc))
            return r
        if os.path.isfile(fs_path + "/index.html"):
            return serve_file(fs_path + "/index.html", req)
        return listing(fs_path, "/" + "".join(s + "/" for s in segs))
    return serve_file(fs_path, req)

ORDER = ["Server", "Content-Type", "Content-Length", "ETag", "Cache-Control", "Accept-Ranges",
         "Content-Range", "Location", "Allow", "Connection"]

def encode(r, head_only, close):
    hs = [("Server", "kara-static")] + r.headers
    if r.length is not None:
        hs.append(("Content-Length", str(r.length)))
    if close:
        hs.append(("Connection", "close"))
    hs.sort(key=lambda h: ORDER.index(h[0]))
    out = f"HTTP/1.1 {r.status} {REASONS[r.status]}\r\n"
    for k, v in hs:
        out += f"{k}: {v}\r\n"
    out += "\r\n"
    data = out.encode()
    sent = 0
    if not head_only:
        data += r.body
        sent = len(r.body)
    return data, sent

def serve(data, log):
    out = bytearray()
    at = 0
    n = 0
    while True:
        end = data.find(b"\r\n\r\n", at)
        if end < 0:
            break
        head = data[at:end].decode()
        at = end + 4
        n += 1
        lines = head.split("\r\n")
        parts = lines[0].split(" ")
        bad = len(parts) != 3 or not parts[2].startswith("HTTP/")
        req = None
        status = 0
        if not bad:
            req = {"method": parts[0], "target": parts[1], "version": parts[2], "headers": {}}
            for ln in lines[1:]:
                c = ln.find(":")
                if c <= 0:
                    bad = True
                    break
                req["headers"][ln[:c].strip().lower()] = ln[c + 1:].strip()
        if bad:
            r = error(400)
            close = True
            label = "- -"
        elif req["version"] not in ("HTTP/1.1", "HTTP/1.0"):
            r = error(505)
            close = True
            label = f"{req['method']} {req['target']}"
        elif req["version"] == "HTTP/1.1" and "host" not in req["headers"]:
            r = error(400)
            close = True
            label = f"{req['method']} {req['target']}"
        else:
            r = route(req)
            conn = req["headers"].get("connection", "").lower()
            close = conn == "close" or (req["version"] == "HTTP/1.0" and conn != "keep-alive")
            label = f"{req['method']} {req['target']}"
        head_only = req is not None and not bad and req["method"] == "HEAD"
        data_out, sent = encode(r, head_only, close)
        out += data_out
        log.append(f"{label} {r.status} {sent}")
        if close:
            break
    return bytes(out), n

def read_all(s):
    b = bytearray()
    while True:
        chunk = s.recv(64)
        if not chunk:
            break
        b += chunk
    return bytes(b)

def shown(b):
    t = b[:48].decode()
    t = t.replace("\\", "\\\\").replace("\r", "\\r").replace("\n", "\\n")
    return t

def session(listener, title, reqs, log):
    print(f"== {title}")
    client = socket.create_connection(("127.0.0.1", PORT))
    wire = "".join(w for _, w, _ in reqs)
    client.sendall(wire.encode())
    client.shutdown(socket.SHUT_WR)
    conn, _ = listener.accept()
    out, n = serve(read_all(conn), log)
    conn.sendall(out)
    conn.close()
    got = read_all(client)
    client.close()
    print(f"  server: {n} requests read")
    at = 0
    tags = {}
    for label, _, no_body in reqs:
        if at >= len(got):
            print(f"  {label} -> (no response)")
            continue
        end = got.find(b"\r\n\r\n", at)
        lines = got[at:end].decode().split("\r\n")
        print(f"  {label} -> {lines[0]}")
        length = 0
        ctype = ""
        for ln in lines[1:]:
            k, v = ln.split(": ", 1)
            if k == "Server":
                continue
            if k == "Content-Length":
                length = int(v)
            if k == "ETag":
                tags[label] = v
            if k == "Content-Type":
                ctype = v
            print(f"    {k}: {v}")
        at = end + 4
        status = int(lines[0].split(" ")[1])
        if no_body or status in (204, 304):
            continue
        body = got[at:at + length]
        at += length
        print(f'    body {len(body)} bytes crc {hex8(crc32(body))} "{shown(body)}"')
        if ctype.startswith("text/html") or ctype.startswith("multipart/"):
            for ln in body.decode().split("\n")[:-1]:
                print("    | " + ln.replace("\r", "\\r"))
    return tags

def req(method, target, extra=None, version="HTTP/1.1", host=True):
    w = f"{method} {target} {version}\r\n"
    if host:
        w += "Host: localhost\r\n"
    for h in extra or []:
        w += h + "\r\n"
    w += "\r\n"
    return (f"{method} {target}" + (f" [{'; '.join(extra)}]" if extra else ""), w, method == "HEAD")

def write_site():
    files = [
        ("index.html", "<!doctype html>\n<title>Kara static</title>\n<h1>It works</h1>\n"),
        ("css/site.css", "body { font: 16px/1.4 sans-serif; }\nh1 { color: #c33; }\n"),
        ("js/app.js", "console.log('ready');\n"),
        ("data/report.json", '{"ok": true, "rows": 3}\n'),
        ("docs/guide.txt", "Read the guide.\n"),
        ("docs/notes v2.txt", "Second notes.\n"),
        ("docs/café.txt", "Un café, s'il vous plaît.\n"),
        ("docs/a&b <c>.txt", "Odd name.\n"),
        ("docs/img/logo.svg", '<svg xmlns="http://www.w3.org/2000/svg"/>\n'),
        ("docs/.draft.txt", "not yet\n"),
        (".env", "SECRET=hunter2\n"),
        ("empty.txt", ""),
    ]
    levels = ["info", "info", "info", "warn", "error"]
    seed = 12345
    log = []
    for i in range(150):
        seed = (seed * 1103515245 + 12345) % 2147483648
        lvl = levels[seed % 5]
        ms = (seed >> 8) % 900 + 3
        log.append(f"2026-10-15T04:{i // 60:02}:{i % 60:02}Z level={lvl} req={1000 + i} took={ms}ms\n")
    files.append(("logs/app.log", "".join(log)))
    for name, text in files:
        p = ROOT + "/" + name
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(text)
        print(f"  wrote {name} ({len(text.encode())} bytes)")

def main():
    print("== site")
    write_site()
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("127.0.0.1", PORT))
    listener.listen(8)
    log = []
    tags = session(listener, "basics", [
        req("GET", "/"),
        req("GET", "/css/site.css"),
        req("HEAD", "/js/app.js"),
        req("GET", "/data/report.json?cache=no#top"),
        req("GET", "/missing.txt"),
        req("GET", "/docs?view=list"),
        req("GET", "/docs/"),
        req("GET", "/docs/caf%C3%A9.txt"),
        req("GET", "/docs/notes%20v2.txt"),
        req("GET", "/docs/a%26b%20%3Cc%3E.txt"),
        req("GET", "/docs/img"),
        req("GET", "/logs/"),
        req("GET", "/./css//site.css"),
    ], log)
    css = tags["GET /css/site.css"]
    tag_log = None
    session(listener, "conditional", [
        req("GET", "/css/site.css", [f"If-None-Match: {css}"]),
        req("GET", "/css/site.css", [f"If-None-Match: W/{css}"]),
        req("GET", "/css/site.css", [f'If-None-Match: "nope", {css}']),
        req("GET", "/css/site.css", ["If-None-Match: *"]),
        req("GET", "/css/site.css", ['If-None-Match: "nope"']),
        req("HEAD", "/css/site.css", [f"If-None-Match: {css}"]),
    ], log)
    head = session(listener, "log etag", [req("HEAD", "/logs/app.log")], log)
    tag_log = head["HEAD /logs/app.log"]
    session(listener, "ranges", [
        req("GET", "/logs/app.log", ["Range: bytes=0-99"]),
        req("GET", "/logs/app.log", ["Range: bytes=-120"]),
        req("GET", "/logs/app.log", ["Range: bytes=7000-"]),
        req("GET", "/logs/app.log", ["Range: bytes=0-9, 50-59, -10"]),
        req("GET", "/logs/app.log", ["Range: bytes=99999-100000"]),
        req("GET", "/logs/app.log", ["Range: bytes=abc"]),
        req("GET", "/logs/app.log", ["Range: bytes=5-2"]),
        req("GET", "/logs/app.log", ["Range: items=0-5"]),
        req("GET", "/logs/app.log", ["Range: bytes=0-4", f"If-Range: {tag_log}"]),
        req("GET", "/logs/app.log", ["Range: bytes=0-4", 'If-Range: "stale"']),
        req("HEAD", "/logs/app.log", ["Range: bytes=0-4"]),
        req("GET", "/logs/app.log", ["Range: bytes=99999-, 0-0"]),
        req("GET", "/empty.txt", ["Range: bytes=0-0"]),
        req("GET", "/empty.txt", ["Range: bytes=-5"]),
        req("GET", "/empty.txt"),
    ], log)
    session(listener, "hostile", [
        req("GET", "/../.env"),
        req("GET", "/docs/../../.env"),
        req("GET", "/%2e%2e/.env"),
        req("GET", "/.env"),
        req("GET", "/docs/.draft.txt"),
        req("GET", "/docs/%2Fetc"),
        req("GET", "/bad%zz"),
        req("GET", "/trunc%4"),
        req("GET", "/nul%00.txt"),
        req("GET", "/docs/img/../../index.html"),
        req("POST", "/index.html"),
        req("DELETE", "/index.html"),
        req("OPTIONS", "*"),
        req("GET", "index.html"),
        req("GET", "/", ["Connection: close"]),
        req("GET", "/js/app.js"),
    ], log)
    session(listener, "http/1.0", [
        req("GET", "/js/app.js", None, "HTTP/1.0", False),
        req("GET", "/", None, "HTTP/1.0", False),
    ], log)
    session(listener, "http/1.0 keep-alive", [
        req("GET", "/js/app.js", ["Connection: keep-alive"], "HTTP/1.0", False),
        req("GET", "/data/report.json", None, "HTTP/1.0", False),
    ], log)
    session(listener, "malformed", [
        ("GARBAGE", "GARBAGE\r\n\r\n", False),
        req("GET", "/"),
    ], log)
    session(listener, "no host", [req("GET", "/", None, "HTTP/1.1", False)], log)
    session(listener, "bad header", [("GET / [broken header]", "GET / HTTP/1.1\r\nHost: x\r\nbroken\r\n\r\n", False)], log)
    session(listener, "version", [req("GET", "/", None, "HTTP/2.0")], log)
    listener.close()

    with open("access.log", "w") as f:
        f.write("".join(l + "\n" for l in log))
    back = open("access.log").read().splitlines()
    print("== access.log")
    counts = {}
    total = 0
    for ln in back:
        f = ln.split(" ")
        counts[f[-2]] = counts.get(f[-2], 0) + 1
        total += int(f[-1])
    print(f"  {len(back)} requests, {total} body bytes")
    print("  " + ", ".join(f"{k}x{counts[k]}" for k in sorted(counts)))
    for ln in back[:3]:
        print(f"  {ln}")

main()
