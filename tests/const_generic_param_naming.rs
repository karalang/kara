//! B-2026-11-18-24: a const generic parameter's name is Const class
//! (design.md §3 CN-3, CN-7; §8 Const generic parameters): the spec's
//! `struct Matrix[T, const ROWS: i64, const COLS: i64]` checks, a one-letter
//! `N` does too, and a PascalCase `Cap` is refused with `CAP` suggested,
//! where the check had the classes the other way round.

use karac::check_source;

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

#[test]
fn const_generic_parameters_are_const_class() {
    for src in [
        "struct Matrix[T, const ROWS: i64, const COLS: i64] {\n    d: Array[T, ROWS],\n}\n\
         fn main() {\n    let m: Matrix[i64, 2, 3] = Matrix { d: [1, 2] };\n    println(m.d.len());\n}\n",
        "trait FixedBuf[const CAP: i64] {\n    fn cap(self) -> i64;\n}\nfn main() {\n    println(1);\n}\n",
        "fn first[const N: i64](a: Array[i64, N]) -> i64 {\n    a[0] + N\n}\n\
         fn main() {\n    let a: Array[i64, 3] = [4, 5, 6];\n    println(first(a));\n}\n",
    ] {
        assert!(errors(src).is_empty(), "{src}\n{:#?}", errors(src));
    }
    let out = karac::run_playground(
        "struct Matrix[T, const ROWS: i64, const COLS: i64] {\n    d: Array[T, ROWS],\n}\n\
         fn main() {\n    let m: Matrix[i64, 2, 3] = Matrix { d: [1, 2] };\n    println(m.d.len());\n}\n",
    );
    assert!(out.ok, "{:#?}", out.diagnostics);
    assert_eq!(out.stdout.concat(), "2\n");
}

#[test]
fn a_const_generic_parameter_not_const_class_is_refused() {
    for (decl, name, want) in [("const Cap: i64", "Cap", "CAP"), ("const n: i64", "n", "N")] {
        let src = format!("struct Buf[{decl}] {{\n    d: Array[i64, {name}],\n}}\nfn main() {{\n    println(1);\n}}\n");
        let es = errors(&src);
        assert!(
            es.iter().any(|e| e.contains(&format!("`{name}` is"))
                && e.contains("const generic parameter names must be Const-class")
                && e.contains(&format!("consider renaming to `{want}`"))),
            "{src}\n{es:#?}"
        );
    }
}
