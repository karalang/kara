// tests/missing_track_caller_lint.rs
//
// Slice 7 of the `#[track_caller]` for stdlib panic-emitters entry:
// `missing_track_caller` stdlib-hygiene lint.
//
// Test surface mirrors `missing_must_use_lint`'s shape — synthetic
// AST + a hand-constructed `EffectCheckResult` rather than the full
// parse/resolve/effectcheck pipeline. The lint is purely
// data-consultative (reads `Function.is_track_caller`, `attributes`,
// `stdlib_origin`, `is_pub`, `effects` + the inferred_effects map) so
// synthetic construction is the cleanest test substrate.

use std::collections::HashMap;

use karac::ast::{
    AttrArg, Attribute, Block, EffectItem, EffectList, EffectVerb, EffectVerbKind, Expr, ExprKind,
    Function, ImplBlock, ImplItem, Item, PathExpr, Program, SelfParam, TypeExpr, TypeKind,
};
use karac::effectchecker::{
    Effect, EffectCheckResult, EffectOrigin, EffectSet, PublicEffectsPolicy,
};
use karac::missing_track_caller_lint::{check_missing_track_caller, LintLevel};
use karac::token::Span;

// ── AST construction helpers ─────────────────────────────────────

fn syn_span() -> Span {
    Span {
        offset: 0,
        length: 0,
        line: 1,
        column: 1,
    }
}

fn path_type(name: &str) -> TypeExpr {
    TypeExpr {
        kind: TypeKind::Path(PathExpr {
            segments: vec![name.to_string()],
            generic_args: None,
            span: syn_span(),
        }),
        span: syn_span(),
    }
}

fn empty_block() -> Block {
    Block {
        stmts: Vec::new(),
        final_expr: None,
        span: syn_span(),
    }
}

fn allow_attr(name: &str) -> Attribute {
    // `#[allow(NAME)]` shape — the lint walker reads either named
    // (`allow(name = X)`) or positional-identifier (`allow(X)`) args;
    // build the positional form here.
    let value_expr = Expr {
        kind: ExprKind::Identifier(name.to_string()),
        span: syn_span(),
    };
    Attribute {
        path: vec!["allow".to_string()],
        args: vec![AttrArg {
            name: None,
            value: Some(value_expr),
            span: syn_span(),
        }],
        span: syn_span(),
        string_value: None,
    }
}

fn panics_effect_list() -> EffectList {
    EffectList {
        items: vec![EffectItem::Verb(EffectVerb {
            kind: EffectVerbKind::Panics,
            resources: Vec::new(),
            span: syn_span(),
        })],
        span: syn_span(),
    }
}

fn make_function(name: &str, stdlib_origin: bool, is_pub: bool) -> Function {
    Function {
        span: syn_span(),
        attributes: Vec::new(),
        doc_comment: None,
        is_pub,
        is_private: false,
        is_unsafe: false,
        name: name.to_string(),
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: Some(path_type("i64")),
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: empty_block(),
        stdlib_origin,
        deprecation: None,
        is_track_caller: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
    }
}

fn make_method(name: &str, stdlib_origin: bool, has_self: bool) -> Function {
    let mut f = make_function(name, stdlib_origin, true);
    if has_self {
        f.self_param = Some(SelfParam::Owned);
    }
    f
}

fn impl_block_inherent(target: &str, methods: Vec<Function>) -> Item {
    Item::ImplBlock(ImplBlock {
        span: syn_span(),
        attributes: Vec::new(),
        generic_params: None,
        trait_name: None,
        target_type: path_type(target),
        where_clause: None,
        items: methods
            .into_iter()
            .map(|m| ImplItem::Method(Box::new(m)))
            .collect(),
        lint_overrides: Vec::new(),
        do_not_recommend: false,
    })
}

fn impl_block_trait(trait_name: &str, target: &str, methods: Vec<Function>) -> Item {
    Item::ImplBlock(ImplBlock {
        span: syn_span(),
        attributes: Vec::new(),
        generic_params: None,
        trait_name: Some(PathExpr {
            segments: vec![trait_name.to_string()],
            generic_args: None,
            span: syn_span(),
        }),
        target_type: path_type(target),
        where_clause: None,
        items: methods
            .into_iter()
            .map(|m| ImplItem::Method(Box::new(m)))
            .collect(),
        lint_overrides: Vec::new(),
        do_not_recommend: false,
    })
}

fn program_with(items: Vec<Item>) -> Program {
    Program {
        items,
        ..Default::default()
    }
}

fn empty_effects() -> EffectCheckResult {
    EffectCheckResult {
        inferred_effects: HashMap::new(),
        declared_effects: HashMap::new(),
        expanded_groups: HashMap::new(),
        transparent_effects: std::collections::HashSet::new(),
        mutual_recursion_groups: Vec::new(),
        function_visibility: HashMap::new(),
        public_effects_policy: PublicEffectsPolicy::Declared,
        errors: Vec::new(),
        queries: Vec::new(),
    }
}

fn effects_with_panics_on(name: &str) -> EffectCheckResult {
    let mut e = empty_effects();
    let mut set = EffectSet::new();
    set.add(
        Effect {
            verb: EffectVerbKind::Panics,
            resource: String::new(),
        },
        EffectOrigin::Direct(syn_span()),
    );
    e.inferred_effects.insert(name.to_string(), set);
    e
}

fn cli() -> karac::lints::CliLintOverrides {
    karac::lints::CliLintOverrides::default()
}

// ── Positive: stdlib pub fn with panics effect, no #[track_caller] ──

#[test]
fn fires_on_stdlib_pub_fn_with_inferred_panics() {
    let mut f = make_function("unwrap", true, true);
    f.is_track_caller = false;
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("unwrap"));
    assert!(diags[0].message.contains("panics"));
    assert!(diags[0].message.contains("`#[track_caller]`"));
    assert_eq!(diags[0].lint_name, "missing_track_caller");
}

#[test]
fn fires_on_stdlib_pub_fn_with_declared_panics() {
    // Declared `with panics` rather than inferred — the lint reads
    // both signal sources.
    let mut f = make_function("unwrap", true, true);
    f.effects = Some(panics_effect_list());
    let prog = program_with(vec![Item::Function(f)]);
    let effects = empty_effects();
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert_eq!(diags.len(), 1);
}

#[test]
fn fires_on_inherent_impl_method() {
    let m = make_method("first", true, true);
    let prog = program_with(vec![impl_block_inherent("Vec", vec![m])]);
    let effects = effects_with_panics_on("Vec.first");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message.contains("Vec.first"));
}

#[test]
fn diagnostic_carries_note_and_help() {
    let f = make_function("unwrap", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert_eq!(diags.len(), 1);
    assert!(diags[0]
        .note
        .as_ref()
        .unwrap()
        .contains("reports the stdlib frame"));
    assert!(diags[0]
        .help
        .as_ref()
        .unwrap()
        .contains("add `#[track_caller]`"));
}

// ── Negative: every exclusion path ───────────────────────────────

#[test]
fn silent_when_track_caller_already_present() {
    let mut f = make_function("unwrap", true, true);
    f.is_track_caller = true;
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_when_allow_attribute_present() {
    let mut f = make_function("unwrap", true, true);
    f.attributes.push(allow_attr("missing_track_caller"));
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_on_user_origin_function() {
    let f = make_function("user_unwrap", false, true);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("user_unwrap");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_on_private_function() {
    let f = make_function("internal_helper", true, false);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("internal_helper");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_when_no_panics_effect() {
    let f = make_function("pure_helper", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    // No panics effect in either declared (the helper above leaves
    // `f.effects = None`) or inferred (empty map).
    let effects = empty_effects();
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_on_trait_impl_method() {
    // Trait-impl methods inherit `#[track_caller]` from the trait
    // method declaration — the lint skips the impl side to avoid
    // forcing every implementor to re-annotate.
    let m = make_method("read", true, true);
    let prog = program_with(vec![impl_block_trait("Reader", "MyReader", vec![m])]);
    let effects = effects_with_panics_on("MyReader.read");
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli());
    assert!(diags.is_empty());
}

#[test]
fn silent_when_effects_argument_is_none() {
    // Effect checker didn't run (earlier phase failed). The lint
    // suppresses entirely rather than firing against stale or absent
    // data.
    let f = make_function("unwrap", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    let diags = check_missing_track_caller(&prog, None, &cli());
    assert!(diags.is_empty());
}

// ── CLI cascade ──────────────────────────────────────────────────

#[test]
fn cli_allow_suppresses() {
    let f = make_function("unwrap", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let cli = karac::lints::CliLintOverrides::with_level(
        "missing_track_caller",
        karac::lints::LintLevel::Allow,
    );
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli);
    assert!(diags.is_empty());
}

#[test]
fn cli_deny_promotes_to_error() {
    let f = make_function("unwrap", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let cli = karac::lints::CliLintOverrides::with_level(
        "missing_track_caller",
        karac::lints::LintLevel::Deny,
    );
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}

#[test]
fn cli_deny_warnings_promotes_to_error() {
    let f = make_function("unwrap", true, true);
    let prog = program_with(vec![Item::Function(f)]);
    let effects = effects_with_panics_on("unwrap");
    let cli = karac::lints::CliLintOverrides::with_deny_warnings();
    let diags = check_missing_track_caller(&prog, Some(&effects), &cli);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].level, LintLevel::Error);
}
