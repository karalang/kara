//! Codegen driver: target-machine setup, optimization passes, and
//! linker invocation.
//!
//! Houses the `link_executable*` public API plus the post-codegen LLVM
//! infrastructure (`create_target_machine`, `apply_optimization_passes`)
//! and the env-flag readers (`read_opt_level_env`,
//! `read_runtime_debug_metadata_env`, `read_auto_par_env`) that drive
//! optimization-level / debug-metadata / auto-par toggles.
//!
//! Self-contained — does not reference the `Codegen` struct. The
//! `compile_to_ir` / `compile_to_object` entry points stay in
//! `super` (codegen.rs) since they instantiate `Codegen` and need
//! field-visibility access.

use inkwell::module::Module;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::OptimizationLevel;

/// Link an object file into an executable using the system C compiler.
///
/// Also statically links the Kāra runtime library (`libkarac_runtime.a`) and
/// pulls in pthread. The runtime path is resolved from (in order):
///   1. `KARAC_RUNTIME` env var (absolute path to `libkarac_runtime.a`).
///   2. Installed distribution: `<karac-binary-dir>/../lib/libkarac_runtime.a`.
///   3. Development fallback: `<workspace>/target/release/libkarac_runtime.a`.
///
/// See design.md § Runtime Distribution.
pub fn link_executable(obj_path: &str, exe_path: &str) -> Result<(), String> {
    link_executable_impl(obj_path, exe_path, &[])
}

/// Link like [`link_executable`], but prepend extra flags to the `cc` invocation
/// (e.g. `-fsanitize=address`). Used by the memory-behavior E2E test harness to
/// run Kāra-compiled binaries under AddressSanitizer.
///
/// ASAN/LSAN interpose libc `malloc`/`free` globally, so the statically linked
/// `libkarac_runtime.a` does not need to be rebuilt with sanitizer flags for
/// leak detection from Kāra-emitted IR to work. UAF detection *inside* runtime
/// code would require an instrumented runtime build; that is out of scope for
/// this harness, which focuses on codegen-emitted heap operations.
pub fn link_executable_with_sanitizer(
    obj_path: &str,
    exe_path: &str,
    sanitizer_flags: &[&str],
) -> Result<(), String> {
    link_executable_impl(obj_path, exe_path, sanitizer_flags)
}

pub(super) fn link_executable_impl(
    obj_path: &str,
    exe_path: &str,
    extra_cc_args: &[&str],
) -> Result<(), String> {
    let runtime_path = resolve_runtime_path()?;
    let mut cmd = std::process::Command::new("cc");
    for arg in extra_cc_args {
        cmd.arg(arg);
    }
    cmd.args([
        obj_path,
        &runtime_path,
        "-o",
        exe_path,
        "-lm",
        "-lpthread",
        "-ldl",
    ]);
    // Binary-size phase 2: cross-archive DCE. The runtime exports HTTP /
    // JSON / par / map / etc. entry points unconditionally as `#[no_mangle]`,
    // and any program statically links the full archive even if it never
    // calls those subsystems. `-Wl,-dead_strip` (Mach-O) / `-Wl,--gc-sections`
    // (ELF) makes the linker compute reachability from the entry point and
    // drop unreached objects across the cc-line archive boundary. Combined
    // with `lto = "thin"` on the runtime crate (workspace `Cargo.toml`'s
    // `[profile.release.package.karac-runtime]`), the unused tokio / hyper /
    // serde_json subgraph collapses to zero bytes when the program imports
    // none of those stdlib modules. Skipped under sanitizer builds: ASAN's
    // interceptor table is reached via `__asan_*` symbols that aren't always
    // referenced from main, and dead-stripping the table breaks
    // instrumentation. See `runtime/SYMBOL_KEEP_LIST.md` for the keep-list
    // audit; the runtime declares no `#[used]` / `#[link_section]` /
    // `#[ctor]` / `#[dtor]` attributes, so every reachable runtime symbol
    // is anchored through a direct call from codegen-emitted IR.
    if !is_sanitizer_link(extra_cc_args) {
        if cfg!(target_os = "macos") {
            cmd.args(["-Wl,-dead_strip"]);
        } else if cfg!(target_os = "linux") {
            cmd.args(["-Wl,--gc-sections"]);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("Failed to invoke linker: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Linker failed: {}", stderr));
    }

    // Binary-size phase 1: strip non-global symbols from the linked
    // executable. `strip -x` keeps the global symbol table intact (so the
    // `karac_*` runtime entry points the program actually calls remain
    // resolvable, and ASAN-instrumented builds keep the `__asan_*`
    // globals they need at runtime) and drops local debug symbols only.
    // Skipped when sanitizer flags are passed: ASAN's stack-trace
    // symbolication walks local symbol tables for function-name lookup,
    // and stripping them turns "leak in karac_par_run+0x42" into
    // "leak in <unknown>+0x42" — the sanitizer harness keeps the full
    // symbol table for diagnostic legibility. Unix-only at v1; Windows
    // toolchains lack a drop-in `strip` equivalent.
    //
    // `strip` failures are non-fatal — the executable already exists and
    // works; we just lose the size-reduction benefit on this specific
    // build. Print a stderr note rather than failing the codegen path so
    // hosts without `strip` (rare on macOS/Linux) keep producing
    // working binaries.
    if cfg!(unix) && !is_sanitizer_link(extra_cc_args) {
        let strip_status = std::process::Command::new("strip")
            .args(["-x", exe_path])
            .output();
        match strip_status {
            Ok(o) if !o.status.success() => {
                eprintln!(
                    "warning: `strip -x {exe_path}` failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                eprintln!("warning: failed to invoke `strip`: {e}");
            }
            _ => {}
        }
    }
    Ok(())
}

/// True if any extra cc flag enables a sanitizer instrumentation runtime.
/// Sanitizer runtimes rely on local symbol tables for stack-trace
/// symbolication, so stripped sanitizer binaries print
/// `<unknown>+0xN` frames in their reports — keep symbols on those
/// builds.
pub(super) fn is_sanitizer_link(extra_cc_args: &[&str]) -> bool {
    extra_cc_args.iter().any(|a| a.starts_with("-fsanitize"))
}

pub(super) fn resolve_runtime_path() -> Result<String, String> {
    if let Ok(p) = std::env::var("KARAC_RUNTIME") {
        if std::path::Path::new(&p).exists() {
            return Ok(p);
        }
        return Err(format!("KARAC_RUNTIME set to {p} but file does not exist"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let installed = bin_dir.join("../lib/libkarac_runtime.a");
            if installed.exists() {
                return Ok(installed.to_string_lossy().into_owned());
            }
        }
    }
    let dev =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/libkarac_runtime.a");
    if dev.exists() {
        return Ok(dev.to_string_lossy().into_owned());
    }
    Err(
        "libkarac_runtime.a not found; set KARAC_RUNTIME or build the runtime crate (`cargo build -p karac-runtime --release`)".to_string(),
    )
}

pub(super) fn create_target_machine() -> Result<TargetMachine, String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("Failed to initialize native target: {}", e))?;

    let triple = TargetMachine::get_default_triple();
    let target =
        Target::from_triple(&triple).map_err(|e| format!("Failed to get target: {}", e))?;

    target
        .create_target_machine(
            &triple,
            "generic",
            "",
            backend_optimization_level(),
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "Failed to create target machine".to_string())
}

/// Resolve the optimization level used by both the **target machine** (LLVM
/// backend codegen quality) and the mid-end **pass pipeline** (the
/// `default<…>` string passed to `Module::run_passes`). They must stay in
/// sync — emitting `-O0`-level pass-pipeline output into a `-O2`-level
/// backend (or vice versa) leaves performance on the table without saving
/// compile time.
///
/// Reads the `KARAC_OPT_LEVEL` env var (set per-invocation, not cached at
/// compiler-binary build time):
///
/// | env value | backend `OptimizationLevel` | pass-pipeline string |
/// |-----------|------------------------------|----------------------|
/// | `0`       | `None`                       | (passes skipped)     |
/// | `1`       | `Less`                       | `default<O1>`        |
/// | unset / `2` / `s` / `z` | `Default`         | `default<O2>` *(default)* |
/// | `3`       | `Aggressive`                 | `default<O3>`        |
///
/// `s` and `z` (size-optimized) map onto `-O2` for the backend at v1 — LLVM's
/// new-pass-manager pipeline strings have native `default<Os>` / `default<Oz>`
/// counterparts but inkwell's `OptimizationLevel` enum has no separate
/// size-tier. Until size-tier is wired through `--target` flags, the env var
/// gives users a way to ask for size-optimization mid-end passes while
/// keeping the backend at `-O2` parity with `Default`.
pub(super) fn read_opt_level_env() -> &'static str {
    // OnceLock cache so repeated `karac build` invocations within one
    // compiler-process lifetime (e.g., tests that compile many .kara files)
    // observe a stable level. Test-only `read_opt_level_env_uncached` —
    // mirrors the `KARAC_RUNTIME_DEBUG_METADATA` cache pattern.
    static CACHE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    CACHE.get_or_init(read_opt_level_env_uncached)
}

pub(super) fn read_opt_level_env_uncached() -> &'static str {
    match std::env::var("KARAC_OPT_LEVEL").as_deref() {
        Ok("0") => "0",
        Ok("1") => "1",
        Ok("3") => "3",
        // Default + explicit "2", "s", "z" all map to the `-O2` mid-end
        // pipeline. See doc-comment table above.
        _ => "2",
    }
}

pub(super) fn backend_optimization_level() -> OptimizationLevel {
    match read_opt_level_env() {
        "0" => OptimizationLevel::None,
        "1" => OptimizationLevel::Less,
        "3" => OptimizationLevel::Aggressive,
        _ => OptimizationLevel::Default,
    }
}

/// Run LLVM mid-end optimization passes on the constructed module.
///
/// karac builds raw IR with locals as alloca'd stack slots (the natural
/// shape produced by a non-SSA-aware codegen). Without this pass run,
/// `mem2reg` + downstream optimizations never fire and tight integer
/// kernels (e.g., the Parallax bench's `busy_loop`) emit ~12 instructions
/// per iter through a stack-spill chain instead of the 4-instruction
/// register-only inner loop LLVM-O2 produces. See
/// `docs/investigations/parallax_perf.md § Findings, 2026-05-10` for the
/// before/after disasm + bench numbers that motivated wiring this up.
///
/// Pass string is the LLVM new-pass-manager pipeline alias `default<O…>` —
/// the same pipeline `clang -O…` runs. `KARAC_OPT_LEVEL=0` short-circuits
/// the pass run entirely (debugging fallback for "did the optimizer eat
/// my IR construct?"). Other levels run the matching `default<O…>`
/// pipeline; size tiers (`s`, `z`) currently fold into `-O2` (see
/// `read_opt_level_env` table).
pub(super) fn apply_optimization_passes(
    module: &Module<'_>,
    target_machine: &TargetMachine,
) -> Result<(), String> {
    let level = read_opt_level_env();
    if level == "0" {
        return Ok(());
    }
    let pipeline = match level {
        "1" => "default<O1>",
        "3" => "default<O3>",
        _ => "default<O2>",
    };
    let options = inkwell::passes::PassBuilderOptions::create();
    module
        .run_passes(pipeline, target_machine, options)
        .map_err(|e| {
            format!(
                "LLVM optimization pass `{}` failed: {}. \
                 Workaround: set KARAC_OPT_LEVEL=0 to skip the pass run.",
                pipeline,
                e.to_string()
            )
        })
}

/// Read the `KARAC_RUNTIME_DEBUG_METADATA` env var to decide whether
/// `KARAC_SPAWN_SITES` (and friends) emit populated. Slice 3 of the
/// Debugger Contract; see `Codegen::runtime_debug_metadata_enabled` for
/// the field doc and `phase-8-stdlib-floor.md` § "Auto-Concurrency
/// Codegen — Debugger Contract slice 3" for the spec.
///
/// - `Ok("0")` → `false` (gate explicitly off).
/// - `Ok(_)`   → `true` (any other value, including empty).
/// - `Err(_)`  → `true` (dev default; profile-aware defaults land in
///   Phase 8.5 Track 2).
pub(super) fn read_runtime_debug_metadata_env() -> bool {
    !matches!(std::env::var("KARAC_RUNTIME_DEBUG_METADATA"), Ok(v) if v == "0")
}

/// Read the `KARAC_AUTO_PAR` env var to decide whether
/// `compile_function_body` dispatches non-trivial parallel groups to
/// `emit_par_run` (auto-par codegen) or falls back to plain sequential
/// `compile_block`. Slice 6 of the Auto-Concurrency Codegen track;
/// mirrors slice 3's `read_runtime_debug_metadata_env` shape exactly.
/// See `phase-8-stdlib-floor.md` § "Auto-Concurrency Codegen —
/// Parallax-lite Workload" for the spec.
///
/// - `Ok("0")` → `false` (gate explicitly off — sequential codegen).
/// - `Ok(_)`   → `true` (any other value, including empty).
/// - `Err(_)`  → `true` (dev default — auto-par on by default; the
///   user-facing `--sequential` CLI flag is a Phase 8.5 Track 2
///   deliverable when the profile system ships).
///
/// Returns `true` iff auto-par dispatch is enabled. The
/// `Codegen::auto_par_disabled` field is `!return_value` so the
/// compile-time check reads naturally as `if self.auto_par_disabled`.
pub(super) fn read_auto_par_env() -> bool {
    !matches!(std::env::var("KARAC_AUTO_PAR"), Ok(v) if v == "0")
}
