//! `Option[String].as_deref()` views the string as an `Option[Str]`
//! without moving or allocating (library/core-types.md), so an owned
//! optional string meets a `Some` of a `Str` constant in `==` and in a
//! `match` on string patterns (B-2026-11-09-97's ruling).
//! `Option[Vec[T]]` views as `Option[Slice[T]]` the same way, through the
//! library-internal `DerefView` trait (B-2026-11-11-3), and either view
//! borrows the receiver: it cannot outlive the string or vector it views.

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn write(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-option-as-deref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

/// Checks clean and prints `stdout` on every MIR lane this build has.
fn runs(name: &str, src: &str, stdout: &str) {
    let path = write(name, src);
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let run = karac(&[b], &path);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            stdout,
            "{b}: {}",
            text(&run)
        );
    }
}

#[test]
fn as_deref_views_an_optional_string_as_an_optional_str() {
    runs(
        "as_deref.kara",
        "const TOKEN: Str = \"purge\";
fn lookup(k: i64) -> Option[String] {
    if k > 0 { Some(f\"key{k}\") } else { None }
}
fn main() {
    let h: Option[String] = Some(\"purge\".to_string());
    let n: Option[String] = None;
    println(f\"{h.as_deref() == Some(TOKEN)} {n.as_deref() == Some(TOKEN)} {n.as_deref().is_none()}\");
    let g = lookup(2);
    let label = match g.as_deref() {
        Some(\"key2\") => \"two\",
        Some(_) => \"other\",
        None => \"none\",
    };
    println(f\"{label} {lookup(0).as_deref().unwrap_or(\"-\")} {g.as_deref().map(|s| s.len()).unwrap_or(0)}\");
    println(f\"{g.unwrap()}\");
}
",
        "true false true\ntwo - 4\nkey2\n",
    );
}

#[test]
fn as_deref_views_an_optional_vec_as_an_optional_slice() {
    runs("vec.kara", "fn find(k: i64) -> Option[Vec[i64]] {
    if k > 0 { Some([1, 2, 3].to_vec()) } else { None }
}
fn total(s: Slice[i64]) -> i64 { s.iter().sum() }
fn main() {
    let a = find(1);
    let b = find(0);
    let w = [1, 2, 3].to_vec();
    let want: Slice[i64] = w.as_slice();
    println(f\"{a.as_deref() == Some(want)} {b.as_deref().is_none()} {a.as_deref().map(|s| s.len()).unwrap_or(0)}\");
    match a.as_deref() {
        Some(s) => println(f\"{total(s)} {s[0]}\"),
        None => println(\"none\"),
    }
    println(f\"{a.unwrap().len()}\");
}
", "true true 3\n6 1\n3\n");
}

/// Refused by `karac check`, saying `msg`.
fn refused(name: &str, src: &str, msg: &str) {
    let path = write(name, src);
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    assert!(text(&check).contains(msg), "{}", text(&check));
}

#[test]
fn an_as_deref_view_cannot_outlive_what_it_views() {
    refused(
        "esc.kara",
        "fn main() {
    let mut keep: Option[Str] = None;
    {
        let s: Option[String] = Some(\"gone\".to_string());
        keep = s.as_deref();
    }
    println(f\"{keep.unwrap_or(\"-\")}\");
}
",
        "cannot drop `s` while `keep` still holds a borrow of it",
    );
    refused(
        "esc2.kara",
        "fn view(o: own Option[Vec[i64]]) -> Option[Slice[i64]] {
    o.as_deref()
}
fn main() {
    println(f\"{view(Some([1].to_vec())).is_some()}\");
}
",
        "cannot drop `o` while it is still borrowed",
    );
    refused(
        "esc3.kara",
        "fn main() {
    let mut s: Option[String] = Some(\"a\".to_string());
    let v = s.as_deref();
    s = None;
    println(f\"{v.unwrap_or(\"-\")}\");
}
",
        "cannot drop `s` while `v` still holds a borrow of it",
    );
}

#[test]
fn as_deref_on_an_option_of_neither_string_nor_vec_is_refused() {
    refused(
        "an.kara",
        "fn main() {
    let o: Option[i64] = Some(1);
    println(f\"{o.as_deref().is_some()}\");
}
",
        "`i64` does not implement `DerefView`",
    );
}

#[test]
fn an_option_vec_against_some_of_a_slice_takes_as_deref() {
    let path = write("cmp.kara", "fn find(k: i64) -> Option[Vec[i64]] {
    if k > 0 { Some([1, 2, 3].to_vec()) } else { None }
}
fn total(s: Slice[i64]) -> i64 { s.iter().sum() }
fn main() {
    let a = find(1);
    let b = find(0);
    let w = [1, 2, 3].to_vec();
    let want: Slice[i64] = w.as_slice();
    println(f\"{a == Some(want)} {b.as_deref().is_none()} {a.as_deref().map(|s| s.len()).unwrap_or(0)}\");
    match a.as_deref() {
        Some(s) => println(f\"{total(s)} {s[0]}\"),
        None => println(\"none\"),
    }
    println(f\"{a.unwrap().len()}\");
}
");
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(
        out.contains("so an `Option[Vec[i64]]` never meets an `Option[Slice[i64]]`"),
        "{out}"
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("a.as_deref() == Some(want)"), "{fixed}");
}
