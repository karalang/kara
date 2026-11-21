//! `(1..100).collect().iter()` (kata 2855): a `collect()` with nothing to
//! say what it builds, a method called straight on it. The check used to
//! say "no method 'iter' on type parameter 'C'; add a trait bound declaring
//! it", naming the type parameter of `collect`'s own signature and a
//! remedy the program does not need. It now says the `collect()` has no
//! target type and how to give it one.

#[test]
fn kata_2855_a_method_on_a_collect_with_no_target_names_the_target() {
    let src = "fn main() {\n\
               \x20   let total: i64 = (1..100).collect().iter().sum();\n\
               \x20   println(f\"{total}\");\n}\n";
    let said: Vec<String> = karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert!(
        said.iter()
            .any(|m| m.contains("a `collect()` with no target type")
                && m.contains("let v: Vec[T] = ...collect();")),
        "{said:?}"
    );
    assert!(
        !said.iter().any(|m| m.contains("add a trait bound")),
        "the trait-bound advice is gone: {said:?}"
    );
}
