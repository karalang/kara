//! B-2026-10-04-33 — `let g = h;` over a user enum whose payload is a
//! heap-boxed `Option`/`Result`, with `h` read again afterwards, segfaulted
//! compiled. The use-after-move copy declined the enum (it needs no scope-exit
//! owner by the buffer test), the site was not recorded as copied, and the
//! source disarm zeroed the box word the later read of `h` then followed.

use super::*;

/// B-2026-10-04-33 — `Option[String]` (Some, None, a unit variant beside it,
/// both values consumed, one consumed and one read), `Result[String, i64]`
/// (Ok and Err), `Option[Vec[i64]]`, `Option[i64]`, and a two-field variant.
/// The interpreter agrees on every cell.
#[test]
fn e2e_moved_enum_with_boxed_option_payload_is_copied_for_its_reuse() {
    let src = r#"shared struct Nd { v: i64 }
impl Drop for Nd { fn drop(mut ref self) { println(f"dN{self.v}") } }
enum Ho { O(Option[String]), E }
enum Hr { R(Result[String, i64]), E }
enum Hv { V(Option[Vec[i64]]), E }
enum Hi { I(Option[i64]), E }
enum Hn { N(Option[Nd]), E }
enum H2 { T(Option[String], i64), E }
fn hs(s: String) -> String { f"{s}xxxxxxxxxxxxxxxxxxxxxxxx" }
fn go(h: ref Ho) -> i64 { match h { Ho.O(o) => match o { Some(s) => s.len(), None => 1 }, Ho.E => 0 } }
fn take(h: Ho) -> String { match h { Ho.O(o) => match o { Some(s) => s, None => "n" }, Ho.E => "e" } }
fn gr(h: ref Hr) -> i64 { match h { Hr.R(r) => match r { Ok(s) => s.len(), Err(e) => e }, Hr.E => 0 } }
fn gv(h: ref Hv) -> i64 { match h { Hv.V(o) => match o { Some(v) => v.len(), None => 1 }, Hv.E => 0 } }
fn gi(h: ref Hi) -> i64 { match h { Hi.I(o) => match o { Some(v) => v, None => 1 }, Hi.E => 0 } }
fn gn(h: ref Hn) -> i64 { match h { Hn.N(o) => match o { Some(n) => n.v, None => 1 }, Hn.E => 0 } }
fn g2(h: ref H2) -> i64 { match h { H2.T(o, k) => match o { Some(s) => s.len() + k, None => k }, H2.E => 0 } }
fn main() {
    { let h = Ho.O(Some(hs("o"))); let g = h; println(f"a{go(g)} {go(h)}"); }
    { let h = Ho.O(None); let g = h; println(f"b{go(g)} {go(h)}"); }
    { let h = Ho.E; let g = h; println(f"c{go(g)} {go(h)}"); }
    { let h = Ho.O(Some(hs("o"))); let g = h; let s1 = take(g); let s2 = take(h); println(f"d{s1.len()} {s2.len()}"); }
    { let h = Hr.R(Ok(hs("r"))); let g = h; println(f"e{gr(g)} {gr(h)}"); }
    { let h = Hr.R(Err(7)); let g = h; println(f"f{gr(g)} {gr(h)}"); }
    { let h = Hv.V(Some([1, 2, 3])); let g = h; println(f"g{gv(g)} {gv(h)}"); }
    { let h = Hi.I(Some(5)); let g = h; println(f"h{gi(g)} {gi(h)}"); }
    { let h = H2.T(Some(hs("t")), 2); let g = h; println(f"j{g2(g)} {g2(h)}"); }
    { let h = Ho.O(Some(hs("o"))); let g = h; let s = take(g); println(f"k{s.len()} {go(h)}"); }
    println("end")
}"#;
    let want = "a25 25\nb1 1\nc0 0\nd25 25\ne25 25\nf7 7\ng3 3\nh5 5\nj27 27\nk25 25\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
