//! B-2026-11-18-38: a hand-written `impl PartialEq` + `impl Eq`, `impl Ord`
//! or `impl Hash` for a distinct type was ignored, as `impl Display` was
//! before B-2026-11-18-20: the gate behind `==`, `T: Ord`, `sort` and a
//! `Map` key consulted the derive set alone, so each was refused. A hand impl
//! now counts as the type's own (design.md §9's newtype escape hatch), and
//! each operation dispatches to it. A distinct type with neither a derive
//! nor an impl still inherits nothing from its base.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const EQ: &str = "distinct type Ci = String;\nimpl PartialEq for Ci {\n    fn eq(self, other: Ci) -> bool { self.raw().to_lowercase() == other.raw().to_lowercase() }\n}\nimpl Eq for Ci {}\nfn same[T: Eq](a: T, b: T) -> bool { a == b }\nfn main() {\n    println(Ci(\"A\".to_string()) == Ci(\"a\".to_string()));\n    println(same(Ci(\"B\".to_string()), Ci(\"b\".to_string())));\n}\n";
const EQ_OUT: &str = "true\ntrue\n";
const ORD: &str = "distinct type Rev = i64;\nimpl PartialEq for Rev { fn eq(self, other: Rev) -> bool { self.raw() == other.raw() } }\nimpl Eq for Rev {}\nimpl PartialOrd for Rev { fn partial_cmp(self, other: Rev) -> Option[Ordering] { Some(other.raw().cmp(self.raw())) } }\nimpl Ord for Rev { fn cmp(self, other: Rev) -> Ordering { other.raw().cmp(self.raw()) } }\nfn top[T: Ord](a: own T, b: own T) -> T { if a > b { a } else { b } }\nfn main() {\n    println(Rev(1) < Rev(2));\n    println(top(Rev(1), Rev(2)).raw());\n    let mut v = [Rev(1), Rev(3), Rev(2)];\n    v.sort();\n    println(v[0].raw());\n}\n";
const ORD_OUT: &str = "false\n1\n3\n";
const HASH: &str = "distinct type Ci = String;\nimpl PartialEq for Ci { fn eq(self, other: Ci) -> bool { self.raw().to_lowercase() == other.raw().to_lowercase() } }\nimpl Eq for Ci {}\nimpl Hash for Ci { fn hash[H: Hasher](ref self, hasher: mut ref H) { self.raw().to_lowercase().hash(hasher) } }\nfn main() {\n    let mut m: Map[Ci, i64] = Map.new();\n    m[Ci(\"A\".to_string())] = 1;\n    m[Ci(\"a\".to_string())] = 2;\n    println(m.len());\n    println(m[Ci(\"A\".to_string())]);\n}\n";
const HASH_OUT: &str = "1\n2\n";

fn karac(tag: &str, sub: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-distinct-hand-{tag}-{sub}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run(tag: &str, lane: &str, src: &str) -> String {
    let check = karac(tag, "check", src);
    assert!(
        check.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = karac(tag, lane, src);
    assert!(
        out.status.success(),
        "{tag} {lane}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_hand_written_eq_on_a_distinct_type_is_used() {
    assert_eq!(run("eq", "__mir-run", EQ), EQ_OUT);
}

#[test]
fn a_hand_written_ord_on_a_distinct_type_is_used() {
    assert_eq!(run("ord", "__mir-run", ORD), ORD_OUT);
}

#[test]
fn a_hand_written_hash_on_a_distinct_type_keys_a_map() {
    assert_eq!(run("hash", "__mir-run", HASH), HASH_OUT);
}

#[test]
fn a_distinct_type_without_eq_still_refuses_equality() {
    let out = karac(
        "bare",
        "check",
        "distinct type D = i64;\nfn main() { println(D(1) == D(2)); }\n",
    );
    assert!(!out.status.success(), "a bare distinct type compared");
}

#[cfg(feature = "llvm")]
#[test]
fn hand_written_eq_ord_and_hash_on_distinct_types_agree_compiled() {
    assert_eq!(run("eq", "__mir-llvm-run", EQ), EQ_OUT);
    assert_eq!(run("ord", "__mir-llvm-run", ORD), ORD_OUT);
    assert_eq!(run("hash", "__mir-llvm-run", HASH), HASH_OUT);
}
