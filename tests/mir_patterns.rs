//! `let` patterns on the MIR pipeline: what a destructuring `let` moves out
//! of its scrutinee, and what it only borrows (core semantics §4.6).

use karac::mir::interp::Outcome;

fn run(src: &str) -> Result<(String, i32), String> {
    let r = karac::mir::lower::run_source(src)?;
    match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => Ok((r.output, code)),
        (Outcome::Error(e), None) => Err(format!("run: {e}\n--- output so far\n{}", r.output)),
        _ => Err("no exit code".into()),
    }
}

fn assert_runs(src: &str, want: &str) {
    match run(src) {
        Ok((out, 0)) if out == want => {}
        other => panic!("want exit 0 and\n{want}--- got {other:?}"),
    }
}

/// A `ref` sub-pattern borrows the part where it is. Over a local whose
/// type runs a `Drop` body, a pattern that only borrows or copies moves
/// nothing, so the local is whole and dies at its scope's end, after the
/// borrow's last use.
#[test]
fn a_ref_sub_pattern_borrows_a_drop_local_in_place() {
    assert_runs(
        r#"
#[derive(Clone)]
struct R { id: i64 }
struct W { r: R, n: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"drop W{self.n}") } }
fn take(w: own W) -> R {
    let W { r: ref r, n } = w;
    println(f"n {n}");
    r.clone()
}
fn main() {
    let r = take(W { r: R { id: 3 }, n: 4 });
    println(r.id);
}
"#,
        "n 4\ndrop W4\n3\n",
    );
}

/// Without a `Drop` body the same pattern binds a reference into the
/// local's part, and a plain binding still moves its part out.
#[test]
fn a_ref_sub_pattern_and_a_move_in_one_let() {
    assert_runs(
        r#"
struct P { a: String, b: String }
fn main() {
    let p = P { a: "x", b: "y" };
    let P { a: ref a, b } = p;
    println(f"{a} {b}");
}
"#,
        "x y\n",
    );
}
