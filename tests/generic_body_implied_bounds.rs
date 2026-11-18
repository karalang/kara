//! B-2026-11-18-25: `==`, `!=`, `<`, `<=`, `>`, `>=` and an f-string
//! interpolation on a value of an unbounded type parameter (design.md §8
//! Bounds and generic bodies).
//!
//! - At the body: a `deprecated` warning in 0.8.x whose machine fix adds the
//!   bound where the parameter is declared (an error at the next minor).
//! - At each call that gives the parameter a type without the trait,
//!   directly or through a generic caller passing its own parameter on: an
//!   error now, naming the call, the type and the trait. Before this, such a
//!   call compared or printed the value by its shape; two `Secret[String]`s
//!   compared equal in variable time (one printed as `<redacted>`).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-generic-implied-bounds-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn deprecated_lines(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("warning[deprecated]") && l.contains("a generic body uses only"))
        .map(str::to_string)
        .collect()
}

fn call_errors(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("error") && l.contains("this call gives"))
        .map(str::to_string)
        .collect()
}

#[test]
fn an_unbounded_comparison_or_interpolation_warns_and_the_fix_adds_the_bound() {
    let path = scratch(
        "fixable.kara",
        "fn both[T](x: T, y: T) -> bool { x == y or x < y }
fn showb[T: Clone](x: T) -> String { f\"{x}\" }
fn twice[T](x: T, y: T) -> bool { x == y and x != y }
fn main() {
    println(both(1, 2));
    println(showb(3));
    println(twice(1, 1));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(deprecated_lines(&text(&check)).len(), 5, "{}", text(&check));
    assert!(call_errors(&text(&check)).is_empty(), "{}", text(&check));
    let before = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&before.stdout), "true\n3\nfalse\n");
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("fn both[T: PartialEq + PartialOrd](x"),
        "{src}"
    );
    assert!(src.contains("fn showb[T: Clone + Display](x"), "{src}");
    assert!(src.contains("fn twice[T: PartialEq](x"), "{src}");
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(
        deprecated_lines(&text(&check)).is_empty(),
        "{}",
        text(&check)
    );
    let after = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&after.stdout), "true\n3\nfalse\n");
}

#[test]
fn a_call_giving_an_unbounded_parameter_a_type_without_the_trait_is_refused() {
    let path = scratch(
        "row.kara",
        "struct S { x: i64 }
fn f[T](x: T, y: T) -> bool { x == y }
fn g[T](x: T, y: T) -> bool { x < y }
fn h[T](x: T) -> String { f\"{x}\" }
fn main() {
    println(f(S { x: 1 }, S { x: 1 }));
    println(g(1, 2));
    println(h(S { x: 1 }));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    let errors = call_errors(&text(&check));
    assert_eq!(errors.len(), 2, "{}", text(&check));
    assert!(errors[0].contains(":6:13:"), "{}", errors[0]);
    assert!(
        errors[0].contains("gives `f`'s `T` the type `S`, which does not implement `PartialEq`"),
        "{}",
        errors[0]
    );
    assert!(errors[1].contains(":8:13:"), "{}", errors[1]);
    assert!(
        errors[1].contains("gives `h`'s `T` the type `S`, which does not implement `Display`"),
        "{}",
        errors[1]
    );
    // The bodies still warn, so the fix that adds the bounds is offered.
    assert_eq!(deprecated_lines(&text(&check)).len(), 3, "{}", text(&check));
}

#[test]
fn the_requirement_follows_a_generic_caller_and_a_generic_method() {
    let path = scratch(
        "transitive.kara",
        "struct S { x: i64 }
struct Holder { v: i64 }
fn eq[T](a: T, b: T) -> bool { a == b }
fn outer[U](a: U, b: U) -> bool { eq(a, b) }
fn outer_ok[U: PartialEq](a: U, b: U) -> bool { eq(a, b) }
impl Holder {
    fn show[T](self, x: T) -> String { f\"{x}\" }
}
fn main() {
    println(outer(S { x: 1 }, S { x: 1 }));
    println(outer(1, 1));
    println(outer_ok(2, 2));
    let b = Holder { v: 1 };
    println(b.show(S { x: 3 }));
    println(b.show(4));
}
",
    );
    let check = karac(&["check"], &path);
    let errors = call_errors(&text(&check));
    assert_eq!(errors.len(), 2, "{}", text(&check));
    assert!(errors[0].contains(":10:13:"), "{}", errors[0]);
    assert!(
        errors[0].contains("gives `outer`'s `U` the type `S`")
            && errors[0].contains("its call of `eq`"),
        "{}",
        errors[0]
    );
    assert!(errors[1].contains(":14:13:"), "{}", errors[1]);
    assert!(
        errors[1]
            .contains("gives `Holder.show`'s `T` the type `S`, which does not implement `Display`"),
        "{}",
        errors[1]
    );
}

#[test]
fn a_secret_cannot_be_printed_or_compared_through_an_unbounded_generic() {
    // Measured on main at 2acf33f426 before the call check: `show(tok)`
    // printed `<redacted>` (the shape rendering redacts a Secret, so no
    // leak), and `same(a, b)` printed `true`: two secrets compared by shape,
    // in variable time, which Secret's missing PartialEq exists to prevent.
    let path = scratch(
        "secret.kara",
        "import std.secret.{Secret};

fn show[T](x: T) -> String { f\"{x}\" }
fn same[T](a: T, b: T) -> bool { a == b }

fn main() {
    let tok: Secret[String] = Secret.new(\"hunter2\");
    println(show(tok));
    let a: Secret[String] = Secret.new(\"abc\");
    let b: Secret[String] = Secret.new(\"abc\");
    println(same(a, b));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
    let errors = call_errors(&text(&check));
    assert_eq!(errors.len(), 2, "{}", text(&check));
    assert!(
        errors[0].contains("the type `Secret[String]`, which does not implement `Display`"),
        "{}",
        errors[0]
    );
    assert!(
        errors[1].contains("the type `Secret[String]`, which does not implement `PartialEq`"),
        "{}",
        errors[1]
    );
    let run = karac(&["__mir-run"], &path);
    assert!(
        !String::from_utf8_lossy(&run.stdout).contains("hunter2"),
        "{}",
        text(&run)
    );
}

#[test]
fn bounded_bodies_and_types_with_the_trait_stay_clean() {
    let path = scratch(
        "clean.kara",
        "#[derive(PartialEq, PartialOrd)]
struct P { x: i64 }
struct Name { s: String }
impl Display for Name {
    fn to_string(ref self) -> String { self.s.clone() }
}
fn lt[T: Ord](x: T, y: T) -> bool { x < y and x != y }
fn show[T](x: T) -> String where T: Display { f\"<{x}>\" }
fn le[T: PartialOrd](x: T, y: T) -> bool { x <= y }
fn main() {
    println(lt(1, 2));
    println(show(Name { s: \"ada\" }));
    println(le(P { x: 1 }, P { x: 2 }));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    assert!(
        deprecated_lines(&text(&check)).is_empty(),
        "{}",
        text(&check)
    );
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "true\n<ada>\ntrue\n",
        "{}",
        text(&run)
    );
}

#[test]
fn allow_deprecated_silences_the_body_but_not_the_call() {
    let path = scratch(
        "allowed.kara",
        "struct S { x: i64 }
#[allow(deprecated)]
fn f[T](x: T, y: T) -> bool { x == y }
fn main() {
    println(f(1, 1));
    println(f(S { x: 1 }, S { x: 1 }));
}
",
    );
    let check = karac(&["check"], &path);
    assert!(
        deprecated_lines(&text(&check)).is_empty(),
        "{}",
        text(&check)
    );
    let errors = call_errors(&text(&check));
    assert_eq!(errors.len(), 1, "{}", text(&check));
    assert!(errors[0].contains(":6:13:"), "{}", errors[0]);
}
