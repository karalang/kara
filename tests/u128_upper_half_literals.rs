//! An unsuffixed integer literal in `u128`'s upper half, past `i128::MAX`,
//! takes its type from its context like any unsuffixed literal (design.md
//! §5 Numeric literals; B-2026-11-13-28). Under a `u128` annotation, as a
//! `u128` argument or element, or beside a `u128` operand it is that `u128`;
//! anywhere else it is out of range, at check. The parser refused it
//! outright ("add the `u128` suffix"), even under `let h: u128 = ...`.
//!
//! The same fix stops the checker reading a `u128` carrier as the negative
//! number its bits spell: `let z: i128 = 170141183460469231731687303715884105728u128`
//! was accepted and printed `i128::MIN`, and `-340282366920938463463374607431768211455u128`
//! was accepted and printed `1`.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn run(args: &[&str], src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-u128-lit-{}-{n}", std::process::id()));
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

const ACCEPTED: &str = r#"fn show(x: u128) -> String {
    f"{x}"
}

fn main() {
    let h: u128 = 170141183460469231731687303715884105728;
    println(h);
    let top: u128 = 340282366920938463463374607431768211455;
    println(top);
    println(show(340282366920938463463374607431768211455));
    let v: Vec[u128] = [170141183460469231731687303715884105729, 5];
    println(v[0] - 1);
    let a = 200000000000000000000000000000000000000;
    let b: u128 = a;
    println(b);
    let x: u128 = 3;
    println(x + 170141183460469231731687303715884105728);
    println(top == u128.MAX);
}
"#;

const WANT: &str = "170141183460469231731687303715884105728\n\
                    340282366920938463463374607431768211455\n\
                    340282366920938463463374607431768211455\n\
                    170141183460469231731687303715884105728\n\
                    200000000000000000000000000000000000000\n\
                    170141183460469231731687303715884105731\n\
                    true\n";

fn accepted(mode: &str) {
    let out = run(&[mode], ACCEPTED);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn u128_upper_half_literal_is_accepted_under_check() {
    let out = run(&["check"], ACCEPTED);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn interp_u128_upper_half_literal_values() {
    accepted("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_u128_upper_half_literal_values() {
    accepted("__mir-llvm-run");
}

/// Each line is refused with the range it misses; none is read as the
/// negative carrier.
#[test]
fn u128_upper_half_literal_is_out_of_range_elsewhere() {
    let src = r#"fn main() {
    let x: i128 = 170141183460469231731687303715884105728;
    let y: u64 = 340282366920938463463374607431768211455;
    let z: i128 = 170141183460469231731687303715884105728u128;
    let a = 200000000000000000000000000000000000000;
    println(a);
    let n = -(200000000000000000000000000000000000000);
    let q = -340282366920938463463374607431768211455u128;
    let r = --170141183460469231731687303715884105728;
}
"#;
    let out = run(&["check"], src);
    let err =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{err}");
    let i128_range = "(expected -170141183460469231731687303715884105728..=170141183460469231731687303715884105727)";
    for want in [
        format!(
            "main.kara:2:19: integer literal 170141183460469231731687303715884105728 out of range for 'i128' {i128_range}"
        ),
        "main.kara:3:18: integer literal 340282366920938463463374607431768211455 out of range for 'u64' (expected 0..=18446744073709551615)".to_string(),
        format!(
            "main.kara:4:19: integer literal 170141183460469231731687303715884105728 out of range for 'i128' {i128_range}"
        ),
        "main.kara:5:13: integer literal 200000000000000000000000000000000000000 out of range for 'i64'".to_string(),
        "main.kara:7:13: integer literal -200000000000000000000000000000000000000 out of range for every integer type".to_string(),
        "main.kara:8:13: negative integer literal -340282366920938463463374607431768211455 cannot initialize unsigned type 'u128'".to_string(),
        format!(
            "main.kara:9:13: integer literal 170141183460469231731687303715884105728 out of range for 'i128' {i128_range}"
        ),
    ] {
        assert!(err.contains(&want), "missing `{want}` in:\n{err}");
    }
}

/// The formatter writes the literal back as it was written, with no suffix.
#[test]
fn fmt_keeps_an_upper_half_literal_unsuffixed() {
    let out = run(&["fmt"], ACCEPTED);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("let h: u128 = 170141183460469231731687303715884105728;\n"),
        "{text}"
    );
    assert!(
        !text.contains("u128;\n") && !text.contains("455u128"),
        "{text}"
    );
}
