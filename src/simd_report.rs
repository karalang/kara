//! SIMD scalarization analysis — the diagnostic/guarantee surface for
//! `Vector[T, N]` codegen (phase-7-codegen.md line 308, slice 5a).
//!
//! Every `Vector[T, N]` operation lowers to one of three tiers
//! (design.md § Portable SIMD):
//!
//!   * **Native** — `<N x T>` fits a single target vector register and the
//!     backend selects one native SIMD instruction.
//!   * **Wide** — `N` exceeds the native lane budget but is a power of two,
//!     so the backend emits a small fixed number of full-width vector
//!     instructions (still vectorised, never scalar).
//!   * **Scalar** — the op cannot be expressed as full-width vector
//!     instructions and the backend falls back to a per-lane scalar loop.
//!     Two causes today: the element type has no SIMD lane representation
//!     (128-bit integers), or the lane count is not a power of two (the
//!     backend has to spill the odd remainder to scalars).
//!
//! This module classifies each vector op in a function body against the
//! target's native vector width and reports the scalar ones. It backs two
//! surfaces:
//!
//!   * `#[require_simd]` (slice 5a, this module's `require_simd_errors`):
//!     a function annotated `#[require_simd]` is rejected at build time if
//!     any vector op in its body classifies Scalar — the programmer asked
//!     for a hard guarantee that nothing silently scalarizes.
//!   * `--simd-report=verbose` (slice 5b): renders the full per-function
//!     finding list (every tier, not just Scalar) — reuses the same
//!     `analyze_program` walk.
//!
//! The classification is a *target model*, not a query of LLVM's instruction
//! selector — it predicts the tier from the element width, lane count, and
//! the target's native vector width. That mirrors what any portable-SIMD
//! surface can promise (Rust's `std::simd` makes no stronger guarantee); the
//! prediction is conservative (it only flags Scalar for cases that scalarize
//! on *every* current target), so a `#[require_simd]` pass is sound.
//!
//! Containment: this module imports no `inkwell`/LLVM types — it consumes
//! the plain-data `TypeCheckResult.expr_types` side-table, keeping the
//! codegen-substrate boundary intact (CLAUDE.md § Codegen containment).

use std::collections::HashMap;

use crate::ast::{
    BinOp, Block, CallArg, Expr, ExprKind, Function, ImplItem, Item, Program, Stmt, StmtKind,
    UnaryOp,
};
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{type_display, FloatSize, IntSize, Type, TypeCheckResult, UIntSize};

/// Native vector register width in bits for every v1 target that has a
/// vector unit. x86-64 (SSE baseline, 128-bit XMM), aarch64 / Apple M1
/// (NEON, 128-bit), and wasm SIMD-128 (`v128`) all expose 128-bit
/// vector registers, so the width is a single constant; whether the
/// active target *has* a vector unit at all is [`native_vector_bits`]'s
/// call. The width becomes a per-target lookup when AVX/AVX-512/SVE
/// feature detection lands (see `default_cpu_and_features` in
/// `src/codegen/driver.rs`, which already distinguishes the host
/// triples).
pub const NATIVE_VECTOR_BITS: u32 = 128;

/// Native vector register width for the active target, or `None` when
/// the target has no vector unit and every vector op scalarizes. The
/// only `None` case today: a wasm target with SIMD-128 disabled via
/// `--target-features=-simd128` (`+simd128` is the wasm default —
/// design.md § Portable SIMD; `create_wasm_target_machine` in
/// `src/codegen/driver.rs`). The enablement chain is read through
/// `target::wasm_simd128_enabled` — plain data, no LLVM types, so the
/// codegen-containment posture of this module holds.
pub fn native_vector_bits() -> Option<u32> {
    if crate::target::active_target_is_wasm() && !crate::target::wasm_simd128_enabled() {
        None
    } else {
        Some(NATIVE_VECTOR_BITS)
    }
}

/// Lowering tier a `(T, N)` vector op falls into on the current target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdTier {
    /// Single native vector instruction — `N` lanes fit one register.
    Native,
    /// Power-of-two `N` wider than one register — a few full-width vector
    /// instructions, still no scalar fallback.
    Wide,
    /// Falls back to a per-lane scalar loop. See [`ScalarReason`].
    Scalar,
}

/// Why a `(T, N)` op scalarizes — drives the diagnostic wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarReason {
    /// Lane count is not a power of two; the backend can't map it to a whole
    /// number of full-width vector ops and spills the remainder to scalars.
    NonPowerOfTwoLanes,
    /// Element type has no SIMD lane representation (128-bit integers — no
    /// target has a 128-bit-lane vector ALU op).
    UnsupportedElement,
    /// The target has no vector unit at all, so every vector op scalarizes
    /// regardless of shape (wasm with SIMD-128 disabled via
    /// `--target-features=-simd128`).
    NoVectorUnit,
}

impl ScalarReason {
    fn phrase(self) -> &'static str {
        match self {
            ScalarReason::NonPowerOfTwoLanes => "lane count is not a power of two",
            ScalarReason::UnsupportedElement => {
                "element type has no SIMD lane representation on this target"
            }
            ScalarReason::NoVectorUnit => {
                "wasm SIMD-128 is disabled by `-simd128`, leaving the target no vector unit"
            }
        }
    }
}

/// SIMD bit-width of a vector element type, or `None` if the type is not a
/// lane-shaped scalar (only the numeric primitives `Vector[T, N]` admits as
/// elements, plus `bool` for mask vectors, return `Some`).
fn element_bits(elem: &Type) -> Option<u32> {
    match elem {
        Type::Bool => Some(1),
        Type::Int(IntSize::I8) | Type::UInt(UIntSize::U8) => Some(8),
        Type::Int(IntSize::I16) | Type::UInt(UIntSize::U16) => Some(16),
        Type::Int(IntSize::I32) | Type::UInt(UIntSize::U32) | Type::Float(FloatSize::F32) => {
            Some(32)
        }
        Type::Int(IntSize::I64)
        | Type::UInt(UIntSize::U64)
        | Type::UInt(UIntSize::Usize)
        | Type::Float(FloatSize::F64) => Some(64),
        Type::Int(IntSize::I128) | Type::UInt(UIntSize::U128) => Some(128),
        _ => None,
    }
}

/// Classify a `Vector[T, N]` op against the *active* target — the
/// [`native_vector_bits`] model. See [`classify_for`].
pub fn classify(elem: &Type, lanes: usize) -> SimdTier {
    classify_for(native_vector_bits(), elem, lanes)
}

/// Classify a `Vector[T, N]` op given its element type, concrete lane
/// count, and the target's vector width (`None` = no vector unit, every
/// op scalarizes). The lane count must be a resolved literal — symbolic
/// (`ConstParam` / `ConstVar`) lane counts arise only in un-monomorphized
/// generic contexts and are out of scope for the per-target guarantee.
/// The width is a parameter (rather than read from the process-global
/// target state inside) so unit tests can exercise every target model in
/// one process.
pub fn classify_for(vector_bits: Option<u32>, elem: &Type, lanes: usize) -> SimdTier {
    let Some(native_bits) = vector_bits else {
        return SimdTier::Scalar;
    };
    let Some(bits) = element_bits(elem) else {
        return SimdTier::Scalar;
    };
    // 128-bit integer lanes have no SIMD ALU on any v1 target.
    if bits >= 128 {
        return SimdTier::Scalar;
    }
    if !lanes.is_power_of_two() {
        return SimdTier::Scalar;
    }
    let total = bits.saturating_mul(lanes as u32);
    if total <= native_bits {
        SimdTier::Native
    } else {
        SimdTier::Wide
    }
}

/// The scalarization cause for a `(T, N)` that classifies [`SimdTier::Scalar`],
/// or `None` when it does not scalarize. Element-unsupported is reported in
/// preference to the other causes when several apply (it survives even
/// re-enabling SIMD-128 / fixing the lane shape); a missing vector unit
/// outranks lane shape (with no vector unit, the lane count is moot).
fn scalar_reason(vector_bits: Option<u32>, elem: &Type, lanes: usize) -> Option<ScalarReason> {
    match element_bits(elem) {
        None => Some(ScalarReason::UnsupportedElement),
        Some(bits) if bits >= 128 => Some(ScalarReason::UnsupportedElement),
        Some(_) if vector_bits.is_none() => Some(ScalarReason::NoVectorUnit),
        Some(_) if !lanes.is_power_of_two() => Some(ScalarReason::NonPowerOfTwoLanes),
        Some(_) => None,
    }
}

/// One vector operation discovered in a function body, with its classified
/// tier. Slice 5a consumes only the `Scalar` ones inside `#[require_simd]`
/// functions; slice 5b's `--simd-report=verbose` renders the full list.
#[derive(Debug, Clone)]
pub struct SimdFinding {
    /// Source span of the operation.
    pub span: Span,
    /// Name of the enclosing function (impl methods render as `Type.method`).
    pub func_name: String,
    /// Whether the enclosing function carries `#[require_simd]`.
    pub require_simd: bool,
    /// Short human description of the op (`element-wise \`+\``, `\`reduce_sum\``…).
    pub op_desc: String,
    /// Display form of the element type `T`.
    pub element: String,
    /// Concrete lane count `N`.
    pub lanes: usize,
    /// Classified lowering tier on the current target.
    pub tier: SimdTier,
    /// Scalarization cause when `tier == Scalar`.
    pub reason: Option<ScalarReason>,
}

impl SimdFinding {
    /// `Vector[T, N]` source-display form of this op's governing vector type.
    pub fn vector_display(&self) -> String {
        format!("Vector[{}, {}]", self.element, self.lanes)
    }

    /// `#[require_simd]` rejection message for a `Scalar` finding.
    pub fn message(&self) -> String {
        let reason = self
            .reason
            .map(|r| r.phrase())
            .unwrap_or("would lower to a scalar loop");
        format!(
            "`#[require_simd]` violated: {} on `Vector[{}, {}]` would lower to a scalar loop on this target ({reason})",
            self.op_desc, self.element, self.lanes,
        )
    }

    /// Actionable hint for a `Scalar` finding.
    pub fn help(&self) -> String {
        match self.reason {
            Some(ScalarReason::NonPowerOfTwoLanes) => format!(
                "use a power-of-two lane count (e.g. `Vector[{}, {}]`), or remove `#[require_simd]` to accept the scalar fallback",
                self.element,
                self.lanes.next_power_of_two(),
            ),
            Some(ScalarReason::UnsupportedElement) => format!(
                "`{}` lanes have no SIMD representation; pick a 8/16/32/64-bit element type, or remove `#[require_simd]`",
                self.element,
            ),
            Some(ScalarReason::NoVectorUnit) => {
                "drop `-simd128` from the target-features chain (wasm SIMD-128 is on by default), or remove `#[require_simd]` to accept the scalar fallback".to_string()
            }
            None => "remove `#[require_simd]` to accept the scalar fallback".to_string(),
        }
    }
}

/// Walk every function in `program` and classify each `Vector[T, N]`
/// operation in its body. Returns one [`SimdFinding`] per op (all tiers).
/// `typed` supplies the `expr_types` side-table that types operands; when
/// it is `None` (typecheck didn't run) the result is empty.
pub fn analyze_program(program: &Program, typed: Option<&TypeCheckResult>) -> Vec<SimdFinding> {
    let Some(typed) = typed else {
        return Vec::new();
    };
    let mut scan = Scan {
        expr_types: &typed.expr_types,
        vector_method_receivers: &typed.vector_method_receivers,
        // Resolved once per walk: the per-op classification must agree
        // with itself across the whole program.
        vector_bits: native_vector_bits(),
        findings: Vec::new(),
        func_name: String::new(),
        require_simd: false,
    };
    for item in &program.items {
        match item {
            Item::Function(f) => scan.scan_function(&f.name, f),
            Item::ImplBlock(b) => {
                let prefix = type_expr_name(&b.target_type);
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        let qualified = format!("{prefix}.{}", m.name);
                        scan.scan_function(&qualified, m);
                    }
                }
            }
            _ => {}
        }
    }
    scan.findings
}

/// Filter an `analyze_program` result to the hard errors a build must
/// reject: `Scalar`-tier ops inside `#[require_simd]` functions.
pub fn require_simd_errors(findings: &[SimdFinding]) -> Vec<SimdFinding> {
    findings
        .iter()
        .filter(|f| f.require_simd && f.tier == SimdTier::Scalar)
        .cloned()
        .collect()
}

/// Render the `--simd-report=verbose` table: every vector op found in the
/// program, grouped by function, with its classified lowering tier and (for
/// `Scalar`) the scalarization cause. Findings are listed in source order
/// within each function — `analyze_program` walks the body top-down.
/// Returns the full report as a string (the CLI prints it to stdout).
pub fn render_simd_report(findings: &[SimdFinding]) -> String {
    let mut out = String::new();
    match native_vector_bits() {
        Some(bits) => out.push_str(&format!(
            "SIMD lowering report (native vector width: {bits} bits)\n"
        )),
        None => out.push_str(
            "SIMD lowering report (no vector unit: wasm SIMD-128 disabled by `-simd128`)\n",
        ),
    }
    if findings.is_empty() {
        out.push_str("  <no vector operations>\n");
        return out;
    }
    // Column widths for the `Vector[T, N]` and op-description columns, so the
    // tier column lines up across rows.
    let type_w = findings
        .iter()
        .map(|f| f.vector_display().len())
        .max()
        .unwrap_or(0);
    let desc_w = findings.iter().map(|f| f.op_desc.len()).max().unwrap_or(0);

    let mut current = None;
    for f in findings {
        if current != Some(&f.func_name) {
            out.push_str(&format!("\n  fn {}\n", f.func_name));
            current = Some(&f.func_name);
        }
        let tier = match f.tier {
            SimdTier::Native => "native".to_string(),
            SimdTier::Wide => "wide".to_string(),
            SimdTier::Scalar => match f.reason {
                Some(r) => format!("SCALAR — {}", r.phrase()),
                None => "SCALAR".to_string(),
            },
        };
        out.push_str(&format!(
            "    {:<type_w$}  {:<desc_w$}  {}\n",
            f.vector_display(),
            f.op_desc,
            tier,
        ));
    }
    out
}

/// Best-effort leading path segment of an impl target type, used to qualify
/// method names in diagnostics (`Vec3.dot`). Falls back to the raw display.
fn type_expr_name(ty: &crate::ast::TypeExpr) -> String {
    match &ty.kind {
        crate::ast::TypeKind::Path(p) => p
            .segments
            .last()
            .cloned()
            .unwrap_or_else(|| "?".to_string()),
        other => format!("{other:?}"),
    }
}

struct Scan<'a> {
    expr_types: &'a HashMap<SpanKey, Type>,
    vector_method_receivers: &'a HashMap<SpanKey, (Type, usize)>,
    /// Active target's vector width ([`native_vector_bits`], resolved once).
    vector_bits: Option<u32>,
    findings: Vec<SimdFinding>,
    func_name: String,
    require_simd: bool,
}

impl Scan<'_> {
    fn scan_function(&mut self, name: &str, f: &Function) {
        self.func_name = name.to_string();
        self.require_simd = f.attributes.iter().any(|a| a.is_bare("require_simd"));
        self.walk_block(&f.body);
    }

    /// `Vector[T, N]` element type + concrete lane count for an expr's
    /// inferred type, or `None` if the expr is not a vector with a resolved
    /// literal lane count.
    ///
    /// Note: a `MethodCall`/`Binary` node's span equals its receiver/left
    /// operand's span, and the outer node's *result* type wins in `expr_types`
    /// (last write). So this yields the **outermost** expr's result type at a
    /// span — correct for arithmetic/bitwise ops (result == operand type) and
    /// for vector-returning calls (construction / `splat` / `from_*` / `cross`
    /// / `select`), but NOT for scalar-returning reductions or comparison
    /// masks. The receiver of an instance method is recovered separately via
    /// `vector_method_receivers`; comparison operands via the right operand.
    fn vector_type_of(&self, e: &Expr) -> Option<(Type, usize)> {
        match self.expr_types.get(&SpanKey::from_span(&e.span)) {
            Some(Type::Vector { element, lanes }) => {
                lanes.as_usize().map(|n| ((**element).clone(), n))
            }
            _ => None,
        }
    }

    /// Receiver `Vector[T, N]` recorded by the typechecker for a vector
    /// instance-method call (`reduce_*` / `dot` / `cross` / `select`), keyed
    /// by the method-call span. Recovers the type `vector_type_of` loses to
    /// the result-type overwrite for scalar-returning methods.
    fn method_receiver_of(&self, call_span: &Span) -> Option<(Type, usize)> {
        self.vector_method_receivers
            .get(&SpanKey::from_span(call_span))
            .cloned()
    }

    fn record(&mut self, span: &Span, op_desc: String, elem: &Type, lanes: usize) {
        let tier = classify_for(self.vector_bits, elem, lanes);
        let reason = if tier == SimdTier::Scalar {
            scalar_reason(self.vector_bits, elem, lanes)
        } else {
            None
        };
        self.findings.push(SimdFinding {
            span: span.clone(),
            func_name: self.func_name.clone(),
            require_simd: self.require_simd,
            op_desc,
            element: type_display(elem),
            lanes,
            tier,
            reason,
        });
    }

    fn walk_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(tail) = &block.final_expr {
            self.walk_expr(tail);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. }
            | StmtKind::LetElse { value, .. }
            | StmtKind::Expr(value) => self.walk_expr(value),
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => self.walk_block(body),
            StmtKind::LetUninit { .. } => {}
        }
        if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
            self.walk_block(else_block);
        }
    }

    fn walk_args(&mut self, args: &[CallArg]) {
        for arg in args {
            self.walk_expr(&arg.value);
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Binary { op, left, right } => {
                // Prefer the RIGHT operand's type: a comparison's result is a
                // `Vector[bool, N]` mask (and the binary node's span == the
                // left operand's span, so both are overwritten with the mask
                // result in `expr_types`), but the right operand keeps its true
                // `Vector[T, N]` element. For arithmetic/bitwise the result
                // equals the operand type, so either operand is correct.
                if let Some((elem, n)) = self
                    .vector_type_of(right)
                    .or_else(|| self.vector_type_of(left))
                    .or_else(|| self.vector_type_of(expr))
                {
                    self.record(&expr.span, binop_desc(op), &elem, n);
                }
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { op, operand } => {
                if !matches!(op, UnaryOp::Deref | UnaryOp::Not) {
                    if let Some((elem, n)) = self.vector_type_of(operand) {
                        self.record(&expr.span, unop_desc(op), &elem, n);
                    }
                }
                self.walk_expr(operand);
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if let Some((elem, n)) = self.method_receiver_of(&expr.span) {
                    // Instance method on a vector receiver (reduce_*, dot,
                    // cross, select, …). The receiver `(T, N)` comes from the
                    // typechecker side-table because the method node overwrites
                    // its own span (== receiver span) with the result type.
                    self.record(&expr.span, format!("`{method}`"), &elem, n);
                } else if let Some((elem, n)) = self.vector_type_of(expr) {
                    // Static constructor whose result is a vector
                    // (`Vector[T, N].splat/from_array/from_slice(...)`); the
                    // receiver is the `Vector` type-path, not a vector value.
                    self.record(&expr.span, format!("`{method}`"), &elem, n);
                }
                self.walk_expr(object);
                self.walk_args(args);
            }
            ExprKind::Call { callee, args } => {
                // Vector construction `Vector[T, N](l0, l1, ...)` lowers to an
                // insertelement chain — its result type is the vector.
                if let Some((elem, n)) = self.vector_type_of(expr) {
                    self.record(&expr.span, "construction".to_string(), &elem, n);
                }
                self.walk_expr(callee);
                self.walk_args(args);
            }
            ExprKind::Index { object, index } => {
                // Lane read (`v[i]`) is an inherently scalar extractelement,
                // not a scalarized vector op — don't flag it; just recurse.
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Question(inner)
            | ExprKind::FieldAccess { object: inner, .. }
            | ExprKind::TupleIndex { object: inner, .. }
            | ExprKind::Cast { expr: inner, .. } => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    self.walk_args(arglist);
                }
            }
            ExprKind::NilCoalesce { left, right } | ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.walk_expr(value);
                self.walk_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.walk_expr(iterable);
                self.walk_block(body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body)
            }
            ExprKind::Closure { body, .. } => self.walk_expr(body),
            ExprKind::Return(inner) | ExprKind::Break { value: inner, .. } => {
                if let Some(e) = inner {
                    self.walk_expr(e);
                }
            }
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for it in items {
                    self.walk_expr(it);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for it in items {
                    self.walk_expr(it);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.walk_expr(mutex);
                self.walk_block(body);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            // Leaves and forms with no nested expressions.
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::InterpolatedStringLit(_)
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::Continue { .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }
}

fn binop_desc(op: &BinOp) -> String {
    let sym = match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::Range => "..",
        BinOp::RangeInclusive => "..=",
    };
    let kind = match op {
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
            "comparison"
        }
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => "bitwise",
        _ => "element-wise",
    };
    format!("{kind} `{sym}`")
}

fn unop_desc(op: &UnaryOp) -> String {
    match op {
        UnaryOp::Neg => "element-wise `-`".to_string(),
        UnaryOp::BitNot => "bitwise `~`".to_string(),
        UnaryOp::Not => "`!`".to_string(),
        UnaryOp::Deref => "`*`".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_native_pow2_within_register() {
        // 4 × i32 = 128 bits = one register.
        assert_eq!(classify(&Type::Int(IntSize::I32), 4), SimdTier::Native);
        // 2 × f64 = 128 bits.
        assert_eq!(classify(&Type::Float(FloatSize::F64), 2), SimdTier::Native);
        // 2 × i32 = 64 bits, pow2, under a register → Native.
        assert_eq!(classify(&Type::Int(IntSize::I32), 2), SimdTier::Native);
    }

    #[test]
    fn classify_wide_pow2_over_register() {
        // 8 × i32 = 256 bits > 128, pow2 → Wide (two 128-bit ops).
        assert_eq!(classify(&Type::Int(IntSize::I32), 8), SimdTier::Wide);
        // 4 × f64 = 256 bits.
        assert_eq!(classify(&Type::Float(FloatSize::F64), 4), SimdTier::Wide);
    }

    #[test]
    fn classify_scalar_non_pow2_lanes() {
        // 3 lanes is not a power of two regardless of element width.
        assert_eq!(classify(&Type::Int(IntSize::I32), 3), SimdTier::Scalar);
        assert_eq!(
            scalar_reason(Some(NATIVE_VECTOR_BITS), &Type::Int(IntSize::I32), 3),
            Some(ScalarReason::NonPowerOfTwoLanes)
        );
        assert_eq!(classify(&Type::Float(FloatSize::F32), 5), SimdTier::Scalar);
    }

    #[test]
    fn classify_scalar_unsupported_element() {
        // 128-bit integer lanes have no SIMD ALU even with a pow2 lane count.
        assert_eq!(classify(&Type::Int(IntSize::I128), 2), SimdTier::Scalar);
        assert_eq!(
            scalar_reason(Some(NATIVE_VECTOR_BITS), &Type::Int(IntSize::I128), 2),
            Some(ScalarReason::UnsupportedElement)
        );
        assert_eq!(classify(&Type::UInt(UIntSize::U128), 4), SimdTier::Scalar);
    }

    #[test]
    fn classify_no_vector_unit_scalarizes_everything() {
        // The wasm `-simd128` model (`native_vector_bits()` = None): every
        // shape scalarizes, including ones Native/Wide on every other target.
        for (elem, lanes) in [
            (Type::Float(FloatSize::F32), 4), // Native elsewhere
            (Type::Int(IntSize::I32), 8),     // Wide elsewhere
            (Type::Int(IntSize::I8), 2),      // tiny pow2, Native elsewhere
        ] {
            assert_eq!(classify_for(None, &elem, lanes), SimdTier::Scalar);
            assert_eq!(
                scalar_reason(None, &elem, lanes),
                Some(ScalarReason::NoVectorUnit)
            );
        }
        // Cause priority: an unsupported element outranks the missing
        // vector unit (re-enabling simd128 wouldn't fix it)…
        assert_eq!(
            scalar_reason(None, &Type::Int(IntSize::I128), 2),
            Some(ScalarReason::UnsupportedElement)
        );
        // …and the missing vector unit outranks lane shape (with no unit,
        // the lane count is moot).
        assert_eq!(
            scalar_reason(None, &Type::Int(IntSize::I32), 3),
            Some(ScalarReason::NoVectorUnit)
        );
    }

    #[test]
    fn classify_for_native_width_matches_default_model() {
        // `classify` is `classify_for` at the active target's width; the
        // default (native) target model is Some(128).
        assert_eq!(
            classify_for(Some(NATIVE_VECTOR_BITS), &Type::Float(FloatSize::F32), 4),
            SimdTier::Native
        );
        assert_eq!(
            classify_for(Some(NATIVE_VECTOR_BITS), &Type::Int(IntSize::I32), 8),
            SimdTier::Wide
        );
    }

    #[test]
    fn classify_small_pow2_is_native() {
        // 2 × i8 = 16 bits — well under a register, single op.
        assert_eq!(classify(&Type::Int(IntSize::I8), 2), SimdTier::Native);
        // 16 × i8 = 128 bits — exactly one register.
        assert_eq!(classify(&Type::Int(IntSize::I8), 16), SimdTier::Native);
        // 32 × i8 = 256 bits — Wide.
        assert_eq!(classify(&Type::Int(IntSize::I8), 32), SimdTier::Wide);
    }

    fn finding(func: &str, elem: Type, lanes: usize, op: &str) -> SimdFinding {
        let tier = classify(&elem, lanes);
        SimdFinding {
            span: Span::default(),
            func_name: func.to_string(),
            require_simd: false,
            op_desc: op.to_string(),
            element: type_display(&elem),
            lanes,
            tier,
            reason: if tier == SimdTier::Scalar {
                scalar_reason(Some(NATIVE_VECTOR_BITS), &elem, lanes)
            } else {
                None
            },
        }
    }

    #[test]
    fn render_empty_report() {
        let out = render_simd_report(&[]);
        assert!(out.contains("native vector width: 128 bits"));
        assert!(out.contains("<no vector operations>"));
    }

    #[test]
    fn render_groups_by_function_and_marks_tiers() {
        let fs = vec![
            finding("add3", Type::Int(IntSize::I32), 3, "element-wise `+`"),
            finding("dot4", Type::Float(FloatSize::F32), 4, "`dot`"),
            finding("wide8", Type::Int(IntSize::I32), 8, "element-wise `*`"),
        ];
        let out = render_simd_report(&fs);
        assert!(out.contains("fn add3"), "groups by function:\n{out}");
        assert!(out.contains("fn dot4"));
        assert!(out.contains("fn wide8"));
        // The scalar op is flagged with its cause; the native/wide ones aren't.
        assert!(
            out.contains("Vector[i32, 3]") && out.contains("SCALAR"),
            "scalar op rendered:\n{out}"
        );
        assert!(
            out.contains("power of two"),
            "scalar cause rendered:\n{out}"
        );
        assert!(out.contains("native"), "native tier rendered:\n{out}");
        assert!(out.contains("wide"), "wide tier rendered:\n{out}");
    }
}
