//! B-2026-09-25-8 -- a bare sequence literal takes the `Array` type a sibling
//! branch has fixed, and the compiled program agrees with `--interp`.

use super::*;

/// B-2026-09-25-8 — before, both programs stopped at typecheck ("match arms
/// have incompatible types" / "if/else branches have incompatible types")
/// because the literal branch was typed `Vec`. Heap `String` elements on the
/// literal side, so a wrong layout cannot print the right text.
#[test]
fn e2e_bare_literal_branch_takes_sibling_array_type() {
    let out = run_program(
        r#"fn mk(n: i64) -> Array[String, 2] { return [f"a{n}", f"b{n}"] }
fn main() {
    let a: Option[Array[String, 1]] = None;
    let r = match a { Some(s) => s, None => [f"heap-string-longer-than-sso-1"] };
    println(f"{r[0]}");
    let c = false;
    let w = if c { mk(1) } else { [f"x{2}", f"y{3}"] };
    println(f"{w[0]}{w[1]}");
    let d: Option[Array[i64, 3]] = None;
    let z = match d { Some(s) => s, None => [9; 3] };
    println(f"{z[2]}");
}
"#,
    );
    assert_eq!(
        out,
        Some("heap-string-longer-than-sso-1\nx2y3\n9\n".to_string()),
        "a bare literal branch must take its sibling's Array type"
    );
}
