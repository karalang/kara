//! Loop-reduction and disjoint-write-loop recognition: the checker-side
//! classifiers that decide which counted loops are reductions, collects,
//! tabulates, or disjoint element writes, and whether their bodies are
//! admissible for fan-out.
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, slice 2). Sibling `impl super::…` block;
//! methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    /// Walk top-level statements in `func.body`; for each loop expression
    /// (`for` / `while` / `loop`), attempt to classify its body as a
    /// reduction over a single outer-scope accumulator. The classifier
    /// is intentionally conservative — anything outside the strict
    /// `acc = acc <op> expr` / `acc op= expr` shape (with op in the
    /// allow-list) returns no recognition. Codegen will re-validate the
    /// shape against type information before emitting the fan-out.
    /// Resolve the SOURCE type of the named scalar accumulator from the
    /// typed AST: find an `Identifier(name)` expression inside the loop
    /// body whose span has an `expr_types` entry (the reduction body always
    /// reads the accumulator — `acc = acc + x` / `acc += x`), and render it
    /// with `type_display`. `None` when the analysis ran untyped or no
    /// typed use resolves — the fan-out gate treats that as eligible
    /// (`par_cost::accumulator_type_fans_out`), preserving pre-gate
    /// behavior rather than inventing a decline codegen might not apply.
    /// B-2026-07-31-14.
    pub(super) fn accumulator_source_type(&self, body: &Block, name: &str) -> Option<String> {
        let types = self.types?;
        fn find_in_expr(
            e: &Expr,
            name: &str,
            types: &crate::typechecker::TypeCheckResult,
        ) -> Option<String> {
            if let ExprKind::Identifier(n) = &e.kind {
                if n == name {
                    if let Some(t) = types
                        .expr_types
                        .get(&crate::resolver::SpanKey::from_span(&e.span))
                    {
                        return Some(crate::typechecker::types::type_display(t));
                    }
                }
            }
            match &e.kind {
                ExprKind::Binary { left, right, .. } => {
                    find_in_expr(left, name, types).or_else(|| find_in_expr(right, name, types))
                }
                ExprKind::Unary { operand, .. } => find_in_expr(operand, name, types),
                ExprKind::Call { callee, args } => {
                    find_in_expr(callee, name, types).or_else(|| {
                        args.iter()
                            .find_map(|a| find_in_expr(&a.value, name, types))
                    })
                }
                ExprKind::MethodCall { object, args, .. } => find_in_expr(object, name, types)
                    .or_else(|| {
                        args.iter()
                            .find_map(|a| find_in_expr(&a.value, name, types))
                    }),
                ExprKind::Index { object, index } => {
                    find_in_expr(object, name, types).or_else(|| find_in_expr(index, name, types))
                }
                ExprKind::FieldAccess { object, .. } => find_in_expr(object, name, types),
                ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } => find_in_expr(condition, name, types)
                    .or_else(|| find_in_block(then_block, name, types))
                    .or_else(|| {
                        else_branch
                            .as_ref()
                            .and_then(|b| find_in_expr(b, name, types))
                    }),
                ExprKind::Block(b) | ExprKind::Seq(b) => find_in_block(b, name, types),
                ExprKind::While {
                    body, condition, ..
                } => find_in_expr(condition, name, types)
                    .or_else(|| find_in_block(body, name, types)),
                ExprKind::For { body, iterable, .. } => {
                    find_in_expr(iterable, name, types).or_else(|| find_in_block(body, name, types))
                }
                ExprKind::Loop { body, .. } => find_in_block(body, name, types),
                _ => None,
            }
        }
        fn find_in_block(
            b: &Block,
            name: &str,
            types: &crate::typechecker::TypeCheckResult,
        ) -> Option<String> {
            for stmt in &b.stmts {
                let hit = match &stmt.kind {
                    StmtKind::Expr(e) => find_in_expr(e, name, types),
                    StmtKind::Assign { target, value } => find_in_expr(target, name, types)
                        .or_else(|| find_in_expr(value, name, types)),
                    StmtKind::CompoundAssign { target, value, .. } => {
                        find_in_expr(target, name, types)
                            .or_else(|| find_in_expr(value, name, types))
                    }
                    StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                        find_in_expr(value, name, types)
                    }
                    _ => None,
                };
                if hit.is_some() {
                    return hit;
                }
            }
            b.final_expr
                .as_ref()
                .and_then(|e| find_in_expr(e, name, types))
        }
        find_in_block(body, name, types)
    }

    pub(super) fn recognize_reductions(
        &self,
        func: &Function,
    ) -> (Vec<LoopReduction>, Vec<DeclinedParLoop>) {
        let mut out = Vec::new();
        // Threaded rather than looked up at the gate: the walk is recursive
        // and does not carry the enclosing `Function`.
        let frozen = self.frozen_param_names(func);
        let mut declined = Vec::new();
        self.recognize_reductions_in_block(&func.body, &mut out, &mut declined, &frozen);
        (out, declined)
    }

    /// Walk the function body for loops over indexed writes and run the
    /// per-iteration disjointness proof
    /// ([`crate::index_disjoint::prove_disjoint_indexed_writes`]) on each.
    ///
    /// See [`DisjointWriteLoop`] for what a discharged proof does and does not
    /// claim.
    pub(super) fn recognize_disjoint_write_loops(&self, func: &Function) -> Vec<DisjointWriteLoop> {
        let mut out = Vec::new();
        self.recognize_disjoint_writes_in_block(func, &func.body, &mut out);
        out
    }

    /// Mirrors [`Self::recognize_reductions_in_block`]'s traversal (top-level
    /// statements, recursing into `if`-arms and loop bodies), with one
    /// difference that follows from the slice scope: once a loop's proof
    /// discharges, its body is **not** re-walked.
    ///
    /// The scope is "parallelize the OUTER loop", and every inner loop of a
    /// disjoint nest is trivially disjoint too (`for c in 0..4 { out[base+c] }`
    /// writes stride-1 slots). Reporting all of them would bury the decision
    /// that matters under its own corollaries. A *declined* loop still recurses,
    /// so an inner candidate stays visible when the outer one is what failed.
    pub(super) fn recognize_disjoint_writes_in_block(
        &self,
        func: &Function,
        block: &Block,
        out: &mut Vec<DisjointWriteLoop>,
    ) {
        for (idx, stmt) in block.stmts.iter().enumerate() {
            let StmtKind::Expr(expr) = &stmt.kind else {
                continue;
            };
            self.recognize_disjoint_writes_in_expr(func, expr, idx, out);
        }
        // B-2026-08-14-24 — a block's TAIL EXPRESSION is part of the block, and
        // skipping it made an ordinary shape invisible: `fn f(..) { if go { for
        // .. } }` has its `if` in tail position, so the loop inside was never
        // CONSIDERED — absent from `--concurrency-report` entirely rather than
        // listed with a decline reason, which reads exactly like a function with
        // no loop in it. The same body with any statement after the `if` was
        // reported, which is what pinned it to tail position rather than to
        // nesting.
        //
        // Indexed at `stmts.len()`, its source-order position.
        if let Some(tail) = block.final_expr.as_deref() {
            self.recognize_disjoint_writes_in_expr(func, tail, block.stmts.len(), out);
        }
    }

    /// One expression's worth of [`Self::recognize_disjoint_writes_in_block`],
    /// so a block's statements and its TAIL EXPRESSION go through identical
    /// logic instead of the tail being skipped (B-2026-08-14-24).
    pub(super) fn recognize_disjoint_writes_in_expr(
        &self,
        func: &Function,
        expr: &Expr,
        idx: usize,
        out: &mut Vec<DisjointWriteLoop>,
    ) {
        match &expr.kind {
            ExprKind::If {
                then_block,
                else_branch,
                ..
            } => {
                self.recognize_disjoint_writes_in_block(func, then_block, out);
                if let Some(else_expr) = else_branch {
                    match &else_expr.kind {
                        // `else { .. }`
                        ExprKind::Block(else_block) => {
                            self.recognize_disjoint_writes_in_block(func, else_block, out)
                        }
                        // `else if ..` — an If expression, not a block, so it
                        // needs the expression walk rather than the block one.
                        _ => self.recognize_disjoint_writes_in_expr(func, else_expr, idx, out),
                    }
                }
                return;
            }
            // B-2026-08-14-24 — a BARE BLOCK hid its loop the same way an `if`
            // did, and for the same reason: nothing descended into it.
            ExprKind::Block(inner) | ExprKind::Seq(inner) => {
                self.recognize_disjoint_writes_in_block(func, inner, out);
                return;
            }
            // ... and so did a MATCH arm. An arm body is an expression, not
            // necessarily a block, so it goes through this walk rather than the
            // block one.
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.recognize_disjoint_writes_in_expr(func, &arm.body, idx, out);
                }
                return;
            }
            ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. } => {}
            _ => return,
        }
        let body = match &expr.kind {
            ExprKind::For { body, .. }
            | ExprKind::While { body, .. }
            | ExprKind::Loop { body, .. } => body,
            _ => unreachable!("filtered above"),
        };
        match self.classify_disjoint_write_loop(func, expr, body, idx) {
            // Not a candidate at all — no indexed write to an outer
            // collection. Keep walking inward.
            None => self.recognize_disjoint_writes_in_block(func, body, out),
            Some(record) => {
                let proven = record.proven();
                out.push(record);
                if !proven {
                    self.recognize_disjoint_writes_in_block(func, body, out);
                }
            }
        }
    }

    /// Run the proof on one loop, then apply the four soundness gates the proof
    /// deliberately does not model. Returns `None` when the loop is not a
    /// candidate (no indexed write to an outside-the-loop collection).
    ///
    /// The gates belong here rather than in `index_disjoint` because each needs
    /// signature, type, or whole-program knowledge the plain-AST footprint walk
    /// does not have:
    ///
    /// - **B-2026-07-16-6** — a body touching a plain (non-`par`) `shared`
    ///   value carries a NON-ATOMIC refcount header. Racing rc-inc/rc-dec
    ///   across workers under-counts it and frees a live object. Disjoint
    ///   *element* writes do not make the refcount traffic disjoint.
    /// - **B-2026-07-23-20** — a callee taking `mut ref` / `mut Slice` (or a
    ///   `mut ref self` method) writes memory this walk never sees, so a
    ///   loop-invariant scratch buffer threaded through a helper is a race the
    ///   footprint proof cannot observe.
    /// - **Console output**, transitively. `karac_par_run` installs a
    ///   per-branch `OutputCapture` and replays it in source order after the
    ///   join, so a `par {}` block's prints are byte-identical to sequential
    ///   execution. `karac_par_reduce` — the substrate this fan-out reuses —
    ///   does **not**: its workers write straight through. A printing body must
    ///   therefore decline, because "auto-par never changes what your program
    ///   prints" is unconditional.
    /// - **Resource effects.** Two iterations running concurrently reorder
    ///   their effects against each other. The statement-level grouper
    ///   serializes conflicting effects for exactly this reason; a loop body is
    ///   no different.
    pub(super) fn classify_disjoint_write_loop(
        &self,
        func: &Function,
        loop_expr: &Expr,
        body: &Block,
        stmt_index: usize,
    ) -> Option<DisjointWriteLoop> {
        let loop_line = loop_expr.span.line;
        let loop_span = loop_expr.span;
        let mk = |loop_var: String,
                  decline: Option<DisjointDecline>,
                  targets: Vec<TargetFootprint>,
                  reason: String| {
            Some(DisjointWriteLoop {
                stmt_index,
                loop_line,
                loop_span,
                loop_var,
                decline,
                targets,
                reason,
            })
        };
        // Candidate filter FIRST. `prove_disjoint_indexed_writes` rejects on
        // loop form before it ever looks for a write, so without this every
        // `while`/`loop` in the program would surface as a declined
        // `unsupported_loop_form` entry — noise that buries the declines
        // naming a real obstacle.
        if !crate::index_disjoint::loop_body_has_outer_indexed_write(body) {
            return None;
        }
        let loop_var = disjoint_candidate_loop_var(loop_expr).unwrap_or_default();
        match prove_disjoint_indexed_writes(loop_expr) {
            Err(DisjointDecline::NoIndexedWrite) => None,
            Err(decline) => mk(
                loop_var,
                Some(decline),
                Vec::new(),
                decline.reason().to_string(),
            ),
            Ok(proof) => {
                if !self.loop_body_types_cross_task_safe(body, &self.frozen_param_names(func)) {
                    let d = DisjointDecline::NotCrossTaskSafe;
                    return mk(proof.loop_var, Some(d), Vec::new(), d.reason().to_string());
                }
                if self.loop_body_shares_outer_mut_borrow(body) {
                    let d = DisjointDecline::SharesOuterMutBorrow;
                    return mk(proof.loop_var, Some(d), Vec::new(), d.reason().to_string());
                }
                if self.loop_body_emits_output(body) {
                    let d = DisjointDecline::BodyEmitsOutput;
                    return mk(proof.loop_var, Some(d), Vec::new(), d.reason().to_string());
                }
                if self.loop_body_has_effects(body) {
                    let d = DisjointDecline::BodyHasEffects;
                    return mk(proof.loop_var, Some(d), Vec::new(), d.reason().to_string());
                }
                if !self.loop_body_write_targets_are_sequences(func, body) {
                    let d = DisjointDecline::UnsupportedTargetType;
                    return mk(proof.loop_var, Some(d), Vec::new(), d.reason().to_string());
                }
                let reason = proof.reason();
                let DisjointWriteProof { loop_var, targets } = proof;
                mk(loop_var, None, targets, reason)
            }
        }
    }

    /// Does this loop body write to the console — directly, or through any
    /// function it can reach?
    ///
    /// Console output is **resourceless by design** (see
    /// `stmt_has_console_output`), so the effect graph cannot see it and
    /// [`Self::loop_body_has_effects`] would wave a printing helper straight
    /// through. The transitive walk is what makes the gate real: the shape that
    /// matters is a kernel calling `log_progress(dy)`, not one calling
    /// `println` inline.
    ///
    /// Bounded by a visited set, so recursion and call cycles terminate.
    /// Unresolvable callees (extern, closures, dynamic dispatch) are treated as
    /// output-emitting — declining a loop costs a missed fan-out; admitting one
    /// scrambles the program's output.
    pub(super) fn loop_body_emits_output(&self, body: &Block) -> bool {
        let mut visited: HashSet<String> = HashSet::new();
        self.block_emits_output_transitively(body, &mut visited, 0)
    }

    /// Recursion depth cap for [`Self::loop_body_emits_output`]. A call chain
    /// deeper than this is treated as output-emitting rather than searched
    /// further — the conservative direction.
    const OUTPUT_WALK_MAX_DEPTH: usize = 16;

    pub(super) fn block_emits_output_transitively(
        &self,
        block: &Block,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> bool {
        if depth > Self::OUTPUT_WALK_MAX_DEPTH {
            return true;
        }
        if block_has_console_output(block) {
            return true;
        }
        let mut callees: HashSet<String> = HashSet::new();
        collect_callee_names_in_block(block, &mut callees);
        for name in callees {
            if !visited.insert(name.clone()) {
                continue;
            }
            // A lowered primitive op arrives as `i64.mul`; the builtin check
            // keys on the method segment, so both spellings resolve.
            let bare = name.rsplit('.').next().unwrap_or(name.as_str());
            let Some(callee) = self
                .function_bodies
                .get(&name)
                .or_else(|| self.method_bodies.get(&name))
            else {
                // Not a source-defined function in this program: a stdlib
                // method (`push`, `len`, `sort`), a lowered primitive op
                // (`i64.mul`), or an `extern`. None of the stdlib surface
                // writes to the console except the console writers themselves,
                // which `unresolved_callee_may_print` names — so treating
                // every unresolvable callee as printing would decline any loop
                // that so much as calls `.push`, which is most of them.
                if unresolved_callee_may_print(bare) {
                    return true;
                }
                continue;
            };
            if self.block_emits_output_transitively(&callee.body, visited, depth + 1) {
                return true;
            }
        }
        false
    }

    /// Is every indexed write in this body a store into a positionally-indexed
    /// SEQUENCE — a `Vec`, a `Slice`, or a fixed-size array?
    ///
    /// The footprint proof reasons about index *arithmetic* and cannot see what
    /// the container is. `m[k] = v` on a `Map[i64, V]` parses exactly like an
    /// element store and its "index" is an ordinary integer expression, so the
    /// proof concludes "iteration `i` writes slot `i`, disjoint". It is not: a
    /// hash insert places by hash, distinct keys share buckets, and an insert
    /// can resize the whole table. `Set` has the same shape.
    ///
    /// Codegen declines a hash container independently — a `Map` control block
    /// is not a Vec header, so `disjoint_target_shares_storage` rejects it, and
    /// the emitted binary was never at risk. What this gate protects is the
    /// QUERY: without it the surface answered `disjoint_writes: true` and
    /// `fanned_out: true` for a loop the binary does not fan out, which is
    /// exactly the recognition-reported-as-emission defect B-2026-07-29-29 was
    /// filed for.
    ///
    /// **Why the declaration and not `expr_types`.** The obvious
    /// implementation — look up the target object's type — does not work: the
    /// typechecker records the INDEX expression's type at the object's span
    /// (`out[i]` on a `Slice[i64]` reports `i64` at `out`), so the lookup
    /// returns the element type for sequence and hash containers alike.
    /// Measured on both before settling on the declaration.
    pub(super) fn loop_body_write_targets_are_sequences(
        &self,
        func: &Function,
        body: &Block,
    ) -> bool {
        let mut objects: Vec<&Expr> = Vec::new();
        collect_index_assign_objects_in_block(body, &mut objects);
        objects.iter().all(|obj| match &obj.kind {
            ExprKind::Identifier(name) => self.binding_is_indexable_sequence(func, name),
            // A non-identifier target root (`self.buf[i]`, `a[i][j]`) is outside
            // the proof's `name[index]` shape anyway.
            _ => false,
        })
    }

    /// Does `name`'s declaration in `func` say it is a positionally-indexed
    /// sequence?
    ///
    /// Checked, in order: the parameter list, then any `let` in the body —
    /// annotation first, initializer second. Fails CLOSED: a binding whose
    /// declaration cannot be read is not assumed to be a `Vec`, because the
    /// cost of a wrong "yes" is a query that claims a fan-out the binary does
    /// not perform.
    pub(super) fn binding_is_indexable_sequence(&self, func: &Function, name: &str) -> bool {
        for p in &func.params {
            if p.pattern.binding_names().iter().any(|n| n == name) {
                return type_expr_is_sequence(&p.ty);
            }
        }
        match find_binding_decl(&func.body, name) {
            Some((Some(ty), _)) => type_expr_is_sequence(ty),
            Some((None, Some(init))) => init_expr_builds_sequence(init),
            _ => false,
        }
    }

    /// Does this loop body perform an effect whose ORDER two concurrent
    /// iterations would change?
    ///
    /// Not every effect qualifies, and treating them alike was measurably too
    /// strict — a body that builds one intermediate `Vec` infers `allocates`,
    /// which would have declined most real image kernels for no reason.
    ///
    /// - `reads` / `writes` / `sends` / `receives` on a user resource, and any
    ///   `UserDefined` verb: **decline**. These are the verbs the
    ///   statement-level grouper serializes on, and a loop body is no
    ///   different. (`reads` included deliberately: two workers reading one
    ///   `File` share its offset.)
    /// - `blocks` / `suspends`: **decline**. The execution verbs drive
    ///   scheduler placement; parking inside a fan-out worker occupies a pool
    ///   thread that the dispatch is counting on.
    /// - `allocates`: **allow**. The allocator is thread-safe and allocation
    ///   order is not observable.
    /// - `panics`: **allow**. A panicking worker aborts the process exactly as
    ///   the sequential loop would; which iteration trips first can differ, but
    ///   only for an already-failing program. Same call the reduction lowering
    ///   makes.
    ///
    /// A polymorphic callee (`with _`) declines: its effects are unknown here,
    /// so none of the reasoning above applies.
    pub(super) fn loop_body_has_effects(&self, body: &Block) -> bool {
        let mut info = StmtInfo::default();
        self.collect_block_effects(body, &mut info);
        if info.calls_polymorphic {
            return true;
        }
        info.effects.iter().any(|e| {
            matches!(
                e.verb,
                EffectVerbKind::Reads
                    | EffectVerbKind::Writes
                    | EffectVerbKind::Sends
                    | EffectVerbKind::Receives
                    | EffectVerbKind::Blocks
                    | EffectVerbKind::Suspends
                    | EffectVerbKind::UserDefined(_)
            )
        })
    }

    /// Walk one block's statements for reduction-shaped loops, recursing
    /// into nested loop bodies and if-arms. Recursion (2026-07-15) is what
    /// lets a `#[par_order_free]` collect loop nested inside an outer
    /// sequential loop fan out — the LBM-substep shape (`while s < steps {
    /// … #[par_order_free] while c < n { out.push(f(grid[c])) } … }`),
    /// which the previous top-level-only walk silently left sequential.
    /// A `LoopReduction`'s `stmt_index` is the loop's index within ITS OWN
    /// block; codegen's lookup disambiguates by (stmt_index, loop_line),
    /// so equal indices across sibling blocks can't cross-match. Recursing
    /// into a body that is itself reduction-classified is deliberate: the
    /// runtime's fork-depth cap (`KARAC_PAR_MAX_FORK_DEPTH`) already makes
    /// inner regions run sequentially inline (see the recursion note
    /// below), so nested tags are safe.
    pub(super) fn recognize_reductions_in_block(
        &self,
        block: &Block,
        out: &mut Vec<LoopReduction>,
        declined: &mut Vec<DeclinedParLoop>,
        frozen: &HashSet<String>,
    ) {
        // B-2026-08-14-24 — the tail expression is walked with the statements.
        // This lane has the same blind spot the disjoint-write one did: it
        // descends into `if` arms correctly but only ever from `block.stmts`, so
        // `fn f(..) { if go { for .. } }` — the `if` in TAIL position — was
        // never reached. The row was filed against the disjoint lane alone and
        // attributed the difference to the two lanes using different walks;
        // measured, both walks are the same shape and both skipped the tail.
        let tail = block.final_expr.as_deref();
        let stmt_exprs =
            block
                .stmts
                .iter()
                .enumerate()
                .filter_map(|(idx, stmt)| match &stmt.kind {
                    StmtKind::Expr(expr) => Some((idx, expr)),
                    _ => None,
                });
        for (idx, expr) in stmt_exprs.chain(tail.map(|t| (block.stmts.len(), t))) {
            let _ = idx;
            match &expr.kind {
                ExprKind::If {
                    then_block,
                    else_branch,
                    ..
                } => {
                    self.recognize_reductions_in_block(then_block, out, declined, frozen);
                    if let Some(else_expr) = else_branch {
                        match &else_expr.kind {
                            ExprKind::Block(else_block) => self
                                .recognize_reductions_in_block(else_block, out, declined, frozen),
                            // `else if ..` chains through as an If expression.
                            ExprKind::If { .. } => {
                                let chain = Block {
                                    stmts: Vec::new(),
                                    final_expr: Some(Box::new((**else_expr).clone())),
                                    span: else_expr.span,
                                };
                                self.recognize_reductions_in_block(&chain, out, declined, frozen);
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                // A BARE BLOCK hid its loop the same way.
                ExprKind::Block(inner) | ExprKind::Seq(inner) => {
                    self.recognize_reductions_in_block(inner, out, declined, frozen);
                    continue;
                }
                // ... and so did a MATCH arm.
                ExprKind::Match { arms, .. } => {
                    for arm in arms {
                        let arm_block = Block {
                            stmts: Vec::new(),
                            final_expr: Some(Box::new(arm.body.clone())),
                            span: arm.body.span,
                        };
                        self.recognize_reductions_in_block(&arm_block, out, declined, frozen);
                    }
                    continue;
                }
                ExprKind::For { .. } | ExprKind::While { .. } | ExprKind::Loop { .. } => {}
                _ => continue,
            }
            let (body, attributes) = match &expr.kind {
                ExprKind::For {
                    body, attributes, ..
                }
                | ExprKind::While {
                    body, attributes, ..
                }
                | ExprKind::Loop {
                    body, attributes, ..
                } => (body, attributes.as_slice()),
                _ => unreachable!("filtered above"),
            };
            self.recognize_reductions_in_block(body, out, declined, frozen);
            let induction_var = loop_induction_var(expr);
            // B-2026-08-15-19: an ANNOTATED loop that does not fan out owes the
            // author a reason, on every path out of here — the classifier's own
            // decline and both soundness gates below. `note_decline` is a no-op
            // for an unannotated loop, so the reporting surface stays the set of
            // loops whose author actually asked for parallelism.
            let annotated = attributes.iter().any(|a| a.is_par_order_free());
            let note_decline = |declined: &mut Vec<DeclinedParLoop>, reason: &'static str| {
                if annotated {
                    declined.push(DeclinedParLoop {
                        stmt_index: idx,
                        loop_line: expr.span.line,
                        reason,
                    });
                }
            };
            let classified = self.classify_loop_body(body, attributes, induction_var.as_deref());
            let classified = match classified {
                Ok(c) => Some(c),
                Err(reason) => {
                    note_decline(declined, reason);
                    None
                }
            };
            if let Some((accumulator, op)) = classified {
                // B-2026-07-16-6 soundness gate: the reduction lowering runs
                // this body on MULTIPLE worker threads, so any value the body
                // touches that is reachable from outside one iteration is
                // visible to all workers. A plain `shared` (non-`par`) handle
                // carries a NON-ATOMIC refcount header — one racing
                // rc-inc/rc-dec pair across workers is a lost update that
                // under-counts the header and frees a still-referenced object
                // (use-after-free / double-free / heap corruption). The body
                // must therefore satisfy the same cross-task-safe predicate an
                // explicit `spawn` capture does; decline the reduction (the
                // loop lowers sequentially) when it doesn't.
                if !self.loop_body_types_cross_task_safe(body, frozen) {
                    note_decline(
                        declined,
                        "the body touches a plain `shared` value, whose refcount header is \
non-atomic and would race across workers",
                    );
                    continue;
                }
                // B-2026-07-23-20 soundness gate: decline when the body passes a
                // `mut ref`/`mut Slice` (or `mut ref self`) to a loop-invariant
                // shared buffer — parallel workers would race on it (the direct-
                // write gates below can't see a mutation performed inside the
                // callee).
                if self.loop_body_shares_outer_mut_borrow(body) {
                    note_decline(
                        declined,
                        "the body passes a `mut ref` to a loop-invariant buffer, which every \
worker would write",
                    );
                    continue;
                }
                // A reduction whose per-iteration delta recurses into the
                // enclosing function (e.g. a backtracking counter
                // `if legal { total = total + count(...deeper...) }`) is
                // recognized and lowered like any other. It used to be declined
                // here (B-2026-07-03-14) because parallelizing every recursion
                // level nested a parallel region per depth and exhausted the
                // stack — but the runtime now caps reduction fan-out depth
                // (`KARAC_PAR_MAX_FORK_DEPTH`, default 1, in `karac_par_reduce`),
                // so only the OUTERMOST level parallelizes and every deeper
                // level runs sequentially inline. That bounds nesting to a
                // constant and turns the crash into the useful case: a
                // backtracking search parallelized at its independent top-level
                // branches. The cost/shape gates in codegen still apply.
                let collect_tabulate = op == ReductionOp::Collect
                    && self.collect_is_tabulate_shape(body, &accumulator);
                // Scalar accumulators only: the Collect lowering's Vec
                // accumulator never consults the integer type gate.
                let accumulator_type = if op == ReductionOp::Collect {
                    None
                } else {
                    self.accumulator_source_type(body, &accumulator)
                };
                out.push(LoopReduction {
                    accumulator,
                    op,
                    stmt_index: idx,
                    loop_line: expr.span.line,
                    collect_tabulate,
                    seq: false,
                    accumulator_type,
                });
            } else if !attributes.iter().any(|a| a.is_par_order_free()) {
                // No reduction classified and no par opt-in: try the
                // SEQUENTIAL collect-tabulate shape. Unlike the par
                // classifier, other loop-carried writes (a scalar
                // accumulation alongside the push, extra counters) are
                // fine — the lowering compiles every non-push statement
                // inline in source order; only the push itself is
                // rewritten into an in-place store. The tabulate shape
                // check guarantees the accumulator appears exactly once,
                // as the receiver of one unconditional top-level push.
                if let Some(acc) = self.classify_seq_collect_tabulate(body, expr) {
                    out.push(LoopReduction {
                        accumulator: acc,
                        op: ReductionOp::Collect,
                        stmt_index: idx,
                        loop_line: expr.span.line,
                        collect_tabulate: true,
                        seq: true,
                        accumulator_type: None,
                    });
                }
            }
        }
    }

    /// Find the single outer-scope Vec accumulator of a sequential
    /// tabulate loop, if the body has that shape: exactly one top-level
    /// unconditional `acc.push(EXPR)` where `acc` is an outer binding
    /// mentioned nowhere else in the body (`collect_is_tabulate_shape`
    /// does the exactness check). Candidate discovery scans top-level
    /// bare pushes; two pushes to DIFFERENT accumulators is declined
    /// (each would fail the other's mention check anyway).
    ///
    /// LOOP-CONTROL immutability (B-2026-07-16-7): the tabulate lowering
    /// precomputes the trip count, so the body must not be able to
    /// change how many iterations the SOURCE loop would run. For a
    /// while-loop, any body write to a variable the condition reads
    /// (the counter itself, the bound, a `.len()` receiver) — other
    /// than the terminal step-one increment the codegen strips — makes
    /// the source trip count body-dependent: DECLINE. (The self-hosted
    /// lexer's `if escaped { i = i + 1 }` skip-advance inside a push
    /// loop is the live shape that miscompiled.) For a for-range loop
    /// the range is evaluated once up front in source semantics too, so
    /// bound writes are harmless — but a body write to the LOOP VAR
    /// still diverges (source rebinds it fresh each iteration; the
    /// lowering persists one alloca): DECLINE that as well.
    pub(super) fn classify_seq_collect_tabulate(
        &self,
        body: &Block,
        loop_expr: &Expr,
    ) -> Option<String> {
        let mut candidate: Option<String> = None;
        let consider = |name: Option<String>, candidate: &mut Option<String>| -> bool {
            let Some(n) = name else { return true };
            match candidate {
                None => {
                    *candidate = Some(n);
                    true
                }
                Some(existing) => *existing == n,
            }
        };
        for stmt in &body.stmts {
            if let StmtKind::Expr(e) = &stmt.kind {
                if !consider(collect_push_shape(e), &mut candidate) {
                    return None;
                }
            }
        }
        if let Some(e) = &body.final_expr {
            if !consider(collect_push_shape(e), &mut candidate) {
                return None;
            }
        }
        let acc = candidate?;
        if !self.collect_is_tabulate_shape(body, &acc) {
            return None;
        }

        // ── Loop-control immutability gate. ──
        // Names the trip count depends on:
        let mut control_reads: HashSet<String> = HashSet::new();
        match &loop_expr.kind {
            ExprKind::While { condition, .. } => {
                self.collect_expr_reads(condition, &mut control_reads);
            }
            ExprKind::For {
                pattern, iterable, ..
            } => {
                // Range bounds are pre-evaluated in source semantics; only
                // the loop variable itself is control state.
                let _ = iterable;
                if let PatternKind::Binding(name) = &pattern.kind {
                    control_reads.insert(name.clone());
                }
            }
            _ => return None,
        }
        if control_reads.is_empty() {
            return Some(acc);
        }

        // Names the body writes — Assign/CompoundAssign targets plus
        // nested writes (if-arms, inner loops, mutating method
        // receivers) via the same walker the auto-par dependency check
        // trusts. Body-local rebindings are not loop-carried; the
        // while-form's TERMINAL step-one increment is the one exempted
        // write (extract_loop_shape strips it before codegen).
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }
        let is_while = matches!(loop_expr.kind, ExprKind::While { .. });
        let last_idx = body.stmts.len().saturating_sub(1);
        let mut written: HashSet<String> = HashSet::new();
        for (i, stmt) in body.stmts.iter().enumerate() {
            match &stmt.kind {
                StmtKind::Assign { target, value } => {
                    if is_while && i == last_idx && body.final_expr.is_none() {
                        if let Some(name) = identifier_name(target) {
                            if induction_step_via_assign(value, &name) {
                                // The terminal counter step — stripped by
                                // extract_loop_shape, exempt here.
                                continue;
                            }
                        }
                    }
                    self.collect_assign_target_defines(target, &mut written);
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_assign_target_defines(target, &mut written);
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::Let { value, .. } => {
                    self.collect_expr_inner_writes(value, &mut written);
                }
                StmtKind::Expr(e) => {
                    self.collect_expr_inner_writes(e, &mut written);
                }
                _ => {}
            }
        }
        if let Some(e) = &body.final_expr {
            self.collect_expr_inner_writes(e, &mut written);
        }
        if written
            .iter()
            .any(|w| !let_introduced.contains(w) && control_reads.contains(w))
        {
            return None;
        }
        Some(acc)
    }

    /// B-2026-07-16-6: true when every typed expression inside `body`
    /// satisfies [`crate::cross_task_safe::is_cross_task_safe`] — the
    /// same predicate enforced on explicit `spawn` / `par {}` captures.
    ///
    /// Implementation is a span sweep over `expr_types` (every entry
    /// whose span lies inside the body block), NOT an AST walk: a walk
    /// has to enumerate every `ExprKind` and a missed variant silently
    /// reopens the soundness hole, while the sweep is shape-blind and
    /// stays exhaustive as the language grows. A body expression with no
    /// `expr_types` entry contributes nothing (the racing values — reads
    /// of outer bindings and their projections — are bread-and-butter
    /// typed expressions). The cost of a false decline is a sequential
    /// loop, never a miscompile.
    ///
    /// B-2026-07-30-1 PRECISION PASS. The sweep alone also declines the
    /// case where the `shared` value is ALLOCATED and fully consumed
    /// inside ONE iteration — thread-local for its whole life, so its
    /// non-atomic header is only ever touched by the thread that made
    /// it. That shape is common (any loop that builds a linked list /
    /// tree per iteration and folds it; katas #23 and #86 got 1.00x /
    /// 1.02x from it while peers reached 2.0–2.9x), so an escape
    /// analysis — [`crate::iter_local`] — now runs alongside the sweep.
    /// It supplies the spans of expressions that provably cannot alias
    /// an object from outside the iteration, and the decline stands only
    /// for unsafe-typed spans OUTSIDE that whitelist.
    ///
    /// The whitelist is fail-CLOSED: it is built from positive evidence
    /// of freshness only, so an `ExprKind` it does not model — today's
    /// or tomorrow's — is simply absent from it and the sweep's decline
    /// holds. It never clears a span the sweep flagged on its own
    /// authority; it only exempts spans it can prove fresh. The type
    /// gate stays the fallback for everything else — do NOT weaken it,
    /// B-2026-07-16-6 documents the use-after-free it prevents.
    ///
    /// Without type info (`self.types` is `None` — the untyped
    /// `concurrency_analyze` convenience entry used by analysis-only
    /// tests), recognition is left unchanged: every path that LOWERS a
    /// reduction (cli.rs `concurrencycheck`) runs the typed form.
    /// Names of `func`'s `frozen` parameters — the roots
    /// [`Self::loop_body_types_cross_task_safe`] may exempt from the
    /// cross-task-safety gate.
    ///
    /// PARAMETERS ONLY, and that is now a deliberate narrowing rather than an
    /// exhaustive list. Stage 2.5 (B-2026-08-01-33) also makes a `let` bound
    /// from a frozen place a frozen root, but that admission lives in the
    /// ownership pass, which this analysis does not receive — re-deriving it
    /// here would be a second opinion about what "frozen" means, the drift
    /// hazard that entry already paid for once. Leaving aliases out only
    /// SHRINKS the whitelist, so the gate's decline stands and the failure
    /// direction is a sequential loop.
    ///
    /// In practice an alias body is still admitted, because the gate's sweep
    /// records no cross-task-unsafe span for the alias itself — the only
    /// unsafe span in such a body is the frozen ROOT, which this list does
    /// cover. Measured and pinned by
    /// `test_disjoint_write_admitted_for_frozen_alias_binding`.
    pub(super) fn frozen_param_names(&self, func: &Function) -> HashSet<String> {
        func.params
            .iter()
            .filter(|p| p.is_frozen)
            .filter_map(|p| p.name().map(str::to_string))
            .collect()
    }

    pub(super) fn loop_body_types_cross_task_safe(
        &self,
        body: &Block,
        frozen_params: &HashSet<String>,
    ) -> bool {
        let Some(tc) = self.types else {
            return true;
        };
        let lo = body.span.offset;
        let hi = body.span.offset + body.span.length;
        let mut unsafe_spans: Vec<SpanKey> = Vec::new();
        for (key, ty) in &tc.expr_types {
            let SpanKey(offset, length) = *key;
            if offset >= lo
                && offset + length <= hi
                && crate::cross_task_safe::is_cross_task_safe(ty, tc).is_err()
            {
                unsafe_spans.push(*key);
            }
        }
        if unsafe_spans.is_empty() {
            return true;
        }
        // Opt-out for the precision pass alone; `=0` restores the
        // pure type gate.
        if std::env::var("KARAC_PAR_ITER_LOCAL_SHARED").as_deref() == Ok("0") {
            return false;
        }
        // B-2026-08-01-33 mechanism 3, auto-par arm: a second whitelist of
        // places rooted at a `frozen` parameter. Such a place is deeply
        // immutable (E0512 refused the parameter otherwise) and emits no
        // refcount traffic (`frozen T` lowers to a borrow) — the same two
        // facts that license admitting it into an explicit `par {}` block,
        // so the auto-par gate that models the identical hazard admits it
        // too. Without this, the two surfaces disagree: a handle explicit
        // `par` accepts still forces the loop sequential.
        //
        // Whitelisted by ROOT, never by type. A body holding both a
        // `frozen S` parameter and a freshly built local `S` must exempt
        // only the former; the local's refcount traffic is exactly the race
        // this gate exists to catch.
        let frozen = crate::iter_local::spans_rooted_at(body, frozen_params);
        // B-2026-08-18-21 — a third whitelist: places the body only ever
        // reaches THROUGH, on the way to a scalar leaf. `out[j] = ps[j].v * 2`
        // over `ps: Vec[P]` with `shared struct P` never holds a `P`; the
        // projection is a GEP and a load, and the loop body emits no refcount
        // traffic at all, so there is no non-atomic header to race. Neither
        // sibling covers it — `ps` is an outer-scope name, not iteration-local,
        // and not a `frozen` parameter.
        //
        // Explicit here because it USED to be accidental. While `Index` copied
        // its object's span, `ps`, `ps[j]` and `ps[j].v` shared one SpanKey and
        // the `i64` was merely the last write, so the sweep never saw
        // `Vec[shared P]`. Giving the subscript its own span surfaced the type
        // the gate always should have had, and the exemption had to be argued
        // rather than inherited. `scalar_projection_bases` taints the whole
        // root on any other use, so the fail-closed direction is unchanged.
        let projected = crate::iter_local::scalar_projection_bases(body, tc);
        if unsafe_spans
            .iter()
            .all(|key| frozen.contains(key) || projected.contains(key))
        {
            return true;
        }
        let Some(local) = crate::iter_local::iteration_local_spans(body, tc) else {
            return false;
        };
        unsafe_spans
            .iter()
            .all(|key| local.contains(key) || frozen.contains(key) || projected.contains(key))
    }

    /// Classify a loop body as a reduction over a single outer-scope
    /// accumulator. Returns `Some((name, op))` if every top-level
    /// loop-carried write to an outer-scope name is reduction-shaped
    /// against the same accumulator with the same op (with induction-
    /// shape writes — `i = i + const_lit`, `i += const_lit` — allowed
    /// alongside as loop-counter steps). Returns `None` for any other
    /// shape: multiple distinct accumulators, mixed ops, non-reduction
    /// writes, or writes nested inside `if`/`else`/inner-loop branches.
    /// `Err` carries the OBLIGATION THAT FAILED, not a generic "no".
    ///
    /// B-2026-08-15-19: a loop carrying `#[par_order_free]` is the one case
    /// where the compiler knows the author expected parallelism, and returning
    /// a bare `None` left them with a sequential binary and nothing to read —
    /// the loop did not even appear among the declined ones in
    /// `karac query concurrency`. `DisjointWriteLoop`'s doc has stated the
    /// principle since it was written: *"one record per candidate loop,
    /// whether or not the proof discharged: the declined case is the whole
    /// point of the surface"*. This is that, for the reduction path.
    ///
    /// The strings are `&'static str` and are attached AT the decline site, so
    /// they cannot drift away from the condition that produced them — a
    /// post-hoc re-diagnosis of the body would be free to disagree with the
    /// real reason, which is worse than silence.
    pub(super) fn classify_loop_body(
        &self,
        body: &Block,
        attributes: &[Attribute],
        induction_var: Option<&str>,
    ) -> Result<(String, ReductionOp), &'static str> {
        // `#[par_order_free]` opts into the collect-shape recognizer
        // (`acc.push(x)` and `if cond { acc.push(x); }`). Other loops
        // see only the scalar-reduction shapes. See
        // `phase-7-codegen.md` collect-style follow-on for the design.
        let par_order_free = attributes.iter().any(|a| a.is_par_order_free());
        // Names freshly introduced inside the loop body. Writes to these
        // are body-scoped and not loop-carried.
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }

        let mut reduction: Option<(String, ReductionOp)> = None;
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Assign { target, value } => {
                    let name = identifier_name(target).ok_or(
                        "the assignment target is not a plain name, so it cannot be a \
 reduction accumulator",
                    )?;
                    if let_introduced.contains(&name) {
                        // Assign to a body-local name (re-bound after let).
                        // Not loop-carried; ignored.
                        continue;
                    }
                    // Induction shape is a strict subset of reduction shape
                    // (`i = i + 1` matches the `+` reduction check too) — so
                    // check induction first and short-circuit, otherwise an
                    // explicit `while`-loop counter would be tagged as the
                    // reduction accumulator and fight whichever real
                    // accumulator the loop also writes to.
                    //
                    // B-2026-08-11-16: that skip MUST be tied to the loop's own
                    // counter by NAME. The shape test alone matches any
                    // literal-step accumulator, so `while i < n { b = b + f(x);
                    // a = a + 1; }` classified `b` as the reduction and silently
                    // IGNORED `a` — neither reduced nor rejected. The lowering
                    // then rebinds only the accumulator and the loop variable
                    // per worker and captures everything else, so `a`'s writes
                    // landed in per-worker copies and the parent kept its
                    // pre-loop value: a wrong answer, no diagnostic. Naming the
                    // counter makes any OTHER literal-step write a reduction
                    // candidate, which is what it is — and two distinct
                    // candidates then decline the loop below, exactly as two
                    // distinct non-literal accumulators already did. Note the
                    // same statement wrapped in `if cond { .. }` was always
                    // treated as a reduction by `conditional_acc_update_shape`,
                    // so the bare form was the odd one out.
                    if induction_step_via_assign(value, &name) && induction_var == Some(name.as_str())
                    {
                        // i = i + const_lit — the loop's own counter step;
                        // ignored.
                    } else {
                        let op = reduction_binary_shape(value, &name).ok_or(
                        "the assignment is not `acc = acc <op> delta` with an associative \
and commutative op",
                    )?;
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                }
                            }
                        }
                    }
                }
                StmtKind::CompoundAssign { target, op, value } => {
                    let name = identifier_name(target).ok_or(
                        "the compound-assignment target is not a plain name, so it cannot be a \
reduction accumulator",
                    )?;
                    if let_introduced.contains(&name) {
                        continue;
                    }
                    let Some(red_op) = ReductionOp::from_compound_op(op) else {
                        // Sub / Div / Mod / Shl / Shr — not in the
                        // associative + commutative allow-list.
                        return Err("the compound-assignment operator is not associative and commutative, so per-worker partials cannot be combined");
                    };
                    // Mirror of the Assign-branch induction-first rule:
                    // `i += 1` matches the `+` reduction shape, so check
                    // for the counter-step shape first — and, per
                    // B-2026-08-11-16, only for the loop's OWN counter.
                    if red_op == ReductionOp::Add
                        && is_int_literal(value)
                        && induction_var == Some(name.as_str())
                    {
                        // i += const_lit — the loop's own counter step; ignored.
                        continue;
                    }
                    match reduction {
                        None => reduction = Some((name, red_op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != red_op {
                                return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                            }
                        }
                    }
                }
                StmtKind::Let { .. } | StmtKind::LetElse { .. } | StmtKind::LetUninit { .. } => {
                    // Fresh body bindings; not loop-carried.
                }
                StmtKind::Expr(expr) => {
                    // First: try the conditional-assign Min/Max desugar —
                    // `if x < acc { acc = x; }` and friends shape a
                    // recognized reduction step even though the inner-write
                    // check below would otherwise reject any if-stmt that
                    // writes an outer-scope name.
                    if let Some((name, op)) = conditional_minmax_shape(expr) {
                        if let_introduced.contains(&name) {
                            // Body-local accumulator; ignore.
                            continue;
                        }
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                }
                            }
                        }
                        continue;
                    }
                    // Next: conditional accumulator-update shape —
                    // `if cond { acc = acc + delta; }` (and the OP=
                    // form). Semantically equivalent to
                    // `acc = acc + (if cond { delta } else { 0 })`,
                    // so reducible under the same associative+commutative
                    // op as the unconditional form. The condition must
                    // not read the accumulator (order-dependent), which
                    // the helper verifies.
                    if let Some((name, op)) = self.conditional_acc_update_shape(expr) {
                        if let_introduced.contains(&name) {
                            continue;
                        }
                        match reduction {
                            None => reduction = Some((name, op)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != op {
                                    return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                }
                            }
                        }
                        continue;
                    }
                    // Collect-style recognition (Phase 2 — gated on
                    // `#[par_order_free]`). Two shapes:
                    //   acc.push(EXPR)                                  (bare)
                    //   if cond { acc.push(EXPR); }                     (conditional)
                    // The combine model is per-worker partial Vecs
                    // concat'd in worker-order. With statically assigned
                    // contiguous chunks and no work-stealing, worker
                    // order IS iteration order, so this path preserves
                    // ordering today (B-2026-07-29-30 measured it with a
                    // position-sensitive digest); the attribute is the
                    // user's promise that they do not depend on that,
                    // which is what leaves reordering available later.
                    // Push arg expressions are accepted as-is (no
                    // acc-read restriction inside them is needed for
                    // correctness: the arg is per-iter data, evaluated
                    // within the worker's slice, never folded with
                    // sibling workers' partials before final concat).
                    if par_order_free {
                        if let Some(name) = collect_push_shape(expr) {
                            if let_introduced.contains(&name) {
                                continue;
                            }
                            match reduction {
                                None => reduction = Some((name, ReductionOp::Collect)),
                                Some((ref existing_name, existing_op)) => {
                                    if existing_name != &name || existing_op != ReductionOp::Collect
                                    {
                                        return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                    }
                                }
                            }
                            continue;
                        }
                        if let Some(name) = self.conditional_collect_shape(expr) {
                            if let_introduced.contains(&name) {
                                continue;
                            }
                            match reduction {
                                None => reduction = Some((name, ReductionOp::Collect)),
                                Some((ref existing_name, existing_op)) => {
                                    if existing_name != &name || existing_op != ReductionOp::Collect
                                    {
                                        return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                    }
                                }
                            }
                            continue;
                        }
                    }
                    // Else: any inner write to an outer-scope name (via
                    // nested if/else or inner loop) breaks the simple-
                    // reduction recognition; defer multi-write loops to a
                    // later slice.
                    let mut inner_writes = HashSet::new();
                    self.collect_expr_inner_writes(expr, &mut inner_writes);
                    for w in &inner_writes {
                        if !let_introduced.contains(w) {
                            return Err("a statement writes a name declared outside the loop and it is not a recognized reduction or `push`");
                        }
                    }
                }
                StmtKind::Defer { .. } | StmtKind::ErrDefer { .. } => {
                    // Defers run at scope exit, not per-iteration; treat
                    // conservatively as a rejection signal — a defer with
                    // a captured-write reads its surrounding loop's
                    // accumulator state in a way the fan-out / combine
                    // model doesn't preserve.
                    return Err("the body has a `defer`, which runs at scope exit rather than per iteration");
                }
            }
        }

        // Same audit on the block's trailing expression. A loop body that
        // ends with `if x < acc { acc = x; }` (no trailing semicolon)
        // parses the if as `final_expr` rather than `Stmt::Expr`; the
        // conditional-assign recognizer must fire here too or the kata-153
        // shape (`for i in 1..n { let x = nums[i]; if x < m { m = x; } }`)
        // silently falls back to sequential.
        if let Some(e) = &body.final_expr {
            if let Some((name, op)) = conditional_minmax_shape(e) {
                if !let_introduced.contains(&name) {
                    match reduction {
                        None => reduction = Some((name, op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != op {
                                return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                            }
                        }
                    }
                }
            } else if let Some((name, op)) = self.conditional_acc_update_shape(e) {
                if !let_introduced.contains(&name) {
                    match reduction {
                        None => reduction = Some((name, op)),
                        Some((ref existing_name, existing_op)) => {
                            if existing_name != &name || existing_op != op {
                                return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                            }
                        }
                    }
                }
            } else if par_order_free {
                // Mirror of the StmtKind::Expr collect-shape arm above.
                // Trailing-expression position (no semicolon on the last
                // collect step) — analogous to `conditional_minmax_shape`
                // landing in both stmt + final_expr positions.
                if let Some(name) =
                    collect_push_shape(e).or_else(|| self.conditional_collect_shape(e))
                {
                    if !let_introduced.contains(&name) {
                        match reduction {
                            None => reduction = Some((name, ReductionOp::Collect)),
                            Some((ref existing_name, existing_op)) => {
                                if existing_name != &name || existing_op != ReductionOp::Collect {
                                    return Err("the body reduces into more than one accumulator, or with more than one operator — fan-out combines a single accumulator");
                                }
                            }
                        }
                    }
                } else {
                    let mut inner_writes = HashSet::new();
                    self.collect_expr_inner_writes(e, &mut inner_writes);
                    for w in &inner_writes {
                        if !let_introduced.contains(w) {
                            return Err("a statement writes a name declared outside the loop and it is not a recognized reduction or `push`");
                        }
                    }
                }
            } else {
                let mut inner_writes = HashSet::new();
                self.collect_expr_inner_writes(e, &mut inner_writes);
                for w in &inner_writes {
                    if !let_introduced.contains(w) {
                        return Err("a statement writes a name declared outside the loop and it is not a recognized reduction or `push`");
                    }
                }
            }
        }

        reduction.ok_or(
            "the body has no recognized reduction: fan-out needs an associative \
accumulate into one name, or — with `#[par_order_free]` — a bare `acc.push(..)`",
        )
    }

    /// Recognize the conditional collect shape:
    ///
    ///   if cond { acc.push(EXPR); }
    ///   if cond { acc.push(EXPR); } else { /* empty */ }
    ///
    /// Returns `Some(acc_name)` when the if-stmt wraps a single
    /// `acc.push(_)` method call. Like the conditional-acc-update
    /// helper, the else-branch must be absent OR an empty block; a
    /// two-arm version (push different values in each arm) is left to
    /// a follow-on if a workload surfaces it. The condition is **not**
    /// required to be acc-free here — `acc.len()` queries inside the
    /// condition are workload-relative but never read partial state
    /// across workers, since each worker's local Vec is independent
    /// until the final concat. The combine model treats every push as
    /// contributing one element to the parent's Vec; ordering is
    /// already worker-driven, so the condition's per-iter timing
    /// doesn't add an extra ordering hazard.
    pub(super) fn conditional_collect_shape(&self, expr: &Expr) -> Option<String> {
        let ExprKind::If {
            condition: _,
            then_block,
            else_branch,
        } = &expr.kind
        else {
            return None;
        };
        if let Some(else_expr) = else_branch {
            let ExprKind::Block(b) = &else_expr.kind else {
                return None;
            };
            if !b.stmts.is_empty() || b.final_expr.is_some() {
                return None;
            }
        }
        if then_block.stmts.len() != 1 || then_block.final_expr.is_some() {
            return None;
        }
        let StmtKind::Expr(inner) = &then_block.stmts[0].kind else {
            return None;
        };
        collect_push_shape(inner)
    }

    /// Is a Collect-classified loop body **tabulate-shaped**: exactly one
    /// top-level bare `acc.push(EXPR)` per iteration — no conditional
    /// pushes, no second push, and `acc` mentioned nowhere else in the
    /// body (including `let` initializers and the push's own argument)?
    ///
    /// Tabulate lets workers write elements directly into a shared
    /// presized buffer at their global iteration index, so the invariant
    /// "iteration i produces exactly output element i" must be airtight:
    /// a body that could push more than once per iteration overflows its
    /// chunk view (and the push grow-path would `free` an interior
    /// pointer), and one that could skip a push leaves garbage holes.
    /// Skips can't happen — `continue` anywhere in the body already
    /// rejects the whole lowering via `block_has_early_exit` — so this
    /// check only has to bound the push count from above, which it does
    /// by requiring the single bare push to be the ONLY mention of `acc`.
    /// Mention-detection over-approximates via `collect_expr_reads` ∪
    /// `collect_expr_inner_writes` (an `Identifier(acc)` anywhere,
    /// receiver positions included, registers as a read). Any shape this
    /// declines still lowers through the partial-Vecs path — declining
    /// costs performance, never correctness.
    pub(super) fn collect_is_tabulate_shape(&self, body: &Block, acc: &str) -> bool {
        // A body-local rebinding of the accumulator name makes every
        // later mention ambiguous between the two; decline outright.
        let mut let_introduced: HashSet<String> = HashSet::new();
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut let_introduced);
                }
                StmtKind::LetUninit { name, .. } => {
                    let_introduced.insert(name.clone());
                }
                _ => {}
            }
        }
        if let_introduced.contains(acc) {
            return false;
        }

        let mentions_acc = |e: &Expr| -> bool {
            let mut names = HashSet::new();
            self.collect_expr_reads(e, &mut names);
            self.collect_expr_inner_writes(e, &mut names);
            names.contains(acc)
        };

        let mut bare_pushes = 0usize;
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Expr(expr) => {
                    if collect_push_shape(expr).as_deref() == Some(acc) {
                        bare_pushes += 1;
                        let ExprKind::MethodCall { args, .. } = &expr.kind else {
                            return false;
                        };
                        if mentions_acc(&args[0].value) {
                            return false;
                        }
                        continue;
                    }
                    // A conditional push means a variable per-iter count.
                    if self.conditional_collect_shape(expr).as_deref() == Some(acc) {
                        return false;
                    }
                    if mentions_acc(expr) {
                        return false;
                    }
                }
                StmtKind::Let { value, .. } => {
                    if mentions_acc(value) {
                        return false;
                    }
                }
                StmtKind::Assign { target, value }
                | StmtKind::CompoundAssign { target, value, .. } => {
                    if mentions_acc(target) || mentions_acc(value) {
                        return false;
                    }
                }
                // LetElse's else-block diverges (break/return), which
                // `block_has_early_exit` rejects downstream anyway;
                // Defer never reaches here (classify_loop_body returns
                // None); MultiAssign is desugared away. Decline all
                // three defensively rather than reasoning about them.
                StmtKind::LetElse { .. }
                | StmtKind::LetUninit { .. }
                | StmtKind::Defer { .. }
                | StmtKind::ErrDefer { .. }
                | StmtKind::MultiAssign { .. } => return false,
            }
        }
        if let Some(e) = &body.final_expr {
            if collect_push_shape(e).as_deref() == Some(acc) {
                bare_pushes += 1;
                let ExprKind::MethodCall { args, .. } = &e.kind else {
                    return false;
                };
                if mentions_acc(&args[0].value) {
                    return false;
                }
            } else if self.conditional_collect_shape(e).as_deref() == Some(acc) || mentions_acc(e) {
                // A conditional push (variable count) or any other
                // accumulator mention — decline.
                return false;
            }
        }
        bare_pushes == 1
    }

    /// Recognize the conditional-accumulator-update shape:
    ///
    ///   if cond { acc = acc + delta; }                              (1-arm)
    ///   if cond { acc OP= delta; }                                  (1-arm CompoundAssign)
    ///   if cond { acc = acc + delta; } else { /* empty */ }         (1-arm + empty else)
    ///   if cond { acc = acc + a; } else { acc = acc + b; }          (2-arm — added 2026-05-20)
    ///   if cond { acc OP= a; }     else { acc OP= b; }              (2-arm CompoundAssign)
    ///
    /// Returns `Some((acc_name, op))` when both arms (or the single
    /// then-arm with absent/empty else) update the same outer-scope
    /// accumulator with the same op. The transformation that justifies
    /// recognizing this as a reduction is:
    ///
    ///   1-arm: if cond { acc = acc + d }      ≡  acc = acc + (if cond { d } else { 0 })
    ///   2-arm: if cond { acc = acc + a }
    ///          else     { acc = acc + b }     ≡  acc = acc + (if cond { a } else { b })
    ///
    /// In both cases the per-iteration contribution is order-independent
    /// for any associative+commutative op with a known identity, so the
    /// par-reduce fan-out + combine model preserves the final value.
    ///
    /// Constraints checked:
    /// - The then-block is exactly one statement of the recognized
    ///   accumulator-update shape (Assign with `reduction_binary_shape`
    ///   match, or CompoundAssign with an allow-listed op).
    /// - The else-branch, if present, is either empty (1-arm shape) or
    ///   exactly one statement of the same update shape, writing the
    ///   *same* accumulator name with the *same* op (mixed ops like
    ///   `if c { acc += 1 } else { acc *= 2 }` are rejected — combine
    ///   ordering only commutes within one op).
    /// - The condition expression does NOT read the accumulator —
    ///   otherwise the per-iter decision depends on accumulator state
    ///   produced by earlier iterations, which is order-dependent and
    ///   not preserved by the fan-out / combine model. Delta expressions
    ///   are guarded transitively via `reduction_binary_shape` (which
    ///   requires acc to appear exactly once on the RHS, so the
    ///   non-acc operand is acc-free by construction) and via the
    ///   CompoundAssign arm's no-self-reference assumption.
    pub(super) fn conditional_acc_update_shape(
        &self,
        expr: &Expr,
    ) -> Option<(String, ReductionOp)> {
        let ExprKind::If {
            condition,
            then_block,
            else_branch,
        } = &expr.kind
        else {
            return None;
        };
        // The then-block must be exactly one accumulator-update stmt.
        let (acc_name, op) = single_stmt_block_as_acc_update(then_block)?;
        // The else-branch, when present, may be empty (1-arm shape) or a
        // single matching update for the same (acc, op).
        if let Some(else_expr) = else_branch {
            let ExprKind::Block(b) = &else_expr.kind else {
                return None;
            };
            if b.final_expr.is_some() {
                return None;
            }
            match b.stmts.len() {
                0 => { /* empty else — 1-arm shape with explicit empty else */ }
                1 => {
                    let (else_acc, else_op) = single_stmt_as_acc_update(&b.stmts[0])?;
                    if else_acc != acc_name || else_op != op {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        // Final guard: condition must not reference the accumulator.
        let mut cond_reads: HashSet<String> = HashSet::new();
        self.collect_expr_reads(condition, &mut cond_reads);
        if cond_reads.contains(&acc_name) {
            return None;
        }
        Some((acc_name, op))
    }
}
