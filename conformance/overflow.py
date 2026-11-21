#!/usr/bin/env python3
"""design.md §5 "Integer overflow" and "Division, remainder and shifts" on
every integer type and every lane.

For each of `i8` .. `i128`, `u8` .. `u128`, `isize`, `usize`:

- one program of the operations that do not trap at the edges of the type
  (`MAX - 1 + 1`, `MIN / 1`, `1 << (bits - 1)`, arithmetic `>>`, truncating
  `/` and `%`, and the `wrapping_`, `checked_`, `saturating_` and
  `overflowing_` forms the page names as the escape), with the expected
  values computed here from the page's rules;
- one program per trap the page lists (`MAX + 1`, `MIN - 1`, `MAX * 2`,
  `-MIN`, `MIN / -1`, `MIN % -1`, `/ 0`, `% 0`, a shift by the width or by
  a negative amount either way, `MIN.abs()`, and `MAX += 1`): it prints a
  line, traps, and must exit 101 (core-semantics.md §10.2) with the
  page's message on stderr and only that line on stdout.

Operands come through a function, so no constant is folded at check time
(a constant overflow is a compile error, which is a different rule).

    python3 conformance/overflow.py
"""

import os
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, run  # noqa: E402

TYPES = [("i8", 8, True), ("i16", 16, True), ("i32", 32, True), ("i64", 64, True), ("i128", 128, True),
         ("isize", 64, True), ("u8", 8, False), ("u16", 16, False), ("u32", 32, False), ("u64", 64, False),
         ("u128", 128, False), ("usize", 64, False)]
OVERFLOW, DIVZERO, SHIFT = "overflow", "division by zero", "shift amount out of range"


def bounds(bits, signed):
    return (-(1 << (bits - 1)), (1 << (bits - 1)) - 1) if signed else (0, (1 << bits) - 1)


def wrap(v, bits, signed):
    v &= (1 << bits) - 1
    return v - (1 << bits) if signed and v >> (bits - 1) else v


def tdiv(a, b):
    q = abs(a) // abs(b)
    return q if (a < 0) == (b < 0) else -q


def edges(t, bits, signed):
    """(Kāra expression, expected text) pairs that must not trap."""
    lo, hi = bounds(bits, signed)
    rows = [
        (f"id({t}.MAX) - 1 + 1", hi),
        (f"id({t}.MIN) + 0", lo),
        (f"id({t}.MIN) / 1", lo),
        (f"id({t}.MAX) / id({t}.MAX)", 1),
        (f"id(1) << amt({bits - 1})", wrap(1 << (bits - 1), bits, signed)),
        (f"id({t}.MAX) >> amt({bits - 1})", hi >> (bits - 1)),
        (f"id({t}.MAX) >> amt(0)", hi),
        (f"id({t}.MAX).wrapping_add(1)", lo),
        (f"id({t}.MIN).wrapping_sub(1)", hi),
        (f"id({t}.MAX).wrapping_mul(2)", wrap(hi * 2, bits, signed)),
        (f"id(1).wrapping_shl(amt({bits}) as u32)", 1),
        (f"id(1).wrapping_shl(amt({bits + 3}) as u32)", wrap(1 << 3, bits, signed)),
        (f"id({t}.MAX).saturating_add(1)", hi),
        (f"id({t}.MIN).saturating_sub(1)", lo),
        (f"id({t}.MAX).checked_add(1).is_none()", "true"),
        (f"id({t}.MAX).checked_sub(1).unwrap()", hi - 1),
        (f"id(7).checked_div(0).is_none()", "true"),
        (f"id({t}.MAX).overflowing_add(1).0", lo),
        (f"id({t}.MAX).overflowing_add(1).1", "true"),
        (f"id({t}.MAX).overflowing_add(0).1", "false"),
    ]
    if signed:
        rows += [
            (f"id(-7) / id(2)", tdiv(-7, 2)),
            (f"id(-7) % id(2)", -7 - tdiv(-7, 2) * 2),
            (f"id(7) % id(-2)", 7 - tdiv(7, -2) * -2),
            (f"id(-8) >> amt(1)", -4),
            (f"id({t}.MIN) >> amt({bits - 1})", -1),
            (f"id({t}.MIN) % id(1)", 0),
            (f"id({t}.MIN + 1).abs()", hi),
            (f"-id({t}.MAX)", -hi),
            (f"id({t}.MIN).wrapping_div(-1)", lo),
            (f"id({t}.MIN).wrapping_rem(-1)", 0),
            (f"id({t}.MIN).wrapping_neg()", lo),
            (f"id({t}.MIN).checked_div(-1).is_none()", "true"),
            (f"id({t}.MIN).checked_neg().is_none()", "true"),
            (f"id({t}.MIN).saturating_mul(2)", lo),
            (f"id({t}.MIN).overflowing_neg().0", lo),
        ]
    else:
        rows += [
            (f"id(7) / id(2)", 3),
            (f"id(7) % id(2)", 1),
            (f"id(0).wrapping_sub(1)", hi),
            (f"id(0).saturating_sub(1)", 0),
            (f"id(0).checked_sub(1).is_none()", "true"),
        ]
    return [(e, str(v)) for e, v in rows]


def traps(t, bits, signed):
    rows = [
        ("add", f"id({t}.MAX) + 1", OVERFLOW),
        ("sub", f"id({t}.MIN) - 1", OVERFLOW),
        ("mul", f"id({t}.MAX) * 2", OVERFLOW),
        ("div_zero", f"id(1) / id(0)", DIVZERO),
        ("rem_zero", f"id(1) % id(0)", DIVZERO),
        ("shl_width", f"id(1) << amt({bits})", SHIFT),
        ("shr_width", f"id(1) >> amt({bits})", SHIFT),
        ("shl_negative", "id(1) << amt(-1)", SHIFT),
        ("shr_negative", "id(1) >> amt(-1)", SHIFT),
        ("compound_add", f"{{ let mut x = id({t}.MAX); x += 1; x }}", OVERFLOW),
    ]
    if signed:
        rows += [
            ("neg", f"-id({t}.MIN)", OVERFLOW),
            ("div_overflow", f"id({t}.MIN) / id(-1)", OVERFLOW),
            ("rem_overflow", f"id({t}.MIN) % id(-1)", OVERFLOW),
            ("abs", f"id({t}.MIN).abs()", OVERFLOW),
        ]
    else:
        rows += [("sub_zero", "id(0) - 1", OVERFLOW)]
    return rows


def prelude(t):
    return f"fn id(x: {t}) -> {t} {{ x }}\nfn amt(x: i64) -> i64 {{ x }}\n"


def main():
    rep = Report("overflow")
    lanes = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))
    jobs = []
    for t, bits, signed in TYPES:
        rows = edges(t, bits, signed)
        body = "".join(f"    println(f\"{{{e}}}\");\n" for e, _ in rows)
        jobs.append((f"{t} edges", prelude(t) + "fn main() {\n" + body + "}\n",
                     [w for _, w in rows], [e for e, _ in rows], None))
        for name, e, msg in traps(t, bits, signed):
            src = prelude(t) + f"fn main() {{\n    println(\"before\");\n    let r: {t} = {e};\n    println(f\"{{r}}\");\n}}\n"
            jobs.append((f"{t} {name}: {e}", src, ["before"], ["before"], msg))

    def one(job):
        return job, run(job[1], backends=lanes, timeout=600)

    with ThreadPoolExecutor(max_workers=int(os.environ.get("CONF_JOBS", "3"))) as ex:
        for (label, _, want, labels, msg), res in ex.map(one, jobs):
            if res["check"][0] != 0:
                rep.failed.append(f"{label}: check refused it:\n{(res['check'][1] + res['check'][2])[-1500:]}")
                continue
            for backend in lanes:
                for code, out, err in res.get(backend, []):
                    if msg is None:
                        if code != 0:
                            rep.failed.append(f"[{backend}] {label}: exit {code}: {err[-800:]}")
                        rep.compare(backend, [f"{label}: {x}" for x in labels], out.splitlines(), want)
                    elif code != 101 or out.splitlines() != want or msg not in err:
                        rep.failed.append(f"[{backend}] {label}: exit {code}, stdout {out!r}, "
                                          f"want 101 and {msg!r} on stderr: {err[-400:]!r}")
                    else:
                        rep.passed += 1
    rep.finish()


if __name__ == "__main__":
    main()
