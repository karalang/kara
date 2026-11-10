//! Kata 2070's Rust arm, LeetCode's template, answers each query with
//! `queries.iter().map(|q| { ..; if idx == 0 { 0 } else { best[idx - 1] } })
//! .collect()` into a `Vec[i32]`, `best` a `Vec[i32]`. The closure's tail
//! was checked against `map`'s own `OutV`, which nothing had fixed, so the
//! first branch's literal settled it to `i64` before the `i32` branch was
//! seen, and the collect was refused "`Vec[i32]` implements
//! `FromIterator[i32]`, not `FromIterator[i64]`" with no fix. The branches
//! swapped, or the `if` bound by a `let` first, checked. An `if` or `match`
//! checked against an open slot is now synthesized, its branches joined
//! first as in a `let`.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn kata_2070_closure_tail_if_takes_the_typed_branch() {
    let src = "fn main() {
    let best: Vec[i32] = [3i32, 4i32].to_vec();
    let qs: Vec[i64] = [0, 1, 2].to_vec();
    let a: Vec[i32] = qs.iter().map(|q| if q == 0 { 0 } else { best[q - 1] }).collect();
    let b: Vec[i32] = qs.iter().map(|q| match q { 0 => 0, _ => best[q - 1] }).collect();
    let c: Vec[i64] = qs.iter().map(|q| if q == 0 { 7 } else { 5 }).collect();
    let e = qs.iter().map(|q| if q == 0 { 1 } else { 2 }).sum();
    println(f\"{a} {b} {c} {e}\");
}
";
    assert_eq!(run(src), "[0, 3, 4] [0, 3, 4] [7, 5, 5] 5\n");
}

#[test]
fn kata_2070_generic_fn_closure_tail_if_takes_the_typed_branch() {
    let src = "fn ap[U](f: Fn(i64) -> U) -> U {
    f(1)
}

fn main() {
    let best: Vec[i32] = [3i32, 4i32].to_vec();
    let b = ap(|q| if q == 0 { 0 } else { best[q] });
    let c: i32 = b;
    println(f\"{c}\");
}
";
    assert_eq!(run(src), "4\n");
}
