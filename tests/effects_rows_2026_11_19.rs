//! Three §12 effect rows from the design-effects probe, 2026-11-19.
//!
//! - B-2026-11-19-14: two traits' same-named methods on one type are
//!   checked apart, each impl against its own trait's ceiling, and a UFCS
//!   call is charged the trait it names (design.md §9 Effects of an
//!   ambiguous call). `Trait.m(x)` under an expected type runs too.
//! - B-2026-11-19-21: an access to a `mut` field of a `shared` value that
//!   carries a borrow flag is a `panics` atom.
//! - B-2026-11-19-22: storing a closure into an effect-typed slot performs
//!   none of its effects; calling through the slot in the same function
//!   still does.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, sub: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-effects-rows-1119-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(sub)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn clean(tag: &str, src: &str) {
    let out = karac(tag, "check", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn refused(tag: &str, src: &str, err: &str) {
    let out = karac(tag, "check", src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag} was accepted");
    assert!(stderr.contains(err), "{tag}: {stderr}");
}

fn runs(tag: &str, src: &str, want: &str) {
    let out = karac(tag, "__mir-run", src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{tag}");
}

const AMBIGUOUS: &str = "effect resource Db;
trait Reader { fn access(self) -> i64 with reads(Db); }
trait Writer { fn access(self) -> i64 with writes(Db); }
pub struct C {}
impl Reader for C { fn access(self) -> i64 with reads(Db) { 1 } }
impl Writer for C { fn access(self) -> i64 with writes(Db) { 2 } }
pub fn r(c: C) -> i64 with reads(Db) { Reader.access(c) }
pub fn w(c: C) -> i64 with writes(Db) { Writer.access(c) }
fn main() { println(r(C {}) + w(C {})); }
";

#[test]
fn effects_1119_ambiguous_trait_methods_are_checked_apart() {
    clean("ambiguous", AMBIGUOUS);
    runs("ambiguous", AMBIGUOUS, "3\n");
    // The reader's caller is still charged the reader's clause only, so
    // declaring the writer's is not enough.
    let src = AMBIGUOUS.replace(
        "pub fn r(c: C) -> i64 with reads(Db)",
        "pub fn r(c: C) -> i64 with writes(Db)",
    );
    refused("ambiguous-wrong-clause", &src, "reads(Db)");
}

#[test]
fn effects_1119_ufcs_call_under_an_expected_type_runs() {
    let src = "trait A { fn go(self) -> i64; }
struct S {}
impl A for S { fn go(self) -> i64 { 1 } }
fn tail(s: S) -> i64 { A.go(s) }
fn ret(s: S) -> i64 { return A.go(s); }
fn annotated(s: S) -> i64 { let x: i64 = A.go(s); x }
fn main() { let s = S {}; println(tail(s) + ret(s) + annotated(s)); }
";
    runs("ufcs-expected", src, "3\n");
}

#[test]
fn effects_1119_shared_flagged_field_access_panics() {
    let src = "shared struct I { mut items: Vec[i64] }
#[no_effect(panics)]
fn g(i: I, j: I) { for x in i.items { j.items.push(x); } }
fn main() { let a = I { items: [1] }; g(a, a); println(a.items.len()); }
";
    refused("flag-loop", src, "E_NO_EFFECT_VIOLATED");
    let src = "shared struct I { mut items: Vec[i64] }
#[no_effect(panics)]
fn f(i: I) -> i64 { i.items.len() }
fn main() { println(f(I { items: [1] })); }
";
    refused("flag-read", src, "E_NO_EFFECT_VIOLATED");
    // A `Copy` field, a field that is not `mut` and a plain struct carry no
    // flag.
    let src = "shared struct I { mut n: i64, items: Vec[i64] }
struct P { mut items: Vec[i64] }
#[no_effect(panics)]
fn f(i: I, p: P) -> bool { i.n == 0 and i.items.is_empty() and p.items.is_empty() }
fn main() { println(f(I { n: 0, items: [] }, P { items: [] })); }
";
    clean("flag-free", src);
}

const BUS: &str = "effect resource OrderDb;
pub struct Event { n: i64 }
pub struct EventBus[with E] {
    handlers: Vec[Fn(Event) with E],
}
fn record(e: Event) with writes(OrderDb) { let _ = e.n; }
";

#[test]
fn effects_1119_storing_a_closure_performs_none_of_its_effects() {
    let src = format!(
        "{BUS}pub fn mk() -> EventBus[with writes(OrderDb)] {{
    EventBus {{ handlers: [|e| record(e)] }}
}}
fn main() {{ let b = mk(); println(b.handlers.len()); }}
"
    );
    clean("store", &src);
    // Calling through the slot in the same function is charged.
    let src = format!(
        "{BUS}pub fn mk() -> EventBus[with writes(OrderDb)] {{
    let b = EventBus {{ handlers: [|e| record(e)] }};
    for h in b.handlers {{ h(Event {{ n: 1 }}); }}
    b
}}
fn main() {{ let b = mk(); println(b.handlers.len()); }}
"
    );
    refused("store-and-call", &src, "E0400");
}
