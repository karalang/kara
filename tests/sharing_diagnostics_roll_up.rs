//! design.md §11 sharing roll-up (B-2026-11-04-72): `own self` on a `sync`
//! type and `weak` on a plain type are refused, and five sharing diagnostics
//! say what the spec says rather than a stale or wrong rule.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-sharing-roll-up-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn expect(tag: &str, src: &str, wants: &[&str], not: &[&str]) {
    let out = check(tag, src);
    for w in wants {
        assert!(out.contains(w), "{tag}: missing {w:?} in:\n{out}");
    }
    for n in not {
        assert!(!out.contains(n), "{tag}: unexpected {n:?} in:\n{out}");
    }
}

#[test]
fn a_sync_method_taking_own_self_is_refused() {
    expect(
        "sync-own-self",
        "sync struct C { n: Atomic[i64] }
impl C { fn done(own self) -> i64 { self.n.load() } }
fn main() { let c = C { n: Atomic.new(1) }; println(c.done()); }
",
        &[
            "2:18:",
            "of the sync type 'C' takes `own self`",
            "Write `self`",
        ],
        &[],
    );
}

#[test]
fn a_sync_mut_ref_self_names_the_sync_type() {
    expect(
        "sync-mut-ref-self",
        "sync struct Counter { n: Atomic[i64] }
impl Counter { fn clear(mut ref self) { self.n.store(0); } }
fn main() { let c = Counter { n: Atomic.new(1) }; c.clear(); println(c.n.load()); }
",
        &["method `clear` of the sync type `Counter` cannot take a `mut ref self`"],
        &["par Counter", "Arc-allocated"],
    );
}

#[test]
fn weak_on_a_plain_type_is_refused() {
    expect(
        "weak-plain",
        "struct Pl { x: i64 }
shared struct H { mut w: weak Pl }
struct J { w: weak Pl }
enum E { V { w: weak Pl } }
fn main() { println(\"x\"); }
",
        &[
            "2:26: field `w` is `weak Pl`, but `Pl` is not a `shared` or `sync` type",
            "3:15: field `w` is `weak Pl`",
            "4:17: field `w` is `weak Pl`",
        ],
        &[],
    );
    expect(
        "weak-shared",
        "shared struct P { mut kids: Vec[C] }
shared struct C { mut parent: weak P, v: i64 }
struct G[T] { w: weak T }
fn main() { let p = P { kids: [] }; let c = C { parent: p, v: 3 }; p.kids.push(c); println(p.kids.len()); }
",
        &["All checks passed"],
        &["weak P`, but"],
    );
}

#[test]
fn writing_a_plain_sync_field_does_not_suggest_mut() {
    expect(
        "sync-field-write",
        "sync struct Sx { x: i64 }
fn main() { let s = Sx { x: 1 }; s.x = 2; println(s.x); }
",
        &["field 'Sx.x' of the sync type 'Sx' cannot be written"],
        &["is not declared mut"],
    );
}

#[test]
fn a_shared_self_field_is_an_uninhabitable_type_not_a_leak() {
    expect(
        "shared-self-field",
        "shared struct Nd { mut next: Nd }
fn main() { println(\"x\"); }
",
        &[
            "type cycle detected: Nd → Nd",
            "no value of it can ever be constructed",
            "Option[T]",
        ],
        &["will leak", "add 'weak'"],
    );
}

#[test]
fn a_mutating_call_through_frozen_says_it_writes() {
    expect(
        "frozen-push",
        "shared struct N { mut neighbors: Vec[i64], val: i64 }
fn f(n: frozen N) { n.neighbors.push(1); }
fn main() { let r = N { neighbors: [], val: 1 }; let g = freeze r; f(g); println(g.val); }
",
        &["`frozen` parameter `n` cannot be written through"],
        &["cannot be projected"],
    );
    expect(
        "frozen-source-push",
        "shared struct N { mut neighbors: Vec[i64], val: i64 }
fn main() { let r = N { neighbors: [], val: 1 }; let g = freeze r; r.neighbors.push(1); println(g.val); }
",
        &["`frozen` source `r` cannot be written through"],
        &["cannot be projected"],
    );
}
