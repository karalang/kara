//! B-2026-11-23-5: a method call the typechecker resolved to a concrete
//! type's method that has no body of its own (a built-in type's method) was
//! charged every trait DEFAULT method of the same name in the program,
//! whether or not the type implements the trait: `s.find(" ")` on a `String`
//! took the effects of any trait's default `find`. core-semantics.md §12
//! default 4: a callee is identified by its definition, never by its name.
//! Such a call reaches a trait's default only when the program has an impl
//! of that trait for the receiver's type (a blanket impl counts for every
//! type).

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A trait whose default `find` writes Stderr, and one type that inherits it.
const FINDER: &str = r#"trait Finder {
    fn key(self) -> i64;

    fn find(self, what: Str) -> Option[i64] {
        eprintln(what);
        Some(self.key())
    }
}

struct Probe { n: i64 }

impl Finder for Probe {
    fn key(self) -> i64 { self.n }
}
"#;

fn errors(src: &str) -> Vec<String> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| format!("{}: {}", d.line, d.message))
        .collect()
}

/// `program_effects` of a one-file program, sorted.
fn program_effects(tag: &str, src: &str) -> Vec<String> {
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "karac-default-by-definition-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara", "--output=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json");
    let mut effects: Vec<String> = v["program_effects"]
        .as_array()
        .expect("program_effects")
        .iter()
        .map(|e| e.as_str().unwrap_or_default().to_string())
        .collect();
    effects.sort();
    effects
}

#[test]
fn a_builtin_method_is_not_charged_a_trait_default_of_its_name() {
    let src = format!(
        "{FINDER}\n#[no_effect(writes(Stderr))]\nfn has_space(s: String) -> bool {{\n    \
         s.find(\" \").is_some()\n}}\n\nfn main() {{\n    \
         println(has_space(\"a b\".to_string()));\n}}\n"
    );
    let found = errors(&src);
    assert!(found.is_empty(), "{found:?}");
    let effects = program_effects("builtin", &src);
    assert!(
        effects.contains(&"writes(Stdout)".to_string()),
        "{effects:?}"
    );
    assert!(
        !effects.contains(&"writes(Stderr)".to_string()),
        "{effects:?}"
    );
}

/// The control arm: the same call on a type that does inherit the default is
/// charged it, so the test above is not passing because nothing is charged.
#[test]
fn a_type_that_inherits_the_default_is_charged_it() {
    let src = format!(
        "{FINDER}\n#[no_effect(writes(Stderr))]\nfn probes(p: Probe) -> bool {{\n    \
         p.find(\" \").is_some()\n}}\n\nfn main() {{\n    println(probes(Probe {{ n: 1 }}));\n}}\n"
    );
    let found = errors(&src);
    assert!(
        found.len() == 1 && found[0].contains("writes(Stderr)"),
        "{found:?}"
    );
    // A refused program reports no effects: read them without the attribute.
    let src = src.replace("#[no_effect(writes(Stderr))]\n", "");
    let effects = program_effects("inherits", &src);
    assert!(
        effects.contains(&"writes(Stderr)".to_string()),
        "{effects:?}"
    );
}

/// Every kind of receiver that has an impl of the trait is still charged its
/// default: a built-in type with a written impl, a generic type, a tuple, a
/// type reached through an alias, and any type through a blanket impl.
#[test]
fn a_builtin_type_with_an_impl_of_the_trait_is_charged_its_default() {
    let loud =
        "trait Loud {\n    fn name(self) -> String;\n\n    fn shout(self) -> i64 {\n        \
                eprintln(self.name());\n        1\n    }\n}\n\n";
    for (tag, target, param) in [
        ("string", "String", "String"),
        ("int", "i64", "i64"),
        ("vec", "Vec[i64]", "Vec[i64]"),
        ("option", "Option[i64]", "Option[i64]"),
        ("slice", "Slice[i64]", "Slice[i64]"),
        ("array", "Array[i64, 2]", "Array[i64, 2]"),
        ("tuple", "(i64, i64)", "(i64, i64)"),
        ("map", "Map[String, i64]", "Map[String, i64]"),
    ] {
        let src = format!(
            "{loud}impl Loud for {target} {{\n    fn name(self) -> String {{ \"x\".to_string() \
             }}\n}}\n\n#[no_effect(writes(Stderr))]\nfn call(p: {param}) -> i64 {{\n    \
             p.shout()\n}}\n\nfn main() {{}}\n"
        );
        let found = errors(&src);
        assert!(
            found.len() == 1 && found[0].contains("writes(Stderr)"),
            "{tag}: {found:?}"
        );
    }
    // A generic type, and the same type under an alias.
    let src = format!(
        "{loud}struct Wrap[T] {{ v: T }}\n\nimpl[T] Loud for Wrap[T] {{\n    fn name(self) -> \
         String {{ \"w\".to_string() }}\n}}\n\ntype Boxed = Wrap[i64];\n\n\
         #[no_effect(writes(Stderr))]\nfn direct(p: Wrap[i64]) -> i64 {{\n    p.shout()\n}}\n\n\
         #[no_effect(writes(Stderr))]\nfn aliased(p: Boxed) -> i64 {{\n    p.shout()\n}}\n\nfn \
         main() {{}}\n"
    );
    let found = errors(&src);
    assert!(
        found.len() == 2 && found.iter().all(|e| e.contains("writes(Stderr)")),
        "{found:?}"
    );
    // A blanket impl gives the default to every type its bound admits.
    let src = "trait Blank {\n    fn tag(self) -> i64 {\n        eprintln(\"blank\");\n        \
               7\n    }\n}\n\nimpl[T: Display] Blank for T {}\n\n#[no_effect(writes(Stderr))]\nfn \
               on_int(p: i64) -> i64 {\n    p.tag()\n}\n\n#[no_effect(writes(Stderr))]\nfn \
               on_string(p: String) -> i64 {\n    p.tag()\n}\n\nfn main() {}\n";
    let found = errors(src);
    assert!(
        found.len() == 2 && found.iter().all(|e| e.contains("writes(Stderr)")),
        "{found:?}"
    );
}

/// Two traits with a default of one name: a type is charged the default of
/// the trait it implements, and not the other's.
#[test]
fn a_type_is_charged_only_the_default_of_the_trait_it_implements() {
    let src = "trait Base {\n    fn log(self) -> i64 {\n        eprintln(\"base\");\n        1\n    \
               }\n}\n\ntrait Other {\n    fn log(self) -> i64 {\n        println(\"other\");\n        \
               0\n    }\n}\n\nimpl Base for i64 {}\n\nimpl Other for String {}\n\nfn main() {\n    \
               let s = \"a\".to_string();\n    let n = s.log();\n}\n";
    assert_eq!(
        program_effects("other", src),
        ["allocates(Heap)", "writes(Stdout)"]
    );
    let src = src.replace("let n = s.log();", "let n = 5.log();");
    let effects = program_effects("base", &src);
    assert!(
        effects.contains(&"writes(Stderr)".to_string()),
        "{effects:?}"
    );
    assert!(
        !effects.contains(&"writes(Stdout)".to_string()),
        "{effects:?}"
    );
}
