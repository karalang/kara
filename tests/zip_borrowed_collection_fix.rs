//! Found writing kata 1357's Rust arm (B-2026-11-21-62).
//!
//! Rust builds a price table as `products.into_iter().zip(prices).collect()`.
//! In Kāra `zip` takes an `Iterator`, and a collection passed to it is
//! iterated with `.into_iter()`, which `karac check` says and `karac fix`
//! writes (B-2026-10-20-33). But when the collection is borrowed, as a bare
//! parameter is, the message was the bare trait failure ("`ref Vec[i32]`
//! does not implement `Iterator`; trait `Iterator` is implemented by: ..")
//! twice, once more at `collect`, and `karac fix` had nothing to apply.
//! A borrowed collection is now iterated by its view, `.iter()`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-zip-borrowed-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const TABLE: &str = "fn table(products: Vec[i32], prices: Vec[i32]) -> Map[i32, i32] {
    products.clone().into_iter().zip(prices).collect()
}

fn main() {
    let t = table([1, 2, 3], [10, 20, 30]);
    println(f\"{t[1]} {t[3]}\");
}
";

#[test]
fn a_borrowed_collection_zipped_is_named_and_fixed_with_its_view() {
    let path = temp("table", TABLE);
    let before = karac(&["check"], &path);
    let said = String::from_utf8_lossy(&before.stderr).into_owned();
    assert!(!before.status.success());
    assert!(
        said.contains("is a collection, not an `Iterator`; iterate it with `.iter()`"),
        "{said}"
    );
    assert!(!said.contains("is implemented by"), "{said}");
    assert_eq!(said.matches("error[").count(), 1, "{said}");
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(fixed.contains(".zip(prices.iter())"), "{fixed}");
    assert!(
        run.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "10 30\n");
}

#[test]
fn an_owned_collection_zipped_still_takes_into_iter() {
    let src = "fn main() {
    let a: Vec[i64] = [1, 2];
    let b: Vec[i64] = [3, 4];
    let s: i64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    println(s);
}
";
    let path = temp("owned", src);
    let _ = karac(&["fix"], &path);
    let fixed = std::fs::read_to_string(&path).unwrap();
    let run = karac(&["run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(fixed.contains(".zip(b.into_iter())"), "{fixed}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "11\n");
}
