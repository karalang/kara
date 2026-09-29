//! B-2026-09-29-105 — a `match` / `if let` over a by-value struct param that
//! owns a `shared` field (a caller-retained VIEW, B-2026-09-29-85) bound a
//! nested struct that ITSELF owns a `shared` field uncopied, and moving that
//! leaf on (`let x = hs`, handing it back, pushing it) freed the caller's
//! buffers twice: `free(): double free detected` at -O0 and -O2.

use super::*;

/// B-2026-09-29-105 — the leaf rebound, handed back, pushed, read in an
/// `if let`, and a `shared` field moved out of it (the case a box-cloning copy
/// leaked), each with a fresh temp and a named argument.
#[test]
fn e2e_caller_retained_struct_param_nested_shared_owning_leaf_freed_once() {
    let src = r#"
shared struct ShIn { s: String }
struct Hs { sh: ShIn, t: String, v: Vec[i64] }
struct Q { i: ShIn, n: i64, hs: Hs }
fn mkq(k: i64) -> Q {
    let mut v = Vec.new(); v.push(k);
    Q { i: ShIn { s: f"shared-heap-string-longer-than-sso-{k}" }, n: k, hs: Hs { sh: ShIn { s: f"inner-shared-longer-than-sso-{k}" }, t: f"hs-heap-string-longer-than-sso-{k}", v: v } }
}
fn f1(q: Q) -> i64 { match q { Q { hs, n, .. } => { let x = hs; x.t.len() + n } } }
fn g2(q: Q) -> Hs { match q { Q { hs, .. } => hs } }
fn f2(q: Q) -> i64 { g2(q).t.len() }
fn f3(q: Q) -> i64 { match q { Q { hs, .. } => { let sh = hs.sh; hs.t.len() + sh.s.len() } } }
fn f4(q: Q) -> i64 { if let Q { hs, .. } = q { hs.t.len() + hs.v.len() } else { 0 } }
fn f5(q: Q) -> i64 { let mut out: Vec[Hs] = Vec.new(); match q { Q { hs, .. } => { out.push(hs); } } out.len() }
fn main() {
    println(f"a{f1(mkq(1))}"); let b1 = mkq(2); println(f"b{f1(b1)}");
    println(f"a{f2(mkq(1))}"); let b2 = mkq(2); println(f"b{f2(b2)}");
    println(f"a{f3(mkq(1))}"); let b3 = mkq(2); println(f"b{f3(b3)}");
    println(f"a{f4(mkq(1))}"); let b4 = mkq(2); println(f"b{f4(b4)}");
    println(f"a{f5(mkq(1))}"); let b5 = mkq(2); println(f"b{f5(b5)}");
    println("end")
}
"#;
    let want = "a33\nb34\na32\nb32\na62\nb62\na33\nb33\na1\nb1\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    if let Some(aot) = run_program(src) {
        assert_eq!(aot, want, "AOT");
    }
}
