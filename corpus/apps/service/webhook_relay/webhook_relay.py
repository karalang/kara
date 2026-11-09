"""Mirror of webhook_relay.kara: a webhook ingress that verifies
GitHub-style HMAC signatures and spools accepted deliveries to disk, a
forwarder that relays the spool to a downstream service with retries and
a dead-letter directory, and a flaky downstream, all over loopback."""

import hashlib
import hmac
import json
import os
import shutil
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

INGRESS = ("127.0.0.1", 47749)
DOWNSTREAM = ("127.0.0.1", 47750)
IN_URL = "http://127.0.0.1:47749"
DOWN_URL = "http://127.0.0.1:47750"
RELAY = "relay"
DOWN = "down"
MAX_ATTEMPTS = 3

# Obviously fake secrets, one per source.
SECRETS = [("github", "fake-secret-gh-0001"), ("billing", "fake-secret-bill-0002")]


def compact(v):
    return json.dumps(v, separators=(",", ":"), ensure_ascii=False)


def secret_of(source):
    for s, k in SECRETS:
        if s == source:
            return k
    return None


def sign(secret, body):
    return "sha256=" + hmac.new(secret.encode(), body.encode(), hashlib.sha256).hexdigest()


def read_text(path, default=""):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError:
        return default


def write_atomic(path, text):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    os.rename(tmp, path)


def append_line(path, line):
    with open(path, "a", encoding="utf-8") as f:
        f.write(line + "\n")


def next_seq():
    path = f"{RELAY}/seq"
    n = int(read_text(path, "0")) + 1
    write_atomic(path, str(n))
    return n


def ingress(method, path, headers, body):
    """The status and JSON body answering one request to the ingress."""
    if path == "/health":
        n = len([f for f in os.listdir(f"{RELAY}/spool") if f.endswith(".json")])
        return 200, compact({"spooled": n})
    if not path.startswith("/hooks/"):
        return 404, compact({"error": "not found"})
    if method != "POST":
        return 405, compact({"error": "use POST"})
    source = path[len("/hooks/"):]
    secret = secret_of(source)
    if secret is None:
        return 404, compact({"error": f"unknown source {source}"})
    delivery = headers.get("x-delivery-id")
    if delivery is None or delivery == "" or not all(c.isascii() and (c.isalnum() or c == "-") for c in delivery):
        return 400, compact({"error": "missing or invalid X-Delivery-Id"})
    sig = headers.get("x-hub-signature-256")
    if sig is None or not sig.startswith("sha256="):
        return 401, compact({"error": "missing signature"})
    if not hmac.compare_digest(sig.encode(), sign(secret, body).encode()):
        return 401, compact({"error": "bad signature"})
    try:
        doc = json.loads(body)
    except ValueError:
        return 422, compact({"error": "invalid json"})
    if not isinstance(doc, dict) or not isinstance(doc.get("event"), str):
        return 422, compact({"error": "missing event"})
    if os.path.exists(f"{RELAY}/seen/{delivery}"):
        return 200, compact({"status": "duplicate", "delivery": delivery})
    write_atomic(f"{RELAY}/seen/{delivery}", source)
    seq = next_seq()
    record = {"source": source, "delivery": delivery, "event": doc["event"], "payload": doc}
    write_atomic(f"{RELAY}/spool/{seq:04d}-{source}-{delivery}.json", compact(record))
    return 202, compact({"status": "queued", "seq": seq})


def downstream(method, path, headers, body):
    """The flaky service the relay delivers to."""
    if path == "/received" and method == "GET":
        return 200, read_text(f"{DOWN}/received.log")
    if path != "/ingest" or method != "POST":
        return 404, compact({"error": "not found"})
    doc = json.loads(body)
    delivery = doc["delivery"]
    counter = f"{DOWN}/attempts-{delivery}"
    attempts = int(read_text(counter, "0")) + 1
    write_atomic(counter, str(attempts))
    if doc["event"] == "poison":
        return 500, compact({"error": "cannot process"})
    if attempts == 1 and delivery[-1] in "13579":
        return 503, compact({"error": "warming up"})
    append_line(f"{DOWN}/received.log", f"{delivery} {doc['source']} {doc['event']} attempt={headers.get('x-relay-attempt')}")
    return 200, compact({"ok": True})


def make_handler(route):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def answer(self):
            n = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(n).decode() if n > 0 else ""
            headers = {k.lower(): v for k, v in self.headers.items()}
            status, text = route(self.command, self.path, headers, body)
            payload = text.encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        do_GET = do_POST = do_PUT = answer

        def log_message(self, *args):
            pass

    return Handler


# The client side.

def send(method, url, headers, body=""):
    req = urllib.request.Request(url, data=body.encode() if body != "" else None, method=method)
    for k, v in headers:
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=5) as resp:
            return True, f"{resp.status} {resp.read().decode()}"
    except urllib.error.HTTPError as e:
        return False, f"{url}: status code {e.code}"


def show(label, r):
    ok, text = r
    print(f"{label}: {text}" if ok else f"{label}: error {text}")


def deliver(label, source, delivery, body, secret):
    headers = []
    if delivery != "":
        headers.append(("X-Delivery-Id", delivery))
    if secret != "":
        headers.append(("X-Hub-Signature-256", sign(secret, body)))
    headers.append(("Content-Type", "application/json"))
    show(label, send("POST", f"{IN_URL}/hooks/{source}", headers, body))


def forward():
    """Relays the spool in order; returns (delivered, dead)."""
    delivered = dead = 0
    for name in sorted(os.listdir(f"{RELAY}/spool")):
        if not name.endswith(".json"):
            continue
        path = f"{RELAY}/spool/{name}"
        body = read_text(path)
        done = False
        for attempt in range(1, MAX_ATTEMPTS + 1):
            ok, text = send("POST", f"{DOWN_URL}/ingest", [("X-Relay-Attempt", str(attempt)), ("Content-Type", "application/json")], body)
            if ok:
                append_line(f"{RELAY}/delivered.log", f"{name} attempts={attempt}")
                os.remove(path)
                delivered += 1
                done = True
                break
            print(f"  {name} attempt {attempt}: {text}")
            time.sleep(0.005 * (2 ** (attempt - 1)))
        if not done:
            os.rename(path, f"{RELAY}/dead/{name}")
            print(f"  {name} dead-lettered after {MAX_ATTEMPTS} attempts")
            dead += 1
    return delivered, dead


def main():
    for d in (RELAY, DOWN):
        shutil.rmtree(d, ignore_errors=True)
    for d in (f"{RELAY}/spool", f"{RELAY}/seen", f"{RELAY}/dead", DOWN):
        os.makedirs(d)
    servers = [ThreadingHTTPServer(INGRESS, make_handler(ingress)), ThreadingHTTPServer(DOWNSTREAM, make_handler(downstream))]
    for s in servers:
        threading.Thread(target=s.serve_forever, daemon=True).start()
    up = False
    for _ in range(100):
        if send("GET", f"{IN_URL}/health", [])[0] and send("GET", f"{DOWN_URL}/received", [])[0]:
            up = True
            break
        time.sleep(0.02)
    print(f"servers up: {str(up).lower()}")

    gh = secret_of("github")
    bill = secret_of("billing")
    push = compact({"event": "push", "ref": "refs/heads/main", "commits": [{"id": "a1b2c3", "message": "fix: résumé parser"}], "forced": False})
    paid = compact({"event": "invoice.paid", "invoice": "in_0042", "amount": 1999, "currency": "eur"})
    issues = compact({"event": "issues", "action": "opened", "number": 7, "labels": ["bug", "p1"]})
    refund = compact({"event": "refund.created", "amount": 250.5, "reason": None})
    poison = compact({"event": "poison", "note": "downstream always rejects this"})
    deliver("push", "github", "d-1001", push, gh)
    deliver("wrong secret", "github", "d-1002", push, bill)
    deliver("invoice", "billing", "d-1003", paid, bill)
    deliver("unknown source", "gitlab", "d-1004", push, gh)
    deliver("no signature", "github", "d-1005", push, "")
    deliver("malformed json", "github", "d-1006", "{\"event\": ", gh)
    deliver("array body", "github", "d-1007", "[1, 2]", gh)
    deliver("no event", "github", "d-1012", compact({"action": "opened"}), gh)
    deliver("redelivered push", "github", "d-1001", push, gh)
    deliver("poison", "billing", "d-1008", poison, bill)
    deliver("issue", "github", "d-1009", issues, gh)
    deliver("refund", "billing", "d-1010", refund, bill)
    deliver("no delivery id", "github", "", push, gh)
    deliver("bad delivery id", "github", "d 1011", push, gh)
    show("get on a hook", send("GET", f"{IN_URL}/hooks/github", []))
    show("health", send("GET", f"{IN_URL}/health", []))

    print("spool:")
    for name in sorted(os.listdir(f"{RELAY}/spool")):
        print(f"  {name} {read_text(f'{RELAY}/spool/{name}')}")
    print("forwarding:")
    delivered, dead = forward()
    print(f"delivered {delivered}, dead {dead}")
    print("delivered.log:")
    for line in read_text(f"{RELAY}/delivered.log").splitlines():
        print(f"  {line}")
    print("dead letters:")
    for name in sorted(os.listdir(f"{RELAY}/dead")):
        print(f"  {name}")
    print("downstream received:")
    ok, text = send("GET", f"{DOWN_URL}/received", [])
    for line in text[len("200 "):].splitlines():
        print(f"  {line}")
    show("health after", send("GET", f"{IN_URL}/health", []))
    for s in servers:
        s.shutdown()
    for d in (RELAY, DOWN):
        shutil.rmtree(d, ignore_errors=True)
    print("done")


main()
