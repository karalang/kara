//! B-2026-11-11-51 — `concat()` on a `Vec` or `Slice` of `Vec`s flattens
//! them, as Rust's does (design.md § Vec): `Vec[Vec[T]]` and
//! `Slice[Vec[T]]` give the `Vec[T]` of every element's items in order. A
//! sequence of `String` keeps the text join. On other `Display` elements
//! the join is deprecated in 0.8.x, with a machine-applicable `join("")`
//! fix; the next minor release makes it an error (B-2026-11-11-81).

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("karac-concat-{tag}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path
}

fn run(file: &str) -> (Option<i32>, String, String) {
    let out = karac(&["__mir-run", file]);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn concat_of_vecs_flattens_on_vec_and_slice_receivers() {
    let src = "fn flat(xs: Slice[Vec[u8]]) -> Vec[u8] { xs.concat() }
fn main() {
    let vv: Vec[Vec[u8]] = [[1u8, 2].to_vec(), [3u8].to_vec()].to_vec();
    let a: Vec[u8] = vv.concat();
    let b = flat(vv);
    let words: Vec[Vec[String]] = [[\"a\".to_string(), \"b\".to_string()].to_vec(), Vec.new(), [\"c\".to_string()].to_vec()].to_vec();
    let w = words.concat();
    let none: Vec[Vec[i64]] = Vec.new();
    let ss: Vec[String] = [\"x\".to_string(), \"y\".to_string()].to_vec();
    println(f\"{a} {b} {w} {none.concat().len()} {words.len()} {ss.concat()}\");
}
";
    let path = scratch("flat", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(check.status.success(), "{text}");
    assert!(!text.contains("deprecated"), "{text}");
    let (code, stdout, stderr) = run(file);
    let _ = std::fs::remove_file(&path);
    assert_eq!(
        (code, stdout.as_str()),
        (Some(0), "[1, 2, 3] [1, 2, 3] [a, b, c] 0 3 xy\n"),
        "{stderr}"
    );
}

#[test]
fn concat_of_display_elements_is_deprecated_with_a_join_fix() {
    let src = "fn main() {
    let n: Vec[i64] = [1, 2, 3].to_vec();
    let c: Vec[char] = ['o', 'k'].to_vec();
    println(f\"{n.concat()} {c.concat()}\");
}
";
    let path = scratch("deprecated", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    assert!(check.status.success(), "{text}");
    assert_eq!(
        text.matches("`concat()` joining the elements' `Display` forms is deprecated")
            .count(),
        2,
        "{text}"
    );
    assert_eq!(run(file).1, "123 ok\n");
    karac(&["fix", file]);
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("{n.join(\"\")} {c.join(\"\")}"),
        "not fixed:\n{fixed}"
    );
    let again = karac(&["check", file]);
    let text = String::from_utf8_lossy(&again.stderr).into_owned()
        + &String::from_utf8_lossy(&again.stdout);
    assert!(!text.contains("deprecated"), "{text}");
    let (code, stdout, stderr) = run(file);
    let _ = std::fs::remove_file(&path);
    assert_eq!((code, stdout.as_str()), (Some(0), "123 ok\n"), "{stderr}");
}

#[test]
fn concat_deprecation_is_suppressed_by_allow_deprecated() {
    let src = "#[allow(deprecated)]
fn main() {
    let n: Vec[i64] = [4, 5].to_vec();
    println(n.concat());
}
";
    let path = scratch("allowed", src);
    let file = path.to_str().unwrap();
    let check = karac(&["check", file]);
    let text = String::from_utf8_lossy(&check.stderr).into_owned()
        + &String::from_utf8_lossy(&check.stdout);
    let (_, stdout, _) = run(file);
    let _ = std::fs::remove_file(&path);
    assert!(!text.contains("deprecated"), "{text}");
    assert_eq!(stdout, "45\n");
}
