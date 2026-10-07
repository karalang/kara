//! B-2026-09-19-60 -- a consuming arm over a SEEDED fresh-temp scrutinee
//! (`match Option.Some(a) { Option.Some(v) => .. }` over a named `Array`
//! local `a`) hands the element buffers to exactly one new owner.

use super::*;

/// B-2026-09-19-60 — rebinding the arm binding into a local (`let u = v;`),
/// typed or untyped, storing it into a struct literal, `if let`, the bare
/// `Some` and `Ok` spellings, a `String` element and a loop body each run
/// every element's `Drop` body once and free every buffer once.
///
/// Before: each of these aborted with `free(): double free detected` at
/// `-O0` -- the named source `a` kept its `StructDrop` while the rebound
/// local took the same buffers.
#[test]
fn asan_seeded_array_payload_rebind_has_one_owner() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
struct H { a: Array[R, 2] }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn mks() -> Array[String, 2] { return [f"aa{1}", f"bb{2}"] }
fn rebind() { let a: Array[R, 2] = mka(10); match Option.Some(a) { Option.Some(v) => { let u = v; println(f"rebind {u[0].id}") }, Option.None => {} } }
fn untyped() { let a = mka(20); match Option.Some(a) { Option.Some(v) => { let u = v; println(f"untyped {u[1].id}") }, Option.None => {} } }
fn store() { let a: Array[R, 2] = mka(30); match Option.Some(a) { Option.Some(v) => { let h = H { a: v }; println(f"store {h.a[0].id}") }, Option.None => {} } }
fn iflet() { let a: Array[R, 2] = mka(40); if let Option.Some(v) = Option.Some(a) { let u = v; println(f"iflet {u[0].id}") } }
fn bare() { let a: Array[R, 2] = mka(50); match Some(a) { Some(v) => { let u = v; println(f"bare {u[0].id}") }, None => {} } }
fn okseed() { let a: Array[R, 2] = mka(60); match Ok(a) { Ok(v) => { let u = v; println(f"ok {u[0].id}") }, Err(e) => { println(f"e{e}") } } }
fn strs() { let a: Array[String, 2] = mks(); match Option.Some(a) { Option.Some(v) => { let u = v; println(f"strs {u[1]}") }, Option.None => {} } }
fn main() {
    rebind();
    untyped();
    store();
    iflet();
    bare();
    okseed();
    strs();
    let mut i = 0;
    while i < 2 { let a: Array[R, 2] = mka(70 + i * 2); match Option.Some(a) { Option.Some(v) => { let u = v; println(f"loop {u[0].id}") }, Option.None => {} }; i = i + 1; }
    println("end")
}"#,
        &[
            "rebind 10",
            "  d10",
            "  d11",
            "untyped 21",
            "  d20",
            "  d21",
            "store 30",
            "  d30",
            "  d31",
            "iflet 40",
            "  d40",
            "  d41",
            "bare 50",
            "  d50",
            "  d51",
            "ok 60",
            "  d60",
            "  d61",
            "strs bb2",
            "loop 70",
            "  d70",
            "  d71",
            "loop 72",
            "  d72",
            "  d73",
            "end",
        ],
        "seeded_array_payload_rebind",
    );
}

/// B-2026-09-19-60 — the other ways the arm binding leaves: pushed into a
/// `Vec`, returned as a `match` tail into a typed `let`, rebound and
/// handed to a by-value callee, and the seeded rebind inside a callee whose
/// source is its own by-value parameter.
///
/// Before: the push and tail cells aborted with a double free at `-O0`.
#[test]
fn asan_seeded_array_payload_moved_out_has_one_owner() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn eat(a: Array[R, 2]) -> i64 { return a[0].id }
fn keep(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { let u = v; return u[1].id }, Option.None => { return 0 } } }
fn push() -> i64 { let a: Array[R, 2] = mka(10); let mut w: Vec[Array[R, 2]] = Vec.new(); match Option.Some(a) { Option.Some(v) => { w.push(v) }, Option.None => {} }; return w.len() }
fn tail() -> i64 { let a: Array[R, 2] = mka(20); let k: Array[R, 2] = match Option.Some(a) { Option.Some(v) => v, Option.None => mka(0) }; return k[1].id }
fn rebeat() -> i64 { let a: Array[R, 2] = mka(30); match Option.Some(a) { Option.Some(v) => { let u = v; return eat(u) }, Option.None => { return 0 } } }
fn main() {
    let b = mka(40);
    println(f"keep {keep(b)}");
    println(f"push {push()}");
    println(f"tail {tail()}");
    println(f"rebeat {rebeat()}");
    println("end")
}"#,
        &[
            "keep 41",
            "  d40",
            "  d41",
            "  d10",
            "  d11",
            "push 1",
            "  d20",
            "  d21",
            "tail 21",
            "  d30",
            "  d31",
            "rebeat 30",
            "end",
        ],
        "seeded_array_payload_moved_out",
    );
}
