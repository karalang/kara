"""Oracle for bt_tracker.kara: a BitTorrent tracker (BEP 3 announce, BEP 23
compact peer lists, BEP 48 scrape) served over HTTP on loopback, and the
peers that announce to it. Every tracker answer is a bencoded dictionary;
the compact peer list and the scrape's keys are raw bytes, not text."""

import hashlib
import http.server
import threading
import time
import urllib.error
import urllib.request

ADDR = ("127.0.0.1", 23784)
BASE = f"http://{ADDR[0]}:{ADDR[1]}"
INTERVAL = 1800
MIN_INTERVAL = 900
DEFAULT_NUMWANT = 50
MAX_NUMWANT = 200


# ---- bencode


def bencode(v):
    if isinstance(v, int):
        return b"i" + str(v).encode() + b"e"
    if isinstance(v, bytes):
        return str(len(v)).encode() + b":" + v
    if isinstance(v, str):
        return bencode(v.encode())
    if isinstance(v, list):
        return b"l" + b"".join(bencode(x) for x in v) + b"e"
    if isinstance(v, dict):
        keys = sorted((k.encode() if isinstance(k, str) else k, val) for k, val in v.items())
        return b"d" + b"".join(bencode(k) + bencode(val) for k, val in keys) + b"e"
    raise TypeError(v)


class BencodeError(Exception):
    pass


def bdecode(data):
    v, i = _bdecode(data, 0)
    if i != len(data):
        raise BencodeError(f"{len(data) - i} bytes after the value")
    return v


def _bdecode(d, i):
    if i >= len(d):
        raise BencodeError("ends early")
    c = d[i]
    if c == ord("i"):
        e = d.find(b"e", i)
        if e < 0:
            raise BencodeError("integer has no end")
        return int(d[i + 1:e]), e + 1
    if c == ord("l"):
        i += 1
        out = []
        while i < len(d) and d[i] != ord("e"):
            v, i = _bdecode(d, i)
            out.append(v)
        if i >= len(d):
            raise BencodeError("list has no end")
        return out, i + 1
    if c == ord("d"):
        i += 1
        out = {}
        while i < len(d) and d[i] != ord("e"):
            k, i = _bdecode(d, i)
            if not isinstance(k, bytes):
                raise BencodeError("a key that is not a string")
            v, i = _bdecode(d, i)
            out[k] = v
        if i >= len(d):
            raise BencodeError("dictionary has no end")
        return out, i + 1
    if ord("0") <= c <= ord("9"):
        colon = d.find(b":", i)
        if colon < 0:
            raise BencodeError("string has no colon")
        n = int(d[i:colon])
        if colon + 1 + n > len(d):
            raise BencodeError("string runs past the end")
        return d[colon + 1:colon + 1 + n], colon + 1 + n
    raise BencodeError(f"unexpected byte 0x{c:02x}")


# ---- metainfo


def make_info(name, length, piece_len):
    pieces = b""
    n = (length + piece_len - 1) // piece_len
    for i in range(n):
        pieces += hashlib.sha1(f"{name}:{i}".encode()).digest()
    return {"length": length, "name": name, "piece length": piece_len, "pieces": pieces}


def pct(data):
    safe = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"
    return "".join(chr(b) if b in safe else f"%{b:02X}" for b in data)


def unpct(text):
    out = bytearray()
    i = 0
    raw = text.encode()
    while i < len(raw):
        if raw[i] == ord("%"):
            h = raw[i + 1:i + 3]
            if len(h) < 2 or any(x not in b"0123456789abcdefABCDEF" for x in h):
                return None
            out.append(int(h, 16))
            i += 3
        else:
            out.append(raw[i])
            i += 1
    return bytes(out)


# ---- the tracker


class Torrent:
    def __init__(self, name):
        self.name = name
        self.peers = {}  # peer_id -> (ip, port, left)
        self.downloaded = 0

    def seeds(self):
        return sum(1 for p in self.peers.values() if p[2] == 0)


class Tracker:
    def __init__(self):
        self.torrents = {}
        self.lock = threading.Lock()
        self.stats = {"announce": 0, "scrape": 0, "failure": 0, "notfound": 0}

    def register(self, info_hash, name):
        self.torrents[info_hash] = Torrent(name)

    def handle(self, path, query):
        with self.lock:
            if path == "/announce":
                self.stats["announce"] += 1
                return 200, "text/plain", self.announce(query)
            if path == "/scrape":
                self.stats["scrape"] += 1
                return 200, "text/plain", self.scrape(query)
            if path == "/stats":
                s = self.stats
                peers = sum(len(t.peers) for t in self.torrents.values())
                return 200, "text/plain", (
                    f"torrents {len(self.torrents)} peers {peers} announces {s['announce']} "
                    f"scrapes {s['scrape']} failures {s['failure']} not found {s['notfound']}"
                ).encode()
            self.stats["notfound"] += 1
            return 404, "text/plain", f"no route for {path}".encode()

    def fail(self, why):
        self.stats["failure"] += 1
        return bencode({"failure reason": why})

    def announce(self, q):
        params = {}
        for k, v in q:
            params.setdefault(k, v)
        if "info_hash" not in params:
            return self.fail("missing info_hash")
        ih = params["info_hash"]
        if len(ih) != 20:
            return self.fail(f"info_hash is {len(ih)} bytes, not 20")
        t = self.torrents.get(ih)
        if t is None:
            return self.fail("unregistered torrent")
        if "peer_id" not in params:
            return self.fail("missing peer_id")
        pid = params["peer_id"]
        if len(pid) != 20:
            return self.fail(f"peer_id is {len(pid)} bytes, not 20")
        nums = {}
        for k in ("port", "uploaded", "downloaded", "left"):
            raw = params.get(k)
            if raw is None:
                return self.fail(f"missing {k}")
            if not raw.isdigit():
                return self.fail(f"invalid {k}")
            nums[k] = int(raw)
        if not 1 <= nums["port"] <= 65535:
            return self.fail("invalid port")
        ip = params.get("ip", b"")
        parts = ip.split(b".")
        if len(parts) != 4 or not all(p.isdigit() and int(p) < 256 for p in parts):
            return self.fail("invalid ip")
        event = params.get("event", b"").decode()
        if event not in ("", "started", "completed", "stopped"):
            return self.fail(f"invalid event {event}")
        numwant = DEFAULT_NUMWANT
        if "numwant" in params:
            if not params["numwant"].isdigit():
                return self.fail("invalid numwant")
            numwant = min(int(params["numwant"]), MAX_NUMWANT)
        compact = params.get("compact", b"1") == b"1"
        no_peer_id = params.get("no_peer_id", b"0") == b"1"

        if event == "stopped":
            t.peers.pop(pid, None)
        else:
            was = t.peers.get(pid)
            if event == "completed" and was is not None and was[2] > 0:
                t.downloaded += 1
            t.peers[pid] = (bytes(int(p) for p in parts), nums["port"], nums["left"])

        others = [(k, v) for k, v in sorted(t.peers.items()) if k != pid]
        if event == "stopped":
            others = []
        others = others[:numwant]
        if compact:
            peers = b"".join(ip4 + port.to_bytes(2, "big") for _, (ip4, port, _) in others)
        else:
            peers = []
            for k, (ip4, port, _) in others:
                d = {"ip": ".".join(str(b) for b in ip4), "port": port}
                if not no_peer_id:
                    d["peer id"] = k
                peers.append(d)
        seeds = t.seeds()
        return bencode({
            "complete": seeds,
            "downloaded": t.downloaded,
            "incomplete": len(t.peers) - seeds,
            "interval": INTERVAL,
            "min interval": MIN_INTERVAL,
            "peers": peers,
        })

    def scrape(self, q):
        wanted = [v for k, v in q if k == "info_hash"]
        if not wanted:
            wanted = sorted(self.torrents)
        files = {}
        for ih in wanted:
            t = self.torrents.get(ih)
            if t is None:
                continue
            seeds = t.seeds()
            files[ih] = {"complete": seeds, "downloaded": t.downloaded,
                         "incomplete": len(t.peers) - seeds}
        return bencode({"files": files})


TRACKER = Tracker()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        path, _, query = self.path.partition("?")
        q = []
        bad = False
        for part in query.split("&") if query else []:
            k, _, v = part.partition("=")
            kb, vb = unpct(k), unpct(v)
            if kb is None or vb is None:
                bad = True
                break
            q.append((kb.decode("latin-1"), vb))
        if bad:
            status, ctype, body = 400, "text/plain", b"bad percent-encoding"
        else:
            status, ctype, body = TRACKER.handle(path, q)
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


# ---- the peers


def show_peers(peers):
    if isinstance(peers, bytes):
        if len(peers) % 6:
            return f"compact list of {len(peers)} bytes, not a multiple of 6"
        out = []
        for i in range(0, len(peers), 6):
            ip = ".".join(str(b) for b in peers[i:i + 4])
            out.append(f"{ip}:{int.from_bytes(peers[i + 4:i + 6], 'big')}")
        return "compact [" + ", ".join(out) + "]"
    out = []
    for p in peers:
        s = f"{p[b'ip'].decode()}:{p[b'port']}"
        if b"peer id" in p:
            s += f" {p[b'peer id'].decode()}"
        out.append(s)
    return "dicts [" + ", ".join(out) + "]"


def get(path):
    for _ in range(100):
        try:
            with urllib.request.urlopen(BASE + path, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()
        except (ConnectionRefusedError, urllib.error.URLError):
            time.sleep(0.02)
    raise RuntimeError("tracker never came up")


class Peer:
    def __init__(self, name, peer_id, ip, port):
        self.name = name
        self.peer_id = peer_id
        self.ip = ip
        self.port = port

    def announce(self, ih, left, event="", extra="", uploaded=0, downloaded=0):
        q = (f"info_hash={pct(ih)}&peer_id={pct(self.peer_id)}&port={self.port}"
             f"&uploaded={uploaded}&downloaded={downloaded}&left={left}&ip={self.ip}")
        if event:
            q += f"&event={event}"
        q += extra
        status, body = get("/announce?" + q)
        what = event or "update"
        self.report(what, status, body)

    def report(self, what, status, body):
        try:
            d = bdecode(body)
        except BencodeError as e:
            print(f"{self.name}: {what} -> {status}, not bencode: {e}")
            return
        if b"failure reason" in d:
            print(f"{self.name}: {what} -> failure: {d[b'failure reason'].decode()}")
            return
        print(f"{self.name}: {what} -> {len(body)} bytes, seeds {d[b'complete']} leechers "
              f"{d[b'incomplete']} done {d[b'downloaded']} every {d[b'interval']}s "
              f"(min {d[b'min interval']}s)")
        print(f"{self.name}:   {show_peers(d[b'peers'])}")


def scrape(label, hashes, names):
    q = "&".join(f"info_hash={pct(h)}" for h in hashes)
    status, body = get("/scrape" + ("?" + q if q else ""))
    d = bdecode(body)
    files = d[b"files"]
    print(f"scrape {label} -> {status}, {len(body)} bytes, {len(files)} torrents")
    for ih in sorted(files):
        f = files[ih]
        print(f"  {ih.hex()[:16]} {names.get(ih, '?')}: seeds {f[b'complete']} "
              f"leechers {f[b'incomplete']} done {f[b'downloaded']}")


def main():
    infos = [make_info("debian-13.iso", 700_000, 65_536),
             make_info("weather-2026.tar", 40_000, 16_384)]
    names = {}
    hashes = []
    print("== metainfo")
    for info in infos:
        meta = bencode({"announce": BASE + "/announce", "created by": "kara-tracker-demo",
                        "info": info})
        ih = hashlib.sha1(bencode(info)).digest()
        hashes.append(ih)
        names[ih] = info["name"]
        TRACKER.register(ih, info["name"])
        back = bdecode(meta)
        again = hashlib.sha1(bencode(back[b"info"])).digest()
        print(f"{info['name']}: {len(meta)}-byte metainfo, {len(info['pieces']) // 20} pieces, "
              f"info_hash {ih.hex()}, round trip {str(again == ih).lower()}")
    iso, tar = hashes
    unknown = hashlib.sha1(b"not registered").digest()

    server = http.server.ThreadingHTTPServer(ADDR, Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    seed = Peer("seed", b"-KR0100-seed00000001", "10.0.0.1", 6881)
    alice = Peer("alice", b"-KR0100-alice0000002", "10.0.0.2", 6882)
    bob = Peer("bob", b"-KR0100-bob000000003", "10.0.0.3", 51413)
    carol = Peer("carol", b"-KR0100-carol0000004", "192.168.7.20", 6889)

    print("== announce")
    seed.announce(iso, 0, "started")
    alice.announce(iso, 700_000, "started")
    bob.announce(iso, 700_000, "started", "&compact=0")
    bob.announce(iso, 500_000, "", "&compact=0&no_peer_id=1&numwant=1", downloaded=200_000)
    carol.announce(iso, 700_000, "started", "&numwant=0")
    alice.announce(iso, 0, "completed", "", uploaded=50_000, downloaded=700_000)
    alice.announce(iso, 0, "completed")
    seed.announce(iso, 0, "stopped", "", uploaded=900_000)
    bob.announce(iso, 300_000, "", "&numwant=500")
    carol.announce(tar, 40_000, "started")
    seed.announce(tar, 0, "started")
    carol.announce(tar, 0, "completed", "", downloaded=40_000)

    print("== scrape")
    scrape("iso", [iso], names)
    scrape("both", [tar, iso], names)
    scrape("all", [], names)
    scrape("unknown", [unknown], names)

    print("== refused")
    bad = Peer("mallory", b"-XX0001-mallory00009", "10.0.0.9", 6881)
    for label, q in [
        ("no info_hash", "peer_id=x"),
        ("short info_hash", f"info_hash={pct(iso[:19])}&peer_id={pct(bad.peer_id)}"),
        ("unknown torrent", f"info_hash={pct(unknown)}&peer_id={pct(bad.peer_id)}"),
        ("no peer_id", f"info_hash={pct(iso)}&port=1"),
        ("long peer_id", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id + b'!')}&port=1"),
        ("port 0", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id)}&port=0&uploaded=0"
                   "&downloaded=0&left=0&ip=10.0.0.9"),
        ("left -1", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id)}&port=1&uploaded=0"
                    "&downloaded=0&left=-1&ip=10.0.0.9"),
        ("bad ip", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id)}&port=1&uploaded=0"
                   "&downloaded=0&left=0&ip=10.0.0.300"),
        ("bad event", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id)}&port=1&uploaded=0"
                      "&downloaded=0&left=0&ip=10.0.0.9&event=paused"),
        ("bad numwant", f"info_hash={pct(iso)}&peer_id={pct(bad.peer_id)}&port=1&uploaded=0"
                        "&downloaded=0&left=0&ip=10.0.0.9&numwant=lots"),
    ]:
        status, body = get("/announce?" + q)
        bad.report(label, status, body)
    status, body = get("/announce?info_hash=%G1")
    print(f"mallory: bad escape -> {status} {body.decode()}")
    status, body = get("/peers")
    print(f"mallory: /peers -> {status} {body.decode()}")

    print("== stats")
    status, body = get("/stats")
    print(f"stats -> {status} {body.decode()}")
    server.shutdown()
    print("done")


main()
