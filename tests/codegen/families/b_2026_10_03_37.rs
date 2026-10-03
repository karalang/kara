//! B-2026-10-03-37 — a nested struct-variant pattern through a
//! `shared enum` crashed: `H2.Z(M2.P { v, k })` sized the inner shared
//! struct variant at its inline layout and deboxed its RC handle, and a
//! struct-variant OUTER (`H5.Z { m: M2.P { .. }, j }`) tested neither the
//! inner tag nor moved the payload once. A shared struct variant is now one
//! handle word, and a shared struct-variant outer takes the nested tag test
//! and move-out recursion the tuple-variant outer has (B-2026-10-03-36).

use super::*;

/// B-2026-10-03-37 — struct variants inside and outside, matching and not,
/// under `match` and `if let`, with `..`, two levels deep, a `String` field.
#[test]
fn e2e_nested_shared_struct_variant_pattern_binds_its_handle() {
    let src = r#"
shared enum M2 { P { v: Vec[String], k: i64 }, Q }
shared enum H2 { Z(M2), N }
shared enum H6 { W(H2), N }
shared enum Ms { S { t: String }, T }
shared enum Hs { Z(Ms), N }
shared enum H5 { Z { m: M2, j: i64 }, N }
fn mkv(s: String) -> Vec[String] { let mut v = Vec.new(); v.push(s); v.push("second-heap-string-long-enough".to_string()); v }
fn hs(k: i64) -> String { f"heap-string-long-enough-{k}" }
fn main() {
    let a = H2.Z(M2.P { v: mkv(hs(1)), k: 4 });
    let na = match a { H2.Z(M2.P { v, k }) => v.len() + k, _ => 0 };
    println(f"a {na}");
    let b = H2.Z(M2.Q);
    let nb = match b { H2.Z(M2.P { v, k }) => v.len() + k, H2.Z(M2.Q) => 9, H2.N => 0 };
    println(f"b {nb}");
    let c = H2.Z(M2.P { v: mkv(hs(3)), k: 4 });
    if let H2.Z(M2.P { v, .. }) = c { println(f"c {v[0]}") }
    let d = H2.Z(M2.P { v: mkv(hs(4)), k: 6 });
    let nd = match d { H2.Z(M2.P { k, .. }) => k, _ => 0 };
    println(f"d {nd}");
    let e = H6.W(H2.Z(M2.P { v: mkv(hs(5)), k: 1 }));
    let ne = match e { H6.W(H2.Z(M2.P { v, k })) => v.len() + k, _ => 0 };
    println(f"e {ne}");
    let f = Hs.Z(Ms.S { t: hs(6) });
    let sf = match f { Hs.Z(Ms.S { t }) => t, _ => "none".to_string() };
    println(f"f {sf}");
    let g = H5.Z { m: M2.P { v: mkv(hs(7)), k: 2 }, j: 3 };
    let ng = match g { H5.Z { m: M2.P { v, k }, j } => v.len() + k + j, _ => 0 };
    println(f"g {ng}");
    let h = H5.Z { m: M2.Q, j: 3 };
    let nh = match h { H5.Z { m: M2.P { v, k }, j } => v.len() + k + j, H5.Z { m: M2.Q, j } => j * 10, _ => 0 };
    println(f"h {nh}");
    println("end")
}"#;
    let want = "a 6\nb 9\nc heap-string-long-enough-3\nd 6\ne 3\nf heap-string-long-enough-6\ng 7\nh 30\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
