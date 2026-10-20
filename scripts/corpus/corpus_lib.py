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
    "legacy-interp": {"run": ["run", "--interp"]},
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
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="corpus-") as tmp:
        tmpd = Path(tmp)
        shutil.copy(entry / source, tmpd / "source.kara")
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
                if r.returncode != 0 and REFUSAL.search(r.stderr) and b"panicked" not in r.stderr \
                        and b"runtime error" not in r.stderr:
                    return {"status": "refused", "exit": r.returncode, "stdout": r.stdout,
                            "stderr": r.stderr, "secs": time.monotonic() - start}
        except subprocess.TimeoutExpired as t:
            return {"status": "timeout", "exit": None, "stdout": t.stdout or b"", "stderr": t.stderr or b"",
                    "secs": time.monotonic() - start}
    return {"status": "ran", "exit": r.returncode, "stdout": r.stdout, "stderr": r.stderr,
            "secs": time.monotonic() - start}


def execute(cmd: list, cwd: Path, env: dict, timeout: float, driver):
    """Run the program, or, for an entry with a `driver.toml`, serve it."""
    if driver is None:
        return subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, timeout=timeout,
                              stdin=subprocess.DEVNULL)
    return drive(cmd, cwd, env, timeout, driver)


BOUND_PORT = re.compile(rb"BOUND_PORT=(\d+)")


def drive(cmd: list, cwd: Path, env: dict, timeout: float, driver: dict):
    """A server entry. `driver.toml` lists `[[connection]]` tables, each with
    `send`, a string or a list of strings. The program binds port 0 and
    prints `BOUND_PORT=<n>` on stdout or stderr. Each connection then sends
    its strings in order, and after each one it reads until the server
    closes the connection or, once a reply has begun, sends nothing for
    `idle` seconds (default 1.0). The program is stopped after the last connection. Its stdout is
    then the transcript, `=== connection <k> ===` and the bytes that came
    back on each connection with CRLF written as LF, so that expected.out
    states what a client sees. With `wait = true` the program is left to end by itself and its
    exit code is the entry's; with `stdout = true` the transcript ends with
    `=== stdout ===` and what the program printed (its `BOUND_PORT` line
    dropped). A program that exits before it reports a port is judged on what
    it printed and its exit code, like any other entry."""
    import signal
    import socket
    import threading

    idle = float(driver.get("idle", 1.0))
    deadline = time.monotonic() + timeout
    # Its own process group: a backend that runs the program as a child of
    # karac (the ASan lane) is stopped with it, so no server outlives its
    # entry and holds a port while the next entry runs.
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
            transcript += b"=== stdout ===\n" + re.sub(rb"(?m)^BOUND_PORT=\d+\n", b"", bytes(out))
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
