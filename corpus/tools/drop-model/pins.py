#!/usr/bin/env python3
"""The drop pins of review/core-pins/, encoded for the model.

check_pins() compares the model's output with each pin's hand-derived
.expected, and the error pins with a ModelError. That is the cross-check
between two independent derivations of the same rules.
"""
from model import *  # noqa: F401,F403

P_DEF = StructDef("P", [("a", "R"), ("b", "R"), ("c", "R")])
W_DEF = StructDef("W", [("a", "R"), ("b", "R")], "dW")
E_DEF = EnumDef("E", [("Two", ["R", "R"]), ("One", ["R"])])


def R(n):
    return New(Lit(n))


def pr(*parts):
    return Print(list(parts))


def main(*body):
    return FnDef("main", [], None, list(body))


TAKE = FnDef("take", [Param("r", "R")], None, [pr("took", Field(Var("r"), "id"))])

PINS = {
    "drop_scope": Program([main(
        Let(PBind("a"), R(1)), Let(PBind("b"), R(2)),
        pr("use", Field(Var("a"), "id")), pr("end"))]),
    "drop_defer": Program([main(
        Let(PBind("a"), R(1)), Defer([pr("defer")]), Let(PBind("b"), R(2)), pr("body"))]),
    "drop_nested": Program([main(
        Let(PBind("a"), R(1)),
        Block([Let(PBind("b"), R(2)), pr("inner")]),
        pr("outer"))]),
    "drop_temp_stmt": Program([
        FnDef("make", [Param("id", "i64")], "R", [], New(Var("id"))),
        main(pr(Field(Call("make", [Lit(1)]), "id")), pr("next"))]),
    "drop_match_scrutinee": Program([
        FnDef("make", [Param("id", "i64")], "R", [], New(Var("id"))),
        main(Match(Call("make", [Lit(1)]),
                   [(PStruct("R", [("id", PBind("id"))]), [pr("arm", Var("id"))])]),
             pr("after"))]),
    "drop_assign": Program([main(
        Let(PBind("x"), R(1), mut=True), pr("a"), Assign(Var("x"), R(2)), pr("b"))]),
    "drop_loop": Program([main(
        For("i", 0, 2, [Let(PBind("r"), New(Var("i"))), pr("it", Var("i"))]),
        pr("done"))]),
    "drop_early_return": Program([
        FnDef("f", [Param("c", "bool")], None, [
            Let(PBind("a"), R(1)),
            If(Var("c"), [Let(PBind("b"), R(2)), Return()]),
            pr("not taken")]),
        main(ExprStmt(Call("f", [BoolLit(True)])), pr("after"))]),
    "drop_callee_owns": Program([
        FnDef("take", [Param("r", "R")], None, [
            Let(PBind("x"), R(9)), pr("in", Field(Var("r"), "id"))]),
        main(Let(PBind("a"), R(1)), ExprStmt(Call("take", [Var("a")])), pr("after"))]),
    "drop_callee_returns": Program([
        FnDef("pass", [Param("r", "R")], "R", [], Var("r")),
        main(Let(PBind("a"), R(1)), Let(PBind("b"), Call("pass", [Var("a")])), pr("after"))]),
    "drop_conditional_move": Program([
        TAKE,
        FnDef("f", [Param("c", "bool")], None, [
            Let(PBind("a"), R(1)),
            If(Var("c"), [ExprStmt(Call("take", [Var("a")]))]),
            pr("end")]),
        main(ExprStmt(Call("f", [BoolLit(True)])), ExprStmt(Call("f", [BoolLit(False)])))]),
    "drop_aggregates": Program([main(
        Let(PBind("v"), VecLit([R(1), R(2), R(3)]), ty="Vec[R]"),
        Let(PBind("t"), TupleLit([R(4), R(5), R(6)])),
        Let(PBind("p"), StructLit("P", [("a", R(7)), ("b", R(8)), ("c", R(9))])),
        Let(PBind("e"), EnumLit("E", "Two", [R(10), R(11)])),
        pr("end"))], [P_DEF], [E_DEF]),
    "drop_user_body_first": Program([main(
        Let(PBind("w"), StructLit("W", [("a", R(1)), ("b", R(2))])), pr("end"))], [W_DEF]),
    "drop_partial_move": Program([main(
        Let(PBind("p"), StructLit("P", [("a", R(7)), ("b", R(8)), ("c", R(9))])),
        Let(PBind("x"), Field(Var("p"), "b")),
        pr("mid"))], [P_DEF]),
    "drop_shadowing": Program([main(
        Let(PBind("x"), R(1)), Let(PBind("x"), R(2)), pr("end", Field(Var("x"), "id")))]),
    "drop_underscore": Program([
        FnDef("pair", [], "(R, R)", [], TupleLit([R(1), R(2)])),
        main(Let(PBind("x"), R(9)), Let(PWild(), Var("x")),
             Let(PTuple([PBind("a"), PWild()]), Call("pair", [])),
             pr("mid", Field(Var("a"), "id")), pr("x", Field(Var("x"), "id")))]),
    "ok_match_binding_modes": Program([
        FnDef("show", [Param("r", "R", "ref")], None, [pr("show", Field(Var("r"), "id"))]),
        FnDef("eat", [Param("r", "R")], None, [pr("eat", Field(Var("r"), "id"))]),
        main(
            Let(PBind("a"), SomeE(R(1)), ty="Option[R]"),
            Match(Var("a"), [(PSome(PBind("r")), [ExprStmt(Call("show", [Var("r")]))]),
                             (PNone(), [pr("none")])]),
            pr("after move-mode match"),
            Let(PBind("b"), SomeE(R(2)), ty="Option[R]"),
            Match(Var("b"), [(PSome(PBind("r", ref=True)), [ExprStmt(Call("show", [Var("r")]))]),
                             (PNone(), [pr("none")])]),
            pr("after ref-mode match"),
            Let(PBind("c"), SomeE(R(3)), ty="Option[R]"),
            Match(Var("c"), [(PSome(PBind("r")), [ExprStmt(Call("eat", [Var("r")]))]),
                             (PNone(), [pr("none")])]),
            pr("end"))]),
    "errdefer_only_on_error": Program([
        FnDef("f", [Param("fail", "bool")], "Result[i64, String]", [
            Errdefer([pr("rollback")]), Defer([pr("defer")]),
            If(Var("fail"), [Return(ErrE(StrLit("x")))])], OkE(Lit(1))),
        main(Let(PWild(), Call("f", [BoolLit(False)])), pr("--"),
             Let(PWild(), Call("f", [BoolLit(True)])))]),
    "ok_reinit": Program([
        FnDef("take", [Param("s", "String")], None, [pr(Var("s"))]),
        main(Let(PBind("s"), StrLit("a"), mut=True), ExprStmt(Call("take", [Var("s")])),
             Assign(Var("s"), StrLit("b")), pr(Var("s")))]),
    "panic_runs_no_drops": Program([main(
        Let(PBind("a"), R(1)), Defer([pr("defer")]), PanicStmt())]),
    # error pins: the model must refuse these on the executed path
    "err_use_after_move": Program([
        FnDef("take", [Param("s", "String")], None, [pr(Var("s"))]),
        main(Let(PBind("s"), StrLit("hi")), ExprStmt(Call("take", [Var("s")])), pr(Var("s")))]),
    "err_maybe_moved": Program([
        FnDef("take", [Param("s", "String")], None, [pr(Var("s"))]),
        FnDef("f", [Param("c", "bool")], None, [
            Let(PBind("s"), StrLit("hi")),
            If(Var("c"), [ExprStmt(Call("take", [Var("s")]))]), pr(Var("s"))]),
        main(ExprStmt(Call("f", [BoolLit(True)])))]),
    "err_moved_in_loop": Program([
        FnDef("take", [Param("s", "String")], None, [pr(Var("s"))]),
        main(Let(PBind("s"), StrLit("hi")), For("i", 0, 2, [ExprStmt(Call("take", [Var("s")]))]))]),
    "err_move_out_of_ref": Program([
        FnDef("name_of", [Param("u", "User", "ref")], "String", [], Field(Var("u"), "name")),
        main(Let(PBind("u"), StructLit("User", [("name", StrLit("x"))])),
             pr(Call("name_of", [Var("u")])))], [StructDef("User", [("name", "String")])]),
    "err_partial_move_drop_type": Program([main(
        Let(PBind("w"), StructLit("W", [("a", StrLit("a"))])),
        Let(PBind("x"), Field(Var("w"), "a")), pr(Var("x")))],
        [StructDef("W", [("a", "String")], "dW")]),
    "err_partial_then_whole": Program([
        FnDef("show", [Param("p", "P", "ref")], None, [pr(Field(Var("p"), "a"))]),
        main(Let(PBind("p"), StructLit("P", [("a", StrLit("a")), ("b", StrLit("b"))])),
             Let(PBind("x"), Field(Var("p"), "b")),
             ExprStmt(Call("show", [Var("p")])), pr(Var("x")))],
        [StructDef("P", [("a", "String"), ("b", "String")])]),
}


def expected_lines(name, pin_dir):
    import os
    if name.startswith("err_"):
        return "ERROR", None
    if name == "panic_runs_no_drops":
        return [], 101
    corpus = os.path.join(pin_dir, name, "expected.out")  # corpus/core/<name>/expected.out
    path = corpus if os.path.exists(corpus) else os.path.join(pin_dir, f"{name}.expected")
    return open(path).read().splitlines(), 0


def check_pins(pin_dir):
    bad = 0
    for name, prog in PINS.items():
        want, want_exit = expected_lines(name, pin_dir)
        try:
            got, code = run(prog)
        except ModelError as e:
            got, code = "ERROR", None
            msg = str(e)
        else:
            msg = ""
        ok = (got == want and code == want_exit) if want != "ERROR" else got == "ERROR"
        print(f"{'PASS' if ok else 'FAIL'} {name}" + (f"  ({msg})" if msg else ""))
        if not ok:
            bad += 1
            print("   want:", want, want_exit)
            print("   got: ", got, code)
    print(f"{len(PINS) - bad}/{len(PINS)} pins agree")
    return bad


if __name__ == "__main__":
    import sys
    sys.exit(1 if check_pins(sys.argv[1] if len(sys.argv) > 1 else "corpus/core") else 0)
