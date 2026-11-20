"""Shared runner for the library conformance probes in this directory.

Each probe script builds one Kāra program from generated cases, runs it on
the MIR interpreter (`karac __mir-run`) and, when the karac binary has the
LLVM backend, compiled (`karac build`), and compares every output line with
a reference oracle computed in Python (the standard library's `base64`,
`hashlib`, `hmac`, `urllib.parse`, or published test vectors). A run that
executes nothing is a failure, never a pass.
"""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
KARAC = os.environ.get("KARAC", str(ROOT / "target/release/karac"))


def kara_str(s: str) -> str:
    """`s` as a Kāra string literal."""
    out = []
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\t":
            out.append("\\t")
        elif ch == "\r":
            out.append("\\r")
        elif ord(ch) < 0x20 or ord(ch) == 0x7F:
            out.append("\\u{%x}" % ord(ch))
        else:
            out.append(ch)
    return '"' + "".join(out) + '"'


def run(src: str, backends=("interp", "build"), stdin: str = "", timeout: int = 600):
    """The program's stdout on each backend, as {backend: (code, stdout, stderr)}."""
    out = {}
    with tempfile.TemporaryDirectory(prefix="kara-conf-") as d:
        path = Path(d) / "main.kara"
        path.write_text(src)
        check = subprocess.run([KARAC, "check", str(path)], capture_output=True, text=True, timeout=timeout)
        out["check"] = (check.returncode, check.stdout, check.stderr)
        if check.returncode != 0:
            return out
        if "interp" in backends:
            r = subprocess.run([KARAC, "__mir-run", str(path)], capture_output=True, text=True,
                               input=stdin, timeout=timeout)
            out["interp"] = (r.returncode, r.stdout, r.stderr)
        if "build" in backends and has_backend():
            exe = Path(d) / "main.bin"
            b = subprocess.run([KARAC, "build", "-o", str(exe), str(path)], capture_output=True,
                               text=True, timeout=timeout)
            if b.returncode != 0:
                out["build"] = (b.returncode, "", b.stdout + b.stderr)
            else:
                r = subprocess.run([str(exe)], capture_output=True, text=True, input=stdin, timeout=timeout)
                out["build"] = (r.returncode, r.stdout, r.stderr)
    return out


_backend = None


def has_backend() -> bool:
    global _backend
    if _backend is None:
        if os.environ.get("CONF_BUILD") == "0":
            _backend = False
        else:
            with tempfile.TemporaryDirectory(prefix="kara-conf-") as d:
                path = Path(d) / "main.kara"
                path.write_text("fn main() { println(1); }\n")
                b = subprocess.run([KARAC, "build", "-o", str(Path(d) / "m"), str(path)],
                                   capture_output=True, text=True)
                _backend = b.returncode == 0
    return _backend


class Report:
    """Collects case verdicts; `finish` prints the totals and exits 1 on any
    failure, and 2 when nothing was compared."""

    def __init__(self, name: str):
        self.name = name
        self.passed = 0
        self.failed = []

    def compare(self, backend: str, labels, got_lines, want_lines):
        if len(got_lines) != len(want_lines):
            self.failed.append(f"[{backend}] {len(got_lines)} lines, want {len(want_lines)}")
        for label, got, want in zip(labels, got_lines, want_lines):
            ok = want(got) if callable(want) else got == want
            if ok:
                self.passed += 1
            else:
                self.failed.append(f"[{backend}] {label}: got {got!r}, want {getattr(want, 'text', want)!r}")

    def check_runs(self, results, labels, want_lines):
        if results["check"][0] != 0:
            self.failed.append("check refused the program:\n" + results["check"][1] + results["check"][2])
            return
        for backend in ("interp", "build"):
            if backend not in results:
                continue
            code, stdout, stderr = results[backend]
            if code != 0:
                self.failed.append(f"[{backend}] exit {code}: {stderr[-2000:]}")
            self.compare(backend, labels, stdout.splitlines(), want_lines)

    def finish(self):
        for f in self.failed[:60]:
            print("FAIL", f)
        if len(self.failed) > 60:
            print(f"... {len(self.failed) - 60} more")
        print(f"{self.name}: {self.passed} passed, {len(self.failed)} failed")
        if self.failed:
            sys.exit(1)
        if self.passed == 0:
            sys.exit(2)
