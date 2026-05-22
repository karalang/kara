//! `karac query attributes [--tool PREFIX]` collector — slice 3 of
//! v60 item 37 (Tool-Namespaced Attributes).
//!
//! Walks every attribute-bearing item in a `Program`, collects every
//! multi-segment attribute path (single-segment / bare-name paths are
//! the compiler's own surface — `#[derive]`, `#[deprecated]`, etc. —
//! and outside this query's scope), and produces one
//! [`AttributeQueryRecord`] per occurrence. The query is the
//! tool-facing read surface — external tools (formatters, linters,
//! doc generators) consume the JSON output via `karac query
//! attributes`.
//!
//! ## Why multi-segment only
//!
//! Bare-name attributes are the compiler's closed v1 surface
//! ([`crate::attribute_validator::RECOGNIZED_BARE_ATTRIBUTES`]).
//! Reporting them through the same channel would conflate the
//! compiler's own surface with the tool-namespaced surface and force
//! external tools to filter; restricting the query to multi-segment
//! paths matches the spec's "any namespace other than the compiler-
//! reserved set" framing.
//!
//! ## Filtering
//!
//! When [`AttributeQueryFilter::tool_prefix`] is `Some(name)`, the
//! collector emits only attributes whose first path segment is
//! `name` — `--tool karafmt` returns every `#[karafmt::*]` and nothing
//! else. An unfiltered query (`None`) emits every multi-segment
//! attribute in the program, including compiler-reserved
//! `#[diagnostic::*]` members.

use crate::ast::{AttrArg, Attribute, ExprKind, ExternItem, ImplItem, Item, Program, TraitItem};
use crate::token::Span;

/// One record per multi-segment attribute occurrence in the program.
#[derive(Debug, Clone)]
pub struct AttributeQueryRecord {
    /// Full attribute path — `#[diagnostic::on_unimplemented]` →
    /// `vec!["diagnostic", "on_unimplemented"]`. Always at least two
    /// segments; bare-name attributes are excluded.
    pub path: Vec<String>,
    /// Parsed argument list. Each arg has an optional `name` (`Some`
    /// for `name: value` style, `None` for positional) and an optional
    /// `value` rendering classified by literal kind for simple cases.
    pub args: Vec<AttributeQueryArg>,
    /// "Where the attribute is attached" — a human-readable item
    /// identifier of the form `<kind> <qualified-name>`, e.g.
    /// `fn parse`, `struct UserRecord`, `struct UserRecord.id`,
    /// `impl T for S`, `impl T for S.method`. Tools use this to scope
    /// their behaviour (e.g. `karafmt::skip` on a `fn` vs. an `impl`).
    pub attached_to: String,
    /// Source span of the attribute itself (the `#[...]` syntax).
    pub span: Span,
}

/// One argument inside an attribute. Lossy by design — the goal is a
/// machine-readable summary for tools, not a full re-parser.
#[derive(Debug, Clone)]
pub struct AttributeQueryArg {
    pub name: Option<String>,
    /// Classified value rendering. `Some` for recognised literal
    /// shapes (string / int / bool / path); `None` for compound or
    /// unrecognised expressions — tools that need the full expression
    /// should consume the source text at `span` instead.
    pub value: Option<AttributeQueryValue>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum AttributeQueryValue {
    String(String),
    Int(i128),
    Bool(bool),
    /// Dotted path (e.g. `std.option.None`) rendered as a single
    /// string with `.` separators.
    Path(String),
    /// Anything else — float, struct lit, function call, arithmetic,
    /// etc. The token "expr" signals "no structured value, but the
    /// argument is well-formed".
    Other,
}

/// Filter for the collector pass. `tool_prefix` is a first-segment
/// match; when `None`, every multi-segment attribute flows through.
#[derive(Debug, Clone, Default)]
pub struct AttributeQueryFilter {
    pub tool_prefix: Option<String>,
}

/// Walk `program` and produce one record per multi-segment attribute
/// occurrence, honouring `filter`. Records are emitted in source
/// order (the order each item is visited, with sub-items walked
/// inline immediately after their parent).
pub fn collect_attributes(
    program: &Program,
    filter: &AttributeQueryFilter,
) -> Vec<AttributeQueryRecord> {
    let mut out = Vec::new();
    for item in &program.items {
        walk_item(item, filter, &mut out);
    }
    out
}

fn walk_item(item: &Item, filter: &AttributeQueryFilter, out: &mut Vec<AttributeQueryRecord>) {
    match item {
        Item::Function(f) => emit_attrs(&f.attributes, &format!("fn {}", f.name), filter, out),
        Item::StructDef(s) => {
            emit_attrs(&s.attributes, &format!("struct {}", s.name), filter, out);
            for field in &s.fields {
                emit_attrs(
                    &field.attributes,
                    &format!("struct {}.{}", s.name, field.name),
                    filter,
                    out,
                );
            }
        }
        Item::UnionDef(u) => {
            emit_attrs(&u.attributes, &format!("union {}", u.name), filter, out);
            for field in &u.fields {
                emit_attrs(
                    &field.attributes,
                    &format!("union {}.{}", u.name, field.name),
                    filter,
                    out,
                );
            }
        }
        Item::EnumDef(e) => {
            emit_attrs(&e.attributes, &format!("enum {}", e.name), filter, out);
            for v in &e.variants {
                emit_attrs(
                    &v.attributes,
                    &format!("enum {}.{}", e.name, v.name),
                    filter,
                    out,
                );
            }
        }
        Item::TraitDef(t) => {
            emit_attrs(&t.attributes, &format!("trait {}", t.name), filter, out);
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    emit_attrs(
                        &m.attributes,
                        &format!("trait {}.{}", t.name, m.name),
                        filter,
                        out,
                    );
                }
            }
        }
        Item::TraitAlias(t) => emit_attrs(&t.attributes, &format!("trait {}", t.name), filter, out),
        Item::MarkerTrait(t) => emit_attrs(
            &t.attributes,
            &format!("marker trait {}", t.name),
            filter,
            out,
        ),
        Item::ImplBlock(i) => {
            let target = render_impl_target(i);
            emit_attrs(&i.attributes, &target, filter, out);
            for ii in &i.items {
                if let ImplItem::Method(m) = ii {
                    emit_attrs(
                        &m.attributes,
                        &format!("{}.{}", target, m.name),
                        filter,
                        out,
                    );
                }
            }
        }
        Item::ConstDecl(c) => emit_attrs(&c.attributes, &format!("const {}", c.name), filter, out),
        Item::ModuleBinding(b) => {
            let kw = if b.is_mut { "let mut" } else { "let" };
            emit_attrs(&b.attributes, &format!("{kw} {}", b.name), filter, out)
        }
        Item::TypeAlias(t) => emit_attrs(&t.attributes, &format!("type {}", t.name), filter, out),
        Item::DistinctType(d) => {
            emit_attrs(&d.attributes, &format!("distinct {}", d.name), filter, out)
        }
        Item::ExternFunction(f) => {
            emit_attrs(&f.attributes, &format!("extern fn {}", f.name), filter, out)
        }
        Item::ExternBlock(b) => {
            emit_attrs(&b.attributes, "extern block", filter, out);
            for it in &b.items {
                match it {
                    ExternItem::Function(f) => {
                        emit_attrs(&f.attributes, &format!("extern fn {}", f.name), filter, out)
                    }
                    ExternItem::OpaqueType(o) => emit_attrs(
                        &o.attributes,
                        &format!("extern type {}", o.name),
                        filter,
                        out,
                    ),
                }
            }
        }
        Item::LayoutDef(l) => emit_attrs(&l.attributes, &format!("layout {}", l.name), filter, out),
        Item::TestCase(t) => emit_attrs(&t.attributes, &format!("test {:?}", t.name), filter, out),
        // Effect / use / import / alias / independent decls carry no
        // attributes at the AST level today (slice 2 of item 36's
        // namespace-dispatch walker covers the same set of kinds).
        Item::EffectResource(_)
        | Item::EffectGroup(_)
        | Item::EffectVerbDecl(_)
        | Item::UseDecl(_)
        | Item::Import(_)
        | Item::AliasDecl(_)
        | Item::IndependentDecl(_) => {}
    }
}

fn render_impl_target(i: &crate::ast::ImplBlock) -> String {
    let target = crate::parser::render_type_for_diagnostic(&i.target_type);
    match &i.trait_name {
        Some(t) => format!("impl {} for {}", t.segments.join("."), target),
        None => format!("impl {}", target),
    }
}

fn emit_attrs(
    attrs: &[Attribute],
    attached_to: &str,
    filter: &AttributeQueryFilter,
    out: &mut Vec<AttributeQueryRecord>,
) {
    for attr in attrs {
        if attr.path.len() < 2 {
            continue;
        }
        if let Some(prefix) = &filter.tool_prefix {
            if attr.path[0] != *prefix {
                continue;
            }
        }
        out.push(AttributeQueryRecord {
            path: attr.path.clone(),
            args: attr.args.iter().map(arg_to_record).collect(),
            attached_to: attached_to.to_string(),
            span: attr.span.clone(),
        });
    }
}

fn arg_to_record(a: &AttrArg) -> AttributeQueryArg {
    AttributeQueryArg {
        name: a.name.clone(),
        value: a.value.as_ref().map(|e| classify_value(&e.kind)),
        span: a.span.clone(),
    }
}

fn classify_value(kind: &ExprKind) -> AttributeQueryValue {
    match kind {
        ExprKind::StringLit(s) => AttributeQueryValue::String(s.clone()),
        ExprKind::Integer(n, _) => AttributeQueryValue::Int(*n as i128),
        ExprKind::Bool(b) => AttributeQueryValue::Bool(*b),
        ExprKind::Path { segments, .. } => AttributeQueryValue::Path(segments.join(".")),
        ExprKind::Identifier(name) => AttributeQueryValue::Path(name.clone()),
        _ => AttributeQueryValue::Other,
    }
}
