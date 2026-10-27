//! A trait impl's method named like an inherent method of the same type
//! is a body of its own: a call through a bound (`c.into_iter()` under
//! `C: IntoIterator`, `t.to_string()` under `T: Display`) runs the trait
//! impl's, a direct call the inherent one, on the MIR interpreter and on
//! the built binary. B-2026-10-27-10.
#![cfg(feature = "llvm")]

mod common;

use std::process::{Command, Output};

const SRC: &str = r#"struct Bag { items: Vec[i64] }
impl Bag { fn into_iter(own self) -> VecIntoIter[i64] { self.items.into_iter() } }
impl IntoIterator for Bag {
    type Item = i64;
    fn into_iter(own self) -> impl Iterator[Item = i64] { vec![100].into_iter() }
}
struct P { n: i64 }
impl P { fn to_string(ref self) -> String { f"inherent {self.n}" } }
impl Display for P { fn to_string(ref self) -> String { f"display {self.n}" } }
fn total[C: IntoIterator[Item = i64]](c: own C) -> i64 { let mut s = 0; for x in c.into_iter() { s += x; } s }
fn show[T: Display](t: ref T) -> String { t.to_string() }
fn main() {
    println(total(Bag { items: vec![3, 4] }));
    let mut s = 0;
    for x in Bag { items: vec![3, 4] }.into_iter() { s += x; }
    println(s);
    let p = P { n: 1 };
    println(p.to_string());
    println(f"{p}");
    println(show(p));
}
"#;

const OUT: &str = "100\n7\ninherent 1\ndisplay 1\ndisplay 1\n";

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn mir_a_trait_method_named_like_an_inherent_one_keeps_its_body() {
    let dir = std::env::temp_dir().join(format!("karac-named-apart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let karac = || Command::new(env!("CARGO_BIN_EXE_karac"));
    let out = karac()
        .args(["__mir-run", file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), OUT);

    let exe = dir.join("main");
    let built = karac()
        .args(["build", file.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&built.stderr).to_string();
    assert!(
        built.status.success() || err.contains("Linker failed"),
        "does not build: {err}"
    );
    if common::link_or_skip(if built.status.success() {
        Ok(())
    } else {
        Err(err)
    })
    .is_some()
    {
        let ran = Command::new(&exe).output().unwrap();
        assert!(ran.status.success(), "{}", text(&ran));
        assert_eq!(String::from_utf8_lossy(&ran.stdout), OUT);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
