"""Shared pieces of the corpus scripts: paths, meta.toml I/O, backends, and
running one program on one backend."""

import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Files that record one run of the program; a changed source invalidates them.
RECORDED = ("legacy.out", "expected.out")  # expected.out only while unclassified

# Meta keys in the order they are written, so diffs stay small.
META_ORDER = ["source", "dedup_of", "expect", "exit", "tags", "class",
              "expected_from", "legacy_backend", "legacy_exit", "rules", "backends", "leaks", "env", "note"]

# A backend is either a single command that compiles and runs (`run`) or a
# compile step that leaves ./source behind and is then executed (`build`).
# `check` runs `karac check` first and counts its refusal as the backend's.
# `mir-interp` is the hidden `karac __mir-run` (single-file programs: MIR
# builder, drop elaboration, MIR interpreter); `mir-llvm` is the hidden
# `karac __mir-llvm-run` (the same MIR compiled to LLVM IR and run under
# `karac_jit_runner`).
BACKENDS = {
    "legacy-interp": {"run": ["run", "--legacy", "--interp"]},
    "legacy-build": {"build": ["build"], "env": {"KARAC_AUTO_PAR": "0"}},
    "mir-interp": {"check": True, "run": ["__mir-run"]},
    "mir-llvm": {"check": True, "run": ["__mir-llvm-run"]},
    "mir-llvm-asan": {"check": True, "run": ["__mir-llvm-asan"]},
    "mir-build": {"check": True, "build": ["build", "--mir"]},
}


def toml_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{k} = {toml_value(x)}" for k, x in v.items()) + " }" if v else "{}"
    return json.dumps(str(v), ensure_ascii=False)


def write_toml(path: Path, meta: dict) -> None:
    keys = [k for k in META_ORDER if k in meta] + sorted(k for k in meta if k not in META_ORDER)
    path.write_text("".join(f"{k} = {toml_value(meta[k])}\n" for k in keys))


def read_toml(path: Path) -> dict:
    if not path.exists():
        return {}
    with path.open("rb") as f:
        return tomllib.load(f)


def entries(corpus: Path) -> list[Path]:
    return sorted(p.parent for p in corpus.rglob("source.kara"))


def matches_filter(rel: str, filters: list[str]) -> bool:
    """`--filter` as the scripts read it: a substring of the entry's path."""
    return any(f in rel for f in filters)


# A front-end or backend refusal prints `error[<phase>]: ...` / `error: ...`
# before the program runs; a runtime panic prints differently and exits 101.
REFUSAL = re.compile(rb"^error(\[[A-Za-z_-]+\])?:", re.M)


def run_program(entry: Path, backend: str, karac: str, timeout: float, env_extra: dict,
                source: str = "source.kara") -> dict:
    """Run one entry. Returns {status, exit, stdout, stderr, secs} where status
    is ran | refused | timeout."""
    spec = BACKENDS[backend]
    env = dict(os.environ)
    env.update(spec.get("env", {}))
    env.update({k: str(v) for k, v in env_extra.items()})
    driver = read_toml(entry / "driver.toml") or None
    template = read_toml(entry / "template.toml") or None
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="corpus-") as tmp:
        tmpd = Path(tmp)
        origin = None
        if template is None:
            shutil.copy(entry / source, tmpd / "source.kara")
        else:
            origin = Origin(template["origin"]) if "origin" in template else None
            (tmpd / "source.kara").write_text(
                render_template((entry / source).read_text(), template, tmpd, origin))
        try:
            if spec.get("check"):
                c = subprocess.run([karac, "check", "source.kara"], cwd=tmpd, env=env,
                                   capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL)
                if c.returncode != 0:
                    return {"status": "refused", "exit": c.returncode, "stdout": c.stdout,
                            "stderr": c.stderr, "secs": time.monotonic() - start}
            if "build" in spec:
                b = subprocess.run([karac, *spec["build"], "source.kara"], cwd=tmpd, env=env,
                                   capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL)
                if b.returncode != 0 or not (tmpd / "source").exists():
                    return {"status": "refused", "exit": b.returncode, "stdout": b.stdout,
                            "stderr": b.stderr, "secs": time.monotonic() - start}
                r = execute(["./source"], tmpd, env, timeout, driver)
            else:
                r = execute([karac, *spec["run"], "source.kara"], tmpd, env, timeout, driver)
                # A deadlock is a panic (core semantics §10.1) whose record
                # reads `error: deadlock: ...`, not a refusal.
                if r.returncode != 0 and REFUSAL.search(r.stderr) and b"panicked" not in r.stderr \
                        and b"runtime error" not in r.stderr and b"error: deadlock:" not in r.stderr:
                    return {"status": "refused", "exit": r.returncode, "stdout": r.stdout,
                            "stderr": r.stderr, "secs": time.monotonic() - start}
        except subprocess.TimeoutExpired as t:
            return {"status": "timeout", "exit": None, "stdout": t.stdout or b"", "stderr": t.stderr or b"",
                    "secs": time.monotonic() - start}
        finally:
            if origin is not None:
                origin.stop()
        stdout = template_stdout(r.stdout, template, tmpd, origin)
    return {"status": "ran", "exit": r.returncode, "stdout": stdout, "stderr": r.stderr,
            "secs": time.monotonic() - start}


def template_stdout(stdout: bytes, template, tmpd: Path, origin) -> bytes:
    """A template entry's stdout as its expected.out holds it: the program's
    own, then what the origin recorded, then the files `show_files` names."""
    if origin is not None and origin.record:
        stdout += b"=== origin ===\n" + origin.transcript()
    if origin is not None and (template or {}).get("mask_origin_port"):
        # A program that prints its origin's URL (an error naming it): the
        # port is the run's own, so expected.out holds `ORIGIN_PORT`.
        stdout = stdout.replace(f"127.0.0.1:{origin.port}".encode(), b"127.0.0.1:ORIGIN_PORT")
    # Files whose contents its test checks after the run.
    for name in (template or {}).get("show_files", []):
        f = tmpd / name
        stdout += f"=== file {name} ===\n".encode() + (f.read_bytes() if f.exists() else b"(missing)\n")
    return stdout


# A `{{`, a `}}` or a `{name}` hole of a Rust `format!` template.
HOLE = re.compile(r"\{\{|\}\}|\{([A-Za-z_][A-Za-z0-9_]*)\}")


def render_template(src: str, template: dict, tmpd: Path, origin) -> str:
    """A `format!` template entry, filled in. `template.toml`'s `[holes]` gives
    each hole's value as its test fills it in; a value may name `{tmp}`, the
    run's own directory (for a file the program writes or reads), and
    `{origin_port}`, the port of the HTTP origin `[origin]` describes.
    A template filled in by `str::replace` instead gives `[replace]`, each
    word and its value, and has no holes.
    `[files]` are written into that directory first, and the contents of the
    files `show_files` names end the entry's stdout, each after a
    `=== file <name> ===` line, for a test that checks a file after the run.
    With `mask_origin_port = true` the origin's port in the entry's stdout
    is written `ORIGIN_PORT`."""
    fills = {"tmp": str(tmpd), "origin_port": str(origin.port) if origin else ""}
    holes = {k: str(v).format(**fills) for k, v in template.get("holes", {}).items()}
    for name, text in template.get("files", {}).items():
        (tmpd / name).write_text(text)
    if "replace" in template:
        # A raw-string template its test fills in with `str::replace`
        # (`DIR`, `PORT`): its braces are Kara's own, so no holes.
        for word, value in template["replace"].items():
            src = src.replace(word, str(value).format(**fills))
        return src

    def fill(m):
        if m.group(1) is None:
            return m.group(0)[0]
        if m.group(1) not in holes:
            raise KeyError(f"template.toml has no value for the hole {{{m.group(1)}}}")
        return holes[m.group(1)]
    return HOLE.sub(fill, src)


class Origin:
    """The HTTP server a client entry talks to, standing in for the one its
    Rust test starts: on 127.0.0.1, port 0, answering every request with
    `reply`. With `record = true` the entry's stdout ends with
    `=== origin ===` and each request it got: the request line, the headers
    named in `record_headers` (in that order, names in lower case) and the
    body, with CRLF written as LF.

    An origin that answers by route lists `[[origin.route]]` tables instead,
    the first that matches answering: `method` and `path` (either left out
    matches any), `status` (`"200 OK"`), `headers` (lines after the
    `Content-Length` and `Connection: close` ones) and `body`, or `body_hex` for bytes that are not
    text. `body` and `headers` may name `{method}`, `{path}`, `{body}` and a
    request header as `{h[x-echo]}`. `reply` then answers what no route
    matches."""

    def __init__(self, spec: dict):
        import socket
        import threading
        self.reply = spec.get("reply", "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").encode("latin-1")
        self.routes = spec.get("route", [])
        self.record = bool(spec.get("record"))
        self.headers = [h.lower() for h in spec.get("record_headers", [])]
        self.seen = []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(16)
        self.port = self.sock.getsockname()[1]
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            with conn:
                conn.settimeout(10)
                data = b""
                try:
                    while b"\r\n\r\n" not in data:
                        got = conn.recv(65536)
                        if not got:
                            break
                        data += got
                    head, _, body = data.partition(b"\r\n\r\n")
                    lines = head.split(b"\r\n")
                    fields = {}
                    for line in lines[1:]:
                        k, _, v = line.partition(b":")
                        fields[k.strip().lower().decode("latin-1")] = v.strip()
                    want = int(fields.get("content-length", b"0") or 0)
                    while len(body) < want:
                        got = conn.recv(65536)
                        if not got:
                            break
                        body += got
                    self.seen.append((lines[0], fields, body))
                    conn.sendall(self.answer(lines[0], fields, body))
                except OSError:
                    pass

    def answer(self, line: bytes, fields: dict, body: bytes) -> bytes:
        words = line.decode("latin-1").split()
        method, path = (words + ["", ""])[:2]
        for r in self.routes:
            if r.get("method", method) != method or r.get("path", path) != path:
                continue
            vals = {"method": method, "path": path, "body": body.decode("latin-1"),
                    "h": {k: v.decode("latin-1") for k, v in fields.items()}}
            if "body_hex" in r:
                text = bytes.fromhex(r["body_hex"])
            else:
                text = r.get("body", "").format(**vals).encode("latin-1")
            # The origin closes each connection after one answer, so it says
            # so: a client that pooled the connection would otherwise send a
            # POST down a socket already closed and read a reset.
            head = (f"HTTP/1.1 {r.get('status', '200 OK')}\r\nContent-Length: {len(text)}\r\n"
                    "Connection: close\r\n")
            for h in r.get("headers", []):
                head += h.format(**vals) + "\r\n"
            return (head + "\r\n").encode("latin-1") + text
        return self.reply

    def stop(self):
        import socket
        try:
            self.sock.shutdown(socket.SHUT_RDWR)  # wakes the blocked accept (a close alone does not)
        except OSError:
            pass
        self.sock.close()
        self.thread.join(1)

    def transcript(self) -> bytes:
        out = b""
        for line, fields, body in self.seen:
            out += line + b"\n"
            for h in self.headers:
                if h in fields:
                    out += h.encode() + b": " + fields[h] + b"\n"
            out += b"\n" + body + b"\n"
        return out


def execute(cmd: list, cwd: Path, env: dict, timeout: float, driver):
    """Run the program, or, for an entry with a `driver.toml`, serve it."""
    if driver is None:
        return run_capped(cmd, cwd, env, timeout)
    return drive(cmd, cwd, env, timeout, driver)


# A program that prints without end (a test that stops it with a closed pipe
# or a signal) would fill memory before its timeout: past this many bytes of
# output it is stopped and counts as timed out.
OUTPUT_CAP = 64 << 20


def run_capped(cmd: list, cwd: Path, env: dict, timeout: float) -> subprocess.CompletedProcess:
    """subprocess.run with captured output, stdin closed and `timeout`, which
    raises TimeoutExpired also when stdout or stderr passes OUTPUT_CAP."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=out, stderr=err, stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + timeout
        pause = 0.001
        while p.poll() is None:
            if time.monotonic() > deadline or os.fstat(out.fileno()).st_size > OUTPUT_CAP \
                    or os.fstat(err.fileno()).st_size > OUTPUT_CAP:
                p.kill()
                p.wait()
                raise subprocess.TimeoutExpired(cmd, timeout)
            time.sleep(pause)
            pause = min(pause * 2, 0.02)
        out.seek(0)
        err.seek(0)
        return subprocess.CompletedProcess(cmd, p.returncode, out.read(), err.read())


BOUND_PORT = re.compile(rb"BOUND_PORT=(\d+)")


def drive(cmd: list, cwd: Path, env: dict, timeout: float, driver: dict):
    """A server entry. `driver.toml` lists `[[connection]]` tables, each with
    `send`, a string or a list of strings. The program binds port 0 and
    prints `BOUND_PORT=<n>` on stdout or stderr; it runs with
    `KARAC_ANNOUNCE_PORT=1`, which makes a v2 `TcpListener.bind` print it. Each connection then sends
    its strings in order, and after each one it reads until the server
    closes the connection or, once a reply has begun, sends nothing for
    `idle` seconds (default 1.0). The program is stopped after the last connection. Its stdout is
    then the transcript, `=== connection <k> ===` and the bytes that came
    back on each connection with CRLF written as LF, so that expected.out
    states what a client sees. With `wait = true` the program is left to end by itself and its
    exit code is the entry's; with `stdout = true` the transcript ends with
    `=== stdout ===` and what the program printed (its `BOUND_PORT` line
    dropped), and `unordered = true` sorts those lines, for a program whose
    tasks print concurrently. A program that exits before it reports a port is judged on what
    it printed and its exit code, like any other entry."""
    import signal
    import socket
    import threading

    idle = float(driver.get("idle", 1.0))
    deadline = time.monotonic() + timeout
    # Its own process group: a backend that runs the program as a child of
    # karac (the ASan lane) is stopped with it, so no server outlives its
    # entry and holds a port while the next entry runs.
    # A listener bound to port 0 reports its port on stderr only when asked
    # (B-2026-10-15-69); a driver is the one asking.
    env = {**env, "KARAC_ANNOUNCE_PORT": "1"}
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, start_new_session=True)
    out, err = bytearray(), bytearray()
    seen = threading.Event()
    closed = []

    def pump(stream, buf):
        for chunk in iter(lambda: stream.read1(65536), b""):
            buf.extend(chunk)
            if BOUND_PORT.search(buf):
                seen.set()
        closed.append(stream)
        if len(closed) == 2:
            seen.set()  # both streams closed: the program will not report a port

    pumps = [threading.Thread(target=pump, args=a, daemon=True) for a in ((p.stdout, out), (p.stderr, err))]
    for t in pumps:
        t.start()
    try:
        while not seen.wait(0.1):
            if time.monotonic() > deadline:
                raise subprocess.TimeoutExpired(cmd, timeout, bytes(out), bytes(err))
        m = BOUND_PORT.search(out) or BOUND_PORT.search(err)
        if m is None:
            # Exited (or closed both streams) without serving.
            try:
                code = p.wait(max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise subprocess.TimeoutExpired(cmd, timeout, bytes(out), bytes(err)) from None
            for t in pumps:
                t.join(1)
            return subprocess.CompletedProcess(cmd, code, bytes(out), bytes(err))
        port = int(m.group(1))
        transcript = bytearray()
        for k, conn in enumerate(driver.get("connection", []), 1):
            sends = conn.get("send", [])
            if isinstance(sends, str):
                sends = [sends]
            transcript += f"=== connection {k} ===\n".encode()
            with socket.create_connection(("127.0.0.1", port), timeout=max(0.1, deadline - time.monotonic())) as s:
                for chunk in sends:
                    s.sendall(chunk.encode("latin-1"))
                    first = True
                    while True:
                        left = deadline - time.monotonic()
                        if left <= 0:
                            raise subprocess.TimeoutExpired(cmd, timeout, bytes(transcript), bytes(err))
                        # The first bytes of a reply may take as long as the
                        # entry's timeout (a slow backend); after that, a
                        # pause of `idle` ends the reply.
                        s.settimeout(left if first else min(idle, left))
                        try:
                            got = s.recv(65536)
                        except socket.timeout:
                            if first:
                                raise subprocess.TimeoutExpired(cmd, timeout, bytes(transcript),
                                                                bytes(err)) from None
                            break
                        except OSError:
                            got = b""
                        if not got:
                            break
                        first = False
                        transcript += got
            if not transcript.endswith(b"\n"):
                transcript += b"\n"
        code = 0
        if driver.get("wait"):
            # A program that ends once it has answered: its exit code is its own.
            try:
                code = p.wait(max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise subprocess.TimeoutExpired(cmd, timeout, bytes(transcript), bytes(err)) from None
            for t in pumps:
                t.join(1)
        # Line ends as `\n`: a checkout normalizes expected.out to LF
        # (.gitattributes), so an HTTP reply's CRLF could never match.
        transcript = bytearray(bytes(transcript).replace(b"\r\n", b"\n"))
        if driver.get("stdout"):
            printed = re.sub(rb"(?m)^BOUND_PORT=\d+\n", b"", bytes(out))
            if driver.get("unordered"):
                # Lines from concurrent tasks, whose order is not the program's.
                printed = b"".join(sorted(printed.splitlines(keepends=True)))
            transcript += b"=== stdout ===\n" + printed
        return subprocess.CompletedProcess(cmd, code, bytes(transcript), bytes(err))
    finally:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        p.wait()
        for t in pumps:
            t.join(1)


def error_codes(text: bytes) -> list[str]:
    """Diagnostic codes named in a refusal's output (`error[E0500]`-style or a JSON `code`)."""
    found = re.findall(rb"\b([EWN]\d{4})\b", text)
    return list(dict.fromkeys(x.decode() for x in found))
