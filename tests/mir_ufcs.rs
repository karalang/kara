//! UFCS (design.md § Method Resolution): `Trait.method(x, ..)` calls the
//! named trait's impl for `x`, and `Type.method(x, ..)` is `x.method(..)`
//! (B-2026-10-28-17, B-2026-10-28-20). An operator trait's method called
//! by name on a number is the operator (B-2026-10-28-19). Each program runs
//! the same on the interpreter and compiled at -O0 and -O2.
#![cfg(feature = "llvm")]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-ufcs-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = Scratch::new();
    let obj = dir.path().join("main.o");
    let exe = dir.path().join("main");
    karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    Some((
        out.status.success(),
        String::from_utf8(out.stdout).expect("utf-8"),
    ))
}

/// The interpreter prints `want`, and so does the program compiled.
fn runs(src: &str, want: &str) {
    let got = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(got, want);
    for opt in [0, 2] {
        if let Some((ok, out)) = build_and_run(src, opt) {
            assert!(ok, "at -O{opt}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

/// The program is refused before MIR with a message containing `want`.
fn refused(src: &str, want: &str) {
    let err = match karac::mir::lower::build_source(src) {
        Ok(_) => panic!("built"),
        Err(e) => e,
    };
    assert!(err.contains(want), "{err}");
}

const TRAITS: &str = r#"struct P { x: i64 }
impl P { fn inh(self) -> i64 { self.x + 1 } fn twice(self) -> i64 { self.x * 100 } }
trait Plain { fn twice(self) -> i64; }
impl Plain for P { fn twice(self) -> i64 { self.x * 2 } }
trait Other { fn twice(self) -> i64; }
impl Other for P { fn twice(self) -> i64 { self.x * 3 } }
trait WithOut { type O; fn make(self) -> Self.O; }
impl WithOut for P { type O = i64; fn make(self) -> i64 { self.x + 100 } }
trait Gen[R] { fn combine(self, r: R) -> i64; }
impl Gen[i64] for P { fn combine(self, r: i64) -> i64 { self.x + r } }
trait Bump { fn bump(mut ref self); }
impl Bump for P { fn bump(mut ref self) { self.x += 1; } }
"#;

/// The named trait's method, past an inherent method and another trait's
/// method of the same name.
#[test]
fn mir_ufcs_names_the_trait_whose_method_it_calls() {
    let src = format!(
        "{TRAITS}{}",
        r#"fn main() {
    let mut p = P { x: 3 };
    println(f"{Plain.twice(p)} {Other.twice(p)} {P.twice(p)} {p.twice()} {P.inh(p)}");
    println(f"{WithOut.make(p)} {Gen.combine(p, 4)}");
    Bump.bump(mut p);
    P.bump(mut p);
    println(f"{p.x}");
}
"#
    );
    runs(&src, "6 9 300 300 4\n103 7\n5\n");
}

/// `Add.add(a, b)` and `P.add(a, b)` on owned values, and in a generic body
/// instantiated at a user type, `i64`, `f64` and `u8`.
#[test]
fn mir_ufcs_calls_an_operator_traits_method() {
    let src = r#"struct Q { v: Vec[i64] }
impl Add for Q {
    type Output = Q;
    fn add(own self, rhs: own Q) -> Q { let mut v = self.v; v.extend(rhs.v); Q { v } }
}
fn sum[T: Add[Output = T]](a: own T, b: own T) -> T { Add.add(a, b) }
fn main() {
    let a = Q { v: vec![1] };
    let b = Q { v: vec![2, 3] };
    let c = Add.add(a, b);
    let d = Q.add(c, Q { v: vec![4] });
    let e = sum(d, Q { v: vec![5] });
    println(f"{e.v.len()} {Add.add(2, 3)} {Mul.mul(2.5, 2.0)} {Neg.neg(4)}");
    let small: u8 = 200;
    println(f"{sum(3, 4)} {sum(1.5, 2.0)} {sum(small, 50)}");
}
"#;
    runs(src, "5 5 5.0 -4\n7 3.5 250\n");
}

/// `a.add(b)` on a number is `a + b`: the same result, and the same
/// overflow and division-by-zero traps.
#[test]
fn mir_operator_method_on_a_number_is_the_operator() {
    let src = r#"fn main() {
    let a = 6;
    println(f"{2.add(3)} {a.sub(1)} {a.mul(7)} {a.div(4)} {a.rem(4)} {a.neg()}");
    println(f"{a.bitand(3)} {a.bitor(9)} {a.bitxor(1)} {a.shl(2)} {a.shr(1)} {a.not()}");
    let f = 2.5;
    println(f"{f.mul(4.0)} {f.neg()} {f.div(2.0)} {f.sub(1.0)} {true.not()}");
    println(f"{i64.neg(4)} {i64.bitand(6, 3)} {f64.mul(2.0, 1.5)} {bool.not(false)}");
    let v = vec![1, 2];
    for x in v.iter() {
        println(f"{x.add(10)} {x.neg()}");
    }
}
"#;
    runs(
        src,
        "5 5 42 1 2 -6\n2 15 7 24 3 -7\n10.0 -2.5 1.25 1.5 false\n-4 2 3.0 true\n11 -1\n12 -2\n",
    );
    for (expr, trap) in [
        ("a.add(100)", "Aborted(Overflow)"),
        ("b.div(0)", "Aborted(DivByZero)"),
    ] {
        let src = format!(
            "fn main() {{\n    let a: u8 = 200;\n    let b = 7;\n    println(f\"{{{expr}}}\");\n}}\n"
        );
        let run = karac::mir::lower::run_source(&src).expect("MIR interpreter");
        assert!(run.output.is_empty(), "{}", run.output);
        let outcome = format!("{:?}", run.outcome);
        assert!(outcome.contains(trap), "{outcome}");
        if let Some((ok, _)) = build_and_run(&src, 0) {
            assert!(!ok, "{expr} compiled ran clean");
        }
    }
}

/// A receiver of another type, and a missing receiver, are refused by the
/// check.
#[test]
fn mir_ufcs_refuses_a_receiver_of_another_type_or_none() {
    let wrong = format!(
        "{TRAITS}{}",
        "struct R { x: i64 }\nimpl R { fn inh(self) -> i64 { self.x } }\nfn main() { let r = R { x: 1 }; println(f\"{P.inh(r)}\"); }\n"
    );
    refused(&wrong, "`P.inh` takes a `P` receiver, found `R`");
    let none = format!("{TRAITS}fn main() {{ println(f\"{{Plain.twice()}}\"); }}\n");
    refused(
        &none,
        "`Plain.twice` takes `self`: pass the receiver as the first argument",
    );
}
