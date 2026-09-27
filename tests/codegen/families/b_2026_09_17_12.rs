//! B-2026-09-17-12 -- a read-only arm over a local of a heap-boxed GENERIC
//! enum is a view of the box: the enum's own `Drop` body runs before its
//! payload's, and a second read-only match does not free the payload again.

use super::*;

/// B-2026-09-17-12 — the arm binding over a boxed generic payload used to TAKE
/// the box's interior even when it only read it, so the payload's body ran at
/// the arm's end and the enum's own body after it (`dR dG` where design.md
/// Part 8 and `--interp` give `dG dR`), and `twice` / `two_variants` /
/// `looped` freed the interior once per match (`free(): double free`). Every
/// cell is a local scrutinee; the by-value-param spelling is a separate row.
#[test]
fn e2e_boxed_generic_enum_readonly_arm_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
enum G[T] { X(T), Y }
impl[T] Drop for G[T] { fn drop(mut ref self) { println("dG") } }
enum P[T] { A(T), B(T) }
impl[T] Drop for P[T] { fn drop(mut ref self) { println("dP") } }
fn unused() { let g: G[R] = G.X(mk(1)); match g { G.X(t) => { println("a1") } G.Y => { println("a0") } } }
fn read_it() { let g: G[R] = G.X(mk(2)); match g { G.X(t) => { println(f"b{t.id}{t.tag}"); println("b-after") } G.Y => { println("b0") } } }
fn iflet() { let g: G[R] = G.X(mk(3)); if let G.X(t) = g { println(f"c{t.id}") } }
fn twice() { let g: G[R] = G.X(mk(4)); match g { G.X(t) => { println("d1") } G.Y => { println("d0") } }; println("d-mid"); match g { G.X(t) => { println(f"d{t.id}") } G.Y => { println("d0") } } }
fn value() -> i64 { let g: G[R] = G.X(mk(5)); let n = match g { G.X(t) => t.id, G.Y => 0 }; println(f"e{n}"); return n }
fn guarded() { let g: G[R] = G.X(mk(6)); match g { G.X(t) if t.id > 5 => { println(f"f-big{t.id}") } G.X(t) => { println(f"f{t.id}") } G.Y => { println("f0") } } }
fn vecs() { let g: G[Vec[R]] = G.X([mk(7), mk(8)]); match g { G.X(v) => { println(f"g{v.len()}") } G.Y => { println("g0") } } }
fn two_variants() { let g: P[R] = P.B(mk(9)); match g { P.A(t) => { println(f"h{t.id}") } P.B(t) => { println(f"hb{t.id}") } }; match g { P.A(t) => { println(f"h{t.tag}") } P.B(t) => { println(f"hb{t.tag}") } } }
fn fstr() { let g: G[R] = G.X(mk(10)); let s = match g { G.X(t) => f"{t.tag}!", G.Y => "none" }; println(f"i{s}") }
fn looped() { let g: G[R] = G.X(mk(11)); for i in 0..2 { match g { G.X(t) => { println(f"j{t.id}") } G.Y => { println("j0") } } } }
fn main() {
    unused(); println("--");
    read_it(); println("--");
    iflet(); println("--");
    twice(); println("--");
    value(); println("--");
    guarded(); println("--");
    vecs(); println("--");
    two_variants(); println("--");
    fstr(); println("--");
    looped(); println("end");
}
"#,
    ) else {
        return;
    };
    let got: Vec<&str> = out.lines().collect();
    assert_eq!(
        got,
        [
            "a1", "dG", "dR1", "--", "b2t2", "b-after", "dG", "dR2", "--", "c3", "dG", "dR3", "--",
            "d1", "d-mid", "d4", "dG", "dR4", "--", "dG", "dR5", "e5", "--", "f-big6", "dG", "dR6",
            "--", "g2", "dG", "dR7", "dR8", "--", "hb9", "hbt9", "dP", "dR9", "--", "dG", "dR10",
            "it10!", "--", "j11", "j11", "dG", "dR11", "end",
        ],
        "got:
{out}"
    );
}
