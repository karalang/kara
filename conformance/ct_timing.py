#!/usr/bin/env python3
"""crypto.md's timing promise for `constant_time_eq`, measured.

"For equal lengths the time taken depends only on the length, not on where
the bytes differ." A compiled program compares one 1 MiB buffer with two
others of the same length, one differing at the first byte and one at the
last, many times each, interleaved, and prints the total time of each. Plain
`==` on the same pairs is the positive control: it stops at the first
difference, so its early/late ratio shows the instrument can see a
data-dependent comparison. `Secret.ct_eq` (secret.md) is measured the same
way.

The verdict is a ratio, so it is noisy: a run fails only when the
constant-time ratio leaves [1/1.5, 1.5] on the median of the repeats while
the control's ratio is above 5 (the instrument works). A control that
cannot tell the two apart voids the run, and that is a failure too.

    python3 conformance/ct_timing.py [--repeats N]
"""

import argparse
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, has_backend, run  # noqa: E402

PROGRAM = """import std.crypto.{constant_time_eq};
import std.secret.{Secret};
import std.time.{Duration, Instant};

// Each comparison runs on two buffers, x and y, against a; x differs from a
// at the first byte and y at the last, then the two positions are swapped
// and it runs again. Summing each position over both buffers cancels any
// effect of where a buffer sits in memory, which a single phase measured
// as a steady 1.27x on one of them. The byte at index 1 changes every
// round, in all three, so no comparison is loop-invariant.
fn ct(a: mut ref Vec[u8], x: mut ref Vec[u8], y: mut ref Vec[u8]) -> (i64, i64) {
    let mut tx = Duration.ns(0);
    let mut ty = Duration.ns(0);
    for r in 0..200 {
        let v = (r % 251) as u8;
        a[1] = v;
        x[1] = v;
        y[1] = v;
        let t = Instant.now();
        if constant_time_eq(a.as_slice(), x.as_slice()) { println("equal"); }
        tx += t.elapsed();
        let t = Instant.now();
        if constant_time_eq(a.as_slice(), y.as_slice()) { println("equal"); }
        ty += t.elapsed();
    }
    (tx.as_ns(), ty.as_ns())
}

fn eq(a: mut ref Vec[u8], x: mut ref Vec[u8], y: mut ref Vec[u8]) -> (i64, i64) {
    let mut tx = Duration.ns(0);
    let mut ty = Duration.ns(0);
    for r in 0..200 {
        let v = (r % 251) as u8;
        a[1] = v;
        x[1] = v;
        y[1] = v;
        let t = Instant.now();
        if a == x { println("equal"); }
        tx += t.elapsed();
        let t = Instant.now();
        if a == y { println("equal"); }
        ty += t.elapsed();
    }
    (tx.as_ns(), ty.as_ns())
}

fn sec(a: mut ref Secret[Vec[u8]], x: mut ref Secret[Vec[u8]], y: mut ref Secret[Vec[u8]]) -> (i64, i64) {
    let mut tx = Duration.ns(0);
    let mut ty = Duration.ns(0);
    for r in 0..200 {
        let v = (r % 251) as u8;
        a.expose_mut()[1] = v;
        x.expose_mut()[1] = v;
        y.expose_mut()[1] = v;
        let t = Instant.now();
        if a.ct_eq(x) { println("equal"); }
        tx += t.elapsed();
        let t = Instant.now();
        if a.ct_eq(y) { println("equal"); }
        ty += t.elapsed();
    }
    (tx.as_ns(), ty.as_ns())
}

fn swap_ends(x: mut ref Vec[u8], y: mut ref Vec[u8]) {
    let n = x.len();
    let (x0, xn) = (x[0], x[n - 1]);
    x[0] = xn;
    x[n - 1] = x0;
    let (y0, yn) = (y[0], y[n - 1]);
    y[0] = yn;
    y[n - 1] = y0;
}

fn main() {
    let n: i64 = 1048576;
    let mut a: Vec[u8] = Vec.filled(n, 0u8);
    let mut x: Vec[u8] = Vec.filled(n, 0u8);
    let mut y: Vec[u8] = Vec.filled(n, 0u8);
    x[0] = 1u8;
    y[n - 1] = 1u8;
    let (e1, l1) = ct(mut a, mut x, mut y);
    let (e3, l3) = eq(mut a, mut x, mut y);
    swap_ends(mut x, mut y);
    let (l2, e2) = ct(mut a, mut x, mut y);
    let (l4, e4) = eq(mut a, mut x, mut y);
    println(f"ct {e1 + e2} {l1 + l2}");
    println(f"eq {e3 + e4} {l3 + l4}");
    let mut se: Vec[u8] = Vec.filled(n, 0u8);
    let mut sl: Vec[u8] = Vec.filled(n, 0u8);
    se[0] = 1u8;
    sl[n - 1] = 1u8;
    let mut sa: Secret[Vec[u8]] = Secret.new(Vec.filled(n, 0u8));
    let mut sx: Secret[Vec[u8]] = Secret.new(se);
    let mut sy: Secret[Vec[u8]] = Secret.new(sl);
    let (e5, l5) = sec(mut sa, mut sx, mut sy);
    swap_ends(mut sx.expose_mut(), mut sy.expose_mut());
    let (l6, e6) = sec(mut sa, mut sx, mut sy);
    println(f"secret {e5 + e6} {l5 + l6}");
}
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeats", type=int, default=5)
    a = ap.parse_args()
    rep = Report("ct_timing")
    if not has_backend():
        rep.failed.append("needs the compiled backend: timing under the interpreter measures the interpreter")
        rep.finish()
    res = run(PROGRAM, backends=("build",), repeat=a.repeats)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + res["check"][1] + res["check"][2])
        rep.finish()
    ratios = {"ct": [], "eq": [], "secret": []}
    for code, out, err in res.get("build", []):
        if code != 0:
            rep.failed.append(f"exit {code}: {err[-1500:]}")
            continue
        for line in out.splitlines():
            k, *v = line.split()
            if k in ratios:
                e, l = int(v[0]), int(v[1])
                ratios[k].append(l / max(e, 1))
            elif k == "equal":
                rep.failed.append("an unequal pair compared equal")
    med = {k: statistics.median(v) if v else None for k, v in ratios.items()}
    print("late/early time ratio, median of", a.repeats, {k: round(x, 3) if x else x for k, x in med.items()})
    if not med["eq"] or med["eq"] < 5:
        rep.failed.append(f"control `==` ratio {med['eq']}: the instrument cannot see an early exit; void")
    else:
        rep.passed += 1
    for k in ("ct", "secret"):
        x = med[k]
        if x is None or not (1 / 1.5 <= x <= 1.5):
            rep.failed.append(f"{k} late/early ratio {x}: time depends on where the bytes differ")
        else:
            rep.passed += 1
    rep.finish()


if __name__ == "__main__":
    main()
