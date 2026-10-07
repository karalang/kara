//! B-2026-10-01-16 — a fresh `Option`/`Result` temporary destructured into a
//! struct payload (`match mo(k) { Option.Some(H2 { v, .. }) => .. }`, and the
//! same under `if let`) follows the named convention of B-2026-09-30-103 on
//! both backends. When every `Drop`-bearing leaf the arm binds is only read,
//! the leaves are views and the payload walk runs every field's body once at
//! the match exit, in reverse field order. Otherwise each such leaf owns its
//! field and the walk covers the rest. Compiled, each leaf used to carry the
//! WHOLE payload walk gated on its width against the envelope, so bodies ran
//! twice (`d30 d3 d30 d3`), siblings were lost, a `Result` lost every sibling,
//! and the unwalked fields leaked; the interpreter ran only the leaf's body.

use super::*;

/// B-2026-10-01-16 — read-only leaves are views: the payload walk runs every field body once at the match exit, through `match`, `if let`, `Result`, an early `return`, `continue` and a loop.
#[test]
fn e2e_freshtemp_optres_struct_destructure_view_leaves() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H2 { v: R, r: R, n: i64 }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn mo(k: i64) -> Option[H2] { if k > 0 { Option.Some(H2 { v: mk(k), r: mk(k * 10), n: 1 }) } else { Option.None } }
fn rs(k: i64) -> Result[H2, i64] { if k > 0 { Result.Ok(H2 { v: mk(k), r: mk(k * 10), n: 1 }) } else { Result.Err(k) } }
fn a1(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, .. }) => v.id, Option.None => 0 } }
fn a2(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, r, n }) => v.id + r.id + n, Option.None => 0 } }
fn a3(k: i64) -> i64 { match mo(k) { Option.Some(H2 { n, .. }) => n, Option.None => 0 } }
fn a4(k: i64) -> i64 { match rs(k) { Result.Ok(H2 { v, .. }) => v.id, Result.Err(e) => e } }
fn a5(k: i64) -> i64 { if let Option.Some(H2 { v, .. }) = mo(k) { v.id } else { 0 } }
fn a6(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, .. }) => { println(v.s.len()); v.id }, Option.None => 0 } }
fn a7(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, .. }) => { if v.id > 2 { return v.id } 0 }, Option.None => 0 } }
fn a8(n: i64) -> i64 { let mut t = 0; for i in 1..n { let z = match mo(i) { Option.Some(H2 { v, .. }) => v.id, Option.None => 0 }; t = t + z; } t }
fn a9(n: i64) -> i64 { let mut t = 0; for i in 1..n { let z = match mo(i) { Option.Some(H2 { v, .. }) => { if i == 2 { continue } v.id }, Option.None => 0 }; t = t + z; } t }
fn a10(k: i64) -> i64 { if let Result.Ok(H2 { v, r, .. }) = rs(k) { v.id + r.id } else { 0 } }
fn main() {
    println(a1(3));
    println("-");
    println(a1(0));
    println("-");
    println(a2(3));
    println("-");
    println(a3(3));
    println("-");
    println(a4(3));
    println("-");
    println(a4(0));
    println("-");
    println(a5(3));
    println("-");
    println(a6(3));
    println("-");
    println(a7(3));
    println("-");
    println(a7(1));
    println("-");
    println(a8(3));
    println("-");
    println(a9(4));
    println("-");
    println(a10(4));
    println("end")
}"#;
    let want = "d30\nd3\n3\n-\n0\n-\nd30\nd3\n34\n-\nd30\nd3\n1\n-\nd30\nd3\n3\n-\n0\n-\nd30\nd3\n3\n-\n31\nd30\nd3\n3\n-\nd30\nd3\n3\n-\nd10\nd1\n0\n-\nd10\nd1\nd20\nd2\n3\n-\nd10\nd1\nd20\nd2\nd30\nd3\n4\n-\nd40\nd4\n44\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}

/// B-2026-10-01-16 — a leaf that is moved (`take(v)`, `let w = v`) owns its field, and the payload walk runs only the remaining fields.
#[test]
fn e2e_freshtemp_optres_struct_destructure_owning_leaf() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H2 { v: R, r: R, n: i64 }
fn mk(k: i64) -> R { R { id: k, s: f"a-heap-string-longer-than-sso-{k}" } }
fn mo(k: i64) -> Option[H2] { if k > 0 { Option.Some(H2 { v: mk(k), r: mk(k * 10), n: 1 }) } else { Option.None } }
fn rs(k: i64) -> Result[H2, i64] { if k > 0 { Result.Ok(H2 { v: mk(k), r: mk(k * 10), n: 1 }) } else { Result.Err(k) } }
fn take(x: R) -> i64 { x.id }
fn b1(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, .. }) => take(v), Option.None => 0 } }
fn b2(k: i64) -> i64 { match mo(k) { Option.Some(H2 { v, .. }) => { let w = v; w.id }, Option.None => 0 } }
fn b3(k: i64) -> i64 { match rs(k) { Result.Ok(H2 { v, .. }) => take(v), Result.Err(e) => e } }
fn b4(k: i64) -> i64 { if let Option.Some(H2 { v, .. }) = mo(k) { take(v) } else { 0 } }
fn main() {
    println(b1(3));
    println("-");
    println(b2(3));
    println("-");
    println(b3(3));
    println("-");
    println(b4(3));
    println("end")
}"#;
    let want = "d3\nd30\n3\n-\nd3\nd30\n3\n-\nd3\nd30\n3\n-\nd3\nd30\n3\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
