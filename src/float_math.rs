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
//! inverse-trig / hyperbolic set (`asin`/`acos`/`atan`, `sinh`/`cosh`/`tanh`),
//! `cbrt`, and `tan`/`atan2` are the exceptions: their LLVM intrinsics are
//! LLVM-19+, absent on the 18.1 pin, so they lower to a direct width-correct
//! libm call (`tan`/`tanf`, `asin`/`asinf`, …). The interpreter
//! (`method_call.rs`) delegates to Rust's `f64::*` — except for the three
//! inverse hyperbolics and `cbrt`, which call libm directly through the shims
//! at the bottom of this file because Rust's std does not implement those in
//! terms of libm at all (B-2026-08-29-60, B-2026-08-30-4).
//!
//! **One implementation per lane.** A bare libm name does not name one
//! function: `compiler_builtins` ships weak definitions for part of the math
//! surface, and they win over the platform libm's wherever a Rust object is in
//! the link — which the interpreter and an AOT binary always have and the JIT,
//! resolving through `dlsym`, does not. `cbrt` is the one name where the two
//! implementations disagree, so `codegen::lljit` republishes it into the JIT;
//! `PUBLISHED_LIBM_SYMBOLS` and the test beside it are what keep that set
//! honest as toolchains move.

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
/// and stay inline at each site.
///
/// `cbrt` WAS absent because Rust implements it itself rather than calling
/// libm, so `f64::cbrt` and the symbol codegen emits could disagree
/// (B-2026-08-30-4). The shim block below removes that the way B-2026-08-29-60
/// removed it for `asinh`/`acosh`/`atanh`: the interpreter calls the SAME
/// symbol codegen emits, so the two agree by construction rather than by the
/// exclusion.
///
/// MEASURED, because the direction is easy to get backwards and the row that
/// asked for this admission had it backwards. On x86-64 glibc with rustc
/// 1.94.1, `27.0.cbrt()` is `3` from Rust and `3.0000000000000004` from
/// glibc's `cbrt` — the opposite of what that row recorded — and Rust's
/// `f64::cbrt` is bit-identical to a Rust binary's `extern "C" cbrt` on all
/// 2000 sampled inputs at both widths. Both facts have the same cause:
/// `compiler_builtins` ships its own weak `cbrt`/`cbrtf`, that definition
/// wins over the platform libm's wherever a Rust object is in the link, and
/// Rust's std routes `f64::cbrt` to it. So the shim is not load-bearing on
/// THIS host — it is what keeps the two backends tied together on a host
/// where std stops agreeing with the linked symbol, which is a fact about a
/// Rust version rather than an invariant.
///
/// The hazard the admission DID expose is one lane over, and
/// `codegen::lljit`'s `define_compiler_rt_builtins` is what closes it: the
/// JIT resolves the emitted call through `dlsym`, which cannot see a local
/// archive symbol, so it alone reached the platform's `cbrt` while the
/// interpreter and AOT lanes reached compiler_builtins'.
pub fn classify(method: &str) -> Option<FloatMathKind> {
    Some(match method {
        "sin" | "cos" | "tan" | "exp" | "ln" | "log2" | "floor" | "ceil" | "round" | "asin"
        | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "exp2" | "log10" | "trunc" | "asinh"
        | "acosh" | "atanh" | "exp_m1" | "ln_1p" | "cbrt" => FloatMathKind::Unary,
        "pow" | "atan2" | "hypot" | "copysign" => FloatMathKind::Binary,
        _ => return None,
    })
}

/// Is `method`'s result unchanged by being computed in a WIDER format and
/// rounded down to the receiver's width?
///
/// This is the double-rounding question, and it decides whether LLVM's
/// constant folder may be left to evaluate a compile-time-known receiver
/// (B-2026-08-29-61). The folder computes every one of these through the
/// HOST's `double` function and rounds the result to the receiver's type,
/// while a receiver the compiler cannot see through goes to the target's
/// width-correct symbol (`coshf`, `llvm.log10.f32`). Two roundings versus
/// one, so for an inexact method the same expression takes two different
/// values inside a single binary depending on whether its argument happened
/// to be foldable -- measured on `sinh`, `log10`, `atan2`, `cosh`, `tanh`,
/// `atan` and `asin` at f32.
///
/// `floor` / `ceil` / `round` / `trunc` return a value that is exactly
/// representable at the receiver's width, and `copysign` only moves a sign
/// bit, so for these five the wider computation and the narrow one agree on
/// every input and the fold is free. Everything else is transcendental and
/// must be computed once, by the target's libm.
///
/// (`sqrt` is not in this table, but belongs on the exact side for a less
/// obvious reason: rounding a binary64 square root to binary32 is correctly
/// rounded because 53 >= 2*24 + 2. That is a property of `sqrt` alone -- it
/// does not extend to any transcendental here.)
pub fn constant_fold_is_exact(method: &str) -> bool {
    matches!(method, "floor" | "ceil" | "round" | "trunc" | "copysign")
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
// `cbrt` was kept out of the language for what reads like the same hazard,
// and joined the block when B-2026-08-30-4 admitted it — but measuring it
// showed the two cases are NOT the same, which is worth knowing before the
// next name is added on the strength of the resemblance. The three above are
// genuinely a different algorithm from the linked symbol. `cbrt` is not:
// Rust's `f64::cbrt` IS the symbol codegen emits, because
// `compiler_builtins` supplies a weak `cbrt` that wins in any Rust link and
// std routes to it — bit-identical over 2000 sampled inputs at both widths.
// So the shim below is load-bearing for `asinh`/`acosh`/`atanh` and merely
// insurance for `cbrt`. See `classify`'s doc for the measurement, and for the
// lane where `cbrt` DID diverge, which was the JIT rather than either of
// these two backends.
//
// C99 §7.12.5 and §7.12.7.1, so the symbols are present in glibc, musl, macOS
// libSystem and the Windows UCRT alike. On the platforms that build codegen
// these are the very eight the compiled program already links; on Windows,
// where CI runs the default leg only, this block is the first thing to
// require them, so a gap there would surface as a link error rather than a
// wrong answer.
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
    // B-2026-08-30-4 — `cbrt`, on the weaker footing described above: not a
    // measured divergence between Rust and the linked symbol, but the same
    // shape of risk, held down the same way.
    #[link_name = "cbrt"]
    fn c_cbrt(x: f64) -> f64;
    #[link_name = "cbrtf"]
    fn c_cbrtf(x: f32) -> f32;
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

/// `cbrt` at f64, from libm — the symbol codegen calls (B-2026-08-30-4).
pub fn cbrt_f64(x: f64) -> f64 {
    unsafe { c_cbrt(x) }
}
/// `cbrtf` at f32, from libm — the symbol codegen calls (B-2026-08-30-4).
pub fn cbrt_f32(x: f32) -> f32 {
    unsafe { c_cbrtf(x) }
}

/// Addresses of the `cbrt` / `cbrtf` THIS binary linked, for the JIT to
/// publish as absolute symbols (B-2026-08-30-4).
///
/// `cbrt` is the one name in this file where the host has TWO implementations
/// and they disagree. `compiler_builtins` — linked into every Rust binary and
/// every Rust staticlib — ships a weak `cbrt`/`cbrtf` of its own, and it wins
/// over the platform libm's strong definition wherever a Rust object is in the
/// link. So the interpreter (inside `karac`) and an AOT binary (which links
/// `libkarac_runtime.a`) both get compiler_builtins'; the JIT, whose runner
/// references neither, used to resolve the emitted `cbrt` call through
/// `dlsym` and get the platform's. Measured on x86-64 glibc: `27.0.cbrt()` is
/// `3` from compiler_builtins and `3.0000000000000004` from glibc, so the
/// admission of `cbrt` opened a `run == build` split on the JIT lane alone.
///
/// Handing these addresses to `LLJITEngine::define_absolute_symbols` closes
/// it the same way `__muloti4` was closed: the JIT calls the very function
/// `c_cbrt` resolves to here, which is the same one the other two lanes link.
pub fn cbrt_libm_addrs() -> (usize, usize) {
    (c_cbrt as *const () as usize, c_cbrtf as *const () as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// B-2026-08-29-61 — the exact/inexact split is what decides whether LLVM
    /// may constant-fold a call, so it is pinned rather than left to drift.
    ///
    /// A method wrongly on the EXACT side gets folded through the host's
    /// `double` libm and silently returns a different value from the same
    /// expression with a runtime argument. A method wrongly on the INEXACT
    /// side only loses a fold. The asymmetry is why every name in the table
    /// is listed here explicitly instead of the test checking a handful.
    #[test]
    fn constant_fold_exactness_is_pinned_for_every_float_math_method() {
        let exact = ["floor", "ceil", "round", "trunc", "copysign"];
        for m in exact {
            assert!(
                classify(m).is_some(),
                "{m} is claimed exact but is not a float-math method"
            );
            assert!(constant_fold_is_exact(m), "{m} must keep the fold");
        }
        for m in [
            "sin", "cos", "tan", "exp", "ln", "log2", "asin", "acos", "atan", "sinh", "cosh",
            "tanh", "exp2", "log10", "asinh", "acosh", "atanh", "exp_m1", "ln_1p", "pow", "atan2",
            "hypot", "cbrt",
        ] {
            assert!(
                classify(m).is_some(),
                "{m} is claimed inexact but is not a float-math method"
            );
            assert!(
                !constant_fold_is_exact(m),
                "{m} is transcendental — folding it at double precision gives a \
                 different answer from the emitted call (B-2026-08-29-61)"
            );
        }
    }
}
