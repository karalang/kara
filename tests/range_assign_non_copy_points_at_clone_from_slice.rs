//! `v[a..b] = other` needs `T: Copy` (collections.md, `[a..b]=`). For a
//! `String` element the lowering's "not lowered yet" was the only word
//! (B-2026-11-06-30); the checker now names the requirement and `karac fix`
//! rewrites the statement to `v[a..b].clone_from_slice(other)`.

use std::process::Command;

fn karac(args: &[&str], dir: &std::path::Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-rangeclone-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    dir
}

#[test]
fn a_non_copy_range_assign_is_rewritten_to_clone_from_slice() {
    let src = "fn main() {
    let mut w: Vec[String] = Vec.new();
    w.push(\"a\".to_string());
    w.push(\"b\".to_string());
    let mut o: Vec[String] = Vec.new();
    o.push(\"x\".to_string());
    w[0..1] = o;
    println(w.join(\",\"));
}
";
    let dir = scratch("fix", src);
    let (ok, out) = karac(&["check", "main.kara"], &dir);
    assert!(!ok);
    assert!(out.contains("needs `String: Copy`"), "{out}");
    assert!(out.contains("clone_from_slice"), "{out}");
    let (_, out) = karac(&["fix", "main.kara"], &dir);
    let fixed = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    assert!(
        fixed.contains("w[0..1].clone_from_slice(o);"),
        "{out}\n{fixed}"
    );
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "x,b\n");
}

#[test]
fn a_copy_range_assign_still_checks() {
    let src = "fn main() {
    let mut w: Vec[i64] = Vec.new();
    w.push(1);
    w.push(2);
    let o: Vec[i64] = Vec.filled(1, 9);
    w[1..2] = o;
    println(w[1]);
}
";
    let dir = scratch("copy", src);
    let (ok, out) = karac(&["run", "main.kara"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{out}");
    assert_eq!(out, "9\n");
}
