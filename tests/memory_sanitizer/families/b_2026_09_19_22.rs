//! B-2026-09-19-22, B-2026-09-19-23 -- a by-value boxed generic enum argument
//! the callee may hand back: its payload bodies run at the end of the call when
//! the box does not come back, and a fresh temp's box has an owner.

use super::*;

/// B-2026-09-19-22, B-2026-09-19-23 — a boxed generic enum passed by value to a
/// callee that returns it on SOME paths (`mid`), on every path (`idg`), or
/// inside a struct (`wrap`): named and fresh-temp arguments, bound and
/// discarded results, both legs, a `Drop` payload and a `String` one.
///
/// Before: on the leg where the param dies inside the callee, the payload's
/// `Drop` body ran nowhere compiled (the caller retracted its walk for the leg
/// that hands the box back), and a DISCARDED call lost it on every leg. A
/// fresh-temp argument had no owner for its box at all: 34-42 B leaked per
/// call. `--interp` was right in every cell.
#[test]
fn asan_generic_enum_arg_maybe_handed_back_drops_at_call_end_clean() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum G1[T] { Y(T), N }
struct H[T] { g: G1[T] }
fn mid[T](g: G1[T], c: bool) -> G1[T] { if c { return g } return G1.N }
fn idg[T](g: G1[T]) -> G1[T] { return g }
fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }
fn mk(n: i64) -> R { R { id: n, s: f"ssssssss{n}" } }
fn two[T](a: G1[T], b: G1[T], c: bool) -> G1[T] { if c { return a } return b }
struct W { n: i64 }
impl W { fn keep[T](ref self, g: G1[T], c: bool) -> G1[T] { if c { return g } return G1.N } }
fn early(c: bool) -> i64 { let g: G1[R] = G1.Y(mk(50)); let b = mid(g, c); if c { return 1 } println("early_tail"); 0 }

fn main() {
 { let g: G1[R] = G1.Y(mk(1)); let b = mid(g, false); println("n_bound_dies") }
 { let g: G1[R] = G1.Y(mk(2)); let b = mid(g, true); println("n_bound_back") }
 { let g: G1[R] = G1.Y(mk(3)); mid(g, false); println("n_disc_dies") }
 { let g: G1[R] = G1.Y(mk(4)); mid(g, true); println("n_disc_back") }
 { let g: G1[R] = G1.Y(mk(5)); idg(g); println("n_disc_id") }
 { let g: G1[R] = G1.Y(mk(6)); let b = idg(g); println("n_bound_id") }
 { let g: G1[R] = G1.Y(mk(7)); let h = wrap(g, false); println("n_wrap_dies") }
 { let b = mid(G1.Y(mk(11)), false); println("t_bound_dies") }
 { let b = mid(G1.Y(mk(12)), true); println("t_bound_back") }
 { mid(G1.Y(mk(13)), false); println("t_disc_dies") }
 { mid(G1.Y(mk(14)), true); println("t_disc_back") }
 { idg(G1.Y(mk(15))); println("t_disc_id") }
 { let b = idg(G1.Y(mk(16))); println("t_bound_id") }
 { let h = wrap(G1.Y(mk(17)), false); println("t_wrap_dies") }
 { let b = mid(G1.Y(f"aaaaaaaa-21"), false); println("s_bound_dies") }
 { mid(G1.Y(f"aaaaaaaa-22"), true); println("s_disc_back") }
 { mid(G1.Y(f"aaaaaaaa-23"), false); println("s_disc_dies") }
 { idg(G1.Y(f"aaaaaaaa-24")); println("s_disc_id") }
 { let g: G1[String] = G1.Y(f"aaaaaaaa-25"); mid(g, false); println("s_n_disc_dies") }
 { let mut k = 0; while k < 2 { let b = mid(G1.Y(mk(30 + k)), k == 1); k = k + 1; } println("t_loop") }
 { let g: G1[R] = G1.Y(mk(40)); let b = if true { mid(g, false) } else { G1.N }; println("n_ifexpr") }
 { let x: G1[R] = G1.Y(mk(41)); let y: G1[R] = G1.Y(mk(42)); let b = two(x, y, true); println("two_a") }
 { let x: G1[R] = G1.Y(mk(43)); let y: G1[R] = G1.Y(mk(44)); let b = two(x, y, false); println("two_b") }
 { let w = W { n: 1 }; let g: G1[R] = G1.Y(mk(45)); let b = w.keep(g, false); println("m_dies") }
 { let w = W { n: 1 }; let g: G1[R] = G1.Y(mk(46)); let b = w.keep(g, true); println("m_back") }
 { let w = W { n: 1 }; let b = w.keep(G1.Y(mk(47)), false); println("mt_dies") }
 { let g: G1[i64] = G1.Y(5); let b = two(g, G1.N, false); println("scalar") }
 println(f"{early(true)}")
 println(f"{early(false)}")
 println("end")
}"#,
        &[
            "dR1",
            "n_bound_dies",
            "dR2",
            "n_bound_back",
            "dR3",
            "n_disc_dies",
            "dR4",
            "n_disc_back",
            "dR5",
            "n_disc_id",
            "dR6",
            "n_bound_id",
            "dR7",
            "n_wrap_dies",
            "dR11",
            "t_bound_dies",
            "dR12",
            "t_bound_back",
            "dR13",
            "t_disc_dies",
            "dR14",
            "t_disc_back",
            "dR15",
            "t_disc_id",
            "dR16",
            "t_bound_id",
            "dR17",
            "t_wrap_dies",
            "s_bound_dies",
            "s_disc_back",
            "s_disc_dies",
            "s_disc_id",
            "s_n_disc_dies",
            "dR30",
            "dR31",
            "t_loop",
            "dR40",
            "n_ifexpr",
            "dR42",
            "dR41",
            "two_a",
            "dR43",
            "dR44",
            "two_b",
            "dR45",
            "m_dies",
            "dR46",
            "m_back",
            "dR47",
            "mt_dies",
            "scalar",
            "dR50",
            "1",
            "dR50",
            "early_tail",
            "0",
            "end",
        ],
        "generic_enum_arg_maybe_handed_back",
    );
}
