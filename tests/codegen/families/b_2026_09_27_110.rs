//! B-2026-09-27-110 — a `while let` over a heap-BOXED generic enum local whose
//! body reassigns the scrutinee freed the payload twice on every compiled
//! surface: the read-only view classifier declined any body that mentions the
//! scrutinee, so the loop took the owning path, every pass took the box's
//! interior, and the overwrite's displaced-value drop freed it again. In a
//! `while let` the view now stands unless the body hands the scrutinee on or
//! reads the binding after a possible overwrite. `match` / `if let` keep the
//! owning path (c6, c7), and the concrete-enum loop (c8) is the control.
//! No `Drop` anywhere: a reassigned generic box still runs no displaced payload
//! body (B-2026-09-26-21), so bodies would pin that bug rather than this one.

use super::*;

/// B-2026-09-27-110 — a `while let` over a boxed generic enum local that reassigns its scrutinee frees the box once: on a later pass, on the first, twice in one loop, inside an `if`, and with `break`.
#[test]
fn e2e_while_let_boxed_generic_reassigned_scrutinee_frees_once() {
    let src = r#"
struct R { id: i64, tag: String, xs: Vec[i64] }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}-heap-string-long-enough-to-allocate", xs: [i] }; }
enum H[T] { X(T), Y }
enum E { X(R), Y }
fn c1() { let mut g: H[R] = H.X(mk(20)); let mut i = 0; while let H.X(t) = g { println(f"w{t.id} {t.tag.len()} {t.xs[0]}"); i = i + 1; if i > 1 { g = H.Y; } } }
fn c2() { let mut g: H[R] = H.X(mk(21)); while let H.X(t) = g { println(f"w{t.tag}"); g = H.Y; } }
fn c3() { let mut g: H[R] = H.X(mk(22)); let mut i = 0; while let H.X(t) = g { println(f"w{t.tag.len()}"); i = i + 1; if i > 1 { g = H.X(mk(5)); } if i > 2 { g = H.Y; } } }
fn c4() { let mut g: H[R] = H.X(mk(23)); let mut i = 0; while let H.X(t) = g { i = i + 1; if i > 1 { println(t.id); g = H.Y; } else { println(t.tag) } } }
fn c5() { let mut g: H[R] = H.X(mk(24)); let mut i = 0; while let H.X(t) = g { println(t.id); i = i + 1; if i > 2 { break; } } }
fn c6() { let mut g: H[R] = H.X(mk(25)); match g { H.X(t) => { println(f"w{t.id}"); g = H.Y; } H.Y => {} } }
fn c7() { let mut g: H[R] = H.X(mk(26)); if let H.X(t) = g { println(f"w{t.id}"); g = H.Y; } }
fn c8() { let mut g = E.X(mk(27)); let mut i = 0; while let E.X(t) = g { println(f"w{t.id}"); i = i + 1; if i > 1 { g = E.Y; } } }
fn main() {
    c1();
    c2();
    c3();
    c4();
    c5();
    c6();
    c7();
    c8();
    println("end")
}"#;
    let want = "w20 39 20\nw20 39 20\nwt21-heap-string-long-enough-to-allocate\nw39\nw39\nw38\nt23-heap-string-long-enough-to-allocate\n23\n24\n24\n24\nw25\nw26\nw27\nw27\nend\n";
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(interp_errs.is_empty(), "interp errored: {interp_errs:?}");
    assert_eq!(interp_out.join(""), want, "interpreter");
    assert_eq!(run_program(src).as_deref(), Some(want), "AOT");
}
