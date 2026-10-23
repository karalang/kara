#!/usr/bin/env python3
"""string-matrix.py — sweep String operations across every surface and report
where they disagree.

WHY THIS EXISTS. `scripts/width-matrix.py` did for integer widening what hand
tests could not: it swept a cross-product instead of the boundaries someone
happened to think of, and found six bugs in an afternoon. The String surface has
the same shape of risk — many operations, implemented separately in the
interpreter (`interpreter/`) and in codegen (`codegen/vec_method.rs`,
`codegen/sso.rs`, `codegen/interner.rs`), with a small-string optimization and an
interner sitting under some of them and not others. So "String is broken" is not
the failure mode; "String.<op> is broken on <this shape of input>" is.

    operation  x  input shape  x  surface

The INTERPRETER is the oracle, on the same evidence width-matrix.py cites: every
occurrence of this class so far has had the interpreter right and a compiled
backend wrong.

INPUT SHAPES ARE THE POINT, exactly as boundary values were there. The shapes
that distinguish implementations are the ones nobody types by hand: the empty
string, a string that is ONLY the separator, a separator at both ends, a doubled
separator, and — the one that separates a byte-oriented implementation from a
codepoint-oriented one — multi-byte UTF-8. A sweep over "hello" and "world"
proves nothing, in the same way that a width sweep over the byte 97 proved
nothing.

Usage:
    python3 scripts/string-matrix.py                 # full sweep
    python3 scripts/string-matrix.py --quick         # one input per op
    python3 scripts/string-matrix.py --op split      # filter to matching ops
    python3 scripts/string-matrix.py --keep DIR      # keep generated .kara files
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# Input shapes. Each entry is (label, Kāra string literal). The interesting ones
# are the degenerate and the multi-byte; the ordinary ones are here so a
# divergence can be localized to a shape rather than to "strings".
INPUTS = [
    ("empty",        '""'),
    ("one",          '"a"'),
    ("plain",        '"one two three"'),
    ("lead_sep",     '" ab"'),
    ("trail_sep",    '"ab "'),
    ("both_sep",     '" ab "'),
    ("double_sep",   '"a  b"'),
    ("only_sep",     '" "'),
    ("only_seps",    '"   "'),
    ("latin1",       '"café"'),
    ("cjk",          '"日本語"'),
    ("emoji",        '"a🙂b"'),
    ("mixed",        '"a é 日 b"'),
    ("long",         '"' + "abcdefghij" * 12 + '"'),
]

# Operation sites. `{S}` is the input literal. Every site must print something
# derived from the result, because the comparison is on stdout.
OPS = {
    "len":            'let s = {S};\n    println(f"len {{s.len()}}");',
    "is_empty":       'let s = {S};\n    println(f"is_empty {{s.is_empty()}}");',
    "bytes_len":      'let s = {S};\n    let b = s.bytes();\n    println(f"bytes {{b.len()}}");',
    "bytes_sum":      'let s = {S};\n    let b = s.bytes();\n    let mut t = 0i64;\n    let mut i = 0i64;\n    while i < b.len() {{ t = t + b[i] as i64; i = i + 1i64; }}\n    println(f"bytes_sum {{t}}");',
    "chars_count":    'let s = {S};\n    let c = s.chars();\n    println(f"chars {{c.len()}}");',
    "concat_self":    'let s = {S};\n    let t = s + s;\n    println(f"concat [{{t}}] {{t.len()}}");',
    "concat_loop":    'let s = {S};\n    let mut t = "";\n    let mut i = 0i64;\n    while i < 5i64 {{ t = t + s; i = i + 1i64; }}\n    println(f"concat_loop [{{t}}] {{t.len()}}");',
    "split_space":    'let s = {S};\n    let p = s.split(" ");\n    let mut o = "";\n    let mut i = 0i64;\n    while i < p.len() {{ o = o + "<" + p[i] + ">"; i = i + 1i64; }}\n    println(f"split {{p.len()}} {{o}}");',
    "split_ws":       'let s = {S};\n    let p = s.split_whitespace();\n    let mut o = "";\n    let mut i = 0i64;\n    while i < p.len() {{ o = o + "<" + p[i] + ">"; i = i + 1i64; }}\n    println(f"split_ws {{p.len()}} {{o}}");',
    "trim":           'let s = {S};\n    println(f"trim [{{s.trim()}}]");',
    "trim_start":     'let s = {S};\n    println(f"trim_start [{{s.trim_start()}}]");',
    "trim_end":       'let s = {S};\n    println(f"trim_end [{{s.trim_end()}}]");',
    "to_upper":       'let s = {S};\n    println(f"upper [{{s.to_uppercase()}}]");',
    "to_lower":       'let s = {S};\n    println(f"lower [{{s.to_lowercase()}}]");',
    "repeat":         'let s = {S};\n    println(f"repeat [{{s.repeat(3i64)}}]");',
    "contains":       'let s = {S};\n    let a = s.contains("a");\n    let b = s.contains(" ");\n    println(f"contains {{a}} {{b}}");',
    "starts_ends":    'let s = {S};\n    let a = s.starts_with("a");\n    let b = s.ends_with("b");\n    println(f"se {{a}} {{b}}");',
    "find":           'let s = {S};\n    println(f"find {{s.find("a")}}");',
    "replace":        'let s = {S};\n    println(f"replace [{{s.replace(" ", "_")}}]");',
    "eq_ne":          'let s = {S};\n    let t = {S};\n    let a = s == t;\n    let b = s != t;\n    println(f"eq {{a}} {{b}}");',
    "cmp":            'let s = {S};\n    let a = s < "b";\n    let b = s > "b";\n    println(f"cmp {{a}} {{b}}");',
    "vec_push_idx":   'let mut v: Vec[String] = Vec.new();\n    v.push({S});\n    v.push({S});\n    println(f"vec [{{v[0i64]}}] [{{v[1i64]}}] {{v.len()}}");',
    "vec_join":       'let mut v: Vec[String] = Vec.new();\n    v.push({S});\n    v.push({S});\n    println(f"join [{{v.join(",")}}]");',
    "set_roundtrip":  'let mut st: Set[String] = Set.new();\n    st.insert({S});\n    let c = st.contains({S});\n    println(f"set {{c}} {{st.len()}}");',
    "map_roundtrip":  'let mut m: Map[String, i64] = Map.new();\n    m.insert({S}, 7i64);\n    let c = m.contains_key({S});\n    println(f"map {{c}} {{m.len()}}");',
    "fstr_interp":    'let s = {S};\n    println(f"interp [{{s}}] [{{s}}{{s}}]");',
    # B-2026-08-14-20: `s.bytes()` is a `Slice[u8]` and `from_utf8` wants a
    # `Vec[u8]`, with nothing bridging them — so the round trip has to copy. The
    # copy is written out here so this op measures the CONVERSION rather than
    # re-reporting the type error on every input shape.
    "from_utf8":      'let s = {S};\n    let b = s.bytes();\n    let mut v: Vec[u8] = Vec.new();\n    let mut i = 0i64;\n    while i < b.len() {{ v.push(b[i]); i = i + 1i64; }}\n    match String.from_utf8(v) {{\n        Ok(t) => println(f"utf8 ok [{{t}}]"),\n        Err(_) => println("utf8 err"),\n    }}',
    "push_str":       'let mut s = {S};\n    s.push_str("XY");\n    println(f"push_str [{{s}}] {{s.len()}}");',
    "substring":      'let s = {S};\n    let n = s.len();\n    let mut hi = 2i64;\n    if hi > n {{ hi = n; }}\n    println(f"substring [{{s.substring(0i64, hi)}}]");',
    "char_at":        'let s = {S};\n    if s.len() > 0i64 {{ println(f"char_at [{{s.char_at(0i64)}}]"); }} else {{ println("char_at none"); }}',
}


def run(cmd, cwd=None, env=None):
    """Capture BYTES, not text. A String operation that emits invalid UTF-8 is
    itself a finding, and decoding here would crash the sweep instead of
    reporting it."""
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, timeout=180)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, b"", b"timeout"


def show(b):
    """Printable form of a captured byte string: valid UTF-8 as itself, invalid
    bytes backslash-escaped so the report survives them."""
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return b.decode("utf-8", "backslashreplace")


def is_valid_utf8(b):
    try:
        b.decode("utf-8")
        return True
    except UnicodeDecodeError:
        return False


def surfaces(path, workdir):
    """{surface: stdout} for every surface that produced one, plus failures."""
    out, bad = {}, {}
    rc, so, se = run(["karac", "run", "--interp", str(path)])
    if rc != 0:
        return None, {"interp": show(se or so).strip().splitlines()[:2]}
    out["interp"] = so

    rc, so, se = run(["karac", "run", str(path)])
    if rc == 0:
        out["jit"] = so
    else:
        bad["jit"] = show(se or so).strip().splitlines()[:2]

    stem = path.stem
    for label, env_extra in (("aot", {}), ("aot_seq", {"KARAC_AUTO_PAR": "0"})):
        env = dict(os.environ, **env_extra)
        binp = workdir / stem
        if binp.exists():
            binp.unlink()
        rc, so, se = run(["karac", "build", str(path)], cwd=workdir, env=env)
        if rc != 0 or not binp.exists():
            bad[label] = show(se or so).strip().splitlines()[:2]
            continue
        rc, so, se = run([str(binp)], cwd=workdir)
        if rc == 0:
            out[label] = so
        else:
            bad[label] = show(se or so).strip().splitlines()[:2]
    return out, bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true")
    ap.add_argument("--op", default="")
    ap.add_argument("--keep", default="")
    args = ap.parse_args()

    names = [n for n in OPS if not args.op or args.op in n]
    if not names:
        print(f"no op matches {args.op!r}", file=sys.stderr)
        return 2
    inputs = INPUTS[:1] + INPUTS[9:10] if args.quick else INPUTS

    workdir = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="strmat-"))
    workdir.mkdir(parents=True, exist_ok=True)

    divergences, unsupported, invalid_utf8, cases = [], [], [], 0
    for op in names:
        for label, lit in inputs:
            body = OPS[op].format(S=lit)
            src = "fn main() {\n    " + body + "\n}\n"
            path = workdir / f"{op}_{label}.kara"
            path.write_text(src)
            got, bad = surfaces(path, workdir)
            cases += 1
            if got is None:
                unsupported.append((op, label, bad["interp"]))
                continue
            oracle = got["interp"]
            if not is_valid_utf8(oracle):
                invalid_utf8.append((op, label, "interp", show(oracle).strip()))
            for surf, val in got.items():
                if surf != "interp" and val != oracle:
                    divergences.append((op, label, surf, show(oracle).strip(), show(val).strip()))
            for surf, why in bad.items():
                divergences.append((op, label, surf, show(oracle).strip(), f"FAILED: {why}"))
        print(f"  {op:16s} done", file=sys.stderr)

    print(f"\n{cases} cases · {len(divergences)} divergences · "
          f"{len(invalid_utf8)} invalid-UTF-8 outputs · "
          f"{len(unsupported)} not accepted by the interpreter")
    if invalid_utf8:
        print("\nINVALID UTF-8 ON STDOUT (all surfaces agreed, and all were wrong):")
        for op, label, surf, val in invalid_utf8:
            print(f"  {op:16s} {label:12s} {val}")
    if unsupported:
        print("\nNOT ACCEPTED (op/shape the front end rejects — a gap, not a divergence):")
        for op, label, why in unsupported:
            print(f"  {op:16s} {label:12s} {why[0] if why else ''}")
    if divergences:
        print("\nDIVERGENCES (interpreter is the oracle):")
        for op, label, surf, want, got in divergences:
            print(f"  {op:16s} {label:12s} {surf:8s}")
            print(f"      interp: {want}")
            print(f"      {surf:6s}: {got}")
    if not args.keep:
        shutil.rmtree(workdir, ignore_errors=True)
    return 1 if divergences else 0


if __name__ == "__main__":
    sys.exit(main())
