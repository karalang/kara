//! `karac explain <file>.<item>`: a function's effects, each with the
//! chain of calls that gives it, its `with` clause, and its parameters'
//! would-be modes (design.md §17 `karac fix` and `karac explain`,
//! B-2026-11-05-28).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-explain-item-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const SRC: &str = "fn inner() { println(\"x\"); }
fn mid() { inner(); }
fn top() { mid(); }
pub fn told() with writes(Stdout) { mid(); }
fn keep(s: own String) -> i64 { s.len() }
fn eat(s: own String) -> String { s }
struct P { x: i64 }
impl P {
    fn get(ref self) -> i64 { self.x }
}
fn main() {
    top();
    told();
    println(keep(\"a\".to_string()));
    println(eat(\"b\".to_string()));
    println(P { x: 1 }.get());
}
";

/// `karac explain <target> [args]` in a directory holding `a.kara`.
fn explain(target: &str, args: &[&str]) -> (bool, String, String) {
    let dir = scratch("run");
    std::fs::write(dir.join("a.kara"), SRC).unwrap();
    let out = karac()
        .arg("explain")
        .arg(target)
        .args(args)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_items_effect_comes_with_its_chain_of_calls() {
    let (ok, text, err) = explain("a.kara.top", &[]);
    assert!(ok, "{err}");
    assert!(
        text.contains(
            "  writes(Stdout)\n    a.kara:3  top calls mid\n    a.kara:2  mid calls inner\n    \
             a.kara:1  inner calls println, which has writes(Stdout)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("Declared effects: no `with` clause"),
        "{text}"
    );
    let (_, json, _) = explain("a.kara.top", &["--format=json"]);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(v["kind"], "item");
    assert_eq!(v["line"], 3);
    assert_eq!(v["inferred_effects"][0], "writes(Stdout)");
    let chain = &v["effects"][0]["derivation"];
    assert_eq!(chain.as_array().unwrap().len(), 3, "{json}");
    assert_eq!(chain[0]["file"], "a.kara");
    assert_eq!(chain[0]["line"], 3);
    assert_eq!(chain[0]["reason"], "top calls mid");
    assert!(v["declared_effects"].is_null(), "{json}");
}

#[test]
fn a_declared_items_chain_ends_at_its_clause() {
    let (ok, text, err) = explain("a.kara.told", &[]);
    assert!(ok, "{err}");
    assert!(
        text.contains("    a.kara:4  told declares writes(Stdout)\n"),
        "{text}"
    );
    assert!(text.contains("Declared effects: writes(Stdout)"), "{text}");
}

#[test]
fn an_items_parameters_show_their_would_be_modes() {
    let (_, text, _) = explain("a.kara.keep", &[]);
    assert!(
        text.contains("  s: declared own, the body needs only ref: could be borrowed (no mode)"),
        "{text}"
    );
    assert!(text.contains("Effects: none"), "{text}");
    let (_, text, _) = explain("a.kara.eat", &[]);
    assert!(text.contains("  s: own\n"), "{text}");
    let (_, json, _) = explain("a.kara.keep", &["--format=json"]);
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(v["parameters"][0]["declared_mode"], "own", "{json}");
    assert_eq!(v["parameters"][0]["would_be_mode"], "ref", "{json}");
}

#[test]
fn a_method_is_named_by_its_type() {
    let (ok, text, err) = explain("a.kara.P.get", &[]);
    assert!(ok && text.contains("item: P.get (a.kara:9)"), "{text}{err}");
    let (ok, _, err) = explain("a.kara.nope", &[]);
    assert!(
        !ok && err.contains("no function `nope` in `a.kara`"),
        "{err}"
    );
    let (ok, _, err) = explain("b.kara.top", &[]);
    assert!(!ok && err.contains("cannot read `b.kara`"), "{err}");
}
