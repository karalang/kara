//! B-2026-09-19-18 -- a PLAIN holder of a `shared` value whose release runs a
//! user `Drop` body (a struct field, a `Vec` element, an `Option` payload, an
//! enum payload, a tuple element) releases it at the holder's live-range end.

use super::*;

/// B-2026-09-19-18 — `shared enum SMono { P(R2), Q }` over `R2`'s own `Drop`,
/// held one indirection out from a binding: a struct field (`field`, and with
/// the holder read after its `let` in `fuse`, handed by value in `fmove`), a
/// `Vec` element (`vec`, `vuse`), an `Option` payload (`opt`, `ouse`), an enum
/// payload (`enum`), a tuple element (`tuple`); then a holder pushed into a
/// container (`pushed`), returned from a callee (`ret`), reassigned (`reass`),
/// rebound whole (`rebind`), shadowed (`shadow`) and declared in a loop body
/// (`loop`).
///
/// Before: every compiled surface ran the body at lexical scope exit, after
/// `mid`, where `--interp` ran it at the holder's last use, before `mid`; one
/// body either way. design.md § Drop ordering puts destructor calls, `Rc`
/// decrements included, at each binding's live-range end, which is the
/// interpreter's placement, and the bare `shared` binding already fired there
/// (B-2026-09-17-19).
#[test]
fn test_plain_holder_of_drop_relevant_shared_releases_at_last_use() {
    let out = run(r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
shared enum SMono { P(R2), Q }
fn mkr(n: i64) -> R2 { R2 { s: f"aaaaaaaa-{n}", t: f"t{n}", u: f"u{n}" } }
struct W { e: SMono, n: i64 }
enum H3 { P(SMono), Q }
fn eat(w: W) { println("  eat") }
fn mkw(n: i64) -> W { let w = W { e: SMono.P(mkr(n)), n: n }; println("  built"); w }

fn main() {
    println("field");  { let w = W { e: SMono.P(mkr(1)), n: 1 }; println("  mid") } println("  out")
    println("fuse");   { let w = W { e: SMono.P(mkr(2)), n: 2 }; println(f"  n={w.n}"); println("  mid") } println("  out")
    println("fmove");  { let w = W { e: SMono.P(mkr(3)), n: 3 }; eat(w); println("  mid") } println("  out")
    println("vec");    { let v: Vec[SMono] = [SMono.P(mkr(4))]; println("  mid") } println("  out")
    println("vuse");   { let v: Vec[SMono] = [SMono.P(mkr(5)), SMono.Q]; println(f"  n={v.len()}"); println("  mid") } println("  out")
    println("opt");    { let o: Option[SMono] = Some(SMono.P(mkr(6))); println("  mid") } println("  out")
    println("ouse");   { let o: Option[SMono] = Some(SMono.P(mkr(7))); println(f"  s={o.is_some()}"); println("  mid") } println("  out")
    println("enum");   { let h: H3 = H3.P(SMono.P(mkr(8))); println("  mid") } println("  out")
    println("tuple");  { let t: (SMono, i64) = (SMono.P(mkr(9)), 3); println("  mid") } println("  out")
    println("pushed"); { let w = W { e: SMono.P(mkr(1)), n: 1 }; let mut v: Vec[W] = Vec.new(); v.push(w); println(f"  n={v.len()}"); println("  mid") } println("  out")
    println("ret");    { let x = mkw(2); println("  mid") } println("  out")
    println("reass");  { let mut w = W { e: SMono.P(mkr(3)), n: 1 }; println("  a"); w = W { e: SMono.P(mkr(44)), n: 2 }; println(f"  n={w.n}"); println("  mid") } println("  out")
    println("rebind"); { let v: Vec[SMono] = [SMono.P(mkr(5))]; let x = v; println(f"  n={x.len()}"); println("  mid") } println("  out")
    println("shadow"); { let w = W { e: SMono.P(mkr(6)), n: 1 }; let w = W { e: SMono.P(mkr(77)), n: 2 }; println("  mid") } println("  out")
    println("loop");   { let mut i = 0; while i < 2 { let w = W { e: SMono.P(mkr(i)), n: i }; println(f"  it{i}"); i = i + 1; } println("  mid") } println("  out")
    println("end")
}"#);
    assert_eq!(out, "field\n  d2:10\n  mid\n  out\nfuse\n  n=2\n  d2:10\n  mid\n  out\nfmove\n  eat\n  d2:10\n  mid\n  out\nvec\n  d2:10\n  mid\n  out\nvuse\n  n=2\n  d2:10\n  mid\n  out\nopt\n  d2:10\n  mid\n  out\nouse\n  s=true\n  d2:10\n  mid\n  out\nenum\n  d2:10\n  mid\n  out\ntuple\n  d2:10\n  mid\n  out\npushed\n  n=1\n  d2:10\n  mid\n  out\nret\n  built\n  d2:10\n  mid\n  out\nreass\n  a\n  d2:10\n  n=2\n  d2:11\n  mid\n  out\nrebind\n  n=1\n  d2:10\n  mid\n  out\nshadow\n  d2:10\n  d2:11\n  mid\n  out\nloop\n  d2:10\n  it0\n  d2:10\n  it1\n  mid\n  out\nend\n", "got:\n{out}");
}
