//! `Iterator.eq` compares two iterators' items, as Rust's does
//! (B-2026-11-10-26). A trait default whose bound equates an associated
//! type with `Self.Item` (`J: Iterator[Item = Self.Item]`) is copied into
//! every impl, and there `Self.Item` names the impl's own item type: before,
//! it stayed an unresolved projection, and `==` between the two items was
//! refused in every copy, so adding `eq` broke every program.

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
    let dir = std::env::temp_dir().join(format!("karac-iterator-eq-{}", std::process::id()));
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
fn iterator_eq_compares_the_items_of_two_iterators() {
    runs(
        "eq.kara",
        "fn is_palindrome(s: Str) -> bool {
    s.chars().eq(s.chars().rev())
}
fn main() {
    let d = [1, 2, 1];
    let e = [1, 2, 3];
    println(f\"{d.iter().eq(d.iter().rev())} {e.iter().eq(e.iter().rev())}\");
    let v = [1, 2, 3];
    println(f\"{(1..4).eq(v.clone().into_iter())} {(1..3).eq(v.clone().into_iter())} {v.iter().eq(v.iter().take(2))}\");
    println(f\"{is_palindrome(\"racecar\")} {is_palindrome(\"kara\")}\");
    let w: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    let u: Vec[String] = [\"a\".to_string(), \"b\".to_string()].to_vec();
    println(f\"{w.iter().eq(u.iter())} {w.into_iter().eq(u.into_iter().rev())}\");
    let none: Vec[i64] = Vec.new();
    println(f\"{none.iter().eq(none.iter())}\");
}
",
        "true false\ntrue false false\ntrue false\ntrue false\ntrue\n",
    );
}

#[test]
fn iterator_eq_refuses_two_iterators_with_different_items() {
    let path = write(
        "mismatch.kara",
        "fn main() {
    let v = [1, 2, 3];
    let s = [\"a\", \"b\"];
    println(f\"{v.iter().eq(s.iter())}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success());
    let out = text(&check);
    assert!(
        out.contains("E_WHERE_CLAUSE_ASSOC_TYPE_EQ_NOT_SATISFIED")
            && out.contains("`J.Item = ref i64`"),
        "{out}"
    );
}

#[test]
fn a_user_trait_default_bound_on_self_item_holds_in_a_generic_impl() {
    runs(
        "user.kara",
        "trait It {
    type Item;
    fn nx(mut ref self) -> Option[Self.Item];
    fn same[J: It[Item = Self.Item]](own self, other: own J) -> bool where Self.Item: PartialEq {
        let mut a = self;
        let mut b = other;
        match a.nx() {
            Some(x) => match b.nx() {
                Some(y) => x == y,
                None => false,
            },
            None => false,
        }
    }
}
struct C { n: i64 }
impl It for C {
    type Item = i64;
    fn nx(mut ref self) -> Option[i64] { self.n += 1; Some(self.n) }
}
struct W[I] { i: I }
impl[I: It] It for W[I] {
    type Item = I.Item;
    fn nx(mut ref self) -> Option[I.Item] { self.i.nx() }
}
fn main() {
    let a = W { i: C { n: 0 } };
    let b = C { n: 0 };
    let c = C { n: 1 };
    let d = W { i: C { n: 0 } };
    println(f\"{a.same(b)} {d.same(c)}\");
}
",
        "true false\n",
    );
}
