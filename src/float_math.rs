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
//! `llvm.pow` / `llvm.floor` / `llvm.ceil` / `llvm.round`), which lower to
//! libm calls on most targets (and on wasm too — the math symbols live in
//! wasi-libc's `libc.a`, already linked by the wasm-ld path, so no
//! archive/`--export` work is needed). `tan` and `atan2` are the exceptions:
//! `llvm.tan` / `llvm.atan2` are LLVM-19+, absent on the 18.1 pin, so they
//! lower to a direct width-correct libm call (`tan`/`tanf`, `atan2`/`atan2f`).
//! The interpreter (`method_call.rs`) delegates to Rust's `f64::*`.

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
pub fn classify(method: &str) -> Option<FloatMathKind> {
    Some(match method {
        "sin" | "cos" | "tan" | "exp" | "ln" | "log2" | "floor" | "ceil" | "round" => {
            FloatMathKind::Unary
        }
        "pow" | "atan2" => FloatMathKind::Binary,
        _ => return None,
    })
}
