//! B-2026-09-28-4 — a fresh `Option`/`Result` temp that is a param VIEW
//! (`mk2o(x)` or `Some(x)` inside `fn c(x: S)`) leaves `x`'s `Drop` body to the
//! caller in every position that owns a temp with no binding.

use super::*;

/// B-2026-09-28-4 — the discard statement, `let _`, a by-value argument to a
/// free fn, a method and an associated fn, an `is_*` probe, a rebound param,
/// and an owned `self` each ran the payload's body while the caller ran it
/// again (`d1 in1 d1` where one `d1` is due). A temp that only reads the param
/// (`mk2(x.r.id + 100)`) is fresh and still runs its own body.
#[test]
fn test_param_view_temp_leaves_body_to_caller() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2(i: i64) -> Option[S] { Some(mk(i)) }
fn mk2o(x: S) -> Option[S] { Some(x) }
fn eat(o: Option[S]) { println("eat") }
struct E { n: i64 }
impl E {
    fn eat(ref self, o: Option[S]) { println("meat") }
    fn aeat(o: Option[S]) { println("aeat") }
}
impl S {
    fn wrap(self) -> Option[S] { Some(self) }
    fn probe(self) -> bool { self.wrap().is_some() }
}
fn c1(x: S) { mk2o(x); println("in1") }
fn c2(x: S) { let _ = mk2o(x); println("in2") }
fn c3(x: S) { let _ = Some(x); println("in3") }
fn c4(x: S) { eat(mk2o(x)); println("in4") }
fn c5(x: S) { let e = E { n: 0 }; e.eat(mk2o(x)); println("in5") }
fn c6(x: S) { E.aeat(mk2o(x)); println("in6") }
fn c7(x: S) -> bool { mk2o(x).is_some() }
fn c8(x: S) -> bool { let y = x; mk2o(y).is_some() }
fn c9(x: S) -> bool { mk2(x.r.id + 100).is_some() }
fn main() {
    c1(mk(1));
    println("o1");
    c2(mk(2));
    println("o2");
    c3(mk(3));
    println("o3");
    c4(mk(4));
    println("o4");
    c5(mk(5));
    println("o5");
    c6(mk(6));
    println("o6");
    let a = c7(mk(7));
    println(f"a{a}");
    let b = c8(mk(8));
    println(f"b{b}");
    let c = c9(mk(9));
    println(f"c{c}");
    let d = mk(10).probe();
    println(f"d{d}");
    println("end")
}"#);
    assert_eq!(out, "in1\nd1\no1\nin2\nd2\no2\nin3\nd3\no3\neat\nin4\nd4\no4\nmeat\nin5\nd5\no5\naeat\nin6\nd6\no6\nd7\natrue\nd8\nbtrue\nd109\nd9\nctrue\nd10\ndtrue\nend\n");
}

/// B-2026-09-28-4 — the `if let` / `while let` / `let … else` miss edges and
/// a `match` over the same param-view temp.
#[test]
fn test_param_view_scrutinee_temp_leaves_body_to_caller() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn mk2o(x: S) -> Option[S] { Some(x) }
fn s1(x: S) -> bool { if let None = mk2o(x) { false } else { true } }
fn s2(x: S) { while let Some(_) = mk2o(x) { break; } println("in2") }
fn s3(x: S) -> bool { let Some(_) = mk2o(x) else { return false }; true }
fn s4(x: S) { match mk2o(x) { Some(_) => println("m"), None => println("n") } println("in4") }
fn main() {
    let a = s1(mk(1));
    println(f"a{a}");
    s2(mk(2));
    println("o2");
    let c = s3(mk(3));
    println(f"c{c}");
    s4(mk(4));
    println("o4");
    println("end")
}"#);
    assert_eq!(
        out,
        "d1\natrue\nin2\nd2\no2\nd3\nctrue\nm\nin4\nd4\no4\nend\n"
    );
}
