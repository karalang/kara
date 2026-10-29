//! The MIR backend's partial-unroll hint (`mir::counters::unroll_partial_latches`,
//! B-2026-10-28-39): a pure-scalar counting loop with a runtime bound gets
//! `llvm.loop.unroll.count 4`, the legacy backend's hint for the same shape
//! (B-2026-07-08-24), because LLVM 18's cost model declines to unroll a
//! loop-carried scalar recurrence such as kata 70's. A loop that touches
//! memory or calls anything is left to the cost model, and a small constant
//! trip count keeps the full-unroll hint instead.
#![cfg(feature = "llvm")]

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

const COUNT: &str = "llvm.loop.unroll.count";

/// Kata 70's shape: `while i <= n` over scalars, `n` known only at run time.
#[test]
fn mir_unroll_partial_hints_a_scalar_recurrence() {
    let fib = ir(r#"fn climb(n: i64) -> i64 {
    if n <= 2 {
        return n;
    }
    let mut a = 1;
    let mut b = 2;
    let mut i = 3;
    while i <= n {
        let next = a + b;
        a = b;
        b = next;
        i = i + 1;
    }
    b
}

fn main() {
    let mut acc = 0;
    let mut k = 0;
    while k < 1000 {
        acc = (acc + climb(1 + k % 45)) % 1000000007;
        k = k + 1;
    }
    println(acc);
}
"#);
    assert!(fib.contains(COUNT), "{fib}");
}

/// The strict `<` guard with a runtime bound is hinted too.
#[test]
fn mir_unroll_partial_hints_a_strict_runtime_bound() {
    let src = ir(r#"fn sum_squares(n: i64) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < n {
        s = s + i * i;
        i = i + 1;
    }
    s
}

fn main() {
    println(sum_squares(1000));
}
"#);
    assert!(src.contains(COUNT), "{src}");
}

/// Element access is memory: the cost model already serves an array scan,
/// and a forced count only bloats it.
#[test]
fn mir_unroll_partial_skips_a_loop_over_vec_elements() {
    let src = ir(r#"fn total(v: ref Vec[i64]) -> i64 {
    let mut s = 0;
    let mut i = 0;
    while i < v.len() {
        s = s + v[i];
        i = i + 1;
    }
    s
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut q = 0;
    while q < 100 { v.push(q); q = q + 1; }
    println(total(v));
}
"#);
    assert!(!src.contains(COUNT), "{src}");
}

/// Any call in the body keeps the hint off.
#[test]
fn mir_unroll_partial_skips_a_loop_with_a_call() {
    let src = ir(r#"fn main() {
    let n = 50;
    let mut i = 0;
    while i < n {
        println(i);
        i = i + 1;
    }
}
"#);
    assert!(!src.contains(COUNT), "{src}");
}

/// A constant trip count of at most 32 is unrolled fully, not by four.
#[test]
fn mir_unroll_partial_yields_to_the_full_hint() {
    let src = ir(r#"fn main() {
    let mut s = 0;
    let mut i = 0;
    while i < 16 {
        s = s + i * 3;
        i = i + 1;
    }
    println(s);
}
"#);
    assert!(src.contains("llvm.loop.unroll.full"), "{src}");
    assert!(!src.contains(COUNT), "{src}");
}
