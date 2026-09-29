//! B-2026-09-29-89 — a `match` / `if let` over a by-value struct param that
//! owns a `shared` field (a caller-retained VIEW, B-2026-09-29-85) bound a
//! nested STRUCT field (`Q { h, .. }` with `h: H { t: String }`) uncopied; the
//! leaf freed the caller's buffers and so did the caller's walk over its
//! argument: `free(): double free detected` at -O0 and -O2.

use super::*;

/// B-2026-09-29-89 — a nested struct field bound by `match` and `if let`,
/// passed on, pushed into a local `Vec`, handed back, and a `Drop`-bodied
/// nested field, each with a fresh temp and a named argument.
#[test]
fn asan_caller_retained_struct_param_nested_struct_leaf_freed_once() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct ShIn { s: String }
struct H { t: String, w: Vec[String] }
struct Rd { id: i64, t: String }
impl Drop for Rd { fn drop(mut ref self) { println(f"d{self.id}"); } }
struct Q { i: ShIn, n: i64, h: H, rd: Rd }
fn mkq(k: i64) -> Q {
    let mut w = Vec.new(); w.push(f"vec-string-longer-than-sso-{k}");
    Q { i: ShIn { s: f"shared-heap-string-longer-than-sso-{k}" }, n: k, h: H { t: f"nested-heap-string-longer-than-sso-{k}", w: w }, rd: Rd { id: k, t: f"drop-heap-string-longer-than-sso-{k}" } }
}
fn keep(h: H) -> i64 { h.t.len() }
fn f1(q: Q) -> i64 { match q { Q { h, .. } => h.t.len() + h.w.len() } }
fn f2(q: Q) -> i64 { if let Q { h, .. } = q { h.t.len() } else { 0 } }
fn f3(q: Q) -> i64 { match q { Q { h, .. } => keep(h) } }
fn f4(q: Q) -> i64 { let mut v: Vec[H] = Vec.new(); match q { Q { h, .. } => { v.push(h); } } v.len() }
fn g5(q: Q) -> H { match q { Q { h, .. } => h } }
fn f5(q: Q) -> i64 { g5(q).t.len() }
fn f6(q: Q) -> i64 { match q { Q { rd, .. } => rd.t.len() } }
fn main() {
    println(f"a{f1(mkq(1))}"); let b1 = mkq(2); println(f"b{f1(b1)}");
    println(f"a{f2(mkq(1))}"); let b2 = mkq(2); println(f"b{f2(b2)}");
    println(f"a{f3(mkq(1))}"); let b3 = mkq(2); println(f"b{f3(b3)}");
    println(f"a{f4(mkq(1))}"); let b4 = mkq(2); println(f"b{f4(b4)}");
    println(f"a{f5(mkq(1))}"); let b5 = mkq(2); println(f"b{f5(b5)}");
    println(f"a{f6(mkq(1))}"); let b6 = mkq(2); println(f"b{f6(b6)}");
    println("end")
}
"#,
        &[
            "d1", "a37", "b37", "d2", "d1", "a36", "b36", "d2", "d1", "a36", "b36", "d2", "d1",
            "a1", "b1", "d2", "d1", "a36", "b36", "d2", "d1", "a34", "b34", "d2", "end",
        ],
        "asan_caller_retained_struct_param_nested_struct_leaf_freed_once",
        100,
    );
}
