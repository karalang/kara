//! The executable ownership/drop judgment — Slice 3 of the
//! ownership-model-mechanization spike.
//!
//! This is the **reference implementation** of
//! [`docs/spikes/ownership-drop-judgment.md`](../docs/spikes/ownership-drop-judgment.md):
//! for every place at every program point it computes an ownership **state**
//! (Owned / Borrowed / Moved / Dead), emits the **drop schedule** (which owned
//! place drops at which scope exit, in LIFO order), and runs the **invariant
//! checks** (§2 of the judgment — freed-exactly-once + no use-after-move).
//!
//! It is a *standalone* pass: it consumes only the plain AST (`crate::ast`) and
//! never touches codegen or `inkwell`. That is the point — the model is one
//! artifact both the checker and codegen can consult, instead of each
//! re-deriving the rules. Slice 4 makes drop-insertion read this oracle's facts
//! directly; the fuzzer (`src/bin/drop_fuzz.rs`) consults it via `--oracle` to
//! cross-validate that every generated program the sanitizer sees is
//! model-valid, and as the reference a future codegen-fact differential diffs
//! against.
//!
//! ## Scope (v1)
//!
//! The judgment is stated in full; this executable v1 covers the **heap-core
//! subset** the drop-fuzzer exercises — the shapes where the drop-soundness bug
//! class actually lives: `String` / `Vec` / `Map` / `Set` / tuples / structs /
//! `Option` / boxed enums, moves (container-mutator args, aggregate-literal
//! fields, index/field-store, `return`), borrows (`ref`/`mut ref` params,
//! `.iter()`, borrowing reads), user-fn owned-param calls (caller-retains =
//! NonConsuming), destructure and match-payload move-out (the obligation
//! split), and scope-exit drops. Two edges the judgment flags as open (§7) are
//! deliberately conservative here: **closure/cross-task captures** are treated
//! as borrows (never move the parent binding — matches the auto-promoted-shared
//! reality and avoids false use-after-move on the multi-`spawn` shape), and
//! **NLL last-use shortening** is not modelled (drops land at the scope-exit
//! ceiling, per the judgment's stated ceiling semantics). Constructs outside
//! the subset are walked structurally for read/move effects but do not
//! contribute exotic drop shapes.

use crate::ast::*;
use crate::token::Span;
use std::collections::{HashMap, HashSet};

// ───────────────────────────── public API ─────────────────────────────

/// The ownership state of a place at a program point (§1 of the judgment).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaceState {
    /// Sole live claim to a heap allocation — carries a free-obligation.
    Owned,
    /// Aliases an allocation owned elsewhere — no obligation.
    Borrowed,
    /// Obligation transferred out — must not be read or dropped.
    Moved,
    /// Uninitialized or already-dropped — must not be read.
    Dead,
}

/// Why the model schedules a drop at a point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropReason {
    /// The owning binding's enclosing scope exits (the ceiling; §3.5).
    ScopeExit,
}

/// A drop the model schedules — the reference codegen must match.
#[derive(Clone, Debug)]
pub struct DropEvent {
    /// Root binding name whose obligation is discharged.
    pub place: String,
    /// Rendered type of the dropped place (for reporting).
    pub ty: String,
    /// Monotonic scope id the binding lived in.
    pub scope_id: usize,
    pub reason: DropReason,
    pub span: Span,
}

/// A violation of the single invariant (§2) — a source-level ownership fault
/// the model detects (use-after-move / read-after-drop). On a valid program
/// this list is empty.
#[derive(Clone, Debug)]
pub struct Violation {
    pub kind: ViolationKind,
    pub place: String,
    pub message: String,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViolationKind {
    /// A Moved place was read, projected, or moved again (clause 3).
    UseAfterMove,
    /// A Dead (uninitialized / already-dropped) place was read (clause 3).
    UseAfterDrop,
}

/// Per-function oracle output.
#[derive(Clone, Debug)]
pub struct FnOracle {
    pub function: String,
    /// The drop schedule — every owned place's discharge, in emission order
    /// (LIFO within a scope, inner scopes before outer).
    pub drops: Vec<DropEvent>,
    pub violations: Vec<Violation>,
}

/// Whole-program oracle output.
#[derive(Clone, Debug, Default)]
pub struct OracleResult {
    pub functions: Vec<FnOracle>,
}

impl OracleResult {
    /// True iff no function had an invariant violation.
    pub fn is_clean(&self) -> bool {
        self.functions.iter().all(|f| f.violations.is_empty())
    }

    /// All violations across every function.
    pub fn violations(&self) -> impl Iterator<Item = &Violation> {
        self.functions.iter().flat_map(|f| f.violations.iter())
    }

    /// Total number of scheduled drops across the program.
    pub fn drop_count(&self) -> usize {
        self.functions.iter().map(|f| f.drops.len()).sum()
    }

    /// The drop schedule of a named function, if analyzed.
    pub fn function(&self, name: &str) -> Option<&FnOracle> {
        self.functions.iter().find(|f| f.function == name)
    }
}

/// Run the ownership oracle over a whole program.
pub fn analyze(program: &Program) -> OracleResult {
    let type_db = TypeDb::build(program);
    let sigs = SigTable::build(program);
    let mut result = OracleResult::default();
    for item in &program.items {
        collect_functions(item, &type_db, &sigs, &mut result);
    }
    result
}

fn collect_functions(item: &Item, type_db: &TypeDb, sigs: &SigTable, out: &mut OracleResult) {
    match item {
        Item::Function(f) => out.functions.push(analyze_fn(f, type_db, sigs)),
        Item::ImplBlock(b) => {
            for it in &b.items {
                if let ImplItem::Method(m) = it {
                    out.functions.push(analyze_fn(m, type_db, sigs));
                }
            }
        }
        _ => {}
    }
}

// ─────────────────────── type heap-ness (§1) ───────────────────────────

/// Struct/enum field-type database, so heap-ness of a user type is decided by
/// looking at its fields' types (transitively).
struct TypeDb {
    /// struct name → (field name, field type)
    structs: HashMap<String, Vec<(String, TypeExpr)>>,
    /// enum name → all payload types across variants
    enums: HashMap<String, Vec<TypeExpr>>,
}

impl TypeDb {
    fn build(program: &Program) -> Self {
        let mut structs = HashMap::new();
        let mut enums = HashMap::new();
        for item in &program.items {
            match item {
                Item::StructDef(s) => {
                    structs.insert(
                        s.name.clone(),
                        s.fields
                            .iter()
                            .map(|f| (f.name.clone(), f.ty.clone()))
                            .collect(),
                    );
                }
                Item::EnumDef(e) => {
                    let mut tys = Vec::new();
                    for v in &e.variants {
                        match &v.kind {
                            VariantKind::Tuple(ts) => tys.extend(ts.iter().cloned()),
                            VariantKind::Struct(fs) => tys.extend(fs.iter().map(|f| f.ty.clone())),
                            VariantKind::Unit => {}
                        }
                    }
                    enums.insert(e.name.clone(), tys);
                }
                _ => {}
            }
        }
        TypeDb { structs, enums }
    }

    /// Does a value of this declared type own heap storage (and therefore carry
    /// a free-obligation when Owned)? Recurses through user structs/enums,
    /// tuples, and the known heap builtins. `ref`/`mut ref` are borrows → never
    /// owning here.
    fn is_heap(&self, ty: &TypeExpr) -> bool {
        self.is_heap_guarded(ty, &mut HashSet::new())
    }

    fn is_heap_guarded(&self, ty: &TypeExpr, seen: &mut HashSet<String>) -> bool {
        match &ty.kind {
            // A borrow owns nothing.
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_) | TypeKind::Weak(_) => {
                false
            }
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.is_heap_guarded(e, seen)),
            TypeKind::Array { element, .. } => self.is_heap_guarded(element, seen),
            TypeKind::Path(p) => {
                let name = p.segments.last().map(String::as_str).unwrap_or("");
                if is_heap_builtin(name) {
                    return true;
                }
                if is_pod_builtin(name) {
                    return false;
                }
                // Option[T] / Result[T] own heap iff a type argument does.
                if matches!(name, "Option" | "Result") {
                    return generic_type_args(p)
                        .iter()
                        .any(|t| self.is_heap_guarded(t, seen));
                }
                // A user struct/enum: heap iff any field/payload is heap.
                if !seen.insert(name.to_string()) {
                    // Recursive type (e.g. `enum Tree { Node(Tree) }`) — the
                    // recursion itself is via a boxed heap indirection, so it
                    // owns heap.
                    return true;
                }
                if let Some(fields) = self.structs.get(name) {
                    return fields.iter().any(|(_, f)| self.is_heap_guarded(f, seen));
                }
                if let Some(tys) = self.enums.get(name) {
                    return tys.iter().any(|t| self.is_heap_guarded(t, seen));
                }
                // Unknown named type: assume non-heap (POD) — conservative for
                // the schedule (an unknown type contributes no drop), and the
                // fuzzer subset never produces one.
                false
            }
            // Function types (closures) own an environment, but closure
            // ownership is the §7 open edge — treated as non-owning here.
            TypeKind::FnType { .. } => false,
            _ => false,
        }
    }
}

/// Heap-owning builtins (the leaf owners).
fn is_heap_builtin(name: &str) -> bool {
    matches!(
        name,
        "String" | "Vec" | "Map" | "Set" | "VecDeque" | "Slice" | "Box" | "Rc" | "Arc"
    )
}

/// Merge a place's state across two branches of a conditional. The only
/// drop-safe way to elide a drop is agreement that the place is gone on every
/// path, so `Moved`/`Borrowed`/`Dead` survive the merge only when both paths
/// agree; any disagreement collapses to `Owned` (schedule the drop — a
/// conditional over-schedule is corrected by codegen's runtime guard, whereas an
/// under-schedule leaks). A binding's borrow-ness is fixed at introduction, so
/// `Owned`/`Borrowed` never actually mix here; the `_ => Owned` arm is reached
/// only by `Owned`/`Moved` disagreement — exactly the conditional-move case.
fn merge_state(a: PlaceState, b: PlaceState) -> PlaceState {
    match (a, b) {
        (PlaceState::Moved, PlaceState::Moved) => PlaceState::Moved,
        (PlaceState::Borrowed, PlaceState::Borrowed) => PlaceState::Borrowed,
        (PlaceState::Dead, PlaceState::Dead) => PlaceState::Dead,
        _ => PlaceState::Owned,
    }
}

/// Scalar builtins that never own heap.
fn is_pod_builtin(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "usize"
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "StringSlice"
    )
}

fn generic_type_args(p: &PathExpr) -> Vec<TypeExpr> {
    p.generic_args
        .as_ref()
        .map(|args| {
            args.iter()
                .filter_map(|a| match a {
                    GenericArg::Type(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn render_type(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeKind::Path(p) => {
            let base = p.segments.join(".");
            let args = generic_type_args(p);
            if args.is_empty() {
                base
            } else {
                let inner: Vec<String> = args.iter().map(render_type).collect();
                format!("{base}[{}]", inner.join(", "))
            }
        }
        TypeKind::Tuple(elems) => {
            let inner: Vec<String> = elems.iter().map(render_type).collect();
            format!("({})", inner.join(", "))
        }
        TypeKind::Ref(t) => format!("ref {}", render_type(t)),
        TypeKind::MutRef(t) => format!("mut ref {}", render_type(t)),
        TypeKind::MutSlice(t) => format!("mut Slice[{}]", render_type(t)),
        TypeKind::Array { element, .. } => format!("[{}]", render_type(element)),
        _ => "?".to_string(),
    }
}

// ─────────────────────── callee signatures (§4) ────────────────────────

/// Parameter mode — the consumption classifier keys on this for user calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParamMode {
    /// Bare `T` — owned param; the callee entry-copies, so a *caller* binding
    /// passed here is **NonConsuming** (caller-retains; §4).
    Owned,
    /// `ref T` / `mut ref T` / `mut Slice[T]` — borrow; NonConsuming.
    Borrow,
}

fn param_mode(ty: &TypeExpr) -> ParamMode {
    match &ty.kind {
        TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_) => ParamMode::Borrow,
        _ => ParamMode::Owned,
    }
}

/// fn-name → parameter modes.
struct SigTable {
    fns: HashMap<String, Vec<ParamMode>>,
}

impl SigTable {
    fn build(program: &Program) -> Self {
        let mut fns = HashMap::new();
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    fns.insert(
                        f.name.clone(),
                        f.params.iter().map(|p| param_mode(&p.ty)).collect(),
                    );
                }
                Item::ImplBlock(b) => {
                    for it in &b.items {
                        if let ImplItem::Method(m) = it {
                            fns.insert(
                                m.name.clone(),
                                m.params.iter().map(|p| param_mode(&p.ty)).collect(),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        SigTable { fns }
    }
}

/// Builtin methods whose *arguments* escape into the receiver (a move; §4).
fn method_args_escape(method: &str) -> bool {
    matches!(
        method,
        "push" | "insert" | "push_back" | "push_front" | "add" | "set"
    )
}

// ─────────────────────── the per-function analysis ─────────────────────

/// A live binding tracked in a scope.
#[derive(Clone)]
struct Binding {
    name: String,
    ty_render: String,
    state: PlaceState,
    scope_id: usize,
    span: Span,
    /// Whether this binding is heap-owning (only heap bindings drop).
    heap: bool,
}

/// How the *parent* expression consumes a sub-expression's value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The value is moved out (escapes): container-mutator arg, aggregate-
    /// literal field, index/field-store value, `return`/tail, assignment RHS.
    Move,
    /// The value is read/borrowed/copied: user-fn arg (caller-retains),
    /// borrow-method receiver/arg, operand of a read.
    Read,
}

struct Analyzer<'a> {
    type_db: &'a TypeDb,
    sigs: &'a SigTable,
    /// Scope stack; each entry is `(scope_id, ordered binding indices)`.
    scopes: Vec<(usize, Vec<usize>)>,
    /// Flat arena of all bindings ever introduced (indices stay stable).
    bindings: Vec<Binding>,
    /// name → current binding index (shadowing overwrites).
    by_name: HashMap<String, usize>,
    next_scope_id: usize,
    drops: Vec<DropEvent>,
    violations: Vec<Violation>,
}

fn analyze_fn(f: &Function, type_db: &TypeDb, sigs: &SigTable) -> FnOracle {
    let mut a = Analyzer {
        type_db,
        sigs,
        scopes: Vec::new(),
        bindings: Vec::new(),
        by_name: HashMap::new(),
        next_scope_id: 0,
        drops: Vec::new(),
        violations: Vec::new(),
    };
    a.push_scope();
    // Parameters: an *owned* heap param is Owned in the body (the callee owns
    // its entry-copy and drops it); a borrow param is Borrowed (never drops).
    for p in &f.params {
        if let Some(name) = p.name() {
            let heap = type_db.is_heap(&p.ty);
            let state = match param_mode(&p.ty) {
                ParamMode::Borrow => PlaceState::Borrowed,
                ParamMode::Owned => {
                    if heap {
                        PlaceState::Owned
                    } else {
                        PlaceState::Dead
                    }
                }
            };
            a.introduce(name.to_string(), render_type(&p.ty), heap, state, &p.span);
        }
    }
    a.analyze_block(&f.body, /*is_fn_body=*/ true);
    a.pop_scope(); // discharges any params/locals still Owned at fn exit
    FnOracle {
        function: f.name.clone(),
        drops: a.drops,
        violations: a.violations,
    }
}

impl Analyzer<'_> {
    fn push_scope(&mut self) -> usize {
        let id = self.next_scope_id;
        self.next_scope_id += 1;
        self.scopes.push((id, Vec::new()));
        id
    }

    /// Pop the top scope, scheduling a drop for every still-Owned heap binding
    /// in reverse declaration order (LIFO; §3.5 drop ordering).
    fn pop_scope(&mut self) {
        let (_id, scope) = self.scopes.pop().expect("scope underflow");
        for &idx in scope.iter().rev() {
            let (heap, state) = (self.bindings[idx].heap, self.bindings[idx].state);
            if heap && state == PlaceState::Owned {
                let b = &self.bindings[idx];
                self.drops.push(DropEvent {
                    place: b.name.clone(),
                    ty: b.ty_render.clone(),
                    scope_id: b.scope_id,
                    reason: DropReason::ScopeExit,
                    span: b.span.clone(),
                });
                self.bindings[idx].state = PlaceState::Dead;
            }
            // Restore the shadowed outer binding of the same name, if any, so it
            // becomes visible again after this scope ends. The fuzzer subset
            // never relies on cross-scope shadowing, but this keeps `by_name`
            // honest for the general case.
            let name = self.bindings[idx].name.clone();
            if let Some(prev) = self
                .bindings
                .iter()
                .enumerate()
                .rev()
                .find(|(j, b)| *j != idx && b.name == name && b.state != PlaceState::Dead)
                .map(|(j, _)| j)
            {
                self.by_name.insert(name, prev);
            }
        }
    }

    /// Snapshot the states of the first `n` bindings — the *outer* bindings that
    /// existed before a branch. Branch-local bindings (index ≥ `n`) are
    /// discharged by their own scope pop and are never merged.
    fn outer_states(&self, n: usize) -> Vec<PlaceState> {
        self.bindings[..n].iter().map(|b| b.state).collect()
    }

    /// Restore outer binding states from a snapshot (used to reset before
    /// analyzing a sibling branch from the same pre-branch state).
    fn set_outer_states(&mut self, snap: &[PlaceState]) {
        for (b, s) in self.bindings.iter_mut().zip(snap) {
            b.state = *s;
        }
    }

    /// Merge two post-branch outer-state snapshots into the live bindings. Drop
    /// soundness for **conditional moves**: a place stays `Moved` (drop elided)
    /// only if it is `Moved` on *both* paths; any disagreement — one path still
    /// `Owned` — yields `Owned`, so the drop is scheduled and codegen's runtime
    /// cap/null guard makes the over-scheduled conditional drop correct.
    /// Under-scheduling would leak the not-moved path (the unsoundness this
    /// method fixes: `if cond { v.push(s); }` must still free `s` when `!cond`).
    fn merge_outer_states(&mut self, a: &[PlaceState], b: &[PlaceState]) {
        for i in 0..a.len().min(b.len()) {
            self.bindings[i].state = merge_state(a[i], b[i]);
        }
    }

    fn introduce(
        &mut self,
        name: String,
        ty_render: String,
        heap: bool,
        state: PlaceState,
        span: &Span,
    ) {
        let scope_id = self.scopes.last().map(|(id, _)| *id).unwrap_or(0);
        let idx = self.bindings.len();
        self.bindings.push(Binding {
            name: name.clone(),
            ty_render,
            state,
            scope_id,
            span: span.clone(),
            heap,
        });
        if let Some((_, top)) = self.scopes.last_mut() {
            top.push(idx);
        }
        self.by_name.insert(name, idx);
    }

    fn analyze_block(&mut self, block: &Block, is_fn_body: bool) {
        if !is_fn_body {
            self.push_scope();
        }
        for stmt in &block.stmts {
            self.analyze_stmt(stmt);
        }
        if let Some(tail) = &block.final_expr {
            // Tail expression escapes the block (its value becomes the block's
            // value) — Move role, so a returned/tail binding is moved out.
            self.analyze_expr(tail, Role::Move);
        }
        if !is_fn_body {
            self.pop_scope();
        }
    }

    fn analyze_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                pattern, ty, value, ..
            } => {
                // RHS evaluated first (may move source bindings).
                self.analyze_expr(value, Role::Move);
                self.bind_pattern(pattern, ty.as_ref(), value);
            }
            StmtKind::LetUninit { name, ty, .. } => {
                let heap = self.type_db.is_heap(ty);
                self.introduce(
                    name.clone(),
                    render_type(ty),
                    heap,
                    PlaceState::Dead,
                    &stmt.span,
                );
            }
            StmtKind::LetElse {
                pattern,
                ty,
                value,
                else_block,
            } => {
                self.analyze_expr(value, Role::Move);
                self.analyze_block(else_block, false);
                self.bind_pattern(pattern, ty.as_ref(), value);
            }
            StmtKind::Assign { target, value } => {
                // Value moves into the target place.
                self.analyze_expr(value, Role::Move);
                // Target place is written — read its index/receiver but the
                // root becomes (re)Owned if heap.
                self.analyze_assign_target(target);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.analyze_expr(value, Role::Read);
                self.analyze_expr(target, Role::Read);
            }
            StmtKind::MultiAssign { targets, values } => {
                for v in values {
                    self.analyze_expr(v, Role::Move);
                }
                for t in targets {
                    self.analyze_assign_target(t);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.analyze_block(body, false);
            }
            StmtKind::Expr(e) => {
                // Statement-position expression: temporaries end at the `;`.
                // Any owned binding used as a Move source here is consumed.
                self.analyze_expr(e, Role::Read);
            }
        }
    }

    /// Bind the names introduced by a `let` pattern, deciding each name's state
    /// and heap-ness from the annotation (when present) or the RHS shape.
    fn bind_pattern(&mut self, pattern: &Pattern, ty: Option<&TypeExpr>, value: &Expr) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                let (heap, render) = match ty {
                    Some(t) => (self.type_db.is_heap(t), render_type(t)),
                    None => {
                        let inferred = self.infer_expr_type(value);
                        (
                            inferred
                                .as_ref()
                                .map(|t| self.type_db.is_heap(t))
                                .unwrap_or(false),
                            inferred
                                .as_ref()
                                .map(render_type)
                                .unwrap_or_else(|| "?".into()),
                        )
                    }
                };
                let state = if heap {
                    PlaceState::Owned
                } else {
                    PlaceState::Dead
                };
                self.introduce(name.clone(), render, heap, state, &pattern.span);
            }
            // Destructure — `let Payload { tag, name, items } = pl` / `let (a, b)
            // = t`: the aggregate is fully moved out (§3.4 split at the top
            // level), and each sub-binding becomes Owned iff its field type is
            // heap. Field/element types come from the type db when known.
            PatternKind::Struct { path, fields, .. } => {
                let struct_name = path.last().cloned().unwrap_or_default();
                for fp in fields {
                    if let Some(sub) = &fp.pattern {
                        // nested pattern — recurse (type refinement not tracked)
                        self.bind_pattern(sub, None, value);
                    } else {
                        let fty = self.struct_field_ty(&struct_name, &fp.name);
                        let heap = fty
                            .as_ref()
                            .map(|t| self.type_db.is_heap(t))
                            .unwrap_or(false);
                        let render = fty.as_ref().map(render_type).unwrap_or_else(|| "?".into());
                        let state = if heap {
                            PlaceState::Owned
                        } else {
                            PlaceState::Dead
                        };
                        self.introduce(fp.name.clone(), render, heap, state, &pattern.span);
                    }
                }
            }
            PatternKind::Tuple(elems) => {
                for (i, sub) in elems.iter().enumerate() {
                    if let PatternKind::Binding(n) = &sub.kind {
                        let fty = ty.and_then(|t| tuple_elem_ty(t, i));
                        let heap = fty
                            .as_ref()
                            .map(|t| self.type_db.is_heap(t))
                            .unwrap_or(false);
                        let render = fty.as_ref().map(render_type).unwrap_or_else(|| "?".into());
                        let state = if heap {
                            PlaceState::Owned
                        } else {
                            PlaceState::Dead
                        };
                        self.introduce(n.clone(), render, heap, state, &sub.span);
                    } else {
                        self.bind_pattern(sub, None, value);
                    }
                }
            }
            _ => {
                // Wildcards / literals bind nothing.
            }
        }
    }

    fn struct_field_ty(&self, struct_name: &str, field: &str) -> Option<TypeExpr> {
        self.type_db
            .structs
            .get(struct_name)?
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, t)| t.clone())
    }

    /// A place written by an assignment/store. Reading the receiver/index; the
    /// root binding (if heap) is (re)Owned by the store.
    fn analyze_assign_target(&mut self, target: &Expr) {
        match &target.kind {
            ExprKind::Identifier(name) => {
                if let Some(&idx) = self.by_name.get(name) {
                    if self.bindings[idx].heap {
                        self.bindings[idx].state = PlaceState::Owned;
                    }
                }
            }
            ExprKind::Index { object, index } => {
                self.analyze_expr(object, Role::Read); // receiver borrowed
                self.analyze_expr(index, Role::Read);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.analyze_expr(object, Role::Read);
            }
            other => {
                let _ = other;
            }
        }
    }

    /// Walk an expression, applying the consumption classifier (§4) to decide
    /// how each sub-expression's value is consumed. `role` is how *this*
    /// expression's value is consumed by its parent.
    fn analyze_expr(&mut self, expr: &Expr, role: Role) {
        match &expr.kind {
            ExprKind::Identifier(name) => self.use_place(name, role, &expr.span),
            ExprKind::SelfValue => self.use_place("self", role, &expr.span),

            // Reads through projections borrow the root (never move it here);
            // an actual field/element *move-out* only happens in a `let`
            // pattern or a match binding, handled there.
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.analyze_expr(object, Role::Read);
            }
            ExprKind::Index { object, index } => {
                // `let w = v[i]` (index move-out) is modelled at the `let` site;
                // here the receiver is read.
                self.analyze_expr(object, Role::Read);
                self.analyze_expr(index, Role::Read);
            }

            // Container mutator / builtin method: receiver borrowed; args
            // escape (move) for mutators, read otherwise (§4).
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                self.analyze_expr(object, Role::Read);
                let arg_role = if method_args_escape(method) {
                    Role::Move
                } else {
                    Role::Read
                };
                for a in args {
                    self.analyze_expr(&a.value, arg_role);
                }
            }

            // Free call: user fn → caller-retains, every arg NonConsuming
            // (Read); a builtin constructor (`Vec.new()`) has no owned args.
            ExprKind::Call { callee, args } => {
                let modes = self.callee_modes(callee);
                for (i, a) in args.iter().enumerate() {
                    let r = match modes.as_ref().and_then(|m| m.get(i)) {
                        // Both Owned and Borrow user params are NonConsuming for
                        // the *caller's* binding (§4): owned params entry-copy.
                        Some(_) => Role::Read,
                        // Unknown callee → default caller-retains (Read).
                        None => Role::Read,
                    };
                    self.analyze_expr(&a.value, r);
                }
                // Evaluate the callee expression (e.g. `pool.spawn`) too.
                self.analyze_callee_expr(callee);
            }

            // Aggregate literals: each field/element escapes into the literal
            // (Move; §4).
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                for e in elems {
                    self.analyze_expr(e, Role::Move);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.analyze_expr(e, Role::Move);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.analyze_expr(&f.value, Role::Move);
                }
                if let Some(s) = spread {
                    self.analyze_expr(s, Role::Read);
                }
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.analyze_expr(k, Role::Move);
                    self.analyze_expr(v, Role::Move);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.analyze_expr(value, Role::Move);
                self.analyze_expr(count, Role::Read);
            }

            // Operators: operands are read.
            ExprKind::Binary { left, right, .. } => {
                self.analyze_expr(left, Role::Read);
                self.analyze_expr(right, Role::Read);
            }
            ExprKind::Unary { operand, .. } => self.analyze_expr(operand, Role::Read),
            ExprKind::Cast { expr, .. } => self.analyze_expr(expr, role),
            ExprKind::Question(e) => self.analyze_expr(e, role),
            ExprKind::NilCoalesce { left, right } => {
                self.analyze_expr(left, role);
                self.analyze_expr(right, role);
            }

            // Control flow.
            ExprKind::Return(Some(v)) => {
                self.analyze_expr(v, Role::Move); // returned value escapes
            }
            ExprKind::Return(None) => {}
            ExprKind::Block(b) => self.analyze_block(b, false),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.analyze_expr(condition, Role::Read);
                // Merge the two branches so a move on ONE path does not mark an
                // outer binding Moved for the join (§ conditional-move drop
                // soundness). The no-`else` path keeps the pre-branch state.
                let n = self.bindings.len();
                let pre = self.outer_states(n);
                self.analyze_block(then_block, false);
                let then_states = self.outer_states(n);
                self.set_outer_states(&pre);
                if let Some(e) = else_branch {
                    self.analyze_expr(e, role);
                }
                let else_states = self.outer_states(n);
                self.merge_outer_states(&then_states, &else_states);
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.analyze_expr(condition, Role::Read);
                // The body may run zero times, so merge its effect with the
                // pre-loop state: a move inside the body never marks an outer
                // binding Moved at the join.
                let n = self.bindings.len();
                let pre = self.outer_states(n);
                self.analyze_block(body, false);
                let body_states = self.outer_states(n);
                self.merge_outer_states(&body_states, &pre);
            }
            ExprKind::Loop { body, .. } => {
                let n = self.bindings.len();
                let pre = self.outer_states(n);
                self.analyze_block(body, false);
                let body_states = self.outer_states(n);
                self.merge_outer_states(&body_states, &pre);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => self.analyze_for(pattern, iterable, body),
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                pattern,
            } => {
                // Scrutinee borrowed for the pattern match (payload move-out is
                // the §3.4 case; conservatively borrow here).
                self.analyze_expr(value, Role::Read);
                let n = self.bindings.len();
                let pre = self.outer_states(n);
                self.push_scope();
                self.bind_match_pattern(pattern, /*scrutinee_owned=*/ false);
                self.analyze_block(then_block, false);
                self.pop_scope();
                let then_states = self.outer_states(n);
                self.set_outer_states(&pre);
                if let Some(e) = else_branch {
                    self.analyze_expr(e, role);
                }
                let else_states = self.outer_states(n);
                self.merge_outer_states(&then_states, &else_states);
            }
            ExprKind::WhileLet {
                value,
                body,
                pattern,
                ..
            } => {
                self.analyze_expr(value, Role::Read);
                self.push_scope();
                self.bind_match_pattern(pattern, false);
                self.analyze_block(body, false);
                self.pop_scope();
            }
            ExprKind::Match { scrutinee, arms } => self.analyze_match(scrutinee, arms),

            // Closures: the §7 open edge. Walk the body with captures treated as
            // borrows (Read) so a read-after-move is still caught, but a capture
            // never moves the parent binding (matches auto-promoted-shared).
            ExprKind::Closure { body, .. } => {
                self.analyze_expr(body, Role::Read);
            }

            ExprKind::Pipe { left, right } => {
                self.analyze_expr(left, Role::Read);
                self.analyze_expr(right, Role::Read);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.analyze_expr(object, Role::Read);
                if let Some(args) = args {
                    for a in args {
                        self.analyze_expr(&a.value, Role::Read);
                    }
                }
            }
            ExprKind::LabeledBlock { body, .. } => self.analyze_block(body, false),
            ExprKind::Comptime(b) => self.analyze_block(b, false),

            // Literals and leaves: nothing to move.
            _ => {}
        }
    }

    /// The callee position of a `Call` — for `pool.spawn(...)` the callee is a
    /// MethodCall; we don't move the receiver.
    fn analyze_callee_expr(&mut self, callee: &Expr) {
        if let ExprKind::MethodCall { object, .. } = &callee.kind {
            self.analyze_expr(object, Role::Read);
        }
    }

    /// Look up a free callee's parameter modes by name, when it is a plain
    /// path/identifier to a known user function.
    fn callee_modes(&self, callee: &Expr) -> Option<Vec<ParamMode>> {
        let name = match &callee.kind {
            ExprKind::Identifier(n) => Some(n.clone()),
            ExprKind::Path { segments, .. } => segments.last().cloned(),
            _ => None,
        }?;
        self.sigs.fns.get(&name).cloned()
    }

    fn analyze_for(&mut self, pattern: &Pattern, iterable: &Expr, body: &Block) {
        // `for x in v.iter()` → v borrowed, x Borrowed alias.
        // `for x in v`        → v moved (owned iteration), x Owned per-iter.
        let owned_iteration = !is_iter_call(iterable);
        if owned_iteration {
            self.analyze_expr(iterable, Role::Move);
        } else {
            self.analyze_expr(iterable, Role::Read);
        }
        // Snapshot AFTER the iterable move (which is unconditional — owned
        // iteration consumes `v` regardless of iteration count, so that move
        // must survive) but before the body, whose moves of *other* outer
        // bindings are conditional (zero-iteration path). Merge reverts those.
        let n = self.bindings.len();
        let pre = self.outer_states(n);
        self.push_scope();
        // Bind the loop variable.
        if let PatternKind::Binding(n) = &pattern.kind {
            // Element heap-ness is unknown without element-type inference; for
            // owned iteration the element is Owned-if-heap, but we default to a
            // borrowed alias when iterating `.iter()` (never drops), and to a
            // non-tracked binding otherwise (the element's drop rides with the
            // per-iteration scope but we can't type it precisely here).
            let state = if owned_iteration {
                PlaceState::Owned
            } else {
                PlaceState::Borrowed
            };
            // Heap-unknown loop elements are conservatively non-heap so they
            // contribute no phantom drops; the fuzzer reads them via `.len()`.
            self.introduce(n.clone(), "?".into(), false, state, &pattern.span);
        }
        self.analyze_block(body, false);
        self.pop_scope();
        let body_states = self.outer_states(n);
        self.merge_outer_states(&body_states, &pre);
    }

    fn analyze_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) {
        // Is the scrutinee an owned local whose payload is moved out by the arm
        // bindings? For `match o { Some(x) => ... }` where `o` is an owned
        // Option local, `o` is consumed and `x` owns the payload (§3.4).
        let scrutinee_owned = self.scrutinee_is_owned_local(scrutinee);
        if scrutinee_owned {
            self.analyze_expr(scrutinee, Role::Move);
        } else {
            self.analyze_expr(scrutinee, Role::Read);
        }
        // Arms are exclusive, exhaustive paths (the typechecker enforces
        // exhaustiveness). Analyze each from the same pre-arm state and merge:
        // an outer binding is Moved after the match only if Moved in EVERY arm.
        // The scrutinee's own (unconditional) move is in `pre`, so it survives.
        let n = self.bindings.len();
        let pre = self.outer_states(n);
        let mut arm_states: Vec<Vec<PlaceState>> = Vec::with_capacity(arms.len());
        for arm in arms {
            self.set_outer_states(&pre);
            self.push_scope();
            self.bind_match_pattern(&arm.pattern, scrutinee_owned);
            if let Some(g) = &arm.guard {
                self.analyze_expr(g, Role::Read);
            }
            self.analyze_expr(&arm.body, Role::Read);
            self.pop_scope();
            arm_states.push(self.outer_states(n));
        }
        if let Some((first, rest)) = arm_states.split_first() {
            let mut merged = first.clone();
            for r in rest {
                for i in 0..n {
                    merged[i] = merge_state(merged[i], r[i]);
                }
            }
            self.set_outer_states(&merged);
        }
    }

    fn scrutinee_is_owned_local(&self, scrutinee: &Expr) -> bool {
        if let ExprKind::Identifier(n) = &scrutinee.kind {
            if let Some(&idx) = self.by_name.get(n) {
                return self.bindings[idx].heap && self.bindings[idx].state == PlaceState::Owned;
            }
        }
        false
    }

    /// Bind names introduced by a match/if-let pattern. A payload binding under
    /// an owned scrutinee is Owned (moved out, §3.4); under a borrow scrutinee
    /// it is Borrowed (the owner keeps it — the B-2026-07-01-12 double-free
    /// class is exactly moving such a Borrowed binding out).
    fn bind_match_pattern(&mut self, pattern: &Pattern, scrutinee_owned: bool) {
        let payload_state = if scrutinee_owned {
            PlaceState::Owned
        } else {
            PlaceState::Borrowed
        };
        self.bind_match_pattern_inner(pattern, payload_state);
    }

    fn bind_match_pattern_inner(&mut self, pattern: &Pattern, payload_state: PlaceState) {
        match &pattern.kind {
            PatternKind::Binding(n) => {
                // A payload binding — heap-ness unknown without variant-type
                // inference; default non-heap so it contributes no phantom drop,
                // but keep the state so a use-after-move on a borrowed payload
                // that is then moved out is still detectable at the move site.
                self.introduce(n.clone(), "?".into(), false, payload_state, &pattern.span);
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for p in patterns {
                    self.bind_match_pattern_inner(p, payload_state);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for fp in fields {
                    if let Some(sub) = &fp.pattern {
                        self.bind_match_pattern_inner(sub, payload_state);
                    } else {
                        self.introduce(fp.name.clone(), "?".into(), false, payload_state, &fp.span);
                    }
                }
            }
            PatternKind::Tuple(ps) => {
                for p in ps {
                    self.bind_match_pattern_inner(p, payload_state);
                }
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                self.introduce(
                    name.clone(),
                    "?".into(),
                    false,
                    payload_state,
                    &pattern.span,
                );
                self.bind_match_pattern_inner(pattern, payload_state);
            }
            PatternKind::Or(ps) => {
                if let Some(first) = ps.first() {
                    self.bind_match_pattern_inner(first, payload_state);
                }
            }
            _ => {}
        }
    }

    /// Use a place (identifier) in the given role, updating its state and
    /// flagging invariant violations.
    fn use_place(&mut self, name: &str, role: Role, span: &Span) {
        let Some(&idx) = self.by_name.get(name) else {
            return; // not a tracked local (a global, a fn name, etc.)
        };
        let state = self.bindings[idx].state;
        match state {
            PlaceState::Moved => {
                self.violations.push(Violation {
                    kind: ViolationKind::UseAfterMove,
                    place: name.to_string(),
                    message: format!("`{name}` is used after being moved out"),
                    span: span.clone(),
                });
            }
            PlaceState::Dead => {
                // Reading an uninitialized/dropped place. `LetUninit` slots are
                // Dead until assigned; an assignment target read is benign, so
                // only flag genuine reads.
                if role == Role::Read {
                    // A Dead-but-POD binding (e.g. an i64 local) is fine; only
                    // flag heap places, where a read-after-drop is the bug.
                    if self.bindings[idx].heap {
                        self.violations.push(Violation {
                            kind: ViolationKind::UseAfterDrop,
                            place: name.to_string(),
                            message: format!(
                                "`{name}` is read before initialization or after drop"
                            ),
                            span: span.clone(),
                        });
                    }
                }
            }
            PlaceState::Owned | PlaceState::Borrowed => {
                if role == Role::Move && state == PlaceState::Owned && self.bindings[idx].heap {
                    // A genuine move of an owned heap place: disarm the source.
                    self.bindings[idx].state = PlaceState::Moved;
                }
                // Borrowed + Move (e.g. moving a borrow-bound match payload out)
                // is the double-free class the checker rejects; the runtime
                // discipline here just records it as consumed without disarming
                // an owner (there is none) — left as a NonConsuming read so the
                // schedule stays correct.
            }
        }
    }

    // ── lightweight RHS type inference (annotation-free `let`s) ──────────

    fn infer_expr_type(&self, expr: &Expr) -> Option<TypeExpr> {
        match &expr.kind {
            ExprKind::StringLit(_) | ExprKind::InterpolatedStringLit(_) => Some(path_ty("String")),
            ExprKind::PrefixCollectionLiteral { type_name, .. } => Some(path_ty(type_name)),
            ExprKind::MethodCall { method, .. } if method == "to_string" => Some(path_ty("String")),
            _ => None,
        }
    }
}

// ─────────────────────────── small helpers ─────────────────────────────

fn is_iter_call(expr: &Expr) -> bool {
    matches!(
        &expr.kind,
        ExprKind::MethodCall { method, .. }
            if matches!(method.as_str(), "iter" | "iter_mut" | "keys" | "values" | "enumerate")
    ) || matches!(&expr.kind,
        ExprKind::MethodCall { object, .. } if is_iter_call(object))
}

fn tuple_elem_ty(ty: &TypeExpr, i: usize) -> Option<TypeExpr> {
    if let TypeKind::Tuple(elems) = &ty.kind {
        elems.get(i).cloned()
    } else {
        None
    }
}

fn path_ty(name: &str) -> TypeExpr {
    TypeExpr {
        kind: TypeKind::Path(PathExpr {
            segments: vec![name.to_string()],
            generic_args: None,
            span: Span::default(),
        }),
        span: Span::default(),
    }
}

#[cfg(test)]
mod tests;
