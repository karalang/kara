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
fn asan_plain_holder_of_drop_relevant_shared_releases_at_last_use_clean() {
    assert_clean_asan_run(
        r#"struct R2 { s: String, t: String, u: String }
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
}"#,
        &[
            "field", "  d2:10", "  mid", "  out", "fuse", "  n=2", "  d2:10", "  mid", "  out",
            "fmove", "  eat", "  d2:10", "  mid", "  out", "vec", "  d2:10", "  mid", "  out",
            "vuse", "  n=2", "  d2:10", "  mid", "  out", "opt", "  d2:10", "  mid", "  out",
            "ouse", "  s=true", "  d2:10", "  mid", "  out", "enum", "  d2:10", "  mid", "  out",
            "tuple", "  d2:10", "  mid", "  out", "pushed", "  n=1", "  d2:10", "  mid", "  out",
            "ret", "  built", "  d2:10", "  mid", "  out", "reass", "  a", "  d2:10", "  n=2",
            "  d2:11", "  mid", "  out", "rebind", "  n=1", "  d2:10", "  mid", "  out", "shadow",
            "  d2:10", "  d2:11", "  mid", "  out", "loop", "  d2:10", "  it0", "  d2:10", "  it1",
            "  mid", "  out", "end",
        ],
        "plain_holder_of_drop_relevant_shared",
    );
}

/// B-2026-09-19-18 — the holder a `let` pattern DESTRUCTURES keeps its release
/// at lexical scope exit. `let (s, n) = t` and `let W { e, n } = w` leave `s` /
/// `e` as bit-copy views of the holder's part, so the holder's own last use
/// (the destructure) is too early: releasing there freed the `SMono` that
/// `show(s)` then read (`Invalid read of size 8`, and `q` printed for `p10`,
/// measured on the first draft of the fix). `--interp` runs the body before
/// `mid`, at the views' last use; this side stays after it, which is the
/// placement this row's fix does not reach (filed with its remainder).
#[test]
fn asan_destructured_holder_of_shared_enum_keeps_its_release_clean() {
    assert_clean_asan_run(
        r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
shared enum SMono { P(R2), Q }
fn mkr(n: i64) -> R2 { R2 { s: f"aaaaaaaa-{n}", t: f"t{n}", u: f"u{n}" } }
struct W { e: SMono, n: i64 }
fn show(s: SMono) { match s { SMono.P(r) => println(f"  p{r.s.len()}"), SMono.Q => println("  q") } }

fn main() {
    println("tuple"); { let t: (SMono, i64) = (SMono.P(mkr(1)), 3); let (s, n) = t; println(f"  n={n}"); show(s); println("  mid") } println("  out")
    println("struct"); { let w = W { e: SMono.P(mkr(2)), n: 2 }; let W { e, n } = w; println(f"  n={n}"); show(e); println("  mid") } println("  out")
    println("end")
}"#,
        &[
            "tuple", "  n=3", "  p10", "  mid", "  d2:10", "  out", "struct", "  n=2", "  p10",
            "  mid", "  d2:10", "  out", "end",
        ],
        "destructured_holder_of_shared_enum",
    );
}
