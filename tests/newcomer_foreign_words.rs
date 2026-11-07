//! B-2026-11-07-40 — a newcomer's first-hour words from Go and Python are
//! named, with Kāra's spelling, instead of an edit-distance guess that
//! `karac fix` then applies (`null` -> `Mul`, `range(3)` -> `Range(3)`,
//! `func main()` -> `func; main();`). Each fixed program is the expected
//! Kāra program, and each fix leaves a program that checks.

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

fn file(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-newcomer-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

/// The message `karac check` gives, then the program `karac fix` leaves,
/// which must check.
fn check_then_fix(tag: &str, src: &str, message: &str, fixed: &str) {
    let path = file(tag, src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(
        !ok && out.contains(message),
        "{tag}: want `{message}`:\n{out}"
    );
    for _ in 0..3 {
        karac(&["fix", p]);
    }
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, fixed, "{tag}: karac fix");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "{tag}: the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn null_true_false_range_are_named_and_fixed() {
    check_then_fix(
        "null",
        "fn find(v: Vec[i64]) -> Option[i64] { if v.len() > 0 { Some(v[0]) } else { null } }\nfn main() { println(find([])); }\n",
        "Kāra has no null",
        "fn find(v: Vec[i64]) -> Option[i64] { if v.len() > 0 { Some(v[0]) } else { None } }\nfn main() { println(find([])); }\n",
    );
    check_then_fix(
        "bool",
        "fn main() { let ok = True; let no = False; println(ok != no); }\n",
        "`True` is Python",
        "fn main() { let ok = true; let no = false; println(ok != no); }\n",
    );
    check_then_fix(
        "range",
        "fn main() { for i in range(3) { println(i); } for j in range(1, n()) { println(j); } }\nfn n() -> i64 { 3 }\n",
        "`range(n)` is Python",
        "fn main() { for i in 0..3 { println(i); } for j in 1..n() { println(j); } }\nfn n() -> i64 { 3 }\n",
    );
}

#[test]
fn go_func_and_fmt_println_are_named_and_fixed() {
    check_then_fix(
        "go",
        "func main() { fmt.Println(\"hi\") }\n",
        "`func` is Go: Kāra declares a function with `fn`",
        "fn main() { println(\"hi\") }\n",
    );
}

#[test]
fn python_def_nil_and_a_go_error_pair_are_named() {
    let path = file("def", "def main():\n    print(\"hi\")\n");
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(!ok && out.contains("`def` is Python"), "{out}");
    assert_eq!(
        out.matches("error").count(),
        2,
        "one diagnostic, no `;` cascade:\n{out}"
    );
    let path = file(
        "nil",
        "fn main() { let (n, err) = i64.parse(\"12\"); if err != nil { return; } println(n); }\n",
    );
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(!ok && out.contains("Kāra has no `nil`"), "{out}");
    assert!(
        !out.contains("did you mean"),
        "no edit-distance guess:\n{out}"
    );
    let path = file(
        "pair",
        "fn main() { let (n, err) = i64.parse(\"12\"); println(n); }\n",
    );
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(!ok && out.contains("Go's two-value return"), "{out}");
    assert!(
        !out.contains("refutable"),
        "the mismatch is reported once:\n{out}"
    );
}

#[test]
fn a_short_name_is_offered_only_a_one_edit_neighbour() {
    // `nmbr` is two edits from `number` and four letters long: no guess.
    // `cuont` swaps two letters of `count`, which counts as one edit.
    let path = file("floor", "fn main() { let number = 1; println(nmbr); }\n");
    let (_, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(!out.contains("did you mean"), "{out}");
    let path = file("swap", "fn main() { let count = 1; println(cuont); }\n");
    let (_, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(out.contains("did you mean 'count'"), "{out}");
    let path = file("near", "fn main() { let counter = 1; println(countr); }\n");
    let (_, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(out.contains("did you mean 'counter'"), "{out}");
}
