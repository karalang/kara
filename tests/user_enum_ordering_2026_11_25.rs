//! B-2026-11-25-55 — design.md §4: any local name shadows a prelude name.
//! A program's own `enum Ordering` was unusable (`Ordering.Mine` E0236, a
//! match arm E0273), while its own `Option`, `Result` or `Duration` worked:
//! the compiler spells `Ordering` for `cmp` and `sort`, and only the names in
//! `TYPES_THE_COMPILER_SPELLS` are renamed apart. `Ordering` is one now. The
//! declaration still warns `prelude_shadow` (README lists `Ordering`).

use std::path::PathBuf;
use std::process::{Command, Output};

const SRC: &str = "enum Ordering {
    Mine,
    Two(i64),
}

fn score(o: Ordering) -> i64 {
    match o {
        Ordering.Mine => 1,
        Ordering.Two(n) => n,
    }
}

fn main() {
    println(score(Ordering.Mine));
    println(score(Ordering.Two(5)));
    let mut v = [3, 1, 2];
    v.sort();
    println(v[0]);
}
";

const MINE: &str = "pub enum Ordering {
    Mine,
    Two(i64),
}

pub fn max(a: i64, b: i64) -> i64 {
    if a > b { a } else { b }
}
";

const MAIN: &str = "import mine.{max, Ordering};

fn score(o: Ordering) -> i64 {
    match o {
        Ordering.Mine => 1,
        Ordering.Two(n) => n,
    }
}

fn main() {
    println(score(Ordering.Mine) + max(2, 3));
    println(score(Ordering.Two(7)));
}
";

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("karac-ordering-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn karac(cwd: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr)
}

fn single(tag: &str) -> PathBuf {
    let d = dir(tag);
    std::fs::write(d.join("main.kara"), SRC).unwrap();
    d
}

fn package(tag: &str) -> PathBuf {
    let d = dir(tag);
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(
        d.join("kara.toml"),
        "[package]\nname = \"ord\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(d.join("src/mine.kara"), MINE).unwrap();
    std::fs::write(d.join("src/main.kara"), MAIN).unwrap();
    d
}

#[test]
fn a_programs_own_enum_ordering_checks_and_warns_prelude_shadow() {
    let d = single("check");
    let out = karac(&d, &["check", "main.kara"]);
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    assert!(t.contains("warning[prelude_shadow]"), "{t}");
    assert!(t.contains("declaration of 'Ordering'"), "{t}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_programs_own_enum_ordering_runs_interpreted() {
    let d = single("interp");
    let out = karac(&d, &["run", "--interp", "main.kara"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n5\n1\n",
        "{}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn an_imported_enum_ordering_checks_and_runs_interpreted() {
    let d = package("pkg-interp");
    let out = karac(&d, &["check"]);
    assert!(out.status.success(), "{}", text(&out));
    let out = karac(&d, &["run", "--interp"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "4\n7\n",
        "{}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[cfg(feature = "llvm")]
#[test]
fn a_programs_own_enum_ordering_runs_compiled() {
    let d = single("jit");
    let out = karac(&d, &["run", "main.kara"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "1\n5\n1\n",
        "{}",
        text(&out)
    );
    let d2 = package("pkg-jit");
    let out = karac(&d2, &["run"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "4\n7\n",
        "{}",
        text(&out)
    );
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&d2);
}
