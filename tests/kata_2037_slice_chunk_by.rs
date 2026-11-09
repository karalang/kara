//! `v.chunk_by(|a, b| a == b)` on a `Vec` was refused as an iterator
//! adaptor with the machine fix `v.iter().chunk_by(..)`, itself refused
//! (B-2026-11-09-37). The review's ruling (docs/library/collections.md)
//! gives `Slice` Rust's `chunk_by`: the consecutive runs in which `same`
//! accepts each adjacent pair, and no runs for an empty slice.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2037_chunk_by_runs_of_equal_and_rising() {
    let src = "fn main() {
    let v = [1, 1, 2, 3, 3].to_vec();
    let e: Vec[i64] = Vec.new();
    let up = [1, 2, 3, 2, 5].to_vec();
    let lens: Vec[i64] = up.chunk_by(|a, b| a < b).iter().map(|r| r.len()).collect();
    let s: Vec[String] = [\"a\".to_string(), \"a\".to_string(), \"b\".to_string()].to_vec();
    println(f\"{v.chunk_by(|a, b| a == b)} {e.chunk_by(|a, b| a == b).len()} {lens}\");
    println(f\"{s.chunk_by(|x, y| x == y)} {[4, 4, 5].chunk_by(|a, b| a == b)}\");
}
";
    assert_eq!(
        run(src),
        "[[1, 1], [2], [3, 3]] 0 [3, 2]\n[[a, a], [b]] [[4, 4], [5]]\n"
    );
}
