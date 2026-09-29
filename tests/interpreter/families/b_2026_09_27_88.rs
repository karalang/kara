//! B-2026-09-27-88 — a fresh-temp `Option` argument whose payload is a struct
//! laid inline with a `shared` field (`Option[ShP]`, `ShP { i: ShIn, n }`) is
//! owned by the caller exactly when the callee does not take the payload, the
//! question the named spelling asks too (B-2026-09-27-87). It used to be the
//! body channel's consuming question, which counts a returned `p.n` as handing
//! the payload out, and the param-level escape set, which counts `w.is_some()`:
//! either way the temp had no owner and leaked its `shared` field. A callee
//! that hands the whole param back (`return w;`) now leaves the result as the
//! fresh temp's only owner.

use super::*;

/// B-2026-09-27-88 — read-only callees (`Some(p) => p.n` as a tail, a
/// `return p.n`, `let k = p.n`, `w.is_some()`, a field move out of the
/// payload), a pass-through `return w` (fresh, named, nested, handed on, a
/// `None`), each in a loop too, and a pass-through of a payload with a `Drop`
/// field. Before the fix this program leaked 15 blocks at `-O0` (480 B direct).
#[test]
fn test_optres_param_shared_field_payload_not_taken_has_one_owner() {
    let out = run(r#"shared struct ShIn { s: String }
struct ShP { i: ShIn, n: i64 }
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct ShD { r: R, i: ShIn, n: i64 }
fn mk(n: i64) -> ShP { return ShP { i: ShIn { s: f"shared-heap-string-longer-than-sso-{n}" }, n: n }; }
fn mkd(n: i64) -> ShD { return ShD { r: R { id: n }, i: ShIn { s: f"shared-heap-string-longer-than-sso-{n}" }, n: n }; }
fn f1(w: Option[ShP]) -> i64 { match w { Some(p) => p.n, None => 0 } }
fn f2(w: Option[ShP]) -> i64 { match w { Some(p) => { return p.n; } None => { return 0; } } }
fn f3(w: Option[ShP]) -> i64 { match w { Some(p) => { let k = p.n; return k; } None => { return 0; } } }
fn f4(w: Option[ShP]) -> i64 { if w.is_some() { return 1; } return 0; }
fn f5(w: Option[ShP]) -> i64 { if let Some(p) = w { let t = p.i; return p.n; } return 0; }
fn pass(w: Option[ShP]) -> Option[ShP] { return w; }
fn keep(w: Option[ShP]) -> i64 { if let Some(p) = w { return p.n; } return 0; }
fn passd(w: Option[ShD]) -> Option[ShD] { return w; }
fn main() {
    println(f"a{f1(Some(mk(1)))}"); println(f"b{f2(Some(mk(2)))}"); println(f"c{f3(Some(mk(3)))}"); println(f"e{f4(Some(mk(4)))}"); println(f"f{f5(Some(mk(5)))}");
    let a = Some(mk(6)); println(f"a{f1(a)}"); let b = Some(mk(7)); println(f"e{f4(b)}"); let c = Some(mk(8)); println(f"f{f5(c)}");
    let r1 = pass(Some(mk(9))); println(f"p{r1.is_some()}");
    let a2 = Some(mk(10)); let r2 = pass(a2); println(f"p{r2.is_some()}");
    let r3 = pass(pass(Some(mk(11)))); println(f"p{r3.is_some()}");
    let r4 = pass(Some(mk(12))); println(f"k{keep(r4)}");
    let z: Option[ShP] = None; let r5 = pass(z); println(f"p{r5.is_some()}");
    let mut i = 20; while i < 23 { println(f"l{f1(Some(mk(i)))}"); let r = pass(Some(mk(i))); println(f"q{r.is_some()}"); i = i + 1; }
    let s1 = passd(Some(mkd(31))); println(f"s{s1.is_some()}");
    let a3 = Some(mkd(32)); let s2 = passd(a3); println(f"s{s2.is_some()}");
    let s3 = passd(passd(Some(mkd(33)))); println(f"s{s3.is_some()}");
    println("end")
}"#);
    assert_eq!(out, "a1\nb2\nc3\ne1\nf5\na6\ne1\nf8\nptrue\nptrue\nptrue\nk12\npfalse\nl20\nqtrue\nl21\nqtrue\nl22\nqtrue\nstrue\nd31\nstrue\nd32\nstrue\nd33\nend\n");
}
