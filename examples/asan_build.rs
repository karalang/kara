//! Build a single `.kara` file to a binary linked with AddressSanitizer.
//!
//! The investigation harness for heap-corruption findings: `karac build` links
//! plain, so a double-free or use-after-free surfaces only as glibc's
//! `free(): double free detected` or a SIGSEGV, with no allocation site and no
//! stack. This links the same emitted object with `-fsanitize=address` (the
//! path `tests/memory_sanitizer.rs` uses) so the report names the free, the
//! previous free, and the allocation.
//!
//!   cargo run --features llvm --example asan_build -- path/to/file.kara /tmp/out
//!   ASAN_OPTIONS=detect_leaks=0 /tmp/out
//!
//! `lower_program` is not optional here for the same reason it is not in
//! `dump_ir`: it forwards the typechecker's span-keyed side tables into the AST,
//! and skipping it degrades pattern bindings to the i64 default.
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: asan_build <file.kara> <out>");
    let out = std::env::args()
        .nth(2)
        .expect("usage: asan_build <file.kara> <out>");
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
    let eff = karac::effectcheck(&program);
    let conc = karac::concurrency_analyze_typed(&program, &eff, Some(&tc));

    let obj = format!("{out}.o");
    karac::codegen::compile_to_object(&program, &obj, Some(&own), Some(&conc))
        .expect("codegen failed");
    karac::codegen::link_executable_with_sanitizer(&obj, &out, &["-fsanitize=address"])
        .expect("asan link failed");
    eprintln!("built (asan): {out}");
}
