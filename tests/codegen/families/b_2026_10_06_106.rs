//! B-2026-10-06-106 -- an index store into a module-level `let mut` array
//! (`COUNTS[1] = 40`) failed `karac build` with "Undefined variable in index
//! store", though the matching index READ already used the binding's global.

use super::*;

#[test]
fn e2e_index_store_into_module_mut_array() {
    let src = r#"let mut COUNTS: Array[i64, 3] = [1, 2, 3];
let mut NAMES: Array[String, 2] = ["a", "bb"];
fn bump() { COUNTS[2] = COUNTS[2] + 100; }
fn main() {
    COUNTS[1] = 40;
    bump();
    let mut s = 0;
    for x in COUNTS { s = s + x; }
    NAMES[0] = f"z{s}";
    NAMES[0] = f"w{s}";
    println(f"{s} {COUNTS[2]} {NAMES[0]} {NAMES[1]}");
}
"#;
    let want = "144 103 w144 bb\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
