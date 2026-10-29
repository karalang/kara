//! The front end of effect variables (design.md § Effect variables): `[with E]`
//! declared on structs, enums, impls, functions and methods; the solved set
//! written as a type argument (`EventBus[with writes(OrderDb)]`); and the
//! resolver's kind errors. Solving and charging the variable is the effect
//! checker's, pinned elsewhere.

use karac::ast::{EffectItem, ImplBlock, Item, TypeExpr};
use karac::resolver::{ResolveError, ResolveErrorKind};
use karac::{parse, resolve};

const BUS: &str = "effect resource OrderDb;
struct Event { n: i64 }

struct EventBus[with E] {
    handlers: Vec[Fn(Event) with E],
}

impl[with E] EventBus[with E] {
    fn register(mut ref self, h: escaping Fn(Event) with E) {
        self.handlers.push(h);
    }

    fn emit(self, e: Event) with E {
        for h in self.handlers { h(e); }
    }
}
";

fn parsed(src: &str) -> karac::ast::Program {
    let p = parse(src);
    let errs: Vec<String> = p.errors.iter().map(|e| e.to_string()).collect();
    assert!(errs.is_empty(), "parse errors: {errs:?}");
    p.program
}

fn resolve_errors(src: &str) -> Vec<ResolveError> {
    let program = parsed(src);
    resolve(&program)
        .errors
        .into_iter()
        .filter(|e| !karac::resolver::resolve_kind_is_note(&e.kind))
        .collect()
}

fn only_error(src: &str, kind: ResolveErrorKind) -> ResolveError {
    let errs = resolve_errors(src);
    assert_eq!(errs.len(), 1, "want one error, got {errs:?}");
    let err = errs.into_iter().next().unwrap();
    assert_eq!(err.kind, kind, "{err:?}");
    err
}

fn impl_target(program: &karac::ast::Program) -> &TypeExpr {
    program
        .items
        .iter()
        .find_map(|it| match it {
            Item::ImplBlock(ImplBlock { target_type, .. }) => Some(target_type),
            _ => None,
        })
        .expect("an impl block")
}

#[test]
fn the_spec_event_bus_parses_and_resolves() {
    let src = format!(
        "{BUS}
fn audit(db: OrderDb) {{
    let mut bus = EventBus {{ handlers: Vec.new() }};
    bus.register(|e| println(e.n));
    bus.emit(Event {{ n: 7 }});
}}
fn main() {{ println(1); }}
"
    );
    let program = parsed(&src);
    let target = impl_target(&program);
    let args = target
        .effect_args()
        .expect("`EventBus[with E]` keeps its effect argument");
    assert!(
        matches!(args.items.as_slice(), [EffectItem::Variable(v)] if v == "E"),
        "{args:?}"
    );
    let errs = resolve_errors(&src);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_solved_effect_set_written_as_a_type_argument_resolves() {
    let src = format!(
        "{BUS}
fn count(b: ref EventBus[with writes(OrderDb)]) -> i64 {{ b.handlers.len() as i64 }}
fn main() {{
    let b: EventBus[with writes(OrderDb)] = EventBus {{ handlers: Vec.new() }};
    println(count(b));
}}
"
    );
    let errs = resolve_errors(&src);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn a_clause_combines_an_effect_variable_with_a_named_effect() {
    let src = "effect resource Log;
fn apply[with E](f: Fn(i64) -> i64 with E, x: i64) -> i64 with E writes(Log) { f(x) }
fn main() { println(1); }
";
    let errs = resolve_errors(src);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn an_enum_declares_an_effect_variable() {
    let src = "enum Step[with E] { Run(Fn(i64) with E), Stop }
fn main() { println(1); }
";
    let errs = resolve_errors(src);
    assert!(errs.is_empty(), "{errs:?}");
}

#[test]
fn an_effect_variable_used_as_a_type_is_a_kind_error() {
    let err = only_error(
        "fn f[with E](x: E) -> i64 { 1 }\nfn main() { println(1); }\n",
        ResolveErrorKind::EffectVarKindMismatch,
    );
    assert_eq!(
        err.message,
        "`E` is an effect variable, not a type; it goes in a `with` clause (`Fn(T) with E`) \
         or a type's effect argument (`S[with E]`)"
    );
}

#[test]
fn a_type_parameter_used_as_an_effect_set_is_a_kind_error() {
    let err = only_error(
        "fn g[T](f: Fn(i64) -> i64 with T, x: i64) -> i64 { f(x) }\nfn main() { println(1); }\n",
        ResolveErrorKind::EffectVarKindMismatch,
    );
    assert_eq!(
        err.message,
        "`T` is a type parameter, not an effect variable; an effect set is declared `[with T]`"
    );
}

#[test]
fn a_type_that_declares_no_effect_variable_takes_no_with_argument() {
    let err = only_error(
        "effect resource Db;
struct Pair[T] { a: T }
fn h(p: Pair[i64, with writes(Db)]) -> i64 { 1 }
fn main() { println(1); }
",
        ResolveErrorKind::EffectVarKindMismatch,
    );
    assert_eq!(
        err.message,
        "`Pair` declares no effect variable, so it takes no `with` argument"
    );
}

#[test]
fn a_trait_with_an_effect_variable_is_refused_once() {
    // The trait's methods still see `E`, so the one error is the declaration
    // and not also each `with E` as an undefined name.
    let err = only_error(
        "trait Bus[with E] { fn emit(ref self) with E; }\nfn main() { println(1); }\n",
        ResolveErrorKind::TraitEffectVarNotAllowed,
    );
    assert!(
        err.message
            .starts_with("a trait takes no effect variable (`E`) in v1"),
        "{err:?}"
    );
}

#[test]
fn a_public_signature_spells_the_effect_set_of_a_type_that_declares_one() {
    let err = only_error(
        "pub struct Bus[with E] { pub hs: Vec[Fn(i64) with E] }
pub fn make() -> Bus { Bus { hs: Vec.new() } }
fn main() { println(1); }
",
        ResolveErrorKind::EffectArgsMissingInPublicSignature,
    );
    assert_eq!(
        err.message,
        "public function `make` writes `Bus` without its effect set; a public signature spells \
         it, as `Bus[with ...]` (or `Bus[with E]` for a variable the function declares)"
    );
    for sig in [
        "pub fn make() -> Bus[with writes(Db)] { Bus { hs: Vec.new() } }",
        "pub fn make[with E]() -> Bus[with E] { Bus { hs: Vec.new() } }",
        "fn make() -> Bus { Bus { hs: Vec.new() } }",
    ] {
        let src = format!(
            "effect resource Db;
pub struct Bus[with E] {{ pub hs: Vec[Fn(i64) with E] }}
{sig}
fn main() {{ println(1); }}
"
        );
        let errs = resolve_errors(&src);
        assert!(errs.is_empty(), "{sig}: {errs:?}");
    }
}

#[test]
fn an_effect_argument_outside_a_type_is_a_parse_error() {
    let p = parse("fn main() { let b = Vec[with E].new(); }\n");
    let errs: Vec<String> = p.errors.iter().map(|e| e.message.clone()).collect();
    assert_eq!(
        errs,
        [
            "an effect argument (`with ...`) is written on a type, such as `EventBus[with E]`, \
          not in this bracket list"
        ],
    );
}

#[test]
fn a_struct_declares_at_most_one_effect_variable_and_a_function_several() {
    // design.md § Effect variables (ruled 85d53d9b6): a type's set is one
    // `with` argument, so a struct or enum declares one variable.
    let err = only_error(
        "struct Two[with E, F] { a: Vec[Fn(i64) with E], b: Vec[Fn(i64) with F] }
fn main() { println(1); }
",
        ResolveErrorKind::EffectVarKindMismatch,
    );
    assert_eq!(
        err.message,
        "`Two` declares a second effect variable `F`; a struct or enum declares at most one \
         in v1, since its effect set is written as one `with` argument (`Two[with ...]`)"
    );
    let errs = resolve_errors(
        "fn both[with E, F](f: Fn(i64) with E, g: Fn(i64) with F) with E F { f(1); g(2); }
fn main() { println(1); }
",
    );
    assert!(errs.is_empty(), "{errs:?}");
}
