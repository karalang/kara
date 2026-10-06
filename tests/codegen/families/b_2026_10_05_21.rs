//! B-2026-10-05-21 -- reassigning a heap `String` local to a string literal
//! (`c = "zz"`) leaked the displaced buffer compiled.

use super::*;

/// A local, a by-value param source, a loop, and a static starting value.
#[test]
fn e2e_string_reassigned_to_literal_frees_old() {
    let src = r#"fn lit() { let mut c = f"s{1}"; c = "zz"; println(f"a:{c}"); }
fn lit_param(a: String) { let mut c = a; c = "zz"; println(f"b:{c}"); }
fn lit_loop() { let mut c = f"s{1}"; let mut i = 0; while i < 3 { c = "zz"; c = f"t{i}"; i = i + 1; } println(f"c:{c}"); }
fn lit_static() { let mut c = "st"; c = "zz"; println(f"d:{c}"); }
fn main() { lit(); lit_param(f"p{1}"); lit_loop(); lit_static(); }
"#;
    let want = "a:zz\nb:zz\nc:t2\nd:zz\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
