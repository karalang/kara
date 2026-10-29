//! A bare `None` in a literal that is a method's receiver takes the element
//! type the call's context decides, as a bound `let none: Option[String]`
//! does (B-2026-10-29-9).

/// The interpreter prints `want` and finishes.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert_eq!(run.output, want);
}

#[test]
fn mir_none_in_a_receiver_takes_the_contexts_type() {
    runs(
        r#"fn mk() -> Vec[Option[String]] { return [None].to_vec(); }
fn main() {
    let v: Vec[Option[String]] = [None].to_vec();
    let w: Vec[Option[String]] = [None, None].to_vec();
    let x: Vec[Option[i64]] = [None, Some(3)].to_vec();
    let y: Option[Option[String]] = Some(None).clone();
    println(f"{v.len()} {w.len()} {mk().len()} {x} {y.is_some()}");
}
"#,
        "1 2 1 [None, Some(3)] true\n",
    );
}
