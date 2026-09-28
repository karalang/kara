//! B-2026-09-28-24: a temporary `Option` / `Result` argument forwarded to a method or generic consumer.

use super::*;

/// B-2026-09-28-24 — B-2026-09-28-10's forward when the consumer is an
/// instance METHOD, an ASSOCIATED function or a GENERIC free function (bare
/// `T` and `Option[T]`). Each keeps nothing, so the temporary's body runs in
/// the forwarding frame; the lent hook answered only a bare-identifier call to
/// a non-generic free function, so every compiled surface lost these bodies.
/// A named local, and `None`, alongside.
#[test]
fn interp_temp_option_arg_forwarded_to_a_method_or_generic_consumer() {
    let out = run(r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn geat[T](t: T) { println("gx") }
fn geo[T](o: Option[T]) { println("go") }
struct K { k: i64 }
impl K {
    fn eato(ref self, o: Option[S]) { println("mo") }
    fn reado(ref self, o: Option[S]) { match o { Option.Some(s) => println(f"r{s.id}"), Option.None => println("n") } }
    fn takeo(ref self, o: Option[S], v: mut ref Vec[S]) { match o { Option.Some(s) => v.push(s), Option.None => println("n") } }
    fn aeo(o: Option[S]) { println("ao") }
}
fn f1(h: Option[S], q: K) { q.eato(h); println("after") }
fn f2(h: Option[S], q: K) { q.reado(h); println("after") }
fn f3(h: Option[S]) { K.aeo(h); println("after") }
fn f4(h: Option[S]) { geat(h); println("after") }
fn f5(h: Option[S]) { geo(h); println("after") }
fn main() {
    f1(Option.Some(mks(1)), K { k: 0 }); f2(Option.Some(mks(2)), K { k: 0 }); f3(Option.Some(mks(3)));
    f4(Option.Some(mks(4))); f5(Option.Some(mks(5)));
    let a = Option.Some(mks(6)); f2(a, K { k: 0 }); let b = Option.Some(mks(7)); f5(b); f2(Option.None, K { k: 0 });
    println("end")
}
"#);
    assert_eq!(out, "mo\nafter\ndS1\nr2\nafter\ndS2\nao\nafter\ndS3\ngx\nafter\ndS4\ngo\nafter\ndS5\nr6\nafter\ndS6\ngo\nafter\ndS7\nn\nafter\nend\n");
}

/// B-2026-09-28-24 — the `Result` twin: a method that only reads the
/// payload, an associated function and a generic bare-`T` function.
#[test]
fn interp_temp_result_arg_forwarded_to_a_method_or_generic_consumer() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
fn geat[T](t: T) { println("gx") }
struct K { k: i64 }
impl K {
    fn peek(ref self, x: Result[S, i64]) { match x { Ok(y) => println(f"p{y.r.id}"), Err(e) => println("e") } }
    fn ae(x: Result[S, i64]) { println("ae") }
}
fn f1(a: Result[S, i64], q: K) { q.peek(a); println("after") }
fn f2(a: Result[S, i64]) { K.ae(a); println("after") }
fn f3(a: Result[S, i64]) { geat(a); println("after") }
fn main() {
    f1(Ok(mk(1)), K { k: 0 }); f2(Ok(mk(2))); f3(Ok(mk(3)));
    let b: Result[S, i64] = Ok(mk(4)); f1(b, K { k: 0 }); f1(Err(9), K { k: 0 });
    println("end")
}
"#);
    assert_eq!(
        out,
        "p1\nafter\nd1\nae\nafter\nd2\ngx\nafter\nd3\np4\nafter\nd4\ne\nafter\nend\n"
    );
}

// The keeping-method control lives in the compiled suites only: the
// interpreter runs that payload body twice, which is B-2026-09-28-25.
