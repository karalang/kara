//! B-2026-10-04-47: a shared value held in a by-value argument is released at the call

use super::*;

/// B-2026-10-04-47 — design.md rule 3: a by-value argument handed to a callee that
/// keeps it is dropped when the call returns, before the caller's next statement.
/// That includes a `shared` value it holds in a struct field, an `Option` or enum
/// payload, a tuple or a `Vec`, which both backends used to release at the
/// caller's scope exit for most of those shapes. A handed-back argument (`_7`) and
/// one whose last use is a read (`_8`) still release at their own scope end.
#[test]
fn asan_shared_holder_moved_into_call_releases_at_the_call() {
    assert_clean_asan_run(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
struct W { o: Option[H], n: i64 }
struct X { h: H, n: i64 }
enum E { A(H), B }
fn cw(w: W) { println(f"w{w.n}") }
fn cx(x: X) { println(f"x{x.n}") }
fn co(o: Option[H]) { println("o") }
fn ct(t: (H, i64)) { println(f"t{t.1}") }
fn ce(e: E) { println("e") }
fn cv(v: Vec[H]) { println(f"v{v.len()}") }
fn keepw(w: W) -> W { w }
fn main() {
    let w = W { o: Some(H { id: 1 }), n: 7 }; cw(w); println("_1");
    let x = X { h: H { id: 2 }, n: 2 }; cx(x); println("_2");
    let o = Some(H { id: 3 }); co(o); println("_3");
    let t = (H { id: 4 }, 1); ct(t); println("_4");
    let e = E.A(H { id: 5 }); ce(e); println("_5");
    let v = vec![H { id: 6 }]; cv(v); println("_6");
    { let w7 = W { o: Some(H { id: 7 }), n: 8 }; let k = keepw(w7); println(f"_7 {k.n}"); }
    println("_7b");
    { let r = Some(H { id: 8 }); println(f"_8 {r.is_some()}"); }
    println("_8b");
    println("end")
}
"#,
        &[
            "w7", "dH1", "_1", "x2", "dH2", "_2", "o", "dH3", "_3", "t1", "dH4", "_4", "e", "dH5",
            "_5", "v1", "dH6", "_6", "_7 8", "dH7", "_7b", "_8 true", "dH8", "_8b", "end",
        ],
        "asan_shared_holder_moved_into_call_releases_at_the_call",
    );
}
