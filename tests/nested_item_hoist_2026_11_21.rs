//! B-2026-11-21-5: a `struct` or `enum` declared inside a function body
//! read as the keyword misused as an identifier ("'struct' is a reserved
//! keyword ... write `r#struct`"), then cascaded to the body's `}` and to
//! "top-level statements beside `fn main`". It now gets the one diagnostic
//! a nested `fn` gets, and the same fix: the type, with its attributes,
//! moves above the item it was in.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// The first check's output, the fixed text, and the fixed program's run.
fn hoist(src: &str) -> (String, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-nested-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let (_, first) = karac(&dir, &["check", "main.kara"]);
    karac(&dir, &["fix", "main.kara"]);
    let text = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    let (ok, out) = karac(&dir, &["__mir-run", "main.kara"]);
    assert!(ok, "{text}\n{out}");
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, out)
}

#[test]
fn nested_struct_is_one_diagnostic_and_moves_up() {
    let (first, text, out) = hoist(
        "fn main() {\n    struct P { x: i64 }\n    let p = P { x: 1 };\n    println(f\"{p.x}\");\n}\n",
    );
    assert!(
        first.contains("a `struct` cannot be declared inside a function body"),
        "{first}"
    );
    assert!(!first.contains("reserved keyword"), "{first}");
    assert!(first.contains("1 error(s) found"), "{first}");
    assert!(
        text.starts_with("struct P { x: i64 }\n\nfn main() {\n    let p = P { x: 1 };\n"),
        "{text}"
    );
    assert_eq!(out, "1\n");
}

#[test]
fn nested_enum_with_its_derive_moves_up_inside_an_impl() {
    let (first, text, out) = hoist(
        "struct S {}\n\nimpl S {\n    fn run() -> i64 {\n        #[derive(Clone)]\n        enum E {\n            A,\n            B(i64),\n        }\n        let e = E.B(4);\n        match e.clone() { E.A => 0, E.B(n) => n }\n    }\n}\n\nfn main() {\n    println(S.run());\n}\n",
    );
    assert!(
        first.contains("an `enum` cannot be declared inside a function body"),
        "{first}"
    );
    assert!(!first.contains("#[derive]` cannot apply"), "{first}");
    assert!(first.contains("1 error(s) found"), "{first}");
    assert!(
        text.contains(
            "struct S {}\n\n#[derive(Clone)]\nenum E {\n    A,\n    B(i64),\n}\n\nimpl S {\n    fn run() -> i64 {\n        let e = E.B(4);\n"
        ),
        "{text}"
    );
    assert_eq!(out, "4\n");
}
