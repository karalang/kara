//! A bare constructor's parameter that nothing decided (`Err(-100)`'s `T`,
//! a `None`'s `T`) is solved by the other operand of a comparison, or by
//! the arguments of a method called on it, as Rust's inference solves it
//! (design.md § Type inference; B-2026-11-19-58, B-2026-11-19-59).

use karac::{check_source, run_playground};

fn runs(src: &str, want: &str) {
    let errs: Vec<String> = check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert!(errs.is_empty(), "{src}\n{errs:#?}");
    let r = run_playground(src);
    assert!(r.ok, "{src}\n{:#?}", r.diagnostics);
    assert_eq!(r.stdout.concat(), want, "{src}");
}

#[test]
fn ok_and_err_compared_inline_share_one_result_type() {
    runs("fn main() { println(Ok(100) == Err(-100)); }\n", "false\n");
    runs("fn main() { println(Err(-100) != Ok(100)); }\n", "true\n");
    runs(
        "fn main() { println(Err(\"x\".to_string()) == Ok(1)); }\n",
        "false\n",
    );
    runs(
        "fn main() { let s = Ok(\"ab\".to_string()); println(s == Err(3)); }\n",
        "false\n",
    );
}

#[test]
fn ordering_an_open_operand_against_a_decided_one() {
    runs(
        "fn main() { let a = None; let b = Some(2); println(a < b); }\n",
        "true\n",
    );
    runs("fn main() { println(Ok(1) < Err(0)); }\n", "true\n");
}

#[test]
fn methods_on_a_bare_none() {
    runs("fn main() { println(None.unwrap_or(4)); }\n", "4\n");
    runs("fn main() { println(None.or(Some(3))); }\n", "Some(3)\n");
    runs(
        "fn main() { let x = None.or(Some(3)); println(x); }\n",
        "Some(3)\n",
    );
    runs(
        "fn main() { println(None.or_else(|| Some(5))); }\n",
        "Some(5)\n",
    );
    runs(
        "fn main() { println(None.map_or(1, |x: i64| x + 1)); }\n",
        "1\n",
    );
    runs("fn main() { println(None.is_none()); }\n", "true\n");
}

#[test]
fn an_empty_map_or_set_compares_with_a_filled_one() {
    runs(
        "fn main() { println(Map.new() == Map.from([(1, 2)])); }\n",
        "false\n",
    );
    runs(
        "fn main() { println(Set.new() == Set.from([1])); }\n",
        "false\n",
    );
}

#[test]
fn an_operand_parameter_nothing_fixes_is_refused_for_every_operator() {
    for src in [
        "fn main() { println(None == None); }\n",
        "fn main() { println(Err(1) == Err(2)); }\n",
        "fn main() { println(Err(1) < Err(2)); }\n",
        "fn main() { let a = None; let b = None; println(a == b); }\n",
    ] {
        let errs: Vec<String> = check_source(src)
            .into_iter()
            .filter(|d| d.severity == "error")
            .map(|d| d.message)
            .collect();
        assert!(
            errs.iter()
                .any(|m| m.contains("cannot infer type parameter")),
            "{src}\n{errs:#?}"
        );
    }
}
