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
    // `KARAC_DUMP_AUTO_PAR=1` runs the concurrency analysis and hands it to
    // codegen, i.e. what `karac build` does by default. Without it the dump is
    // the `KARAC_AUTO_PAR=0` shape, and the two differ in ways that matter for
    // a memory bug: auto-par lifts work into `__par_branch_*` functions, so a
    // value that never escaped one frame sequentially now crosses a thread
    // boundary through the fork's return struct. B-2026-08-18-48 is only
    // OBSERVABLE in that shape -- sequentially the same missing free hides
    // because the allocation is optimized away entirely.
    let concurrency = std::env::var("KARAC_DUMP_AUTO_PAR")
        .is_ok_and(|v| v != "0")
        .then(|| {
            let effects = karac::effectcheck(&program);
            karac::concurrency_analyze_typed(&program, &effects, Some(&tc))
        });
    let ir = karac::codegen::compile_to_ir(&program, Some(&own), concurrency.as_ref()).unwrap();
    println!("{ir}");
}
