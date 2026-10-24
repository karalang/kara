//! `.into()` spelled straight on a unit variant (B-2026-10-24-8):
//! `A.Y.into()` parses as a call of the path `A.Y.into`, which the
//! checker resolves as a conversion through the user's `From` impl, with
//! no callee; the builder converts the variant the path names.

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

/// An owning `from` into another enum, beside the parenthesised spelling
/// that already lowered, and a borrowing `from` into a struct.
#[test]
fn a_unit_variant_converts_through_its_from_impl() {
    let src = r#"
enum A { X, Y }
enum B { P, Q }
struct N { v: i64 }
impl From[A] for B {
    fn from(a: own A) -> B { match a { A.X => B.P, A.Y => B.Q } }
}
impl From[A] for N {
    fn from(a: A) -> N { match a { A.X => N { v: 1 }, A.Y => N { v: 2 } } }
}
fn main() {
    let b: B = A.Y.into();
    match b { B.P => println("P"), B.Q => println("Q") }
    let c: B = (A.X).into();
    match c { B.P => println("P"), B.Q => println("Q") }
    let n: N = A.Y.into();
    println(n.v);
}
"#;
    match run(src) {
        Ok((out, 0)) if out == "Q\nP\n2\n" => {}
        other => panic!("want exit 0 and Q P 2, got {other:?}"),
    }
}
