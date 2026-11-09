//! Inlay hints (v1 checklist): what the checker inferred and the source does
//! not say. A `let` without a type annotation shows the type its binding
//! got, and a private function without a `with` clause shows the effects it
//! inferred (design.md §3: effects are inferred for private functions and
//! declared for public ones). Read off the same check that produced the
//! buffer's diagnostics, so the editor gets them with no second check
//! ([`super::editor_incremental::DocumentChecker::inlay_hints`]).

use super::Pipeline;
use crate::ast::{Block, ImplItem, Item, PatternKind, Stmt, StmtKind, TraitItem};
use crate::span_visitor::{walk_item_mut_with, MutVisitor};
use crate::token::Span;
use crate::typechecker::types::type_display;

/// What an [`InlayHint`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlayHintKind {
    /// A `let` binding's inferred type, after its name.
    Type,
    /// A private function's inferred effects, after its signature.
    Effects,
}

/// Text an editor shows inline at a byte offset of the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub offset: usize,
    /// `: Vec[String]` for a type; `with writes(Console)` for effects.
    pub label: String,
    pub kind: InlayHintKind,
}

/// The hints of the buffer `pipeline` checked, in source order.
pub(super) fn hints_of(pipeline: &Pipeline, source: &str) -> Vec<InlayHint> {
    let (Some(typed), Some(effects)) = (pipeline.typed.as_ref(), pipeline.effects.as_ref()) else {
        return Vec::new();
    };
    let text = pipeline.source.as_deref().unwrap_or(source);
    let user_len = pipeline
        .library_len
        .unwrap_or(text.len())
        .min(text.len())
        .min(source.len());
    let text = &text[..user_len];
    let mut out = Vec::new();
    let mut bodies: Vec<&Item> = Vec::new();
    for item in &pipeline.parsed.program.items {
        if item.span().offset >= user_len {
            continue;
        }
        match item {
            Item::Function(f) => {
                effect_hint(
                    &mut out,
                    text,
                    effects,
                    &f.name,
                    f.effects.is_some() || f.is_pub,
                    &f.body,
                );
            }
            Item::ImplBlock(ib) => {
                let span = &ib.target_type.span;
                let Some(target) = text.get(span.offset..span.offset + span.length) else {
                    continue;
                };
                let head = target.split('[').next().unwrap_or(target).trim();
                // A trait's methods are as public as the trait.
                let in_trait = ib.trait_name.is_some();
                for it in &ib.items {
                    if let ImplItem::Method(f) = it {
                        let key = format!("{head}.{}", f.name);
                        let declared = f.effects.is_some() || f.is_pub || in_trait;
                        effect_hint(&mut out, text, effects, &key, declared, &f.body);
                    }
                }
            }
            Item::TraitDef(td) => {
                for ti in &td.items {
                    if let TraitItem::Method(m) = ti {
                        if let Some(body) = &m.body {
                            let key = format!("{}.{}", td.name, m.name);
                            let declared = m.effects.is_some() || td.is_pub;
                            effect_hint(&mut out, text, effects, &key, declared, body);
                        }
                    }
                }
            }
            _ => continue,
        }
        bodies.push(item);
    }
    struct Lets<'a> {
        text: &'a str,
        found: Vec<(usize, crate::ids::NodeId)>,
    }
    impl MutVisitor for Lets<'_> {
        fn span(&mut self, _: &mut Span) {}
        fn stmt(&mut self, s: &mut Stmt) {
            let StmtKind::Let {
                pattern, ty: None, ..
            } = &s.kind
            else {
                return;
            };
            let PatternKind::Binding(name) = &pattern.kind else {
                return;
            };
            if name.starts_with("__") {
                return;
            }
            let end = pattern.span.offset + pattern.span.length;
            // Written `let name` / `let mut name`: not a binding a
            // desugaring made.
            let Some(written) = self.text.get(pattern.span.offset..end) else {
                return;
            };
            if !written.ends_with(name.as_str()) {
                return;
            }
            let lead = self.text[..pattern.span.offset].trim_end();
            let lead = lead.strip_suffix("mut").map_or(lead, str::trim_end);
            let after_let = lead
                .strip_suffix("let")
                .is_some_and(|l| !l.ends_with(|c: char| c.is_alphanumeric() || c == '_'));
            if after_let {
                self.found.push((end, pattern.id));
            }
        }
    }
    let mut lets = Lets {
        text,
        found: Vec::new(),
    };
    for item in bodies {
        walk_item_mut_with(&mut item.clone(), &mut lets);
    }
    for (at, id) in lets.found {
        let Some((ty, _)) = typed.node_types.get(&id) else {
            continue;
        };
        let shown = type_display(ty);
        if shown.contains("<error>") || shown.contains("?T") {
            continue;
        }
        out.push(InlayHint {
            offset: at,
            label: format!(": {shown}"),
            kind: InlayHintKind::Type,
        });
    }
    out.sort_by_key(|h| h.offset);
    out
}

/// A function's inferred effects after its signature, unless they are
/// declared (or must be: a public function's), or there are none.
fn effect_hint(
    out: &mut Vec<InlayHint>,
    text: &str,
    effects: &crate::effectchecker::EffectCheckResult,
    key: &str,
    declared: bool,
    body: &Block,
) {
    if declared {
        return;
    }
    let Some(set) = effects.inferred_effects.get(key) else {
        return;
    };
    if set.effects.is_empty() {
        return;
    }
    let Some(head) = text.get(..body.span.offset) else {
        return;
    };
    out.push(InlayHint {
        offset: head.trim_end().len(),
        // A `with` clause separates its items with spaces.
        label: format!("with {}", crate::render_effect_set(set).replace(" + ", " ")),
        kind: InlayHintKind::Effects,
    });
}
