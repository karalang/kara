//! `frozen` parameter escape check — B-2026-08-01-33 mechanism 3, stages 1–2.
//!
//! ## Why this exists, and why it landed first
//!
//! A `frozen T` is a **non-counting** handle: codegen emits no
//! `rc_inc`/`rc_dec` for it, which is what makes concurrent reads across `par`
//! branches safe (it removes the raced refcount header rather than making it
//! atomic). That property is only sound while the handle cannot outlive the
//! owner whose count it is skipping — a non-counting handle that escapes is a
//! use-after-free.
//!
//! So escape checking is the precondition for every other part of the feature.
//! It landed *before* `par` admission and before RC suppression, while the mode
//! was still inert, so that admission had something to stand on. Both now
//! depend on it. See
//! [`docs/spikes/freeze-point-design.md`](../../docs/spikes/freeze-point-design.md)
//! § "Risks, stated plainly".
//!
//! ## Shape: whitelist, not blacklist
//!
//! The repo's three analysis bugs of 2026-08-04 (B-2026-08-04-13/-14/-15) were
//! one failure mode in three subsystems: *a walk that recognized some
//! place-expression spellings and silently ignored the rest*. Every one of them
//! enumerated the forms it handled and let the others fall through a `_` arm.
//!
//! This module inverts that. The walks below are **exhaustive matches with no
//! `_` wildcard**, so a new AST node breaks this file's build instead of
//! silently opening an escape route. And a frozen PLACE is judged in
//! [`walk_expr`]'s prologue, which consumes it: only the positions explicitly
//! permitted below skip that judgement, and they do so by intercepting *before*
//! the prologue runs. Every other position — including any position nobody
//! thought about — reaches the prologue with a place that is not a scalar read
//! and is reported. The failure direction is a false positive (a rejected
//! program that could have been allowed), never a missed escape.
//!
//! ## What is permitted (stage 2: PLACES, not bindings)
//!
//! Stage 1 permitted exactly two shapes: reading an immutable *scalar field*
//! one level deep, and passing the *bare handle* to another `frozen`
//! parameter. Stage 2 generalises both to a **frozen place** — the parameter
//! itself, or any chain of field access / indexing / tuple indexing rooted at
//! it (`n.inner.deep`, `n.kids[i]`, `n.pair.1`).
//!
//! 1. **Reading a frozen place whose type is a scalar** — `n.val`,
//!    `n.inner.deep.d`, `n.kids[i].val`. Lowers to a chain of derefs and
//!    yields a register copy, so no handle leaves.
//! 2. **Passing a frozen place whole to another `frozen` parameter** —
//!    `helper(n)`, `helper(n.inner)`, `helper(n.kids[i])`. The callee is
//!    checked by this same pass, so the guarantee composes across the call
//!    rather than being re-derived at each site. This is the property that
//!    makes a callee-side traversal (LeetCode #133) reachable at all.
//! 3. **`len()` / `is_empty()` on a builtin container reached through a
//!    place** — `n.kids.len()`. A narrow, explicitly-enumerated exception,
//!    justified below.
//!
//! **Why places and not bindings — and what changed.** Measured per-function
//! refcount traffic in the emitted IR, borrow-mode receiver, `outer`'s own
//! frame:
//!
//! | body | `rc_inc` | `rc_dec` |
//! |---|---|---|
//! | `readd(o.inner.deep)` — chained projection into a frozen slot | 0 | 0 |
//! | `readi(o.kids[i])` — index projection into a frozen slot | 0 | 0 |
//! | `o.kids.len()` | 0 | 0 |
//! | `let k = o.inner; k.v` — **bound to a local** | **2** | **3** |
//!
//! A projection is a deref; a *binding* materialised a counted handle. Stage 2
//! drew its boundary there, on that measurement, and refused every binding
//! form. Stage 2.5 does not move the boundary by argument — it **removes the
//! traffic**, which is the only thing that was ever in the way:
//!
//! 4. **Binding a frozen place to an immutable local** — `let k = n.kids[i];`.
//!    Admitted, and compiled as a NON-COUNTING ALIAS: codegen skips the
//!    element clone, the receive-inc, and the scope-exit dec, so the row above
//!    reads 0/0 for this shape too. The local then becomes a frozen root
//!    itself, so every later use of it is judged by exactly the rules that
//!    govern the parameter — it can be read, projected, passed to another
//!    `frozen` slot, and aliased again, and it can no more escape than the
//!    parameter can.
//! 5. **Iterating a container reached through a frozen place** — `for k in
//!    n.kids { … }` (stage 2.6). The loop variable is a frozen root on the
//!    same terms as (4) when the element is a `shared struct`, and an
//!    ordinary binding when it is a scalar, since a register copy of an `i64`
//!    has nothing to escape with.
//!
//!    This one needed **no codegen change at all**, and stage 2.5 said
//!    otherwise: it recorded `for` as refused "because the loop lowers an
//!    element copy, not an alias". That was asserted without being measured
//!    and it is wrong. The emitted loop is
//!
//!    ```text
//!    %for.v.elem = load ptr, ptr %for.v.elem.ptr   ; the handle, out of the Vec
//!    store ptr %for.v.elem, ptr %k                 ; into the loop variable's slot
//!    ```
//!
//!    — a pointer load and a store, with no clone, no retain, no release and
//!    no cleanup at `for.exit`, in borrow mode and `frozen` mode alike. There
//!    was never any traffic here to remove; only this walk was refusing.
//!
//! This is the "codegen binding class whose slot aliases an existing owner
//! without retaining" that `docs/spikes/freeze-point-design.md` § "Stage 3"
//! identifies as the capability the freeze *statement* needs; stage 2.5 builds
//! it for the parameter-rooted case, where the owner is the caller and its
//! lifetime is the whole call. What licenses it is not new: the freeze-site
//! check (E0512) has already proved every reachable type is free of `mut`
//! fields, so no branch can write the payload, and this walk has already
//! proved the root cannot escape, so no alias of it can outlive the owner.
//!
//! What is still refused: a `mut` alias, any binding whose place this pass
//! cannot resolve to a `shared struct`, a `for` over a container this pass
//! does not model (`Map`/`Set`) or one whose element is neither a handle nor
//! a scalar, and every escape. Both directions are pinned from the codegen
//! side by
//! `frozen_place_projection_emits_no_refcount_traffic` (tests/par_codegen.rs),
//! so a codegen change that starts retaining across a projection or an alias
//! turns that test red instead of silently invalidating this rule.
//!
//! **Why `len` / `is_empty` are carved out.** Without a length there is no
//! bounded traversal, so the rest of stage 2 would be unreachable for the
//! shape that motivates it. The exception is kept honest by being narrow on
//! three independent axes: the method name is one of exactly two, the receiver
//! must resolve to a BUILTIN container (never a user aggregate, so no user
//! body ever receives the handle), and any type carrying a user `impl` block
//! is excluded outright — so `impl Vec { fn len(...) }` cannot smuggle a body
//! in through the builtin name. Both methods return a count; neither can
//! retain the receiver.
//!
//! ## Known conservatism, stated so it is not mistaken for a hole
//!
//! - **Shadowing is not tracked.** An inner `let n = …` that shadows a frozen
//!   parameter makes later uses of `n` look like uses of the parameter. That
//!   over-reports (the shadowing `let` is itself already an escape, so the
//!   function is rejected either way) and never under-reports.
//! - **Only free-function calls compose.** A `frozen` argument in a *method*
//!   call is reported, because resolving a method to its declaration needs the
//!   typechecker's callee map and this pass does not wire it.
//! - **Unknown types fail closed.** Place-type resolution is positive-evidence
//!   only: a field on a type with no `struct_info` entry, an index into
//!   anything that is not a `Vec` / array / slice, or a projection through a
//!   field this pass cannot resolve all yield "no type", and a place with no
//!   type is reported.
//! - **A `mut` field is refused at every step**, even though the freeze-site
//!   check (E0512) has already refused any type whose reachable closure
//!   contains one. Two independent guards, because the projection rule's
//!   safety argument ("concurrent readers cannot race a payload nobody may
//!   write") should not silently depend on a check in another module.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{types::Type, TypeCheckResult};

use super::{OwnershipError, OwnershipErrorKind};

/// Why a particular use was rejected. Selects the diagnostic wording; each
/// variant names a *specific* thing the user wrote rather than falling back on
/// "this is not allowed", because the legal surface is small enough that a bare
/// rejection would leave the reader guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reason {
    /// A bare use of the handle in any position other than the permitted
    /// ones — returned, bound to a local, stored, captured, and so on.
    Materialized,
    /// `….field` where `field` is `mut` or unresolvable, or where the place it
    /// names is neither read as a scalar nor passed to a `frozen` slot.
    Projection { field: String },
    /// `….[i]` / `….0` in the same situation as [`Reason::Projection`] — the
    /// element is a handle and the position does not permit one.
    Element,
    /// Passed to a call slot whose parameter is not declared `frozen` (or to a
    /// callee this pass cannot resolve, which includes every method call).
    NonFrozenArgument,
    /// Referenced from inside a CLOSURE BODY. The closure's environment holds
    /// the handle, and the closure can outlive the call (returned, stored,
    /// handed to `spawn`), so even the otherwise-permitted uses are reported
    /// there. `par` / `seq` blocks are NOT closures for this purpose: their
    /// branches join before the function returns, and admitting exactly that
    /// sharing is the point of the feature.
    Captured,
}

/// One rejected use, with enough to word the diagnostic. `is_alias`
/// distinguishes a stage-2.5 LOCAL alias from the parameter it was derived
/// from: both obey the same rule, but calling a `let` binding a "parameter"
/// sends the reader to the wrong line to fix it.
struct Rejection {
    name: String,
    span: Span,
    reason: Reason,
    is_alias: bool,
}

/// Walk state. `frozen` maps each in-scope frozen parameter name to the *type*
/// its place chain starts from; every projection resolves one step further
/// through [`place_type`], so a chain of any depth is answerable from here.
struct Cx<'a, 't> {
    tc: &'t TypeCheckResult,
    frozen: HashMap<&'a str, Type>,
    /// Free-function name → per-position `is_frozen` flags, used to decide
    /// whether passing the handle on is permitted.
    fn_frozen_params: HashMap<&'a str, Vec<bool>>,
    /// Type names carrying a user `impl` block. A builtin container name that a
    /// user has extended is not treated as a builtin by the `len`/`is_empty`
    /// exception, so no user body can receive the handle through it.
    user_impl_types: HashSet<String>,
    /// True while walking a closure body. A reference to a frozen parameter
    /// there is a CAPTURE into an environment that can outlive the call, so
    /// the two permitted positions are suppressed — reading a scalar field
    /// off the handle is only safe because no handle leaves, and inside a
    /// closure the handle itself has already left. Same argument, and the same
    /// flag, as `result_escape.rs`'s `in_closure`.
    in_closure: bool,
    /// Names in `frozen` that are stage-2.5 LOCAL ALIASES rather than
    /// parameters. Diagnostics only — the rule is identical for both.
    aliases: HashSet<&'a str>,
    found: Vec<Rejection>,
    /// Stage 2.5 — the initializer span of every `let` admitted as a
    /// NON-COUNTING ALIAS of a frozen place. Handed to codegen (through
    /// `OwnershipCheckResult::frozen_alias_bindings`) as the instruction to
    /// skip the element clone, the receive-inc, and the scope-exit dec for
    /// that binding. Keyed by the initializer rather than the statement
    /// because that is the span codegen already has in hand at the decision
    /// point, and the one `vec_index_borrow_spans` — the existing
    /// alias-instead-of-own channel — is keyed by.
    alias_spans: HashSet<SpanKey>,
}

impl<'a> Cx<'a, '_> {
    /// The frozen parameter a PLACE expression is rooted at, if any.
    ///
    /// Recognises exactly the three projection forms stage 2 makes sticky —
    /// field access, indexing, tuple indexing — plus the bare identifier. Any
    /// other expression shape is not a place, so it is not rooted anywhere and
    /// its sub-expressions are walked normally (where a frozen identifier
    /// reports at the leaf).
    fn frozen_place_root(&self, e: &Expr) -> Option<&'a str> {
        match &e.kind {
            ExprKind::Identifier(name) => self.frozen.get_key_value(name.as_str()).map(|(k, _)| *k),
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::TupleIndex { object, .. } => self.frozen_place_root(object),
            _ => None,
        }
    }

    /// The static type of a frozen place, resolved one projection at a time.
    ///
    /// POSITIVE EVIDENCE ONLY: every step that this pass cannot resolve —
    /// a field on a type with no `struct_info` entry, an index into something
    /// that is not a `Vec` / array / slice, a `mut` field — yields `None`, and
    /// a place with no type is reported by the caller. The failure direction is
    /// a rejected program, never an admitted handle.
    fn place_type(&self, e: &Expr) -> Option<Type> {
        match &e.kind {
            ExprKind::Identifier(name) => self.frozen.get(name.as_str()).cloned(),
            ExprKind::FieldAccess { object, field } => {
                let base = self.place_type(object)?;
                let info = self.tc.struct_info.get(head_type_name(&base)?)?;
                // Refused even though E0512 has already refused any type whose
                // reachable closure contains a `mut` field — see the module
                // header on why this guard is deliberately independent.
                if info.mut_fields.contains(field) {
                    return None;
                }
                info.fields
                    .iter()
                    .find(|(fname, _, _)| fname == field)
                    .map(|(_, fty, _)| fty.clone())
            }
            ExprKind::Index { object, .. } => element_type(&self.place_type(object)?),
            ExprKind::TupleIndex { object, index } => match self.place_type(object)? {
                Type::Tuple(items) => items.get(*index as usize).cloned(),
                _ => None,
            },
            _ => None,
        }
    }

    /// The type of a frozen place when it names a **`shared struct` handle** —
    /// the only thing stage 2.5 will alias to a local.
    ///
    /// Narrower than "not a scalar" on purpose. A place typed `Vec[T]` or
    /// `String` is a `{ptr,len,cap}` aggregate whose binding codegen registers
    /// as a buffer owner, and a place on a GENERIC type resolves its fields
    /// through unsubstituted parameter names — neither is a single refcounted
    /// pointer, which is what the alias class is. Both yield `None` here and
    /// are reported by the ordinary walk.
    fn shared_handle_type(&self, e: &Expr) -> Option<Type> {
        self.as_shared_handle(self.place_type(e)?)
    }

    /// `Some(ty)` when `ty` names a non-generic `shared struct`. Shared by the
    /// two admission sites so "what may be aliased" has one definition.
    fn as_shared_handle(&self, ty: Type) -> Option<Type> {
        let info = self.tc.struct_info.get(head_type_name(&ty)?)?;
        (info.is_shared && info.generic_params.is_empty()).then_some(ty)
    }

    /// Whether a frozen place reads through to a SCALAR — the register-copy
    /// case, which no position can turn into an escape because no handle
    /// exists to escape.
    fn reads_as_scalar(&self, e: &Expr) -> bool {
        self.place_type(e)
            .is_some_and(|t| super::is_copy_type_basic(&t))
    }

    /// Whether `method` on this frozen place is one of the two read-only
    /// builtin-container queries stage 2 permits. See the module header for why
    /// the carve-out exists and on which three axes it is kept narrow.
    fn permits_builtin_query(&self, receiver: &Expr, method: &str) -> bool {
        if !matches!(method, "len" | "is_empty") {
            return false;
        }
        let Some(ty) = self.place_type(receiver) else {
            return false;
        };
        match &ty {
            Type::Array { .. } | Type::Slice { .. } | Type::Str => true,
            Type::Named { name, .. } => {
                matches!(name.as_str(), "Vec" | "String" | "Map" | "Set" | "Deque")
                    && !self.user_impl_types.contains(name)
            }
            _ => false,
        }
    }

    fn flag(&mut self, name: &str, span: &Span, reason: Reason) {
        self.found.push(Rejection {
            is_alias: self.aliases.contains(name),
            name: name.to_string(),
            span: span.clone(),
            reason,
        });
    }
}

/// The struct/enum name a type is headed by, for a `struct_info` lookup.
fn head_type_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Shared(name) => Some(name),
        Type::Named { name, .. } => Some(name),
        _ => None,
    }
}

/// The element type indexing yields, for the containers stage 2 models.
///
/// Deliberately short: a `Map`/`Set` index is not listed, so indexing one
/// yields `None` and is reported. Widening this list is a decision, not an
/// oversight.
fn element_type(ty: &Type) -> Option<Type> {
    match ty {
        Type::Array { element, .. }
        | Type::Slice { element, .. }
        | Type::Vector { element, .. } => Some((**element).clone()),
        Type::Named { name, args } if name == "Vec" && args.len() == 1 => Some(args[0].clone()),
        _ => None,
    }
}

/// The diagnostic category a rejected frozen place falls into, chosen from the
/// place's OWN outermost form so the message names what the author wrote.
fn place_reason(e: &Expr) -> Reason {
    match &e.kind {
        ExprKind::FieldAccess { field, .. } => Reason::Projection {
            field: field.clone(),
        },
        ExprKind::Index { .. } | ExprKind::TupleIndex { .. } => Reason::Element,
        _ => Reason::Materialized,
    }
}

impl super::OwnershipChecker<'_> {
    /// Report every use of a `frozen` parameter of `f` that is not one of the
    /// shapes this pass permits. Emits `E0511` at each offending use.
    ///
    /// No-op — and, importantly, no program-wide work — for a function with no
    /// `frozen` parameter, which today is every function in every program.
    pub(crate) fn check_frozen_param_escape(&mut self, f: &Function) {
        if !f.params.iter().any(|p| p.is_frozen) {
            return;
        }

        // Computed through a free function so the immutable borrow of
        // `typecheck_result` / `program` the walk needs is finished before the
        // errors are pushed through `&mut self`.
        let (found, aliases) = collect_frozen_escapes(f, self.program, self.typecheck_result);

        // Stage 2.5 alias bindings, surfaced to codegen. Recorded even when
        // this function also produced errors — the program will not compile in
        // that case, so the set is never read, and gating on `found.is_empty()`
        // would just add a state the tests cannot reach.
        self.frozen_alias_bindings.extend(aliases);

        for Rejection {
            name,
            span,
            reason,
            is_alias,
        } in found
        {
            // A stage-2.5 local alias obeys the parameter's rule but is not a
            // parameter; naming it one would point the reader at the signature
            // when the line to change is the `let`.
            let noun = if is_alias { "alias" } else { "parameter" };
            // Only the `Materialized` help offers "take it by value instead",
            // which is advice about a SIGNATURE — so an alias needs the extra
            // sentence saying which signature.
            let alias_note = if is_alias {
                format!(
                    " `{name}` is a non-counting alias of a `frozen` parameter and inherits its \
                     restrictions, so the declaration to change is that parameter's."
                )
            } else {
                String::new()
            };
            let (what, fix) = match &reason {
                Reason::Materialized => (
                    format!("`frozen` {noun} `{name}` escapes here"),
                    format!(
                        "a `frozen` handle is non-counting, so it must not outlive the call. \
                         The permitted uses are: reading a place off `{name}` whose type is a \
                         SCALAR (`{name}.field`, `{name}.a.b`, `{name}.items[i].n`), passing \
                         such a place whole to another parameter that is also declared \
                         `frozen`, `len()` / `is_empty()` on a container reached through one, \
                         and binding one to an IMMUTABLE local when it names a `shared struct` \
                         (`let k = {name}.items[i];` — `k` is then an alias of `{name}` and \
                         carries the same restrictions). To store, return, or capture the \
                         handle, take the parameter by value instead of `frozen`.{alias_note}"
                    ),
                ),
                Reason::Projection { field } => (
                    format!("`frozen` {noun} `{name}` cannot be projected through `.{field}` yet"),
                    format!(
                        "a place off a `frozen` handle may be READ when it resolves to a \
                         scalar, or passed whole to another `frozen` parameter — both lower to \
                         derefs with no refcount traffic. This one is neither: `{field}` is \
                         `mut`, or lives on a type this pass could not resolve, or the place \
                         is a handle in a position that would materialise it"
                    ),
                ),
                Reason::Element => (
                    format!("`frozen` {noun} `{name}` cannot be used through this element yet"),
                    format!(
                        "indexing a place off a `frozen` handle yields a value that may be READ \
                         when it is a scalar, or passed whole to another `frozen` parameter. \
                         This position would materialise it instead — or the container is one \
                         this pass does not model (only `Vec`, arrays and slices are indexed \
                         through `{name}`)"
                    ),
                ),
                Reason::Captured => (
                    format!("`frozen` {noun} `{name}` is captured by a closure"),
                    format!(
                        "the closure's environment holds the handle and can outlive the call \
                         — returned, stored, or handed to `spawn` — so a non-counting handle \
                         would be left pointing at freed memory. Read what you need from \
                         `{name}` into a local BEFORE the closure and capture that instead. \
                         (`par` / `seq` branches are not closures here: they join before the \
                         function returns.)"
                    ),
                ),
                Reason::NonFrozenArgument => (
                    format!("`frozen` {noun} `{name}` is passed to a non-`frozen` slot"),
                    "the callee could store the handle, so the guarantee has to hold on its \
                     side too. Declare the receiving parameter `frozen` as well, and the check \
                     composes across the call. Method calls are not resolved by this pass and \
                     are reported even when the parameter is `frozen`"
                        .to_string(),
                ),
            };
            self.errors.push(OwnershipError {
                message: what,
                span,
                kind: OwnershipErrorKind::FrozenParamEscapes,
                suggestion: Some(fix),
                replacement: None,
                consume_span: None,
            });
        }
    }
}

/// Run the walk over `f` and return every rejected use, plus the initializer
/// spans of the `let` bindings admitted as non-counting aliases. Free-standing
/// so the shared borrows of `program` / `tc` it holds end before the caller
/// pushes diagnostics through `&mut self`.
fn collect_frozen_escapes<'a>(
    f: &'a Function,
    program: &'a Program,
    tc: &TypeCheckResult,
) -> (Vec<Rejection>, HashSet<SpanKey>) {
    let mut frozen: HashMap<&str, Type> = HashMap::new();
    for p in f.params.iter().filter(|p| p.is_frozen) {
        let Some(root) = frozen_root_type(&p.ty) else {
            continue;
        };
        for name in binding_names_of(&p.pattern) {
            frozen.insert(name, root.clone());
        }
    }
    if frozen.is_empty() {
        return (Vec::new(), HashSet::new());
    }

    let mut cx = Cx {
        tc,
        frozen,
        fn_frozen_params: collect_fn_frozen_params(program),
        user_impl_types: collect_user_impl_types(program),
        in_closure: false,
        aliases: HashSet::new(),
        found: Vec::new(),
        alias_spans: HashSet::new(),
    };
    walk_block(&f.body, &mut cx);
    (cx.found, cx.alias_spans)
}

/// The type a `frozen` parameter's place chain starts from.
///
/// Fail-closed: a type expression that is not a plain path yields `None`, the
/// parameter contributes no tracked name, and — if it was the only one — the
/// function is skipped. That is safe because a parameter whose type this pass
/// cannot name is also one the freeze-site check (E0512) refuses outright, so
/// the program does not compile either way.
fn frozen_root_type(ty: &TypeExpr) -> Option<Type> {
    // A `frozen` param is stored as `Ref(T)` — see the parser's `frozen` arm.
    let ty = match &ty.kind {
        TypeKind::Ref(inner) => inner.as_ref(),
        _ => ty,
    };
    let TypeKind::Path(path) = &ty.kind else {
        return None;
    };
    // The last segment is the `struct_info` key. A generic instantiation
    // (`Vec[T]`) correctly fails that lookup later and is reported.
    path.segments.last().map(|name| Type::Named {
        name: name.clone(),
        args: Vec::new(),
    })
}

/// Every type name carrying a user `impl` block. Consulted only by the
/// `len`/`is_empty` carve-out, to keep a user-authored body from reaching a
/// frozen handle through a builtin container name.
fn collect_user_impl_types(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &program.items {
        if let Item::ImplBlock(b) = item {
            if let TypeKind::Path(path) = &b.target_type.kind {
                if let Some(name) = path.segments.last() {
                    out.insert(name.clone());
                }
            }
        }
    }
    out
}

/// Every top-level function's per-position `is_frozen` flags, keyed by name.
///
/// Free functions only: impl methods and trait methods are deliberately absent,
/// so a frozen argument in a method call falls through to
/// [`Reason::NonFrozenArgument`]. Resolving a method call to its declaration
/// needs the typechecker's callee-type map, which this pass does not wire —
/// leaving it out over-reports rather than admitting an unchecked callee.
fn collect_fn_frozen_params(program: &Program) -> HashMap<&str, Vec<bool>> {
    let mut map: HashMap<&str, Vec<bool>> = HashMap::new();
    for item in &program.items {
        if let Item::Function(f) = item {
            map.insert(
                f.name.as_str(),
                f.params.iter().map(|p| p.is_frozen).collect(),
            );
        }
    }
    map
}

fn binding_names_of(p: &Pattern) -> Vec<&str> {
    match &p.kind {
        PatternKind::Binding(name) => vec![name.as_str()],
        // Any destructuring parameter pattern spreads the handle across several
        // bindings whose individual modes are not defined. Returning
        // nothing here means the parameter contributes no tracked name, and the
        // `frozen.is_empty()` guard above then skips the function entirely —
        // which would be a HOLE, so the caller must keep at least one plain
        // binding for the check to apply. Stage 1 only accepts `frozen` on a
        // parameter, and a destructured `frozen` parameter is rejected by the
        // walk below the moment any of its bindings is used, because those
        // names are not in `frozen` and are ordinary values. That is sound:
        // destructuring a handle already materializes its parts.
        _ => Vec::new(),
    }
}

// ── Walks ───────────────────────────────────────────────────────
//
// Exhaustive, no `_` arms. A frozen PLACE is judged at the outermost place
// expression (see [`walk_expr`]'s prologue), and every position that merely
// recurses is covered automatically — including positions added to the AST
// after this was written, which will fail to compile here until they are
// handled explicitly.

fn walk_block<'a>(b: &'a Block, cx: &mut Cx<'a, '_>) {
    for s in &b.stmts {
        walk_stmt(s, cx);
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, cx);
    }
}

fn walk_stmt<'a>(s: &'a Stmt, cx: &mut Cx<'a, '_>) {
    match &s.kind {
        StmtKind::Let {
            is_mut,
            pattern,
            value,
            ..
        } => {
            // Stage 2.5: a `let` whose initializer is a frozen place naming a
            // `shared` handle becomes a non-counting ALIAS rather than an
            // escape. `try_admit_frozen_alias` consumes the initializer when it
            // takes it; otherwise the ordinary walk judges it as before.
            if !try_admit_frozen_alias(*is_mut, pattern, value, cx) {
                walk_expr(value, cx);
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, cx);
            walk_block(else_block, cx);
        }
        StmtKind::Defer { body } => walk_block(body, cx),
        StmtKind::ErrDefer { body, .. } => walk_block(body, cx),
        StmtKind::Assign { target, value } => {
            walk_expr(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                walk_expr(t, cx);
            }
            for v in values {
                walk_expr(v, cx);
            }
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            walk_expr(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::Expr(e) => walk_expr(e, cx),
    }
}

/// Stage 2.5 — `let k = <frozen place>;`, where the place names a `shared`
/// handle. Returns `true` when the binding was admitted as a NON-COUNTING
/// ALIAS, which has two consequences: the initializer's span goes to codegen
/// as "skip the clone, the inc, and the dec", and `k` joins the frozen set, so
/// every later use of it is judged by exactly the rules that govern the
/// parameter it aliases.
///
/// Every rejection here is a fall-through to the ordinary walk, which reports
/// the initializer as it always did. So a shape this function declines to
/// admit is refused, never silently allowed.
fn try_admit_frozen_alias<'a>(
    is_mut: bool,
    pattern: &'a Pattern,
    value: &'a Expr,
    cx: &mut Cx<'a, '_>,
) -> bool {
    // Inside a closure the handle has already left, whatever is done with it.
    if cx.in_closure {
        return false;
    }
    // `let mut k = …` could be repointed at a handle from anywhere. The
    // assignment itself would be reported by the ordinary walk (a `mut` alias
    // in target position is a handle, not a scalar read), but refusing the
    // DECLARATION makes the alias immutable by construction instead of by a
    // second argument that a later change to the assignment walk could break.
    if is_mut {
        return false;
    }
    // One name only. A destructuring pattern spreads the handle across
    // bindings whose modes are not defined — the same reason
    // `binding_names_of` takes only `Binding`.
    let PatternKind::Binding(name) = &pattern.kind else {
        return false;
    };
    if cx.frozen_place_root(value).is_none() {
        return false;
    }
    // A scalar place is a register copy and the local it makes is an `i64`,
    // not a handle: already permitted, already 0/0. Leaving it out keeps the
    // alias set to bindings that actually alias.
    if cx.reads_as_scalar(value) {
        return false;
    }
    let Some(ty) = cx.shared_handle_type(value) else {
        return false;
    };
    // The projection chain is consumed by the admission; any index OPERAND
    // inside it is an ordinary expression and still has to be walked, exactly
    // as in the permitted argument position (`let k = n.kids[g(x)];`).
    walk_place_indices(value, cx);
    cx.alias_spans.insert(SpanKey::from_span(&value.span));
    cx.frozen.insert(name.as_str(), ty);
    cx.aliases.insert(name.as_str());
    true
}

/// Stage 2.6 — `for k in <frozen place>;` over a container reached through a
/// frozen root. Returns `true` when the loop was admitted, which consumes the
/// ITERABLE (the body is walked by the caller either way).
///
/// Two element cases, and they differ in what the loop variable becomes:
///
/// * a `shared struct` element — `k` joins the frozen set, exactly as a `let`
///   alias does, so every use of it inside the body is judged by the rules
///   that govern the parameter;
/// * a SCALAR element (`Vec[i64]`) — `k` is a register copy of a value, not a
///   handle, so it is left as an ordinary binding. Nothing can escape through
///   it because there is nothing to escape.
///
/// Anything else — a non-container place, a `Map`/`Set` (which
/// [`element_type`] deliberately does not model), a compound non-shared
/// element, a destructuring pattern — falls through to the ordinary walk and
/// is reported.
fn try_admit_frozen_for_element<'a>(
    pattern: &'a Pattern,
    iterable: &'a Expr,
    cx: &mut Cx<'a, '_>,
) -> bool {
    if cx.in_closure {
        return false;
    }
    let PatternKind::Binding(name) = &pattern.kind else {
        return false;
    };
    if cx.frozen_place_root(iterable).is_none() {
        return false;
    }
    let Some(elem) = cx.place_type(iterable).as_ref().and_then(element_type) else {
        return false;
    };
    // Classify BEFORE consuming anything: a rejection here falls through to
    // the caller's ordinary walk, and walking the index operands twice would
    // report them twice.
    let handle = cx.as_shared_handle(elem.clone());
    if handle.is_none() && !super::is_copy_type_basic(&elem) {
        // Neither a `shared` handle nor a scalar — a `Vec[Vec[i64]]` element,
        // say, whose binding codegen registers as a buffer owner. Not modelled
        // here, so it must not be admitted; report the iterable as before.
        return false;
    }
    walk_place_indices(iterable, cx);
    if let Some(handle) = handle {
        cx.frozen.insert(name.as_str(), handle);
        cx.aliases.insert(name.as_str());
    }
    true
}

/// Walk the sub-expressions a frozen place contains that are NOT part of the
/// place itself — the index operands. `f(n.kids[g(x)])` still has to check
/// `g(x)`; only the projection chain is consumed by the permitting rule.
fn walk_place_indices<'a>(e: &'a Expr, cx: &mut Cx<'a, '_>) {
    match &e.kind {
        ExprKind::Identifier(_) => {}
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            walk_place_indices(object, cx)
        }
        ExprKind::Index { object, index } => {
            walk_place_indices(object, cx);
            walk_expr(index, cx);
        }
        // Not a place at all — reachable only if a caller asks about an
        // expression `frozen_place_root` rejected, which never happens.
        _ => walk_expr(e, cx),
    }
}

/// Walk a call's arguments, permitting a frozen place only when it reads as a
/// scalar (safe in any slot) or when the slot's declared parameter is itself
/// `frozen` (the guarantee composes across the call).
fn walk_call_args<'a>(callee: Option<&str>, args: &'a [CallArg], cx: &mut Cx<'a, '_>) {
    let sig = callee.and_then(|name| cx.fn_frozen_params.get(name).cloned());
    for (i, a) in args.iter().enumerate() {
        let Some(name) = cx.frozen_place_root(&a.value) else {
            walk_expr(&a.value, cx);
            continue;
        };
        if cx.in_closure {
            cx.flag(name, &a.value.span, Reason::Captured);
            walk_place_indices(&a.value, cx);
            continue;
        }
        // A scalar read is a register copy: no handle exists to escape, so the
        // slot's mode is irrelevant.
        if cx.reads_as_scalar(&a.value) {
            walk_place_indices(&a.value, cx);
            continue;
        }
        let resolved = cx.place_type(&a.value).is_some();
        // A LABELLED argument is not matched positionally, and this pass does
        // not reorder against the declaration — so it is not resolved, and
        // falls through to the report. Conservative, not a hole.
        let permitted = resolved
            && a.label.is_none()
            && !a.mut_marker
            && sig
                .as_ref()
                .is_some_and(|s| s.get(i).copied() == Some(true));
        if !permitted {
            // An UNRESOLVABLE place is reported for what it is (a projection
            // this pass could not follow) rather than as a slot-mode problem,
            // which would send the reader to fix the wrong declaration.
            let reason = if resolved {
                Reason::NonFrozenArgument
            } else {
                place_reason(&a.value)
            };
            cx.flag(name, &a.value.span, reason);
        }
        walk_place_indices(&a.value, cx);
    }
}

fn walk_expr<'a>(e: &'a Expr, cx: &mut Cx<'a, '_>) {
    // ── Frozen places are judged here, before the shape dispatch ──
    //
    // A place rooted at a frozen parameter — the bare name or any chain of
    // `.field` / `[i]` / `.0` off it — is consumed by this prologue and NOT
    // recursed into, which is what makes the permitted forms permitted. In
    // VALUE position the only permitted form is a read that resolves to a
    // scalar; the two handle-carrying positions (a `frozen` argument slot, a
    // builtin `len`/`is_empty` receiver) intercept before reaching here.
    if let Some(root) = cx.frozen_place_root(e) {
        if cx.in_closure {
            cx.flag(root, &e.span, Reason::Captured);
        } else if !cx.reads_as_scalar(e) {
            let reason = place_reason(e);
            cx.flag(root, &e.span, reason);
        }
        walk_place_indices(e, cx);
        return;
    }

    match &e.kind {
        // Reached only for a NON-frozen identifier — a frozen one is consumed
        // by the prologue above.
        ExprKind::Identifier(_) => {}

        ExprKind::FieldAccess { object, .. } => walk_expr(object, cx),

        // ── Permitted position: pass-through to a frozen slot ───
        ExprKind::Call { callee, args } => {
            let name = match &callee.kind {
                ExprKind::Identifier(n) => Some(n.as_str()),
                _ => None,
            };
            walk_expr(callee, cx);
            walk_call_args(name, args, cx);
        }

        // ── Everything else: recurse; frozen uses report at the leaf ──
        ExprKind::Integer(..)
        | ExprKind::Float(..)
        | ExprKind::CharLit(_)
        | ExprKind::ByteLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Path { .. }
        | ExprKind::SelfValue
        | ExprKind::SelfType
        | ExprKind::PipePlaceholder
        | ExprKind::Continue { .. }
        | ExprKind::OffsetOf { .. }
        | ExprKind::Error => {}

        ExprKind::InterpolatedStringLit(parts) => {
            for p in parts {
                match p {
                    ParsedInterpolationPart::Text(_) => {}
                    ParsedInterpolationPart::Expr(inner, _) => walk_expr(inner, cx),
                }
            }
        }
        ExprKind::Binary { left, right, .. }
        | ExprKind::NilCoalesce { left, right }
        | ExprKind::Pipe { left, right } => {
            walk_expr(left, cx);
            walk_expr(right, cx);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, cx),
        ExprKind::Question(inner) => walk_expr(inner, cx),
        ExprKind::OptionalChain { object, args, .. } => {
            walk_expr(object, cx);
            if let Some(args) = args {
                walk_call_args(None, args, cx);
            }
        }
        // ── Permitted position: a read-only builtin container query ──
        //
        // `n.kids.len()`. Narrow on three axes at once — see the module header
        // — so that no user body can receive the handle. Anything else falls
        // through to `walk_expr`, where the receiver is judged as an ordinary
        // frozen place and reported.
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            if !cx.in_closure
                && cx.frozen_place_root(object).is_some()
                && cx.permits_builtin_query(object, method)
            {
                walk_place_indices(object, cx);
            } else {
                walk_expr(object, cx);
            }
            walk_call_args(None, args, cx);
        }
        ExprKind::TupleIndex { object, .. } => walk_expr(object, cx),
        ExprKind::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
        ExprKind::Block(b)
        | ExprKind::Comptime(b)
        | ExprKind::Unsafe(b)
        | ExprKind::Try(b)
        | ExprKind::Seq(b)
        | ExprKind::Par(b) => walk_block(b, cx),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            walk_expr(condition, cx);
            walk_block(then_block, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, cx);
            }
        }
        ExprKind::IfLet {
            value,
            then_block,
            else_branch,
            ..
        } => {
            walk_expr(value, cx);
            walk_block(then_block, cx);
            if let Some(eb) = else_branch {
                walk_expr(eb, cx);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, cx);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, cx);
                }
                walk_expr(&arm.body, cx);
            }
        }
        ExprKind::While {
            condition, body, ..
        } => {
            walk_expr(condition, cx);
            walk_block(body, cx);
        }
        ExprKind::WhileLet { value, body, .. } => {
            walk_expr(value, cx);
            walk_block(body, cx);
        }
        // ── Permitted position: iterating a container off a frozen root ──
        ExprKind::For {
            pattern,
            iterable,
            body,
            ..
        } => {
            if !try_admit_frozen_for_element(pattern, iterable, cx) {
                walk_expr(iterable, cx);
            }
            walk_block(body, cx);
        }
        ExprKind::Loop { body, .. } => walk_block(body, cx),
        ExprKind::LabeledBlock { body, .. } => walk_block(body, cx),
        ExprKind::Closure { body, .. } => {
            let saved = std::mem::replace(&mut cx.in_closure, true);
            walk_expr(body, cx);
            cx.in_closure = saved;
        }
        ExprKind::Return(v) => {
            if let Some(v) = v {
                walk_expr(v, cx);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                walk_expr(v, cx);
            }
        }
        ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
            for i in items {
                walk_expr(i, cx);
            }
        }
        ExprKind::PrefixCollectionLiteral { items, .. } => {
            for i in items {
                walk_expr(i, cx);
            }
        }
        ExprKind::RepeatLiteral { value, count, .. } => {
            walk_expr(value, cx);
            walk_expr(count, cx);
        }
        ExprKind::MapLiteral(entries) => {
            for (k, v) in entries {
                walk_expr(k, cx);
                walk_expr(v, cx);
            }
        }
        ExprKind::StructLiteral { fields, spread, .. } => {
            for f in fields {
                walk_expr(&f.value, cx);
            }
            if let Some(s) = spread {
                walk_expr(s, cx);
            }
        }
        ExprKind::Cast { expr, .. } => walk_expr(expr, cx),
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, cx);
            }
            if let Some(en) = end {
                walk_expr(en, cx);
            }
        }
        ExprKind::Lock { mutex, body, .. } => {
            walk_expr(mutex, cx);
            walk_block(body, cx);
        }
        ExprKind::Providers { bindings, body } => {
            for b in bindings {
                walk_expr(&b.value, cx);
            }
            walk_block(body, cx);
        }
    }
}
