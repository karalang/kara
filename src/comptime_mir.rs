//! Compile-time evaluation on the MIR interpreter (B-2026-10-22-20).
//!
//! The v2 pipeline's replacement for [`super::evaluate`]'s tree-walk
//! evaluation: every `comptime { .. }` block, every `#[derive(X)]` backed by
//! a `comptime fn derive_x` and every `#[proto_schema]` constant becomes one
//! root function of an *evaluation program* (the program itself plus those
//! roots), which is resolved, type-checked, lowered to MIR from the roots and
//! run, one root at a time, on the MIR interpreter. The natives the roots
//! reach (`T.fields()`, `ast.item(s)`, `compiler.error(msg)`) live in
//! `crate::mir::interp::comptime`; this module builds the roots, runs them
//! and splices what they returned back into the program:
//!
//! * a block's value folds into a literal at the block (an `Expr` it built
//!   is parsed and spliced instead);
//! * a derive's `Vec[Item]` is parsed and spliced after the derive site;
//! * a schema's `Vec[Item]` replaces the constant.

use std::collections::HashMap;

use super::{ordered_derived_traits, to_snake_case, ComptimeError};
use crate::ast::*;
use crate::mir::interp::comptime::{self as natives, CtValue, ReflField, ReflType, ReflVariant};
use crate::mir::interp::{self, Outcome};
use crate::resolver::SpanKey;
use crate::span_visitor::MutVisitor;
use crate::token::{FloatSuffix, IntSuffix, Span};
use crate::typechecker::types::{FloatSize, IntSize, Type, UIntSize, VariantTypeInfo};
use crate::typechecker::{type_display, TypeCheckResult};

/// The MIR steps one evaluation may take: deferred.md § Comptime — Resource
/// limits sets an instruction ceiling of `2^24` per top-level invocation; a
/// MIR step is finer than a source instruction, so this allows four steps
/// for each.
pub const STEP_LIMIT: u64 = 1 << 26;

/// Where a derive's or a schema's generated items are placed: a high window
/// per fragment, so each generated node keeps a distinct span (the legacy
/// evaluator's rule, B-2026-07-08-15).
const SPLICE_BASE: usize = 1_000_000_000;

/// Whether `span` is in an item a derive or a schema generated: in a splice
/// window, below the library's own spans.
pub fn is_spliced(span: &Span) -> bool {
    (SPLICE_BASE..crate::prelude::gated_span_base(0)).contains(&span.offset)
}

enum JobKind {
    /// A `comptime { .. }` block, by its span, with its type.
    Block { ty: Option<Type> },
    /// A derive's items, spliced after item `after`.
    Derive { after: usize },
    /// A `#[proto_schema]` constant's items, replacing item `at`.
    Schema { at: usize },
}

struct Job {
    kind: JobKind,
    /// Where the work was asked for: the block, the derived type or the
    /// constant.
    site: Span,
    /// The root's name in the evaluation program.
    root: String,
    /// What the diagnostics call it.
    what: String,
}

/// What a whole evaluation left to splice.
#[derive(Default)]
struct Spliced {
    blocks: HashMap<(usize, usize), Expr>,
    derived: Vec<(usize, Vec<Item>)>,
    schemas: Vec<(usize, Vec<Item>)>,
}

/// Evaluates `program`'s comptime work on the MIR interpreter and splices
/// the results in place. `program` is the v2 pipeline's: the library is
/// appended from `library_start` on, it is resolved and type-checked as
/// `typed`, and `src` is its text. `retype` type-checks a program as the
/// caller's pipeline does (the library's methods from source, pruned as the
/// caller pruned them).
pub fn evaluate_on_mir(
    program: &mut Program,
    typed: &TypeCheckResult,
    src: &str,
    schemas_only: bool,
    retype: &dyn Fn(&Program, &crate::resolver::ResolveResult) -> TypeCheckResult,
) -> Vec<ComptimeError> {
    let mut jobs = Vec::new();
    let mut roots: Vec<Item> = Vec::new();
    let mut errors = Vec::new();
    plan(
        program,
        typed,
        schemas_only,
        &mut jobs,
        &mut roots,
        &mut errors,
    );
    if jobs.is_empty() {
        return errors;
    }
    let mut eval = program.clone();
    for item in &mut eval.items {
        if let Item::Function(f) = item {
            if f.is_comptime {
                comptime_params_as_receivers(f);
            }
        }
    }
    eval.items.extend(roots);
    crate::node_ids::assign_node_ids(&mut eval);
    let r = crate::resolve_v2(&eval);
    let tc = retype(&eval, &r);
    let refl = reflection(&eval, &tc);
    let names: Vec<String> = jobs.iter().map(|j| j.root.clone()).collect();
    let user_len = program.library_start;
    let mut lowered =
        crate::mir::lower::lower_program_with_roots(&eval, src, user_len, &tc, &r, &names);
    // A root the builder cannot lower is reported at its own site; the
    // others still run, from a lowering of their own.
    let mut broken: HashMap<String, Vec<String>> = HashMap::new();
    if !lowered.errors.is_empty() {
        let mut fine = Vec::new();
        for name in &names {
            let one = crate::mir::lower::lower_program_with_roots(
                &eval,
                src,
                user_len,
                &tc,
                &r,
                std::slice::from_ref(name),
            );
            if one.errors.is_empty() {
                fine.push(name.clone());
            } else {
                broken.insert(name.clone(), one.errors);
            }
        }
        lowered = crate::mir::lower::lower_program_with_roots(&eval, src, user_len, &tc, &r, &fine);
    }
    let mut spliced = Spliced::default();
    let mut window = SPLICE_BASE;
    for job in &jobs {
        if let Some(why) = broken.get(&job.root) {
            errors.push(ComptimeError {
                message: format!(
                    "error[E_COMPTIME_PANIC]: {} could not be compiled: {}",
                    job.what,
                    why.join("; ")
                ),
                span: job.site,
            });
            continue;
        }
        let (run, diags) = natives::with_state(refl.clone(), || {
            let run = interp::run_untraced(&lowered.program, &lowered.tys, &job.root, Vec::new());
            (run, natives::take_result())
        });
        let (run, result) = run;
        for d in diags {
            let at = Span {
                line: d.line,
                column: d.column,
                offset: d.offset,
                length: d.length,
            };
            let in_library = |s: &Span| user_len.is_some_and(|n| s.offset >= n);
            errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_ERROR]: {}", d.message),
                span: if in_library(&at) && !in_library(&job.site) {
                    job.site
                } else {
                    at
                },
            });
        }
        let value = match run.outcome {
            Outcome::Returned(_) => match result {
                Some(Ok(v)) => v,
                Some(Err(why)) => {
                    errors.push(non_foldable(job, &why));
                    continue;
                }
                None => {
                    errors.push(non_foldable(job, "it returned nothing"));
                    continue;
                }
            },
            Outcome::Error(e) if e.contains("step limit exceeded") => {
                errors.push(ComptimeError {
                    message: format!(
                        "error[E_COMPTIME_ITER_LIMIT_EXCEEDED]: {} did not terminate within its \
                         instruction limit (deferred.md § Comptime — Resource limits)",
                        job.what
                    ),
                    span: job.site,
                });
                continue;
            }
            other => {
                let detail = match other {
                    Outcome::Error(e) => e,
                    _ => {
                        // The panic's message, without the trace after it.
                        let text = run.stderr.trim().lines().next().unwrap_or("").trim();
                        if text.is_empty() {
                            "evaluation failed".to_string()
                        } else {
                            text.to_string()
                        }
                    }
                };
                errors.push(ComptimeError {
                    message: format!("error[E_COMPTIME_PANIC]: {} panicked: {detail}", job.what),
                    span: job.site,
                });
                continue;
            }
        };
        match &job.kind {
            JobKind::Block { ty } => {
                let is_expr = matches!(ty, Some(Type::Named { name, .. }) if name == "Expr");
                let folded = match (&value, is_expr) {
                    (CtValue::Str(text), true) => match super::parse_quoted_expr(text) {
                        Ok(mut e) => {
                            crate::span_visitor::visit_expr_spans_mut(&mut e, &mut |s| {
                                *s = job.site
                            });
                            Ok(e)
                        }
                        Err(why) => Err(format!("ast.expr: could not parse `{text}`: {why}")),
                    },
                    _ => fold(&value, ty.as_ref(), &job.site),
                };
                match folded {
                    Ok(e) => {
                        spliced.blocks.insert((job.site.offset, job.site.length), e);
                    }
                    Err(why) => errors.push(non_foldable(job, &why)),
                }
            }
            JobKind::Derive { after } | JobKind::Schema { at: after } => {
                let texts = match &value {
                    CtValue::Vec(items) | CtValue::Array(items) => items.clone(),
                    CtValue::Str(_) => vec![value.clone()],
                    _ => Vec::new(),
                };
                let mut items = Vec::new();
                let mut ok = true;
                for t in &texts {
                    let CtValue::Str(text) = t else {
                        ok = false;
                        break;
                    };
                    match super::parse_quoted_item(text) {
                        Ok(mut item) => {
                            let base = window;
                            window += 1_000_000;
                            crate::span_visitor::visit_item_spans_mut(&mut item, &mut |s| {
                                s.line = job.site.line;
                                s.column = job.site.column;
                                s.offset = base.wrapping_add(s.offset);
                            });
                            items.push(item);
                        }
                        Err(why) => {
                            errors.push(ComptimeError {
                                message: format!(
                                    "error[E_COMPTIME_PANIC]: {} panicked: ast.item: could not \
                                     parse quoted item `{text}`: {why}",
                                    job.what
                                ),
                                span: job.site,
                            });
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    continue;
                }
                match job.kind {
                    JobKind::Derive { after } => spliced.derived.push((after, items)),
                    _ => spliced.schemas.push((*after, items)),
                }
            }
        }
    }
    splice(program, spliced);
    errors
}

fn non_foldable(job: &Job, why: &str) -> ComptimeError {
    ComptimeError {
        message: format!(
            "error[E_COMPTIME_NON_FOLDABLE_RESULT]: {} produced a value that cannot be spliced \
             as a constant: {why}",
            job.what
        ),
        span: job.site,
    }
}

/// The jobs of `program`, each with its root function.
fn plan(
    program: &Program,
    typed: &TypeCheckResult,
    schemas_only: bool,
    jobs: &mut Vec<Job>,
    roots: &mut Vec<Item>,
    errors: &mut Vec<ComptimeError>,
) {
    let mut push = |errors: &mut Vec<ComptimeError>,
                    kind: JobKind,
                    site: Span,
                    what: String,
                    ret: &str,
                    body: Block| {
        let root = format!("__comptime_job_{}", jobs.len());
        match root_fn(&root, ret, body, site) {
            Ok(f) => {
                roots.push(Item::Function(f));
                jobs.push(Job {
                    kind,
                    site,
                    root,
                    what,
                });
            }
            Err(why) => errors.push(ComptimeError {
                message: format!("error[E_COMPTIME_PANIC]: {what} could not be compiled: {why}"),
                span: site,
            }),
        }
    };
    for (idx, item) in program.items.iter().enumerate() {
        if let Item::ConstDecl(c) = item {
            if c.attributes.iter().any(|a| a.is_bare("proto_schema")) {
                let call = call_expr("proto_parse_schema", vec![c.value.clone()], c.value.span);
                push(
                    errors,
                    JobKind::Schema { at: idx },
                    c.value.span,
                    "proto schema".to_string(),
                    "Vec[Item]",
                    expr_block(call),
                );
            }
        }
    }
    if schemas_only {
        return;
    }
    // `comptime { .. }` blocks, outermost first: an evaluation runs what is
    // nested in it.
    let mut blocks = Vec::new();
    for item in &program.items {
        let mut item = item.clone();
        crate::span_visitor::walk_item_mut_with(&mut item, &mut Blocks(&mut blocks));
    }
    for (span, id, block) in blocks {
        // A block that builds code (`ast.expr(..)`) is typed by what it
        // splices, so its own result is read off its last expression.
        let quoted = block
            .final_expr
            .as_ref()
            .and_then(|e| typed.node_types.get(&e.id));
        let ty = match quoted {
            Some((t @ Type::Named { name, .. }, _)) if name == "Expr" => Some(t.clone()),
            _ => typed
                .node_types
                .get(&id)
                .map(|(t, _)| t.clone())
                .or_else(|| typed.expr_types.get(&SpanKey::from_span(&span)).cloned()),
        };
        let ret = match &ty {
            Some(Type::Error) | None => {
                errors.push(ComptimeError {
                    message: "error[E_COMPTIME_NON_FOLDABLE_RESULT]: `comptime` block has no \
                              known type"
                        .to_string(),
                    span,
                });
                continue;
            }
            Some(t) => type_display(t),
        };
        push(
            errors,
            JobKind::Block { ty },
            span,
            "`comptime` evaluation".to_string(),
            &ret,
            block,
        );
    }
    let derive_fns = super::collect_derive_fns(program);
    if derive_fns.is_empty() {
        return;
    }
    let mut quoted = 0;
    for (idx, item) in program.items.iter().enumerate() {
        let (type_name, attributes, site) = match item {
            Item::StructDef(s) => (&s.name, &s.attributes, s.span),
            Item::EnumDef(e) => (&e.name, &e.attributes, e.span),
            _ => continue,
        };
        for trait_name in ordered_derived_traits(attributes) {
            let fn_name = format!("derive_{}", to_snake_case(&trait_name));
            if !derive_fns.contains(&fn_name) {
                continue;
            }
            quoted += 1;
            let call = match quoted_call(&format!("{fn_name}({type_name})"), site, quoted) {
                Ok(c) => c,
                Err(why) => {
                    errors.push(ComptimeError {
                        message: format!("error[E_COMPTIME_PANIC]: derive `{fn_name}`: {why}"),
                        span: site,
                    });
                    continue;
                }
            };
            push(
                errors,
                JobKind::Derive { after: idx },
                site,
                format!("derive `{fn_name}`"),
                "Vec[Item]",
                expr_block(call),
            );
        }
    }
}

/// Collects the outermost `comptime { .. }` blocks, by span.
struct Blocks<'a>(&'a mut Vec<(Span, crate::ids::NodeId, Block)>);

impl MutVisitor for Blocks<'_> {
    fn span(&mut self, _: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        if let ExprKind::Comptime(b) = &e.kind {
            self.0.push((e.span, e.id, b.clone()));
            // Not walked into: what is nested runs with the block.
            e.kind = ExprKind::Tuple(Vec::new());
        }
    }
}

/// Replaces the folded blocks.
struct Fold<'a>(&'a HashMap<(usize, usize), Expr>);

impl MutVisitor for Fold<'_> {
    fn span(&mut self, _: &mut Span) {}
    fn expr(&mut self, e: &mut Expr) {
        if matches!(e.kind, ExprKind::Comptime(_)) {
            if let Some(folded) = self.0.get(&(e.span.offset, e.span.length)) {
                *e = folded.clone();
            }
        }
    }
}

fn splice(program: &mut Program, spliced: Spliced) {
    if !spliced.blocks.is_empty() {
        for item in &mut program.items {
            crate::span_visitor::walk_item_mut_with(item, &mut Fold(&spliced.blocks));
        }
    }
    // Last to first, so earlier indices stay put. A schema replaces its
    // constant; a derive's items follow its type.
    let mut edits: Vec<(usize, bool, Vec<Item>)> = spliced
        .derived
        .into_iter()
        .map(|(i, items)| (i, false, items))
        .chain(
            spliced
                .schemas
                .into_iter()
                .map(|(i, items)| (i, true, items)),
        )
        .collect();
    edits.sort_by_key(|(i, replace, _)| (*i, *replace));
    for (idx, replace, items) in edits.into_iter().rev() {
        let at = if replace {
            program.items.remove(idx);
            idx
        } else {
            idx + 1
        };
        for (k, it) in items.into_iter().enumerate() {
            program.items.insert(at + k, it);
        }
    }
}

/// The call `text`, its nodes at distinct offsets in a window of their own
/// (a type checker table keyed by span would otherwise take the callee and
/// its argument for one node), at `site`'s line and column.
fn quoted_call(text: &str, site: Span, k: usize) -> Result<Expr, String> {
    let mut e = super::parse_quoted_expr(text)?;
    let base = 1_500_000_000 + k * 10_000;
    crate::span_visitor::visit_expr_spans_mut(&mut e, &mut |s| {
        s.line = site.line;
        s.column = site.column;
        s.offset = base.wrapping_add(s.offset);
    });
    Ok(e)
}

/// `comptime fn <name>() -> <ret> <body>`.
fn root_fn(name: &str, ret: &str, body: Block, site: Span) -> Result<Function, String> {
    let text = format!("comptime fn {name}() -> {ret} {{ }}");
    let mut item = super::parse_quoted_item(&text)?;
    crate::span_visitor::visit_item_spans_mut(&mut item, &mut |s| *s = site);
    let Item::Function(mut f) = item else {
        return Err(format!("`{text}` is not a function"));
    };
    f.body = body;
    Ok(f)
}

fn call_expr(name: &str, args: Vec<Expr>, span: Span) -> Expr {
    Expr {
        id: crate::ids::NodeId::DUMMY,
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                id: crate::ids::NodeId::DUMMY,
                kind: ExprKind::Identifier(name.to_string()),
                span,
            }),
            args: args
                .into_iter()
                .map(|value| CallArg {
                    label: None,
                    mut_marker: false,
                    mut_marker_span: None,
                    span: value.span,
                    value,
                })
                .collect(),
        },
        span,
    }
}

fn expr_block(e: Expr) -> Block {
    Block {
        stmts: Vec::new(),
        span: e.span,
        final_expr: Some(Box::new(e)),
    }
}

/// `T.name()` on a `comptime T: Type` parameter parses as a path call; the
/// evaluation program spells it as the method call on the value it is.
fn comptime_params_as_receivers(f: &mut Function) {
    let names: Vec<String> = f
        .params
        .iter()
        .filter(|p| p.is_comptime)
        .filter_map(|p| match &p.pattern.kind {
            PatternKind::Binding(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        return;
    }
    struct Receivers<'a>(&'a [String]);
    impl MutVisitor for Receivers<'_> {
        fn span(&mut self, _: &mut Span) {}
        fn expr(&mut self, e: &mut Expr) {
            let ExprKind::Call { callee, args } = &mut e.kind else {
                return;
            };
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return;
            };
            let [p, m] = segments.as_slice() else {
                return;
            };
            if !self.0.contains(p) {
                return;
            }
            let object = Expr {
                id: crate::ids::NodeId::DUMMY,
                kind: ExprKind::Identifier(p.clone()),
                span: callee.span,
            };
            e.kind = ExprKind::MethodCall {
                object: Box::new(object),
                method: m.clone(),
                turbofish: None,
                args: std::mem::take(args),
                args_close_span: e.span,
            };
        }
    }
    crate::span_visitor::walk_function_mut_with(f, &mut Receivers(&names));
}

/// The program's structs, enums and unions, for the reflection methods.
fn reflection(program: &Program, tc: &TypeCheckResult) -> natives::Reflection {
    let attrs = |a: &[Attribute]| a.iter().map(crate::typechecker::render_attribute).collect();
    let mut item_attrs: HashMap<&str, Vec<String>> = HashMap::new();
    let mut variant_attrs: HashMap<(&str, &str), Vec<String>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::StructDef(s) => {
                item_attrs.insert(&s.name, attrs(&s.attributes));
            }
            Item::EnumDef(e) => {
                item_attrs.insert(&e.name, attrs(&e.attributes));
                for v in &e.variants {
                    variant_attrs.insert((&e.name, &v.name), attrs(&v.attributes));
                }
            }
            _ => {}
        }
    }
    let field = |(name, ty, is_pub): &(String, Type, bool),
                 fa: Option<&HashMap<String, Vec<String>>>| ReflField {
        name: name.clone(),
        ty: type_display(ty),
        is_pub: *is_pub,
        attrs: fa.and_then(|m| m.get(name).cloned()).unwrap_or_default(),
    };
    let mut types = rustc_hash::FxHashMap::default();
    for (name, s) in &tc.struct_info {
        let mut derives: Vec<String> = s.derived_traits.iter().cloned().collect();
        derives.sort();
        types.insert(
            name.clone(),
            ReflType {
                is_struct: true,
                generic_params: s.generic_params.clone(),
                derives,
                attrs: item_attrs.get(name.as_str()).cloned().unwrap_or_default(),
                fields: s
                    .fields
                    .iter()
                    .map(|f| field(f, Some(&s.field_attrs)))
                    .collect(),
                ..Default::default()
            },
        );
    }
    for (name, u) in &tc.union_info {
        types.insert(
            name.clone(),
            ReflType {
                is_union: true,
                fields: u.fields.iter().map(|f| field(f, None)).collect(),
                ..Default::default()
            },
        );
    }
    for (name, e) in &tc.enum_info {
        let mut derives: Vec<String> = e.derived_traits.iter().cloned().collect();
        derives.sort();
        let variants = e
            .variants
            .iter()
            .map(|(vname, info)| {
                let (field_names, field_types): (Vec<String>, Vec<String>) = match info {
                    VariantTypeInfo::Unit => (Vec::new(), Vec::new()),
                    VariantTypeInfo::Tuple(tys) => tys
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (i.to_string(), type_display(t)))
                        .unzip(),
                    VariantTypeInfo::Struct(fs) => {
                        fs.iter().map(|(n, t)| (n.clone(), type_display(t))).unzip()
                    }
                };
                ReflVariant {
                    name: vname.clone(),
                    field_count: field_names.len() as i64,
                    payload: match info {
                        VariantTypeInfo::Tuple(tys) if tys.len() == 1 => type_display(&tys[0]),
                        _ => String::new(),
                    },
                    field_names,
                    field_types,
                    attrs: variant_attrs
                        .get(&(name.as_str(), vname.as_str()))
                        .cloned()
                        .unwrap_or_default(),
                }
            })
            .collect();
        types.insert(
            name.clone(),
            ReflType {
                is_enum: true,
                generic_params: e.generic_params.clone(),
                derives,
                attrs: item_attrs.get(name.as_str()).cloned().unwrap_or_default(),
                variants,
                ..Default::default()
            },
        );
    }
    natives::Reflection { types }
}

/// A comptime value as the literal it folds to, at type `ty`.
fn fold(value: &CtValue, ty: Option<&Type>, span: &Span) -> Result<Expr, String> {
    let kind = match value {
        CtValue::Int(n) => ExprKind::Integer(*n, int_suffix(ty)),
        CtValue::Float(f) => ExprKind::Float(*f, float_suffix(ty)),
        CtValue::Bool(b) => ExprKind::Bool(*b),
        CtValue::Char(c) => ExprKind::CharLit(*c),
        CtValue::Str(s) => ExprKind::StringLit(s.clone()),
        CtValue::Unit => ExprKind::Tuple(Vec::new()),
        CtValue::Tuple(items) => {
            let tys = match ty {
                Some(Type::Tuple(ts)) => Some(ts.as_slice()),
                _ => None,
            };
            ExprKind::Tuple(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| fold(v, tys.and_then(|ts| ts.get(i)), span))
                    .collect::<Result<_, _>>()?,
            )
        }
        CtValue::Vec(items) | CtValue::Array(items) => {
            let elem = match ty {
                Some(Type::Array { element, .. }) => Some((**element).clone()),
                Some(Type::Named { name, args }) if name == "Vec" && args.len() == 1 => {
                    Some(args[0].clone())
                }
                _ => None,
            };
            let folded = items
                .iter()
                .map(|v| fold(v, elem.as_ref(), span))
                .collect::<Result<Vec<_>, _>>()?;
            // `[..]` is a `Vec` unless its context makes it an array.
            ExprKind::ArrayLiteral(folded)
        }
        CtValue::Struct(name, _) => {
            return Err(format!(
                "a `{name}` struct value (struct folding is a later comptime substrate)"
            ))
        }
        CtValue::Variant(name, _, _) => {
            return Err(format!(
                "a `{name}` enum value (enum folding is a later comptime substrate)"
            ))
        }
    };
    Ok(Expr {
        id: crate::ids::NodeId::DUMMY,
        kind,
        span: *span,
    })
}

fn int_suffix(ty: Option<&Type>) -> Option<IntSuffix> {
    match ty? {
        Type::Int(IntSize::I8) => Some(IntSuffix::I8),
        Type::Int(IntSize::I16) => Some(IntSuffix::I16),
        Type::Int(IntSize::I32) => Some(IntSuffix::I32),
        // `i64` is an unsuffixed literal's type already.
        Type::Int(IntSize::I64) => None,
        Type::Int(IntSize::I128) => Some(IntSuffix::I128),
        Type::Int(IntSize::Isize) => Some(IntSuffix::Isize),
        Type::UInt(UIntSize::U8) => Some(IntSuffix::U8),
        Type::UInt(UIntSize::U16) => Some(IntSuffix::U16),
        Type::UInt(UIntSize::U32) => Some(IntSuffix::U32),
        Type::UInt(UIntSize::U64) => Some(IntSuffix::U64),
        Type::UInt(UIntSize::U128) => Some(IntSuffix::U128),
        Type::UInt(UIntSize::Usize) => Some(IntSuffix::Usize),
        _ => None,
    }
}

fn float_suffix(ty: Option<&Type>) -> Option<FloatSuffix> {
    match ty? {
        Type::Float(FloatSize::F32) => Some(FloatSuffix::F32),
        Type::Float(FloatSize::F64) => None,
        _ => None,
    }
}
