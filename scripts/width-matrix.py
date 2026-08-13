#!/usr/bin/env python3
"""width-matrix.py — sweep the integer-widening cross-product across every
surface, and report where they disagree.

WHY THIS EXISTS. Kāra's widening coercions are implicit (`check_int_widening_
coercion` rejects only narrowing), so a narrower integer may appear at any site
that wants a wider one. Codegen has to perform that widening, with the SOURCE's
signedness, at every such site. There are many such sites and they are
implemented in different files — B-2026-08-13-15's fix alone touched nine — so
the failure mode is not "widening is broken" but "widening is broken at the one
boundary nobody wrote a test for".

Hand-written tests cover the boundaries someone thought of. This sweeps the
product instead:

    site  x  source type  x  boundary value  x  surface

and uses the INTERPRETER as the oracle, because on every occurrence of this bug
so far the interpreter has been right and the compiled backends wrong.

BOUNDARY VALUES ARE THE POINT. B-2026-08-13-15 was first filed with a scope
table calling four shapes correct; every probe in it used the byte 97, and 97 is
precisely where the bug cannot appear — sign- and zero-extension agree below 128
and the undefined high bytes happened to be zero. Re-probed with the high bit
set, 14 of 15 shapes were wrong. So every source type here carries a value whose
top bit is SET (unsigned) or which is NEGATIVE (signed): those are the only
values that distinguish sext from zext.

Usage:
    python3 scripts/width-matrix.py                 # full sweep
    python3 scripts/width-matrix.py --quick         # one value per source type
    python3 scripts/width-matrix.py --site vec_push # filter to matching sites
    python3 scripts/width-matrix.py --keep DIR      # keep generated .kara files
"""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# (source type, value literal, printed-as-i64 expectation is the interpreter's
# job — we never hardcode it, we only require the surfaces to agree with interp)
#
# Each entry: (type, [values]). The FIRST value of every entry has its high bit
# set (unsigned) or is negative (signed) — the discriminating case. The rest are
# controls, including the deliberately non-discriminating small value that made
# the original scope table wrong.
SOURCES = [
    ("u8",  ["200u8", "255u8", "127u8", "97u8", "0u8"]),
    ("i8",  ["-56i8", "-128i8", "127i8", "1i8"]),
    ("u16", ["60000u16", "65535u16", "255u16", "7u16"]),
    ("i16", ["-3000i16", "-32768i16", "32767i16"]),
    ("u32", ["4000000000u32", "4294967295u32", "65535u32"]),
    ("i32", ["-1000000i32", "-2147483648i32", "2147483647i32"]),
]

# Destinations a narrower type may legally widen into.
RANK = {"i8": 1, "u8": 1, "i16": 2, "u16": 2, "i32": 3, "u32": 3, "i64": 4}


def widens_to(src, dst):
    """Is src -> dst a widening this language permits implicitly?"""
    if src == dst:
        return False
    if dst == "i64":
        return RANK[src] < 4
    if dst == "i32":
        return RANK[src] < 3
    return False


# Each site is a fragment printing exactly one labelled line. {V} is the source
# binding, {D} the destination type. Sites are grouped so a compile error in one
# does not blind the others — see GROUPS.
SITES = {
    "let_annot":      'let sa: {D} = {V};\n    println(f"let_annot {{sa}}");',
    "fn_arg":         'println(f"fn_arg {{take_{D}({V})}}");',
    "method_arg":     'let mb = Box{D} {{ f: 0 as {D} }};\n    println(f"method_arg {{mb.echo({V})}}");',
    "struct_lit":     'let sl = Box{D} {{ f: {V} }};\n    println(f"struct_lit {{sl.f}}");',
    "field_assign":   'let mut fa = Box{D} {{ f: 0 as {D} }};\n    fa.f = {V};\n    println(f"field_assign {{fa.f}}");',
    "tuple_annot":    'let ta: ({D}, {D}) = ({V}, {V});\n    println(f"tuple_annot {{ta.0}} {{ta.1}}");',
    "array_elem":     'let ae: Array[{D}, 2] = [{V}, {V}];\n    println(f"array_elem {{ae[0i64]}}");',
    "vec_push":       'let mut vp: Vec[{D}] = Vec.new();\n    vp.push({V});\n    println(f"vec_push {{vp[0i64]}}");',
    "vec_contains":   'let mut vc: Vec[{D}] = Vec.new();\n    vc.push({V});\n    println(f"vec_contains {{vc.contains({V})}}");',
    "vec_idx_assign": 'let mut va: Vec[{D}] = Vec.new();\n    va.push(0 as {D});\n    va[0i64] = {V};\n    println(f"vec_idx_assign {{va[0i64]}}");',
    "set_roundtrip":  'let mut sr: Set[{D}] = Set.new();\n    sr.insert({V});\n    println(f"set_roundtrip {{sr.contains({V})}} {{sr.len()}}");',
    "set_remove":     'let mut sm: Set[{D}] = Set.new();\n    sm.insert({V});\n    sm.remove({V});\n    println(f"set_remove {{sm.len()}}");',
    "map_roundtrip":  'let mut mr: Map[{D}, {D}] = Map.new();\n    mr.insert({V}, {V});\n    println(f"map_roundtrip {{mr.contains_key({V})}}");',
    "ret_coerce":     'println(f"ret_coerce {{ret_{D}({V})}}");',
    "cmp_wide":       'let cw: {D} = {V};\n    println(f"cmp_wide {{cw == ({V} as {D})}}");',
    "arith_wide":     'let aw: {D} = {V};\n    println(f"arith_wide {{aw + (1 as {D})}}");',
}

# Sites are emitted one program per group, so a hard compile error is contained
# to its group instead of blanking the whole sweep for that (type, value).
GROUPS = [
    ["let_annot", "fn_arg", "ret_coerce", "cmp_wide", "arith_wide"],
    ["struct_lit", "field_assign", "method_arg"],
    ["tuple_annot", "array_elem"],
    ["vec_push", "vec_contains", "vec_idx_assign"],
    ["set_roundtrip", "set_remove", "map_roundtrip"],
]

PRELUDE = """struct Box{D} {{ f: {D} }}

impl Box{D} {{
    fn echo(ref self, x: {D}) -> {D} {{ return x; }}
}}

fn take_{D}(x: {D}) -> {D} {{ return x; }}
fn ret_{D}(x: {D}) -> {D} {{ return x; }}
"""


def gen(dst, value, sites):
    body = "\n    ".join(SITES[s].format(D=dst, V=value) for s in sites)
    return PRELUDE.format(D=dst) + "\nfn main() {\n    " + body + "\n}\n"


def run(cmd, cwd=None, env=None):
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=120)
        return p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        return 124, "", "timeout"


def surfaces(path, workdir):
    """Return {surface: output} for every surface that produced one, plus a
    dict of surfaces that failed with the reason."""
    out, bad = {}, {}
    rc, so, se = run(["karac", "run", "--interp", str(path)])
    if rc != 0:
        return None, {"interp": (se or so).strip().splitlines()[:2]}
    out["interp"] = so

    rc, so, se = run(["karac", "run", str(path)])
    if rc == 0:
        out["jit"] = so
    else:
        bad["jit"] = (se or so).strip().splitlines()[:2]

    stem = path.stem
    for label, env_extra in (("aot", {}), ("aot_seq", {"KARAC_AUTO_PAR": "0"})):
        env = dict(os.environ, **env_extra)
        binp = workdir / f"{stem}"
        if binp.exists():
            binp.unlink()
        rc, so, se = run(["karac", "build", str(path)], cwd=workdir, env=env)
        if rc != 0 or not binp.exists():
            bad[label] = (se or so).strip().splitlines()[:2]
            continue
        rc, so, se = run([str(binp)], cwd=workdir)
        if rc == 0:
            out[label] = so
        else:
            bad[label] = (se or so).strip().splitlines()[:2]
    return out, bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="one value per source type")
    ap.add_argument("--site", default=None, help="only groups containing a site matching this substring")
    ap.add_argument("--keep", default=None, help="directory to keep generated .kara files in")
    args = ap.parse_args()

    if not shutil.which("karac"):
        print("karac not found on PATH — build it with `cargo build --release --features llvm`", file=sys.stderr)
        return 2

    groups = GROUPS
    if args.site:
        groups = [g for g in GROUPS if any(args.site in s for s in g)]
        if not groups:
            print(f"no group contains a site matching {args.site!r}", file=sys.stderr)
            return 2

    workdir = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="widthmatrix-"))
    workdir.mkdir(parents=True, exist_ok=True)

    divergences, skips, compiled, cases = [], [], 0, 0
    for dst in ("i64", "i32"):
        for src, values in SOURCES:
            if not widens_to(src, dst):
                continue
            vals = values[:1] if args.quick else values
            for value in vals:
                for gi, group in enumerate(groups):
                    name = f"w_{dst}_{src}_{gi}_{value.replace('-', 'neg')}"
                    path = workdir / f"{name}.kara"
                    path.write_text(gen(dst, value, group), encoding="utf-8")
                    out, bad = surfaces(path, workdir)
                    if out is None:
                        skips.append((dst, src, value, group, "interp", bad["interp"]))
                        continue
                    compiled += 1
                    ref = out["interp"].splitlines()
                    cases += len(ref)
                    for label, reason in bad.items():
                        skips.append((dst, src, value, group, label, reason))
                    for label, text in out.items():
                        if label == "interp":
                            continue
                        got = text.splitlines()
                        for i, line in enumerate(ref):
                            g = got[i] if i < len(got) else "<missing>"
                            if g != line:
                                divergences.append((dst, src, value, label, line, g))

    print(f"width-matrix: {compiled} programs, {cases} case-lines, "
          f"{len(divergences)} divergences, {len(skips)} surface skips")
    print(f"generated sources: {workdir}")

    if divergences:
        print("\nDIVERGENCES (interpreter is the oracle):")
        seen = set()
        for dst, src, value, label, want, got in divergences:
            site = want.split()[0]
            key = (site, src, dst, label)
            if key in seen:
                continue
            seen.add(key)
            print(f"  {site:<16} {src:>4} -> {dst:<4} [{label:<7}]  interp: {want!r}  got: {got!r}  (value {value})")
        print(f"\n  {len(divergences)} raw, {len(seen)} distinct (site, src, dst, surface)")

    if skips:
        print("\nSURFACE SKIPS (compile or run failure — may be a legitimate rejection):")
        seen = set()
        for dst, src, value, group, label, reason in skips:
            key = (tuple(group), src, dst, label)
            if key in seen:
                continue
            seen.add(key)
            print(f"  {'/'.join(group)[:44]:<46} {src:>4} -> {dst:<4} [{label}] {reason}")

    return 1 if divergences else 0


if __name__ == "__main__":
    sys.exit(main())
