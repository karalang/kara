//! `Array[T]` written as a parameter type with no length (kata 2764) was
//! taken as a type of its own, and the call with a list literal was then
//! refused as "expected 'Array[i64]', found 'Vec[i64]'", which names
//! neither the missing length nor the type meant (B-2026-11-20-143). `Array[T]`
//! now says that `Array` takes a length and that a list of any length is
//! borrowed as `Slice[T]`, with the fix to `Slice[T]`, and checks on as
//! that slice.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-array-no-length-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const PAIRS: &str = r#"fn pairs(list: Array[(i64, i64)]) -> Vec[Vec[i64]] {
    list.iter().map(|(a, b)| [a, b].to_vec()).collect()
}

fn main() {
    let rows = pairs([(0, -1), (1, 0), (2, 0)]);
    println(f"{rows}");
}
"#;

#[test]
fn an_array_type_with_no_length_is_fixed_to_a_slice() {
    let path = temp("pairs", PAIRS);
    let first = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains(
            "`Array` takes an element type and a length, `Array[(i64, i64), N]`; a list of any length is borrowed as `Slice[(i64, i64)]`"
        ),
        "{stderr}"
    );
    assert_eq!(stderr.matches("error[").count(), 1, "{stderr}");
    let fixed = karac(&["fix"], &path);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("fn pairs(list: Slice[(i64, i64)])"), "{text}");
    let out = karac(&["run", "--interp"], &path);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "[[0, -1], [1, 0], [2, 0]]\n"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_array_type_with_its_length_is_left_alone() {
    let src = "fn total(a: Array[i64, 3]) -> i64 {\n    a.iter().sum()\n}\n\nfn main() {\n    \
               println(f\"{total([1, 2, 3])}\");\n}\n";
    let path = temp("sized", src);
    let out = karac(&["check"], &path);
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(text, src);
}
