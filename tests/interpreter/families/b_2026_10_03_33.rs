//! B-2026-10-03-33 -- comparing a signed with an unsigned integer is a type
//! error, as design.md § Integer overflow says, and as the arithmetic
//! operators already enforced.

use super::*;

fn type_errors(src: &str) -> Vec<String> {
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    typed.errors.iter().map(|e| e.to_string()).collect()
}

/// Before, every line type-checked and the backends then disagreed: the
/// interpreter compared values (`3 > -1` true) and compiled code compared at
/// one width and signedness (`-1` read as 255, false). `u == k` and `w == j`
/// diverged the same way. A negated literal outside `u8`'s range is not
/// promoted (B-2026-09-29-94), so `u > -1` is a mixed comparison too.
#[test]
fn test_mixed_signedness_comparison_is_a_type_error() {
    let errs = type_errors(
        r#"fn main() {
    let u: u8 = 3;
    let k: i64 = -1;
    println(f"{u > k}");
    println(f"{u == k}");
    println(f"{u > -1}");
    let s: i32 = -5;
    let t: u32 = 4000000000;
    println(f"{s < t}");
    println(f"{t != s}");
}
"#,
    );
    assert_eq!(errs.len(), 5, "{errs:?}");
    for (e, (l, r)) in errs.iter().zip([
        ("u8", "i64"),
        ("u8", "i64"),
        ("u8", "i64"),
        ("i32", "u32"),
        ("u32", "i32"),
    ]) {
        assert!(
            e.contains(&format!("cannot compare '{l}' and '{r}'"))
                && e.contains("mixing signed and unsigned"),
            "{errs:?}"
        );
    }
}

/// Same-signedness comparisons, a literal beside an unsigned value, and an
/// explicit cast stay accepted and print the same answers they always did.
#[test]
fn test_same_signedness_and_cast_comparisons_still_check() {
    let src = r#"fn main() {
    let u: u8 = 3;
    let k: i64 = -1;
    let w: u64 = 7;
    let b: i8 = -2;
    println(f"{u > 0} {u == 3} {w > 6} {b < k} {(u as i64) > k} {u > (k as u8)}");
}
"#;
    let errs = type_errors(src);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(run(src), "true true true true true false\n");
}

/// A literal beside a BORROWED integer: a match over `ref Tok` binds `b` as
/// a `ref u8`, which literal promotion does not see through, so `43` keeps
/// its default `i64` at the check. It must stay accepted (the self-host
/// corpus has this shape), while an out-of-range negated literal and a
/// signed variable beside the same borrow are still rejected.
#[test]
fn test_literal_beside_a_borrowed_unsigned_still_checks() {
    let ok = r#"enum Tok { Eof, ByteTok(u8) }
fn code(t: ref Tok) -> i64 {
    match t {
        ByteTok(b) => {
            if b == 43 {
                return 2000;
            }
            if b > 40 {
                return 41;
            }
            return 2;
        }
        Eof => {}
    }
    return 0;
}
fn main() {
    println(f"{code(Tok.ByteTok(43))} {code(Tok.ByteTok(42))} {code(Tok.ByteTok(1))}");
}
"#;
    let errs = type_errors(ok);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(run(ok), "2000 41 2\n");

    let bad = r#"enum Tok { Eof, ByteTok(u8) }
fn code(t: ref Tok, k: i64) -> bool {
    match t {
        ByteTok(b) => b > -1 or b == k,
        Eof => false,
    }
}
fn main() {
    println(f"{code(Tok.ByteTok(4), 4)}");
}
"#;
    let errs = type_errors(bad);
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert!(
        errs.iter()
            .all(|e| e.contains("mixing signed and unsigned")),
        "{errs:?}"
    );
}
