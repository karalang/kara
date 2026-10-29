//! A binary operator whose left operand is a primitive and whose impl is
//! the program's (`impl Mul[Col] for f64`, then `2.0 * col`, design.md
//! § Operator traits; B-2026-10-28-16): the check consulted only the
//! primitive's own `Mul for f64` and refused the right operand. Outputs
//! are worked out by hand.
//!
//! With `--features llvm` each program is also compiled, at -O0 and -O2,
//! and must print the same.

#[cfg(feature = "llvm")]
mod common;

use karac::mir::interp::Outcome;

/// The interpreter prints `want` and finishes with nothing leaked, and so
/// does the program compiled.
fn runs(src: &str, want: &str) {
    let run = karac::mir::lower::run_source(src).expect("MIR");
    assert!(
        matches!(run.outcome, Outcome::Returned(_)),
        "{:?}\n{}",
        run.outcome,
        run.output
    );
    assert_eq!(run.output, want);
    #[cfg(feature = "llvm")]
    for opt in [0, 2] {
        if let Some((ok, out)) = compiled::build_and_run(src, opt) {
            assert!(ok, "at -O{opt}: {out}");
            assert_eq!(out, want, "at -O{opt}");
        }
    }
}

#[cfg(feature = "llvm")]
mod compiled {
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What the program prints compiled at `opt`, and whether it exited
    /// cleanly; `None` when there is no runtime archive to link.
    pub fn build_and_run(src: &str, opt: u8) -> Option<(bool, String)> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let lowered = karac::mir::lower::build_source(src).expect("MIR");
        karac::mir::lower::compiled_check(&lowered).expect("checks");
        let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("karac-prim-op-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let (obj, exe) = (dir.join("main.o"), dir.join("main"));
        karac::backend::mir_aot::compile_object(&ir, &obj, opt).expect("object");
        let linked = super::common::link_or_skip(karac::backend::driver::link_executable(
            &obj.to_string_lossy(),
            &exe.to_string_lossy(),
        ));
        let out = linked.map(|()| Command::new(&exe).output().expect("run"));
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        Some((
            out.status.success(),
            String::from_utf8(out.stdout).expect("utf-8"),
        ))
    }
}

/// `2.0 * col`, `10.0 - col` and `5 * icol` call the program's impls
/// for `f64` and `i64`, and `2.0 * 3.0` beside them stays the built-in
/// operator.
#[test]
fn primitive_left_operand_calls_the_programs_impl() {
    runs(
        r#"struct Col { v: Vec[f64] }

impl Mul[Col] for f64 {
    type Output = Col;
    fn mul(own self, rhs: own Col) -> Col {
        let mut out: Vec[f64] = Vec.new();
        for x in rhs.v { out.push(self * *x); }
        Col { v: out }
    }
}

struct ICol { v: Vec[i64] }

impl Mul[ICol] for i64 {
    type Output = ICol;
    fn mul(own self, rhs: own ICol) -> ICol {
        let mut out: Vec[i64] = Vec.new();
        for x in rhs.v { out.push(self * *x); }
        ICol { v: out }
    }
}

impl Sub[Col] for f64 {
    type Output = Col;
    fn sub(own self, rhs: own Col) -> Col {
        let mut out: Vec[f64] = Vec.new();
        for x in rhs.v { out.push(self - *x); }
        Col { v: out }
    }
}

fn main() {
    let c = Col { v: vec![1.0, 2.0] };
    let d = 2.0 * c;
    println(f"{d.v[0]} {d.v[1]}");
    let e = 10.0 - d;
    println(f"{e.v[0]} {e.v[1]}");
    let ic = ICol { v: vec![1, 2, 3] };
    let id = 5 * ic;
    println(f"{id.v[0]} {id.v[2]}");
    let x = 2.0 * 3.0;
    let y = 5 * 7;
    println(f"{x} {y}");
}
"#,
        "2.0 4.0
8.0 6.0
5 15
6.0 35
",
    );
}

/// A borrowing impl leaves its operand usable, a `ref Col` parameter
/// fills it, a `Col * f64` impl can forward to the `f64 * Col` one, and a
/// `shared` right operand works the same way.
#[test]
fn primitive_left_operand_borrows_and_forwards() {
    runs(
        r#"struct Col { v: Vec[f64] }

impl Mul[Col] for f64 {
    type Output = Col;
    fn mul(self, rhs: Col) -> Col {
        let mut out: Vec[f64] = Vec.new();
        for x in rhs.v { out.push(self * *x); }
        Col { v: out }
    }
}

impl Mul[f64] for Col {
    type Output = Col;
    fn mul(self, rhs: f64) -> Col {
        rhs * self
    }
}

shared struct Acc { n: i64 }

impl Add[Acc] for i64 {
    type Output = i64;
    fn add(self, rhs: Acc) -> i64 { self + rhs.n }
}

fn scale(k: f64, c: ref Col) -> Col { k * c }

fn main() {
    let c = Col { v: vec![1.0, 2.0] };
    let d = 2.0 * c;
    let e = 3.0 * c;
    let f = c * 0.5;
    let g = scale(10.0, c);
    println(f"{d.v[1]} {e.v[1]} {f.v[1]} {g.v[0]} {c.v[0]}");
    let a = Acc { n: 40 };
    let k = 2 + a;
    println(f"{k} {a.n}");
    println(f"{2.0 * 3.0} {7 + 1}");
}
"#,
        "4.0 6.0 1.0 10.0 1.0
42 40
6.0 8
",
    );
}
