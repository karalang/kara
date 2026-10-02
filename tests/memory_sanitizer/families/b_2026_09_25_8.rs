//! B-2026-09-25-8 -- a bare sequence literal typed `Array` by a sibling
//! branch owns its elements' heap exactly once.

use super::*;

/// B-2026-09-25-8 — the literal arm's heap `String`s and the taken arm's
/// payload are each freed once, on both arms.
#[test]
fn asan_bare_literal_branch_array_frees_elements_once() {
    assert_clean_asan_run(
        r#"fn main() {
    let a: Option[Array[String, 2]] = None;
    let r = match a { Some(s) => s, None => [f"aaaaaaaaaaaaaaaaaaaaaaaaaaa{1}", f"bbbbbbbbbbbbbbbbbbbbbbbbbbbb{2}"] };
    println(f"{r[0]}{r[1]}");
    let b: Option[Array[String, 2]] = Some([f"ccccccccccccccccccccccccccc{3}", f"ddddddddddddddddddddddddddd{4}"]);
    let q = match b { Some(s) => s, None => [f"e{5}", f"f{6}"] };
    println(f"{q[0]}{q[1]}");
}
"#,
        &[
            "aaaaaaaaaaaaaaaaaaaaaaaaaaa1bbbbbbbbbbbbbbbbbbbbbbbbbbbb2",
            "ccccccccccccccccccccccccccc3ddddddddddddddddddddddddddd4",
        ],
        "B-2026-09-25-8 sibling-typed Array literal arm",
    );
}
