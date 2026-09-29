//! B-2026-09-19-29 -- a `shared enum` held by several holders runs its
//! payload's `Drop` body once, when the last holder lets it go.

use super::*;

/// B-2026-09-19-29 — two, three, or staggered holders of ONE `shared enum`
/// value, in every holder position (struct field, tuple element, `Vec`
/// element, `Option` payload, plain-enum payload), plus a rebinding, a
/// callee that returns its holder, a by-value callee, and an inner block.
/// Before the fix the interpreter deep-copied the value into each holder and
/// ran the body once per holder (`A d2:9 d2:9 ok`); every compiled surface
/// runs it once, at the last release. Same source and string as the codegen
/// twin `e2e_shared_enum_many_holders_release_once`.
#[test]
fn test_shared_enum_many_holders_release_once() {
    let out = run(r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
shared enum SMono { P(R2), Q }
struct Hs { m: SMono }
enum H3 { P(SMono), Q }
fn mkr(n: i64) -> R2 { R2 { s: "abcdefghi", t: "x", u: "y" } }
fn mk() -> SMono { SMono.P(mkr(1)) }
fn f(h: Hs) { println("  in"); println("  in2") }
fn g(s: SMono) { println("  gin") }
fn keep(h: Hs) -> Hs { println("  k"); h }
fn fu(h: Hs) { match h.m { SMono.P(r) => println(f"  r{r.t}"), SMono.Q => println("  q") }; println("  in2") }
fn main() {
    println("field"); { let s: SMono = mk(); println("  A"); let a: Hs = Hs { m: s }; let b: Hs = Hs { m: s }; println("  ok") }
    println("three"); { let s: SMono = mk(); let a: Hs = Hs { m: s }; let b: Hs = Hs { m: s }; let c: Hs = Hs { m: s }; println("  ok") }
    println("tuple"); { let s: SMono = mk(); let a = (s, 1); let b = (s, 2); println(f"  ok{a.1}{b.1}") }
    println("vec"); { let s: SMono = mk(); let v: Vec[SMono] = [s, s]; println(f"  ok{v.len()}") }
    println("option"); { let s: SMono = mk(); let a: Option[SMono] = Some(s); let b: Option[SMono] = Some(s); println("  ok") }
    println("enum"); { let s: SMono = mk(); let a: H3 = H3.P(s); let b: H3 = H3.P(s); println("  ok") }
    println("staggered"); {
        let s: SMono = mk();
        let a: Hs = Hs { m: s };
        let b: Hs = Hs { m: s };
        println("  ok");
        match a.m { SMono.P(r) => println(f"  a{r.t}"), SMono.Q => println("  q") }
        println("  mid");
        match b.m { SMono.P(r) => println(f"  b{r.t}"), SMono.Q => println("  q") }
        println("  end")
    }
    println("rebind"); { let s: SMono = mk(); let b = s; println("  ok") }
    println("returned"); { let s: SMono = mk(); let a: Hs = Hs { m: s }; let b: Hs = keep(a); println("  after"); let c = b; println("  end") }
    println("callee"); { let s: SMono = mk(); let a: Hs = Hs { m: s }; f(a); println("  after"); g(s); println("  end") }
    println("calleeuse"); { let s: SMono = mk(); let a: Hs = Hs { m: s }; fu(a); println("  after") }
    println("inner"); { let s: SMono = mk(); { let a: Hs = Hs { m: s }; println("  inner") }; println("  after") }
    println("done")
}
"#);
    assert_eq!(out, "field\n  A\n  d2:9\n  ok\nthree\n  d2:9\n  ok\ntuple\n  ok12\n  d2:9\nvec\n  ok2\n  d2:9\noption\n  d2:9\n  ok\nenum\n  d2:9\n  ok\nstaggered\n  ok\n  ax\n  mid\n  bx\n  d2:9\n  end\nrebind\n  d2:9\n  ok\nreturned\n  k\n  after\n  d2:9\n  end\ncallee\n  in\n  in2\n  after\n  gin\n  d2:9\n  end\ncalleeuse\n  rx\n  in2\n  d2:9\n  after\ninner\n  inner\n  d2:9\n  after\ndone\n", "got:
{out}");
}

/// B-2026-09-27-64 — a FRESH `shared enum` temp handed to a by-value callee
/// or used as an owned-`self` receiver is released when the enclosing
/// statement is done with it, after the statement prints the result — the
/// compiled placement. Before the refcount the interpreter ran the body when
/// the callee returned (`dR28 x28` against `x28 dR28`). Codegen twin:
/// `e2e_fresh_shared_enum_temp_released_after_statement`.
#[test]
fn test_fresh_shared_enum_temp_released_after_statement() {
    let out = run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
shared enum Sh { A(R), B }
impl Sh { fn read(self) -> i64 { match self { Sh.A(r) => { return r.id; } Sh.B => { return 0; } } }
          fn none(self) -> i64 { return 5; } }
fn mk(n: i64) -> R { return R { id: n, tag: "t", xs: [1] } }
fn rd(s: Sh) -> i64 { match s { Sh.A(r) => { return r.id; } Sh.B => { return 0; } } }
fn main() {
    println("ptmp");  { println(f"  x{rd(Sh.A(mk(28)))}"); println("  after") }
    println("rtmp");  { println(f"  x{Sh.A(mk(29)).read()}"); println("  after") }
    println("ntmp");  { println(f"  x{Sh.A(mk(31)).none()}"); println("  after") }
}
"#);
    assert_eq!(
        out,
        "ptmp\n  x28\n  dR28\n  after\nrtmp\n  x29\n  dR29\n  after\nntmp\n  x5\n  dR31\n  after\n",
        "got:
{out}"
    );
}
