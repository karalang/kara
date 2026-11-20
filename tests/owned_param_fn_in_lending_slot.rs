//! A function whose parameter is `own T` cannot be passed where each call
//! lends a `ref T`: `w.iter().map(tag)` with `fn tag(s: own String)` would
//! move every item out of `w` (core semantics §3.7). Check refused the
//! closure form and passed the function item, which the build then refused
//! in the library's `IterMap.next` (B-2026-11-20-100).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-owned-lent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const FNS: &str = "fn tag(s: own String) -> String { s }\nfn nlen(s: String) -> i64 { s.len() }\n";

#[test]
fn an_owned_parameter_function_over_lent_items_is_refused() {
    let src = format!(
        "{FNS}fn main() {{ let w: Vec[String] = [\"a\".to_string()]; \
         let m: Vec[String] = w.iter().map(tag).collect(); println(m.len()); }}\n"
    );
    let (ok, text) = check("lent", &src);
    assert!(!ok, "{text}");
    assert!(text.contains("error[E0200]"), "{text}");
    assert!(
        text.contains("takes its parameter as 'String', but each call here lends a 'ref String'"),
        "{text}"
    );
}

#[test]
fn owned_items_and_a_borrowing_function_are_accepted() {
    let src = format!(
        "{FNS}fn main() {{ let w: Vec[String] = [\"a\".to_string()]; \
         let n: Vec[i64] = w.iter().map(nlen).collect(); \
         let c: Vec[String] = w.iter().cloned().map(tag).collect(); \
         let m: Vec[String] = w.into_iter().map(tag).collect(); \
         println(n.len() + c.len() + m.len()); }}\n"
    );
    let (ok, text) = check("owned", &src);
    assert!(ok, "{text}");
}
