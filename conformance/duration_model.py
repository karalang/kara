"""docs/library/time-random-env.md § `Clock` (`Duration`), against Go's
`time.Duration`, whose format the page names. Random spans built with
`Duration.secs`, `ms`, `us` and `ns` (values to the edges that stay in
range), combined with `+`, `-`, `* k` and `/ k` (rounded toward zero, as
Go's integer division is), compared, and read back with `as_secs`,
`as_ms`, `as_us` and `as_ns` (toward zero). Each span is printed with
`Display` and must equal Go's `Duration.String()`; every reader and
comparison must equal Go's integer arithmetic on the nanosecond count.

    python3 conformance/duration_model.py [--seed N] [--cases N] [--lanes interp,build,asan]

`GO=<path>` picks the go binary (default `go` on PATH).
"""

import argparse
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from harness import Report, run

I64 = (-2**63, 2**63 - 1)
UNITS = {"secs": 10**9, "ms": 10**6, "us": 10**3, "ns": 1}
GO_UNIT = {"secs": "time.Second", "ms": "time.Millisecond", "us": "time.Microsecond", "ns": "time.Nanosecond"}


def rand_ns(rng):
    """A nanosecond count, often near a unit boundary."""
    k = rng.randrange(6)
    if k == 0:
        return rng.choice([0, 1, -1, 999, 1000, 1001, 999_999, 1_000_000, 1_500_000, 999_999_999,
                           10**9, 10**9 + 1, 60 * 10**9, 3600 * 10**9, 90 * 10**9, -1500])
    if k == 1:
        return rng.randrange(-10**12, 10**12)
    if k == 2:
        return rng.randrange(I64[0] // 4, I64[1] // 4)
    if k == 3:
        return rng.choice([I64[1], I64[0] + 1, I64[1] - 1, I64[0]])
    return rng.choice([1, -1]) * rng.randrange(0, 10**rng.randrange(1, 18))


def make(rng):
    """(kara expr, go expr, ns) for a constructed span in range."""
    unit = rng.choice(list(UNITS))
    m = UNITS[unit]
    v = rand_ns(rng) // m
    if not (I64[0] <= v * m <= I64[1]):
        v = 0
    lit = f"(id({v + 1}) - 1)" if v == I64[0] else f"id({v})"
    return f"Duration.{unit}({lit})", f"time.Duration({v})*{GO_UNIT[unit]}", v * m


def trunc_div(a, b):
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b > 0) else -q


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=300)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("duration_model")
    go = os.environ.get("GO") or shutil.which("go")
    if not go:
        sys.exit("duration_model: no go binary (set GO=<path>)")
    kara, gol = [], []
    for c in range(args.cases):
        ka, ga, a = make(rng)
        kb, gb, b = make(rng)
        ops = []
        if I64[0] <= a + b <= I64[1]:
            ops.append(("+", f"{ka} + {kb}", f"({ga} + {gb})"))
        if I64[0] <= a - b <= I64[1]:
            ops.append(("-", f"{ka} - {kb}", f"({ga} - {gb})"))
        k = rng.choice([2, 3, -1, 7, 1000, -4])
        if I64[0] <= a * k <= I64[1]:
            ops.append(("*", f"{ka} * id({k})", f"({ga} * {k})"))
        if not (a == I64[0] and k == -1):
            ops.append(("/", f"{ka} / id({k})", f"({ga} / {k})"))
        ops.append(("=", ka, ga))
        name, ke, ge = rng.choice(ops)
        kara.append(f"    {{\n        let d = {ke};\n        let e = {kb};\n"
                    f'        println(f"{c} {{d}} {{d.as_secs()}} {{d.as_ms()}} {{d.as_us()}} {{d.as_ns()}} '
                    f'{{d < e}} {{d == e}} {{d >= e}}");\n    }}')
        gol.append(f"\t{{\n\t\td := {ge}\n\t\te := {gb}\n"
                   f'\t\tfmt.Println("{c}", d.String(), int64(d/time.Second), int64(d/time.Millisecond), '
                   f'int64(d/time.Microsecond), int64(d), d < e, d == e, d >= e)\n\t}}')
    ksrc = ("import std.time.{Duration};\n\nfn id(x: i64) -> i64 {\n    x\n}\n\nfn main() {\n"
            + "\n".join(kara) + "\n}\n")
    gsrc = "package main\n\nimport (\n\t\"fmt\"\n\t\"time\"\n)\n\nfunc main() {\n" + "\n".join(gol) + "\n}\n"
    with tempfile.TemporaryDirectory(prefix="dur-go-") as d:
        (Path(d) / "main.go").write_text(gsrc)
        g = subprocess.run([go, "run", "main.go"], cwd=d, capture_output=True, text=True,
                           env=dict(os.environ, GOFLAGS="-mod=mod", GO111MODULE="off"))
        if g.returncode != 0:
            sys.exit("go failed:\n" + g.stderr[-3000:])
    want = g.stdout.splitlines()
    res = run(ksrc, backends=tuple(args.lanes.split(",")))
    rep.check_runs(res, [w.split(" ", 1)[0] for w in want], want)
    rep.finish()


if __name__ == "__main__":
    main()
