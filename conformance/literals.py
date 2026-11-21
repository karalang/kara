#!/usr/bin/env python3
"""design.md §3 "Numeric, character and boolean literals": random literals
against Python's reading of the same text.

Integers in every base with `_` between digits, at random magnitudes up to
the type's bound and at the bounds themselves; floats in decimal notation
with a point, an exponent or both, `_` in the mantissa and the exponent,
from subnormals to near `f64.MAX`. Python's `int(text, 0)` and `float(text)`
accept the same spellings (Python also takes `_` between digits), so the
oracle is Python's value: an integer is printed, and a float is compared by
its bits (`to_bits()`), so a misrounded last digit cannot hide behind a
printed form. An `f32`-suffixed literal is compared with Python's value
rounded to single precision.

    python3 conformance/literals.py [--seed N] [--count N]
"""

import argparse
import os
import random
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, run  # noqa: E402

INTS = [("i8", 8, True), ("i16", 16, True), ("i32", 32, True), ("i64", 64, True), ("i128", 128, True),
        ("u8", 8, False), ("u16", 16, False), ("u32", 32, False), ("u64", 64, False), ("u128", 128, False)]


def group(digits, rng):
    """`digits` with a `_` between some pairs."""
    out = digits[0]
    for d in digits[1:]:
        if rng.random() < 0.2:
            out += "_"
        out += d
    return out


def int_literal(rng):
    t, bits, signed = rng.choice(INTS)
    hi = (1 << (bits - 1)) - 1 if signed else (1 << bits) - 1
    v = rng.choice([hi, 0, 1, rng.randint(0, hi), rng.randint(0, min(hi, 1 << rng.randint(1, bits)))])
    base, prefix, fmt = rng.choice([(10, "", "d"), (16, "0x", "x"), (16, "0x", "X"), (2, "0b", "b"), (8, "0o", "o")])
    text = prefix + group(format(v, fmt), rng)
    if signed and rng.random() < 0.4:
        # A negative literal down to MIN, whose magnitude is one past MAX.
        m = rng.choice([hi + 1, rng.randint(1, hi + 1)])
        return t, "-" + prefix + group(format(m, fmt), rng), -m
    return t, text, v


def float_literal(rng):
    mant = str(rng.randint(0, 10 ** rng.randint(1, 18)))
    frac = "".join(rng.choice("0123456789") for _ in range(rng.randint(1, 18)))
    exp = rng.choice([None, rng.randint(-330, 300), rng.randint(-40, 40)])
    kind = rng.choice(["point", "exp", "both"]) if exp is not None else "point"
    if kind == "exp":
        text = group(mant, rng) + rng.choice("eE") + (rng.choice(["", "+"]) if exp >= 0 else "-") + group(str(abs(exp)), rng)
    else:
        text = group(mant, rng) + "." + group(frac, rng)
        if kind == "both":
            text += rng.choice("eE") + (rng.choice(["", "+"]) if exp >= 0 else "-") + group(str(abs(exp)), rng)
    v = float(text)
    return text, v


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--count", type=int, default=1500)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    rep = Report("literals")
    lines, want, labels = [], [], []
    for i in range(a.count):
        r = rng.random()
        if r < 0.45:
            t, text, v = int_literal(rng)
            suffixed = rng.random() < 0.5
            lit = text + t if suffixed and "0x" not in text else text
            if suffixed and "0x" in text:
                lines.append(f"    let x{i}: {t} = {text};\n    println(x{i});")
            elif suffixed:
                lines.append(f"    println({lit});")
            else:
                lines.append(f"    let x{i}: {t} = {text};\n    println(x{i});")
            want.append(str(v))
            labels.append(f"{lit} : {t}")
        else:
            text, v = float_literal(rng)
            if v == float("inf"):
                continue
            if rng.random() < 0.2 and abs(v) < 3.4e38:
                single = struct.unpack("<f", struct.pack("<f", v))[0]
                if single == float("inf"):
                    continue
                lines.append(f"    println({text}f32.to_bits32());")
                want.append(str(struct.unpack("<I", struct.pack("<f", single))[0]))
                labels.append(text + "f32")
            else:
                lines.append(f"    println({text}.to_bits());")
                want.append(str(struct.unpack("<Q", struct.pack("<d", v))[0]))
                labels.append(text)
    src = "fn main() {\n" + "\n".join(lines) + "\n}\n"
    lanes = tuple(os.environ.get("CONF_LANES", "interp,build").split(","))
    res = run(src, backends=lanes, timeout=1800)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + (res["check"][1] + res["check"][2])[-3000:])
        rep.finish()
    for backend in lanes:
        for code, out, err in res.get(backend, []):
            if code != 0:
                rep.failed.append(f"[{backend}] exit {code}: {err[-1500:]}")
            rep.compare(backend, labels, out.splitlines(), want)
    rep.finish()


if __name__ == "__main__":
    main()
