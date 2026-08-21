//! WGSL codegen — GPU spike **slice-0b**
//! ([`docs/spikes/gpu-wgsl-slice0.md`]).
//!
//! Lowers a `#[gpu]` kernel of the element-wise-map shape
//!
//! ```text
//! #[gpu] fn double(x: f32) -> f32 { x * 2.0 }
//! ```
//!
//! into the WGSL compute shader that [`crate::gpu_wgsl`]'s runtime twin
//! ([`karac-runtime`'s `dispatch_f32_map`]) dispatches: a fixed boilerplate
//! wrapper around one kernel-specific line, `output[i] = <body>`, where the
//! single kernel parameter `x` maps to the indexed input load `input[i]`.
//!
//! **Architecture — respects the codegen-containment invariant.** WGSL is
//! *text*; this module imports no `inkwell`/LLVM types and is *not* part of
//! `src/codegen.rs`. `codegen.rs` (slice-0c) consumes the [`String`] this
//! produces as plain data — the same plain-data-hint pattern every other
//! analysis pass uses to feed the backend. See the invariant in `CLAUDE.md`.
//!
//! **Scope (slice-0 floor).** The per-element map `fn k(x: T) -> U` over a
//! `[T]` buffer producing `[U]`, with `T = U = f32` (what the proven runtime
//! spine handles). The body is the trivial GpuSafe subset: numeric literals,
//! binary arithmetic (`+ - * / %`), unary negation, and the single parameter.
//! Everything else — additional parameters, non-`f32` element types, locals,
//! control flow, calls — returns a structured [`WgslError`] so slice-0c can
//! gate cleanly rather than emit invalid WGSL. Reductions, whole-array forms,
//! and multi-buffer dispatch are explicitly later increments.
//!
//! The FE-1–4 front-end already guarantees a `#[gpu]` kernel is GpuSafe and
//! effect-clean, so this emitter assumes a clean subset and only has to reject
//! the shapes slice-0 has not *yet* grown to lower (not ill-formed programs).

use crate::ast::{
    BinOp, Block, CallArg, CompoundOp, Expr, ExprKind, Function, Param, PatternKind, Stmt,
    StmtKind, TypeExpr, TypeKind, UnaryOp,
};
use crate::reduce_kernel::{ReduceOp, GPU_MATMUL_TILE, GPU_REDUCE_WIDTH};
use std::collections::{HashMap, HashSet};

/// Why a `#[gpu]` kernel could not be lowered to slice-0 WGSL. Every variant
/// is a "slice-0 does not handle this *yet*" shape, not an ill-formed program
/// (the front-end already proved GpuSafe). Carries a human-readable reason for
/// the slice-0c diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WgslError {
    /// The kernel signature is not the slice-0 element-wise-map shape
    /// (exactly one `f32` parameter returning `f32`).
    UnsupportedSignature(String),
    /// The kernel body is not a single expression over the trivial subset.
    UnsupportedBody(String),
}

impl WgslError {
    /// The human-readable reason, for surfacing in a diagnostic.
    pub fn reason(&self) -> &str {
        match self {
            WgslError::UnsupportedSignature(s) | WgslError::UnsupportedBody(s) => s,
        }
    }
}

impl std::fmt::Display for WgslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason())
    }
}

/// The workgroup size baked into the emitted shader. Must match the
/// `dispatch_workgroups(ceil(n / N))` divisor in the runtime spine.
const WORKGROUP_SIZE: u32 = 64;

/// Threads per dispatch-grid X ROW: `65535 workgroups × WORKGROUP_SIZE`.
/// wgpu caps each dispatch dimension at 65535 workgroups, so `run_compute`
/// spreads larger element counts across a 2D grid with the X extent FIXED
/// at 65535 whenever a second row exists; every kernel entry recovers the
/// flat thread index as `gid.y * DISPATCH_X_SPAN + gid.x` — a fold-time
/// constant, no uniform. For a single-row dispatch (`y == 1`, any x)
/// `gid.y == 0` and the formula degenerates to `gid.x`, so the one entry
/// line is correct at every size. Overshoot threads in the last row exit
/// on the existing `>= arrayLength` guard. Runtime twin:
/// `runtime/src/gpu.rs::run_compute` (the dispatch split must match).
pub const DISPATCH_X_SPAN: u32 = 65535 * WORKGROUP_SIZE;

/// WORKGROUPS per dispatch-grid X row — the same `65535` cap, counted in
/// workgroups rather than threads. A kernel that writes ONE value per
/// workgroup (every reduction partial, every scan chunk-total, every
/// overflow flag) must index that output by the FLAT workgroup number
/// `wid.y * DISPATCH_X_WORKGROUPS + wid.x`, because `wid.x` alone repeats
/// on every row of a 2D dispatch and the rows would overwrite each other.
/// Degenerates to `wid.x` on a single-row dispatch (`wid.y == 0`), so the
/// one form is correct at every size — exactly like [`DISPATCH_X_SPAN`].
/// Runtime twin: `runtime/src/gpu.rs::run_compute` (`x = wg.min(65535)`).
pub const DISPATCH_X_WORKGROUPS: u32 = DISPATCH_X_SPAN / WORKGROUP_SIZE;

/// Emit the WGSL compute shader for a slice-0 element-wise-map `#[gpu]`
/// kernel. On success the returned string is a complete, standalone module
/// with `@group(0) @binding(0)` = read `input: array<f32>`, `@binding(1)` =
/// read_write `output: array<f32>`, and a `@compute @workgroup_size(64) fn
/// main` entry point — exactly the layout the runtime `dispatch_f32_map`
/// expects.
pub fn emit_kernel(func: &Function, helpers: &[&Function]) -> Result<String, WgslError> {
    // Before anything is emitted: refuse trapping integer arithmetic, which
    // WGSL cannot express (B-2026-08-19-1). Runs first so the diagnostic names
    // the semantics problem rather than whatever the emitter would have
    // complained about downstream.
    reject_trapping_int_arith(func)?;
    for h in helpers {
        reject_trapping_int_arith(h)?;
    }
    let param = kernel_param(func)?;
    let param_name = param.name().ok_or_else(|| {
        WgslError::UnsupportedSignature(
            "the GPU kernel parameter must be a plain binding".to_string(),
        )
    })?;

    // Slice-0 floor: a single scalar `T -> T` over the WGSL-native 4-byte
    // scalars (`f32` / `i32` / `u32`). The runtime dispatch is byte-oriented,
    // so any of the three works; the shader's `array<T>` bindings carry the
    // element interpretation.
    let param_scalar = wgsl_scalar(&param.ty, "parameter")?;
    let return_scalar = match &func.return_type {
        Some(ty) => wgsl_scalar(ty, "return type")?,
        None => {
            return Err(WgslError::UnsupportedSignature(
                "a GPU kernel must return a scalar (f32 / i32 / u32) — slice-0 element-wise map"
                    .to_string(),
            ));
        }
    };
    if param_scalar != return_scalar {
        return Err(WgslError::UnsupportedSignature(format!(
            "a slice-0 GPU kernel must map `T -> T` (found `{param_scalar} -> {return_scalar}`)"
        )));
    }
    let scalar = param_scalar;

    // `#[gpu]` helper functions reachable from the kernel body (GPU-LBM-5),
    // emitted as WGSL `fn`s before `main`; their names are recognized as calls.
    let (helper_defs, helper_names) = emit_helpers(func, helpers)?;

    // The body is a statement sequence — `let` / `let mut` bindings,
    // assignments, and `while` loops — followed by the tail expression
    // (B-2026-08-18-40). `Scope` carries the source-name → WGSL-name mapping so
    // the param resolves to `input[i]` and a local resolves to its own (possibly
    // renamed) identifier.
    let (stmts, body_expr) = scalar_body_stmts(func)?;
    let mut scope = Scope::new(param_name);
    let mut decls = String::new();
    emit_stmts(&stmts, &mut scope, &helper_names, 1, &mut decls)?;
    let body_wgsl = lower_expr(body_expr, &scope.resolver(), &helper_names)?;

    Ok(format!(
        "{helper_defs}@group(0) @binding(0) var<storage, read>       input:  array<{scalar}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{scalar}>;\n\
         \n\
         @compute @workgroup_size({WORKGROUP_SIZE})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   if (i >= arrayLength(&input)) {{ return; }}\n\
         {decls}\
         \x20   output[i] = {body_wgsl};\n\
         }}\n"
    ))
}

/// One statement of a scalar `#[gpu]` kernel body. The scalar path supports
/// strictly more than the struct-SoA / stencil emitters (which take only `let`
/// bindings through [`kernel_body_parts`]), so it carries its own statement
/// model rather than widening a helper two callers cannot use.
enum KStmt<'a> {
    /// `let name = value;` / `let mut name = value;` → WGSL `let` / `var`.
    Bind {
        name: &'a str,
        mutable: bool,
        value: &'a Expr,
    },
    /// `target = value;` or `target += value;` — `op` is the compound
    /// operator's WGSL spelling, or `None` for a plain assignment.
    Assign {
        target: &'a str,
        op: Option<&'static str>,
        value: &'a Expr,
    },
    /// `while cond { body }` → WGSL `while (cond) { … }`.
    While {
        cond: &'a Expr,
        body: Vec<KStmt<'a>>,
    },
    /// `for v in start..end { body }` → WGSL
    /// `for (var v = start; v < end; v = v + 1) { … }`. The bound is `<=` when
    /// the Kāra range is inclusive (`..=`).
    ForRange {
        var: &'a str,
        start: &'a Expr,
        end: &'a Expr,
        inclusive: bool,
        body: Vec<KStmt<'a>>,
    },
    /// `if cond { … } else { … }` in STATEMENT position → WGSL's native `if`
    /// statement (B-2026-08-18-49).
    ///
    /// This is a genuinely different lowering from the value-`if` a few
    /// hundred lines down, which becomes a branchless `select(else, then,
    /// cond)`. Position picks between them, and both are wanted: `select`
    /// keeps a value-producing conditional divergence-free, which is what a
    /// GPU wants, while a conditional that ASSIGNS cannot be an expression at
    /// all and needs the real statement.
    If {
        cond: &'a Expr,
        then_body: Vec<KStmt<'a>>,
        /// `None` for a bare `if` with no `else`. An `else if` chain nests
        /// here as a single-element list holding another [`KStmt::If`].
        else_body: Option<Vec<KStmt<'a>>>,
    },
    /// `var name: T;` — an uninitialized declaration, emitted only as the
    /// hoisted destination of a value-`if` whose branches carry statements
    /// (B-2026-08-18-49 step 2). WGSL requires the type here because there is
    /// no initializer to infer from, which is why that desugar needs the
    /// binding's annotation.
    DeclareVar {
        name: &'a str,
        wgsl_ty: &'static str,
    },
}

/// Lexical scope for a scalar kernel body: a stack of (source name, WGSL name)
/// pairs plus the kernel parameter. Entering a block records the stack depth and
/// exiting truncates back to it, so a `let` inside a `while` body is not visible
/// after the loop — matching both Kāra's scoping and WGSL's.
struct Scope<'a> {
    param: &'a str,
    /// Innermost binding last; lookups scan in reverse.
    names: Vec<(&'a str, String, bool)>,
}

impl<'a> Scope<'a> {
    fn new(param: &'a str) -> Self {
        Scope {
            param,
            names: Vec::new(),
        }
    }

    /// Bind `name`, returning the WGSL identifier it will use. A name that
    /// collides with the generated wrapper's own declarations (or a WGSL
    /// keyword) is renamed rather than rejected — a loop counter called `i` is
    /// too idiomatic to turn away, and the rename is invisible in Kāra source.
    fn bind(&mut self, name: &'a str, mutable: bool) -> String {
        let mut wgsl = if WGSL_RESERVED_LOCALS.contains(&name) {
            format!("{name}_k")
        } else {
            name.to_string()
        };
        // Renaming could itself collide with a live binding; walk to a free one.
        while self.names.iter().any(|(_, w, _)| *w == wgsl) {
            wgsl.push('_');
        }
        self.names.push((name, wgsl.clone(), mutable));
        wgsl
    }

    fn depth(&self) -> usize {
        self.names.len()
    }

    fn truncate(&mut self, depth: usize) {
        self.names.truncate(depth);
    }

    /// The innermost binding of `name`, if any.
    fn lookup(&self, name: &str) -> Option<&(&'a str, String, bool)> {
        self.names.iter().rev().find(|(n, _, _)| *n == name)
    }

    /// Identifier resolver for [`lower_expr`]: a local wins over the parameter,
    /// which reads this thread's element.
    fn resolver(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |n: &str| {
            if let Some((_, wgsl, _)) = self.lookup(n) {
                Some(wgsl.clone())
            } else if n == self.param {
                Some("input[i]".to_string())
            } else {
                None
            }
        }
    }
}

/// Split a scalar kernel body into its statements and tail expression.
fn scalar_body_stmts(func: &Function) -> Result<(Vec<KStmt<'_>>, &Expr), WgslError> {
    let block = &func.body;
    let (stmts, tail) = if let Some(t) = &block.final_expr {
        (block.stmts.as_slice(), t.as_ref())
    } else if let Some((last, init)) = block.stmts.split_last() {
        if let StmtKind::Expr(Expr {
            kind: ExprKind::Return(Some(inner)),
            ..
        }) = &last.kind
        {
            (init, inner.as_ref())
        } else {
            return Err(WgslError::UnsupportedBody(
                "a GPU kernel body must end in a scalar expression or `return <expr>;`".to_string(),
            ));
        }
    } else {
        return Err(WgslError::UnsupportedBody(
            "a GPU kernel body is empty".to_string(),
        ));
    };
    Ok((scalar_stmt_list(stmts)?, tail))
}

/// Convert a Kāra statement slice into the kernel statement model, rejecting
/// anything outside the supported subset with a diagnostic that names it.
fn scalar_stmt_list(stmts: &[Stmt]) -> Result<Vec<KStmt<'_>>, WgslError> {
    let mut out = Vec::new();
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Let {
                is_mut,
                pattern,
                ty,
                value,
            } => {
                let PatternKind::Binding(name) = &pattern.kind else {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `let` must bind a simple name (no destructuring)".to_string(),
                    ));
                };
                // `let y = if c { <stmts> v } else { … };` — the branches carry
                // statements, so the value-`if`'s `select()` cannot express it
                // (a `select` operand is one expression). Hoist a `var` and let
                // each arm ASSIGN into it, which is exactly `KStmt::If`.
                if let ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } = &value.kind
                {
                    if value_if_carries_statements(value) {
                        let Some(ty) = ty else {
                            return Err(WgslError::UnsupportedBody(format!(
                                "a GPU kernel's `let {name} = if …` needs a type annotation when a \
                                 branch declares a local (`let {name}: f32 = if …`) — the \
                                 declaration is hoisted above the `if`, and WGSL cannot infer a \
                                 type there"
                            )));
                        };
                        out.push(KStmt::DeclareVar {
                            name: name.as_str(),
                            wgsl_ty: wgsl_scalar(ty, "binding")?,
                        });
                        out.push(stmt_if_assigning(
                            condition,
                            then_block,
                            else_branch,
                            name.as_str(),
                        )?);
                        continue;
                    }
                }
                out.push(KStmt::Bind {
                    name: name.as_str(),
                    mutable: *is_mut,
                    value,
                });
            }
            StmtKind::Assign { target, value } => {
                let target = assign_target_name(target)?;
                // Same desugar, but the destination already exists — so unlike
                // the `let` form this one needs no annotation at all.
                if let ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } = &value.kind
                {
                    if value_if_carries_statements(value) {
                        out.push(stmt_if_assigning(
                            condition,
                            then_block,
                            else_branch,
                            target,
                        )?);
                        continue;
                    }
                }
                out.push(KStmt::Assign {
                    target,
                    op: None,
                    value,
                });
            }
            StmtKind::CompoundAssign { target, op, value } => {
                out.push(KStmt::Assign {
                    target: assign_target_name(target)?,
                    op: Some(compound_assign_op(op)?),
                    value,
                });
            }
            StmtKind::Expr(Expr {
                kind:
                    ExprKind::For {
                        pattern,
                        iterable,
                        body,
                        ..
                    },
                ..
            }) => {
                let PatternKind::Binding(var) = &pattern.kind else {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `for` must bind a simple loop variable (no destructuring)"
                            .to_string(),
                    ));
                };
                // Only an integer range is iterable in a scalar kernel: there is
                // no collection to walk — the buffer element is the parameter.
                let ExprKind::Range {
                    start,
                    end,
                    inclusive,
                } = &iterable.kind
                else {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `for` must iterate a range (`for i in 0..n`) — there is no \
                         collection to iterate inside a kernel"
                            .to_string(),
                    ));
                };
                let (Some(start), Some(end)) = (start.as_deref(), end.as_deref()) else {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `for` needs both range bounds (`for i in 0..n`)".to_string(),
                    ));
                };
                if body.final_expr.is_some() {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `for` body must not produce a value".to_string(),
                    ));
                }
                out.push(KStmt::ForRange {
                    var: var.as_str(),
                    start,
                    end,
                    inclusive: *inclusive,
                    body: scalar_stmt_list(&body.stmts)?,
                });
            }
            StmtKind::Expr(Expr {
                kind: ExprKind::While {
                    condition, body, ..
                },
                ..
            }) => {
                if body.final_expr.is_some() {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `while` body must not produce a value".to_string(),
                    ));
                }
                out.push(KStmt::While {
                    cond: condition,
                    body: scalar_stmt_list(&body.stmts)?,
                });
            }
            StmtKind::Expr(Expr {
                kind:
                    ExprKind::If {
                        condition,
                        then_block,
                        else_branch,
                    },
                ..
            }) => {
                out.push(stmt_if(condition, then_block, else_branch)?);
            }
            _ => {
                return Err(WgslError::UnsupportedBody(
                    "a GPU kernel body supports `let` bindings, assignments, `if`, `while` and \
                     `for` before the final expression"
                        .to_string(),
                ));
            }
        }
    }
    Ok(out)
}

/// The assignable local's name. Only a bare identifier is assignable in a
/// kernel: the parameter is a read-only storage load, and field / index targets
/// need the struct paths the SoA emitter owns.
fn assign_target_name(target: &Expr) -> Result<&str, WgslError> {
    match &target.kind {
        ExprKind::Identifier(name) => Ok(name.as_str()),
        _ => Err(WgslError::UnsupportedBody(
            "a GPU kernel can only assign to a local `let mut` binding".to_string(),
        )),
    }
}

/// WGSL spelling of a compound-assignment operator.
fn compound_assign_op(op: &CompoundOp) -> Result<&'static str, WgslError> {
    match op {
        CompoundOp::Add => Ok("+="),
        CompoundOp::Sub => Ok("-="),
        CompoundOp::Mul => Ok("*="),
        CompoundOp::Div => Ok("/="),
        CompoundOp::Mod => Ok("%="),
        // The bitwise / shift compounds have WGSL spellings, but the scalar
        // kernel subset has no bitwise operators yet (`binop_str` rejects
        // them), so accepting them here would emit unreachable shapes.
        CompoundOp::BitAnd
        | CompoundOp::BitOr
        | CompoundOp::BitXor
        | CompoundOp::Shl
        | CompoundOp::Shr => Err(WgslError::UnsupportedBody(
            "a GPU kernel does not support bitwise compound assignment yet".to_string(),
        )),
    }
}

/// Emit a statement list into `out` at `indent` levels of four spaces.
/// Whether a value-`if` has any branch that declares locals, i.e. whose arms
/// are not each a single expression.
///
/// This is the fork between the two value-`if` lowerings. A plain one stays a
/// branchless `select(else, then, cond)`; only this shape needs the hoisted-var
/// desugar, because a `select` operand is ONE expression and cannot carry a
/// `let`. Keeping the cheap form on the cheap path matters — `select` is
/// divergence-free, which is what a GPU wants.
fn value_if_carries_statements(e: &Expr) -> bool {
    let ExprKind::If {
        then_block,
        else_branch,
        ..
    } = &e.kind
    else {
        return false;
    };
    if !then_block.stmts.is_empty() {
        return true;
    }
    match else_branch.as_deref() {
        Some(Expr {
            kind: ExprKind::Block(b),
            ..
        }) => !b.stmts.is_empty(),
        // An `else if` chain: any arm carrying statements forces the desugar.
        Some(
            nested @ Expr {
                kind: ExprKind::If { .. },
                ..
            },
        ) => value_if_carries_statements(nested),
        _ => false,
    }
}

/// Build a [`KStmt::If`] whose every arm ends by ASSIGNING its branch value to
/// `target` — the desugar that turns a value-`if` with locals into the
/// statement form (B-2026-08-18-49 step 2).
///
/// `let y: f32 = if c { let t = x * 2.0; t } else { x };` becomes
/// `var y: f32; if (c) { let t = …; y = t; } else { y = input[i]; }`.
///
/// Note this is strictly MORE faithful than `select`, not merely equivalent:
/// only the taken arm's statements run, where `select` evaluates both. Both are
/// sound under the effect gate; this one also matches the interpreter's
/// evaluation order.
fn stmt_if_assigning<'a>(
    condition: &'a Expr,
    then_block: &'a Block,
    else_branch: &'a Option<Box<Expr>>,
    target: &'a str,
) -> Result<KStmt<'a>, WgslError> {
    let arm = |b: &'a Block| -> Result<Vec<KStmt<'a>>, WgslError> {
        let mut body = scalar_stmt_list(&b.stmts)?;
        let value = b.final_expr.as_deref().ok_or_else(|| {
            WgslError::UnsupportedBody(
                "a GPU `if` branch bound to a value must end in an expression".to_string(),
            )
        })?;
        body.push(KStmt::Assign {
            target,
            op: None,
            value,
        });
        Ok(body)
    };

    let then_body = arm(then_block)?;
    let else_body = match else_branch.as_deref() {
        None => {
            return Err(WgslError::UnsupportedBody(
                "a GPU `if` must have an `else` — it produces a value".to_string(),
            ));
        }
        Some(e) => match &e.kind {
            ExprKind::Block(b) => Some(arm(b)?),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => Some(vec![stmt_if_assigning(
                condition,
                then_block,
                else_branch,
                target,
            )?]),
            // `else <expr>` with no block of its own.
            _ => Some(vec![KStmt::Assign {
                target,
                op: None,
                value: e,
            }]),
        },
    };
    Ok(KStmt::If {
        cond: condition,
        then_body,
        else_body,
    })
}

/// Build a [`KStmt::If`] from the pieces of an `ExprKind::If` in statement
/// position, recursing through an `else if` chain.
///
/// A branch that produces a VALUE is rejected here rather than silently
/// discarded: in statement position its value would go nowhere, and the shape
/// that means to produce one (`let y = if c { a } else { b };`) is already
/// handled by the value-`if` lowering, which is a `select()`.
fn stmt_if<'a>(
    condition: &'a Expr,
    then_block: &'a Block,
    else_branch: &'a Option<Box<Expr>>,
) -> Result<KStmt<'a>, WgslError> {
    let branch_stmts = |b: &'a Block, which: &str| -> Result<Vec<KStmt<'a>>, WgslError> {
        if b.final_expr.is_some() {
            return Err(WgslError::UnsupportedBody(format!(
                "a GPU kernel's `{which}` branch must not produce a value when the `if` is a \
                 statement — bind it instead (`let y = if c {{ a }} else {{ b }};`)"
            )));
        }
        scalar_stmt_list(&b.stmts)
    };

    let then_body = branch_stmts(then_block, "if")?;
    let else_body = match else_branch.as_deref() {
        None => None,
        Some(e) => match &e.kind {
            ExprKind::Block(b) => Some(branch_stmts(b, "else")?),
            // `else if` — one nested statement, so the emitter's recursion
            // produces WGSL's own `else if` spelling without extra machinery.
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => Some(vec![stmt_if(condition, then_block, else_branch)?]),
            _ => {
                return Err(WgslError::UnsupportedBody(
                    "a GPU kernel's `else` must be a block or another `if`".to_string(),
                ));
            }
        },
    };
    Ok(KStmt::If {
        cond: condition,
        then_body,
        else_body,
    })
}

fn emit_stmts<'a>(
    stmts: &[KStmt<'a>],
    scope: &mut Scope<'a>,
    helpers: &HashSet<String>,
    indent: usize,
    out: &mut String,
) -> Result<(), WgslError> {
    let pad = "    ".repeat(indent);
    for stmt in stmts {
        match stmt {
            KStmt::Bind {
                name,
                mutable,
                value,
            } => {
                let rhs = lower_expr(value, &scope.resolver(), helpers)?;
                let kw = if *mutable { "var" } else { "let" };
                let wgsl = scope.bind(name, *mutable);
                out.push_str(&format!("{pad}{kw} {wgsl} = {rhs};\n"));
            }
            KStmt::Assign { target, op, value } => {
                let Some((_, wgsl, mutable)) = scope.lookup(target) else {
                    return Err(WgslError::UnsupportedBody(format!(
                        "a GPU kernel cannot assign to `{target}` — it is not a local binding"
                    )));
                };
                if !*mutable {
                    return Err(WgslError::UnsupportedBody(format!(
                        "a GPU kernel cannot assign to the immutable local `{target}` — \
                         declare it `let mut`"
                    )));
                }
                let wgsl = wgsl.clone();
                let rhs = lower_expr(value, &scope.resolver(), helpers)?;
                let op = op.unwrap_or("=");
                out.push_str(&format!("{pad}{wgsl} {op} {rhs};\n"));
            }
            KStmt::While { cond, body } => {
                let c = lower_expr(cond, &scope.resolver(), helpers)?;
                out.push_str(&format!("{pad}while ({c}) {{\n"));
                // A binding inside the loop body is scoped to it.
                let depth = scope.depth();
                emit_stmts(body, scope, helpers, indent + 1, out)?;
                scope.truncate(depth);
                out.push_str(&format!("{pad}}}\n"));
            }
            KStmt::ForRange {
                var,
                start,
                end,
                inclusive,
                body,
            } => {
                // Both bounds are evaluated in the ENCLOSING scope, before the
                // loop variable exists — `for i in 0..i` would otherwise read
                // the variable being declared.
                let lo = lower_expr(start, &scope.resolver(), helpers)?;
                let hi = lower_expr(end, &scope.resolver(), helpers)?;
                let depth = scope.depth();
                // Kāra's loop variable is immutable, so it is bound non-mutable
                // (assigning to it is rejected) even though WGSL needs a `var`
                // to carry the increment.
                let v = scope.bind(var, false);
                let cmp = if *inclusive { "<=" } else { "<" };
                out.push_str(&format!(
                    "{pad}for (var {v} = {lo}; {v} {cmp} {hi}; {v} = {v} + 1) {{\n"
                ));
                emit_stmts(body, scope, helpers, indent + 1, out)?;
                scope.truncate(depth);
                out.push_str(&format!("{pad}}}\n"));
            }
            KStmt::DeclareVar { name, wgsl_ty } => {
                // Bound MUTABLE so the desugared arms may assign into it. Kāra's
                // own `let`-immutability is enforced upstream by the
                // typechecker; this lowering is not the mutability checker, and
                // treating the hoisted slot as immutable here would reject the
                // very assignment the desugar just generated.
                let wgsl = scope.bind(name, true);
                out.push_str(&format!("{pad}var {wgsl}: {wgsl_ty};\n"));
            }
            KStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let c = lower_expr(cond, &scope.resolver(), helpers)?;
                out.push_str(&format!("{pad}if ({c}) {{\n"));
                // Each branch is its own scope, exactly as a loop body is: a
                // `let` in the `then` arm must not be visible in `else` or
                // after the `if`.
                let depth = scope.depth();
                emit_stmts(then_body, scope, helpers, indent + 1, out)?;
                scope.truncate(depth);
                match else_body.as_deref() {
                    None => out.push_str(&format!("{pad}}}\n")),
                    // An `else if` arrives as one nested `KStmt::If`. Emit it
                    // into a scratch buffer at this indent and splice it after
                    // `} else `, which yields WGSL's flat `} else if (c) {`
                    // rather than a block nested one level deeper per arm.
                    Some(nested @ [KStmt::If { .. }]) => {
                        let mut chained = String::new();
                        let depth = scope.depth();
                        emit_stmts(nested, scope, helpers, indent, &mut chained)?;
                        scope.truncate(depth);
                        out.push_str(&format!("{pad}}} else {}", chained.trim_start()));
                    }
                    Some(stmts) => {
                        out.push_str(&format!("{pad}}} else {{\n"));
                        let depth = scope.depth();
                        emit_stmts(stmts, scope, helpers, indent + 1, out)?;
                        scope.truncate(depth);
                        out.push_str(&format!("{pad}}}\n"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Extract the kernel's sole parameter, rejecting the zero-param and
/// multi-param shapes (multi-buffer dispatch is a later increment).
fn kernel_param(func: &Function) -> Result<&Param, WgslError> {
    if func.self_param.is_some() {
        return Err(WgslError::UnsupportedSignature(
            "a GPU kernel cannot take a self receiver".to_string(),
        ));
    }
    match func.params.as_slice() {
        [p] => Ok(p),
        [] => Err(WgslError::UnsupportedSignature(
            "a GPU kernel must take exactly one f32 parameter (slice-0)".to_string(),
        )),
        _ => Err(WgslError::UnsupportedSignature(format!(
            "a GPU kernel takes exactly one parameter in slice-0, found {}",
            func.params.len()
        ))),
    }
}

/// Map a Kāra scalar `TypeExpr` to its WGSL scalar-type spelling, or reject it.
/// Slice-0 supports the three WGSL-native 4-byte numeric scalars — `f32`,
/// `i32`, `u32` (WGSL has no native `i64`/`f64`, and `f16` needs an extension,
/// so those stay later increments). The Kāra and WGSL spellings coincide.
fn wgsl_scalar(ty: &TypeExpr, position: &str) -> Result<&'static str, WgslError> {
    match scalar_name(ty).as_deref() {
        Some("f32") => Ok("f32"),
        Some("i32") => Ok("i32"),
        Some("u32") => Ok("u32"),
        _ => Err(WgslError::UnsupportedSignature(format!(
            "the GPU kernel {position} must be f32, i32, or u32 in slice-0"
        ))),
    }
}

/// The single-segment type name of a scalar `TypeExpr` (`f32`, `i32`, …), or
/// `None` for any compound / generic / qualified type.
fn scalar_name(ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeKind::Path(path) if path.generic_args.is_none() && path.segments.len() == 1 => {
            Some(path.segments[0].clone())
        }
        _ => None,
    }
}

/// Whether `ty` is a `Vec[...]` — a stencil kernel's whole-buffer parameter
/// (GPU-LBM-6), as opposed to an element struct `S`. The distinguishing signal
/// that routes [`emit_kernel_soa`] to the stencil emitter.
fn is_vec_type(ty: &TypeExpr) -> bool {
    matches!(
        &ty.kind,
        TypeKind::Path(p) if p.segments.len() == 1 && p.segments[0] == "Vec"
    )
}

/// Lower one scalar body expression to a WGSL text fragment. `resolve` maps an
/// identifier to its WGSL (the kernel's sole param → `input[i]`; a helper's params
/// → themselves); `helpers` is the set of reachable `#[gpu]` helper names (for
/// call recognition). Handles both the pre-lowering `Binary` operator form (the
/// scalar kernel emitter runs at typecheck) and the post-lowering `<type>.<op>`
/// call form (helper bodies on the SoA/codegen path), plus `#[gpu]` helper calls.
fn lower_expr(
    expr: &Expr,
    resolve: &dyn Fn(&str) -> Option<String>,
    helpers: &HashSet<String>,
) -> Result<String, WgslError> {
    match &expr.kind {
        ExprKind::Identifier(name) => resolve(name).ok_or_else(|| {
            WgslError::UnsupportedBody(format!("unknown identifier '{name}' in a GPU kernel"))
        }),
        ExprKind::Integer(n, _) => Ok(n.to_string()),
        ExprKind::Float(f, _) => lower_float(*f),
        ExprKind::Binary { op, left, right } => {
            let op_str = binop_str(op)?;
            let l = lower_expr(left, resolve, helpers)?;
            let r = lower_expr(right, resolve, helpers)?;
            Ok(format!("({l} {op_str} {r})"))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            let inner = lower_expr(operand, resolve, helpers)?;
            Ok(format!("-({inner})"))
        }
        // Value `if c { a } else { b }` → WGSL `select(b, a, c)` (GPU-LBM-4).
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_expr(condition, resolve, helpers)?;
            let t = lower_expr(then_e, resolve, helpers)?;
            let e = lower_expr(else_e, resolve, helpers)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        // Value `match` → a nested `select()` chain, the same branchless shape
        // the value-`if` above uses (B-2026-08-18-40 increment 4). WGSL's own
        // `switch` is a STATEMENT, so it cannot produce the value a Kāra `match`
        // is; `select` can, and it composes into every expression position at
        // once — a `let` initializer, an assignment RHS, a loop body, the tail.
        ExprKind::Match { scrutinee, arms } => lower_match(scrutinee, arms, resolve, helpers),
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, &|e| lower_expr(e, resolve, helpers), helpers)
        }
        // Scalar math intrinsic method (`e.sqrt()` → `sqrt(e)`) — GPU-SLIP-2a.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            // Wrapping integer arithmetic lowers to the BARE INFIX OPERATOR —
            // which is exactly what WGSL integer `+`/`-`/`*` already mean
            // (overflow wraps; there is no trap to emit). B-2026-08-19-1: this
            // is the spelling that lets a kernel say it means wraparound, now
            // that bare `+` on integers is rejected here. The two directions
            // matter together: without this arm the honest spelling would be
            // refused while the silent one was accepted, which is the state the
            // bug describes.
            if let Some(op) = wrapping_infix_wgsl(method, args.len()) {
                let lhs = lower_expr(object, resolve, helpers)?;
                let rhs = lower_expr(&args[0].value, resolve, helpers)?;
                return Ok(format!("({lhs} {op} {rhs})"));
            }
            let builtin = math_intrinsic_wgsl(method, args.len()).ok_or_else(|| {
                WgslError::UnsupportedBody(format!(
                    "method `.{method}()` is not supported in a GPU kernel body"
                ))
            })?;
            Ok(format!(
                "{builtin}({})",
                lower_expr(object, resolve, helpers)?
            ))
        }
        // Numeric `as` cast (`e as f64` → `f32(e)`) — GPU-SLIP-2a.
        ExprKind::Cast { expr, ty } => {
            let ctor = cast_ctor(ty).ok_or_else(|| {
                WgslError::UnsupportedBody(
                    "unsupported `as` cast target in a GPU kernel body".to_string(),
                )
            })?;
            Ok(format!("{ctor}({})", lower_expr(expr, resolve, helpers)?))
        }
        _ => Err(WgslError::UnsupportedBody(
            "unsupported expression in a GPU kernel body (numeric literals, `+ - * / %`, \
             unary `-`, comparisons, value `if`/`else`, `.sqrt()`, `as` casts, `#[gpu]` \
             helper calls)"
                .to_string(),
        )),
    }
}

/// The WGSL spelling of a binary arithmetic or comparison operator. Comparisons
/// (used only inside an `if` condition — GPU-LBM-4) produce `bool`; logical /
/// bitwise operators remain out of scope.
fn binop_str(op: &BinOp) -> Result<&'static str, WgslError> {
    match op {
        BinOp::Add => Ok("+"),
        BinOp::Sub => Ok("-"),
        BinOp::Mul => Ok("*"),
        BinOp::Div => Ok("/"),
        BinOp::Mod => Ok("%"),
        BinOp::Gt => Ok(">"),
        BinOp::Lt => Ok("<"),
        BinOp::GtEq => Ok(">="),
        BinOp::LtEq => Ok("<="),
        BinOp::Eq => Ok("=="),
        BinOp::NotEq => Ok("!="),
        // Short-circuit logical operators (bool operands) — GPU-SLIP-2a. WGSL
        // `&&`/`||` mirror Kāra `and`/`or`; used to compose the `stream` boundary
        // and bounce-back conditions (`x == 0 or … or is_solid(…)`).
        BinOp::And => Ok("&&"),
        BinOp::Or => Ok("||"),
        _ => Err(WgslError::UnsupportedBody(
            "only arithmetic (`+ - * / %`), comparison (`> < >= <= == !=`), and logical \
             (`and` / `or`) operators are supported in a GPU kernel"
                .to_string(),
        )),
    }
}

/// Emit the tree-reduction shader for `gpu.sum(buf)` / `gpu.prod(buf)`
/// (B-2026-08-19-10, slice 1).
///
/// **This function defines language semantics, not just codegen.** A GPU
/// reduction is a TREE, a CPU fold is a LINE, and `f32` addition is not
/// associative — so the two disagree on real inputs (64 copies of `0.1` give
/// `6.400000` here and `6.399996` under a left fold). Kāra specifies the tree
/// order and the interpreter reproduces it, so `karac run` and `karac build`
/// agree bit-for-bit rather than within an epsilon; the alternative would have
/// been an epsilon-tolerant oracle, which weakens the A/B rule that
/// kara-katas/CLAUDE.md calls non-negotiable.
///
/// The order the interpreter twin must match exactly: pad to
/// [`WORKGROUP_SIZE`] with the operation's identity, then halve —
/// `s[t] = s[t] OP s[t + stride]` for stride 32, 16, 8, 4, 2, 1. Lane 0 of
/// each workgroup writes THAT workgroup's partial to `output[workgroup_id]`.
///
/// **One dispatch is one level of the tree, not the whole reduction.** A
/// buffer longer than one workgroup leaves `ceil(n / WORKGROUP_SIZE)` partials
/// behind, and the host re-dispatches this same shader over them until one
/// value remains. So a long reduction is a TREE OF TREES whose grouping is
/// observable in `f32` — which is why the width is fixed in `reduce_kernel`
/// alongside the twin rather than chosen here, and why the twin recurses the
/// same way (`reduce_kernel::tree_fold_f32`).
pub fn emit_reduce_kernel(op: ReduceOp, elem: &str) -> Result<String, WgslError> {
    let (prelude, combine, identity) = reduce_combine_wgsl(op, elem)?;
    // One width, defined in `reduce_kernel` — the shader, its scratch array
    // and the CPU twin's padding must agree or the answer changes.
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{elem}>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         {prelude}\n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   if (i < arrayLength(&input)) {{ scratch[t] = input[i]; }} else {{ scratch[t] = {identity}; }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{ scratch[t] = {combine}; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // Each workgroup writes ITS OWN partial. With one workgroup\n\
         \x20   // that is slot 0 and the answer; with several the host folds\n\
         \x20   // the partials by dispatching this same shader over them.\n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; }}\n\
         }}\n"
    ))
}

/// Emit the tree-reduction shader for a reduction over ONE FIELD of a resident
/// `gpu.Buffer[S]` — `gpu.sum(buf.mass)` (GPU-SLIP-4b-3).
///
/// Identical in tree order to [`emit_reduce_kernel`]; the only difference is
/// where a lane finds its element. A resident buffer is stored per LAYOUT
/// GROUP, so one group's device buffer holds `n` records of `stride` f32s and
/// the requested field sits at `offset` within each. `stride == 1 && offset ==
/// 0` (the one-field-per-group default) degenerates to a contiguous read.
///
/// **The tree order is deliberately shared, not merely similar.** The oracle
/// for a resident field reduction is the round-trip reduction of that same
/// field — `gpu.sum(buf.mass)` must equal `gpu.sum(download(buf).map(|r| r.mass))`
/// BIT-FOR-BIT, not within an epsilon. That holds only because both walk the
/// same padded-halving tree at the same [`GPU_REDUCE_WIDTH`] and chunk into
/// partials the same way, so f32 non-associativity lands identically on both
/// sides. Any divergence here (a different width, an unpadded tail, a
/// strided-only fast path that folds pairs in a different order) would turn an
/// exact oracle into an approximate one.
///
/// **`arrayLength` is in f32s, not in records.** The bound is therefore
/// `arrayLength(&input) / stride`, and reading it any other way would let the
/// last workgroup of an interleaved group run off the end of the record grid
/// (or, with `stride` folded in twice, silently reduce a prefix).
pub fn emit_reduce_field_kernel(
    op: ReduceOp,
    elem: &str,
    stride: u32,
    offset: u32,
) -> Result<String, WgslError> {
    if stride == 0 {
        return Err(WgslError::UnsupportedBody(
            "a resident field reduction needs a non-zero group stride".to_string(),
        ));
    }
    if offset >= stride {
        return Err(WgslError::UnsupportedBody(format!(
            "field offset {offset} is outside its layout group's stride {stride}"
        )));
    }
    let (prelude, combine, identity) = reduce_combine_wgsl(op, elem)?;
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{elem}>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         {prelude}\n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   // `arrayLength` counts f32s; the grid is records of {stride}.\n\
         \x20   let n = arrayLength(&input) / {stride}u;\n\
         \x20   if (i < n) {{ scratch[t] = input[i * {stride}u + {offset}u]; }} else {{ scratch[t] = {identity}; }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{ scratch[t] = {combine}; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // Level 0 only. The partials this leaves behind are CONTIGUOUS,\n\
         \x20   // so the host folds them with the ordinary contiguous kernel.\n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; }}\n\
         }}\n"
    ))
}

/// The NaN predicate every float reduction shares — a BIT-PATTERN test rather
/// than `x != x` (B-2026-08-20-2).
///
/// **`x != x` does not survive Metal.** MSL is compiled with fast-math by
/// default, which licenses the compiler to assume no NaN exists and fold
/// `x == x` to `true` — silently deleting every guard written that way. It was
/// measured, not theorised: `gpu.argmin([NaN, 3.0, 1.0])` answered `0` on
/// Metal (the NaN won) where lavapipe and the interpreter agreed on `2`.
///
/// A float is NaN exactly when its exponent is all ones and its mantissa is
/// non-zero, i.e. when its bits with the sign cleared exceed `+inf`'s. Integer
/// arithmetic carries no fast-math licence, so this survives every backend —
/// and `bitcast` is already proven on both, since the ±∞ identities use it.
const NAN_PREDICATE_WGSL: &str = "fn karac_is_nan(x: f32) -> bool {\n\
     \x20   return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;\n\
     }";

/// Emit phase 1 of `gpu.prefix_sum(buf)`: each workgroup scans its own
/// [`GPU_REDUCE_WIDTH`]-element chunk in place and records that chunk's total
/// (B-2026-08-19-13).
///
/// **The first GPU op here that is not a fold.** Every reduction before it
/// converged to one value; a prefix sum produces `n` of them, so the shader
/// writes a full-width output and the host never gets to stop at "one value
/// remains". That is why the row tracked it as a separate project rather than
/// another combine string.
///
/// The order is Hillis-Steele and it is language semantics, specified in
/// [`reduce_kernel::tree_prefix_sum_f32`]: for stride 1, 2, 4, 8, 16, 32,
/// every lane at or past `stride` adds the lane `stride` below it, and every
/// lane reads the values as they stood BEFORE the step.
///
/// **The read-barrier-write-barrier pair is the whole correctness argument.**
/// A single barrier per step is not enough and the bug it admits is subtle:
/// without the FIRST barrier a lane could write `scratch[t]` before a higher
/// lane has read it, so the higher lane adds an already-updated value and
/// double-counts an element. The result stays monotone and still looks like a
/// prefix sum. The CPU twin expresses the same constraint as a `prev` copy.
///
/// Two outputs, not one: the scanned chunk AND its total, because phase 2
/// prefix-sums the totals and phase 3 adds them back. Lane 63 holds the total
/// unconditionally — a short chunk is padded with `0.0`, the Sum identity, so
/// the padding contributes nothing.
pub fn emit_scan_kernel(elem: &str) -> Result<String, WgslError> {
    if elem != "f32" {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `prefix_sum` over `{elem}` is not supported yet — it is f32-only. \
             An integer prefix sum has to carry the overflow flag of \
             `emit_int_reduce_kernel` through every lane, not just lane 0"
        )));
    }
    let width = GPU_REDUCE_WIDTH;
    let last = width - 1;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:   array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output:  array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> totals:  array<{elem}>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   // Every lane loads, including the padding lanes — the scan\n\
         \x20   // reads across the whole width, so a lane left uninitialised\n\
         \x20   // would poison its neighbours. 0.0 is the Sum identity.\n\
         \x20   if (i < arrayLength(&input)) {{ scratch[t] = input[i]; }} else {{ scratch[t] = 0.0; }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = 1u;\n\
         \x20   loop {{\n\
         \x20       if (stride >= {width}u) {{ break; }}\n\
         \x20       // READ first, for every lane, then barrier, then write.\n\
         \x20       // One barrier per step would let a low lane's new value\n\
         \x20       // reach a high lane within the same step, double-counting\n\
         \x20       // an element while still looking like a prefix sum.\n\
         \x20       var addend: {elem} = 0.0;\n\
         \x20       if (t >= stride) {{ addend = scratch[t - stride]; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       if (t >= stride) {{ scratch[t] = scratch[t] + addend; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride * 2u;\n\
         \x20   }}\n\
         \n\
         \x20   if (i < arrayLength(&input)) {{ output[i] = scratch[t]; }}\n\
         \x20   // Lane {last} always holds this chunk's total: a short chunk is\n\
         \x20   // padded with the identity, so the padding adds nothing.\n\
         \x20   if (t == {last}u) {{ totals[wg] = scratch[{last}]; }}\n\
         }}\n"
    ))
}

/// Emit the CHECKED INTEGER phase-1 scan for `gpu.prefix_sum`
/// (B-2026-08-19-13).
///
/// Same Hillis-Steele step order as [`emit_scan_kernel`], with one structural
/// difference that is the whole reason this is a separate emitter rather than
/// a combine string:
///
/// **THE OVERFLOW FLAG IS OR'D ACROSS EVERY LANE, not read from lane 0.**
/// Every checked kernel above this one is a REDUCTION, where only lane 0
/// survives and a lane above the stride holds a partial nobody reads — so
/// `ovf[0]` after the fold is the whole story. A scan writes all `n` values,
/// so an overflow in any lane is an overflow in the ANSWER. Inheriting the
/// reduction's habit here would silently drop overflows in exactly the
/// elements the caller asked for.
///
/// **Padded lanes are checked too, and that is not conservatism.** A lane past
/// the buffer starts at the identity, but the scan sweeps real values into it,
/// so by the last step it holds the CHUNK TOTAL — which phase 2 folds and
/// every later chunk's offset depends on. Its overflow is a real overflow of a
/// real quantity.
///
/// The two-barrier read/write split is [`emit_scan_kernel`]'s and is preserved
/// exactly: read every addend first, barrier, then write. One barrier per step
/// would let a low lane's new value reach a high lane within the same step,
/// double-counting an element while still looking like a prefix sum.
pub fn emit_int_scan_kernel(elem: &str) -> Result<String, WgslError> {
    let (identity, add) = match elem {
        "i32" => (
            "0",
            "let s = scratch[t] + addend;\n\
             \x20           ovf[t] = ovf[t] | \
             select(0u, 1u, ((scratch[t] ^ s) & (addend ^ s)) < 0);\n\
             \x20           scratch[t] = s;",
        ),
        "u32" => (
            "0u",
            "let s = scratch[t] + addend;\n\
             \x20           ovf[t] = ovf[t] | select(0u, 1u, s < scratch[t]);\n\
             \x20           scratch[t] = s;",
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU `prefix_sum` over `{elem}` is not supported — the \
                 integer entry points cover i32 and u32"
            )))
        }
    };
    let width = GPU_REDUCE_WIDTH;
    let last = width - 1;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:   array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output:  array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> totals:  array<{elem}>;\n\
         @group(0) @binding(3) var<storage, read_write> flags:   array<u32>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         var<workgroup> ovf: array<u32, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   ovf[t] = 0u;\n\
         \x20   // Every lane loads, padding included — the scan reads across\n\
         \x20   // the whole width, so an uninitialised lane would poison its\n\
         \x20   // neighbours. 0 is the Sum identity.\n\
         \x20   if (i < arrayLength(&input)) {{ scratch[t] = input[i]; }} else {{ scratch[t] = {identity}; }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = 1u;\n\
         \x20   loop {{\n\
         \x20       if (stride >= {width}u) {{ break; }}\n\
         \x20       // READ first, for every lane, then barrier, then write.\n\
         \x20       var addend: {elem} = {identity};\n\
         \x20       if (t >= stride) {{ addend = scratch[t - stride]; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       if (t >= stride) {{\n\
         \x20           {add}\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride * 2u;\n\
         \x20   }}\n\
         \n\
         \x20   if (i < arrayLength(&input)) {{ output[i] = scratch[t]; }}\n\
         \x20   if (t == {last}u) {{ totals[wg] = scratch[{last}]; }}\n\
         \x20   workgroupBarrier();\n\
         \x20   // ONE lane ORs the whole workgroup's flags. A scan's every\n\
         \x20   // lane holds a live output, so `ovf[0]` alone would miss the\n\
         \x20   // overflows that matter most.\n\
         \x20   if (t == 0u) {{\n\
         \x20       var any: u32 = 0u;\n\
         \x20       var q: u32 = 0u;\n\
         \x20       loop {{\n\
         \x20           if (q >= {width}u) {{ break; }}\n\
         \x20           any = any | ovf[q];\n\
         \x20           q = q + 1u;\n\
         \x20       }}\n\
         \x20       flags[wg] = any;\n\
         \x20   }}\n\
         }}\n"
    ))
}

/// Emit the CHECKED INTEGER phase 3 of `gpu.prefix_sum`: shift every chunk by
/// the total of all chunks before it (B-2026-08-19-13).
///
/// **The offset add is checked, and it is the step most easily forgotten.**
/// Phases 1 and 2 look like "the arithmetic" and this one looks like
/// bookkeeping, but `scanned[i] + offset` adds two real values and overflows
/// exactly as readily — it is where a long buffer's total finally exceeds the
/// range, since that is the only place a late chunk meets an early chunk's
/// sum.
pub fn emit_int_scan_offset_kernel(elem: &str) -> Result<String, WgslError> {
    let (identity, add) = match elem {
        "i32" => (
            "0",
            "let s = scanned[i] + off;\n\
             \x20   let bad = ((scanned[i] ^ s) & (off ^ s)) < 0;",
        ),
        "u32" => (
            "0u",
            "let s = scanned[i] + off;\n\
             \x20   let bad = s < scanned[i];",
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU `prefix_sum` over `{elem}` is not supported — the \
                 integer entry points cover i32 and u32"
            )))
        }
    };
    let width = GPU_REDUCE_WIDTH;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       scanned: array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       offsets: array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output:  array<{elem}>;\n\
         @group(0) @binding(3) var<storage, read_write> flags:   array<u32>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(local_invocation_id) lid: vec3<u32>) {{\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   // Lane 0 clears its workgroup's flag word before any lane can\n\
         \x20   // set it. Without this a workgroup whose threads all return\n\
         \x20   // early would leave the slot unwritten.\n\
         \x20   if (lid.x == 0u) {{ flags[wg] = 0u; }}\n\
         \x20   workgroupBarrier();\n\
         \x20   if (i >= arrayLength(&scanned)) {{ return; }}\n\
         \x20   let c = i / {width}u;\n\
         \x20   // `offsets` is the INCLUSIVE prefix of the chunk totals, so\n\
         \x20   // chunk c's EXCLUSIVE offset is one position back. Chunk 0\n\
         \x20   // has nothing before it.\n\
         \x20   var off: {elem} = {identity};\n\
         \x20   if (c > 0u) {{ off = offsets[c - 1u]; }}\n\
         \x20   {add}\n\
         \x20   output[i] = s;\n\
         \x20   // Any lane may raise it; the host ORs across workgroups.\n\
         \x20   if (bad) {{ flags[wg] = 1u; }}\n\
         }}\n"
    ))
}

/// Emit phase 3 of `gpu.prefix_sum(buf)`: shift every chunk by the total of
/// all chunks before it (B-2026-08-19-13).
///
/// Phase 2 is not a shader — it is this whole three-phase dance run again, one
/// level up, over the per-chunk totals. So a long prefix sum is a prefix sum
/// OF PREFIX SUMS, the same self-similarity `tree_fold_f32` has, and the twin
/// recurses identically.
///
/// The offsets arriving here are the INCLUSIVE prefix of the chunk totals, so
/// chunk `c` wants element `c - 1` — the exclusive prefix is the inclusive one
/// read a position back. Chunk 0 has nothing before it and is left alone. That
/// off-by-one is the whole shader, which is why the twin asserts a
/// one-per-element step over a long all-ones buffer: a misindexed offset shows
/// up as a jump at a chunk boundary rather than as a wrong total.
///
/// Written as a copying pass (two reads, one write) rather than an in-place
/// update, because the runtime's `run_compute` allocates outputs rather than
/// aliasing an input — and an in-place variant would need the scanned buffer
/// bound as both, which the binding convention (inputs, then outputs, then
/// uniforms) has no way to express.
pub fn emit_scan_offset_kernel(elem: &str) -> Result<String, WgslError> {
    if elem != "f32" {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `prefix_sum` over `{elem}` is not supported yet — it is f32-only"
        )));
    }
    let width = GPU_REDUCE_WIDTH;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       scanned: array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       offsets: array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output:  array<{elem}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   if (i >= arrayLength(&scanned)) {{ return; }}\n\
         \x20   let c = i / {width}u;\n\
         \x20   // `offsets` is the INCLUSIVE prefix of the chunk totals, so\n\
         \x20   // chunk c's EXCLUSIVE offset is one position back. Chunk 0\n\
         \x20   // has nothing before it.\n\
         \x20   var off: {elem} = 0.0;\n\
         \x20   if (c > 0u) {{ off = offsets[c - 1u]; }}\n\
         \x20   output[i] = scanned[i] + off;\n\
         }}\n"
    ))
}

/// Emit the level-0 shader of a `gpu.variance` / `gpu.stddev` second pass:
/// square each element's deviation from the mean ON LOAD, then run the same
/// halving sum tree (B-2026-08-19-13).
///
/// **The first shader here that takes a UNIFORM.** Variance is genuinely two
/// passes — the mean has to exist before a single deviation can be formed —
/// so the host runs a complete sum reduction, reads the mean back, and
/// dispatches this with the mean bound at `@binding(2)`. Every reduction
/// before variance was a single converging fold.
///
/// The fusion is the one `gpu.dot` uses: the deviation is squared on load, so
/// no `n`-element intermediate is ever written. And like `dot`, only LEVEL 0
/// is special — the per-workgroup partials are folded by the ordinary sum
/// shader, which is what makes `gpu.variance` and "sum the squared deviations
/// yourself" the same number rather than merely close.
///
/// Binding order follows the runtime's convention — inputs, then outputs,
/// then uniforms — so a 1-in/1-out kernel puts its uniform at 2.
pub fn emit_deviation_kernel(elem: &str) -> Result<String, WgslError> {
    if elem != "f32" {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `variance`/`stddev` over `{elem}` is not supported yet — they are f32-only"
        )));
    }
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read>       mean_u: array<{elem}>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   // The ONLY difference from the sum shader: the squared\n\
         \x20   // deviation is formed on load, so no n-element intermediate\n\
         \x20   // is written. The mean arrives as a uniform because it cannot\n\
         \x20   // be known until a whole reduction has already finished.\n\
         \x20   if (i < arrayLength(&input)) {{\n\
         \x20       let d = input[i] - mean_u[0];\n\
         \x20       scratch[t] = d * d;\n\
         \x20   }} else {{\n\
         \x20       scratch[t] = 0.0;\n\
         \x20   }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{ scratch[t] = scratch[t] + scratch[t + stride]; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // One partial per workgroup; the host folds them with the\n\
         \x20   // plain SUM shader, exactly as the dot path does.\n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; }}\n\
         }}\n"
    ))
}

/// Emit an `argmin`/`argmax` tree-reduction shader (B-2026-08-19-13).
///
/// The Arg family is the one reduction whose scratch holds PAIRS: an index
/// alone cannot be compared and a value alone cannot be reported. That makes
/// it a genuinely different shader rather than a different combine string —
/// which is why it was deferred through four earlier slices.
///
/// **Two levels, two shaders, one index space.** `fold = false` emits the
/// level-0 kernel, where every element is its own candidate (`idx = gid.x`).
/// `fold = true` emits the kernel for every level after that, which takes the
/// surviving candidate indices at `@binding(1)` and re-reads their values from
/// the ORIGINAL buffer. Indices are therefore absolute at every level, and no
/// value is ever carried between dispatches — nothing can be lost or rounded
/// in transit, and the host never has to ship a second buffer of values back.
///
/// The combine is [`crate::reduce_kernel::arg_takes_b`]'s rule, verbatim:
/// strictly better value wins; on an exact tie the SMALLER index wins; a NaN
/// loses to anything real and ties with another NaN. That is a lexicographic
/// order, hence a semilattice, hence grouping-independent — `argmin` can
/// promise the same answer at every buffer length where `sum` cannot.
///
/// **Integers drop the NaN half and nothing else.** They are totally ordered,
/// so there is nothing for those two guards to catch. Signedness needs no
/// separate spelling either: WGSL's `<` and `>` are signed on an `array<i32>`
/// and unsigned on an `array<u32>`, so declaring the element type is what
/// makes `4294967295` the largest `u32` rather than `-1`. And unlike the value
/// reductions, the RESULT needs no widening decision at all — an index is a
/// `u32` whatever the buffer holds.
///
/// Padding is marked by the INDEX sentinel
/// ([`crate::reduce_kernel::ARG_INVALID`]), never by a value. A value sentinel
/// would have to be NaN to lose reliably, and NaN preservation is an OPTIONAL
/// Vulkan feature — a device that flushed it would silently let padding win
/// and report a nonexistent index.
pub fn emit_arg_kernel(op: ReduceOp, elem: &str, fold: bool) -> Result<String, WgslError> {
    if !matches!(elem, "f32" | "i32" | "u32") {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `argmin`/`argmax` over `{elem}` is not supported — the arg reductions cover \
             f32, i32 and u32"
        )));
    }
    let want_max = match op {
        ReduceOp::Argmin => false,
        ReduceOp::Argmax => true,
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "`{op:?}` is not an arg reduction — `emit_arg_kernel` covers `argmin` and `argmax`"
            )))
        }
    };
    let strict = if want_max { ">" } else { "<" };
    // Integers are TOTALLY ORDERED, so the NaN rules have nothing to bite on
    // and are simply absent. WGSL's `<` / `>` are signed on an `array<i32>`
    // and unsigned on an `array<u32>`, so the declared element type is what
    // makes `4294967295` the largest u32 rather than `-1` — no separate
    // comparison spelling is needed.
    // The emitted comment has to match the emitted CODE — an integer shader
    // that talked about NaN would be describing guards it does not contain.
    let combine_doc = if elem == "f32" {
        "// The combine, verbatim from `reduce_kernel::arg_takes_b`: a strictly\n\
         // better value wins, an exact tie goes to the SMALLER index, and a NaN\n\
         // loses to anything real (ties with another NaN, where the smaller\n\
         // index survives). Lexicographic, therefore grouping-independent.\n"
    } else {
        "// The combine: a strictly better value wins, an exact tie goes to the\n\
         // SMALLER index. Integers are totally ordered, so there is no NaN rule\n\
         // — and `<` / `>` are signed or unsigned to match the declared element\n\
         // type. Lexicographic, therefore grouping-independent.\n"
    };
    let nan_guards = if elem == "f32" {
        "\x20   if (karac_is_nan(a)) { return !karac_is_nan(b); }\n\
         \x20   if (karac_is_nan(b)) { return false; }\n"
    } else {
        ""
    };
    // The NaN predicate is only declared where it is used.
    let nan_fn = if elem == "f32" {
        format!("\n{}\n", NAN_PREDICATE_WGSL)
    } else {
        String::new()
    };
    let invalid = format!("{}u", crate::reduce_kernel::ARG_INVALID);
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;

    // Level 0 reads the buffer directly; the fold levels read the surviving
    // candidates and look their values up in the same buffer.
    let (candidates_binding, out_binding, seed) = if fold {
        (
            "@group(0) @binding(1) var<storage, read>       cand:   array<u32>;\n\
             @group(0) @binding(2) var<storage, read_write> output: array<u32>;\n"
                .to_string(),
            2,
            format!(
                "\x20   if (i < arrayLength(&cand)) {{ idxs[t] = cand[i]; }} else {{ idxs[t] = {invalid}; }}"
            ),
        )
    } else {
        (
            "@group(0) @binding(1) var<storage, read_write> output: array<u32>;\n".to_string(),
            1,
            format!(
                "\x20   if (i < arrayLength(&input)) {{ idxs[t] = i; }} else {{ idxs[t] = {invalid}; }}"
            ),
        )
    };
    let _ = out_binding;

    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         {candidates_binding}\
         \n\
         var<workgroup> idxs: array<u32, {width}>;\n\
         {nan_fn}\
         \n\
         {combine_doc}\
         fn takes_b(ia: u32, ib: u32) -> bool {{\n\
         \x20   if (ib == {invalid}) {{ return false; }}\n\
         \x20   if (ia == {invalid}) {{ return true; }}\n\
         \x20   let a = input[ia];\n\
         \x20   let b = input[ib];\n\
         {nan_guards}\
         \x20   return (b {strict} a) || (b == a && ib < ia);\n\
         }}\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         {seed}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{\n\
         \x20           if (takes_b(idxs[t], idxs[t + stride])) {{ idxs[t] = idxs[t + stride]; }}\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // One surviving candidate per workgroup, as an ABSOLUTE index\n\
         \x20   // into `input` — so the next level can look its value up again.\n\
         \x20   if (t == 0u) {{ output[wg] = idxs[0]; }}\n\
         }}\n"
    ))
}

/// Emit the CHECKED integer tree-reduction shader (B-2026-08-19-13).
///
/// The integer sibling of [`emit_reduce_kernel`], and structurally different
/// from it in one way: it carries a second output, `flags`, holding one
/// per-workgroup overflow bit. Integer reductions TRAP where float ones
/// saturate — `v.sum()` over a `Vec[i32]` already fails with `integer
/// overflow` on both surfaces, and moving the reduction to a GPU must not
/// silently turn that trap into a wrapped wrong answer (design.md § Integer
/// reductions overflow-check).
///
/// WGSL has no overflow flag and no trapping arithmetic — its integer ops are
/// *defined* to wrap — so the check is written out. For a signed add,
/// `(a ^ s) & (b ^ s) < 0` is true exactly when the operands shared a sign and
/// the result did not; for unsigned, the carry shows as `s < a`. The bit is
/// then OR-folded through the SAME halving tree as the value, so a single
/// overflowing lane at any stride reaches lane 0.
///
/// `min`/`max` cannot overflow, so they carry no `ovf` array and no per-step
/// bookkeeping — lane 0 simply writes a zero flag. The `flags` binding is
/// still declared for them, so every integer reduction has ONE dispatch shape
/// and the host needs one readback path rather than two.
///
/// **`prod` is deliberately absent.** A checked multiply needs a widening
/// intermediate that WGSL does not have, and the usual `s / a != b`
/// substitute is unsound there: `i32::MIN / -1` is an indeterminate value in
/// WGSL, so the check would misfire on exactly the input it exists to catch.
/// Integer products also overflow after ~31 terms, so the operation has little
/// real use at buffer scale. It refuses rather than guessing.
pub fn emit_int_reduce_kernel(op: ReduceOp, elem: &str) -> Result<String, WgslError> {
    let signed = match elem {
        "i32" => true,
        "u32" => false,
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU reduction over `{elem}` is not supported — the integer \
                 reduction entry point covers i32 and u32"
            )))
        }
    };
    // `identity` pads a short chunk; `body` is the combine, which for `sum`
    // also folds the overflow bit.
    let (identity, body) = match (op, signed) {
        (ReduceOp::Sum, true) => (
            "0".to_string(),
            "let a = scratch[t];\n\
             \x20           let b = scratch[t + stride];\n\
             \x20           let s = a + b;\n\
             \x20           scratch[t] = s;\n\
             \x20           // Signed add overflowed iff the operands shared a sign\n\
             \x20           // and the result did not. WGSL wraps by definition,\n\
             \x20           // so `s` is the wrapped value and this is exact.\n\
             \x20           ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, ((a ^ s) & (b ^ s)) < 0);"
                .to_string(),
        ),
        (ReduceOp::Sum, false) => (
            "0u".to_string(),
            "let a = scratch[t];\n\
             \x20           let b = scratch[t + stride];\n\
             \x20           let s = a + b;\n\
             \x20           scratch[t] = s;\n\
             \x20           // Unsigned add overflowed iff it carried, i.e. wrapped below\n\
             \x20           // either operand.\n\
             \x20           ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, s < a);"
                .to_string(),
        ),
        // `-2147483648` is not writable as an i32 literal (unary minus applies
        // to a value already out of range), hence the subtraction.
        (ReduceOp::Min, true) => (
            "2147483647".to_string(),
            "scratch[t] = min(scratch[t], scratch[t + stride]);".to_string(),
        ),
        (ReduceOp::Max, true) => (
            "-2147483647 - 1".to_string(),
            "scratch[t] = max(scratch[t], scratch[t + stride]);".to_string(),
        ),
        (ReduceOp::Min, false) => (
            "4294967295u".to_string(),
            "scratch[t] = min(scratch[t], scratch[t + stride]);".to_string(),
        ),
        (ReduceOp::Max, false) => (
            "0u".to_string(),
            "scratch[t] = max(scratch[t], scratch[t + stride]);".to_string(),
        ),
        // `prod` reached the device once the widening multiply did
        // (B-2026-08-19-13). The combine is a CHECKED multiply whose overflow
        // bit folds exactly like the sum's, so nothing else about the kernel
        // changes — which is the point of having built the primitive.
        (ReduceOp::Prod, true) => (
            "1".to_string(),
            "let a = scratch[t];\n\
             \x20           let b = scratch[t + stride];\n\
             \x20           let r = karac_mul_i32_checked(a, b);\n\
             \x20           scratch[t] = r.x;\n\
             \x20           ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, r.y != 0);"
                .to_string(),
        ),
        (ReduceOp::Prod, false) => (
            "1u".to_string(),
            "let a = scratch[t];\n\
             \x20           let b = scratch[t + stride];\n\
             \x20           let r = karac_mul_u32_checked(a, b);\n\
             \x20           scratch[t] = r.x;\n\
             \x20           ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, r.y != 0u);"
                .to_string(),
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU reduction `{op:?}` is not supported — the integer ops \
                 are `sum`, `prod`, `min` and `max`"
            )))
        }
    };
    // `prod` overflows exactly as `sum` does, so it carries the same flag
    // buffer and the same host-side fold.
    let can_overflow = matches!(op, ReduceOp::Sum | ReduceOp::Prod);
    // The multiply helpers are only pulled in for `prod`; `sum`/`min`/`max`
    // emit the same text they always did, so their shaders keep their
    // pipeline-cache identity.
    let mul_helpers = if matches!(op, ReduceOp::Prod) {
        format!("{WIDE_U64_WGSL}{CHECKED_MUL_WGSL}")
    } else {
        String::new()
    };
    let ovf_decl = if can_overflow {
        format!("var<workgroup> ovf: array<u32, {}>;\n", GPU_REDUCE_WIDTH)
    } else {
        String::new()
    };
    let ovf_init = if can_overflow {
        "    ovf[t] = 0u;\n"
    } else {
        ""
    };
    let ovf_out = if can_overflow { "ovf[0]" } else { "0u" };

    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> flags:  array<u32>;\n\
         \n\
         {mul_helpers}\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         {ovf_decl}\n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   if (i < arrayLength(&input)) {{ scratch[t] = input[i]; }} else {{ scratch[t] = {identity}; }}\n\
         {ovf_init}\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{\n\
         \x20           {body}\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // One partial AND one overflow bit per workgroup; the host ORs\n\
         \x20   // the bits and re-dispatches over the partials.\n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; flags[wg] = {ovf_out}; }}\n\
         }}\n"
    ))
}

/// WGSL helpers for EXACT 64-bit integer arithmetic out of 32-bit parts, as a
/// `vec2<u32>` of `(lo, hi)` (B-2026-08-19-13).
///
/// **This is the primitive that retires "blocked on WGSL" from the integer
/// side of the reduction family.** That phrase meant WGSL has no widening
/// multiply, which is true of the INTRINSIC and not of the capability: a
/// `u32 × u32 → u64` product is four 16-bit partial products and two carries,
/// and a `u64 + u64` is one add plus a carry test. Both are exact, both are
/// device-portable, and both are validated against Rust's own `u64` on the
/// device by `runtime/src/gpu.rs`'s `wgsl_can_multiply_32x32_into_an_exact_64_bit_product`.
///
/// The carry tests are the whole subtlety. WGSL's `u32` arithmetic WRAPS by
/// definition, so `a + b < a` is exactly the carry-out — the same trick the
/// unsigned branch of [`emit_int_reduce_kernel`] already uses. Nothing here
/// relies on undefined behaviour or on a wider intermediate type existing.
const WIDE_U64_WGSL: &str = "\
fn karac_mul_wide(a: u32, b: u32) -> vec2<u32> {\n\
\x20   // 32x32 -> 64 by 16-bit splitting. WGSL has no widening multiply, but\n\
\x20   // each 16-bit half-product fits a u32 exactly, so the four partials\n\
\x20   // recombine with explicit carries and nothing is lost.\n\
\x20   let a_lo = a & 0xffffu;\n\
\x20   let a_hi = a >> 16u;\n\
\x20   let b_lo = b & 0xffffu;\n\
\x20   let b_hi = b >> 16u;\n\
\x20   let ll = a_lo * b_lo;\n\
\x20   let lh = a_lo * b_hi;\n\
\x20   let hl = a_hi * b_lo;\n\
\x20   let hh = a_hi * b_hi;\n\
\x20   // The two middle terms straddle bit 16 and their SUM can carry into\n\
\x20   // bit 32, which is why `mid` needs a carry of its own rather than\n\
\x20   // being folded straight in.\n\
\x20   let mid = lh + hl;\n\
\x20   let mid_carry = select(0u, 0x10000u, mid < lh);\n\
\x20   let lo = ll + (mid << 16u);\n\
\x20   let lo_carry = select(0u, 1u, lo < ll);\n\
\x20   let hi = hh + (mid >> 16u) + mid_carry + lo_carry;\n\
\x20   return vec2<u32>(lo, hi);\n\
}\n\
\n\
fn karac_add_wide(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {\n\
\x20   let lo = a.x + b.x;\n\
\x20   // u32 addition wraps, so wrapping below an operand IS the carry-out.\n\
\x20   let carry = select(0u, 1u, lo < a.x);\n\
\x20   return vec2<u32>(lo, a.y + b.y + carry);\n\
}\n\
\n\
fn karac_add_wide_overflowed(a: vec2<u32>, b: vec2<u32>, s: vec2<u32>) -> bool {\n\
\x20   // The 64-bit sum overflowed iff the HIGH word wrapped. Testing the\n\
\x20   // high word against `a.y` alone is not enough — the low carry can push\n\
\x20   // it to exactly `a.y` — so the carry is folded in first.\n\
\x20   let carry = select(0u, 1u, (a.x + b.x) < a.x);\n\
\x20   return s.y < a.y || (b.y + carry) < b.y || (a.y + b.y + carry) < a.y;\n\
}\n";

/// WGSL helpers for a CHECKED 32-bit integer multiply, built on
/// [`WIDE_U64_WGSL`]'s exact widening product (B-2026-08-19-13).
///
/// **This is what "`prod` needs a widening multiply WGSL does not have" was
/// waiting for**, and the wait was based on a misreading: WGSL lacks the
/// widening-multiply INTRINSIC, not the capability. With an exact
/// `u32 × u32 → u64` in hand, "did this product overflow 32 bits?" is a
/// question about the high word, which is exactly how a hardware multiplier
/// answers it.
///
/// Unsigned is the easy case: `a * b` overflows `u32` iff the wide product's
/// high word is non-zero.
///
/// Signed needs the magnitude, because `i32`'s range is asymmetric. The
/// product's magnitude is `|a| · |b|`, computed wide; it fits iff it is at
/// most `2147483647` for a positive result, or `2147483648` for a negative
/// one — that extra value being `i32::MIN`, the one magnitude only the
/// negative side can hold. Taking `|i32::MIN|` as a `u32` is exact
/// (`2147483648u`), which is why the magnitudes are carried unsigned rather
/// than negated in place, where `-i32::MIN` would itself overflow.
const CHECKED_MUL_WGSL: &str = "\
fn karac_mul_u32_checked(a: u32, b: u32) -> vec2<u32> {\n\
\x20   // Returns (product, overflowed). The wide product's high word is\n\
\x20   // non-zero exactly when the result does not fit u32.\n\
\x20   let w = karac_mul_wide(a, b);\n\
\x20   return vec2<u32>(w.x, select(0u, 1u, w.y != 0u));\n\
}\n\
\n\
fn karac_abs_u32(x: i32) -> u32 {\n\
\x20   // |x| as a u32. Exact even at i32::MIN, whose magnitude 2147483648\n\
\x20   // has no i32 representation — negating in i32 would overflow, so the\n\
\x20   // negation happens after the bitcast, in u32, where it wraps to\n\
\x20   // exactly the right magnitude.\n\
\x20   if (x < 0) { return 0u - bitcast<u32>(x); }\n\
\x20   return bitcast<u32>(x);\n\
}\n\
\n\
fn karac_mul_i32_checked(a: i32, b: i32) -> vec2<i32> {\n\
\x20   // Returns (product, overflowed). Magnitudes multiplied wide, then\n\
\x20   // range-checked against the bound for the RESULT's sign — i32's range\n\
\x20   // is asymmetric, so a negative result may reach one further.\n\
\x20   let w = karac_mul_wide(karac_abs_u32(a), karac_abs_u32(b));\n\
\x20   let negative = (a < 0) != (b < 0);\n\
\x20   let limit = select(2147483647u, 2147483648u, negative);\n\
\x20   if (w.y != 0u || w.x > limit) { return vec2<i32>(0, 1); }\n\
\x20   // In range: rebuild the signed value from the magnitude.\n\
\x20   let mag = w.x;\n\
\x20   let v = select(bitcast<i32>(mag), bitcast<i32>(0u - mag), negative);\n\
\x20   return vec2<i32>(v, 0);\n\
}\n";

/// Emit the squared-deviation kernel for an INTEGER `gpu.variance` /
/// `gpu.stddev` (B-2026-08-19-13).
///
/// **The integer variance is EXACT, which is the reverse of what this feature
/// was expected to deliver.** The recorded objection was that a `(x - mean)²`
/// formed on-device in f32 quantises every element above 2²⁴, so `mean`'s
/// promote-late trick could not carry over. Two changes remove it:
///
///  * **The shift is by an INTEGER.** `Var(x) = Var(x - K)`, so the host sends
///    `K = round(mean)` and the device subtracts it in exact integer
///    arithmetic — nothing is converted to a float at any point. The
///    deviation's size is then bounded by the data's SPREAD rather than its
///    position, which is the dependency a variance should have.
///  * **The square is exact.** [`WIDE_U64_WGSL`]'s `karac_mul_wide` gives a
///    true `u32 × u32 → u64`, so `d²` is not rounded either.
///
/// The deviation is taken as an unsigned MAGNITUDE (`|x - K|`) rather than a
/// signed difference, which is not a shortcut: `x - K` can exceed `i32`'s
/// range (an `i32::MIN` element against a positive `K`), while `|x - K|`
/// always fits a `u32`, and the square does not care about the sign.
/// Computing the magnitude by branching on the comparison, rather than
/// negating, is what keeps that true at the very edge of the type.
///
/// Emits one `u64` partial and one overflow bit per workgroup, on the same
/// flag channel [`emit_int_reduce_kernel`] uses — so the host folds partials
/// and ORs flags exactly as it does for a checked integer sum, and an integer
/// variance TRAPS on overflow like every other integer reduction here.
pub fn emit_int_deviation_kernel(elem: &str) -> Result<String, WgslError> {
    let to_i64 = match elem {
        // `i32` is widened to a signed 64-bit compare against `K`; `u32` is
        // already non-negative. Both end up as an unsigned magnitude.
        "i32" => {
            "let xv: i32 = input[i];\n\
                  \x20       let big: bool = i64_ge_i32(xv, k_lo, k_hi);\n\
                  \x20       let d: u32 = select(sub_i32_from_i64(k_lo, k_hi, xv), \
                  sub_i64_from_i32(xv, k_lo, k_hi), big);"
        }
        "u32" => {
            "let xv: u32 = input[i];\n\
                  \x20       let big: bool = u64_le_u32(k_lo, k_hi, xv);\n\
                  \x20       let d: u32 = select(sub_u32_from_u64(k_lo, k_hi, xv), \
                  sub_u64_from_u32(xv, k_lo, k_hi), big);"
        }
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "integer GPU variance over `{elem}` is not supported — the integer reduction \
                 entry points cover i32 and u32"
            )))
        }
    };
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    let helpers = int_deviation_helpers(elem);
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<u32>;\n\
         @group(0) @binding(2) var<storage, read_write> flags:  array<u32>;\n\
         // K, the integer shift, as two u32 words (lo, hi) of a signed 64-bit\n\
         // value. It arrives at 64 bits because `round(mean)` of a u32 buffer\n\
         // can exceed i32, and a K that did not fit would defeat the whole\n\
         // point of shifting.\n\
         @group(0) @binding(3) var<storage, read>       shift:  array<u32>;\n\
         \n\
         {WIDE_U64_WGSL}\
         {helpers}\
         \n\
         var<workgroup> scratch: array<vec2<u32>, {width}>;\n\
         var<workgroup> ovf: array<u32, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   let k_lo = shift[0];\n\
         \x20   let k_hi = shift[1];\n\
         \x20   ovf[t] = 0u;\n\
         \x20   if (i < arrayLength(&input)) {{\n\
         \x20       {to_i64}\n\
         \x20       scratch[t] = karac_mul_wide(d, d);\n\
         \x20   }} else {{\n\
         \x20       // 0 is the Sum identity, and squaring it is still 0 — a\n\
         \x20       // padded lane contributes nothing to either word.\n\
         \x20       scratch[t] = vec2<u32>(0u, 0u);\n\
         \x20   }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{\n\
         \x20           let a = scratch[t];\n\
         \x20           let b = scratch[t + stride];\n\
         \x20           let s = karac_add_wide(a, b);\n\
         \x20           scratch[t] = s;\n\
         \x20           ovf[t] = ovf[t] | ovf[t + stride] | \
         select(0u, 1u, karac_add_wide_overflowed(a, b, s));\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // Two words per workgroup partial, plus its overflow bit.\n\
         \x20   if (t == 0u) {{\n\
         \x20       output[2u * wg] = scratch[0].x;\n\
         \x20       output[2u * wg + 1u] = scratch[0].y;\n\
         \x20       flags[wg] = ovf[0];\n\
         \x20   }}\n\
         }}\n"
    ))
}

/// Emit the kernel that folds `u64` partials from
/// [`emit_int_deviation_kernel`] — level 1 and above of the same reduction.
///
/// A separate shader because level 0 reads ELEMENTS and squares them while
/// every level after reads 64-bit PARTIALS and only adds. Fusing them would
/// need a mode flag in the shader, and a branch on it in every lane, to save
/// one small emitter.
pub fn emit_wide_fold_kernel() -> String {
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    format!(
        "@group(0) @binding(0) var<storage, read>       input:  array<u32>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<u32>;\n\
         @group(0) @binding(2) var<storage, read_write> flags:  array<u32>;\n\
         \n\
         {WIDE_U64_WGSL}\
         \n\
         var<workgroup> scratch: array<vec2<u32>, {width}>;\n\
         var<workgroup> ovf: array<u32, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   ovf[t] = 0u;\n\
         \x20   // Two words per partial, so the element count is half the\n\
         \x20   // array length.\n\
         \x20   if (2u * i + 1u < arrayLength(&input)) {{\n\
         \x20       scratch[t] = vec2<u32>(input[2u * i], input[2u * i + 1u]);\n\
         \x20   }} else {{\n\
         \x20       scratch[t] = vec2<u32>(0u, 0u);\n\
         \x20   }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{\n\
         \x20           let a = scratch[t];\n\
         \x20           let b = scratch[t + stride];\n\
         \x20           let s = karac_add_wide(a, b);\n\
         \x20           scratch[t] = s;\n\
         \x20           ovf[t] = ovf[t] | ovf[t + stride] | \
         select(0u, 1u, karac_add_wide_overflowed(a, b, s));\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   if (t == 0u) {{\n\
         \x20       output[2u * wg] = scratch[0].x;\n\
         \x20       output[2u * wg + 1u] = scratch[0].y;\n\
         \x20       flags[wg] = ovf[0];\n\
         \x20   }}\n\
         }}\n"
    )
}

/// The element-specific half of [`emit_int_deviation_kernel`]: comparing an
/// element against the 64-bit shift `K` and taking the unsigned magnitude of
/// their difference.
///
/// Written at 64 bits on the `K` side throughout. `K` is `round(mean)`, which
/// for a `u32` buffer can exceed `i32::MAX` and for an `i32` buffer is an
/// ordinary signed value — so a 32-bit `K` would either overflow or need a
/// sign convention that differs between the two element types.
fn int_deviation_helpers(elem: &str) -> String {
    if elem == "i32" {
        "\n\
         fn i64_ge_i32(x: i32, k_lo: u32, k_hi: u32) -> bool {\n\
         \x20   // Sign-extend x to 64 bits and compare against K.\n\
         \x20   let x_hi = select(0u, 0xffffffffu, x < 0);\n\
         \x20   let xh = bitcast<i32>(x_hi);\n\
         \x20   let kh = bitcast<i32>(k_hi);\n\
         \x20   if (xh != kh) { return xh > kh; }\n\
         \x20   return bitcast<u32>(x) >= k_lo;\n\
         }\n\
         \n\
         fn sub_i64_from_i32(x: i32, k_lo: u32, k_hi: u32) -> u32 {\n\
         \x20   // x - K, known non-negative and known to fit u32 (the caller\n\
         \x20   // only reaches here when x >= K, and both are within i32 of\n\
         \x20   // each other by construction — K lies inside the data range).\n\
         \x20   return bitcast<u32>(x) - k_lo;\n\
         }\n\
         \n\
         fn sub_i32_from_i64(k_lo: u32, k_hi: u32, x: i32) -> u32 {\n\
         \x20   return k_lo - bitcast<u32>(x);\n\
         }\n"
        .to_string()
    } else {
        "\n\
         fn u64_le_u32(k_lo: u32, k_hi: u32, x: u32) -> bool {\n\
         \x20   if (k_hi != 0u) { return false; }\n\
         \x20   return k_lo <= x;\n\
         }\n\
         \n\
         fn sub_u64_from_u32(x: u32, k_lo: u32, k_hi: u32) -> u32 {\n\
         \x20   return x - k_lo;\n\
         }\n\
         \n\
         fn sub_u32_from_u64(k_lo: u32, k_hi: u32, x: u32) -> u32 {\n\
         \x20   return k_lo - x;\n\
         }\n"
        .to_string()
    }
}

/// Emit the CHECKED INTEGER tiled matmul kernel (B-2026-08-19-13).
///
/// **Same tiling, same order, plus overflow flags.** The float kernel's
/// promise is that tiling preserves the naive `k`-ascending accumulation
/// order; the integer form inherits it and gains a sharper consequence —
/// because the order is identical, so is the set of intermediates, so
/// `gpu.matmul` and `a.matmul(b)` agree about WHICH CONTRACTIONS OVERFLOW as
/// well as what they return. Reordering the tile loop would break that even
/// where it preserved the final value.
///
/// Both the product and the accumulation are checked: `65536 * 65536` leaves
/// `i32` in one term, so checking only the running sum would let a wrapped
/// product through.
///
/// The overflow flag is per WORKGROUP, on the same channel every checked
/// integer kernel here uses, and the host ORs it — one poisoned output cell
/// traps the whole matmul, which is what a language that traps on overflow
/// has to do with a value it cannot represent.
///
/// PADDED LANES CANNOT RAISE THE FLAG. A lane past `m`/`n`/`k` stages zeros
/// and multiplies them, so it contributes `0 * 0` and never overflows; only a
/// lane holding real operands can. Without that, an edge workgroup on a
/// perfectly valid matrix would trap on arithmetic that is not in the data.
pub fn emit_int_matmul_kernel(elem: &str) -> Result<String, WgslError> {
    let (zero, mul, add) = match elem {
        "i32" => (
            "0",
            "let pr = karac_mul_i32_checked(av, bv);\n\
             \x20           let po = pr.y != 0;\n\
             \x20           let s = acc + pr.x;\n\
             \x20           let so = ((acc ^ s) & (pr.x ^ s)) < 0;\n\
             \x20           acc = s;",
            "ovf_local = ovf_local || po || so;",
        ),
        "u32" => (
            "0u",
            "let pr = karac_mul_u32_checked(av, bv);\n\
             \x20           let po = pr.y != 0u;\n\
             \x20           let s = acc + pr.x;\n\
             \x20           let so = s < acc;\n\
             \x20           acc = s;",
            "ovf_local = ovf_local || po || so;",
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU `matmul` over `{elem}` is not supported — the integer \
                 entry points cover i32 and u32"
            )))
        }
    };
    let tile = GPU_MATMUL_TILE;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       a:      array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       b:      array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output: array<{elem}>;\n\
         @group(0) @binding(3) var<storage, read_write> flags:  array<u32>;\n\
         @group(0) @binding(4) var<storage, read>       dims:   array<u32>;\n\
         \n\
         {WIDE_U64_WGSL}{CHECKED_MUL_WGSL}\
         \n\
         var<workgroup> a_tile: array<{elem}, {tile}u * {tile}u>;\n\
         var<workgroup> b_tile: array<{elem}, {tile}u * {tile}u>;\n\
         var<workgroup> ovf: array<u32, {tile}u * {tile}u>;\n\
         \n\
         @compute @workgroup_size({tile}, {tile})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(num_workgroups) nwg: vec3<u32>) {{\n\
         \x20   let m = dims[0];\n\
         \x20   let k = dims[1];\n\
         \x20   let n = dims[2];\n\
         \x20   let ty = lid.y;\n\
         \x20   let tx = lid.x;\n\
         \x20   let row = wid.y * {tile}u + ty;\n\
         \x20   let col = wid.x * {tile}u + tx;\n\
         \x20   let lane = ty * {tile}u + tx;\n\
         \x20   ovf[lane] = 0u;\n\
         \x20   var ovf_local: bool = false;\n\
         \n\
         \x20   var acc: {elem} = {zero};\n\
         \x20   let tiles = (k + {tile}u - 1u) / {tile}u;\n\
         \x20   var t: u32 = 0u;\n\
         \x20   loop {{\n\
         \x20       if (t >= tiles) {{ break; }}\n\
         \x20       let a_k = t * {tile}u + tx;\n\
         \x20       let b_k = t * {tile}u + ty;\n\
         \x20       if (row < m && a_k < k) {{\n\
         \x20           a_tile[lane] = a[row * k + a_k];\n\
         \x20       }} else {{\n\
         \x20           a_tile[lane] = {zero};\n\
         \x20       }}\n\
         \x20       if (col < n && b_k < k) {{\n\
         \x20           b_tile[lane] = b[b_k * n + col];\n\
         \x20       }} else {{\n\
         \x20           b_tile[lane] = {zero};\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \n\
         \x20       // Only a lane holding a REAL output cell accumulates. A\n\
         \x20       // padded lane would multiply zeros harmlessly, but letting\n\
         \x20       // it run would also let it raise the overflow flag for\n\
         \x20       // arithmetic that is not in the data.\n\
         \x20       if (row < m && col < n) {{\n\
         \x20           var p: u32 = 0u;\n\
         \x20           loop {{\n\
         \x20               if (p >= {tile}u) {{ break; }}\n\
         \x20               let av = a_tile[ty * {tile}u + p];\n\
         \x20               let bv = b_tile[p * {tile}u + tx];\n\
         \x20               {mul}\n\
         \x20               {add}\n\
         \x20               p = p + 1u;\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       t = t + 1u;\n\
         \x20   }}\n\
         \n\
         \x20   if (row < m && col < n) {{ output[row * n + col] = acc; }}\n\
         \x20   ovf[lane] = select(0u, 1u, ovf_local);\n\
         \x20   workgroupBarrier();\n\
         \x20   // Lane 0 ORs its workgroup's flags into one word, indexed by\n\
         \x20   // the FLATTENED workgroup id — the grid is 2-D here, so a\n\
         \x20   // `wid.x`-only index would collide across rows.\n\
         \x20   if (lane == 0u) {{\n\
         \x20       var any: u32 = 0u;\n\
         \x20       var q: u32 = 0u;\n\
         \x20       loop {{\n\
         \x20           if (q >= {tile}u * {tile}u) {{ break; }}\n\
         \x20           any = any | ovf[q];\n\
         \x20           q = q + 1u;\n\
         \x20       }}\n\
         \x20       flags[wid.y * nwg.x + wid.x] = any;\n\
         \x20   }}\n\
         }}\n"
    ))
}

/// Emit the tiled matrix-multiply kernel for `gpu.matmul(a, b)`
/// (B-2026-08-19-13).
///
/// **The first 2-D shader in this file, and the reason matmul was tracked as a
/// separate project rather than another reduction.** Every kernel above is a
/// line of `@workgroup_size(64)` lanes indexed by `local_invocation_id.x`;
/// this one is a `TILE x TILE` square indexed by `.x` AND `.y`, and it is
/// dispatched over a genuinely 2-D grid rather than the flattened one
/// `run_compute` builds. The `x`-only model could not express it, which is
/// what "2-D workgroup indexing the 1-D model lacks" meant.
///
/// Each workgroup computes one `TILE x TILE` block of the output. It walks the
/// contraction in `TILE`-wide steps: stage one tile of `a` and one of `b` into
/// workgroup memory, barrier, accumulate `TILE` products from the staged
/// tiles, barrier, advance. Staging is the entire point — every value read
/// from global memory is used `TILE` times instead of once.
///
/// **THE ACCUMULATION ORDER IS THE NAIVE ONE, and that is a promise, not an
/// accident.** Tiles are visited in ascending `k` and the inner loop runs
/// `p = 0..TILE` in order, so the products are added in `k = 0, 1, 2, ...`
/// order — exactly what `Tensor.matmul`'s triple loop does on both CPU
/// surfaces. Unlike `gpu.sum` (a tree where the CPU is a line) and
/// `gpu.prefix_sum` (whose total differs from `gpu.sum` for that very reason),
/// `gpu.matmul(a, b)` is bit-for-bit `a.matmul(b)`. Reordering this loop, or
/// splitting the contraction across workgroups and summing the partials, would
/// break that — such a split is a semantics change, not an optimization.
///
/// **TWO BARRIERS PER TILE, not one.** The second one — after the inner loop,
/// before the next tile is staged — is the one that looks redundant and is
/// not: without it a fast lane could overwrite `a_tile` for tile `t + 1` while
/// a slow lane is still reading tile `t`. The corruption is a race, so it
/// appears only under contention and only on some devices.
///
/// **Both tiles are zero-padded at the same `k`.** A lane whose `k` is past
/// the contraction stages `0.0` on BOTH sides, contributing `0.0 * 0.0`.
/// Padding one side only would let a real value meet a padded zero, and `inf *
/// 0.0` is NaN — an output element poisoned by arithmetic that was never in
/// the data. The `m`/`n` edges are guarded separately: an out-of-range lane
/// stages zeros and skips its final store, so its garbage accumulator never
/// reaches the output.
///
/// Dimensions arrive as a uniform buffer (`dims[0..3]` = `m`, `k`, `n`) rather
/// than being baked into the shader text, so one compiled pipeline serves
/// every shape — the pipeline cache is keyed on the WGSL string, and
/// specializing it per shape would recompile on every new matrix.
pub fn emit_matmul_kernel(elem: &str) -> Result<String, WgslError> {
    if elem != "f32" {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `matmul` over `{elem}` is not supported yet — it is f32-only. \
             An integer matmul accumulates a sum of PRODUCTS, so it needs both the \
             checked multiply WGSL lacks (no widening multiply) and the overflow flag \
             of `emit_int_reduce_kernel` carried through every one of its `k` steps"
        )));
    }
    let tile = GPU_MATMUL_TILE;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       a:      array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       b:      array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output: array<{elem}>;\n\
         // m, k, n. A storage buffer rather than a `uniform` one, matching the\n\
         // convention in `run_compute`: it avoids the 16-byte uniform\n\
         // alignment rule for three loose u32s.\n\
         @group(0) @binding(3) var<storage, read>       dims:   array<u32>;\n\
         \n\
         var<workgroup> a_tile: array<{elem}, {tile}u * {tile}u>;\n\
         var<workgroup> b_tile: array<{elem}, {tile}u * {tile}u>;\n\
         \n\
         @compute @workgroup_size({tile}, {tile})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>) {{\n\
         \x20   let m = dims[0];\n\
         \x20   let k = dims[1];\n\
         \x20   let n = dims[2];\n\
         \x20   // .y indexes the output ROW, .x the output COLUMN — the same\n\
         \x20   // orientation as the 2-D dispatch, whose x spans n and y spans m.\n\
         \x20   let ty = lid.y;\n\
         \x20   let tx = lid.x;\n\
         \x20   let row = wid.y * {tile}u + ty;\n\
         \x20   let col = wid.x * {tile}u + tx;\n\
         \n\
         \x20   var acc: {elem} = 0.0;\n\
         \x20   let tiles = (k + {tile}u - 1u) / {tile}u;\n\
         \x20   var t: u32 = 0u;\n\
         \x20   loop {{\n\
         \x20       if (t >= tiles) {{ break; }}\n\
         \x20       // STAGE. The k each lane stages differs per side: for `a`\n\
         \x20       // the lane's column walks k, for `b` its row does.\n\
         \x20       let a_k = t * {tile}u + tx;\n\
         \x20       let b_k = t * {tile}u + ty;\n\
         \x20       // Padded on BOTH sides at the same k, and at the m/n edges,\n\
         \x20       // so a padded lane always contributes 0.0 * 0.0.\n\
         \x20       if (row < m && a_k < k) {{\n\
         \x20           a_tile[ty * {tile}u + tx] = a[row * k + a_k];\n\
         \x20       }} else {{\n\
         \x20           a_tile[ty * {tile}u + tx] = 0.0;\n\
         \x20       }}\n\
         \x20       if (col < n && b_k < k) {{\n\
         \x20           b_tile[ty * {tile}u + tx] = b[b_k * n + col];\n\
         \x20       }} else {{\n\
         \x20           b_tile[ty * {tile}u + tx] = 0.0;\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \n\
         \x20       // ACCUMULATE, ascending p — this is what keeps the order\n\
         \x20       // identical to the naive triple loop.\n\
         \x20       var p: u32 = 0u;\n\
         \x20       loop {{\n\
         \x20           if (p >= {tile}u) {{ break; }}\n\
         \x20           acc = acc + a_tile[ty * {tile}u + p] * b_tile[p * {tile}u + tx];\n\
         \x20           p = p + 1u;\n\
         \x20       }}\n\
         \x20       // The barrier that looks redundant and is not: without it a\n\
         \x20       // fast lane stages tile t+1 over values a slow lane is still\n\
         \x20       // reading from tile t.\n\
         \x20       workgroupBarrier();\n\
         \x20       t = t + 1u;\n\
         \x20   }}\n\
         \n\
         \x20   // Edge workgroups overshoot; only real output cells store.\n\
         \x20   if (row < m && col < n) {{ output[row * n + col] = acc; }}\n\
         }}\n"
    ))
}

/// Emit the CHECKED integer level-0 kernel for `gpu.dot(a, b)`
/// (B-2026-08-19-13).
///
/// **`gpu.dot(a, b)` is `gpu.sum(a * b)` to the last bit, and over integers
/// that promise now extends to WHICH PROGRAMS TRAP.** Both the per-element
/// product and every accumulation are checked, so a dot product overflows on
/// exactly the inputs the equivalent `sum`-of-products would — the identity
/// design.md states for floats holds for integers without an exception
/// clause.
///
/// Two overflow sources, and both matter. The PRODUCT can overflow on its own
/// (`65536 * 65536` passes `i32` with one term), and so can the SUM of
/// products that individually fit. Checking only the accumulation would let
/// the first slip through wrapped, which is the plausible-wrong-number shape
/// this family is built to refuse.
///
/// Levels 1+ are the ordinary CHECKED integer sum kernel, exactly as the float
/// form reuses the plain sum — which is what keeps `dot` and `sum` agreeing
/// rather than merely being tested to agree.
pub fn emit_int_dot_kernel(elem: &str) -> Result<String, WgslError> {
    let (identity, body) = match elem {
        "i32" => (
            "0",
            "let p = karac_mul_i32_checked(a[i], b[i]);\n\
             \x20       scratch[t] = p.x;\n\
             \x20       ovf[t] = select(0u, 1u, p.y != 0);",
        ),
        "u32" => (
            "0u",
            "let p = karac_mul_u32_checked(a[i], b[i]);\n\
             \x20       scratch[t] = p.x;\n\
             \x20       ovf[t] = select(0u, 1u, p.y != 0u);",
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "checked integer GPU `dot` over `{elem}` is not supported — the integer \
                 reduction entry points cover i32 and u32"
            )))
        }
    };
    // The combine is the CHECKED sum, identical to `emit_int_reduce_kernel`'s
    // — reused verbatim so the two cannot drift on the overflow rule.
    let add = if elem == "i32" {
        "let x = scratch[t];\n\
         \x20           let y = scratch[t + stride];\n\
         \x20           let s = x + y;\n\
         \x20           scratch[t] = s;\n\
         \x20           ovf[t] = ovf[t] | ovf[t + stride] | \
         select(0u, 1u, ((x ^ s) & (y ^ s)) < 0);"
    } else {
        "let x = scratch[t];\n\
         \x20           let y = scratch[t + stride];\n\
         \x20           let s = x + y;\n\
         \x20           scratch[t] = s;\n\
         \x20           ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, s < x);"
    };
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       a:      array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       b:      array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output: array<{elem}>;\n\
         @group(0) @binding(3) var<storage, read_write> flags:  array<u32>;\n\
         \n\
         {WIDE_U64_WGSL}{CHECKED_MUL_WGSL}\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         var<workgroup> ovf: array<u32, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   if (i < arrayLength(&a)) {{\n\
         \x20       {body}\n\
         \x20   }} else {{\n\
         \x20       // A padded lane contributes the Sum identity and cannot\n\
         \x20       // raise the flag — it multiplied nothing.\n\
         \x20       scratch[t] = {identity};\n\
         \x20       ovf[t] = 0u;\n\
         \x20   }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{\n\
         \x20           {add}\n\
         \x20       }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; flags[wg] = ovf[0]; }}\n\
         }}\n"
    ))
}

/// Emit the `gpu.dot(a, b)` level-0 shader: multiply the two buffers
/// element-wise on load, then run the SAME halving tree `emit_reduce_kernel`
/// emits for a sum (B-2026-08-19-13).
///
/// **This is one level, not the whole reduction.** It leaves one partial per
/// workgroup, and the host folds those with the plain SUM shader — a dot
/// product is a map fused into the first level of a sum, and only the first
/// level knows about two buffers. Reusing the sum shader for every later level
/// is what keeps the two paths from drifting: `gpu.dot(a, b)` and
/// `gpu.sum(a * b)` agree bit-for-bit by construction, because after level 0
/// they ARE the same computation.
///
/// The two inputs sit at `@binding(0)` and `@binding(1)`, the partials at
/// `@binding(2)` — the runtime's dispatch binds inputs first, then outputs, so
/// a 2-in/1-out kernel lands exactly there.
///
/// Multiplying on load rather than in a separate map dispatch is the point:
/// it halves the device traffic (no `n`-element intermediate is ever written)
/// and is why `dot` is worth its own entry point instead of being sugar for
/// `sum(zip_mul(a, b))`.
pub fn emit_dot_kernel(elem: &str) -> Result<String, WgslError> {
    if elem != "f32" {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU `dot` over `{elem}` is not supported yet — the reduction entry point is f32-only"
        )));
    }
    let width = GPU_REDUCE_WIDTH;
    let half = width / 2;
    Ok(format!(
        "@group(0) @binding(0) var<storage, read>       a:      array<{elem}>;\n\
         @group(0) @binding(1) var<storage, read>       b:      array<{elem}>;\n\
         @group(0) @binding(2) var<storage, read_write> output: array<{elem}>;\n\
         \n\
         var<workgroup> scratch: array<{elem}, {width}>;\n\
         \n\
         @compute @workgroup_size({width})\n\
         fn main(@builtin(local_invocation_id) lid: vec3<u32>,\n\
         \x20       @builtin(workgroup_id) wid: vec3<u32>,\n\
         \x20       @builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let t = lid.x;\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;\n\
         \x20   // The ONLY difference from the sum shader: the product is\n\
         \x20   // formed on load, so no n-element intermediate is written.\n\
         \x20   if (i < arrayLength(&a)) {{ scratch[t] = a[i] * b[i]; }} else {{ scratch[t] = 0.0; }}\n\
         \x20   workgroupBarrier();\n\
         \n\
         \x20   var stride: u32 = {half}u;\n\
         \x20   loop {{\n\
         \x20       if (stride == 0u) {{ break; }}\n\
         \x20       if (t < stride) {{ scratch[t] = scratch[t] + scratch[t + stride]; }}\n\
         \x20       workgroupBarrier();\n\
         \x20       stride = stride / 2u;\n\
         \x20   }}\n\
         \n\
         \x20   // One partial per workgroup; the host folds them with the\n\
         \x20   // plain SUM shader, which is what makes dot and sum agree.\n\
         \x20   if (t == 0u) {{ output[wg] = scratch[0]; }}\n\
         }}\n"
    ))
}

/// The three pieces `emit_reduce_kernel` needs for one op: any helper-function
/// PRELUDE, the COMBINE expression folding `scratch[t]` with
/// `scratch[t + stride]`, and the IDENTITY that pads a short chunk.
///
/// Only the associative folds have a single-shader tree form. `Mean` needs a
/// count division, `Var`/`Std` need two passes, and the Arg family needs an
/// index carried alongside the value through every halving step (a scratch of
/// PAIRS, which is a different shader rather than a different combine string)
/// — each is its own slice, and saying so beats emitting a shader that
/// silently computes the wrong statistic.
///
/// **`min`/`max` on floats do not use WGSL's `min`/`max` builtins.** The
/// builtin is specified as "returns `e2` if `e2 < e1`, and `e1` otherwise",
/// and every comparison against NaN is false — so `min(x, NaN)` is `x` but
/// `min(NaN, x)` is `NaN`. That positional tie-break is harmless in a left
/// fold and fatal in a tree, where a NaN's position in the halving decides how
/// many times it is on the left. The emitted helper ignores NaN from either
/// side, matching `f32::min` and `reduce_kernel`'s twin, which makes the
/// operation associative and therefore grouping-independent.
///
/// Integer `min`/`max` have no NaN and use the builtin directly.
fn reduce_combine_wgsl(op: ReduceOp, elem: &str) -> Result<(String, String, String), WgslError> {
    let lhs = "scratch[t]";
    let rhs = "scratch[t + stride]";
    let infix = |o: &str| format!("{lhs} {o} {rhs}");
    let call = |f: &str| format!("{f}({lhs}, {rhs})");
    // Bit patterns rather than literals: WGSL has no `inf` literal, and a
    // finite stand-in like `f32::MAX` would be BEATEN by a real `f32::MAX`
    // element in a padded chunk — the identity has to be unreachable.
    let pos_inf = "bitcast<f32>(0x7f800000u)";
    let neg_inf = "bitcast<f32>(0xff800000u)";
    // WGSL's `select(f, t, cond)` takes the false value first. `!(x == x)` is
    // the NaN test that survives an `FOrd` lowering of `==`, where `NaN == NaN`
    // is false; spelling it `x != x` would rely on `!=` lowering to the
    // UNORDERED form, which is not something to bet the semantics on.
    let nan_ignoring = |name: &str, cmp: &str| {
        format!(
            "\n{NAN_PREDICATE_WGSL}\n\
             fn {name}(a: f32, b: f32) -> f32 {{\n\
             \x20   if (karac_is_nan(a)) {{ return b; }}\n\
             \x20   if (karac_is_nan(b)) {{ return a; }}\n\
             \x20   return select(a, b, b {cmp} a);\n\
             }}\n"
        )
    };
    // INTEGER ELEMENTS ARE NOT HANDLED HERE, deliberately. This emitter
    // produces an UNCHECKED tree, which is correct for floats (they saturate)
    // and wrong for integers (Kāra traps where WGSL wraps). Integer reductions
    // go through `emit_int_reduce_kernel`, which carries the overflow flag —
    // routing them here would silently produce the wrapped answer, which is
    // the exact failure the integer-reduction rule exists to prevent. Refusing
    // here rather than merely documenting it keeps the wrong shader
    // unreachable.
    if matches!(elem, "i32" | "u32") {
        return Err(WgslError::UnsupportedSignature(format!(
            "integer GPU reduction over `{elem}` must go through the CHECKED emitter \
             (`emit_int_reduce_kernel`) — this one emits an unchecked tree, which would wrap \
             where Kāra traps"
        )));
    }
    let (prelude, combine, identity) = match (op, elem) {
        (ReduceOp::Sum, "f32") => (String::new(), infix("+"), "0.0".to_string()),
        (ReduceOp::Prod, "f32") => (String::new(), infix("*"), "1.0".to_string()),
        (ReduceOp::Min, "f32") => (
            nan_ignoring("karac_min", "<"),
            call("karac_min"),
            pos_inf.to_string(),
        ),
        (ReduceOp::Max, "f32") => (
            nan_ignoring("karac_max", ">"),
            call("karac_max"),
            neg_inf.to_string(),
        ),
        _ => {
            return Err(WgslError::UnsupportedSignature(format!(
                "GPU reduction `{op:?}` over `{elem}` is not supported yet — the tree-shaped \
                 ops are `sum`, `prod`, `min` and `max` over f32/i32/u32"
            )))
        }
    };
    Ok((prelude, combine, identity))
}

/// WGSL infix operator for a `wrapping_{add,sub,mul}` method call, or `None`
/// for any other method.
///
/// WGSL integer arithmetic IS two's-complement wrapping — the spec defines
/// overflow that way and offers no trapping form — so the wrapping family needs
/// no helper function, just the operator. That equivalence is the whole reason
/// this lowering is sound rather than approximate.
fn wrapping_infix_wgsl(method: &str, argc: usize) -> Option<&'static str> {
    if argc != 1 {
        return None;
    }
    match method {
        "wrapping_add" => Some("+"),
        "wrapping_sub" => Some("-"),
        "wrapping_mul" => Some("*"),
        _ => None,
    }
}

// ── Trapping integer arithmetic is not expressible on a GPU (B-2026-08-19-1) ──

/// What a kernel expression evaluates to, as far as this pass can tell.
///
/// Three-valued on purpose: `Unknown` means "do not judge", and the pass only
/// ever REJECTS on `Int`, so an incomplete judgment costs coverage rather than
/// producing a false rejection.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KTy {
    Int,
    Float,
    Unknown,
}

/// Reject bare `+ - * / %` on integer operands in a `#[gpu]` body.
///
/// WGSL defines integer overflow as WRAPPING and division by zero as returning
/// an implementation-defined value; there is no trapping form to emit. Kāra's
/// `app`/`lib` profiles promise the opposite (design.md § Arithmetic Overflow:
/// integer overflow traps), so lowering bare `+` silently changed a program's
/// meaning on the device — measured, `x + 1` at `i32::MAX` traps under
/// `--interp` and yields `-2147483648` on the GPU, and `100 / 0` yields `100`.
///
/// design.md is explicit that the overflow escape hatch must stay "narrow and
/// local … never a project-wide `overflow-checks=off` switch, which would strip
/// the guarantee invisibly". `#[gpu]` was exactly such an invisible switch. So
/// the fix is not to weaken the guarantee but to make the kernel NAME the
/// wrapping intent: `x.wrapping_add(1)`, which lowers to the same WGSL infix
/// operator and is what the device does anyway.
///
/// Float arithmetic is untouched — IEEE ops do not trap, so f32 kernels (which
/// is every shipped GPU example) are unaffected.
fn reject_trapping_int_arith(func: &Function) -> Result<(), WgslError> {
    let mut env: Vec<(&str, KTy)> = func
        .params
        .iter()
        .filter_map(|p| p.name().map(|n| (n, kty_of_type(&p.ty))))
        .collect();
    check_block(&func.body, &mut env)
}

fn kty_of_type(ty: &TypeExpr) -> KTy {
    match scalar_name(ty).as_deref() {
        Some("i32" | "u32" | "i64" | "u64" | "i8" | "u8" | "i16" | "u16" | "usize") => KTy::Int,
        Some("f32" | "f64") => KTy::Float,
        _ => KTy::Unknown,
    }
}

fn check_block<'a>(b: &'a Block, env: &mut Vec<(&'a str, KTy)>) -> Result<(), WgslError> {
    let depth = env.len();
    for st in &b.stmts {
        match &st.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            } => {
                check_expr(value, env)?;
                if let PatternKind::Binding(name) = &pattern.kind {
                    let t = match ty {
                        Some(t) => kty_of_type(t),
                        None => kty_of(value, env),
                    };
                    env.push((name.as_str(), t));
                }
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                check_expr(target, env)?;
                check_expr(value, env)?;
            }
            StmtKind::Expr(e) => check_expr(e, env)?,
            _ => {}
        }
    }
    if let Some(t) = &b.final_expr {
        check_expr(t, env)?;
    }
    env.truncate(depth);
    Ok(())
}

fn check_expr<'a>(e: &'a Expr, env: &mut Vec<(&'a str, KTy)>) -> Result<(), WgslError> {
    match &e.kind {
        ExprKind::Binary { op, left, right } => {
            check_expr(left, env)?;
            check_expr(right, env)?;
            let trapping = matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            );
            if trapping && (kty_of(left, env) == KTy::Int || kty_of(right, env) == KTy::Int) {
                let (name, spelled) = match op {
                    BinOp::Add => ("+", "wrapping_add"),
                    BinOp::Sub => ("-", "wrapping_sub"),
                    BinOp::Mul => ("*", "wrapping_mul"),
                    BinOp::Div => ("/", ""),
                    _ => ("%", ""),
                };
                return Err(WgslError::UnsupportedBody(if spelled.is_empty() {
                    format!(
                        "integer `{name}` is not allowed in a GPU kernel — Kāra traps on division \
                         by zero, and WGSL has no trapping form (it returns an \
                         implementation-defined value instead), so the same expression would mean \
                         different things on CPU and GPU. Guard the divisor in the kernel, or keep \
                         the division on the host."
                    )
                } else {
                    format!(
                        "integer `{name}` is not allowed in a GPU kernel — Kāra traps on overflow \
                         and WGSL wraps, so the same expression would mean different things on CPU \
                         and GPU. Write `a.{spelled}(b)` to say you mean wraparound (it lowers to \
                         the same WGSL `{name}`), or use f32."
                    )
                }));
            }
            Ok(())
        }
        ExprKind::Unary { operand, .. } => check_expr(operand, env),
        ExprKind::MethodCall { object, args, .. } => {
            check_expr(object, env)?;
            for a in args {
                check_expr(&a.value, env)?;
            }
            Ok(())
        }
        ExprKind::Call { args, .. } => {
            for a in args {
                check_expr(&a.value, env)?;
            }
            Ok(())
        }
        ExprKind::Cast { expr, .. } => check_expr(expr, env),
        ExprKind::Index { object, index } => {
            check_expr(object, env)?;
            check_expr(index, env)
        }
        ExprKind::FieldAccess { object, .. } => check_expr(object, env),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            check_expr(condition, env)?;
            check_block(then_block, env)?;
            match else_branch {
                Some(b) => check_expr(b, env),
                None => Ok(()),
            }
        }
        ExprKind::Block(b) => check_block(b, env),
        ExprKind::While {
            condition, body, ..
        } => {
            check_expr(condition, env)?;
            check_block(body, env)
        }
        ExprKind::For { iterable, body, .. } => {
            check_expr(iterable, env)?;
            check_block(body, env)
        }
        ExprKind::Match { scrutinee, arms } => {
            check_expr(scrutinee, env)?;
            for arm in arms {
                check_expr(&arm.body, env)?;
            }
            Ok(())
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                check_expr(s, env)?;
            }
            if let Some(en) = end {
                check_expr(en, env)?;
            }
            Ok(())
        }
        ExprKind::Return(Some(inner)) => check_expr(inner, env),
        _ => Ok(()),
    }
}

/// Best-effort type of a kernel expression. Only ever used to decide whether an
/// arithmetic operand is an integer, so `Unknown` is always the safe answer.
fn kty_of(e: &Expr, env: &[(&str, KTy)]) -> KTy {
    match &e.kind {
        ExprKind::Integer(..) => KTy::Int,
        ExprKind::Float(..) => KTy::Float,
        ExprKind::Identifier(n) => env
            .iter()
            .rev()
            .find(|(k, _)| k == n)
            .map(|(_, t)| *t)
            .unwrap_or(KTy::Unknown),
        ExprKind::Binary { op, left, right } => {
            if matches!(
                op,
                BinOp::Eq
                    | BinOp::NotEq
                    | BinOp::Lt
                    | BinOp::LtEq
                    | BinOp::Gt
                    | BinOp::GtEq
                    | BinOp::And
                    | BinOp::Or
            ) {
                return KTy::Unknown; // bool
            }
            match kty_of(left, env) {
                KTy::Unknown => kty_of(right, env),
                t => t,
            }
        }
        ExprKind::Unary { operand, .. } => kty_of(operand, env),
        ExprKind::Cast { ty, .. } => kty_of_type(ty),
        // A math intrinsic (`sqrt`, `abs`, …) is float-valued; `wrapping_*`
        // takes the receiver's type. Anything else is not judged.
        ExprKind::MethodCall { object, method, .. } => {
            if wrapping_infix_wgsl(method, 1).is_some() {
                kty_of(object, env)
            } else if math_intrinsic_wgsl(method, 1).is_some()
                || math_intrinsic_wgsl(method, 2).is_some()
            {
                KTy::Float
            } else {
                KTy::Unknown
            }
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            if let Some(t) = &then_block.final_expr {
                let t = kty_of(t, env);
                if t != KTy::Unknown {
                    return t;
                }
            }
            match else_branch {
                Some(b) => kty_of(b, env),
                None => KTy::Unknown,
            }
        }
        ExprKind::Block(b) => match &b.final_expr {
            Some(t) => kty_of(t, env),
            None => KTy::Unknown,
        },
        _ => KTy::Unknown,
    }
}

/// The WGSL builtin for a supported scalar math **method** call (`x.sqrt()` →
/// `sqrt(x)`) — GPU-SLIP-2a. Only the nullary float intrinsics the LBM kernels
/// need are allowed; `None` for any other method, so `buf.len()` and unknown
/// methods fall through to their own handling / an error.
fn math_intrinsic_wgsl(method: &str, arg_count: usize) -> Option<&'static str> {
    match (method, arg_count) {
        ("sqrt", 0) => Some("sqrt"),
        ("abs", 0) => Some("abs"),
        ("floor", 0) => Some("floor"),
        ("ceil", 0) => Some("ceil"),
        _ => None,
    }
}

/// The WGSL numeric constructor for an `as` cast's target type — GPU-SLIP-2a.
/// Every float cast targets `f32` (WGSL has no f64, per GPU-LBM-1); signed
/// integer casts target `i32` (indices may go negative in neighbour math),
/// unsigned target `u32`. `None` for a non-scalar or unsupported target.
fn cast_ctor(ty: &TypeExpr) -> Option<&'static str> {
    match scalar_name(ty)?.as_str() {
        "f32" | "f64" => Some("f32"),
        "i8" | "i16" | "i32" | "i64" | "isize" => Some("i32"),
        "u8" | "u16" | "u32" | "u64" | "usize" => Some("u32"),
        _ => None,
    }
}

/// The WGSL comparison operator for a lowered comparison method name (`gt`, `lt`,
/// …) — the post-lowering form the SoA emitter sees (`f32.gt(a, b)`). `None` for a
/// non-comparison method.
fn cmp_method_op(name: &str) -> Option<&'static str> {
    match name {
        "gt" => Some(">"),
        "lt" => Some("<"),
        "ge" => Some(">="),
        "le" => Some("<="),
        "eq" => Some("=="),
        "ne" => Some("!="),
        _ => None,
    }
}

/// The WGSL operator for a lowered arithmetic method name (`add`, `mul`, …) — the
/// post-lowering call form. `None` for a non-arithmetic method.
fn arith_method_op(name: &str) -> Option<&'static str> {
    match name {
        "add" => Some("+"),
        "sub" => Some("-"),
        "mul" => Some("*"),
        "div" => Some("/"),
        "rem" | "mod" => Some("%"),
        _ => None,
    }
}

/// The function name a call's callee names, for a bare identifier or a
/// 1-segment path (a free `#[gpu]` helper). `None` for a 2-segment `<type>.<op>`
/// operator method or any other callee.
fn call_helper_name(callee: &Expr) -> Option<&str> {
    match &callee.kind {
        ExprKind::Identifier(n) => Some(n.as_str()),
        ExprKind::Path { segments, .. } if segments.len() == 1 => Some(segments[0].as_str()),
        _ => None,
    }
}

/// Lower a `Call`: a 2-segment `<type>.<op>` operator method (arithmetic /
/// comparison / unary `neg` — the post-lowering form) or a user `#[gpu]` helper
/// call (GPU-LBM-5). `lower_arg` lowers each argument in the caller's context
/// (kernel: field/`input[i]`; helper: identity). Shared by both emitter paths.
fn lower_call(
    callee: &Expr,
    args: &[CallArg],
    lower_arg: &dyn Fn(&Expr) -> Result<String, WgslError>,
    helpers: &HashSet<String>,
) -> Result<String, WgslError> {
    // 2-segment path = a lowered operator method (`f32.add`, `f32.gt`, `f32.neg`).
    if let ExprKind::Path { segments, .. } = &callee.kind {
        if segments.len() == 2 {
            let op = segments[1].as_str();
            if let Some(o) = arith_method_op(op).or_else(|| cmp_method_op(op)) {
                if args.len() == 2 {
                    let l = lower_arg(&args[0].value)?;
                    let r = lower_arg(&args[1].value)?;
                    return Ok(format!("({l} {o} {r})"));
                }
            }
            if op == "neg" && args.len() == 1 {
                return Ok(format!("-({})", lower_arg(&args[0].value)?));
            }
        }
    }
    // A bare identifier / 1-segment path naming a reachable `#[gpu]` helper.
    if let Some(name) = call_helper_name(callee) {
        if helpers.contains(name) {
            let lowered = args
                .iter()
                .map(|a| lower_arg(&a.value))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(format!("{name}({})", lowered.join(", ")));
        }
    }
    Err(WgslError::UnsupportedBody(
        "unsupported call in a GPU kernel body — only arithmetic / comparison operators \
         and `#[gpu]` helper functions are supported"
            .to_string(),
    ))
}

/// The `#[gpu]` helper functions transitively reachable from `root`'s body, in
/// callee-before-caller order (WGSL requires a function be declared before use).
/// `all` maps every `#[gpu]` function name to its `Function`; `root` itself is
/// excluded. Also returns the set of reachable helper names (for call recognition
/// during lowering).
fn reachable_helpers<'a>(
    root: &Function,
    all: &HashMap<String, &'a Function>,
) -> (Vec<&'a Function>, HashSet<String>) {
    fn calls_in(expr: &Expr, all: &HashMap<String, &Function>, out: &mut Vec<String>) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(name) = call_helper_name(callee) {
                    if all.contains_key(name) {
                        out.push(name.to_string());
                    }
                }
                for a in args {
                    calls_in(&a.value, all, out);
                }
                calls_in(callee, all, out);
            }
            ExprKind::Binary { left, right, .. } => {
                calls_in(left, all, out);
                calls_in(right, all, out);
            }
            ExprKind::Unary { operand, .. } => calls_in(operand, all, out),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                calls_in(condition, all, out);
                calls_in_block(then_block, all, out);
                if let Some(e) = else_branch {
                    calls_in(e, all, out);
                }
            }
            ExprKind::Block(b) => calls_in_block(b, all, out),
            ExprKind::StructLiteral { fields, .. } => {
                for f in fields {
                    calls_in(&f.value, all, out);
                }
            }
            ExprKind::FieldAccess { object, .. } => calls_in(object, all, out),
            // B-2026-07-11-20: a helper call can also sit inside an index
            // (`coll[g_idx(…)]`), a method receiver/args, or a cast operand — the
            // `stream` kernel puts `g_idx` in exactly these positions. Missing
            // them left the helper ungathered, so codegen rejected the call.
            ExprKind::Index { object, index } => {
                calls_in(object, all, out);
                calls_in(index, all, out);
            }
            ExprKind::MethodCall { object, args, .. } => {
                calls_in(object, all, out);
                for a in args {
                    calls_in(&a.value, all, out);
                }
            }
            ExprKind::Cast { expr, .. } => calls_in(expr, all, out),
            _ => {}
        }
    }
    fn calls_in_block(b: &Block, all: &HashMap<String, &Function>, out: &mut Vec<String>) {
        if let Some(e) = &b.final_expr {
            calls_in(e, all, out);
        }
        for s in &b.stmts {
            match &s.kind {
                StmtKind::Expr(e) => calls_in(e, all, out),
                // B-2026-07-11-20: a `let` RHS can carry helper calls too
                // (`let c = coll[g_idx(x, y, w)]`).
                StmtKind::Let { value, .. } => calls_in(value, all, out),
                _ => {}
            }
        }
    }
    fn visit<'a>(
        f: &Function,
        all: &HashMap<String, &'a Function>,
        seen: &mut HashSet<String>,
        order: &mut Vec<&'a Function>,
    ) {
        let mut called = Vec::new();
        calls_in_block(&f.body, all, &mut called);
        for name in called {
            if let Some(&h) = all.get(&name) {
                if seen.insert(name) {
                    visit(h, all, seen, order); // callees first
                    order.push(h);
                }
            }
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(root.name.clone());
    let mut order = Vec::new();
    visit(root, all, &mut seen, &mut order);
    let names: HashSet<String> = order.iter().map(|f| f.name.clone()).collect();
    (order, names)
}

/// The WGSL type of a `#[gpu]` helper parameter or return — a numeric scalar or
/// `bool` (GPU-SLIP-2b). Numerics use the 32-bit `cast_ctor` mapping (f64→f32,
/// i64→i32, u64→u32): WGSL is 32-bit, and a helper param declared `i64` to match
/// Kāra's index arithmetic (`i % w`) maps to WGSL `i32` exactly as the kernel's
/// index parameter already does (`i32(gid.x)`) — sound for the index domain,
/// consistent with GPU-LBM-1's f64→f32.
fn wgsl_helper_type(ty: &TypeExpr, position: &str) -> Result<&'static str, WgslError> {
    if matches!(scalar_name(ty).as_deref(), Some("bool")) {
        return Ok("bool");
    }
    cast_ctor(ty).ok_or_else(|| {
        WgslError::UnsupportedSignature(format!(
            "a GPU helper {position} must be a numeric scalar or bool"
        ))
    })
}

/// Emit a reachable `#[gpu]` helper as a WGSL `fn name(p0: T0, …) -> R { … }`.
/// Each parameter carries its real WGSL scalar type (`i32`/`f32`/`u32`) and the
/// return type may be a scalar or `bool` (GPU-SLIP-2b). The body is a sequence of
/// `let` bindings (GPU-SLIP-2b) followed by the return expression; params and
/// locals resolve to themselves (identity), calls to other helpers are
/// recognized via `helper_names`.
fn emit_helper_def(func: &Function, helper_names: &HashSet<String>) -> Result<String, WgslError> {
    if func.self_param.is_some() {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU helper `{}` cannot take a self receiver",
            func.name
        )));
    }
    // `scope` = every identifier that lowers to itself: params first, then each
    // `let` local as it is bound.
    let mut scope: HashSet<String> = HashSet::new();
    let mut sig = String::new();
    for (i, p) in func.params.iter().enumerate() {
        let name = p.name().ok_or_else(|| {
            WgslError::UnsupportedSignature(format!(
                "GPU helper `{}` parameter must be a plain binding",
                func.name
            ))
        })?;
        let wty = wgsl_helper_type(&p.ty, "parameter")?; // f32/i32/u32 or bool
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(&format!("{name}: {wty}"));
        scope.insert(name.to_string());
    }
    let ret_ty = match &func.return_type {
        Some(ty) => wgsl_helper_type(ty, "return type")?,
        None => {
            return Err(WgslError::UnsupportedSignature(format!(
                "GPU helper `{}` must return a scalar or bool",
                func.name
            )));
        }
    };
    // Body: leading `let` bindings then the return expression.
    let (lets, ret) = kernel_body_parts(func, "an expression")?;
    let mut lets_wgsl = String::new();
    for (name, value) in lets {
        let resolve = |n: &str| -> Option<String> { scope.contains(n).then(|| n.to_string()) };
        let rhs = lower_expr(value, &resolve, helper_names)?;
        lets_wgsl.push_str(&format!("let {name} = {rhs}; "));
        scope.insert(name.to_string());
    }
    let resolve = |n: &str| -> Option<String> { scope.contains(n).then(|| n.to_string()) };
    let body_wgsl = lower_expr(ret, &resolve, helper_names)?;
    Ok(format!(
        "fn {}({sig}) -> {ret_ty} {{ {lets_wgsl}return {body_wgsl}; }}\n",
        func.name
    ))
}

/// Emit the WGSL definitions of every `#[gpu]` helper reachable from `root`, in
/// declaration order, and return them with the reachable-helper name set.
fn emit_helpers(
    root: &Function,
    all_helpers: &[&Function],
) -> Result<(String, HashSet<String>), WgslError> {
    let all: HashMap<String, &Function> =
        all_helpers.iter().map(|h| (h.name.clone(), *h)).collect();
    let (order, names) = reachable_helpers(root, &all);
    let mut defs = String::new();
    for h in order {
        defs.push_str(&emit_helper_def(h, &names)?);
    }
    Ok((defs, names))
}

/// Extract the value expressions of `if cond { then } else { else }` used as a
/// value. Both branches must be a single expression; the `else` may be a block
/// (`else { .. }`) or another `if` (else-if chain, recursed by the caller). No
/// `else` is an error — a value `if` needs both arms. WGSL has no statement `if`
/// in this subset, so this lowers to `select(else, then, cond)`.
/// Lower a value `match` into a nested `select()` chain, built from the last
/// arm outward: `match s { p1 => e1, p2 => e2, _ => ef }` becomes
/// `select(select(ef, e2, s == p2), e1, s == p1)`.
///
/// **Every arm is evaluated**, because `select` is not short-circuiting — and
/// that is sound here precisely because of the `#[gpu]` effect gate. A kernel's
/// whole transitive call graph is already proven free of allocation, host I/O,
/// channel traffic and explicit panics, so an unselected arm can have no
/// observable effect. (The implicit trap sources FE-4b keeps permitted —
/// integer division by zero, indexing — are defined, non-trapping operations in
/// WGSL, so they cannot fault an unselected arm either.) Branchless is also the
/// shape a GPU wants: no per-lane divergence.
///
/// The LAST arm is the fallback and its pattern is not tested. That is sound
/// without inspecting it: the typechecker has already proven the match
/// exhaustive, so when no earlier arm matches the last one must.
fn lower_match(
    scrutinee: &Expr,
    arms: &[crate::ast::MatchArm],
    resolve: &dyn Fn(&str) -> Option<String>,
    helpers: &HashSet<String>,
) -> Result<String, WgslError> {
    let Some((fallback, rest)) = arms.split_last() else {
        return Err(WgslError::UnsupportedBody(
            "a GPU kernel `match` must have at least one arm".to_string(),
        ));
    };
    if arms.iter().any(|a| a.guard.is_some()) {
        return Err(WgslError::UnsupportedBody(
            "a GPU kernel `match` arm cannot have a guard yet".to_string(),
        ));
    }
    // The scrutinee text is repeated once per tested arm; naga's own CSE folds
    // the duplicates, and keeping it textual avoids a temporary that would need
    // statement position.
    let subject = lower_expr(scrutinee, resolve, helpers)?;
    // Validate every pattern BEFORE lowering any body — including the
    // fallback's, whose condition is discarded but whose shape still has to be
    // supported. Otherwise an unsupported pattern surfaces as whatever its body
    // happens to trip over first (a binding arm `n => n` reported "unknown
    // identifier 'n'", naming the symptom rather than the cause).
    let conditions = rest
        .iter()
        .map(|arm| match_arm_condition(&arm.pattern, &subject))
        .collect::<Result<Vec<_>, _>>()?;
    match_arm_condition(&fallback.pattern, &subject)?;

    let mut acc = lower_expr(&fallback.body, resolve, helpers)?;
    for (arm, cond) in rest.iter().zip(conditions).rev() {
        let body = lower_expr(&arm.body, resolve, helpers)?;
        match cond {
            // An irrefutable arm before the end makes every later arm dead.
            None => acc = body,
            Some(cond) => acc = format!("select({acc}, {body}, {cond})"),
        }
    }
    Ok(acc)
}

/// The WGSL test for one `match` arm against `subject`, or `None` when the
/// pattern is irrefutable (`_`). Only the pattern forms with a WGSL equality
/// exist here: integer and bool literals, and `|` alternatives of those.
fn match_arm_condition(
    pattern: &crate::ast::Pattern,
    subject: &str,
) -> Result<Option<String>, WgslError> {
    use crate::ast::LiteralPattern;
    match &pattern.kind {
        PatternKind::Wildcard => Ok(None),
        PatternKind::Literal(LiteralPattern::Integer(n, _)) => {
            Ok(Some(format!("({subject} == {n})")))
        }
        PatternKind::Literal(LiteralPattern::Bool(b)) => Ok(Some(format!("({subject} == {b})"))),
        PatternKind::Or(alts) => {
            let mut parts = Vec::new();
            for alt in alts {
                match match_arm_condition(alt, subject)? {
                    // `_` inside an alternation makes the whole arm irrefutable.
                    None => return Ok(None),
                    Some(c) => parts.push(c),
                }
            }
            if parts.is_empty() {
                return Err(WgslError::UnsupportedBody(
                    "a GPU kernel `match` alternation must have at least one pattern".to_string(),
                ));
            }
            Ok(Some(format!("({})", parts.join(" || "))))
        }
        PatternKind::Literal(LiteralPattern::Float(_, _)) => Err(WgslError::UnsupportedBody(
            "a GPU kernel `match` cannot test a float literal — exact float equality is not a \
             reliable selector; match on an integer instead"
                .to_string(),
        )),
        PatternKind::Binding(name) => Err(WgslError::UnsupportedBody(format!(
            "a GPU kernel `match` arm cannot bind `{name}` — the branchless `select` lowering has \
             no place to introduce a binding; use `_` and read the scrutinee directly"
        ))),
        _ => Err(WgslError::UnsupportedBody(
            "a GPU kernel `match` supports integer / bool literal patterns, `|` alternations of \
             those, and `_`"
                .to_string(),
        )),
    }
}

fn if_branches<'a>(
    then_block: &'a Block,
    else_branch: &'a Option<Box<Expr>>,
) -> Result<(&'a Expr, &'a Expr), WgslError> {
    let block_value = |b: &'a Block| -> Result<&'a Expr, WgslError> {
        if !b.stmts.is_empty() {
            return Err(WgslError::UnsupportedBody(
                "a GPU `if` branch must be a single expression (no locals)".to_string(),
            ));
        }
        b.final_expr
            .as_deref()
            .ok_or_else(|| WgslError::UnsupportedBody("a GPU `if` branch has no value".to_string()))
    };
    let then_e = block_value(then_block)?;
    let else_box = else_branch.as_deref().ok_or_else(|| {
        WgslError::UnsupportedBody(
            "a GPU `if` must have an `else` — it produces a value".to_string(),
        )
    })?;
    let else_e = match &else_box.kind {
        ExprKind::Block(b) => block_value(b)?,
        // else-if chain: recurse on the whole `if`.
        ExprKind::If { .. } => else_box,
        _ => else_box,
    };
    Ok((then_e, else_e))
}

/// Format an `f64` literal as a WGSL float literal — always with a decimal
/// point (or exponent) so it lexes as a floating-point (abstract-float)
/// constant rather than an integer. Non-finite literals are rejected (they
/// have no WGSL literal spelling; a GpuSafe kernel should not contain one).
fn lower_float(f: f64) -> Result<String, WgslError> {
    if !f.is_finite() {
        return Err(WgslError::UnsupportedBody(
            "non-finite float literal has no WGSL spelling".to_string(),
        ));
    }
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        Ok(s)
    } else {
        Ok(format!("{s}.0"))
    }
}

// ── CG-4: struct-SoA multi-buffer emitter ───────────────────────────────────

/// One layout group in GPU-binding order for the SoA emitter. `name` is the
/// group name (→ its WGSL sub-struct name / binding prefix); `fields` are the
/// struct fields the group carries (all `f32`), in sub-struct order. A
/// single-field group binds a plain `array<f32>`; a multi-field group binds a
/// WGSL `struct` `array` over the coalesced sub-struct (GPU-LBM-3). Plain data
/// built by codegen from the `SoaLayout` — keeps this emitter free of any
/// codegen/inkwell type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoaGpuGroup {
    pub name: String,
    pub fields: Vec<String>,
}

impl SoaGpuGroup {
    fn is_multi(&self) -> bool {
        self.fields.len() > 1
    }
    /// WGSL sub-struct type name for a multi-field group (`G_`-prefixed so it
    /// cannot collide with a user type).
    fn wgsl_struct(&self) -> String {
        format!("G_{}", self.name)
    }
    /// The WGSL element type of this group's `array` binding.
    fn elem_ty(&self) -> String {
        if self.is_multi() {
            self.wgsl_struct()
        } else {
            "f32".to_string()
        }
    }
}

/// Emit the WGSL compute shader for a struct-SoA `#[gpu]` kernel `fn k(p: S) -> S`
/// dispatched over a `layout`-blocked `Vec[S]`. `groups` lists the layout groups
/// in binding order. Each group binds one input buffer at `@binding(0..n)` and one
/// output at `@binding(n..2n)`: a single-field group is a plain `array<f32>`; a
/// multi-field group is `array<G_<name>>` over an emitted WGSL sub-struct
/// (GPU-LBM-3 coalesced group). `<param>.<field>` reads the group's materialized
/// element; the returned struct literal stores each field into its group's output.
pub fn emit_kernel_soa(
    func: &Function,
    groups: &[SoaGpuGroup],
    helpers: &[&Function],
) -> Result<String, WgslError> {
    if func.self_param.is_some() {
        return Err(WgslError::UnsupportedSignature(
            "a GPU kernel cannot take a self receiver".to_string(),
        ));
    }
    // GPU-LBM-6: a *stencil* kernel reads neighbours. Its first parameter is the
    // whole `Vec[S]` buffer (not an element `S`) followed by an integer index; the
    // body indexes `buf[j].field`. Route to the stencil emitter — the bindings are
    // identical (the whole input is already bound read-only), only the body can now
    // address arbitrary elements.
    if func
        .params
        .first()
        .map(|p| is_vec_type(&p.ty))
        .unwrap_or(false)
    {
        return emit_kernel_stencil(func, groups, helpers);
    }
    // The first parameter is the struct buffer element; any further parameters are
    // scalar uniforms (GPU-LBM-2) — each `f32`, bound after the group buffers and
    // read in the body as `<name>_u[0]`.
    let (struct_param, uniform_params) = func.params.split_first().ok_or_else(|| {
        WgslError::UnsupportedSignature("a struct GPU kernel needs a struct parameter".to_string())
    })?;
    let param_name = struct_param.name().ok_or_else(|| {
        WgslError::UnsupportedSignature(
            "the GPU kernel parameter must be a plain binding".to_string(),
        )
    })?;
    let mut uniform_names: Vec<String> = Vec::new();
    for u in uniform_params {
        wgsl_scalar(&u.ty, "uniform parameter")?;
        let un = u.name().ok_or_else(|| {
            WgslError::UnsupportedSignature(
                "a GPU uniform parameter must be a plain binding".to_string(),
            )
        })?;
        uniform_names.push(un.to_string());
    }
    let uniform_set: HashSet<String> = uniform_names.iter().cloned().collect();
    if groups.is_empty() {
        return Err(WgslError::UnsupportedSignature(
            "a struct GPU kernel needs at least one layout group".to_string(),
        ));
    }
    for g in groups {
        if g.fields.is_empty() {
            return Err(WgslError::UnsupportedSignature(format!(
                "layout group `{}` has no fields",
                g.name
            )));
        }
    }
    let n = groups.len();

    // WGSL sub-struct definitions for multi-field groups (before the bindings).
    let mut structs = String::new();
    for g in groups {
        if g.is_multi() {
            let members = g
                .fields
                .iter()
                .map(|f| format!("{f}: f32"))
                .collect::<Vec<_>>()
                .join(", ");
            structs.push_str(&format!("struct {} {{ {members} }};\n", g.wgsl_struct()));
        }
    }

    // Bindings: inputs at 0..n, outputs at n..2n. `<group>_in` / `<group>_out`.
    let mut decls = String::new();
    for (i, g) in groups.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({i}) var<storage, read> {}_in: array<{}>;\n",
            g.name,
            g.elem_ty()
        ));
    }
    for (i, g) in groups.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({}) var<storage, read_write> {}_out: array<{}>;\n",
            n + i,
            g.name,
            g.elem_ty()
        ));
    }
    // Scalar uniforms at binding 2n..2n+u — 1-element storage arrays.
    for (u, un) in uniform_names.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({}) var<storage, read> {un}_u: array<f32>;\n",
            2 * n + u
        ));
    }

    // Materialize each field once: `let <p>_<field> = <group>_in[i]{.field}?;`.
    let mut materialize = String::new();
    for g in groups {
        for f in &g.fields {
            if g.is_multi() {
                materialize.push_str(&format!(
                    "    let {param_name}_{f} = {}_in[i].{f};\n",
                    g.name
                ));
            } else {
                materialize.push_str(&format!("    let {param_name}_{f} = {}_in[i];\n", g.name));
            }
        }
    }

    // `#[gpu]` helper functions reachable from the kernel (GPU-LBM-5), emitted as
    // WGSL `fn`s before the bindings.
    let (helper_defs, helper_names) = emit_helpers(func, helpers)?;

    // The body is an optional sequence of `let` bindings (GPU-SLIP-1) followed by
    // the struct-valued return. Lower each `let <name> = <expr>;` to a WGSL `let`,
    // registering `name` so later bindings (and the return) resolve it as a scalar
    // local; then store each group's fields from the return struct. This is what
    // lets the real LBM `collide` body (`let rho`/`ux`/`uy` + the equilibrium
    // terms) run without hand-flattening it into one nested expression.
    let (lets, ret) = kernel_body_parts(func, "a struct expression")?;
    let mut locals: HashSet<String> = HashSet::new();
    let mut let_decls = String::new();
    for (name, value) in lets {
        let ctx = SoaCtx {
            param_name,
            groups,
            helpers: &helper_names,
            uniforms: &uniform_set,
            locals: &locals,
        };
        let rhs = lower_soa_expr(value, &ctx)?;
        let_decls.push_str(&format!("    let {name} = {rhs};\n"));
        locals.insert(name.to_string());
    }
    let ctx = SoaCtx {
        param_name,
        groups,
        helpers: &helper_names,
        uniforms: &uniform_set,
        locals: &locals,
    };
    let stores = lower_struct_return(ret, &ctx)?;

    // arrayLength guard keys off the first input buffer (all equal length).
    let guard_group = &groups[0].name;

    Ok(format!(
        "{helper_defs}{structs}{decls}\n\
         @compute @workgroup_size({WORKGROUP_SIZE})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   if (i >= arrayLength(&{guard_group}_in)) {{ return; }}\n\
         {materialize}{let_decls}{stores}\
         }}\n"
    ))
}

/// Lowering context for a struct-SoA `#[gpu]` kernel body. Every field is a
/// borrow, so the context is `Copy` and threads cheaply through the recursive
/// lowering (mirrors [`StencilCtx`]). `locals` is the growing set of `let`-bound
/// scalar names (GPU-SLIP-1) — an identifier in it lowers to itself.
#[derive(Clone, Copy)]
struct SoaCtx<'a> {
    /// The struct-buffer element parameter name (`n` in `n.f0`).
    param_name: &'a str,
    groups: &'a [SoaGpuGroup],
    helpers: &'a HashSet<String>,
    uniforms: &'a HashSet<String>,
    /// `let`-bound scalar locals in scope; each lowers to its own WGSL name.
    locals: &'a HashSet<String>,
}

/// A GPU kernel body split into its leading `let` bindings (`(name, init)` in
/// source order) and the final return expression — all borrowed from the
/// function AST. Shared by the struct-SoA (GPU-SLIP-1) and stencil
/// (GPU-SLIP-2a) emitters.
type KernelBody<'a> = (Vec<(&'a str, &'a Expr)>, &'a Expr);

/// Split a GPU kernel body into its leading `let` bindings and the final return
/// expression. Each statement before the return must be a simple
/// `let <name> = <expr>;` (no `mut`, no destructuring, no statement-form early
/// `return`); the return is the block's tail expression or a trailing
/// `return <expr>;`. Body-shape-generic — used by both the struct-SoA and
/// stencil emitters.
fn kernel_body_parts<'a>(func: &'a Function, tail_desc: &str) -> Result<KernelBody<'a>, WgslError> {
    let block = &func.body;
    let (stmts, ret) = if let Some(tail) = &block.final_expr {
        (block.stmts.as_slice(), tail.as_ref())
    } else if let Some((last, init)) = block.stmts.split_last() {
        if let StmtKind::Expr(Expr {
            kind: ExprKind::Return(Some(inner)),
            ..
        }) = &last.kind
        {
            (init, inner.as_ref())
        } else {
            return Err(WgslError::UnsupportedBody(format!(
                "a GPU kernel body must end in {tail_desc} or `return <expr>;`"
            )));
        }
    } else {
        return Err(WgslError::UnsupportedBody(
            "a GPU kernel body is empty".to_string(),
        ));
    };
    let mut lets: Vec<(&str, &Expr)> = Vec::new();
    for stmt in stmts {
        match &stmt.kind {
            StmtKind::Let {
                is_mut: false,
                pattern,
                value,
                ..
            } => {
                if let PatternKind::Binding(name) = &pattern.kind {
                    reject_reserved_local(name)?;
                    // WGSL forbids redeclaring a name in the same scope, so a
                    // shadowing `let` would emit a shader that only fails inside
                    // the driver at dispatch. Reject it here, where the
                    // diagnostic can still point at the kernel.
                    if lets.iter().any(|(n, _)| *n == name.as_str()) {
                        return Err(WgslError::UnsupportedBody(format!(
                            "a GPU kernel cannot shadow the local `{name}` — WGSL has no \
                             same-scope redeclaration; rename the second binding"
                        )));
                    }
                    lets.push((name.as_str(), value));
                } else {
                    return Err(WgslError::UnsupportedBody(
                        "a GPU kernel `let` must bind a simple name (no destructuring)".to_string(),
                    ));
                }
            }
            StmtKind::Let { is_mut: true, .. } => {
                return Err(WgslError::UnsupportedBody(
                    "a GPU kernel `let mut` is not supported yet — mutable locals arrive with \
                     loop support (B-2026-08-18-40); use an immutable `let`"
                        .to_string(),
                ));
            }
            _ => {
                return Err(WgslError::UnsupportedBody(
                    "a GPU kernel body supports only `let` bindings before the final expression"
                        .to_string(),
                ));
            }
        }
    }
    Ok((lets, ret))
}

/// Names the generated shader's own wrapper occupies, plus the WGSL keywords a
/// Kāra identifier could plausibly collide with. A kernel local spelled one of
/// these emits a shader that fails to compile inside the GPU driver — a runtime
/// failure a long way from its cause — so it is rejected here instead. The
/// wrapper set (`i` / `gid` / `input` / `output` / `main`) is what the emitters
/// declare around the body.
const WGSL_RESERVED_LOCALS: &[&str] = &[
    // Generated-wrapper names.
    "i",
    "gid",
    "input",
    "output",
    "main", // WGSL keywords a Kāra local could realistically use.
    "var",
    "let",
    "const",
    "fn",
    "struct",
    "array",
    "loop",
    "break",
    "continue",
    "discard",
    "return",
    "if",
    "else",
    "switch",
    "case",
    "default",
    "while",
    "for",
    "true",
    "false",
    "bool",
    "f32",
    "i32",
    "u32",
    "f16",
    "vec2",
    "vec3",
    "vec4",
    "select",
    "workgroup",
    "storage",
    "uniform",
    "private",
    "function",
];

fn reject_reserved_local(name: &str) -> Result<(), WgslError> {
    if WGSL_RESERVED_LOCALS.contains(&name) {
        return Err(WgslError::UnsupportedBody(format!(
            "a GPU kernel local cannot be named `{name}` — the generated WGSL shader reserves \
             that identifier; rename the binding"
        )));
    }
    Ok(())
}

/// Emit the WGSL for a **stencil** `#[gpu]` kernel
/// `fn k(buf: Vec[S], i: i64, ...uniforms) -> S` (GPU-LBM-6). Where the
/// element-wise SoA kernel materializes the thread's *own* element
/// (`<param>_<field>`), a stencil reads *neighbours* by indexing the buffer
/// directly: `buf[j].field` → `<group>_in[<j>]{.field}`, the index parameter →
/// the thread index `i32(gid.x)`, and `buf.len()` → `i32(arrayLength(&<first>_in))`.
/// The whole input buffer is already bound read-only (bindings are identical to
/// the element-wise SoA kernel), so **no runtime change is needed** — the body
/// can simply address any element. This is what the LBM `stream` kernel needs
/// (each cell reads its 3×3 neighbourhood).
fn emit_kernel_stencil(
    func: &Function,
    groups: &[SoaGpuGroup],
    helpers: &[&Function],
) -> Result<String, WgslError> {
    // params: [buffer: Vec[S]] [index: integer] [uniforms: f32...].
    if func.params.len() < 2 {
        return Err(WgslError::UnsupportedSignature(
            "a stencil GPU kernel needs a buffer parameter and an index parameter".to_string(),
        ));
    }
    let buf_name = func.params[0].name().ok_or_else(|| {
        WgslError::UnsupportedSignature(
            "the GPU stencil buffer parameter must be a plain binding".to_string(),
        )
    })?;
    let idx_name = func.params[1].name().ok_or_else(|| {
        WgslError::UnsupportedSignature(
            "the GPU stencil index parameter must be a plain binding".to_string(),
        )
    })?;
    let mut uniform_names: Vec<String> = Vec::new();
    for u in &func.params[2..] {
        wgsl_scalar(&u.ty, "uniform parameter")?;
        let un = u.name().ok_or_else(|| {
            WgslError::UnsupportedSignature(
                "a GPU uniform parameter must be a plain binding".to_string(),
            )
        })?;
        uniform_names.push(un.to_string());
    }
    let uniform_set: HashSet<String> = uniform_names.iter().cloned().collect();
    if groups.is_empty() {
        return Err(WgslError::UnsupportedSignature(
            "a struct GPU kernel needs at least one layout group".to_string(),
        ));
    }
    for g in groups {
        if g.fields.is_empty() {
            return Err(WgslError::UnsupportedSignature(format!(
                "layout group `{}` has no fields",
                g.name
            )));
        }
    }
    let n = groups.len();

    // WGSL sub-struct definitions for multi-field groups.
    let mut structs = String::new();
    for g in groups {
        if g.is_multi() {
            let members = g
                .fields
                .iter()
                .map(|f| format!("{f}: f32"))
                .collect::<Vec<_>>()
                .join(", ");
            structs.push_str(&format!("struct {} {{ {members} }};\n", g.wgsl_struct()));
        }
    }

    // Bindings: inputs 0..n, outputs n..2n, uniforms 2n..2n+u — identical to the
    // element-wise SoA kernel. Binding the whole input read-only is exactly what
    // lets the body address arbitrary neighbours.
    let mut decls = String::new();
    for (i, g) in groups.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({i}) var<storage, read> {}_in: array<{}>;\n",
            g.name,
            g.elem_ty()
        ));
    }
    for (i, g) in groups.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({}) var<storage, read_write> {}_out: array<{}>;\n",
            n + i,
            g.name,
            g.elem_ty()
        ));
    }
    for (u, un) in uniform_names.iter().enumerate() {
        decls.push_str(&format!(
            "@group(0) @binding({}) var<storage, read> {un}_u: array<f32>;\n",
            2 * n + u
        ));
    }

    // `#[gpu]` helper functions reachable from the kernel (GPU-LBM-5).
    let (helper_defs, helper_names) = emit_helpers(func, helpers)?;

    let first_group = groups[0].name.clone();
    // Body: leading `let` bindings (GPU-SLIP-2a) then the struct-valued return.
    // Each `let` lowers to a WGSL `let`, its name registered in `locals` so later
    // bindings and the return resolve it to itself. The context is rebuilt per
    // step (it borrows the growing `locals`); every other field is a stable
    // borrow, so this is cheap.
    let (lets, ret) = kernel_body_parts(func, "a struct expression")?;
    let mut locals: HashSet<String> = HashSet::new();
    let mut cell_aliases: HashMap<String, String> = HashMap::new();
    let mut let_decls = String::new();
    for (name, value) in lets {
        let ctx = StencilCtx {
            buf: buf_name,
            idx: idx_name,
            groups,
            helpers: &helper_names,
            uniforms: &uniform_set,
            first_group: &first_group,
            locals: &locals,
            cell_aliases: &cell_aliases,
        };
        // A whole-cell alias `let c = buf[<idx>]` has no single WGSL value (the
        // cell spans one binding per group), so it emits no `let`; record the
        // lowered index and resolve `c.field` per group later (GPU-SLIP-2c).
        if let Some(idx_expr) = whole_cell_read(value, buf_name) {
            let idx_wgsl = lower_stencil_expr(idx_expr, &ctx)?;
            cell_aliases.insert(name.to_string(), idx_wgsl);
        } else {
            let rhs = lower_stencil_expr(value, &ctx)?;
            let_decls.push_str(&format!("    let {name} = {rhs};\n"));
            locals.insert(name.to_string());
        }
    }
    let ctx = StencilCtx {
        buf: buf_name,
        idx: idx_name,
        groups,
        helpers: &helper_names,
        uniforms: &uniform_set,
        first_group: &first_group,
        locals: &locals,
        cell_aliases: &cell_aliases,
    };
    let stores = lower_stencil_return(ret, &ctx)?;

    // The thread owns output element `gi`; the kernel's index parameter is the
    // same index as `i32` (WGSL array subscripts want i32/u32, and neighbour
    // arithmetic like `i - 1` must be signed to bounds-check cleanly).
    Ok(format!(
        "{helper_defs}{structs}{decls}\n\
         @compute @workgroup_size({WORKGROUP_SIZE})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let gi = gid.y * {DISPATCH_X_SPAN}u + gid.x;\n\
         \x20   if (gi >= arrayLength(&{first_group}_in)) {{ return; }}\n\
         \x20   let {idx_name} = i32(gi);\n\
         {let_decls}{stores}\
         }}\n"
    ))
}

/// Lowering context for a stencil kernel body (GPU-LBM-6). Every field is a
/// borrow, so the context is `Copy` and threads cheaply through the recursive
/// lowering (and the shared [`lower_call`] closure).
#[derive(Clone, Copy)]
struct StencilCtx<'a> {
    /// The buffer parameter name (`buf` in `buf[j].field`).
    buf: &'a str,
    /// The index parameter name — maps to the thread index `i32(gid.x)`.
    idx: &'a str,
    groups: &'a [SoaGpuGroup],
    helpers: &'a HashSet<String>,
    uniforms: &'a HashSet<String>,
    /// `groups[0].name`, for the `arrayLength` behind `buf.len()`.
    first_group: &'a str,
    /// `let`-bound scalar locals in scope (GPU-SLIP-2a); each lowers to itself.
    locals: &'a HashSet<String>,
    /// `let`-bound whole-cell aliases (GPU-SLIP-2c): `let c = buf[<idx>]` maps
    /// `c` to the lowered index string, so `c.field` reads
    /// `<group_of_field>_in[<idx>].field` (the LBM `stream` centre cell `c`).
    cell_aliases: &'a HashMap<String, String>,
}

/// If `expr` is a whole-cell buffer read `buf[<idx>]` (an `Index` on the stencil
/// buffer identifier, with no field access), return the index expression — the
/// RHS shape of a cell-alias `let c = coll[idx(x, y, w)]` (GPU-SLIP-2c). `None`
/// otherwise.
fn whole_cell_read<'a>(expr: &'a Expr, buf: &str) -> Option<&'a Expr> {
    if let ExprKind::Index { object, index } = &expr.kind {
        if matches!(&object.kind, ExprKind::Identifier(b) if b == buf) {
            return Some(index);
        }
    }
    None
}

/// Lower a stencil kernel's struct-valued return into one output store per
/// group, keyed by the thread's output index `gi`. The return is a struct
/// literal or an `if`/`else` over struct literals — a stencil has no
/// whole-buffer identity return (the parameter is the buffer, not an element).
fn lower_stencil_return(expr: &Expr, ctx: &StencilCtx) -> Result<String, WgslError> {
    let mut out = String::new();
    for g in ctx.groups {
        if g.is_multi() {
            let vals = g
                .fields
                .iter()
                .map(|f| stencil_field_wgsl(expr, f, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            out.push_str(&format!(
                "    {}_out[gi] = {}({});\n",
                g.name,
                g.wgsl_struct(),
                vals.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "    {}_out[gi] = {};\n",
                g.name,
                stencil_field_wgsl(expr, &g.fields[0], ctx)?
            ));
        }
    }
    Ok(out)
}

/// WGSL for struct field `field` of a stencil kernel's struct-valued return —
/// the stencil analogue of [`struct_field_wgsl`]: a struct literal lowers the
/// field's init; a struct-valued `if` becomes a per-field `select`.
fn stencil_field_wgsl(expr: &Expr, field: &str, ctx: &StencilCtx) -> Result<String, WgslError> {
    match &expr.kind {
        ExprKind::StructLiteral { fields, spread, .. } => {
            if spread.is_some() {
                return Err(WgslError::UnsupportedBody(
                    "struct-literal spread (`..`) is not supported in a GPU kernel".to_string(),
                ));
            }
            let init = fields.iter().find(|f| f.name == field).ok_or_else(|| {
                WgslError::UnsupportedBody(format!(
                    "the returned struct is missing field `{field}`"
                ))
            })?;
            lower_stencil_expr(&init.value, ctx)
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_stencil_expr(condition, ctx)?;
            let t = stencil_field_wgsl(then_e, field, ctx)?;
            let e = stencil_field_wgsl(else_e, field, ctx)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        _ => Err(WgslError::UnsupportedBody(
            "a stencil GPU kernel must return a struct literal or an `if`/`else` over struct \
             literals"
                .to_string(),
        )),
    }
}

/// Lower one stencil body expression. The scalar sources are neighbour reads
/// `buf[j].field` (→ `<group>_in[<j>]{.field}`), the index parameter (→ the
/// thread index), scalar uniforms (→ `<name>_u[0]`), and `buf.len()`
/// (→ `i32(arrayLength(&<first>_in))`). Arithmetic / comparison / value `if` /
/// helper calls reuse the shared lowering ([`lower_call`], [`if_branches`]) — an
/// index expression like `i - 1` lowers to i32 (the index param is `i32`), while
/// a value read like `buf[j].a` is `f32`; WGSL types each from the AST context.
fn lower_stencil_expr(expr: &Expr, ctx: &StencilCtx) -> Result<String, WgslError> {
    match &expr.kind {
        // Field read of a buffer cell — either an explicit neighbour read
        // `buf[j].field` → `<group_of_field>_in[<j>]{.field}`, or a whole-cell
        // alias `c.field` where `c` was bound `let c = buf[<idx>]` (GPU-SLIP-2c).
        ExprKind::FieldAccess { object, field } => {
            let idx_wgsl: Option<String> = match &object.kind {
                ExprKind::Index {
                    object: base,
                    index,
                } if matches!(&base.kind, ExprKind::Identifier(b) if b == ctx.buf) => {
                    Some(lower_stencil_expr(index, ctx)?)
                }
                ExprKind::Identifier(obj) => ctx.cell_aliases.get(obj).cloned(),
                _ => None,
            };
            let j = idx_wgsl.ok_or_else(|| {
                WgslError::UnsupportedBody(
                    "a stencil field read must be a neighbour read `buf[index].field` or a \
                     `let`-bound cell alias `c.field`"
                        .to_string(),
                )
            })?;
            let g = ctx
                .groups
                .iter()
                .find(|g| g.fields.iter().any(|gf| gf == field))
                .ok_or_else(|| {
                    WgslError::UnsupportedBody(format!(
                        "field `{field}` is not a layout group of the GPU stencil buffer"
                    ))
                })?;
            Ok(if g.is_multi() {
                format!("{}_in[{j}].{field}", g.name)
            } else {
                format!("{}_in[{j}]", g.name)
            })
        }
        // `buf.len()` → element count as i32.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } if method == "len"
            && args.is_empty()
            && matches!(&object.kind, ExprKind::Identifier(b) if b == ctx.buf) =>
        {
            Ok(format!("i32(arrayLength(&{}_in))", ctx.first_group))
        }
        // Scalar math intrinsic method (`e.sqrt()` → `sqrt(e)`) — GPU-SLIP-2a.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            let builtin = math_intrinsic_wgsl(method, args.len()).ok_or_else(|| {
                WgslError::UnsupportedBody(format!(
                    "method `.{method}()` is not supported in a stencil GPU kernel body"
                ))
            })?;
            Ok(format!("{builtin}({})", lower_stencil_expr(object, ctx)?))
        }
        // Numeric `as` cast (`e as f64` → `f32(e)`) — GPU-SLIP-2a.
        ExprKind::Cast { expr, ty } => {
            let ctor = cast_ctor(ty).ok_or_else(|| {
                WgslError::UnsupportedBody(
                    "unsupported `as` cast target in a stencil GPU kernel body".to_string(),
                )
            })?;
            Ok(format!("{ctor}({})", lower_stencil_expr(expr, ctx)?))
        }
        // The index parameter → the thread index (the `i32(gid.x)` local).
        ExprKind::Identifier(name) if name == ctx.idx => Ok(ctx.idx.to_string()),
        // A scalar uniform parameter → its 1-element storage buffer.
        ExprKind::Identifier(name) if ctx.uniforms.contains(name) => Ok(format!("{name}_u[0]")),
        // A `let`-bound scalar local (GPU-SLIP-2a) → its own WGSL name.
        ExprKind::Identifier(name) if ctx.locals.contains(name) => Ok(name.clone()),
        ExprKind::Integer(n, _) => Ok(n.to_string()),
        ExprKind::Float(f, _) => lower_float(*f),
        ExprKind::Binary { op, left, right } => {
            let op_str = binop_str(op)?;
            let l = lower_stencil_expr(left, ctx)?;
            let r = lower_stencil_expr(right, ctx)?;
            Ok(format!("({l} {op_str} {r})"))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => Ok(format!("-({})", lower_stencil_expr(operand, ctx)?)),
        // Post-lowering operator methods (`i - 1` → `i64.sub(i, 1)`) and helper calls.
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, &|e| lower_stencil_expr(e, ctx), ctx.helpers)
        }
        // Value `if c { a } else { b }` → WGSL `select(b, a, c)`.
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_stencil_expr(condition, ctx)?;
            let t = lower_stencil_expr(then_e, ctx)?;
            let e = lower_stencil_expr(else_e, ctx)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        ExprKind::Identifier(name) => Err(WgslError::UnsupportedBody(format!(
            "identifier `{name}` — a stencil GPU kernel body reads `buf[index].field`, the index, \
             a uniform, or `buf.len()`"
        ))),
        _ => Err(WgslError::UnsupportedBody(
            "unsupported expression in a stencil GPU kernel body (neighbour reads, the index, \
             numeric literals, `+ - * / %`, unary `-`, comparisons, value `if`/`else`, \
             `buf.len()`, helper calls)"
                .to_string(),
        )),
    }
}

/// Lower the kernel's struct-valued return into one output store per group: a
/// single-field group stores its field's value; a multi-field group stores a
/// `G_<name>(...)` constructor over its fields. The return may be a struct
/// literal, the whole-input parameter, or a struct-valued `if` (GPU-LBM-4b) —
/// see [`struct_field_wgsl`].
fn lower_struct_return(expr: &Expr, ctx: &SoaCtx) -> Result<String, WgslError> {
    let mut out = String::new();
    for g in ctx.groups {
        if g.is_multi() {
            let vals = g
                .fields
                .iter()
                .map(|f| struct_field_wgsl(expr, f, ctx))
                .collect::<Result<Vec<_>, _>>()?;
            out.push_str(&format!(
                "    {}_out[i] = {}({});\n",
                g.name,
                g.wgsl_struct(),
                vals.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "    {}_out[i] = {};\n",
                g.name,
                struct_field_wgsl(expr, &g.fields[0], ctx)?
            ));
        }
    }
    Ok(out)
}

/// WGSL for struct field `field` of a struct-VALUED expression:
/// - a struct literal `S { field: e, … }` → lower field `field`'s init;
/// - the whole-input parameter (`n`) → the field's materialized input value;
/// - a struct-valued `if c { S } else { S }` → per-field
///   `select(else.field, then.field, c)` (the LBM `collide` guard
///   `if rho <= 0 { n } else { … }`, decomposed to a per-field select with a
///   shared condition; GPU-LBM-4b).
fn struct_field_wgsl(expr: &Expr, field: &str, ctx: &SoaCtx) -> Result<String, WgslError> {
    match &expr.kind {
        ExprKind::StructLiteral { fields, spread, .. } => {
            if spread.is_some() {
                return Err(WgslError::UnsupportedBody(
                    "struct-literal spread (`..`) is not supported in a GPU kernel".to_string(),
                ));
            }
            let init = fields.iter().find(|f| f.name == field).ok_or_else(|| {
                WgslError::UnsupportedBody(format!(
                    "the returned struct is missing field `{field}`"
                ))
            })?;
            lower_soa_expr(&init.value, ctx)
        }
        ExprKind::Identifier(name) if name == ctx.param_name => {
            if ctx
                .groups
                .iter()
                .any(|g| g.fields.iter().any(|gf| gf == field))
            {
                Ok(format!("{}_{field}", ctx.param_name))
            } else {
                Err(WgslError::UnsupportedBody(format!(
                    "field `{field}` is not a layout group of the GPU kernel parameter"
                )))
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_soa_expr(condition, ctx)?;
            let t = struct_field_wgsl(then_e, field, ctx)?;
            let e = struct_field_wgsl(else_e, field, ctx)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        _ => Err(WgslError::UnsupportedBody(
            "a struct GPU kernel must return a struct literal, the input parameter, or an \
             `if`/`else` over those"
                .to_string(),
        )),
    }
}

/// Lower one body expression for the SoA case. Like [`lower_expr`] but the sole
/// scalar source is a `<param>.<field>` field access (→ the materialized
/// `<param>_<field>` local), since the whole-struct parameter has no scalar
/// WGSL form.
fn lower_soa_expr(expr: &Expr, ctx: &SoaCtx) -> Result<String, WgslError> {
    match &expr.kind {
        ExprKind::FieldAccess { object, field } => {
            if let ExprKind::Identifier(obj) = &object.kind {
                if obj == ctx.param_name {
                    if ctx
                        .groups
                        .iter()
                        .any(|g| g.fields.iter().any(|gf| gf == field))
                    {
                        return Ok(format!("{}_{field}", ctx.param_name));
                    }
                    return Err(WgslError::UnsupportedBody(format!(
                        "field `{field}` is not a layout group of the GPU kernel parameter"
                    )));
                }
            }
            Err(WgslError::UnsupportedBody(
                "only `<param>.<field>` field access is supported in a struct GPU kernel body"
                    .to_string(),
            ))
        }
        ExprKind::Integer(n, _) => Ok(n.to_string()),
        ExprKind::Float(f, _) => lower_float(*f),
        ExprKind::Binary { op, left, right } => {
            let op_str = binop_str(op)?;
            let l = lower_soa_expr(left, ctx)?;
            let r = lower_soa_expr(right, ctx)?;
            Ok(format!("({l} {op_str} {r})"))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            let inner = lower_soa_expr(operand, ctx)?;
            Ok(format!("-({inner})"))
        }
        // Post-lowering operator methods (`a + b` → `<type>.add(a, b)`) and
        // `#[gpu]` helper calls — the SoA emitter runs at codegen, after lowering.
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, &|e| lower_soa_expr(e, ctx), ctx.helpers)
        }
        // Value `if c { a } else { b }` → WGSL `select(b, a, c)` (GPU-LBM-4).
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_soa_expr(condition, ctx)?;
            let t = lower_soa_expr(then_e, ctx)?;
            let e = lower_soa_expr(else_e, ctx)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        // A `let`-bound scalar local (GPU-SLIP-1) → its own WGSL name. Checked
        // before uniforms so a local may shadow a uniform, matching Kāra scoping.
        ExprKind::Identifier(name) if ctx.locals.contains(name) => Ok(name.clone()),
        // A bare identifier naming a scalar uniform parameter → `<name>_u[0]`.
        ExprKind::Identifier(name) if ctx.uniforms.contains(name) => Ok(format!("{name}_u[0]")),
        ExprKind::Identifier(name) => Err(WgslError::UnsupportedBody(format!(
            "identifier `{name}` — a struct GPU kernel body accesses `<param>.<field>`, a \
             uniform, or a `let` local, not the whole struct value"
        ))),
        // Scalar math intrinsic method (`e.sqrt()` → `sqrt(e)`) — GPU-SLIP-2a,
        // now in the element-wise SoA body too (previously stencil + scalar only).
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            let builtin = math_intrinsic_wgsl(method, args.len()).ok_or_else(|| {
                WgslError::UnsupportedBody(format!(
                    "method `.{method}()` is not supported in a struct GPU kernel body"
                ))
            })?;
            Ok(format!("{builtin}({})", lower_soa_expr(object, ctx)?))
        }
        // Numeric `as` cast (`e as f64` → `f32(e)`) — GPU-SLIP-2a.
        ExprKind::Cast { expr, ty } => {
            let ctor = cast_ctor(ty).ok_or_else(|| {
                WgslError::UnsupportedBody(
                    "unsupported `as` cast target in a struct GPU kernel body".to_string(),
                )
            })?;
            Ok(format!("{ctor}({})", lower_soa_expr(expr, ctx)?))
        }
        _ => Err(WgslError::UnsupportedBody(
            "unsupported expression in a struct GPU kernel body (field access, numeric \
             literals, `+ - * / %`, unary `-`, comparisons, value `if`/`else`, `.sqrt()` / \
             `.abs()` / `.floor()` / `.ceil()`, `as` casts, helper calls)"
                .to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a single top-level `fn` out of `src` for emitter tests.
    fn parse_kernel(src: &str) -> Function {
        let result = crate::parse(src);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        for item in result.program.items {
            if let crate::ast::Item::Function(f) = item {
                return f;
            }
        }
        panic!("no function item found in source");
    }

    /// Parse all top-level `fn`s (for multi-function helper tests).
    fn parse_fns(src: &str) -> Vec<Function> {
        let result = crate::parse(src);
        assert!(
            result.errors.is_empty(),
            "parse errors: {:?}",
            result.errors
        );
        result
            .program
            .items
            .into_iter()
            .filter_map(|it| match it {
                crate::ast::Item::Function(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn emits_scalar_helper_call() {
        // `upd` calls the `#[gpu]` helper `sq` — GPU-LBM-5.
        let fns = parse_fns(
            "#[gpu]\nfn sq(v: f32) -> f32 { v * v }\n\
             #[gpu]\nfn upd(x: f32) -> f32 { sq(x) + 1.0 }\n",
        );
        let sq = fns.iter().find(|f| f.name == "sq").unwrap();
        let upd = fns.iter().find(|f| f.name == "upd").unwrap();
        let wgsl = emit_kernel(upd, &[sq]).unwrap();
        assert!(
            wgsl.contains("fn sq(v: f32) -> f32 { return (v * v); }"),
            "{wgsl}"
        );
        assert!(wgsl.contains("output[i] = (sq(input[i]) + 1.0);"), "{wgsl}");
    }

    #[test]
    fn emits_bool_helper_with_let_and_mixed_params() {
        // GPU-SLIP-2b: a helper may return `bool`, take mixed-type params
        // (`i64` maps to WGSL `i32`, `f32` stays), and bind `let` locals in its
        // body — the `is_solid(x, y, s) -> bool` shape.
        let fns = parse_fns(
            "#[gpu]\nfn edge(x: i64, s: f32) -> bool { let lim = s * 2.0; x < 0 or x >= 3 or s > lim }\n\
             #[gpu]\nfn pick(p: Cell, s: f32) -> Cell { Cell { a: if edge(0, s) { p.a } else { p.b }, b: p.b } }\n",
        );
        let edge = fns.iter().find(|f| f.name == "edge").unwrap();
        let pick = fns.iter().find(|f| f.name == "pick").unwrap();
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(pick, &groups, &[edge]).unwrap();
        // Helper: bool return, `i32`/`f32` params (not hardcoded f32), a `let`.
        assert!(
            wgsl.contains(
                "fn edge(x: i32, s: f32) -> bool { let lim = (s * 2.0); return (((x < 0) || (x >= 3)) || (s > lim)); }"
            ),
            "{wgsl}"
        );
        // Called in the SoA return's per-field select condition.
        assert!(
            wgsl.contains("ga_out[i] = select(p_b, p_a, edge(0, s_u[0]));"),
            "{wgsl}"
        );
    }

    #[test]
    fn rejects_bad_helper_return_type() {
        // A helper returning a non-scalar/bool (e.g. a struct) is still rejected.
        let fns = parse_fns(
            "#[gpu]\nfn mk(v: f32) -> Cell { Cell { a: v, b: v } }\n\
             #[gpu]\nfn k(x: f32) -> f32 { mk(x).a }\n",
        );
        let mk = fns.iter().find(|f| f.name == "mk").unwrap();
        let k = fns.iter().find(|f| f.name == "k").unwrap();
        let err = emit_kernel(k, &[mk]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedSignature(_)), "{err:?}");
    }

    #[test]
    fn emits_soa_helper_call() {
        let fns = parse_fns(
            "#[gpu]\nfn sq(v: f32) -> f32 { v * v }\n\
             #[gpu]\nfn upd(x: Cell) -> Cell { Cell { a: sq(x.a), b: x.b } }\n",
        );
        let sq = fns.iter().find(|f| f.name == "sq").unwrap();
        let upd = fns.iter().find(|f| f.name == "upd").unwrap();
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(upd, &groups, &[sq]).unwrap();
        assert!(
            wgsl.contains("fn sq(v: f32) -> f32 { return (v * v); }"),
            "{wgsl}"
        );
        assert!(wgsl.contains("ga_out[i] = sq(x_a);"), "{wgsl}");
    }

    #[test]
    fn emits_soa_scalar_uniform() {
        // GPU-LBM-2: a scalar uniform param `k` bound at `@binding(2n)` and read
        // as `k_u[0]`.
        let func = parse_kernel(
            "#[gpu]\nfn scale(x: Cell, k: f32) -> Cell { Cell { a: x.a * k, b: x.b } }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(
            wgsl.contains("@group(0) @binding(4) var<storage, read> k_u: array<f32>;"),
            "{wgsl}"
        );
        assert!(wgsl.contains("ga_out[i] = (x_a * k_u[0]);"), "{wgsl}");
        assert!(wgsl.contains("gb_out[i] = x_b;"), "{wgsl}");
    }

    #[test]
    fn emits_soa_let_bindings() {
        // GPU-SLIP-1: `let` locals in a struct-SoA body lower to WGSL `let`s and
        // resolve to themselves in later expressions and the return. This is the
        // shape the real LBM `collide` body needs (`let rho`/`ux` + equilibrium).
        let func = parse_kernel(
            "#[gpu]\nfn k(x: Cell, om: f32) -> Cell {\n\
             \x20   let s = x.a + x.b;\n\
             \x20   let scaled = s * om;\n\
             \x20   Cell { a: x.a + scaled, b: scaled }\n\
             }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(wgsl.contains("let s = (x_a + x_b);"), "{wgsl}");
        assert!(wgsl.contains("let scaled = (s * om_u[0]);"), "{wgsl}");
        assert!(wgsl.contains("ga_out[i] = (x_a + scaled);"), "{wgsl}");
        assert!(wgsl.contains("gb_out[i] = scaled;"), "{wgsl}");
    }

    #[test]
    fn emits_soa_math_intrinsic() {
        // GPU-SLIP-4 residual: `.sqrt()` (and `.abs()`/`.floor()`/`.ceil()`) now
        // lower in the element-wise SoA body too, not just the stencil + scalar
        // paths — a common-case gap surfaced by the compute-heavy re-bench.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: Cell) -> Cell {\n\
             \x20   let m = (x.a * x.a + x.b * x.b).sqrt();\n\
             \x20   Cell { a: m, b: x.b }\n\
             }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(
            wgsl.contains("let m = sqrt(((x_a * x_a) + (x_b * x_b)));"),
            "{wgsl}"
        );
        assert!(wgsl.contains("ga_out[i] = m;"), "{wgsl}");
    }

    #[test]
    fn soa_let_then_guarded_return() {
        // A `let` local feeding a struct-valued `if` guard — the `collide` shape
        // `if rho <= 0 { n } else { <relaxed> }` decomposed to per-field `select`.
        let func = parse_kernel(
            "#[gpu]\nfn k(n: Cell) -> Cell {\n\
             \x20   let rho = n.a + n.b;\n\
             \x20   if rho <= 0.0 { n } else { Cell { a: n.a + rho, b: n.b } }\n\
             }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(wgsl.contains("let rho = (n_a + n_b);"), "{wgsl}");
        assert!(
            wgsl.contains("ga_out[i] = select((n_a + rho), n_a, (rho <= 0.0));"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("gb_out[i] = select(n_b, n_b, (rho <= 0.0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn emits_transitive_helpers_callee_first() {
        // `outer` → `mid` → `inner`; emitted callee-before-caller.
        let fns = parse_fns(
            "#[gpu]\nfn inner(v: f32) -> f32 { v + 1.0 }\n\
             #[gpu]\nfn mid(v: f32) -> f32 { inner(v) * 2.0 }\n\
             #[gpu]\nfn outer(x: f32) -> f32 { mid(x) }\n",
        );
        let inner = fns.iter().find(|f| f.name == "inner").unwrap();
        let mid = fns.iter().find(|f| f.name == "mid").unwrap();
        let outer = fns.iter().find(|f| f.name == "outer").unwrap();
        let wgsl = emit_kernel(outer, &[inner, mid]).unwrap();
        let ip = wgsl.find("fn inner").unwrap();
        let mp = wgsl.find("fn mid").unwrap();
        assert!(ip < mp, "inner must be declared before mid:\n{wgsl}");
        assert!(wgsl.contains("output[i] = mid(input[i]);"), "{wgsl}");
    }

    #[test]
    fn emits_the_canonical_double_kernel() {
        let func = parse_kernel("#[gpu]\nfn double(x: f32) -> f32 { x * 2.0 }\n");
        let wgsl = emit_kernel(&func, &[]).expect("double kernel should lower");

        // The fixed boilerplate the runtime spine's binding layout requires.
        assert!(wgsl.contains("@group(0) @binding(0) var<storage, read>       input:  array<f32>;"));
        assert!(wgsl.contains("@group(0) @binding(1) var<storage, read_write> output: array<f32>;"));
        assert!(wgsl.contains("@compute @workgroup_size(64)"));
        assert!(wgsl.contains("fn main(@builtin(global_invocation_id) gid: vec3<u32>)"));
        assert!(wgsl.contains("let i = gid.y * 4194240u + gid.x;"));
        assert!(wgsl.contains("if (i >= arrayLength(&input)) { return; }"));
        // The one kernel-specific line: `x` → `input[i]`, `2.0` preserved.
        assert!(
            wgsl.contains("output[i] = (input[i] * 2.0);"),
            "generated body line missing:\n{wgsl}"
        );
    }

    #[test]
    fn lowers_nested_arithmetic_with_precedence_parens() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x * 3.0 + 1.0 }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        // `x * 3.0 + 1.0` parses as `(x * 3.0) + 1.0`; parens preserve it.
        assert!(
            wgsl.contains("output[i] = ((input[i] * 3.0) + 1.0);"),
            "{wgsl}"
        );
    }

    #[test]
    fn lowers_all_arithmetic_operators() {
        for (src_op, wgsl_op) in [("+", "+"), ("-", "-"), ("*", "*"), ("/", "/"), ("%", "%")] {
            let func = parse_kernel(&format!(
                "#[gpu]\nfn k(x: f32) -> f32 {{ x {src_op} 2.0 }}\n"
            ));
            let wgsl = emit_kernel(&func, &[]).unwrap();
            assert!(
                wgsl.contains(&format!("output[i] = (input[i] {wgsl_op} 2.0);")),
                "op {src_op}:\n{wgsl}"
            );
        }
    }

    #[test]
    fn lowers_unary_negation() {
        let func = parse_kernel("#[gpu]\nfn neg(x: f32) -> f32 { -x }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("output[i] = -(input[i]);"), "{wgsl}");
    }

    #[test]
    fn lowers_via_explicit_return() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { return x * 2.0; }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("output[i] = (input[i] * 2.0);"), "{wgsl}");
    }

    #[test]
    fn integer_literal_lowers_without_trailing_decimal() {
        // An integer literal in an f32 expression is a WGSL abstract-int that
        // converts to f32 in a float context — emit it verbatim.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x + 5.0 * 2.0 }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("(5.0 * 2.0)"), "{wgsl}");
    }

    #[test]
    fn rejects_multiple_parameters() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32, y: f32) -> f32 { x + y }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedSignature(_)), "{err:?}");
    }

    #[test]
    fn rejects_zero_parameters() {
        let func = parse_kernel("#[gpu]\nfn k() -> f32 { 1.0 }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedSignature(_)), "{err:?}");
    }

    #[test]
    fn lowers_i32_kernel_over_i32_array() {
        // Integer scalars are WGSL-native (4-byte) — `array<i32>`, integer
        // literal preserved.
        let func = parse_kernel("#[gpu]\nfn k(x: i32) -> i32 { x.wrapping_mul(2) }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("input:  array<i32>;"), "{wgsl}");
        assert!(wgsl.contains("output: array<i32>;"), "{wgsl}");
        assert!(wgsl.contains("output[i] = (input[i] * 2);"), "{wgsl}");
    }

    #[test]
    fn lowers_u32_kernel_over_u32_array() {
        let func = parse_kernel("#[gpu]\nfn k(x: u32) -> u32 { x.wrapping_add(1) }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("input:  array<u32>;"), "{wgsl}");
        assert!(wgsl.contains("output[i] = (input[i] + 1);"), "{wgsl}");
    }

    #[test]
    fn rejects_mismatched_param_and_return_scalar() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> i32 { 0 }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedSignature(_)), "{err:?}");
    }

    #[test]
    fn rejects_non_wgsl_scalar_element() {
        // WGSL has no native i64/f64 — those stay a later increment.
        for ty in ["i64", "f64", "bool", "u8"] {
            let func = parse_kernel(&format!("#[gpu]\nfn k(x: {ty}) -> {ty} {{ x }}\n"));
            let err = emit_kernel(&func, &[]).unwrap_err();
            assert!(
                matches!(err, WgslError::UnsupportedSignature(_)),
                "{ty}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_missing_return_type() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) { let _y = x; }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedSignature(_)), "{err:?}");
    }

    #[test]
    fn rejects_unknown_identifier() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { y * 2.0 }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    #[test]
    fn accepts_body_with_locals() {
        // Was `rejects_body_with_locals`: the single-expression floor is lifted
        // (B-2026-08-18-40), so an unannotated `let` now lowers to a WGSL `let`
        // and WGSL infers its type from the initializer.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { let y = x * 2.0; y }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let y = (input[i] * 2.0);"), "{wgsl}");
        assert!(wgsl.contains("output[i] = y;"), "{wgsl}");
    }

    #[test]
    fn rejects_non_arithmetic_operator() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x & 2.0 }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    // ── CG-4 struct-SoA emitter ──────────────────────────────────

    fn particle_groups() -> Vec<SoaGpuGroup> {
        vec![
            SoaGpuGroup {
                name: "gp".into(),
                fields: vec!["pos".into()],
            },
            SoaGpuGroup {
                name: "gv".into(),
                fields: vec!["vel".into()],
            },
        ]
    }

    #[test]
    fn emits_soa_particle_step() {
        let func = parse_kernel(
            "#[gpu]\nfn step(p: Particle) -> Particle { Particle { pos: p.pos + p.vel, vel: p.vel } }\n",
        );
        let wgsl =
            emit_kernel_soa(&func, &particle_groups(), &[]).expect("soa kernel should lower");
        // Single-field groups bind plain `array<f32>`; inputs 0..n, outputs n..2n.
        assert!(
            wgsl.contains("@group(0) @binding(0) var<storage, read> gp_in: array<f32>;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(1) var<storage, read> gv_in: array<f32>;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(2) var<storage, read_write> gp_out: array<f32>;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(3) var<storage, read_write> gv_out: array<f32>;"),
            "{wgsl}"
        );
        assert!(wgsl.contains("let p_pos = gp_in[i];"), "{wgsl}");
        assert!(wgsl.contains("let p_vel = gv_in[i];"), "{wgsl}");
        assert!(
            wgsl.contains("if (i >= arrayLength(&gp_in)) { return; }"),
            "{wgsl}"
        );
        assert!(wgsl.contains("gp_out[i] = (p_pos + p_vel);"), "{wgsl}");
        assert!(wgsl.contains("gv_out[i] = p_vel;"), "{wgsl}");
    }

    #[test]
    fn emits_soa_multi_field_group() {
        // GPU-LBM-3: group `ab { a, b }` is a multi-field group → a WGSL sub-struct
        // binding; group `cg { c }` stays a plain `array<f32>`.
        let func = parse_kernel(
            "#[gpu]\nfn upd(x: Cell) -> Cell { Cell { a: x.a + x.c, b: x.b, c: x.c } }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ab".into(),
                fields: vec!["a".into(), "b".into()],
            },
            SoaGpuGroup {
                name: "cg".into(),
                fields: vec!["c".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(wgsl.contains("struct G_ab { a: f32, b: f32 };"), "{wgsl}");
        assert!(
            wgsl.contains("@group(0) @binding(0) var<storage, read> ab_in: array<G_ab>;"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("@group(0) @binding(1) var<storage, read> cg_in: array<f32>;"),
            "{wgsl}"
        );
        // Multi-field group → `.field` access; single-field → scalar.
        assert!(wgsl.contains("let x_a = ab_in[i].a;"), "{wgsl}");
        assert!(wgsl.contains("let x_b = ab_in[i].b;"), "{wgsl}");
        assert!(wgsl.contains("let x_c = cg_in[i];"), "{wgsl}");
        // Multi-field output → struct constructor; single-field → scalar store.
        assert!(
            wgsl.contains("ab_out[i] = G_ab((x_a + x_c), x_b);"),
            "{wgsl}"
        );
        assert!(wgsl.contains("cg_out[i] = x_c;"), "{wgsl}");
    }

    #[test]
    fn emits_soa_identity_return() {
        // Returning the whole input parameter is a valid identity kernel — each
        // field copied through (GPU-LBM-4b's struct-value handling; previously an
        // unsupported non-struct-literal return).
        let func = parse_kernel("#[gpu]\nfn step(p: Particle) -> Particle { p }\n");
        let wgsl = emit_kernel_soa(&func, &particle_groups(), &[]).unwrap();
        assert!(wgsl.contains("gp_out[i] = p_pos;"), "{wgsl}");
        assert!(wgsl.contains("gv_out[i] = p_vel;"), "{wgsl}");
    }

    #[test]
    fn soa_rejects_missing_field_in_return() {
        // The returned struct omits `vel`.
        let func = parse_kernel(
            "#[gpu]\nfn step(p: Particle) -> Particle { Particle { pos: p.pos + p.vel } }\n",
        );
        let err = emit_kernel_soa(&func, &particle_groups(), &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    // ── GPU-LBM-4 control flow ───────────────────────────────────

    #[test]
    fn emits_scalar_if_as_select() {
        // `if x > 0 { x } else { 0 }` → `select(0.0, input[i], (input[i] > 0.0))`.
        let func =
            parse_kernel("#[gpu]\nfn relu(x: f32) -> f32 { if x > 0.0 { x } else { 0.0 } }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("output[i] = select(0.0, input[i], (input[i] > 0.0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn emits_all_comparison_operators() {
        for (src, op) in [
            (">", ">"),
            ("<", "<"),
            (">=", ">="),
            ("<=", "<="),
            ("==", "=="),
            ("!=", "!="),
        ] {
            let func = parse_kernel(&format!(
                "#[gpu]\nfn k(x: f32) -> f32 {{ if x {src} 1.0 {{ x }} else {{ 0.0 }} }}\n"
            ));
            let wgsl = emit_kernel(&func, &[]).unwrap();
            assert!(
                wgsl.contains(&format!("(input[i] {op} 1.0)")),
                "op {src}:\n{wgsl}"
            );
        }
    }

    #[test]
    fn emits_soa_field_level_if() {
        // A field expr with a value `if` over a field comparison.
        let func = parse_kernel(
            "#[gpu]\nfn upd(x: Cell) -> Cell { Cell { a: if x.c > 0.0 { x.a + x.c } else { x.a }, b: x.b, c: x.c } }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
            SoaGpuGroup {
                name: "gc".into(),
                fields: vec!["c".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(
            wgsl.contains("ga_out[i] = select(x_a, (x_a + x_c), (x_c > 0.0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn rejects_if_without_else() {
        // A value `if` needs an `else`.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { if x > 0.0 { x } }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    #[test]
    fn emits_soa_struct_valued_if_guard() {
        // The LBM `collide` guard shape: `if cond { S { .. } } else { n }` where the
        // else branch is the whole input struct → per-field `select` with a shared
        // condition (GPU-LBM-4b).
        let func = parse_kernel(
            "#[gpu]\nfn guard(x: Cell) -> Cell { if x.b > 0.0 { Cell { a: x.a + x.b, b: x.b } } else { x } }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).unwrap();
        assert!(
            wgsl.contains("ga_out[i] = select(x_a, (x_a + x_b), (x_b > 0.0));"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("gb_out[i] = select(x_b, x_b, (x_b > 0.0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn emits_stencil_neighbour_read() {
        // GPU-LBM-6: a stencil kernel's first parameter is the whole `Vec[S]`
        // buffer plus an index — the body reads a neighbour `g[i + 1].a`, bounded
        // by `g.len()`. No per-element materialize; the index maps to `i32(gid.x)`,
        // and neighbour reads index the input buffer directly.
        let func = parse_kernel(
            "#[gpu]\nfn shift_up(g: Vec[Cell], i: i64) -> Cell { Cell { a: if i < g.len() - 1 { g[i + 1].a } else { g[i].a } } }\n",
        );
        let groups = vec![SoaGpuGroup {
            name: "ga".into(),
            fields: vec!["a".into()],
        }];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).expect("stencil kernel should lower");
        // Bindings unchanged — the whole input is bound read-only.
        assert!(
            wgsl.contains("@group(0) @binding(0) var<storage, read> ga_in: array<f32>;"),
            "{wgsl}"
        );
        // The index parameter becomes the signed thread index; the guard/output
        // key off the unsigned `gi`.
        assert!(
            wgsl.contains("let gi = gid.y * 4194240u + gid.x;"),
            "{wgsl}"
        );
        assert!(wgsl.contains("let i = i32(gi);"), "{wgsl}");
        // No per-element materialize (a stencil indexes the buffer directly).
        assert!(!wgsl.contains("let g_a ="), "{wgsl}");
        // Neighbour read + bounds via `.len()` → `arrayLength`, stored at `gi`.
        assert!(
            wgsl.contains(
                "ga_out[gi] = select(ga_in[i], ga_in[(i + 1)], (i < (i32(arrayLength(&ga_in)) - 1)));"
            ),
            "{wgsl}"
        );
    }

    #[test]
    fn stencil_lowers_let_logical_sqrt_cast() {
        // GPU-SLIP-2a: a stencil body may bind `let` locals, use `or`, cast an
        // index to float, and call `.sqrt()` — the shapes the `stream` boundary
        // math needs.
        let func = parse_kernel(
            "#[gpu]\nfn k(g: Vec[Cell], i: i64, t: f32) -> Cell {\n\
             \x20   let w = 4;\n\
             \x20   let x = i % w;\n\
             \x20   let d = (x as f32) - t;\n\
             \x20   let m = (d * d).sqrt();\n\
             \x20   Cell { a: if x == 0 or x == w - 1 { m } else { g[i].a } }\n\
             }\n",
        );
        let groups = vec![SoaGpuGroup {
            name: "ga".into(),
            fields: vec!["a".into()],
        }];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).expect("stencil should lower");
        assert!(wgsl.contains("let w = 4;"), "{wgsl}");
        assert!(wgsl.contains("let x = (i % w);"), "{wgsl}");
        // `x as f32` → `f32(x)`; the uniform `t` → `t_u[0]`.
        assert!(wgsl.contains("let d = (f32(x) - t_u[0]);"), "{wgsl}");
        // `.sqrt()` → `sqrt(...)`.
        assert!(wgsl.contains("let m = sqrt((d * d));"), "{wgsl}");
        // `or` → `||`; the local `m` and neighbour `g[i].a` feed the select.
        assert!(
            wgsl.contains("ga_out[gi] = select(ga_in[i], m, ((x == 0) || (x == (w - 1))));"),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_lowers_logical_and_sqrt() {
        // GPU-SLIP-2a in the scalar element-wise path: `and` + `.sqrt()`.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 { if x > 0.0 and x < 1.0 { x.sqrt() } else { x } }\n",
        );
        let wgsl = emit_kernel(&func, &[]).expect("scalar kernel should lower");
        assert!(
            wgsl.contains(
                "select(input[i], sqrt(input[i]), ((input[i] > 0.0) && (input[i] < 1.0)))"
            ),
            "{wgsl}"
        );
    }

    #[test]
    fn rejects_unsupported_method_in_gpu_body() {
        // A non-intrinsic method on a scalar body is still rejected.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x.to_bits() }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    #[test]
    fn stencil_lowers_cell_alias_local() {
        // GPU-SLIP-2c: `let c = g[i]` binds the whole centre cell; `c.field`
        // reads that cell's field (no WGSL `let` — the cell spans two bindings).
        // This is the LBM `stream` centre-cell shape (`let c = coll[idx]; c.f0`).
        let func = parse_kernel(
            "#[gpu]\nfn k(g: Vec[Cell], i: i64) -> Cell {\n\
             \x20   let c = g[i];\n\
             \x20   Cell { a: c.a, b: if i == 0 { c.b } else { g[i - 1].b } }\n\
             }\n",
        );
        let groups = vec![
            SoaGpuGroup {
                name: "ga".into(),
                fields: vec!["a".into()],
            },
            SoaGpuGroup {
                name: "gb".into(),
                fields: vec!["b".into()],
            },
        ];
        let wgsl = emit_kernel_soa(&func, &groups, &[]).expect("cell-alias stencil should lower");
        // No `let c =` — the alias has no single WGSL value.
        assert!(!wgsl.contains("let c ="), "{wgsl}");
        // `c.a` → the centre cell's group-a binding at index `i`.
        assert!(wgsl.contains("ga_out[gi] = ga_in[i];"), "{wgsl}");
        // `c.b` (then) vs neighbour `g[i-1].b` (else), keyed by `gi`.
        assert!(
            wgsl.contains("gb_out[gi] = select(gb_in[(i - 1)], gb_in[i], (i == 0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn gathers_helper_in_index_and_let_rhs() {
        // B-2026-07-11-20: reachable_helpers must find helper calls inside an
        // `Index` and a `let` RHS (`let c = g[flat(i, 0)]`), not only in
        // struct-literal / `if`-condition positions — the `stream` kernel calls
        // its `idx` helper exactly there. Missing it made codegen reject the call.
        let fns = parse_fns(
            "#[gpu]\nfn flat(a: i64, b: i64) -> i64 { a + b }\n\
             #[gpu]\nfn k(g: Vec[Cell], i: i64) -> Cell { let c = g[flat(i, 0)]; Cell { a: c.a } }\n",
        );
        let flat = fns.iter().find(|f| f.name == "flat").unwrap();
        let k = fns.iter().find(|f| f.name == "k").unwrap();
        let groups = vec![SoaGpuGroup {
            name: "ga".into(),
            fields: vec!["a".into()],
        }];
        let wgsl = emit_kernel_soa(k, &groups, &[flat]).expect("helper in index should gather");
        assert!(
            wgsl.contains("fn flat(a: i32, b: i32) -> i32 { return (a + b); }"),
            "{wgsl}"
        );
        // The cell alias `c = g[flat(i, 0)]`, so `c.a` reads `ga_in[flat(i, 0)]`.
        assert!(wgsl.contains("ga_out[gi] = ga_in[flat(i, 0)];"), "{wgsl}");
    }

    #[test]
    fn stencil_rejects_whole_buffer_value() {
        // Reading the buffer as a value (not `buf[index].field`) is rejected —
        // a stencil body addresses individual neighbours, never the whole buffer.
        let func =
            parse_kernel("#[gpu]\nfn bad(g: Vec[Cell], i: i64) -> Cell { Cell { a: g.a } }\n");
        let groups = vec![SoaGpuGroup {
            name: "ga".into(),
            fields: vec!["a".into()],
        }];
        let err = emit_kernel_soa(&func, &groups, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
    }

    // ── Scalar-kernel `let` locals (B-2026-08-18-40) ────────────────

    #[test]
    fn scalar_kernel_lowers_let_locals_in_order() {
        // The headline case the single-expression floor used to reject: name an
        // intermediate instead of hand-inlining it. Each binding is visible to
        // the next and to the tail.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let doubled: f32 = x * 2.0;\n    \
             let shifted: f32 = doubled + 1.0;\n    shifted * shifted\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let doubled = (input[i] * 2.0);"), "{wgsl}");
        assert!(wgsl.contains("let shifted = (doubled + 1.0);"), "{wgsl}");
        assert!(wgsl.contains("output[i] = (shifted * shifted);"), "{wgsl}");
        // Declared before use, inside `main`.
        let decl = wgsl.find("let doubled").unwrap();
        let use_ = wgsl.find("(doubled + 1.0)").unwrap();
        assert!(decl < use_, "binding must precede its use:\n{wgsl}");
    }

    #[test]
    fn scalar_kernel_local_may_shadow_nothing_but_reads_the_param() {
        // A local reading the kernel parameter resolves the param to `input[i]`;
        // referring to the local afterwards resolves to the local's own name.
        let func = parse_kernel("#[gpu]\nfn k(v: f32) -> f32 {\n    let t: f32 = v + v;\n    t\n}");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let t = (input[i] + input[i]);"), "{wgsl}");
        assert!(wgsl.contains("output[i] = t;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_shadowing_local_gets_a_distinct_wgsl_name() {
        // Was `scalar_kernel_rejects_shadowing_local` in increment 1. WGSL has
        // no same-scope redeclaration, so the scope stack renames the inner
        // binding instead of rejecting: the second RHS still sees the FIRST
        // `a`, and the tail sees the innermost — Kara's shadowing semantics.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let a: f32 = x * 2.0;\n    let a: f32 = a + 1.0;\n    a\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let a = (input[i] * 2.0);"), "{wgsl}");
        assert!(wgsl.contains("let a_ = (a + 1.0);"), "{wgsl}");
        assert!(wgsl.contains("output[i] = a_;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_renames_a_reserved_local_rather_than_rejecting() {
        // Was `scalar_kernel_rejects_reserved_local_name` in increment 1: a
        // local colliding with the wrapper's own `i` is renamed, not turned
        // away — a loop counter called `i` is too idiomatic to reject.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 {\n    let i: f32 = x;\n    i\n}");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let i_k = input[i];"), "{wgsl}");
        assert!(wgsl.contains("output[i] = i_k;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_lowers_let_mut_to_a_wgsl_var() {
        // Was `scalar_kernel_rejects_let_mut_pointing_at_the_next_increment` in
        // increment 1; that increment is this one.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut a: f32 = x;\n    a = a + 1.0;\n    a\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var a = input[i];"), "{wgsl}");
        assert!(wgsl.contains("a = (a + 1.0);"), "{wgsl}");
    }

    // ── `while` loops + mutable locals (B-2026-08-18-40, increment 2) ──

    #[test]
    fn scalar_kernel_lowers_while_loop_with_accumulator() {
        // The reduction shape the single-expression floor made impossible.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             let mut n: i32 = 0;\n    while n < 4 {\n        acc = acc + x;\n        \
             n = n.wrapping_add(1);\n    }\n    acc * 2.0\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var acc = 0.0;"), "{wgsl}");
        assert!(wgsl.contains("var n = 0;"), "{wgsl}");
        assert!(wgsl.contains("while ((n < 4)) {"), "{wgsl}");
        assert!(wgsl.contains("acc = (acc + input[i]);"), "{wgsl}");
        assert!(wgsl.contains("output[i] = (acc * 2.0);"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_renames_a_local_that_collides_with_the_wrapper() {
        // A loop counter called `i` is idiomatic, but `i` is the generated
        // wrapper's THREAD index. The local is renamed so the parameter keeps
        // resolving to the thread's own element — a correctness property, not
        // just ergonomics: `input[i]` must not pick up the loop counter.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             let mut i: i32 = 0;\n    while i < 4 {\n        acc = acc + x;\n        \
             i = i.wrapping_add(1);\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var i_k = 0;"), "{wgsl}");
        assert!(wgsl.contains("while ((i_k < 4))"), "{wgsl}");
        assert!(wgsl.contains("i_k = (i_k + 1);"), "{wgsl}");
        // The thread index is untouched, and the param still reads through it.
        assert!(wgsl.contains("let i = gid.y"), "{wgsl}");
        assert!(wgsl.contains("acc = (acc + input[i]);"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_lowers_compound_assignment() {
        let func = parse_kernel(
            "#[gpu]\nfn s(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             let mut n: i32 = 0;\n    while n < 3 {\n        acc += x;\n        \
             n += 1;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("acc += input[i];"), "{wgsl}");
        assert!(wgsl.contains("n += 1;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_scopes_a_binding_to_the_loop_body() {
        // A `let` inside the loop is declared inside the WGSL block, so it is
        // re-bound each iteration and invisible afterwards.
        let func = parse_kernel(
            "#[gpu]\nfn s(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             let mut n: i32 = 0;\n    while n < 3 {\n        let step: f32 = x + 1.0;\n        \
             acc += step;\n        n += 1;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        let body = wgsl.split("while").nth(1).unwrap();
        assert!(body.contains("let step = (input[i] + 1.0);"), "{wgsl}");
        assert!(wgsl.contains("acc += step;"), "{wgsl}");
    }

    // ── `for` over a range (B-2026-08-18-40, increment 3) ──────────

    #[test]
    fn scalar_kernel_lowers_for_over_an_exclusive_range() {
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 0..4 {\n        acc = acc + x;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("for (var n = 0; n < 4; n = n + 1) {"),
            "{wgsl}"
        );
        assert!(wgsl.contains("acc = (acc + input[i]);"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_lowers_inclusive_range_with_le() {
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 1..=3 {\n        acc = acc + x;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("for (var n = 1; n <= 3; n = n + 1) {"),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_nests_for_loops_and_renames_the_wrapper_collision() {
        // The outer counter is `i` — the wrapper's thread index — so it renames;
        // the inner `k` does not. The param still reads the THREAD element.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for i in 0..4 {\n        for k in 1..=2 {\n            acc = acc + x;\n        }\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("for (var i_k = 0; i_k < 4; i_k = i_k + 1) {"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("for (var k = 1; k <= 2; k = k + 1) {"),
            "{wgsl}"
        );
        assert!(wgsl.contains("acc = (acc + input[i]);"), "{wgsl}");
    }

    // ── Statement-form `if` (B-2026-08-18-49) ───────────────────────────

    #[test]
    fn scalar_kernel_lowers_statement_if_to_a_real_wgsl_if() {
        // The conditional accumulator: the shape B-2026-08-18-49 was filed for,
        // and the reason "locals inside an `if` branch" understated the gap —
        // NEITHER branch here declares a local, and it was still rejected.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             let mut n: i32 = 0;\n    while n < 3 {\n        \
             if x > 2.0 {\n            acc = acc + x;\n        } else {\n            \
             acc = acc - 1.0;\n        }\n        n = n.wrapping_add(1);\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("if ((input[i] > 2.0)) {"), "{wgsl}");
        assert!(wgsl.contains("acc = (acc + input[i]);"), "{wgsl}");
        assert!(wgsl.contains("} else {"), "{wgsl}");
        assert!(wgsl.contains("acc = (acc - 1.0);"), "{wgsl}");
        // A statement `if` must NOT go through the value-`if` lowering.
        assert!(!wgsl.contains("select("), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_lowers_bare_statement_if_without_else() {
        // A value-`if` REQUIRES an `else` (it must produce something); a
        // statement one does not, which is the other reason these cannot share
        // a lowering.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = x;\n    \
             if x > 2.0 {\n        acc = acc * 10.0;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("if ((input[i] > 2.0)) {"), "{wgsl}");
        assert!(wgsl.contains("acc = (acc * 10.0);"), "{wgsl}");
        assert!(!wgsl.contains("else"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_flattens_an_else_if_chain() {
        // `else if` nests in the AST but must emit FLAT, or a long chain walks
        // one indent level right per arm and reads nothing like the source.
        let func = parse_kernel(
            "#[gpu]\nfn pick(x: i32) -> i32 {\n    let mut r: i32 = 0;\n    \
             if x == 0 {\n        r = 100;\n    } else if x == 1 {\n        \
             r = 200;\n    } else {\n        r = 300;\n    }\n    r\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("} else if ((input[i] == 1)) {"), "{wgsl}");
        // Flat: every arm's body sits at one indent inside `main`, so no arm is
        // pushed deeper than the first.
        assert!(wgsl.contains("\n        r = 100;\n"), "{wgsl}");
        assert!(wgsl.contains("\n        r = 200;\n"), "{wgsl}");
        assert!(wgsl.contains("\n        r = 300;\n"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_scopes_a_binding_to_its_if_branch() {
        // Each branch is its own scope, as a loop body is. The `then` arm's `t`
        // and the `else` arm's `t` are separate bindings, and neither escapes.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             if x > 2.0 {\n        let t: f32 = x * 2.0;\n        acc = t;\n    } else {\n        \
             let t: f32 = x + 1.0;\n        acc = t;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        // Both arms may use the plain name — the first is popped at arm exit.
        assert_eq!(wgsl.matches("let t = ").count(), 2, "{wgsl}");
        assert!(wgsl.contains("let t = (input[i] * 2.0);"), "{wgsl}");
        assert!(wgsl.contains("let t = (input[i] + 1.0);"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_rejects_a_statement_if_branch_that_produces_a_value() {
        // In statement position the value would go nowhere. The diagnostic
        // points at the shape that DOES produce one, which is a `select()`.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             if x > 2.0 {\n        acc = x;\n        x\n    } else {\n        acc = 0.0;\n    }\n    acc\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("must not produce a value"), "{msg}");
        assert!(msg.contains("bind it instead"), "{msg}");
    }

    // ── Value-`if` with locals, desugared onto the statement form (step 2) ──

    #[test]
    fn scalar_kernel_hoists_a_var_for_a_let_bound_if_with_locals() {
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    \
             let y: f32 = if x > 2.0 { let t: f32 = x * 2.0; t + 1.0 } else { x };\n    y\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var y: f32;"), "{wgsl}");
        assert!(wgsl.contains("if ((input[i] > 2.0)) {"), "{wgsl}");
        assert!(wgsl.contains("let t = (input[i] * 2.0);"), "{wgsl}");
        assert!(wgsl.contains("y = (t + 1.0);"), "{wgsl}");
        assert!(wgsl.contains("y = input[i];"), "{wgsl}");
        assert!(!wgsl.contains("select("), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_assign_form_if_with_locals_needs_no_annotation() {
        // The destination already exists, so nothing is hoisted and no type is
        // required — the annotation is a cost of the `let` form alone.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             acc = if x > 2.0 { let t: f32 = x * 3.0; t } else { let u: f32 = x + 1.0; u };\n    \
             acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var acc = 0.0;"), "{wgsl}");
        // No second declaration of `acc` — the existing one is assigned into.
        assert_eq!(wgsl.matches("var acc").count(), 1, "{wgsl}");
        assert!(wgsl.contains("acc = t;"), "{wgsl}");
        assert!(wgsl.contains("acc = u;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_keeps_select_for_a_value_if_without_locals() {
        // The fork that keeps the cheap path cheap: a branchless `select` is
        // what a GPU wants, so only the shapes `select` CANNOT express take the
        // heavier statement lowering. A regression here would be invisible in
        // output but real in divergence.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let y: f32 = if x > 2.0 { x * 2.0 } else { x };\n    y\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("select("), "{wgsl}");
        assert!(!wgsl.contains("var y: f32;"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_rejects_an_unannotated_let_bound_if_with_locals() {
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    \
             let y = if x > 2.0 { let t: f32 = x * 2.0; t } else { x };\n    y\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("needs a type annotation"), "{msg}");
        // Names the binding and shows the repair, rather than just refusing.
        assert!(msg.contains("let y: f32 = if"), "{msg}");
    }

    #[test]
    fn scalar_kernel_desugars_an_else_if_chain_with_locals() {
        let func = parse_kernel(
            "#[gpu]\nfn pick(x: f32) -> f32 {\n    \
             let y: f32 = if x > 3.0 { let a: f32 = x * 2.0; a } \
             else if x > 1.0 { let b: f32 = x + 5.0; b } else { x };\n    y\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("var y: f32;"), "{wgsl}");
        assert!(wgsl.contains("} else if ((input[i] > 1.0)) {"), "{wgsl}");
        assert!(wgsl.contains("y = a;"), "{wgsl}");
        assert!(wgsl.contains("y = b;"), "{wgsl}");
        assert!(wgsl.contains("y = input[i];"), "{wgsl}");
    }

    // ── GPU reductions (B-2026-08-19-10) ────────────────────────────────

    #[test]
    fn reduce_kernel_matches_the_hand_validated_shader() {
        // `runtime/src/gpu.rs`'s SUM_REDUCE_WGSL was written by hand and proven
        // on lavapipe BEFORE any of this existed. The generator must reproduce
        // it, because that constant is what the tree order was validated
        // against — if they drift, the runtime test is no longer testing the
        // shader the compiler emits.
        let wgsl = emit_reduce_kernel(ReduceOp::Sum, "f32").unwrap();
        for needle in [
            "var<workgroup> scratch: array<f32, 64>;",
            "@compute @workgroup_size(64)",
            "if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = 0.0; }",
            "workgroupBarrier();",
            "var stride: u32 = 32u;",
            "if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }",
            "stride = stride / 2u;",
            "if (t == 0u) { output[wg] = scratch[0]; }",
        ] {
            assert!(wgsl.contains(needle), "missing `{needle}` in:\n{wgsl}");
        }
    }

    /// The strided field kernel reads the requested field, and bounds itself
    /// in RECORDS rather than f32s (GPU-SLIP-4b-3).
    ///
    /// Two things can only go wrong here, and they are the two asserted.
    /// `arrayLength(&input)` counts f32s, so the element bound must divide by
    /// the stride — forgetting that reduces `stride`× too many slots and runs
    /// off the group; folding the stride in twice silently reduces a prefix.
    /// And the load must be `i * stride + offset`, since offset alone would
    /// read record 0's neighbour every time.
    ///
    /// The rejection cases matter because both are reachable from a codegen
    /// bug rather than from user input: a zero stride would divide by zero in
    /// the shader, and an offset outside the stride would read the NEXT
    /// record's field — a wrong answer rather than a crash.
    #[test]
    fn reduce_field_kernel_indexes_by_stride_and_offset() {
        let w = emit_reduce_field_kernel(ReduceOp::Sum, "f32", 4, 3).unwrap();
        assert!(
            w.contains("let n = arrayLength(&input) / 4u;"),
            "the element bound must be in RECORDS (arrayLength / stride); got:\n{w}"
        );
        assert!(
            w.contains("scratch[t] = input[i * 4u + 3u];"),
            "the load must be `i * stride + offset`; got:\n{w}"
        );

        // A single-field group degenerates to a contiguous read, which is the
        // default layout and therefore the common case.
        let flat = emit_reduce_field_kernel(ReduceOp::Sum, "f32", 1, 0).unwrap();
        assert!(
            flat.contains("let n = arrayLength(&input) / 1u;")
                && flat.contains("scratch[t] = input[i * 1u + 0u];"),
            "stride 1 / offset 0 must still be well-formed; got:\n{flat}"
        );

        assert!(
            emit_reduce_field_kernel(ReduceOp::Sum, "f32", 0, 0).is_err(),
            "a zero stride would divide by zero in the shader"
        );
        assert!(
            emit_reduce_field_kernel(ReduceOp::Sum, "f32", 2, 2).is_err(),
            "an offset outside the stride would read the next record's field"
        );
    }

    /// Every reduce/scan shader must recover the FLAT thread index and the
    /// FLAT workgroup number (B-2026-08-21-13).
    ///
    /// `run_compute` caps a dispatch's X extent at 65535 workgroups and
    /// spreads anything longer across grid ROWS. So past
    /// `65535 * 64 = 4_194_240` elements, a shader reading `gid.x` sees only
    /// row 0, and one writing `output[wid.x]` has every row overwrite row
    /// 0's partials. All twelve emitters here did both: `gpu.sum` of
    /// 5_000_000 ones answered `4_194_240` (exactly one row), and `gpu.max`
    /// answered 1 with the true maximum planted in the last element.
    ///
    /// The three MAP kernels always had this right, which is exactly why it
    /// hid — the convention is documented on [`DISPATCH_X_SPAN`] and was
    /// honoured everywhere except the family added last. A structural test
    /// beats another end-to-end case here: the failure only appears above
    /// four million elements, so no ordinary fixture reaches it.
    ///
    /// **A new reduce/scan emitter must be added to this list** — the
    /// distinct-emitter count below is what makes that a failure rather
    /// than an omission.
    #[test]
    fn every_reduction_shader_indexes_the_full_2d_dispatch_grid() {
        let mut shaders: Vec<(&str, String)> = Vec::new();
        for op in [ReduceOp::Sum, ReduceOp::Prod, ReduceOp::Min, ReduceOp::Max] {
            if let Ok(w) = emit_reduce_kernel(op, "f32") {
                shaders.push(("reduce", w));
            }
            if let Ok(w) = emit_int_reduce_kernel(op, "i32") {
                shaders.push(("int_reduce", w));
            }
        }
        for op in [ReduceOp::Argmin, ReduceOp::Argmax] {
            for fold in [false, true] {
                if let Ok(w) = emit_arg_kernel(op, "f32", fold) {
                    shaders.push(("arg", w));
                }
            }
        }
        for (name, made) in [
            ("scan", emit_scan_kernel("f32")),
            ("scan_offset", emit_scan_offset_kernel("f32")),
            ("int_scan", emit_int_scan_kernel("i32")),
            ("int_scan_offset", emit_int_scan_offset_kernel("i32")),
            ("deviation", emit_deviation_kernel("f32")),
            ("int_deviation", emit_int_deviation_kernel("i32")),
            ("dot", emit_dot_kernel("f32")),
            ("int_dot", emit_int_dot_kernel("i32")),
        ] {
            shaders.push((name, made.unwrap()));
        }
        shaders.push(("wide_fold", emit_wide_fold_kernel()));
        // The STRIDED level-0 kernel for a resident field reduction
        // (GPU-SLIP-4b-3). It recovers its own index, so it can reintroduce
        // this defect independently of the twelve above — and it is the one
        // emitter whose bound is `arrayLength / stride` rather than the array
        // length, so both a contiguous group (stride 1) and an interleaved one
        // are covered here.
        for (stride, offset) in [(1u32, 0u32), (4, 3)] {
            if let Ok(w) = emit_reduce_field_kernel(ReduceOp::Sum, "f32", stride, offset) {
                shaders.push(("reduce_field", w));
            }
        }

        let distinct: std::collections::BTreeSet<&str> = shaders.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            distinct.len(),
            13,
            "every reduce/scan emitter must be covered; got {distinct:?}"
        );

        let flat_thread = format!("let i = gid.y * {DISPATCH_X_SPAN}u + gid.x;");
        let flat_group = format!("let wg = wid.y * {DISPATCH_X_WORKGROUPS}u + wid.x;");
        for (name, wgsl) in &shaders {
            assert!(
                wgsl.contains(&flat_thread),
                "{name}: reads only row 0 of the dispatch grid\n{wgsl}"
            );
            // Only the shaders that actually write a per-workgroup output
            // bind a workgroup id — `scan_offset` writes `output[i]` per
            // THREAD and needs none, so demanding `wg` there would emit a
            // reference to an unbound `wid` and fail shader validation.
            if wgsl.contains("@builtin(workgroup_id)") {
                assert!(
                    wgsl.contains(&flat_group),
                    "{name}: binds a workgroup id but never forms the flat \
                     workgroup number\n{wgsl}"
                );
            }
            // The load-bearing half: NOTHING may index an output by `wid.x`,
            // which repeats on every row of the grid. The binding above is
            // the single legal mention, so strike it and look for the rest.
            let stray = wgsl.replace(&flat_group, "");
            assert!(
                !stray.contains("wid.x"),
                "{name}: indexes by `wid.x`, which collides across grid rows\n{wgsl}"
            );
        }
    }

    #[test]
    fn scan_kernel_matches_the_hand_validated_shader() {
        // Same contract as `reduce_kernel_matches_the_hand_validated_shader`:
        // `runtime/src/gpu.rs`'s SCAN_WGSL / SCAN_OFFSET_WGSL are what the
        // prefix sum was proven against on lavapipe, so the generator must
        // reproduce them or the runtime tests stop testing the emitted shader.
        let scan = emit_scan_kernel("f32").unwrap();
        for needle in [
            "var<workgroup> scratch: array<f32, 64>;",
            "@compute @workgroup_size(64)",
            "if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = 0.0; }",
            "var stride: u32 = 1u;",
            "if (stride >= 64u) { break; }",
            // The read-barrier-write-barrier trio IS the algorithm — a single
            // barrier per step double-counts while still looking plausible.
            "if (t >= stride) { addend = scratch[t - stride]; }",
            "if (t >= stride) { scratch[t] = scratch[t] + addend; }",
            "stride = stride * 2u;",
            "if (i < arrayLength(&input)) { output[i] = scratch[t]; }",
            "if (t == 63u) { totals[wg] = scratch[63]; }",
        ] {
            assert!(scan.contains(needle), "missing `{needle}` in:\n{scan}");
        }
        // Two barriers per step, not one.
        assert_eq!(
            scan.matches("workgroupBarrier();").count(),
            3,
            "one load barrier plus the read/write pair inside the loop:\n{scan}"
        );

        let off = emit_scan_offset_kernel("f32").unwrap();
        for needle in [
            "let c = i / 64u;",
            // The exclusive prefix is the inclusive one read a position back.
            "if (c > 0u) { off = offsets[c - 1u]; }",
            "output[i] = scanned[i] + off;",
        ] {
            assert!(off.contains(needle), "missing `{needle}` in:\n{off}");
        }
    }

    #[test]
    fn scan_kernels_refuse_integer_elements() {
        // An integer prefix sum has to carry the overflow flag of
        // `emit_int_reduce_kernel` through EVERY lane, not just lane 0 —
        // a different shader, and one nothing calls yet. Refusing beats
        // emitting a silently-wrapping scan, which is the hazard the float
        // reduce emitter already had to close.
        for elem in ["i32", "u32"] {
            assert!(emit_scan_kernel(elem).is_err(), "{elem} scan");
            assert!(emit_scan_offset_kernel(elem).is_err(), "{elem} offset");
        }
    }

    #[test]
    fn reduce_kernel_uses_the_right_identity_per_op_and_type() {
        // The identity is not decoration: a `prod` that padded with 0.0 would
        // return 0 for every buffer shorter than the workgroup.
        let sum_f = emit_reduce_kernel(ReduceOp::Sum, "f32").unwrap();
        assert!(sum_f.contains("scratch[t] = 0.0;"), "{sum_f}");
        assert!(
            sum_f.contains("scratch[t] + scratch[t + stride]"),
            "{sum_f}"
        );

        let prod_f = emit_reduce_kernel(ReduceOp::Prod, "f32").unwrap();
        assert!(prod_f.contains("scratch[t] = 1.0;"), "{prod_f}");
        assert!(
            prod_f.contains("scratch[t] * scratch[t + stride]"),
            "{prod_f}"
        );

        // Integers come from the CHECKED emitter — this one refuses them.
        let sum_i = emit_int_reduce_kernel(ReduceOp::Sum, "i32").unwrap();
        assert!(sum_i.contains("array<i32, 64>"), "{sum_i}");
        assert!(sum_i.contains("scratch[t] = 0;"), "{sum_i}");
        let sum_u = emit_int_reduce_kernel(ReduceOp::Sum, "u32").unwrap();
        assert!(sum_u.contains("scratch[t] = 0u;"), "{sum_u}");
    }

    #[test]
    fn reduce_kernel_refuses_the_ops_that_need_more_than_one_pass() {
        // Mean needs a count division, Var/Std need two passes, the Arg family
        // needs an index carried alongside the value through every halving step
        // — a scratch of PAIRS, which is a different shader rather than a
        // different combine string. Refusing beats emitting a shader that
        // silently computes a different statistic.
        for op in [
            ReduceOp::Mean,
            ReduceOp::Var { bessel: false },
            ReduceOp::Std { bessel: true },
            ReduceOp::Argmax,
            ReduceOp::Argmin,
            ReduceOp::Median,
        ] {
            let err = emit_reduce_kernel(op, "f32").unwrap_err();
            let msg = format!("{err:?}");
            assert!(msg.contains("not supported yet"), "{msg}");
        }
    }

    #[test]
    fn arg_kernel_carries_indices_and_marks_padding_by_index() {
        let seed = emit_arg_kernel(ReduceOp::Argmin, "f32", false).unwrap();
        // The scratch holds INDICES, not values — an index alone cannot be
        // compared, so the combine looks each one's value up in `input`.
        assert!(
            seed.contains("var<workgroup> idxs: array<u32, 64>;"),
            "{seed}"
        );
        assert!(
            !seed.contains("var<workgroup> vals"),
            "no value scratch:\n{seed}"
        );
        assert!(seed.contains("let a = input[ia];"), "{seed}");

        // Padding is marked by the INDEX sentinel, never by a value. A value
        // sentinel would have to be NaN to lose reliably, and NaN preservation
        // is an OPTIONAL Vulkan feature — a device that flushed it would let
        // padding win and report a nonexistent index.
        assert!(seed.contains("idxs[t] = 4294967295u;"), "{seed}");
        assert!(
            seed.contains("if (ib == 4294967295u) { return false; }"),
            "{seed}"
        );
        assert!(
            seed.contains("if (ia == 4294967295u) { return true; }"),
            "{seed}"
        );

        // The combine: strictly better wins, exact tie goes to the SMALLER
        // index, NaN loses to anything real.
        assert!(
            seed.contains("if (karac_is_nan(a)) { return !karac_is_nan(b); }"),
            "{seed}"
        );
        assert!(
            seed.contains("if (karac_is_nan(b)) { return false; }"),
            "{seed}"
        );
        // The predicate is a BIT test, not `x != x` — see B-2026-08-20-2.
        assert!(
            seed.contains("return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u;"),
            "{seed}"
        );
        assert!(
            !seed.contains("a == a"),
            "no fast-math-foldable NaN test:\n{seed}"
        );
        assert!(
            seed.contains("return (b < a) || (b == a && ib < ia);"),
            "{seed}"
        );
        let seed_max = emit_arg_kernel(ReduceOp::Argmax, "f32", false).unwrap();
        assert!(
            seed_max.contains("return (b > a) || (b == a && ib < ia);"),
            "{seed_max}"
        );
    }

    #[test]
    fn arg_fold_kernel_rereads_values_from_the_original_buffer() {
        // The level-1+ kernel takes the surviving candidate INDICES and looks
        // their values up in the same `input` buffer, so indices stay absolute
        // and no value ever crosses a dispatch boundary.
        let seed = emit_arg_kernel(ReduceOp::Argmin, "f32", false).unwrap();
        let fold = emit_arg_kernel(ReduceOp::Argmin, "f32", true).unwrap();

        assert!(
            !seed.contains("var<storage, read>       cand:"),
            "level 0 has no candidate BUFFER (its prose mentions candidates):\n{seed}"
        );
        assert!(
            seed.contains("if (i < arrayLength(&input)) { idxs[t] = i; }"),
            "{seed}"
        );

        assert!(fold.contains("@binding(1) var<storage, read>       cand:   array<u32>;"));
        assert!(fold.contains("@binding(2) var<storage, read_write> output: array<u32>;"));
        assert!(
            fold.contains("if (i < arrayLength(&cand)) { idxs[t] = cand[i]; }"),
            "{fold}"
        );
        // Both still read values from `input`, which is what keeps the two
        // levels comparing the same numbers.
        assert!(fold.contains("let a = input[ia];"), "{fold}");

        // Everything after the seed is shared, verbatim.
        for shared in [
            "var<workgroup> idxs: array<u32, 64>;",
            "if (takes_b(idxs[t], idxs[t + stride])) { idxs[t] = idxs[t + stride]; }",
            "if (t == 0u) { output[wg] = idxs[0]; }",
        ] {
            assert!(seed.contains(shared), "seed missing `{shared}`");
            assert!(fold.contains(shared), "fold missing `{shared}`");
        }
    }

    #[test]
    fn deviation_kernel_is_the_sum_kernel_with_the_squared_deviation_on_load() {
        // Same relationship the dot kernel has to the sum kernel: everything
        // after the load is shared, so the fold levels can be the ordinary sum
        // shader and the answer is identical to summing the squared deviations
        // by hand.
        let dev = emit_deviation_kernel("f32").unwrap();
        let sum = emit_reduce_kernel(ReduceOp::Sum, "f32").unwrap();

        // THE distinguishing feature: a uniform. Variance is genuinely two
        // passes — the mean cannot be known until a whole reduction has
        // finished — so it arrives bound after the in/out buffers.
        assert!(
            dev.contains("@binding(2) var<storage, read>       mean_u: array<f32>;"),
            "{dev}"
        );
        assert!(!sum.contains("mean_u"), "the sum shader takes no uniform");
        assert!(dev.contains("let d = input[i] - mean_u[0];"), "{dev}");
        assert!(dev.contains("scratch[t] = d * d;"), "{dev}");
        // Out-of-range lanes contribute nothing to a sum of squares.
        assert!(dev.contains("scratch[t] = 0.0;"), "{dev}");

        for shared in [
            "var<workgroup> scratch: array<f32, 64>;",
            "@compute @workgroup_size(64)",
            "var stride: u32 = 32u;",
            "if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }",
            "if (t == 0u) { output[wg] = scratch[0]; }",
        ] {
            assert!(dev.contains(shared), "deviation missing `{shared}`:\n{dev}");
            assert!(sum.contains(shared), "sum missing `{shared}`");
        }
    }

    #[test]
    fn deviation_kernel_refuses_non_f32() {
        // An integer form would have to promote its deviations — the mean is
        // fractional — and doing that on a device means f32, which loses whole
        // integers above 2^24. Its own decision, not a widening.
        for elem in ["i32", "u32", "f64"] {
            let err = emit_deviation_kernel(elem).unwrap_err();
            assert!(format!("{err:?}").contains("f32-only"), "{elem}: {err:?}");
        }
    }

    #[test]
    fn arg_kernel_integer_drops_the_nan_guards_and_nothing_else() {
        let f = emit_arg_kernel(ReduceOp::Argmin, "f32", false).unwrap();
        let i = emit_arg_kernel(ReduceOp::Argmin, "i32", false).unwrap();
        let u = emit_arg_kernel(ReduceOp::Argmax, "u32", false).unwrap();

        // Integers are totally ordered, so there is nothing for the NaN
        // guards to catch — and a shader that carried them would be describing
        // a case that cannot arise.
        assert!(
            f.contains("if (karac_is_nan(a)) { return !karac_is_nan(b); }"),
            "{f}"
        );
        for w in [&i, &u] {
            assert!(
                !w.contains("karac_is_nan"),
                "integer shader has no NaN guard:\n{w}"
            );
        }
        // The emitted PROSE must match the emitted code.
        assert!(!i.contains("NaN loses"), "{i}");
        assert!(i.contains("there is no NaN rule"), "{i}");

        // Signedness needs no separate spelling: WGSL's `<` is signed on an
        // `array<i32>` and unsigned on an `array<u32>`, so declaring the
        // element type is the whole mechanism.
        assert!(i.contains("input:  array<i32>;"), "{i}");
        assert!(u.contains("input:  array<u32>;"), "{u}");
        assert!(i.contains("return (b < a) || (b == a && ib < ia);"), "{i}");
        assert!(u.contains("return (b > a) || (b == a && ib < ia);"), "{u}");

        // Everything else is shared — including the INDEX output, which is
        // u32 whatever the buffer holds.
        for w in [&f, &i, &u] {
            assert!(w.contains("output: array<u32>;"), "{w}");
            assert!(w.contains("var<workgroup> idxs: array<u32, 64>;"), "{w}");
            assert!(
                w.contains("if (ib == 4294967295u) { return false; }"),
                "{w}"
            );
        }
    }

    #[test]
    fn arg_kernel_refuses_non_f32_and_non_arg_ops() {
        // Used to reject i32/u32 as well; they are supported now. What is
        // still refused is anything that is not a 32-bit element.
        for elem in ["f64", "i64", "u8"] {
            let err = emit_arg_kernel(ReduceOp::Argmin, elem, false).unwrap_err();
            assert!(
                format!("{err:?}").contains("f32, i32 and u32"),
                "{elem}: {err:?}"
            );
        }
        // The value reductions have their own emitters; routing one here would
        // produce an index where a value was expected.
        for op in [ReduceOp::Sum, ReduceOp::Min, ReduceOp::Mean] {
            let err = emit_arg_kernel(op, "f32", false).unwrap_err();
            assert!(
                format!("{err:?}").contains("not an arg reduction"),
                "{op:?}"
            );
        }
    }

    #[test]
    fn dot_kernel_is_the_sum_kernel_with_the_product_formed_on_load() {
        // The design claim, checked against the two shaders rather than
        // asserted in a comment: everything after the load is byte-identical
        // to the sum kernel, which is why `gpu.dot(a, b)` and
        // `gpu.sum(a * b)` agree bit-for-bit.
        let dot = emit_dot_kernel("f32").unwrap();
        let sum = emit_reduce_kernel(ReduceOp::Sum, "f32").unwrap();

        // Two inputs and ONE output — the binding shape a reduction needs and
        // an element-wise map does not. The runtime binds inputs first, then
        // outputs, so the partials land at `@binding(2)`.
        assert!(dot.contains("@binding(0) var<storage, read>       a:      array<f32>;"));
        assert!(dot.contains("@binding(1) var<storage, read>       b:      array<f32>;"));
        assert!(dot.contains("@binding(2) var<storage, read_write> output: array<f32>;"));

        // The one substantive difference: the product is formed on load, so no
        // n-element intermediate is ever written.
        assert!(dot.contains(
            "if (i < arrayLength(&a)) { scratch[t] = a[i] * b[i]; } else { scratch[t] = 0.0; }"
        ));
        assert!(sum.contains(
            "if (i < arrayLength(&input)) { scratch[t] = input[i]; } else { scratch[t] = 0.0; }"
        ));

        // Everything after the load is shared, verbatim.
        for shared in [
            "var<workgroup> scratch: array<f32, 64>;",
            "@compute @workgroup_size(64)",
            "var stride: u32 = 32u;",
            "if (t < stride) { scratch[t] = scratch[t] + scratch[t + stride]; }",
            "workgroupBarrier();",
            "stride = stride / 2u;",
            "if (t == 0u) { output[wg] = scratch[0]; }",
        ] {
            assert!(dot.contains(shared), "dot missing `{shared}`:\n{dot}");
            assert!(sum.contains(shared), "sum missing `{shared}`:\n{sum}");
        }
    }

    #[test]
    fn dot_kernel_refuses_non_f32_elements() {
        // The runtime entry point is f32-only, and an integer routed through
        // it would lose precision above 2^24 — the same reason the reduction
        // typechecker is narrower than the reduction emitter.
        for elem in ["i32", "u32", "f16"] {
            let err = emit_dot_kernel(elem).unwrap_err();
            assert!(format!("{err:?}").contains("f32-only"), "{elem}");
        }
    }

    #[test]
    fn reduce_kernel_min_max_ignore_nan_rather_than_calling_the_builtin() {
        // WGSL's `min(e1, e2)` is "e2 if e2 < e1, else e1", and every
        // comparison against NaN is false — so the builtin's answer depends on
        // which side the NaN is on. Harmless in a left fold, fatal in a tree,
        // where the halving decides that position. The emitted helper ignores
        // NaN from EITHER side, which makes the op associative and therefore
        // grouping-independent, matching `reduce_kernel`'s twin.
        let min = emit_reduce_kernel(ReduceOp::Min, "f32").unwrap();
        assert!(
            min.contains("fn karac_min(a: f32, b: f32) -> f32"),
            "float min must emit its own NaN-ignoring helper:\n{min}"
        );
        assert!(min.contains("if (karac_is_nan(a)) { return b; }"), "{min}");
        assert!(min.contains("if (karac_is_nan(b)) { return a; }"), "{min}");
        assert!(min.contains("scratch[t] = karac_min(scratch[t], scratch[t + stride]);"));
        // The identity is unreachable, not merely large: a real `f32::MAX`
        // element in a padded chunk would BEAT a finite stand-in.
        assert!(
            min.contains("scratch[t] = bitcast<f32>(0x7f800000u);"),
            "min pads with +inf:\n{min}"
        );

        let max = emit_reduce_kernel(ReduceOp::Max, "f32").unwrap();
        assert!(max.contains("return select(a, b, b > a);"), "{max}");
        assert!(
            max.contains("scratch[t] = bitcast<f32>(0xff800000u);"),
            "max pads with -inf:\n{max}"
        );

        // Integers have no NaN, so they take the builtin and a finite
        // identity — but they come from the CHECKED emitter, because this one
        // would emit a tree that wraps where Kāra traps.
        let min_i = emit_int_reduce_kernel(ReduceOp::Min, "i32").unwrap();
        assert!(!min_i.contains("fn karac_min"), "{min_i}");
        assert!(min_i.contains("scratch[t] = min(scratch[t], scratch[t + stride]);"));
        assert!(min_i.contains("scratch[t] = 2147483647;"), "{min_i}");
        let max_i = emit_int_reduce_kernel(ReduceOp::Max, "i32").unwrap();
        // `-2147483648` is not writable as an i32 literal — unary minus applies
        // to a value already out of range.
        assert!(min_i.contains("2147483647") && max_i.contains("-2147483647 - 1"));
        let max_u = emit_int_reduce_kernel(ReduceOp::Max, "u32").unwrap();
        assert!(max_u.contains("scratch[t] = 0u;"), "{max_u}");
    }

    #[test]
    fn float_emitter_refuses_integer_elements_outright() {
        // The unchecked tree is CORRECT for floats (they saturate) and WRONG
        // for integers (Kāra traps where WGSL wraps). Refusing here rather
        // than merely documenting it is what keeps the wrong shader
        // unreachable — this emitter used to accept i32/u32 and would have
        // produced a silently-wrapping reduction the moment anything wired it
        // up.
        for elem in ["i32", "u32"] {
            for op in [ReduceOp::Sum, ReduceOp::Min, ReduceOp::Max] {
                let err = emit_reduce_kernel(op, elem).unwrap_err();
                let msg = format!("{err:?}");
                assert!(msg.contains("CHECKED emitter"), "{elem} {op:?}: {msg}");
            }
        }
    }

    #[test]
    fn checked_int_emitter_carries_an_overflow_flag_only_where_it_can_overflow() {
        // `sum` can overflow, so it declares the `ovf` scratch, folds the bit
        // through the same halving tree as the value, and writes it out.
        let sum_i = emit_int_reduce_kernel(ReduceOp::Sum, "i32").unwrap();
        assert!(
            sum_i.contains("var<workgroup> ovf: array<u32, 64>;"),
            "{sum_i}"
        );
        assert!(
            sum_i.contains(
                "ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, ((a ^ s) & (b ^ s)) < 0);"
            ),
            "signed add overflow test:\n{sum_i}"
        );
        assert!(sum_i.contains("flags[wg] = ovf[0];"), "{sum_i}");
        // Unsigned overflow is a carry, not a sign flip.
        let sum_u = emit_int_reduce_kernel(ReduceOp::Sum, "u32").unwrap();
        assert!(
            sum_u.contains("ovf[t] = ovf[t] | ovf[t + stride] | select(0u, 1u, s < a);"),
            "unsigned carry test:\n{sum_u}"
        );

        // `min`/`max` cannot overflow, so they carry NO per-step bookkeeping —
        // but they still declare the `flags` binding and write a zero, so the
        // host has one dispatch shape for every integer reduction.
        for op in [ReduceOp::Min, ReduceOp::Max] {
            let w = emit_int_reduce_kernel(op, "i32").unwrap();
            assert!(
                !w.contains("var<workgroup> ovf"),
                "{op:?} needs no ovf:\n{w}"
            );
            assert!(w.contains("@binding(2) var<storage, read_write> flags:  array<u32>;"));
            assert!(w.contains("flags[wg] = 0u;"), "{op:?}:\n{w}");
        }
    }

    #[test]
    fn checked_int_emitter_supports_prod_and_only_32_bit_elements() {
        // `prod` was refused for several revisions on the grounds that WGSL
        // has no widening multiply. It has no widening-multiply INTRINSIC; the
        // capability is four 16-bit partial products and two carries, which
        // `karac_mul_wide` performs exactly (B-2026-08-19-13). So `prod` emits
        // a CHECKED multiply and carries the same overflow flag `sum` does.
        //
        // The substitute this emitter used to reject — `s / a != b` — really
        // is unsound in WGSL, where `i32::MIN / -1` is indeterminate, so it
        // would misfire on exactly the input it exists to catch. The wide
        // product avoids the division entirely.
        for elem in ["i32", "u32"] {
            let w = emit_int_reduce_kernel(ReduceOp::Prod, elem).unwrap();
            assert!(
                w.contains("karac_mul_wide"),
                "{elem} prod must use the exact widening product:\n{w}"
            );
            assert!(
                w.contains("var<workgroup> ovf"),
                "{elem} prod must carry an overflow flag:\n{w}"
            );
        }
        // The element set is still the two 32-bit integer types: WGSL has no
        // 64-bit ELEMENT type, and the emulated 64-bit arithmetic above
        // operates on accumulators, not on elements.
        for elem in ["f32", "i64", "u8"] {
            let err = emit_int_reduce_kernel(ReduceOp::Sum, elem).unwrap_err();
            assert!(
                format!("{err:?}").contains("i32 and u32"),
                "{elem}: {err:?}"
            );
        }
    }

    // ── Trapping integer arithmetic (B-2026-08-19-1) ────────────────────

    #[test]
    fn scalar_kernel_rejects_bare_integer_arithmetic() {
        // The bug: this compiled, and silently WRAPPED on the device while
        // trapping on CPU. Each operator names its own wrapping spelling.
        for (body, op, spelled) in [
            ("x + 1", "+", "wrapping_add"),
            ("x - 1", "-", "wrapping_sub"),
            ("x * 2", "*", "wrapping_mul"),
        ] {
            let func = parse_kernel(&format!("#[gpu]\nfn k(x: i32) -> i32 {{ {body} }}"));
            let err = emit_kernel(&func, &[]).unwrap_err();
            let msg = format!("{err:?}");
            assert!(msg.contains(&format!("integer `{op}`")), "{msg}");
            assert!(msg.contains(spelled), "{msg}");
        }
    }

    #[test]
    fn scalar_kernel_rejects_integer_division_without_offering_a_wrapping_form() {
        // Division is NOT the same case: there is no `wrapping_div` that means
        // what Kāra's `/` means, because the divergence is div-by-zero rather
        // than overflow. The diagnostic must not invent a spelling.
        let func = parse_kernel("#[gpu]\nfn k(x: i32) -> i32 { 100 / x }");
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("division by zero"), "{msg}");
        assert!(!msg.contains("wrapping_div"), "{msg}");
    }

    #[test]
    fn scalar_kernel_accepts_wrapping_and_emits_the_same_infix() {
        // The escape hatch lowers to EXACTLY the operator the bare form would
        // have emitted — which is the proof that naming the intent costs
        // nothing on the device, and that WGSL's `+` was always the wrapping
        // one.
        let bare = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x + 1.0 }");
        let bare_wgsl = emit_kernel(&bare, &[]).unwrap();
        assert!(
            bare_wgsl.contains("output[i] = (input[i] + 1.0);"),
            "{bare_wgsl}"
        );

        let wrapped = parse_kernel("#[gpu]\nfn k(x: i32) -> i32 { x.wrapping_add(1) }");
        let wrapped_wgsl = emit_kernel(&wrapped, &[]).unwrap();
        assert!(
            wrapped_wgsl.contains("output[i] = (input[i] + 1);"),
            "{wrapped_wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_leaves_float_arithmetic_alone() {
        // f32 is every shipped GPU example, and IEEE ops do not trap — so the
        // restriction must not touch them.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let y: f32 = x * 2.0 + 1.0;\n    y / 3.0\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("let y = ((input[i] * 2.0) + 1.0);"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_for_range_counter_needs_no_wrapping_spelling() {
        // The increment of a `for` counter is EMITTER-generated, not user
        // arithmetic, so the idiomatic counted loop is unaffected. Only a
        // `while` loop's hand-written `n = n + 1` has to name the intent —
        // that asymmetry is the whole ergonomic cost of this rule, and it is
        // worth pinning so a future change notices if `for` starts paying it.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    \
             for n in 0..4 { acc = acc + x; }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("for (var n = 0; n < 4; n = n + 1)"), "{wgsl}");
    }

    #[test]
    fn scalar_kernel_body_rejection_message_lists_every_supported_form() {
        // The `_ =>` fallthrough message was written at increment 2 and went
        // stale: it never mentioned `for` (supported since increment 3) and
        // never mentioned `if`. A message that under-reports what works sends
        // people looking for workarounds they do not need.
        let func = parse_kernel("#[gpu]\nfn poly(x: f32) -> f32 {\n    loop {\n    }\n    x\n}");
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = format!("{err:?}");
        for form in ["`let`", "assignments", "`if`", "`while`", "`for`"] {
            assert!(msg.contains(form), "message omits {form}: {msg}");
        }
    }

    #[test]
    fn scalar_kernel_scopes_the_loop_variable_to_its_loop() {
        // Two sequential loops may reuse a name: the first binding is popped at
        // the end of its loop, so the second gets the same WGSL name rather than
        // an ever-growing suffix.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 0..2 {\n        acc = acc + x;\n    }\n    for n in 0..3 {\n        acc = acc + x;\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("for (var n = 0; n < 2; n = n + 1) {"),
            "{wgsl}"
        );
        assert!(
            wgsl.contains("for (var n = 0; n < 3; n = n + 1) {"),
            "{wgsl}"
        );
        // No renamed sibling was minted (`var n_ = …`). Matching on the
        // declaration, not a bare `n_`, because `global_invocation_id` in the
        // entry-point signature contains that substring.
        assert!(
            !wgsl.contains("var n_"),
            "the loop name must not accumulate suffixes:\n{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_rejects_assignment_to_the_loop_variable() {
        // Kāra's loop variable is immutable, even though WGSL needs a `var` to
        // carry the increment.
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in 0..4 {\n        n = n.wrapping_add(1);\n    }\n    acc\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("immutable local"), "{msg}");
    }

    #[test]
    fn scalar_kernel_rejects_for_over_a_non_range() {
        let func = parse_kernel(
            "#[gpu]\nfn poly(x: f32) -> f32 {\n    let mut acc: f32 = 0.0;\n    for n in x {\n        acc = acc + x;\n    }\n    acc\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must iterate a range"), "{msg}");
    }

    // ── value `match` (B-2026-08-18-40, increment 4) ───────────────

    #[test]
    fn scalar_kernel_lowers_match_to_a_select_chain() {
        let func = parse_kernel(
            "#[gpu]\nfn weight(x: i32) -> i32 {\n    let w: i32 = match x {\n        0 => 4,\n        1 | 2 => 2,\n        _ => 1,\n    };\n    w.wrapping_mul(10)\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains(
                "let w = select(select(1, 2, ((input[i] == 1) || (input[i] == 2))), 4, (input[i] == 0));"
            ),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_lowers_match_in_tail_position() {
        // `match` is an expression, so it works wherever one does — here as the
        // kernel's own value, not only as a `let` initializer.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: i32) -> i32 {\n    match x {\n        0 => 7,\n        _ => 9,\n    }\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("output[i] = select(9, 7, (input[i] == 0));"),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_match_uses_the_last_arm_as_the_fallback() {
        // Exhaustive without a `_`: the typechecker has already proven that when
        // no earlier arm matches, the last one must, so its pattern is not
        // tested and it becomes the base of the chain. The scrutinee is a
        // comparison because `bool` is not a legal kernel PARAMETER type.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: i32) -> i32 {\n    match x > 0 {\n        true => 1,\n        false => 0,\n    }\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("output[i] = select(0, 1, ((input[i] > 0) == true));"),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_match_composes_inside_a_loop() {
        let func = parse_kernel(
            "#[gpu]\nfn k(x: i32) -> i32 {\n    let mut acc: i32 = 0;\n    for n in 0..3 {\n        acc = acc.wrapping_add(match x {\n            0 => 1,\n            _ => 2,\n        });\n    }\n    acc\n}",
        );
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(
            wgsl.contains("acc = (acc + select(2, 1, (input[i] == 0)));"),
            "{wgsl}"
        );
    }

    #[test]
    fn scalar_kernel_rejects_a_match_guard() {
        let func = parse_kernel(
            "#[gpu]\nfn k(x: i32) -> i32 {\n    match x {\n        0 if x > 1 => 1,\n        _ => 2,\n    }\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(err.to_string().contains("guard"), "{err:?}");
    }

    #[test]
    fn scalar_kernel_rejects_a_binding_match_arm() {
        // The branchless `select` lowering has nowhere to introduce a binding.
        let func = parse_kernel(
            "#[gpu]\nfn k(x: i32) -> i32 {\n    match x {\n        0 => 1,\n        n => n,\n    }\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(err.to_string().contains("cannot bind"), "{err:?}");
    }

    #[test]
    fn scalar_kernel_rejects_a_float_literal_match() {
        let func = parse_kernel(
            "#[gpu]\nfn k(x: f32) -> f32 {\n    match x {\n        1.0 => 1.0,\n        _ => 2.0,\n    }\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(err.to_string().contains("float literal"), "{err:?}");
    }

    #[test]
    fn scalar_kernel_rejects_assignment_to_immutable_local() {
        let func = parse_kernel(
            "#[gpu]\nfn s(x: f32) -> f32 {\n    let a: f32 = x;\n    a = a + 1.0;\n    a\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("immutable local"), "{msg}");
        assert!(msg.contains("let mut"), "{msg}");
    }

    #[test]
    fn scalar_kernel_rejects_assignment_to_the_parameter() {
        // The parameter is a read-only storage load, not a place.
        let func = parse_kernel("#[gpu]\nfn s(x: f32) -> f32 {\n    x = x + 1.0;\n    x\n}");
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not a local binding"), "{msg}");
    }

    #[test]
    fn scalar_kernel_rejects_a_while_body_that_produces_a_value() {
        let func = parse_kernel(
            "#[gpu]\nfn s(x: f32) -> f32 {\n    let mut n: i32 = 0;\n    \
             while n < 2 {\n        n = n.wrapping_add(1);\n        x\n    }\n    x\n}",
        );
        let err = emit_kernel(&func, &[]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must not produce a value"), "{msg}");
    }

    #[test]
    fn scalar_kernel_still_accepts_the_bare_single_expression() {
        // The slice-0 shape keeps working unchanged.
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { x * 2.0 }");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("output[i] = (input[i] * 2.0);"), "{wgsl}");
        assert!(!wgsl.contains("    let doubled"), "{wgsl}");
    }
}
