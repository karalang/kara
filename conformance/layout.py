#!/usr/bin/env python3
"""design.md §15 "#[repr]: ABI layout" and "extern \"C\"": random
`#[repr(C)]` structs and `#[repr(intN)]` enums against the C compiler.

Each run draws a set of `#[repr(C)]` structs over every type the page says
crosses the C ABI (the integers up to 128 bits, `usize` and `isize`, `bool`,
`char` as `uint32_t`, `f32`, `f64`, a raw pointer, a C-like `#[repr(u8)]` /
`u16` / `u32` / `i32` / `C` enum, an `Array[T, N]` of a scalar, and an
earlier struct nested by value), writes the same declarations as C, and
compiles that C with the platform's C compiler (GCC; `CC` overrides) into a shared library the Kāra program links
through `kara.toml`'s `[link]` table.

The oracle is the C compiler: `sizeof` and `_Alignof` are printed by a C program and
compared with `S.size_of()` and `S.align_of()`. Field offsets are checked by
what crosses: the Kāra program builds each struct from named fields and
hands it to C by pointer and by value (beside scalars, so a struct lands in
registers or memory as the ABI says), and C hashes every field it reads
through its own layout; C returns a struct by value and fills one through a
pointer, and the Kāra program prints every field it reads. A field at the
wrong offset reads the wrong bytes on one side. Scalar calls with up to 14
mixed integer and float arguments, each scalar's identity round trip, and
each enum's round trip as its C integer complete the ABI half.

Lanes: the MIR interpreter (`karac __mir-run`, which calls C through
`dlopen`), `karac build` (the binary run with the library on
`LD_LIBRARY_PATH`), and `karac run` (the JIT). Varargs are not in the
language (design.md §15 names none; `...` in an extern is a parse error),
so none are drawn.

    python3 conformance/layout.py [--seed N] [--structs N]
"""

import argparse
import os
import random
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
KARAC = os.environ.get("KARAC", str(ROOT / "target/release/karac"))
# The platform's C compiler is the ABI's reference. GCC, not clang: clang 18
# (Ubuntu 24.04's) passes an `__int128` that no longer fits the integer
# registers half in `r9` and half on the stack, where GCC and LLVM 19 on put
# it on the stack whole and give `r9` to the next integer argument.
CC = os.environ.get("CC", "gcc")
M64 = (1 << 64) - 1

# kara name, C name, bits, signed
INTS = [("u8", "uint8_t", 8, False), ("i8", "int8_t", 8, True), ("u16", "uint16_t", 16, False),
        ("i16", "int16_t", 16, True), ("u32", "uint32_t", 32, False), ("i32", "int32_t", 32, True),
        ("u64", "uint64_t", 64, False), ("i64", "int64_t", 64, True), ("usize", "size_t", 64, False),
        ("isize", "ptrdiff_t", 64, True), ("u128", "unsigned __int128", 128, False),
        ("i128", "__int128", 128, True)]
FLOATS = [("f32", "float"), ("f64", "double")]
CHARS = "abcxyzQ09é€ж😀𝄞"
ENUM_REPRS = [("u8", "uint8_t", 8, False), ("u16", "uint16_t", 16, False), ("u32", "uint32_t", 32, False),
              ("i32", "int32_t", 32, True), ("C", "int", 32, True)]


class Leaf:
    """One scalar the hash reads: its access path, kind and C type."""

    def __init__(self, path, kind, ctype, info=None):
        self.path, self.kind, self.ctype, self.info = path, kind, ctype, info


class Enum:
    def __init__(self, rng, i):
        self.name = f"E{i}"
        self.repr, self.cint, bits, signed = rng.choice(ENUM_REPRS)
        lo, hi = (-(1 << (bits - 1)), (1 << (bits - 1)) - 1) if signed else (0, (1 << bits) - 1)
        n = rng.randint(1, 5)
        vals = set()
        while len(vals) < n:
            vals.add(rng.choice([lo, hi, 0, 1, rng.randint(lo, hi)]))
        self.values = list(vals)
        rng.shuffle(self.values)

    def kara(self):
        vs = ", ".join(f"V{j} = {v}" for j, v in enumerate(self.values))
        return f"#[repr({self.repr})]\n#[derive(Copy, Clone)]\nenum {self.name} {{ {vs} }}\n"


class Struct:
    def __init__(self, rng, i, earlier, enums):
        self.name = f"S{i}"
        self.fields = []   # (name, kind, data)
        for j in range(rng.randint(1, 7)):
            r = rng.random()
            if r < 0.45:
                self.fields.append((f"f{j}", "int", rng.choice(INTS)))
            elif r < 0.6:
                self.fields.append((f"f{j}", "float", rng.choice(FLOATS)))
            elif r < 0.66:
                self.fields.append((f"f{j}", "bool", None))
            elif r < 0.72:
                self.fields.append((f"f{j}", "char", None))
            elif r < 0.76:
                self.fields.append((f"f{j}", "ptr", None))
            elif r < 0.84 and enums:
                self.fields.append((f"f{j}", "enum", rng.choice(enums)))
            elif r < 0.92:
                el = rng.choice(INTS[:10] + [("f32", "float", 32, None), ("f64", "double", 64, None)])
                self.fields.append((f"f{j}", "array", (el, rng.randint(1, 4))))
            elif earlier:
                self.fields.append((f"f{j}", "struct", rng.choice(earlier[-6:])))
            else:
                self.fields.append((f"f{j}", "int", rng.choice(INTS)))

    def kara(self):
        def ty(kind, d):
            return {"int": lambda: d[0], "float": lambda: d[0], "bool": lambda: "bool", "char": lambda: "char",
                    "ptr": lambda: "*const u8", "enum": lambda: d.name, "struct": lambda: d.name,
                    "array": lambda: f"Array[{d[0][0]}, {d[1]}]"}[kind]()
        fs = ", ".join(f"{n}: {ty(k, d)}" for n, k, d in self.fields)
        return f"#[repr(C)]\n#[derive(Copy, Clone)]\nstruct {self.name} {{ {fs} }}\n"

    def c(self):
        def decl(n, k, d):
            if k in ("int", "float"):
                return f"{d[1]} {n};"
            if k == "bool":
                return f"bool {n};"
            if k == "char":
                return f"uint32_t {n};"
            if k == "ptr":
                return f"const uint8_t* {n};"
            if k == "enum":
                return f"{d.cint} {n};"
            if k == "struct":
                return f"struct {d.name} {n};"
            return f"{d[0][1]} {n}[{d[1]}];"
        return f"struct {self.name} {{ " + " ".join(decl(*f) for f in self.fields) + " };\n"

    def leaves(self, prefix=""):
        out = []
        for n, k, d in self.fields:
            p = f"{prefix}{n}"
            if k == "struct":
                out += d.leaves(p + ".")
            elif k == "array":
                el, cnt = d
                for i in range(cnt):
                    out.append(Leaf(f"{p}[{i}]", "float" if el[0][0] == "f" else "int", el[1], el))
            else:
                out.append(Leaf(p, k, None, d))
        return out


def draw(rng, leaf):
    """A value for `leaf` (Python form)."""
    k, info = leaf.kind, leaf.info
    if k == "int":
        bits, signed = info[2], info[3]
        lo, hi = (-(1 << (bits - 1)), (1 << (bits - 1)) - 1) if signed else (0, (1 << bits) - 1)
        return rng.choice([lo, hi, 0, 1, rng.randint(lo, hi), rng.randint(lo, hi)])
    if k == "float":
        while True:
            q = rng.randint(-4000, 4000)
            if q % 4:
                return q / 4
    if k == "bool":
        return rng.random() < 0.5
    if k == "char":
        return rng.choice(CHARS)
    if k == "ptr":
        return None
    if k == "enum":
        return rng.randrange(len(info.values))
    raise ValueError(k)


def as_int(leaf, v):
    """The integer the hash takes for `leaf`'s value `v`."""
    k = leaf.kind
    if k == "int":
        if leaf.info[2] < 128:
            return v & M64
        u = v & ((1 << 128) - 1)
        return (u & M64) ^ (u >> 64)
    if k == "float":
        return int(v * 4) & M64
    if k == "bool":
        return int(v)
    if k == "char":
        return ord(v)
    if k == "ptr":
        return 1
    if k == "enum":
        return leaf.info.values[v] & M64
    raise ValueError(k)


def hash_of(leaves, vals):
    return sum((i + 1) * as_int(lf, v) for i, (lf, v) in enumerate(zip(leaves, vals))) & M64


def c_leaf_int(lf, expr):
    """C: `expr` (a leaf's lvalue) as the uint64_t the hash takes."""
    k = lf.kind
    if k == "int":
        if lf.info[2] == 128:
            return f"((uint64_t)(unsigned __int128){expr} ^ (uint64_t)(((unsigned __int128){expr}) >> 64))"
        return f"(uint64_t)(int64_t){expr}" if lf.info[3] else f"(uint64_t){expr}"
    if k == "float":
        return f"(uint64_t)(int64_t)({expr} * 4)"
    if k == "bool":
        return f"(uint64_t){expr}"
    if k == "char":
        return f"(uint64_t){expr}"
    if k == "ptr":
        return f"(uint64_t)({expr} == 0 ? 1 : 99)"
    if k == "enum":
        return f"(uint64_t)(int64_t){expr}"
    raise ValueError(k)


def c_literal(lf, v):
    k = lf.kind
    if k == "int":
        bits, signed = lf.info[2], lf.info[3]
        if bits == 128:
            u = v & ((1 << 128) - 1)
            return f"(({lf.info[1]})(((unsigned __int128){u >> 64}ULL << 64) | {u & M64}ULL))"
        if signed and v == -(1 << (bits - 1)):
            return f"({lf.info[1]})({v + 1}LL - 1)"
        return f"({lf.info[1]}){v}{'LL' if signed else 'ULL'}"
    if k == "float":
        return repr(v)
    if k == "bool":
        return "1" if v else "0"
    if k == "char":
        return str(ord(v))
    if k == "ptr":
        return "0"
    if k == "enum":
        ev = lf.info.values[v]
        return f"({lf.info.cint})({ev + 1}LL - 1)" if ev < 0 else f"({lf.info.cint}){ev}LL"
    raise ValueError(k)


def kara_literal(lf, v):
    k = lf.kind
    if k == "int":
        return str(v)
    if k == "float":
        return repr(v)
    if k == "bool":
        return "true" if v else "false"
    if k == "char":
        return "'" + v + "'"
    if k == "ptr":
        return "ptr.null()"
    if k == "enum":
        return f"{lf.info.name}.V{v}"
    raise ValueError(k)


def kara_value(st, vals_iter, leaves_iter):
    """A Kāra struct literal for `st` from the leaf values, in leaf order."""
    parts = []
    for n, k, d in st.fields:
        if k == "struct":
            parts.append(f"{n}: {kara_value(d, vals_iter, leaves_iter)}")
        elif k == "array":
            items = [kara_literal(next(leaves_iter), next(vals_iter)) for _ in range(d[1])]
            parts.append(f"{n}: [{', '.join(items)}]")
        else:
            parts.append(f"{n}: {kara_literal(next(leaves_iter), next(vals_iter))}")
    return f"{st.name} {{ {', '.join(parts)} }}"


def shown(lf, v):
    """What the Kāra program prints for a leaf's value."""
    if lf.kind == "bool":
        return "true" if v else "false"
    if lf.kind == "ptr":
        return "null"
    if lf.kind == "enum":
        return str(lf.info.values[v])
    return str(v)


def kara_show(lf, expr):
    if lf.kind == "ptr":
        return f'{{if {expr}.is_null() {{ "null" }} else {{ "set" }}}}'
    if lf.kind == "enum":
        return f"{{{expr}.discriminant()}}"
    return f"{{{expr}}}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--structs", type=int, default=40)
    ap.add_argument("--scalar-calls", type=int, default=40)
    a = ap.parse_args()
    rng = random.Random(a.seed)
    lanes = os.environ.get("CONF_LANES", "interp,build,run").split(",")

    enums = [Enum(rng, i) for i in range(6)]
    structs = []
    for i in range(a.structs):
        structs.append(Struct(rng, i, structs, enums))

    kara, c, want = [], [], []
    kara += [e.kara() for e in enums]
    kara += [s.kara() for s in structs]
    c.append("#include <stdint.h>\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdio.h>\n")
    c += [s.c() for s in structs]
    externs, body = [], []

    def emit(line, expected, label):
        body.append(line)
        want.append((label, expected))

    for s in structs:
        leaves = s.leaves()
        va = [draw(rng, lf) for lf in leaves]   # Kāra builds this one
        vb = [draw(rng, lf) for lf in leaves]   # C returns this one
        vc = [draw(rng, lf) for lf in leaves]   # C writes this one through a pointer
        n = s.name
        hsum = " + ".join(f"{i + 1}ULL * {c_leaf_int(lf, 'p->' + lf.path)}" for i, lf in enumerate(leaves))
        hsum_v = hsum.replace("p->", "s.")
        c.append(f"uint64_t h_{n}(const struct {n}* p) {{ return {hsum}; }}\n")
        c.append(f"uint64_t v_{n}(struct {n} s) {{ return {hsum_v}; }}\n")
        c.append(f"uint64_t sv_{n}(int32_t a, struct {n} s, double b, int8_t d) {{ return ({hsum_v}) * 3ULL "
                 f"+ (uint64_t)(int64_t)a * 5ULL + (uint64_t)(int64_t)(b * 4) * 7ULL + (uint64_t)(int64_t)d * 11ULL; }}\n")
        assigns_b = " ".join(f"s.{lf.path} = {c_literal(lf, v)};" for lf, v in zip(leaves, vb))
        c.append(f"struct {n} r_{n}(void) {{ struct {n} s; __builtin_memset(&s, 0xA5, sizeof s); {assigns_b} return s; }}\n")
        assigns_c = " ".join(f"p->{lf.path} = {c_literal(lf, v)};" for lf, v in zip(leaves, vc))
        c.append(f"void w_{n}(struct {n}* p) {{ {assigns_c} }}\n")
        externs += [f"fn h_{n}(p: *const {n}) -> u64;", f"fn v_{n}(s: {n}) -> u64;",
                    f"fn sv_{n}(a: i32, s: {n}, b: f64, d: i8) -> u64;", f"fn r_{n}() -> {n};",
                    f"fn w_{n}(p: *mut {n});"]
        lit = kara_value(s, iter(va), iter(leaves))
        body.append(f"    let a_{n} = {lit};")
        emit(f"    println(f\"{n}.size_of {{{n}.size_of()}}\");", None, f"{n} size")
        emit(f"    println(f\"{n}.align_of {{{n}.align_of()}}\");", None, f"{n} align")
        h = hash_of(leaves, va)
        emit(f"    println(h_{n}(ptr.const(a_{n})));", str(h), f"{n} to C by pointer")
        emit(f"    println(v_{n}(a_{n}));", str(h), f"{n} to C by value")
        ai, bf, di = rng.randint(-1000, 1000), rng.randint(-400, 400) / 4 + 0.25, rng.randint(-128, 127)
        hs = (h * 3 + (ai & M64) * 5 + (int(bf * 4) & M64) * 7 + (di & M64) * 11) & M64
        emit(f"    println(sv_{n}({ai}, a_{n}, {bf!r}, {di}));", str(hs), f"{n} to C by value among scalars")
        shows = " ".join(kara_show(lf, f"r_{n}_.{lf.path}") for lf in leaves)
        body.append(f"    let r_{n}_ = r_{n}();")
        emit(f"    println(f\"{shows}\");", " ".join(shown(lf, v) for lf, v in zip(leaves, vb)),
             f"{n} returned by value from C")
        emit(f"    println(h_{n}(ptr.const(r_{n}_)));", str(hash_of(leaves, vb)), f"{n} from C, back to C")
        body.append(f"    let mut m_{n}_ = r_{n}();")
        body.append(f"    w_{n}(ptr.mut(m_{n}_));")
        shows = " ".join(kara_show(lf, f"m_{n}_.{lf.path}") for lf in leaves)
        emit(f"    println(f\"{shows}\");", " ".join(shown(lf, v) for lf, v in zip(leaves, vc)),
             f"{n} written by C through a pointer")

    # Scalars: identity round trips and mixed-argument calls.
    scalars = [Leaf("x", "int", it[1], it) for it in INTS] + [Leaf("x", "float", f[1], f) for f in FLOATS] + \
              [Leaf("x", "bool", "bool"), Leaf("x", "char", "uint32_t")]

    def kty(lf):
        return {"int": lambda: lf.info[0], "float": lambda: lf.info[0], "bool": lambda: "bool",
                "char": lambda: "char"}[lf.kind]()

    def cty(lf):
        return {"int": lambda: lf.info[1], "float": lambda: lf.info[1], "bool": lambda: "bool",
                "char": lambda: "uint32_t"}[lf.kind]()

    for i, lf in enumerate(scalars):
        c.append(f"{cty(lf)} id{i}({cty(lf)} x) {{ return x; }}\n")
        externs.append(f"fn id{i}(x: {kty(lf)}) -> {kty(lf)};")
        for _ in range(3):
            v = draw(rng, lf)
            emit(f"    println(id{i}({kara_literal(lf, v)}));", shown(lf, v), f"id {kty(lf)} {v!r}")
    for e in enums:
        n = len(e.values)
        cases = " ".join(f"case {e.values[j]}: return {e.values[(j + 1) % n]};" for j in range(n)) \
            if all(abs(v) < (1 << 31) for v in e.values) else ""
        c.append(f"{e.cint} next_{e.name}({e.cint} x) {{ switch ((int64_t)x) {{ "
                 + " ".join(f"case {v}LL: return ({e.cint})({e.values[(j + 1) % n]}LL);"
                            for j, v in enumerate(e.values)) + " } return x; }\n")
        del cases
        externs.append(f"fn next_{e.name}(x: {e.name}) -> {e.name};")
        for j in range(n):
            emit(f"    println(next_{e.name}({e.name}.V{j}).discriminant());", str(e.values[(j + 1) % n]),
                 f"{e.name} repr({e.repr}) V{j} through C")
        emit(f"    println({e.name}.size_of());", None, f"{e.name} size")
    for k in range(a.scalar_calls):
        args = [rng.choice(scalars) for _ in range(rng.randint(1, 14))]
        vals = [draw(rng, lf) for lf in args]
        params_c = ", ".join(f"{cty(lf)} a{j}" for j, lf in enumerate(args))
        params_k = ", ".join(f"a{j}: {kty(lf)}" for j, lf in enumerate(args))
        hsum = " + ".join(f"{j + 1}ULL * {c_leaf_int(lf, f'a{j}')}" for j, lf in enumerate(args))
        c.append(f"uint64_t mix{k}({params_c}) {{ return {hsum}; }}\n")
        externs.append(f"fn mix{k}({params_k}) -> u64;")
        emit(f"    println(mix{k}({', '.join(kara_literal(lf, v) for lf, v in zip(args, vals))}));",
             str(hash_of(args, vals)), f"mix{k}({', '.join(kty(lf) for lf in args)})")

    src = "".join(kara) + "/// # Safety\n/// The C side is generated with these declarations.\nunsafe extern \"C\" {\n" + \
        "".join(f"    {x}\n" for x in externs) + "}\n\nfn main() {\n" + "\n".join(body) + "\n}\n"

    # clang's sizes, alignments and offsets.
    layout_c = "".join(c[:1 + len(structs)]) + "int main(void) {\n" + "".join(
        f'    printf("{s.name}.size_of %zu\\n{s.name}.align_of %zu\\n", sizeof(struct {s.name}), '
        f'_Alignof(struct {s.name}));\n' for s in structs) + "".join(
        f'    printf("%zu\\n", sizeof({e.cint}));\n' for e in enums) + "    return 0;\n}\n"

    fails, passed = [], 0
    with tempfile.TemporaryDirectory(prefix="layout-conf-") as d:
        d = Path(d)
        (d / "layout.c").write_text(layout_c)
        subprocess.run([CC, "-std=c11", "-O1", "-o", d / "layout", d / "layout.c"], check=True)
        clang_lines = subprocess.run([d / "layout"], capture_output=True, text=True, check=True).stdout.split("\n")
        sizes = dict(line.rsplit(" ", 1) for line in clang_lines[:2 * len(structs)])
        enum_sizes = clang_lines[2 * len(structs):2 * len(structs) + len(enums)]
        (d / "lib.c").write_text("".join(c))
        subprocess.run([CC, "-std=c11", "-O1", "-shared", "-fPIC", "-o", d / "libconf.so", d / "lib.c"],
                       check=True)
        (d / "src").mkdir()
        (d / "src/main.kara").write_text(src)
        (d / "kara.toml").write_text(f'[package]\nname = "conf"\nversion = "0.1.0"\n\n[link]\nlibs = ["conf"]\n'
                                     f'search-paths = ["{d}"]\n')
        if os.environ.get("LAYOUT_KEEP"):
            keep = Path(os.environ["LAYOUT_KEEP"])
            keep.mkdir(parents=True, exist_ok=True)
            for f in ("src/main.kara", "lib.c", "layout.c", "kara.toml"):
                (keep / Path(f).name).write_text((d / f).read_text())
        expected = []
        ei = 0
        for label, w in want:
            if w is None and label.endswith(" size") and label.startswith("E"):
                w = enum_sizes[ei]
                ei += 1
            elif w is None:
                key = label.split(" ")[0] + (".size_of" if label.endswith("size") else ".align_of")
                w = f"{key} {sizes[key]}"
            expected.append((label, w))
        chk = subprocess.run([KARAC, "check", "src/main.kara"], cwd=d, capture_output=True, text=True)
        if chk.returncode != 0:
            print("FAIL check refused the program:\n" + (chk.stdout + chk.stderr)[-3000:])
            sys.exit(1)
        env = dict(os.environ, LD_LIBRARY_PATH=str(d))
        for lane in lanes:
            if lane == "interp":
                r = subprocess.run([KARAC, "__mir-run", "src/main.kara"], cwd=d, capture_output=True, text=True,
                                   timeout=1800)
            elif lane == "run":
                r = subprocess.run([KARAC, "run", "src/main.kara"], cwd=d, capture_output=True, text=True,
                                   timeout=1800)
            else:
                b = subprocess.run([KARAC, "build"], cwd=d, capture_output=True, text=True, timeout=1800)
                exe = d / "dist/native/conf"
                if b.returncode != 0 or not exe.exists():
                    fails.append(f"[build] build failed: {(b.stdout + b.stderr)[-2000:]}")
                    continue
                r = subprocess.run([exe], cwd=d, capture_output=True, text=True, timeout=600, env=env)
            got = r.stdout.splitlines()
            if r.returncode != 0:
                fails.append(f"[{lane}] exit {r.returncode}: {r.stderr[-1500:]}")
            if len(got) != len(expected):
                fails.append(f"[{lane}] {len(got)} lines, want {len(expected)}")
            for (label, w), g in zip(expected, got):
                if g == w:
                    passed += 1
                else:
                    fails.append(f"[{lane}] {label}: got {g!r}, want {w!r}")
    for f in fails[:60]:
        print("FAIL", f)
    if len(fails) > 60:
        print(f"... {len(fails) - 60} more")
    print(f"layout: {passed} passed, {len(fails)} failed")
    sys.exit(1 if fails else (2 if passed == 0 else 0))


if __name__ == "__main__":
    main()
