//! B-2026-10-03-14 -- reassigning a `String` from a branch whose other arm is
//! the target itself (`s = if c { f"q{2}" } else { s }`) leaked the old value
//! when the replacing arm ran.

use super::*;

/// `if` with the self arm second or first, a literal arm, a `match`, and a
/// loop that alternates the two arms.
#[test]
fn e2e_string_branch_with_self_arm_frees_when_replaced() {
    let src = r#"fn self_arm(c: bool) -> String { let mut s = f"a{1}"; s = if c { f"q{2}" } else { s }; return s; }
fn self_arm_lit(c: bool) -> String { let mut s = f"a{1}"; s = if c { "lit" } else { s }; return s; }
fn self_first(c: bool) -> String { let mut s = f"a{1}"; s = if c { s } else { f"r{3}" }; return s; }
fn self_match(k: i64) -> String { let mut s = f"a{1}"; s = match k { 0 => s, 1 => f"m{1}", _ => "other" }; return s; }
fn self_loop() -> String { let mut s = f"a{1}"; let mut i = 0; while i < 4 { s = if i % 2 == 0 { f"x{i}" } else { s }; i = i + 1; } return s; }
fn main() {
    println(f"e:{self_arm(true)} {self_arm(false)} {self_arm_lit(true)} {self_arm_lit(false)}");
    println(f"f:{self_first(true)} {self_first(false)}");
    println(f"g:{self_match(0)} {self_match(1)} {self_match(2)}");
    println(f"h:{self_loop()}");
}
"#;
    let want = "e:q2 a1 lit a1\nf:a1 r3\ng:a1 m1 other\nh:x2\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
