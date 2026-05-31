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
    // Binary-size phase 4 (Part B): pick the lean, rustls-free runtime
    // archive (`libkarac_runtime_min.a`) for programs that reference none
    // of the TLS-only runtime symbols. The lean archive omits the
    // rustls/ring dependency tree, whose unwinding/backtrace-symbolizer
    // machinery would otherwise survive `-dead_strip` onto the compute
    // path (~65 KiB on every binary — see phase-7-codegen.md § "Phase 4").
    // Detection reads the just-emitted object's referenced symbols
    // directly (ground truth), so it stays decoupled from codegen
    // internals. Any uncertainty falls back to the full archive, which is
    // always correct.
    let prefer_min = !object_references_tls(obj_path);
    let runtime_path = resolve_runtime_path(prefer_min)?;
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

/// Runtime symbols that exist ONLY in the full (`tls`-feature) archive.
/// If a program's emitted object references any of these, it must link
/// the full `libkarac_runtime.a`; otherwise the lean
/// `libkarac_runtime_min.a` (rustls-free, ~65 KiB lighter on the linked
/// binary — see phase-7-codegen.md § "Phase 4") is sufficient and
/// preferred. Matched as substrings of `nm` symbol names (Mach-O prefixes
/// each with `_`, so substring matching is prefix-agnostic). NOTE the
/// `_https` discriminator: plain `karac_runtime_serve_http` /
/// `_serve_http_static` and the server-side `_http_request_*` /
/// `_http_response_*` getters stay in BOTH archives and must NOT match.
const TLS_RUNTIME_SYMBOL_MARKERS: &[&str] = &[
    "karac_runtime_tls_",
    "karac_runtime_serve_https",
    "karac_runtime_http_client_",
    "karac_runtime_http_builder_",
    "karac_runtime_ws_accept_tls",
];

/// Scan `obj_path`'s symbol table (via `nm`) for any reference to a
/// TLS-only runtime symbol. A Kāra-emitted object never *defines*
/// `karac_runtime_*` (those live in the archive), so any appearance is a
/// reference. Conservative on failure: if `nm` is missing or errors, or
/// `KARAC_FORCE_FULL_RUNTIME` is set, returns `true` so the caller links
/// the full archive (always correct, just larger).
fn object_references_tls(obj_path: &str) -> bool {
    if std::env::var_os("KARAC_FORCE_FULL_RUNTIME").is_some() {
        return true;
    }
    let output = std::process::Command::new("nm").arg(obj_path).output();
    match output {
        Ok(o) if o.status.success() => {
            symbol_listing_references_tls(&String::from_utf8_lossy(&o.stdout))
        }
        // nm absent / failed: can't prove the program is TLS-free, so be safe.
        _ => true,
    }
}

/// Pure predicate over `nm`-style symbol-listing text: true iff any line
/// names a TLS-only runtime symbol. Split out from the `nm` shell-out so
/// the marker matching — in particular the `serve_http` vs `serve_https`
/// and `http_request`/`http_response` (server, both archives) vs
/// `http_client`/`http_builder` (client, TLS-only) discrimination — is
/// unit-testable without an object file.
fn symbol_listing_references_tls(nm_output: &str) -> bool {
    nm_output
        .lines()
        .any(|line| TLS_RUNTIME_SYMBOL_MARKERS.iter().any(|m| line.contains(m)))
}

/// Resolve the runtime archive to link. When `prefer_min` is true (the
/// program referenced no TLS-only symbol), prefer the lean
/// `libkarac_runtime_min.a` at each resolution tier, falling back to the
/// full `libkarac_runtime.a` when no lean archive is present (so a
/// distribution that ships only the full archive still links correctly).
/// Resolution order: `KARAC_RUNTIME` override → installed `<bin>/../lib`
/// → dev `target/release`.
pub(super) fn resolve_runtime_path(prefer_min: bool) -> Result<String, String> {
    const FULL: &str = "libkarac_runtime.a";
    const MIN: &str = "libkarac_runtime_min.a";

    // Pick the preferred archive name within a directory: lean first when
    // `prefer_min`, else the full archive.
    let pick = |dir: &std::path::Path| -> Option<String> {
        if prefer_min {
            let m = dir.join(MIN);
            if m.exists() {
                return Some(m.to_string_lossy().into_owned());
            }
        }
        let f = dir.join(FULL);
        if f.exists() {
            return Some(f.to_string_lossy().into_owned());
        }
        None
    };

    // 1. Explicit override. Honor the given path, but when a lean archive
    //    would do and a `libkarac_runtime_min.a` sits beside the override,
    //    use that instead — so pointing `KARAC_RUNTIME` at a full archive
    //    still gets the size win for compute-only programs.
    if let Ok(p) = std::env::var("KARAC_RUNTIME") {
        let path = std::path::Path::new(&p);
        if !path.exists() {
            return Err(format!("KARAC_RUNTIME set to {p} but file does not exist"));
        }
        if prefer_min {
            if let Some(dir) = path.parent() {
                let sib = dir.join(MIN);
                if sib.exists() {
                    return Ok(sib.to_string_lossy().into_owned());
                }
            }
        }
        return Ok(p);
    }

    // 2. Installed distribution: `<karac-binary-dir>/../lib/`.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(found) = pick(&bin_dir.join("../lib")) {
                return Ok(found);
            }
        }
    }

    // 3. Development fallback: `<workspace>/target/release/`.
    let dev_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release");
    if let Some(found) = pick(&dev_dir) {
        return Ok(found);
    }

    Err(
        "libkarac_runtime.a not found; set KARAC_RUNTIME or build the runtime crate (`cargo rustc -p karac-runtime --release --crate-type staticlib` — NOT plain `cargo build`, which co-emits the rlib and defeats the staticlib's dead-strip; see runtime/Cargo.toml). For the lean compute-only archive also build `--no-default-features` and install it as `libkarac_runtime_min.a` alongside.".to_string(),
    )
}

pub(super) fn create_target_machine() -> Result<TargetMachine, String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("Failed to initialize native target: {}", e))?;

    let triple = TargetMachine::get_default_triple();
    let target =
        Target::from_triple(&triple).map_err(|e| format!("Failed to get target: {}", e))?;

    let triple_str = triple.as_str().to_str().unwrap_or("");
    let (cpu, features) = default_cpu_and_features(triple_str);

    target
        .create_target_machine(
            &triple,
            cpu,
            features,
            backend_optimization_level(),
            // PIC, not `Default`. The link step (`link_executable_impl`)
            // invokes `cc` with no `-no-pie`, and every modern toolchain
            // defaults `cc` to producing a PIE — so the object we emit must
            // be position-independent. Under `RelocMode::Default`, LLVM picks
            // a Static reloc model on `x86_64-*-linux`, which emits absolute
            // 32-bit relocations (`R_X86_64_32`) against `.rodata` string
            // literals; the default-PIE `ld` then rejects the link with
            // "can not be used when making a PIE object". AArch64 dodged this
            // because its ADRP/ADD addressing is PC-relative even under Static
            // reloc, so the bug only surfaced on the first x86_64-Linux build.
            // PIC matches what rustc/clang emit for all our Tier-1 targets and
            // is a no-op on Darwin (which is PIC-only regardless), so setting
            // it unconditionally fixes x86_64-Linux without regressing the
            // arm64-Linux / macOS paths — and yields ASLR-enabled binaries.
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| "Failed to create target machine".to_string())
}

/// Per-target CPU + feature defaults, mirroring rustc's target-spec baselines.
///
/// LLVM's `"generic"` default emits ARMv8.0-A on aarch64 and v1 AMD64 on
/// x86_64 — strictly conservative, but on `aarch64-apple-darwin` that means
/// shipping pre-M1 instructions to a fleet where every device is M1 or newer.
/// rustc encodes the right per-target baseline in its target-spec JSON; this
/// table mirrors those values so karac-built binaries don't lag what every
/// other shipping compiler produces.
///
/// Override via `--target-cpu` / `KARAC_TARGET_CPU` / `[release] target-cpu`
/// in `kara.toml` lands as a follow-up (see phase-10-targets.md). The
/// fallback `("generic", "")` is intentional for unknown triples: better a
/// portable binary than one that won't load.
///
/// See `design.md § CPU Baseline Targeting`.
fn default_cpu_and_features(triple: &str) -> (&'static str, &'static str) {
    // Triples vary in suffix shape (`arm64-apple-darwin25.0.0` vs
    // `aarch64-apple-darwin`), so match on the arch prefix and OS
    // substring instead of equality on the full string.
    let is_aarch64 = triple.starts_with("aarch64-") || triple.starts_with("arm64-");
    let is_x86_64 = triple.starts_with("x86_64-");
    let is_darwin = triple.contains("-apple-darwin");
    let is_linux = triple.contains("-linux-");

    match (is_aarch64, is_x86_64, is_darwin, is_linux) {
        (true, false, true, false) => ("apple-m1", ""),
        (true, false, false, true) => ("generic", "+v8a,+outline-atomics"),
        (false, true, false, true) => ("x86-64", ""),
        (false, true, true, false) => ("core2", ""),
        _ => ("generic", ""),
    }
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
    // Coroutine lowering is a CORRECTNESS pass, not an optimization: a
    // function marked `presplitcoroutine` (the A2 network-async transform —
    // see `docs/spikes/network-async-coroutine-transform.md`) is NOT a
    // valid runnable function until CoroSplit rewrites it into the
    // ramp/resume/destroy clones. So the coro pipeline must run at EVERY
    // opt level, including `-O0` / `KARAC_OPT_LEVEL=0` — otherwise a debug
    // build would silently emit un-split coroutines (a no-op task, exactly
    // the bug C class). For non-coroutine modules (everything today) this is
    // a pure no-op: the coro passes only touch `presplitcoroutine` funcs.
    {
        let coro_opts = inkwell::passes::PassBuilderOptions::create();
        module
            .run_passes(
                "coro-early,coro-split,coro-cleanup",
                target_machine,
                coro_opts,
            )
            .map_err(|e| {
                format!(
                    "LLVM coroutine lowering passes failed: {}. \
                     This is a correctness pass (CoroSplit) — it cannot be \
                     skipped for `presplitcoroutine` functions.",
                    e.to_string()
                )
            })?;
    }

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

/// Read the `KARAC_STRIP_CONTRACTS` env var to decide whether contract
/// machinery (`requires` / `ensures` / `old(...)` capture / struct & impl
/// `invariant` checks) is emitted into AOT binaries. design.md § Contracts:
/// contracts are "checked at runtime in debug builds, stripped in release" —
/// so a release build elides every contract assert, paying zero runtime cost
/// (including the `old(...)` pre-state clone). This is the codegen trigger
/// for that release behavior; a future `karac build --release` CLI flag sets
/// the same env var (mirrors how `KARAC_OPT_LEVEL` / `KARAC_AUTO_PAR` are
/// env-driven build knobs read fresh at `Codegen` construction).
///
/// - `Ok("1")` / `Ok("true")` → `true` (release — strip all contracts).
/// - anything else (incl. unset) → `false` (debug default — contracts active).
///
/// Returns `true` iff contracts should be stripped. Read fresh per `Codegen`
/// (no `OnceLock` cache) so a process compiling many programs can vary it,
/// and stored in `Codegen::strip_contracts`.
pub(super) fn read_strip_contracts_env() -> bool {
    matches!(std::env::var("KARAC_STRIP_CONTRACTS"), Ok(v) if v == "1" || v == "true")
}

#[cfg(test)]
mod tests {
    use super::{default_cpu_and_features, symbol_listing_references_tls};

    #[test]
    fn tls_detection_flags_client_tls_https_ws_symbols() {
        // Each TLS-only symbol (as `nm` would print it, Mach-O `_` prefix)
        // must route a program to the full archive.
        for sym in [
            "0000000000000000 U _karac_runtime_tls_config_new",
            "                 U _karac_runtime_tls_client_connect",
            "                 U _karac_runtime_serve_https",
            "                 U _karac_runtime_http_client_get",
            "                 U _karac_runtime_http_client_post",
            "                 U _karac_runtime_http_builder_send",
            "                 U _karac_runtime_ws_accept_tls",
        ] {
            assert!(
                symbol_listing_references_tls(sym),
                "expected TLS detection for `{sym}`"
            );
        }
    }

    #[test]
    fn tls_detection_does_not_flag_compute_or_plain_server_symbols() {
        // The lean archive keeps plain HTTP serving, TCP, plain WS, JSON,
        // par, map, string, and the server-side request/response getters —
        // none of these may trip TLS detection (the discriminator is the
        // `_https` suffix and the `_client_`/`_builder_` infixes).
        let listing = "\
                 U _karac_par_reduce\n\
                 U _karac_par_run\n\
                 U _karac_map_new\n\
                 U _karac_string_clone\n\
                 U _karac_runtime_serve_http\n\
                 U _karac_runtime_serve_http_static\n\
                 U _karac_runtime_http_request_path\n\
                 U _karac_runtime_http_request_method\n\
                 U _karac_runtime_http_response_set_body\n\
                 U _karac_runtime_http_response_header\n\
                 U _karac_runtime_ws_accept\n\
                 U _karac_runtime_tcp_bind\n\
                 U _karac_runtime_json_parse\n";
        assert!(
            !symbol_listing_references_tls(listing),
            "compute / plain-server symbols must not trip TLS detection"
        );
    }

    #[test]
    fn tls_detection_empty_listing_is_tls_free() {
        assert!(!symbol_listing_references_tls(""));
    }

    #[test]
    fn aarch64_apple_darwin_defaults_to_apple_m1() {
        assert_eq!(
            default_cpu_and_features("aarch64-apple-darwin"),
            ("apple-m1", "")
        );
        // Tolerate inkwell's `arm64-apple-darwin25.0.0` shape too.
        assert_eq!(
            default_cpu_and_features("arm64-apple-darwin25.0.0"),
            ("apple-m1", "")
        );
    }

    #[test]
    fn aarch64_linux_keeps_generic_with_v8a_features() {
        assert_eq!(
            default_cpu_and_features("aarch64-unknown-linux-gnu"),
            ("generic", "+v8a,+outline-atomics")
        );
    }

    #[test]
    fn x86_64_linux_defaults_to_x86_64_baseline() {
        assert_eq!(
            default_cpu_and_features("x86_64-unknown-linux-gnu"),
            ("x86-64", "")
        );
    }

    #[test]
    fn unknown_triple_falls_back_to_generic() {
        assert_eq!(
            default_cpu_and_features("riscv64-unknown-elf"),
            ("generic", "")
        );
    }
}
