"""Mirror of uptime_monitor.kara: an endpoint monitor in the shape of Gatus,
tested first against fake network, clock and file-system objects (the Kara
program installs them as providers), then run against real servers on
loopback (HTTP on 47751, a line-protocol cache on 47752)."""

import json
import os
import re
import shutil
import socket
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HTTP_ADDR = ("127.0.0.1", 47751)
TCP_ADDR = ("127.0.0.1", 47752)
ROOT = "uptime_data"

CONFIG = """# endpoints watched by the uptime monitor
[endpoint api-health]
url = http://127.0.0.1:47751/health
condition = [STATUS] == 200
condition = [BODY].status == UP
condition = [RESPONSE_TIME] < 1000
failure-threshold = 2

[endpoint orders]
url = http://127.0.0.1:47751/orders
condition = [STATUS] == 200
condition = len([BODY].items) >= 2
condition = [BODY].items[0].state != failed

[endpoint flaky]
url = http://127.0.0.1:47751/flaky
condition = [STATUS] == 200
failure-threshold = 3
success-threshold = 2

[endpoint slow]
url = http://127.0.0.1:47751/slow
condition = [STATUS] == 200
condition = [RESPONSE_TIME] < 800

[endpoint hang]
url = http://127.0.0.1:47751/hang
timeout = 300ms
condition = [CONNECTED] == true
failure-threshold = 2

[endpoint login]
url = http://127.0.0.1:47751/login
condition = [STATUS] == 200
condition = [BODY] == pat(*Sign in*)

[endpoint cache]
url = tcp://127.0.0.1:47752
send = PING
timeout = 300ms
condition = [CONNECTED] == true
condition = [BODY] == +PONG
failure-threshold = 2

[endpoint docs]
url = http://127.0.0.1:47751/docs
enabled = false
condition = [STATUS] == 200
"""

INT = re.compile(r"[+-]?[0-9]+")


def parse_int(s):
    return int(s) if INT.fullmatch(s) else None


def b(x):
    return "true" if x else "false"


# ---------------------------------------------------------------- config

class Endpoint:
    def __init__(self, name):
        self.name = name
        self.kind = "http"
        self.url = ""
        self.send = ""
        self.timeout_ms = 5000
        self.conditions = []
        self.failure_threshold = 3
        self.success_threshold = 2
        self.enabled = True

    def copy(self):
        e = Endpoint(self.name)
        e.__dict__.update(self.__dict__)
        e.conditions = list(self.conditions)
        return e


def parse_duration(text):
    if text.endswith("ms"):
        return parse_int(text[:-2])
    if text.endswith("s"):
        n = parse_int(text[:-1])
        return None if n is None else n * 1000
    return parse_int(text)


OPERATORS = ["==", "!=", "<=", ">=", "<", ">"]


def split_condition(text):
    for op in OPERATORS:
        sep = f" {op} "
        if sep in text:
            l, r = text.split(sep, 1)
            return l.strip(), op, r.strip()
    return None


def parse_config(text):
    eps, errors = [], []
    line_no = 0
    for raw in text.splitlines():
        line_no += 1
        line = raw.strip()
        if line == "" or line.startswith("#"):
            continue
        if line.startswith("["):
            if line.startswith("[endpoint ") and line.endswith("]"):
                eps.append(Endpoint(line[len("[endpoint "):-1].strip()))
            else:
                errors.append(f"line {line_no}: bad section {line}")
            continue
        if "=" not in line:
            errors.append(f"line {line_no}: expected key = value")
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip()
        if not eps:
            errors.append(f"line {line_no}: {key} outside an endpoint")
            continue
        ep = eps[-1]
        if key == "url":
            if value.startswith("tcp://"):
                ep.kind = "tcp"
            elif not value.startswith("http://") and not value.startswith("https://"):
                errors.append(f"line {line_no}: {ep.name}: unsupported url {value}")
            ep.url = value
        elif key == "send":
            ep.send = value
        elif key == "timeout":
            ms = parse_duration(value)
            if ms is None:
                errors.append(f"line {line_no}: {ep.name}: bad timeout {value}")
            else:
                ep.timeout_ms = ms
        elif key == "condition":
            if split_condition(value) is None:
                errors.append(f"line {line_no}: {ep.name}: condition has no operator: {value}")
            else:
                ep.conditions.append(value)
        elif key in ("failure-threshold", "success-threshold"):
            n = parse_int(value)
            if n is not None and n >= 1:
                if key == "failure-threshold":
                    ep.failure_threshold = n
                else:
                    ep.success_threshold = n
            else:
                errors.append(f"line {line_no}: {ep.name}: {key} must be a positive number")
        elif key == "enabled":
            ep.enabled = value == "true"
        else:
            errors.append(f"line {line_no}: {ep.name}: unknown key {key}")
    for ep in eps:
        if ep.url == "":
            errors.append(f"{ep.name}: no url")
        if not ep.conditions:
            errors.append(f"{ep.name}: no conditions")
    return eps, errors


# ------------------------------------------------------------ conditions

class Outcome:
    def __init__(self, connected, status, body, ms, error):
        self.connected = connected
        self.status = status
        self.body = body
        self.ms = ms
        self.error = error


def json_text(v):
    if isinstance(v, str):
        return v
    if isinstance(v, bool):
        return b(v)
    if isinstance(v, (int, float)):
        return repr(v)
    if v is None:
        return "null"
    return json.dumps(v, separators=(",", ":"))


MISSING = object()


def json_at(root, path):
    cur = root
    for seg in path.split("."):
        if "[" in seg:
            i = seg.index("[")
            key, rest = seg[:i], seg[i:]
        else:
            key, rest = seg, ""
        if key:
            if not isinstance(cur, dict) or key not in cur:
                return MISSING
            cur = cur[key]
        idx = rest
        while idx.startswith("["):
            after = idx[1:]
            if "]" not in after:
                return MISSING
            n, tail = after.split("]", 1)
            i = parse_int(n)
            if i is None or not isinstance(cur, list) or i < 0 or i >= len(cur):
                return MISSING
            cur = cur[i]
            idx = tail
    return cur


def parse_body(body):
    try:
        return json.loads(body)
    except ValueError:
        return MISSING


def resolve(term, o):
    if term.startswith("len(") and term.endswith(")"):
        inner = term[4:-1]
        if inner.startswith("[BODY]."):
            root = parse_body(o.body)
            if root is MISSING:
                return None
            v = json_at(root, inner[len("[BODY]."):])
            if v is MISSING:
                return None
            if isinstance(v, (list, dict)):
                return str(len(v))
            return str(len(json_text(v).encode()))
        r = resolve(inner, o)
        return None if r is None else str(len(r.encode()))
    if term == "[STATUS]":
        return str(o.status)
    if term == "[RESPONSE_TIME]":
        return str(o.ms)
    if term == "[CONNECTED]":
        return b(o.connected)
    if term == "[BODY]":
        return o.body
    if term.startswith("[BODY]."):
        root = parse_body(o.body)
        if root is MISSING:
            return None
        v = json_at(root, term[len("[BODY]."):])
        return None if v is MISSING else json_text(v)
    return term


def glob(pat, text):
    parts = pat.split("*")
    if len(parts) == 1:
        return pat == text
    first, last = parts[0], parts[-1]
    if not text.startswith(first) or len(text) < len(first) + len(last):
        return False
    at = len(first)
    for k in range(1, len(parts) - 1):
        i = text[at:].find(parts[k])
        if i < 0:
            return False
        at = at + i + len(parts[k])
    return len(text) - len(last) >= at and text.endswith(last)


def compare(left, op, right):
    if right.startswith("pat(") and right.endswith(")"):
        m = glob(right[4:-1], left)
        return m if op == "==" else (not m) if op == "!=" else False
    a, c = parse_int(left), parse_int(right)
    if a is not None and c is not None:
        return {"==": a == c, "!=": a != c, "<": a < c, "<=": a <= c, ">": a > c, ">=": a >= c}[op]
    if op == "==":
        return left == right
    if op == "!=":
        return left != right
    return False


def shorten(s):
    one = s.replace("\n", " ")
    return f"{one[:21]}..." if len(one) > 24 else one


def evaluate(cond, o, exact_times):
    parts = split_condition(cond)
    if parts is None:
        return False, f"{cond} (unparsable)"
    left, op, right = parts
    actual = resolve(left, o)
    if actual is None:
        return False, f"{left} (missing) {op} {right}"
    ok = compare(actual, op, right)
    shown = f"~{o.ms // 1000}s" if left == "[RESPONSE_TIME]" and not exact_times else shorten(actual)
    return ok, f"{left} ({shown}) {op} {right}"


def judge(ep, o, exact_times):
    failed = []
    for c in ep.conditions:
        ok, shown = evaluate(c, o, exact_times)
        if not ok:
            failed.append(shown)
    return (not failed), failed


# ---------------------------------------------------------------- checks

class RealNet:
    """The real client and sockets, with the real monotonic clock."""

    def now_ms(self):
        return time.monotonic() * 1000

    def http_get(self, url, timeout_ms):
        try:
            with urllib.request.urlopen(url, timeout=timeout_ms / 1000) as r:
                return r.status, r.read().decode()
        except urllib.error.HTTPError as e:
            raise CheckError(f"{url}: status code {e.code}")
        except (TimeoutError, socket.timeout):
            raise CheckError(f"{url}: request timed out after {timeout_ms} ms")
        except urllib.error.URLError as e:
            if isinstance(e.reason, (TimeoutError, socket.timeout)):
                raise CheckError(f"{url}: request timed out after {timeout_ms} ms")
            raise CheckError(f"{url}: {e.reason}")

    def tcp_exchange(self, host, port, payload, timeout_ms):
        try:
            s = socket.create_connection((host, port))
        except ConnectionRefusedError:
            return False, None, "connect: connection refused"
        try:
            s.settimeout(timeout_ms / 1000)
            if payload:
                s.sendall(f"{payload}\r\n".encode())
            got = b""
            while True:
                try:
                    chunk = s.recv(256)
                except (TimeoutError, socket.timeout):
                    return False, None, "read: timed out"
                if not chunk:
                    break
                got += chunk
                if b"\n" in got:
                    break
            return True, got.decode(errors="replace"), ""
        finally:
            s.close()


class CheckError(Exception):
    pass


class FakeNet:
    """The Kara program's FakeNet provider, with its StepClock: every
    reading of the clock is 120 ms after the last; a fake socket's
    `set_read_timeout` is accepted and never fires, and every read returns
    the whole reply."""

    def __init__(self, pages):
        self.pages = pages
        self.t = 0

    def now_ms(self):
        t = self.t
        self.t += 120
        return t

    def http_get(self, url, timeout_ms):
        for u, status, body in self.pages:
            if u == url:
                return status, body
        raise CheckError(f"{url}: connection refused")

    def tcp_exchange(self, host, port, payload, timeout_ms):
        return True, "+PONG\r\n", ""


def check_http(net, ep):
    start = net.now_ms()
    try:
        status, body = net.http_get(ep.url, ep.timeout_ms)
        ms = int(net.now_ms() - start)
        return Outcome(True, status, body, ms, "")
    except CheckError as e:
        ms = int(net.now_ms() - start)
        return Outcome(False, 0, "", ms, str(e))


def check_tcp(net, ep):
    start = net.now_ms()
    addr = ep.url[len("tcp://"):] if ep.url.startswith("tcp://") else ep.url
    host, port = addr.rsplit(":", 1)
    connected, text, error = net.tcp_exchange(host, int(port), ep.send, ep.timeout_ms)
    ms = int(net.now_ms() - start)
    if error:
        return Outcome(connected, 0, "", ms, error)
    lines = text.splitlines()
    return Outcome(True, 0, lines[0] if lines else "", ms, "")


def check(net, ep):
    return check_http(net, ep) if ep.kind == "http" else check_tcp(net, ep)


# ---------------------------------------------------------------- alerts

class Health:
    def __init__(self, failures=0, successes=0, triggered=False, checks=0, passed=0):
        self.failures = failures
        self.successes = successes
        self.triggered = triggered
        self.checks = checks
        self.passed = passed


def record(ep, h, passed, failed):
    h.checks += 1
    if passed:
        h.passed += 1
        h.successes += 1
        h.failures = 0
        if h.triggered and h.successes >= ep.success_threshold:
            h.triggered = False
            return f"RESOLVED {ep.name} after {h.successes} successes"
    else:
        h.failures += 1
        h.successes = 0
        if not h.triggered and h.failures >= ep.failure_threshold:
            h.triggered = True
            return f"TRIGGERED {ep.name} after {h.failures} failures: {failed[0]}"
    return None


# ----------------------------------------------------------------- state

def state_text(eps, hs, rnd):
    out = f"# uptime state v1\nround {rnd}\n"
    for ep, h in zip(eps, hs):
        out += f"{ep.name} {h.failures} {h.successes} {b(h.triggered)} {h.checks} {h.passed}\n"
    return out


class RealFs:
    def read_to_string(self, path):
        try:
            with open(path, encoding="utf-8") as f:
                return f.read()
        except FileNotFoundError:
            raise IoErr("not found")

    def write_file(self, path, text):
        with open(path, "w", encoding="utf-8") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())

    def rename(self, a, c):
        os.replace(a, c)

    def exists(self, path):
        return os.path.exists(path)


class IoErr(Exception):
    pass


class MemFs:
    def __init__(self, files, full):
        self.files = dict(files)
        self.full = full

    def read_to_string(self, path):
        if path not in self.files:
            raise IoErr("not found")
        return self.files[path]

    def write_file(self, path, text):
        self.files[path] = ""
        if self.full and text:
            raise IoErr("no space left on device")
        self.files[path] = text

    def rename(self, a, c):
        if a not in self.files:
            raise IoErr("not found")
        self.files[c] = self.files.pop(a)

    def exists(self, path):
        return path in self.files


def save_state(fs, path, text):
    tmp = f"{path}.tmp"
    fs.write_file(tmp, text)
    fs.rename(tmp, path)


def load_state(fs, path):
    text = fs.read_to_string(path)
    rnd, out = 0, []
    for line in text.splitlines():
        if line.startswith("#"):
            continue
        f = line.split(" ")
        if len(f) == 2 and f[0] == "round":
            rnd = parse_int(f[1]) or 0
        elif len(f) == 6:
            n = lambda s: parse_int(s) or 0
            out.append((f[0], Health(n(f[1]), n(f[2]), f[3] == "true", n(f[4]), n(f[5]))))
    return rnd, out


def restore(eps, saved):
    hs = []
    for ep in eps:
        h = Health()
        for name, s in saved:
            if name == ep.name:
                h = Health(s.failures, s.successes, s.triggered, s.checks, s.passed)
        hs.append(h)
    return hs


# ------------------------------------------------------------- self-test

def show_check(label, ep, o):
    passed, failed = judge(ep, o, True)
    if passed:
        print(f"  {label}: pass")
    else:
        print(f"  {label}: fail: {'; '.join(failed)}")
    if o.error:
        print(f"    error: {o.error}")


def self_test(eps):
    print("== self-test: conditions")
    body = '{"status":"UP","items":[{"id":17,"state":"shipped"},{"id":18}],"n":2.5,"ok":true}'
    o = Outcome(True, 200, body, 120, "")
    cases = [
        "[STATUS] == 200",
        "[STATUS] != 200",
        "[STATUS] < 300",
        "[RESPONSE_TIME] <= 120",
        "[RESPONSE_TIME] > 500",
        "[BODY].status == UP",
        "[BODY].items[0].id == 17",
        "[BODY].items[1].state == shipped",
        "[BODY].items[5].id == 1",
        "len([BODY].items) == 2",
        "len([BODY].status) == 2",
        "[BODY].n == 2.5",
        "[BODY].ok == true",
        '[BODY] == pat(*"UP"*)',
        "[BODY] == pat(*DOWN*)",
        "[CONNECTED] == true",
        "[STATUS] ~ 200",
    ]
    for c in cases:
        ok, shown = evaluate(c, o, True)
        print(f"  {b(ok)}: {shown}")

    print("== self-test: config errors")
    bad = "[endpoint a]\nurl = ftp://x\ncondition = [STATUS] 200\nretries = 3\n[endpoint b]\nurl = http://x\ntimeout = soon\nfailure-threshold = 0\ncondition = [STATUS] == 200\n[endpoint c\nnonsense\n"
    parsed, errors = parse_config(bad)
    print(f"  {len(parsed)} endpoints, {len(errors)} errors")
    for e in errors:
        print(f"  {e}")

    print("== self-test: http checks against a fake network and clock")
    health = eps[0].copy()
    net = FakeNet([
        ("http://127.0.0.1:47751/health", 200, '{"status":"UP"}'),
        ("http://127.0.0.1:47751/flaky", 503, "upstream unavailable"),
    ])
    show_check("health", health, check(net, health))
    strict = health.copy()
    strict.conditions.append("[RESPONSE_TIME] < 100")
    show_check("health under 100ms", strict, check(net, strict))
    show_check("flaky", eps[2], check(net, eps[2]))
    show_check("login (no page)", eps[5], check(net, eps[5]))
    show_check("cache", eps[6], check(net, eps[6]))

    print("== self-test: state files against a fake file system")
    mem = MemFs({"/etc/uptime/config.ini": CONFIG}, False)
    try:
        mem.read_to_string("/etc/uptime/missing.ini")
        print("  missing config: read?")
    except IoErr as e:
        print(f"  missing config: {e}")
    eps2, errs2 = parse_config(mem.read_to_string("/etc/uptime/config.ini"))
    print(f"  config from fake fs: {len(eps2)} endpoints, {len(errs2)} errors")
    hs = [Health() for _ in eps2]
    hs[2] = Health(3, 0, True, 5, 2)
    try:
        save_state(mem, "/var/uptime/state", state_text(eps2, hs, 5))
        print("  saved state")
    except IoErr as e:
        print(f"  save failed: {e}")
    try:
        rnd, saved = load_state(mem, "/var/uptime/state")
        back = restore(eps2, saved)
        print(f"  loaded round {rnd}: flaky failures {back[2].failures} triggered {b(back[2].triggered)}, {back[2].passed}/{back[2].checks} passed")
    except IoErr as e:
        print(f"  load failed: {e}")
    print(f"  temp file left: {b(mem.exists('/var/uptime/state.tmp'))}")

    full = MemFs({"/var/uptime/state": "# uptime state v1\nround 4\n"}, True)
    try:
        save_state(full, "/var/uptime/state", "# uptime state v1\nround 5\n")
        print("  full disk: saved?")
    except IoErr as e:
        print(f"  full disk: {e}")
    try:
        rnd, _ = load_state(full, "/var/uptime/state")
        print(f"  full disk: state still at round {rnd}")
    except IoErr as e:
        print(f"  full disk: load failed: {e}")


# --------------------------------------------------------------- servers

hits = {}
hits_lock = threading.Lock()


def next_hit(key):
    with hits_lock:
        hits[key] = hits.get(key, 0) + 1
        return hits[key]


def page_for(path):
    n = next_hit(path)
    if path == "/health":
        return 200, '{"status":"UP","version":"2.4.1"}'
    if path == "/orders":
        state = "failed" if n == 6 else "shipped"
        return 200, f'{{"items":[{{"id":17,"state":"{state}"}},{{"id":18,"state":"pending"}}]}}'
    if path == "/flaky":
        return (503, "upstream unavailable") if 3 <= n <= 5 else (200, "ok")
    if path == "/slow":
        if n in (3, 4):
            time.sleep(1.2)
        return 200, "ok"
    if path == "/hang":
        if n in (5, 6):
            time.sleep(1.5)
        return 200, "ok"
    if path == "/login":
        return (500, "internal error") if n == 7 else (200, "<html><h1>Sign in</h1></html>")
    return 404, "not found"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        status, body = page_for(self.path.split("?")[0])
        data = body.encode()
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *args):
        pass


def cache_conn(conn, n):
    with conn:
        try:
            conn.recv(64)
            if n in (4, 5):
                time.sleep(1.5)
                return
            conn.sendall(b"-ERR loading\r\n" if n >= 8 else b"+PONG\r\n")
        except OSError:
            pass


def serve_cache(listener):
    while True:
        try:
            conn, _ = listener.accept()
        except OSError:
            return
        n = next_hit("tcp")
        threading.Thread(target=cache_conn, args=(conn, n), daemon=True).start()


# ------------------------------------------------------------------- run

def run_round(net, eps, hs, rnd, log):
    print(f"== round {rnd}")
    for i, ep in enumerate(eps):
        if not ep.enabled:
            continue
        o = check(net, ep)
        passed, failed = judge(ep, o, False)
        if passed:
            print(f"  {ep.name:<10} ok")
        else:
            print(f"  {ep.name:<10} FAIL {'; '.join(failed)}")
            if o.error:
                print(f"  {'':<10} error: {o.error}")
        event = record(ep, hs[i], passed, failed)
        if event is not None:
            print(f"  ** {event}")
            log.write(f"round {rnd} {event}\n")
    log.flush()


def main():
    eps, errors = parse_config(CONFIG)
    print(f"config: {len(eps)} endpoints, {len(errors)} errors")
    for ep in eps:
        state = "" if ep.enabled else " (disabled)"
        print(f"  {ep.name} {ep.kind} {len(ep.conditions)} conditions, alert after {ep.failure_threshold} failures, resolve after {ep.success_threshold}{state}")

    self_test(eps)

    shutil.rmtree(ROOT, ignore_errors=True)
    os.makedirs(ROOT, exist_ok=True)
    state_path = f"{ROOT}/state"
    log_path = f"{ROOT}/incidents.log"
    fs = RealFs()

    httpd = ThreadingHTTPServer(HTTP_ADDR, Handler)
    httpd.daemon_threads = True
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(TCP_ADDR)
    listener.listen(16)
    threading.Thread(target=serve_cache, args=(listener,), daemon=True).start()
    print("servers up: true")

    net = RealNet()
    hs = [Health() for _ in eps]
    with open(log_path, "a", encoding="utf-8") as log:
        for rnd in range(1, 9):
            run_round(net, eps, hs, rnd, log)
            save_state(fs, state_path, state_text(eps, hs, rnd))

    print("== restart")
    eps, _ = parse_config(CONFIG)
    rnd, saved = load_state(fs, state_path)
    hs = restore(eps, saved)
    print(f"  resumed after round {rnd}")
    for ep, h in zip(eps, hs):
        if h.failures > 0 or h.triggered:
            print(f"  {ep.name}: {h.failures} failures in a row, triggered {b(h.triggered)}")
    with open(log_path, "a", encoding="utf-8") as log:
        run_round(net, eps, hs, rnd + 1, log)
        save_state(fs, state_path, state_text(eps, hs, rnd + 1))

    print("== incidents")
    with open(log_path, encoding="utf-8") as f:
        for line in f.read().splitlines():
            print(f"  {line}")

    print("== uptime")
    for ep, h in zip(eps, hs):
        if h.checks == 0:
            print(f"  {ep.name:<10} no checks")
            continue
        pct = 100.0 * h.passed / h.checks
        print(f"  {ep.name:<10} {h.passed}/{h.checks} {pct:.1f}%")
    text = fs.read_to_string(state_path)
    print(f"state file: {len(text.splitlines())} lines, temp left {b(fs.exists(state_path + '.tmp'))}")

    httpd.shutdown()
    listener.close()
    shutil.rmtree(ROOT, ignore_errors=True)
    print("done")


if __name__ == "__main__":
    main()
