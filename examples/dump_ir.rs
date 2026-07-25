//! Dump the pre-optimization LLVM IR for a single `.kara` file.
//!
//! The investigation harness for codegen bugs — reach for this before
//! hand-reading `karac build` output when you need to see what the backend
//! actually emitted.
//!
//!   cargo run --features llvm --example dump_ir -- path/to/file.kara
//!
//! **`lower_program` is not optional.** Lowering is what forwards the
//! typechecker's span-keyed side tables into the AST (`pattern_binding_types`,
//! `pattern_binding_inner_types`, the Str-span table — see `src/lowering.rs`).
//! Skip it and codegen sees them EMPTY, degrading every match-arm payload
//! binding to the 1-word i64 default: a correct program then dumps as an
//! `alloca i64` GEP'd at `{ptr, i64, i64}` width and a phi-less `match.merge`.
//! Both are artifacts of the missing pass, not of the compiler — an earlier
//! investigation lost a session to exactly that (bug-ledger B-2026-07-25-1's
//! retraction note). Keep this chain in sync with the real driver.
fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_ir <file.kara>");
    let src = std::fs::read_to_string(&path).unwrap();
    let parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let mut program = parsed.program;
    let res = karac::resolve(&program);
    assert!(res.errors.is_empty(), "resolve errors: {:?}", res.errors);
    let tc = karac::typecheck(&program, &res);
    assert!(tc.errors.is_empty(), "typecheck errors: {:?}", tc.errors);
    karac::lowering::lower_program(&mut program, &tc);
    let own = karac::ownershipcheck(&program, &tc);
    let ir = karac::codegen::compile_to_ir(&program, Some(&own), None).unwrap();
    println!("{ir}");
}
