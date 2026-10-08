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
              "expected_from", "legacy_backend", "legacy_exit", "rules", "backends", "env", "note"]

# A backend is either a single command that compiles and runs (`run`) or a
# compile step that leaves ./source behind and is then executed (`build`).
# `check` runs `karac check` first and counts its refusal as the backend's.
# `mir-interp` is the hidden `karac __mir-run` (single-file programs: MIR
# builder, drop elaboration, MIR interpreter); the other mir-* slots have no
# command until MIR->LLVM exists.
BACKENDS = {
    "legacy-interp": {"run": ["run", "--interp"]},
    "legacy-build": {"build": ["build"], "env": {"KARAC_AUTO_PAR": "0"}},
    "mir-interp": {"check": True, "run": ["__mir-run"]},
    "mir-llvm": None,
    "mir-llvm-asan": None,
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
                r = subprocess.run(["./source"], cwd=tmpd, env=env, capture_output=True,
                                   timeout=timeout, stdin=subprocess.DEVNULL)
            else:
                r = subprocess.run([karac, *spec["run"], "source.kara"], cwd=tmpd, env=env,
                                   capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL)
                if r.returncode != 0 and REFUSAL.search(r.stderr) and b"panicked" not in r.stderr \
                        and b"runtime error" not in r.stderr:
                    return {"status": "refused", "exit": r.returncode, "stdout": r.stdout,
                            "stderr": r.stderr, "secs": time.monotonic() - start}
        except subprocess.TimeoutExpired as t:
            return {"status": "timeout", "exit": None, "stdout": t.stdout or b"", "stderr": t.stderr or b"",
                    "secs": time.monotonic() - start}
    return {"status": "ran", "exit": r.returncode, "stdout": r.stdout, "stderr": r.stderr,
            "secs": time.monotonic() - start}


def error_codes(text: bytes) -> list[str]:
    """Diagnostic codes named in a refusal's output (`error[E0500]`-style or a JSON `code`)."""
    found = re.findall(rb"\b([EWN]\d{4})\b", text)
    return list(dict.fromkeys(x.decode() for x in found))
