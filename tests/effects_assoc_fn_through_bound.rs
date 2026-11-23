//! B-2026-11-23-16: an associated function called through a type-parameter
//! bound (`T.make()` with `T: Make`) was charged the trait's written clause
//! and nothing else, so `fn build[T: Make]() -> T { T.make() }` at a `Big`
//! whose `make` writes Stderr reported no effects. It is charged the
//! instance's impl now, as the method form through a bound is (design.md
//! § Effects).

use std::path::PathBuf;
use std::process::{Command, Stdio};

const MAKE: &str = "pub struct Big { v: i64 }
pub struct Small { v: i64 }

trait Make {
    fn make() -> Self;
    fn pair() -> (Self, Self) { (Self.make(), Self.make()) }
}

impl Make for Big {
    fn make() -> Big {
        eprintln(\"loud\");
        Big { v: 1 }
    }
}

impl Make for Small {
    fn make() -> Small { Small { v: 2 } }
}

fn build[T: Make]() -> T { T.make() }
fn twice[T: Make]() -> T { build() }
";

fn check_json(tag: &str, src: &str) -> serde_json::Value {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-assoc-fn-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara", "--output=json"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("json")
}

fn errors(v: &serde_json::Value) -> Vec<String> {
    v["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| d["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Each public function that builds a `Big` through the bound owes
/// `writes(Stderr)`; the ones that build a `Small`, whose `make` is silent,
/// owe nothing.
#[test]
fn an_associated_function_through_a_bound_is_charged_the_instances_impl() {
    let src = format!(
        "{MAKE}
pub fn loud() -> Big {{ build() }}
pub fn quiet() -> Small {{ build() }}
pub fn loud_twice() -> Big {{ twice() }}
pub fn quiet_twice() -> Small {{ twice() }}
pub fn declared() -> Big with writes(Stderr) {{ build() }}

fn main() {{
    println(loud().v + quiet().v + loud_twice().v + quiet_twice().v + declared().v);
}}
"
    );
    let found = errors(&check_json("instance", &src));
    for name in ["loud", "loud_twice"] {
        assert!(
            found.iter().any(|m| m.contains(&format!(
                "public function '{name}' performs effects [writes(Stderr)]"
            ))),
            "`{name}` runs `Big.make`: {found:?}"
        );
    }
    for name in ["quiet", "quiet_twice", "declared"] {
        assert!(
            !found.iter().any(|m| m.contains(&format!("'{name}'"))),
            "`{name}` owes nothing more: {found:?}"
        );
    }
    assert_eq!(found.len(), 2, "{found:?}");
}

/// `Self.make()` in a trait's default body runs the implementing type's
/// `make`.
#[test]
fn an_associated_function_on_self_in_a_default_body_is_charged() {
    let src = format!(
        "{MAKE}
pub fn two() -> i64 {{
    let (a, b) = Big.pair();
    a.v + b.v
}}

fn main() {{
    println(two());
}}
"
    );
    let found = errors(&check_json("self", &src));
    assert!(
        found
            .iter()
            .any(|m| m.contains("public function 'two' performs effects [writes(Stderr)]")),
        "`Big.pair` calls `Big.make` twice: {found:?}"
    );
}

/// What the under-report let through: `#[no_effect]` on a function that
/// writes Stderr through the bound.
#[test]
fn an_associated_function_through_a_bound_reaches_no_effect() {
    let src = format!(
        "{MAKE}
#[no_effect(writes(Stderr))]
fn pure_build() -> Big {{ build() }}

#[no_effect(writes(Stderr))]
fn pure_small() -> Small {{ build() }}

fn main() {{
    println(pure_build().v + pure_small().v);
}}
"
    );
    let found = errors(&check_json("no-effect", &src));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("pure_build") && found[0].contains("writes(Stderr)"),
        "{found:?}"
    );
}
