//! B-2026-10-05-86 — an `Option` local reassigned after an arm moved its payload out.

use super::*;

/// After a `match` or `if let` bound the payload out of a `let mut`
/// `Option`/`Result` local, reassigning the local never ran the NEW value's
/// `Drop` body compiled: the arm's per-path payload flag stayed `false`, and
/// both the live-range fire and the scope-exit walk read it. Covers `match`,
/// `if let`, an `Array` payload, a by-value param rebound, a loop, a guarded
/// arm that takes nothing, a match on only some paths, `Result`, two
/// reassigns, a reassign through `None`, and a second match after the first.
#[test]
fn asan_option_local_reassigned_after_a_move_runs_the_new_body() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mat() { let mut h: Option[R] = Some(mk(31)); match h { Some(a) => println(f"m{a.id}"), None => println("n") } h = Some(mk(37)); println("w1") }
fn iflet() { let mut h: Option[R] = Some(mk(40)); if let Some(a) = h { println(f"m{a.id}") } h = Some(mk(41)); println(f"w2 {h.is_some()}") }
fn arr() { let mut h: Option[Array[R, 2]] = Some([mk(27), mk(28)]); match h { Some(a) => println(f"m{a[0].id}"), None => println("n") } h = Some([mk(29), mk(30)]); println("w3") }
fn param(x: Option[Array[R, 2]]) { let mut h = x; match h { Some(a) => println(f"m{a[0].id}"), None => println("n") } h = Some([mk(7), mk(8)]); println("w4") }
fn looped() { let mut h: Option[R] = Some(mk(32)); let mut i = 0; while i < 2 { match h { Some(a) => println(f"m{a.id}"), None => println("n") } h = Some(mk(60 + i)); i = i + 1; } println("w5") }
fn guarded() { let mut h: Option[R] = Some(mk(5)); match h { Some(a) if a.id > 100 => println(f"m{a.id}"), _ => println("miss") } h = Some(mk(6)); println("w6") }
fn some_paths(c: bool) { let mut h: Option[R] = Some(mk(10)); if c { match h { Some(a) => println(f"m{a.id}"), None => println("n") } } h = Some(mk(11)); println(f"w7 {c}") }
fn res() { let mut h: Result[R, i64] = Ok(mk(12)); match h { Ok(a) => println(f"m{a.id}"), Err(e) => println(f"e{e}") } h = Ok(mk(13)); println("w8") }
fn twice() { let mut h: Option[R] = Some(mk(14)); if let Some(a) = h { println(f"m{a.id}") } h = Some(mk(15)); println("mid"); h = Some(mk(16)); println(f"w9 {h.is_some()}") }
fn via_none() { let mut h: Option[R] = Some(mk(17)); if let Some(a) = h { println(f"m{a.id}") } h = None; println("mid"); h = Some(mk(18)); println("w10") }
fn rematch() { let mut h: Option[R] = Some(mk(25)); match h { Some(a) => println(f"m{a.id}"), None => println("n") } h = Some(mk(26)); match h { Some(b) => println(f"m{b.id}"), None => println("n") } h = Some(mk(27)); println(f"w11 {h.is_some()}") }
fn main() {
    mat(); iflet(); arr(); param(Some([mk(51), mk(52)])); looped(); guarded();
    some_paths(false); some_paths(true); res(); twice(); via_none(); rematch();
    println("end")
}
"#,
        &[
            "m31", "dR31", "dR37", "w1", "m40", "dR40", "w2 true", "dR41", "m27", "dR27", "dR28",
            "dR29", "dR30", "w3", "m51", "dR51", "dR52", "w4", "dR7", "dR8", "m32", "dR32", "m60",
            "dR60", "dR61", "w5", "miss", "dR5", "dR6", "w6", "dR10", "dR11", "w7 false", "m10",
            "dR10", "dR11", "w7 true", "m12", "dR12", "dR13", "w8", "m14", "dR14", "mid", "dR15",
            "w9 true", "dR16", "m17", "dR17", "mid", "dR18", "w10", "m25", "dR25", "m26", "dR26",
            "w11 true", "dR27", "end",
        ],
        "option_local_reassigned_after_a_move_runs_the_new_body",
    );
}
