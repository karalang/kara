//! integer and float behaviour, SIMD lanes, math intrinsics -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter numerics::
//!
//! New fixtures about integer and float behaviour, SIMD lanes, math intrinsics belong in this file.

use super::*;

#[test]
fn test_branch_tail_fstring_arm_values_round_trip() {
    assert_eq!(
        run(r#"fn use_s(s: String) -> i64 { return s.len(); }

fn main() {
    let n: i64 = "ab".len();
    let c = n > 0;
    let b = if c { f"x{n}" } else { f"y{n}" }.contains("x");
    let d = use_s(if c { f"x{n}" } else { f"y{n}" });
    println(f"b={b} d={d}");
    let p1 = { f"p{n}" }.contains("p");
    let p2 = { { f"q{n}" } }.contains("q");
    let p3 = match n { 0 => f"m{n}", _ => f"o{n}" }.contains("o");
    let p4 = if n < 0 { f"u{n}" } else if c { f"v{n}" } else { f"w{n}" }.contains("v");
    println(f"p1={p1} p2={p2} p3={p3} p4={p4}");
    let p5 = if c { f"r{n}" } else { f"s{n}" }.len();
    let p6 = f"[{if c { f"t{n}" } else { f"z{n}" }}]";
    let p7 = if c { f"c{n}" } else { f"e{n}" } + "-tail";
    println(f"p5={p5} p6={p6} p7={p7}");
    let mut v: Vec[String] = [];
    v.push(if c { f"k{n}" } else { f"l{n}" });
    let p8 = if c { f"g{n}" } else { f"h{n}" };
    println(f"v0={v[0]} p8={p8} again={p8}");
}
"#),
        "b=true d=2\np1=true p2=true p3=true p4=true\np5=2 p6=[t2] p7=c2-tail\nv0=k2 p8=g2 again=g2\n"
    );
}

#[test]
fn a_float_field_compares_by_ieee_semantics_not_by_bits() {
    // B-2026-08-27-9. Every TYPE-DIRECTED comparator routed a float field to
    // `emit_eq_fn_for_type`'s byte loop, and bit equality differs from float
    // equality in exactly the two places IEEE-754 defines specially. So `==`
    // was wrong in BOTH directions on the compiled backends: `0.0` and `-0.0`
    // are numerically EQUAL with different bits (answered false), and NaN is
    // UNEQUAL TO ITSELF with identical bits (answered true).
    //
    // THE OPERANDS COME FROM OPAQUE FUNCTIONS ON PURPOSE. With `0.0 / 0.0`
    // written inline, LLVM constant-folds the whole comparison and returns the
    // RIGHT answer without ever running the emitted comparator — which is
    // exactly how the first version of this fix looked correct while the
    // runtime path was untouched. A test that lets the folder answer proves
    // nothing about codegen.
    //
    // Four shapes because four routes reach the comparator family: a shared
    // struct's direct field, a `Vec[f64]` element (via the vec comparator), a
    // PLAIN struct with a Vec field (a Vec field is what pulls a plain struct
    // onto the type-directed path at all), and a nested plain struct inside a
    // shared one. Codegen twin: `test_e2e_float_field_compares_by_ieee`.
    let src = "#[derive(PartialEq)]
        shared struct Sf { x: f64 }
        #[derive(PartialEq)]
        shared struct Sv { v: Vec[f64] }
        #[derive(PartialEq)]
        struct Pv { v: Vec[f64] }
        #[derive(PartialEq)]
        struct Inner { x: f64 }
        #[derive(PartialEq)]
        shared struct Outer { i: Inner }
        fn nan64(z: f64) -> f64 { return z / z; }
        fn negzero(z: f64) -> f64 { return -z; }
        fn one(x: f64) -> Vec[f64] {
            let mut v: Vec[f64] = Vec.new();
            v.push(x);
            return v;
        }
        fn main() {
            let p = 0.0;
            let n = negzero(p);
            let q = nan64(p);

            println(f\"scalar-negzero={p == n}\");
            println(f\"scalar-nan={q == q}\");

            let sa = Sf { x: p };
            let sb = Sf { x: n };
            let sq1 = Sf { x: q };
            let sq2 = Sf { x: q };
            println(f\"shared-negzero={sa == sb}\");
            println(f\"shared-nan={sq1 == sq2}\");

            let va = Sv { v: one(p) };
            let vb = Sv { v: one(n) };
            let vq1 = Sv { v: one(q) };
            let vq2 = Sv { v: one(q) };
            println(f\"sharedvec-negzero={va == vb}\");
            println(f\"sharedvec-nan={vq1 == vq2}\");

            let pa = Pv { v: one(p) };
            let pb = Pv { v: one(n) };
            let pq1 = Pv { v: one(q) };
            let pq2 = Pv { v: one(q) };
            println(f\"plainvec-negzero={pa == pb}\");
            println(f\"plainvec-nan={pq1 == pq2}\");

            let o1 = Outer { i: Inner { x: p } };
            let o2 = Outer { i: Inner { x: n } };
            println(f\"nested-negzero={o1 == o2}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "scalar-negzero=true\nscalar-nan=false\n\
         shared-negzero=true\nshared-nan=false\n\
         sharedvec-negzero=true\nsharedvec-nan=false\n\
         plainvec-negzero=true\nplainvec-nan=false\n\
         nested-negzero=true\n"
    );
}

#[test]
fn test_sqrt_float() {
    assert_eq!(run("fn main() { println((16.0f64).sqrt()); }"), "4\n");
    assert_eq!(
        run("fn main() { println((2.0f64).sqrt()); }"),
        "1.4142135623730951\n"
    );
}

/// B-2026-08-30-34 — A `u64` AT OR ABOVE 2^63 CONVERTED TO EVERY FLOAT TYPE AS
/// ITS NEGATIVE TWO'S-COMPLEMENT IMAGE, on the interpreter only.
///
/// `Value::Int` is a signed `i128` with no signedness tag — deliberately — so a
/// `u64` past 2^63 rides as its negative twin and each reader that must know
/// consults the recorded type (`span_unsigned_int_width` / `int_width_at`). The
/// float conversions were the readers that never did, so `u64::MAX as f64`
/// answered -1 here while all three compiled surfaces answered
/// 18446744073709552000. The cast-path residual of B-2026-07-04-8, which built
/// that u64 model for printing, comparison and sorting and never mentions casts.
///
/// It was NOT one site. An integer reaches a float slot by ten routes and each
/// converted the carrier independently: the `as` cast, `to_f32`/`to_f64`, a
/// float-annotated `let`, a call argument, a method argument, a function's tail
/// expression, an explicit `return`, a struct-literal field, a field
/// assignment, and a `Vec[f64]` element store. Measured over a 9-value x
/// 10-shape sweep: 45 of 90 rows diverged from the compiled backends before,
/// 0 of 90 after.
///
/// The last two rows are the CONTROL, and they are what makes this test able to
/// fail in both directions: `ints` pins that the integer-target casts and the
/// division still read the carrier as they always have (`mx as i64` is -1,
/// `mx as u32` is 4294967295), so a "fix" that normalized the carrier
/// everywhere — breaking B-2026-07-04-8's model — would fail here rather than
/// pass quietly. `u128` pins the widening this fix also had to correct:
/// `u64 as u128` kept the sign and stored -1, which printing and comparison
/// got wrong too, not just the float read.
///
/// Twin: `tests/codegen.rs::e2e_u64_above_2_63_converts_to_float_as_unsigned`.
#[test]
fn u64_above_2_63_converts_to_every_float_type_as_unsigned() {
    // Literal seed rather than `env.args().len()` — an in-process interpreter
    // test sees the TEST binary's argv; the codegen twin needs an opaque seed
    // to survive -O2 folding and 1 is what that yields under its harness.
    assert_eq!(
        run(
            r#"
fn tailf(v: u64) -> f64 { v }
fn retf(v: u64) -> f64 { return v; }
fn takef(x: f64) -> f64 { x }
struct S { f: f64 }
impl S { fn setm(mut ref self, x: f64) -> f64 { self.f = x; self.f } }
fn main() {
    let n = 1i64;
    let zero: i64 = n - 1;
    let mx: u64 = (18446744073709551615u64 + (zero as u64));
    let hi: u64 = (9223372036854775808u64 + (zero as u64));
    let lo: u64 = (9223372036854775807u64 + (zero as u64));
    println(f"cast {(mx as f64)} {(hi as f64)} {(lo as f64)}");
    println(f"f32  {(mx as f32)} {(hi as f32)}");
    println(f"half {(mx as f16)} {(mx as bf16)}");
    println(f"meth {mx.to_f64()} {mx.to_f32()}");
    let slot: f64 = mx;
    println(f"slot {slot}");
    println(f"call {takef(mx)} {tailf(mx)} {retf(mx)}");
    let mut s: S = S { f: mx };
    println(f"fldi {s.f}");
    s.f = hi;
    println(f"flda {s.f} {s.setm(mx)}");
    let w: u128 = (mx as u128);
    println(f"u128 {w} {(w as f64)}");
    let mut vf: Vec[f64] = [];
    vf.push(mx);
    println(f"push {vf[0]}");
    println(f"ints {mx} {(mx as i64)} {(mx as u32)} {(mx / 3u64)}");
}
"#
        ),
        "cast 18446744073709552000 9223372036854776000 9223372036854776000\nf32  18446744073709552000 9223372036854776000\nhalf inf 18446744073709552000\nmeth 18446744073709552000 18446744073709552000\nslot 18446744073709552000\ncall 18446744073709552000 18446744073709552000 18446744073709552000\nfldi 18446744073709552000\nflda 9223372036854776000 18446744073709552000\nu128 18446744073709551615 18446744073709552000\npush 18446744073709552000\nints 18446744073709551615 -1 4294967295 6148914691236517205\n"
    );
}

/// B-2026-08-30-9 — a `Vector[T, N]` renders its lanes through the TYPE-DIRECTED
/// renderer, so an unsigned lane above `i64::MAX` reads back as unsigned.
///
/// The headline shape is the SELF-CONTRADICTION inside one program: `u[0]` and
/// `u` disagreed about the very same lane. The indexed read reinterprets by the
/// expression's static type and printed 18446744073709551615; the whole vector
/// fell through `render_typed_mode`'s arms to the untyped `Display` in
/// `value.rs`, which sees a bare `Value::Int(-1)` carrier and prints `-1`.
/// Asserting both in ONE program is the point — either line alone looks right.
///
/// The nested cases (`Vec` / tuple of a vector) are worth pinning here because
/// they come from the container arms recursing INTO the new one, so they are
/// what proves the fix was made at the recursion point rather than
/// special-cased at depth 0.
///
/// They HAD no codegen twin when this was written, because the compiled
/// backends could not render a nested vector at all — they aborted with
/// `emit_display_fn_for_type: type_name 'Vector_u64_2' not yet supported`.
/// B-2026-08-30-39 closed that, and the `Vec` and tuple rows are now twinned in
/// `tests/codegen.rs::e2e_nested_vector_display_agrees_with_the_interpreter`.
///
/// `inopt` is the one row that is still interpreter-only, and for a reason
/// unrelated to vectors: an `Option` whose payload makes the option's LLVM
/// value a non-`String` struct has no f-string Display path in codegen at all
/// — `Option[Vec[i64]]` fails identically, while `Option[i64]` is fine. Filed
/// separately; do not read its absence here as a vector gap.
///
/// `i64` lanes are in the table as the control that keeps the fix honest — a
/// signed `-1` must still print `-1`, which is what stops "render lanes
/// unsigned" from being implemented as "render lanes unsigned unconditionally".
///
/// Twin (depth-0 rows only): `tests/codegen.rs::e2e_vector_display_agrees_with_the_interpreter`.
#[test]
fn vector_display_renders_wide_unsigned_lanes_as_unsigned() {
    assert_eq!(
        run(
            r#"
fn main() {
    let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);
    println(f"lane {u[0]}");
    println(f"whole {u}");
    println(f"bare");
    println(u);
    let w: Vector[u128, 2] = Vector[u128, 2](340282366920938463463374607431768211455u128, 1u128);
    println(f"u128 {w[0]} {w}");
    let b: Vector[u64, 2] = Vector[u64, 2](9223372036854775808u64, 1u64);
    println(f"bound {b}");
    let s: Vector[i64, 2] = Vector[i64, 2](-1, 1);
    println(f"signed {s}");
    let n: Vector[u8, 4] = Vector[u8, 4](200u8, 1u8, 2u8, 3u8);
    println(f"narrow {n}");
    let vs: Vec[Vector[u64, 2]] = [u];
    println(f"invec {vs}");
    let t = (u, 7i64);
    println(f"intup {t}");
    let o: Option[Vector[u64, 2]] = Some(u);
    println(f"inopt {o}");
}
"#
        ),
        "lane 18446744073709551615\nwhole Vector(18446744073709551615, 1)\nbare\nVector(18446744073709551615, 1)\nu128 340282366920938463463374607431768211455 Vector(340282366920938463463374607431768211455, 1)\nbound Vector(9223372036854775808, 1)\nsigned Vector(-1, 1)\nnarrow Vector(200, 1, 2, 3)\ninvec [Vector(18446744073709551615, 1)]\nintup (Vector(18446744073709551615, 1), 7)\ninopt Some(Vector(18446744073709551615, 1))\n"
    );
}

/// B-2026-08-30-26 — the interpreter half of the int <-> `bf16` conversion
/// fixture, and a PARITY PIN rather than a regression test: the interpreter was
/// never wrong here. It performed every one of these while both compiled
/// backends refused to build the program at all, which is what made the bug a
/// run-vs-build divergence instead of a wrong answer.
///
/// It is pinned because the codegen fix routes int->bf16 through f32 while this
/// side goes through f64, so the two agree by a double-rounding argument
/// (p1 >= 2*p2 + 2) rather than by sharing code. If either route is ever
/// changed, this and its codegen twin move apart and both fail.
///
/// Twin: `tests/codegen.rs::e2e_int_bf16_conversions_are_lowered_through_f32`.
#[test]
fn int_bf16_conversions_round_and_saturate() {
    // The seed is the literal 1 rather than `env.args().len()`: the codegen
    // twin needs an opaque seed to survive -O2 constant folding and 1 is what
    // that yields under its harness, while `env.args()` in an IN-PROCESS
    // interpreter test reports the TEST binary's argv (same reasoning as
    // `test_optres_tuple_payload_is_owned_exactly_once`). The interpreter folds
    // nothing, so it needs no opacity — only the same values.
    assert_eq!(
        run(
            r#"
fn main() {
    let n = 1i64;
    let zero: i64 = n - 1;
    let onef: f32 = (n as f32);
    let a1: i8 = (-127i8 + (zero as i8));
    let a2: i8 = (3i8 + (zero as i8));
    let a3: u8 = (255u8 + (zero as u8));
    let a4: i16 = (257i16 + (zero as i16));
    let a5: i16 = (-12345i16 + (zero as i16));
    let a6: u16 = (65535u16 + (zero as u16));
    let a7: i32 = (2147483647i32 + (zero as i32));
    let a8: u32 = (4294967295u32 + (zero as u32));
    let a9: i64 = (9223372036854775807i64 + zero);
    let a10: i64 = (-9223372036854775807i64 + zero);
    let a11: u64 = (6148914691236517205u64 + (zero as u64));
    println(f"i2b {(a1 as bf16)} {(a2 as bf16)} {(a3 as bf16)} {(a4 as bf16)}");
    println(f"i2b {(a5 as bf16)} {(a6 as bf16)} {(a7 as bf16)} {(a8 as bf16)}");
    println(f"i2b {(a9 as bf16)} {(a10 as bf16)} {(a11 as bf16)}");
    let b1: bf16 = ((2.5f32 * onef) as bf16);
    let b2: bf16 = ((-2.5f32 * onef) as bf16);
    let b3: bf16 = ((1000.0f32 * onef) as bf16);
    let b4: bf16 = ((-1000.0f32 * onef) as bf16);
    let b5: bf16 = ((1.0e30f32 * onef) as bf16);
    let b6: bf16 = ((-1.0e30f32 * onef) as bf16);
    let b7: bf16 = ((0.4f32 * onef) as bf16);
    println(f"b2i {(b1 as i8)} {(b2 as i8)} {(b3 as i8)} {(b4 as i8)}");
    println(f"b2i {(b1 as u8)} {(b2 as u8)} {(b3 as u8)} {(b7 as u8)}");
    println(f"b2i {(b5 as i32)} {(b6 as i32)} {(b5 as u32)} {(b6 as u32)}");
    println(f"b2i {(b5 as i64)} {(b6 as i64)} {(b3 as i16)} {(b4 as i16)}");
}
"#
        ),
        "i2b -127 3 255 256\ni2b -12352 65536 2147483648 4294967296\ni2b 9223372036854776000 -9223372036854776000 6160924290242839000\nb2i 2 -2 127 -128\nb2i 2 0 255 0\nb2i 2147483647 -2147483648 4294967295 0\nb2i 9223372036854775807 -9223372036854775808 1000 -1000\n"
    );
}

#[test]
fn test_float_math_transcendental_and_rounding() {
    // Scalar transcendental + rounding math (`crate::float_math`): unary
    // `sin`/`cos`/`tan`/`exp`/`ln`/`log2`/`floor`/`ceil`/`round` and binary
    // `pow`/`atan2`, delegating to Rust's `f64::*`. Exact-result inputs so the
    // assertion is platform-independent (codegen's libm twin must match).
    assert_eq!(run("fn main() { println((0.0f64).sin()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).cos()); }"), "1\n");
    assert_eq!(run("fn main() { println((0.0f64).tan()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).exp()); }"), "1\n");
    assert_eq!(run("fn main() { println((1.0f64).ln()); }"), "0\n");
    assert_eq!(run("fn main() { println((1024.0f64).log2()); }"), "10\n");
    assert_eq!(
        run("fn main() { println((2.0f64).pow(10.0f64)); }"),
        "1024\n"
    );
    assert_eq!(run("fn main() { println((0.0f64).atan2(1.0f64)); }"), "0\n");
    assert_eq!(run("fn main() { println((2.7f64).floor()); }"), "2\n");
    assert_eq!(run("fn main() { println((2.2f64).ceil()); }"), "3\n");
    // `round` is half-away-from-zero (Rust `f64::round` / `llvm.round`).
    assert_eq!(run("fn main() { println((2.5f64).round()); }"), "3\n");
    assert_eq!(run("fn main() { println((-2.5f64).round()); }"), "-3\n");
    // Irrational checks — the interpreter is the f64 reference oracle.
    assert_eq!(
        run("fn main() { println((1.0f64).sin()); }"),
        "0.8414709848078965\n"
    );
    assert_eq!(
        run("fn main() { println((2.0f64).ln()); }"),
        "0.6931471805599453\n"
    );
}

#[test]
fn test_float_math_inverse_hyperbolic_and_extras() {
    // Second wave of the `crate::float_math` surface: inverse trig
    // (`asin`/`acos`/`atan`), hyperbolics (`sinh`/`cosh`/`tanh`), and
    // `exp2`/`log10`/`trunc`. Exact-result inputs so the assertion is
    // platform-independent (codegen's libm twin must match — the inverse-trig /
    // hyperbolic set lowers to direct libm calls, the rest to LLVM intrinsics).
    assert_eq!(run("fn main() { println((0.0f64).asin()); }"), "0\n");
    assert_eq!(run("fn main() { println((1.0f64).acos()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).atan()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).sinh()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).cosh()); }"), "1\n");
    assert_eq!(run("fn main() { println((0.0f64).tanh()); }"), "0\n");
    assert_eq!(run("fn main() { println((3.0f64).exp2()); }"), "8\n");
    assert_eq!(run("fn main() { println((1000.0f64).log10()); }"), "3\n");
    assert_eq!(run("fn main() { println((2.7f64).trunc()); }"), "2\n");
    assert_eq!(run("fn main() { println((-2.7f64).trunc()); }"), "-2\n");
    // Irrational check — the interpreter is the f64 reference oracle. Compared
    // numerically (not by exact string) because asin lowers to libm, whose
    // last ULP differs macOS vs Linux (see `assert_prints_float_near`).
    assert_prints_float_near(
        "fn main() { println((0.5f64).asin()); }",
        std::f64::consts::FRAC_PI_6,
    );
}

#[test]
fn test_float_math_hypot_inverse_hyperbolic_exp1p() {
    // Third wave of the `crate::float_math` surface: `hypot` (binary) plus the
    // inverse hyperbolics (`asinh`/`acosh`/`atanh`) and `exp_m1`/`ln_1p`. All
    // lower to direct libm calls in codegen (no LLVM intrinsic; `exp_m1`/`ln_1p`
    // map to libm's `expm1`/`log1p`). Exact-result inputs for a
    // platform-independent assertion; NB `cbrt` is intentionally excluded —
    // Rust's in-Rust `f64::cbrt` disagrees with libm's, breaking run == build.
    assert_eq!(run("fn main() { println((3.0f64).hypot(4.0f64)); }"), "5\n");
    assert_eq!(run("fn main() { println((0.0f64).asinh()); }"), "0\n");
    assert_eq!(run("fn main() { println((1.0f64).acosh()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).atanh()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).exp_m1()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).ln_1p()); }"), "0\n");
    // Irrational check — the interpreter is the f64 reference oracle. Compared
    // numerically (not by exact string): asinh lowers to libm, whose last ULP
    // differs macOS vs Linux (see `assert_prints_float_near`).
    assert_prints_float_near(
        "fn main() { println((0.7f64).asinh()); }",
        0.6526665660823557,
    );
}

#[test]
fn test_signum_signed_int_and_float() {
    // `x.signum()`: signed ints → -1 / 0 / 1 (`iN::signum`), floats →
    // -1.0 / +1.0 / NaN (`f64::signum`). The float form carries the sign of a
    // signed zero (`(-0.0).signum() == -1.0`) and preserves NaN — codegen
    // mirrors this with `copysign` + a NaN guard.
    assert_eq!(run("fn main() { println((42i64).signum()); }"), "1\n");
    assert_eq!(run("fn main() { println((-42i64).signum()); }"), "-1\n");
    assert_eq!(run("fn main() { println((0i64).signum()); }"), "0\n");
    assert_eq!(
        run("fn main() { println((0i32 - 7i32).signum()); }"),
        "-1\n"
    );
    assert_eq!(run("fn main() { println((3.5f64).signum()); }"), "1\n");
    assert_eq!(run("fn main() { println((-3.5f64).signum()); }"), "-1\n");
    // +0.0 → 1.0; -0.0 (built as `0.0 * -1.0`) → -1.0; NaN → NaN.
    assert_eq!(run("fn main() { println((0.0f64).signum()); }"), "1\n");
    assert_eq!(
        run("fn main() { let z: f64 = 0.0 * (0.0 - 1.0); println(z.signum()); }"),
        "-1\n"
    );
    assert_eq!(
        run("fn main() { let n: f64 = (0.0 - 1.0).sqrt(); println(n.signum()); }"),
        "NaN\n"
    );
}

#[test]
fn test_float_recip_and_angle_conversions() {
    // `recip` = `1.0 / x`; `to_degrees` / `to_radians` scale by Rust's exact
    // constants. Codegen replicates the same `fdiv`/`fmul` + constants, so the
    // irrational results below are bit-exact between `karac run` and
    // `karac build` (the interpreter is the f64 reference oracle).
    assert_eq!(run("fn main() { println((4.0f64).recip()); }"), "0.25\n");
    assert_eq!(run("fn main() { println((0.5f64).recip()); }"), "2\n");
    assert_eq!(run("fn main() { println((0.0f64).to_degrees()); }"), "0\n");
    assert_eq!(run("fn main() { println((0.0f64).to_radians()); }"), "0\n");
    assert_eq!(
        run("fn main() { println((1.0f64).to_radians()); }"),
        "0.017453292519943295\n"
    );
    assert_eq!(
        run("fn main() { println((1.0f64).to_degrees()); }"),
        "57.29577951308232\n"
    );
}

#[test]
fn test_float_copysign_and_fract() {
    // `copysign(x, y)` carries `y`'s sign onto `|x|` (a single sign-bit op);
    // `fract` = `x - x.trunc()`, sign-preserving. Both are exact IEEE
    // operations, so codegen (`llvm.copysign`, `fsub` against `llvm.trunc`)
    // matches the interpreter's `f64::*` bit-for-bit.
    assert_eq!(
        run("fn main() { println((3.5f64).copysign(-1.0f64)); }"),
        "-3.5\n"
    );
    assert_eq!(
        run("fn main() { println((-3.5f64).copysign(1.0f64)); }"),
        "3.5\n"
    );
    assert_eq!(run("fn main() { println((2.75f64).fract()); }"), "0.75\n");
    assert_eq!(run("fn main() { println((-2.75f64).fract()); }"), "-0.75\n");
    assert_eq!(run("fn main() { println((5.0f64).fract()); }"), "0\n");
    // Irrational fraction round-trips exactly (both sides do `x - trunc(x)`).
    assert_eq!(run("fn main() { println((0.1f64).fract()); }"), "0.1\n");
}

#[test]
fn test_float_bit_reinterpret_roundtrip() {
    // IEEE-754 bit reinterpretation: `to_bits`/`to_bits32` and the inverse
    // `bits_as_f64`/`bits_as_f32` round-trip a float through its integer bits.
    assert_eq!(
        run("fn main() { let d: f64 = 3.5; println(d.to_bits().bits_as_f64() == d); }"),
        "true\n"
    );
    assert_eq!(
        run("fn main() { let f: f32 = 2.25; println(f.to_bits32().bits_as_f32() == f); }"),
        "true\n"
    );
    // 1.0f64 has the bit pattern 0x3FF0000000000000.
    assert_eq!(
        run("fn main() { println((1.0f64).to_bits()); }"),
        "4607182418800017408\n"
    );
}

#[test]
fn test_abs_int_min_traps() {
    // `iN::MIN.abs()` has no representable result and traps as integer
    // overflow, matching the checked-neg arm — not a panic/ICE.
    let errors =
        runtime_errors("fn main() { let x = -9223372036854775807i64 - 1i64; println(x.abs()); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected integer-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_i64_div_rem_euclid() {
    // `i64::{div_euclid, rem_euclid}` — the remainder is always non-negative,
    // so the quotient rounds toward negative infinity. Exercises all four
    // dividend/divisor sign combinations against Rust's semantics.
    let out = run("fn main() {\n\
             println((-7i64).div_euclid(3i64));\n\
             println((-7i64).rem_euclid(3i64));\n\
             println((7i64).div_euclid(3i64));\n\
             println((7i64).rem_euclid(3i64));\n\
             println((-7i64).div_euclid(-3i64));\n\
             println((-7i64).rem_euclid(-3i64));\n\
             println((7i64).div_euclid(-3i64));\n\
             println((7i64).rem_euclid(-3i64));\n\
             println((6i64).div_euclid(3i64));\n\
             println((6i64).rem_euclid(3i64));\n\
         }");
    assert_eq!(out, "-3\n2\n2\n1\n3\n2\n-2\n1\n2\n0\n");
}

#[test]
fn test_i64_div_rem_euclid_traps() {
    // Same trap set as `/` and `%`: a zero divisor is `division by zero`, and
    // `i64::MIN.{div,rem}_euclid(-1)` overflows (`checked_*_euclid` → None).
    for m in ["div_euclid", "rem_euclid"] {
        let errors = runtime_errors(&format!(
            "fn main() {{ let z: i64 = 0; println((5i64).{m}(z)); }}"
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("division by zero")),
            "expected division-by-zero trap for {m}, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        let errors = runtime_errors(&format!(
            "fn main() {{ let m: i64 = -9223372036854775807i64 - 1i64; \
             let n: i64 = 0i64 - 1i64; println(m.{m}(n)); }}"
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "expected MIN/-1 overflow trap for {m}, got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_overflow_arith_methods_width_correct() {
    // checked/saturating/overflowing _add/_sub/_mul are width-aware: the
    // receiver's width (recovered from the typechecker) sizes the overflow
    // check. i32 2e9+2e9 overflows (checked None / saturating MAX / overflowing
    // wraps+flags); i64 stays exact; u8 sub underflows to 0; u32 mul overflows.
    let out = run("fn main() {\n\
             let a = 2000000000i32;\n\
             match a.checked_add(2000000000i32) { Some(v) => println(v), None => println(-1i32) }\n\
             match a.checked_add(100i32) { Some(v) => println(v), None => println(-1i32) }\n\
             println(a.saturating_add(2000000000i32));\n\
             let u: u8 = 3u8;\n\
             println(u.saturating_sub(10u8));\n\
             let pair = a.overflowing_add(2000000000i32);\n\
             println(pair.0);\n\
             if pair.1 { println(1i32); } else { println(0i32); }\n\
             let big = 9000000000000000000i64;\n\
             match big.checked_add(big) { Some(v) => println(v), None => println(-7i64) }\n\
             let w: u32 = 4000000000u32;\n\
             println(w.checked_mul(2u32).is_none());\n\
             println(w.saturating_add(1000000000u32));\n\
         }");
    assert_eq!(
        out,
        "-1\n2000000100\n2147483647\n0\n-294967296\n1\n-7\ntrue\n4294967295\n"
    );
}

#[test]
fn test_int_pow_values_and_zero_exponent() {
    // `n.pow(k)` is repeated multiplication; `k` is `u32`. `pow(0)` is 1 for any
    // base. Defined on every integer width (here i64 / u64).
    let out = run("fn main() {\n\
             println(2i64.pow(10u32));\n\
             println(3i64.pow(0u32));\n\
             println(5i64.pow(3u32));\n\
             let e: u32 = 6;\n\
             println(2i64.pow(e));\n\
             let b: u64 = 1000000;\n\
             println(b.pow(2u32));\n\
         }");
    assert_eq!(out, "1024\n1\n125\n64\n1000000000000\n");
}

#[test]
fn test_int_pow_overflow_traps_at_receiver_width() {
    // `pow` traps `integer overflow` at the receiver width, like the `*` it
    // iterates: `u8 16^2 = 256` exceeds the u8 range and traps (it does not
    // silently widen to i64).
    let errors = runtime_errors("fn main() { let x: u8 = 16; println(x.pow(2u32)); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected u8 pow-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // i64 10^19 overflows i64 (10^18 fits).
    let errors = runtime_errors("fn main() { let b: i64 = 10; println(b.pow(19u32)); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected i64 pow-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_int_float_min_max() {
    // `a.min(b)` / `a.max(b)` on numeric scalars pick the smaller / larger,
    // like Rust's `Ord::min`/`max` (ints) and `f64::min`/`max` (floats). The
    // arg is the same numeric type (a bare literal coerces). Defined on signed,
    // unsigned, and float widths.
    let out = run("fn main() {\n\
             println(7i64.min(3i64));\n\
             println(7i64.max(3i64));\n\
             println((0 - 5i64).max(0i64));\n\
             let x: f64 = 1.5;\n\
             let y: f64 = 2.5;\n\
             println(x.min(y));\n\
             println(x.max(y));\n\
             let u: u8 = 200;\n\
             println(u.min(100u8));\n\
             let w: u32 = 4000000000;\n\
             println(w.max(1u32));\n\
         }");
    assert_eq!(out, "3\n7\n0\n1.5\n2.5\n100\n4000000000\n");
}

#[test]
fn test_int_float_clamp_method() {
    // `v.clamp(lo, hi)` (method sibling of the `clamp` free fn): pins `v` into
    // `[lo, hi]`, nested-bound form so `lo` wins on an inverted range. Defined
    // on signed / unsigned / float widths. Line 4 (`7.clamp(10, 5)`) pins the
    // inverted-range case, line 5–6 float, 7–8 unsigned.
    let out = run("fn main() {\n\
             println(15i64.clamp(0i64, 10i64));\n\
             println((0 - 3i64).clamp(0i64, 10i64));\n\
             println(5i64.clamp(0i64, 10i64));\n\
             println(7i64.clamp(10i64, 5i64));\n\
             let x: f64 = 1.5;\n\
             println(x.clamp(2.0, 3.0));\n\
             let y: f64 = 2.5;\n\
             println(y.clamp(0.0, 2.0));\n\
             let u: u8 = 200;\n\
             println(u.clamp(0u8, 100u8));\n\
             let w: u32 = 4000000000;\n\
             println(w.clamp(1u32, 4294967295u32));\n\
         }");
    assert_eq!(out, "10\n0\n5\n10\n2\n2\n100\n4000000000\n");
}

#[test]
fn test_next_power_of_two_overflow_traps() {
    // The result would exceed the width (`u8 129` → 256 doesn't fit u8), so it
    // traps `integer overflow` — the same trap policy as `*`/`pow`.
    let errors = runtime_errors("fn main() { let g: u8 = 129; println(g.next_power_of_two()); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected an integer-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_float_to_int_saturating() {
    // phase-8 cast slice 2: saturating clamps to the target's MIN/MAX and
    // truncates toward zero in range.
    assert_eq!(
        run("fn main() { println((3.7f64).saturating_to_i32()); }"),
        "3\n"
    );
    assert_eq!(
        run("fn main() { println((-3.7f64).saturating_to_i32()); }"),
        "-3\n"
    );
    assert_eq!(
        run("fn main() { println((1e30f64).saturating_to_i32()); }"),
        "2147483647\n"
    );
    assert_eq!(
        run("fn main() { println((-1e30f64).saturating_to_i32()); }"),
        "-2147483648\n"
    );
    assert_eq!(
        run("fn main() { println((1e30f64).saturating_to_u8()); }"),
        "255\n"
    );
    assert_eq!(
        run("fn main() { println((-1.0f64).saturating_to_u8()); }"),
        "0\n"
    );
}

#[test]
fn test_float_to_int_wrapping() {
    // Modular truncation: 300 → 44 in i8, 256 → 0 / 257 → 1 in u8.
    assert_eq!(
        run("fn main() { println((300.0f64).wrapping_to_i8()); }"),
        "44\n"
    );
    assert_eq!(
        run("fn main() { println((256.0f64).wrapping_to_u8()); }"),
        "0\n"
    );
    assert_eq!(
        run("fn main() { println((257.9f64).wrapping_to_u8()); }"),
        "1\n"
    );
}

#[test]
fn test_float_to_int_checked() {
    // `checked_*` → `Some(trunc)` in range, `None` on NaN / out-of-range.
    assert_eq!(
        run("fn main() { match (1.5f64).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "1\n"
    );
    assert_eq!(
        run("fn main() { match (1e30f64).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "-1\n"
    );
    assert_eq!(
        run("fn main() { match (f64.NAN).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "-1\n"
    );
}

#[test]
fn test_float_to_int_trunc_traps_out_of_range() {
    // `trunc_*` is the trapping form — out-of-range / NaN records a structured
    // "float-to-int out of range" runtime error (not a panic/ICE).
    let errors = runtime_errors("fn main() { println((1e30f64).trunc_to_i32()); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("float-to-int out of range")),
        "expected out-of-range trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // In-range `trunc_*` returns the truncated value.
    assert_eq!(
        run("fn main() { println((42.9f64).trunc_to_i32()); }"),
        "42\n"
    );
}

#[test]
fn test_int_to_float_methods() {
    // Symmetric `to_f32` / `to_f64` widen an integer to a float.
    assert_eq!(run("fn main() { println((42i64).to_f64()); }"), "42\n");
    assert_eq!(run("fn main() { println((42i32).to_f32()); }"), "42\n");
}

#[test]
fn test_float_arithmetic() {
    assert_eq!(run("fn main() { println(1.5 + 2.5); }"), "4\n");
}

#[test]
fn test_float_int_literal_promotion() {
    // B-2026-07-04-12: a float operand + an unsuffixed integer LITERAL — the
    // typechecker promotes the `1` to `f64` (`a + 1` type-checks) and codegen
    // lowers it as `1.0`, but the tree-walker used to evaluate the bare literal
    // as `Value::Int` and error on the `(Float, Int)` pair. `run` must now match
    // check + `build`: the literal promotes to float. Covers both operand
    // orders, every arithmetic op, a comparison, an `f32` receiver, and a
    // literal on the receiver side of a method arg.
    assert_eq!(
        run(r#"
        fn main() {
            let a: f64 = 2.0;
            println(f"{a + 1} {1 + a} {a * 3} {a - 1} {a / 2}");
            println(f"{a < 5} {a >= 2}");
            let f: f32 = 1.5;
            println(f"{f + 2}");
        }
    "#),
        "3 3 6 1 1\ntrue true\n3.5\n"
    );
}

#[test]
fn test_int_arithmetic_literal_unaffected_by_float_promotion() {
    // The promotion is gated on a FLOAT peer — a pure integer expression with a
    // literal is untouched (stays i64, no accidental float widening).
    assert_eq!(
        run("fn main() { let n: i64 = 7; println(f\"{n + 1} {2 * n} {n / 2}\"); }"),
        "8 14 3\n"
    );
}

#[test]
fn test_float_compound_assign_int_literal_promotion() {
    // B-2026-07-04-12 also covers the CompoundAssign path (`x += 1` with
    // `x: f64`), which evaluates its binop via a separate `eval_binary` call.
    assert_eq!(
        run(r#"
        fn main() {
            let mut a: f64 = 2.0;
            a += 1;
            a *= 2;
            a -= 3;
            a /= 2;
            println(f"{a}");
        }
    "#),
        "1.5\n"
    );
}

// ── `Vec.filled(n, val)` (design.md:1631) ───────────────────────

#[test]
fn test_vec_filled_i64() {
    // `Vec.filled(3, 7)` → length 3, all 7s.
    let out = run(r#"
        fn main() {
            let v: Vec[i64] = Vec.filled(3, 7);
            println(v.len());
            println(v[0]);
            println(v[2]);
        }
    "#);
    assert_eq!(out, "3\n7\n7\n");
}

#[test]
fn test_vec_nested_indexed_write_round_trip() {
    // `rows[r][c] = val` on `Vec[Vec[T]]` — the kata-6 _faster
    // shape that previously needed a flat-layout workaround.
    // Pre-fix, the interpreter's set_index silently no-op'd on
    // non-Identifier targets, and codegen errored "Index
    // assignment target must be a variable". Both now route
    // through to the leaf slot.
    let out = run(r#"
        fn main() {
            let mut rows: Vec[Vec[i64]] = Vec.new();
            let r0: Vec[i64] = Vec.filled(3, 0);
            let r1: Vec[i64] = Vec.filled(3, 0);
            rows.push(r0);
            rows.push(r1);
            rows[0][1] = 42;
            rows[1][2] = 99;
            println(rows[0][0]);
            println(rows[0][1]);
            println(rows[1][2]);
        }
    "#);
    assert_eq!(out, "0\n42\n99\n");
}

#[test]
fn test_field_index_write_round_trip_plain_and_shared() {
    // `obj.field[i] = val` — the write half of the kata-133-audit
    // FieldAccess-rooted indexing bug (2026-06-06). Pre-fix, the
    // interpreter's set_index hit the catch-all `_ => return` arm for
    // FieldAccess targets and SILENTLY no-op'd the store (the program
    // ran but printed the stale value); codegen errored "Index
    // assignment target must be a variable". Both now route through:
    // the interpreter evals the field access (Value::Array clones the
    // Arc, aliasing the field's storage), codegen goes through
    // `lower_field_access_ptr` + a synth identifier. Plain and shared
    // structs both covered.
    let out = run_no_errors(
        r#"
        struct Holder { tag: i64, mut items: Vec[i64] }
        shared struct Cell { mut vals: Vec[i64] }
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            v.push(41);
            v.push(42);
            let mut h = Holder { tag: 7, items: v };
            h.items[0] = 99;
            println(h.items[0]);
            println(h.items[1]);

            let mut w: Vec[i64] = Vec.new();
            w.push(5);
            let c = Cell { vals: w };
            c.vals[0] = 6;
            println(c.vals[0]);
        }
    "#,
    );
    assert_eq!(out, "99\n42\n6\n");
}

#[test]
fn test_bitwise_ops_post_lowering() {
    // `&`, `|`, `^`, `<<`, `>>` all flow through the lowered Call path.
    assert_eq!(run("fn main() { println(0b1100 & 0b1010); }"), "8\n");
    assert_eq!(run("fn main() { println(0b1100 | 0b1010); }"), "14\n");
    assert_eq!(run("fn main() { println(0b1100 ^ 0b1010); }"), "6\n");
    assert_eq!(run("fn main() { println(1 << 3); }"), "8\n");
    assert_eq!(run("fn main() { println(16 >> 2); }"), "4\n");
}

#[test]
fn test_compound_assignment_int_subtract() {
    assert_eq!(
        run("fn main() { let mut x = 10; x -= 3; println(x); }"),
        "7\n"
    );
}

// ── Integer Overflow ───────────────────────────────────────────

#[test]
fn test_integer_overflow_traps() {
    let errors = runtime_errors("fn main() { let x = 9223372036854775807 + 1; }");
    assert!(
        errors.iter().any(|e| e.message.contains("overflow")),
        "expected an overflow runtime error, got {:?}",
        errors
    );
}

// ── Print ──────────────────────────────────────────────────────

#[test]
fn test_print_no_newline() {
    assert_eq!(run(r#"fn main() { print("a"); print("b"); }"#), "ab");
}

// ── IEEE 754 Float Semantics ──────────────────────────────────

#[test]
fn test_float_nan_not_equal() {
    // IEEE 754: NaN != NaN
    let output = run("fn main() { let nan = 0.0 / 0.0; println(nan == nan); }");
    assert_eq!(output, "false\n");
}

#[test]
fn test_sort_by_key_float_nan_sorts_largest_interp_parity() {
    // B-2026-08-11-17 — the INTERPRETER twin of
    // `test_e2e_vec_sort_by_key_float_nan_sorts_largest` (tests/codegen.rs),
    // same input and same expected order. Only the compiled side was ever
    // asserted, which is why this survived: `value_compare` ordered floats
    // with `partial_cmp(…).unwrap_or(Equal)`, so EVERY comparison involving a
    // NaN answered "equal". That is not merely a misplaced NaN — it makes the
    // order intransitive and the whole sort incoherent. This exact input came
    // back `3.5 NaN -2 1.2 2.7`, not sorted in any order, while both compiled
    // backends gave `-2 1.2 2.7 3.5 NaN`.
    //
    // The typechecker deliberately ALLOWS a float sort KEY (the concession is
    // documented at `check_sort_key_closure`) on the grounds that the backends
    // implement bit-level total order — true of codegen via `karac_float_cmp`,
    // and now true of the interpreter. It is also the last remaining way to
    // get an incoherent float ordering without a diagnostic, since
    // B-2026-08-11-7 gated `sort`/`sorted`/`binary_search` and -15 gated
    // `max`/`min` on `Vec[f64]`.
    let out = run("fn main() {\n\
             let mut v: Vec[f64] = Vec.new();\n\
             let nan: f64 = 0.0 / 0.0;\n\
             v.push(3.5); v.push(nan); v.push(1.2); v.push(0.0 - 2.0); v.push(2.7);\n\
             v.sort_by_key(|x| x);\n\
             for x in v.iter() { println(x); }\n\
         }");
    assert_eq!(out, "-2\n1.2\n2.7\n3.5\nNaN\n", "got {out:?}");
}

#[test]
fn test_sort_by_key_float_nan_order_is_provenance_independent() {
    // B-2026-08-11-17, second half. `total_cmp` alone fixes the incoherence
    // but NOT the run-vs-build split, because it is sign-sensitive: a NEGATIVE
    // NaN sorts before `-Infinity` while a POSITIVE one sorts after
    // `+Infinity`, and nothing in the source chooses which you get — an x86
    // runtime `z / z` yields a negative NaN, LLVM's constant folder a positive
    // one. Mid-fix this was measured NaN-first under interp and JIT but
    // NaN-LAST under AOT, which inlined `zero()` and folded the division.
    //
    // Both NaNs must therefore sort identically. `zero()` defeats constant
    // folding so the two provenances appear in one program.
    let out = run("fn zero() -> f64 { return 0.0; }\n\
         fn main() {\n\
             let z = zero();\n\
             let rt = z / z;\n\
             let ct = 0.0 / 0.0;\n\
             let mut a: Vec[f64] = Vec.new();\n\
             a.push(1.0); a.push(rt); a.push(0.0 - 1.0);\n\
             a.sort_by_key(|x| x);\n\
             let mut b: Vec[f64] = Vec.new();\n\
             b.push(1.0); b.push(ct); b.push(0.0 - 1.0);\n\
             b.sort_by_key(|x| x);\n\
             for x in a.iter() { println(x); }\n\
             for x in b.iter() { println(x); }\n\
         }");
    assert_eq!(out, "-1\n1\nNaN\n-1\n1\nNaN\n", "got {out:?}");
}

#[test]
fn test_float_wrapper_value_from_enum_payload_interp_parity() {
    // B-2026-07-23-2 — `.value` on a float-wrapper bound out of a user-enum
    // payload, in a match arm beside an `f64` arm. The interpreter was already
    // correct here (the bug was codegen's phi-type mismatch bailing to `ret i64
    // 0`); this pins the run==build contract for the codegen E2E fix. Same
    // program + output as `test_e2e_float_wrapper_value_field_from_enum_payload`.
    let output = run("enum Num { I(i64), F(F32) }\n\
             enum Dbl { I(i64), D(F64) }\n\
             fn show(n: Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value } }\n\
             fn show_ref(n: ref Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value } }\n\
             fn show_first(n: Num) -> f64 { match n { F(f) => f.value, I(x) => x as f64 } }\n\
             fn show_arith(n: Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value + 1.0 } }\n\
             fn show_d(n: Dbl) -> f64 { match n { I(x) => x as f64, D(d) => d.value } }\n\
             fn main() {\n\
                 let a = Num.F(F32 { value: 2.5 });\n\
                 println(show(a));\n\
                 println(show(Num.I(7)));\n\
                 let b = Num.F(F32 { value: 4.0 });\n\
                 println(show_ref(b));\n\
                 println(show_first(Num.F(F32 { value: 1.5 })));\n\
                 println(show_arith(Num.F(F32 { value: 2.5 })));\n\
                 println(show_d(Dbl.D(F64 { value: 3.25 })));\n\
                 println(show_d(Dbl.I(9)));\n\
             }");
    assert_eq!(output, "2.5\n7\n4\n1.5\n3.5\n3.25\n9\n");
}

// ── Numeric primitive From (Step 4) ────────────────────────────

#[test]
fn test_int_from_widening() {
    let output = run("fn main() { let x: i32 = 42; let y: i64 = i64.from(x); println(y); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_float_from_widening() {
    let output = run("fn main() { let x: f32 = 1.5; let y: f64 = f64.from(x); println(y); }");
    assert_eq!(output, "1.5\n");
}

// ── Primitive-type associated constants ──────────────────────
//
// Theme 7 (2026-05-10) — `i64.MAX` / `f64.INFINITY` / `usize.MAX` etc.
// dispatch through the shared `PRIMITIVE_CONSTS` table at
// `src/prelude.rs`. The interpreter intercepts the `FieldAccess` arm
// before the bare-primitive identifier would panic; codegen mirrors
// at `compile_field_access`. NaN tests assert the rendered string
// matches `Value::Float`'s Display impl ("NaN").

#[test]
fn test_interp_primitive_const_i64_max() {
    let output = run("fn main() { let x = i64.MAX; println(x); }");
    assert_eq!(output, "9223372036854775807\n");
}

#[test]
fn test_interp_primitive_const_i64_min() {
    let output = run("fn main() { let x = i64.MIN; println(x); }");
    assert_eq!(output, "-9223372036854775808\n");
}

#[test]
fn test_interp_primitive_const_u64_max() {
    // B-2026-07-04-8: the i64-carrier `Value::Int` still holds u64::MAX's bit
    // pattern (all-ones == -1 signed), but the print sink now recovers the
    // `u64` static type from the printed expression's span and renders the bits
    // unsigned — matching codegen's unsigned display instead of the old `-1`.
    let output = run("fn main() { let x = u64.MAX; println(x); }");
    assert_eq!(output, "18446744073709551615\n");
}

#[test]
fn test_interp_primitive_const_usize_max() {
    // usize is 64-bit here, so usize::MAX prints the same unsigned all-ones
    // value as u64::MAX (B-2026-07-04-8).
    let output = run("fn main() { let x = usize.MAX; println(x); }");
    assert_eq!(output, "18446744073709551615\n");
}

#[test]
fn test_interp_primitive_const_isize_max_and_min() {
    // B-2026-08-21-31 — `isize` did not exist as a type at all, though
    // design.md names it a v1 numeric primitive in four normative passages and
    // three of the compiler's own back-end tables (`cheader.rs` -> `ptrdiff_t`,
    // `deque_head.rs`, `wasm_glue.rs`) already mapped it. It is pointer-width
    // SIGNED, so unlike `usize.MAX` it prints the i64 bounds.
    assert_eq!(
        run("fn main() { println(isize.MAX); }"),
        "9223372036854775807\n"
    );
    assert_eq!(
        run("fn main() { println(isize.MIN); }"),
        "-9223372036854775808\n"
    );
}

#[test]
fn test_interp_isize_is_signed_end_to_end() {
    // The whole point of the type: negatives, signed division and signed
    // comparison, none of which `usize` can express. `usize`-style unsigned
    // reinterpretation of the i64 carrier would answer differently for every
    // line here.
    let output = run(
        "fn f(a: isize, b: isize) -> isize { a / b }\n         fn main() {\n             let a: isize = -7isize;\n             println(a);\n             println(f(a, 2isize));\n             println(a < 0isize);\n             println(a.abs());\n         }",
    );
    assert_eq!(output, "-7\n-3\ntrue\n7\n");
}

#[test]
fn test_interp_isize_overflow_traps_at_the_signed_boundary() {
    // Not the unsigned one: `isize.MAX + 1` must trap, where the same bit
    // pattern is an ordinary mid-range value for `usize`.
    let errs = runtime_errors("fn main() { let a: isize = isize.MAX; println(a + 1isize); }");
    assert!(
        errs.iter().any(|e| e.message.contains("integer overflow")),
        "isize must trap at the SIGNED boundary, got: {errs:?}"
    );
}

#[test]
fn test_interp_isize_carries_the_four_overflow_method_families() {
    // design.md:2178 promises `checked_*` / `wrapping_*` / `saturating_*` /
    // `overflowing_*` on EVERY integer primitive. `wrapping_*` gates on an
    // explicit width list, so it is the one that silently omitted `isize`.
    let output = run(
        "fn main() {\n             println(isize.MAX.checked_add(1isize));\n             println(isize.MAX.wrapping_add(1isize));\n             println(isize.MAX.saturating_add(1isize));\n             println(isize.MAX.overflowing_add(1isize));\n         }",
    );
    assert_eq!(
        output,
        "None\n-9223372036854775808\n9223372036854775807\n(-9223372036854775808, true)\n"
    );
}

// ── u64 model (B-2026-07-04-8) ──────────────────────────────────────
//
// The tree-walk interpreter stores every integer width in the i64-carrier
// `Value::Int`, so a `u64` / `usize` value ≥ 2⁶³ rides as a negative
// two's-complement i64. These tests pin the span-threaded unsigned-64 model
// that reinterprets the bits as `u64` at the operators that differ (compare /
// div / rem / shr / the add overflow boundary), at the print sinks, and at the
// sort / argsort / argmin / argmax paths — so `karac run` matches codegen's
// unsigned lowering. Values ≥ 2⁶³ are shift-constructed (the lexer rejects
// integer literals > i64::MAX).

#[test]
fn test_interp_u64_print_ge_2_63() {
    // The core mis-print: 2⁶³ rode as i64::MIN and printed with a spurious
    // minus sign. Now rendered unsigned from the printed expr's u64 span.
    let output = run("fn main() { let hi: u64 = 1u64 << 63; println(f\"{hi}\"); }");
    assert_eq!(output, "9223372036854775808\n");
}

#[test]
fn test_interp_u64_comparison_is_unsigned() {
    // hi = 2⁶³ (i64::MIN signed), mid = 2⁶². Signed `>` said false; unsigned
    // says true. Operand signedness is recovered from the operand span since
    // the comparison result types the expression as `bool`.
    let output = run(
        "fn main() { let hi: u64 = 1u64 << 63; let mid: u64 = 1u64 << 62; \
         println(f\"{hi > mid}\"); }",
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_interp_u64_div_rem_shr_unsigned() {
    // Signed sdiv/srem/ashr would smear the high bit; unsigned udiv/urem/lshr.
    let output = run("fn main() { let hi: u64 = 1u64 << 63; \
         println(f\"{hi / 2u64}\"); println(f\"{hi % 7u64}\"); \
         println(f\"{hi >> 1u64}\"); }");
    // 2⁶³/2 = 2⁶², 2⁶³ % 7 = 1, 2⁶³ >> 1 (logical) = 2⁶².
    assert_eq!(output, "4611686018427387904\n1\n4611686018427387904\n");
}

#[test]
fn test_interp_u64_add_no_false_overflow_trap() {
    // 2⁶³ + 5 overflows i64 (would false-trap under the signed `checked_add`
    // arm) but fits u64 — codegen uses `uadd.with.overflow`, so the interpreter
    // must not trap here either.
    let output = run("fn main() { let big: u64 = (1u64 << 63) + 5u64; println(f\"{big}\"); }");
    assert_eq!(output, "9223372036854775813\n");
}

#[test]
fn test_interp_u64_compound_assign_unsigned() {
    // `>>=` (logical), `/=` (unsigned), `%=` (unsigned) on a u64 target —
    // signedness threaded from the assignment target's span.
    let output = run("fn main() { \
         let mut a: u64 = 1u64 << 63; a >>= 1u64; println(f\"{a}\"); \
         let mut b: u64 = 1u64 << 63; b /= 4u64; println(f\"{b}\"); \
         let mut c: u64 = 1u64 << 63; c %= 7u64; println(f\"{c}\"); }");
    assert_eq!(output, "4611686018427387904\n2305843009213693952\n1\n");
}

#[test]
fn test_interp_vec_u64_sort_is_unsigned() {
    // A value ≥ 2⁶³ must sort after the positives, not first as a negative i64.
    let output = run(
        "fn main() { let mut xs: Vec[u64] = [1u64 << 63, 5u64, 1u64 << 62, 0u64]; \
         xs.sort(); \
         println(f\"{xs[0]}\"); println(f\"{xs[1]}\"); \
         println(f\"{xs[2]}\"); println(f\"{xs[3]}\"); }",
    );
    assert_eq!(output, "0\n5\n4611686018427387904\n9223372036854775808\n");
}

#[test]
fn test_interp_vec_u64_sorted_returns_unsigned_order() {
    let output = run(
        "fn main() { let xs: Vec[u64] = [1u64 << 63, 5u64, 1u64 << 62]; \
         let s = xs.sorted(); \
         println(f\"{s[0]}\"); println(f\"{s[1]}\"); println(f\"{s[2]}\"); }",
    );
    assert_eq!(output, "5\n4611686018427387904\n9223372036854775808\n");
}

#[test]
fn test_env_set_round_trips_via_var() {
    // env.set("X", "v") then env.var("X") observes "v". Confirms the new
    // stdlib method writes to the same process-environment block that
    // `env.var` reads from. Use a unique name to avoid collisions with
    // other tests running in the same process.
    std::env::remove_var("KARAC_ENV_SET_ROUND_TRIP_TEST");
    let output = run("fn main() {
         env.set(\"KARAC_ENV_SET_ROUND_TRIP_TEST\", \"hello-set\");
         match env.var(\"KARAC_ENV_SET_ROUND_TRIP_TEST\") {
             Ok(v) => println(v),
             Err(_) => println(\"unset\"),
         }
     }");
    std::env::remove_var("KARAC_ENV_SET_ROUND_TRIP_TEST");
    assert_eq!(output, "hello-set\n");
}

#[test]
fn test_ambient_random_source_next_u64_advances_state() {
    // Two consecutive draws from the default `RandomSource` must return
    // different values — the xorshift state advances on every call. We
    // can't assert any specific number (seeded from wall-clock nanoseconds)
    // but inequality is a sharp witness that state advanced.
    let output = run("fn main() {\n\
                          let a = RandomSource.next_u64();\n\
                          let b = RandomSource.next_u64();\n\
                          println(a != b);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_stdout_print_resource_method_writes_without_newline() {
    // `Stdout.print(s)` does NOT append a newline — the test asserts
    // two `print` calls concatenate cleanly, then a trailing `println`
    // closes the line so the captured buffer ends with `\n`.
    let output = run("fn main() {\n\
                          Stdout.print(\"a\");\n\
                          Stdout.print(\"b\");\n\
                          Stdout.println(\"c\");\n\
                      }");
    assert_eq!(output, "abc\n");
}

// ── Standard I/O interpreter tests ──────────────────────────────────────────

#[test]
fn test_filesystem_write_and_read_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_fs_roundtrip.txt");
    // Escape backslashes so Windows paths (C:\Users\...) are valid inside a Kāra string literal.
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    // Write via the host OS directly, then read back through the interpreter.
    std::fs::write(&tmp, "hello kara").expect("temp write");
    let src = format!(
        "fn main() {{
             let r = FileSystem.read_to_string(\"{path}\");
             match r {{
                 Ok(contents) => println(contents),
                 Err(_) => println(\"read error\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "hello kara\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Phase 8 File handle slice F1 — interpreter MVP ─────────────────
//
// `File.open` / `.create` / `.append` return `Result[File, IoError]`;
// `file.read` / `.write` / `.flush` operate on the live handle. Drop
// on the last Arc clone closes the OS fd. Tests cover the round-trip
// (create + write + flush + reopen + read), the error path (open
// nonexistent → IoError.NotFound), and the appendconstructor.

#[test]
fn test_file_create_write_flush_reopen_read_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_file_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let data = [104u8, 105u8, 10u8];
                     match f.write(data[0..3]) {{
                         Ok(n) => println(\"wrote \" + n.to_string()),
                         Err(_) => println(\"write err\"),
                     }}
                     match f.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"flush err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let buf = [0u8, 0u8, 0u8, 0u8];
                     match f.read(buf[0..4]) {{
                         Ok(n) => println(\"read \" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote 3\nflushed\nread 3\n");
    // Confirm the file has the expected contents (write actually
    // persisted, not just that the interpreter said it did).
    let written = std::fs::read(&tmp).expect("temp read");
    assert_eq!(written, b"hi\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Phase 8 BufReader[R] — interpreter MVP ────────────────────────
//
// BufReader.new / .with_capacity wrap a `File` (via a dup of its fd)
// with a buffered reader; read_line / read_to_string append into a
// `mut ref String` and return the byte count (0 from read_line = EOF);
// read fills a `mut Slice[u8]`. Tests cover the line-then-rest
// round-trip, read_to_string slurping, the Slice read, with_capacity,
// and the EOF count.

#[test]
fn test_bufreader_read_line_then_read_to_string_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_bufreader_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"hi\nyo\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut line = String.new();
                     match br.read_line(line) {{
                         Ok(n) => println(\"line n=\" + n.to_string() + \" [\" + line + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                     let mut rest = String.new();
                     match br.read_to_string(rest) {{
                         Ok(n) => println(\"rest n=\" + n.to_string() + \" [\" + rest + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "line n=3 [hi\n]\nrest n=3 [yo\n]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_fill_buf_peek_consume_read_roundtrip() {
    // fill_buf peeks the buffered bytes without consuming; consume(5) advances
    // past "HELLO"; a subsequent read then returns the remaining "WORLD".
    let tmp = std::env::temp_dir().join("karac_test_bufreader_peek.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"HELLOWORLD").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     match br.fill_buf() {{
                         Ok(buf) => println(\"peek len=\" + buf.len().to_string()
                             + \" b0=\" + buf[0].to_string()),
                         Err(_) => println(\"fill err\"),
                     }}
                     br.consume(5);
                     let rest = [0u8, 0u8, 0u8, 0u8, 0u8];
                     match br.read(rest[0..5]) {{
                         Ok(n) => println(\"read n=\" + n.to_string()
                             + \" first=\" + rest[0].to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    // 'H' == 72 (peeked, not consumed), 'W' == 87 (first byte after consume(5)).
    assert_eq!(out, "peek len=10 b0=72\nread n=5 first=87\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Phase 8 BufWriter[W] — interpreter MVP ────────────────────────
//
// BufWriter.new / .with_capacity wrap a `File` (via a dup of its fd)
// with a buffered writer; `write` accepts a `Slice[u8]` and returns the
// byte count buffered; `flush` drains the buffer to the underlying fd.
// Tests cover the write-flush-reopen-read round-trip, with_capacity, the
// drop-flush (no explicit flush) path, and an empty write.

#[test]
fn test_bufwriter_write_flush_reopen_read_roundtrip() {
    // Write "hi\n" through a BufWriter, flush, then read the file back via
    // FileSystem.read_to_string to prove the bytes reached disk.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [104u8, 105u8, 10u8];
                     match bw.write(data[0..3]) {{
                         Ok(n) => println(\"wrote \" + n.to_string()),
                         Err(_) => println(\"write err\"),
                     }}
                     match bw.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"flush err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote 3\nflushed\ncontents=[hi\n]\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Distinct types — constructor + .raw() (zero-cost) ──────────────

#[test]
fn test_distinct_constructor_and_raw_roundtrip() {
    // `UserId(42)` wraps a base value (zero-cost) and `.raw()` unwraps it.
    let output = run_no_errors(
        "distinct type UserId = i64;\n\
         fn main() {\n\
             let u = UserId(42);\n\
             let raw: i64 = u.raw();\n\
             println(raw);\n\
         }",
    );
    assert_eq!(output, "42\n");
}

#[test]
fn test_distinct_constructor_float_base() {
    // The wrap is value-preserving for non-integer bases too.
    let output = run_no_errors(
        "distinct type Meters = f64;\n\
         fn main() {\n\
             let m = Meters(3.5);\n\
             println(m.raw());\n\
         }",
    );
    assert_eq!(output, "3.5\n");
}

#[test]
fn test_map_index_read_and_write_string_and_int_keys() {
    // B-2026-07-16-13: the `m[k]` index operator now works in the interpreter
    // for any key type (read panics if missing; write inserts / overwrites),
    // matching codegen's native path. Pre-fix a non-integer key was rejected
    // at typecheck, and even an integer-keyed `m[1]` `unreachable!`'d the
    // interpreter (no Map-index arm). Covers String keys, int keys, a SortedMap
    // receiver, and overwrite-on-existing.
    let output = run("fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             m[\"alice\"] = 1_i64;\n\
             m[\"bob\"] = 2_i64;\n\
             m[\"alice\"] = 100_i64;\n\
             println(m[\"alice\"]);\n\
             println(m[\"bob\"]);\n\
             let mut mi: Map[i64, i64] = Map.new();\n\
             mi[5_i64] = 50_i64;\n\
             println(mi[5_i64]);\n\
             let mut sm: SortedMap[String, i64] = SortedMap.new();\n\
             sm[\"k\"] = 7_i64;\n\
             sm[\"k\"] = 8_i64;\n\
             println(sm[\"k\"]);\n\
         }");
    assert_eq!(output, "100\n2\n50\n8\n");
}

#[test]
fn test_map_prefix_literal_int_keys() {
    let output = run("fn main() {\n\
             let m = Map[1_i64: 100_i64, 2_i64: 200_i64];\n\
             println(m.len());\n\
             match m.get(2_i64) {\n\
                 Some(v) => println(v),\n\
                 None => println(0_i64),\n\
             }\n\
         }");
    assert_eq!(output, "2\n200\n");
}

#[cfg(unix)]
#[test]
fn test_process_write_stdin_close_and_read_stdout_roundtrip() {
    // Full parent-drives-child round-trip through `/bin/cat` (echoes
    // stdin to stdout): spawn with BOTH stdin and stdout piped, write a
    // line to the child's stdin, then `close()` it. The close is the
    // load-bearing step — `cat` reads to EOF, so without closing stdin
    // it would block forever and the subsequent `read_to_string` would
    // deadlock (the exact footgun the read side guards). After close,
    // `cat` flushes "ping\n" and exits; the captured stdout reads it.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/cat").stdin(Stdio.Piped).stdout(Stdio.Piped);
         match cmd.spawn() {
             Ok(child) => {
                 match child.stdin() {
                     Some(inp) => {
                         match inp.write("ping\n") {
                             Ok(_) => {}
                             Err(_) => println("write_err"),
                         }
                         match inp.close() {
                             Ok(_) => {}
                             Err(_) => println("close_err"),
                         }
                     }
                     None => println("no_stdin"),
                 }
                 match child.stdout() {
                     Some(out) => {
                         match out.read_to_string() {
                             Ok(s) => print(s),
                             Err(_) => println("read_err"),
                         }
                     }
                     None => println("no_stdout"),
                 }
                 match child.wait() {
                     Ok(_) => {}
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "ping\n");
}

#[test]
fn test_arena_push_get_roundtrip() {
    // Bump-allocate three values; each returned `ArenaRef` resolves back
    // to its stored value via `get`. `get` returns `ref T`, which
    // Displays through `println` directly.
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             let r0 = a.push(10);
             let r1 = a.push(20);
             let r2 = a.push(30);
             println(a.get(r0));
             println(a.get(r1));
             println(a.get(r2));
         }"#);
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_arena_rewind_keeps_pre_checkpoint_items() {
    // A handle minted before the checkpoint stays valid after a rewind —
    // only items pushed *past* the mark are dropped.
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             let r0 = a.push(100);
             let cp = a.high_water_mark();
             let _r1 = a.push(200);
             a.rewind_to(cp);
             println(a.get(r0));
             println(a.len());
         }"#);
    assert_eq!(output, "100\n1\n");
}

#[test]
fn test_arena_rewind_with_foreign_checkpoint_is_ignored() {
    // A checkpoint minted by a *different* arena must not truncate this
    // one — the handle-id guard rejects the cross-arena rewind, so the
    // length is unchanged.
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             let b: Arena[i64] = Arena.new();
             let _ra = a.push(1);
             let _rb0 = b.push(10);
             let _rb1 = b.push(20);
             let foreign = a.high_water_mark();
             b.rewind_to(foreign);
             println(b.len());
         }"#);
    assert_eq!(output, "2\n");
}

#[test]
fn test_interner_resolve_roundtrip() {
    // `resolve` hands back the interned string for a handle. `resolve`
    // returns `ref String`, which Displays through `println` directly.
    let output = run(r#"fn main() {
             let mut tab: Interner = Interner.new();
             let a = tab.intern("alpha");
             let b = tab.intern("beta");
             println(tab.resolve(a));
             println(tab.resolve(b));
         }"#);
    assert_eq!(output, "alpha\nbeta\n");
}

#[test]
fn test_base64_decode_roundtrip() {
    let output = run("fn main() {\n\
             match Base64.decode(\"Zm9vYmFy\") {\n\
                 Ok(bs) => println(bs.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_url_decode_roundtrip() {
    let output = run("fn main() {\n\
             match Url.decode(\"a%20b%2Fc\") {\n\
                 Ok(s) => println(s),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "a b/c\n");
}

// ── Display / to_string ───────────────────────────────────────────

#[test]
fn test_to_string_i64() {
    let output = run("fn main() { let n: i64 = 42; println(n.to_string()); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_fstring_interpolates_i64() {
    let output = run(r#"fn main() { let n: i64 = 7; println(f"n is {n}"); }"#);
    assert_eq!(output, "n is 7\n");
}

#[test]
fn test_println_i64_direct() {
    let output = run("fn main() { println(123_i64); }");
    assert_eq!(output, "123\n");
}

#[test]
fn test_println_float_direct() {
    let output = run("fn main() { println(3.14_f64); }");
    assert_eq!(output, "3.14\n");
}

#[test]
fn test_u8_ascii_predicates_interpreter() {
    // ASCII byte-classification on the `u8` bytes from `String.bytes()`:
    // is_ascii_digit / is_ascii_alphabetic / is_ascii_hexdigit. Phase-8 floor
    // for the self-hosting lexer's byte-indexed scan.
    let output = run(r#"fn main() {
            let s: String = "aZ9_ f";
            for b in s.bytes() {
                println(f"{b.is_ascii_digit()} {b.is_ascii_alphabetic()} {b.is_ascii_hexdigit()}");
            }
        }"#);
    // a:_/alpha/hex  Z:_/alpha/_  9:digit/_/hex  _:none  space:none  f:_/alpha/hex
    assert_eq!(
        output,
        "false true true\n\
         false true false\n\
         true false true\n\
         false false false\n\
         false false false\n\
         false true true\n"
    );
}

#[test]
fn test_i64_parse_interpreter() {
    // Five cases mirror `/tmp/kara-probes/i64_parse_full_probe.kara`:
    // numeric / non-numeric / negative / whitespace-padded / empty.
    let output = run(r#"fn main() {
            match i64.parse("42") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("not a number") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("-7") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("  100  ") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("") {
                Some(n) => println(n),
                None => println(-1),
            }
        }"#);
    assert_eq!(output, "42\n-1\n-7\n100\n-1\n");
}

#[test]
fn test_i64_from_str_radix_interpreter() {
    // Radix parse for the self-hosting lexer's hex/binary/octal literals:
    // hex / binary / octal / reject-bad-digit / hex-positive. Invalid radix
    // (>36) and bad digits → None.
    let output = run(
        r#"fn pr(o: Option[i64]) { match o { Some(n) => println(n), None => println(-1), } }
        fn main() {
            pr(i64.from_str_radix("ff", 16));
            pr(i64.from_str_radix("1010", 2));
            pr(i64.from_str_radix("17", 8));
            pr(i64.from_str_radix("zz", 16));
            pr(i64.from_str_radix("7f", 16));
        }"#,
    );
    assert_eq!(output, "255\n10\n15\n-1\n127\n");
}

#[test]
fn test_numeric_try_from_interpreter() {
    // Built-in numeric narrowing `T.try_from(x) -> Result[T, String]`
    // (design.md § Conversion Traits). Covers: in-range Ok, narrowing
    // out-of-range Err, sign-change (negative → unsigned) Err, widening always
    // Ok, an unsigned target printing a value that overflows the signed target,
    // and the `.try_into()` desugar. Must match the codegen E2E
    // (`test_e2e_numeric_try_from`).
    let output = run(r#"fn main() {
            match i8.try_from(100) { Ok(v) => println(v), Err(e) => println(e) }
            match i8.try_from(300) { Ok(v) => println(v), Err(e) => println(e) }
            match u8.try_from(-1) { Ok(v) => println(v), Err(e) => println(e) }
            match i64.try_from(42) { Ok(v) => println(v), Err(e) => println(e) }
            let big: i64 = 3000000000;
            match u32.try_from(big) { Ok(v) => println(v), Err(e) => println(e) }
            match i32.try_from(big) { Ok(v) => println(v), Err(e) => println(e) }
            let n: i32 = 70000;
            let r: Result[i16, String] = n.try_into();
            match r { Ok(v) => println(v), Err(e) => println(e) }
        }"#);
    assert_eq!(
        output,
        "100\nout of range for i8\nout of range for u8\n42\n3000000000\nout of range for i32\nout of range for i16\n"
    );
}

// ── String/VecDeque constructor family (typechecker special-arm paths) ──
//
// These paths have no syntactic stdlib declaration — the typechecker
// special-cases them (typechecker/expr_call.rs) and codegen claims them
// directly, so each needs an explicit interpreter arm in
// eval_call.rs. Surfaced by the 2026-06-05 kata-corpus audit: every one
// of these previously died at the eval_expr unwired-path panic under
// `karac run` while building fine under `karac build`.

#[test]
fn test_string_new_push_roundtrip() {
    let output = run_no_errors(
        r#"fn main() {
            let mut s = String.new();
            s.push_str("ab");
            s.push('c');
            println(s);
        }"#,
    );
    assert_eq!(output, "abc\n");
}

#[test]
fn test_i64_cmp_method() {
    // `cmp` returns `Ordering` — pin the variant-name round trip.
    let output = run("fn main() {
            let a = 3_i64;
            let b = 5_i64;
            match a.cmp(b) {
                Ordering.Less => println(\"less\"),
                Ordering.Equal => println(\"equal\"),
                Ordering.Greater => println(\"greater\"),
            }
        }");
    assert_eq!(output, "less\n");
}

// `c as i32` used to be a no-op in the interpreter (codegen lowered it
// correctly via LLVM `int_cast`). The downstream subtraction then panicked
// in `eval_ops` with "type mismatch in binary operation Sub". The fix
// mirrors `check_cast_pair`'s accepted shapes (char → wide-int).
// Surfaced while writing kata #8 (string-to-integer atoi).
#[test]
fn test_interp_char_as_i32_digit_subtraction() {
    let output = run(r#"
fn main() {
    let s = "0123";
    for c in s.chars() {
        let n: i32 = c as i32;
        let d: i32 = n - 48i32;
        println(d);
    }
}
"#);
    assert_eq!(output, "0\n1\n2\n3\n");
}

#[test]
fn test_interp_int_to_float_cast() {
    let output = run(r#"
fn main() {
    let n: i64 = 7;
    let f: f64 = n as f64;
    println(f);
}
"#);
    assert_eq!(output.trim(), "7");
}

// ── Portable SIMD `Vector[T, N]` — slice 1b interpreter parity ────────
//
// design.md § Portable SIMD + "Interpreter parity scope": the tree-walk
// interpreter and codegen must produce equivalent observable output for the
// same program. These mirror the `tests/codegen.rs::test_vector_*` run-tests
// (same sources, same expected stdout) so the two backends are pinned to the
// same behaviour for construction, element-wise arithmetic, and lane read.

#[test]
fn test_vector_simd_math_transcendentals() {
    // std.simd.math (phase-11): element-wise sqrt/exp/ln/sigmoid/tanh on a
    // float vector, computed per lane. Exact/saturating oracles matching
    // tests/codegen.rs::test_e2e_vector_simd_math_transcendentals.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([4.0f32, 9.0f32, 16.0f32, 25.0f32]);
    let s = v.sqrt();
    println(s[0]);
    println(s[3]);
    let z = Vector[f32, 4].splat(0.0f32);
    let ex = z.exp();
    println(ex[0]);
    let sg = z.sigmoid();
    println(sg[0]);
    let th = z.tanh();
    println(th[0]);
    let o = Vector[f32, 4].splat(1.0f32);
    let l = o.ln();
    println(l[0]);
}
"#,
    );
    assert_eq!(out, "2\n5\n1\n0.5\n0\n0\n");
}

#[test]
fn test_vector_simd_math_rounding() {
    // std.simd.math (phase-11): element-wise floor/ceil/round/trunc on a float
    // vector, per lane. `round` is half-away-from-zero. Oracle matches
    // tests/codegen.rs::test_e2e_vector_simd_math_rounding. Lanes [2.5, -2.5]
    // pin the distinct rounding directions: floor→[2,-3], ceil→[3,-2],
    // round→[3,-3] (ties away from zero), trunc→[2,-2].
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([2.5f32, -2.5f32, 2.7f32, -2.3f32]);
    let fl = v.floor();
    println(fl[0]); println(fl[1]);
    let ce = v.ceil();
    println(ce[0]); println(ce[1]);
    let ro = v.round();
    println(ro[0]); println(ro[1]);
    let tr = v.trunc();
    println(tr[0]); println(tr[1]);
}
"#,
    );
    assert_eq!(out, "2\n-3\n3\n-2\n3\n-3\n2\n-2\n");
}

#[test]
fn test_vector_simd_math_bits_roundtrip() {
    // std.simd.math (phase-11): element-wise IEEE-754 bitcast between a float
    // vector and a same-width integer vector. Known patterns: 1.0f32 =
    // 0x3F800000 = 1065353216; 1.0f64 = 0x3FF0000000000000 =
    // 4607182418800017408. to_bits -> bits_as_f* round-trips recover the
    // original value. Matches tests/codegen.rs::test_e2e_vector_simd_math_bits.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([1.0f32, 2.0f32, 0.0f32, -1.0f32]);
    let b = v.to_bits();
    println(b[0]);
    let r = b.bits_as_f32();
    println(r[3]);
    let w = Vector[f64, 2].from_array([1.0, 2.0]);
    let wb = w.to_bits();
    println(wb[0]);
    let wr = wb.bits_as_f64();
    println(wr[1]);
}
"#,
    );
    assert_eq!(out, "1065353216\n-1\n4607182418800017408\n2\n");
}

// B-2026-08-06-7 — an out-of-range shift AMOUNT is a structured diagnostic,
// not a process panic. Reaching the assertions at all proves the no-panic
// half: before the fix this exact program aborted the interpreter with
// "attempt to shift left with overflow" from Rust's own shift operator, so
// the test binary would have died before it could inspect `errors`.
//
// Both directions and both a narrow and a 64-bit width, because the amount
// is checked against the DECLARED width — 32 for `i32`, 64 for `i64` — and a
// single hard-coded 64 would silently let every narrow over-shift through.
#[test]
fn test_shift_amount_out_of_range_is_runtime_error_not_panic() {
    for (src, what) in [
        (
            "let a: i32 = 1i32; let s: i32 = 32i32; println(a << s);",
            "i32 <<",
        ),
        (
            "let a: i32 = 1i32; let s: i32 = 32i32; println(a >> s);",
            "i32 >>",
        ),
        (
            "let a: i64 = 1i64; let s: i64 = 64i64; println(a << s);",
            "i64 <<",
        ),
        (
            "let a: i64 = 1i64; let s: i64 = -1i64; println(a << s);",
            "i64 << negative",
        ),
    ] {
        let errors = runtime_errors(&format!("fn main() {{\n    {src}\n}}\n"));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("shift amount out of range")),
            "{what}: expected a `shift amount out of range` runtime error, got: {errors:?}"
        );
    }
}

// B-2026-08-06-13: the i64::MIN literal evaluates to i64::MIN — the value, not
// a trap. It reaches the interpreter as a plain already-negative `Integer` node
// (the parser folds it), so the Neg arm's range check never sees it; the
// arithmetic identity is the check that the fold produced the RIGHT value
// rather than merely a parseable one.
#[test]
fn test_i64_min_literal_evaluates_to_i64_min() {
    let out = run_no_errors(
        "fn main() {\n\
         \x20   let a: i64 = -9223372036854775808i64;\n\
         \x20   println(a);\n\
         \x20   println(a == -9223372036854775807i64 - 1i64);\n\
         \x20   println(a + 1i64);\n\
         }\n",
    );
    assert_eq!(
        out.trim(),
        "-9223372036854775808\ntrue\n-9223372036854775807"
    );
}

// A shift amount BELOW the width stays legal at every width — the guard must
// not over-trap. `31` on an i32 and `63` on an i64 are the boundary values the
// spec explicitly calls legal.
#[test]
fn test_shift_amount_at_width_minus_one_is_legal() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let a: i32 = 1i32; let s: i32 = 31i32; println(a << s);\n\
             \x20   let b: i64 = 1i64; let t: i64 = 63i64; println(b << t);\n\
             }\n"),
        "-2147483648\n-9223372036854775808\n"
    );
}

// B-2026-08-06-7 — the interpreter half of the shift rules (design.md
// § 2141-2142). Kept in step with the codegen twin
// `e2e_shift_runs_at_declared_width` (same arms, same expected output).
//
// Before the fix, both arms of the interpreter's shift dispatch were a bare
// Rust `a << b` / `a >> b` on the i64 carrier. That gave the same
// out-of-declared-width results codegen gave — `1i32 << 31` was 2147483648 —
// and, for an amount >= 64, PANICKED THE INTERPRETER PROCESS outright
// ("attempt to shift left with overflow" from eval_ops.rs), which is both a
// crash and a violation of the repo standard that every phase emits a
// structured diagnostic rather than panicking.
#[test]
fn test_shift_runs_at_declared_width() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let a: i32 = 1i32;\n\
             \x20   println(a << 31i32);\n\
             \x20   let b: i32 = 1000000i32;\n\
             \x20   let s: i32 = b << 20i32;\n\
             \x20   println(s);\n\
             \x20   println(s > 2147483647i32);\n\
             \x20   let u: u8 = 200u8;\n\
             \x20   println(u << 4u8);\n\
             \x20   let n: i32 = -8i32;\n\
             \x20   println(n >> 1i32);\n\
             }\n"),
        "-2147483648\n603979776\nfalse\n128\n-4\n"
    );
}

// The interpreter half of "`~` complements at the DECLARED width", kept in
// step with the codegen twin `e2e_bitwise_not_runs_at_declared_width` (same
// arms, same expected output).
//
// The interpreter stores every integer in an i128 `Value::Int`, so `!i` on
// the carrier is only the declared answer when the declared type IS the
// carrier width. `~5u8` is 250; the carrier flip gave -6 — not merely wrong
// but unrepresentable in a `u8` — and codegen's i64 twin gave 2^64-6, so the
// backends disagreed with each other as well as with the spec.
//
// Unlike codegen, the interpreter was wrong for BOTH literal spellings
// (suffixed and not), because the carrier has no notion of the local's
// storage width at all. Both are kept so the two backends' tests stay
// arm-for-arm comparable.
#[test]
fn test_bitwise_not_runs_at_declared_width() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let a: u8 = 5;\n\
             \x20   println(~a);\n\
             \x20   let b: u16 = 5;\n\
             \x20   println(~b);\n\
             \x20   let c: u32 = 5;\n\
             \x20   println(~c);\n\
             \x20   let d: u8 = 5;\n\
             \x20   let rd: u8 = ~d;\n\
             \x20   println(rd);\n\
             \x20   let e: u8 = 5u8;\n\
             \x20   println(~e);\n\
             \x20   let f: u8 = 5u8;\n\
             \x20   let rf: u8 = ~f;\n\
             \x20   println(rf);\n\
             \x20   let g: i8 = 5;\n\
             \x20   println(~g);\n\
             \x20   let h: i32 = 5;\n\
             \x20   println(~h);\n\
             \x20   let i: u64 = 5;\n\
             \x20   println(~i);\n\
             \x20   let z: u8 = 0;\n\
             \x20   println(~z);\n\
             \x20   let m: u8 = 255;\n\
             \x20   println(~m);\n\
             \x20   let lo: i8 = -128;\n\
             \x20   println(~lo);\n\
             \x20   let n: u8 = 250;\n\
             \x20   println(~n == 5);\n\
             }\n"),
        "250\n65530\n4294967290\n250\n250\n250\n\
         -6\n-6\n18446744073709551610\n255\n0\n127\ntrue\n"
    );
}

// `~` on an integer-lane `Vector[T, N]` complements EACH LANE at the lane
// width. The interpreter recurses into lanes carrying the CONTAINER's span,
// so the width lookup has to peel `Vector` to reach the lane type — without
// that peel it fell back to a signed 64 and `~Vector[u8, 4](5,…)` gave -6 per
// lane while codegen, whose lanes are a real `<4 x i8>`, gave 250.
#[test]
fn test_bitwise_not_on_vector_lanes_uses_lane_width() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let a: u8 = 5;\n\
             \x20   let v: Vector[u8, 4] = Vector[u8, 4](a, a, a, a);\n\
             \x20   let w: Vector[u8, 4] = ~v;\n\
             \x20   println(w[0]);\n\
             \x20   println(w[3]);\n\
             }\n"),
        "250\n250\n"
    );
}

// A `Vector[T, N]` unary math lane must be ROUNDED BACK to the element width,
// exactly as the scalar `x.sqrt()` / `x.exp()` path is. The tree-walk computes
// every lane in f64, so before B-2026-08-29-40 a narrow-lane vector kept bits
// its own element type cannot hold: `Vector[bf16, 4].sqrt()` returned the f64
// 1.4142135623730951 for a lane of 2, where all three compiled backends return
// the bf16 1.4140625 — and the interpreter disagreed with ITSELF, since its own
// scalar `two.sqrt()` already rounded.
//
// The row that filed this said only `sqrt` diverged and that the other eight
// methods "agree by luck of the inputs". Measured: `exp`, `ln`, `tanh` and
// `sigmoid` diverge too, on inputs as ordinary as 1..7 — only the four rounding
// methods (`floor`/`ceil`/`round`/`trunc`) agreed, and those agree
// STRUCTURALLY, because an integral result already sits on the narrow grid. So
// all five non-integral methods are pinned here, not just `sqrt`.
//
// The f32 and f64 lines are the two boundaries: f32 must round (to the f32
// sqrt(2)), and f64 must NOT — it is the identity, and a fix that over-rounded
// would show up as an f64 lane losing digits. Every value below is the output
// of `karac build`, byte-identical under `karac run` and `KARAC_AUTO_PAR=0`.
#[test]
fn test_vector_unary_math_rounds_lanes_to_element_width() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let v: Vector[bf16, 4] = Vector[bf16, 4](1.0bf16, 2.0bf16, 3.0bf16, 7.0bf16);\n\
             \x20   println(v.sqrt()[1]);\n\
             \x20   println(v.sqrt().reduce_sum());\n\
             \x20   println(v.exp()[1]);\n\
             \x20   println(v.ln()[2]);\n\
             \x20   println(v.tanh()[0]);\n\
             \x20   println(v.sigmoid()[1]);\n\
             \x20   let f: Vector[f32, 4] = Vector[f32, 4](2.0f32, 3.0f32, 5.0f32, 7.0f32);\n\
             \x20   println(f.sqrt()[0]);\n\
             \x20   let d: Vector[f64, 2] = Vector[f64, 2](2.0, 3.0);\n\
             \x20   println(d.sqrt()[0]);\n\
             }\n"),
        "1.4140625\n\
         6.75\n\
         7.375\n\
         1.1015625\n\
         0.76171875\n\
         0.87890625\n\
         1.4142135381698608\n\
         1.4142135623730951\n"
    );
}

// The ORACLE half of B-2026-08-29-53, whose compiled twin is
// `tests/codegen.rs::test_e2e_vector_f16_transcendentals_match_the_scalar_and_the_interpreter`.
//
// The interpreter was RIGHT here and this test passes without that fix — its
// job is to freeze the answers the compiled half is measured against, so the
// two cannot drift apart the way the bf16 pair did. Codegen's
// `apply_vector_float_unary` widened bf16 lanes to f32 for the whole body but
// left f16 narrow, so `tanh`'s `e^2ˣ` intermediate overflowed f16's 65504
// ceiling above x ≈ 5.545 and the quotient came out `inf/inf` = NaN; at x = 4,
// under the threshold, both `e^2ˣ ± 1` rounded to the same f16 and the quotient
// was exactly 1 instead of 0.99951171875. The interpreter computes each lane in
// f64 and rounds once to the element width (B-2026-08-29-40), which is why it
// never had either failure.
//
// The seed is the literal 1 rather than `env.args().len()`: the codegen fixture
// needs an opaque seed to survive constant folding and 1 is what that yields
// under its harness, while `env.args()` in an in-process interpreter test would
// report the TEST binary's argv. Values below are otherwise character-identical
// to the compiled twin.
#[test]
fn test_vector_f16_transcendentals_are_computed_without_overflowing_the_lane() {
    assert_eq!(
        run("fn main() {\n\
             \x20   let n: i64 = 1;\n\
             \x20   let a: f16 = ((n + 3) as f32) as f16;\n\
             \x20   let b: f16 = ((n + 5) as f32) as f16;\n\
             \x20   let c: f16 = ((n + 6) as f32) as f16;\n\
             \x20   let d: f16 = ((n + 19) as f32) as f16;\n\
             \x20   let v: Vector[f16, 4] = Vector[f16, 4](a, b, c, d);\n\
             \x20   println(f\"vt {v.tanh()[0]} {v.tanh()[1]} {v.tanh()[2]} {v.tanh()[3]}\");\n\
             \x20   println(f\"st {a.tanh()} {b.tanh()} {c.tanh()} {d.tanh()}\");\n\
             \x20   let e: f16 = ((n + 1) as f32) as f16;\n\
             \x20   let s: Vector[f16, 4] = Vector[f16, 4](e, b, c, d);\n\
             \x20   println(f\"sg {s.sigmoid()[0]} {s.sigmoid()[1]}\");\n\
             \x20   println(f\"ve {v.exp()[0]} {v.ln()[1]} {v.sqrt()[2]}\");\n\
             \x20   println(f\"se {a.exp()} {b.ln()} {c.sqrt()}\");\n\
             }\n"),
        "vt 0.99951171875 1 1 1\n\
         st 0.99951171875 1 1 1\n\
         sg 0.880859375 0.99755859375\n\
         ve 54.59375 1.7919921875 2.646484375\n\
         se 54.59375 1.7919921875 2.646484375\n"
    );
}

// f16 ARITHMETIC must produce a value the type can actually hold — the oracle
// half of B-2026-08-30-32, whose compiled twin is
// `wasm_f16_arithmetic_build_and_run_e2e` in `tests/cli.rs`.
//
// The bug was wasm-only: LLVM 18 legalizes wasm32 `half` with `PromoteFloat`,
// which computes in `f32` and never rounds the result back, so a wasm module
// carried values no `f16` can represent while the interpreter and the native
// binary were correct. This test therefore passes with or without the codegen
// fix BY DESIGN — its job is to pin what the answers ARE, so the compiled
// backend has something to be compared against that is not itself a compiled
// backend.
//
// The receiver is the literal `1` rather than `env.args().len()`: the compiled
// fixture needs `env.args()` to defeat constant folding, but an in-process
// interpreter test would read the TEST BINARY's argv, which is not 1.
// A generic body must compute at the width it was INSTANTIATED at, not at the
// tree-walk's f64 carrier. `Value::Float` is an f64 and the typechecker records
// the operand type inside `fn g[T: Add](a: T, b: T) -> T { a + b }` as the PARAM
// `T`, so `span_float_width` answered `None` and the rounding this file's other
// f16/f32 tests pin was skipped entirely one level of abstraction up
// (B-2026-08-30-36).
//
// It was never an f16 problem. At f32 the same body already read
// `0.4000000134110451` here against every compiled backend's
// `0.4000000059604645`, and an f32 product that overflows printed a finite
// 9e76 where they printed `inf`. f16 and bf16 only made it loud: 11 and 8
// significand bits diverge on nearly every operation, and 65504 * 3 is an
// overflow anyone can reach.
//
// Codegen has no matching hole — it monomorphizes, so the body is compiled
// against the concrete width — which made this a run-vs-build divergence. Every
// value below is the compiled backends' answer, and `tests/codegen.rs`'s
// `test_e2e_generic_arithmetic_at_reduced_precision` runs the same program
// through a real binary so the two stay pinned to each other.
#[test]
fn test_generic_arithmetic_computes_at_the_instantiated_float_width() {
    assert_eq!(
        run("fn g_add[T: Add](a: T, b: T) -> T { a + b }\n\
             fn g_sub[T: Sub](a: T, b: T) -> T { a - b }\n\
             fn g_mul[T: Mul](a: T, b: T) -> T { a * b }\n\
             fn g_div[T: Div](a: T, b: T) -> T { a / b }\n\
             fn g_rem[T: Rem](a: T, b: T) -> T { a % b }\n\
             fn g_neg[T: Neg](a: T) -> T { -a }\n\
             fn g_poly[T: Add + Mul](a: T, b: T) -> T { g_add(g_mul(a, b), b) }\n\
             fn g_sum[T: Add](xs: Vec[T], zero: T) -> T {\n\
             \x20   let mut acc = zero;\n\
             \x20   for x in xs { acc = acc + x; }\n\
             \x20   acc\n\
             }\n\
             fn main() {\n\
             \x20   let n: i64 = 1;\n\
             \x20   let one: f32 = n as f32;\n\
             \x20   let p: f16 = (one * 0.1f32) as f16;\n\
             \x20   let q: f16 = (one * 0.3f32) as f16;\n\
             \x20   let bp: bf16 = (one * 0.1f32) as bf16;\n\
             \x20   let bq: bf16 = (one * 0.3f32) as bf16;\n\
             \x20   let sp: f32 = one * 0.1f32;\n\
             \x20   let sq: f32 = one * 0.3f32;\n\
             \x20   println(f\"h {g_add(p,q)} {g_sub(p,q)} {g_mul(p,q)} {g_div(p,q)} {g_rem(p,q)} {g_neg(p)}\");\n\
             \x20   println(f\"b {g_add(bp,bq)} {g_sub(bp,bq)} {g_mul(bp,bq)} {g_div(bp,bq)} {g_rem(bp,bq)} {g_neg(bp)}\");\n\
             \x20   println(f\"s {g_add(sp,sq)} {g_sub(sp,sq)} {g_mul(sp,sq)} {g_div(sp,sq)} {g_rem(sp,sq)} {g_neg(sp)}\");\n\
             \x20   let big: f16 = (one * 65504.0f32) as f16;\n\
             \x20   let three: f16 = (one * 3.0f32) as f16;\n\
             \x20   let tiny: f16 = (one * 0.00001f32) as f16;\n\
             \x20   println(f\"o {g_mul(big,three)} {g_mul(tiny,tiny)}\");\n\
             \x20   println(f\"y {g_poly(p,q)} {g_poly(bp,bq)} {g_poly(sp,sq)}\");\n\
             \x20   let mut hx: Vec[f16] = vec![];\n\
             \x20   let mut bx: Vec[bf16] = vec![];\n\
             \x20   let mut i: i64 = 0;\n\
             \x20   while i < 10 { hx.push(p); bx.push(bp); i = i + 1; }\n\
             \x20   println(f\"a {g_sum(hx, (one * 0.0f32) as f16)} {g_sum(bx, (one * 0.0f32) as bf16)}\");\n\
             }\n"),
        // 65504 is the largest finite f16, so the generic product overflows;
        // tiny*tiny underflows past the smallest subnormal. Pre-fix this
        // surface printed 196512 and 1.0027e-10 for those two, and f32-or-wider
        // digits on every other line.
        "h 0.39990234375 -0.2000732421875 0.029998779296875 0.333251953125 0.0999755859375 -0.0999755859375\n\
         b 0.400390625 -0.201171875 0.0301513671875 0.33203125 0.10009765625 -0.10009765625\n\
         s 0.4000000059604645 -0.20000001788139343 0.030000001192092896 0.3333333134651184 0.10000000149011612 -0.10000000149011612\n\
         o inf 0\n\
         y 0.330078125 0.330078125 0.33000001311302185\n\
         a 1 1.0078125\n"
    );
}

// A scalar float method must be COMPUTED at the receiver's declared width, not
// computed in f64 and rounded into it afterwards. The tree-walk carries every
// float as an f64, so the second is the tempting shortcut — and it is exact for
// `+ - * /` and `sqrt`, where one correctly-rounded operation with 2p+2
// intermediate bits gives the same answer either way. It is NOT exact for the
// transcendentals: two roundings are not one, and codegen calls the f32 symbol.
//
// B-2026-08-29-41 filed this as `to_degrees` / `to_radians` / `cosh`. Measured,
// the set was bigger and the mechanism split in two:
//
//   * `to_degrees` / `to_radians` / `recip` never touched the width AT ALL — the
//     interpreter printed the same 17 digits for f32, f16 and bf16. `recip` was
//     not in the row: it shares their arm, and was missed because the row's
//     receiver made `1/x` exact. That is the same "agrees by luck of the inputs"
//     trap the vector twin B-2026-08-29-40 documents.
//   * `cosh`, and with it `sinh` / `log10` / `atan`, DID round but computed in
//     f64 first, landing 1 ULP off codegen on 34 of 160 f32 samples.
//
// Both are fixed by one rule, so both are pinned here. The f64 pair at the end
// is the boundary: f64 must NOT move, since a fix that computed everything at
// f32 would visibly truncate it.
//
// THE TRANSCENDENTAL LINES ARE NOT FROZEN DIGITS, and that is B-2026-09-01-48.
// They were, and the digits were glibc's, so this test failed on every
// `Test (macos-latest)` run: `coshf` is not correctly rounded on every
// platform's libm. Measured 2026-09-01 — `cosh(2.0f32)` is 3.7621958255767822
// under glibc and 3.762195587158203 under Apple's libm, and the f64 value
// 3.7621956910836314 is 1.04e-7 from the second against 1.34e-7 from the
// first, so macOS returns the CORRECTLY-ROUNDED f32 and glibc is the 1-ULP-off
// one. No single literal can hold on both hosts.
//
// So the oracle for those lines is Rust's own `f32` method. That is not a
// weakening: `f32::cosh` lowers to the same `coshf` codegen emits, so the
// assertion still reads "the interpreter agreed with the symbol codegen calls"
// — the property B-2026-08-29-41 fixed — it just stops spelling that symbol's
// answer as one host's decimal digits. The width-visible methods
// (`to_degrees` / `to_radians` / `recip`) stay literal: they are a single
// correctly-rounded multiply or divide, identical on any IEEE host, and their
// pre-fix failure was f64's 17 digits appearing on an f16 line, not 1 ULP.
//
// NOT `assert_prints_float_near`, this file's other answer to the same
// macOS/Linux last-ULP split (see its use on `asinh` above). That helper is
// right where the last ULP is the noise; here the last ULP is the SIGNAL —
// f64-then-round differs from the f32 symbol by exactly one — so a tolerant
// compare would admit the very regression being guarded. Exact equality
// against a per-host oracle is the stronger tool, and the available one,
// because Rust reaches the same symbol.
//
// ANTI-VACUITY. Whether a given receiver discriminates the two routes AT ALL
// is itself platform-dependent for the libm methods, and the four this test
// shipped with do not discriminate on macOS — measured, all four agree there,
// so on that host the test could only ever have been checking the frozen
// digits. Three receivers that DO discriminate on Apple's libm are added
// below. On windows-latest NONE of those seven discriminates — measured from
// CI, where this assertion took a required job red for a day
// (B-2026-09-03-3). WHY they all agree there is NOT measured; the leading
// candidate is that MSVC's `coshf`/`sinhf`/`atanf`/`log10f` are the double
// routine plus a round, which would make the two routes literally the same
// code and no receiver able to separate them ever. The repair below does not
// depend on settling that, which is the point of it.
//
// So the guard does not rest on libm at all. `to_degrees` and `to_radians`
// are a single f32 multiply, which makes "does the f32-width route differ
// from compute-in-f64-then-round" a fact about IEEE-754 rather than about the
// host: measured, 562 of the 3999 receivers i/1000 discriminate for
// `to_degrees` and 368 for `to_radians` (`recip` has none in that range — a
// reciprocal's double rounding is benign, which is why the receivers this
// test shipped with, `to_degrees(2.0)` / `to_radians(2.0)` / `recip(3.0)`,
// all happened to agree and left the guard leaning on libm). `9.0f32` and
// `0.3f32` are two that discriminate, they are asserted in the program below,
// and they are what the anti-vacuity assertion now requires. The libm samples
// keep their exact per-host oracle; their vacuity is REPORTED rather than
// asserted, because a host with no genuine f32 route makes that half
// unobservable, not wrong.
#[test]
fn test_scalar_float_methods_compute_at_the_declared_width() {
    // The f32 transcendental samples, as (receiver, f32-width answer).
    // `2.0` / `0.2857…` are the historical receivers (they separate the
    // routes under glibc); `1.22` / `1.175` / `0.057` were found by sweeping
    // i/1000 for i in 1..4000 and separate them under Apple's libm.
    // `black_box` is LOAD-BEARING, not decoration (B-2026-09-01-47). These
    // receivers are compile-time constants, so with optimization on LLVM folds
    // `a.cosh()` at build time using DOUBLE-precision host math and rounds the
    // result once — which is precisely the f64 route this test exists to tell
    // APART from the f32 route. The oracle would then equal the value it is
    // supposed to differ from, and the two `samples` columns would collapse
    // into one. Opaque receivers force real `coshf`/`cosh` calls, so the two
    // routes stay distinct and the expected text matches what the interpreter
    // and both compiled backends actually produce (all three call libm).
    //
    // Measured: `2.0f32.cosh()` is 3.7621958255767822 as a `coshf` call —
    // glibc's `coshf` is not correctly rounded — and 3.762195587158203 when
    // constant-folded through double. The interpreter, the JIT and the AOT
    // binary all print the first. Before this, an optimized build of this test
    // asserted the second.
    let a: f32 = std::hint::black_box(2.0);
    // The Kāra source spells this `0.2857142984867096f32` — the f64 digits of
    // the same f32. Kept short here because `clippy::excessive_precision`
    // rejects the long form on an `f32` binding; bit-identical either way.
    let s: f32 = std::hint::black_box(0.2857143);
    let c1: f32 = std::hint::black_box(1.22);
    let c2: f32 = std::hint::black_box(1.175);
    let c3: f32 = std::hint::black_box(0.057);
    let libm_samples: [(f32, f32, f32); 7] = [
        (a, a.cosh(), (a as f64).cosh() as f32),
        (s, s.sinh(), (s as f64).sinh() as f32),
        (s, s.log10(), (s as f64).log10() as f32),
        (s, s.atan(), (s as f64).atan() as f32),
        (c1, c1.cosh(), (c1 as f64).cosh() as f32),
        (c2, c2.sinh(), (c2 as f64).sinh() as f32),
        (c3, c3.atan(), (c3 as f64).atan() as f32),
    ];

    // The width discriminators that do not go through libm. `to_degrees` and
    // `to_radians` are one f32 multiply each, so whether the f32-width answer
    // differs from the f64-then-round one is settled by IEEE-754 and is the
    // same on every host — including one whose `coshf` is the double routine
    // in disguise. These two receivers separate the routes; `2.0` (which this
    // test already prints) does not, for either method.
    let g1: f32 = std::hint::black_box(9.0);
    let g2: f32 = std::hint::black_box(0.3);
    let ieee_samples: [(f32, f32); 2] = [
        (g1.to_degrees(), (g1 as f64).to_degrees() as f32),
        (g2.to_radians(), (g2 as f64).to_radians() as f32),
    ];

    // Kāra prints a narrow float by widening to f64, so the expected text for
    // an f32 result is that f64's shortest round-tripping form.
    let f32_line = |v: f32| format!("{}\n", v as f64);

    let expected = format!(
        "114.59156036376953\n\
         0.03490658476948738\n\
         {deg_g1}\
         {rad_g2}\
         {cosh_a}\
         114.5625\n\
         114.5\n\
         0.333984375\n\
         0.3333333432674408\n\
         {sinh_s}\
         {log10_s}\
         {atan_s}\
         {cosh_c1}\
         {sinh_c2}\
         {atan_c3}\
         114.59155902616465\n\
         {cosh_d}\n",
        deg_g1 = f32_line(ieee_samples[0].0),
        rad_g2 = f32_line(ieee_samples[1].0),
        cosh_a = f32_line(libm_samples[0].1),
        sinh_s = f32_line(libm_samples[1].1),
        log10_s = f32_line(libm_samples[2].1),
        atan_s = f32_line(libm_samples[3].1),
        cosh_c1 = f32_line(libm_samples[4].1),
        sinh_c2 = f32_line(libm_samples[5].1),
        atan_c3 = f32_line(libm_samples[6].1),
        cosh_d = std::hint::black_box(2.0f64).cosh(),
    );

    assert_eq!(
        run("fn main() {\n\
             \x20   let a: f32 = 2.0f32;\n\
             \x20   println(a.to_degrees());\n\
             \x20   println(a.to_radians());\n\
             \x20   let g1: f32 = 9.0f32;\n\
             \x20   println(g1.to_degrees());\n\
             \x20   let g2: f32 = 0.3f32;\n\
             \x20   println(g2.to_radians());\n\
             \x20   println(a.cosh());\n\
             \x20   let h: f16 = 2.0f16;\n\
             \x20   println(h.to_degrees());\n\
             \x20   let b: bf16 = 2.0bf16;\n\
             \x20   println(b.to_degrees());\n\
             \x20   let three: bf16 = 3.0bf16;\n\
             \x20   println(three.recip());\n\
             \x20   let t: f32 = 3.0f32;\n\
             \x20   println(t.recip());\n\
             \x20   let s: f32 = 0.2857142984867096f32;\n\
             \x20   println(s.sinh());\n\
             \x20   println(s.log10());\n\
             \x20   println(s.atan());\n\
             \x20   let c1: f32 = 1.22f32;\n\
             \x20   println(c1.cosh());\n\
             \x20   let c2: f32 = 1.175f32;\n\
             \x20   println(c2.sinh());\n\
             \x20   let c3: f32 = 0.057f32;\n\
             \x20   println(c3.atan());\n\
             \x20   let d: f64 = 2.0;\n\
             \x20   println(d.to_degrees());\n\
             \x20   println(d.cosh());\n\
             }\n"),
        expected
    );

    // The check that keeps the above honest: at least one receiver must
    // actually separate "computed at f32 width" from "computed at f64 and
    // rounded", or a tree-walk that took the f64 shortcut would satisfy every
    // line and this test would pass while testing nothing.
    //
    // Drawn over the ARITHMETIC receivers, not the libm ones, because those
    // discriminate by IEEE-754 rather than by what the host's libm happens to
    // do — so this holds on every host, which is what B-2026-09-03-3 was: on
    // windows-latest the guard fired over the libm samples, correctly
    // reporting that none of the seven it carried discriminates there, and
    // took a required CI job red for a day. If this fires, `g1` / `g2` were
    // edited into non-discriminating receivers; sweep i/1000 for a
    // replacement pair. Do not delete the assertion.
    assert!(
        ieee_samples
            .iter()
            .any(|(at_width, via_f64)| at_width.to_bits() != via_f64.to_bits()),
        "no arithmetic sample separates the f32-width route from \
         compute-in-f64-then-round; the receivers above must be ones that do, \
         or the test cannot observe the regression it exists for"
    );

    // The libm half's vacuity is REPORTED, not asserted. If `coshf` and
    // friends on a host are the double routine plus a round, the two routes
    // are the same code there and no receiver can separate them — nothing to
    // fix, and the assertion above already keeps the test from being vacuous.
    // Reporting rather than failing also means the next host in this position
    // says so instead of going red. Visible under `--nocapture`.
    if libm_samples
        .iter()
        .all(|(_, at_width, via_f64)| at_width.to_bits() == via_f64.to_bits())
    {
        eprintln!(
            "note: no cosh/sinh/log10/atan receiver here separates the f32-width \
             route from compute-in-f64-then-round, so those lines are pinned to \
             this host's libm but do not discriminate width on it"
        );
    }
}

#[test]
fn test_float_helper_block_computes_at_the_receiver_width_at_every_width() {
    // B-2026-08-30-5, interpreter half. Codegen was rounding the irrational
    // `to_degrees` / `to_radians` constant INTO an f16 receiver before
    // multiplying, where the interpreter builds it at f32 and rounds once at
    // the end; the two disagreed on ~25% of f16 inputs. The codegen twin of
    // this fixture lives in `tests/codegen.rs`
    // (`test_e2e_reduced_precision_receiver_multiplies_its_constant_at_f32`)
    // and asserts the SAME expected text, so the pair is the run==build
    // assertion split across the two suites: whichever backend drifts, one of
    // them fails, and a "fix" that moved the interpreter to match a broken
    // codegen would fail here rather than pass quietly.
    //
    // Bit-exact pins are portable here, unlike the libm transcendentals
    // `approx_line` exists for: an fmul by a constant, an fdiv by 1.0 and
    // `x - trunc(x)` are IEEE-exact on every platform.
    let src = r#"
fn main() {
    let a: f16 = 0.0304718017578125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -0.0304718017578125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 0.060943603515625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -0.060943603515625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -3.900390625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 7.80078125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -7.80078125f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -249.625f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = 499.25f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let a: f16 = -499.25f16;
    println(f"f16  {a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
    let b: f16 = 1.25f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = -3.5f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = 0.375f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f16 = -17.75f16;
    println(f"f16  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = 1.25bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = -3.5bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = 0.375bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: bf16 = -17.75bf16;
    println(f"bf16 {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = 1.25f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = -3.5f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = 0.375f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f32 = -17.75f32;
    println(f"f32  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = 1.25f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = -3.5f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = 0.375f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
    let b: f64 = -17.75f64;
    println(f"f64  {b.to_degrees()} {b.to_radians()} {b.recip()} {b.fract()}");
}
"#;
    assert_eq!(
        run(src),
        concat!(
            "f16  1.74609375 0.0005316734313964844 32.8125 0.0304718017578125\n",
            "f16  -1.74609375 -0.0005316734313964844 -32.8125 -0.0304718017578125\n",
            "f16  3.4921875 0.0010633468627929688 16.40625 0.060943603515625\n",
            "f16  -3.4921875 -0.0010633468627929688 -16.40625 -0.060943603515625\n",
            "f16  -223.5 -0.06805419921875 -0.25634765625 -0.900390625\n",
            "f16  447 0.1361083984375 0.128173828125 0.80078125\n",
            "f16  -447 -0.1361083984375 -0.128173828125 -0.80078125\n",
            "f16  -14304 -4.35546875 -0.00400543212890625 -0.625\n",
            "f16  28608 8.7109375 0.002002716064453125 0.25\n",
            "f16  -28608 -8.7109375 -0.002002716064453125 -0.25\n",
            "f16  71.625 0.021820068359375 0.7998046875 0.25\n",
            "f16  -200.5 -0.06109619140625 -0.28564453125 -0.5\n",
            "f16  21.484375 0.0065460205078125 2.666015625 0.375\n",
            "f16  -1017 -0.309814453125 -0.05633544921875 -0.75\n",
            "bf16 71.5 0.0218505859375 0.80078125 0.25\n",
            "bf16 -201 -0.06103515625 -0.28515625 -0.5\n",
            "bf16 21.5 0.00653076171875 2.671875 0.375\n",
            "bf16 -1016 -0.310546875 -0.056396484375 -0.75\n",
            "f32  71.6197280883789 0.021816615015268326 0.800000011920929 0.25\n",
            "f32  -200.5352325439453 -0.06108652427792549 -0.2857142984867096 -0.5\n",
            "f32  21.485918045043945 0.006544984877109528 2.6666667461395264 0.375\n",
            "f32  -1017.0001220703125 -0.30979594588279724 -0.056338027119636536 -0.75\n",
            "f64  71.6197243913529 0.02181661564992912 0.8 0.25\n",
            "f64  -200.53522829578813 -0.061086523819801536 -0.2857142857142857 -0.5\n",
            "f64  21.48591731740587 0.006544984694978736 2.6666666666666665 0.375\n",
            "f64  -1017.0000863572112 -0.3097959422289935 -0.056338028169014086 -0.75\n",
        )
    );
}

#[test]
fn test_vector_integer_shift() {
    // std.simd.math (phase-11): element-wise `<<` / `>>` on integer vectors
    // (the last Sleef building block). `>>` is logical on unsigned lanes and
    // arithmetic on signed lanes. The final line exercises the Sleef 2^n
    // idiom: bits_as_f32((n + 127) << 23) with n=3 → 8.0. Matches
    // tests/codegen.rs::test_e2e_vector_integer_shift.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[u32, 4].from_array([1u32, 2u32, 3u32, 4u32]);
    let l = v << Vector[u32, 4].splat(3u32);
    println(l[0]);
    println(l[3]);
    let r = Vector[u32, 4].splat(2147483648u32) >> Vector[u32, 4].splat(4u32);
    println(r[0]);
    let ar = Vector[i32, 4].splat(-16) >> Vector[i32, 4].splat(2);
    println(ar[0]);
    let expo = (Vector[u32, 4].splat(3u32) + Vector[u32, 4].splat(127u32))
        << Vector[u32, 4].splat(23u32);
    let pow = expo.bits_as_f32();
    println(pow[0]);
}
"#,
    );
    assert_eq!(out, "8\n32\n134217728\n-4\n8\n");
}

#[test]
fn test_vector_i64_construct_add_index() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let b: Vector[i64, 4] = Vector[i64, 4](10, 20, 30, 40);
    let c = a + b;
    println(c[0]);
    println(c[3]);
}
"#,
    );
    assert_eq!(out, "11\n44\n");
}

#[test]
fn test_vector_i64_mul_and_sub() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](2, 3, 4, 5);
    let b: Vector[i64, 4] = Vector[i64, 4](10, 10, 10, 10);
    let prod = a * b;
    let diff = b - a;
    println(prod[1]);
    println(diff[2]);
}
"#,
    );
    // prod = [20, 30, 40, 50] -> [1] == 30; diff = [8, 7, 6, 5] -> [2] == 6
    assert_eq!(out, "30\n6\n");
}

#[test]
fn test_vector_inferred_binding_type() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](7, 8);
    let b = Vector[i64, 2](100, 200);
    let c = a + b;
    println(c[1]);
}
"#,
    );
    assert_eq!(out, "208\n");
}

#[test]
fn test_vector_f64_elementwise_div() {
    // Parity with codegen: f64 whole numbers print without a decimal point.
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vector[f64, 2] = Vector[f64, 2](10.0, 9.0);
    let b: Vector[f64, 2] = Vector[f64, 2](2.0, 3.0);
    let q = a / b;
    println(q[0]);
    println(q[1]);
}
"#,
    );
    assert_eq!(out, "5\n3\n");
}

#[test]
fn test_vector_value_semantics_no_aliasing() {
    // `Vector` is Copy: rebinding does not alias. (Lane mutation isn't in the
    // slice-1 surface, so this pins the representation choice for when it is.)
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](1, 2);
    let b = a;
    let c = a + b;
    println(c[0]);
    println(c[1]);
}
"#,
    );
    assert_eq!(out, "2\n4\n");
}

#[test]
fn test_vector_lane_out_of_bounds_is_runtime_error() {
    let errs = runtime_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](1, 2);
    println(a[5]);
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.message.contains("lane index")),
        "expected a vector lane out-of-bounds runtime error, got: {errs:?}"
    );
}

// ── Portable SIMD `Vector[T, N]` — slice 2 reductions (interpreter) ───
// Mirror the codegen slice-2 run-tests for cross-backend parity.

#[test]
fn test_vector_reduce_sum_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_sum()); }");
    assert_eq!(out, "10\n");
}

#[test]
fn test_vector_reduce_sum_f64() {
    let out =
        run_no_errors("fn main() { let v = Vector[f64, 2](1.5, 2.5); println(v.reduce_sum()); }");
    assert_eq!(out, "4\n");
}

#[test]
fn test_vector_dot_i64() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let b = Vector[i64, 4](10, 20, 30, 40);
    println(a.dot(b));
}
"#,
    );
    // 1*10 + 2*20 + 3*30 + 4*40 = 10 + 40 + 90 + 160 = 300
    assert_eq!(out, "300\n");
}

#[test]
fn test_vector_dot_f64() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[f64, 2](2.0, 3.0);
    let b = Vector[f64, 2](4.0, 5.0);
    println(a.dot(b));
}
"#,
    );
    // 2*4 + 3*5 = 8 + 15 = 23
    assert_eq!(out, "23\n");
}

// ── Vector slice 2b — product + bitwise reductions (interpreter) ──────

#[test]
fn test_vector_reduce_product_i64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_product()); }",
    );
    assert_eq!(out, "24\n");
}

#[test]
fn test_vector_reduce_and_i64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[i64, 4](15, 7, 3, 1); println(v.reduce_and()); }",
    );
    assert_eq!(out, "1\n");
}

#[test]
fn test_vector_reduce_or_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_or()); }");
    assert_eq!(out, "15\n");
}

#[test]
fn test_vector_reduce_xor_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_xor()); }");
    assert_eq!(out, "15\n");
}

// ── Vector slice 2c — min/max (interpreter) ──────────────────────────

#[test]
fn test_vector_reduce_min_max_i64() {
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4](3, 1, 4, 2);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "1\n4\n");
}

#[test]
fn test_vector_reduce_min_max_i64_negative() {
    // Signed comparison: -5 is the min, 2 the max.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 3](-5, 2, -3);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "-5\n2\n");
}

#[test]
fn test_vector_reduce_min_max_f64() {
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[f64, 2](2.5, 1.5);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "1.5\n2.5\n");
}

#[test]
fn test_vector_reduce_min_max_u32() {
    // Slice 2e-ii: unsigned element accepted (previously a type error). The
    // interpreter reads element signedness off the receiver's recorded type
    // and compares its `Value::Int` carrier as `u64`. u32 values all fit
    // positively in i64, so signed and unsigned agree here — this pins the
    // parity contract with codegen's unsigned-result `5\n4000000000\n`.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[u32, 4](3000000000, 5, 10, 4000000000);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "5\n4000000000\n");
}

// ── Vector slice 2c — cross product interpreter parity ───────────────

#[test]
fn test_vector_cross_i64() {
    // (2,3,4) × (5,6,7) = (-3, 6, -3). Same expected output as the codegen
    // E2E test (`tests/codegen.rs::test_vector_cross_i64`) — interpreter and
    // compiled backends must agree lane-for-lane.
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vector[i64, 3] = Vector[i64, 3](2, 3, 4);
    let b: Vector[i64, 3] = Vector[i64, 3](5, 6, 7);
    let c = a.cross(b);
    println(c[0]);
    println(c[1]);
    println(c[2]);
}
"#,
    );
    assert_eq!(out, "-3\n6\n-3\n");
}

#[test]
fn test_vector_cross_f64_orthonormal() {
    // x̂ × ŷ = ẑ: (1,0,0) × (0,1,0) = (0,0,1).
    let out = run_no_errors(
        r#"
fn main() {
    let a: Vector[f64, 3] = Vector[f64, 3](1.0, 0.0, 0.0);
    let b: Vector[f64, 3] = Vector[f64, 3](0.0, 1.0, 0.0);
    let c = a.cross(b);
    println(c[0]);
    println(c[1]);
    println(c[2]);
}
"#,
    );
    assert_eq!(out, "0\n0\n1\n");
}

// ── Vector slice 2d — splat (scalar broadcast) interpreter parity ─────

#[test]
fn test_vector_splat_i64() {
    // Vector[i64, 4].splat(7) → all four lanes == 7.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4].splat(7);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    assert_eq!(out, "7\n7\n");
}

#[test]
fn test_vector_splat_enables_scalar_broadcast_arithmetic() {
    // splat is the explicit broadcast: `v + Vector[T,N].splat(s)` is how a
    // scalar combines with a vector (bare vector-vs-scalar arithmetic stays
    // a type error). [1,2,3,4] + splat(10) = [11,12,13,14].
    let out = run_no_errors(
        r#"
fn main() {
    let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let r = v + Vector[i64, 4].splat(10);
    println(r[0]);
    println(r[3]);
}
"#,
    );
    assert_eq!(out, "11\n14\n");
}

#[test]
fn test_vector_splat_f64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[f64, 2].splat(1.5); println(v[0]); println(v[1]); }",
    );
    assert_eq!(out, "1.5\n1.5\n");
}

#[test]
fn test_vector_from_array_i64() {
    // Vector[i64, 4].from_array([10, 20, 30, 40]) → lanes in order.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4].from_array([10, 20, 30, 40]);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    assert_eq!(out, "10\n40\n");
}

#[test]
fn test_vector_from_array_feeds_arithmetic() {
    // from_array participates in element-wise vector ops like any other
    // Vector[T, N]: [1,2,3,4] + [10,20,30,40] = [11,22,33,44].
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4].from_array([1, 2, 3, 4]);
    let b = Vector[i64, 4].from_array([10, 20, 30, 40]);
    let r = a + b;
    println(r[0]);
    println(r[3]);
}
"#,
    );
    assert_eq!(out, "11\n44\n");
}

#[test]
fn test_vector_from_array_f64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[f64, 2].from_array([1.5, 2.5]); println(v[0]); println(v[1]); }",
    );
    assert_eq!(out, "1.5\n2.5\n");
}

// ── Vector slice 2e-iii — from_slice (runtime-length construction) ────

#[test]
fn test_vector_from_slice_i64() {
    // Whole-array slice → vector. The runtime len==N check passes.
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let v = Vector[i64, 4].from_slice(a.as_slice());
    println(v.reduce_sum());
    println(v[0]);
    println(v[3]);
}
"#,
    );
    assert_eq!(out, "100\n10\n40\n");
}

#[test]
fn test_vector_from_slice_subslice_offset() {
    // A range-indexed sub-slice (start != 0): `a[1..5]` is the window
    // {2,3,4,5}, so the interpreter must read from `start..start+len`.
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 6] = [1, 2, 3, 4, 5, 6];
    let v = Vector[i64, 4].from_slice(a[1..5]);
    println(v[0]);
    println(v[3]);
    println(v.reduce_sum());
}
"#,
    );
    assert_eq!(out, "2\n5\n14\n");
}

#[test]
fn test_vector_from_slice_length_mismatch_panics() {
    // A 3-element slice for a 4-lane vector is a runtime error (the length
    // is only known at runtime, so the typechecker can't catch it).
    let errs = runtime_errors(
        r#"
fn main() {
    let a: Array[i64, 3] = [10, 20, 30];
    let v = Vector[i64, 4].from_slice(a.as_slice());
    println(v[0]);
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| format!("{e:?}").contains("does not match Vector lane count")),
        "expected a from_slice length-mismatch runtime error; got: {errs:?}"
    );
}

// ── Vector slice 3a — bitwise & | ^ (binary) and ~ (unary) ───────────

#[test]
fn test_vector_bitwise_and_or_xor() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](12, 10, 15, 3);
    let b = Vector[i64, 4](10, 6, 1, 3);
    let band = a & b;
    let bor = a | b;
    let bxor = a ^ b;
    println(band[0]); // 12 & 10 = 8
    println(bor[1]);  // 10 | 6  = 14
    println(bxor[2]); // 15 ^ 1  = 14
}
"#,
    );
    assert_eq!(out, "8\n14\n14\n");
}

#[test]
fn test_vector_bitnot() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](0, 3, -1, 255);
    let n = ~a;
    println(n[0]); // ~0   = -1
    println(n[1]); // ~3   = -4
    println(n[2]); // ~-1  = 0
    println(n[3]); // ~255 = -256
}
"#,
    );
    assert_eq!(out, "-1\n-4\n0\n-256\n");
}

// ── Vector slice 3b — comparison → Mask[N] + select ──────────────────

#[test]
fn test_vector_compare_mask() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 5, 3, 8);
    let b = Vector[i64, 4](4, 2, 3, 6);
    let lt = a < b;
    let eq = a == b;
    println(lt[0]); // 1<4 = true
    println(lt[1]); // 5<2 = false
    println(eq[2]); // 3==3 = true
    println(eq[0]); // 1==4 = false
}
"#,
    );
    assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
}

// ── Slice 6a — lane permutations (parity with codegen) ──────────────

#[test]
fn test_vector_reverse() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.reverse(); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "4\n3\n2\n1\n");
}

#[test]
fn test_vector_rotate_lanes_left() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_left(1); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "20\n30\n40\n10\n");
}

#[test]
fn test_vector_rotate_lanes_right() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_right(1); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "40\n10\n20\n30\n");
}

#[test]
fn test_vector_rotate_wraps_modulo_lanes() {
    // rotate_left(5) on 4 lanes wraps to rotate_left(1) — parity with codegen.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_left(5); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "20\n30\n40\n10\n");
}

#[test]
fn test_vector_replace() {
    // replace(2, 99) returns a new vector with lane 2 set; the original is
    // unchanged (value semantics — a[2] still reads 3).
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.replace(2, 99); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); println(a[2]); }",
    );
    assert_eq!(out, "1\n2\n99\n4\n3\n");
}

#[test]
fn test_vector_replace_runtime_index() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let i = 0; let r = a.replace(i, 7); \
         println(r[0]); println(r[1]); }",
    );
    assert_eq!(out, "7\n2\n");
}

#[test]
fn test_vector_shuffle_permute() {
    // shuffle gathers source lanes by index — parity with codegen.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.shuffle([0, 2, 1, 3]); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "10\n30\n20\n40\n");
}

#[test]
fn test_vector_shuffle_widening_with_repeats() {
    // M (index-list length) may differ from N, and indices may repeat.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 2](7, 9); let r = a.shuffle([1, 0, 1, 0]); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "9\n7\n9\n7\n");
}

#[test]
fn test_vector_load_masked_tail() {
    // Tail handling: a 2-element slice into a 4-lane vector, mask true for the
    // first two lanes — active lanes load, inactive read 0 (parity w/ codegen).
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let tail = a[0..2];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let v = Vector[i64, 4].load_masked(tail, m);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    assert_eq!(out, "10\n20\n0\n0\n");
}

#[test]
fn test_vector_load_masked_float_zero_fill() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[f64, 4] = [1.5, 2.5, 3.5, 4.5];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let v = Vector[f64, 4].load_masked(a.as_slice(), m);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    assert_eq!(out, "1.5\n2.5\n0\n0\n");
}

#[test]
fn test_vector_store_masked_partial() {
    // Writes active lanes through a mut slice; inactive lanes preserved
    // (parity with codegen). Lanes 0,1 active → written; 2,3 inactive.
    let out = run_no_errors(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    v.store_masked(xs, m);
}
fn main() {
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    fill(mut a);
    println(a[0]); println(a[1]); println(a[2]); println(a[3]);
}
"#,
    );
    assert_eq!(out, "10\n20\n3\n4\n");
}

#[test]
fn test_vector_gather_permuted_indices() {
    // gather reads slice[indices[i]] per lane (parity with codegen).
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let idx = Vector[i64, 4](5, 0, 3, 1);
    let v = Vector[i64, 4].gather(a.as_slice(), idx);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    assert_eq!(out, "60\n10\n40\n20\n");
}

#[test]
fn test_vector_scatter_permuted_indices() {
    // scatter writes slice[indices[i]] = v[i] (parity with codegen).
    let out = run_no_errors(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](3, 1, 0, 2);
    v.scatter(xs, idx);
}
fn main() {
    let mut a: Array[i64, 4] = [0, 0, 0, 0];
    fill(mut a);
    println(a[0]); println(a[1]); println(a[2]); println(a[3]);
}
"#,
    );
    assert_eq!(out, "30\n20\n40\n10\n");
}

#[test]
fn test_vector_cast_from_roundtrip() {
    // f64 -> i64 (truncate) then i64 -> f64 (parity with codegen).
    let out = run_no_errors(
        r#"
fn main() {
    let f = Vector[f64, 4](1.7, 2.2, 3.9, 4.0);
    let i = Vector[i64, 4].cast_from(f);
    println(i[0]); println(i[1]); println(i[2]); println(i[3]);
    let back = Vector[f64, 4].cast_from(i);
    println(back[0]); println(back[3]);
}
"#,
    );
    assert_eq!(out, "1\n2\n3\n4\n1\n4\n");
}

#[test]
fn test_vector_compare_unsigned_mask() {
    // Unsigned compare: 3000000000 (high bit set as i32) is NOT < 10.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[u32, 2](3000000000, 5);
    let b = Vector[u32, 2](10, 10);
    let m = a < b;
    println(m[0]); // false (unsigned)
    println(m[1]); // true
}
"#,
    );
    assert_eq!(out, "false\ntrue\n");
}

// ── Slice 4 — first-class Numeric trait + lane-literal ergonomics ─────

#[test]
fn test_numeric_generic_arithmetic() {
    // `[T: Numeric]` enables arithmetic on the bounded parameter; monomorphized
    // for both i64 and f64.
    let out = run_no_errors(
        r#"
fn add3[T: Numeric](a: T, b: T, c: T) -> T { a + b + c }
fn neg[T: Numeric](x: T) -> T { -x }
fn main() {
    println(add3(1, 2, 3));
    println(add3(1.5, 2.5, 3.0));
    println(neg(5));
}
"#,
    );
    assert_eq!(out, "6\n7\n-5\n");
}

#[test]
fn test_vector_f32_suffixless_lanes() {
    // Lane literals `1.0` (default f64) coerce to f32 lanes — no suffix.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[f32, 4](1.0, 2.0, 3.0, 4.0);
    let b = Vector[f32, 4](0.5, 0.5, 0.5, 0.5);
    let c = a * b;
    println(c[0]); // 0.5
    println(c[3]); // 2
}
"#,
    );
    assert_eq!(out, "0.5\n2\n");
}

#[test]
fn test_vector_i32_suffixless_lanes() {
    // Lane literals `1` (default i64) coerce to i32 lanes — no suffix.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i32, 4](1, 2, 3, 4);
    let b = Vector[i32, 4](10, 20, 30, 40);
    let c = a + b;
    println(c[0]); // 11
    println(c[3]); // 44
}
"#,
    );
    assert_eq!(out, "11\n44\n");
}

/// Interpreter ORACLE for B-2026-08-27-37: a generic struct destructured out
/// of a by-value TUPLE param, with its heap field moved out and returned. The
/// compiled backends DOUBLE-FREED this (the mono's tuple param got no
/// entry-copy while the caller still dropped its temp, so both aliased one
/// buffer); the interpreter was correct throughout, which is what made the
/// run/build divergence visible at all.
///
/// Both element types run — this fired at `T = i64` as well as `T = String`,
/// unlike the wrong-monomorph family where the scalar leg is silently fine.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_generic_struct_destructured_from_a_tuple_param_frees_once`, with the
/// ASAN twin in `tests/memory_sanitizer.rs`.
#[test]
fn generic_struct_destructured_from_a_tuple_param_round_trips() {
    let out = run(r#"
struct Bag[T] { xs: Vec[T] }
fn take[T](p: (Bag[T], i64)) -> Vec[T] { let (b, _n) = p; b.xs }
fn main() {
    let a = take((Bag { xs: ["x", "y"] }, 0));
    println(f"{a.len()} {a[0]}");
    let b = take((Bag { xs: [10, 20] }, 0));
    println(f"{b.len()} {b[0]}");
}
"#);
    assert_eq!(out, "2 x\n2 10\n");
}

/// Interpreter oracle for B-2026-08-27-44's escape shapes — the twin of
/// `tests/codegen.rs`'s `e2e_heap_tuple_arg_escaping_the_frame_is_freed_once`.
///
/// The defect itself was codegen-only (a missing caller-side free), so what
/// this pins is the ORACLE: these five programs' values, against which the
/// compiled backends must agree. The interpreter was already correct on all of
/// them, which is exactly what makes it a usable oracle here.
#[test]
fn heap_tuple_arg_escaping_the_frame_round_trips() {
    assert_eq!(
        run(r#"
struct Bag[T] { xs: Vec[T] }
fn passthru[T](p: (Bag[T], i64)) -> (Bag[T], i64) { p }
fn main() { let (b, n) = passthru((Bag { xs: ["x", "y"] }, 7)); println(f"{n} {b.xs.len()} {b.xs[0]}"); }
"#),
        "7 2 x\n"
    );
    assert_eq!(
        run(r#"
struct Bag { xs: Vec[String] }
fn passthru(p: (Bag, i64)) -> (Bag, i64) { p }
fn main() { let (b, n) = passthru((Bag { xs: ["x", "y"] }, 7)); println(f"{n} {b.xs.len()} {b.xs[0]}"); }
"#),
        "7 2 x\n"
    );
    assert_eq!(
        run(r#"
struct Bag[T] { xs: Vec[T] }
fn mk[T](v: Vec[T]) -> (Bag[T], i64) { (Bag { xs: v }, 3) }
fn use2[T](p: (Bag[T], i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { println(f"{use2(mk(["x", "y"]))}"); }
"#),
        "5\n"
    );
    assert_eq!(
        run(r#"
struct Bag { xs: Vec[String] }
fn mk(v: Vec[String]) -> (Bag, i64) { (Bag { xs: v }, 3) }
fn use2(p: (Bag, i64)) -> i64 { let (b, n) = p; b.xs.len() + n }
fn main() { println(f"{use2(mk(["x"])) + use2(mk(["y", "z"]))}"); }
"#),
        "9\n"
    );
    assert_eq!(
        run(r#"
fn passthru(p: (String, i64)) -> (String, i64) { p }
fn main() { let (s, n) = passthru((f"abc", 7)); println(f"{n} {s}"); }
"#),
        "7 abc\n"
    );
}

#[test]
fn container_argument_from_a_struct_field_round_trips() {
    // The row's own repro: a nameless tuple element, inside a generic impl.
    assert_eq!(
        run(r#"
struct Bag[=T] { xs: Vec[T] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
impl[T] Bag[T] {
    fn go(mut ref self) { swap01(mut self.xs); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}
fn main() {
    let mut a: Bag[(i64, i64)] = Bag { xs: Vec.new() };
    a.xs.push((1, 10)); a.xs.push((2, 20)); a.go();
    let z0 = a.at(0); let z1 = a.at(1);
    println(f"{z0.0}:{z0.1} {z1.0}:{z1.1}");
}
"#),
        "2:20 1:10\n"
    );
    // A NAMED element in the same impl — the compiled backends segfaulted.
    assert_eq!(
        run(r#"
struct Bag[=T] { xs: Vec[T] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
impl[T] Bag[T] {
    fn go(mut ref self) { swap01(mut self.xs); }
    fn at(ref self, i: i64) -> T { return self.xs[i]; }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb"); a.go();
    println(f"{a.at(0)} {a.at(1)}");
}
"#),
        "bb aa\n"
    );
    // No impl, non-generic struct, nameless element.
    assert_eq!(
        run(r#"
struct Bag { xs: Vec[(i64, i64)] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
fn main() {
    let mut a: Bag = Bag { xs: Vec.new() };
    a.xs.push((1, 10)); a.xs.push((2, 20));
    swap01(mut a.xs);
    let z0 = a.xs[0]; let z1 = a.xs[1];
    println(f"{z0.0}:{z0.1} {z1.0}:{z1.1}");
}
"#),
        "2:20 1:10\n"
    );
    // The RETURNING callee — the loud half, which failed module verification.
    assert_eq!(
        run(r#"
struct Bag[=T] { xs: Vec[T] }
fn first[T](v: ref Vec[T]) -> T { return v[0]; }
impl[T] Bag[T] {
    fn head(ref self) -> T { return first(self.xs); }
}
fn main() {
    let mut a: Bag[(i64, i64)] = Bag { xs: Vec.new() };
    a.xs.push((1, 10)); a.xs.push((2, 20));
    let h = a.head();
    println(f"{h.0}:{h.1}");
}
"#),
        "1:10\n"
    );
    // The read-only `ref` spelling (-51): the compiled backends aborted with
    // `free(): double free detected` where the tree walk was always correct.
    assert_eq!(
        run(r#"
struct Bag[=T] { xs: Vec[T] }
fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }
impl[T] Bag[T] {
    fn go(ref self) -> i64 { return vlen(self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); println(f"{a.go()}");
}
"#),
        "1\n"
    );
}

/// B-2026-08-31-24 (oracle half) — what a `Vec[Vector[T, N]]` and its
/// over-aligned relatives print.
///
/// The bug was an ALIGNMENT one, invisible to this backend: the interpreter has no
/// `vmovaps` and no malloc'd element buffer to under-align, so it rendered all of
/// this correctly throughout. It is here as the oracle the compiled side is
/// asserted against — a shape added to one fixture widens the other.
///
/// Twin of `tests/codegen.rs`'s `e2e_vec_of_vector_operations_round_trip`, pinned
/// to the same string.
///
/// The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
/// interpreter test sees the TEST binary's argv, which is 1 only when the suite
/// runs unfiltered.
#[test]
fn test_vec_of_vector_operations_round_trip() {
    assert_eq!(
        run(r#"#[derive(Display)]
struct Holder { v: Vector[i64, 4], n: i64 }
#[derive(Display)]
enum E { V(Vector[i64, 4]), N }

fn mk(n: i64) -> Vec[Vector[i64, 4]] {
    let mut v: Vec[Vector[i64, 4]] = Vec.new();
    v.push(Vector[i64, 4](n, n + 1, n + 2, n + 3));
    return v
}

fn main() {
    let n: i64 = 1;
    let vv: Vector[i64, 4] = Vector[i64, 4](n, n + 1, n + 2, n + 3);
    let ww: Vector[i64, 4] = Vector[i64, 4](n + 4, n + 5, n + 6, n + 7);
    let v8: Vector[i64, 8] = Vector[i64, 8](n, n+1, n+2, n+3, n+4, n+5, n+6, n+7);

    let lit: Vec[Vector[i64, 4]] = [vv, ww];
    println(f"lit  {lit}");
    println(f"idx  {lit[0]}");

    let mut p: Vec[Vector[i64, 4]] = Vec.new();
    p.push(vv);
    p.push(ww);
    p.push(vv);
    println(f"push {p}");
    for x in p { println(f"for  {x}"); }
    p[1] = ww;
    println(f"set  {p}");
    match p.pop() { Some(w) => { println(f"pop  {w}"); } None => {} }
    p.insert(0, ww);
    println(f"ins  {p}");
    println(f"rem  {p.remove(0)}");

    println(f"ret  {mk(n)}");

    let mut wide: Vec[Vector[i64, 8]] = Vec.new();
    wide.push(v8);
    println(f"w8   {wide}");

    let mut hs: Vec[Holder] = Vec.new();
    hs.push(Holder { v: vv, n: n });
    println(f"hs   {hs}");

    let mut es: Vec[E] = Vec.new();
    es.push(E.V(vv));
    println(f"es   {es}");

    let mut m: Map[String, Vector[i64, 4]] = Map.new();
    m.insert(f"k", vv);
    println(f"map  {m}");

    let mut os: Vec[Option[Vector[i64, 4]]] = Vec.new();
    os.push(Some(vv));
    println(f"os   {os}");

    let arr: Array[Vector[i64, 4], 2] = [vv, ww];
    let mut av: Vec[Array[Vector[i64, 4], 2]] = Vec.new();
    av.push(arr);
    println(f"av   {av}");

    let mut big: Vec[Vector[i64, 4]] = Vec.new();
    let mut i = 0;
    while i < 40 {
        big.push(Vector[i64, 4](i + n, i + n + 1, i + n + 2, i + n + 3));
        i = i + 1;
    }
    let mut s: i64 = 0;
    for x in big { s = s + x.reduce_sum(); }
    println(f"sum  {s}");
}
"#),
        r#"lit  [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]
idx  Vector(1, 2, 3, 4)
push [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8), Vector(1, 2, 3, 4)]
for  Vector(1, 2, 3, 4)
for  Vector(5, 6, 7, 8)
for  Vector(1, 2, 3, 4)
set  [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8), Vector(1, 2, 3, 4)]
pop  Vector(1, 2, 3, 4)
ins  [Vector(5, 6, 7, 8), Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]
rem  Vector(5, 6, 7, 8)
ret  [Vector(1, 2, 3, 4)]
w8   [Vector(1, 2, 3, 4, 5, 6, 7, 8)]
hs   [Holder { v: Vector(1, 2, 3, 4), n: 1 }]
es   [V(Vector(1, 2, 3, 4))]
map  {k: Vector(1, 2, 3, 4)}
os   [Some(Vector(1, 2, 3, 4))]
av   [[Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]]
sum  3520
"#
    );
}

/// B-2026-08-31-18 — a `Vector[T, N]` or `Array[T, N]` ENUM PAYLOAD survives
/// the round trip through the variant's payload words.
///
/// Both were sized as ONE word. For an array that was a pure under-count (the
/// pack side correctly produced N words, so `out.len() > num_words` heap-boxed
/// it while the unpack, recomputing the same conservative 1, read word 0 as the
/// value); for a vector it was worse, because `coerce_to_i64` has no vector arm
/// and returns a literal ZERO — the payload was not truncated, it was erased.
///
/// The `m*` rows are the ones that matter. A `match` binding is a VALUE, so
/// `E.V4(w) => w` bound `0` (or the first element, or a box pointer) with
/// nothing in the program to say so; the Display rows only make the same
/// corruption visible. `b[0]` on an array binding was a hard codegen error
/// ("Index operator applied to non-array type") because the binding rebuilt as
/// an `i64`.
///
/// EVERY ROW IS A DIFFERENT PART OF THE WORD ACCOUNTING:
///  - `d4`/`m4` — 4 lanes into a variant area wide enough to hold them inline.
///  - `d8`/`m8`/`o8` — 8 lanes. Inline in `E` (whose area is the max over
///    variants) and BOXED in `Option`, whose area is 3, so this is the pair
///    that exercises the debox path and the `malloc`-alignment fix with it.
///  - `d32`/`m32` — lanes NARROWER than a word, which the one-word-per-lane
///    convention zero-extends and the rebuild truncates back.
///  - `df`/`mf` — f64 lanes, which round-trip as bit patterns, and `dh`/`mh` +
///    `du`/`mu` — f16 and u8 lanes, SUB-WORD components unpacked at their exact
///    width. Narrow floats have been the omitted case in several hand-written
///    width lists this month, so they are pinned here.
///  - `ma`/`oa` — the array half, read through an INDEX so the binding's LLVM
///    type is asserted and not just its rendering.
///  - `hw` — a vector AND an array as FIELDS of a struct payload, the shape
///    that reached `reconstruct_payload_value`'s "unexpected multi-word
///    non-struct field" fallback and `insertvalue`d an `i64` into a
///    `<4 x i64>` slot: invalid IR that failed module verification.
///
/// The array payloads sit on a NON-derived enum deliberately: `Array[T, N]` has
/// no arm in `emit_display_fn_for_type_expr`, so a derived-Display enum
/// carrying one panics the compiler (B-2026-08-31-19) before any of this runs.
/// `Vec[Vector[T, N]]` is absent for a different reason — its element buffer is
/// `malloc`ed and accessed at the vector's natural 32-byte alignment, which
/// faults depending on heap state (B-2026-08-31-24).
///
/// Twin of `tests/codegen.rs`'s `e2e_vector_array_enum_payload_round_trip`,
/// pinned to the same string. The interpreter rendered all of this correctly
/// throughout — it is the oracle the compiled side is asserted against.
///
/// The seed is the literal 1 rather than `env.args().len()`: an IN-PROCESS
/// interpreter test sees the TEST binary's argv, which is 1 only when the suite
/// runs unfiltered. The codegen twin keeps `env.args()` because it needs an
/// opaque seed to survive -O2 folding.
#[test]
fn test_vector_array_enum_payload_round_trip() {
    assert_eq!(
        run(r#"#[derive(Display)]
enum E {
    V4(Vector[i64, 4]),
    V8(Vector[i64, 8]),
    V32(Vector[i32, 4]),
    Vf(Vector[f64, 2]),
    Vh(Vector[f16, 2]),
    Vu(Vector[u8, 4]),
    N,
}

struct Holder { v: Vector[i64, 4], a: Array[i64, 3], n: i64 }
enum H { W(Holder), N }

// Array payloads live on a NON-derived enum: `Array[T, N]` has no Display
// arm in codegen at any depth (B-2026-08-31-19), so a derived-Display enum
// carrying one panics the compiler before this row's reconstruction runs.
enum A { A3(Array[i64, 3]), N }

fn main() {
    let n: i64 = 1;
    let v4: Vector[i64, 4] = Vector[i64, 4](n, n + 1, n + 2, n + 3);
    let v8: Vector[i64, 8] = Vector[i64, 8](n, n+1, n+2, n+3, n+4, n+5, n+6, n+7);
    let v32: Vector[i32, 4] = Vector[i32, 4](n as i32, (n+1) as i32, (n+2) as i32, (n+3) as i32);
    let vf: Vector[f64, 2] = Vector[f64, 2](n as f64 + 0.5, n as f64 + 1.5);
    let vh: Vector[f16, 2] = Vector[f16, 2]((n as f32 + 1.5f32) as f16, (n as f32 + 2.5f32) as f16);
    let vu: Vector[u8, 4] = Vector[u8, 4](n as u8, (n + 1) as u8, (n + 2) as u8, (n + 3) as u8);
    let a3: Array[i64, 3] = [n, n + 1, n + 2];

    let d4 = E.V4(v4);
    println(f"d4  {d4}");
    let d8 = E.V8(v8);
    println(f"d8  {d8}");
    let d32 = E.V32(v32);
    println(f"d32 {d32}");
    let df = E.Vf(vf);
    println(f"df  {df}");

    let dh = E.Vh(vh);
    println(f"dh  {dh}");
    let du = E.Vu(vu);
    println(f"du  {du}");

    match d4 { E.V4(w) => { println(f"m4  {w}"); } E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match d8 { E.V8(w) => { println(f"m8  {w}"); } E.V4(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match d32 { E.V32(w) => { println(f"m32 {w}"); } E.V4(w) => {} E.V8(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match df { E.Vf(w) => { println(f"mf  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match dh { E.Vh(w) => { println(f"mh  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vu(w) => {} E.N => {} }
    match du { E.Vu(w) => { println(f"mu  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.N => {} }

    let da = A.A3(a3);
    match da { A.A3(b) => { println(f"ma  {b[0]} {b[1]} {b[2]}"); } A.N => {} }

    let o4: Option[Vector[i64, 4]] = Some(v4);
    match o4 { Some(w) => { println(f"o4  {w}"); } None => {} }
    let o8: Option[Vector[i64, 8]] = Some(v8);
    match o8 { Some(w) => { println(f"o8  {w}"); } None => {} }
    let oa: Option[Array[i64, 3]] = Some(a3);
    match oa { Some(b) => { println(f"oa  {b[0]} {b[2]}"); } None => {} }
    let r4: Result[Vector[i64, 4], i64] = Ok(v4);
    match r4 { Ok(w) => { println(f"r4  {w}"); } Err(x) => { println(f"re  {x}"); } }

    let h = H.W(Holder { v: v4, a: a3, n: n });
    match h { H.W(x) => { println(f"hw  {x.v} {x.a[0]} {x.a[2]} {x.n}"); } H.N => {} }
}
"#),
        r#"d4  V4(Vector(1, 2, 3, 4))
d8  V8(Vector(1, 2, 3, 4, 5, 6, 7, 8))
d32 V32(Vector(1, 2, 3, 4))
df  Vf(Vector(1.5, 2.5))
dh  Vh(Vector(2.5, 3.5))
du  Vu(Vector(1, 2, 3, 4))
m4  Vector(1, 2, 3, 4)
m8  Vector(1, 2, 3, 4, 5, 6, 7, 8)
m32 Vector(1, 2, 3, 4)
mf  Vector(1.5, 2.5)
mh  Vector(2.5, 3.5)
mu  Vector(1, 2, 3, 4)
ma  1 2 3
o4  Vector(1, 2, 3, 4)
o8  Vector(1, 2, 3, 4, 5, 6, 7, 8)
oa  1 3
r4  Vector(1, 2, 3, 4)
hw  Vector(1, 2, 3, 4) 1 3 1
"#
    );
}

/// B-2026-08-01-3 residual (pass-roundtrip, closed) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_roundtrip_reassign_single_nll_fire`, same
/// source and expected string. The interpreter fires the bodies exactly
/// once at the binding's NLL last-use statement and manages memory in Rust
/// — this pin is the parity target the codegen memory-only eager free must
/// not disturb (the AOT leak itself is gated by the LSan suite).
#[test]
fn test_roundtrip_reassign_single_nll_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
                 fn drop(mut ref self) {\n\
                     println(\"loud drop\")\n\
                 }\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
                 return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn pass(b: Loud) -> Loud {\n\
                 return b;\n\
             }\n\
             fn mk_res(n: i64) -> Res {\n\
                 return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn pass_res(b: Res) -> Res {\n\
                 return b;\n\
             }\n\
             fn main() {\n\
                 let mut e = mk_loud(7);\n\
                 println(\"a\");\n\
                 e = pass(e);\n\
                 println(\"b\");\n\
                 let mut s = mk_res(4);\n\
                 s = pass_res(s);\n\
                 println(s.name);\n\
                 println(\"end\");\n\
             }\n"),
        "a\nloud drop\ndrop 7 l7\nb\nr4\ndrop 4 r4\nend\n"
    );
}

/// B-2026-08-14-6 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_int_to_float_widening_reaches_container_and_probe`, same source
/// and expected string.
///
/// Unlike most of this family the interpreter was NOT the oracle here: it
/// stored an `Int` in a `Vec[f64]`, so `contains(200.0)` was false for a Vec it
/// had just been pushed a 200. It needed a typechecker-recorded span set to fix
/// — it keeps no declared element type of its own — which is why the twin
/// matters: the two halves were fixed by different mechanisms and could drift.
#[test]
fn test_int_to_float_widening_reaches_container_and_probe() {
    assert_eq!(
        run("struct H { mut f: f64 }\n\
             fn main() {\n\
                 let v = 200u8;\n\
                 let n = -5i8;\n\
                 let mut vc: Vec[f64] = Vec.new();\n\
                 vc.push(v);\n\
                 println(vc.contains(200.0));\n\
                 println(vc.contains(v));\n\
                 let mut vd: Vec[f64] = Vec.new();\n\
                 vd.push(200.0);\n\
                 println(vd.contains(v));\n\
                 let mut ve: Vec[f64] = Vec.new();\n\
                 ve.push(0.0);\n\
                 ve[0i64] = v;\n\
                 println(ve.contains(200.0));\n\
                 let mut arr: Array[f64, 2] = [0.0, 0.0];\n\
                 arr[0i64] = v;\n\
                 println(arr[0i64]);\n\
                 let mut x: f64 = 0.0;\n\
                 x = v;\n\
                 println(x == 200.0);\n\
                 let mut vn: Vec[f64] = Vec.new();\n\
                 vn.push(n);\n\
                 println(vn.contains(-5.0));\n\
                 let mut y: f64 = 0.0;\n\
                 y = n;\n\
                 println(y == -5.0);\n\
                 let mut vf: Vec[f64] = Vec.new();\n\
                 vf.push(1.5);\n\
                 println(vf.contains(1.5));\n\
                 let h = H { f: 0.0 };\n\
                 println(h.f == 0.0);\n\
             }"),
        "true\ntrue\ntrue\ntrue\n200\ntrue\ntrue\ntrue\ntrue\ntrue\n"
    );
}

/// B-2026-08-13-18 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_implicit_int_to_float_widening_at_every_boundary`, same source
/// and expected string.
///
/// The interpreter is the oracle here: it ran all of these correctly the whole
/// time, which is what made the compiled behaviour a divergence rather than a
/// language question. Pinning the twin keeps the two surfaces from drifting —
/// three of these positions failed `karac build` outright and two more were
/// silent bit reinterpretations.
#[test]
fn test_implicit_int_to_float_widening_at_every_boundary() {
    assert_eq!(
        run("struct W { mut a: i64, mut f: f64 }\n\
             enum E { F(f64) }\n\
             struct M { mut f: f64, mut n: i64 }\n\
             impl M {\n\
                 fn setf(mut ref self, x: f64) { self.f = x; }\n\
                 fn setn(mut ref self, x: i64) { self.n = x; }\n\
             }\n\
             fn takesf(x: f64) -> f64 { x }\n\
             fn retf(b: u8) -> f64 { return b; }\n\
             fn main() {\n\
                 let b: u8 = 200u8;\n\
                 let s: i8 = -5i8;\n\
                 let w = W { a: 0i64, f: b };\n\
                 println(w.f);\n\
                 println(takesf(b));\n\
                 println(retf(b));\n\
                 let mut q = W { a: 0i64, f: 0.0 };\n\
                 q.f = b;\n\
                 println(q.f);\n\
                 let mut m = M { f: 0.0, n: 0 };\n\
                 m.setf(b);\n\
                 m.setn(b);\n\
                 println(m.f);\n\
                 println(m.n);\n\
                 let e = E.F(b);\n\
                 match e { F(x) => { println(x); } }\n\
                 let mut mp: Map[i64, f64] = Map.new();\n\
                 let _ = mp.insert(1, b);\n\
                 mp[2] = b;\n\
                 match mp.get(1) { Some(x) => { println(x); } None => { println(0.0); } }\n\
                 match mp.get(2) { Some(x) => { println(x); } None => { println(0.0); } }\n\
                 println(takesf(s));\n\
                 let ws = W { a: 0i64, f: s };\n\
                 println(ws.f);\n\
             }"),
        "200\n200\n200\n200\n200\n200\n200\n200\n200\n-5\n-5\n"
    );
}

/// B-2026-08-14-11 — an UNSUFFIXED float literal takes its width from the
/// DESTINATION, so it is the same value as the suffixed spelling.
///
/// Synthesis types a bare literal `f64` and nothing moved it, so the literal's
/// own span said `f64` while it sat in a narrow-float slot. The interpreter
/// reads that span, so it kept the full double at EVERY such position, and
/// codegen narrowed at all but the annotated `let` — which is how
/// `let a: f32 = 0.1; let b: f32 = 0.1f32; a == b` came to answer `false` under
/// `--interp` and `true` compiled, on a program containing no arithmetic.
///
/// Seven positions, all the ones a bare literal can reach, plus `f16` to show
/// the rule is about the declared width rather than about f32. Each prints the
/// SUFFIXED spelling's value; pre-fix the interpreter printed `0.1` on all
/// seven.
///
/// B-2026-08-31-20 added six more, and they are the positions "all the ones a
/// bare literal can reach" missed: an `Option` / `Result` PAYLOAD in all three
/// constructor spellings (`Option.Some`, bare `Some`, `Ok` / `Err`), a `Vec`
/// literal element, and a PREFIX `Array[...]` element. The width has to travel
/// through the constructor call or the collection literal to reach the literal,
/// and `record_narrow_float_literal` recursed through tuples only.
///
/// The `v.push(0.1)` line above is why this looked covered: `Vec` elements DO
/// narrow through the method spelling, which has its own recording site, so a
/// reader checking "do Vec elements narrow?" got yes — while `[0.1]`, the same
/// store written as a literal, did not. Two spellings, one of them tested.
#[test]
fn test_unsuffixed_float_literal_takes_the_destination_width() {
    assert_eq!(
        run("struct Bx { f: f32 }\n\
             fn takef(x: f32) -> f32 { x }\n\
             fn retf() -> f32 { 0.1 }\n\
             fn main() {\n\
                 let la: f32 = 0.1;\n\
                 println(la);\n\
                 let sl = Bx { f: 0.1 };\n\
                 println(sl.f);\n\
                 println(takef(0.1));\n\
                 println(retf());\n\
                 let mut v: Vec[f32] = Vec.new();\n\
                 v.push(0.1);\n\
                 println(v[0]);\n\
                 let ar: Array[f32, 1] = [0.1];\n\
                 println(ar[0]);\n\
                 let tu: (f32, f32) = (0.1, 0.2);\n\
                 println(tu.0);\n\
                 let po: Option[f32] = Option.Some(0.1);\n\
                 println(po);\n\
                 let pb: Option[f32] = Some(0.1);\n\
                 println(pb);\n\
                 let pk: Result[f32, i64] = Ok(0.1);\n\
                 println(pk);\n\
                 let pe: Result[i64, f32] = Err(0.1);\n\
                 println(pe);\n\
                 let vl: Vec[f32] = [0.1];\n\
                 println(vl[0]);\n\
                 let ap: Array[f32, 1] = Array[0.1];\n\
                 println(ap[0]);\n\
                 let h: f16 = 0.1;\n\
                 println(h);\n\
             }"),
        "0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         Some(0.10000000149011612)\n\
         Some(0.10000000149011612)\n\
         Ok(0.10000000149011612)\n\
         Err(0.10000000149011612)\n\
         0.10000000149011612\n\
         0.10000000149011612\n\
         0.0999755859375\n"
    );
}

/// B-2026-08-14-11, the other direction: an `f64` destination and a SUFFIXED
/// literal are both left exactly as they were.
///
/// The re-record is narrow-floats-only, so an `f64` slot keeps the full double;
/// and a suffix is the author naming the width, so it wins over the
/// destination — `let w: f64 = 0.1f32` holds the f32 value widened, not `0.1`.
/// This passes before the fix as well: it is an over-reach guard, not a
/// regression witness.
#[test]
fn test_float_literal_width_respects_f64_slots_and_suffixes() {
    assert_eq!(
        run("fn main() {\n\
                 let a: f64 = 0.1;\n\
                 println(a);\n\
                 let w: f64 = 0.1f32;\n\
                 println(w);\n\
                 let b = 0.1;\n\
                 println(b);\n\
             }"),
        "0.1\n0.10000000149011612\n0.1\n"
    );
}

/// B-2026-08-14-13 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_mixed_float_arithmetic_as_cast_computes_at_the_stated_width`,
/// same source and same expected string.
///
/// An OVER-REACH GUARD, not a regression witness: every line is explicitly
/// cast, so it ran to these values before the gate too. This surface is the one
/// that made the old behaviour a run-vs-build split rather than merely an
/// order-dependence — `b * a` with `b: f64`, `a: f32` took `f64`, and the
/// interpreter KEPT the double where the binary rounded to f32 — so it is worth
/// pinning that with the width named in the source the two backends agree, and
/// that the two spellings still give different answers, because the width is a
/// real choice and not a formality. The rejection of the uncast spelling is
/// asserted in `tests/typechecker.rs::mixed_width_float_arithmetic_is_rejected`.
#[test]
fn test_mixed_float_arithmetic_as_cast_computes_at_the_stated_width() {
    assert_eq!(
        run("fn main() {\n\
                 let a: f32 = 1.1f32;\n\
                 let b: f64 = 1.1;\n\
                 let wide: f64 = (a as f64) * b;\n\
                 println(wide);\n\
                 let narrow: f32 = a * (b as f32);\n\
                 println(narrow);\n\
                 let h: f16 = 1.5f16;\n\
                 let up: f32 = (h as f32) * a;\n\
                 println(up);\n\
             }"),
        "1.2100000262260437\n1.2100000381469727\n1.6500000953674316\n"
    );
}

/// B-2026-08-14-31 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_print_a_map_or_set_place_expression`, same source
/// and same expected string.
///
/// This surface rendered every one of these correctly the whole time; the
/// compiled backends printed pointers. So this test is the ORACLE the compiled
/// twin is checked against — the two assert one string, so a codegen fix that
/// stopped printing addresses but printed something else would fail the pair
/// rather than quietly redefine what printing a Map means.
#[test]
fn test_print_a_map_or_set_place_expression() {
    assert_eq!(
        run("struct B { m: Map[String, i64], s: Set[String] }\n\
             fn mkm() -> Map[String, i64] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); m }\n\
             fn mks() -> Set[String] { let mut s: Set[String] = Set.new(); s.insert(\"e\"); s }\n\
             fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"k\", 1);\n\
                 let mut st: Set[String] = Set.new();\n\
                 st.insert(\"e\");\n\
                 let b = B { m: m, s: st };\n\
                 println(f\"{b.m}\");\n\
                 println(b.m);\n\
                 println(f\"{b.s}\");\n\
                 println(b.s);\n\
                 println(f\"{mkm()}\");\n\
                 println(f\"{mks()}\");\n\
                 let mut m2: Map[String, i64] = Map.new();\n\
                 m2.insert(\"k\", 1);\n\
                 let t = (m2, 1);\n\
                 println(f\"{t.0}\");\n\
                 let mut m3: Map[String, i64] = Map.new();\n\
                 m3.insert(\"k\", 1);\n\
                 let v: Vec[Map[String, i64]] = [m3];\n\
                 println(f\"{v[0]}\");\n\
                 println(f\"{b.m}\");\n\
             }"),
        "{k: 1}\n{k: 1}\nSet{e}\nSet{e}\n{k: 1}\nSet{e}\n{k: 1}\n{k: 1}\n{k: 1}\n"
    );
}

/// B-2026-08-14-30 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_print_a_vec_place_expression`, same source and
/// same expected string.
///
/// This surface printed every one of these correctly the whole time — the bug
/// was compiled-only, a double free and then a SEGV on a shape as ordinary as
/// printing a struct field. So this test's job is to be the ORACLE the compiled
/// twin is checked against: the two assert one string, so a codegen fix that
/// stopped crashing but printed something else would fail the pair rather than
/// quietly redefine what printing a Vec field means.
#[test]
fn test_print_a_vec_place_expression() {
    assert_eq!(
        run(
            "struct B { xs: Vec[String], ns: Vec[i64], nested: Vec[Vec[i64]] }\n\
             shared struct S { xs: Vec[String] }\n\
             fn mk() -> Vec[String] { [\"x\", \"y\"] }\n\
             fn main() {\n\
                 let b = B { xs: [\"a\", \"b\"], ns: [1, 2, 3], nested: [[1, 2], [3]] };\n\
                 let s = S { xs: [\"p\", \"q\"] };\n\
                 println(b.xs);\n\
                 println(f\"{b.xs}\");\n\
                 println(b.ns);\n\
                 println(f\"{b.nested}\");\n\
                 println(f\"{b.nested[0]}\");\n\
                 println(f\"{s.xs}\");\n\
                 println([9, 8]);\n\
                 println(mk());\n\
                 println(f\"{b.xs}\");\n\
             }"
        ),
        "[a, b]\n[a, b]\n[1, 2, 3]\n[[1, 2], [3]]\n[1, 2]\n[p, q]\n[9, 8]\n[x, y]\n[a, b]\n"
    );
}

/// B-2026-08-14-19 — the interpreter half of the `String.substring` boundary
/// fault, twin of `tests/codegen.rs::test_e2e_substring_non_codepoint_boundary_panics`.
///
/// This surface used to run a LOSSY conversion over the byte range, so it
/// answered where the compiled backends answered differently: `"日本語".substring(0, 2)`
/// measured `3 3 1` here (one U+FFFD) against `2 2 2` compiled. Both were
/// defensible readings of a mid-codepoint cut and that is exactly the problem —
/// they have different LENGTHS, and length is what the next line branches on.
///
/// Rejecting rather than picking one garbage is the design.md answer twice
/// over: `String` is UTF-8 encoded, so raw bytes are not representable in one;
/// and the overflow rule already settled that a silent plausible-looking wrong
/// value loses to a loud failure the Mend loop can act on.
#[test]
fn test_substring_non_codepoint_boundary_faults() {
    let errors = runtime_errors(
        "fn main() { let s = \"\u{65e5}\u{672c}\u{8a9e}\"; println(s.substring(0i64, 2i64).len()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not a UTF-8 codepoint boundary")),
        "expected a boundary fault, got: {errors:?}"
    );
    // A START that is off-boundary faults too, and so does an EMPTY range at a
    // bad index — the index is just as invalid whether or not bytes come back.
    let errors = runtime_errors(
        "fn main() { let s = \"\u{65e5}\u{672c}\u{8a9e}\"; println(s.substring(1i64, 1i64).len()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("start byte index 1")),
        "expected a start-boundary fault, got: {errors:?}"
    );
}

/// B-2026-08-14-2 — an int at a FLOAT-declared destination is converted, at
/// every position whose declared type the interpreter can reach.
///
/// Kāra's widening coercions are implicit (`check_int_widening_coercion`
/// rejects only narrowing), so an int may legally appear wherever a float is
/// declared, with no `as` in the source. Codegen converts at every such
/// boundary (B-2026-08-13-18); the interpreter converted at NONE, so the int
/// stayed an int in a float slot — and because the operator dispatch has no
/// mixed Int/Float arm, the program did not answer wrong, it ABORTED. On source
/// `karac check` passes and `karac build` runs correctly, with a runtime
/// message asserting a typecheck error that does not exist.
///
/// Every line is `true`, and every one of them aborted before the fix. They are
/// in ONE program on purpose: each abort kills the run, so a reader who breaks
/// the first position sees it immediately rather than a subtly wrong number.
///
/// The comparison against a float literal is what makes the conversion
/// observable at all — printing `Int(200)` and `Float(200.0)` both render
/// "200", which is why several of these positions look fine under any
/// print-based oracle and were latently broken for as long as they were.
#[test]
fn test_int_at_a_float_destination_is_converted() {
    assert_eq!(
        run("struct Bx { mut f: f64 }\n\
             fn takef(x: f64) -> bool { x == 200.0 }\n\
             fn retf(v: u8) -> f64 { v }\n\
             impl Bx { fn echo(self, x: f64) -> bool { x == 200.0 } }\n\
             fn main() {\n\
                 let v = 200u8;\n\
                 let la: f64 = v;\n\
                 println(la == 200.0);\n\
                 println(takef(v));\n\
                 println(retf(v) == 200.0);\n\
                 let sl = Bx { f: v };\n\
                 println(sl.f == 200.0);\n\
                 let mut fa = Bx { f: 0.0 };\n\
                 fa.f = v;\n\
                 println(fa.f == 200.0);\n\
                 let mb = Bx { f: 0.0 };\n\
                 println(mb.echo(v));\n\
                 let ta: (f64, f64) = (v, v);\n\
                 println(ta.0 == 200.0);\n\
                 let ae: Array[f64, 2] = [v, v];\n\
                 println(ae[0] == 200.0);\n\
             }"),
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n"
    );
}

/// B-2026-08-14-2, the precision half: the conversion goes through the
/// destination's real storage width, not straight to `f64`.
///
/// `Value::Float` is an f64, so a naive `i as f64` would let an `f32` slot hold
/// a value it cannot represent — `2147483647i32` would read back exactly here
/// and as `2147483648` on every compiled surface, trading one divergence for
/// another. Routing through the existing `cast_value` table is what makes the
/// `f32` / `f16` / `bf16` slots round the way their storage does.
#[test]
fn test_int_to_float_conversion_rounds_at_the_declared_width() {
    assert_eq!(
        run("fn main() {\n\
                 let big = 2147483647i32;\n\
                 let wide: f64 = big;\n\
                 let narrow: f32 = big;\n\
                 println(wide);\n\
                 println(narrow);\n\
             }"),
        "2147483647\n2147483648\n"
    );
}

/// B-2026-07-31-35 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_deque_head_read_group_lane_consistency`, same source and expected
/// string. The interpreter is the oracle for what `q.len()` must report
/// after 50 pops, whatever lane auto-par picks under codegen.
#[test]
fn test_deque_head_read_group_lane_consistency() {
    assert_eq!(
        run("fn main() {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 let mut i = 0;\n\
                 while i < 100 {\n\
                     q.push_back(i);\n\
                     i = i + 1;\n\
                 }\n\
                 let mut drained = 0;\n\
                 while drained < 50 {\n\
                     match q.pop_front() {\n\
                         Some(x) => {\n\
                             drained = drained + 1;\n\
                             if x > 1000 { println(x); }\n\
                         }\n\
                         None => {}\n\
                     }\n\
                 }\n\
                 let a = q.len();\n\
                 let b = q.len();\n\
                 println(a);\n\
                 println(b);\n\
                 println(drained);\n\
             }\n"),
        "50\n50\n50\n"
    );
}

// ── The i128 value carrier (B-2026-08-19-8 stage 1) ──────────────

/// Every integer width still traps on overflow after `Value::Int` widened from
/// `i64` to `i128`.
///
/// THIS IS THE TEST THE WIDENING EXISTS FOR. While the carrier was an i64, a
/// 64-bit overflow was caught by the CARRIER itself — `checked_add` on the
/// carrier returned `None` — and `narrow_oob` only range-checked the narrow
/// widths (i8..i32, u8..u32), explicitly documented as "a no-op for
/// i64/u64/usize/isize". Widening the carrier silently removed that:
/// `i64::MAX + 1` simply fits in an i128. The width now comes from the
/// declared type for EVERY width, not just the narrow ones.
///
/// A 64-bit case belongs here for each arithmetic operator, because each has
/// its own arm and the old carrier covered all of them for free.
#[test]
fn i64_overflow_still_traps_with_the_i128_carrier() {
    for (expr, what) in [
        ("9223372036854775807i64 + 1i64", "add"),
        ("(-9223372036854775807i64 - 1i64) - 1i64", "sub"),
        ("9223372036854775807i64 * 2i64", "mul"),
    ] {
        let errors = runtime_errors(&format!("fn main() {{ let x: i64 = {expr}; println(x); }}"));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "expected an integer-overflow trap for {what} ({expr}), got: {:?}",
            errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }
}

/// The carrier is 128 bits wide, pinned at compile time.
///
/// Stages 2-5 of B-2026-08-19-8 build on this; a well-meaning "nothing needs
/// i128 yet, narrow it back to i64" would silently undo stage 1 and re-open
/// B-2026-08-19-6's class of defect. This fails to COMPILE if the carrier
/// narrows, which is the loudest available signal.
#[test]
fn value_int_carries_128_bits() {
    let v = karac::interpreter::Value::Int(i128::MAX);
    let karac::interpreter::Value::Int(n) = v else {
        panic!("expected Value::Int");
    };
    assert_eq!(n, i128::MAX);
}

#[test]
fn gpu_u32_reductions_use_the_unsigned_rules_throughout() {
    // Above 2^31 the unsigned reading differs from the signed one at every
    // step: `4294967295` is `-1` as i32, so a signed compare answers max/min
    // backwards, and a signed widen reports the result as negative.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "4294967295");

    // The OVERFLOW POINT differs too, and this is the case range-sniffing the
    // values could never get right: `2147483647 + 1` traps as i32 and is
    // perfectly ordinary as u32. Signedness has to come from the type, not
    // from the data.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[u32] = [2147483647, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert_eq!(out.trim(), "2147483648");

    // And u32 still traps at ITS OWN boundary — a carry, not a sign flip.
    let errs = runtime_errors(
        "fn main() {\n\
        \x20   let v: Vec[u32] = [4294967295, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert!(
        format!("{errs:?}").contains("integer overflow"),
        "u32 must trap on carry, got: {errs:?}"
    );
}

#[test]
fn gpu_integer_mean_promotes_and_rounds_exactly_once() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [1, 2];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "1.5", "promotes like Stats.mean, not truncates");

    // Promoting LATE is what buys the accuracy: both elements are above 2^24,
    // where an f32 promotion would quantise each one BEFORE the sum. The exact
    // integer sum widened to f64 gives the true mean.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [16777217, 16777219];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "16777218");

    // Empty has no mean — checked on the LENGTH, since a sum of 0 is a fine
    // answer for a non-empty buffer.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [];\n\
        \x20   let m = gpu.mean(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "empty");
}

#[test]
fn gpu_integer_reductions_trap_on_overflow() {
    // The rule: integer GPU reductions trap, exactly as `v.sum()` over a
    // `Vec[i32]` already does. Wrapping would turn a trap into a wrong answer
    // the moment a reduction moved to the GPU.
    let errs = runtime_errors(
        "fn main() {\n\
        \x20   let v: Vec[i32] = [2147483647, 1];\n\
        \x20   println(f\"{gpu.sum(v)}\")\n\
        }",
    );
    assert!(
        format!("{errs:?}").contains("integer overflow"),
        "expected an overflow trap, got: {errs:?}"
    );
}

/// A 128-bit scalar survives an enum-payload round trip in the INTERPRETER
/// (B-2026-08-19-19). The codegen twin is `e2e_128bit_enum_payload_round_trip`;
/// the pair is what makes `karac run` == `karac build` an assertion rather than
/// a hope, and the payload path is where the two backends had every reason to
/// disagree (codegen packs 64-bit words, the interpreter holds a whole i128).
#[test]
fn a_128bit_enum_payload_round_trips_in_the_interpreter() {
    assert_eq!(
        run_no_errors(
            "enum Box128 { W(i128), Pair(i128, i64), Nothing }\n\
             fn main() {\n\
             let a: Option[i128] = Some(1267650600228229401496703205376i128);\n\
             match a { Some(x) => println(x), None => println(\"none-a\") }\n\
             let d: Result[i128, String] = Ok(-1267650600228229401496703205376i128);\n\
             match d { Ok(x) => println(x), Err(e) => println(e) }\n\
             let g: Box128 = Box128.Pair(1267650600228229401496703205376i128, 42i64);\n\
             match g {\n\
             Box128.W(x) => println(x),\n\
             Box128.Pair(p, q) => { println(p) println(q) }\n\
             Box128.Nothing => println(\"nb\"),\n\
             }\n\
             let i: Option[i128] = None;\n\
             match i { Some(x) => println(x), None => println(\"none-i\") }\n\
             }"
        ),
        "1267650600228229401496703205376\n\
         -1267650600228229401496703205376\n\
         1267650600228229401496703205376\n\
         42\n\
         none-i\n"
    );
}

/// The overflow-aware families at 128 bits, in the interpreter
/// (B-2026-08-19-19). Two defects lived here, both invisible until the
/// type-check rejection was lifted:
///
///   - `saturating_*` clamped by OPERATION (`sub` → MIN, else MAX), so an
///     overflowing NEGATIVE product saturated to `i128::MAX`. The rule is the
///     sign of the true result, which is what codegen already did.
///   - every UNSIGNED 128-bit case fell through to a generic tail whose
///     `(a as u64) as i128` truncates the operand to 64 bits, so
///     `(2^100 as u128).checked_mul(2)` answered `0` here and `2^101` compiled.
#[test]
fn the_128bit_overflow_families_are_width_correct_in_the_interpreter() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let neg: i128 = -1267650600228229401496703205376i128;\n\
             println(neg.saturating_mul(1000000000000i128));\n\
             let mx: i128 = 170141183460469231731687303715884105727i128;\n\
             println(mx.saturating_add(1i128));\n\
             match mx.checked_add(1i128) { Some(v) => println(v), None => println(\"ovf\") }\n\
             let u: u128 = 1267650600228229401496703205376u128;\n\
             match u.checked_mul(2u128) { Some(v) => println(v), None => println(\"ovf\") }\n\
             println(u.saturating_sub(1u128));\n\
             println(u.wrapping_mul(3u128));\n\
             let t = u.overflowing_add(1u128);\n\
             println(t.0);\n\
             }"
        ),
        "-170141183460469231731687303715884105728\n\
         170141183460469231731687303715884105727\n\
         ovf\n\
         2535301200456458802993406410752\n\
         1267650600228229401496703205375\n\
         3802951800684688204490109616128\n\
         1267650600228229401496703205377\n"
    );
}

/// The UPPER HALF of `u128` — every value past `i128::MAX` — works end to end
/// (B-2026-08-19-23). `Value::Int`'s carrier is a signed `i128`, so those
/// values ride as NEGATIVE bit patterns exactly as a `u64` past `i64::MAX`
/// does; the difference was that every consumer of the "is this unsigned"
/// predicate read the carrier back at 64 bits, because that predicate was a
/// bool from a `u64`-only era. Reading a `u128` at 64 bits keeps the low half.
///
/// Every literal here is above `i128::MAX`, so a signed reading is not merely
/// imprecise, it changes the answer's SIGN — `u128::MAX` reads as `-1`.
#[test]
fn the_upper_half_of_u128_is_correct_in_the_interpreter() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let m: u128 = 340282366920938463463374607431768211455u128;\n\
             let h: u128 = 200000000000000000000000000000000000000u128;\n\
             println(m);\n\
             println(f\"{m}\");\n\
             println(m.to_string());\n\
             println(h > 5u128);\n\
             println(h < 5u128);\n\
             println(m >= h);\n\
             println(h / 3u128);\n\
             println(h % 7u128);\n\
             println(m / 2u128);\n\
             println(m >> 100u32);\n\
             println(h - 5u128);\n\
             println(m.count_ones());\n\
             }"
        ),
        "340282366920938463463374607431768211455\n\
         340282366920938463463374607431768211455\n\
         340282366920938463463374607431768211455\n\
         true\n\
         false\n\
         true\n\
         66666666666666666666666666666666666666\n\
         4\n\
         170141183460469231731687303715884105727\n\
         268435455\n\
         199999999999999999999999999999999999995\n\
         128\n"
    );
}

/// `Vec[u128].sort()` orders by MAGNITUDE (B-2026-08-19-23). The comparator is
/// picked from the element type at the close-paren leaf; that selection was a
/// bool, so a `u128` element got the signed comparator and everything past
/// `i128::MAX` sorted ahead of the small values. The `u64` half is asserted
/// alongside it because the same selector serves both and must not regress.
#[test]
fn a_u128_vec_sorts_by_magnitude() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let mut a: Vec[u64] = vec![];\n\
             a.push(18446744073709551615u64);\n\
             a.push(5u64);\n\
             a.sort();\n\
             println(a[0]);\n\
             let mut b: Vec[u128] = vec![];\n\
             b.push(340282366920938463463374607431768211455u128);\n\
             b.push(200000000000000000000000000000000000000u128);\n\
             b.push(5u128);\n\
             b.sort();\n\
             println(b[0]);\n\
             println(b[1]);\n\
             println(b[2]);\n\
             }"
        ),
        "5\n\
         5\n\
         200000000000000000000000000000000000000\n\
         340282366920938463463374607431768211455\n"
    );
}

/// A shift amount up to the operand's own width is legal, and the width is the
/// VALUE's — not 64, and not the amount's (B-2026-08-19-23 / B-2026-08-19-26).
/// `span_int_width` had no 128-bit arms, so `1i128 << 100` was rejected as
/// "shift amount out of range" against a 64-bit bound. The 64-bit cases pin the
/// widths that already worked; the codegen twin is where the interesting half
/// of this lives, because there the VALUE was being truncated.
#[test]
fn shifts_run_at_the_values_own_width() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let a: i128 = 1i128;\n\
             println(a << 100u32);\n\
             let b: i128 = 1267650600228229401496703205376i128;\n\
             println(b >> 100u32);\n\
             let c: i64 = 1099511627776i64;\n\
             println(c << 1u32);\n\
             println(c >> 1u32);\n\
             let d: u64 = 18446744073709551615u64;\n\
             println(d >> 32u32);\n\
             }"
        ),
        "1267650600228229401496703205376\n\
         1\n\
         2199023255552\n\
         549755813888\n\
         4294967295\n"
    );
}

/// The same for `u128`, whose top half sits past `i128::MAX`, plus a struct
/// field and a `Map` whose VALUE is itself a container — the deepest nesting
/// the peel has to survive (B-2026-08-19-27).
#[test]
fn a_nested_u128_renders_at_its_own_width() {
    assert_eq!(
        run_no_errors(
            "#[derive(Display)]\n\
             struct Pair { pub u: u128, pub v: u64 }\n\
             fn main() {\n\
             let m: u128 = 340282366920938463463374607431768211455u128;\n\
             let big: u64 = 18446744073709551615u64;\n\
             let o: Option[u128] = Some(m);\n\
             println(o);\n\
             let mut v: Vec[u128] = vec![];\n\
             v.push(m);\n\
             v.push(5u128);\n\
             println(v);\n\
             let p = Pair { u: m, v: big };\n\
             println(p);\n\
             let mut vv: Vec[Option[u128]] = vec![];\n\
             vv.push(Some(m));\n\
             println(vv);\n\
             let mut mp: Map[String, Vec[u64]] = Map.new();\n\
             let mut inner: Vec[u64] = vec![];\n\
             inner.push(big);\n\
             mp.insert(\"k\", inner);\n\
             println(mp);\n\
             }"
        ),
        "Some(340282366920938463463374607431768211455)\n\
         [340282366920938463463374607431768211455, 5]\n\
         Pair { u: 340282366920938463463374607431768211455, v: 18446744073709551615 }\n\
         [Some(340282366920938463463374607431768211455)]\n\
         {k: [18446744073709551615]}\n"
    );
}

#[test]
fn is_sorted_reads_a_u64_element_as_unsigned() {
    // The element type reaches the interpreter through the close-paren stash
    // the sort family uses. Without it these order as negative two's-complement
    // i64 and the answers invert — and `is_sorted` would then contradict the
    // `sort()` two lines above it.
    let out = run("fn main() {\n\
        let v: Vec[u64] = [1u64, 18446744073709551615u64];\n\
        let w: Vec[u64] = [18446744073709551615u64, 1u64];\n\
        println(f\"{v.is_sorted()} {w.is_sorted()}\");\n\
        let mut s: Vec[u64] = [9223372036854775809u64, 3u64, 1u64];\n\
        s.sort();\n\
        println(s.is_sorted());\n\
    }");
    assert_eq!(out, "true false\ntrue\n");
}

#[test]
fn enum_try_from_round_trips_through_discriminant() {
    // The two halves of the surface are inverses on every declared value.
    let out = run("#[repr(u8)]\n\
    enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }\n\
    fn main() {\n\
        match UsbClass.try_from(UsbClass.MassStorage.discriminant()) {\n\
            Ok(c) => println(c.discriminant()),\n\
            Err(_) => println(999),\n\
        }\n\
    }");
    assert_eq!(out, "8\n");
}

// ── `Vector[T, N]` integer lane arithmetic WRAPS (B-2026-08-26-8) ──────────
//
// The one deliberate exception to design.md § Integer overflow's trap rule.
// Pre-fix the interpreter did NEITHER: it computed on the i128 carrier, so a
// `u8` lane sum of 200 + 200 was 400 — wrong under wrap (144) AND under trap
// (`integer overflow`), and divergent from codegen, whose lanes are a real
// `<N x iX>`. See `docs/design.md § Portable SIMD` for why `Vector` departs
// from `Column` / `Tensor`, which trap at the element width in both backends.

#[test]
fn vector_unsigned_lane_add_wraps_at_the_lane_width() {
    assert_eq!(
        run("fn main() {\n\
                 let v: Vector[u8, 4] = Vector[u8, 4].splat(200);\n\
                 let w = v + v;\n\
                 println(w[0] as i64);\n\
             }\n"),
        "144\n"
    );
}

/// Signed lanes wrap in two's complement, not toward zero and not saturating.
/// `100 + 100` is `200`, which is `-56` read back as `i8`.
#[test]
fn vector_signed_lane_arithmetic_wraps_in_twos_complement() {
    assert_eq!(
        run("fn main() {\n\
                 let a: Vector[i8, 4] = Vector[i8, 4].splat(100);\n\
                 println((a + a)[0] as i64);\n\
                 let b: Vector[i16, 4] = Vector[i16, 4].splat(300);\n\
                 println((b * b)[0] as i64);\n\
             }\n"),
        "-56\n24464\n"
    );
}

/// Unsigned underflow wraps around the bottom of the lane rather than trapping
/// or clamping at 0 — `5 - 10` on a `u8` lane is `251`.
#[test]
fn vector_unsigned_lane_underflow_wraps_rather_than_trapping() {
    assert_eq!(
        run("fn main() {\n\
                 let c: Vector[u8, 4] = Vector[u8, 4].splat(5);\n\
                 let d: Vector[u8, 4] = Vector[u8, 4].splat(10);\n\
                 println((c - d)[0] as i64);\n\
             }\n"),
        "251\n"
    );
}

/// A WIDE `N` — past what any native vector unit covers, so codegen legalizes
/// it into several narrower vectors — must wrap identically. Pinned because
/// design.md promises "the user's source program is identical across targets —
/// performance, not correctness, is what varies", and a lane rule that held
/// only at machine-native widths would break exactly that.
#[test]
fn vector_lane_wrap_is_independent_of_the_lane_count() {
    assert_eq!(
        run("fn main() {\n\
                 let v: Vector[u8, 64] = Vector[u8, 64].splat(200);\n\
                 println((v + v)[0] as i64);\n\
             }\n"),
        "144\n"
    );
}

/// THE GUARD ON THE EXCEPTION. Making `Vector` wrap must not have widened into
/// the trapping widths it sits beside — `narrow_oob` is still the path for
/// every one of them. `Column[T]` element ops and plain scalars are the two
/// nearest neighbours, and both must still trap.
#[test]
fn wrapping_vector_lanes_did_not_stop_columns_or_scalars_trapping() {
    for (label, src) in [
        (
            "Column[i32] element op",
            "fn main() {\n\
                 let c: Column[i32] = Column.from_vec([2147483647]);\n\
                 let d = c + 1;\n\
                 match d[0] { Some(x) => { println(x); }, None => { println(0); } }\n\
             }\n",
        ),
        (
            "scalar i32",
            "fn main() { let x: i32 = 2147483647; let y = x + 1; println(y); }\n",
        ),
    ] {
        let errors = runtime_errors(src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("integer overflow")),
            "{label} must still trap at its declared width; got {errors:?}"
        );
    }
}

// ── B-2026-08-26-22: the fallible-allocation remainder ──────────

/// `String.reserve` is a pure hint — no `String.capacity()` exists to observe
/// it, deliberately, because `Value::String` is clone-on-write and would
/// discard the reservation on the next mutation while codegen's
/// `{ptr,len,cap}` keeps it. What both backends owe is that the CONTENT is
/// unchanged, which is what this asserts.
#[test]
fn string_reserve_is_a_hint_that_leaves_the_content_alone() {
    let out = run(r#"
fn main() {
    let mut s: String = String.new();
    s.push_str("hello");
    s.reserve(1000);
    println(f"[{s}] {s.len()}");
    s.push_str(" world");
    s.reserve(-5);
    s.reserve(0);
    println(f"[{s}] {s.len()}");
}
"#);
    assert_eq!(out, "[hello] 5\n[hello world] 11\n");
}

#[test]
fn interp_mixed_int_float_branch_arms_are_the_codegen_oracle() {
    // B-2026-08-30-49 — the oracle half. The defect was codegen-only: every
    // one of these was ALREADY correct under `--interp`, which is what made
    // the interpreter the reference the compiled backends were fixed against.
    //
    // So this test could not fail before the fix and is not a regression test
    // for it. It exists because the fix's correctness argument rests entirely
    // on "the interpreter is right here" — if that ever stops being true, the
    // codegen twin in `tests/codegen.rs`
    // (`e2e_mixed_int_float_branch_arms_convert_rather_than_zero`) would be
    // asserting agreement with a moved reference and would not notice. The
    // case table below is COPIED FROM that twin verbatim, so the two cannot
    // disagree about which shapes or which expected strings are at issue.
    let cases: &[(&str, &str, &str)] = &[
        (
            "if-int-then",
            "fn main() { let n: i64 = 7; let a: f64 = if true { n } else { 0.0 }; println(a); }",
            "7\n",
        ),
        (
            "if-int-else",
            "fn main() { let n: i64 = 7; let a: f64 = if false { 0.0 } else { n }; println(a); }",
            "7\n",
        ),
        (
            "match-int-arm",
            "fn main() { let n: i64 = 7; let a: f64 = match 1 { 1 => n, _ => 0.0 }; println(a); }",
            "7\n",
        ),
        (
            "match-float-first",
            "fn main() { let n: i64 = 7; let a: f64 = match 1 { 0 => 0.0, _ => n }; println(a); }",
            "7\n",
        ),
        (
            "f32-annotation",
            "fn main() { let n: i64 = 7; let a: f32 = if true { n } else { 0.0 }; println(a); }",
            "7\n",
        ),
        (
            "unsigned-above-i64max",
            "fn main() { let u: u64 = 18446744073709551615u64; let a: f64 = if true { u } else { 0.0 }; println(a); }",
            "18446744073709552000\n",
        ),
        (
            "negative-int-arm",
            "fn main() { let n: i64 = -3; let a: f64 = if true { n } else { 0.0 }; println(a); }",
            "-3\n",
        ),
        (
            "if-let-payload",
            "fn opt() -> Option[i64] { Some(7) }\n\
             fn main() { let a: f64 = if let Some(v) = opt() { v } else { 0.0 }; println(a); }",
            "7\n",
        ),
        (
            "return-position-if",
            "fn f(c: bool, n: i64) -> f64 { if c { n } else { 0.5 } }\n\
             fn main() { println(f(true, 7)); println(f(false, 7)); }",
            "7\n0.5\n",
        ),
        (
            "return-position-match",
            "fn f(k: i64, n: i64) -> f64 { match k { 0 => n, _ => 2.5 } }\n\
             fn main() { println(f(0, 7)); println(f(1, 7)); }",
            "7\n2.5\n",
        ),
        (
            "nested-branch",
            "fn main() { let n: i64 = 7; let a: f64 = if true { if true { n } else { 1.0 } } else { 2.0 }; println(a); }",
            "7\n",
        ),
        (
            "multi-int-arms",
            "fn main() { let n: i64 = 7; let a: f64 = match 2 { 0 => 1, 1 => 2, 2 => n, _ => 0.0 }; println(a); }",
            "7\n",
        ),
        (
            "block-bodied-arm",
            "fn main() { let a: f64 = if true { let z: i64 = 4; z } else { 0.0 }; println(a); }",
            "4\n",
        ),
        (
            "narrow-u8-arm",
            "fn main() { let b: u8 = 200; let a: f64 = if true { b } else { 0.0 }; println(a); }",
            "200\n",
        ),
        (
            "control-all-float",
            "fn main() { let a: f64 = if true { 1.5 } else { 0.0 }; println(a); }",
            "1.5\n",
        ),
        (
            "control-all-int",
            "fn main() { let n: i64 = 7; let a: i64 = if true { n } else { 0 }; println(a); }",
            "7\n",
        ),
        (
            "control-plain-let",
            "fn main() { let n: i64 = 7; let a: f64 = n; println(a); }",
            "7\n",
        ),
    ];
    for (label, src, want) in cases {
        assert_eq!(run(src), *want, "{label}");
    }
}

#[test]
fn interp_int_reaching_a_float_slot_through_an_aggregate_converts() {
    // B-2026-08-30-48 — an int RHS at a float-annotated binding is an implicit
    // widening the language performs, and B-2026-08-14-2 made the interpreter
    // do it for a SCALAR slot. Two holes were left, and both compiled backends
    // are correct at every shape below, so `karac build`'s output is the oracle
    // and every expectation here is what it already printed.
    //
    // (a) SIGNEDNESS through an aggregate literal. The conversion consults the
    //     RHS expression's unsigned width, but a tuple / array literal's own
    //     type is not an integer, so the lookup answered `None` and every
    //     element converted as SIGNED. `let t: (f64, i64) = (u, 1)` with
    //     `u: u64 = u64::MAX` read -1 against the compiled 1.8446744073709552e19.
    //     Each element has its own expression and its own signedness, so they
    //     are now resolved positionally at the `let`. The same shape one level
    //     in is a CONSTRUCTOR CALL: `Some(u)` has no integer type either, which
    //     is what `option-payload-u64` covers.
    //
    // (b) SHAPES THAT CONVERTED NOT AT ALL, leaving a `Value::Int` in a slot
    //     the program declared `f64`: a `Vec` element, an `Option`/`Result`
    //     payload, and an enum STRUCT-variant field. Each derives its target
    //     type from the annotation's own generic argument (or, for the variant,
    //     from the variant's payload declaration), so no program-wide lookup is
    //     needed — the conversion is the same one the scalar arm performs.
    //
    // The probe value is 2^53+1: it survives as ...993 when nothing converts
    // and lands on ...992 once it round-trips a double, so it separates a
    // correct store from a skipped conversion. `u64::MAX` separates `uitofp`
    // from `sitofp` the same way (18446744073709552000 vs -1).
    //
    // Measured on the pre-fix interpreter: 8 of these 15 diverged from AOT and
    // 7 agreed — the 5 named controls plus `tuple-i64-2p53` / `array-i64-2p53`,
    // whose recursion B-2026-08-14-2 already added and whose SIGNED reading was
    // correct for a signed source. Post-fix all 15 agree.
    let cases: &[(&str, &str, &str)] = &[
        // (a) signedness through an aggregate literal
        (
            "tuple-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
             let t: (f64, i64) = (u, 1); println(t.0); }",
            "18446744073709552000\n",
        ),
        (
            "tuple-mixed-signs",
            "fn main() { let u: u64 = 18446744073709551615u64; let s: i64 = -3;\n\
             let t: (f64, f64) = (u, s); println(f\"{t.0} {t.1}\"); }",
            "18446744073709552000 -3\n",
        ),
        (
            "array-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
             let a: Array[f64, 2] = [u, 1.0]; println(a[0]); }",
            "18446744073709552000\n",
        ),
        (
            "option-payload-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
             let o: Option[f64] = Some(u);\n\
             match o { Some(x) => println(x), None => println(-1.0) } }",
            "18446744073709552000\n",
        ),
        // (b) shapes that did not convert at all
        (
            "vec-literal-element",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let v: Vec[f64] = [m]; println(v[0]); }",
            "9007199254740992\n",
        ),
        (
            "option-payload",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let o: Option[f64] = Some(m);\n\
             match o { Some(x) => println(x), None => println(-1.0) } }",
            "9007199254740992\n",
        ),
        (
            "result-payload",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let r: Result[f64, i64] = Ok(m);\n\
             match r { Ok(x) => println(x), Err(_) => println(-1.0) } }",
            "9007199254740992\n",
        ),
        (
            "enum-struct-variant-field",
            "enum P { V { f: f64 } }\n\
             fn main() { let m: i64 = 9007199254740993;\n\
             match P.V { f: m } { V { f } => println(f) } }",
            "9007199254740992\n",
        ),
        // Already correct before the fix — here so a regression in the
        // recursion B-2026-08-14-2 added is caught alongside the new arms.
        (
            "tuple-i64-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let t: (f64, i64) = (m, 1); println(t.0); }",
            "9007199254740992\n",
        ),
        (
            "array-i64-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let a: Array[f64, 2] = [m, 1.0]; println(a[0]); }",
            "9007199254740992\n",
        ),
        // A LATER MUTATION, not an annotated binding: `Map`/`SortedMap.insert`
        // reaches its element type through `float_coerced_arg_sites` (the
        // channel `Vec.push` has used since B-2026-08-14-6), which the two map
        // store sites never consulted. `map-insert-f32` additionally pins that
        // the DECLARED WIDTH rides along — 4294967295 is not representable in
        // f32 and every compiled backend rounds it to 4294967296.
        (
            "map-insert-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let mut mp: Map[i64, f64] = Map.new(); mp.insert(1, m);\n\
             match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "9007199254740992\n",
        ),
        (
            "map-insert-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
             let mut mp: Map[i64, f64] = Map.new(); mp.insert(1, u);\n\
             match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "18446744073709552000\n",
        ),
        (
            "sortedmap-insert",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let mut mp: SortedMap[i64, f64] = SortedMap.new(); mp.insert(1, m);\n\
             match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "9007199254740992\n",
        ),
        (
            "map-insert-f32",
            "fn main() { let m: u32 = 4294967295u32;\n\
             let mut mp: Map[i64, f32] = Map.new(); mp.insert(1, m);\n\
             match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "4294967296\n",
        ),
        // Already correct before the fix — `Vec.push` is the shape the map
        // sites were missing, so a regression there breaks the same channel.
        (
            "vec-push",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let mut v: Vec[f64] = Vec.new(); v.push(m); println(v[0]); }",
            "9007199254740992\n",
        ),
        // Controls. The first two are the scalar and struct-field slots that
        // already converted; the rest are the same aggregate and insert SHAPES
        // with nothing to convert, so a fix that over-reached would move them.
        (
            "control-scalar-let",
            "fn main() { let m: i64 = 9007199254740993; let d: f64 = m; println(d); }",
            "9007199254740992\n",
        ),
        (
            "control-struct-field",
            "struct S { f: f64 }\n\
             fn main() { let m: i64 = 9007199254740993; let s = S { f: m }; println(s.f); }",
            "9007199254740992\n",
        ),
        (
            "control-tuple-all-float",
            "fn main() { let t: (f64, f64) = (1.5, 2.5); println(f\"{t.0} {t.1}\"); }",
            "1.5 2.5\n",
        ),
        (
            "control-option-string",
            "fn main() { let o: Option[String] = Some(\"hi\");\n\
             match o { Some(x) => println(x), None => println(\"no\") } }",
            "hi\n",
        ),
        (
            "control-vec-i64",
            "fn main() { let v: Vec[i64] = [7]; println(v[0]); }",
            "7\n",
        ),
        (
            "control-map-insert-i64",
            "fn main() { let m: i64 = 9007199254740993;\n\
             let mut mp: Map[i64, i64] = Map.new(); mp.insert(1, m);\n\
             match mp.get(1) { Some(x) => println(x), None => println(-1) } }",
            "9007199254740993\n",
        ),
        (
            "control-map-insert-str",
            "fn main() { let mut mp: Map[i64, String] = Map.new();\n\
             mp.insert(1, \"hi\");\n\
             match mp.get(1) { Some(x) => println(x), None => println(\"no\") } }",
            "hi\n",
        ),
    ];
    for (label, src, want) in cases {
        assert_eq!(run(src), *want, "{label}");
    }
}

/// B-2026-08-31-4 / B-2026-08-31-6 — the ORACLE side of the auto-par
/// swallowing pins in `tests/par_codegen.rs`. The interpreter has no
/// outlining, so it has always fired these at the live-range end; the value of
/// having it here is that the two files now assert the SAME string, which is
/// what makes the compiled pin a run-vs-build check rather than a snapshot of
/// whatever codegen happened to do.
///
/// design.md § Drop ordering: "Destructors fire at each binding's live-range
/// end, not at lexical scope end … a value whose last use is mid-scope is
/// dropped at that use and does not appear in the end-of-scope stack at all."
#[test]
fn nll_drop_point_at_a_call_that_last_uses_the_binding() {
    assert_eq!(
        run("struct S { e: E }\n\
             enum E { A(Vec[String]), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             fn mkv() -> Vec[String] {\n\
             \x20   let mut v: Vec[String] = Vec.new();\n\
             \x20   v.push(\"xyz\");\n\
             \x20   return v\n\
             }\n\
             fn ret_str(s: ref S) -> String {\n\
             \x20   match s.e { E.A(v) => { let m = v; return m[0] } E.B => { return \"n\" } }\n\
             }\n\
             fn ret_i64(s: ref S) -> i64 {\n\
             \x20   match s.e { E.A(v) => { let m = v; return m.len() } E.B => { return 0 } }\n\
             }\n\
             fn c_i64() {\n\
             \x20   println(\"i64\");\n\
             \x20   let s = S { e: E.A(mkv()) };\n\
             \x20   println(f\"{ret_i64(s)}\");\n\
             \x20   println(\"i64 end\");\n\
             }\n\
             fn c_str() {\n\
             \x20   println(\"str\");\n\
             \x20   let s = S { e: E.A(mkv()) };\n\
             \x20   println(f\"{ret_str(s)}\");\n\
             \x20   println(\"str end\");\n\
             }\n\
             fn main() { c_i64(); c_str(); println(\"end\"); }\n"),
        "i64\n1\ndE\ni64 end\nstr\nxyz\ndE\nstr end\nend\n",
        "the callee's return type does not move the caller's drop point"
    );

    // The two neighbouring spellings, same oracle as the compiled pin.
    assert_eq!(
        run("struct R { id: i64, v: Vec[String] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(id: i64) -> R {\n\
             \x20   let mut v: Vec[String] = Vec.new();\n\
             \x20   v.push(\"p\");\n\
             \x20   return R { id: id, v: v }\n\
             }\n\
             fn use_r(r: ref R) -> String { return r.v[0] }\n\
             fn before_group() {\n\
             \x20   println(\"B\");\n\
             \x20   let r = mk(1);\n\
             \x20   println(f\"{use_r(r)}\");\n\
             \x20   println(\"B end\");\n\
             }\n\
             fn both_inside() {\n\
             \x20   println(\"D\");\n\
             \x20   let r = mk(7);\n\
             \x20   let s = use_r(r);\n\
             \x20   println(s);\n\
             \x20   println(\"D end\");\n\
             }\n\
             fn staggered() {\n\
             \x20   println(\"E\");\n\
             \x20   let a = mk(8);\n\
             \x20   let b = mk(9);\n\
             \x20   println(f\"{use_r(a)}\");\n\
             \x20   println(f\"{use_r(b)}\");\n\
             \x20   println(\"E end\");\n\
             }\n\
             fn main() { before_group(); both_inside(); staggered(); println(\"end\"); }\n"),
        "B\np\ndR1\nB end\nD\ndR7\np\nD end\nE\np\ndR8\np\ndR9\nE end\nend\n",
        "each binding dies at its own live-range end"
    );
}

/// B-2026-09-01-16 — A STRUCT WHOSE FIELD IS PASSED BY VALUE FROM INSIDE AN
/// INTERPOLATED-STRING ARGUMENT HAS ITS `Drop` DEFERRED TO SCOPE EXIT ON THE
/// COMPILED SURFACES — the interpreter oracle.
///
/// `h`'s last use is the `readf(h.r)` call inside the `println` f-string, so
/// design.md § Drop ("Destructors fire at each binding's live-range end, not
/// at lexical scope end") puts `drop 40` after `field=43` and BEFORE `end`.
/// The row measured every compiled column printing `field=43 end drop 40`
/// while the interpreter had the order above; hoisting the call into its own
/// `let` made all four agree, which is what localized it to the admission of
/// the binding rather than the existence of the early-fire pass.
///
/// The row closed without a change of its own. Probed at its filing commit
/// (`7717d75`) the default build printed `field=43 end drop 40` while
/// `KARAC_AUTO_PAR=0` was ALREADY the interpreter's order — the same `np`
/// mismeasurement the class row's fix corrected — and `aa21ffb` (the
/// B-2026-08-31-4 / B-2026-08-31-6 fix: an auto-par group no longer swallows
/// a covered NLL drop point) took the divergence. So the row's distinction
/// from its class ("neither a `ref` arg nor a heap return") was drawn against
/// the misattributed mechanism; what selected this program was a group
/// covering the f-string statement. This pin exists so it stays gone — a
/// position-only drift like this is invisible to every count-based drop
/// gate.
#[test]
fn nll_drop_point_at_a_by_value_field_arg_inside_an_fstring() {
    assert_eq!(
        run(r#"struct Res { id: i64, buf: Vec[i64] }
impl Drop for Res { fn drop(mut ref self) { println(f"drop {self.id}") } }
struct Holder { r: Res }

fn mk(n: i64) -> Res {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 { v.push(n + i); i = i + 1; }
    return Res { id: n, buf: v }
}

fn hand(r: Res) -> Res { return r }
fn readf(r: Res) -> i64 { return r.buf[3] }

fn main() {
    let b = mk(20);
    let hb = hand(b);
    println(f"hand={hb.buf[1]}");
    let h = Holder { r: mk(40) };
    println(f"field={readf(h.r)}");
    println("end");
}
"#),
        "hand=21\ndrop 20\nfield=43\ndrop 40\nend\n",
        "`h` dies at the f-string statement that last uses it, after the line \
         that statement prints and before the next statement"
    );
}

/// B-2026-09-15-5 — EVERY lookup entry point on EVERY container runs its key
/// temporary's user `Drop` body, and this matrix exists because the first pass
/// missed three of the eleven.
///
/// Codegen funnels all eleven through ONE chokepoint
/// (`free_fresh_owned_struct_key_arg`), so its half was complete the moment that
/// dispatcher gained the bodies call. The interpreter has a SEPARATE arm per
/// container per method, so a per-site fix there is only as complete as the list
/// the author enumerated — and mine was short by `Set.remove`,
/// `SortedSet.remove` and `Vec.contains`. The asymmetry is the whole lesson: a
/// one-chokepoint backend and an eleven-site backend cannot be paired by fixing
/// "the obvious sites", and the gap it leaves is a RUN/BUILD DIVERGENCE (codegen
/// correct, interpreter silent), which is worse than the symmetric gap it
/// replaced.
///
/// Found by sweeping the matrix against the compiled backend rather than by
/// reading the interpreter, which is why every cell is here rather than only the
/// three that were broken: the eight that were already right are what make the
/// three a gap instead of a guess.
///
/// Each cell looks up a key the container does NOT hold, so the two bodies are
/// unambiguous: `dK2` is the discarded argument temporary (owed AT the lookup)
/// and `dK1` is the container's own stored element, which fires at the
/// container's live-range end — the lookup, since that is its last use.
#[test]
fn test_every_lookup_entry_point_runs_its_key_temporarys_body() {
    let hdr = "#[derive(Hash, Eq, PartialEq, Ord)]\n\
               struct K { n: i64 }\n\
               impl Drop for K { fn drop(mut ref self) { println(f\"dK{self.n}\"); } }\n";
    for (label, stmts, want) in [
        (
            "Map.get",
            "let mut c: Map[K, i64] = Map.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             match c.get(K { n: 2 }) { Some(v) => { println(\"hit\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "Map.contains_key",
            "let mut c: Map[K, i64] = Map.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             if c.contains_key(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "Map.remove",
            "let mut c: Map[K, i64] = Map.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             c.remove(K { n: 2 });\n\
             println(\"post\");\n",
            "pre\ndK2\ndK1\npost\n",
        ),
        (
            "SortedMap.get",
            "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             match c.get(K { n: 2 }) { Some(v) => { println(\"hit\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "SortedMap.contains_key",
            "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             if c.contains_key(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "SortedMap.remove",
            "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
             c.insert(K { n: 1 }, 1);\n\
             println(\"pre\");\n\
             c.remove(K { n: 2 });\n\
             println(\"post\");\n",
            "pre\ndK2\ndK1\npost\n",
        ),
        (
            "Set.contains",
            "let mut c: Set[K] = Set.new();\n\
             c.insert(K { n: 1 });\n\
             println(\"pre\");\n\
             if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "Set.remove -- MISSED on the first pass",
            "let mut c: Set[K] = Set.new();\n\
             c.insert(K { n: 1 });\n\
             println(\"pre\");\n\
             c.remove(K { n: 2 });\n\
             println(\"post\");\n",
            "pre\ndK2\ndK1\npost\n",
        ),
        (
            "SortedSet.contains",
            "let mut c: SortedSet[K] = SortedSet.new();\n\
             c.insert(K { n: 1 });\n\
             println(\"pre\");\n\
             if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
        (
            "SortedSet.remove -- MISSED on the first pass",
            "let mut c: SortedSet[K] = SortedSet.new();\n\
             c.insert(K { n: 1 });\n\
             println(\"pre\");\n\
             c.remove(K { n: 2 });\n\
             println(\"post\");\n",
            "pre\ndK2\ndK1\npost\n",
        ),
        (
            "Vec.contains -- MISSED on the first pass",
            "let mut c: Vec[K] = Vec.new();\n\
             c.push(K { n: 1 });\n\
             println(\"pre\");\n\
             if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndK2\nmiss\ndK1\npost\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}
