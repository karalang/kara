//! B-2026-10-04-33 — the ASAN twin of the codegen fixture, over the cells
//! that hold no payload heap a box drop leaves behind (the `Some(String)`
//! cells read twice still lose the interior, which is B-2026-09-29-33).

use super::*;

/// B-2026-10-04-33 — a `None`, a unit variant, both copies consumed, an
/// `Err(i64)` and an `Option[i64]`: no access through a zeroed box word.
#[test]
fn asan_moved_enum_with_boxed_option_payload_is_copied_for_its_reuse() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct Nd { v: i64 }
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
    { let h = Ho.O(None); let g = h; println(f"b{go(g)} {go(h)}"); }
    { let h = Ho.E; let g = h; println(f"c{go(g)} {go(h)}"); }
    { let h = Ho.O(Some(hs("o"))); let g = h; let s1 = take(g); let s2 = take(h); println(f"d{s1.len()} {s2.len()}"); }
    { let h = Hr.R(Err(7)); let g = h; println(f"f{gr(g)} {gr(h)}"); }
    { let h = Hi.I(Some(5)); let g = h; println(f"h{gi(g)} {gi(h)}"); }
    println("end")
}"#,
        &["b1 1", "c0 0", "d25 25", "f7 7", "h5 5", "end"],
        "asan_moved_enum_with_boxed_option_payload_is_copied_for_its_reuse",
        4,
    );
}
