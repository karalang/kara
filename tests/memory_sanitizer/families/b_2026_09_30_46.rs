//! B-2026-09-30-46 — a `match` arm that binds a named STRUCT scrutinee WHOLE
//! (`match g { x => .. }`) gave the binding its own owner while nothing
//! disarmed the scrutinee's, so both dropped one value: the struct's memory
//! freed twice for a local (a plain `String` field, no `Drop` anywhere, was
//! enough), and every field body ran twice for a param. Such an arm is now
//! compiled as `let x = g` after its guard. The `let` path gained the by-
//! transfer half it lacked (`let x = g` over `g: G[R]` ran no body at all),
//! and `--interp` stopped leaving the scrutinee armed beside a binding that
//! moved on (`let h = match g { x => x }` ran the body twice there).

use super::*;

/// B-2026-09-30-46 — whole-value arms over caller-retained and by-transfer
/// params, guarded and multi-arm matches, `let x = g` over a by-transfer
/// param, and named locals (with and without `Drop`) whose binding is read,
/// dropped at the arm, or moved on into a new local.
#[test]
fn asan_whole_value_match_arm_over_named_struct_runs_each_drop_once() {
    assert_clean_asan_run_min_allocs(
        r#"struct G[T] { v: T, n: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct Rs { id: i64, s: String }
impl Drop for Rs { fn drop(mut ref self) { println(f"ds{self.id} {self.s.len()}") } }
struct H { v: Rs, n: i64 }
struct P { s: String, n: i64 }
fn mk(k: i64) -> String { f"a-heap-string-longer-than-sso-{k}" }
fn sink(h: H) -> i64 { h.n }
fn b1(g: H) -> i64 { match g { x => x.n } }
fn b2(g: G[R]) -> i64 { match g { x => x.n } }
fn b3(g: G[Rs]) -> i64 { match g { x => { let v = x.v; v.id } } }
fn b4(g: H) -> i64 { match g { H { n: 0, .. } => 0, x => sink(x) } }
fn b5(g: G[R]) -> i64 { let x = g; let v = x.v; v.id }
fn b6(g: H) -> i64 { match g { x if x.n > 5 => x.n, y => 0 - y.n } }
fn main() {
    println(b1(H { v: Rs { id: 1, s: mk(1) }, n: 10 }));
    println(b2(G { v: R { id: 2 }, n: 20 }));
    println(b3(G { v: Rs { id: 3, s: mk(3) }, n: 30 }));
    println(b4(H { v: Rs { id: 4, s: mk(4) }, n: 40 }));
    println(b4(H { v: Rs { id: 5, s: mk(5) }, n: 0 }));
    println(b5(G { v: R { id: 6 }, n: 60 }));
    println(b6(H { v: Rs { id: 7, s: mk(7) }, n: 70 }));
    println(b6(H { v: Rs { id: 8, s: mk(8) }, n: 1 }));
    let p = P { s: mk(9), n: 90 };
    let k = match p { x => x.n };
    println(k);
    let h = H { v: Rs { id: 10, s: mk(10) }, n: 100 };
    let k2 = match h { x => x.n + 1 };
    println(k2);
    let q = H { v: Rs { id: 11, s: mk(11) }, n: 110 };
    let q2 = match q { x => x };
    println(q2.n);
    let t = R { id: 12 };
    let t2 = match t { x => { x } };
    println(t2.id);
    let u = G { v: Rs { id: 13, s: mk(13) }, n: 130 };
    let k3 = match u { G { n: 0, .. } => 0, x => x.n };
    println(k3);
    println("end");
}"#,
        &[
            "ds1 31", "10", "d2", "20", "ds3 31", "3", "ds4 31", "40", "ds5 31", "0", "d6", "6",
            "ds7 31", "70", "ds8 31", "-1", "90", "ds10 32", "101", "110", "ds11 32", "12", "d12",
            "ds13 32", "130", "end",
        ],
        "asan_whole_value_match_arm_over_named_struct_runs_each_drop_once",
        8,
    );
}
