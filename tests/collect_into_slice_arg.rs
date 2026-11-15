//! B-2026-11-15-21: a `collect()` passed where a call takes a `Slice[T]`
//! (strings.md's `String.from_utf8(bytes: Slice[u8])`, or a user
//! `fn total(xs: Slice[i64])`) was refused with "`Slice[u8]` does not
//! implement `FromIterator`", though the same call takes any `Vec[T]`
//! value. It now collects into a `Vec[T]` there. A `let` bound as
//! `Slice[T]` is still refused: its temporary would not outlive the `let`.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn run(tag: &str, src: &str) -> (std::process::Output, String) {
    let dir = std::env::temp_dir().join(format!("karac-collectslice-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let out = karac(&["run", "--interp", p]);
    let _ = std::fs::remove_dir_all(&dir);
    (check, String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn a_collect_passed_to_a_slice_parameter_collects_into_a_vec() {
    let src = "fn total(xs: Slice[i64]) -> i64 {\n    xs.iter().sum()\n}\n\nfn main() {\n    let s = \"abc\".to_string();\n    let t = String.from_utf8(s.bytes().iter().map(|c| c + 1).collect()).unwrap();\n    println(t);\n    println(f\"{total((1..5).map(|x| x * x).collect())}\");\n}\n";
    let (check, out) = run("arg", src);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(out, "bcd\n30\n");
}

#[test]
fn a_let_bound_as_a_slice_from_a_collect_is_still_refused() {
    let src = "fn main() {\n    let v: Slice[i64] = (1..5).map(|x| x * x).collect();\n    println(f\"{v.len()}\");\n}\n";
    let (check, _) = run("let", src);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(!check.status.success(), "{said}");
    assert!(said.contains("FromIterator"), "{said}");
}
