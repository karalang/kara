//! B-2026-10-03-52 -- moving the `Option[R]` envelope out of a
//! `Result[Option[R], E]` freed R's heap twice.

use super::*;

/// B-2026-10-03-52 — the envelope binding `o` of `Ok(o)` carries the nested
/// box's pointer, so moving it (returning it, pushing it) hands the box to the
/// new owner; the source's `NestedBoxedEnumDrop` freed it as well. Covers a
/// by-value param returned and pushed through `match`, `if let` and
/// `let ... else`, and a local pushed through `match`.
///
/// The `let ... else` line is the compiled answer; the interpreter runs that
/// body twice (B-2026-10-03-49's family).
#[test]
fn asan_moved_optres_envelope_box_freed_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn ret(x: Result[Option[R], i64]) -> Option[R] { match x { Ok(o) => o, Err(e) => None } }
fn mo(x: Result[Option[R], i64], vo: mut ref Vec[Option[R]]) -> i64 { match x { Ok(o) => { vo.push(o); 1 } Err(e) => e } }
fn io(x: Result[Option[R], i64], vo: mut ref Vec[Option[R]]) -> i64 { if let Ok(o) = x { vo.push(o); return 1 } return 0 }
fn lo(x: Result[Option[R], i64], vo: mut ref Vec[Option[R]]) -> i64 { let Ok(o) = x else { return 0 }; vo.push(o); return 1 }
fn main() {
    let r = ret(Ok(Some(mk(1))));
    println(r.is_some());
    let mut vo: Vec[Option[R]] = Vec.new();
    println(mo(Ok(Some(mk(2))), mut vo));
    println(io(Ok(Some(mk(3))), mut vo));
    println(lo(Ok(Some(mk(4))), mut vo));
    let a: Result[Option[R], i64] = Ok(Some(mk(5)));
    match a { Ok(o) => vo.push(o), Err(e) => println(e) }
    println(vo.len());
    println("end");
}
"#,
        &[
            "true", "dR1", "1", "1", "1", "4", "dR2", "dR3", "dR4", "dR5", "end",
        ],
        "moved_optres_envelope_box",
    );
}
