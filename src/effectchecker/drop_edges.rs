//! The drops a function's own body runs, as calls of the `drop` bodies they
//! run (core semantics §12 default 3: "A drop's effects are those of the
//! dropped type's drop glue, charged to the scope where the drop runs").
//! B-2026-11-17-13: these were left out of every function's effect set, so a
//! public function whose local prints when it drops declared nothing, a
//! `#[no_effect(writes(Stdout))]` held, and `karac query effects` showed
//! none of it, while the MIR `par` check charged them.
//!
//! The walk sees the AST, not MIR's drop elaboration, so it charges a drop
//! where one surely runs and stays silent where a move may take the value:
//!
//! - an owned binding (a `let`, a `for` pattern, an `own` parameter or an
//!   `own self` receiver) whose name is never used where a use may move
//!   it. A use borrows when it is the receiver of a method that does not
//!   take `own self`, an argument to a parameter that is not `own`, an
//!   operand, an interpolation, a `ref` or a field read; any other use, and
//!   any use inside a closure, counts as a move;
//! - a temporary that nothing takes: an expression statement, `let _ =`,
//!   a receiver of a borrowing method, an argument to a borrowing
//!   parameter;
//! - an assignment, which drops the value it replaces;
//! - `drop(x)`, whose drop runs in the library function.
//!
//! Only `drop` bodies in the program are charged: the library's own
//! `Drop` impls release handles, and their effects are the library's.

use super::*;

/// Library methods that borrow their receiver, so calling one on an owned
/// binding does not move it. A library method not listed here is read as
/// taking its receiver, which keeps the walk from charging a drop a move
/// may have taken.
const BORROWING_LIBRARY_METHODS: &[&str] = &[
    "len",
    "is_empty",
    "iter",
    "iter_mut",
    "get",
    "get_mut",
    "contains",
    "contains_key",
    "push",
    "push_back",
    "push_front",
    "pop",
    "pop_back",
    "pop_front",
    "insert",
    "remove",
    "clear",
    "first",
    "last",
    "keys",
    "values",
    "values_mut",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "as_ref",
    "as_mut",
    "clone",
    "to_string",
    "sort",
    "sort_by",
    "sort_by_key",
    "reverse",
    "truncate",
    "retain",
    "extend",
    "swap",
];

/// Free functions that only read their arguments.
const BORROWING_LIBRARY_FUNCTIONS: &[&str] = &[
    "print",
    "println",
    "eprint",
    "eprintln",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
];

#[derive(Clone, Copy, PartialEq)]
enum Use {
    Move,
    Borrow,
}

struct Walk<'c, 'a> {
    checker: &'c EffectChecker<'a>,
    /// Owned bindings: name, the drop bodies the value runs, the site, and
    /// the closure depth it was bound at.
    owned: Vec<(String, Vec<Symbol>, Span)>,
    depth_of: FxHashMap<String, usize>,
    moved: FxHashSet<String>,
    /// Assignments to a name, charged unless the name is ever moved (it may
    /// then be uninitialized where it is assigned).
    assigned: Vec<(String, Vec<Symbol>, Span)>,
    uninit: FxHashSet<String>,
    charged: Vec<(Symbol, Span)>,
    closure_depth: usize,
}

impl EffectChecker<'_> {
    /// Fill [`Self::drop_calls`] for every function and method body.
    pub(crate) fn build_drop_calls(&mut self) {
        if self.drop_nodes.is_empty() && self.drops_by_type.is_empty() {
            return;
        }
        let mut out: FxHashMap<Symbol, Vec<(Symbol, Span)>> = FxHashMap::default();
        for (&name, f) in self.function_bodies.iter().chain(self.method_bodies.iter()) {
            let edges = self.drop_calls_of(name, f);
            if !edges.is_empty() {
                out.insert(name, edges);
            }
        }
        self.drop_calls = out;
    }

    /// The `T.drop` keys for a value whose drop glue reaches `types`, each a
    /// `drop` body in the program.
    fn drop_keys(&self, types: &[String]) -> Vec<Symbol> {
        types
            .iter()
            .filter_map(|t| {
                let key = self.interner.dotted_str(t, "drop");
                self.method_bodies.contains_key(&key).then_some(key)
            })
            .collect()
    }

    fn node_drop_keys(&self, id: crate::ids::NodeId) -> Vec<Symbol> {
        self.drop_nodes
            .get(&id)
            .map(|t| self.drop_keys(t))
            .unwrap_or_default()
    }

    fn drop_calls_of(&self, name: Symbol, f: &Function) -> Vec<(Symbol, Span)> {
        let mut w = Walk {
            checker: self,
            owned: Vec::new(),
            depth_of: FxHashMap::default(),
            moved: FxHashSet::default(),
            assigned: Vec::new(),
            uninit: FxHashSet::default(),
            charged: Vec::new(),
            closure_depth: 0,
        };
        for p in &f.params {
            if p.is_own {
                w.bind_pattern(&p.pattern);
            }
        }
        if f.self_param.is_some() && f.self_is_own {
            let key = self.interner.resolve(name).to_string();
            if let Some((ty, _)) = key.rsplit_once('.') {
                if let Some(types) = self.drops_by_type.get(ty) {
                    let keys = self.drop_keys(types);
                    if !keys.is_empty() {
                        w.owned.push(("self".to_string(), keys, f.span));
                        w.depth_of.insert("self".to_string(), 0);
                    }
                }
            }
        }
        w.block(&f.body, Use::Move);
        let Walk {
            owned,
            moved,
            assigned,
            uninit,
            mut charged,
            ..
        } = w;
        for (n, keys, span) in owned.into_iter().chain(assigned) {
            if moved.contains(&n) || uninit.contains(&n) {
                continue;
            }
            charged.extend(keys.into_iter().map(|k| (k, span)));
        }
        charged
    }
}

/// An expression whose value is new here, so if nothing takes it, it drops
/// at the end of its statement.
fn is_temporary(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StructLiteral { .. }
            | ExprKind::Tuple(_)
            | ExprKind::ArrayLiteral(_)
    )
}

impl Walk<'_, '_> {
    fn charge(&mut self, keys: Vec<Symbol>, span: Span) {
        self.charged.extend(keys.into_iter().map(|k| (k, span)));
    }

    fn charge_temporary(&mut self, e: &Expr) {
        if is_temporary(e) {
            let keys = self.checker.node_drop_keys(e.id);
            self.charge(keys, e.span);
        }
    }

    fn bind(&mut self, name: &str, id: crate::ids::NodeId, span: Span) {
        self.depth_of.insert(name.to_string(), self.closure_depth);
        let keys = self.checker.node_drop_keys(id);
        if !keys.is_empty() {
            self.owned.push((name.to_string(), keys, span));
        }
    }

    fn bind_pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Binding(n) => self.bind(n, p.id, p.span),
            PatternKind::AtBinding {
                name,
                pattern,
                by_ref,
            } => {
                if !by_ref {
                    self.bind(name, p.id, p.span);
                }
                self.bind_pattern(pattern);
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(sub) => self.bind_pattern(sub),
                        // `S { x }` binds `x`; its type is the field's,
                        // which the pattern node does not carry.
                        None => {
                            self.depth_of.insert(f.name.clone(), self.closure_depth);
                        }
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } | PatternKind::Tuple(patterns) => {
                for sub in patterns {
                    self.bind_pattern(sub);
                }
            }
            PatternKind::Or(alts) => {
                if let Some(first) = alts.first() {
                    self.bind_pattern(first);
                }
            }
            PatternKind::Slice { prefix, suffix, .. } => {
                for sub in prefix.iter().chain(suffix) {
                    self.bind_pattern(sub);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }

    /// A pattern that leaves part of its value unbound drops that part.
    fn has_wildcard(p: &Pattern) -> bool {
        match &p.kind {
            PatternKind::Wildcard => true,
            PatternKind::AtBinding { pattern, .. } => Self::has_wildcard(pattern),
            PatternKind::Struct {
                fields, has_rest, ..
            } => {
                *has_rest
                    || fields
                        .iter()
                        .any(|f| f.pattern.as_ref().is_some_and(Self::has_wildcard))
            }
            PatternKind::TupleVariant { patterns, .. } | PatternKind::Tuple(patterns) => {
                patterns.iter().any(Self::has_wildcard)
            }
            PatternKind::Or(alts) => alts.iter().any(Self::has_wildcard),
            PatternKind::Slice { .. } => true,
            PatternKind::Binding(_)
            | PatternKind::Literal(_)
            | PatternKind::RangePattern { .. } => false,
        }
    }

    fn name_use(&mut self, name: &str, how: Use) {
        let inner = self
            .depth_of
            .get(name)
            .is_some_and(|&d| self.closure_depth > d);
        if how == Use::Move || inner {
            self.moved.insert(name.to_string());
        }
    }

    fn block(&mut self, b: &Block, tail: Use) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(e) = &b.final_expr {
            self.expr(e, tail);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Let { pattern, value, .. } => {
                if matches!(pattern.kind, PatternKind::Wildcard) {
                    self.expr(value, Use::Borrow);
                    self.charge_temporary(value);
                    return;
                }
                self.expr(value, Use::Move);
                if Self::has_wildcard(pattern) {
                    self.charge_temporary(value);
                }
                self.bind_pattern(pattern);
            }
            StmtKind::LetElse {
                pattern,
                value,
                else_block,
                ..
            } => {
                self.expr(value, Use::Move);
                self.block(else_block, Use::Move);
                self.bind_pattern(pattern);
            }
            StmtKind::LetUninit { name, .. } => {
                self.uninit.insert(name.clone());
                self.depth_of.insert(name.clone(), self.closure_depth);
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.block(body, Use::Borrow);
            }
            StmtKind::Assign { target, value } => {
                self.expr(value, Use::Move);
                self.expr(target, Use::Borrow);
                let keys = self.checker.node_drop_keys(target.id);
                if keys.is_empty() {
                    return;
                }
                match &target.kind {
                    ExprKind::Identifier(n) => self.assigned.push((n.clone(), keys, s.span)),
                    _ => self.charge(keys, s.span),
                }
            }
            StmtKind::MultiAssign { targets, values } => {
                for v in values {
                    self.expr(v, Use::Move);
                }
                for t in targets {
                    self.expr(t, Use::Move);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(value, Use::Move);
                self.expr(target, Use::Borrow);
            }
            StmtKind::Expr(e) => {
                self.expr(e, Use::Borrow);
                self.charge_temporary(e);
            }
        }
    }

    /// The modes of a known function's parameters: `true` for `own`.
    fn param_modes(&self, key: Symbol) -> Option<(Vec<bool>, bool)> {
        let f = self
            .checker
            .function_bodies
            .get(&key)
            .or_else(|| self.checker.method_bodies.get(&key))?;
        Some((
            f.params.iter().map(|p| p.is_own).collect(),
            f.self_param.is_some(),
        ))
    }

    fn args(&mut self, args: &[CallArg], modes: Option<&[bool]>) {
        for (i, a) in args.iter().enumerate() {
            let own = match modes {
                Some(m) => m.get(i).copied().unwrap_or(true),
                None => true,
            };
            if own {
                self.expr(&a.value, Use::Move);
            } else {
                self.expr(&a.value, Use::Borrow);
                self.charge_temporary(&a.value);
            }
        }
    }

    fn expr(&mut self, e: &Expr, how: Use) {
        match &e.kind {
            ExprKind::Identifier(n) => self.name_use(n, how),
            ExprKind::SelfValue => self.name_use("self", how),
            ExprKind::Integer(..)
            | ExprKind::Float(..)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::ByteStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Continue { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for p in parts {
                    if let ParsedInterpolationPart::Expr(x, _) = p {
                        self.expr(x, Use::Borrow);
                    }
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.expr(left, Use::Borrow);
                self.expr(right, Use::Borrow);
            }
            // `ref x`, `mut ref x`, `*x` and the arithmetic operators read
            // their operand.
            ExprKind::Unary { operand, .. } => self.expr(operand, Use::Borrow),
            ExprKind::Question(inner) | ExprKind::Cast { expr: inner, .. } => {
                self.expr(inner, Use::Move)
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.expr(object, Use::Move);
                if let Some(args) = args {
                    self.args(args, None);
                }
            }
            ExprKind::NilCoalesce { left, right } | ExprKind::Pipe { left, right } => {
                self.expr(left, Use::Move);
                self.expr(right, Use::Move);
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Identifier(n) = &callee.kind {
                    if n == "drop" {
                        for a in args {
                            self.expr(&a.value, Use::Move);
                            let keys = self.checker.node_drop_keys(a.value.id);
                            self.charge(keys, e.span);
                        }
                        return;
                    }
                    if BORROWING_LIBRARY_FUNCTIONS.contains(&n.as_str()) {
                        self.args(args, Some(&[]));
                        return;
                    }
                }
                self.expr(callee, Use::Borrow);
                let modes = self
                    .checker
                    .extract_callee_name(callee)
                    .and_then(|k| self.param_modes(k));
                match modes {
                    // `Type.method(x, ..)` passes the receiver first.
                    Some((m, true)) => {
                        let mut all = vec![false];
                        all.extend(m);
                        self.args(args, Some(&all));
                    }
                    Some((m, false)) => self.args(args, Some(&m)),
                    None => self.args(args, None),
                }
            }
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                let key = self.checker.resolve_method_callee_key(&e.span);
                let user = key.and_then(|k| {
                    let f = self.checker.method_bodies.get(&k)?;
                    Some((
                        f.self_is_own,
                        f.params.iter().map(|p| p.is_own).collect::<Vec<_>>(),
                    ))
                });
                let (takes_self, modes) = match user {
                    Some((own, m)) => (own, Some(m)),
                    None => (!BORROWING_LIBRARY_METHODS.contains(&method.as_str()), None),
                };
                if takes_self {
                    self.expr(object, Use::Move);
                } else {
                    self.expr(object, Use::Borrow);
                    self.charge_temporary(object);
                }
                self.args(args, modes.as_deref());
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                let part_moves = how == Use::Move && !self.checker.node_drop_keys(e.id).is_empty();
                self.expr(object, if part_moves { Use::Move } else { Use::Borrow });
            }
            ExprKind::Index { object, index } => {
                let part_moves = how == Use::Move && !self.checker.node_drop_keys(e.id).is_empty();
                self.expr(object, if part_moves { Use::Move } else { Use::Borrow });
                self.expr(index, Use::Move);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.block(b, how),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.expr(condition, Use::Borrow);
                self.block(then_block, how);
                if let Some(x) = else_branch {
                    self.expr(x, how);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.expr(value, Use::Move);
                self.block(then_block, how);
                if let Some(x) = else_branch {
                    self.expr(x, how);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee, Use::Move);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g, Use::Borrow);
                    }
                    self.expr(&arm.body, how);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.expr(condition, Use::Borrow);
                self.block(body, Use::Borrow);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.expr(value, Use::Move);
                self.block(body, Use::Borrow);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.expr(iterable, Use::Move);
                self.bind_pattern(pattern);
                self.block(body, Use::Borrow);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.block(body, how)
            }
            ExprKind::Closure { body, .. } => {
                self.closure_depth += 1;
                self.expr(body, Use::Move);
                self.closure_depth -= 1;
            }
            ExprKind::Return(v) | ExprKind::Break { value: v, .. } => {
                if let Some(v) = v {
                    self.expr(v, Use::Move);
                }
            }
            ExprKind::Tuple(items)
            | ExprKind::ArrayLiteral(items)
            | ExprKind::PrefixCollectionLiteral { items, .. } => {
                for x in items {
                    self.expr(x, Use::Move);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.expr(value, Use::Move);
                self.expr(count, Use::Move);
            }
            ExprKind::MapLiteral { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k, Use::Move);
                    self.expr(v, Use::Move);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.expr(&f.value, Use::Move);
                }
                if let Some(s) = spread {
                    self.expr(s, Use::Move);
                }
            }
            ExprKind::Range { start, end, .. } => {
                for x in [start, end].into_iter().flatten() {
                    self.expr(x, Use::Move);
                }
            }
            ExprKind::Lock { mutex, body, .. } => {
                self.expr(mutex, Use::Borrow);
                self.block(body, how);
            }
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.expr(&b.value, Use::Move);
                }
                self.block(body, how);
            }
        }
    }
}
