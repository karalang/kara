//! B-2026-11-25-38 — design.md §4: prelude names are not reserved, "though a
//! lint flags the likely-unintended cases". A program's own `Option` or
//! `Result` is checked under a name of its own (`Optio_`, so the library's
//! `?` and `get` keep the prelude's), and the `prelude_shadow` lint saw that
//! name and said nothing. It warns, naming the type as written; the
//! declaration's `#[allow(prelude_shadow)]` silences it; and a program's own
//! `Client`, renamed apart the same way but not a prelude name
//! (library/README.md § The prelude), stays silent.

use std::process::Command;

fn check(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-shadow-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, format!("{src}\nfn main() {{ println(\"x\"); }}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{tag}: check failed");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_programs_own_option_or_result_warns_prelude_shadow() {
    for (tag, src, name) in [
        ("enum_option", "enum Option[T] { Some(T), None }", "Option"),
        ("struct_option", "struct Option { a: i64 }", "Option"),
        ("enum_result", "enum Result { A }", "Result"),
        // README lists the core error types in the prelude too.
        ("io_error", "struct IoError { code: i64 }", "IoError"),
    ] {
        let out = check(tag, src);
        assert!(out.contains("warning[prelude_shadow]"), "{tag}: {out}");
        assert!(
            out.contains(&format!("declaration of '{name}'")),
            "{tag}: {out}"
        );
        assert!(
            !out.contains("Optio_") && !out.contains("Resul_"),
            "{tag}: {out}"
        );
    }
}

#[test]
fn allow_prelude_shadow_silences_a_renamed_option() {
    let out = check("allowed", "#[allow(prelude_shadow)]\nenum Option { A }");
    assert!(!out.contains("prelude_shadow"), "{out}");
}

/// library/README.md § The prelude is the list: a domain name the compiler
/// keeps in scope (`Server`, `Channel`, `F64`, `Span`) is not on it.
#[test]
fn a_renamed_domain_type_is_not_a_prelude_shadow() {
    for (tag, src) in [
        ("client", "struct Client { id: i64 }"),
        ("server", "struct Server { port: i64 }"),
        ("request", "struct Request { path: String }"),
        ("channel", "struct Channel { id: i64 }"),
        ("f64", "struct F64 { x: i64 }"),
        ("span", "struct Span { x: i64 }"),
    ] {
        let out = check(tag, src);
        assert!(!out.contains("prelude_shadow"), "{tag}: {out}");
    }
}
