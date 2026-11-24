//! `karac explain <file>.<item>`: for a function, its inferred effect set
//! with the derivation of each effect, its declared set, and each
//! parameter's would-be mode beside the declared one (design.md §17
//! `karac fix` and `karac explain`, §2 Reported behavior; B-2026-11-05-28).

use super::*;
use crate::effect_graph::{derivation, derivation_json, effect_display, DerivationStep};
use crate::effectchecker::DeclaredEffects;

/// One parameter: its name, its declared mode and the weakest mode its
/// body needs.
struct Param {
    name: String,
    declared: OwnershipMode,
    would_be: OwnershipMode,
}

/// What `explain` reports about one function.
struct ItemReport {
    item: String,
    file: String,
    line: usize,
    inferred: Vec<(String, Vec<DerivationStep>)>,
    declared: Option<String>,
    declared_json: String,
    params: Vec<Param>,
}

/// `a.kara.top` as `("a.kara", "top")`; a method is `a.kara.Type.method`.
pub(super) fn split_item_target(name: &str) -> Option<(&str, &str)> {
    let at = name.find(".kara.")?;
    let (file, item) = (&name[..at + 5], &name[at + 6..]);
    (!item.is_empty()).then_some((file, item))
}

fn report(file: &str, item: &str) -> Result<ItemReport, String> {
    let source = std::fs::read_to_string(file).map_err(|e| format!("cannot read `{file}`: {e}"))?;
    // The program `karac check` sees: the library appended, the core rules
    // on (B-2026-11-06-5).
    let mut pipeline = Pipeline::new(file, &source).strict_core().with_library();
    pipeline.resolve();
    if pipeline.has_fatal_errors() {
        return Err(format!(
            "`{file}` does not resolve; `karac check {file}` reports why"
        ));
    }
    pipeline.typecheck();
    pipeline.lower();
    pipeline.effectcheck();
    pipeline.ownershipcheck();
    let graph =
        crate::call_graph::build(&pipeline.parsed.program, file, file.ends_with("_test.kara"));
    let Some(node) = graph.nodes.get(item) else {
        return Err(format!(
            "no function `{item}` in `{file}` (a method is named `Type.method`)"
        ));
    };
    let effects = pipeline.effects.as_ref();
    let mut inferred: Vec<(String, Vec<DerivationStep>)> = effects
        .and_then(|e| e.inferred_effects.get(item))
        .map(|set| {
            let mut seen = std::collections::HashSet::new();
            set.effects
                .iter()
                .filter(|te| seen.insert(te.effect.clone()))
                .map(|te| {
                    let steps = derivation(
                        effects.unwrap(),
                        item,
                        &te.effect,
                        file,
                        pipeline.parsed.program.library_start,
                    );
                    (effect_display(&te.effect), steps)
                })
                .collect()
        })
        .unwrap_or_default();
    inferred.sort_by(|a, b| a.0.cmp(&b.0));
    let declared_effects = effects.and_then(|e| e.declared_effects.get(item));
    let declared_effects = declared_effects.filter(|d| !matches!(d, DeclaredEffects::None));
    let declared = declared_effects.map(|d| match d {
        DeclaredEffects::Explicit(set) => set
            .effects
            .iter()
            .map(|te| effect_display(&te.effect))
            .collect::<Vec<_>>()
            .join(" "),
        DeclaredEffects::None => String::new(),
        DeclaredEffects::Polymorphic => "polymorphic".to_string(),
        DeclaredEffects::PolymorphicWithFixed(set) => format!(
            "polymorphic, with {}",
            set.effects
                .iter()
                .map(|te| effect_display(&te.effect))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    });
    let params = pipeline
        .ownership
        .as_ref()
        .and_then(|o| {
            let body = o.param_modes.get(item)?;
            let declared = o.declared_param_modes.get(item);
            Some(
                body.iter()
                    .enumerate()
                    .map(|(i, (name, would_be))| Param {
                        name: name.clone(),
                        declared: declared
                            .and_then(|d| d.get(i))
                            .map_or(would_be.clone(), |(_, m)| m.clone()),
                        would_be: would_be.clone(),
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    Ok(ItemReport {
        item: item.to_string(),
        file: file.to_string(),
        line: node.line,
        inferred,
        declared,
        declared_json: crate::effect_graph::declared_effects_json(declared_effects),
        params,
    })
}

fn mode_word(m: &OwnershipMode) -> String {
    m.to_string()
}

fn render_text(r: &ItemReport) -> String {
    let mut out = format!(
        "karac explain — item: {} ({}:{})\n\n",
        r.item, r.file, r.line
    );
    if r.inferred.is_empty() {
        out.push_str("Effects: none\n");
    } else {
        out.push_str("Effects:\n");
        for (effect, steps) in &r.inferred {
            out.push_str(&format!("  {effect}\n"));
            for s in steps {
                out.push_str(&format!("    {}:{}  {}\n", s.file, s.line, s.reason));
            }
        }
    }
    match &r.declared {
        Some(d) if !d.is_empty() => out.push_str(&format!("Declared effects: {d}\n")),
        Some(_) => out.push_str("Declared effects: none (an empty `with` clause)\n"),
        None => out.push_str("Declared effects: no `with` clause\n"),
    }
    if !r.params.is_empty() {
        out.push_str("\nParameters:\n");
        for p in &r.params {
            if p.declared == p.would_be {
                out.push_str(&format!("  {}: {}\n", p.name, mode_word(&p.declared)));
            } else {
                out.push_str(&format!(
                    "  {}: declared {}, the body needs only {}: could be {}\n",
                    p.name,
                    mode_word(&p.declared),
                    mode_word(&p.would_be),
                    match p.would_be {
                        OwnershipMode::Ref => "borrowed (no mode)".to_string(),
                        ref m => mode_word(m),
                    }
                ));
            }
        }
    }
    out
}

fn render_json(r: &ItemReport) -> String {
    let inferred: Vec<String> = r.inferred.iter().map(|(e, _)| json_string(e)).collect();
    let derivations: Vec<String> = r
        .inferred
        .iter()
        .map(|(e, steps)| {
            format!(
                "{{\"effect\":{},\"derivation\":{}}}",
                json_string(e),
                derivation_json(steps)
            )
        })
        .collect();
    let params: Vec<String> = r
        .params
        .iter()
        .map(|p| {
            format!(
                "{{\"name\":{},\"declared_mode\":{},\"would_be_mode\":{}}}",
                json_string(&p.name),
                json_string(ownership_mode_str(&p.declared)),
                json_string(ownership_mode_str(&p.would_be)),
            )
        })
        .collect();
    format!(
        "{{\"kind\":\"item\",\"item\":{},\"file\":{},\"line\":{},\"inferred_effects\":[{}],\"declared_effects\":{},\"effects\":[{}],\"parameters\":[{}]}}",
        json_string(&r.item),
        json_string(&r.file),
        r.line,
        inferred.join(","),
        r.declared_json,
        derivations.join(","),
        params.join(","),
    )
}

/// Render `karac explain <file>.<item>`; an error names what is missing.
pub(super) fn render_to_string(
    file: &str,
    item: &str,
    format: ExplainFormat,
) -> Result<String, String> {
    if !item.contains('.') {
        if let Some(out) = type_flags(file, item, format)? {
            return Ok(out);
        }
    }
    let r = report(file, item)?;
    Ok(match format {
        ExplainFormat::Text => render_text(&r),
        ExplainFormat::Json => render_json(&r),
    })
}

/// `karac explain <file>.<Type>` for a `shared` or `sync` type: the fields
/// that carry a run-time borrow flag and the total, a byte each per
/// instance before padding (design.md § Shared types; B-2026-11-23-75).
/// `None` when `item` is no such type, so the function report answers.
fn type_flags(file: &str, item: &str, format: ExplainFormat) -> Result<Option<String>, String> {
    let source = std::fs::read_to_string(file).map_err(|e| format!("cannot read `{file}`: {e}"))?;
    let declared = crate::parse(&source)
        .program
        .items
        .iter()
        .find_map(|i| match i {
            crate::ast::Item::StructDef(s) if s.name == item => Some(s.is_shared || s.is_par),
            crate::ast::Item::EnumDef(e) if e.name == item => Some(e.is_shared || e.is_par),
            _ => None,
        });
    match declared {
        None => return Ok(None),
        Some(false) => {
            let text = format!(
                "karac explain — type: {item} ({file})\n\nBorrow flags: none (only a `shared` or `sync` type's fields carry one)\n"
            );
            return Ok(Some(match format {
                ExplainFormat::Text => text,
                ExplainFormat::Json => format!(
                    "{{\"kind\":\"type\",\"item\":{},\"file\":{},\"keyword\":null,\"borrow_flags\":0,\"flagged\":[],\"unflagged\":[]}}",
                    json_string(item),
                    json_string(file),
                ),
            }));
        }
        Some(true) => {}
    }
    let Some(r) = crate::mir::lower::borrow_flag_report(&source, item)
        .map_err(|e| format!("`{file}` does not build far enough to place borrow flags: {e}"))?
    else {
        return Ok(None);
    };
    let n = r.flagged.len();
    let kind = if r.is_enum { "enum" } else { "struct" };
    Ok(Some(match format {
        ExplainFormat::Text => {
            let mut out = format!(
                "karac explain — type: {item} ({file}), a `{} {kind}`\n\nBorrow flags: {n} ({n} byte{} per instance before padding)\n",
                r.keyword,
                if n == 1 { "" } else { "s" },
            );
            for (f, param) in &r.flagged {
                let note = if *param {
                    " (typed by a parameter: at an instance whose argument is neither `Copy` nor a handle)"
                } else {
                    ""
                };
                out.push_str(&format!("  {f}: flagged{note}\n"));
            }
            for (f, why) in &r.unflagged {
                out.push_str(&format!("  {f}: no flag ({why})\n"));
            }
            out
        }
        ExplainFormat::Json => {
            let flagged: Vec<String> = r
                .flagged
                .iter()
                .map(|(f, p)| format!("{{\"field\":{},\"by_parameter\":{p}}}", json_string(f)))
                .collect();
            let unflagged: Vec<String> = r
                .unflagged
                .iter()
                .map(|(f, why)| {
                    format!(
                        "{{\"field\":{},\"why\":{}}}",
                        json_string(f),
                        json_string(why)
                    )
                })
                .collect();
            format!(
                "{{\"kind\":\"type\",\"item\":{},\"file\":{},\"keyword\":{},\"borrow_flags\":{n},\"flagged\":[{}],\"unflagged\":[{}]}}",
                json_string(item),
                json_string(file),
                json_string(r.keyword),
                flagged.join(","),
                unflagged.join(","),
            )
        }
    }))
}
