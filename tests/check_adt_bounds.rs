//! A generic struct's or enum's declared parameter bounds hold wherever the
//! type is written with arguments (B-2026-10-28-2), as a type alias's do.
//! `struct M[T: Hash + Eq]` let `fn mk() -> M[f64]` and `let x: M[f64]`
//! through, and the `f64` key surfaced only at a later `x.m.len()`.

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-adt-bounds-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const M: &str = "struct M[T: Hash + Eq] { m: Map[T, i64] }\nenum Tree[T: Ord] { Leaf, Node(T) }\n";

#[test]
fn a_concrete_argument_without_the_bound_is_refused_where_written() {
    let path = write(
        "concrete",
        &format!(
            "{M}fn mk() -> M[f64] {{ M {{ m: Map.new() }} }}\n\
             fn t() -> Tree[f64] {{ Tree.Leaf }}\n\
             fn main() {{\n    let x: M[f64] = mk();\n    println(x.m.len());\n}}\n"
        ),
    );
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    let m = "`M` requires its parameter `T` to implement `Hash + Eq`, but `f64` does not \
             implement `Hash + Eq`";
    // The signature and the annotation, each once.
    assert!(err.contains(&format!("main.kara:3:12: {m}")), "{err}");
    assert!(err.contains(&format!("main.kara:6:12: {m}")), "{err}");
    assert!(
        err.contains(
            "main.kara:4:11: `Tree` requires its parameter `T` to implement `Ord`, but `f64` \
             does not implement `Ord`"
        ),
        "{err}"
    );
}

/// A type parameter given for a bounded one needs the bound where it is
/// declared, and `karac fix` writes it.
#[test]
fn a_parameter_argument_needs_the_bound_declared() {
    let src = format!("{M}struct Wrap[U] {{ inner: M[U] }}\nfn main() {{\n    println(1);\n}}\n");
    let path = write("param", &src);
    let out = karac(&["check"], &path);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains(
            "`M` requires its parameter `T` to implement `Hash + Eq`, but the type parameter \
             `U` given for it is not bounded by `Hash + Eq`"
        ),
        "{err}"
    );
    let out = karac(&["fix"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("struct Wrap[U: Hash + Eq] { inner: M[U] }"),
        "{fixed}"
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn arguments_with_the_bound_are_accepted() {
    let path = write(
        "accepted",
        &format!(
            "{M}fn mk[K: Hash + Eq]() -> M[K] {{ M {{ m: Map.new() }} }}\n\
             fn t() -> Tree[String] {{ Tree.Node(\"a\") }}\n\
             fn main() {{\n    let x: M[String] = mk();\n    let y: M[i64] = mk();\n    \
             println(x.m.len() + y.m.len());\n    match t() {{\n        Tree.Node(s) => println(s),\n        \
             Tree.Leaf => println(\"leaf\"),\n    }}\n}}\n"
        ),
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let out = karac(&["run", "--interp"], &path);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "0\na\n");
}
