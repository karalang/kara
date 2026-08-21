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
//! 6. **A method whose receiver is declared `frozen self`** — `n.kids[i].m()`
//!    (stage 2.7). Resolved by the receiver PLACE's type, which this pass
//!    already computes, so `(type, method)` names exactly one declaration and
//!    no typechecker callee map is needed. Its parameters compose too, on the
//!    same per-position flags a free function's do.
//!
//!    **`frozen self` is the whole reason this is sound.** A `ref self`
//!    method emits the same zero traffic — measured 0/0 in the caller's
//!    frame, the callee's frame, and a nested `ref self` call — so the
//!    emitted code cannot tell the two apart. What differs is that a `frozen
//!    self` method's BODY is checked by this pass, and a `ref self` method's
//!    is not: it may store or return `self`, and doing that with a
//!    non-counting handle is a use-after-free. So the admission keys on the
//!    declared mode, never on the emitted traffic.
//!
//!    `self` is a frozen root under the name `"self"`, and note that it is a
//!    DISTINCT `ExprKind` rather than an `Identifier` whose name happens to
//!    be "self". Missing that arm is not a false positive but a HOLE — the
//!    first draft of stage 2.7 had it, and `frozen self` parsed, checked
//!    clean, and protected nothing, with every accept-row passing vacuously.
//!    Pinned by `frozen_self_receiver_is_checked_like_a_frozen_parameter`'s
//!    closure-capture row (tests/ownership.rs), which is red without it.
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
//! - **A receiver cannot be passed to a borrow parameter at all.** `frz(self)`
//!   from inside a `frozen self` method is a TYPE error — but so is
//!   `byref(self)` from a plain `ref self` method, and from an owned `self`
//!   one. The typechecker types `self` as `T` regardless of receiver mode,
//!   while a `ref T` / `frozen T` parameter wants `ref T`. That predates all
//!   of this and is filed as B-2026-08-07-8; the OO path (calling another `frozen
//!   self` method) is unaffected and is what stage 2.7 is for.
//! - ~~**Only free-function calls compose.**~~ Superseded by stage 2.7 for
//!   IMPL methods with a `frozen self` receiver. Still true of TRAIT methods
//!   and of any method whose receiver place this pass cannot type — a
//!   trait-dispatched call has no single declaration to check, so leaving it
//!   out is accurate as well as fail-closed. A `frozen` argument in a *method*
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

use rustc_hash::FxHashSet;
use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::{types::Type, TypeCheckResult};

use super::frozen_freeze_site::FreezeSite;
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
    /// Stage 3b step 3 — the place is the TARGET of a write. Reported
    /// separately from every read-shaped refusal because the fix is different
    /// in kind: a read through a frozen place has a permitted spelling to move
    /// to, a write has none — the region is immutable by definition, so the
    /// only repair is to move the write outside it or not freeze.
    Written,
    /// Stage 3 — a `freeze` this pass will not honour, carrying the specific
    /// cause. REPORTED rather than declined: a `freeze` that silently does
    /// nothing is worse than one that is refused, because the author is left
    /// believing a guarantee they do not have.
    FreezeRefused(&'static str),
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
    is_source: bool,
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
    /// `(impl target type, method name)` → per-position `is_frozen` flags, for
    /// every impl method declaring a `frozen self` receiver (stage 2.7). A
    /// method call on a frozen place is admitted only when its declaration is
    /// in here, which is what makes the callee's body checked rather than
    /// assumed — the same composition rule `fn_frozen_params` gives free
    /// functions. Keyed by the type this pass resolves the RECEIVER PLACE to,
    /// never by method name alone, so two types with a same-named method
    /// cannot borrow each other's guarantee.
    frozen_methods: HashMap<(&'a str, &'a str), Vec<bool>>,
    /// `freeze <place>` initializer spans, from `Program::freeze_spans`
    /// (stage 3). A `let` whose initializer is in here is a FREEZE SITE: it
    /// introduces a frozen root from an ordinary binding, rather than deriving
    /// one from a root that was already frozen.
    freeze_spans: &'a FxHashSet<SpanKey>,
    /// Every `freeze` site this walk visited, with the type name it resolved
    /// the source to (`None` when it could not) and the span to report at.
    /// Handed to the freeze-site classifier by the caller so that ADMISSION
    /// and FREEZABILITY are decided from one enumeration of the sites — a
    /// second walk looking for freeze statements could miss a nesting form
    /// this one handles, and a site admitted but never classified is exactly
    /// the hole the two-guard design exists to prevent.
    freeze_sites: Vec<(Option<String>, Span, FreezeSite)>,
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
    /// Names in `frozen` that are the SOURCE of a stage-3 `freeze` — the
    /// binding that still owns the value. Diagnostics only, and separate from
    /// `aliases` because the line to change is different again: an alias is
    /// fixed at its `let`, a source at the `freeze` that restricted it.
    sources: HashSet<&'a str>,
    found: Vec<Rejection>,
    /// Stage 2.5 — the initializer span of every `let` admitted as a
    /// NON-COUNTING ALIAS of a frozen place. Handed to codegen (through
    /// `OwnershipCheckResult::frozen_alias_bindings`) as the instruction to
    /// skip the element clone, the receive-inc, and the scope-exit dec for
    /// that binding. Keyed by the initializer rather than the statement
    /// because that is the span codegen already has in hand at the decision
    /// point, and the one `vec_index_borrow_spans` — the existing
    /// alias-instead-of-own channel — is keyed by.
    alias_spans: FxHashSet<SpanKey>,
    /// Stage 3b step 1 — every local name USED so far, in walk order.
    ///
    /// This is the alias/uniqueness precondition the design calls "local and
    /// cheap" and which, until now, had NO enforcement at all: `let a = t; let
    /// g = freeze t;` checked clean, and so did `take(t)` before the freeze.
    /// That was harmless only because E0512 refused every type where a second
    /// handle could WRITE — so relaxing E0512 (the same slice as this) is
    /// exactly what makes it start mattering.
    ///
    /// The walk is ordered, so at a freeze site this set holds precisely the
    /// uses that PRECEDE it. A root absent from it has been named nowhere but
    /// its own `let`, which is what "no other live binding of this instance"
    /// reduces to for the build-then-freeze shape the design motivates.
    ///
    /// Deliberately coarse: ANY earlier use disqualifies, including a scalar
    /// read that could not possibly produce a handle. Fail-closed is the whole
    /// point here — the cost of a false NEGATIVE is a refused freeze the author
    /// can work around by moving the read, and the cost of a false POSITIVE is
    /// a data race on a non-atomic refcount, which this repo has already seen
    /// go arch-specific and green on x86 CI (B-2026-07-12-29).
    used_names: HashSet<&'a str>,
    /// Locals whose `let` initializer was a PLACE — i.e. the binding is itself
    /// an alias of some other live name (`let t = a;`), the uniqueness failure
    /// pointing the other way from `used_names`. Both directions must be
    /// closed or the check proves nothing: one catches the alias made from the
    /// source, the other the source made from an alias.
    place_bound: HashSet<&'a str>,

    // ── Stage 3c: frozen-element CONTAINERS (B-2026-08-07-23) ───────
    /// Containers ASSUMED to hold frozen elements for this round, mapped to
    /// the container's own type (`Vec[Node]`), from which [`element_type`]
    /// answers what `c[i]` is. See [`collect_frozen_escapes`] for why the
    /// fixpoint runs downward from "all candidates" rather than upward from
    /// none.
    ///
    /// A container in here is NOT itself a frozen place — `c` is an ordinary
    /// mutable local and `c.push(..)` / `c.len()` are ordinary calls. Only
    /// `c[i]` is frozen, because only the ELEMENTS are non-counting aliases.
    frozen_containers: HashMap<&'a str, Type>,
    /// Every `let` this walk saw whose annotation names `Vec[S]` for a
    /// non-generic `shared struct` S — the candidate set the fixpoint starts
    /// from, discovered by the walk itself rather than by a second pre-pass.
    container_candidates: HashMap<&'a str, Type>,
    /// Candidates disqualified during this round, for any of the three
    /// reasons: an unwhitelisted use, a non-frozen (MIXED) push, or a push
    /// whose element owner does not outlive the container.
    container_rejects: HashSet<&'a str>,
    /// Names bound by a `let` more than once in this function. Shadowing would
    /// make this name-keyed analysis conflate two different containers, so a
    /// rebound name is never a container.
    container_rebound: HashSet<&'a str>,
    /// The `let` initializer span of each candidate — the codegen key, chosen
    /// to match `frozen_alias_bindings` (the span codegen already holds at the
    /// decision point).
    container_spans: HashMap<&'a str, Span>,
    /// Block path of each candidate's `let`, for the lifetime test below.
    container_path: HashMap<&'a str, Vec<usize>>,
    /// Block path of the OWNER whose refcount each frozen name is skipping. A
    /// `frozen` parameter / `frozen self` is owned by the CALLER, so its path
    /// is empty — a prefix of every path, i.e. it outlives everything. A
    /// `freeze` statement's source is a local, so it is owned by the block it
    /// was bound in, and a derived alias inherits its root's owner.
    ///
    /// This is what makes a container store safe: a container may only hold a
    /// non-counting alias whose owner's block ENCLOSES the container's, or the
    /// owner dies first and every element dangles.
    frozen_owner_path: HashMap<&'a str, Vec<usize>>,
    /// Path of the block being walked, as a chain of unique block ids. Prefix
    /// containment on this is exactly "encloses".
    block_path: Vec<usize>,
    next_block_id: usize,
    /// Block path of the innermost enclosing closure body, `defer` /
    /// `errdefer` body, or `par` / `seq` block — the regions a container
    /// declared OUTSIDE may not be touched from.
    ///
    /// Positional rather than a bare flag, and that distinction is the whole
    /// feature rather than a refinement of it: a worklist declared INSIDE a
    /// `par` branch is branch-local and is exactly the shape `frozen` exists
    /// to admit, while one declared outside and pushed to from two branches is
    /// two threads writing one `{ptr,len,cap}`. A flag cannot tell those
    /// apart and refusing both would leave the mechanism with nothing to do.
    ///
    /// Only the innermost region is tracked, which loses nothing: a container
    /// declared inside it is inside every enclosing one too.
    restricted_region: Option<Vec<usize>>,
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
            // `self` is a place root too, and it is a DISTINCT `ExprKind` —
            // not an `Identifier` whose name happens to be "self". Missing
            // this arm is not a false positive, it is a HOLE: the prologue
            // would never judge a receiver at all, so `frozen self` would
            // parse, check clean, and protect nothing. Found by probing a
            // closure capture of `self`, which passed when it must report.
            ExprKind::SelfValue => self.frozen.get_key_value("self").map(|(k, _)| *k),
            // Stage 3c — an ELEMENT of a proved frozen-element container is a
            // frozen place rooted at the container's name. The container
            // itself is NOT (the `Identifier` arm above does not consult
            // `frozen_containers`), which is the whole distinction: `c` is an
            // ordinary mutable local that may be pushed to and measured, and
            // only what comes OUT of it is non-counting.
            ExprKind::Index { object, .. }
                if matches!(&object.kind, ExprKind::Identifier(n)
                    if self.frozen_containers.contains_key(n.as_str())) =>
            {
                let ExprKind::Identifier(n) = &object.kind else {
                    unreachable!("guarded above")
                };
                self.frozen_containers
                    .get_key_value(n.as_str())
                    .map(|(k, _)| *k)
            }
            ExprKind::FieldAccess { object, .. }
            | ExprKind::Index { object, .. }
            | ExprKind::TupleIndex { object, .. } => self.frozen_place_root(object),
            _ => None,
        }
    }

    /// The container a `c.push(x)` / `c.len()` receiver names, when `c` is a
    /// candidate frozen-element container. Returns the interned name so the
    /// caller can record against it.
    fn container_receiver(&self, object: &Expr) -> Option<&'a str> {
        let ExprKind::Identifier(n) = &object.kind else {
            return None;
        };
        self.container_candidates
            .get_key_value(n.as_str())
            .map(|(k, _)| *k)
    }

    /// Whether the owner of frozen place root `root` outlives container `c`.
    ///
    /// Both are block paths, so "outlives" is exactly "the owner's block
    /// encloses the container's" — prefix containment. A `frozen` parameter's
    /// owner path is empty and therefore encloses every block, which is why the
    /// parameter-rooted shape needs no analysis at all.
    ///
    /// Fail-closed: an owner this pass did not record is treated as born at the
    /// deepest point it could be, so the store is refused.
    fn owner_outlives_container(&self, root: &str, c: &str) -> bool {
        let (Some(owner), Some(cont)) =
            (self.frozen_owner_path.get(root), self.container_path.get(c))
        else {
            return false;
        };
        owner.len() <= cont.len() && cont[..owner.len()] == owner[..]
    }

    /// Whether container `c` may be touched at the point being walked: either
    /// there is no restricted region in scope, or `c` was declared inside it.
    ///
    /// Fail-closed: a container whose declaring block this pass did not record
    /// (it has not been walked yet, which a well-formed program cannot do) is
    /// not visible.
    fn container_visible_here(&self, c: &str) -> bool {
        let Some(region) = &self.restricted_region else {
            return true;
        };
        let Some(path) = self.container_path.get(c) else {
            return false;
        };
        path.len() >= region.len() && path[..region.len()] == region[..]
    }

    /// Disqualify a candidate container. Idempotent, and deliberately the only
    /// way a candidate leaves the set — the fixpoint below runs DOWNWARD, so
    /// every rejection is monotone and the walk can never re-admit one.
    fn reject_container(&mut self, c: &'a str) {
        self.container_rejects.insert(c);
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
            ExprKind::SelfValue => self.frozen.get("self").cloned(),
            ExprKind::FieldAccess { object, field } => {
                let base = self.place_type(object)?;
                let info = self.tc.struct_info.get(head_type_name(&base)?)?;
                // B-2026-08-01-33 stage 3b step 3 — a `mut` field RESOLVES here
                // now. It used to return `None`, which refused it in read and
                // write position alike; that is what § "Two guards" meant by
                // "needs no write-position tracking", and it is what blocked
                // #133 (`for k in root.neighbors`).
                //
                // Resolving is not admitting. Position is decided by the
                // CALLER, and the permitted set is unchanged: a read that
                // resolves to a scalar, a `len`/`is_empty` builtin query, or a
                // whole handle into a `frozen` slot. Everything else — every
                // mutating method among them, since none is `len`/`is_empty` —
                // still falls through to the prologue and reports. The one
                // shape this widening would newly admit is a SCALAR `mut` field
                // in ASSIGN position (`g.count = 5` reads as a scalar and would
                // have been permitted), and that is closed at the three
                // assignment statement forms, which are matched exhaustively
                // with no wildcard arm.
                info.fields
                    .iter()
                    .find(|(fname, _, _)| fname == field)
                    .map(|(_, fty, _)| fty.clone())
            }
            // Stage 3c — an element of a proved container. Its type comes from
            // the container's `let` ANNOTATION, recorded when the candidate was
            // discovered, never from the method-call receiver's recorded type:
            // that lookup returns `Unit` at a `push` site, which is what made
            // the first attempt at this decline every element read.
            ExprKind::Index { object, .. }
                if matches!(&object.kind, ExprKind::Identifier(n)
                    if self.frozen_containers.contains_key(n.as_str())) =>
            {
                let ExprKind::Identifier(n) = &object.kind else {
                    unreachable!("guarded above")
                };
                element_type(self.frozen_containers.get(n.as_str())?)
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

    /// The place a `for` loop iterates, seeing through a bare `.iter()`.
    ///
    /// WHY THE ADAPTER HAS TO BE HERE. Stage 2.6 admitted `for k in n.kids` and
    /// nothing else, which reads as a spelling detail and is not one: across
    /// kara-katas' leetcode corpus `for x in <place>.iter()` is written 22
    /// times and the bare-place form over a field ZERO times. Refusing the
    /// adapter therefore refuses the idiom, and a kata rewritten to dodge it
    /// has stopped doing its job (kara-katas CLAUDE.md § "Katas are
    /// bug-finders").
    ///
    /// It is admitted on exactly the terms `len` / `is_empty` already are, and
    /// on the same three independent axes: the method name is one exact name;
    /// the receiver must resolve to a BUILTIN container, so no user body ever
    /// receives the handle; and a type carrying a user `impl` block is
    /// excluded, so `impl Vec { fn iter(...) }` cannot smuggle a body in
    /// through the builtin name. `.iter()` on a builtin container yields the
    /// same elements the place form does and lowers the same way — stage 2.6
    /// measured that loop as a pointer load and a store, with no clone, retain
    /// or release — so nothing about an element's counting changes.
    ///
    /// The two forms are not merely similar, they are the same lowering:
    /// `codegen/control_flow_for.rs` peels exactly this call off a `for`
    /// iterable before compiling the loop, so admitting it here changes what is
    /// CHECKED and nothing about what is emitted.
    ///
    /// Only a BARE `iter` is peeled — NOT `into_iter`, which codegen's peel does
    /// accept. The names differ in intent (`into_iter` reads as consuming the
    /// receiver) and this pass has measured neither that nor what a future
    /// divergence would mean for a non-counting element, so the fail-closed
    /// half of the pair is the one to take. Anything further along a chain
    /// (`.iter().map(..)`, `.iter().rev()`) is likewise a different expression
    /// whose element provenance is unmeasured, and falls through to the
    /// ordinary walk that reports it.
    fn frozen_iterable_place<'e>(&self, e: &'e Expr) -> &'e Expr {
        let ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } = &e.kind
        else {
            return e;
        };
        if method != "iter" || !args.is_empty() {
            return e;
        }
        // `permits_builtin_query`'s receiver test, reused rather than
        // reimplemented: two opinions about what counts as a builtin container
        // is how this module's guards drift apart.
        if self.permits_builtin_query(object, "len") {
            object
        } else {
            e
        }
    }

    fn flag(&mut self, name: &str, span: &Span, reason: Reason) {
        self.found.push(Rejection {
            is_alias: self.aliases.contains(name),
            is_source: self.sources.contains(name),
            name: name.to_string(),
            span: *span,
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
    pub(crate) fn check_frozen_param_escape(&mut self, f: &Function, impl_type: Option<&str>) {
        // A function with no frozen parameter and no `frozen self` can still
        // contain a stage-3 `freeze` statement. Gating on the PROGRAM using
        // the mode at all keeps this free for every program that does not —
        // and forgetting it is not a false positive but a silent no-op: the
        // first draft of stage 3 returned here for `main`, so every freeze
        // site went unchecked and every negative probe "passed".
        if !f.params.iter().any(|p| p.is_frozen)
            && !f.self_is_frozen
            && self.program.freeze_spans.is_empty()
        {
            return;
        }

        // Computed through a free function so the immutable borrow of
        // `typecheck_result` / `program` the walk needs is finished before the
        // errors are pushed through `&mut self`.
        let (found, aliases, freeze_sites, containers) =
            collect_frozen_escapes(f, impl_type, self.program, self.typecheck_result);

        // Stage 3 — every `freeze` site this function contains, classified by
        // the same rule a `frozen` parameter's declared type is.
        self.report_freeze_sites(freeze_sites);

        // Stage 2.5 alias bindings, surfaced to codegen. Recorded even when
        // this function also produced errors — the program will not compile in
        // that case, so the set is never read, and gating on `found.is_empty()`
        // would just add a state the tests cannot reach.
        self.frozen_alias_bindings.extend(aliases);
        // Stage 3c — the container hint, recorded on the same terms: the two
        // codegen suppressions it drives are a PAIR (skip the push retain,
        // skip the element release), so they are surfaced through one set and
        // a container that fails either condition appears in neither.
        self.frozen_element_containers.extend(containers);

        for Rejection {
            name,
            span,
            reason,
            is_alias,
            is_source,
        } in found
        {
            // Three nouns, because all three name a different line to go fix:
            // a stage-2.5 local ALIAS obeys the parameter's rule but is a
            // `let`; a stage-2.7 `frozen self` RECEIVER is the signature but
            // not a parameter; everything else is a parameter.
            let noun = if is_alias {
                "alias"
            } else if is_source {
                "source"
            } else if name == "self" {
                "receiver"
            } else {
                "parameter"
            };
            // Only the `Materialized` help offers "take it by value instead",
            // which is advice about a SIGNATURE — so an alias needs the extra
            // sentence saying which signature.
            let alias_note = if name == "self" {
                " `self` here is a `frozen self` receiver, so the declaration to change is this \
                 method's — take `ref self` (or `self`) instead if the body needs to store, \
                 return, or capture the handle."
                    .to_string()
            } else if is_source {
                format!(
                    " `{name}` is the SOURCE of a `freeze` in this scope, so it is restricted \
                     from the `freeze` onward — the frozen handle is a non-counting alias of it \
                     and would dangle if `{name}` went away. Move the use above the `freeze`, or \
                     drop the `freeze` if the value has to be consumed."
                )
            } else if is_alias {
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
                Reason::Written => (
                    format!("`frozen` {noun} `{name}` cannot be written through"),
                    format!(
                        "a `freeze` claims the value is deeply immutable for the region, and \
                         every branch reading through it is relying on that — so a write \
                         through `{name}`, or through any place rooted at it, is the one thing \
                         the claim rules out. Move the write BEFORE the `freeze` (the source \
                         stays writable until then), or drop the `freeze` if the value has to \
                         change while it is shared"
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
                Reason::FreezeRefused(why) => (
                    format!("this `freeze` cannot be honoured: {why}"),
                    format!(
                        "a `freeze` binds a NON-COUNTING handle to an existing value, so it \
                         needs an immutable binding (`let {name} = freeze <place>;`) naming a \
                         place whose owner outlives it — a binding, or a field / element \
                         reached from one. A temporary has no such owner, a `mut` binding could \
                         be repointed at one that does not, and a closure's environment can \
                         outlive the call. Bind the value first if it is a temporary, and drop \
                         `mut` if it is there"
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

/// What one function's walk produces: the rejected uses, the initializer spans
/// of the bindings admitted as non-counting aliases, the freeze sites seen
/// (each with the type name it resolved to, for the freeze-site classifier),
/// and the initializer spans of the `let`s proved to be frozen-element
/// containers.
type WalkResult = (
    Vec<Rejection>,
    FxHashSet<SpanKey>,
    Vec<(Option<String>, Span, FreezeSite)>,
    FxHashSet<SpanKey>,
);

/// Run the walk over `f` and return every rejected use, plus the initializer
/// spans of the `let` bindings admitted as non-counting aliases. Free-standing
/// so the shared borrows of `program` / `tc` it holds end before the caller
/// pushes diagnostics through `&mut self`.
fn collect_frozen_escapes<'a>(
    f: &'a Function,
    impl_type: Option<&str>,
    program: &'a Program,
    tc: &TypeCheckResult,
) -> WalkResult {
    let mut frozen: HashMap<&str, Type> = HashMap::new();
    for p in f.params.iter().filter(|p| p.is_frozen) {
        let Some(root) = frozen_root_type(&p.ty) else {
            continue;
        };
        for name in binding_names_of(&p.pattern) {
            frozen.insert(name, root.clone());
        }
    }
    // A `frozen self` receiver (stage 2.7) is a frozen root under the name
    // `self`, typed by the impl target. From here on it is indistinguishable
    // from a `frozen` parameter — same whitelist, same escape rule — which is
    // the point: the receiver's guarantee has to be the parameter's guarantee
    // or a method call cannot compose with a free-function call.
    if f.self_is_frozen {
        if let Some(t) = impl_type {
            frozen.insert(
                "self",
                Type::Named {
                    name: t.to_string(),
                    args: Vec::new(),
                },
            );
        }
    }
    // A function with no frozen parameter and no `frozen self` can still
    // contain a `freeze` statement, which seeds a root mid-body. Proceeding on
    // "the PROGRAM uses `freeze` at all" keeps the early-out free for every
    // program that does not — which is every program that does not use the
    // mode — without a per-function body scan.
    if frozen.is_empty() && program.freeze_spans.is_empty() {
        return (
            Vec::new(),
            FxHashSet::default(),
            Vec::new(),
            FxHashSet::default(),
        );
    }

    // Stage 3c — the frozen-element container fixpoint (B-2026-08-07-23).
    //
    // WHY IT IS A FIXPOINT AT ALL. `work[i]` is walked BEFORE the
    // `work.push(k)` further down the loop body, while at runtime the
    // container already holds what earlier iterations pushed. One ordered pass
    // therefore cannot decide the question: whether `work[i]` yields a frozen
    // element depends on facts the same pass has not reached yet.
    //
    // WHY IT RUNS DOWNWARD. Starting from "nothing is a container" and adding
    // never converges anywhere useful — `work` is a container only if `work[i]`
    // is frozen, and `work[i]` is frozen only if `work` is a container, so the
    // LEAST fixpoint of that is the empty set and the shape stays refused. So
    // the iteration starts from "every candidate is a container" and REMOVES
    // the ones the walk disproves, which is the greatest fixpoint.
    //
    // WHY THE GREATEST FIXPOINT IS SOUND, since a coinductive rule that
    // justifies itself is exactly what usually is not. The worry is a cycle
    // like `c.push(c[0])`, which survives on its own assumption. It cannot
    // introduce an unfrozen element: a container starts EMPTY, so the first
    // push into it can never be self-sourced, and by induction on push order
    // every element traces back to a genuine seed (a `frozen` parameter or a
    // `freeze` statement). A purely self-referential container stays empty
    // forever and there is nothing to be wrong about.
    //
    // EACH ROUND REBUILDS `frozen` FROM THE INITIAL SET, which is not
    // incidental: carrying it across rounds means a name frozen LATE in one
    // round is already frozen at the TOP of the next, so statements ABOVE a
    // `freeze` get judged against a guarantee that does not exist yet. The
    // first draft did that and refused a write that legitimately preceded its
    // `freeze`.
    //
    // ROUND 0 IS DISCOVERY ONLY. It finds the candidate `let`s; its rejections
    // are DISCARDED, because with nothing assumed frozen every element read
    // looks like an ordinary use and would disqualify every container in the
    // program.
    let mk = |assumed: HashMap<&'a str, Type>, candidates: HashMap<&'a str, Type>| Cx {
        tc,
        frozen: frozen.clone(),
        fn_frozen_params: collect_fn_frozen_params(program),
        frozen_methods: collect_frozen_methods(program),
        freeze_spans: &program.freeze_spans,
        freeze_sites: Vec::new(),
        used_names: HashSet::new(),
        place_bound: HashSet::new(),
        user_impl_types: collect_user_impl_types(program),
        in_closure: false,
        aliases: HashSet::new(),
        sources: HashSet::new(),
        found: Vec::new(),
        alias_spans: FxHashSet::default(),
        // A `frozen` parameter / `frozen self` is owned by the CALLER, whose
        // value lives for the whole call — the empty path, a prefix of every
        // block, i.e. it outlives anything a local container can be.
        frozen_owner_path: frozen.keys().map(|k| (*k, Vec::new())).collect(),
        frozen_containers: assumed,
        container_candidates: candidates,
        container_rejects: HashSet::new(),
        container_rebound: HashSet::new(),
        container_spans: HashMap::new(),
        container_path: HashMap::new(),
        block_path: Vec::new(),
        next_block_id: 0,
        restricted_region: None,
    };

    let mut cx = mk(HashMap::new(), HashMap::new());
    walk_block(&f.body, &mut cx);
    let mut assumed = cx.container_candidates.clone();
    for n in cx.container_rebound {
        assumed.remove(n);
    }
    // Bounded by the candidate count: every round but the last removes at
    // least one, and the `+ 2` covers the discovery round and the confirming
    // round that changes nothing.
    let rounds = assumed.len() + 2;
    for _ in 0..rounds {
        let candidates = assumed.clone();
        cx = mk(assumed.clone(), candidates);
        walk_block(&f.body, &mut cx);
        let mut next = assumed.clone();
        for n in &cx.container_rejects {
            next.remove(*n);
        }
        if next.len() == assumed.len() {
            break;
        }
        assumed = next;
    }
    let containers = assumed
        .keys()
        .filter_map(|n| cx.container_spans.get(*n))
        .map(SpanKey::from_span)
        .collect();
    (cx.found, cx.alias_spans, cx.freeze_sites, containers)
}

/// Whether a `let`'s ANNOTATION names `Vec[S]` for a non-generic `shared
/// struct` S — the only shape stage 3c models — and if so the container type
/// `element_type` will be asked about later.
///
/// Deliberately annotation-only and deliberately `Vec`-only. The annotation is
/// the one place the element type is unambiguously written down (see the
/// module header's note on why the push site cannot answer it), and `Vec` is
/// the container the measured traversal actually uses: `VecDeque` additionally
/// wants `pop_front`, whose result is a VALUE and not a place, so it needs a
/// second mechanism rather than a wider list here.
fn container_candidate_type(ty: Option<&TypeExpr>, tc: &TypeCheckResult) -> Option<Type> {
    let TypeKind::Path(p) = &ty?.kind else {
        return None;
    };
    if p.segments.last().map(String::as_str) != Some("Vec") {
        return None;
    }
    let args = p.generic_args.as_ref()?;
    let [GenericArg::Type(elem)] = args.as_slice() else {
        return None;
    };
    let TypeKind::Path(ep) = &elem.kind else {
        return None;
    };
    let name = ep.segments.last()?;
    if ep.generic_args.is_some() {
        return None;
    }
    let info = tc.struct_info.get(name.as_str())?;
    if !info.is_shared || !info.generic_params.is_empty() {
        return None;
    }
    Some(Type::Named {
        name: "Vec".to_string(),
        args: vec![Type::Named {
            name: name.clone(),
            args: Vec::new(),
        }],
    })
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

/// Every impl method declaring `frozen self`, keyed by `(target type, method
/// name)` with its per-position parameter `is_frozen` flags.
///
/// Impl blocks only. A TRAIT method cannot declare `frozen self` (stage 2.7
/// does not add the receiver form there), and a trait-dispatched call has no
/// single declaration to check anyway, so leaving them out is both accurate
/// and fail-closed.
fn collect_frozen_methods(program: &Program) -> HashMap<(&str, &str), Vec<bool>> {
    let mut map: HashMap<(&str, &str), Vec<bool>> = HashMap::new();
    for item in &program.items {
        let Item::ImplBlock(b) = item else { continue };
        let TypeKind::Path(path) = &b.target_type.kind else {
            continue;
        };
        let Some(target) = path.segments.last() else {
            continue;
        };
        for it in &b.items {
            let ImplItem::Method(m) = it else { continue };
            if !m.self_is_frozen {
                continue;
            }
            map.insert(
                (target.as_str(), m.name.as_str()),
                m.params.iter().map(|p| p.is_frozen).collect(),
            );
        }
    }
    map
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
    // Stage 3c — each block gets a unique id, so a block's PATH identifies it
    // and prefix containment on two paths is exactly "one encloses the other".
    // Ids come from a per-round counter, which makes the paths stable across
    // fixpoint rounds (the walk order is identical every time).
    let id = cx.next_block_id;
    cx.next_block_id += 1;
    cx.block_path.push(id);
    for s in &b.stmts {
        walk_stmt(s, cx);
    }
    if let Some(fe) = &b.final_expr {
        walk_expr(fe, cx);
    }
    cx.block_path.pop();
}

fn walk_stmt<'a>(s: &'a Stmt, cx: &mut Cx<'a, '_>) {
    match &s.kind {
        StmtKind::Let {
            is_mut,
            pattern,
            ty,
            value,
        } => {
            // Stage 3c — record a frozen-element container CANDIDATE. Done
            // before the initializer is walked so a self-referential
            // initializer would be judged against the candidate, and keyed on
            // the annotation (see `container_candidate_type`).
            if let PatternKind::Binding(n) = &pattern.kind {
                // Keyed on `container_spans`, which only a `let` this ROUND
                // actually walked can populate — `container_candidates` is
                // pre-seeded from the previous round, so testing that would
                // call every container a shadow of itself.
                if cx.container_spans.contains_key(n.as_str()) {
                    // A second `let` of the same name. This analysis is
                    // name-keyed, so shadowing would conflate two containers;
                    // refuse both rather than pick one.
                    cx.container_rebound.insert(n.as_str());
                    cx.reject_container(n.as_str());
                }
                if let Some(ct) = container_candidate_type(ty.as_ref(), cx.tc) {
                    cx.container_candidates.insert(n.as_str(), ct);
                    cx.container_spans.insert(n.as_str(), value.span);
                    cx.container_path.insert(n.as_str(), cx.block_path.clone());
                    // An element read out of the container is owned by whatever
                    // was pushed in, and every one of those was proved to
                    // outlive the container — so attributing the container's
                    // OWN block to its elements is sound, and it is what lets a
                    // traversal push an element back in (`work.push(k)` where
                    // `k` came from `work[i]`).
                    cx.frozen_owner_path
                        .insert(n.as_str(), cx.block_path.clone());
                }
            }
            // Stage 2.5: a `let` whose initializer is a frozen place naming a
            // `shared` handle becomes a non-counting ALIAS rather than an
            // escape. `try_admit_frozen_alias` consumes the initializer when it
            // takes it; otherwise the ordinary walk judges it as before.
            if !try_admit_frozen_freeze(*is_mut, pattern, value, cx)
                && !try_admit_frozen_alias(*is_mut, pattern, value, cx)
            {
                walk_expr(value, cx);
            }
            // Stage 3b step 1 — a binding whose initializer is a PLACE is an
            // alias of whatever that place is rooted at, so it can never be
            // the unique name for its instance. Recorded AFTER the walk so the
            // initializer's own uses land in `used_names` first; recording it
            // here rather than at the freeze site is what lets the check see
            // `let t = a; let g = freeze t;`, where the offending `let` is not
            // the freeze's own.
            if let (PatternKind::Binding(n), true) =
                (&pattern.kind, place_root_name(value).is_some())
            {
                cx.place_bound.insert(n.as_str());
            }
        }
        StmtKind::LetUninit { .. } => {}
        StmtKind::LetElse {
            value, else_block, ..
        } => {
            walk_expr(value, cx);
            walk_block(else_block, cx);
        }
        // A `defer` body runs AT scope exit, by which point an element owner
        // bound in this scope may already be gone — so a container touched
        // from one is disqualified (stage 3c).
        StmtKind::Defer { body } => walk_restricted_block(body, cx),
        StmtKind::ErrDefer { body, .. } => walk_restricted_block(body, cx),
        // ── Stage 3b step 3: WRITE positions ────────────────────────────
        //
        // These three arms are the entire write-position enumeration, and they
        // are what pays for `place_type` resolving `mut` fields. A frozen place
        // in TARGET position is refused here rather than in the prologue,
        // because the prologue judges by SHAPE and a scalar field read is a
        // permitted shape — `g.count = 5` would otherwise sail through it.
        //
        // Exhaustiveness is structural, not vigilance: `StmtKind` is matched
        // with no `_` arm, so a new statement kind fails the build and lands
        // here to be classified. The other two write channels are refused
        // elsewhere and stay that way — a `mut`-marked call argument by
        // `walk_call_args_with_sig`'s `!a.mut_marker`, and a mutating method by
        // simply not being `len`/`is_empty`.
        StmtKind::Assign { target, value } => {
            reject_frozen_write(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::MultiAssign { targets, values } => {
            for t in targets {
                reject_frozen_write(t, cx);
            }
            for v in values {
                walk_expr(v, cx);
            }
        }
        StmtKind::CompoundAssign { target, value, .. } => {
            reject_frozen_write(target, cx);
            walk_expr(value, cx);
        }
        StmtKind::Expr(e) => walk_expr(e, cx),
    }
}

/// Stage 3b step 3 — judge a place in WRITE position.
///
/// A frozen place is refused outright: the region claims deep immutability, so
/// a write through any name reaching it is precisely what must not happen. This
/// runs INSTEAD of `walk_expr` on the target, because `walk_expr`'s prologue
/// permits a scalar read and an assignment target is scalar-shaped exactly when
/// it is most dangerous.
///
/// A non-frozen target is walked normally, so nested frozen uses inside an
/// index expression (`other[g.i] = 1`) are still judged.
/// Stage 3c — the two method shapes a frozen-element container permits on its
/// own name: `c.push(<frozen place>)` / `c.push_back(..)`, and the read-only
/// `c.len()` / `c.is_empty()`. Returns `true` when the call was consumed, which
/// means the RECEIVER is not walked — that is what keeps the container's name
/// from reaching [`walk_expr`]'s `Identifier` arm and disqualifying itself.
///
/// The third permitted shape, `c[i]`, needs nothing here: it is a frozen place
/// by [`Cx::frozen_place_root`] and the prologue consumes it.
///
/// EVERY DISQUALIFYING PATH STILL RETURNS `false`, so the call falls through to
/// the ordinary walk and is reported there. A container this function declines
/// is refused, never silently allowed.
fn try_admit_container_method<'a>(
    object: &'a Expr,
    method: &str,
    args: &'a [CallArg],
    cx: &mut Cx<'a, '_>,
) -> bool {
    let Some(c) = cx.container_receiver(object) else {
        return false;
    };
    // NO CONTAINER ADMISSION INSIDE A CLOSURE, whatever `restricted_region`
    // says. That test only disqualifies a container declared OUTSIDE the
    // closure; one declared INSIDE it passes, and the frozen root pushed into
    // it still comes from the enclosing frame. A closure's environment can
    // outlive the call, so the handle it carries is exactly the non-counting
    // handle outliving its owner that property 3 forbids — the same reason
    // every other admission in this module is suppressed by `in_closure`
    // (stage 1's first draft accepted a scalar read inside a closure body for
    // want of this). Rejecting rather than merely declining is deliberate: it
    // takes the container out of the candidate set, so `c[i]` inside the
    // closure stops being a frozen place too, and the push falls through to
    // the ordinary walk that reports it as the capture it is.
    if cx.in_closure {
        cx.reject_container(c);
        return false;
    }
    // A container declared outside the closure / `defer` / `par` body this
    // call sits in — see `restricted_region`.
    if !cx.container_visible_here(c) {
        cx.reject_container(c);
        return false;
    }
    if matches!(method, "len" | "is_empty") && args.is_empty() {
        return true;
    }
    if !matches!(method, "push" | "push_back") {
        // Any other method — `pop`, `remove`, `sort`, a user `impl Vec` method
        // — is outside the whitelist. `pop` in particular is not an oversight:
        // its result is a VALUE, not a place, so a non-counting element handed
        // back through it has nothing this pass can keep track of.
        cx.reject_container(c);
        return false;
    }
    let [arg] = args else {
        cx.reject_container(c);
        return false;
    };
    if arg.label.is_some() || arg.mut_marker {
        cx.reject_container(c);
        return false;
    }
    let Some(root) = cx.frozen_place_root(&arg.value) else {
        // An ORDINARY value pushed in. The container would then hold a mix of
        // counted and non-counting elements, and the scope-exit drop is
        // all-or-nothing — skipping it would leak the counted ones, running it
        // would over-release the frozen ones. So the container is disqualified
        // and any frozen push into it reports on the next round.
        cx.reject_container(c);
        return false;
    };
    // A scalar element is not a handle; it cannot be what makes this container
    // frozen, and `container_candidate_type` has already restricted the
    // element type to a `shared struct`, so this is a place that resolves to
    // something else entirely (an `i64` field of a frozen node, say).
    if cx.reads_as_scalar(&arg.value) {
        cx.reject_container(c);
        return false;
    }
    // THE LIFETIME TEST, and the reason a `freeze` STATEMENT's local can be
    // stored at all. A non-counting element is only safe while the owner whose
    // count it skips is still alive; the container must therefore die first.
    if !cx.owner_outlives_container(root, c) {
        cx.reject_container(c);
        return false;
    }
    if !cx.frozen_containers.contains_key(c) {
        // Disproved on an earlier round (or this is the discovery round). The
        // push falls through and is reported as the ordinary escape it is.
        return false;
    }
    // Consumed. The projection chain is permitted; any index OPERAND inside it
    // is still an ordinary expression and is walked, exactly as in the
    // permitted argument position.
    walk_place_indices(&arg.value, cx);
    true
}

/// Walk a block that a container declared OUTSIDE it may not be touched from —
/// a `defer` / `errdefer` body, a `par` / `seq` block. A container declared
/// INSIDE is unaffected; see [`Cx::restricted_region`] for why that difference
/// carries the feature.
///
/// The region is identified by the path [`walk_block`] is about to build, which
/// is why the id is read (not consumed) here: `walk_block` allocates
/// `next_block_id` as its first action, so `path + [next_block_id]` is exactly
/// the block being entered.
fn walk_restricted_block<'a>(b: &'a Block, cx: &mut Cx<'a, '_>) {
    let mut region = cx.block_path.clone();
    region.push(cx.next_block_id);
    let saved = cx.restricted_region.replace(region);
    walk_block(b, cx);
    cx.restricted_region = saved;
}

fn reject_frozen_write<'a>(target: &'a Expr, cx: &mut Cx<'a, '_>) {
    // Stage 3c — a write THROUGH a container (`c[i] = x`, `c = other`) is not
    // in the whitelist, so the container is disqualified rather than the write
    // being judged against a guarantee the container will not end up having.
    // Recorded from the target's own root, before the frozen-place test below
    // consumes it.
    if let Some(c) = place_root_name(target)
        .and_then(|r| cx.container_candidates.get_key_value(r).map(|(k, _)| *k))
    {
        cx.reject_container(c);
    }
    if let Some(root) = cx.frozen_place_root(target) {
        cx.flag(root, &target.span, Reason::Written);
        // Still walk the INDEX sub-expressions: `g.kids[other.i] = x` has a
        // second place inside it that deserves its own judgement.
        walk_place_indices(target, cx);
        return;
    }
    walk_expr(target, cx);
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
    // Stage 3c — the alias inherits its ROOT's owner: the object it points at
    // is the same object, so it can be stored exactly where the root could.
    // An unrecorded root defaults to the current block, the most restrictive
    // answer available.
    let owner = cx
        .frozen_place_root(value)
        .and_then(|r| cx.frozen_owner_path.get(r).cloned())
        .unwrap_or_else(|| cx.block_path.clone());
    walk_place_indices(value, cx);
    cx.alias_spans.insert(SpanKey::from_span(&value.span));
    cx.frozen.insert(name.as_str(), ty);
    cx.frozen_owner_path.insert(name.as_str(), owner);
    cx.aliases.insert(name.as_str());
    true
}

/// Stage 3 — `let g = freeze src;`, the FREEZE STATEMENT.
///
/// Where the stage-2.5 alias derives a frozen root from one that was already
/// frozen, this INTRODUCES one from an ordinary binding — which is the whole
/// point: with `frozen` available only as a parameter mode, "is this instance
/// immutable for the region" is a question about every call site. The
/// statement makes the region explicit and the check local.
///
/// Two names come out frozen, not one:
///
/// * `g`, the frozen handle, compiled as a non-counting alias exactly as a
///   stage-2.5 binding is — the owner is now a local in the same frame rather
///   than the caller's value, but the ownership rule that pays for it is the
///   same one, and it is the second name below that supplies it;
/// * the SOURCE's place root, for the rest of the walk. The design says
///   "`graph` stays usable read-only; freezing does not consume it", and this
///   is how that is enforced: once the root is in the frozen set, the ordinary
///   whitelist refuses to move, reassign, return, or capture it, so the owner
///   whose refcount `g` is skipping cannot go away while `g` is live. Without
///   this the alias would dangle the moment the source was consumed.
///
/// FREEZABILITY IS NOT CHECKED HERE. `check_frozen_freeze_site` reports E0512
/// on the same statement, exactly as it does for a parameter — two independent
/// entry points into one classifier, so a type that may not be frozen is
/// refused whether it was declared or frozen.
fn try_admit_frozen_freeze<'a>(
    is_mut: bool,
    pattern: &'a Pattern,
    value: &'a Expr,
    cx: &mut Cx<'a, '_>,
) -> bool {
    if !cx.freeze_spans.contains(&SpanKey::from_span(&value.span)) {
        return false;
    }
    // From here the statement IS a freeze site, so every path below reports
    // and consumes rather than falling through — a `freeze` that quietly
    // degrades to an ordinary binding would leave the author believing in a
    // guarantee they do not have, which is the failure mode this stage's own
    // first draft had twice (once on the early-out, once here).
    let binding_name = match &pattern.kind {
        PatternKind::Binding(n) => Some(n),
        _ => None,
    };
    let why = if cx.in_closure {
        Some("a `freeze` inside a closure body would put the handle in an environment that can outlive the call")
    } else if is_mut {
        Some(
            "the binding is `mut`, so it could be repointed at a value this `freeze` never checked",
        )
    } else if binding_name.is_none() {
        Some("the pattern binds more than one name, and a frozen handle cannot be spread across several")
    } else {
        None
    };
    if let Some(why) = why {
        let shown = binding_name.map(String::as_str).unwrap_or("_");
        cx.flag(shown, &value.span, Reason::FreezeRefused(why));
        walk_expr(value, cx);
        return true;
    }
    let name = binding_name.expect("checked above");
    // The source's type comes from the typechecker rather than from
    // `place_type`, because the source is NOT yet a frozen root — that is what
    // distinguishes a freeze site from an alias. Positive evidence only: an
    // expression with no recorded type, or one that is not a non-generic
    // `shared struct`, is not admitted and falls through to the ordinary walk.
    let raw = cx
        .tc
        .expr_types
        .get(&SpanKey::from_span(&value.span))
        .cloned();
    // Stage 3b step 1 — the uniqueness precondition, computed HERE because
    // this is the last point before the operand is walked (which would add the
    // source's own name to `used_names` and make every freeze look aliased).
    //
    // Both directions must hold: nothing has named the root yet, and the root
    // is not itself a place-bound alias. A root that is not a plain identifier
    // (`freeze holder.node`) is never unique — the owner `holder` is a second
    // live handle to the instance's container by construction, and proving
    // otherwise is the interprocedural analysis this stage exists to avoid.
    let uniquely_bound = matches!(&value.kind, ExprKind::Identifier(n)
        if !cx.used_names.contains(n.as_str()) && !cx.place_bound.contains(n.as_str()));
    // Recorded BEFORE the admission decision, so a source this pass declines
    // to freeze still gets classified and reported rather than silently
    // becoming an ordinary binding.
    cx.freeze_sites.push((
        raw.as_ref().and_then(head_type_name).map(str::to_string),
        value.span,
        FreezeSite::Statement { uniquely_bound },
    ));
    let Some(ty) = raw.and_then(|t| cx.as_shared_handle(t)) else {
        return false;
    };
    // The source must be a PLACE — freezing a temporary would name an owner
    // that no binding holds, so there is nothing whose lifetime dominates the
    // frozen handle's. REPORTED rather than declined: falling through would
    // make the `freeze` a silent no-op, which is worse than a refusal.
    let Some(root) = place_root_name(value) else {
        cx.flag(
            name.as_str(),
            &value.span,
            Reason::FreezeRefused("its operand is a temporary, not a place"),
        );
        walk_expr(value, cx);
        return true;
    };
    walk_place_indices(value, cx);
    cx.alias_spans.insert(SpanKey::from_span(&value.span));
    cx.frozen.insert(name.as_str(), ty.clone());
    // Stage 3c — a `freeze` STATEMENT's owner is the local it froze, so both
    // names live and die with THIS block. That is what stops a container
    // declared in an enclosing scope from holding the resulting alias: the
    // container would outlive the owner and every element would dangle.
    cx.frozen_owner_path
        .insert(name.as_str(), cx.block_path.clone());
    cx.frozen_owner_path.insert(root, cx.block_path.clone());
    cx.aliases.insert(name.as_str());
    // The source root is frozen from here on. Its own type is looked up the
    // same way; if it cannot be resolved the root is still frozen (with the
    // frozen handle's type as a stand-in), which over-restricts rather than
    // under-restricts.
    cx.frozen.entry(root).or_insert(ty);
    cx.sources.insert(root);
    true
}

/// The root name of a place expression, for the freeze source. Deliberately
/// short: `self` and a bare identifier are roots, a projection recurses, and
/// everything else (a call, a literal, a struct expression) is not a place and
/// yields `None`.
fn place_root_name(e: &Expr) -> Option<&str> {
    match &e.kind {
        ExprKind::Identifier(name) => Some(name.as_str()),
        ExprKind::SelfValue => Some("self"),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::TupleIndex { object, .. } => place_root_name(object),
        _ => None,
    }
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
    // See through a bare `.iter()` — the spelling the corpus actually uses.
    // Everything below judges the underlying PLACE, and consuming the whole
    // `for` is what keeps the peeled-away method call from reaching the
    // ordinary walk and reporting its receiver.
    let iterable = cx.frozen_iterable_place(iterable);
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
    // Stage 3c — the loop variable aliases an element of the iterated place,
    // so it inherits that place's owner exactly as a `let` alias does.
    let owner = cx
        .frozen_place_root(iterable)
        .and_then(|r| cx.frozen_owner_path.get(r).cloned())
        .unwrap_or_else(|| cx.block_path.clone());
    walk_place_indices(iterable, cx);
    if let Some(handle) = handle {
        cx.frozen.insert(name.as_str(), handle);
        cx.frozen_owner_path.insert(name.as_str(), owner);
        cx.aliases.insert(name.as_str());
    }
    true
}

/// Walk the sub-expressions a frozen place contains that are NOT part of the
/// place itself — the index operands. `f(n.kids[g(x)])` still has to check
/// `g(x)`; only the projection chain is consumed by the permitting rule.
fn walk_place_indices<'a>(e: &'a Expr, cx: &mut Cx<'a, '_>) {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => {}
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
    walk_call_args_with_sig(sig, args, cx)
}

/// The body of [`walk_call_args`], taking the resolved per-position flags
/// directly so a `frozen self` METHOD call (stage 2.7) can supply its own —
/// method declarations are keyed by `(type, method)`, not by a bare name.
fn walk_call_args_with_sig<'a>(sig: Option<Vec<bool>>, args: &'a [CallArg], cx: &mut Cx<'a, '_>) {
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
        // B-2026-08-01-33 stage 3b step 3 — a `mut`-MARKED argument is a WRITE
        // channel and must be judged before the scalar fast path below, not
        // after. `bump(mut g.count)` is scalar-shaped, so the fast path used to
        // return `continue` and the `!a.mut_marker` test further down never ran.
        //
        // It was harmless until step 3: a `mut` field was unresolvable, so
        // `reads_as_scalar` answered false and the argument fell through to
        // that test. Making the field resolve is exactly what moved this shape
        // onto the fast path — found by probing the write channels one by one,
        // not by reading, which is why the enumeration is written down here.
        if a.mut_marker {
            cx.flag(name, &a.value.span, Reason::Written);
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
        // Stage 3c — an ELEMENT READ (`c[i]`) reached from a closure /
        // `defer` / `par` body the container was not declared in. Rejecting
        // takes effect on the next fixpoint round, which is enough: the
        // rounds run to a fixed point before any diagnostic is emitted.
        if cx.container_candidates.contains_key(root) && !cx.container_visible_here(root) {
            cx.reject_container(root);
        }
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
        // Reached only for a NON-frozen identifier or receiver — a frozen one
        // is consumed by the prologue above.
        //
        // Recorded for the stage-3b uniqueness precondition (`used_names`).
        // This arm is the single funnel for a bare name in value position:
        // every handle-producing form that matters — a call argument, a
        // composite-literal field, the initializer of another `let` — reaches
        // its root through here, because the shape arms below all recurse into
        // their operands rather than inspecting roots themselves.
        ExprKind::Identifier(name) => {
            cx.used_names.insert(name.as_str());
            // Stage 3c — reaching here with a candidate CONTAINER's name means
            // the name was used BARE: returned, passed to a call, captured,
            // iterated, aliased. The three permitted shapes (`push` /
            // `len`-`is_empty` receiver, `[i]` object) all intercept before the
            // prologue, so this arm is the single funnel for everything else,
            // and reaching it disqualifies the container.
            //
            // This is the same inverted-whitelist discipline as the rest of the
            // module: the failure direction is a container refused (its frozen
            // pushes then report as ordinary escapes), never one admitted whose
            // elements can leave the frame.
            if let Some(c) = cx
                .container_candidates
                .get_key_value(name.as_str())
                .map(|(k, _)| *k)
            {
                cx.reject_container(c);
            }
        }
        ExprKind::SelfValue => {}

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
        | ExprKind::ByteStringLit(_)
        | ExprKind::StringLit(_)
        | ExprKind::MultiStringLit(_)
        | ExprKind::CStringLit { .. }
        | ExprKind::Bool(_)
        | ExprKind::Path { .. }
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
            // Stage 3c — the container whitelist, checked BEFORE the frozen
            // receiver logic below, because a container receiver is an
            // ordinary local and must not be judged as a frozen place.
            if try_admit_container_method(object, method, args, cx) {
                return;
            }
            // Stage 2.7 — a USER method whose receiver is declared `frozen
            // self`. Resolved by the receiver PLACE's type, which this pass
            // already computes, so no typechecker callee map is needed: the
            // pair `(type, method)` names exactly one declaration, and that
            // declaration's body is checked by this same pass. Passing the
            // handle on is permitted for the same reason it is to a `frozen`
            // free-function parameter — the guarantee composes across the call
            // instead of being re-derived at each site.
            let frozen_receiver = !cx.in_closure
                && cx.frozen_place_root(object).is_some()
                && cx
                    .place_type(object)
                    .as_ref()
                    .and_then(head_type_name)
                    .is_some_and(|t| cx.frozen_methods.contains_key(&(t, method.as_str())));
            if frozen_receiver
                || (!cx.in_closure
                    && cx.frozen_place_root(object).is_some()
                    && cx.permits_builtin_query(object, method))
            {
                walk_place_indices(object, cx);
            } else {
                walk_expr(object, cx);
            }
            // A `frozen self` method's own parameters compose too, so its
            // declared flags select the permitted argument slots exactly as a
            // free function's do. Every other method call still passes `None`,
            // which reports any frozen place in an argument.
            let sig = if frozen_receiver {
                cx.place_type(object)
                    .as_ref()
                    .and_then(head_type_name)
                    .and_then(|t| cx.frozen_methods.get(&(t, method.as_str())).cloned())
            } else {
                None
            };
            walk_call_args_with_sig(sig, args, cx);
        }
        ExprKind::TupleIndex { object, .. } => walk_expr(object, cx),
        ExprKind::Index { object, index } => {
            walk_expr(object, cx);
            walk_expr(index, cx);
        }
        ExprKind::Block(b) | ExprKind::Comptime(b) | ExprKind::Unsafe(b) | ExprKind::Try(b) => {
            walk_block(b, cx)
        }
        // A container declared OUTSIDE a `par` / `seq` and pushed to from
        // inside is two branches writing one `{ptr,len,cap}` — a different
        // question from the one stage 3c answers, so it is disqualified and
        // the existing capture gate decides it instead. One declared INSIDE a
        // branch is branch-local and is the shape this exists for.
        ExprKind::Seq(b) | ExprKind::Par(b) => walk_restricted_block(b, cx),
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
            // A closure's environment can outlive the call, so a container
            // declared outside and touched inside is disqualified on the same
            // terms as a `defer` body. `in_closure` above independently
            // suppresses every frozen PLACE, which is a separate guarantee.
            let mut region = cx.block_path.clone();
            region.push(cx.next_block_id);
            let saved_region = cx.restricted_region.replace(region);
            walk_expr(body, cx);
            cx.restricted_region = saved_region;
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
