//! B-2026-11-19-57: the symbolic codes design.md and core-semantics.md give
//! diagnostics reported under a number are the record's `code`, with the
//! number as its `parent_code`, and `karac explain` knows each.

use std::process::Command;

fn check_json(name: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("karac-spec-symbols-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.kara"));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .arg(&path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn assert_code(name: &str, src: &str, code: &str, parent: &str) {
    let json = check_json(name, src);
    let want = format!("\"code\":\"{code}\",\"parent_code\":\"{parent}\"");
    assert!(json.contains(&want), "{name}: want {want} in {json}");
}

#[test]
fn spec_symbol_move_not_used() {
    assert_code(
        "move",
        "fn main() {\n    let f = move || 1;\n    println(f());\n}\n",
        "E_MOVE_NOT_USED",
        "E0001",
    );
}

#[test]
fn spec_symbol_reserved_keyword() {
    assert_code(
        "kw",
        "fn main() {\n    let match = 1;\n}\n",
        "E_RESERVED_KEYWORD",
        "E0003",
    );
}

#[test]
fn spec_symbol_raw_ident_not_allowed() {
    assert_code(
        "raw",
        "fn main() {\n    let r#self = 1;\n}\n",
        "E_RAW_IDENT_NOT_ALLOWED",
        "E0004",
    );
}

#[test]
fn spec_symbol_opaque_type_generic_forbidden() {
    assert_code(
        "opaque",
        "unsafe extern \"C\" {\n    type Foo[T];\n}\nfn main() {}\n",
        "E_OPAQUE_TYPE_GENERIC_FORBIDDEN",
        "E0001",
    );
}

#[test]
fn spec_symbol_no_provider() {
    assert_code(
        "noprov",
        "trait Ctr { fn get(self) -> i64; }\neffect resource R: Ctr;\nfn main() { println(R.get()); }\n",
        "E_NO_PROVIDER",
        "E0419",
    );
}

#[test]
fn spec_symbols_are_explained() {
    for code in [
        "E_NO_PROVIDER",
        "E_STATEFUL_DEFAULT",
        "E_PROVIDER_UNSUPPORTED",
        "E_DYN_PROVIDER",
        "E_MOVE_NOT_USED",
        "E_OPAQUE_TYPE_GENERIC_FORBIDDEN",
        "E_RESERVED_KEYWORD",
        "E_RAW_IDENT_NOT_ALLOWED",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["explain", code])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{code}: {text}");
        assert!(text.contains(code), "{code}: {text}");
        assert!(!text.contains("unknown diagnostic"), "{code}: {text}");
    }
}
