//! B-2026-09-28-48: a by-value Option/Result param whose arm moves the payload into a local that only reads it.

use super::*;

/// B-2026-09-28-48 — `Some(y) => { let z = y; .. }` over a by-value boxed
/// `Option[S]`, and the `Result` twin: the local only reads, so the payload
/// stays with the caller, whose walk runs each body once after the call.
#[test]
fn e2e_optres_arm_payload_rebound_into_a_local_runs_its_body_once_in_the_caller() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct Q { r: R }
fn mq(i: i64) -> Q { Q { r: R { id: i } } }

fn go(x: Option[S]) { match x { Some(y) => { let z = y; println(f"z{z.r.id}"); println("w") }, None => println("n") }; println("post") }
fn gr(x: Result[S, i64]) { match x { Ok(y) => { let z = y; println(f"z{z.r.id}"); println("w") }, Err(e) => println("e") }; println("post") }
fn main() {
    go(Some(mk(1)));
    let b = Some(mk(2));
    go(b);
    println("mid");
    gr(Ok(mk(3)));
    let c: Result[S, i64] = Ok(mk(4));
    gr(c);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "z1\nw\npost\nd1\nz2\nw\npost\nd2\nmid\nz3\nw\npost\nd3\nz4\nw\npost\nd4\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-28-48 — the `if let` spellings, and an associated function.
#[test]
fn e2e_optres_arm_payload_rebound_into_a_local_by_if_let_or_associated_fn() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct Q { r: R }
fn mq(i: i64) -> Q { Q { r: R { id: i } } }

struct H { v: i64 }
impl H { fn ga(x: Option[S]) { match x { Some(y) => { let z = y; println(f"a{z.r.id}") }, None => println("n") }; println("post") } }
fn io(x: Option[S]) { if let Some(y) = x { let z = y; println(f"i{z.r.id}") }; println("post") }
fn ir(x: Result[S, i64]) { if let Ok(y) = x { let z = y; println(f"r{z.r.id}") }; println("post") }
fn main() {
    io(Some(mk(1)));
    let b = Some(mk(2));
    io(b);
    ir(Ok(mk(3)));
    let c: Result[S, i64] = Ok(mk(4));
    ir(c);
    H.ga(Some(mk(5)));
    let d = Some(mk(6));
    H.ga(d);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "i1\npost\nd1\ni2\npost\nd2\nr3\npost\nd3\nr4\npost\nd4\na5\npost\nd5\na6\npost\nd6\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-28-48 — behind a forwarding frame, and an unboxed payload.
#[test]
fn e2e_optres_arm_payload_rebound_into_a_local_through_a_forward_or_unboxed() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct S { r: R, s: String }
fn mk(i: i64) -> S { S { r: R { id: i }, s: f"heap-string-longer-than-sso-{i}" } }
struct Q { r: R }
fn mq(i: i64) -> Q { Q { r: R { id: i } } }

fn go(x: Option[S]) { match x { Some(y) => { let z = y; println(f"z{z.r.id}") }, None => println("n") }; println("post") }
fn fw(a: Option[S]) { go(a); println("after") }
fn qo(x: Option[Q]) { match x { Some(y) => { let z = y; println(f"q{z.r.id}") }, None => println("n") } }
fn qr(x: Result[Q, i64]) { match x { Ok(y) => { let z = y; println(f"k{z.r.id}") }, Err(e) => println("e") } }
fn main() {
    fw(Some(mk(1)));
    qo(Some(mq(2)));
    let b = Some(mq(3));
    qo(b);
    qr(Ok(mq(4)));
    let c: Result[Q, i64] = Ok(mq(5));
    qr(c);
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "z1\npost\nafter\nd1\nq2\nd2\nq3\nd3\nk4\nd4\nk5\nd5\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-28-48 follow-up — a payload whose type declares its OWN `Drop`,
/// rebound whole into a reading local (`Some(r) => { let z = r; .. }`), from a
/// fresh temp, a `Result`, an `if let` and a named argument. The caller's and
/// callee's escape questions reach different branches for such a payload, and
/// following the rebind in only one lost the body; the rebind also took the
/// memory the caller's box drop frees, a double free at `-O0` on every tree.
#[test]
fn e2e_own_drop_optres_payload_rebound_into_a_local_runs_its_body_once() {
    let Some(out) = run_program(
        r#"struct W { a: String, b: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}") } }
fn mkw(i: i64) -> W { W { a: f"a{i}", b: f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb{i}" } }
fn n4(x: Option[W]) { match x { Some(r) => { let z = r; println("n4") }, None => {} } }
fn nr(x: Result[W, i64]) { match x { Ok(r) => { let z = r; println(f"r{z.b.len()}") }, Err(e) => {} } }
fn io(x: Option[W]) { if let Some(r) = x { let z = r; println("io") } }
fn t1() { n4(Option.Some(mkw(1))) }
fn t2() { nr(Ok(mkw(22))) }
fn t3() { io(Some(mkw(333))) }
fn t4() { let o = Some(mkw(4444)); n4(o) }
fn main() { t1(); println("a"); t2(); println("b"); t3(); println("c"); t4(); println("end") }
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "n4\ndW2\na\nr38\ndW3\nb\nio\ndW4\nc\nn4\ndW5\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-28-48 follow-up — the same own-`Drop` payload through a two-step
/// rebind (`let z = r; let y = z;`), a clone handed out, the local returned, an
/// associated function, a named argument and a loop.
#[test]
fn e2e_own_drop_optres_payload_rebound_twice_returned_or_in_a_loop() {
    let Some(out) = run_program(
        r#"struct In { s: String }
struct W { a: String, b: String, i: In }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}:{self.i.s}") } }
fn mkw(i: i64) -> W { W { a: f"a{i}", b: f"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb{i}", i: In { s: f"in{i}" } } }
fn chain(x: Option[W]) { match x { Some(r) => { let z = r; let y = z; println(f"c{y.b.len()}") }, None => {} }; println("cpost") }
fn gives(x: Option[W]) -> String { match x { Some(r) => { let z = r; z.b.clone() }, None => "none" } }
fn keep(x: Option[W]) -> W { match x { Some(r) => { let z = r; z }, None => mkw(0) } }
struct H { v: i64 }
impl H { fn m(x: Option[W]) { match x { Some(r) => { let z = r; println("hm") }, None => {} } } }
fn lp(n: i64) { let mut i = 0; while i < n { match Option.Some(mkw(i)) { Some(r) => { let z = r; println("lp") }, None => {} }; i = i + 1; } }
fn t1() { chain(Some(mkw(1))) }
fn t2() { let s = gives(Some(mkw(22))); println(s.len()) }
fn t3() { let w = keep(Some(mkw(333))); println("kept") }
fn t4() { H.m(Some(mkw(4444))) }
fn t5() { let o = Some(mkw(55555)); chain(o) }
fn main() { t1(); println("a"); t2(); println("b"); t3(); println("c"); t4(); println("d"); t5(); lp(2); println("end") }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "c37\ncpost\ndW2:in1\na\ndW3:in22\n38\nb\ndW4:in333\nkept\nc\nhm\ndW5:in4444\nd\nc41\ncpost\ndW6:in55555\ndW2:in0\nlp\ndW2:in1\nlp\nend\n", "got:\n{out}");
}
