//! A feature docs/deferred.md holds is refused under its own code, which
//! `karac check --output=json` carries in `code` so `karac explain` reaches
//! it (B-2026-11-16-298): a `try` block, a trait alias and a type alias
//! `impl Trait`, each refused once at the declaration; and an `effect group`
//! (B-2026-11-16-256).

use std::process::Command;

/// `karac check --output=json` on `src`: the JSON and the exit status.
fn check_json(tag: &str, src: &str) -> (String, bool) {
    let dir = std::env::temp_dir().join(format!("karac-deferred-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("d.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["check", "--output=json", "d.kara"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// `src` is refused with one error, coded `code`, at `line`, naming `section`
/// of deferred.md.
fn refused_once(tag: &str, src: &str, code: &str, line: usize, section: &str) {
    let (json, ok) = check_json(tag, src);
    assert!(!ok, "{json}");
    assert_eq!(json.matches("\"severity\":\"error\"").count(), 1, "{json}");
    assert!(json.contains(&format!("\"code\":\"{code}\"")), "{json}");
    assert!(json.contains(&format!("\"line\":{line},")), "{json}");
    assert!(json.contains(section), "{json}");
    assert!(!json.contains("E0200"), "{json}");
}

#[test]
fn deferred_try_block_has_its_own_code() {
    refused_once(
        "try",
        "fn f() -> Result[i64, String] {\n    let r = try { 1 };\n    r\n}\nfn main() {}\n",
        "E_TRY_BLOCK_NOT_IMPLEMENTED_YET",
        2,
        "deferred.md § Try Blocks",
    );
}

#[test]
fn deferred_trait_alias_is_refused_once_at_its_declaration() {
    for (tag, tail) in [
        ("alias-unused", ""),
        ("alias-used", "fn g[T: B](x: T) -> i64 {\n    x.a()\n}\n"),
    ] {
        refused_once(
            tag,
            &format!(
                "trait A {{\n    fn a(self) -> i64;\n}}\ntrait B = A;\n{tail}fn main() {{}}\n"
            ),
            "E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET",
            4,
            "deferred.md § Trait Aliases",
        );
    }
}

#[test]
fn deferred_tait_is_refused_at_its_declaration() {
    for (tag, tail) in [
        ("tait-unused", ""),
        (
            "tait-used",
            "struct S {}\nimpl A for S {\n    fn a(self) -> i64 {\n        1\n    }\n}\n\
             fn mk() -> T {\n    S {}\n}\n",
        ),
    ] {
        refused_once(
            tag,
            &format!(
                "trait A {{\n    fn a(self) -> i64;\n}}\ntype T = impl A;\n{tail}fn main() {{}}\n"
            ),
            "E_TAIT_NOT_IMPLEMENTED_YET",
            4,
            "deferred.md § Type Alias `impl Trait`",
        );
    }
}

#[test]
fn deferred_effect_group_is_refused() {
    refused_once(
        "group",
        "effect group io = reads(FileSystem) + writes(FileSystem);\n\
         fn main() {\n    println(\"x\");\n}\n",
        "E_DEFERRED_EFFECT_GROUP",
        1,
        "deferred.md § Effect Groups and Composition",
    );
}

/// A `layout` block is deferred (deferred.md § Layout): MIR never read one,
/// so it was accepted and did nothing (B-2026-11-16-257).
#[test]
fn deferred_layout_block_has_its_own_code() {
    refused_once(
        "layout",
        "struct P {\n    x: f64,\n    y: f64,\n}\nlayout ps: Vec[P] {\n    group xy { x, y }\n}\nfn main() {}\n",
        "E_DEFERRED_LAYOUT_BLOCK",
        5,
        "deferred.md § Layout",
    );
}
