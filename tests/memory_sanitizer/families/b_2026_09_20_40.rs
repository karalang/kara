//! B-2026-09-20-40 -- a generic `shared enum` whose payload has heap releases
//! its RC box at a plain `let`, with no call of any kind.

use super::*;

/// B-2026-09-20-40 — `let s: S[String] = S.Y(f"..")` leaked its 24 B box plus
/// the buffer, and `S[Array[String, 2]]` its box and both elements, while the
/// monomorphic twin and `S[i64]` were clean. Fixed by B-2026-09-19-53's
/// one-owner rule for a generic `shared enum`'s boxed payload (7faff81eb);
/// that row's fixtures all reach the value through a call, so this pins the
/// plain-`let` spelling the row was filed on.
#[test]
fn asan_generic_shared_enum_at_a_plain_let_releases_its_box() {
    assert_clean_asan_run(
        r#"
shared enum S[T] { Y(T), N }
shared enum M { Y(String), N }
fn main() {
    let s: S[String] = S.Y(f"ccccccccccccccccc{38}");
    println("a");
    let t: S[Array[String, 2]] = S.Y([f"x-{1}", f"y-{2}"]);
    println("b");
    let u: S[String] = S.Y(f"taken-{3}");
    match u { S.Y(v) => println(f"c:{v}"), S.N => println("c:none") }
    let w: S[i64] = S.Y(7);
    let m: M = M.Y(f"mono-{4}");
    println("end");
}
"#,
        &["a", "b", "c:taken-3", "end"],
        "asan_generic_shared_enum_at_a_plain_let_releases_its_box",
    );
}
