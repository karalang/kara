//! Kata 2086's text arm counted the shared buckets of a street of hamsters
//! as Rust does, `hamsters.matches("H.H").count()`, and was refused "no
//! method 'matches' on type 'Str'", though `match_indices` is there.
//! `matches` is `match_indices` without the offsets: every non-overlapping
//! match, left to right, each a view into the text.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2086_matches_non_overlapping_on_str_and_string() {
    let src = "fn shared(hamsters: Str) -> i64 {
    hamsters.matches(\"H.H\").count()
}

fn main() {
    let s = \"H.H.H.H\".to_string();
    let m: Vec[Str] = \"aaaa\".matches(\"aa\").collect();
    let e = \"h\u{e9}\".matches(\"\").count();
    let words: Vec[String] = \"a1b22c\".matches(\"2\").map(|x| x.to_uppercase()).collect();
    println(f\"{shared(s)} {s.matches(\"H\").count()} {m} {e} {words} {shared(\"x\")}\");
}
";
    assert_eq!(run(src), "2 4 [aa, aa] 3 [2, 2] 0\n");
}
