//! B-2026-11-18-26: `#[deprecated]` on an enum, a trait or a type alias
//! warns at every use (design.md § `#[deprecated]`): the alias where it is
//! named, the enum at its type name, each variant constructor and each
//! pattern, and the trait at each bound and `impl`. Each use warns once,
//! though a signature is lowered more than once, and `#[allow(deprecated)]`
//! on the enclosing item silences it.

use karac::check_source;

/// The `deprecated` warnings for `src`, as `line:column` of each, in order.
fn deprecated_at(src: &str) -> Vec<String> {
    let ds = check_source(src);
    assert!(
        ds.iter().all(|d| d.severity != "error"),
        "{src}\n{:#?}",
        ds.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
    ds.iter()
        .filter(|d| d.severity == "warning" && d.message.starts_with("use of deprecated item"))
        .map(|d| {
            format!(
                "{}:{} {}",
                d.line,
                d.column,
                d.message.split(':').next().unwrap()
            )
        })
        .collect()
}

#[test]
fn a_deprecated_trait_warns_at_each_bound_and_impl() {
    let src = "#[deprecated(note: \"use T2\")]
trait T1 {
    fn f(self) -> i64;
}
struct S {}
impl T1 for S {
    fn f(self) -> i64 {
        1
    }
}
fn use_it[T: T1](x: T) -> i64 {
    x.f()
}
fn where_it[T](x: T) -> i64 where T: T1 {
    x.f()
}
fn main() {
    println(use_it(S {}) + where_it(S {}));
}
";
    assert_eq!(
        deprecated_at(src),
        [
            "6:6 use of deprecated item `T1`",
            "11:14 use of deprecated item `T1`",
            "14:38 use of deprecated item `T1`",
        ],
    );
    let quiet = src.replace("impl T1 for S", "#[allow(deprecated)]\nimpl T1 for S");
    assert_eq!(deprecated_at(&quiet).len(), 2, "{quiet}");
}

#[test]
fn a_deprecated_alias_warns_where_it_is_named() {
    assert_eq!(
        deprecated_at(
            "#[deprecated(note: \"use i64\")]
type Old = i64;
fn f(x: Old) -> Old {
    x
}
fn main() {
    println(f(1));
}
"
        ),
        [
            "3:9 use of deprecated item `Old`",
            "3:17 use of deprecated item `Old`"
        ],
    );
}

#[test]
fn a_deprecated_enum_warns_at_its_name_constructors_and_patterns() {
    assert_eq!(
        deprecated_at(
            "#[deprecated(note: \"use F\")]
enum E {
    A,
    B,
}
fn g(e: E) -> i64 {
    match e {
        E.A => 1,
        E.B => 2,
    }
}
fn main() {
    let e = E.A;
    println(g(e));
}
"
        ),
        [
            "6:9 use of deprecated item `E`",
            "8:9 use of deprecated item `E`",
            "9:9 use of deprecated item `E`",
            "13:13 use of deprecated item `E`",
        ],
    );
}
