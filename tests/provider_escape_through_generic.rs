//! core-semantics §14.5: a closure that uses a provider-rooted resource
//! cannot leave its `with_provider`, and that holds when it leaves through a
//! generic function: one that stores the parameter (`keep`) or hands it back
//! (`id`). A generic function that only calls it keeps it in the call
//! (B-2026-11-19-104).

use karac::{check_source, PlaygroundDiagnostic};

const P: &str = "effect resource Clk: ClkP;
trait ClkP { fn now(ref self) -> i64; }
struct F { t: i64 }
impl ClkP for F { fn now(ref self) -> i64 { self.t } }
";

fn errors(body: &str) -> Vec<PlaygroundDiagnostic> {
    check_source(&format!("{P}{body}"))
        .into_iter()
        .filter(|d| d.severity == "error")
        .collect()
}

fn refused(body: &str, why: &str) {
    let es = errors(body);
    assert_eq!(es.len(), 1, "{es:#?}");
    assert!(es[0].message.contains("`Clk`"), "{es:#?}");
    assert!(es[0].message.contains(why), "{es:#?}");
}

#[test]
fn provider_escape_through_a_generic_function_that_stores_it() {
    refused(
        "fn keep[Fx](v: mut ref Vec[Fx], f: own Fx) { v.push(f); }
fn main() { let mut hs: Vec[Fn() -> i64] = Vec.new(); let z = with_provider[Clk](F { t: 1 }, || { keep(mut hs, || Clk.now()); 0 }); println(hs.len() + z); }
",
        "passed to a generic parameter its callee stores",
    );
}

#[test]
fn provider_escape_through_a_generic_function_that_stores_it_by_another() {
    refused(
        "fn keep[Fx](v: mut ref Vec[Fx], f: own Fx) { v.push(f); }
fn relay[G](v: mut ref Vec[G], g: own G) { keep(mut v, g); }
fn main() { let mut hs: Vec[Fn() -> i64] = Vec.new(); let z = with_provider[Clk](F { t: 1 }, || { relay(mut hs, || Clk.now()); 0 }); println(hs.len() + z); }
",
        "passed to a generic parameter its callee stores",
    );
}

#[test]
fn provider_escape_through_a_generic_function_that_returns_it() {
    refused(
        "fn id[T](x: own T) -> T { x }
fn main() { let f = with_provider[Clk](F { t: 1 }, || id(|| Clk.now())); println(f()); }
",
        "returned",
    );
}

#[test]
fn provider_escape_leaves_a_generic_function_that_only_calls_it() {
    for body in [
        "fn ap[Fx: Fn() -> i64](f: Fx) -> i64 { f() }
fn main() { println(with_provider[Clk](F { t: 3 }, || ap(|| Clk.now()) + Clk.now())); }
",
        "fn ap3[Fx: MutFn() -> i64](f: own Fx) -> i64 { let mut h = f; h() + h() }
fn main() { println(with_provider[Clk](F { t: 0 }, || ap3(|| Clk.now()))); }
",
        "fn id[T](x: own T) -> T { x }
fn main() { println(with_provider[Clk](F { t: 2 }, || { let g = id(|| Clk.now()); g() })); }
",
    ] {
        assert!(errors(body).is_empty(), "{body}: {:#?}", errors(body));
    }
}
