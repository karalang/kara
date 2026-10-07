//! B-2026-10-02-43: an explicit `return` of a Drop struct payload out of a boxed Option/Result frees it once

use super::*;

/// B-2026-10-02-43 — `struct R { id: i64, s: String }` with a `Drop` body is 4 words
/// against `Option`'s 3-word area, so `Some(mkr(2))` boxes it. An arm binding of
/// that payload is a view of the box interior; `Some(v) => v` stood the interior
/// walk down at the arm tail, but an explicit `return v` drained the box with the
/// walk armed and the caller freed the same `String` again. Cells cover the fresh
/// and named envelopes, a by-value param handed back, `if let`, `let .. else`, a
/// guard, `return Some(v)`, a loop, and the not-taken legs, which must still free
/// the box's payload.
#[test]
fn e2e_explicit_return_of_boxed_optres_drop_payload_frees_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
fn a1(c: bool) -> R { match Option.Some(mkr(1)) { Some(v) => { if c { return v } println("keep"); return mkr(0) } None => { return mkr(0) } } }
fn a2(c: bool) -> R { match Some(mkr(2)) { Some(v) if c => { return v } Some(w) => { println(f"w{w.id}"); return mkr(0) } None => { return mkr(0) } } }
fn a3() -> Option[R] { match Some(mkr(3)) { Some(v) => { return Some(v) } None => { return None } } }
fn a4() -> R { let x: Result[R, i64] = Ok(mkr(4)); match x { Ok(v) => { return v } Err(_) => { return mkr(0) } } }
fn a5() -> R { match Result.Ok(mkr(5)) { Ok(v) => { return v } Err(e) => { return mkr(e) } } }
fn a6() -> R { if let Some(v) = Some(mkr(6)) { return v } return mkr(0) }
fn a7() -> R { let Some(v) = Some(mkr(7)) else { return mkr(0) }; return v }
fn a8(c: bool) -> R { let o = Some(mkr(8)); match o { Some(v) => { if c { return v } println("k8"); return mkr(0) } None => { return mkr(0) } } }
fn a9(n: i64) -> R { let mut i = 0; while i < 3 { let o = Some(mkr(90 + i)); if let Some(v) = o { if i == n { return v; } } i = i + 1; } return mkr(0) }
fn a10() -> R { match Option.Some(mkr(10)) { Some(v) => { let k = v; return k } None => { return mkr(0) } } }
fn a11() -> i64 { match Option.Some(mkr(11)) { Some(v) => { return v.id } None => { return 0 } } }
fn h1(a: R) -> R { match Option.Some(a) { Some(v) => { return v } None => { return mkr(0) } } }
fn h6(a: R) -> R { if let Some(v) = Some(a) { return v } return mkr(0) }
fn main() {
    println("-a1t"); let r = a1(true); println(f"y{r.id}");
    println("-a1f"); let r = a1(false); println(f"y{r.id}");
    println("-a2t"); let r = a2(true); println(f"y{r.id}");
    println("-a2f"); let r = a2(false); println(f"y{r.id}");
    println("-a3"); let r = a3(); println(f"y{r.is_some()}");
    println("-a4"); let r = a4(); println(f"y{r.id}");
    println("-a5"); let r = a5(); println(f"y{r.id}");
    println("-a6"); let r = a6(); println(f"y{r.id}");
    println("-a7"); let r = a7(); println(f"y{r.id}");
    println("-a8t"); let r = a8(true); println(f"y{r.id}");
    println("-a8f"); let r = a8(false); println(f"y{r.id}");
    println("-a9"); let r = a9(1); println(f"y{r.id}");
    println("-a9n"); let r = a9(7); println(f"y{r.id}");
    println("-a10"); let r = a10(); println(f"y{r.id}");
    println("-a11"); let z = a11(); println(f"y{z}");
    println("-h1"); let r = h1(mkr(12)); println(f"y{r.id}");
    println("-h6"); let r = h6(mkr(13)); println(f"y{r.id}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "-a1t\ny1\nd1\n-a1f\nkeep\nd1\ny0\nd0\n-a2t\ny2\nd2\n-a2f\nw2\nd2\ny0\nd0\n-a3\nytrue\nd3\n-a4\ny4\nd4\n-a5\ny5\nd5\n-a6\ny6\nd6\n-a7\ny7\nd7\n-a8t\ny8\nd8\n-a8f\nk8\nd8\ny0\nd0\n-a9\nd90\ny91\nd91\n-a9n\nd90\nd91\nd92\ny0\nd0\n-a10\ny10\nd10\n-a11\nd11\ny11\n-h1\ny12\nd12\n-h6\ny13\nd13\nend\n", "got:\n{out}");
}
