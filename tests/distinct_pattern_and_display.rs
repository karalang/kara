//! Distinct types, two rows released by Thread A.
//!
//! B-2026-11-18-19: a binding inside a distinct type's constructor pattern
//! (`UserId(n) => n * 2`) was typed `ref <error>`, so any use of it failed
//! (E0208 on `*`), and `let UserId(n) = u` was not lowered. The sub-pattern
//! now binds the base value, in a `match`, under a guard and in a `let`.
//!
//! B-2026-11-18-20: a hand-written `impl Display for` a distinct type was
//! ignored by f-strings, `println` and `T: Display` bounds (design.md §9's
//! newtype escape hatch); the derive set alone answered.

use std::process::Command;

const PATTERNS: &str = "distinct type UserId = i64;\ndistinct type Name = String;\ndistinct type Pt = (i64, i64);\nfn f(u: UserId) -> i64 { match u { UserId(0) => -1, UserId(n) if n > 100 => 100, UserId(n) => n * 2 } }\nfn h(n: own Name) -> String { match n { Name(s) => s } }\nfn g(p: Pt) -> i64 { match p { Pt(t) => t.0 + t.1 } }\nfn main() {\n    println(f(UserId(21)));\n    println(f(UserId(0)));\n    println(f(UserId(500)));\n    println(h(Name(\"moved\".to_string())));\n    println(g(Pt((3, 4))));\n    let u = UserId(7);\n    let UserId(k) = u;\n    println(k + 1);\n    let UserId(z) = UserId(9);\n    println(z);\n}\n";
const PATTERNS_OUT: &str = "42\n-1\n100\nmoved\n7\n8\n9\n";
const DISPLAY: &str = "distinct type DisplayVec = Vec[i64];\nimpl Display for DisplayVec {\n    fn to_string(self) -> String { f\"[{self.raw().len()} items]\" }\n}\nfn show[T: Display](x: T) { println(f\"<{x}>\"); }\nfn main() {\n    let d = DisplayVec([1, 2, 3]);\n    println(f\"{d}\");\n    println(d);\n    show(d);\n    println(d.to_string());\n}\n";
const DISPLAY_OUT: &str = "[3 items]\n[3 items]\n<[3 items]>\n[3 items]\n";

fn run(tag: &str, lane: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-distinct-{tag}-{lane}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(lane)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(
        out.status.success(),
        "{tag} {lane}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_distinct_constructor_pattern_binds_the_base_value() {
    assert_eq!(run("patterns", "__mir-run", PATTERNS), PATTERNS_OUT);
}

#[test]
fn a_hand_written_display_on_a_distinct_type_is_used() {
    assert_eq!(run("display", "__mir-run", DISPLAY), DISPLAY_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn distinct_patterns_and_display_agree_compiled() {
    assert_eq!(run("patterns", "__mir-llvm-run", PATTERNS), PATTERNS_OUT);
    assert_eq!(run("display", "__mir-llvm-run", DISPLAY), DISPLAY_OUT);
}
