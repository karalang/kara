//! B-2026-09-28-55 — a fresh temp struct ARGUMENT's `shared` field is
//! released at the end of the statement that built it, on every backend.

use super::*;

/// B-2026-09-28-55 — `if zn(mkz(1)) > 0 { println("t1") } println("f1end");`
/// released the temp's `shared` field (its `Drop` body `dN1`) at the
/// FUNCTION's end on the compiled surfaces: the argument temp's memory drop
/// sat on the scope frame, which a `let` initializer hit as well. It now
/// drains with the statement's other fresh temps. The interpreter half: a
/// nested block's TAIL (a loop body) ends its holders when the tail has
/// evaluated rather than at the enclosing statement, and two temp arguments
/// of one call release right to left. Neighbours: a `let`-bound `if`, a
/// `let`, a returning branch, `else if`, two arguments, a method, a loop
/// body tail, a generic callee (compiled nested), a struct literal, a
/// `match` scrutinee.
#[test]
fn e2e_temp_arg_shared_field_released_at_statement_end() {
    let src = r#"struct D { id: i64, name: String }
impl Drop for D { fn drop(mut ref self) { println(f"dD{self.id}{self.name}") } }
fn mkd(n: i64) -> D { return D { id: n, name: f"n{n}" }; }
shared struct N { v: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.v}") } }
struct Z { d: D, h: N }
fn mkz(n: i64) -> Z { Z { d: mkd(n), h: N { v: n } } }
fn zn(z: Z) -> i64 { z.h.v }
fn zz(a: Z, b: Z) -> i64 { a.h.v + b.h.v }
struct Y { h: N, k: i64 }
fn yn(y: Y) -> i64 { y.k }
struct H { c: i64 }
impl H { fn m(ref self, z: Z) -> i64 { z.h.v + self.c } }
fn gz[T](t: T, z: Z) -> i64 { zn(z) }
fn f1() { if zn(mkz(1)) > 0 { println("t1") } println("f1end"); }
fn f2() { let x = if zn(mkz(2)) > 0 { 1 } else { 2 }; println(f"t2{x}"); }
fn f3() { let q = zn(mkz(3)); println(f"q{q}"); }
fn f4() -> i64 { if zn(mkz(4)) > 0 { println("r4"); return 4; } 0 }
fn f5() { if zn(mkz(5)) > 10 { println("x") } else if zn(mkz(6)) > 0 { println("e6") } println("f5end"); }
fn f6() { let v = zz(mkz(7), mkz(8)); println(f"v{v}"); }
fn f7() { let h = H { c: 1 }; if h.m(mkz(9)) > 0 { println("h9") } println("f7end"); }
fn f8() { for k in 0..2 { if zn(mkz(10 + k)) > 0 { println(f"k{k}") } } println("f8end"); }
fn f9() { if gz(1, mkz(12)) > 0 { println("t12") } println("f9end"); }
fn f10() { if yn(Y { h: N { v: 13 }, k: 13 }) > 0 { println("t13") } println("f10end"); }
fn f11() { match zn(mkz(14)) { 14 => println("m14"), _ => println("mx") } println("f11end"); }
fn main() { f1(); f2(); f3(); println(f"{f4()}"); f5(); f6(); f7(); f8(); f9(); f10(); f11(); println("end"); }"#;
    let want = "dD1n1\nt1\ndN1\nf1end\ndD2n2\ndN2\nt21\ndD3n3\ndN3\nq3\ndD4n4\nr4\ndN4\n4\ndD5n5\ndD6n6\ne6\ndN6\ndN5\nf5end\ndD8n8\ndD7n7\ndN8\ndN7\nv15\ndD9n9\nh9\ndN9\nf7end\ndD10n10\nk0\ndN10\ndD11n11\nk1\ndN11\nf8end\ndD12n12\nt12\ndN12\nf9end\nt13\ndN13\nf10end\ndD14n14\nm14\ndN14\nf11end\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
