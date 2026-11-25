//! `karac explain` on a function in a recursive group reports the fixpoint
//! chain design.md §12 (Recursion and fixpoints) promises: `∅ = φ₀ ⊊ φ₁ ⊊ …
//! ⊊ φ*`, each step with the callee or operation that added atoms
//! (B-2026-11-24-46).

use std::process::{Command, Output, Stdio};

fn explain(tag: &str, src: &str, args: &[&str]) -> (Output, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixpoint-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("rec.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg("explain")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

const GROUP: &str = "fn a(n: i64) -> i64 { if n > 0 { b(n - 1) } else { println(n); 0 } }
fn b(n: i64) -> i64 { if n > 0 { a(n - 1) } else { let v: Vec[i64] = Vec.new(); v.len() } }
fn main() { println(a(3)); }
";

#[test]
fn fixpoint_chain_of_a_mutually_recursive_pair() {
    let (out, text) = explain("pair", GROUP, &["rec.kara.a"]);
    assert!(out.status.success(), "{text}");
    for line in [
        "Fixpoint: `a` is in the recursive group {a, b}; its set grew from ∅ in 2 steps",
        "  φ0 = ∅",
        "  φ1 = φ0 + panics, writes(Stdout)",
        "    rec.kara:1  a calls println, adding writes(Stdout)",
        "  φ2 = φ1 + allocates(Heap) = φ*",
        "    rec.kara:1  a calls b, adding allocates(Heap)",
    ] {
        assert!(text.contains(line), "missing {line:?} in\n{text}");
    }
}

#[test]
fn fixpoint_chain_of_a_self_recursive_function() {
    let src = "fn f(n: i64) -> i64 { if n > 0 { f(n - 1) } else { println(n); 0 } }
fn main() { println(f(2)); }
";
    let (out, text) = explain("self", src, &["rec.kara.f"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("Fixpoint: `f` calls itself; its set grew from ∅ in 1 step\n"),
        "{text}"
    );
    assert!(
        text.contains("  φ1 = φ0 + panics, writes(Stdout) = φ*"),
        "{text}"
    );
}

#[test]
fn no_fixpoint_section_outside_a_cycle() {
    let (out, text) = explain("main", GROUP, &["rec.kara.main"]);
    assert!(out.status.success(), "{text}");
    assert!(!text.contains("Fixpoint:"), "{text}");
    let (out, text) = explain("main-json", GROUP, &["--format=json", "rec.kara.main"]);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("\"fixpoint\":null"), "{text}");
}

#[test]
fn fixpoint_chain_in_json() {
    let (out, text) = explain("json", GROUP, &["--format=json", "rec.kara.a"]);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("\"fixpoint\":{\"group\":[\"a\",\"b\"],\"chain\":[{\"added\":[\"panics\",\"writes(Stdout)\"],"),
        "{text}"
    );
    assert!(
        text.contains("{\"added\":[\"allocates(Heap)\"],\"derivation\":[{\"file\":\"rec.kara\",\"line\":1,\"reason\":\"a calls b, adding allocates(Heap)\"}]}]}"),
        "{text}"
    );
}
