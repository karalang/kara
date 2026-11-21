//! Kata 2851's matrix arm builds its step matrix as `let step = [[0, a],
//! [1, b]];`, copies it with `let mut b = step;`, and squares `b` through
//! `fn times(a: Array[Array[i64, 2], 2], ..)`. A list literal with no
//! expected type is a `Vec` (design.md § Collection literals), and a use
//! of the literal's own binding as an `Array` already said so and offered
//! `: Array[T, N]` on its `let`; a use through the copy said neither, and
//! `karac fix` had nothing to apply. The copy now leads back to the
//! literal's binding, which takes the annotation, and the copy its type
//! from it.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `karac` on `cell.kara` in `dir`: its verdict, its output, and the
/// file as it stands after.
fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

/// One `karac fix` round over `src`, then a second that must find nothing,
/// then `__mir-run`: the source after the one round, and what it prints.
fn one_round(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2851-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (_, out, fixed) = karac(&dir, &["fix"]);
    assert!(out.contains("applied"), "{tag}: {out}");
    let (_, out, again) = karac(&dir, &["fix"]);
    assert_eq!(
        again, fixed,
        "{tag}: a second round changed the source: {out}"
    );
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

#[test]
fn kata_2851_list_literal_copy_used_as_array_declares_the_literal() {
    let src = r#"fn times(a: Array[Array[i64, 2], 2], b: Array[Array[i64, 2], 2]) -> Array[Array[i64, 2], 2] {
    let mut c: Array[Array[i64, 2], 2] = [[0, 0], [0, 0]];
    for i in 0..2 {
        for j in 0..2 {
            c[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j];
        }
    }
    c
}

fn main() {
    let step = [[0, 1], [1, 1]];
    let mut b = step;
    let mut e = 10;
    let mut result: Array[Array[i64, 2], 2] = [[1, 0], [0, 1]];
    while e > 0 {
        if e % 2 == 1 {
            result = times(result, b);
        }
        b = times(b, b);
        e /= 2;
    }
    println(result[0][1]);
}
"#;
    let dir = std::env::temp_dir().join(format!("karac-kata-2851-msg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (ok, out, _) = karac(&dir, &["check"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`b` holds `step`, and `step` (bound at 12:9) is a `Vec`"),
        "{out}"
    );
    assert!(
        out.contains("declare it `step: Array[Array[i64, 2], 2]`"),
        "{out}"
    );
    let (fixed, out) = one_round("copy", src);
    assert!(
        fixed.contains(
            "let step: Array[Array[i64, 2], 2] = [[0, 1], [1, 1]];\n    let mut b = step;"
        ),
        "{fixed}"
    );
    assert_eq!(out.trim(), "55");
}

#[test]
fn kata_2851_list_literal_and_its_copy_both_used_take_one_annotation() {
    let (fixed, out) = one_round(
        "both",
        r#"fn sum2(a: Array[i64, 2]) -> i64 {
    a[0] + a[1]
}

fn main() {
    let step = [3, 4];
    let copy = step;
    println(f"{sum2(step)} {sum2(copy)}");
}
"#,
    );
    assert_eq!(fixed.matches(": Array[i64, 2]").count(), 2, "{fixed}");
    // The same round also clones `step` into `copy`, the move E0500 asks
    // for while it is a `Vec`.
    assert!(
        fixed.contains("let step: Array[i64, 2] = [3, 4];\n    let copy = step"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "7 7");
}

#[test]
fn kata_2851_list_literal_copy_given_another_vec_gets_no_fix() {
    let src = r#"fn sum2(a: Array[i64, 2]) -> i64 {
    a[0] + a[1]
}

fn main() {
    let step = [3, 4];
    let mut b = step;
    b = [5, 6];
    println(f"{sum2(b)}");
}
"#;
    let dir = std::env::temp_dir().join(format!("karac-kata-2851-other-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(!ok, "{out}");
    assert!(out.contains("`b` holds `step`"), "{out}");
    assert!(!out.contains("declare it"), "{out}");
    let (_, _, after) = karac(&dir, &["fix"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(after, src);
}
