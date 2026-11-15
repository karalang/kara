//! A `///` before a method's receiver is ignored (design.md §3 Doc
//! comments, B-2026-11-15-24): the receiver is not a documented parameter,
//! and a doc comment on the parameter after it still attaches there. It
//! was a parse error naming `self` / `ref` as a reserved keyword.

use karac::ast::*;

fn method(src: &str) -> Function {
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let Some(Item::ImplBlock(imp)) = parsed.program.items.into_iter().next() else {
        panic!("expected an impl");
    };
    match imp.items.into_iter().next() {
        Some(ImplItem::Method(f)) => *f,
        _ => panic!("expected one method"),
    }
}

#[test]
fn doc_comment_before_self_is_ignored_and_the_next_param_keeps_its_own() {
    let f = method(
        "impl P {\n\
           /// The method.\n\
           fn a(\n\
             /// on self\n\
             self,\n\
             /// on n\n\
             n: i64,\n\
           ) -> i64 { n }\n\
         }",
    );
    assert!(f.self_param.is_some());
    assert_eq!(f.doc_comment.as_deref(), Some("The method."));
    assert_eq!(f.params.len(), 1);
    assert_eq!(f.params[0].doc_comment.as_deref(), Some("on n"));
}

#[test]
fn doc_comment_before_each_receiver_mode_is_ignored() {
    for recv in ["ref self", "mut ref self", "own self", "frozen self"] {
        let f = method(&format!("impl P {{ fn b(/// d\n {recv}) -> i64 {{ 1 }} }}"));
        assert!(f.self_param.is_some(), "{recv}");
        assert!(f.params.is_empty(), "{recv}");
    }
}

#[test]
fn doc_comment_before_a_plain_first_param_still_attaches() {
    let f = method("impl P { fn c(/// on m\n m: i64) -> i64 { m } }");
    assert!(f.self_param.is_none());
    assert_eq!(f.params[0].doc_comment.as_deref(), Some("on m"));
}
