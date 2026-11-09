//! A `ref T` parameter whose pointee holds nothing a callee could write
//! while the borrow lasts (numbers, a `String` or `Vec` header, a shared
//! handle, and tuples, arrays and options of them) is `noalias`: the
//! borrow freezes those bytes for the call, so LLVM may keep them in
//! registers across the stores the body makes (kata 718's `ref Vec`
//! reloaded its header on every write to another vector, B-2026-11-09-8). A
//! struct parameter is left as it was. `KARAC_MIR_NOALIAS_REF=0` turns it
//! off.

#![cfg(feature = "llvm")]

const SRC: &str = r#"
struct Pair { a: i64, b: i64 }

fn total(v: ref Vec[i64], p: ref Pair) -> i64 {
    let mut s = p.a + p.b;
    for i in 0..v.len() {
        s += v[i];
    }
    s
}

fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let p = Pair { a: 4, b: 5 };
    println(f"{total(v, p)}");
}
"#;

#[test]
fn mir_llvm_frozen_ref_params_are_noalias() {
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let def = ir
        .lines()
        .find(|l| l.starts_with("define internal i64 @kara.total("))
        .unwrap_or_else(|| panic!("no `total` in\n{ir}"));
    assert!(
        def.contains(
            "(ptr noalias align 8 dereferenceable(24) %0, ptr align 8 dereferenceable(16) %1)"
        ),
        "{def}"
    );
}
