//! An unsuffixed integer literal in `u128`'s upper half that nothing types
//! takes the `i64` default and is a range error there (design.md § 8;
//! B-2026-11-14-40). The parser still carries it as `u128`, so a `u128`
//! context accepts it; before this fix `println(<it>)` was accepted as a
//! `u128` too.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run_u128_default(args: &[&str], src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-u128-default-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn u128_upper_half_literal_in_println_is_refused_at_i64() {
    let src = "fn main() {\n    println(170141183460469231731687303715884105728);\n}\n";
    let out = run_u128_default(&["check"], src);
    let err = text(&out);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains(
            "main.kara:2:13: integer literal 170141183460469231731687303715884105728 out of range for 'i64'"
        ),
        "{err}"
    );
    assert!(
        err.contains("write `170141183460469231731687303715884105728u128` or annotate"),
        "{err}"
    );
}

#[test]
fn u128_upper_half_literal_in_fstring_is_refused_at_i64() {
    let src = "fn main() {\n    println(f\"{340282366920938463463374607431768211455}\");\n}\n";
    let out = run_u128_default(&["check"], src);
    let err = text(&out);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("out of range for 'i64'"), "{err}");
}

#[test]
fn u128_upper_half_literal_under_u128_annotation_is_accepted() {
    let src = "fn main() {\n    let h: u128 = 170141183460469231731687303715884105728;\n    println(h);\n}\n";
    let check = run_u128_default(&["check"], src);
    assert!(check.status.success(), "{}", text(&check));
    let run = run_u128_default(&["__mir-run"], src);
    assert!(run.status.success(), "{}", text(&run));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "170141183460469231731687303715884105728\n"
    );
}
