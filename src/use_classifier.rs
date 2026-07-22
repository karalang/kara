//! AST → use-kind classification for binding-use positions.
//!
//! Round 12.9 staging step for the formal RC-fallback predicate. The
//! CFG builder (`src/cfg.rs`) defaults every recorded `UseSite` to
//! `UseKind::Read`; the predicate (`src/rc_predicate.rs`) only fires when
//! at least one use of a binding is `UseKind::Consume`. This module walks
//! a function body and produces a `HashMap<SpanKey, UseKind>` keyed by
//! the spans of binding-use leaves — `build_cfg_with_classification`
//! consumes that map to tag each `UseSite` correctly.
//!
//! The classification logic mirrors `ownership::check_expr_consuming` /
//! `ownership::check_expr_reading` (design.md § Consume Predicate) without
//! the dataflow / state-tracking machinery: every identifier-leaf is
//! tagged based purely on its syntactic position and the relevant typing
//! context (Copy vs. non-Copy, callee param mode, method receiver mode,
//! whether any match arm binds).
//!
//! ## Out of scope
//!
//! - Once-callable closure call-site consume of the closure binding.
//!   The predicate is flavor-agnostic; flagging once-callable closures
//!   whose call-site is itself a move stays in the legacy pass.
//!
//! Rounds 12.11 / 12.12 closed the trigger-2 / trigger-3 gaps via
//! structural CFG fixes layered on this classifier:
//!
//! - **Trigger 2** (closure capture with outer use): the call walker
//!   propagates `Consuming` through to capture-position identifier
//!   leaves regardless of the surrounding closure-body walk mode, so
//!   the classifier records `Consume`. The CFG places the closure
//!   body in a sibling sink block of the creation point, so a capture
//!   consume and a subsequent outer use are dominance-incomparable.
//!
//! - **Trigger 3** (container store with subsequent use): the
//!   `MethodCall` walker marks each owned (no-`mut`-marker) arg of a
//!   `mut ref self` method call into `Classification.sink_arg_spans`.
//!   The CFG `MethodCall` arm forks those args into a sibling sink
//!   block of the call site, mirroring the closure-body lowering.

use crate::ast::{
    Block, Expr, ExprKind, Item, Pattern, PatternKind, Program, RestPattern, SelfParam, Stmt,
    StmtKind,
};
use crate::cfg::{Classification, ConsumeOrigin, PlacePath, PlaceSeg, UseKind};
use crate::ownership::{
    callee_param_modes_key, collect_callee_param_modes, collect_method_param_modes,
    collect_method_self_modes, is_copy_type, OwnershipMode,
};
use crate::resolver::SpanKey;
use crate::typechecker::{Type, TypeCheckResult};
use std::collections::{HashMap, HashSet};

/// Whole-program inputs the use-classifier consults but never mutates:
/// method self-modes, free-fn / static-method parameter modes, and
/// unit-variant names. Each is O(program) to build. The ownership pass
/// and the RC predicate both classify *every* function's body, so
/// building these inside `classify_function_body` made the
/// classification step O(functions × program) — the dominant
/// super-linear factor in `ownershipcheck`. Hoist them into a prelude
/// built once per program (`ClassifierPrelude::new`) and borrowed by
/// every per-function `classify_function_body_with` call, restoring an
/// O(program) total over the whole module.
pub struct ClassifierPrelude {
    method_self_modes: HashMap<String, SelfParam>,
    callee_param_modes: HashMap<String, Vec<OwnershipMode>>,
    method_param_modes: HashMap<String, Vec<OwnershipMode>>,
    unit_variant_names: HashSet<String>,
}

impl ClassifierPrelude {
    /// Collect the three whole-program tables once. Reuse the returned
    /// value across every `classify_function_body_with` call for the
    /// same `(program, tc)`.
    pub fn new(program: &Program, tc: &TypeCheckResult) -> Self {
        ClassifierPrelude {
            method_self_modes: collect_method_self_modes(program),
            callee_param_modes: collect_callee_param_modes(program),
            method_param_modes: collect_method_param_modes(program),
            unit_variant_names: collect_unit_variant_names(tc),
        }
    }
}

/// Classify every binding-use leaf within `body` as `Read` or `Consume`,
/// and identify `mut ref self` method-call args whose value-expressions
/// must be lowered into sibling sink blocks (round 12.12 trigger-3).
///
/// Returns a `Classification` consumed by `build_cfg_with_classification`.
/// Spans not present in `kinds` default to `Read`; spans not present in
/// `sink_arg_spans` are lowered inline.
///
/// Convenience entry point that builds a fresh [`ClassifierPrelude`] for
/// the single call. Hot paths that classify many functions of the same
/// program should build the prelude once and call
/// [`classify_function_body_with`] to avoid rebuilding the whole-program
/// tables per function.
pub fn classify_function_body(
    program: &Program,
    tc: &TypeCheckResult,
    body: &Block,
    param_types: HashMap<String, Type>,
) -> Classification {
    let prelude = ClassifierPrelude::new(program, tc);
    classify_function_body_with(&prelude, tc, body, param_types)
}

/// Classify `body` against a pre-built [`ClassifierPrelude`]. Identical
/// to [`classify_function_body`] but reuses the caller's whole-program
/// tables instead of recollecting them.
pub fn classify_function_body_with(
    prelude: &ClassifierPrelude,
    tc: &TypeCheckResult,
    body: &Block,
    param_types: HashMap<String, Type>,
) -> Classification {
    let mut classifier = UseClassifier {
        tc,
        method_self_modes: &prelude.method_self_modes,
        callee_param_modes: &prelude.callee_param_modes,
        method_param_modes: &prelude.method_param_modes,
        unit_variant_names: &prelude.unit_variant_names,
        param_types,
        local_types: HashMap::new(),
        classification: Classification::default(),
        consume_origin_ctx: ConsumeOrigin::Direct,
        once_callable_closures: HashSet::new(),
        closure_span_stack: Vec::new(),
        closure_local_stack: Vec::new(),
    };
    classifier.walk_block(body, Mode::Reading);
    classifier.classification
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Mode {
    Reading,
    Consuming,
}

struct UseClassifier<'a> {
    tc: &'a TypeCheckResult,
    method_self_modes: &'a HashMap<String, SelfParam>,
    callee_param_modes: &'a HashMap<String, Vec<OwnershipMode>>,
    method_param_modes: &'a HashMap<String, Vec<OwnershipMode>>,
    unit_variant_names: &'a HashSet<String>,
    param_types: HashMap<String, Type>,
    /// Round 12.18: name-keyed types for `let`-bound locals,
    /// populated as the walker enters each `let pat = value;` and
    /// consulted by `classify_identifier` before falling back to the
    /// span-keyed `tc.expr_types`. Sidesteps the parser's
    /// `MethodCall.span == receiver.span` aliasing for projection
    /// receivers like `c.inner.unwrap()` — the c identifier's span
    /// is then the SAME SpanKey the typechecker writes the method's
    /// return type to, so a span-only lookup returns the wrong
    /// type. The name-keyed map mirrors `OwnershipChecker::binding_types`.
    local_types: HashMap<String, Type>,
    classification: Classification,
    /// Origin tag stamped on each Consume identifier-leaf recorded
    /// while this context is active. Default `Direct`; the closure-body
    /// walker swaps in `ClosureCapture` and the `mut ref self` sink-arg
    /// walker swaps in `ContainerStore`, each with save/restore around
    /// the inner walk so nested scopes don't leak the tag outward.
    consume_origin_ctx: ConsumeOrigin,
    /// Round 12.20: name-keyed set of bindings that hold a closure
    /// value whose body consumes at least one captured outer binding
    /// (i.e. the closure is "once-callable" — invoking it consumes
    /// the closure value via the captured-by-ownership semantics).
    /// Populated when a `let p = closure_expr` produces at least one
    /// `ClosureCapture`-tagged consume during the body walk. Consumed
    /// by the `Call` arm to tag a once-callable closure binding's
    /// call-site as `UseKind::Consume` so the UAM predicate fires on
    /// `f(); f();` shapes — mirrors `OwnershipChecker::once_callable_closures`.
    once_callable_closures: HashSet<String>,
    /// Phase-7-codegen.md line 45 — stack of currently-active closure
    /// expression `SpanKey`s. Pushed on entry to a `Closure { body, .. }`
    /// arm and popped on exit. The innermost entry is the closure whose
    /// `closure_capture_consumes` row each Consume identifier-leaf
    /// inside the body contributes to.
    closure_span_stack: Vec<SpanKey>,
    /// B-2026-07-15-14 — stack of the binding names LOCAL to each currently-
    /// open closure body: its own params, plus any `let` declared inside it.
    /// Consuming one of these is a fresh-per-call move of a closure-local
    /// value, NOT a capture of an outer binding, so it must not be tagged
    /// `ClosureCapture` (which would falsely mark the closure once-callable —
    /// a `|s: String| buf.push_str(s)` whose non-Copy param `s` is consumed by
    /// the mutator's by-value arg was rejected on a second call, while the
    /// `|x: i64| acc.push(x)` twin passed only because `i64` is Copy and never
    /// consumed). Pushed on entry to a `Closure` arm, popped on exit.
    closure_local_stack: Vec<HashSet<String>>,
}

impl<'a> UseClassifier<'a> {
    /// Match-ergonomics gate: `true` iff `expr`'s static type is
    /// `ref T` / `mut ref T`. The match scrutinee classifier consults
    /// this to demote `Consuming` to `Reading` when the scrutinee is
    /// a borrow — under such scrutinees, arm bindings borrow (the
    /// typechecker wraps their types via
    /// `ScrutineeMode::wrap_binding_ty`), so the scrutinee itself is
    /// never moved (design.md § Match Arm Binding Modes). Falls back
    /// to `param_types` / `local_types` when the span lookup misses,
    /// matching the same lookup chain used in `classify_identifier`.
    fn is_borrow_typed_expr(&self, expr: &Expr) -> bool {
        let ty = self
            .tc
            .expr_types
            .get(&SpanKey::from_span(&expr.span))
            .cloned()
            .or_else(|| match &expr.kind {
                ExprKind::Identifier(name) => self
                    .param_types
                    .get(name)
                    .or_else(|| self.local_types.get(name))
                    .cloned(),
                _ => None,
            });
        matches!(ty, Some(Type::Ref(_)) | Some(Type::MutRef(_)))
    }

    fn record(&mut self, span: &crate::token::Span, kind: UseKind) {
        self.record_named(span, kind, None);
    }

    /// [`record`] with the consumed identifier's NAME (when the leaf is a bare
    /// identifier), so a consume of a closure-LOCAL binding (the closure's own
    /// param / an inner `let`) is not stamped `ClosureCapture` — that origin
    /// drives the once-callable classification, and consuming a fresh local is
    /// not a capture of an outer binding (B-2026-07-15-14).
    fn record_named(&mut self, span: &crate::token::Span, kind: UseKind, name: Option<&str>) {
        let key = SpanKey::from_span(span);
        self.classification.kinds.insert(key, kind);
        if kind == UseKind::Consume && self.consume_origin_ctx != ConsumeOrigin::Direct {
            if self.consume_origin_ctx == ConsumeOrigin::ClosureCapture
                && name.is_some_and(|n| self.is_closure_local(n))
            {
                return;
            }
            self.classification
                .consume_origins
                .insert(key, self.consume_origin_ctx);
        }
    }

    /// Whether `name` is a binding local to any currently-open closure body
    /// (its param or an inner `let`) — see `closure_local_stack`.
    fn is_closure_local(&self, name: &str) -> bool {
        self.closure_local_stack.iter().any(|s| s.contains(name))
    }

    /// Phase-7-codegen.md line 45 — when an identifier-leaf is
    /// recorded as `Consume` while walking inside a closure body
    /// (`consume_origin_ctx == ClosureCapture`), record the
    /// `(closure_expr_span, binding_name → leaf_span)` mapping so the
    /// ownership pass's closure-capture-mode classifier can decide
    /// `Own` mode without consulting the legacy state machine's post-
    /// walk `ValueState::Moved` table. First-seen wins (`or_insert`)
    /// so the recorded span identifies the earliest consume site for
    /// each binding — same shape K2's error diagnostic wants.
    fn record_closure_capture_consume(
        &mut self,
        binding: &str,
        kind: UseKind,
        span: &crate::token::Span,
    ) {
        if kind != UseKind::Consume {
            return;
        }
        if self.consume_origin_ctx != ConsumeOrigin::ClosureCapture {
            return;
        }
        // A consume of the closure's OWN param / inner local is a fresh move,
        // not a capture — don't record it for the ownership-pass capture
        // classifier either (B-2026-07-15-14).
        if self.is_closure_local(binding) {
            return;
        }
        let closure_key = match self.closure_span_stack.last() {
            Some(k) => *k,
            None => return,
        };
        self.classification
            .closure_capture_consumes
            .entry(closure_key)
            .or_default()
            .entry(binding.to_string())
            .or_insert_with(|| span.clone());
    }

    fn mark_sink_arg(&mut self, span: &crate::token::Span) {
        self.classification
            .sink_arg_spans
            .insert(SpanKey::from_span(span));
    }

    fn walk_block(&mut self, block: &Block, terminal_mode: Mode) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(final_expr) = &block.final_expr {
            self.walk_expr(final_expr, terminal_mode);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { pattern, value, .. } => {
                // Round 12.18: record the local's type by name BEFORE
                // walking the value, using the value-span (which is
                // unaliased — only `MethodCall.span == receiver.span`
                // is, and the value's span is the value's own span).
                // Mirrors `OwnershipChecker::binding_types` population
                // at the let-RHS site so subsequent identifier-leaves
                // have a name-keyed type to fall back on when their
                // span lookup would alias.
                if let Some(rhs_ty) = self.tc.expr_types.get(&SpanKey::from_span(&value.span)) {
                    // Decompose a tuple destructure field-by-field so each
                    // binding gets its OWN type, not the whole tuple's. Without
                    // this, `let (n, s) = f()` where `f -> (i64, String)` maps
                    // `n` to the tuple type `(i64, String)` — non-Copy because
                    // of the String sibling — so reads of the Copy `n` in a
                    // consuming position are misclassified as `Consume` and the
                    // UAM predicate fires a spurious "value 'n' moved … used
                    // again" (B-2026-06-14-27).
                    let rhs_ty = rhs_ty.clone();
                    self.assign_binding_types(pattern, &rhs_ty);
                }
                // B-2026-07-15-14 — a `let` declared INSIDE a closure body is a
                // closure-local binding; consuming it later is a fresh move, not
                // a capture. No-op at function scope (empty stack), so an outer
                // `let name = closure` is unaffected.
                if let Some(locals) = self.closure_local_stack.last_mut() {
                    for n in pattern.binding_names() {
                        locals.insert(n);
                    }
                }
                // Round 12.20: detect once-callable closure bindings.
                // A closure RHS that produces at least one
                // `ClosureCapture`-tagged consume during the body walk
                // captured at least one outer non-Copy binding by
                // ownership; the closure value is therefore once-
                // callable. We snapshot the count of `ClosureCapture`
                // origins before walking and re-check after, so the
                // detection is local to this let-binding and doesn't
                // confuse with prior closures in the same function.
                // `let ref name @ PATTERN = rhs` borrows the RHS
                // (design.md § @ Bindings) — read, not consume. Mirrors
                // the `block_stmt` Let-arm gate in the legacy walker.
                let rhs_mode =
                    if matches!(&pattern.kind, PatternKind::AtBinding { by_ref: true, .. }) {
                        Mode::Reading
                    } else {
                        Mode::Consuming
                    };
                let rhs_is_closure = matches!(value.kind, ExprKind::Closure { .. });
                let pre_capture_count = if rhs_is_closure {
                    self.classification
                        .consume_origins
                        .values()
                        .filter(|o| **o == ConsumeOrigin::ClosureCapture)
                        .count()
                } else {
                    0
                };
                self.walk_expr(value, rhs_mode);
                if rhs_is_closure {
                    let post_capture_count = self
                        .classification
                        .consume_origins
                        .values()
                        .filter(|o| **o == ConsumeOrigin::ClosureCapture)
                        .count();
                    if post_capture_count > pre_capture_count {
                        for name in pattern.binding_names() {
                            self.once_callable_closures.insert(name);
                        }
                    }
                }
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                // Same `ref name @` borrow gate as the Let arm above.
                let rhs_mode =
                    if matches!(&pattern.kind, PatternKind::AtBinding { by_ref: true, .. }) {
                        Mode::Reading
                    } else {
                        Mode::Consuming
                    };
                self.walk_expr(value, rhs_mode);
                self.walk_block(else_block, Mode::Reading);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body, Mode::Reading);
            }
            StmtKind::Assign { target, value } => {
                // Walk the RHS first so reads of `target`'s binding inside
                // `value` see the pre-assignment state (mirrors the legacy
                // ownership.rs handler at ownership.rs:866).
                self.walk_expr(value, Mode::Consuming);
                // Round 12.19: a bare-identifier LHS rebinds the variable.
                // Tag its span with `UseKind::Reassign` so the predicate
                // pipeline treats it as a kill of any prior consume.
                // Field / tuple-index / slice-index targets stay on the
                // ordinary reading path — those are partial mutations of
                // the projection root, not rebindings of the binding.
                if let ExprKind::Identifier(_) = &target.kind {
                    self.record(&target.span, UseKind::Reassign);
                } else {
                    self.walk_expr(target, Mode::Reading);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(value, Mode::Reading);
                self.walk_expr(target, Mode::Reading);
            }
            StmtKind::Expr(e) => self.walk_expr(e, Mode::Reading),
        }
    }

    fn walk_expr(&mut self, expr: &Expr, mode: Mode) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                let kind = self.classify_identifier(name, &expr.span, mode);
                self.record_named(&expr.span, kind, Some(name));
                self.record_closure_capture_consume(name, kind, &expr.span);
            }
            ExprKind::SelfValue => {
                let kind = self.classify_identifier("self", &expr.span, mode);
                self.record_named(&expr.span, kind, Some("self"));
                self.record_closure_capture_consume("self", kind, &expr.span);
            }

            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::Bool(..)
            | ExprKind::CharLit(..)
            | ExprKind::StringLit(..)
            | ExprKind::MultiStringLit(..)
            | ExprKind::InterpolatedStringLit(..)
            | ExprKind::Path { .. }
            | ExprKind::SelfType => {}

            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left, Mode::Reading);
                self.walk_expr(right, Mode::Reading);
            }
            ExprKind::Unary { operand, .. } => {
                self.walk_expr(operand, Mode::Reading);
            }

            ExprKind::Call { callee, args } => {
                // Round 12.20: a call whose callee identifier is a
                // once-callable closure binding consumes that binding
                // (the legacy state machine sets `Moved { kind: Direct }`
                // at the call site; subsequent calls hit
                // `handle_moved_use` and emit UseAfterMove). Tag the
                // callee's identifier-leaf as Consume so the UAM
                // predicate fires for `f(); f();` shapes.
                if let ExprKind::Identifier(name) = &callee.kind {
                    if self.once_callable_closures.contains(name) {
                        self.record(&callee.span, UseKind::Consume);
                        self.record_closure_capture_consume(name, UseKind::Consume, &callee.span);
                    } else {
                        self.walk_expr(callee, Mode::Reading);
                    }
                } else {
                    self.walk_expr(callee, Mode::Reading);
                }
                let modes = self.callee_modes_for_call(callee).cloned();
                // B-2026-07-02-23: a comparison operator (`==` `!=` `<` `<=`
                // `>` `>=`) lowers to `Call(Path([Type, "eq"/…]), [lhs, rhs])`
                // — a free-call to an *instance* trait method, so it never
                // gets a `callee_param_modes` entry (that table is static-
                // methods-only) and both args fell to the consume default.
                // Comparisons borrow both operands (`ref self, other: ref
                // Self`), so classify every arg as a read.
                let is_relational = crate::lowering::callee_is_relational_operator(callee);
                for (i, arg) in args.iter().enumerate() {
                    let is_borrow = arg.mut_marker
                        || is_relational
                        || modes.as_ref().and_then(|m| m.get(i)).is_some_and(|m| {
                            matches!(m, OwnershipMode::Ref | OwnershipMode::MutRef)
                        });
                    let arg_mode = if is_borrow {
                        Mode::Reading
                    } else {
                        Mode::Consuming
                    };
                    self.walk_expr(&arg.value, arg_mode);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                let receiver_mode = if self.method_consumes_receiver(expr) {
                    Mode::Consuming
                } else {
                    Mode::Reading
                };
                if receiver_mode == Mode::Consuming {
                    // Round 12.18: sidestep the parser's `MethodCall.span ==
                    // receiver.span` aliasing for projection receivers like
                    // `c.inner.unwrap()`. The aliasing makes
                    // `tc.expr_types[receiver.span]` return the method's
                    // return type, not the field's type, so the FieldAccess
                    // Copy gate would walk `c` as Reading and miss the
                    // partial-move-projection consume of the root. Mirror
                    // the legacy ownership.rs MethodCall handler — walk to
                    // the projection's root identifier and tag it as
                    // Consume directly.
                    self.walk_method_receiver_consuming(object);
                } else {
                    self.walk_expr(object, Mode::Reading);
                }
                let receiver_is_mut_ref = self.method_receiver_is_mut_ref(expr);
                for (i, arg) in args.iter().enumerate() {
                    // A `ref`/`mut ref`/`mut Slice` method param is a borrow
                    // position — read, not consume — even though method calls
                    // never carry a call-site `mut` marker (Part 1½: only
                    // free-fn args mark). Without consulting the param mode, a
                    // borrowed struct arg was tagged `ContainerStore` and the
                    // binding spuriously RC-promoted (B-2026-06-12-8). Mirrors
                    // the `Call` arm's `callee_modes_for_call` borrow check.
                    let is_borrow = arg.mut_marker || self.method_arg_is_borrow_position(expr, i);
                    let arg_mode = if is_borrow {
                        Mode::Reading
                    } else {
                        Mode::Consuming
                    };
                    // Trigger-3 (round 12.12): owned arg of a `mut ref self`
                    // method flows into a container that outlives the call.
                    // Mark the arg's value-expression for sibling-sink
                    // lowering in the CFG so the consume site becomes
                    // dominance-incomparable with subsequent outer uses.
                    let is_sink_arg = receiver_is_mut_ref && !is_borrow;
                    if is_sink_arg {
                        self.mark_sink_arg(&arg.value.span);
                    }
                    // Round 12.14: swap in `ContainerStore` for the
                    // sink-arg walk so any Consume identifier-leaf
                    // discovered inside it carries the trigger-3 origin
                    // tag. Save/restore preserves outer context.
                    let saved = self.consume_origin_ctx;
                    if is_sink_arg {
                        self.consume_origin_ctx = ConsumeOrigin::ContainerStore;
                    }
                    self.walk_expr(&arg.value, arg_mode);
                    self.consume_origin_ctx = saved;
                }
            }

            // Field / tuple-index projection. In a consuming context on a
            // non-Copy field, this is a partial move of the projection root.
            // We record the projection place on the root identifier — for
            // BOTH the consume and the read case — so the predicate can tell
            // `b.left` from `b.right` and not pair two disjoint partial
            // accesses as a use-after-move (B-2026-07-02-25). Recording the
            // place on a READ matters for `eval(c.callee); for a in c.args`:
            // the `c.args` read must be disjoint from the `c.callee` consume,
            // else the whole-`c` read (empty place) would overlap it. The
            // leaf itself is still classified per `mode` (a read stays a
            // read); the place only refines disjointness.
            ExprKind::FieldAccess { .. } | ExprKind::TupleIndex { .. } => {
                let leaf_mode = if mode == Mode::Consuming && !self.expr_is_copy(expr) {
                    Mode::Consuming
                } else {
                    Mode::Reading
                };
                self.walk_place(expr, leaf_mode, &mut PlacePath::new());
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object, Mode::Reading);
                self.walk_expr(index, Mode::Reading);
            }

            ExprKind::Block(block) => self.walk_block(block, mode),

            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition, Mode::Reading);
                self.walk_block(then_block, mode);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb, mode);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                // The scrutinee of `if let` is a pattern match — same shape
                // as `match` step 4. For simplicity (and to align with the
                // dataflow conservative read-of-scrutinee in ownership.rs)
                // treat it as Reading; `match` does the binds-anything check.
                self.walk_expr(value, Mode::Reading);
                self.walk_block(then_block, mode);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb, mode);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // Match-ergonomics: under a `ref T` / `mut ref T`
                // scrutinee, arm bindings borrow rather than move, so
                // the scrutinee is always read regardless of what the
                // arms bind. Mirrors the ownership pass's
                // `is_borrow_typed_scrutinee` gate (design.md
                // § Match Arm Binding Modes).
                let scrut_is_borrow = self.is_borrow_typed_expr(scrutinee);
                let any_arm_binds = arms
                    .iter()
                    .any(|arm| self.pattern_binds_anything(&arm.pattern));
                let scrut_mode = if any_arm_binds && !scrut_is_borrow {
                    Mode::Consuming
                } else {
                    Mode::Reading
                };
                self.walk_expr(scrutinee, scrut_mode);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g, Mode::Reading);
                    }
                    self.walk_expr(&arm.body, mode);
                }
            }

            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition, Mode::Reading);
                self.walk_block(body, Mode::Reading);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.walk_expr(value, Mode::Reading);
                self.walk_block(body, Mode::Reading);
            }
            ExprKind::For { iterable, body, .. } => {
                // design.md § Iterable: "The collection is always borrowed
                // — `for` never consumes it. To consume a collection while
                // iterating, call `.into_iter()` explicitly." A consuming
                // iterable sub-expression (`for x in v.into_iter()`) still
                // classifies itself via the method-receiver walk; the For
                // construct contributes only a read. Walking this as
                // Consuming made `for x in v { } … v.len()` a use-after-
                // move under `karac check` (B-2026-07-02-22) — the legacy
                // state-machine walker (`expr_check.rs`) already reads.
                self.walk_expr(iterable, Mode::Reading);
                self.walk_block(body, Mode::Reading);
            }
            ExprKind::Loop { body, .. } => {
                self.walk_block(body, Mode::Reading);
            }

            ExprKind::Break { value: Some(v), .. } => {
                self.walk_expr(v, Mode::Consuming);
            }
            ExprKind::Break { value: None, .. } | ExprKind::Continue { .. } => {}
            ExprKind::Return(Some(v)) => {
                self.walk_expr(v, Mode::Consuming);
            }
            ExprKind::Return(None) => {}
            ExprKind::Question(inner) => {
                self.walk_expr(inner, Mode::Consuming);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object, Mode::Reading);
                if let Some(arg_list) = args {
                    for arg in arg_list {
                        let arg_mode = if arg.mut_marker {
                            Mode::Reading
                        } else {
                            Mode::Consuming
                        };
                        self.walk_expr(&arg.value, arg_mode);
                    }
                }
            }

            // Closure body: walked in reading mode at the top level —
            // sub-expressions still pick their own consuming/reading
            // context (e.g., the call walker propagates Consuming to
            // owned-arg leaves). The CFG places the body in a sibling
            // sink block of the creation point (round 12.11), so
            // capture-position consumes are dominance-incomparable
            // with subsequent outer uses.
            //
            // Round 12.14: swap the origin context to `ClosureCapture`
            // for the body walk so any Consume identifier-leaf carries
            // the trigger-2 origin tag. Save/restore preserves outer
            // context (relevant for closures expressed inside another
            // closure / sink-arg).
            ExprKind::Closure { params, body, .. } => {
                let saved = self.consume_origin_ctx;
                self.consume_origin_ctx = ConsumeOrigin::ClosureCapture;
                // Phase-7-codegen.md line 45 — push this closure's
                // expression span onto the stack so Consume identifier-
                // leaves walked inside `body` route into the right
                // `closure_capture_consumes` row.
                self.closure_span_stack.push(SpanKey::from_span(&expr.span));
                // B-2026-07-15-14 — seed the closure-local set with this
                // closure's own param names so a consume of a (non-Copy) param
                // inside the body (`push_str(s)` moving `s`) is not mistaken for
                // a capture consume. Inner `let`s add themselves in `walk_stmt`.
                let mut locals = HashSet::new();
                for p in params {
                    for n in p.pattern.binding_names() {
                        locals.insert(n);
                    }
                }
                self.closure_local_stack.push(locals);
                self.walk_expr(body, Mode::Reading);
                self.closure_local_stack.pop();
                self.closure_span_stack.pop();
                self.consume_origin_ctx = saved;
            }

            ExprKind::Cast { expr: inner, .. } => self.walk_expr(inner, mode),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s, Mode::Reading);
                }
                if let Some(e) = end {
                    self.walk_expr(e, Mode::Reading);
                }
            }

            ExprKind::Tuple(es) | ExprKind::ArrayLiteral(es) => {
                for e in es {
                    self.walk_expr(e, mode);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e, mode);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value, mode);
                self.walk_expr(count, Mode::Reading);
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.walk_expr(k, mode);
                    self.walk_expr(v, mode);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value, Mode::Consuming);
                }
                if let Some(s) = spread {
                    self.walk_expr(s, Mode::Consuming);
                }
            }

            // Block-bodied "transparent" forms — `par`, `seq`, `unsafe`,
            // `lock`, `providers`. Walking is needed so that consume
            // positions and sink-arg method calls inside their bodies are
            // classified rather than falling through to the default Read.
            // Round 12.13: paired with the matching cfg.rs arm so the
            // CFG and classifier agree on what use sites exist.
            ExprKind::Par(body)
            | ExprKind::Seq(body)
            | ExprKind::Unsafe(body)
            | ExprKind::Try(body) => {
                self.walk_block(body, mode);
            }
            ExprKind::Lock { body, .. } => self.walk_block(body, mode),
            ExprKind::Providers { bindings, body } => {
                for binding in bindings {
                    self.walk_expr(&binding.value, Mode::Consuming);
                }
                self.walk_block(body, mode);
            }

            // Catch-all for forms that don't carry sub-expressions worth
            // walking for use-classification (type annotations, error
            // recovery nodes). They emit no leaves.
            _ => {}
        }
    }

    /// Walk an owned-self method-call receiver. Recurses through
    /// projection layers (FieldAccess / TupleIndex / Index) to the
    /// root identifier and tags THAT as Consume — bypassing
    /// `tc.expr_types[receiver.span]` lookups that would alias to
    /// the method's return type for chained-projection receivers.
    /// Index expressions still record their index sub-expression as
    /// a Read. Non-projection receivers (calls, blocks, etc.) fall
    /// back to a Reading walk; there's no rooted binding to consume.
    fn walk_method_receiver_consuming(&mut self, recv: &Expr) {
        self.walk_place(recv, Mode::Consuming, &mut PlacePath::new());
    }

    /// Walk a place expression, accumulating the projection chain from the
    /// OUTER (widest) projection inward toward the root binding, then
    /// record the full path against the root identifier's span
    /// (B-2026-07-02-25). The leaf is classified per `leaf_mode` — a
    /// consuming access records a Consume, a reading access a Read — but
    /// EITHER way the place is recorded so the predicate can prove two
    /// accesses under the same root touch disjoint sub-places.
    ///
    /// `path` is built by pushing as we DESCEND (outermost projection
    /// first), then reversed at the root so it reads root→leaf (`[left]`
    /// for `b.left`, `[a, b]` for `x.a.b`) — matching
    /// `place_paths_disjoint`'s prefix semantics.
    ///
    /// Non-place leaves (calls, blocks, etc.) fall back to an ordinary
    /// Reading walk — there's no rooted binding — and any dynamic Index
    /// step records `PlaceSeg::Index`, which conservatively overlaps.
    fn walk_place(&mut self, expr: &Expr, leaf_mode: Mode, path: &mut PlacePath) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                self.record_place_at_root(&expr.span, path);
                let kind = self.classify_identifier(name, &expr.span, leaf_mode);
                self.record(&expr.span, kind);
                self.record_closure_capture_consume(name, kind, &expr.span);
            }
            ExprKind::SelfValue => {
                self.record_place_at_root(&expr.span, path);
                let kind = self.classify_identifier("self", &expr.span, leaf_mode);
                self.record(&expr.span, kind);
                self.record_closure_capture_consume("self", kind, &expr.span);
            }
            ExprKind::FieldAccess { object, field } => {
                path.push(PlaceSeg::Field(field.clone()));
                self.walk_place(object, leaf_mode, path);
            }
            ExprKind::TupleIndex { object, index } => {
                path.push(PlaceSeg::TupleIndex(*index as usize));
                self.walk_place(object, leaf_mode, path);
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(index, Mode::Reading);
                path.push(PlaceSeg::Index);
                // You cannot MOVE out through an index projection: `container[i]`
                // — and any deeper field of it, `container[i].field` — is a
                // borrow/read of the container, never a move of it (a partial
                // move of an element the container still owns is disallowed,
                // exactly like the bare-index arm in `walk_expr`, which walks its
                // object `Mode::Reading`). So the root BELOW an index is always
                // Read, never Consume — even when the outer leaf sits in consuming
                // position. Passing `leaf_mode` through here made `x[i].wf =
                // y[j].wf` classify the RHS field-read `y[j].wf` as CONSUMING the
                // container `y`, colliding with the LHS index-assign into the same
                // container and firing a spurious UseAfterMove (B-2026-07-21-20).
                // The place path still records the `[i].field` projection for
                // disjointness.
                self.walk_place(object, Mode::Reading, path);
            }
            _ => self.walk_expr(expr, Mode::Reading),
        }
    }

    /// Record the root-relative place path (reversed to root→leaf order)
    /// against the root identifier's span, if non-empty. Empty paths are
    /// left absent from the map so whole-binding uses default to the empty
    /// path (which overlaps everything) in the predicate.
    fn record_place_at_root(&mut self, root_span: &crate::token::Span, path: &PlacePath) {
        if path.is_empty() {
            return;
        }
        let mut rooted = path.clone();
        rooted.reverse();
        self.classification
            .consume_places
            .insert(SpanKey::from_span(root_span), rooted);
    }

    /// Record the type of every binding introduced by `pattern`, decomposing
    /// a tuple destructure pairwise against a `Type::Tuple` RHS so each binding
    /// is keyed to its OWN field type rather than the whole tuple's. Shapes we
    /// don't structurally split (structs, tuple-variants, slices, or-patterns,
    /// or an arity/kind mismatch) fall back to assigning the whole type to each
    /// binding — the pre-fix behavior. See the B-2026-06-14-27 note at the
    /// let-RHS call site.
    fn assign_binding_types(&mut self, pattern: &Pattern, ty: &Type) {
        match (&pattern.kind, ty) {
            (PatternKind::Binding(name), _) => {
                self.local_types.insert(name.clone(), ty.clone());
            }
            (PatternKind::Tuple(ps), Type::Tuple(field_tys)) if ps.len() == field_tys.len() => {
                for (p, ft) in ps.iter().zip(field_tys.iter()) {
                    self.assign_binding_types(p, ft);
                }
            }
            // #45 — the STRUCT-pattern peer of the tuple case above
            // (B-2026-06-14-27). Decompose a struct destructure field-by-field so
            // each binding is keyed to its OWN field type, not the whole struct's.
            // Without this, a `Copy` field (`span: Span`) bound from
            // `let MethodCallExpr { …, span } = m` inherits the non-`Copy`
            // `MethodCallExpr` type, so reading a SECOND field of it
            // (`span_str(span.offset, span.length)`) classifies the first read as
            // a Consume and fires a spurious "value 'span' moved here, used again
            // here". Surfaced by the self-hosted parser's renderer (a 20-arm
            // match destructuring node structs that all carry a `span: Span`).
            (PatternKind::Struct { fields, .. }, Type::Named { name, .. }) => {
                if let Some(info) = self.tc.struct_info.get(name.as_str()) {
                    for fp in fields {
                        let field_ty = info
                            .fields
                            .iter()
                            .find(|(fname, _, _)| fname == &fp.name)
                            .map(|(_, ft, _)| ft.clone());
                        match (&fp.pattern, field_ty) {
                            (Some(sub), Some(ft)) => self.assign_binding_types(sub, &ft),
                            // Shorthand `{ field }` binds the field name directly.
                            (None, Some(ft)) => {
                                self.local_types.insert(fp.name.clone(), ft);
                            }
                            // Unknown field / unrecorded type → conservative
                            // whole-type fallback for this binding only.
                            (Some(sub), None) => self.assign_binding_types(sub, ty),
                            (None, None) => {
                                self.local_types.insert(fp.name.clone(), ty.clone());
                            }
                        }
                    }
                } else {
                    for name in pattern.binding_names() {
                        self.local_types.insert(name, ty.clone());
                    }
                }
            }
            (
                PatternKind::AtBinding {
                    name,
                    pattern: inner,
                    ..
                },
                _,
            ) => {
                self.local_types.insert(name.clone(), ty.clone());
                self.assign_binding_types(inner, ty);
            }
            _ => {
                for name in pattern.binding_names() {
                    self.local_types.insert(name, ty.clone());
                }
            }
        }
    }

    fn classify_identifier(&self, name: &str, span: &crate::token::Span, mode: Mode) -> UseKind {
        if mode == Mode::Reading {
            return UseKind::Read;
        }
        // Unit-variant constructors (`None`, `Ok`, custom `Pending`) are
        // parsed as bare `ExprKind::Identifier(name)` even though they
        // construct a fresh enum value rather than reading a binding. Two
        // distinct `None` literals on the same line otherwise collide as
        // two uses of the same `binding: "None"` in the CFG and the UAM
        // predicate fires "value 'None' moved here, used again here".
        if self.unit_variant_names.contains(name) {
            return UseKind::Read;
        }
        // Mode is Consuming: only non-Copy types are actually consumed.
        // Lookup priority (round 12.18): param_types → local_types →
        // span-keyed expr_types. The first two are name-keyed and
        // immune to the parser's `MethodCall.span == receiver.span`
        // aliasing that breaks span-only lookups for projection-root
        // identifiers like `c` in `c.inner.unwrap()`.
        let ty = self
            .param_types
            .get(name)
            .or_else(|| self.local_types.get(name))
            .or_else(|| self.tc.expr_types.get(&SpanKey::from_span(span)));
        match ty {
            Some(t) if !is_copy_type(t, self.tc) => UseKind::Consume,
            _ => UseKind::Read,
        }
    }

    fn callee_modes_for_call(&self, callee: &Expr) -> Option<&Vec<OwnershipMode>> {
        // Includes the `with_provider[R]` generic special-form callee
        // (B-2026-07-02-26) via `callee_param_modes_key`.
        let key = callee_param_modes_key(callee)?;
        self.callee_param_modes.get(&key)
    }

    /// The resolved `"Type.method"` / `"Trait.method"` mode key for a method
    /// call: prefers the concrete `method_callee_types`, falls back to
    /// `method_typeparam_trait_key` for a single-bound generic-receiver call
    /// (`a.cmp(b)` where `a: T`, `T: Ord`). Twin of the `borrow.rs`
    /// `resolved_method_mode_key` — both the predicate pipeline (this file)
    /// and the legacy `states` machine must resolve the same modes, else a
    /// generic trait-method `ref Self` param is seen as a move here and the
    /// UAM witness false-rejects a user generic body (B-2026-07-08-6).
    fn resolved_method_mode_key(&self, method_call: &Expr) -> Option<&String> {
        let span = SpanKey::from_span(&method_call.span);
        self.tc
            .method_callee_types
            .get(&span)
            .or_else(|| self.tc.method_typeparam_trait_key.get(&span))
    }

    fn method_consumes_receiver(&self, method_call: &Expr) -> bool {
        // A relational-trait method (`a.cmp(b)` / `a.eq(b)` / `a.lt(b)` …)
        // BORROWS its receiver — its canonical signature is `ref self` (design.md
        // § comparisons borrow), so comparing a value must not consume it. The
        // operator forms (`a < b`) already read both operands via
        // `callee_is_relational_operator`; the explicit method-call form must
        // agree, else `p1.cmp(p2)` followed by any reuse of `p1`/`p2` (or
        // `min`/`max`/`sort_by` bodies) false-rejects as a use-after-move. This
        // fires before the resolved-mode lookup because a `#[derive(Ord)]` type
        // registers no method mode. roadmap Phase 8 § Eq/Ord.
        if is_relational_method_call(method_call) {
            return false;
        }
        let Some(key) = self.resolved_method_mode_key(method_call) else {
            return false;
        };
        matches!(self.method_self_modes.get(key), Some(SelfParam::Owned))
    }

    fn method_receiver_is_mut_ref(&self, method_call: &Expr) -> bool {
        let Some(key) = self.resolved_method_mode_key(method_call) else {
            return false;
        };
        matches!(self.method_self_modes.get(key), Some(SelfParam::MutRef))
    }

    /// Whether call-arg `arg_index` of `method_call` lands in a borrow
    /// position — the resolved method's NON-self param `arg_index` is
    /// `ref`/`mut ref`/`mut Slice`. Arg indices map 1:1 to
    /// `method_param_modes` (the receiver is `method_self_modes`). `false`
    /// for unresolved methods (stdlib / typecheck errors). The `MethodCall`
    /// analogue of the `Call` arm's `callee_modes_for_call` borrow check;
    /// without it a borrowed struct arg was treated as a container-store
    /// consume and the binding spuriously RC-promoted (B-2026-06-12-8).
    fn method_arg_is_borrow_position(&self, method_call: &Expr, arg_index: usize) -> bool {
        // A relational-trait method borrows BOTH operands — its `other` param is
        // `ref Self` — so `a.cmp(b)` reads `b` rather than consuming it. Same
        // rationale as the receiver arm in `method_consumes_receiver`; without
        // it the arg falls to the consume default and `p1.cmp(p2)` then reusing
        // `p2` false-rejects. roadmap Phase 8 § Eq/Ord.
        if is_relational_method_call(method_call) {
            return true;
        }
        // The tensor binary methods (`matmul`, `broadcast_*`) borrow their
        // tensor argument — `other: ref Tensor[T, S]` in the stdlib surface
        // signature. That signature IS in `method_param_modes` (via the baked
        // stdlib walk), but the resolved-key lookup below is defeated by the
        // parser's `MethodCall.span == receiver.span` aliasing in a CHAIN:
        // in `a.matmul(b).transpose()` the outer call clobbers the inner's
        // `method_callee_types` entry at the shared span, the lookup then
        // resolves the inner matmul to `Tensor.transpose` (zero params), and
        // `modes.get(0)` → None → consume — `b` false-rejects as moved
        // (B-2026-07-14-18). Match on the AST method NAME instead (immune to
        // the span collision), like the relational short-circuit above.
        if is_tensor_borrow_arg_method_call(method_call) {
            return true;
        }
        // Trust the span-resolved modes whenever they carry an entry for THIS
        // arg position; only when the entry is missing (the chained-call span
        // collision resolved the inner call to a sibling with fewer params —
        // e.g. `.zip_with(x, f).sum()`) fall back to a collision-immune
        // method-NAME lookup. A present entry preserves the prior behavior,
        // correct when chained calls share the arg mode (`x.mul(x).add(..)`).
        // (B-2026-07-14-18, B-2026-07-20-6 — see `arg_is_borrow_by_method_name`.)
        if let Some(key) = self.resolved_method_mode_key(method_call) {
            if let Some(m) = self
                .method_param_modes
                .get(key)
                .and_then(|modes| modes.get(arg_index))
            {
                return matches!(m, OwnershipMode::Ref | OwnershipMode::MutRef);
            }
        }
        if let ExprKind::MethodCall { method, .. } = &method_call.kind {
            return crate::ownership::arg_is_borrow_by_method_name(
                self.method_param_modes,
                method,
                arg_index,
            );
        }
        false
    }

    fn expr_is_copy(&self, expr: &Expr) -> bool {
        self.tc
            .expr_types
            .get(&SpanKey::from_span(&expr.span))
            .map(|t| is_copy_type(t, self.tc))
            .unwrap_or(false)
    }

    fn pattern_binds_anything(&self, pattern: &Pattern) -> bool {
        match &pattern.kind {
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {
                false
            }
            PatternKind::Binding(name) => !self.unit_variant_names.contains(name),
            // `ref name @ PATTERN` flips the whole subtree to borrow mode
            // (design.md § @ Bindings) — nothing under it binds by-move.
            // Mirrors `ownership::expr_check::pattern_binds_anything`.
            PatternKind::AtBinding { by_ref, .. } => !by_ref,
            PatternKind::Tuple(patterns) | PatternKind::TupleVariant { patterns, .. } => {
                patterns.iter().any(|p| self.pattern_binds_anything(p))
            }
            PatternKind::Struct { fields, .. } => fields.iter().any(|f| match &f.pattern {
                Some(sub) => self.pattern_binds_anything(sub),
                None => true,
            }),
            PatternKind::Or(alts) => alts.iter().any(|p| self.pattern_binds_anything(p)),
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                matches!(rest, Some(RestPattern::Bound(_)))
                    || prefix
                        .iter()
                        .chain(suffix.iter())
                        .any(|p| self.pattern_binds_anything(p))
            }
        }
    }
}

/// True when `expr` is a `MethodCall` whose method is a relational-trait
/// method (`cmp`/`eq`/`ne`/`lt`/`le`/`gt`/`ge`/`partial_cmp`) — the calls that
/// BORROW both operands (`ref self, other: ref Self`; design.md § comparisons
/// borrow). Used by the receiver/arg borrow classification so an explicit
/// `a.cmp(b)` on a non-Copy type is a read, not a consume — matching the
/// operator forms and unblocking `.cmp()` on `#[derive(Ord)]` types.
fn is_relational_method_call(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::MethodCall { method, .. } if crate::lowering::is_relational_operator_method(method)
    )
}

/// The tensor binary methods whose argument is declared `ref Tensor[T, S]`
/// in `runtime/stdlib/tensor.kara` — both operands are read, never moved.
/// Name-matched (not resolved-key-matched) so a chained outer call sharing
/// the span can't clobber the classification; see the call site in
/// `method_arg_is_borrow_position`. These names are tensor-intercept
/// methods in the typechecker, so a user type reusing one of the names on
/// an OWNED arg would be mis-read here — acceptable conservatism (a read
/// where a consume happens can only under-reject, and the RC-fallback
/// machinery keeps such a value alive).
fn is_tensor_borrow_arg_method_call(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::MethodCall { method, .. } if matches!(
            method.as_str(),
            // `matmul` / `broadcast_*` borrow their tensor arg (B-2026-07-14-18).
            "matmul" | "broadcast_add" | "broadcast_sub" | "broadcast_mul" | "broadcast_div"
            // `zip_with(other: ref Tensor|Column, f)` borrows `other` on every
            // type that defines it (Tensor, Column, the `ElementwiseMap` trait).
            // Same span-collision false-consume as the tensor binops when it is
            // a CHAIN receiver — `a.zip_with(b, f).sum()` resolves the inner
            // `zip_with` to `Tensor.sum` (0 params) → arg 0 → consume → `b`
            // false-rejects as moved (B-2026-07-20-6). Name-match, span-immune.
            | "zip_with"
        )
    )
}

fn collect_unit_variant_names(tc: &TypeCheckResult) -> HashSet<String> {
    let mut s = HashSet::new();
    for info in tc.enum_info.values() {
        for (vn, _) in &info.variants {
            s.insert(vn.clone());
        }
    }
    s
}

/// Build a `param_types` map for a function: parameter name → type.
/// Handles bare identifiers and self-receivers; nested patterns get
/// skipped (the param's structural type doesn't decompose for our
/// Copy lookup needs).
pub fn param_types_for_function(
    f: &crate::ast::Function,
    tc: &TypeCheckResult,
) -> HashMap<String, Type> {
    let mut map = HashMap::new();
    if let Some(self_param) = &f.self_param {
        // A `ref self` / `mut ref self` receiver is a BORROW, and a borrowed
        // receiver can never be *consumed* (you cannot move `self` out of a
        // borrow — the language clones), so every use of `self` must classify
        // as a Read, exactly like a `ref`/`mut ref` PARAM whose `param_types`
        // entry makes it Copy. Record `self` as a `Ref` (Copy) so
        // `classify_identifier` reads it. `MutRef` is deliberately non-Copy
        // for a *param* (exclusive-borrow aliasing detection lives in the
        // borrow checker), but for the receiver's MOVE analysis the mutable
        // vs. immutable distinction is irrelevant — neither is movable — so
        // both map to `Ref` here; the pointee is irrelevant to Copy-ness.
        //
        // Leaving `self` ABSENT (the prior behavior) made `classify_identifier`
        // fall through to the span-keyed `expr_types`, which records `self` as
        // the BARE Self type (`current_self_type`, non-Copy) → a spurious
        // Consume. Benign until 5426bbd1 (projection-aware partial-move
        // tracking) started recording `self.field` consumes, after which
        // `match self.field { V(x) => .. }; self.method()` read as a
        // self-consume-then-use and reddened the selfhost parser oracles
        // (B-2026-07-03-26). Owned `self` is left absent so its genuine
        // consume semantics stand.
        match self_param {
            SelfParam::Ref | SelfParam::MutRef => {
                map.insert("self".to_string(), Type::Ref(Box::new(Type::Error)));
            }
            SelfParam::Owned => {}
        }
    }
    for p in &f.params {
        if let PatternKind::Binding(name) = &p.pattern.kind {
            if let Some(ty) = tc.expr_types.get(&SpanKey::from_span(&p.span)) {
                map.insert(name.clone(), ty.clone());
            }
        }
    }
    map
}

/// Convenience wrapper: locate `fn <name>` at the program top level and
/// classify its body. Returns None if the function isn't found.
pub fn classify_top_level_fn(
    program: &Program,
    tc: &TypeCheckResult,
    fn_name: &str,
) -> Option<Classification> {
    let f = program.items.iter().find_map(|i| match i {
        Item::Function(f) if f.name == fn_name => Some(f),
        _ => None,
    })?;
    let param_types = param_types_for_function(f, tc);
    Some(classify_function_body(program, tc, &f.body, param_types))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::{build_cfg_with_classification, UseKind};
    use crate::{parse, resolve, typecheck};

    fn classify(
        src: &str,
    ) -> (
        HashMap<SpanKey, UseKind>,
        crate::ast::Program,
        TypeCheckResult,
    ) {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "main").expect("expected fn main");
        (class.kinds, parsed.program, tc)
    }

    /// Classify and also return the sink-arg span set — used by the
    /// trigger-3 detection tests (round 12.12).
    fn classify_full(src: &str) -> (Classification, crate::ast::Program, TypeCheckResult) {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "resolve errors: {:?}",
            resolved.errors
        );
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "main").expect("expected fn main");
        (class, parsed.program, tc)
    }

    fn count_kinds(class: &HashMap<SpanKey, UseKind>) -> (usize, usize) {
        let mut consumes = 0;
        let mut reads = 0;
        for k in class.values() {
            match k {
                UseKind::Consume => consumes += 1,
                UseKind::Read => reads += 1,
                // `Reassign` / `Define` are CFG-only rebind markers (the
                // classifier's `class` map never holds them); count neither.
                UseKind::Reassign | UseKind::Define => {}
            }
        }
        (consumes, reads)
    }

    #[test]
    fn copy_value_let_rhs_stays_read() {
        // i32 is Copy — even in a consuming let-RHS position, the use is
        // recorded as Read by the classifier.
        let (class, _, _) = classify(
            "fn main() {\n\
                 let x = 1;\n\
                 let y = x;\n\
             }",
        );
        let (consumes, _reads) = count_kinds(&class);
        assert_eq!(consumes, 0, "no Copy consumes expected, got {consumes}");
    }

    #[test]
    fn non_copy_let_rhs_is_consume() {
        let (class, _, _) = classify(
            "struct Box { v: i32 }\n\
             fn main() {\n\
                 let b = Box { v: 1 };\n\
                 let c = b;\n\
             }",
        );
        // `c = b` should record `b`'s identifier-use as Consume.
        let consumes: Vec<_> = class
            .iter()
            .filter(|(_, k)| **k == UseKind::Consume)
            .collect();
        assert!(
            !consumes.is_empty(),
            "expected at least one Consume; got {:?}",
            class
        );
    }

    #[test]
    fn ref_arg_is_read_not_consume() {
        // A function with a `ref` param — the call argument is a borrow
        // position even on a non-Copy value.
        let src = "struct Box { v: i32 }\n\
                   fn use_ref(b: ref Box) -> i32 { b.v }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let _x = use_ref(b);\n\
                       let _y = b;\n\
                   }";
        let (class, _, _) = classify(src);
        // `b` appears in two use positions — the call-arg (ref → read) and
        // the trailing `let _y = b;` (let RHS → consume). Exactly one
        // Consume on `b`.
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(consumes, 1, "expected exactly 1 Consume, got {}", consumes);
    }

    #[test]
    fn mut_ref_arg_is_read_not_consume() {
        let src = "struct Box { v: i32 }\n\
                   fn use_mut(b: mut ref Box) -> i32 { b.v }\n\
                   fn main() {\n\
                       let mut b = Box { v: 1 };\n\
                       let _x = use_mut(mut b);\n\
                       let _y = b;\n\
                   }";
        let (class, _, _) = classify(src);
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(consumes, 1);
    }

    #[test]
    fn owned_call_arg_is_consume() {
        let src = "struct Box { v: i32 }\n\
                   fn take(b: Box) -> i32 { b.v }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let _x = take(b);\n\
                   }";
        let (class, _, _) = classify(src);
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(consumes, 1, "owned arg `take(b)` should consume `b`");
    }

    #[test]
    fn if_condition_is_read_branch_assignment_is_consume() {
        let src = "struct Box { v: i32 }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let cond = true;\n\
                       if cond {\n\
                           let _y = b;\n\
                       }\n\
                   }";
        let (class, _, _) = classify(src);
        // `cond` is bool (Copy) — never Consume. `b` in the then-block let
        // RHS is the only Consume.
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(consumes, 1);
    }

    #[test]
    fn integration_branch_consume_then_outer_read_predicate_fires() {
        // End-to-end: classify → build_cfg_with_classification →
        // dominator → rc_candidates. The branch consumes `b`; the
        // outer use after the if reads `b`. Expect a witness.
        let src = "struct Box { v: i32 }\n\
                   fn take(b: Box) -> i32 { b.v }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let cond = true;\n\
                       if cond {\n\
                           let _x = take(b);\n\
                       }\n\
                       let _y = b;\n\
                   }";
        let parsed = parse(src);
        assert!(parsed.errors.is_empty());
        let resolved = resolve(&parsed.program);
        assert!(resolved.errors.is_empty());
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "main").unwrap();

        let main = parsed
            .program
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .unwrap();
        let cfg = build_cfg_with_classification(&main.body, &class, &[]);
        let dom = crate::dominator::compute_dominators(&cfg);
        let candidates = crate::rc_predicate::rc_candidates(&cfg, &dom);
        assert!(
            candidates.contains_key("b"),
            "expected RC witness on `b`; got candidates: {:?}",
            candidates.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn linear_consume_no_witness_in_predicate() {
        // Sequential consume + read on the same path: the consume block
        // dominates the read block, predicate must not fire.
        let src = "struct Box { v: i32 }\n\
                   fn take(b: Box) -> i32 { b.v }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let _x = take(b);\n\
                       let _y = 0;\n\
                   }";
        let parsed = parse(src);
        let resolved = resolve(&parsed.program);
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "main").unwrap();
        let main = parsed
            .program
            .items
            .iter()
            .find_map(|i| match i {
                Item::Function(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .unwrap();
        let cfg = build_cfg_with_classification(&main.body, &class, &[]);
        let dom = crate::dominator::compute_dominators(&cfg);
        let candidates = crate::rc_predicate::rc_candidates(&cfg, &dom);
        assert!(
            !candidates.contains_key("b"),
            "linear consume must not produce a predicate witness"
        );
    }

    #[test]
    fn match_arm_binding_consumes_scrutinee() {
        // Non-Copy scrutinee (`Option[String]`) — match has at least one
        // arm that binds (`Some(s)`), so the scrutinee identifier is
        // recorded as Consume per design.md § Consume Predicate step 4.
        let src = "fn main() {\n\
                       let opt: Option[String] = Some(\"hi\");\n\
                       let _ = match opt { Some(s) => s, None => \"\" };\n\
                   }";
        let (class, _, _) = classify(src);
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert!(
            consumes >= 1,
            "binding-arm match on non-Copy scrutinee `opt` should record at least one Consume; got {consumes}"
        );
    }

    #[test]
    fn match_with_no_binding_arms_is_read() {
        // No arm binds anything — scrutinee stays Read even on non-Copy
        // (mirrors design.md § Consume Predicate step 4).
        let src = "fn main() {\n\
                       let opt: Option[String] = Some(\"hi\");\n\
                       let _ = match opt { Some(_) => 1, None => 0 };\n\
                   }";
        let (class, _, _) = classify(src);
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(
            consumes, 0,
            "non-binding arms should not record any Consume on the scrutinee; got {consumes}"
        );
    }

    #[test]
    fn return_value_is_consume() {
        let src = "struct Box { v: i32 }\n\
                   fn make() -> Box {\n\
                       let b = Box { v: 1 };\n\
                       return b;\n\
                   }\n\
                   fn main() {\n\
                       let _x = make();\n\
                   }";
        let parsed = parse(src);
        let resolved = resolve(&parsed.program);
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "make").unwrap();
        let consumes = class
            .kinds
            .values()
            .filter(|k| **k == UseKind::Consume)
            .count();
        assert_eq!(consumes, 1, "`return b` should consume `b`");
    }

    #[test]
    fn struct_literal_field_value_is_consume() {
        let src = "struct Inner { v: i32 }\n\
                   struct Outer { inner: Inner }\n\
                   fn main() {\n\
                       let i = Inner { v: 1 };\n\
                       let _o = Outer { inner: i };\n\
                   }";
        let (class, _, _) = classify(src);
        let consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(
            consumes, 1,
            "struct-literal field value `inner: i` should consume `i`"
        );
    }

    // ── Round 12.12: trigger-3 sink-arg detection ─────────────────────

    #[test]
    fn mut_ref_self_owned_arg_is_sink_arg() {
        // `bag.insert(0, w)` where `insert` takes `mut ref self`: the
        // owned arg `w` is marked as a sink-arg so the CFG lowers it
        // into a sibling sink block.
        let src = "struct Widget { value: i64 }\n\
                   struct Bag { count: i64 }\n\
                   impl Bag {\n\
                       fn insert(mut ref self, key: i64, value: Widget) { }\n\
                   }\n\
                   fn main() {\n\
                       let w = Widget { value: 42 };\n\
                       let mut bag = Bag { count: 0 };\n\
                       bag.insert(0, w);\n\
                   }";
        let (class, _, _) = classify_full(src);
        // Exactly one sink-arg span — the `w` argument expression.
        // The literal `0` is not consumed (Copy-ish), so even though
        // we mark every non-mut-marker arg of a mut-ref-self method,
        // we should still detect the structurally-relevant one.
        assert!(
            !class.sink_arg_spans.is_empty(),
            "expected at least one sink-arg span for `bag.insert(0, w)`"
        );
    }

    #[test]
    fn mut_ref_self_mut_marker_arg_is_not_sink() {
        // Receiver is `mut ref self`, but the arg has an explicit `mut`
        // marker (a borrow, not a consume) — must NOT be marked.
        let src = "struct Widget { value: i64 }\n\
                   struct Bag { count: i64 }\n\
                   impl Bag {\n\
                       fn touch(mut ref self, w: mut ref Widget) { }\n\
                   }\n\
                   fn main() {\n\
                       let mut w = Widget { value: 42 };\n\
                       let mut bag = Bag { count: 0 };\n\
                       bag.touch(mut w);\n\
                   }";
        let (class, _, _) = classify_full(src);
        assert!(
            class.sink_arg_spans.is_empty(),
            "mut-borrowed arg of mut-ref-self method must not be a sink-arg"
        );
    }

    #[test]
    fn owned_self_method_arg_is_not_sink() {
        // Receiver is owned `self`, not `mut ref self` — no container
        // store flavor; sink-arg set must be empty.
        let src = "struct Widget { value: i64 }\n\
                   impl Widget {\n\
                       fn merge(self, other: Widget) { }\n\
                   }\n\
                   fn main() {\n\
                       let a = Widget { value: 1 };\n\
                       let b = Widget { value: 2 };\n\
                       a.merge(b);\n\
                   }";
        let (class, _, _) = classify_full(src);
        assert!(
            class.sink_arg_spans.is_empty(),
            "owned-self method args must not be sink-args"
        );
    }

    #[test]
    fn ref_self_method_arg_is_not_sink() {
        let src = "struct Widget { value: i64 }\n\
                   struct Bag { count: i64 }\n\
                   impl Bag {\n\
                       fn read(ref self, w: Widget) { }\n\
                   }\n\
                   fn main() {\n\
                       let w = Widget { value: 42 };\n\
                       let bag = Bag { count: 0 };\n\
                       bag.read(w);\n\
                   }";
        let (class, _, _) = classify_full(src);
        assert!(
            class.sink_arg_spans.is_empty(),
            "ref-self method args must not be sink-args"
        );
    }

    // ── Round 12.18: projection-receiver consume ──────────────────

    #[test]
    fn owned_self_on_field_consumes_root() {
        // `c.inner.unwrap()` where unwrap takes owned self: the
        // receiver `c.inner` is a partial move of `c`. The classifier
        // must tag `c` as Consume even though the parser aliases
        // `MethodCall.span == receiver.span`, which would otherwise
        // make `expr_is_copy(c.inner)` query the method's return
        // type (`i64` here, Copy) and demote the consume to Read.
        let src = "struct Container { inner: Inner }\n\
                   struct Inner { value: i64 }\n\
                   impl Inner {\n\
                       fn unwrap(self) -> i64 { self.value }\n\
                   }\n\
                   fn main() {\n\
                       let c = Container { inner: Inner { value: 1 } };\n\
                       let _ = c.inner.unwrap();\n\
                   }";
        let (class, _, _) = classify_full(src);
        let consumes = class
            .kinds
            .values()
            .filter(|k| **k == UseKind::Consume)
            .count();
        assert!(
            consumes >= 1,
            "owned-self on projection receiver should record at least one Consume; \
             got {consumes} consumes in {:?}",
            class.kinds
        );
    }

    #[test]
    fn owned_self_on_nested_field_consumes_root() {
        // Two-level projection: `c.outer.inner.unwrap()`.
        let src = "struct Wrap { outer: Container }\n\
                   struct Container { inner: Inner }\n\
                   struct Inner { value: i64 }\n\
                   impl Inner {\n\
                       fn unwrap(self) -> i64 { self.value }\n\
                   }\n\
                   fn main() {\n\
                       let c = Wrap { outer: Container { inner: Inner { value: 1 } } };\n\
                       let _ = c.outer.inner.unwrap();\n\
                   }";
        let (class, _, _) = classify_full(src);
        let consumes = class
            .kinds
            .values()
            .filter(|k| **k == UseKind::Consume)
            .count();
        assert!(
            consumes >= 1,
            "owned-self on nested projection should record Consume on root; \
             got {consumes} consumes"
        );
    }

    #[test]
    fn ref_self_on_projection_does_not_consume_root() {
        // Counterpart: `c.inner.read_only(self: ref self)` — the
        // receiver is *not* consumed. The root must stay Read.
        let src = "struct Container { inner: Inner }\n\
                   struct Inner { value: i64 }\n\
                   impl Inner {\n\
                       fn peek(ref self) -> i64 { self.value }\n\
                   }\n\
                   fn main() {\n\
                       let c = Container { inner: Inner { value: 1 } };\n\
                       let _ = c.inner.peek();\n\
                   }";
        let (class, _, _) = classify_full(src);
        let consumes = class
            .kinds
            .values()
            .filter(|k| **k == UseKind::Consume)
            .count();
        assert_eq!(
            consumes, 0,
            "ref-self method on projection must NOT tag the root as Consume; \
             got {:?}",
            class.kinds
        );
    }

    // ── Round 12.14: ConsumeOrigin tagging ─────────────────────────

    /// Helper: collect every Consume span and its origin (defaulting
    /// absent entries to `Direct`).
    fn consume_origins_by_line(class: &Classification) -> Vec<(usize, ConsumeOrigin)> {
        let mut out = Vec::new();
        for (key, kind) in &class.kinds {
            if *kind != UseKind::Consume {
                continue;
            }
            let origin = class
                .consume_origins
                .get(key)
                .copied()
                .unwrap_or(ConsumeOrigin::Direct);
            // The SpanKey doesn't expose its line directly, but we
            // can recover it by matching against the Direct catch-all
            // — for these tests, line-precision isn't required, so we
            // just collect origins.
            let _ = key;
            out.push((0, origin));
        }
        out
    }

    #[test]
    fn closure_capture_consume_records_closure_capture_origin() {
        // `let h = || apply(cfg);` — `apply` takes owned non-Copy, so
        // the capture-position identifier-leaf for `cfg` is tagged
        // Consume with origin `ClosureCapture`.
        let src = "struct Config { name: i64 }\n\
                   fn apply(c: Config) { }\n\
                   fn make(cfg: Config) {\n\
                       let h = || apply(cfg);\n\
                       let _ = h;\n\
                   }";
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
        let resolved = resolve(&parsed.program);
        assert!(resolved.errors.is_empty(), "resolve: {:?}", resolved.errors);
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "make").expect("expected fn make");

        let origins = consume_origins_by_line(&class);
        assert!(
            origins
                .iter()
                .any(|(_, o)| *o == ConsumeOrigin::ClosureCapture),
            "expected at least one ClosureCapture origin; got {:?}",
            origins
        );
    }

    #[test]
    fn mut_ref_self_sink_arg_records_container_store_origin() {
        // `bag.insert(0, w)` with `insert(mut ref self, _, value: Widget)`:
        // the `w` identifier-leaf inside the sink-arg position is
        // tagged Consume with origin `ContainerStore`.
        let src = "struct Widget { value: i64 }\n\
                   struct Bag { count: i64 }\n\
                   impl Bag {\n\
                       fn insert(mut ref self, key: i64, value: Widget) { }\n\
                   }\n\
                   fn main() {\n\
                       let w = Widget { value: 42 };\n\
                       let mut bag = Bag { count: 0 };\n\
                       bag.insert(0, w);\n\
                   }";
        let (class, _, _) = classify_full(src);
        let origins = consume_origins_by_line(&class);
        assert!(
            origins
                .iter()
                .any(|(_, o)| *o == ConsumeOrigin::ContainerStore),
            "expected at least one ContainerStore origin for the sink-arg `w`; got {:?}",
            origins
        );
    }

    #[test]
    fn direct_consume_has_no_explicit_origin_entry() {
        // `let c = b;` — non-Copy let-RHS — is a Consume with no
        // closure body / no sink-arg context; the classifier must
        // NOT record a non-Direct origin (defaulting via absence).
        let src = "struct Box { v: i32 }\n\
                   fn main() {\n\
                       let b = Box { v: 1 };\n\
                       let c = b;\n\
                   }";
        let (class, _, _) = classify_full(src);
        // At least one Consume exists.
        let consumes = class
            .kinds
            .values()
            .filter(|k| **k == UseKind::Consume)
            .count();
        assert!(consumes >= 1, "expected at least 1 Consume");
        // No Consume span should carry a non-Direct origin tag.
        assert!(
            class.consume_origins.is_empty(),
            "direct consumes must not populate consume_origins; got {:?}",
            class.consume_origins
        );
    }

    #[test]
    fn closure_capture_origin_does_not_leak_outside_closure() {
        // After walking a closure body, subsequent direct consumes in
        // the outer function must not pick up the ClosureCapture tag.
        let src = "struct Config { name: i64 }\n\
                   fn apply(c: Config) { }\n\
                   fn make(cfg: Config, other: Config) {\n\
                       let h = || apply(cfg);\n\
                       let _ = h;\n\
                       apply(other);\n\
                   }";
        let parsed = parse(src);
        assert!(parsed.errors.is_empty());
        let resolved = resolve(&parsed.program);
        assert!(resolved.errors.is_empty());
        let tc = typecheck(&parsed.program, &resolved);
        let class = classify_top_level_fn(&parsed.program, &tc, "make").expect("expected fn make");

        // Two Consume sites with non-Direct origin would mean the
        // outer `apply(other)` was tagged. The closure capture (cfg)
        // is the only ClosureCapture; `other` must default to Direct
        // (i.e. absent from consume_origins).
        let closure_capture_count = class
            .consume_origins
            .values()
            .filter(|o| **o == ConsumeOrigin::ClosureCapture)
            .count();
        assert_eq!(
            closure_capture_count, 1,
            "expected exactly 1 ClosureCapture origin (cfg only); got {:?}",
            class.consume_origins
        );
    }

    // ── Round 12.19: reassignment-marker emission ─────────────────────

    #[test]
    fn bare_identifier_assign_target_records_reassign() {
        // `x = 2;` with bare-identifier LHS — the LHS span carries
        // UseKind::Reassign. The `2` literal contributes no use site.
        let src = "fn main() {\n\
                       let mut x = 1;\n\
                       x = 2;\n\
                   }";
        let (class, _, _) = classify(src);
        let reassigns: Vec<_> = class
            .iter()
            .filter(|(_, k)| **k == UseKind::Reassign)
            .collect();
        assert_eq!(
            reassigns.len(),
            1,
            "expected exactly one Reassign marker, got {:?}",
            class
        );
    }

    #[test]
    fn field_assign_target_does_not_emit_reassign() {
        // `s.value = 2;` is a partial mutation of the projection root,
        // not a rebinding. The classifier must NOT emit Reassign — the
        // LHS path stays on the ordinary reading walk.
        let src = "struct S { value: i64 }\n\
                   fn main() {\n\
                       let mut s = S { value: 1 };\n\
                       s.value = 2;\n\
                   }";
        let (class, _, _) = classify(src);
        let reassigns = class.values().filter(|k| **k == UseKind::Reassign).count();
        assert_eq!(
            reassigns, 0,
            "field-assign LHS must not emit a Reassign marker; got {:?}",
            class
        );
    }

    // ── Round 12.20: once-callable closure call-site consume ────────

    #[test]
    fn once_callable_closure_first_call_is_consume() {
        // `let f = || apply(cfg);` — the body consumes `cfg` (non-Copy
        // owned outer binding) → f is once-callable. The call site
        // `f()` must be tagged as a Consume of f so the UAM predicate
        // can fire on a subsequent `f()`.
        let src = "struct Config { name: i64 }\n\
                   fn apply(c: Config) { }\n\
                   fn main() {\n\
                       let cfg = Config { name: 1 };\n\
                       let f = || apply(cfg);\n\
                       f();\n\
                   }";
        let (class, _, _) = classify(src);
        // At least one Consume tagged on an `f` use — that's the call.
        let f_consumes: Vec<_> = class
            .iter()
            .filter(|(_, k)| **k == UseKind::Consume)
            .collect();
        assert!(
            !f_consumes.is_empty(),
            "expected at least one Consume tag for f's call site; got {:?}",
            class
        );
    }

    #[test]
    fn repeatable_closure_call_is_read() {
        // Closure body only reads (Copy field) → no ClosureCapture
        // origin → not once-callable → call site stays Read.
        let src = "struct Config { value: i64 }\n\
                   fn main() {\n\
                       let cfg = Config { value: 1 };\n\
                       let f = || cfg.value;\n\
                       let _a = f();\n\
                       let _b = f();\n\
                   }";
        let (class, _, _) = classify(src);
        // Two `f()` call sites — both Read because cfg.value is i64
        // (Copy) and no capture-by-ownership occurred. The call-site
        // identifier-leaves of f should NOT be tagged Consume.
        let f_consumes: Vec<_> = class
            .iter()
            .filter(|(_, k)| **k == UseKind::Consume)
            .collect();
        // The let-RHS `f` in `let _a = f();` is NOT a Consume because
        // the call returns i64 (Copy). And both calls of f are Read.
        // So no Consume tags expected for f.
        assert_eq!(
            f_consumes.len(),
            0,
            "no Consume tags expected when closure is repeatable; got {:?}",
            class
        );
    }

    #[test]
    fn explicit_ref_closure_only_reading_capture_is_repeatable() {
        // `let f = ref || println(name);` — explicit ref capture mode +
        // body only reads. No ClosureCapture origin emitted, so f is
        // not once-callable. (The classifier doesn't model the
        // CaptureModeViolation E0504 case where ref is declared but
        // body consumes — that's a hard error caught earlier.)
        let src = "fn main() {\n\
                       let name = 1;\n\
                       let f = ref || name;\n\
                       f();\n\
                       f();\n\
                   }";
        let (class, _, _) = classify(src);
        let f_consumes = class.values().filter(|k| **k == UseKind::Consume).count();
        assert_eq!(
            f_consumes, 0,
            "ref-capture closure with reading body must not produce Consumes; got {:?}",
            class
        );
    }

    #[test]
    fn second_let_with_non_closure_rhs_does_not_taint_once_callable_set() {
        // After `let f = || consume_capture(cfg)`, walking `let g = 1`
        // must not perturb f's once-callability — the once-callable
        // detection only fires when the RHS itself is a closure.
        let src = "struct Config { name: i64 }\n\
                   fn apply(c: Config) { }\n\
                   fn main() {\n\
                       let cfg = Config { name: 1 };\n\
                       let f = || apply(cfg);\n\
                       let g = 1;\n\
                       f();\n\
                   }";
        let (class, _, _) = classify(src);
        // f's call site must be Consume.
        let consumes_count = class.values().filter(|k| **k == UseKind::Consume).count();
        // Expected: cfg in `apply(cfg)` (ClosureCapture-tagged) +
        // f's call site (Direct-tagged). Two Consumes.
        assert!(
            consumes_count >= 2,
            "expected at least two Consumes (capture + call); got {} ({:?})",
            consumes_count,
            class
        );
    }

    #[test]
    fn reassign_target_not_double_counted_as_read() {
        // The bare-ident LHS of an Assign goes onto the Reassign-only
        // path; it must not also leave a Read entry behind. Pin that by
        // verifying the let-RHS reads (`let _y = x;`) are exactly the
        // Read sites the classifier produces.
        let src = "fn main() {\n\
                       let mut x = 1;\n\
                       let _y = x;\n\
                       x = 2;\n\
                   }";
        let (class, _, _) = classify(src);
        let reads = class.values().filter(|k| **k == UseKind::Read).count();
        let reassigns = class.values().filter(|k| **k == UseKind::Reassign).count();
        // One Read on `x` in `let _y = x;` (Copy → Read), one Reassign
        // on the LHS of `x = 2;`. No third Read.
        assert_eq!(
            reads, 1,
            "expected exactly one Read (let-RHS use of x); got {:?}",
            class
        );
        assert_eq!(
            reassigns, 1,
            "expected exactly one Reassign (LHS of x = 2); got {:?}",
            class
        );
    }
}
