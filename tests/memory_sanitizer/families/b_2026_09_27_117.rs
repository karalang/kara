//! B-2026-09-27-117 -- a generic method in a CONCRETE impl block over a
//! generic enum binds its own params from its arguments, not from the
//! receiver's instantiation.

use super::*;

/// B-2026-09-27-117 — the method-call path passed the receiver's type args
/// (`R` of a `G[R]` receiver) as a prefix of the generic method's params, which
/// is right for `impl[T] G[T]` and wrong for `impl G[R]`, whose params are the
/// method's own: `b.wg(7)` built `G.wg$R` taking a `G[R]` for the `i64`
/// argument and failed module verification, and `b.pick(3, 4)` returned an
/// `R`. Covers owned and `ref` receivers, a `String` and an `i64` `U`, a
/// `Display`-bounded `U`, and the generic-impl spelling beside it. The
/// owned-self calls use a payload-free receiver: an owned-self generic method
/// leaks a heap payload on the generic-impl spelling too (B-2026-10-03-31).
#[test]
fn asan_concrete_impl_generic_method_binds_own_params() {
    assert_clean_asan_run(
        r#"struct R { id: i64, tag: String }
enum G[T] { X(T), Y }
impl G[R] {
    fn wg[U](self, u: U) -> i64 { match self { G.X(_) => { return 2; } G.Y => { return 0; } } }
    fn pick[U](ref self, u: U, d: U) -> U { match self { G.X(_) => { return u; } G.Y => { return d; } } }
    fn tag2[U: Display](ref self, u: U) -> String { match self { G.X(_) => { return f"x:{u}"; } G.Y => { return f"y:{u}"; } } }
}
enum H[T] { A(T), B }
impl[T] H[T] { fn hw[U](self, u: U) -> U { match self { H.A(_) => { return u; } H.B => { return u; } } } }
fn main() {
    let b: G[R] = G.X(R { id: 2, tag: f"b{1}" });
    let p1 = b.pick(f"s{1}", f"d{2}"); let p2 = b.pick(3, 4);
    println(f"p {p1} {p2}");
    let t1 = b.tag2(5); let t2 = b.tag2(f"w{6}");
    println(f"t {t1} {t2}");
    let y: G[R] = G.Y;
    let p3 = y.pick(1, 9);
    println(f"y {p3}");
    let y2: G[R] = G.Y;
    let w1 = y.wg(7); let w2 = y2.wg(f"q{1}");
    println(f"w {w1} {w2}");
    let h2: H[i64] = H.A(1);
    let h3 = h2.hw(f"z{2}");
    println(f"h {h3}");
}
"#,
        &["p s1 3", "t x:5 x:w6", "y 9", "w 0 0", "h z2"],
        "B-2026-09-27-117 concrete-impl generic method binds its own params",
    );
}
