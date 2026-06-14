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
    link_executable_exports(obj_path, exe_path, &[])
}

/// As [`link_executable`], but surfacing the discovered WASM entry-point
/// exports (phase-10 "WASM entry-point discovery") as `--export=<name>`
/// wasm-ld arguments. `wasm_exports` is ignored on native targets (which
/// have no module-export concept); the WASM build paths in `cli.rs` pass
/// the [`crate::wasm_exports::collect_wasm_exports`] result here.
pub fn link_executable_exports(
    obj_path: &str,
    exe_path: &str,
    wasm_exports: &[String],
) -> Result<(), String> {
    if crate::target::active_target_is_wasm() {
        return link_wasm_executable(obj_path, exe_path, wasm_exports);
    }
    link_executable_impl(obj_path, exe_path, &[])
}

/// Link a wasm32 object into a WASI command module (phase-10 WASM build
/// path, `--target=wasm_wasi` and `--target=wasm_browser` — browser
/// modules are wasip1 modules whose WASI surface the generated JS glue
/// polyfills; `host fn` imports stay undefined here by design, carrying
/// explicit `wasm-import-module` attributes that wasm-ld turns into
/// import entries instead of undefined-symbol errors).
///
/// Inputs, in link order:
///   1. `crt1-command.o` — wasi-libc's `_start` (enters at the
///      `__main_void` shim codegen emitted; see `emit_wasm_entry_shim`).
///   2. The karac-emitted object.
///   3. `libkarac_runtime_wasm.a` — the runtime built
///      `--no-default-features --target wasm32-wasip1` (see
///      [`resolve_wasm_runtime_path`]).
///   4. `libc.a` — wasi-libc, from the Rust toolchain's self-contained
///      sysroot for `wasm32-wasip1` (karac already requires the Rust
///      toolchain to build the runtime archive, so reusing its sysroot
///      adds no new dependency; no wasi-sdk install needed).
///
/// `wasm-ld` garbage-collects unreferenced sections by default, so the
/// runtime archive's unused subsystems cost nothing — same effect as
/// the native path's `-dead_strip`. Stack: 1 MiB (rustc's wasm32
/// default; wasm-ld's own 64 KiB default is tight for array-heavy Kāra
/// frames), placed before globals (`--stack-first`) so overflow traps
/// instead of silently corrupting the data section.
fn link_wasm_executable(
    obj_path: &str,
    exe_path: &str,
    wasm_exports: &[String],
) -> Result<(), String> {
    let (linker, flavor_args) = resolve_wasm_linker()?;
    let sysroot = resolve_wasi_self_contained_dir("wasm32-wasip1")?;
    let crt1 = sysroot.join("crt1-command.o");
    let libc = sysroot.join("libc.a");
    for (what, p) in [("crt1-command.o", &crt1), ("libc.a", &libc)] {
        if !p.exists() {
            return Err(format!(
                "{} not found at {} — reinstall the wasm32-wasip1 target \
                 (`rustup target add wasm32-wasip1`)",
                what,
                p.display()
            ));
        }
    }
    let runtime_path = resolve_wasm_runtime_path(false)?;

    let mut cmd = std::process::Command::new(&linker);
    cmd.args(&flavor_args);
    cmd.arg(&crt1);
    cmd.arg(obj_path);
    cmd.arg(&runtime_path);
    cmd.arg(&libc);
    cmd.args(["-z", "stack-size=1048576", "--stack-first"]);
    // Phase-10 WASM entry-point discovery: surface each `pub fn` tagged
    // for this target as a wasm module export (`crate::wasm_exports`).
    // `pub` already gives them external linkage; `--export=` keeps them
    // through wasm-ld's default section GC and lists them as exports.
    append_wasm_export_flags(&mut cmd, wasm_exports);
    // Rich-export builds (browser + component) need the canonical-ABI
    // allocator surfaced so the host can lower variable-length data
    // (strings/lists) into our linear memory — `wasm-tools component new`
    // on the component path, the JS glue on the browser path (sub-slices
    // D/E). Harmless when unused; the symbol lives in the wasm runtime
    // archive (`runtime/wasm_alloc.rs`).
    if crate::target::wasm_export_marshalling() {
        cmd.arg("--export=cabi_realloc");
    }
    cmd.args(["-o", exe_path]);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to invoke {}: {}", linker.display(), e))?;
    if !output.status.success() {
        return Err(format!(
            "wasm-ld failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Link a wasm32 object into a **shared-memory threaded** WASI command
/// module — the second artifact of a `--features wasm-threads` build
/// (phase-10 "WASM concurrency lowering — `--features wasm-threads`
/// opt-in"). Differences from [`link_wasm_executable`]:
///
///   - Sysroot + crt1 + libc come from the **`wasm32-wasip1-threads`**
///     self-contained dir — the only rustup-shipped wasi-libc built with
///     `+atomics`/`+bulk-memory` (wasm-ld refuses `--shared-memory`
///     against any object lacking them) and the one whose pthreads ride
///     the wasi-threads ABI (`wasi.thread-spawn` import /
///     `wasi_thread_start` export, serviced by the JS glue's Web
///     Workers).
///   - Runtime archive is `libkarac_runtime_wasm_threads.a` (the
///     `--features wasm-threads` build — pool-backed scheduler).
///   - `--import-memory --export-memory --shared-memory
///     --max-memory=<bytes>`: shared memories must be imported (the glue
///     creates the `WebAssembly.Memory({shared: true})` and hands it to
///     every worker instantiation) and must declare a maximum.
///     `--export-memory` re-exports the import so the glue's existing
///     `instance.exports.memory` reads keep working. This flag set
///     mirrors rustc's own wasm32-wasip1-threads link line (verified via
///     `--print link-args`); rust-lld auto-handles the TLS exports
///     (`__wasm_init_tls` & co.) under `--shared-memory`, no extra
///     flags.
///
/// `max_memory_bytes` comes from the manifest's `[wasm]
/// max-memory-pages` knob (default 16384 pages = 1 GiB — rustc's own
/// default for this target) via the CLI layer.
pub fn link_wasm_executable_threaded(
    obj_path: &str,
    exe_path: &str,
    max_memory_bytes: u64,
    wasm_exports: &[String],
) -> Result<(), String> {
    let (linker, flavor_args) = resolve_wasm_linker()?;
    let sysroot = resolve_wasi_self_contained_dir("wasm32-wasip1-threads")?;
    let crt1 = sysroot.join("crt1-command.o");
    let libc = sysroot.join("libc.a");
    for (what, p) in [("crt1-command.o", &crt1), ("libc.a", &libc)] {
        if !p.exists() {
            return Err(format!(
                "{} not found at {} — reinstall the wasm32-wasip1-threads target \
                 (`rustup target add wasm32-wasip1-threads`)",
                what,
                p.display()
            ));
        }
    }
    let runtime_path = resolve_wasm_runtime_path(true)?;

    let mut cmd = std::process::Command::new(&linker);
    cmd.args(&flavor_args);
    cmd.arg(&crt1);
    cmd.arg(obj_path);
    cmd.arg(&runtime_path);
    cmd.arg(&libc);
    cmd.args([
        "-z",
        "stack-size=1048576",
        "--stack-first",
        "--import-memory",
        "--export-memory",
        "--shared-memory",
        &format!("--max-memory={max_memory_bytes}"),
        // Host-async channel producers (phase-10 `std.web.time.*`): the glue
        // stands up a second main-thread "service" instance over the shared
        // memory whose only job is to send into a channel from a host event
        // callback (`setTimeout` etc.) and wake the worker parked in `recv`.
        // These two externs are otherwise internal (not user-callable wasm
        // exports), so surface them explicitly. Harmless for programs that
        // never use timers — the symbols are already linked from the archive.
        "--export=karac_runtime_channel_send",
        "--export=karac_runtime_channel_drop_sender",
        // `animation_frames` backpressure: the rAF service callback probes the
        // channel depth so it feeds at most one un-drained `()` tick per loop
        // (a slow consumer drops backlog instead of accumulating frame lag).
        // Service-instance-only, so surface it explicitly like the two above.
        "--export=karac_runtime_channel_pending",
        // The service instance never runs `_start`/`wasi_thread_start`, so
        // its `__stack_pointer` still aliases the primary worker's stack.
        // The glue retargets it to a dedicated scratch buffer (top from
        // `karac_runtime_service_stack_top`) right after instantiation, so
        // a timer-callback `channel_send` can't clobber the parked worker's
        // live frames. Both must be exported for the glue to reach them.
        "--export=__stack_pointer",
        "--export=karac_runtime_service_stack_top",
        // Non-unit event-data producers (`std.web.events.*`, `Channel[T]` for
        // `T != ()`): the service callback marshals an event payload into this
        // scratch buffer in shared memory before `channel_send` copies it onto
        // the queue. Service-instance-only (JS reaches it, no wasm caller), so
        // — like the channel/stack externs above — wasm-ld would dead-strip it
        // without an explicit `--export`. Harmless for programs that use no
        // event-data producer; the symbol is already linked from the archive.
        "--export=karac_runtime_event_scratch",
    ]);
    // Phase-10 WASM entry-point discovery — same per-target `pub fn`
    // exports as the sequential path (see [`link_wasm_executable`]).
    append_wasm_export_flags(&mut cmd, wasm_exports);
    cmd.args(["-o", exe_path]);

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to invoke {}: {}", linker.display(), e))?;
    if !output.status.success() {
        return Err(format!(
            "wasm-ld (threaded) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

/// Append a `--export=<name>` wasm-ld argument for each discovered WASM
/// export entry point (phase-10 "WASM entry-point discovery"). The
/// symbols already have external linkage (they are `pub fn`s); `--export`
/// both pins them through wasm-ld's default dead-section GC and lists
/// them in the module's export section so JS / a component host can call
/// them. No-op for programs with no tagged exports.
fn append_wasm_export_flags(cmd: &mut std::process::Command, wasm_exports: &[String]) {
    for name in wasm_exports {
        cmd.arg(format!("--export={name}"));
    }
}

/// Locate a wasm-capable linker. Resolution order:
///   1. `KARAC_WASM_LD` env var — honored verbatim (mirror of
///      `KARAC_RUNTIME`'s contract).
///   2. `wasm-ld` on `PATH`.
///   3. Homebrew LLVM's `wasm-ld` (macOS: Apple's toolchain ships no
///      wasm backend driver, brew's llvm/llvm@18 do).
///   4. The Rust toolchain's `rust-lld` with `-flavor wasm` — present in
///      every rustup install, so the karac-runtime build prerequisite
///      already guarantees a working fallback.
///
/// Returns the command plus any prefix args the flavor needs.
fn resolve_wasm_linker() -> Result<(std::path::PathBuf, Vec<String>), String> {
    if let Some(p) = std::env::var_os("KARAC_WASM_LD") {
        return Ok((std::path::PathBuf::from(p), vec![]));
    }
    // PATH probe: `wasm-ld --version` exiting zero is the existence check.
    let on_path = std::process::Command::new("wasm-ld")
        .arg("--version")
        .output();
    if matches!(on_path, Ok(ref o) if o.status.success()) {
        return Ok((std::path::PathBuf::from("wasm-ld"), vec![]));
    }
    for brew in [
        "/opt/homebrew/opt/llvm/bin/wasm-ld",
        "/opt/homebrew/opt/llvm@18/bin/wasm-ld",
        "/usr/local/opt/llvm/bin/wasm-ld",
        "/usr/local/opt/llvm@18/bin/wasm-ld",
    ] {
        if std::path::Path::new(brew).exists() {
            return Ok((std::path::PathBuf::from(brew), vec![]));
        }
    }
    // rust-lld lives under <sysroot>/lib/rustlib/<host>/bin/.
    let sysroot = rustc_print(&["--print", "sysroot"])?;
    let host = rustc_host_triple()?;
    let rust_lld = std::path::Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host.trim())
        .join("bin/rust-lld");
    if rust_lld.exists() {
        return Ok((rust_lld, vec!["-flavor".to_string(), "wasm".to_string()]));
    }
    Err(
        "no wasm linker found: set KARAC_WASM_LD, or install one of wasm-ld (PATH / \
         homebrew llvm) — rust-lld via rustup also works"
            .to_string(),
    )
}

/// The Rust toolchain's self-contained wasi sysroot for the given wasm
/// triple (`<target-libdir>/self-contained` — holds `crt1-command.o` and
/// wasi-libc's `libc.a`). `wasm32-wasip1` for the sequential module;
/// `wasm32-wasip1-threads` for the threaded one (its libc is built with
/// atomics + pthreads-over-wasi-threads — the two are not
/// interchangeable under `--shared-memory`).
fn resolve_wasi_self_contained_dir(triple: &str) -> Result<std::path::PathBuf, String> {
    let libdir = rustc_print(&["--print", "target-libdir", "--target", triple])?;
    let dir = std::path::Path::new(libdir.trim()).join("self-contained");
    if !dir.is_dir() {
        return Err(format!(
            "{triple} self-contained sysroot not found at {} — install it with \
             `rustup target add {triple}`",
            dir.display()
        ));
    }
    Ok(dir)
}

fn rustc_print(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("rustc")
        .args(args)
        .output()
        .map_err(|e| format!("failed to invoke rustc {}: {}", args.join(" "), e))?;
    if !out.status.success() {
        return Err(format!(
            "rustc {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn rustc_host_triple() -> Result<String, String> {
    let verbose = rustc_print(&["-vV"])?;
    verbose
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|s| s.to_string())
        .ok_or_else(|| "rustc -vV printed no host line".to_string())
}

/// Resolve a wasm runtime archive. Mirrors [`resolve_runtime_path`]'s
/// tiers with the `_wasm` / `_wasm_threads` artifact name, plus the
/// cargo target-dir location as a dev convenience (where the documented
/// build command leaves it before the `cp` step):
///   1. `KARAC_RUNTIME` — verbatim, same contract as native.
///   2. Installed: `<karac-bin-dir>/../lib/libkarac_runtime_wasm[_threads].a`.
///   3. Dev: `<workspace>/target/release/libkarac_runtime_wasm[_threads].a`.
///   4. Dev: `<workspace>/target/<wasm-triple>/release/libkarac_runtime.a`.
///
/// `threaded` selects the `--features wasm-threads` archive built for
/// `wasm32-wasip1-threads` (pool-backed scheduler, atomics-clean
/// objects) — the sequential and threaded archives are never
/// interchangeable: wasm-ld rejects the sequential one under
/// `--shared-memory` (no atomics features) and the threaded one without
/// it (shared-memory objects need the threaded link).
fn resolve_wasm_runtime_path(threaded: bool) -> Result<String, String> {
    if let Ok(p) = std::env::var("KARAC_RUNTIME") {
        return Ok(p);
    }
    let (installed_name, dev_rels, recipe): (_, [&str; 2], _) = if threaded {
        (
            "../lib/libkarac_runtime_wasm_threads.a",
            [
                "target/release/libkarac_runtime_wasm_threads.a",
                "target/wasm32-wasip1-threads/release/libkarac_runtime.a",
            ],
            "libkarac_runtime_wasm_threads.a not found; set KARAC_RUNTIME or build it: \
             `cargo rustc -p karac-runtime --release --target wasm32-wasip1-threads \
             --no-default-features --features wasm-threads --crate-type staticlib` then copy \
             target/wasm32-wasip1-threads/release/libkarac_runtime.a to \
             target/release/libkarac_runtime_wasm_threads.a (see CLAUDE.md / design.md § \
             Runtime Distribution)",
        )
    } else {
        (
            "../lib/libkarac_runtime_wasm.a",
            [
                "target/release/libkarac_runtime_wasm.a",
                "target/wasm32-wasip1/release/libkarac_runtime.a",
            ],
            "libkarac_runtime_wasm.a not found; set KARAC_RUNTIME or build it: \
             `cargo rustc -p karac-runtime --release --target wasm32-wasip1 \
             --no-default-features --crate-type staticlib` then copy \
             target/wasm32-wasip1/release/libkarac_runtime.a to \
             target/release/libkarac_runtime_wasm.a (see CLAUDE.md / design.md § \
             Runtime Distribution)",
        )
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let installed = bin_dir.join(installed_name);
            if installed.exists() {
                return Ok(installed.to_string_lossy().into_owned());
            }
        }
    }
    let dev_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in dev_rels {
        let p = dev_root.join(rel);
        if p.exists() {
            return Ok(p.to_string_lossy().into_owned());
        }
    }
    Err(recipe.to_string())
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
    // External native libraries from `kara.toml`'s `[link]` table
    // (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking). Search paths
    // (`-L`) precede the `-l` flags so the linker can resolve each lib
    // against them, and both follow the karac-emitted object on the line so
    // the object's undefined symbols pull from these libraries. The
    // motivating consumer is the self-hosted codegen leg linking
    // `libLLVM-18`; absent a `[link]` table this loop is empty and the line
    // is byte-identical to before.
    if let Some(link) = crate::target::native_link_config() {
        for dir in &link.search_paths {
            cmd.arg(format!("-L{dir}"));
        }
        for lib in &link.libs {
            cmd.arg(format!("-l{lib}"));
        }
    }
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
///
/// `KARAC_RUNTIME` is honored **verbatim** — it names the exact archive
/// file to link, with no lean-sibling substitution. It's the dev/test
/// iteration hatch ("use exactly this archive I just built"), and tests
/// that link feature-gated symbols (e.g. `tests/park_and_wake.rs`'s
/// `test-helpers` build) depend on the named file being the linked file.
/// An earlier version substituted a `libkarac_runtime_min.a` sitting
/// beside the override when `prefer_min` held; that silently swapped the
/// test-helpers archive for the lean one and broke park_and_wake with an
/// undefined `_karac_runtime_test_bind_and_print_port` whenever a lean
/// archive existed on disk. The min preference now applies only to the
/// directory-search tiers (2 and 3), where no specific file was named.
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

    // 1. Explicit override — honored verbatim (see doc comment above; no
    //    lean-sibling substitution, regardless of `prefer_min`).
    if let Ok(p) = std::env::var("KARAC_RUNTIME") {
        let path = std::path::Path::new(&p);
        if !path.exists() {
            return Err(format!("KARAC_RUNTIME set to {p} but file does not exist"));
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
        "libkarac_runtime.a not found; set KARAC_RUNTIME or build the runtime crate (`cargo rustc -p karac-runtime --release --crate-type staticlib` — NOT plain `cargo build`, which co-emits the rlib and defeats the staticlib's dead-strip; see runtime/Cargo.toml). For the lean compute-only archive also build `--no-default-features --features net` and install it as `libkarac_runtime_min.a` alongside.".to_string(),
    )
}

/// The C-allocator symbol codegen declares for RC/heap allocation, by
/// target. Native: libc `malloc` (size arg is i64 = size_t on every
/// 64-bit native target). wasm targets: `__karac_malloc64`, the wasm
/// runtime's 64-bit-size shim over wasi-libc `malloc` — wasm32's
/// `size_t` is i32 and wasm traps on signature-mismatched calls, so
/// declaring libc `malloc` with the i64 signature karac IR uses would
/// fault at the first allocation (`RuntimeError: unreachable,
/// signature_mismatch:malloc`). See `runtime/src/wasm_alloc.rs`.
pub(super) fn c_malloc_symbol() -> &'static str {
    if crate::target::active_target_is_wasm() {
        "__karac_malloc64"
    } else {
        "malloc"
    }
}

/// CStr form of [`c_malloc_symbol`] for the llvm-sys raw-FFI declare
/// path (`coro.rs`).
pub(super) fn c_malloc_symbol_cstr() -> &'static std::ffi::CStr {
    if crate::target::active_target_is_wasm() {
        c"__karac_malloc64"
    } else {
        c"malloc"
    }
}

/// Symbol for the panicking allocation wrapper. Native: the real
/// `usize`-as-i64 `karac_alloc_or_panic`. wasm: the `__karac_alloc_or_panic64`
/// i64 shim — wasm32's `size_t` is i32 and the runtime wrapper takes `usize`,
/// so a direct i64 codegen call traps `signature_mismatch:karac_alloc_or_panic`
/// (B-2026-06-12-1). Twin of [`c_malloc_symbol`]; see `runtime/src/wasm_alloc.rs`.
pub(super) fn c_alloc_or_panic_symbol() -> &'static str {
    if crate::target::active_target_is_wasm() {
        "__karac_alloc_or_panic64"
    } else {
        "karac_alloc_or_panic"
    }
}

/// Symbol for the fallible allocation wrapper — the `null`-on-OOM sibling of
/// [`c_alloc_or_panic_symbol`], same wasm size_t-width rationale.
pub(super) fn c_alloc_fallible_symbol() -> &'static str {
    if crate::target::active_target_is_wasm() {
        "__karac_alloc_fallible64"
    } else {
        "karac_alloc_fallible"
    }
}

/// Symbol for the panicking reallocation wrapper — the grow-path counterpart of
/// [`c_alloc_or_panic_symbol`]. Native: `karac_realloc_or_panic`. wasm: the
/// `__karac_realloc_or_panic64` i64-size shim (same size_t-width rationale as
/// the alloc symbols; B-2026-06-12-1).
pub(super) fn c_realloc_or_panic_symbol() -> &'static str {
    if crate::target::active_target_is_wasm() {
        "__karac_realloc_or_panic64"
    } else {
        "karac_realloc_or_panic"
    }
}

/// Dispatch on the active compilation target (phase-10 `--target`).
/// `Codegen::new` routes the module's triple + datalayout through this,
/// so swapping the target machine here re-points the whole emission
/// path — datalayout included, which `llvm.coro.size` folds against
/// (the eba48194 lesson: an unset/wrong layout under-allocates coro
/// frames).
pub(super) fn create_target_machine() -> Result<TargetMachine, String> {
    create_target_machine_with_cpu(crate::target::target_cpu_override())
}

/// `cpu_override` swaps the CPU baseline while keeping every other
/// target-machine parameter (triple, features, reloc model) at the
/// per-target default — the `--target-cpu` contract (design.md § CPU
/// Baseline Targeting): widening or narrowing the baseline is the
/// user's call, the rest of the machine configuration is not. The
/// `--target-features` override (its sibling chain) is read inside the
/// per-target constructors and *appended* after the default features.
fn create_target_machine_with_cpu(cpu_override: Option<&str>) -> Result<TargetMachine, String> {
    if crate::target::active_target_is_wasm() {
        create_wasm_target_machine(cpu_override)
    } else {
        create_native_target_machine(cpu_override)
    }
}

/// Combine the per-target default features with the `--target-features`
/// override: defaults first, user list appended — LLVM applies feature
/// flags in order with last-wins resolution, so a user `-feat` genuinely
/// disables a table default (e.g. `-outline-atomics` on aarch64-linux)
/// and the default can never silently re-override the user.
fn combined_features(default_features: &str) -> String {
    match crate::target::target_features_override() {
        None => default_features.to_string(),
        Some(user) if default_features.is_empty() => user.to_string(),
        Some(user) => format!("{default_features},{user}"),
    }
}

/// Print LLVM's supported-CPU table for the active target to stderr
/// (the `karac build --target-cpu=help` listing, mirroring `rustc -C
/// target-cpu=help`). Constructing a target machine with the
/// pseudo-CPU `"help"` makes LLVM's `MCSubtargetInfo` dump the
/// registry — `Available CPUs for this target:` followed by one
/// two-space-indented `<name> - <desc>` line per CPU, then the
/// features table. There is no LLVM-C API for this list (rustc carries
/// a custom C++ shim); the stderr dump is the only portable channel,
/// which is also why validation (`validate_target_cpu`) captures it
/// from a child process instead of in-process.
pub fn print_target_cpu_listing() {
    // One dump serves both `--target-cpu=help` and
    // `--target-features=help`: MCSubtargetInfo prints the CPUs table
    // and the features table together.
    eprintln!(
        "Supported CPUs and features for target `{}`:",
        crate::target::active_target()
    );
    let _ = create_target_machine_with_cpu(Some("help"));
}

/// Target machine for the wasm targets (`wasm_wasi`, `wasm_browser` —
/// both wasip1 modules in v1): wasm32 with the WASI preview-1 OS tag —
/// matches rustc's `wasm32-wasip1` llvm-target and the triple the
/// runtime archive (`libkarac_runtime_wasm.a`) is built for. CPU
/// baseline `generic`. Static reloc: `wasm-ld` links a non-relocatable
/// module; PIC is only for shared-library wasm.
fn create_wasm_target_machine(cpu_override: Option<&str>) -> Result<TargetMachine, String> {
    Target::initialize_webassembly(&InitializationConfig::default());

    let triple = inkwell::targets::TargetTriple::create("wasm32-wasip1");
    let target =
        Target::from_triple(&triple).map_err(|e| format!("Failed to get wasm32 target: {}", e))?;

    target
        .create_target_machine(
            &triple,
            cpu_override.unwrap_or("generic"),
            // `+simd128` by default: WASM SIMD-128 is a first-class
            // lowering target (design.md § Portable SIMD; phase-10 WASM
            // SIMD-128 entry) and part of the WASM 2.0 baseline every
            // current engine ships, so `Vector[T, N]` ops select single
            // `v128` instructions up to 128 bits and split under tier 2
            // above that. Hosts without SIMD-128 reject a module
            // containing `v128` at validation (the feature is
            // module-granular, not per-instruction), so the
            // portable-by-guarantee fallback is the opt-out *build*:
            // `--target-features=-simd128` appends after this default
            // and wins (last-wins resolution — `combined_features`),
            // and LLVM then scalarizes every vector op into an
            // MVP-clean module. The `#[require_simd]` / `--simd-report`
            // target model mirrors this default+override chain via
            // `target::wasm_simd128_enabled`.
            &combined_features("+simd128"),
            backend_optimization_level(),
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "Failed to create wasm32 target machine".to_string())
}

/// Target machine for the **threaded pass** of a `--features
/// wasm-threads` build (phase-10 wasm-threads entry): triple
/// `wasm32-wasip1-threads` with the threading feature set rustc uses
/// for that target — `+atomics` (the emitted object must carry it or
/// wasm-ld refuses `--shared-memory`), `+bulk-memory` (`memory.init`
/// guarded data segments + `memory.atomic.*`), `+mutable-globals` (the
/// per-thread `__stack_pointer`/TLS globals), plus the `+simd128`
/// default shared with the sequential machine. The `--target-features`
/// override still appends last-wins via [`combined_features`] — a user
/// `-simd128` composes; stripping `-atomics` would just fail the link,
/// which is the right loud failure.
pub(super) fn create_target_machine_threaded() -> Result<TargetMachine, String> {
    create_wasm_target_machine_threaded(crate::target::target_cpu_override())
}

fn create_wasm_target_machine_threaded(
    cpu_override: Option<&str>,
) -> Result<TargetMachine, String> {
    Target::initialize_webassembly(&InitializationConfig::default());

    let triple = inkwell::targets::TargetTriple::create("wasm32-wasip1-threads");
    let target = Target::from_triple(&triple)
        .map_err(|e| format!("Failed to get wasm32-threads target: {}", e))?;

    target
        .create_target_machine(
            &triple,
            cpu_override.unwrap_or("generic"),
            // Threading features first (see the doc comment on
            // `create_target_machine_threaded` for what each buys),
            // `+simd128` kept from the sequential default — same
            // rationale and same `-simd128` opt-out path (last-wins via
            // `combined_features`).
            &combined_features("+atomics,+bulk-memory,+mutable-globals,+simd128"),
            backend_optimization_level(),
            RelocMode::Default,
            CodeModel::Default,
        )
        .ok_or_else(|| "Failed to create wasm32-threads target machine".to_string())
}

fn create_native_target_machine(cpu_override: Option<&str>) -> Result<TargetMachine, String> {
    Target::initialize_native(&InitializationConfig::default())
        .map_err(|e| format!("Failed to initialize native target: {}", e))?;

    let triple = TargetMachine::get_default_triple();
    let target =
        Target::from_triple(&triple).map_err(|e| format!("Failed to get target: {}", e))?;

    let triple_str = triple.as_str().to_str().unwrap_or("");
    // A CPU override swaps the CPU only; the table's default *features*
    // string stays (rustc's `-C target-cpu` posture — target-spec
    // features apply regardless of the CPU choice). E.g. on
    // aarch64-linux, `--target-cpu=neoverse-v1` keeps
    // `+outline-atomics`. A `--target-features` override appends after
    // the defaults (last-wins — see `combined_features`).
    let (default_cpu, default_features) = default_cpu_and_features(triple_str);
    let cpu = cpu_override.unwrap_or(default_cpu);
    let features = combined_features(default_features);

    target
        .create_target_machine(
            &triple,
            cpu,
            &features,
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
/// in `kara.toml` (precedence in that order) swaps the CPU while keeping
/// this table's features string — see `create_native_target_machine` and
/// `cli.rs::apply_target_cpu_override`. The fallback `("generic", "")` is
/// intentional for unknown triples: better a portable binary than one
/// that won't load.
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

/// Validate a `--target-cpu` value against LLVM's CPU registry for the
/// active target. `Err` carries the full user-facing diagnostic
/// (unknown name + the supported listing — the tracker's `rustc -C
/// target-cpu=help` mirror).
///
/// Mechanism: LLVM-C has no CPU-listing or `has_known_cpu` API, and on
/// an unknown CPU `createTargetMachine` merely warns on stderr and
/// falls back to generic — exactly the silent baseline-neutering this
/// validation exists to close. The registry's only portable channel is
/// MCSubtargetInfo's stderr dump on the pseudo-CPU `"help"`, so karac
/// re-invokes itself (`__list-target-cpus <target>`, a hidden argv
/// handled in `cli/args.rs`) and parses the child's stderr. If the
/// child can't run or its output doesn't parse (e.g. library consumers
/// whose `current_exe` is not karac), validation degrades to
/// pass-through — LLVM's own warn-and-ignore still surfaces the typo
/// at codegen, just without the hard stop.
pub fn validate_target_cpu(cpu: &str) -> Result<(), String> {
    let Some(known) = supported_target_cpus() else {
        return Ok(());
    };
    if known.iter().any(|k| k == cpu) {
        return Ok(());
    }
    Err(format!(
        "error: unknown CPU '{}' for target `{}`.\nSupported CPUs: {}\nhint: run `karac build --target-cpu=help` for the annotated listing",
        cpu,
        crate::target::active_target(),
        known.join(", "),
    ))
}

/// Validate a `--target-features` value against LLVM's feature registry
/// for the active target — the `validate_target_cpu` sibling, with one
/// extra layer: token *shape*. Every comma-separated entry must carry
/// an explicit `+` or `-` prefix (LLVM's feature-string grammar; a bare
/// name would be silently meaningless) and name a registered feature.
/// LLVM's native behavior on an unknown feature is warn-and-ignore —
/// the same silent neutering the CPU validation closes. Registry
/// capture rides the same `__list-target-cpus` child dump (its
/// `Available features` section); unavailable → shape checks still
/// apply, membership degrades to pass-through.
pub fn validate_target_features(features: &str) -> Result<(), String> {
    let known = supported_target_features();
    for token in features.split(',') {
        let token = token.trim();
        if token.is_empty() {
            return Err(format!(
                "error: empty entry in --target-features '{features}' — use a comma-separated list like +aes,-sve"
            ));
        }
        let Some(name) = token.strip_prefix('+').or_else(|| token.strip_prefix('-')) else {
            return Err(format!(
                "error: --target-features entry '{token}' is missing its '+' or '-' prefix — write '+{token}' to enable or '-{token}' to disable"
            ));
        };
        if let Some(ref known) = known {
            if !known.iter().any(|k| k == name) {
                return Err(format!(
                    "error: unknown feature '{}' for target `{}`.\nSupported features: {}\nhint: run `karac build --target-features=help` for the annotated listing",
                    name,
                    crate::target::active_target(),
                    known.join(", "),
                ));
            }
        }
    }
    Ok(())
}

/// Capture the active target's CPU registry by re-invoking karac with
/// the hidden `__list-target-cpus` argv and parsing the child's stderr
/// dump. `None` when the listing is unavailable (spawn failure, non-
/// karac `current_exe`, non-llvm child) — callers degrade gracefully.
fn supported_target_cpus() -> Option<Vec<String>> {
    let names = parse_cpu_names_from_help_listing(&capture_help_listing()?);
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// Capture the active target's feature registry — same child dump as
/// `supported_target_cpus`, parsing the `Available features` section.
fn supported_target_features() -> Option<Vec<String>> {
    let names = parse_feature_names_from_help_listing(&capture_help_listing()?);
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

/// Re-invoke karac with the hidden `__list-target-cpus` argv and return
/// the child's stderr (the MCSubtargetInfo dump: CPUs table + features
/// table). `None` on spawn failure.
fn capture_help_listing() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new(exe)
        .args(["__list-target-cpus", crate::target::active_target()])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Extract CPU names from MCSubtargetInfo's `"help"` dump. The shape
/// (LLVM 18, `MCSubtargetInfo.cpp::Help`):
///
/// ```text
/// Available CPUs for this target:
///
///   apple-m1        - Select the apple-m1 processor.
///   ...
///
/// Available features for this target:
/// ```
///
/// Names are the first token of two-space-indented lines between the
/// CPUs header and the features header; everything else (headers,
/// blank lines, the `Use +feature…` trailer) is dropped.
fn parse_cpu_names_from_help_listing(listing: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_cpu_section = false;
    for line in listing.lines() {
        if line.starts_with("Available CPUs") {
            in_cpu_section = true;
            continue;
        }
        if line.starts_with("Available features") {
            break;
        }
        if !in_cpu_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ") {
            if let Some(name) = rest.split_whitespace().next() {
                names.push(name.to_string());
            }
        }
    }
    names
}

/// Extract feature names from the same MCSubtargetInfo dump — the
/// `Available features for this target:` section that
/// `parse_cpu_names_from_help_listing` deliberately stops at. Ends at
/// the `Use +feature…` trailer (its lines are not two-space-indented,
/// so the indent filter would drop them anyway; the explicit break
/// documents the boundary).
fn parse_feature_names_from_help_listing(listing: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_features_section = false;
    for line in listing.lines() {
        if line.starts_with("Available features") {
            in_features_section = true;
            continue;
        }
        if line.starts_with("Use +feature") {
            break;
        }
        if !in_features_section {
            continue;
        }
        if let Some(rest) = line.strip_prefix("  ") {
            if let Some(name) = rest.split_whitespace().next() {
                names.push(name.to_string());
            }
        }
    }
    names
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

/// Release trigger for the `?`-error-return-trace instrumentation (`KARAC_STRIP
/// _ERROR_TRACE`). When set, codegen emits no `karac_error_trace_push` at `?`
/// failure sites and no `karac_error_trace_clear` on the success path — the
/// trace is a debug-only diagnostic (mirrors `strip_contracts`), so a release
/// build pays zero `?`-site instrumentation cost. `karac build --release` forces
/// this on alongside contract stripping; the env var is the per-construction
/// knob the `release` flag and `Codegen::new` default compose with (OR).
///
/// - `Ok("1")` / `Ok("true")` → `true` (strip the trace).
/// - anything else (incl. unset) → `false` (debug default — trace active).
pub(super) fn read_strip_error_trace_env() -> bool {
    matches!(std::env::var("KARAC_STRIP_ERROR_TRACE"), Ok(v) if v == "1" || v == "true")
}

#[cfg(test)]
mod tests {
    use super::{
        default_cpu_and_features, parse_cpu_names_from_help_listing,
        parse_feature_names_from_help_listing, symbol_listing_references_tls,
    };

    #[test]
    fn cpu_help_listing_parse_extracts_names_and_stops_at_features() {
        // Canned MCSubtargetInfo "help" dump shape (LLVM 18): names are
        // the first token of two-space-indented lines between the CPUs
        // header and the features header; headers, blank lines, and
        // everything from the features section on (including feature
        // names, which would otherwise pollute the valid-CPU set) are
        // dropped.
        let listing = "\
Supported CPUs for target `native`:
Available CPUs for this target:

  apple-m1        - Select the apple-m1 processor.
  apple-m2        - Select the apple-m2 processor.
  generic         - Select the generic processor.

Available features for this target:

  aes             - Enable AES support.

Use +feature to enable a feature, or -feature to disable it.
For example, llc -mcpu=mycpu -mattr=+feature1,-feature2
";
        assert_eq!(
            parse_cpu_names_from_help_listing(listing),
            vec!["apple-m1", "apple-m2", "generic"],
        );
    }

    #[test]
    fn cpu_help_listing_parse_handles_garbage() {
        // No CPUs header → nothing parsed (the validate caller treats
        // an empty parse as "listing unavailable" and passes through).
        assert!(parse_cpu_names_from_help_listing("").is_empty());
        assert!(
            parse_cpu_names_from_help_listing("running 0 tests\n\ntest result: ok.\n").is_empty()
        );
    }

    #[test]
    fn feature_help_listing_parse_extracts_features_only() {
        // The features parser is the CPU parser's mirror: it skips the
        // CPUs section entirely (CPU names must not pollute the valid-
        // feature set) and stops at the `Use +feature…` trailer.
        let listing = "\
Supported CPUs and features for target `native`:
Available CPUs for this target:

  apple-m1        - Select the apple-m1 processor.

Available features for this target:

  aes             - Enable AES support.
  outline-atomics - Enable out of line atomics to support LSE instructions.

Use +feature to enable a feature, or -feature to disable it.
For example, llc -mcpu=mycpu -mattr=+feature1,-feature2
";
        assert_eq!(
            parse_feature_names_from_help_listing(listing),
            vec!["aes", "outline-atomics"],
        );
        // Garbage → empty (validation degrades to pass-through).
        assert!(parse_feature_names_from_help_listing("").is_empty());
    }

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
