//! B-2026-09-30-76 — an arm over a by-value, TRANSFER-owned enum param that
//! MOVES the payload (a nested struct pattern, or a sibling handed on) cleared
//! the callee's payload walk, while a leaf whose type has its own `Drop` was
//! registered memory-only as if the caller would run its body. The caller had
//! stood its walk down too, so the body ran nowhere. Such a leaf now takes the
//! body-running registration a local's leaf gets.

use super::*;

/// B-2026-09-30-76 — nested struct patterns under tuple and struct variants, a flat sibling beside a nest, a sibling handed to a callee, a leaf moved on, and the read-only arm that was already right.
#[test]
fn e2e_transfer_owned_enum_param_move_arm_runs_leaf_drop_bodies() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id} {self.s.len()}") } }
struct H { v: R, n: i64 }
enum F { A(R, H), B }
enum Fh { A(H, i64), B }
enum Fs { A { x: H, k: i64 }, B }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn eat(h: H) -> i64 { h.n }
fn c1(e: Fh) -> i64 { match e { Fh.A(H { v, n }, k) => v.id + n + k, Fh.B => 0 } }
fn c2(e: Fs) -> i64 { match e { Fs.A { x: H { v, n }, k } => v.id + n + k, Fs.B => 0 } }
fn c3(e: F) -> i64 { match e { F.A(r, H { v, n }) => r.id + v.id + n, F.B => 0 } }
fn c4(e: F) -> i64 { match e { F.A(r, h) => r.id + eat(h), F.B => 0 } }
fn c5(e: F) -> i64 { match e { F.A(r, H { v, n }) => { let w = v; r.id + w.id + n }, F.B => 0 } }
fn c6(e: F) -> i64 { match e { F.A(r, h) => r.id + h.n, F.B => 0 } }
fn main() {
    println(c1(Fh.A(H { v: R { id: 1, s: mk(1) }, n: 10 }, 100)));
    println(c1(Fh.B));
    println(c2(Fs.A { x: H { v: R { id: 2, s: mk(2) }, n: 20 }, k: 200 }));
    println(c3(F.A(R { id: 3, s: mk(3) }, H { v: R { id: 4, s: mk(4) }, n: 40 })));
    println(c4(F.A(R { id: 5, s: mk(5) }, H { v: R { id: 6, s: mk(6) }, n: 60 })));
    println(c5(F.A(R { id: 7, s: mk(7) }, H { v: R { id: 8, s: mk(8) }, n: 80 })));
    println(c6(F.A(R { id: 9, s: mk(9) }, H { v: R { id: 11, s: mk(11) }, n: 110 })));
    println("end");
}"#;
    let want = "d1 31\n111\n0\nd2 31\n222\nd4 31\nd3 31\n47\nd6 31\nd5 31\n65\nd8 31\nd7 31\n95\nd11 32\nd9 31\n119\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
