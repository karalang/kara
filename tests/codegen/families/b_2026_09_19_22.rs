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
fn e2e_generic_enum_arg_maybe_handed_back_drops_at_call_end() {
    let Some(out) = run_program(
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
 println(f"{early(true)}");
 println(f"{early(false)}");
 println("end")
}"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\nn_bound_dies\ndR2\nn_bound_back\ndR3\nn_disc_dies\ndR4\nn_disc_back\ndR5\nn_disc_id\ndR6\nn_bound_id\ndR7\nn_wrap_dies\ndR11\nt_bound_dies\ndR12\nt_bound_back\ndR13\nt_disc_dies\ndR14\nt_disc_back\ndR15\nt_disc_id\ndR16\nt_bound_id\ndR17\nt_wrap_dies\ns_bound_dies\ns_disc_back\ns_disc_dies\ns_disc_id\ns_n_disc_dies\ndR30\ndR31\nt_loop\ndR40\nn_ifexpr\ndR42\ndR41\ntwo_a\ndR43\ndR44\ntwo_b\ndR45\nm_dies\ndR46\nm_back\ndR47\nmt_dies\nscalar\ndR50\n1\ndR50\nearly_tail\n0\nend\n", "got:\n{out}");
}
