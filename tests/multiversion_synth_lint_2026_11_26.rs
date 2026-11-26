//! B-2026-11-25-69: `#[multiversion]`'s synthesized variants and dispatch
//! thunk draw no `undocumented_unsafe` warning. The desugaring put
//! `#[allow(undocumented_unsafe)]` on each of them, but the lint reads a
//! function's level from `lint_overrides`, which the parser fills, so the
//! attribute alone changed nothing and `karac check` warned twice, naming
//! `addup$avx2`, a function the program cannot edit.

use std::process::Command;

fn check_json(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac_mv_lint_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn multiversion_variants_draw_no_undocumented_unsafe_warning() {
    let out = check_json(
        "plain",
        "#[multiversion(baseline, \"avx2\")]\nfn addup(a: i64, b: i64) -> i64 {\n    a + b\n}\nfn main() {\n    println(f\"{addup(1, 2)}\");\n}\n",
    );
    assert!(out.contains("\"diagnostics\":[]"), "got: {out}");
}

#[test]
fn a_users_own_unsafe_block_beside_a_multiversion_fn_still_warns() {
    // The control: the lint is live in the same program, so the clean
    // answer above is the suppression, not a lint that never ran.
    let out = check_json(
        "control",
        "#[multiversion(baseline, \"avx2\")]\nfn addup(a: i64, b: i64) -> i64 {\n    a + b\n}\nfn main() {\n    let v = [1, 2, 3];\n    let a = unsafe { v.get_unchecked(1) };\n    println(f\"{addup(1, 2)} {a}\");\n}\n",
    );
    assert_eq!(
        out.matches("\"lint_name\":\"undocumented_unsafe\"").count(),
        1,
        "got: {out}"
    );
    assert!(!out.contains("addup$"), "got: {out}");
}
