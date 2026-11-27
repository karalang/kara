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


def run(src: str, backends=("interp", "build"), stdin: str = "", timeout: int = 600, repeat: int = 1,
        env=None, args=None):
    """The program's results on each backend: {"check": (code, stdout,
    stderr), backend: [(code, stdout, stderr), ...]}, one entry per run when
    `repeat` asks for several (a schedule-sensitive case). The compiled
    program is built once and run `repeat` times. `args` are the program's
    command-line arguments (str, or bytes for a non-UTF-8 one); with them
    the interpreter runs through `karac run --interp`, since the corpus
    runner's `__mir-run` passes none."""
    out = {}
    run_env = dict(os.environ, **{k: os.fsdecode(v) for k, v in (env or {}).items()})
    argv = [os.fsdecode(a) for a in (args or [])]
    with tempfile.TemporaryDirectory(prefix="kara-conf-") as d:
        path = Path(d) / "main.kara"
        path.write_text(src)
        check = subprocess.run([KARAC, "check", str(path)], capture_output=True, text=True, timeout=timeout)
        out["check"] = (check.returncode, check.stdout, check.stderr)
        if check.returncode != 0:
            return out

        def once(cmd):
            # Each run in its own empty directory, so relative paths a program
            # writes start from nothing and stay out of the caller's tree.
            work = tempfile.mkdtemp(prefix="run-", dir=d)
            try:
                r = subprocess.run(cmd, capture_output=True, text=True, input=stdin, timeout=timeout,
                                   env=run_env, cwd=work, errors="surrogateescape")
                return (r.returncode, r.stdout, r.stderr)
            except subprocess.TimeoutExpired:
                return (-999, "", f"timed out after {timeout}s")

        if "interp" in backends:
            cmd = [KARAC, "run", "--interp", str(path), "--", *argv] if args is not None else [KARAC, "__mir-run", str(path)]
            out["interp"] = [once(cmd) for _ in range(repeat)]
        # `karac run`: the JIT, through karac_jit_runner (CONF_LANES=jit).
        if "jit" in backends and has_backend():
            out["jit"] = [once([KARAC, "run", str(path), "--", *argv]) for _ in range(repeat)]
        # The sanitizer lanes (CONF_LANES=asan,tsan in cases.py): the same IR
        # built with AddressSanitizer or ThreadSanitizer, as the corpus runner's
        # mir-llvm-asan and mir-llvm-tsan backends build it. A report exits 86.
        # TSan runs on four workers and at least three times, since a race is
        # schedule-sensitive. They pass no command-line arguments.
        if "asan" in backends and has_backend() and args is None:
            out["asan"] = [once([KARAC, "__mir-llvm-asan", str(path)]) for _ in range(repeat)]
        if "tsan" in backends and has_backend() and args is None:
            tsan_env = dict(run_env, KARAC_THREADS="4")

            def tsan_once():
                try:
                    r = subprocess.run([KARAC, "__mir-llvm-tsan", str(path)], capture_output=True, text=True,
                                       input=stdin, timeout=timeout, env=tsan_env)
                    return (r.returncode, r.stdout, r.stderr)
                except subprocess.TimeoutExpired:
                    return (-999, "", f"timed out after {timeout}s")

            out["tsan"] = [tsan_once() for _ in range(max(repeat, 3))]
        if "build" in backends and has_backend():
            exe = Path(d) / "main.bin"
            b = subprocess.run([KARAC, "build", "-o", str(exe), str(path)], capture_output=True,
                               text=True, timeout=timeout)
            if b.returncode != 0 or not exe.exists():
                out["build"] = [(b.returncode or 1, "", b.stdout + b.stderr)]
            else:
                out["build"] = [once([str(exe), *argv]) for _ in range(repeat)]
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
                # A karac without the llvm feature type-checks, exits 0 and
                # writes nothing (src/cli/build_v2.rs), so the file decides.
                _backend = b.returncode == 0 and (Path(d) / "m").exists()
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
        # Every lane the run asked for: a sanitizer lane left out here would
        # report nothing and read as a pass.
        for backend in ("interp", "build", "asan", "tsan"):
            for code, stdout, stderr in results.get(backend, []):
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
