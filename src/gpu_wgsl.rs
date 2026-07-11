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
    BinOp, Block, CallArg, Expr, ExprKind, Function, Param, StmtKind, TypeExpr, TypeKind, UnaryOp,
};
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

/// Emit the WGSL compute shader for a slice-0 element-wise-map `#[gpu]`
/// kernel. On success the returned string is a complete, standalone module
/// with `@group(0) @binding(0)` = read `input: array<f32>`, `@binding(1)` =
/// read_write `output: array<f32>`, and a `@compute @workgroup_size(64) fn
/// main` entry point — exactly the layout the runtime `dispatch_f32_map`
/// expects.
pub fn emit_kernel(func: &Function, helpers: &[&Function]) -> Result<String, WgslError> {
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

    let body_expr = kernel_return_expr(func)?;
    let resolve = |n: &str| (n == param_name).then(|| "input[i]".to_string());
    let body_wgsl = lower_expr(body_expr, &resolve, &helper_names)?;

    Ok(format!(
        "{helper_defs}@group(0) @binding(0) var<storage, read>       input:  array<{scalar}>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<{scalar}>;\n\
         \n\
         @compute @workgroup_size({WORKGROUP_SIZE})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let i = gid.x;\n\
         \x20   if (i >= arrayLength(&input)) {{ return; }}\n\
         \x20   output[i] = {body_wgsl};\n\
         }}\n"
    ))
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

/// The expression whose value the kernel returns — the block tail expression,
/// or a trailing `return <expr>;`. Slice-0 kernels have no locals, so any
/// preceding statements (other than the trailing return) are rejected.
fn kernel_return_expr(func: &Function) -> Result<&Expr, WgslError> {
    let block = &func.body;
    if let Some(final_expr) = &block.final_expr {
        if !block.stmts.is_empty() {
            return Err(WgslError::UnsupportedBody(
                "a slice-0 GPU kernel body must be a single expression (no locals)".to_string(),
            ));
        }
        return Ok(final_expr);
    }
    // No tail expression: accept a lone trailing `return <expr>;`.
    match block.stmts.as_slice() {
        [stmt] => {
            if let StmtKind::Expr(Expr {
                kind: ExprKind::Return(Some(inner)),
                ..
            }) = &stmt.kind
            {
                return Ok(inner);
            }
            Err(WgslError::UnsupportedBody(
                "a slice-0 GPU kernel body must be a single expression or `return <expr>;`"
                    .to_string(),
            ))
        }
        _ => Err(WgslError::UnsupportedBody(
            "a slice-0 GPU kernel body must be a single expression (no locals)".to_string(),
        )),
    }
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
        ExprKind::Call { callee, args } => {
            lower_call(callee, args, &|e| lower_expr(e, resolve, helpers), helpers)
        }
        _ => Err(WgslError::UnsupportedBody(
            "unsupported expression in a GPU kernel body (numeric literals, `+ - * / %`, \
             unary `-`, comparisons, value `if`/`else`, `#[gpu]` helper calls)"
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
        _ => Err(WgslError::UnsupportedBody(
            "only arithmetic (`+ - * / %`) and comparison (`> < >= <= == !=`) operators \
             are supported in a GPU kernel"
                .to_string(),
        )),
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
            _ => {}
        }
    }
    fn calls_in_block(b: &Block, all: &HashMap<String, &Function>, out: &mut Vec<String>) {
        if let Some(e) = &b.final_expr {
            calls_in(e, all, out);
        }
        for s in &b.stmts {
            if let StmtKind::Expr(e) = &s.kind {
                calls_in(e, all, out);
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

/// Emit a reachable `#[gpu]` helper as a WGSL `fn name(p0: f32, …) -> f32 { return
/// <body>; }`. Params are `f32` scalars (the GPU-LBM-5 floor); the body is lowered
/// with each parameter resolving to itself (identity) and calls to other helpers
/// recognized via `helper_names`.
fn emit_helper_def(func: &Function, helper_names: &HashSet<String>) -> Result<String, WgslError> {
    if func.self_param.is_some() {
        return Err(WgslError::UnsupportedSignature(format!(
            "GPU helper `{}` cannot take a self receiver",
            func.name
        )));
    }
    let mut param_names = HashSet::new();
    let mut sig = String::new();
    for (i, p) in func.params.iter().enumerate() {
        let name = p.name().ok_or_else(|| {
            WgslError::UnsupportedSignature(format!(
                "GPU helper `{}` parameter must be a plain binding",
                func.name
            ))
        })?;
        wgsl_scalar(&p.ty, "helper parameter")?; // f32/i32/u32
        if i > 0 {
            sig.push_str(", ");
        }
        sig.push_str(&format!("{name}: f32"));
        param_names.insert(name.to_string());
    }
    match &func.return_type {
        Some(ty) => {
            wgsl_scalar(ty, "helper return type")?;
        }
        None => {
            return Err(WgslError::UnsupportedSignature(format!(
                "GPU helper `{}` must return a scalar",
                func.name
            )));
        }
    }
    let body = kernel_return_expr(func)?;
    let resolve = |n: &str| -> Option<String> { param_names.get(n).cloned() };
    let body_wgsl = lower_expr(body, &resolve, helper_names)?;
    Ok(format!(
        "fn {}({sig}) -> f32 {{ return {body_wgsl}; }}\n",
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

    // The body is a struct literal; store each group's fields to its output.
    let body = kernel_return_expr(func)?;
    let stores = lower_struct_return(body, param_name, groups, &helper_names, &uniform_set)?;

    // arrayLength guard keys off the first input buffer (all equal length).
    let guard_group = &groups[0].name;

    Ok(format!(
        "{helper_defs}{structs}{decls}\n\
         @compute @workgroup_size({WORKGROUP_SIZE})\n\
         fn main(@builtin(global_invocation_id) gid: vec3<u32>) {{\n\
         \x20   let i = gid.x;\n\
         \x20   if (i >= arrayLength(&{guard_group}_in)) {{ return; }}\n\
         {materialize}{stores}\
         }}\n"
    ))
}

/// Lower the kernel's struct-valued return into one output store per group: a
/// single-field group stores its field's value; a multi-field group stores a
/// `G_<name>(...)` constructor over its fields. The return may be a struct
/// literal, the whole-input parameter, or a struct-valued `if` (GPU-LBM-4b) —
/// see [`struct_field_wgsl`].
fn lower_struct_return(
    expr: &Expr,
    param_name: &str,
    groups: &[SoaGpuGroup],
    helpers: &HashSet<String>,
    uniforms: &HashSet<String>,
) -> Result<String, WgslError> {
    let mut out = String::new();
    for g in groups {
        if g.is_multi() {
            let vals = g
                .fields
                .iter()
                .map(|f| struct_field_wgsl(expr, f, param_name, groups, helpers, uniforms))
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
                struct_field_wgsl(expr, &g.fields[0], param_name, groups, helpers, uniforms)?
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
fn struct_field_wgsl(
    expr: &Expr,
    field: &str,
    param_name: &str,
    groups: &[SoaGpuGroup],
    helpers: &HashSet<String>,
    uniforms: &HashSet<String>,
) -> Result<String, WgslError> {
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
            lower_soa_expr(&init.value, param_name, groups, helpers, uniforms)
        }
        ExprKind::Identifier(name) if name == param_name => {
            if groups.iter().any(|g| g.fields.iter().any(|gf| gf == field)) {
                Ok(format!("{param_name}_{field}"))
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
            let cond = lower_soa_expr(condition, param_name, groups, helpers, uniforms)?;
            let t = struct_field_wgsl(then_e, field, param_name, groups, helpers, uniforms)?;
            let e = struct_field_wgsl(else_e, field, param_name, groups, helpers, uniforms)?;
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
fn lower_soa_expr(
    expr: &Expr,
    param_name: &str,
    groups: &[SoaGpuGroup],
    helpers: &HashSet<String>,
    uniforms: &HashSet<String>,
) -> Result<String, WgslError> {
    match &expr.kind {
        ExprKind::FieldAccess { object, field } => {
            if let ExprKind::Identifier(obj) = &object.kind {
                if obj == param_name {
                    if groups.iter().any(|g| g.fields.iter().any(|gf| gf == field)) {
                        return Ok(format!("{param_name}_{field}"));
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
            let l = lower_soa_expr(left, param_name, groups, helpers, uniforms)?;
            let r = lower_soa_expr(right, param_name, groups, helpers, uniforms)?;
            Ok(format!("({l} {op_str} {r})"))
        }
        ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } => {
            let inner = lower_soa_expr(operand, param_name, groups, helpers, uniforms)?;
            Ok(format!("-({inner})"))
        }
        // Post-lowering operator methods (`a + b` → `<type>.add(a, b)`) and
        // `#[gpu]` helper calls — the SoA emitter runs at codegen, after lowering.
        ExprKind::Call { callee, args } => lower_call(
            callee,
            args,
            &|e| lower_soa_expr(e, param_name, groups, helpers, uniforms),
            helpers,
        ),
        // Value `if c { a } else { b }` → WGSL `select(b, a, c)` (GPU-LBM-4).
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let (then_e, else_e) = if_branches(then_block, else_branch)?;
            let cond = lower_soa_expr(condition, param_name, groups, helpers, uniforms)?;
            let t = lower_soa_expr(then_e, param_name, groups, helpers, uniforms)?;
            let e = lower_soa_expr(else_e, param_name, groups, helpers, uniforms)?;
            Ok(format!("select({e}, {t}, {cond})"))
        }
        // A bare identifier naming a scalar uniform parameter → `<name>_u[0]`.
        ExprKind::Identifier(name) if uniforms.contains(name) => Ok(format!("{name}_u[0]")),
        ExprKind::Identifier(name) => Err(WgslError::UnsupportedBody(format!(
            "identifier `{name}` — a struct GPU kernel body accesses `<param>.<field>` or a \
             uniform, not the whole struct value"
        ))),
        _ => Err(WgslError::UnsupportedBody(
            "unsupported expression in a struct GPU kernel body (field access, numeric \
             literals, `+ - * / %`, unary `-`, comparisons, value `if`/`else`, helper calls)"
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
        assert!(wgsl.contains("let i = gid.x;"));
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
        let func = parse_kernel("#[gpu]\nfn k(x: i32) -> i32 { x * 2 }\n");
        let wgsl = emit_kernel(&func, &[]).unwrap();
        assert!(wgsl.contains("input:  array<i32>;"), "{wgsl}");
        assert!(wgsl.contains("output: array<i32>;"), "{wgsl}");
        assert!(wgsl.contains("output[i] = (input[i] * 2);"), "{wgsl}");
    }

    #[test]
    fn lowers_u32_kernel_over_u32_array() {
        let func = parse_kernel("#[gpu]\nfn k(x: u32) -> u32 { x + 1 }\n");
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
    fn rejects_body_with_locals() {
        let func = parse_kernel("#[gpu]\nfn k(x: f32) -> f32 { let y = x * 2.0; y }\n");
        let err = emit_kernel(&func, &[]).unwrap_err();
        assert!(matches!(err, WgslError::UnsupportedBody(_)), "{err:?}");
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
}
