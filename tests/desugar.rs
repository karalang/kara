// tests/desugar.rs

//! Focused tests for the desugar pass (`src/desugar.rs`) — the AST-rewriting
//! pre-resolve passes every compilation crosses.
//!
//! Until these existed (project-review-2026-08-16 item 8), desugar had zero
//! unit tests and no dedicated integration file: a regression surfaced only
//! as downstream phase misbehavior, with poor localization. Each test here
//! parses real source, runs `karac::desugar_program`, and asserts the
//! REWRITTEN AST SHAPE — the pass's actual contract with the phases behind
//! it — rather than end-to-end behavior (which tests/interpreter.rs and
//! tests/codegen.rs already cover).

use karac::ast::*;
use karac::desugar_program;

fn parse_and_desugar(src: &str) -> Program {
    let result = karac::parse(src);
    assert!(
        result.errors.is_empty(),
        "test source must parse clean, got: {:?}",
        result.errors
    );
    let mut program = result.program;
    let comptime_errors = desugar_program(&mut program);
    assert!(
        comptime_errors.is_empty(),
        "desugar/comptime reported errors: {comptime_errors:?}"
    );
    program
}

fn find_fn<'a>(program: &'a Program, name: &str) -> Option<&'a Function> {
    program.items.iter().find_map(|item| match item {
        Item::Function(f) if f.name == name => Some(f),
        _ => None,
    })
}

// ── MultiAssign elimination ─────────────────────────────────────────

/// The contract stated on `StmtKind::MultiAssign` itself: "no phase from the
/// resolver onward ever observes it" — the subtyping walk even panics
/// `unreachable!` on one. Pin that the desugar output honors it.
#[test]
fn multi_assign_is_rewritten_to_a_block_of_temps() {
    let program = parse_and_desugar(
        "fn main() {\n    let mut a = 1;\n    let mut b = 2;\n    a, b = b, a;\n    print(\"{a}{b}\");\n}\n",
    );
    let main = find_fn(&program, "main").expect("main exists");
    for stmt in &main.body.stmts {
        assert!(
            !matches!(stmt.kind, StmtKind::MultiAssign { .. }),
            "MultiAssign must not survive desugar"
        );
    }
    // The rewrite is a block-expr statement holding let-temps + assigns:
    // all values evaluate into temporaries before any target is written.
    let has_swap_block = main.body.stmts.iter().any(|stmt| {
        let StmtKind::Expr(e) = &stmt.kind else {
            return false;
        };
        let ExprKind::Block(b) = &e.kind else {
            return false;
        };
        let temp_lets = b
            .stmts
            .iter()
            .filter(|s| matches!(s.kind, StmtKind::Let { .. }))
            .count();
        let assigns = b
            .stmts
            .iter()
            .filter(|s| matches!(s.kind, StmtKind::Assign { .. }))
            .count();
        temp_lets == 2 && assigns == 2
    });
    assert!(
        has_swap_block,
        "expected a block-expr stmt with 2 let-temps + 2 assigns; got: {:#?}",
        main.body.stmts
    );
}

// ── impl Trait argument desugar ─────────────────────────────────────

/// Argument-position `impl Trait` becomes a fresh anonymous generic with the
/// trait as its bound; no `TypeKind::ImplTrait` survives in the signature.
#[test]
fn impl_trait_arg_becomes_fresh_bounded_generic() {
    let program = parse_and_desugar(
        "trait Show {\n    fn show(ref self) -> i64;\n}\n\nfn takes(x: impl Show) -> i64 {\n    x.show()\n}\n\nfn main() {}\n",
    );
    let takes = find_fn(&program, "takes").expect("takes exists");
    assert!(
        !takes
            .params
            .iter()
            .any(|p| matches!(p.ty.kind, TypeKind::ImplTrait { .. })),
        "ImplTrait must not survive in param position"
    );
    let generics = takes
        .generic_params
        .as_ref()
        .expect("a synthetic generic param list was added");
    assert_eq!(generics.params.len(), 1, "exactly one synthetic generic");
    let g = &generics.params[0];
    assert_eq!(g.bounds.len(), 1, "the trait is the bound");
    assert_eq!(g.bounds[0].path, vec!["Show".to_string()]);
    // The param's type is now a bare path to the synthetic generic.
    let TypeKind::Path(p) = &takes.params[0].ty.kind else {
        panic!("param type should be a path to the synthetic generic");
    };
    assert_eq!(p.segments, vec![g.name.clone()]);
}

// ── #[multiversion] desugar ─────────────────────────────────────────

/// `#[multiversion(baseline, "avx2")] fn f` becomes: `f$avx2` (tagged
/// `#[target_feature]`, unsafe), `f$baseline` (safe, un-widened), and `f`
/// rewritten into a safe dispatch thunk. All three are ordinary functions
/// every later phase sees.
#[test]
fn multiversion_synthesizes_variants_and_dispatch_thunk() {
    let program = parse_and_desugar(
        "#[multiversion(baseline, \"avx2\")]\nfn sum2(a: i64, b: i64) -> i64 {\n    a + b\n}\n\nfn main() {\n    print(\"{sum2(1, 2)}\");\n}\n",
    );
    let variant = find_fn(&program, "sum2$avx2").expect("widened variant synthesized");
    assert!(variant.is_unsafe, "feature variant is unsafe");
    assert!(
        variant
            .attributes
            .iter()
            .any(|a| a.path == vec!["target_feature".to_string()]),
        "feature variant carries #[target_feature]; got: {:?}",
        variant.attributes
    );
    let baseline = find_fn(&program, "sum2$baseline").expect("baseline variant synthesized");
    assert!(!baseline.is_unsafe, "baseline variant stays safe");
    let thunk = find_fn(&program, "sum2").expect("original name remains as the thunk");
    assert!(!thunk.is_unsafe, "dispatch thunk is safe");
    assert!(
        !thunk
            .attributes
            .iter()
            .any(|a| a.path == vec!["multiversion".to_string()]),
        "the multiversion attribute is consumed by the rewrite"
    );
}

// ── Trait default-method synthesis ──────────────────────────────────

/// A trait default method is materialized into every impl that omits it, so
/// later phases never chase a missing method body.
#[test]
fn trait_default_method_is_synthesized_into_impl() {
    let program = parse_and_desugar(
        "trait Greeter {\n    fn greet(ref self) -> i64 {\n        41\n    }\n}\n\nstruct S {\n    x: i64,\n}\n\nimpl Greeter for S {}\n\nfn main() {}\n",
    );
    let imp = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::ImplBlock(i) if i.trait_name.is_some() => Some(i),
            _ => None,
        })
        .expect("the Greeter-for-S impl exists");
    let has_greet = imp.items.iter().any(|it| match it {
        ImplItem::Method(m) => m.name == "greet",
        _ => false,
    });
    assert!(
        has_greet,
        "default method `greet` must be materialized into the impl"
    );
}

// ── #[derive(Default)] synthesis ────────────────────────────────────

/// `#[derive(Default)]` on a struct synthesizes an INHERENT `impl P` (no
/// trait name — see `make_default_impl`) carrying a no-arg `default()`
/// returning `P`.
#[test]
fn derive_default_synthesizes_inherent_default_impl() {
    let program = parse_and_desugar(
        "#[derive(Default)]\nstruct P {\n    x: i64,\n    y: i64,\n}\n\nfn main() {}\n",
    );
    let default_fn = program
        .items
        .iter()
        .find_map(|item| {
            let Item::ImplBlock(i) = item else {
                return None;
            };
            if i.trait_name.is_some() {
                return None;
            }
            let TypeKind::Path(p) = &i.target_type.kind else {
                return None;
            };
            if p.segments.last().map(String::as_str) != Some("P") {
                return None;
            }
            i.items.iter().find_map(|it| match it {
                ImplItem::Method(m) if m.name == "default" => Some(m),
                _ => None,
            })
        })
        .expect("expected a synthesized inherent impl P with fn default()");
    assert!(default_fn.params.is_empty(), "default() takes no params");
    assert!(default_fn.self_param.is_none(), "default() is associated");
}
