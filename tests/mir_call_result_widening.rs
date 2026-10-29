//! A call's result widened into a wider numeric slot (design.md §5,
//! B-2026-10-29-22): an annotated `let`, a return, a tail, an `if` or
//! `match` arm, a block. The call writes a temporary of its own type and
//! the slot takes it widened; compiled, the slot was written with the
//! narrower bytes and half of it left uninitialized. Outputs are worked
//! out by hand.
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
        let dir = std::env::temp_dir().join(format!("karac-widen-{}-{n}", std::process::id()));
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

/// The row's program: each form of a call's result into a wider slot.
#[test]
fn mir_widens_a_calls_result_into_its_slot() {
    runs(
        r#"struct S {
    x: f32,
}

impl S {
    fn get(ref self) -> f32 {
        self.x
    }
}

fn h() -> f32 {
    1.5
}

fn i() -> i32 {
    -7
}

fn ret_stmt() -> f64 {
    return h();
}

fn ret_tail() -> f64 {
    h()
}

fn ret_int() -> i64 {
    i()
}

fn take(x: f64) -> f64 {
    x
}

fn main() {
    let a: f64 = h();
    println(f"let call: {a}");
    let n: i64 = i();
    println(f"let int call: {n}");
    let s = S { x: 2.5 };
    let m: f64 = s.get();
    println(f"let method: {m}");
    println(f"return stmt: {ret_stmt()}");
    println(f"tail: {ret_tail()}");
    println(f"tail int: {ret_int()}");
    let b: f64 = if true { h() } else { 0.0 };
    println(f"if arm: {b}");
    let mut c: f64 = 0.0;
    c = h();
    println(f"assign: {c}");
    println(f"argument: {take(h())}");
    let g = h();
    let d: f64 = g;
    println(f"let from a binding: {d}");
}
"#,
        "let call: 1.5
let int call: -7
let method: 2.5
return stmt: 1.5
tail: 1.5
tail int: -7
if arm: 1.5
assign: 1.5
argument: 1.5
let from a binding: 1.5
",
    );
}

/// A field, an element, an operator, a closure call, a block, a `match`
/// arm and a payload or tuple part widen too.
#[test]
fn mir_widens_other_narrow_values_into_their_slots() {
    runs(
        r#"struct S { x: f32, n: i32 }
fn h() -> f32 { 1.5 }
fn i() -> i32 { -7 }
fn m(k: i64) -> f64 {
    match k {
        0 => h(),
        _ => { i() }
    }
}
fn main() {
    let s = S { x: 2.5, n: -3 };
    let a: f64 = s.x;
    let b: i64 = s.n;
    let v: Vec[i32] = vec![-4, 5];
    let c: i64 = v[0];
    let x: f32 = 0.25;
    let y: f32 = 0.5;
    let d: f64 = x + y;
    let f = |q: i32| q * 2;
    let e: i64 = f(-6);
    let g: f64 = { h() };
    let o: Option[f64] = Some(h());
    let t: (f64, i64) = (h(), i());
    println(f"{a} {b} {c} {d} {e} {g} {m(0)} {m(1)} {o} {t}");
}
"#,
        "2.5 -3 -4 0.75 -12 1.5 1.5 -7.0 Some(1.5) (1.5, -7)
",
    );
}
