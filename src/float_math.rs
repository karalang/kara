//! Shared classification of the built-in scalar transcendental + rounding
//! math methods on float primitives — `x.sin()`, `x.pow(y)`, `x.floor()`,
//! and so on. The typechecker, interpreter, and codegen all key off this
//! single table so the surface can't drift between `karac run` and
//! `karac build`.
//!
//! **Surface decision** (the open question this slice settled): these are
//! *value-receiver methods* (mirroring the shipped `x.sqrt()` / `x.abs()`),
//! not a `std.math` free-function module. `sqrt` predates this table and
//! stays inline at each site; everything here is the second wave driven by
//! the Plume flow-field dogfood, which had to hand-build curl-noise from
//! rational vortices precisely because no trig existed yet.
//!
//! **Lowering** (codegen `method_call.rs`): most map to their LLVM intrinsic
//! (`llvm.sin` / `llvm.cos` / `llvm.exp` / `llvm.log` / `llvm.log2` /
//! `llvm.pow` / `llvm.floor` / `llvm.ceil` / `llvm.round` / `llvm.exp2` /
//! `llvm.log10` / `llvm.trunc`), which lower to libm calls on most targets
//! (and on wasm too — the math symbols live in wasi-libc's `libc.a`, already
//! linked by the wasm-ld path, so no archive/`--export` work is needed). The
//! inverse-trig / hyperbolic set (`asin`/`acos`/`atan`, `sinh`/`cosh`/`tanh`)
//! and `tan`/`atan2` are the exceptions: their LLVM intrinsics are LLVM-19+,
//! absent on the 18.1 pin, so they lower to a direct width-correct libm call
//! (`tan`/`tanf`, `asin`/`asinf`, …). The interpreter (`method_call.rs`)
//! delegates to Rust's `f64::*` — except for the three inverse hyperbolics,
//! which call libm directly through the shims at the bottom of this file
//! because Rust's std does not implement those in terms of libm at all
//! (B-2026-08-29-60).

/// Arity of a float-math method beyond the receiver.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FloatMathKind {
    /// `x.m() -> Self` — no extra argument (`sin`, `cos`, `floor`, …).
    Unary,
    /// `x.m(y) -> Self` — one argument of the same float type (`pow`, `atan2`).
    Binary,
}

/// Classify `method` as a built-in float-math method, returning its arity.
/// Returns `None` for any other name (callers fall through to normal method
/// dispatch). Intentionally excludes `sqrt`/`abs`, which predate this table
/// and stay inline at each site. `cbrt` is deliberately absent: Rust's
/// `f64::cbrt` is an in-Rust implementation, not a libm call, and it disagrees
/// with the system libm `cbrt` this table would lower to (e.g. `27.0.cbrt()`
/// is `3.0000000000000004` in the interpreter but `3.0` from libm), which
/// would break `run == build`.
pub fn classify(method: &str) -> Option<FloatMathKind> {
    Some(match method {
        "sin" | "cos" | "tan" | "exp" | "ln" | "log2" | "floor" | "ceil" | "round" | "asin"
        | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "exp2" | "log10" | "trunc" | "asinh"
        | "acosh" | "atanh" | "exp_m1" | "ln_1p" => FloatMathKind::Unary,
        "pow" | "atan2" | "hypot" | "copysign" => FloatMathKind::Binary,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// libm shims for the inverse hyperbolics
// ---------------------------------------------------------------------------

// `asinh` / `acosh` / `atanh` are the one place where delegating to Rust's
// `f64::*` does NOT reproduce what codegen emits. Rust's std does not route
// these to libm the way it does `cosh` / `log10` / `atan`; it evaluates them
// as formulas (`asinh` as an `ln_1p` of a `hypot` expression, `atanh` as
// `0.5 * ((2x)/(1-x)).ln_1p()`, and so on). Codegen lowers them to the libm
// calls listed under "Lowering" above, so a Rust-side evaluation is a
// DIFFERENT ALGORITHM rather than a different rounding — and two algorithms
// for one function disagree in the last ULP on a large fraction of inputs, at
// f64 just as much as at f32.
//
// Measured over a 120-point series per method (B-2026-08-29-60), Rust's
// formula differs from libm on 14/17/3 of 120 inputs at f64 and 10/12/12 at
// f32 for `asinh`/`acosh`/`atanh`. Those counts matched the observed
// interp-vs-codegen divergence exactly, in all six cells, which is what
// identified the mechanism: the compiled output was libm's value on every
// line, the interpreted output Rust's formula's on every line.
//
// Calling the same libm entry point here makes the two backends agree by
// CONSTRUCTION rather than by tuning a rounding step — the same "one
// implementation, both backends" rule the `karac-hash` crate and the Arrow
// IPC twin already follow. It also tracks the host libm, which is what
// codegen's answer depends on; no Rust-side reimplementation could.
//
// This is precisely the hazard `classify`'s doc names for `cbrt`
// (Rust-implemented rather than libm-delegating, so `run == build` breaks).
// That one was handled by leaving `cbrt` out of the language; these three
// were admitted without anyone noticing they sit in the same class.
//
// C99 §7.12.5, so the symbols are present in glibc, musl, macOS libSystem
// and the Windows UCRT alike. On the platforms that build codegen these are
// the very six the compiled program already links; on Windows, where CI runs
// the default leg only, this block is the first thing to require them, so a
// gap there would surface as a link error rather than a wrong answer.
extern "C" {
    #[link_name = "asinh"]
    fn c_asinh(x: f64) -> f64;
    #[link_name = "asinhf"]
    fn c_asinhf(x: f32) -> f32;
    #[link_name = "acosh"]
    fn c_acosh(x: f64) -> f64;
    #[link_name = "acoshf"]
    fn c_acoshf(x: f32) -> f32;
    #[link_name = "atanh"]
    fn c_atanh(x: f64) -> f64;
    #[link_name = "atanhf"]
    fn c_atanhf(x: f32) -> f32;
}

/// `asinh` at f64, from libm — the symbol codegen calls.
pub fn asinh_f64(x: f64) -> f64 {
    unsafe { c_asinh(x) }
}
/// `asinhf` at f32, from libm — the symbol codegen calls.
pub fn asinh_f32(x: f32) -> f32 {
    unsafe { c_asinhf(x) }
}
/// `acosh` at f64, from libm — the symbol codegen calls.
pub fn acosh_f64(x: f64) -> f64 {
    unsafe { c_acosh(x) }
}
/// `acoshf` at f32, from libm — the symbol codegen calls.
pub fn acosh_f32(x: f32) -> f32 {
    unsafe { c_acoshf(x) }
}
/// `atanh` at f64, from libm — the symbol codegen calls.
pub fn atanh_f64(x: f64) -> f64 {
    unsafe { c_atanh(x) }
}
/// `atanhf` at f32, from libm — the symbol codegen calls.
pub fn atanh_f32(x: f32) -> f32 {
    unsafe { c_atanhf(x) }
}
