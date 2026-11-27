"""docs/library/core-types.md § Numbers, against Rust's integer methods,
which the page names as the meaning of each: for `i8` .. `i64` and `u8` ..
`u64`, random operands (the type's edges, small values, random ones) through
the `checked_`, `wrapping_`, `saturating_` and `overflowing_` families of
`add`, `sub`, `mul`, `div`, `rem`, `neg`, `shl`, `shr` and `pow` (the
saturating ones the page lists), the `abs` families and `unsigned_abs`,
the six bit counts and `BITS`, the byte orders and their inverses,
`div_euclid`, `rem_euclid` and `div_ceil`, `isqrt`, `ilog2`, `ilog10`,
`ilog` and their `checked_` forms, and `parse` of each value's text with
whitespace and a `+`. Calls that panic (a zero divisor outside `checked_`,
a logarithm of a value not positive, `isqrt` of a negative one) are left
out; overflow.py and the page's cells hold those. The Rust side is one
program `rustc` builds.

    python3 conformance/int_methods_model.py [--seed N] [--cases N] [--lanes interp,build,asan]
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

TYPES = {t: (-(2 ** (b - 1)), 2 ** (b - 1) - 1, b) for t, b in (("i8", 8), ("i16", 16), ("i32", 32), ("i64", 64))}
TYPES.update({t: (0, 2 ** b - 1, b) for t, b in (("u8", 8), ("u16", 16), ("u32", 32), ("u64", 64))})


def val(rng, t):
    lo, hi, _ = TYPES[t]
    return rng.choice([lo, lo + 1, hi, hi - 1, 0, 1, 2, 3, 7, 10, 100, -1 if lo else 5, -7 if lo else 9,
                       rng.randrange(lo, hi + 1), rng.randrange(max(lo, -1000), min(hi, 1000) + 1)])


def klit(t, v):
    lo = TYPES[t][0]
    if v == lo and lo < 0:
        return f"(id_{t}({v + 1}) - id_{t}(1))"
    return f"id_{t}({v})"


def rlit(t, v):
    lo = TYPES[t][0]
    if v == lo and lo < 0:
        return f"{t}::MIN"
    return f"({v}{t})"


def ops(t, x, y, e, base):
    """(name, kara expr, rust expr) for each call that cannot panic."""
    lo, hi, bits = TYPES[t]
    signed = lo < 0
    out = []
    for op in ("add", "sub", "mul", "div", "rem"):
        out.append((f"checked_{op}", f"o(x.checked_{op}(y))", f"o(x.checked_{op}(y))"))
        if op in ("div", "rem") and y == 0:
            continue
        out.append((f"wrapping_{op}", f"x.wrapping_{op}(y)", f"x.wrapping_{op}(y)"))
        out.append((f"overflowing_{op}", f"p(x.overflowing_{op}(y))", f"p(x.overflowing_{op}(y))"))
        if op != "rem":
            out.append((f"saturating_{op}", f"x.saturating_{op}(y)", f"x.saturating_{op}(y)"))
    out.append(("checked_neg", "o(x.checked_neg())", "o(x.checked_neg())"))
    out.append(("wrapping_neg", "x.wrapping_neg()", "x.wrapping_neg()"))
    out.append(("overflowing_neg", "p(x.overflowing_neg())", "p(x.overflowing_neg())"))
    if signed:
        out.append(("saturating_neg", "x.saturating_neg()", "x.saturating_neg()"))
        for f in ("checked_abs", "overflowing_abs"):
            w = "o" if f.startswith("checked") else "p"
            out.append((f, f"{w}(x.{f}())", f"{w}(x.{f}())"))
        out += [("wrapping_abs", "x.wrapping_abs()", "x.wrapping_abs()"),
                ("saturating_abs", "x.saturating_abs()", "x.saturating_abs()"),
                ("unsigned_abs", "x.unsigned_abs()", "x.unsigned_abs()")]
    for op in ("shl", "shr"):
        out.append((f"checked_{op}", f"o(x.checked_{op}(e))", f"o(x.checked_{op}(e))"))
        out.append((f"wrapping_{op}", f"x.wrapping_{op}(e)", f"x.wrapping_{op}(e)"))
        out.append((f"overflowing_{op}", f"p(x.overflowing_{op}(e))", f"p(x.overflowing_{op}(e))"))
    for f in ("checked_pow", "overflowing_pow"):
        w = "o" if f.startswith("checked") else "p"
        out.append((f, f"{w}(x.{f}(e))", f"{w}(x.{f}(e))"))
    out += [("wrapping_pow", "x.wrapping_pow(e)", "x.wrapping_pow(e)"),
            ("saturating_pow", "x.saturating_pow(e)", "x.saturating_pow(e)")]
    for f in ("count_ones", "count_zeros", "leading_zeros", "trailing_zeros", "leading_ones", "trailing_ones"):
        out.append((f, f"x.{f}()", f"x.{f}()"))
    out.append(("BITS", f"{t}.BITS", f"{t}::BITS"))
    out.append(("bytes", "b(x.to_le_bytes()) + \" \" + b(x.to_be_bytes())",
                "format!(\"{} {}\", b(&x.to_le_bytes()), b(&x.to_be_bytes()))"))
    out.append(("from_bytes", f"{t}.from_be_bytes(x.to_le_bytes())", f"{t}::from_be_bytes(x.to_le_bytes())"))
    out.append(("from_le", f"{t}.from_le_bytes(x.to_le_bytes())", f"{t}::from_le_bytes(x.to_le_bytes())"))
    if y != 0:
        if not (signed and x == lo and y == -1):
            out.append(("div_euclid", "x.div_euclid(y)", "x.div_euclid(y)"))
            out.append(("div_ceil", "x.div_ceil(y)", "dceil(x as i128, y as i128)"))
        out.append(("rem_euclid", "x.rem_euclid(y)", "x.rem_euclid(y)" if not (signed and x == lo and y == -1) else "0"))
    if x >= 0:
        out.append(("isqrt", "x.isqrt()", "x.isqrt()"))
    out += [("checked_ilog2", "o(x.checked_ilog2())", "o(x.checked_ilog2())"),
            ("checked_ilog10", "o(x.checked_ilog10())", "o(x.checked_ilog10())"),
            ("checked_ilog", f"o(x.checked_ilog({klit(t, base)}))", f"o(x.checked_ilog({rlit(t, base)}))")]
    if x > 0:
        out += [("ilog", f"x.ilog2() + x.ilog10() * 100 + x.ilog({klit(t, base)}) * 10000",
                 f"x.ilog2() + x.ilog10() * 100 + x.ilog({rlit(t, base)}) * 10000")]
    sign = "+" if x >= 0 else ""
    out.append(("parse", f"r({t}.parse(\" {sign}\" + x.to_string() + \"\\n\"))",
                f"r(format!(\" {sign}{{}}\\n\", x).trim().parse::<{t}>())"))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=60)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("int_methods_model")
    rustc = os.environ.get("RUSTC") or shutil.which("rustc")
    if not rustc:
        sys.exit("int_methods_model: no rustc")
    kfns, rmain, kmain = [], [], []
    for c in range(args.cases):
        t = rng.choice(list(TYPES))
        x, y = val(rng, t), val(rng, t)
        e = rng.choice([0, 1, 2, 3, 7, TYPES[t][2] - 1, TYPES[t][2], TYPES[t][2] + 1, 63, 64, 100])
        base = rng.choice([2, 3, 10, 16, 7])
        lines = ops(t, x, y, e, base)
        # Each case is its own function: B-2026-11-27-71 makes one long
        # function slow to check.
        kb = [f"fn case{c}() {{", f"    let x = {klit(t, x)};", f"    let y = {klit(t, y)};", f"    let e: u32 = {e};"]
        rb = [f"    {{ let x: {t} = {rlit(t, x)}; let y: {t} = {rlit(t, y)}; let e: u32 = {e};"]
        for name, ke, re_ in lines:
            kb.append(f'    println(f"{c} {name} {{{ke}}}");')
            rb.append(f'    println!("{c} {name} {{}}", {re_});')
        kb.append("}")
        rb.append("    }")
        kfns.append("\n".join(kb))
        kmain.append(f"    case{c}();")
        rmain.append("\n".join(rb))
    helpers = "".join(f"fn id_{t}(v: {t}) -> {t} {{\n    v\n}}\n\n" for t in TYPES)
    khead = (helpers + "fn o[T: Display](v: Option[T]) -> String {\n    match v {\n        Some(x) => f\"S{x}\",\n"
             "        None => \"N\".to_string(),\n    }\n}\n\n"
             "fn p[T: Display](v: (T, bool)) -> String {\n    f\"{v.0}/{v.1}\"\n}\n\n"
             "fn r[T: Display](v: Result[T, ParseError]) -> String {\n    match v {\n        Ok(x) => f\"O{x}\",\n"
             "        Err(_) => \"E\".to_string(),\n    }\n}\n\n"
             "fn b[const N: i64](a: Array[u8, N]) -> String {\n    let mut s = String.new();\n    for x in a {\n"
             "        s.push_str(f\"{x},\");\n    }\n    s\n}\n\n")
    rhead = ("#![allow(unused, overflowing_literals)]\nuse std::fmt::Display;\n"
             "fn o<T: Display>(v: Option<T>) -> String { match v { Some(x) => format!(\"S{}\", x), None => \"N\".into() } }\n"
             "fn p<T: Display>(v: (T, bool)) -> String { format!(\"{}/{}\", v.0, v.1) }\n"
             "fn r<T: Display, E>(v: Result<T, E>) -> String { match v { Ok(x) => format!(\"O{}\", x), Err(_) => \"E\".into() } }\n"
             "fn b(a: &[u8]) -> String { a.iter().map(|x| format!(\"{},\", x)).collect() }\n"
             "fn dceil(a: i128, b: i128) -> i128 { let q = a / b; if (a % b != 0) && ((a < 0) == (b < 0)) { q + 1 } else { q } }\n")
    with tempfile.TemporaryDirectory(prefix="intm-rs-") as d:
        (Path(d) / "m.rs").write_text(rhead + "fn main() {\n" + "\n".join(rmain) + "\n}\n")
        bres = subprocess.run([rustc, "-O", "--edition", "2021", "-o", str(Path(d) / "m"), str(Path(d) / "m.rs")],
                              capture_output=True, text=True)
        if bres.returncode != 0:
            sys.exit("rustc failed:\n" + bres.stderr[-4000:])
        want = subprocess.run([str(Path(d) / "m")], capture_output=True, text=True, check=True).stdout.splitlines()
    src = khead + "\n\n".join(kfns) + "\n\nfn main() {\n" + "\n".join(kmain) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    rep.check_runs(res, [" ".join(w.split(" ", 2)[:2]) for w in want], want)
    rep.finish()


if __name__ == "__main__":
    main()
