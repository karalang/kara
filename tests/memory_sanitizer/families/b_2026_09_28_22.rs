//! B-2026-09-28-22 — B-2026-09-27-98's forward to a method, associated or generic consumer.

use super::*;

/// B-2026-09-28-22 — B-2026-09-27-98's forward when the consumer is an
/// instance METHOD (struct and enum receivers), an ASSOCIATED function or a
/// GENERIC free function (bare `T`, and `Option[T]`). Each keeps nothing, so
/// the forwarding frame's per-path flag must stay armed on the exit that only
/// passed the param along; the exception resolved only a bare-identifier call
/// to a non-generic free function, so these lost the body on every surface.
/// Struct, `Option`, a rebind before the branch, and the returning path.
#[test]
fn asan_param_handed_back_on_some_paths_forwarded_to_a_method_or_generic_consumer() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn geat[T](t: T) { println("gx") }
fn geo[T](o: Option[T]) { println("go") }
fn gkeep[T](t: T, v: mut ref Vec[T]) { v.push(t) }
struct K { k: i64 }
impl K {
    fn eat(ref self, s: S) { println("mx") }
    fn eato(ref self, o: Option[S]) { println("mo") }
    fn aeat(s: S) { println("ax") }
    fn back(ref self, s: S) -> S { return s }
}
struct V { v: Vec[S] }
impl V { fn put(mut ref self, s: S) { self.v.push(s) } }
enum E { A(i64), B }
impl E { fn eat(ref self, s: S) { println("ex") } }
fn pm(s: S, k: bool, q: K) -> S { if k { return s } q.eat(s); return mks(0) }
fn pa(s: S, k: bool) -> S { if k { return s } K.aeat(s); return mks(0) }
fn pg(s: S, k: bool) -> S { if k { return s } geat(s); return mks(0) }
fn po(h: Option[S], k: bool, q: K) -> Option[S] { if k { return h } q.eato(h); return Option.None }
fn pgo(h: Option[S], k: bool) -> Option[S] { if k { return h } geo(h); return Option.None }
fn pe(s: S, k: bool, e: E) -> S { if k { return s } e.eat(s); return mks(0) }
fn pl(s: S, k: bool, q: K) -> S { let m = s; if k { return m } q.eat(m); return mks(0) }
fn main() {
    let a1 = pm(mks(1), false, K { k: 1 }); println(f"a{a1.id}");
    let a2 = pm(mks(2), true, K { k: 1 }); println(f"a{a2.id}");
    let b3 = pa(mks(3), false); println(f"b{b3.id}");
    let c4 = pg(mks(4), false); println(f"c{c4.id}");
    let c5 = pg(mks(5), true); println(f"c{c5.id}");
    let o6 = po(Option.Some(mks(6)), false, K { k: 1 }); println(f"o{o6.is_some()}");
    let o7 = pgo(Option.Some(mks(7)), false); println(f"o{o7.is_some()}");
    let e8 = pe(mks(8), false, E.A(1)); println(f"e{e8.id}");
    let l9 = pl(mks(9), false, K { k: 1 }); println(f"l{l9.id}");
    println("end")
}
"#,
        &[
            "mx", "dS1", "a0", "dS0", "a2", "dS2", "ax", "dS3", "b0", "dS0", "gx", "dS4", "c0",
            "dS0", "c5", "dS5", "mo", "dS6", "ofalse", "go", "dS7", "ofalse", "ex", "dS8", "e0",
            "dS0", "mx", "dS9", "l0", "dS0", "end",
        ],
        "asan_param_handed_back_on_some_paths_forwarded_to_a_method_or_generic_consumer",
    );
}

/// B-2026-09-28-22 — the controls: a generic consumer that pushes its param
/// into an accumulator, a method that stores it into its receiver, and a
/// method and a generic function that hand it back. Each owns the body, so the
/// forward must disarm the flag and the body runs once.
#[test]
fn asan_param_handed_back_on_some_paths_forwarded_to_a_keeping_method_or_generic_consumer() {
    assert_clean_asan_run(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn geat[T](t: T) { println("gx") }
fn geo[T](o: Option[T]) { println("go") }
fn gkeep[T](t: T, v: mut ref Vec[T]) { v.push(t) }
struct K { k: i64 }
impl K {
    fn eat(ref self, s: S) { println("mx") }
    fn eato(ref self, o: Option[S]) { println("mo") }
    fn aeat(s: S) { println("ax") }
    fn back(ref self, s: S) -> S { return s }
}
struct V { v: Vec[S] }
impl V { fn put(mut ref self, s: S) { self.v.push(s) } }
fn gid[T](t: T) -> T { return t }
fn pk(s: S, k: bool, v: mut ref Vec[S]) -> S { if k { return s } gkeep(s, v); return mks(0) }
fn pv(s: S, k: bool, w: mut ref V) -> S { if k { return s } w.put(s); return mks(0) }
fn pb(s: S, k: bool, q: K) -> S { if k { return s } let t = q.back(s); println(f"t{t.id}"); return mks(0) }
fn pr(s: S, k: bool) -> S { if k { return s } let t = gid(s); println(f"t{t.id}"); return mks(0) }
fn main() {
    let mut v: Vec[S] = Vec.new();
    let a = pk(mks(1), false, mut v); println(f"a{a.id}"); println(f"n{v.len()}");
    let mut w = V { v: Vec.new() };
    let b = pv(mks(2), false, mut w); println(f"b{b.id}"); println(f"n{w.v.len()}");
    let c = pb(mks(3), false, K { k: 1 }); println(f"c{c.id}");
    let d = pr(mks(4), false); println(f"d{d.id}");
    println("end")
}
"#,
        &[
            "a0", "dS0", "n1", "dS1", "b0", "dS0", "n1", "dS2", "t3", "dS3", "c0", "dS0", "t4",
            "dS4", "d0", "dS0", "end",
        ],
        "asan_param_handed_back_on_some_paths_forwarded_to_a_keeping_method_or_generic_consumer",
    );
}
