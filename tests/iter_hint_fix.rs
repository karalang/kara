//! An iterator adaptor or terminal called on a collection (`s.bytes().collect()`,
//! `v.map(..)`) is refused with a message naming the `.iter()` to insert, and
//! the diagnostic carries that insertion, so `karac fix` makes it
//! (B-2026-10-29-3): on a `Slice` a method call returns, a `Vec` binding, a
//! fixed `Array` and a field.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_inserts_the_iter_an_adaptor_needs() {
    let src = "struct P { xs: Vec[i64] }
fn main() {
    let s = \"a-b\".to_string();
    let bs: Vec[u8] = s.bytes().collect();
    let v = [1, 2, 3].to_vec();
    let w: Vec[i64] = v.map(|x| x * 2).collect();
    let a = [5; 3];
    let d: Vec[i64] = a.map(|x| x + 1).collect();
    let p = P { xs: [7, 8].to_vec() };
    let n = p.xs.filter(|x| *x > 7).count();
    println(f\"{bs.len()} {w[2]} {d[0]} {n}\");
}
";
    let path =
        std::env::temp_dir().join(format!("karac-iter-hint-fix-{}.kara", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();

    let check = karac(&["check", "--output=json", file]);
    let json = String::from_utf8_lossy(&check.stdout);
    assert_eq!(json.matches("\"text\":\".iter()\"").count(), 4, "{json}");

    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    for want in [
        "s.bytes().iter().collect()",
        "v.iter().map(",
        "a.iter().map(",
        "p.xs.iter().filter(",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (
            run.status.code(),
            String::from_utf8_lossy(&run.stdout).as_ref()
        ),
        (Some(0), "3 6 6 1\n"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
