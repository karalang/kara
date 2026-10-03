//! B-2026-10-02-67 -- a nested `Option[Option[R]]` local aliased through an
//! identity call and then passed by value read the freed box.

use super::*;

/// B-2026-10-02-67 — `let c = id(b); cls(c)`: leg A disarmed the OWNER's box
/// word (`b`), while the alias `c` kept its payload-bodies walk, which read
/// the box the callee had freed (3 invalid reads per call at -O0). Covers a
/// bound arm, a `_` arm through two identity calls, a `Some(None)` value,
/// and an alias that is never passed on (its walk must still run). The read
/// is uninstrumented code, so only the instrumented -O0 leg reports it.
#[test]
fn asan_nested_option_alias_passed_by_value_reads_no_freed_box() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn id(x: Option[Option[R]]) -> Option[Option[R]] { return x }
fn cls(x: Option[Option[R]]) -> i64 {
    match x {
        Some(inner) => 7,
        None => 0,
    }
}
fn clsw(x: Option[Option[R]]) -> i64 {
    match x {
        Some(_) => 6,
        None => 0,
    }
}
fn main() {
    let b: Option[Option[R]] = Some(Some(mk(1)));
    let c = id(b);
    let k = cls(c);
    println(k);
    let b2: Option[Option[R]] = Some(Some(mk(2)));
    let c2 = id(id(b2));
    let k2 = clsw(c2);
    println(k2);
    let b3: Option[Option[R]] = Some(None);
    let c3 = id(b3);
    let k3 = cls(c3);
    println(k3);
    let b4: Option[Option[R]] = Some(Some(mk(4)));
    let c4 = id(b4);
    println("kept");
    println("end");
}
"#,
        &["dR1", "7", "dR2", "6", "7", "dR4", "kept", "end"],
        "nested_option_alias_passed_by_value",
    );
}
