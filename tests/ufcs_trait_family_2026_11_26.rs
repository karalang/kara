//! B-2026-11-26-4: a type- or trait-qualified call (design.md §9 "Method
//! calls and UFCS are the same call"). `Vec.len(v)` is the receiver call
//! since B-2026-11-25-40. A trait name reaches its supertraits' methods,
//! as a bound does: `Ord.partial_cmp(a, b)` is `PartialOrd`'s. A method no
//! trait of the family declares (`Ord.max`, whose spelling is the free
//! `max`) is refused at the call; it was typed as an error with no
//! diagnostic, and check printed MIR's "a type that failed to check is not
//! lowered yet". `bool` has no `then`, so `bool.then(c, f)` is refused as
//! `c.then(f)` is.

use std::path::PathBuf;
use std::process::Command;

fn program(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-ufcs-trait-family-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const CALLS: &str = "trait A {
    fn a(self) -> i64;
}

trait B: A {
    fn b(self) -> i64;
}

struct P {
    x: i64,
}

impl A for P {
    fn a(self) -> i64 {
        self.x
    }
}

impl B for P {
    fn b(self) -> i64 {
        self.x * 2
    }
}

fn main() {
    let a: i32 = 3;
    let b: i32 = 5;
    let v: Vec[i64] = [1, 2];
    println(f\"{Vec.len(v)}\");
    println(f\"{i32.max(a, b)}\");
    println(Ord.cmp(a, b) == Ordering.Less);
    println(Ord.partial_cmp(b, a) == Some(Ordering.Greater));
    let p = P { x: 4 };
    println(B.a(p));
    println(B.b(p));
}
";

const CALLS_EXPECTED: &str = "2\n5\ntrue\ntrue\n4\n8\n";

#[test]
fn ufcs_reaches_supertrait_methods_interpreted() {
    let file = program("calls-interp", CALLS);
    let f = file.to_str().unwrap();
    let (ok, out) = karac(&["check", f]);
    assert!(ok, "{out}");
    let (ok, out) = karac(&["__mir-run", f]);
    assert!(ok, "{out}");
    assert_eq!(out, CALLS_EXPECTED);
}

#[cfg(feature = "llvm")]
#[test]
fn ufcs_reaches_supertrait_methods_compiled() {
    let file = program("calls-run", CALLS);
    let (ok, out) = karac(&["run", file.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert_eq!(out, CALLS_EXPECTED);
}

#[test]
fn ufcs_of_a_method_no_trait_of_the_family_declares_is_refused_by_name() {
    let file = program(
        "ord-max",
        "fn main() {\n    let a: i32 = 3;\n    let b: i32 = 5;\n    println(f\"{Ord.max(a, b)}\");\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("trait `Ord` has no method `max`, and none of its supertraits declares one; the free `max(..)` takes any `PartialOrd` values"),
        "{out}"
    );
    assert!(!out.contains("not lowered yet"), "{out}");
}

#[test]
fn ufcs_of_a_user_trait_names_the_missing_method() {
    let file = program(
        "user-missing",
        "trait A {\n    fn a(self) -> i64;\n}\n\nstruct P {\n    x: i64,\n}\n\nimpl A for P {\n    fn a(self) -> i64 {\n        self.x\n    }\n}\n\nfn main() {\n    let p = P { x: 1 };\n    println(A.zz(p));\n}\n",
    );
    let (ok, out) = karac(&["check", file.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("trait `A` has no method `zz`, and none of its supertraits declares one"),
        "{out}"
    );
}

#[test]
fn bool_then_is_refused_in_both_forms() {
    // `bool` has no `then` (library/core-types.md lists none): the UFCS form
    // and the receiver form agree.
    for (name, call) in [("ufcs", "bool.then(c, || 1)"), ("recv", "c.then(|| 1)")] {
        let file = program(
            &format!("bool-then-{name}"),
            &format!("fn main() {{\n    let c = true;\n    println({call});\n}}\n"),
        );
        let (ok, out) = karac(&["check", file.to_str().unwrap()]);
        assert!(!ok, "{call}: {out}");
        assert!(out.contains("'then' on type 'bool'"), "{call}: {out}");
    }
}
