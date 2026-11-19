//! Generic literals whose type parameter only another part of the literal
//! fixes.
//!
//! B-2026-11-19-135: a bare `[]` in a field (or a variant payload) typed by
//! the parameter (`W { v: 1, xs: [] }`, `Tree.Node(1, [])`) minted
//! `Vec[Error]` and stopped at lowering with an internal E0516; it takes the
//! slot's list type, which the other field solves.
//!
//! B-2026-11-19-136: a generic struct with a field holding itself
//! (`next: Option[Ls[T]]`) could not be built with a literal: `Ls { v: 1,
//! next: None }` reported E0200, because the field's own `T` was taken as
//! a second, unrelated parameter.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str], src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-generic-literal-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    let p = f.to_str().unwrap().to_string();
    all.push(&p);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(&all)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn runs(src: &str, expected: &str) {
    let (ok, text) = karac(&["check"], src);
    assert!(ok, "check: {text}");
    let (ok, text) = karac(&["__mir-run"], src);
    assert!(ok, "run: {text}");
    assert_eq!(text, expected);
}

#[test]
fn empty_list_field_takes_the_parameter_another_field_solves() {
    runs(
        "struct W[T] { v: T, xs: Vec[T] }\n\
         fn main() { let w = W { v: 1, xs: [] }; println(w.v + w.xs.len()); }\n",
        "1\n",
    );
}

#[test]
fn empty_list_of_self_field_takes_the_parameter() {
    runs(
        "struct Tr[T] { v: T, kids: Vec[Tr[T]] }\n\
         fn leaf[T](v: own T) -> Tr[T] { Tr { v: v, kids: [] } }\n\
         fn main() {\n\
           let t = Tr { v: 1, kids: [] };\n\
           let mut u = Tr { v: 2, kids: [] };\n\
           u.kids.push(t);\n\
           u.kids.push(leaf(5));\n\
           println(u.v + u.kids.len() + u.kids[1].v);\n\
         }\n",
        "9\n",
    );
}

#[test]
fn empty_list_variant_payload_takes_the_parameter() {
    runs(
        "enum Tree[T] { Leaf, Node(T, Vec[Tree[T]]) }\n\
         enum E[T] { Z, A(T, Vec[T]) }\n\
         fn node[T](v: own T) -> Tree[T] { Tree.Node(v, []) }\n\
         fn main() {\n\
           let n = Tree.Node(1, []);\n\
           match n { Tree.Node(v, k) => println(v + k.len()), Tree.Leaf => {} }\n\
           let e = E.A(2, []);\n\
           match e { E.A(v, k) => println(v + k.len()), E.Z => {} }\n\
           match node(\"s\") { Tree.Node(v, k) => println(f\"{v}{k.len()}\"), Tree.Leaf => {} }\n\
         }\n",
        "1\n2\ns0\n",
    );
}

#[test]
fn self_typed_option_field_literal_builds() {
    runs(
        "struct Ls[T] { v: T, next: Option[Ls[T]] }\n\
         fn main() {\n\
           let a = Ls { v: 1, next: None };\n\
           let b = Ls { v: 2, next: Some(a) };\n\
           match b.next { Some(n) => println(b.v + n.v), None => println(0) }\n\
         }\n",
        "3\n",
    );
}

#[test]
fn self_typed_weak_field_literal_builds() {
    runs(
        "shared struct Bx[T] { v: T, mut back: weak Bx[T] }\n\
         fn main() {\n\
           let a = Bx { v: 1, back: None };\n\
           let b = Bx { v: 2, back: a };\n\
           println(b.v + a.v);\n\
         }\n",
        "3\n",
    );
}

#[test]
fn self_typed_field_still_refuses_a_conflicting_parameter() {
    let (ok, text) = karac(
        &["check"],
        "struct Ls[T] { v: T, next: Option[Ls[T]] }\n\
         fn main() {\n\
           let a = Ls { v: \"s\", next: None };\n\
           let b = Ls { v: 2, next: Some(a) };\n\
           println(b.v);\n\
         }\n",
    );
    assert!(!ok, "{text}");
}

#[test]
fn self_typed_weak_field_still_refuses_a_conflicting_parameter() {
    let (ok, text) = karac(
        &["check"],
        "shared struct Bx[T] { v: T, mut back: weak Bx[T] }\n\
         fn main() {\n\
           let a = Bx { v: \"s\", back: None };\n\
           let b = Bx { v: 2, back: a };\n\
           println(b.v);\n\
         }\n",
    );
    assert!(!ok, "{text}");
    assert!(text.contains("weak Bx[String]"), "{text}");
}
