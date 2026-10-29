//! A binary operator whose left operand is a primitive and whose impl is
//! the program's (`impl Mul[Col] for f64`, then `2.0 * col`, design.md
//! § Operator traits; B-2026-10-28-16): the check consulted only the
//! primitive's own `Mul for f64` and refused the right operand. The method
//! form of such a call, `col.mul(2.0)` with several `Mul` impls, was refused
//! as ambiguous (B-2026-10-28-18), and of a type's two `Index` impls only
//! the first was consulted (B-2026-10-28-21). Outputs are worked out by
//! hand.
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
        let dir = std::env::temp_dir().join(format!("karac-op-choice-{}-{n}", std::process::id()));
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

/// `col.mul(2.0)` on a type with several `Mul` impls that differ in the
/// right operand, `2.5.mul(col)` beside the number's own `Mul`, and the
/// trait-qualified `Mul.mul(..)` each take the impl their argument names,
/// and `2.0.mul(3.0)` stays the number's operator (B-2026-10-28-18).
#[test]
fn operator_method_call_picks_its_impl_by_argument() {
    runs(
        r#"struct Col { v: Vec[f64] }
struct Flags { on: Vec[bool] }

fn scaled(c: ref Col, k: f64) -> Col {
    let mut out: Vec[f64] = Vec.new();
    for x in c.v { out.push(*x * k); }
    Col { v: out }
}

impl Mul[f64] for Col {
    type Output = Col;
    fn mul(self, rhs: f64) -> Col { scaled(self, rhs) }
}
impl Mul[i64] for Col {
    type Output = Col;
    fn mul(self, rhs: i64) -> Col { scaled(self, rhs as f64) }
}
impl Mul[Col] for Col {
    type Output = Col;
    fn mul(self, rhs: Col) -> Col {
        let mut out: Vec[f64] = Vec.new();
        for i in 0..self.v.len() { out.push(self.v[i] * rhs.v[i]); }
        Col { v: out }
    }
}
impl Mul[Flags] for Col {
    type Output = Col;
    fn mul(self, rhs: Flags) -> Col {
        let mut out: Vec[f64] = Vec.new();
        for i in 0..self.v.len() { out.push(if rhs.on[i] { self.v[i] } else { 0.0 }); }
        Col { v: out }
    }
}
impl Mul[Col] for f64 {
    type Output = Col;
    fn mul(self, rhs: Col) -> Col { scaled(rhs, self) }
}

fn show(c: Col) { println(f"{c.v[0]} {c.v[1]}"); }

fn main() {
    let c = Col { v: vec![1.0, 2.0] };
    let m = Flags { on: vec![false, true] };
    show(c.mul(2.0));
    show(c.mul(3));
    show(c.mul(c));
    show(c.mul(m));
    show(2.5.mul(c));
    show(Mul.mul(c, 4.0));
    show(Mul.mul(c, m));
    show(Mul.mul(10.0, c));
    println(f"{2.0.mul(3.0)} {Mul.mul(2.0, 0.5)}");
}
"#,
        "2.0 4.0\n3.0 6.0\n1.0 4.0\n0.0 2.0\n2.5 5.0\n4.0 8.0\n0.0 2.0\n10.0 20.0\n6.0 1.0\n",
    );
}

/// A type with `Index[String]` and `Index[i64]` impls reads through the
/// one the key's type names, and a `String` key lent to the borrowing
/// impl stays usable (B-2026-10-28-21).
#[test]
fn index_picks_its_impl_by_the_key() {
    runs(
        r#"struct Col { v: Vec[f64] }
struct Frame { names: Vec[String], cols: Vec[Col] }

impl Index[i64] for Col {
    type Output = f64;
    fn index(self, i: i64) -> ref f64 { ref self.v[i] }
}

impl Index[String] for Frame {
    type Output = Col;
    fn index(self, name: String) -> ref Col {
        let mut at = 0;
        for i in 0..self.names.len() {
            if self.names[i] == name { at = i; }
        }
        ref self.cols[at]
    }
}

impl Index[i64] for Frame {
    type Output = Col;
    fn index(self, i: i64) -> ref Col { ref self.cols[i] }
}

fn main() {
    let f = Frame {
        names: ["a".to_string(), "b".to_string()].to_vec(),
        cols: vec![Col { v: vec![1.0, 2.0] }, Col { v: vec![3.0, 4.0] }],
    };
    println(f"{f[1][0]} {f["a"][1]} {f[0][0]} {f["b"][1]}");
    let k = "b".to_string();
    println(f"{f[k][0]} {k}");
}
"#,
        "3.0 2.0 1.0 4.0\n3.0 b\n",
    );
}

/// The same with two `IndexMut` impls: plain and compound stores through
/// either key.
#[test]
fn index_mut_picks_its_impl_by_the_key() {
    runs(
        r#"struct Col { v: Vec[f64] }
struct Frame { names: Vec[String], cols: Vec[Col] }

impl Index[i64] for Col {
    type Output = f64;
    fn index(self, i: i64) -> ref f64 { ref self.v[i] }
}
impl IndexMut[i64] for Col {
    type Output = f64;
    fn index_mut(mut ref self, i: i64) -> mut ref f64 { mut ref self.v[i] }
}

fn find(names: ref Vec[String], name: ref String) -> i64 {
    let mut at = 0;
    for i in 0..names.len() {
        if names[i] == *name { at = i; }
    }
    at
}

impl Index[String] for Frame {
    type Output = Col;
    fn index(self, name: String) -> ref Col { ref self.cols[find(self.names, name)] }
}
impl IndexMut[String] for Frame {
    type Output = Col;
    fn index_mut(mut ref self, name: String) -> mut ref Col {
        let at = find(self.names, name);
        mut ref self.cols[at]
    }
}
impl Index[i64] for Frame {
    type Output = Col;
    fn index(self, i: i64) -> ref Col { ref self.cols[i] }
}
impl IndexMut[i64] for Frame {
    type Output = Col;
    fn index_mut(mut ref self, i: i64) -> mut ref Col { mut ref self.cols[i] }
}

fn main() {
    let mut f = Frame {
        names: vec!["a".to_string(), "b".to_string()],
        cols: vec![Col { v: vec![1.0, 2.0] }, Col { v: vec![3.0, 4.0] }],
    };
    f[1][0] = 9.0;
    f["a"][1] = 7.0;
    f[0][0] += 0.5;
    f["b"][1] *= 2.0;
    let k = "b".to_string();
    f[k][0] -= 1.0;
    println(f"{f[0][0]} {f[0][1]} {f[1][0]} {f[1][1]} {f[k][0]} {k}");
}
"#,
        "1.5 7.0 8.0 8.0 8.0 b\n",
    );
}
