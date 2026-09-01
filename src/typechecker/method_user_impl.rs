//! User-`impl` method dispatch — the terminal arm of `infer_method_call`.
//!
//! Sixth slice of the `infer_method_call` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]). This is the
//! **tail** of the first-match-wins chain: everything the built-in surfaces
//! above did not claim lands here, where the receiver is resolved to an
//! `impl` table key, candidate `impl` blocks are collected, one is picked
//! (including trait-bound gating and specialization), and the call is typed
//! against the chosen signature — or a diagnostic is produced.
//!
//! Because it is the last link rather than a guard, it returns `Type`
//! directly instead of `Option<Type>`: reaching it *is* the decision. The
//! `no method 'X' on T` diagnostics — with their candidate listing,
//! near-miss detection and bound-gate explanations — all live here.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::env::{FunctionSig, ImplInfo};
use super::inference::substitute_type_params;
use super::types::{method_callee_type_name, type_display, SubstValue, Type};
use super::TypeErrorKind;
use crate::typechecker::type_to_concrete_or_param_name;
use rustc_hash::FxHashMap;
use std::collections::HashMap;

impl<'a> super::TypeChecker<'a> {
    /// Dispatch a method call to a user `impl` block.
    ///
    /// The terminal arm of `infer_method_call`'s chain — called once, from
    /// the end, with the receiver already normalized to
    /// `receiver_for_lookup`. Returns the call's type, or `Type::Error`
    /// after emitting a diagnostic.
    /// Does any user `impl` on the `Array` head declare `method`?
    ///
    /// The single predicate behind B-2026-08-18-13's split: a fixed-array
    /// receiver whose method a user impl declares is normalized to a `Named`
    /// receiver and looked up in the impl table, and one whose method nothing
    /// declares keeps the dedicated Array REJECTION path — the one that renders
    /// "no method 'map' on type 'Array': iterator adaptors/terminals require an
    /// explicit `.iter()`". Both call sites read this, so the two halves cannot
    /// disagree about which receiver goes where.
    fn array_user_impl_declares(&self, method: &str) -> bool {
        self.env
            .impls
            .iter()
            .any(|imp| imp.target_type == "Array" && imp.methods.contains_key(method))
    }

    pub(super) fn dispatch_user_impl_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        span: &Span,
        receiver_for_lookup: &Type,
    ) -> Type {
        // B-2026-08-18-13 — a fixed-size `Array[T, N]` receiver whose method a
        // user impl actually declares is looked up like a `Named` receiver,
        // under the head name `env_add_impl` registers it by ("Array") and its
        // ELEMENT as the single arg (the size is not part of the key; see that
        // arm for why).
        //
        // Normalized here, rather than by widening the `Str | Slice` arm below,
        // to keep the Array REJECTION path exactly where it was. That path — the
        // `_` arm's Array branch — is what renders "no method 'map' on type
        // 'Array': iterator adaptors/terminals require an explicit `.iter()`",
        // and routing every Array receiver through the impl table would land an
        // absent method in the table's own not-found tail instead, which does
        // not know about `.iter()` and (for a non-user-defined, non-prelude name
        // like "Array") falls silent. Measured: with the wider arm,
        // `a.map(...)` and a plain typo both went from a clear rejection to
        // "All checks passed". So only a receiver with a MATCHING user impl is
        // normalized, and everything else keeps the diagnostic it had.
        let array_user_impl_receiver: Option<Type> = match receiver_for_lookup {
            Type::Array { element, .. } if self.array_user_impl_declares(method) => {
                Some(Type::Named {
                    name: "Array".to_string(),
                    args: vec![(**element).clone()],
                })
            }
            _ => None,
        };
        if array_user_impl_receiver.is_some() {
            // Name the resolved head for the BACKENDS. The interpreter cannot
            // work it out: a fixed `Array[T, N]` and a `Vec[T]` are both
            // `Value::Array` at runtime and `value_type_name` reports "Vec" for
            // either, so the key it builds is `Vec.<method>` while the env holds
            // `Array.<method>` — the call died with "not found on type 'Vec'
            // (no interpreter dispatch arm)" on a program `karac check` and
            // `karac build` both accepted. That run-vs-build split is the exact
            // failure that got the `Slice` version of this row reverted twice
            // (B-2026-08-13-7), so it is not a detail to leave for later.
            //
            // `method_impl_dispatch` is the right table rather than
            // `method_callee_types`, and the difference is not cosmetic: it is
            // keyed by (span, METHOD), so the chained-call span aliasing cannot
            // clobber it. Measured on the first attempt through the other
            // table — `a.head_or(-1).to_string()` shares one span, and the
            // OUTER call had overwritten the entry with "i64.to_string", so the
            // recovery correctly declined its own stale recording and the
            // interpreter still failed.
            //
            // Codegen reads the same table through `impl_dispatch_segment_at`,
            // whose fallback is the bare head name — which for an Array
            // receiver is already "Array". So this is inert there, and the
            // compiled backends behave exactly as they did.
            self.method_impl_dispatch.insert(
                (SpanKey::from_span(span), method.to_string()),
                "Array".to_string(),
            );
        }
        let receiver_for_lookup: &Type = array_user_impl_receiver
            .as_ref()
            .unwrap_or(receiver_for_lookup);

        let (type_name, type_args) = match receiver_for_lookup {
            Type::Named { name, args } => (name.clone(), args.clone()),
            // B-2026-08-12-32 — a `String` receiver reaching here has already
            // been declined by the builtin surface (the early return above
            // routes every name in `STRING_BUILTIN_METHODS`), so the only way
            // to arrive is a method that surface does not have. Keyed through
            // `impl_table_key` so this cannot drift from what `env_add_impl`
            // registers.
            //
            // `Slice` joins it in B-2026-08-13-7 and on the same terms: its
            // gate near the top of this function declines only names the
            // builtin surface does not answer, so a slice receiver reaching
            // here has a user impl waiting for it. `Array`/`Vector` are still
            // OUT — they have no such gate, so widening to them intercepts
            // every absent-method call ahead of the branch below that renders
            // the "iterator adaptors require an explicit `.iter()`" hint,
            // turning a helpful rejection into a silent `Type::Error`
            // (`test_absent_fixed_array_methods_rejected_with_iter_hint`).
            //
            // Without this arm they fell to the `else` branch below, which
            // type-checks the arguments and returns `Type::Error` in SILENCE —
            // the historical fall-through that comment describes. That silence
            // is why the row's declaration-side symptom looked like a missing
            // feature rather than a hole: nothing complained anywhere.
            Type::Str | Type::Slice { .. } => {
                match super::types::impl_table_key(receiver_for_lookup) {
                    Some(key) => key,
                    None => {
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Type::Error;
                    }
                }
            }
            // A refinement receiver that survived the base-deref above
            // (i.e. it declares this method itself) resolves under its
            // nominal name. Non-generic at v1, so no type args.
            Type::Refinement { name, .. } => (name.clone(), Vec::new()),
            Type::TypeParam(name) if name == "Self" => {
                // Self-receiver dispatch (slice 3.5 of the method-resolution
                // CR — `phase-4-interpreter.md` item 8). `self.method()`
                // inside a trait default body resolves through the enclosing
                // trait's own methods + supertrait closure. Outside trait
                // bodies (`enclosing_trait == None`) the dispatcher returns
                // `Type::Error` to preserve the pre-existing silent
                // fallthrough — impl-method bodies bind `Self` via
                // `current_self_type`, a different mechanism.
                return self.dispatch_self_receiver_method(method, args, span);
            }
            Type::TypeParam(name) => {
                // Receiver-form generic call-site dispatch (slice 2 of the
                // method-resolution CR — see `phase-4-interpreter.md` item 8).
                // The complement to type-prefixed `T.method()` dispatch via
                // `try_dispatch_typeparam_assoc_fn` (`infer_call`): for
                // `t.method(args)` where `t: T` and `T: SomeTrait` declares
                // `method`, look up T's bounds in `enclosing_bounds`, find
                // the trait declaring `method`, and lower the trait method's
                // signature with `Self → Type::TypeParam(T)` substitution.
                // Multiple matching bounds → AmbiguousAssocFn (UFCS hint);
                // zero matches → NoMethodFound; exactly one → dispatch.
                //
                // `Self` is handled in the arm above (slice 3.5) — it
                // routes to `dispatch_self_receiver_method` which consults
                // the enclosing trait being defined, not just bounds.
                return self.dispatch_typeparam_receiver_method(name, method, args, span);
            }
            Type::Existential { .. } => {
                // `impl Trait` slice 6 — TAIT and return-position
                // existentials dispatch through the declared trait
                // surface. Find the trait's own method by name; if
                // missing, emit `E_TAIT_NOT_IMPLEMENTED_YET` (slice 6)
                // when the existential is TAIT-sourced (the witness's
                // own non-trait method might exist but resolving it
                // requires the witness-inference pipeline that lands
                // in P1), or the generic no-method-on-trait diagnostic
                // for return-position existentials. Method calls that
                // hit the trait surface succeed exactly as if the
                // receiver were a `Type::TypeParam` with the trait as
                // its only bound — slice 3's `enclosing_bounds` story
                // already covers the lookup machinery.
                let receiver_existential = receiver_for_lookup.clone();
                return self.dispatch_existential_receiver_method(
                    &receiver_existential,
                    method,
                    args,
                    span,
                );
            }
            _ => {
                // A SCALAR PRIMITIVE receiver (`i64`, `u32`, `f64`, `char`,
                // `bool`) with a USER trait/inherent impl method (`impl Dbl for
                // u8 { fn dbl(self) -> Self { ... } }`) dispatches through the
                // same impl-table path a `Named` receiver uses: register it as
                // `(prim, [])` and fall through to the resolution below. The
                // builtin comparison ops have dedicated backend arms and their
                // baked stdlib impls (`Ord`/`Eq`/… on primitives) carry a
                // `(self, other)`-shaped signature that the impl-table dispatch
                // mis-counts (`a.cmp(b)` → "expects 2 args, found 1"); keep them
                // on the historical poison-with-diagnostic path. So route ONLY a
                // NON-builtin method that has a real impl candidate; everything
                // else falls through to the poison branch. B-2026-07-03-5.
                //
                // `cast` was on this list until B-2026-08-11-4 and did not
                // belong: it names no dispatchable method anywhere. Kāra spells
                // conversion with the `as` OPERATOR (design.md § Numeric
                // Semantics defines every legal pair; the fallible int→char form
                // is the static `char.try_from`), and grepping `src/` and
                // `runtime/src/` found the string `"cast"` in this list and in no
                // dispatch arm, trait, test, example or `.kara` source. So the
                // entry exempted a method that does not exist, which cost two
                // things: `v.cast()` was check-green then run-red on EVERY
                // primitive alike, and a user `impl i64 { fn cast(self) -> f64 }`
                // was unreachable, because the exemption short-circuits ahead of
                // the impl lookup. Removing it needs no replacement — the normal
                // path already does the right thing in both directions.
                //
                // `char` and `bool` joined the numeric primitives here in
                // B-2026-08-11-2. They had been left on the silent fall-through
                // below, which made them the ONLY receivers in the language that
                // accepted any method name at all: `c.completely_bogus()`
                // typechecked, and because the poison `Type::Error` is
                // universally assignable it also unified with whatever the use
                // site asked for (`let n: i64 = …`, `let s: String = …`, `let v:
                // Vec[i64] = …` all passed off the same call). The program then
                // died at run time — under `--interp` with "no interpreter
                // dispatch arm", and under `build` with a message telling the
                // author they had found a CODEGEN BUG and should go add a
                // dispatcher arm. A typo on a `char` method is user error; it
                // must not read as a compiler defect. Their surface is closed
                // and small (the early intercepts above: `clone`/`to_string` on
                // both, plus `is_*`/`to_ascii_*case`/`to_digit` on `char`), so
                // an unknown name here is a genuine typo — exactly the numeric
                // receivers' argument. Routing real impls through the table is a
                // second win: a `impl char { fn shout(self) -> String }` call
                // used to poison to `Type::Error` too, so `let n: i64 =
                // c.shout()` passed; now its return type is checked.
                // B-2026-08-31-15 — the name-keyed exemption that used to sit
                // here (`PRIMITIVE_VALUE_METHODS = ["cmp", "eq", "ne", "lt",
                // "le", "gt", "ge"]`) is GONE. It shielded those seven names on
                // a non-float scalar receiver from the impl-table dispatch
                // below, and what it did was route them into the branch that
                // type-checks the ARGUMENTS and returns `Type::Error`. That
                // poison is universally assignable, so `let s: String =
                // n.cmp(m)` type-checked and printed `Less` into a String slot,
                // and `let v: Vec[i64] = n.cmp(m)` type-checked too — the same
                // silent-poison class this function's comments above record
                // closing for `char`/`bool` (B-2026-08-11-2) and for the float
                // receivers (B-2026-08-11-9). It survived those because it
                // predates them and read as an arity workaround rather than as
                // a resolution bypass.
                //
                // It was kept once, deliberately, on a trade that MEASUREMENT
                // later refuted. The reasoning was: routing the six
                // operator-named methods through the impl table makes them
                // type-check and then fail `karac build` with "no handler for
                // method 'eq'", and turning a silently-poisoned call into a
                // check-green build-red one is the worse trade. The premise was
                // false — that divergence was ALREADY the state of `main`,
                // exemption or not. Measured across the receiver classes that
                // accept them, `eq`/`ne`/`lt`/`le`/`gt`/`ge` were check-green,
                // interp-working and build-failing in 36 cells. `String` is the
                // independent proof: it is not in the exempted receiver gate at
                // all, typed correctly as `bool` through this very table, and
                // still failed to build — with a different message ("Vec/String
                // method 'eq' is not yet supported in codegen"), which is what
                // shows the gap was codegen's and never the exemption's. The
                // value-receiver arms landed in `compile_method_call` alongside
                // this change, so all 36 now build and agree with the
                // interpreter, and dropping the exemption costs nothing.
                //
                // What the impl table now gives these seven: `cmp` types as
                // `Ordering` and the six as `bool`, so `let s: String =
                // n.cmp(m)` fails exactly as it already did on a String
                // receiver. The `(self, other)` arity mis-count that was the
                // exemption's ORIGINAL justification is gone too —
                // B-2026-08-31-13 made the value-receiver arity structural by
                // dropping a leading `self` param in this function's pick
                // branch below, which is also why `partial_cmp` never needed to
                // be on the list.
                //
                // FLOATS ARE STILL CARVED OUT, and the carve-out needs NO
                // PREDICATE — which is the part worth reading before touching
                // this. `env_build.rs`'s
                // `eq_ord_targets` registers `Eq`/`Ord` for the integers,
                // `bool`, `char`, `String` and the F32/F64 wrapper types, and
                // DELIBERATELY skips `f32`/`f64` — "IEEE NaN breaks Eq/Ord",
                // which matches Rust (`f64: PartialOrd` but not `Ord`). A bare
                // float has no baked candidate, so `find_methods_with_args`
                // comes back empty and the existing error branch says `no
                // method 'cmp' on type 'f64'` — the right answer, and the one
                // B-2026-08-11-9 closed. So the correct amount of float-specific
                // code here is NONE, and writing any is how it breaks: the first
                // attempt at this change kept a float arm of the old predicate,
                // which INVERTED its meaning (the flag means "skip the table AND
                // skip the error, return poison" — floats had been outside it,
                // which is what produced the error) and re-poisoned all seven
                // float cells in one line. Measured on the probe, not reasoned
                // about, which is the only reason it was caught.
                // `partial_cmp` is the spelling that serves them, and it
                // resolves through the table on every receiver including
                // floats. The `==` / `<` / `>` OPERATORS on floats are a
                // separate lowering and are untouched.
                if matches!(
                    receiver_for_lookup,
                    Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char
                ) {
                    if let Some(prim) = method_callee_type_name(receiver_for_lookup) {
                        if !self
                            .env
                            .find_methods_with_args(&prim, &[], method)
                            .is_empty()
                        {
                            // Route to impl-table dispatch (arg inference /
                            // label validation / Self resolution happen there).
                            (prim, Vec::new())
                        } else {
                            // For non-impl methods, just type-check args and
                            // return Error. Close the silent-poison hole for
                            // numeric receivers: their method surface is closed
                            // (registered builtin ops + a small value-receiver
                            // special set), so an unknown method here is a
                            // genuine typo, not a partially-implicit prelude
                            // surface. Without the error it returned
                            // `Type::Error` (poison, universally assignable, so
                            // `let s: String = x.bogus()` typechecked clean) and
                            // then exploded in the backend. `abs`/`clone`/
                            // `to_string` are handled in the early intercept
                            // above and never reach here.
                            for arg in args {
                                self.infer_expr(&arg.value);
                            }
                            {
                                let mut msg = format!("no method '{}' on type '{}'", method, prim);
                                // `char` has no numeric-value method — the route
                                // is the cast. This is the exact spelling the
                                // dogfood that filed B-2026-08-11-2 guessed
                                // (`c.to_i64()`), so name the replacement rather
                                // than leaving the author to search a surface
                                // that does not have one.
                                if matches!(receiver_for_lookup, Type::Char)
                                    && matches!(
                                        method,
                                        "to_i64"
                                            | "as_i64"
                                            | "to_int"
                                            | "to_u32"
                                            | "as_u32"
                                            | "code_point"
                                            | "ord"
                                    )
                                {
                                    msg.push_str(
                                        ": a `char`'s numeric value comes from the cast — \
                                         write `c as i64` (or `as u32`)",
                                    );
                                }
                                // The `is_digit` hint that sat here (B-2026-08-11-2)
                                // is gone because the method is no longer missing
                                // (B-2026-08-12-25). A bare `c.is_digit()` now lands
                                // on the radix arm's arity error, which carries the
                                // same two routes.
                                self.type_error(msg, *span, TypeErrorKind::NoMethodFound);
                            }
                            return Type::Error;
                        }
                    } else {
                        for arg in args {
                            self.infer_expr(&arg.value);
                        }
                        return Type::Error;
                    }
                } else if matches!(receiver_for_lookup, Type::Array { .. })
                    && !self.array_user_impl_declares(method)
                {
                    // A fixed-size `Array[T, N]` receiver whose method was not
                    // resolved by the modelled read arm (`len`/`is_empty`/`get`/
                    // `first`/`last`/`contains`), the iterator-source arm
                    // (`iter`/`into_iter`), or `as_slice`/`as_ptr`: the method is
                    // genuinely absent on both backends (`to_vec`/`slice`/`rev`
                    // are interp 'method not found'; a direct iterator adaptor
                    // `a.map(...)` miscompiles). Reject rather than the silent
                    // `Type::Error` (B-2026-07-17-19), with the same actionable
                    // `.iter()` hint Vec uses when the name is an iterator-
                    // surface method (a fixed array is iterable).
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    let mut msg = format!("no method '{}' on type 'Array'", method);
                    if Self::is_iterator_surface_method(method) {
                        let recv = match &object.kind {
                            ExprKind::Identifier(n) => n.clone(),
                            _ => "xs".to_string(),
                        };
                        msg.push_str(&format!(
                            ": iterator adaptors/terminals require an explicit `.iter()` — write `{}.iter().{}(...)`",
                            recv, method
                        ));
                    }
                    self.type_error(msg, *span, TypeErrorKind::NoMethodFound);
                    return Type::Error;
                } else {
                    // For other non-named types (chiefly a bare `Type::Str`
                    // receiver, left on the historical silent fall-through —
                    // String has a large partially-implicit method surface not
                    // modelled in the impl table), just type-check args and
                    // return Error. `char`/`bool` used to sit here too; they
                    // moved up to the scalar-primitive arm in B-2026-08-11-2.
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return Type::Error;
                }
            }
        };

        // Look up method on the receiver type with inherent-beats-trait
        // priority per design.md § Method Resolution Step 3, plus
        // conditional-impl filtering against the receiver's concrete
        // generic args (slice 1 of the method-resolution CR — see
        // `phase-4-interpreter.md`). All-candidates collection lets us
        // detect Step-4 ambiguity (slice 3): >1 surviving candidate at
        // the same priority tier (e.g. two trait impls when no inherent
        // matches) emits AmbiguousMethod and returns Type::Error.
        let candidates = self
            .env
            .find_methods_with_args(&type_name, &type_args, method);
        let method_pick: Option<(ImplInfo, FunctionSig)> = if candidates.len() > 1 {
            // Render each candidate as `Trait.method(receiver)` (or
            // `Type.method(receiver)` for the rare inherent-vs-inherent
            // case). The signature display includes the receiver-then-args
            // tuple plus return type so the programmer can tell the
            // candidates apart at a glance.
            let candidate_lines: Vec<String> = candidates
                .iter()
                .map(|(imp, sig)| {
                    let dispatcher = imp
                        .trait_name
                        .clone()
                        .unwrap_or_else(|| imp.target_type.clone());
                    let params_display = std::iter::once(type_name.clone())
                        .chain(sig.params.iter().map(type_display))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "    `{}.{}({})` -> {}",
                        dispatcher,
                        method,
                        params_display,
                        type_display(&sig.return_type),
                    )
                })
                .collect();
            let receiver_display = if type_args.is_empty() {
                type_name.clone()
            } else {
                format!(
                    "{}[{}]",
                    type_name,
                    type_args
                        .iter()
                        .map(type_display)
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            };
            self.type_error(
                format!(
                    "ambiguous method '{}' on receiver of type '{}': \
                     multiple candidates apply. Use UFCS to disambiguate:\n{}",
                    method,
                    receiver_display,
                    candidate_lines.join("\n"),
                ),
                *span,
                TypeErrorKind::AmbiguousMethod,
            );
            for arg in args {
                self.infer_expr(&arg.value);
            }
            return Type::Error;
        } else {
            candidates
                .into_iter()
                .next()
                .map(|(imp, sig)| (imp.clone(), sig.clone()))
        };

        match method_pick {
            Some((imp, sig)) => {
                // B-2026-08-13-8 — name the WINNER for the backends. This is
                // the one point in the compiler that knows which of several
                // same-head impls a call resolved to: `find_methods_with_args`
                // has just compared `target_args` vector-wise, so `imp` is the
                // right impl even when `Vec[i64]` and `Vec[String]` both define
                // `describe`. Neither runtime can redo that — codegen's
                // `inferred_receiver_type` yields a bare head name and the
                // interpreter's `value_type_name` reads a type-erased runtime
                // value — so both used to rebuild `Vec.describe` and get
                // whichever impl their own lookup order happened to surface
                // (LLVM's first, the env's last). Recording the resolved impl's
                // qualified segment here is what lets them agree with each
                // other AND with check.
                //
                // Nothing is recorded unless the impl is in a colliding group,
                // so this table is empty for almost every program.
                if let Some(target_span) = imp.target_span.as_ref() {
                    if let Some(qualified) = self
                        .impl_dispatch_names
                        .get(&SpanKey::from_span(target_span))
                    {
                        self.method_impl_dispatch.insert(
                            (SpanKey::from_span(span), method.to_string()),
                            qualified.clone(),
                        );
                    }
                }
                // B-2026-08-31-15 — the SECOND population of this table, for a
                // second resolution neither runtime can redo, and for exactly
                // the reason above. A user `impl i64 { fn lt(self, other: i64)
                // -> String }` now wins over the baked `Ord` for `a.lt(b)`
                // (removing `PRIMITIVE_VALUE_METHODS` is what made the impl
                // reachable at all), and both backends have to agree with that.
                //
                // Codegen can: its arm consults `user_impl_method_exists`,
                // which reads the same static receiver type the typechecker
                // did. The INTERPRETER cannot — `value_type_name` reads a
                // type-erased `Value::Int`, which says `i64` for a receiver
                // that is statically `u32`, so a bare env lookup applied the
                // `impl i64` method to a `u32` receiver. Measured: with that
                // lookup, `c.lt(d)` on `u32` answered `user` under `--interp`
                // and `false` compiled, where the typechecker types it `bool`
                // — the compiled side was right. Recording the winner here is
                // what lets the interpreter ask "did CHECK pick a user impl
                // for this call site?" instead of guessing from the value.
                //
                // Inert for codegen, like the Array insert above:
                // `impl_dispatch_segment_at`'s fallback is the bare head name,
                // which for an `i64` receiver is already `i64`.
                //
                // `target_span.is_some()` is the USER-vs-BAKED discriminator,
                // and it is load-bearing: `register_builtin_impl` puts the
                // baked `Ord`/`Eq` in this same table, reaching this same
                // `method_pick` branch with `target_type == "i64"`. Recording
                // those too made the interpreter's gate fire for EVERY
                // primitive comparison and stand the builtin aside with nothing
                // behind it — 40 of 41 cells went from agreeing to "method 'lt'
                // not found on type 'i64' (no interpreter dispatch arm)". A
                // synthesized impl has no source target to point at; a written
                // one does.
                if imp.target_span.is_some()
                    && matches!(method, "cmp" | "eq" | "ne" | "lt" | "le" | "gt" | "ge")
                    && crate::prelude::PRELUDE_PRIMITIVES.contains(&imp.target_type.as_str())
                {
                    self.method_impl_dispatch.insert(
                        (SpanKey::from_span(span), method.to_string()),
                        imp.target_type.clone(),
                    );
                }
                // B-2026-09-01-20 — instance-method DEFAULT ARGUMENT fill.
                //
                // This is the first phase that can do it. `default_args`'s
                // pre-resolve pass fills a free function's and an associated
                // function's calls, because their callee is named
                // syntactically; a method's is named through the RECEIVER, so
                // the impl cannot be picked until the receiver has a type. It
                // has one here, and `sig` is the resolved winner, so the key
                // below is exact rather than a guess by method name — which is
                // what makes this safe where a name-keyed fill in the earlier
                // pass would not be (a stdlib method of the same name could
                // claim a call on a type that does not have it, silently).
                //
                // The plan comes from the SAME `try_fill` the other two
                // spellings use, so labels, contiguity and the
                // no-name-destructuring rule cannot drift between them; it
                // self-gates on arity, so calling it unconditionally is safe.
                // The completed list is recorded for `lowering` to splice into
                // the AST — before effectcheck, ownership, the interpreter and
                // codegen — so every surface sees one ordinary full-arity call.
                //
                // BEFORE `validate_labels`, not after: a fill exists precisely
                // to let a label SKIP a defaulted parameter (`h.g(1, c: 9)`),
                // and validating the author's shorter list rejects that as
                // "label 'c' does not match parameter 'b' at position 2" —
                // measured. Every check from here on sees the completed list,
                // which is what the author would have written out in full.
                let default_filled: Option<Vec<CallArg>> = self
                    .method_defaults
                    .get(&format!("{type_name}.{method}"))
                    .and_then(|info| crate::default_args::try_fill(args, info));
                if let Some(list) = &default_filled {
                    self.method_default_fills
                        .insert((SpanKey::from_span(span), method.to_string()), list.clone());
                }
                let args: &[CallArg] = default_filled.as_deref().unwrap_or(args);
                // Validate labels against method parameter names
                self.validate_labels(args, &sig.param_names, span);
                // Pre-bind the impl's generic params to the receiver's
                // concrete type args (mirroring the concrete-type UFCS path
                // above) BEFORE solving the call args. Without this, a method
                // whose only `T`-position is the return type or a
                // closure-return param — e.g. `OnceLock[T].get_or_init(init:
                // Fn() -> T) -> ref T` — leaves `T` unsolved (nothing in the
                // non-closure args pins it), so the receiver's concrete
                // `[i64]` never reaches the signature and inference fails with
                // "cannot infer type parameter 'T'". Binding here makes the
                // value-receiver path consistent with UFCS dispatch; for a
                // non-generic receiver (empty `type_args` / no impl generics)
                // `recv_subs` is empty and behavior is unchanged.
                let recv_subs: HashMap<String, SubstValue> = imp
                    .generic_params
                    .as_ref()
                    .map(|gp| {
                        gp.params
                            .iter()
                            .zip(type_args.iter())
                            .map(|(p, t)| (p.name.clone(), SubstValue::Type(t.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                // B-2026-08-30-43 — hand the same binding to the interpreter.
                // It is computed here anyway; the tree-walk had no way to see
                // it, because an impl binds `T` from the RECEIVER's type args
                // and only free-function calls ever pushed a substitution
                // frame. Keyed by `(span, method)` like `method_impl_dispatch`
                // above, since `MethodCall.span == receiver.span` aliases a
                // chain. Entries that do not name a concrete type are dropped
                // rather than recorded as themselves: `resolve_type_param`
                // treats any hit as an answer, so recording `T -> "T"` would
                // shadow an OUTER frame that does know what `T` is, turning a
                // resolvable nested call into an unresolvable one.
                if !recv_subs.is_empty() {
                    let frame: FxHashMap<String, String> = recv_subs
                        .iter()
                        .filter_map(|(name, sv)| match sv {
                            SubstValue::Type(t) => type_to_concrete_or_param_name(t)
                                .filter(|resolved| resolved != name)
                                .map(|resolved| (name.clone(), resolved)),
                            _ => None,
                        })
                        .collect();
                    if !frame.is_empty() {
                        self.method_impl_type_subs
                            .insert((SpanKey::from_span(span), method.to_string()), frame);
                    }
                }
                // Resolve `Self` in the signature to the concrete receiver
                // type. `recv_subs` only binds the impl's own generic params
                // (e.g. `T`); a method declared `-> Self` (or taking
                // `other: Self`) otherwise leaves `Self` unresolved at the call
                // site, so `a.m()` would type as `Self` and downstream field
                // access / codegen field-offset recovery fails (reads 0). In a
                // concrete-receiver dispatch `Self` always names the receiver's
                // type. (Self-receiver dispatch returned earlier at the
                // `TypeParam("Self")` arm, so `receiver_for_lookup` is concrete
                // here.)
                let params: Vec<Type> = sig
                    .params
                    .iter()
                    .map(|p| substitute_type_params(p, &recv_subs))
                    .map(|p| Self::resolve_self_in_type(p, receiver_for_lookup))
                    .collect();
                let return_type = Self::resolve_self_in_type(
                    substitute_type_params(&sig.return_type, &recv_subs),
                    receiver_for_lookup,
                );
                // B-2026-08-31-13 — a BAKED builtin impl carries its receiver
                // IN `params`: `register_builtin_impl`'s comparison signatures
                // are `param_names: [self, other], params: [ty, ty]`. A USER
                // impl does not — its receiver rides `self_param` and never
                // reaches `FunctionSig::params` — which is why the comment
                // below could say "excluding self" and be right for every case
                // that had been exercised.
                //
                // So the value-receiver spelling of a baked two-arg method
                // counted 2 expected against 1 given: `x.partial_cmp(y)` was
                // rejected with "expects 2 argument(s), found 1" on every
                // concrete receiver. `cmp` escaped only because a name-keyed
                // exemption above routed it away from this dispatch entirely —
                // into a branch that returns `Type::Error`, so it was never
                // really checked either.
                //
                // Dropping the leading `self` here is structural rather than
                // name-keyed, so it covers every baked method with a `self`
                // param at once; the exemption list then no longer has to grow
                // a name each time one is added.
                let params: Vec<Type> = if sig.param_names.first().map(Option::as_deref)
                    == Some(Some("self"))
                    && params.len() == sig.param_names.len()
                    && !params.is_empty()
                {
                    params[1..].to_vec()
                } else {
                    params
                };
                // Check argument count (excluding self)
                if args.len() != params.len() {
                    // A defaulted method's expectation is a RANGE, phrased the
                    // way the free-function path phrases it — otherwise
                    // "expects 2, found 1" is actively misleading about a
                    // signature one of whose parameters is optional.
                    let optional = self
                        .method_defaults
                        .get(&format!("{type_name}.{method}"))
                        .map(|i| i.defaults.iter().filter(|d| d.is_some()).count())
                        .unwrap_or(0);
                    let expected = if optional > 0 && optional < params.len() {
                        format!("{} to {}", params.len() - optional, params.len())
                    } else {
                        params.len().to_string()
                    };
                    self.type_error(
                        format!(
                            "method '{}' expects {} argument(s), found {}",
                            method,
                            expected,
                            args.len()
                        ),
                        *span,
                        TypeErrorKind::WrongNumberOfArgs,
                    );
                    for arg in args {
                        self.infer_expr(&arg.value);
                    }
                    return return_type;
                }
                // Reuse the round-10.1 closure-pushdown helper so any
                // remaining method-level generics solve from non-closure args
                // before checking closure args. `apply_call_site_marker` is
                // `false`: per design.md, the call-site `mut` marker rule
                // applies only to free-function calls, never to method calls.
                // B-2026-08-22-3 — hand the method's OWN where clause and
                // formal generic-param names to the substitution checker, so
                // the call-site bound discharge actually runs here.
                //
                // This path used to call the thin `check_call_args_with_substitution`
                // wrapper, which passes `None` for the where clause — so a
                // method discharged NOTHING: not the assoc-type-equality bound
                // this row is about, not a plain `T: Trait`, not a projection
                // bound, not a const predicate. The free-function path
                // (`expr_call.rs`) always passed its clause, so the same
                // program was checked on one spelling and unchecked on the
                // other. Found while fixing the assoc-eq discharge and fixed
                // with it, because the spec's motivating example for that
                // bound (`fn extend[I: Iterator[Item = T]]`) is a METHOD —
                // fixing only free functions would have left it unenforced
                // exactly where it is written.
                //
                // `apply_call_site_marker` stays `false`: per design.md the
                // call-site `mut` marker rule applies only to free-function
                // calls. `explicit_generic_args` stays `None` — this dispatch
                // has no turbofish surface to pass along.
                self.check_call_args_with_substitution_full(
                    args,
                    &params,
                    &return_type,
                    span,
                    /* apply_call_site_marker = */ false,
                    None,
                    Some(&sig.generic_params),
                    sig.where_clause.as_ref(),
                    span,
                )
            }
            None => {
                for arg in args {
                    self.infer_expr(&arg.value);
                }
                // Tightening: error only for user-defined types whose impls
                // are exhaustively known. Built-in prelude types (`Option`,
                // `Result`, `Vec`, `Regex`, etc. — see `prelude::PRELUDE_TYPES`)
                // have a partially-implicit method surface (`.unwrap()`,
                // `.is_ok()`, regex methods that route through Type::Named
                // dispatch above but may not match every name) so they keep
                // the historical silent fall-through.
                let is_user_defined = (self.env.structs.contains_key(&type_name)
                    || self.env.enums.contains_key(&type_name)
                    // Distinct types have an exhaustively-known method surface
                    // (inherent impls only — no base deref), so an unresolved
                    // method on one is a real `NoMethodFound`, not the
                    // historical silent prelude fall-through.
                    || self.env.distinct_bases.contains_key(&type_name))
                    && !crate::prelude::PRELUDE_TYPES.contains(&type_name.as_str());
                // These prelude types have method surfaces EXHAUSTIVELY
                // resolved before this fall-through, so a method that reaches
                // here is genuinely absent. For `Option`/`Result` the surface
                // is small and every valid method (`unwrap`, `map`, `is_some`,
                // `ok_or`, `map_err`, …) resolves via a dedicated arm above or
                // a baked stdlib impl. For `Vec`/`VecDeque` the native
                // surface (`push`/`pop`/`get`/`len`/`sort`/`sum`/`join`/…)
                // resolves in dedicated arms, and the iterator ADAPTOR/TERMINAL
                // surface (`map`/`filter`/`collect`/`fold`/…) resolves only
                // through an explicit `.iter()` (the `Iterator[T]` dispatch
                // above) — a direct `v.map(...)` runs on NO backend
                // (interpreter: "method not found"; AOT: link/miscompile), so
                // accepting it was a pure check/execution hole
                // (B-2026-07-17-12, extended to `Tensor`/`DataFrame`).
                // For `Tensor`/`DataFrame` the numerical surface
                // (`reshape`/`zip_with`/`matmul`/`map`/`sum`/…) resolves in
                // `infer_tensor_*` / the dataframe arms; an absent name that
                // slips past them ran on no backend just the same. (The
                // fixed-size `Array[T, N]` type is a STRUCTURAL `Type::Array`,
                // not `Type::Named{"Array"}`, so it never reaches this
                // Named-keyed arm — its own unknown-method hole is a separate
                // follow-up.) The common
                // way to reach here is either that iter-less adaptor call or a
                // wrong-container call — invoking an inner-type method on an
                // un-unwrapped optional (`opt.len()`, `res.push(x)`,
                // `grid.get(i).len()` where `get` returns `Option[Vec[_]]`).
                // The silent fall-through poisoned those to `Type::Error`
                // (universally assignable), so they typechecked clean and then
                // detonated at runtime — an interpreter `unreachable!` (“the
                // typechecker accepted .len() on a type without one”) or a
                // codegen “no handler for method”. Reject them here like a
                // user-defined type, the same silent-poison tightening applied
                // to numeric receivers (B-2026-07-03-5) and user types.
                const EXHAUSTIVE_PRELUDE: &[&str] =
                    &["Option", "Result", "Vec", "VecDeque", "Tensor", "DataFrame"];
                let is_exhaustive_prelude = EXHAUSTIVE_PRELUDE.contains(&type_name.as_str());
                // B-2026-07-31-1 — a BAKED stdlib handle type (`File`,
                // `TcpStream`, `Mutex`, `Regex`, `Pool`, `Interner`,
                // `Semaphore`, `Arena`, `BufReader`/`BufWriter`, …) has its
                // ENTIRE method surface declared as real `impl` blocks in
                // `runtime/stdlib/*.kara`, spliced in by `register_baked_stdlib`.
                // Those impls resolve through the `Some(...)` dispatch above, so
                // reaching this `None` arm means the method genuinely is not on
                // the type — yet the blanket `PRELUDE_TYPES` exclusion in
                // `is_user_defined` let every such name fall through silently,
                // so `f.totally_bogus_method_xyz()` on a `File` typechecked
                // clean while the same call on `String`/`Vec`/`Map` was
                // correctly rejected (11 of 11 resolvable baked handle types
                // affected). `is_baked_stdlib_nominal_type` restores the check
                // by scanning the baked programs for a struct/enum/distinct of
                // this name — the `defining_stdlib_origin` flag can't be used,
                // as `STDLIB_PROGRAMS` items carry it `false` at parse time (a
                // baked `File` reads the same `false` a user struct does). The
                // compiler-native prelude collections
                // (`Vec`/`VecDeque`/`Option`/`Result`/`Tensor`/`DataFrame`) are
                // declared in the baked sources too but are already covered by
                // `is_exhaustive_prelude`; `Map`/`Set`/`SortedMap`/… reject via
                // their own dedicated arms above and never reach here — so any
                // method that DOES reach this arm on a baked type is genuinely
                // absent (a dedicated arm, if one existed for it, would have
                // matched first).
                let is_baked_stdlib_type = crate::prelude::is_baked_stdlib_nominal_type(&type_name);
                // Args-specialization tightening: even on prelude types, fire
                // NoMethodFound when the method exists on a *different*
                // args-specialization of this type-name (e.g.,
                // `Option[i32].is_lt()` when only `impl Option[Ordering]`
                // declares `is_lt`). Preserves the silent fall-through when
                // the method is genuinely absent (`Vec[i32].some_typo()`
                // stays silent) while surfacing the args-mismatch case that
                // would otherwise silently reach the interpreter and produce
                // a wrong answer through unrelated dispatch.
                let method_on_other_specialization =
                    self.env.impls.iter().any(|imp| {
                        imp.target_type == type_name && imp.methods.contains_key(method)
                    });
                // A comptime-derived type (e.g. `#[derive(Message)]`) gains
                // methods only after typecheck, so its method set is open here —
                // suppress the not-found diagnostic for such types.
                // Before the generic "no method" message: the method may
                // genuinely EXIST on a matching impl that was FILTERED OUT of
                // `find_methods_with_args` because the receiver's element type
                // fails one of the impl's bounds (`impl[T: Ord] Trait for
                // Column[T]` invoked on `Column[f64]` — f64 is deliberately not
                // `Ord`). "no method 'span'" hides that; surface the failing
                // bound instead (reusing the float-`Ord`/`Eq`/`Hash` → wrapper
                // hint). This is the clarity B-2026-07-04-15 lacked — the
                // rejection there was CORRECT-BY-DESIGN, but read as a
                // container/monomorphization bug because the message named the
                // wrong problem.
                let bound_gate = self.env.impls.iter().find_map(|imp| {
                    if imp.target_type != type_name
                        || !imp.methods.contains_key(method)
                        || !super::types::impl_args_match(&imp.target_args, &type_args)
                    {
                        return None;
                    }
                    self.env
                        .first_unsatisfied_bound(imp, &type_args)
                        .map(|(pn, b, cty)| (imp.trait_name.clone(), pn, b, cty))
                });
                if let Some((trait_of_impl, param_name, bound, concrete)) = bound_gate {
                    // An ERROR-typed argument means the type parameter was never
                    // INFERRED, not that some concrete type fails the bound
                    // (B-2026-08-25-26). `W.from([])` on `impl[T: Ord] W[T]`
                    // leaves `T = Type::Error`, and rendering the bound message
                    // for it produced three wrong things at once: it blamed the
                    // bound rather than the inference failure, printed `<error>`
                    // as though it were a type name, and then listed every type
                    // implementing `Ord` — none of which is the fix. The fix is
                    // an annotation on the literal.
                    //
                    // The sibling discharge path in `exprs.rs` already skips
                    // `Type::Error` for exactly this reason ("already-error —
                    // upstream diagnostics handle. Avoid noise."); this gate was
                    // simply never given the same guard. It cannot skip outright,
                    // though: when the un-inferrable literal is the BARE `[]`
                    // form nothing upstream reports it, so skipping would accept
                    // the program in silence. So: stay silent when the root cause
                    // is already on the record (the `Vec[]` prefix form, which
                    // reports at the literal's own span), and otherwise say what
                    // is actually wrong.
                    if matches!(concrete, Type::Error) {
                        if !self.reported_uninferrable_empty_literal {
                            // Anchor the caret on the empty literal that caused
                            // this, when the receiver is a binding whose origin
                            // was recorded exactly (B-2026-08-25-31). Pre-fix
                            // the caret always landed on the USE, which sits an
                            // unbounded number of lines from the edit the
                            // message asks for — and the sibling `Vec[]` form
                            // already reports at its own literal, so pointing
                            // there makes the two halves of one rule agree.
                            //
                            // Falls back to the call span whenever provenance
                            // is not exact, which keeps a wrong caret off the
                            // screen: a confidently mis-placed underline reads
                            // worse than a correct but distant one.
                            let origin = match &object.kind {
                                ExprKind::Identifier(name) => {
                                    self.uninferrable_binding_origins.get(name).copied()
                                }
                                _ => None,
                            };
                            let msg = format!(
                                "method '{}' is not callable on this `{}`: the type \
                                 argument for `{}` could not be inferred — annotate the \
                                 value it came from, e.g. `let v: Vec[i64] = [];`",
                                method, type_name, param_name
                            );
                            self.type_error(
                                msg,
                                origin.unwrap_or(*span),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                        return Type::Error;
                    }
                    let bound_trait = bound.path.last().cloned().unwrap_or_default();
                    let detail = self.render_unsatisfied_bound_message(
                        &param_name,
                        &bound_trait,
                        &concrete,
                        &bound,
                    );
                    let via = trait_of_impl
                        .map(|t| format!(" (via `impl {} for {}`)", t, type_name))
                        .unwrap_or_default();
                    let msg = format!(
                        "method '{}' is not callable on this `{}`{}: {}",
                        method, type_name, via, detail
                    );
                    self.type_error(msg, *span, TypeErrorKind::NoMethodFound);
                    return Type::Error;
                }
                if (is_user_defined
                    || is_exhaustive_prelude
                    || method_on_other_specialization
                    || is_baked_stdlib_type)
                    && !self.type_has_comptime_derive(&type_name)
                {
                    let mut msg = format!("no method '{}' on type '{}'", method, type_name);
                    // Iterator adaptors/terminals (`map`/`filter`/`collect`/…)
                    // are not methods on a `Vec`/`VecDeque` directly — they live
                    // on `Iterator[T]`, reached via `.iter()`. A direct
                    // `v.map(...)` reaches here (silent `Type::Error` pre-fix,
                    // runs on no backend). When the absent method IS an iterator
                    // method, the actionable fix is the `.iter()` hop, not an
                    // edit-distance neighbour — surface that instead
                    // (B-2026-07-17-12). Scoped to the iterable sequence types;
                    // `Tensor`/`DataFrame` are not `.iter()`-adapted, so an
                    // absent method there falls to the edit-distance suggestion.
                    if matches!(type_name.as_str(), "Vec" | "VecDeque")
                        && Self::is_iterator_surface_method(method)
                    {
                        let recv = match &object.kind {
                            ExprKind::Identifier(n) => n.clone(),
                            _ => "xs".to_string(),
                        };
                        msg.push_str(&format!(
                            ": iterator adaptors/terminals require an explicit `.iter()` — write `{}.iter().{}(...)`",
                            recv, method
                        ));
                    } else {
                        let candidates = self.env.collect_method_names(&type_name, &[]);
                        let candidate_refs: Vec<&str> =
                            candidates.iter().map(String::as_str).collect();
                        if let Some(suggestion) =
                            crate::edit_distance::suggest_similar(method, &candidate_refs)
                        {
                            msg.push_str(&format!(", did you mean '{}'?", suggestion));
                        }
                    }
                    self.type_error(msg, *span, TypeErrorKind::NoMethodFound);
                }
                Type::Error
            }
        }
    }
}
