//! The MIR backend's full-unroll hints (`mir::counters::unroll_full_latches`):
//! a small counting loop whose body reads and writes `Vec` elements is
//! hinted as one over scalars is (kata 265, whose per-element bounds checks
//! fold once its 32-trip rows unroll); a loop with any other call is not.
#![cfg(feature = "llvm")]

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::codegen::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

#[test]
fn mir_unroll_hints_a_loop_over_vec_elements() {
    let elements = ir(r#"fn main() {
    let mut a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    let mut q = 0;
    while q < 40 { a.push(q); b.push(0); q = q + 1; }
    let start = a.len() as i64 - 33;
    let mut t = 0;
    while t < 32 {
        b[t] = a[start + t] + 1;
        t = t + 1;
    }
    println(b[31]);
}
"#);
    assert!(elements.contains("llvm.loop.unroll.full"), "{elements}");
    let calls = ir(r#"fn main() {
    let mut t = 0;
    while t < 32 {
        println(t);
        t = t + 1;
    }
}
"#);
    assert!(!calls.contains("llvm.loop.unroll.full"));
}
