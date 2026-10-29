//! A closure parameter typed as a view (`|n: Str|`, `|s: Slice[i64]|`)
//! borrows its argument, as a function's `Str` parameter does: a `String`
//! or `Vec` passed to it is still usable afterwards (B-2026-10-29-49). The
//! check moved it, because only a `ref` slot of a function value was read
//! as a borrow.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-clview-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_closure_view_parameter_borrows_its_argument() {
    for (name, src, want) in [
        (
            "str",
            "fn main() {
    let on = \"a\".to_string();
    let skip = |n: Str| n == on;
    let n = \"b\".to_string();
    let c = not skip(n) and n.len() > 0;
    println(f\"{c} {n}\");
}
",
            "true b\n",
        ),
        (
            "slice",
            "fn main() {
    let total = |s: Slice[i64]| s.len();
    let v: Vec[i64] = vec![1, 2, 3];
    let n = total(v);
    println(f\"{n} {v.len()}\");
}
",
            "3 3\n",
        ),
    ] {
        let (ok, err) = check(name, src);
        assert!(ok, "{name}: {err}");
        assert_eq!(
            karac::mir::lower::run_source(src)
                .expect("MIR interpreter")
                .output,
            want,
            "{name}"
        );
    }
}
