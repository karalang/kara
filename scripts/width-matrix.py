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

THREE AXES, because the rule has three halves and each has its own failure mode:

    (default)    int -> wider int    must widen with the SOURCE's signedness
    --floats     int -> f64 / f32    must convert with sitofp vs uitofp likewise
                                     (B-2026-08-13-18's half, whose fix note
                                     records a signedness-BLIND boundary sweep
                                     turning a build failure into a wrong answer)
    --narrowing  wide -> narrower    must be REJECTED at compile time; a site
                                     that accepts one truncates silently, which
                                     no runtime oracle can catch

The narrowing mode is the inverse test: it asserts a compile ERROR, so it runs
`karac check` per site and reports any site that accepts what the language says
it refuses.

The roundtrip axis exists because the other three all pair a NARROW source with
a WIDE destination, which leaves a whole quadrant untested: a narrow value living
in a container of its own width, where no coercion should happen at all. Three of
this file's findings (B-2026-08-13-22, B-2026-08-14-3, B-2026-08-14-4) are in
that quadrant — a `Vec[u8]` holding 200 is fine, but an `Array[u8]`, a
`Vec[Struct-with-u8-field]` and any generic at `T = u8` are not, and every one of
them sign-extends on the way out.

Usage:
    python3 scripts/width-matrix.py                 # int -> int sweep
    python3 scripts/width-matrix.py --floats        # int -> float sweep
    python3 scripts/width-matrix.py --narrowing     # rejection sweep
    python3 scripts/width-matrix.py --roundtrip     # same-width roundtrip sweep
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


FLOAT_DSTS = ("f64", "f32")

# Legitimately inapplicable to a float destination: kāra refuses `Set[f64]` /
# `Map[f64, _]` because floats are not `Hash + Eq`. That is correct language
# behaviour, so these are excluded rather than reported as skips.
FLOAT_NA = {"set_roundtrip", "set_remove", "map_roundtrip"}

# Wide sources for the narrowing sweep, paired with a destination they must NOT
# implicitly reach and a value outside that destination's range — so that if a
# site does accept the coercion, the truncation is observable rather than
# theoretical.
NARROWING = [
    ("300i64", "u8"),
    ("300i64", "i8"),
    ("70000i64", "u16"),
    ("70000i64", "i16"),
    ("5000000000i64", "i32"),
    ("-1i64", "u8"),
]


def widens_to(src, dst):
    """Is src -> dst a widening this language permits implicitly?"""
    if src == dst:
        return False
    if dst in FLOAT_DSTS:
        return True
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


def gen(dst, value, sites, preamble=""):
    body = "\n    ".join(SITES[s].format(D=dst, V=value) for s in sites)
    return PRELUDE.format(D=dst) + "\nfn main() {\n    " + preamble + body + "\n}\n"


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


# --- roundtrip axis --------------------------------------------------------
# A narrow value in a container of its OWN type. No coercion is involved, so
# every one of these must read back exactly what went in.
RT_PRELUDE = """struct RtPlain{D} {{ a: {D} }}
struct RtBox{D}[T] {{ v: T }}

fn rt_id{D}[T](x: T) -> T {{ return x; }}
"""

RT_SITES = {
    "scalar":        'let sc: {D} = {V};\n    println(f"scalar {{sc}}");',
    "vec":           'let mut rv: Vec[{D}] = Vec.new();\n    rv.push({V});\n    println(f"vec {{rv[0i64]}}");',
    "set":           'let mut rs: Set[{D}] = Set.new();\n    rs.insert({V});\n    println(f"set {{rs.contains({V})}} {{rs.len()}}");',
    "map":           'let mut rm: Map[{D}, {D}] = Map.new();\n    rm.insert({V}, {V});\n    println(f"map {{rm.contains_key({V})}}");',
    "array":         'let ra: Array[{D}, 2] = [{V}, {V}];\n    println(f"array {{ra[0i64]}}");',
    "tuple":         'let rt: ({D}, {D}) = ({V}, {V});\n    println(f"tuple {{rt.0}}");',
    "struct_field":  'let rp = RtPlain{D} {{ a: {V} }};\n    println(f"struct_field {{rp.a}}");',
    "struct_in_vec": 'let rp2 = RtPlain{D} {{ a: {V} }};\n    let mut rvp: Vec[RtPlain{D}] = Vec.new();\n    rvp.push(rp2);\n    println(f"struct_in_vec {{rvp[0i64].a}}");',
    "generic_fn":    'println(f"generic_fn {{rt_id{D}({V})}}");',
    "generic_struct":'let rb: RtBox{D}[{D}] = RtBox{D} {{ v: {V} }};\n    println(f"generic_struct {{rb.v}}");',
}


def roundtrip_sweep(workdir, quick, site_filter):
    names = [n for n in RT_SITES if not site_filter or site_filter in n]
    divergences, skips, cases = [], [], 0
    for src, values in SOURCES:
        for value in (values[:1] if quick else values):
            for name in names:
                body = RT_SITES[name].format(D=src, V=value)
                text = RT_PRELUDE.format(D=src) + "\nfn main() {\n    " + body + "\n}\n"
                path = workdir / f"rt_{src}_{name}_{value.replace('-', 'neg')}.kara"
                path.write_text(text, encoding="utf-8")
                out, bad = surfaces(path, workdir)
                if out is None:
                    skips.append((src, value, name, "interp", bad["interp"]))
                    continue
                ref = out["interp"].splitlines()
                cases += len(ref)
                for lbl, rsn in bad.items():
                    skips.append((src, value, name, lbl, rsn))
                for lbl, txt in out.items():
                    if lbl == "interp":
                        continue
                    got = txt.splitlines()
                    for i, line in enumerate(ref):
                        g = got[i] if i < len(got) else "<missing>"
                        if g != line:
                            divergences.append((src, value, name, lbl, line, g))

    print(f"width-matrix --roundtrip: {cases} case-lines, {len(divergences)} divergences, {len(skips)} skips")
    print(f"generated sources: {workdir}")
    if divergences:
        print("\nDIVERGENCES (a same-width roundtrip must be the identity):")
        seen = set()
        for src, value, name, lbl, want, got in divergences:
            key = (name, src, lbl)
            if key in seen:
                continue
            seen.add(key)
            print(f"  {name:<16} {src:>4}  [{lbl:<7}]  interp: {want!r}  got: {got!r}  (value {value})")
        print(f"\n  {len(divergences)} raw, {len(seen)} distinct (site, type, surface)")
    if skips:
        print("\nSKIPS:")
        seen = set()
        for src, value, name, lbl, rsn in skips:
            key = (name, src, lbl)
            if key in seen:
                continue
            seen.add(key)
            print(f"  {name:<16} {src:>4} [{lbl}] {rsn}")
    return 1 if divergences else 0


def narrowing_sweep(workdir, groups):
    """Every site must REFUSE an implicit narrowing. One site per program, so a
    rejection is attributable; `karac check` only, since nothing should build."""
    sites = [s for g in groups for s in g]
    holes, checked = [], 0
    for site in sites:
        for value, dst in NARROWING:
            # The value must arrive through a VARIABLE, not as a literal at the
            # site. A literal is range-checked against the destination directly
            # ("integer literal 300 out of range for 'u8'"), which is a correct
            # but DIFFERENT rejection and never exercises the coercion path this
            # sweep is about. Binding it first is what makes the site decide.
            preamble = f"let nsrc = {value};\n    "
            src = gen(dst, "nsrc", [site], preamble)
            path = workdir / f"n_{site}_{dst}_{value.replace('-', 'neg')}.kara"
            path.write_text(src, encoding="utf-8")
            rc, so, se = run(["karac", "check", str(path)])
            checked += 1
            blob = (se or "") + (so or "")
            if rc == 0:
                holes.append((site, value, dst, "ACCEPTED — no diagnostic"))
            elif not any(k in blob for k in ("narrow", "sign", "out of range", "mix integer types")):
                first = blob.strip().splitlines()[:1]
                holes.append((site, value, dst, f"rejected for another reason: {first}"))

    print(f"width-matrix --narrowing: {checked} site/pair checks, {len(holes)} holes")
    print(f"generated sources: {workdir}")
    if holes:
        print("\nHOLES (the language says narrowing is refused; these did not refuse it):")
        for site, value, dst, why in holes:
            print(f"  {site:<16} {value:>16} -> {dst:<4}  {why}")
    return 1 if holes else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--floats", action="store_true", help="sweep int -> f64/f32 instead of int -> int")
    ap.add_argument("--narrowing", action="store_true", help="assert every site REJECTS a narrowing")
    ap.add_argument("--roundtrip", action="store_true", help="assert a same-width container roundtrip is the identity")
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

    if args.roundtrip:
        return roundtrip_sweep(workdir, args.quick, args.site)
    if args.narrowing:
        return narrowing_sweep(workdir, groups)

    divergences, skips, compiled, cases = [], [], 0, 0
    for dst in (FLOAT_DSTS if args.floats else ("i64", "i32")):
        for src, values in SOURCES:
            if not widens_to(src, dst):
                continue
            vals = values[:1] if args.quick else values
            for value in vals:
                for gi, group in enumerate(groups):
                    if dst in FLOAT_DSTS:
                        group = [g for g in group if g not in FLOAT_NA]
                        if not group:
                            continue
                    name = f"w_{dst}_{src}_{gi}_{value.replace('-', 'neg')}"
                    path = workdir / f"{name}.kara"
                    path.write_text(gen(dst, value, group), encoding="utf-8")
                    out, bad = surfaces(path, workdir)
                    if out is None:
                        # One unusable site must not blank its group. Retry the
                        # members individually so the rest still get swept —
                        # otherwise a single inapplicable construct silently
                        # costs four sites of coverage, which is exactly the
                        # kind of hole this tool exists to close.
                        if len(group) > 1:
                            for one in group:
                                p1 = workdir / f"{name}_{one}.kara"
                                p1.write_text(gen(dst, value, [one]), encoding="utf-8")
                                o1, b1 = surfaces(p1, workdir)
                                if o1 is None:
                                    skips.append((dst, src, value, [one], "interp", b1["interp"]))
                                    continue
                                compiled += 1
                                r1 = o1["interp"].splitlines()
                                cases += len(r1)
                                for lbl, rsn in b1.items():
                                    skips.append((dst, src, value, [one], lbl, rsn))
                                for lbl, txt in o1.items():
                                    if lbl == "interp":
                                        continue
                                    g1 = txt.splitlines()
                                    for i, line in enumerate(r1):
                                        gg = g1[i] if i < len(g1) else "<missing>"
                                        if gg != line:
                                            divergences.append((dst, src, value, lbl, line, gg))
                        else:
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
