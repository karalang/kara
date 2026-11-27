"""docs/library/core-types.md § Parsing and § Printing, against Python.

- `Display` of `f64` and `f32`: random values (bit patterns of every
  exponent, subnormals, integers near 2^53, decimal ties such as
  `-75455689800034.625`, powers of ten at the exponent-form edges) must
  print as the page's rule says, which is Python's `repr` (shortest digits
  that read back, exponent form below 1e-4 and from 1e16, a tie to the even
  digit) with `NaN` for `nan`; an `f32` with the shortest digits that read
  back at single precision.
- `f64.parse`: random texts (signs, a leading or trailing point,
  exponents, many digits, the special values in any case, surrounding
  whitespace, junk) must give Python's `float()` bit for bit, and fail
  where it fails.
- Integer `parse` on every type: random texts must give the page's
  answer: whitespace trimmed, one leading sign, `Empty`, `InvalidDigit`,
  `Overflow` and `Underflow` as the page assigns them, `-0` zero for every
  type.

    python3 conformance/numtext_model.py [--seed N] [--cases N] [--lanes interp,build,asan]
"""

import argparse
import math
import random
import struct

from harness import Report, kara_str, run

INTS = {t: (-(2 ** (b - 1)), 2 ** (b - 1) - 1) for t, b in (("i8", 8), ("i16", 16), ("i32", 32), ("i64", 64))}
INTS.update({t: (0, 2 ** b - 1) for t, b in (("u8", 8), ("u16", 16), ("u32", 32), ("u64", 64))})


def f64_of_bits(b):
    return struct.unpack("<d", struct.pack("<Q", b))[0]


def f32_round(x):
    return struct.unpack("<f", struct.pack("<f", x))[0]


def rand_f64(rng):
    k = rng.randrange(8)
    if k == 0:
        return f64_of_bits(rng.getrandbits(64))
    if k == 1:
        return f64_of_bits(rng.getrandbits(52))  # subnormal
    if k == 2:
        return float(rng.choice([1, -1]) * (2 ** 53 + rng.randrange(-5, 6)))
    if k == 3:
        return rng.choice([1e16, 1e15, 9999999999999998.0, 1e-4, 1e-5, 0.0001, 0.00009999, 1e22, 1e21, 5e-324,
                           1.7976931348623157e308, -0.0, 0.0, 0.1, 0.3, 2.5, -75455689800034.625, 123456789012345.67])
    if k == 4:
        return rng.randrange(-10**6, 10**6) / rng.choice([1, 2, 4, 8, 1024, 10, 100, 3])
    if k == 5:
        return rng.uniform(-1, 1) * 10 ** rng.randrange(-30, 30)
    if k == 6:
        return float(rng.randrange(-10**17, 10**17)) + rng.choice([0.5, 0.25, 0.125, 0.0])
    return rng.choice([float("inf"), float("-inf"), float("nan")])


def repr_rule(x):
    """The page's rendering of an f64: Python's repr, with NaN."""
    if math.isnan(x):
        return "NaN"
    return repr(x)


def reads_back_f32(v, x):
    """Whether the decimal `v` (a Fraction) rounds to the positive f32 `x`
    when read at single precision, ties to even."""
    from fractions import Fraction
    bx = struct.unpack("<I", struct.pack("<f", x))[0]
    lo = struct.unpack("<f", struct.pack("<I", bx - 1))[0] if bx > 0 else 0.0
    hi_bits = bx + 1
    if hi_bits >= 0x7F800000:
        hi = Fraction(2) ** 128  # rounding past f32's max goes to inf
    else:
        hi = Fraction(struct.unpack("<f", struct.pack("<I", hi_bits))[0])
    fx, flo = Fraction(x), Fraction(lo)
    a, b = (flo + fx) / 2, (fx + hi) / 2
    even = bx % 2 == 0
    return (a < v < b) or (even and (v == a or v == b))


def shortest_f32(x):
    """The page's rendering of an f32 value `x` (already rounded): the
    fewest significant digits that read back at single precision, the
    nearest of those to `x` (a tie to the even digit), laid out as repr."""
    if math.isnan(x):
        return "NaN"
    if math.isinf(x):
        return "inf" if x > 0 else "-inf"
    if x == 0:
        return "-0.0" if math.copysign(1, x) < 0 else "0.0"
    from fractions import Fraction
    fx = Fraction(x)
    for p in range(1, 18):
        mant, ex = f"{abs(x):.{p - 1}e}".split("e")
        n0, exp = int(mant.replace(".", "")), int(ex) - (p - 1)
        cands = []
        for n in (n0 - 1, n0, n0 + 1):
            if n <= 0:
                continue
            v = Fraction(n) * Fraction(10) ** exp
            if reads_back_f32(v, abs(x)):
                cands.append((abs(v - abs(fx)), n % 2, n))
        if cands:
            n = min(cands)[2]
            break
    dg = str(n)
    e10 = exp + len(dg) - 1  # exponent of the first digit
    dg = dg.rstrip("0") or "0"
    sign = "-" if x < 0 else ""
    if -4 <= e10 < 16:
        if e10 >= 0:
            ip = dg[:e10 + 1].ljust(e10 + 1, "0")
            fp = dg[e10 + 1:] or "0"
            return f"{sign}{ip}.{fp}"
        return f"{sign}0.{'0' * (-e10 - 1)}{dg}"
    m = dg[0] + ("." + dg[1:] if len(dg) > 1 else "")
    return f"{sign}{m}e{'+' if e10 >= 0 else '-'}{abs(e10):02d}"


def rand_float_text(rng):
    k = rng.randrange(7)
    if k == 0:
        return rng.choice(["inf", "-inf", "+inf", "Infinity", "-INFINITY", "nan", "NaN", "-nan", "+NaN", "infinit", "na"])
    ws = lambda: rng.choice(["", "", " ", "\t", "\n", "  "])
    sign = rng.choice(["", "", "-", "+", "--", "+-"])
    ip = "".join(rng.choice("0123456789") for _ in range(rng.randrange(0, 20)))
    fp = "".join(rng.choice("0123456789") for _ in range(rng.randrange(0, 20)))
    body = ip + (rng.choice([".", ".", ""]) + fp if fp or rng.random() < 0.3 else "")
    if rng.random() < 0.5:
        body += rng.choice(["e", "E"]) + rng.choice(["", "-", "+"]) + str(rng.randrange(0, 400))
    if rng.random() < 0.1:
        body += rng.choice(["x", "e", ".", "_", "1 2"])
    return ws() + sign + body + ws()


def py_float(s):
    t = s.strip(" \t\n")
    if "_" in t or t != t.strip():
        return None
    try:
        return float(t)
    except ValueError:
        return None


def bits(x):
    return struct.unpack("<Q", struct.pack("<d", x))[0]


def rand_int_text(rng, t):
    lo, hi = INTS[t]
    k = rng.randrange(8)
    ws = lambda: rng.choice(["", "", " ", "\t", "\n"])
    if k == 0:
        return rng.choice(["", " ", "-", "+", "-0", "+0", "0", "00", "-00", "1-2", "+-1", "1 2", "0x10", "١", "1_0"])
    n = rng.choice([lo, hi, lo - 1, hi + 1, 0, -1, 1, rng.randrange(lo, hi + 1),
                    rng.choice([1, -1]) * 10 ** rng.randrange(1, 25)])
    s = str(n)
    if n >= 0 and rng.random() < 0.3:
        s = "+" + s
    if rng.random() < 0.2:
        s = s[0] + "0" + s[1:] if s[0] in "+-" else "0" + s
    return ws() + s + ws()


def int_rule(t, s):
    lo, hi = INTS[t]
    u = s.strip(" \t\n")
    if u == "":
        return "Empty"
    sign, digits = "", u
    if u[0] in "+-":
        sign, digits = u[0], u[1:]
    if digits == "" or not all(c in "0123456789" for c in digits):
        return "InvalidDigit"
    v = int(digits) * (-1 if sign == "-" else 1)
    if v > hi:
        return "Overflow"
    if v < lo:
        return "Underflow"
    return f"O{v}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=400)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("numtext_model")
    fns, calls, want = [], [], []
    per = 50  # statements per function (B-2026-11-27-71)
    stmts = []
    for c in range(args.cases):
        x = rand_f64(rng)
        b = bits(x)
        stmts.append(f'    println(f"{{u64.bits_as_f64(id_u64({b}))}}");')
        want.append(repr_rule(x))
        F32_MAX = 3.4028234663852886e38
        if math.isnan(x) or math.isinf(x):
            y = x
        elif abs(x) >= F32_MAX + 2.0 ** 103:  # past max + half an ulp: inf
            y = math.copysign(float("inf"), x)
        elif abs(x) > F32_MAX:
            y = math.copysign(F32_MAX, x)
        else:
            y = f32_round(x)
        if not math.isnan(y):
            stmts.append(f'    println(f"{{u64.bits_as_f64(id_u64({bits(y)})) as f32}}");')
            want.append(shortest_f32(y))
        s = rand_float_text(rng)
        pv = py_float(s)
        stmts.append(f"    println(fp(f64.parse({kara_str(s)})));")
        want.append("E" if pv is None else ("NaN" if math.isnan(pv) else f"O{bits(pv)}"))
        t = rng.choice(list(INTS))
        s = rand_int_text(rng, t)
        stmts.append(f"    println(ip(\"{t}\", {t}.parse({kara_str(s)})));")
        want.append(int_rule(t, s))
    for i in range(0, len(stmts), per):
        fns.append(f"fn part{i // per}() {{\n" + "\n".join(stmts[i:i + per]) + "\n}\n")
        calls.append(f"    part{i // per}();")
    head = ("fn id_u64(v: u64) -> u64 {\n    v\n}\n\n"
            "fn fp(r: Result[f64, ParseError]) -> String {\n    match r {\n"
            "        Ok(v) => if v.is_nan() { \"NaN\".to_string() } else { f\"O{v.to_bits()}\" },\n"
            "        Err(_) => \"E\".to_string(),\n    }\n}\n\n"
            "fn ip[T: Display](t: Str, r: Result[T, ParseError]) -> String {\n    match r {\n"
            "        Ok(v) => f\"O{v}\",\n        Err(ParseError.Empty) => \"Empty\".to_string(),\n"
            "        Err(ParseError.InvalidDigit) => \"InvalidDigit\".to_string(),\n"
            "        Err(ParseError.Overflow) => \"Overflow\".to_string(),\n"
            "        Err(ParseError.Underflow) => \"Underflow\".to_string(),\n"
            "        Err(_) => \"Invalid\".to_string(),\n    }\n}\n\n")
    src = head + "\n".join(fns) + "\nfn main() {\n" + "\n".join(calls) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    rep.check_runs(res, [f"line {i}" for i in range(len(want))], want)
    rep.finish()


if __name__ == "__main__":
    main()
