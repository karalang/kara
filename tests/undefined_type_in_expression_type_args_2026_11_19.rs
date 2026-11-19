//! B-2026-11-19-123: an undefined type in an expression's type arguments
//! (`Vec[Qqq].new()`, `Option[Qqq].None`, `Mix[Zzz].N(63)`, `Pair[Ppp] { a:
//! 1 }`) reached MIR lowering, which reported the internal 'an expression
//! whose type does not lower (Type(UnknownName("Qqq")))' at the `let`. The
//! resolver walks those arguments as it does a type's, so the name is
//! reported where it is written, with the suggestion a type position gets.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn check(src: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-exprtyargs-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn undefined_type_in_expression_type_args_is_reported_at_the_name() {
    let err = check(
        "enum Mix[T] { N(i64), S(T) }
struct Pair[T] { a: T }
fn main() {
    let v = Vec[Qqq].new();
    let o = Option[Rrr].None;
    let m = Mix[Zzz].N(63);
    let p = Pair[Ppp] { a: 1 };
}
",
    );
    for (name, at) in [
        ("Qqq", "4:17"),
        ("Rrr", "5:20"),
        ("Zzz", "6:17"),
        ("Ppp", "7:18"),
    ] {
        assert!(
            err.contains(&format!("main.kara:{at}: undefined type '{name}'")),
            "{name}: {err}"
        );
    }
    assert!(!err.contains("does not lower"), "{err}");
}

#[test]
fn misspelled_type_in_expression_type_args_gets_the_suggestion() {
    let err = check(
        "struct Point { x: i64 }\nfn main() { let v = Vec[Pointt].new(); println(v.len()); }\n",
    );
    assert!(
        err.contains("undefined type 'Pointt', did you mean 'Point'?"),
        "{err}"
    );
}

#[test]
fn defined_and_const_expression_type_args_still_resolve() {
    let err = check(
        "fn wrap[T](x: own T) -> Vec[T] { let mut v = Vec[T].new(); v.push(x); v }
const K: i64 = 3;
struct Pair[T] { a: T }
fn main() { println(wrap(5)); let o = Option[i64].None; println(o.is_none()); let p = Pair[i64] { a: K }; println(p.a); }
",
    );
    assert!(!err.contains("error"), "{err}");
}
