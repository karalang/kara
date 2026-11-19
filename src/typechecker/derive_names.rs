//! `#[derive(...)]`'s list is checked against what derive can do
//! (design.md §9 Derive, B-2026-11-19-13): a name outside the closed table
//! that no `comptime fn derive_<name>` backs is E_DERIVE_UNKNOWN, with the
//! nearest derivable name offered; a derive on an item that is not a struct,
//! enum or distinct type is E_DERIVE_TARGET; a trait listed twice is
//! E_DERIVE_DUPLICATE, whose fix removes the second.

use super::{FixIt, TypeChecker, TypeErrorKind};
use crate::ast::*;
use crate::token::Span;

/// The traits the compiler derives itself. `Drop` is refused by its own
/// rule (`validate_derive_lists`), so it is not unknown here.
const BUILT_IN: &[&str] = &[
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Clone",
    "Copy",
    "Default",
    "Display",
    "Debug",
    "Serialize",
    "Deserialize",
    "Arithmetic",
    "Pod",
    "Drop",
];

/// The derive's trait name and the span of that name, for one argument.
fn derive_arg_name(arg: &AttrArg) -> Option<(&str, Span)> {
    match &arg.value {
        Some(Expr {
            kind: ExprKind::Identifier(name),
            span,
            ..
        }) => Some((name.as_str(), *span)),
        Some(Expr {
            kind: ExprKind::Call { callee, .. },
            ..
        }) => match &callee.kind {
            ExprKind::Identifier(name) => Some((name.as_str(), callee.span)),
            _ => None,
        },
        _ => None,
    }
}

impl TypeChecker<'_> {
    pub(super) fn validate_derive_names(&mut self) {
        let mut found: Vec<(String, Span, Option<FixIt>)> = Vec::new();
        for item in &self.program.items {
            let (attrs, what): (&[Attribute], Option<&str>) = match item {
                Item::StructDef(d) => (&d.attributes, None),
                Item::EnumDef(d) => (&d.attributes, None),
                Item::DistinctType(d) => (&d.attributes, None),
                Item::Function(f) => (&f.attributes, Some("a function")),
                Item::ConstDecl(c) => (&c.attributes, Some("a `const`")),
                Item::TraitDef(t) => (&t.attributes, Some("a trait")),
                Item::MarkerTrait(t) => (&t.attributes, Some("a marker trait")),
                Item::TraitAlias(t) => (&t.attributes, Some("a trait alias")),
                Item::UnionDef(u) => (&u.attributes, Some("a union")),
                Item::ImplBlock(i) => (&i.attributes, Some("an impl block")),
                Item::TypeAlias(t) => (&t.attributes, Some("a type alias")),
                Item::ModuleBinding(b) => (&b.attributes, Some("a module-level `let`")),
                Item::ExternFunction(f) => (&f.attributes, Some("an extern function")),
                _ => continue,
            };
            for attr in attrs.iter().filter(|a| a.is_bare("derive")) {
                if let Some(what) = what {
                    found.push((
                        format!(
                            "error[E_DERIVE_TARGET]: `#[derive(..)]` on {what}: derive generates \
                             trait impls from a type's fields, so it goes on a `struct`, an \
                             `enum` or a distinct type; remove the attribute, or write the impl \
                             by hand (design.md §9 Derive)"
                        ),
                        attr.span,
                        None,
                    ));
                    continue;
                }
                let mut seen: Vec<&str> = Vec::new();
                let mut prev_end: Option<usize> = None;
                for arg in &attr.args {
                    let Some((name, span)) = derive_arg_name(arg) else {
                        continue;
                    };
                    let arg_end = arg.span.offset + arg.span.length;
                    if seen.contains(&name) {
                        // `, PartialEq`: from the end of the argument before
                        // it to its own end.
                        let fix = prev_end.filter(|&p| p <= arg.span.offset).map(|p| FixIt {
                            span: Span {
                                offset: p,
                                length: arg_end.saturating_sub(p),
                                ..arg.span
                            },
                            replacement: String::new(),
                        });
                        found.push((
                            format!(
                                "error[E_DERIVE_DUPLICATE]: `{name}` is listed twice in this \
                                 derive; a type derives a trait once, so remove the second"
                            ),
                            span,
                            fix,
                        ));
                        prev_end = Some(arg_end);
                        continue;
                    }
                    seen.push(name);
                    prev_end = Some(arg_end);
                    if BUILT_IN.contains(&name)
                        || self.is_comptime_derive_fn(&format!(
                            "derive_{}",
                            crate::comptime::to_snake_case(name)
                        ))
                    {
                        continue;
                    }
                    let near = crate::edit_distance::suggest_similar(name, BUILT_IN)
                        .filter(|n| n != "Drop");
                    let message = match &near {
                        Some(n) => format!(
                            "error[E_DERIVE_UNKNOWN]: `{name}` is not derivable: did you mean \
                             `{n}`? derive takes the built-in traits (PartialEq, Eq, \
                             PartialOrd, Ord, Hash, Clone, Copy, Default, Display, Debug, \
                             Serialize, Deserialize, Arithmetic, Pod) or a trait a \
                             `comptime fn derive_{}` generates (design.md §9 Derive)",
                            crate::comptime::to_snake_case(name)
                        ),
                        None => format!(
                            "error[E_DERIVE_UNKNOWN]: `{name}` is not derivable: derive takes \
                             the built-in traits (PartialEq, Eq, PartialOrd, Ord, Hash, Clone, \
                             Copy, Default, Display, Debug, Serialize, Deserialize, Arithmetic, \
                             Pod) or a trait a `comptime fn derive_{}` generates; remove it, \
                             write the impl by hand, or define that comptime fn \
                             (design.md §9 Derive)",
                            crate::comptime::to_snake_case(name)
                        ),
                    };
                    let fix = near.map(|n| FixIt {
                        span,
                        replacement: n,
                    });
                    found.push((message, span, fix));
                }
            }
        }
        for (message, span, fix) in found {
            match fix {
                Some(fix) => {
                    self.type_error_with_fix_it(message, span, TypeErrorKind::TypeMismatch, fix)
                }
                None => self.type_error(message, span, TypeErrorKind::TypeMismatch),
            }
        }
    }
}
