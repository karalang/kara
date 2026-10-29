//! A bare `collect()` inside a tuple that is an `Ok` / `Err` / `Some`
//! payload takes its target from the payload slot, as it does in a bare
//! tuple and directly in the constructor (B-2026-10-28-29).

/// The interpreter prints `want` and finishes.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert_eq!(run.output, want);
}

#[test]
fn mir_collect_in_a_tuple_payload_takes_the_slot() {
    runs(
        r#"fn labels(qs: Vec[f64]) -> Result[(i64, Vec[String]), String] {
    Ok((qs.len(), qs.iter().map(|q| q.to_string()).collect()))
}
fn both(n: i64) -> Option[(Vec[i64], (i64, Vec[i64]))] {
    Some(((0..n).collect(), (n, (0..n).map(|i| i * i).collect())))
}
fn fail(n: i64) -> Result[i64, (String, Vec[i64])] {
    Err(("no", (0..n).collect()))
}
fn main() {
    match labels([0.25, 0.5].to_vec()) {
        Ok((n, l)) => println(f"{n} {l}"),
        Err(e) => println(e),
    }
    let (a, (n, b)) = both(3).unwrap();
    println(f"{a} {n} {b}");
    match fail(2) {
        Ok(v) => println(f"{v}"),
        Err((m, v)) => println(f"{m} {v}"),
    }
}
"#,
        "2 [0.25, 0.5]\n[0, 1, 2] 3 [0, 1, 4]\nno [0, 1]\n",
    );
}
