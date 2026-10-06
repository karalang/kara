//! B-2026-10-06-43 -- a `const` of `Array` or `Vec` type used directly as an
//! index base, a `.len()` receiver or a `for` iterable failed `karac build`.
//! Every use compiles the const's value afresh, so each read owns (and must
//! free) its own copy, and binding the const to a local must not copy it a
//! second time as though it had been moved.

use super::*;

#[test]
fn asan_const_vec_and_array_reads_are_balanced() {
    assert_clean_asan_run(
        r#"const NUMS: Vec[i64] = [10, 20, 30];
const NAMES: Vec[String] = ["ab".to_string(), "cd".to_string()];
const CW: Array[String, 2] = ["x", "yy"];
let WORDS: Array[String, 2] = ["a", "bb"];
fn main() {
    let v = NUMS;
    let w = NUMS[0];
    let mut k = 0;
    for nm in NAMES { k += nm.len(); }
    for nm in NAMES.iter() { k += nm.len(); }
    let mut out: Vec[String] = Vec.new();
    for w2 in WORDS { out.push(w2.clone()); }
    for c in CW { out.push(c.clone()); }
    out.push(NAMES[1]);
    let mut i = 0;
    while i < 2 { k += NAMES[1].len() + WORDS[1].len() + CW[0].clone().len(); i += 1; }
    println(f"{v[2]} {w} {k} {NAMES.len()} {NAMES[0]} {out.len()} {out[4]} {CW.len()}");
}
"#,
        &["30 10 18 2 ab 5 cd 2"],
        "asan_const_vec_and_array_reads_are_balanced",
    );
}
