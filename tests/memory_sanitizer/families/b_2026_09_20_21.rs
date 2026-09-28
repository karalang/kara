//! B-2026-09-20-21 -- a `let` that rebinds a name, in a sibling block or in
//! the same scope, does not inherit the earlier binding's boxed struct-payload
//! facts, so a boxed TUPLE payload handed to a consuming callee is freed once.

use super::*;

/// B-2026-09-20-21 — before the fix the second binding of each name kept its
/// box on the caller side while `ttake` freed it: `free(): double free
/// detected in tcache 2`.
#[test]
fn asan_rebound_name_boxed_tuple_payload_frees_once() {
    assert_clean_asan_run(
        r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"p{t.r.v}.{t.s.v}") } Option.None => { println("n") } } }
fn ttake(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println(f"t{x.v}.{t.1.v}") } Option.None => { println("n") } } }
fn main() {
    { let a = Option.Some(Q { r: mkr(1), s: mkr(2) }); peek(a) }
    println("out1");
    { let a = Option.Some((mkr(3), mkr(4))); ttake(a) }
    println("out2");
    let b = Option.Some(Q { r: mkr(5), s: mkr(6) });
    peek(b);
    let b = Option.Some((mkr(7), mkr(8)));
    ttake(b);
    println("end")
}
"#,
        &[
            "p10.20", "d2v20", "d1v10", "out1", "t30.40", "d3v30", "d4v40", "out2", "p50.60",
            "d6v60", "d5v50", "t70.80", "d7v70", "d8v80", "end",
        ],
        "asan_rebound_name_boxed_tuple_payload_frees_once",
    );
}

/// B-2026-09-20-21 — the let-site multi-field marker. The locals here are
/// named `q` so no callee param shares the name: with the callees' `g` the
/// param site's marker used to survive into `main` and hid that the `let`
/// never set its own, so `fh(q)` zeroed the whole slot and leaked the heap
/// sibling (30 B for `Gh`, 192 B for `Gv` at `-O0`).
#[test]
fn asan_let_bound_multi_field_generic_variant_box_and_heap_sibling_both_freed() {
    assert_clean_asan_run(
        r#"
enum Gh[T] { Y(T, String), N }
enum Ghs[T] { Y(String, T), N }
enum Gt[T] { Y(T, T), N }
enum Gv[T] { Y(T, Vec[String]), N }
fn mkI(i: i64) -> Array[i64, 4] { return [i, i + 1, i + 2, i + 3]; }
fn mkv(s: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(s); return v; }
fn fh(g: Gh[Array[i64, 4]]) -> i64 { match g { Gh.Y(x, s) => { return x[0] + s.len(); } Gh.N => { return 0; } } }
fn fhs(g: Ghs[Array[i64, 4]]) -> i64 { match g { Ghs.Y(s, x) => { return x[0] + s.len(); } Ghs.N => { return 0; } } }
fn ft(g: Gt[Array[i64, 4]]) -> i64 { match g { Gt.Y(x, y) => { return x[0] + y[1]; } Gt.N => { return 0; } } }
fn fv(g: Gv[Array[i64, 4]]) -> i64 { match g { Gv.Y(x, v) => { return x[0] + v.len(); } Gv.N => { return 0; } } }
fn main() {
    let mut i = 1;
    while i < 3 {
        { let q: Gh[Array[i64, 4]] = Gh.Y(mkI(i), f"sib-{i}-padpadpad"); println(f"h:{fh(q)}"); }
        { let q: Ghs[Array[i64, 4]] = Ghs.Y(f"sib-{i}-padpadpad", mkI(i)); println(f"s:{fhs(q)}"); }
        { let q: Gt[Array[i64, 4]] = Gt.Y(mkI(i), mkI(i + 10)); println(f"t:{ft(q)}"); }
        { let q: Gv[Array[i64, 4]] = Gv.Y(mkI(i), mkv(f"v-{i}-padpadpad")); println(f"v:{fv(q)}"); }
        { let q: Gh[Array[i64, 4]] = Gh.Y(mkI(i), f"loc-{i}-padpadpad");
          match q { Gh.Y(x, s) => { println(f"L:{x[0] + s.len()}"); } Gh.N => { println("n"); } } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "h:16", "s:16", "t:13", "v:2", "L:16", "h:17", "s:17", "t:15", "v:3", "L:17", "end",
        ],
        "asan_let_bound_multi_field_generic_variant_box_and_heap_sibling_both_freed",
    );
}
