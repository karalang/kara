//! Kata 2102's harness listed its runs as `let examples: Vec[Vec[(String,
//! i64)]] = [a.collect(), b.collect(), ..].to_vec();`. Each bare
//! `collect()` names no type of its own; the `let` names it, through
//! `to_vec`, whose receiver the context then refines. The refinement
//! reached the elements' types but not the `collect` calls' own type
//! argument, so `karac check` stopped at "the method call `.collect` with
//! no resolved callee is not lowered yet". It now reaches the calls too.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2102_to_vec_of_bare_collects_lowers() {
    let src = "fn main() {
    let xs: Vec[Vec[i64]] = [[1, 2].iter().map(|x| x * 10).collect(), [3].iter().map(|x| x + 1).collect()].to_vec();
    println(f\"{xs}\");
}
";
    assert_eq!(run(src), "[[10, 20], [4]]\n");
}

#[test]
fn kata_2102_to_vec_of_collected_tuples_lowers() {
    let src = "fn main() {
    let runs: Vec[Vec[(String, i64)]] = [
        [(\"a\", 1), (\"b\", 2)].iter().map(|(n, s)| (n.to_string(), s)).collect(),
        [(\"c\", 3)].iter().map(|(n, s)| (n.to_string(), s * 10)).collect(),
    ].to_vec();
    for run in runs.iter() {
        println(f\"{run.len()} {run[0].0} {run[run.len() - 1].1}\");
    }
}
";
    assert_eq!(run(src), "2 a 2\n1 c 30\n");
}
