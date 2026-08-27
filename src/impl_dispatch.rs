//! Disambiguating dispatch names for impls whose head name is not a unique
//! dispatch identity — because they differ only in their target's type
//! arguments (B-2026-08-13-8), or only in the arguments of the PARAMETERIZED
//! TRAIT they implement (B-2026-08-27-1).
//!
//! An impl method is named `<target-head>.<method>` everywhere it is dispatched:
//! codegen emits the LLVM function under that name, the interpreter binds it in
//! its env under that key, and every call site rebuilds it from the receiver's
//! head name. The head name alone is not an identity when a program writes two
//! impls of one trait for two instantiations of one type:
//!
//! ```text
//! impl Zero for Vec[i64]    { fn describe(ref self) -> String { "VEC-I64" } }
//! impl Zero for Vec[String] { fn describe(ref self) -> String { "VEC-STR" } }
//! ```
//!
//! Both wanted to be `Vec.describe`. LLVM renamed the second, so `get_function`
//! handed the FIRST to every receiver; the interpreter's env kept the LAST and
//! handed that to every receiver. So the two backends printed opposite wrong
//! answers for the same program, and `karac check` accepted it — the typechecker
//! keys its impl table on `(name, args)` and had resolved both correctly. Only
//! the runtime NAME was lossy.
//!
//! The second axis is the same sentence with the arguments moved: two impls of
//! one PARAMETERIZED trait for a single target.
//!
//! ```text
//! impl From[ParseError] for AppError { fn from(e: ParseError) -> AppError { … } }
//! impl From[DbError]    for AppError { fn from(e: DbError)    -> AppError { … } }
//! ```
//!
//! Both wanted to be `AppError.from`, with the same measured split — and this
//! one is the shape design.md § Conversion Traits TEACHES, one impl per error
//! type flowing into one application error. When the two payloads had different
//! layouts it stopped being a wrong answer and became a type confusion: an
//! interpreter `unreachable!`, and on AOT a `String`'s three words read as three
//! `i64` fields, printing a raw heap pointer and exiting 0.
//!
//! What makes both axes one problem is that the typechecker had ALREADY chosen
//! correctly in each case — `impl_args_match` on the first, `find_from_impl`'s
//! source-type match on the second. Only the name was ambiguous.
//!
//! This module is the single place that decides (a) which impls need more than a
//! head name and (b) what that longer name is. All three phases call it on the
//! same AST, so there is exactly one rendering and no way for the emitters and
//! the dispatch sites to disagree — the failure mode the fix has to avoid is
//! naming an impl one way and looking it up another, which turns a wrong answer
//! into a link error rather than into a correct program.
//!
//! # Only on collision
//!
//! A qualified name is minted ONLY for a `(head, method)` that genuinely has two
//! or more impls with different IDENTITIES. Every other impl in the language
//! keeps the exact name it has today, which is what makes this additive: a
//! program that compiles now has no colliding group, so not one symbol moves.
//! In particular a lone `impl From[X] for T` — the common case by far — is not a
//! group, and the baked stdlib defines no `From` impls at all (its numeric
//! conversions are compiler builtins), so nothing outside a program that is
//! already broken can move.
//!
//! # Qualifying the TYPE segment, not the method
//!
//! The qualified name is `Vec[i64].describe`, not `Vec.describe$i64`. Dispatch
//! code all over codegen splits these keys on the LAST `.` to recover the method
//! segment — the chained-call span guard in `compile_method_call` is the sharp
//! case, since `recv.inner().outer()` aliases one span and the guard tells the
//! links apart by comparing that segment to the call's own method name. Widening
//! the type segment leaves every such split working unchanged; a suffix after
//! the method would silently break all of them.

use rustc_hash::FxHashMap;
use std::collections::HashMap;

use crate::ast::{GenericArg, Item, Program, TypeExpr, TypeKind};
use crate::resolver::SpanKey;
use crate::token::Span;

/// Qualified type segments for impls whose head name is not a unique dispatch
/// identity, keyed by the SPAN of the impl block's target type expression.
///
/// The span is the identity because it is the one handle all three phases
/// already have: codegen and the interpreter walk `Item::ImplBlock` directly and
/// the typechecker records it on `ImplInfo`. An impl absent from this map keeps
/// its head name, which is the overwhelmingly common case.
pub type ImplDispatchNames = FxHashMap<SpanKey, String>;

/// One impl block's contribution to a collision group: the span that identifies
/// it, and its rendered target (`None` when [`render_impl_target`] declined).
type GroupMember = (SpanKey, Option<String>);

/// Impl blocks sharing a `(type-head, method-name)` pair — the unit collisions
/// are decided over.
type CollisionGroups = HashMap<(String, String), Vec<GroupMember>>;

/// Render an impl target type expression to the string used as the type segment
/// of its dispatch key — `Vec[i64]`, `Map[String, i64]`, `Box[Vec[i64]]`.
///
/// Returns `None` for any shape this cannot render EXACTLY. That is deliberate
/// and load-bearing: a target carrying a const arg, a shape literal, or a
/// non-path form has no faithful spelling here, and inventing a lossy one would
/// let two distinct impls render identically — reintroducing the very collision
/// this module exists to remove, but now with the collision hidden behind a
/// name that looks disambiguated. A `None` anywhere in a group disables
/// qualification for that whole group (see [`collect_impl_dispatch_names`]),
/// which restores today's behaviour rather than risking a worse one.
pub fn render_impl_target(target: &TypeExpr) -> Option<String> {
    let TypeKind::Path(path) = &target.kind else {
        return None;
    };
    let head = path.segments.last()?.clone();
    let Some(args) = &path.generic_args else {
        return Some(head);
    };
    if args.is_empty() {
        return Some(head);
    }
    let mut rendered = Vec::with_capacity(args.len());
    for arg in args {
        match arg {
            GenericArg::Type(t) => rendered.push(render_impl_target(t)?),
            // Const and shape args are not part of the dispatch axis this
            // module disambiguates on, and rendering them would require
            // const-evaluating an expression here. Decline the whole target.
            GenericArg::Const(_) | GenericArg::Shape(_) => return None,
        }
    }
    Some(format!("{}[{}]", head, rendered.join(", ")))
}

/// The head name an impl target dispatches under today — the last path segment.
/// Mirrors codegen's `impl_target_name` and the interpreter's inline extraction;
/// `None` for a non-path target, which neither of those can name either.
fn impl_target_head(target: &TypeExpr) -> Option<String> {
    match &target.kind {
        TypeKind::Path(p) => p.segments.last().cloned(),
        _ => None,
    }
}

/// What an impl's implemented trait contributes to its dispatch identity.
///
/// The three cases must stay distinct, and conflating the last two is a silent
/// miscompile: "carries no arguments" means the impl is not on this axis and
/// keeps the name it has today, while "carries arguments that cannot be
/// rendered" means two such impls are INDISTINGUISHABLE and the program has to
/// be refused rather than guessed at.
enum TraitAxis {
    /// An inherent impl, or a plain `Ord` / `Display` — not on this axis.
    Absent,
    /// Parameterized, but an argument has no faithful spelling here (a const or
    /// shape arg — the same shapes [`render_impl_target`] declines).
    Declined,
    /// `From[ParseError]`, `TryFrom[A]`, …
    Rendered(String),
}

/// The implemented trait rendered WITH its generic arguments.
fn render_trait_with_args(trait_name: Option<&crate::ast::PathExpr>) -> TraitAxis {
    let Some(path) = trait_name else {
        return TraitAxis::Absent;
    };
    let Some(args) = path.generic_args.as_ref() else {
        return TraitAxis::Absent;
    };
    if args.is_empty() {
        return TraitAxis::Absent;
    }
    let Some(head) = path.segments.last().cloned() else {
        return TraitAxis::Declined;
    };
    let mut rendered = Vec::with_capacity(args.len());
    for arg in args {
        match arg {
            GenericArg::Type(t) => match render_impl_target(t) {
                Some(r) => rendered.push(r),
                None => return TraitAxis::Declined,
            },
            GenericArg::Const(_) | GenericArg::Shape(_) => return TraitAxis::Declined,
        }
    }
    TraitAxis::Rendered(format!("{}[{}]", head, rendered.join(", ")))
}

/// An impl's full dispatch IDENTITY: its rendered target, plus the implemented
/// trait's own arguments when it has any (`AppError@From[ParseError]`).
///
/// # Why the trait's arguments belong in the identity
///
/// The target alone is not an identity for a PARAMETERIZED trait. `impl
/// From[ParseError] for AppError` and `impl From[DbError] for AppError` are two
/// different impls that the language explicitly supports — design.md § Conversion
/// Traits teaches exactly this pair, one per error type flowing into one
/// application error — and both render the target `AppError`, so both wanted the
/// symbol `AppError.from` (B-2026-08-27-1).
///
/// That is the same lossy-name failure as the target-args axis this module was
/// written for, with the same measured shape: the interpreter's env kept the
/// LAST registered and codegen's module handed out the FIRST, so `--interp` and
/// `karac build` ran DIFFERENT conversion functions on the same program. With
/// mismatched payloads it stopped being a wrong answer and became a type
/// confusion — an interpreter `unreachable!`, and on AOT a `String`'s three
/// words read as three `i64` fields, printing a raw heap pointer and exiting 0.
///
/// # Why this is not the coherence case the module declines
///
/// [`collect_impl_dispatch_names`] leaves `impl Ord for Foo` alongside `impl
/// Display for Foo` alone, on the grounds that two same-named methods on one
/// concrete type are a coherence question rather than a naming one. That
/// reasoning holds only for UNPARAMETERIZED traits. `From[A]` and `From[B]` are
/// distinct trait instances, legal together, and distinguishable by argument —
/// the typechecker's `find_from_impl` already picks the right one. Nothing was
/// ambiguous except the name, which is precisely what this module exists to fix.
///
/// The `@` separator is deliberate: dispatch keys are split on the LAST `.` to
/// recover the method segment (see the module header), so the type segment must
/// stay dot-free.
fn render_impl_identity(imp: &crate::ast::ImplBlock) -> Option<String> {
    let target = render_impl_target(&imp.target_type)?;
    match render_trait_with_args(imp.trait_name.as_ref()) {
        TraitAxis::Absent => Some(target),
        TraitAxis::Rendered(tr) => Some(format!("{target}@{tr}")),
        // Declining the whole identity is what routes a pair like
        // `impl From[Array[i64, 3]] for T` / `impl From[Array[i64, 5]] for T`
        // into `unqualifiable_collision_groups`, which REFUSES the program.
        // Falling back to the bare target instead would render both impls
        // identically and hand them back the silent collision this module
        // exists to remove — measured on exactly that pair: an
        // `Array[i64, 3]` value converted through the `Array[i64, 5]` impl.
        TraitAxis::Declined => None,
    }
}

/// Compute the qualified type segment for every impl whose head name collides
/// with another impl's on some method.
///
/// Two impls collide when they share a head name AND both define a method of the
/// same name AND their rendered IDENTITIES differ. The last conjunct is what
/// keeps this narrow: `impl Ord for Foo` and `impl Display for Foo` both render
/// `Foo`, so they are not a collision this module can fix (a genuine same-name
/// method clash on one concrete type is a coherence question, not a naming one)
/// and they are left exactly as they are.
///
/// The identity is [`render_impl_identity`], not the target alone, so the axis
/// covers a parameterized trait as well: `impl From[A] for T` and `impl From[B]
/// for T` share a target but are two legal, distinguishable impls, and only
/// their NAME was ambiguous (B-2026-08-27-1).
///
/// If ANY impl in a colliding group fails to render, the entire group is left
/// unqualified. Half-qualifying a group is the one outcome worse than the bug:
/// the rendered members would move to new symbols while the unrendered one kept
/// the old name, so the group would still be ambiguous AND some call sites would
/// now look up a name nobody emitted.
pub fn collect_impl_dispatch_names(program: &Program) -> ImplDispatchNames {
    // (head, method) -> [(target-span, rendered-target)]
    let mut groups: CollisionGroups = CollisionGroups::new();
    for item in &program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(head) = impl_target_head(&imp.target_type) else {
            continue;
        };
        // A GENERIC impl (`impl[T] Zero for Vec[T]`) is not on this axis at all:
        // the monomorphizer already mangles its methods per instantiation, so it
        // owns no unmangled symbol to collide over. Including it here would
        // qualify a group that is not actually ambiguous.
        if imp.generic_params.is_some() {
            continue;
        }
        let key = SpanKey::from_span(&imp.target_type.span);
        let rendered = render_impl_identity(imp);
        for it in &imp.items {
            let crate::ast::ImplItem::Method(m) = it else {
                continue;
            };
            groups
                .entry((head.clone(), m.name.clone()))
                .or_default()
                .push((key, rendered.clone()));
        }
    }

    let mut out = ImplDispatchNames::default();
    for ((head, _method), members) in groups {
        if members.len() < 2 {
            continue;
        }
        // All renderable, and at least two genuinely distinct targets.
        let mut names = Vec::with_capacity(members.len());
        for (_, rendered) in &members {
            match rendered {
                Some(r) => names.push(r.clone()),
                None => {
                    names.clear();
                    break;
                }
            }
        }
        if names.is_empty() || names.iter().all(|n| *n == names[0]) {
            continue;
        }
        for (span, rendered) in members {
            let rendered = rendered.expect("checked renderable above");
            // A target that renders to its bare head (`impl Zero for Foo` in a
            // group alongside `impl Zero for Foo[i64]`) keeps the head name, so
            // it stays on the symbol it already owns.
            if rendered != head {
                out.insert(span, rendered);
            }
        }
    }
    out
}

/// The colliding groups this module CANNOT name apart: two or more impls that
/// share a head name and a method name, whose targets are not all identical,
/// and at least one of which [`render_impl_target`] declines (today: a target
/// carrying a CONST argument — `Array[i64, 3]`, `Tensor[f32, [4]]`).
///
/// Returns one entry per member, `(target span, head, method)`, so a caller can
/// point at every impl in the group.
///
/// # Why this needs reporting rather than a silent fallback
///
/// [`collect_impl_dispatch_names`] leaves such a group unqualified on purpose —
/// half-qualifying it is worse, as that function's doc explains. But leaving it
/// unqualified is not harmless: the group collapses onto one `Head.method`
/// symbol, and the two backends then disagree about WHICH member owns it.
/// Measured on two `impl Head for Tensor[f32, [2]]` / `Tensor[f64, [2]]` blocks,
/// with a `Tensor[f32, [2]]` receiver: `--interp` printed `F64` and `karac
/// build` printed `F32` — opposite wrong answers, and `karac check` accepted
/// the program. That is the B-2026-08-13-8 failure this module exists to
/// prevent, surviving in the one corner it cannot rename its way out of.
///
/// So the answer is to refuse the program. A rejection an author can act on
/// beats two silently disagreeing binaries, and it costs nothing for the
/// overwhelmingly common case: a group needs two impls of one trait method on
/// one head with different const-carrying targets before this fires at all.
/// `(head, method)` -> the impls in that group, as `(target span, rendered
/// target)`. The span-carrying sibling of [`CollisionGroups`], which keys by
/// `SpanKey` because it only needs to look a rendering up; this one has to
/// point a diagnostic at each member.
type SpannedCollisionGroups = HashMap<(String, String), Vec<(Span, Option<String>)>>;

pub fn unqualifiable_collision_groups(program: &Program) -> Vec<(Span, String, String)> {
    // (head, method) -> [(target span, rendered target)]
    let mut groups: SpannedCollisionGroups = SpannedCollisionGroups::new();
    for item in &program.items {
        let Item::ImplBlock(imp) = item else { continue };
        let Some(head) = impl_target_head(&imp.target_type) else {
            continue;
        };
        // Same exclusion as `collect_impl_dispatch_names`: a generic impl is
        // mangled per instantiation and owns no unmangled symbol to collide over.
        if imp.generic_params.is_some() {
            continue;
        }
        let rendered = render_impl_identity(imp);
        for it in &imp.items {
            let crate::ast::ImplItem::Method(m) = it else {
                continue;
            };
            groups
                .entry((head.clone(), m.name.clone()))
                .or_default()
                .push((imp.target_type.span, rendered.clone()));
        }
    }

    let mut out = Vec::new();
    for ((head, method), members) in groups {
        if members.len() < 2 {
            continue;
        }
        // Every target identical is not a collision this module is about — it
        // is a coherence question, reported elsewhere.
        let all_same = members
            .iter()
            .all(|(_, r)| *r == members[0].1 && r.is_some());
        if all_same {
            continue;
        }
        if members.iter().all(|(_, r)| r.is_some()) {
            continue; // renderable: `collect_impl_dispatch_names` qualifies it
        }
        for (span, _) in members {
            out.push((span, head.clone(), method.clone()));
        }
    }
    // Deterministic order for reproducible diagnostics.
    out.sort_by_key(|(sp, head, m)| (sp.offset, sp.length, head.clone(), m.clone()));
    out
}

/// The dispatch type segment for one impl block: its qualified name when the
/// group needs one, else its head name.
pub fn impl_dispatch_segment(target: &TypeExpr, names: &ImplDispatchNames) -> Option<String> {
    let key = SpanKey::from_span(&target.span);
    if let Some(q) = names.get(&key) {
        return Some(q.clone());
    }
    impl_target_head(target)
}

/// Does an impl head named `name` keep its concrete type arguments on `self`?
///
/// A builtin container's arguments ARE its element types, so dropping them
/// leaves the method body with a receiver it can barely use: `impl Trait for
/// Slice[i64]` typed `self` as an args-less `Slice`, and `self[0]` was then
/// rejected with "'Slice' does not support indexing with []" — whose own help
/// text lists `Slice[T]` as indexable. A USER type is the opposite case: the
/// args there are the impl's own generic params (`impl Foo[T]`), and erasing
/// them to the head name is what makes `self` usable as `Foo`.
///
/// # Why this predicate is shared rather than spelled twice
///
/// Two places decide it — `TypeChecker::check_impl_block` (which types `self`
/// for the body) and `make_impl_method_function` (which synthesizes the `self`
/// parameter codegen registers side-tables from). They were hand-mirrored
/// name lists, and both grew one head at a time as each was hit: `Column` and
/// `Tensor` for a reduction intercept, then `Vec` and `VecDeque` for a fold.
/// The set that resulted was an accident of discovery order, so `Map`, `Set`,
/// `SortedMap`, `SortedSet`, `Option`, `Result`, `Slice`, `Array` and `Vector`
/// were all still dropping their args — the front end rejecting the body, and
/// codegen, on the shapes the front end did admit, dying on "Index operator
/// applied to non-array type" for want of a registered element type
/// (B-2026-08-17-44). One list means the two can no longer drift apart.
///
/// The caller pairs this with a concreteness check: a head whose args mention
/// a type param has something generic to erase and takes the erasing path.
pub fn impl_head_keeps_type_args(name: &str) -> bool {
    matches!(
        name,
        "Vec"
            | "VecDeque"
            | "Map"
            | "SortedMap"
            | "Set"
            | "SortedSet"
            | "Column"
            | "Tensor"
            | "Option"
            | "Result"
            | "Slice"
            | "Array"
            | "Vector"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rendered targets for every impl the collision pass qualifies, sorted so
    /// the assertions do not depend on `HashMap` iteration order.
    fn qualified(src: &str) -> Vec<String> {
        let program = crate::parse(src).program;
        let mut names: Vec<String> = collect_impl_dispatch_names(&program)
            .into_values()
            .collect();
        names.sort();
        names
    }

    #[test]
    fn single_impl_is_never_qualified() {
        // The additive property this fix rests on: a program with no colliding
        // group keeps every symbol it has today. If this ever fails, the change
        // has stopped being additive and every existing program is at risk.
        assert!(qualified(
            "trait Z { fn d(ref self) -> String; }\n\
             impl Z for Vec[i64] { fn d(ref self) -> String { return f\"a\"; } }"
        )
        .is_empty());
    }

    #[test]
    fn distinct_heads_are_never_qualified() {
        // `Map` and `Set` are already distinguishable by head name — qualifying
        // them would move symbols for no reason.
        assert!(qualified(
            "trait Z { fn d(ref self) -> String; }\n\
             impl Z for Map[String, i64] { fn d(ref self) -> String { return f\"a\"; } }\n\
             impl Z for Set[i64] { fn d(ref self) -> String { return f\"b\"; } }"
        )
        .is_empty());
    }

    #[test]
    fn same_head_different_args_are_qualified() {
        assert_eq!(
            qualified(
                "trait Z { fn d(ref self) -> String; }\n\
                 impl Z for Vec[i64] { fn d(ref self) -> String { return f\"a\"; } }\n\
                 impl Z for Vec[String] { fn d(ref self) -> String { return f\"b\"; } }"
            ),
            vec!["Vec[String]".to_string(), "Vec[i64]".to_string()],
        );
    }

    #[test]
    fn two_impls_of_one_parameterized_trait_are_qualified() {
        // B-2026-08-27-1. Both target `AppError`, so the TARGET axis renders
        // them identically and the group was skipped as a coherence question —
        // which it is not: `From[A]` and `From[B]` are distinct trait instances
        // the language supports together, and `find_from_impl` already tells
        // them apart. Only the name collapsed.
        assert_eq!(
            qualified(
                "struct A { v: i64 }\n\
                 struct B { v: i64 }\n\
                 struct T { v: i64 }\n\
                 impl From[A] for T { fn from(x: A) -> T { return T { v: x.v }; } }\n\
                 impl From[B] for T { fn from(x: B) -> T { return T { v: x.v }; } }"
            ),
            vec!["T@From[A]".to_string(), "T@From[B]".to_string()],
        );
    }

    #[test]
    fn a_lone_parameterized_trait_impl_is_never_qualified() {
        // The additive property, on the new axis. One `impl From[A] for T` is
        // not a group, so it keeps the `T.from` symbol it owns today. If this
        // fails, every single-impl program in existence has moved.
        assert!(qualified(
            "struct A { v: i64 }\n\
             struct T { v: i64 }\n\
             impl From[A] for T { fn from(x: A) -> T { return T { v: x.v }; } }"
        )
        .is_empty());
    }

    #[test]
    fn an_unparameterized_trait_pair_stays_a_coherence_question() {
        // Two same-named methods on one concrete type via two ARGUMENT-LESS
        // traits render identically and must still be left alone — renaming
        // them would be this module overreaching into coherence, which it
        // deliberately does not do. The trait-args axis widened the identity
        // only for traits that HAVE arguments.
        assert!(qualified(
            "trait P { fn d(ref self) -> String; }\n\
             trait Q { fn d(ref self) -> String; }\n\
             struct Foo { v: i64 }\n\
             impl P for Foo { fn d(ref self) -> String { return f\"p\"; } }\n\
             impl Q for Foo { fn d(ref self) -> String { return f\"q\"; } }"
        )
        .is_empty());
    }

    #[test]
    fn the_two_axes_compose_on_one_impl() {
        // A parameterized trait AND a generic target: the identity has to carry
        // both, or `From[A] for Box[i64]` and `From[B] for Box[i64]` would
        // collapse back together the moment the target args matched.
        assert_eq!(
            qualified(
                "struct A { v: i64 }\n\
                 struct B { v: i64 }\n\
                 impl From[A] for Vec[i64] { fn from(x: A) -> Vec[i64] { return Vec.new(); } }\n\
                 impl From[B] for Vec[i64] { fn from(x: B) -> Vec[i64] { return Vec.new(); } }"
            ),
            vec![
                "Vec[i64]@From[A]".to_string(),
                "Vec[i64]@From[B]".to_string()
            ],
        );
    }

    #[test]
    fn multi_arg_and_nested_targets_render_in_full() {
        // `Map[String, i64]` vs `Map[i64, String]` differ only by ARG ORDER, so
        // a renderer that dropped or sorted args would collapse them right back
        // together — with the collapse now hidden behind a name that looks
        // disambiguated.
        assert_eq!(
            qualified(
                "trait Z { fn d(ref self) -> String; }\n\
                 impl Z for Map[String, i64] { fn d(ref self) -> String { return f\"a\"; } }\n\
                 impl Z for Map[i64, String] { fn d(ref self) -> String { return f\"b\"; } }"
            ),
            vec![
                "Map[String, i64]".to_string(),
                "Map[i64, String]".to_string()
            ],
        );
    }

    #[test]
    fn only_the_colliding_method_forces_qualification() {
        // `other` exists on one impl only; `d` collides. Both methods of a
        // qualified impl move together — they share the impl's symbol prefix —
        // so what is asserted here is that the impl is qualified at all, and
        // that the non-colliding sibling impl is left alone.
        assert_eq!(
            qualified(
                "trait Z { fn d(ref self) -> String; }\n\
                 trait Y { fn e(ref self) -> String; }\n\
                 impl Z for Vec[i64] { fn d(ref self) -> String { return f\"a\"; } }\n\
                 impl Z for Vec[String] { fn d(ref self) -> String { return f\"b\"; } }\n\
                 impl Y for Set[i64] { fn e(ref self) -> String { return f\"c\"; } }"
            ),
            vec!["Vec[String]".to_string(), "Vec[i64]".to_string()],
        );
    }

    #[test]
    fn generic_impls_are_left_alone() {
        // A generic impl's methods are mangled per instantiation by the
        // monomorphizer, so it owns no unmangled symbol to collide over.
        // Qualifying it would rename a symbol nothing looks up.
        assert!(qualified(
            "trait Z { fn d(ref self) -> String; }\n\
             impl[T] Z for Vec[T] { fn d(ref self) -> String { return f\"a\"; } }"
        )
        .is_empty());
    }

    #[test]
    fn bare_head_member_of_a_group_keeps_its_head_name() {
        // `impl Z for Foo` renders as `Foo` — identical to the name it already
        // owns — so it must NOT be moved, or its existing symbol would vanish
        // while every call site still asked for `Foo.d`.
        let names = qualified(
            "struct Foo { }\n\
             trait Z { fn d(ref self) -> String; }\n\
             impl Z for Foo { fn d(ref self) -> String { return f\"a\"; } }\n\
             impl Z for Foo[i64] { fn d(ref self) -> String { return f\"b\"; } }",
        );
        assert_eq!(names, vec!["Foo[i64]".to_string()]);
    }

    #[test]
    fn render_declines_const_args() {
        // A const arg cannot be rendered faithfully without const-evaluating it
        // here, so the target declines — and a declining member disables its
        // whole group rather than half-qualifying it.
        assert!(qualified(
            "trait Z { fn d(ref self) -> String; }\n\
             impl Z for Array[i64, 3] { fn d(ref self) -> String { return f\"a\"; } }\n\
             impl Z for Array[i64, 4] { fn d(ref self) -> String { return f\"b\"; } }"
        )
        .is_empty());
    }

    #[test]
    fn a_const_arg_in_the_trait_position_also_declines_and_is_reported() {
        // B-2026-08-27-1's conservative corner. Both impls share the target `T`
        // AND carry a const argument inside the trait's own arguments, so
        // neither axis can name them apart.
        //
        // The trap this pins is the difference between "the trait has no
        // arguments" and "the trait's arguments cannot be rendered". Collapsing
        // the second case back to the bare target would render both impls
        // identically, and the group would then look like a coherence question
        // and be skipped in silence — which is the original bug, restored in a
        // corner. Measured on exactly this pair before the distinction existed:
        // an `Array[i64, 3]` value converted through the `Array[i64, 5]` impl,
        // with no diagnostic.
        let src = "struct T { v: i64 }\n\
                   impl From[Array[i64, 3]] for T { fn from(x: Array[i64, 3]) -> T { return T { v: 1 }; } }\n\
                   impl From[Array[i64, 5]] for T { fn from(x: Array[i64, 5]) -> T { return T { v: 2 }; } }";
        // Not qualified — there is no faithful name to qualify with …
        assert!(qualified(src).is_empty());
        // … so the group must instead be REPORTED, one entry per member, which
        // is what turns the silent wrong conversion into a refusal.
        let program = crate::parse(src).program;
        let groups = unqualifiable_collision_groups(&program);
        assert_eq!(
            groups.len(),
            2,
            "both impls must be pointed at, got {groups:?}"
        );
        assert!(groups.iter().all(|(_, head, m)| head == "T" && m == "from"));
    }
}
