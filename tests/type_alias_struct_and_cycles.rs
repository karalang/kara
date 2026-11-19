//! B-2026-11-19-8: a type alias of a struct is transparent (design.md
//! § Type aliases), but `type Q = P; Q { a: 1 }` was E0207 "'Q' is not a
//! struct" and the pattern `Q { a }` matched nothing (E0205). A struct
//! literal or struct pattern through an alias now names the struct, a generic
//! alias's arguments included.
//!
//! B-2026-11-19-9: alias declarations were not checked on their own. A cycle
//! (`type A = Vec[B]; type B = A;`) passed check until used and then gave an
//! internal "no definition for type" error; it is now refused at the
//! declaration, naming the cycle. And an alias's bound was not enforced on a
//! generic parameter (`fn f[T](v: SortedVec[T])` with `type SortedVec[T:
//! Ord]`); it is now E0262 with the machine fix `T: Ord`, as a bounded
//! struct's argument is.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(src: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-alias-fixes-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap().to_string();
    (dir, p)
}

const STRUCT: &str = "struct P { a: i64 }\nstruct Pair[T] { a: T, b: T }\ntype Q = P;\ntype IntPair = Pair[i64];\ntype Ip2 = IntPair;\nfn sum(p: IntPair) -> i64 { match p { IntPair { a, b } => a + b } }\nfn main() {\n    let q = Q { a: 1 };\n    match q { Q { a } => println(a) }\n    println(sum(IntPair { a: 1, b: 2 }));\n    let Ip2 { a, b: _ } = Ip2 { a: 3, b: 4 };\n    println(a);\n    let r = IntPair { a: 5, ..IntPair { a: 0, b: 6 } };\n    println(r.a + r.b);\n}\n";
const STRUCT_OUT: &str = "1\n3\n3\n11\n";
const BOUND: &str = "type SortedVec[T: Ord] = Vec[T];\nfn f[T](v: SortedVec[T]) -> i64 { v.len() }\nfn g[T: Ord](v: SortedVec[T]) -> i64 { v.len() }\nfn main() { println(f([1])); println(g([1, 2])); }\n";

fn check(src: &str) -> (bool, String) {
    let (dir, p) = scratch(src);
    let out = karac(&["check", &p]);
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn run(src: &str, lane: &str) -> String {
    let (dir, p) = scratch(src);
    let out = karac(&[lane, &p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_struct_alias_builds_and_matches_the_struct() {
    let (ok, text) = check(STRUCT);
    assert!(ok, "{text}");
    assert_eq!(run(STRUCT, "__mir-run"), STRUCT_OUT);
}

#[test]
fn an_alias_cycle_is_refused_at_its_declaration() {
    for (src, chain) in [
        (
            "type A = Vec[B];\ntype B = A;\nfn main() {}\n",
            "(`A` -> `B` -> `A`)",
        ),
        (
            "type A = Vec[B];\ntype B = A;\nfn main() { let x: A = []; println(x.len()); }\n",
            "(`A` -> `B` -> `A`)",
        ),
        ("type C = C;\nfn main() {}\n", "(`C` -> `C`)"),
    ] {
        let (ok, text) = check(src);
        assert!(!ok, "{src}");
        assert!(text.contains("E0104"), "{text}");
        assert!(text.contains(chain), "{text}");
        assert!(!text.contains("not lowered yet"), "{text}");
    }
    // A generic alias's own parameter is not a cycle.
    let (ok, text) = check(
        "type L[T] = Vec[T];\ntype M = L[i64];\nfn main() { let m: M = [1]; println(m.len()); }\n",
    );
    assert!(ok, "{text}");
}

#[test]
fn an_alias_bound_holds_on_a_generic_parameter() {
    let (ok, text) = check(BOUND);
    assert!(!ok);
    assert!(text.contains("E0262"), "{text}");
    assert!(text.contains("declare `T: Ord`"), "{text}");
    assert_eq!(text.matches("E0262").count(), 1, "{text}");
    let (dir, p) = scratch(BOUND);
    karac(&["fix", &p]);
    let fixed = std::fs::read_to_string(&p).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(fixed.contains("fn f[T: Ord](v: SortedVec[T])"), "{fixed}");
    let (ok, text) = check(&fixed);
    assert!(ok, "{text}");
    assert_eq!(run(&fixed, "__mir-run"), "1\n2\n");
}

#[cfg(feature = "llvm")]
#[test]
fn a_struct_alias_runs_compiled() {
    assert_eq!(run(STRUCT, "__mir-llvm-run"), STRUCT_OUT);
}
