//! B-2026-11-25-58: `karac query ownership` reports a parameter's DECLARED
//! mode (B-2026-10-30-35), and its derivation used to give only the reason
//! for the mode the body alone would infer, so `cell` read as `own` with
//! "`cell` is written here, so the body needs `mut ref`". Where the two
//! differ, the reason now names both.

fn query(name: &str, src: &str, target: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-qown-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("m.kara");
    std::fs::write(&file, src).expect("write source");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["query", "ownership"])
        .arg(format!("{}.{target}", file.display()))
        .output()
        .expect("run karac");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn declared_own_that_the_body_only_reads_names_both_modes() {
    let src = "fn f(s: own String) -> i64 { s.len() }\n\
               fn main() { println(f(\"ab\".to_string())); }\n";
    let out = query("own", src, "f");
    assert!(out.contains("\"mode\":\"own\""), "{out}");
    assert!(
        out.contains("the body never moves or writes `s`, so a borrow would do; it is declared `own`, which is how a caller passes it"),
        "{out}"
    );
    assert!(!out.contains("a borrow will do\""), "{out}");
}

#[test]
fn agreeing_modes_keep_the_plain_reason() {
    let src = "fn g(s: String) -> i64 { s.len() }\n\
               fn main() { println(g(\"ab\".to_string())); }\n";
    let out = query("ref", src, "g");
    assert!(out.contains("\"mode\":\"ref\""), "{out}");
    assert!(
        out.contains("the body never moves `s`, so a borrow will do"),
        "{out}"
    );
    assert!(!out.contains("it is declared"), "{out}");
}
