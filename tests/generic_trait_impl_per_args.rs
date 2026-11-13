//! Two impls of one generic trait for one type run the impl each call names
//! (B-2026-11-13-44). On a primitive receiver (`impl Conv[String] for i64`,
//! `impl Conv[f64] for i64`) both impls' methods were instanced under one
//! name, `i64.conv`, so the first body lowered took every call: the
//! interpreter printed `n5 n5`, and LLVM read the `String` as an `f64`. A call
//! under a generic bound (`x.conv()` under `X: Conv[f64]`) took the type's
//! first impl of the trait whatever the bound's arguments, on a struct
//! receiver as on a primitive. Each program runs on the interpreter and, with
//! the backend, compiled.
//!
//! A `#[derive(PartialEq)]` on a generic type holds only where its arguments
//! are `PartialEq` (design.md § "Derive adds the bounds"): `Pair[NoEq] ==`
//! is refused at the use site (B-2026-11-13-45).

use std::path::PathBuf;
use std::process::{Command, Output};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-generic-trait-args-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .output()
        .unwrap()
}

fn backends() -> &'static [&'static str] {
    if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    }
}

fn runs_as(tag: &str, src: &str, want: &str) {
    let check = karac(tag, &["check"], src);
    assert!(
        check.status.success(),
        "{tag}: check refused it:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    for b in backends() {
        let out = karac(tag, &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "{tag} on {b} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const CONV_I64: &str = "trait Conv[T] { fn conv(self) -> T; }\n\
     impl Conv[String] for i64 { fn conv(self) -> String { f\"n{self}\" } }\n\
     impl Conv[f64] for i64 { fn conv(self) -> f64 { self as f64 } }\n";

const CONV_S: &str = "struct S { v: i64 }\n\
     trait Conv[T] { fn conv(self) -> T; }\n\
     impl Conv[String] for S { fn conv(self) -> String { f\"n{self.v}\" } }\n\
     impl Conv[f64] for S { fn conv(self) -> f64 { self.v as f64 } }\n";

#[test]
fn generic_trait_twice_on_a_primitive_one_instance_each() {
    // g1, g2: one instance named, whichever impl comes first.
    runs_as(
        "g1",
        &format!("{CONV_I64}fn main() {{ let n: i64 = 5; let x: f64 = Conv[f64].conv(n); println(x); }}\n"),
        "5.0\n",
    );
    runs_as(
        "g2",
        "trait Conv[T] { fn conv(self) -> T; }\n\
         impl Conv[f64] for i64 { fn conv(self) -> f64 { self as f64 } }\n\
         impl Conv[String] for i64 { fn conv(self) -> String { f\"n{self}\" } }\n\
         fn main() { let n: i64 = 5; let s: String = Conv[String].conv(n); println(s); }\n",
        "n5\n",
    );
}

#[test]
fn generic_trait_twice_on_a_primitive_both_instances() {
    // g3: both in one body.
    runs_as(
        "g3",
        &format!(
            "{CONV_I64}fn main() {{ let n: i64 = 5; let s: String = Conv[String].conv(n); \
             let x: f64 = Conv[f64].conv(n); println(s); println(x); }}\n"
        ),
        "n5\n5.0\n",
    );
    // g4: each in a function of its own.
    runs_as(
        "g4",
        &format!(
            "{CONV_I64}fn a(n: i64) -> String {{ Conv[String].conv(n) }}\n\
             fn b(n: i64) -> f64 {{ Conv[f64].conv(n) }}\n\
             fn main() {{ println(a(5)); println(b(5)); }}\n"
        ),
        "n5\n5.0\n",
    );
    // s19: both in one f-string.
    runs_as(
        "s19",
        &format!(
            "{CONV_I64}fn main() {{ let n: i64 = 5; \
             println(f\"{{Conv[String].conv(n)}} {{Conv[f64].conv(n)}}\"); }}\n"
        ),
        "n5 5.0\n",
    );
}

#[test]
fn generic_trait_twice_under_a_bound_takes_the_bound_s_instance() {
    // g5: the bound's arguments pick the impl, on a primitive receiver...
    runs_as(
        "g5",
        &format!(
            "{CONV_I64}fn tf[X: Conv[f64]](x: X) -> f64 {{ x.conv() }}\n\
             fn ts[X: Conv[String]](x: X) -> String {{ x.conv() }}\n\
             fn main() {{ println(ts(5)); println(tf(5)); }}\n"
        ),
        "n5\n5.0\n",
    );
    // ...and on a struct receiver.
    runs_as(
        "g5_struct",
        &format!(
            "{CONV_S}fn tf[X: Conv[f64]](x: X) -> f64 {{ x.conv() }}\n\
             fn ts[X: Conv[String]](x: X) -> String {{ x.conv() }}\n\
             fn main() {{ let s = S {{ v: 5 }}; println(ts(s)); println(tf(s)); }}\n"
        ),
        "n5\n5.0\n",
    );
    // An associated function under the bound (`X.make(..)`).
    runs_as(
        "assoc",
        "trait Make[T] { fn make(v: T) -> Self; }\n\
         impl Make[String] for i64 { fn make(v: String) -> i64 { v.len() as i64 } }\n\
         impl Make[bool] for i64 { fn make(v: bool) -> i64 { if v { 1 } else { 0 } } }\n\
         fn ms[X: Make[String]]() -> X { X.make(\"abcd\") }\n\
         fn mb[X: Make[bool]]() -> X { X.make(true) }\n\
         fn main() { let a: i64 = ms(); let b: i64 = mb(); println(f\"{a} {b}\"); }\n",
        "4 1\n",
    );
}

#[test]
fn generic_trait_twice_on_a_struct_and_on_other_primitives() {
    runs_as(
        "struct",
        &format!(
            "{CONV_S}fn main() {{ let s = S {{ v: 5 }}; let a: String = Conv[String].conv(s); \
             let b: f64 = Conv[f64].conv(s); println(f\"{{a}} {{b}}\"); }}\n"
        ),
        "n5 5.0\n",
    );
    runs_as(
        "mixed",
        "trait Conv[T] { fn conv(self) -> T; }\n\
         impl Conv[bool] for i64 { fn conv(self) -> bool { self > 3 } }\n\
         impl Conv[i64] for i64 { fn conv(self) -> i64 { self * 10 } }\n\
         impl Conv[i32] for f64 { fn conv(self) -> i32 { 7 } }\n\
         impl Conv[String] for f64 { fn conv(self) -> String { \"f\" } }\n\
         fn main() {\n    let n: i64 = 5;\n    let a: bool = Conv[bool].conv(n);\n    \
         let b: i64 = Conv[i64].conv(n);\n    let x: f64 = 1.5;\n    \
         let c: i32 = Conv[i32].conv(x);\n    let d: String = Conv[String].conv(x);\n    \
         println(f\"{a} {b} {c} {d}\");\n}\n",
        "true 50 7 f\n",
    );
}

#[test]
fn derived_partial_eq_needs_its_argument_s_partial_eq() {
    let src = "#[derive(PartialEq)]\nstruct Pair[T] { first: T }\nstruct NoEq { a: i64 }\n\
               fn main() {\n    let a = Pair { first: NoEq { a: 1 } };\n    \
               let b = Pair { first: NoEq { a: 1 } };\n    println(a == b);\n}\n";
    let out = karac("derive-unmet", &["check"], src);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "accepted: {err}");
    assert!(
        err.contains(":7:")
            && err.contains(
                "type 'Pair[NoEq]' does not implement PartialEq: its #[derive(PartialEq)] \
                 holds only where 'NoEq' implements PartialEq"
            ),
        "{err}"
    );
    // At an argument that has it, the derive holds.
    runs_as(
        "derive-met",
        "#[derive(PartialEq)]\nstruct Pair[T] { first: T }\n\
         #[derive(PartialEq)]\nstruct Yes { a: i64 }\n\
         fn main() {\n    let a = Pair { first: Yes { a: 1 } };\n    \
         let b = Pair { first: Yes { a: 1 } };\n    let c = Pair { first: 2.5 };\n    \
         println(f\"{a == b} {c != Pair { first: 2.5 }}\");\n}\n",
        "true false\n",
    );
}

/// Two non-generic traits with a method of one name, both implemented for a
/// primitive: `A.m(n)` and `B.m(n)` each run their trait's impl, as on a
/// struct (B-2026-11-13-46). The call used to reach the backends as a native
/// `i64.m`, unknown to both.
#[test]
fn two_traits_one_method_name_on_a_primitive() {
    let traits = "trait A { fn m(self) -> String; }\ntrait B { fn m(self) -> String; }\n";
    runs_as(
        "ab_prim",
        &format!(
            "{traits}impl A for i64 {{ fn m(self) -> String {{ \"a\" }} }}\n\
             impl B for i64 {{ fn m(self) -> String {{ \"b\" }} }}\n\
             fn main() {{ let n: i64 = 1; println(f\"{{A.m(n)}} {{B.m(n)}}\"); }}\n"
        ),
        "a b\n",
    );
    runs_as(
        "ab_bound",
        &format!(
            "{traits}impl A for i64 {{ fn m(self) -> String {{ \"a\" }} }}\n\
             impl B for i64 {{ fn m(self) -> String {{ \"b\" }} }}\n\
             fn fa[X: A](x: X) -> String {{ x.m() }}\n\
             fn fb[X: B](x: X) -> String {{ x.m() }}\n\
             fn main() {{ println(f\"{{fa(1)}} {{fb(1)}}\"); }}\n"
        ),
        "a b\n",
    );
    runs_as(
        "ab_struct",
        &format!(
            "{traits}struct S {{ v: i64 }}\n\
             impl A for S {{ fn m(self) -> String {{ \"a\" }} }}\n\
             impl B for S {{ fn m(self) -> String {{ \"b\" }} }}\n\
             fn main() {{ let n = S {{ v: 1 }}; println(f\"{{A.m(n)}} {{B.m(n)}}\"); }}\n"
        ),
        "a b\n",
    );
}
