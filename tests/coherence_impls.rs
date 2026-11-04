//! design.md § Coherence and § Conformance, checked where each impl is
//! declared:
//!
//! - Two impls of one trait whose headers unify are refused at the second,
//!   bounds ignored, and a second `impl Iterator` names the first's `Item`
//!   (B-2026-11-04-14: each was accepted, and a call listed two identical
//!   UFCS candidates).
//! - An inherent impl on a type the program does not declare is refused
//!   (B-2026-11-04-15: `impl Vec[i64] { .. }` ran). One on an alias of the
//!   program's own struct is that struct's, by method call and by UFCS
//!   through either name (B-2026-11-04-50: it failed in MIR).
//! - A blanket impl of the program's trait over a bare parameter is found by
//!   a method call and by UFCS on a type meeting its bounds
//!   (B-2026-11-04-16: "no method 'twice' on type 'i64'").
//! - A blanket impl's body calling another method of the impl on its
//!   parameter, written there or in a trait default, calls the impl's
//!   method (B-2026-11-04-49: "no resolved callee").
//! - `T.f(x)` with two impls of one trait declaring `f` takes the impl
//!   the argument fits (B-2026-11-04-54: the first impl's parameter).
//! - An impl method's receiver may not be stronger than the trait's
//!   (B-2026-11-04-13: `mut ref self` for the trait's `self` was given the
//!   trait's receiver and accepted).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir =
            std::env::temp_dir().join(format!("karac-coherence-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, text: &str) -> PathBuf {
        let p = self.0.join("main.kara");
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `karac check` refuses `src` with one error at `line`, whose message
/// contains `want`.
fn refused(tag: &str, src: &str, line: usize, want: &str) {
    let dir = Scratch::new(tag);
    let f = dir.file(src);
    let out = karac(&dir.0, &["check", f.to_str().unwrap()]);
    let all = text(&out.stdout) + &text(&out.stderr);
    assert!(!out.status.success(), "{tag}: check must refuse:\n{all}");
    assert!(all.contains(want), "{tag}: wanted `{want}` in:\n{all}");
    assert!(
        all.contains(&format!("main.kara:{line}:")),
        "{tag}: wanted the error at line {line}:\n{all}"
    );
    assert!(all.contains("1 error(s) found"), "{tag}: one error:\n{all}");
}

/// `src` prints `want` under both `karac run` executors.
fn runs(tag: &str, src: &str, want: &str) {
    let dir = Scratch::new(tag);
    let f = dir.file(src);
    let f = f.to_str().unwrap();
    for run in [&["run"][..], &["run", "--interp"]] {
        let mut args = run.to_vec();
        args.push(f);
        let out = karac(&dir.0, &args);
        assert!(out.status.success(), "{tag} {run:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), want, "{tag} {run:?}");
    }
}

#[test]
fn a_duplicate_trait_impl_is_refused_at_the_second() {
    refused(
        "dup",
        "trait T { fn f(self) -> i64; }\nstruct S {}\n\
         impl T for S { fn f(self) -> i64 { 1 } }\n\
         impl T for S { fn f(self) -> i64 { 2 } }\n\
         fn main() { println(S {}.f()); }\n",
        4,
        "conflicting impl: `S` already implements `T`",
    );
}

#[test]
fn impls_whose_headers_differ_only_in_bounds_overlap() {
    refused(
        "bounds",
        "trait T { fn f(self) -> i64; }\nstruct B[X] { x: X }\n\
         impl[X: Eq] T for B[X] { fn f(self) -> i64 { 1 } }\n\
         impl[X: Ord] T for B[X] { fn f(self) -> i64 { 2 } }\n\
         fn main() { println(B { x: 1 }.f()); }\n",
        4,
        "conflicting impl: `B` already implements `T`",
    );
}

#[test]
fn a_second_iterator_impl_names_the_first_item() {
    refused(
        "iter",
        "struct Up { n: i64 }\n\
         impl Iterator for Up {\n    type Item = i64;\n    fn next(mut ref self) -> Option[i64] { None }\n}\n\
         impl Iterator for Up {\n    type Item = String;\n    fn next(mut ref self) -> Option[String] { None }\n}\n\
         fn main() { let mut u = Up { n: 0 }; println(u.next()); }\n",
        6,
        "`Up` already implements `Iterator`, with Item = i64",
    );
}

#[test]
fn impls_of_one_trait_on_distinct_headers_coexist() {
    runs(
        "distinct-headers",
        "trait T { fn f(self) -> i64; }\nstruct B[X] { x: X }\n\
         impl T for B[i64] { fn f(self) -> i64 { 1 } }\n\
         impl T for B[String] { fn f(self) -> i64 { 2 } }\n\
         struct A { n: i64 }\nstruct C { n: i64 }\nstruct Out { n: i64 }\n\
         impl From[A] for Out { fn from(a: A) -> Out { Out { n: a.n } } }\n\
         impl From[C] for Out { fn from(c: C) -> Out { Out { n: c.n * 10 } } }\n\
         fn main() {\n    println(B { x: 1 }.f() + B { x: \"a\" }.f());\n\
             let o: Out = A { n: 4 }.into();\n    let p: Out = C { n: 5 }.into();\n\
             println(o.n + p.n);\n}\n",
        "3\n54\n",
    );
}

#[test]
fn an_inherent_impl_through_an_alias_of_an_own_struct_is_the_structs() {
    runs(
        "alias-impl",
        "struct W { n: i64 }\ntype Wrap = W;\n\
         impl Wrap { fn get(self) -> i64 { self.n } }\n\
         impl W { fn dbl(self) -> i64 { self.n * 2 } }\n\
         fn main() {\n\
             let w: Wrap = W { n: 4 };\n\
             println(W { n: 4 }.get());\n\
             println(w.dbl() + w.get());\n\
             println(Wrap.get(w));\n\
             println(W.get(w));\n\
             println(Wrap.dbl(w));\n\
         }\n",
        "4\n12\n4\n4\n8\n",
    );
}

#[test]
fn an_inherent_impl_on_a_standard_type_is_refused() {
    refused(
        "inherent-vec",
        "impl Vec[i64] { fn total(self) -> i64 { let mut t = 0; for x in self { t += x; } t } }\n\
         fn main() { let v: Vec[i64] = [1, 2].to_vec(); println(v.total()); }\n",
        1,
        "inherent impl on `Vec[i64]`, a type this program does not declare",
    );
    refused(
        "inherent-i64",
        "impl i64 { fn dbl(self) -> i64 { self * 2 } }\nfn main() { println(2.dbl()); }\n",
        1,
        "or wrap the type in a `distinct type`",
    );
    refused(
        "inherent-alias",
        "type Grid = Vec[i64];\nimpl Grid { fn total(self) -> i64 { 0 } }\nfn main() { println(1); }\n",
        2,
        "inherent impl on `Grid`",
    );
}

#[test]
fn a_trait_impl_on_a_standard_type_and_a_distinct_type_are_allowed() {
    runs(
        "own-trait",
        "trait Local { fn total(self) -> i64; }\n\
         impl Local for Vec[i64] { fn total(self) -> i64 { let mut t = 0; for x in self { t += x; } t } }\n\
         distinct type Ids = Vec[i64];\n\
         impl Ids { fn tag(self) -> String { \"ids\" } }\n\
         fn main() {\n    let v: Vec[i64] = [1, 2].to_vec();\n    println(v.total());\n\
             let ids = Ids([7, 8].to_vec());\n    println(ids.tag());\n}\n",
        "3\nids\n",
    );
}

const BLANKET: &str = "trait Twice { fn twice(self) -> String; }\n\
impl[T: Display] Twice for T { fn twice(self) -> String { f\"{self}{self}\" } }\n";

#[test]
fn a_blanket_impl_over_a_bare_parameter_is_found_on_a_concrete_receiver() {
    runs(
        "blanket",
        &format!(
            "{BLANKET}struct P {{ n: i64 }}\n\
             impl Display for P {{ fn to_string(self) -> String {{ f\"P{{self.n}}\" }} }}\n\
             fn main() {{\n    println(5.twice());\n    println(\"ab\".twice());\n\
                 println(P {{ n: 1 }}.twice());\n    println(Twice.twice(7));\n}}\n"
        ),
        "55\nabab\nP1P1\n77\n",
    );
}

#[test]
fn ufcs_reaches_the_blanket_method_past_an_inherent_one() {
    runs(
        "blanket-ufcs",
        &format!(
            "{BLANKET}struct Q {{ n: i64 }}\n\
             impl Q {{ fn twice(self) -> String {{ \"inherent\" }} }}\n\
             impl Display for Q {{ fn to_string(self) -> String {{ \"q\" }} }}\n\
             fn main() {{\n    println(Q {{ n: 1 }}.twice());\n    println(Twice.twice(Q {{ n: 1 }}));\n}}\n"
        ),
        "inherent\nqq\n",
    );
}

#[test]
fn a_blanket_default_body_calls_the_impls_own_method() {
    runs(
        "blanket-sibling",
        "trait Shout {\n\
             fn tag(self) -> String;\n\
             fn shout(self, n: i64) -> String { let mut s = self.tag(); let mut i = 1; while i < n { s = s + \"!\"; i += 1; } s }\n\
             fn twice(self) -> String { self.tag() + self.tag() }\n\
         }\n\
         impl[T: Display] Shout for T {\n\
             fn tag(self) -> String { f\"<{self}>\" }\n\
             fn twice(self) -> String { self.tag() + \"=\" + self.tag() }\n\
         }\n\
         fn go[U: Shout](u: U) -> String { u.shout(3) }\n\
         fn main() { println(go(3)); println(4.shout(2)); println(true.twice()); }\n",
        "<3>!!\n<4>!\n<true>=<true>\n",
    );
}

#[test]
fn an_assoc_fn_of_two_impls_of_one_trait_is_picked_by_argument() {
    runs(
        "assoc-by-arg",
        "trait Make[T] { fn make(x: T) -> Self; }\n\
         struct Out { n: i64 }\n\
         impl Make[i64] for Out { fn make(x: i64) -> Out { Out { n: x } } }\n\
         impl Make[String] for Out { fn make(x: String) -> Out { Out { n: x.len() } } }\n\
         fn main() { println(Out.make(7).n); println(Out.make(\"abcd\".to_string()).n); }\n",
        "7\n4\n",
    );
    refused(
        "assoc-by-arg-none",
        "trait Make[T] { fn make(x: T) -> Self; }\n\
         struct Out { n: i64 }\n\
         impl Make[i64] for Out { fn make(x: i64) -> Out { Out { n: x } } }\n\
         impl Make[String] for Out { fn make(x: String) -> Out { Out { n: x.len() } } }\n\
         fn main() { println(Out.make(true).n); }\n",
        5,
        "no `Make` impl of `Out` has a `make` taking (bool); its impls take `make(String)`, `make(i64)`",
    );
}

#[test]
fn a_blanket_impl_does_not_reach_a_type_missing_its_bound() {
    refused(
        "blanket-bound",
        &format!(
            "{BLANKET}struct N {{ n: i64 }}\nfn main() {{ println(N {{ n: 1 }}.twice()); }}\n"
        ),
        4,
        "no method 'twice' on type 'N'",
    );
}

#[test]
fn an_impl_receiver_stronger_than_the_trait_is_refused() {
    refused(
        "recv-stronger",
        "trait T { fn look(self) -> i64; }\nstruct S { n: i64 }\n\
         impl T for S { fn look(mut ref self) -> i64 { self.n += 1; self.n } }\n\
         fn main() { let mut s = S { n: 1 }; println(s.look()); }\n",
        3,
        "takes its receiver as `mut ref self`, but 'T.look' declares `self`",
    );
}

#[test]
fn an_impl_receiver_weaker_than_the_trait_is_accepted() {
    runs(
        "recv-weaker",
        "trait T { fn look(own self) -> i64; fn poke(mut ref self) -> i64; }\n\
         struct S { n: i64 }\n\
         impl T for S { fn look(self) -> i64 { self.n } fn poke(self) -> i64 { self.n + 1 } }\n\
         fn main() { let mut s = S { n: 1 }; println(s.poke()); println(s.look()); }\n",
        "2\n1\n",
    );
}
