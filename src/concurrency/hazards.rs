//! Capture-hazard analysis: container locals, heap-owning types, and
//! move-hazard classification feeding parallel-group admission.
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, slice 2). Sibling `impl super::…` block;
//! methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    /// Find groups of statements that can run in parallel.
    /// Uses a greedy approach: walk statements in order, grouping consecutive
    /// independent statements.
    /// Collect the names of locals bound to a HEAP-OWNING CONTAINER type
    /// (`Vec` / `String` / `Map` / `Set` / `SortedMap` / `SortedSet`) anywhere
    /// in `block`, recursively. Classification prefers the `let`'s explicit
    /// annotation; unannotated bindings use the typechecker's recorded pattern
    /// type (`pattern_binding_types`, keyed by the pattern span). Name-based
    /// on purpose: a same-named container binding in ANY scope conservatively
    /// marks the name (over-marking only de-parallelizes — it can never
    /// introduce a race). Feeds `ParallelGroup::captured_container_mutations`
    /// (B-2026-07-15-2).
    pub(super) fn collect_container_locals(&self, block: &Block) -> HashSet<String> {
        fn type_name_is_container(name: &str) -> bool {
            let head = name.split(['[', ' ']).next().unwrap_or("");
            // VecDeque added for B-2026-07-31-35: it shares Vec's
            // `{ptr, len, cap}` header, so a lost branch-local mutation
            // orphans the realloc'd buffer exactly like the write-only
            // `Vec[shared]` push (B-2026-07-15-2), and the head-index
            // pop_front lowering additionally relies on deque-mutating
            // groups being forced sequential so the two auto-par lanes
            // never disagree on what the header's `len` field means.
            matches!(
                head,
                "Vec" | "String" | "Map" | "Set" | "SortedMap" | "SortedSet" | "VecDeque"
            )
        }
        // B-2026-07-31-41: a local whose type carries a user `impl Drop` is
        // drop-OBSERVABLE — displacement (`x = Res{..}` over a live value)
        // and scope exit run the user body, so a lost branch-local mutation
        // is never a dead write: the parent's scope-exit fire reads the
        // value the branch never published, and the lanes disagree on which
        // value's body fires where (default build printed `drop 6` at the
        // assignment and `drop 5` after `end`, while every sequential lane
        // prints `drop 5` then `drop 6`). Same never-a-dead-write argument
        // that put containers in this set (B-2026-07-15-2) — the field name
        // `captured_container_mutations` predates the widening.
        fn type_name_is_drop_observable(this: &ConcurrencyChecker, name: &str) -> bool {
            let head = name.split(['[', ' ']).next().unwrap_or("");
            type_name_is_container(name) || this.drop_observable_type_names.contains(head)
        }
        fn type_expr_is_container(this: &ConcurrencyChecker, te: &TypeExpr) -> bool {
            match &te.kind {
                TypeKind::Path(p) => p
                    .segments
                    .last()
                    .is_some_and(|s| type_name_is_drop_observable(this, s)),
                _ => false,
            }
        }
        fn walk_block(this: &ConcurrencyChecker, block: &Block, out: &mut HashSet<String>) {
            for stmt in &block.stmts {
                walk_stmt(this, stmt, out);
            }
            if let Some(fe) = &block.final_expr {
                walk_expr(this, fe, out);
            }
        }
        fn classify_let(
            this: &ConcurrencyChecker,
            pattern: &Pattern,
            ty: &Option<TypeExpr>,
            value: Option<&Expr>,
            out: &mut HashSet<String>,
        ) {
            let PatternKind::Binding(name) = &pattern.kind else {
                return;
            };
            let is_container = match ty {
                Some(te) => type_expr_is_container(this, te),
                None => {
                    this.types
                        .and_then(|t| {
                            t.pattern_binding_types
                                .get(&SpanKey::from_span(&pattern.span))
                        })
                        .is_some_and(|n| type_name_is_drop_observable(this, n))
                        // Unannotated `let mut x = Res { .. }` — the struct
                        // literal names the type directly, independent of
                        // whether the binding-types table covered this span
                        // (B-2026-07-31-41).
                        || value.is_some_and(|v| {
                            matches!(&v.kind, ExprKind::StructLiteral { path, .. }
                                if path.last().is_some_and(|n|
                                    this.drop_observable_type_names.contains(n.as_str())))
                        })
                }
            };
            if is_container {
                out.insert(name.clone());
            }
        }
        fn walk_stmt(this: &ConcurrencyChecker, stmt: &Stmt, out: &mut HashSet<String>) {
            match &stmt.kind {
                StmtKind::Let {
                    pattern, ty, value, ..
                } => {
                    classify_let(this, pattern, ty, Some(value), out);
                    walk_expr(this, value, out);
                }
                StmtKind::LetUninit { name, ty, .. } => {
                    if type_expr_is_container(this, ty) {
                        out.insert(name.clone());
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    ty,
                    value,
                    else_block,
                    ..
                } => {
                    classify_let(this, pattern, ty, Some(value), out);
                    walk_expr(this, value, out);
                    walk_block(this, else_block, out);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    walk_block(this, body, out);
                }
                StmtKind::Assign { target, value } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for e in targets.iter().chain(values.iter()) {
                        walk_expr(this, e, out);
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::Expr(e) => walk_expr(this, e, out),
            }
        }
        fn walk_expr(this: &ConcurrencyChecker, e: &Expr, out: &mut HashSet<String>) {
            match &e.kind {
                ExprKind::Block(b)
                | ExprKind::Seq(b)
                | ExprKind::Par(b)
                | ExprKind::Unsafe(b)
                | ExprKind::Try(b)
                | ExprKind::Comptime(b) => walk_block(this, b, out),
                ExprKind::LabeledBlock { body, .. } => walk_block(this, body, out),
                ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } => {
                    walk_expr(this, condition, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::IfLet {
                    value,
                    then_block,
                    else_branch,
                    ..
                } => {
                    walk_expr(this, value, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::Match { scrutinee, arms } => {
                    walk_expr(this, scrutinee, out);
                    for arm in arms {
                        walk_expr(this, &arm.body, out);
                    }
                }
                ExprKind::While { body, .. }
                | ExprKind::WhileLet { body, .. }
                | ExprKind::For { body, .. }
                | ExprKind::Loop { body, .. } => walk_block(this, body, out),
                ExprKind::Lock { body, .. } => walk_block(this, body, out),
                ExprKind::Providers { body, .. } => walk_block(this, body, out),
                _ => {}
            }
        }
        let mut out = HashSet::new();
        walk_block(self, block, &mut out);
        out
    }

    /// Locals whose type OWNS non-RC heap — a bare container
    /// (`Vec`/`String`/`Map`/`Set`/`SortedMap`/`SortedSet`) or an
    /// `Option[..]`/`Result[..]` whose payload carries one. These are the
    /// bindings for which a par-branch capture is a bit-copy of an OWNING
    /// header: a consuming use inside the branch (payload move-out, owned
    /// call arg) frees heap the parent's scope-exit cleanup still references
    /// (B-2026-07-16-19). `shared` payloads are excluded — RC capture
    /// bookkeeping keeps each side's counts balanced, so a consumed
    /// `Option[SharedNode]` capture is safe.
    ///
    /// Classification mirrors `collect_container_locals`: the declared
    /// annotation when present, else the typechecker's recorded binding type
    /// (string form). `HashMap` value is `true` when the type is an
    /// `Option`/`Result` wrapper (whose combinator METHODS consume `self`),
    /// `false` for a bare container (whose methods are ref-self dominated).
    /// Does the named user type (enum/struct) transitively OWN non-RC heap
    /// — a `String`/`Vec`/`Map`/`Set`/… payload or field, directly or
    /// through a nested non-`shared` aggregate? Drives the B-2026-07-22-9
    /// move-hazard classification: a bare owned-heap enum/struct binding
    /// produced in a par branch and then MOVED (`let c = a`, owned call
    /// arg) double-frees through the par-return writeback, exactly like the
    /// `Option`/`Result` payload case (B-2026-07-16-19). `shared`/`par`
    /// aggregates are RC-managed (balanced retain/release across the branch
    /// bit-copy) and excluded. `visited` guards recursive enums
    /// (`enum List { Nil, Cons(i64, List) }`).
    pub(super) fn named_type_owns_heap(&self, name: &str, visited: &mut HashSet<String>) -> bool {
        if name.is_empty() || !visited.insert(name.to_string()) {
            return false;
        }
        for item in &self.program.items {
            match item {
                Item::EnumDef(e) if e.name == name => {
                    if e.is_shared || e.is_par {
                        return false;
                    }
                    return e.variants.iter().any(|v| match &v.kind {
                        VariantKind::Unit => false,
                        VariantKind::Tuple(tys) => {
                            tys.iter().any(|t| self.type_expr_owns_heap(t, visited))
                        }
                        VariantKind::Struct(fs) => {
                            fs.iter().any(|f| self.type_expr_owns_heap(&f.ty, visited))
                        }
                    });
                }
                Item::StructDef(s) if s.name == name => {
                    if s.is_shared || s.is_par {
                        return false;
                    }
                    return s
                        .fields
                        .iter()
                        .any(|f| self.type_expr_owns_heap(&f.ty, visited));
                }
                _ => {}
            }
        }
        false
    }

    /// Type-expr twin of [`Self::named_type_owns_heap`]: does this type own
    /// non-RC heap? A bare heap container, an `Option`/`Result` with a heap
    /// payload, or a user enum/struct that transitively owns heap.
    pub(super) fn type_expr_owns_heap(&self, te: &TypeExpr, visited: &mut HashSet<String>) -> bool {
        match &te.kind {
            TypeKind::Path(p) => {
                let head = p.segments.last().map(String::as_str).unwrap_or("");
                if matches!(
                    head,
                    "String" | "Vec" | "Map" | "Set" | "SortedMap" | "SortedSet"
                ) {
                    return true;
                }
                if matches!(head, "Option" | "Result") {
                    return p.generic_args.iter().flatten().any(
                        |a| matches!(a, GenericArg::Type(t) if self.type_expr_owns_heap(t, visited)),
                    );
                }
                self.named_type_owns_heap(head, visited)
            }
            _ => false,
        }
    }

    pub(super) fn collect_move_hazard_locals(&self, block: &Block) -> HashMap<String, bool> {
        fn head_is_container(head: &str) -> bool {
            matches!(
                head,
                "Vec" | "String" | "Map" | "Set" | "SortedMap" | "SortedSet"
            )
        }
        fn type_expr_hazard(this: &ConcurrencyChecker, te: &TypeExpr) -> Option<bool> {
            match &te.kind {
                TypeKind::Path(p) => {
                    let head = p.segments.last().map(String::as_str).unwrap_or("");
                    if head_is_container(head) {
                        return Some(false);
                    }
                    if matches!(head, "Option" | "Result") {
                        let payload_hazard = p.generic_args.iter().flatten().any(
                            |a| matches!(a, GenericArg::Type(t) if type_expr_hazard(this, t).is_some()),
                        );
                        if payload_hazard {
                            return Some(true);
                        }
                    }
                    // A user enum/struct that transitively owns heap is a
                    // move-hazard too (B-2026-07-22-9) — classified
                    // non-wrapper (`false`): its methods aren't `Option`/
                    // `Result` combinators, it just must not be
                    // par-produced-then-moved.
                    if this.named_type_owns_heap(head, &mut HashSet::new()) {
                        return Some(false);
                    }
                    None
                }
                _ => None,
            }
        }
        // Semantic-`Type` twin for the un-annotated case, resolved from the
        // typechecker's `expr_types` keyed by the LET RHS's span — the
        // `pattern_binding_types` string records only the head name
        // ("Option"), losing the payload that decides hazard-ness.
        fn semantic_type_hazard(
            this: &ConcurrencyChecker,
            t: &crate::typechecker::types::Type,
        ) -> Option<bool> {
            use crate::typechecker::types::Type as T;
            match t {
                T::Str => Some(false),
                T::Named { name, args } => {
                    if head_is_container(name) {
                        return Some(false);
                    }
                    if matches!(name.as_str(), "Option" | "Result")
                        && args.iter().any(|a| semantic_type_hazard(this, a).is_some())
                    {
                        return Some(true);
                    }
                    // User enum/struct transitively owning heap
                    // (B-2026-07-22-9) — the un-annotated `let a = mk_nums()`
                    // twin of the `type_expr_hazard` enum/struct arm.
                    if this.named_type_owns_heap(name, &mut HashSet::new()) {
                        return Some(false);
                    }
                    None
                }
                _ => None,
            }
        }
        fn classify_let(
            this: &ConcurrencyChecker,
            pattern: &Pattern,
            ty: &Option<TypeExpr>,
            value_span: Option<&crate::token::Span>,
            out: &mut HashMap<String, bool>,
        ) {
            let PatternKind::Binding(name) = &pattern.kind else {
                return;
            };
            let hazard = match ty {
                Some(te) => type_expr_hazard(this, te),
                None => value_span.and_then(|vs| {
                    this.types
                        .and_then(|t| t.expr_types.get(&SpanKey::from_span(vs)))
                        .and_then(|t| semantic_type_hazard(this, t))
                }),
            };
            if let Some(is_wrapper) = hazard {
                out.insert(name.clone(), is_wrapper);
            }
        }
        fn walk_block(this: &ConcurrencyChecker, block: &Block, out: &mut HashMap<String, bool>) {
            for stmt in &block.stmts {
                walk_stmt(this, stmt, out);
            }
            if let Some(fe) = &block.final_expr {
                walk_expr(this, fe, out);
            }
        }
        fn walk_stmt(this: &ConcurrencyChecker, stmt: &Stmt, out: &mut HashMap<String, bool>) {
            match &stmt.kind {
                StmtKind::Let {
                    pattern, ty, value, ..
                } => {
                    classify_let(this, pattern, ty, Some(&value.span), out);
                    walk_expr(this, value, out);
                }
                StmtKind::LetUninit { name, ty, .. } => {
                    if let Some(is_wrapper) = type_expr_hazard(this, ty) {
                        out.insert(name.clone(), is_wrapper);
                    }
                }
                StmtKind::LetElse {
                    pattern,
                    ty,
                    value,
                    else_block,
                    ..
                } => {
                    classify_let(this, pattern, ty, Some(&value.span), out);
                    walk_expr(this, value, out);
                    walk_block(this, else_block, out);
                }
                StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                    walk_block(this, body, out);
                }
                StmtKind::Assign { target, value } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::MultiAssign { targets, values } => {
                    for e in targets.iter().chain(values.iter()) {
                        walk_expr(this, e, out);
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    walk_expr(this, target, out);
                    walk_expr(this, value, out);
                }
                StmtKind::Expr(e) => walk_expr(this, e, out),
            }
        }
        fn walk_expr(this: &ConcurrencyChecker, e: &Expr, out: &mut HashMap<String, bool>) {
            match &e.kind {
                ExprKind::Block(b)
                | ExprKind::Seq(b)
                | ExprKind::Par(b)
                | ExprKind::Unsafe(b)
                | ExprKind::Try(b)
                | ExprKind::Comptime(b) => walk_block(this, b, out),
                ExprKind::LabeledBlock { body, .. } => walk_block(this, body, out),
                ExprKind::If {
                    condition,
                    then_block,
                    else_branch,
                } => {
                    walk_expr(this, condition, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::IfLet {
                    value,
                    then_block,
                    else_branch,
                    ..
                } => {
                    walk_expr(this, value, out);
                    walk_block(this, then_block, out);
                    if let Some(eb) = else_branch {
                        walk_expr(this, eb, out);
                    }
                }
                ExprKind::Match { scrutinee, arms } => {
                    walk_expr(this, scrutinee, out);
                    for arm in arms {
                        walk_expr(this, &arm.body, out);
                    }
                }
                ExprKind::While { body, .. }
                | ExprKind::WhileLet { body, .. }
                | ExprKind::For { body, .. }
                | ExprKind::Loop { body, .. } => walk_block(this, body, out),
                ExprKind::Lock { body, .. } => walk_block(this, body, out),
                ExprKind::Providers { body, .. } => walk_block(this, body, out),
                _ => {}
            }
        }
        let mut out = HashMap::new();
        walk_block(self, block, &mut out);
        out
    }

    /// The set of move-hazard locals this statement CONSUMES — reads that
    /// transfer heap ownership out of the binding, so the stmt must not run
    /// in a par-branch worker while the parent still owns the original
    /// (B-2026-07-16-19). Consuming shapes recognized:
    ///
    ///   * `match X { .. }` / `if let P = X` on a bare hazard binding where
    ///     some arm pattern binds a payload out (the proven-broken repro:
    ///     the branch moves the payload into the arm binding and frees it,
    ///     the parent's scope-exit payload free fires again);
    ///   * a METHOD call on a bare `Option`/`Result` hazard receiver — the
    ///     combinator family (`unwrap*`/`map*`/`ok`/`take`/..) consumes
    ///     `self` (bare-container receivers stay eligible: their methods are
    ///     ref-self dominated, and gating them would de-parallelize the
    ///     bread-and-butter `v.iter().sum()` reader workers);
    ///   * a bare hazard binding as a call arg in an OWNED parameter
    ///     position — a user free fn whose param is neither `ref` /
    ///     `mut ref` / `mut Slice`, or a `Some`/`Ok`/`Err` constructor
    ///     (unresolvable callees — builtins like `println` — are treated as
    ///     borrowing);
    ///   * a bare hazard binding as ANY method-call argument (`v2.push(s)`
    ///     moves; read-only bare-container method args are rare enough that
    ///     the over-approximation costs little);
    ///   * a bare hazard binding as a `let`/`Assign` RHS (alias-move), a
    ///     struct-literal / array / tuple element (move into aggregate), or
    ///     a `for` iterable (owned iteration).
    ///
    /// Names introduced by the statement itself are the caller's job to
    /// subtract (see `analyze_function`).
    ///
    /// `count_wrapper_method_receiver` gates the `Option`/`Result` combinator-
    /// method-receiver case (`a.unwrap_or(..)` consumes `a`). The CONSUMER
    /// guard passes `true` (it must not enter a par group). The PRODUCER guard
    /// (B-2026-07-22-9) passes `false`: a published slot consumed by a wrapper
    /// combinator is made round-trip-safe by B-2026-07-17-4's branch-side
    /// publish suppression + parent re-registration, so its producer stays
    /// parallelizable — only genuine alias/owned MOVES of the published binding
    /// (`let c = a`, owned-arg, aggregate element, for-iterable) double-free
    /// with the writeback and must de-parallelize the producer.
    pub(super) fn stmt_consuming_hazard_reads(
        &self,
        stmt: &Stmt,
        hazards: &HashMap<String, bool>,
        count_wrapper_method_receiver: bool,
    ) -> HashSet<String> {
        fn bare_name(e: &Expr) -> Option<&str> {
            match &e.kind {
                ExprKind::Identifier(n) => Some(n.as_str()),
                _ => None,
            }
        }
        struct W<'a> {
            this: &'a ConcurrencyChecker<'a>,
            hazards: &'a HashMap<String, bool>,
            count_wrapper_method_receiver: bool,
            out: HashSet<String>,
        }
        impl W<'_> {
            fn mark_if_hazard(&mut self, e: &Expr) {
                if let Some(n) = bare_name(e) {
                    if self.hazards.contains_key(n) {
                        self.out.insert(n.to_string());
                    }
                }
            }
            fn callee_param_owned(&self, callee: &str, idx: usize) -> bool {
                match self.this.function_bodies.get(callee) {
                    Some(f) => f.params.get(idx).is_none_or(|p| {
                        !matches!(
                            p.ty.kind,
                            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
                        )
                    }),
                    // Unresolvable callee: builtins (`println`, `assert`, ..)
                    // borrow their args — treat as non-consuming.
                    None => false,
                }
            }
            fn block(&mut self, b: &Block) {
                for s in &b.stmts {
                    self.stmt(s);
                }
                if let Some(fe) = &b.final_expr {
                    self.expr(fe);
                }
            }
            fn stmt(&mut self, s: &Stmt) {
                match &s.kind {
                    StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                        self.mark_if_hazard(value);
                        self.expr(value);
                        if let StmtKind::LetElse { else_block, .. } = &s.kind {
                            self.block(else_block);
                        }
                    }
                    StmtKind::LetUninit { .. } => {}
                    StmtKind::Assign { target, value } => {
                        self.mark_if_hazard(value);
                        self.expr(target);
                        self.expr(value);
                    }
                    StmtKind::MultiAssign { targets, values } => {
                        for v in values {
                            self.mark_if_hazard(v);
                        }
                        for e in targets.iter().chain(values.iter()) {
                            self.expr(e);
                        }
                    }
                    StmtKind::CompoundAssign { target, value, .. } => {
                        self.expr(target);
                        self.expr(value);
                    }
                    StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                        self.block(body);
                    }
                    StmtKind::Expr(e) => self.expr(e),
                }
            }
            fn expr(&mut self, e: &Expr) {
                match &e.kind {
                    ExprKind::Match { scrutinee, arms } => {
                        if let Some(n) = bare_name(scrutinee) {
                            if self.hazards.contains_key(n)
                                && arms.iter().any(|a| !a.pattern.binding_names().is_empty())
                            {
                                self.out.insert(n.to_string());
                            }
                        }
                        self.expr(scrutinee);
                        for arm in arms {
                            self.expr(&arm.body);
                        }
                    }
                    ExprKind::IfLet {
                        pattern,
                        value,
                        then_block,
                        else_branch,
                    } => {
                        if let Some(n) = bare_name(value) {
                            if self.hazards.contains_key(n) && !pattern.binding_names().is_empty() {
                                self.out.insert(n.to_string());
                            }
                        }
                        self.expr(value);
                        self.block(then_block);
                        if let Some(eb) = else_branch {
                            self.expr(eb);
                        }
                    }
                    ExprKind::MethodCall { object, args, .. } => {
                        if let Some(n) = bare_name(object) {
                            // Wrapper (`Option`/`Result`) receivers: the
                            // combinator family consumes self. Counted for the
                            // consumer guard; excluded for the producer guard
                            // (B-2026-07-17-4 makes the published-slot round-
                            // trip safe — see the fn doc comment).
                            if self.count_wrapper_method_receiver
                                && self.hazards.get(n).copied() == Some(true)
                            {
                                self.out.insert(n.to_string());
                            }
                        }
                        for a in args {
                            self.mark_if_hazard(&a.value);
                        }
                        self.expr(object);
                        for a in args {
                            self.expr(&a.value);
                        }
                    }
                    ExprKind::Call { callee, args } => {
                        if let Some(cn) = bare_name(callee) {
                            let is_ctor = matches!(cn, "Some" | "Ok" | "Err");
                            for (i, a) in args.iter().enumerate() {
                                if let Some(n) = bare_name(&a.value) {
                                    if self.hazards.contains_key(n)
                                        && (is_ctor || self.callee_param_owned(cn, i))
                                    {
                                        self.out.insert(n.to_string());
                                    }
                                }
                            }
                        }
                        self.expr(callee);
                        for a in args {
                            self.expr(&a.value);
                        }
                    }
                    ExprKind::For { iterable, body, .. } => {
                        self.mark_if_hazard(iterable);
                        self.expr(iterable);
                        self.block(body);
                    }
                    ExprKind::StructLiteral { fields, spread, .. } => {
                        for f in fields {
                            self.mark_if_hazard(&f.value);
                            self.expr(&f.value);
                        }
                        if let Some(sp) = spread {
                            self.expr(sp);
                        }
                    }
                    ExprKind::ArrayLiteral(elems) | ExprKind::Tuple(elems) => {
                        for el in elems {
                            self.mark_if_hazard(el);
                            self.expr(el);
                        }
                    }
                    ExprKind::Block(b)
                    | ExprKind::Seq(b)
                    | ExprKind::Par(b)
                    | ExprKind::Unsafe(b)
                    | ExprKind::Try(b)
                    | ExprKind::Comptime(b) => self.block(b),
                    ExprKind::LabeledBlock { body, .. } => self.block(body),
                    ExprKind::If {
                        condition,
                        then_block,
                        else_branch,
                    } => {
                        self.expr(condition);
                        self.block(then_block);
                        if let Some(eb) = else_branch {
                            self.expr(eb);
                        }
                    }
                    ExprKind::While {
                        condition, body, ..
                    } => {
                        self.expr(condition);
                        self.block(body);
                    }
                    ExprKind::WhileLet { value, body, .. } => {
                        self.expr(value);
                        self.block(body);
                    }
                    ExprKind::Loop { body, .. } => self.block(body),
                    ExprKind::Lock { body, .. } | ExprKind::Providers { body, .. } => {
                        self.block(body)
                    }
                    ExprKind::Binary { left, right, .. } => {
                        self.expr(left);
                        self.expr(right);
                    }
                    ExprKind::Unary { operand, .. } => self.expr(operand),
                    ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                        self.expr(object)
                    }
                    ExprKind::Index { object, index } => {
                        self.expr(object);
                        self.expr(index);
                    }
                    ExprKind::Range { start, end, .. } => {
                        if let Some(s) = start {
                            self.expr(s);
                        }
                        if let Some(en) = end {
                            self.expr(en);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut w = W {
            this: self,
            hazards,
            count_wrapper_method_receiver,
            out: HashSet::new(),
        };
        w.stmt(stmt);
        w.out
    }
}
