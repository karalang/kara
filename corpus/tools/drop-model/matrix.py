#!/usr/bin/env python3
"""Drop matrix: value shapes x positions, each a small program whose expected
stdout comes from the model (model.py), never from a compiler.

    python3 matrix.py OUT_DIR [--legacy PATH_TO_KARAC]

Writes OUT_DIR/<shape>__<position>/{main.kara, expected.out, meta.toml}.
With --legacy it also runs each program on `karac run --interp`, writes
legacy.out, and buckets the pair: SAME, ORDER (same lines, different order,
the class-c shape), DIFF (different lines: a legacy bug or a model bug, to be
read by hand) or LEGACY-REJECT.
"""
import os
import subprocess
import sys
from collections import Counter

from model import *  # noqa: F401,F403

# ── shapes ──


def idx(base, k):
    return Lit(base.n + 10 * k) if isinstance(base, Lit) else Add(base, Lit(10 * k))


P_DEF = StructDef("P", [("a", "R"), ("b", "R"), ("c", "R")])
W_DEF = StructDef("W", [("k", "i64"), ("a", "R"), ("b", "R")], "w{k}")
Q_DEF = StructDef("Q", [("w", "W"), ("t", "(R, R)")])
E_DEF = EnumDef("E", [("Two", ["R", "R"]), ("One", ["R"])])


class Shape:
    def __init__(self, name, ty, make, structs=(), enums=(), part_pat=None, part_place=None,
                 whole_drop_body=False):
        self.name, self.ty, self.make = name, ty, make
        self.structs, self.enums = list(structs), list(enums)
        self.part_pat = part_pat  # pattern moving ONE part out, binding `p`
        self.part_place = part_place  # fn(place Expr) -> Expr for a partial move
        self.whole_drop_body = whole_drop_body


SHAPES = [
    Shape("r", "R", lambda b: New(idx(b, 0))),
    Shape("struct", "P", lambda b: StructLit("P", [("a", New(idx(b, 0))), ("b", New(idx(b, 1))),
                                                  ("c", New(idx(b, 2)))]),
          [P_DEF], part_pat=PStruct("P", [("b", PBind("p"))], rest=True),
          part_place=lambda x: Field(x, "b")),
    Shape("struct_written_order", "P", lambda b: StructLit("P", [("c", New(idx(b, 2))), ("a", New(idx(b, 0))),
                                                                ("b", New(idx(b, 1)))]), [P_DEF]),
    Shape("dropbody", "W", lambda b: StructLit("W", [("k", idx(b, 0)), ("a", New(idx(b, 1))),
                                                    ("b", New(idx(b, 2)))]), [W_DEF], whole_drop_body=True),
    Shape("tuple", "(R, R, R)", lambda b: TupleLit([New(idx(b, 0)), New(idx(b, 1)), New(idx(b, 2))]),
          part_pat=PTuple([PWild(), PBind("p"), PWild()]), part_place=lambda x: TupIdx(x, 1)),
    Shape("vec", "Vec[R]", lambda b: VecLit([New(idx(b, 0)), New(idx(b, 1)), New(idx(b, 2))])),
    Shape("option", "Option[R]", lambda b: SomeE(New(idx(b, 0))), part_pat=PSome(PBind("p"))),
    Shape("enum", "E", lambda b: EnumLit("E", "Two", [New(idx(b, 0)), New(idx(b, 1))]), enums=[E_DEF],
          part_pat=PEnum("E", "Two", [PBind("p"), PWild()])),
    Shape("nested", "Q", lambda b: StructLit("Q", [
        ("w", StructLit("W", [("k", idx(b, 0)), ("a", New(idx(b, 1))), ("b", New(idx(b, 2)))])),
        ("t", TupleLit([New(idx(b, 3)), New(idx(b, 4))]))]),
        [W_DEF, Q_DEF], part_place=lambda x: Field(x, "t")),
]


def pr(*parts):
    return Print(list(parts))


def main(*body):
    return FnDef("main", [], None, list(body))


# ── positions: shape -> list of (suffix, Program) ──

def positions(s: Shape):
    T = s.ty
    mk = s.make
    defs = dict(structs=s.structs, enums=s.enums)
    peek = FnDef("peek", [Param("x", T, "ref")], None, [pr("peek")])
    consume = FnDef("consume", [Param("x", T)], None, [Let(PBind("l"), New(Lit(9))), pr("in")])
    passf = FnDef("pass", [Param("x", T)], T, [pr("pass")], Var("x"))
    make = FnDef("make", [], T, [], mk(Lit(1)))
    out = []

    def add(suffix, fns, **kw):
        out.append((suffix, Program(fns, s.structs + kw.get("extra_structs", []), s.enums)))

    add("local", [main(Let(PBind("x"), mk(Lit(1))), pr("end"))])
    add("two_locals", [main(Let(PBind("x"), mk(Lit(1))), Let(PBind("y"), mk(Lit(2))), pr("end"))])
    add("nested_block", [main(Let(PBind("x"), mk(Lit(1))),
                              Block([Let(PBind("y"), mk(Lit(2))), pr("inner")]), pr("outer"))])
    add("stmt_temp_ref_arg", [peek, main(ExprStmt(Call("peek", [mk(Lit(1))])), pr("next"))])
    add("by_value_arg", [consume, main(Let(PBind("x"), mk(Lit(1))), ExprStmt(Call("consume", [Var("x")])),
                                       pr("after"))])
    add("temp_by_value_arg", [consume, main(ExprStmt(Call("consume", [mk(Lit(1))])), pr("after"))])
    add("returned", [passf, main(Let(PBind("x"), mk(Lit(1))), Let(PBind("y"), Call("pass", [Var("x")])),
                                 pr("after"))])
    add("discarded_return", [make, main(ExprStmt(Call("make", [])), pr("after"))])
    for c in (True, False):
        add(f"cond_move_{str(c).lower()}", [consume, FnDef("f", [Param("c", "bool")], None, [
            Let(PBind("x"), mk(Lit(1))), If(Var("c"), [ExprStmt(Call("consume", [Var("x")]))]), pr("end")]),
            main(ExprStmt(Call("f", [BoolLit(c)])), pr("after"))])
    add("loop", [main(For("i", 0, 2, [Let(PBind("x"), mk(Var("i"))), pr("it")]), pr("done"))])
    add("loop_break", [main(For("i", 0, 3, [Let(PBind("x"), mk(Var("i"))), pr("it"), Break()]), pr("done"))])
    add("loop_continue", [main(For("i", 0, 2, [Let(PBind("x"), mk(Var("i"))), pr("it"), Continue()]),
                               pr("done"))])
    add("loop_outer_moved_reinit", [consume, main(Let(PBind("x"), mk(Lit(5)), mut=True),
                                                  For("i", 0, 2, [ExprStmt(Call("consume", [Var("x")])),
                                                                  Assign(Var("x"), mk(Var("i")))]),
                                                  pr("done"))])
    add("reassign", [main(Let(PBind("x"), mk(Lit(1)), mut=True), pr("a"), Assign(Var("x"), mk(Lit(2))),
                          pr("b"))])
    add("reassign_after_move", [consume, main(Let(PBind("x"), mk(Lit(1)), mut=True),
                                              ExprStmt(Call("consume", [Var("x")])),
                                              Assign(Var("x"), mk(Lit(2))), pr("b"))])
    add("shadowed", [main(Let(PBind("x"), mk(Lit(1))), Let(PBind("x"), mk(Lit(2))), pr("end"))])
    add("defer_between", [main(Let(PBind("x"), mk(Lit(1))), Defer([pr("defer")]),
                               Let(PBind("y"), mk(Lit(2))), pr("body"))])
    add("early_return", [FnDef("f", [Param("c", "bool")], None, [
        Let(PBind("x"), mk(Lit(1))), If(Var("c"), [Let(PBind("y"), mk(Lit(2))), Return()]), pr("not taken")]),
        main(ExprStmt(Call("f", [BoolLit(True)])), pr("after"))])
    add("errdefer", [FnDef("f", [Param("fail", "bool")], "Result[i64, i64]", [
        Let(PBind("x"), mk(Lit(1))), Errdefer([pr("rollback")]), Let(PBind("y"), mk(Lit(2))),
        If(Var("fail"), [Return(ErrE(Lit(1)))])], OkE(Lit(0))),
        main(Let(PWild(), Call("f", [BoolLit(False)])), pr("--"), Let(PWild(), Call("f", [BoolLit(True)])))])
    add("let_underscore", [main(Let(PBind("x"), mk(Lit(1))), Let(PWild(), Var("x")), pr("end"))])
    add("let_underscore_temp", [make, main(Let(PWild(), Call("make", [])), pr("end"))])
    add("match_whole_ref", [main(Let(PBind("x"), mk(Lit(1))),
                                 Match(Var("x"), [(PBind("v", ref=True), [pr("arm")])]), pr("end"))])
    add("match_temp_wild", [make, main(Match(Call("make", []), [(PWild(), [pr("arm")])]), pr("end"))])
    add("stored_in_struct", [main(Let(PBind("x"), mk(Lit(1))),
                                  Let(PBind("h"), StructLit("H", [("z", New(Lit(1))), ("s", Var("x"))])),
                                  pr("end"))], extra_structs=[StructDef("H", [("s", T), ("z", "R")])])
    add("pushed", [main(Let(PBind("v"), VecLit([]), mut=True, ty=f"Vec[{T}]"),
                        Push(Var("v"), mk(Lit(1))), Push(Var("v"), mk(Lit(2))), pr("end"))])
    add("panic", [main(Let(PBind("x"), mk(Lit(1))), Defer([pr("defer")]), PanicStmt())])
    if s.part_pat is not None:
        add("match_part_move", [main(Let(PBind("x"), mk(Lit(1))),
                                     Match(Var("x"), [(s.part_pat, [pr("arm")]), (PWild(), [pr("other")])]),
                                     pr("end"))])
        add("match_temp_part_move", [make, main(Match(Call("make", []), [(s.part_pat, [pr("arm")]),
                                                                         (PWild(), [pr("other")])]),
                                                pr("end"))])
    if s.part_place is not None:
        add("partial_move", [main(Let(PBind("x"), mk(Lit(1))), Let(PBind("p"), s.part_place(Var("x"))),
                                  pr("mid"))])
        add("partial_move_restore", [main(Let(PBind("x"), mk(Lit(1)), mut=True),
                                          Let(PBind("p"), s.part_place(Var("x"))), pr("mid"),
                                          Assign(s.part_place(Var("x")), Var("p")), pr("restored"))])
    return out


def all_programs():
    for s in SHAPES:
        for suffix, prog in positions(s):
            yield f"{s.name}__{suffix}", prog


def run_legacy(karac, path):
    r = subprocess.run([karac, "run", "--interp", path], capture_output=True, text=True, timeout=60)
    return r.stdout.splitlines(), r.returncode, r.stderr


def main_cli(argv):
    out_dir = argv[1]
    karac = argv[argv.index("--legacy") + 1] if "--legacy" in argv else None
    buckets = Counter()
    for name, prog in all_programs():
        try:
            want, code = run(prog)
        except ModelError as e:
            print(f"MODEL-REJECT {name}: {e}")
            buckets["MODEL-REJECT"] += 1
            continue
        d = os.path.join(out_dir, name)
        os.makedirs(d, exist_ok=True)
        src = os.path.join(d, "main.kara")
        open(src, "w").write(emit(prog))
        open(os.path.join(d, "expected.out"), "w").write("".join(l + "\n" for l in want))
        open(os.path.join(d, "meta.toml"), "w").write(
            f'source = "corpus/tools/drop-model/matrix.py::{name}"\nexpect = "{"panic:101" if code else "stdout"}"\n'
            f'exit = {code}\ntags = ["drop"]\nexpected_from = "spec"\n')
        if not karac:
            continue
        got, rc, err = run_legacy(karac, src)
        open(os.path.join(d, "legacy.out"), "w").write("".join(l + "\n" for l in got))
        with open(os.path.join(d, "meta.toml"), "a") as mt:
            mt.write(f"legacy_exit = {rc}\n")
        if rc not in (0, 101):
            b = "LEGACY-REJECT"
        elif got == want and rc == code:
            b = "SAME"
        elif Counter(got) == Counter(want):
            b = "ORDER"
        else:
            b = "DIFF"
        buckets[b] += 1
        if b in ("DIFF", "LEGACY-REJECT"):
            print(f"{b} {name}\n   model : {' '.join(want)} (exit {code})\n   legacy: {' '.join(got)} (exit {rc})"
                  + (f"\n   stderr: {err.strip().splitlines()[0][:200]}" if err.strip() and b == "LEGACY-REJECT" else ""))
    total = sum(buckets.values())
    print(f"programs: {total}  " + "  ".join(f"{k}={v}" for k, v in sorted(buckets.items())))


if __name__ == "__main__":
    main_cli(sys.argv)
