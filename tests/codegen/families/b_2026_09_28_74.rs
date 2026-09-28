//! B-2026-09-28-74 -- an enum binding moved by an ASSIGNMENT (`k = s`, `h.e = s`)
//! leaves the source's drop armed on the compiled backends: a double free for a
//! value enum, a use-after-free for a `shared` field.

use super::*;

/// B-2026-09-28-74 — a named enum binding MOVED by an assignment: into another
/// binding (`k = s`) or into a struct field (`h.e = s`). Covers a `String`
/// payload, a payload with its own `Drop`, an enum with its own `Drop`, a
/// `shared enum` field (also from inside a block, displacing a live value, in a
/// loop, and through a `mut ref self` method), an `Option` / `Result` field, a
/// generic enum field, and the stores on a taken and a not-taken branch.
///
/// Before: the store left the SOURCE's drop armed. A value enum freed its
/// payload twice (`free(): double free` at -O0, and at -O2 too once the payload
/// had a `Drop` body); a `shared` field got the handle without a reference of
/// its own, so the source's release freed the node the field still held (a
/// use-after-free that ran the payload's body early). `--interp` was right.
#[test]
fn e2e_enum_moved_by_assignment_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R2 { s: String, t: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d{self.s.len()}") } }
fn mkr(n: i64) -> R2 { R2 { s: f"aaaaaaaa{n}", t: f"t{n}" } }
enum Pl { P(String), Q }
enum Pd { P(R2), Q }
shared enum Sh { P(R2), Q }
enum Own { P(String), Q }
impl Drop for Own { fn drop(mut ref self) { println("  dOwn") } }
enum G[T] { X(T), Y }
struct H1 { e: Pl }
struct H2 { e: Pd }
struct H3 { e: Sh }
struct H4 { e: Own }
struct Ho { o: Option[String] }
struct Hor { o: Option[R2] }
struct Hre { r: Result[R2, i64] }
struct Hg { e: G[String] }
impl H3 { fn put(mut ref self, s: Sh) { self.e = s; } }

fn bind_plain() { let s = Pl.P(f"aaaaaaaa{1}"); let mut k = Pl.Q; k = s; if let Pl.P(r) = k { println(f"  k{r.len()}") } }
fn bind_drop() { let s = Pd.P(mkr(1)); let mut k = Pd.Q; k = s; if let Pd.P(r) = k { println(f"  k{r.s.len()}") } }
fn bind_own() { let s = Own.P(f"aaaaaaaa{1}"); let mut k = Own.Q; k = s; if let Own.P(r) = k { println(f"  k{r.len()}") } }
fn bind_inner() { let mut k = Pl.Q; { let s = Pl.P(f"aaaaaaaa{1}"); k = s; } if let Pl.P(r) = k { println(f"  k{r.len()}") } }
fn bind_taken() { let s = Pl.P(f"aaaaaaaa{1}"); let mut k = Pl.Q; if true { k = s; } if let Pl.P(r) = k { println(f"  k{r.len()}") } }
fn bind_untaken() { let s = Own.P(f"aaaaaaaa{1}"); let mut k = Own.Q; if false { k = s; } println("  mid") }
fn bind_disp() { let s = Pl.P(f"aaaaaaaa{1}"); let mut k = Pl.P(f"bb"); k = s; if let Pl.P(r) = k { println(f"  k{r.len()}") } }
fn fld_plain() { let s = Pl.P(f"aaaaaaaa{1}"); let mut h = H1 { e: Pl.Q }; h.e = s; if let Pl.P(r) = h.e { println(f"  k{r.len()}") } }
fn fld_drop() { let s = Pd.P(mkr(1)); let mut h = H2 { e: Pd.Q }; h.e = s; if let Pd.P(r) = h.e { println(f"  k{r.s.len()}") } }
fn fld_shared() { let s = Sh.P(mkr(1)); let mut h = H3 { e: Sh.Q }; h.e = s; if let Sh.P(r) = h.e { println(f"  k{r.s.len()}") } }
fn fld_shared_inner() { let mut h = H3 { e: Sh.Q }; { let s = Sh.P(mkr(1)); h.e = s; } println("  mid"); if let Sh.P(r) = h.e { println(f"  k{r.s.len()}") } }
fn fld_shared_disp() { let s = Sh.P(mkr(1)); let mut h = H3 { e: Sh.P(mkr(22)) }; h.e = s; println("  x"); let k = h; println("  y") }
fn fld_shared_loop() { let mut h = H3 { e: Sh.Q }; let mut i = 0; while i < 3 { let s = Sh.P(mkr(i)); h.e = s; i = i + 1; } println("  mid") }
fn fld_shared_untaken() { let s = Sh.P(mkr(1)); let mut h = H3 { e: Sh.Q }; if false { h.e = s; } println("  mid") }
fn fld_shared_method() { let s = Sh.P(mkr(1)); let mut h = H3 { e: Sh.Q }; h.put(s); if let Sh.P(r) = h.e { println(f"  k{r.s.len()}") } }
fn fld_own() { let s = Own.P(f"aaaaaaaa{1}"); let mut h = H4 { e: Own.Q }; h.e = s; if let Own.P(r) = h.e { println(f"  k{r.len()}") } }
fn fld_own_untaken() { let s = Own.P(f"aaaaaaaa{1}"); let mut h = H4 { e: Own.Q }; if false { h.e = s; } println("  mid") }
fn fld_taken() { let s = Pd.P(mkr(1)); let mut h = H2 { e: Pd.Q }; if true { h.e = s; } println("  mid") }
fn fld_loop() { let mut h = H1 { e: Pl.Q }; let mut i = 0; while i < 3 { let s = Pl.P(f"aaaaaaaa{i}"); h.e = s; i = i + 1; } if let Pl.P(r) = h.e { println(f"  k{r.len()}") } }
fn fld_opt() { let s: Option[String] = Some(f"aaaaaaaa{1}"); let mut h = Ho { o: None }; h.o = s; if let Some(r) = h.o { println(f"  k{r.len()}") } }
fn fld_opt_taken() { let s: Option[String] = Some(f"aaaaaaaa{1}"); let mut h = Ho { o: None }; if true { h.o = s; } if let Some(r) = h.o { println(f"  k{r.len()}") } }
fn fld_opt_drop() { let s: Option[R2] = Some(mkr(1)); let mut h = Hor { o: None }; h.o = s; println("  mid") }
fn fld_opt_loop() { let mut h = Hor { o: None }; let mut i = 0; while i < 3 { let s: Option[R2] = Some(mkr(i)); h.o = s; i = i + 1; } println("  mid") }
fn fld_res() { let s: Result[R2, i64] = Ok(mkr(1)); let mut h = Hre { r: Err(0) }; h.r = s; println("  mid") }
fn fld_gen() { let s = G.X(f"aaaaaaaa{1}"); let mut h = Hg { e: G.Y }; h.e = s; if let G.X(r) = h.e { println(f"  k{r.len()}") } }

fn main() {
    println("bind_plain"); bind_plain()
    println("bind_drop"); bind_drop()
    println("bind_own"); bind_own()
    println("bind_inner"); bind_inner()
    println("bind_taken"); bind_taken()
    println("bind_untaken"); bind_untaken()
    println("bind_disp"); bind_disp()
    println("fld_plain"); fld_plain()
    println("fld_drop"); fld_drop()
    println("fld_shared"); fld_shared()
    println("fld_shared_inner"); fld_shared_inner()
    println("fld_shared_disp"); fld_shared_disp()
    println("fld_shared_loop"); fld_shared_loop()
    println("fld_shared_untaken"); fld_shared_untaken()
    println("fld_shared_method"); fld_shared_method()
    println("fld_own"); fld_own()
    println("fld_own_untaken"); fld_own_untaken()
    println("fld_taken"); fld_taken()
    println("fld_loop"); fld_loop()
    println("fld_opt"); fld_opt()
    println("fld_opt_taken"); fld_opt_taken()
    println("fld_opt_drop"); fld_opt_drop()
    println("fld_opt_loop"); fld_opt_loop()
    println("fld_res"); fld_res()
    println("fld_gen"); fld_gen()
    println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "bind_plain\n  k9\nbind_drop\n  k9\n  d9\nbind_own\n  dOwn\n  k9\n  dOwn\nbind_inner\n  k9\nbind_taken\n  k9\nbind_untaken\n  dOwn\n  dOwn\n  mid\nbind_disp\n  k9\nfld_plain\n  k9\nfld_drop\n  k9\n  d9\nfld_shared\n  k9\n  d9\nfld_shared_inner\n  mid\n  k9\n  d9\nfld_shared_disp\n  d10\n  x\n  d9\n  y\nfld_shared_loop\n  d9\n  d9\n  d9\n  mid\nfld_shared_untaken\n  d9\n  mid\nfld_shared_method\n  k9\n  d9\nfld_own\n  dOwn\n  k9\n  dOwn\nfld_own_untaken\n  dOwn\n  dOwn\n  mid\nfld_taken\n  d9\n  mid\nfld_loop\n  k9\nfld_opt\n  k9\nfld_opt_taken\n  k9\nfld_opt_drop\n  d9\n  mid\nfld_opt_loop\n  d9\n  mid\nfld_res\n  d9\n  mid\nfld_gen\n  k9\nend\n", "got:\n{out}");
}
