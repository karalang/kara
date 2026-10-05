//! B-2026-10-05-44 -- `.clone()` on a generic-typed binding inside a
//! monomorph had no handler when compiled: a `ref T` param bound to a `Vec`
//! from a temporary argument (`rc(mkv())`), and a leaf of a destructuring `let`
//! (`let (v, n) = (x.clone(), 1); v.clone()`), where `--interp` ran both.

use super::*;

/// The clones are freed once each, and the temporaries behind the `ref T`
/// args are freed once.
#[test]
fn asan_generic_clone_on_ref_temp_and_destructured_leaf() {
    assert_clean_asan_run(
        r#"fn mkv() -> Vec[i64] { vec![1, 2, 3] }
fn rc[T: Clone](x: ref T) -> T { x.clone() }
fn pair[T: Clone](x: ref T) -> T {
    let (v, n) = (x.clone(), 1);
    let w = v.clone();
    w
}
fn tail[T: Clone](x: ref T) -> T {
    let (v, n) = (x.clone(), 1);
    v.clone()
}
fn main() {
    let d = rc(mkv());
    println(f"a:{d.len()} {d[2]}");
    let e = rc(vec![4, 5]);
    println(f"b:{e.len()}");
    println(f"c:{pair("ab".to_string())} {tail("cd".to_string())} {tail(7)}");
    let p = pair(mkv());
    println(f"d:{p.len()}");
}
"#,
        &["a:3 3", "b:2", "c:ab cd 7", "d:3"],
        "asan_generic_clone_on_ref_temp_and_destructured_leaf",
    );
}
