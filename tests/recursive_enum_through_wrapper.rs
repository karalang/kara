//! B-2026-11-19-11: a plain enum that holds itself inline through `Option`,
//! `Result` or `Array` is a recursive enum without indirection (design.md §
//! Recursive enums require `shared enum`). 0.8.x accepted it, so it is a
//! `deprecated` warning whose fix is `shared enum`, and an error in the next
//! minor release (tracker B-2026-11-19-39). The direct case is E0501, and a
//! tuple around the enum is refused already.

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn scratch(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-recenum-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

const LIST: &str = "enum L { Nil, Cons(i64, Option[L]) }

fn sum(l: L) -> i64 {
    match l {
        L.Nil => 0,
        L.Cons(v, next) => match next {
            Some(n) => v + sum(n),
            None => v,
        },
    }
}

fn main() {
    let l = L.Cons(1, Some(L.Cons(2, Some(L.Cons(3, None)))));
    println(sum(l));
}
";

#[test]
fn recursive_enum_through_option_warns_and_fix_makes_it_shared() {
    let f = scratch("list", LIST);
    let p = f.to_str().unwrap();
    let check = karac(&["check", p]);
    let err = text(&check.stderr);
    assert!(check.status.success(), "{err}");
    assert_eq!(err.matches("warning[deprecated]").count(), 1, "{err}");
    assert!(
        err.contains("enum 'L' holds itself inline through `Option`"),
        "{err}"
    );
    assert!(err.contains("`shared enum L`"), "{err}");
    let before = karac(&["run", "--interp", p]);
    assert_eq!(text(&before.stdout), "6\n");
    let fixed = karac(&["fix", p]);
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.starts_with("shared enum L {"), "{after}");
    let recheck = text(&karac(&["check", p]).stderr);
    assert!(!recheck.contains("warning"), "{recheck}");
    assert_eq!(text(&karac(&["run", "--interp", p]).stdout), "6\n");
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}

/// Each wrapper that slips past E0501 warns, mutual recursion included;
/// a `shared enum`, and an enum holding another that holds itself, do not.
#[test]
fn recursive_enum_through_each_wrapper_warns() {
    let src = "enum M { Leaf, Node(Result[M, String]) }
enum A { Z, Arr(Array[A, 2]) }
pub enum P { X, Y(Option[P]) }
enum G[T] { N, C(T, Option[G[T]]) }
enum Even { Zero, Next(Option[Odd]) }
enum Odd { One(Even) }
shared enum S { Nil, Cons(i64, Option[S]) }
enum T { E, Tu((i64, S)) }
fn main() {}
";
    let f = scratch("each", src);
    let p = f.to_str().unwrap();
    let err = text(&karac(&["check", p]).stderr);
    for (name, via) in [
        ("M", "Result"),
        ("A", "Array"),
        ("P", "Option"),
        ("G", "Option"),
        ("Even", "Option"),
        ("Odd", "Option"),
    ] {
        assert!(
            err.contains(&format!(
                "enum '{name}' holds itself inline through `{via}`"
            )),
            "{name}: {err}"
        );
    }
    assert_eq!(err.matches("warning[deprecated]").count(), 6, "{err}");
    let fixed = karac(&["fix", p]);
    assert!(fixed.status.success(), "{}", text(&fixed.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("pub shared enum P"), "{after}");
    let recheck = karac(&["check", p]);
    assert!(recheck.status.success(), "{}", text(&recheck.stderr));
    assert!(
        !text(&recheck.stderr).contains("warning"),
        "{}",
        text(&recheck.stderr)
    );
    let _ = std::fs::remove_dir_all(f.parent().unwrap());
}
