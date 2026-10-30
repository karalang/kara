//! `unused_effect` (design.md § Effects, B-2026-10-30-22): a declaration may
//! name more than its body performs, so a library can reserve an effect, and
//! this lint, allowed by default, reports each such effect.
//!
//! The declared set seeds a function's inferred effects (a declaration is
//! trusted for what the body cannot show), so the inferred set always holds
//! the declared one and cannot tell them apart. The body's own set is taken
//! here instead: the settled effects of every callee it reaches, as
//! inference unions them, without the seed. A call back into the function
//! adds nothing, and one into a mutually recursive partner brings that
//! partner's set, the declaration included, so a declaration on a cycle is
//! never reported.
//!
//! The checker only finds the candidates. Whether the lint is on comes from
//! the function's or its `impl` block's lint attribute, else from the
//! command line and the manifest, which the CLI holds; it reports them with
//! a fix that writes the clause without the unused effects
//! ([`UnusedEffect`]).

use std::collections::HashSet;

use crate::ast::*;
use crate::intern::Symbol;
use crate::lints::LintLevel;
use crate::token::Span;

use super::{DeclaredEffects, Effect};

/// One function whose declaration names effects its body never performs.
#[derive(Debug, Clone)]
pub struct UnusedEffect {
    pub function: String,
    /// The `with ...` clause, from `with` to its last item.
    pub clause: Span,
    /// The unused effects, as written (`writes(Log)`, a group's name).
    pub unused: Vec<String>,
    /// The clause without them (`with reads(Clock)`), or empty when none is
    /// left. `None` when the remainder cannot be written back as it was (a
    /// resource with an argument).
    pub replacement: Option<String>,
    /// `#[allow]` / `#[warn]` / `#[deny]` / `#[expect]` of `unused_effect`
    /// on the function, else on its `impl` block.
    pub source_level: Option<LintLevel>,
}

pub const LINT_NAME: &str = "unused_effect";

/// The level a lint attribute on `attrs` gives `unused_effect`.
fn attr_level(attrs: &[Attribute]) -> Option<LintLevel> {
    attrs.iter().rev().find_map(|a| {
        let level = ["allow", "warn", "deny", "expect"]
            .into_iter()
            .find(|k| a.is_bare(k))?;
        let named = a.args.iter().any(|arg| {
            arg.name.as_deref() == Some(LINT_NAME)
                || matches!(
                    arg.value.as_ref().map(|v| &v.kind),
                    Some(ExprKind::Identifier(n)) if n == LINT_NAME
                )
        });
        if !named {
            return None;
        }
        Some(match level {
            "allow" => LintLevel::Allow,
            "warn" => LintLevel::Warn,
            "deny" => LintLevel::Deny,
            _ => LintLevel::Expect,
        })
    })
}

/// One piece of a clause the lint can keep or drop: a resource of a verb
/// (or the verb itself when it names none), or a group.
struct Unit<'p> {
    item: usize,
    resource: Option<&'p Resource>,
    text: String,
    effects: HashSet<Effect>,
}

impl<'a> super::EffectChecker<'a> {
    pub(crate) fn find_unused_effects(&self) -> Vec<UnusedEffect> {
        let mut out = Vec::new();
        for item in &self.program.items {
            match item {
                Item::Function(f) => {
                    let sym = self.interner.intern(&f.name);
                    out.extend(self.unused_in(sym, f, attr_level(&f.attributes)));
                }
                Item::ImplBlock(imp) => {
                    let TypeKind::Path(p) = &imp.target_type.kind else {
                        continue;
                    };
                    let Some(type_name) = p.segments.last() else {
                        continue;
                    };
                    let impl_level = attr_level(&imp.attributes);
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let key = self.interner.dotted_str(type_name, &m.name);
                            out.extend(self.unused_in(
                                key,
                                m,
                                attr_level(&m.attributes).or(impl_level),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Whether the body's set performs something `d` declares: `writes(R)`
    /// covers a read of `R`, and `blocks(R)` a suspension on it.
    fn covers(d: &Effect, body: &HashSet<Effect>) -> bool {
        body.iter().any(|e| {
            e.resource == d.resource
                && (e.verb == d.verb
                    || matches!(
                        (&d.verb, &e.verb),
                        (EffectVerbKind::Writes, EffectVerbKind::Reads)
                            | (EffectVerbKind::Blocks, EffectVerbKind::Suspends)
                    ))
        })
    }

    fn unused_in(
        &self,
        sym: Symbol,
        f: &Function,
        source_level: Option<LintLevel>,
    ) -> Option<UnusedEffect> {
        if f.stdlib_origin {
            return None;
        }
        let list = f.effects.as_ref()?;
        if !matches!(
            self.declared_effects.get(&sym),
            Some(DeclaredEffects::Explicit(_))
        ) {
            return None;
        }
        let mut body: HashSet<Effect> = HashSet::new();
        for (callee, _) in self.body_calls(sym, &f.body) {
            if callee != sym {
                body.extend(self.callee_effect_sets(callee).iter().cloned());
            }
        }
        let mut units: Vec<Unit<'_>> = Vec::new();
        for (i, item) in list.items.iter().enumerate() {
            match item {
                EffectItem::Verb(v) if v.resources.is_empty() => units.push(Unit {
                    item: i,
                    resource: None,
                    text: super::verb_name(&v.kind),
                    effects: self.unit_effects(item.clone(), list.span),
                }),
                EffectItem::Verb(v) => {
                    for r in &v.resources {
                        let one = EffectItem::Verb(EffectVerb {
                            kind: v.kind.clone(),
                            resources: vec![r.clone()],
                            span: v.span,
                        });
                        units.push(Unit {
                            item: i,
                            resource: Some(r),
                            text: format!("{}({})", super::verb_name(&v.kind), r.path.join(".")),
                            effects: self.unit_effects(one, list.span),
                        });
                    }
                }
                EffectItem::Group(g) => units.push(Unit {
                    item: i,
                    resource: None,
                    text: g.clone(),
                    effects: self.unit_effects(item.clone(), list.span),
                }),
                EffectItem::Polymorphic | EffectItem::Variable(_) => return None,
            }
        }
        let unused: Vec<&Unit<'_>> = units
            .iter()
            .filter(|u| {
                let reportable: Vec<&Effect> = u
                    .effects
                    .iter()
                    .filter(|e| {
                        !self.is_transparent_verb(&e.verb) && !self.is_default_permitted_effect(e)
                    })
                    .collect();
                !reportable.is_empty() && !reportable.iter().any(|d| Self::covers(d, &body))
            })
            .collect();
        if unused.is_empty() {
            return None;
        }
        Some(UnusedEffect {
            function: self.interner.resolve(sym).to_string(),
            clause: list.span,
            unused: unused.iter().map(|u| u.text.clone()).collect(),
            replacement: kept_clause(list, &units, &unused),
            source_level,
        })
    }

    /// The effects one clause item resolves to.
    fn unit_effects(&self, item: EffectItem, span: Span) -> HashSet<Effect> {
        self.resolve_effect_list_to_set(
            &EffectList {
                items: vec![item],
                span,
            },
            None,
        )
    }
}

/// `list` written without the `unused` units: `with` and what is left, or
/// empty when nothing is. `None` when a kept resource has an argument, which
/// [`crate::formatter::render_effect_list`] does not write.
fn kept_clause(list: &EffectList, units: &[Unit<'_>], unused: &[&Unit<'_>]) -> Option<String> {
    let dropped = |u: &Unit<'_>| unused.iter().any(|x| std::ptr::eq(*x, u));
    let mut items: Vec<EffectItem> = Vec::new();
    for (i, item) in list.items.iter().enumerate() {
        let mine: Vec<&Unit<'_>> = units.iter().filter(|u| u.item == i).collect();
        if mine.iter().all(|u| dropped(u)) {
            continue;
        }
        match item {
            EffectItem::Verb(v) if !v.resources.is_empty() => {
                let resources: Vec<Resource> = mine
                    .iter()
                    .filter(|u| !dropped(u))
                    .filter_map(|u| u.resource.cloned())
                    .collect();
                if resources.iter().any(|r| r.param.is_some()) {
                    return None;
                }
                items.push(EffectItem::Verb(EffectVerb {
                    resources,
                    ..v.clone()
                }));
            }
            other => items.push(other.clone()),
        }
    }
    if items.is_empty() {
        return Some(String::new());
    }
    let kept = EffectList {
        items,
        span: list.span,
    };
    Some(format!(
        "with {}",
        crate::formatter::render_effect_list(&kept)
    ))
}
