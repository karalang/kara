//! Kata 2174: `karac fix` wrote `vec![false; 1 << (m * n)]` as
//! `Vec.filled(1 << (m * n, false)` and then refused to write it. The count
//! was cut from its own span, and a binary expression whose right operand
//! is parenthesized has a span that stops before the last `)`. The operands
//! are now cut from the text between the brackets.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn kata_2174_vec_repeat_with_a_parenthesized_count_is_fixed_whole() {
    let dir = std::env::temp_dir().join(format!("karac-vec-repeat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "fn main() {\n    let m = 2;\n    let n = 3;\n    let seen = vec![false; 1 << (m * n)];\n    let pairs = vec![(0, (1)); (n)];\n    let sums = vec![(m + n) * 2 ; m * (n + 1)];\n    println(f\"{seen.len()} {pairs.len()} {sums[0]} {sums.len()}\");\n}\n",
    )
    .unwrap();
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["fix", p]);
    assert!(ok, "{out}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.contains("let seen = Vec.filled(1 << (m * n), false);"),
        "{after}"
    );
    assert!(
        after.contains("let pairs = Vec.filled((n), (0, (1)));"),
        "{after}"
    );
    assert!(
        after.contains("let sums = Vec.filled(m * (n + 1), (m + n) * 2);"),
        "{after}"
    );
    let (_, out) = karac(&["__mir-run", p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.trim(), "64 3 10 8", "{out}");
}
