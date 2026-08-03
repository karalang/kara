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

/// Emit the git-derived version stamp (`dev.<commit-count>+g<short-sha>`
/// with a `.dirty` suffix on uncommitted trees) as `KARAC_VERSION_STAMP`.
/// `karac --version` renders `<CARGO_PKG_VERSION>-<stamp>` — Zig-style
/// derived build identity: the base version is a human decision that
/// moves on citable milestones only; the suffix identifies the exact
/// build with zero bookkeeping, and the short SHA maps a user's version
/// string directly onto bug-ledger `fix` SHAs and `git log`.
///
/// On a non-git checkout (source tarball) every git invocation fails and
/// the stamp falls back to `dev.unknown` — a build is never blocked on
/// git being present, but an unstamped binary still says so instead of
/// masquerading as a known build.
fn emit_version_stamp() {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git").args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    // Re-stamp when HEAD moves: `.git/HEAD` changes on branch switches,
    // and the ref file it points at changes on every commit. Cost: one
    // build-script rerun + an incremental karac recompile per commit —
    // the price of the version string never lying about which commit
    // built the binary.
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{}", reference.trim());
        }
    }
    // A SHALLOW clone truncates history, so `rev-list --count` reports
    // the clone depth, not the real commit ordinal — and CI checkouts
    // default to depth 1, which would stamp every release build as
    // `dev.1`. Never emit a number that lies: shallow clones stamp the
    // literal `shallow` in the count slot (the short SHA remains the
    // true identifier), which doubles as the loud signal that a release
    // pipeline forgot `fetch-depth: 0`.
    let shallow = std::path::Path::new(".git/shallow").exists();
    let count = if shallow {
        Some("shallow".to_string())
    } else {
        git(&["rev-list", "--count", "HEAD"])
    };
    let sha = git(&["rev-parse", "--short=9", "HEAD"]);
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty());
    let stamp = match (count, sha) {
        (Some(count), Some(sha)) => {
            let dirty_suffix = if dirty == Some(true) { ".dirty" } else { "" };
            format!("dev.{count}+g{sha}{dirty_suffix}")
        }
        _ => "dev.unknown".to_string(),
    };
    println!("cargo:rustc-env=KARAC_VERSION_STAMP={stamp}");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    emit_version_stamp();

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
