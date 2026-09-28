//! B-2026-09-27-123 -- a boxed generic enum handed back whole by a generic
//! method's receiver, or by a generic call inside a branch or loop body, is
//! freed once.

use super::*;

/// B-2026-09-27-123 — a generic method that returns its owned receiver
/// (`fn id(self) -> Self { return self; }`) handed the heap box back to the
/// caller's result while the receiver binding still freed it: `let d2 =
/// d.id()` over `G[R]` aborted with `free(): double free` on every compiled
/// backend. The dynamic box-word compare that disarms a hand-back argument
/// never ran for the receiver, whose hand-back is spelled `return self` and
/// whose `G[T]` resolves through the receiver's own instantiation. Covers a
/// tail `self`, a chain, a fresh-temp receiver, and `String`/`Vec`/`Array`
/// payloads.
#[test]
fn e2e_generic_method_receiver_handed_back_whole_freed_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum G[T] { X(T), Y }
impl[T] G[T] {
    fn id(self) -> Self { return self; }
    fn tail(self) -> Self { self }
}
fn main() {
    { let d: G[R] = G.X(R { id: 1, tag: f"a" }); let d2 = d.id(); match d2 { G.X(t) => { println(f"m{t.id}") } G.Y => { println("mY") } } }
    { let d: G[String] = G.X(f"abc"); let d2 = d.id(); match d2 { G.X(t) => { println(f"s{t}") } G.Y => {} } }
    { let d: G[R] = G.X(R { id: 2, tag: f"b" }); let d2 = d.tail(); match d2 { G.X(t) => { println(f"t{t.id}") } G.Y => {} } }
    { let d: G[R] = G.X(R { id: 3, tag: f"c" }); let d2 = d.id().id(); match d2 { G.X(t) => { println(f"c{t.id}") } G.Y => {} } }
    { let d2 = G.X(R { id: 4, tag: f"d" }).id(); match d2 { G.X(t) => { println(f"f{t.id}") } G.Y => {} } }
    { let d2 = G.X(f"xyz").id().id(); match d2 { G.X(t) => { println(f"g{t}") } G.Y => {} } }
    { let d: G[Vec[String]] = G.X(["a", "b"]); let d2 = d.id(); match d2 { G.X(v) => { println(f"v{v.len()}") } G.Y => {} } }
    { let d: G[Array[String, 2]] = G.X([f"a", f"b"]); let d2 = d.id(); match d2 { G.X(v) => { println(f"a{v[1]}") } G.Y => {} } }
    let mut i = 0;
    while i < 2 { let d: G[R] = G.X(R { id: 10 + i, tag: f"l" }); let d2 = d.id(); match d2 { G.X(t) => { println(f"l{t.id}") } G.Y => {} } i = i + 1; }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "m1\ndR1\nsabc\nt2\ndR2\nc3\ndR3\nf4\ndR4\ngxyz\nv2\nab\nl10\ndR10\nl11\ndR11\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-27-123 — the "discarded statement" window that stops a generic
/// call's hand-back disarm when nothing consumes its result was armed over a
/// whole `if`, `match`, `for`, `while`, `while let` or `loop` statement, so
/// `let d2 = idg(d)` inside any such body read as discarded and `d` kept the
/// box `d2` also freed (a double free, or a crash with no output). The window
/// now names each arm's or body's tail only.
#[test]
fn e2e_generic_hand_back_inside_branch_or_loop_body_freed_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum G[T] { X(T), Y }
impl[T] G[T] { fn id(self) -> Self { return self; } }
fn idg[T](g: G[T]) -> G[T] { return g }
fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.Y }
fn mk(n: i64) -> G[R] { return G.X(R { id: n, tag: f"t" }) }
fn show(g: G[R]) { match g { G.X(t) => { println(f"s{t.id}") } G.Y => { println("sy") } } }
fn main() {
    let c = true;
    if c { let d = mk(1); let d2 = idg(d); show(d2) }
    match c { true => { let d = mk(2); let d2 = idg(d); show(d2) } false => {} }
    for k in 0..2 { let d = mk(3 + k); let d2 = idg(d); show(d2) }
    let mut j = 0; loop { let d = mk(5 + j); let d2 = idg(d); show(d2); j = j + 1; if j > 1 { break } }
    let mut o: Option[i64] = Some(1); while let Some(z) = o { let d = mk(7); let d2 = mid(d, true); show(d2); o = None; }
    if c { let d = mk(8); let d2 = d.id(); show(d2) }
    { let d = mk(19); if c { let d2 = mid(d, true); show(d2) } }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "s1\ndR1\ns2\ndR2\ns3\ndR3\ns4\ndR4\ns5\ndR5\ns6\ndR6\ns7\ndR7\ns8\ndR8\ns19\ndR19\nend\n",
        "got:\n{out}"
    );
}
