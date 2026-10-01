//! B-2026-09-30-103 — a destructure of an `Option`/`Result` STRUCT payload
//! that binds only some of its `Drop`-bearing fields. A leaf the arm only
//! reads is a VIEW of the husk, which runs every field's body in reverse
//! field order (design.md § Match Arm Binding Modes, § Field drop order) --
//! the order both backends already gave a user enum's payload. A leaf the arm
//! takes owns its field, and the fields it leaves stay with the husk. The
//! interpreter stood the whole payload walk down for any such arm (losing the
//! unbound fields' bodies, and running a read-only leaf at the arm's end);
//! codegen zeroed the bodies flag for a taking arm and for a `Result` whose
//! `Err` arm hands back a scalar.

use super::*;

/// B-2026-09-30-103 — the codegen fixture's program under ASAN: a taken leaf (`m6`, `y6`, `y7`) leaked 32 B when it was recorded as a view of the box.
#[test]
fn asan_optres_struct_payload_partial_destructure_bodies() {
    assert_clean_asan_run_min_allocs(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H2 { v: R, r: R, n: i64 }
struct G2[T] { v: T, r: R }
struct In { p: R, q: R }
struct H3 { i: In, k: R }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn h(k: i64) -> H2 { H2 { v: mk(k), r: mk(k * 10 + k), n: 1 } }
fn m1() -> i64 { let o = Option.Some(h(1)); match o { Option.Some(H2 { v, .. }) => v.id, Option.None => 0 } }
fn m2() -> i64 { let o = Option.Some(h(2)); match o { Option.Some(H2 { v, r, .. }) => v.id + r.id, Option.None => 0 } }
fn m3() -> i64 { let o = Option.Some(h(3)); match o { Option.Some(H2 { v: _, r, n }) => r.id + n, Option.None => 0 } }
fn m4() -> i64 { let o = Option.Some(h(4)); if let Option.Some(H2 { v, .. }) = o { println(v.id) } 0 }
fn m5() -> i64 { let o: Result[H2, i64] = Result.Ok(h(5)); match o { Result.Ok(H2 { r, .. }) => r.id, Result.Err(e) => e } }
fn m6() -> i64 { let o = Option.Some(h(6)); match o { Option.Some(H2 { v, .. }) => { let w = v; w.id }, Option.None => 0 } }
fn m7() -> i64 { let o = Option.Some(G2 { v: mk(7), r: mk(77) }); match o { Option.Some(G2 { v, .. }) => v.id, Option.None => 0 } }
fn y6() -> i64 { let o: Result[H2, i64] = Result.Ok(h(8)); match o { Result.Ok(H2 { v, .. }) => { let w = v; w.id }, Result.Err(e) => e } }
fn y7() -> i64 { let o = Option.Some(h(9)); if let Option.Some(H2 { v, .. }) = o { let w = v; println(w.id) } 0 }
fn y8() -> i64 { let o = Option.Some(H3 { i: In { p: mk(12), q: mk(13) }, k: mk(14) }); match o { Option.Some(H3 { i: In { p, .. }, .. }) => p.id, Option.None => 0 } }
fn y9() -> i64 { let o = Option.Some(h(15)); match o { Option.Some(H2 { v, n, .. }) if n > 0 => v.id, Option.Some(H2 { r, .. }) => r.id, Option.None => 0 } }
fn yb() -> i64 { let o = Option.Some(h(16)); match o { Option.Some(H2 { v, n, .. }) => { println(v.id); n }, Option.None => 0 } }
fn yc() -> i64 { let o: Result[H2, R] = Result.Ok(h(17)); match o { Result.Ok(H2 { v, .. }) => v.id, Result.Err(e) => e.id } }
fn main() {
    println(m1()); println(m2()); println(m3()); println(m4()); println(m5()); println(m6()); println(m7())
    println(y6()); println(y7()); println(y8()); println(y9()); println(yb()); println(yc())
    println("end")
}"#,
        &[
            "d11", "d1", "1", "d22", "d2", "24", "d33", "d3", "34", "4", "d44", "d4", "0", "d55",
            "d5", "55", "d6", "d66", "6", "d77", "d7", "7", "d8", "d88", "8", "9", "d9", "d99",
            "0", "d14", "d13", "d12", "12", "d165", "d15", "15", "16", "d176", "d16", "1", "d187",
            "d17", "17", "end",
        ],
        "asan_optres_struct_payload_partial_destructure_bodies",
        3,
    );
}
