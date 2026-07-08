//! Build script — dynamic-symbol export for the LLJIT execution backend.
//!
//! The always-JIT lane (`src/codegen/lljit.rs`, the `karac_jit_runner`
//! bin, and the in-process JIT tests) resolves the statically-linked
//! `karac_*` runtime FFI symbols through ORC's process-symbol-search
//! generator (`LLVMOrcCreateDynamicLibrarySearchGeneratorForProcess`),
//! which is a `dlsym(RTLD_DEFAULT, …)` lookup under the hood.
//!
//! On ELF targets `dlsym` on the running executable only sees symbols the
//! program exports in its **dynamic** symbol table (`.dynsym`). Rust links
//! executables without exporting their symbols, so the `karac_*` symbols —
//! kept alive against DCE by `karac_runtime::__preserve_no_mangle_symbols`
//! but living only in `.symtab` — are invisible to `dlsym`. The JIT then
//! fails to materialize any program that touches the runtime with
//! `Symbols not found: [karac_runtime_*]`, and the program produces empty
//! output (observed as ~1,400 codegen-E2E-via-JIT failures on Linux while
//! macOS stayed green — Mach-O's flat/two-level `dlsym` resolves main-image
//! symbols without an export flag, so this never surfaced there).
//!
//! Fix: add the `karac_*` surface to `.dynsym` for the JIT-hosting binaries
//! (`karac`, `karac_jit_runner`) and the integration-test binaries that run
//! the JIT in-process. The export is scoped to the `karac_*` glob rather
//! than a blanket `--export-dynamic` so `.dynsym` stays lean (the runtime
//! surface is ~500 symbols; the whole binary's is far larger). Only emitted
//! when the JIT engine is actually compiled in (the `lljit_prototype`
//! feature) and only on ELF platforms whose `dlsym` needs it.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Nothing to do unless the JIT engine is compiled in. Cargo sets
    // `CARGO_FEATURE_<NAME>` for every active feature (uppercased, `-`→`_`).
    // Since LLJIT Slice 1 (de-gate) the JIT rides the `llvm` feature, so the
    // ELF dynamic-symbol export keys on `CARGO_FEATURE_LLVM`.
    if env::var_os("CARGO_FEATURE_LLVM").is_none() {
        return;
    }

    // Mach-O's `dlsym` resolves main-image symbols without an export flag,
    // and Windows is not a JIT target — so the export is only needed (and
    // only understood) on ELF/GNU-ld-style toolchains.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let needs_export = matches!(target_os.as_str(), "linux" | "android")
        || target_os.ends_with("bsd")
        || target_os == "dragonfly";
    if !needs_export {
        return;
    }

    // `--export-dynamic-symbol=<glob>` (GNU ld / gold / lld) adds every
    // matching symbol to `.dynsym`. Apply to both the package binaries
    // (`bins`) and the integration-test binaries (`tests`) — the latter
    // host the JIT in-process (`tests/lljit_prototype.rs`) and so need the
    // same visibility.
    for scope in ["bins", "tests"] {
        println!("cargo:rustc-link-arg-{scope}=-Wl,--export-dynamic-symbol=karac_*");
    }
}
