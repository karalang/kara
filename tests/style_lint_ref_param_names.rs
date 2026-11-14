//! B-2026-11-14-21: `redundant_ref_param` leaves alone a function used as a
//! value, and it used to find those by name alone, so any bare identifier
//! (a harness's `count` parameter) kept the `ref` on every method of that
//! name. A method is named as a value only by a path (`S.one`), and a bare
//! identifier that names one of its function's own parameters reads that
//! parameter.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn fixed(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-refnames-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let out = karac(&["fix", p]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let run = karac(&["run", "--interp", p]);
    assert!(
        run.status.success(),
        "{after}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
    (after, String::from_utf8_lossy(&run.stdout).into_owned())
}

#[test]
fn ref_param_dropped_on_a_method_named_like_a_parameter() {
    let src = "struct S {}\n\nimpl S {\n    fn count(v: ref Vec[i64]) -> i64 {\n        v.len()\n    }\n}\n\n\
fn batch(count: i64) -> i64 {\n    let mut total = 0;\n    for _ in 0..count {\n        total += S.count([1, 2]);\n    }\n    total\n}\n\n\
fn main() {\n    println(batch(3));\n}\n";
    let (after, out) = fixed("method", src);
    assert!(after.contains("fn count(v: Vec[i64]) -> i64"), "{after}");
    assert_eq!(out, "6\n");
}

#[test]
fn ref_param_dropped_on_a_function_named_like_a_parameter() {
    let src = "fn size(v: ref Vec[i64]) -> i64 {\n    v.len()\n}\n\n\
fn twice(size: i64) -> i64 {\n    size * 2\n}\n\n\
fn main() {\n    println(twice(size([1, 2, 3])));\n}\n";
    let (after, out) = fixed("function", src);
    assert!(after.contains("fn size(v: Vec[i64]) -> i64"), "{after}");
    assert_eq!(out, "6\n");
}

#[test]
fn ref_param_kept_on_a_method_named_as_a_value_by_a_path() {
    let src = "struct S {}\n\nimpl S {\n    fn one(x: ref i64) -> i64 {\n        x + 1\n    }\n\n\
    fn all(xs: Vec[i64]) -> Vec[i64] {\n        xs.iter().map(S.one).collect()\n    }\n}\n\n\
fn main() {\n    println(f\"{S.all([1, 2])}\");\n}\n";
    let (after, out) = fixed("path", src);
    assert!(after.contains("fn one(x: ref i64) -> i64"), "{after}");
    assert_eq!(out, "[2, 3]\n");
}
