//! B-2026-09-27-129 — the receiver of an `is_some` / `is_none` / `is_ok` /
//! `is_err` probe that is a fresh temporary dies at the probe: its payload's
//! `Drop` body runs there and its box is freed.

use super::*;

/// B-2026-09-27-129 — `mk2(3).is_some()` printed no `d3` on any surface and
/// leaked the temp's box compiled, while the discarded `mk2(3);` and the named
/// `let o = mk2(3); o.is_some()` were both right. A probe only READS its
/// receiver, so a fresh temp there has no owner after it; it now gets the
/// discard statement's cleanup. Covers `is_some` / `is_none` / `is_ok` /
/// `is_err`, an `if` and a `while` condition, `and`, an inline `Option[R]`, a
/// generic producer, a method producer, a `Some(..)` ctor and `pop()`.
#[test]
fn test_probed_fresh_temp_optres_runs_payload_body() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2(i: i64) -> Option[S] { Some(mk(i)) }
fn mkr(i: i64) -> Option[R] { Some(R { id: i }) }
fn mkok(i: i64) -> Result[S, i64] { Ok(mk(i)) }
fn mkerr(i: i64) -> Result[i64, S] { Err(mk(i)) }
fn wrapo[T](x: T) -> Option[T] { Some(x) }
struct F { n: i64 }
impl F { fn mko(ref self, i: i64) -> Option[S] { Some(mk(i + self.n)) } }
fn main() {
    let a = mk2(1).is_some();
    println(f"a{a}");
    let b = mk2(2).is_none();
    println(f"b{b}");
    if mk2(3).is_some() { println("y3") }
    let c = mkr(4).is_some();
    println(f"c{c}");
    let d = mkok(5).is_ok() and mkerr(6).is_err();
    println(f"d{d}");
    let e = mkok(7).is_err();
    println(f"e{e}");
    let g = wrapo(mk(8)).is_some();
    println(f"g{g}");
    let f = F { n: 100 };
    let h = f.mko(9).is_some();
    println(f"h{h}");
    let k = Some(mk(10)).is_some();
    println(f"k{k}");
    let mut v = vec![mk(11), mk(12)];
    let p = v.pop().is_some();
    println(f"p{p} {v.len()}");
    let mut i = 13;
    while mk2(i).is_some() and i < 15 { i += 1; }
    println(f"i{i}");
    println("end")
}"#);
    assert_eq!(out, "d1\natrue\nd2\nbfalse\nd3\ny3\nd4\nctrue\nd5\nd6\ndtrue\nd7\nefalse\nd8\ngtrue\nd109\nhtrue\nd10\nktrue\nd12\nptrue 1\nd11\nd13\nd14\nd15\ni15\nend\n");
}

/// B-2026-09-27-129 — the shapes the probe must NOT fire for: the borrow
/// accessors (`first` / `get` alias an element the Vec still owns), a named
/// receiver (its own drop runs the body), and a temp that carries an owned
/// param or an owned `self` (under caller-retains the CALLER runs that body,
/// so firing at the probe too printed `d4 d4`). One body each.
#[test]
fn test_probe_leaves_borrowed_named_and_param_view_receivers_alone() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2(i: i64) -> Option[S] { Some(mk(i)) }
fn mk2o(x: S) -> Option[S] { Some(x) }
fn chk(x: S) -> bool { mk2o(x).is_some() }
impl S { fn wrap(self) -> Option[S] { Some(self) } fn probe(self) -> bool { self.wrap().is_some() } }
fn main() {
    let v = vec![mk(1), mk(2)];
    let a = v.first().is_some();
    let b = v.get(1).is_some();
    println(f"a{a} b{b} {v.len()}");
    let o = mk2(3);
    let c = o.is_some();
    println(f"c{c}");
    let t = chk(mk(4));
    println(f"t{t}");
    let u = mk(5).probe();
    println(f"u{u}");
    println("end")
}"#);
    assert_eq!(
        out,
        "atrue btrue 2\nd1\nd2\nd3\nctrue\nd4\nttrue\nd5\nutrue\nend\n"
    );
}
