//! Expression read collection — which places an expression reads.
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, 2026-08-16). Lives in a sibling
//! `impl super::ConcurrencyChecker` block; methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    pub(super) fn collect_expr_reads(&self, expr: &Expr, reads: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Identifier(name) => {
                reads.insert(name.clone());
            }
            // `self` (a `ref self`/`mut ref self` receiver) reads through the
            // canonical name "self" — the same name `collect_assign_target_defines`
            // records for a `self.field = …` write and a mutating `self.method()`
            // call. Without this arm a `self.field` read recorded nothing, so a
            // statement reading `self` after a `mut ref self` method mutated it
            // showed "no data dependency" and the auto-parallelizer raced the
            // two (self-hosting #8: the lexer's `skip_whitespace()` then
            // `self.start = self.pos`).
            ExprKind::SelfValue => {
                reads.insert("self".to_string());
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Pipe { left, right } => {
                self.collect_expr_reads(left, reads);
                self.collect_expr_reads(right, reads);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.collect_expr_reads(left, reads);
                self.collect_expr_reads(right, reads);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.collect_expr_reads(operand, reads);
            }
            ExprKind::Call { callee, args } => {
                self.collect_expr_reads(callee, reads);
                for arg in args {
                    self.collect_expr_reads(&arg.value, reads);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.collect_expr_reads(object, reads);
                for arg in args {
                    self.collect_expr_reads(&arg.value, reads);
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_reads(object, reads);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_reads(object, reads);
                self.collect_expr_reads(index, reads);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_reads(object, reads);
                if let Some(args) = args {
                    for arg in args {
                        self.collect_expr_reads(&arg.value, reads);
                    }
                }
            }
            ExprKind::Block(block) | ExprKind::Comptime(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Seq(block)
            | ExprKind::Par(block) => {
                self.collect_block_reads(block, reads);
            }
            ExprKind::Lock { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_reads(condition, reads);
                self.collect_block_reads(then_block, reads);
                if let Some(e) = else_branch {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_reads(value, reads);
                self.collect_block_reads(then_block, reads);
                if let Some(e) = else_branch {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.collect_expr_reads(scrutinee, reads);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.collect_expr_reads(guard, reads);
                    }
                    self.collect_expr_reads(&arm.body, reads);
                }
            }
            ExprKind::While {
                condition, body, ..
            }
            | ExprKind::For {
                iterable: condition,
                body,
                ..
            } => {
                self.collect_expr_reads(condition, reads);
                self.collect_block_reads(body, reads);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_reads(value, reads);
                self.collect_block_reads(body, reads);
            }
            ExprKind::Loop { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_reads(body, reads);
            }
            ExprKind::Closure { body, .. } => {
                self.collect_expr_reads(body, reads);
            }
            ExprKind::Return(Some(inner)) => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Break {
                value: Some(inner), ..
            } => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => {
                for e in exprs {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_reads(value, reads);
                self.collect_expr_reads(count, reads);
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::MapLiteral(entries) => {
                for (k, v) in entries {
                    self.collect_expr_reads(k, reads);
                    self.collect_expr_reads(v, reads);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_reads(&f.value, reads);
                }
                if let Some(s) = spread {
                    self.collect_expr_reads(s, reads);
                }
            }
            ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_reads(inner, reads);
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_reads(s, reads);
                }
                if let Some(e) = end {
                    self.collect_expr_reads(e, reads);
                }
            }
            ExprKind::Path { segments, .. } => {
                // A path like Mod::val — the first segment could be a variable
                if let Some(first) = segments.first() {
                    reads.insert(first.clone());
                }
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.collect_expr_reads(&b.value, reads);
                }
                self.collect_block_reads(body, reads);
            }
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let ParsedInterpolationPart::Expr(inner, _) = part {
                        self.collect_expr_reads(inner, reads);
                    }
                }
            }
            // Leaf expressions that don't read variables
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_) | ExprKind::ByteStringLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            // NOTE: `ExprKind::SelfValue` is handled explicitly above (records
            // the read of "self") — it is intentionally NOT in this no-op leaf
            // group (self-hosting #8).
            | ExprKind::SelfType
            | ExprKind::Continue { .. }
            | ExprKind::Return(None)
            | ExprKind::Break { value: None, .. }
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
        }
    }

    /// Walk an expression's nested blocks and record any outer-scope
    /// names written via `Assign` / `CompoundAssign` into `writes`.
    /// Critical for the auto-parallelizer's data-dependency reasoning:
    /// a `for v in coll { if v > m { m = v; } }` expression-statement
    /// must record `m` as a write so subsequent stmts that read `m`
    /// serialize against it. Local variables shadowed inside nested
    /// blocks (introduced by `let`) are intentionally still recorded
    /// here — the conflict check at the call site uses
    /// `Set::intersect` over a flat name set, so non-disjoint local
    /// shadowing of the same name produces an over-serialization that
    /// is correct (and conservative) rather than incorrect.
    pub(super) fn collect_expr_inner_writes(&self, expr: &Expr, writes: &mut HashSet<String>) {
        match &expr.kind {
            ExprKind::Block(block) | ExprKind::Seq(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.collect_expr_inner_writes(condition, writes);
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_block_inner_writes(then_block, writes);
                if let Some(e) = else_branch {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                // B-2026-07-12-5: the SCRUTINEE (and arm guards) can mutate —
                // `match b.take() { .. }` where `take(mut ref self)` pops the
                // receiver. Without walking them the auto-par data-dependency
                // check missed the receiver write, so it raced the statements.
                self.collect_expr_inner_writes(scrutinee, writes);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.collect_expr_inner_writes(g, writes);
                    }
                    self.collect_expr_inner_writes(&arm.body, writes);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.collect_expr_inner_writes(condition, writes);
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::Loop { body, .. } => self.collect_block_inner_writes(body, writes),
            ExprKind::For { body, .. } => self.collect_block_inner_writes(body, writes),
            ExprKind::Unsafe(block) | ExprKind::Par(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                // A method whose declared/inferred effects include any
                // non-pure verb (`Writes`, `Allocates`, `Sends`, `Receives`,
                // `Panics`) is treated as mutating its receiver — record the
                // receiver's root identifier as a write so the
                // data-dependency check serializes it against sibling
                // reads of the same name. Without this, two `a.push(...)`
                // / `a.push(...)` calls are seen as read-only on `a` and
                // the auto-parallelizer races them on shared Vec state.
                //
                // A `mut ref self` method ALSO mutates its receiver — through
                // the borrow — even when it carries no resource-effect verb.
                // A parser cursor advance (`self.pos = self.pos + 1` inside
                // `self.parse_block()`) writes a plain scalar field, which is
                // ownership-level mutation with NO `writes(Resource)` effect,
                // so the effect heuristic above misses it. Without the
                // receiver-mode check, three sequential cursor-advancing calls
                // (`self.parse_expr_bp(0)`, `self.parse_block()`,
                // `self.parse_else()` in `parse_if`) recorded no write on
                // `self`, so the data-dependency check saw them as independent
                // and the auto-parallelizer raced them through `karac_par_run`
                // — corrupting the shared parser state (B-2026-07-09-12: the
                // self-hosted parser SEGV'd on every `if`/`loop`/`for`/`while`).
                if self.method_effects_imply_receiver_mutation(method)
                    || self.method_receiver_is_mut_ref(method)
                {
                    self.collect_assign_target_defines(object, writes);
                }
                self.collect_expr_inner_writes(object, writes);
                for arg in args {
                    self.collect_expr_inner_writes(&arg.value, writes);
                }
            }
            ExprKind::Call { callee, args } => {
                // A free-function call mutates caller-visible state through
                // `mut ref T` / `mut Slice[T]` parameters — record each
                // mutably-passed argument's root identifier as a write so
                // subsequent statements that read it serialize against the
                // call. Without this arm, `add_one(mut out); println(out.len())`
                // in `main` records no write on `out`, the dependency check
                // sees two reads, and the auto-parallelizer races the two
                // statements via `karac_par_run` — with `out` captured into
                // the par env BY VALUE, so the callee's header writeback
                // (len/cap/data after a push-grow) lands in the env copy and
                // the caller observes a stale empty Vec (kata 22, 2026-06-06).
                //
                // Two detection paths, OR'd:
                //   - the call-site `mut` marker (`f(mut x)` — required for
                //     fresh owned bindings per design.md Feature 4 Part 1½);
                //   - the callee's declared param mode when its body is in
                //     this program (`function_bodies`) — covers the unmarked
                //     mut-ref forwarding form (`x` already `mut ref` in
                //     scope) and any future marker-elision sites.
                // B-2026-08-08-17: calling a `let`-bound CLOSURE mutates what it
                // captured, and the capture is invisible here — no `mut` marker
                // on the argument, no declared `mut ref` param to inspect. Walk
                // the closure's body so its writes (`buf.push_str(s)` on the
                // captured `buf`) are attributed to this call site. Without it
                // the parallelizer classed `append(s)` and `println(buf.len())`
                // as independent and raced them, with `buf` copied into the par
                // env by value — the write landed in the real binding and the
                // sibling branch printed the stale snapshot.
                if let Some(name) = self.extract_callee_name(callee) {
                    let recursing = self.closure_expansion_stack.borrow().contains(&name);
                    if !recursing {
                        if let Some(body) = self.closure_bodies.get(&name).copied() {
                            self.closure_expansion_stack.borrow_mut().push(name.clone());
                            self.collect_expr_inner_writes(body, writes);
                            self.closure_expansion_stack.borrow_mut().pop();
                        }
                    }
                }
                let callee_params = self
                    .extract_callee_name(callee)
                    .and_then(|n| self.function_bodies.get(&n))
                    .map(|f| f.params.as_slice());
                for (i, arg) in args.iter().enumerate() {
                    let param = callee_params.and_then(|ps| ps.get(i));
                    let param_is_mut_ref = param.is_some_and(|p| {
                        matches!(p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_))
                    });
                    // B-2026-08-11-27. A `shared struct` / `shared enum`
                    // argument is REFERENCE SEMANTICS, so a callee that mutates
                    // its parameter mutates the CALLER'S object — but it is
                    // passed by value, with no `mut` marker (markers are the
                    // `mut ref T` rule, design.md Feature 4 Part 1½) and no
                    // `MutRef` in the signature. Both gates above therefore miss
                    // it, and the call recorded NO write.
                    //
                    // That was a miscompile, not a missed optimization:
                    //
                    //     shared struct S { mut grads: Vec[Tensor[f32, [?]]] }
                    //     go(s);                 // pushes + overwrites s.grads[0]
                    //     let r = s.grads[0];    // reads it
                    //
                    // read and write looked independent, auto-par raced them,
                    // and the shipped default `karac build` printed `0` for `1`
                    // on ~2% of runs and SEGV'd on ~1.3% — the wrong answer
                    // being the worse half, since it exits 0 and propagates.
                    //
                    // Gated on the callee actually writing that parameter, so a
                    // read-only shared argument still parallelizes. Same shape
                    // as the closure-body walk above (B-2026-08-08-17): the
                    // mutation is real but invisible at the call site, so the
                    // callee's body is where it has to be found.
                    let param_is_mutated_shared = param.is_some_and(|p| {
                        if !self.type_is_shared(&p.ty) {
                            return false;
                        }
                        match &p.pattern.kind {
                            PatternKind::Binding(n) => self.callee_writes_param(callee, n),
                            // A destructured shared parameter cannot be matched
                            // by name against the callee's write set, so fall
                            // back to serializing: unsound silence is the bug
                            // this row is about.
                            _ => true,
                        }
                    });
                    if arg.mut_marker || param_is_mut_ref || param_is_mutated_shared {
                        self.collect_assign_target_defines(&arg.value, writes);
                    }
                    self.collect_expr_inner_writes(&arg.value, writes);
                }
            }
            // B-2026-07-12-5: a mutating method call (`b.push(x)`, `b.take()`)
            // can hide in ANY value-expression position, not just a bare
            // statement or a block. The auto-parallelizer's data-dependency
            // check reached the `MethodCall` / `Call` arms above only through
            // the block/branch arms, so a mutation nested in an f-string
            // interpolation, a `Some(..)` / tuple / index / binary / … missed
            // the receiver write and the statements were classed independent
            // and RACED — with the receiver captured BY VALUE into the par env,
            // so the mutation landed in a discarded copy (silent wrong answer:
            // `println(f"{b.push_len(x)}")` in a loop; `match b.take()`;
            // `Some(b.pop())`). Recurse into every sub-expression so those arms
            // see the write. Safe: a write is recorded ONLY for a genuinely
            // mutating call (the receiver-mode / mut-arg gates), so this only
            // ever ADDS serialization — it can never introduce a race.
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e, _) = part {
                        self.collect_expr_inner_writes(e, writes);
                    }
                }
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::NilCoalesce { left, right }
            | ExprKind::Pipe { left, right } => {
                self.collect_expr_inner_writes(left, writes);
                self.collect_expr_inner_writes(right, writes);
            }
            ExprKind::Unary { operand, .. } => self.collect_expr_inner_writes(operand, writes),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => {
                self.collect_expr_inner_writes(inner, writes);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.collect_expr_inner_writes(object, writes);
            }
            ExprKind::Index { object, index } => {
                self.collect_expr_inner_writes(object, writes);
                self.collect_expr_inner_writes(index, writes);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.collect_expr_inner_writes(object, writes);
                if let Some(args) = args {
                    for a in args {
                        self.collect_expr_inner_writes(&a.value, writes);
                    }
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.collect_expr_inner_writes(s, writes);
                }
                if let Some(e) = end {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::Tuple(items)
            | ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.collect_expr_inner_writes(e, writes);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.collect_expr_inner_writes(value, writes);
                self.collect_expr_inner_writes(count, writes);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.collect_expr_inner_writes(k, writes);
                    self.collect_expr_inner_writes(v, writes);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.collect_expr_inner_writes(&f.value, writes);
                }
                if let Some(s) = spread {
                    self.collect_expr_inner_writes(s, writes);
                }
            }
            ExprKind::Return(Some(e)) => self.collect_expr_inner_writes(e, writes),
            ExprKind::Break { value: Some(e), .. } => self.collect_expr_inner_writes(e, writes),
            ExprKind::Try(block) | ExprKind::Comptime(block) => {
                self.collect_block_inner_writes(block, writes);
            }
            ExprKind::LabeledBlock { body, .. } => {
                self.collect_block_inner_writes(body, writes);
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.collect_expr_inner_writes(mutex, writes);
                self.collect_block_inner_writes(body, writes);
            }
            _ => {}
        }
    }

    /// `true` when the type expression names a `shared struct` / `shared enum`
    /// (B-2026-08-11-27). Such a value is a reference-semantics handle: passing
    /// it BY VALUE still lets the callee mutate the caller's object, which is
    /// the whole point of `shared` and the reason the `mut ref` / `mut`-marker
    /// gates never fire for one.
    pub(super) fn type_is_shared(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Path(p) => p
                .segments
                .first()
                .is_some_and(|n| self.shared_type_names.contains(n)),
            _ => false,
        }
    }

    /// `true` when the named callee's body writes the parameter bound to
    /// `param_name` (B-2026-08-11-27).
    ///
    /// Gating on this rather than on the shared type alone keeps a READ-ONLY
    /// shared argument parallelizable — passing a `shared` handle to a function
    /// that only reads it is common and safe, and blanket-serializing every
    /// shared argument would cost that. `collect_block_inner_writes` already
    /// covers both spellings the mutation can take: a direct
    /// `s.field[i] = …` assignment and a mutating method call like
    /// `s.field.push(…)`.
    ///
    /// Re-entry is guarded through the same name stack the closure walk uses,
    /// so a self- or mutually-recursive callee terminates instead of expanding
    /// forever. A guarded bail returns `false`, which matches the pre-existing
    /// behaviour for that (rare) shape rather than making it newly serial.
    pub(super) fn callee_writes_param(&self, callee: &Expr, param_name: &str) -> bool {
        let Some(name) = self.extract_callee_name(callee) else {
            return false;
        };
        if self.closure_expansion_stack.borrow().contains(&name) {
            return false;
        }
        let Some(func) = self.function_bodies.get(&name).copied() else {
            return false;
        };
        self.closure_expansion_stack.borrow_mut().push(name);
        let mut writes = HashSet::new();
        self.collect_block_inner_writes(&func.body, &mut writes);
        self.closure_expansion_stack.borrow_mut().pop();
        writes.contains(param_name)
    }

    /// Returns `true` if any callee key matching `<Type>.<method>` (or the
    /// bare `<method>`) carries an effect verb that implies mutation of
    /// the receiver state. Conservative: any non-pure verb counts, since
    /// the auto-parallelizer's job is to be sound, not maximally
    /// permissive. Lookup mirrors `collect_expr_effects`'s MethodCall arm.
    pub(super) fn method_effects_imply_receiver_mutation(&self, method: &str) -> bool {
        let suffix = format!(".{}", method);
        for (key, set) in &self.effects.inferred_effects {
            if (key == method || key.ends_with(&suffix)) && effect_set_has_nonpure_verb(set) {
                return true;
            }
        }
        for (key, decl) in &self.effects.declared_effects {
            if key != method && !key.ends_with(&suffix) {
                continue;
            }
            match decl {
                DeclaredEffects::Explicit(set) | DeclaredEffects::PolymorphicWithFixed(set) => {
                    if effect_set_has_nonpure_verb(set) {
                        return true;
                    }
                }
                // Unknown effects → assume mutating.
                DeclaredEffects::Polymorphic => return true,
                DeclaredEffects::None => {}
            }
        }
        false
    }

    /// Returns `true` if any method named `method` (matched as `<Type>.<method>`)
    /// declares a `mut ref self` receiver. Such a method CAN mutate the receiver
    /// through the borrow independent of any resource-effect verb, so a call to
    /// it must be treated as writing its receiver for the auto-parallelizer's
    /// data-dependency gate. Conservative: matches on the method name across all
    /// types (like `method_effects_imply_receiver_mutation`), which can only
    /// over-serialize, never under-serialize — the sound direction for auto-par.
    /// This is the receiver-mode counterpart to the effect-verb heuristic and
    /// catches plain-field mutation (a parser cursor advance) that carries no
    /// `writes(Resource)` effect (B-2026-07-09-12).
    pub(super) fn method_receiver_is_mut_ref(&self, method: &str) -> bool {
        let suffix = format!(".{}", method);
        self.method_bodies.iter().any(|(key, f)| {
            (key == method || key.ends_with(&suffix))
                && matches!(f.self_param, Some(SelfParam::MutRef))
        })
    }

    /// Resolve a call's callee expression to its declared parameter list, using
    /// the same free-fn / associated-fn rule as `stmt_fanout_args_safe`. `None`
    /// for a computed callee or an extern one (body absent from this program).
    pub(super) fn resolve_callee_params(&self, callee: &Expr) -> Option<&[Param]> {
        match &callee.kind {
            ExprKind::Identifier(n) => self.function_bodies.get(n).map(|f| f.params.as_slice()),
            ExprKind::Path { segments, .. } if segments.len() == 1 => self
                .function_bodies
                .get(&segments[0])
                .map(|f| f.params.as_slice()),
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                resolve_assoc_callee(segments, &self.method_bodies).map(|f| f.params.as_slice())
            }
            _ => None,
        }
    }

    /// B-2026-07-23-20 reduction soundness gate. The reduction lowering runs the
    /// loop body on MULTIPLE worker threads concurrently. A call that passes a
    /// `mut ref` / `mut Slice` argument (or invokes a `mut ref self` method)
    /// whose place-root is a binding declared OUTSIDE the loop is a
    /// LOOP-INVARIANT SHARED MUTABLE: every iteration writes the same buffer
    /// *through the callee*, so the iterations are NOT independent and parallel
    /// workers race on it — nondeterministic WRONG output from the default
    /// `karac build` (the `while w < n { sink = sink + kern(base, w, width, mut
    /// a, mut b) }` KMP-scratch shape, where `a`/`b` are allocated once before
    /// the loop). The existing reduction gates only see DIRECT `Assign` writes
    /// (`collect_expr_inner_writes`); a mutation performed inside the callee is
    /// invisible to them, which is the soundness hole. Decline the reduction
    /// (the loop lowers sequentially) when the body has such a call.
    ///
    /// A mutable borrow rooted at a PER-ITERATION-FRESH binding (`let mut tmp`
    /// declared inside the loop body) is safe — each worker owns its own copy —
    /// so `locals` is threaded in statement order and only outside-the-body
    /// roots disqualify. Read-only `ref T` arguments (immutable shared data,
    /// e.g. `base`) are deliberately NOT flagged: every worker only reads them.
    pub(super) fn loop_body_shares_outer_mut_borrow(&self, body: &Block) -> bool {
        self.block_shares_outer_mut_borrow(body, &HashSet::new())
    }

    pub(super) fn block_shares_outer_mut_borrow(
        &self,
        block: &Block,
        outer: &HashSet<String>,
    ) -> bool {
        let mut locals = outer.clone();
        for stmt in &block.stmts {
            if self.stmt_shares_outer_mut_borrow(stmt, &locals) {
                return true;
            }
            // Extend scope AFTER checking the statement's own exprs: a `let x =
            // f(mut outer)` RHS is evaluated before `x` is in scope, and a body-
            // local `let a` that shadows an outer `a` must not retroactively make
            // an EARLIER `mut a` call look local.
            match &stmt.kind {
                StmtKind::Let { pattern, .. } | StmtKind::LetElse { pattern, .. } => {
                    self.collect_pattern_bindings(pattern, &mut locals);
                }
                StmtKind::LetUninit { name, .. } => {
                    locals.insert(name.clone());
                }
                _ => {}
            }
        }
        if let Some(fe) = &block.final_expr {
            if self.expr_shares_outer_mut_borrow(fe, &locals) {
                return true;
            }
        }
        false
    }

    pub(super) fn stmt_shares_outer_mut_borrow(
        &self,
        stmt: &Stmt,
        locals: &HashSet<String>,
    ) -> bool {
        match &stmt.kind {
            StmtKind::Expr(e) => self.expr_shares_outer_mut_borrow(e, locals),
            StmtKind::Let { value, .. } => self.expr_shares_outer_mut_borrow(value, locals),
            StmtKind::LetElse {
                value, else_block, ..
            } => {
                self.expr_shares_outer_mut_borrow(value, locals)
                    || self.block_shares_outer_mut_borrow(else_block, locals)
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.expr_shares_outer_mut_borrow(target, locals)
                    || self.expr_shares_outer_mut_borrow(value, locals)
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.block_shares_outer_mut_borrow(body, locals)
            }
            _ => false,
        }
    }

    /// Comprehensive recursive walk — mirrors `collect_expr_reads`'s traversal
    /// so no call position is missed (a missed call would leave a genuine race
    /// parallelized). At each `Call`/`MethodCall` node, flag a mutable borrow of
    /// an outside-the-loop place-root; then recurse into children, threading
    /// `locals` through binding-introducing forms (loop patterns, match arms,
    /// closure params, nested blocks).
    pub(super) fn expr_shares_outer_mut_borrow(
        &self,
        expr: &Expr,
        locals: &HashSet<String>,
    ) -> bool {
        let root_is_outer = |arg: &Expr| -> bool {
            match place_root(arg) {
                Some(root) => !locals.contains(&root),
                None => false,
            }
        };
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                let params = self.resolve_callee_params(callee);
                for (i, arg) in args.iter().enumerate() {
                    if (arg.mut_marker || param_is_mut_borrow(params, i))
                        && root_is_outer(&arg.value)
                    {
                        return true;
                    }
                }
                if self.expr_shares_outer_mut_borrow(callee, locals) {
                    return true;
                }
                args.iter()
                    .any(|a| self.expr_shares_outer_mut_borrow(&a.value, locals))
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                if self.method_receiver_is_mut_ref(method) && root_is_outer(object) {
                    return true;
                }
                if args.iter().any(|a| a.mut_marker && root_is_outer(&a.value)) {
                    return true;
                }
                if self.expr_shares_outer_mut_borrow(object, locals) {
                    return true;
                }
                args.iter()
                    .any(|a| self.expr_shares_outer_mut_borrow(&a.value, locals))
            }
            ExprKind::Binary { left, right, .. }
            | ExprKind::Pipe { left, right }
            | ExprKind::NilCoalesce { left, right } => {
                self.expr_shares_outer_mut_borrow(left, locals)
                    || self.expr_shares_outer_mut_borrow(right, locals)
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.expr_shares_outer_mut_borrow(operand, locals)
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.expr_shares_outer_mut_borrow(object, locals)
            }
            ExprKind::Index { object, index } => {
                self.expr_shares_outer_mut_borrow(object, locals)
                    || self.expr_shares_outer_mut_borrow(index, locals)
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.expr_shares_outer_mut_borrow(object, locals)
                    || args.iter().flatten().any(|a| {
                        (a.mut_marker && root_is_outer(&a.value))
                            || self.expr_shares_outer_mut_borrow(&a.value, locals)
                    })
            }
            ExprKind::Block(block)
            | ExprKind::Comptime(block)
            | ExprKind::Unsafe(block)
            | ExprKind::Try(block)
            | ExprKind::Seq(block)
            | ExprKind::Par(block) => self.block_shares_outer_mut_borrow(block, locals),
            ExprKind::Lock { body, .. } => self.block_shares_outer_mut_borrow(body, locals),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr_shares_outer_mut_borrow(condition, locals)
                    || self.block_shares_outer_mut_borrow(then_block, locals)
                    || else_branch
                        .as_ref()
                        .is_some_and(|e| self.expr_shares_outer_mut_borrow(e, locals))
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                if self.expr_shares_outer_mut_borrow(value, locals) {
                    return true;
                }
                let mut inner = locals.clone();
                self.collect_pattern_bindings(pattern, &mut inner);
                self.block_shares_outer_mut_borrow(then_block, &inner)
                    || else_branch
                        .as_ref()
                        .is_some_and(|e| self.expr_shares_outer_mut_borrow(e, locals))
            }
            ExprKind::Match { scrutinee, arms } => {
                if self.expr_shares_outer_mut_borrow(scrutinee, locals) {
                    return true;
                }
                arms.iter().any(|arm| {
                    let mut inner = locals.clone();
                    self.collect_pattern_bindings(&arm.pattern, &mut inner);
                    arm.guard
                        .as_ref()
                        .is_some_and(|g| self.expr_shares_outer_mut_borrow(g, &inner))
                        || self.expr_shares_outer_mut_borrow(&arm.body, &inner)
                })
            }
            ExprKind::While {
                condition: head,
                body,
                ..
            }
            | ExprKind::For {
                iterable: head,
                body,
                ..
            } => {
                if self.expr_shares_outer_mut_borrow(head, locals) {
                    return true;
                }
                let mut inner = locals.clone();
                if let ExprKind::For { pattern, .. } = &expr.kind {
                    self.collect_pattern_bindings(pattern, &mut inner);
                }
                self.block_shares_outer_mut_borrow(body, &inner)
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                if self.expr_shares_outer_mut_borrow(value, locals) {
                    return true;
                }
                let mut inner = locals.clone();
                self.collect_pattern_bindings(pattern, &mut inner);
                self.block_shares_outer_mut_borrow(body, &inner)
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.block_shares_outer_mut_borrow(body, locals)
            }
            ExprKind::Closure { params, body, .. } => {
                let mut inner = locals.clone();
                for p in params {
                    self.collect_pattern_bindings(&p.pattern, &mut inner);
                }
                self.expr_shares_outer_mut_borrow(body, &inner)
            }
            ExprKind::Return(Some(inner))
            | ExprKind::Break {
                value: Some(inner), ..
            } => self.expr_shares_outer_mut_borrow(inner, locals),
            ExprKind::Tuple(exprs) | ExprKind::ArrayLiteral(exprs) => exprs
                .iter()
                .any(|e| self.expr_shares_outer_mut_borrow(e, locals)),
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.expr_shares_outer_mut_borrow(value, locals)
                    || self.expr_shares_outer_mut_borrow(count, locals)
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => items
                .iter()
                .any(|e| self.expr_shares_outer_mut_borrow(e, locals)),
            ExprKind::MapLiteral(entries) => entries.iter().any(|(k, v)| {
                self.expr_shares_outer_mut_borrow(k, locals)
                    || self.expr_shares_outer_mut_borrow(v, locals)
            }),
            ExprKind::StructLiteral { fields, spread, .. } => {
                fields
                    .iter()
                    .any(|f| self.expr_shares_outer_mut_borrow(&f.value, locals))
                    || spread
                        .as_ref()
                        .is_some_and(|s| self.expr_shares_outer_mut_borrow(s, locals))
            }
            ExprKind::Cast { expr: inner, .. } => self.expr_shares_outer_mut_borrow(inner, locals),
            ExprKind::Range { start, end, .. } => {
                start
                    .as_ref()
                    .is_some_and(|s| self.expr_shares_outer_mut_borrow(s, locals))
                    || end
                        .as_ref()
                        .is_some_and(|e| self.expr_shares_outer_mut_borrow(e, locals))
            }
            ExprKind::Providers { bindings, body } => {
                bindings
                    .iter()
                    .any(|b| self.expr_shares_outer_mut_borrow(&b.value, locals))
                    || self.block_shares_outer_mut_borrow(body, locals)
            }
            ExprKind::InterpolatedStringLit(parts) => parts.iter().any(|part| {
                matches!(part, ParsedInterpolationPart::Expr(inner, _)
                    if self.expr_shares_outer_mut_borrow(inner, locals))
            }),
            // Leaf / no-call forms.
            _ => false,
        }
    }

    /// Walk a block's statements and record any outer-scope names
    /// written via `Assign` / `CompoundAssign` (plus inner writes of
    /// nested expressions). Companion to `collect_expr_inner_writes`.
    pub(super) fn collect_block_inner_writes(&self, block: &Block, writes: &mut HashSet<String>) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } => {
                    self.collect_assign_target_defines(target, writes);
                }
                StmtKind::Expr(e) => self.collect_expr_inner_writes(e, writes),
                StmtKind::Let { value, .. } => self.collect_expr_inner_writes(value, writes),
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_inner_writes(value, writes);
                    self.collect_block_inner_writes(else_block, writes);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_inner_writes(body, writes);
                }
                _ => {}
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_inner_writes(e, writes);
        }
    }

    pub(super) fn collect_block_reads(&self, block: &Block, reads: &mut HashSet<String>) {
        for stmt in &block.stmts {
            match &stmt.kind {
                StmtKind::MultiAssign { .. } => unreachable!(
                    "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
                ),
                StmtKind::Let { value, .. } => self.collect_expr_reads(value, reads),
                StmtKind::LetUninit { .. } => {}
                StmtKind::LetElse {
                    value, else_block, ..
                } => {
                    self.collect_expr_reads(value, reads);
                    self.collect_block_reads(else_block, reads);
                }
                StmtKind::Assign { target, value } => {
                    self.collect_expr_reads(target, reads);
                    self.collect_expr_reads(value, reads);
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.collect_expr_reads(target, reads);
                    self.collect_expr_reads(value, reads);
                }
                StmtKind::Expr(e) => self.collect_expr_reads(e, reads),
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    self.collect_block_reads(body, reads);
                }
            }
        }
        if let Some(e) = &block.final_expr {
            self.collect_expr_reads(e, reads);
        }
    }
}
