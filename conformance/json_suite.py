#!/usr/bin/env python3
"""json.md against JSONTestSuite and a number round-trip oracle.

Parser: every `test_parsing/` file of Nicolas Seriot's JSONTestSuite
(https://github.com/nst/JSONTestSuite). A `y_` file must parse, an `n_` file
must be refused, and an `i_` file may go either way; the page's own rules
decide the `i_` files it speaks to (127 levels of nesting) and the rest are
reported, not judged. A file that is not UTF-8 cannot reach `parse` (it takes
`Str`), so `fs.read_to_string` refusing it counts as the refusal.

Writer: for every `y_` file, `stringify` of the parsed value must equal the
text the page specifies, computed here from Python's parse of the same file:
compact, keys in their first place with their last value, an integer that
fits an `i64` as `Int`, any other number as the shortest `f64` text in
serde_json's layout with a signed exponent, and serde_json's string
escapes.

Numbers: random `f64` bit patterns and the edge cases (`-0`, subnormals,
the largest and smallest finite values, powers of ten at the layout
boundaries) must stringify to that text and read back to the same bits, and
fixed texts (`1e400`, `-0`, integers just past `i64`) must parse as the page
and serde_json say.

    JSONTESTSUITE=<checkout> python3 conformance/json_suite.py --seed 1

Without JSONTESTSUITE only the number cases run.
"""

import argparse
import json
import math
import os
import random
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import Report, kara_str, run  # noqa: E402

I64_MIN, I64_MAX = -(2**63), 2**63 - 1


def ryu_text(x: float) -> str:
    """`x` as `stringify` writes a `Number`: the shortest digits that read
    back (Python's repr finds them), laid out as serde_json 1.0.149 lays them
    out through `zmij` (positional when the decimal exponent is in -5..=15,
    as `zmij::Buffer::format_finite` decides), with `+` on a positive
    exponent (json.md)."""
    if math.isnan(x) or math.isinf(x):
        return "null"
    neg = math.copysign(1.0, x) < 0
    if x == 0:
        return "-0.0" if neg else "0.0"
    r = repr(abs(x))
    mant, _, exp = r.partition("e")
    e = int(exp) if exp else 0
    if "." in mant:
        ip, fp = mant.split(".")
    else:
        ip, fp = mant, ""
    digits = (ip + fp).lstrip("0")
    # value = 0.<digits> * 10^kk; k is the exponent of the last digit.
    point = len(ip) + e  # digits before the decimal point, from ip's start
    lead_zeros = len(ip + fp) - len((ip + fp).lstrip("0"))
    kk = point - lead_zeros
    digits = digits.rstrip("0") or "0"
    n = len(digits)
    k = kk - n
    if 0 <= k and kk <= 16:
        s = digits + "0" * k + ".0"
    elif 0 < kk <= 16:
        s = digits[:kk] + "." + digits[kk:]
    elif -5 < kk <= 0:
        s = "0." + "0" * (-kk) + digits
    else:
        ex = kk - 1
        sign = "-" if ex < 0 else "+"
        s = (digits if n == 1 else digits[0] + "." + digits[1:]) + "e" + sign + str(abs(ex))
    return ("-" if neg else "") + s


def esc(s: str) -> str:
    out = ['"']
    for ch in s:
        o = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif ch == "\r":
            out.append("\\r")
        elif ch == "\t":
            out.append("\\t")
        elif ch == "\b":
            out.append("\\b")
        elif ch == "\f":
            out.append("\\f")
        elif o < 0x20:
            out.append("\\u%04x" % o)
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def parse_int(s: str):
    v = int(s)
    if s.lstrip().startswith("-") and v == 0:
        return -0.0
    return v if I64_MIN <= v <= I64_MAX else float(v)


def parse_float(s: str):
    v = float(s)
    if math.isinf(v):
        raise OverflowError(s)
    return v


class Pairs(list):
    """A JSON object as Python read it, in order, repeats kept."""


def text_of(v) -> str:
    if v is None:
        return "null"
    if v is True:
        return "true"
    if v is False:
        return "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return ryu_text(v)
    if isinstance(v, str):
        return esc(v)
    if isinstance(v, list) and not isinstance(v, Pairs):
        return "[" + ",".join(text_of(x) for x in v) + "]"
    # An object (Pairs): first place, last value.
    order, last = [], {}
    for k, x in v:
        if k not in last:
            order.append(k)
        last[k] = x
    return "{" + ",".join(esc(k) + ":" + text_of(last[k]) for k in order) + "}"


def expected_stringify(raw: bytes):
    """The page's text for a `y_` file, or None when Python cannot read it
    (a lone surrogate, a number past f64)."""
    try:
        text = raw.decode("utf-8")
        v = json.loads(text, parse_int=parse_int, parse_float=parse_float,
                       object_pairs_hook=Pairs, parse_constant=lambda c: (_ for _ in ()).throw(ValueError(c)))
        s = text_of(v)
        s.encode("utf-8")  # a lone surrogate cannot be written
        return s
    except (ValueError, OverflowError, UnicodeError):
        return None


PROGRAM = """import std.json.{Json};

fn main() {
    let paths: Vec[String] = [%s];
    for p in paths {
        match fs.read_to_string(p) {
            Ok(t) => match Json.parse(t) {
                Ok(v) => println(f"OK {v.stringify()}"),
                Err(e) => println(f"ERR {e}"),
            },
            Err(_) => println("NOTUTF8"),
        }
    }
    let bits: Vec[u64] = [%s];
    for b in bits {
        let s = Json.Number(b.bits_as_f64()).stringify();
        match Json.parse(s) {
            Ok(Json.Number(y)) => println(f"N {s} {y.to_bits()}"),
            Ok(Json.Int(i)) => println(f"N {s} int {i}"),
            Ok(_) => println(f"N {s} other"),
            Err(e) => println(f"N {s} ERR {e}"),
        }
    }
}
"""

# Fixed parse texts and the page's (serde_json's) answer.
FIXED = [
    ("-0", "OK -0.0"),
    ("0", "OK 0"),
    ("-0.0", "OK -0.0"),
    ("1e400", "ERR number out of range at line 1 column 5"),
    ("-1e400", "ERR number out of range at line 1 column 6"),
    ("1e-400", "OK 0.0"),
    ("9223372036854775807", "OK 9223372036854775807"),
    ("-9223372036854775808", "OK -9223372036854775808"),
    ("9223372036854775808", "OK 9.223372036854776e+18"),
    ("-9223372036854775809", "OK -9.223372036854776e+18"),
    ("123456789012345678901234567890", "OK 1.2345678901234568e+29"),
    ("1E2", "OK 100.0"),
    ("1.5e-7", "OK 1.5e-7"),
    ("0.00001", "OK 0.00001"),
    ("0.000001", "OK 1e-6"),
    ("1e-7", "OK 1e-7"),
    ("1e21", "OK 1e+21"),
    ("1e20", "OK 1e+20"),
    ("1e15", "OK 1000000000000000.0"),
    ("[1.0, 2]", "OK [1.0,2]"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--count", type=int, default=400)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    rep = Report("json")

    import tempfile
    tmp = Path(tempfile.mkdtemp(prefix="kara-json-"))
    files = []  # (label, path, kind, raw)
    suite = os.environ.get("JSONTESTSUITE")
    if suite:
        for f in sorted((Path(suite) / "test_parsing").glob("*.json")):
            files.append((f.name, f, f.name[0], f.read_bytes()))
    for i, (text, _) in enumerate(FIXED):
        p = tmp / f"fixed_{i}.json"
        p.write_text(text)
        files.append((f"fixed {text}", p, "f", text.encode()))

    specials = [0.0, -0.0, 5e-324, -5e-324, 2.2250738585072014e-308, 1.7976931348623157e308,
                -1.7976931348623157e308, 0.1, 1.0, 1e15, 1e16, 1e17, 1e20, 1e21, 1e22, 1e-4,
                1e-5, 1e-6, 1e-7, 123456.789, 2.0**53, 2.0**63, 0.3, 1 / 3]
    floats = list(specials)
    while len(floats) < len(specials) + a.count:
        b = rng.getrandbits(64)
        x = struct.unpack("<d", struct.pack("<Q", b))[0]
        if not (math.isnan(x) or math.isinf(x)):
            floats.append(x)
    bits = [struct.unpack("<Q", struct.pack("<d", x))[0] for x in floats]

    src = PROGRAM % (", ".join(kara_str(str(p)) for _, p, _, _ in files),
                     ", ".join(f"{b}u64" for b in bits))
    res = run(src, timeout=1800)
    if res["check"][0] != 0:
        rep.failed.append("check refused the program:\n" + res["check"][1] + res["check"][2])
        rep.finish()

    indeterminate = {}
    for backend in ("interp", "build"):
        for code, stdout, stderr in res.get(backend, []):
            if code != 0:
                rep.failed.append(f"[{backend}] exit {code}: {stderr[-2000:]}")
            lines = stdout.split("\n")
            if lines and lines[-1] == "":
                lines.pop()
            want_n = len(files) + len(bits)
            if len(lines) != want_n:
                rep.failed.append(f"[{backend}] {len(lines)} lines, want {want_n}")
                continue
            for (label, _, kind, raw), got in zip(files, lines[: len(files)]):
                ok = got.startswith("OK ")
                if kind == "y":
                    want = expected_stringify(raw)
                    if not ok:
                        rep.failed.append(f"[{backend}] {label}: refused a y_ file: {got}")
                    elif want is not None and got[3:] != want:
                        rep.failed.append(f"[{backend}] {label}: stringify {got[3:]!r}, want {want!r}")
                    else:
                        rep.passed += 1
                elif kind == "n":
                    if ok:
                        rep.failed.append(f"[{backend}] {label}: accepted an n_ file: {got}")
                    else:
                        rep.passed += 1
                elif kind == "i":
                    indeterminate.setdefault(label, {})[backend] = got
                    # The page's own rule: deeper than 127 levels is an error.
                    if label == "i_structure_500_nested_arrays.json":
                        if ok:
                            rep.failed.append(f"[{backend}] {label}: 500 levels accepted; json.md caps 127")
                        else:
                            rep.passed += 1
                else:
                    want = dict(FIXED)[label[len("fixed "):]]
                    if got != want:
                        rep.failed.append(f"[{backend}] {label}: got {got!r}, want {want!r}")
                    else:
                        rep.passed += 1
            for x, b, got in zip(floats, bits, lines[len(files):]):
                want = f"N {ryu_text(x)} {b}"
                if got != want:
                    rep.failed.append(f"[{backend}] f64 {x!r}: got {got!r}, want {want!r}")
                else:
                    rep.passed += 1

    if indeterminate:
        print("i_ files (reported, not judged):")
        for label, by in sorted(indeterminate.items()):
            vals = sorted(set(by.values()))
            v = vals[0] if len(vals) == 1 else " | ".join(f"{k}: {by[k]}" for k in sorted(by))
            print(f"  {label}: {v[:120]}")
    rep.finish()


if __name__ == "__main__":
    main()
