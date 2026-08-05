//! E_CONCURRENT_SHARED_STRUCT + E_CONCURRENT_PLAIN_STRUCT — phase-7 line 197.
//!
//! Detects struct/enum bindings that are referenced from two or more
//! top-level statements (branches) of the same `par {}` block.
//!
//! Two related diagnostics, one detection pass:
//!
//! - **`E_CONCURRENT_SHARED_STRUCT`** — `shared struct` / `shared enum`
//!   binding. Per design.md § Rc vs Arc — Two-Phase Algorithm "Rule for
//!   `shared struct`": `live_range(v) ∩ parallel_region ≠ ∅` AND
//!   reachable from > 1 concurrent branch → compile error. Sole-
//!   ownership move into exactly one branch is not an error.
//! - **`E_CONCURRENT_PLAIN_STRUCT`** — plain (non-shared) `struct`
//!   binding. Per design.md § Compiler-assisted migration from plain
//!   `struct` to `par struct`: silent promotion is rejected (the field
//!   constraints differ structurally), so the compiler emits the
//!   structured error and the programmer migrates to `par struct`.
//!
//! Both diagnostics carry the same `fix_diff` envelope: per-`mut`-field
//! `Mutex[T]` wrap edits derivable from each `StructField.ty.span` —
//! two pure-insertion `TextEdit`s per field (`Mutex[` prefix + `]`
//! suffix around the field's type). Stored on
//! `OwnershipChecker.error_fix_diffs` keyed by the diagnostic's primary
//! `SpanKey`, surfaced via `OwnershipCheckResult.error_fix_diffs`.
//!
//! Keyword rename (`shared struct` / `struct` → `par struct`) and the
//! `mut ` keyword stripping live in suggestion prose until the parser
//! exposes keyword spans on `StructDef`. Lock-block insertion at write
//! sites and Rc→Arc clone-semantics notes are sibling follow-ups.
//!
//! Scope (v1):
//! - Direct binding-name references within branch statements count.
//! - Field-access (`s.field`) and method-call (`s.method(...)`)
//!   receivers count via the inner Identifier walk.
//! - Closure captures count via `closure_bindings` expansion (mirrors
//!   the round-12.34 mechanism in `par_helpers.rs`): a `let f = ||
//!   use(c);` registration plus a sibling-branch `spawn(f)` counts as
//!   a branch-use of `c`. Inline `spawn(|| use(c))` closures count via
//!   `OwnershipChecker.closure_captures` lookup at the closure's span.
//! - `spawn(...)` boundaries beyond a `par {}` block fall outside the
//!   v1 detection — sibling follow-up.

// The `karac fix`/`karac migrate` edit-builders in this module (everything
// re-exported from `ownership.rs` and their private helpers) are reached only
// through `crate::cli`, which is native-only. On the wasm32 playground build
// that whole half is unreachable; only `check_concurrent_shared_struct` (the
// diagnostic pass the playground runs) stays live. Allow the resulting dead
// code on wasm32 without weakening the native dead-code gate.
#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::ownership::{stdlib_method_self_borrow_kind, BorrowKind, PlaceExpr, Projection};
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;

use super::{OwnershipError, OwnershipErrorKind, OwnershipMode};

type BindingTypeMap = HashMap<SpanKey, String>;
type ClosureCaptures = HashMap<SpanKey, Vec<(String, OwnershipMode)>>;
type ClosureBindings = HashMap<String, Vec<String>>;

/// L205 — bundles the two maps needed to decide whether a `MethodCall`
/// mutates its receiver: the typechecker's per-call-site canonical
/// `Type.method` key (`method_callee_types`) plus the receiver-mode
/// classifier (`method_self_modes` for user impl methods; fallback to
/// `stdlib_method_self_borrow_kind` for built-in `Vec`/`Map`/etc.
/// methods). Threaded through the par-conflict scan so the lock-block
/// edit emitter at `build_lock_block_edits_for_binding` can wrap
/// mutating method-call writes (`c.field.push(x)`) alongside the
/// L201b-shipped assign / compound-assign cases.
struct MethodMutClassifier<'a> {
    method_callee_types: &'a HashMap<SpanKey, String>,
    method_self_modes: &'a HashMap<String, SelfParam>,
}

impl MethodMutClassifier<'_> {
    /// Whether the method call at `method_call_span` mutates its
    /// receiver. Returns `false` when the typechecker didn't record a
    /// callee key (resolution failure upstream) or when the method is
    /// a read-only / consuming receiver. Conservative default: if the
    /// signal is missing, assume non-mutating (no wrap emitted).
    fn is_mutating(&self, method_call_span: &Span) -> bool {
        let key = match self
            .method_callee_types
            .get(&SpanKey::from_span(method_call_span))
        {
            Some(k) => k,
            None => return false,
        };
        if let Some(self_param) = self.method_self_modes.get(key) {
            return matches!(self_param, SelfParam::MutRef);
        }
        matches!(
            stdlib_method_self_borrow_kind(key),
            Some(BorrowKind::MutRef)
        )
    }
}

/// Discriminator carried alongside each tracked binding so one walk
/// catches both diagnostic flavors without two parallel maps.
///
/// Exposed at crate visibility so `cli::cmd_migrate` (phase-7 L215a) can
/// reuse [`build_fix_diff_edits`] to produce the same type-definition
/// rewrite edits the `karac fix` diagnostic path emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingKind {
    Shared,
    Plain,
}

/// Controls the textual shape of the `lock { ... }` block emitted by the
/// L201b/L207 walker.
///
/// - `Shorthand` (`lock <field> { ... }`) — used by `karac fix`'s
///   E_CONCURRENT_SHARED_STRUCT path. The wrap sits inside an impl
///   method body's par block where `self` is in scope and the field-
///   shorthand resolves; inner `<binding>.<field>` accesses are left
///   untouched.
/// - `SelfPrefix` (`lock self.<field> { ... }` plus `<binding>.<field>`
///   → `self.<field>` rewrite inside the wrap body) — used by
///   `karac migrate shared-to-par` (L215b2) per design.md line 8522.
///   The output compiles cleanly when the surrounding context already
///   has `self` in scope (impl method bodies); in free functions
///   taking the binding by `ref` / `mut ref`, the wrap references
///   `self` which isn't in scope, and the reviewer hand-completes the
///   refactor — consistent with the "always **manual at the review
///   step**" clause of design.md § Compiler-assisted migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WrapShape {
    Shorthand,
    SelfPrefix,
}

#[derive(Debug, Clone)]
struct TrackedBinding {
    type_name: String,
    kind: BindingKind,
    /// B-2026-08-01-33 — the fields of `type_name` a par branch may project
    /// WITHOUT emitting any refcount traffic: declared immutable (absent from
    /// [`crate::typechecker::env::StructInfo::mut_fields`]) and of scalar
    /// `Copy` type. Reading one lowers to a plain deref of the handle's
    /// payload, so it neither touches the RC header nor copies a buffer.
    ///
    /// Carried on the tracked binding rather than looked up at the use site
    /// so the per-branch identifier walk — which already threads `tracked`
    /// through ~40 recursive call sites — needs no extra parameter.
    ///
    /// Empty for a `Plain` binding: this relaxation is about refcount
    /// atomicity, and `E_CONCURRENT_PLAIN_STRUCT` fires for an unrelated
    /// reason (the `par struct` migration is structural, not RC-driven).
    readonly_scalar_fields: HashSet<String>,
    /// B-2026-08-01-33 mechanism 2 — when `Some`, the set of `shared` type
    /// names transitively reachable from `type_name` (itself included), ALL of
    /// which are free of `mut` fields. Such a value is deeply immutable, so
    /// promoting every one of those types to ATOMIC refcounting makes it safe
    /// to reach from sibling branches: the header races vanish (that is what
    /// the promotion buys) and there is no payload to race in the first place
    /// (that is what the immutability buys).
    ///
    /// `None` — the common case — when any reachable type has a `mut` field.
    /// Atomic RC would fix only the HEADER; a `mut` payload written from two
    /// branches is still a data race, which is precisely why `par struct`
    /// forces `Atomic[T]` / `Mutex[T]` on its mutable fields. Distinguishing a
    /// write to a branch-LOCAL instance from one to the shared value needs
    /// instance-level escape analysis (mechanism 1), not a type-level check.
    atomic_promotion: Option<Vec<String>>,
}

/// One branch's reference to a tracked binding, tagged with whether it can
/// emit refcount traffic — B-2026-08-01-33.
///
/// `material: false` marks the one shape proven not to: reading an immutable
/// scalar field off the handle (`root.a`), which lowers to a plain payload
/// deref. Every other reference — a bare identifier, a method receiver, a
/// by-value pass, a closure capture, a projection of a `mut` or non-scalar
/// field — is `material: true`.
///
/// The polarity is deliberate and matches `iter_local`'s: the walk only ever
/// marks a use NON-material on positive structural evidence, from one arm it
/// must reach explicitly. An `ExprKind` this pass does not special-case falls
/// through to the ordinary identifier walk and lands here material, so a
/// language addition costs a spurious error, never an admitted race.
#[derive(Debug, Clone)]
struct BranchUse {
    span: Span,
    material: bool,
}

/// Record a branch's reference to `name`, keeping the strongest classification
/// seen: once any reference in the branch is material the binding stays
/// material, and the reported span moves to that reference (the diagnostic
/// should point at the use that actually races, not at a benign projection
/// earlier in the same branch).
fn record_use(out: &mut HashMap<String, BranchUse>, name: String, span: Span, material: bool) {
    match out.get_mut(&name) {
        Some(existing) => {
            if material && !existing.material {
                existing.material = true;
                existing.span = span;
            }
        }
        None => {
            out.insert(name, BranchUse { span, material });
        }
    }
}

impl<'a> super::OwnershipChecker<'a> {
    /// Walk every function body, collect bindings whose surface type is
    /// a (shared OR plain) `struct` / `shared enum`, and emit
    /// `E_CONCURRENT_SHARED_STRUCT` / `E_CONCURRENT_PLAIN_STRUCT` for
    /// any such binding referenced from more than one branch (top-level
    /// statement) of the same `par {}` block. Plain enums and unions
    /// are skipped — design.md only calls out the struct/shared-struct/
    /// shared-enum cases.
    pub(crate) fn check_concurrent_shared_struct(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        let mut errors: Vec<OwnershipError> = Vec::new();
        let mut fix_diffs: HashMap<SpanKey, Vec<TextEdit>> = HashMap::new();
        // B-2026-08-01-33 mechanism 2 — `shared` types this pass promoted to
        // ATOMIC refcounting so a multi-branch capture could be admitted.
        // Consumed by codegen's `heap_type_uses_atomic_rc`.
        let mut promoted: HashSet<String> = HashSet::new();
        let closure_captures = &self.closure_captures;
        let classifier = MethodMutClassifier {
            method_callee_types: &self.typecheck_result.method_callee_types,
            method_self_modes: &self.method_self_modes,
        };
        for item in &items {
            match item {
                Item::Function(f) => {
                    let tracked = self.collect_tracked_bindings(&f.params, &f.body);
                    if !tracked.is_empty() {
                        let mut closure_bindings = ClosureBindings::new();
                        build_closure_bindings(&f.body, closure_captures, &mut closure_bindings);
                        scan_block_for_par_conflicts(
                            &f.body,
                            &tracked,
                            &items,
                            closure_captures,
                            &closure_bindings,
                            &classifier,
                            &mut errors,
                            &mut fix_diffs,
                            &mut promoted,
                        );
                    }
                }
                Item::ImplBlock(imp) => {
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let tracked = self.collect_tracked_bindings(&m.params, &m.body);
                            if !tracked.is_empty() {
                                let mut closure_bindings = ClosureBindings::new();
                                build_closure_bindings(
                                    &m.body,
                                    closure_captures,
                                    &mut closure_bindings,
                                );
                                scan_block_for_par_conflicts(
                                    &m.body,
                                    &tracked,
                                    &items,
                                    closure_captures,
                                    &closure_bindings,
                                    &classifier,
                                    &mut errors,
                                    &mut fix_diffs,
                                    &mut promoted,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.errors.extend(errors);
        self.error_fix_diffs.extend(fix_diffs);
        self.atomic_promoted_types.extend(promoted);
    }

    /// Collect bindings (parameters + let-introduced) whose surface
    /// type resolves to either a `shared struct` / `shared enum` OR a
    /// plain (non-shared) `struct`. The discriminator is recorded as
    /// `BindingKind` so the per-branch detector picks the right
    /// diagnostic flavor without re-querying. Plain enums and unions
    /// fall through (no migration target spec'd).
    fn collect_tracked_bindings(
        &self,
        params: &[Param],
        body: &Block,
    ) -> HashMap<String, TrackedBinding> {
        let mut tracked = HashMap::new();
        for p in params {
            let Some(name) = p.name() else { continue };
            let Some(head) = type_expr_head_name(&p.ty) else {
                continue;
            };
            if let Some((kind, readonly_scalar_fields, atomic_promotion)) =
                self.classify_tracked_binding(&head)
            {
                tracked.insert(
                    name.to_string(),
                    TrackedBinding {
                        type_name: head,
                        kind,
                        readonly_scalar_fields,
                        atomic_promotion,
                    },
                );
            }
        }
        collect_let_tracked_bindings(
            body,
            &self.typecheck_result.pattern_binding_types,
            &mut tracked,
            |n| self.classify_tracked_binding(n),
        );
        tracked
    }

    /// Return `Shared` for `shared struct` / `shared enum`, `Plain` for
    /// non-shared `struct`, `None` for anything else (plain enum,
    /// union, primitive, generic param, `par struct` / `par enum`, …).
    ///
    /// `par struct` / `par enum` return `None` — they are **cross-task-safe by
    /// definition** (always Arc; every `mut` field is `Atomic[T]` / `Mutex[T]`,
    /// enforced at the definition site by Slice A). The whole point of `par`
    /// types is that they cross `par {}` / `spawn` / `TaskGroup` boundaries
    /// freely, so a `par` binding reachable from 2+ sibling branches is safe,
    /// not an `E_CONCURRENT_*` error. (design.md § Part 5b > "Crossing parallel
    /// region boundaries".) The `is_par` check precedes `is_shared`; the two
    /// are mutually exclusive but the order makes the exemption explicit.
    fn classify_binding_type(&self, name: &str) -> Option<BindingKind> {
        // Concurrency primitives — `Atomic[T]`, `Mutex[T]`, `RwLock[T]`,
        // `Arc[..]` — are cross-task-safe BY DEFINITION: they are the
        // design-sanctioned escape for sharing mutable state across `par {}` /
        // `spawn` / TaskGroup boundaries (design.md §1329 concurrency rule +
        // § Part 5b). Their par captures are shared by POINTER, not copied by
        // value, so every branch RMW / lock hits the one cell
        // (B-2026-07-18-28) — a binding of one of these types reachable from
        // 2+ sibling branches is safe, not an `E_CONCURRENT_*` conflict. The
        // baked stdlib registers `struct Atomic[T] {}` / `Mutex[T]` / … as
        // ordinary structs in `struct_info`, so WITHOUT this early exemption
        // the plain-`struct` arm below mis-classifies them as `Plain` and the
        // sanctioned pattern is rejected at `karac check`. Same `None`-means-
        // exempt contract as the `par struct` / `par enum` arms below.
        if matches!(name, "Atomic" | "Mutex" | "RwLock" | "Arc") {
            return None;
        }
        if let Some(info) = self.typecheck_result.struct_info.get(name) {
            if info.is_par {
                return None;
            }
            return Some(if info.is_shared {
                BindingKind::Shared
            } else {
                BindingKind::Plain
            });
        }
        if let Some(info) = self.typecheck_result.enum_info.get(name) {
            if info.is_par {
                return None;
            }
            if info.is_shared {
                return Some(BindingKind::Shared);
            }
        }
        None
    }

    /// B-2026-08-01-33 — the fields of `type_name` whose read from a par
    /// branch is provably free of refcount traffic. Populates
    /// [`TrackedBinding::readonly_scalar_fields`]; see
    /// [`Self::classify_tracked_binding`] for the safety argument.
    ///
    /// Two structural conditions, both fail-closed (an unknown type, an enum,
    /// or a field this pass cannot resolve yields the empty set, i.e. today's
    /// unconditional conflict):
    ///
    /// 1. **Immutable** — the field is absent from `StructInfo::mut_fields`,
    ///    which for a `shared` struct is exactly the set assignable through a
    ///    handle (declared `mut`, or an interior-mutable `Atomic[T]` /
    ///    `Mutex[T]`). An immutable field cannot be written by ANY branch, so
    ///    admitting concurrent readers cannot introduce a payload race and the
    ///    walk needs no write-position tracking. This is the same guarantee
    ///    `par struct` gives ("immutable fields are freely readable across
    ///    tasks") applied to the `shared` tier.
    /// 2. **Scalar** — `is_copy_type_basic`, deliberately NARROWER than
    ///    `is_copy_type`: no `ref`/tuple/array/SIMD arms. Reading a scalar
    ///    copies a register; a compound field would raise buffer-aliasing
    ///    questions this entry has not measured.
    fn readonly_scalar_fields(&self, type_name: &str) -> HashSet<String> {
        let Some(info) = self.typecheck_result.struct_info.get(type_name) else {
            return HashSet::new();
        };
        info.fields
            .iter()
            .filter(|(fname, fty, _)| {
                !info.mut_fields.contains(fname) && super::is_copy_type_basic(fty)
            })
            .map(|(fname, _, _)| fname.clone())
            .collect()
    }

    /// The set of `shared` types transitively reachable from `type_name` when
    /// EVERY type in that closure is free of `mut` fields; `None` otherwise.
    /// In one word: is `type_name` **deeply immutable**, and if so, what is
    /// reachable from it?
    ///
    /// TWO CONSUMERS, deliberately one definition. B-2026-08-01-33 mechanism 2
    /// (atomic promotion) uses the returned set as the types to promote.
    /// Mechanism 3's freeze-site check (`frozen_freeze_site.rs`) uses only
    /// `is_some()` — a `frozen T` claims deep immutability, and stage 1 has no
    /// per-instance check to back that claim, so it demands the type provide it
    /// structurally. Both ask the same question; splitting it into two
    /// predicates would let the compiler hold two different opinions about what
    /// "deeply immutable" means.
    ///
    /// Promoting that whole set to atomic refcounting is what makes a
    /// multi-branch capture safe, and it has to be the whole set, not just the
    /// root: a branch traversing the structure retains the interior handles it
    /// walks, and those are ordinary `shared` values whose inc/dec would stay
    /// non-atomic if only the root were promoted. That is B-2026-07-28-13's
    /// race, and it is why the closure below follows fields rather than
    /// stopping at the named type.
    ///
    /// Fail-closed at every step: an unresolvable type name, a generic
    /// parameter, or any `mut` field anywhere in the closure yields `None`,
    /// i.e. today's unconditional conflict.
    pub(super) fn deep_immutability_closure(&self, type_name: &str) -> Option<Vec<String>> {
        // NOTE: the env gate lives at the ADMIT site, not here. The closure is
        // computed unconditionally because the DIAGNOSTIC needs it too: an
        // immutable type gets a one-keyword suggestion, a `mut`-bearing one
        // gets the full migration. Gating the computation would make the
        // diagnostic quality depend on an unrelated opt-in.
        //
        // Promotion itself is DEFAULT OFF (`KARAC_PAR_ATOMIC_PROMOTION=1`).
        //
        // Admitting this capture is not a precision fix to an over-approximating
        // gate — it CHANGES A DOCUMENTED LANGUAGE RULE. design.md § Rc vs Arc —
        // Two-Phase Algorithm states that a `shared struct` reachable from more
        // than one concurrent branch is a compile error, and six tests encode
        // that rule directly. The mechanism below is implemented and verified
        // (every refcount op on a promoted type censuses atomic, on all four
        // par branches of the probe, including one building a fresh local
        // instance), but flipping it on by default is a spec decision for the
        // language owner, not something a soundness argument alone settles.
        //
        // So it ships inert, the way `KARAC_RC_ELIDE_REF_PARAMS` and
        // `KARAC_PAR_ITER_LOCAL_SHARED` did before their defaults moved: the
        // capability is available to evaluate, and nothing changes until
        // someone decides it should.
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = vec![type_name.to_string()];
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            // Only `shared` struct/enum types carry a refcount to promote. A
            // plain aggregate reached through a field is copied by value and
            // has no header, so it neither needs promotion nor blocks it —
            // but its OWN fields may reach further `shared` types, so the walk
            // continues through it.
            if let Some(info) = self.typecheck_result.struct_info.get(&name) {
                if !info.mut_fields.is_empty() {
                    return None;
                }
                for (_, fty, _) in &info.fields {
                    collect_named_types(fty, &mut queue);
                }
                continue;
            }
            if let Some(info) = self.typecheck_result.enum_info.get(&name) {
                for (_, payload) in &info.variants {
                    match payload {
                        crate::typechecker::VariantTypeInfo::Unit => {}
                        crate::typechecker::VariantTypeInfo::Tuple(tys) => {
                            for t in tys {
                                collect_named_types(t, &mut queue);
                            }
                        }
                        crate::typechecker::VariantTypeInfo::Struct(fields) => {
                            for (_, t) in fields {
                                collect_named_types(t, &mut queue);
                            }
                        }
                    }
                }
                continue;
            }
            // Not a user aggregate — a builtin container or scalar. Its own
            // element types were already queued by `collect_named_types`.
        }
        // Keep only the types that actually have a refcount to promote.
        let promoted: Vec<String> = seen
            .into_iter()
            .filter(|n| {
                self.typecheck_result
                    .struct_info
                    .get(n)
                    .is_some_and(|i| i.is_shared)
                    || self
                        .typecheck_result
                        .enum_info
                        .get(n)
                        .is_some_and(|i| i.is_shared)
            })
            .collect();
        (!promoted.is_empty()).then_some(promoted)
    }

    /// Pairs [`Self::classify_binding_type`] with the B-2026-08-01-33
    /// read-only projection set, so all three `TrackedBinding` construction
    /// sites derive both from one call.
    ///
    /// WHY A `shared` BINDING MAY BE READ FROM SIBLING BRANCHES AT ALL, which
    /// the unconditional gate this refines did not distinguish. Measured on
    /// x86-64 by disassembling `__par_branch_0_0` for three programs differing
    /// only in how the branch touches a captured `shared struct` binding:
    ///
    /// ```text
    ///   branch body                        atomic rc ops   PLAIN (racy) rc ops
    ///   root.a  (immutable i64 field)            2                  0
    ///   take(root)      (root by value)          3                  0
    ///   take(root.inner)  (nested handle)        3                  1
    /// ```
    ///
    /// The first two are `lock incq` / `lock decq` throughout — the
    /// `ParCaptureMode::SharedRc` prologue pair, plus (for the by-value case)
    /// the `arc_values` promotion that `classify_par_capture_modes` applies to
    /// the captured binding. Only materializing a NESTED handle emits an
    /// unlocked `decq`, which is B-2026-07-28-13's race and must keep failing.
    ///
    /// So only condition 1 of the entry's three candidate mechanisms is
    /// exercised here, in its degenerate form: rather than ELIDING refcount
    /// traffic, this admits exactly the shapes that provably emit none. The
    /// gate is not loosened for anything that does.
    ///
    /// Deliberately NOT admitted, though it measured clean above: passing the
    /// root by value (`take(root)`). One disassembly of one shape is not
    /// enough to relax a gate whose failure mode is a SIGSEGV — a consuming
    /// callee that stores the handle raises lifetime questions this pass does
    /// not answer. Recorded in the ledger with the measurement so the next
    /// widening starts from evidence.
    fn classify_tracked_binding(
        &self,
        name: &str,
    ) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> {
        let kind = self.classify_binding_type(name)?;
        let (fields, promotion) = match kind {
            BindingKind::Shared => (
                self.readonly_scalar_fields(name),
                self.deep_immutability_closure(name),
            ),
            // `E_CONCURRENT_PLAIN_STRUCT` fires for a structural reason, not an
            // RC one — a plain struct has no refcount to promote.
            BindingKind::Plain => (HashSet::new(), None),
        };
        Some((kind, fields, promotion))
    }
}

/// Scan `body` for `ExprKind::Par` blocks; for each, walk every
/// top-level statement (branch) collecting referenced names, and
/// emit one diagnostic per binding present in more than one branch.
#[allow(clippy::too_many_arguments)] // L205 threads classifier alongside existing detector args
fn scan_block_for_par_conflicts(
    block: &Block,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    classifier: &MethodMutClassifier,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
    promoted: &mut HashSet<String>,
) {
    for stmt in &block.stmts {
        scan_stmt_for_par_conflicts(
            stmt,
            tracked,
            program_items,
            closure_captures,
            closure_bindings,
            classifier,
            errors,
            fix_diffs,
            promoted,
        );
    }
    if let Some(e) = &block.final_expr {
        scan_expr_for_par_conflicts(
            e,
            tracked,
            program_items,
            closure_captures,
            closure_bindings,
            classifier,
            errors,
            fix_diffs,
            promoted,
        );
    }
}

fn type_expr_head_name(ty: &TypeExpr) -> Option<String> {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Weak(inner) => {
            type_expr_head_name(inner)
        }
        _ => None,
    }
}

/// Walk a block looking for `let pat = rhs;` forms and record every
/// leaf binding whose `pattern_binding_types`-recorded surface type
/// resolves to a tracked kind. Threads through nested blocks.
fn collect_let_tracked_bindings(
    block: &Block,
    pattern_binding_types: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> + Copy,
) {
    for stmt in &block.stmts {
        collect_let_in_stmt(stmt, pattern_binding_types, out, classify);
    }
    if let Some(e) = &block.final_expr {
        collect_let_in_expr(e, pattern_binding_types, out, classify);
    }
}

fn collect_let_in_stmt(
    stmt: &Stmt,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> + Copy,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { pattern, value, .. } | StmtKind::LetElse { pattern, value, .. } => {
            record_pattern_bindings(pattern, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
            if let StmtKind::LetElse { else_block, .. } = &stmt.kind {
                collect_let_tracked_bindings(else_block, pbt, out, classify);
            }
        }
        StmtKind::LetUninit { .. } => {
            // Uninitialized bindings are restricted to a single name with
            // a required type annotation. Plain-struct uninitialized
            // bindings are flagged at first read by definite-assignment
            // analysis (UseOfUninitialized) before they could reach a
            // par-block; shared-struct uninitialized bindings cannot
            // construct via let-uninit at all. Skip safely.
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        StmtKind::Assign { target, value } => {
            collect_let_in_expr(target, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_let_in_expr(target, pbt, out, classify);
            collect_let_in_expr(value, pbt, out, classify);
        }
        StmtKind::Expr(e) => {
            collect_let_in_expr(e, pbt, out, classify);
        }
    }
}

fn record_pattern_bindings(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> + Copy,
) {
    record_pattern_inner(pattern, pbt, out, classify);
}

/// Queue every user-aggregate name reachable from `ty`, following containers,
/// tuples, borrows and generic args.
///
/// EXHAUSTIVE by construction — a `_` arm here would silently stop the walk at
/// a type variant added later, which would drop a reachable `shared` type out
/// of the promotion closure and leave its refcount non-atomic while its
/// neighbours were promoted. That is a race the promotion is supposed to
/// remove, so the match must break the build instead.
fn collect_named_types(ty: &crate::typechecker::Type, out: &mut Vec<String>) {
    use crate::typechecker::Type as T;
    match ty {
        T::Named { name, args } => {
            out.push(name.clone());
            for a in args {
                collect_named_types(a, out);
            }
        }
        T::Shared(name) => out.push(name.clone()),
        T::Rc(inner) | T::Arc(inner) | T::Ref(inner) | T::MutRef(inner) => {
            collect_named_types(inner, out)
        }
        T::Tuple(tys) => {
            for t in tys {
                collect_named_types(t, out);
            }
        }
        T::Array { element, .. } | T::Vector { element, .. } | T::Slice { element, .. } => {
            collect_named_types(element, out)
        }
        T::Function {
            params,
            return_type,
        }
        | T::OnceFunction {
            params,
            return_type,
        } => {
            for pth in params {
                collect_named_types(pth, out);
            }
            collect_named_types(return_type, out);
        }
        // Scalars and markers reach nothing.
        T::Int(_) | T::UInt(_) | T::Float(_) | T::Bool | T::Char | T::Str | T::Unit | T::Never => {}
        other => {
            // Anything not enumerated above is treated as opaque AND
            // poisoning: push a name that cannot resolve so the closure's
            // caller fails closed rather than under-approximating.
            let _ = other;
            out.push("__karac_unresolved_type__".to_string());
        }
    }
}

fn record_pattern_inner(
    pattern: &Pattern,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> + Copy,
) {
    match &pattern.kind {
        PatternKind::Binding(name) => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if let Some((kind, readonly_scalar_fields, atomic_promotion)) = classify(head) {
                    out.insert(
                        name.clone(),
                        TrackedBinding {
                            type_name: head.clone(),
                            kind,
                            readonly_scalar_fields,
                            atomic_promotion,
                        },
                    );
                }
            }
        }
        PatternKind::AtBinding {
            name, pattern: sub, ..
        } => {
            let key = SpanKey::from_span(&pattern.span);
            if let Some(head) = pbt.get(&key) {
                if let Some((kind, readonly_scalar_fields, atomic_promotion)) = classify(head) {
                    out.insert(
                        name.clone(),
                        TrackedBinding {
                            type_name: head.clone(),
                            kind,
                            readonly_scalar_fields,
                            atomic_promotion,
                        },
                    );
                }
            }
            record_pattern_inner(sub, pbt, out, classify);
        }
        PatternKind::Tuple(items) => {
            for p in items {
                record_pattern_inner(p, pbt, out, classify);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                if let Some(p) = &f.pattern {
                    record_pattern_inner(p, pbt, out, classify);
                }
            }
        }
        PatternKind::TupleVariant { patterns, .. } => {
            for p in patterns {
                record_pattern_inner(p, pbt, out, classify);
            }
        }
        _ => {}
    }
}

fn collect_let_in_expr(
    expr: &Expr,
    pbt: &BindingTypeMap,
    out: &mut HashMap<String, TrackedBinding>,
    classify: impl Fn(&str) -> Option<(BindingKind, HashSet<String>, Option<Vec<String>>)> + Copy,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            collect_let_tracked_bindings(b, pbt, out, classify);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_let_in_expr(condition, pbt, out, classify);
            collect_let_tracked_bindings(then_block, pbt, out, classify);
            if let Some(else_b) = else_branch {
                collect_let_in_expr(else_b, pbt, out, classify);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_let_in_expr(condition, pbt, out, classify);
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_let_in_expr(iterable, pbt, out, classify);
            collect_let_tracked_bindings(body, pbt, out, classify);
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)] // L205 threads classifier alongside existing detector args
fn scan_stmt_for_par_conflicts(
    stmt: &Stmt,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    classifier: &MethodMutClassifier,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
    promoted: &mut HashSet<String>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } => {
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for s in &else_block.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &else_block.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        StmtKind::Assign { target, value } => {
            scan_expr_for_par_conflicts(
                target,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            scan_expr_for_par_conflicts(
                target,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            scan_expr_for_par_conflicts(
                value,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        StmtKind::Expr(e) => {
            scan_expr_for_par_conflicts(
                e,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // L205 threads classifier alongside existing detector args
fn scan_expr_for_par_conflicts(
    expr: &Expr,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    classifier: &MethodMutClassifier,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
    promoted: &mut HashSet<String>,
) {
    match &expr.kind {
        ExprKind::Par(par_body) => {
            detect_par_block_conflicts(
                par_body,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for stmt in &par_body.stmts {
                scan_stmt_for_par_conflicts(
                    stmt,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &par_body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Block(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            for stmt in &b.stmts {
                scan_stmt_for_par_conflicts(
                    stmt,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &b.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            scan_expr_for_par_conflicts(
                condition,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for s in &then_block.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &then_block.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(else_b) = else_branch {
                scan_expr_for_par_conflicts(
                    else_b,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            scan_expr_for_par_conflicts(
                condition,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::For { iterable, body, .. } => {
            scan_expr_for_par_conflicts(
                iterable,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for s in &body.stmts {
                scan_stmt_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = &body.final_expr {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_expr_for_par_conflicts(
                scrutinee,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    scan_expr_for_par_conflicts(
                        g,
                        tracked,
                        program_items,
                        closure_captures,
                        closure_bindings,
                        classifier,
                        errors,
                        fix_diffs,
                        promoted,
                    );
                }
                scan_expr_for_par_conflicts(
                    &arm.body,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            scan_expr_for_par_conflicts(
                callee,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for a in args {
                scan_expr_for_par_conflicts(
                    &a.value,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            for a in args {
                scan_expr_for_par_conflicts(
                    &a.value,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        ExprKind::Index { object, index } => {
            scan_expr_for_par_conflicts(
                object,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            scan_expr_for_par_conflicts(
                index,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        ExprKind::Binary { left, right, .. } => {
            scan_expr_for_par_conflicts(
                left,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
            scan_expr_for_par_conflicts(
                right,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        ExprKind::Unary { operand, .. } => {
            scan_expr_for_par_conflicts(
                operand,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        ExprKind::Tuple(items) => {
            for e in items {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        ExprKind::Cast { expr, .. } => {
            scan_expr_for_par_conflicts(
                expr,
                tracked,
                program_items,
                closure_captures,
                closure_bindings,
                classifier,
                errors,
                fix_diffs,
                promoted,
            );
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                scan_expr_for_par_conflicts(
                    s,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
            if let Some(e) = end.as_deref() {
                scan_expr_for_par_conflicts(
                    e,
                    tracked,
                    program_items,
                    closure_captures,
                    closure_bindings,
                    classifier,
                    errors,
                    fix_diffs,
                    promoted,
                );
            }
        }
        _ => {}
    }
}

/// Walk each top-level statement of `par_body` collecting the set of
/// tracked-binding names referenced anywhere in that branch's subtree.
/// Any binding appearing in two or more branches → emit the right
/// diagnostic flavor at the second-branch use site, with the first-
/// branch use threaded as the `consume_span` secondary.
#[allow(clippy::too_many_arguments)] // L205 threads classifier alongside existing detector args
fn detect_par_block_conflicts(
    par_body: &Block,
    tracked: &HashMap<String, TrackedBinding>,
    program_items: &[Item],
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    classifier: &MethodMutClassifier,
    errors: &mut Vec<OwnershipError>,
    fix_diffs: &mut HashMap<SpanKey, Vec<TextEdit>>,
    promoted: &mut HashSet<String>,
) {
    let mut first_use: HashMap<String, (usize, Span)> = HashMap::new();
    let mut reported: HashSet<String> = HashSet::new();

    // Pass 1 — per-branch uses. Collected up front because the
    // B-2026-08-01-33 suppression is a property of the WHOLE par block (does
    // any branch materialize this binding?), which the original single-pass
    // detector could not see: it fired on reaching the second branch, before
    // later branches had been walked.
    let per_branch: Vec<HashMap<String, BranchUse>> = par_body
        .stmts
        .iter()
        .map(|stmt| {
            let mut uses: HashMap<String, BranchUse> = HashMap::new();
            collect_identifier_uses_in_stmt(
                stmt,
                tracked,
                closure_captures,
                closure_bindings,
                &mut uses,
            );
            uses
        })
        .collect();

    // Pass 2 — B-2026-08-01-33. A binding every branch merely PROJECTS an
    // immutable scalar field from emits no refcount traffic anywhere in the
    // block, so there is nothing for the branches to race on and the conflict
    // is suppressed.
    //
    // The quantifier is "no branch materializes", not "at most one does". A
    // single materializing branch is enough to disqualify the whole block:
    // `ParCaptureMode::SharedRc` gives every capturing branch an ATOMIC
    // prologue inc, so one branch performing a plain rc op concurrently with
    // another's atomic one is mixed-atomicity access to a single word — a
    // race in its own right, whatever the counts work out to.
    let materialized_anywhere: HashSet<&str> = per_branch
        .iter()
        .flat_map(|uses| {
            uses.iter()
                .filter(|(_, u)| u.material)
                .map(|(n, _)| n.as_str())
        })
        .collect();

    for (branch_idx, uses) in per_branch.iter().enumerate() {
        for (name, use_entry) in uses {
            let (name, use_span) = (name.clone(), use_entry.span.clone());
            if reported.contains(&name) {
                continue;
            }
            if !materialized_anywhere.contains(name.as_str()) {
                continue;
            }
            match first_use.get(&name) {
                Some((prev_idx, prev_span)) if *prev_idx != branch_idx => {
                    let binding = &tracked[&name];
                    // B-2026-08-01-33 mechanism 2 — WHOLE-PROGRAM ATOMICITY
                    // PROMOTION. When every `shared` type reachable from this
                    // binding is free of `mut` fields, promoting all of them to
                    // atomic refcounting makes the multi-branch capture safe,
                    // and the conflict is suppressed rather than reported.
                    //
                    // Both halves are load-bearing and neither suffices alone.
                    // The promotion removes the RC-HEADER race — including on
                    // the interior handles a traversal materializes, which is
                    // why the closure covers the reachable set and not just the
                    // named type. The immutability removes the PAYLOAD race,
                    // which atomic refcounting does nothing about; that is the
                    // reason a type with a `mut` field is not promotable here
                    // even though its header could be made safe.
                    if let Some(types) = &binding.atomic_promotion {
                        if std::env::var("KARAC_PAR_ATOMIC_PROMOTION").as_deref() == Ok("1") {
                            promoted.extend(types.iter().cloned());
                            reported.insert(name);
                            continue;
                        }
                    }
                    let err = build_concurrent_struct_error(
                        &name,
                        binding,
                        use_span.clone(),
                        prev_span.clone(),
                    );
                    let mut edits =
                        build_fix_diff_edits(&binding.type_name, binding.kind, program_items);
                    let lock_edits = build_lock_block_edits_for_binding(
                        par_body,
                        &name,
                        &binding.type_name,
                        program_items,
                        classifier,
                    );
                    edits.extend(lock_edits);
                    if !edits.is_empty() {
                        fix_diffs.insert(SpanKey::from_span(&err.span), edits);
                    }
                    errors.push(err);
                    reported.insert(name);
                }
                None => {
                    first_use.insert(name, (branch_idx, use_span));
                }
                _ => {}
            }
        }
    }
}

fn build_concurrent_struct_error(
    binding: &str,
    tracked: &TrackedBinding,
    second_use_span: Span,
    first_use_span: Span,
) -> OwnershipError {
    let (kind_label, kind_variant) = match tracked.kind {
        BindingKind::Shared => (
            "shared struct",
            OwnershipErrorKind::ConcurrentSharedStruct {
                type_name: tracked.type_name.clone(),
                binding: binding.to_string(),
            },
        ),
        BindingKind::Plain => (
            "plain struct",
            OwnershipErrorKind::ConcurrentPlainStruct {
                type_name: tracked.type_name.clone(),
                binding: binding.to_string(),
            },
        ),
    };
    let message = format!(
        "{kind_label} `{ty}` cannot be accessed from multiple concurrent tasks (binding `{bn}` reachable from two par-block branches: first use at line {fl}:{fc}, conflicting use here)",
        ty = tracked.type_name,
        bn = binding,
        fl = first_use_span.line,
        fc = first_use_span.column,
    );
    // B-2026-08-01-33 — an IMMUTABLE type gets a one-keyword answer, not the
    // full migration. `par struct` with immutable fields shares lock-free
    // across tasks with no annotation at the use site, so telling such an
    // author to "wrap each mut field in Mutex[T]" is advice for a program they
    // did not write: there are no mut fields to wrap, and the migration is a
    // single keyword `karac fix` already applies.
    //
    // This distinction is why the entry that filed this diagnostic was itself
    // written believing the cheap answer did not exist, and had to be corrected
    // in place. The diagnostic taught that belief; conflating the two cases is
    // the defect.
    if tracked.kind == BindingKind::Shared && tracked.atomic_promotion.is_some() {
        let suggestion = format!(
            concat!(
                "`{ty}` has no `mut` fields, so it is already safe to read from several ",
                "tasks at once — rename `shared struct {ty}` to `par struct {ty}` and ",
                "nothing else changes. A `par` type's immutable fields are freely readable ",
                "across tasks: no `Mutex[T]`, no `Atomic[T]`, no annotation at the use ",
                "site. The only cost is that the type's reference counting becomes atomic. ",
                "`karac fix` applies this rename for you.",
            ),
            ty = tracked.type_name,
        );
        return OwnershipError {
            message,
            span: second_use_span,
            kind: kind_variant,
            suggestion: Some(suggestion),
            replacement: None,
            consume_span: Some(first_use_span),
        };
    }
    let suggestion = match tracked.kind {
        BindingKind::Shared => format!(
            "convert `{ty}` to `par struct` and wrap mut fields in `Mutex[T]`/`Atomic[T]`. The migration is structural:\n  1. rename `shared struct {ty}` to `par struct {ty}`\n  2. wrap each bare `mut` field in `Mutex[T]` (refine to `Atomic[T]` post-review where lock-free access is appropriate)\n  3. insert `lock field {{ ... }}` blocks at every write site within `par` regions\n  4. call sites that previously relied on implicit `Rc`-clone now produce `Arc`-clone semantics\nThe machine-applicable `fix_diff` covers steps 1 and 2 (keyword rewrite, `mut ` stripping, and per-field `Mutex[T]` wrap); steps 3 and 4 remain the human review step. Or run `karac migrate shared-to-par {ty}` for a preemptive workspace rewrite.",
            ty = tracked.type_name,
        ),
        BindingKind::Plain => format!(
            "convert `{ty}` to `par struct` and wrap mut fields in `Mutex[T]`/`Atomic[T]`. The migration is structural:\n  1. rename `struct {ty}` to `par struct {ty}`\n  2. wrap each bare `mut` field in `Mutex[T]` (refine to `Atomic[T]` post-review where lock-free access is appropriate)\n  3. insert `lock field {{ ... }}` blocks at every write site within `par` regions\n  4. consumers that took the value by ownership now share an `Arc` clone\nThe machine-applicable `fix_diff` covers steps 1 and 2 (keyword insert, `mut ` stripping, and per-field `Mutex[T]` wrap); steps 3 and 4 remain the human review step.",
            ty = tracked.type_name,
        ),
    };
    OwnershipError {
        message,
        span: second_use_span,
        kind: kind_variant,
        suggestion: Some(suggestion),
        replacement: None,
        consume_span: Some(first_use_span),
    }
}

/// Build the `fix_diff` edit list for a `ConcurrentSharedStruct` or
/// `ConcurrentPlainStruct` diagnostic. Three edit families are emitted
/// per affected `StructDef`:
///
/// 1. **Keyword rewrite** — `Shared` kind replaces the `shared` keyword
///    with `par` (replacement edit); `Plain` kind inserts `par ` before
///    the `struct` keyword (pure insertion). Driven by
///    `StructDef.kind_keyword_span` / `struct_keyword_span` (parser-
///    captured per L201a). When the kind/struct span is a synthetic
///    zero-width placeholder (prelude stubs), the keyword rewrite
///    silently skips — those defs never resolve to a real par-block
///    binding, so the path is unreachable in practice but the guard
///    keeps the function total.
/// 2. **`mut ` keyword strip** — for every `mut` field, delete the run
///    from `mut_keyword_span.offset` to `name_span.offset`. The
///    deletion length is derived from the two parser-captured spans
///    rather than a fixed `"mut ".len()` so intervening whitespace
///    (tabs, multiple spaces, embedded comments) gets removed too —
///    the source text doesn't need to be re-scanned.
/// 3. **`Mutex[T]` wrap** — pure-insertion `Mutex[` prefix before
///    `field.ty.span.offset` and `]` suffix after the type span, for
///    every `mut` field. Pre-existing edit family from the L197 fix-
///    diff slice.
///
/// Edits are emitted in source order; the consumer applies them back-
/// to-front (standard `karac fix` discipline) so offsets stay stable.
/// Returns an empty vec when no matching `StructDef` is found.
///
/// Exposed at crate visibility so `cli::cmd_migrate` (phase-7 L215a)
/// can reuse the same edit emitter for the preemptive `karac migrate
/// shared-to-par <Type>` workspace rewrite — same type-definition
/// rewrite shape, called directly off a parsed source rather than
/// indirectly via a fired diagnostic.
pub(crate) fn build_fix_diff_edits(
    type_name: &str,
    kind: BindingKind,
    program_items: &[Item],
) -> Vec<TextEdit> {
    build_fix_diff_edits_with_field_kinds(type_name, kind, program_items, &HashMap::new())
}

/// Per-field wrapper kind. The L215c Atomic heuristic classifies each
/// mut field as either `Mutex` (the L215a–b4 default) or `Atomic`
/// (workspace-write analysis confirms every observed write is a bare
/// `=` assignment AND the field type is one of the lock-free Atomic-
/// eligible Copy types). The classifier flows from the project-mode
/// walker (`cmd_migrate_project`) into both the type-def emitter
/// ([`build_fix_diff_edits_with_field_kinds`]) and the consumer-rewrite
/// path (via mut-fields subsetting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldWrapKind {
    Mutex,
    Atomic,
}

/// L215c variant of [`build_fix_diff_edits`] that consults a per-field
/// classifier to swap the wrap type from `Mutex[T]` to `Atomic[T]` for
/// Atomic-classified fields. Fields not present in `field_kinds`
/// default to `Mutex` (preserves L215a–b4 behavior).
pub(crate) fn build_fix_diff_edits_with_field_kinds(
    type_name: &str,
    kind: BindingKind,
    program_items: &[Item],
    field_kinds: &HashMap<String, FieldWrapKind>,
) -> Vec<TextEdit> {
    let Some(struct_def) = program_items.iter().find_map(|it| match it {
        Item::StructDef(s) if s.name == type_name => Some(s),
        _ => None,
    }) else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    // (1) Keyword rewrite — drives the `shared struct`/`struct` →
    // `par struct` half of the migration.
    match kind {
        BindingKind::Shared => {
            if let Some(kw) = &struct_def.kind_keyword_span {
                if kw.length > 0 {
                    edits.push(TextEdit {
                        offset: kw.offset,
                        length: kw.length,
                        replacement: "par".to_string(),
                    });
                }
            }
        }
        BindingKind::Plain => {
            let sk = &struct_def.struct_keyword_span;
            if sk.length > 0 {
                edits.push(TextEdit {
                    offset: sk.offset,
                    length: 0,
                    replacement: "par ".to_string(),
                });
            }
        }
    }
    // (2) `mut ` strip + (3) wrap type per mut field. The wrap
    // selector — `Mutex[` vs `Atomic[` — comes from `field_kinds`;
    // absent entries default to `Mutex` to preserve the L215a–b4 path.
    for field in &struct_def.fields {
        if !field.is_mut {
            continue;
        }
        if let Some(mut_kw) = &field.mut_keyword_span {
            let strip_end = field.name_span.offset;
            if strip_end > mut_kw.offset {
                edits.push(TextEdit {
                    offset: mut_kw.offset,
                    length: strip_end - mut_kw.offset,
                    replacement: String::new(),
                });
            }
        }
        let ty_off = field.ty.span.offset;
        let ty_len = field.ty.span.length;
        let wrap_prefix = match field_kinds.get(&field.name).copied() {
            Some(FieldWrapKind::Atomic) => "Atomic[",
            _ => "Mutex[",
        };
        edits.push(TextEdit {
            offset: ty_off,
            length: 0,
            replacement: wrap_prefix.to_string(),
        });
        edits.push(TextEdit {
            offset: ty_off + ty_len,
            length: 0,
            replacement: "]".to_string(),
        });
    }
    edits
}

/// Returns `true` when `ty` is a single-segment name in the lock-free
/// Atomic-compatible Copy-eligible set the L215c heuristic uses. The
/// set matches the platforms karac targets (x86_64 / aarch64): single-
/// word integers (`i32`, `i64`, `u32`, `u64`, `usize`, `isize`) and
/// `bool`. Floats are excluded — most CPUs lack native atomic float
/// instructions, so the runtime would emulate via CAS loops which the
/// reviewer is unlikely to want hidden behind an automatic migration.
pub(crate) fn is_atomic_eligible_type(ty: &TypeExpr) -> bool {
    let TypeKind::Path(p) = &ty.kind else {
        return false;
    };
    if p.segments.len() != 1 {
        return false;
    }
    if p.generic_args.as_ref().is_some_and(|args| !args.is_empty()) {
        return false;
    }
    matches!(
        p.segments[0].as_str(),
        "i32" | "i64" | "u32" | "u64" | "usize" | "isize" | "bool"
    )
}

/// Consumer-site rewrite edits for `karac migrate shared-to-par <Type>`.
/// Walks every top-level function body (and impl-block method body) in
/// `program_items`, discovers bindings of the migrating type — both
/// function parameters (`fn f(c: Counter)`, `fn f(c: ref Counter)`,
/// `fn f(c: mut ref Counter)`) and `let` declarations with explicit
/// type annotations (`let c: Counter = ...`) — and emits two flavors
/// of edits for each discovered binding:
///
/// 1. **Write-site wraps** — `lock self.<field> { ... }` around every
///    assign / compound-assign whose target's binding-root chain
///    matches `<binding>.<mut_field>...` (L207 walker reused), with
///    `<binding>.<field>` accesses inside the wrap body rewritten to
///    `self.<field>` per design.md line 8522. (L215b1 shipped the
///    write walker integration; L215b2 changed the wrap shape from
///    `lock <field>` shorthand to `lock self.<field>` + binding-root
///    rewrite.)
/// 2. **Read-site wraps** — every `<binding>.<mut_field>` access in
///    rvalue position (outside any matching write statement) is
///    replaced with `lock self.<field> { self.<field> }`. (L215b2.)
///
/// Edits whose offset falls inside any `par { ... }` body are dropped
/// post-collection — those sites are handled by `karac fix`'s
/// `E_CONCURRENT_SHARED_STRUCT` path, not by the preemptive migrate.
///
/// **Scope (v1):**
/// - Explicit type annotations only — params with annotated type
///   (including `ref` / `mut ref` borrow forms) and `let c: Foo = ...`
///   bindings. Type-inferred bindings (`let c = make_foo()`) require
///   typechecker integration and are deferred to L215b3.
/// - Single-file: the caller passes one file's `program_items`. Cross-
///   file project-mode walk lands in L215b4.
/// - Writes (assign / compound-assign) + reads (rvalue field access).
///   Mutating method-call wraps need typecheck-supplied receiver-mode
///   data (`MethodMutClassifier`), which migrate's parse-only pipeline
///   doesn't have; an empty classifier is supplied so method-call
///   sites silently no-op on the write side, and the read walker
///   explicitly skips statement-position method-call receivers rooted
///   at the binding so reviewers see the unwrapped call site directly.
/// - Shadowing: bindings are name-matched globally inside each function
///   body. If a function has two bindings with the same name in
///   disjoint scopes (one of the migrating type, one not), the
///   non-matching scope's edits would still be emitted. v1 ignores
///   this corner — the inner walker's name-only match is the limit.
/// - Self-prefix wrap shape compiles cleanly when the surrounding
///   context already has `self` in scope (impl methods); in free
///   functions taking the binding by `ref` / `mut ref`, the wrap
///   references `self` which isn't in scope and the reviewer hand-
///   completes the refactor per design.md's "always **manual at the
///   review step**" clause.
///
/// Returns an empty vec when the struct has no mut fields, when the
/// struct definition isn't found in `program_items`, or when no
/// matching binding declarations exist.
pub(crate) fn build_consumer_rewrite_edits_in_program(
    type_name: &str,
    program_items: &[Item],
    type_ctx: Option<ConsumerRewriteTypeCtx<'_>>,
) -> Vec<TextEdit> {
    let mut_fields = collect_mut_field_names(type_name, program_items);
    // Single-file/no-classifier callers get the L215b1-b4 default
    // (every field is Mutex-shaped); the Atomic dispatch (L215c-cons)
    // is project-mode-only and only fires when the caller threads a
    // populated atomic_fields set.
    let empty_atomic: HashSet<String> = HashSet::new();
    build_consumer_rewrite_edits_with_mut_fields(
        type_name,
        program_items,
        type_ctx,
        &mut_fields,
        &empty_atomic,
    )
}

/// Project-mode entry (L215b4). The cross-file walk computes mut-field
/// names once (in the file that defines `shared struct <Type>`) and
/// reuses them for every consumer file's rewrite. The default
/// `build_consumer_rewrite_edits_in_program` looks up mut-fields in the
/// local `program_items`, which silently no-ops on consumer-only files
/// — this entry takes the def-file's `mut_fields` directly so consumer
/// modules participate. Use [`collect_struct_mut_field_names`] to
/// extract the set from the def-file's parsed items.
pub(crate) fn build_consumer_rewrite_edits_with_mut_fields(
    type_name: &str,
    program_items: &[Item],
    type_ctx: Option<ConsumerRewriteTypeCtx<'_>>,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
) -> Vec<TextEdit> {
    if mut_fields.is_empty() {
        return Vec::new();
    }
    // When typecheck data is available, the classifier can fire mutating
    // method-call wraps (`c.field.push(x)` → `lock self.field { ... }`)
    // because `method_callee_types` resolves each call-site span to its
    // `Type.method` key, which `method_self_modes` then resolves to the
    // declared SelfParam (MutRef means the call mutates the receiver).
    // Without typecheck data, the parse-only fallback supplies an empty
    // callee-type map so the classifier silently no-ops on method calls
    // — matching L215b1/b2 semantics. `method_self_modes` is derivable
    // from the parsed program alone, so we compute it locally and reuse
    // it across both paths.
    let method_self_modes = collect_method_self_modes_in_items(program_items);
    let empty_callee_types: HashMap<SpanKey, String> = HashMap::new();
    let classifier = MethodMutClassifier {
        method_callee_types: type_ctx
            .as_ref()
            .map(|c| c.method_callee_types)
            .unwrap_or(&empty_callee_types),
        method_self_modes: &method_self_modes,
    };
    let par_spans = collect_par_body_spans_in_items(program_items);
    let mut edits = Vec::new();
    visit_each_function(program_items, &mut |params, body| {
        let mut bindings: Vec<String> = Vec::new();
        for p in params {
            if let Some(name) = p.name() {
                // Parse-only annotation check covers `ref Foo` / `mut ref Foo`
                // (which the typechecker's `bind_pattern_types` doesn't
                // record in `pattern_binding_types` because the outer
                // type is `Ref` / `MutRef`, not `Named` / `Shared`). The
                // typecheck-aware lookup below is an additive overlay —
                // both can fire for the same binding (dedup catches it).
                if type_expr_is_single_segment_named(&p.ty, type_name) {
                    bindings.push(name.to_string());
                }
                if let Some(ctx) = &type_ctx {
                    let key = SpanKey::from_span(&p.pattern.span);
                    if ctx.pattern_binding_types.get(&key).map(|s| s.as_str()) == Some(type_name) {
                        bindings.push(name.to_string());
                    }
                }
            }
        }
        collect_typed_bindings_in_block(body, type_name, type_ctx.as_ref(), &mut bindings);
        // Dedup: the annotation-check and typecheck-lookup paths both
        // fire on annotated bindings (`let c: Counter = ...`); the
        // inner walkers' name-only matching tolerates duplicates but
        // wastes work and inflates emitted edits before sort+dedup.
        bindings.sort();
        bindings.dedup();
        for binding_name in &bindings {
            collect_lock_block_writes_in_block(
                body,
                binding_name,
                mut_fields,
                atomic_fields,
                &classifier,
                WrapShape::SelfPrefix,
                &mut edits,
            );
            collect_lock_block_reads_in_block(
                body,
                binding_name,
                mut_fields,
                atomic_fields,
                &mut edits,
            );
        }
    });
    // Drop edits inside par bodies — those are the par-conflict
    // diagnostic's territory (`karac fix` emits the same wrap there).
    edits.retain(|e| {
        !par_spans
            .iter()
            .any(|(s, en)| *s <= e.offset && e.offset < *en)
    });
    edits
}

/// Public access to the mut-field collector for project-mode callers
/// (L215b4). The def-file's parse feeds this; the resulting set is
/// passed verbatim to [`build_consumer_rewrite_edits_with_mut_fields`]
/// for every other file in the workspace walk.
pub(crate) fn collect_struct_mut_field_names(
    type_name: &str,
    program_items: &[Item],
) -> HashSet<String> {
    collect_mut_field_names(type_name, program_items)
}

/// Typecheck-derived data threaded into
/// [`build_consumer_rewrite_edits_in_program`]. Caller passes `Some(ctx)`
/// when the full pipeline (parse + resolve + typecheck) ran successfully;
/// `None` degrades to parse-only behavior — annotation-typed bindings
/// only, no mutating-method-call wraps. The graceful degradation matches
/// design.md § Compiler-assisted migration's "always **manual at the
/// review step**" clause: a typecheck-failing source still produces a
/// starting-point diff; the human reviewer hand-completes.
///
/// Phase-7 line 221 (L215b3).
pub(crate) struct ConsumerRewriteTypeCtx<'a> {
    /// `TypeCheckResult.pattern_binding_types` — maps each pattern
    /// binding's `SpanKey` to its canonical type name. Used to discover
    /// bindings of the migrating type whose annotation is absent (the
    /// inferred-type case `let c = make_counter()` that the parse-only
    /// `type_expr_is_single_segment_named` path can't see). `ref` / `mut
    /// ref` annotated bindings stay on the parse-only path because the
    /// typechecker doesn't record them in this map (outer `Type::Ref` /
    /// `Type::MutRef` doesn't match the `Named` / `Shared` insertion arms
    /// of `bind_pattern_types`).
    pub pattern_binding_types: &'a HashMap<SpanKey, String>,
    /// `TypeCheckResult.method_callee_types` — maps each `MethodCall`
    /// span to its resolved `Type.method` key. Combined with the locally
    /// derived `method_self_modes`, this lifts the L215b1/b2 limitation
    /// where mutating method-call writes (`c.field.push(x)`) silently
    /// no-op'd under parse-only mode.
    pub method_callee_types: &'a HashMap<SpanKey, String>,
}

/// Per-`Type.method` `SelfParam` table derived from `&[Item]`. Mirrors
/// the body of [`crate::ownership::collect_method_self_modes`] but takes
/// items directly so the consumer-rewrite path doesn't need a `&Program`
/// reference (it has only the program-items slice). Kept private —
/// outside callers go through the `&Program` variant.
fn collect_method_self_modes_in_items(items: &[Item]) -> HashMap<String, SelfParam> {
    let mut map = HashMap::new();
    for item in items {
        match item {
            Item::ImplBlock(impl_block) => {
                let Some(target_name) = (if let TypeKind::Path(path) = &impl_block.target_type.kind
                {
                    path.segments.last().cloned()
                } else {
                    None
                }) else {
                    continue;
                };
                for impl_item in &impl_block.items {
                    if let ImplItem::Method(method) = impl_item {
                        if let Some(self_param) = &method.self_param {
                            map.insert(
                                format!("{target_name}.{}", method.name),
                                self_param.clone(),
                            );
                        }
                    }
                }
            }
            Item::TraitDef(trait_def) => {
                for trait_item in &trait_def.items {
                    if let TraitItem::Method(tm) = trait_item {
                        if let Some(self_param) = &tm.self_param {
                            map.insert(
                                format!("{}.{}", trait_def.name, tm.name),
                                self_param.clone(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
    map
}

fn visit_each_function(items: &[Item], visitor: &mut impl FnMut(&[Param], &Block)) {
    for it in items {
        match it {
            Item::Function(f) => visitor(&f.params, &f.body),
            Item::ImplBlock(ib) => {
                for ii in &ib.items {
                    if let ImplItem::Method(m) = ii {
                        visitor(&m.params, &m.body);
                    }
                }
            }
            _ => {}
        }
    }
}

fn visit_fn_bodies(items: &[Item], visitor: &mut impl FnMut(&Block)) {
    visit_each_function(items, &mut |_params, body| visitor(body));
}

fn collect_typed_bindings_in_block(
    block: &Block,
    type_name: &str,
    type_ctx: Option<&ConsumerRewriteTypeCtx<'_>>,
    out: &mut Vec<String>,
) {
    for stmt in &block.stmts {
        collect_typed_bindings_in_stmt(stmt, type_name, type_ctx, out);
    }
    if let Some(e) = &block.final_expr {
        collect_typed_bindings_in_expr(e, type_name, type_ctx, out);
    }
}

/// Whether `pattern` was typed as `type_name` by the typechecker. Returns
/// `false` when `type_ctx` is `None` (parse-only mode) or when the
/// pattern isn't in the table — covers `ref` / `mut ref` annotations
/// (no entry recorded) and unresolved/error types (no entry recorded).
fn pattern_binding_matches_type(
    pattern: &Pattern,
    type_name: &str,
    type_ctx: Option<&ConsumerRewriteTypeCtx<'_>>,
) -> bool {
    let Some(ctx) = type_ctx else {
        return false;
    };
    ctx.pattern_binding_types
        .get(&SpanKey::from_span(&pattern.span))
        .map(|s| s.as_str())
        == Some(type_name)
}

fn collect_typed_bindings_in_stmt(
    stmt: &Stmt,
    type_name: &str,
    type_ctx: Option<&ConsumerRewriteTypeCtx<'_>>,
    out: &mut Vec<String>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let {
            pattern, ty, value, ..
        } => {
            if let PatternKind::Binding(name) = &pattern.kind {
                let by_annotation = ty
                    .as_ref()
                    .map(|t| type_expr_is_single_segment_named(t, type_name))
                    .unwrap_or(false);
                let by_typecheck = pattern_binding_matches_type(pattern, type_name, type_ctx);
                if by_annotation || by_typecheck {
                    out.push(name.clone());
                }
            }
            collect_typed_bindings_in_expr(value, type_name, type_ctx, out);
        }
        StmtKind::LetUninit { name, ty, .. } => {
            // `LetUninit` records its type via `expr_types[name_span]`,
            // not `pattern_binding_types` — but it always has an explicit
            // type annotation (no inference possible without an RHS), so
            // the parse-only check is sufficient here.
            if type_expr_is_single_segment_named(ty, type_name) {
                out.push(name.clone());
            }
        }
        StmtKind::LetElse {
            pattern,
            ty,
            value,
            else_block,
        } => {
            if let PatternKind::Binding(name) = &pattern.kind {
                let by_annotation = ty
                    .as_ref()
                    .map(|t| type_expr_is_single_segment_named(t, type_name))
                    .unwrap_or(false);
                let by_typecheck = pattern_binding_matches_type(pattern, type_name, type_ctx);
                if by_annotation || by_typecheck {
                    out.push(name.clone());
                }
            }
            collect_typed_bindings_in_expr(value, type_name, type_ctx, out);
            collect_typed_bindings_in_block(else_block, type_name, type_ctx, out);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            collect_typed_bindings_in_expr(target, type_name, type_ctx, out);
            collect_typed_bindings_in_expr(value, type_name, type_ctx, out);
        }
        StmtKind::Expr(e) => collect_typed_bindings_in_expr(e, type_name, type_ctx, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_typed_bindings_in_block(body, type_name, type_ctx, out);
        }
    }
}

fn collect_typed_bindings_in_expr(
    expr: &Expr,
    type_name: &str,
    type_ctx: Option<&ConsumerRewriteTypeCtx<'_>>,
    out: &mut Vec<String>,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            collect_typed_bindings_in_block(b, type_name, type_ctx, out)
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_typed_bindings_in_expr(condition, type_name, type_ctx, out);
            collect_typed_bindings_in_block(then_block, type_name, type_ctx, out);
            if let Some(eb) = else_branch {
                collect_typed_bindings_in_expr(eb, type_name, type_ctx, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_typed_bindings_in_expr(condition, type_name, type_ctx, out);
            collect_typed_bindings_in_block(body, type_name, type_ctx, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_typed_bindings_in_expr(iterable, type_name, type_ctx, out);
            collect_typed_bindings_in_block(body, type_name, type_ctx, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_typed_bindings_in_expr(scrutinee, type_name, type_ctx, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_typed_bindings_in_expr(g, type_name, type_ctx, out);
                }
                collect_typed_bindings_in_expr(&arm.body, type_name, type_ctx, out);
            }
        }
        ExprKind::Call { callee, args } => {
            collect_typed_bindings_in_expr(callee, type_name, type_ctx, out);
            for a in args {
                collect_typed_bindings_in_expr(&a.value, type_name, type_ctx, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_typed_bindings_in_expr(object, type_name, type_ctx, out);
            for a in args {
                collect_typed_bindings_in_expr(&a.value, type_name, type_ctx, out);
            }
        }
        _ => {}
    }
}

/// True iff `ty` is an unqualified single-segment named type whose
/// name equals `type_name`. Strips `ref` / `mut ref` borrow modifiers
/// first so `Counter`, `ref Counter`, and `mut ref Counter` all match
/// — call-site code that mutates `c.field` through any of those forms
/// is wrapped uniformly by the migrate path. Does not strip
/// `mut Slice[T]` (that's a slice value, not a `T` binding).
fn type_expr_is_single_segment_named(ty: &TypeExpr, type_name: &str) -> bool {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.len() == 1 && p.segments[0] == type_name,
        TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
            type_expr_is_single_segment_named(inner, type_name)
        }
        _ => false,
    }
}

fn collect_par_body_spans_in_items(items: &[Item]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    visit_fn_bodies(items, &mut |body| {
        collect_par_body_spans_in_block(body, &mut out);
    });
    out
}

fn collect_par_body_spans_in_block(block: &Block, out: &mut Vec<(usize, usize)>) {
    for stmt in &block.stmts {
        collect_par_body_spans_in_stmt(stmt, out);
    }
    if let Some(e) = &block.final_expr {
        collect_par_body_spans_in_expr(e, out);
    }
}

fn collect_par_body_spans_in_stmt(stmt: &Stmt, out: &mut Vec<(usize, usize)>) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
            collect_par_body_spans_in_expr(value, out);
        }
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            collect_par_body_spans_in_expr(target, out);
            collect_par_body_spans_in_expr(value, out);
        }
        StmtKind::Expr(e) => collect_par_body_spans_in_expr(e, out),
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_par_body_spans_in_block(body, out);
        }
        StmtKind::LetUninit { .. } => {}
    }
}

fn collect_par_body_spans_in_expr(expr: &Expr, out: &mut Vec<(usize, usize)>) {
    match &expr.kind {
        ExprKind::Par(b) => {
            out.push((b.span.offset, b.span.offset + b.span.length));
            // No recurse: nested par-in-par would be redundant since
            // the outer span already covers everything.
        }
        ExprKind::Block(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => collect_par_body_spans_in_block(b, out),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_par_body_spans_in_expr(condition, out);
            collect_par_body_spans_in_block(then_block, out);
            if let Some(eb) = else_branch {
                collect_par_body_spans_in_expr(eb, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_par_body_spans_in_expr(condition, out);
            collect_par_body_spans_in_block(body, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_par_body_spans_in_expr(iterable, out);
            collect_par_body_spans_in_block(body, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_par_body_spans_in_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_par_body_spans_in_expr(g, out);
                }
                collect_par_body_spans_in_expr(&arm.body, out);
            }
        }
        ExprKind::Call { callee, args } => {
            collect_par_body_spans_in_expr(callee, out);
            for a in args {
                collect_par_body_spans_in_expr(&a.value, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_par_body_spans_in_expr(object, out);
            for a in args {
                collect_par_body_spans_in_expr(&a.value, out);
            }
        }
        _ => {}
    }
}

/// Lock-block wrap edits for writes to `binding_name.<mut_field>`
/// occurring textually inside `par_body`. Phase-7 L201b shipped the
/// `Assign` / `CompoundAssign` cases; **L205** extends the walker to
/// also wrap mutating *method-call* writes (`c.field.push(x)`,
/// `c.field.clear()`, etc.) so the migration spec's step 3 (design.md
/// § Compiler-assisted migration from `shared struct` to `par struct`)
/// covers the canonical Vec/Map/Set/String mutation idioms.
///
/// **Detection scope** (v1 + L205):
/// - Receiver shape: `Identifier(binding_name).<field>` — simple
///   binding-rooted field access. Chained projections
///   (`c.nested.field`), index accesses (`arr[0].field`), and
///   receivers on temporary expressions fall outside v1 and remain
///   the human review step.
/// - Write shape: `StmtKind::Assign`, `StmtKind::CompoundAssign`, and
///   *statement-position* mutating `MethodCall`s on the same receiver
///   shape. A method is "mutating" when its receiver-mode is
///   `mut ref self` — `SelfParam::MutRef` for user impls (from
///   `method_self_modes`), or `BorrowKind::MutRef` from the stdlib
///   `Vec.push` / `Map.insert` / `String.push_str` / etc. table
///   (`stdlib_method_self_borrow_kind`). The classifier handles both
///   sources transparently. Method calls in non-statement position
///   (e.g. an RHS expression contributing a value) are NOT wrapped —
///   wrapping inside a value expression would require splitting the
///   enclosing statement; the surrounding stmt is handled instead.
/// - Containment: any depth inside `par_body` (nested `if` / `while`
///   / `for` / `match` / `block` blocks are traversed).
/// - Field filter: only `mut` fields of the struct definition.
///
/// **Edit shape**: two pure-insertion edits per write site —
/// `lock <field> {\n    ` before the statement's start and `\n}` after
/// the statement's end. For method calls, the end-anchor is the
/// closing `)` derived from `MethodCall.args_close_span` (parser-
/// captured; the outer `Expr.span` covers only the receiver). For
/// assigns it's `value.span.offset + value.span.length`. Both shapes
/// leave the trailing `;` outside the wrap, becoming the lock
/// statement's own terminator.
///
/// Returns an empty vec when the struct has no mut fields, when no
/// matching writes exist, or when the struct definition isn't found
/// in `program_items`.
fn build_lock_block_edits_for_binding(
    par_body: &Block,
    binding_name: &str,
    type_name: &str,
    program_items: &[Item],
    classifier: &MethodMutClassifier,
) -> Vec<TextEdit> {
    let mut_fields = collect_mut_field_names(type_name, program_items);
    if mut_fields.is_empty() {
        return Vec::new();
    }
    let mut edits = Vec::new();
    // `karac fix` always emits the field-shorthand wrap because the
    // diagnostic fires inside an impl method body's par block where
    // `self` is already in scope. The L215b2 SelfPrefix shape is
    // reserved for the migrate path. The Atomic dispatch (L215c-cons)
    // is migrate-only — `karac fix` has no Atomic classifier feeding
    // it, so an empty atomic_fields set is passed and every wrap stays
    // Mutex-shaped.
    let empty_atomic: HashSet<String> = HashSet::new();
    collect_lock_block_writes_in_block(
        par_body,
        binding_name,
        &mut_fields,
        &empty_atomic,
        classifier,
        WrapShape::Shorthand,
        &mut edits,
    );
    edits
}

fn collect_mut_field_names(type_name: &str, program_items: &[Item]) -> HashSet<String> {
    let Some(struct_def) = program_items.iter().find_map(|it| match it {
        Item::StructDef(s) if s.name == type_name => Some(s),
        _ => None,
    }) else {
        return HashSet::new();
    };
    struct_def
        .fields
        .iter()
        .filter(|f| f.is_mut)
        .map(|f| f.name.clone())
        .collect()
}

#[allow(clippy::too_many_arguments)] // L215c-cons threads atomic_fields alongside the existing walker args
fn collect_lock_block_writes_in_block(
    block: &Block,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    shape: WrapShape,
    out: &mut Vec<TextEdit>,
) {
    for stmt in &block.stmts {
        collect_lock_block_writes_in_stmt(
            stmt,
            binding_name,
            mut_fields,
            atomic_fields,
            classifier,
            shape,
            out,
        );
    }
    if let Some(e) = &block.final_expr {
        collect_lock_block_writes_in_expr(
            e,
            binding_name,
            mut_fields,
            atomic_fields,
            classifier,
            shape,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)] // L215c-cons threads atomic_fields alongside the existing walker args
fn collect_lock_block_writes_in_stmt(
    stmt: &Stmt,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    shape: WrapShape,
    out: &mut Vec<TextEdit>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Assign { target, value } => {
            if let Some(field) = matched_self_field_access(target, binding_name, mut_fields) {
                if atomic_fields.contains(field) {
                    // L215c-cons — bare `<binding>.<field> = <value>`
                    // on an Atomic[T]-classified field. Rewrite to
                    // `<binding>.<field>.store(<value>, MemoryOrdering.Release)`.
                    // Don't emit the binding-root SelfPrefix rewrite —
                    // the source binding name stays in place (no
                    // enclosing lock body to retarget at `self`), and
                    // the load/store-shape rewrite leaves the
                    // `<binding>.<field>` prefix intact. Restrict to
                    // the single-step case `Identifier(<binding>).<field>` —
                    // multi-step chains (`c.field.subfield = ...`) can't
                    // legally apply to an Atomic-classified field since
                    // `Atomic[T]` has no user fields, and the parser
                    // gives `target.span` as only the *object* span
                    // (object alone, not the full FieldAccess — see
                    // `src/parser/exprs.rs:149`), so the field text
                    // length has to be reconstructed from the field
                    // name. The same whitespace-around-dot caveat from
                    // the read walker applies here.
                    if let ExprKind::FieldAccess {
                        object,
                        field: outer_field,
                    } = &target.kind
                    {
                        if matches!(&object.kind, ExprKind::Identifier(n) if n == binding_name) {
                            let target_full_end =
                                object.span.offset + object.span.length + 1 + outer_field.len();
                            let value_start = value.span.offset;
                            let value_end = value.span.offset + value.span.length;
                            emit_atomic_store_around(target_full_end, value_start, value_end, out);
                        }
                    }
                } else {
                    // The parser sets `stmt.span` to the target's span
                    // only (see src/parser/stmts.rs's Assign arm), so
                    // `stmt.span.offset + stmt.span.length` points one
                    // past the target's first token, not past the
                    // value. Anchor the wrap from the target's start
                    // (== stmt span start) to the value's end so the
                    // wrapped statement covers `target = value` in
                    // full; the trailing `;` falls outside the wrap,
                    // becoming the lock-statement's own terminator.
                    let wrap_start = target.span.offset;
                    let wrap_end = value.span.offset + value.span.length;
                    // L215b2 SelfPrefix mode — emit binding-root rewrite
                    // edits BEFORE the wrap-prefix insertion so the
                    // stable sort by `Reverse(offset)` preserves their
                    // relative order at the same offset.
                    if shape == WrapShape::SelfPrefix {
                        collect_binding_root_rewrites_in_expr(target, binding_name, out);
                        collect_binding_root_rewrites_in_expr(value, binding_name, out);
                    }
                    emit_lock_wrap_around(wrap_start, wrap_end, field, shape, out);
                }
            }
            // Recurse into target / value to catch writes nested inside
            // RHS expressions (e.g. a block-expr value containing
            // another assign — rare but possible).
            collect_lock_block_writes_in_expr(
                target,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_expr(
                value,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            if let Some(field) = matched_self_field_access(target, binding_name, mut_fields) {
                // L215c-cons defensive guard: CompoundAssign on an
                // Atomic-classified field should never reach here —
                // `collect_atomic_disqualifying_writes_in_*` removes
                // those fields from the Atomic set before this walker
                // runs. If somehow we do, emit no edit so the
                // reviewer notices (rather than mis-rewriting to
                // `.store(+= value)` or a lock-wrap that semantically
                // contradicts the field's Atomic classification).
                if !atomic_fields.contains(field) {
                    let wrap_start = target.span.offset;
                    let wrap_end = value.span.offset + value.span.length;
                    if shape == WrapShape::SelfPrefix {
                        collect_binding_root_rewrites_in_expr(target, binding_name, out);
                        collect_binding_root_rewrites_in_expr(value, binding_name, out);
                    }
                    emit_lock_wrap_around(wrap_start, wrap_end, field, shape, out);
                }
            }
            collect_lock_block_writes_in_expr(
                target,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_expr(
                value,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        StmtKind::Expr(e) => {
            // L205 — mutating method call in statement position.
            // `c.field.push(x);` parses as `StmtKind::Expr(MethodCall {
            // object: FieldAccess { Identifier(c), field }, method,
            // args, args_close_span })`. Wrap iff (a) the receiver
            // matches `Identifier(binding_name).<mut_field>`, and (b)
            // the classifier says the method takes a `mut ref self`
            // receiver. Atomic-classified fields are skipped — the
            // classifier disqualifies fields with any mutating-method
            // call before this walker runs, so an Atomic field hitting
            // this branch indicates a classifier/walker mismatch; the
            // defensive guard emits no wrap so the reviewer notices.
            if let ExprKind::MethodCall {
                object,
                args_close_span,
                ..
            } = &e.kind
            {
                if classifier.is_mutating(&e.span) {
                    if let Some(field) = matched_self_field_access(object, binding_name, mut_fields)
                    {
                        if !atomic_fields.contains(field) {
                            let wrap_start = e.span.offset;
                            let wrap_end = args_close_span.offset + args_close_span.length;
                            if shape == WrapShape::SelfPrefix {
                                collect_binding_root_rewrites_in_expr(e, binding_name, out);
                            }
                            emit_lock_wrap_around(wrap_start, wrap_end, field, shape, out);
                        }
                    }
                }
            }
            collect_lock_block_writes_in_expr(
                e,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        StmtKind::Let { value, .. } => {
            collect_lock_block_writes_in_expr(
                value,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_lock_block_writes_in_expr(
                value,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_block(
                else_block,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_lock_block_writes_in_block(
                body,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)] // L215c-cons threads atomic_fields alongside the existing walker args
fn collect_lock_block_writes_in_expr(
    expr: &Expr,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    shape: WrapShape,
    out: &mut Vec<TextEdit>,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            collect_lock_block_writes_in_block(
                b,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_lock_block_writes_in_expr(
                condition,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_block(
                then_block,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            if let Some(eb) = else_branch {
                collect_lock_block_writes_in_expr(
                    eb,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    classifier,
                    shape,
                    out,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_lock_block_writes_in_expr(
                condition,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_block(
                body,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        ExprKind::For { iterable, body, .. } => {
            collect_lock_block_writes_in_expr(
                iterable,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            collect_lock_block_writes_in_block(
                body,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_lock_block_writes_in_expr(
                scrutinee,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_lock_block_writes_in_expr(
                        g,
                        binding_name,
                        mut_fields,
                        atomic_fields,
                        classifier,
                        shape,
                        out,
                    );
                }
                collect_lock_block_writes_in_expr(
                    &arm.body,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    classifier,
                    shape,
                    out,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            collect_lock_block_writes_in_expr(
                callee,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            for a in args {
                collect_lock_block_writes_in_expr(
                    &a.value,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    classifier,
                    shape,
                    out,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_lock_block_writes_in_expr(
                object,
                binding_name,
                mut_fields,
                atomic_fields,
                classifier,
                shape,
                out,
            );
            for a in args {
                collect_lock_block_writes_in_expr(
                    &a.value,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    classifier,
                    shape,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Return the field name iff `target` is `Identifier(binding_name).<field>`
/// where `field` is in `mut_fields`. Filters chained projections,
/// index access, deref, and other complex receiver shapes that v1
/// leaves to the human review step.
/// Resolve `target` to its rooted-field if it's a place chain rooted
/// at `binding_name` whose **first projection on the binding** is a
/// `Field` in `mut_fields`. L201b shipped the v1 shape (single-step
/// `Identifier(binding_name).<field>`); L207 generalizes to multi-step
/// projection chains where the first step off the binding is still a
/// field access.
///
/// **Accepts** (returns `Some(first_field_on_binding)`):
/// - `c.field` — v1 baseline (single field).
/// - `c.field.subfield` — deeper field chain through `c.field`.
/// - `c.field.subfield.subsubfield…` — any depth of field projections.
/// - `c.field[0]` — field followed by index/range projection.
/// - `c.field.0` — field followed by tuple index projection.
/// - All of the above as receivers of mutating method calls
///   (`c.field.subfield.push(x)`, `c.field[0].clear()`, etc. — the
///   L205 caller passes `MethodCall.object` here).
///
/// **Rejects** (returns `None`):
/// - Different root binding (`other.field`).
/// - First projection on the binding isn't a `Field` — `c[0].field`
///   (index-first), `c[0..3]` (range), `c.0` (tuple). Index-rooted
///   writes need element-level locking semantics that don't map to
///   `lock <field> { ... }`; out of L207 scope.
/// - First field projection isn't a `mut` field of the diagnosed
///   struct (e.g., write through an immutable field — would be a
///   typecheck error anyway, but the gate keeps the function total).
/// - Root isn't a bare identifier — temporaries (`f().field`),
///   method-call results (`c.foo().field`), and other non-place
///   receivers fall through here.
///
/// The returned `&String` is borrowed from `mut_fields`; the caller
/// uses it immediately to format the lock prefix, so the lifetime
/// works out.
fn matched_self_field_access<'a>(
    target: &Expr,
    binding_name: &str,
    mut_fields: &'a HashSet<String>,
) -> Option<&'a String> {
    let place = resolve_place_chain(target)?;
    if place.root != binding_name {
        return None;
    }
    let first_field = match place.projections.first()? {
        Projection::Field(f) => f,
        Projection::Index | Projection::Range => return None,
    };
    mut_fields.get(first_field)
}

/// L215c walker that tags fields of `<Type>` bindings as "disqualified
/// from Atomic[T]" when it sees a non-bare write — compound assign
/// (`c.field += v`) or mutating method call (`c.field.push(v)`). Bare
/// `=` assigns do NOT disqualify (they map cleanly to `Atomic.store`).
fn collect_atomic_disqualifying_writes_in_block(
    block: &Block,
    binding_name: &str,
    candidate_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    disqualified: &mut HashSet<String>,
) {
    for stmt in &block.stmts {
        collect_atomic_disqualifying_writes_in_stmt(
            stmt,
            binding_name,
            candidate_fields,
            classifier,
            disqualified,
        );
    }
    if let Some(e) = &block.final_expr {
        collect_atomic_disqualifying_writes_in_expr(
            e,
            binding_name,
            candidate_fields,
            classifier,
            disqualified,
        );
    }
}

fn collect_atomic_disqualifying_writes_in_stmt(
    stmt: &Stmt,
    binding_name: &str,
    candidate_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    disqualified: &mut HashSet<String>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::CompoundAssign { target, value, .. } => {
            if let Some(field) = matched_self_field_access(target, binding_name, candidate_fields) {
                disqualified.insert(field.clone());
            }
            collect_atomic_disqualifying_writes_in_expr(
                target,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_expr(
                value,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        StmtKind::Assign { target, value } => {
            // Bare `=` does not disqualify — it maps to `Atomic.store`.
            // Still recurse into the RHS for nested writes.
            collect_atomic_disqualifying_writes_in_expr(
                target,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_expr(
                value,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        StmtKind::Expr(e) => {
            if let ExprKind::MethodCall { object, .. } = &e.kind {
                if classifier.is_mutating(&e.span) {
                    if let Some(field) =
                        matched_self_field_access(object, binding_name, candidate_fields)
                    {
                        disqualified.insert(field.clone());
                    }
                }
            }
            collect_atomic_disqualifying_writes_in_expr(
                e,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        StmtKind::Let { value, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                value,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_atomic_disqualifying_writes_in_expr(
                value,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_block(
                else_block,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_atomic_disqualifying_writes_in_block(
                body,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
    }
}

fn collect_atomic_disqualifying_writes_in_expr(
    expr: &Expr,
    binding_name: &str,
    candidate_fields: &HashSet<String>,
    classifier: &MethodMutClassifier,
    disqualified: &mut HashSet<String>,
) {
    // Mutating method call in rvalue position: `let r = c.field.push(x);`
    // — still disqualifies even though the method-call's return value
    // is consumed by the let.
    if let ExprKind::MethodCall { object, .. } = &expr.kind {
        if classifier.is_mutating(&expr.span) {
            if let Some(field) = matched_self_field_access(object, binding_name, candidate_fields) {
                disqualified.insert(field.clone());
            }
        }
    }
    // Recurse into sub-expressions. The walker is shape-agnostic — it
    // descends through every expression form that can house a nested
    // write or method call.
    match &expr.kind {
        ExprKind::FieldAccess { object, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                object,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::Index { object, index } => {
            collect_atomic_disqualifying_writes_in_expr(
                object,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_expr(
                index,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                object,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            for a in args {
                collect_atomic_disqualifying_writes_in_expr(
                    &a.value,
                    binding_name,
                    candidate_fields,
                    classifier,
                    disqualified,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            collect_atomic_disqualifying_writes_in_expr(
                callee,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            for a in args {
                collect_atomic_disqualifying_writes_in_expr(
                    &a.value,
                    binding_name,
                    candidate_fields,
                    classifier,
                    disqualified,
                );
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                left,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_expr(
                right,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::Unary { operand, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                operand,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::Block(b) => {
            collect_atomic_disqualifying_writes_in_block(
                b,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_atomic_disqualifying_writes_in_expr(
                condition,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_block(
                then_block,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            if let Some(eb) = else_branch {
                collect_atomic_disqualifying_writes_in_expr(
                    eb,
                    binding_name,
                    candidate_fields,
                    classifier,
                    disqualified,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_atomic_disqualifying_writes_in_expr(
                condition,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_block(
                body,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::For { iterable, body, .. } => {
            collect_atomic_disqualifying_writes_in_expr(
                iterable,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
            collect_atomic_disqualifying_writes_in_block(
                body,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        ExprKind::Loop { body, .. } => {
            collect_atomic_disqualifying_writes_in_block(
                body,
                binding_name,
                candidate_fields,
                classifier,
                disqualified,
            );
        }
        _ => {
            // Leaf forms (literals, identifiers, path nodes, etc.) and
            // forms that don't carry binding-field write sites in v1 —
            // skip. New expression shapes that could carry writes
            // (e.g. async block, try-expr) extend this match the same
            // way `collect_lock_block_writes_in_expr` does.
        }
    }
}

/// Project-mode L215c entry: given the def-file's struct definition,
/// the mut-field set, and every walked file's parsed items, return a
/// per-field classification (`Atomic` vs `Mutex`). A field is Atomic-
/// eligible iff (a) its declared type is in the lock-free Atomic Copy
/// set ([`is_atomic_eligible_type`]) AND (b) every observed write
/// across the workspace is a bare `=` assignment — no compound assign,
/// no mutating method call. Fields outside the Atomic-eligible type
/// set always classify as `Mutex`; fields with any disqualifying
/// write also classify as `Mutex`.
pub(crate) fn classify_field_wrap_kinds(
    type_name: &str,
    mut_fields: &HashSet<String>,
    def_program_items: &[Item],
    consumer_files: &[ProjectMigrationFile<'_>],
) -> HashMap<String, FieldWrapKind> {
    let mut field_kinds: HashMap<String, FieldWrapKind> = HashMap::new();
    let Some(struct_def) = def_program_items.iter().find_map(|it| match it {
        Item::StructDef(s) if s.name == type_name => Some(s),
        _ => None,
    }) else {
        return field_kinds;
    };
    // (1) Seed each mut field with its by-type eligibility.
    let mut atomic_candidates: HashSet<String> = HashSet::new();
    for field in &struct_def.fields {
        if !field.is_mut {
            continue;
        }
        if !mut_fields.contains(&field.name) {
            continue;
        }
        if is_atomic_eligible_type(&field.ty) {
            atomic_candidates.insert(field.name.clone());
        } else {
            field_kinds.insert(field.name.clone(), FieldWrapKind::Mutex);
        }
    }
    if atomic_candidates.is_empty() {
        return field_kinds;
    }
    // (2) Walk every consumer file. For each binding of `<Type>`,
    // tag any field that has a disqualifying write. Disqualified
    // fields fall back to `Mutex` in the final classification.
    let mut disqualified: HashSet<String> = HashSet::new();
    for file in consumer_files {
        let method_self_modes = collect_method_self_modes_in_items(file.program_items);
        let empty_callee_types: HashMap<SpanKey, String> = HashMap::new();
        let classifier = MethodMutClassifier {
            method_callee_types: file
                .type_ctx
                .as_ref()
                .map(|c| c.method_callee_types)
                .unwrap_or(&empty_callee_types),
            method_self_modes: &method_self_modes,
        };
        visit_each_function(file.program_items, &mut |params, body| {
            let mut bindings: Vec<String> = Vec::new();
            for p in params {
                if let Some(name) = p.name() {
                    if type_expr_is_single_segment_named(&p.ty, type_name) {
                        bindings.push(name.to_string());
                    }
                    if let Some(ctx) = &file.type_ctx {
                        let key = SpanKey::from_span(&p.pattern.span);
                        if ctx.pattern_binding_types.get(&key).map(|s| s.as_str())
                            == Some(type_name)
                        {
                            bindings.push(name.to_string());
                        }
                    }
                }
            }
            collect_typed_bindings_in_block(body, type_name, file.type_ctx.as_ref(), &mut bindings);
            bindings.sort();
            bindings.dedup();
            for binding_name in &bindings {
                collect_atomic_disqualifying_writes_in_block(
                    body,
                    binding_name,
                    &atomic_candidates,
                    &classifier,
                    &mut disqualified,
                );
            }
        });
    }
    // (3) Atomic-eligible fields not in `disqualified` → Atomic;
    // disqualified or non-eligible → Mutex.
    for field in &atomic_candidates {
        let kind = if disqualified.contains(field) {
            FieldWrapKind::Mutex
        } else {
            FieldWrapKind::Atomic
        };
        field_kinds.insert(field.clone(), kind);
    }
    field_kinds
}

/// Project-mode L215c bundle threaded into [`classify_field_wrap_kinds`].
/// Each entry is one walked file's parsed items + an optional typecheck
/// snapshot for the inferred-binding / mutating-method-call paths.
pub(crate) struct ProjectMigrationFile<'a> {
    pub program_items: &'a [Item],
    pub type_ctx: Option<ConsumerRewriteTypeCtx<'a>>,
}

/// Mirror of `OwnershipChecker::place_expr_root` for the
/// concurrent-shared detector — same projection shapes, but free-
/// standing (doesn't need `slice_binding_sources` resolution). Walks
/// `expr` down through `FieldAccess` / `Index` / `TupleIndex` to find
/// the root identifier and the projection chain root-to-leaf. Returns
/// `None` for expressions that aren't place chains rooted at an
/// identifier (literals, calls, struct literals, etc.).
fn resolve_place_chain(expr: &Expr) -> Option<PlaceExpr> {
    match &expr.kind {
        ExprKind::Identifier(name) => Some(PlaceExpr {
            root: name.clone(),
            projections: Vec::new(),
        }),
        ExprKind::FieldAccess { object, field } => {
            let mut p = resolve_place_chain(object)?;
            p.projections.push(Projection::Field(field.clone()));
            Some(p)
        }
        ExprKind::Index { object, index } => {
            let mut p = resolve_place_chain(object)?;
            let proj = if matches!(&index.kind, ExprKind::Range { .. }) {
                Projection::Range
            } else {
                Projection::Index
            };
            p.projections.push(proj);
            Some(p)
        }
        ExprKind::TupleIndex { object, .. } => {
            let mut p = resolve_place_chain(object)?;
            p.projections.push(Projection::Index);
            Some(p)
        }
        _ => None,
    }
}

fn emit_lock_wrap_around(
    start: usize,
    end: usize,
    field: &str,
    shape: WrapShape,
    out: &mut Vec<TextEdit>,
) {
    let prefix = match shape {
        WrapShape::Shorthand => format!("lock {field} {{\n    "),
        WrapShape::SelfPrefix => format!("lock self.{field} {{\n    "),
    };
    out.push(TextEdit {
        offset: start,
        length: 0,
        replacement: prefix,
    });
    out.push(TextEdit {
        offset: end,
        length: 0,
        replacement: "\n}".to_string(),
    });
}

/// L215c-cons — Emit the two-edit rewrite that turns a bare assign
/// `<binding>.<field> = <value>` into the Atomic store form
/// `<binding>.<field>.store(<value>, MemoryOrdering.Release)`. Edits:
/// (1) overwrite the byte range between the target's last byte and the
/// value's first byte (the ` = ` separator) with `.store(`; (2) insert
/// `, MemoryOrdering.Release)` immediately after the value's last byte.
/// The trailing `;` (statement terminator) falls outside both edits and
/// stays in place — `<binding>.<field>.store(v, MemoryOrdering.Release);`
/// is a valid expression-statement form. `MemoryOrdering.Release` is
/// chosen as the v1 default ordering for store; it pairs with the
/// `MemoryOrdering.Acquire` chosen for [`emit_atomic_load_after`] to
/// give canonical acquire/release semantics for ISR-style signaling.
fn emit_atomic_store_around(
    target_end: usize,
    value_start: usize,
    value_end: usize,
    out: &mut Vec<TextEdit>,
) {
    out.push(TextEdit {
        offset: target_end,
        length: value_start - target_end,
        replacement: ".store(".to_string(),
    });
    out.push(TextEdit {
        offset: value_end,
        length: 0,
        replacement: ", MemoryOrdering.Release)".to_string(),
    });
}

/// L215c-cons — Emit the one-edit rewrite that turns an rvalue read
/// `<binding>.<field>` into the Atomic load form
/// `<binding>.<field>.load(MemoryOrdering.Acquire)`. Inserts the
/// `.load(...)` suffix immediately after the field-access span; the
/// original prefix bytes (`<binding>.<field>`) stay intact so chained
/// projections / method calls on the resulting load value remain
/// syntactically rooted at the same place as the original read.
fn emit_atomic_load_after(end: usize, out: &mut Vec<TextEdit>) {
    out.push(TextEdit {
        offset: end,
        length: 0,
        replacement: ".load(MemoryOrdering.Acquire)".to_string(),
    });
}

/// L215b2 — Walk `expr` and emit a binding-root rewrite `TextEdit`
/// (`<binding>` → `self`) for every place-rooted occurrence of
/// `Identifier(<binding>)` at the object position of a `FieldAccess`.
/// Used by the SelfPrefix wrap path so wrap bodies reference
/// `self.<field>` instead of the migrating binding's name, matching
/// design.md line 8522's `lock self.field { ... }` wrap shape.
///
/// Only fires for `Identifier(<binding>)` at a `FieldAccess` object
/// position; bare references (`func(c)`, `let x = c;`) are NOT
/// rewritten — those usages pass / move the binding as a whole and
/// should stay literal. Method-call receivers and chained projections
/// rooted at `<binding>` get rewritten via the FieldAccess recursion
/// (e.g., `c.field.subfield` → the inner `FieldAccess { Identifier(c),
/// "field" }` is matched and `c` becomes `self`).
fn collect_binding_root_rewrites_in_expr(expr: &Expr, binding_name: &str, out: &mut Vec<TextEdit>) {
    match &expr.kind {
        ExprKind::FieldAccess { object, .. } => {
            if let ExprKind::Identifier(name) = &object.kind {
                if name == binding_name {
                    out.push(TextEdit {
                        offset: object.span.offset,
                        length: object.span.length,
                        replacement: "self".to_string(),
                    });
                }
            }
            // Recurse so chained field access (`c.outer.inner`) and
            // method-call receivers (`c.f.method()` — but the method
            // call form is handled via the MethodCall arm) get the
            // root rewrite via the inner FieldAccess match above.
            collect_binding_root_rewrites_in_expr(object, binding_name, out);
        }
        ExprKind::Index { object, index } => {
            collect_binding_root_rewrites_in_expr(object, binding_name, out);
            collect_binding_root_rewrites_in_expr(index, binding_name, out);
        }
        ExprKind::TupleIndex { object, .. } => {
            collect_binding_root_rewrites_in_expr(object, binding_name, out);
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_binding_root_rewrites_in_expr(object, binding_name, out);
            for a in args {
                collect_binding_root_rewrites_in_expr(&a.value, binding_name, out);
            }
        }
        ExprKind::Call { callee, args } => {
            collect_binding_root_rewrites_in_expr(callee, binding_name, out);
            for a in args {
                collect_binding_root_rewrites_in_expr(&a.value, binding_name, out);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_binding_root_rewrites_in_expr(left, binding_name, out);
            collect_binding_root_rewrites_in_expr(right, binding_name, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_binding_root_rewrites_in_expr(operand, binding_name, out);
        }
        ExprKind::Cast { expr, .. } => {
            collect_binding_root_rewrites_in_expr(expr, binding_name, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                collect_binding_root_rewrites_in_expr(s, binding_name, out);
            }
            if let Some(e) = end.as_deref() {
                collect_binding_root_rewrites_in_expr(e, binding_name, out);
            }
        }
        ExprKind::Tuple(items) => {
            for e in items {
                collect_binding_root_rewrites_in_expr(e, binding_name, out);
            }
        }
        _ => {}
    }
}

/// L215b2 — Read-site walker. Walks the function body and emits a
/// replacement edit `<binding>.<mut_field>` → `lock self.<field> {
/// self.<field> }` for every rvalue access of a migrating binding's
/// mut field. Statement-position write sites (Assign / CompoundAssign
/// rooted at the migrating binding, and mutating method-call writes on
/// the same root) are skipped — those are handled by the write walker,
/// which already covers reads in the value RHS via the surrounding
/// `lock self.<field> { ... }` block.
///
/// **Skip rules** (avoid double-wrapping or wrapping inside an
/// already-wrapped write):
/// - Assign / CompoundAssign whose target's binding root matches
///   `<binding>` — both target and value RHS are subsumed by the
///   write-wrap.
/// - Statement-position MethodCall whose receiver's binding root
///   matches `<binding>` (`c.field.push(x);`). The write walker's
///   classifier is empty in the migrate path so it doesn't fire on
///   these — but they DO contain a read of `c.field` as the receiver,
///   and independently wrapping the receiver as a read would produce
///   `lock self.field { self.field }.push(x)` (push outside the lock,
///   race condition). Leave the statement alone for the reviewer.
///
/// In all other contexts, every `FieldAccess { Identifier(<binding>),
/// <mut_field> }` is replaced with the lock-wrapped form. The
/// replacement spans the entire `FieldAccess` expression, so chained
/// projections / index access on the result (`c.field.subfield`,
/// `c.field[0]`, `c.field.to_string()`) wrap only the inner
/// `c.field` part and the outer chain follows the lock-wrap.
fn collect_lock_block_reads_in_block(
    block: &Block,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    out: &mut Vec<TextEdit>,
) {
    for stmt in &block.stmts {
        collect_lock_block_reads_in_stmt(stmt, binding_name, mut_fields, atomic_fields, out);
    }
    if let Some(e) = &block.final_expr {
        collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
    }
}

fn collect_lock_block_reads_in_stmt(
    stmt: &Stmt,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    out: &mut Vec<TextEdit>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
            // Skip both target+value when target's binding root is the
            // migrating binding — the write-wrap (or Atomic store
            // rewrite, for atomic_fields) covers the full statement.
            if let Some(place) = resolve_place_chain(target) {
                if place.root == binding_name {
                    return;
                }
            }
            collect_lock_block_reads_in_expr(target, binding_name, mut_fields, atomic_fields, out);
            collect_lock_block_reads_in_expr(value, binding_name, mut_fields, atomic_fields, out);
        }
        StmtKind::Let { value, .. } => {
            collect_lock_block_reads_in_expr(value, binding_name, mut_fields, atomic_fields, out);
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_lock_block_reads_in_expr(value, binding_name, mut_fields, atomic_fields, out);
            collect_lock_block_reads_in_block(
                else_block,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            collect_lock_block_reads_in_block(body, binding_name, mut_fields, atomic_fields, out);
        }
        StmtKind::Expr(e) => {
            // Skip statement-position method calls whose receiver root
            // is the migrating binding — see doc comment on the read
            // walker for the deadlock-vs-race rationale.
            if let ExprKind::MethodCall { object, .. } = &e.kind {
                if let Some(place) = resolve_place_chain(object) {
                    if place.root == binding_name {
                        return;
                    }
                }
            }
            collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
        }
    }
}

fn collect_lock_block_reads_in_expr(
    expr: &Expr,
    binding_name: &str,
    mut_fields: &HashSet<String>,
    atomic_fields: &HashSet<String>,
    out: &mut Vec<TextEdit>,
) {
    // Read-site pattern: `<binding>.<mut_field>` as a rvalue.
    //
    // FieldAccess `Expr.span` only covers the object (see
    // `src/parser/exprs.rs:149` — `span: lhs.span.clone()`), so we
    // compute the replacement extent ourselves: object span +
    // `.` + field name length. This assumes no whitespace around
    // the `.`, which is the canonical Kāra formatting; whitespace-
    // around-dot lands a slightly-wrong edit and the reviewer
    // hand-completes it.
    if let ExprKind::FieldAccess { object, field } = &expr.kind {
        if let ExprKind::Identifier(name) = &object.kind {
            if name == binding_name && mut_fields.contains(field) {
                let start = object.span.offset;
                let len = object.span.length + 1 + field.len();
                if atomic_fields.contains(field) {
                    // L215c-cons — Atomic[T]-classified field read.
                    // Append `.load(MemoryOrdering.Acquire)` to the
                    // FieldAccess; leave the original `<binding>.<field>`
                    // text intact so chained projections on the result
                    // (`c.field.subfield`, `c.field[0]`) still resolve
                    // against the loaded value. Defensive note: the
                    // classifier disqualifies fields with any mutating
                    // method call, so `c.field.push()` shapes don't
                    // reach here — but for a `.method()` shape that
                    // somehow does, this append would produce
                    // `c.field.load(MemoryOrdering.Acquire).method()`
                    // which is semantically a load-then-call (safe vs
                    // wrong-receiver). Reviewer would catch the shape.
                    emit_atomic_load_after(start + len, out);
                } else {
                    out.push(TextEdit {
                        offset: start,
                        length: len,
                        replacement: format!("lock self.{field} {{ self.{field} }}"),
                    });
                }
                // Don't descend — the FieldAccess.object (`Identifier(c)`)
                // and `.field` are subsumed by the wrap-replacement (or
                // the load-rewrite has produced its single edit and the
                // children are pure binding+field-name with no further
                // reads to visit).
                return;
            }
        }
    }
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            collect_lock_block_reads_in_block(b, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_lock_block_reads_in_expr(
                condition,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
            collect_lock_block_reads_in_block(
                then_block,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
            if let Some(eb) = else_branch {
                collect_lock_block_reads_in_expr(eb, binding_name, mut_fields, atomic_fields, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_lock_block_reads_in_expr(
                condition,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
            collect_lock_block_reads_in_block(body, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::For { iterable, body, .. } => {
            collect_lock_block_reads_in_expr(
                iterable,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
            collect_lock_block_reads_in_block(body, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_lock_block_reads_in_expr(
                scrutinee,
                binding_name,
                mut_fields,
                atomic_fields,
                out,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_lock_block_reads_in_expr(
                        g,
                        binding_name,
                        mut_fields,
                        atomic_fields,
                        out,
                    );
                }
                collect_lock_block_reads_in_expr(
                    &arm.body,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    out,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            collect_lock_block_reads_in_expr(callee, binding_name, mut_fields, atomic_fields, out);
            for a in args {
                collect_lock_block_reads_in_expr(
                    &a.value,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    out,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_lock_block_reads_in_expr(object, binding_name, mut_fields, atomic_fields, out);
            for a in args {
                collect_lock_block_reads_in_expr(
                    &a.value,
                    binding_name,
                    mut_fields,
                    atomic_fields,
                    out,
                );
            }
        }
        ExprKind::FieldAccess { object, .. } => {
            // Non-matching FieldAccess (other.field, c.non_mut_field).
            collect_lock_block_reads_in_expr(object, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Index { object, index } => {
            collect_lock_block_reads_in_expr(object, binding_name, mut_fields, atomic_fields, out);
            collect_lock_block_reads_in_expr(index, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::TupleIndex { object, .. } => {
            collect_lock_block_reads_in_expr(object, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Binary { left, right, .. } => {
            collect_lock_block_reads_in_expr(left, binding_name, mut_fields, atomic_fields, out);
            collect_lock_block_reads_in_expr(right, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Unary { operand, .. } => {
            collect_lock_block_reads_in_expr(operand, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Cast { expr, .. } => {
            collect_lock_block_reads_in_expr(expr, binding_name, mut_fields, atomic_fields, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                collect_lock_block_reads_in_expr(s, binding_name, mut_fields, atomic_fields, out);
            }
            if let Some(e) = end.as_deref() {
                collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
            }
        }
        ExprKind::Tuple(items) => {
            for e in items {
                collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                collect_lock_block_reads_in_expr(e, binding_name, mut_fields, atomic_fields, out);
            }
        }
        _ => {}
    }
}

fn collect_identifier_uses_in_stmt(
    stmt: &Stmt,
    tracked: &HashMap<String, TrackedBinding>,
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    out: &mut HashMap<String, BranchUse>,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { value, .. } => {
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &else_block.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &else_block.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        StmtKind::Assign { target, value } => {
            collect_identifier_uses_in_expr(
                target,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            collect_identifier_uses_in_expr(
                target,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                value,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        StmtKind::Expr(e) => {
            collect_identifier_uses_in_expr(e, tracked, closure_captures, closure_bindings, out);
        }
    }
}

fn collect_identifier_uses_in_expr(
    expr: &Expr,
    tracked: &HashMap<String, TrackedBinding>,
    closure_captures: &ClosureCaptures,
    closure_bindings: &ClosureBindings,
    out: &mut HashMap<String, BranchUse>,
) {
    match &expr.kind {
        ExprKind::Identifier(name) => {
            // Direct tracked-binding reference.
            if tracked.contains_key(name) {
                record_use(out, name.clone(), expr.span.clone(), true);
            }
            // Indirect reference via a let-bound closure that captures
            // tracked bindings — `let f = || use(c);` followed by a
            // sibling-branch `Identifier(f)` counts as a branch-use of
            // `c`. Mirrors `par_helpers.rs::scan_expr_for_par_uses`'s
            // round-12.34 closure_bindings propagation so closure-
            // dispatched flows of a shared/plain struct don't slip past
            // the per-branch identifier walk.
            for cap in expand_through_closure_bindings(name, closure_bindings) {
                if tracked.contains_key(&cap) {
                    record_use(out, cap, expr.span.clone(), true);
                }
            }
        }
        ExprKind::Closure { body, .. } => {
            // Inline closure form — `spawn(|| use(c))`. The closure's
            // captures (resolved by the OwnershipChecker's capture-
            // inference pass and stored in `closure_captures` keyed by
            // the closure expression's span) are the explicit set of
            // outer names the body references; each captured name that
            // resolves to a tracked binding (directly or transitively
            // via another closure binding) counts as a branch-use of
            // that name, recorded at the closure expression's span.
            let key = SpanKey::from_span(&expr.span);
            if let Some(captures) = closure_captures.get(&key) {
                for (cap_name, _) in captures {
                    if tracked.contains_key(cap_name) {
                        record_use(out, cap_name.clone(), expr.span.clone(), true);
                    }
                    for chained in expand_through_closure_bindings(cap_name, closure_bindings) {
                        if tracked.contains_key(&chained) {
                            record_use(out, chained, expr.span.clone(), true);
                        }
                    }
                }
            }
            // Closure bodies are NOT recursively walked here — the
            // `closure_captures` map is the authoritative set of outer
            // names the body references (filtered through the body's
            // own bindings and shadowing). Walking the body textually
            // would risk double-counting locals or shadowed names.
            // The body's effects on outer state are already reflected
            // in the captures list. Nested par-blocks inside closure
            // bodies are likewise not detected here at v1; if a future
            // workload surfaces them, the fix is to recurse via
            // `scan_block_for_par_conflicts` over the closure body
            // rather than through this identifier walker.
            let _ = body;
        }
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            for s in &b.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &b.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            collect_identifier_uses_in_expr(
                condition,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &then_block.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &then_block.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(eb) = else_branch {
                collect_identifier_uses_in_expr(
                    eb,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            collect_identifier_uses_in_expr(
                condition,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::For { iterable, body, .. } => {
            collect_identifier_uses_in_expr(
                iterable,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for s in &body.stmts {
                collect_identifier_uses_in_stmt(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = &body.final_expr {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_identifier_uses_in_expr(
                scrutinee,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_identifier_uses_in_expr(
                        g,
                        tracked,
                        closure_captures,
                        closure_bindings,
                        out,
                    );
                }
                collect_identifier_uses_in_expr(
                    &arm.body,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            collect_identifier_uses_in_expr(
                callee,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for a in args {
                collect_identifier_uses_in_expr(
                    &a.value,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            for a in args {
                collect_identifier_uses_in_expr(
                    &a.value,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::FieldAccess { object, field } => {
            // B-2026-08-01-33 — the one arm that can mark a use NON-material.
            // `<tracked shared binding>.<immutable scalar field>` reads the
            // handle's payload with a plain deref: no rc inc/dec, no buffer
            // copy (measured — see `classify_tracked_binding`). Recording it
            // here and NOT descending is what keeps the bare `Identifier`
            // underneath from registering a material use.
            //
            // The field must be BOTH immutable and scalar, so this can admit
            // neither a nested-handle materialization (a `shared`-typed field
            // is not scalar) nor a write. Writes are covered from both sides,
            // which is why this arm needs no write-position tracking:
            //   * `root.a = 9` with `a` DECLARED MUT — `a` is in
            //     `mut_fields`, so it is absent from the readonly set and
            //     falls through to the material walk below, which fires.
            //   * `root.a = 9` with `a` IMMUTABLE — this arm does mark it
            //     non-material, but the program is already dead: the
            //     typechecker rejects it upstream with "shared struct field
            //     'Node.a' is not declared mut" (verified). No suppression
            //     here can make such a program compile.
            if let ExprKind::Identifier(base) = &object.kind {
                if tracked
                    .get(base)
                    .is_some_and(|b| b.readonly_scalar_fields.contains(field))
                {
                    record_use(out, base.clone(), expr.span.clone(), false);
                    return;
                }
            }
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Index { object, index } => {
            collect_identifier_uses_in_expr(
                object,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
            collect_identifier_uses_in_expr(
                index,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Binary { left, right, .. } => {
            collect_identifier_uses_in_expr(left, tracked, closure_captures, closure_bindings, out);
            collect_identifier_uses_in_expr(
                right,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Unary { operand, .. } => {
            collect_identifier_uses_in_expr(
                operand,
                tracked,
                closure_captures,
                closure_bindings,
                out,
            );
        }
        ExprKind::Tuple(items) => {
            for e in items {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Return(inner) => {
            if let Some(e) = inner.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        ExprKind::Cast { expr, .. } => {
            collect_identifier_uses_in_expr(expr, tracked, closure_captures, closure_bindings, out);
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start.as_deref() {
                collect_identifier_uses_in_expr(
                    s,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
            if let Some(e) = end.as_deref() {
                collect_identifier_uses_in_expr(
                    e,
                    tracked,
                    closure_captures,
                    closure_bindings,
                    out,
                );
            }
        }
        _ => {}
    }
}

/// Expand `name` through `closure_bindings` and return every closure-
/// captured name reachable from it (excluding `name` itself). Cycle-
/// safe via the visited set — guards against pathological self-capture
/// shapes the parser can't produce today but the data structure
/// permits. The returned set never contains `name` itself; the caller
/// is responsible for recording the direct hit when applicable.
fn expand_through_closure_bindings(name: &str, closure_bindings: &ClosureBindings) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(name.to_string());
    let mut stack: Vec<String> = vec![name.to_string()];
    while let Some(n) = stack.pop() {
        if let Some(captures) = closure_bindings.get(&n) {
            for c in captures {
                if seen.insert(c.clone()) {
                    out.push(c.clone());
                    stack.push(c.clone());
                }
            }
        }
    }
    out
}

/// Forward-walk every `let pat = closure_expr;` form in `body` and
/// register each pattern binding name in `out` mapping to the closure's
/// inferred capture list. Mirrors `par_helpers.rs`'s round-12.34 Step
/// 6 mechanism so a sibling-branch `Identifier(f)` use of a let-bound
/// closure can transitively count as a use of each captured tracked
/// binding. Single forward pass is sufficient because closure bindings
/// must be declared before they are referenced in source order.
fn build_closure_bindings(
    body: &Block,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    for stmt in &body.stmts {
        build_closure_bindings_stmt(stmt, closure_captures, out);
    }
    if let Some(e) = &body.final_expr {
        build_closure_bindings_expr(e, closure_captures, out);
    }
}

fn build_closure_bindings_stmt(
    stmt: &Stmt,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    match &stmt.kind {
        StmtKind::MultiAssign { .. } => unreachable!(
            "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
        ),
        StmtKind::Let { pattern, value, .. } => {
            if matches!(value.kind, ExprKind::Closure { .. }) {
                if let Some(captures) = closure_captures.get(&SpanKey::from_span(&value.span)) {
                    let names: Vec<String> = captures.iter().map(|(n, _)| n.clone()).collect();
                    for binding in pattern.binding_names() {
                        out.insert(binding, names.clone());
                    }
                }
            }
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            build_closure_bindings_expr(value, closure_captures, out);
            build_closure_bindings(else_block, closure_captures, out);
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            build_closure_bindings(body, closure_captures, out);
        }
        StmtKind::Assign { target, value } => {
            build_closure_bindings_expr(target, closure_captures, out);
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            build_closure_bindings_expr(target, closure_captures, out);
            build_closure_bindings_expr(value, closure_captures, out);
        }
        StmtKind::Expr(e) => {
            build_closure_bindings_expr(e, closure_captures, out);
        }
    }
}

fn build_closure_bindings_expr(
    expr: &Expr,
    closure_captures: &ClosureCaptures,
    out: &mut ClosureBindings,
) {
    match &expr.kind {
        ExprKind::Block(b)
        | ExprKind::Par(b)
        | ExprKind::Seq(b)
        | ExprKind::Try(b)
        | ExprKind::Unsafe(b)
        | ExprKind::LabeledBlock { body: b, .. }
        | ExprKind::Loop { body: b, .. }
        | ExprKind::Lock { body: b, .. } => {
            build_closure_bindings(b, closure_captures, out);
        }
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            build_closure_bindings_expr(condition, closure_captures, out);
            build_closure_bindings(then_block, closure_captures, out);
            if let Some(eb) = else_branch {
                build_closure_bindings_expr(eb, closure_captures, out);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            build_closure_bindings_expr(condition, closure_captures, out);
            build_closure_bindings(body, closure_captures, out);
        }
        ExprKind::For { iterable, body, .. } => {
            build_closure_bindings_expr(iterable, closure_captures, out);
            build_closure_bindings(body, closure_captures, out);
        }
        ExprKind::Match { scrutinee, arms } => {
            build_closure_bindings_expr(scrutinee, closure_captures, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    build_closure_bindings_expr(g, closure_captures, out);
                }
                build_closure_bindings_expr(&arm.body, closure_captures, out);
            }
        }
        ExprKind::Call { callee, args } => {
            build_closure_bindings_expr(callee, closure_captures, out);
            for a in args {
                build_closure_bindings_expr(&a.value, closure_captures, out);
            }
        }
        ExprKind::MethodCall { object, args, .. } => {
            build_closure_bindings_expr(object, closure_captures, out);
            for a in args {
                build_closure_bindings_expr(&a.value, closure_captures, out);
            }
        }
        _ => {}
    }
}
