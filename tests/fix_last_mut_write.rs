//! Rust's `*v.last_mut().unwrap() = x` (kata 1520's Rust template; also
//! `first_mut`, `.expect(..)`, a compound assignment and a field path):
//! the library has no `last_mut`, and `karac fix` now writes the index the
//! error names, `v[v.len() - 1]` / `v[0]`, for the whole left side.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Every check's diagnostics, the text `karac fix` leaves, whether that
/// text checks clean, and what it prints under the MIR interpreter.
fn mend(dir: &Path, src: &str) -> (String, String, bool, String) {
    std::fs::create_dir_all(dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, format!("{src}\n")).unwrap();
    let mut first = String::new();
    for _ in 0..8 {
        first += &karac(dir, &["check", "cell.kara"]).2;
        let before = std::fs::read_to_string(&file).unwrap();
        karac(dir, &["fix", "cell.kara"]);
        if std::fs::read_to_string(&file).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&file).unwrap();
    let (clean, _, _) = karac(dir, &["check", "cell.kara"]);
    let (_, out, _) = karac(dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(dir);
    (first, text, clean, out)
}

#[test]
fn last_mut_unwrap_write_becomes_an_index_write() {
    let src = r#"struct Runs {
    lens: Vec<i64>,
}

impl Runs {
    fn extend(&mut self, x: i64) {
        *self.lens.last_mut().unwrap() += x;
    }
}

fn main() {
    let mut v: Vec<String> = vec!["a".to_string(), "b".to_string()];
    *v.last_mut().unwrap() = "c".to_string();
    let mut w: Vec<i64> = vec![1, 2, 3];
    *w.first_mut().expect("not empty") = 10;
    *w.last_mut().unwrap() *= 5;
    let mut r = Runs { lens: vec![1, 1] };
    r.extend(4);
    println(f"{v[0]} {v[1]} {w[0]} {w[1]} {w[2]} {r.lens[1]}");
}"#;
    let dir = std::env::temp_dir().join(format!("karac-last-mut-write-{}", std::process::id()));
    let (_, text, clean, out) = mend(&dir, src);
    assert!(clean, "still refused after fix:\n{text}");
    assert!(
        text.contains("v[v.len() - 1] = ")
            && text.contains("w[0] = 10")
            && text.contains("w[w.len() - 1] *= 5")
            && text.contains("self.lens[self.lens.len() - 1] += x")
            && !text.contains("last_mut")
            && !text.contains("first_mut"),
        "{text}"
    );
    assert_eq!(out, "a c 10 2 15 5\n", "{text}");
}
