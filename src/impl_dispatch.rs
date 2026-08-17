//! Disambiguating dispatch names for impls that differ only in their target's
//! type arguments (B-2026-08-13-8).
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
//! or more impls with different targets. Every other impl in the language keeps
//! the exact name it has today, which is what makes this additive: a program
//! that compiles now has no colliding group, so not one symbol moves.
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

/// Compute the qualified type segment for every impl whose head name collides
/// with another impl's on some method.
///
/// Two impls collide when they share a head name AND both define a method of the
/// same name AND their rendered targets differ. The last conjunct is what keeps
/// this narrow: `impl Ord for Foo` and `impl Display for Foo` both render `Foo`,
/// so they are not a collision this module can fix (a genuine same-name method
/// clash on one concrete type is a coherence question, not a naming one) and
/// they are left exactly as they are.
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
        let rendered = render_impl_target(&imp.target_type);
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

/// The dispatch type segment for one impl block: its qualified name when the
/// group needs one, else its head name.
pub fn impl_dispatch_segment(target: &TypeExpr, names: &ImplDispatchNames) -> Option<String> {
    let key = SpanKey::from_span(&target.span);
    if let Some(q) = names.get(&key) {
        return Some(q.clone());
    }
    impl_target_head(target)
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
}
