//! `karac catalog` — public API surface index.
//!
//! Walks a parsed [`Program`] and emits one JSON record per public
//! item (`fn`, `struct`, `enum`, `trait`, `const`, `type alias`,
//! `distinct type`, `effect resource`). Output is JSONL (one record
//! per line) so downstream consumers (LLM agents, IDE plugins, doc
//! generators) can stream-parse without buffering the whole surface.
//!
//! Phase-5 line 643 / `docs/deferred.md § Signature Catalog`.
//!
//! Public surface only — items whose visibility is `Default` or
//! `Private` are skipped (their inferred reported-tier effect rows
//! aren't stable enough to index). For `impl` blocks, only the
//! individual `pub` methods are emitted (the block itself has no
//! visibility marker).
//!
//! Single-file mode (today's wiring) keys items by their bare name.
//! Multi-module mode (post-v1, alongside `module::ProgramTree`) will
//! prepend the module path — the JSON field is named `name` rather
//! than `path` so the same field absorbs the future change without
//! breaking consumers.

use crate::ast::*;
use crate::formatter::{render_effect_list, render_expr, render_trait_bound, render_type_expr};
use crate::token::Span;

/// Render the public-surface catalog for `program` as JSONL.
///
/// `filename` is recorded in every record's `span` payload so a
/// multi-file project's catalog stays sortable / joinable by file.
/// Returns the empty string when no public items exist.
pub fn render(program: &Program, filename: &str) -> String {
    let mut out = String::new();
    for item in &program.items {
        render_item(item, filename, &mut out);
    }
    out
}

fn render_item(item: &Item, filename: &str, out: &mut String) {
    match item {
        Item::Function(f) if f.visibility() == Visibility::Pub => {
            push_line(out, &render_function(f, None, filename));
        }
        Item::ExternFunction(f) if f.visibility() == Visibility::Pub => {
            push_line(out, &render_extern_function(f, filename));
        }
        Item::ExternBlock(block) => {
            for inner in &block.items {
                if let ExternItem::Function(f) = inner {
                    if f.visibility() == Visibility::Pub {
                        push_line(out, &render_extern_function(f, filename));
                    }
                }
                if let ExternItem::OpaqueType(t) = inner {
                    if t.visibility() == Visibility::Pub {
                        push_line(out, &render_opaque_type(t, filename));
                    }
                }
            }
        }
        Item::StructDef(s) if s.visibility() == Visibility::Pub => {
            push_line(out, &render_struct(s, filename));
        }
        Item::UnionDef(u) if u.visibility() == Visibility::Pub => {
            push_line(out, &render_union(u, filename));
        }
        Item::EnumDef(e) if e.visibility() == Visibility::Pub => {
            push_line(out, &render_enum(e, filename));
        }
        Item::TraitDef(t) if t.visibility() == Visibility::Pub => {
            push_line(out, &render_trait(t, filename));
        }
        Item::ConstDecl(c) if c.visibility() == Visibility::Pub => {
            push_line(out, &render_const(c, filename));
        }
        Item::TypeAlias(t) if t.visibility() == Visibility::Pub => {
            push_line(out, &render_type_alias(t, filename));
        }
        Item::DistinctType(d) if d.visibility() == Visibility::Pub => {
            push_line(out, &render_distinct(d, filename));
        }
        Item::EffectResource(r) => {
            // `effect resource R;` declarations have no visibility marker
            // in the AST today; surface every declaration. The catalog
            // schema is forward-compatible with a `pub` discriminator
            // once one lands.
            push_line(out, &render_effect_resource(r, filename));
        }
        Item::ImplBlock(b) => {
            let receiver = receiver_label(&b.target_type);
            for inner in &b.items {
                if let ImplItem::Method(m) = inner {
                    if m.visibility() == Visibility::Pub {
                        push_line(out, &render_function(m, Some(&receiver), filename));
                    }
                }
            }
        }
        _ => {}
    }
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

// ── Functions ────────────────────────────────────────────────────

fn render_function(f: &Function, impl_target: Option<&str>, filename: &str) -> String {
    let kind = if impl_target.is_some() {
        "impl_method"
    } else {
        "fn"
    };
    let qualified_name = match impl_target {
        Some(t) => format!("{t}.{}", f.name),
        None => f.name.clone(),
    };
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string(kind));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&qualified_name));
    if f.is_unsafe {
        record.push(',');
        write_kv(&mut record, "unsafe", "true");
    }
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&f.generic_params),
    );
    if let Some(self_param) = &f.self_param {
        record.push(',');
        write_kv(
            &mut record,
            "self",
            &json_string(self_param_str(self_param)),
        );
    }
    record.push(',');
    write_kv(&mut record, "params", &render_params_json(&f.params));
    record.push(',');
    write_kv(
        &mut record,
        "return_type",
        &render_return_type_json(&f.return_type),
    );
    record.push(',');
    write_kv(&mut record, "effects", &render_effects_json(&f.effects));
    if !f.requires.is_empty() {
        record.push(',');
        write_kv(
            &mut record,
            "requires",
            &render_expr_array_json(&f.requires),
        );
    }
    if !f.ensures.is_empty() {
        record.push(',');
        write_kv(
            &mut record,
            "ensures",
            &render_ensures_array_json(&f.ensures),
        );
    }
    if let Some(wc) = &f.where_clause {
        record.push(',');
        write_kv(&mut record, "where", &render_where_json(wc));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&f.span, filename));
    record.push('}');
    record
}

fn render_extern_function(f: &ExternFunction, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("extern_fn"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&f.name));
    record.push(',');
    write_kv(&mut record, "abi", &json_string(&f.abi));
    record.push(',');
    write_kv(&mut record, "generics", "[]");
    record.push(',');
    write_kv(&mut record, "params", &render_params_json(&f.params));
    record.push(',');
    write_kv(
        &mut record,
        "return_type",
        &render_return_type_json(&f.return_type),
    );
    record.push(',');
    write_kv(&mut record, "effects", &render_effects_json(&f.effects));
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&f.span, filename));
    record.push('}');
    record
}

fn render_opaque_type(t: &OpaqueTypeDecl, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("opaque_type"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&t.name));
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&t.span, filename));
    record.push('}');
    record
}

// ── Structs / Unions / Enums / Traits ───────────────────────────

fn render_struct(s: &StructDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("struct"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&s.name));
    if s.is_shared {
        record.push(',');
        write_kv(&mut record, "shared", "true");
    }
    if s.is_non_exhaustive {
        record.push(',');
        write_kv(&mut record, "non_exhaustive", "true");
    }
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&s.generic_params),
    );
    record.push(',');
    write_kv(&mut record, "fields", &render_struct_fields_json(&s.fields));
    if !s.invariants.is_empty() {
        record.push(',');
        write_kv(
            &mut record,
            "invariants",
            &render_expr_array_json(&s.invariants),
        );
    }
    if let Some(wc) = &s.where_clause {
        record.push(',');
        write_kv(&mut record, "where", &render_where_json(wc));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&s.span, filename));
    record.push('}');
    record
}

fn render_union(u: &UnionDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("union"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&u.name));
    record.push(',');
    let fields: Vec<String> = u
        .fields
        .iter()
        .map(|f| {
            let mut s = String::new();
            s.push('{');
            write_kv(&mut s, "name", &json_string(&f.name));
            s.push(',');
            write_kv(&mut s, "ty", &json_string(&render_type_expr(&f.ty)));
            s.push(',');
            write_kv(&mut s, "pub", if f.is_pub { "true" } else { "false" });
            s.push('}');
            s
        })
        .collect();
    write_kv(&mut record, "fields", &format!("[{}]", fields.join(",")));
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&u.span, filename));
    record.push('}');
    record
}

fn render_enum(e: &EnumDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("enum"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&e.name));
    if e.is_shared {
        record.push(',');
        write_kv(&mut record, "shared", "true");
    }
    if e.is_non_exhaustive {
        record.push(',');
        write_kv(&mut record, "non_exhaustive", "true");
    }
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&e.generic_params),
    );
    record.push(',');
    let variants: Vec<String> = e.variants.iter().map(render_variant_json).collect();
    write_kv(
        &mut record,
        "variants",
        &format!("[{}]", variants.join(",")),
    );
    if let Some(wc) = &e.where_clause {
        record.push(',');
        write_kv(&mut record, "where", &render_where_json(wc));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&e.span, filename));
    record.push('}');
    record
}

fn render_variant_json(v: &Variant) -> String {
    let mut s = String::new();
    s.push('{');
    write_kv(&mut s, "name", &json_string(&v.name));
    s.push(',');
    match &v.kind {
        VariantKind::Unit => write_kv(&mut s, "shape", &json_string("unit")),
        VariantKind::Tuple(types) => {
            write_kv(&mut s, "shape", &json_string("tuple"));
            s.push(',');
            let tys: Vec<String> = types
                .iter()
                .map(|t| json_string(&render_type_expr(t)))
                .collect();
            write_kv(&mut s, "fields", &format!("[{}]", tys.join(",")));
        }
        VariantKind::Struct(fields) => {
            write_kv(&mut s, "shape", &json_string("struct"));
            s.push(',');
            write_kv(&mut s, "fields", &render_struct_fields_json(fields));
        }
    }
    s.push('}');
    s
}

fn render_trait(t: &TraitDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("trait"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&t.name));
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&t.generic_params),
    );
    record.push(',');
    let supers: Vec<String> = t
        .supertraits
        .iter()
        .map(|b| json_string(&render_trait_bound(b)))
        .collect();
    write_kv(
        &mut record,
        "supertraits",
        &format!("[{}]", supers.join(",")),
    );
    if let Some(te) = &t.trait_effects {
        record.push(',');
        write_kv(
            &mut record,
            "trait_effects",
            &json_string(&render_effect_list(te)),
        );
    }
    record.push(',');
    let methods: Vec<String> = t
        .items
        .iter()
        .filter_map(|i| match i {
            TraitItem::Method(m) => Some(render_trait_method_json(m)),
            _ => None,
        })
        .collect();
    write_kv(&mut record, "methods", &format!("[{}]", methods.join(",")));
    let assocs: Vec<String> = t
        .items
        .iter()
        .filter_map(|i| match i {
            TraitItem::AssocType(a) => Some(render_assoc_type_json(a)),
            _ => None,
        })
        .collect();
    if !assocs.is_empty() {
        record.push(',');
        write_kv(
            &mut record,
            "assoc_types",
            &format!("[{}]", assocs.join(",")),
        );
    }
    if let Some(wc) = &t.where_clause {
        record.push(',');
        write_kv(&mut record, "where", &render_where_json(wc));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&t.span, filename));
    record.push('}');
    record
}

fn render_trait_method_json(m: &TraitMethod) -> String {
    let mut s = String::new();
    s.push('{');
    write_kv(&mut s, "name", &json_string(&m.name));
    if m.is_unsafe {
        s.push(',');
        write_kv(&mut s, "unsafe", "true");
    }
    s.push(',');
    write_kv(&mut s, "generics", &render_generics_json(&m.generic_params));
    if let Some(sp) = &m.self_param {
        s.push(',');
        write_kv(&mut s, "self", &json_string(self_param_str(sp)));
    }
    s.push(',');
    write_kv(&mut s, "params", &render_params_json(&m.params));
    s.push(',');
    write_kv(
        &mut s,
        "return_type",
        &render_return_type_json(&m.return_type),
    );
    s.push(',');
    write_kv(&mut s, "effects", &render_effects_json(&m.effects));
    s.push(',');
    write_kv(
        &mut s,
        "has_default",
        if m.body.is_some() { "true" } else { "false" },
    );
    s.push('}');
    s
}

// ── Consts / Type Aliases / Distinct / Effect Resources ─────────

fn render_const(c: &ConstDecl, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("const"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&c.name));
    record.push(',');
    write_kv(&mut record, "ty", &json_string(&render_type_expr(&c.ty)));
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&c.span, filename));
    record.push('}');
    record
}

fn render_type_alias(t: &TypeAliasDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("type_alias"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&t.name));
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&t.generic_params),
    );
    record.push(',');
    write_kv(&mut record, "ty", &json_string(&render_type_expr(&t.ty)));
    if let Some(r) = &t.refinement {
        record.push(',');
        write_kv(&mut record, "refinement", &json_string(&render_expr(r)));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&t.span, filename));
    record.push('}');
    record
}

fn render_distinct(d: &DistinctTypeDef, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("distinct_type"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&d.name));
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&d.generic_params),
    );
    record.push(',');
    write_kv(
        &mut record,
        "base_type",
        &json_string(&render_type_expr(&d.base_type)),
    );
    if let Some(r) = &d.refinement {
        record.push(',');
        write_kv(&mut record, "refinement", &json_string(&render_expr(r)));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&d.span, filename));
    record.push('}');
    record
}

fn render_effect_resource(r: &EffectResourceDecl, filename: &str) -> String {
    let mut record = String::new();
    record.push('{');
    write_kv(&mut record, "kind", &json_string("effect_resource"));
    record.push(',');
    write_kv(&mut record, "name", &json_string(&r.name));
    record.push(',');
    write_kv(
        &mut record,
        "generics",
        &render_generics_json(&r.generic_params),
    );
    if let Some(p) = &r.provider_trait {
        record.push(',');
        write_kv(&mut record, "provider_trait", &json_string(p));
    }
    record.push(',');
    write_kv(&mut record, "span", &render_span_json(&r.span, filename));
    record.push('}');
    record
}

fn render_assoc_type_json(a: &AssocTypeDecl) -> String {
    let mut s = String::new();
    s.push('{');
    write_kv(&mut s, "name", &json_string(&a.name));
    if let Some(gp) = &a.generic_params {
        s.push(',');
        write_kv(&mut s, "generics", &render_generics_json(&Some(gp.clone())));
    }
    let bounds: Vec<String> = a
        .bounds
        .iter()
        .map(|b| json_string(&render_trait_bound(b)))
        .collect();
    if !bounds.is_empty() {
        s.push(',');
        write_kv(&mut s, "bounds", &format!("[{}]", bounds.join(",")));
    }
    s.push('}');
    s
}

// ── Receiver-type helpers ───────────────────────────────────────

fn receiver_label(ty: &TypeExpr) -> String {
    // For the common `impl Type { ... }` (or `impl Type[Args] { ... }`)
    // form, the receiver label is the rendered type-expression. Future
    // refinement (e.g. canonical `impl Trait for Type` rendering) can
    // tighten this without breaking the schema.
    render_type_expr(ty)
}

// ── Params + types ──────────────────────────────────────────────

fn render_params_json(params: &[Param]) -> String {
    let entries: Vec<String> = params
        .iter()
        .map(|p| {
            let mut s = String::new();
            s.push('{');
            let name = p.name().unwrap_or("_");
            write_kv(&mut s, "name", &json_string(name));
            s.push(',');
            let (mode, inner) = param_mode_and_inner(&p.ty);
            write_kv(&mut s, "mode", &json_string(mode));
            s.push(',');
            write_kv(&mut s, "ty", &json_string(&inner));
            if p.default_value.is_some() {
                s.push(',');
                write_kv(&mut s, "has_default", "true");
            }
            s.push('}');
            s
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn render_return_type_json(rt: &Option<TypeExpr>) -> String {
    match rt {
        Some(t) => json_string(&render_type_expr(t)),
        None => json_string("()"),
    }
}

fn render_effects_json(effects: &Option<EffectList>) -> String {
    match effects {
        Some(e) => {
            let items: Vec<String> = e
                .items
                .iter()
                .map(|item| match item {
                    EffectItem::Verb(v) => {
                        let mut s = String::new();
                        s.push('{');
                        write_kv(&mut s, "verb", &json_string(effect_verb_kind_str(&v.kind)));
                        s.push(',');
                        let resources: Vec<String> = v
                            .resources
                            .iter()
                            .map(|r| json_string(&r.path.join(".")))
                            .collect();
                        write_kv(&mut s, "resources", &format!("[{}]", resources.join(",")));
                        s.push('}');
                        s
                    }
                    EffectItem::Group(g) => format!("{{\"group\":{}}}", json_string(g)),
                    EffectItem::Polymorphic => "{\"polymorphic\":true}".to_string(),
                    EffectItem::Variable(v) => format!("{{\"variable\":{}}}", json_string(v)),
                })
                .collect();
            format!("[{}]", items.join(","))
        }
        None => "[]".to_string(),
    }
}

fn render_expr_array_json(exprs: &[Expr]) -> String {
    let parts: Vec<String> = exprs.iter().map(|e| json_string(&render_expr(e))).collect();
    format!("[{}]", parts.join(","))
}

fn render_ensures_array_json(ensures: &[EnsuresClause]) -> String {
    let parts: Vec<String> = ensures
        .iter()
        .map(|e| {
            let mut s = String::new();
            s.push('{');
            if let Some(p) = &e.param {
                write_kv(&mut s, "param", &json_string(p));
                s.push(',');
            }
            write_kv(&mut s, "expr", &json_string(&render_expr(&e.body)));
            s.push('}');
            s
        })
        .collect();
    format!("[{}]", parts.join(","))
}

fn render_struct_fields_json(fields: &[StructField]) -> String {
    let entries: Vec<String> = fields
        .iter()
        .map(|f| {
            let mut s = String::new();
            s.push('{');
            write_kv(&mut s, "name", &json_string(&f.name));
            s.push(',');
            write_kv(&mut s, "ty", &json_string(&render_type_expr(&f.ty)));
            s.push(',');
            write_kv(&mut s, "pub", if f.is_pub { "true" } else { "false" });
            if f.is_mut {
                s.push(',');
                write_kv(&mut s, "mut", "true");
            }
            s.push('}');
            s
        })
        .collect();
    format!("[{}]", entries.join(","))
}

fn render_generics_json(gp: &Option<GenericParams>) -> String {
    let Some(gp) = gp else {
        return "[]".to_string();
    };
    let mut entries: Vec<String> = gp
        .params
        .iter()
        .map(|p| {
            let mut s = String::new();
            s.push('{');
            write_kv(&mut s, "name", &json_string(&p.name));
            if p.is_const {
                s.push(',');
                write_kv(&mut s, "const", "true");
                if let Some(ct) = &p.const_type {
                    s.push(',');
                    write_kv(&mut s, "const_type", &json_string(&render_type_expr(ct)));
                }
            }
            if !p.bounds.is_empty() {
                s.push(',');
                let bounds: Vec<String> = p
                    .bounds
                    .iter()
                    .map(|b| json_string(&render_trait_bound(b)))
                    .collect();
                write_kv(&mut s, "bounds", &format!("[{}]", bounds.join(",")));
            }
            s.push('}');
            s
        })
        .collect();
    for ep in &gp.effect_params {
        let mut s = String::new();
        s.push('{');
        write_kv(&mut s, "name", &json_string(&ep.name));
        s.push(',');
        write_kv(&mut s, "effect", "true");
        if !ep.bounds.is_empty() {
            s.push(',');
            let bounds: Vec<String> = ep
                .bounds
                .iter()
                .map(|b| json_string(&render_trait_bound(b)))
                .collect();
            write_kv(&mut s, "bounds", &format!("[{}]", bounds.join(",")));
        }
        s.push('}');
        entries.push(s);
    }
    format!("[{}]", entries.join(","))
}

fn render_where_json(wc: &WhereClause) -> String {
    let parts: Vec<String> = wc
        .constraints
        .iter()
        .map(|c| match c {
            WhereConstraint::TypeBound {
                type_name, bounds, ..
            } => {
                let bounds: Vec<String> = bounds
                    .iter()
                    .map(|b| json_string(&render_trait_bound(b)))
                    .collect();
                format!(
                    "{{\"kind\":\"type_bound\",\"type\":{},\"bounds\":[{}]}}",
                    json_string(type_name),
                    bounds.join(",")
                )
            }
            WhereConstraint::AssocTypeEq {
                type_name,
                assoc_name,
                ty,
                ..
            } => format!(
                "{{\"kind\":\"assoc_eq\",\"type\":{},\"assoc\":{},\"ty\":{}}}",
                json_string(type_name),
                json_string(assoc_name),
                json_string(&render_type_expr(ty)),
            ),
            WhereConstraint::ProjectionBound {
                projection, bounds, ..
            } => {
                let bounds: Vec<String> = bounds
                    .iter()
                    .map(|b| json_string(&render_trait_bound(b)))
                    .collect();
                format!(
                    "{{\"kind\":\"projection_bound\",\"projection\":{},\"bounds\":[{}]}}",
                    json_string(&render_type_expr(projection)),
                    bounds.join(",")
                )
            }
            WhereConstraint::ConstPredicate { expr, .. } => format!(
                "{{\"kind\":\"const_predicate\",\"expr\":{}}}",
                json_string(&render_expr(expr))
            ),
        })
        .collect();
    format!("[{}]", parts.join(","))
}

// ── Param mode classification ───────────────────────────────────

fn param_mode_and_inner(ty: &TypeExpr) -> (&'static str, String) {
    match &ty.kind {
        TypeKind::Ref(inner) => ("ref", render_type_expr(inner)),
        TypeKind::MutRef(inner) => ("mut ref", render_type_expr(inner)),
        TypeKind::MutSlice(inner) => ("mut slice", format!("Slice[{}]", render_type_expr(inner))),
        _ => ("own", render_type_expr(ty)),
    }
}

fn self_param_str(s: &SelfParam) -> &'static str {
    match s {
        SelfParam::Owned => "self",
        SelfParam::Ref => "ref self",
        SelfParam::MutRef => "mut ref self",
    }
}

fn effect_verb_kind_str(v: &EffectVerbKind) -> &'static str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(_) => "user_defined",
    }
}

// ── Span ────────────────────────────────────────────────────────

fn render_span_json(span: &Span, filename: &str) -> String {
    format!(
        "{{\"file\":{},\"line\":{},\"col\":{},\"offset\":{},\"length\":{}}}",
        json_string(filename),
        span.line,
        span.column,
        span.offset,
        span.length
    )
}

// ── JSON primitives ─────────────────────────────────────────────

fn write_kv(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(value);
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Program {
        let pr = crate::parse(source);
        pr.program
    }

    #[test]
    fn private_function_is_omitted() {
        let p = parse("fn add(x: i64, y: i64) -> i64 { x + y }\n");
        let out = render(&p, "src/m.kara");
        assert!(out.is_empty(), "expected empty output, got: {out}");
    }

    #[test]
    fn pub_function_carries_signature_fields() {
        let p = parse("pub fn add(x: i64, y: i64) -> i64 { x + y }\n");
        let out = render(&p, "src/m.kara");
        let line = out.trim();
        assert!(line.contains("\"kind\":\"fn\""), "got: {line}");
        assert!(line.contains("\"name\":\"add\""), "got: {line}");
        assert!(
            line.contains("\"params\":[{\"name\":\"x\",\"mode\":\"own\",\"ty\":\"i64\"},{\"name\":\"y\",\"mode\":\"own\",\"ty\":\"i64\"}]"),
            "got: {line}"
        );
        assert!(line.contains("\"return_type\":\"i64\""), "got: {line}");
        assert!(line.contains("\"effects\":[]"), "got: {line}");
        assert!(line.contains("\"file\":\"src/m.kara\""), "got: {line}");
    }

    #[test]
    fn ref_and_mut_ref_params_classify_mode() {
        let p = parse("pub fn count(xs: ref Vec[i64], buf: mut ref Buffer) -> i64 { 0 }\n");
        let out = render(&p, "m.kara");
        let line = out.trim();
        assert!(line.contains("\"mode\":\"ref\""), "got: {line}");
        assert!(line.contains("\"mode\":\"mut ref\""), "got: {line}");
        assert!(line.contains("\"ty\":\"Vec[i64]\""), "got: {line}");
        assert!(line.contains("\"ty\":\"Buffer\""), "got: {line}");
    }

    #[test]
    fn return_type_unit_when_omitted() {
        let p = parse("pub fn noop() { }\n");
        let out = render(&p, "m.kara");
        assert!(out.contains("\"return_type\":\"()\""), "got: {out}");
    }

    #[test]
    fn generic_params_with_bounds_render() {
        let p = parse("pub fn sort[T: Ord](xs: mut Slice[T]) { }\n");
        let out = render(&p, "m.kara");
        assert!(
            out.contains("\"generics\":[{\"name\":\"T\",\"bounds\":[\"Ord\"]}]"),
            "got: {out}"
        );
        assert!(out.contains("\"mode\":\"mut slice\""), "got: {out}");
        assert!(out.contains("\"ty\":\"Slice[T]\""), "got: {out}");
    }

    #[test]
    fn declared_effects_render_per_verb() {
        let p = parse("pub fn save(s: ref String) with writes(Fs) reads(Time) { }\n");
        let out = render(&p, "m.kara");
        assert!(
            out.contains("\"effects\":[{\"verb\":\"writes\",\"resources\":[\"Fs\"]},{\"verb\":\"reads\",\"resources\":[\"Time\"]}]"),
            "got: {out}"
        );
    }

    #[test]
    fn pub_struct_emits_fields_with_visibility_and_mutability() {
        let p = parse("pub struct Point { pub x: f64, pub mut y: f64, internal: i32 }\n");
        let out = render(&p, "m.kara");
        let line = out.trim();
        assert!(line.contains("\"kind\":\"struct\""), "got: {line}");
        assert!(line.contains("\"name\":\"Point\""), "got: {line}");
        assert!(
            line.contains("\"name\":\"x\",\"ty\":\"f64\",\"pub\":true"),
            "got: {line}"
        );
        assert!(
            line.contains("\"name\":\"y\",\"ty\":\"f64\",\"pub\":true,\"mut\":true"),
            "got: {line}"
        );
        assert!(
            line.contains("\"name\":\"internal\",\"ty\":\"i32\",\"pub\":false"),
            "got: {line}"
        );
    }

    #[test]
    fn pub_enum_emits_variants_with_shapes() {
        let p = parse("pub enum Shape { Unit, Tup(i64, f64), Rec { width: f64, height: f64 } }\n");
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"enum\""), "got: {out}");
        assert!(
            out.contains("{\"name\":\"Unit\",\"shape\":\"unit\"}"),
            "got: {out}"
        );
        assert!(
            out.contains("\"shape\":\"tuple\",\"fields\":[\"i64\",\"f64\"]"),
            "got: {out}"
        );
        assert!(out.contains("\"shape\":\"struct\""), "got: {out}");
    }

    #[test]
    fn pub_trait_emits_method_signatures_and_supertraits() {
        let p = parse(
            "pub trait MyOrd: Eq { fn compare(ref self, other: ref Self) -> i32; fn is_lt(ref self, other: ref Self) -> bool { false } }\n",
        );
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"trait\""), "got: {out}");
        assert!(out.contains("\"supertraits\":[\"Eq\"]"), "got: {out}");
        assert!(out.contains("\"name\":\"compare\""), "got: {out}");
        assert!(out.contains("\"has_default\":false"), "got: {out}");
        assert!(out.contains("\"has_default\":true"), "got: {out}");
    }

    #[test]
    fn pub_const_and_type_alias_emit_records() {
        let p = parse("pub const MAX: i64 = 1024;\npub type Id = u32;\n");
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"const\""), "got: {out}");
        assert!(out.contains("\"name\":\"MAX\""), "got: {out}");
        assert!(out.contains("\"ty\":\"i64\""), "got: {out}");
        assert!(out.contains("\"kind\":\"type_alias\""), "got: {out}");
        assert!(out.contains("\"name\":\"Id\""), "got: {out}");
        assert!(out.contains("\"ty\":\"u32\""), "got: {out}");
    }

    #[test]
    fn pub_impl_methods_qualify_name_with_target_type() {
        let p = parse(
            "pub struct Point { pub x: f64, pub y: f64 }\nimpl Point { pub fn new(x: f64, y: f64) -> Point { Point { x: x, y: y } } fn private_helper() -> i64 { 0 } }\n",
        );
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"impl_method\""), "got: {out}");
        assert!(out.contains("\"name\":\"Point.new\""), "got: {out}");
        assert!(
            !out.contains("\"name\":\"Point.private_helper\""),
            "private impl method should be skipped: {out}"
        );
    }

    #[test]
    fn effect_resource_decl_emits_record() {
        let p = parse("effect resource Clock;\n");
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"effect_resource\""), "got: {out}");
        assert!(out.contains("\"name\":\"Clock\""), "got: {out}");
    }

    #[test]
    fn distinct_type_emits_base_and_refinement() {
        let p = parse("pub distinct type Age = u8 where it >= 0 && it <= 150;\n");
        let out = render(&p, "m.kara");
        assert!(out.contains("\"kind\":\"distinct_type\""), "got: {out}");
        assert!(out.contains("\"base_type\":\"u8\""), "got: {out}");
        assert!(out.contains("\"refinement\":"), "got: {out}");
    }

    #[test]
    fn span_records_line_col_offset() {
        let p = parse("\n\npub fn f() { }\n");
        let out = render(&p, "m.kara");
        // The `fn` keyword anchors the Function span (col 5 = after "pub ").
        assert!(out.contains("\"line\":3"), "got: {out}");
        assert!(out.contains("\"file\":\"m.kara\""), "got: {out}");
    }
}
