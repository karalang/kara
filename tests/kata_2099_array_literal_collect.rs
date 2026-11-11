//! Kata 2099's harness lists its largest inputs as one array literal,
//! `[(0..1000).map(..).collect(), Vec.filled(1000, -7), ..]`. A bare
//! `collect()` names no type of its own and takes it from its context, and
//! a branch of an `if` already took it from the other branch (B-2026-10-19-25);
//! an element of an array literal did not take it from its siblings, so
//! `karac check` refused the literal with "an expression whose type does
//! not lower (UnsolvedParam("C"))". It now does, as Rust's does.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2099_collect_element_takes_a_siblings_type() {
    let src = "fn main() {
    let n = 3;
    let lists = [(0..n).map(|i| i * 2).collect(), Vec.filled(2, -7), (5..7).collect()];
    for v in lists.iter() {
        println(f\"{v.len()} {v}\");
    }
    let later = [[1].to_vec(), (0..3).collect(), []];
    println(f\"{later}\");
    let words = [\"ab\".chars().collect(), ['x'].to_vec()];
    println(f\"{words}\");
}
";
    assert_eq!(
        run(src),
        "3 [0, 2, 4]\n2 [-7, -7]\n2 [5, 6]\n[[1], [0, 1, 2], []]\n[[a, b], [x]]\n"
    );
}
