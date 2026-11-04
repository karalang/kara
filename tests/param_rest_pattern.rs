//! design.md § Destructuring parameters: a parameter's struct pattern may end
//! in `..` (`fn inspect(Request { headers, .. }: Request)`), and a `..` that
//! is not last is refused as in any struct pattern (B-2026-11-04-63: the
//! parameter parser refused every `..`). The run-time behaviour is pinned by
//! corpus/fixtures/params/destructure_with_rest.

use karac::ast::{Item, PatternKind};

#[test]
fn a_parameter_struct_pattern_ends_in_a_rest() {
    let parsed = karac::parse("struct R { a: i64, b: i64 }\nfn f(R { a, .. }: R) -> i64 { a }\n");
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let f = parsed
        .program
        .items
        .iter()
        .find_map(|i| match i {
            Item::Function(f) if f.name == "f" => Some(f),
            _ => None,
        })
        .unwrap();
    match &f.params[0].pattern.kind {
        PatternKind::Struct {
            fields, has_rest, ..
        } => {
            assert!(*has_rest);
            assert_eq!(fields.len(), 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_parameter_rest_before_a_field_is_refused() {
    let parsed =
        karac::parse("struct R { a: i64, b: i64 }\nfn f(R { a, .., b }: R) -> i64 { a }\n");
    assert!(
        parsed
            .errors
            .iter()
            .any(|e| e.to_string().contains("E_REST_PATTERN_NOT_LAST")),
        "{:?}",
        parsed.errors
    );
}
